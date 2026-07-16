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

pub use models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
pub use osc::{encode_agent_osc_frame, AgentOscDecodeResult, AgentOscDecoder, AgentOscDiagnostic};

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
