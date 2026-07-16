//! workbench/agent_runtime/reducer — owner 侧 Agent session mutation 串行归约
//!
//! Business Logic（为什么需要这个模块）:
//!     OSC/Hook/Orchestrator 事件必须在 owning device 上串行应用 CAS；迟到事件、
//!     terminal 错配、owner 重启后的幽灵 active 不得覆盖新 session。
//!
//! Code Logic（这个模块做什么）:
//!     `AgentRuntimeReducer` 持有 repo，提供 apply / start_or_replace_active /
//!     reconcile_active_sessions；不解析 terminal bytes 或 transcript。

use super::models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
use crate::error::AppError;
use crate::storage::WorkbenchAgentSessionRepo;
use std::collections::HashSet;

/// mutation 应用结果。
///
/// Business Logic（为什么需要这个类型）:
///     调用方需要区分已写入 / 幂等丢弃 / 拒绝，以便 metrics 与测试断言。
///
/// Code Logic（这个类型做什么）:
///     Applied 携带更新后行；Ignored 带稳定 reason token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentReduceOutcome {
    /// 已持久化
    Applied(AgentSessionRuntime),
    /// 幂等或策略拒绝（非错误）
    Ignored(&'static str),
}

/// Agent session 运行时 reducer（owner-local）。
///
/// Business Logic（为什么需要这个结构体）:
///     所有 mutation 必须经单一逻辑入口，保证 version 单调与 active 唯一。
///
/// Code Logic（这个结构体做什么）:
///     包装 `WorkbenchAgentSessionRepo` 的高层语义；可被 worker 循环或同步测试调用。
#[derive(Clone)]
pub struct AgentRuntimeReducer {
    repo: WorkbenchAgentSessionRepo,
}

impl AgentRuntimeReducer {
    /// 构造 reducer。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState / 测试 fixture 需要注入共享 repo。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 repo clone。
    pub fn new(repo: WorkbenchAgentSessionRepo) -> Self {
        Self { repo }
    }

    /// 访问底层 repo（bridge / snapshot 复用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator bridge 与 snapshot 需要同一权威存储。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 repo 引用。
    pub fn repo(&self) -> &WorkbenchAgentSessionRepo {
        &self.repo
    }

    /// 应用一条 OSC/Hook mutation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     迟到 event、错误 terminal、已替换 session 不得写坏当前 active。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) get by agent id；缺失 → Ignored；
    ///     2) terminal 不一致 → Ignored；
    ///     3) event_version <= current.version → Ignored（stale）；
    ///     4) 非 active 且非同 id 路径 → Ignored；
    ///     5) 用 current.version 作 expected 重写 mutation 再 CAS；
    ///     6) 成功返回 Applied。
    pub async fn apply(
        &self,
        mutation: AgentRuntimeMutation,
    ) -> Result<AgentReduceOutcome, AppError> {
        let Some(current) = self.repo.get(&mutation.agent_session_id).await? else {
            return Ok(AgentReduceOutcome::Ignored("agent_not_found"));
        };
        if current.terminal_session_id != mutation.terminal_session_id {
            return Ok(AgentReduceOutcome::Ignored("terminal_mismatch"));
        }
        if !current.is_active {
            // 已终结的 session 拒绝复活（迟到 Working 不得覆盖新 active）
            return Ok(AgentReduceOutcome::Ignored("session_not_active"));
        }
        if mutation.event_version <= current.version {
            return Ok(AgentReduceOutcome::Ignored("stale_version"));
        }
        let mut normalized = mutation;
        normalized.expected_version = current.version;
        let applied = self.repo.apply_mutation(&normalized).await?;
        if !applied {
            return Ok(AgentReduceOutcome::Ignored("cas_rejected"));
        }
        let updated = self
            .repo
            .get(&normalized.agent_session_id)
            .await?
            .ok_or_else(|| AppError::generic("agent session missing after apply"))?;
        Ok(AgentReduceOutcome::Applied(updated))
    }

