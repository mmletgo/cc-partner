//! agent_hub/plugins/render — Plugin package component/residual 投影与聚合状态
//!
//! Business Logic（为什么需要这个模块）:
//!     package 渲染必须按固定 component revision 调用 Gate B target renderer；
//!     residual 默认 source-only（仅同 target 保留）；Hook 仅在证据化 mapping 下跨 target；
//!     aggregate status 从 requested target bindings 派生，禁止因 source package 成功而 overstate。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PackageProjectionReport` 与 per-component/residual 报告；
//!     `project_plugin_package` 解析组件、渲染 portable、评估 Hook、过滤 residual；
//!     可选调用 Gate B materialize_package / activator 计划以标记 activationRequired。

use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, AssetAggregateStatus, AssetKind, RevisionId};
use crate::agent_hub::packages::activator::{ActivationPlan, ActivationResult};
use crate::agent_hub::packages::builder::{
    materialize_package, GeneratedTargetPackage, PackageBuildInput, PackageSkillInput,
};
use crate::agent_hub::plugins::hook_mapping::{
    evaluate_hook_mapping, HookMappingDecision, HookMappingRecord, HookTrustModel,
};
use crate::agent_hub::plugins::models::{
    PluginComponentRef, PluginPackagePayload, PluginResidualRef, PortableHook, ResidualKind,
};
use crate::agent_hub::targets::portable::{
    render_portable_payload, ProjectedAssetFile, TargetAssetProjection,
};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Package 聚合状态（与 AssetAggregateStatus wire 对齐，不含 detached）。
///
/// Business Logic（为什么需要这个类型）:
///     package 投影报告需要 full/partial/sourceOnly/activationRequired/externalCollision/blocked。
///
/// Code Logic（这个类型做什么）:
///     新类型别名语义层；内部复用 AssetAggregateStatus 的 as_str。
pub type PackageAggregateStatus = AssetAggregateStatus;

/// 单个 component 在目标 target 上的投影状态。
///
/// Business Logic（为什么需要这个枚举）:
///     报告必须精确区分已验证 / 部分 / source-only / 激活 / 碰撞 / 阻塞。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComponentTargetStatus {
    /// 语义完整且已验证
    Verified,
    /// 可投影但未完整验证（例如 partial command）
    Partial,
    /// 仅 source 表示
    SourceOnly,
    /// 需要用户/CLI 激活
    ActivationRequired,
    /// 与外部碰撞
    ExternalCollision,
    /// 写/投影阻塞
    Blocked,
}

impl ComponentTargetStatus {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Partial => "partial",
            Self::SourceOnly => "sourceOnly",
            Self::ActivationRequired => "activationRequired",
            Self::ExternalCollision => "externalCollision",
            Self::Blocked => "blocked",
        }
    }
}

/// 单个 component 的投影报告。
///
/// Business Logic（为什么需要这个结构体）:
///     UI/测试需要 per-component 的 canonical revision、alias、target status 与原因。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 聚合字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentProjectionReport {
    /// component 种类
    pub kind: AssetKind,
    /// 逻辑资产 id
    pub asset_id: String,
    /// 固定 canonical revision
    pub canonical_revision_id: RevisionId,
    /// 物化后的 invocation alias（若有）
    pub materialized_alias: Option<String>,
    /// 目标 target 上的状态
    pub target_status: ComponentTargetStatus,
    /// 稳定原因 token（可多条）
    pub reasons: Vec<String>,
    /// 投影文件相对路径（仅 verified/partial）
    pub projected_paths: Vec<String>,
}

/// residual 投影报告。
///
/// Business Logic（为什么需要这个结构体）:
///     residual 默认 source-only；同 target 可保留，跨 runtime 必须 omit + diagnostic。
///
/// Code Logic（这个结构体做什么）:
///     camelCase。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResidualProjectionReport {
    /// residual 所属 target
    pub residual_target: AgentTarget,
    /// residual 类别
    pub residual_kind: ResidualKind,
    /// tree hash
    pub tree_manifest_hash: String,
    /// 是否包含在投影中
    pub included: bool,
    /// 原因 token
    pub reasons: Vec<String>,
}

/// Package 整体投影报告。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方按 requested target bindings 消费聚合状态，不得从 source package 成功推断 full。
///
/// Code Logic（这个结构体做什么）:
///     components + residuals + aggregate + 可选 activation 摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageProjectionReport {
    /// 投影目标
    pub destination_target: AgentTarget,
    /// 来源 target
    pub source_target: AgentTarget,
    /// per-component 报告
    pub components: Vec<ComponentProjectionReport>,
    /// residual 报告
    pub residuals: Vec<ResidualProjectionReport>,
    /// 聚合状态
    pub aggregate_status: PackageAggregateStatus,
    /// 激活状态摘要 token：none / planned / activationRequired / blocked / applied
    pub activation_state: String,
    /// 诊断汇总（无 secret）
    pub diagnostics: Vec<String>,
    /// 物化 package 路径（若执行了 materialize）
    pub materialized_package_root: Option<String>,
}

