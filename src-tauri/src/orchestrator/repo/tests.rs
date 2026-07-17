//! OrchestratorRepo 单测。
#![allow(dead_code)]
#![allow(unused_imports)]

use super::*;
use crate::error::AppError;
use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimCandidate, ClaimCasOutcome, ClaimScanCursor,
    CLAIM_CANDIDATE_LIMIT,
};
use crate::orchestrator::claude_runtime::ClaudeRuntimeSummary;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorAttemptStatus, OrchestratorCreateAction,
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorRunState,
    OrchestratorTaskAttemptRow, OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
    OrchestratorWorkflowState, SplitTaskState, EVIDENCE_KIND_REPAIR_PROMPT,
};
use crate::orchestrator::outbox::{
    OrchestratorRemoteOutboxRow, RemoteMirrorTask, RemoteOutboxStatus,
};
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Acquire, Row};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;
/// Business Logic（为什么需要这个函数）:
///     仓储测试必须使用隔离内存数据库，避免污染用户真实 cc-partner 数据。
///
/// Code Logic（这个函数做什么）:
///     创建单连接内存 SQLite pool，执行 Orchestrator schema 初始化并返回 pool 与 repo。
async fn setup_repo() -> (SqlitePool, OrchestratorRepo) {
    let pool = setup_raw_pool().await;
    OrchestratorRepo::init_schema(&pool).await.unwrap();
    let repo = OrchestratorRepo::new(pool.clone());
    (pool, repo)
}

/// Business Logic（为什么需要这个函数）:
///     旧库迁移测试需要在 init_schema 之前手动创建 legacy 表，不能复用已初始化 schema 的 setup_repo。
///
/// Code Logic（这个函数做什么）:
///     创建单连接内存 SQLite pool，不执行任何 schema 初始化，交给调用方安排建表顺序。
async fn setup_raw_pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

/// Business Logic（为什么需要这个函数）:
///     全局调度容量只针对本机 Workbench 项目生效，repo 单测需要最小 workbench_projects 表来模拟项目来源。
///
/// Code Logic（这个函数做什么）:
///     在内存 SQLite 中创建 claim 查询所需的 workbench_projects 字段子集。
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
///     全局调度测试要区分 local/remote Workbench 项目，确保远端快捷方式不会被本机 scheduler 领取。
///
/// Code Logic（这个函数做什么）:
///     插入一条最小 Workbench 项目记录，kind 由调用方指定。
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
///     仓储测试需要模拟命令层已经构造好的任务 Row，确保 repo 不替调用方决定任务字段。
///
/// Code Logic（这个函数做什么）:
///     基于传入 id/project/status 构造完整 OrchestratorTaskRow，其它字段使用稳定测试值。
fn task_row(id: &str, project_id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
    OrchestratorTaskRow {
        id: id.to_string(),
        project_id: project_id.to_string(),
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
///     split state 调度测试需要构造与 legacy status 解耦的任务，验证新调度条件只看业务泳道和运行态。
///
/// Code Logic（这个函数做什么）:
///     直接更新测试任务的 workflow_state、run_state 和 blocked_reason，保持其它字段不变。
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
///     基础 schema 初始化必须能支撑任务创建、列表读取和项目配置默认策略。
///
/// Code Logic（这个函数做什么）:
///     初始化内存库后插入任务并查询列表，再验证 project_config 的 enabled/retry_limit 默认值。
#[tokio::test]
async fn init_schema_creates_tables() {
    let (pool, repo) = setup_repo().await;
    let task = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);

    repo.create_task(&task).await.unwrap();
    let listed = repo.list_tasks(Some("project-1")).await.unwrap();
    sqlx::query(
        "INSERT INTO orchestrator_project_config (project_id, created_at, updated_at) \
         VALUES (?, ?, ?)",
    )
    .bind("project-1")
    .bind("2026-07-05T00:00:00Z")
    .bind("2026-07-05T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();
    let config = sqlx::query(
        "SELECT enabled, retry_limit FROM orchestrator_project_config WHERE project_id = ?",
    )
    .bind("project-1")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, task.id);
    assert_eq!(config.try_get::<i64, _>("enabled").unwrap(), 0);
    assert_eq!(config.try_get::<i64, _>("retry_limit").unwrap(), 0);
}

/// Business Logic（为什么需要这个测试）:
///     split state 迁移必须在旧库启动时补齐所有新列，否则任务列表读取会因缺列失败。
///
/// Code Logic（这个测试做什么）:
///     初始化内存 schema 后读取 orchestrator_tasks 列清单，断言 workflow/run/runtime 元数据列全部存在。
#[tokio::test]
async fn init_schema_adds_split_state_columns() {
    let (pool, _repo) = setup_repo().await;

    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('orchestrator_tasks')")
            .fetch_all(&pool)
            .await
            .expect("读取列信息成功");

    for expected in [
        "workflow_state",
        "run_state",
        "attempt_phase",
        "source",
        "external_id",
        "external_identifier",
        "external_url",
        "external_state",
        "external_labels_json",
        "runner_provider",
        "claude_session_id",
        "transcript_path",
        "runtime_started_at",
        "last_activity_at",
        "last_runtime_event",
        "last_runtime_message",
    ] {
        assert!(
            columns.iter().any(|column| column == expected),
            "missing column {expected}"
        );
    }
}

/// Business Logic（为什么需要这个测试）:
///     tracker 预留字段必须随任务从数据库行投影到前端 DTO，后续 Linear/GitHub 等外部系统同步才不会丢状态和标签。
///
/// Code Logic（这个测试做什么）:
///     创建任务后直接写入 external_state/external_labels_json，读取 DTO JSON 并断言 camelCase 字段和值完整保留。
#[tokio::test]
async fn tracker_reserved_fields_roundtrip_from_row_to_dto() {
    let (pool, repo) = setup_repo().await;
    let task = task_row("tracker-task", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&task).await.unwrap();
    sqlx::query(
        "UPDATE orchestrator_tasks SET external_state = ?, external_labels_json = ? WHERE id = ?",
    )
    .bind("in_progress")
    .bind(r#"["frontend","p1"]"#)
    .bind(&task.id)
    .execute(&pool)
    .await
    .unwrap();

    let persisted = repo.get_task(&task.id).await.unwrap();
    let value = serde_json::to_value(OrchestratorTaskDto::from(persisted)).unwrap();

    assert_eq!(value.get("externalState").unwrap(), "in_progress");
    assert_eq!(
        value.get("externalLabels").unwrap(),
        &serde_json::json!(["frontend", "p1"])
    );
}

/// Business Logic（为什么需要这个测试）:
///     用户从旧版本升级时，已有 orchestrator_tasks 表没有 split state 列，启动迁移必须补列并按旧 status 回填看板状态。
///
/// Code Logic（这个测试做什么）:
///     手动创建旧版任务表并插入多种 legacy status，执行 init_schema 后断言 workflow_state/run_state 已按映射更新。
#[tokio::test]
async fn init_schema_backfills_split_state_for_legacy_tasks() {
    let pool = setup_raw_pool().await;
    sqlx::query(
        "CREATE TABLE orchestrator_tasks (
          id TEXT PRIMARY KEY,
          project_id TEXT NOT NULL,
          title TEXT NOT NULL,
          goal TEXT NOT NULL,
          acceptance_criteria TEXT NOT NULL,
          status TEXT NOT NULL,
          priority INTEGER NOT NULL DEFAULT 0,
          branch_name TEXT,
          worktree_id TEXT,
          session_id TEXT,
          blocked_reason TEXT,
          attempt INTEGER NOT NULL DEFAULT 0,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          started_at TEXT,
          finished_at TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("创建旧版任务表成功");

    for (id, status) in [
        ("task-queued", OrchestratorTaskStatus::Queued),
        ("task-blocked", OrchestratorTaskStatus::Blocked),
        ("task-delivering", OrchestratorTaskStatus::Delivering),
        ("task-done", OrchestratorTaskStatus::Done),
    ] {
        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, \
              created_at, updated_at) \
             VALUES (?, 'project-1', ?, 'goal', 'criteria', ?, 0, ?, ?)",
        )
        .bind(id)
        .bind(format!("Task {id}"))
        .bind(status.as_str())
        .bind("2026-07-05T00:00:00Z")
        .bind("2026-07-05T00:00:00Z")
        .execute(&pool)
        .await
        .expect("插入旧任务成功");
    }

    OrchestratorRepo::init_schema(&pool)
        .await
        .expect("旧库 schema 迁移成功");

    let rows =
        sqlx::query("SELECT id, workflow_state, run_state FROM orchestrator_tasks ORDER BY id ASC")
            .fetch_all(&pool)
            .await
            .expect("读取迁移后任务成功");

    let migrated: Vec<(String, String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("id").expect("读取 id"),
                row.try_get("workflow_state").expect("读取 workflow_state"),
                row.try_get("run_state").expect("读取 run_state"),
            )
        })
        .collect();

    assert_eq!(
        migrated,
        vec![
            (
                "task-blocked".to_string(),
                "rework".to_string(),
                "blocked".to_string()
            ),
            (
                "task-delivering".to_string(),
                "merging".to_string(),
                "delivering".to_string()
            ),
            (
                "task-done".to_string(),
                "done".to_string(),
                "idle".to_string()
            ),
            (
                "task-queued".to_string(),
                "todo".to_string(),
                "idle".to_string()
            ),
        ]
    );

    let repo = OrchestratorRepo::new(pool);
    let listed = repo
        .list_tasks(Some("project-1"))
        .await
        .expect("迁移后任务可用新 row scanner 读取");
    assert_eq!(listed.len(), 4);
}

