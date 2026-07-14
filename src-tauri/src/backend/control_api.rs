//! backend/control_api.rs — loopback control API（status / get-config / update-config）。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 只能通过本机控制面读取/更新 sidecar 权威运行配置，不得自建第二 runtime。
//!     control API 与无鉴权 LAN 业务 API 分离：仅 loopback + control-file token。
//!
//! Code Logic（这个模块做什么）:
//!     提供 status / get-config / update-config / events / orchestrator snapshot /
//!     cloud-sync/{trigger,test,claude-md-push} / backup/{create,inspect,restore,list-jobs,list-backups,rollback} /
//!     transfer/prepare-open
//!     handler 与路由挂载；请求体 ≤256 KiB，普通元数据响应 ≤1 MiB；
//!     鉴权顺序：ConnectInfo loopback → token；cloud_sync_phase 映射真实 CloudSyncRuntime 相位；
//!     从不记录 control token。

use crate::backend::control::{self, BackendControlFile};
use crate::backend::event_bus::{BackendRuntimeCursor, RuntimeRelayMessage};
use crate::backup::{
    create_export_archive, list_pre_restore_backups, pre_restore_dir, pre_restore_infos_from_paths,
    BackupRestoreService, CreateBackupResult, InspectPreview, PreRestoreBackupInfo, RestoreMode,
    RestoreRequest, RestoreResult, FORMAT_VERSION,
};
use crate::commands::orchestrator::{
    get_orchestrator_runtime_snapshot_for_state_with_request_id, OrchestratorRuntimeSnapshotDto,
};
use crate::commands::transfer::prepare_transfer_open_for_state;
use crate::config_runtime::{
    ConfigSnapshot, ConfigUpdateResponse, OrchestratorRuntimeSummary, RuntimeConfigPatch,
    RuntimeOwnerStatus,
};
use crate::error::AppError;
use crate::models::transfer::{LocalTransferOpenTarget, TransferOpenAction};
use crate::net::error_response::{P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::require_loopback_peer;
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::RecoveryJobRow;
use axum::body::Body;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

/// control API 请求体上限（256 KiB）。
pub const CONTROL_REQUEST_BODY_LIMIT_BYTES: usize = 256 * 1024;
/// control API 普通元数据响应上限（1 MiB）。
pub const CONTROL_RESPONSE_BODY_LIMIT_BYTES: usize = 1024 * 1024;

/// 带 control token 的鉴权请求体（status / get-config）。
///
/// Business Logic（为什么需要这个结构）:
///     调用方必须证明读到本机控制文件令牌；token 只走请求体比较，不写日志。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase `controlToken`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlAuthRequest {
    pub control_token: String,
}

/// update-config HTTP 请求（token + CAS 字段）。
///
/// Business Logic（为什么需要这个结构）:
///     配置更新需同时鉴权（token）与 CAS（owner/generation/patch）。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + expectedOwnerInstanceId + expectedGeneration + patch。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlConfigUpdateRequest {
    pub control_token: String,
    pub expected_owner_instance_id: String,
    pub expected_generation: u64,
    pub patch: RuntimeConfigPatch,
}

/// get-config 响应包装。
///
/// Business Logic（为什么需要这个结构）:
///     与 status 区分，明确返回配置快照。
///
/// Code Logic（这个结构做什么）:
///     直接嵌套 ConfigSnapshot。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlConfigResponse {
    pub snapshot: ConfigSnapshot,
}

/// 返回 sidecar 权威 owner status。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 诊断/对账在 mutation 前后读取 owner/generation/fingerprint。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → 组装 RuntimeOwnerStatus（终端/bridge 计数取自 AppState 轻量观测）。
pub async fn control_status(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<RuntimeOwnerStatus>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let status = build_owner_status(&state)
        .map_err(|e| P2pError::from_app_error(e, &context, "control.status"))?;
    ensure_response_within_limit(&status, &context)?;
    Ok(Json(status))
}

