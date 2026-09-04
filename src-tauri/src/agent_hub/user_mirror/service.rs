//! user_mirror/service — 本机 Pull 的 preview / apply / get 门面
//!
//! Business Logic（为什么需要这个模块）:
//!     镜像必须先 preview 落库，apply 原子 claim 后再写盘；同 request 重放结果，
//!     崩溃未完成经 get 暴露 outcomeUnknown，禁止跳过 plan 直接覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     preview 插入 `agent_hub_user_mirror_plans`；apply claim → 调 apply.rs → complete；
//!     get 按 client_request_id 读 result 或拼 outcomeUnknown。本地 Pull 可用两份 AppState。

use super::apply::apply_user_mirror_instructions_with_env;
use super::inventory::{
    build_local_user_mirror_inventory, build_local_user_mirror_inventory_with_env,
};
use super::ledger::{UserMirrorClaim, UserMirrorPlanRecord};
use super::models::{
    ApplyUserMirrorRequest, PreviewUserMirrorRequest, UserMirrorAgentResultDto,
    UserMirrorDirection, UserMirrorInventoryDto, UserMirrorItemState, UserMirrorPlanDto,
    UserMirrorResultDto, USER_MIRROR_CAPABILITY_UNSUPPORTED, USER_MIRROR_DEST_MAX_TOTAL_BYTES,
    USER_MIRROR_PEER_OFFLINE, USER_MIRROR_PREVIEW_REQUIRED, USER_MIRROR_STALE,
    USER_MIRROR_TRANSFER_LIMIT,
};
use super::preview::preview_from_two_inventories as build_preview_from_two_inventories;
use super::receive::{UserMirrorSelectionQuery, UserMirrorSelectionResponse};
use super::selection::{
    filter_inventory_for_freeze, freeze_user_mirror_selection, UserMirrorObjectBinding,
};
use super::store_migration::migrate_portable_assets_into_store;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::targets::TargetEnvironment;
use crate::error::AppError;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::CAPABILITY_USER_MIRROR_V1;
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// 对端全 Agent inventory 扫描预算：两侧均可接近 portable 的 30s，再加 LAN 余量。
const USER_MIRROR_PEER_INVENTORY_TIMEOUT: Duration = Duration::from_secs(60);

/// 用两份已构建 inventory 生成 preview 并写入 dest 端 plan ledger。
///
/// Business Logic（为什么需要这个函数）:
///     本地-本地测试与后续 LAN dest apply 都需要把 plan 绑到 dest owner 的 SQLite；
///     纯 diff 的空 token 不能当正式 apply 凭证。
///
/// Code Logic（这个函数做什么）:
///     调用 preview.rs 填 token/TTL，再 `insert_user_mirror_plan`；plan_json 为 DTO。
pub async fn preview_from_two_inventories(
    dest_state: &AppState,
    source: &UserMirrorInventoryDto,
    dest: &UserMirrorInventoryDto,
    source_device_id: &str,
    dest_device_id: &str,
    direction: UserMirrorDirection,
) -> Result<UserMirrorPlanDto, AppError> {
    let plan = build_preview_from_two_inventories(
        source,
        dest,
        source_device_id,
        dest_device_id,
        direction,
    );
    persist_plan(dest_state, &plan).await?;
    Ok(plan)
}

/// 本地 Pull：从 source/dest 两份 AppState 扫 inventory 并在 dest 落 plan。
///
/// Business Logic（为什么需要这个函数）:
///     T7 先接通本机双 AppState Pull；LAN 源 inventory 由后续路由替换扫描入口。
///
/// Code Logic（这个函数做什么）:
///     用进程环境扫两端 `build_local`；dest 为 apply 端并插入 plan 行。
pub async fn preview_user_mirror(
    dest_state: &AppState,
    source_state: &AppState,
    request: PreviewUserMirrorRequest,
) -> Result<UserMirrorPlanDto, AppError> {
    let env = TargetEnvironment::from_process();
    preview_user_mirror_with_envs(dest_state, source_state, request, &env, &env).await
}

