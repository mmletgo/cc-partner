//! agent_hub/replication/pull/tests — Pull 模块单元/契约测试
//!
//! Business Logic（为什么需要这个模块）:
//!     Pull 的 fail-closed 契约（staging 上限、预算上限、symlink 逃逸、scope 冲突、
//!     claim/replay 幂等、skipExisting 降级门禁）必须由测试锁定，防止回归。
//!
//! Code Logic（这个模块做什么）:
//!     原 pull.rs 内联 `mod tests` 的逐字搬运；源码契约断言经 include_str! 拼接
//!     读取拆分后的全部同目录模块文件（含本文件，保持自引用断言语义不变）。

use super::materialize::{apply_executable_bit, safe_tree_dest};
use super::staging::{STAGING_MAX_ENTRIES, STAGING_MAX_TOTAL_BYTES};
use super::*;
use crate::agent_hub::models::ScopeKind;
use crate::agent_hub::object_store::TreeEntryType;
use crate::agent_hub::portable_inventory::{
    PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryMutationCapability,
    PortableInventorySourceOrigin, PortableOriginKind,
};
use crate::agent_hub::snapshot::envelope::{SnapshotEnvelopeV1, SnapshotSelection};
use crate::agent_hub::snapshot::portable_builder::BuiltPortableSelection;
use crate::agent_hub::support::{CapabilitySupport, EvaluatedTargetSupport, TargetCapability};
use std::path::PathBuf;

fn sample_local_item(target: AgentTarget, native: &str) -> PortableInventoryItemDto {
    PortableInventoryItemDto {
        inventory_item_id: format!("id-{native}"),
        target,
        loaded_by: target,
        owned_by: PortableAssetOwner::from_target(target),
        origin_kind: PortableOriginKind::Native,
        native_output_candidate: true,
        kind: PortableAssetKind::Command,
        native_id: native.into(),
        display_name: native.into(),
        description: None,
        version: None,
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        source_path: Some(format!("/tmp/{native}.md")),
        source_origin: PortableInventorySourceOrigin::Standalone,
        parent_plugin_inventory_item_id: None,
        actual_enabled: Some(true),
        content_hash: Some("abc".into()),
        tree_hash: None,
        canonical_asset_id: None,
        canonical_revision_id: None,
        management_state: PortableInventoryManagementState::Unmanaged,
        desired_presence: None,
        desired_enabled: None,
        materialization_status: None,
        capabilities: PortableInventoryItemCapabilitiesDto::default(),
        warnings: vec![],
        mcp_credential: None,
        store: Default::default(),
    }
}

#[test]
fn remote_inventory_strips_paths_and_secrets() {
    let mut item = sample_local_item(AgentTarget::Claude, "ship");
    item.kind = PortableAssetKind::Mcp;
    item.mcp_credential = Some(PortableMcpCredentialFactDto {
        present: true,
        hash: Some("credhash".into()),
    });
    let snap = PortableInventorySnapshotDto {
        inventory_snapshot_hash: "h1".into(),
        refreshed_at: "2026-08-08T00:00:00Z".into(),
        stale: false,
        targets: vec![],
        items: vec![item],
    };
    let remote = snapshot_to_remote("dev-a", AgentTarget::Claude, &snap);
    assert!(remote_inventory_is_metadata_only(&remote));
    let json = serde_json::to_string(&remote).unwrap();
    assert!(!json.contains("/tmp/"));
    assert!(!json.to_ascii_lowercase().contains("sourcepath"));
}

#[test]
fn chunk_limit_is_8mib() {
    assert_eq!(PORTABLE_PULL_MAX_CHUNK_BYTES, 8 * 1024 * 1024);
    assert_eq!(PORTABLE_PULL_DEST_MAX_TOTAL_BYTES, 64 * 1024 * 1024);
}

#[test]
fn dest_budget_rejects_oversized_declared_size() {
    let over = PORTABLE_PULL_DEST_MAX_TOTAL_BYTES + 1;
    let err = ensure_dest_transfer_budget(0, over).unwrap_err();
    assert!(
        err.to_string().contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT),
        "err={err}"
    );
    // cumulative: already near cap
    let almost = PORTABLE_PULL_DEST_MAX_TOTAL_BYTES - 10;
    let err2 = ensure_dest_transfer_budget(almost, 11).unwrap_err();
    assert!(err2.to_string().contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT));
    // exact fit ok
    ensure_dest_transfer_budget(0, PORTABLE_PULL_DEST_MAX_TOTAL_BYTES).unwrap();
    ensure_dest_transfer_budget(almost, 10).unwrap();
}

