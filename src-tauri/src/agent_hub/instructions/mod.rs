//! agent_hub/instructions — Instruction Compiler 与三方 Reconciler
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 需要把目录级 Markdown 指令编译为稳定块，
//!     按 shared/adapted/targetOnly 投影到 Claude/Codex/OpenCode，
//!     并对外部编辑做三方对账。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task 4：document 块模型、compiler 切块/分类/渲染、reconcile 三方合并。

pub mod compiler;
pub mod document;
pub mod reconcile;

pub use compiler::{
    ancestor_agent_paths_for_directory, block_needs_target_isolation, classify_import,
    compile_render, parse_markdown_blocks, render_discovery_before_edit, render_opencode_prelude,
    CompiledRenderedInstruction, ImportClassification, ImportScopeContext, ParsedMarkdownBlock,
    TargetMarkdownSource,
};
pub use document::{
    new_block_id, AgentHubConflictScope, InstructionBlock, InstructionBlockMode,
    InstructionDocument, NewAgentHubConflict, NewInstructionRevision, PortabilityDiagnostic,
    RenderedBlockRange, StructuredInstructionIntent,
};
pub use reconcile::{
    reconcile_against_rendered, reconcile_instruction, BaseBlockRecord, ExternalObservation,
    InstructionReconcileOutcome, ReconcileInput,
};
