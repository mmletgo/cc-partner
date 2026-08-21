//! backend/control_api.rs — loopback control API（status / get-config / update-config）。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 只能通过本机控制面读取/更新 sidecar 权威运行配置，不得自建第二 runtime。
//!     control API 与无鉴权 LAN 业务 API 分离：仅 loopback + control-file token。
//!
//! Code Logic（这个模块做什么）:
//!     提供 status / get-config / update-config / events / orchestrator snapshot /
//!     orchestrator/{deliver-reviewed,complete-agent-run,dispatch-once,workflow-document/{get,validate,save}} /
//!     orchestrator/experiments/{create,list,get,approve-winner,cancel,prepare-downgrade} /
//!     workbench-launch-summary（5 段独立 section outcomes，每段 max 5）/
//!     cloud-sync/{trigger,test,claude-md-push} / backup/{create,inspect,restore,list-jobs,list-backups,rollback} /
//!     transfer/prepare-open + transfer/{send,retry,resume,get-operation,cancel}
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
    append_orchestrator_task_block_member_view_for_state,
    approve_orchestrator_experiment_winner_for_state, cancel_orchestrator_experiment_for_state,
    complete_orchestrator_agent_run_for_state, create_orchestrator_experiment_for_state,
    create_orchestrator_task_block_view_for_state,
    deliver_reviewed_orchestrator_task_view_for_state, get_orchestrator_experiment_for_state,
    get_orchestrator_runtime_snapshot_for_state_with_request_id, get_workflow_document_for_state,
    list_orchestrator_experiments_for_state, prepare_experiment_downgrade_for_state,
    reorder_orchestrator_task_block_members_view_for_state, save_workflow_document_for_state,
    validate_workflow_document_for_state, AppendOrchestratorTaskBlockMemberRequest,
    CreateOrchestratorTaskBlockRequest, OrchestratorRuntimeSnapshotDto,
    OrchestratorTaskBlockViewCreatedDto, OrchestratorTaskViewDto,
    ReorderOrchestratorTaskBlockMembersRequest,
};
use crate::commands::transfer::prepare_transfer_open_for_state;
use crate::config_runtime::{
    ConfigSnapshot, ConfigUpdateResponse, OrchestratorRuntimeSummary, RuntimeConfigPatch,
    RuntimeOwnerStatus,
};
use crate::error::AppError;
use crate::models::transfer::{
    LocalTransferOpenTarget, TransferDirection, TransferOpenAction, TransferOperationStatus,
    TransferStatus, TransferTask, TransferTaskDto,
};
use crate::net::error_response::{P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::require_loopback_peer;
use crate::net::request_context::P2pRequestContext;
use crate::orchestrator::experiments::{CreateExperimentRequest, OrchestratorExperimentDto};
use crate::orchestrator::models::{OperationalNotificationSnapshot, OrchestratorTaskDto};
use crate::orchestrator::notifications::capture_operational_notification_snapshot;
use crate::orchestrator::workflow::WorkflowDocument;
use crate::state::AppState;
use crate::storage::RecoveryJobRow;
use crate::transfer::sender;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// 启动摘要每段独立结果（成功 value 或错误 message）。
///
/// Business Logic（为什么需要这个结构）:
///     Workbench Continue Working 表面需要 projects/sessions/tasks/transfers/devices 五段摘要；
///     任一段失败不得拖垮整响应，前端可按段展示降级。
///
/// Code Logic（这个结构做什么）:
///     serde `tag = "kind"`：`{kind:"ready",value}` / `{kind:"error",message}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SectionOutcome<T> {
    #[serde(rename = "ready")]
    Ready { value: T },
    #[serde(rename = "error")]
    Error { message: String },
}

impl<T> SectionOutcome<T> {
    /// Business Logic（为什么需要这个函数）:
    ///     section 查询成功/失败需统一映射为 Ready/Error，避免 handler 重复 match。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `Ok(v)→Ready`；`Err(e)→Error{message:e.to_string()}`。
    pub fn from_result(result: Result<T, AppError>) -> Self {
        match result {
            Ok(value) => SectionOutcome::Ready { value },
            Err(err) => SectionOutcome::Error {
                message: err.to_string(),
            },
        }
    }
}

/// Workbench 启动摘要总 DTO（五段独立 section + generatedAt）。
///
/// Business Logic（为什么需要这个结构）:
///     Continue Working 入口一次读出有界近期上下文，避免前端扇出多路 invoke。
///
/// Code Logic（这个结构做什么）:
///     camelCase wire；每段 `SectionOutcome`；`generated_at` 为 RFC3339。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchSummaryDto {
    pub projects: SectionOutcome<Vec<WorkbenchLaunchProjectDto>>,
    pub sessions: SectionOutcome<Vec<WorkbenchLaunchSessionDto>>,
    pub tasks: SectionOutcome<Vec<WorkbenchLaunchTaskDto>>,
    pub transfers: SectionOutcome<Vec<WorkbenchLaunchTransferDto>>,
    pub devices: SectionOutcome<Vec<WorkbenchLaunchDeviceDto>>,
    pub generated_at: String,
}

/// 启动摘要项目段条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchProjectDto {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub device_id: String,
    pub device_name: String,
    pub path: String,
    pub last_opened_at: String,
}

/// 启动摘要活跃会话段条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchSessionDto {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub worktree_id: Option<String>,
    pub name: String,
    pub status: String,
    pub started_at: String,
}

