//! storage/workbench_agent_session_repo — Agent session 运行时持久化
//!
//! Business Logic（为什么需要这个模块）:
//!     owning device 需要在 SQLite 中保存 provider-neutral Agent session 的最小 metadata，
//!     支撑 active 唯一性、CAS version、resume 关系与启动对账；不得落正文/路径/凭据。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_agent_sessions` 表：ensure_schema、create_active、apply_mutation、
//!     end_active_for_terminal、list_active、mark_disconnected、get；写路径经 shared lease。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::workbench::agent_runtime::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// Agent session 表 DDL（文档 + runtime 共用字面量）。
pub const WORKBENCH_AGENT_SESSION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS workbench_agent_sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    terminal_session_id TEXT NOT NULL,
    orchestrator_task_id TEXT,
    orchestrator_attempt INTEGER,
    provider_id TEXT NOT NULL,
    native_session_id TEXT,
    phase TEXT NOT NULL,
    version INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    last_activity_at TEXT NOT NULL,
    ended_at TEXT,
    outcome_code TEXT,
    resumed_from_agent_session_id TEXT,
    is_active INTEGER NOT NULL DEFAULT 1
)";

/// 每个 terminal 至多一个 active Agent session。
pub const WORKBENCH_AGENT_SESSION_ACTIVE_TERMINAL_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_workbench_agent_sessions_active_terminal \
     ON workbench_agent_sessions(terminal_session_id) WHERE is_active = 1";

/// 项目维度活动索引。
pub const WORKBENCH_AGENT_SESSION_PROJECT_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_workbench_agent_sessions_project \
     ON workbench_agent_sessions(project_id, last_activity_at DESC, id)";

/// worktree 维度活动索引。
pub const WORKBENCH_AGENT_SESSION_WORKTREE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_workbench_agent_sessions_worktree \
     ON workbench_agent_sessions(worktree_id, last_activity_at DESC, id)";

/// 全局 last_activity 索引（snapshot 排序）。
pub const WORKBENCH_AGENT_SESSION_ACTIVITY_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_workbench_agent_sessions_activity \
     ON workbench_agent_sessions(last_activity_at DESC, id)";

const SELECT_COLUMNS: &str = "id, project_id, worktree_id, terminal_session_id, \
    orchestrator_task_id, orchestrator_attempt, provider_id, native_session_id, \
    phase, version, started_at, last_activity_at, ended_at, outcome_code, \
    resumed_from_agent_session_id, is_active";

