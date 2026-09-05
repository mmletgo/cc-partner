//! agent_hub/replication/pull — 同类 Agent 远端 portable inventory + 选择性 Pull（目录模块）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户从远端设备加载 metadata inventory，勾选后 preview/apply 到本机同类 Agent；
//!     只允许 sourceTarget == destinationTarget；未映射/未 opt-in 只导入 canonical。
//!     expected-device / clientRequestId 是路由绑定与幂等标签，**不是**身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     目录模块拆分：`dto`（wire DTO / 请求响应）、`staging`（源端选择暂存）、
//!     `materialize`（CAS 树安全物化）、`install_target`（安装目标解析与能力门禁）、
//!     `remote_project`（远端项目代理与账本对账）；本文件承载主入口与源端供数：
//!     远端 inventory 路由（无 secret）；本地 preview plan + apply（CAS 分块 ≤8MiB 续传、
//!     SnapshotImporter 导入、映射项安装）；SQLite clientRequestId claim/replay 与 partial 报告。

mod dto;
mod install_target;
mod materialize;
mod remote_project;
mod staging;

pub use dto::{
    ApplyPortablePullRequest, ApplyRemoteProjectPortableActionRequest,
    GetRemoteProjectPortableActionRequest, InspectRemoteProjectPortableInventoryRequest,
    ListRemotePortableInventoryRequest, PortablePullChangeDto, PortablePullInstallMode,
    PortablePullItemResultDto, PortablePullItemState, PortablePullPlanDto, PortablePullResultDto,
    PreviewPortablePullRequest, PreviewRemoteProjectPortableActionRequest, RemoteInventoryQuery,
    RemotePortableInventoryDto, RemotePortableInventoryItemDto, RemotePortableSelectionResponse,
    RemoteProjectPortableInventoryQuery, RemoteProjectRefRequest, RemoteSelectionQuery,
};
pub use remote_project::{
    apply_remote_project_portable_action, enable_remote_project,
    get_remote_project_portable_action, inspect_remote_project_portable_inventory,
    preview_remote_project, preview_remote_project_portable_action,
};

use crate::agent_hub::assets::{from_canonical_bytes, PortableAssetPayload};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, value_content_hash, JsoncConfigPatcher, ManagedConfigPatch,
};
use crate::agent_hub::models::{AgentTarget, PortablePullClaim, PortablePullPlanRecord};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::portable_actions::PortableAssetConflictPolicy;
use crate::agent_hub::portable_inventory::{
    evaluate_current_portable_target_support, inspect_portable_inventory_force_query,
    inspect_portable_inventory_query, PortableAssetKind, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryQuery, PortableInventorySnapshotDto,
    PortableMcpCredentialFactDto,
};
use crate::agent_hub::replication::receiver::AGENT_HUB_MAX_CHUNK_BYTES;
use crate::agent_hub::snapshot::envelope::compute_snapshot_hash;
use crate::agent_hub::snapshot::importer::{
    ConfirmedImportSelection, SnapshotImporter, ValidatedSnapshot,
};
use crate::agent_hub::snapshot::portable_builder::{bytes_are_legacy_lossy, PortableSelectionItem};
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::PeerCallError;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::{CAPABILITY_PORTABLE_PROJECT_V1, CAPABILITY_PORTABLE_PULL_V1};
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

use install_target::{
    destination_mutation_capability, inventory_has_scoped_item, mutation_allows_install_to_target,
    native_mcp_leaf_value, portable_pull_inventory_query, resolve_claude_mcp_config_path,
    resolve_destination_scope_id, resolve_install_root, resolve_inventory_scope_id,
    resolve_opencode_mcp_config_path,
};
use materialize::materialize_tree_atomic_replace;
use remote_project::{
    build_source_selection_for_items, fold_pending_pull_observations, outcome_unknown_pull_result,
    parse_stored_pull_plan, resolve_remote_portable_project,
};
use staging::{
    evict_expired_staging, staging, staging_insert, staging_remove, StoredPortablePullPlan,
    StoredRemoteItemBinding,
};

/// CAS 分块上限（与 receiver 一致）。
pub const PORTABLE_PULL_MAX_CHUNK_BYTES: usize = AGENT_HUB_MAX_CHUNK_BYTES;

/// Destination object reassembly total budget (matches source selection/staging 64 MiB).
/// LAN peers are unauthenticated; destination must not trust declared sizes or stream unbounded.
pub const PORTABLE_PULL_DEST_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Stable error when destination transfer budget / chunk body limits are exceeded.
pub const PORTABLE_PULL_DEST_TRANSFER_LIMIT: &str = "PORTABLE_PULL_DEST_TRANSFER_LIMIT";

/// preview plan TTL（分钟）。
pub const PULL_PLAN_TTL_MINUTES: i64 = 15;

// ───────────────────────── 源端 inventory ─────────────────────────

/// 构建脱敏远端 inventory（本机作为源被查询）。
pub async fn build_remote_inventory_for_target(
    state: &AppState,
    source_target: AgentTarget,
    source_local_project_id: Option<String>,
) -> Result<RemotePortableInventoryDto, AppError> {
    let snap = inspect_portable_inventory_query(
        state,
        portable_pull_inventory_query(source_target, source_local_project_id),
    )
    .await?;
    Ok(snapshot_to_remote(
        state.device_id.as_str(),
        source_target,
        &snap,
    ))
}

fn snapshot_to_remote(
    device_id: &str,
    source_target: AgentTarget,
    snap: &PortableInventorySnapshotDto,
) -> RemotePortableInventoryDto {
    let items = snap
        .items
        .iter()
        .filter(|i| i.target == source_target)
        .map(to_remote_item)
        .collect();
    RemotePortableInventoryDto {
        source_device_id: device_id.to_string(),
        source_target,
        inventory_snapshot_hash: snap.inventory_snapshot_hash.clone(),
        refreshed_at: snap.refreshed_at.clone(),
        stale: snap.stale,
        items,
    }
}

fn to_remote_item(item: &PortableInventoryItemDto) -> RemotePortableInventoryItemDto {
    RemotePortableInventoryItemDto {
        inventory_item_id: item.inventory_item_id.clone(),
        target: item.target,
        kind: item.kind,
        native_id: item.native_id.clone(),
        display_name: item.display_name.clone(),
        description: item.description.clone(),
        version: item.version.clone(),
        scope_id: item.scope_id.clone(),
        project_id: item.project_id.clone(),
        project_opted_in: item.project_opted_in,
        source_origin: item.source_origin,
        actual_enabled: item.actual_enabled,
        content_hash: item.content_hash.clone(),
        tree_hash: item.tree_hash.clone(),
        warnings: item.warnings.clone(),
        mcp_credential: item
            .mcp_credential
            .as_ref()
            .map(|c| PortableMcpCredentialFactDto {
                present: c.present,
                hash: c.hash.clone(),
            }),
    }
}

/// 校验 remote inventory DTO 不含 secret/path 字段。
pub fn remote_inventory_is_metadata_only(dto: &RemotePortableInventoryDto) -> bool {
    let json = serde_json::to_string(dto).unwrap_or_default();
    let lower = json.to_ascii_lowercase();
    !lower.contains("\"sourcepath\"")
        && !lower.contains("\"env\"")
        && !lower.contains("\"authorization\"")
        && !lower.contains("\"api_token\"")
        && !lower.contains("\"apitoken\"")
}

// ───────────────────────── 源端 selection / objects ─────────────────────────

