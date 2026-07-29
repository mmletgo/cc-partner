//! agent_hub — Multi-CLI Agent Hub 领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Claude Code / Codex CLI / OpenCode 之间维护指令与资产时，需要一个可崩溃恢复的
//!     Canonical Hub 作为权威源，避免各 CLI 本地文件各自漂移。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task 1：canonical 数据模型（models）；
//!     Gate A Task 2：明文 CAS（object_store）与 Revision DAG merge-base（revision_graph）；
//!     Gate A Task 3：targets（path resolver + instruction-only AssetAdapter 合同）；
//!     Gate A Task 4：instructions（块编译 / OpenCode prelude / 三方 reconcile）；
//!     Gate A Task 5：project_scope（opt-in preview/enable/refresh checkout bindings）；
//!     Gate A Task 8：用户级登录自启动（autostart）。
//!     后续任务再组装 service、projection、runtime。

pub mod autostart;
pub mod instructions;
pub mod models;
pub mod object_store;
pub mod project_scope;
pub mod revision_graph;
pub mod targets;

pub use instructions::{
    classify_import, compile_render, reconcile_instruction, AgentHubConflictScope,
    CompiledRenderedInstruction, InstructionBlock, InstructionBlockMode,
    InstructionDocument as CompiledInstructionDocument, InstructionReconcileOutcome,
    NewAgentHubConflict, NewInstructionRevision, PortabilityDiagnostic,
    StructuredInstructionIntent,
};
pub use models::{
    AgentHubConflict, AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset,
    Materialization, MaterializationStatus, NewLogicalAsset, NewRevision, NewScopeNode,
    NewTargetBinding, Revision, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
    ScopeNode, TargetBinding,
};
pub use object_store::{
    sha256_hex, ObjectStore, PutTreeResult, StoredObject, TreeEntry, TreeEntryDiagnostic,
    TreeEntryType, TreeManifest,
};
pub use project_scope::{
    build_project_enable_preview, enable_project_scope, refresh_checkout_bindings,
    AgentHubProjectPreview, AgentHubProjectStatus, EnableAgentHubProjectRequest,
    PreviewCheckoutEntry, PreviewPlannedAction, ProjectCheckoutBinding,
};
pub use revision_graph::{
    ContentMergeResult, MergeBaseOutcome, MergePayload, RevisionGraph, MAX_VISITED_REVISIONS,
};
pub use targets::{
    AdapterSupportLevel, AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter,
    InstructionDocument, InstructionRenderContext, InstructionSource, InstructionSourceRole,
    LocalScopeMapping, OpenCodeHomePaths, OpenCodeInstructionAdapter, RenderedInstruction,
    TargetEnvironment, TargetHomePaths, TargetHomes, TargetPathResolver, TargetProbe,
};
