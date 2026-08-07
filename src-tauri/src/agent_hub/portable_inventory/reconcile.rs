//! portable_inventory/reconcile — Inventory ↔ Canonical 只读对账
//!
//! Business Logic（为什么需要这个模块）:
//!     扫描结果只描述目标事实；对账输出 unmanaged/hubManaged/drifted/externalCollision/unsupported。
//!     不得因扫描缺失 tombstone canonical；不得因 desired presence 推断本机文件存在；
//!     不得静默合并 standalone 与 plugin component；不得写 adoption/binding/revision 表。
//!
//! Code Logic（这个模块做什么）:
//!     `reconcile_portable_inventory_with_facts` 纯函数驱动五态；
//!     `reconcile_portable_inventory` 从 `AgentHubRepo` 只读预取 facts 后调用纯函数。

use crate::agent_hub::models::{
    AdoptionState, AgentTarget, DesiredPresence, MaterializationStatus,
};
use crate::agent_hub::portable_inventory::models::{
    inventory_snapshot_hash, PortableAssetKind, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventorySnapshotDto, PortableInventorySourceOrigin,
    PortableInventoryTargetDto,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// 预取的 canonical 对账事实（只读；非持久写模型）。
///
/// Business Logic（为什么需要这个结构体）:
///     单元测试可在无 SQLite 时驱动五态；生产路径从 repo 组装同等事实。
///
/// Code Logic（这个结构体做什么）:
///     聚合 asset/binding/materialization/ownership 匹配字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortableCanonicalFact {
    /// logical asset id
    pub asset_id: String,
    /// scope id
    pub scope_id: String,
    /// 四类 kind
    pub kind: PortableAssetKind,
    /// origin namespace（standalone / plugin:<id>）
    pub origin_namespace: String,
    /// 逻辑键
    pub logical_key: String,
    /// target
    pub target: AgentTarget,
    /// 是否 Hub 拥有（committed adoption 或 materialization 绑定）
    pub hub_owned: bool,
    /// desired presence
    pub desired_presence: Option<DesiredPresence>,
    /// desired enabled
    pub desired_enabled: Option<bool>,
    /// materialization status
    pub materialization_status: Option<MaterializationStatus>,
    /// 上次成功投影内容 hash
    pub rendered_hash: Option<String>,
    /// 观测外部 hash
    pub observed_external_hash: Option<String>,
    /// 上次投影 revision
    pub last_projected_revision_id: Option<String>,
    /// 原生路径
    pub native_path: Option<String>,
    /// adapter/版本不支持
    pub unsupported: bool,
    /// 外部碰撞标记
    pub external_collision: bool,
}

/// 使用预取 facts 对账并生成快照（纯函数，不访问 DB）。
///
/// Business Logic（为什么需要这个函数）:
///     对账必须只读；单元测试可直接驱动五态且保证不写表。
///
/// Code Logic（这个函数做什么）:
///     对每条 discovered 按 target/scope/origin/logical key/source identity/hash 匹配 facts，
///     填 managementState 与 desired 摘要；排序后算 snapshot hash。
pub fn reconcile_portable_inventory_with_facts(
    targets: Vec<PortableInventoryTargetDto>,
    discovered: Vec<PortableInventoryItemDto>,
    facts: &[PortableCanonicalFact],
) -> Result<PortableInventorySnapshotDto, AppError> {
    let mut items = Vec::with_capacity(discovered.len());
    for mut item in discovered {
        let fact = find_matching_fact(&item, facts);
        apply_reconcile_state(&mut item, fact);
        items.push(item);
    }

    // 稳定排序：target → kind → inventory_item_id
    items.sort_by(|a, b| {
        a.target
            .as_str()
            .cmp(b.target.as_str())
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
            .then_with(|| a.inventory_item_id.cmp(&b.inventory_item_id))
    });

    let mut targets = targets;
    targets.sort_by(|a, b| {
        a.target
            .as_str()
            .cmp(b.target.as_str())
            .then_with(|| a.config_root.cmp(&b.config_root))
    });

    let inventory_snapshot_hash = inventory_snapshot_hash(&targets, &items)?;
    Ok(PortableInventorySnapshotDto {
        inventory_snapshot_hash,
        refreshed_at: Utc::now().to_rfc3339(),
        stale: false,
        targets,
        items,
    })
}

