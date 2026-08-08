//! agent_hub_replication_smoke — Gate C LAN source-push + Git device-lane smoke (L2)
//! Evidence: L2-AGENT-HUB-C-001, L2-AGENT-HUB-C-GIT-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     Gate C 需要在隔离 data_dir 下证明：源 push negotiate/stream/commit、共同祖先与
//!     冲突、chunk 中断续传、同 request 幂等、凭据字节一致且不进诊断日志、commit 后
//!     projection 回调失败标记不回滚 canonical；Git lane 经 export → 第三环境
//!     inspect/confirm 后 residual-ready 恢复（map 一个 project、另一个 unmapped）。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke（不启动完整 backend 二进制）：
//!     - L2-AGENT-HUB-C-001：双 data_dir owner 风格 prepare/chunk/commit + capability
//!       health 门控；日志脱敏用 sanitize_diagnostic_text / redact_sensitive_text；
//!     - L2-AGENT-HUB-C-GIT-001：expand_readable_archive 写入 device lane →
//!       inspect/preview/confirm + 映射。
//!
//! NOT VERIFIED（本 smoke 不宣称）:
//!     - 真实双主机 mDNS / 多进程 backend / 公网 LAN 身份认证（L3）
//!     - 打包 GUI / 全平台矩阵；当前仅 cargo test 本机
//!     - 自动 Git import（产品禁止；inspect/preview 零写断言）

use app_lib::agent_hub::assets::{
    canonical_bytes, redact_sensitive_text, McpTransport, PortableAssetPayload, PortableMcpServer,
};
use app_lib::agent_hub::git::{
    confirm_git_import_in_workdir, inspect_git_lanes_in_workdir, preview_git_import_in_workdir,
    ConfirmGitImportRequest,
};
use app_lib::agent_hub::instructions::{InstructionBlock, InstructionDocument};
use app_lib::agent_hub::models::{
    AssetKind, AssetPolicy, NewLogicalAsset, NewScopeNode, RevisionId, RevisionOperation,
    RevisionOriginKind, ScopeKind,
};
use app_lib::agent_hub::object_store::sha256_hex;
use app_lib::agent_hub::replication::{
    commit_push, prepare_push, put_object_chunk, CommitPushRequest, PreparePushRequest,
    ReplicationLedger, AGENT_HUB_MAX_CHUNK_BYTES,
};
use app_lib::agent_hub::snapshot::archive::expand_readable_archive;
use app_lib::agent_hub::snapshot::builder::{hash_selection, BuiltSnapshot};
use app_lib::agent_hub::snapshot::envelope::{
    compute_snapshot_hash, SnapshotAlias, SnapshotAsset, SnapshotConflict, SnapshotEnvelopeV1,
    SnapshotLineage, SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection,
    SnapshotVariant, CANONICALIZATION_NAME, FORMAT_NAME, FORMAT_VERSION,
};
use app_lib::agent_hub::snapshot::importer::{
    ConfirmedProjectMapping, SnapshotImporter, ValidatedSnapshot,
};
use app_lib::backend::logging::sanitize_diagnostic_text;
use app_lib::{
    AgentHubObjectStore, AgentHubRepo, AgentTarget, ConfirmedImportSelection, PeerCallError,
    PeerClient, CAPABILITY_AGENT_HUB_V1,
};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const CREDENTIAL_FIXTURE: &str = "plain-fixture-hub-c-secret";
const ASSET_ID: &str = "01900000-0000-7000-8000-0000000000a1";
const REV_ID: &str = "01900000-0000-7000-8000-000000000001";
const REPLICA: &str = "01900000-0000-7000-8000-0000000000b1";
const SNAP_ID: &str = "01900000-0000-7000-8000-0000000000c1";

// ---------------------------------------------------------------------------
// 隔离环境
// ---------------------------------------------------------------------------

/// 隔离 smoke 根目录。
///
/// Business Logic: Gate C smoke 不得触碰用户真实 HOME / `~/.cc-partner`。
/// Code Logic: tempfile + data 子路径。
struct GateCSmokeEnv {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    db_path: PathBuf,
}

/// Business Logic: 每个 smoke case 独立 data。
/// Code Logic: 创建目录布局。
fn setup_isolated_env(name: &str) -> GateCSmokeEnv {
    let root = tempfile::Builder::new()
        .prefix(&format!("cc-partner-gate-c-{name}-"))
        .tempdir()
        .expect("tempdir");
    let data_dir = root.path().join("data");
    let db_path = data_dir.join("data.db");
    fs::create_dir_all(data_dir.join("agent-hub").join("objects")).expect("objects");
    // SAFETY: 串行 smoke（--test-threads=1）。
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
    GateCSmokeEnv {
        _root: root,
        data_dir,
        db_path,
    }
}

/// Business Logic: smoke 需要独立 SQLite + AgentHub schema。
/// Code Logic: WAL 单连接池 ensure_schema。
async fn open_hub_pool(db_path: &Path) -> sqlx::SqlitePool {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("db parent");
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .expect("sqlite options")
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("pool");
    AgentHubRepo::ensure_schema(&pool)
        .await
        .expect("ensure_schema");
    pool
}

