//! agent_hub — Multi-CLI Agent Hub 领域模块
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Claude Code / Codex CLI / OpenCode 之间维护指令与资产时，需要一个可崩溃恢复的
//!     Canonical Hub 作为权威源，避免各 CLI 本地文件各自漂移。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task 1：canonical 数据模型（models）；
//!     Gate A Task 2：明文 CAS（object_store）与 Revision DAG merge-base（revision_graph）。
//!     后续任务再组装 service、projection 与 target adapter。

pub mod models;
pub mod object_store;
pub mod revision_graph;

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
pub use revision_graph::{
    ContentMergeResult, MergeBaseOutcome, MergePayload, RevisionGraph, MAX_VISITED_REVISIONS,
};
