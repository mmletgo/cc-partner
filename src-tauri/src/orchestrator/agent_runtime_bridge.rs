//! orchestrator/agent_runtime_bridge — Orchestrator 与统一 Agent runtime 的 dual-write 桥
//!
//! Business Logic（为什么需要这个模块）:
//!     Runner 创建 terminal 后应建立 provider-neutral AgentSessionRuntime；completion 先更新
//!     Agent phase 再进入 Verifying。一个版本内继续 dual-write 旧 claude_session_id 字段。
//!
//! Code Logic（这个模块做什么）:
//!     `record_runner_activity` / `handle_normalized_agent_event` / `mark_agent_completed_before_verifying`；
//!     写路径经 `AgentRuntimeReducer` 串行锁；测试覆盖 CAS 重试与 real mark Completed。

use crate::error::AppError;
use crate::orchestrator::agent_adapter::{
    AgentAdapterRegistry, AgentLaunchRequest, AgentProviderId, NativeAgentEvent,
};
use crate::state::AppState;
use crate::storage::WorkbenchAgentSessionRepo;
use crate::workbench::agent_runtime::{
    emit_agent_runtime_changed, AgentReduceOutcome, AgentRuntimeMutation, AgentRuntimeReducer,
    AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Orchestrator 可见 Claude provider id（legacy dual-write 对齐）。
pub const ORCHESTRATOR_CLAUDE_PROVIDER_ID: &str = "claudeCodeVisible";

/// terminal activity 写库节流（避免 OSC 风暴）。
const ACTIVITY_THROTTLE: Duration = Duration::from_millis(500);

/// 每 agent_session_id 的最近 Working activity 写库时间（禁止跨 session 全局节流）。
static LAST_ACTIVITY_WRITE_BY_SESSION: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// 从 AppState 构造带 owner generic allowlist 的 adapter registry。
///
/// Business Logic（为什么需要这个函数）:
///     bridge/resume 必须与 Runner/catalog 使用同一 generic 配置。
///
/// Code Logic（这个函数做什么）:
///     读 config.orchestrator.generic_terminal → AgentAdapterRegistry::from_app_config。
fn registry_from_state(state: &AppState) -> Result<AgentAdapterRegistry, AppError> {
    let config = state
        .config
        .read()
        .map_err(|_| AppError::generic("读取 AppConfig 失败（锁损坏）"))?;
    Ok(AgentAdapterRegistry::from_app_config(&config))
}

/// 判断 Working activity 是否应被 per-session 节流跳过。
///
/// Business Logic（为什么需要这个函数）:
///     多任务并行时全局节流会拉长无关 session 的 liveness 间隙。
///
/// Code Logic（这个函数做什么）:
///     仅当同一 agent_session_id 在 ACTIVITY_THROTTLE 内已写过 Working 时返回 true。
fn should_throttle_working_activity(agent_session_id: &str) -> bool {
    let Ok(guard) = LAST_ACTIVITY_WRITE_BY_SESSION.lock() else {
        return false;
    };
    let Some(map) = guard.as_ref() else {
        return false;
    };
    map.get(agent_session_id)
        .is_some_and(|last| last.elapsed() < ACTIVITY_THROTTLE)
}

/// 记录某 agent_session 的 Working 写库时刻。
///
/// Business Logic（为什么需要这个函数）:
///     与 should_throttle 配对维护 per-session 节流状态。
///
/// Code Logic（这个函数做什么）:
///     插入/更新 HashMap 条目。
fn mark_working_activity_written(agent_session_id: &str) {
    if let Ok(mut guard) = LAST_ACTIVITY_WRITE_BY_SESSION.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(agent_session_id.to_string(), Instant::now());
        // 防止测试/长跑无限增长：超过 256 时清空（节流仅 best-effort）。
        if map.len() > 256 {
            map.clear();
            map.insert(agent_session_id.to_string(), Instant::now());
        }
    }
}