/// 启动摘要编排任务段条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchTaskDto {
    pub id: String,
    pub project_id: String,
    pub project_name: Option<String>,
    pub title: String,
    pub status: String,
    pub workflow_state: String,
    pub run_state: String,
    pub updated_at: String,
}

/// 启动摘要传输段条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchTransferDto {
    pub id: String,
    pub filename: String,
    pub status: String,
    pub direction: String,
    pub progress: Option<f64>,
    pub size: Option<u64>,
    pub updated_at: String,
}

/// 启动摘要设备段条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchLaunchDeviceDto {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub last_seen: Option<String>,
    pub address: Option<String>,
}

/// 启动摘要每段最大条数。
pub const WORKBENCH_LAUNCH_SECTION_LIMIT: i64 = 5;

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

/// 返回 Workbench Continue Working 启动摘要（五段独立 section）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 启动表面需要 sidecar 权威的最近项目/会话/任务/传输/设备；不得读 GUI 本地空库。
///
/// Code Logic（这个函数做什么）:
///     authorize 优先 → `build_workbench_launch_summary_for_state` → 1 MiB 上限。
pub async fn control_workbench_launch_summary(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAuthRequest>,
) -> P2pResult<Json<WorkbenchLaunchSummaryDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let body = build_workbench_launch_summary_for_state(&state).await;
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// 从 AppState 组装 Workbench 启动摘要（供 control handler 与 HeadlessOwner Tauri 命令复用）。
///
/// Business Logic（为什么需要这个函数）:
///     owner 进程内可直接读库；五段互不依赖，单段失败仍返回其它段。
///
/// Code Logic（这个函数做什么）:
///     `tokio::join!` 并发五段；每段 Result→SectionOutcome；`generated_at=Utc::now().to_rfc3339()`。
pub async fn build_workbench_launch_summary_for_state(
    state: &AppState,
) -> WorkbenchLaunchSummaryDto {
    let (projects, sessions, tasks, transfers, devices) = tokio::join!(
        load_launch_projects_section(state),
        load_launch_sessions_section(state),
        load_launch_tasks_section(state),
        load_launch_transfers_section(state),
        async { load_launch_devices_section(state) },
    );
    WorkbenchLaunchSummaryDto {
        projects,
        sessions,
        tasks,
        transfers,
        devices,
        generated_at: Utc::now().to_rfc3339(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     启动摘要项目区只展示最近打开项目。
///
/// Code Logic（这个函数做什么）:
///     `workbench_project_repo.list_recent(5)` → LaunchProjectDto。
async fn load_launch_projects_section(
    state: &AppState,
) -> SectionOutcome<Vec<WorkbenchLaunchProjectDto>> {
    SectionOutcome::from_result(load_launch_projects(state).await)
}

/// Business Logic（为什么需要这个函数）:
///     项目段查询失败时需捕获为 error message，而非 panic。
///
/// Code Logic（这个函数做什么）:
///     list_recent → map 字段。
async fn load_launch_projects(
    state: &AppState,
) -> Result<Vec<WorkbenchLaunchProjectDto>, AppError> {
    let rows = state
        .workbench_project_repo
        .list_recent(WORKBENCH_LAUNCH_SECTION_LIMIT)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| WorkbenchLaunchProjectDto {
            id: row.id,
            name: row.name,
            kind: row.kind,
            device_id: row.device_id,
            device_name: row.device_name,
            path: row.path,
            last_opened_at: row.last_opened_at,
        })
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     启动摘要会话区展示最近活跃 workbench 终端（非 claude_sessions）。
///
/// Code Logic（这个函数做什么）:
///     list_recent_active + 一次 projects list 建 id→name 映射（无 N+1）。
async fn load_launch_sessions_section(
    state: &AppState,
) -> SectionOutcome<Vec<WorkbenchLaunchSessionDto>> {
    SectionOutcome::from_result(load_launch_sessions(state).await)
}

/// Business Logic（为什么需要这个函数）:
///     会话段独立失败路径。
///
/// Code Logic（这个函数做什么）:
///     active sessions + project name map。
async fn load_launch_sessions(
    state: &AppState,
) -> Result<Vec<WorkbenchLaunchSessionDto>, AppError> {
    let sessions = state
        .workbench_session_repo
        .list_recent_active(WORKBENCH_LAUNCH_SECTION_LIMIT)
        .await?;
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    // 一次加载项目表建映射；项目数量通常远小于会话 N+1 查询代价。
    let projects = state.workbench_project_repo.list().await?;
    let name_by_id: HashMap<String, String> =
        projects.into_iter().map(|p| (p.id, p.name)).collect();
    Ok(sessions
        .into_iter()
        .map(|s| {
            let project_name = name_by_id
                .get(&s.project_id)
                .cloned()
                .unwrap_or_else(|| s.project_id.clone());
            WorkbenchLaunchSessionDto {
                id: s.id,
                project_id: s.project_id,
                project_name,
                worktree_id: s.worktree_id,
                name: s.name,
                status: s.status,
                started_at: s.started_at,
            }
        })
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     启动摘要任务区展示全局值得关注的编排任务。
///
/// Code Logic（这个函数做什么）:
///     `list_launch_tasks(5)` + 一次 projects list 映射 projectName。
async fn load_launch_tasks_section(
    state: &AppState,
) -> SectionOutcome<Vec<WorkbenchLaunchTaskDto>> {
    SectionOutcome::from_result(load_launch_tasks(state).await)
}

/// Business Logic（为什么需要这个函数）:
///     任务段独立失败路径。
///
/// Code Logic（这个函数做什么）:
///     list_launch_tasks → DTO + project name map。
async fn load_launch_tasks(state: &AppState) -> Result<Vec<WorkbenchLaunchTaskDto>, AppError> {
    let tasks = state
        .orchestrator_repo
        .list_launch_tasks(WORKBENCH_LAUNCH_SECTION_LIMIT)
        .await?;
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let projects = state.workbench_project_repo.list().await?;
    let name_by_id: HashMap<String, String> =
        projects.into_iter().map(|p| (p.id, p.name)).collect();
    Ok(tasks
        .into_iter()
        .map(|t| WorkbenchLaunchTaskDto {
            id: t.id,
            project_name: name_by_id.get(&t.project_id).cloned(),
            project_id: t.project_id,
            title: t.title,
            status: t.status.as_str().to_string(),
            workflow_state: t.workflow_state.as_str().to_string(),
            run_state: t.run_state.as_str().to_string(),
            updated_at: t.updated_at,
        })
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     启动摘要传输区合并活跃 registry 与最近 failed 历史。
///
/// Code Logic（这个函数做什么）:
///     active list + history list → 过滤 pending/transferring/failed → 按时间倒序 max 5。
async fn load_launch_transfers_section(
    state: &AppState,
) -> SectionOutcome<Vec<WorkbenchLaunchTransferDto>> {
    SectionOutcome::from_result(load_launch_transfers(state).await)
}

/// Business Logic（为什么需要这个函数）:
///     传输段独立失败路径。
///
/// Code Logic（这个函数做什么）:
///     合并 active + history failed，去重后有界排序。
async fn load_launch_transfers(
    state: &AppState,
) -> Result<Vec<WorkbenchLaunchTransferDto>, AppError> {
    let mut by_id: HashMap<String, TransferTask> = HashMap::new();
    for task in state.transfers.list() {
        if matches!(
            task.status,
            TransferStatus::Pending | TransferStatus::Transferring | TransferStatus::Failed
        ) {
            by_id.insert(task.id.clone(), task);
        }
    }
    let history = state.transfer_repo.list().await?;
    for task in history {
        if matches!(
            task.status,
            TransferStatus::Pending | TransferStatus::Transferring | TransferStatus::Failed
        ) {
            by_id.entry(task.id.clone()).or_insert(task);
        }
    }
    let mut items: Vec<TransferTask> = by_id.into_values().collect();
    items.sort_by(|a, b| {
        let a_ts = a.completed_at.as_deref().unwrap_or(a.created_at.as_str());
        let b_ts = b.completed_at.as_deref().unwrap_or(b.created_at.as_str());
        b_ts.cmp(a_ts).then_with(|| b.id.cmp(&a.id))
    });
    items.truncate(WORKBENCH_LAUNCH_SECTION_LIMIT as usize);
    Ok(items
        .into_iter()
        .map(|t| {
            let progress = t.progress();
            let status = match t.status {
                TransferStatus::Pending => "pending",
                TransferStatus::Transferring => "transferring",
                TransferStatus::Completed => "completed",
                TransferStatus::Failed => "failed",
                TransferStatus::Cancelled => "cancelled",
            };
            let direction = match t.direction {
                TransferDirection::Send => "send",
                TransferDirection::Receive => "receive",
            };
            let updated_at = t
                .completed_at
                .clone()
                .unwrap_or_else(|| t.created_at.clone());
            WorkbenchLaunchTransferDto {
                id: t.id,
                filename: t.filename,
                status: status.to_string(),
                direction: direction.to_string(),
                progress: Some(progress),
                size: Some(t.size),
                updated_at,
            }
        })
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     启动摘要设备区展示在线优先的最近设备。
///
/// Code Logic（这个函数做什么）:
///     读 devices RwLock → online 优先 + last_seen DESC → max 5；无网络 I/O。
fn load_launch_devices_section(state: &AppState) -> SectionOutcome<Vec<WorkbenchLaunchDeviceDto>> {
    SectionOutcome::from_result(load_launch_devices(state))
}

/// Business Logic（为什么需要这个函数）:
///     设备段独立失败路径（锁中毒等）。
///
/// Code Logic（这个函数做什么）:
///     排序截断后 map 到 LaunchDeviceDto（address=host:port）。
fn load_launch_devices(state: &AppState) -> Result<Vec<WorkbenchLaunchDeviceDto>, AppError> {
    let devices = state
        .devices
        .read()
        .map_err(|_| AppError::generic("devices 读锁中毒"))?
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut devices = devices;
    devices.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.id.cmp(&b.id))
    });
    devices.truncate(WORKBENCH_LAUNCH_SECTION_LIMIT as usize);
    Ok(devices
        .into_iter()
        .map(|d| WorkbenchLaunchDeviceDto {
            id: d.id,
            name: d.name,
            online: d.online,
            last_seen: Some(d.last_seen.to_rfc3339()),
            address: Some(format!("{}:{}", d.host, d.port)),
        })
        .collect())
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

/// 运营通知 snapshot 请求（仅 controlToken）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI handshake 经 loopback+token 拉取当前 opaque baseline，无业务入参。
///
/// Code Logic（这个结构做什么）:
///     camelCase `controlToken`。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOperationalNotificationSnapshotRequest {
    pub control_token: String,
}

/// 返回运营通知 baseline snapshot（稳定 asOfCursor）。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 冷启动/Gap 需要 owner 当前 HumanReview/Blocked/Done/outboxFailed opaque 状态 + 事件游标，
///     建立 no-notify baseline；控制面 only，不进 LAN capabilities。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → `capture_operational_notification_snapshot`（cursor 稳定窗口）。
pub async fn control_operational_notification_snapshot(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOperationalNotificationSnapshotRequest>,
) -> P2pResult<Json<OperationalNotificationSnapshot>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let snapshot = capture_operational_notification_snapshot(&state)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.operational_notification_snapshot")
        })?;
    ensure_response_within_limit(&snapshot, &context)?;
    Ok(Json(snapshot))
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
pub(crate) fn authorize_control_request(
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

/// 接受 GUI Rust 到 sidecar owner 的终端输入 WebSocket。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 GUI 的本地 IPC 只能等待有界队列接纳，实际输入经一条 loopback 常驻连接送到唯一 owner。
///
/// Code Logic（这个函数做什么）:
///     upgrade 前严格执行 loopback→header token 鉴权，再协商 v1 子协议并启动 remote-aware 网关。
pub async fn control_terminal_input_stream(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
) -> Result<Response, P2pError> {
    let token = headers
        .get(crate::workbench::terminal_input::CONTROL_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    authorize_control_request(peer, &context, token)?;
    state.runtime_role.require_owner().map_err(|error| {
        P2pError::from_app_error(error, &context, "control.terminal_input_stream")
    })?;
    Ok(ws
        .protocols([crate::workbench::terminal_input::TERMINAL_INPUT_SUBPROTOCOL])
        .on_upgrade(move |socket| {
            crate::workbench::terminal_input::serve_terminal_input_socket(
                socket,
                state,
                crate::workbench::terminal_input::TerminalInputGatewayMode::RemoteAware,
            )
        }))
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

// ── transfer lifecycle control（N5 Open/Reveal prepare + owner mutations）──

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

/// transfer send 请求体。
///
/// Business Logic: GuiClient 不得在本进程 claim/spawn；必须代理到 owner registry。
///     clientOperationId 保证 lost ACK 后同意图幂等，禁止盲双发。
/// Code Logic: camelCase controlToken + deviceId + filePath + clientOperationId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferSendRequest {
    pub control_token: String,
    pub device_id: String,
    pub file_path: String,
    pub client_operation_id: String,
}

/// transfer retry/resume 请求体。
///
/// Business Logic: recovery 幂等 claim 必须在 owner 单进程 registry 上执行。
/// Code Logic: camelCase controlToken + taskId + clientOperationId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferRecoveryRequest {
    pub control_token: String,
    pub task_id: String,
    pub client_operation_id: String,
}

/// transfer get-operation 请求体。
///
/// Business Logic: operation ledger 对账在 owner 上（registry + lost-ACK）。
/// Code Logic: camelCase controlToken + clientOperationId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferGetOperationRequest {
    pub control_token: String,
    pub client_operation_id: String,
}

/// transfer cancel 请求体。
///
/// Business Logic: CancellationToken 只存在于 owner TransferRegistry。
/// Code Logic: camelCase controlToken + taskId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlTransferCancelRequest {
    pub control_token: String,
    pub task_id: String,
}

/// owner 路径：发起发送（claim 后 spawn，立即返回 accepted）。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 是唯一 runtime owner；GUI 代理 send 避免双 registry 双 drive。
///     clientOperationId claim 防止 lost ACK 导致重复发送。
///
/// Code Logic（这个函数做什么）:
///     authorize → require_owner → sender::start_sending → `{accepted,deviceId,filePath,id}`。
pub async fn control_transfer_send(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferSendRequest>,
) -> P2pResult<Json<serde_json::Value>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_send"))?;
    let transfer_id = sender::start_sending(
        state.clone(),
        request.device_id.clone(),
        request.file_path.clone(),
        request.client_operation_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_send"))?;
    let body = serde_json::json!({
        "accepted": true,
        "deviceId": request.device_id,
        "filePath": request.file_path,
        "id": transfer_id,
    });
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// owner 路径：幂等 retry。
///
/// Business Logic: claim+spawn 必须在 owner 上，与 recover_pending 同进程。
/// Code Logic: authorize → require_owner → sender::retry_transfer → TransferTaskDto。
pub async fn control_transfer_retry(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferRecoveryRequest>,
) -> P2pResult<Json<TransferTaskDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_retry"))?;
    let task = sender::retry_transfer(state.clone(), request.task_id, request.client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_retry"))?;
    let dto = task.to_dto(None);
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// owner 路径：幂等 resume。
///
/// Business Logic: resume claim 与 peer capability 探测在 owner 执行。
/// Code Logic: authorize → require_owner → sender::resume_transfer → TransferTaskDto。
pub async fn control_transfer_resume(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferRecoveryRequest>,
) -> P2pResult<Json<TransferTaskDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_resume"))?;
    let task = sender::resume_transfer(state.clone(), request.task_id, request.client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_resume"))?;
    let dto = task.to_dto(None);
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// owner 路径：按 clientOperationId 查 operation 真值。
///
/// Business Logic: lost-ACK 对账与 registry 优先读取必须在 owner。
/// Code Logic: authorize → require_owner → sender::get_transfer_operation。
pub async fn control_transfer_get_operation(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferGetOperationRequest>,
) -> P2pResult<Json<TransferOperationStatus>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_get_operation"))?;
    let status = sender::get_transfer_operation(&state, &request.client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_get_operation"))?;
    ensure_response_within_limit(&status, &context)?;
    Ok(Json(status))
}

/// owner 路径：取消活跃传输。
///
/// Business Logic: cancel token 只在 owner registry；GUI 不得 no-op 假取消。
/// Code Logic: authorize → require_owner → transfers.cancel → `{ok,id}`。
pub async fn control_transfer_cancel(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlTransferCancelRequest>,
) -> P2pResult<Json<serde_json::Value>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.transfer_cancel"))?;
    let ok = state.transfers.cancel(&request.task_id);
    if !ok {
        return Err(P2pError::from_app_error(
            AppError::not_found(format!("传输任务不存在: {}", request.task_id)),
            &context,
            "control.transfer_cancel",
        ));
    }
    let body = serde_json::json!({ "ok": true, "id": request.task_id });
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// Orchestrator deliver-reviewed control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient 交付必须把 projectId/taskId 交给 owner，由 sidecar 执行 Settings gate 与 Git delivery。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + projectId + taskId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorDeliverReviewedRequest {
    pub control_token: String,
    pub project_id: String,
    pub task_id: String,
}

/// owner 路径：交付人工复核任务（完整 delivery pipeline）。
///
/// Business Logic（为什么需要这个函数）:
///     Git commit/push/merge、delivery lock 与 Settings gate 只能在 HeadlessOwner 进程执行；
///     GuiClient 不得自跑 pipeline。A0 后无人工 digest 门禁。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `deliver_reviewed_orchestrator_task_view_for_state`
///     → OrchestratorTaskViewDto。
pub async fn control_orchestrator_deliver_reviewed(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorDeliverReviewedRequest>,
) -> P2pResult<Json<OrchestratorTaskViewDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_deliver_reviewed")
    })?;
    let view = deliver_reviewed_orchestrator_task_view_for_state(
        &state,
        request.project_id.trim(),
        request.task_id.trim(),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_deliver_reviewed"))?;
    ensure_response_within_limit(&view, &context)?;
    Ok(Json(view))
}

/// complete-agent-run control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient 手动完成必须把 taskId 交给 owner，由 sidecar 跑验证/delivery pipeline。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + taskId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorCompleteAgentRunRequest {
    pub control_token: String,
    pub task_id: String,
}

/// owner 路径：完成 Agent 运行并执行验证/交付 pipeline。
///
/// Business Logic（为什么需要这个函数）:
///     Running→Verifying、验证命令、Claude verifier、delivery lock 只能在 HeadlessOwner 执行；
///     GuiClient 不得自跑 pipeline 或写空库状态。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `complete_orchestrator_agent_run_for_state`
///     → OrchestratorTaskDto。
pub async fn control_orchestrator_complete_agent_run(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorCompleteAgentRunRequest>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_complete_agent_run")
    })?;
    let task = complete_orchestrator_agent_run_for_state(&state, request.task_id.trim())
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_complete_agent_run")
        })?;
    ensure_response_within_limit(&task, &context)?;
    Ok(Json(task))
}

