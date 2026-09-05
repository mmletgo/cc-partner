//! agent_hub/service/summary — summary/probe 构建与单元格判定
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub 列表/详情必须展示各 CLI 真实可用性（supported/verified/sourceOnly），
//!     批量列表禁止 N 次全表查询与 N×3 CLI probe；probe 失败不得伪装 supported。
//!
//! Code Logic（这个模块做什么）:
//!     probe_all_targets_best_effort / probe_support_map 经 support manifest 评估支持级；
//!     SummarySharedContext 一次预载 mats/bindings/ownerships/conflicts 后批量构建
//!     AgentHubAssetSummaryDto（单元格 sourceOnly/verified 判定）与 detail。

use super::dto::{
    AgentHubAssetDetailDto, AgentHubAssetSummaryDto, AgentHubConflictDto, AgentHubProbeDto,
    AgentHubTargetCellDto, InstructionBlockDto,
};
use crate::agent_hub::models::{
    compute_asset_aggregate_status, AgentTarget, AssetKind, DesiredPresence, LogicalAsset,
    Materialization, MaterializationStatus, TargetBinding, TargetStatusSnapshot,
    UserInstructionOwnershipRecord,
};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, CursorInstructionAdapter,
    GeminiInstructionAdapter, GrokInstructionAdapter, OpenCodeInstructionAdapter,
    PiInstructionAdapter, TargetEnvironment,
};
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// best-effort 探测三 CLI，并经 support manifest 评估展示态。
///
/// Business Logic（为什么需要这个函数）:
///     status 顶部展示本机 Claude/Codex/OpenCode 可用性。
///     未认证（null 版本 / 写能力 blocked）不得显示绿色 Supported，必须 scanOnly。
///
/// Code Logic（这个函数做什么）:
///     home+env 构造 TargetEnvironment；adapter.probe 后经 evaluate_target_support 映射 support 字段。
pub(super) fn probe_all_targets_best_effort() -> Vec<AgentHubProbeDto> {
    use crate::agent_hub::support::{
        builtin_support_manifest, evaluate_target_support, EvaluatedSupportMode,
        RuntimeProbeSnapshot,
    };
    let env = current_target_environment();
    let manifest = match builtin_support_manifest() {
        Ok(m) => Some(m),
        Err(e) => {
            tracing::warn!(error = %e, "agent_hub probe: builtin support manifest unavailable");
            None
        }
    };
    AgentTarget::ALL
        .into_iter()
        .map(
            |target| match crate::agent_hub::targets::probe_target(target, &env) {
                Ok(probe) => {
                    let support = if let Some(manifest) = manifest.as_ref() {
                        let snap = RuntimeProbeSnapshot {
                            target: probe.target,
                            executable: probe.executable.clone(),
                            version: probe.version.clone(),
                            config_root: probe.config_root.clone(),
                            fingerprint: probe.fingerprint.clone(),
                            help_fingerprint: None,
                        };
                        let eval = evaluate_target_support(manifest, &snap);
                        match &eval.mode {
                            EvaluatedSupportMode::Certified => {
                                if eval.write_allowed {
                                    "supported".to_string()
                                } else {
                                    "scanOnly".to_string()
                                }
                            }
                            EvaluatedSupportMode::ScanOnly { .. } => "scanOnly".to_string(),
                            EvaluatedSupportMode::Blocked { .. } => "unsupported".to_string(),
                        }
                    } else {
                        // fail-closed：manifest 不可用时不得抬升为 Supported
                        "scanOnly".to_string()
                    };
                    AgentHubProbeDto {
                        target: probe.target,
                        executable: probe.executable.map(|p| p.to_string_lossy().into_owned()),
                        version: probe.version,
                        support,
                        config_root: Some(probe.config_root.to_string_lossy().into_owned()),
                    }
                }
                Err(_) => AgentHubProbeDto {
                    target,
                    executable: None,
                    version: None,
                    support: "unsupported".to_string(),
                    config_root: None,
                },
            },
        )
        .collect()
}

