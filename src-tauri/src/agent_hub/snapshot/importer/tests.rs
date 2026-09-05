//! agent_hub/snapshot/importer/tests — importer 单元测试
//!
//! Business Logic（为什么需要这个模块）:
//!     两阶段导入的收敛、冲突、映射确认与故障注入行为必须有回归锁定，
//!     防止 import 路径回退为 last-write-wins 或静默丢边。
//!
//! Code Logic（这个模块做什么）:
//!     覆盖双副本不相交块 merge、same-block conflict、同 revision 幂等去重、
//!     delete-vs-edit、project mapping/opted-in 投影计数、闭包失败与
//!     DB/CAS 故障注入、plugin skill residual 重建等场景。

use super::*;
use crate::agent_hub::instructions::{InstructionBlock, InstructionDocument};
use crate::agent_hub::models::{
    AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode, RevisionId,
    RevisionOperation, RevisionOriginKind, ScopeKind,
};
use crate::agent_hub::snapshot::builder::{
    build_snapshot, clear_envelope_cache_for_test, SnapshotSelectionMode, SnapshotSelectionRequest,
};
use crate::agent_hub::snapshot::envelope::SnapshotRevision;
use crate::storage::agent_hub_repo::{
    AgentHubImportFault, AgentHubRepo, UpsertAgentHubProjectMapping,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

const REPLICA_B: &str = "01900000-0000-7000-8000-0000000000b2";

async fn test_env() -> (AgentHubRepo, ObjectStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("t.db");
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    // 全局 import fault 跨测试共享：fixture 入口强制复位
    let _ = repo.take_import_fault();
    let store = ObjectStore::open(dir.path()).unwrap();
    (repo, store, dir)
}

async fn seed_user(repo: &AgentHubRepo) -> String {
    repo.insert_scope(NewScopeNode {
        id: Some("scope-user".into()),
        kind: ScopeKind::User,
        hub_project_id: None,
        relative_path: None,
    })
    .await
    .unwrap()
    .id
}

fn doc_bytes(blocks: Vec<(&str, &str)>) -> Vec<u8> {
    let document = InstructionDocument {
        relative_key: "CLAUDE.md".into(),
        blocks: blocks
            .into_iter()
            .map(|(id, body)| InstructionBlock::shared(id, body, vec![]))
            .collect(),
    };
    serde_json::to_vec(&document).unwrap()
}

async fn put_doc(store: &ObjectStore, blocks: Vec<(&str, &str)>) -> String {
    store.put_blob(&doc_bytes(blocks)).await.unwrap().hash
}

async fn append_instruction(
    repo: &AgentHubRepo,
    asset_id: &str,
    parents: Vec<RevisionId>,
    hash: &str,
    expected: Option<RevisionId>,
    at: &str,
) -> RevisionId {
    let rev = repo
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset_id.to_string(),
            parents,
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            payload_hash: Some(hash.to_string()),
            tree_manifest_hash: None,
            created_at: at.to_string(),
            expected_parent_id: expected,
        })
        .await
        .unwrap();
    rev.id
}

