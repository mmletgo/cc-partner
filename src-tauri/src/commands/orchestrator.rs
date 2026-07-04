use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorProjectConfigDto, OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
};
use crate::state::AppState;
use chrono::Utc;
use tauri::State;
use uuid::Uuid;

/// 创建 Orchestrator 任务的命令入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端创建编排任务时只提交用户可编辑字段，后端统一补齐 id、状态、关联执行信息和时间戳。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 接收 Tauri invoke 参数，并保留 priority 可选值用于默认优先级归一。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskRequest {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: Option<i64>,
}

/// 构造待持久化的 Orchestrator 任务 Row。
///
/// Business Logic（为什么需要这个函数）:
///     创建任务时需要在命令层统一做必填校验、清理用户输入，并初始化任务为 Draft 状态。
///
/// Code Logic（这个函数做什么）:
///     校验 project/title/goal 非空，生成 UUID 和 UTC 时间戳，返回完整 OrchestratorTaskRow。
fn build_orchestrator_task_row(
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskRow, AppError> {
    let project_id = request.project_id.trim();
    let title = request.title.trim();
    let goal = request.goal.trim();
    let acceptance_criteria = request.acceptance_criteria.trim();

    if project_id.is_empty() {
        return Err(AppError::generic("项目不能为空"));
    }
    if title.is_empty() {
        return Err(AppError::generic("任务标题不能为空"));
    }
    if goal.is_empty() {
        return Err(AppError::generic("任务目标不能为空"));
    }

    let now = Utc::now().to_rfc3339();
    Ok(OrchestratorTaskRow {
        id: Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        title: title.to_string(),
        goal: goal.to_string(),
        acceptance_criteria: acceptance_criteria.to_string(),
        status: OrchestratorTaskStatus::Draft,
        priority: request.priority.unwrap_or(0),
        branch_name: None,
        worktree_id: None,
        session_id: None,
        blocked_reason: None,
        attempt: 0,
        created_at: now.clone(),
        updated_at: now,
        started_at: None,
        finished_at: None,
    })
}

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
///     调用纯 helper 完成校验和 Row 初始化，再委托 OrchestratorRepo 插入并返回 DTO。
#[tauri::command]
pub async fn create_orchestrator_task(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskDto, AppError> {
    let row = build_orchestrator_task_row(request)?;
    state.orchestrator_repo.create_task(&row).await?;
    Ok(OrchestratorTaskDto::from(row))
}

/// 查询项目 Orchestrator 策略。
///
/// Business Logic（为什么需要这个函数）:
///     页面右侧策略卡需要展示当前 Workbench 项目的自动化策略，缺失时按默认策略初始化。
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

/// 将 Orchestrator 草稿任务加入队列。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认草稿任务后，需要把任务状态切换为 Queued，供后续调度器按队列处理。
///
/// Code Logic（这个函数做什么）:
///     调用 repo.set_task_status 把指定 task_id 更新为 queued，并把完整任务 Row 转换为 DTO。
#[tauri::command]
pub async fn queue_orchestrator_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .set_task_status(&task_id, OrchestratorTaskStatus::Queued, None)
        .await?;
    Ok(OrchestratorTaskDto::from(task))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 任务创建命令必须拒绝没有项目归属的任务，避免调度器后续无法定位工作台项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 project_id 请求并断言 row builder 返回错误。
    #[test]
    fn build_task_row_rejects_blank_project_id() {
        let request = CreateOrchestratorTaskRequest {
            project_id: "  ".to_string(),
            title: "实现任务".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };

        let result = build_orchestrator_task_row(request);

        assert!(result.is_err());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 任务必须有可展示标题和明确目标，否则任务列表与调度执行都缺少必要语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分别构造空标题和空目标请求，并断言 row builder 拒绝。
    #[test]
    fn build_task_row_rejects_blank_title_and_goal() {
        let blank_title = CreateOrchestratorTaskRequest {
            project_id: "project-1".to_string(),
            title: " ".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };
        let blank_goal = CreateOrchestratorTaskRequest {
            project_id: "project-1".to_string(),
            title: "实现任务".to_string(),
            goal: " ".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };

        assert!(build_orchestrator_task_row(blank_title).is_err());
        assert!(build_orchestrator_task_row(blank_goal).is_err());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建任务进入草稿态，用户输入的文本需要清理首尾空白，未显式设置优先级时按普通任务处理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造带空白和空 priority 的请求，断言生成 Row 的初始状态、trim 和默认字段。
    #[test]
    fn build_task_row_trims_fields_and_sets_defaults() {
        let request = CreateOrchestratorTaskRequest {
            project_id: "  project-1  ".to_string(),
            title: "  实现 API  ".to_string(),
            goal: "  暴露任务命令  ".to_string(),
            acceptance_criteria: "  测试通过  ".to_string(),
            priority: None,
        };

        let row = build_orchestrator_task_row(request).expect("row");

        assert_eq!(row.project_id, "project-1");
        assert_eq!(row.title, "实现 API");
        assert_eq!(row.goal, "暴露任务命令");
        assert_eq!(row.acceptance_criteria, "测试通过");
        assert_eq!(row.status, OrchestratorTaskStatus::Draft);
        assert_eq!(row.priority, 0);
        assert_eq!(row.attempt, 0);
        assert!(row.branch_name.is_none());
        assert!(row.worktree_id.is_none());
        assert!(row.session_id.is_none());
        assert!(row.blocked_reason.is_none());
        assert!(row.started_at.is_none());
        assert!(row.finished_at.is_none());
        assert!(!row.id.trim().is_empty());
        assert!(!row.created_at.trim().is_empty());
        assert_eq!(row.created_at, row.updated_at);
    }
}