/// abort-task control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient Abort 必须在 owner 上检查 delivery 租约并 CAS 状态，禁止本机空库误成功。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + taskId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorAbortTaskRequest {
    pub control_token: String,
    pub task_id: String,
}

/// owner 路径：终止任务（保留 Done）。
///
/// Business Logic（为什么需要这个函数）:
///     abort 与 delivery 共享 owner DB 租约；GuiClient 必须代理到 sidecar。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `abort_task_preserving_done`（含 lease 检查）
///     → 可选 experiment candidate sync → OrchestratorTaskDto。
pub async fn control_orchestrator_abort_task(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorAbortTaskRequest>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_abort_task"))?;
    let task_id = request.task_id.trim();
    let updated = state
        .orchestrator_repo
        .abort_task_preserving_done(task_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_abort_task"))?;
    if updated.experiment_id.is_some() {
        if let Err(err) =
            crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                state.orchestrator_repo.as_ref(),
                &updated.id,
                updated.status,
            )
            .await
        {
            tracing::debug!(task_id = %updated.id, "sync_candidate after control abort: {err}");
        }
    }
    let dto = OrchestratorTaskDto::from(updated);
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// cancel-task control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient cancel 必须在 owner 上检查 delivery 租约。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + taskId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorCancelTaskRequest {
    pub control_token: String,
    pub task_id: String,
}

