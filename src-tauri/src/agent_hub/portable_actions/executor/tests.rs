//! portable_actions/executor/tests — executor 单元测试
//!
//! Business Logic（为什么需要这个模块）:
//!     apply 链路的 claim 幂等、capability 阻断、hash 写前复验、rescan 对账、
//!     跨 Agent 聚合确认等合同必须由注入 FakeProcessRunner 与快照 seam 的单测锁定，
//!     防止 executor 拆分或后续迭代造成回归。
//!
//! Code Logic（这个模块做什么）:
//!     覆盖 confirm current version、escape link 修复、enable/disable/uninstall argv、
//!     partial results、outcomeUnknown 等场景；含 `test_deps_with_runner` 测试辅助。
//!     仅在 cfg(test) 下编译（mod.rs 中 `#[cfg(test)] mod tests;`）。

use super::*;
use crate::agent_hub::models::{
    AgentTarget, AssetKind, AssetPolicy, DesiredPresence, MaterializationStatus, NewLogicalAsset,
    NewMaterialization, NewScopeNode, NewTargetBinding, ScopeKind,
};
use crate::agent_hub::packages::activator::FakeProcessRunner;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionKind, PortableAssetActionPlanDto, PortableAssetConflictPolicy,
    PreviewPortableAssetActionRequest,
};
use crate::agent_hub::portable_actions::planner::preview_portable_asset_action_with_inventory;
use crate::agent_hub::portable_inventory::{
    hash_plugin_root, inventory_item_id, inventory_snapshot_hash, PortableAssetKind,
    PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryMutationCapability,
    PortableInventoryScanCapability, PortableInventorySourceOrigin, PortableInventoryTargetDto,
    PortableOriginKind, PortableStoreFactDto,
};
use crate::agent_hub::targets::portable::hash_skill_directory;
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;
use std::sync::Arc;

/// 测试辅助：空 Fake runner 依赖。
pub fn test_deps_with_runner(
    repo: AgentHubRepo,
    runner: Arc<crate::agent_hub::packages::activator::FakeProcessRunner>,
) -> PortableActionExecutorDeps {
    PortableActionExecutorDeps {
        repo,
        runner,
        env: None,
        pre_inventory: None,
        claude_config_dir: None,
        data_dir: None,
        rescan_override: None,
    }
}

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

fn sample_target(target: AgentTarget) -> PortableInventoryTargetDto {
    PortableInventoryTargetDto {
        target,
        installed: true,
        version: Some("1.0.0".into()),
        executable: Some(format!("/bin/{}", target.as_str())),
        config_root: format!("/cfg/{}", target.as_str()),
        scan_capability: PortableInventoryScanCapability::Supported,
        mutation_capability: PortableInventoryMutationCapability::Supported,
        reason_code: None,
        evidence_ids: vec![],
    }
}

fn sample_item(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    path: &str,
    enabled: Option<bool>,
) -> PortableInventoryItemDto {
    // source_identity 路径无关：与生产 scanner 语义一致（standalone 资产用 "standalone"），
    // 同一逻辑资产在 active/disabled 路径下产出相同 id。path 仅落到 source_path 字段。
    PortableInventoryItemDto {
        inventory_item_id: inventory_item_id(target, "user", "standalone", native_id),
        target,
        loaded_by: target,
        owned_by: PortableAssetOwner::from_target(target),
        origin_kind: PortableOriginKind::Native,
        native_output_candidate: true,
        kind,
        native_id: native_id.into(),
        display_name: native_id.into(),
        description: None,
        version: None,
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        source_path: Some(path.into()),
        source_origin: PortableInventorySourceOrigin::Standalone,
        parent_plugin_inventory_item_id: None,
        actual_enabled: enabled,
        content_hash: Some("content-hash".into()),
        tree_hash: Some("tree-hash".into()),
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
            can_install_to_source_target: true,
            can_migrate_to_store: false,
            can_attach: false,
            can_detach: false,
            can_destroy_store: false,
            can_confirm_current_version: false,
            can_materialize_escape_link: false,
            reason_code: None,
            evidence_ids: vec![],
        },
        warnings: vec![],
        mcp_credential: None,
        store: Default::default(),
    }
}

fn snapshot_from(
    targets: Vec<PortableInventoryTargetDto>,
    items: Vec<PortableInventoryItemDto>,
) -> PortableInventorySnapshotDto {
    let hash = inventory_snapshot_hash(&targets, &items).expect("hash");
    PortableInventorySnapshotDto {
        inventory_snapshot_hash: hash,
        refreshed_at: Utc::now().to_rfc3339(),
        stale: false,
        targets,
        items,
    }
}

async fn preview_action(
    repo: &AgentHubRepo,
    snap: &PortableInventorySnapshotDto,
    ids: Vec<String>,
    action: PortableAssetActionKind,
    keep_data: bool,
) -> PortableAssetActionPlanDto {
    preview_portable_asset_action_with_inventory(
        repo,
        PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: snap.inventory_snapshot_hash.clone(),
            inventory_query: Default::default(),
            inventory_item_ids: ids,
            action,
            keep_data,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        },
        snap,
        "owner-fp",
    )
    .await
    .expect("preview")
}