/// Business Logic（为什么需要这个测试）:
///     split state 列一旦存在就可能已经承载用户看板位置，后续启动不能再用 legacy status 覆盖用户调整。
///
/// Code Logic（这个测试做什么）:
///     创建已带 workflow/run 列的旧表并写入非默认 status 组合，执行 init_schema 后断言 split state 保持原值。
#[tokio::test]
async fn init_schema_does_not_backfill_existing_split_state_columns() {
    let pool = setup_raw_pool().await;
    sqlx::query(
        "CREATE TABLE orchestrator_tasks (
          id TEXT PRIMARY KEY,
          project_id TEXT NOT NULL,
          title TEXT NOT NULL,
          goal TEXT NOT NULL,
          acceptance_criteria TEXT NOT NULL,
          status TEXT NOT NULL,
          workflow_state TEXT NOT NULL DEFAULT 'backlog',
          run_state TEXT NOT NULL DEFAULT 'idle',
          priority INTEGER NOT NULL DEFAULT 0,
          branch_name TEXT,
          worktree_id TEXT,
          session_id TEXT,
          blocked_reason TEXT,
          attempt INTEGER NOT NULL DEFAULT 0,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          started_at TEXT,
          finished_at TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("创建带 split state 的任务表成功");
    sqlx::query(
        "INSERT INTO orchestrator_tasks \
         (id, project_id, title, goal, acceptance_criteria, status, workflow_state, \
          run_state, priority, created_at, updated_at) \
         VALUES ('task-blocked', 'project-1', 'Blocked task', 'goal', 'criteria', \
          'blocked', 'backlog', 'idle', 0, '2026-07-05T00:00:00Z', '2026-07-05T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("插入已有 split state 任务成功");

    OrchestratorRepo::init_schema(&pool)
        .await
        .expect("schema 增量初始化成功");

    let row = sqlx::query(
        "SELECT workflow_state, run_state FROM orchestrator_tasks WHERE id = 'task-blocked'",
    )
    .fetch_one(&pool)
    .await
    .expect("读取任务 split state 成功");

    assert_eq!(
        row.try_get::<String, _>("workflow_state")
            .expect("读取 workflow_state"),
        "backlog"
    );
    assert_eq!(
        row.try_get::<String, _>("run_state")
            .expect("读取 run_state"),
        "idle"
    );
}

/// Business Logic（为什么需要这个测试）:
///     曾经落库为 Todo/Queued 的排队任务在升级后仍应被 scheduler 领取，不能因为 run_state 旧值卡住。
///
/// Code Logic（这个测试做什么）:
///     创建已带 split state 的旧表并插入 queued/todo/queued，执行 init_schema 后断言只把 run_state 规范化为 idle。
#[tokio::test]
async fn init_schema_normalizes_existing_queued_split_state() {
    let pool = setup_raw_pool().await;
    sqlx::query(
        "CREATE TABLE orchestrator_tasks (
          id TEXT PRIMARY KEY,
          project_id TEXT NOT NULL,
          title TEXT NOT NULL,
          goal TEXT NOT NULL,
          acceptance_criteria TEXT NOT NULL,
          status TEXT NOT NULL,
          workflow_state TEXT NOT NULL DEFAULT 'backlog',
          run_state TEXT NOT NULL DEFAULT 'idle',
          priority INTEGER NOT NULL DEFAULT 0,
          branch_name TEXT,
          worktree_id TEXT,
          session_id TEXT,
          blocked_reason TEXT,
          attempt INTEGER NOT NULL DEFAULT 0,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          started_at TEXT,
          finished_at TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("创建带 split state 的任务表成功");
    sqlx::query(
        "INSERT INTO orchestrator_tasks \
         (id, project_id, title, goal, acceptance_criteria, status, workflow_state, \
          run_state, priority, created_at, updated_at) \
         VALUES ('task-queued', 'project-1', 'Queued task', 'goal', 'criteria', \
          'queued', 'todo', 'queued', 0, '2026-07-05T00:00:00Z', '2026-07-05T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("插入旧 queued split state 任务成功");

    OrchestratorRepo::init_schema(&pool)
        .await
        .expect("schema 增量初始化成功");

    let row = sqlx::query(
        "SELECT workflow_state, run_state FROM orchestrator_tasks WHERE id = 'task-queued'",
    )
    .fetch_one(&pool)
    .await
    .expect("读取任务 split state 成功");

    assert_eq!(
        row.try_get::<String, _>("workflow_state")
            .expect("读取 workflow_state"),
        "todo"
    );
    assert_eq!(
        row.try_get::<String, _>("run_state")
            .expect("读取 run_state"),
        "idle"
    );
}

/// Business Logic（为什么需要这个测试）:
///     远端 create 请求超时重试时，同一个 clientRequestId + 同项目 + 同 payload 必须返回第一次创建的任务，避免 owning device 产生重复任务。
///
/// Code Logic（这个测试做什么）:
///     用同一 clientRequestId 与语义等价 payload（仅 task id 不同）重放，断言第二次返回第一条任务且数据库只保留一条。
#[tokio::test]
async fn remote_create_client_request_is_idempotent() {
    let (_pool, repo) = setup_repo().await;
    let first = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);
    // 重放时 id 由客户端每次生成不同 UUID，但业务 payload 与 createAction 必须一致才算幂等。
    let mut second = task_row("task-2", "project-1", OrchestratorTaskStatus::Draft);
    second.title = first.title.clone();
    second.goal = first.goal.clone();
    second.acceptance_criteria = first.acceptance_criteria.clone();
    second.priority = first.priority;
    second.source = first.source.clone();
    second.external_id = first.external_id.clone();
    second.external_identifier = first.external_identifier.clone();
    second.external_url = first.external_url.clone();
    second.external_state = first.external_state.clone();
    second.external_labels = first.external_labels.clone();

    let created = repo
        .create_remote_task_for_client_request(
            "client-request-1",
            &first,
            OrchestratorCreateAction::Todo,
        )
        .await
        .unwrap();
    let replayed = repo
        .create_remote_task_for_client_request(
            "client-request-1",
            &second,
            OrchestratorCreateAction::Todo,
        )
        .await
        .unwrap();
    let listed = repo.list_tasks(Some("project-1")).await.unwrap();

    assert!(created.newly_created);
    assert!(!replayed.newly_created);
    assert_eq!(created.task.id, "task-1");
    assert_eq!(created.task.status, OrchestratorTaskStatus::Queued);
    assert_eq!(replayed.task.id, "task-1");
    assert_eq!(replayed.task.status, OrchestratorTaskStatus::Queued);
    assert_eq!(listed.len(), 1);
}

/// Business Logic（为什么需要这个测试）:
///     同一 clientRequestId 若被用于不同 project，绝不能返回 project A 的任务给 project B（数据泄露）。
///
/// Code Logic（这个测试做什么）:
///     先在 project-1 创建，再以相同 requestId 请求 project-2，断言 conflict 且 project-2 无任务。
#[tokio::test]
async fn remote_create_client_request_rejects_cross_project_reuse() {
    let (_pool, repo) = setup_repo().await;
    let first = task_row("task-a", "project-1", OrchestratorTaskStatus::Draft);
    let second = task_row("task-b", "project-2", OrchestratorTaskStatus::Draft);

    repo.create_remote_task_for_client_request(
        "shared-request",
        &first,
        OrchestratorCreateAction::Todo,
    )
    .await
    .unwrap();

    let err = repo
        .create_remote_task_for_client_request(
            "shared-request",
            &second,
            OrchestratorCreateAction::Todo,
        )
        .await
        .expect_err("跨项目复用 clientRequestId 必须 conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
    let listed_b = repo.list_tasks(Some("project-2")).await.unwrap();
    assert!(listed_b.is_empty(), "project-2 不得创建任务");
    let listed_a = repo.list_tasks(Some("project-1")).await.unwrap();
    assert_eq!(listed_a.len(), 1);
    assert_eq!(listed_a[0].id, "task-a");
}

/// Business Logic（为什么需要这个测试）:
///     同项目同 clientRequestId 但 payload 不同时，必须 conflict，避免静默返回错误内容的旧任务。
///
/// Code Logic（这个测试做什么）:
///     首次创建后用不同 goal 重放同一 requestId，断言 Conflict 且仍只保留第一条。
#[tokio::test]
async fn remote_create_client_request_rejects_same_project_payload_mismatch() {
    let (_pool, repo) = setup_repo().await;
    let first = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);
    let mut second = task_row("task-2", "project-1", OrchestratorTaskStatus::Draft);
    second.goal = "completely-different-goal".to_string();

    repo.create_remote_task_for_client_request(
        "payload-mismatch",
        &first,
        OrchestratorCreateAction::Todo,
    )
    .await
    .unwrap();
    let err = repo
        .create_remote_task_for_client_request(
            "payload-mismatch",
            &second,
            OrchestratorCreateAction::Todo,
        )
        .await
        .expect_err("同键不同 payload 必须 conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
    let listed = repo.list_tasks(Some("project-1")).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "task-1");
}

/// Business Logic（为什么需要这个函数）:
///     远端 HTTP createAction=todo 是移动端和远端代理的待执行入口，创建后的任务必须被本机 scheduler 领取。
///
/// Code Logic（这个函数做什么）:
///     通过幂等 create helper 创建 queued 任务，再执行全局 claim，断言 split state 从 Todo/Idle 进入 InProgress/Preparing。
#[tokio::test]
async fn remote_create_action_todo_result_is_claimable_by_split_state_scheduler() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let row = task_row("remote-created", "project-1", OrchestratorTaskStatus::Draft);

    let created = repo
        .create_remote_task_for_client_request(
            "client-request-claimable",
            &row,
            OrchestratorCreateAction::Todo,
        )
        .await
        .unwrap();
    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();

    assert!(created.newly_created);
    assert_eq!(created.task.status, OrchestratorTaskStatus::Queued);
    assert_eq!(created.task.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(created.task.run_state, OrchestratorRunState::Idle);
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, created.task.id);
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    assert_eq!(
        claimed[0].workflow_state,
        OrchestratorWorkflowState::InProgress
    );
    assert_eq!(claimed[0].run_state, OrchestratorRunState::Preparing);
}

/// Business Logic（为什么需要这个函数）:
///     创建弹窗默认动作是 Backlog，不能因为旧 queue 默认值导致任务被 scheduler 自动领取。
///
/// Code Logic（这个函数做什么）:
///     通过幂等 create helper 使用 backlog 动作创建任务，断言 legacy/split state 均保持草稿待办池语义。
#[tokio::test]
async fn remote_create_action_backlog_keeps_task_in_backlog_idle() {
    let (_pool, repo) = setup_repo().await;
    let row = task_row("remote-backlog", "project-1", OrchestratorTaskStatus::Draft);

    let created = repo
        .create_remote_task_for_client_request(
            "client-request-backlog",
            &row,
            OrchestratorCreateAction::Backlog,
        )
        .await
        .unwrap();

    assert!(created.newly_created);
    assert_eq!(created.task.status, OrchestratorTaskStatus::Draft);
    assert_eq!(
        created.task.workflow_state,
        OrchestratorWorkflowState::Backlog
    );
    assert_eq!(created.task.run_state, OrchestratorRunState::Idle);
}

/// Business Logic（为什么需要这个函数）:
///     Create and Start 在调度前的持久化语义必须与 Todo 一致，后续 dispatch 只能领取 Todo/Idle。
///
/// Code Logic（这个函数做什么）:
///     通过幂等 create helper 使用 start 动作创建任务，断言任务先落库为 legacy Queued + Todo/Idle。
#[tokio::test]
async fn remote_create_action_start_persists_as_todo_idle_before_dispatch() {
    let (_pool, repo) = setup_repo().await;
    let row = task_row("remote-start", "project-1", OrchestratorTaskStatus::Draft);

    let created = repo
        .create_remote_task_for_client_request(
            "client-request-start",
            &row,
            OrchestratorCreateAction::Start,
        )
        .await
        .unwrap();

    assert!(created.newly_created);
    assert_eq!(created.task.status, OrchestratorTaskStatus::Queued);
    assert_eq!(created.task.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(created.task.run_state, OrchestratorRunState::Idle);
}

/// Business Logic（为什么需要这个测试）:
///     NUL-join 指纹可在字段边界被内部 NUL 碰撞；结构化 JSON 编码必须区分此类不同语义元组。
///
/// Code Logic（这个测试做什么）:
///     构造 title/goal 跨字段 NUL 迁移的两份 payload，断言指纹不同，且同 clientRequestId 重放 conflict。
#[tokio::test]
async fn create_request_fingerprint_resists_nul_field_boundary_collision() {
    let mut left = task_row("task-left", "project-1", OrchestratorTaskStatus::Draft);
    left.title = "a\0goal=b".to_string();
    left.goal = "c".to_string();

    let mut right = task_row("task-right", "project-1", OrchestratorTaskStatus::Draft);
    right.title = "a".to_string();
    right.goal = "b\0goal=c".to_string();

    let fp_left = create_request_fingerprint(&left, OrchestratorCreateAction::Todo, &None).unwrap();
    let fp_right =
        create_request_fingerprint(&right, OrchestratorCreateAction::Todo, &None).unwrap();
    assert_ne!(
        fp_left, fp_right,
        "跨字段 NUL 迁移不得生成相同指纹: left={fp_left} right={fp_right}"
    );

    // 分隔符文本本身也不应碰撞：显式含 key 名的字段值 vs 真实字段赋值。
    let mut spoof = task_row("task-spoof", "project-1", OrchestratorTaskStatus::Draft);
    spoof.title = "x".to_string();
    spoof.goal = "y\0acceptance=z".to_string();
    spoof.acceptance_criteria = "".to_string();
    let mut real = task_row("task-real", "project-1", OrchestratorTaskStatus::Draft);
    real.title = "x".to_string();
    real.goal = "y".to_string();
    real.acceptance_criteria = "z".to_string();
    let fp_spoof =
        create_request_fingerprint(&spoof, OrchestratorCreateAction::Todo, &None).unwrap();
    let fp_real = create_request_fingerprint(&real, OrchestratorCreateAction::Todo, &None).unwrap();
    assert_ne!(fp_spoof, fp_real, "分隔符文本不得伪造字段边界");

    // 端到端：同 request id 的不同语义 payload 必须 conflict，不得误判幂等命中。
    let (_pool, repo) = setup_repo().await;
    repo.create_remote_task_for_client_request(
        "nul-collision-request",
        &left,
        OrchestratorCreateAction::Todo,
    )
    .await
    .unwrap();
    let err = repo
        .create_remote_task_for_client_request(
            "nul-collision-request",
            &right,
            OrchestratorCreateAction::Todo,
        )
        .await
        .expect_err("NUL 边界不同 payload 必须 conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
}

/// Business Logic（为什么需要这个测试）:
///     旧库映射行 request_fingerprint 为空时不能当通配符；同键不同 payload 必须 conflict，
///     强制客户端换新 request id，避免静默返回错误内容的旧任务。
///
/// Code Logic（这个测试做什么）:
///     直接插入空 fingerprint 映射与任务，再用不同 payload 调用幂等 create，断言 Conflict。
#[tokio::test]
async fn remote_create_legacy_empty_fingerprint_conflicts_on_payload_mismatch() {
    let (pool, repo) = setup_repo().await;
    let first = task_row("task-legacy", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&first).await.unwrap();
    sqlx::query(
        "INSERT INTO orchestrator_remote_task_create_requests \
         (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
         VALUES (?, ?, ?, '', ?, ?)",
    )
    .bind("legacy-empty-fp")
    .bind("project-1")
    .bind("task-legacy")
    .bind("2026-07-05T00:00:00Z")
    .bind("2026-07-05T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let mut second = task_row("task-new", "project-1", OrchestratorTaskStatus::Draft);
    second.goal = "different-payload-goal".to_string();
    let err = repo
        .create_remote_task_for_client_request(
            "legacy-empty-fp",
            &second,
            OrchestratorCreateAction::Todo,
        )
        .await
        .expect_err("legacy empty fingerprint must fail closed");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
    assert!(
        err.to_string().contains("缺少可靠请求指纹") || err.to_string().contains("clientRequestId"),
        "错误应提示指纹不可靠: {err}"
    );
    let listed = repo.list_tasks(Some("project-1")).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "task-legacy");
}

/// Business Logic（为什么需要这个测试）:
///     Start 幂等命中必须返回 newly_created=false，调用方据此跳过 dispatch，
///     防止任务 A 完成后同键重放启动队列中的任务 B。
///
/// Code Logic（这个测试做什么）:
///     首次 Start 创建 A，再创建排队任务 B 并标记 A 为 Done；同键重放 Start 断言
///     newly_created=false 且 B 仍保持 Todo/Idle（未被 claim）。
#[tokio::test]
async fn remote_create_start_replay_reports_not_newly_created_when_other_tasks_queued() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;

    let first = task_row("task-a", "project-1", OrchestratorTaskStatus::Draft);
    let created = repo
        .create_remote_task_for_client_request(
            "start-replay-key",
            &first,
            OrchestratorCreateAction::Start,
        )
        .await
        .unwrap();
    assert!(created.newly_created);
    assert_eq!(created.task.id, "task-a");

    // 模拟 A 已完成，队列中另有可调度任务 B。
    sqlx::query(
        "UPDATE orchestrator_tasks SET status='done', workflow_state='done', \
         run_state='idle', updated_at=? WHERE id=?",
    )
    .bind("2026-07-05T00:01:00Z")
    .bind("task-a")
    .execute(&pool)
    .await
    .unwrap();
    let queued_b = task_row("task-b", "project-1", OrchestratorTaskStatus::Queued);
    repo.create_task(&queued_b).await.unwrap();

    let mut replay_row = task_row("task-a-replay", "project-1", OrchestratorTaskStatus::Draft);
    replay_row.title = first.title.clone();
    replay_row.goal = first.goal.clone();
    replay_row.acceptance_criteria = first.acceptance_criteria.clone();
    let replayed = repo
        .create_remote_task_for_client_request(
            "start-replay-key",
            &replay_row,
            OrchestratorCreateAction::Start,
        )
        .await
        .unwrap();
    assert!(!replayed.newly_created);
    assert_eq!(replayed.task.id, "task-a");

    let b = repo.get_task("task-b").await.unwrap();
    assert_eq!(b.status, OrchestratorTaskStatus::Queued);
    assert_eq!(b.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(b.run_state, OrchestratorRunState::Idle);
    let listed = repo.list_tasks(Some("project-1")).await.unwrap();
    assert_eq!(listed.len(), 2);
}

/// Business Logic（为什么需要这个函数）:
///     调度器应优先处理高 priority 任务；同优先级下旧任务先执行，避免饥饿。
///
/// Code Logic（这个函数做什么）:
///     创建两条任务后直接固定 priority/created_at，再断言 list 使用 priority DESC, created_at ASC。
#[tokio::test]
async fn create_and_list_task_orders_by_priority_and_created() {
    let (pool, repo) = setup_repo().await;
    let older = task_row("older", "project-1", OrchestratorTaskStatus::Queued);
    let newer = task_row("newer", "project-1", OrchestratorTaskStatus::Queued);
    repo.create_task(&older).await.unwrap();
    repo.create_task(&newer).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET priority = ?, created_at = ? WHERE id = ?")
        .bind(1_i64)
        .bind("2026-07-05T00:00:00Z")
        .bind(&older.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET priority = ?, created_at = ? WHERE id = ?")
        .bind(2_i64)
        .bind("2026-07-05T01:00:00Z")
        .bind(&newer.id)
        .execute(&pool)
        .await
        .unwrap();

    let listed = repo.list_tasks(Some("project-1")).await.unwrap();
    let dto = OrchestratorTaskDto::from(listed[0].clone());

    assert_eq!(
        listed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec![newer.id.as_str(), older.id.as_str()]
    );
    assert_eq!(dto.status, OrchestratorTaskStatus::Queued);
}

/// Business Logic（为什么需要这个函数）:
///     状态更新不能破坏任务身份，避免调度器推进状态时丢失项目、标题和目标。
///
/// Code Logic（这个函数做什么）:
///     创建 Draft 后更新为 Queued，断言 id/project/title 保持不变且状态改变。
#[tokio::test]
async fn set_task_status_preserves_identity() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let updated = repo
        .set_task_status(&created.id, OrchestratorTaskStatus::Queued, None)
        .await
        .unwrap();

    assert_eq!(updated.id, created.id);
    assert_eq!(updated.project_id, "project-1");
    assert_eq!(updated.title, "Task task-1");
    assert_eq!(updated.goal, "Goal task-1");
    assert_eq!(updated.status, OrchestratorTaskStatus::Queued);
}

/// Business Logic（为什么需要这个函数）:
///     用户只能把草稿任务送入队列，避免运行中或已完成任务被人工重复排队。
///
/// Code Logic（这个函数做什么）:
///     创建 Draft 任务后调用安全入队方法，断言任务进入 Queued 且身份字段保持不变。
#[tokio::test]
async fn queue_task_allows_draft_task() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let queued = repo.queue_task(&created.id).await.unwrap();

    assert_eq!(queued.id, created.id);
    assert_eq!(queued.project_id, created.project_id);
    assert_eq!(queued.title, created.title);
    assert_eq!(queued.status, OrchestratorTaskStatus::Queued);
    assert_eq!(queued.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(queued.run_state, OrchestratorRunState::Idle);
}

/// Business Logic（为什么需要这个函数）:
///     手动入队仍保留 legacy Queued 状态，但 split state 必须进入 scheduler 可领取的 Todo/Idle。
///
/// Code Logic（这个函数做什么）:
///     创建本机项目和 Draft 任务，queue_task 后执行全局 claim，断言任务能进入 Preparing。
#[tokio::test]
async fn queue_task_result_is_claimable_by_split_state_scheduler() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let created = task_row("task-claimable", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let queued = repo.queue_task(&created.id).await.unwrap();
    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();

    assert_eq!(queued.status, OrchestratorTaskStatus::Queued);
    assert_eq!(queued.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(queued.run_state, OrchestratorRunState::Idle);
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, created.id);
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    assert_eq!(
        claimed[0].workflow_state,
        OrchestratorWorkflowState::InProgress
    );
    assert_eq!(claimed[0].run_state, OrchestratorRunState::Preparing);
    assert_eq!(
        claimed[0].attempt_phase,
        Some(OrchestratorAttemptPhase::PreparingWorkspace)
    );
}

/// Business Logic（为什么需要这个函数）:
///     看板拖拽只改 workflow_state，不得把 Draft 变成可调度的 Queued；否则启用自动化后拖入 Todo 会隐式启动 Claude。
///
/// Code Logic（这个函数做什么）:
///     创建 Draft 任务并 move 到 Todo，再 claim，断言 0 领取且 status 仍为 Draft。
#[tokio::test]
async fn draft_moved_to_todo_is_not_claimable_without_queue() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let created = task_row(
        "draft-todo-drag",
        "project-1",
        OrchestratorTaskStatus::Draft,
    );
    repo.create_task(&created).await.unwrap();
    let moved = repo
        .move_task_workflow_state(&created.id, OrchestratorWorkflowState::Todo)
        .await
        .unwrap();
    assert_eq!(moved.status, OrchestratorTaskStatus::Draft);
    assert_eq!(moved.workflow_state, OrchestratorWorkflowState::Todo);

    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(4)
        .await
        .unwrap();
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(claimed.is_empty());
    assert_eq!(persisted.status, OrchestratorTaskStatus::Draft);
    assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
}

/// Business Logic（为什么需要这个函数）:
///     claim 后崩溃留下的 Preparing 必须可被回收，否则全局槽位永久耗尽。
///
/// Code Logic（这个函数做什么）:
///     插入过期 Preparing 任务，调用 recover_stale_local_preparing_tasks，断言变为 Blocked 并释放 active 计数。
#[tokio::test]
async fn recover_stale_local_preparing_tasks_blocks_and_frees_capacity() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    repo.create_task(&task_row(
        "stale-preparing",
        "project-1",
        OrchestratorTaskStatus::Preparing,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "stale-preparing",
        OrchestratorWorkflowState::InProgress,
        OrchestratorRunState::Preparing,
        None,
    )
    .await;
    sqlx::query("UPDATE orchestrator_tasks SET updated_at = '2020-01-01T00:00:00Z' WHERE id = ?")
        .bind("stale-preparing")
        .execute(&pool)
        .await
        .unwrap();

    let recovered = repo
        .recover_stale_local_preparing_tasks(std::time::Duration::from_secs(60))
        .await
        .unwrap();
    let task = repo.get_task("stale-preparing").await.unwrap();
    let active_count: i64 = sqlx::query(
        "SELECT COUNT(*) AS count FROM orchestrator_tasks \
         WHERE run_state IN ('preparing','running','verifying','delivering')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .try_get("count")
    .unwrap();

    assert_eq!(recovered, 1);
    assert_eq!(task.status, OrchestratorTaskStatus::Blocked);
    assert_eq!(task.run_state, OrchestratorRunState::Blocked);
    assert!(task.blocked_reason.is_some());
    // Blocked 不占 active 容量。
    assert_eq!(active_count, 0);
}

/// Business Logic（为什么需要这个函数）:
///     长 git worktree 创建可能超过调度 lease；prepare 续租后，并发 recover 不得误杀仍在 Preparing 的任务。
///
/// Code Logic（这个函数做什么）:
///     插入 Preparing 任务并故意把 updated_at 回拨到 2020；调用 touch_preparing_lease 刷新后，
///     recover_stale_local_preparing_tasks(60s) 必须返回 0 且状态仍为 Preparing。
#[tokio::test]
async fn touch_preparing_lease_keeps_active_prepare_from_stale_recovery() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    repo.create_task(&task_row(
        "live-preparing",
        "project-1",
        OrchestratorTaskStatus::Preparing,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "live-preparing",
        OrchestratorWorkflowState::InProgress,
        OrchestratorRunState::Preparing,
        None,
    )
    .await;
    sqlx::query(
        "UPDATE orchestrator_tasks SET updated_at = '2020-01-01T00:00:00Z', prepare_claim_token = 'token-live' WHERE id = ?",
    )
    .bind("live-preparing")
    .execute(&pool)
    .await
    .unwrap();

    let touched = repo
        .touch_preparing_lease("live-preparing", "token-live")
        .await
        .unwrap();
    assert!(touched, "仍在 Preparing 时续租必须命中");
    assert!(!repo
        .touch_preparing_lease("live-preparing", "token-stale")
        .await
        .unwrap());

    let recovered = repo
        .recover_stale_local_preparing_tasks(std::time::Duration::from_secs(60))
        .await
        .unwrap();
    let task = repo.get_task("live-preparing").await.unwrap();

    assert_eq!(recovered, 0);
    assert_eq!(task.status, OrchestratorTaskStatus::Preparing);
    assert_eq!(task.run_state, OrchestratorRunState::Preparing);
    assert!(
        task.updated_at.as_str() > "2020-01-01T00:00:00Z",
        "touch 必须刷新 updated_at"
    );
}

/// Business Logic（为什么需要这个函数）:
///     旧 Preparing runner lease 过期被回收后，retry 会签发新 claim token；旧 touch/mark 不得劫持新 claim。
///
/// Code Logic（这个函数做什么）:
///     claim 得 token-A → 回收 → 重回 Queued 再 claim 得 token-B；旧 token-A 的 touch/mark 不命中，token-B 可 mark Running。
#[tokio::test]
async fn prepare_claim_token_cas_blocks_stale_runner_hijack() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let created = task_row("task-aba", "project-1", OrchestratorTaskStatus::Queued);
    repo.create_task(&created).await.unwrap();
    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    let token_a = claimed[0]
        .prepare_claim_token
        .clone()
        .expect("claim 必须签发 token");

    let recovered = repo
        .recover_stale_local_preparing_tasks(std::time::Duration::from_secs(0))
        .await
        .unwrap();
    assert_eq!(recovered, 1);
    sqlx::query(
        "UPDATE orchestrator_tasks SET status = 'queued', workflow_state = 'todo', run_state = 'idle',              prepare_claim_token = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(Utc::now().to_rfc3339())
    .bind("task-aba")
    .execute(&pool)
    .await
    .unwrap();
    let claimed2 = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();
    assert_eq!(claimed2.len(), 1);
    let token_b = claimed2[0]
        .prepare_claim_token
        .clone()
        .expect("新 claim 必须签发新 token");
    assert_ne!(token_a, token_b);

    assert!(!repo
        .touch_preparing_lease("task-aba", &token_a)
        .await
        .unwrap());
    assert!(repo
        .touch_preparing_lease("task-aba", &token_b)
        .await
        .unwrap());

    let hijack = repo
        .mark_task_running_attempt(
            "task-aba",
            "agent/old",
            "wt-old",
            "sess-old",
            1,
            &token_a,
            None,
            &RunnerAttemptPolicy::claude_default(),
        )
        .await
        .unwrap();
    assert_eq!(hijack.status, OrchestratorTaskStatus::Preparing);
    assert_ne!(hijack.session_id.as_deref(), Some("sess-old"));

    let running = repo
        .mark_task_running_attempt(
            "task-aba",
            "agent/new",
            "wt-new",
            "sess-new",
            1,
            &token_b,
            None,
            &RunnerAttemptPolicy::claude_default(),
        )
        .await
        .unwrap();
    assert_eq!(running.status, OrchestratorTaskStatus::Running);
    assert_eq!(running.session_id.as_deref(), Some("sess-new"));
    assert!(running.prepare_claim_token.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     旧 runner 在 phase CAS 未命中时若采用返回行的新 token，会绕过 claim 世代隔离继续 prepare。
///
/// Code Logic（这个函数做什么）:
///     claim token-A → 回收 → 再 claim token-B；旧 token-A 的 update_task_attempt_phase 必须返回 None，
///     且不得把 phase 写成旧 runner 想写的值覆盖新 claim。
#[tokio::test]
async fn update_task_attempt_phase_miss_returns_none_on_stale_claim_token() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let created = task_row(
        "task-phase-aba",
        "project-1",
        OrchestratorTaskStatus::Queued,
    );
    repo.create_task(&created).await.unwrap();
    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();
    let token_a = claimed[0]
        .prepare_claim_token
        .clone()
        .expect("claim 必须签发 token");

    let recovered = repo
        .recover_stale_local_preparing_tasks(std::time::Duration::from_secs(0))
        .await
        .unwrap();
    assert_eq!(recovered, 1);
    sqlx::query(
        "UPDATE orchestrator_tasks SET status = 'queued', workflow_state = 'todo', run_state = 'idle',              prepare_claim_token = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(Utc::now().to_rfc3339())
    .bind("task-phase-aba")
    .execute(&pool)
    .await
    .unwrap();
    let claimed2 = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();
    let token_b = claimed2[0]
        .prepare_claim_token
        .clone()
        .expect("新 claim 必须签发新 token");
    assert_ne!(token_a, token_b);

    let miss = repo
        .update_task_attempt_phase(
            "task-phase-aba",
            OrchestratorAttemptPhase::BuildingPrompt,
            &token_a,
        )
        .await
        .unwrap();
    assert!(miss.is_none(), "旧 token phase CAS 必须 miss");

    let hit = repo
        .update_task_attempt_phase(
            "task-phase-aba",
            OrchestratorAttemptPhase::PreparingWorkspace,
            &token_b,
        )
        .await
        .unwrap()
        .expect("新 token 必须命中");
    assert_eq!(
        hit.attempt_phase,
        Some(OrchestratorAttemptPhase::PreparingWorkspace)
    );
    assert_eq!(hit.prepare_claim_token.as_deref(), Some(token_b.as_str()));
}

/// Business Logic（为什么需要这个函数）:
///     verifier failed 修复转换必须签发 claim token，否则 repair prepare 会因空 token 失败。
///
/// Code Logic（这个函数做什么）:
///     Verifying 任务调用 try_transition_verifying_to_preparing_with_claim，断言 Preparing/Rework
///     且 prepare_claim_token 非空。
#[tokio::test]
async fn verifying_to_preparing_with_claim_issues_token() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-verify-claim",
        "project-1",
        OrchestratorTaskStatus::Verifying,
    );
    repo.create_task(&task).await.unwrap();

    let updated = repo
        .try_transition_verifying_to_preparing_with_claim(&task.id)
        .await
        .unwrap()
        .expect("Verifying 任务应转换");
    assert_eq!(updated.status, OrchestratorTaskStatus::Preparing);
    assert_eq!(updated.workflow_state, OrchestratorWorkflowState::Rework);
    assert_eq!(updated.run_state, OrchestratorRunState::Preparing);
    assert_eq!(
        updated.attempt_phase,
        Some(OrchestratorAttemptPhase::Failed)
    );
    let token = updated
        .prepare_claim_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .expect("修复轮必须签发 claim token");
    assert!(!token.is_empty());
}

/// Business Logic（为什么需要这个函数）:
///     显式 startTask 动作需要把草稿或待办任务放入 scheduler 可领取路径，而不是直接进入交付或清理现场。
///
/// Code Logic（这个函数做什么）:
///     创建 Backlog/Draft 任务后调用 start_task，断言任务进入 Todo/Idle 且可被全局 scheduler claim 到 Preparing。
#[tokio::test]
async fn start_task_moves_backlog_draft_to_claimable_todo_idle() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "project-1", "local").await;
    let created = task_row("task-start", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let started = repo.start_task(&created.id).await.unwrap();
    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(1)
        .await
        .unwrap();

    assert_eq!(started.status, OrchestratorTaskStatus::Queued);
    assert_eq!(started.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(started.run_state, OrchestratorRunState::Idle);
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, created.id);
    assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    assert_eq!(
        claimed[0].workflow_state,
        OrchestratorWorkflowState::InProgress
    );
    assert_eq!(claimed[0].run_state, OrchestratorRunState::Preparing);
}

/// Business Logic（为什么需要这个函数）:
///     人工复核失败时用户需要显式 requestRework，并保留原因、证据与执行现场供下一轮 scheduler 继续接管。
///
/// Code Logic（这个函数做什么）:
///     构造 HumanReview/Done-compatible 任务并写入现场字段，调用 request_task_rework 后断言进入 Rework/Idle、
///     worktree/session 未清理，且 repairPrompt evidence 保存了人工返工原因。
#[tokio::test]
async fn request_rework_from_human_review_records_reason_and_keeps_execution_site() {
    let (pool, repo) = setup_repo().await;
    let mut created = task_row("task-rework", "project-1", OrchestratorTaskStatus::Done);
    created.worktree_id = Some("worktree-1".to_string());
    created.session_id = Some("session-1".to_string());
    repo.create_task(&created).await.unwrap();
    set_task_split_state(
        &pool,
        &created.id,
        OrchestratorWorkflowState::HumanReview,
        OrchestratorRunState::Idle,
        None,
    )
    .await;

    let rework = repo
        .request_task_rework(&created.id, "请补充边界条件测试")
        .await
        .unwrap();
    let evidence = repo.list_evidence(&created.id).await.unwrap();

    assert_eq!(rework.status, OrchestratorTaskStatus::Queued);
    assert_eq!(rework.workflow_state, OrchestratorWorkflowState::Rework);
    assert_eq!(rework.run_state, OrchestratorRunState::Idle);
    assert_eq!(rework.worktree_id.as_deref(), Some("worktree-1"));
    assert_eq!(rework.session_id.as_deref(), Some("session-1"));
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].kind, EVIDENCE_KIND_REPAIR_PROMPT);
    assert_eq!(evidence[0].content, "请补充边界条件测试");
}

/// Business Logic（为什么需要这个函数）:
///     显式 deliverReviewedTask 只能从人工复核泳道进入交付，不能把普通 Done 或其它状态偷偷交付。
///
/// Code Logic（这个函数做什么）:
///     构造 HumanReview/Done-compatible 任务并进入 Merging/Delivering，再验证普通 Done 任务被拒绝且状态不变。
#[tokio::test]
async fn start_delivery_requires_human_review_task() {
    let (pool, repo) = setup_repo().await;
    let reviewed = task_row("task-reviewed", "project-1", OrchestratorTaskStatus::Done);
    repo.create_task(&reviewed).await.unwrap();
    set_task_split_state(
        &pool,
        &reviewed.id,
        OrchestratorWorkflowState::HumanReview,
        OrchestratorRunState::Idle,
        None,
    )
    .await;
    let plain_done = task_row("task-done", "project-1", OrchestratorTaskStatus::Done);
    repo.create_task(&plain_done).await.unwrap();

    let delivering = repo
        .start_delivery_from_human_review(&reviewed.id)
        .await
        .unwrap();
    let error = repo
        .start_delivery_from_human_review(&plain_done.id)
        .await
        .expect_err("plain done task must not deliver");
    let persisted_plain_done = repo.get_task(&plain_done.id).await.unwrap();

    assert_eq!(delivering.status, OrchestratorTaskStatus::Delivering);
    assert_eq!(
        delivering.workflow_state,
        OrchestratorWorkflowState::Merging
    );
    assert_eq!(delivering.run_state, OrchestratorRunState::Delivering);
    assert!(error.to_string().contains("人工复核"));
    assert_eq!(persisted_plain_done.status, OrchestratorTaskStatus::Done);
    assert_eq!(
        persisted_plain_done.workflow_state,
        OrchestratorWorkflowState::Done
    );
}

/// Business Logic（为什么需要这个测试）:
///     commit 边界 digest 漂移后，Delivering 任务必须 CAS 回 Human Review，供前端强制 re-review。
///
/// Code Logic（这个测试做什么）:
///     先 start_delivery_from_human_review 切入 Delivering，再 revert_delivery_to_human_review；
///     断言 status=Done、workflow=HumanReview、run=Idle；对已非 Delivering 任务调用应保持不变。
#[tokio::test]
async fn revert_delivery_to_human_review_cas_from_delivering() {
    let (pool, repo) = setup_repo().await;
    let reviewed = task_row("task-revert-hr", "project-1", OrchestratorTaskStatus::Done);
    repo.create_task(&reviewed).await.unwrap();
    set_task_split_state(
        &pool,
        &reviewed.id,
        OrchestratorWorkflowState::HumanReview,
        OrchestratorRunState::Idle,
        None,
    )
    .await;
    let delivering = repo
        .start_delivery_from_human_review(&reviewed.id)
        .await
        .unwrap();
    assert_eq!(delivering.status, OrchestratorTaskStatus::Delivering);

    let reverted = repo
        .revert_delivery_to_human_review(&reviewed.id)
        .await
        .unwrap();
    assert_eq!(reverted.status, OrchestratorTaskStatus::Done);
    assert_eq!(
        reverted.workflow_state,
        OrchestratorWorkflowState::HumanReview
    );
    assert_eq!(reverted.run_state, OrchestratorRunState::Idle);
    assert_eq!(
        reverted.attempt_phase,
        Some(OrchestratorAttemptPhase::Succeeded)
    );
    assert!(reverted.blocked_reason.is_none());

    // 已不在 Delivering：CAS miss 返回当前 Human Review 行。
    let again = repo
        .revert_delivery_to_human_review(&reviewed.id)
        .await
        .unwrap();
    assert_eq!(again.status, OrchestratorTaskStatus::Done);
    assert_eq!(again.workflow_state, OrchestratorWorkflowState::HumanReview);
}

/// Business Logic（为什么需要这个函数）:
///     显式 cancelTask 需要把任务移到 Canceled/Idle，同时保留 worktree、session 和既有 evidence 供用户审计。
///
/// Code Logic（这个函数做什么）:
///     创建 Running 任务并写入 evidence，调用 cancel_task 后断言 legacy status 映射为 Aborted、
///     split state 为 Canceled/Idle，现场字段与 evidence 均未丢失。
#[tokio::test]
async fn cancel_task_moves_to_canceled_idle_and_preserves_execution_site_and_evidence() {
    let (_pool, repo) = setup_repo().await;
    let mut created = task_row("task-cancel", "project-1", OrchestratorTaskStatus::Running);
    created.worktree_id = Some("worktree-1".to_string());
    created.session_id = Some("session-1".to_string());
    repo.create_task(&created).await.unwrap();
    repo.add_evidence(
        &created.id,
        "verificationOutput",
        "验证",
        "running",
        "still running",
    )
    .await
    .unwrap();

    let canceled = repo.cancel_task(&created.id).await.unwrap();
    let evidence = repo.list_evidence(&created.id).await.unwrap();

    assert_eq!(canceled.status, OrchestratorTaskStatus::Aborted);
    assert_eq!(canceled.workflow_state, OrchestratorWorkflowState::Canceled);
    assert_eq!(canceled.run_state, OrchestratorRunState::Idle);
    assert_eq!(canceled.worktree_id.as_deref(), Some("worktree-1"));
    assert_eq!(canceled.session_id.as_deref(), Some("session-1"));
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].content, "still running");
}

