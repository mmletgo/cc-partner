//! user_mirror/push — 源侧 multi-peer 用户级镜像 Push
//!
//! Business Logic（为什么需要这个模块）:
//!     Push 的 apply 端是对端 owning process：本机只 freeze 一次，再把 envelope/objects
//!     推到新的 user-mirror 路由上 commit 写盘。缺能力不得回落旧 `/api/agent-hub/push/prepare`。
//!
//! Code Logic（这个模块做什么）:
//!     claim 源 plan → 校验 inventory → freeze 一次 → 每 peer 并发 ≤3：
//!     health 要求 `agent-hub.user-mirror.v1` + `device.request-binding.v1` 且
//!     `health.device_id == peer_id` → prepare → PUT missing（并发）→ commit。
//!     单 peer 失败写入 `kind=user_mirror` 源侧失败表，不回滚其它 peer。

use super::inventory::build_local_user_mirror_inventory;
use super::ledger::UserMirrorClaim;
use super::models::{
    ApplyUserMirrorRequest, UserMirrorAgentResultDto, UserMirrorDirection, UserMirrorItemState,
    UserMirrorPlanDto, UserMirrorResultDto, USER_MIRROR_CAPABILITY_UNSUPPORTED,
    USER_MIRROR_PEER_OFFLINE, USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE,
};
use super::receive::{CommitUserMirrorRequest, CommitUserMirrorResponse, PrepareUserMirrorRequest};
use super::selection::{freeze_user_mirror_selection, BuiltUserMirrorSelection};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::replication::receiver::{
    PreparePushResponse, PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
use crate::agent_hub::replication::sender::{
    TargetPushStatus, MAX_TARGET_PARALLELISM, SOURCE_PUSH_KIND_USER_MIRROR,
};
use crate::agent_hub::snapshot::builder::hash_selection;
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::{CAPABILITY_DEVICE_REQUEST_BINDING_V1, CAPABILITY_USER_MIRROR_V1};
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use crate::storage::maintenance_gate::with_shared_write_lease;
use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;

/// health.device_id 与所选 peer 不一致（fail-closed，不传 objects）。
const USER_MIRROR_DEVICE_ID_MISMATCH: &str = "USER_MIRROR_DEVICE_ID_MISMATCH";

const OBJECT_CHUNK_TIMEOUT: Duration = Duration::from_secs(120);
/// 单 peer 对象 PUT 并发。串行 1 万对象在 overlay RTT 下会打满 apply 墙钟。
const OBJECT_PUT_PARALLELISM: usize = 16;

/// 源侧 Push：freeze 一次并对每个 peer dest-apply。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认 preview 后，本机覆盖所选对端；一 peer 失败不得撤回已成功对端。
///
/// Code Logic（这个函数做什么）:
///     claim 源 plan → 校验 TTL/inventory → freeze → buffer_unordered(3) 推送 → complete。
pub async fn apply_push_user_mirror(
    state: &AppState,
    request: ApplyUserMirrorRequest,
) -> Result<UserMirrorResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let row = state
        .agent_hub_repo
        .get_user_mirror_plan(&request.plan_token)
        .await?
        .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
    let plan = parse_plan(&row.plan_json)?;
    if plan.direction != UserMirrorDirection::Push {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let peers = push_peer_ids(&plan);
    if peers.is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }

    let claim = state
        .agent_hub_repo
        .claim_user_mirror_plan(&request.plan_token, &request.client_request_id)
        .await?;
    match claim {
        UserMirrorClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
        UserMirrorClaim::Pending => Ok(outcome_unknown_result(
            &request.plan_token,
            &request.client_request_id,
            &plan,
        )),
        UserMirrorClaim::Claimed(_) => run_claimed_push(state, &request, &plan, &peers).await,
    }
}