/// 源端按 item ids 构建 selection 并登记 transfer。
pub async fn source_prepare_selection(
    state: &AppState,
    source_target: AgentTarget,
    source_local_project_id: Option<String>,
    item_ids: Vec<String>,
) -> Result<RemotePortableSelectionResponse, AppError> {
    let built =
        build_source_selection_for_items(state, source_target, source_local_project_id, &item_ids)
            .await?;
    let transfer_id = format!("ppull-src-{}", Uuid::now_v7());
    let missing: Vec<String> = built
        .envelope
        .objects
        .iter()
        .map(|o| o.hash.clone())
        .collect();
    let resp = RemotePortableSelectionResponse {
        transfer_id: transfer_id.clone(),
        envelope: built.envelope.clone(),
        items: built.items.clone(),
        missing_object_hashes: missing,
    };
    // freeze inventory snapshot hash on the envelope source id for apply revalidation
    staging_insert(transfer_id, built)?;
    Ok(resp)
}

/// 源端读取 object chunk（offset 续传）。
pub fn source_read_object_chunk(
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let mut g = staging().lock().expect("staging");
    evict_expired_staging(&mut g);
    let staged = g
        .get_mut(transfer_id)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_TRANSFER_NOT_FOUND".to_string()))?;
    let bytes = staged
        .built
        .object_bytes
        .get(object_hash)
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_OBJECT_NOT_FOUND".to_string()))?;
    if offset as usize >= bytes.len() {
        // 空对象或已越过末尾：标记该 hash 已消费（size==0 的 blob 也算 fully read）
        if bytes.is_empty() {
            staged.fully_read_hashes.insert(object_hash.to_string());
        }
        // 即使最后一个 chunk 已经读完，也不能在返回 HTTP 响应前释放 staging：
        // 客户端可能只收到 response body 前的连接断开，需要按同一 offset 重试。
        // 显式 release 或 TTL GC 才回收 transfer。
        return Ok(Vec::new());
    }
    let end = (offset as usize + PORTABLE_PULL_MAX_CHUNK_BYTES).min(bytes.len());
    let chunk = bytes[offset as usize..end].to_vec();
    // 完整读到本对象末尾 → 记入 fully_read；全部 hash 读完才释放 staging
    if end >= bytes.len() {
        staged.fully_read_hashes.insert(object_hash.to_string());
    }
    drop(g);
    Ok(chunk)
}

/// 源端显式释放 transfer staging（完整传输后或 plan 过期）。
pub fn source_release_transfer(transfer_id: &str) {
    staging_remove(transfer_id);
}

/// 列出远端 inventory（本机作为客户端）。
pub async fn list_remote_portable_inventory(
    state: &AppState,
    request: ListRemotePortableInventoryRequest,
) -> Result<RemotePortableInventoryDto, AppError> {
    let project_context = if let Some(project_ref) = request.source_project_ref.as_deref() {
        Some(resolve_remote_portable_project(state, project_ref).await?)
    } else {
        None
    };
    let (base_url, expected_device_id, source_local_project_id) =
        if let Some(context) = project_context {
            (
                context.base_url,
                context.device_id,
                Some(context.remote_project_id),
            )
        } else {
            let device = resolve_device(state, &request.source_device_id)?;
            (
                device.base_url(),
                request.source_device_id.clone(),
                request.source_local_project_id.clone(),
            )
        };
    let peer = PeerClient::new();
    let required_capability = if source_local_project_id.is_some() {
        CAPABILITY_PORTABLE_PROJECT_V1
    } else {
        CAPABILITY_PORTABLE_PULL_V1
    };
    peer.require_capability(&base_url, required_capability)
        .await
        .map_err(peer_err)?;
    fetch_remote_inventory(
        &peer,
        &base_url,
        &expected_device_id,
        request.source_target,
        source_local_project_id,
    )
    .await
}