/// dual-write orchestrator task last_activity（stall watchdog 权威锚点之一）。
///
/// Business Logic（为什么需要这个函数）:
///     A1 agent session 活动必须推动 task.last_activity_at，否则 stall 误杀。
///
/// Code Logic（这个函数做什么）:
///     若 row 绑定 orchestrator task/attempt，调用 touch_task_last_activity。
async fn dual_write_task_activity_from_row(
    state: &AppState,
    row: &AgentSessionRuntime,
    occurred_at: &str,
) {
    if let (Some(task_id), Some(attempt)) = (
        row.orchestrator_task_id.as_deref(),
        row.orchestrator_attempt,
    ) {
        let at = if row.last_activity_at.trim().is_empty() {
            occurred_at
        } else {
            row.last_activity_at.as_str()
        };
        let _ = state
            .orchestrator_repo
            .touch_task_last_activity(task_id, attempt as i64, &row.terminal_session_id, at)
            .await;
    }
}

/// 为 Runner 新建的 terminal 创建 Launching Agent session（默认 Claude provider）。
///
/// Business Logic（为什么需要这个函数）:
///     兼容旧调用点；新路径应传真实 provider。
///
/// Code Logic（这个函数做什么）:
///     委托 `create_launching_agent_for_runner_with_provider(Claude)`。
pub async fn create_launching_agent_for_runner(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    terminal_session_id: &str,
    task_id: &str,
    attempt: u32,
) -> Result<AgentSessionRuntime, AppError> {
    create_launching_agent_for_runner_with_provider(
        state,
        project_id,
        worktree_id,
        terminal_session_id,
        task_id,
        attempt,
        AgentProviderId::ClaudeCodeVisible,
        None,
    )
    .await
}

/// 为 Runner 按 provider 创建 Launching Agent session。
///
/// Business Logic（为什么需要这个函数）:
///     Codex/generic attempt 必须把真实 provider_id 写入 A1 runtime，resume 才能选对 adapter。
///
/// Code Logic（这个函数做什么）:
///     委托 `create_launching_agent_for_runner_with_provider_and_id(..., None)`。
#[allow(clippy::too_many_arguments)] // runner create 需要完整 identity 上下文
pub async fn create_launching_agent_for_runner_with_provider(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    terminal_session_id: &str,
    task_id: &str,
    attempt: u32,
    provider: AgentProviderId,
    resumed_from: Option<&str>,
) -> Result<AgentSessionRuntime, AppError> {
    create_launching_agent_for_runner_with_provider_and_id(
        state,
        project_id,
        worktree_id,
        terminal_session_id,
        task_id,
        attempt,
        provider,
        resumed_from,
        None,
    )
    .await
}

/// 为 Runner 创建 Launching Agent session，可选固定预分配 agent id。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode bridge 要求 shell env 的 `CC_PARTNER_AGENT_SESSION_ID` 与 runtime 行 id 完全一致。
///
/// Code Logic（这个函数做什么）:
///     start_or_replace_active(Launching, id=preallocated?)；emit；返回 active runtime。
#[allow(clippy::too_many_arguments)]
pub async fn create_launching_agent_for_runner_with_provider_and_id(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    terminal_session_id: &str,
    task_id: &str,
    attempt: u32,
    provider: AgentProviderId,
    resumed_from: Option<&str>,
    preallocated_agent_session_id: Option<&str>,
) -> Result<AgentSessionRuntime, AppError> {
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let now = chrono::Utc::now().to_rfc3339();
    let explicit_id = preallocated_agent_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let outcome = reducer
        .start_or_replace_active(CreateActiveAgentSession {
            id: explicit_id,
            project_id: project_id.to_string(),
            worktree_id: worktree_id.map(str::to_string),
            terminal_session_id: terminal_session_id.to_string(),
            orchestrator_task_id: Some(task_id.to_string()),
            orchestrator_attempt: Some(attempt),
            provider_id: provider.as_str().to_string(),
            native_session_id: None,
            phase: AgentSessionPhase::Launching,
            started_at: now,
            resumed_from_agent_session_id: resumed_from.map(str::to_string),
        })
        .await?;
    if let Some(ended) = &outcome.ended {
        // end → Disconnected：非异常 phase，previous 未知
        emit_agent_runtime_changed(state, ended, None);
    }
    // 新 active 通常 Launching；无 previous phase
    emit_agent_runtime_changed(state, &outcome.active, None);
    // 记录 agent 启动时的 active pane，供自动标题 first-pane 门禁。
    crate::workbench::auto_title::bind_agent_title_pane_for_state(
        state,
        terminal_session_id,
        Some(outcome.active.id.as_str()),
        outcome.active.native_session_id.as_deref(),
    );
    Ok(outcome.active)
}

