//! agent_hub/plugins/ownership — 引用派生的 component 删除决策
//!
//! Business Logic（为什么需要这个模块）:
//!     删除 package 不得误删被其他 package head 或 standalone 引用的 component；
//!     引用数必须从边表实时查询，禁止维护易漂移计数器。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `ComponentDeleteDecision`；提供基于 live package-head refs 与 standalone 边的决策函数；
//!     事务内决策在 repo 路径执行（见 `AgentHubRepo::decide_component_delete`）。

use serde::{Deserialize, Serialize};

/// package 删除时对单个 component 的处置。
///
/// Business Logic（为什么需要这个枚举）:
///     仅 package 独占且无 standalone 引用的 component 才能 tombstone；共享/独立引用必须保留。
///
/// Code Logic（这个枚举做什么）:
///     camelCase wire；as_str / parse 供日志与测试断言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComponentDeleteDecision {
    /// 无其它 live package head 与 standalone 引用 → 可 tombstone
    TombstoneOwned,
    /// 仍被其它 live package head 引用 → 保留
    PreserveShared,
    /// 存在 standalone 逻辑资产边 → 保留
    PreserveStandalone,
}

impl ComponentDeleteDecision {
    /// 稳定 wire/日志字符串。
    ///
    /// Business Logic: 删除 preview 与测试断言依赖稳定 token。
    /// Code Logic: camelCase。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TombstoneOwned => "tombstoneOwned",
            Self::PreserveShared => "preserveShared",
            Self::PreserveStandalone => "preserveStandalone",
        }
    }

    /// 解析 wire 字符串。
    ///
    /// Business Logic: 未知决策 fail-closed。
    /// Code Logic: 仅匹配 as_str。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tombstoneOwned" => Some(Self::TombstoneOwned),
            "preserveShared" => Some(Self::PreserveShared),
            "preserveStandalone" => Some(Self::PreserveStandalone),
            _ => None,
        }
    }
}

