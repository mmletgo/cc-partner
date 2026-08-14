//! workbench/agent_runtime — provider-neutral Agent session 运行时
//!
//! Business Logic（为什么需要这个模块）:
//!     普通 Workbench 终端与 Orchestrator 需要共享稳定的 Agent session 身份、phase、
//!     version 与 Gap 恢复能力；不能依赖 terminal 字节流或 task 私有 Claude 字段。
//!
//! Code Logic（这个模块做什么）:
//!     导出领域模型、OSC 解码与 mutation ingress；reducer/snapshot 由后续任务叠加。

mod agent_usage;
mod claude_status;
pub mod models;
pub mod opencode_bridge;
pub mod osc;
pub mod reducer;
pub mod snapshot;

pub use models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
pub use opencode_bridge::{
    OpenCodeBridgeOutcome, OpenCodeEventMapper, OpenCodeOfficialEvent, OpenCodeRuntimeBridge,
    OPENCODE_RUNTIME_BRIDGE_REL_PATH, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH,
};
#[cfg(test)]
pub use osc::encode_agent_osc_frame;
pub use osc::AgentOscDecoder;
// encode_agent_osc_frame_full is used by opencode_bridge within the crate; keep module public API narrow.
pub use reducer::{
    collect_alive_terminal_ids, AgentReduceOutcome, AgentRuntimeReducer, EnsureInteractiveOutcome,
};
pub use snapshot::{
    emit_agent_runtime_changed, get_agent_runtime_snapshot_for_state, AgentRuntimeSnapshot,
};

use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

/// 有界 mutation ingress 容量（OSC 热路径 try_send，满则丢弃并记诊断）。
const AGENT_MUTATION_CHANNEL_CAP: usize = 256;

/// 进程内 mutation 发送端（由 `install_agent_mutation_ingress` 安装）。
static AGENT_MUTATION_TX: OnceLock<mpsc::Sender<AgentRuntimeMutation>> = OnceLock::new();

/// channel 满时保留最近一次高优先级（Completed/Failed/NeedsInput）mutation。
static TERMINAL_MUTATION_OVERFLOW: Mutex<Option<AgentRuntimeMutation>> = Mutex::new(None);