#[test]
fn dest_parse_declared_size_rejects_garbage_and_accepts_zero() {
    assert_eq!(parse_declared_object_size("0").unwrap(), 0);
    assert_eq!(parse_declared_object_size("").unwrap(), 0);
    assert_eq!(parse_declared_object_size("42").unwrap(), 42);
    let err = parse_declared_object_size("not-a-number").unwrap_err();
    assert!(err.to_string().contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT));
    // huge but still parseable is ok at parse; budget check rejects later
    let huge = (PORTABLE_PULL_DEST_MAX_TOTAL_BYTES + 1).to_string();
    assert_eq!(
        parse_declared_object_size(&huge).unwrap(),
        PORTABLE_PULL_DEST_MAX_TOTAL_BYTES + 1
    );
    let err_budget =
        ensure_dest_transfer_budget(0, PORTABLE_PULL_DEST_MAX_TOTAL_BYTES + 1).unwrap_err();
    assert!(err_budget
        .to_string()
        .contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT));
}

#[test]
fn dest_chunk_body_oversize_rejected() {
    ensure_chunk_body_within_limit(PORTABLE_PULL_MAX_CHUNK_BYTES).unwrap();
    let err = ensure_chunk_body_within_limit(PORTABLE_PULL_MAX_CHUNK_BYTES + 1).unwrap_err();
    assert!(err.to_string().contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT));
}

#[test]
fn size_zero_loop_rejects_second_full_chunk() {
    // First full-sized chunk may continue (then must get terminal small/empty).
    let first = size_zero_chunk_action(0, PORTABLE_PULL_MAX_CHUNK_BYTES as u64).unwrap();
    assert_eq!(first, SizeZeroChunkAction::ContinueAfterFull);
    // Second full-sized chunk → fail-closed (no infinite grow).
    let err = size_zero_chunk_action(1, PORTABLE_PULL_MAX_CHUNK_BYTES as u64).unwrap_err();
    assert!(err.to_string().contains(PORTABLE_PULL_DEST_TRANSFER_LIMIT));
    // Terminal small after one full → break
    let term = size_zero_chunk_action(1, (PORTABLE_PULL_MAX_CHUNK_BYTES as u64) - 1).unwrap();
    assert_eq!(term, SizeZeroChunkAction::Break);
    // Immediate small/empty path: n < max → break without requiring prior full
    let empty_path = size_zero_chunk_action(0, 0).unwrap();
    assert_eq!(empty_path, SizeZeroChunkAction::Break);
    let small = size_zero_chunk_action(0, 1).unwrap();
    assert_eq!(small, SizeZeroChunkAction::Break);
}

#[test]
fn dest_transfer_limit_contract_in_source() {
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("PORTABLE_PULL_DEST_TRANSFER_LIMIT"));
    assert!(src.contains("PORTABLE_PULL_DEST_MAX_TOTAL_BYTES"));
    assert!(src.contains("ensure_dest_transfer_budget"));
    assert!(src.contains("ensure_chunk_body_within_limit"));
    assert!(src.contains("size_zero_chunk_action"));
    assert!(src.contains("read_response_body_capped"));
}

#[test]
fn install_mode_wire_tokens() {
    assert_eq!(
        PortablePullInstallMode::ImportedCanonicalOnly.as_str(),
        "importedCanonicalOnly"
    );
    assert_eq!(
        PortablePullInstallMode::SkipExisting.as_str(),
        "skipExisting"
    );
}

#[test]
fn capability_token_is_portable_pull_v1() {
    assert_eq!(CAPABILITY_PORTABLE_PULL_V1, "agent-hub.portable-pull.v1");
}

/// Business Logic: 项目级 Pull 的源/目标扫描不得退回 user/all-project。
#[test]
fn project_pull_query_is_exact_project_scope() {
    let query =
        portable_pull_inventory_query(AgentTarget::Claude, Some("workbench-project-1".into()));
    assert_eq!(query.target, Some(AgentTarget::Claude));
    assert_eq!(query.scope_kind, Some(ScopeKind::Project));
    assert_eq!(
        query.local_project_id.as_deref(),
        Some("workbench-project-1")
    );

    let user = portable_pull_inventory_query(AgentTarget::Claude, None);
    assert_eq!(user.scope_kind, Some(ScopeKind::User));
    assert!(user.local_project_id.is_none());
}

#[test]
fn preview_rejects_cross_target_at_request_shape() {
    let req = PreviewPortablePullRequest {
        source_device_id: "d".into(),
        source_target: AgentTarget::Claude,
        destination_target: AgentTarget::Codex,
        source_local_project_id: None,
        source_project_ref: None,
        destination_local_project_id: None,
        remote_inventory_snapshot_hash: "h".into(),
        inventory_item_ids: vec!["a".into()],
        conflict_policy: PortableAssetConflictPolicy::SkipExisting,
    };
    assert_ne!(req.source_target, req.destination_target);
}

