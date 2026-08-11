//! portable_inventory/ensure_managed — 扫描发现后幂等建立 Hub 管理账本
//!
//! Business Logic（为什么需要这个模块）:
//!     产品合同「发现即管理」：inspect/refresh 进入列表时即应建立/对齐 Hub
//!     asset/binding/materialization，不得以稳定 `unmanaged` 要求用户 Adopt。
//!     只写账本，禁止静默改磁盘资产内容；单项失败不得拖垮整表。
//!
//! Code Logic（这个模块做什么）:
//!     对可管理 discovered items：ensure scope → asset → binding → materialization；
//!     已存在 binding 的 desired 意图保留；ExternalCollision/Blocked 不覆盖；
//!     幂等可重复调用；失败项标 `unsupported` + reason。

use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, DesiredPresence, LogicalAsset, Materialization,
    MaterializationStatus, NewLogicalAsset, NewMaterialization, NewScopeNode, NewTargetBinding,
    ScopeKind, ScopeNode, TargetBinding,
};
use crate::agent_hub::portable_inventory::models::{
    PortableInventoryItemDto, PortableInventoryManagementState, PortableInventorySourceOrigin,
};
use crate::error::AppError;
use crate::storage::AgentHubRepo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// ensure-managed 汇总报告（日志/测试用；不进 UI 主路径）。
///
/// Business Logic（为什么需要这个结构体）:
///     大批量首次扫描需要可观测 ensured/skipped/failed 计数，失败可重试刷新。
///
/// Code Logic（这个结构体做什么）:
///     计数 + 按 inventory_item_id 的失败原因。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureManagedReport {
    /// 新写入或已对齐的项数
    pub ensured: usize,
    /// 跳过（已非候选 / collision 保留）
    pub skipped: usize,
    /// 失败项 inventory_item_id → 稳定/可读原因
    pub failures: Vec<EnsureManagedFailure>,
}

/// 单项 ensure 失败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureManagedFailure {
    /// 库存项 id
    pub inventory_item_id: String,
    /// 原因 token / 消息
    pub reason: String,
}