/// Business Logic: 确认当前版本只重记哈希，不 spawn CLI、不改磁盘。
#[tokio::test]
async fn confirm_current_version_updates_materialization_hashes() {
    let repo = test_repo().await;
    repo.insert_scope(NewScopeNode {
        id: Some("user".into()),
        kind: ScopeKind::User,
        hub_project_id: None,
        relative_path: None,
    })
    .await
    .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: "user".into(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            display_name: "tool".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .expect("asset");
    let binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    repo.upsert_materialization(NewMaterialization {
        asset_id: asset.id.clone(),
        target: AgentTarget::Claude,
        target_binding_id: binding.id.clone(),
        native_path: Some("/skills/tool".into()),
        last_projected_revision_id: None,
        rendered_hash: Some("old-hash".into()),
        observed_external_hash: Some("new-hash".into()),
        status: MaterializationStatus::Drift,
        last_error: None,
    })
    .await
    .expect("materialization");

    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "tool",
        "/skills/tool",
        Some(true),
    );
    item.canonical_asset_id = Some(asset.id.clone());
    item.management_state = PortableInventoryManagementState::Drifted;
    item.content_hash = Some("new-hash".into());
    item.tree_hash = Some("tree-new".into());
    item.capabilities.can_confirm_current_version = true;
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::ConfirmCurrentVersion,
        false,
    )
    .await;
    assert!(
        plan.blocking_reasons.is_empty(),
        "unexpected blocking: {:?}",
        plan.blocking_reasons
    );

    let mut post_item = item.clone();
    post_item.management_state = PortableInventoryManagementState::HubManaged;
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let runner = Arc::new(FakeProcessRunner::new());
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-confirm-current".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    assert!(runner.calls().is_empty(), "ledger-only must not spawn CLI");
    let mat = repo
        .get_materialization_by_binding(&binding.id)
        .await
        .expect("read mat")
        .expect("mat exists");
    assert_eq!(mat.rendered_hash.as_deref(), Some("new-hash"));
    assert_eq!(mat.observed_external_hash.as_deref(), Some("new-hash"));
    assert_eq!(mat.status, MaterializationStatus::Synced);
}

/// Business Logic: 逃逸软链确认当前版本只改账本，不跟随目标树、不改磁盘。
#[tokio::test]
async fn confirm_current_version_accepts_escaped_skill_without_following() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("agents/grilling");
    std::fs::create_dir_all(real.join("nested")).unwrap();
    std::fs::write(real.join("SKILL.md"), "---\nname: grilling\n---\nbody\n").unwrap();
    std::fs::write(real.join("nested/data.txt"), "payload").unwrap();
    let link = dir.path().join("claude/skills/grilling");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, &link).unwrap();
    let identity = crate::agent_hub::object_store::sha256_hex(
        format!(
            "store_symlink_escape\0{}",
            std::fs::read_link(&link)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        )
        .as_bytes(),
    );
    let followed = hash_skill_directory(&real).unwrap();
    assert_ne!(identity, followed.0);

    let repo = test_repo().await;
    repo.insert_scope(NewScopeNode {
        id: Some("user".into()),
        kind: ScopeKind::User,
        hub_project_id: None,
        relative_path: None,
    })
    .await
    .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: "user".into(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "grilling".into(),
            display_name: "grilling".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .expect("asset");
    let binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    repo.upsert_materialization(NewMaterialization {
        asset_id: asset.id.clone(),
        target: AgentTarget::Claude,
        target_binding_id: binding.id.clone(),
        native_path: Some(link.to_string_lossy().into()),
        last_projected_revision_id: None,
        rendered_hash: Some("old-skill-md".into()),
        observed_external_hash: Some("old-skill-md".into()),
        status: MaterializationStatus::Drift,
        last_error: None,
    })
    .await
    .expect("materialization");

    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "grilling",
        link.to_str().unwrap(),
        Some(true),
    );
    item.canonical_asset_id = Some(asset.id.clone());
    item.management_state = PortableInventoryManagementState::Drifted;
    item.content_hash = Some(identity.clone());
    item.tree_hash = None;
    item.warnings = vec!["store_symlink_escape".into(), "source_blocked".into()];
    item.capabilities.can_confirm_current_version = true;
    item.capabilities.reason_code = Some("source_blocked".into());
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::ConfirmCurrentVersion,
        false,
    )
    .await;
    assert_eq!(
        plan.changes[0].expected_source_hash.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(plan.changes[0].expected_tree_hash, None);

    let mut post_item = item.clone();
    post_item.management_state = PortableInventoryManagementState::HubManaged;
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let runner = Arc::new(FakeProcessRunner::new());
    let env = crate::agent_hub::targets::TargetEnvironment {
        home: dir.path().to_path_buf(),
        vars: Default::default(),
        path_entries: vec![],
    };
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: Some(env),
        pre_inventory: Some(snap),
        claude_config_dir: Some(dir.path().join("claude")),
        data_dir: Some(dir.path().join("data")),
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-confirm-escape".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "escaped confirm must not fail source-hash recheck: {:?}",
        result.items[0].error_code
    );
    assert_ne!(
        result.items[0].error_code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED")
    );
    assert!(runner.calls().is_empty(), "ledger-only must not spawn CLI");
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_link(&link).unwrap(), real);
    let mat = repo
        .get_materialization_by_binding(&binding.id)
        .await
        .expect("read mat")
        .expect("mat exists");
    assert_eq!(mat.rendered_hash.as_deref(), Some(identity.as_str()));
    assert_eq!(
        mat.observed_external_hash.as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(mat.status, MaterializationStatus::Synced);
}