/// 构造当前进程注入环境。
///
/// Business Logic（为什么需要这个函数）:
///     probe 不得改 process env，但必须读取真实 home/PATH 与 CLI 变量。
///
/// Code Logic（这个函数做什么）:
///     dirs::home_dir + 关注 env + PATH 切分。
fn current_target_environment() -> TargetEnvironment {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let interest = [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "OPENCODE_CONFIG_DIR",
        "OPENCODE_CONFIG",
        "XDG_CONFIG_HOME",
        "GROK_HOME",
        "GEMINI_HOME",
        "HOME",
        "USERPROFILE",
    ];
    let mut vars = BTreeMap::new();
    for key in interest {
        if let Ok(v) = std::env::var(key) {
            if !v.trim().is_empty() {
                vars.insert(key.to_string(), v);
            }
        }
    }
    let path_entries = crate::agent_hub::targets::paths::gui_augmented_path_entries(
        &home,
        std::env::var_os("PATH").as_deref(),
    );
    TargetEnvironment {
        home,
        vars,
        path_entries,
    }
}

/// list/detail 共用的 summary 共享输入（mats/probe/conflicts 只拉一次）。
struct SummarySharedContext {
    mat_by_binding: BTreeMap<String, Materialization>,
    bindings_by_asset: BTreeMap<String, Vec<TargetBinding>>,
    ownerships_by_asset: BTreeMap<String, Vec<UserInstructionOwnershipRecord>>,
    support_by_target: BTreeMap<AgentTarget, bool>,
    /// asset_id 存在未解决 conflict（canonical 或任意 target）。
    conflict_asset_ids: std::collections::HashSet<String>,
}

/**
 * Business Logic: 批量 list 时禁止 N 次全表 mats + N×3 CLI probe + N×4 conflict。
 * Code Logic: 并行读取 mats/bindings/ownerships/conflicts，各表一次；CLI probe 也只执行一次。
 */
async fn load_summary_shared_context(state: &AppState) -> Result<SummarySharedContext, AppError> {
    let (mats, bindings, ownerships, conflicts) = tokio::try_join!(
        state.agent_hub_repo.list_materializations(),
        state.agent_hub_repo.list_target_bindings(),
        state.agent_hub_repo.list_user_instruction_ownerships_all(),
        state.agent_hub_repo.list_unresolved_conflicts(),
    )?;
    let support_by_target = probe_support_map();
    let conflict_asset_ids = conflicts.into_iter().map(|c| c.asset_id).collect();
    let mat_by_binding = mats
        .into_iter()
        .map(|materialization| (materialization.target_binding_id.clone(), materialization))
        .collect();
    let mut bindings_by_asset: BTreeMap<String, Vec<TargetBinding>> = BTreeMap::new();
    for binding in bindings {
        bindings_by_asset
            .entry(binding.asset_id.clone())
            .or_default()
            .push(binding);
    }
    let mut ownerships_by_asset: BTreeMap<String, Vec<UserInstructionOwnershipRecord>> =
        BTreeMap::new();
    for ownership in ownerships {
        ownerships_by_asset
            .entry(ownership.asset_id.clone())
            .or_default()
            .push(ownership);
    }
    Ok(SummarySharedContext {
        mat_by_binding,
        bindings_by_asset,
        ownerships_by_asset,
        support_by_target,
        conflict_asset_ids,
    })
}

/**
 * Business Logic: 列表路径对 N 条 asset 共享 probe/mats/conflicts。
 * Code Logic: 预载 shared → 内存按 asset 关联 bindings/ownerships/mats。
 */
pub(super) async fn build_summaries_for_assets(
    state: &AppState,
    assets: &[LogicalAsset],
) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let shared = load_summary_shared_context(state).await?;
    let mut out = Vec::with_capacity(assets.len());
    for asset in assets {
        out.push(build_summary_with_shared(asset, &shared));
    }
    Ok(out)
}

/// 构建资产摘要。
///
/// Business Logic（为什么需要这个函数）:
///     列表/详情/set_binding 共用 summary。
///
/// Code Logic（这个函数做什么）:
///     单条路径加载 shared 后委托 build_summary_with_shared（与批量字段一致）。
pub(super) async fn build_summary(
    state: &AppState,
    asset: &LogicalAsset,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let shared = load_summary_shared_context(state).await?;
    Ok(build_summary_with_shared(asset, &shared))
}

/**
 * Business Logic: 固定三 target 单元格 + has_conflict（来自 shared 集合）。
 * Code Logic: bindings/ownership/mats/probe/conflicts 全部从 shared 内存索引读取。
 */
