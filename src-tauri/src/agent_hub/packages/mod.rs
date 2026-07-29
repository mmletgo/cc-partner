//! agent_hub/packages — 隔离 managed package 物化与 target 激活
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate B 要求 Claude/Codex 受管 Skill 终态只落在 cc-partner 生成的 Plugin，
//!     OpenCode 写原生 `skills/commands/agents`；禁止把 managed 输出写进
//!     `.claude/skills` / `.agents/skills`（否则 OpenCode 兼容扫描会重复发现）。
//!
//! Code Logic（这个模块做什么）:
//!     `builder` 确定性生成 package 布局并原子落盘到
//!     `<data_dir>/agent-hub/materialized-packages/<target>/<scope>/<package-id>/`；
//!     `activator` 按 support manifest 构造 argv 计划并通过可注入 ProcessRunner 执行；
//!     导出激活 DTO 与 `ManagedPackageActivator` 合同。

pub mod activator;
pub mod builder;

pub use activator::{
    ActivationInspection, ActivationPlan, ActivationResult, ActivationStep, ArgvPlan,
    ClaudePackageActivator, CodexPackageActivator, FakeProcessRunner, ManagedPackageActivator,
    OpenCodePackageActivator, ProcessOutcome, ProcessRunner, ProcessSpec,
};
pub use builder::{
    build_package_id, materialize_package, package_materialized_root, GeneratedTargetPackage,
    PackageBuildInput, PackageMaterializationMeta, PackageSkillInput, MARKETPLACE_NAME,
    PLUGIN_NAME, PLUGIN_SELECTOR,
};