/// 从 AgentHubRepo 只读预取后对账。
///
/// Business Logic（为什么需要这个函数）:
///     生产 inspect 入口：刷新库存只读，不创建/修改 canonical、revision、binding、ownership。
///
/// Code Logic（这个函数做什么）:
///     list assets/bindings/materializations/adoptions → PortableCanonicalFact → with_facts。
///     不写任何表。
pub async fn reconcile_portable_inventory(
    repo: &AgentHubRepo,
    targets: Vec<PortableInventoryTargetDto>,
    discovered: Vec<PortableInventoryItemDto>,
) -> Result<PortableInventorySnapshotDto, AppError> {
    let facts = load_canonical_facts(repo).await?;
    reconcile_portable_inventory_with_facts(targets, discovered, &facts)
}

/// 只读加载对账事实。
///
/// Business Logic: 仅 SELECT；失败上抛，不部分写回。
/// Code Logic: assets × bindings × materializations × committed adoptions。
async fn load_canonical_facts(repo: &AgentHubRepo) -> Result<Vec<PortableCanonicalFact>, AppError> {
    let assets = repo.list_assets(None, None).await?;
    let materializations = repo.list_materializations().await?;
    let adoptions = repo.list_adoptions().await?;

    let mut facts = Vec::new();
    for asset in assets {
        let kind = match PortableAssetKind::try_from_asset_kind(asset.kind) {
            Ok(k) => k,
            Err(_) => continue, // 跳过 Instruction/Agent/Hook
        };
        let bindings = repo.list_target_bindings_for_asset(&asset.id).await?;
        // 无 binding 时仍可按 asset 身份匹配（hub_owned=false → unmanaged/无 desired）
        if bindings.is_empty() {
            for target in [
                AgentTarget::Claude,
                AgentTarget::Codex,
                AgentTarget::OpenCode,
            ] {
                facts.push(PortableCanonicalFact {
                    asset_id: asset.id.clone(),
                    scope_id: asset.scope_id.clone(),
                    kind,
                    origin_namespace: asset.origin_namespace.clone(),
                    logical_key: asset.logical_key.clone(),
                    target,
                    hub_owned: false,
                    desired_presence: None,
                    desired_enabled: None,
                    materialization_status: None,
                    rendered_hash: None,
                    observed_external_hash: None,
                    last_projected_revision_id: asset
                        .current_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string()),
                    native_path: None,
                    unsupported: false,
                    external_collision: false,
                });
            }
            continue;
        }
        for binding in bindings {
            let mat = materializations
                .iter()
                .find(|m| m.target_binding_id == binding.id && m.asset_id == asset.id);
            let committed_adoption = adoptions.iter().any(|a| {
                a.asset_id.as_deref() == Some(asset.id.as_str())
                    && a.target == binding.target
                    && a.state == AdoptionState::Committed
            });
            let hub_owned = mat.is_some() || committed_adoption;
            let materialization_status = mat.map(|m| m.status);
            let unsupported = matches!(
                materialization_status,
                Some(MaterializationStatus::Unsupported)
            );
            let external_collision = matches!(
                materialization_status,
                Some(MaterializationStatus::ExternalCollision)
            ) || adoptions.iter().any(|a| {
                a.asset_id.as_deref() == Some(asset.id.as_str())
                    && a.target == binding.target
                    && a.state == AdoptionState::ExternalCollision
            });
            facts.push(PortableCanonicalFact {
                asset_id: asset.id.clone(),
                scope_id: asset.scope_id.clone(),
                kind,
                origin_namespace: asset.origin_namespace.clone(),
                logical_key: asset.logical_key.clone(),
                target: binding.target,
                hub_owned,
                desired_presence: Some(binding.desired_presence),
                desired_enabled: Some(binding.desired_enabled),
                materialization_status,
                rendered_hash: mat.and_then(|m| m.rendered_hash.clone()),
                observed_external_hash: mat.and_then(|m| m.observed_external_hash.clone()),
                last_projected_revision_id: mat.and_then(|m| {
                    m.last_projected_revision_id
                        .as_ref()
                        .map(|r| r.as_str().to_string())
                }),
                native_path: mat.and_then(|m| m.native_path.clone()),
                unsupported,
                external_collision,
            });
        }
    }
    Ok(facts)
}