/// 注入源/目标环境的本地 preview（DualEnv 测试与生产共用落库规则）。
///
/// Business Logic: 隔离 HOME 必须与生产走同一 inventory 白名单，plan 仍写 dest DB。
/// Code Logic: 分别扫 source/dest inventory，再 `preview_from_two_inventories` 落库。
pub(crate) async fn preview_user_mirror_with_envs(
    dest_state: &AppState,
    source_state: &AppState,
    request: PreviewUserMirrorRequest,
    source_env: &TargetEnvironment,
    dest_env: &TargetEnvironment,
) -> Result<UserMirrorPlanDto, AppError> {
    let source_device_id = match request.direction {
        UserMirrorDirection::Pull => request
            .source_device_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?
            .to_string(),
        UserMirrorDirection::Push => source_state.device_id.as_str().to_string(),
    };
    let dest_device_id = dest_state.device_id.as_str();
    let source_inventory =
        build_local_user_mirror_inventory_with_env(source_state, &source_device_id, source_env)
            .await?;
    let dest_inventory =
        build_local_user_mirror_inventory_with_env(dest_state, dest_device_id, dest_env).await?;
    preview_from_two_inventories(
        dest_state,
        &source_inventory,
        &dest_inventory,
        &source_device_id,
        dest_device_id,
        request.direction,
    )
    .await
}

/// 应用已预览镜像（claim → 写盘+extras → complete）。
///
/// Business Logic（为什么需要这个函数）:
///     dest owner 必须按 preview 覆盖；同 request 重放；缺 plan 强制重新预览。
///
/// Code Logic（这个函数做什么）:
///     用进程 `TargetEnvironment` 调 `apply_user_mirror_with_env`。
pub async fn apply_user_mirror(
    dest_state: &AppState,
    request: ApplyUserMirrorRequest,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
) -> Result<UserMirrorResultDto, AppError> {
    apply_user_mirror_with_env(
        dest_state,
        request,
        objects,
        bindings,
        &TargetEnvironment::from_process(),
    )
    .await
}

/// 注入 dest 环境的 apply（测试隔离 HOME）。
///
/// Business Logic: DualEnv 不得扫到开发者真实配置，写盘规则与生产相同。
/// Code Logic: claim 三态；Claimed 调 apply.rs 后 complete；失败也 complete 以免假死 Pending。
pub(crate) async fn apply_user_mirror_with_env(
    dest_state: &AppState,
    request: ApplyUserMirrorRequest,
    objects: &BTreeMap<String, Vec<u8>>,
    bindings: &[UserMirrorObjectBinding],
    env: &TargetEnvironment,
) -> Result<UserMirrorResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let claim = dest_state
        .agent_hub_repo
        .claim_user_mirror_plan(&request.plan_token, &request.client_request_id)
        .await?;
    match claim {
        UserMirrorClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
        UserMirrorClaim::Pending => {
            let row = dest_state
                .agent_hub_repo
                .get_user_mirror_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
            let plan = parse_plan(&row.plan_json)?;
            Ok(outcome_unknown_result(
                &request.plan_token,
                &request.client_request_id,
                &plan,
            ))
        }
        UserMirrorClaim::Claimed(record) => {
            let mut plan = parse_plan(&record.plan_json)?;
            // apply 的 claim 从 DB plan_json 解析；request.selection 在此合并进内存 plan
            // （request 优先，push-dest 则保留 dest plan 已携带的 selection）。落库 plan 不改。
            if request.selection.is_some() {
                plan.selection = request.selection.clone();
            }
            if plan.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
                let fail =
                    failed_stale_result(&request.plan_token, &request.client_request_id, &plan);
                dest_state
                    .agent_hub_repo
                    .complete_user_mirror_plan(
                        &request.plan_token,
                        &request.client_request_id,
                        &serde_json::to_string(&fail)?,
                    )
                    .await?;
                return Err(AppError::conflict(USER_MIRROR_STALE));
            }
            let result = match apply_user_mirror_instructions_with_env(
                dest_state, &plan, objects, bindings, env,
            )
            .await
            {
                Ok(agents) => build_result(
                    &request.plan_token,
                    &request.client_request_id,
                    &plan,
                    agents,
                ),
                Err(error) => {
                    let fail = failed_apply_result(
                        &request.plan_token,
                        &request.client_request_id,
                        &plan,
                        &error,
                    );
                    dest_state
                        .agent_hub_repo
                        .complete_user_mirror_plan(
                            &request.plan_token,
                            &request.client_request_id,
                            &serde_json::to_string(&fail)?,
                        )
                        .await?;
                    return Ok(fail);
                }
            };
            dest_state
                .agent_hub_repo
                .complete_user_mirror_plan(
                    &request.plan_token,
                    &request.client_request_id,
                    &serde_json::to_string(&result)?,
                )
                .await?;
            Ok(result)
        }
    }
}