/// Business Logic（为什么需要这个测试）:
///     已 Done 任务不能被 cancel 覆写，否则丢失交付终态。
///
/// Code Logic（这个测试做什么）:
///     创建 Done 任务，cancel_task 必须 conflict，状态仍为 Done。
#[tokio::test]
async fn cancel_task_refuses_done_status() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row(
        "task-done-cancel",
        "project-1",
        OrchestratorTaskStatus::Done,
    );
    repo.create_task(&created).await.unwrap();

    let err = repo
        .cancel_task(&created.id)
        .await
        .expect_err("Done 不能取消");
    assert!(
        err.to_string().contains("已完成") || err.to_string().contains("不能取消"),
        "unexpected: {err}"
    );
    let persisted = repo.get_task(&created.id).await.unwrap();
    assert_eq!(persisted.status, OrchestratorTaskStatus::Done);
}

/// Business Logic（为什么需要这个测试）:
///     Abort 不得把 Done 改成 Aborted。
///
/// Code Logic（这个测试做什么）:
///     创建 Done 任务，abort_task_preserving_done 必须 conflict。
#[tokio::test]
async fn abort_task_preserving_done_refuses_done_status() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-done-abort", "project-1", OrchestratorTaskStatus::Done);
    repo.create_task(&created).await.unwrap();

    let err = repo
        .abort_task_preserving_done(&created.id)
        .await
        .expect_err("Done 不能终止");
    assert!(
        err.to_string().contains("已完成") || err.to_string().contains("不能终止"),
        "unexpected: {err}"
    );
    let persisted = repo.get_task(&created.id).await.unwrap();
    assert_eq!(persisted.status, OrchestratorTaskStatus::Done);
}

