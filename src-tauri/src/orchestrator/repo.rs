//! Orchestrator SQLite repository.

#![allow(dead_code)]

use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorTaskAttemptRow,
    OrchestratorTaskRow, OrchestratorTaskStatus,
};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use uuid::Uuid;

const TASK_COLUMNS: &str = "id, project_id, title, goal, acceptance_criteria, status, priority, \
    branch_name, worktree_id, session_id, blocked_reason, attempt, created_at, updated_at, \
    started_at, finished_at";
const PROJECT_CONFIG_COLUMNS: &str = "project_id, enabled, max_concurrent_tasks, branch_prefix, \
    verification_commands_json, auto_commit, auto_push_task_branch, auto_merge_to_main, \
    auto_push_main, retry_limit, retain_worktree_on_done, retain_worktree_on_blocked, \
    created_at, updated_at";
const EVIDENCE_COLUMNS: &str = "id, task_id, kind, title, summary, content, created_at";
const ATTEMPT_COLUMNS: &str = "id, task_id, attempt, worktree_id, session_id, prompt, status, \
    created_at, completed_at";

pub const ORCHESTRATOR_TASK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_tasks (
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

/// Orchestrator 仓储，封装任务、事件和证据表访问。
///
/// Business Logic（为什么需要这个结构体）:
///     自动编排器需要把任务队列、状态变化事件和验证证据持久化，供后续调度器与页面共享。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool，并提供任务 CRUD、项目策略读取、状态更新、事件和证据追加方法。
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
        for statement in [
            ORCHESTRATOR_TASK_SCHEMA,
            ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX,
            ORCHESTRATOR_TASKS_STATUS_INDEX,
            ORCHESTRATOR_PROJECT_CONFIG_SCHEMA,
            ORCHESTRATOR_EVENT_SCHEMA,
            ORCHESTRATOR_TASK_EVENTS_INDEX,
            ORCHESTRATOR_EVIDENCE_SCHEMA,
            ORCHESTRATOR_TASK_EVIDENCE_INDEX,
            ORCHESTRATOR_TASK_ATTEMPT_SCHEMA,
            ORCHESTRATOR_TASK_ATTEMPTS_SESSION_INDEX,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层会先完成校验、生成 id/时间戳并构造 Row，仓储只负责按 Row 持久化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将调用方传入的 OrchestratorTaskRow 全字段插入 orchestrator_tasks，不改写业务字段。
    pub async fn create_task(&self, row: &OrchestratorTaskRow) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
              worktree_id, session_id, blocked_reason, attempt, created_at, updated_at, \
              started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.project_id)
        .bind(&row.title)
        .bind(&row.goal)
        .bind(&row.acceptance_criteria)
        .bind(row.status.as_str())
        .bind(row.priority)
        .bind(&row.branch_name)
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
    ///     legacy 项目策略仍用于页面展示和调试历史数据；新项目第一次进入时应自动拥有一份默认策略。
    ///     Phase 3 后 scheduler、验证和 delivery 运行时不再读取该表。
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
    ///     legacy 项目策略列表仅用于展示/调试旧项目配置；运行时调度已改读 AppConfig.orchestrator。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 enabled=1 的项目策略，按 project_id 稳定排序后转换为 OrchestratorProjectConfigDto。
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
    ///     统计 Preparing/Running/Verifying/Delivering 四个 active 状态数量，返回 SQLite COUNT 结果。
    pub async fn count_active_tasks(&self, project_id: &str) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks \
             WHERE project_id = ? AND status IN (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 的调度容量属于本设备全局配置，必须统计所有本机 local Workbench 项目的执行中任务。
    ///     远端项目只是快捷方式，不能占用或触发本机 Runner 容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     通过 workbench_projects join 过滤 kind='local'，统计 Preparing/Running/Verifying/Delivering 四个 active 状态。
    pub async fn count_active_local_tasks(&self) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_tasks task \
             INNER JOIN workbench_projects project ON project.id = task.project_id \
             WHERE project.kind = 'local' AND task.status IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
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
        let result = sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Preparing.as_str())
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
    ///     全局 scheduler 一次 tick 需要按设备级剩余容量，在所有本机 local Workbench 项目中领取 queued 任务。
    ///     remote 项目必须跳过，避免本机在远端快捷方式上创建 worktree/terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在单个事务内完成 active local 计数、剩余容量计算、local queued 选择和 Queued→Preparing 条件更新。
    ///     最多领取 limit-active 个任务，排序保持 priority DESC, created_at ASC；limit<=0 或容量已满返回空 Vec。
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
             WHERE project.kind = 'local' AND task.status IN (?, ?, ?, ?)",
        )
        .bind(OrchestratorTaskStatus::Preparing.as_str())
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(OrchestratorTaskStatus::Verifying.as_str())
        .bind(OrchestratorTaskStatus::Delivering.as_str())
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
               WHERE project.kind = 'local' AND task.status = ? \
               ORDER BY task.priority DESC, task.created_at ASC LIMIT ?\
             ) \
             ORDER BY priority DESC, created_at ASC"
        ))
        .bind(OrchestratorTaskStatus::Queued.as_str())
        .bind(remaining)
        .fetch_all(&mut *tx)
        .await?;

        let mut claimed = Vec::new();
        for row in selected {
            let task_id: String = row.try_get("id")?;
            let now = Utc::now().to_rfc3339();
            let result = sqlx::query(
                "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
                 WHERE id = ? AND status = ?",
            )
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(Option::<&str>::None)
            .bind(now)
            .bind(&task_id)
            .bind(OrchestratorTaskStatus::Queued.as_str())
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
    ///     按 task_id+attempt 更新 status=completed 和 completed_at；找不到记录时返回 not_found。
    pub async fn mark_attempt_completed(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = sqlx::query(
            "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
             WHERE task_id = ? AND attempt = ?",
        )
        .bind("completed")
        .bind(&now)
        .bind(task_id)
        .bind(attempt)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 任务尝试不存在: {task_id}#{attempt}"
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
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, branch_name = ?, worktree_id = ?, session_id = ?, \
                 blocked_reason = ?, started_at = COALESCE(started_at, ?), updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(branch_name)
        .bind(worktree_id)
        .bind(session_id)
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
        sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ?",
        )
        .bind(status.as_str())
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
        sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ?, finished_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Done.as_str())
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
        sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Blocked.as_str())
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
        sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Blocked.as_str())
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
        let result = sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(next_status.as_str())
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
        let result = sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(next_status.as_str())
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
        let result = sqlx::query(
            "UPDATE orchestrator_tasks SET status = ?, blocked_reason = ?, updated_at = ? \
             WHERE id = ? AND status = ?",
        )
        .bind(OrchestratorTaskStatus::Queued.as_str())
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
}