/// 预览同类 Agent pull（零写入目标文件）。
pub async fn preview_portable_pull(
    state: &AppState,
    request: PreviewPortablePullRequest,
) -> Result<PortablePullPlanDto, AppError> {
    if request.source_target != request.destination_target {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MISMATCH:source!=destination".to_string(),
        ));
    }
    if request.inventory_item_ids.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_SELECTION_EMPTY".to_string(),
        ));
    }

    let project_context = if let Some(project_ref) = request.source_project_ref.as_deref() {
        Some(resolve_remote_portable_project(state, project_ref).await?)
    } else {
        None
    };
    let (base_url, expected_device_id, source_local_project_id) =
        if let Some(context) = project_context {
            (
                context.base_url,
                context.device_id,
                Some(context.remote_project_id),
            )
        } else {
            let device = resolve_device(state, &request.source_device_id)?;
            (
                device.base_url(),
                request.source_device_id.clone(),
                request.source_local_project_id.clone(),
            )
        };
    let peer = PeerClient::new();
    let required_capability = if source_local_project_id.is_some() {
        CAPABILITY_PORTABLE_PROJECT_V1
    } else {
        CAPABILITY_PORTABLE_PULL_V1
    };
    peer.require_capability(&base_url, required_capability)
        .await
        .map_err(peer_err)?;

    let remote = fetch_remote_inventory(
        &peer,
        &base_url,
        &expected_device_id,
        request.source_target,
        source_local_project_id.clone(),
    )
    .await?;

    if remote.inventory_snapshot_hash != request.remote_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
        ));
    }

    let wanted: BTreeSet<&str> = request
        .inventory_item_ids
        .iter()
        .map(String::as_str)
        .collect();
    let remote_selected: Vec<_> = remote
        .items
        .iter()
        .filter(|i| wanted.contains(i.inventory_item_id.as_str()))
        .cloned()
        .collect();
    if remote_selected.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_ITEMS_NOT_FOUND".to_string(),
        ));
    }
    for item in &remote_selected {
        if item.target != request.source_target {
            return Err(AppError::validation(format!(
                "PORTABLE_PULL_TARGET_MISMATCH:item={}",
                item.inventory_item_id
            )));
        }
    }

    let local_query = portable_pull_inventory_query(
        request.destination_target,
        request.destination_local_project_id.clone(),
    );
    let local = inspect_portable_inventory_query(state, local_query).await?;
    let destination_scope_id =
        resolve_destination_scope_id(state, request.destination_local_project_id.as_deref())
            .await?;
    // Conflict identity includes resolved scope — user + project same nativeId are distinct.
    let local_by_identity: BTreeMap<
        (AgentTarget, PortableAssetKind, String, String),
        &PortableInventoryItemDto,
    > = local
        .items
        .iter()
        .map(|i| {
            (
                (
                    i.target,
                    i.kind,
                    i.native_id.clone(),
                    resolve_inventory_scope_id(i),
                ),
                i,
            )
        })
        .collect();

    let mut changes = Vec::new();
    let mut blocking = Vec::new();
    let mut credential_bearing = 0u64;
    // destination mutation gate：无 L3 写证据时 fail-closed 为 canonical-only
    let dest_mutation = destination_mutation_capability(&local, request.destination_target);
    let destination_support =
        evaluate_current_portable_target_support(request.destination_target).ok();

    for rem in &remote_selected {
        let mutation_install_ok = mutation_allows_install_to_target(
            dest_mutation,
            destination_support.as_ref(),
            rem.kind,
        );
        let mut item_blocking = Vec::new();
        let mut warnings = rem.warnings.clone();
        let rem_scope = destination_scope_id.clone();
        let existing = local_by_identity.get(&(
            rem.target,
            rem.kind,
            rem.native_id.clone(),
            rem_scope.clone(),
        ));
        let unmapped_project = rem.project_id.is_some() && !rem.project_opted_in;
        let local_project_opted = rem.project_id.as_ref().map(|pid| {
            local
                .items
                .iter()
                .any(|i| i.project_id.as_deref() == Some(pid.as_str()) && i.project_opted_in)
        });
        // 显式目标项目已由 resolve_destination_scope_id 校验并重映射；不能再拿来源设备的
        // hubProjectId 去本机查同名 mapping（不同设备的 Hub id 本就可以不同）。
        let mapping_missing = request.destination_local_project_id.is_none()
            && rem.project_id.is_some()
            && local_project_opted != Some(true)
            && rem.scope_id.starts_with("project:");

        let legacy_lossy = warnings
            .iter()
            .any(|w| w.to_ascii_lowercase().contains("legacylossy"));
        let cred = rem
            .mcp_credential
            .as_ref()
            .map(|c| c.present)
            .unwrap_or(false);
        if cred {
            credential_bearing += 1;
        }

        let install_mode = if legacy_lossy {
            item_blocking.push("legacyLossy credential blocked".into());
            PortablePullInstallMode::Blocked
        } else if let Some(local_item) = existing {
            match request.conflict_policy {
                PortableAssetConflictPolicy::SkipExisting => PortablePullInstallMode::SkipExisting,
                PortableAssetConflictPolicy::ReplaceAfterPreview => {
                    if local_item.management_state
                        == PortableInventoryManagementState::ExternalCollision
                    {
                        item_blocking.push("externalCollision".into());
                        PortablePullInstallMode::Blocked
                    } else if mapping_missing || unmapped_project {
                        PortablePullInstallMode::ImportedCanonicalOnly
                    } else if !mutation_install_ok {
                        warnings.push(
                            "PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED:destination mutation not Supported"
                                .into(),
                        );
                        PortablePullInstallMode::ImportedCanonicalOnly
                    } else {
                        PortablePullInstallMode::InstallToTarget
                    }
                }
            }
        } else if mapping_missing || unmapped_project {
            warnings.push("project unmapped or not opted-in; canonical only".into());
            PortablePullInstallMode::ImportedCanonicalOnly
        } else if !mutation_install_ok {
            warnings.push(
                "PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED:destination mutation not Supported"
                    .into(),
            );
            PortablePullInstallMode::ImportedCanonicalOnly
        } else {
            PortablePullInstallMode::InstallToTarget
        };

        if !item_blocking.is_empty() {
            blocking.extend(item_blocking.iter().cloned());
        }

        changes.push(PortablePullChangeDto {
            inventory_item_id: rem.inventory_item_id.clone(),
            kind: rem.kind,
            native_id: rem.native_id.clone(),
            display_name: rem.display_name.clone(),
            scope_id: rem_scope,
            install_mode,
            conflict: existing.is_some(),
            legacy_lossy,
            credential_bearing: cred,
            blocking_reasons: item_blocking,
            warnings,
        });
    }

    let selection_manifest_hash = sha256_hex(
        serde_json::to_string(&request.inventory_item_ids)
            .unwrap_or_default()
            .as_bytes(),
    );
    let plan_token = format!("ppull-{}", Uuid::now_v7());
    let expires_at = (Utc::now() + ChronoDuration::minutes(PULL_PLAN_TTL_MINUTES)).to_rfc3339();
    let remote_item_ids: Vec<String> = remote_selected
        .iter()
        .map(|r| r.inventory_item_id.clone())
        .collect();
    let remote_item_bindings: Vec<StoredRemoteItemBinding> = remote_selected
        .iter()
        .map(|r| StoredRemoteItemBinding {
            inventory_item_id: r.inventory_item_id.clone(),
            content_hash: r.content_hash.clone(),
            tree_hash: r.tree_hash.clone(),
        })
        .collect();

    let public = PortablePullPlanDto {
        plan_token: plan_token.clone(),
        expires_at: expires_at.clone(),
        source_device_id: expected_device_id,
        source_target: request.source_target,
        destination_target: request.destination_target,
        source_local_project_id,
        source_project_ref: request.source_project_ref.clone(),
        destination_local_project_id: request.destination_local_project_id.clone(),
        remote_inventory_snapshot_hash: remote.inventory_snapshot_hash.clone(),
        local_inventory_snapshot_hash: local.inventory_snapshot_hash.clone(),
        conflict_policy: request.conflict_policy,
        selection_manifest_hash,
        credential_bearing_count: credential_bearing,
        has_credential_bearing_assets: credential_bearing > 0,
        changes,
        blocking_reasons: blocking,
    };

    let stored = StoredPortablePullPlan {
        public: public.clone(),
        remote_item_ids,
        remote_item_bindings,
    };
    let plan_json = serde_json::to_string(&stored)?;
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    repo.insert_portable_pull_plan(PortablePullPlanRecord {
        plan_token,
        expires_at,
        remote_inventory_snapshot_hash: remote.inventory_snapshot_hash,
        local_inventory_snapshot_hash: local.inventory_snapshot_hash,
        plan_json,
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: Utc::now().to_rfc3339(),
    })
    .await?;
    Ok(public)
}

/// 执行 pull（SQLite claim → transfer → import → install → rescan → complete）。
pub async fn apply_portable_pull(
    state: &AppState,
    request: ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_APPLY_IDS_REQUIRED".to_string(),
        ));
    }
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    let claim = repo
        .claim_portable_pull_plan(&request.plan_token, &request.client_request_id)
        .await?;

    match claim {
        PortablePullClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
        PortablePullClaim::Pending => {
            // 未完成 claim：诚实 OutcomeUnknown，best-effort 本地 inventory rescan 附加观察
            let row = repo
                .get_portable_pull_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::not_found("PORTABLE_PULL_PLAN_NOT_FOUND".to_string()))?;
            let stored = parse_stored_pull_plan(&row.plan_json)?;
            let mut result = outcome_unknown_pull_result(
                &request.plan_token,
                &request.client_request_id,
                &stored.public,
            );
            let query = portable_pull_inventory_query(
                stored.public.destination_target,
                stored.public.destination_local_project_id.clone(),
            );
            if let Ok(post) = inspect_portable_inventory_force_query(state, query).await {
                fold_pending_pull_observations(&mut result, &stored.public, &post);
            }
            Ok(result)
        }
        PortablePullClaim::Claimed(record) => {
            let stored = parse_stored_pull_plan(&record.plan_json)?;
            let result = match execute_claimed_pull(state, &stored, &request).await {
                Ok(r) => r,
                Err(e) => {
                    // fail-closed 完成 ledger，避免永远 Pending 假死；调用方可 get 到失败结果。
                    // complete 失败必须上抛（不得吞掉），否则行卡 Pending → 重试 OutcomeUnknown。
                    let fail = PortablePullResultDto {
                        plan_token: request.plan_token.clone(),
                        client_request_id: request.client_request_id.clone(),
                        source_device_id: stored.public.source_device_id.clone(),
                        source_target: stored.public.source_target,
                        destination_target: stored.public.destination_target,
                        partial: true,
                        items: stored
                            .public
                            .changes
                            .iter()
                            .map(|c| PortablePullItemResultDto {
                                inventory_item_id: c.inventory_item_id.clone(),
                                state: PortablePullItemState::Failed,
                                install_mode: Some(c.install_mode),
                                error_code: Some("PORTABLE_PULL_APPLY_FAILED".into()),
                                message: Some(e.to_string()),
                            })
                            .collect(),
                    };
                    repo.complete_portable_pull_plan(
                        &request.plan_token,
                        &request.client_request_id,
                        &serde_json::to_string(&fail)?,
                    )
                    .await?;
                    return Ok(fail);
                }
            };
            repo.complete_portable_pull_plan(
                &request.plan_token,
                &request.client_request_id,
                &serde_json::to_string(&result)?,
            )
            .await?;
            Ok(result)
        }
    }
}