/// claim 成功后 freeze 并 fan-out。
async fn run_claimed_push(
    state: &AppState,
    request: &ApplyUserMirrorRequest,
    plan: &UserMirrorPlanDto,
    peers: &[String],
) -> Result<UserMirrorResultDto, AppError> {
    if plan.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
        let fail = failed_code_result(
            request,
            plan,
            USER_MIRROR_STALE,
            UserMirrorItemState::Failed,
        );
        complete_plan(state, request, &fail).await?;
        return Err(AppError::conflict(USER_MIRROR_STALE));
    }

    let local = build_local_user_mirror_inventory(state, state.device_id.as_str()).await?;
    if local.inventory_snapshot_hash != plan.local_inventory_snapshot_hash {
        let fail = failed_code_result(
            request,
            plan,
            USER_MIRROR_STALE,
            UserMirrorItemState::Failed,
        );
        complete_plan(state, request, &fail).await?;
        return Err(AppError::conflict(USER_MIRROR_STALE));
    }

    let built = match freeze_user_mirror_selection(state, &local).await {
        Ok(built) => Arc::new(built),
        Err(error) => {
            let fail = failed_code_result(
                request,
                plan,
                &error.to_string(),
                UserMirrorItemState::Failed,
            );
            complete_plan(state, request, &fail).await?;
            return Ok(fail);
        }
    };
    let selection_hash = hash_selection(&built.envelope.selection)?;

    persist_source_request(
        state,
        &request.client_request_id,
        &selection_hash,
        &built.envelope.snapshot_hash,
        &request.plan_token,
    )
    .await?;

    let device_map = {
        let guard = state
            .devices
            .read()
            .map_err(|_| AppError::generic("user_mirror_push_devices_lock_poisoned".to_string()))?;
        guard.clone()
    };

    let mut jobs = Vec::with_capacity(peers.len());
    for peer_id in peers {
        let device = device_map.get(peer_id).cloned();
        let label = device
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| peer_id.clone());
        persist_target_pending(
            state,
            &request.client_request_id,
            peer_id,
            &label,
            &format!("{}:{peer_id}", request.client_request_id),
        )
        .await?;
        jobs.push((peer_id.clone(), label, device));
    }

    let source_device_id = state.device_id.as_str().to_string();
    let peer_client = PeerClient::new();
    let request_id = request.client_request_id.clone();
    let plan = Arc::new(plan.clone());
    let outcomes: Vec<PeerPushOutcome> = stream::iter(jobs)
        .map(|(peer_id, _label, device)| {
            let built = Arc::clone(&built);
            let plan = Arc::clone(&plan);
            let peer_client = peer_client.clone();
            let source_device_id = source_device_id.clone();
            let request_id = request_id.clone();
            let selection_hash = selection_hash.clone();
            async move {
                push_one_peer(PushOnePeerArgs {
                    peer_client: &peer_client,
                    source_device_id: &source_device_id,
                    request_id: &request_id,
                    peer_id: &peer_id,
                    device: device.as_ref(),
                    built: built.as_ref(),
                    plan: plan.as_ref(),
                    selection_hash: &selection_hash,
                })
                .await
            }
        })
        .buffer_unordered(MAX_TARGET_PARALLELISM)
        .collect()
        .await;

    for outcome in &outcomes {
        persist_target_outcome(state, &request.client_request_id, outcome).await?;
    }

    let result = aggregate_result(request, plan.as_ref(), &outcomes);
    complete_plan(state, request, &result).await?;
    Ok(result)
}

struct PushOnePeerArgs<'a> {
    peer_client: &'a PeerClient,
    source_device_id: &'a str,
    request_id: &'a str,
    peer_id: &'a str,
    device: Option<&'a Device>,
    built: &'a BuiltUserMirrorSelection,
    plan: &'a UserMirrorPlanDto,
    selection_hash: &'a str,
}

struct PeerPushOutcome {
    peer_device_id: String,
    status: TargetPushStatus,
    error_code: Option<String>,
    transfer_id: Option<String>,
    missing_object_count: u32,
    transferred_object_count: u32,
    dest_result: Option<UserMirrorResultDto>,
}

/// 单 peer：capability + binding → 新 user-mirror prepare/objects/commit。
async fn push_one_peer(args: PushOnePeerArgs<'_>) -> PeerPushOutcome {
    let PushOnePeerArgs {
        peer_client,
        source_device_id,
        request_id,
        peer_id,
        device,
        built,
        plan,
        selection_hash,
    } = args;
    let fail = |code: &str| PeerPushOutcome {
        peer_device_id: peer_id.to_string(),
        status: TargetPushStatus::Failed,
        error_code: Some(code.to_string()),
        transfer_id: None,
        missing_object_count: 0,
        transferred_object_count: 0,
        dest_result: None,
    };

    let Some(device) = device else {
        return fail(USER_MIRROR_PEER_OFFLINE);
    };
    if !device.online {
        return fail(USER_MIRROR_PEER_OFFLINE);
    }
    let base_url = device.base_url();

    if let Err(code) = ensure_user_mirror_peer_binding(peer_client, &base_url, peer_id).await {
        return fail(&code);
    }

    let mut dest_plan = plan.clone();
    dest_plan.destination_device_id = peer_id.to_string();
    let dest_client_request_id = format!("{request_id}:{peer_id}");
    let prepare_body = PrepareUserMirrorRequest {
        envelope: built.envelope.clone(),
        source_device_id: source_device_id.to_string(),
        client_request_id: dest_client_request_id.clone(),
        selection_hash: selection_hash.to_string(),
        plan_token: plan.plan_token.clone(),
        item_bindings: built.item_bindings.clone(),
        plan: dest_plan,
    };
    let prep: PreparePushResponse = match post_json_bound(
        peer_client,
        &base_url,
        "/api/agent-hub/user-mirror/prepare",
        &prepare_body,
        peer_id,
        PeerTimeoutClass::long_running(Duration::from_secs(60)),
    )
    .await
    {
        Ok(v) => v,
        Err(err) => return fail(&map_peer_error_code(&err)),
    };

    let transfer_id = prep.transfer_id.clone();
    let missing = prep.missing_object_hashes.clone();
    if prep.status == "committed" {
        return PeerPushOutcome {
            peer_device_id: peer_id.to_string(),
            status: TargetPushStatus::Committed,
            error_code: None,
            transfer_id: Some(transfer_id),
            missing_object_count: 0,
            transferred_object_count: 0,
            dest_result: None,
        };
    }

    let mut transferred = 0u32;
    let mut puts = stream::iter(missing.iter().cloned())
        .map(|object_hash| {
            let bytes = built.object_bytes.get(&object_hash).cloned();
            let peer_client = peer_client.clone();
            let base_url = base_url.clone();
            let peer_id = peer_id.to_string();
            let transfer_id = transfer_id.clone();
            async move {
                let Some(bytes) = bytes else {
                    return Err("USER_MIRROR_OBJECT_BYTES_MISSING".to_string());
                };
                stream_object(
                    &peer_client,
                    &base_url,
                    &peer_id,
                    &transfer_id,
                    &object_hash,
                    &bytes,
                )
                .await
                .map_err(|err| map_peer_error_code(&err))
            }
        })
        .buffer_unordered(OBJECT_PUT_PARALLELISM);
    while let Some(item) = puts.next().await {
        match item {
            Ok(()) => transferred += 1,
            Err(code) => return fail(&code),
        }
    }
    drop(puts);

    let commit_body = CommitUserMirrorRequest {
        source_device_id: source_device_id.to_string(),
        client_request_id: dest_client_request_id,
        selection_hash: selection_hash.to_string(),
        snapshot_hash: built.envelope.snapshot_hash.clone(),
        plan_token: plan.plan_token.clone(),
    };
    let path = format!("/api/agent-hub/user-mirror/{transfer_id}/commit");
    match post_json_bound::<CommitUserMirrorResponse, _>(
        peer_client,
        &base_url,
        &path,
        &commit_body,
        peer_id,
        PeerTimeoutClass::long_running(Duration::from_secs(900)),
    )
    .await
    {
        Ok(resp) => PeerPushOutcome {
            peer_device_id: peer_id.to_string(),
            status: TargetPushStatus::Committed,
            error_code: None,
            transfer_id: Some(transfer_id),
            missing_object_count: missing.len() as u32,
            transferred_object_count: transferred,
            dest_result: Some(resp.result),
        },
        Err(err) => fail(&map_peer_error_code(&err)),
    }
}