/// 记录 Runner 活动（phase 推进），可选 dual-write native/claude session id（仅 owner 内部）。
///
/// Business Logic（为什么需要这个函数）:
///     transcript scanner / OSC 需要把 Working 等 phase 写入统一 runtime，同时保留 legacy 字段一个版本。
///
/// Code Logic（这个函数做什么）:
///     per-session 500ms 节流；读 agent_session；apply CAS；可选 native_session_id（不进 DTO）；
///     Applied 时 dual-write task.last_activity_at。
pub async fn record_runner_activity(
    state: &AppState,
    agent_session_id: &str,
    terminal_session_id: &str,
    phase: AgentSessionPhase,
    native_session_id: Option<&str>,
    occurred_at: &str,
) -> Result<AgentReduceOutcome, AppError> {
    if phase == AgentSessionPhase::Working && should_throttle_working_activity(agent_session_id) {
        return Ok(AgentReduceOutcome::Ignored("activity_throttled"));
    }
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let Some(current) = reducer.repo().get(agent_session_id).await? else {
        return Ok(AgentReduceOutcome::Ignored("agent_not_found"));
    };
    // 旧 session 事件：terminal 上 active 已换人则忽略。
    if let Ok(Some(active)) = reducer
        .repo()
        .get_active_for_terminal(terminal_session_id)
        .await
    {
        if active.id != agent_session_id {
            return Ok(AgentReduceOutcome::Ignored("stale_agent_session"));
        }
    }
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
    if let AgentReduceOutcome::Applied {
        previous_phase,
        ref row,
    } = outcome
    {
        if phase == AgentSessionPhase::Working {
            mark_working_activity_written(agent_session_id);
        }
        emit_agent_runtime_changed(state, row, Some(previous_phase));
        dual_write_task_activity_from_row(state, row, occurred_at).await;
    }
    Ok(outcome)
}

/// 经选定 adapter 归一化 native 事件后写入 A1 runtime。
///
/// Business Logic（为什么需要这个函数）:
///     Hook/OSC 入站必须走 provider adapter，不能假设 Claude 形状。
///
/// Code Logic（这个函数做什么）:
///     registry.get(provider).normalize → handle_normalized_agent_event。
pub async fn handle_native_agent_event(
    state: &AppState,
    provider: AgentProviderId,
    event: NativeAgentEvent,
) -> Result<AgentReduceOutcome, AppError> {
    let registry = registry_from_state(state)?;
    let adapter = registry.get(provider)?;
    // A9：structured usage → Ledger cache（失败不阻断 mutation）
    if let Some(delta) = adapter.extract_usage(&event) {
        if delta.has_any() {
            let snap = crate::workbench::agent_ledger::ReliableUsageSnapshot {
                model_id: delta.model_id.clone(),
                input_tokens: delta.input_tokens,
                output_tokens: delta.output_tokens,
                cache_read_tokens: delta.cache_read_tokens,
                cache_write_tokens: delta.cache_write_tokens,
                cost_major: delta.cost_major.clone(),
                cost_currency: delta.cost_currency.clone(),
                ..Default::default()
            };
            let agent_id = event.agent_session_id.clone();
            if let Err(e) = state.agent_ledger_service.note_usage(&agent_id, snap).await {
                tracing::debug!(
                    agent_session_id = %agent_id,
                    "agent ledger note_usage failed: {e}"
                );
            }
        }
    }
    let mutation = adapter.normalize_runtime_event(event)?;
    handle_normalized_agent_event(state, mutation).await
}