async fn execute_claimed_pull(
    state: &AppState,
    stored: &StoredPortablePullPlan,
    request: &ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    // transfer 前强制同源 target + 本机 inventory 再验证
    if stored.public.source_target != stored.public.destination_target {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MISMATCH:source!=destination".to_string(),
        ));
    }
    if stored.public.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
        return Err(AppError::conflict("PORTABLE_PULL_PLAN_EXPIRED".to_string()));
    }
    let local_query = portable_pull_inventory_query(
        stored.public.destination_target,
        stored.public.destination_local_project_id.clone(),
    );
    let local_now = inspect_portable_inventory_force_query(state, local_query.clone()).await?;
    if local_now.inventory_snapshot_hash != stored.public.local_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_LOCAL_INVENTORY_STALE".to_string(),
        ));
    }

    let project_context = if let Some(project_ref) = stored.public.source_project_ref.as_deref() {
        Some(resolve_remote_portable_project(state, project_ref).await?)
    } else {
        None
    };
    let (base_url, expected_device_id, source_local_project_id) =
        if let Some(context) = project_context {
            if context.device_id != stored.public.source_device_id {
                return Err(AppError::conflict(
                    "PORTABLE_PULL_SOURCE_PROJECT_OWNER_CHANGED",
                ));
            }
            (
                context.base_url,
                context.device_id,
                Some(context.remote_project_id),
            )
        } else {
            let device = resolve_device(state, &stored.public.source_device_id)?;
            (
                device.base_url(),
                stored.public.source_device_id.clone(),
                stored.public.source_local_project_id.clone(),
            )
        };
    let peer = PeerClient::new();
    let required_capability = if source_local_project_id.is_some() {
        CAPABILITY_PORTABLE_PROJECT_V1
    } else {
        CAPABILITY_PORTABLE_PULL_V1
    };
    peer.require_capability(&base_url, required_capability)
        .await
        .map_err(peer_err)?;

    // Apply 前重新校验远端 inventory 快照，防止 preview 后远端 MCP/资产漂移仍安装
    let remote_now = fetch_remote_inventory(
        &peer,
        &base_url,
        &expected_device_id,
        stored.public.source_target,
        source_local_project_id.clone(),
    )
    .await?;
    if remote_now.inventory_snapshot_hash != stored.public.remote_inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
        ));
    }
    // selection 必须仍能覆盖 plan 中的 item ids，且 content/tree hash 与 preview 绑定一致
    let remote_by_id: BTreeMap<&str, &RemotePortableInventoryItemDto> = remote_now
        .items
        .iter()
        .map(|i| (i.inventory_item_id.as_str(), i))
        .collect();
    for id in &stored.remote_item_ids {
        if !remote_by_id.contains_key(id.as_str()) {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
            ));
        }
    }
    // 优先使用 plan 冻结的 binding；旧 plan 无 binding 字段时从当前 remote 再冻结一次
    let bindings: Vec<StoredRemoteItemBinding> = if stored.remote_item_bindings.is_empty() {
        stored
            .remote_item_ids
            .iter()
            .filter_map(|id| {
                remote_by_id
                    .get(id.as_str())
                    .map(|rem| StoredRemoteItemBinding {
                        inventory_item_id: rem.inventory_item_id.clone(),
                        content_hash: rem.content_hash.clone(),
                        tree_hash: rem.tree_hash.clone(),
                    })
            })
            .collect()
    } else {
        // revalidated inventory 必须仍匹配 plan 绑定
        for b in &stored.remote_item_bindings {
            let Some(rem) = remote_by_id.get(b.inventory_item_id.as_str()) else {
                return Err(AppError::conflict(
                    "PORTABLE_PULL_REMOTE_INVENTORY_STALE".to_string(),
                ));
            };
            if rem.content_hash != b.content_hash || rem.tree_hash != b.tree_hash {
                return Err(AppError::conflict(
                    "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
                ));
            }
        }
        stored.remote_item_bindings.clone()
    };

    let selection = fetch_remote_selection(
        &peer,
        &base_url,
        &expected_device_id,
        stored.public.source_target,
        source_local_project_id,
        &stored.remote_item_ids,
    )
    .await?;

    // selection 必须与 revalidated inventory / plan binding 对齐（同 id 下 content 不得漂移）
    // + selection item.target 必须等于 plan.destination_target（防同 id 伪 target 侧写）
    bind_selection_to_inventory_bindings(&selection, &bindings, stored.public.destination_target)?;

    let data_dir = crate::config::data_dir()?;
    let store = ObjectStore::open(&data_dir)?;
    let mut collected: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut collected_total: u64 = 0;

    for obj in &selection.envelope.objects {
        // CAS 已有 blob 时仍 rehash 校验（integrity）
        if let Ok(existing) = store.get_blob(&obj.hash).await {
            let got = sha256_hex(&existing);
            if got != obj.hash {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_OBJECT_HASH_MISMATCH:{}",
                    obj.hash
                )));
            }
            ensure_dest_transfer_budget(collected_total, existing.len() as u64)?;
            collected_total = collected_total.saturating_add(existing.len() as u64);
            collected.insert(obj.hash.clone(), existing);
            continue;
        }
        // Cap declared size before any allocate/fetch (attacker-controlled size string).
        let size = parse_declared_object_size(&obj.size)?;
        // size==0 still needs per-chunk growth checks below; pre-check only reserves size>0.
        if size > 0 {
            ensure_dest_transfer_budget(collected_total, size)?;
        }
        let mut offset = 0u64;
        let mut buf = if size > 0 {
            Vec::with_capacity(size as usize)
        } else {
            Vec::new()
        };
        // size==0: allow at most one full-sized chunk, then require terminal small/empty.
        let mut size_zero_full_chunks: u32 = 0;
        loop {
            let chunk = fetch_object_chunk(
                &peer,
                &base_url,
                &expected_device_id,
                &selection.transfer_id,
                &obj.hash,
                offset,
            )
            .await?;
            if chunk.is_empty() {
                // size>0 但提前 empty → fail-closed
                if size > 0 && offset < size {
                    return Err(AppError::validation(format!(
                        "PORTABLE_PULL_OBJECT_INCOMPLETE:{}",
                        obj.hash
                    )));
                }
                break;
            }
            ensure_chunk_body_within_limit(chunk.len())?;
            let n = chunk.len() as u64;
            // Cumulative budget before growing buf (covers size==0 multi-chunk growth).
            ensure_dest_transfer_budget(collected_total.saturating_add(buf.len() as u64), n)?;
            if size > 0 && offset.saturating_add(n) > size {
                return Err(AppError::validation(
                    PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
                ));
            }
            buf.extend_from_slice(&chunk);
            offset += n;
            if size > 0 && offset >= size {
                break;
            }
            // size==0: fail-closed — one terminal small/empty ends; second full chunk rejects.
            if size == 0 {
                match size_zero_chunk_action(size_zero_full_chunks, n)? {
                    SizeZeroChunkAction::Break => break,
                    SizeZeroChunkAction::ContinueAfterFull => {
                        size_zero_full_chunks = size_zero_full_chunks.saturating_add(1);
                    }
                }
            }
        }
        if bytes_are_legacy_lossy(&buf) {
            continue;
        }
        if !buf.is_empty() || size == 0 {
            let got = sha256_hex(&buf);
            if size > 0 && got != obj.hash {
                return Err(AppError::validation(format!(
                    "PORTABLE_PULL_OBJECT_HASH_MISMATCH:{}",
                    obj.hash
                )));
            }
            if !buf.is_empty() {
                ensure_dest_transfer_budget(collected_total, buf.len() as u64)?;
                store.put_blob(&buf).await?;
                collected_total = collected_total.saturating_add(buf.len() as u64);
                collected.insert(obj.hash.clone(), buf);
            }
        }
    }

    for obj in &selection.envelope.objects {
        if !collected.contains_key(&obj.hash) {
            if let Ok(b) = store.get_blob(&obj.hash).await {
                ensure_dest_transfer_budget(collected_total, b.len() as u64)?;
                collected_total = collected_total.saturating_add(b.len() as u64);
                collected.insert(obj.hash.clone(), b);
            }
        }
    }

    // 传输完成后 best-effort 释放源端 multi-object staging（源侧 all-read 也会清；双保险）
    let _ = release_remote_transfer(
        &peer,
        &base_url,
        &expected_device_id,
        &selection.transfer_id,
    )
    .await;

    let destination_scope_id = stored
        .public
        .changes
        .first()
        .map(|change| change.scope_id.as_str())
        .unwrap_or("user");
    let import_ok =
        import_selection_canonical(state, &store, &data_dir, &selection, destination_scope_id)
            .await;
    let local_before = local_now;
    // apply 路径 re-check mutation：不得仅信 preview 计划（inventory 可能已变）
    let dest_mutation_now =
        destination_mutation_capability(&local_before, stored.public.destination_target);
    let destination_support_now =
        evaluate_current_portable_target_support(stored.public.destination_target).ok();

    let mut items = Vec::new();
    let mut any_fail = false;
    for change in &stored.public.changes {
        let mutation_install_ok_now = mutation_allows_install_to_target(
            dest_mutation_now,
            destination_support_now.as_ref(),
            change.kind,
        );
        if change.legacy_lossy || change.install_mode == PortablePullInstallMode::Blocked {
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Blocked,
                install_mode: Some(change.install_mode),
                error_code: Some("PORTABLE_PULL_ITEM_BLOCKED".into()),
                message: change.blocking_reasons.first().cloned(),
            });
            continue;
        }
        if change.install_mode == PortablePullInstallMode::SkipExisting {
            // 写时再次确认本地仍存在（含 scope），避免 preview 后被删仍 skip /
            // 或被另一 scope 同 nativeId 假命中。
            let still = inventory_has_scoped_item(
                &local_before,
                stored.public.destination_target,
                change.kind,
                &change.native_id,
                &change.scope_id,
            );
            if still {
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Skipped,
                    install_mode: Some(change.install_mode),
                    error_code: None,
                    message: Some("skipExisting".into()),
                });
            } else if !mutation_install_ok_now {
                // 降级 install 也必须过 mutation gate
                if import_ok.is_err() {
                    any_fail = true;
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Failed,
                        install_mode: Some(PortablePullInstallMode::ImportedCanonicalOnly),
                        error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                        message: Some(
                            "canonical import failed before skipExisting demotion canonical-only"
                                .into(),
                        ),
                    });
                } else {
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::ImportedCanonicalOnly,
                        install_mode: Some(PortablePullInstallMode::ImportedCanonicalOnly),
                        error_code: Some("PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED".into()),
                        message: Some(
                            "skipExisting demotion blocked by destination mutation capability"
                                .into(),
                        ),
                    });
                }
            } else {
                // 已不存在：降级为 installToTarget 语义。
                // 与主 InstallToTarget 一致：import 必成才写目标；成功后 rescan 门禁。
                if import_ok.is_err() {
                    any_fail = true;
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Failed,
                        install_mode: Some(PortablePullInstallMode::InstallToTarget),
                        error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                        message: Some(format!(
                            "canonical import failed before skipExisting demotion install: {}",
                            import_ok
                                .as_ref()
                                .err()
                                .map(|e| e.to_string())
                                .unwrap_or_default()
                        )),
                    });
                    continue;
                }
                match install_change(state, &store, &selection, change, &local_query).await {
                    Ok(()) => {
                        let post =
                            inspect_portable_inventory_force_query(state, local_query.clone())
                                .await?;
                        let observed = inventory_has_scoped_item(
                            &post,
                            stored.public.destination_target,
                            change.kind,
                            &change.native_id,
                            &change.scope_id,
                        );
                        if observed {
                            items.push(PortablePullItemResultDto {
                                inventory_item_id: change.inventory_item_id.clone(),
                                state: PortablePullItemState::Succeeded,
                                install_mode: Some(PortablePullInstallMode::InstallToTarget),
                                error_code: None,
                                message: Some(
                                    "skipExisting demotion install verified by rescan".into(),
                                ),
                            });
                        } else {
                            any_fail = true;
                            items.push(PortablePullItemResultDto {
                                inventory_item_id: change.inventory_item_id.clone(),
                                state: PortablePullItemState::Failed,
                                install_mode: Some(PortablePullInstallMode::InstallToTarget),
                                error_code: Some("PORTABLE_PULL_RESCAN_MISSING".into()),
                                message: Some(
                                    "skipExisting demotion install not observed after rescan"
                                        .into(),
                                ),
                            });
                        }
                    }
                    Err(e) => {
                        any_fail = true;
                        items.push(PortablePullItemResultDto {
                            inventory_item_id: change.inventory_item_id.clone(),
                            state: PortablePullItemState::Failed,
                            install_mode: Some(change.install_mode),
                            error_code: Some("PORTABLE_PULL_INSTALL_FAILED".into()),
                            message: Some(e.to_string()),
                        });
                    }
                }
            }
            continue;
        }
        // plan 标 InstallToTarget 但 apply 时 mutation 不再 Supported → 降级 canonical-only
        let effective_mode = if change.install_mode == PortablePullInstallMode::InstallToTarget
            && !mutation_install_ok_now
        {
            PortablePullInstallMode::ImportedCanonicalOnly
        } else {
            change.install_mode
        };
        if effective_mode == PortablePullInstallMode::ImportedCanonicalOnly {
            if import_ok.is_err() {
                any_fail = true;
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Failed,
                    install_mode: Some(PortablePullInstallMode::ImportedCanonicalOnly),
                    error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                    message: Some("canonical import failed".into()),
                });
            } else {
                let msg = if change.install_mode == PortablePullInstallMode::InstallToTarget
                    && !mutation_install_ok_now
                {
                    "canonical only; destination mutation not Supported".into()
                } else {
                    "canonical only; project unmapped or not opted-in".into()
                };
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::ImportedCanonicalOnly,
                    install_mode: Some(PortablePullInstallMode::ImportedCanonicalOnly),
                    error_code: if change.install_mode == PortablePullInstallMode::InstallToTarget
                        && !mutation_install_ok_now
                    {
                        Some("PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED".into())
                    } else {
                        None
                    },
                    message: Some(msg),
                });
            }
            continue;
        }

        // InstallToTarget：canonical import 必须成功，否则不得标 Succeeded
        if import_ok.is_err() {
            any_fail = true;
            items.push(PortablePullItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortablePullItemState::Failed,
                install_mode: Some(change.install_mode),
                error_code: Some("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED".into()),
                message: Some(format!(
                    "canonical import failed: {}",
                    import_ok
                        .as_ref()
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                )),
            });
            continue;
        }

        match install_change(state, &store, &selection, change, &local_query).await {
            Ok(()) => {
                // post-install rescan gate — same scope identity as conflict map
                let post =
                    inspect_portable_inventory_force_query(state, local_query.clone()).await?;
                let observed = inventory_has_scoped_item(
                    &post,
                    stored.public.destination_target,
                    change.kind,
                    &change.native_id,
                    &change.scope_id,
                );
                if observed {
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Succeeded,
                        install_mode: Some(change.install_mode),
                        error_code: None,
                        message: Some("install verified by rescan".into()),
                    });
                } else {
                    any_fail = true;
                    items.push(PortablePullItemResultDto {
                        inventory_item_id: change.inventory_item_id.clone(),
                        state: PortablePullItemState::Failed,
                        install_mode: Some(change.install_mode),
                        error_code: Some("PORTABLE_PULL_RESCAN_MISSING".into()),
                        message: Some("install not observed after rescan".into()),
                    });
                }
            }
            Err(e) => {
                any_fail = true;
                items.push(PortablePullItemResultDto {
                    inventory_item_id: change.inventory_item_id.clone(),
                    state: PortablePullItemState::Failed,
                    install_mode: Some(change.install_mode),
                    error_code: Some("PORTABLE_PULL_INSTALL_FAILED".into()),
                    message: Some(e.to_string()),
                });
            }
        }
    }

    Ok(PortablePullResultDto {
        plan_token: request.plan_token.clone(),
        client_request_id: request.client_request_id.clone(),
        source_device_id: stored.public.source_device_id.clone(),
        source_target: stored.public.source_target,
        destination_target: stored.public.destination_target,
        partial: any_fail || import_ok.is_err(),
        items,
    })
}