    /// 在 terminal 上启动新 active Agent；若已有 active 则先终结为 Disconnected。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同一 terminal 任一时刻最多一个 active；替换必须先 end 旧 row。
    ///
    /// Code Logic（这个函数做什么）:
    ///     end_active_for_terminal(Disconnected) → create_active。
    pub async fn start_or_replace_active(
        &self,
        input: CreateActiveAgentSession,
    ) -> Result<AgentSessionRuntime, AppError> {
        let at = input.started_at.clone();
        let _ = self
            .repo
            .end_active_for_terminal(
                &input.terminal_session_id,
                AgentSessionPhase::Disconnected,
                Some("replaced"),
                &at,
            )
            .await?;
        self.repo.create_active(input).await
    }

    /// owner 启动时对账：active row 的 terminal 不在存活集合则 mark_disconnected。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     进程重启后内存 terminal 丢失，SQLite 中幽灵 active 必须变为 Disconnected。
    ///
    /// Code Logic（这个函数做什么）:
    ///     list_active 全量（上限 10_000）；terminal 不在 alive 集合则 mark_disconnected；
    ///     返回断开数量。
    pub async fn reconcile_active_sessions(
        &self,
        alive_terminal_ids: &HashSet<String>,
        at: &str,
    ) -> Result<u32, AppError> {
        let active = self.repo.list_active(None, 10_000).await?;
        let mut disconnected = 0u32;
        for row in active {
            if !alive_terminal_ids.contains(&row.terminal_session_id) {
                if self.repo.mark_disconnected(&row.id, at).await? {
                    disconnected = disconnected.saturating_add(1);
                }
            }
        }
        Ok(disconnected)
    }

    /// 读取 terminal 当前 active session。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试与 bridge 需要查询某 terminal 权威 active。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 repo.get_active_for_terminal。
    pub async fn active_for_terminal(
        &self,
        terminal_session_id: &str,
    ) -> Result<Option<AgentSessionRuntime>, AppError> {
        self.repo.get_active_for_terminal(terminal_session_id).await
    }
}

