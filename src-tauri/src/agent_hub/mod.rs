//! agent_hub — Multi-CLI Agent Hub 领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Claude Code / Codex CLI / OpenCode 之间维护指令与资产时，需要一个可崩溃恢复的
//!     Canonical Hub 作为权威源，避免各 CLI 本地文件各自漂移。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task1 models + Task5 project_scope（opt-in preview/enable/refresh checkout bindings）。
//!     后续任务再组装 projection、runtime 与 target adapter。

pub mod models;
pub mod project_scope;

pub use models::{
    AgentHubConflict, AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset,
    Materialization, MaterializationStatus, NewLogicalAsset, NewRevision, NewScopeNode,
    NewTargetBinding, Revision, RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
    ScopeNode, TargetBinding,
};
pub use project_scope::{
    build_project_enable_preview, enable_project_scope, refresh_checkout_bindings,
    AgentHubProjectPreview, AgentHubProjectStatus, EnableAgentHubProjectRequest,
    PreviewCheckoutEntry, PreviewPlannedAction, ProjectCheckoutBinding,
};