/// 已执行终态 usage 补记的 agent_session_id 集合（进程内一次性去重）。
static TERMINAL_USAGE_NOTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// 终态时从 CLI 本地会话文件提取 usage 并补记 Ledger（每 agent 一次）。
///
/// Business Logic（为什么需要这个函数）:
///     Agent Ledger 的 tokens 生产中长期为 null（`note_usage()` 无调用者）；各 CLI 终态后
///     已把可靠 usage 落到本地 session 文件/SQLite，需要在 runtime 终态时提取补记。
///
/// Code Logic（这个模块做什么）:
///     仅 terminal phase + 支持 usage 的三 provider + 非空 native_session_id 才触发；
///     `TERMINAL_USAGE_NOTED` 按 agent_session_id 一次性去重后 tokio::spawn：
///     spawn_blocking 提取 → has_any 时 note_usage；Err 仅 debug 日志（不打路径），不阻断。
async fn maybe_note_terminal_usage(state: &AppState, row: &AgentSessionRuntime) {
    if !row.phase.is_terminal() {
        return;
    }
    if !matches!(
        row.provider_id.as_str(),
        "claudeCodeVisible" | "codex" | "opencode"
    ) {
        return;
    }
    let Some(native) = row
        .native_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    {
        let mut guard = TERMINAL_USAGE_NOTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !guard.insert(row.id.clone()) {
            return;
        }
    }
    let ledger_svc = state.agent_ledger_service.clone();
    let agent_id = row.id.clone();
    let provider = row.provider_id.clone();
    let native = native.to_string();
    tokio::spawn(async move {
        let extracted = tokio::task::spawn_blocking(move || {
            agent_usage::extract_provider_usage(&provider, &native)
        })
        .await;
        match extracted {
            Ok(Some(snapshot)) if snapshot.has_any() => {
                if let Err(err) = ledger_svc.note_usage(&agent_id, snapshot).await {
                    tracing::debug!(
                        agent_id = %agent_id,
                        "terminal usage note_usage failed: {err}"
                    );
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::debug!(
                    agent_id = %agent_id,
                    "terminal usage extract task failed: {err}"
                );
            }
        }
    });
}

/// 测试可替换的 send 钩子（默认走 channel）。
type AgentMutationTestHook = Option<Box<dyn Fn(AgentRuntimeMutation) + Send>>;
static AGENT_MUTATION_TEST_HOOK: Mutex<AgentMutationTestHook> = Mutex::new(None);

/// 判断 mutation 是否为不得丢弃的高优先级 phase。
///
/// Business Logic（为什么需要这个函数）:
///     channel 满时普通 Working 可丢并由对账恢复；Completed/Failed/NeedsInput 必须送达。
///
/// Code Logic（这个函数做什么）:
///     phase ∈ {Completed, Failed, NeedsInput}。
fn is_priority_agent_mutation(mutation: &AgentRuntimeMutation) -> bool {
    matches!(
        mutation.phase,
        AgentSessionPhase::Completed | AgentSessionPhase::Failed | AgentSessionPhase::NeedsInput
    )
}

/// 取出并清空 overflow 槽中的高优先级 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     worker 在消费 channel 间隙必须冲刷 overflow，避免终态永久滞留。
///
/// Code Logic（这个函数做什么）:
///     `TERMINAL_MUTATION_OVERFLOW.take()`。
fn take_terminal_mutation_overflow() -> Option<AgentRuntimeMutation> {
    TERMINAL_MUTATION_OVERFLOW
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
}

/// 向指定 sender 投递 mutation；满时对高优先级写入 overflow。
///
/// Business Logic（为什么需要这个函数）:
///     单测可注入 capacity=1 的 channel 验证 Completed 不被丢弃。
///
/// Code Logic（这个函数做什么）:
///     try_send；Full 且 priority → overflow slot（last-wins）；其它记 debug。
fn enqueue_agent_mutation_to(
    tx: &mpsc::Sender<AgentRuntimeMutation>,
    mutation: AgentRuntimeMutation,
) {
    match tx.try_send(mutation) {
        Ok(()) => {}
        Err(TrySendError::Full(m)) => {
            if is_priority_agent_mutation(&m) {
                if let Ok(mut slot) = TERMINAL_MUTATION_OVERFLOW.lock() {
                    *slot = Some(m);
                    tracing::debug!(
                        "agent mutation ingress full; retained priority mutation in overflow"
                    );
                }
            } else {
                tracing::debug!("agent mutation ingress full or closed; drop non-priority event");
            }
        }
        Err(TrySendError::Closed(_)) => {
            tracing::debug!("agent mutation ingress closed; drop event");
        }
    }
}

/// 安装 owner mutation ingress，返回 receiver 供 reducer worker 消费。
///
/// Business Logic（为什么需要这个函数）:
///     reader 线程不得直接 SQL；必须有单一有界 channel 串行化 mutation。
///
/// Code Logic（这个函数做什么）:
///     创建 mpsc channel，OnceLock 安装 tx；重复安装返回 None。
#[allow(dead_code)] // T3 reducer worker 安装
pub fn install_agent_mutation_ingress() -> Option<mpsc::Receiver<AgentRuntimeMutation>> {
    let (tx, rx) = mpsc::channel(AGENT_MUTATION_CHANNEL_CAP);
    match AGENT_MUTATION_TX.set(tx) {
        Ok(()) => Some(rx),
        Err(_) => None,
    }
}

/// 非阻塞投递 mutation；未安装 ingress 时丢弃；channel 满时普通事件丢弃，
/// Completed/Failed/NeedsInput 写入 overflow 槽由 worker 冲刷。
///
/// Business Logic（为什么需要这个函数）:
///     PTY reader 在 OS 线程上运行，不能 block 等 DB；终态不得因 channel 满而永久丢失。
///
/// Code Logic（这个函数做什么）:
///     优先 test hook；否则 `enqueue_agent_mutation_to`；`state` 预留给后续 metrics。
pub fn try_enqueue_agent_mutation(state: &AppState, mutation: AgentRuntimeMutation) {
    let _ = state;
    if let Ok(guard) = AGENT_MUTATION_TEST_HOOK.lock() {
        if let Some(hook) = guard.as_ref() {
            hook(mutation);
            return;
        }
    }
    if let Some(tx) = AGENT_MUTATION_TX.get() {
        enqueue_agent_mutation_to(tx, mutation);
    }
}

/// 测试：安装同步 hook（绕过 async channel）。
///
/// Business Logic（为什么需要这个函数）:
///     OSC→sessions 集成测需要断言 enqueue 内容，无需启动完整 reducer。
///
/// Code Logic（这个函数做什么）:
///     替换全局 hook；`None` 清除。
/// 测试 hook 安装（仅 test 二进制使用）。
#[cfg(test)]
#[allow(dead_code)]
pub fn set_agent_mutation_test_hook(hook: Option<Box<dyn Fn(AgentRuntimeMutation) + Send>>) {
    *AGENT_MUTATION_TEST_HOOK.lock().expect("hook lock") = hook;
}

/// 在 owner 上应用并投影一条 Agent runtime mutation。
///
/// Business Logic（为什么需要这个函数）:
///     OSC、Hook 与 provider 结构化状态对账必须经过同一 durable/emit/Ledger/completion 路径，
///     否则某一来源会只改数据库却不刷新顶部计数，或漏掉终态副作用。
///
/// Code Logic（这个函数做什么）:
///     reducer.apply 后统一回填 native pane 绑定、发 runtime 事件、写终态 Ledger、更新
///     Orchestrator activity，并在编排 Agent Completed 时推进 Verifying。
async fn apply_owner_agent_mutation(
    state: &crate::state::AppState,
    reducer: &AgentRuntimeReducer,
    mutation: AgentRuntimeMutation,
) {
    let occurred_at = mutation.occurred_at.clone();
    match reducer.apply(mutation).await {
        Ok(AgentReduceOutcome::Applied {
            previous_phase,
            row,
        }) => {
            tracing::debug!(
                agent_id = %row.id,
                phase = row.phase.as_str(),
                version = row.version,
                "agent runtime mutation applied"
            );
            // native_session_id 回填时挂上 title-pane 绑定，供 Codex/Claude 索引按 native 命中 owner pane。
            if let Some(native) = row.native_session_id.as_deref() {
                let _ = state
                    .workbench_sessions
                    .bind_native_title_pane(&row.terminal_session_id, native);
                // 若启动时未 bind terminal（极少），用当前 active 再 seed agent/terminal 映射。
                if state
                    .workbench_sessions
                    .agent_title_pane_for(&row.terminal_session_id, Some(native))
                    .is_none()
                {
                    crate::workbench::auto_title::bind_agent_title_pane_for_state(
                        state,
                        &row.terminal_session_id,
                        Some(row.id.as_str()),
                        Some(native),
                    );
                }
            }
            emit_agent_runtime_changed(state, &row, Some(previous_phase));
            // A9：首次终态旁路写 Ledger；失败隔离，不阻断 runtime 完成路径。
            if row.phase.is_terminal() {
                // A9b：终态后从 CLI 本地会话文件提取 usage 补记 Ledger（每 agent 一次）。
                maybe_note_terminal_usage(state, &row).await;
                let ledger_svc = state.agent_ledger_service.clone();
                let row_for_ledger = row.clone();
                let prev = previous_phase;
                tokio::spawn(async move {
                    crate::workbench::agent_ledger::service::on_agent_runtime_terminal(
                        &ledger_svc,
                        &row_for_ledger,
                        Some(prev),
                    )
                    .await;
                });
            }
            // H1：OSC/Hook/provider 状态路径 dual-write task.last_activity_at，供 stall watchdog 使用。
            if let (Some(task_id), Some(attempt)) = (
                row.orchestrator_task_id.as_deref(),
                row.orchestrator_attempt,
            ) {
                let activity_at = if row.last_activity_at.trim().is_empty() {
                    occurred_at.as_str()
                } else {
                    row.last_activity_at.as_str()
                };
                if let Err(err) = state
                    .orchestrator_repo
                    .touch_task_last_activity(
                        task_id,
                        attempt as i64,
                        &row.terminal_session_id,
                        activity_at,
                    )
                    .await
                {
                    tracing::debug!(
                        task_id = %task_id,
                        "touch_task_last_activity after Agent runtime apply failed: {err}"
                    );
                }
            }
            // HookEvent completion：runtime Completed 时按 attempt 冻结合同推进 Verifying。
            if row.phase == AgentSessionPhase::Completed && row.orchestrator_task_id.is_some() {
                if let Err(err) =
                    crate::orchestrator::completion::maybe_complete_from_agent_runtime_completed(
                        state, &row,
                    )
                    .await
                {
                    tracing::warn!(
                        agent_id = %row.id,
                        "HookEvent completion from agent runtime failed: {err}"
                    );
                }
            }
        }
        Ok(AgentReduceOutcome::Ignored(reason)) => {
            tracing::debug!(reason, "agent runtime mutation ignored");
        }
        Err(e) => tracing::warn!("agent runtime mutation apply error: {e}"),
    }
}

/// 启动 owner Agent runtime worker：安装 ingress、对账 active、串行消费 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     HeadlessOwner 进程启动后必须恢复 Agent 真值：幽灵 active → Disconnected，
///     并把 OSC 入站 mutation 串行写入 SQLite。
///
/// Code Logic（这个函数做什么）:
///     install channel → reconcile（registry + running DB sessions）→ select 消费 mutation 与
///     Claude 结构化状态轮询；所有 mutation 交给统一 apply 路径。
pub async fn spawn_owner_agent_runtime_worker(state: crate::state::AppState) {
    let Some(mut rx) = install_agent_mutation_ingress() else {
        tracing::debug!("agent mutation ingress already installed; skip worker");
        return;
    };
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let registry_ids = state.workbench_sessions.registry_session_ids();
    match collect_alive_terminal_ids(registry_ids, &state.workbench_session_repo).await {
        Ok(alive) => {
            let at = chrono::Utc::now().to_rfc3339();
            match reducer.reconcile_active_sessions(&alive, &at).await {
                Ok(disconnected) if !disconnected.is_empty() => {
                    tracing::info!(
                        "agent runtime reconcile disconnected {} stale sessions",
                        disconnected.len()
                    );
                    for row in &disconnected {
                        // reconcile → Disconnected：无异常通知；previous 未知用 None
                        emit_agent_runtime_changed(&state, row, None);
                        // A9：对账断开也写 Ledger（失败隔离）；同时尝试补记 usage。
                        maybe_note_terminal_usage(&state, row).await;
                        let ledger_svc = state.agent_ledger_service.clone();
                        let row_for_ledger = row.clone();
                        tokio::spawn(async move {
                            crate::workbench::agent_ledger::service::on_agent_runtime_terminal(
                                &ledger_svc,
                                &row_for_ledger,
                                None,
                            )
                            .await;
                        });
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("agent runtime reconcile failed: {e}"),
            }
        }
        Err(e) => tracing::warn!("agent runtime collect alive terminals failed: {e}"),
    }
    let mut claude_status_tick = tokio::time::interval(Duration::from_secs(1));
    claude_status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut unavailable_claude_status_counts = HashMap::new();
    loop {
        // 先冲刷 overflow（channel 曾满时保留的终态），避免轮询让高优先级事件饥饿。
        if let Some(mutation) = take_terminal_mutation_overflow() {
            apply_owner_agent_mutation(&state, &reducer, mutation).await;
            continue;
        }
        tokio::select! {
            biased;
            mutation = rx.recv() => {
                let Some(mutation) = mutation else {
                    if let Some(overflow) = take_terminal_mutation_overflow() {
                        apply_owner_agent_mutation(&state, &reducer, overflow).await;
                    }
                    break;
                };
                apply_owner_agent_mutation(&state, &reducer, mutation).await;
            }
            _ = claude_status_tick.tick() => {
                match claude_status::collect_claude_status_mutations(
                    &reducer,
                    &mut unavailable_claude_status_counts,
                ).await {
                    Ok(mutations) => {
                        for mutation in mutations {
                            apply_owner_agent_mutation(&state, &reducer, mutation).await;
                        }
                    }
                    Err(error) => {
                        tracing::debug!("Claude session 状态对账失败: {error}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::agent_runtime::models::AgentSessionPhase;

    /// 构造最小 mutation 夹具。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     channel 满/overflow 单测只需 phase 与身份字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回固定 id 的 AgentRuntimeMutation。
    fn sample_mutation(phase: AgentSessionPhase, version: u64) -> AgentRuntimeMutation {
        AgentRuntimeMutation {
            agent_session_id: "agent-1".into(),
            terminal_session_id: "term-1".into(),
            expected_version: version.saturating_sub(1),
            event_version: version,
            phase,
            native_session_id: None,
            outcome_code: None,
            occurred_at: "2026-07-01T10:00:00Z".into(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     channel 满时 Completed 不得静默丢弃，否则 runtime 真值卡住 Working。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity=1 填满 Working 后入队 Completed；断言 overflow 持有 Completed。
    #[test]
    fn completed_retained_in_overflow_when_channel_full() {
        // 清理可能残留的 overflow
        let _ = take_terminal_mutation_overflow();
        let (tx, mut rx) = mpsc::channel(1);
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::Working, 1));
        // 再塞一条 Working：Full 且非 priority → drop
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::Working, 2));
        assert!(take_terminal_mutation_overflow().is_none());
        // Completed：Full → overflow
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::Completed, 3));
        let retained = take_terminal_mutation_overflow().expect("Completed must be retained");
        assert_eq!(retained.phase, AgentSessionPhase::Completed);
        assert_eq!(retained.event_version, 3);
        // channel 内仍是第一条 Working
        let first = rx.try_recv().expect("first Working still in channel");
        assert_eq!(first.phase, AgentSessionPhase::Working);
        assert_eq!(first.event_version, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     NeedsInput 与 Failed 同属高优先级，满载时同样进入 overflow。
    #[test]
    fn needs_input_and_failed_are_priority_overflow() {
        let _ = take_terminal_mutation_overflow();
        let (tx, _rx) = mpsc::channel(1);
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::Working, 1));
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::NeedsInput, 2));
        let n = take_terminal_mutation_overflow().unwrap();
        assert_eq!(n.phase, AgentSessionPhase::NeedsInput);
        enqueue_agent_mutation_to(&tx, sample_mutation(AgentSessionPhase::Failed, 3));
        let f = take_terminal_mutation_overflow().unwrap();
        assert_eq!(f.phase, AgentSessionPhase::Failed);
    }
}