fn build_summary_with_shared(
    asset: &LogicalAsset,
    shared: &SummarySharedContext,
) -> AgentHubAssetSummaryDto {
    let bindings = shared
        .bindings_by_asset
        .get(&asset.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let needs_ownership = asset.scope_id == crate::agent_hub::migration::USER_SCOPE_STABLE_ID
        && asset.logical_key == crate::agent_hub::migration::USER_INSTRUCTION_LOGICAL_KEY;
    let user_instruction_ownership = shared
        .ownerships_by_asset
        .get(&asset.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let support_by_target = &shared.support_by_target;
    let mut targets = Vec::new();
    let mut snaps = Vec::new();
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        let supported = support_by_target.get(&target).copied().unwrap_or(false);
        if let Some(b) = bindings.iter().find(|b| b.target == target) {
            let mat = shared.mat_by_binding.get(&b.id);
            let legacy_unselected = needs_ownership
                && b.desired_presence == DesiredPresence::Absent
                && !b.desired_enabled
                && b.local_scope_mapping_id.is_none()
                && b.checkout_binding_id.is_none()
                && mat.is_none()
                && !user_instruction_ownership
                    .iter()
                    .any(|ownership| ownership.target == target);
            let mat_status = mat.map(|m| m.status);
            let source_only = is_source_only_cell(asset, mat_status);
            let verified = is_verified_cell(asset, b, mat);
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: b.desired_presence,
                desired_enabled: b.desired_enabled,
                materialization_status: mat_status.map(|s| s.as_str().to_string()),
                last_error: mat.and_then(|m| m.last_error.clone()),
                requested: !legacy_unselected,
                supported,
                source_only,
                verified,
            });
            if !legacy_unselected {
                snaps.push(TargetStatusSnapshot {
                    requested: true,
                    desired_presence: b.desired_presence,
                    desired_enabled: b.desired_enabled,
                    supported,
                    source_only,
                    materialization_status: mat_status,
                    verified,
                });
            }
        } else {
            targets.push(AgentHubTargetCellDto {
                target,
                desired_presence: DesiredPresence::Absent,
                desired_enabled: false,
                materialization_status: None,
                last_error: None,
                requested: false,
                supported,
                source_only: false,
                verified: false,
            });
            // 无 binding 的 target 不进入 requested 聚合
        }
    }
    let aggregate_status = compute_asset_aggregate_status(&snaps).as_str().to_string();
    let has_conflict = shared.conflict_asset_ids.contains(&asset.id);
    AgentHubAssetSummaryDto {
        asset_id: asset.id.clone(),
        scope_id: asset.scope_id.clone(),
        kind: asset.kind.as_str().to_string(),
        display_name: asset.display_name.clone(),
        logical_key: asset.logical_key.clone(),
        origin_namespace: asset.origin_namespace.clone(),
        policy: asset.policy.as_str().to_string(),
        current_revision_id: asset
            .current_revision_id
            .as_ref()
            .map(|r| r.as_str().to_string()),
        targets,
        has_conflict,
        aggregate_status,
    }
}

/// best-effort 三 target support 探测映射。
///
/// Business Logic: 聚合 full 需 supported；probe 失败不得伪装 supported。
/// Code Logic: 复用 adapters；失败 → false。
pub(crate) fn evaluate_target_support_flags(
    evaluated: &crate::agent_hub::support::EvaluatedTargetSupport,
    capability: crate::agent_hub::support::TargetCapability,
) -> bool {
    use crate::agent_hub::support::CapabilitySupport;
    matches!(
        evaluated.capability(capability),
        CapabilitySupport::Supported
            | CapabilitySupport::SupportedAfterRestart
            | CapabilitySupport::ActivationRequired
            | CapabilitySupport::ReadOnly
    )
}

