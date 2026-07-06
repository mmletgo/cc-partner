//! Orchestrator global scheduler.
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器需要按设备级全局配置定期领取本机 Queued 任务，并把任务交给可见 Workbench Runner。
//!
//! Code Logic（这个模块做什么）:
//!     提供后台 scheduler runtime、单次 dispatch 入口和并发容量相关纯 helper。

use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::runner::prepare_visible_runner;
use crate::state::AppState;
use std::time::Duration;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

const SCHEDULER_TICK_SECS: u64 = 10;

/// Orchestrator scheduler 运行时句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     应用启动后需要保存后台调度循环的取消令牌，退出时能优雅停止自动任务领取。
///
/// Code Logic（这个结构体做什么）:
///     包装 tokio CancellationToken，并提供轻量 clone 与读取 token 的方法。
#[derive(Clone)]
pub struct OrchestratorRuntime {
    cancel: CancellationToken,
}

impl OrchestratorRuntime {
    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 启动时需要创建一份新的取消令牌供后台循环和 AppState 共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造包含 CancellationToken::new() 的 OrchestratorRuntime。
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     lib.rs 需要拿到取消令牌并存入 AppState，后台 task 也需要持有同一令牌。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone 内部 CancellationToken；clone 后 cancel 会广播到所有持有者。
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Business Logic（为什么需要这个函数）:
///     应用启动后应自动轮询 Orchestrator 队列，无需用户保持页面打开。
///
/// Code Logic（这个函数做什么）:
///     使用 tauri async runtime 启动后台循环，每 10 秒调用 dispatch_once，收到 cancel 后退出。
pub fn start_orchestrator_scheduler(app_handle: AppHandle, state: AppState) -> CancellationToken {
    let runtime = OrchestratorRuntime::new();
    let cancel = runtime.cancel_token();
    let task_cancel = runtime.cancel_token();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    tracing::info!("Orchestrator scheduler 已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(SCHEDULER_TICK_SECS)) => {
                    if let Err(err) = dispatch_once(&state, app_handle.clone()).await {
                        tracing::error!("Orchestrator scheduler dispatch 失败: {err}");
                    }
                }
            }
        }
    });
    cancel
}

/// Business Logic（为什么需要这个函数）:
///     scheduler tick 和手动命令都需要执行同一套领取逻辑，避免 UI 手动触发与后台行为不一致。
///
/// Code Logic（这个函数做什么）:
///     每次从 AppState 读取全局 Orchestrator 配置，在 repo 事务内按全局本机容量批量 claim 可执行泳道任务并交给 runner；
///     runner 失败时仅在任务仍为 Preparing 或 bootstrap Running 时把任务置为 Blocked 并追加事件，成功时计入 dispatched。
pub async fn dispatch_once(state: &AppState, app_handle: AppHandle) -> Result<usize, AppError> {
    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    let tasks = claim_tasks_for_dispatch(state.orchestrator_repo.as_ref(), &config).await?;
    let mut dispatched = 0usize;
    for task in tasks {
        match prepare_visible_runner(state, app_handle.clone(), &task).await {
            Ok(_) => {
                dispatched += 1;
            }
            Err(err) => {
                let reason = err.to_string();
                record_runner_failure(&state.orchestrator_repo, &task.id, &reason).await?;
            }
        }
    }
    Ok(dispatched)
}

/// Business Logic（为什么需要这个函数）:
///     后台 scheduler 和手动 dispatch 应共享全局开关与容量解释，测试也需要绕开 Tauri AppHandle 只验证领取语义。
///
/// Code Logic（这个函数做什么）:
///     enabled=false 时返回空；enabled=true 时委托 repo 在事务内按全局本机容量领取 Todo/Rework 且未运行的 local 任务。
async fn claim_tasks_for_dispatch(
    repo: &OrchestratorRepo,
    config: &OrchestratorAutomationConfig,
) -> Result<Vec<OrchestratorTaskRow>, AppError> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    repo.claim_next_local_queued_tasks_with_global_capacity(config.max_concurrent_tasks)
        .await
}