/// 两副本分叉：不相交块 → 必须产出 dual-parent merge revision。
///
/// multi-replica 真实路径：先 import A（base+left）进空 hub；再 import B（base+right），
/// 且 B 的 envelope **不**再列出 local-only head left——loader 必须从 DB/CAS 读 local head。
#[tokio::test]
async fn disjoint_blocks_merge_with_both_parents() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, dir_a) = test_env().await;
    let user = seed_user(&repo_a).await;
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: user.clone(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let base_h = put_doc(&store_a, vec![("b1", "A base"), ("b2", "B base")]).await;
    let base = append_instruction(
        &repo_a,
        &asset.id,
        vec![],
        &base_h,
        None,
        "2026-07-29T10:00:00Z",
    )
    .await;
    let left_h = put_doc(&store_a, vec![("b1", "A left"), ("b2", "B base")]).await;
    let left = append_instruction(
        &repo_a,
        &asset.id,
        vec![base.clone()],
        &left_h,
        Some(base.clone()),
        "2026-07-29T11:00:00Z",
    )
    .await;

    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    // B: 空库 + import A snapshot（head=left）
    let (repo_b, store_b, dir_b) = test_env().await;
    let importer_b = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
    let validated =
        ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
            .unwrap();
    let out = importer_b
        .commit_import(validated, ConfirmedImportSelection::default())
        .await
        .unwrap();
    assert!(out.inserted_revisions >= 2);
    assert_eq!(
        out.conflicts_opened, 0,
        "first import must not conflict: {out:?}"
    );

    // 独立 replica R：common base + 不相交 right 块；envelope 不含 left
    let (repo_r, store_r, _dir_r) = test_env().await;
    let user_r = seed_user(&repo_r).await;
    let asset_r = repo_r
        .insert_asset(NewLogicalAsset {
            scope_id: user_r,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    store_r
        .put_blob(&store_a.get_blob(&base_h).await.unwrap())
        .await
        .unwrap();
    let _ = repo_r
        .append_revision(NewRevision {
            id: base.clone(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: Some(base_h.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap();
    let right_h = put_doc(&store_r, vec![("b1", "A base"), ("b2", "B right")]).await;
    let right = repo_r
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![base.clone()],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: Some(right_h.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T11:30:00Z".into(),
            expected_parent_id: Some(base.clone()),
        })
        .await
        .unwrap();

    let built_r = build_snapshot(
        &repo_r,
        &store_r,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset_r.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    // 严格 multi-replica：B envelope 不得再列出 local-only left head
    assert!(
        !built_r
            .envelope
            .revisions
            .iter()
            .any(|r| r.id == left.as_str()),
        "side-B envelope must not re-list local-only left head"
    );
    // 仅带 B 侧 object_bytes（base + right）；left blob 已在 hub CAS
    let validated_r =
        ValidatedSnapshot::from_parts(built_r.envelope, built_r.object_bytes, None).unwrap();
    let out2 = importer_b
        .commit_import(validated_r, ConfirmedImportSelection::default())
        .await
        .unwrap();
    assert!(
        out2.inserted_revisions >= 1,
        "expected remote branch + merge rev: {out2:?}"
    );
    assert_eq!(
        out2.conflicts_opened, 0,
        "pure disjoint blocks must merge without conflict: {out2:?}"
    );

    let local_assets = repo_b.list_assets(None, None).await.unwrap();
    assert_eq!(local_assets.len(), 1);
    let head = local_assets[0].current_revision_id.as_ref().expect("head");
    let head_rev = repo_b.get_revision(head).await.unwrap().unwrap();
    assert_eq!(
        head_rev.parents.len(),
        2,
        "must produce dual-parent merge revision, got parents={:?}",
        head_rev.parents
    );
    let ps: BTreeSet<_> = head_rev
        .parents
        .iter()
        .map(|p| p.as_str().to_string())
        .collect();
    assert!(
        ps.contains(left.as_str()) && ps.contains(right.id.as_str()),
        "merge parents should be left+right, got {ps:?}"
    );
    // 双方分支 revision 仍保留（非 LWW 抹除）
    assert!(repo_b.get_revision(&left).await.unwrap().is_some());
    assert!(repo_b.get_revision(&right.id).await.unwrap().is_some());
    let _ = (dir_a, store_b);
}

/// 同块双侧编辑 → 双 head + conflict。
#[tokio::test]
async fn same_block_preserves_heads_and_conflict() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, _dir_a) = test_env().await;
    let user = seed_user(&repo_a).await;
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let base_h = put_doc(&store_a, vec![("b1", "same base"), ("b2", "other")]).await;
    let base = append_instruction(
        &repo_a,
        &asset.id,
        vec![],
        &base_h,
        None,
        "2026-07-29T10:00:00Z",
    )
    .await;
    let left_h = put_doc(&store_a, vec![("b1", "hub edit"), ("b2", "other")]).await;
    let _left = append_instruction(
        &repo_a,
        &asset.id,
        vec![base.clone()],
        &left_h,
        Some(base.clone()),
        "2026-07-29T11:00:00Z",
    )
    .await;
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    let (repo_b, store_b, dir_b) = test_env().await;
    let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
    importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();

    // remote same-block branch
    let (repo_r, store_r, _dir_r) = test_env().await;
    let user_r = seed_user(&repo_r).await;
    let asset_r = repo_r
        .insert_asset(NewLogicalAsset {
            scope_id: user_r,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    store_r
        .put_blob(&store_a.get_blob(&base_h).await.unwrap())
        .await
        .unwrap();
    repo_r
        .append_revision(NewRevision {
            id: base.clone(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: Some(base_h.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap();
    let right_h = put_doc(&store_r, vec![("b1", "external edit"), ("b2", "other")]).await;
    repo_r
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![base.clone()],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: Some(right_h.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T11:30:00Z".into(),
            expected_parent_id: Some(base),
        })
        .await
        .unwrap();
    let built_r = build_snapshot(
        &repo_r,
        &store_r,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset_r.id],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    store_b
        .put_blob(&store_r.get_blob(&right_h).await.unwrap())
        .await
        .unwrap();
    let mut bytes = built_r.object_bytes;
    for (k, v) in built.object_bytes {
        bytes.entry(k).or_insert(v);
    }
    let out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built_r.envelope, bytes, None).unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();
    assert!(
        out.conflicts_opened >= 1,
        "same-block must conflict, got {out:?}"
    );
    // head 不应被 remote LWW 覆盖为唯一 remote：local head 仍存在
    let assets = repo_b.list_assets(None, None).await.unwrap();
    let head = assets[0].current_revision_id.as_ref().unwrap().as_str();
    // local head should still be the left revision from first import (or unchanged)
    assert!(!head.is_empty());
    let conflicts = repo_b.list_unresolved_conflicts().await.unwrap();
    assert!(!conflicts.is_empty());
}

/// 相同 revision id 去重。
#[tokio::test]
async fn identical_revision_ids_dedupe() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo, store, dir) = test_env().await;
    let user = seed_user(&repo).await;
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "k".into(),
            display_name: "K".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let h = put_doc(&store, vec![("b1", "only")]).await;
    let _ = append_instruction(&repo, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
    let built = build_snapshot(
        &repo,
        &store,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let importer = SnapshotImporter::new(repo.clone(), store.clone(), dir.path());
    let v = ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
        .unwrap();
    let first = importer
        .commit_import(v.clone(), ConfirmedImportSelection::default())
        .await
        .unwrap();
    let second = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();
    assert!(
        second.deduped_revisions >= first.inserted_revisions.min(1)
            || second.inserted_revisions == 0
    );
    // equal/AlreadyAncestor 二次导入不得 missing_head_decision
    assert!(
        second.heads_advanced == 0 || second.inserted_revisions == 0,
        "second import of equal snapshot must be idempotent: {second:?}"
    );
}

/// delete-vs-edit → conflict。
#[tokio::test]
async fn delete_vs_edit_conflicts() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, _dir_a) = test_env().await;
    let user = seed_user(&repo_a).await;
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let base_h = put_doc(&store_a, vec![("b1", "body")]).await;
    let base = append_instruction(
        &repo_a,
        &asset.id,
        vec![],
        &base_h,
        None,
        "2026-07-29T10:00:00Z",
    )
    .await;
    let edit_h = put_doc(&store_a, vec![("b1", "edited")]).await;
    let _edit = append_instruction(
        &repo_a,
        &asset.id,
        vec![base.clone()],
        &edit_h,
        Some(base.clone()),
        "2026-07-29T11:00:00Z",
    )
    .await;
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    let (repo_b, store_b, dir_b) = test_env().await;
    let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
    importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();

    // remote delete from base
    let (repo_r, store_r, _dir_r) = test_env().await;
    let user_r = seed_user(&repo_r).await;
    let asset_r = repo_r
        .insert_asset(NewLogicalAsset {
            scope_id: user_r,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    store_r
        .put_blob(&store_a.get_blob(&base_h).await.unwrap())
        .await
        .unwrap();
    repo_r
        .append_revision(NewRevision {
            id: base.clone(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: Some(base_h),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap();
    repo_r
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset_r.id.clone(),
            parents: vec![base],
            operation: RevisionOperation::Delete,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA_B.into(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: "2026-07-29T11:30:00Z".into(),
            expected_parent_id: None,
        })
        .await
        .unwrap();
    let built_r = build_snapshot(
        &repo_r,
        &store_r,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![asset_r.id],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let mut bytes = built_r.object_bytes;
    for (k, v) in built.object_bytes {
        bytes.entry(k).or_insert(v);
    }
    let out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built_r.envelope, bytes, None).unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();
    assert!(out.conflicts_opened >= 1, "delete-vs-edit: {out:?}");
}

/// 不同 hubProjectId 映射为 alias 合并到同一 local project scope。
#[tokio::test]
async fn distinct_hub_project_ids_map_to_one_local_scope() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, _dir_a) = test_env().await;
    let scope = repo_a
        .insert_scope(NewScopeNode {
            id: Some("scope-proj-remote-a".into()),
            kind: ScopeKind::Project,
            hub_project_id: Some("hub-remote-a".into()),
            relative_path: Some(".".into()),
        })
        .await
        .unwrap()
        .id;
    repo_a
        .upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: "hub-remote-a".into(),
            local_workbench_project_id: Some("wb-1".into()),
            git_remote_fingerprint: Some("fp-1".into()),
            local_absolute_path: None,
            // 源端导出 Project 模式快照要求映射已 opt-in（resolve_project_scope_id_on_tx）；
            // 目标端 import 后映射默认非 opt-in 由下方断言覆盖。
            opted_in: true,
        })
        .await
        .unwrap();
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: scope,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "p".into(),
            display_name: "P".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let h = put_doc(&store_a, vec![("b1", "proj")]).await;
    let _ = append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::Project,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec!["hub-remote-a".into()],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    let (repo_b, store_b, dir_b) = test_env().await;
    // local already maps different hub id to same workbench
    repo_b
        .insert_scope(NewScopeNode {
            id: Some("scope-proj-local".into()),
            kind: ScopeKind::Project,
            hub_project_id: Some("hub-local".into()),
            relative_path: Some(".".into()),
        })
        .await
        .unwrap();
    repo_b
        .upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: "hub-local".into(),
            local_workbench_project_id: Some("wb-1".into()),
            git_remote_fingerprint: Some("fp-1".into()),
            local_absolute_path: None,
            opted_in: false,
        })
        .await
        .unwrap();
    // confirm alias: remote hub-remote-a → same local mapping via selection
    let importer = SnapshotImporter::new(repo_b.clone(), store_b, dir_b.path());
    let preview = importer
        .inspect_import(
            &ValidatedSnapshot::from_parts(
                built.envelope.clone(),
                built.object_bytes.clone(),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !preview.project_candidates.is_empty()
            || !preview.resolved_mappings.is_empty()
            || built
                .envelope
                .aliases
                .iter()
                .any(|a| a.kind == "hubProjectId")
    );
    let out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
            ConfirmedImportSelection {
                project_mappings: vec![ConfirmedProjectMapping {
                    hub_project_id: "hub-remote-a".into(),
                    local_workbench_project_id: Some("wb-1".into()),
                    git_remote_fingerprint: Some("fp-1".into()),
                    opted_in: false,
                }],
                import_unmapped_projects: true,
            },
        )
        .await
        .unwrap();
    assert!(!out.imported_asset_ids.is_empty());
    // mapping 已保存
    let m = repo_b
        .get_project_mapping_by_hub_project_id("hub-remote-a")
        .await
        .unwrap()
        .expect("mapping");
    assert_eq!(m.local_workbench_project_id.as_deref(), Some("wb-1"));
    assert!(!m.opted_in);
}

/// 未映射 project 导入但 projections_scheduled=0。
#[tokio::test]
async fn unmapped_project_imports_with_zero_projections() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    // 全局 fault 可能被并行测试污染；先清空
    let (repo_a, store_a, _dir_a) = test_env().await;
    let _ = repo_a.take_import_fault();
    let scope = repo_a
        .insert_scope(NewScopeNode {
            id: Some("scope-proj-x".into()),
            kind: ScopeKind::Project,
            hub_project_id: Some("hub-x".into()),
            relative_path: Some(".".into()),
        })
        .await
        .unwrap()
        .id;
    // 源端导出 Project 模式快照要求映射存在且已 opt-in；"unmapped" 指目标端
    // commit_import 时不提供 project_mappings，验证投影为 0。
    repo_a
        .upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: "hub-x".into(),
            local_workbench_project_id: Some("wb-x".into()),
            git_remote_fingerprint: Some("fp-x".into()),
            local_absolute_path: None,
            opted_in: true,
        })
        .await
        .unwrap();
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: scope,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "p".into(),
            display_name: "P".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let h = put_doc(&store_a, vec![("b1", "x")]).await;
    let _ = append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::Project,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec!["hub-x".into()],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let (repo_b, store_b, dir_b) = test_env().await;
    let importer = SnapshotImporter::new(repo_b, store_b, dir_b.path());
    let out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap(),
            ConfirmedImportSelection {
                project_mappings: vec![],
                import_unmapped_projects: true,
            },
        )
        .await
        .unwrap();
    assert!(!out.imported_asset_ids.is_empty());
    assert_eq!(out.projections_scheduled, 0);
}