/// 按 clientRequestId 读取镜像结果；未完成返回 outcomeUnknown。
///
/// Business Logic（为什么需要这个函数）:
///     UI/重试在不确定窗口必须诚实未知，不得把崩溃中的 apply 标成功。
///
/// Code Logic（这个函数做什么）:
///     查 dest ledger；有 result_json 则反序列化，否则按 plan 拼 OutcomeUnknown。
pub async fn get_user_mirror(
    dest_state: &AppState,
    client_request_id: &str,
) -> Result<UserMirrorResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(USER_MIRROR_PREVIEW_REQUIRED));
    }
    let row = dest_state
        .agent_hub_repo
        .get_user_mirror_by_request_id(client_request_id)
        .await?
        .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
    if let Some(result_json) = row.result_json.as_deref() {
        return serde_json::from_str(result_json).map_err(AppError::from);
    }
    let plan = parse_plan(&row.plan_json)?;
    Ok(outcome_unknown_result(
        &row.plan_token,
        client_request_id,
        &plan,
    ))
}

/// Owner 面用户级镜像门面（Tauri / control 共用，禁止 GUI 直连 peer）。
///
/// Business Logic（为什么需要这个结构体）:
///     桌面 invoke 与 loopback control 必须走同一套 preview/apply/get；
///     Pull 的 apply 只在本机 dest owner 执行，对端 HTTP 由 sidecar 发起。
///
/// Code Logic（这个结构体做什么）:
///     纯静态方法命名空间；preview 拉源 inventory 后落 plan，apply Pull 拉 objects 后写盘。
pub struct UserMirrorService;

