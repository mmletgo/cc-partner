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
    build_orchestrator_task_row, create_orchestrator_task_view_for_http,
    list_orchestrator_task_views_for_state, CreateOrchestratorTaskRequest, OrchestratorTaskViewDto,
};
use crate::commands::prompt_optimizer::{
    local_complete_orchestrator_task_prompt, OrchestratorTaskPromptCompletionDto,
};
use crate::config::AppConfig;
use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
};
use crate::orchestrator::outbox::open_remote_project_for_shortcut;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::{
    RemoteCompleteOrchestratorTaskPromptReq, RemoteCreateOrchestratorTaskReq, RemoteListTasksReq,
    RemoteOrchestratorConfigResp, RemoteOrchestratorEvidenceResp, RemoteOrchestratorTaskListResp,
    RemoteTaskReq,
};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::storage::WorkbenchProjectRepo;
use crate::workbench::models::WorkbenchProjectRow;
use axum::extract::State;
use axum::Json;
use serde::Serialize;
use std::sync::{Arc, RwLock};

/// Orchestrator 远端 route 需要的共享状态子集。
///
/// Business Logic（为什么需要这个结构体）:
///     HTTP handler 生产态从 AppState 取依赖，单测只需要最小仓储和配置，不应强行构造完整 Tauri AppHandle。
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
///     检查项目 row 的 kind 是否为 local；非 local 返回清晰协议错误。
fn ensure_remote_orchestrator_local_project(project: &WorkbenchProjectRow) -> Result<(), AppError> {
    if project.kind != "local" {
        return Err(AppError::generic("远端 Orchestrator 只接受对端本机项目"));
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
///     本机 remote shortcut 创建任务时，owning device 需要生成权威任务行，并可按 queue=true 立即入队。
///
/// Code Logic（这个函数做什么）:
///     确认 projectId 为 local 后要求非空 clientRequestId，再复用命令层 row builder 创建 Draft；
///     repo 事务按 clientRequestId 保证重复请求返回同一任务。
async fn create_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteCreateOrchestratorTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    ensure_remote_orchestrator_local_project_id(state, &req.project_id).await?;
    let queue = req.queue;
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
    })?;
    let created = state
        .orchestrator_repo
        .create_remote_task_for_client_request(&client_request_id, &row, queue)
        .await?;
    Ok(OrchestratorTaskDto::from(created))
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