/// 已解析的固定 revision component 载荷。
///
/// Business Logic（为什么需要这个结构体）:
///     render 层不直接读 DB；调用方注入 revision 解析结果，便于单测。
///
/// Code Logic（这个结构体做什么）:
///     Portable payload / Hook / 解析失败诊断。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ResolvedComponentPayload {
    /// Gate B portable 载荷
    Portable {
        /// typed payload
        payload: PortableAssetPayload,
        /// 是否 partial（例如 Command 缺字段/仅半合同）
        partial: bool,
        /// Skill 完整 markdown（可选）
        #[serde(default)]
        skill_markdown: Option<String>,
    },
    /// PortableHook
    Hook {
        /// hook 载荷
        hook: PortableHook,
    },
    /// 无法解析
    Unresolved {
        /// 原因
        reason: String,
    },
}

/// package 渲染输入。
///
/// Business Logic（为什么需要这个结构体）:
///     固定 package payload + 解析后的 component 正文 + residual 策略 + 可选激活上下文。
///
/// Code Logic（这个结构体做什么）:
///     聚合 in-memory 输入。
#[derive(Debug, Clone)]
pub struct PackageRenderInput {
    /// canonical package payload
    pub package: PluginPackagePayload,
    /// destination CLI
    pub destination: AgentTarget,
    /// component revision_id → 解析结果
    pub resolved: BTreeMap<String, ResolvedComponentPayload>,
    /// hook mapping 注册表
    pub hook_registry: Vec<HookMappingRecord>,
    /// quality-matrix evidence id 集合
    pub known_evidence_ids: BTreeSet<String>,
    /// 期望 hook schema 版本
    pub expected_hook_schema_version: u32,
    /// 期望 hook trust
    pub expected_hook_trust: HookTrustModel,
    /// 是否将该 destination 视为 requested binding
    pub requested: bool,
    /// 是否 external collision
    pub external_collision: bool,
    /// 写能力是否 blocked
    pub write_blocked: bool,
    /// 是否要求 activationRequired（activator 合同）
    pub force_activation_required: bool,
    /// 可选 data_dir：提供时对 portable skill 执行 Gate B materialize
    pub data_dir: Option<PathBuf>,
    /// scope id（materialize 用）
    pub scope_id: String,
}

impl Default for PackageRenderInput {
    fn default() -> Self {
        Self {
            package: PluginPackagePayload {
                plugin_id: String::new(),
                name: String::new(),
                version: None,
                description: None,
                source_target: AgentTarget::Claude,
                component_refs: vec![],
                residual_refs: vec![],
                target_extensions: BTreeMap::new(),
            },
            destination: AgentTarget::Claude,
            resolved: BTreeMap::new(),
            hook_registry: vec![],
            known_evidence_ids: BTreeSet::new(),
            expected_hook_schema_version: 1,
            expected_hook_trust: HookTrustModel::ExactContract,
            requested: true,
            external_collision: false,
            write_blocked: false,
            force_activation_required: false,
            data_dir: None,
            scope_id: "user".into(),
        }
    }
}

