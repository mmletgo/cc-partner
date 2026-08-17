//! list/create/view/move
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskDto;
use crate::orchestrator::outbox::{
    create_pending_remote_task, is_remote_network_error, open_remote_project_for_shortcut,
    sync_remote_task_mirror_for_project,
};
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::workflow::{
    load_workflow_document, save_workflow_document_at_project_root, validate_workflow_content,
    WorkflowDocument,
};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use std::path::Path;
use tauri::State;

use super::common::*;

/// 查询 Orchestrator 任务列表。
///
/// Business Logic（为什么需要这个函数）:
///     前端 Orchestrator 页面后续需要按项目读取任务队列，也需要支持全局列表调试和管理。
///
/// Code Logic（这个函数做什么）:
///     透传可选 project_id 给仓储 list_tasks，并把 Row 投影为 camelCase DTO。
#[tauri::command]
pub async fn list_orchestrator_tasks(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskDto>, AppError> {
    let rows = state
        .orchestrator_repo
        .list_tasks(project_id.as_deref())
        .await?;
    Ok(rows.into_iter().map(OrchestratorTaskDto::from).collect())
}

/// 创建 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户在前端提交任务后，需要立即生成草稿任务并保存到 SQLite，供后续队列和调度器处理。
///
/// Code Logic（这个函数做什么）:
///     调用共享 helper 完成校验、createAction 状态映射、插入和 Start best-effort dispatch，再返回 DTO。
#[tauri::command]
pub async fn create_orchestrator_task(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskDto, AppError> {
    let row = create_local_task_with_action(state.inner(), request).await?;
    Ok(OrchestratorTaskDto::from(row))
}

/// 查询 remote-aware Orchestrator 任务视图列表。
///
/// Business Logic（为什么需要这个函数）:
///     Phase 6 前端需要在远端项目中展示远端真实任务、本机 pending outbox 和离线 mirror 快照。
///
/// Code Logic（这个函数做什么）:
///     local 项目读取本机任务并包装 Local；remote 项目在线时同步 mirror（可转发 request_id），
///     离线时读最近 mirror；最后追加 pending/sending/failed outbox 项。
pub(crate) async fn list_orchestrator_task_views_for_state(
    state: &AppState,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    list_orchestrator_task_views_for_state_with_request_id(state, project_id, None).await
}

/// Business Logic（为什么需要这个函数）:
///     mobile HTTP list 需要把入站 request_id 贯穿 open-project 与 owner list，避免多跳 ID 断链。
///
/// Code Logic（这个函数做什么）:
///     与 `list_orchestrator_task_views_for_state` 相同，但把 `forwarded_request_id` 传给 mirror 同步。
pub(crate) async fn list_orchestrator_task_views_for_state_with_request_id(
    state: &AppState,
    project_id: Option<String>,
    forwarded_request_id: Option<&str>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let Some(project_id) = project_id else {
        let rows = state.orchestrator_repo.list_tasks(None).await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    };

    let project = get_orchestrator_workbench_project(state, &project_id).await?;
    if project.kind != "remote" {
        let rows = state
            .orchestrator_repo
            .list_tasks(Some(&project_id))
            .await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    }

    let mirrors =
        match sync_remote_task_mirror_for_project(state, &project, forwarded_request_id).await {
            Ok(mirrors) => mirrors,
            Err(err) if is_remote_network_error(&err) => {
                state
                    .orchestrator_repo
                    .list_remote_task_mirrors_for_project_path(&project.device_id, &project.path)
                    .await?
            }
            Err(err) => return Err(err),
        };
    let mut views = remote_mirror_views(mirrors, &project)?;
    views.extend(pending_remote_task_views_for_project(state, &project).await?);
    Ok(views)
}

#[tauri::command]
pub async fn list_orchestrator_task_views(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    list_orchestrator_task_views_for_state(&state, project_id).await
}

/// 创建 remote-aware Orchestrator 任务视图。
///
/// Business Logic（为什么需要这个函数）:
///     local 项目应继续创建本机任务；remote 项目在线时创建远端权威任务，离线时写 pending outbox。
///
/// Code Logic（这个函数做什么）:
///     先按 projectId 读取 Workbench 项目；local 走旧 row builder + repo，remote 先尝试在线创建，
///     遇到网络/离线错误时创建 pending outbox 并返回 PendingRemote。
pub(crate) async fn create_orchestrator_task_view_for_state(
    state: &AppState,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind != "remote" {
        let row = create_local_task_with_action(state, request).await?;
        return Ok(local_task_view(row));
    }

    let remote_request = remote_create_request_from_local(&request);
    // Tauri IPC 无入站 request_id，第二跳由 client 生成新 ID。
    match create_remote_orchestrator_task_online(state, &project, remote_request.clone(), None)
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

#[tauri::command]
pub async fn create_orchestrator_task_view(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    create_orchestrator_task_view_for_state(&state, request).await
}

/// 启动 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要一个显式 startTask 动作，把 Backlog/Draft 或 Todo/Idle 任务放入 scheduler 路径；
///     remote shortcut 上必须转发到 owning device，pendingRemote outbox 不可启动。
///
/// Code Logic（这个函数做什么）:
///     local 项目校验任务归属后调用 repo.start_task，再 best-effort dispatch 并返回最新任务；
///     remote 项目剥离 remote task id 后调用远端 start endpoint，并刷新 mirror。
#[tauri::command]
pub async fn start_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        get_local_project_task_for_action(state.orchestrator_repo.as_ref(), &project_id, &task_id)
            .await?;
        let started = state.orchestrator_repo.start_task(&task_id).await?;
        dispatch_orchestrator_best_effort(state.inner()).await;
        let latest = state.orchestrator_repo.get_task(&started.id).await?;
        return Ok(local_task_view(latest));
    }
    reject_pending_remote_task_action(state.orchestrator_repo.as_ref(), &task_id).await?;
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.start_task(&base_url, &id).await },
    )
    .await
}