/// 纯函数：根据引用存在性决定删除处置。
///
/// Business Logic（为什么需要这个函数）:
///     决策顺序固定：standalone 优先于 shared；二者皆无才 tombstone。
///     调用方须在删除事务内用**最新** live head 读，禁止使用陈旧计数。
///
/// Code Logic（这个函数做什么）:
///     `has_standalone` → PreserveStandalone；
///     `other_live_package_head_refs > 0` → PreserveShared；
///     否则 TombstoneOwned。
///     `other_live_package_head_refs` **不含**正在删除的 package 自身。
pub fn decide_component_delete(
    has_standalone_ref: bool,
    other_live_package_head_refs: u64,
) -> ComponentDeleteDecision {
    if has_standalone_ref {
        return ComponentDeleteDecision::PreserveStandalone;
    }
    if other_live_package_head_refs > 0 {
        return ComponentDeleteDecision::PreserveShared;
    }
    ComponentDeleteDecision::TombstoneOwned
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::assets::{PortableAssetPayload, PortableSkill};
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode,
        RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::agent_hub::object_store::{ObjectStore, TreeEntry, TreeEntryType, TreeManifest};
    use crate::agent_hub::plugins::models::{
        ComponentOwnership, PluginComponentRef, PluginPackagePayload,
    };
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use tempfile::TempDir;

    async fn test_repo() -> AgentHubRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    async fn test_store() -> (TempDir, ObjectStore) {
        let dir = TempDir::new().unwrap();
        let store = ObjectStore::open(dir.path()).unwrap();
        (dir, store)
    }

    async fn user_scope(repo: &AgentHubRepo) -> crate::agent_hub::models::ScopeNode {
        repo.insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap()
    }

    async fn seed_skill(
        repo: &AgentHubRepo,
        store: &ObjectStore,
        scope_id: &str,
        origin_namespace: &str,
        key: &str,
        body: &str,
    ) -> (
        crate::agent_hub::models::LogicalAsset,
        crate::agent_hub::models::Revision,
    ) {
        let md = store.put_blob(body.as_bytes()).await.unwrap();
        let tree = store
            .put_tree(&TreeManifest {
                entries: vec![TreeEntry {
                    path: "SKILL.md".into(),
                    blob_hash: md.hash.clone(),
                    entry_type: TreeEntryType::File,
                    executable: false,
                }],
            })
            .await
            .unwrap();
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope_id.into(),
                kind: AssetKind::Skill,
                origin_namespace: origin_namespace.into(),
                logical_key: key.into(),
                display_name: key.into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let rev = repo
            .append_portable_asset_revision(
                &asset.id,
                &PortableAssetPayload::Skill(PortableSkill {
                    name: key.into(),
                    description: "d".into(),
                    skill_markdown_hash: md.hash,
                    tree_manifest_hash: tree.hash,
                    target_extensions: Default::default(),
                }),
                store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();
        (asset, rev)
    }

    async fn seed_plugin(
        repo: &AgentHubRepo,
        store: &ObjectStore,
        scope_id: &str,
        plugin_id: &str,
        skill_asset_id: &str,
        skill_rev: &RevisionId,
        ownership: ComponentOwnership,
    ) -> (
        crate::agent_hub::models::LogicalAsset,
        crate::agent_hub::models::Revision,
    ) {
        let plugin = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope_id.into(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: plugin_id.into(),
                display_name: plugin_id.into(),
                policy: AssetPolicy::TargetOnly,
            })
            .await
            .unwrap();
        let payload = PluginPackagePayload {
            plugin_id: plugin_id.into(),
            name: plugin_id.into(),
            version: Some("1".into()),
            description: None,
            source_target: AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill_asset_id.into(),
                revision_id: skill_rev.clone(),
                ownership,
            }],
            residual_refs: vec![],
            target_extensions: Default::default(),
        };
        let rev = repo
            .append_plugin_package_revision(
                &plugin.id,
                &payload,
                store,
                RevisionOriginKind::Ui,
                Some(AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();
        (plugin, rev)
    }

    #[test]
    fn pure_decision_order_standalone_then_shared_then_owned() {
        assert_eq!(
            decide_component_delete(true, 99),
            ComponentDeleteDecision::PreserveStandalone
        );
        assert_eq!(
            decide_component_delete(false, 1),
            ComponentDeleteDecision::PreserveShared
        );
        assert_eq!(
            decide_component_delete(false, 0),
            ComponentDeleteDecision::TombstoneOwned
        );
    }

    /// package A 独占 S；package B 共享 S；standalone 也引用 S。
    /// 删除 A 保留 S；删 B 后 standalone 仍保留；仅显式 standalone 删除后才 tombstone。
    #[tokio::test]
    async fn package_delete_preserves_shared_and_standalone_until_last_ref() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let (_dir, store) = test_store().await;

        // skill S under plugin namespace first; standalone link later
        let (skill, s1) = seed_skill(
            &repo,
            &store,
            &scope.id,
            "plugin:pkg-a",
            "shared-skill",
            "skill-body",
        )
        .await;

        let (pkg_a, _pa) = seed_plugin(
            &repo,
            &store,
            &scope.id,
            "pkg-a",
            &skill.id,
            &s1.id,
            ComponentOwnership::PackageOwned,
        )
        .await;
        let (pkg_b, _pb) = seed_plugin(
            &repo,
            &store,
            &scope.id,
            "pkg-b",
            &skill.id,
            &s1.id,
            ComponentOwnership::Shared,
        )
        .await;

        // standalone logical asset pointing at same component
        let standalone = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope.id.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "standalone".into(),
                logical_key: "shared-skill".into(),
                display_name: "standalone shared".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        // reuse same payload revision under standalone asset by linking edge only
        repo.upsert_component_standalone_ref(&skill.id, &standalone.id)
            .await
            .unwrap();

        // Delete A → skill preserved (B + standalone)
        let dec_a = repo
            .delete_plugin_package_with_ownership(
                &pkg_a.id,
                &store,
                RevisionOriginKind::Ui,
                "01900000-0000-7000-8000-0000000000d1",
            )
            .await
            .unwrap();
        assert!(dec_a
            .component_decisions
            .iter()
            .any(|d| d.component_asset_id == skill.id
                && d.decision == ComponentDeleteDecision::PreserveStandalone));
        let skill_after_a = repo.get_asset(&skill.id).await.unwrap().unwrap();
        assert!(skill_after_a.deleted_at.is_none());
        // package A tombstoned; old package revision still loadable (immutable)
        let pkg_a_asset = repo.get_asset(&pkg_a.id).await.unwrap().unwrap();
        assert!(pkg_a_asset.deleted_at.is_some());
        let hist = repo
            .load_plugin_package_revision(pkg_a_asset.current_revision_id.as_ref().unwrap(), &store)
            .await
            .unwrap();
        // delete tombstone has no payload
        assert!(hist.is_none());
        // but historical package revision before tombstone remains via previous id chain
        // (edge rows for non-deleted revision still present — use listed components from
        // the revision that was head before delete by reading parent of current delete)
        let delete_rev = repo
            .get_revision(pkg_a_asset.current_revision_id.as_ref().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delete_rev.operation, RevisionOperation::Delete);
        assert!(!delete_rev.parents.is_empty());
        let old_pkg_rev = &delete_rev.parents[0];
        let old_comps = repo
            .list_plugin_components_for_revision(old_pkg_rev.as_str())
            .await
            .unwrap();
        assert_eq!(old_comps.len(), 1);
        assert_eq!(old_comps[0].revision_id, s1.id);

        // Delete B → still preserved by standalone
        let dec_b = repo
            .delete_plugin_package_with_ownership(
                &pkg_b.id,
                &store,
                RevisionOriginKind::Ui,
                "01900000-0000-7000-8000-0000000000d1",
            )
            .await
            .unwrap();
        assert!(dec_b.component_decisions.iter().any(|d| {
            d.component_asset_id == skill.id
                && d.decision == ComponentDeleteDecision::PreserveStandalone
        }));
        let skill_after_b = repo.get_asset(&skill.id).await.unwrap().unwrap();
        assert!(skill_after_b.deleted_at.is_none());

        // Explicit standalone deletion of skill (component) may tombstone
        let tombstone = NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: skill.id.clone(),
            parents: skill
                .current_revision_id
                .clone()
                .or(Some(s1.id.clone()))
                .into_iter()
                .collect(),
            operation: RevisionOperation::Delete,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: Some(AgentTarget::Claude),
            origin_replica_id: "01900000-0000-7000-8000-0000000000d1".into(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expected_parent_id: skill.current_revision_id.clone().or(Some(s1.id.clone())),
        };
        // Clear standalone edge first then decide
        repo.clear_component_standalone_refs(&skill.id)
            .await
            .unwrap();
        let decision = repo.decide_component_delete(&skill.id, None).await.unwrap();
        assert_eq!(decision, ComponentDeleteDecision::TombstoneOwned);
        let _ = repo
            .delete_asset_everywhere_atomic(&skill.id, tombstone, vec![])
            .await
            .unwrap();
        let skill_final = repo.get_asset(&skill.id).await.unwrap().unwrap();
        assert!(skill_final.deleted_at.is_some());
    }

    #[tokio::test]
    async fn deleting_only_owner_package_tombstones_component() {
        let repo = test_repo().await;
        let scope = user_scope(&repo).await;
        let (_dir, store) = test_store().await;
        let (skill, s1) = seed_skill(
            &repo,
            &store,
            &scope.id,
            "plugin:only",
            "only-skill",
            "body",
        )
        .await;
        let (pkg, _) = seed_plugin(
            &repo,
            &store,
            &scope.id,
            "only-pkg",
            &skill.id,
            &s1.id,
            ComponentOwnership::PackageOwned,
        )
        .await;
        let result = repo
            .delete_plugin_package_with_ownership(
                &pkg.id,
                &store,
                RevisionOriginKind::Ui,
                "01900000-0000-7000-8000-0000000000d1",
            )
            .await
            .unwrap();
        assert!(result.component_decisions.iter().any(|d| {
            d.component_asset_id == skill.id
                && d.decision == ComponentDeleteDecision::TombstoneOwned
        }));
        let skill_after = repo.get_asset(&skill.id).await.unwrap().unwrap();
        assert!(skill_after.deleted_at.is_some());
    }
}