/// 返回权威配置快照。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 在 generation 冲突后刷新表单需要完整 allowlisted 运行配置投影。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → `config_runtime.snapshot_with_generation`。
pub async fn control_get_config(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<ControlConfigResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let snapshot = state
        .config_runtime
        .snapshot_with_generation()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.get_config"))?;
    let body = ControlConfigResponse { snapshot };
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// CAS 更新权威运行配置。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 提交 allowlist patch + expected generation；sidecar 在既有事务 writer 下提交。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → `apply_patch_if_generation`；冲突映射为 409。
pub async fn control_update_config(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlConfigUpdateRequest>,
) -> P2pResult<Json<ConfigUpdateResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let response = state
        .config_runtime
        .apply_patch_if_generation(
            &request.expected_owner_instance_id,
            request.expected_generation,
            request.patch,
        )
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.update_config"))?;
    ensure_response_within_limit(&response, &context)?;
    Ok(Json(response))
}

/// Orchestrator runtime snapshot 请求（token + projectId）。
///
/// Business Logic（为什么需要这个结构）:
///     桌面 GUI 不得读本地空 telemetry，必须经 control 拉取 sidecar remote-aware 快照。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + projectId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRuntimeSnapshotRequest {
    pub control_token: String,
    pub project_id: String,
}

/// 事件 catch-up / stream 请求。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 用 afterSequence + owner 重连；owner 变化时服务端按新 owner 清旧游标语义处理。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + 可选 afterOwnerInstanceId + afterSequence。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEventsRequest {
    pub control_token: String,
    #[serde(default)]
    pub after_owner_instance_id: Option<String>,
    #[serde(default)]
    pub after_sequence: Option<u64>,
}

/// 事件 catch-up 响应。
///
/// Business Logic（为什么需要这个结构）:
///     批量回放 + 最新游标，便于 smoke 与 GUI 先 resync 再 attach live。
///
/// Code Logic（这个结构做什么）:
///     messages 为 Event/Gap；latest 为当前 owner 最新 cursor。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEventsCatchUpResponse {
    pub messages: Vec<RuntimeRelayMessage>,
    pub latest: BackendRuntimeCursor,
}

/// 返回 sidecar 权威 Orchestrator runtime snapshot。
///
/// Business Logic（为什么需要这个函数）:
///     桌面状态条必须展示 owner scheduler tick，禁止 GUI 用本地空 telemetry 补值。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → `get_orchestrator_runtime_snapshot_for_state`（owner 本地 remote-aware 路径）。
pub async fn control_orchestrator_runtime_snapshot(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlRuntimeSnapshotRequest>,
) -> P2pResult<Json<OrchestratorRuntimeSnapshotDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let snapshot = get_orchestrator_runtime_snapshot_for_state_with_request_id(
        &state,
        &request.project_id,
        None,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &context, "control.runtime_snapshot"))?;
    ensure_response_within_limit(&snapshot, &context)?;
    Ok(Json(snapshot))
}