/// owner 路径：取消任务。
///
/// Business Logic（为什么需要这个函数）:
///     cancel 与 delivery 共享 owner DB 租约；GuiClient 必须代理到 sidecar。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `cancel_task` → OrchestratorTaskDto。
pub async fn control_orchestrator_cancel_task(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorCancelTaskRequest>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_cancel_task"))?;
    let updated = state
        .orchestrator_repo
        .cancel_task(request.task_id.trim())
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_cancel_task"))?;
    let dto = OrchestratorTaskDto::from(updated);
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// dispatch-once control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient 手动 tick 调度必须在 owner 进程领取队列。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorDispatchOnceRequest {
    pub control_token: String,
}

/// owner 路径：触发一次 Orchestrator 调度。
///
/// Business Logic（为什么需要这个函数）:
///     scheduler 队列/PTY spawn 权威在 HeadlessOwner；GuiClient 不得本进程 dispatch。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `scheduler::dispatch_once` → `{dispatched}`。
pub async fn control_orchestrator_dispatch_once(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorDispatchOnceRequest>,
) -> P2pResult<Json<serde_json::Value>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state
        .runtime_role
        .require_owner()
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_dispatch_once"))?;
    let dispatched = crate::orchestrator::scheduler::dispatch_once(&state)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_dispatch_once"))?;
    let body = serde_json::json!({ "dispatched": dispatched });
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// workflow-document/get control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     向导必须读 sidecar 上的项目根 WORKFLOW.md，而非 GuiClient 本地空路径。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + projectId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkflowDocumentGetRequest {
    pub control_token: String,
    pub project_id: String,
}