/// 请求 remote-aware Orchestrator 任务返工。
///
/// Business Logic（为什么需要这个函数）:
///     人工复核未通过时，用户需要显式记录返工原因并把任务移回 Rework 可领取路径；
///     remote shortcut 上原因必须写入 owning device 的 evidence。
///
/// Code Logic（这个函数做什么）:
///     local 项目校验任务归属后调用 repo.request_task_rework；remote 项目调用远端 request-rework endpoint。
#[tauri::command]
pub async fn request_orchestrator_task_rework_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
    reason: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        get_local_project_task_for_action(state.orchestrator_repo.as_ref(), &project_id, &task_id)
            .await?;
        let task = state
            .orchestrator_repo
            .request_task_rework(&task_id, &reason)
            .await?;
        return Ok(local_task_view(task));
    }
    reject_pending_remote_task_action(state.orchestrator_repo.as_ref(), &task_id).await?;
    let reason = reason.trim().to_string();
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move {
            client.request_rework_task(&base_url, &id, &reason).await
        },
    )
    .await
}

/// 交付 remote-aware 人工复核任务（owner 本地路径）。
///
/// Business Logic（为什么需要这个函数）:
///     用户复核通过后可以显式 deliver，但必须只有 Settings 允许 full-auto delivery 时才进入 Git delivery pipeline。
///     A0 后不再要求人工 digest；remote shortcut 上交付由 owning device 检查 Settings 并执行。
///     Git delivery / Settings gate / delivery lock / event_bus 必须在 HeadlessOwner 进程执行。
///
/// Code Logic（这个函数做什么）:
///     local 项目校验任务归属 → Settings gate → start_delivery_from_human_review →
///     run_delivery_for_task；remote 转发 deliver-reviewed endpoint 并刷新 mirror。
///     供 Tauri owner 路径与 control API 共用。
pub(crate) async fn deliver_reviewed_orchestrator_task_view_for_state(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    if project.kind != "remote" {
        let _task = get_local_project_task_for_action(
            state.orchestrator_repo.as_ref(),
            project_id,
            task_id,
        )
        .await?;
        let config = state
            .config
            .read()
            .expect("config 读锁中毒")
            .orchestrator
            .clone();
        ensure_reviewed_delivery_allowed(&config)?;
        let delivering = state
            .orchestrator_repo
            .start_delivery_from_human_review(task_id)
            .await?;
        // Human Review 显式 Deliver：A0 后无 verifier digest rebind，传 None。
        let delivered = run_delivery_for_task(state, &delivering.id, None).await?;
        return Ok(OrchestratorTaskViewDto::Local { task: delivered });
    }
    reject_pending_remote_task_action(state.orchestrator_repo.as_ref(), task_id).await?;
    update_remote_orchestrator_task_status(
        state,
        &project,
        task_id,
        |client, base_url, id| async move { client.deliver_reviewed_task(&base_url, &id).await },
    )
    .await
}