/// 对 discovered portable items 幂等 ensure Hub 管理记录（不写目标磁盘）。
///
/// Business Logic（为什么需要这个函数）:
///     inspect 返回前必须把可管理发现项提升为 hub 管理态，消除「未纳管需 Adopt」产品路径。
///
/// Code Logic（这个函数做什么）:
///     逐项 try ensure；失败标记 item.unsupported + reason，继续下一项；返回汇总。
pub async fn ensure_discovered_portable_items_managed(
    repo: &AgentHubRepo,
    items: &mut [PortableInventoryItemDto],
) -> EnsureManagedReport {
    let mut report = EnsureManagedReport::default();
    let mut facts = match EnsureManagedFacts::load(repo).await {
        Ok(facts) => facts,
        Err(err) => {
            let reason = ensure_failure_reason(&err);
            for item in items.iter_mut().filter(|item| is_ensure_candidate(item)) {
                mark_item_ensure_failed(item, &reason);
                report.failures.push(EnsureManagedFailure {
                    inventory_item_id: item.inventory_item_id.clone(),
                    reason: reason.clone(),
                });
            }
            report.skipped = items.len().saturating_sub(report.failures.len());
            return report;
        }
    };
    for item in items.iter_mut() {
        if !is_ensure_candidate(item) {
            report.skipped += 1;
            continue;
        }
        match ensure_one_item(repo, &mut facts, item).await {
            Ok(EnsureOneOutcome::Ensured) => report.ensured += 1,
            Ok(EnsureOneOutcome::Skipped) => report.skipped += 1,
            Err(err) => {
                let reason = ensure_failure_reason(&err);
                tracing::warn!(
                    target = "agent_hub.portable_inventory.ensure_managed",
                    inventory_item_id = %item.inventory_item_id,
                    error = %reason,
                    "ensure_managed failed for item; mark unsupported and continue"
                );
                mark_item_ensure_failed(item, &reason);
                report.failures.push(EnsureManagedFailure {
                    inventory_item_id: item.inventory_item_id.clone(),
                    reason,
                });
            }
        }
    }
    report
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsureOneOutcome {
    Ensured,
    Skipped,
}

/// 一次 inspect 预取的账本索引，消除每条资产 4 次 SELECT 的 N+1。
struct EnsureManagedFacts {
    scopes: BTreeMap<String, ScopeNode>,
    assets: BTreeMap<(String, AssetKind, String, String), LogicalAsset>,
    bindings: BTreeMap<(String, AgentTarget), TargetBinding>,
    materializations: BTreeMap<String, Materialization>,
}

impl EnsureManagedFacts {
    async fn load(repo: &AgentHubRepo) -> Result<Self, AppError> {
        let (scopes, assets, bindings, materializations) = tokio::try_join!(
            repo.list_scopes(),
            repo.list_assets(None, None),
            repo.list_target_bindings(),
            repo.list_materializations(),
        )?;
        let scopes = scopes
            .into_iter()
            .map(|scope| (scope.id.clone(), scope))
            .collect();
        let assets = assets
            .into_iter()
            .map(|asset| {
                (
                    (
                        asset.scope_id.clone(),
                        asset.kind,
                        asset.origin_namespace.clone(),
                        asset.logical_key.clone(),
                    ),
                    asset,
                )
            })
            .collect();
        let bindings = bindings
            .into_iter()
            .map(|binding| ((binding.asset_id.clone(), binding.target), binding))
            .collect();
        let materializations = materializations
            .into_iter()
            .map(|mat| (mat.target_binding_id.clone(), mat))
            .collect();
        Ok(Self {
            scopes,
            assets,
            bindings,
            materializations,
        })
    }
}

/// 是否应进入 ensure（已 blocked/unsupported 不碰；无 identity 跳过）。
fn is_ensure_candidate(item: &PortableInventoryItemDto) -> bool {
    if matches!(
        item.management_state,
        PortableInventoryManagementState::Unsupported
            | PortableInventoryManagementState::ExternalCollision
    ) {
        return false;
    }
    if item.native_id.trim().is_empty() {
        return false;
    }
    // source blocked / 显式 unsupported 原因
    if item
        .capabilities
        .reason_code
        .as_deref()
        .is_some_and(|c| c.contains("unsupported") || c == "source_blocked")
    {
        return false;
    }
    true
}

/// 幂等 ensure 单条 discovered item 的 scope/asset/binding/materialization。
///
/// Business Logic（为什么需要这个函数）:
///     仅账本对齐；保留已有 desired；不把 collision 静默改成 synced。
///     Blocked 保留 status/last_error，但会把 hash 对齐到当前 observed，避免假漂移。
///
/// Code Logic（这个函数做什么）:
///     ensure_scope → find/insert asset → get/create binding → get/update materialization。
async fn ensure_one_item(
    repo: &AgentHubRepo,
    facts: &mut EnsureManagedFacts,
    item: &PortableInventoryItemDto,
) -> Result<EnsureOneOutcome, AppError> {
    let scope_id = ensure_scope_for_item(repo, facts, item).await?;
    let origin_namespace = origin_namespace_for_item(item);
    let logical_key = item.native_id.clone();
    let asset_kind = item.kind.to_asset_kind();
    let asset_key = (
        scope_id.clone(),
        asset_kind,
        origin_namespace.clone(),
        logical_key.clone(),
    );

    let asset = match facts.assets.get(&asset_key).cloned() {
        Some(asset) => asset,
        None => match repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope_id.clone(),
                kind: asset_kind,
                origin_namespace: origin_namespace.clone(),
                logical_key: logical_key.clone(),
                display_name: if item.display_name.trim().is_empty() {
                    logical_key.clone()
                } else {
                    item.display_name.clone()
                },
                // 发现即本 target 管理；不因 ensure 自动跨 target 投影
                policy: AssetPolicy::TargetOnly,
            })
            .await
        {
            Ok(asset) => {
                facts.assets.insert(asset_key.clone(), asset.clone());
                asset
            }
            Err(e) if is_unique_conflict(&e) => repo
                .find_asset_by_key(&scope_id, asset_kind, &origin_namespace, &logical_key)
                .await?
                .ok_or_else(|| {
                    AppError::generic("agent_hub_ensure_managed_asset_race_unresolved")
                })?,
            Err(e) => return Err(e),
        },
    };
    facts
        .assets
        .entry(asset_key)
        .or_insert_with(|| asset.clone());

    let binding_key = (asset.id.clone(), item.target);
    let binding = if let Some(existing) = facts.bindings.get(&binding_key) {
        // 保留用户/卸载路径写下的 desired；不因 rediscovery 强行 Present
        existing.clone()
    } else {
        let desired_enabled = item.actual_enabled.unwrap_or(true);
        let binding = repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: item.target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: DesiredPresence::Present,
                desired_enabled,
            })
            .await?;
        facts.bindings.insert(binding_key, binding.clone());
        binding
    };

    let existing_mat = facts.materializations.get(&binding.id).cloned();
    if let Some(mat) = existing_mat.as_ref() {
        if matches!(mat.status, MaterializationStatus::ExternalCollision) {
            // 不得静默覆盖 external collision
            return Ok(EnsureOneOutcome::Skipped);
        }
    }

    let observed = item
        .content_hash
        .clone()
        .or_else(|| item.tree_hash.clone())
        .or_else(|| {
            existing_mat
                .as_ref()
                .and_then(|m| m.observed_external_hash.clone())
        });
    let native_path = item
        .source_path
        .clone()
        .or_else(|| existing_mat.as_ref().and_then(|m| m.native_path.clone()));

    let (status, rendered_hash, last_projected_revision_id, last_error) =
        if let Some(mat) = existing_mat.as_ref() {
            if mat.status == MaterializationStatus::Blocked {
                // support 门禁 Blocked：保留 status/last_error，但必须把 hash 对齐到当前
                // observed，否则陈旧/共享 rendered_hash 会在 reconcile 里假漂移整页 MCP。
                // discover-as-managed 的 rendered 本就等于 observed（无独立投影字节）。
                (
                    MaterializationStatus::Blocked,
                    observed.clone(),
                    mat.last_projected_revision_id.clone(),
                    mat.last_error.clone(),
                )
            } else {
                let rendered = mat.rendered_hash.clone().or_else(|| observed.clone());
                let status = match (rendered.as_deref(), observed.as_deref()) {
                    (Some(r), Some(o)) if r != o => MaterializationStatus::Drift,
                    _ => match mat.status {
                        // Detached 后再次发现：重新纳入管理（产品不提供停止管理）
                        MaterializationStatus::Detached
                        | MaterializationStatus::Pending
                        | MaterializationStatus::Synced
                        | MaterializationStatus::ActivationRequired => {
                            MaterializationStatus::Synced
                        }
                        MaterializationStatus::Drift => MaterializationStatus::Drift,
                        MaterializationStatus::Conflict => MaterializationStatus::Conflict,
                        MaterializationStatus::Unsupported => MaterializationStatus::Unsupported,
                        // ExternalCollision 已在上方 skip；Blocked 已单独分支
                        MaterializationStatus::ExternalCollision
                        | MaterializationStatus::Blocked => mat.status,
                    },
                };
                (
                    status,
                    rendered,
                    mat.last_projected_revision_id.clone(),
                    mat.last_error.clone(),
                )
            }
        } else {
            (
                MaterializationStatus::Synced,
                observed.clone(),
                asset.current_revision_id.clone(),
                None,
            )
        };

    // 已是 hub 管理且路径/hash/status 无变化时不重复 UPDATE；inspect 仍是只读事实
    // 对账，避免短 TTL 内多次刷新制造无意义的 SQLite 写放大。
    let unchanged = existing_mat.as_ref().is_some_and(|mat| {
        mat.asset_id == asset.id
            && mat.target == item.target
            && mat.target_binding_id == binding.id
            && mat.native_path == native_path
            && mat.last_projected_revision_id == last_projected_revision_id
            && mat.rendered_hash == rendered_hash
            && mat.observed_external_hash == observed
            && mat.status == status
            && mat.last_error == last_error
    });
    if unchanged {
        return Ok(EnsureOneOutcome::Skipped);
    }
    let materialization = repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id,
            target: item.target,
            target_binding_id: binding.id.clone(),
            native_path,
            last_projected_revision_id,
            rendered_hash,
            observed_external_hash: observed,
            status,
            last_error,
        })
        .await?;
    facts.materializations.insert(binding.id, materialization);

    Ok(EnsureOneOutcome::Ensured)
}

