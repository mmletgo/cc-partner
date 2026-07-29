//! agent_hub/plugins — Canonical PluginPackage 分解与固定 revision 引用
//!
//! Business Logic（为什么需要这个模块）:
//!     Plugin 不是最低同步单位：须拆成 Skill/MCP/Command/Agent/Hook 与 residual runtime，
//!     并以固定 component revision ref 进入同一 Revision DAG / CAS / Snapshot 路径。
//!
//! Code Logic（这个模块做什么）:
//!     Gate D Task 1：typed `PluginPackagePayload` / Hook / residual 模型；
//!     Gate D Task 2：`decompose` 检视/import 与 `ownership` 引用派生删除决策。

pub mod decompose;
pub mod models;
pub mod ownership;

pub use decompose::{
    ensure_preview_skills_in_cas, import_confirmed, inspect_plugin_source, ComponentPayloadPreview,
    ComponentPortability, ComponentPreview, ConfirmedPluginDecomposition, DefaultPluginDecomposer,
    DiscoveredPluginSource, PluginDecomposer, PluginDecompositionPreview, PluginPackageRevision,
    ResidualPreview,
};
pub use models::{
    canonical_plugin_package_bytes, canonical_portable_hook_bytes, ensure_component_kind_allowed,
    from_plugin_package_bytes, from_portable_hook_bytes, sort_plugin_package_payload,
    validate_plugin_package_payload, validate_portable_hook, ComponentOwnership, HookEventIntent,
    PluginComponentRef, PluginPackagePayload, PluginResidualRef, PortableHook, ResidualKind,
};
pub use ownership::{decide_component_delete, ComponentDeleteDecision};