/// 交付 remote-aware 人工复核任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户复核通过后可以显式 deliver，但必须只有 Settings 允许 full-auto delivery 时才进入 Git delivery pipeline。
///     GuiClient 不得在本进程跑 commit/push/merge 或持有 delivery lock；必须代理到 sidecar owner。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → `BackendControlClient::deliver_reviewed_orchestrator_task`；
///     HeadlessOwner → `deliver_reviewed_orchestrator_task_view_for_state`。
#[tauri::command]
pub async fn deliver_reviewed_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .deliver_reviewed_orchestrator_task(&project_id, &task_id)
            .await;
    }
    deliver_reviewed_orchestrator_task_view_for_state(state.inner(), &project_id, &task_id).await
}

/// 取消 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     cancelTask 是显式业务动作，用于停止任务继续被 scheduler/delivery 接管，但保留现场和 evidence。
///     remote shortcut 上必须取消 owning device 的权威任务，pendingRemote outbox 不可取消为任务。
///
/// Code Logic（这个函数做什么）:
///     local 项目校验归属后调用 repo.cancel_task；remote 项目调用远端 cancel endpoint 并刷新 mirror。
#[tauri::command]
pub async fn cancel_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    use crate::backend::authority::RuntimeRole;
    use crate::backend::control_client::BackendControlClient;
    if state.runtime_role == RuntimeRole::GuiClient {
        let task = BackendControlClient::from_control_file()?
            .cancel_orchestrator_task(&task_id)
            .await?;
        return Ok(OrchestratorTaskViewDto::Local { task });
    }
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        get_local_project_task_for_action(state.orchestrator_repo.as_ref(), &project_id, &task_id)
            .await?;
        let task = state.orchestrator_repo.cancel_task(&task_id).await?;
        return Ok(local_task_view(task));
    }
    reject_pending_remote_task_action(state.orchestrator_repo.as_ref(), &task_id).await?;
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.cancel_task(&base_url, &id).await },
    )
    .await
}

/// 刷新 remote-aware Orchestrator 项目。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要显式 refreshOrchestratorProject 触发一次 best-effort dispatch/reconcile，并得到 dispatched 数量。
///     remote shortcut 上刷新必须转发到 owning device。
///
/// Code Logic（这个函数做什么）:
///     local 项目调用 scheduler dispatch_once 的 best-effort wrapper；remote 项目打开远端项目后调用 refresh endpoint，
///     返回值 projectId 始终使用本机当前 Workbench project id。
#[tauri::command]
pub async fn refresh_orchestrator_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorProjectRefreshDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let dispatched = dispatch_orchestrator_best_effort(state.inner()).await;
        return Ok(OrchestratorProjectRefreshDto {
            project_id,
            dispatched,
        });
    }

    let context = open_remote_project_for_shortcut(state.inner(), &project, None).await?;
    let refreshed = RemoteOrchestratorClient::new()
        .with_expected_device_id(&context.device_id)
        .refresh_project(&context.base_url, &context.remote_project_id)
        .await?;
    Ok(OrchestratorProjectRefreshDto {
        project_id,
        dispatched: refreshed.dispatched,
    })
}

