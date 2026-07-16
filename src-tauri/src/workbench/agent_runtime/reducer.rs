//! workbench/agent_runtime/reducer — owner 侧 Agent session mutation 串行归约
//!
//! Business Logic（为什么需要这个模块）:
//!     OSC/Hook/Orchestrator 事件必须在 owning device 上串行应用 CAS；迟到事件、
//!     terminal 错配、owner 重启后的幽灵 active 不得覆盖新 session。
//!
//! Code Logic（这个模块做什么）:
//!     `AgentRuntimeReducer` 持有 repo，提供 apply / start_or_replace_active /
//!     reconcile_active_sessions；所有写路径经进程内 async mutex 与 OSC worker 串行。

use super::models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
use crate::error::AppError;
use crate::storage::WorkbenchAgentSessionRepo;
use std::collections::HashSet;
use std::sync::OnceLock;
use tokio::sync::Mutex;

/// 进程内 Agent runtime 写串行锁（OSC worker 与 Orchestrator bridge 共用）。
///
/// Business Logic（为什么需要这个函数）:
///     设计要求单一写入口；bridge 若直写 repo 会与 OSC CAS 竞态导致 completion 丢标记。
///
/// Code Logic（这个函数做什么）:
///     返回 OnceLock 初始化的 tokio Mutex；所有 reducer 公开 mutation 方法在 await 前 acquire。
fn agent_runtime_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

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