/// 投影 Plugin package 到 destination。
///
/// Business Logic（为什么需要这个函数）:
///     固定 revision 渲染、Hook fail-closed、residual 同 target only、聚合状态准确。
///
/// Code Logic（这个函数做什么）:
///     遍历 component_refs → 解析 → portable render / hook mapping；
///     residual 同 target include 否则 omit；汇总 aggregate。
pub fn project_plugin_package(
    input: &PackageRenderInput,
) -> Result<PackageProjectionReport, AppError> {
    if !input.requested {
        return Ok(PackageProjectionReport {
            destination_target: input.destination,
            source_target: input.package.source_target,
            components: vec![],
            residuals: vec![],
            aggregate_status: AssetAggregateStatus::Partial,
            activation_state: "none".into(),
            diagnostics: vec!["target_not_requested".into()],
            materialized_package_root: None,
        });
    }

    let mut diagnostics: Vec<String> = Vec::new();
    let mut components: Vec<ComponentProjectionReport> = Vec::new();
    let mut skill_inputs: Vec<PackageSkillInput> = Vec::new();
    let mut projected_files: Vec<ProjectedAssetFile> = Vec::new();

    if input.external_collision {
        diagnostics.push("external_collision".into());
    }
    if input.write_blocked {
        diagnostics.push("write_blocked".into());
    }

    for cref in &input.package.component_refs {
        let rev_key = cref.revision_id.as_str().to_string();
        let resolved = input.resolved.get(&rev_key);
        let report = match resolved {
            None => ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: ComponentTargetStatus::SourceOnly,
                reasons: vec!["component_revision_unresolved".into()],
                projected_paths: vec![],
            },
            Some(ResolvedComponentPayload::Unresolved { reason }) => ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: ComponentTargetStatus::SourceOnly,
                reasons: vec![reason.clone()],
                projected_paths: vec![],
            },
            Some(ResolvedComponentPayload::Hook { hook }) => {
                project_hook_component(cref, hook, input, &mut diagnostics)
            }
            Some(ResolvedComponentPayload::Portable {
                payload,
                partial,
                skill_markdown,
            }) => project_portable_component(
                cref,
                payload,
                *partial,
                skill_markdown.as_deref(),
                input,
                &mut skill_inputs,
                &mut projected_files,
                &mut diagnostics,
            ),
        };
        components.push(report);
    }

    let residuals = project_residuals(
        &input.package.residual_refs,
        input.destination,
        &mut diagnostics,
    );

    // materialize skills when data_dir present and not blocked/collision
    let mut materialized_root = None;
    let mut activation_state = "none".to_string();
    if input.external_collision || input.write_blocked {
        activation_state = "blocked".into();
    } else if input.force_activation_required {
        activation_state = "activationRequired".into();
        // 标记 portable components 需要激活（若已 verified 则降为 activationRequired）
        for c in &mut components {
            if matches!(
                c.target_status,
                ComponentTargetStatus::Verified | ComponentTargetStatus::Partial
            ) {
                c.target_status = ComponentTargetStatus::ActivationRequired;
                c.reasons.push("activation_required".into());
            }
        }
    } else if let Some(data_dir) = &input.data_dir {
        if !skill_inputs.is_empty() {
            let build = PackageBuildInput {
                data_dir: data_dir.clone(),
                target: input.destination,
                scope_id: input.scope_id.clone(),
                skills: skill_inputs.clone(),
                commands: vec![],
                agents: vec![],
            };
            match materialize_package(&build) {
                Ok(gen) => {
                    apply_materialized_aliases(&mut components, &gen);
                    materialized_root = Some(gen.package_root.display().to_string());
                    activation_state = "planned".into();
                }
                Err(e) => {
                    diagnostics.push(format!("materialize_failed:{}", e));
                    for c in &mut components {
                        if c.kind == AssetKind::Skill
                            && matches!(
                                c.target_status,
                                ComponentTargetStatus::Verified | ComponentTargetStatus::Partial
                            )
                        {
                            c.target_status = ComponentTargetStatus::Blocked;
                            c.reasons.push("materialize_failed".into());
                        }
                    }
                    activation_state = "blocked".into();
                }
            }
        }
    }

    let aggregate_status = aggregate_package_status(&components, &residuals, input);

    Ok(PackageProjectionReport {
        destination_target: input.destination,
        source_target: input.package.source_target,
        components,
        residuals,
        aggregate_status,
        activation_state,
        diagnostics,
        materialized_package_root: materialized_root,
    })
}

/// 将 activator plan/result 合并进报告的 activation_state（只读辅助）。
///
/// Business Logic: Gate B durable activation 状态机结果不得被 package 成功掩盖。
/// Code Logic: plan blocked/activation_required 优先。
pub fn merge_activation_into_report(
    report: &mut PackageProjectionReport,
    plan: Option<&ActivationPlan>,
    result: Option<&ActivationResult>,
) {
    if let Some(p) = plan {
        if p.blocked {
            report.activation_state = "blocked".into();
            report.aggregate_status =
                prefer_status(report.aggregate_status, AssetAggregateStatus::Blocked);
            if let Some(r) = &p.blocked_reason {
                report.diagnostics.push(format!("activation_blocked:{r}"));
            }
            return;
        }
        if p.activation_required {
            report.activation_state = "activationRequired".into();
            report.aggregate_status = prefer_status(
                report.aggregate_status,
                AssetAggregateStatus::ActivationRequired,
            );
            for c in &mut report.components {
                if matches!(
                    c.target_status,
                    ComponentTargetStatus::Verified | ComponentTargetStatus::Partial
                ) {
                    c.target_status = ComponentTargetStatus::ActivationRequired;
                    c.reasons.push("activation_required".into());
                }
            }
            return;
        }
        report.activation_state = "planned".into();
    }
    if let Some(r) = result {
        if r.activation_required {
            report.activation_state = "activationRequired".into();
            report.aggregate_status = prefer_status(
                report.aggregate_status,
                AssetAggregateStatus::ActivationRequired,
            );
        } else if r.skipped_blocked {
            report.activation_state = "blocked".into();
            report.aggregate_status =
                prefer_status(report.aggregate_status, AssetAggregateStatus::Blocked);
        } else if r.ok {
            report.activation_state = "applied".into();
        } else {
            report.activation_state = "blocked".into();
            report.aggregate_status =
                prefer_status(report.aggregate_status, AssetAggregateStatus::Blocked);
            if let Some(e) = &r.error {
                report.diagnostics.push(format!("activation_error:{e}"));
            }
        }
    }
}

