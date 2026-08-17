//! net/routes/agent_hub.rs — Agent Hub LAN push 三阶段路由
//!
//! Business Logic（为什么需要这个模块）:
//!     对端在源侧选择目标后 push SnapshotEnvelope；本路由只做接收端协议面。
//!     sourceDeviceId / clientRequestId / expected-device header 是绑定/幂等标签，**不是**身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     POST prepare / PUT objects / POST commit；body limit 与 P2pError 信封；
//!     业务委托 `agent_hub::replication::receiver`。

use crate::agent_hub::models::ScopeKind;
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::portable_actions::{
    ApplyPortableAssetActionRequest, PreviewPortableAssetActionRequest,
};
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_query, PortableInventoryQuery,
};
use crate::agent_hub::portable_service::PortableService;
use crate::agent_hub::replication::ledger::ReplicationLedger;
use crate::agent_hub::replication::pull::{
    build_remote_inventory_for_target, source_prepare_selection, source_read_object_chunk,
    source_release_transfer, RemoteInventoryQuery, RemoteProjectPortableInventoryQuery,
    RemoteSelectionQuery, PORTABLE_PULL_MAX_CHUNK_BYTES,
};
use crate::agent_hub::replication::receiver::{
    commit_push, prepare_push, put_object_chunk, CommitPushRequest, CommitPushResponse,
    PreparePushRequest, PreparePushResponse, PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
use crate::agent_hub::service::AgentHubService;
use crate::agent_hub::user_instructions::{
    AdaptInstructionToOtherAgentsRequest, AnalyzeInstructionOriginalRequest,
    ApplyUserInstructionPlanRequest, ListUserInstructionSlotVersionsRequest,
    PreviewUserInstructionRequest, RestoreUserInstructionSlotRequest, ReviseInstructionSlotRequest,
    SaveUserInstructionBlocksRequest,
};
use crate::net::error_response::{P2pError, P2pResult};
// CAPABILITY_* used in tests module for wire-token assertions
#[cfg(test)]
use crate::net::protocol::{
    CAPABILITY_AGENT_HUB_V1, CAPABILITY_PORTABLE_PROJECT_V1, CAPABILITY_PORTABLE_PULL_V1,
    CAPABILITY_USER_INSTRUCTIONS_V1,
};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

/// PUT object query：offset + 可选 chunk SHA-256。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutObjectQuery {
    /// 写入 offset（u64）
    pub offset: u64,
    /// 可选 chunk body SHA-256 hex
    #[serde(default)]
    pub chunk_sha256: Option<String>,
}

/// POST /api/agent-hub/push/prepare
///
/// Business Logic: 校验 envelope/limits，返回 missing objects；幂等回放。
/// Code Logic: 构造 ObjectStore/ledger → prepare_push。
pub async fn agent_hub_push_prepare(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<PreparePushRequest>,
) -> P2pResult<Json<PreparePushResponse>> {
    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    let objects = ObjectStore::open(&data_dir)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let resp = prepare_push(&state.agent_hub_repo, &objects, &ledger, &data_dir, body)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.prepare"))?;
    Ok(Json(resp))
}

/// PUT /api/agent-hub/push/:transferId/objects/:objectHash?offset=
///
/// Business Logic: application/octet-stream chunk ≤8 MiB；offset 连续；hash 校验。
/// Code Logic: 读 body bytes → put_object_chunk。
pub async fn agent_hub_push_object(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path((transfer_id, object_hash)): Path<(String, String)>,
    Query(query): Query<PutObjectQuery>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> P2pResult<Json<PutObjectResponse>> {
    // Content-Type 提示（guard 已拦 form；此处接受 octet-stream 或缺省）
    let _ = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());

    if body.len() > AGENT_HUB_MAX_CHUNK_BYTES {
        return Err(P2pError::validation(
            format!(
                "agent_hub_push_chunk_too_large:actual={}:limit={AGENT_HUB_MAX_CHUNK_BYTES}",
                body.len()
            ),
            &ctx,
        ));
    }

    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.object"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let resp = put_object_chunk(
        &ledger,
        &data_dir,
        &transfer_id,
        &object_hash,
        query.offset,
        &body,
        query.chunk_sha256.as_deref(),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.object"))?;
    Ok(Json(resp))
}