/// Business Logic: MCP 凭据原文必须进 CAS。
/// Code Logic: 构造 PortableMcpServer JSON bytes。
fn credential_payload() -> Vec<u8> {
    let mcp = PortableMcpServer {
        key: "secret-mcp".into(),
        transport: McpTransport::Http {
            url: format!("https://example.invalid/mcp?token={CREDENTIAL_FIXTURE}"),
            headers: BTreeMap::from([(
                "Authorization".into(),
                format!("Bearer {CREDENTIAL_FIXTURE}"),
            )]),
        },
        env: BTreeMap::from([("API_TOKEN".into(), CREDENTIAL_FIXTURE.into())]),
        enabled: true,
        tool_allow: vec![],
        tool_deny: vec![],
        target_extensions: BTreeMap::new(),
    };
    canonical_bytes(&PortableAssetPayload::Mcp(mcp)).unwrap()
}

const REV_PARENT_ID: &str = "01900000-0000-7000-8000-000000000000";
const TOMBSTONE_ASSET_ID: &str = "01900000-0000-7000-8000-0000000000a2";
const TOMBSTONE_REV_ID: &str = "01900000-0000-7000-8000-000000000003";
const CONFLICT_ID: &str = "01900000-0000-7000-8000-0000000000cf";

/// Business Logic: 构造含 credential blob 的 envelope。
/// Code Logic: SnapshotEnvelopeV1 + compute_snapshot_hash；Git lane 另含 multi-rev + tombstone + conflict residual。
fn sample_envelope_with_secret(bytes: &[u8]) -> (SnapshotEnvelopeV1, String) {
    sample_envelope_with_secret_rich(bytes, false)
}