/// owner 路径：读取 WORKFLOW 文档状态。
///
/// Business Logic（为什么需要这个函数）:
///     WORKFLOW.md 权威在项目所在 owner 设备；GuiClient 只代理。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `get_workflow_document_for_state`。
pub async fn control_orchestrator_workflow_document_get(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlWorkflowDocumentGetRequest>,
) -> P2pResult<Json<WorkflowDocument>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_workflow_document_get")
    })?;
    let doc = get_workflow_document_for_state(&state, request.project_id.trim())
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_workflow_document_get")
        })?;
    ensure_response_within_limit(&doc, &context)?;
    Ok(Json(doc))
}

/// workflow-document/validate control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     保存前权威 parser 必须与 owner save 路径一致。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + projectId + content。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkflowDocumentValidateRequest {
    pub control_token: String,
    pub project_id: String,
    pub content: String,
}

/// owner 路径：权威校验 WORKFLOW 内容。
///
/// Business Logic（为什么需要这个函数）:
///     前端 YAML 提示不得当最终结果；validate 与 save 同进程。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `validate_workflow_document_for_state`。
pub async fn control_orchestrator_workflow_document_validate(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlWorkflowDocumentValidateRequest>,
) -> P2pResult<Json<WorkflowDocument>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(
            e,
            &context,
            "control.orchestrator_workflow_document_validate",
        )
    })?;
    let doc =
        validate_workflow_document_for_state(&state, request.project_id.trim(), &request.content)
            .await
            .map_err(|e| {
                P2pError::from_app_error(
                    e,
                    &context,
                    "control.orchestrator_workflow_document_validate",
                )
            })?;
    ensure_response_within_limit(&doc, &context)?;
    Ok(Json(doc))
}