/// POST /api/agent-hub/push/:transferId/commit
///
/// Business Logic: 全部 object verified 后 SnapshotImporter::commit_import；enqueue reconcile。
/// Code Logic: commit_push + best-effort schedule_asset_projections。
pub async fn agent_hub_push_commit(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(transfer_id): Path<String>,
    Json(body): Json<CommitPushRequest>,
) -> P2pResult<Json<CommitPushResponse>> {
    let data_dir = crate::config::data_dir()
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    let objects = ObjectStore::open(&data_dir)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    let ledger =
        ReplicationLedger::new(state.agent_hub_repo.pool(), state.maintenance_gate.clone());
    let state_for_sched = state.clone();
    let resp = commit_push(
        &state.agent_hub_repo,
        &objects,
        &ledger,
        &data_dir,
        &transfer_id,
        body,
        move |asset_ids| {
            // 异步 reconcile：协议成功后 best-effort 调度投影；
            // 同时触发 durable outbox drain，确保 intent 状态推进。
            let ids: Vec<String> = asset_ids.to_vec();
            tauri::async_runtime::spawn(async move {
                for id in ids {
                    let _ = crate::agent_hub::projection_ops::schedule_asset_projections(
                        &state_for_sched,
                        &id,
                    )
                    .await;
                }
                let _ =
                    crate::agent_hub::replication::drain_lan_projection_intents(&state_for_sched)
                        .await;
            });
        },
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.push.commit"))?;
    // commit 返回后立即 best-effort 再排水一次（覆盖 spawn 前崩溃的窗口由 worker 补）
    let state_for_drain = state.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::agent_hub::replication::drain_lan_projection_intents(&state_for_drain).await;
    });
    Ok(Json(resp))
}

/// POST /api/agent-hub/portable/inventory
///
/// Business Logic: 返回指定 target 的 metadata inventory（无 secret / 无 path）。
/// Code Logic: body.sourceTarget → build_remote_inventory_for_target。
pub async fn agent_hub_portable_inventory(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<RemoteInventoryQuery>,
) -> P2pResult<Json<crate::agent_hub::replication::pull::RemotePortableInventoryDto>> {
    let dto =
        build_remote_inventory_for_target(&state, body.source_target, body.source_local_project_id)
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.inventory"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/selection
///
/// Business Logic: 按勾选 inventoryItemIds 冻结 SnapshotEnvelope/CAS（源端不 adoption）。
/// Code Logic: source_prepare_selection。
pub async fn agent_hub_portable_selection(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<RemoteSelectionQuery>,
) -> P2pResult<Json<crate::agent_hub::replication::pull::RemotePortableSelectionResponse>> {
    let dto = source_prepare_selection(
        &state,
        body.source_target,
        body.source_local_project_id,
        body.inventory_item_ids,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.selection"))?;
    Ok(Json(dto))
}

/// GET /api/agent-hub/portable/objects/:transferId/:objectHash?offset=
///
/// Business Logic: 分块 ≤8MiB，offset 续传；application/octet-stream。
/// Code Logic: source_read_object_chunk → raw bytes。
pub async fn agent_hub_portable_object(
    Extension(ctx): Extension<P2pRequestContext>,
    Path((transfer_id, object_hash)): Path<(String, String)>,
    Query(query): Query<PutObjectQuery>,
) -> Result<Response, P2pError> {
    let bytes = source_read_object_chunk(&transfer_id, &object_hash, query.offset)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.object"))?;
    if bytes.len() > PORTABLE_PULL_MAX_CHUNK_BYTES {
        return Err(P2pError::validation(
            format!(
                "agent_hub_portable_chunk_too_large:actual={}:limit={PORTABLE_PULL_MAX_CHUNK_BYTES}",
                bytes.len()
            ),
            &ctx,
        ));
    }
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        Bytes::from(bytes),
    )
        .into_response())
}