/// Agent session 运行时仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层、reducer、Orchestrator bridge 需要复用同一套 CAS 与 active 唯一语义。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + maintenance gate，提供 CRUD/mutation API。
#[derive(Clone)]
pub struct WorkbenchAgentSessionRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchAgentSessionRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测不需要跨进程 maintenance 锁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     全部 ordinary writer 与 restore 共用同一 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 pool + Arc gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// 返回底层 pool（测试 reopened repo 用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     fixture 需要验证跨实例持久化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone pool。
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// 幂等创建表与索引（含旧库升级）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户库无迁移框架；必须 CREATE IF NOT EXISTS 即可获得 Agent runtime 表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 schema + 四个索引；schema bootstrap 不经 write lease。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(WORKBENCH_AGENT_SESSION_SCHEMA)
            .execute(pool)
            .await?;
        sqlx::query(WORKBENCH_AGENT_SESSION_ACTIVE_TERMINAL_INDEX)
            .execute(pool)
            .await?;
        sqlx::query(WORKBENCH_AGENT_SESSION_PROJECT_INDEX)
            .execute(pool)
            .await?;
        sqlx::query(WORKBENCH_AGENT_SESSION_WORKTREE_INDEX)
            .execute(pool)
            .await?;
        sqlx::query(WORKBENCH_AGENT_SESSION_ACTIVITY_INDEX)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// 创建一条 active Agent session；同一 terminal 已有 active 时冲突。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新 Agent 启动时必须在 owner 落库并成为该 terminal 唯一 active；冲突交给调用方先 end。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT is_active=1 version=1（或调用方 phase）；唯一索引冲突 → `agent_session_conflict`。
    pub async fn create_active(
        &self,
        input: CreateActiveAgentSession,
    ) -> Result<AgentSessionRuntime, AppError> {
        let id = input
            .id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let version: u64 = 1;
        let phase = input.phase;
        if phase.is_terminal() {
            return Err(AppError::validation(
                "create_active 不能以终态 phase 创建".to_string(),
            ));
        }
        let started_at = input.started_at.clone();
        let row = AgentSessionRuntime {
            id: id.clone(),
            project_id: input.project_id,
            worktree_id: input.worktree_id,
            terminal_session_id: input.terminal_session_id,
            orchestrator_task_id: input.orchestrator_task_id,
            orchestrator_attempt: input.orchestrator_attempt,
            provider_id: input.provider_id,
            native_session_id: input.native_session_id,
            phase,
            version,
            started_at: started_at.clone(),
            last_activity_at: started_at,
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: input.resumed_from_agent_session_id,
            is_active: true,
        };

        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query(
                "INSERT INTO workbench_agent_sessions \
                 (id, project_id, worktree_id, terminal_session_id, orchestrator_task_id, \
                  orchestrator_attempt, provider_id, native_session_id, phase, version, \
                  started_at, last_activity_at, ended_at, outcome_code, \
                  resumed_from_agent_session_id, is_active) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, 1)",
            )
            .bind(&row.id)
            .bind(&row.project_id)
            .bind(&row.worktree_id)
            .bind(&row.terminal_session_id)
            .bind(&row.orchestrator_task_id)
            .bind(row.orchestrator_attempt.map(i64::from))
            .bind(&row.provider_id)
            .bind(&row.native_session_id)
            .bind(row.phase.as_str())
            .bind(row.version as i64)
            .bind(&row.started_at)
            .bind(&row.last_activity_at)
            .bind(&row.resumed_from_agent_session_id)
            .execute(&self.pool)
            .await;

            match result {
                Ok(_) => Ok(row),
                Err(sqlx::Error::Database(db_err)) if is_unique_violation(db_err.as_ref()) => {
                    Err(AppError::conflict("agent_session_conflict".to_string()))
                }
                Err(e) => Err(AppError::from(e)),
            }
        })
        .await
    }

    /// CAS 应用 mutation；版本不匹配或 terminal 不一致时返回 false（幂等丢弃）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     OSC/Hook 迟到事件不得覆盖更新的 Agent session；成功后 version 单调前进。
    ///
    /// Code Logic（这个函数做什么）:
    ///     WHERE id + terminal + version=expected；SET phase/version/activity/native；
    ///     终态时 is_active=0 并写 ended_at；affected==0 → Ok(false)。
    pub async fn apply_mutation(
        &self,
        mutation: &AgentRuntimeMutation,
    ) -> Result<bool, AppError> {
        if mutation.event_version <= mutation.expected_version {
            return Ok(false);
        }
        let is_terminal = mutation.phase.is_terminal();
        let is_active: i64 = if is_terminal { 0 } else { 1 };
        let ended_at = if is_terminal {
            Some(mutation.occurred_at.as_str())
        } else {
            None
        };

        with_shared_write_lease(&self.gate, async {
            // native_session_id：Some 时覆盖，None 时保留旧值（COALESCE 式）
            let result = sqlx::query(
                "UPDATE workbench_agent_sessions SET \
                   phase = ?, \
                   version = ?, \
                   last_activity_at = ?, \
                   native_session_id = COALESCE(?, native_session_id), \
                   outcome_code = COALESCE(?, outcome_code), \
                   ended_at = CASE WHEN ? = 0 THEN COALESCE(ended_at, ?) ELSE ended_at END, \
                   is_active = ? \
                 WHERE id = ? AND terminal_session_id = ? AND version = ? AND is_active = 1",
            )
            .bind(mutation.phase.as_str())
            .bind(mutation.event_version as i64)
            .bind(&mutation.occurred_at)
            .bind(&mutation.native_session_id)
            .bind(&mutation.outcome_code)
            .bind(is_active)
            .bind(ended_at)
            .bind(is_active)
            .bind(&mutation.agent_session_id)
            .bind(&mutation.terminal_session_id)
            .bind(mutation.expected_version as i64)
            .execute(&self.pool)
            .await?;

            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// 测试/简化路径：按 id/terminal/expected/new version 做 CAS 升版（保持 phase）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CAS 守卫单测只需验证 version 匹配，不必构造完整 mutation phase 场景。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读当前 phase；委托 apply_mutation；expected 不匹配时 Ok(false)。
    pub async fn apply_version_cas(
        &self,
        agent_session_id: &str,
        terminal_session_id: &str,
        expected_version: u64,
        event_version: u64,
    ) -> Result<bool, AppError> {
        let Some(current) = self.get(agent_session_id).await? else {
            return Ok(false);
        };
        if current.terminal_session_id != terminal_session_id {
            return Ok(false);
        }
        let mutation = AgentRuntimeMutation {
            agent_session_id: agent_session_id.to_string(),
            terminal_session_id: terminal_session_id.to_string(),
            expected_version,
            event_version,
            phase: current.phase,
            native_session_id: None,
            outcome_code: None,
            occurred_at: current.last_activity_at.clone(),
        };
        self.apply_mutation(&mutation).await
    }

    /// 终结某 terminal 上当前 active Agent（若有）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动替换 Agent 或关闭 terminal 前必须先释放 active 唯一槽位。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE is_active=0 + phase/ended_at/version+1；返回更新后的行或 None。
    pub async fn end_active_for_terminal(
        &self,
        terminal_session_id: &str,
        phase: AgentSessionPhase,
        outcome_code: Option<&str>,
        ended_at: &str,
    ) -> Result<Option<AgentSessionRuntime>, AppError> {
        if !phase.is_terminal() {
            return Err(AppError::validation(
                "end_active_for_terminal 要求终态 phase".to_string(),
            ));
        }
        with_shared_write_lease(&self.gate, async {
            let existing = sqlx::query(&format!(
                "SELECT {SELECT_COLUMNS} FROM workbench_agent_sessions \
                 WHERE terminal_session_id = ? AND is_active = 1 LIMIT 1"
            ))
            .bind(terminal_session_id)
            .fetch_optional(&self.pool)
            .await?;
            let Some(row) = existing else {
                return Ok(None);
            };
            let current = row_to_runtime(&row)?;
            let new_version = current.version.saturating_add(1);
            sqlx::query(
                "UPDATE workbench_agent_sessions SET \
                   phase = ?, version = ?, last_activity_at = ?, ended_at = ?, \
                   outcome_code = COALESCE(?, outcome_code), is_active = 0 \
                 WHERE id = ? AND is_active = 1",
            )
            .bind(phase.as_str())
            .bind(new_version as i64)
            .bind(ended_at)
            .bind(ended_at)
            .bind(outcome_code)
            .bind(&current.id)
            .execute(&self.pool)
            .await?;

            self.get(&current.id).await
        })
        .await
    }

    /// 列出 active sessions（可选按 project 过滤），按 last_activity_at DESC, id 稳定排序。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     snapshot 与对账需要有界 active 列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     WHERE is_active=1 [AND project_id=?] ORDER BY last_activity_at DESC, id LIMIT。
    pub async fn list_active(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<AgentSessionRuntime>, AppError> {
        let limit = limit.clamp(0, 10_000);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = if let Some(project_id) = project_id {
            sqlx::query(&format!(
                "SELECT {SELECT_COLUMNS} FROM workbench_agent_sessions \
                 WHERE is_active = 1 AND project_id = ? \
                 ORDER BY last_activity_at DESC, id ASC LIMIT ?"
            ))
            .bind(project_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(&format!(
                "SELECT {SELECT_COLUMNS} FROM workbench_agent_sessions \
                 WHERE is_active = 1 \
                 ORDER BY last_activity_at DESC, id ASC LIMIT ?"
            ))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter().map(row_to_runtime).collect()
    }

    /// 将指定 active session 标为 Disconnected（owner 启动对账）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     terminal 不存在或已 exited 时 active Agent 必须进入 Disconnected，避免幽灵运行态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     终态 phase=Disconnected、is_active=0、version+1；不存在或已非 active → false。
    pub async fn mark_disconnected(
        &self,
        agent_session_id: &str,
        at: &str,
    ) -> Result<bool, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query(
                "UPDATE workbench_agent_sessions SET \
                   phase = 'disconnected', \
                   version = version + 1, \
                   last_activity_at = ?, \
                   ended_at = COALESCE(ended_at, ?), \
                   is_active = 0 \
                 WHERE id = ? AND is_active = 1",
            )
            .bind(at)
            .bind(at)
            .bind(agent_session_id)
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// 按 id 读取完整 runtime 行（含 native_session_id）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     reducer / bridge 需要加载当前 version 与关联 ID。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 全列；不存在返回 None。
    pub async fn get(&self, id: &str) -> Result<Option<AgentSessionRuntime>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {SELECT_COLUMNS} FROM workbench_agent_sessions WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_runtime(&r)).transpose()
    }

    /// 读取某 terminal 当前 active session。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     替换启动与 OSC 归属需要快速定位 terminal 上的 active Agent。
    ///
    /// Code Logic（这个函数做什么）:
    ///     WHERE terminal_session_id AND is_active=1 LIMIT 1。
    pub async fn get_active_for_terminal(
        &self,
        terminal_session_id: &str,
    ) -> Result<Option<AgentSessionRuntime>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {SELECT_COLUMNS} FROM workbench_agent_sessions \
             WHERE terminal_session_id = ? AND is_active = 1 LIMIT 1"
        ))
        .bind(terminal_session_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_runtime(&r)).transpose()
    }
}

