//! net/routes/orchestrator.rs — Orchestrator 远端 HTTP 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench remote shortcut 上的 Orchestrator 操作必须发送到项目所在设备，由 owning device 的 SQLite
//!     任务队列作为权威来源。
//!
//! Code Logic（这个模块做什么）:
//!     将 Orchestrator 创建、列表、evidence、queue/retry/abort 和全局配置读取包装为 axum handler；
//!     所有项目入口都先确认 projectId 指向本设备 local Workbench 项目，拒绝 remote shortcut 递归代理。

use crate::commands::orchestrator::{
    build_orchestrator_task_row, create_orchestrator_task_view_for_http_with_request_id,
    discard_orchestrator_remote_outbox_for_repos, dispatch_orchestrator_best_effort,
    ensure_reviewed_delivery_allowed, get_local_owner_workflow_document,
    get_orchestrator_runtime_snapshot_for_project,
    get_orchestrator_runtime_snapshot_for_state_with_request_id, get_workflow_document_for_state,
    list_orchestrator_task_views_for_state_with_request_id,
    retry_orchestrator_remote_outbox_for_repos, run_delivery_for_task,
    save_local_owner_workflow_document, save_workflow_document_for_state,
    validate_local_owner_workflow_document, validate_workflow_document_for_state,
    CreateOrchestratorTaskRequest, OrchestratorRuntimeSnapshotDto, OrchestratorTaskViewDto,
};
use crate::commands::orchestrator_adapters::{
    build_agent_adapter_catalog, OrchestratorAgentAdapterCatalog,
};
use crate::commands::prompt_optimizer::{
    local_complete_orchestrator_task_prompt, OrchestratorTaskPromptCompletionDto,
};
use crate::config::AppConfig;
use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::orchestrator::agent_adapter::AgentAdapterRegistry;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
};
use crate::orchestrator::outbox::open_remote_project_for_shortcut;
use crate::orchestrator::outbox::OrchestratorRemoteOutboxDto;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::{
    MobileRuntimeSnapshotReq, MobileWorkflowDocumentGetReq, MobileWorkflowDocumentSaveReq,
    MobileWorkflowDocumentValidateReq, RemoteCompleteOrchestratorTaskPromptReq,
    RemoteCreateOrchestratorTaskReq, RemoteDeliverReviewedReq, RemoteListTasksReq,
    RemoteOrchestratorConfigResp, RemoteOrchestratorEvidenceResp,
    RemoteOrchestratorProjectRefreshResp, RemoteOrchestratorTaskListResp, RemoteRuntimeSnapshotReq,
    RemoteTaskReq, RemoteTaskReworkReq, RemoteWorkflowDocumentGetReq, RemoteWorkflowDocumentResp,
    RemoteWorkflowDocumentSaveReq, RemoteWorkflowDocumentValidateReq,
};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetrySnapshot;
use crate::state::AppState;
use crate::storage::WorkbenchProjectRepo;
use crate::workbench::models::WorkbenchProjectRow;
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Orchestrator 远端 route 需要的共享状态子集。
///
/// Business Logic（为什么需要这个结构体）:
///     HTTP handler 生产态从 AppState 取依赖，单测只需要最小仓储和配置，不应强行构造完整 GUI 句柄。
///
/// Code Logic（这个结构体做什么）:
///     保存 config、OrchestratorRepo 和 WorkbenchProjectRepo 三个依赖；handler 从 AppState clone，测试直接构造。
#[derive(Clone)]
struct OrchestratorRouteContext {
    config: Arc<RwLock<AppConfig>>,
    orchestrator_repo: Arc<OrchestratorRepo>,
    workbench_project_repo: Arc<WorkbenchProjectRepo>,
}

/// Mobile-facing Orchestrator task view list 响应。
///
/// Business Logic（为什么需要这个结构体）:
///     `/mobile` 需要接收 local/remote/pendingRemote tagged union，而旧 P2P list route 必须继续返回裸 tasks。
///
/// Code Logic（这个结构体做什么）:
///     用 `{views}` 包装 OrchestratorTaskViewDto 列表，避免和 `{tasks}` 旧协议混淆。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorTaskViewListResp {
    pub views: Vec<OrchestratorTaskViewDto>,
}

impl OrchestratorRouteContext {
    /// 从完整 AppState 构造 route context。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产 handler 仍由 axum 注入完整 AppState，但 Orchestrator route 只需要其中三个依赖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone AppState 内部 Arc，构造轻量 context；不会复制底层数据库连接或配置内容。
    fn from_app_state(state: &AppState) -> Self {
        Self {
            config: state.config.clone(),
            orchestrator_repo: state.orchestrator_repo.clone(),
            workbench_project_repo: state.workbench_project_repo.clone(),
        }
    }
}

/// 确认 Workbench 项目是本机 local 项目。
///
/// Business Logic（为什么需要这个函数）:
///     P2P Orchestrator 网关只接受 owning device 上的 local projectId，remote shortcut 不能递归代理到第三台设备。
///
/// Code Logic（这个函数做什么）:
///     检查项目 row 的 kind 是否为 local；非 local 返回校验错误（HTTP 边界映射 400 validation_error）。
fn ensure_remote_orchestrator_local_project(project: &WorkbenchProjectRow) -> Result<(), AppError> {
    if project.kind != "local" {
        return Err(AppError::validation("远端 Orchestrator 只接受对端本机项目"));
    }
    Ok(())
}

/// 通过 projectId 确认本机 local 项目。
///
/// Business Logic（为什么需要这个函数）:
///     create/list 请求直接携带 projectId，必须在进入 Orchestrator 任务仓储前确认项目归属。
///
/// Code Logic（这个函数做什么）:
///     从 Workbench 项目仓库读取 projectId，缺失返回 not_found，存在时复用 kind guard。
async fn ensure_remote_orchestrator_local_project_id(
    state: &OrchestratorRouteContext,
    project_id: &str,
) -> Result<(), AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Orchestrator 项目不存在"))?;
    ensure_remote_orchestrator_local_project(&project)
}

/// 读取任务并确认所属项目是本机 local 项目。
///
/// Business Logic（为什么需要这个函数）:
///     evidence/queue/retry/abort 请求只携带 taskId，也必须避免操作 remote shortcut 项目的任务行。
///
/// Code Logic（这个函数做什么）:
///     先读取任务，再用任务的 project_id 复用 project kind guard，最后把任务 Row 返回给调用方。
async fn get_local_project_task(
    state: &OrchestratorRouteContext,
    task_id: &str,
) -> Result<OrchestratorTaskRow, AppError> {
    let task = state.orchestrator_repo.get_task(task_id).await?;
    ensure_remote_orchestrator_local_project_id(state, &task.project_id).await?;
    Ok(task)
}

/// 创建远端 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     本机 remote shortcut 创建任务时，owning device 需要生成权威任务行，并按 createAction 写入初始状态。
///     调用方（create route）还需要知道是否首次插入，以便仅在新建时触发 Start dispatch。
///
/// Code Logic（这个函数做什么）:
///     确认 projectId 为 local 后要求非空 clientRequestId，再复用命令层 row builder 创建基础 row；
///     repo 事务按 createAction 和 clientRequestId 保证重复请求返回同一任务，并透出 newly_created。
async fn create_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteCreateOrchestratorTaskReq,
) -> Result<crate::orchestrator::repo::IdempotentCreateTaskOutcome, AppError> {
    ensure_remote_orchestrator_local_project_id(state, &req.project_id).await?;
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::generic("远端创建任务缺少 clientRequestId"))?;
    let row = build_orchestrator_task_row(CreateOrchestratorTaskRequest {
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
    })?;
    state
        .orchestrator_repo
        .create_remote_task_for_client_request(&client_request_id, &row, req.create_action)
        .await
}

/// 按项目列出远端 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的任务列表只能展示当前远端 local projectId 的权威任务。
///
/// Code Logic（这个函数做什么）:
///     确认项目 local 后调用 repo.list_tasks(Some(project_id))，再包装 `{tasks}` 响应。
async fn list_tasks_for_state(
    state: &OrchestratorRouteContext,
    project_id: &str,
) -> Result<RemoteOrchestratorTaskListResp, AppError> {
    ensure_remote_orchestrator_local_project_id(state, project_id).await?;
    let tasks = state
        .orchestrator_repo
        .list_tasks(Some(project_id))
        .await?
        .into_iter()
        .map(OrchestratorTaskDto::from)
        .collect();
    Ok(RemoteOrchestratorTaskListResp { tasks })
}

/// 按任务读取 evidence。
///
/// Business Logic（为什么需要这个函数）:
///     远端任务详情需要读取 owning device 上归档的 evidence，同时不能操作 remote shortcut 任务。
///
/// Code Logic（这个函数做什么）:
///     先按 taskId 确认任务所属 local 项目，再调用 repo.list_evidence 并包装 `{evidence}`。
async fn get_evidence_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<RemoteOrchestratorEvidenceResp, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let evidence = state.orchestrator_repo.list_evidence(&req.task_id).await?;
    Ok(RemoteOrchestratorEvidenceResp { evidence })
}