/// `start_or_replace_active` 结果：可选被替换的旧行 + 新 active。
///
/// Business Logic（为什么需要这个类型）:
///     替换时旧 Agent 进入 Disconnected 也是 durable phase 变化，调用方必须 emit 投影。
///
/// Code Logic（这个类型做什么）:
///     ended = 被 end 的旧 active（若有）；active = 新创建行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOrReplaceOutcome {
    /// 被替换并终结的旧 session（若 terminal 上曾有 active）
    pub ended: Option<AgentSessionRuntime>,
    /// 新 active session
    pub active: AgentSessionRuntime,
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
    ///     获取写锁后：get → terminal/active/version 校验 → CAS apply → Applied/Ignored。
    pub async fn apply(
        &self,
        mutation: AgentRuntimeMutation,
    ) -> Result<AgentReduceOutcome, AppError> {
        let _guard = agent_runtime_write_lock().lock().await;
        self.apply_unlocked(mutation).await
    }

    /// 无锁 apply（调用方已持有写锁时使用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mark_completed 等复合路径需要在同一临界区内读-改-写，避免中途释放锁被 OSC 抢占。
    ///
    /// Code Logic（这个函数做什么）:
    ///     与 `apply` 相同归约语义，不 acquire 锁。
    async fn apply_unlocked(
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
    ///     写锁内 end_active_for_terminal(Disconnected) → create_active；返回 ended+active。
    pub async fn start_or_replace_active(
        &self,
        input: CreateActiveAgentSession,
    ) -> Result<StartOrReplaceOutcome, AppError> {
        let _guard = agent_runtime_write_lock().lock().await;
        let at = input.started_at.clone();
        let ended = self
            .repo
            .end_active_for_terminal(
                &input.terminal_session_id,
                AgentSessionPhase::Disconnected,
                Some("replaced"),
                &at,
            )
            .await?;
        let active = self.repo.create_active(input).await?;
        Ok(StartOrReplaceOutcome { ended, active })
    }

    /// 将 Agent 标为 Completed（completion 路径，优先 by id，CAS 失败则 end_active）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Spec 要求进入 Verifying 前 Agent 必须 durable Completed；不得在 CAS 失败后返回仍 Working 的行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁内：对 id 最多 3 次读 version+CAS Completed；仍失败则 end_active_for_terminal；
    ///     若目标仍 active Working → 错误。
    pub async fn mark_completed(
        &self,
        agent_session_id: Option<&str>,
        terminal_session_id: &str,
        at: &str,
    ) -> Result<Option<AgentSessionRuntime>, AppError> {
        let _guard = agent_runtime_write_lock().lock().await;
        const MAX_CAS_RETRIES: u32 = 3;

        if let Some(id) = agent_session_id {
            for _ in 0..MAX_CAS_RETRIES {
                let Some(current) = self.repo.get(id).await? else {
                    break;
                };
                if !current.is_active {
                    if current.phase == AgentSessionPhase::Completed {
                        return Ok(Some(current));
                    }
                    // 已非 active 但非 Completed（例如 Disconnected）——仍尝试 terminal end 对齐
                    break;
                }
                if current.terminal_session_id != terminal_session_id {
                    break;
                }
                let mutation = AgentRuntimeMutation {
                    agent_session_id: id.to_string(),
                    terminal_session_id: terminal_session_id.to_string(),
                    expected_version: current.version,
                    event_version: current.version.saturating_add(1),
                    phase: AgentSessionPhase::Completed,
                    native_session_id: None,
                    outcome_code: Some("dev_done".to_string()),
                    occurred_at: at.to_string(),
                };
                if self.repo.apply_mutation(&mutation).await? {
                    let updated = self
                        .repo
                        .get(id)
                        .await?
                        .ok_or_else(|| AppError::generic("agent session missing after complete"))?;
                    if updated.phase == AgentSessionPhase::Completed && !updated.is_active {
                        return Ok(Some(updated));
                    }
                }
                // CAS 失败：循环用新鲜 version 重试
            }
        }

        let ended = self
            .repo
            .end_active_for_terminal(
                terminal_session_id,
                AgentSessionPhase::Completed,
                Some("dev_done"),
                at,
            )
            .await?;

        if let Some(id) = agent_session_id {
            if let Some(row) = self.repo.get(id).await? {
                if row.is_active && !row.phase.is_terminal() {
                    // end_active 未命中该 id（terminal 错配等）：强制 mark 终态
                    let force = AgentRuntimeMutation {
                        agent_session_id: id.to_string(),
                        terminal_session_id: row.terminal_session_id.clone(),
                        expected_version: row.version,
                        event_version: row.version.saturating_add(1),
                        phase: AgentSessionPhase::Completed,
                        native_session_id: None,
                        outcome_code: Some("dev_done".to_string()),
                        occurred_at: at.to_string(),
                    };
                    if !self.repo.apply_mutation(&force).await? {
                        return Err(AppError::generic(format!(
                            "agent completion mark failed after retries: {id}"
                        )));
                    }
                    return self.repo.get(id).await;
                }
                if row.phase == AgentSessionPhase::Completed {
                    return Ok(Some(row));
                }
            }
        }

        Ok(ended)
    }

    /// owner 启动时对账：active row 的 terminal 不在存活集合则 mark_disconnected。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     进程重启后内存 terminal 丢失，SQLite 中幽灵 active 必须变为 Disconnected。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁内 list_active；terminal 不在 alive 则 mark_disconnected；返回已断开行供 emit。
    pub async fn reconcile_active_sessions(
        &self,
        alive_terminal_ids: &HashSet<String>,
        at: &str,
    ) -> Result<Vec<AgentSessionRuntime>, AppError> {
        let _guard = agent_runtime_write_lock().lock().await;
        let active = self.repo.list_active(None, 10_000).await?;
        let mut disconnected = Vec::new();
        for row in active {
            if !alive_terminal_ids.contains(&row.terminal_session_id) {
                if self.repo.mark_disconnected(&row.id, at).await? {
                    if let Some(updated) = self.repo.get(&row.id).await? {
                        disconnected.push(updated);
                    }
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
            .unwrap()
            .active;
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
            .unwrap()
            .active;
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
            .unwrap()
            .active;
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
            .unwrap()
            .active;
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
            .unwrap()
            .active;
        let b = reducer
            .start_or_replace_active(create_input("gone", "p", "2026-07-15T00:00:01Z"))
            .await
            .unwrap()
            .active;
        let mut alive = HashSet::new();
        alive.insert("alive".to_string());
        let disconnected = reducer
            .reconcile_active_sessions(&alive, "2026-07-15T00:02:00Z")
            .await
            .unwrap();
        assert_eq!(disconnected.len(), 1);
        assert_eq!(disconnected[0].id, b.id);
        assert_eq!(disconnected[0].phase, AgentSessionPhase::Disconnected);
        let a2 = reducer.repo().get(&a.id).await.unwrap().unwrap();
        let b2 = reducer.repo().get(&b.id).await.unwrap().unwrap();
        assert!(a2.is_active);
        assert!(!b2.is_active);
        assert_eq!(b2.phase, AgentSessionPhase::Disconnected);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     mark_completed 在 version 竞态后必须仍落到 Completed，不得返回 Working。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先 Working；并发式 advance version 一次后 mark_completed；断言 Completed + inactive。
    #[tokio::test]
    async fn mark_completed_retries_after_version_race() {
        let reducer = fixture_reducer().await;
        let agent = reducer
            .start_or_replace_active(create_input("t-race", "p", "2026-07-15T00:00:00Z"))
            .await
            .unwrap()
            .active;
        // 模拟 OSC 抢先升版（与 mark 竞态）
        let _ = reducer
            .apply(event(&agent, 2, AgentSessionPhase::Working))
            .await
            .unwrap();
        let completed = reducer
            .mark_completed(Some(&agent.id), "t-race", "2026-07-15T00:02:00Z")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.phase, AgentSessionPhase::Completed);
        assert!(!completed.is_active);
        assert!(completed.version >= 3);
    }
}