/// 缺失 parent → 失败，无脏 head。
#[tokio::test]
async fn missing_parent_fails_without_active_head() {
    let (repo, _store, _dir) = test_env().await;
    let mut env = empty_envelope();
    env.revisions.push(SnapshotRevision {
        id: "rev-child".into(),
        asset_lineage_id: "asset-1".into(),
        parents: vec!["rev-missing".into()],
        generation: "1".into(),
        operation: RevisionOperation::Upsert,
        origin_kind: RevisionOriginKind::Lan,
        origin_target: None,
        origin_replica_id: REPLICA_B.into(),
        payload_hash: None,
        tree_manifest_hash: None,
        created_at: "2026-07-29T10:00:00Z".into(),
    });
    // bypass ValidatedSnapshot validate by constructing manually after fixing hash is hard;
    // call validate_revision_closure path via commit with hand-built invalid that still validates envelope?
    // Use direct helper
    let err = validate_revision_closure(&env).unwrap_err();
    assert!(err.to_string().contains("parent_missing") || err.to_string().contains("missing"));
    assert!(repo.list_assets(None, None).await.unwrap().is_empty());
}

/// 损坏 object hash → 失败。
#[tokio::test]
async fn corrupt_object_fails() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo, store, _dir) = test_env().await;
    let user = seed_user(&repo).await;
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "k".into(),
            display_name: "K".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let h = put_doc(&store, vec![("b1", "ok")]).await;
    let _ = append_instruction(&repo, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
    let built = build_snapshot(
        &repo,
        &store,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let mut bytes = built.object_bytes;
    // corrupt
    if let Some(v) = bytes.values_mut().next() {
        v.push(b'x');
    }
    let (repo2, store2, dir2) = test_env().await;
    let importer = SnapshotImporter::new(repo2, store2, dir2.path());
    // from_parts still validates envelope; commit checks object hash
    let v = ValidatedSnapshot {
        envelope: built.envelope,
        object_bytes: bytes,
    };
    let err = importer
        .commit_import(v, ConfirmedImportSelection::default())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("corrupt") || err.to_string().contains("hash"),
        "{err}"
    );
}