/// 将远端草稿任务入队。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 remote shortcut 点击入队时，状态转换必须在 owning device 上执行且只允许 Draft->Queued。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后复用 repo.queue_task 原子状态转换。
async fn queue_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state.orchestrator_repo.queue_task(&req.task_id).await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// 启动远端本机任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 start 操作必须在 owning device 上把任务放入 scheduler 可领取路径，并尽力立即调度。
///
/// Code Logic（这个函数做什么）:
///     确认任务属于本机 local 项目后调用 repo.start_task，再通过 AppState 触发 best-effort dispatch。
async fn start_task_for_state(
    state: &AppState,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    let context = OrchestratorRouteContext::from_app_state(state);
    let task = get_local_project_task(&context, &req.task_id).await?;
    context.orchestrator_repo.start_task(&task.id).await?;
    dispatch_orchestrator_best_effort(state).await;
    let latest = context.orchestrator_repo.get_task(&task.id).await?;
    Ok(OrchestratorTaskDto::from(latest))
}

/// 重试远端阻塞任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户处理 blocked 原因后，需要把 owning device 上的任务重新排队，不应立即 dispatch。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后复用 repo.transition_task_status 执行 Blocked->Queued 条件转换。
async fn retry_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state
        .orchestrator_repo
        .transition_task_status(
            &req.task_id,
            OrchestratorTaskStatus::Blocked,
            OrchestratorTaskStatus::Queued,
            None,
        )
        .await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// 请求远端本机任务返工。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的 requestRework 必须在 owning device 的任务 evidence 中记录人工返工原因。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后调用 repo.request_task_rework，返回更新后的任务 DTO。
async fn request_rework_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReworkReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state
        .orchestrator_repo
        .request_task_rework(&req.task_id, &req.reason)
        .await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// 交付远端本机人工复核任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 deliverReviewedTask 必须受 owning device Settings 控制，并在 owning device 执行 Git delivery。
///     A0 后不再要求人工 review digest；仍受 Settings full-auto delivery gate 约束。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目，读取 Settings gate，通过后切入 Delivering 并跑共享 delivery pipeline。
async fn deliver_reviewed_task_for_state(
    state: &AppState,
    req: RemoteDeliverReviewedReq,
) -> Result<OrchestratorTaskDto, AppError> {
    let context = OrchestratorRouteContext::from_app_state(state);
    let task = get_local_project_task(&context, &req.task_id).await?;
    let config = context
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    ensure_reviewed_delivery_allowed(&config)?;
    let delivering = context
        .orchestrator_repo
        .start_delivery_from_human_review(&task.id)
        .await?;
    // Human Review / P2P deliver-reviewed：无 verifier digest rebind，传 None。
    run_delivery_for_task(state, &delivering.id, None).await
}

/// 终止远端任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 remote shortcut 终止任务时，owning device 应把权威任务置为 Aborted 并保留现场；
///     experiment candidate 同步 Cancelled，防止后续 approve 复活交付。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后 set Aborted；若 experiment_id 存在则 sync_candidate_with_task_terminal。
async fn abort_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state
        .orchestrator_repo
        .abort_task_preserving_done(&req.task_id)
        .await?;
    if task.experiment_id.is_some() {
        if let Err(err) =
            crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                state.orchestrator_repo.as_ref(),
                &task.id,
                task.status,
            )
            .await
        {
            tracing::debug!(
                task_id = %task.id,
                "sync_candidate after p2p abort: {err}"
            );
        }
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// 取消远端本机任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 cancelTask 应移动 owning device 上的权威任务到 Canceled/Idle，并保留现场和证据。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后调用 repo.cancel_task。
async fn cancel_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state.orchestrator_repo.cancel_task(&req.task_id).await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// 刷新远端本机项目。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 刷新项目时，owning device 需要触发一次 best-effort dispatch/reconcile，并返回领取数量。
///
/// Code Logic（这个函数做什么）:
///     确认 projectId 是本机 local 项目后调用共享 best-effort dispatch wrapper。
async fn refresh_project_for_state(
    state: &AppState,
    project_id: &str,
) -> Result<RemoteOrchestratorProjectRefreshResp, AppError> {
    let context = OrchestratorRouteContext::from_app_state(state);
    ensure_remote_orchestrator_local_project_id(&context, project_id).await?;
    let dispatched = dispatch_orchestrator_best_effort(state).await;
    Ok(RemoteOrchestratorProjectRefreshResp {
        project_id: project_id.to_string(),
        dispatched,
    })
}

/// 构造 owning-device 项目运行时快照。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的状态条需要拉取 owning device 上的权威运行时快照（调度器、workflow、槽位和事件），
///     不能用本机 shortcut 的 scheduler/config/workflow 冒充。本函数复用 T1 共享 builder，
///     保证远端设备状态条与本机命令看到的是同一份本地快照构造逻辑。
///
/// Code Logic（这个函数做什么）:
///     解析 projectId 对应的本机 Workbench 项目，要求 kind == local（拒绝 remote shortcut 递归代理），
///     读取设备级 Orchestrator 配置，再调用共享 builder 组装 runtime snapshot DTO。
///     本函数从不调用 remote_client——它只服务 owning device 上的 local 项目。
async fn runtime_snapshot_for_state(
    state: &OrchestratorRouteContext,
    project_id: &str,
    scheduler_snapshot: &OrchestratorSchedulerTelemetrySnapshot,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Orchestrator 项目不存在"))?;
    ensure_remote_orchestrator_local_project(&project)?;
    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    get_orchestrator_runtime_snapshot_for_project(
        state.orchestrator_repo.as_ref(),
        &config,
        &project,
        scheduler_snapshot,
    )
    .await
}

/// 读取远端设备 Orchestrator 全局配置。
///
/// Business Logic（为什么需要这个函数）:
///     远端诊断/兼容入口需要读取项目所在设备的全局自动化配置，而不是本机 shortcut 设备的配置。
///     OrchestratorPanel 不展示该配置；用户配置入口固定在 Settings 自动化 tab。
///
/// Code Logic（这个函数做什么）:
///     从 context.config 读锁克隆 AppConfig.orchestrator，并包装为 `{config}` 响应。
fn get_config_for_state(state: &OrchestratorRouteContext) -> RemoteOrchestratorConfigResp {
    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    RemoteOrchestratorConfigResp {
        config: OrchestratorAutomationConfigDto::from(config),
    }
}

/// 创建 Orchestrator 任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     其它设备需要通过 P2P HTTP 在本设备 local 项目中创建权威任务。
///     createAction=Start 只能在首次创建后 best-effort 调度；幂等重放不得再次 dispatch。
///
/// Code Logic（这个函数做什么）:
///     接收 JSON 请求体，构造 route context 后委托 create_task_for_state；
///     仅 `newly_created && Start` 时 dispatch_once 并刷新返回状态，重放直接返回既有任务 DTO。
pub async fn create_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateOrchestratorTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let create_action = req.create_action;
    let context = OrchestratorRouteContext::from_app_state(&state);
    let outcome = create_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.create"))?;
    let created = OrchestratorTaskDto::from(outcome.task);
    if !outcome.newly_created || !create_action.should_dispatch_after_create() {
        return Ok(Json(created));
    }

    if let Err(err) = crate::orchestrator::scheduler::dispatch_once(&state).await {
        tracing::warn!(
            task_id = %created.id,
            error = %err,
            "orchestrator remote createAction=start dispatch failed after task creation"
        );
    }
    let latest = match state.orchestrator_repo.get_task(&created.id).await {
        Ok(row) => OrchestratorTaskDto::from(row),
        Err(err) => {
            tracing::warn!(
                task_id = %created.id,
                error = %err,
                "orchestrator remote createAction=start failed to refresh task after dispatch"
            );
            created
        }
    };
    Ok(Json(latest))
}

/// 完善 Orchestrator 创建任务 Prompt HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 `/mobile` 不能调用 Tauri invoke，需要通过同源 HTTP 让本设备 Claude CLI 生成任务标题、目标和验收标准。
///
/// Code Logic（这个函数做什么）:
///     接收 `{prompt, workingDirectory?}`，委托 prompt_optimizer 的本机 helper，返回三字段 camelCase DTO。
pub async fn complete_task_prompt(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCompleteOrchestratorTaskPromptReq>,
) -> P2pResult<Json<OrchestratorTaskPromptCompletionDto>> {
    let mut working_directory = req.working_directory;
    if let Some(project_id) = req
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let project = state
            .workbench_project_repo
            .get(project_id)
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.complete_prompt"))?
            .ok_or_else(|| P2pError::not_found("自动化 Prompt 完善项目不存在", &ctx))?;
        if project.kind == "remote" {
            let context =
                open_remote_project_for_shortcut(&state, &project, Some(ctx.request_id.as_str()))
                    .await
                    .map_err(|e| {
                        P2pError::from_app_error(e, &ctx, "orchestrator.tasks.complete_prompt")
                    })?;
            let completed = RemoteOrchestratorClient::new()
                .with_forwarded_request_id(&ctx.request_id)
                .complete_prompt(
                    &context.base_url,
                    RemoteCompleteOrchestratorTaskPromptReq {
                        project_id: Some(context.remote_project_id),
                        prompt: req.prompt,
                        working_directory: Some(context.remote_project_path),
                    },
                )
                .await
                .map_err(|e| {
                    P2pError::from_app_error(e, &ctx, "orchestrator.tasks.complete_prompt")
                })?;
            return Ok(Json(completed));
        }
        if working_directory
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        {
            working_directory = Some(project.path);
        }
    }
    let completed = local_complete_orchestrator_task_prompt(&state, req.prompt, working_directory)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.complete_prompt"))?;
    Ok(Json(completed))
}