/// 移动 Orchestrator 任务工作流泳道。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 自动化看板需要通过拖拽调整任务所在业务泳道，但移动规则必须由后端统一校验。
///
/// Code Logic（这个函数做什么）:
///     本机项目委托 local helper；远端项目 reject pending 后经 RemoteOrchestratorClient 代理到 owning device。
#[tauri::command]
pub async fn move_orchestrator_task_workflow_state(
    state: State<'_, AppState>,
    request: MoveOrchestratorTaskWorkflowStateRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &request.project_id).await?;
    if project.kind == "remote" {
        reject_pending_remote_task_action(state.orchestrator_repo.as_ref(), &request.task_id)
            .await?;
        let target_state = request.target_state;
        return update_remote_orchestrator_task_with_project(
            state.inner(),
            &project,
            &request.task_id,
            move |client, base_url, project_id, task_id| async move {
                client
                    .move_task_workflow_state(
                        &base_url,
                        crate::orchestrator::remote_protocol::RemoteMoveWorkflowStateReq {
                            project_id,
                            task_id,
                            target_state,
                        },
                    )
                    .await
            },
        )
        .await;
    }
    move_orchestrator_task_workflow_state_for_local_project(
        state.orchestrator_repo.as_ref(),
        &project,
        request,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     向导需要 remote-aware 读取 WORKFLOW.md 状态；local 直接读盘，remote 由 owning device 权威返回。
///
/// Code Logic（这个函数做什么）:
///     解析 Workbench 项目；local 在 project.path 上 `load_workflow_document`；
///     remote open owning device 后 capability-gated 调用 get_workflow_document。
pub(crate) async fn get_workflow_document_for_state(
    state: &AppState,
    project_id: &str,
) -> Result<WorkflowDocument, AppError> {
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    if project.kind == "remote" {
        return get_remote_workflow_document(state, &project, WorkflowDocumentRemoteOp::Get).await;
    }
    get_local_owner_workflow_document(&project)
}

/// Business Logic（为什么需要这个函数）:
///     保存前权威校验可在 local 或 remote owner 上执行，前端不得把前端 YAML 提示当最终结果。
///
/// Code Logic（这个函数做什么）:
///     local 直接 `validate_workflow_content`；remote 转发 content 到 owning device validate 路由。
pub(crate) async fn validate_workflow_document_for_state(
    state: &AppState,
    project_id: &str,
    content: &str,
) -> Result<WorkflowDocument, AppError> {
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    if project.kind == "remote" {
        return get_remote_workflow_document(
            state,
            &project,
            WorkflowDocumentRemoteOp::Validate {
                content: Some(content.to_string()),
            },
        )
        .await;
    }
    // validate 不读盘，但要求 project 存在且为 local，避免对已删除项目误报。
    let _ = require_local_project_path(&project)?;
    Ok(validate_workflow_content(content))
}

/// Business Logic（为什么需要这个函数）:
///     CAS 保存必须在文件所在设备执行；成功后不 dispatch、不改变 delivery。
///
/// Code Logic（这个函数做什么）:
///     local 调 `save_workflow_document_at_project_root`；remote capability-gated 转发 save。
pub(crate) async fn save_workflow_document_for_state(
    state: &AppState,
    project_id: &str,
    expected_hash: &str,
    content: &str,
) -> Result<WorkflowDocument, AppError> {
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    if project.kind == "remote" {
        return get_remote_workflow_document(
            state,
            &project,
            WorkflowDocumentRemoteOp::Save {
                content: Some(content.to_string()),
                expected_hash: Some(expected_hash.to_string()),
            },
        )
        .await;
    }
    let project_path = require_local_project_path(&project)?;
    // spawn_blocking 避免阻塞 async runtime 的磁盘 IO。
    let expected_hash = expected_hash.to_string();
    let content = content.to_string();
    tokio::task::spawn_blocking(move || {
        save_workflow_document_at_project_root(&project_path, &expected_hash, &content)
    })
    .await
    .map_err(|error| AppError::generic(format!("保存 WORKFLOW.md join 失败: {error}")))?
}

/// Business Logic（为什么需要这个函数）:
///     owning-device P2P 路由只接受 local 项目，需要在确认 kind 后复用磁盘文档 helper。
///
/// Code Logic（这个函数做什么）:
///     要求 project.kind=local，在 project.path 上 load。
pub(crate) fn get_local_owner_workflow_document(
    project: &WorkbenchProjectRow,
) -> Result<WorkflowDocument, AppError> {
    let project_path = require_local_project_path(project)?;
    Ok(load_workflow_document(&project_path))
}

/// Business Logic（为什么需要这个函数）:
///     owning-device save 必须确认 local 项目后 CAS 写盘，且不触发 dispatch。
///
/// Code Logic（这个函数做什么）:
///     校验 local path 后调用 `save_workflow_document_at_project_root`。
pub(crate) fn save_local_owner_workflow_document(
    project: &WorkbenchProjectRow,
    expected_hash: &str,
    content: &str,
) -> Result<WorkflowDocument, AppError> {
    let project_path = require_local_project_path(project)?;
    save_workflow_document_at_project_root(&project_path, expected_hash, content)
}

/// Business Logic（为什么需要这个函数）:
///     owning-device validate 只需确认 local 项目身份后对 content 跑权威 parser。
///
/// Code Logic（这个函数做什么）:
///     require local 后返回 `validate_workflow_content(content)`。
pub(crate) fn validate_local_owner_workflow_document(
    project: &WorkbenchProjectRow,
    content: &str,
) -> Result<WorkflowDocument, AppError> {
    let _ = require_local_project_path(project)?;
    Ok(validate_workflow_content(content))
}

/// Business Logic（为什么需要这个函数）:
///     所有文档 API 只能作用在本机 local 项目路径，remote shortcut 必须走 P2P。
///
/// Code Logic（这个函数做什么）:
///     kind!=local → validation；否则返回 PathBuf(project.path)。
fn require_local_project_path(
    project: &WorkbenchProjectRow,
) -> Result<std::path::PathBuf, AppError> {
    if project.kind != "local" {
        return Err(AppError::validation("远端 Orchestrator 只接受对端本机项目"));
    }
    let path = Path::new(&project.path);
    if project.path.trim().is_empty() {
        return Err(AppError::validation("项目路径不能为空"));
    }
    Ok(path.to_path_buf())
}

/// remote workflow document 操作类型。
enum WorkflowDocumentRemoteOp {
    Get,
    Validate {
        content: Option<String>,
    },
    Save {
        content: Option<String>,
        expected_hash: Option<String>,
    },
}

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 WORKFLOW 向导必须读写 owning device 上的权威文件。
///
/// Code Logic（这个函数做什么）:
///     open remote project → capability-gated client 方法 → 返回 WorkflowDocument。
async fn get_remote_workflow_document(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    op: WorkflowDocumentRemoteOp,
) -> Result<WorkflowDocument, AppError> {
    let context = open_remote_project_for_shortcut(state, remote_shortcut, None).await?;
    let client = RemoteOrchestratorClient::new().with_expected_device_id(&context.device_id);
    let result = match op {
        WorkflowDocumentRemoteOp::Get => {
            client
                .get_workflow_document(&context.base_url, &context.remote_project_id)
                .await
        }
        WorkflowDocumentRemoteOp::Validate { content } => {
            let content = content.unwrap_or_default();
            client
                .validate_workflow_document(&context.base_url, &context.remote_project_id, &content)
                .await
        }
        WorkflowDocumentRemoteOp::Save {
            content,
            expected_hash,
        } => {
            let content = content.unwrap_or_default();
            let expected_hash = expected_hash.unwrap_or_default();
            client
                .save_workflow_document(
                    &context.base_url,
                    &context.remote_project_id,
                    &expected_hash,
                    &content,
                )
                .await
        }
    };
    result.map_err(|error| {
        crate::net::peer_error::peer_call_error_to_app_error(error, "远端 Orchestrator")
    })
}

/// 读取 remote-aware WORKFLOW 文档状态。
///
/// Business Logic（为什么需要这个函数）:
///     向导检测步骤需要 missing/valid/invalid/readError 与 contentHash。
///     GuiClient 必须经 control 读 owner 权威文件，禁止本进程猜盘路径。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `orchestrator/workflow-document/get`；
///     HeadlessOwner → `get_workflow_document_for_state`。
#[tauri::command]
pub async fn get_workflow_document(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<WorkflowDocument, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .get_workflow_document(project_id.trim())
            .await;
    }
    get_workflow_document_for_state(state.inner(), project_id.trim()).await
}