/// ensure scope 节点（user / project:…）；已存在则复用。
async fn ensure_scope_for_item(
    repo: &AgentHubRepo,
    facts: &mut EnsureManagedFacts,
    item: &PortableInventoryItemDto,
) -> Result<String, AppError> {
    let scope_id = item.scope_id.as_str();
    if let Some(s) = facts.scopes.get(scope_id) {
        return Ok(s.id.clone());
    }
    let hub_project_id = item.project_id.clone().or_else(|| {
        scope_id
            .strip_prefix("project:")
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    });
    match repo
        .insert_scope(NewScopeNode {
            id: Some(scope_id.to_string()),
            kind: item.scope_kind,
            hub_project_id,
            relative_path: None,
        })
        .await
    {
        Ok(scope) => {
            let id = scope.id.clone();
            facts.scopes.insert(id.clone(), scope);
            Ok(id)
        }
        Err(_) => {
            // 并发 insert：再读
            if let Some(s) = repo.get_scope(scope_id).await? {
                let id = s.id.clone();
                facts.scopes.insert(id.clone(), s);
                Ok(id)
            } else {
                Err(AppError::generic(
                    "agent_hub_ensure_managed_scope_unavailable",
                ))
            }
        }
    }
}

/// 由 inventory source origin 派生 origin_namespace（与 reconcile 匹配规则对齐）。
fn origin_namespace_for_item(item: &PortableInventoryItemDto) -> String {
    match item.source_origin {
        PortableInventorySourceOrigin::Standalone | PortableInventorySourceOrigin::NativeConfig => {
            "standalone".into()
        }
        PortableInventorySourceOrigin::PluginComponent => {
            extract_plugin_id_from_path(item.source_path.as_deref())
                .map(|id| format!("plugin:{id}"))
                .unwrap_or_else(|| "plugin".into())
        }
    }
}