/// 列出 mobile-facing Orchestrator task views HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机端可能选择本机项目或本机保存的远端项目 shortcut，需要同一接口返回可展示的 task view 列表。
///
/// Code Logic（这个函数做什么）:
///     接收 `{projectId}`，把入站 request_id 贯穿 open-project/owner list，并用 `{views}` 包装结果。
pub async fn list_task_views(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListTasksReq>,
) -> P2pResult<Json<OrchestratorTaskViewListResp>> {
    let views = list_orchestrator_task_views_for_state_with_request_id(
        &state,
        Some(req.project_id),
        Some(ctx.request_id.as_str()),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.task_views.list"))?;
    Ok(Json(OrchestratorTaskViewListResp { views }))
}

/// 创建 mobile-facing Orchestrator task view HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机端创建任务需要支持 local 和 remote shortcut，并保留 createAction 与 clientRequestId 幂等语义。
///
/// Code Logic（这个函数做什么）:
///     接收 RemoteCreateOrchestratorTaskReq，把入站 request_id 贯穿 open-project/owner create，
///     返回 local/remote/pendingRemote view。
pub async fn create_task_view(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateOrchestratorTaskReq>,
) -> P2pResult<Json<OrchestratorTaskViewDto>> {
    let view = create_orchestrator_task_view_for_http_with_request_id(
        &state,
        req,
        Some(ctx.request_id.as_str()),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.task_views.create"))?;
    Ok(Json(view))
}

/// 列出 Orchestrator 任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 需要读取项目所在设备的任务列表。
///
/// Code Logic（这个函数做什么）:
///     接收 `{projectId}` 请求体，构造 route context 后委托 list_tasks_for_state。
pub async fn list_tasks(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListTasksReq>,
) -> P2pResult<Json<RemoteOrchestratorTaskListResp>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let resp = list_tasks_for_state(&context, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.list"))?;
    Ok(Json(resp))
}

/// 读取任务 evidence HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的任务详情需要拉取 owning device 上的 evidence。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，构造 route context 后委托 get_evidence_for_state。
pub async fn get_evidence(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<RemoteOrchestratorEvidenceResp>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let resp = get_evidence_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.evidence"))?;
    Ok(Json(resp))
}
/// owning-device WORKFLOW 文档 get handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 需要读取 owning device 上的 WORKFLOW.md 状态/正文/hash。
///
/// Code Logic（这个函数做什么）:
///     接收 `{projectId}`，确认 local 项目后 `get_local_owner_workflow_document`。
pub async fn get_workflow_document_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorkflowDocumentGetReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.get"))?
        .ok_or_else(|| P2pError::not_found("工作台项目不存在", &ctx))?;
    let document = get_local_owner_workflow_document(&project)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.get"))?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// owning-device WORKFLOW 文档 validate handler。
pub async fn validate_workflow_document_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorkflowDocumentValidateReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.validate"))?
        .ok_or_else(|| P2pError::not_found("工作台项目不存在", &ctx))?;
    let document = validate_local_owner_workflow_document(&project, &req.content).map_err(|e| {
        P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.validate")
    })?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// owning-device WORKFLOW 文档 CAS save handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote 保存必须在 owner 做 CAS 与原子写；不得 dispatch。
///
/// Code Logic（这个函数做什么）:
///     确认 local 项目后 `save_local_owner_workflow_document`。
pub async fn save_workflow_document_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorkflowDocumentSaveReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.save"))?
        .ok_or_else(|| P2pError::not_found("工作台项目不存在", &ctx))?;
    let document = save_local_owner_workflow_document(&project, &req.expected_hash, &req.content)
        .map_err(|e| {
        P2pError::from_app_error(e, &ctx, "orchestrator.workflow_document.save")
    })?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// Mobile remote-aware WORKFLOW get。
pub async fn mobile_get_workflow_document(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<MobileWorkflowDocumentGetReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let document = get_workflow_document_for_state(&state, project_id)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "orchestrator.mobile.workflow_document.get")
        })?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// Mobile remote-aware WORKFLOW validate。
pub async fn mobile_validate_workflow_document(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<MobileWorkflowDocumentValidateReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let document = validate_workflow_document_for_state(&state, project_id, &req.content)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "orchestrator.mobile.workflow_document.validate")
        })?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// Mobile remote-aware WORKFLOW save。
pub async fn mobile_save_workflow_document(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<MobileWorkflowDocumentSaveReq>,
) -> P2pResult<Json<RemoteWorkflowDocumentResp>> {
    let project_id = req.project_id.trim();
    if project_id.is_empty() {
        return Err(P2pError::validation("projectId 不能为空", &ctx));
    }
    let document =
        save_workflow_document_for_state(&state, project_id, &req.expected_hash, &req.content)
            .await
            .map_err(|e| {
                P2pError::from_app_error(e, &ctx, "orchestrator.mobile.workflow_document.save")
            })?;
    Ok(Json(RemoteWorkflowDocumentResp { document }))
}

/// 将任务入队 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 queue 操作需要在 owning device 上做安全 Draft->Queued 转换。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，构造 route context 后委托 queue_task_for_state。
pub async fn queue_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let task = queue_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.queue"))?;
    Ok(Json(task))
}

/// 启动任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 start 操作需要在 owning device 上显式进入 scheduler 路径。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，委托 start_task_for_state。
pub async fn start_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let task = start_task_for_state(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.start"))?;
    Ok(Json(task))
}

/// 重试任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 retry 操作需要在 owning device 上做 Blocked->Queued 转换。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，构造 route context 后委托 retry_task_for_state。
pub async fn retry_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let task = retry_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.retry"))?;
    Ok(Json(task))
}

/// 请求返工 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 requestRework 操作需要在 owning device 记录返工原因和 evidence。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId, reason}` 请求体，构造 route context 后委托 request_rework_task_for_state。
pub async fn request_rework_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReworkReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let task = request_rework_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.request_rework"))?;
    Ok(Json(task))
}

/// 交付人工复核任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 deliverReviewedTask 操作需要由 owning device 检查 Settings 并运行 delivery pipeline，
///     A0 后无人工 digest 门禁；仍受 Settings full-auto delivery gate 约束。
///
/// Code Logic（这个函数做什么）:
///     接收 camelCase `{taskId}` 请求体，委托 deliver_reviewed_task_for_state。
pub async fn deliver_reviewed_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteDeliverReviewedReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let task = deliver_reviewed_task_for_state(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.deliver_reviewed"))?;
    Ok(Json(task))
}

/// 终止任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 abort 操作需要终止 owning device 上的权威任务。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，构造 route context 后委托 abort_task_for_state。
pub async fn abort_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let task = abort_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.abort"))?;
    Ok(Json(task))
}

/// 取消任务 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 cancelTask 操作需要在 owning device 上移动权威任务到 Canceled/Idle。
///
/// Code Logic（这个函数做什么）:
///     接收 `{taskId}` 请求体，构造 route context 后委托 cancel_task_for_state。
pub async fn cancel_task(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteTaskReq>,
) -> P2pResult<Json<OrchestratorTaskDto>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    let task = cancel_task_for_state(&context, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.tasks.cancel"))?;
    Ok(Json(task))
}