impl UserMirrorService {
    /// 预览用户级镜像并在本机 owner 落 plan。
    ///
    /// Business Logic: Pull 选一台源设备；Push 以本机为源、对端为 dest 做 diff。
    /// Code Logic: 缺能力 fail-closed；写 plan ledger。
    pub async fn preview_user_mirror(
        state: &AppState,
        request: PreviewUserMirrorRequest,
    ) -> Result<UserMirrorPlanDto, AppError> {
        match request.direction {
            UserMirrorDirection::Pull => {
                let source_device_id = request
                    .source_device_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?
                    .to_string();
                let source = load_source_inventory(state, &source_device_id).await?;
                let dest =
                    build_local_user_mirror_inventory(state, state.device_id.as_str()).await?;
                preview_from_two_inventories(
                    state,
                    &source,
                    &dest,
                    &source_device_id,
                    state.device_id.as_str(),
                    UserMirrorDirection::Pull,
                )
                .await
            }
            UserMirrorDirection::Push => {
                let peer_ids: Vec<String> = {
                    let mut seen = std::collections::BTreeSet::new();
                    let mut ids = Vec::new();
                    for id in &request.peer_device_ids {
                        let trimmed = id.trim();
                        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                            continue;
                        }
                        ids.push(trimmed.to_string());
                    }
                    ids
                };
                let dest_device_id = peer_ids
                    .first()
                    .cloned()
                    .ok_or_else(|| AppError::validation(USER_MIRROR_PREVIEW_REQUIRED))?;
                // Push 以本机为源：preview 前先把 user-scope Skill/Command 收编进
                // portable-store（确认版本），扫描级故障经 `?` 传播。
                migrate_portable_assets_into_store(state).await?;
                let source =
                    build_local_user_mirror_inventory(state, state.device_id.as_str()).await?;
                let dest = load_source_inventory(state, &dest_device_id).await?;
                let mut plan = build_preview_from_two_inventories(
                    &source,
                    &dest,
                    state.device_id.as_str(),
                    &dest_device_id,
                    UserMirrorDirection::Push,
                );
                plan.peer_device_ids = peer_ids;
                persist_plan(state, &plan).await?;
                Ok(plan)
            }
        }
    }

    /// 应用已预览镜像（Pull 本机写盘；Push 源侧 freeze 后对每 peer dest commit）。
    ///
    /// Business Logic: 同 clientRequestId 幂等；GUI 不得把 apply 打到 LAN。
    /// Code Logic: 读 plan 方向并把 request.selection 合并进内存 plan（后续 collect /
    /// fan-out / 对端 prepare 携带）；Pull 收集源 objects 后调 dest apply；Push 走 sender。
    pub async fn apply_user_mirror(
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
        let mut plan = parse_plan(&row.plan_json)?;
        // 匹配方向前把 request.selection 写进 plan；后续 push fan-out 的 dest_plan
        // 序列化与对端 prepare/commit 自然携带。
        if request.selection.is_some() {
            plan.selection = request.selection.clone();
        }
        match plan.direction {
            UserMirrorDirection::Pull => {
                let (objects, bindings) = collect_source_objects(state, &plan).await?;
                apply_user_mirror(state, request, &objects, &bindings).await
            }
            UserMirrorDirection::Push => super::push::apply_push_user_mirror(state, request).await,
        }
    }

    /// 按 clientRequestId 对账镜像结果。
    ///
    /// Business Logic: 未完成必须 outcomeUnknown，不得标成功。
    /// Code Logic: 委托 `get_user_mirror`。
    pub async fn get_user_mirror(
        state: &AppState,
        client_request_id: &str,
    ) -> Result<UserMirrorResultDto, AppError> {
        get_user_mirror(state, client_request_id).await
    }
}

/// 读取源端 inventory：本机直扫，对端经 user-mirror inventory 路由。
async fn load_source_inventory(
    state: &AppState,
    device_id: &str,
) -> Result<UserMirrorInventoryDto, AppError> {
    if device_id == state.device_id.as_str() {
        return build_local_user_mirror_inventory(state, device_id).await;
    }
    fetch_peer_user_mirror_inventory(state, device_id).await
}