async fn install_change(
    state: &AppState,
    store: &ObjectStore,
    selection: &RemotePortableSelectionResponse,
    change: &PortablePullChangeDto,
    local_query: &PortableInventoryQuery,
) -> Result<(), AppError> {
    // 写盘前强制再验 mutation：preview 之后 support 可能回落 Blocked/PreviewOnly。
    // 这里不能复用带缓存的 inventory，避免 scan-only manifest 在缓存窗口内被旧
    // capability 误放行；direct-local allowlist 也只能由 scanner 的 manifest gate 产生。
    let live = inspect_portable_inventory_force_query(state, local_query.clone()).await?;
    let selected_item = selection
        .items
        .iter()
        .find(|s| s.inventory_item_id == change.inventory_item_id)
        .ok_or_else(|| AppError::generic("selection item missing after transfer"))?;
    let aggregate = destination_mutation_capability(&live, selected_item.target);
    let evaluated = evaluate_current_portable_target_support(selected_item.target).ok();
    if !mutation_allows_install_to_target(aggregate, evaluated.as_ref(), selected_item.kind) {
        return Err(AppError::validation(
            "PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED".to_string(),
        ));
    }
    install_payload_to_target(state, store, selected_item, change).await
}

/// selection 响应必须绑定 revalidated inventory 的 content/tree hash（同 id 内容漂移 → fail），
/// 且每条 item.target 必须等于 plan destination_target（同 id 伪 target → mismatch）。
fn bind_selection_to_inventory_bindings(
    selection: &RemotePortableSelectionResponse,
    bindings: &[StoredRemoteItemBinding],
    destination_target: AgentTarget,
) -> Result<(), AppError> {
    let by_id: BTreeMap<&str, &StoredRemoteItemBinding> = bindings
        .iter()
        .map(|b| (b.inventory_item_id.as_str(), b))
        .collect();
    if selection.items.is_empty() {
        return Err(AppError::conflict(
            "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
        ));
    }
    for item in &selection.items {
        if item.target != destination_target {
            return Err(AppError::validation(format!(
                "PORTABLE_PULL_TARGET_MISMATCH:item={}",
                item.inventory_item_id
            )));
        }
        let Some(bound) = by_id.get(item.inventory_item_id.as_str()) else {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
            ));
        };
        // 优先比对 selection 自带的冻结 hash；缺字段时至少保证 binding 有事实
        if let Some(sel_ch) = item.content_hash.as_ref() {
            if bound.content_hash.as_ref() != Some(sel_ch) {
                return Err(AppError::conflict(
                    "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
                ));
            }
        } else if bound.content_hash.is_some() {
            // selection 未回传 content_hash 但 inventory 有 → 无法证明同源，fail-closed
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
            ));
        }
        if let Some(sel_th) = item.tree_hash.as_ref() {
            if bound.tree_hash.as_ref() != Some(sel_th) {
                return Err(AppError::conflict(
                    "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
                ));
            }
        } else if bound.tree_hash.is_some() {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
            ));
        }
    }
    // plan 中每一 binding 都必须出现在 selection
    for b in bindings {
        if !selection
            .items
            .iter()
            .any(|s| s.inventory_item_id == b.inventory_item_id)
        {
            return Err(AppError::conflict(
                "PORTABLE_PULL_REMOTE_SELECTION_DRIFT".to_string(),
            ));
        }
    }
    Ok(())
}

