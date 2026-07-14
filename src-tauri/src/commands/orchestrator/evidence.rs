//! evidence/config
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorReviewDiff,
    OrchestratorTaskRow, OrchestratorWorkflowState,
};
use crate::orchestrator::outbox::open_remote_project_for_shortcut;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::review_diff::collect_review_diff_for_worktree;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::remote_entity_id;
use std::path::Path;
use tauri::State;

use super::common::{
    get_local_project_task_for_action, get_orchestrator_workbench_project,
    map_remote_evidence_for_shortcut, remote_inner_task_id_for_shortcut,
};

/// Human Review / Rework 之外请求 review diff 时的稳定业务 code。
pub(crate) const REVIEW_DIFF_UNAVAILABLE_CODE: &str = "review_diff_unavailable";

/// Business Logic（为什么需要这个函数）:
///     review diff 只在 Human Review / Rework 阶段对用户有意义；其它泳道请求必须用稳定 code 拒绝，
///     避免前端把任意状态的任务误当成可审阅。
///
/// Code Logic（这个函数做什么）:
///     检查 `workflow_state` 是否为 HumanReview 或 Rework；否则返回 Conflict + 稳定 code。
pub(crate) fn ensure_review_diff_available(task: &OrchestratorTaskRow) -> Result<(), AppError> {
    match task.workflow_state {
        OrchestratorWorkflowState::HumanReview | OrchestratorWorkflowState::Rework => Ok(()),
        _ => Err(AppError::conflict(REVIEW_DIFF_UNAVAILABLE_CODE)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     owning device 本地采集 review diff 时，base/head 只能从 task/worktree 权威元数据派生，
///     不能接受浏览器任意 repo path/ref。
///
/// Code Logic（这个函数做什么）:
///     校验 Human Review/Rework、读取 worktree 行，以 worktree.path 与可选 base_branch 调用
///     `collect_review_diff_for_worktree`。
pub(crate) async fn collect_local_orchestrator_review_diff(
    state: &AppState,
    task: &OrchestratorTaskRow,
) -> Result<OrchestratorReviewDiff, AppError> {
    ensure_review_diff_available(task)?;
    let worktree_id = task.worktree_id.as_deref().filter(|id| !id.trim().is_empty());
    let Some(worktree_id) = worktree_id else {
        return Err(AppError::validation("任务缺少 worktree，无法采集 review diff"));
    };
    let worktree = state
        .workbench_worktree_repo
        .get(worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("找不到任务 worktree: {worktree_id}")))?;
    if worktree.project_id != task.project_id {
        return Err(AppError::validation("任务 worktree 不属于当前项目"));
    }
    let preferred_base = worktree.base_branch.as_deref();
    collect_review_diff_for_worktree(&task.id, Path::new(&worktree.path), preferred_base)
}

/// Business Logic（为什么需要这个函数）:
///     desktop/remote/mobile 共享同一入口：local 直接采集，remote shortcut 转发 owning device。
///
/// Code Logic（这个函数做什么）:
///     读取 Workbench 项目；local 校验归属后采集；remote 打开对端、剥离 inner task id、
///     capability-gated 调用对端，并把返回 `taskId` 映射为 `remote:<deviceId>:<inner>`。
pub(crate) async fn get_orchestrator_review_diff_for_state(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<OrchestratorReviewDiff, AppError> {
    let project = get_orchestrator_workbench_project(state, project_id).await?;
    if project.kind == "remote" {
        return get_remote_orchestrator_review_diff(state, &project, task_id).await;
    }
    let task = get_local_project_task_for_action(
        state.orchestrator_repo.as_ref(),
        project_id,
        task_id,
    )
    .await?;
    collect_local_orchestrator_review_diff(state, &task).await
}

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 Human Review 详情需要 owning device 生成的权威有界 diff，
///     且必须在缺失 `orchestrator.review-diff.v1` 时明确失败。
///
/// Code Logic（这个函数做什么）:
///     open remote project → 剥离 remote task id → `RemoteOrchestratorClient::get_review_diff`
///     → 映射 task_id 为 remote entity id。
async fn get_remote_orchestrator_review_diff(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    task_id: &str,
) -> Result<OrchestratorReviewDiff, AppError> {
    let context = open_remote_project_for_shortcut(state, remote_shortcut, None).await?;
    let remote_task_id = remote_inner_task_id_for_shortcut(remote_shortcut, task_id)?;
    let mut diff = RemoteOrchestratorClient::new()
        .get_review_diff(&context.base_url, &remote_task_id)
        .await
        .map_err(|error| {
            crate::net::peer_error::peer_call_error_to_app_error(error, "远端 Orchestrator")
        })?;
    diff.task_id = remote_entity_id(&remote_shortcut.device_id, &diff.task_id);
    Ok(diff)
}

/// Business Logic（为什么需要这个函数）:
///     owning-device P2P 路由只接收 taskId，需要在确认 local 项目后复用同一采集逻辑。
///
/// Code Logic（这个函数做什么）:
///     读取任务，要求所属项目 kind=local，再调用 `collect_local_orchestrator_review_diff`。
pub(crate) async fn get_local_owner_orchestrator_review_diff(
    state: &AppState,
    task_id: &str,
) -> Result<OrchestratorReviewDiff, AppError> {
    let task = state.orchestrator_repo.get_task(task_id).await?;
    let project = get_orchestrator_workbench_project(state, &task.project_id).await?;
    if project.kind != "local" {
        return Err(AppError::validation(
            "远端 Orchestrator 只接受对端本机项目",
        ));
    }
    collect_local_orchestrator_review_diff(state, &task).await
}

/// 按项目读取 remote-aware Orchestrator review diff。
///
/// Business Logic（为什么需要这个函数）:
///     Human Review 详情 Changes tab 需要有界只读 diff；local 本机采集，remote 由 owning device 生成。
///
/// Code Logic（这个函数做什么）:
///     委托 `get_orchestrator_review_diff_for_state`，入参仅 projectId/taskId，拒绝任意 repo path/ref。
#[tauri::command]
pub async fn get_orchestrator_review_diff(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorReviewDiff, AppError> {
    get_orchestrator_review_diff_for_state(state.inner(), &project_id, &task_id).await
}

/// 按项目读取 remote-aware Orchestrator evidence。
///
/// Business Logic（为什么需要这个函数）:
///     remote task 详情需要展示 owning device 上真实写入的 evidence；local 任务继续读本机 SQLite。
///
/// Code Logic（这个函数做什么）:
///     local 项目走 repo.list_evidence；remote 项目打开远端项目后调用 RemoteOrchestratorClient::get_evidence。
#[tauri::command]
pub async fn list_orchestrator_task_evidence_for_project(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        return state.orchestrator_repo.list_evidence(&task_id).await;
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project, None).await?;
    let remote_task_id = remote_inner_task_id_for_shortcut(&project, &task_id)?;
    let evidence = RemoteOrchestratorClient::new()
        .get_evidence(&context.base_url, &remote_task_id)
        .await?;
    Ok(map_remote_evidence_for_shortcut(evidence, &project))
}

/// 按项目读取 remote-aware Orchestrator 自动化配置。
///
/// Business Logic（为什么需要这个函数）:
///     remote 项目展示的自动化策略应来自远端设备 Settings，而不是本机 shortcut 的配置。
///
/// Code Logic（这个函数做什么）:
///     local 项目返回本机 AppConfig.orchestrator DTO；remote 项目打开远端后调用远端 config endpoint。
#[tauri::command]
pub async fn get_orchestrator_config_for_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let config = state
            .config
            .read()
            .expect("config 读锁中毒")
            .orchestrator
            .clone();
        return Ok(config.into());
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project, None).await?;
    RemoteOrchestratorClient::new()
        .get_config(&context.base_url)
        .await
}

/// 查询 legacy Orchestrator 项目配置。
///
/// Business Logic（为什么需要这个函数）:
///     历史版本写入过项目级自动化配置，后端仍保留兼容/调试读取能力。
///     用户可见配置入口已经收敛到 Settings 自动化 tab，scheduler、验证和 delivery 统一读取 AppConfig.orchestrator。
///
/// Code Logic（这个函数做什么）:
///     委托仓储 get_or_create_project_config，并返回 camelCase DTO。
#[tauri::command]
pub async fn get_orchestrator_project_config(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorProjectConfigDto, AppError> {
    state
        .orchestrator_repo
        .get_or_create_project_config(&project_id)
        .await
}

/// 查询 Orchestrator 任务证据列表。
///
/// Business Logic（为什么需要这个函数）:
///     任务详情右侧 evidence 卡需要读取当前任务的验证输出与交付证据。
///
/// Code Logic（这个函数做什么）:
///     透传 task_id 给仓储 list_evidence，并返回 camelCase DTO 列表。
#[tauri::command]
pub async fn list_orchestrator_task_evidence(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    state.orchestrator_repo.list_evidence(&task_id).await
}