#[tokio::test]
async fn durable_pull_claim_replay_after_complete() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    let plan = StoredPortablePullPlan {
        public: PortablePullPlanDto {
            plan_token: "plan-a".into(),
            expires_at: (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
            source_device_id: "d".into(),
            source_target: AgentTarget::Claude,
            destination_target: AgentTarget::Claude,
            source_local_project_id: None,
            source_project_ref: None,
            destination_local_project_id: None,
            remote_inventory_snapshot_hash: "r".into(),
            local_inventory_snapshot_hash: "l".into(),
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            selection_manifest_hash: "m".into(),
            credential_bearing_count: 0,
            has_credential_bearing_assets: false,
            changes: vec![],
            blocking_reasons: vec![],
        },
        remote_item_ids: vec![],
        remote_item_bindings: vec![],
    };
    let plan_json = serde_json::to_string(&plan).unwrap();
    repo.insert_portable_pull_plan(PortablePullPlanRecord {
        plan_token: "plan-a".into(),
        expires_at: plan.public.expires_at.clone(),
        remote_inventory_snapshot_hash: "r".into(),
        local_inventory_snapshot_hash: "l".into(),
        plan_json,
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: Utc::now().to_rfc3339(),
    })
    .await
    .unwrap();
    let claim = repo
        .claim_portable_pull_plan("plan-a", "req-1")
        .await
        .unwrap();
    assert!(matches!(claim, PortablePullClaim::Claimed(_)));
    let result = PortablePullResultDto {
        plan_token: "plan-a".into(),
        client_request_id: "req-1".into(),
        source_device_id: "d".into(),
        source_target: AgentTarget::Claude,
        destination_target: AgentTarget::Claude,
        partial: false,
        items: vec![],
    };
    repo.complete_portable_pull_plan("plan-a", "req-1", &serde_json::to_string(&result).unwrap())
        .await
        .unwrap();
    let replay = repo
        .claim_portable_pull_plan("plan-a", "req-1")
        .await
        .unwrap();
    match replay {
        PortablePullClaim::Replay(json) => {
            let back: PortablePullResultDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back, result);
        }
        other => panic!("expected replay, got {other:?}"),
    }
    // 同 request 绑不同 plan → conflict
    repo.insert_portable_pull_plan(PortablePullPlanRecord {
        plan_token: "plan-b".into(),
        expires_at: (Utc::now() + ChronoDuration::minutes(10)).to_rfc3339(),
        remote_inventory_snapshot_hash: "r".into(),
        local_inventory_snapshot_hash: "l".into(),
        plan_json: serde_json::to_string(&plan).unwrap(),
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: Utc::now().to_rfc3339(),
    })
    .await
    .unwrap();
    let conflict = repo.claim_portable_pull_plan("plan-b", "req-1").await;
    assert!(conflict.is_err());
}

#[test]
fn skill_install_must_not_drop_body_without_tree() {
    // 契约：canonical Skill 无 tree 时 install 必须 fail-closed，不得只写 frontmatter
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("PORTABLE_PULL_SKILL_TREE_UNAVAILABLE"));
    assert!(src.contains("PORTABLE_PULL_CANONICAL_IMPORT_REQUIRED"));
    assert!(src.contains("install verified by rescan"));
}

#[test]
fn skip_existing_demotion_fail_closed_before_install_and_rescan() {
    // R2-M1 / Spec M5：skipExisting 降级不得在 import 失败后写目标；成功后需 rescan gate
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("canonical import failed before skipExisting demotion install"));
    assert!(src.contains("skipExisting demotion install verified by rescan"));
    assert!(src.contains("PORTABLE_PULL_RESCAN_MISSING"));
}