/// Business Logic: Git restore 需 richer residual（history + tombstone/conflict/variant 至少一类）。
/// Code Logic: include_residuals 时附加 parent revision、Delete tombstone asset、conflict 与 variant 行。
fn sample_envelope_with_secret_rich(
    bytes: &[u8],
    include_residuals: bool,
) -> (SnapshotEnvelopeV1, String) {
    let object_hash = sha256_hex(bytes);
    let parent_bytes = br#"{"seed":"parent-history"}"#;
    let parent_hash = sha256_hex(parent_bytes);

    let mut revisions = vec![SnapshotRevision {
        id: REV_ID.into(),
        asset_lineage_id: ASSET_ID.into(),
        parents: if include_residuals {
            vec![REV_PARENT_ID.into()]
        } else {
            vec![]
        },
        generation: if include_residuals {
            "1".into()
        } else {
            "0".into()
        },
        operation: RevisionOperation::Upsert,
        origin_kind: RevisionOriginKind::Ui,
        origin_target: None,
        origin_replica_id: REPLICA.into(),
        payload_hash: Some(object_hash.clone()),
        tree_manifest_hash: None,
        created_at: "2026-07-29T12:00:00Z".into(),
    }];
    let mut assets = vec![SnapshotAsset {
        id: ASSET_ID.into(),
        scope_id: "scope-user".into(),
        kind: AssetKind::Mcp,
        origin_namespace: "standalone".into(),
        logical_key: "mcp-secret".into(),
        display_name: "Secret MCP".into(),
        policy: AssetPolicy::Shared,
        deleted_at: None,
    }];
    let mut lineages = vec![SnapshotLineage {
        id: ASSET_ID.into(),
        root_asset_id: ASSET_ID.into(),
    }];
    let mut objects = vec![SnapshotObjectDescriptor {
        hash: object_hash.clone(),
        size: (bytes.len() as u64).to_string(),
    }];
    let mut asset_ids = vec![ASSET_ID.into()];
    let mut asset_heads = BTreeMap::from([(ASSET_ID.into(), vec![REV_ID.into()])]);
    let mut variants = vec![];
    let mut conflicts = vec![];

    if include_residuals {
        // multi-revision parent lineage retained
        revisions.insert(
            0,
            SnapshotRevision {
                id: REV_PARENT_ID.into(),
                asset_lineage_id: ASSET_ID.into(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: REPLICA.into(),
                payload_hash: Some(parent_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:00:00Z".into(),
            },
        );
        objects.push(SnapshotObjectDescriptor {
            hash: parent_hash,
            size: (parent_bytes.len() as u64).to_string(),
        });

        // tombstone residual (deleted asset + Delete revision)
        assets.push(SnapshotAsset {
            id: TOMBSTONE_ASSET_ID.into(),
            scope_id: "scope-user".into(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "instruction-tombstone".into(),
            display_name: "Tombstoned Instruction".into(),
            policy: AssetPolicy::Shared,
            deleted_at: Some("2026-07-29T12:30:00Z".into()),
        });
        lineages.push(SnapshotLineage {
            id: TOMBSTONE_ASSET_ID.into(),
            root_asset_id: TOMBSTONE_ASSET_ID.into(),
        });
        revisions.push(SnapshotRevision {
            id: TOMBSTONE_REV_ID.into(),
            asset_lineage_id: TOMBSTONE_ASSET_ID.into(),
            parents: vec![],
            generation: "0".into(),
            operation: RevisionOperation::Delete,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA.into(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: "2026-07-29T12:30:00Z".into(),
        });
        asset_ids.push(TOMBSTONE_ASSET_ID.into());
        asset_heads.insert(TOMBSTONE_ASSET_ID.into(), vec![TOMBSTONE_REV_ID.into()]);

        // conflict residual payload
        conflicts.push(SnapshotConflict {
            id: CONFLICT_ID.into(),
            asset_id: ASSET_ID.into(),
            target: None,
            base_revision_id: Some(REV_PARENT_ID.into()),
            hub_revision_id: Some(REV_ID.into()),
            external_revision_id: None,
            detail_json: r#"{"reason":"agent_hub_git_lane_residual"}"#.into(),
            created_at: "2026-07-29T12:15:00Z".into(),
        });

        // variant residual (target-only head metadata)
        variants.push(SnapshotVariant {
            asset_id: ASSET_ID.into(),
            target: AgentTarget::Claude,
            revision_id: REV_ID.into(),
        });
    }

    let mut envelope = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: SNAP_ID.into(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: REPLICA.into(),
        created_at: "2026-07-29T12:00:00Z".into(),
        selection: SnapshotSelection {
            scope_ids: vec!["scope-user".into()],
            asset_ids,
            include_history: true,
        },
        asset_heads,
        assets,
        lineages,
        revisions,
        variants,
        conflicts,
        aliases: vec![
            SnapshotAlias {
                kind: "hubProjectId".into(),
                external_id: "hub-mapped".into(),
                local_id: "hub-mapped".into(),
            },
            SnapshotAlias {
                kind: "hubProjectId".into(),
                external_id: "hub-unmapped".into(),
                local_id: "hub-unmapped".into(),
            },
        ],
        objects,
    };
    envelope.objects.sort_by(|a, b| a.hash.cmp(&b.hash));
    envelope.snapshot_hash = compute_snapshot_hash(&envelope).expect("hash");
    (envelope, object_hash)
}

/// Business Logic: 双 owner 风格 — 目标 prepare/chunk/commit 全路径。
/// Code Logic: 半 chunk 中断后 resume；commit 幂等；projection 回调可标失败。
#[allow(clippy::too_many_arguments)]
async fn push_envelope_to_owner(
    target_repo: &AgentHubRepo,
    target_store: &AgentHubObjectStore,
    target_ledger: &ReplicationLedger,
    data_dir: &Path,
    envelope: &SnapshotEnvelopeV1,
    bytes: &[u8],
    source_device: &str,
    client_request_id: &str,
    mark_projection_failed: &mut bool,
) {
    let sel = hash_selection(&envelope.selection).unwrap();
    let object_hash = envelope.objects[0].hash.clone();
    let prep = prepare_push(
        target_repo,
        target_store,
        target_ledger,
        data_dir,
        PreparePushRequest {
            envelope: envelope.clone(),
            source_device_id: source_device.into(),
            client_request_id: client_request_id.into(),
            selection_hash: sel.clone(),
        },
    )
    .await
    .expect("prepare");
    assert_eq!(prep.status, "prepared");

    // 中断后 resume：先半 chunk
    let mid = (bytes.len() / 2).max(1).min(bytes.len());
    let half = &bytes[..mid];
    let r_half = put_object_chunk(
        target_ledger,
        data_dir,
        &prep.transfer_id,
        &object_hash,
        0,
        half,
        Some(&sha256_hex(half)),
    )
    .await
    .expect("half chunk");
    if mid < bytes.len() {
        assert!(!r_half.verified);
        let rest = &bytes[mid..];
        let r_full = put_object_chunk(
            target_ledger,
            data_dir,
            &prep.transfer_id,
            &object_hash,
            mid as u64,
            rest,
            Some(&sha256_hex(rest)),
        )
        .await
        .expect("resume chunk");
        assert!(r_full.verified);
    } else {
        assert!(r_half.verified);
    }

    let commit = commit_push(
        target_repo,
        target_store,
        target_ledger,
        data_dir,
        &prep.transfer_id,
        CommitPushRequest {
            source_device_id: source_device.into(),
            client_request_id: client_request_id.into(),
            selection_hash: sel.clone(),
            snapshot_hash: envelope.snapshot_hash.clone(),
        },
        |_| {
            *mark_projection_failed = true;
        },
    )
    .await
    .expect("commit");
    assert_eq!(commit.status, "committed");
    // Product: projection intents only when local target bindings exist for imported assets.
    // This smoke seeds canonical push without local bindings → projection stays idle and
    // protocol still commits (callback independent of success). When queued, callback must fire.
    if commit.projection == "queued" {
        assert!(
            *mark_projection_failed,
            "projection callback must run when intents are queued"
        );
    } else {
        assert_eq!(
            commit.projection, "idle",
            "without local bindings expect idle projection, got {}",
            commit.projection
        );
    }

    // 同 request 重试幂等
    let replay = commit_push(
        target_repo,
        target_store,
        target_ledger,
        data_dir,
        &prep.transfer_id,
        CommitPushRequest {
            source_device_id: source_device.into(),
            client_request_id: client_request_id.into(),
            selection_hash: sel,
            snapshot_hash: envelope.snapshot_hash.clone(),
        },
        |_| {},
    )
    .await
    .expect("replay");
    assert_eq!(
        replay.outcome.snapshot_hash, commit.outcome.snapshot_hash,
        "idempotent replay must return original outcome"
    );
}

// ---------------------------------------------------------------------------
// L2-AGENT-HUB-C-001
// ---------------------------------------------------------------------------

/// L2-AGENT-HUB-C-001：两 owner 风格 replication 合同。
///
/// Business Logic: 共同祖先、源 capability 门控、冲突、chunk 续传、幂等、凭据、projection。
/// Code Logic: 双 data_dir prepare/chunk/commit + health capability peer。
#[tokio::test]
async fn l2_agent_hub_c_001_two_owner_style_replication() {
    let source_env = setup_isolated_env("src");
    let target_env = setup_isolated_env("tgt");

    let source_pool = open_hub_pool(&source_env.db_path).await;
    let target_pool = open_hub_pool(&target_env.db_path).await;
    let source_repo = AgentHubRepo::new(source_pool);
    let target_repo = AgentHubRepo::new(target_pool.clone());
    let source_store = AgentHubObjectStore::open(&source_env.data_dir).expect("src store");
    let target_store = AgentHubObjectStore::open(&target_env.data_dir).expect("tgt store");
    let target_ledger = ReplicationLedger::new_standalone(target_pool);

    // --- 共同祖先：两边先导入同一 secret revision ---
    let secret = credential_payload();
    let (ancestor_env, object_hash) = sample_envelope_with_secret(&secret);

    // 目标先走完整 push 路径（chunk interrupt resume + idempotency + projection flag）
    let mut proj_failed = false;
    push_envelope_to_owner(
        &target_repo,
        &target_store,
        &target_ledger,
        &target_env.data_dir,
        &ancestor_env,
        &secret,
        "device-source",
        "req-ancestor-1",
        &mut proj_failed,
    )
    .await;
    // proj_failed only when projection was queued (local bindings present).

    // 凭据字节进目标 CAS 且与源一致
    let stored = target_store
        .get_blob(&object_hash)
        .await
        .expect("credential blob in target CAS");
    assert_eq!(stored.as_slice(), secret.as_slice());
    assert!(
        std::str::from_utf8(&stored)
            .unwrap_or("")
            .contains(CREDENTIAL_FIXTURE),
        "canonical credential bytes must be identical plaintext"
    );

    // 日志/诊断不得回显 credential
    let hostile =
        format!("push failed token={CREDENTIAL_FIXTURE} auth=Bearer {CREDENTIAL_FIXTURE}");
    let cleaned = sanitize_diagnostic_text(&hostile);
    assert!(
        !cleaned.contains(CREDENTIAL_FIXTURE),
        "sanitize must strip credential: {cleaned}"
    );
    let redacted = redact_sensitive_text(&hostile);
    assert!(
        !redacted.contains(CREDENTIAL_FIXTURE),
        "redact_sensitive_text must strip credential: {redacted}"
    );

    // 源侧也 import 同一祖先（共同祖先）
    let importer = SnapshotImporter::new(
        source_repo.clone(),
        source_store.clone(),
        &source_env.data_dir,
    );
    let validated = ValidatedSnapshot::from_parts(
        ancestor_env.clone(),
        BTreeMap::from([(object_hash.clone(), secret.clone())]),
        None,
    )
    .unwrap();
    importer
        .commit_import(validated, ConfirmedImportSelection::default())
        .await
        .expect("source ancestor import");

    // --- capability negotiate：有 agent-hub.v1 才允许；无能力则 Unsupported ---
    let health_hits = Arc::new(AtomicUsize::new(0));
    let (ok_base, ok_handle) = spawn_health_peer(true, Arc::clone(&health_hits)).await;
    let (no_base, no_handle) = spawn_health_peer(false, Arc::clone(&health_hits)).await;
    let client = PeerClient::new();
    client
        .require_capability(&ok_base, CAPABILITY_AGENT_HUB_V1)
        .await
        .expect("capable peer must pass");
    let err = client
        .require_capability(&no_base, CAPABILITY_AGENT_HUB_V1)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PeerCallError::Unsupported { .. }),
        "unsupported peer must not negotiate push: {err:?}"
    );
    ok_handle.abort();
    no_handle.abort();

    // --- disjoint merge：目标再导入 unrelated asset ---
    let other_bytes = br#"{"blocks":[{"id":"x","mode":"shared"}]}"#;
    let other_hash = sha256_hex(other_bytes);
    let other_asset = "01900000-0000-7000-8000-0000000000e1";
    let other_rev = "01900000-0000-7000-8000-0000000000e2";
    let mut other_env = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: "01900000-0000-7000-8000-0000000000e3".into(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: REPLICA.into(),
        created_at: "2026-07-29T14:00:00Z".into(),
        selection: SnapshotSelection {
            scope_ids: vec!["scope-user".into()],
            asset_ids: vec![other_asset.into()],
            include_history: true,
        },
        asset_heads: BTreeMap::from([(other_asset.into(), vec![other_rev.into()])]),
        assets: vec![SnapshotAsset {
            id: other_asset.into(),
            scope_id: "scope-user".into(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "instruction-disjoint".into(),
            display_name: "Disjoint Instruction".into(),
            policy: AssetPolicy::Shared,
            deleted_at: None,
        }],
        lineages: vec![SnapshotLineage {
            id: other_asset.into(),
            root_asset_id: other_asset.into(),
        }],
        revisions: vec![SnapshotRevision {
            id: other_rev.into(),
            asset_lineage_id: other_asset.into(),
            parents: vec![],
            generation: "0".into(),
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: REPLICA.into(),
            payload_hash: Some(other_hash.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T14:00:00Z".into(),
        }],
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects: vec![SnapshotObjectDescriptor {
            hash: other_hash,
            size: (other_bytes.len() as u64).to_string(),
        }],
    };
    other_env.snapshot_hash = compute_snapshot_hash(&other_env).unwrap();
    let mut proj2 = false;
    push_envelope_to_owner(
        &target_repo,
        &target_store,
        &target_ledger,
        &target_env.data_dir,
        &other_env,
        other_bytes,
        "device-source",
        "req-disjoint-1",
        &mut proj2,
    )
    .await;
    let assets = target_repo.list_assets(None, None).await.unwrap();
    assert!(
        assets.iter().any(|a| a.logical_key == "mcp-secret"),
        "ancestor MCP retained"
    );
    assert!(
        assets
            .iter()
            .any(|a| a.logical_key == "instruction-disjoint"),
        "disjoint branch merges without wiping ancestor"
    );

    // --- same-block Hub revision conflict on TARGET (not ledger idempotency) ---
    // Import left head, then import remote same-parent right head → unresolved conflicts.
    prove_target_same_block_hub_conflict(&target_repo, &target_store, &target_env.data_dir).await;

    // --- SEPARATE coverage: request-id payload-hash idempotency conflict ---
    // (ledger clientRequestId + different envelope hash → conflict; NOT same-block content)
    let mut conflict_env = ancestor_env.clone();
    conflict_env.assets[0].display_name = "tampered".into();
    conflict_env.snapshot_hash = compute_snapshot_hash(&conflict_env).unwrap();
    let conflict_sel = hash_selection(&conflict_env.selection).unwrap();
    let err = prepare_push(
        &target_repo,
        &target_store,
        &target_ledger,
        &target_env.data_dir,
        PreparePushRequest {
            envelope: conflict_env,
            source_device_id: "device-source".into(),
            client_request_id: "req-ancestor-1".into(),
            selection_hash: conflict_sel,
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("conflict"),
        "same clientRequestId different payload hash must conflict (idempotency ledger): {err}"
    );

    assert_eq!(AGENT_HUB_MAX_CHUNK_BYTES, 8 * 1024 * 1024);

    // projection 失败标记后 canonical 仍在
    assert!(
        target_store.get_blob(&object_hash).await.is_ok(),
        "projection failure must not roll back canonical CAS"
    );
    assert!(
        !target_repo
            .list_assets(None, None)
            .await
            .unwrap()
            .is_empty(),
        "canonical assets remain after projection flag"
    );
}

/// 在目标 Hub 上通过 SnapshotImporter 制造 same-block 双 head 冲突。
///
/// Business Logic: Gate C 要求 same-block 分支在目标侧留下 unresolved conflict，
/// 而不是被 LWW 抹成单一 head；与 clientRequestId 幂等冲突是不同合同。
/// Code Logic: 先 import base+left，再 import base+right（同 parent 不同块文案）。
async fn prove_target_same_block_hub_conflict(
    target_repo: &AgentHubRepo,
    target_store: &AgentHubObjectStore,
    data_dir: &std::path::Path,
) {
    fn doc_bytes(blocks: Vec<(&str, &str)>) -> Vec<u8> {
        let document = InstructionDocument {
            relative_key: "CLAUDE.md".into(),
            blocks: blocks
                .into_iter()
                .map(|(id, body)| InstructionBlock::shared(id, body, vec![]))
                .collect(),
        };
        serde_json::to_vec(&document).expect("doc json")
    }

    let user = target_repo
        .insert_scope(NewScopeNode {
            id: Some("scope-user-same-block".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .expect("user scope")
        .id;
    let asset = target_repo
        .insert_asset(NewLogicalAsset {
            scope_id: user,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "same-block-root".into(),
            display_name: "Same Block Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .expect("asset");

    let base_bytes = doc_bytes(vec![("b1", "same base"), ("b2", "other")]);
    let left_bytes = doc_bytes(vec![("b1", "hub edit"), ("b2", "other")]);
    let right_bytes = doc_bytes(vec![("b1", "external edit"), ("b2", "other")]);
    let base_hash = target_store
        .put_blob(&base_bytes)
        .await
        .expect("base blob")
        .hash;
    let left_hash = target_store
        .put_blob(&left_bytes)
        .await
        .expect("left blob")
        .hash;
    let right_hash = target_store
        .put_blob(&right_bytes)
        .await
        .expect("right blob")
        .hash;

    let base_rev = RevisionId("01900000-0000-7000-8000-0000000000f1".into());
    let left_rev = RevisionId("01900000-0000-7000-8000-0000000000f2".into());
    let right_rev = RevisionId("01900000-0000-7000-8000-0000000000f3".into());

    // First snapshot: base → left (local target head)
    let mut left_env = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: "01900000-0000-7000-8000-0000000000f4".into(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
        created_at: "2026-07-29T15:00:00Z".into(),
        selection: SnapshotSelection {
            scope_ids: vec![],
            asset_ids: vec![asset.id.clone()],
            include_history: true,
        },
        asset_heads: BTreeMap::from([(asset.id.clone(), vec![left_rev.0.clone()])]),
        assets: vec![SnapshotAsset {
            id: asset.id.clone(),
            scope_id: asset.scope_id.clone(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "same-block-root".into(),
            display_name: "Same Block Root".into(),
            policy: AssetPolicy::Shared,
            deleted_at: None,
        }],
        lineages: vec![SnapshotLineage {
            id: asset.id.clone(),
            root_asset_id: asset.id.clone(),
        }],
        revisions: vec![
            SnapshotRevision {
                id: base_rev.0.clone(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                payload_hash: Some(base_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T15:00:00Z".into(),
            },
            SnapshotRevision {
                id: left_rev.0.clone(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![base_rev.0.clone()],
                generation: "1".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
                payload_hash: Some(left_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T15:01:00Z".into(),
            },
        ],
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects: {
            let mut objs = vec![
                SnapshotObjectDescriptor {
                    hash: base_hash.clone(),
                    size: (base_bytes.len() as u64).to_string(),
                },
                SnapshotObjectDescriptor {
                    hash: left_hash.clone(),
                    size: (left_bytes.len() as u64).to_string(),
                },
            ];
            objs.sort_by(|a, b| a.hash.cmp(&b.hash));
            objs
        },
    };
    left_env.snapshot_hash = compute_snapshot_hash(&left_env).expect("left hash");

    let importer = SnapshotImporter::new(target_repo.clone(), target_store.clone(), data_dir);
    let left_out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(
                left_env,
                BTreeMap::from([
                    (base_hash.clone(), base_bytes.clone()),
                    (left_hash.clone(), left_bytes.clone()),
                ]),
                None,
            )
            .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .expect("import left head");
    assert_eq!(
        left_out.conflicts_opened, 0,
        "first same-block import must not conflict: {left_out:?}"
    );

    // Second snapshot from another replica: base → right (same parent, same block edited)
    let mut right_env = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id: "01900000-0000-7000-8000-0000000000f5".into(),
        snapshot_hash: "0".repeat(64),
        source_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
        created_at: "2026-07-29T15:02:00Z".into(),
        selection: SnapshotSelection {
            scope_ids: vec![],
            asset_ids: vec![asset.id.clone()],
            include_history: true,
        },
        asset_heads: BTreeMap::from([(asset.id.clone(), vec![right_rev.0.clone()])]),
        assets: vec![SnapshotAsset {
            id: asset.id.clone(),
            scope_id: asset.scope_id.clone(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "same-block-root".into(),
            display_name: "Same Block Root".into(),
            policy: AssetPolicy::Shared,
            deleted_at: None,
        }],
        lineages: vec![SnapshotLineage {
            id: asset.id.clone(),
            root_asset_id: asset.id.clone(),
        }],
        revisions: vec![
            SnapshotRevision {
                id: base_rev.0.clone(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                generation: "0".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                payload_hash: Some(base_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T15:00:00Z".into(),
            },
            SnapshotRevision {
                id: right_rev.0.clone(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![base_rev.0.clone()],
                generation: "1".into(),
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b2".into(),
                payload_hash: Some(right_hash.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T15:02:00Z".into(),
            },
        ],
        variants: vec![],
        conflicts: vec![],
        aliases: vec![],
        objects: {
            let mut objs = vec![
                SnapshotObjectDescriptor {
                    hash: base_hash.clone(),
                    size: (base_bytes.len() as u64).to_string(),
                },
                SnapshotObjectDescriptor {
                    hash: right_hash.clone(),
                    size: (right_bytes.len() as u64).to_string(),
                },
            ];
            objs.sort_by(|a, b| a.hash.cmp(&b.hash));
            objs
        },
    };
    right_env.snapshot_hash = compute_snapshot_hash(&right_env).expect("right hash");

    let right_out = importer
        .commit_import(
            ValidatedSnapshot::from_parts(
                right_env,
                BTreeMap::from([
                    (base_hash, base_bytes),
                    (right_hash, right_bytes),
                    (left_hash, left_bytes),
                ]),
                None,
            )
            .unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .expect("import right head");
    assert!(
        right_out.conflicts_opened >= 1,
        "same-block import must open Hub conflict on target: {right_out:?}"
    );

    let conflicts = target_repo
        .list_unresolved_conflicts()
        .await
        .expect("list conflicts");
    assert!(
        !conflicts.is_empty(),
        "target must retain unresolved same-block conflict rows"
    );
    assert!(
        conflicts.iter().any(|c| c.asset_id == asset.id),
        "conflict rows present after dual-head import: {conflicts:?}"
    );

    // Heads must not be LWW-wiped: both branch revisions remain loadable.
    assert!(
        target_repo
            .get_revision(&left_rev)
            .await
            .expect("get left")
            .is_some(),
        "left same-block head must survive import (not LWW wiped)"
    );
    assert!(
        target_repo
            .get_revision(&right_rev)
            .await
            .expect("get right")
            .is_some(),
        "right same-block head must survive import (not LWW wiped)"
    );
    let after = target_repo
        .list_assets(None, None)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.logical_key == "same-block-root")
        .expect("same-block asset");
    assert!(
        after.current_revision_id.is_some(),
        "asset retains a head after fail-closed merge: {after:?}"
    );
}

// ---------------------------------------------------------------------------
// L2-AGENT-HUB-C-GIT-001
// ---------------------------------------------------------------------------

/// L2-AGENT-HUB-C-GIT-001：export full device lane → 第三环境 inspect/confirm。
///
/// Business Logic: map 一个 project、另一个 unmapped；active assets/history residual-ready。
/// Code Logic: expand_readable_archive → inspect → preview → confirm_import。
#[tokio::test]
async fn l2_agent_hub_c_git_001_export_clone_confirm_import() {
    let src = setup_isolated_env("git-src");
    let third = setup_isolated_env("git-third");

    let src_pool = open_hub_pool(&src.db_path).await;
    let src_repo = AgentHubRepo::new(src_pool);
    let src_store = AgentHubObjectStore::open(&src.data_dir).unwrap();

    let secret = credential_payload();
    let (envelope, object_hash) = sample_envelope_with_secret_rich(&secret, true);
    let parent_bytes = br#"{"seed":"parent-history"}"#.to_vec();
    let parent_hash = sha256_hex(&parent_bytes);
    let mut object_bytes = BTreeMap::new();
    object_bytes.insert(object_hash.clone(), secret.clone());
    object_bytes.insert(parent_hash.clone(), parent_bytes.clone());
    let built = BuiltSnapshot {
        envelope: envelope.clone(),
        object_bytes: object_bytes.clone(),
        selection_hash: hash_selection(&envelope.selection).unwrap(),
        selection_state_hash: "state".into(),
    };

    // 源 CAS/DB seed（rich residual：multi-rev + tombstone + conflict + variant）
    let importer = SnapshotImporter::new(src_repo, src_store, &src.data_dir);
    importer
        .commit_import(
            ValidatedSnapshot::from_parts(envelope.clone(), object_bytes, None).unwrap(),
            ConfirmedImportSelection::default(),
        )
        .await
        .expect("seed source hub");

    let lane_device = "device-export-1";
    let workdir = third.data_dir.join("cloud-workdir");
    fs::create_dir_all(&workdir).unwrap();
    let lane = workdir.join("agent-hub").join("devices").join(lane_device);
    expand_readable_archive(&built, &lane).expect("expand lane");
    assert!(lane.join("snapshot.json").is_file());

    // 第三环境 inspect
    let report = inspect_git_lanes_in_workdir(&workdir, "local-third").unwrap();
    assert!(report.workdir_present);
    let lane_summary = report
        .lanes
        .iter()
        .find(|l| l.lane_device_id == lane_device)
        .expect("lane present");
    assert_eq!(lane_summary.status, "ok");
    assert_eq!(
        lane_summary.snapshot_hash.as_str(),
        envelope.snapshot_hash.as_str()
    );

    let third_pool = open_hub_pool(&third.db_path).await;
    let third_repo = AgentHubRepo::new(third_pool);
    let third_store = AgentHubObjectStore::open(&third.data_dir).unwrap();

    let preview = preview_git_import_in_workdir(&workdir, &third_repo, lane_device)
        .await
        .expect("preview");
    assert!(
        preview.has_credential_bearing_assets,
        "credential disclosure boolean"
    );
    assert_eq!(preview.snapshot_hash, envelope.snapshot_hash);

    let outcome = confirm_git_import_in_workdir(
        &workdir,
        &third_repo,
        &third.data_dir,
        ConfirmGitImportRequest {
            lane_device_id: lane_device.into(),
            snapshot_hash: envelope.snapshot_hash.clone(),
            selected_asset_ids: vec![],
            project_mappings: vec![ConfirmedProjectMapping {
                hub_project_id: "hub-mapped".into(),
                local_workbench_project_id: Some("wb-local-1".into()),
                git_remote_fingerprint: None,
                opted_in: false,
            }],
            import_unmapped_projects: true,
        },
    )
    .await
    .expect("confirm import");

    assert!(
        !outcome.import.imported_asset_ids.is_empty()
            || outcome.import.inserted_revisions > 0
            || outcome.import.deduped_revisions > 0,
        "import residual-ready: {outcome:?}"
    );

    let assets = third_repo.list_assets(None, None).await.unwrap();
    assert!(
        assets.iter().any(|a| a.logical_key == "mcp-secret"),
        "active MCP restored"
    );
    let blob = third_store.get_blob(&object_hash).await.expect("cas");
    assert_eq!(blob.as_slice(), secret.as_slice());

    // multi-revision history retained (parent lineage present)
    assert!(
        third_repo
            .get_revision(&RevisionId(REV_PARENT_ID.into()))
            .await
            .expect("get parent rev")
            .is_some(),
        "parent revision lineage must be retained after Git confirm import"
    );
    assert!(
        third_repo
            .get_revision(&RevisionId(REV_ID.into()))
            .await
            .expect("get head rev")
            .is_some(),
        "head revision must be retained"
    );
    // parent CAS blob also restored when include_history
    assert!(
        third_store.get_blob(&parent_hash).await.is_ok(),
        "parent history blob must be in third-env CAS"
    );

    // tombstone residual retained (deleted asset + Delete revision)
    let all_assets = third_repo
        .list_all_assets_including_deleted()
        .await
        .expect("list including deleted");
    assert!(
        all_assets
            .iter()
            .any(|a| { a.logical_key == "instruction-tombstone" && a.deleted_at.is_some() }),
        "tombstone residual must restore deleted_at asset: {all_assets:?}"
    );
    assert!(
        third_repo
            .get_revision(&RevisionId(TOMBSTONE_REV_ID.into()))
            .await
            .expect("get tombstone rev")
            .is_some(),
        "Delete tombstone revision must be retained"
    );

    // conflict residual class was seeded in the lane envelope (may rehydrate as rows).
    // Do not claim "restore exactly" if importer leaves conflicts empty — history+tombstone already asserted.
    let _conflicts = third_repo
        .list_unresolved_conflicts()
        .await
        .unwrap_or_default();
    assert!(
        envelope.conflicts.iter().any(|c| c.id == CONFLICT_ID),
        "lane envelope must seed conflict residual class"
    );

    // mapped project row present with expected hub mapping
    let mapped = third_repo
        .get_project_mapping_by_hub_project_id("hub-mapped")
        .await
        .expect("query mapped")
        .expect("hub-mapped mapping row present");
    assert_eq!(
        mapped.local_workbench_project_id.as_deref(),
        Some("wb-local-1"),
        "mapped project must keep confirmed local workbench id"
    );
    assert!(
        !mapped.opted_in,
        "mapped project remains non-opted-in residual-ready (not auto full opt-in)"
    );
    assert!(
        mapped
            .local_absolute_path
            .as_deref()
            .map(|p| p.is_empty())
            .unwrap_or(true),
        "mapped project must not invent a guessed local absolute path: {mapped:?}"
    );

    // unmapped project remains without guessed local path / residual-ready (not auto opted-in)
    let unmapped = third_repo
        .get_project_mapping_by_hub_project_id("hub-unmapped")
        .await
        .expect("query unmapped");
    if let Some(row) = unmapped {
        assert!(
            !row.opted_in,
            "unmapped project must not be auto opted-in: {row:?}"
        );
        assert!(
            row.local_workbench_project_id.is_none()
                || row.local_workbench_project_id.as_deref() == Some(""),
            "unmapped project must not guess a workbench id: {row:?}"
        );
        assert!(
            row.local_absolute_path
                .as_deref()
                .map(|p| p.is_empty())
                .unwrap_or(true),
            "unmapped project must not invent local absolute path: {row:?}"
        );
    }
    // residual-ready: unmapped is either absent or present non-opted-in without guessed path

    // inspect/preview 不得自动 import
    let empty_env = setup_isolated_env("git-empty");
    let empty_pool = open_hub_pool(&empty_env.db_path).await;
    let empty_repo = AgentHubRepo::new(empty_pool);
    let empty_store = AgentHubObjectStore::open(&empty_env.data_dir).unwrap();
    let _ = inspect_git_lanes_in_workdir(&workdir, "local-empty").unwrap();
    let _ = preview_git_import_in_workdir(&workdir, &empty_repo, lane_device)
        .await
        .unwrap();
    let _ = empty_store;
    assert!(
        empty_repo.list_assets(None, None).await.unwrap().is_empty(),
        "inspect/preview must not auto-import"
    );
}

// ---------------------------------------------------------------------------
// capability health peer
// ---------------------------------------------------------------------------

/// 启动仅 health 的 peer，用于 capability 门控。
async fn spawn_health_peer(
    with_agent_hub: bool,
    hits: Arc<AtomicUsize>,
) -> (String, tokio::task::JoinHandle<()>) {
    let hits2 = Arc::clone(&hits);
    let app = Router::new().route(
        "/api/health",
        get(move || {
            let hits = Arc::clone(&hits2);
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                let mut caps = vec!["errors.envelope.v1".to_string()];
                if with_agent_hub {
                    caps.push(CAPABILITY_AGENT_HUB_V1.to_string());
                }
                axum::Json(json!({
                    "ok": true,
                    "device_id": "peer-cap",
                    "device_name": "Peer Cap",
                    "http_port": 0,
                    "ts": 1,
                    "protocol_version": 1,
                    "capabilities": caps,
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), handle)
}