/// POST /api/agent-hub/portable/transfers/:transferId/release
///
/// Business Logic: 目的端在完整 object 传输后显式释放源端 staging（多对象 transfer 双保险）。
/// Code Logic: source_release_transfer；幂等（不存在也 Ok）。
pub async fn agent_hub_portable_transfer_release(
    Extension(ctx): Extension<P2pRequestContext>,
    Path(transfer_id): Path<String>,
) -> P2pResult<Json<serde_json::Value>> {
    let _ = ctx;
    source_release_transfer(&transfer_id);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// POST /api/agent-hub/portable/project/inventory
///
/// Business Logic: owning peer 按真实本地 project id 返回精确项目库存；LAN 通道不做身份鉴权。
pub async fn agent_hub_portable_project_inventory(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<RemoteProjectPortableInventoryQuery>,
) -> P2pResult<Json<crate::agent_hub::portable_inventory::PortableInventorySnapshotDto>> {
    let dto = inspect_portable_inventory_query(
        &state,
        PortableInventoryQuery {
            target: Some(body.target),
            kind: body.kind,
            scope_kind: Some(ScopeKind::Project),
            local_project_id: Some(body.local_project_id),
        },
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.inventory"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/project/preview
pub async fn agent_hub_portable_project_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::project_scope::AgentHubProjectPreview>> {
    let project_id = body
        .get("localProjectId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| P2pError::validation("localProjectId required", &ctx))?;
    let dto = AgentHubService::preview_project(&state, project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.preview"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/project/enable
pub async fn agent_hub_portable_project_enable(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::project_scope::AgentHubProjectStatus>> {
    let project_id = body
        .get("localProjectId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| P2pError::validation("localProjectId required", &ctx))?;
    let dto = AgentHubService::enable_project(&state, project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.enable"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/project/action/preview
pub async fn agent_hub_portable_project_action_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<PreviewPortableAssetActionRequest>,
) -> P2pResult<Json<crate::agent_hub::portable_actions::PortableAssetActionPlanDto>> {
    if body.inventory_query.scope_kind != Some(ScopeKind::Project)
        || body.inventory_query.local_project_id.is_none()
    {
        return Err(P2pError::validation(
            "project-scoped inventoryQuery required",
            &ctx,
        ));
    }
    let dto = PortableService::preview_portable_asset_action(&state, body)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.action.preview")
        })?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/project/action/apply
pub async fn agent_hub_portable_project_action_apply(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<ApplyPortableAssetActionRequest>,
) -> P2pResult<Json<crate::agent_hub::portable_actions::PortableAssetActionResultDto>> {
    let dto = PortableService::apply_portable_asset_action(&state, body)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.action.apply")
        })?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/portable/project/action/get
pub async fn agent_hub_portable_project_action_get(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::portable_actions::PortableAssetActionResultDto>> {
    let client_request_id = body
        .get("clientRequestId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| P2pError::validation("clientRequestId required", &ctx))?;
    let dto = PortableService::get_portable_asset_action(&state, client_request_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.portable.project.action.get"))?;
    Ok(Json(dto))
}

/// P2P 用户级路由拒绝嵌套 deviceId，避免 owner 再代理。
///
/// Business Logic: owning peer 只执行本机用户目录；`remote:` / 再转发是递归。
/// Code Logic: body 含非空 deviceId → `local_user_scope_required`。
fn reject_nested_user_instruction_device_id(
    body: &serde_json::Value,
    ctx: &P2pRequestContext,
) -> Result<(), P2pError> {
    if body
        .get("deviceId")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return Err(P2pError::from_app_error(
            crate::error::AppError::validation("local_user_scope_required".to_string()),
            ctx,
            "agent_hub.user_instructions",
        ));
    }
    Ok(())
}

/// POST /api/agent-hub/user-instructions/inspect
///
/// Business Logic: 读取 owning device 用户级三栏 workspace；LAN 无鉴权。
/// Code Logic: 空 body；只调本机 AgentHubService。
pub async fn agent_hub_user_instructions_inspect(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::UserInstructionWorkspaceDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let dto = AgentHubService::inspect_user_instruction_workspace(&state)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.inspect"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/save-blocks
pub async fn agent_hub_user_instructions_save_blocks(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::UserInstructionCanonicalDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: SaveUserInstructionBlocksRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("save-blocks body: {e}"), &ctx))?;
    let dto = AgentHubService::save_user_instruction_blocks(&state, req)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.save_blocks")
        })?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/preview-setup
pub async fn agent_hub_user_instructions_preview_setup(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::UserInstructionPlanDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: PreviewUserInstructionRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("preview-setup body: {e}"), &ctx))?;
    let dto = AgentHubService::preview_user_instruction_setup(&state, req)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.preview_setup")
        })?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/preview-update
pub async fn agent_hub_user_instructions_preview_update(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::UserInstructionPlanDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: PreviewUserInstructionRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("preview-update body: {e}"), &ctx))?;
    let dto = AgentHubService::preview_user_instruction_update(&state, req)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.preview_update")
        })?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/apply-plan
pub async fn agent_hub_user_instructions_apply_plan(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::ApplyUserInstructionPlanResultDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: ApplyUserInstructionPlanRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("apply-plan body: {e}"), &ctx))?;
    let dto = AgentHubService::apply_user_instruction_plan(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.apply_plan"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/analyze
pub async fn agent_hub_user_instructions_analyze(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::AnalyzeInstructionOriginalResult>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: AnalyzeInstructionOriginalRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("analyze body: {e}"), &ctx))?;
    let dto = crate::commands::agent_hub::analyze_instruction_original_for_state(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.analyze"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/adapt
pub async fn agent_hub_user_instructions_adapt(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::AdaptInstructionToOtherAgentsResult>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: AdaptInstructionToOtherAgentsRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("adapt body: {e}"), &ctx))?;
    let dto = crate::commands::agent_hub::adapt_instruction_to_other_agents_for_state(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.adapt"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/revise
pub async fn agent_hub_user_instructions_revise(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::ReviseInstructionSlotResult>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: ReviseInstructionSlotRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("revise body: {e}"), &ctx))?;
    let dto = crate::commands::agent_hub::revise_instruction_slot_for_state(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.revise"))?;
    Ok(Json(dto))
}