/// best-effort 通知源端释放 transfer staging（路由可选；失败不阻断 apply）。
async fn release_remote_transfer(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    transfer_id: &str,
) -> Result<(), AppError> {
    let path = format!("/api/agent-hub/portable/transfers/{transfer_id}/release");
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .post(&url)
        .timeout(PeerTimeoutClass::Metadata.timeout())
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .send()
        .await;
    // 旧 peer 无 release 路由 → 忽略（源侧 all-read 追踪仍会释放）
    match resp {
        Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => Ok(()),
        Ok(_) | Err(_) => Ok(()),
    }
}

/// 按 clientRequestId 查询 pull 结果。
pub async fn get_portable_pull(
    state: &AppState,
    client_request_id: &str,
) -> Result<PortablePullResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_REQUEST_ID_REQUIRED".to_string(),
        ));
    }
    let repo = AgentHubRepo::new(state.agent_hub_repo.pool().clone());
    let row = repo
        .get_portable_pull_by_request_id(client_request_id)
        .await?
        .ok_or_else(|| AppError::not_found("PORTABLE_PULL_REQUEST_NOT_FOUND".to_string()))?;
    if let Some(result_json) = row.result_json.as_deref() {
        return serde_json::from_str(result_json).map_err(AppError::from);
    }
    // incomplete ledger row：OutcomeUnknown + best-effort destination observation rescan
    let stored = parse_stored_pull_plan(&row.plan_json)?;
    let mut result =
        outcome_unknown_pull_result(&row.plan_token, client_request_id, &stored.public);
    let local_query = portable_pull_inventory_query(
        stored.public.destination_target,
        stored.public.destination_local_project_id.clone(),
    );
    if let Ok(post) = inspect_portable_inventory_force_query(state, local_query).await {
        fold_pending_pull_observations(&mut result, &stored.public, &post);
    }
    Ok(result)
}

// ───────────────────────── helpers ─────────────────────────

/// Parse attacker-controlled declared object size; reject non-numeric / overflow strings.
fn parse_declared_object_size(raw: &str) -> Result<u64, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| AppError::validation(PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string()))
}

