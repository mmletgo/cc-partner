//! workbench/agent_runtime — provider-neutral Agent session 运行时
//!
//! Business Logic（为什么需要这个模块）:
//!     普通 Workbench 终端与 Orchestrator 需要共享稳定的 Agent session 身份、phase、
//!     version 与 Gap 恢复能力；不能依赖 terminal 字节流或 task 私有 Claude 字段。
//!
//! Code Logic（这个模块做什么）:
//!     导出领域模型；后续任务再叠加 OSC 解码、reducer、snapshot 投影。

pub mod models;

pub use models::{
    AgentRuntimeMutation, AgentSessionPhase, AgentSessionRuntime, CreateActiveAgentSession,
};