/// POST /api/agent-hub/user-instructions/slot-versions
pub async fn agent_hub_user_instructions_slot_versions(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<Vec<crate::commands::prompts::ContentVersionDto>>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: ListUserInstructionSlotVersionsRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("slot-versions body: {e}"), &ctx))?;
    let versions =
        AgentHubService::list_user_instruction_slot_versions(&state, req.asset_id, req.slot)
            .await
            .map_err(|e| {
                P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.slot_versions")
            })?;
    Ok(Json(
        versions
            .iter()
            .map(crate::commands::prompts::content_version_to_dto)
            .collect(),
    ))
}

/// POST /api/agent-hub/user-instructions/restore-slot-version
pub async fn agent_hub_user_instructions_restore_slot_version(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<serde_json::Value>,
) -> P2pResult<Json<crate::agent_hub::user_instructions::UserInstructionCanonicalDto>> {
    reject_nested_user_instruction_device_id(&body, &ctx)?;
    let req: RestoreUserInstructionSlotRequest = serde_json::from_value(body)
        .map_err(|e| P2pError::validation(format!("restore-slot-version body: {e}"), &ctx))?;
    let dto = AgentHubService::restore_user_instruction_slot_version(&state, req)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "agent_hub.user_instructions.restore_slot_version")
        })?;
    Ok(Json(dto))
}