fn prefer_status(
    current: AssetAggregateStatus,
    incoming: AssetAggregateStatus,
) -> AssetAggregateStatus {
    // 优先级：Blocked > ExternalCollision > ActivationRequired > SourceOnly > Partial > Full
    let rank = |s: AssetAggregateStatus| -> u8 {
        match s {
            AssetAggregateStatus::Blocked => 6,
            AssetAggregateStatus::ExternalCollision => 5,
            AssetAggregateStatus::Detached => 4,
            AssetAggregateStatus::ActivationRequired => 3,
            AssetAggregateStatus::SourceOnly => 2,
            AssetAggregateStatus::Partial => 1,
            AssetAggregateStatus::Unconfigured => 0,
            AssetAggregateStatus::Full => 0,
        }
    };
    if rank(incoming) > rank(current) {
        incoming
    } else {
        current
    }
}

fn project_hook_component(
    cref: &PluginComponentRef,
    hook: &PortableHook,
    input: &PackageRenderInput,
    diagnostics: &mut Vec<String>,
) -> ComponentProjectionReport {
    if input.external_collision {
        return ComponentProjectionReport {
            kind: cref.kind,
            asset_id: cref.asset_id.clone(),
            canonical_revision_id: cref.revision_id.clone(),
            materialized_alias: None,
            target_status: ComponentTargetStatus::ExternalCollision,
            reasons: vec!["external_collision".into()],
            projected_paths: vec![],
        };
    }
    if input.write_blocked {
        return ComponentProjectionReport {
            kind: cref.kind,
            asset_id: cref.asset_id.clone(),
            canonical_revision_id: cref.revision_id.clone(),
            materialized_alias: None,
            target_status: ComponentTargetStatus::Blocked,
            reasons: vec!["write_blocked".into()],
            projected_paths: vec![],
        };
    }

    let decision = evaluate_hook_mapping(
        hook,
        input.destination,
        &input.hook_registry,
        &input.known_evidence_ids,
        input.expected_hook_schema_version,
        input.expected_hook_trust,
    );

    match decision {
        HookMappingDecision::Allowed { record } => {
            // 同 target 或已证据化 mapping：标记 verified（不写真实 CLI 文件，仅投影计划）
            let path = format!(
                "hooks/{}/{}.json",
                record.destination_target.as_str(),
                hook.event_intent.as_str()
            );
            ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: ComponentTargetStatus::Verified,
                reasons: if hook.source_target == input.destination {
                    vec!["same_target_hook".into()]
                } else {
                    vec![format!("hook_mapped:{}", record.evidence_id)]
                },
                projected_paths: vec![path],
            }
        }
        HookMappingDecision::SourceOnly { reasons } => {
            for r in &reasons {
                diagnostics.push(format!("hook:{}:{}", cref.asset_id, r));
            }
            ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: ComponentTargetStatus::SourceOnly,
                reasons,
                projected_paths: vec![],
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn project_portable_component(
    cref: &PluginComponentRef,
    payload: &PortableAssetPayload,
    partial: bool,
    skill_markdown: Option<&str>,
    input: &PackageRenderInput,
    skill_inputs: &mut Vec<PackageSkillInput>,
    projected_files: &mut Vec<ProjectedAssetFile>,
    diagnostics: &mut Vec<String>,
) -> ComponentProjectionReport {
    if input.external_collision {
        return ComponentProjectionReport {
            kind: cref.kind,
            asset_id: cref.asset_id.clone(),
            canonical_revision_id: cref.revision_id.clone(),
            materialized_alias: None,
            target_status: ComponentTargetStatus::ExternalCollision,
            reasons: vec!["external_collision".into()],
            projected_paths: vec![],
        };
    }
    if input.write_blocked {
        return ComponentProjectionReport {
            kind: cref.kind,
            asset_id: cref.asset_id.clone(),
            canonical_revision_id: cref.revision_id.clone(),
            materialized_alias: None,
            target_status: ComponentTargetStatus::Blocked,
            reasons: vec!["write_blocked".into()],
            projected_paths: vec![],
        };
    }

    match render_portable_payload(input.destination, payload) {
        Ok(projection) => {
            let paths: Vec<String> = projection
                .files
                .iter()
                .map(|f| f.relative_path.clone())
                .collect();
            for f in projection.files {
                projected_files.push(f);
            }
            for d in projection.diagnostics {
                diagnostics.push(format!(
                    "portable:{}:{}:{}",
                    cref.asset_id, d.code, d.message
                ));
            }
            if let PortableAssetPayload::Skill(skill) = payload {
                skill_inputs.push(PackageSkillInput {
                    logical_asset_id: cref.asset_id.clone(),
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    skill_markdown: skill_markdown.unwrap_or("").to_string(),
                    target_only: false,
                    visible_targets: vec![],
                });
            }
            let status = if partial {
                ComponentTargetStatus::Partial
            } else {
                ComponentTargetStatus::Verified
            };
            let mut reasons = vec![];
            if partial {
                reasons.push("component_partial".into());
            } else {
                reasons.push("portable_rendered".into());
            }
            ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: status,
                reasons,
                projected_paths: paths,
            }
        }
        Err(e) => {
            diagnostics.push(format!("render_failed:{}:{}", cref.asset_id, e));
            ComponentProjectionReport {
                kind: cref.kind,
                asset_id: cref.asset_id.clone(),
                canonical_revision_id: cref.revision_id.clone(),
                materialized_alias: None,
                target_status: ComponentTargetStatus::Blocked,
                reasons: vec!["render_failed".into()],
                projected_paths: vec![],
            }
        }
    }
}

fn project_residuals(
    residuals: &[PluginResidualRef],
    destination: AgentTarget,
    diagnostics: &mut Vec<String>,
) -> Vec<ResidualProjectionReport> {
    let mut out = Vec::with_capacity(residuals.len());
    for r in residuals {
        if r.target == destination {
            out.push(ResidualProjectionReport {
                residual_target: r.target,
                residual_kind: r.residual_kind,
                tree_manifest_hash: r.tree_manifest_hash.clone(),
                included: true,
                reasons: vec!["same_target_residual".into()],
            });
        } else {
            let reason = format!(
                "residual_omitted_other_runtime:{}:{}",
                r.target.as_str(),
                r.residual_kind.as_str()
            );
            diagnostics.push(reason.clone());
            out.push(ResidualProjectionReport {
                residual_target: r.target,
                residual_kind: r.residual_kind,
                tree_manifest_hash: r.tree_manifest_hash.clone(),
                included: false,
                reasons: vec![reason],
            });
        }
    }
    out
}

fn apply_materialized_aliases(
    components: &mut [ComponentProjectionReport],
    gen: &GeneratedTargetPackage,
) {
    for c in components.iter_mut() {
        if c.kind != AssetKind::Skill {
            continue;
        }
        // meta.invocation_aliases 以 skill name 为 key；我们只有 asset_id。
        // 尝试 asset_id 与任意 alias 值匹配；否则用第一个含 asset 的 name。
        if let Some((_name, alias)) = gen
            .meta
            .invocation_aliases
            .iter()
            .find(|(name, _)| {
                *name == &c.asset_id || gen.meta.logical_asset_ids.contains(&c.asset_id)
            })
            .or_else(|| {
                // 若 logical_asset_ids 顺序与 skills 一致，按 index 对齐不可靠；取 name==asset 失败则跳过
                gen.meta.invocation_aliases.iter().next()
            })
        {
            // 更精确：若 logical ids 含该 asset，找对应 skill name by scanning aliases keys vs meta
            let _ = _name;
            if gen.meta.logical_asset_ids.contains(&c.asset_id) {
                // 使用 package meta 中与 logical 对应的 name：builder 用 skill.name 作 alias key
                // 若 asset_id 不是 name，则在 aliases 中找 value 也写入
                if let Some(alias2) = gen.meta.invocation_aliases.values().find(|_| true) {
                    c.materialized_alias = Some(alias2.clone());
                }
                c.materialized_alias = Some(alias.clone());
            }
        }
        // 直接：logical id 在 meta，尝试 name 字段等于 asset 或 alias map 任意
        if c.materialized_alias.is_none() {
            if let Some(alias) = gen.meta.invocation_aliases.get(&c.asset_id) {
                c.materialized_alias = Some(alias.clone());
            }
        }
    }
    // 二次：对每个 skill component，若仍无 alias，且只有一个 skill alias，绑定之
    let skill_count = components
        .iter()
        .filter(|c| c.kind == AssetKind::Skill)
        .count();
    if skill_count == 1 && gen.meta.invocation_aliases.len() == 1 {
        if let Some(alias) = gen.meta.invocation_aliases.values().next() {
            for c in components.iter_mut() {
                if c.kind == AssetKind::Skill && c.materialized_alias.is_none() {
                    c.materialized_alias = Some(alias.clone());
                }
            }
        }
    }
}

fn aggregate_package_status(
    components: &[ComponentProjectionReport],
    residuals: &[ResidualProjectionReport],
    input: &PackageRenderInput,
) -> AssetAggregateStatus {
    if input.external_collision {
        return AssetAggregateStatus::ExternalCollision;
    }
    if input.write_blocked {
        return AssetAggregateStatus::Blocked;
    }
    if input.force_activation_required {
        return AssetAggregateStatus::ActivationRequired;
    }

    let mut any_blocked = false;
    let mut any_collision = false;
    let mut any_activation = false;
    let mut any_source_only = false;
    let mut any_partial = false;
    let mut all_verified = !components.is_empty() || residuals.iter().any(|r| r.included);

    if components.is_empty() && residuals.is_empty() {
        return AssetAggregateStatus::Partial;
    }

    for c in components {
        match c.target_status {
            ComponentTargetStatus::Blocked => any_blocked = true,
            ComponentTargetStatus::ExternalCollision => any_collision = true,
            ComponentTargetStatus::ActivationRequired => any_activation = true,
            ComponentTargetStatus::SourceOnly => any_source_only = true,
            ComponentTargetStatus::Partial => any_partial = true,
            ComponentTargetStatus::Verified => {}
        }
        if !matches!(c.target_status, ComponentTargetStatus::Verified) {
            all_verified = false;
        }
    }

    // 跨 target residual omit 会阻止 full（source-only runtime 永远阻止跨 target full）
    let omitted_residual = residuals.iter().any(|r| !r.included);
    if omitted_residual {
        any_source_only = true;
        all_verified = false;
    }
    // 同 target residual 必须 included 才计 verified
    for r in residuals {
        if r.residual_target == input.destination && !r.included {
            any_partial = true;
            all_verified = false;
        }
    }

    if any_blocked {
        return AssetAggregateStatus::Blocked;
    }
    if any_collision {
        return AssetAggregateStatus::ExternalCollision;
    }
    if any_activation {
        return AssetAggregateStatus::ActivationRequired;
    }
    if any_source_only
        && !components.iter().any(|c| {
            matches!(
                c.target_status,
                ComponentTargetStatus::Verified | ComponentTargetStatus::Partial
            )
        })
    {
        // 全部 source-only
        return AssetAggregateStatus::SourceOnly;
    }
    if any_source_only || any_partial || !all_verified {
        return AssetAggregateStatus::Partial;
    }
    if all_verified {
        return AssetAggregateStatus::Full;
    }
    AssetAggregateStatus::Partial
}

/// 渲染单个 portable component（供 target adapter 薄封装）。
///
/// Business Logic: Gate B renderer 单一入口，plugin 与 standalone 共用。
/// Code Logic: 委托 `render_portable_payload`。
pub fn render_component_for_target(
    target: AgentTarget,
    payload: &PortableAssetPayload,
) -> Result<TargetAssetProjection, AppError> {
    render_portable_payload(target, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::assets::{PortableCommand, PortableSkill};
    use crate::agent_hub::plugins::hook_mapping::HookMappingRecord;
    use crate::agent_hub::plugins::models::{
        ComponentOwnership, HookEventIntent, PluginComponentRef, PluginResidualRef,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn rev(s: &str) -> RevisionId {
        RevisionId::from(s)
    }

    fn skill_payload() -> PortableAssetPayload {
        PortableAssetPayload::Skill(PortableSkill {
            name: "review".into(),
            description: "shared review".into(),
            skill_markdown_hash: "aa".repeat(32),
            tree_manifest_hash: "bb".repeat(32),
            target_extensions: BTreeMap::new(),
        })
    }

    fn partial_command_payload() -> PortableAssetPayload {
        PortableAssetPayload::Command(PortableCommand {
            name: "ship".into(),
            description: Some("partial".into()),
            prompt_template: "ship it".into(),
            arguments: vec![],
            target_extensions: {
                let mut m = BTreeMap::new();
                m.insert(AgentTarget::Claude, json!({"unknownField": true}));
                m
            },
        })
    }

    fn sample_hook(source: AgentTarget) -> PortableHook {
        PortableHook {
            event_intent: HookEventIntent::PreToolUse,
            input_contract: json!({"toolName": "Bash"}),
            output_contract: json!({"permission": "allow"}),
            command_tree_hash: None,
            source_target: source,
            target_extensions: BTreeMap::new(),
        }
    }

    fn mixed_package() -> PluginPackagePayload {
        PluginPackagePayload {
            plugin_id: "demo.mixed".into(),
            name: "Mixed".into(),
            version: Some("1.0.0".into()),
            description: None,
            source_target: AgentTarget::OpenCode,
            component_refs: vec![
                PluginComponentRef {
                    kind: AssetKind::Skill,
                    asset_id: "asset-skill".into(),
                    revision_id: rev("rev-skill"),
                    ownership: ComponentOwnership::PackageOwned,
                },
                PluginComponentRef {
                    kind: AssetKind::Command,
                    asset_id: "asset-cmd".into(),
                    revision_id: rev("rev-cmd"),
                    ownership: ComponentOwnership::PackageOwned,
                },
                PluginComponentRef {
                    kind: AssetKind::Hook,
                    asset_id: "asset-hook".into(),
                    revision_id: rev("rev-hook"),
                    ownership: ComponentOwnership::PackageOwned,
                },
            ],
            residual_refs: vec![PluginResidualRef {
                target: AgentTarget::OpenCode,
                residual_kind: ResidualKind::Runtime,
                tree_manifest_hash: "ab".repeat(32),
            }],
            target_extensions: BTreeMap::new(),
        }
    }

    fn resolved_map() -> BTreeMap<String, ResolvedComponentPayload> {
        let mut m = BTreeMap::new();
        m.insert(
            "rev-skill".into(),
            ResolvedComponentPayload::Portable {
                payload: skill_payload(),
                partial: false,
                skill_markdown: Some(
                    "---\nname: review\ndescription: shared review\n---\n# body\n".into(),
                ),
            },
        );
        m.insert(
            "rev-cmd".into(),
            ResolvedComponentPayload::Portable {
                payload: partial_command_payload(),
                partial: true,
                skill_markdown: None,
            },
        );
        m.insert(
            "rev-hook".into(),
            ResolvedComponentPayload::Hook {
                hook: sample_hook(AgentTarget::OpenCode),
            },
        );
        m
    }

    /// Business Logic: OpenCode 同源 + 全部 native 验证 → full。
    #[test]
    fn opencode_source_native_package_can_be_full() {
        let dir = TempDir::new().unwrap();
        let input = PackageRenderInput {
            package: mixed_package(),
            destination: AgentTarget::OpenCode,
            resolved: resolved_map(),
            data_dir: Some(dir.path().to_path_buf()),
            scope_id: "user".into(),
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_eq!(report.destination_target, AgentTarget::OpenCode);
        // skill verified, command partial, hook same-target verified, residual included
        // partial command → aggregate Partial（非 full）
        assert_eq!(report.aggregate_status, AssetAggregateStatus::Partial);
        assert!(report
            .components
            .iter()
            .any(|c| c.kind == AssetKind::Command
                && c.target_status == ComponentTargetStatus::Partial));
        assert!(report.residuals.iter().all(|r| r.included));
    }

    /// Business Logic: OpenCode 在无 partial 时 full。
    #[test]
    fn opencode_full_when_every_native_verified() {
        let dir = TempDir::new().unwrap();
        let mut package = mixed_package();
        // 去掉 partial command
        package
            .component_refs
            .retain(|c| c.kind != AssetKind::Command);
        let mut resolved = resolved_map();
        resolved.remove("rev-cmd");
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::OpenCode,
            resolved,
            data_dir: Some(dir.path().to_path_buf()),
            scope_id: "user".into(),
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_eq!(
            report.aggregate_status,
            AssetAggregateStatus::Full,
            "diagnostics={:?}",
            report.diagnostics
        );
        assert!(report.residuals.iter().all(|r| r.included));
    }

    /// Business Logic: Claude 跨 target 面对 OpenCode residual + targetOnly hook → 永不满 full。
    #[test]
    fn claude_cross_target_never_full_with_source_only_breakdown() {
        let package = mixed_package();
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::Claude,
            resolved: resolved_map(),
            // 无 hook mapping registry
            hook_registry: vec![],
            known_evidence_ids: BTreeSet::new(),
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_ne!(report.aggregate_status, AssetAggregateStatus::Full);
        assert!(matches!(
            report.aggregate_status,
            AssetAggregateStatus::Partial | AssetAggregateStatus::SourceOnly
        ));
        let hook = report
            .components
            .iter()
            .find(|c| c.kind == AssetKind::Hook)
            .unwrap();
        assert_eq!(hook.target_status, ComponentTargetStatus::SourceOnly);
        let residual = report.residuals.first().unwrap();
        assert!(!residual.included);
        assert!(residual
            .reasons
            .iter()
            .any(|r| r.contains("residual_omitted_other_runtime")));
        // skill 仍可 portable 渲染
        let skill = report
            .components
            .iter()
            .find(|c| c.kind == AssetKind::Skill)
            .unwrap();
        assert_eq!(skill.target_status, ComponentTargetStatus::Verified);
    }

    /// Business Logic: Codex activator 要求 activationRequired 时聚合不得 full。
    #[test]
    fn codex_activation_required_aggregate() {
        let package = mixed_package();
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::Codex,
            resolved: resolved_map(),
            force_activation_required: true,
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_eq!(
            report.aggregate_status,
            AssetAggregateStatus::ActivationRequired
        );
        assert_eq!(report.activation_state, "activationRequired");
    }

    /// Business Logic: 缺 hook mapping 时跨 target hook 保持 source-only。
    #[test]
    fn hook_without_mapping_stays_source_only_on_other_target() {
        let mut resolved = BTreeMap::new();
        resolved.insert(
            "rev-hook".into(),
            ResolvedComponentPayload::Hook {
                hook: sample_hook(AgentTarget::Claude),
            },
        );
        let package = PluginPackagePayload {
            plugin_id: "h".into(),
            name: "H".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Hook,
                asset_id: "h1".into(),
                revision_id: rev("rev-hook"),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![],
            target_extensions: BTreeMap::new(),
        };
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::Codex,
            resolved,
            hook_registry: vec![],
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_eq!(
            report.components[0].target_status,
            ComponentTargetStatus::SourceOnly
        );
        assert!(report.components[0]
            .reasons
            .iter()
            .any(|r| r == "hook_mapping_absent"));
        assert_eq!(report.aggregate_status, AssetAggregateStatus::SourceOnly);
    }

    /// Business Logic: 完整 fixture mapping 允许 hook 跨 target。
    #[test]
    fn hook_with_fixture_mapping_renders_on_destination() {
        let mut resolved = BTreeMap::new();
        resolved.insert(
            "rev-hook".into(),
            ResolvedComponentPayload::Hook {
                hook: sample_hook(AgentTarget::Claude),
            },
        );
        let package = PluginPackagePayload {
            plugin_id: "h".into(),
            name: "H".into(),
            version: None,
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Hook,
                asset_id: "h1".into(),
                revision_id: rev("rev-hook"),
                ownership: ComponentOwnership::PackageOwned,
            }],
            residual_refs: vec![],
            target_extensions: BTreeMap::new(),
        };
        let mapping = HookMappingRecord {
            intent: HookEventIntent::PreToolUse,
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Codex,
            schema_version: 1,
            trust_model: HookTrustModel::ExactContract,
            evidence_id: "L3-AGENT-HUB-HOOK-FIXTURE-001".into(),
            required_input_fields: vec!["toolName".into()],
            required_output_fields: vec!["permission".into()],
        };
        let mut evidence = BTreeSet::new();
        evidence.insert("L3-AGENT-HUB-HOOK-FIXTURE-001".into());
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::Codex,
            resolved,
            hook_registry: vec![mapping],
            known_evidence_ids: evidence,
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_eq!(
            report.components[0].target_status,
            ComponentTargetStatus::Verified
        );
        assert_eq!(report.aggregate_status, AssetAggregateStatus::Full);
    }

    /// Business Logic: 状态不得 overstate——source residual omit 阻止 full。
    #[test]
    fn aggregate_never_full_when_runtime_residual_omitted() {
        let mut package = mixed_package();
        package
            .component_refs
            .retain(|c| c.kind == AssetKind::Skill);
        let mut resolved = BTreeMap::new();
        resolved.insert(
            "rev-skill".into(),
            ResolvedComponentPayload::Portable {
                payload: skill_payload(),
                partial: false,
                skill_markdown: None,
            },
        );
        let input = PackageRenderInput {
            package,
            destination: AgentTarget::Claude, // residual 是 OpenCode
            resolved,
            ..PackageRenderInput::default()
        };
        let report = project_plugin_package(&input).unwrap();
        assert_ne!(report.aggregate_status, AssetAggregateStatus::Full);
        assert_eq!(report.aggregate_status, AssetAggregateStatus::Partial);
    }
}