/// health 一次：两能力 + device_id 精确匹配；失败不打任何 push 路由。
async fn ensure_user_mirror_peer_binding(
    peer: &PeerClient,
    base_url: &str,
    peer_id: &str,
) -> Result<(), String> {
    let health = peer
        .health_info(base_url)
        .await
        .map_err(|err| map_peer_error_code(&err))?;
    let info = health.protocol_info();
    if !info.supports(CAPABILITY_USER_MIRROR_V1) {
        return Err(USER_MIRROR_CAPABILITY_UNSUPPORTED.to_string());
    }
    if !info.supports(CAPABILITY_DEVICE_REQUEST_BINDING_V1) {
        return Err(USER_MIRROR_CAPABILITY_UNSUPPORTED.to_string());
    }
    if health.device_id.trim() != peer_id.trim() {
        return Err(USER_MIRROR_DEVICE_ID_MISMATCH.to_string());
    }
    Ok(())
}

async fn stream_object(
    peer: &PeerClient,
    base_url: &str,
    peer_id: &str,
    transfer_id: &str,
    object_hash: &str,
    bytes: &[u8],
) -> Result<(), PeerCallError> {
    let total = bytes.len() as u64;
    // 空文件 blob 的 SHA 仍在 envelope.objects 里；不 PUT 则 dest commit 报 unverified。
    if total == 0 {
        let chunk_sha = sha256_hex(&[]);
        let url = format!(
            "{base_url}/api/agent-hub/user-mirror/{transfer_id}/objects/{object_hash}?offset=0&chunkSha256={chunk_sha}"
        );
        let resp: PutObjectResponse = put_bytes_bound(peer, &url, peer_id, Vec::new()).await?;
        if !resp.verified {
            return Err(PeerCallError::InvalidResponse {
                url,
                reason: "user_mirror_empty_object_unverified".to_string(),
            });
        }
        return Ok(());
    }
    let mut offset: u64 = 0;
    while offset < total {
        let end = std::cmp::min(offset as usize + AGENT_HUB_MAX_CHUNK_BYTES, bytes.len());
        let chunk = &bytes[offset as usize..end];
        let chunk_sha = sha256_hex(chunk);
        let url = format!(
            "{base_url}/api/agent-hub/user-mirror/{transfer_id}/objects/{object_hash}?offset={offset}&chunkSha256={chunk_sha}"
        );
        let resp: PutObjectResponse = put_bytes_bound(peer, &url, peer_id, chunk.to_vec()).await?;
        if resp.received_bytes < offset {
            return Err(PeerCallError::InvalidResponse {
                url: url.clone(),
                reason: format!(
                    "user_mirror_peer_offset_regressed:was={offset}:now={}",
                    resp.received_bytes
                ),
            });
        }
        if resp.received_bytes == offset && !chunk.is_empty() && !resp.verified {
            return Err(PeerCallError::InvalidResponse {
                url,
                reason: "user_mirror_chunk_no_progress".to_string(),
            });
        }
        offset = if resp.verified {
            total
        } else {
            resp.received_bytes
        };
    }
    Ok(())
}

async fn post_json_bound<T, B>(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    body: &B,
    expected_device_id: &str,
    timeout: Duration,
) -> Result<T, PeerCallError>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .post(&url)
        .timeout(timeout)
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .json(body)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.clone(),
            source: e,
        })?;
    crate::net::peer_error::parse_peer_response::<T>(resp, &url).await
}

async fn put_bytes_bound(
    peer: &PeerClient,
    url: &str,
    expected_device_id: &str,
    body: Vec<u8>,
) -> Result<PutObjectResponse, PeerCallError> {
    let resp = peer
        .http_client()
        .put(url)
        .timeout(OBJECT_CHUNK_TIMEOUT)
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
        .body(body)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.to_string(),
            source: e,
        })?;
    crate::net::peer_error::parse_peer_response::<PutObjectResponse>(resp, url).await
}