/// 将 sqlx 行映射为 AgentSessionRuntime。
///
/// Business Logic（为什么需要这个函数）:
///     所有 SELECT 路径需要统一解析 phase token 与整数 version。
///
/// Code Logic（这个函数做什么）:
///     try_get 各列；非法 phase → Validation。
fn row_to_runtime(row: &SqliteRow) -> Result<AgentSessionRuntime, AppError> {
    let phase_raw: String = row.try_get("phase")?;
    let phase = AgentSessionPhase::parse(&phase_raw).ok_or_else(|| {
        AppError::validation(format!("unknown agent session phase: {phase_raw}"))
    })?;
    let version: i64 = row.try_get("version")?;
    let attempt: Option<i64> = row.try_get("orchestrator_attempt")?;
    let is_active: i64 = row.try_get("is_active")?;
    Ok(AgentSessionRuntime {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        worktree_id: row.try_get("worktree_id")?,
        terminal_session_id: row.try_get("terminal_session_id")?,
        orchestrator_task_id: row.try_get("orchestrator_task_id")?,
        orchestrator_attempt: attempt.map(|v| v.clamp(0, i64::from(u32::MAX)) as u32),
        provider_id: row.try_get("provider_id")?,
        native_session_id: row.try_get("native_session_id")?,
        phase,
        version: version.max(0) as u64,
        started_at: row.try_get("started_at")?,
        last_activity_at: row.try_get("last_activity_at")?,
        ended_at: row.try_get("ended_at")?,
        outcome_code: row.try_get("outcome_code")?,
        resumed_from_agent_session_id: row.try_get("resumed_from_agent_session_id")?,
        is_active: is_active != 0,
    })
}