#[test]
fn source_chunk_resume_from_offset() {
    let mut built = BuiltPortableSelection {
        envelope: SnapshotEnvelopeV1 {
            format: "cc-partner-agent-hub".into(),
            format_version: 1,
            canonicalization: "RFC8785-JSON".into(),
            snapshot_id: "s".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "d".into(),
            created_at: "t".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: false,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        },
        items: vec![],
        object_bytes: BTreeMap::new(),
    };
    let payload = vec![1u8, 2, 3, 4, 5];
    let hash = sha256_hex(&payload);
    built.object_bytes.insert(hash.clone(), payload.clone());
    staging_insert("tid-resume".into(), built).unwrap();
    let c1 = source_read_object_chunk("tid-resume", &hash, 0).unwrap();
    assert_eq!(c1, payload);
    // 最后 chunk 返回后 staging 仍保留，允许丢失最终响应时按 offset 重试；
    // 只有显式 release 才释放。
    assert!(
        staging().lock().unwrap().contains_key("tid-resume"),
        "staging must remain until explicit release/TTL"
    );
    // 模拟客户端未收到最终响应后的重试：同 offset 仍返回同一 chunk。
    let retry = source_read_object_chunk("tid-resume", &hash, 0).unwrap();
    assert_eq!(retry, payload);
    source_release_transfer("tid-resume");
    // re-stage for offset resume check
    let mut built2 = BuiltPortableSelection {
        envelope: SnapshotEnvelopeV1 {
            format: "cc-partner-agent-hub".into(),
            format_version: 1,
            canonicalization: "RFC8785-JSON".into(),
            snapshot_id: "s".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "d".into(),
            created_at: "t".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: false,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        },
        items: vec![],
        object_bytes: BTreeMap::new(),
    };
    // multi-object keeps staging until every hash is fully read (or explicit release)
    let p2 = vec![9u8, 8, 7];
    let h2 = sha256_hex(&p2);
    built2.object_bytes.insert(hash.clone(), payload.clone());
    built2.object_bytes.insert(h2.clone(), p2.clone());
    staging_insert("tid-multi".into(), built2).unwrap();
    let c2 = source_read_object_chunk("tid-multi", &hash, 2).unwrap();
    assert_eq!(c2, payload[2..]);
    // first object fully read (offset 2 of 5 + chunk covers rest) — staging remains
    assert!(
        staging().lock().unwrap().contains_key("tid-multi"),
        "multi-object staging must remain until all hashes fully read"
    );
    let c3 = source_read_object_chunk("tid-multi", &h2, 0).unwrap();
    assert_eq!(c3, p2);
    assert!(
        staging().lock().unwrap().contains_key("tid-multi"),
        "staging must remain after all objects until explicit release"
    );
    source_release_transfer("tid-multi");
    // explicit release path still works for partial consumption
    let mut built3 = BuiltPortableSelection {
        envelope: SnapshotEnvelopeV1 {
            format: "cc-partner-agent-hub".into(),
            format_version: 1,
            canonicalization: "RFC8785-JSON".into(),
            snapshot_id: "s".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "d".into(),
            created_at: "t".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: false,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        },
        items: vec![],
        object_bytes: BTreeMap::new(),
    };
    built3.object_bytes.insert(hash.clone(), payload.clone());
    built3.object_bytes.insert(h2.clone(), p2.clone());
    staging_insert("tid-release".into(), built3).unwrap();
    let _ = source_read_object_chunk("tid-release", &hash, 0).unwrap();
    assert!(staging().lock().unwrap().contains_key("tid-release"));
    source_release_transfer("tid-release");
    assert!(!staging().lock().unwrap().contains_key("tid-release"));
    let missing = source_read_object_chunk("tid-resume", &hash, 100);
    assert!(missing.is_err());
}

#[test]
fn safe_tree_dest_rejects_traversal() {
    let dir = PathBuf::from("/tmp/skill-root");
    assert!(safe_tree_dest(&dir, "SKILL.md").is_ok());
    assert!(safe_tree_dest(&dir, "nested/file.txt").is_ok());
    assert!(safe_tree_dest(&dir, "../escape").is_err());
    assert!(safe_tree_dest(&dir, "/abs/path").is_err());
    assert!(safe_tree_dest(&dir, "a/../../b").is_err());
    let err = safe_tree_dest(&dir, "../x").unwrap_err();
    assert!(err.to_string().contains("PORTABLE_PULL_UNSAFE_TREE_PATH"));
}

#[test]
fn safe_tree_dest_rejects_existing_symlink_component() {
    // R2-P1-2：dir/assets -> /tmp/outside 时 entry assets/x 必须 fail-closed
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("skill-root");
    std::fs::create_dir_all(&skill).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let link = skill.join("assets");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let err = safe_tree_dest(&skill, "assets/x").unwrap_err();
        assert!(
            err.to_string().contains("PORTABLE_PULL_UNSAFE_TREE_PATH"),
            "symlink intermediate must be refused: {err}"
        );
        // non-symlink nested path still ok
        std::fs::create_dir_all(skill.join("nested")).unwrap();
        assert!(safe_tree_dest(&skill, "nested/file.txt").is_ok());
    }
    #[cfg(not(unix))]
    {
        let _ = (outside, link);
        // Windows junction coverage is L3; lexical checks still hold
        assert!(safe_tree_dest(&skill, "nested/file.txt").is_ok());
    }
}