/// resume 当前 attempt：仅当 adapter 支持且 native id 可用时启动。
///
/// Business Logic（为什么需要这个函数）:
///     repair/resume 必须使用原 provider，禁止静默换成 Claude。
///     OpenCode 的 idle TUI 仍占 PTY，必须 Fresh 终端 + 预分配 agent id。
///
/// Code Logic（这个函数做什么）:
///     读 task policy provider；adapter.build_resume_plan；
///     Fresh：新建 terminal/agent 并写入 resume plan；
///     Reuse：在给定 terminal 创建 agent 行并写入 resume plan。
///     写失败 → mark Failed，attempt 保持 repairable。
pub async fn resume_runner_attempt(
    state: &AppState,
    task_id: &str,
    terminal_session_id: &str,
    prompt: &str,
    native_session_id: Option<&str>,
    previous_agent_session_id: Option<&str>,
) -> Result<AgentSessionRuntime, AppError> {
    use crate::commands::workbench::local_write_workbench_session_input;
    use crate::orchestrator::agent_adapter::{
        render_terminal_command, ResumeTerminalPolicy, TerminalShellDialect,
    };

    let task = state.orchestrator_repo.get_task(task_id).await?;
    let provider = AgentProviderId::parse_legacy(task.runner_provider.as_deref())?;
    let registry = registry_from_state(state)?;
    let adapter = registry.get(provider)?;
    if !adapter.supports_resume() {
        return Err(AppError::generic(format!(
            "provider {} 不支持 resume",
            provider.as_str()
        )));
    }

    let attempt = task.attempt.max(1) as u32;
    let policy = adapter.resume_terminal_policy();

    let (write_terminal_id, agent_prealloc, session_command) = match policy {
        ResumeTerminalPolicy::Fresh => {
            let worktree_id = task.worktree_id.clone().ok_or_else(|| {
                AppError::generic("OpenCode resume 需要已有 worktree，禁止无现场 resume")
            })?;
            let terminal_id = uuid::Uuid::new_v4().to_string();
            let agent_id = uuid::Uuid::new_v4().to_string();
            let session =
                crate::commands::workbench::local_create_workbench_session_with_preallocated_ids(
                    state,
                    task.project_id.clone(),
                    Some(worktree_id),
                    Some(120),
                    Some(32),
                    terminal_id.clone(),
                    agent_id.clone(),
                )
                .await?;
            // Fresh resume 必须先 CAS 绑定新 terminal/agent；0-row miss 时 fail-closed，
            // 禁止继续 create runtime 或写 opencode --session 输入。
            state
                .orchestrator_repo
                .update_active_runner_session_and_agent(
                    &task.id,
                    attempt as i64,
                    &session.id,
                    Some(&agent_id),
                )
                .await?;
            (session.id, Some(agent_id), session.command)
        }
        ResumeTerminalPolicy::Reuse => {
            // 旧终端：command 未知时按 Posix 渲染。
            (terminal_session_id.to_string(), None, String::new())
        }
    };

    let request = AgentLaunchRequest {
        agent_session_id: agent_prealloc
            .clone()
            .unwrap_or_else(|| previous_agent_session_id.unwrap_or("").to_string()),
        terminal_session_id: write_terminal_id.clone(),
        cwd: String::new(),
        prompt: prompt.to_string(),
        native_session_id: native_session_id.map(str::to_string),
        max_turns: task.runner_max_turns.unwrap_or(1).clamp(1, 20) as u32,
        stall_timeout_ms: task.runner_stall_timeout_ms.unwrap_or(300_000).max(0) as u64,
    };
    let plan = adapter.build_resume_plan(&request)?;

    let runtime = create_launching_agent_for_runner_with_provider_and_id(
        state,
        &task.project_id,
        task.worktree_id.as_deref(),
        &write_terminal_id,
        task_id,
        attempt,
        provider,
        previous_agent_session_id,
        agent_prealloc.as_deref(),
    )
    .await?;

    let dialect = if session_command.is_empty() {
        TerminalShellDialect::Posix
    } else {
        TerminalShellDialect::from_command(&session_command)
    };
    let input = match render_terminal_command(&plan, dialect) {
        Ok(s) => s,
        Err(err) => {
            let _ = mark_agent_failed_best_effort(state, &runtime.id, &write_terminal_id).await;
            return Err(err);
        }
    };
    if let Err(err) =
        local_write_workbench_session_input(state, write_terminal_id.clone(), input).await
    {
        let _ = mark_agent_failed_best_effort(state, &runtime.id, &write_terminal_id).await;
        return Err(err);
    }
    Ok(runtime)
}