/// Business Logic（为什么需要这个函数）:
///     Runner 准备失败可能发生在挂账 Running 前后，也可能与用户 Abort 并发发生；失败补偿必须尊重用户终止。
///
/// Code Logic（这个函数做什么）:
///     使用 expected-status 原子转移仅允许 Preparing/Running→Blocked；命中后追加 blocked event，
///     未命中时读取并返回当前任务，不写 blocked event。
async fn record_runner_failure(
    repo: &OrchestratorRepo,
    task_id: &str,
    reason: &str,
) -> Result<OrchestratorTaskRow, AppError> {
    let blocked_task = if let Some(task) = repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Preparing,
            OrchestratorTaskStatus::Blocked,
            Some(reason),
        )
        .await?
    {
        task
    } else if let Some(task) = repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Blocked,
            Some(reason),
        )
        .await?
    {
        task
    } else {
        return repo.get_task(task_id).await;
    };

    repo.add_event(
        task_id,
        "blocked",
        &format!("Runner 准备失败: {reason}"),
        None,
    )
    .await?;
    Ok(blocked_task)
}

/// Business Logic（为什么需要这个函数）:
///     项目配置的并发上限可能被设为 0 或负数，调度器需要安全地把它解释为不调度。
///
/// Code Logic（这个函数做什么）:
///     返回 max_concurrent_tasks - active_count 的非负剩余容量；非正上限直接返回 0。
#[cfg(test)]
pub(crate) fn dispatch_capacity(max_concurrent_tasks: i64, active_count: i64) -> i64 {
    if max_concurrent_tasks <= 0 {
        return 0;
    }
    (max_concurrent_tasks - active_count).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorAutomationConfig;
    use crate::orchestrator::models::{
        OrchestratorAttemptPhase, OrchestratorRunState, OrchestratorTaskRow,
        OrchestratorTaskStatus, OrchestratorWorkflowState,
    };
    use crate::orchestrator::repo::OrchestratorRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Row;
    use sqlx::SqlitePool;
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 测试需要隔离 SQLite，避免 runner 失败路径污染用户真实任务表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接内存数据库，初始化 Orchestrator schema 并返回 repo。
    async fn setup_repo() -> (SqlitePool, OrchestratorRepo) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        let repo = OrchestratorRepo::new(pool.clone());
        (pool, repo)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler Phase 3 只领取本机 local Workbench 项目的任务，单测需要最小项目表模拟 local/remote。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 claim 查询依赖的 workbench_projects 表。
    async fn create_workbench_projects_table(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度测试需要显式声明项目是 local 还是 remote，避免误把远端快捷方式当成本机执行目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入一条 Workbench 项目记录，除 kind/id 外字段使用稳定测试值。
    async fn insert_workbench_project(pool: &SqlitePool, id: &str, kind: &str) {
        sqlx::query(
            "INSERT INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'device-test', 'Device Test', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("Project {id}"))
        .bind(kind)
        .bind(format!("/tmp/{id}"))
        .bind("2026-07-05T00:00:00Z")
        .bind("2026-07-05T00:00:00Z")
        .bind("2026-07-05T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runner 失败路径测试只关心任务状态和 id，但 repo 插入需要完整任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造字段完整、时间戳稳定的 OrchestratorTaskRow。
    fn task_row(id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: "project-1".to_string(),
            title: format!("Task {id}"),
            goal: format!("Goal {id}"),
            acceptance_criteria: format!("Criteria {id}"),
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
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 全局容量测试需要在多个项目之间分布任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复用基础任务构造，再覆盖 project_id。
    fn task_row_for_project(
        id: &str,
        project_id: &str,
        status: OrchestratorTaskStatus,
    ) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            project_id: project_id.to_string(),
            ..task_row(id, status)
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     split state 调度测试需要构造 legacy status 与新 workflow/run state 不完全一致的任务，覆盖迁移后的真实选择条件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接更新测试任务的 workflow_state、run_state 和 blocked_reason，避免通过 legacy status 间接推导。
    async fn set_task_split_state(
        pool: &SqlitePool,
        task_id: &str,
        workflow_state: OrchestratorWorkflowState,
        run_state: OrchestratorRunState,
        blocked_reason: Option<&str>,
    ) {
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET workflow_state = ?, run_state = ?, blocked_reason = ? WHERE id = ?",
        )
        .bind(workflow_state.as_str())
        .bind(run_state.as_str())
        .bind(blocked_reason)
        .bind(task_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度测试只关心 enabled 和全局并发上限，其它自动交付开关使用默认值即可。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 OrchestratorAutomationConfig，并覆盖 enabled/max_concurrent_tasks。
    fn global_config(enabled: bool, max_concurrent_tasks: i64) -> OrchestratorAutomationConfig {
        OrchestratorAutomationConfig {
            enabled,
            max_concurrent_tasks,
            ..OrchestratorAutomationConfig::default()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目并发上限为 0 或负数时必须视为不调度，避免错误配置触发自动执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用容量 helper，断言非正上限和已满项目都返回 0，未满项目返回剩余容量。
    #[test]
    fn dispatch_capacity_clamps_non_positive_limits() {
        assert_eq!(super::dispatch_capacity(0, 0), 0);
        assert_eq!(super::dispatch_capacity(-2, 0), 0);
        assert_eq!(super::dispatch_capacity(2, 2), 0);
        assert_eq!(super::dispatch_capacity(3, 1), 2);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局 Orchestrator 开关关闭时，后台 scheduler 即使存在 queued local 任务也必须 dispatch 0。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入 local queued 任务后用 enabled=false 配置执行领取 helper，断言返回空且任务仍为 Queued。
    #[tokio::test]
    async fn global_disabled_config_claims_no_tasks() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "project-1", "local").await;
        repo.create_task(&task_row("task-queued", OrchestratorTaskStatus::Queued))
            .await
            .unwrap();

        let claimed = claim_tasks_for_dispatch(&repo, &global_config(false, 4))
            .await
            .unwrap();
        let persisted = repo.get_task("task-queued").await.unwrap();

        assert!(claimed.is_empty());
        assert_eq!(persisted.status, OrchestratorTaskStatus::Queued);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局并发上限应在所有本机 local 项目之间共享，而不是每个项目各自拥有一份容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     制造 1 个 active local 任务和 3 个 queued local 任务，max=3 时只能再领取 2 个。
    #[tokio::test]
    async fn global_capacity_is_shared_across_local_projects() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        insert_workbench_project(&pool, "local-b", "local").await;
        repo.create_task(&task_row_for_project(
            "active",
            "local-a",
            OrchestratorTaskStatus::Running,
        ))
        .await
        .unwrap();
        for (id, project_id, priority) in [
            ("queued-high", "local-b", 30_i64),
            ("queued-mid", "local-a", 20_i64),
            ("queued-low", "local-b", 10_i64),
        ] {
            repo.create_task(&task_row_for_project(
                id,
                project_id,
                OrchestratorTaskStatus::Queued,
            ))
            .await
            .unwrap();
            sqlx::query("UPDATE orchestrator_tasks SET priority = ? WHERE id = ?")
                .bind(priority)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            set_task_split_state(
                &pool,
                id,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Idle,
                None,
            )
            .await;
        }

        let claimed = claim_tasks_for_dispatch(&repo, &global_config(true, 3))
            .await
            .unwrap();

        assert_eq!(
            claimed
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["queued-high", "queued-mid"]
        );
        assert_eq!(
            repo.get_task("queued-low").await.unwrap().status,
            OrchestratorTaskStatus::Queued
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     split state 后，scheduler 应只领取 Todo/Idle 与 Rework/Blocked 这类 active workflow 任务，Backlog 不应自动启动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Backlog idle、Todo idle 和 Rework blocked 三个本机任务，断言只领取 Todo/Rework 并写入 Preparing split state。
    #[tokio::test]
    async fn scheduler_claims_todo_and_rework_only() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        for id in ["backlog-idle", "todo-idle", "rework-blocked"] {
            repo.create_task(&task_row_for_project(
                id,
                "local-a",
                OrchestratorTaskStatus::Draft,
            ))
            .await
            .unwrap();
        }
        set_task_split_state(
            &pool,
            "todo-idle",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        set_task_split_state(
            &pool,
            "rework-blocked",
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            Some("验证失败"),
        )
        .await;

        let claimed = claim_tasks_for_dispatch(&repo, &global_config(true, 3))
            .await
            .unwrap();
        let backlog = repo.get_task("backlog-idle").await.unwrap();
        let rework = repo.get_task("rework-blocked").await.unwrap();

        let claimed_ids = claimed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(claimed_ids.len(), 2);
        assert!(claimed_ids.contains(&"todo-idle"));
        assert!(claimed_ids.contains(&"rework-blocked"));
        assert!(claimed.iter().all(|task| {
            task.status == OrchestratorTaskStatus::Preparing
                && task.workflow_state == OrchestratorWorkflowState::InProgress
                && task.run_state == OrchestratorRunState::Preparing
                && task.attempt_phase == Some(OrchestratorAttemptPhase::PreparingWorkspace)
                && task.blocked_reason.is_none()
        }));
        assert_eq!(backlog.workflow_state, OrchestratorWorkflowState::Backlog);
        assert_eq!(backlog.run_state, OrchestratorRunState::Idle);
        assert_eq!(rework.blocked_reason, None);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench shortcut 不能由本机 scheduler 启动 Runner，否则会在错误设备上创建 worktree/terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同时插入 remote 高优先级任务和 local 低优先级任务，断言只领取 local。
    #[tokio::test]
    async fn remote_workbench_projects_are_skipped_by_global_claim() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        insert_workbench_project(&pool, "remote-a", "remote").await;
        repo.create_task(&task_row_for_project(
            "remote-high",
            "remote-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        repo.create_task(&task_row_for_project(
            "local-low",
            "local-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "remote-high",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        set_task_split_state(
            &pool,
            "local-low",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        sqlx::query("UPDATE orchestrator_tasks SET priority = 100 WHERE id = 'remote-high'")
            .execute(&pool)
            .await
            .unwrap();

        let claimed = claim_tasks_for_dispatch(&repo, &global_config(true, 5))
            .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "local-low");
        assert_eq!(
            repo.get_task("remote-high").await.unwrap().status,
            OrchestratorTaskStatus::Queued
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 runtime 不再读取 legacy project_config；旧表里的 disabled/max=0 不能阻止全局开启后的 dispatch。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 disabled 且 max=0 的 legacy 配置，再用全局 enabled/max=1 领取任务，断言任务仍被领取。
    #[tokio::test]
    async fn legacy_project_config_does_not_affect_global_dispatch_claim() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "project-1", "local").await;
        repo.get_or_create_project_config("project-1")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE orchestrator_project_config SET enabled = 0, max_concurrent_tasks = 0 WHERE project_id = ?",
        )
        .bind("project-1")
        .execute(&pool)
        .await
        .unwrap();
        repo.create_task(&task_row("task-queued", OrchestratorTaskStatus::Queued))
            .await
            .unwrap();
        set_task_split_state(
            &pool,
            "task-queued",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;

        let claimed = claim_tasks_for_dispatch(&repo, &global_config(true, 1))
            .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "task-queued");
        assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可能在 runner 返回错误前终止 Preparing 任务，失败补偿不得把 Aborted 覆盖为 Blocked 或追加误导事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Preparing 任务后模拟 Abort，再执行 runner 失败记录 helper，断言返回/持久化状态仍为 Aborted 且 event 表为空。
    #[tokio::test]
    async fn runner_failure_after_abort_does_not_block_or_write_event() {
        let (pool, repo) = setup_repo().await;
        let task = task_row("task-1", OrchestratorTaskStatus::Preparing);
        repo.create_task(&task).await.unwrap();
        repo.set_task_status(&task.id, OrchestratorTaskStatus::Aborted, None)
            .await
            .unwrap();

        let returned = record_runner_failure(&repo, &task.id, "runner failed")
            .await
            .unwrap();
        let persisted = repo.get_task(&task.id).await.unwrap();
        let event_count = sqlx::query("SELECT COUNT(*) AS count FROM orchestrator_task_events")
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get::<i64, _>("count")
            .unwrap();

        assert_eq!(returned.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
        assert!(persisted.blocked_reason.is_none());
        assert_eq!(event_count, 0);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 已挂账为 Running 后写入终端仍可能失败，此时任务应进入 Blocked 并保留可接管现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Running 任务后执行 runner 失败记录 helper，断言状态变为 Blocked、原因保留且 event 只写一条。
    #[tokio::test]
    async fn runner_failure_after_running_blocks_and_writes_event() {
        let (pool, repo) = setup_repo().await;
        let mut task = task_row("task-1", OrchestratorTaskStatus::Running);
        task.branch_name = Some("agent/task-1-test".to_string());
        task.worktree_id = Some("worktree-1".to_string());
        task.session_id = Some("session-1".to_string());
        repo.create_task(&task).await.unwrap();

        let returned = record_runner_failure(&repo, &task.id, "prompt write failed")
            .await
            .unwrap();
        let persisted = repo.get_task(&task.id).await.unwrap();
        let event_count = sqlx::query("SELECT COUNT(*) AS count FROM orchestrator_task_events")
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get::<i64, _>("count")
            .unwrap();

        assert_eq!(returned.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(
            persisted.blocked_reason.as_deref(),
            Some("prompt write failed")
        );
        assert_eq!(persisted.worktree_id.as_deref(), Some("worktree-1"));
        assert_eq!(persisted.session_id.as_deref(), Some("session-1"));
        assert_eq!(event_count, 1);
    }
}