/// 有界事件 catch-up（afterSequence）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 断线重连需要 ring 回放；若游标早于 ring 必须先收到 Gap。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → open_relay 排空 pending（catch-up 部分）→ 返回 messages + latest。
pub async fn control_events_catch_up(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlEventsRequest>,
) -> P2pResult<Json<ControlEventsCatchUpResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let after = match (
        request.after_owner_instance_id.as_deref(),
        request.after_sequence,
    ) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let mut relay = state.event_bus.open_relay(after.as_ref());
    let mut messages = Vec::new();
    while let Some(msg) = relay.try_recv() {
        messages.push(msg);
    }
    let latest = BackendRuntimeCursor {
        owner_instance_id: state.event_bus.owner_instance_id().to_string(),
        sequence: state.event_bus.latest_sequence(),
    };
    let body = ControlEventsCatchUpResponse { messages, latest };
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// 事件 NDJSON 流：先 catch-up，再 live（可取消连接即停）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 需要可取消的本机 relay，持续接收 terminal/merge/transfer/runtime 事件。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → open_relay → NDJSON stream；鉴权失败返回 401/403 JSON。
pub async fn control_events_stream(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlEventsRequest>,
) -> Response {
    if let Err(err) = authorize_control_request(peer, &context, &request.control_token) {
        return err.into_response();
    }
    let after = match (
        request.after_owner_instance_id.as_deref(),
        request.after_sequence,
    ) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let bus = Arc::clone(&state.event_bus);
    let relay = bus.open_relay(after.as_ref());
    let stream = stream::unfold(relay, |mut relay| async move {
        let msg = relay.recv().await?;
        let line = serde_json::to_string(&msg).ok()?;
        Some((Ok::<_, Infallible>(format!("{line}\n")), relay))
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/x-ndjson"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// loopback + control token 双重鉴权。
///
/// Business Logic（为什么需要这个函数）:
///     control API 不是 LAN 业务面：非本机 peer 即使持有 token 也必须 403；
///     token 不匹配返回 401；从不把 token 写入日志。
///
/// Code Logic（这个函数做什么）:
///     先 `require_loopback_peer`，再读控制文件比较 token；空 token 拒绝。
fn authorize_control_request(
    peer: SocketAddr,
    context: &P2pRequestContext,
    request_token: &str,
) -> Result<(), P2pError> {
    require_loopback_peer(peer.ip(), context)?;
    let control = control::read_control_file()
        .map_err(|_| P2pError::from_code("控制文件不可读", P2pErrorCode::Unauthorized, context))?;
    if !control_token_matches(request_token, control.as_ref()) {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            context,
        ));
    }
    Ok(())
}

/// 校验请求 token 是否匹配控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     与 stop route 一致：空 token 或缺失控制文件一律失败。
///
/// Code Logic（这个函数做什么）:
///     控制文件存在且请求 token 非空并与 `control_token` 完全一致。
fn control_token_matches(request_token: &str, control: Option<&BackendControlFile>) -> bool {
    let Some(control) = control else {
        return false;
    };
    !request_token.is_empty() && request_token == control.control_token
}

/// 从 AppState 组装 RuntimeOwnerStatus。
///
/// Business Logic（为什么需要这个函数）:
///     status 需要 owner/generation 与轻量 runtime 计数，供 GUI 诊断页展示。
///
/// Code Logic（这个函数做什么）:
///     terminal/bridge 计数取 list/len 轻量观测；cloud_sync_phase 映射 owner CloudSyncRuntime 真实相位；
///     orchestrator 摘要只暴露 tick 时间与错误类别 token，不回传原文。
fn build_owner_status(state: &AppState) -> Result<RuntimeOwnerStatus, AppError> {
    let terminal_session_count = state.workbench_sessions.list(None).len();
    let bridge_count = state.workbench_remote_event_bridges.active_bridge_count();
    let bridges = state.workbench_remote_event_bridges.snapshots();
    let orch_snap = state.orchestrator_scheduler_telemetry.snapshot();
    let orch = OrchestratorRuntimeSummary {
        latest_tick_at: orch_snap.latest_tick_at,
        latest_error_class: orch_snap
            .latest_error
            .as_ref()
            .map(|_| "scheduler_error".to_string()),
    };
    let cloud_sync_phase = state.cloud_sync_runtime.phase_token();
    state.config_runtime.owner_status_with_bridges(
        terminal_session_count,
        bridge_count,
        cloud_sync_phase,
        orch,
        bridges,
    )
}

/// CLAUDE.md 云端推送请求（token + 本机已保存 row 字段）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 先落本地文件/DB，再把权威 row 交给 owner 写 Git workdir；禁止 GUI 自建第二 git 临界区。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + content/updatedAt/deviceId/vectorClock。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlClaudeMdPushRequest {
    pub control_token: String,
    pub content: String,
    pub updated_at: String,
    pub device_id: String,
    pub vector_clock: std::collections::HashMap<String, u64>,
}

/// 手动触发 owner 侧 Cloud Sync。
///
/// Business Logic（为什么需要这个函数）:
///     GUI「立即同步」与 sidecar scheduler 必须共享同一 CloudSyncRuntime 单飞门闸。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `engine::trigger_cloud_sync`。
pub async fn control_cloud_sync_trigger(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<crate::cloud_sync::engine::CloudSyncResult>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.cloud_sync_trigger"))?;
    let result = crate::cloud_sync::engine::trigger_cloud_sync(&state).await;
    ensure_response_within_limit(&result, &context)?;
    Ok(Json(result))
}