pub(super) fn probe_support_map() -> BTreeMap<AgentTarget, bool> {
    use crate::agent_hub::support::{
        builtin_support_manifest, evaluate_target_support, RuntimeProbeSnapshot, TargetCapability,
    };
    let env = current_target_environment_for_summary();
    let manifest = builtin_support_manifest().ok();
    let adapters: Vec<(Box<dyn AssetAdapter>, AgentTarget)> = vec![
        (Box::new(ClaudeInstructionAdapter), AgentTarget::Claude),
        (Box::new(CodexInstructionAdapter), AgentTarget::Codex),
        (Box::new(OpenCodeInstructionAdapter), AgentTarget::OpenCode),
        (Box::new(GrokInstructionAdapter), AgentTarget::Grok),
        (Box::new(GeminiInstructionAdapter), AgentTarget::Gemini),
        (Box::new(CursorInstructionAdapter), AgentTarget::Cursor),
        (Box::new(PiInstructionAdapter), AgentTarget::Pi),
    ];
    let mut map = BTreeMap::new();
    for (adapter, target) in adapters {
        let supported = match (manifest.as_ref(), adapter.probe(&env)) {
            (Some(manifest), Ok(probe)) => {
                let snapshot = RuntimeProbeSnapshot {
                    target: probe.target,
                    executable: probe.executable,
                    version: probe.version,
                    config_root: probe.config_root,
                    fingerprint: probe.fingerprint,
                    help_fingerprint: None,
                };
                let evaluated = evaluate_target_support(manifest, &snapshot);
                evaluate_target_support_flags(&evaluated, TargetCapability::ScanInstruction)
            }
            _ => false,
        };
        map.insert(target, supported);
    }
    map
}

/// summary 用 TargetEnvironment（复用 probe 环境构造）。
fn current_target_environment_for_summary() -> TargetEnvironment {
    current_target_environment()
}

/// 单元格是否 sourceOnly。
///
/// Business Logic: 无可投影 materialization 且 kind 无法在该 target 落地。
/// Code Logic: 无 mat 且 desired Present → sourceOnly 倾向；指令有 path 可投影 → false。
fn is_source_only_cell(asset: &LogicalAsset, mat_status: Option<MaterializationStatus>) -> bool {
    if mat_status.is_some() {
        return false;
    }
    // Instruction 始终可投影路径；package 缺 mat 时视为 sourceOnly（仅 hub 源）
    !matches!(asset.kind, AssetKind::Instruction)
}

/// 单元格是否 verified。
///
/// Business Logic: full 禁止仅凭 package write 成功；需 activation/list 通过。
/// Code Logic: Instruction Synced → verified；package Synced + 无 disable_strategy 错误 → verified；
/// ActivationRequired/Pending/Blocked 等 → false。
fn is_verified_cell(
    asset: &LogicalAsset,
    binding: &TargetBinding,
    mat: Option<&Materialization>,
) -> bool {
    let Some(mat) = mat else {
        return false;
    };
    match mat.status {
        MaterializationStatus::Synced => {
            if binding.desired_presence == DesiredPresence::Absent {
                return true;
            }
            // package：若 last_error 标记仍待激活则非 verified
            if !matches!(asset.kind, AssetKind::Instruction) {
                if let Some(err) = mat.last_error.as_deref() {
                    if err.contains("activation") || err.contains("disable_strategy") {
                        return false;
                    }
                }
            }
            true
        }
        MaterializationStatus::ActivationRequired
        | MaterializationStatus::Pending
        | MaterializationStatus::Blocked
        | MaterializationStatus::Unsupported
        | MaterializationStatus::Drift
        | MaterializationStatus::Conflict
        | MaterializationStatus::Detached
        | MaterializationStatus::ExternalCollision => false,
    }
}

/// summary → detail。
///
/// Business Logic（为什么需要这个函数）:
///     复用 summary 字段填充扁平 detail。
///
/// Code Logic（这个函数做什么）:
///     字段拷贝 + blocks/content/conflicts。
pub(super) fn detail_from_summary(
    summary: AgentHubAssetSummaryDto,
    blocks: Vec<InstructionBlockDto>,
    content_markdown: Option<String>,
    conflicts: Vec<AgentHubConflictDto>,
) -> AgentHubAssetDetailDto {
    AgentHubAssetDetailDto {
        asset_id: summary.asset_id,
        scope_id: summary.scope_id,
        kind: summary.kind,
        display_name: summary.display_name,
        logical_key: summary.logical_key,
        origin_namespace: summary.origin_namespace,
        policy: summary.policy,
        current_revision_id: summary.current_revision_id,
        targets: summary.targets,
        has_conflict: summary.has_conflict,
        aggregate_status: summary.aggregate_status,
        blocks,
        content_markdown,
        conflicts,
    }
}