/// Business Logic: 同一仓库真树被多个 Agent 软链观测时，确认一次当前版本即让
/// 同一 canonical asset 下其它 target 观测到相同内容 hash 的 Drifted 项一并确认。
#[tokio::test]
async fn confirm_current_version_aggregates_same_asset_other_targets() {
    let repo = test_repo().await;
    repo.insert_scope(NewScopeNode {
        id: Some("user".into()),
        kind: ScopeKind::User,
        hub_project_id: None,
        relative_path: None,
    })
    .await
    .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: "user".into(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            display_name: "tool".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .expect("asset");
    let claude_binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    let codex_binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Codex,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    for binding in [&claude_binding, &codex_binding] {
        repo.upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: binding.target,
            target_binding_id: binding.id.clone(),
            native_path: Some(format!("/{}/skills/tool", binding.target.as_str())),
            last_projected_revision_id: None,
            rendered_hash: Some("old-hash".into()),
            observed_external_hash: Some("old-hash".into()),
            status: MaterializationStatus::Drift,
            last_error: None,
        })
        .await
        .expect("materialization");
    }

    let mut claude_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "tool",
        "/claude/skills/tool",
        Some(true),
    );
    claude_item.canonical_asset_id = Some(asset.id.clone());
    claude_item.management_state = PortableInventoryManagementState::Drifted;
    claude_item.content_hash = Some("new-hash".into());
    claude_item.tree_hash = None;
    claude_item.capabilities.can_confirm_current_version = true;
    // Codex 软链同一仓库真树：同 canonical asset、同观测内容 hash、同样 Drifted。
    let mut codex_item = claude_item.clone();
    codex_item.target = AgentTarget::Codex;
    codex_item.loaded_by = AgentTarget::Codex;
    codex_item.owned_by = PortableAssetOwner::from_target(AgentTarget::Codex);
    codex_item.source_path = Some("/codex/skills/tool".into());
    codex_item.inventory_item_id =
        inventory_item_id(AgentTarget::Codex, "user", "standalone", "tool");
    let snap = snapshot_from(
        vec![
            sample_target(AgentTarget::Claude),
            sample_target(AgentTarget::Codex),
        ],
        vec![claude_item.clone(), codex_item],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![claude_item.inventory_item_id.clone()],
        PortableAssetActionKind::ConfirmCurrentVersion,
        false,
    )
    .await;
    assert!(
        plan.blocking_reasons.is_empty(),
        "unexpected blocking: {:?}",
        plan.blocking_reasons
    );

    let mut post_item = claude_item.clone();
    post_item.management_state = PortableInventoryManagementState::HubManaged;
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let runner = Arc::new(FakeProcessRunner::new());
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: None,
        // 聚合重扫走 resolve_force_inventory seam：注入 pre_inventory 即返回全量观测
        //（Claude + Codex 均 Drifted、hash 一致），无需真实 inspect。
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-confirm-aggregate".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    assert_eq!(
        result.items[0].message.as_deref(),
        Some("current version recorded (2 agent observations)"),
        "aggregate count must surface in message: {:?}",
        result.items[0].message
    );
    assert!(runner.calls().is_empty(), "ledger-only must not spawn CLI");
    for binding in [&claude_binding, &codex_binding] {
        let mat = repo
            .get_materialization_by_binding(&binding.id)
            .await
            .expect("read mat")
            .expect("mat exists");
        assert_eq!(
            mat.rendered_hash.as_deref(),
            Some("new-hash"),
            "target {} materialization must be confirmed",
            binding.target.as_str()
        );
        assert_eq!(mat.observed_external_hash.as_deref(), Some("new-hash"));
        assert_eq!(mat.status, MaterializationStatus::Synced);
    }
}

