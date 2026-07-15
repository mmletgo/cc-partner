//! runtime snapshot + view queue/retry/abort
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskStatus;
use crate::orchestrator::outbox::{create_pending_remote_task, is_remote_network_error};
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::state::AppState;
use tauri::State;

use super::common::{
    abort_orchestrator_task_target_status, create_local_task_for_client_request,
    create_remote_orchestrator_task_online, get_orchestrator_workbench_project, local_task_view,
    update_remote_orchestrator_task_status, CreateOrchestratorTaskRequest,
    OrchestratorRuntimeSnapshotDto, OrchestratorTaskViewDto,
};
use super::tasks::get_orchestrator_runtime_snapshot_for_state;

/// 获取 Orchestrator 项目运行时快照。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 自动化状态条需要一个轻量观测接口展示 scheduler、workflow 和槽位状态；
///     远端 shortcut 必须向 owning device 拉取权威快照，并映射为 live/offline/unsupported/unavailable。
///
/// Code Logic（这个函数做什么）:
///     委托 `get_orchestrator_runtime_snapshot_for_state`，供桌面 Tauri invoke 使用。
#[tauri::command]
pub async fn get_orchestrator_runtime_snapshot(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    get_orchestrator_runtime_snapshot_for_state(state.inner(), &project_id).await
}

/// 获取运营通知 baseline snapshot。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 OS 通知 coordinator 需要 owner 当前 opaque 状态 + asOfCursor 建立 no-notify baseline。
///
/// Code Logic（这个函数做什么）:
///     GuiClient 经 control client 代理；HeadlessOwner 本地 `capture_operational_notification_snapshot`。
#[tauri::command]
pub async fn get_operational_notification_snapshot(
    state: State<'_, AppState>,
) -> Result<crate::orchestrator::models::OperationalNotificationSnapshot, AppError> {
    use crate::backend::authority::RuntimeRole;
    use crate::backend::control_client::BackendControlClient;
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .operational_notification_snapshot()
            .await;
    }
    crate::orchestrator::notifications::capture_operational_notification_snapshot(state.inner())
        .await
}

/// 通过 HTTP task-view 协议创建 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 创建任务需要支持 Backlog/Todo/Start 三种创建动作，同时还要支持 remote shortcut 代理。
///
/// Code Logic（这个函数做什么）:
///     local 项目用 clientRequestId 幂等创建并按 createAction 决定初始状态；remote 项目保持同一 requestId 转发到 owning device，
///     网络失败时写 pending outbox 并返回 PendingRemote。
/// Business Logic（为什么需要这个函数）:
///     mobile create 入站 request_id 必须贯穿 open-project 与 owner create，否则 owner 侧看到重生 ID。
///
/// Code Logic（这个函数做什么）:
///     校验 clientRequestId；local 幂等创建；remote 把 `forwarded_request_id` 传给 open-project 与 owner create。
pub(crate) async fn create_orchestrator_task_view_for_http_with_request_id(
    state: &AppState,
    req: RemoteCreateOrchestratorTaskReq,
    forwarded_request_id: Option<&str>,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &req.project_id).await?;
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::generic("移动端创建任务缺少 clientRequestId"))?;

    if project.kind != "remote" {
        let row = create_local_task_for_client_request(
            state,
            &client_request_id,
            CreateOrchestratorTaskRequest {
                project_id: req.project_id,
                title: req.title,
                goal: req.goal,
                acceptance_criteria: req.acceptance_criteria,
                priority: Some(req.priority),
                create_action: req.create_action,
                source: req.source,
                external_id: req.external_id,
                external_identifier: req.external_identifier,
                external_url: req.external_url,
                external_state: req.external_state,
                external_labels: req.external_labels,
            },
        )
        .await?;
        return Ok(local_task_view(row));
    }

    let remote_request = RemoteCreateOrchestratorTaskReq {
        client_request_id: Some(client_request_id),
        ..req
    };
    match create_remote_orchestrator_task_online(
        state,
        &project,
        remote_request.clone(),
        forwarded_request_id,
    )
    .await
    {
        Ok(view) => Ok(view),
        Err(err) if is_remote_network_error(&err) => {
            let item = create_pending_remote_task(state, &project, remote_request).await?;
            Ok(OrchestratorTaskViewDto::PendingRemote {
                item: item.to_dto(),
            })
        }
        Err(err) => Err(err),
    }
}

/// 将 remote-aware Orchestrator 任务加入队列。
///
/// Business Logic（为什么需要这个函数）:
///     local 任务入队发生在本机；remote 任务入队必须转发给 owning device，pending item 不能被本机入队。
///
/// Code Logic（这个函数做什么）:
///     project 为 local 时复用 repo.queue_task；remote 时先识别 pending outbox id，否则调用远端 queue 并 upsert mirror。
#[tauri::command]
pub async fn queue_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let task = state.orchestrator_repo.queue_task(&task_id).await?;
        return Ok(local_task_view(task));
    }
    if let Some(item) = state
        .orchestrator_repo
        .get_remote_outbox_item(&task_id)
        .await?
    {
        return Ok(OrchestratorTaskViewDto::PendingRemote {
            item: item.to_dto(),
        });
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.queue_task(&base_url, &id).await },
    )
    .await
}

/// 重试 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的重试必须作用于远端权威任务，不能把本机 mirror 当作任务状态机处理。
///
/// Code Logic（这个函数做什么）:
///     local 项目复用 Blocked->Queued 原子转换；remote 项目调用 RemoteOrchestratorClient::retry_task 并刷新 mirror。
#[tauri::command]
pub async fn retry_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let updated = state
            .orchestrator_repo
            .transition_task_status(
                &task_id,
                OrchestratorTaskStatus::Blocked,
                OrchestratorTaskStatus::Queued,
                None,
            )
            .await?;
        return Ok(local_task_view(updated));
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.retry_task(&base_url, &id).await },
    )
    .await
}

/// 终止 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的 Abort 必须终止 owning device 上的真实任务，并保留远端现场。
///
/// Code Logic（这个函数做什么）:
///     local 项目复用 set_task_status；remote 项目调用 RemoteOrchestratorClient::abort_task 并刷新 mirror。
#[tauri::command]
pub async fn abort_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let task = state.orchestrator_repo.get_task(&task_id).await?;
        let target = abort_orchestrator_task_target_status(task.status);
        let updated = state
            .orchestrator_repo
            .set_task_status(&task.id, target, None)
            .await?;
        return Ok(local_task_view(updated));
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.abort_task(&base_url, &id).await },
    )
    .await
}