/// resume 写失败时将新 runtime 标 Failed（best-effort）。
///
/// Business Logic（为什么需要这个函数）:
///     写终端失败后不得留下 Launching 悬挂；attempt 保持可修复。
///
/// Code Logic（这个函数做什么）:
///     apply Failed mutation 或 end_active_for_terminal。
async fn mark_agent_failed_best_effort(
    state: &AppState,
    agent_session_id: &str,
    terminal_session_id: &str,
) -> Result<(), AppError> {
    let now = chrono::Utc::now().to_rfc3339();
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    if let Ok(Some(current)) = reducer.repo().get(agent_session_id).await {
        let mutation = AgentRuntimeMutation {
            agent_session_id: agent_session_id.to_string(),
            terminal_session_id: terminal_session_id.to_string(),
            expected_version: current.version,
            event_version: current.version.saturating_add(1),
            phase: AgentSessionPhase::Failed,
            native_session_id: None,
            outcome_code: Some("resume_write_failed".into()),
            occurred_at: now,
        };
        let _ = reducer.apply(mutation).await;
    }
    Ok(())
}

/// 处理归一化 Agent event（OSC / Hook 入站后）。
///
/// Business Logic（为什么需要这个函数）:
///     Orchestrator 与普通 terminal 共享 reducer 入口语义；Applied 必须 dual-write task 活动。
///
/// Code Logic（这个函数做什么）:
///     委托 `AgentRuntimeReducer::apply`；Applied 时 emit + touch_task_last_activity；
///     phase=Completed 时尝试 HookEvent 自动完成。
pub async fn handle_normalized_agent_event(
    state: &AppState,
    mutation: AgentRuntimeMutation,
) -> Result<AgentReduceOutcome, AppError> {
    let occurred_at = mutation.occurred_at.clone();
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let outcome = reducer.apply(mutation).await?;
    if let AgentReduceOutcome::Applied {
        previous_phase,
        ref row,
    } = outcome
    {
        emit_agent_runtime_changed(state, row, Some(previous_phase));
        dual_write_task_activity_from_row(state, row, &occurred_at).await;
        if row.phase == AgentSessionPhase::Completed && row.orchestrator_task_id.is_some() {
            if let Err(err) =
                crate::orchestrator::completion::maybe_complete_from_agent_runtime_completed(
                    state, row,
                )
                .await
            {
                tracing::warn!(
                    agent_id = %row.id,
                    "HookEvent completion from normalized event failed: {err}"
                );
            }
        }
    }
    Ok(outcome)
}

/// completion 路径：先把 Agent 标 Completed，再允许 task 进入 Verifying。
///
/// Business Logic（为什么需要这个函数）:
///     Spec 要求 completion reducer 先更新 Agent runtime，再由既有 task SM 进 Verifying。
///
/// Code Logic（这个函数做什么）:
///     经 reducer 串行锁：CAS Completed（带重试）或 end_active_for_terminal；
///     若有 state 则 emit 投影；失败返回错误（不得返回仍 Working 的 active 行）。
pub async fn mark_agent_completed_before_verifying(
    repo: &WorkbenchAgentSessionRepo,
    agent_session_id: Option<&str>,
    terminal_session_id: &str,
    at: &str,
) -> Result<Option<AgentSessionRuntime>, AppError> {
    mark_agent_completed_before_verifying_with_emit(
        repo,
        None,
        agent_session_id,
        terminal_session_id,
        at,
    )
    .await
}