/// 在 owner 侧测试 Cloud Sync 连通性。
///
/// Business Logic（为什么需要这个函数）:
///     连通性探测可能触达正式 workdir fetch，须与写路径同一 owner。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `engine::test_connection`。
pub async fn control_cloud_sync_test(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<crate::cloud_sync::engine::TestCloudSyncResult>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.cloud_sync_test"))?;
    let result = crate::cloud_sync::engine::test_connection(&state).await;
    ensure_response_within_limit(&result, &context)?;
    Ok(Json(result))
}

/// 在 owner 侧推送 CLAUDE.md 到 GitHub 云端工作区。
///
/// Business Logic（为什么需要这个函数）:
///     CLAUDE.md 云推送与完整 sync 共享 Git workdir 临界区；仅 sidecar HeadlessOwner 可写。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → 组装 ClaudeMdRow → `push_claude_md_to_cloud`。
pub async fn control_cloud_sync_claude_md_push(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlClaudeMdPushRequest>,
) -> P2pResult<Json<crate::cloud_sync::engine::CloudClaudeMdPushResultDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.cloud_sync_claude_md_push"))?;
    let row = crate::models::claude_md::ClaudeMdRow {
        id: crate::models::claude_md::CLAUDE_MD_ID.into(),
        content: request.content,
        updated_at: request.updated_at,
        device_id: request.device_id,
        vector_clock: request.vector_clock,
    };
    let result = crate::cloud_sync::engine::push_claude_md_to_cloud(&state, &row)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.cloud_sync_claude_md_push"))?;
    let dto = crate::cloud_sync::engine::CloudClaudeMdPushResultDto::from(result);
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

// ── backup control（N2 可验证导出/恢复）────────────────────────────────

/// backup/create 请求。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 选择目标路径后由 owner 写出可校验 ZIP。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + destPath。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBackupCreateRequest {
    pub control_token: String,
    pub dest_path: String,
}

/// backup/inspect 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBackupInspectRequest {
    pub control_token: String,
    pub archive_path: String,
}

/// backup/restore 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBackupRestoreRequest {
    pub control_token: String,
    pub archive_path: String,
    pub mode: RestoreMode,
    pub domains: Vec<String>,
}

/// backup/list-jobs 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBackupListJobsRequest {
    pub control_token: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// backup/rollback 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBackupRollbackRequest {
    pub control_token: String,
    pub job_id: String,
}

/// owner 侧创建导出备份。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 导出必须在 sidecar 读权威 DB 并写出 ZIP；GUI 只代理路径。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → create_export_archive → {path,formatVersion}。
pub async fn control_backup_create(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlBackupCreateRequest>,
) -> P2pResult<Json<CreateBackupResult>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_create"))?;
    let dest = PathBuf::from(&request.dest_path);
    create_export_archive(&state, &dest)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_create"))?;
    let result = CreateBackupResult {
        path: dest.display().to_string(),
        format_version: FORMAT_VERSION,
    };
    ensure_response_within_limit(&result, &context)?;
    Ok(Json(result))
}

/// owner 侧只读 inspect 备份包。
///
/// Business Logic（为什么需要这个函数）:
///     恢复确认前预览领域计数/警告；确认前零写入。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → BackupRestoreService::inspect。
pub async fn control_backup_inspect(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlBackupInspectRequest>,
) -> P2pResult<Json<InspectPreview>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_inspect"))?;
    let service = BackupRestoreService::new(state);
    let preview = service
        .inspect(PathBuf::from(&request.archive_path).as_path())
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_inspect"))?;
    ensure_response_within_limit(&preview, &context)?;
    Ok(Json(preview))
}

/// owner 侧事务恢复。
///
/// Business Logic（为什么需要这个函数）:
///     merge/replace-domain 恢复必须持 exclusive maintenance_gate，仅 owner 可写。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → service.restore(RestoreRequest)。
pub async fn control_backup_restore(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlBackupRestoreRequest>,
) -> P2pResult<Json<RestoreResult>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_restore"))?;
    let service = BackupRestoreService::new(state);
    let result = service
        .restore(RestoreRequest {
            archive_path: request.archive_path,
            mode: request.mode,
            domains: request.domains,
        })
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_restore"))?;
    ensure_response_within_limit(&result, &context)?;
    Ok(Json(result))
}

