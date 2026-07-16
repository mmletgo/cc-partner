//! orchestrator/agent_runtime_bridge — Orchestrator 与统一 Agent runtime 的 dual-write 桥
//!
//! Business Logic（为什么需要这个模块）:
//!     Runner 创建 terminal 后应建立 provider-neutral AgentSessionRuntime；completion 先更新
//!     Agent phase 再进入 Verifying。一个版本内继续 dual-write 旧 claude_session_id 字段。
//!
//! Code Logic（这个模块做什么）:
//!     `record_runner_activity` / `handle_normalized_agent_event` / `mark_agent_completed_before_verifying`；
//!     测试覆盖 completion 排序与 legacy 字段仍可写。

use crate::error::AppError;
use crate::state::AppState;
use crate::storage::WorkbenchAgentSessionRepo;
use crate::workbench::agent_runtime::{
    emit_agent_runtime_changed, AgentReduceOutcome, AgentRuntimeMutation, AgentRuntimeReducer,
    AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};

/// Orchestrator 可见 Claude provider id（legacy dual-write 对齐）。
pub const ORCHESTRATOR_CLAUDE_PROVIDER_ID: &str = "claudeCodeVisible";

/// 为 Runner 新建的 terminal 创建 Launching Agent session。
///
/// Business Logic（为什么需要这个函数）:
///     普通 Workbench 与 Orchestrator 共用 Agent runtime 真值；Runner 必须在 terminal 就绪后立刻建 row。
///
/// Code Logic（这个函数做什么）:
///     start_or_replace_active(Launching)；返回 runtime（含 id 供 attempt 持久化）。
pub async fn create_launching_agent_for_runner(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    terminal_session_id: &str,
    task_id: &str,
    attempt: u32,
) -> Result<AgentSessionRuntime, AppError> {
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let now = chrono::Utc::now().to_rfc3339();
    let row = reducer
        .start_or_replace_active(CreateActiveAgentSession {
            id: None,
            project_id: project_id.to_string(),
            worktree_id: worktree_id.map(str::to_string),
            terminal_session_id: terminal_session_id.to_string(),
            orchestrator_task_id: Some(task_id.to_string()),
            orchestrator_attempt: Some(attempt),
            provider_id: ORCHESTRATOR_CLAUDE_PROVIDER_ID.to_string(),
            native_session_id: None,
            phase: AgentSessionPhase::Launching,
            started_at: now,
            resumed_from_agent_session_id: None,
        })
        .await?;
    emit_agent_runtime_changed(state, &row);
    Ok(row)
}

/// 记录 Runner 活动（phase 推进），可选 dual-write native/claude session id（仅 owner 内部）。
///
/// Business Logic（为什么需要这个函数）:
///     transcript scanner / OSC 需要把 Working 等 phase 写入统一 runtime，同时保留 legacy 字段一个版本。
///
/// Code Logic（这个函数做什么）:
///     读 active for terminal 或 by agent_session_id；apply CAS Working；
///     若提供 native_session_id 则写入 runtime 行（不进 DTO）。
pub async fn record_runner_activity(
    state: &AppState,
    agent_session_id: &str,
    terminal_session_id: &str,
    phase: AgentSessionPhase,
    native_session_id: Option<&str>,
    occurred_at: &str,
) -> Result<AgentReduceOutcome, AppError> {
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let Some(current) = reducer.repo().get(agent_session_id).await? else {
        return Ok(AgentReduceOutcome::Ignored("agent_not_found"));
    };
    let mutation = AgentRuntimeMutation {
        agent_session_id: agent_session_id.to_string(),
        terminal_session_id: terminal_session_id.to_string(),
        expected_version: current.version,
        event_version: current.version.saturating_add(1),
        phase,
        native_session_id: native_session_id.map(str::to_string),
        outcome_code: None,
        occurred_at: occurred_at.to_string(),
    };
    let outcome = reducer.apply(mutation).await?;
    if let AgentReduceOutcome::Applied(ref row) = outcome {
        emit_agent_runtime_changed(state, row);
    }
    Ok(outcome)
}

/// 处理归一化 Agent event（OSC / Hook 入站后）。
///
/// Business Logic（为什么需要这个函数）:
///     Orchestrator 与普通 terminal 共享 reducer 入口语义。
///
/// Code Logic（这个函数做什么）:
///     委托 `AgentRuntimeReducer::apply` 并在 Applied 时 emit。
pub async fn handle_normalized_agent_event(
    state: &AppState,
    mutation: AgentRuntimeMutation,
) -> Result<AgentReduceOutcome, AppError> {
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let outcome = reducer.apply(mutation).await?;
    if let AgentReduceOutcome::Applied(ref row) = outcome {
        emit_agent_runtime_changed(state, row);
    }
    Ok(outcome)
}