/// Business Logic（为什么需要这个测试）:
///     对非 Done 任务 Abort 应成功，且幂等返回 Aborted。
///
/// Code Logic（这个测试做什么）:
///     Running → abort → Aborted；再次 abort 仍 Aborted 且 Ok。
#[tokio::test]
async fn abort_task_preserving_done_aborts_running_and_is_idempotent() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row(
        "task-abort-run",
        "project-1",
        OrchestratorTaskStatus::Running,
    );
    repo.create_task(&created).await.unwrap();

    let aborted = repo.abort_task_preserving_done(&created.id).await.unwrap();
    assert_eq!(aborted.status, OrchestratorTaskStatus::Aborted);
    let again = repo.abort_task_preserving_done(&created.id).await.unwrap();
    assert_eq!(again.status, OrchestratorTaskStatus::Aborted);
}

/// Business Logic（为什么需要这个测试）:
///     owner delivery 租约必须挡住并发 abort，释放后才允许终止。
///
/// Code Logic（这个测试做什么）:
///     acquire lease → abort conflict → release → abort 成功。
#[tokio::test]
async fn delivery_lease_blocks_abort_until_released() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row(
        "task-lease-abort",
        "project-1",
        OrchestratorTaskStatus::Delivering,
    );
    repo.create_task(&created).await.unwrap();

    let acquired = repo
        .try_acquire_delivery_lease(&created.id, "holder-a", 600)
        .await
        .unwrap();
    assert!(acquired, "first acquire must succeed");
    let err = repo
        .abort_task_preserving_done(&created.id)
        .await
        .expect_err("abort must conflict while leased");
    assert!(
        err.to_string().contains("交付") || err.to_string().contains("conflict"),
        "unexpected: {err}"
    );
    repo.release_delivery_lease(&created.id, "holder-a")
        .await
        .unwrap();
    let aborted = repo.abort_task_preserving_done(&created.id).await.unwrap();
    assert_eq!(aborted.status, OrchestratorTaskStatus::Aborted);
}

/// Business Logic（为什么需要这个测试）:
///     delivery 获取租约后第二 holder 不得抢占。
///
/// Code Logic（这个测试做什么）:
///     holder-a 占租约；holder-b 再 acquire 返回 false。
#[tokio::test]
async fn delivery_lease_acquire_is_exclusive() {
    let (_pool, repo) = setup_repo().await;
    assert!(repo
        .try_acquire_delivery_lease("task-lease-x", "holder-a", 600)
        .await
        .unwrap());
    assert!(!repo
        .try_acquire_delivery_lease("task-lease-x", "holder-b", 600)
        .await
        .unwrap());
    assert!(repo
        .try_acquire_delivery_lease("task-lease-x", "holder-a", 600)
        .await
        .unwrap());
}

/// Business Logic（为什么需要这个函数）:
///     非草稿任务可能已经在执行、完成或阻塞，重复入队会回退状态并丢失阻塞原因。
///
/// Code Logic（这个函数做什么）:
///     分别构造 Running、Done、Blocked 任务并调用安全入队方法，断言返回错误且数据库行保持原状态。
#[tokio::test]
async fn queue_task_rejects_non_draft_without_mutating_task() {
    let (_pool, repo) = setup_repo().await;
    let cases = [
        ("running-task", OrchestratorTaskStatus::Running, None),
        ("done-task", OrchestratorTaskStatus::Done, None),
        (
            "blocked-task",
            OrchestratorTaskStatus::Blocked,
            Some("等待人工确认".to_string()),
        ),
    ];

    for (id, status, blocked_reason) in cases {
        let mut created = task_row(id, "project-1", status);
        created.blocked_reason = blocked_reason.clone();
        repo.create_task(&created).await.unwrap();

        let result = repo.queue_task(&created.id).await;
        let persisted = repo.get_task(&created.id).await.unwrap();

        assert!(result.is_err());
        assert_eq!(persisted.status, status);
        assert_eq!(persisted.blocked_reason, blocked_reason);
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户在看板拖拽任务时，只允许把非运行中任务移动到相邻泳道，避免一次跨越多个阶段绕过工作流约束。
///
/// Code Logic（这个函数做什么）:
///     创建 Backlog/Idle 任务并移动到 Todo，断言只更新 workflow_state，不改变 legacy status 或 run_state。
#[tokio::test]
async fn move_task_workflow_state_allows_adjacent_lane() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("drag-adjacent", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let moved = repo
        .move_task_workflow_state(&created.id, OrchestratorWorkflowState::Todo)
        .await
        .unwrap();

    assert_eq!(moved.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(moved.run_state, OrchestratorRunState::Idle);
    assert_eq!(moved.status, OrchestratorTaskStatus::Draft);
}

/// Business Logic（为什么需要这个函数）:
///     看板拖拽不应允许从 Backlog 直接跳到 HumanReview 等非相邻泳道，否则会绕过 Todo/InProgress 的任务语义。
///
/// Code Logic（这个函数做什么）:
///     创建 Backlog 任务后尝试跨泳道移动，断言返回中文错误且持久化 workflow_state 不变。
#[tokio::test]
async fn move_task_workflow_state_rejects_non_adjacent_lane() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("drag-cross", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();

    let error = repo
        .move_task_workflow_state(&created.id, OrchestratorWorkflowState::HumanReview)
        .await
        .expect_err("跨泳道移动应失败");
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(error.to_string().contains("只能移动到相邻泳道"));
    assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Backlog);
}

/// Business Logic（为什么需要这个函数）:
///     Runner 已经接管的任务不能被拖拽改变工作流阶段，否则 UI 状态会与真实执行现场冲突。
///
/// Code Logic（这个函数做什么）:
///     创建 Running 任务后尝试移动到相邻泳道，断言返回运行中中文错误且 workflow_state 不变。
#[tokio::test]
async fn move_task_workflow_state_rejects_running_task() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("drag-running", "project-1", OrchestratorTaskStatus::Running);
    repo.create_task(&created).await.unwrap();

    let error = repo
        .move_task_workflow_state(&created.id, OrchestratorWorkflowState::HumanReview)
        .await
        .expect_err("运行中任务拖拽应失败");
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(error.to_string().contains("运行中的任务不能通过拖拽移动"));
    assert_eq!(
        persisted.workflow_state,
        OrchestratorWorkflowState::InProgress
    );
}

/// Business Logic（为什么需要这个函数）:
///     拖拽移动和 scheduler claim 可能并发发生，旧拖拽快照不能覆盖已经变化的 workflow/run state。
///
/// Code Logic（这个函数做什么）:
///     读取 Backlog/Idle 快照后先把数据库改成 Todo/Idle，再用旧快照执行移动，断言 CAS 未命中且持久化状态不被覆盖。
#[tokio::test]
async fn move_task_workflow_state_rejects_stale_snapshot_without_overwrite() {
    let (pool, repo) = setup_repo().await;
    let created = task_row("drag-stale", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&created).await.unwrap();
    let stale_snapshot = repo.get_task(&created.id).await.unwrap();
    set_task_split_state(
        &pool,
        &created.id,
        OrchestratorWorkflowState::Todo,
        OrchestratorRunState::Idle,
        None,
    )
    .await;

    let error = repo
        .move_task_workflow_state_from_snapshot(&stale_snapshot, OrchestratorWorkflowState::Todo)
        .await
        .expect_err("旧快照不应覆盖已变化的任务状态");
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(error.to_string().contains("任务状态已变化"));
    assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Todo);
    assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Draft);
}

/// Business Logic（为什么需要这个函数）:
///     完成验证和重试任务都依赖 expected-status 原子转移，状态已被其它流程推进时不得被旧操作覆盖。
///
/// Code Logic（这个函数做什么）:
///     构造 Running 任务但要求 Blocked->Queued 转移，断言返回错误且数据库状态仍为 Running。
#[tokio::test]
async fn transition_task_status_rejects_unexpected_status_without_mutating_task() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Running);
    repo.create_task(&created).await.unwrap();

    let result = repo
        .transition_task_status(
            &created.id,
            OrchestratorTaskStatus::Blocked,
            OrchestratorTaskStatus::Queued,
            None,
        )
        .await;
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(result.is_err());
    assert_eq!(persisted.status, OrchestratorTaskStatus::Running);
    assert!(persisted.blocked_reason.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成后只能由 Running 任务进入 Verifying，成功路径必须返回更新后的完整任务 Row。
///
/// Code Logic（这个函数做什么）:
///     创建 Running 任务后执行 Running->Verifying 条件更新，断言返回值和持久化状态一致。
#[tokio::test]
async fn transition_task_status_moves_running_to_verifying() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Running);
    repo.create_task(&created).await.unwrap();

    let updated = repo
        .transition_task_status(
            &created.id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Verifying,
            None,
        )
        .await
        .unwrap();
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert_eq!(updated.status, OrchestratorTaskStatus::Verifying);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Verifying);
    assert_eq!(updated.id, created.id);
}

/// Business Logic（为什么需要这个函数）:
///     Verifying→Delivering 需要可跳过的 expected-status 转换，状态被 Abort 抢先改变时命令层应返回当前任务而不是报错覆盖。
///
/// Code Logic（这个函数做什么）:
///     创建 Aborted 任务后尝试按 Verifying 预期切到 Delivering，断言返回 None 且持久化状态不变。
#[tokio::test]
async fn try_transition_task_status_returns_none_for_unexpected_status() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Aborted);
    repo.create_task(&created).await.unwrap();

    let transitioned = repo
        .try_transition_task_status(
            &created.id,
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Delivering,
            None,
        )
        .await
        .unwrap();
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(transitioned.is_none());
    assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
}

/// Business Logic（为什么需要这个函数）:
///     旧 terminal session 的迟到 completion sentinel 不能推进当前 active runner attempt。
///
/// Code Logic（这个函数做什么）:
///     构造 Running 任务 active attempt=2/session-2，先用旧 attempt/session 尝试转移并断言 no-op，再用匹配值转到 Verifying。
#[tokio::test]
async fn try_transition_running_attempt_to_verifying_requires_active_attempt_and_session() {
    let (_pool, repo) = setup_repo().await;
    let mut created = task_row("task-1", "project-1", OrchestratorTaskStatus::Running);
    created.attempt = 2;
    created.session_id = Some("session-2".to_string());
    created.worktree_id = Some("worktree-1".to_string());
    repo.create_task(&created).await.unwrap();

    let stale_attempt = repo
        .try_transition_running_attempt_to_verifying(&created.id, 1, "session-1")
        .await
        .unwrap();
    let stale_session = repo
        .try_transition_running_attempt_to_verifying(&created.id, 2, "session-1")
        .await
        .unwrap();
    let persisted = repo.get_task(&created.id).await.unwrap();

    assert!(stale_attempt.is_none());
    assert!(stale_session.is_none());
    assert_eq!(persisted.status, OrchestratorTaskStatus::Running);
    assert_eq!(persisted.attempt, 2);
    assert_eq!(persisted.session_id.as_deref(), Some("session-2"));

    let transitioned = repo
        .try_transition_running_attempt_to_verifying(&created.id, 2, "session-2")
        .await
        .unwrap()
        .expect("active runner should transition");

    assert_eq!(transitioned.status, OrchestratorTaskStatus::Verifying);
    assert_eq!(transitioned.attempt, 2);
    assert_eq!(transitioned.session_id.as_deref(), Some("session-2"));
}

