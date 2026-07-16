//! Agent Adapter Platform — provider-neutral probe/launch/resume/normalize。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator Runner 不能硬编码 Claude；需要 owner-local adapter registry
//!     承载 Claude/Codex/generic terminal，同时保持 claim/worktree/terminal 状态机在 Runner。
//!
//! Code Logic（这个模块做什么）:
//!     导出类型与后续 registry/adapters；probe/launch plan 不经 P2P 传 path/env/credential。

pub mod types;

pub use types::{
    resolve_task_runner_policy, validate_max_turns, validate_stall_timeout_ms, AgentCompletionContract,
    AgentProviderId, RunnerAttemptPolicy, DEFAULT_MAX_TURNS, DEFAULT_STALL_TIMEOUT_MS,
    MAX_MAX_TURNS, MAX_STALL_TIMEOUT_MS, MIN_MAX_TURNS, MIN_STALL_TIMEOUT_MS,
};