/// Business Logic: 其它 target 观测的内容 hash 与本次确认不一致（内容分叉）时，
/// 不得被聚合确认，保持原 Drifted materialization。
#[tokio::test]
async fn confirm_current_version_does_not_aggregate_different_content() {
    let repo = test_repo().await;
    repo.insert_scope(NewScopeNode {
        id: Some("user".into()),
        kind: ScopeKind::User,
        hub_project_id: None,
        relative_path: None,
    })
    .await
    .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: "user".into(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "tool".into(),
            display_name: "tool".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .expect("asset");
    let claude_binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    let codex_binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Codex,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    repo.upsert_materialization(NewMaterialization {
        asset_id: asset.id.clone(),
        target: AgentTarget::Claude,
        target_binding_id: claude_binding.id.clone(),
        native_path: Some("/claude/skills/tool".into()),
        last_projected_revision_id: None,
        rendered_hash: Some("old-hash".into()),
        observed_external_hash: Some("old-hash".into()),
        status: MaterializationStatus::Drift,
        last_error: None,
    })
    .await
    .expect("materialization");
    repo.upsert_materialization(NewMaterialization {
        asset_id: asset.id.clone(),
        target: AgentTarget::Codex,
        target_binding_id: codex_binding.id.clone(),
        native_path: Some("/codex/skills/tool".into()),
        last_projected_revision_id: None,
        rendered_hash: Some("codex-old-hash".into()),
        observed_external_hash: Some("codex-old-hash".into()),
        status: MaterializationStatus::Drift,
        last_error: None,
    })
    .await
    .expect("materialization");

    let mut claude_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "tool",
        "/claude/skills/tool",
        Some(true),
    );
    claude_item.canonical_asset_id = Some(asset.id.clone());
    claude_item.management_state = PortableInventoryManagementState::Drifted;
    claude_item.content_hash = Some("new-hash".into());
    claude_item.tree_hash = None;
    claude_item.capabilities.can_confirm_current_version = true;
    // Codex 观测的是分叉后的不同内容，不满足聚合条件。
    let mut codex_item = claude_item.clone();
    codex_item.target = AgentTarget::Codex;
    codex_item.loaded_by = AgentTarget::Codex;
    codex_item.owned_by = PortableAssetOwner::from_target(AgentTarget::Codex);
    codex_item.source_path = Some("/codex/skills/tool".into());
    codex_item.content_hash = Some("codex-hash".into());
    codex_item.inventory_item_id =
        inventory_item_id(AgentTarget::Codex, "user", "standalone", "tool");
    let snap = snapshot_from(
        vec![
            sample_target(AgentTarget::Claude),
            sample_target(AgentTarget::Codex),
        ],
        vec![claude_item.clone(), codex_item],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![claude_item.inventory_item_id.clone()],
        PortableAssetActionKind::ConfirmCurrentVersion,
        false,
    )
    .await;
    assert!(
        plan.blocking_reasons.is_empty(),
        "unexpected blocking: {:?}",
        plan.blocking_reasons
    );

    let mut post_item = claude_item.clone();
    post_item.management_state = PortableInventoryManagementState::HubManaged;
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let runner = Arc::new(FakeProcessRunner::new());
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-confirm-no-aggregate".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    assert_eq!(
        result.items[0].message.as_deref(),
        Some("current version recorded"),
        "no aggregation must keep the plain message: {:?}",
        result.items[0].message
    );
    assert!(runner.calls().is_empty(), "ledger-only must not spawn CLI");
    let claude_mat = repo
        .get_materialization_by_binding(&claude_binding.id)
        .await
        .expect("read mat")
        .expect("mat exists");
    assert_eq!(claude_mat.rendered_hash.as_deref(), Some("new-hash"));
    assert_eq!(claude_mat.status, MaterializationStatus::Synced);
    let codex_mat = repo
        .get_materialization_by_binding(&codex_binding.id)
        .await
        .expect("read mat")
        .expect("mat exists");
    assert_eq!(
        codex_mat.rendered_hash.as_deref(),
        Some("codex-old-hash"),
        "divergent content must not be aggregated"
    );
    assert_eq!(
        codex_mat.observed_external_hash.as_deref(),
        Some("codex-old-hash")
    );
    assert_eq!(codex_mat.status, MaterializationStatus::Drift);
}

/// Business Logic: 逃逸软链恢复进仓库并挂正规软链，不删源树、不 spawn CLI。
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn materialize_escape_link_restores_into_store_without_cli() {
    let _guard = crate::agent_hub::targets::portable::DATA_DIR_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let repo = test_repo().await;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);
    let real = dir.path().join("agents/skills/grilling");
    std::fs::create_dir_all(real.join("nested")).unwrap();
    std::fs::write(real.join("SKILL.md"), "---\nname: grilling\n---\nbody\n").unwrap();
    std::fs::write(real.join("nested/data.txt"), "payload").unwrap();
    let link = dir.path().join("claude/skills/grilling");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&real, &link).unwrap();
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "grilling",
        link.to_str().unwrap(),
        Some(true),
    );
    item.tree_hash = None;
    item.warnings = vec!["store_symlink_escape".into(), "source_blocked".into()];
    item.capabilities.can_enable = false;
    item.capabilities.can_disable = false;
    item.capabilities.can_uninstall = false;
    item.capabilities.can_materialize_escape_link = true;
    item.capabilities.reason_code = Some("source_blocked".into());
    item.management_state = PortableInventoryManagementState::Unsupported;
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::MaterializeEscapeLink,
        false,
    )
    .await;
    assert!(
        plan.blocking_reasons.is_empty(),
        "unexpected blocking: {:?}",
        plan.blocking_reasons
    );

    let mut post_item = item.clone();
    post_item.warnings.clear();
    post_item.capabilities.can_materialize_escape_link = false;
    post_item.capabilities.reason_code = None;
    post_item.management_state = PortableInventoryManagementState::Unmanaged;
    post_item.store = PortableStoreFactDto {
        store_id: Some("skill:grilling".into()),
        store_attached: true,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let runner = Arc::new(FakeProcessRunner::new());
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: Some(data.clone()),
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-materialize-escape".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "materialize failed: {:?}",
        result.items[0].error_code
    );
    assert!(runner.calls().is_empty(), "repair must not spawn CLI");
    assert!(
        link.symlink_metadata().unwrap().file_type().is_symlink(),
        "native must become a store symlink"
    );
    let store_tree = crate::agent_hub::portable_store::store_skill_dir(
        &crate::agent_hub::portable_store::portable_store_root(&data),
        "grilling",
    );
    assert!(store_tree.join("SKILL.md").is_file());
    assert!(store_tree.join("nested/data.txt").is_file());
    assert!(
        real.join("SKILL.md").is_file(),
        "original target must remain"
    );
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(&store_tree).unwrap()
    );
    std::env::remove_var("CC_PARTNER_DATA_DIR");
}

/// Business Logic: Claude Plugin enable 必须带 --scope user argv。
#[tokio::test]
async fn claude_plugin_enable_locks_scope_argv() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_ok("ok");
    let item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "review@local",
        "/plugins/review",
        Some(false),
    );
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Enable,
        false,
    )
    .await;

    let mut post_item = item.clone();
    post_item.actual_enabled = Some(true);
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-plugin-1".into(),
        },
    )
    .await
    .expect("apply");

    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    let calls = runner.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args[0], "plugin");
    assert_eq!(calls[0].args[1], "enable");
    let scope_idx = calls[0].args.iter().position(|a| a == "--scope").unwrap();
    assert_eq!(calls[0].args[scope_idx + 1], "user");
}