/// Business Logic（为什么需要这个函数）:
///     用户可能在 delivery 完成写 Done 前终止任务，完成写入不得覆盖用户终止状态。
///
/// Code Logic（这个函数做什么）:
///     创建 Delivering 任务后先模拟用户终止为 Aborted，再调用完成 helper，断言任务仍保持 Aborted。
#[tokio::test]
async fn finish_task_done_does_not_override_aborted_task() {
    let (_pool, repo) = setup_repo().await;
    let created = task_row("task-1", "project-1", OrchestratorTaskStatus::Delivering);
    repo.create_task(&created).await.unwrap();
    repo.set_task_status(&created.id, OrchestratorTaskStatus::Aborted, None)
        .await
        .unwrap();

    let finished = repo.finish_task_done(&created.id).await.unwrap();
    let persisted = repo.get_task(&created.id).await.unwrap();

    match finished {
        crate::orchestrator::repo::FinishTaskDoneOutcome::CasMiss(row) => {
            assert_eq!(row.status, OrchestratorTaskStatus::Aborted);
        }
        crate::orchestrator::repo::FinishTaskDoneOutcome::Transitioned(_) => {
            panic!("aborted task must not transition to Done");
        }
    }
    assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
    assert!(persisted.finished_at.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     legacy 项目配置默认应提供完整自动化开关组合，但必须保持 disabled，避免旧数据被误用于自动执行。
///
/// Code Logic（这个函数做什么）:
///     通过 get_or_create_project_config 创建缺失项目配置，并断言 full auto 相关默认值与 enabled=false。
#[tokio::test]
async fn config_defaults_to_full_auto_but_disabled() {
    let (_pool, repo) = setup_repo().await;
    let config = repo
        .get_or_create_project_config("project-1")
        .await
        .unwrap();

    assert!(!config.enabled);
    assert_eq!(config.max_concurrent_tasks, 1);
    assert!(config.auto_commit);
    assert!(config.auto_push_task_branch);
    assert!(config.auto_merge_to_main);
    assert!(config.auto_push_main);
    assert!(config.retain_worktree_on_blocked);
}

/// Business Logic（为什么需要这个函数）:
///     调度、验证和交付阶段需要把事件和证据持久化，后续详情页才能追溯执行过程。
///
/// Code Logic（这个函数做什么）:
///     追加一条 event 与 evidence，再按 task_id 查询对应表确认 kind/summary/content 已落库。
#[tokio::test]
async fn events_and_evidence_are_persisted() {
    let (pool, repo) = setup_repo().await;
    let task = task_row("task-1", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&task).await.unwrap();

    repo.add_event(
        &task.id,
        "queued",
        "任务已进入队列",
        Some(r#"{"source":"test"}"#),
    )
    .await
    .unwrap();
    repo.add_evidence(&task.id, "command", "cargo test", "tests passed", "ok")
        .await
        .unwrap();

    let events = sqlx::query(
        "SELECT kind, message, payload_json FROM orchestrator_task_events \
         WHERE task_id = ? ORDER BY created_at ASC",
    )
    .bind(&task.id)
    .fetch_all(&pool)
    .await
    .unwrap();
    let evidence = sqlx::query(
        "SELECT kind, title, summary, content FROM orchestrator_task_evidence \
         WHERE task_id = ? ORDER BY created_at ASC",
    )
    .bind(&task.id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].try_get::<String, _>("kind").unwrap(), "queued");
    assert_eq!(
        events[0].try_get::<String, _>("message").unwrap(),
        "任务已进入队列"
    );
    assert_eq!(
        events[0]
            .try_get::<Option<String>, _>("payload_json")
            .unwrap(),
        Some(r#"{"source":"test"}"#.to_string())
    );
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].try_get::<String, _>("kind").unwrap(), "command");
    assert_eq!(
        evidence[0].try_get::<String, _>("title").unwrap(),
        "cargo test"
    );
    assert_eq!(
        evidence[0].try_get::<String, _>("summary").unwrap(),
        "tests passed"
    );
    assert_eq!(evidence[0].try_get::<String, _>("content").unwrap(), "ok");
}

/// Business Logic（为什么需要这个函数）:
///     Orchestrator 首页需要能展示所有项目的任务，不能强制带 project_id 筛选。
///
/// Code Logic（这个函数做什么）:
///     插入两个不同项目的任务，调用 list_tasks(None) 后断言全局列表包含两条。
#[tokio::test]
async fn list_tasks_none_returns_global_list() {
    let (_pool, repo) = setup_repo().await;
    let first = task_row("task-1", "project-1", OrchestratorTaskStatus::Queued);
    let second = task_row("task-2", "project-2", OrchestratorTaskStatus::Queued);
    repo.create_task(&first).await.unwrap();
    repo.create_task(&second).await.unwrap();

    let listed = repo.list_tasks(None).await.unwrap();

    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|task| task.id == first.id));
    assert!(listed.iter().any(|task| task.id == second.id));
}

/// Business Logic（为什么需要这个测试）:
///     workbench_launch_summary 任务区需要有界全局查询，且不得循环按项目拉任务。
///
/// Code Logic（这个测试做什么）:
///     插入 draft/queued/running/blocked/humanReview 与多余 running；list_launch_tasks(5)
///     只返回 interesting 且最多 5 条。
#[tokio::test]
async fn workbench_launch_summary_list_launch_tasks_bounded_no_n_plus_one() {
    let (_pool, repo) = setup_repo().await;
    let mut draft = task_row("t-draft", "p1", OrchestratorTaskStatus::Draft);
    draft.updated_at = "2026-07-05T00:00:00Z".into();
    let mut queued = task_row("t-queued", "p1", OrchestratorTaskStatus::Queued);
    queued.updated_at = "2026-07-05T00:01:00Z".into();
    let mut running = task_row("t-run", "p1", OrchestratorTaskStatus::Running);
    running.updated_at = "2026-07-05T00:05:00Z".into();
    let mut blocked = task_row("t-block", "p2", OrchestratorTaskStatus::Blocked);
    blocked.updated_at = "2026-07-05T00:04:00Z".into();
    let mut human = task_row("t-human", "p2", OrchestratorTaskStatus::Done);
    human.workflow_state = OrchestratorWorkflowState::HumanReview;
    human.updated_at = "2026-07-05T00:03:00Z".into();
    for i in 0..5 {
        let mut extra = task_row(
            &format!("t-extra-{i}"),
            "p3",
            OrchestratorTaskStatus::Running,
        );
        extra.updated_at = format!("2026-07-05T00:1{i}:00Z");
        repo.create_task(&extra).await.unwrap();
    }
    repo.create_task(&draft).await.unwrap();
    repo.create_task(&queued).await.unwrap();
    repo.create_task(&running).await.unwrap();
    repo.create_task(&blocked).await.unwrap();
    repo.create_task(&human).await.unwrap();

    let listed = repo.list_launch_tasks(5).await.unwrap();
    assert_eq!(listed.len(), 5);
    assert!(!listed
        .iter()
        .any(|t| t.id == "t-draft" || t.id == "t-queued"));
    assert!(listed.iter().any(|t| t.id == "t-extra-4"));
}

/// Business Logic（为什么需要这个函数）:
///     命令层读取不存在任务时应得到项目统一 not-found 错误，而不是空 Row 或 panic。
///
/// Code Logic（这个函数做什么）:
///     查询缺失 id 并断言仓储返回 Err，保持当前代码库 AppError::not_found 风格。
#[tokio::test]
async fn get_task_returns_not_found_for_missing_task() {
    let (_pool, repo) = setup_repo().await;

    let result = repo.get_task("missing-task").await;

    assert!(result.is_err());
}

/// Business Logic（为什么需要这个函数）:
///     数据库中若出现未知状态字符串，应显式失败，避免调度器误处理损坏数据。
///
/// Code Logic（这个函数做什么）:
///     手动写入非法 status 任务，再通过 get_task 读取并断言返回错误。
#[tokio::test]
async fn get_task_returns_error_for_invalid_status() {
    let (pool, repo) = setup_repo().await;
    sqlx::query(
        "INSERT INTO orchestrator_tasks \
         (id, project_id, title, goal, acceptance_criteria, status, priority, attempt, \
          created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("bad-status")
    .bind("project-1")
    .bind("Bad")
    .bind("Goal")
    .bind("Criteria")
    .bind("not-a-status")
    .bind(0_i64)
    .bind(0_i64)
    .bind("2026-07-05T00:00:00Z")
    .bind("2026-07-05T00:00:00Z")
    .execute(&pool)
    .await
    .unwrap();

    let result = repo.get_task("bad-status").await;

    assert!(result.is_err());
}

/// Business Logic（为什么需要这个函数）:
///     调度器只能扫描用户显式启用自动编排的项目，默认 disabled 项目不得被后台领取任务。
///
/// Code Logic（这个函数做什么）:
///     创建两个项目配置，仅把一个更新为 enabled=1，再断言 enabled 列表只返回该项目。
#[tokio::test]
async fn list_enabled_project_configs_returns_only_enabled_projects() {
    let (pool, repo) = setup_repo().await;
    repo.get_or_create_project_config("project-enabled")
        .await
        .unwrap();
    repo.get_or_create_project_config("project-disabled")
        .await
        .unwrap();
    sqlx::query("UPDATE orchestrator_project_config SET enabled = 1 WHERE project_id = ?")
        .bind("project-enabled")
        .execute(&pool)
        .await
        .unwrap();

    let configs = repo.list_enabled_project_configs().await.unwrap();

    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].project_id, "project-enabled");
    assert!(configs[0].enabled);
}

/// Business Logic（为什么需要这个函数）:
///     项目级并发控制只应统计 Preparing/Running/Verifying/Delivering，Queued 和终态不占用执行槽位。
///
/// Code Logic（这个函数做什么）:
///     为同一项目插入所有关键状态任务，断言 active count 仅包含四个执行中阶段。
#[tokio::test]
async fn count_active_tasks_counts_only_in_progress_statuses() {
    let (_pool, repo) = setup_repo().await;
    for (id, status) in [
        ("queued", OrchestratorTaskStatus::Queued),
        ("preparing", OrchestratorTaskStatus::Preparing),
        ("running", OrchestratorTaskStatus::Running),
        ("verifying", OrchestratorTaskStatus::Verifying),
        ("delivering", OrchestratorTaskStatus::Delivering),
        ("done", OrchestratorTaskStatus::Done),
        ("blocked", OrchestratorTaskStatus::Blocked),
    ] {
        repo.create_task(&task_row(id, "project-1", status))
            .await
            .unwrap();
    }

    let active = repo.count_active_tasks("project-1").await.unwrap();

    assert_eq!(active, 4);
}

/// Business Logic（为什么需要这个函数）:
///     Phase 3 后并发容量是设备级全局容量，只应统计所有本机 local Workbench 项目的执行中任务。
///
/// Code Logic（这个函数做什么）:
///     插入 local、remote 和缺失 Workbench 项目的 active 任务，断言全局本机计数只包含 local 项目。
#[tokio::test]
async fn count_active_local_tasks_counts_only_local_workbench_projects() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "local-a", "local").await;
    insert_workbench_project(&pool, "local-b", "local").await;
    insert_workbench_project(&pool, "remote-a", "remote").await;
    for (id, project_id, status) in [
        (
            "local-preparing",
            "local-a",
            OrchestratorTaskStatus::Preparing,
        ),
        ("local-running", "local-b", OrchestratorTaskStatus::Running),
        (
            "remote-running",
            "remote-a",
            OrchestratorTaskStatus::Running,
        ),
        (
            "missing-running",
            "missing",
            OrchestratorTaskStatus::Running,
        ),
        ("local-queued", "local-a", OrchestratorTaskStatus::Queued),
    ] {
        repo.create_task(&task_row(id, project_id, status))
            .await
            .unwrap();
    }

    let active = repo.count_active_local_tasks().await.unwrap();

    assert_eq!(active, 2);
}

/// Business Logic（为什么需要这个函数）:
///     全局 scheduler 需要在所有本机 local 项目之间按统一优先级领取任务，并跳过远端项目。
///
/// Code Logic（这个函数做什么）:
///     先制造 1 个本机 active 任务，再按全局 limit=3 领取 queued，断言只领取剩余 2 个 local 任务且排序正确。
#[tokio::test]
async fn claim_next_local_queued_tasks_with_global_capacity_claims_remaining_local_slots() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "local-a", "local").await;
    insert_workbench_project(&pool, "local-b", "local").await;
    insert_workbench_project(&pool, "remote-a", "remote").await;
    repo.create_task(&task_row(
        "active-local",
        "local-a",
        OrchestratorTaskStatus::Running,
    ))
    .await
    .unwrap();
    for (id, project_id, priority) in [
        ("remote-high", "remote-a", 100_i64),
        ("local-high", "local-b", 50_i64),
        ("local-low", "local-a", 10_i64),
        ("local-wait", "local-b", 1_i64),
    ] {
        repo.create_task(&task_row(id, project_id, OrchestratorTaskStatus::Queued))
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

    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(3)
        .await
        .unwrap();
    let remote = repo.get_task("remote-high").await.unwrap();
    let waiting = repo.get_task("local-wait").await.unwrap();
    let active = repo.count_active_local_tasks().await.unwrap();

    assert_eq!(
        claimed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        vec!["local-high", "local-low"]
    );
    assert!(claimed
        .iter()
        .all(|task| task.status == OrchestratorTaskStatus::Preparing
            && task.workflow_state == OrchestratorWorkflowState::InProgress
            && task.run_state == OrchestratorRunState::Preparing
            && task.attempt_phase == Some(OrchestratorAttemptPhase::PreparingWorkspace)));
    assert_eq!(remote.status, OrchestratorTaskStatus::Queued);
    assert_eq!(waiting.status, OrchestratorTaskStatus::Queued);
    assert_eq!(active, 3);
}

/// Business Logic（为什么需要这个测试）:
///     Blocked 表示用户必须显式 retry 才能再跑；后台 scheduler 若把 Blocked 当可领取态，
///     会造成无界自动重跑、Attention 闪烁，并破坏 Blocked→Queued 用户契约。
///
/// Code Logic（这个测试做什么）:
///     写入一个 legacy Blocked + run_state=Blocked 的本机任务；全局 claim 后断言 0 条领取，
///     且任务仍保持 Blocked（含 blocked_reason）。
#[tokio::test]
async fn claim_next_local_queued_tasks_does_not_auto_claim_blocked() {
    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "local-a", "local").await;
    repo.create_task(&task_row(
        "blocked-task",
        "local-a",
        OrchestratorTaskStatus::Blocked,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "blocked-task",
        OrchestratorWorkflowState::Rework,
        OrchestratorRunState::Blocked,
        Some("verification failed"),
    )
    .await;

    let claimed = repo
        .claim_next_local_queued_tasks_with_global_capacity(3)
        .await
        .unwrap();
    let still = repo.get_task("blocked-task").await.unwrap();

    assert!(claimed.is_empty(), "Blocked 不得被后台 scheduler 自动领取");
    assert_eq!(still.status, OrchestratorTaskStatus::Blocked);
    assert_eq!(still.run_state, OrchestratorRunState::Blocked);
    assert_eq!(still.blocked_reason.as_deref(), Some("verification failed"));
}

