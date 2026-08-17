//! Agent Adapter Platform — provider-neutral probe/launch/resume/normalize。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator Runner 不能硬编码 Claude；需要 owner-local adapter registry
//!     承载 Claude/Codex/generic/OpenCode terminal，同时保持 claim/worktree/terminal 状态机在 Runner。
//!
//! Code Logic（这个模块做什么）:
//!     导出类型、registry 与各内置 adapter；probe/launch plan 不经 P2P 传 path/env/credential。

pub mod claude_code;
pub mod codex;
pub mod cursor_cli;
pub mod gemini_cli;
pub mod generic_terminal;
pub mod grok_build;
pub mod opencode;
pub mod registry;
pub mod types;

pub use claude_code::ClaudeCodeAdapter;
pub use codex::CodexAdapter;
pub use generic_terminal::{GenericTerminalAdapter, GenericTerminalConfig};
pub use opencode::{
    ensure_opencode_bridge_verified, opencode_preflight_block_reason, OpenCodeAdapter,
    REASON_EXTERNAL_COLLISION, REASON_L3_RUNTIME_EVIDENCE_MISSING, REASON_PROVIDER_UNAVAILABLE,
    REASON_RUNTIME_BRIDGE_REQUIRED, REASON_UNSUPPORTED_CLI_VERSION,
};
pub use registry::{
    render_terminal_command, AgentAdapter, AgentAdapterRegistry, AgentAvailability,
    AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult, AgentUsageDelta, NativeAgentEvent,
    ResumeTerminalPolicy, TerminalShellDialect,
};
pub use types::{
    resolve_task_runner_policy, validate_max_turns, validate_stall_timeout_ms,
    AgentCompletionContract, AgentProviderId, RunnerAttemptPolicy, DEFAULT_MAX_TURNS,
    DEFAULT_STALL_TIMEOUT_MS, MAX_MAX_TURNS, MAX_STALL_TIMEOUT_MS, MIN_MAX_TURNS,
    MIN_STALL_TIMEOUT_MS,
};