/// Business Logic: 真实 plugin 根 + inventory hash 域 recheck 不得误报 SOURCE_HASH_CHANGED。
/// Code Logic: temp plugin root + hash_plugin_root → preview → apply；CLI 必须执行。
#[tokio::test]
async fn plugin_real_root_hash_domain_passes_recheck() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_root = dir.path().join("plugins").join("demo-plugin");
    std::fs::create_dir_all(plugin_root.join(".claude-plugin")).unwrap();
    std::fs::write(
        plugin_root.join(".claude-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::create_dir_all(plugin_root.join("skills")).unwrap();
    let (content_hash, tree_hash) = hash_plugin_root(&plugin_root).unwrap();
    // 生产 inventory 对有 manifest 的 plugin 用 material hash，不等于 path-string sha
    let path_string_hash =
        crate::agent_hub::object_store::sha256_hex(plugin_root.display().to_string().as_bytes());
    assert_ne!(content_hash, path_string_hash);

    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_ok("enabled");
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "demo-plugin@local",
        plugin_root.to_str().unwrap(),
        Some(false),
    );
    item.content_hash = Some(content_hash);
    item.tree_hash = Some(tree_hash);
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Enable,
        false,
    )
    .await;
    assert_eq!(
        plan.changes[0].expected_source_hash.as_deref(),
        item.content_hash.as_deref()
    );

    let mut post_item = item.clone();
    post_item.actual_enabled = Some(true);
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-plugin-hash-domain".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "unchanged real plugin root must not fail source-hash recheck: {:?}",
        result.items[0].error_code
    );
    assert_ne!(
        result.items[0].error_code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED")
    );
    assert_eq!(runner.calls().len(), 1);
}

/// Business Logic: Skill disable 必须 move 到 disabled 且零 spawn。
#[tokio::test]
async fn skill_disable_moves_to_disabled_with_backup_root() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let data = dir.path().join("data");
    std::fs::create_dir_all(claude.join("skills/my-skill")).unwrap();
    std::fs::write(claude.join("skills/my-skill/SKILL.md"), "# skill\n").unwrap();
    let skill_path = claude.join("skills/my-skill");
    let (hash, _, _, _) = hash_skill_directory(&skill_path).unwrap();

    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "my-skill",
        skill_path.to_str().unwrap(),
        Some(true),
    );
    item.content_hash = Some(hash);
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;

    let mut post_item = item.clone();
    post_item.actual_enabled = Some(false);
    post_item.source_path = Some(
        data.join("claude-assets/disabled/skills/my-skill")
            .to_string_lossy()
            .into(),
    );
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: Some(claude.clone()),
        data_dir: Some(data.clone()),
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-skill-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    assert!(!skill_path.exists());
    assert!(data.join("claude-assets/disabled/skills/my-skill").exists());
    assert!(runner.calls().is_empty());
}

/// Business Logic: inventory_item_id 已路径无关——disable 把 skill 从 active 路径物理移动到
/// disabled 路径后，pre 和 post 拥有相同的 inventory_item_id（因为 source_identity 是
/// origin_namespace "standalone" 而非绝对路径）。所以精确匹配即可命中 post 投影，无需 fallback。
/// Code Logic: 构造 pre（active 路径）与 post（disabled 路径）两份 item，断言两者 id 相同，
/// resolve_post_item 通过精确匹配（不走 fallback）命中 post_item，actual_enabled == Some(false)。
#[test]
fn reconcile_disable_matches_by_stable_id_when_path_moves() {
    let target = AgentTarget::Claude;
    let pre_path = "/home/user/.claude/skills/hyperframes";
    let post_path = "/data/cc-partner/claude-assets/disabled/skills/hyperframes";
    let native_id = "hyperframes";
    let scope_id = "user";

    // 路径无关契约：source_identity = "standalone"（与生产 scanner 一致），
    // 不同路径产出相同 id。
    let stable_id = inventory_item_id(target, scope_id, "standalone", native_id);

    let pre_item = sample_item(
        target,
        PortableAssetKind::Skill,
        native_id,
        pre_path,
        Some(true),
    );
    assert_eq!(
        pre_item.inventory_item_id, stable_id,
        "pre item id must be the path-independent stable id"
    );

    // post item 用 disabled 路径独立构造，模拟 scanner 重新扫描得到的真实 inventory。
    let mut post_item = sample_item(
        target,
        PortableAssetKind::Skill,
        native_id,
        post_path,
        Some(false),
    );
    assert_eq!(
        post_item.inventory_item_id, stable_id,
        "post item id must equal pre id (path-independent)"
    );
    post_item.scope_id = scope_id.into();

    let post_by_id: BTreeMap<String, &PortableInventoryItemDto> =
        [(post_item.inventory_item_id.clone(), &post_item)]
            .into_iter()
            .collect();
    let post_by_logical_key: BTreeMap<(AgentTarget, String, String), &PortableInventoryItemDto> =
        [(
            (
                post_item.target,
                post_item.scope_id.clone(),
                post_item.native_id.clone(),
            ),
            &post_item,
        )]
        .into_iter()
        .collect();

    // 精确匹配命中 post（路径无关 → pre id == post id，不需要 fallback）。
    let resolved = resolve_post_item(
        &stable_id,
        Some(&pre_item),
        &post_by_id,
        &post_by_logical_key,
    )
    .expect("exact match must hit post item");
    assert_eq!(resolved.inventory_item_id, stable_id);
    assert_eq!(resolved.actual_enabled, Some(false));

    // 对照：pre 缺失且 id 不匹配 → None（不假命中）。
    let none = resolve_post_item("nonexistent-id", None, &post_by_id, &post_by_logical_key);
    assert!(none.is_none());
}