/// Business Logic（为什么需要这个测试）:
///     有界 claim 候选读取必须稳定分页且排除 remote/Draft/Blocked，否则阶段 A 无法替换无界 SELECT。
///
/// Code Logic（这个测试做什么）:
///     插入 300 条同优先级 local Queued/Idle 任务，并混入 remote/Draft/Blocked 噪声；
///     断言 page1=256、page2=44、并集 300 唯一 ID，排序为 priority DESC/created_at ASC/id ASC，
///     且每行 JOIN 已带 project path，无需二次查询。
#[tokio::test]
async fn claim_candidate_list_keyset_pages_300_and_skips_noise() {
    use crate::orchestrator::claim::{ClaimScanCursor, CLAIM_CANDIDATE_LIMIT};
    use std::collections::HashSet;

    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "local-a", "local").await;
    insert_workbench_project(&pool, "remote-a", "remote").await;

    // 300 条同优先级 local Queued/Idle；id 零填充保证字典序与插入序一致。
    for i in 0..300 {
        let id = format!("q-{i:04}");
        let created_at = format!("2026-07-05T00:{:02}:{:02}Z", i / 60, i % 60);
        let mut row = task_row(&id, "local-a", OrchestratorTaskStatus::Queued);
        row.priority = 10;
        row.created_at = created_at.clone();
        row.updated_at = created_at;
        repo.create_task(&row).await.unwrap();
        set_task_split_state(
            &pool,
            &id,
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
    }

    // 噪声：remote queued、local draft、local blocked，均不得进入候选。
    repo.create_task(&task_row(
        "remote-queued",
        "remote-a",
        OrchestratorTaskStatus::Queued,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "remote-queued",
        OrchestratorWorkflowState::Todo,
        OrchestratorRunState::Idle,
        None,
    )
    .await;
    repo.create_task(&task_row(
        "local-draft",
        "local-a",
        OrchestratorTaskStatus::Draft,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "local-draft",
        OrchestratorWorkflowState::Todo,
        OrchestratorRunState::Idle,
        None,
    )
    .await;
    repo.create_task(&task_row(
        "local-blocked",
        "local-a",
        OrchestratorTaskStatus::Blocked,
    ))
    .await
    .unwrap();
    set_task_split_state(
        &pool,
        "local-blocked",
        OrchestratorWorkflowState::Rework,
        OrchestratorRunState::Blocked,
        Some("hold"),
    )
    .await;

    let page1 = repo
        .list_local_queued_claim_candidates(None, CLAIM_CANDIDATE_LIMIT)
        .await
        .expect("page1");
    assert_eq!(page1.len(), 256, "page1 必须正好 256");
    assert_eq!(CLAIM_CANDIDATE_LIMIT, 256);

    // 请求更大 limit 仍被硬上限夹到 256。
    let capped = repo
        .list_local_queued_claim_candidates(None, 10_000)
        .await
        .expect("capped");
    assert_eq!(capped.len(), 256);

    let last = page1.last().expect("page1 non-empty");
    assert_eq!(
        last.project_path.as_os_str(),
        std::ffi::OsStr::new("/tmp/local-a"),
        "JOIN 必须直接带回 project path"
    );
    let cursor = ClaimScanCursor::from_task(&last.task);

    let page2 = repo
        .list_local_queued_claim_candidates(Some(&cursor), CLAIM_CANDIDATE_LIMIT)
        .await
        .expect("page2");
    assert_eq!(page2.len(), 44, "page2 必须正好 44");

    let mut union = HashSet::new();
    let mut ordered_ids = Vec::new();
    for candidate in page1.iter().chain(page2.iter()) {
        assert_eq!(
            candidate.project_path.as_os_str(),
            std::ffi::OsStr::new("/tmp/local-a")
        );
        assert_eq!(candidate.task.status, OrchestratorTaskStatus::Queued);
        assert_eq!(candidate.task.run_state, OrchestratorRunState::Idle);
        assert_eq!(candidate.task.project_id, "local-a");
        assert!(
            union.insert(candidate.task.id.clone()),
            "候选 ID 不得重复: {}",
            candidate.task.id
        );
        ordered_ids.push(candidate.task.id.clone());
    }
    assert_eq!(union.len(), 300);
    assert_eq!(ordered_ids.len(), 300);

    // 全序：priority DESC, created_at ASC, id ASC
    let mut expected = ordered_ids.clone();
    expected.sort_by(|a, b| {
        // 本 fixture priority 全相同，按 id 即 created_at/id 升序（id 与 created 同步编码）
        a.cmp(b)
    });
    assert_eq!(ordered_ids, expected);

    // 噪声 ID 不得出现
    for noise in ["remote-queued", "local-draft", "local-blocked"] {
        assert!(
            !union.contains(noise),
            "噪声任务 {noise} 不得进入 claim 候选"
        );
    }

    // 第三页应为空（已耗尽）
    let last2 = page2.last().expect("page2 non-empty");
    let page3 = repo
        .list_local_queued_claim_candidates(
            Some(&ClaimScanCursor::from_task(&last2.task)),
            CLAIM_CANDIDATE_LIMIT,
        )
        .await
        .expect("page3");
    assert!(page3.is_empty());
}

/// Business Logic（为什么需要这个测试）:
///     跨优先级 keyset 必须先消耗更高 priority，再按 created_at/id 前进，否则高优任务会被饿死。
///
/// Code Logic（这个测试做什么）:
///     插入不同 priority 的 local Queued 任务，断言首页顺序与 cursor 后第二页首条正确。
#[tokio::test]
async fn claim_candidate_list_orders_by_priority_desc_then_created_id_asc() {
    use crate::orchestrator::claim::ClaimScanCursor;

    let (pool, repo) = setup_repo().await;
    create_workbench_projects_table(&pool).await;
    insert_workbench_project(&pool, "local-a", "local").await;

    for (id, priority, created_at) in [
        ("mid-b", 50_i64, "2026-07-05T00:00:02Z"),
        ("high-a", 100, "2026-07-05T00:00:03Z"),
        ("mid-a", 50, "2026-07-05T00:00:01Z"),
        ("low-a", 10, "2026-07-05T00:00:00Z"),
        ("high-b", 100, "2026-07-05T00:00:04Z"),
    ] {
        let mut row = task_row(id, "local-a", OrchestratorTaskStatus::Queued);
        row.priority = priority;
        row.created_at = created_at.to_string();
        row.updated_at = created_at.to_string();
        repo.create_task(&row).await.unwrap();
        set_task_split_state(
            &pool,
            id,
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
    }

    let page = repo
        .list_local_queued_claim_candidates(None, 3)
        .await
        .expect("page");
    assert_eq!(
        page.iter().map(|c| c.task.id.as_str()).collect::<Vec<_>>(),
        vec!["high-a", "high-b", "mid-a"]
    );

    let cursor = ClaimScanCursor::from_task(&page[2].task);
    let rest = repo
        .list_local_queued_claim_candidates(Some(&cursor), 10)
        .await
        .expect("rest");
    assert_eq!(
        rest.iter().map(|c| c.task.id.as_str()).collect::<Vec<_>>(),
        vec!["mid-b", "low-a"]
    );
}

/// Business Logic（为什么需要这个函数）:
///     Phase 7 的 terminal completion sentinel 依赖 attempt history；schema 和仓储方法必须先能稳定记录和完成一轮尝试。
///
/// Code Logic（这个函数做什么）:
///     新增 running attempt 后调用 mark_attempt_completed，断言 status/completed_at 更新且 prompt/session 保持不变。
#[tokio::test]
async fn add_attempt_and_mark_attempt_completed_persists_attempt_history() {
    let (_pool, repo) = setup_repo().await;

    let created = repo
        .add_attempt(
            "task-1",
            1,
            "worktree-1",
            "session-1",
            "implement task\nORCHESTRATOR_DEV_DONE",
            &RunnerAttemptPolicy::claude_default(),
            None,
            OrchestratorAttemptStatus::Running,
        )
        .await
        .unwrap();
    let completed = repo.mark_attempt_completed("task-1", 1).await.unwrap();

    assert_eq!(created.task_id, "task-1");
    assert_eq!(created.status, "running");
    assert_eq!(completed.id, created.id);
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.session_id, "session-1");
    assert_eq!(completed.prompt, "implement task\nORCHESTRATOR_DEV_DONE");
    assert!(completed.completed_at.is_some());
}

/// Business Logic（为什么需要这个函数）:
///     completion hook 只拿得到 Workbench session_id，必须能反查仍在 running 的 attempt 才能定位 task。
///
/// Code Logic（这个函数做什么）:
///     插入 running 和 completed 两条 attempt，断言只返回 session 匹配且 status=running 的记录。
#[tokio::test]
async fn get_running_attempt_by_session_returns_only_running_attempt() {
    let (_pool, repo) = setup_repo().await;
    let running = repo
        .add_attempt(
            "task-running",
            1,
            "worktree-1",
            "session-running",
            "prompt",
            &RunnerAttemptPolicy::claude_default(),
            None,
            OrchestratorAttemptStatus::Running,
        )
        .await
        .unwrap();
    repo.add_attempt(
        "task-completed",
        1,
        "worktree-2",
        "session-completed",
        "prompt",
        &RunnerAttemptPolicy::claude_default(),
        None,
        OrchestratorAttemptStatus::Running,
    )
    .await
    .unwrap();
    repo.mark_attempt_completed("task-completed", 1)
        .await
        .unwrap();

    let found = repo
        .get_running_attempt_by_session("session-running")
        .await
        .unwrap()
        .expect("running attempt");
    let completed = repo
        .get_running_attempt_by_session("session-completed")
        .await
        .unwrap();

    assert_eq!(found.id, running.id);
    assert_eq!(found.task_id, "task-running");
    assert_eq!(found.status, "running");
    assert!(completed.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     调度器领取任务必须原子地把 Queued 切到 Preparing，避免两个 tick 或手动触发重复调度同一任务。
///
/// Code Logic（这个函数做什么）:
///     插入高低优先级 Queued 与 Draft 任务，claim 一次后断言只领取最高优先级 Queued 且状态变 Preparing。
#[tokio::test]
async fn claim_next_queued_task_claims_only_queued_task() {
    let (pool, repo) = setup_repo().await;
    let high = task_row("high", "project-1", OrchestratorTaskStatus::Queued);
    let low = task_row("low", "project-1", OrchestratorTaskStatus::Queued);
    let draft = task_row("draft", "project-1", OrchestratorTaskStatus::Draft);
    repo.create_task(&high).await.unwrap();
    repo.create_task(&low).await.unwrap();
    repo.create_task(&draft).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET priority = ? WHERE id = ?")
        .bind(10_i64)
        .bind(&high.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET priority = ? WHERE id = ?")
        .bind(1_i64)
        .bind(&low.id)
        .execute(&pool)
        .await
        .unwrap();

    let claimed = repo
        .claim_next_queued_task_with_capacity("project-1", 1)
        .await
        .unwrap()
        .expect("claimed task");
    let low_persisted = repo.get_task(&low.id).await.unwrap();
    let draft_persisted = repo.get_task(&draft.id).await.unwrap();

    assert_eq!(claimed.id, high.id);
    assert_eq!(claimed.status, OrchestratorTaskStatus::Preparing);
    assert_eq!(low_persisted.status, OrchestratorTaskStatus::Queued);
    assert_eq!(draft_persisted.status, OrchestratorTaskStatus::Draft);
}

/// Business Logic（为什么需要这个函数）:
///     max_concurrent_tasks=1 时，后台 tick 与手动 dispatch 不能各自领取一个任务打穿项目并发上限。
///
/// Code Logic（这个函数做什么）:
///     插入两条 Queued 任务，连续调用容量内领取两次，断言第二次返回 None 且 active 数量仍为 1。
#[tokio::test]
async fn claim_next_queued_task_with_capacity_stops_at_single_slot_limit() {
    let (_pool, repo) = setup_repo().await;
    repo.create_task(&task_row(
        "task-1",
        "project-1",
        OrchestratorTaskStatus::Queued,
    ))
    .await
    .unwrap();
    repo.create_task(&task_row(
        "task-2",
        "project-1",
        OrchestratorTaskStatus::Queued,
    ))
    .await
    .unwrap();

    let first = repo
        .claim_next_queued_task_with_capacity("project-1", 1)
        .await
        .unwrap();
    let second = repo
        .claim_next_queued_task_with_capacity("project-1", 1)
        .await
        .unwrap();
    let active = repo.count_active_tasks("project-1").await.unwrap();

    assert!(first.is_some());
    assert!(second.is_none());
    assert_eq!(active, 1);
}

/// Business Logic（为什么需要这个函数）:
///     max_concurrent_tasks=2 时，调度器应允许两个执行槽位被领取，但第三个必须等待下一轮容量释放。
///
/// Code Logic（这个函数做什么）:
///     插入三条 Queued 任务，连续容量内领取三次，断言前两次成功、第三次返回 None 且 active 数量为 2。
#[tokio::test]
async fn claim_next_queued_task_with_capacity_allows_two_slots_only() {
    let (_pool, repo) = setup_repo().await;
    for id in ["task-1", "task-2", "task-3"] {
        repo.create_task(&task_row(id, "project-1", OrchestratorTaskStatus::Queued))
            .await
            .unwrap();
    }

    let first = repo
        .claim_next_queued_task_with_capacity("project-1", 2)
        .await
        .unwrap();
    let second = repo
        .claim_next_queued_task_with_capacity("project-1", 2)
        .await
        .unwrap();
    let third = repo
        .claim_next_queued_task_with_capacity("project-1", 2)
        .await
        .unwrap();
    let active = repo.count_active_tasks("project-1").await.unwrap();

    assert!(first.is_some());
    assert!(second.is_some());
    assert!(third.is_none());
    assert_eq!(active, 2);
}

/// Business Logic（为什么需要这个函数）:
///     Runner 创建出 worktree 和 terminal 后，任务行必须持久化现场入口，供用户从 Orchestrator 接管。
///
/// Code Logic（这个函数做什么）:
///     创建 Preparing 任务后写入 branch/worktree/session，断言状态进入 Running 且关联字段落库。
#[tokio::test]
async fn mark_task_running_persists_runner_fields() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row("task-1", "project-1", OrchestratorTaskStatus::Preparing);
    repo.create_task(&task).await.unwrap();

    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    let running = repo
        .mark_task_running(
            &task.id,
            "agent/task-1-test",
            "worktree-1",
            "session-1",
            "tok",
        )
        .await
        .unwrap();

    assert_eq!(running.status, OrchestratorTaskStatus::Running);
    assert_eq!(running.branch_name.as_deref(), Some("agent/task-1-test"));
    assert_eq!(running.worktree_id.as_deref(), Some("worktree-1"));
    assert_eq!(running.session_id.as_deref(), Some("session-1"));
    assert!(running.started_at.is_some());
}

/// Business Logic（为什么需要这个函数）:
///     任务重试新 attempt 时不能继续展示上一轮 Claude transcript/session，否则用户会误以为新轮次已关联旧运行时。
///
/// Code Logic（这个函数做什么）:
///     先写入旧 runtime 字段，再把任务挂账为新 Running attempt，断言 runtime 字段被清空且 runtime_started_at 更新。
#[tokio::test]
async fn mark_task_running_attempt_clears_previous_runtime_and_sets_started_at() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-runtime-reset",
        "project-1",
        OrchestratorTaskStatus::Preparing,
    );
    repo.create_task(&task).await.unwrap();
    sqlx::query(
        "UPDATE orchestrator_tasks \
         SET claude_session_id = 'old-claude-session', transcript_path = '/tmp/old.jsonl', \
             runtime_started_at = '2026-07-05T00:00:00Z', last_activity_at = '2026-07-05T00:01:00Z', \
             last_runtime_event = 'assistant', last_runtime_message = 'old message' \
         WHERE id = ?",
    )
    .bind(&task.id)
    .execute(&repo.pool)
    .await
    .unwrap();

    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    let running = repo
        .mark_task_running_attempt(
            &task.id,
            "agent/task-runtime-reset",
            "worktree-2",
            "session-2",
            2,
            "tok",
            None,
            &RunnerAttemptPolicy::claude_default(),
        )
        .await
        .unwrap();

    assert_eq!(running.status, OrchestratorTaskStatus::Running);
    assert_eq!(running.attempt, 2);
    assert_eq!(running.session_id.as_deref(), Some("session-2"));
    assert_eq!(
        running.runner_provider.as_deref(),
        Some("claudeCodeVisible")
    );
    assert!(running.runtime_started_at.is_some());
    assert_ne!(
        running.runtime_started_at.as_deref(),
        Some("2026-07-05T00:00:00Z")
    );
    assert!(running.claude_session_id.is_none());
    assert!(running.transcript_path.is_none());
    assert!(running.last_activity_at.is_none());
    assert!(running.last_runtime_event.is_none());
    assert!(running.last_runtime_message.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     Runner 准备过程中需要把 PreparingWorkspace/BuildingPrompt/Streaming 等阶段写入任务行，供 UI 展示进度。
///
/// Code Logic（这个函数做什么）:
///     创建 Preparing 任务后调用 phase helper，断言 attempt_phase 和 updated_at 已持久化。
#[tokio::test]
async fn update_task_attempt_phase_persists_phase() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row("task-phase", "project-1", OrchestratorTaskStatus::Preparing);
    repo.create_task(&task).await.unwrap();

    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    let updated = repo
        .update_task_attempt_phase(&task.id, OrchestratorAttemptPhase::BuildingPrompt, "tok")
        .await
        .unwrap()
        .expect("matching claim token must hit phase CAS");

    assert_eq!(
        updated.attempt_phase,
        Some(OrchestratorAttemptPhase::BuildingPrompt)
    );
    assert_ne!(updated.updated_at, task.updated_at);
}

/// Business Logic（为什么需要这个函数）:
///     Runner 迟到异步流程可能来自旧 attempt/session；这些迟到 phase 或 runtime 更新不能覆盖当前 active runner。
///
/// Code Logic（这个函数做什么）:
///     挂账当前 Running attempt/session 后先用正确 guard 写入，再用旧 attempt/session 尝试覆盖，断言最终字段保持当前值。
#[tokio::test]
async fn active_runner_guard_prevents_old_phase_and_runtime_updates() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-active-guard",
        "project-1",
        OrchestratorTaskStatus::Preparing,
    );
    repo.create_task(&task).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    repo.mark_task_running_attempt(
        &task.id,
        "agent/task-active-guard",
        "worktree-2",
        "session-new",
        2,
        "tok",
        None,
        &RunnerAttemptPolicy::claude_default(),
    )
    .await
    .unwrap();

    repo.update_active_runner_attempt_phase(
        &task.id,
        2,
        "session-new",
        OrchestratorAttemptPhase::BuildingPrompt,
    )
    .await
    .unwrap();
    repo.update_active_runner_attempt_phase(
        &task.id,
        1,
        "session-old",
        OrchestratorAttemptPhase::Streaming,
    )
    .await
    .unwrap();

    let current_summary = crate::orchestrator::claude_runtime::ClaudeRuntimeSummary {
        claude_session_id: Some("claude-session-new".to_string()),
        transcript_path: Some("/tmp/new.jsonl".to_string()),
        last_activity_at: Some("2026-07-06T00:01:00Z".to_string()),
        last_runtime_event: Some("assistant".to_string()),
        last_runtime_message: Some("new message".to_string()),
    };
    let stale_summary = crate::orchestrator::claude_runtime::ClaudeRuntimeSummary {
        claude_session_id: Some("claude-session-old".to_string()),
        transcript_path: Some("/tmp/old.jsonl".to_string()),
        last_activity_at: Some("2026-07-06T00:02:00Z".to_string()),
        last_runtime_event: Some("assistant".to_string()),
        last_runtime_message: Some("old message".to_string()),
    };
    let updated = repo
        .update_active_runner_runtime_summary(&task.id, 2, "session-new", &current_summary)
        .await
        .unwrap()
        .expect("current runner runtime should update");
    assert_eq!(
        updated.claude_session_id.as_deref(),
        Some("claude-session-new")
    );

    let stale_update = repo
        .update_active_runner_runtime_summary(&task.id, 1, "session-old", &stale_summary)
        .await
        .unwrap();
    assert!(stale_update.is_none());

    let persisted = repo.get_task(&task.id).await.unwrap();
    assert_eq!(
        persisted.attempt_phase,
        Some(OrchestratorAttemptPhase::BuildingPrompt)
    );
    assert_eq!(
        persisted.claude_session_id.as_deref(),
        Some("claude-session-new")
    );
    assert_eq!(persisted.transcript_path.as_deref(), Some("/tmp/new.jsonl"));
    assert_eq!(
        persisted.last_runtime_message.as_deref(),
        Some("new message")
    );
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code visible runtime 关联成功后，只有当前 active attempt/session 能写入任务详情字段。
///
/// Code Logic（这个函数做什么）:
///     挂账 Running 任务后用匹配的 attempt/session 写入 ClaudeRuntimeSummary，断言所有 runtime 字段落库。
#[tokio::test]
async fn update_active_runner_runtime_summary_persists_claude_fields() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-runtime",
        "project-1",
        OrchestratorTaskStatus::Preparing,
    );
    repo.create_task(&task).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    repo.mark_task_running_attempt(
        &task.id,
        "agent/task-runtime",
        "worktree-1",
        "session-1",
        1,
        "tok",
        None,
        &RunnerAttemptPolicy::claude_default(),
    )
    .await
    .unwrap();
    let summary = crate::orchestrator::claude_runtime::ClaudeRuntimeSummary {
        claude_session_id: Some("claude-session-1".to_string()),
        transcript_path: Some("/tmp/transcript.jsonl".to_string()),
        last_activity_at: Some("2026-07-06T00:01:00Z".to_string()),
        last_runtime_event: Some("assistant".to_string()),
        last_runtime_message: Some("done".to_string()),
    };

    let updated = repo
        .update_active_runner_runtime_summary(&task.id, 1, "session-1", &summary)
        .await
        .unwrap()
        .expect("matching runner should update runtime");

    assert_eq!(
        updated.claude_session_id.as_deref(),
        Some("claude-session-1")
    );
    assert_eq!(
        updated.transcript_path.as_deref(),
        Some("/tmp/transcript.jsonl")
    );
    assert_eq!(
        updated.last_activity_at.as_deref(),
        Some("2026-07-06T00:01:00Z")
    );
    assert_eq!(updated.last_runtime_event.as_deref(), Some("assistant"));
    assert_eq!(updated.last_runtime_message.as_deref(), Some("done"));
}

