//! workbench/agent_runtime — provider-neutral Agent session 运行时
//!
//! Business Logic（为什么需要这个模块）:
//!     普通 Workbench 终端与 Orchestrator 需要共享稳定的 Agent session 身份、phase、
//!     version 与 Gap 恢复能力；不能依赖 terminal 字节流或 task 私有 Claude 字段。
//!
//! Code Logic（这个模块做什么）:
//!     导出领域模型、OSC 解码与 mutation ingress；reducer/snapshot 由后续任务叠加。

pub mod models;
pub mod osc;
pub mod reducer;
pub mod snapshot;

pub use models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
pub use osc::{encode_agent_osc_frame, AgentOscDecoder};
pub use reducer::{collect_alive_terminal_ids, AgentReduceOutcome, AgentRuntimeReducer};
pub use snapshot::{
    emit_agent_runtime_changed, get_agent_runtime_snapshot_for_state, AgentRuntimeSnapshot,
    AgentSessionRuntimeDto,
};

use crate::state::AppState;
use std::sync::{Mutex, OnceLock};
use tokio::sync::mpsc;

/// 有界 mutation ingress 容量（OSC 热路径 try_send，满则丢弃并记诊断）。
const AGENT_MUTATION_CHANNEL_CAP: usize = 256;

/// 进程内 mutation 发送端（由 `install_agent_mutation_ingress` 安装）。
static AGENT_MUTATION_TX: OnceLock<mpsc::Sender<AgentRuntimeMutation>> = OnceLock::new();

/// 测试可替换的 send 钩子（默认走 channel）。
static AGENT_MUTATION_TEST_HOOK: Mutex<Option<Box<dyn Fn(AgentRuntimeMutation) + Send>>> =
    Mutex::new(None);

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

/// 非阻塞投递 mutation；未安装 ingress 或 channel 满时丢弃。
///
/// Business Logic（为什么需要这个函数）:
///     PTY reader 在 OS 线程上运行，不能 block 等 DB；满载时丢弃由 snapshot/对账恢复。
///
/// Code Logic（这个函数做什么）:
///     优先 test hook；否则 `try_send`；`state` 预留给后续 metrics（当前未读）。
pub fn try_enqueue_agent_mutation(state: &AppState, mutation: AgentRuntimeMutation) {
    let _ = state;
    if let Ok(guard) = AGENT_MUTATION_TEST_HOOK.lock() {
        if let Some(hook) = guard.as_ref() {
            hook(mutation);
            return;
        }
    }
    if let Some(tx) = AGENT_MUTATION_TX.get() {
        if tx.try_send(mutation).is_err() {
            tracing::debug!("agent mutation ingress full or closed; drop event");
        }
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

/// 启动 owner Agent runtime worker：安装 ingress、对账 active、串行消费 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     HeadlessOwner 进程启动后必须恢复 Agent 真值：幽灵 active → Disconnected，
///     并把 OSC 入站 mutation 串行写入 SQLite。
///
/// Code Logic（这个函数做什么）:
///     install channel → reconcile（registry + running DB sessions）→ loop recv apply。
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
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("agent runtime reconcile failed: {e}"),
            }
        }
        Err(e) => tracing::warn!("agent runtime collect alive terminals failed: {e}"),
    }
    while let Some(mutation) = rx.recv().await {
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
                emit_agent_runtime_changed(&state, &row, Some(previous_phase));
                // H1：OSC 生产路径 dual-write task.last_activity_at，供 stall watchdog 使用。
                if let (Some(task_id), Some(attempt)) =
                    (row.orchestrator_task_id.as_deref(), row.orchestrator_attempt)
                {
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
                            "touch_task_last_activity after OSC apply failed: {err}"
                        );
                    }
                }
                // HookEvent completion：runtime Completed 时按 attempt 冻结合同推进 Verifying。
                if row.phase == AgentSessionPhase::Completed
                    && row.orchestrator_task_id.is_some()
                {
                    if let Err(err) = crate::orchestrator::completion::maybe_complete_from_agent_runtime_completed(
                        &state,
                        &row,
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
}