/// Business Logic: inventory_item_id 路径无关后，disable 把 skill 从 active 物理移动到 disabled
/// 路径，pre 与 post 的 id 保持相同；apply 通过精确匹配直接命中 post，报 Succeeded，
/// 不走也不需要 logical fallback。
/// Code Logic: 用 sample_item 构造 pre（active 路径）与 post（disabled 路径），断言 id 相同；
/// 通过 rescan_override 注入 post 快照验证端到端精确匹配生效。
#[tokio::test]
async fn skill_disable_with_path_move_succeeds_via_exact_id_match() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let data = dir.path().join("data");
    std::fs::create_dir_all(claude.join("skills/hyperframes")).unwrap();
    std::fs::write(claude.join("skills/hyperframes/SKILL.md"), "# skill\n").unwrap();
    let skill_path = claude.join("skills/hyperframes");
    let (hash, _, _, _) = hash_skill_directory(&skill_path).unwrap();

    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let mut pre_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "hyperframes",
        skill_path.to_str().unwrap(),
        Some(true),
    );
    pre_item.content_hash = Some(hash);
    let snap = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![pre_item.clone()],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![pre_item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;

    // 模拟 scanner rescan：disabled 路径 + actual_enabled=false。
    // 路径无关契约：post id 与 pre id 相同。
    let disabled_path = data.join("claude-assets/disabled/skills/hyperframes");
    let post_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "hyperframes",
        disabled_path.to_str().unwrap(),
        Some(false),
    );
    assert_eq!(
        post_item.inventory_item_id, pre_item.inventory_item_id,
        "path-independent id must be equal across active/disabled paths"
    );
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: Some(claude.clone()),
        data_dir: Some(data.clone()),
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-skill-exact-match".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "disable with path move must succeed via exact id match: {:?} / {:?}",
        result.items[0].error_code,
        result.items[0].message
    );
    assert_ne!(
        result.items[0].error_code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING")
    );
    assert!(!skill_path.exists());
    assert!(disabled_path.exists());
    assert!(runner.calls().is_empty());
}

/// Business Logic: MCP disable 使用 semantic patch，保留 sibling keys，DTO 无 secret。
#[tokio::test]
async fn mcp_disable_semantic_patch_preserves_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&claude).unwrap();
    let cfg = claude.join(".claude.json");
    std::fs::write(
        &cfg,
        r#"{
  // keep comment
  "mcpServers": {
    "keep-me": { "command": "uvx", "env": { "TOKEN": "secret-value" } },
    "drop-me": { "command": "npx", "env": { "KEY": "secret-key" } }
  }
}
"#,
    )
    .unwrap();

    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Mcp,
        "drop-me",
        cfg.to_str().unwrap(),
        Some(true),
    );
    // 避免整文件 hash 误伤（MCP 语义 path CAS 独立）
    item.content_hash = None;
    item.tree_hash = Some("t".into());
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;

    let mut post_item = item.clone();
    post_item.actual_enabled = Some(false);
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: Some(claude),
        data_dir: Some(data),
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-mcp-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(text.contains("keep-me"));
    assert!(!text.contains("\"drop-me\""));
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("secret-value"));
    assert!(!json.contains("secret-key"));
    assert!(runner.calls().is_empty());
}

/// Business Logic: OpenCode 仍未认证写能力时零 spawn 且 blocked。
#[tokio::test]
async fn unsupported_target_zero_spawn() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let item = sample_item(
        AgentTarget::OpenCode,
        PortableAssetKind::Skill,
        "x",
        "/skills/x",
        Some(true),
    );
    let snap = snapshot_from(
        vec![sample_target(AgentTarget::OpenCode)],
        vec![item.clone()],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap.clone()),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(snap),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-opencode-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(result.items[0].state, PortableAssetActionItemState::Blocked);
    assert!(runner.calls().is_empty());
}

/// 生产执行顺序合同：target mutation capability 必须在 adapter 前最后复验。
#[test]
fn mutation_revalidation_precedes_target_executor() {
    let src = include_str!("mod.rs");
    let gate = src
        .find("if let Some(outcome) = revalidate_target_mutation_before_write(")
        .expect("write gate call");
    let adapter = src
        .find("let outcome = match exec.execute_change(")
        .expect("target adapter call");
    assert!(gate < adapter, "mutation gate must precede adapter write");
}

/// Business Logic: source hash 变化 fail-closed，不执行 CLI。
#[tokio::test]
async fn changed_source_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let claude = dir.path().join("claude");
    std::fs::create_dir_all(claude.join("skills/s")).unwrap();
    std::fs::write(claude.join("skills/s/SKILL.md"), "v1").unwrap();
    let path = claude.join("skills/s");

    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "s",
        path.to_str().unwrap(),
        Some(true),
    );
    item.content_hash = Some("stale-hash".into());
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    std::fs::write(claude.join("skills/s/SKILL.md"), "v2-changed").unwrap();

    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap.clone()),
        claude_config_dir: Some(claude),
        data_dir: Some(dir.path().join("data")),
        rescan_override: Some(snap),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-drift-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(result.items[0].state, PortableAssetActionItemState::Failed);
    assert_eq!(
        result.items[0].error_code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED")
    );
    assert!(runner.calls().is_empty());
}