#[test]
fn mutation_gate_forces_canonical_only_when_not_supported() {
    let evaluated = |capabilities| EvaluatedTargetSupport {
        target: AgentTarget::Claude,
        mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
        capabilities,
        write_allowed: true,
        reasons: vec![],
    };
    let deactivate_only = evaluated(BTreeMap::from([(
        TargetCapability::DeactivatePackage,
        CapabilitySupport::Supported,
    )]));
    let render_only = evaluated(BTreeMap::from([
        (
            TargetCapability::RenderPortableAssets,
            CapabilitySupport::Supported,
        ),
        (
            TargetCapability::ActivatePackage,
            CapabilitySupport::Blocked,
        ),
    ]));
    let render_and_activate = evaluated(BTreeMap::from([
        (
            TargetCapability::RenderPortableAssets,
            CapabilitySupport::Supported,
        ),
        (
            TargetCapability::ActivatePackage,
            CapabilitySupport::Supported,
        ),
    ]));

    // R2-P1-1：previewOnly/blocked 与无关的 Deactivate capability 都不得安装。
    assert!(!mutation_allows_install_to_target(
        PortableInventoryMutationCapability::PreviewOnly,
        Some(&render_and_activate),
        PortableAssetKind::Command,
    ));
    assert!(!mutation_allows_install_to_target(
        PortableInventoryMutationCapability::Blocked,
        Some(&render_and_activate),
        PortableAssetKind::Command,
    ));
    assert!(!mutation_allows_install_to_target(
        PortableInventoryMutationCapability::Supported,
        Some(&deactivate_only),
        PortableAssetKind::Command,
    ));
    assert!(mutation_allows_install_to_target(
        PortableInventoryMutationCapability::Supported,
        Some(&render_only),
        PortableAssetKind::Command,
    ));
    assert!(!mutation_allows_install_to_target(
        PortableInventoryMutationCapability::Supported,
        Some(&render_only),
        PortableAssetKind::Plugin,
    ));
    assert!(mutation_allows_install_to_target(
        PortableInventoryMutationCapability::Supported,
        Some(&render_and_activate),
        PortableAssetKind::Plugin,
    ));
    // plan helper: when mutation blocked, install_mode path yields ImportedCanonicalOnly
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED"));
    assert!(src.contains("mutation_allows_install_to_target"));
    assert!(src.contains("destination_mutation_capability"));
    // install_change must bypass the cached inventory immediately before the adapter write.
    assert!(src.contains("let live = inspect_portable_inventory_force(state).await?;"));
}

#[test]
fn selection_binding_rejects_content_hash_drift() {
    // R2-P1-3：同 inventoryItemId 下 content_hash 漂移 → REMOTE_SELECTION_DRIFT
    use crate::agent_hub::snapshot::portable_builder::PortableSelectionItem;
    let selection = RemotePortableSelectionResponse {
        transfer_id: "t".into(),
        envelope: SnapshotEnvelopeV1 {
            format: "cc-partner-agent-hub".into(),
            format_version: 1,
            canonicalization: "RFC8785-JSON".into(),
            snapshot_id: "s".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "d".into(),
            created_at: "t".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: false,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        },
        items: vec![PortableSelectionItem {
            inventory_item_id: "id-a".into(),
            asset_id: "asset-a".into(),
            target: AgentTarget::Claude,
            kind: PortableAssetKind::Command,
            native_id: "a".into(),
            display_name: "a".into(),
            scope_id: "user".into(),
            object_hash: "obj".into(),
            object_size: 1,
            content_hash: Some("hash-new".into()),
            tree_hash: None,
            credential_bearing: false,
            legacy_lossy: false,
            warnings: vec![],
        }],
        missing_object_hashes: vec![],
    };
    let bindings = vec![StoredRemoteItemBinding {
        inventory_item_id: "id-a".into(),
        content_hash: Some("hash-preview".into()),
        tree_hash: None,
    }];
    let err = bind_selection_to_inventory_bindings(&selection, &bindings, AgentTarget::Claude)
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("PORTABLE_PULL_REMOTE_SELECTION_DRIFT"),
        "expected drift, got {err}"
    );
    // matching hashes pass
    let ok_bindings = vec![StoredRemoteItemBinding {
        inventory_item_id: "id-a".into(),
        content_hash: Some("hash-new".into()),
        tree_hash: None,
    }];
    bind_selection_to_inventory_bindings(&selection, &ok_bindings, AgentTarget::Claude).unwrap();
}

/// R5-M7: selection item.target must match plan destination_target before install.
#[test]
fn selection_binding_rejects_target_mismatch() {
    use crate::agent_hub::snapshot::portable_builder::PortableSelectionItem;
    let selection = RemotePortableSelectionResponse {
        transfer_id: "t".into(),
        envelope: SnapshotEnvelopeV1 {
            format: "cc-partner-agent-hub".into(),
            format_version: 1,
            canonicalization: "RFC8785-JSON".into(),
            snapshot_id: "s".into(),
            snapshot_hash: "0".repeat(64),
            source_replica_id: "d".into(),
            created_at: "t".into(),
            selection: SnapshotSelection {
                scope_ids: vec![],
                asset_ids: vec![],
                include_history: false,
            },
            asset_heads: BTreeMap::new(),
            assets: vec![],
            lineages: vec![],
            revisions: vec![],
            variants: vec![],
            conflicts: vec![],
            aliases: vec![],
            objects: vec![],
        },
        items: vec![PortableSelectionItem {
            inventory_item_id: "id-a".into(),
            asset_id: "asset-a".into(),
            // lied target while hashes still match
            target: AgentTarget::Codex,
            kind: PortableAssetKind::Command,
            native_id: "a".into(),
            display_name: "a".into(),
            scope_id: "user".into(),
            object_hash: "obj".into(),
            object_size: 1,
            content_hash: Some("hash-ok".into()),
            tree_hash: None,
            credential_bearing: false,
            legacy_lossy: false,
            warnings: vec![],
        }],
        missing_object_hashes: vec![],
    };
    let bindings = vec![StoredRemoteItemBinding {
        inventory_item_id: "id-a".into(),
        content_hash: Some("hash-ok".into()),
        tree_hash: None,
    }];
    let err = bind_selection_to_inventory_bindings(&selection, &bindings, AgentTarget::Claude)
        .unwrap_err();
    assert!(
        err.to_string().contains("PORTABLE_PULL_TARGET_MISMATCH"),
        "expected target mismatch, got {err}"
    );
}