/// Fail-closed when declared size or cumulative collected would exceed dest budget.
fn ensure_dest_transfer_budget(current: u64, next: u64) -> Result<(), AppError> {
    if current.saturating_add(next) > PORTABLE_PULL_DEST_MAX_TOTAL_BYTES {
        return Err(AppError::validation(
            PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
        ));
    }
    Ok(())
}

/// Reject a single chunk response body larger than the wire chunk contract.
fn ensure_chunk_body_within_limit(len: usize) -> Result<(), AppError> {
    if len > PORTABLE_PULL_MAX_CHUNK_BYTES {
        return Err(AppError::validation(
            PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
        ));
    }
    Ok(())
}

/// size==0 multi-chunk control: one full-sized chunk may continue; second full rejects;
/// any sub-max (terminal small) ends assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeZeroChunkAction {
    Break,
    ContinueAfterFull,
}

fn size_zero_chunk_action(
    full_chunks_seen: u32,
    chunk_len: u64,
) -> Result<SizeZeroChunkAction, AppError> {
    let max = PORTABLE_PULL_MAX_CHUNK_BYTES as u64;
    if chunk_len < max {
        return Ok(SizeZeroChunkAction::Break);
    }
    // full-sized chunk
    if full_chunks_seen >= 1 {
        return Err(AppError::validation(
            PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
        ));
    }
    Ok(SizeZeroChunkAction::ContinueAfterFull)
}

fn resolve_device(state: &AppState, device_id: &str) -> Result<Device, AppError> {
    let devices = state.devices.read().expect("devices lock");
    devices
        .get(device_id)
        .cloned()
        .ok_or_else(|| AppError::not_found("设备不存在或已离线".to_string()))
}

fn peer_err(e: PeerCallError) -> AppError {
    match e {
        PeerCallError::Unsupported { .. } => {
            AppError::unavailable("PORTABLE_PULL_UNSUPPORTED_PEER".to_string())
        }
        PeerCallError::Network { .. } => {
            AppError::unavailable("PORTABLE_PULL_PEER_NETWORK".to_string())
        }
        PeerCallError::Remote { message, .. } => {
            // Preserve destination transfer limit as Validation + stable code.
            if message.contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT)
                || message.contains("PORTABLE_PULL_STAGING_LIMIT")
            {
                AppError::validation(PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string())
            } else {
                AppError::generic(message)
            }
        }
        PeerCallError::InvalidResponse { .. } => {
            AppError::generic("PORTABLE_PULL_INVALID_RESPONSE".to_string())
        }
    }
}