/// Pull apply 前收集源 CAS 对象（本机 freeze 或对端 selection+objects）。
///
/// Business Logic: selection 只影响冻结打包范围；stale 校验必须仍基于全量探测 inventory。
/// Code Logic: 先全量探测 + stale 校验；本机源分支裁剪 inventory 副本后 freeze，
/// 对端分支把 selection 带进 selection 路由由对端裁剪。
async fn collect_source_objects(
    state: &AppState,
    plan: &UserMirrorPlanDto,
) -> Result<(BTreeMap<String, Vec<u8>>, Vec<UserMirrorObjectBinding>), AppError> {
    let inventory = load_source_inventory(state, &plan.source_device_id).await?;
    if inventory.inventory_snapshot_hash != plan.remote_inventory_snapshot_hash {
        return Err(AppError::conflict(USER_MIRROR_STALE));
    }
    if plan.source_device_id == state.device_id.as_str() {
        // 本机源：freeze 前先收编 Skill/Command 进 portable-store，再用重建后的
        // inventory 冻结；stale 校验仍基于迁移前探测的 inventory，互不影响。
        migrate_portable_assets_into_store(state).await?;
        let migrated_inventory =
            build_local_user_mirror_inventory(state, state.device_id.as_str()).await?;
        let frozen = filter_inventory_for_freeze(&migrated_inventory, plan.selection.as_ref());
        let built = freeze_user_mirror_selection(state, &frozen).await?;
        return Ok((built.object_bytes, built.item_bindings));
    }
    let (base_url, expected) = resolve_online_peer(state, &plan.source_device_id)?;
    let peer = PeerClient::new();
    let health = peer
        .require_capability(&base_url, CAPABILITY_USER_MIRROR_V1)
        .await
        .map_err(map_user_mirror_peer_err)?;
    ensure_health_device_id(&health.device_id, &expected)?;
    let selection: UserMirrorSelectionResponse = post_json_bound(
        &peer,
        &base_url,
        "/api/agent-hub/user-mirror/selection",
        &UserMirrorSelectionQuery {
            inventory,
            selection: plan.selection.clone(),
        },
        &expected,
        PeerTimeoutClass::Metadata.timeout(),
    )
    .await
    .map_err(map_user_mirror_peer_err)?;
    let hashes: Vec<String> = selection
        .envelope
        .objects
        .iter()
        .map(|object| object.hash.clone())
        .collect();
    let objects =
        download_user_mirror_objects(&peer, &base_url, &expected, &selection.transfer_id, &hashes)
            .await?;
    Ok((objects, selection.item_bindings))
}

/// 对端 metadata inventory（无 path / secret）。
async fn fetch_peer_user_mirror_inventory(
    state: &AppState,
    device_id: &str,
) -> Result<UserMirrorInventoryDto, AppError> {
    let (base_url, expected) = resolve_online_peer(state, device_id)?;
    let peer = PeerClient::new();
    let health = peer
        .require_capability(&base_url, CAPABILITY_USER_MIRROR_V1)
        .await
        .map_err(map_user_mirror_peer_err)?;
    ensure_health_device_id(&health.device_id, &expected)?;
    post_json_bound(
        &peer,
        &base_url,
        "/api/agent-hub/user-mirror/inventory",
        &serde_json::json!({}),
        &expected,
        PeerTimeoutClass::long_running(USER_MIRROR_PEER_INVENTORY_TIMEOUT),
    )
    .await
    .map_err(map_user_mirror_peer_err)
}

fn resolve_online_peer(state: &AppState, device_id: &str) -> Result<(String, String), AppError> {
    let devices = state.devices.read().expect("devices lock");
    let device = devices
        .get(device_id)
        .ok_or_else(|| AppError::unavailable(USER_MIRROR_PEER_OFFLINE.to_string()))?;
    if !device.online {
        return Err(AppError::unavailable(USER_MIRROR_PEER_OFFLINE.to_string()));
    }
    Ok((device.base_url(), device.id.clone()))
}

fn ensure_health_device_id(actual: &str, expected: &str) -> Result<(), AppError> {
    if actual.trim() == expected {
        return Ok(());
    }
    Err(AppError::unavailable(USER_MIRROR_PEER_OFFLINE.to_string()))
}

fn map_user_mirror_peer_err(error: PeerCallError) -> AppError {
    match error {
        PeerCallError::Unsupported { .. } => {
            AppError::unavailable(USER_MIRROR_CAPABILITY_UNSUPPORTED.to_string())
        }
        PeerCallError::Network { .. } => {
            AppError::unavailable(USER_MIRROR_PEER_OFFLINE.to_string())
        }
        PeerCallError::Remote { message, .. } => AppError::generic(message),
        PeerCallError::InvalidResponse { .. } => {
            AppError::generic("USER_MIRROR_INVALID_RESPONSE".to_string())
        }
    }
}