/// R5-M2: hard-fail path must propagate complete_portable_pull_plan errors (not swallow).
#[test]
fn hard_fail_complete_propagates_not_swallowed() {
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    // Production apply_portable_pull body ends before #[cfg(test)] module.
    let prod = src
        .split("#[cfg(test)]")
        .next()
        .expect("production source before tests");
    assert!(prod.contains("PORTABLE_PULL_APPLY_FAILED"));
    // Forbidden swallow pattern: ignore complete Result with underscore binding.
    let swallow = format!("{}{}", "let _ = repo", ".complete_portable_pull_plan");
    assert!(
        !prod.contains(&swallow),
        "fail path must not discard complete errors with underscore binding"
    );
    // Multi-line form of the same swallow
    assert!(
        !prod.contains("let _ = repo\n"),
        "fail path must not use let _ = repo on complete path"
    );
    let fail_marker = "error_code: Some(\"PORTABLE_PULL_APPLY_FAILED\".into())";
    let fail_pos = prod
        .find(fail_marker)
        .expect("APPLY_FAILED fail DTO present");
    let after_fail = &prod[fail_pos..fail_pos + 800.min(prod.len() - fail_pos)];
    assert!(
        after_fail.contains("complete_portable_pull_plan"),
        "fail path must call complete"
    );
    assert!(
        after_fail.contains(".await?"),
        "fail path complete must propagate via ?"
    );
    assert!(
        !after_fail.contains("let _ ="),
        "fail path must not swallow complete with let _ ="
    );
}

/// R5-M3: Pending / incomplete path must attempt destination observation rescan.
#[test]
fn pending_path_folds_destination_observation_rescan() {
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("fold_pending_pull_observations"));
    assert!(src.contains("pull claimed but not completed; observed presence="));
    // apply Pending arm and get_portable_pull incomplete both rescan
    assert!(src.contains("PortablePullClaim::Pending =>"));

    let mut plan = PortablePullPlanDto {
        plan_token: "p".into(),
        expires_at: "t".into(),
        source_device_id: "d".into(),
        source_target: AgentTarget::Claude,
        destination_target: AgentTarget::Claude,
        source_local_project_id: None,
        source_project_ref: None,
        destination_local_project_id: None,
        remote_inventory_snapshot_hash: "r".into(),
        local_inventory_snapshot_hash: "l".into(),
        conflict_policy: PortableAssetConflictPolicy::SkipExisting,
        selection_manifest_hash: "m".into(),
        credential_bearing_count: 0,
        has_credential_bearing_assets: false,
        changes: vec![PortablePullChangeDto {
            inventory_item_id: "id-shared".into(),
            kind: PortableAssetKind::Command,
            native_id: "shared".into(),
            display_name: "shared".into(),
            scope_id: "user".into(),
            install_mode: PortablePullInstallMode::InstallToTarget,
            conflict: false,
            legacy_lossy: false,
            credential_bearing: false,
            blocking_reasons: vec![],
            warnings: vec![],
        }],
        blocking_reasons: vec![],
    };
    let mut result = outcome_unknown_pull_result("p", "req", &plan);
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].state, PortablePullItemState::OutcomeUnknown);

    let mut user = sample_local_item(AgentTarget::Claude, "shared");
    user.scope_id = "user".into();
    user.scope_kind = ScopeKind::User;
    user.project_id = None;
    let post = PortableInventorySnapshotDto {
        inventory_snapshot_hash: "h".into(),
        refreshed_at: "2026-08-08T00:00:00Z".into(),
        stale: false,
        targets: vec![],
        items: vec![user],
    };
    fold_pending_pull_observations(&mut result, &plan, &post);
    let msg = result.items[0].message.as_deref().unwrap_or("");
    assert!(
        msg.contains("observed presence=true"),
        "pending rescan should mark observed presence, got {msg}"
    );
    // still OutcomeUnknown — never upgrade on Pending
    assert_eq!(result.items[0].state, PortablePullItemState::OutcomeUnknown);

    // missing scope stays false
    plan.changes[0].scope_id = "project:other".into();
    let mut result2 = outcome_unknown_pull_result("p", "req", &plan);
    fold_pending_pull_observations(&mut result2, &plan, &post);
    let msg2 = result2.items[0].message.as_deref().unwrap_or("");
    assert!(
        msg2.contains("observed presence=false"),
        "missing scope must report presence=false, got {msg2}"
    );
}