/// owner 侧列出 recovery jobs。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 展示最近恢复历史；读路径仍要求 owner（job 表在 sidecar DB）。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → list_jobs(limit default 50)。
pub async fn control_backup_list_jobs(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlBackupListJobsRequest>,
) -> P2pResult<Json<Vec<RecoveryJobRow>>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_list_jobs"))?;
    let limit = request.limit.unwrap_or(50);
    let service = BackupRestoreService::new(state);
    let jobs = service
        .list_jobs(limit)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_list_jobs"))?;
    ensure_response_within_limit(&jobs, &context)?;
    Ok(Json(jobs))
}

/// owner 侧列出 pre-restore 备份文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户查看恢复前自动备份路径与时间戳。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → list_pre_restore_backups → PreRestoreBackupInfo[]。
pub async fn control_backup_list_backups(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<Vec<PreRestoreBackupInfo>>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_list_backups"))?;
    let dir = pre_restore_dir()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_list_backups"))?;
    let paths = list_pre_restore_backups(&dir)
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_list_backups"))?;
    let infos = pre_restore_infos_from_paths(&paths);
    ensure_response_within_limit(&infos, &context)?;
    Ok(Json(infos))
}

/// owner 侧按 job 回退。
///
/// Business Logic（为什么需要这个函数）:
///     恢复失败/误操作时用 pre-restore 备份 replace-domain 回灌。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → rollback_job。
pub async fn control_backup_rollback(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlBackupRollbackRequest>,
) -> P2pResult<Json<RestoreResult>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_rollback"))?;
    let service = BackupRestoreService::new(state);
    let result = service
        .rollback_job(&request.job_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.backup_rollback"))?;
    ensure_response_within_limit(&result, &context)?;
    Ok(Json(result))
}

// ── transfer lifecycle control（N5 Open/Reveal prepare）────────────────

/// transfer prepare-open 请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     GuiClient 经 loopback control 向 sidecar 索取 completed Receive 的 local path；
///     不得经 P2P/mobile 暴露路径。
///
/// Code Logic（这个结构体做什么）:
///     camelCase：controlToken + taskId + action(open|reveal)。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferPrepareOpenRequest {
    pub control_token: String,
    pub task_id: String,
    pub action: TransferOpenAction,
}

/// 为 same-device GUI 准备 Open/Reveal local target。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 是 transfer_history 与最终落盘路径的权威；GUI 只拿 local target 再调 opener。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → prepare_transfer_open_for_state → LocalTransferOpenTarget。
pub async fn control_transfer_prepare_open(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferPrepareOpenRequest>,
) -> P2pResult<Json<LocalTransferOpenTarget>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_prepare_open"))?;
    let target = prepare_transfer_open_for_state(&state, &request.task_id, request.action)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_prepare_open"))?;
    ensure_response_within_limit(&target, &context)?;
    Ok(Json(target))
}

/// 序列化后检查响应不超过 1 MiB。
///
/// Business Logic（为什么需要这个函数）:
///     control 元数据响应有独立 1 MiB 上限，防止意外膨胀。
///
/// Code Logic（这个函数做什么）:
///     serde_json 序列化后比长度；超限返回 413。
fn ensure_response_within_limit<T: Serialize>(
    value: &T,
    context: &P2pRequestContext,
) -> Result<(), P2pError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| P2pError::from_code("控制响应序列化失败", P2pErrorCode::Internal, context))?;
    if encoded.len() > CONTROL_RESPONSE_BODY_LIMIT_BYTES {
        return Err(P2pError::from_code(
            "控制响应超过 1 MiB 限制",
            P2pErrorCode::PayloadTooLarge,
            context,
        ));
    }
    Ok(())
}

