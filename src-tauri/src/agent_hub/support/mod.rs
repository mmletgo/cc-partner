//! agent_hub/support — 版本化 adapter 支持合同（Gate B Task 4）
//!
//! Business Logic（为什么需要这个模块）:
//!     缺少 exact min/current tested version 或 quality evidence 时，写能力必须 blocked，
//!     禁止用“命令退出码碰巧为 0”猜测激活/投影命令；runtime 不得改写编译期 manifest。
//!
//! Code Logic（这个模块做什么）:
//!     以 `include_str!` 编译 `support-manifest.json`；解析为 `TargetSupportRecord`；
//!     按版本/指纹/evidence 对每个 `TargetCapability` 做 fail-closed 求值。

pub mod manifest;

pub use manifest::{
    builtin_support_manifest, evaluate_target_support, find_target_record, format_probe_identity,
    load_support_manifest_from_str, parse_semver_core, CapabilitySupport, EvaluatedSupportMode,
    EvaluatedTargetSupport, ExecutableProbeSpec, RuntimeProbeSnapshot, SupportManifest,
    TargetCapability, TargetSupportRecord, SUPPORT_MANIFEST_JSON,
};