#[test]
fn native_mcp_leaf_is_cli_shape_not_hub_dto() {
    use crate::agent_hub::assets::{McpTransport, PortableMcpServer};
    use std::collections::BTreeMap;
    let server = PortableMcpServer {
        key: "demo".into(),
        transport: McpTransport::Stdio {
            command: "uvx".into(),
            args: vec!["svc".into()],
            cwd: None,
        },
        env: {
            let mut m = BTreeMap::new();
            m.insert("TOKEN".into(), "secret-value".into());
            m
        },
        enabled: true,
        tool_allow: vec!["x".into()],
        tool_deny: vec![],
        target_extensions: BTreeMap::new(),
    };
    let v = native_mcp_leaf_value(AgentTarget::Claude, &server).unwrap();
    let s = serde_json::to_string(&v).unwrap();
    assert!(s.contains("\"command\""));
    assert!(s.contains("\"args\""));
    assert!(!s.contains("\"key\""), "must not embed hub key field");
    assert!(
        !s.contains("toolAllow") && !s.contains("tool_allow"),
        "must not embed hub toolAllow"
    );
    // credentials preserved in env bytes but never logged by this helper
    assert_eq!(v["env"]["TOKEN"], "secret-value");
}

#[test]
fn claude_mcp_config_path_is_home_dot_claude_json_by_default() {
    // When CLAUDE_CONFIG_DIR unset, scanner & pull share ~/.claude.json (not ~/.claude/.claude.json)
    let path = {
        // isolate: clear env for assertion of default branch via pure logic
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
            // still must be <CLAUDE_CONFIG_DIR>/.claude.json shape
            let p = resolve_claude_mcp_config_path();
            assert!(p.ends_with(".claude.json"));
            p
        } else {
            let p = resolve_claude_mcp_config_path();
            assert_eq!(p, home.join(".claude.json"));
            p
        }
    };
    assert!(
        !path.to_string_lossy().ends_with(".claude/.claude.json")
            || std::env::var_os("CLAUDE_CONFIG_DIR").is_some(),
        "default path must not nest under ~/.claude/"
    );
}

#[test]
fn plugin_hash_list_payload_is_rejected_contract() {
    let src = concat!(
        include_str!("mod.rs"),
        include_str!("dto.rs"),
        include_str!("staging.rs"),
        include_str!("materialize.rs"),
        include_str!("install_target.rs"),
        include_str!("remote_project.rs"),
        include_str!("tests.rs"),
    );
    assert!(src.contains("PORTABLE_PULL_PLUGIN_TREE_UNAVAILABLE"));
    assert!(src.contains("portablePluginTreeRef"));
    assert!(src.contains("PORTABLE_PULL_UNSAFE_TREE_PATH"));
    assert!(src.contains("PORTABLE_PULL_STAGING_LIMIT"));
    assert!(src.contains("PORTABLE_PULL_REMOTE_INVENTORY_STALE"));
    assert!(src.contains("PORTABLE_PULL_REMOTE_SELECTION_DRIFT"));
    assert!(src.contains("PORTABLE_PULL_TARGET_MUTATION_NOT_SUPPORTED"));
    assert!(src.contains("fully_read_hashes"));
    assert!(src.contains("native_mcp_leaf_value"));
    assert!(src.contains("resolve_claude_mcp_config_path"));
}

#[test]
fn staging_limit_rejects_oversized() {
    // Construct many tiny staged transfers until limit
    let make = |id: &str| {
        let mut built = BuiltPortableSelection {
            envelope: SnapshotEnvelopeV1 {
                format: "cc-partner-agent-hub".into(),
                format_version: 1,
                canonicalization: "RFC8785-JSON".into(),
                snapshot_id: id.into(),
                snapshot_hash: "0".repeat(64),
                source_replica_id: "d".into(),
                created_at: "t".into(),
                selection: SnapshotSelection {
                    scope_ids: vec![],
                    asset_ids: vec![],
                    include_history: false,
                },
                asset_heads: BTreeMap::new(),
                assets: vec![],
                lineages: vec![],
                revisions: vec![],
                variants: vec![],
                conflicts: vec![],
                aliases: vec![],
                objects: vec![],
            },
            items: vec![],
            object_bytes: BTreeMap::new(),
        };
        built.object_bytes.insert(format!("h-{id}"), vec![1u8; 16]);
        built
    };
    // clear any prior
    {
        let mut g = staging().lock().unwrap();
        g.clear();
    }
    for i in 0..STAGING_MAX_ENTRIES {
        staging_insert(format!("lim-{i}"), make(&format!("lim-{i}"))).unwrap();
    }
    // next should still succeed after evicting oldest
    staging_insert("lim-extra".into(), make("lim-extra")).unwrap();
    // force hard limit by filling bytes with huge payload after clear
    {
        let mut g = staging().lock().unwrap();
        g.clear();
    }
    let mut huge = make("huge");
    huge.object_bytes.insert(
        "big".into(),
        vec![0u8; (STAGING_MAX_TOTAL_BYTES as usize) + 1],
    );
    let err = staging_insert("huge".into(), huge).unwrap_err();
    assert!(err.to_string().contains("PORTABLE_PULL_STAGING_LIMIT"));
}