async fn fetch_remote_inventory(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    source_target: AgentTarget,
    source_local_project_id: Option<String>,
) -> Result<RemotePortableInventoryDto, AppError> {
    post_json_bound(
        peer,
        base_url,
        "/api/agent-hub/portable/inventory",
        &RemoteInventoryQuery {
            source_target,
            source_local_project_id,
        },
        expected_device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

async fn fetch_remote_selection(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    source_target: AgentTarget,
    source_local_project_id: Option<String>,
    item_ids: &[String],
) -> Result<RemotePortableSelectionResponse, AppError> {
    post_json_bound(
        peer,
        base_url,
        "/api/agent-hub/portable/selection",
        &RemoteSelectionQuery {
            source_target,
            source_local_project_id,
            inventory_item_ids: item_ids.to_vec(),
        },
        expected_device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

async fn fetch_object_chunk(
    peer: &PeerClient,
    base_url: &str,
    expected_device_id: &str,
    transfer_id: &str,
    object_hash: &str,
    offset: u64,
) -> Result<Vec<u8>, AppError> {
    let path =
        format!("/api/agent-hub/portable/objects/{transfer_id}/{object_hash}?offset={offset}");
    get_bytes_bound(
        peer,
        base_url,
        &path,
        expected_device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(peer_err)
}

async fn post_json_bound<T, B>(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    body: &B,
    expected_device_id: &str,
    class: PeerTimeoutClass,
) -> Result<T, PeerCallError>
where
    T: for<'de> Deserialize<'de>,
    B: Serialize + ?Sized,
{
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .post(&url)
        .timeout(class.timeout())
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
            code: "portable_pull_object".into(),
            message: format!("object chunk http {}", resp.status()),
            request_id: String::new(),
            retryable: false,
            legacy: false,
            details: Box::new(serde_json::json!({})),
        });
    }
    // Reject oversize Content-Length before buffering; stream-read with hard cap.
    if let Some(len) = resp.content_length() {
        if len > PORTABLE_PULL_MAX_CHUNK_BYTES as u64 {
            return Err(PeerCallError::Remote {
                url,
                status: 413,
                code: "portable_pull_object".into(),
                message: PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
                request_id: String::new(),
                retryable: false,
                legacy: false,
                details: Box::new(serde_json::json!({
                    "reason": "chunk_body_oversize",
                    "contentLength": len,
                    "max": PORTABLE_PULL_MAX_CHUNK_BYTES,
                })),
            });
        }
    }
    read_response_body_capped(resp, PORTABLE_PULL_MAX_CHUNK_BYTES, &url).await
}

/// Stream-read response body with a hard max; fail if peer exceeds it (chunked without length).
async fn read_response_body_capped(
    resp: reqwest::Response,
    max_bytes: usize,
    url: &str,
) -> Result<Vec<u8>, PeerCallError> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| PeerCallError::Network {
            url: url.to_string(),
            source: e,
        })?;
        if out.len().saturating_add(chunk.len()) > max_bytes {
            return Err(PeerCallError::Remote {
                url: url.to_string(),
                status: 413,
                code: "portable_pull_object".into(),
                message: PORTABLE_PULL_DEST_TRANSFER_LIMIT.to_string(),
                request_id: String::new(),
                retryable: false,
                legacy: false,
                details: Box::new(serde_json::json!({
                    "reason": "chunk_body_oversize",
                    "max": max_bytes,
                })),
            });
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

async fn import_selection_canonical(
    state: &AppState,
    store: &ObjectStore,
    data_dir: &std::path::Path,
    selection: &RemotePortableSelectionResponse,
    destination_scope_id: &str,
) -> Result<(), AppError> {
    if selection.envelope.assets.is_empty() {
        return Ok(());
    }
    let mut object_bytes = BTreeMap::new();
    for obj in &selection.envelope.objects {
        if let Ok(b) = store.get_blob(&obj.hash).await {
            object_bytes.insert(obj.hash.clone(), b);
        }
    }
    let mut envelope = selection.envelope.clone();
    // 项目级 Pull 是复制到目标 scope，不是把来源设备的 Hub project id 原样导入本机。
    envelope.selection.scope_ids = vec![destination_scope_id.to_string()];
    for asset in &mut envelope.assets {
        asset.scope_id = destination_scope_id.to_string();
    }
    envelope.snapshot_hash = compute_snapshot_hash(&envelope)
        .map_err(|error| AppError::validation(format!("PORTABLE_PULL_SCOPE_REMAP:{error}")))?;
    let validated = ValidatedSnapshot::from_parts(envelope, object_bytes, None)?;
    let importer = SnapshotImporter::new(
        AgentHubRepo::new(state.agent_hub_repo.pool().clone()),
        store.clone(),
        data_dir,
    );
    let _ = importer
        .commit_import(validated, ConfirmedImportSelection::default())
        .await?;
    Ok(())
}

#[allow(clippy::collapsible_if, clippy::collapsible_match)]
async fn install_payload_to_target(
    state: &AppState,
    store: &ObjectStore,
    item: &PortableSelectionItem,
    change: &PortablePullChangeDto,
) -> Result<(), AppError> {
    let bytes = store.get_blob(&item.object_hash).await?;
    if bytes_are_legacy_lossy(&bytes) {
        return Err(AppError::validation(
            "PORTABLE_PULL_LEGACY_LOSSY".to_string(),
        ));
    }
    // Project-scoped selection → mapped project asset root; never silent-install into user root.
    let root = resolve_install_root(state, item, change).await?;
    match item.kind {
        PortableAssetKind::Command => {
            let dir = root.join("commands");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join(format!("{}.md", item.native_id));
            if change.install_mode == PortablePullInstallMode::SkipExisting && path.exists() {
                return Ok(());
            }
            if path.exists()
                && matches!(
                    change.install_mode,
                    PortablePullInstallMode::InstallToTarget
                )
            {
                // replaceAfterPreview：允许覆盖；skipExisting 已在上分支处理
            }
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Command(cmd) = payload {
                    let text = format!(
                        "---\nname: {}\ndescription: {}\n---\n{}\n",
                        cmd.name,
                        cmd.description.as_deref().unwrap_or(""),
                        cmd.prompt_template
                    );
                    std::fs::write(&path, text)?;
                    return Ok(());
                }
            }
            std::fs::write(&path, &bytes)?;
        }
        PortableAssetKind::Skill => {
            let dir = root.join("skills").join(&item.native_id);
            if change.install_mode == PortablePullInstallMode::SkipExisting && dir.exists() {
                return Ok(());
            }
            // 优先 CAS 树还原完整 body（tree_hash 在 envelope revision / payload）
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Skill(skill) = payload {
                    if let Ok(manifest) = store.get_tree(&skill.tree_manifest_hash).await {
                        // Atomic replace: materialize to temp then rename over dest so
                        // replaceAfterPreview does not keep stale files from the old tree.
                        materialize_tree_atomic_replace(store, &dir, &manifest).await?;
                        // 若树未含 SKILL.md，至少写 frontmatter（fail-closed 再检查）
                        if !dir.join("SKILL.md").exists() {
                            return Err(AppError::validation(
                                "PORTABLE_PULL_SKILL_TREE_MISSING_SKILL_MD".to_string(),
                            ));
                        }
                        return Ok(());
                    }
                    // 无树：不得只写 frontmatter 丢 body —— fail-closed
                    return Err(AppError::validation(format!(
                        "PORTABLE_PULL_SKILL_TREE_UNAVAILABLE:{}",
                        skill.tree_manifest_hash
                    )));
                }
            }
            // 非 canonical skill payload：整文件当 SKILL.md（单文件 skill）
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("SKILL.md"), &bytes)?;
        }
        PortableAssetKind::Plugin => {
            let dir = root.join("plugins").join(&item.native_id);
            if change.install_mode == PortablePullInstallMode::SkipExisting && dir.exists() {
                return Ok(());
            }
            // 目录 Plugin：portablePluginTreeRef + CAS tree 还原；禁止 hash 清单当 plugin.json
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if v.get("kind").and_then(|k| k.as_str()) == Some("portablePluginTreeRef") {
                    let tree_hash = v
                        .get("treeManifestHash")
                        .and_then(|h| h.as_str())
                        .unwrap_or("");
                    if tree_hash.is_empty() {
                        return Err(AppError::validation(
                            "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE".to_string(),
                        ));
                    }
                    let manifest = store.get_tree(tree_hash).await.map_err(|_| {
                        AppError::validation(format!(
                            "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE:{tree_hash}"
                        ))
                    })?;
                    materialize_tree_atomic_replace(store, &dir, &manifest).await?;
                    return Ok(());
                }
                // 旧 path+hash-only 清单：fail-closed，绝不当 plugin.json 假成功
                if v.get("kind").and_then(|k| k.as_str()) == Some("portablePluginTree") {
                    return Err(AppError::validation(
                        "PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE".to_string(),
                    ));
                }
            }
            // 单文件 plugin.json body
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("plugin.json"), &bytes)?;
        }
        PortableAssetKind::Mcp => {
            // 权威 MCP 配置路径：Claude 用 home/.claude.json（非 ~/.claude/.claude.json）；
            // OpenCode 尊重 OPENCODE_CONFIG / jsonc。
            let path = match item.target {
                AgentTarget::Claude => {
                    if item.scope_id.starts_with("project:") {
                        // 项目 MCP：优先 `.mcp.json` 再 settings.local.json
                        let project_root = root
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| root.clone());
                        let mcp = project_root.join(".mcp.json");
                        if mcp.exists() {
                            mcp
                        } else {
                            root.join("settings.local.json")
                        }
                    } else {
                        resolve_claude_mcp_config_path()
                    }
                }
                AgentTarget::Codex | AgentTarget::Grok => root.join("config.toml"),
                AgentTarget::OpenCode => resolve_opencode_mcp_config_path(&root),
                AgentTarget::Gemini => root.join("settings.json"),
                AgentTarget::Cursor => root.join("mcp.json"),
                AgentTarget::Pi => {
                    return Ok(());
                }
            };
            if let Ok(payload) = from_canonical_bytes(&bytes) {
                if let PortableAssetPayload::Mcp(server) = payload {
                    if matches!(
                        item.target,
                        AgentTarget::Claude
                            | AgentTarget::OpenCode
                            | AgentTarget::Gemini
                            | AgentTarget::Cursor
                    ) {
                        // ownership-aware semantic patch；skipExisting 若 leaf 已存在则跳过
                        let current_bytes = if path.exists() {
                            std::fs::read(&path)?
                        } else {
                            b"{}".to_vec()
                        };
                        let existing = {
                            use crate::agent_hub::config_patch::SemanticConfigPatcher;
                            JsoncConfigPatcher
                                .inspect(&current_bytes, &["mcpServers".into(), server.key.clone()])
                                .ok()
                        };
                        if change.install_mode == PortablePullInstallMode::SkipExisting
                            && existing.as_ref().map(|o| o.present).unwrap_or(false)
                        {
                            return Ok(());
                        }
                        // 原生 CLI shape（command/args/env 或 url/headers），禁止 Hub DTO 直序列化
                        let value = native_mcp_leaf_value(item.target, &server)?;
                        let expected = existing.and_then(|o| {
                            if o.present {
                                Some(value_content_hash(&o.value))
                            } else {
                                None
                            }
                        });
                        let patches = [ManagedConfigPatch {
                            owner_id: format!("portable-pull:{}", server.key),
                            path: vec!["mcpServers".into(), server.key.clone()],
                            value: Some(value),
                            expected_base_hash: expected,
                        }];
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        let prepared =
                            apply_config_patch_atomically(&JsoncConfigPatcher, &path, &patches)?;
                        if !matches!(
                            prepared.patched.outcome,
                            crate::agent_hub::config_patch::ConfigPatchOutcome::Applied
                        ) {
                            return Err(AppError::conflict(format!(
                                "PORTABLE_PULL_MCP_PATCH:{:?}",
                                prepared.patched.outcome
                            )));
                        }
                        return Ok(());
                    }
                }
            }
            // 非 JSON MCP / Codex：fail-closed，禁止 silent side-file 冒充安装成功
            return Err(AppError::validation(
                "PORTABLE_PULL_MCP_INSTALL_UNSUPPORTED".to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