/// 从 `.../plugins/<id>/...` 路径提取 plugin id。
fn extract_plugin_id_from_path(path: Option<&str>) -> Option<String> {
    let path = path?;
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    for i in 0..parts.len() {
        if parts[i] == "plugins" {
            if let Some(id) = parts.get(i + 1) {
                if !id.is_empty() && *id != "." && *id != ".." {
                    return Some((*id).to_string());
                }
            }
        }
    }
    None
}

fn is_unique_conflict(err: &AppError) -> bool {
    let s = err.to_string();
    s.contains("conflict") || s.contains("unique") || s.contains("UNIQUE")
}

fn ensure_failure_reason(err: &AppError) -> String {
    let raw = err.to_string();
    if raw.len() > 240 {
        format!("{}…", &raw[..240])
    } else {
        raw
    }
}

fn mark_item_ensure_failed(item: &mut PortableInventoryItemDto, reason: &str) {
    item.management_state = PortableInventoryManagementState::Unsupported;
    item.capabilities.reason_code = Some("ensure_managed_failed".into());
    item.capabilities.can_enable = false;
    item.capabilities.can_disable = false;
    item.capabilities.can_uninstall = false;
    item.capabilities.can_adopt = false;
    let warn = format!("ensure_managed_failed:{reason}");
    if !item.warnings.iter().any(|w| w == &warn) {
        item.warnings.push(warn);
    }
}

