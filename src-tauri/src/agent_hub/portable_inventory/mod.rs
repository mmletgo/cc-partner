//! agent_hub/portable_inventory — 四类资产真实库存 + 发现即管理
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub Portable Assets 需要展示本机实际 Skill/Command/Plugin/MCP 库存，
//!     并与 Hub canonical/binding/materialization 对账；扫描事实不得冒充 desired 状态。
//!     inspect/refresh 在返回前对可管理发现项幂等 ensure 管理账本（不写目标磁盘内容）。
//!
//! Code Logic（这个模块做什么）:
//!     定义库存 DTO、稳定 inventory_item_id、确定性 inventory_snapshot_hash，
//!     ensure_managed（ledger only）+ 只读 reconcile；不写目标文件/CAS 原字节。

pub mod cache;
pub mod ensure_managed;
pub mod models;
pub mod plugin_enablement;
pub mod plugin_paths;
pub mod reconcile;
pub mod scanner;

pub use crate::agent_hub::targets::portable::{PortableAssetOwner, PortableOriginKind};
pub use cache::invalidate_portable_inventory_cache;
pub use ensure_managed::{
    ensure_discovered_portable_items_managed, EnsureManagedFailure, EnsureManagedReport,
};
pub use models::{
    inventory_item_id, inventory_snapshot_hash, PortableAssetKind,
    PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryMutationCapability, PortableInventoryQuery,
    PortableInventoryScanCapability, PortableInventorySnapshotDto, PortableInventorySourceOrigin,
    PortableInventoryTargetDto, PortableMcpCredentialFactDto, PortableStoreFactDto,
};
pub use plugin_paths::{
    infer_plugin_package_root, is_plugin_infrastructure_name, is_plugin_infrastructure_path,
    plugin_id_from_path,
};
pub use reconcile::{
    reconcile_portable_inventory, reconcile_portable_inventory_with_facts, PortableCanonicalFact,
};
pub(crate) use scanner::evaluate_current_portable_target_support;
pub use scanner::{
    hash_directory_tree, hash_plugin_root, inspect_portable_inventory,
    inspect_portable_inventory_force, inspect_portable_inventory_force_query,
    inspect_portable_inventory_force_with_env, inspect_portable_inventory_force_with_env_query,
    inspect_portable_inventory_query, inspect_portable_inventory_with_env,
    inspect_portable_inventory_with_env_query, scan_portable_inventory_facts,
    scan_portable_inventory_facts_query, PortableScanScope,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, DesiredPresence, MaterializationStatus};

    #[test]
    fn inventory_identity_is_path_independent_and_distinguishes_logical_origin() {
        // 路径无关契约：source_identity 用 origin_namespace（"standalone"）而非绝对路径，
        // 所以同一逻辑资产在 active 路径与 disabled 路径下产出相同 id。
        // 这是 enable/disable 物理移动文件后 rescan 对账不误报 MISSING 的核心契约。
        let active_id = inventory_item_id(AgentTarget::Claude, "user", "standalone", "tool");
        let disabled_id = inventory_item_id(AgentTarget::Claude, "user", "standalone", "tool");
        assert_eq!(
            active_id, disabled_id,
            "path-independent source_identity must yield same id for the same logical asset"
        );
        // 对照：若把绝对路径塞进 source_identity（旧设计），id 会随路径变化——这正是本次重构消除的漂移。
        let legacy_active = inventory_item_id(AgentTarget::Claude, "user", "/x/active", "tool");
        let legacy_disabled = inventory_item_id(AgentTarget::Claude, "user", "/x/disabled", "tool");
        assert_ne!(
            legacy_active, legacy_disabled,
            "legacy path-sensitive source_identity must produce drifting ids (sanity check)"
        );

        // 不同 source_origin（standalone vs plugin:foo）→ 不同 id
        assert_ne!(
            active_id,
            inventory_item_id(AgentTarget::Claude, "user", "plugin:foo", "tool")
        );

        // 不同 native_id → 不同 id
        assert_ne!(
            active_id,
            inventory_item_id(AgentTarget::Claude, "user", "standalone", "other")
        );

        // 不同 target → 不同 id
        assert_ne!(
            active_id,
            inventory_item_id(AgentTarget::Codex, "user", "standalone", "tool")
        );

        // 不同 scope_id → 不同 id
        assert_ne!(
            active_id,
            inventory_item_id(AgentTarget::Claude, "project:p1", "standalone", "tool")
        );

        // 相同输入必须稳定
        assert_eq!(
            active_id,
            inventory_item_id(AgentTarget::Claude, "user", "standalone", "tool")
        );
    }

    fn sample_target(target: AgentTarget, version: &str) -> PortableInventoryTargetDto {
        PortableInventoryTargetDto {
            target,
            installed: true,
            version: Some(version.to_string()),
            executable: Some(format!("/bin/{}", target.as_str())),
            config_root: format!("/cfg/{}", target.as_str()),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec!["L2-PORTABLE-INVENTORY-001".into()],
        }
    }

    fn sample_item(
        target: AgentTarget,
        native_id: &str,
        source_path: &str,
        content_hash: &str,
        actual_enabled: Option<bool>,
        project_opted_in: bool,
    ) -> PortableInventoryItemDto {
        let source_identity = source_path;
        PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(target, "user", source_identity, native_id),
            target,
            loaded_by: target,
            owned_by: PortableAssetOwner::from_target(target),
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Skill,
            native_id: native_id.to_string(),
            display_name: native_id.to_string(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: crate::agent_hub::models::ScopeKind::User,
            project_id: None,
            project_opted_in,
            source_path: Some(source_path.to_string()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled,
            content_hash: Some(content_hash.to_string()),
            tree_hash: Some(format!("tree-{content_hash}")),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: true,
                can_disable: true,
                can_uninstall: true,
                can_adopt: true,
                can_install_to_source_target: false,
                can_migrate_to_store: false,
                can_attach: false,
                can_detach: false,
                can_destroy_store: false,

                reason_code: None,
                evidence_ids: vec![],
            },
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        }
    }

    #[test]
    fn inventory_snapshot_hash_is_order_independent_and_fact_sensitive() {
        let t1 = sample_target(AgentTarget::Claude, "1.0.0");
        let t2 = sample_target(AgentTarget::Codex, "2.0.0");
        let i1 = sample_item(
            AgentTarget::Claude,
            "skill-a",
            "/skills/a",
            "hash-a",
            Some(true),
            true,
        );
        let i2 = sample_item(
            AgentTarget::Codex,
            "skill-b",
            "/skills/b",
            "hash-b",
            Some(false),
            false,
        );

        let h1 = inventory_snapshot_hash(&[t1.clone(), t2.clone()], &[i1.clone(), i2.clone()])
            .expect("hash");
        let h2 = inventory_snapshot_hash(&[t2.clone(), t1.clone()], &[i2.clone(), i1.clone()])
            .expect("hash");
        assert_eq!(h1, h2, "insertion order must not affect snapshot hash");

        let mut enabled_changed = i1.clone();
        enabled_changed.actual_enabled = Some(false);
        let h_enabled =
            inventory_snapshot_hash(&[t1.clone(), t2.clone()], &[enabled_changed, i2.clone()])
                .expect("hash");
        assert_ne!(h1, h_enabled, "actualEnabled change must rehash");

        let mut hash_changed = i1.clone();
        hash_changed.content_hash = Some("hash-a-mutated".into());
        let h_source =
            inventory_snapshot_hash(&[t1.clone(), t2.clone()], &[hash_changed, i2.clone()])
                .expect("hash");
        assert_ne!(h1, h_source, "source hash change must rehash");

        let mut cli_changed = t1.clone();
        cli_changed.version = Some("1.0.1".into());
        let h_cli = inventory_snapshot_hash(&[cli_changed, t2.clone()], &[i1.clone(), i2.clone()])
            .expect("hash");
        assert_ne!(h1, h_cli, "CLI fingerprint change must rehash");

        let mut opt_in_changed = i1.clone();
        opt_in_changed.project_opted_in = false;
        let h_opt =
            inventory_snapshot_hash(&[t1.clone(), t2.clone()], &[opt_in_changed, i2.clone()])
                .expect("hash");
        assert_ne!(h1, h_opt, "project opt-in change must rehash");

        let mut ownership_changed = i1.clone();
        ownership_changed.management_state = PortableInventoryManagementState::HubManaged;
        ownership_changed.canonical_asset_id = Some("asset-1".into());
        let h_own = inventory_snapshot_hash(&[t1, t2], &[ownership_changed, i2]).expect("hash");
        assert_ne!(h1, h_own, "ownership/management change must rehash");
    }

    #[test]
    fn portable_asset_kind_rejects_instruction_agent_and_hook() {
        assert!(PortableAssetKind::try_from_asset_kind(
            crate::agent_hub::models::AssetKind::Instruction
        )
        .is_err());
        assert!(
            PortableAssetKind::try_from_asset_kind(crate::agent_hub::models::AssetKind::Agent)
                .is_err()
        );
        assert!(
            PortableAssetKind::try_from_asset_kind(crate::agent_hub::models::AssetKind::Hook)
                .is_err()
        );
        assert_eq!(
            PortableAssetKind::try_from_asset_kind(crate::agent_hub::models::AssetKind::Skill)
                .unwrap(),
            PortableAssetKind::Skill
        );
    }

    #[test]
    fn mcp_credential_fact_exposes_present_and_hash_only() {
        let json = serde_json::to_value(PortableMcpCredentialFactDto {
            present: true,
            hash: Some("abc".into()),
        })
        .unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("present"));
        assert!(obj.contains_key("hash"));
        assert!(!obj.contains_key("secret"));
        assert!(!obj.contains_key("value"));
        assert!(!obj.contains_key("token"));
    }

    fn base_discovered() -> PortableInventoryItemDto {
        sample_item(
            AgentTarget::Claude,
            "tool",
            "/x/a",
            "content-1",
            Some(true),
            true,
        )
    }

    #[test]
    fn reconcile_marks_unmanaged_when_no_canonical_match() {
        let discovered = base_discovered();
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &[],
        )
        .expect("reconcile");
        assert_eq!(snap.items.len(), 1);
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::Unmanaged
        );
        assert!(snap.items[0].canonical_asset_id.is_none());
        assert!(snap.items[0].desired_presence.is_none());
    }

    #[test]
    fn reconcile_marks_hub_managed_when_ownership_and_hash_match() {
        let discovered = base_discovered();
        let facts = [PortableCanonicalFact {
            asset_id: "asset-1".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::Synced),
            rendered_hash: Some("content-1".into()),
            observed_external_hash: Some("content-1".into()),
            last_projected_revision_id: Some("rev-1".into()),
            native_path: Some("/x/a".into()),
            unsupported: false,
            external_collision: false,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::HubManaged
        );
        assert_eq!(snap.items[0].canonical_asset_id.as_deref(), Some("asset-1"));
        assert_eq!(
            snap.items[0].desired_presence,
            Some(DesiredPresence::Present)
        );
        assert_eq!(snap.items[0].desired_enabled, Some(true));
        assert_eq!(
            snap.items[0].canonical_revision_id.as_deref(),
            Some("rev-1")
        );
    }

    #[test]
    fn reconcile_marks_drifted_when_hub_owned_hash_diverges() {
        let discovered = base_discovered();
        let facts = [PortableCanonicalFact {
            asset_id: "asset-1".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::Drift),
            rendered_hash: Some("content-applied".into()),
            observed_external_hash: Some("content-1".into()),
            last_projected_revision_id: Some("rev-1".into()),
            native_path: Some("/x/a".into()),
            unsupported: false,
            external_collision: false,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::Drifted
        );
    }

    #[test]
    fn reconcile_blocked_support_gate_with_stale_rendered_hash_is_hub_managed_not_drifted() {
        // support 门禁 Blocked（如 min_tested_version_missing）可能留下与当前
        // scan content_hash 不一致的 rendered_hash；不得因此把整页 MCP 标成 drifted。
        let mut discovered = base_discovered();
        discovered.kind = PortableAssetKind::Mcp;
        discovered.content_hash = Some("leaf-hash-current".into());
        discovered.tree_hash = None;
        let facts = [PortableCanonicalFact {
            asset_id: "asset-mcp".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Mcp,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::Blocked),
            rendered_hash: Some("stale-shared-hash".into()),
            observed_external_hash: Some("stale-shared-hash".into()),
            last_projected_revision_id: Some("rev-1".into()),
            native_path: Some("/Users/x/.claude.json".into()),
            unsupported: false,
            external_collision: false,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::HubManaged,
            "Blocked support-gate ledger must not surface as content drift"
        );
        assert_eq!(
            snap.items[0].materialization_status.as_deref(),
            Some("blocked")
        );
    }

    #[test]
    fn reconcile_marks_external_collision_for_incompatible_external_source() {
        let discovered = base_discovered();
        let facts = [PortableCanonicalFact {
            asset_id: "asset-1".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::ExternalCollision),
            rendered_hash: Some("content-applied".into()),
            observed_external_hash: Some("other".into()),
            last_projected_revision_id: Some("rev-1".into()),
            native_path: Some("/x/a".into()),
            unsupported: false,
            external_collision: true,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::ExternalCollision
        );
    }

    #[test]
    fn reconcile_marks_unsupported_when_adapter_lacks_semantics() {
        let mut discovered = base_discovered();
        discovered.capabilities.reason_code = Some("portable_semantics_unsupported".into());
        let facts = [PortableCanonicalFact {
            asset_id: "asset-1".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::Unsupported),
            rendered_hash: Some("content-1".into()),
            observed_external_hash: Some("content-1".into()),
            last_projected_revision_id: Some("rev-1".into()),
            native_path: Some("/x/a".into()),
            unsupported: true,
            external_collision: false,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![discovered],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(
            snap.items[0].management_state,
            PortableInventoryManagementState::Unsupported
        );
    }

    #[test]
    fn reconcile_does_not_merge_standalone_and_plugin_component_by_name() {
        let standalone = sample_item(
            AgentTarget::Claude,
            "shared-name",
            "/skills/shared-name",
            "hash-standalone",
            Some(true),
            true,
        );
        let mut plugin_comp = sample_item(
            AgentTarget::Claude,
            "shared-name",
            "/plugins/p1/skills/shared-name",
            "hash-plugin",
            Some(true),
            true,
        );
        plugin_comp.source_origin = PortableInventorySourceOrigin::PluginComponent;
        plugin_comp.parent_plugin_inventory_item_id = Some("plugin-inv-1".into());
        plugin_comp.inventory_item_id = inventory_item_id(
            AgentTarget::Claude,
            "user",
            "/plugins/p1/skills/shared-name",
            "shared-name",
        );

        let facts = [
            PortableCanonicalFact {
                asset_id: "asset-standalone".into(),
                scope_id: "user".into(),
                kind: PortableAssetKind::Skill,
                origin_namespace: "standalone".into(),
                logical_key: "shared-name".into(),
                target: AgentTarget::Claude,
                hub_owned: true,
                desired_presence: Some(DesiredPresence::Present),
                desired_enabled: Some(true),
                materialization_status: Some(MaterializationStatus::Synced),
                rendered_hash: Some("hash-standalone".into()),
                observed_external_hash: Some("hash-standalone".into()),
                last_projected_revision_id: Some("rev-s".into()),
                native_path: Some("/skills/shared-name".into()),
                unsupported: false,
                external_collision: false,
            },
            PortableCanonicalFact {
                asset_id: "asset-plugin".into(),
                scope_id: "user".into(),
                kind: PortableAssetKind::Skill,
                origin_namespace: "plugin:p1".into(),
                logical_key: "shared-name".into(),
                target: AgentTarget::Claude,
                hub_owned: true,
                desired_presence: Some(DesiredPresence::Present),
                desired_enabled: Some(true),
                materialization_status: Some(MaterializationStatus::Synced),
                rendered_hash: Some("hash-plugin".into()),
                observed_external_hash: Some("hash-plugin".into()),
                last_projected_revision_id: Some("rev-p".into()),
                native_path: Some("/plugins/p1/skills/shared-name".into()),
                unsupported: false,
                external_collision: false,
            },
        ];

        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![standalone, plugin_comp],
            &facts,
        )
        .expect("reconcile");
        assert_eq!(snap.items.len(), 2);
        let standalone_item = snap
            .items
            .iter()
            .find(|i| i.source_origin == PortableInventorySourceOrigin::Standalone)
            .unwrap();
        let plugin_item = snap
            .items
            .iter()
            .find(|i| i.source_origin == PortableInventorySourceOrigin::PluginComponent)
            .unwrap();
        assert_eq!(
            standalone_item.canonical_asset_id.as_deref(),
            Some("asset-standalone")
        );
        assert_eq!(
            plugin_item.canonical_asset_id.as_deref(),
            Some("asset-plugin")
        );
        assert_ne!(
            standalone_item.inventory_item_id,
            plugin_item.inventory_item_id
        );
    }

    #[test]
    fn reconcile_absence_does_not_invent_observed_file_from_desired_presence() {
        // Canonical desires presence, but discovered list has no file → snapshot only has
        // discovered items; desired alone never synthesizes an observed inventory row.
        let facts = [PortableCanonicalFact {
            asset_id: "asset-missing".into(),
            scope_id: "user".into(),
            kind: PortableAssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "ghost".into(),
            target: AgentTarget::Claude,
            hub_owned: true,
            desired_presence: Some(DesiredPresence::Present),
            desired_enabled: Some(true),
            materialization_status: Some(MaterializationStatus::Detached),
            rendered_hash: Some("old".into()),
            observed_external_hash: None,
            last_projected_revision_id: Some("rev-old".into()),
            native_path: Some("/missing".into()),
            unsupported: false,
            external_collision: false,
        }];
        let snap = reconcile_portable_inventory_with_facts(
            vec![sample_target(AgentTarget::Claude, "1.0.0")],
            vec![],
            &facts,
        )
        .expect("reconcile");
        assert!(
            snap.items.is_empty(),
            "desired presence must not invent observed inventory items"
        );
    }
}