/// 从 AppState 终端 registry + session repo 收集“仍存活”的 terminal id。
///
/// Business Logic（为什么需要这个函数）:
///     reconcile 需要知道哪些 terminal 仍 running（内存或持久 status）。
///
/// Code Logic（这个函数做什么）:
///     合并 in-memory registry session ids 与 DB 中 status=running 的 id。
pub async fn collect_alive_terminal_ids(
    registry_ids: impl IntoIterator<Item = String>,
    session_repo: &crate::storage::WorkbenchSessionRepo,
) -> Result<HashSet<String>, AppError> {
    let mut set: HashSet<String> = registry_ids.into_iter().collect();
    // 持久化 running 会话在 restore 前也可能算“应存活”；exited 不算。
    let rows = session_repo.list(None).await?;
    for row in rows {
        if row.status == "running" {
            set.insert(row.id);
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 内存 reducer fixture。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     归约语义测试不依赖完整 AppState。
    ///
    /// Code Logic（这个函数做什么）:
    ///     memory SQLite + ensure_schema + AgentRuntimeReducer。
    async fn fixture_reducer() -> AgentRuntimeReducer {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        WorkbenchAgentSessionRepo::ensure_schema(&pool)
            .await
            .unwrap();
        AgentRuntimeReducer::new(WorkbenchAgentSessionRepo::new(pool))
    }

    /// 构造 create 输入。
    fn create_input(terminal: &str, provider: &str, at: &str) -> CreateActiveAgentSession {
        CreateActiveAgentSession {
            id: None,
            project_id: "p1".to_string(),
            worktree_id: None,
            terminal_session_id: terminal.to_string(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: provider.to_string(),
            native_session_id: None,
            phase: AgentSessionPhase::Launching,
            started_at: at.to_string(),
            resumed_from_agent_session_id: None,
        }
    }

    /// 构造 mutation。
    fn event(
        agent: &AgentSessionRuntime,
        version: u64,
        phase: AgentSessionPhase,
    ) -> AgentRuntimeMutation {
        AgentRuntimeMutation {
            agent_session_id: agent.id.clone(),
            terminal_session_id: agent.terminal_session_id.clone(),
            expected_version: version.saturating_sub(1),
            event_version: version,
            phase,
            native_session_id: None,
            outcome_code: None,
            occurred_at: "2026-07-15T01:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 Agent 被替换后，迟到 Working 事件不得复活旧 session 或顶替新 active。
    ///
    /// Code Logic（这个测试做什么）:
    ///     start old → replace new → reduce old@v2 Working → active 仍是 new。
    #[tokio::test]
    async fn stale_event_cannot_replace_new_active_agent() {
        let reducer = fixture_reducer().await;
        let old = reducer
            .start_or_replace_active(create_input(
                "terminal-1",
                "claudeCodeVisible",
                "2026-07-15T00:00:00Z",
            ))
            .await
            .unwrap();
        // 升到 v1 后替换
        let _ = reducer
            .apply(event(&old, 2, AgentSessionPhase::Working))
            .await
            .unwrap();
        let new = reducer
            .start_or_replace_active(create_input(
                "terminal-1",
                "codexVisible",
                "2026-07-15T00:01:00Z",
            ))
            .await
            .unwrap();
        let outcome = reducer
            .apply(event(&old, 3, AgentSessionPhase::Working))
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            AgentReduceOutcome::Ignored("session_not_active")
        ));
        let active = reducer
            .active_for_terminal("terminal-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.id, new.id);
        assert_eq!(active.provider_id, "codexVisible");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     event version 非递增时必须幂等丢弃。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply v2 Working 两次；第二次 Ignored stale_version。
    #[tokio::test]
    async fn non_increasing_version_is_ignored() {
        let reducer = fixture_reducer().await;
        let agent = reducer
            .start_or_replace_active(create_input("t2", "p", "2026-07-15T00:00:00Z"))
            .await
            .unwrap();
        assert!(matches!(
            reducer
                .apply(event(&agent, 2, AgentSessionPhase::Working))
                .await
                .unwrap(),
            AgentReduceOutcome::Applied(_)
        ));
        assert!(matches!(
            reducer
                .apply(event(&agent, 2, AgentSessionPhase::Idle))
                .await
                .unwrap(),
            AgentReduceOutcome::Ignored("stale_version")
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     terminal id 错配的 mutation 不得写库。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mutation.terminal 改写后 apply → terminal_mismatch。
    #[tokio::test]
    async fn mismatched_terminal_id_is_rejected() {
        let reducer = fixture_reducer().await;
        let agent = reducer
            .start_or_replace_active(create_input("t3", "p", "2026-07-15T00:00:00Z"))
            .await
            .unwrap();
        let mut bad = event(&agent, 2, AgentSessionPhase::Working);
        bad.terminal_session_id = "other-terminal".to_string();
        assert!(matches!(
            reducer.apply(bad).await.unwrap(),
            AgentReduceOutcome::Ignored("terminal_mismatch")
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 启动对账必须把丢失 terminal 的 active 标 Disconnected。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两 active；alive 仅含一个；reconcile 后另一个 disconnected。
    #[tokio::test]
    async fn reconcile_marks_missing_terminals_disconnected() {
        let reducer = fixture_reducer().await;
        let a = reducer
            .start_or_replace_active(create_input("alive", "p", "2026-07-15T00:00:00Z"))
            .await
            .unwrap();
        let b = reducer
            .start_or_replace_active(create_input("gone", "p", "2026-07-15T00:00:01Z"))
            .await
            .unwrap();
        let mut alive = HashSet::new();
        alive.insert("alive".to_string());
        let n = reducer
            .reconcile_active_sessions(&alive, "2026-07-15T00:02:00Z")
            .await
            .unwrap();
        assert_eq!(n, 1);
        let a2 = reducer.repo().get(&a.id).await.unwrap().unwrap();
        let b2 = reducer.repo().get(&b.id).await.unwrap().unwrap();
        assert!(a2.is_active);
        assert!(!b2.is_active);
        assert_eq!(b2.phase, AgentSessionPhase::Disconnected);
    }
}