/// 权威校验 WORKFLOW 文档内容。
///
/// Business Logic（为什么需要这个函数）:
///     保存前必须调用后端 parser；返回 diagnostics 与规范化 preview。
///     GuiClient 校验也必须代理到 owner，保持与 save 同一权威进程。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `orchestrator/workflow-document/validate`；
///     HeadlessOwner → `validate_workflow_document_for_state`。
#[tauri::command]
pub async fn validate_workflow_document(
    state: State<'_, AppState>,
    project_id: String,
    content: String,
) -> Result<WorkflowDocument, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .validate_workflow_document(project_id.trim(), &content)
            .await;
    }
    validate_workflow_document_for_state(state.inner(), project_id.trim(), &content).await
}

/// CAS 保存 WORKFLOW 文档。
///
/// Business Logic（为什么需要这个函数）:
///     向导保存使用 expectedHash；冲突要求重新加载；成功后不自动 dispatch。
///     GuiClient 不得在本进程 CAS 写盘；排他 create / hash 门禁只在 owner 执行。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `orchestrator/workflow-document/save`（mutation 不重试）；
///     HeadlessOwner → `save_workflow_document_for_state`；不调用 scheduler。
#[tauri::command]
pub async fn save_workflow_document(
    state: State<'_, AppState>,
    project_id: String,
    expected_hash: String,
    content: String,
) -> Result<WorkflowDocument, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .save_workflow_document(project_id.trim(), &expected_hash, &content)
            .await;
    }
    save_workflow_document_for_state(state.inner(), project_id.trim(), &expected_hash, &content)
        .await
}