/// Business Logic（为什么需要这个函数）:
///     verifier pass 且 delivery 关闭时需要进入 HumanReview/Idle/Succeeded，不能被 legacy Done 映射覆盖。
///
/// Code Logic（这个函数做什么）:
///     用 expected-status helper 从 Verifying 转为 legacy Done，同时指定自定义 split state 和 attempt phase。
#[tokio::test]
async fn try_transition_task_split_state_sets_custom_lane_and_phase() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-human-review",
        "project-1",
        OrchestratorTaskStatus::Verifying,
    );
    repo.create_task(&task).await.unwrap();

    let updated = repo
        .try_transition_task_split_state(
            &task.id,
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            Some(OrchestratorAttemptPhase::Succeeded),
            None,
        )
        .await
        .unwrap()
        .expect("transition should match");

    assert_eq!(updated.status, OrchestratorTaskStatus::Done);
    assert_eq!(
        updated.workflow_state,
        OrchestratorWorkflowState::HumanReview
    );
    assert_eq!(updated.run_state, OrchestratorRunState::Idle);
    assert_eq!(
        updated.attempt_phase,
        Some(OrchestratorAttemptPhase::Succeeded)
    );
}

/// Business Logic（为什么需要这个函数）:
///     用户可能在 Runner 准备 worktree/terminal 期间终止任务，Runner 的迟到成功回写不得覆盖终止状态。
///
/// Code Logic（这个函数做什么）:
///     创建 Preparing 任务后先模拟用户终止为 Aborted，再调用 mark_task_running，断言状态与 runner 字段都未被改写。
#[tokio::test]
async fn mark_task_running_does_not_override_aborted_task() {
    let (_pool, repo) = setup_repo().await;
    let task = task_row("task-1", "project-1", OrchestratorTaskStatus::Preparing);
    repo.create_task(&task).await.unwrap();
    repo.set_task_status(&task.id, OrchestratorTaskStatus::Aborted, None)
        .await
        .unwrap();

    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind(&task.id)
        .execute(&repo.pool)
        .await
        .unwrap();
    let returned = repo
        .mark_task_running(
            &task.id,
            "agent/task-1-test",
            "worktree-1",
            "session-1",
            "tok",
        )
        .await
        .unwrap();
    let persisted = repo.get_task(&task.id).await.unwrap();

    assert_eq!(returned.status, OrchestratorTaskStatus::Aborted);
    assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
    assert!(persisted.branch_name.is_none());
    assert!(persisted.worktree_id.is_none());
    assert!(persisted.session_id.is_none());
    assert!(persisted.started_at.is_none());
}

/// Business Logic（为什么需要这个函数）:
///     验证阶段写入的 evidence 必须能按任务读取，页面才能展示当前任务的验证输出。
///
/// Code Logic（这个函数做什么）:
///     追加一条 verificationOutput 证据，再按 task_id 读取并断言 kind 与内容保持原样。
#[tokio::test]
async fn evidence_is_listed_by_task() {
    let (_pool, repo) = setup_repo().await;

    repo.add_evidence(
        "task-1",
        "verificationOutput",
        "npm test",
        "passed",
        "output",
    )
    .await
    .unwrap();
    let items = repo.list_evidence("task-1").await.unwrap();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, "verificationOutput");
    assert_eq!(items[0].title, "npm test");
    assert_eq!(items[0].summary, "passed");
    assert_eq!(items[0].content, "output");
}

/// Business Logic（为什么需要这个辅助函数）:
///     Retry/Discard 单测需要快速把 outbox 推到 failed，才能验证 failed-only 状态机。
///
/// Code Logic（这个函数做什么）:
///     插入 pending → claim sending → mark failed，返回最终 failed 行。
async fn seed_failed_remote_outbox(
    repo: &OrchestratorRepo,
    request_json: &str,
    last_error: &str,
) -> OrchestratorRemoteOutboxRow {
    let item = repo
        .insert_remote_outbox_pending(
            "device-1",
            "Mac mini",
            "/Users/hans/project",
            None,
            request_json,
        )
        .await
        .expect("insert pending");
    repo.claim_remote_outbox_item_as_sending(&item.id)
        .await
        .expect("claim")
        .expect("claimed");
    repo.mark_remote_outbox_failed(&item.id, last_error)
        .await
        .expect("mark failed")
}

