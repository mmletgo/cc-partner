//! Orchestrator SQLite repository.

#![allow(dead_code)]

use crate::error::AppError;
use crate::orchestrator::claude_runtime::ClaudeRuntimeSummary;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorEvidenceDto,
    OrchestratorProjectConfigDto, OrchestratorRunState, OrchestratorTaskAttemptRow,
    OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState, SplitTaskState,
    EVIDENCE_KIND_REPAIR_PROMPT,
};
use crate::orchestrator::outbox::{
    OrchestratorRemoteOutboxRow, RemoteMirrorTask, RemoteOutboxStatus,
};
use crate::orchestrator::workflow::resolve_project_workflow;
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

const TASK_COLUMNS: &str = "id, project_id, title, goal, acceptance_criteria, status, priority, \
    workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
    external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
    transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
    last_runtime_message, branch_name, worktree_id, session_id, blocked_reason, attempt, \
    created_at, updated_at, started_at, finished_at";
const WORKFLOW_LANE_ORDER: [OrchestratorWorkflowState; 8] = [
    OrchestratorWorkflowState::Backlog,
    OrchestratorWorkflowState::Todo,
    OrchestratorWorkflowState::InProgress,
    OrchestratorWorkflowState::HumanReview,
    OrchestratorWorkflowState::Rework,
    OrchestratorWorkflowState::Merging,
    OrchestratorWorkflowState::Done,
    OrchestratorWorkflowState::Canceled,
];
const PROJECT_CONFIG_COLUMNS: &str = "project_id, enabled, max_concurrent_tasks, branch_prefix, \
    verification_commands_json, auto_commit, auto_push_task_branch, auto_merge_to_main, \
    auto_push_main, retry_limit, retain_worktree_on_done, retain_worktree_on_blocked, \
    created_at, updated_at";

/// Business Logic（为什么需要这个函数）:
///     看板拖拽必须使用固定泳道顺序判断相邻关系，避免前端或调用方传入任意状态导致跨阶段跳转。
///
/// Code Logic（这个函数做什么）:
///     在 WORKFLOW_LANE_ORDER 中查找 workflow_state 的索引；枚举完整覆盖，缺失时返回业务错误暴露代码缺陷。
fn workflow_lane_index(state: OrchestratorWorkflowState) -> Result<usize, AppError> {
    WORKFLOW_LANE_ORDER
        .iter()
        .position(|item| *item == state)
        .ok_or_else(|| AppError::generic("未知 Orchestrator 工作流泳道"))
}

/// Business Logic（为什么需要这个函数）:
///     拖拽移动不能作用于已经由 Runner 或交付流程接管的任务，否则看板阶段会与真实运行现场冲突。
///
/// Code Logic（这个函数做什么）:
///     判断 run_state 是否属于 Preparing/Running/Verifying/Delivering 四个执行中状态。
fn is_active_run_state(state: OrchestratorRunState) -> bool {
    matches!(
        state,
        OrchestratorRunState::Preparing
            | OrchestratorRunState::Running
            | OrchestratorRunState::Verifying
            | OrchestratorRunState::Delivering
    )
}
const EVIDENCE_COLUMNS: &str = "id, task_id, kind, title, summary, content, created_at";
const ATTEMPT_COLUMNS: &str = "id, task_id, attempt, worktree_id, session_id, prompt, status, \
    created_at, completed_at";
const REMOTE_OUTBOX_COLUMNS: &str = "id, device_id, device_name, remote_project_path, \
    remote_project_id, request_json, status, remote_task_id, last_error, created_at, updated_at, \
    sent_at";
const REMOTE_MIRROR_COLUMNS: &str = "id, device_id, device_name, remote_project_id, \
    remote_project_path, remote_task_id, payload_json, last_synced_at";

/// 幂等 create 结果：任务行 + 是否首次插入。
///
/// Business Logic（为什么需要这个结构体）:
///     客户端用同一 clientRequestId 重试 create 时，仓储必须返回既有任务，但调用方还需要知道
///     本次是否真的新建了任务。Start 动作只能在首次插入后触发全局 dispatch，否则会在 A 完成后
///     误启动队列中的任务 B。
///
/// Code Logic（这个结构体做什么）:
///     `task` 是权威任务行；`newly_created=true` 表示本请求完成了 insert，`false` 表示幂等命中。
#[derive(Debug, Clone)]
pub struct IdempotentCreateTaskOutcome {
    pub task: OrchestratorTaskRow,
    pub newly_created: bool,
}

pub const ORCHESTRATOR_TASK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_tasks (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  goal TEXT NOT NULL,
  acceptance_criteria TEXT NOT NULL,
  status TEXT NOT NULL,
  workflow_state TEXT NOT NULL DEFAULT 'backlog',
  run_state TEXT NOT NULL DEFAULT 'idle',
  attempt_phase TEXT,
  source TEXT NOT NULL DEFAULT 'internal',
  external_id TEXT,
  external_identifier TEXT,
  external_url TEXT,
  external_state TEXT,
  external_labels_json TEXT,
  runner_provider TEXT,
  claude_session_id TEXT,
  transcript_path TEXT,
  runtime_started_at TEXT,
  last_activity_at TEXT,
  last_runtime_event TEXT,
  last_runtime_message TEXT,
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
)";

const ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_tasks_project_status \
     ON orchestrator_tasks(project_id, status, priority, created_at)";

const ORCHESTRATOR_TASKS_STATUS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_tasks_status \
     ON orchestrator_tasks(status, priority, created_at)";

pub const ORCHESTRATOR_PROJECT_CONFIG_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_project_config (
  project_id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  max_concurrent_tasks INTEGER NOT NULL DEFAULT 1,
  branch_prefix TEXT NOT NULL DEFAULT 'agent',
  verification_commands_json TEXT NOT NULL DEFAULT '[]',
  auto_commit INTEGER NOT NULL DEFAULT 1,
  auto_push_task_branch INTEGER NOT NULL DEFAULT 1,
  auto_merge_to_main INTEGER NOT NULL DEFAULT 1,
  auto_push_main INTEGER NOT NULL DEFAULT 1,
  retry_limit INTEGER NOT NULL DEFAULT 0,
  retain_worktree_on_done INTEGER NOT NULL DEFAULT 0,
  retain_worktree_on_blocked INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)";

pub const ORCHESTRATOR_EVENT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_task_events (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  message TEXT NOT NULL,
  payload_json TEXT,
  created_at TEXT NOT NULL
)";

const ORCHESTRATOR_TASK_EVENTS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_task_events_task \
     ON orchestrator_task_events(task_id, created_at)";

/// Orchestrator 最近事件查询行。
///
/// Business Logic（为什么需要这个结构体）:
///     runtime snapshot 状态条需要展示当前项目最近 scheduler/runner 事件，帮助用户理解任务推进过程。
///
/// Code Logic（这个结构体做什么）:
///     保存 event 表字段和 join 得到的任务标题，供命令层投影为前端 DTO。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorRecentEventRow {
    pub id: String,
    pub task_id: String,
    pub task_title: String,
    pub kind: String,
    pub message: String,
    pub created_at: String,
}

pub const ORCHESTRATOR_EVIDENCE_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_task_evidence (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  summary TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL
)";

const ORCHESTRATOR_TASK_EVIDENCE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_task_evidence_task \
     ON orchestrator_task_evidence(task_id, created_at)";

pub const ORCHESTRATOR_TASK_ATTEMPT_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_task_attempts (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  worktree_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  prompt TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  completed_at TEXT,
  UNIQUE(task_id, attempt)
)";

const ORCHESTRATOR_TASK_ATTEMPTS_SESSION_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_task_attempts_session \
     ON orchestrator_task_attempts(session_id, status)";

pub const ORCHESTRATOR_REMOTE_OUTBOX_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_remote_outbox (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_project_id TEXT,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL,
  remote_task_id TEXT,
  last_error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sent_at TEXT
)";

const ORCHESTRATOR_REMOTE_OUTBOX_STATUS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_outbox_status \
     ON orchestrator_remote_outbox(status, updated_at, device_id)";

const ORCHESTRATOR_REMOTE_OUTBOX_PROJECT_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_outbox_project \
     ON orchestrator_remote_outbox(device_id, remote_project_path, status)";

pub const ORCHESTRATOR_REMOTE_TASK_MIRROR_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_remote_task_mirrors (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_id TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_task_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  last_synced_at TEXT NOT NULL,
  UNIQUE(device_id, remote_task_id)
)";

const ORCHESTRATOR_REMOTE_TASK_MIRROR_PROJECT_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_task_mirrors_project \
     ON orchestrator_remote_task_mirrors(device_id, remote_project_id, last_synced_at)";

pub const ORCHESTRATOR_REMOTE_TASK_CREATE_REQUEST_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_remote_task_create_requests (
  request_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL DEFAULT '',
  task_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)";