/// workflow-document/save control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     CAS save 的 expectedHash + content 必须在 owner 排他写盘。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + projectId + expectedHash + content。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkflowDocumentSaveRequest {
    pub control_token: String,
    pub project_id: String,
    pub expected_hash: String,
    pub content: String,
}

/// owner 路径：CAS 保存 WORKFLOW 文档。
///
/// Business Logic（为什么需要这个函数）:
///     排他 create 与 expected-hash CAS 只能在 owner 执行；成功后不 dispatch。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `save_workflow_document_for_state`。
pub async fn control_orchestrator_workflow_document_save(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlWorkflowDocumentSaveRequest>,
) -> P2pResult<Json<WorkflowDocument>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_workflow_document_save")
    })?;
    let doc = save_workflow_document_for_state(
        &state,
        request.project_id.trim(),
        &request.expected_hash,
        &request.content,
    )
    .await
    .map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_workflow_document_save")
    })?;
    ensure_response_within_limit(&doc, &context)?;
    Ok(Json(doc))
}

/// experiment create control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient 不得在本进程写 experiment 仓储或 dispatch candidate；必须交给 sidecar owner。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + 扁平化 `CreateExperimentRequest` 字段。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentCreateRequest {
    pub control_token: String,
    #[serde(flatten)]
    pub request: CreateExperimentRequest,
}

/// owner 路径：创建实验组。
///
/// Business Logic（为什么需要这个函数）:
///     实验组创建与 candidate dispatch 只能在 HeadlessOwner 执行；GuiClient 空库创建会漂移。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `create_orchestrator_experiment_for_state` → DTO。
pub async fn control_orchestrator_experiment_create(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentCreateRequest>,
) -> P2pResult<Json<OrchestratorExperimentDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_experiment_create")
    })?;
    let outcome = create_orchestrator_experiment_for_state(&state, request.request)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_experiment_create")
        })?;
    ensure_response_within_limit(&outcome.experiment, &context)?;
    Ok(Json(outcome.experiment))
}

/// experiment list control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     GuiClient 本地 DB 无 owner 实验行；列表必须读 sidecar 权威仓储。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + 可选 projectId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentListRequest {
    pub control_token: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

/// owner 路径：列出实验组。
///
/// Business Logic（为什么需要这个函数）:
///     桌面看板不得用 GuiClient 空库冒充 owner 列表。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `list_orchestrator_experiments_for_state`。
pub async fn control_orchestrator_experiment_list(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentListRequest>,
) -> P2pResult<Json<Vec<OrchestratorExperimentDto>>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_experiment_list")
    })?;
    let experiments = list_orchestrator_experiments_for_state(
        &state,
        request
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &context, "control.orchestrator_experiment_list"))?;
    ensure_response_within_limit(&experiments, &context)?;
    Ok(Json(experiments))
}

/// experiment get control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     详情必须读 owner 仓储（含 candidates），禁止 GuiClient 空库 NotFound 误导 UI。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + experimentId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentGetRequest {
    pub control_token: String,
    pub experiment_id: String,
}