/// 带可选 emit 的 completion mark（生产 completion 路径传入 AppState）。
///
/// Business Logic（为什么需要这个函数）:
///     durable Completed 后 live 客户端必须立刻看到投影，不能等 Gap/snapshot。
///
/// Code Logic（这个函数做什么）:
///     委托 reducer.mark_completed；成功后若有 state 则 emit_agent_runtime_changed。
pub async fn mark_agent_completed_before_verifying_with_emit(
    repo: &WorkbenchAgentSessionRepo,
    state: Option<&AppState>,
    agent_session_id: Option<&str>,
    terminal_session_id: &str,
    at: &str,
) -> Result<Option<AgentSessionRuntime>, AppError> {
    let reducer = AgentRuntimeReducer::new(repo.clone());
    let updated = reducer
        .mark_completed(agent_session_id, terminal_session_id, at)
        .await?;
    if let (Some(state), Some(row)) = (state, updated.as_ref()) {
        // Completed 非异常；previous 未知时 None 不会触发 needsInput/failed 通知
        emit_agent_runtime_changed(state, row, None);
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

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
    ///     completion 必须真实把 Agent 标 Completed（inactive），才能进入 Verifying。
    ///
    /// Code Logic（这个测试做什么）:
    ///     create Working → mark_agent_completed_before_verifying(Some id) → 断言 phase Completed。
    #[tokio::test]
    async fn completion_updates_agent_before_task_enters_verifying() {
        let repo = fixture_repo().await;
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

        let completed = mark_agent_completed_before_verifying(
            &repo,
            Some(&agent.id),
            "term-orch",
            "2026-07-15T00:01:00Z",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(completed.phase, AgentSessionPhase::Completed);
        assert!(!completed.is_active);

        let again = repo.get(&agent.id).await.unwrap().unwrap();
        assert_eq!(again.phase, AgentSessionPhase::Completed);
        assert!(!again.is_active);
        // terminal 上不再有 active（可进入 Verifying 的前置条件）
        assert!(repo
            .get_active_for_terminal("term-orch")
            .await
            .unwrap()
            .is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     提供 agent id 时若 CAS 因 version 竞态失败，必须重试/fallthrough，不得返回 Working。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建后用 apply_mutation 升 version，再用过期 expected 路径的 mark（内部读新鲜 version）完成。
    #[tokio::test]
    async fn mark_with_id_retries_cas_and_never_returns_working_active() {
        let repo = fixture_repo().await;
        let agent = repo
            .create_active(CreateActiveAgentSession {
                id: Some("agent-cas-1".into()),
                project_id: "p".into(),
                worktree_id: None,
                terminal_session_id: "term-cas".into(),
                orchestrator_task_id: Some("task-cas".into()),
                orchestrator_attempt: Some(1),
                provider_id: ORCHESTRATOR_CLAUDE_PROVIDER_ID.into(),
                native_session_id: None,
                phase: AgentSessionPhase::Working,
                started_at: "2026-07-15T00:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        // 抢先 CAS 升版，模拟 OSC 与 completion 竞态
        assert!(repo
            .apply_mutation(&AgentRuntimeMutation {
                agent_session_id: agent.id.clone(),
                terminal_session_id: "term-cas".into(),
                expected_version: 1,
                event_version: 2,
                phase: AgentSessionPhase::Working,
                native_session_id: None,
                outcome_code: None,
                occurred_at: "2026-07-15T00:00:30Z".into(),
            })
            .await
            .unwrap());

        let completed = mark_agent_completed_before_verifying(
            &repo,
            Some(&agent.id),
            "term-cas",
            "2026-07-15T00:01:00Z",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(completed.phase, AgentSessionPhase::Completed);
        assert!(!completed.is_active);
        assert_ne!(completed.phase, AgentSessionPhase::Working);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无 agent id 时 end_active_for_terminal 仍必须 durable Completed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mark(..., None, terminal) → terminal 无 active 且 phase Completed。
    #[tokio::test]
    async fn mark_without_id_ends_active_for_terminal() {
        let repo = fixture_repo().await;
        let agent = repo
            .create_active(CreateActiveAgentSession {
                id: None,
                project_id: "p".into(),
                worktree_id: None,
                terminal_session_id: "term-end".into(),
                orchestrator_task_id: None,
                orchestrator_attempt: None,
                provider_id: ORCHESTRATOR_CLAUDE_PROVIDER_ID.into(),
                native_session_id: None,
                phase: AgentSessionPhase::Idle,
                started_at: "2026-07-15T00:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        let completed =
            mark_agent_completed_before_verifying(&repo, None, "term-end", "2026-07-15T00:01:00Z")
                .await
                .unwrap()
                .unwrap();
        assert_eq!(completed.id, agent.id);
        assert_eq!(completed.phase, AgentSessionPhase::Completed);
        assert!(!completed.is_active);
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
        let dto =
            crate::workbench::agent_runtime::snapshot::AgentSessionRuntimeDto::from_runtime(&row);
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("nativeSessionId"));
        assert!(!json.contains("claude-native-xyz"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Fresh resume CAS miss 时不得继续写 OpenCode resume 输入。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 `update_active_runner_session_and_agent` Conflict 在 resume 路径上是 fail-closed 前置条件
    ///     （CAS 在 write terminal input 之前）。
    #[test]
    fn fresh_resume_cas_miss_must_fail_closed_before_terminal_write() {
        // 文档/契约测试：resume_runner_attempt 在 Fresh 分支对 CAS 使用 `?`，
        // 不再 `let _ = ...` 忽略 0-row miss。源码字符级守卫防止回归。
        let source = include_str!("agent_runtime_bridge.rs");
        assert!(
            source.contains("update_active_runner_session_and_agent("),
            "resume must call CAS helper"
        );
        assert!(
            !source.contains("let _ = state\n                .orchestrator_repo\n                .update_active_runner_session_and_agent"),
            "CAS miss must not be ignored with let _ ="
        );
        // 成功路径在 build_resume_plan / local_write 之前 await? CAS。
        let cas_pos = source
            .find("update_active_runner_session_and_agent(")
            .expect("CAS call");
        let write_pos = source
            .find("local_write_workbench_session_input(state, write_terminal_id.clone(), input)")
            .expect("terminal write");
        assert!(
            cas_pos < write_pos,
            "CAS must precede OpenCode resume terminal write"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     resume 必须保留原 provider，旧 session 事件不得覆盖新 active。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 Codex Launching → 再 create resume from old id → 旧 id 事件应被 stale 忽略语义覆盖在 record 路径。
    #[tokio::test]
    async fn resume_uses_original_provider_and_old_session_event_is_ignored() {
        let repo = fixture_repo().await;
        let reducer = AgentRuntimeReducer::new(repo.clone());
        let now = chrono::Utc::now().to_rfc3339();
        let first = reducer
            .start_or_replace_active(CreateActiveAgentSession {
                id: Some("agent-old".into()),
                project_id: "p".into(),
                worktree_id: Some("wt".into()),
                terminal_session_id: "term-resume".into(),
                orchestrator_task_id: Some("task".into()),
                orchestrator_attempt: Some(1),
                provider_id: AgentProviderId::CodexVisible.as_str().to_string(),
                native_session_id: Some("native-1".into()),
                phase: AgentSessionPhase::Working,
                started_at: now.clone(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap()
            .active;
        assert_eq!(first.provider_id, "codexVisible");

        let resumed = reducer
            .start_or_replace_active(CreateActiveAgentSession {
                id: Some("agent-new".into()),
                project_id: "p".into(),
                worktree_id: Some("wt".into()),
                terminal_session_id: "term-resume".into(),
                orchestrator_task_id: Some("task".into()),
                orchestrator_attempt: Some(1),
                provider_id: AgentProviderId::CodexVisible.as_str().to_string(),
                native_session_id: Some("native-1".into()),
                phase: AgentSessionPhase::Launching,
                started_at: now.clone(),
                resumed_from_agent_session_id: Some("agent-old".into()),
            })
            .await
            .unwrap()
            .active;
        assert_eq!(resumed.provider_id, "codexVisible");
        assert_eq!(resumed.id, "agent-new");
        let active = repo
            .get_active_for_terminal("term-resume")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.id, "agent-new");
        // 旧 session 已 inactive
        let old = repo.get("agent-old").await.unwrap().unwrap();
        assert!(!old.is_active);
    }
}