/// Orchestrator 仓储，封装任务、事件和证据表访问。
///
/// Business Logic（为什么需要这个结构体）:
///     自动编排器需要把任务队列、状态变化事件和验证证据持久化，供后续调度器与页面共享。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool，并提供任务 CRUD、legacy 项目配置读取、状态更新、事件和证据追加方法。
#[derive(Clone)]
pub struct OrchestratorRepo {
    pool: SqlitePool,
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造 Orchestrator 仓储。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SqlitePool clone；pool 内部是 Arc，clone 廉价。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用启动时必须确保 Orchestrator 所需表和索引存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐条执行 CREATE TABLE/CREATE INDEX IF NOT EXISTS，兼容旧库且不使用 sqlx 迁移宏。
    pub async fn init_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(ORCHESTRATOR_TASK_SCHEMA).execute(pool).await?;
        let added_workflow_state = ensure_column(
            pool,
            "orchestrator_tasks",
            "workflow_state",
            "TEXT NOT NULL DEFAULT 'backlog'",
        )
        .await?;
        let added_run_state = ensure_column(
            pool,
            "orchestrator_tasks",
            "run_state",
            "TEXT NOT NULL DEFAULT 'idle'",
        )
        .await?;
        ensure_column(pool, "orchestrator_tasks", "attempt_phase", "TEXT").await?;
        ensure_column(
            pool,
            "orchestrator_tasks",
            "source",
            "TEXT NOT NULL DEFAULT 'internal'",
        )
        .await?;
        ensure_column(pool, "orchestrator_tasks", "external_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_identifier", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_url", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_state", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_labels_json", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "runner_provider", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "claude_session_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "transcript_path", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "runtime_started_at", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_activity_at", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_runtime_event", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_runtime_message", "TEXT").await?;
        if added_workflow_state || added_run_state {
            backfill_split_state_from_legacy_status(pool).await?;
        }
        normalize_queued_split_state(pool).await?;

        for statement in [
            ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX,
            ORCHESTRATOR_TASKS_STATUS_INDEX,
            ORCHESTRATOR_PROJECT_CONFIG_SCHEMA,
            ORCHESTRATOR_EVENT_SCHEMA,
            ORCHESTRATOR_TASK_EVENTS_INDEX,
            ORCHESTRATOR_EVIDENCE_SCHEMA,
            ORCHESTRATOR_TASK_EVIDENCE_INDEX,
            ORCHESTRATOR_TASK_ATTEMPT_SCHEMA,
            ORCHESTRATOR_TASK_ATTEMPTS_SESSION_INDEX,
            ORCHESTRATOR_REMOTE_OUTBOX_SCHEMA,
            ORCHESTRATOR_REMOTE_OUTBOX_STATUS_INDEX,
            ORCHESTRATOR_REMOTE_OUTBOX_PROJECT_INDEX,
            ORCHESTRATOR_REMOTE_TASK_MIRROR_SCHEMA,
            ORCHESTRATOR_REMOTE_TASK_MIRROR_PROJECT_INDEX,
            ORCHESTRATOR_REMOTE_TASK_CREATE_REQUEST_SCHEMA,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        // 旧库仅有 request_id 主键时补齐 project_id / request_fingerprint，并把 project_id 回填为任务归属。
        migrate_remote_task_create_request_scope(pool).await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层会先完成校验、生成 id/时间戳并构造 Row，仓储只负责按 Row 持久化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将调用方传入的 OrchestratorTaskRow 全字段插入 orchestrator_tasks，不改写业务字段。
    pub async fn create_task(&self, row: &OrchestratorTaskRow) -> Result<(), AppError> {
        let external_labels_json = serialize_external_labels(&row.external_labels)?;
        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
              workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
              external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
              transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
              last_runtime_message, worktree_id, session_id, blocked_reason, attempt, created_at, \
              updated_at, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.project_id)
        .bind(&row.title)
        .bind(&row.goal)
        .bind(&row.acceptance_criteria)
        .bind(row.status.as_str())
        .bind(row.priority)
        .bind(&row.branch_name)
        .bind(row.workflow_state.as_str())
        .bind(row.run_state.as_str())
        .bind(row.attempt_phase.map(OrchestratorAttemptPhase::as_str))
        .bind(&row.source)
        .bind(&row.external_id)
        .bind(&row.external_identifier)
        .bind(&row.external_url)
        .bind(&row.external_state)
        .bind(&external_labels_json)
        .bind(&row.runner_provider)
        .bind(&row.claude_session_id)
        .bind(&row.transcript_path)
        .bind(&row.runtime_started_at)
        .bind(&row.last_activity_at)
        .bind(&row.last_runtime_event)
        .bind(&row.last_runtime_message)
        .bind(&row.worktree_id)
        .bind(&row.session_id)
        .bind(&row.blocked_reason)
        .bind(row.attempt)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.started_at)
        .bind(&row.finished_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 P2P create 请求可能在 owning device 已创建任务后响应超时，客户端会用同一 clientRequestId 重试。
    ///     仓储必须把 requestId->taskId 与任务创建放在同一事务中，避免重复任务；
    ///     同时必须按 project 隔离幂等键，防止跨项目返回另一项目的任务内容。
    ///     调用方还需要区分“首次插入”与“幂等命中”，避免 Start 重放再次触发全局调度副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在事务内对非空 client_request_id：
    ///     1) 按 request_id 查找既有映射；
    ///     2) 映射的 project_id 与本次请求不一致 → conflict（跨项目不得复用）；
    ///     3) 同项目且 fingerprint 匹配 → 返回既有 task，`newly_created=false`；
    ///     4) 同项目但 fingerprint 为空或不同 → conflict（旧行空指纹 fail-closed）；
    ///     5) 首次登记写入 (request_id, project_id, fingerprint, task_id) 后插入任务，
    ///        返回 `newly_created=true`。
    pub async fn create_remote_task_idempotent(
        &self,
        client_request_id: Option<&str>,
        row: &OrchestratorTaskRow,
        create_action: OrchestratorCreateAction,
    ) -> Result<IdempotentCreateTaskOutcome, AppError> {
        let client_request_id = client_request_id.and_then(non_empty_trimmed);
        let row_to_insert = task_row_for_create_action(row, create_action);
        let external_labels_json = serialize_external_labels(&row_to_insert.external_labels)?;
        let request_fingerprint =
            create_request_fingerprint(&row_to_insert, create_action, &external_labels_json)?;
        let mut tx = self.pool.begin().await?;

        if let Some(request_id) = client_request_id {
            if let Some(existing) = sqlx::query(
                "SELECT project_id, task_id, request_fingerprint \
                 FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
            )
            .bind(request_id)
            .fetch_optional(&mut *tx)
            .await?
            {
                let task = resolve_existing_create_request(
                    &mut tx,
                    request_id,
                    &row_to_insert.project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok(IdempotentCreateTaskOutcome {
                    task,
                    newly_created: false,
                });
            }

            let now = Utc::now().to_rfc3339();
            let inserted = sqlx::query(
                "INSERT OR IGNORE INTO orchestrator_remote_task_create_requests \
                 (request_id, project_id, task_id, request_fingerprint, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(request_id)
            .bind(&row_to_insert.project_id)
            .bind(&row_to_insert.id)
            .bind(&request_fingerprint)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() != 1 {
                let existing = sqlx::query(
                    "SELECT project_id, task_id, request_fingerprint \
                     FROM orchestrator_remote_task_create_requests WHERE request_id = ?",
                )
                .bind(request_id)
                .fetch_one(&mut *tx)
                .await?;
                let task = resolve_existing_create_request(
                    &mut tx,
                    request_id,
                    &row_to_insert.project_id,
                    &request_fingerprint,
                    existing,
                )
                .await?;
                tx.commit().await?;
                return Ok(IdempotentCreateTaskOutcome {
                    task,
                    newly_created: false,
                });
            }
        }

        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
              workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
              external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
              transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
              last_runtime_message, worktree_id, session_id, blocked_reason, attempt, created_at, \
              updated_at, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row_to_insert.id)
        .bind(&row_to_insert.project_id)
        .bind(&row_to_insert.title)
        .bind(&row_to_insert.goal)
        .bind(&row_to_insert.acceptance_criteria)
        .bind(row_to_insert.status.as_str())
        .bind(row_to_insert.priority)
        .bind(&row_to_insert.branch_name)
        .bind(row_to_insert.workflow_state.as_str())
        .bind(row_to_insert.run_state.as_str())
        .bind(row_to_insert.attempt_phase.map(OrchestratorAttemptPhase::as_str))
        .bind(&row_to_insert.source)
        .bind(&row_to_insert.external_id)
        .bind(&row_to_insert.external_identifier)
        .bind(&row_to_insert.external_url)
        .bind(&row_to_insert.external_state)
        .bind(&external_labels_json)
        .bind(&row_to_insert.runner_provider)
        .bind(&row_to_insert.claude_session_id)
        .bind(&row_to_insert.transcript_path)
        .bind(&row_to_insert.runtime_started_at)
        .bind(&row_to_insert.last_activity_at)
        .bind(&row_to_insert.last_runtime_event)
        .bind(&row_to_insert.last_runtime_message)
        .bind(&row_to_insert.worktree_id)
        .bind(&row_to_insert.session_id)
        .bind(&row_to_insert.blocked_reason)
        .bind(row_to_insert.attempt)
        .bind(&row_to_insert.created_at)
        .bind(&row_to_insert.updated_at)
        .bind(&row_to_insert.started_at)
        .bind(&row_to_insert.finished_at)
        .execute(&mut *tx)
        .await?;

        let task_row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(&row_to_insert.id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(IdempotentCreateTaskOutcome {
            task: row_to_task(&task_row)?,
            newly_created: true,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 route 使用非空 clientRequestId 时，需要一个语义明确的仓储入口表达“按客户端请求幂等创建”。
    ///
    /// Code Logic（这个函数做什么）:
    ///     作为 create_remote_task_idempotent 的非空 request id 包装，复用同一个事务实现。
    pub async fn create_remote_task_for_client_request(
        &self,
        client_request_id: &str,
        row: &OrchestratorTaskRow,
        create_action: OrchestratorCreateAction,
    ) -> Result<IdempotentCreateTaskOutcome, AppError> {
        self.create_remote_task_idempotent(Some(client_request_id), row, create_action)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     任务列表需要支持按项目筛选或返回全局任务，并按调度优先级排序。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据可选 project_id 选择 SQL，结果统一按 priority DESC, created_at ASC 排序。
    pub async fn list_tasks(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let rows = match project_id {
            Some(project_id) => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     WHERE project_id = ? ORDER BY priority DESC, created_at ASC"
                ))
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     ORDER BY priority DESC, created_at ASC"
                ))
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度器和详情页需要按任务 id 读取完整任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询任务表，缺失时转换为 AppError::not_found。
    pub async fn get_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row_to_task(&row),
            None => Err(AppError::not_found(format!(
                "Orchestrator 任务不存在: {task_id}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     legacy 项目配置表仍需支持旧数据兼容和调试读取；新项目缺失记录时保持可创建默认行。
    ///     Settings 自动化 tab 已成为唯一用户配置入口，scheduler、验证和 delivery 运行时不再读取该表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对缺失 project_id 执行 INSERT OR IGNORE 写入 full-auto-but-disabled 默认值，再读取并解析 DTO。
    pub async fn get_or_create_project_config(
        &self,
        project_id: &str,
    ) -> Result<OrchestratorProjectConfigDto, AppError> {
        let project_id = project_id.trim();
        if project_id.is_empty() {
            return Err(AppError::generic("项目不能为空"));
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT OR IGNORE INTO orchestrator_project_config \
             (project_id, enabled, max_concurrent_tasks, branch_prefix, verification_commands_json, \
              auto_commit, auto_push_task_branch, auto_merge_to_main, auto_push_main, retry_limit, \
              retain_worktree_on_done, retain_worktree_on_blocked, created_at, updated_at) \
             VALUES (?, 0, 1, 'agent', '[]', 1, 1, 1, 1, 0, 0, 1, ?, ?)",
        )
        .bind(project_id)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;

        let row = sqlx::query(&format!(
            "SELECT {PROJECT_CONFIG_COLUMNS} FROM orchestrator_project_config WHERE project_id = ?"
        ))
        .bind(project_id)
        .fetch_one(&self.pool)
        .await?;

        row_to_project_config(&row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     legacy 项目配置列表仅用于兼容/调试旧数据；运行时调度已改读 AppConfig.orchestrator。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 enabled=1 的 legacy 配置，按 project_id 稳定排序后转换为 OrchestratorProjectConfigDto。
    pub async fn list_enabled_project_configs(
        &self,
    ) -> Result<Vec<OrchestratorProjectConfigDto>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {PROJECT_CONFIG_COLUMNS} FROM orchestrator_project_config \
             WHERE enabled = 1 ORDER BY project_id ASC"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_project_config).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目级并发上限只应计算已经被调度器接管或正在后续阶段处理的任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     统计 Preparing/Running/Verifying/Delivering 四个 active run_state 数量，返回 SQLite COUNT 结果。
    pub async fn count_active_tasks(&self, project_id: &str) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state IN (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要展示当前项目已占用的本机自动化槽位，避免 UI 重新拼 SQL 或误用 legacy status。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 统计 Preparing/Running/Verifying/Delivering 四个 active run_state 数量。
    pub async fn count_active_run_states_for_project(
        &self,
        project_id: &str,
    ) -> Result<i64, AppError> {
        self.count_active_tasks(project_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 应尽量暴露最近阻塞原因，帮助用户理解自动化为什么没有继续推进。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询同项目最近一个 run_state=blocked 且 blocked_reason 非空的任务，按 updated_at/id 倒序取一条。
    pub async fn latest_blocked_reason_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<String>, AppError> {
        let row = sqlx::query(
            "SELECT blocked_reason FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state = ? \
               AND blocked_reason IS NOT NULL AND TRIM(blocked_reason) <> '' \
             ORDER BY updated_at DESC, id DESC LIMIT 1",
        )
        .bind(project_id)
        .bind(OrchestratorRunState::Blocked.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|item| item.try_get("blocked_reason")).transpose()?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要展示当前仍占用执行槽位的任务摘要，避免用户只能看到槽位数字。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 查询 Preparing/Running/Verifying/Delivering run_state 的任务，按更新时间倒序限制数量。
    pub async fn list_active_runtime_tasks_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? AND run_state IN (?, ?, ?, ?) \
             ORDER BY updated_at DESC, priority DESC, created_at ASC LIMIT ?"
        ))
        .bind(project_id)
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要突出需要重试或返工的任务，让用户能从状态条直接看到阻塞队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 workflow_state=Rework 或 run_state=Retrying/Blocked 的任务，按更新时间倒序限制数量。
    pub async fn list_retrying_runtime_tasks_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? \
               AND (workflow_state = ? OR run_state IN (?, ?)) \
             ORDER BY updated_at DESC, priority DESC, created_at ASC LIMIT ?"
        ))
        .bind(project_id)
        .bind(OrchestratorWorkflowState::Rework.as_str())
        .bind(OrchestratorRunState::Retrying.as_str())
        .bind(OrchestratorRunState::Blocked.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_task).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 状态条需要展示当前项目最近的 scheduler/runner 事件，而不是让前端遍历全量任务事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     join task_events 与 tasks 过滤 project_id，按事件 created_at/id 倒序取最近 limit 条。
    pub async fn list_recent_events_for_project(
        &self,
        project_id: &str,
        limit: i64,
    ) -> Result<Vec<OrchestratorRecentEventRow>, AppError> {
        let limit = limit.clamp(0, 50);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT event.id, event.task_id, task.title AS task_title, \
                    event.kind, event.message, event.created_at \
             FROM orchestrator_task_events event \
             INNER JOIN orchestrator_tasks task ON task.id = event.task_id \
             WHERE task.project_id = ? \
             ORDER BY event.created_at DESC, event.id DESC LIMIT ?",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(OrchestratorRecentEventRow {
                    id: row.try_get("id")?,
                    task_id: row.try_get("task_id")?,
                    task_title: row.try_get("task_title")?,
                    kind: row.try_get("kind")?,
                    message: row.try_get("message")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 的调度容量属于本设备全局配置，必须统计所有本机 local Workbench 项目的执行中任务。
    ///     远端项目只是快捷方式，不能占用或触发本机 Runner 容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过 workbench_projects join 过滤 kind='local'，统计 Preparing/Running/Verifying/Delivering 四个 active run_state。
    pub async fn count_active_local_tasks(&self) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' AND task.run_state IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度器领取任务必须在同一个仓储原子边界内校验项目并发容量，避免后台 tick 与手动 dispatch 同时看到空闲槽位后各自领取任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个事务中处理 max<=0、active 状态计数、Queued 任务选择、带 status 条件的 Preparing 更新和更新后 Row 读取。
    ///     active 达到上限、无 Queued 任务或并发竞争导致 UPDATE 未命中时返回 None。
    pub async fn claim_next_queued_task_with_capacity(
        &self,
        project_id: &str,
        max_concurrent_tasks: i64,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let mut tx = self.pool.begin().await?;
        if max_concurrent_tasks <= 0 {
            tx.commit().await?;
            return Ok(None);
        }

        let active = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks \
             WHERE project_id = ? AND status IN (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .fetch_one(&mut *tx)
        .await?
        .try_get::<i64, _>("count")?;
        if active >= max_concurrent_tasks {
            tx.commit().await?;
            return Ok(None);
        }

        let selected = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE project_id = ? AND status = ? \
             ORDER BY priority DESC, created_at ASC LIMIT 1"
        ))
        .bind(project_id)
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = selected else {
            tx.commit().await?;
            return Ok(None);
        };
        let task_id: String = row.try_get("id")?;
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Preparing);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(&task_id)
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(None);
        }

        let row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(&task_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(row_to_task(&row)?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局 scheduler 一次 tick 需要按设备级剩余容量，在所有本机 local Workbench 项目中领取项目 workflow 允许的活跃泳道任务。
    ///     remote 项目必须跳过；项目 WORKFLOW.md 无效时不能把该项目任务提前 claim 到 Preparing。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个事务内完成 active local run_state 计数、剩余容量计算、idle/blocked 候选读取和 Preparing 条件更新。
    ///     每个候选按 Workbench 项目 path 动态解析 WORKFLOW.md，使用 ResolvedWorkflow.active_states 判断是否可领取。
    pub async fn claim_next_local_queued_tasks_with_global_capacity(
        &self,
        limit: i64,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let mut tx = self.pool.begin().await?;
        if limit <= 0 {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let active = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' AND task.run_state IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorRunState::Preparing.as_str())
        .bind(OrchestratorRunState::Running.as_str())
        .bind(OrchestratorRunState::Verifying.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .fetch_one(&mut *tx)
        .await?
        .try_get::<i64, _>("count")?;
        let remaining = limit - active;
        if remaining <= 0 {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let selected = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
             WHERE id IN (\
               SELECT task.id FROM orchestrator_tasks task \
               INNER JOIN workbench_projects project ON project.id = task.project_id \
               WHERE project.kind = 'local' \
                 AND task.run_state IN (?, ?) \
               ORDER BY task.priority DESC, task.created_at ASC\
             ) \
             ORDER BY priority DESC, created_at ASC"
        ))
        .bind(OrchestratorRunState::Idle.as_str())
        .bind(OrchestratorRunState::Blocked.as_str())
        .fetch_all(&mut *tx)
        .await?;

        let mut claimed = Vec::new();
        for row in selected {
            if claimed.len() >= remaining as usize {
                break;
            }
            let task_id: String = row.try_get("id")?;
            let project_id: String = row.try_get("project_id")?;
            let candidate = row_to_task(&row)?;
            let project_path =
                sqlx::query("SELECT path FROM workbench_projects WHERE id = ? AND kind = 'local'")
                    .bind(&project_id)
                    .fetch_one(&mut *tx)
                    .await?
                    .try_get::<String, _>("path")?;
            let workflow = match resolve_project_workflow(Path::new(&project_path)) {
                Ok(workflow) => workflow,
                Err(err) => {
                    tracing::warn!(
                        project_id = %project_id,
                        project_path = %project_path,
                        "跳过无效 WORKFLOW.md 项目的 Orchestrator dispatch: {err}"
                    );
                    continue;
                }
            };
            if !workflow.active_states.contains(&candidate.workflow_state) {
                continue;
            }
            let now = Utc::now().to_rfc3339();
            let result = sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? \
                   AND workflow_state = ? \
                   AND run_state IN (?, ?)",
            )
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(OrchestratorWorkflowState::InProgress.as_str())
            .bind(OrchestratorRunState::Preparing.as_str())
            .bind(OrchestratorAttemptPhase::PreparingWorkspace.as_str())
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(&task_id)
            .bind(candidate.workflow_state.as_str())
            .bind(OrchestratorRunState::Idle.as_str())
            .bind(OrchestratorRunState::Blocked.as_str())
            .execute(&mut *tx)
            .await?;

            if result.rows_affected() != 1 {
                continue;
            }

            let updated = sqlx::query(&format!(
                "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
            ))
            .bind(&task_id)
            .fetch_one(&mut *tx)
            .await?;
            claimed.push(row_to_task(&updated)?);
        }

        tx.commit().await?;
        Ok(claimed)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每一轮 Claude Code 开发尝试都需要持久化 prompt、worktree 和可见 terminal session，
    ///     后续 completion sentinel 与任务详情才能把输出映射回具体任务轮次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 attempt id 和 created_at，插入 orchestrator_task_attempts；task_id+attempt 唯一约束由 SQLite 保证。
    pub async fn add_attempt(
        &self,
        task_id: &str,
        attempt: i64,
        worktree_id: &str,
        session_id: &str,
        prompt: &str,
        status: &str,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        if task_id.trim().is_empty() {
            return Err(AppError::generic("任务不能为空"));
        }
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        if worktree_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 worktree"));
        }
        if session_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }
        if status.trim().is_empty() {
            return Err(AppError::generic("任务尝试状态不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO orchestrator_task_attempts \
             (id, task_id, attempt, worktree_id, session_id, prompt, status, created_at, completed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(task_id.trim())
        .bind(attempt)
        .bind(worktree_id.trim())
        .bind(session_id.trim())
        .bind(prompt)
        .bind(status.trim())
        .bind(&now)
        .bind(Option::<&str>::None)
        .execute(&self.pool)
        .await?;

        Ok(OrchestratorTaskAttemptRow {
            id,
            task_id: task_id.trim().to_string(),
            attempt,
            worktree_id: worktree_id.trim().to_string(),
            session_id: session_id.trim().to_string(),
            prompt: prompt.to_string(),
            status: status.trim().to_string(),
            created_at: now,
            completed_at: None,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     开发 terminal 输出 completion sentinel 后，系统需要标记对应尝试已完成，避免同一轮重复触发验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id+attempt 且当前 status=running 更新 status=completed 和 completed_at；找不到运行中记录时返回 not_found。
    pub async fn mark_attempt_completed(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
             WHERE task_id = ? AND attempt = ? AND status = ?",
        )
        .bind("completed")
        .bind(&now)
        .bind(task_id)
        .bind(attempt)
        .bind("running")
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 运行中任务尝试不存在: {task_id}#{attempt}"
            )));
        }

        let row = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM orchestrator_task_attempts \
             WHERE task_id = ? AND attempt = ?"
        ))
        .bind(task_id)
        .bind(attempt)
        .fetch_one(&self.pool)
        .await?;
        row_to_attempt(&row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     terminal completion hook 只能拿到 Workbench session_id，必须反查当前 running attempt 才能定位 task_id。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 session_id 和 status='running' 查询最新 attempt；缺失返回 None，存在时转换为 OrchestratorTaskAttemptRow。
    pub async fn get_running_attempt_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<OrchestratorTaskAttemptRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM orchestrator_task_attempts \
             WHERE session_id = ? AND status = ? \
             ORDER BY attempt DESC, created_at DESC LIMIT 1"
        ))
        .bind(session_id.trim())
        .bind("running")
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_attempt(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 创建出某一轮尝试的 worktree 和 terminal 后，需要把 active session、attempt 与可见运行器类型写回任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当任务仍为 Preparing 时写入 branch/worktree/session/attempt/runner_provider，把状态切到 Running；
    ///     同时清空上一轮 Claude runtime 字段并设置 runtime_started_at；未命中时返回当前任务。
    pub async fn mark_task_running_attempt(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Running);
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, runner_provider = ?, branch_name = ?, worktree_id = ?, session_id = ?, attempt = ?, \
                 claude_session_id = ?, transcript_path = ?, runtime_started_at = ?, last_activity_at = ?, last_runtime_event = ?, last_runtime_message = ?, \
                 blocked_reason = ?, started_at = COALESCE(started_at, ?), updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind("claudeCodeVisible")
        .bind(branch_name)
        .bind(worktree_id)
        .bind(session_id)
        .bind(attempt)
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(now.clone())
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 准备阶段需要持续反馈 PreparingWorkspace/BuildingPrompt/Streaming 等细分进度，方便用户判断当前卡点。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在任务仍处于 Preparing 时更新 attempt_phase 和 updated_at；Running 后必须改用 active runner guard helper。
    pub async fn update_task_attempt_phase(
        &self,
        task_id: &str,
        phase: OrchestratorAttemptPhase,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET attempt_phase = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(phase.as_str())
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 挂账 Running 后的阶段更新可能与用户 Abort 或新 attempt 并发，旧 runner 迟到更新不能覆盖当前任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 task_id、status=Running、attempt 和 session_id 全部匹配时写入 attempt_phase；未命中返回当前任务。
    pub async fn update_active_runner_attempt_phase(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        phase: OrchestratorAttemptPhase,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET attempt_phase = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
        )
        .bind(phase.as_str())
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(attempt)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude Code visible runtime 关联是可选增强；旧 runner 迟到关联不能覆盖新 attempt 的 session/transcript。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 task_id、status=Running、attempt 和 session_id 全部匹配时写入 ClaudeRuntimeSummary；
    ///     guard 未命中时返回 None，命中时返回更新后的任务行。
    pub async fn update_active_runner_runtime_summary(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        summary: &ClaudeRuntimeSummary,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET claude_session_id = ?, transcript_path = ?, last_activity_at = ?, \
                 last_runtime_event = ?, last_runtime_message = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
        )
        .bind(summary.claude_session_id.as_deref())
        .bind(summary.transcript_path.as_deref())
        .bind(summary.last_activity_at.as_deref())
        .bind(summary.last_runtime_event.as_deref())
        .bind(summary.last_runtime_message.as_deref())
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(attempt)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 创建出任务 worktree 和 terminal 后，需要把可接管现场持久化到任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当任务仍为 Preparing 时写入 branch_name/worktree_id/session_id，把状态切到 Running，
    ///     清空 blocked_reason，并首次设置 started_at；未命中时返回当前任务且不覆盖 Abort/Block。
    pub async fn mark_task_running(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        self.mark_task_running_attempt(task_id, branch_name, worktree_id, session_id, 1)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     队列动作和状态机推进后需要只更新任务状态和阻塞原因，保持任务身份字段不变。
    ///
    /// Code Logic（这个函数做什么）:
    ///     只更新 status、blocked_reason 和 updated_at，再读取完整任务返回。
    pub async fn set_task_status(
        &self,
        task_id: &str,
        status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(status);
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(blocked_reason)
        .bind(now)
        .bind(task_id)
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付完整成功后，任务需要进入 Done；但用户可能在交付期间终止任务，Done 写入不能覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Delivering 时将 status 置为 done、清空 blocked_reason 并写 finished_at；
    ///     条件未命中时读取并返回当前任务，不改写终止或其它状态。
    pub async fn finish_task_done(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Done);
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ?, finished_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Done.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(&now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动交付失败后任务应进入 Blocked；但用户终止状态优先，失败兜底不得覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Delivering 时写入 Blocked 和 blocked_reason；未命中时返回当前任务。
    pub async fn block_task_if_delivering(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Blocked.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(reason)
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证阶段失败应进入 Blocked；但用户可能在验证运行期间终止任务，失败处理不能覆盖 Aborted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当当前状态仍为 Verifying 时写入 Blocked 和 blocked_reason；未命中时返回当前任务。
    pub async fn block_task_if_verifying(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Blocked.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(reason)
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .execute(&self.pool)
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证成功后推进 Delivering 可能与用户 Abort 并发发生；调用方需要知道是否成功取得下一阶段执行权。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?` 条件更新；命中返回 Some(更新后 Row)，
    ///     未命中时确认任务仍存在并返回 None，不把当前状态当作业务错误。
    pub async fn try_transition_task_status(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(next_status);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(next_status.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(blocked_reason)
        .bind(now)
        .bind(task_id)
        .bind(expected_status.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier pass/fail 需要在保留 legacy status 原子守卫的同时写入新的 workflow/run/attempt split state。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?`，命中时按调用方传入的 split state 与 attempt phase 更新；
    ///     未命中时确认任务仍存在并返回 None，避免迟到 verifier 覆盖 Abort 或其它并发状态。
    #[allow(clippy::too_many_arguments)]
    pub async fn try_transition_task_split_state(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        workflow_state: OrchestratorWorkflowState,
        run_state: OrchestratorRunState,
        attempt_phase: Option<OrchestratorAttemptPhase>,
        blocked_reason: Option<&str>,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(next_status.as_str())
        .bind(workflow_state.as_str())
        .bind(run_state.as_str())
        .bind(attempt_phase.map(|phase| phase.as_str()))
        .bind(blocked_reason)
        .bind(now)
        .bind(task_id)
        .bind(expected_status.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     terminal sentinel 来自某个具体 session/attempt，旧 session 的迟到哨兵不能推进当前 active runner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原子校验任务仍为 Running 且 active attempt/session 匹配后切到 Verifying；未命中确认任务存在并返回 None。
    pub async fn try_transition_running_attempt_to_verifying(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        if session_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Verifying);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
        )
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(attempt)
        .bind(session_id.trim())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     完成验证、重试等用户动作可能与 scheduler 并发发生，状态推进必须在数据库内一次性校验当前状态，避免旧点击覆盖新状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 `UPDATE ... WHERE id=? AND status=?` 条件更新；命中则返回更新后 Row，未命中时读取当前任务并返回中文业务错误，缺失任务沿用 not_found。
    pub async fn transition_task_status(
        &self,
        task_id: &str,
        expected_status: OrchestratorTaskStatus,
        next_status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(next_status);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(next_status.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(blocked_reason)
        .bind(now)
        .bind(task_id)
        .bind(expected_status.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        let current = self.get_task(task_id).await?;
        Err(AppError::generic(format!(
            "任务状态已变化，无法从 {} 切换到 {}，当前状态为 {}",
            expected_status.as_str(),
            next_status.as_str(),
            current.status.as_str()
        )))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户手动入队只应作用于草稿任务，避免运行中、已完成、阻塞或已终止任务被回退到队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用带 status=draft 条件的原子 UPDATE 切换到 queued；未更新时读取当前任务并返回中文业务错误。
    pub async fn queue_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Queued);
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Draft.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        let current = self.get_task(task_id).await?;
        Err(AppError::generic(format!(
            "只有草稿任务可以入队，当前状态为 {}",
            current.status.as_str()
        )))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 startTask 动作只负责把 Backlog/Draft 或 Todo/Idle 任务放入 scheduler 可领取路径，
    ///     后续是否立即启动 Runner 交给调度器按全局开关和容量 best-effort 决定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先读取当前任务并校验允许的 split state；通过后用 status/workflow/run 三字段 CAS 写为 Queued + Todo/Idle，
    ///     清空 blocked_reason 和 attempt_phase，保留 worktree/session/evidence。
    pub async fn start_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        let can_start_from_backlog = current.status == OrchestratorTaskStatus::Draft
            && current.workflow_state == OrchestratorWorkflowState::Backlog
            && current.run_state == OrchestratorRunState::Idle;
        let can_start_from_todo = current.workflow_state == OrchestratorWorkflowState::Todo
            && current.run_state == OrchestratorRunState::Idle;
        if !can_start_from_backlog && !can_start_from_todo {
            return Err(AppError::generic(format!(
                "只有待整理草稿或待办空闲任务可以开始，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
        )
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .bind(OrchestratorWorkflowState::Todo.as_str())
        .bind(OrchestratorRunState::Idle.as_str())
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(&current.id)
        .bind(current.status.as_str())
        .bind(current.workflow_state.as_str())
        .bind(current.run_state.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(&current.id).await;
        }

        self.get_task(&current.id).await?;
        Err(AppError::generic("任务状态已变化，请刷新后重试"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     人工复核未通过时，用户需要显式 requestRework 并留下原因，后续 scheduler 才能在同一现场继续领取任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅允许 legacy Done + workflow HumanReview + run Idle 的任务进入 Queued + Rework/Idle；
    ///     写入 repairPrompt evidence 和 rework event，保留 worktree/session/runtime 字段。
    pub async fn request_task_rework(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(AppError::generic("返工原因不能为空"));
        }
        let current = self.get_task(task_id).await?;
        if current.status != OrchestratorTaskStatus::Done
            || current.workflow_state != OrchestratorWorkflowState::HumanReview
            || current.run_state != OrchestratorRunState::Idle
        {
            return Err(AppError::generic(format!(
                "只有人工复核中的任务可以请求返工，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
        )
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .bind(OrchestratorWorkflowState::Rework.as_str())
        .bind(OrchestratorRunState::Idle.as_str())
        .bind(OrchestratorAttemptPhase::Failed.as_str())
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(&current.id)
        .bind(OrchestratorTaskStatus::Done.as_str())
        .bind(OrchestratorWorkflowState::HumanReview.as_str())
        .bind(OrchestratorRunState::Idle.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() != 1 {
            self.get_task(&current.id).await?;
            return Err(AppError::generic("任务状态已变化，请刷新后重试"));
        }

        self.add_evidence(
            &current.id,
            EVIDENCE_KIND_REPAIR_PROMPT,
            "人工返工原因",
            "failed",
            reason,
        )
        .await?;
        self.add_event(&current.id, "rework", reason, None).await?;
        self.get_task(&current.id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 deliverReviewedTask 只能从人工复核泳道取得交付执行权，避免普通完成态被误送入 Git side effect pipeline。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅允许 legacy Done + workflow HumanReview + run Idle 原子切到 Delivering + Merging/Delivering；
    ///     不清理 worktree/session，后续由 delivery pipeline 在 per-task lock 内处理 Git 阶段。
    pub async fn start_delivery_from_human_review(
        &self,
        task_id: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        if current.status != OrchestratorTaskStatus::Done
            || current.workflow_state != OrchestratorWorkflowState::HumanReview
            || current.run_state != OrchestratorRunState::Idle
        {
            return Err(AppError::generic(format!(
                "只有人工复核中的任务可以交付，当前 workflow={}, run={}, status={}",
                current.workflow_state.as_str(),
                current.run_state.as_str(),
                current.status.as_str()
            )));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ? AND workflow_state = ? AND run_state = ?",
        )
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .bind(OrchestratorWorkflowState::Merging.as_str())
        .bind(OrchestratorRunState::Delivering.as_str())
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(&current.id)
        .bind(OrchestratorTaskStatus::Done.as_str())
        .bind(OrchestratorWorkflowState::HumanReview.as_str())
        .bind(OrchestratorRunState::Idle.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(&current.id).await;
        }

        self.get_task(&current.id).await?;
        Err(AppError::generic("任务状态已变化，请刷新后重试"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式 cancelTask 表示用户不再希望任务继续被 scheduler 或 delivery 接管，但仍要保留现场和证据供人工审计。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 legacy status 写为 Aborted、split state 写为 Canceled/Idle，清空 blocked_reason 和 attempt_phase；
    ///     不修改 branch/worktree/session/runtime/evidence 字段。
    pub async fn cancel_task(&self, task_id: &str) -> Result<OrchestratorTaskRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(OrchestratorTaskStatus::Aborted.as_str())
        .bind(OrchestratorWorkflowState::Canceled.as_str())
        .bind(OrchestratorRunState::Idle.as_str())
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(now)
        .bind(task_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self.get_task(task_id).await;
        }

        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     看板拖拽是用户手动调整任务工作流阶段的轻量动作，必须避免隐式启动 Runner 或改变交付设置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取当前任务，拒绝 active run_state，只允许移动到固定顺序中的相邻泳道；通过后仅更新 workflow_state 和 updated_at。
    pub async fn move_task_workflow_state(
        &self,
        task_id: &str,
        target: OrchestratorWorkflowState,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let current = self.get_task(task_id).await?;
        self.move_task_workflow_state_from_snapshot(&current, target)
            .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     拖拽移动需要基于用户看到的任务快照做条件更新，防止 scheduler 或其它动作在读写之间改变任务后被覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验快照 run_state 和相邻泳道后，用 workflow_state/run_state 作为 CAS 条件更新；未命中时读取最新任务返回中文业务错误。
    async fn move_task_workflow_state_from_snapshot(
        &self,
        current: &OrchestratorTaskRow,
        target: OrchestratorWorkflowState,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if is_active_run_state(current.run_state) {
            return Err(AppError::generic("运行中的任务不能通过拖拽移动"));
        }

        let current_index = workflow_lane_index(current.workflow_state)?;
        let target_index = workflow_lane_index(target)?;
        if current_index.abs_diff(target_index) != 1 {
            return Err(AppError::generic("只能移动到相邻泳道"));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET workflow_state = ?, updated_at = ? \
             WHERE id = ? AND workflow_state = ? AND run_state = ?",
        )
        .bind(target.as_str())
        .bind(now)
        .bind(&current.id)
        .bind(current.workflow_state.as_str())
        .bind(current.run_state.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() != 1 {
            let latest = self.get_task(&current.id).await?;
            if is_active_run_state(latest.run_state) {
                return Err(AppError::generic("运行中的任务不能通过拖拽移动"));
            }
            return Err(AppError::generic("任务状态已变化，请刷新后重试"));
        }

        self.get_task(&current.id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     早期调用点仍使用 update_task_status；保留兼容可避免后续计划任务重构被一次性阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接转发到 set_task_status，使旧 API 与新命名共享同一实现。
    pub async fn update_task_status(
        &self,
        task_id: &str,
        status: OrchestratorTaskStatus,
        blocked_reason: Option<&str>,
    ) -> Result<OrchestratorTaskRow, AppError> {
        self.set_task_status(task_id, status, blocked_reason).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后续调度器需要记录任务生命周期事件，便于页面展示和问题排查。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成事件 id 和 UTC 时间戳，向 orchestrator_task_events 追加一行。
    pub async fn add_event(
        &self,
        task_id: &str,
        kind: &str,
        message: &str,
        payload_json: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO orchestrator_task_events \
             (id, task_id, kind, message, payload_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(kind)
        .bind(message)
        .bind(payload_json)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后续验证流程需要保存命令输出、文件摘要等交付证据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成证据 id 和 UTC 时间戳，向 orchestrator_task_evidence 追加一行。
    pub async fn add_evidence(
        &self,
        task_id: &str,
        kind: &str,
        title: &str,
        summary: &str,
        content: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO orchestrator_task_evidence \
             (id, task_id, kind, title, summary, content, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(kind)
        .bind(title)
        .bind(summary)
        .bind(content)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 详情页需要按任务读取验证输出和交付证据，且不能混入其它任务的记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id 查询 orchestrator_task_evidence，并按 created_at ASC、id ASC 稳定排序后转换 DTO。
    pub async fn list_evidence(
        &self,
        task_id: &str,
    ) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVIDENCE_COLUMNS} FROM orchestrator_task_evidence \
             WHERE task_id = ? ORDER BY created_at ASC, id ASC"
        ))
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_evidence).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端设备离线时，本机仍允许用户创建远端任务；创建请求必须先持久化为 pending outbox。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验目标设备、路径和请求 JSON 非空，生成 outbox id/时间戳，并插入 status=pending 行。
    pub async fn insert_remote_outbox_pending(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_path: &str,
        remote_project_id: Option<&str>,
        request_json: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_project_id = remote_project_id.and_then(non_empty_trimmed);
        if device_id.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少设备名称"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少项目路径"));
        }
        if request_json.trim().is_empty() {
            return Err(AppError::generic("远端 outbox 请求不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO orchestrator_remote_outbox \
             (id, device_id, device_name, remote_project_path, remote_project_id, request_json, \
              status, remote_task_id, last_error, created_at, updated_at, sent_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(device_id)
        .bind(device_name)
        .bind(remote_project_path)
        .bind(remote_project_id)
        .bind(request_json)
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(Option::<&str>::None)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(&now)
        .bind(Option::<&str>::None)
        .execute(&self.pool)
        .await?;

        Ok(OrchestratorRemoteOutboxRow {
            id,
            device_id: device_id.to_string(),
            device_name: device_name.to_string(),
            remote_project_path: remote_project_path.to_string(),
            remote_project_id: remote_project_id.map(str::to_string),
            request_json: request_json.to_string(),
            status: RemoteOutboxStatus::Pending,
            remote_task_id: None,
            last_error: None,
            created_at: now.clone(),
            updated_at: now,
            sent_at: None,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层和 dispatcher 需要按 id 读取 outbox 当前状态，便于返回 UI 或处理并发 claim 失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询单条 outbox 行；缺失返回 None，存在时转换为强类型 Row。
    pub async fn get_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox WHERE id = ?"
        ))
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_remote_outbox(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后台 dispatcher 每个 tick 需要按创建顺序扫描尚未投递的 pending 远端任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 status=pending 的 outbox 行，按 created_at/id 稳定排序并限制批量大小。
    pub async fn list_pending_remote_outbox_items(
        &self,
        limit: i64,
    ) -> Result<Vec<OrchestratorRemoteOutboxRow>, AppError> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox \
             WHERE status = ? ORDER BY created_at ASC, id ASC LIMIT ?"
        ))
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_outbox).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用在 outbox item 进入 sending 后崩溃时，后台 dispatcher 重启后必须释放旧 lease，避免任务永久卡在发送中。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 updated_at 早于 lease 截止时间的 sending 行恢复为 pending，写入 last_error 和 updated_at，返回恢复数量。
    pub async fn recover_stale_remote_outbox_sending_items(
        &self,
        lease: Duration,
    ) -> Result<u64, AppError> {
        let lease = chrono::Duration::from_std(lease)
            .map_err(|err| AppError::generic(format!("远端 outbox lease 无效: {err}")))?;
        let cutoff = (Utc::now() - lease).to_rfc3339();
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET status = ?, last_error = ?, updated_at = ? \
             WHERE status = ? AND updated_at < ?",
        )
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind("sending lease expired; recovered for retry")
        .bind(now)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端项目任务列表需要合并本机尚未投递或投递失败的 outbox 项，避免用户看不到离线提交。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id 和 remote_project_path 查询 pending/sending/failed 行；mirrored 行不再作为 pending 展示。
    pub async fn list_remote_outbox_items_for_project_path(
        &self,
        device_id: &str,
        remote_project_path: &str,
    ) -> Result<Vec<OrchestratorRemoteOutboxRow>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox \
             WHERE device_id = ? AND remote_project_path = ? AND status IN (?, ?, ?) \
             ORDER BY created_at ASC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_path)
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(RemoteOutboxStatus::Sending.as_str())
        .bind(RemoteOutboxStatus::Failed.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_outbox).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 必须先独占 claim 一条 pending item，避免多个 tick 或多个任务重复创建远端 task。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 `UPDATE ... WHERE status=pending` 原子切换到 sending；命中返回更新后 Row，未命中返回 None。
    pub async fn claim_remote_outbox_item_as_sending(
        &self,
        item_id: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(RemoteOutboxStatus::Sending.as_str())
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Pending.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
            .map(Some)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     网络离线、设备不在线或连接超时不代表请求无效，必须回到 pending 等待下次自动投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 sending item 改回 pending，写 last_error/updated_at，并返回当前 Row。
    pub async fn mark_remote_outbox_pending_after_network_failure(
        &self,
        item_id: &str,
        last_error: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(last_error)
        .bind(now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .execute(&self.pool)
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端协议错误或业务校验错误不可自动重试，应停止 dispatcher 对该 item 的反复投递。
    ///     但旧投递路径晚到时不能覆盖已经恢复、成功镜像或人工处理过的 outbox 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 item 仍为 sending 时标记 failed 并保存 last_error；非 sending 时不覆盖，返回当前 Row。
    pub async fn mark_remote_outbox_failed(
        &self,
        item_id: &str,
        last_error: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_remote_outbox SET status = ?, last_error = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(RemoteOutboxStatus::Failed.as_str())
        .bind(last_error)
        .bind(now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .execute(&self.pool)
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在原 Automation UI 对协议/校验失败的 outbox 选择「重新发送」时，必须原子恢复为 pending，
    ///     并保留原 request payload 与 clientRequestId，避免创建重复远端任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅用 `WHERE id=? AND status='failed'` 更新 status=pending、清空 last_error、刷新 updated_at，
    ///     绝不改写 request_json；0 行更新时读取当前状态，缺失返回 not_found，其它状态返回 conflict。
    pub async fn retry_failed_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let item_id = item_id.trim();
        if item_id.is_empty() {
            return Err(AppError::validation("远端 outbox ID 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET status = ?, last_error = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(RemoteOutboxStatus::Pending.as_str())
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Failed.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self
                .get_remote_outbox_item(item_id)
                .await?
                .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")));
        }

        match self.get_remote_outbox_item(item_id).await? {
            None => Err(AppError::not_found(format!(
                "远端 outbox 不存在: {item_id}"
            ))),
            Some(current) => Err(AppError::conflict(format!(
                "只有失败的远端 outbox 可以重新发送，当前状态为 {}",
                current.status.as_str()
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户确认放弃某条失败 outbox 后，条目应进入 discarded 终态：保留审计与 last_error，
    ///     但不再参与 dispatcher、active 列表或 Attention 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅用 `WHERE id=? AND status='failed'` 更新 status=discarded 与 updated_at，保留 last_error/request_json；
    ///     0 行更新时读取当前状态，缺失返回 not_found，其它状态返回 conflict。
    pub async fn discard_failed_remote_outbox_item(
        &self,
        item_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let item_id = item_id.trim();
        if item_id.is_empty() {
            return Err(AppError::validation("远端 outbox ID 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET status = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(RemoteOutboxStatus::Discarded.as_str())
        .bind(&now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Failed.as_str())
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 1 {
            return self
                .get_remote_outbox_item(item_id)
                .await?
                .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")));
        }

        match self.get_remote_outbox_item(item_id).await? {
            None => Err(AppError::not_found(format!(
                "远端 outbox 不存在: {item_id}"
            ))),
            Some(current) => Err(AppError::conflict(format!(
                "只有失败的远端 outbox 可以放弃发送，当前状态为 {}",
                current.status.as_str()
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     dispatcher 打开远端项目后会拿到远端 local projectId，outbox 行应记录该映射便于后续排查和 UI 展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 remote_project_id 与 updated_at；不改变投递状态。
    pub async fn update_remote_outbox_remote_project_id(
        &self,
        item_id: &str,
        remote_project_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_remote_outbox SET remote_project_id = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(remote_project_id)
        .bind(now)
        .bind(item_id)
        .execute(&self.pool)
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧 outbox 行可能缺少 clientRequestId；dispatcher 补齐幂等键后需要持久化，确保后续重试复用同一请求体。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 item 仍为 sending 时更新 request_json 和 updated_at；并发状态变化时返回 None。
    pub async fn update_remote_outbox_request_json_if_sending(
        &self,
        item_id: &str,
        request_json: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        if request_json.trim().is_empty() {
            return Err(AppError::generic("远端 outbox 请求不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_remote_outbox SET request_json = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(request_json)
        .bind(now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.get_remote_outbox_item(item_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端任务创建成功后，本机 pending item 应转为 mirrored 并保存远端 task id，避免后续重复投递。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 status=mirrored、remote_task_id、sent_at 和 updated_at，清空 last_error 后返回当前 Row。
    pub async fn mark_remote_outbox_mirrored(
        &self,
        item_id: &str,
        remote_task_id: &str,
    ) -> Result<OrchestratorRemoteOutboxRow, AppError> {
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET status = ?, remote_task_id = ?, last_error = ?, updated_at = ?, sent_at = ? \
             WHERE id = ?",
        )
        .bind(RemoteOutboxStatus::Mirrored.as_str())
        .bind(remote_task_id)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(&now)
        .bind(item_id)
        .execute(&self.pool)
        .await?;
        self.get_remote_outbox_item(item_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {item_id}")))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     成功投递后，outbox mirrored 状态、remote_project_id 和 mirror payload 必须同生共死；
    ///     否则 UI 可能看不到已创建的远端任务，或 outbox 显示已完成但 mirror 丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个 SQLite 事务里仅对 status=sending 的 item 写 mirrored/sent_at/remote_task_id/remote_project_id，
    ///     并按 `(device_id, remote_task_id)` upsert mirror；状态不再是 sending 时返回 None 且不覆盖。
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_remote_outbox_mirrored_and_upsert_mirror_if_sending(
        &self,
        item_id: &str,
        device_id: &str,
        device_name: &str,
        remote_project_id: &str,
        remote_project_path: &str,
        remote_task_id: &str,
        payload_json: &str,
    ) -> Result<Option<OrchestratorRemoteOutboxRow>, AppError> {
        let item_id = item_id.trim();
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_id = remote_project_id.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_task_id = remote_task_id.trim();
        if item_id.is_empty() {
            return Err(AppError::generic("远端 outbox 缺少 ID"));
        }
        if device_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备名称"));
        }
        if remote_project_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目 ID"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目路径"));
        }
        if remote_task_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少任务 ID"));
        }
        if payload_json.trim().is_empty() {
            return Err(AppError::generic("远端任务镜像 payload 不能为空"));
        }

        let mut tx = self.pool.begin().await?;
        let now = Utc::now().to_rfc3339();
        let updated = sqlx::query(
            "UPDATE orchestrator_remote_outbox \
             SET remote_project_id = ?, status = ?, remote_task_id = ?, last_error = ?, \
                 updated_at = ?, sent_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(remote_project_id)
        .bind(RemoteOutboxStatus::Mirrored.as_str())
        .bind(remote_task_id)
        .bind(Option::<&str>::None)
        .bind(&now)
        .bind(&now)
        .bind(item_id)
        .bind(RemoteOutboxStatus::Sending.as_str())
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.commit().await?;
            return Ok(None);
        }

        let mirror_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO orchestrator_remote_task_mirrors \
             (id, device_id, device_name, remote_project_id, remote_project_path, remote_task_id, \
              payload_json, last_synced_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(device_id, remote_task_id) DO UPDATE SET \
               device_name = excluded.device_name, \
               remote_project_id = excluded.remote_project_id, \
               remote_project_path = excluded.remote_project_path, \
               payload_json = excluded.payload_json, \
               last_synced_at = excluded.last_synced_at",
        )
        .bind(&mirror_id)
        .bind(device_id)
        .bind(device_name)
        .bind(remote_project_id)
        .bind(remote_project_path)
        .bind(remote_task_id)
        .bind(payload_json)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(&format!(
            "SELECT {REMOTE_OUTBOX_COLUMNS} FROM orchestrator_remote_outbox WHERE id = ?"
        ))
        .bind(item_id)
        .fetch_one(&mut *tx)
        .await?;
        let item = row_to_remote_outbox(&row)?;
        tx.commit().await?;
        Ok(Some(item))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     本机需要保存远端任务的最新展示快照；同一远端 task 重复同步时应覆盖旧 payload。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用 `(device_id, remote_task_id)` 唯一键 upsert mirror，更新 payload_json 和 last_synced_at 后返回 Row。
    pub async fn upsert_remote_task_mirror(
        &self,
        device_id: &str,
        device_name: &str,
        remote_project_id: &str,
        remote_project_path: &str,
        remote_task_id: &str,
        payload_json: &str,
    ) -> Result<RemoteMirrorTask, AppError> {
        let device_id = device_id.trim();
        let device_name = device_name.trim();
        let remote_project_id = remote_project_id.trim();
        let remote_project_path = remote_project_path.trim();
        let remote_task_id = remote_task_id.trim();
        if device_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备 ID"));
        }
        if device_name.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少设备名称"));
        }
        if remote_project_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目 ID"));
        }
        if remote_project_path.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少项目路径"));
        }
        if remote_task_id.is_empty() {
            return Err(AppError::generic("远端任务镜像缺少任务 ID"));
        }
        if payload_json.trim().is_empty() {
            return Err(AppError::generic("远端任务镜像 payload 不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO orchestrator_remote_task_mirrors \
             (id, device_id, device_name, remote_project_id, remote_project_path, remote_task_id, \
              payload_json, last_synced_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(device_id, remote_task_id) DO UPDATE SET \
               device_name = excluded.device_name, \
               remote_project_id = excluded.remote_project_id, \
               remote_project_path = excluded.remote_project_path, \
               payload_json = excluded.payload_json, \
               last_synced_at = excluded.last_synced_at",
        )
        .bind(&id)
        .bind(device_id)
        .bind(device_name)
        .bind(remote_project_id)
        .bind(remote_project_path)
        .bind(remote_task_id)
        .bind(payload_json)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.get_remote_task_mirror(device_id, remote_task_id)
            .await?
            .ok_or_else(|| AppError::not_found("远端任务镜像不存在"))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mirror upsert 和远端任务详情需要按设备与远端 task id 读取唯一镜像。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 `(device_id, remote_task_id)` 唯一行，缺失返回 None。
    pub async fn get_remote_task_mirror(
        &self,
        device_id: &str,
        remote_task_id: &str,
    ) -> Result<Option<RemoteMirrorTask>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_task_id = ?"
        ))
        .bind(device_id)
        .bind(remote_task_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_remote_mirror(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端项目在线刷新后，UI 需要读取该项目所有已缓存的远端任务镜像。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id + remote_project_id 查询 mirror，按 last_synced_at/id 稳定排序。
    pub async fn list_remote_task_mirrors_for_project(
        &self,
        device_id: &str,
        remote_project_id: &str,
    ) -> Result<Vec<RemoteMirrorTask>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_project_id = ? ORDER BY last_synced_at DESC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_mirror).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端离线时可能无法拿到最新 remote_project_id，本机仍应按保存的远端路径展示最近镜像快照。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id + remote_project_path 查询 mirror，按 last_synced_at/id 稳定排序。
    pub async fn list_remote_task_mirrors_for_project_path(
        &self,
        device_id: &str,
        remote_project_path: &str,
    ) -> Result<Vec<RemoteMirrorTask>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {REMOTE_MIRROR_COLUMNS} FROM orchestrator_remote_task_mirrors \
             WHERE device_id = ? AND remote_project_path = ? ORDER BY last_synced_at DESC, id ASC"
        ))
        .bind(device_id)
        .bind(remote_project_path)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_remote_mirror).collect()
    }
}

/// Business Logic（为什么需要这个函数）:
///     幂等键必须绑定项目与请求内容指纹，避免跨项目泄露任务 goal/acceptance，或同键不同 payload 静默复用旧任务。
///
/// Code Logic（这个函数做什么）:
///     对 create 语义字段做稳定序列化后 SHA256，输出小写 hex 指纹。
fn create_request_fingerprint(
    row: &OrchestratorTaskRow,
    create_action: OrchestratorCreateAction,
    external_labels_json: &Option<String>,
) -> Result<String, AppError> {
    use sha2::{Digest, Sha256};

    let action = match create_action {
        OrchestratorCreateAction::Backlog => "backlog",
        OrchestratorCreateAction::Todo => "todo",
        OrchestratorCreateAction::Start => "start",
    };
    let labels = external_labels_json.as_deref().unwrap_or("");
    let payload = format!(
        "project_id={}\0title={}\0goal={}\0acceptance={}\0priority={}\0action={}\0source={}\0external_id={}\0external_identifier={}\0external_url={}\0external_state={}\0external_labels={}",
        row.project_id,
        row.title,
        row.goal,
        row.acceptance_criteria,
        row.priority,
        action,
        row.source,
        row.external_id.as_deref().unwrap_or(""),
        row.external_identifier.as_deref().unwrap_or(""),
        row.external_url.as_deref().unwrap_or(""),
        row.external_state.as_deref().unwrap_or(""),
        labels,
    );
    let digest = Sha256::digest(payload.as_bytes());
    Ok(format!("{digest:x}"))
}

/// Business Logic（为什么需要这个函数）:
///     幂等命中后必须校验项目与指纹，不能把 project A 的任务返回给 project B 或不同 payload。
///
/// Code Logic（这个函数做什么）:
///     比对映射行的 project_id / request_fingerprint；通过后读取并返回既有 task。
///     调用方负责 commit 事务（本函数不接管 Transaction 所有权）。
async fn resolve_existing_create_request(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    request_id: &str,
    project_id: &str,
    request_fingerprint: &str,
    existing: SqliteRow,
) -> Result<OrchestratorTaskRow, AppError> {
    let mapped_project_id: String = existing.try_get("project_id")?;
    let task_id: String = existing.try_get("task_id")?;
    let mapped_fingerprint: String = existing
        .try_get::<String, _>("request_fingerprint")
        .unwrap_or_default();

    // 旧行可能 project_id 为空：回退到任务表归属做跨项目校验。
    let effective_project_id = if mapped_project_id.trim().is_empty() {
        let task_project: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM orchestrator_tasks WHERE id = ?")
                .bind(&task_id)
                .fetch_optional(&mut **tx)
                .await?;
        task_project.unwrap_or_default()
    } else {
        mapped_project_id
    };

    if effective_project_id != project_id {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 已绑定项目 `{effective_project_id}`，不能用于项目 `{project_id}`"
        )));
    }
    // 旧库迁移后 request_fingerprint 可能仍为空：无法可靠比对 payload，必须 fail-closed，
    // 要求客户端换新 request id，禁止把空指纹当通配符静默匹配任意 payload。
    if mapped_fingerprint.trim().is_empty() {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 缺少可靠请求指纹，请使用新的 clientRequestId 重新创建"
        )));
    }
    if mapped_fingerprint != request_fingerprint {
        return Err(AppError::conflict(format!(
            "clientRequestId `{request_id}` 已用于不同创建内容，拒绝冲突重放"
        )));
    }

    let task_row = sqlx::query(&format!(
        "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
    ))
    .bind(&task_id)
    .fetch_one(&mut **tx)
    .await?;
    row_to_task(&task_row)
}

/// Business Logic（为什么需要这个函数）:
///     旧库的幂等表只有 request_id 主键，缺少 project 作用域与 payload 指纹，升级后必须补齐列并回填 project_id。
///
/// Code Logic（这个函数做什么）:
///     ensure 两列存在；把空 project_id 按 task_id 回填为任务归属项目，保留用户历史映射。
async fn migrate_remote_task_create_request_scope(pool: &SqlitePool) -> Result<(), AppError> {
    ensure_column(
        pool,
        "orchestrator_remote_task_create_requests",
        "project_id",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    ensure_column(
        pool,
        "orchestrator_remote_task_create_requests",
        "request_fingerprint",
        "TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    // 回填 project_id：仅更新仍为空的旧行，避免覆盖新写入的作用域。
    sqlx::query(
        "UPDATE orchestrator_remote_task_create_requests \
         SET project_id = (
             SELECT t.project_id FROM orchestrator_tasks t \
             WHERE t.id = orchestrator_remote_task_create_requests.task_id
         ) \
         WHERE (project_id IS NULL OR project_id = '') \
           AND EXISTS (
             SELECT 1 FROM orchestrator_tasks t \
             WHERE t.id = orchestrator_remote_task_create_requests.task_id
           )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     用户可能从旧版本直接升级，旧 SQLite 表缺少新列时必须原地补齐且保留已有任务数据。
///
/// Code Logic（这个函数做什么）:
///     通过 `pragma_table_info` 检查列是否存在；缺失时执行 `ALTER TABLE ... ADD COLUMN`，并返回是否新增列。
async fn ensure_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<bool, AppError> {
    let existing: Option<String> = sqlx::query_scalar(&format!(
        "SELECT name FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(column)
    .fetch_optional(pool)
    .await?;
    if existing.is_some() {
        return Ok(false);
    }

    sqlx::query(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition}"
    ))
    .execute(pool)
    .await?;
    Ok(true)
}

/// Business Logic（为什么需要这个函数）:
///     split state 新列初次加入旧库时会得到默认 backlog/idle；旧任务需要根据 legacy status 恢复正确看板与运行态。
///
/// Code Logic（这个函数做什么）:
///     查找仍处于默认 workflow/run state 的任务，解析 legacy status 后按 SplitTaskState 映射更新两列。
async fn backfill_split_state_from_legacy_status(pool: &SqlitePool) -> Result<(), AppError> {
    let rows = sqlx::query(
        "SELECT id, status FROM orchestrator_tasks WHERE workflow_state = ? AND run_state = ?",
    )
    .bind(OrchestratorWorkflowState::Backlog.as_str())
    .bind(OrchestratorRunState::Idle.as_str())
    .fetch_all(pool)
    .await?;

    for row in rows {
        let id: String = row.try_get("id")?;
        let status_text: String = row.try_get("status")?;
        let status = OrchestratorTaskStatus::from_str(&status_text)?;
        let split_state = SplitTaskState::from_legacy_status(status);
        sqlx::query("UPDATE orchestrator_tasks SET workflow_state = ?, run_state = ? WHERE id = ?")
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(&id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     旧版 split state 曾把 legacy Queued 映射为 Todo/Queued；升级后 scheduler 只领取 Idle/Blocked，必须避免这些旧排队任务卡住。
///
/// Code Logic（这个函数做什么）:
///     精准把 status=queued、workflow_state=todo、run_state=queued 的历史行规范化为 Todo/Idle，不覆盖其它用户调整过的 split state。
async fn normalize_queued_split_state(pool: &SqlitePool) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE orchestrator_tasks \
         SET run_state = ? \
         WHERE status = ? AND workflow_state = ? AND run_state = ?",
    )
    .bind(OrchestratorRunState::Idle.as_str())
    .bind(OrchestratorTaskStatus::Queued.as_str())
    .bind(OrchestratorWorkflowState::Todo.as_str())
    .bind(OrchestratorRunState::Queued.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     tracker 标签是预留给外部系统的结构化数组，写库时必须保持稳定 JSON array，避免后续 mirror/outbox 解析歧义。
///
/// Code Logic（这个函数做什么）:
///     将 Option<Vec<String>> 序列化为 Option<String>；None 存 NULL，Some 始终存紧凑 JSON array。
fn serialize_external_labels(labels: &Option<Vec<String>>) -> Result<Option<String>, AppError> {
    labels
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(AppError::from)
}

/// Business Logic（为什么需要这个函数）:
///     旧库和 mixed-version remote 可能没有 tracker 标签；读取时需要安全回退为 None，同时拒绝损坏的非数组值。
///
/// Code Logic（这个函数做什么）:
///     将 external_labels_json 的 TEXT/NULL 转为 Option<Vec<String>>；NULL 或空白字符串返回 None。
fn deserialize_external_labels(
    labels_json: Option<String>,
) -> Result<Option<Vec<String>>, AppError> {
    let Some(raw) = labels_json else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str::<Vec<String>>(trimmed)?))
}

/// Business Logic（为什么需要这个函数）:
///     多个查询都需要把 SQLite 行转换成任务 Row，统一解析可避免字段遗漏和状态字符串散落。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取 orchestrator_tasks 全字段，并把 status 字符串解析为枚举。
fn row_to_task(row: &SqliteRow) -> Result<OrchestratorTaskRow, AppError> {
    let status_text: String = row.try_get("status")?;
    let workflow_state_text: String = row.try_get("workflow_state")?;
    let run_state_text: String = row.try_get("run_state")?;
    let attempt_phase_text: Option<String> = row.try_get("attempt_phase")?;
    let external_labels_json: Option<String> = row.try_get("external_labels_json")?;
    Ok(OrchestratorTaskRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        goal: row.try_get("goal")?,
        acceptance_criteria: row.try_get("acceptance_criteria")?,
        status: OrchestratorTaskStatus::from_str(&status_text)?,
        workflow_state: OrchestratorWorkflowState::from_str(&workflow_state_text)?,
        run_state: OrchestratorRunState::from_str(&run_state_text)?,
        attempt_phase: attempt_phase_text
            .as_deref()
            .map(OrchestratorAttemptPhase::from_str)
            .transpose()?,
        source: row.try_get("source")?,
        external_id: row.try_get("external_id")?,
        external_identifier: row.try_get("external_identifier")?,
        external_url: row.try_get("external_url")?,
        external_state: row.try_get("external_state")?,
        external_labels: deserialize_external_labels(external_labels_json)?,
        runner_provider: row.try_get("runner_provider")?,
        claude_session_id: row.try_get("claude_session_id")?,
        transcript_path: row.try_get("transcript_path")?,
        runtime_started_at: row.try_get("runtime_started_at")?,
        last_activity_at: row.try_get("last_activity_at")?,
        last_runtime_event: row.try_get("last_runtime_event")?,
        last_runtime_message: row.try_get("last_runtime_message")?,
        priority: row.try_get("priority")?,
        branch_name: row.try_get("branch_name")?,
        worktree_id: row.try_get("worktree_id")?,
        session_id: row.try_get("session_id")?,
        blocked_reason: row.try_get("blocked_reason")?,
        attempt: row.try_get("attempt")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     legacy 项目配置表用 INTEGER 0/1 保存布尔值，兼容 DTO 需要真实 boolean。
///
/// Code Logic（这个函数做什么）:
///     将 SQLite INTEGER 按非零即 true 的规则转换为 bool。
fn sqlite_bool(value: i64) -> bool {
    value != 0
}

/// Business Logic（为什么需要这个函数）:
///     legacy 验证命令以 JSON 文本持久化，兼容读取时需要还原为字符串数组。
///
/// Code Logic（这个函数做什么）:
///     使用 serde_json 解析 Vec<String>；解析失败返回 AppError，暴露损坏配置数据。
fn parse_verification_commands(value: &str) -> Result<Vec<String>, AppError> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|err| AppError::generic(format!("Orchestrator 验证命令解析失败: {err}")))
}

/// Business Logic（为什么需要这个函数）:
///     仓储读取 legacy 项目配置时需要统一处理 bool 转换和 verification_commands JSON 解析。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 提取 orchestrator_project_config 全字段并组装 OrchestratorProjectConfigDto。
fn row_to_project_config(row: &SqliteRow) -> Result<OrchestratorProjectConfigDto, AppError> {
    let verification_commands_json: String = row.try_get("verification_commands_json")?;
    Ok(OrchestratorProjectConfigDto {
        project_id: row.try_get("project_id")?,
        enabled: sqlite_bool(row.try_get("enabled")?),
        max_concurrent_tasks: row.try_get("max_concurrent_tasks")?,
        branch_prefix: row.try_get("branch_prefix")?,
        verification_commands: parse_verification_commands(&verification_commands_json)?,
        auto_commit: sqlite_bool(row.try_get("auto_commit")?),
        auto_push_task_branch: sqlite_bool(row.try_get("auto_push_task_branch")?),
        auto_merge_to_main: sqlite_bool(row.try_get("auto_merge_to_main")?),
        auto_push_main: sqlite_bool(row.try_get("auto_push_main")?),
        retry_limit: row.try_get("retry_limit")?,
        retain_worktree_on_done: sqlite_bool(row.try_get("retain_worktree_on_done")?),
        retain_worktree_on_blocked: sqlite_bool(row.try_get("retain_worktree_on_blocked")?),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     Evidence 表字段需要统一投影为前端 DTO，避免命令层直接依赖 SQLite row。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取 orchestrator_task_evidence 全字段并组装 OrchestratorEvidenceDto。
fn row_to_evidence(row: &SqliteRow) -> Result<OrchestratorEvidenceDto, AppError> {
    Ok(OrchestratorEvidenceDto {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        kind: row.try_get("kind")?,
        title: row.try_get("title")?,
        summary: row.try_get("summary")?,
        content: row.try_get("content")?,
        created_at: row.try_get("created_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     任务尝试表后续会被 runner、completion sentinel 和详情接口共同读取，必须统一 SQLite row 解析。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取 orchestrator_task_attempts 全字段并组装 OrchestratorTaskAttemptRow。
fn row_to_attempt(row: &SqliteRow) -> Result<OrchestratorTaskAttemptRow, AppError> {
    Ok(OrchestratorTaskAttemptRow {
        id: row.try_get("id")?,
        task_id: row.try_get("task_id")?,
        attempt: row.try_get("attempt")?,
        worktree_id: row.try_get("worktree_id")?,
        session_id: row.try_get("session_id")?,
        prompt: row.try_get("prompt")?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        completed_at: row.try_get("completed_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     optional 文本字段写库前需要把空白归一为 None，避免远端 projectId 空字符串被误当作有效 ID。
///
/// Code Logic（这个函数做什么）:
///     trim 输入字符串，非空返回原 trim 后 &str，空白返回 None。
fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Business Logic（为什么需要这个函数）:
///     任务创建动作需要在入库前统一映射 legacy status 与 split state，避免 Tauri、HTTP 和 P2P 入口语义漂移。
///
/// Code Logic（这个函数做什么）:
///     克隆调用方构造的完整任务行，按 createAction 覆盖 status/workflow_state/run_state，并清理运行期阻塞字段。
fn task_row_for_create_action(
    row: &OrchestratorTaskRow,
    create_action: OrchestratorCreateAction,
) -> OrchestratorTaskRow {
    let mut next = row.clone();
    let split_state = SplitTaskState::from_create_action(create_action);
    next.status = create_action.initial_status();
    next.workflow_state = split_state.workflow_state;
    next.run_state = split_state.run_state;
    next.attempt_phase = None;
    next.blocked_reason = None;
    next
}

/// Business Logic（为什么需要这个函数）:
///     outbox 表读取后必须还原强类型状态和所有时间字段，供 dispatcher 与 UI 共用。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 提取 orchestrator_remote_outbox 全字段并解析 status。
fn row_to_remote_outbox(row: &SqliteRow) -> Result<OrchestratorRemoteOutboxRow, AppError> {
    let status_text: String = row.try_get("status")?;
    Ok(OrchestratorRemoteOutboxRow {
        id: row.try_get("id")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        remote_project_path: row.try_get("remote_project_path")?,
        remote_project_id: row.try_get("remote_project_id")?,
        request_json: row.try_get("request_json")?,
        status: RemoteOutboxStatus::from_str(&status_text)?,
        remote_task_id: row.try_get("remote_task_id")?,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        sent_at: row.try_get("sent_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     mirror 表读取后需要形成统一 Row，命令层再负责把 payload_json 解析为远端任务 DTO。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 提取 orchestrator_remote_task_mirrors 全字段。
fn row_to_remote_mirror(row: &SqliteRow) -> Result<RemoteMirrorTask, AppError> {
    Ok(RemoteMirrorTask {
        id: row.try_get("id")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        remote_project_id: row.try_get("remote_project_id")?,
        remote_project_path: row.try_get("remote_project_path")?,
        remote_task_id: row.try_get("remote_task_id")?,
        payload_json: row.try_get("payload_json")?,
        last_synced_at: row.try_get("last_synced_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::OrchestratorTaskDto;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Row;
    use std::str::FromStr;

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

        let rows = sqlx::query(
            "SELECT id, workflow_state, run_state FROM orchestrator_tasks ORDER BY id ASC",
        )
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
            err.to_string().contains("缺少可靠请求指纹")
                || err.to_string().contains("clientRequestId"),
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
            .move_task_workflow_state_from_snapshot(
                &stale_snapshot,
                OrchestratorWorkflowState::Todo,
            )
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

        assert_eq!(finished.status, OrchestratorTaskStatus::Aborted);
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
                "running",
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
                "running",
            )
            .await
            .unwrap();
        repo.add_attempt(
            "task-completed",
            1,
            "worktree-2",
            "session-completed",
            "prompt",
            "running",
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

        let running = repo
            .mark_task_running(&task.id, "agent/task-1-test", "worktree-1", "session-1")
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

        let running = repo
            .mark_task_running_attempt(
                &task.id,
                "agent/task-runtime-reset",
                "worktree-2",
                "session-2",
                2,
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

        let updated = repo
            .update_task_attempt_phase(&task.id, OrchestratorAttemptPhase::BuildingPrompt)
            .await
            .unwrap();

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
        repo.mark_task_running_attempt(
            &task.id,
            "agent/task-active-guard",
            "worktree-2",
            "session-new",
            2,
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
        repo.mark_task_running_attempt(
            &task.id,
            "agent/task-runtime",
            "worktree-1",
            "session-1",
            1,
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

        let returned = repo
            .mark_task_running(&task.id, "agent/task-1-test", "worktree-1", "session-1")
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
}