fn map_peer_error_code(err: &PeerCallError) -> String {
    match err {
        PeerCallError::Unsupported { .. } => USER_MIRROR_CAPABILITY_UNSUPPORTED.to_string(),
        PeerCallError::Network { .. } => USER_MIRROR_PEER_OFFLINE.to_string(),
        PeerCallError::InvalidResponse { reason, .. } => {
            if reason.contains("device_id") {
                USER_MIRROR_DEVICE_ID_MISMATCH.to_string()
            } else {
                "USER_MIRROR_INVALID_RESPONSE".to_string()
            }
        }
        PeerCallError::Remote { code, message, .. } => {
            let code = code.trim();
            let message = message.trim();
            if message.is_empty() && code.is_empty() {
                "USER_MIRROR_REMOTE".to_string()
            } else if message.is_empty() || message == code {
                code.to_string()
            } else if code.is_empty() {
                message.to_string()
            } else {
                format!("{code}:{message}")
            }
        }
    }
}

fn push_peer_ids(plan: &UserMirrorPlanDto) -> Vec<String> {
    let mut ids = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in &plan.peer_device_ids {
        let trimmed = id.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        ids.push(trimmed.to_string());
    }
    if ids.is_empty() {
        let dest = plan.destination_device_id.trim();
        if !dest.is_empty() {
            ids.push(dest.to_string());
        }
    }
    ids
}

fn parse_plan(plan_json: &str) -> Result<UserMirrorPlanDto, AppError> {
    serde_json::from_str(plan_json).map_err(AppError::from)
}

fn outcome_unknown_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
) -> UserMirrorResultDto {
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial: true,
        agents: plan
            .agents
            .iter()
            .map(|agent| UserMirrorAgentResultDto {
                target: agent.target,
                state: UserMirrorItemState::OutcomeUnknown,
                error_code: None,
                message: None,
            })
            .collect(),
    }
}