/// Business Logic: spawn 不确定 → outcomeUnknown 并 complete ledger 可 replay。
#[tokio::test]
async fn spawn_ambiguity_marks_outcome_unknown_and_completes_ledger() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_io_err(AppError::unavailable("spawn transport lost"));

    let item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "p@x",
        "/p",
        Some(true),
    );
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    let deps = PortableActionExecutorDeps {
        repo: repo.clone(),
        runner,
        env: None,
        pre_inventory: Some(snap.clone()),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(snap),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-unknown-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::OutcomeUnknown
    );
    let replay = claim_portable_asset_action(&repo, &plan.plan_token, "req-unknown-1")
        .await
        .unwrap();
    match replay {
        PortableActionClaim::Replay(json) => {
            let back: PortableAssetActionResultDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back, result);
        }
        other => panic!("expected replay, got {other:?}"),
    }
}

/// Business Logic: 部分项 blocked/部分成功 → 逐项 partial results。
#[tokio::test]
async fn partial_per_item_results() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_ok("ok");
    let ok_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "ok@p",
        "/ok",
        Some(false),
    );
    let mut bad = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "bad@p",
        "/bad",
        Some(false),
    );
    bad.capabilities.can_enable = false;
    let snap = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![ok_item.clone(), bad.clone()],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![
            ok_item.inventory_item_id.clone(),
            bad.inventory_item_id.clone(),
        ],
        PortableAssetActionKind::Enable,
        false,
    )
    .await;

    let mut ok_post = ok_item.clone();
    ok_post.actual_enabled = Some(true);
    let post = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![ok_post, bad.clone()],
    );
    let deps = PortableActionExecutorDeps {
        repo,
        runner,
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-partial-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(result.items.len(), 2);
    let by_id: BTreeMap<_, _> = result
        .items
        .iter()
        .map(|i| (i.inventory_item_id.clone(), i.state))
        .collect();
    assert_eq!(
        by_id.get(&ok_item.inventory_item_id),
        Some(&PortableAssetActionItemState::Succeeded)
    );
    assert_eq!(
        by_id.get(&bad.inventory_item_id),
        Some(&PortableAssetActionItemState::Blocked)
    );
}

/// Business Logic: Adopt 不得假成功，必须 PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED。
#[tokio::test]
async fn adopt_fail_closed_without_ownership_write() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "adopt-me",
        "/skills/adopt-me",
        Some(true),
    );
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Adopt,
        false,
    )
    .await;
    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap.clone()),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(snap),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-adopt-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(result.items[0].state, PortableAssetActionItemState::Failed);
    assert_eq!(
        result.items[0].error_code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED")
    );
    assert!(runner.calls().is_empty());
}

/// Business Logic: 过期 plan 在 claim 时 fail-closed。
#[tokio::test]
async fn expired_plan_rejected_at_claim() {
    let repo = test_repo().await;
    let item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "p@x",
        "/p",
        Some(true),
    );
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let mut plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    // 手工把 expires_at 改到过去并更新 DB
    plan.expires_at = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    // 直接 update row
    sqlx::query(
        "UPDATE agent_hub_portable_asset_action_plans SET expires_at = ? WHERE plan_token = ?",
    )
    .bind(&plan.expires_at)
    .bind(&plan.plan_token)
    .execute(&repo.pool())
    .await
    .unwrap();
    let err = claim_portable_asset_action(&repo, &plan.plan_token, "req-exp-1")
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("PORTABLE_ASSET_ACTION_PLAN_EXPIRED")
            || format!("{err:?}").contains("PORTABLE_ASSET_ACTION_PLAN_EXPIRED")
    );
}

/// Business Logic: Plugin uninstall 固定 --scope，keep_data 传入 argv。
#[tokio::test]
async fn plugin_uninstall_preserves_scope_and_keep_data_argv() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_ok("ok");
    let item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "shared@cc",
        "/plugins/shared",
        Some(true),
    );
    let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snap,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Uninstall,
        true,
    )
    .await;

    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![]);
    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-uninst-1".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    let args = &runner.calls()[0].args;
    assert!(args.iter().any(|a| a == "uninstall"));
    assert!(args.iter().any(|a| a == "--scope"));
    assert!(args.iter().any(|a| a == "--keep-data"));
}

/// Business Logic: 卸载官方 marketplace 行不得把另一 marketplace 的同名插件当成残留。
#[tokio::test]
async fn plugin_uninstall_rescan_ignores_other_marketplace_copy() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    runner.push_ok("ok");
    let official = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "superpowers@claude-plugins-official",
        "/plugins/cache/claude-plugins-official/superpowers/6.3.0",
        Some(false),
    );
    let marketplace = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "superpowers@superpowers-marketplace",
        "/plugins/cache/superpowers-marketplace/superpowers/6.1.1",
        Some(true),
    );
    let snap = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![official.clone(), marketplace.clone()],
    );
    let plan = preview_action(
        &repo,
        &snap,
        vec![official.inventory_item_id.clone()],
        PortableAssetActionKind::Uninstall,
        false,
    )
    .await;
    let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![marketplace]);
    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-uninst-official".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(
        result.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "other marketplace copy must not fail full uninstall: {:?}",
        result.items[0]
    );
    let args = &runner.calls()[0].args;
    assert!(args
        .iter()
        .any(|a| a == "superpowers@claude-plugins-official"));
}