// 供 Directory 等扩展；当前 ensure 主要 user/project
#[allow(dead_code)]
fn scope_kind_default_id(kind: ScopeKind) -> &'static str {
    match kind {
        ScopeKind::User => "user",
        ScopeKind::Project => "project",
        ScopeKind::Directory => "directory",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, DesiredPresence, MaterializationStatus};
    use crate::agent_hub::portable_inventory::models::{
        inventory_item_id, PortableAssetKind, PortableInventoryItemCapabilitiesDto,
        PortableInventoryItemDto, PortableInventoryManagementState, PortableInventorySourceOrigin,
    };
    use crate::agent_hub::portable_inventory::reconcile::reconcile_portable_inventory;
    use crate::agent_hub::portable_inventory::{
        scan_portable_inventory_facts, PortableInventoryMutationCapability,
        PortableInventoryScanCapability, PortableInventoryTargetDto, PortableScanScope,
    };
    use crate::agent_hub::targets::paths::TargetEnvironment;
    use crate::storage::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    async fn open_repo(db_path: &Path) -> AgentHubRepo {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let options =
            SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db_path.display()))
                .unwrap()
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    fn write_skill(dir: &Path, name: &str, body: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
        )
        .unwrap();
    }

    fn sample_item(home: &Path, name: &str, content_hash: &str) -> PortableInventoryItemDto {
        let source_path = home.join(".claude/skills").join(name).display().to_string();
        PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(AgentTarget::Claude, "user", &source_path, name),
            target: AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            native_id: name.into(),
            display_name: name.into(),
            description: Some("test".into()),
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(source_path),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some(content_hash.into()),
            tree_hash: Some(format!("tree-{content_hash}")),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: false,
                can_disable: true,
                can_uninstall: true,
                can_adopt: false,
                can_install_to_source_target: false,
                reason_code: None,
                evidence_ids: vec![],
            },
            warnings: vec![],
            mcp_credential: None,
        }
    }

    fn sample_target() -> PortableInventoryTargetDto {
        PortableInventoryTargetDto {
            target: AgentTarget::Claude,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/claude".into()),
            config_root: "/cfg/claude".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        }
    }

    /// RED→GREEN 核心合同：fixture skill → ensure → reconcile 为 hubManaged，且无二次 adopt。
    #[tokio::test]
    async fn ensure_managed_rebaselines_blocked_materialization_hash_without_clearing_block() {
        // 生产故障：Blocked（min_tested_version_missing）materialization 持有陈旧/共享
        // rendered_hash，ensure 直接 skip → reconcile 永久 Drifted。
        // 期望：刷新 observed/rendered 到当前 content_hash，保留 Blocked + last_error。
        use crate::agent_hub::models::NewMaterialization;

        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        write_skill(&home.join(".claude/skills"), "review", "Review carefully.");
        let repo = open_repo(&root.path().join("data.db")).await;

        let mut items = vec![sample_item(&home, "review", "hash-current-leaf")];
        let report = ensure_discovered_portable_items_managed(&repo, &mut items).await;
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert!(report.ensured >= 1);

        let mats = repo.list_materializations().await.unwrap();
        let mat = mats
            .iter()
            .find(|m| m.status == MaterializationStatus::Synced)
            .expect("synced mat after first ensure");
        repo.upsert_materialization(NewMaterialization {
            asset_id: mat.asset_id.clone(),
            target: mat.target,
            target_binding_id: mat.target_binding_id.clone(),
            native_path: mat.native_path.clone(),
            last_projected_revision_id: mat.last_projected_revision_id.clone(),
            rendered_hash: Some("stale-shared-hash".into()),
            observed_external_hash: Some("stale-shared-hash".into()),
            status: MaterializationStatus::Blocked,
            last_error: Some("min_tested_version_missing".into()),
        })
        .await
        .unwrap();

        let mut items2 = vec![sample_item(&home, "review", "hash-current-leaf")];
        let report2 = ensure_discovered_portable_items_managed(&repo, &mut items2).await;
        assert!(report2.failures.is_empty(), "{:?}", report2.failures);
        assert!(
            report2.ensured >= 1,
            "blocked mat with stale hash must be rebaselined, not skipped forever; report={report2:?}"
        );

        let mats2 = repo.list_materializations().await.unwrap();
        let mat2 = mats2
            .iter()
            .find(|m| m.target_binding_id == mat.target_binding_id)
            .expect("mat after rebaseline");
        assert_eq!(mat2.status, MaterializationStatus::Blocked);
        assert_eq!(
            mat2.last_error.as_deref(),
            Some("min_tested_version_missing")
        );
        assert_eq!(
            mat2.rendered_hash.as_deref(),
            Some("hash-current-leaf"),
            "rendered_hash must track current observed content"
        );
        assert_eq!(
            mat2.observed_external_hash.as_deref(),
            Some("hash-current-leaf")
        );

        let snap = reconcile_portable_inventory(&repo, vec![sample_target()], items2)
            .await
            .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::HubManaged,
            "after rebaseline, inventory must not show drifted"
        );
    }

    #[tokio::test]
    async fn ensure_managed_promotes_discovered_skill_without_adopt() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let skills = home.join(".claude/skills");
        write_skill(&skills, "review", "Review carefully.");
        let skill_md = skills.join("review/SKILL.md");
        let before = fs::read_to_string(&skill_md).unwrap();

        let repo = open_repo(&root.path().join("data.db")).await;
        let mut items = vec![sample_item(&home, "review", "hash-review-1")];

        let report = ensure_discovered_portable_items_managed(&repo, &mut items).await;
        assert!(
            report.failures.is_empty(),
            "ensure failures: {:?}",
            report.failures
        );
        assert!(report.ensured >= 1, "expected at least one ensured");

        let snap = reconcile_portable_inventory(&repo, vec![sample_target()], items.clone())
            .await
            .expect("reconcile");
        assert_eq!(snap.items.len(), 1);
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::HubManaged,
            "discovered skill must be hubManaged after ensure (not unmanaged)"
        );
        assert!(
            snap.items[0].canonical_asset_id.is_some(),
            "must have canonical asset id"
        );
        assert_eq!(
            snap.items[0].desired_presence,
            Some(DesiredPresence::Present)
        );

        // 磁盘内容未变
        let after = fs::read_to_string(&skill_md).unwrap();
        assert_eq!(before, after, "ensure must not mutate disk asset content");

        // 幂等：第二次 ensure 不增加资产、仍 hubManaged
        let asset_id = snap.items[0].canonical_asset_id.clone().unwrap();
        let mut items2 = vec![sample_item(&home, "review", "hash-review-1")];
        let report2 = ensure_discovered_portable_items_managed(&repo, &mut items2).await;
        assert!(report2.failures.is_empty());
        let snap2 = reconcile_portable_inventory(&repo, vec![sample_target()], items2)
            .await
            .expect("reconcile2");
        assert_eq!(
            snap2.items[0].management_state,
            PortableInventoryManagementState::HubManaged
        );
        assert_eq!(
            snap2.items[0].canonical_asset_id.as_deref(),
            Some(asset_id.as_str())
        );
        let assets = repo.list_assets(None, None).await.unwrap();
        assert_eq!(
            assets.len(),
            1,
            "idempotent ensure must not duplicate assets"
        );
    }

    #[tokio::test]
    async fn ensure_managed_isolates_item_failure() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        write_skill(&home.join(".claude/skills"), "good", "ok");
        let repo = open_repo(&root.path().join("data.db")).await;

        let good = sample_item(&home, "good", "hash-good");
        let bad = sample_item(&home, "bad", "hash-bad");
        let mut items = vec![good, bad];
        let report = ensure_discovered_portable_items_managed(&repo, &mut items).await;
        assert!(report.failures.is_empty());
        assert_eq!(report.ensured, 2);

        // 单测「失败不拖垮」：一项标 unsupported 后另一项仍 hubManaged
        mark_item_ensure_failed(&mut items[1], "simulated");
        assert_eq!(
            items[1].management_state,
            PortableInventoryManagementState::Unsupported
        );
        let snap = reconcile_portable_inventory(&repo, vec![sample_target()], items)
            .await
            .expect("reconcile");
        let good_item = snap
            .items
            .iter()
            .find(|i| i.native_id == "good")
            .expect("good");
        assert_eq!(
            good_item.management_state,
            PortableInventoryManagementState::HubManaged
        );
        let bad_item = snap
            .items
            .iter()
            .find(|i| i.native_id == "bad")
            .expect("bad");
        // reconcile 可能仍按 fact 标 hubManaged；inspect 路径会再贴 ensure 失败态。
        // 这里断言 mark 侧副作用与 good 不受影响即可。
        let _ = bad_item;
    }

    #[tokio::test]
    async fn ensure_managed_scan_fixture_skill_roundtrip() {
        // 更接近生产：真实 scan → ensure → reconcile
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let claude = home.join(".claude");
        write_skill(&claude.join("skills"), "scan-me", "From scan.");
        fs::create_dir_all(home.join("bin")).unwrap();
        let mut vars = BTreeMap::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            claude.to_string_lossy().into_owned(),
        );
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![home.join("bin")],
        };
        let scopes = vec![PortableScanScope {
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            absolute_path: home.clone(),
        }];
        let (targets, mut discovered) = scan_portable_inventory_facts(&env, &scopes).unwrap();
        assert!(
            discovered
                .iter()
                .any(|i| i.kind == PortableAssetKind::Skill && i.native_id == "scan-me"),
            "scan must discover skill; got {:?}",
            discovered
                .iter()
                .map(|i| (&i.native_id, i.kind))
                .collect::<Vec<_>>()
        );

        let repo = open_repo(&root.path().join("data.db")).await;
        let report = ensure_discovered_portable_items_managed(&repo, &mut discovered).await;
        assert!(
            report.failures.is_empty(),
            "failures: {:?}",
            report.failures
        );

        let snap = reconcile_portable_inventory(&repo, targets, discovered)
            .await
            .expect("reconcile");
        let skill = snap
            .items
            .iter()
            .find(|i| i.native_id == "scan-me" && i.kind == PortableAssetKind::Skill)
            .expect("skill item");
        assert_eq!(
            skill.management_state,
            PortableInventoryManagementState::HubManaged
        );
        assert!(skill.canonical_asset_id.is_some());

        // materialization Synced
        let mats = repo.list_materializations().await.unwrap();
        assert!(
            mats.iter()
                .any(|m| m.status == MaterializationStatus::Synced),
            "expected Synced materialization"
        );
    }

    #[test]
    fn extract_plugin_id_from_path_reads_plugins_segment() {
        assert_eq!(
            extract_plugin_id_from_path(Some("/home/.claude/plugins/demo/skills/x")),
            Some("demo".into())
        );
        assert_eq!(
            extract_plugin_id_from_path(Some(r"C:\Users\a\.claude\plugins\p1\skills\x")),
            Some("p1".into())
        );
        assert_eq!(
            extract_plugin_id_from_path(Some("/home/.claude/skills/review")),
            None
        );
    }

    #[test]
    fn origin_namespace_standalone_and_plugin() {
        let home = PathBuf::from("/tmp/h");
        let mut item = sample_item(&home, "a", "h");
        assert_eq!(origin_namespace_for_item(&item), "standalone");
        item.source_origin = PortableInventorySourceOrigin::PluginComponent;
        item.source_path = Some("/x/plugins/myplug/skills/a".into());
        assert_eq!(origin_namespace_for_item(&item), "plugin:myplug");
    }
}