/// 为测试注入 control 鉴权（不经过真实磁盘控制文件）。
///
/// Business Logic（为什么需要这个函数）:
///     单测需覆盖 wrong-token / non-loopback，而不依赖全局控制文件路径。
///
/// Code Logic（这个函数做什么）:
///     暴露 loopback + token 比较的 pure helper。
#[cfg(test)]
pub(crate) fn authorize_control_for_test(
    peer: SocketAddr,
    context: &P2pRequestContext,
    request_token: &str,
    control: Option<&BackendControlFile>,
) -> Result<(), P2pError> {
    require_loopback_peer(peer.ip(), context)?;
    if !control_token_matches(request_token, control) {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            context,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::authority::CONTROL_SCHEMA_VERSION;
    use crate::backend::control::BackendControlFile;
    use crate::config::{
        AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use crate::net::request_context::P2pRequestContext;
    use axum::http::StatusCode;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    fn test_ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-control-test".into(),
        }
    }

    fn loopback_peer() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, 9))
    }

    fn lan_peer() -> SocketAddr {
        SocketAddr::from((Ipv4Addr::new(192, 168, 1, 50), 9))
    }

    fn control_file(token: &str) -> BackendControlFile {
        BackendControlFile {
            pid: 1,
            port: 62116,
            device_id: "device-a".into(),
            device_name: "Desk".into(),
            started_at: "2026-07-14T00:00:00Z".into(),
            control_token: token.into(),
            control_schema_version: CONTROL_SCHEMA_VERSION,
            owner_instance_id: Some("owner-a".into()),
        }
    }

    /// 错误 token 必须 401 unauthorized。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     无令牌调用方不得读取/更新 control 面。
    ///
    /// Code Logic（这个测试做什么）:
    ///     loopback + wrong token → Unauthorized。
    #[test]
    fn wrong_token_is_rejected() {
        let ctx = test_ctx();
        let control = control_file("expected-token");
        let err = authorize_control_for_test(loopback_peer(), &ctx, "wrong-token", Some(&control))
            .expect_err("wrong token");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.envelope().code, "unauthorized");
    }

    /// 非 loopback 即使 token 正确也 403。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     control API 不得从局域网对端调用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     LAN peer + 正确 token → Forbidden。
    #[test]
    fn non_loopback_is_rejected_even_with_valid_token() {
        let ctx = test_ctx();
        let control = control_file("expected-token");
        let err = authorize_control_for_test(lan_peer(), &ctx, "expected-token", Some(&control))
            .expect_err("non-loopback");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.envelope().code, "forbidden");
    }

    /// loopback + 正确 token 通过。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     合法本机 GUI/CLI 必须能进入 control 面。
    ///
    /// Code Logic（这个测试做什么）:
    ///     authorize_control_for_test 返回 Ok。
    #[test]
    fn loopback_with_valid_token_accepted() {
        let ctx = test_ctx();
        let control = control_file("expected-token");
        authorize_control_for_test(loopback_peer(), &ctx, "expected-token", Some(&control))
            .expect("should accept");
    }

    /// prepare-open 请求体 camelCase 反序列化。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GuiClient 与 sidecar 共享 control contract：taskId + action。
    ///
    /// Code Logic（这个测试做什么）:
    ///     JSON `{controlToken,taskId,action:"reveal"}` → 字段对齐。
    #[test]
    fn transfer_prepare_open_request_deserializes_camel_case() {
        let raw = r#"{"controlToken":"tok","taskId":"t-1","action":"reveal"}"#;
        let req: ControlTransferPrepareOpenRequest =
            serde_json::from_str(raw).expect("deserialize prepare-open body");
        assert_eq!(req.control_token, "tok");
        assert_eq!(req.task_id, "t-1");
        assert_eq!(req.action, TransferOpenAction::Reveal);
    }

    /// CAS 经 control 路径：正确 generation 成功，旧 generation 冲突。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     update-config 最终落到 ConfigRuntime CAS。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 runtime，成功一次 generation=1，再用 0 调用失败。
    #[tokio::test]
    async fn update_config_cas_path_conflict_on_stale_generation() {
        let initial = AppConfig {
            device_id: "dev-a".into(),
            device_name: "n".into(),
            http_port: 0,
            receive_dir: "/tmp/r".into(),
            db_path: "/tmp/d.db".into(),
            screenshot_hotkey: "<cmd>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::with_owner(initial, store, "owner-a".into());
        let ok = runtime
            .apply_patch_if_generation(
                "owner-a",
                0,
                RuntimeConfigPatch {
                    device_name: Some("next".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("first");
        assert_eq!(ok.generation, 1);
        let err = runtime
            .apply_patch_if_generation(
                "owner-a",
                0,
                RuntimeConfigPatch {
                    device_name: Some("stale".into()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("stale");
        assert_eq!(err.to_string(), "config_generation_conflict");
    }
}