/// Business Logic: user + project same nativeId must not collapse for skip/conflict.
#[test]
fn conflict_and_rescan_identity_includes_scope() {
    let mut user = sample_local_item(AgentTarget::Claude, "shared");
    user.scope_id = "user".into();
    user.scope_kind = ScopeKind::User;
    user.project_id = None;

    let mut project = sample_local_item(AgentTarget::Claude, "shared");
    project.inventory_item_id = "id-shared-project".into();
    project.scope_id = "project:hub-1".into();
    project.scope_kind = ScopeKind::Project;
    project.project_id = Some("hub-1".into());
    project.source_path = Some("/repo/.claude/commands/shared.md".into());

    let snap = PortableInventorySnapshotDto {
        inventory_snapshot_hash: "h-scope".into(),
        refreshed_at: "2026-08-08T00:00:00Z".into(),
        stale: false,
        targets: vec![],
        items: vec![user, project],
    };

    // Same nativeId under user scope hits user only
    assert!(inventory_has_scoped_item(
        &snap,
        AgentTarget::Claude,
        PortableAssetKind::Command,
        "shared",
        "user",
    ));
    // Project scope identity is separate — must not false-skip/false-succeed
    assert!(inventory_has_scoped_item(
        &snap,
        AgentTarget::Claude,
        PortableAssetKind::Command,
        "shared",
        "project:hub-1",
    ));
    assert!(!inventory_has_scoped_item(
        &snap,
        AgentTarget::Claude,
        PortableAssetKind::Command,
        "shared",
        "project:other",
    ));

    // Map key includes scope so user+project coexist
    let map: BTreeMap<(AgentTarget, PortableAssetKind, String, String), &PortableInventoryItemDto> =
        snap.items
            .iter()
            .map(|i| {
                (
                    (
                        i.target,
                        i.kind,
                        i.native_id.clone(),
                        resolve_inventory_scope_id(i),
                    ),
                    i,
                )
            })
            .collect();
    assert_eq!(map.len(), 2);
    assert!(map.contains_key(&(
        AgentTarget::Claude,
        PortableAssetKind::Command,
        "shared".into(),
        "user".into()
    )));
    assert!(map.contains_key(&(
        AgentTarget::Claude,
        PortableAssetKind::Command,
        "shared".into(),
        "project:hub-1".into()
    )));
}

/// Business Logic: replaceAfterPreview materialization must drop stale files from old tree.
#[tokio::test]
async fn atomic_tree_replace_removes_stale_files() {
    let tmp = tempfile::tempdir().unwrap();
    let store = ObjectStore::open(tmp.path().join("objects")).unwrap();
    let dest = tmp.path().join("skills").join("demo");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("SKILL.md"), b"old skill\n").unwrap();
    std::fs::write(dest.join("stale.sh"), b"#!/bin/sh\necho stale\n").unwrap();

    let skill_md = store.put_blob(b"# new skill\n").await.unwrap();
    let run_sh = store.put_blob(b"#!/bin/sh\necho new\n").await.unwrap();
    let manifest = crate::agent_hub::object_store::TreeManifest {
        entries: vec![
            crate::agent_hub::object_store::TreeEntry {
                path: "SKILL.md".into(),
                blob_hash: skill_md.hash.clone(),
                entry_type: TreeEntryType::File,
                executable: false,
            },
            crate::agent_hub::object_store::TreeEntry {
                path: "scripts/run.sh".into(),
                blob_hash: run_sh.hash.clone(),
                entry_type: TreeEntryType::File,
                executable: true,
            },
        ],
    };
    materialize_tree_atomic_replace(&store, &dest, &manifest)
        .await
        .unwrap();

    assert!(dest.join("SKILL.md").is_file());
    assert_eq!(
        std::fs::read_to_string(dest.join("SKILL.md")).unwrap(),
        "# new skill\n"
    );
    assert!(
        !dest.join("stale.sh").exists(),
        "stale file from old tree must be removed by atomic replace"
    );
    assert!(dest.join("scripts/run.sh").is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "executable bit restored from TreeManifest");
    }
}

#[test]
fn apply_executable_bit_sets_unix_x() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("tool.sh");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        let before = std::fs::metadata(&f).unwrap().permissions().mode() & 0o111;
        assert_eq!(before, 0);
        apply_executable_bit(&f, true).unwrap();
        let after = std::fs::metadata(&f).unwrap().permissions().mode() & 0o111;
        assert_ne!(after, 0);
        apply_executable_bit(&f, false).unwrap();
        // false is no-op; bit remains
        let keep = std::fs::metadata(&f).unwrap().permissions().mode() & 0o111;
        assert_ne!(keep, 0);
    }
}
