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
//!     Gate A Task 6：projection（durable jobs + atomic writer + scheduler recovery）；
//!     Gate A Task 7：runtime（sidecar owner watch/debounce/ticker/de-loop scan）；
//!     Gate A Task 8：用户级登录自启动（autostart）。
//!     Gate A Task 9：service 门面 + Attention conflict/blocked 投影 + control/commands。
//!     Gate A Task 10：migration（用户 CLAUDE.md seed + N/N+1 dual-write 摘要）。
//!     Gate A fix r1：`projection_ops` 生产投影调度 + `agent_hub.enabled` 武装路径。
//!     Gate B Task 1：`assets` typed portable payload（Skill/Command/Agent/MCP）。
//!     Gate B Task 2：`config_patch` ownership-aware TOML/JSONC 语义 patch。
//!     Gate B Task 3：targets portable scan/render + Claude assets N/N+1 façade。
//!     Gate B Task 4：`support` 版本化 adapter support manifest（fail-closed 写能力）。
//!     Gate B Task 5：`packages` 隔离 managed package 物化 + target activator。
//!     Gate B Task 6：`packages/adoption` legacy standalone 纳管（激活-before-removal，无双发现）。

pub mod assets;
pub mod autostart;
pub mod config_patch;
pub mod instructions;
pub mod migration;
pub mod models;
pub mod object_store;
pub mod packages;
pub mod project_scope;
pub mod projection;
pub mod projection_ops;
pub mod revision_graph;
pub mod runtime;
pub mod service;
pub mod support;
pub mod targets;

pub use assets::{
    canonical_bytes, ensure_kind_matches_payload, from_canonical_bytes, CommandArgument,
    McpTransport, PortableAgent, PortableAssetPayload, PortableCommand, PortableMcpServer,
    PortableSkill,
};
pub use config_patch::{
    apply_config_patch_atomically, parse_owned_path_meta, prepare_config_projection,
    serialize_owned_path_meta, value_content_hash, ConfigOwnedPathMeta, ConfigPatchOutcome,
    ConfigPathDiff, JsoncConfigPatcher, ManagedConfigPatch, OwnedConfigValue, PatchedConfig,
    PreparedConfigProjection, SemanticConfigPatcher, TomlConfigPatcher,
};

pub use instructions::{
    classify_import, compile_render, reconcile_instruction, AgentHubConflictScope,
    CompiledRenderedInstruction, InstructionBlock, InstructionBlockMode,
    InstructionDocument as CompiledInstructionDocument, InstructionReconcileOutcome,
    NewAgentHubConflict, NewInstructionRevision, PortabilityDiagnostic,
    StructuredInstructionIntent,
};
pub use migration::{
    dual_write_legacy_claude_md_summary, migrate_user_claude_md_state,
    migrate_user_claude_md_state_with, resolve_user_claude_md_content, ClaudeMdMigrationPreview,
    MigrationDeps, USER_INSTRUCTION_DISPLAY_NAME, USER_INSTRUCTION_LOGICAL_KEY,
    USER_INSTRUCTION_NAMESPACE, USER_SCOPE_STABLE_ID,
};
pub use models::{
    AdoptionRecord, AdoptionState, AgentHubConflict, AgentTarget, AssetKind, AssetPolicy,
    DesiredPresence, LogicalAsset, Materialization, MaterializationStatus, NewLogicalAsset,
    NewMaterialization, NewProjectionJob, NewRevision, NewScopeNode, NewTargetBinding,
    ProjectionJob, ProjectionJobState, ProjectionPayloadKind, Revision, RevisionId,
    RevisionOperation, RevisionOriginKind, ScopeKind, ScopeNode, TargetBinding,
};
pub use object_store::{
    sha256_hex, ObjectStore, PutTreeResult, StoredObject, TreeEntry, TreeEntryDiagnostic,
    TreeEntryType, TreeManifest,
};
pub use packages::{
    build_package_id, count_opencode_compat_skills, generation_blocked_for_asset,
    mark_pending_legacy_sources, materialize_package, package_materialized_root,
    ActivationInspection, ActivationPlan, ActivationResult, AdoptionEngine, AdoptionFault,
    AdoptionOutcome, AdoptionPreview, AdoptionRequest, ClaudePackageActivator,
    CodexPackageActivator, GeneratedTargetPackage, ManagedPackageActivator,
    OpenCodePackageActivator, PackageBuildInput, PackageMaterializationMeta, PackageSkillInput,
    PLUGIN_SELECTOR,
};
pub use project_scope::{
    build_project_enable_preview, enable_project_scope, refresh_checkout_bindings,
    AgentHubProjectPreview, AgentHubProjectStatus, EnableAgentHubProjectRequest,
    PreviewCheckoutEntry, PreviewPlannedAction, ProjectCheckoutBinding,
};
pub use projection::{
    AtomicProjectionWriter, ProjectionRequest, ProjectionRunStats, ProjectionScheduler,
    ProjectionWriteFault, MAX_GLOBAL_PROJECTION_PARALLELISM,
};
pub use projection_ops::{
    ensure_agent_hub_enabled, schedule_asset_projections, schedule_project_projections,
};
pub use revision_graph::{
    ContentMergeResult, MergeBaseOutcome, MergePayload, RevisionGraph, MAX_VISITED_REVISIONS,
};
pub use runtime::{
    AgentHubRuntime, ChangedDirLedger, DeLoopScanner, DirtyDebouncer, FakeClock, ScanScope,
    ScanStats, CHANGED_DIR_TICK, FULL_SCOPE_TICK, WATCH_DEBOUNCE,
};
pub use service::{
    enable_project_for_state, get_asset_for_state, get_status_for_state, list_assets_for_state,
    pair_instruction_variants_for_state, preview_project_for_state, resolve_conflict_for_state,
    set_target_binding_for_state, update_instruction_block_for_state, update_instruction_for_state,
    AgentHubAssetDetailDto, AgentHubAssetSummaryDto, AgentHubConflictDto,
    AgentHubConflictResolution, AgentHubInstructionBlockDto, AgentHubProbeDto, AgentHubService,
    AgentHubStatusDto, AgentHubTargetBindingDto, AgentHubTargetCellDto, InstructionBlockDto,
    ListAssetsRequest, PairInstructionVariantsRequest, ResolveConflictRequest,
    SetTargetBindingRequest, UpdateInstructionBlockRequest, UpdateInstructionRequest,
};
pub use support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, format_probe_identity,
    load_support_manifest_from_str, parse_semver_core, CapabilitySupport, EvaluatedSupportMode,
    EvaluatedTargetSupport, ExecutableProbeSpec, RuntimeProbeSnapshot, SupportManifest,
    TargetCapability, TargetSupportRecord, SUPPORT_MANIFEST_JSON,
};
pub use targets::{
    AdapterSupportLevel, AssetAdapter, AssetRenderContext, ClaudeInstructionAdapter,
    CodexInstructionAdapter, DiscoveredPortableAsset, InstructionDocument,
    InstructionRenderContext, InstructionSource, InstructionSourceRole, LocalScopeMapping,
    OpenCodeHomePaths, OpenCodeInstructionAdapter, PortableAssetOrigin, PortableDiscoveryStatus,
    PortableOriginKind, ProjectedAssetFile, RenderedInstruction, TargetAssetProjection,
    TargetEnvironment, TargetHomePaths, TargetHomes, TargetPathResolver, TargetProbe,
};