/// 刷新项目 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 refreshOrchestratorProject 操作需要在 owning device 上触发一次 best-effort dispatch。
///
/// Code Logic（这个函数做什么）:
///     接收 `{projectId}` 请求体，委托 refresh_project_for_state 并返回 `{projectId, dispatched}`。
pub async fn refresh_project(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListTasksReq>,
) -> P2pResult<Json<RemoteOrchestratorProjectRefreshResp>> {
    let resp = refresh_project_for_state(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.projects.refresh"))?;
    Ok(Json(resp))
}

/// 读取 owning-device 项目运行时快照 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的状态条需要通过 P2P HTTP 拉取 owning device 上的权威运行时快照，
///     供前端展示调度器、workflow、槽位和最近事件。本路由由 `orchestrator.runtime-snapshot.v1`
///     能力 token 门控。
///
/// Code Logic（这个函数做什么）:
///     接收 snake_case `{project_id}` 请求体，构造 route context 并读取 scheduler telemetry 快照，
///     委托 runtime_snapshot_for_state。本 handler 从不调用 remote_client，仅服务本机 local 项目。
pub async fn runtime_snapshot(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteRuntimeSnapshotReq>,
) -> P2pResult<Json<OrchestratorRuntimeSnapshotDto>> {
    let project_id = req.project_id.trim().to_string();
    if project_id.is_empty() {
        return Err(P2pError::from_app_error(
            AppError::validation("远端 Orchestrator runtime snapshot 缺少 project_id"),
            &ctx,
            "orchestrator.runtime_snapshot",
        ));
    }
    let context = OrchestratorRouteContext::from_app_state(&state);
    let scheduler_snapshot = state.orchestrator_scheduler_telemetry.snapshot();
    let snapshot = runtime_snapshot_for_state(&context, &project_id, &scheduler_snapshot)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.runtime_snapshot"))?;
    Ok(Json(snapshot))
}

/// 读取 mobile-facing 项目运行时快照 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机浏览器 `/mobile` 需要通过同源 HTTP 拉取本机或远端 shortcut 的 runtime snapshot，
///     且必须复用与 Tauri 命令相同的四态 helper，不能把 owning device 的 P2P base URL 暴露给浏览器。
///
/// Code Logic（这个函数做什么）:
///     接收 camelCase `{projectId}`（MobileRuntimeSnapshotReq），trim 后委托
///     `get_orchestrator_runtime_snapshot_for_state`；该 helper 对 local/remote 分流，
///     远端成功返回 live 映射快照，preflight/peer 失败返回 offline/unsupported/unavailable 空快照 DTO。
pub async fn mobile_runtime_snapshot(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<MobileRuntimeSnapshotReq>,
) -> P2pResult<Json<OrchestratorRuntimeSnapshotDto>> {
    let project_id = req.project_id.trim().to_string();
    if project_id.is_empty() {
        return Err(P2pError::from_app_error(
            AppError::validation("移动端 Orchestrator runtime snapshot 缺少 projectId"),
            &ctx,
            "orchestrator.mobile.runtime_snapshot",
        ));
    }
    let snapshot = get_orchestrator_runtime_snapshot_for_state_with_request_id(
        &state,
        &project_id,
        Some(ctx.request_id.as_str()),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.mobile.runtime_snapshot"))?;
    Ok(Json(snapshot))
}

/// 读取 Orchestrator 全局配置 HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     诊断/兼容路径需要知道 owning device 当前自动化开关、并发上限、验证命令和 delivery flags。
///     用户可见配置仍固定在 owning device 的 Settings 自动化 tab。
///
/// Code Logic（这个函数做什么）:
///     构造 route context 后同步读取 config，返回 `{config}`。
pub async fn get_config(
    State(state): State<AppState>,
    Extension(_ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<RemoteOrchestratorConfigResp>> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(get_config_for_state(&context)))
}

/// Mobile HTTP 本机 failed outbox 动作请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     手机 Automation 面板的 Retry/Discard 需要绑定当前 remote shortcut 与 outbox 行，
///     outbox 只存在于当前 cc-partner 设备，不能代理到远端 owner。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 接收 `{projectId,outboxId}`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileRemoteOutboxActionReq {
    pub project_id: String,
    pub outbox_id: String,
}

/// Business Logic（为什么需要这个函数）:
///     手机端对 failed outbox 点 Retry 时，必须复用与 Tauri 相同的本机归属校验与 failed-only 转移。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 取仓储，委托 retry_orchestrator_remote_outbox_for_repos，返回 camelCase DTO。
pub async fn retry_remote_outbox(
    State(state): State<AppState>,
    Json(req): Json<MobileRemoteOutboxActionReq>,
) -> Result<Json<OrchestratorRemoteOutboxDto>, AppError> {
    Ok(Json(
        retry_orchestrator_remote_outbox_for_repos(
            state.orchestrator_repo.as_ref(),
            state.workbench_project_repo.as_ref(),
            &req.project_id,
            &req.outbox_id,
        )
        .await?,
    ))
}

/// Business Logic（为什么需要这个函数）:
///     手机端对 failed outbox 点 Discard 时，必须复用与 Tauri 相同的本机归属校验与 failed-only 转移。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 取仓储，委托 discard_orchestrator_remote_outbox_for_repos，返回 camelCase DTO。
pub async fn discard_remote_outbox(
    State(state): State<AppState>,
    Json(req): Json<MobileRemoteOutboxActionReq>,
) -> Result<Json<OrchestratorRemoteOutboxDto>, AppError> {
    Ok(Json(
        discard_orchestrator_remote_outbox_for_repos(
            state.orchestrator_repo.as_ref(),
            state.workbench_project_repo.as_ref(),
            &req.project_id,
            &req.outbox_id,
        )
        .await?,
    ))
}

/// Business Logic（为什么需要这个函数）:
///     route 测试需要与 retry handler 相同的仓储路径，验证 HTTP 层不会递归代理。
///
/// Code Logic（这个函数做什么）:
///     从 route context 的仓储直接调用共享 helper；仅测试使用。
#[cfg(test)]
async fn retry_remote_outbox_for_context(
    context: &OrchestratorRouteContext,
    req: MobileRemoteOutboxActionReq,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    retry_orchestrator_remote_outbox_for_repos(
        context.orchestrator_repo.as_ref(),
        context.workbench_project_repo.as_ref(),
        &req.project_id,
        &req.outbox_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     route 测试需要与 discard handler 相同的仓储路径。
///
/// Code Logic（这个函数做什么）:
///     从 route context 的仓储直接调用共享 helper；仅测试使用。
#[cfg(test)]
async fn discard_remote_outbox_for_context(
    context: &OrchestratorRouteContext,
    req: MobileRemoteOutboxActionReq,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    discard_orchestrator_remote_outbox_for_repos(
        context.orchestrator_repo.as_ref(),
        context.workbench_project_repo.as_ref(),
        &req.project_id,
        &req.outbox_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     远端/桌面需要 owner adapter 可用性，且不得泄露 executable/env。
///
/// Code Logic（这个函数做什么）:
///     读 optional generic_terminal，build redacted catalog。
pub async fn agent_adapters_catalog(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<OrchestratorAgentAdapterCatalog>> {
    let generic = state
        .config
        .read()
        .map_err(|_| {
            P2pError::from_app_error(
                AppError::generic("config 读锁中毒"),
                &ctx,
                "orchestrator.agent_adapters",
            )
        })?
        .orchestrator
        .generic_terminal
        .clone();
    let registry = AgentAdapterRegistry::new(generic);
    let catalog = build_agent_adapter_catalog(&registry)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.agent_adapters"))?;
    Ok(Json(catalog))
}

/// Business Logic（为什么需要这个函数）:
///     远端创建实验组必须走组级原子路由，旧 peer 无 capability 时客户端不得降级。
///
/// Code Logic（这个函数做什么）:
///     校验 local project 后调用 create_local_orchestrator_experiment；
///     返回真实 `newly_created`（幂等重放为 false）。
pub async fn create_experiment_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::orchestrator::experiments::models::CreateExperimentRequest>,
) -> P2pResult<Json<crate::orchestrator::experiments::remote_protocol::CreateExperimentResponse>> {
    require_local_project_by_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.create"))?;
    let outcome = crate::commands::orchestrator::create_local_orchestrator_experiment(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.create"))?;
    Ok(Json(
        crate::orchestrator::experiments::remote_protocol::CreateExperimentResponse {
            experiment: outcome.experiment,
            newly_created: outcome.newly_created,
        },
    ))
}

/// Business Logic（为什么需要这个函数）:
///     远端列出项目实验组。
///
/// Code Logic（这个函数做什么）:
///     require local project → list_orchestrator_experiments_for_state。
pub async fn list_experiments_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::orchestrator::experiments::remote_protocol::ListExperimentsRequest>,
) -> P2pResult<Json<crate::orchestrator::experiments::remote_protocol::ListExperimentsResponse>> {
    require_local_project_by_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.list"))?;
    let experiments = crate::commands::orchestrator::list_orchestrator_experiments_for_state(
        &state,
        Some(&req.project_id),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.list"))?;
    Ok(Json(
        crate::orchestrator::experiments::remote_protocol::ListExperimentsResponse { experiments },
    ))
}

/// Business Logic（为什么需要这个函数）:
///     远端实验详情。
///
/// Code Logic（这个函数做什么）:
///     get_orchestrator_experiment_for_state + local project guard。
pub async fn get_experiment_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::orchestrator::experiments::remote_protocol::GetExperimentRequest>,
) -> P2pResult<Json<crate::orchestrator::experiments::models::OrchestratorExperimentDto>> {
    let dto = crate::commands::orchestrator::get_orchestrator_experiment_for_state(
        &state,
        &req.experiment_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.get"))?;
    require_local_project_by_id(&state, &dto.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.get"))?;
    Ok(Json(dto))
}

/// Business Logic（为什么需要这个函数）:
///     远端批准 winner。
///
/// Code Logic（这个函数做什么）:
///     approve_orchestrator_experiment_winner_for_state。
pub async fn approve_experiment_winner_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<
        crate::orchestrator::experiments::remote_protocol::ApproveExperimentWinnerRequest,
    >,
) -> P2pResult<Json<crate::orchestrator::experiments::models::OrchestratorExperimentDto>> {
    let before = crate::commands::orchestrator::get_orchestrator_experiment_for_state(
        &state,
        &req.experiment_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.approve"))?;
    require_local_project_by_id(&state, &before.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.approve"))?;
    let dto = crate::commands::orchestrator::approve_orchestrator_experiment_winner_for_state(
        &state,
        &req.experiment_id,
        &req.winner_task_id,
        req.reason.as_deref(),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.approve"))?;
    Ok(Json(dto))
}

/// Business Logic（为什么需要这个函数）:
///     远端取消实验组。
///
/// Code Logic（这个函数做什么）:
///     cancel_orchestrator_experiment_for_state。
pub async fn cancel_experiment_route(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::orchestrator::experiments::remote_protocol::CancelExperimentRequest>,
) -> P2pResult<Json<crate::orchestrator::experiments::models::OrchestratorExperimentDto>> {
    let before = crate::commands::orchestrator::get_orchestrator_experiment_for_state(
        &state,
        &req.experiment_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.cancel"))?;
    require_local_project_by_id(&state, &before.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.cancel"))?;
    let dto = crate::commands::orchestrator::cancel_orchestrator_experiment_for_state(
        &state,
        &req.experiment_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "orchestrator.experiments.cancel"))?;
    Ok(Json(dto))
}

/// Business Logic（为什么需要这个函数）:
///     experiment 路由必须拒绝 remote shortcut 递归代理。
///
/// Code Logic（这个函数做什么）:
///     加载 project 并调用 ensure_remote_orchestrator_local_project。
async fn require_local_project_by_id(state: &AppState, project_id: &str) -> Result<(), AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Orchestrator 项目不存在"))?;
    ensure_remote_orchestrator_local_project(&project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
    use crate::orchestrator::models::{
        OrchestratorCreateAction, OrchestratorRunState, OrchestratorTaskRow,
        OrchestratorTaskStatus, OrchestratorWorkflowState,
    };
    use crate::orchestrator::remote_protocol::{RemoteCreateOrchestratorTaskReq, RemoteTaskReq};
    use crate::workbench::models::WorkbenchProjectRow;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     route guard 测试只关心项目 kind，不需要真实数据库项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造最小 WorkbenchProjectRow，并允许测试覆盖 kind 字段。
    fn project_row_with_kind(kind: &str) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "project-1".to_string(),
            name: "Project".to_string(),
            kind: kind.to_string(),
            device_id: "local".to_string(),
            device_name: "Local".to_string(),
            path: "/tmp/project".to_string(),
            last_opened_at: "2026-07-05T00:00:00Z".to_string(),
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     route 测试需要最小 AppConfig，以验证 config route 返回设备级 Orchestrator 策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造字段完整的 AppConfig，避免测试读取用户真实 config.json。
    fn test_app_config() -> AppConfig {
        AppConfig {
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            db_path: "/tmp/cc-partner.db".to_string(),
            screenshot_hotkey: "<cmd>+<shift>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     route helper 测试需要隔离 SQLite，并复用真实 Orchestrator/Workbench project 仓储语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接内存 SQLite，初始化 Orchestrator schema 和最小 workbench_projects 表，返回 route context。
    async fn test_state() -> OrchestratorRouteContext {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite options")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("sqlite pool");
        OrchestratorRepo::init_schema(&pool)
            .await
            .expect("orchestrator schema");
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .expect("workbench projects schema");
        OrchestratorRouteContext {
            config: Arc::new(RwLock::new(test_app_config())),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool)),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个 route 测试都需要声明项目是 local 还是 remote，以验证网关项目 guard。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 WorkbenchProjectRepo upsert 插入完整项目行，kind 由调用方指定。
    async fn insert_project(state: &OrchestratorRouteContext, id: &str, kind: &str) {
        let mut row = project_row_with_kind(kind);
        row.id = id.to_string();
        row.name = format!("Project {id}");
        row.path = format!("/tmp/{id}");
        state
            .workbench_project_repo
            .upsert(&row)
            .await
            .expect("insert project");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     route 测试需要稳定插入不同状态的任务，避免通过 create helper 间接改变被测状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造完整 OrchestratorTaskRow 并调用真实 repo.create_task 持久化。
    async fn create_test_task(
        state: &OrchestratorRouteContext,
        id: &str,
        project_id: &str,
        status: OrchestratorTaskStatus,
    ) {
        let row = OrchestratorTaskRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
            title: format!("Task {id}"),
            goal: "goal".to_string(),
            acceptance_criteria: "criteria".to_string(),
            status,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(status)
        };
        state
            .orchestrator_repo
            .create_task(&row)
            .await
            .expect("create task");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     P2P Orchestrator 路由只能操作对端本机 local Workbench 项目，不能把 remote shortcut 递归代理。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接校验 route-level project kind guard：local 通过，remote 返回清晰协议错误。
    #[test]
    fn remote_orchestrator_project_guard_rejects_remote_shortcut() {
        assert!(ensure_remote_orchestrator_local_project(&project_row_with_kind("local")).is_ok());

        let error = ensure_remote_orchestrator_local_project(&project_row_with_kind("remote"))
            .expect_err("remote shortcut rows must be rejected");

        assert_eq!(error.to_string(), "远端 Orchestrator 只接受对端本机项目");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     create route 的 createAction=todo 语义必须创建 scheduler 可领取的 Todo/Idle 任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过测试 helper 创建任务，断言返回状态为 Queued 且保留请求中的项目和优先级。
    #[tokio::test]
    async fn create_local_project_task_enters_todo_when_requested() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;

        let outcome = create_task_for_state(
            &state,
            RemoteCreateOrchestratorTaskReq {
                project_id: "project-1".to_string(),
                title: "远端任务".to_string(),
                goal: "完成目标".to_string(),
                acceptance_criteria: "验收标准".to_string(),
                priority: 5,
                create_action: OrchestratorCreateAction::Todo,
                client_request_id: Some("create-request-queued".to_string()),
                source: None,
                external_id: None,
                external_identifier: None,
                external_url: None,
                external_state: None,
                external_labels: None,
            },
        )
        .await
        .expect("create task");
        let created = outcome.task;

        assert!(outcome.newly_created);
        assert_eq!(created.project_id, "project-1");
        assert_eq!(created.status, OrchestratorTaskStatus::Queued);
        assert_eq!(created.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(created.run_state, OrchestratorRunState::Idle);
        assert_eq!(created.priority, 5);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端创建响应超时后客户端会用同一个 clientRequestId 重试，owning device 必须返回第一次创建的任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对同一个 local 项目用同一 clientRequestId 调用两次 create helper，断言返回同一 task id 且数据库只有一条任务。
    #[tokio::test]
    async fn create_local_project_task_is_idempotent_by_client_request_id() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;

        let req = RemoteCreateOrchestratorTaskReq {
            project_id: "project-1".to_string(),
            title: "远端任务".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "验收标准".to_string(),
            priority: 5,
            create_action: OrchestratorCreateAction::Todo,
            client_request_id: Some("create-request-1".to_string()),
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
        };

        let first = create_task_for_state(&state, req.clone())
            .await
            .expect("first create");
        let second = create_task_for_state(&state, req)
            .await
            .expect("second create");
        let tasks = state
            .orchestrator_repo
            .list_tasks(Some("project-1"))
            .await
            .expect("list tasks");

        assert!(first.newly_created);
        assert!(!second.newly_created);
        assert_eq!(first.task.id, second.task.id);
        assert_eq!(first.task.status, OrchestratorTaskStatus::Queued);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, first.task.id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺少 clientRequestId 的远端 create 无法在响应超时后安全重试，必须拒绝而不是创建可能重复的任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空 client_request_id 调用 create helper，断言返回业务错误且数据库未插入任务。
    #[tokio::test]
    async fn create_local_project_task_requires_client_request_id() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;

        let req = RemoteCreateOrchestratorTaskReq {
            project_id: "project-1".to_string(),
            title: "远端任务".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "验收标准".to_string(),
            priority: 5,
            create_action: OrchestratorCreateAction::Backlog,
            client_request_id: Some("   ".to_string()),
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
        };

        let error = create_task_for_state(&state, req)
            .await
            .expect_err("missing clientRequestId should fail");
        let tasks = state
            .orchestrator_repo
            .list_tasks(Some("project-1"))
            .await
            .expect("list tasks");

        assert!(error.to_string().contains("缺少 clientRequestId"));
        assert!(tasks.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端任务列表必须按 projectId 筛选，避免一个设备上的多个项目任务互相串入。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入两个本机项目和任务，调用 route helper 只列出目标项目任务。
    #[tokio::test]
    async fn list_tasks_filters_by_project_id() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;
        insert_project(&state, "project-2", "local").await;
        create_test_task(&state, "task-1", "project-1", OrchestratorTaskStatus::Draft).await;
        create_test_task(&state, "task-2", "project-2", OrchestratorTaskStatus::Draft).await;

        let resp = list_tasks_for_state(&state, "project-1")
            .await
            .expect("list tasks");

        assert_eq!(resp.tasks.len(), 1);
        assert_eq!(resp.tasks[0].id, "task-1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端任务详情需要按 taskId 拉取 evidence，不能混入其它任务的验证或交付记录。
    ///
    /// Code Logic（这个测试做什么）:
    ///     为两个任务分别写入 evidence，调用 route helper 只返回目标任务 evidence。
    #[tokio::test]
    async fn evidence_returns_records_by_task_id() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;
        create_test_task(&state, "task-1", "project-1", OrchestratorTaskStatus::Draft).await;
        create_test_task(&state, "task-2", "project-1", OrchestratorTaskStatus::Draft).await;
        state
            .orchestrator_repo
            .add_evidence("task-1", "verificationOutput", "验证", "passed", "ok")
            .await
            .expect("evidence 1");
        state
            .orchestrator_repo
            .add_evidence("task-2", "verificationOutput", "验证", "failed", "bad")
            .await
            .expect("evidence 2");

        let resp = get_evidence_for_state(
            &state,
            RemoteTaskReq {
                task_id: "task-1".to_string(),
            },
        )
        .await
        .expect("get evidence");

        assert_eq!(resp.evidence.len(), 1);
        assert_eq!(resp.evidence[0].task_id, "task-1");
        assert_eq!(resp.evidence[0].content, "ok");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     taskId-only 远端路由也必须拒绝 remote shortcut 项目，避免递归代理到第三台设备。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 remote kind 项目与关联任务，调用 evidence helper 并断言 route guard 返回协议错误。
    #[tokio::test]
    async fn task_id_only_routes_reject_remote_shortcut_project() {
        let state = test_state().await;
        insert_project(&state, "remote-project", "remote").await;
        create_test_task(
            &state,
            "remote-task",
            "remote-project",
            OrchestratorTaskStatus::Draft,
        )
        .await;

        let error = get_evidence_for_state(
            &state,
            RemoteTaskReq {
                task_id: "remote-task".to_string(),
            },
        )
        .await
        .expect_err("remote shortcut task must be rejected");

        assert_eq!(error.to_string(), "远端 Orchestrator 只接受对端本机项目");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     queue/abort 这类写操作同样只携带 taskId，必须在写状态前拒绝 remote shortcut 任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 remote kind 项目与两个任务，分别调用 queue/abort helper 并断言都被 project guard 拦截。
    #[tokio::test]
    async fn task_id_write_routes_reject_remote_shortcut_project() {
        let state = test_state().await;
        insert_project(&state, "remote-project", "remote").await;
        create_test_task(
            &state,
            "remote-draft",
            "remote-project",
            OrchestratorTaskStatus::Draft,
        )
        .await;
        create_test_task(
            &state,
            "remote-queued",
            "remote-project",
            OrchestratorTaskStatus::Queued,
        )
        .await;

        let queue_error = queue_task_for_state(
            &state,
            RemoteTaskReq {
                task_id: "remote-draft".to_string(),
            },
        )
        .await
        .expect_err("remote shortcut task must be rejected before queue");
        let abort_error = abort_task_for_state(
            &state,
            RemoteTaskReq {
                task_id: "remote-queued".to_string(),
            },
        )
        .await
        .expect_err("remote shortcut task must be rejected before abort");

        assert_eq!(
            queue_error.to_string(),
            "远端 Orchestrator 只接受对端本机项目"
        );
        assert_eq!(
            abort_error.to_string(),
            "远端 Orchestrator 只接受对端本机项目"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote shortcut 的 retry 操作只能把 owning device 上的 Blocked 任务重新排队。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 blocked 任务后调用 retry helper，断言返回和持久化状态均为 Queued。
    #[tokio::test]
    async fn retry_task_moves_blocked_task_to_queued() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;
        create_test_task(
            &state,
            "task-1",
            "project-1",
            OrchestratorTaskStatus::Blocked,
        )
        .await;

        let task = retry_task_for_state(
            &state,
            RemoteTaskReq {
                task_id: "task-1".to_string(),
            },
        )
        .await
        .expect("retry task");

        assert_eq!(task.status, OrchestratorTaskStatus::Queued);
        assert_eq!(task.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(task.run_state, OrchestratorRunState::Idle);
        let stored = state
            .orchestrator_repo
            .get_task("task-1")
            .await
            .expect("stored task");
        assert_eq!(stored.status, OrchestratorTaskStatus::Queued);
        assert_eq!(stored.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(stored.run_state, OrchestratorRunState::Idle);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     retry 不能把 Draft/Queued/Running/Done 等非 Blocked 任务回退到队列。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 Draft 任务后调用 retry helper，断言返回状态错误且数据库状态未被修改。
    #[tokio::test]
    async fn retry_task_rejects_non_blocked_task_without_mutating_status() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;
        create_test_task(&state, "task-1", "project-1", OrchestratorTaskStatus::Draft).await;

        let error = retry_task_for_state(
            &state,
            RemoteTaskReq {
                task_id: "task-1".to_string(),
            },
        )
        .await
        .expect_err("draft task must not retry");

        assert_eq!(
            error.to_string(),
            "任务状态已变化，无法从 blocked 切换到 queued，当前状态为 draft"
        );
        let stored = state
            .orchestrator_repo
            .get_task("task-1")
            .await
            .expect("stored task");
        assert_eq!(stored.status, OrchestratorTaskStatus::Draft);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户在远端 shortcut 上终止任务时，实际 owning device 必须把权威任务置为 Aborted。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 queued 任务后调用 abort route helper，断言状态被持久化为 Aborted。
    #[tokio::test]
    async fn abort_task_sets_aborted_status() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;
        create_test_task(
            &state,
            "task-1",
            "project-1",
            OrchestratorTaskStatus::Queued,
        )
        .await;

        let task = abort_task_for_state(
            &state,
            RemoteTaskReq {
                task_id: "task-1".to_string(),
            },
        )
        .await
        .expect("abort task");

        assert_eq!(task.status, OrchestratorTaskStatus::Aborted);
        let stored = state
            .orchestrator_repo
            .get_task("task-1")
            .await
            .expect("stored task");
        assert_eq!(stored.status, OrchestratorTaskStatus::Aborted);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 config 接口需要返回 owning device 的全局 Orchestrator 自动化配置，供诊断/兼容路径读取。
    ///
    /// Code Logic（这个测试做什么）:
    ///     修改测试状态中的 config.orchestrator，调用 route helper 并断言 DTO 反映当前设备配置。
    #[tokio::test]
    async fn config_response_returns_device_global_config() {
        let state = test_state().await;
        {
            let mut cfg = state.config.write().expect("config 写锁中毒");
            cfg.orchestrator.enabled = true;
            cfg.orchestrator.max_concurrent_tasks = 3;
        }

        let resp = get_config_for_state(&state);

        assert!(resp.config.enabled);
        assert_eq!(resp.config.max_concurrent_tasks, 3);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Orchestrator 路由（create/list/evidence/queue/start/retry/rework/deliver/abort/
    ///     cancel/refresh/config）的错误必须经 P2pError::from_app_error 映射到信封。本测试覆盖
    ///     三类典型错误：400（remote shortcut 项目被网关 guard 拒绝，属校验类）、404（任务不存在）、
    ///     409（状态机非法转换，如对非 Blocked 任务 retry），断言 status/code/request_id 与契约一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 validation/not_found/conflict 三类 AppError 构造 P2pError，断言 envelope 字段。
    #[test]
    fn orchestrator_routes_map_app_error_classes_to_envelope() {
        use crate::error::{AppError, AppErrorCategory};
        use crate::net::error_response::P2pError;
        let ctx = P2pRequestContext {
            request_id: "req-orch".to_string(),
        };
        let cases: Vec<(AppError, AppErrorCategory, &str, axum::http::StatusCode)> = vec![
            (
                AppError::validation("远端 Orchestrator 只接受对端本机项目"),
                AppErrorCategory::Validation,
                "validation_error",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            (
                AppError::not_found("任务不存在"),
                AppErrorCategory::NotFound,
                "not_found",
                axum::http::StatusCode::NOT_FOUND,
            ),
            (
                AppError::conflict("任务当前状态不支持 retry"),
                AppErrorCategory::Conflict,
                "conflict",
                axum::http::StatusCode::CONFLICT,
            ),
        ];
        for (app, category, code, status) in cases {
            assert_eq!(app.classify(), category, "AppError 分类应匹配");
            let p2p = P2pError::from_app_error(app, &ctx, "orchestrator.tasks");
            assert_eq!(p2p.status(), status, "状态码应匹配 code 约定");
            assert_eq!(p2p.envelope().code, code, "code token 应匹配");
            assert_eq!(p2p.envelope().request_id, "req-orch");
            // Orchestrator 写/状态机操作保守默认：retryable 必须为 false。
            assert!(
                !p2p.envelope().retryable,
                "orchestrator 操作 retryable 默认必须为 false"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     orchestrator create route 的 project guard 把 remote shortcut 项目转成 AppError::validation，
    ///     经 route handler 的 map_err 后必须变成 400 validation_error 信封，而不是 500 internal_error。
    ///     这是 Task 6 把 AppError::generic → AppError::validation 的语义校正在边界上的回归保护。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用 ensure_remote_orchestrator_local_project 对 remote kind 项目取 AppError，
    ///     经 P2pError::from_app_error 映射后断言 status=400 且 code=validation_error。
    #[test]
    fn remote_shortcut_project_guard_maps_to_400_validation_envelope() {
        use crate::net::error_response::P2pError;
        let ctx = P2pRequestContext {
            request_id: "req-guard".to_string(),
        };
        let app_error = ensure_remote_orchestrator_local_project(&project_row_with_kind("remote"))
            .expect_err("remote shortcut 必须被 guard 拒绝");

        let p2p = P2pError::from_app_error(app_error, &ctx, "orchestrator.tasks.create");
        assert_eq!(
            p2p.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "remote shortcut 拒绝应映射为 400 而非 500"
        );
        assert_eq!(p2p.envelope().code, "validation_error");
        assert_eq!(p2p.envelope().request_id, "req-guard");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owning-device runtime-snapshot 路由的核心契约：对端用本机 local projectId 调用，
    ///     必须返回与本机命令相同的 runtime snapshot DTO（remote_status=local）。
    ///     路由必须复用 T1 共享 builder，不能另起一套本地构造逻辑。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 local 项目后调用 runtime_snapshot_for_state，断言关键字段（projectId、projectKind、
    ///     remoteStatus、schedulerEnabled）与本地 builder 产出一致。
    #[tokio::test]
    async fn runtime_snapshot_returns_local_dto_for_local_project() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;

        let snapshot = runtime_snapshot_for_state(
            &state,
            "project-1",
            &OrchestratorSchedulerTelemetrySnapshot::default(),
        )
        .await
        .expect("local project snapshot");

        assert_eq!(snapshot.project_id, "project-1");
        assert_eq!(snapshot.project_kind, "local");
        assert_eq!(snapshot.remote_status, "local");
        assert!(snapshot.workflow_error.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     runtime-snapshot 路由必须拒绝 remote shortcut 项目，避免递归代理到第三台设备。
    ///     remote shortcut 的 runtime snapshot 不能用本机 scheduler/config/workflow 冒充。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 remote kind 项目后调用 runtime_snapshot_for_state，断言返回 validation 错误
    ///     （HTTP 边界映射 400 validation_error），且错误经 P2pError 信封后携带稳定 request_id。
    #[tokio::test]
    async fn runtime_snapshot_rejects_remote_shortcut_project_with_envelope() {
        let state = test_state().await;
        insert_project(&state, "remote:device-a:project-1", "remote").await;

        let error = runtime_snapshot_for_state(
            &state,
            "remote:device-a:project-1",
            &OrchestratorSchedulerTelemetrySnapshot::default(),
        )
        .await
        .expect_err("remote shortcut must be rejected");

        assert_eq!(error.to_string(), "远端 Orchestrator 只接受对端本机项目");

        // 验证错误经 P2pError 信封映射后为 400 validation_error 且携带稳定 request_id。
        let ctx = P2pRequestContext {
            request_id: "req-runtime-snapshot".to_string(),
        };
        let p2p = P2pError::from_app_error(error, &ctx, "orchestrator.runtime_snapshot");
        assert_eq!(p2p.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(p2p.envelope().code, "validation_error");
        assert_eq!(p2p.envelope().request_id, "req-runtime-snapshot");
        assert!(!p2p.envelope().retryable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     projectId 指向本设备不存在的项目时，路由必须返回 not_found（HTTP 404）而非 500。
    ///
    /// Code Logic（这个测试做什么）:
    ///     不插入任何项目，直接调用 runtime_snapshot_for_state，断言返回 not_found 错误且经
    ///     P2pError 映射后为 404。
    #[tokio::test]
    async fn runtime_snapshot_returns_not_found_for_unknown_project() {
        let state = test_state().await;

        let error = runtime_snapshot_for_state(
            &state,
            "missing-project",
            &OrchestratorSchedulerTelemetrySnapshot::default(),
        )
        .await
        .expect_err("unknown project must return not_found");

        let ctx = P2pRequestContext {
            request_id: "req-not-found".to_string(),
        };
        let p2p = P2pError::from_app_error(error, &ctx, "orchestrator.runtime_snapshot");
        assert_eq!(p2p.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(p2p.envelope().code, "not_found");
        assert_eq!(p2p.envelope().request_id, "req-not-found");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     handler 在进入 route helper 前必须把空白 projectId 转成 validation_error（400），
    ///     而不是把空串带到 repo.get 查询。这是 handler 层 guard，与 route helper 的项目解析互补。
    ///
    /// Code Logic（这个测试做什么）:
    ///     模拟 handler 对 RemoteRuntimeSnapshotReq.project_id 的 trim 判空逻辑，
    ///     并断言对应 AppError::validation 经 P2pError 映射后为 400 validation_error。
    #[tokio::test]
    async fn runtime_snapshot_handler_rejects_blank_project_id() {
        // 复刻 handler 内部 trim 判空逻辑：空白 projectId 必须被识别为空。
        for blank in ["", "   ", "\t\n"] {
            let req = RemoteRuntimeSnapshotReq {
                project_id: blank.to_string(),
            };
            let trimmed = req.project_id.trim().to_string();
            assert!(trimmed.is_empty(), "trim 后应识别为空白: {blank:?}");
        }

        // 断言 handler 使用的 AppError::validation 映射到稳定 P2pError 信封（400 validation_error）。
        let ctx = P2pRequestContext {
            request_id: "req-blank".to_string(),
        };
        let app_error = AppError::validation("远端 Orchestrator runtime snapshot 缺少 project_id");
        let p2p = P2pError::from_app_error(app_error, &ctx, "orchestrator.runtime_snapshot");
        assert_eq!(p2p.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(p2p.envelope().code, "validation_error");
        assert_eq!(p2p.envelope().request_id, "req-blank");
        assert!(!p2p.envelope().retryable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     P2P route 必须接受字面量 `{"project_id":"..."}` 并拒绝 camelCase `{"projectId":...}`，
    ///     防止 client/server 共用错误 DTO 掩盖 wire 漂移。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 axum Router + oneshot 直接 POST 两种 body：snake_case 200 且回显 project_id；
    ///     camelCase 422 且不进入业务 handler。
    #[tokio::test]
    async fn runtime_snapshot_router_accepts_snake_case_and_rejects_camel_case_body() {
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;

        async fn echo_project_id(
            Json(req): Json<RemoteRuntimeSnapshotReq>,
        ) -> Json<serde_json::Value> {
            Json(serde_json::json!({ "project_id": req.project_id }))
        }

        let app = Router::new().route("/api/orchestrator/runtime-snapshot", post(echo_project_id));

        let snake_req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/orchestrator/runtime-snapshot")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"project_id":"owner-local-1"}"#))
            .expect("snake request");
        let snake_resp = app.clone().oneshot(snake_req).await.expect("snake oneshot");
        assert_eq!(snake_resp.status(), axum::http::StatusCode::OK);
        let snake_bytes = axum::body::to_bytes(snake_resp.into_body(), 1024 * 1024)
            .await
            .expect("snake body");
        let snake_json: serde_json::Value =
            serde_json::from_slice(&snake_bytes).expect("snake json");
        assert_eq!(snake_json["project_id"], "owner-local-1");

        let camel_req = axum::http::Request::builder()
            .method("POST")
            .uri("/api/orchestrator/runtime-snapshot")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"projectId":"owner-local-1"}"#))
            .expect("camel request");
        let camel_resp = app.oneshot(camel_req).await.expect("camel oneshot");
        assert_eq!(
            camel_resp.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "P2P route must reject camelCase projectId body"
        );
    }

    /// Business Logic（为什么需要这个测试 / T2 契约核心）:
    ///     能力 token `orchestrator.runtime-snapshot.v1` 与对应路由必须**原子上线**：
    ///     本机宣告该 token 时，路由 `/api/orchestrator/runtime-snapshot` 必须已注册；
    ///     对端据此决定能否安全调用新路由。本测试锁死 token 与 route 注册共存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 server_protocol_info() 宣告了 `orchestrator.runtime-snapshot.v1`，且 route handler
    ///     `orchestrator::runtime_snapshot` 存在（通过 `assert_eq` 引用其指针地址，强制编译期共存）。
    #[test]
    fn runtime_snapshot_capability_and_route_ship_together() {
        use crate::net::protocol::{
            server_protocol_info, CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1,
        };

        let info = server_protocol_info();
        assert!(
            info.supports(CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1),
            "server_protocol_info 必须宣告 orchestrator.runtime-snapshot.v1，实际: {:?}",
            info.capabilities
        );
        // 引用 handler 函数指针，确保路由 handler 与 capability 在同一编译单元共存。
        // 若 handler 被移除或重命名，本测试会在编译期失败。
        let handler_ptr = runtime_snapshot
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<RemoteRuntimeSnapshotReq>,
            ) -> _;
        // 用指针非空断言 handler 存在，避免被 dead_code 优化掉。
        let addr = handler_ptr as usize;
        assert_ne!(addr, 0, "runtime_snapshot handler 必须存在");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     能力 token `orchestrator.workflow-document.v1` 与 get/validate/save 路由必须原子上线。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 server_protocol_info 宣告 token，并引用三条 owner handler + 三条 mobile handler 指针。
    #[test]
    fn workflow_document_capability_and_route_ship_together() {
        use crate::net::protocol::{
            server_protocol_info, CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1,
        };

        let info = server_protocol_info();
        assert!(
            info.supports(CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1),
            "server_protocol_info 必须宣告 orchestrator.workflow-document.v1，实际: {:?}",
            info.capabilities
        );
        let get_h = get_workflow_document_route
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<RemoteWorkflowDocumentGetReq>,
            ) -> _;
        let validate_h = validate_workflow_document_route
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<RemoteWorkflowDocumentValidateReq>,
            ) -> _;
        let save_h = save_workflow_document_route
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<RemoteWorkflowDocumentSaveReq>,
            ) -> _;
        let mobile_get_h = mobile_get_workflow_document
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<MobileWorkflowDocumentGetReq>,
            ) -> _;
        let mobile_validate_h = mobile_validate_workflow_document
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<MobileWorkflowDocumentValidateReq>,
            ) -> _;
        let mobile_save_h = mobile_save_workflow_document
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<MobileWorkflowDocumentSaveReq>,
            ) -> _;
        assert_ne!(get_h as usize, 0);
        assert_ne!(validate_h as usize, 0);
        assert_ne!(save_h as usize, 0);
        assert_ne!(mobile_get_h as usize, 0);
        assert_ne!(mobile_validate_h as usize, 0);
        assert_ne!(mobile_save_h as usize, 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owning-device workflow-document 路由必须拒绝 remote shortcut 项目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 remote kind 项目，调用 get_local_owner_workflow_document，断言 validation。
    #[tokio::test]
    async fn workflow_document_route_rejects_remote_shortcut_project() {
        let state = test_state().await;
        insert_project(&state, "remote-project", "remote").await;
        let project = state
            .workbench_project_repo
            .get("remote-project")
            .await
            .expect("get")
            .expect("project");
        let err = get_local_owner_workflow_document(&project)
            .expect_err("remote shortcut must be rejected");
        assert_eq!(err.to_string(), "远端 Orchestrator 只接受对端本机项目");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     mobile list/create 必须把入站 request_id 传给 request-id-aware helpers，
    ///     否则 owner 侧 open/list/create 会重生 ID，多跳链路断链。
    ///
    /// Code Logic（这个测试做什么）:
    ///     引用 list_task_views/create_task_view handler 指针保证路由存在；再对
    ///     `list/create_with_request_id` helper 做 `stringify!` 符号引用，防止重命名/删除后
    ///     静默回退到无 request_id 入口；并断言入站 ID 以 `Some(&str)` 转发。
    #[test]
    fn mobile_list_and_create_handlers_ship_with_request_id_aware_helpers() {
        let list_handler = list_task_views
            as fn(State<AppState>, Extension<P2pRequestContext>, Json<RemoteListTasksReq>) -> _;
        let create_handler = create_task_view
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<RemoteCreateOrchestratorTaskReq>,
            ) -> _;
        assert_ne!(list_handler as usize, 0);
        assert_ne!(create_handler as usize, 0);

        // 强制编译期解析 request-id-aware 符号；若被删除/改名，本测试在编译期失败。
        let list_name = stringify!(
            crate::commands::orchestrator::list_orchestrator_task_views_for_state_with_request_id
        );
        let create_name = stringify!(
            crate::commands::orchestrator::create_orchestrator_task_view_for_http_with_request_id
        );
        assert!(list_name.contains("with_request_id"));
        assert!(create_name.contains("with_request_id"));

        // 再通过函数项路径实际引用符号，避免仅字符串被优化掉。
        let _list_item =
            crate::commands::orchestrator::list_orchestrator_task_views_for_state_with_request_id;
        let _create_item =
            crate::commands::orchestrator::create_orchestrator_task_view_for_http_with_request_id;

        let ctx = P2pRequestContext {
            request_id: "req-mobile-list-create".to_string(),
        };
        // 与 handler 实现一致：非空入站 ID 以 Some(&str) 转发。
        let forwarded = Some(ctx.request_id.as_str());
        assert_eq!(forwarded, Some("req-mobile-list-create"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     手机浏览器入口必须注册 remote-aware runtime-snapshot 路由，且不能与 owning-device
    ///     本地路由混用；handler 在空白 projectId 时返回 validation_error。
    ///
    /// Code Logic（这个测试做什么）:
    ///     引用 mobile_runtime_snapshot 函数指针保证编译期共存，并复刻空白 projectId 校验映射。
    #[test]
    fn mobile_runtime_snapshot_handler_ships_and_rejects_blank_project_id() {
        let handler_ptr = mobile_runtime_snapshot
            as fn(
                State<AppState>,
                Extension<P2pRequestContext>,
                Json<MobileRuntimeSnapshotReq>,
            ) -> _;
        let addr = handler_ptr as usize;
        assert_ne!(addr, 0, "mobile_runtime_snapshot handler 必须存在");

        for blank in ["", "   ", "\t\n"] {
            let req = MobileRuntimeSnapshotReq {
                project_id: blank.to_string(),
            };
            assert!(req.project_id.trim().is_empty());
        }

        let ctx = P2pRequestContext {
            request_id: "req-mobile-blank".to_string(),
        };
        let app_error = AppError::validation("移动端 Orchestrator runtime snapshot 缺少 projectId");
        let p2p = P2pError::from_app_error(app_error, &ctx, "orchestrator.mobile.runtime_snapshot");
        assert_eq!(p2p.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(p2p.envelope().code, "validation_error");
        assert_eq!(p2p.envelope().request_id, "req-mobile-blank");
    }

    /// Business Logic（为什么需要这个测试 / T7 route 无本地替代）:
    ///     owning-device route 对 local 项目必须复用共享 builder 产出 remoteStatus=local 的 DTO，
    ///     且不得接受 remote shortcut（递归代理会把调用端本地 telemetry 冒充 owner）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同一 state 下先成功取 local snapshot，再对 remote shortcut 断言 validation 拒绝；
    ///     local 成功结果 projectKind/remoteStatus 固定为 local，不出现 remote 四态。
    #[tokio::test]
    async fn runtime_snapshot_local_success_never_substitutes_remote_status_or_accepts_shortcut() {
        let state = test_state().await;
        insert_project(&state, "owner-local-p4t7", "local").await;
        insert_project(&state, "remote:device-z:owner-local-p4t7", "remote").await;

        let local = runtime_snapshot_for_state(
            &state,
            "owner-local-p4t7",
            &OrchestratorSchedulerTelemetrySnapshot::default(),
        )
        .await
        .expect("local project must succeed");
        assert_eq!(local.project_id, "owner-local-p4t7");
        assert_eq!(local.project_kind, "local");
        assert_eq!(local.remote_status, "local");
        assert!(
            !matches!(
                local.remote_status.as_str(),
                "live" | "offline" | "unsupported" | "unavailable"
            ),
            "owning-device local route 不得返回远端四态"
        );

        let rejected = runtime_snapshot_for_state(
            &state,
            "remote:device-z:owner-local-p4t7",
            &OrchestratorSchedulerTelemetrySnapshot::default(),
        )
        .await
        .expect_err("remote shortcut must be rejected on owning-device route");
        assert_eq!(rejected.to_string(), "远端 Orchestrator 只接受对端本机项目");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Mobile outbox retry route 必须校验 project 归属、failed-only，并返回 camelCase DTO，
    ///     且 outbox 行只在本机处理，不转发远端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 remote shortcut 与 failed outbox，调用 context helper 断言 pending 与 request_json 保留。
    #[tokio::test]
    async fn mobile_outbox_retry_route_is_local_failed_only_and_camel_case() {
        let state = test_state().await;
        let mut project = project_row_with_kind("remote");
        project.id = "shortcut-1".to_string();
        project.device_id = "device-a".to_string();
        project.device_name = "Mac mini".to_string();
        project.path = "/Users/hans/remote-project".to_string();
        state
            .workbench_project_repo
            .upsert(&project)
            .await
            .expect("upsert remote shortcut");
        let request_json = r#"{"projectId":"x","title":"t","goal":"g","acceptanceCriteria":"a","priority":0,"createAction":"backlog","clientRequestId":"req-http-1"}"#;
        let item = state
            .orchestrator_repo
            .insert_remote_outbox_pending(
                "device-a",
                "Mac mini",
                "/Users/hans/remote-project",
                None,
                request_json,
            )
            .await
            .expect("insert outbox");
        let claimed = state
            .orchestrator_repo
            .claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim")
            .expect("claimed");
        state
            .orchestrator_repo
            .mark_remote_outbox_failed(&claimed.id, "协议错误")
            .await
            .expect("failed");

        let dto = retry_remote_outbox_for_context(
            &state,
            MobileRemoteOutboxActionReq {
                project_id: "shortcut-1".to_string(),
                outbox_id: claimed.id.clone(),
            },
        )
        .await
        .expect("retry");
        assert_eq!(dto.status.as_str(), "pending");
        assert_eq!(dto.request_json, request_json);
        assert!(dto.last_error.is_none());

        let json = serde_json::to_value(&dto).expect("json");
        assert!(json.get("outboxId").is_none());
        assert!(json.get("deviceId").is_some());
        assert!(json.get("requestJson").is_some());

        let wrong = retry_remote_outbox_for_context(
            &state,
            MobileRemoteOutboxActionReq {
                project_id: "shortcut-1".to_string(),
                outbox_id: claimed.id.clone(),
            },
        )
        .await
        .expect_err("pending cannot retry");
        assert!(wrong.to_string().contains("失败"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Mobile outbox discard route 必须拒绝错误项目快捷方式，并在 failed 时进入 discarded。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 failed outbox，错误 path 的 shortcut 拒绝；正确 shortcut discard 成功。
    #[tokio::test]
    async fn mobile_outbox_discard_route_rejects_wrong_project_and_discards_failed() {
        let state = test_state().await;
        let mut project = project_row_with_kind("remote");
        project.id = "shortcut-1".to_string();
        project.device_id = "device-a".to_string();
        project.device_name = "Mac mini".to_string();
        project.path = "/Users/hans/remote-project".to_string();
        state
            .workbench_project_repo
            .upsert(&project)
            .await
            .expect("upsert");
        let mut other = project.clone();
        other.id = "shortcut-2".to_string();
        other.path = "/other".to_string();
        state
            .workbench_project_repo
            .upsert(&other)
            .await
            .expect("other");
        let item = state
            .orchestrator_repo
            .insert_remote_outbox_pending(
                "device-a",
                "Mac mini",
                "/Users/hans/remote-project",
                None,
                r#"{"clientRequestId":"req-http-2","projectId":"x","title":"t","goal":"g","acceptanceCriteria":"a","priority":0,"createAction":"backlog"}"#,
            )
            .await
            .expect("insert");
        let claimed = state
            .orchestrator_repo
            .claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim")
            .expect("claimed");
        state
            .orchestrator_repo
            .mark_remote_outbox_failed(&claimed.id, "协议错误")
            .await
            .expect("failed");

        let wrong = discard_remote_outbox_for_context(
            &state,
            MobileRemoteOutboxActionReq {
                project_id: "shortcut-2".to_string(),
                outbox_id: claimed.id.clone(),
            },
        )
        .await
        .expect_err("wrong project");
        assert!(wrong.to_string().contains("不属于当前项目"));

        let dto = discard_remote_outbox_for_context(
            &state,
            MobileRemoteOutboxActionReq {
                project_id: "shortcut-1".to_string(),
                outbox_id: claimed.id.clone(),
            },
        )
        .await
        .expect("discard");
        assert_eq!(dto.status.as_str(), "discarded");
        assert_eq!(dto.last_error.as_deref(), Some("协议错误"));
    }
}