/// 终止远端任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 remote shortcut 终止任务时，owning device 应把权威任务置为 Aborted 并保留现场。
///
/// Code Logic（这个函数做什么）:
///     确认任务所属 local 项目后调用 repo.set_task_status 写 Aborted，保持 worktree/session 字段不变。
async fn abort_task_for_state(
    state: &OrchestratorRouteContext,
    req: RemoteTaskReq,
) -> Result<OrchestratorTaskDto, AppError> {
    get_local_project_task(state, &req.task_id).await?;
    let task = state
        .orchestrator_repo
        .set_task_status(&req.task_id, OrchestratorTaskStatus::Aborted, None)
        .await?;
    Ok(OrchestratorTaskDto::from(task))
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
///
/// Code Logic（这个函数做什么）:
///     接收 JSON 请求体，构造 route context 后委托 create_task_for_state。
pub async fn create_task(
    State(state): State<AppState>,
    Json(req): Json<RemoteCreateOrchestratorTaskReq>,
) -> Result<Json<OrchestratorTaskDto>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(create_task_for_state(&context, req).await?))
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
    Json(req): Json<RemoteCompleteOrchestratorTaskPromptReq>,
) -> Result<Json<OrchestratorTaskPromptCompletionDto>, AppError> {
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
            .await?
            .ok_or_else(|| AppError::not_found("自动化 Prompt 完善项目不存在"))?;
        if project.kind == "remote" {
            let context = open_remote_project_for_shortcut(&state, &project).await?;
            let completed = RemoteOrchestratorClient::new()
                .complete_prompt(
                    &context.base_url,
                    RemoteCompleteOrchestratorTaskPromptReq {
                        project_id: Some(context.remote_project_id),
                        prompt: req.prompt,
                        working_directory: Some(context.remote_project_path),
                    },
                )
                .await?;
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
    Ok(Json(
        local_complete_orchestrator_task_prompt(&state, req.prompt, working_directory).await?,
    ))
}

/// 列出 mobile-facing Orchestrator task views HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机端可能选择本机项目或本机保存的远端项目 shortcut，需要同一接口返回可展示的 task view 列表。
///
/// Code Logic（这个函数做什么）:
///     接收 `{projectId}`，委托 commands 层 remote-aware helper，并用 `{views}` 包装结果。
pub async fn list_task_views(
    State(state): State<AppState>,
    Json(req): Json<RemoteListTasksReq>,
) -> Result<Json<OrchestratorTaskViewListResp>, AppError> {
    let views = list_orchestrator_task_views_for_state(&state, Some(req.project_id)).await?;
    Ok(Json(OrchestratorTaskViewListResp { views }))
}

/// 创建 mobile-facing Orchestrator task view HTTP handler。
///
/// Business Logic（为什么需要这个函数）:
///     手机端创建任务需要支持 local 和 remote shortcut，并保留 create+queue 与 clientRequestId 幂等语义。
///
/// Code Logic（这个函数做什么）:
///     接收 RemoteCreateOrchestratorTaskReq，委托 commands 层 HTTP helper，返回 local/remote/pendingRemote view。
pub async fn create_task_view(
    State(state): State<AppState>,
    Json(req): Json<RemoteCreateOrchestratorTaskReq>,
) -> Result<Json<OrchestratorTaskViewDto>, AppError> {
    Ok(Json(
        create_orchestrator_task_view_for_http(&state, req).await?,
    ))
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
    Json(req): Json<RemoteListTasksReq>,
) -> Result<Json<RemoteOrchestratorTaskListResp>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(list_tasks_for_state(&context, &req.project_id).await?))
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
    Json(req): Json<RemoteTaskReq>,
) -> Result<Json<RemoteOrchestratorEvidenceResp>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(get_evidence_for_state(&context, req).await?))
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
    Json(req): Json<RemoteTaskReq>,
) -> Result<Json<OrchestratorTaskDto>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(queue_task_for_state(&context, req).await?))
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
    Json(req): Json<RemoteTaskReq>,
) -> Result<Json<OrchestratorTaskDto>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(retry_task_for_state(&context, req).await?))
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
    Json(req): Json<RemoteTaskReq>,
) -> Result<Json<OrchestratorTaskDto>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(abort_task_for_state(&context, req).await?))
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
) -> Result<Json<RemoteOrchestratorConfigResp>, AppError> {
    let context = OrchestratorRouteContext::from_app_state(&state);
    Ok(Json(get_config_for_state(&context)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
    use crate::orchestrator::models::{
        OrchestratorRunState, OrchestratorTaskRow, OrchestratorTaskStatus,
        OrchestratorWorkflowState,
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
    ///     create route 的 queue=true 语义必须先创建 Draft，再复用安全入队逻辑得到 Queued。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过测试 helper 创建任务，断言返回状态为 Queued 且保留请求中的项目和优先级。
    #[tokio::test]
    async fn create_local_project_task_queues_when_requested() {
        let state = test_state().await;
        insert_project(&state, "project-1", "local").await;

        let created = create_task_for_state(
            &state,
            RemoteCreateOrchestratorTaskReq {
                project_id: "project-1".to_string(),
                title: "远端任务".to_string(),
                goal: "完成目标".to_string(),
                acceptance_criteria: "验收标准".to_string(),
                priority: 5,
                queue: true,
                client_request_id: Some("create-request-queued".to_string()),
            },
        )
        .await
        .expect("create task");

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
            queue: true,
            client_request_id: Some("create-request-1".to_string()),
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

        assert_eq!(first.id, second.id);
        assert_eq!(first.status, OrchestratorTaskStatus::Queued);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, first.id);
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
            queue: false,
            client_request_id: Some("   ".to_string()),
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
}