/// Business Logic（为什么需要这个函数）:
///     多个查询都需要把 SQLite 行转换成任务 Row，统一解析可避免字段遗漏和状态字符串散落。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取 orchestrator_tasks 全字段，并把 status 字符串解析为枚举。
fn row_to_task(row: &SqliteRow) -> Result<OrchestratorTaskRow, AppError> {
    let status_text: String = row.try_get("status")?;
    Ok(OrchestratorTaskRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        goal: row.try_get("goal")?,
        acceptance_criteria: row.try_get("acceptance_criteria")?,
        status: OrchestratorTaskStatus::from_str(&status_text)?,
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
///     项目策略表用 INTEGER 0/1 保存布尔值，前端 DTO 需要真实 boolean 以便直接展示开关状态。
///
/// Code Logic（这个函数做什么）:
///     将 SQLite INTEGER 按非零即 true 的规则转换为 bool。
fn sqlite_bool(value: i64) -> bool {
    value != 0
}

/// Business Logic（为什么需要这个函数）:
///     验证命令以 JSON 文本持久化，读取策略时需要还原为字符串数组供前端逐条展示。
///
/// Code Logic（这个函数做什么）:
///     使用 serde_json 解析 Vec<String>；解析失败返回 AppError，暴露损坏配置数据。
fn parse_verification_commands(value: &str) -> Result<Vec<String>, AppError> {
    serde_json::from_str::<Vec<String>>(value)
        .map_err(|err| AppError::generic(format!("Orchestrator 验证命令解析失败: {err}")))
}

/// Business Logic（为什么需要这个函数）:
///     仓储读取项目策略时需要统一处理 bool 转换和 verification_commands JSON 解析。
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
        }
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
    ///     项目策略默认应提供完整自动化开关组合，但必须保持 disabled，避免用户未确认前自动执行任务。
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
            .all(|task| task.status == OrchestratorTaskStatus::Preparing));
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
}