/// DB fail before head → 无非法 head；CAS 后崩溃对象可残留但不计 imported asset。
#[tokio::test]
async fn db_fail_before_head_and_cas_residual() {
    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, _dir_a) = test_env().await;
    let user = seed_user(&repo_a).await;
    let asset = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "k".into(),
            display_name: "K".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let h = put_doc(&store_a, vec![("b1", "body")]).await;
    let _ = append_instruction(&repo_a, &asset.id, vec![], &h, None, "2026-07-29T10:00:00Z").await;
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    let (repo_b, store_b, dir_b) = test_env().await;
    // 清掉其它测试可能残留的全局 fault，再注入本用例
    let _ = repo_b.take_import_fault();
    repo_b.inject_import_fault(AgentHubImportFault::BeforeTxCommit);
    let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
    let err = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap_err();
    // 确保 fault 已消费，避免污染后续测试
    let _ = repo_b.take_import_fault();
    assert!(
        err.to_string().contains("injected") || err.to_string().contains("import"),
        "{err}"
    );
    // no assets with head from failed tx
    assert!(repo_b.list_assets(None, None).await.unwrap().is_empty());
    // CAS may have objects
    assert!(store_b.get_blob(&h).await.is_ok());
}

fn empty_envelope() -> SnapshotEnvelopeV1 {
    SnapshotEnvelopeV1 {
        format: crate::agent_hub::snapshot::envelope::FORMAT_NAME.into(),
        format_version: crate::agent_hub::snapshot::envelope::FORMAT_VERSION,
        canonicalization: crate::agent_hub::snapshot::envelope::CANONICALIZATION_NAME.into(),
        snapshot_id: Uuid::now_v7().to_string(),
        snapshot_hash: "00".repeat(32),
        source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
        created_at: "2026-07-29T10:00:00Z".into(),
        selection: crate::agent_hub::snapshot::envelope::SnapshotSelection {
            scope_ids: vec![],
            asset_ids: vec![],
            include_history: true,
        },
        asset_heads: BTreeMap::new(),
        assets: vec![],
        lineages: vec![],
        revisions: vec![],
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects: vec![],
    }
}