/// 判断 SQLite 唯一约束冲突。
///
/// Business Logic（为什么需要这个函数）:
///     active-terminal 唯一索引冲突必须映射为稳定 `agent_session_conflict`，不能当 500。
///
/// Code Logic（这个函数做什么）:
///     检查 database error code 为 UNIQUE / constraint failed 文案。
fn is_unique_violation(err: &dyn sqlx::error::DatabaseError) -> bool {
    if let Some(code) = err.code() {
        // SQLite SQLITE_CONSTRAINT = 19；部分驱动给扩展码 2067 (UNIQUE)
        if code == "2067" || code == "1555" || code == "19" {
            return true;
        }
    }
    let msg = err.message();
    msg.contains("UNIQUE") || msg.contains("unique")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存库 + schema 的 repo。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仓库测试必须隔离，避免污染用户库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     sqlite::memory: → ensure_schema → WorkbenchAgentSessionRepo::new。
    async fn fixture_repo() -> WorkbenchAgentSessionRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        WorkbenchAgentSessionRepo::ensure_schema(&pool).await.unwrap();
        WorkbenchAgentSessionRepo::new(pool)
    }

    /// 构造 create_active 输入。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多测例共享最小合法字段，突出 terminal/provider 差异。
    ///
    /// Code Logic（这个函数做什么）:
    ///     固定 project/time，填充 terminal + provider。
    fn fixture_create(terminal: &str, provider: &str) -> CreateActiveAgentSession {
        CreateActiveAgentSession {
            id: None,
            project_id: "project-1".to_string(),
            worktree_id: Some("wt-1".to_string()),
            terminal_session_id: terminal.to_string(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: provider.to_string(),
            native_session_id: None,
            phase: AgentSessionPhase::Launching,
            started_at: "2026-07-15T00:00:00Z".to_string(),
            resumed_from_agent_session_id: None,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一 terminal 不得并存两个 active Agent；CAS 错误 version 必须拒绝写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     create_active 两次同 terminal → 第二次 conflict；apply_version_cas 用错误 expected → false。
    #[tokio::test]
    async fn active_agent_is_unique_per_terminal_and_version_is_cas_guarded() {
        let repo = fixture_repo().await;
        let first = repo
            .create_active(fixture_create("terminal-1", "claudeCodeVisible"))
            .await
            .unwrap();
        let err = repo
            .create_active(fixture_create("terminal-1", "codexVisible"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "agent_session_conflict");
        // expected_version 故意用 first.version+1，CAS 失败
        assert!(
            !repo
                .apply_version_cas(
                    &first.id,
                    "terminal-1",
                    first.version + 1,
                    first.version + 2,
                )
                .await
                .unwrap()
        );
        // 正确 CAS：expected=first.version，event=first.version+1
        assert!(
            repo.apply_version_cas(&first.id, "terminal-1", first.version, first.version + 1)
                .await
                .unwrap()
        );
        let updated = repo.get(&first.id).await.unwrap().unwrap();
        assert_eq!(updated.version, first.version + 1);
        assert!(updated.is_active);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     替换 Agent 前 end_active 必须释放槽位，新 create 才能成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     create → end Completed → 再 create 另一 provider → 仅后者 active。
    #[tokio::test]
    async fn end_active_releases_terminal_slot_for_replacement() {
        let repo = fixture_repo().await;
        let first = repo
            .create_active(fixture_create("terminal-1", "claudeCodeVisible"))
            .await
            .unwrap();
        let ended = repo
            .end_active_for_terminal(
                "terminal-1",
                AgentSessionPhase::Completed,
                Some("ok"),
                "2026-07-15T00:01:00Z",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ended.id, first.id);
        assert!(!ended.is_active);
        assert_eq!(ended.phase, AgentSessionPhase::Completed);

        let second = repo
            .create_active(fixture_create("terminal-1", "codexVisible"))
            .await
            .unwrap();
        assert_ne!(second.id, first.id);
        assert!(second.is_active);
        let active = repo
            .get_active_for_terminal("terminal-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.id, second.id);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 对账发现 terminal 丢失时必须 mark_disconnected。
    ///
    /// Code Logic（这个测试做什么）:
    ///     create → mark_disconnected → is_active false + phase Disconnected。
    #[tokio::test]
    async fn mark_disconnected_clears_active_flag() {
        let repo = fixture_repo().await;
        let first = repo
            .create_active(fixture_create("terminal-2", "claudeCodeVisible"))
            .await
            .unwrap();
        assert!(repo
            .mark_disconnected(&first.id, "2026-07-15T00:02:00Z")
            .await
            .unwrap());
        let row = repo.get(&first.id).await.unwrap().unwrap();
        assert!(!row.is_active);
        assert_eq!(row.phase, AgentSessionPhase::Disconnected);
        assert_eq!(repo.list_active(None, 100).await.unwrap().len(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     list_active 必须支持 project 过滤与 activity 排序，供 snapshot 使用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两项目各建 active，list_active(Some(p1)) 只返回 p1。
    #[tokio::test]
    async fn list_active_filters_by_project() {
        let repo = fixture_repo().await;
        let mut a = fixture_create("t-a", "claudeCodeVisible");
        a.project_id = "p1".to_string();
        a.started_at = "2026-07-15T00:00:02Z".to_string();
        let mut b = fixture_create("t-b", "claudeCodeVisible");
        b.project_id = "p2".to_string();
        b.started_at = "2026-07-15T00:00:03Z".to_string();
        repo.create_active(a).await.unwrap();
        repo.create_active(b).await.unwrap();
        let listed = repo.list_active(Some("p1"), 100).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].project_id, "p1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     mutation 终态应同时清除 active，释放 terminal 槽位。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply_mutation phase=Failed → is_active false，可再 create_active。
    #[tokio::test]
    async fn apply_mutation_terminal_phase_deactivates() {
        let repo = fixture_repo().await;
        let first = repo
            .create_active(fixture_create("terminal-3", "claudeCodeVisible"))
            .await
            .unwrap();
        let ok = repo
            .apply_mutation(&AgentRuntimeMutation {
                agent_session_id: first.id.clone(),
                terminal_session_id: "terminal-3".to_string(),
                expected_version: 1,
                event_version: 2,
                phase: AgentSessionPhase::Failed,
                native_session_id: Some("native-xyz".to_string()),
                outcome_code: Some("adapter_failed".to_string()),
                occurred_at: "2026-07-15T00:03:00Z".to_string(),
            })
            .await
            .unwrap();
        assert!(ok);
        let row = repo.get(&first.id).await.unwrap().unwrap();
        assert!(!row.is_active);
        assert_eq!(row.phase, AgentSessionPhase::Failed);
        assert_eq!(row.native_session_id.as_deref(), Some("native-xyz"));
        assert_eq!(row.outcome_code.as_deref(), Some("adapter_failed"));
        // 槽位已释放
        repo.create_active(fixture_create("terminal-3", "other"))
            .await
            .unwrap();
    }
}