fn origin_namespace_matches(item: &PortableInventoryItemDto, namespace: &str) -> bool {
    match item.source_origin {
        PortableInventorySourceOrigin::Standalone | PortableInventorySourceOrigin::NativeConfig => {
            namespace == "standalone" || namespace.is_empty()
        }
        PortableInventorySourceOrigin::PluginComponent => {
            namespace.starts_with("plugin:") || namespace == "plugin"
        }
    }
}

fn find_matching_fact<'a>(
    item: &PortableInventoryItemDto,
    facts: &'a [PortableCanonicalFact],
) -> Option<&'a PortableCanonicalFact> {
    // 优先：target + scope + origin namespace + logical key(+ native_id) + source path
    let mut candidates: Vec<&PortableCanonicalFact> = facts
        .iter()
        .filter(|f| {
            f.target == item.target
                && f.scope_id == item.scope_id
                && f.kind == item.kind
                && origin_namespace_matches(item, &f.origin_namespace)
                && (f.logical_key == item.native_id || f.logical_key == item.display_name)
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // 若有 native_path，优先精确路径
    if let Some(path) = item.source_path.as_deref() {
        if let Some(exact) = candidates
            .iter()
            .copied()
            .find(|f| f.native_path.as_deref() == Some(path))
        {
            return Some(exact);
        }
    }

    // plugin component：origin_namespace 更具体者优先
    if item.source_origin == PortableInventorySourceOrigin::PluginComponent {
        candidates.sort_by_key(|f| std::cmp::Reverse(f.origin_namespace.len()));
    }

    candidates.into_iter().next()
}

fn apply_reconcile_state(
    item: &mut PortableInventoryItemDto,
    fact: Option<&PortableCanonicalFact>,
) {
    let Some(fact) = fact else {
        item.management_state = PortableInventoryManagementState::Unmanaged;
        item.canonical_asset_id = None;
        item.canonical_revision_id = None;
        item.desired_presence = None;
        item.desired_enabled = None;
        item.materialization_status = None;
        return;
    };

    item.canonical_asset_id = Some(fact.asset_id.clone());
    item.canonical_revision_id = fact.last_projected_revision_id.clone();
    item.desired_presence = fact.desired_presence;
    item.desired_enabled = fact.desired_enabled;
    item.materialization_status = fact.materialization_status.map(|s| s.as_str().to_string());

    // unsupported 优先（adapter/版本缺语义）
    if fact.unsupported
        || matches!(
            fact.materialization_status,
            Some(MaterializationStatus::Unsupported)
        )
        || item
            .capabilities
            .reason_code
            .as_deref()
            .is_some_and(|c| c.contains("unsupported"))
    {
        item.management_state = PortableInventoryManagementState::Unsupported;
        return;
    }

    // 外部碰撞
    if fact.external_collision
        || matches!(
            fact.materialization_status,
            Some(MaterializationStatus::ExternalCollision)
        )
    {
        item.management_state = PortableInventoryManagementState::ExternalCollision;
        return;
    }

    if !fact.hub_owned {
        item.management_state = PortableInventoryManagementState::Unmanaged;
        return;
    }

    // Hub ownership：比较 observed 内容/树 hash 与 rendered/applied
    let observed = item
        .content_hash
        .as_deref()
        .or(item.tree_hash.as_deref())
        .or(fact.observed_external_hash.as_deref());
    let expected = fact
        .rendered_hash
        .as_deref()
        .or(fact.observed_external_hash.as_deref());

    let hash_consistent = match (observed, expected) {
        (Some(o), Some(e)) => o == e,
        // 无 hash 可比较时，若 materialization 明确 Drift 则 drifted，否则保守 hubManaged
        _ => !matches!(
            fact.materialization_status,
            Some(MaterializationStatus::Drift)
        ),
    };

    if matches!(
        fact.materialization_status,
        Some(MaterializationStatus::Drift)
    ) || !hash_consistent
    {
        item.management_state = PortableInventoryManagementState::Drifted;
        return;
    }

    item.management_state = PortableInventoryManagementState::HubManaged;
}
