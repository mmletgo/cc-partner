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
//!     Gate D Task 7：Plugin 幂等迁移 preview/confirm + LegacyAgentAssetCompatibilityStatus + N+2 门闩。
//!     Gate A fix r1：`projection_ops` 生产投影调度 + `agent_hub.enabled` 武装路径。
//!     Gate B Task 1：`assets` typed portable payload（Skill/Command/Agent/MCP）。
//!     Gate B Task 2：`config_patch` ownership-aware TOML/JSONC 语义 patch。
//!     Gate B Task 3：targets portable scan/render + Claude assets N/N+1 façade。
//!     Gate B Task 4：`support` 版本化 adapter support manifest（fail-closed 写能力）。
//!     Gate B Task 5：`packages` 隔离 managed package 物化 + target activator。
//!     Gate B Task 6：`packages/adoption` legacy standalone 纳管（激活-before-removal，无双发现）。
//!     Gate B Task 7：target presence/enabled/detach/delete 语义与聚合状态。
//!     Gate C Task 1：`snapshot` SnapshotEnvelope v1 + RFC8785 兼容 canonical JSON 子集。
//!     Gate C Task 2：`snapshot/builder` + `snapshot/archive` 确定性导出与可读 archive 展开/重打包。
//!     Gate C Task 3：`snapshot/importer` 两阶段导入 lineage/alias/head（MCA 合并，禁止 LWW）。
//!     Gate C Task 4：`replication` LAN push 接收端 + 幂等 ledger（prepare/objects/commit）。
//!     Gate C Task 5：`replication/sender` 源侧 multi-target LAN push + source ledger + Attention。
//!     Gate C Task 6：`git` 本机 device-lane 自动备份导出（CloudSyncRuntime 单飞，不自动 import 远端 lane）。
//!     Gate D Task 1：`plugins` 不可变 PluginPackage/Hook/residual schema 与边表引用。
//!     Gate D Task 3：`plugins/hook_mapping` + `plugins/render` 证据化 Hook 映射与 package 投影。

pub mod assets;
pub mod autostart;
pub mod config_patch;
pub mod cross_agent;
pub mod cross_agent_full;
pub mod git;
pub mod instructions;
pub mod migration;
pub mod models;
pub mod object_store;
pub mod packages;
pub mod plugins;
pub mod portable_actions;
pub mod portable_inventory;
pub mod portable_service;
pub mod project_scope;
pub mod projection;
pub mod projection_ops;
pub mod remote_client;
pub mod replication;
pub mod revision_graph;
pub mod runtime;
pub mod service;
pub mod snapshot;
pub mod support;
pub mod targets;
pub mod user_instructions;

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
pub use cross_agent::{
    apply_cross_agent_instruction, preview_cross_agent_instruction,
    preview_cross_agent_plugin_residual, should_enqueue_cross_target_on_external_edit,
    ApplyCrossAgentInstructionRequest, CrossAgentAdaptMode, CrossAgentApplyTargetResult,
    CrossAgentKind, CrossAgentPreviewReport, CrossAgentTargetPreview,
    PreviewCrossAgentInstructionRequest,
};
pub use cross_agent_full::{
    apply_cross_agent_full, apply_cross_agent_full_default, preview_cross_agent_full,
    preview_cross_agent_full_default, ApplyCrossAgentFullRequest, CrossAgentFullApplyItemResult,
    CrossAgentFullApplySelection, CrossAgentFullPlan, CrossAgentFullPlanItem,
    CrossAgentFullPortableRef, CrossAgentFullSnapshot, FullAdaptRunner,
    PreviewCrossAgentFullRequest, StubFullAdaptRunner, FULL_ADAPT_GENERATOR_STUB,
};