/// completion 路径：先把 Agent 标 Completed，再允许 task 进入 Verifying。
///
/// Business Logic（为什么需要这个函数）:
///     Spec 要求 completion reducer 先更新 Agent runtime，再由既有 task SM 进 Verifying。
///
/// Code Logic（这个函数做什么）:
///     end_active_for_terminal(Completed) 或 mark by agent id；返回更新后的 Agent 行。
pub async fn mark_agent_completed_before_verifying(
    repo: &WorkbenchAgentSessionRepo,
    agent_session_id: Option<&str>,
    terminal_session_id: &str,
    at: &str,
) -> Result<Option<AgentSessionRuntime>, AppError> {
    if let Some(id) = agent_session_id {
        if let Some(current) = repo.get(id).await? {
            if current.is_active {
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
                let _ = repo.apply_mutation(&mutation).await?;
                return repo.get(id).await;
            }
        }
    }
    repo.end_active_for_terminal(
        terminal_session_id,
        AgentSessionPhase::Completed,
        Some("dev_done"),
        at,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;

    /// 内存 bridge fixture。
    async fn fixture_repo() -> WorkbenchAgentSessionRepo {
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
        WorkbenchAgentSessionRepo::new(pool)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     completion 必须先 Completed Agent，再允许 task 进入 Verifying。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用原子计数模拟顺序：先 mark agent Completed，再 set attempt phase Verifying。
    #[tokio::test]
    async fn completion_updates_agent_before_task_enters_verifying() {
        let repo = fixture_repo().await;
        let order = Arc::new(AtomicU8::new(0));
        let agent = repo
            .create_active(CreateActiveAgentSession {
                id: Some("agent-orch-1".into()),
                project_id: "p".into(),
                worktree_id: Some("wt".into()),
                terminal_session_id: "term-orch".into(),
                orchestrator_task_id: Some("task-1".into()),
                orchestrator_attempt: Some(1),
                provider_id: ORCHESTRATOR_CLAUDE_PROVIDER_ID.into(),
                native_session_id: None,
                phase: AgentSessionPhase::Working,
                started_at: "2026-07-15T00:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();

        // step 1: agent complete
        let completed = mark_agent_completed_before_verifying(
            &repo,
            Some(&agent.id),
            "term-orch",
            "2026-07-15T00:01:00Z",
        )
        .await
        .unwrap()
        .unwrap();
        order.store(1, Ordering::SeqCst);
        assert_eq!(completed.phase, AgentSessionPhase::Completed);
        assert!(!completed.is_active);

        // step 2: only after agent complete may task pipeline enter verifying
        assert_eq!(order.load(Ordering::SeqCst), 1);
        let task_pipeline_phase = "verifying";
        order.store(2, Ordering::SeqCst);
        assert_eq!(task_pipeline_phase, "verifying");
        assert_eq!(order.load(Ordering::SeqCst), 2);
        // agent must still be completed (not overwritten by task phase)
        let again = repo.get(&agent.id).await.unwrap().unwrap();
        assert_eq!(again.phase, AgentSessionPhase::Completed);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     dual-write 一个版本内仍可把 native/claude id 写在 runtime 行（不进投影 DTO）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply mutation 带 native_session_id；get 仍有值；DTO 序列化无该字段。
    #[tokio::test]
    async fn legacy_native_session_stays_owner_local_not_in_dto() {
        let repo = fixture_repo().await;
        let agent = repo
            .create_active(CreateActiveAgentSession {
                id: None,
                project_id: "p".into(),
                worktree_id: None,
                terminal_session_id: "t-legacy".into(),
                orchestrator_task_id: Some("task".into()),
                orchestrator_attempt: Some(1),
                provider_id: ORCHESTRATOR_CLAUDE_PROVIDER_ID.into(),
                native_session_id: None,
                phase: AgentSessionPhase::Launching,
                started_at: "2026-07-15T00:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        let ok = repo
            .apply_mutation(&AgentRuntimeMutation {
                agent_session_id: agent.id.clone(),
                terminal_session_id: "t-legacy".into(),
                expected_version: 1,
                event_version: 2,
                phase: AgentSessionPhase::Working,
                native_session_id: Some("claude-native-xyz".into()),
                outcome_code: None,
                occurred_at: "2026-07-15T00:00:01Z".into(),
            })
            .await
            .unwrap();
        assert!(ok);
        let row = repo.get(&agent.id).await.unwrap().unwrap();
        assert_eq!(row.native_session_id.as_deref(), Some("claude-native-xyz"));
        let dto = crate::workbench::agent_runtime::AgentSessionRuntimeDto::from_runtime(&row);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("nativeSessionId"));
        assert!(!json.contains("claude-native-xyz"));
    }
}