async fn download_user_mirror_objects(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    transfer_id: &str,
    hashes: &[String],
) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
    let mut collected = BTreeMap::new();
    let mut total = 0u64;
    for hash in hashes {
        let mut offset = 0u64;
        let mut buf = Vec::new();
        loop {
            let chunk = get_object_chunk(
                peer,
                base_url,
                expected_device_id,
                transfer_id,
                hash,
                offset,
            )
            .await?;
            if chunk.is_empty() {
                break;
            }
            if chunk.len() > 8 * 1024 * 1024 {
                return Err(AppError::validation(USER_MIRROR_TRANSFER_LIMIT.to_string()));
            }
            let next = total
                .saturating_add(buf.len() as u64)
                .saturating_add(chunk.len() as u64);
            if next > USER_MIRROR_DEST_MAX_TOTAL_BYTES {
                return Err(AppError::validation(USER_MIRROR_TRANSFER_LIMIT.to_string()));
            }
            offset = offset.saturating_add(chunk.len() as u64);
            buf.extend_from_slice(&chunk);
        }
        if sha256_hex(&buf) != *hash {
            return Err(AppError::validation(format!(
                "USER_MIRROR_OBJECT_HASH_MISMATCH:{hash}"
            )));
        }
        total = total.saturating_add(buf.len() as u64);
        collected.insert(hash.clone(), buf);
    }
    Ok(collected)
}

async fn get_object_chunk(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let path =
        format!("/api/agent-hub/user-mirror/objects/{transfer_id}/{object_hash}?offset={offset}");
    get_bytes_bound(
        peer,
        base_url,
        &path,
        expected_device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(map_user_mirror_peer_err)
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

async fn get_bytes_bound(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    expected_device_id: &str,
    class: PeerTimeoutClass,
) -> Result<Vec<u8>, PeerCallError> {
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .get(&url)
        .timeout(class.timeout())
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.clone(),
            source: e,
        })?;
    if !resp.status().is_success() {
        return Err(PeerCallError::Remote {
            url,
            status: resp.status().as_u16(),
            code: "user_mirror_object".into(),
            message: format!("object chunk http {}", resp.status()),
            request_id: String::new(),
            retryable: false,
            legacy: false,
            details: Box::new(serde_json::json!({})),
        });
    }
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| PeerCallError::Network { url, source: e })
}

/// 把 preview DTO 插入 dest `agent_hub_user_mirror_plans`。
async fn persist_plan(dest_state: &AppState, plan: &UserMirrorPlanDto) -> Result<(), AppError> {
    dest_state
        .agent_hub_repo
        .insert_user_mirror_plan(UserMirrorPlanRecord {
            plan_token: plan.plan_token.clone(),
            expires_at: plan.expires_at.clone(),
            plan_json: serde_json::to_string(plan)?,
            client_request_id: None,
            claimed_at: None,
            consumed_at: None,
            result_json: None,
            created_at: Utc::now().to_rfc3339(),
        })
        .await?;
    Ok(())
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

fn build_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
    agents: Vec<UserMirrorAgentResultDto>,
) -> UserMirrorResultDto {
    let partial = agents
        .iter()
        .any(|agent| agent.state != UserMirrorItemState::Succeeded);
    UserMirrorResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        destination_device_id: plan.destination_device_id.clone(),
        partial,
        agents,
    }
}

fn failed_stale_result(
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
                state: UserMirrorItemState::Failed,
                error_code: Some(USER_MIRROR_STALE.to_string()),
                message: Some(USER_MIRROR_STALE.to_string()),
            })
            .collect(),
    }
}