pub use instructions::{
    classify_import, compile_render, reconcile_instruction, AgentHubConflictScope,
    CompiledRenderedInstruction, InstructionBlock, InstructionBlockMode,
    InstructionDocument as CompiledInstructionDocument, InstructionReconcileOutcome,
    NewAgentHubConflict, NewInstructionRevision, PortabilityDiagnostic,
    StructuredInstructionIntent,
};
pub use migration::{
    confirm_plugin_migration_import, downgrade_compatibility_facade_snapshot,
    dual_write_legacy_claude_md_summary, legacy_agent_asset_compatibility_status,
    legacy_facade_policy, migrate_user_claude_md_state, migrate_user_claude_md_state_with,
    n_plus_two_removal_allowed, preview_plugin_migration, resolve_user_claude_md_content,
    seed_user_instruction_if_head_null, version_cmp, ClaudeMdMigrationPreview,
    DowngradeFacadeSnapshot, LegacyAgentAssetCompatibilityStatus, LegacyFacadePolicy,
    MigrationDeps, PluginMigrationConfirmResult, PluginMigrationPreview,
    PluginMigrationPreviewItem, SeedUserInstructionOutcome, AGENT_HUB_GA_VERSION,
    EARLIEST_LEGACY_REMOVAL_VERSION, STABLE_MIGRATION_EVIDENCE_ID, USER_INSTRUCTION_DISPLAY_NAME,
    USER_INSTRUCTION_LOGICAL_KEY, USER_INSTRUCTION_NAMESPACE, USER_SCOPE_STABLE_ID,
};
pub use models::{
    compute_asset_aggregate_status, AdoptionRecord, AdoptionState, AgentHubConflict, AgentTarget,
    AssetAggregateStatus, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset, Materialization,
    MaterializationStatus, NewLogicalAsset, NewMaterialization, NewProjectionJob, NewRevision,
    NewScopeNode, NewTargetBinding, PortableActionClaim, PortableAssetActionPlanRecord,
    ProjectionJob, ProjectionJobState, ProjectionPayloadKind, Revision, RevisionId,
    RevisionOperation, RevisionOriginKind, ScopeKind, ScopeNode, TargetBinding,
    TargetBindingIntent, TargetBindingTransition, TargetDisableStrategy, TargetStatusSnapshot,
    UserInstructionOwnershipRecord, UserInstructionPlanClaim, UserInstructionPlanRecord,
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
    OpenCodePackageActivator, PackageAgentInput, PackageBuildInput, PackageCommandInput,
    PackageMaterializationMeta, PackageSkillInput, PLUGIN_SELECTOR,
};
pub use plugins::{
    builtin_hook_mapping_registry, canonical_plugin_package_bytes, canonical_portable_hook_bytes,
    decide_component_delete, ensure_component_kind_allowed, ensure_preview_skills_in_cas,
    evaluate_hook_mapping, from_plugin_package_bytes, from_portable_hook_bytes,
    hook_mapping_registry_from_manifest, import_confirmed, inspect_plugin_source,
    merge_activation_into_report, project_plugin_package, render_component_for_target,
    sort_plugin_package_payload, validate_plugin_package_payload, validate_portable_hook,
    ComponentDeleteDecision, ComponentOwnership, ComponentPayloadPreview, ComponentPortability,
    ComponentPreview, ComponentProjectionReport, ComponentTargetStatus,
    ConfirmedPluginDecomposition, DefaultPluginDecomposer, DiscoveredPluginSource, HookEventIntent,
    HookMappingDecision, HookMappingRecord, HookTrustModel, PackageAggregateStatus,
    PackageProjectionReport, PackageRenderInput, PluginComponentRef, PluginDecomposer,
    PluginDecompositionPreview, PluginPackagePayload, PluginPackageRevision, PluginResidualRef,
    PortableHook, ResidualKind, ResidualPreview, ResidualProjectionReport,
    ResolvedComponentPayload,
};
pub use portable_actions::{
    apply_portable_asset_action, apply_portable_asset_action_with, claim_portable_asset_action,
    preview_portable_asset_action, preview_portable_asset_action_with_inventory,
    ApplyPortableAssetActionRequest, PortableActionExecutorDeps, PortableAssetActionChangeDto,
    PortableAssetActionItemResultDto, PortableAssetActionItemState, PortableAssetActionKind,
    PortableAssetActionPlanDto, PortableAssetActionResultDto, PortableAssetBackupPolicy,
    PortableAssetCanonicalEffect, PortableAssetConflictPolicy, PortableAssetPlanOperation,
    PreviewPortableAssetActionRequest, PLAN_TTL_MINUTES,
};
pub use portable_inventory::{
    ensure_discovered_portable_items_managed, inspect_portable_inventory,
    inspect_portable_inventory_with_env, inventory_item_id, inventory_snapshot_hash,
    reconcile_portable_inventory, reconcile_portable_inventory_with_facts,
    scan_portable_inventory_facts, EnsureManagedFailure, EnsureManagedReport, PortableAssetKind,
    PortableCanonicalFact, PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryMutationCapability, PortableInventoryQuery,
    PortableInventoryScanCapability, PortableInventorySnapshotDto, PortableInventorySourceOrigin,
    PortableInventoryTargetDto, PortableMcpCredentialFactDto, PortableScanScope,
};
pub use portable_service::PortableService;
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
    ensure_agent_hub_enabled, schedule_asset_projections, schedule_package_deactivation,
    schedule_project_projections,
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
    AgentHubStatusDto, AgentHubTargetBindingDto, AgentHubTargetCellDto,
    DeleteAssetEverywhereRequest, InstructionBlockDto, ListAssetsRequest,
    PairInstructionVariantsRequest, ResolveConflictRequest, RestoreDetachedTargetRequest,
    SetTargetBindingRequest, SetTargetEnabledRequest, SetTargetPresenceRequest,
    UpdateInstructionBlockRequest, UpdateInstructionRequest,
};
pub use support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, format_probe_identity,
    load_support_manifest_from_str, parse_semver_core, CapabilitySupport, EvaluatedSupportMode,
    EvaluatedTargetSupport, ExecutableProbeSpec, RuntimeProbeSnapshot, SupportManifest,
    TargetCapability, TargetSupportRecord, SUPPORT_MANIFEST_JSON,
};
pub use targets::{
    AdapterSupportLevel, AssetAdapter, AssetRenderContext, ClaudeInstructionAdapter,
    CodexInstructionAdapter, CursorInstructionAdapter, DiscoveredPortableAsset,
    GeminiInstructionAdapter, GrokInstructionAdapter, InstructionDocument,
    InstructionRenderContext, InstructionSource, InstructionSourceRole, LocalScopeMapping,
    OpenCodeHomePaths, OpenCodeInstructionAdapter, PiInstructionAdapter, PortableAssetOrigin,
    PortableAssetOwner, PortableDiscoveryStatus, PortableOriginKind, ProjectedAssetFile,
    RenderedInstruction, TargetAssetProjection, TargetEnvironment, TargetHomePaths, TargetHomes,
    TargetPathResolver, TargetProbe,
};
pub use user_instructions::{
    apply_user_instruction_plan, inspect_user_instruction_workspace,
    preview_user_instruction_setup, preview_user_instruction_update,
    AdaptInstructionToOtherAgentsRequest, AdaptInstructionToOtherAgentsResult,
    AnalyzeInstructionOriginalRequest, AnalyzeInstructionOriginalResult,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, ReviseInstructionSlotRequest, ReviseInstructionSlotResult,
    UserInstructionAction, UserInstructionCanonicalDto, UserInstructionManagementMode,
    UserInstructionPlanDto, UserInstructionTargetDto, UserInstructionWorkspaceDto,
};