fn failed_code_result(
    request: &ApplyUserMirrorRequest,
    plan: &UserMirrorPlanDto,
    code: &str,
    state: UserMirrorItemState,
) -> UserMirrorResultDto {
    UserMirrorResultDto {
        plan_token: request.plan_token.clone(),
        client_request_id: request.client_request_id.clone(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial: true,
        agents: plan
            .agents
            .iter()
            .map(|agent| UserMirrorAgentResultDto {
                target: agent.target,
                state,
                error_code: Some(code.to_string()),
                message: Some(code.to_string()),
            })
            .collect(),
    }
}

fn aggregate_result(
    request: &ApplyUserMirrorRequest,
    plan: &UserMirrorPlanDto,
    outcomes: &[PeerPushOutcome],
) -> UserMirrorResultDto {
    let any_failed = outcomes
        .iter()
        .any(|o| o.status == TargetPushStatus::Failed);
    let dest_result = outcomes
        .iter()
        .find_map(|o| o.dest_result.clone())
        .or_else(|| {
            outcomes.iter().find_map(|o| {
                o.error_code.as_ref().map(|code| {
                    failed_code_result(request, plan, code, UserMirrorItemState::Failed)
                })
            })
        });
    let mut result = dest_result.unwrap_or_else(|| {
        failed_code_result(
            request,
            plan,
            USER_MIRROR_PEER_OFFLINE,
            UserMirrorItemState::Failed,
        )
    });
    result.plan_token = request.plan_token.clone();
    result.client_request_id = request.client_request_id.clone();
    result.source_device_id = plan.source_device_id.clone();
    result.destination_device_id = plan.destination_device_id.clone();
    if any_failed {
        result.partial = true;
    }
    result
}

async fn complete_plan(
    state: &AppState,
    request: &ApplyUserMirrorRequest,
    result: &UserMirrorResultDto,
) -> Result<(), AppError> {
    state
        .agent_hub_repo
        .complete_user_mirror_plan(
            &request.plan_token,
            &request.client_request_id,
            &serde_json::to_string(result)?,
        )
        .await
}

async fn persist_source_request(
    state: &AppState,
    request_id: &str,
    selection_hash: &str,
    snapshot_hash: &str,
    plan_token: &str,
) -> Result<(), AppError> {
    let now = Utc::now().to_rfc3339();
    let selection_json = serde_json::json!({ "planToken": plan_token }).to_string();
    with_shared_write_lease(&state.maintenance_gate, async {
        sqlx::query(
            "INSERT INTO agent_hub_source_push_requests
             (request_id, selection_mode, selection_json, selection_hash, snapshot_hash,
              status, created_at, updated_at)
             VALUES (?, 'user_mirror', ?, ?, ?, 'running', ?, ?)
             ON CONFLICT(request_id) DO UPDATE SET
               selection_mode='user_mirror',
               selection_json=excluded.selection_json,
               selection_hash=excluded.selection_hash,
               snapshot_hash=excluded.snapshot_hash,
               status='running',
               updated_at=excluded.updated_at",
        )
        .bind(request_id)
        .bind(&selection_json)
        .bind(selection_hash)
        .bind(snapshot_hash)
        .bind(&now)
        .bind(&now)
        .execute(&state.agent_hub_repo.pool())
        .await?;
        Ok(())
    })
    .await
}

async fn persist_target_pending(
    state: &AppState,
    request_id: &str,
    peer_id: &str,
    label: &str,
    client_request_id: &str,
) -> Result<(), AppError> {
    let now = Utc::now().to_rfc3339();
    with_shared_write_lease(&state.maintenance_gate, async {
        sqlx::query(
            "INSERT INTO agent_hub_source_push_targets
             (request_id, peer_device_id, peer_label, client_request_id, status, retryable,
              error_code, transfer_id, missing_object_count, transferred_object_count,
              kind, created_at, updated_at)
             VALUES (?, ?, ?, ?, 'pending', 0, NULL, NULL, 0, 0, ?, ?, ?)
             ON CONFLICT(request_id, peer_device_id) DO UPDATE SET
               peer_label=excluded.peer_label,
               client_request_id=agent_hub_source_push_targets.client_request_id,
               kind=excluded.kind,
               status=CASE
                 WHEN agent_hub_source_push_targets.status = 'committed'
                 THEN agent_hub_source_push_targets.status
                 ELSE 'pending'
               END,
               updated_at=excluded.updated_at",
        )
        .bind(request_id)
        .bind(peer_id)
        .bind(label)
        .bind(client_request_id)
        .bind(SOURCE_PUSH_KIND_USER_MIRROR)
        .bind(&now)
        .bind(&now)
        .execute(&state.agent_hub_repo.pool())
        .await?;
        Ok(())
    })
    .await
}

async fn persist_target_outcome(
    state: &AppState,
    request_id: &str,
    outcome: &PeerPushOutcome,
) -> Result<(), AppError> {
    let now = Utc::now().to_rfc3339();
    with_shared_write_lease(&state.maintenance_gate, async {
        sqlx::query(
            "UPDATE agent_hub_source_push_targets
             SET status = ?, retryable = ?, error_code = ?, transfer_id = ?,
                 missing_object_count = ?, transferred_object_count = ?, kind = ?, updated_at = ?
             WHERE request_id = ? AND peer_device_id = ?",
        )
        .bind(outcome.status.as_str())
        .bind(0)
        .bind(&outcome.error_code)
        .bind(&outcome.transfer_id)
        .bind(outcome.missing_object_count as i64)
        .bind(outcome.transferred_object_count as i64)
        .bind(SOURCE_PUSH_KIND_USER_MIRROR)
        .bind(&now)
        .bind(request_id)
        .bind(&outcome.peer_device_id)
        .execute(&state.agent_hub_repo.pool())
        .await?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{
        apply_push_user_mirror, map_peer_error_code, USER_MIRROR_CAPABILITY_UNSUPPORTED,
        USER_MIRROR_DEVICE_ID_MISMATCH,
    };
    use crate::agent_hub::replication::receiver::{PreparePushResponse, PutObjectResponse};
    use crate::agent_hub::replication::sender::{
        list_failed_source_push_targets, SOURCE_PUSH_KIND_USER_MIRROR,
    };
    use crate::agent_hub::user_mirror::inventory::build_local_user_mirror_inventory;
    use crate::agent_hub::user_mirror::models::{
        ApplyUserMirrorRequest, UserMirrorDirection, UserMirrorInventoryDto,
        USER_MIRROR_CAPABILITY_UNSUPPORTED as UNSUPPORTED,
    };
    use crate::agent_hub::user_mirror::receive::{
        CommitUserMirrorRequest, CommitUserMirrorResponse, PrepareUserMirrorRequest,
    };
    use crate::agent_hub::user_mirror::service::preview_from_two_inventories;
    use crate::attention::agent_hub_source::project_source_push_failure_item;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::models::device::Device;
    use crate::net::peer_error::PeerCallError;
    use crate::net::protocol::{CAPABILITY_DEVICE_REQUEST_BINDING_V1, CAPABILITY_USER_MIRROR_V1};
    use crate::state::AppState;
    use axum::extract::Query;
    use axum::routing::{get, post, put};
    use axum::Router;
    use chrono::Utc;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct SourceEnv {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        state: AppState,
        home: PathBuf,
        dest_claude: PathBuf,
    }

    async fn seed_source_env() -> SourceEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("source-home");
        let data = tmp.path().join("source-data");
        let dest_claude = tmp.path().join("dest-home/.claude/CLAUDE.md");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&data).expect("data");
        fs::create_dir_all(dest_claude.parent().expect("parent")).expect("dest parent");
        fs::write(&dest_claude, "OLD-DEST").expect("dest claude");
        let data_guard = install_data_dir_env(Some(data.to_str().expect("utf8 data")));
        let home_guard = install_env_var("HOME", Some(home.to_str().expect("utf8 home")));
        let ui = Arc::new(RecordingBackendUi::default());
        let state = build_app_state(ui).await.expect("state");
        SourceEnv {
            _tmp: tmp,
            _guards: vec![Box::new(data_guard), Box::new(home_guard)],
            state,
            home,
            dest_claude,
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    fn empty_dest_inventory(device: &str) -> UserMirrorInventoryDto {
        UserMirrorInventoryDto {
            source_device_id: device.to_string(),
            inventory_snapshot_hash: format!("dest-{device}"),
            refreshed_at: "2026-08-23T00:00:00Z".into(),
            agents: Vec::new(),
            credential_bearing_count: 0,
        }
    }

    struct FakeCounters {
        health: AtomicUsize,
        old_prepare: AtomicUsize,
        prepare: AtomicUsize,
        object: AtomicUsize,
        commit: AtomicUsize,
        health_device_id: String,
        capabilities: Vec<String>,
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        bindings: Mutex<Vec<crate::agent_hub::user_mirror::UserMirrorObjectBinding>>,
        dest_claude: PathBuf,
    }

    impl FakeCounters {
        fn new(device_id: &str, caps: Vec<String>, dest_claude: PathBuf) -> Arc<Self> {
            Arc::new(Self {
                health: AtomicUsize::new(0),
                old_prepare: AtomicUsize::new(0),
                prepare: AtomicUsize::new(0),
                object: AtomicUsize::new(0),
                commit: AtomicUsize::new(0),
                health_device_id: device_id.to_string(),
                capabilities: caps,
                objects: Mutex::new(BTreeMap::new()),
                bindings: Mutex::new(Vec::new()),
                dest_claude,
            })
        }
    }

    fn supported_caps() -> Vec<String> {
        vec![
            "errors.envelope.v1".into(),
            CAPABILITY_USER_MIRROR_V1.into(),
            CAPABILITY_DEVICE_REQUEST_BINDING_V1.into(),
        ]
    }

    async fn spawn_fake_peer(counters: Arc<FakeCounters>) -> (String, tokio::task::JoinHandle<()>) {
        let c_health = Arc::clone(&counters);
        let c_old = Arc::clone(&counters);
        let c_prep = Arc::clone(&counters);
        let c_obj = Arc::clone(&counters);
        let c_commit = Arc::clone(&counters);
        let app = Router::new()
            .route(
                "/api/health",
                get(move || {
                    let c = Arc::clone(&c_health);
                    async move {
                        c.health.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({
                            "ok": true,
                            "device_id": c.health_device_id,
                            "device_name": "Peer Test",
                            "http_port": 0,
                            "ts": Utc::now().timestamp(),
                            "protocol_version": 1,
                            "capabilities": c.capabilities.clone(),
                        }))
                    }
                }),
            )
            .route(
                "/api/agent-hub/push/prepare",
                post(move |_body: axum::Json<serde_json::Value>| {
                    let c = Arc::clone(&c_old);
                    async move {
                        c.old_prepare.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({ "transferId": "legacy" }))
                    }
                }),
            )
            .route(
                "/api/agent-hub/user-mirror/prepare",
                post(move |body: axum::Json<PrepareUserMirrorRequest>| {
                    let c = Arc::clone(&c_prep);
                    async move {
                        c.prepare.fetch_add(1, Ordering::SeqCst);
                        *c.bindings.lock().expect("bindings") = body.item_bindings.clone();
                        let missing: Vec<String> = body
                            .envelope
                            .objects
                            .iter()
                            .map(|object| object.hash.clone())
                            .collect();
                        axum::Json(PreparePushResponse {
                            transfer_id: "umirror-test-1".into(),
                            status: "prepared".into(),
                            selection_hash: body.selection_hash.clone(),
                            snapshot_hash: body.envelope.snapshot_hash.clone(),
                            missing_object_hashes: missing,
                            missing_revision_ids: vec![],
                            outcome: None,
                        })
                    }
                }),
            )
            .route(
                "/api/agent-hub/user-mirror/:tid/objects/:oh",
                put(
                    move |axum::extract::Path((tid, oh)): axum::extract::Path<(String, String)>,
                          Query(q): Query<crate::net::routes::agent_hub::PutObjectQuery>,
                          body: axum::body::Bytes| {
                        let c = Arc::clone(&c_obj);
                        async move {
                            c.object.fetch_add(1, Ordering::SeqCst);
                            let mut map = c.objects.lock().expect("objects");
                            let entry = map.entry(oh.clone()).or_default();
                            assert_eq!(entry.len() as u64, q.offset);
                            entry.extend_from_slice(&body);
                            let received = entry.len() as u64;
                            axum::Json(PutObjectResponse {
                                transfer_id: tid,
                                object_hash: oh,
                                received_bytes: received,
                                expected_size: received,
                                verified: true,
                            })
                        }
                    },
                ),
            )
            .route(
                "/api/agent-hub/user-mirror/:tid/commit",
                post(
                    move |axum::extract::Path(tid): axum::extract::Path<String>,
                          _body: axum::Json<CommitUserMirrorRequest>| {
                        let c = Arc::clone(&c_commit);
                        async move {
                            c.commit.fetch_add(1, Ordering::SeqCst);
                            let objects = c.objects.lock().expect("objects").clone();
                            let bindings = c.bindings.lock().expect("bindings").clone();
                            if let Some(binding) = bindings.iter().find(|b| {
                                b.logical_id
                                    .as_deref()
                                    .is_some_and(|id| id.contains("CLAUDE.md"))
                            }) {
                                if let Some(bytes) = objects.get(&binding.object_hash) {
                                    fs::write(&c.dest_claude, bytes).expect("write dest claude");
                                }
                            }
                            axum::Json(CommitUserMirrorResponse {
                                transfer_id: tid,
                                status: "committed".into(),
                                result: crate::agent_hub::user_mirror::UserMirrorResultDto {
                                    plan_token: "plan".into(),
                                    client_request_id: "req".into(),
                                    source_device_id: "src".into(),
                                    destination_device_id: c.health_device_id.clone(),
                                    partial: false,
                                    agents: vec![],
                                },
                            })
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), handle)
    }

    fn register_peer(state: &AppState, peer_id: &str, base: &str) {
        let without = base.trim_start_matches("http://");
        let (host, port_s) = without.rsplit_once(':').unwrap();
        let device = Device {
            id: peer_id.into(),
            name: "Peer Test".into(),
            host: host.to_string(),
            port: port_s.parse().unwrap(),
            last_seen: Utc::now(),
            online: true,
            proto_version: 1,
            capabilities: supported_caps(),
        };
        state
            .devices
            .write()
            .expect("devices")
            .insert(peer_id.to_string(), device);
    }

    async fn persist_push_plan(state: &AppState, peer_id: &str) -> String {
        let source = build_local_user_mirror_inventory(state, state.device_id.as_str())
            .await
            .expect("inventory");
        let dest = empty_dest_inventory(peer_id);
        let mut plan = preview_from_two_inventories(
            state,
            &source,
            &dest,
            state.device_id.as_str(),
            peer_id,
            UserMirrorDirection::Push,
        )
        .await
        .expect("preview");
        plan.peer_device_ids = vec![peer_id.to_string()];
        sqlx::query("UPDATE agent_hub_user_mirror_plans SET plan_json = ? WHERE plan_token = ?")
            .bind(serde_json::to_string(&plan).unwrap())
            .bind(&plan.plan_token)
            .execute(&state.agent_hub_repo.pool())
            .await
            .expect("update plan peers");
        plan.plan_token
    }

    /// Business Logic: Push 方向必须走 sender，禁止 fail-closed 占位。
    #[test]
    fn push_direction_is_wired_on_owner_service() {
        let src = include_str!("service.rs");
        assert!(src.contains("super::push::apply_push_user_mirror"));
        assert!(!src.contains("user_mirror_push_sender_not_wired"));
    }

    /// Business Logic: 缺 user-mirror 能力不得打旧 push/prepare。
    /// Code Logic: health 无 token → USER_MIRROR_CAPABILITY_UNSUPPORTED，old_prepare=0。
    #[tokio::test]
    async fn missing_capability_never_hits_legacy_push_prepare() {
        let env = seed_source_env().await;
        write(env.home.join(".claude/CLAUDE.md").as_path(), "FROM-SRC");
        let counters = FakeCounters::new(
            "peer-test",
            vec!["errors.envelope.v1".into()],
            env.dest_claude.clone(),
        );
        let (base, handle) = spawn_fake_peer(Arc::clone(&counters)).await;
        register_peer(&env.state, "peer-test", &base);
        let plan_token = persist_push_plan(&env.state, "peer-test").await;
        let result = apply_push_user_mirror(
            &env.state,
            ApplyUserMirrorRequest {
                plan_token,
                client_request_id: "req-no-cap".into(),
            },
        )
        .await
        .expect("apply returns per-peer result");
        assert!(result.partial);
        assert_eq!(counters.old_prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.object.load(Ordering::SeqCst), 0);
        let failed = list_failed_source_push_targets(&env.state)
            .await
            .expect("failed");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].kind, SOURCE_PUSH_KIND_USER_MIRROR);
        assert_eq!(failed[0].error_code.as_deref(), Some(UNSUPPORTED));
        let item = project_source_push_failure_item(&failed[0]);
        assert_eq!(item.id, "agent-hub:mirror-failed:req-no-cap:peer-test");
        assert!(item.summary.contains(USER_MIRROR_CAPABILITY_UNSUPPORTED));
        assert!(!item.summary.contains("missing="));
        handle.abort();
    }

    /// Business Logic: 有能力时 dest 夹具 CLAUDE.md 必须等于源。
    /// Code Logic: freeze 一次 → 新 prepare/PUT/commit；旧 prepare=0。
    #[tokio::test]
    async fn capable_peer_commit_writes_source_claude_md() {
        let env = seed_source_env().await;
        write(env.home.join(".claude/CLAUDE.md").as_path(), "FROM-SRC");
        let counters = FakeCounters::new("peer-test", supported_caps(), env.dest_claude.clone());
        let (base, handle) = spawn_fake_peer(Arc::clone(&counters)).await;
        register_peer(&env.state, "peer-test", &base);
        let plan_token = persist_push_plan(&env.state, "peer-test").await;
        let result = apply_push_user_mirror(
            &env.state,
            ApplyUserMirrorRequest {
                plan_token,
                client_request_id: "req-ok".into(),
            },
        )
        .await
        .expect("apply");
        assert!(!result.partial, "result={result:?}");
        assert_eq!(counters.old_prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 1);
        assert!(counters.object.load(Ordering::SeqCst) >= 1);
        assert_eq!(counters.commit.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read_to_string(&env.dest_claude).unwrap(), "FROM-SRC");
        handle.abort();
    }

    /// Business Logic: health.device_id 不匹配则 fail-closed，不传 objects。
    /// Code Logic: prepare/object/commit 均为 0。
    #[tokio::test]
    async fn device_id_mismatch_does_not_send_objects() {
        let env = seed_source_env().await;
        write(env.home.join(".claude/CLAUDE.md").as_path(), "FROM-SRC");
        let counters = FakeCounters::new("other-device", supported_caps(), env.dest_claude.clone());
        let (base, handle) = spawn_fake_peer(Arc::clone(&counters)).await;
        register_peer(&env.state, "peer-test", &base);
        let plan_token = persist_push_plan(&env.state, "peer-test").await;
        let result = apply_push_user_mirror(
            &env.state,
            ApplyUserMirrorRequest {
                plan_token,
                client_request_id: "req-mismatch".into(),
            },
        )
        .await
        .expect("apply");
        assert!(result.partial);
        assert_eq!(counters.old_prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.prepare.load(Ordering::SeqCst), 0);
        assert_eq!(counters.object.load(Ordering::SeqCst), 0);
        assert_eq!(counters.commit.load(Ordering::SeqCst), 0);
        let failed = list_failed_source_push_targets(&env.state)
            .await
            .expect("failed");
        assert_eq!(
            failed[0].error_code.as_deref(),
            Some(USER_MIRROR_DEVICE_ID_MISMATCH)
        );
        handle.abort();
    }

    /// Business Logic: 一 peer 失败不回滚其它成功 peer。
    /// Code Logic: 无能力 peer 零旧 prepare；有能力 peer CLAUDE.md 对齐。
    #[tokio::test]
    async fn one_peer_failure_does_not_rollback_other() {
        let env = seed_source_env().await;
        write(env.home.join(".claude/CLAUDE.md").as_path(), "FROM-SRC");
        let dest_ok = env.dest_claude.clone();
        let dest_fail = env._tmp.path().join("dest-fail/.claude/CLAUDE.md");
        fs::create_dir_all(dest_fail.parent().unwrap()).unwrap();
        fs::write(&dest_fail, "OLD-FAIL").unwrap();
        let ok = FakeCounters::new("peer-ok", supported_caps(), dest_ok.clone());
        let bad = FakeCounters::new(
            "peer-bad",
            vec!["errors.envelope.v1".into()],
            dest_fail.clone(),
        );
        let (base_ok, h_ok) = spawn_fake_peer(Arc::clone(&ok)).await;
        let (base_bad, h_bad) = spawn_fake_peer(Arc::clone(&bad)).await;
        register_peer(&env.state, "peer-ok", &base_ok);
        register_peer(&env.state, "peer-bad", &base_bad);

        let source = build_local_user_mirror_inventory(&env.state, env.state.device_id.as_str())
            .await
            .unwrap();
        let dest = empty_dest_inventory("peer-ok");
        let mut plan = preview_from_two_inventories(
            &env.state,
            &source,
            &dest,
            env.state.device_id.as_str(),
            "peer-ok",
            UserMirrorDirection::Push,
        )
        .await
        .unwrap();
        plan.peer_device_ids = vec!["peer-ok".into(), "peer-bad".into()];
        sqlx::query("UPDATE agent_hub_user_mirror_plans SET plan_json = ? WHERE plan_token = ?")
            .bind(serde_json::to_string(&plan).unwrap())
            .bind(&plan.plan_token)
            .execute(&env.state.agent_hub_repo.pool())
            .await
            .unwrap();

        let result = apply_push_user_mirror(
            &env.state,
            ApplyUserMirrorRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-partial".into(),
            },
        )
        .await
        .expect("apply");
        assert!(result.partial);
        assert_eq!(ok.old_prepare.load(Ordering::SeqCst), 0);
        assert_eq!(bad.old_prepare.load(Ordering::SeqCst), 0);
        assert_eq!(bad.prepare.load(Ordering::SeqCst), 0);
        assert_eq!(ok.commit.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read_to_string(&dest_ok).unwrap(), "FROM-SRC");
        assert_eq!(fs::read_to_string(&dest_fail).unwrap(), "OLD-FAIL");
        h_ok.abort();
        h_bad.abort();
    }

    /// Business Logic（为什么需要这个测试）:
    ///     全量 user-mirror 对象数可达四位；串行 PUT 会在 overlay RTT 下打满 apply 墙钟。
    ///
    /// Code Logic（这个测试做什么）:
    ///     锁定 missing object 走 `buffer_unordered(OBJECT_PUT_PARALLELISM)` 且并发为 16。
    #[test]
    fn object_puts_are_parallel() {
        let src = include_str!("push.rs");
        assert!(src.contains("buffer_unordered(OBJECT_PUT_PARALLELISM)"));
        assert!(
            src.contains("const OBJECT_PUT_PARALLELISM: usize = 16"),
            "object put parallelism must stay high enough for full-home mirrors"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Skill 树里的空文件会进入 envelope；漏传空 blob 会让对端 commit 全员失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     锁定 `stream_object` 对 0 字节对象仍 PUT offset=0。
    #[test]
    fn empty_object_is_put_once() {
        let src = include_str!("push.rs");
        assert!(src.contains("user_mirror_empty_object_unverified"));
        assert!(src.contains("if total == 0"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端 prepare 的 `code=validation_error` 必须带上信封诊断，不能只剩分类 token。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 Remote 信封；断言 mapped code 含 manifest/referential 细节。
    #[test]
    fn map_peer_error_code_keeps_remote_validation_message() {
        let err = PeerCallError::Remote {
            url: "http://peer/api/agent-hub/user-mirror/prepare".into(),
            status: 400,
            code: "validation_error".into(),
            message:
                "agent_hub_push_invalid_manifest:snapshot_referential:revision_unknown_tree:hash_len=64"
                    .into(),
            request_id: "req".into(),
            retryable: false,
            legacy: false,
            details: Box::new(serde_json::json!({})),
        };
        let mapped = map_peer_error_code(&err);
        assert!(
            mapped.contains("validation_error") && mapped.contains("revision_unknown_tree"),
            "{mapped}"
        );
    }
}