/// Business Logic（为什么需要这个测试）:
///     用户对协议/校验失败的 outbox 选择重新发送时，必须原子回到 pending，清空 last_error，
///     并完整保留原 request_json/clientRequestId，避免重复创建远端任务。
///
/// Code Logic（这个测试做什么）:
///     构造带 clientRequestId 的 failed item，调用 retry_failed_remote_outbox_item，
///     断言 status=pending、last_error 清空、request_json 原样保留；重复 retry 与非 failed 状态返回 conflict，
///     缺失 id 返回 not_found；discarded 不出现在 active/dispatcher 查询中。
#[tokio::test]
async fn retry_failed_remote_outbox() {
    let (_pool, repo) = setup_repo().await;
    let request_json = r#"{"projectId":"p1","title":"t","goal":"g","acceptanceCriteria":"a","clientRequestId":"client-req-42","createAction":"todo"}"#;
    let failed = seed_failed_remote_outbox(&repo, request_json, "项目不能为空").await;
    assert_eq!(failed.status, RemoteOutboxStatus::Failed);
    assert_eq!(failed.last_error.as_deref(), Some("项目不能为空"));
    assert_eq!(failed.request_json, request_json);

    let retried = repo
        .retry_failed_remote_outbox_item(&failed.id)
        .await
        .expect("retry failed");
    assert_eq!(retried.status, RemoteOutboxStatus::Pending);
    assert!(retried.last_error.is_none());
    assert_eq!(retried.request_json, request_json);
    assert!(retried.request_json.contains("client-req-42"));
    assert!(retried.remote_task_id.is_none());
    assert!(retried.sent_at.is_none());

    let pending = repo
        .list_pending_remote_outbox_items(20)
        .await
        .expect("list pending");
    assert!(pending.iter().any(|item| item.id == failed.id));

    let active = repo
        .list_remote_outbox_items_for_project_path("device-1", "/Users/hans/project")
        .await
        .expect("list active");
    assert!(active.iter().any(|item| item.id == failed.id));

    let duplicate = repo.retry_failed_remote_outbox_item(&failed.id).await;
    assert!(duplicate.is_err());
    let duplicate_err = duplicate.unwrap_err().to_string();
    assert!(
        duplicate_err.contains("只有失败的远端 outbox 可以重新发送"),
        "duplicate retry should be invalid-transition: {duplicate_err}"
    );

    let missing = repo
        .retry_failed_remote_outbox_item("missing-outbox-id")
        .await;
    assert!(missing.is_err());
    let missing_err = missing.unwrap_err().to_string();
    assert!(
        missing_err.contains("不存在"),
        "missing retry should be not-found: {missing_err}"
    );

    for status in [
        RemoteOutboxStatus::Pending,
        RemoteOutboxStatus::Sending,
        RemoteOutboxStatus::Mirrored,
        RemoteOutboxStatus::Discarded,
    ] {
        let item = repo
            .insert_remote_outbox_pending(
                "device-1",
                "Mac mini",
                "/Users/hans/project",
                None,
                request_json,
            )
            .await
            .expect("insert for rejection");
        sqlx::query(
            "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind("prior error")
        .bind(&item.id)
        .execute(&_pool)
        .await
        .expect("force status");
        let rejected = repo.retry_failed_remote_outbox_item(&item.id).await;
        assert!(
            rejected.is_err(),
            "retry should reject status {}",
            status.as_str()
        );
        let err = rejected.unwrap_err().to_string();
        assert!(
            err.contains("只有失败的远端 outbox 可以重新发送"),
            "status {} should be invalid-transition: {err}",
            status.as_str()
        );
        let persisted = repo
            .get_remote_outbox_item(&item.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(persisted.status, status);
        assert_eq!(persisted.request_json, request_json);
    }
}

/// Business Logic（为什么需要这个测试）:
///     用户放弃失败 outbox 后，条目必须进入 discarded 终态：保留 last_error 与 request 审计，
///     但不再出现在 active 列表或 dispatcher pending 队列中。
///
/// Code Logic（这个测试做什么）:
///     构造 failed item 后调用 discard_failed_remote_outbox_item，断言 status=discarded、
///     last_error/request_json 保留；重复 discard 与非 failed 状态返回 conflict，缺失返回 not_found；
///     discarded 被 active/pending 查询排除。
#[tokio::test]
async fn discard_failed_remote_outbox() {
    let (_pool, repo) = setup_repo().await;
    let request_json = r#"{"projectId":"p1","title":"t","goal":"g","acceptanceCriteria":"a","clientRequestId":"client-req-99","createAction":"backlog"}"#;
    let failed = seed_failed_remote_outbox(&repo, request_json, "远端拒绝: 校验失败").await;

    let discarded = repo
        .discard_failed_remote_outbox_item(&failed.id)
        .await
        .expect("discard failed");
    assert_eq!(discarded.status, RemoteOutboxStatus::Discarded);
    assert_eq!(discarded.last_error.as_deref(), Some("远端拒绝: 校验失败"));
    assert_eq!(discarded.request_json, request_json);
    assert!(discarded.request_json.contains("client-req-99"));

    let pending = repo
        .list_pending_remote_outbox_items(20)
        .await
        .expect("list pending");
    assert!(!pending.iter().any(|item| item.id == failed.id));

    let active = repo
        .list_remote_outbox_items_for_project_path("device-1", "/Users/hans/project")
        .await
        .expect("list active");
    assert!(!active.iter().any(|item| item.id == failed.id));

    let still_there = repo
        .get_remote_outbox_item(&failed.id)
        .await
        .expect("get discarded")
        .expect("audit retained");
    assert_eq!(still_there.status, RemoteOutboxStatus::Discarded);

    let duplicate = repo.discard_failed_remote_outbox_item(&failed.id).await;
    assert!(duplicate.is_err());
    let duplicate_err = duplicate.unwrap_err().to_string();
    assert!(
        duplicate_err.contains("只有失败的远端 outbox 可以放弃发送"),
        "duplicate discard should be invalid-transition: {duplicate_err}"
    );

    let missing = repo
        .discard_failed_remote_outbox_item("missing-outbox-id")
        .await;
    assert!(missing.is_err());
    let missing_err = missing.unwrap_err().to_string();
    assert!(
        missing_err.contains("不存在"),
        "missing discard should be not-found: {missing_err}"
    );

    for status in [
        RemoteOutboxStatus::Pending,
        RemoteOutboxStatus::Sending,
        RemoteOutboxStatus::Mirrored,
        RemoteOutboxStatus::Discarded,
    ] {
        let item = repo
            .insert_remote_outbox_pending(
                "device-1",
                "Mac mini",
                "/Users/hans/project",
                None,
                request_json,
            )
            .await
            .expect("insert for rejection");
        sqlx::query(
            "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind("prior error")
        .bind(&item.id)
        .execute(&_pool)
        .await
        .expect("force status");
        let rejected = repo.discard_failed_remote_outbox_item(&item.id).await;
        assert!(
            rejected.is_err(),
            "discard should reject status {}",
            status.as_str()
        );
        let err = rejected.unwrap_err().to_string();
        assert!(
            err.contains("只有失败的远端 outbox 可以放弃发送"),
            "status {} should be invalid-transition: {err}",
            status.as_str()
        );
        let persisted = repo
            .get_remote_outbox_item(&item.id)
            .await
            .expect("get")
            .expect("exists");
        assert_eq!(persisted.status, status);
        assert_eq!(persisted.last_error.as_deref(), Some("prior error"));
    }
}

/// 进入 HumanReview 时 state_version 必须 +1，供 operational 去重。
///
/// Business Logic（为什么需要这个测试）:
///     同一任务再次进入 HR 时必须换 version，否则 GUI 会丢未来通知。
///
/// Code Logic（这个测试做什么）:
///     Verifying→HR split 转移两次，断言 state_version 从 0→1→2。
#[tokio::test]
async fn operational_notification_event_bumps_state_version_on_human_review() {
    let (_pool, repo) = setup_repo().await;
    let mut row = task_row("task-hr", "proj", OrchestratorTaskStatus::Verifying);
    row.state_version = 0;
    repo.create_task(&row).await.unwrap();

    let first = repo
        .try_transition_task_split_state(
            "task-hr",
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            Some(OrchestratorAttemptPhase::Succeeded),
            None,
        )
        .await
        .unwrap()
        .expect("transition");
    assert_eq!(first.state_version, 1);
    assert_eq!(first.workflow_state, OrchestratorWorkflowState::HumanReview);

    // 再回到 Verifying 后再次进 HR
    repo.set_task_status("task-hr", OrchestratorTaskStatus::Verifying, None)
        .await
        .unwrap();
    let second = repo
        .try_transition_task_split_state(
            "task-hr",
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            Some(OrchestratorAttemptPhase::Succeeded),
            None,
        )
        .await
        .unwrap()
        .expect("second transition");
    assert_eq!(second.state_version, 2);
}

/// Blocked 与 finish_task_done 必须 bump state_version。
///
/// Business Logic（为什么需要这个测试）:
///     Blocked/Done 是运营通知 kind，version 不前进会导致去重键卡住。
///
/// Code Logic（这个测试做什么）:
///     Delivering→Blocked 再 Delivering→Done，断言 version 递增。
#[tokio::test]
async fn operational_notification_event_bumps_state_version_on_blocked_and_done() {
    let (_pool, repo) = setup_repo().await;
    let row = task_row("task-bd", "proj", OrchestratorTaskStatus::Delivering);
    repo.create_task(&row).await.unwrap();

    let blocked = repo
        .block_task_if_delivering("task-bd", "delivery failed")
        .await
        .unwrap();
    assert_eq!(blocked.status, OrchestratorTaskStatus::Blocked);
    assert_eq!(blocked.state_version, 1);

    repo.set_task_status("task-bd", OrchestratorTaskStatus::Delivering, None)
        .await
        .unwrap();
    let done = match repo.finish_task_done("task-bd").await.unwrap() {
        crate::orchestrator::repo::FinishTaskDoneOutcome::Transitioned(row) => row,
        crate::orchestrator::repo::FinishTaskDoneOutcome::CasMiss(row) => {
            panic!("expected Transitioned, got CasMiss status={:?}", row.status);
        }
    };
    assert_eq!(done.status, OrchestratorTaskStatus::Done);
    assert!(done.state_version >= 2);
}

/// snapshot 列表只含 opaque 字段且 HR 不与 taskDone 双计。
///
/// Business Logic（为什么需要这个测试）:
///     baseline 不得泄露 title，且 HumanReview(status=done) 不得再计 taskDone。
///
/// Code Logic（这个测试做什么）:
///     插入 HR/Blocked/Done/outboxFailed，list items 断言 kind 集合与 opaque id，无 title。
#[tokio::test]
async fn operational_notification_snapshot_lists_privacy_safe_current_items() {
    use crate::orchestrator::models::OperationalNotificationKind;
    let (_pool, repo) = setup_repo().await;

    let mut hr = task_row("t-hr", "p", OrchestratorTaskStatus::Done);
    hr.workflow_state = OrchestratorWorkflowState::HumanReview;
    hr.title = "SECRET_TITLE".into();
    hr.state_version = 3;
    hr.updated_at = "2026-07-15T03:00:00Z".into();
    repo.create_task(&hr).await.unwrap();

    let mut blocked = task_row("t-bl", "p", OrchestratorTaskStatus::Blocked);
    blocked.state_version = 1;
    blocked.updated_at = "2026-07-15T02:00:00Z".into();
    repo.create_task(&blocked).await.unwrap();

    let mut done = task_row("t-done", "p", OrchestratorTaskStatus::Done);
    done.workflow_state = OrchestratorWorkflowState::Done;
    done.state_version = 5;
    done.updated_at = "2026-07-15T01:00:00Z".into();
    repo.create_task(&done).await.unwrap();

    let pending_item = repo
        .insert_remote_outbox_pending(
            "dev",
            "Dev",
            "/remote/path",
            None,
            r#"{"title":"SECRET_OUTBOX"}"#,
        )
        .await
        .unwrap();
    let outbox_id = pending_item.id.clone();
    repo.claim_remote_outbox_item_as_sending(&outbox_id)
        .await
        .unwrap();
    let failed = repo
        .mark_remote_outbox_failed(&outbox_id, "protocol")
        .await
        .unwrap();
    assert_eq!(failed.state_version, 1);

    let (items, truncated) = repo
        .list_operational_notification_items(1000)
        .await
        .unwrap();
    assert!(!truncated);
    let kinds: Vec<_> = items.iter().map(|i| i.kind).collect();
    assert!(kinds.contains(&OperationalNotificationKind::HumanReview));
    assert!(kinds.contains(&OperationalNotificationKind::Blocked));
    assert!(kinds.contains(&OperationalNotificationKind::TaskDone));
    assert!(kinds.contains(&OperationalNotificationKind::RemoteOutboxFailed));
    // HR 不得双计为 taskDone
    let task_done_ids: Vec<_> = items
        .iter()
        .filter(|i| i.kind == OperationalNotificationKind::TaskDone)
        .map(|i| i.opaque_source_id.as_str())
        .collect();
    assert_eq!(task_done_ids, vec!["t-done"]);
    let hr_item = items
        .iter()
        .find(|i| i.kind == OperationalNotificationKind::HumanReview)
        .unwrap();
    assert_eq!(hr_item.opaque_source_id, "t-hr");
    assert_eq!(hr_item.state_version, 3);
    let text = serde_json::to_string(&items).unwrap();
    assert!(!text.contains("SECRET_TITLE"));
    assert!(!text.contains("SECRET_OUTBOX"));
}

/// snapshot limit 截断：>1000 时 truncated=true 且条数=1000。
///
/// Business Logic（为什么需要这个测试）:
///     owner snapshot 合同硬上限 1000，防止 GUI baseline 无界膨胀。
///
/// Code Logic（这个测试做什么）:
///     插入 1001 条 Blocked，list(1000) 断言 len=1000 且 truncated。
#[tokio::test]
async fn operational_notification_snapshot_truncates_at_max_1000() {
    let (_pool, repo) = setup_repo().await;
    for i in 0..1001 {
        let mut row = task_row(&format!("t{i:04}"), "p", OrchestratorTaskStatus::Blocked);
        row.updated_at = format!("2026-07-15T00:00:{:02}.000Z", i % 60);
        row.state_version = 1;
        repo.create_task(&row).await.unwrap();
    }
    let (items, truncated) = repo
        .list_operational_notification_items(1000)
        .await
        .unwrap();
    assert!(truncated);
    assert_eq!(items.len(), 1000);
}

/// Business Logic（为什么需要这个测试）:
///     attempt 创建时写入的 provider policy 不得因后续 WORKFLOW/配置变化而漂移。
///
/// Code Logic（这个测试做什么）:
///     用 CodexVisible policy 写 attempt，再读回断言 runner_provider 仍为 codexVisible。
#[tokio::test]
async fn attempt_policy_does_not_change_when_workflow_changes() {
    use crate::orchestrator::agent_adapter::types::{AgentProviderId, RunnerAttemptPolicy};

    let (_pool, repo) = setup_repo().await;
    let task = task_row("task-policy", "proj-1", OrchestratorTaskStatus::Running);
    repo.create_task(&task).await.unwrap();

    let policy = RunnerAttemptPolicy::new(
        AgentProviderId::CodexVisible,
        4,
        300_000,
        AgentProviderId::CodexVisible.default_completion_contract(),
    )
    .expect("policy");
    repo.add_attempt(
        "task-policy",
        1,
        "wt-1",
        "sess-1",
        "prompt",
        &policy,
        None,
        OrchestratorAttemptStatus::Running,
    )
    .await
    .unwrap();

    let attempt = repo.get_attempt("task-policy", 1).await.unwrap();
    assert_eq!(attempt.runner_provider, "codexVisible");
    assert_eq!(attempt.max_turns, 4);
    assert_eq!(attempt.stall_timeout_ms, 300_000);
    assert_eq!(attempt.completion_contract, "sentinelLine");
}

/// Business Logic（为什么需要这个测试）:
///     旧 attempt 行 NULL policy 列必须映射 Claude/1/300000/sentinelLine。
///
/// Code Logic（这个测试做什么）:
///     直接 INSERT 无 policy 列值的 attempt，get_attempt 断言默认映射。
#[tokio::test]
async fn legacy_null_attempt_policy_maps_to_claude_defaults() {
    let (pool, repo) = setup_repo().await;
    let task = task_row("task-legacy", "proj-1", OrchestratorTaskStatus::Running);
    repo.create_task(&task).await.unwrap();
    sqlx::query(
        "INSERT INTO orchestrator_task_attempts \
         (id, task_id, attempt, worktree_id, session_id, prompt, status, created_at, completed_at) \
         VALUES ('a1', 'task-legacy', 1, 'wt', 'sess', 'p', 'running', 't', NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let attempt = repo.get_attempt("task-legacy", 1).await.unwrap();
    assert_eq!(attempt.runner_provider, "claudeCodeVisible");
    assert_eq!(attempt.max_turns, 1);
    assert_eq!(attempt.stall_timeout_ms, 300_000);
    assert_eq!(attempt.completion_contract, "sentinelLine");
}

/// Business Logic（为什么需要这个测试）:
///     stall CAS 必须先 task Running→Blocked（含 attempt/session），再标 attempt stalled；
///     已 Verifying 的任务不得被写 attempt=stalled。
///
/// Code Logic（这个测试做什么）:
///     1) Running task+running attempt → CAS 成功 → task Blocked + attempt stalled；
///     2) 再对已 Blocked 二次 CAS 失败；
///     3) 模拟 Verifying task 上 running attempt → CAS miss 且 attempt 仍 running。
#[tokio::test]
async fn stall_cas_is_atomic_and_does_not_pollute_verifying_attempt() {
    use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;
    use crate::orchestrator::runner_watchdog::EVIDENCE_CODE_RUNNER_STALLED;

    let (_pool, repo) = setup_repo().await;
    let task = task_row(
        "task-stall-cas",
        "proj-1",
        OrchestratorTaskStatus::Preparing,
    );
    repo.create_task(&task).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind("task-stall-cas")
        .execute(&_pool)
        .await
        .unwrap();
    let policy = RunnerAttemptPolicy::claude_default();
    let running = repo
        .mark_task_running_attempt(
            "task-stall-cas",
            "agent/stall",
            "wt-1",
            "sess-stall",
            1,
            "tok",
            None,
            &policy,
        )
        .await
        .unwrap();
    assert_eq!(running.status, OrchestratorTaskStatus::Running);
    repo.add_attempt(
        "task-stall-cas",
        1,
        "wt-1",
        "sess-stall",
        "prompt",
        &policy,
        None,
        OrchestratorAttemptStatus::Running,
    )
    .await
    .unwrap();

    let blocked = repo
        .try_cas_running_attempt_to_stalled_blocked(
            "task-stall-cas",
            1,
            "sess-stall",
            EVIDENCE_CODE_RUNNER_STALLED,
        )
        .await
        .unwrap();
    assert!(blocked.is_some());
    assert_eq!(blocked.unwrap().status, OrchestratorTaskStatus::Blocked);
    let attempt = repo.get_attempt("task-stall-cas", 1).await.unwrap();
    assert_eq!(attempt.status, "stalled");

    // 二次 CAS 必须失败
    let again = repo
        .try_cas_running_attempt_to_stalled_blocked(
            "task-stall-cas",
            1,
            "sess-stall",
            EVIDENCE_CODE_RUNNER_STALLED,
        )
        .await
        .unwrap();
    assert!(again.is_none());

    // Verifying + still-running attempt 不得被 stall 污染
    let mut task2 = task_row(
        "task-verifying",
        "proj-1",
        OrchestratorTaskStatus::Verifying,
    );
    task2.attempt = 1;
    task2.session_id = Some("sess-v".into());
    repo.create_task(&task2).await.unwrap();
    repo.add_attempt(
        "task-verifying",
        1,
        "wt-v",
        "sess-v",
        "prompt",
        &policy,
        None,
        OrchestratorAttemptStatus::Running,
    )
    .await
    .unwrap();
    let miss = repo
        .try_cas_running_attempt_to_stalled_blocked(
            "task-verifying",
            1,
            "sess-v",
            EVIDENCE_CODE_RUNNER_STALLED,
        )
        .await
        .unwrap();
    assert!(miss.is_none());
    let attempt_v = repo.get_attempt("task-verifying", 1).await.unwrap();
    assert_eq!(attempt_v.status, "running");
}

/// Business Logic（为什么需要这个测试）:
///     OSC/agent 活动必须刷新 task.last_activity_at，stall 才不会误杀活跃 runner。
///
/// Code Logic（这个测试做什么）:
///     mark Running 后 touch_task_last_activity，读回 last_activity_at。
#[tokio::test]
async fn touch_task_last_activity_updates_running_task_anchor() {
    use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;

    let (_pool, repo) = setup_repo().await;
    let task = task_row("task-act", "proj-1", OrchestratorTaskStatus::Preparing);
    repo.create_task(&task).await.unwrap();
    sqlx::query("UPDATE orchestrator_tasks SET prepare_claim_token = 'tok' WHERE id = ?")
        .bind("task-act")
        .execute(&_pool)
        .await
        .unwrap();
    repo.mark_task_running_attempt(
        "task-act",
        "agent/a",
        "wt",
        "sess-a",
        1,
        "tok",
        None,
        &RunnerAttemptPolicy::claude_default(),
    )
    .await
    .unwrap();

    let ok = repo
        .touch_task_last_activity("task-act", 1, "sess-a", "2026-07-15T12:00:00Z")
        .await
        .unwrap();
    assert!(ok);
    let row = repo.get_task("task-act").await.unwrap();
    assert_eq!(
        row.last_activity_at.as_deref(),
        Some("2026-07-15T12:00:00Z")
    );

    // session 不匹配不得更新
    let miss = repo
        .touch_task_last_activity("task-act", 1, "other-sess", "2026-07-15T13:00:00Z")
        .await
        .unwrap();
    assert!(!miss);
    let row = repo.get_task("task-act").await.unwrap();
    assert_eq!(
        row.last_activity_at.as_deref(),
        Some("2026-07-15T12:00:00Z")
    );
}