/// Business Logic: partial manifest 仅放行 Activate/Render 时，Plugin deactivation
/// 既不能进入 adapter，也不能产生任何 CLI 调用。
#[tokio::test]
async fn partial_deactivate_capability_never_calls_plugin_remove() {
    let repo = test_repo().await;
    let runner = Arc::new(FakeProcessRunner::new());
    let mut item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "blocked@cc",
        "/plugins/blocked",
        Some(true),
    );
    item.capabilities.can_disable = false;
    item.capabilities.can_uninstall = false;
    item.capabilities.reason_code = Some("deactivate_package_not_supported".into());
    let snapshot = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
    let plan = preview_action(
        &repo,
        &snapshot,
        vec![item.inventory_item_id.clone()],
        PortableAssetActionKind::Uninstall,
        false,
    )
    .await;
    assert!(plan.changes[0]
        .blocking_reasons
        .iter()
        .any(|reason| reason == "PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED"));
    let item_gate = item_action_capability_block(
        &plan.changes[0],
        PortableAssetActionKind::Uninstall,
        Some(&item),
    )
    .expect("deactivation gate");
    assert!(matches!(
        item_gate,
        TargetActionRawOutcome::Blocked { ref code, .. }
            if code == "PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED"
    ));
    let mut unavailable = item.clone();
    unavailable.capabilities.reason_code = Some("portable_direct_action_unavailable".into());
    let unavailable_gate = item_action_capability_block(
        &plan.changes[0],
        PortableAssetActionKind::Uninstall,
        Some(&unavailable),
    )
    .expect("direct action gate");
    assert!(matches!(
        unavailable_gate,
        TargetActionRawOutcome::Blocked { ref code, .. }
            if code == "PORTABLE_ASSET_ACTION_ITEM_CAPABILITY_BLOCKED"
    ));

    let deps = PortableActionExecutorDeps {
        repo,
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snapshot.clone()),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(snapshot),
    };
    let result = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan.plan_token,
            client_request_id: "req-deactivate-blocked".into(),
        },
    )
    .await
    .expect("apply");
    assert_eq!(result.items[0].state, PortableAssetActionItemState::Blocked);
    assert!(
        runner.calls().is_empty(),
        "blocked uninstall must not spawn CLI"
    );
}

/// Business Logic: 卸下只拆当前 Agent 软链；仓库真树仍会被 inject，可能仍显示 enabled。
/// Code Logic: Detach 对账看 store_attached，不看 actual_enabled。
#[test]
fn detach_rescan_succeeds_when_unattached_even_if_catalog_still_enabled() {
    let mut pre = sample_item(
        AgentTarget::OpenCode,
        PortableAssetKind::Skill,
        "media-use",
        "/opencode/skills/media-use",
        Some(true),
    );
    pre.store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: true,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    let mut post = pre.clone();
    post.store.store_attached = false;
    post.actual_enabled = Some(true);
    let (state, code, _) = reconcile_item(
        PortableAssetActionKind::Detach,
        false,
        PortableAssetKind::Skill,
        &TargetActionRawOutcome::Applied,
        Some(&pre),
        Some(&post),
    );
    assert_eq!(state, PortableAssetActionItemState::Succeeded);
    assert!(code.is_none());
}

#[test]
fn detach_rescan_fails_when_native_link_still_attached() {
    let mut pre = sample_item(
        AgentTarget::OpenCode,
        PortableAssetKind::Skill,
        "media-use",
        "/opencode/skills/media-use",
        Some(true),
    );
    pre.store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: true,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    let (state, code, _) = reconcile_item(
        PortableAssetActionKind::Detach,
        false,
        PortableAssetKind::Skill,
        &TargetActionRawOutcome::Applied,
        Some(&pre),
        Some(&pre),
    );
    assert_eq!(state, PortableAssetActionItemState::Failed);
    assert_eq!(
        code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH")
    );
}

/// Business Logic: 借用卸下必须让源路径加载消失，不能把「仍经其他路径加载」当成成功。
/// Code Logic: pre.loaded_via_other_path 时 post 仍 via-other → mismatch；item 消失或不再 via-other → 成功。
#[test]
fn borrowed_detach_rescan_fails_when_still_loaded_via_other_path() {
    let mut pre = sample_item(
        AgentTarget::Grok,
        PortableAssetKind::Skill,
        "hyperframes-animation",
        "/claude/skills/hyperframes-animation",
        Some(true),
    );
    pre.origin_kind = PortableOriginKind::Compatibility;
    pre.store = PortableStoreFactDto {
        store_id: Some("skill:hyperframes-animation".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    let (state, code, _) = reconcile_item(
        PortableAssetActionKind::Detach,
        false,
        PortableAssetKind::Skill,
        &TargetActionRawOutcome::Applied,
        Some(&pre),
        Some(&pre),
    );
    assert_eq!(state, PortableAssetActionItemState::Failed);
    assert_eq!(
        code.as_deref(),
        Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH")
    );
}

#[test]
fn borrowed_detach_rescan_succeeds_when_item_gone() {
    let mut pre = sample_item(
        AgentTarget::Grok,
        PortableAssetKind::Skill,
        "hyperframes-animation",
        "/claude/skills/hyperframes-animation",
        Some(true),
    );
    pre.store = PortableStoreFactDto {
        store_id: Some("skill:hyperframes-animation".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    let (state, code, _) = reconcile_item(
        PortableAssetActionKind::Detach,
        false,
        PortableAssetKind::Skill,
        &TargetActionRawOutcome::Applied,
        Some(&pre),
        None,
    );
    assert_eq!(state, PortableAssetActionItemState::Succeeded);
    assert!(code.is_none());
}