/// 获取 Orchestrator 项目运行时快照（remote-aware 共享入口）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Tauri 命令与 `/mobile` HTTP 路由都需要同一套 local/remote 四态分发，
///     避免手机浏览器绕过 owning-device 拉取或读到本机冒充远端的快照。
///
/// Code Logic（这个函数做什么）:
///     读取 Workbench 项目；local 走共享 builder + 本机 config/telemetry；
///     remote 走 open_remote_project_for_shortcut + RemoteOrchestratorClient::runtime_snapshot，
///     成功映射 identity 字段，失败按 PeerCallError 变体回落空快照。
///     本函数从不向调用方暴露 owning device 的 P2P base URL。
pub(crate) async fn get_orchestrator_runtime_snapshot_for_state(
    state: &AppState,
    project_id: &str,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    get_orchestrator_runtime_snapshot_for_state_with_request_id(state, project_id, None).await
}

/// Business Logic（为什么需要这个函数）:
///     Mobile HTTP 入口携带入站 request_id，需要沿两跳链路转发到 owning device；
///     Tauri IPC 入口没有入站 ID，传 None 由下游生成。
///
/// Code Logic（这个函数做什么）:
///     与 `get_orchestrator_runtime_snapshot_for_state` 相同的 local/remote 四态分发，
///     但把可选 `forwarded_request_id` 传给 remote open-project / runtime_snapshot。
pub(crate) async fn get_orchestrator_runtime_snapshot_for_state_with_request_id(
    state: &AppState,
    project_id: &str,
    forwarded_request_id: Option<&str>,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    // GuiClient 必须代理到 sidecar owner；禁止用 GUI 本地空 telemetry 填充 owner 字段。
    // control API 在 HeadlessOwner 上调用本函数时不会进入此分支，避免递归。
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .orchestrator_runtime_snapshot(project_id)
            .await;
    }
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    // 远端 shortcut 不得读本机 scheduler/config/workflow 冒充 owner 状态。
    // 共享 builder 只负责本地项目；远端守卫与四态分发留在命令层。
    if project.kind == "remote" {
        return get_remote_orchestrator_runtime_snapshot(state, &project, forwarded_request_id)
            .await;
    }
    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    let scheduler_snapshot = state.orchestrator_scheduler_telemetry.snapshot();
    get_orchestrator_runtime_snapshot_for_project(
        state.orchestrator_repo.as_ref(),
        &config,
        &project,
        &scheduler_snapshot,
    )
    .await
}