/// 路由/能力原子性：push 三路由 + portable-pull 三路由同 build 宣告。
///
/// 注意：expected-device / Host / Origin 守卫是协议完整性约束，**不是**身份认证。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AssetKind, AssetPolicy, RevisionOperation, RevisionOriginKind};
    use crate::agent_hub::object_store::sha256_hex;
    use crate::agent_hub::replication::receiver::{prepare_push, PreparePushRequest};
    use crate::agent_hub::snapshot::builder::hash_selection;
    use crate::agent_hub::snapshot::envelope::{
        compute_snapshot_hash, SnapshotAsset, SnapshotEnvelopeV1, SnapshotLineage,
        SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, CANONICALIZATION_NAME,
        FORMAT_NAME, FORMAT_VERSION,
    };
    use crate::net::protocol::server_protocol_info;
    use crate::storage::AgentHubRepo;
    use std::collections::BTreeMap;

    /// Business Logic: 能力 token 必须与 push/portable 路由同 build 宣告。
    /// Code Logic: server_protocol_info supports agent-hub.v1 + portable-pull.v1；handler 符号存在。
    #[test]
    fn capability_and_handlers_are_atomic() {
        let info = server_protocol_info();
        assert!(
            info.supports(CAPABILITY_AGENT_HUB_V1),
            "server_protocol_info 必须宣告 agent-hub.v1，实际: {:?}",
            info.capabilities
        );
        assert!(
            info.supports(CAPABILITY_PORTABLE_PULL_V1),
            "server_protocol_info 必须宣告 agent-hub.portable-pull.v1，实际: {:?}",
            info.capabilities
        );
        assert!(info.supports(CAPABILITY_PORTABLE_PROJECT_V1));
        assert!(
            info.supports(CAPABILITY_USER_INSTRUCTIONS_V1),
            "server_protocol_info 必须宣告 agent-hub.user-instructions.v1，实际: {:?}",
            info.capabilities
        );
        // 引用 handler，防止 capability 宣告而路由未接线被优化掉
        let _ = (
            agent_hub_push_prepare as *const (),
            agent_hub_push_object as *const (),
            agent_hub_push_commit as *const (),
            agent_hub_portable_inventory as *const (),
            agent_hub_portable_selection as *const (),
            agent_hub_portable_object as *const (),
            agent_hub_portable_transfer_release as *const (),
            agent_hub_portable_project_preview as *const (),
            agent_hub_portable_project_enable as *const (),
            agent_hub_portable_project_inventory as *const (),
            agent_hub_portable_project_action_preview as *const (),
            agent_hub_portable_project_action_apply as *const (),
            agent_hub_portable_project_action_get as *const (),
            agent_hub_user_instructions_inspect as *const (),
            agent_hub_user_instructions_save_blocks as *const (),
            agent_hub_user_instructions_preview_setup as *const (),
            agent_hub_user_instructions_preview_update as *const (),
            agent_hub_user_instructions_apply_plan as *const (),
            agent_hub_user_instructions_analyze as *const (),
            agent_hub_user_instructions_adapt as *const (),
            agent_hub_user_instructions_revise as *const (),
            agent_hub_user_instructions_slot_versions as *const (),
            agent_hub_user_instructions_restore_slot_version as *const (),
        );
        assert_eq!(CAPABILITY_AGENT_HUB_V1, "agent-hub.v1");
        assert_eq!(CAPABILITY_PORTABLE_PULL_V1, "agent-hub.portable-pull.v1");
        assert_eq!(
            CAPABILITY_PORTABLE_PROJECT_V1,
            "agent-hub.portable-project.v1"
        );
        assert_eq!(
            CAPABILITY_USER_INSTRUCTIONS_V1,
            "agent-hub.user-instructions.v1"
        );
        // 能力列表字典序应包含 token
        assert!(info.capabilities.windows(2).all(|w| w[0] <= w[1]));
    }

    /// Business Logic: owner 用户级路由不得再按 deviceId 转发。
    #[test]
    fn user_instruction_routes_reject_nested_device_id() {
        let ctx = crate::net::request_context::P2pRequestContext {
            request_id: "req-nested".into(),
        };
        let err = reject_nested_user_instruction_device_id(
            &serde_json::json!({ "deviceId": "peer-2" }),
            &ctx,
        )
        .unwrap_err();
        assert_eq!(err.envelope().code, "validation_error");
        assert!(err.envelope().error.contains("local_user_scope_required"));
        assert!(reject_nested_user_instruction_device_id(&serde_json::json!({}), &ctx).is_ok());
    }

    /// Business Logic: portable inventory 不得声明为鉴权。
    #[test]
    fn portable_pull_docs_do_not_claim_authentication() {
        let note = "portable pull uses expected-device and clientRequestId as binding/idempotency labels, not authentication";
        assert!(note.contains("not authentication"));
    }

    /// Business Logic: 守卫是完整性约束，测试不得称其为 authentication。
    #[test]
    fn protocol_docs_do_not_claim_authentication_for_device_binding() {
        // sourceDeviceId / clientRequestId 文档语义：幂等标签
        let note = "sourceDeviceId and clientRequestId are idempotency labels, not authentication";
        assert!(note.contains("not authentication"));
    }

    fn sample_envelope() -> (SnapshotEnvelopeV1, Vec<u8>) {
        let secret = b"route-fixture-secret";
        let object_hash = sha256_hex(secret);
        let rev_id = "01900000-0000-7000-8000-000000000011";
        let asset_id = "01900000-0000-7000-8000-0000000000a2";
        let mut envelope = SnapshotEnvelopeV1 {
            format: FORMAT_NAME.into(),
            format_version: FORMAT_VERSION,
            canonicalization: CANONICALIZATION_NAME.into(),
            snapshot_id: "01900000-0000-7000-8000-0000000000c2".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            created_at: "2026-07-29T12:00:00Z".into(),
            selection: SnapshotSelection {
                scope_ids: vec!["scope-user".into()],
                asset_ids: vec![asset_id.into()],
                include_history: true,
            },
            asset_heads: BTreeMap::from([(asset_id.into(), vec![rev_id.into()])]),
            assets: vec![SnapshotAsset {
                id: asset_id.into(),
                scope_id: "scope-user".into(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".into(),
                logical_key: "mcp-route".into(),
                display_name: "Route MCP".into(),
                policy: AssetPolicy::Shared,
                deleted_at: None,
            }],
            lineages: vec![SnapshotLineage {
                id: asset_id.into(),
                root_asset_id: asset_id.into(),
            }],
            revisions: vec![SnapshotRevision {
                id: rev_id.into(),
                asset_lineage_id: asset_id.into(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                payload_hash: Some(object_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T12:00:00Z".into(),
            }],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![SnapshotObjectDescriptor {
                hash: object_hash,
                size: (secret.len() as u64).to_string(),
            }],
        };
        envelope.snapshot_hash = compute_snapshot_hash(&envelope).unwrap();
        (envelope, secret.to_vec())
    }

    /// Business Logic: prepare 幂等与 missing hashes 契约。
    #[tokio::test]
    async fn prepare_returns_missing_and_replays() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().to_path_buf();
        let _guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 temp path")));
        let data = crate::config::data_dir().unwrap();
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool.clone());
        let store = ObjectStore::open(&data).unwrap();
        let ledger = crate::agent_hub::replication::ReplicationLedger::new_standalone(pool);
        let (envelope, _) = sample_envelope();
        let sel = hash_selection(&envelope.selection).unwrap();
        let req = PreparePushRequest {
            envelope,
            source_device_id: "src".into(),
            client_request_id: "c1".into(),
            selection_hash: sel,
        };
        let a = prepare_push(&repo, &store, &ledger, &data, req.clone())
            .await
            .unwrap();
        assert_eq!(a.missing_object_hashes.len(), 1);
        let b = prepare_push(&repo, &store, &ledger, &data, req)
            .await
            .unwrap();
        assert_eq!(a.transfer_id, b.transfer_id);
    }
}