/// owner 路径：读取实验组详情。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 不得对本机空库 get 后假装实验不存在。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `get_orchestrator_experiment_for_state`。
pub async fn control_orchestrator_experiment_get(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentGetRequest>,
) -> P2pResult<Json<OrchestratorExperimentDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_experiment_get")
    })?;
    let dto = get_orchestrator_experiment_for_state(&state, request.experiment_id.trim())
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_experiment_get")
        })?;
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// experiment approve-winner control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     批准 winner 可能触发 full-auto Git delivery；必须只在 owner 持有 delivery lock。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + experimentId + winnerTaskId + 可选 reason。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentApproveRequest {
    pub control_token: String,
    pub experiment_id: String,
    pub winner_task_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// owner 路径：批准/采用实验 winner（可进入 delivery）。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 与 sidecar 双路径 approve 会造成双重 commit/push/merge；仅 owner 可交付。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `approve_orchestrator_experiment_winner_for_state`。
pub async fn control_orchestrator_experiment_approve_winner(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentApproveRequest>,
) -> P2pResult<Json<OrchestratorExperimentDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(
            e,
            &context,
            "control.orchestrator_experiment_approve_winner",
        )
    })?;
    let dto = approve_orchestrator_experiment_winner_for_state(
        &state,
        request.experiment_id.trim(),
        request.winner_task_id.trim(),
        request.reason.as_deref(),
    )
    .await
    .map_err(|e| {
        P2pError::from_app_error(
            e,
            &context,
            "control.orchestrator_experiment_approve_winner",
        )
    })?;
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// experiment cancel control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     取消整组必须落到 owner 仓储与 child task abort，GuiClient 本地取消无效且危险。
///
/// Code Logic（这个结构做什么）:
///     camelCase：controlToken + experimentId。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentCancelRequest {
    pub control_token: String,
    pub experiment_id: String,
}

/// owner 路径：取消实验组。
///
/// Business Logic（为什么需要这个函数）:
///     组 CAS + candidate abort 只能在 owner 执行。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `cancel_orchestrator_experiment_for_state`。
pub async fn control_orchestrator_experiment_cancel(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentCancelRequest>,
) -> P2pResult<Json<OrchestratorExperimentDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_experiment_cancel")
    })?;
    let dto = cancel_orchestrator_experiment_for_state(&state, request.experiment_id.trim())
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_experiment_cancel")
        })?;
    ensure_response_within_limit(&dto, &context)?;
    Ok(Json(dto))
}

/// experiment prepare-downgrade control 请求体。
///
/// Business Logic（为什么需要这个结构）:
///     降级 quiesce 会 cancel 全部非终态组；必须只在 owner 仓储执行。
///
/// Code Logic（这个结构做什么）:
///     camelCase：仅 controlToken。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorExperimentPrepareDowngradeRequest {
    pub control_token: String,
}

/// owner 路径：关闭 experiments 能力前 quiesce 非终态组。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 本地扫库 cancel 会与 sidecar 双路径漂移；降级准备只能 owner 执行。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → `prepare_experiment_downgrade_for_state` → cancelled 计数。
pub async fn control_orchestrator_experiment_prepare_downgrade(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorExperimentPrepareDowngradeRequest>,
) -> P2pResult<Json<u32>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(
            e,
            &context,
            "control.orchestrator_experiment_prepare_downgrade",
        )
    })?;
    let cancelled = prepare_experiment_downgrade_for_state(&state)
        .await
        .map_err(|e| {
            P2pError::from_app_error(
                e,
                &context,
                "control.orchestrator_experiment_prepare_downgrade",
            )
        })?;
    Ok(Json(cancelled))
}

/// 任务块 create control 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorTaskBlockCreateRequest {
    pub control_token: String,
    #[serde(flatten)]
    pub request: CreateOrchestratorTaskBlockRequest,
}

/// owner 路径：创建串行任务块。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 不得写本机空库；建块权威只在 sidecar。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → create_orchestrator_task_block_view_for_state。
pub async fn control_orchestrator_task_block_create(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorTaskBlockCreateRequest>,
) -> P2pResult<Json<OrchestratorTaskBlockViewCreatedDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_task_block_create")
    })?;
    let outcome = create_orchestrator_task_block_view_for_state(&state, request.request)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_task_block_create")
        })?;
    ensure_response_within_limit(&outcome, &context)?;
    Ok(Json(outcome))
}

/// 任务块 append control 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorTaskBlockAppendRequest {
    pub control_token: String,
    #[serde(flatten)]
    pub request: AppendOrchestratorTaskBlockMemberRequest,
}

/// owner 路径：追加任务块成员。
///
/// Business Logic（为什么需要这个函数）:
///     追加会改变 live last-member；必须在 owner 上校验 head/上限。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → append for_state。
pub async fn control_orchestrator_task_block_append_member(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorTaskBlockAppendRequest>,
) -> P2pResult<Json<OrchestratorTaskViewDto>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_task_block_append")
    })?;
    let outcome = append_orchestrator_task_block_member_view_for_state(&state, request.request)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_task_block_append")
        })?;
    ensure_response_within_limit(&outcome, &context)?;
    Ok(Json(outcome))
}

/// 任务块 reorder control 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlOrchestratorTaskBlockReorderRequest {
    pub control_token: String,
    #[serde(flatten)]
    pub request: ReorderOrchestratorTaskBlockMembersRequest,
}

