//! Orchestrator SQLite repository.

#![allow(dead_code)]

use crate::error::AppError;
use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use uuid::Uuid;

const TASK_COLUMNS: &str = "id, project_id, title, goal, acceptance_criteria, status, priority, \
    branch_name, worktree_id, session_id, blocked_reason, attempt, created_at, updated_at, \
    started_at, finished_at";

const ORCHESTRATOR_TASKS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_tasks (
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

const ORCHESTRATOR_PROJECT_CONFIGS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_project_configs (
  project_id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 1,
  max_concurrent_tasks INTEGER NOT NULL DEFAULT 1,
  branch_prefix TEXT NOT NULL DEFAULT 'agent',
  verification_commands_json TEXT NOT NULL DEFAULT '[]',
  auto_commit INTEGER NOT NULL DEFAULT 1,
  auto_push_task_branch INTEGER NOT NULL DEFAULT 1,
  auto_merge_to_main INTEGER NOT NULL DEFAULT 1,
  auto_push_main INTEGER NOT NULL DEFAULT 1,
  retry_limit INTEGER NOT NULL DEFAULT 1,
  retain_worktree_on_done INTEGER NOT NULL DEFAULT 0,
  retain_worktree_on_blocked INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)";

const ORCHESTRATOR_TASK_EVENTS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_task_events (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  event_type TEXT NOT NULL,
  message TEXT NOT NULL,
  payload_json TEXT,
  created_at TEXT NOT NULL
)";

const ORCHESTRATOR_TASK_EVENTS_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_task_events_task \
     ON orchestrator_task_events(task_id, created_at)";

const ORCHESTRATOR_TASK_EVIDENCE_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_task_evidence (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  exit_code INTEGER,
  created_at TEXT NOT NULL
)";

const ORCHESTRATOR_TASK_EVIDENCE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_task_evidence_task \
     ON orchestrator_task_evidence(task_id, created_at)";

/// Orchestrator 仓储，封装任务、事件和证据表访问。
///
/// Business Logic（为什么需要这个结构体）:
///     自动编排器需要把任务队列、状态变化事件和验证证据持久化，供后续调度器与页面共享。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool，并提供任务 CRUD、状态更新、事件和证据追加方法。
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
            ORCHESTRATOR_TASKS_SCHEMA,
            ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX,
            ORCHESTRATOR_TASKS_STATUS_INDEX,
            ORCHESTRATOR_PROJECT_CONFIGS_SCHEMA,
            ORCHESTRATOR_TASK_EVENTS_SCHEMA,
            ORCHESTRATOR_TASK_EVENTS_INDEX,
            ORCHESTRATOR_TASK_EVIDENCE_SCHEMA,
            ORCHESTRATOR_TASK_EVIDENCE_INDEX,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户后续在前端创建编排任务时，需要生成草稿任务并持久化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 UUID 和 UTC 时间戳，以 Draft/default priority/attempt=0 写入任务表并返回完整 Row。
    pub async fn create_task(
        &self,
        project_id: &str,
        title: &str,
        goal: &str,
        acceptance_criteria: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO orchestrator_tasks \
             (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
              worktree_id, session_id, blocked_reason, attempt, created_at, updated_at, \
              started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, NULL, ?, ?, ?, NULL, NULL)",
        )
        .bind(&id)
        .bind(project_id)
        .bind(title)
        .bind(goal)
        .bind(acceptance_criteria)
        .bind(OrchestratorTaskStatus::Draft.as_str())
        .bind(0_i64)
        .bind(0_i64)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.get_task(&id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     任务列表需要按项目和状态筛选，并按调度优先级排序。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id/status 四种筛选组合选择 SQL，结果统一按 priority DESC, created_at ASC 排序。
    pub async fn list_tasks(
        &self,
        project_id: Option<&str>,
        status: Option<OrchestratorTaskStatus>,
    ) -> Result<Vec<OrchestratorTaskRow>, AppError> {
        let rows = match (project_id, status) {
            (Some(project_id), Some(status)) => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     WHERE project_id = ? AND status = ? ORDER BY priority DESC, created_at ASC"
                ))
                .bind(project_id)
                .bind(status.as_str())
                .fetch_all(&self.pool)
                .await?
            }
            (Some(project_id), None) => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     WHERE project_id = ? ORDER BY priority DESC, created_at ASC"
                ))
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            (None, Some(status)) => {
                sqlx::query(&format!(
                    "SELECT {TASK_COLUMNS} FROM orchestrator_tasks \
                     WHERE status = ? ORDER BY priority DESC, created_at ASC"
                ))
                .bind(status.as_str())
                .fetch_all(&self.pool)
                .await?
            }
            (None, None) => {
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
    ///     状态机推进后需要只更新任务状态和阻塞原因，保持任务身份字段不变。
    ///
    /// Code Logic（这个函数做什么）:
    ///     只更新 status、blocked_reason 和 updated_at，再读取完整任务返回。
    pub async fn update_task_status(
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
    ///     后续调度器需要记录任务生命周期事件，便于页面展示和问题排查。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成事件 id 和 UTC 时间戳，向 orchestrator_task_events 追加一行。
    pub async fn add_event(
        &self,
        task_id: &str,
        event_type: &str,
        message: &str,
        payload_json: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO orchestrator_task_events \
             (id, task_id, event_type, message, payload_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(event_type)
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
        content: &str,
        exit_code: Option<i64>,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO orchestrator_task_evidence \
             (id, task_id, kind, title, content, exit_code, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(task_id)
        .bind(kind)
        .bind(title)
        .bind(content)
        .bind(exit_code)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
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
    ///     基础 schema 初始化必须能支撑任务创建和列表读取，这是后续命令层的最低依赖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     初始化内存库后创建一条任务，再按项目查询列表验证表可用。
    #[tokio::test]
    async fn init_schema_creates_tables() {
        let (_pool, repo) = setup_repo().await;

        let created = repo
            .create_task("project-1", "Task A", "Goal A", "Criteria A")
            .await
            .unwrap();
        let listed = repo.list_tasks(Some("project-1"), None).await.unwrap();

        assert_eq!(created.status, OrchestratorTaskStatus::Draft);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, created.id);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度器应优先处理高 priority 任务；同优先级下旧任务先执行，避免饥饿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建两条任务后直接固定 priority/created_at，再断言 list 使用 priority DESC, created_at ASC。
    #[tokio::test]
    async fn create_and_list_task_orders_by_priority_and_created() {
        let (pool, repo) = setup_repo().await;
        let older = repo
            .create_task("project-1", "Older", "Goal", "Criteria")
            .await
            .unwrap();
        let newer = repo
            .create_task("project-1", "Newer", "Goal", "Criteria")
            .await
            .unwrap();
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

        let listed = repo.list_tasks(Some("project-1"), None).await.unwrap();

        assert_eq!(
            listed
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec![newer.id.as_str(), older.id.as_str()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     状态更新不能破坏任务身份，避免调度器推进状态时丢失项目、标题和目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Draft 后更新为 Queued，断言 id/project/title 保持不变且状态改变。
    #[tokio::test]
    async fn update_task_status_preserves_identity() {
        let (_pool, repo) = setup_repo().await;
        let created = repo
            .create_task("project-1", "Task A", "Goal A", "Criteria A")
            .await
            .unwrap();

        let updated = repo
            .update_task_status(&created.id, OrchestratorTaskStatus::Queued, None)
            .await
            .unwrap();

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.project_id, "project-1");
        assert_eq!(updated.title, "Task A");
        assert_eq!(updated.goal, "Goal A");
        assert_eq!(updated.status, OrchestratorTaskStatus::Queued);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度、验证和交付阶段需要把事件和证据持久化，后续详情页才能追溯执行过程。
    ///
    /// Code Logic（这个函数做什么）:
    ///     追加一条 event 与 evidence，再按 task_id 查询对应表确认内容和 exit_code 已落库。
    #[tokio::test]
    async fn events_and_evidence_are_persisted() {
        let (pool, repo) = setup_repo().await;
        let task = repo
            .create_task("project-1", "Task A", "Goal A", "Criteria A")
            .await
            .unwrap();

        repo.add_event(
            &task.id,
            "queued",
            "任务已进入队列",
            Some(r#"{"source":"test"}"#),
        )
        .await
        .unwrap();
        repo.add_evidence(&task.id, "command", "cargo test", "ok", Some(0))
            .await
            .unwrap();

        let events = sqlx::query(
            "SELECT event_type, message, payload_json FROM orchestrator_task_events \
             WHERE task_id = ? ORDER BY created_at ASC",
        )
        .bind(&task.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        let evidence = sqlx::query(
            "SELECT kind, title, content, exit_code FROM orchestrator_task_evidence \
             WHERE task_id = ? ORDER BY created_at ASC",
        )
        .bind(&task.id)
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].try_get::<String, _>("event_type").unwrap(),
            "queued"
        );
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
        assert_eq!(evidence[0].try_get::<String, _>("content").unwrap(), "ok");
        assert_eq!(
            evidence[0].try_get::<Option<i64>, _>("exit_code").unwrap(),
            Some(0)
        );
    }
}