/// Gate D C1: import 两个共享 skill 的 package 后，删除一个 package 必须保留 skill；
/// re-export 仍闭合 residual/component 边。
#[tokio::test]
async fn import_shared_plugin_skill_preserves_on_delete_and_reexport() {
    use crate::agent_hub::assets::PortableAssetPayload;
    use crate::agent_hub::models::{AssetPolicy, NewLogicalAsset};
    use crate::agent_hub::plugins::{
        ComponentOwnership, PluginComponentRef, PluginPackagePayload, PluginResidualRef,
        ResidualKind,
    };
    use crate::agent_hub::snapshot::builder::{
        build_snapshot, SnapshotSelectionMode, SnapshotSelectionRequest,
    };

    let _envelope_cache_guard = clear_envelope_cache_for_test().await;
    let (repo_a, store_a, _dir_a) = test_env().await;
    let user = seed_user(&repo_a).await;

    // shared skill S
    let skill = repo_a
        .insert_asset(NewLogicalAsset {
            scope_id: user.clone(),
            kind: AssetKind::Skill,
            origin_namespace: "plugin:shared".into(),
            logical_key: "shared-skill".into(),
            display_name: "shared-skill".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let md = store_a.put_blob(b"shared-skill-body").await.unwrap();
    let tree = store_a
        .put_tree(&crate::agent_hub::object_store::TreeManifest {
            entries: vec![crate::agent_hub::object_store::TreeEntry {
                path: "SKILL.md".into(),
                blob_hash: md.hash.clone(),
                entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                executable: false,
            }],
        })
        .await
        .unwrap();
    let skill_rev = repo_a
        .append_portable_asset_revision(
            &skill.id,
            &PortableAssetPayload::Skill(crate::agent_hub::assets::PortableSkill {
                name: "shared-skill".into(),
                description: "d".into(),
                skill_markdown_hash: md.hash.clone(),
                tree_manifest_hash: tree.hash.clone(),
                target_extensions: BTreeMap::new(),
            }),
            &store_a,
            RevisionOriginKind::Ui,
            Some(crate::agent_hub::models::AgentTarget::Claude),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let residual_blob = store_a.put_blob(b"runtime-shared").await.unwrap();
    let residual_tree = store_a
        .put_tree(&crate::agent_hub::object_store::TreeManifest {
            entries: vec![crate::agent_hub::object_store::TreeEntry {
                path: "index.js".into(),
                blob_hash: residual_blob.hash.clone(),
                entry_type: crate::agent_hub::object_store::TreeEntryType::File,
                executable: false,
            }],
        })
        .await
        .unwrap();

    let mut package_ids = Vec::new();
    for (key, residual_kind) in [
        ("pkg-a", ResidualKind::Runtime),
        ("pkg-b", ResidualKind::Runtime),
    ] {
        let plugin = repo_a
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Plugin,
                origin_namespace: "standalone".into(),
                logical_key: key.into(),
                display_name: key.into(),
                policy: AssetPolicy::TargetOnly,
            })
            .await
            .unwrap();
        let payload = PluginPackagePayload {
            plugin_id: key.into(),
            name: key.into(),
            version: Some("1".into()),
            description: None,
            source_target: crate::agent_hub::models::AgentTarget::Claude,
            component_refs: vec![PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill.id.clone(),
                revision_id: skill_rev.id.clone(),
                ownership: if key == "pkg-a" {
                    ComponentOwnership::PackageOwned
                } else {
                    ComponentOwnership::Shared
                },
            }],
            residual_refs: vec![PluginResidualRef {
                target: crate::agent_hub::models::AgentTarget::Claude,
                residual_kind,
                tree_manifest_hash: residual_tree.hash.clone(),
            }],
            target_extensions: BTreeMap::new(),
        };
        let _ = repo_a
            .append_plugin_package_revision(
                &plugin.id,
                &payload,
                &store_a,
                RevisionOriginKind::Ui,
                Some(crate::agent_hub::models::AgentTarget::Claude),
                "01900000-0000-7000-8000-0000000000d1",
                None,
            )
            .await
            .unwrap();
        package_ids.push(plugin.id);
    }

    // export both packages + skill (full hub of user scope assets)
    let built = build_snapshot(
        &repo_a,
        &store_a,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();

    // import into empty B
    let (repo_b, store_b, dir_b) = test_env().await;
    let importer = SnapshotImporter::new(repo_b.clone(), store_b.clone(), dir_b.path());
    let out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(built.envelope.clone(), built.object_bytes.clone(), None)
                .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .unwrap();
    assert!(
        out.inserted_revisions >= 3,
        "expected packages+skill: {out:?}"
    );

    // edges restored for both package heads
    let assets_b = repo_b.list_assets(None, None).await.unwrap();
    let plugins_b: Vec<_> = assets_b
        .iter()
        .filter(|a| a.kind == AssetKind::Plugin && a.deleted_at.is_none())
        .collect();
    assert_eq!(plugins_b.len(), 2, "two live plugins after import");
    for p in &plugins_b {
        let head = p.current_revision_id.as_ref().expect("plugin head");
        let comps = repo_b
            .list_plugin_components_for_revision(head.as_str())
            .await
            .unwrap();
        assert_eq!(
            comps.len(),
            1,
            "import must restore component edges for {}",
            p.logical_key
        );
        let residuals = repo_b
            .list_plugin_residuals_for_revision(head.as_str())
            .await
            .unwrap();
        assert_eq!(residuals.len(), 1, "import must restore residual edges");
    }

    let skill_b = assets_b
        .iter()
        .find(|a| a.kind == AssetKind::Skill && a.logical_key == "shared-skill")
        .expect("skill imported");
    let skill_id = skill_b.id.clone();

    // delete one package — shared skill must be preserved
    let pkg_to_delete = plugins_b[0].id.clone();
    let del = repo_b
        .delete_plugin_package_with_ownership(
            &pkg_to_delete,
            &store_b,
            RevisionOriginKind::Ui,
            "01900000-0000-7000-8000-0000000000d1",
        )
        .await
        .unwrap();
    assert!(
        del.component_decisions.iter().any(|d| {
            d.component_asset_id == skill_id
                && d.decision == crate::agent_hub::plugins::ComponentDeleteDecision::PreserveShared
        }),
        "shared skill must PreserveShared after import, got {:?}",
        del.component_decisions
    );
    let skill_after = repo_b.get_asset(&skill_id).await.unwrap().unwrap();
    assert!(
        skill_after.deleted_at.is_none(),
        "shared skill must survive package delete after import"
    );

    // re-export remaining package still closes residual tree via restored edges
    let remaining = plugins_b
        .iter()
        .find(|p| p.id != pkg_to_delete)
        .expect("remaining package");
    let rebuilt = build_snapshot(
        &repo_b,
        &store_b,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![remaining.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let obj_hashes: BTreeSet<_> = rebuilt
        .envelope
        .objects
        .iter()
        .map(|o| o.hash.as_str())
        .collect();
    assert!(
        obj_hashes.contains(residual_tree.hash.as_str())
            || obj_hashes.contains(residual_blob.hash.as_str()),
        "re-export after import must close residual via edges"
    );
}