/// owner 路径：重排任务块成员。
///
/// Business Logic（为什么需要这个函数）:
///     重排只能在 owner 上校验全部成员仍 backlog/todo 且 idle。
///
/// Code Logic（这个函数做什么）:
///     loopback → token → require_owner → reorder for_state。
pub async fn control_orchestrator_task_block_reorder_members(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlOrchestratorTaskBlockReorderRequest>,
) -> P2pResult<Json<Vec<OrchestratorTaskViewDto>>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    state.runtime_role.require_owner().map_err(|e| {
        P2pError::from_app_error(e, &context, "control.orchestrator_task_block_reorder")
    })?;
    let outcome = reorder_orchestrator_task_block_members_view_for_state(&state, request.request)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &context, "control.orchestrator_task_block_reorder")
        })?;
    ensure_response_within_limit(&outcome, &context)?;
    Ok(Json(outcome))
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
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
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
            agent_hub_api_version: crate::backend::control::AGENT_HUB_API_VERSION,
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

    /// Business Logic（为什么需要这个测试）:
    ///     workbench_launch_summary 鉴权必须先于任何 repo 访问；错误 token 不得进入构建路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     authorize_control_for_test + wrong token → Unauthorized。
    #[test]
    fn workbench_launch_summary_auth_fails_before_repo() {
        let ctx = test_ctx();
        let control = control_file("expected-token");
        let err = authorize_control_for_test(loopback_peer(), &ctx, "wrong-token", Some(&control))
            .expect_err("wrong token");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.envelope().code, "unauthorized");
        let err = authorize_control_for_test(lan_peer(), &ctx, "expected-token", Some(&control))
            .expect_err("non-loopback");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     任一段 Ready/Error 必须独立序列化为 kind/value/message，互不折叠。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造混有 Ready/Error 的 summary 并 round-trip JSON 断言 wire shape。
    #[test]
    fn workbench_launch_summary_independent_section_outcomes() {
        let summary = WorkbenchLaunchSummaryDto {
            projects: SectionOutcome::Ready {
                value: vec![WorkbenchLaunchProjectDto {
                    id: "p1".into(),
                    name: "P".into(),
                    kind: "local".into(),
                    device_id: "d".into(),
                    device_name: "Desk".into(),
                    path: "/tmp/p".into(),
                    last_opened_at: "2026-07-15T00:00:00Z".into(),
                }],
            },
            sessions: SectionOutcome::Error {
                message: "session boom".into(),
            },
            tasks: SectionOutcome::Ready { value: vec![] },
            transfers: SectionOutcome::Error {
                message: "transfer boom".into(),
            },
            devices: SectionOutcome::Ready { value: vec![] },
            generated_at: "2026-07-15T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&summary).expect("serialize");
        assert_eq!(json["projects"]["kind"], "ready");
        assert_eq!(json["projects"]["value"][0]["id"], "p1");
        assert_eq!(json["sessions"]["kind"], "error");
        assert_eq!(json["sessions"]["message"], "session boom");
        assert_eq!(json["tasks"]["kind"], "ready");
        assert_eq!(json["transfers"]["kind"], "error");
        assert!(json["sessions"].get("value").is_none());
        // 其它段仍 ready，证明独立 outcome 不被折叠
        assert_eq!(json["devices"]["kind"], "ready");

        let from_err = SectionOutcome::<Vec<String>>::from_result(Err(AppError::generic(
            "unit section failure",
        )));
        match from_err {
            SectionOutcome::Error { message } => {
                assert!(message.contains("unit section failure"));
            }
            SectionOutcome::Ready { .. } => panic!("expected error outcome"),
        }
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

    /// Orchestrator deliver/workflow control body 必须 camelCase 对齐。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GuiClient control client 与 owner handler 共用 contract；字段名漂移会导致静默丢 hash。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化 deliver / workflow save 请求体并断言字段。
    #[test]
    fn orchestrator_control_request_bodies_deserialize_camel_case() {
        let deliver_raw = r#"{"controlToken":"tok","projectId":"p1","taskId":"t1"}"#;
        let deliver: ControlOrchestratorDeliverReviewedRequest =
            serde_json::from_str(deliver_raw).expect("deliver body");
        assert_eq!(deliver.control_token, "tok");
        assert_eq!(deliver.project_id, "p1");
        assert_eq!(deliver.task_id, "t1");

        let complete_raw = r#"{"controlToken":"tok","taskId":"t-run-1"}"#;
        let complete: ControlOrchestratorCompleteAgentRunRequest =
            serde_json::from_str(complete_raw).expect("complete-agent-run body");
        assert_eq!(complete.control_token, "tok");
        assert_eq!(complete.task_id, "t-run-1");

        let dispatch_raw = r#"{"controlToken":"tok"}"#;
        let dispatch: ControlOrchestratorDispatchOnceRequest =
            serde_json::from_str(dispatch_raw).expect("dispatch-once body");
        assert_eq!(dispatch.control_token, "tok");

        let save_raw =
            r#"{"controlToken":"tok","projectId":"p1","expectedHash":"h1","content":"---\n"}"#;
        let save: ControlWorkflowDocumentSaveRequest =
            serde_json::from_str(save_raw).expect("workflow save body");
        assert_eq!(save.expected_hash, "h1");
        assert_eq!(save.content, "---\n");
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
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = AppConfig {
            device_id: "dev-a".into(),
            device_name: "n".into(),
            http_port: 0,
            receive_dir: "/tmp/r".into(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: "/tmp/d.db".into(),
            screenshot_hotkey: "<cmd>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_optimizer_provider: "claude".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
            experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
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