fn failed_apply_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &UserMirrorPlanDto,
    error: &AppError,
) -> UserMirrorResultDto {
    let message = error.to_string();
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
                state: UserMirrorItemState::Failed,
                error_code: Some(message.clone()),
                message: Some(message.clone()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_user_mirror_with_env, get_user_mirror, preview_from_two_inventories,
        preview_user_mirror_with_envs, UserMirrorService,
    };
    use crate::agent_hub::targets::TargetEnvironment;
    use crate::agent_hub::user_mirror::inventory::build_local_user_mirror_inventory_with_env;
    use crate::agent_hub::user_mirror::models::{
        ApplyUserMirrorRequest, PreviewUserMirrorRequest, UserMirrorDirection, UserMirrorItemState,
    };
    use crate::agent_hub::user_mirror::selection::freeze_user_mirror_selection_with_env;
    use crate::backend::runtime::build_app_state;
    use crate::backend::ui::RecordingBackendUi;
    use crate::config::{install_data_dir_env, install_env_var};
    use crate::state::AppState;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// Business Logic: control/Tauri 共用 UserMirrorService 三入口。
    /// Code Logic: 结构体 + impl 方法签名存在。
    #[test]
    fn owner_user_mirror_service_exposes_preview_apply_get() {
        let src = include_str!("service.rs");
        let name = "UserMirrorService";
        assert!(src.contains(&format!("pub struct {name};")));
        assert!(src.contains(&format!("impl {name} {{")));
        let _ = std::any::type_name::<UserMirrorService>();
    }

    /// Business Logic: 对端 inventory 是全 Agent 扫描，不能套 10s metadata。
    /// Code Logic: inventory POST 使用 60s long_running；selection 仍走 metadata。
    #[test]
    fn peer_inventory_uses_long_running_budget() {
        use super::USER_MIRROR_PEER_INVENTORY_TIMEOUT;
        use crate::net::peer_timeout::PeerTimeoutClass;
        use std::time::Duration;
        let src = include_str!("service.rs");
        assert_eq!(USER_MIRROR_PEER_INVENTORY_TIMEOUT, Duration::from_secs(60));
        assert!(USER_MIRROR_PEER_INVENTORY_TIMEOUT > PeerTimeoutClass::Metadata.timeout());
        assert!(src.contains("/api/agent-hub/user-mirror/inventory"));
        assert!(src.contains("long_running(USER_MIRROR_PEER_INVENTORY_TIMEOUT)"));
        assert!(src.contains("PeerTimeoutClass::Metadata.timeout()"));
    }

    struct DualEnv {
        _tmp: tempfile::TempDir,
        _guards: Vec<Box<dyn std::any::Any>>,
        source_state: AppState,
        dest_state: AppState,
        source_home: PathBuf,
        dest_home: PathBuf,
        source_env: TargetEnvironment,
        dest_env: TargetEnvironment,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     service 测试必须隔离源/目标 HOME 与 data_dir，避免改写开发者真实配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先构建 source AppState 并释放其 env 锁，再安装 dest HOME/data_dir。
    async fn seed_dual_env() -> DualEnv {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source_home = tmp.path().join("source-home");
        let dest_home = tmp.path().join("dest-home");
        let source_data = tmp.path().join("source-data");
        let dest_data = tmp.path().join("dest-data");
        for path in [&source_home, &dest_home, &source_data, &dest_data] {
            fs::create_dir_all(path).expect("mkdir");
        }
        let source_env = isolated_target_env(&source_home);
        let dest_env = isolated_target_env(&dest_home);

        let source_state = {
            let _data = install_data_dir_env(Some(source_data.to_str().expect("utf8 source data")));
            let _home = install_env_var(
                "HOME",
                Some(source_home.to_str().expect("utf8 source home")),
            );
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("source state")
        };
        let dest_data_guard =
            install_data_dir_env(Some(dest_data.to_str().expect("utf8 dest data")));
        let dest_home_guard =
            install_env_var("HOME", Some(dest_home.to_str().expect("utf8 dest home")));
        let dest_state = {
            let ui = Arc::new(RecordingBackendUi::default());
            build_app_state(ui).await.expect("dest state")
        };
        DualEnv {
            _tmp: tmp,
            _guards: vec![Box::new(dest_data_guard), Box::new(dest_home_guard)],
            source_state,
            dest_state,
            source_home,
            dest_home,
            source_env,
            dest_env,
        }
    }

    fn isolated_target_env(home: &Path) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries: Vec::new(),
        }
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, text).expect("write");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本地 Pull 必须把 preview 写入 dest ledger，apply 写盘后同 request 重放且 get 一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     DualEnv 源 CLAUDE.md=FROM-SRC；preview_user_mirror 落库 → freeze → apply →
    ///     dest 文件覆盖；第二次 apply 与 get 返回同一 result。
    #[tokio::test]
    async fn local_pull_preview_apply_replays_and_get_matches() {
        let env = seed_dual_env().await;
        write(
            env.source_home.join(".claude/CLAUDE.md").as_path(),
            "FROM-SRC",
        );
        write(
            env.dest_home.join(".claude/CLAUDE.md").as_path(),
            "OLD-DEST",
        );
        let plan = preview_user_mirror_with_envs(
            &env.dest_state,
            &env.source_state,
            PreviewUserMirrorRequest {
                direction: UserMirrorDirection::Pull,
                source_device_id: Some("src-dev".into()),
                peer_device_ids: Vec::new(),
            },
            &env.source_env,
            &env.dest_env,
        )
        .await
        .expect("preview");
        let stored = env
            .dest_state
            .agent_hub_repo
            .get_user_mirror_plan(&plan.plan_token)
            .await
            .unwrap()
            .expect("plan row");
        assert!(stored.client_request_id.is_none());
        assert!(stored.result_json.is_none());

        let source_inventory = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .expect("source inventory");
        let built = freeze_user_mirror_selection_with_env(
            &env.source_state,
            &source_inventory,
            &env.source_env,
        )
        .await
        .expect("freeze");
        let request = ApplyUserMirrorRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-local-1".into(),
            selection: None,
        };
        let first = apply_user_mirror_with_env(
            &env.dest_state,
            request.clone(),
            &built.object_bytes,
            &built.item_bindings,
            &env.dest_env,
        )
        .await
        .expect("apply");
        assert!(!first.partial);
        assert_eq!(
            fs::read_to_string(env.dest_home.join(".claude/CLAUDE.md")).unwrap(),
            "FROM-SRC"
        );
        let replay = apply_user_mirror_with_env(
            &env.dest_state,
            request,
            &BTreeMap::new(),
            &[],
            &env.dest_env,
        )
        .await
        .expect("replay");
        assert_eq!(replay.plan_token, first.plan_token);
        assert_eq!(replay.client_request_id, first.client_request_id);
        assert_eq!(replay.partial, first.partial);
        let got = get_user_mirror(&env.dest_state, "req-local-1")
            .await
            .expect("get");
        assert_eq!(got.plan_token, first.plan_token);
        assert_eq!(got.client_request_id, "req-local-1");
        assert_eq!(got.partial, first.partial);
        assert!(got
            .agents
            .iter()
            .all(|agent| agent.state == UserMirrorItemState::Succeeded));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview_from_two_inventories 必须在 dest DB 插入可 claim 的 plan 行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两份空 inventory persist 后按 token 读到行，expires_at 非空。
    #[tokio::test]
    async fn preview_from_two_inventories_inserts_plan_row() {
        let env = seed_dual_env().await;
        let source = build_local_user_mirror_inventory_with_env(
            &env.source_state,
            "src-dev",
            &env.source_env,
        )
        .await
        .unwrap();
        let dest =
            build_local_user_mirror_inventory_with_env(&env.dest_state, "dst-dev", &env.dest_env)
                .await
                .unwrap();
        let plan = preview_from_two_inventories(
            &env.dest_state,
            &source,
            &dest,
            "src-dev",
            "dst-dev",
            UserMirrorDirection::Pull,
        )
        .await
        .unwrap();
        assert!(!plan.plan_token.is_empty());
        let row = env
            .dest_state
            .agent_hub_repo
            .get_user_mirror_plan(&plan.plan_token)
            .await
            .unwrap()
            .expect("inserted");
        assert_eq!(row.plan_token, plan.plan_token);
        assert_eq!(row.expires_at, plan.expires_at);
        assert!(row.client_request_id.is_none());
    }
}
