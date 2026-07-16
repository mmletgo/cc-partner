//! OrchestratorRepo 共享常量与 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分后的子模块需要共享列清单、行映射与迁移辅助函数。
//!
//! Code Logic（这个模块做什么）:
//!     导出 monofile 顶层常量、schema SQL 与 helper（`pub(crate)`）。

#![allow(dead_code)]
#![allow(unused_imports)]

//! Orchestrator SQLite repository.

use crate::error::AppError;
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimCandidate, ClaimCasOutcome, ClaimScanCursor,
    CLAIM_CANDIDATE_LIMIT,
};
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
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Acquire, Row};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

pub(crate) const TASK_COLUMNS: &str = "id, project_id, title, goal, acceptance_criteria, status, priority, \
    workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
    external_url, external_state, external_labels_json, runner_provider, claude_session_id, \
    agent_session_id, transcript_path, runtime_started_at, last_activity_at, last_runtime_event, \
    last_runtime_message, branch_name, worktree_id, session_id, prepare_claim_token, blocked_reason, attempt, \
    state_version, created_at, updated_at, started_at, finished_at";

/// Business Logic（为什么需要这个函数）:
///     claim 候选 SELECT 需要 JOIN `workbench_projects`，未加表前缀时 `id/created_at/updated_at` 会与 project 列歧义。
///
/// Code Logic（这个函数做什么）:
///     把 `TASK_COLUMNS` 每个字段加上 `table.` 前缀，供 JOIN 查询使用。
pub(crate) fn task_columns_for_alias(table: &str) -> String {
    TASK_COLUMNS
        .split(',')
        .map(|column| format!("{table}.{}", column.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) const WORKFLOW_LANE_ORDER: [OrchestratorWorkflowState; 8] = [
    OrchestratorWorkflowState::Backlog,
    OrchestratorWorkflowState::Todo,
    OrchestratorWorkflowState::InProgress,
    OrchestratorWorkflowState::HumanReview,
    OrchestratorWorkflowState::Rework,
    OrchestratorWorkflowState::Merging,
    OrchestratorWorkflowState::Done,
    OrchestratorWorkflowState::Canceled,
];
pub(crate) const PROJECT_CONFIG_COLUMNS: &str =
    "project_id, enabled, max_concurrent_tasks, branch_prefix, \
    verification_commands_json, auto_commit, auto_push_task_branch, auto_merge_to_main, \
    auto_push_main, retry_limit, retain_worktree_on_done, retain_worktree_on_blocked, \
    created_at, updated_at";

/// Business Logic（为什么需要这个函数）:
///     看板拖拽必须使用固定泳道顺序判断相邻关系，避免前端或调用方传入任意状态导致跨阶段跳转。
///
/// Code Logic（这个函数做什么）:
///     在 WORKFLOW_LANE_ORDER 中查找 workflow_state 的索引；枚举完整覆盖，缺失时返回业务错误暴露代码缺陷。
pub(crate) fn workflow_lane_index(state: OrchestratorWorkflowState) -> Result<usize, AppError> {
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
pub(crate) fn is_active_run_state(state: OrchestratorRunState) -> bool {
    matches!(
        state,
        OrchestratorRunState::Preparing
            | OrchestratorRunState::Running
            | OrchestratorRunState::Verifying
            | OrchestratorRunState::Delivering
    )
}
pub(crate) const EVIDENCE_COLUMNS: &str = "id, task_id, kind, title, summary, content, created_at";
pub(crate) const ATTEMPT_COLUMNS: &str =
    "id, task_id, attempt, worktree_id, session_id, prompt, status, \
    created_at, completed_at";
pub(crate) const REMOTE_OUTBOX_COLUMNS: &str = "id, device_id, device_name, remote_project_path, \
    remote_project_id, request_json, status, remote_task_id, last_error, state_version, created_at, updated_at, \
    sent_at";
pub(crate) const REMOTE_MIRROR_COLUMNS: &str = "id, device_id, device_name, remote_project_id, \
    remote_project_path, remote_task_id, payload_json, last_synced_at";

/// Orchestrator 任务表 schema。
///
/// Business Logic（为什么需要这个常量）:
///     任务队列持久化需要稳定的 CREATE TABLE 语句，供 init_schema 与测试共享。
///
/// Code Logic（这个常量做什么）:
///     定义 `orchestrator_tasks` 的 CREATE TABLE IF NOT EXISTS 语句。
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
  agent_session_id TEXT,
  transcript_path TEXT,
  runtime_started_at TEXT,
  last_activity_at TEXT,
  last_runtime_event TEXT,
  last_runtime_message TEXT,
  priority INTEGER NOT NULL DEFAULT 0,
  branch_name TEXT,
  worktree_id TEXT,
  session_id TEXT,
  prepare_claim_token TEXT,
  blocked_reason TEXT,
  attempt INTEGER NOT NULL DEFAULT 0,
  state_version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT
)";

pub(crate) const ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_tasks_project_status \
     ON orchestrator_tasks(project_id, status, priority, created_at)";

pub(crate) const ORCHESTRATOR_TASKS_STATUS_INDEX: &str =
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

pub(crate) const ORCHESTRATOR_TASK_EVENTS_INDEX: &str =
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

pub(crate) const ORCHESTRATOR_TASK_EVIDENCE_INDEX: &str =
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

pub(crate) const ORCHESTRATOR_TASK_ATTEMPTS_SESSION_INDEX: &str =
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
  state_version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sent_at TEXT
)";

pub(crate) const ORCHESTRATOR_REMOTE_OUTBOX_STATUS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_remote_outbox_status \
     ON orchestrator_remote_outbox(status, updated_at, device_id)";

pub(crate) const ORCHESTRATOR_REMOTE_OUTBOX_PROJECT_INDEX: &str =
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

pub(crate) const ORCHESTRATOR_REMOTE_TASK_MIRROR_PROJECT_INDEX: &str =
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

/// Business Logic（为什么需要这个函数）:
///     幂等键必须绑定项目与请求内容指纹，避免跨项目泄露任务 goal/acceptance，或同键不同 payload 静默复用旧任务。
///     指纹编码必须无歧义：字段内 NUL/分隔符不得制造跨字段边界碰撞，否则 fail-closed 会被绕过。
///
/// Code Logic（这个函数做什么）:
///     将 create 语义字段打包为固定 key 顺序的 JSON 对象后 SHA256，输出小写 hex 指纹。
///     JSON 字符串转义保证字段边界无歧义，比 NUL-join 更安全。
pub(crate) fn create_request_fingerprint(
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
    // 固定 key 顺序的结构化对象：serde_json::json! 按插入顺序序列化，字段值经 JSON 转义，
    // title="a\0goal=b" 与跨字段迁移不会产生相同字节串。
    let payload = serde_json::json!({
        "project_id": row.project_id,
        "title": row.title,
        "goal": row.goal,
        "acceptance": row.acceptance_criteria,
        "priority": row.priority,
        "action": action,
        "source": row.source,
        "external_id": row.external_id.as_deref().unwrap_or(""),
        "external_identifier": row.external_identifier.as_deref().unwrap_or(""),
        "external_url": row.external_url.as_deref().unwrap_or(""),
        "external_state": row.external_state.as_deref().unwrap_or(""),
        "external_labels": external_labels_json.as_deref().unwrap_or(""),
    });
    let encoded = serde_json::to_vec(&payload)?;
    let digest = Sha256::digest(&encoded);
    Ok(format!("{digest:x}"))
}

/// Business Logic（为什么需要这个函数）:
///     幂等命中后必须校验项目与指纹，不能把 project A 的任务返回给 project B 或不同 payload。
///
/// Code Logic（这个函数做什么）:
///     比对映射行的 project_id / request_fingerprint；通过后读取并返回既有 task。
///     调用方负责 commit 事务（本函数不接管 Transaction 所有权）。
pub(crate) async fn resolve_existing_create_request(
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
pub(crate) async fn migrate_remote_task_create_request_scope(
    pool: &SqlitePool,
) -> Result<(), AppError> {
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
pub(crate) async fn ensure_column(
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
pub(crate) async fn backfill_split_state_from_legacy_status(
    pool: &SqlitePool,
) -> Result<(), AppError> {
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
///     旧版 split state 曾把 legacy Queued 映射为 Todo/Queued；升级后 scheduler 只领取 Idle，必须避免这些旧排队任务卡住。
///
/// Code Logic（这个函数做什么）:
///     精准把 status=queued、workflow_state=todo、run_state=queued 的历史行规范化为 Todo/Idle，不覆盖其它用户调整过的 split state。
pub(crate) async fn normalize_queued_split_state(pool: &SqlitePool) -> Result<(), AppError> {
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
pub(crate) fn serialize_external_labels(
    labels: &Option<Vec<String>>,
) -> Result<Option<String>, AppError> {
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
pub(crate) fn deserialize_external_labels(
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
pub(crate) fn row_to_task(row: &SqliteRow) -> Result<OrchestratorTaskRow, AppError> {
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
        agent_session_id: row.try_get("agent_session_id").unwrap_or(None),
        transcript_path: row.try_get("transcript_path")?,
        runtime_started_at: row.try_get("runtime_started_at")?,
        last_activity_at: row.try_get("last_activity_at")?,
        last_runtime_event: row.try_get("last_runtime_event")?,
        last_runtime_message: row.try_get("last_runtime_message")?,
        priority: row.try_get("priority")?,
        branch_name: row.try_get("branch_name")?,
        worktree_id: row.try_get("worktree_id")?,
        session_id: row.try_get("session_id")?,
        prepare_claim_token: row.try_get("prepare_claim_token")?,
        blocked_reason: row.try_get("blocked_reason")?,
        attempt: row.try_get("attempt")?,
        state_version: row.try_get("state_version").unwrap_or(0),
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
pub(crate) fn sqlite_bool(value: i64) -> bool {
    value != 0
}

/// Business Logic（为什么需要这个函数）:
///     legacy 验证命令以 JSON 文本持久化，兼容读取时需要还原为字符串数组。
///
/// Code Logic（这个函数做什么）:
///     使用 serde_json 解析 Vec<String>；解析失败返回 AppError，暴露损坏配置数据。
pub(crate) fn parse_verification_commands(value: &str) -> Result<Vec<String>, AppError> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|err| AppError::generic(format!("Orchestrator 验证命令解析失败: {err}")))
}

/// Business Logic（为什么需要这个函数）:
///     仓储读取 legacy 项目配置时需要统一处理 bool 转换和 verification_commands JSON 解析。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 提取 orchestrator_project_config 全字段并组装 OrchestratorProjectConfigDto。
pub(crate) fn row_to_project_config(
    row: &SqliteRow,
) -> Result<OrchestratorProjectConfigDto, AppError> {
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
pub(crate) fn row_to_evidence(row: &SqliteRow) -> Result<OrchestratorEvidenceDto, AppError> {
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
pub(crate) fn row_to_attempt(row: &SqliteRow) -> Result<OrchestratorTaskAttemptRow, AppError> {
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
pub(crate) fn non_empty_trimmed(value: &str) -> Option<&str> {
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
pub(crate) fn task_row_for_create_action(
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
pub(crate) fn row_to_remote_outbox(
    row: &SqliteRow,
) -> Result<OrchestratorRemoteOutboxRow, AppError> {
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
        state_version: row.try_get("state_version").unwrap_or(0),
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
pub(crate) fn row_to_remote_mirror(row: &SqliteRow) -> Result<RemoteMirrorTask, AppError> {
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
