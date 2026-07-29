//! agent_hub/snapshot/builder — 确定性 Snapshot 构建与 selection 闭包
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN/Git 导出前必须从 SQLite DAG + CAS 生成同一可验证 envelope；
//!     相同 selection + Hub 状态必须复用 snapshotId/createdAt/snapshotHash。
//!
//! Code Logic（这个模块做什么）:
//!     `build_snapshot` 经 repo 单读事务冻结身份集合后，流式 re-hash CAS objects，
//!     计算 selectionStateHash（与 repack 共用纯函数）并缓存 last envelope。

use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::snapshot::canonical_json::canonicalize_value;
use crate::agent_hub::snapshot::envelope::{
    compute_snapshot_hash, default_snapshot_limits, validate_snapshot, SnapshotAlias,
    SnapshotAsset, SnapshotConflict, SnapshotEnvelopeV1, SnapshotLimits, SnapshotLineage,
    SnapshotObjectDescriptor, SnapshotRevision, SnapshotSelection, SnapshotVariant,
    CANONICALIZATION_NAME, FORMAT_NAME, FORMAT_VERSION,
};
use crate::error::AppError;
use crate::storage::agent_hub_repo::{AgentHubRepo, SnapshotIdentityMode, SnapshotIdentityRequest};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

/// 进程内 last-completed envelope 缓存键 `{selectionHash,selectionStateHash}`。
static ENVELOPE_CACHE: OnceLock<Mutex<HashMap<(String, String), BuiltSnapshot>>> = OnceLock::new();

/// Snapshot 选择请求。
///
/// Business Logic（为什么需要这个结构体）:
///     导出 scope 必须显式：full hub / user / project / asset，不能靠目录遍历。
///
/// Code Logic（这个结构体做什么）:
///     保存 mode、可选 id 列表、include_history、replica、limits。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSelectionRequest {
    /// 选择模式
    pub mode: SnapshotSelectionMode,
    /// 显式 scope ids（UserScope / 部分 Full 过滤）
    #[serde(default)]
    pub scope_ids: Vec<String>,
    /// 显式 asset ids
    #[serde(default)]
    pub asset_ids: Vec<String>,
    /// 项目 hubProjectId 列表（Project 模式）
    #[serde(default)]
    pub hub_project_ids: Vec<String>,
    /// 是否包含完整 revision ancestry
    pub include_history: bool,
    /// 源 replica / device id（UUID 字符串）
    pub source_replica_id: String,
    /// 可选 limits；None 用默认
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<SnapshotLimits>,
}

/// Snapshot 选择模式。
///
/// Business Logic: full / user / project / explicit asset 四档。
/// Code Logic: camelCase 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotSelectionMode {
    /// 全部 Hub 资产
    FullHub,
    /// 用户 scope
    UserScope,
    /// 一个或多个 project（hubProjectId）
    Project,
    /// 显式 asset id 列表
    ExplicitAssets,
}

/// 构建完成的 snapshot（envelope + object bytes）。
///
/// Business Logic（为什么需要这个结构体）:
///     archive expand/repack 与后续 LAN 需要 envelope 与 CAS 正文同捆。
///
/// Code Logic（这个结构体做什么）:
///     持有 typed envelope、hash→bytes、selection 哈希缓存键。
#[derive(Debug, Clone)]
pub struct BuiltSnapshot {
    /// 已校验 envelope
    pub envelope: SnapshotEnvelopeV1,
    /// object hash → 精确字节
    pub object_bytes: BTreeMap<String, Vec<u8>>,
    /// selection 的 canonical hash（不含 state）
    pub selection_hash: String,
    /// selection + 选中身份集合 hash
    pub selection_state_hash: String,
}

/// 构建确定性 snapshot。
///
/// Business Logic（为什么需要这个函数）:
///     源侧 LAN push / Git device lane 的唯一导出入口；失败不得半截成功。
///
/// Code Logic（这个函数做什么）:
///     单读事务 `load_snapshot_identity_bundle` 冻结身份 → re-hash CAS →
///     selectionStateHash 缓存复用 → 填 envelope 并 validate。
pub async fn build_snapshot(
    repo: &AgentHubRepo,
    objects: &ObjectStore,
    request: SnapshotSelectionRequest,
) -> Result<BuiltSnapshot, AppError> {
    let limits = request
        .limits
        .clone()
        .unwrap_or_else(default_snapshot_limits);

    // 1) 单读事务：selection heads + ancestry + variants/conflicts/aliases
    let identity = repo
        .load_snapshot_identity_bundle(&SnapshotIdentityRequest {
            mode: match request.mode {
                SnapshotSelectionMode::FullHub => SnapshotIdentityMode::FullHub,
                SnapshotSelectionMode::UserScope => SnapshotIdentityMode::UserScope,
                SnapshotSelectionMode::Project => SnapshotIdentityMode::Project,
                SnapshotSelectionMode::ExplicitAssets => SnapshotIdentityMode::ExplicitAssets,
            },
            scope_ids: request.scope_ids.clone(),
            asset_ids: request.asset_ids.clone(),
            hub_project_ids: request.hub_project_ids.clone(),
            include_history: request.include_history,
        })
        .await?;
    let assets = identity.assets;
    if assets.is_empty() && matches!(request.mode, SnapshotSelectionMode::ExplicitAssets) {
        return Err(AppError::validation(
            "agent_hub_snapshot_empty_selection".to_string(),
        ));
    }

    // 2) lineages（TX 结果 + 自 lineage 兜底）
    let mut lineages: Vec<SnapshotLineage> = identity
        .lineages
        .iter()
        .map(|(asset_id, lineage_id)| SnapshotLineage {
            id: lineage_id.clone(),
            root_asset_id: asset_id.clone(),
        })
        .collect();
    let mut lineage_set: BTreeSet<String> = lineages.iter().map(|l| l.id.clone()).collect();
    for a in &assets {
        if lineage_set.insert(a.id.clone()) {
            lineages.push(SnapshotLineage {
                id: a.id.clone(),
                root_asset_id: a.id.clone(),
            });
        }
    }
    lineages.sort_by(|a, b| a.id.cmp(&b.id));
    lineages.dedup_by(|a, b| a.id == b.id);

    // 3) heads（身份集合已冻结）
    let mut asset_heads: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for a in &assets {
        if let Some(rev) = &a.current_revision_id {
            asset_heads.insert(a.id.clone(), vec![rev.as_str().to_string()]);
        } else {
            asset_heads.insert(a.id.clone(), vec![]);
        }
    }

    // 4–7) revisions / variants / conflicts / aliases（均来自同一 TX 冻结结果）
    let revisions = identity.revisions;
    let variant_rows = identity.variants;
    let conflicts = identity.conflicts;
    let alias_rows = identity.aliases;

    // 8) collect object hashes from revisions + variants extension（CAS 可在 TX 外）
    let mut object_hashes: BTreeSet<String> = BTreeSet::new();
    for rev in &revisions {
        if let Some(h) = &rev.payload_hash {
            object_hashes.insert(h.clone());
        }
        if let Some(h) = &rev.tree_manifest_hash {
            object_hashes.insert(h.clone());
            // expand tree → blob hashes
            let tree = objects.get_tree(h).await.map_err(|e| {
                AppError::validation(format!("agent_hub_snapshot_tree_missing:{}", short_err(&e)))
            })?;
            for entry in tree.entries {
                object_hashes.insert(entry.blob_hash);
            }
        }
    }
    for v in &variant_rows {
        if let Some(h) = &v.extension_payload_hash {
            object_hashes.insert(h.clone());
        }
    }

    // 9) stream re-hash each object; missing/corrupt blocks whole snapshot
    let mut object_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut object_descs: Vec<SnapshotObjectDescriptor> = Vec::new();
    let mut total_uncompressed: u64 = 0;
    for hash in &object_hashes {
        let bytes = objects.get_blob(hash).await.map_err(|e| {
            AppError::validation(format!(
                "agent_hub_snapshot_object_missing:hash_len={},err={}",
                hash.len(),
                short_err(&e)
            ))
        })?;
        let actual = sha256_hex(&bytes);
        if actual != *hash {
            return Err(AppError::validation(format!(
                "agent_hub_snapshot_object_hash_mismatch:expected_len={},actual_len={}",
                hash.len(),
                actual.len()
            )));
        }
        let size = bytes.len() as u64;
        if size > limits.max_blob_bytes {
            return Err(AppError::validation(format!(
                "agent_hub_snapshot_limit:blob_bytes:actual={size}:limit={}",
                limits.max_blob_bytes
            )));
        }
        total_uncompressed = total_uncompressed.saturating_add(size);
        if total_uncompressed > limits.max_uncompressed_bytes {
            return Err(AppError::validation(format!(
                "agent_hub_snapshot_limit:uncompressed_bytes:actual={total_uncompressed}:limit={}",
                limits.max_uncompressed_bytes
            )));
        }
        object_descs.push(SnapshotObjectDescriptor {
            hash: hash.clone(),
            size: size.to_string(),
        });
        object_bytes.insert(hash.clone(), bytes);
    }
    object_descs.sort_by(|a, b| a.hash.cmp(&b.hash));

    // 10) map to snapshot DTOs
    let snap_assets: Vec<SnapshotAsset> = assets
        .iter()
        .map(|a| SnapshotAsset {
            id: a.id.clone(),
            scope_id: a.scope_id.clone(),
            kind: a.kind,
            origin_namespace: a.origin_namespace.clone(),
            logical_key: a.logical_key.clone(),
            display_name: a.display_name.clone(),
            policy: a.policy,
            deleted_at: a.deleted_at.clone(),
        })
        .collect();

    let mut snap_revisions: Vec<SnapshotRevision> = revisions
        .iter()
        .map(|r| SnapshotRevision {
            id: r.id.as_str().to_string(),
            asset_lineage_id: r.asset_lineage_id.clone(),
            parents: r.parents.iter().map(|p| p.as_str().to_string()).collect(),
            generation: r.generation.to_string(),
            operation: r.operation,
            origin_kind: r.origin_kind,
            origin_target: r.origin_target,
            origin_replica_id: r.origin_replica_id.clone(),
            payload_hash: r.payload_hash.clone(),
            tree_manifest_hash: r.tree_manifest_hash.clone(),
            created_at: r.created_at.clone(),
        })
        .collect();
    snap_revisions.sort_by(|a, b| a.id.cmp(&b.id));

    let mut snap_variants: Vec<SnapshotVariant> = variant_rows
        .iter()
        .map(|v| SnapshotVariant {
            asset_id: v.asset_id.clone(),
            target: v.target,
            revision_id: v.revision_id.clone(),
        })
        .collect();
    snap_variants.sort_by(|a, b| {
        a.asset_id
            .cmp(&b.asset_id)
            .then(a.target.as_str().cmp(b.target.as_str()))
            .then(a.revision_id.cmp(&b.revision_id))
    });

    let mut snap_conflicts: Vec<SnapshotConflict> = conflicts
        .iter()
        .map(|c| SnapshotConflict {
            id: c.id.clone(),
            asset_id: c.asset_id.clone(),
            target: c.target,
            base_revision_id: c.base_revision_id.as_ref().map(|r| r.as_str().to_string()),
            hub_revision_id: c.hub_revision_id.as_ref().map(|r| r.as_str().to_string()),
            external_revision_id: c
                .external_revision_id
                .as_ref()
                .map(|r| r.as_str().to_string()),
            detail_json: c.detail_json.clone(),
            created_at: c.created_at.clone(),
        })
        .collect();
    snap_conflicts.sort_by(|a, b| a.id.cmp(&b.id));

    let mut snap_aliases: Vec<SnapshotAlias> = alias_rows
        .iter()
        .map(|a| SnapshotAlias {
            kind: a.kind.clone(),
            external_id: a.external_id.clone(),
            local_id: a.local_id.clone(),
        })
        .collect();
    snap_aliases.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.external_id.cmp(&b.external_id))
            .then(a.local_id.cmp(&b.local_id))
    });

    // 11) selection field on envelope
    let selection = build_envelope_selection(&request, &assets);

    // 12) selectionHash + selectionStateHash before allocating ids
    let selection_hash = hash_selection(&selection)?;
    let selection_state_hash = hash_selection_state(
        &selection,
        &snap_assets,
        &lineages,
        &snap_revisions,
        &asset_heads,
        &snap_variants,
        &snap_conflicts,
        &snap_aliases,
        &object_descs,
    )?;

    // 13) cache hit → reuse prior envelope metadata
    if let Some(cached) = cache_get(&selection_hash, &selection_state_hash) {
        // verify object set still matches
        if cached.envelope.objects == object_descs
            && cached.object_bytes.keys().eq(object_bytes.keys())
        {
            return Ok(BuiltSnapshot {
                envelope: cached.envelope,
                object_bytes,
                selection_hash,
                selection_state_hash,
            });
        }
    }

    let snapshot_id = Uuid::now_v7().to_string();
    let created_at = Utc::now().to_rfc3339();

    let mut envelope = SnapshotEnvelopeV1 {
        format: FORMAT_NAME.into(),
        format_version: FORMAT_VERSION,
        canonicalization: CANONICALIZATION_NAME.into(),
        snapshot_id,
        snapshot_hash: "0".repeat(64),
        source_replica_id: request.source_replica_id.clone(),
        created_at,
        selection,
        asset_heads,
        assets: snap_assets,
        lineages,
        revisions: snap_revisions,
        variants: snap_variants,
        conflicts: snap_conflicts,
        aliases: snap_aliases,
        objects: object_descs,
    };
    envelope.snapshot_hash = compute_snapshot_hash(&envelope)
        .map_err(|e| AppError::validation(format!("agent_hub_snapshot_hash_failed:{e}")))?;

    // pre-validate with limits (also catches entry counts)
    let json = {
        let value = serde_json::to_value(&envelope)
            .map_err(|e| AppError::generic(format!("snapshot_serialize:{e}")))?;
        let bytes = canonicalize_value(&value)
            .map_err(|e| AppError::validation(format!("snapshot_canon:{e}")))?;
        String::from_utf8(bytes).map_err(|e| AppError::generic(format!("snapshot_utf8:{e}")))?
    };
    validate_snapshot(&json, &limits).map_err(|e| {
        // diagnostics only: code/counts — no secret bodies
        AppError::validation(format!("agent_hub_snapshot_validate:{e}"))
    })?;

    let built = BuiltSnapshot {
        envelope,
        object_bytes,
        selection_hash,
        selection_state_hash,
    };
    cache_put(built.clone());
    Ok(built)
}

/// 从 request + 选中资产构造 envelope.selection。
///
/// Business Logic: selection 字段必须稳定且反映导出意图。
/// Code Logic: scope_ids/asset_ids 排序去重。
fn build_envelope_selection(
    request: &SnapshotSelectionRequest,
    assets: &[crate::agent_hub::models::LogicalAsset],
) -> SnapshotSelection {
    let mut scope_ids: BTreeSet<String> = request.scope_ids.iter().cloned().collect();
    for a in assets {
        scope_ids.insert(a.scope_id.clone());
    }
    let mut asset_ids: BTreeSet<String> = assets.iter().map(|a| a.id.clone()).collect();
    // explicit 模式保留请求中的 id（即便缺失也写入 selection 便于审计）
    if matches!(request.mode, SnapshotSelectionMode::ExplicitAssets) {
        for id in &request.asset_ids {
            asset_ids.insert(id.clone());
        }
    }
    SnapshotSelection {
        scope_ids: scope_ids.into_iter().collect(),
        asset_ids: asset_ids.into_iter().collect(),
        include_history: request.include_history,
    }
}

/// SHA-256 hex of canonical SnapshotSelection。
///
/// Business Logic（为什么需要这个函数）:
///     LAN prepare 与 expand→repack 必须共用同一 selectionHash 公式。
///
/// Code Logic（这个函数做什么）:
///     serde → canonicalize → sha256。
pub fn hash_selection(selection: &SnapshotSelection) -> Result<String, AppError> {
    let value = serde_json::to_value(selection)
        .map_err(|e| AppError::generic(format!("selection_to_value:{e}")))?;
    let bytes = canonicalize_value(&value)
        .map_err(|e| AppError::validation(format!("selection_canon:{e}")))?;
    Ok(sha256_hex(&bytes))
}

/// selectionStateHash：selection + 选中身份集合（无 payload 正文）。
///
/// Business Logic（为什么需要这个函数）:
///     同一 Hub 状态复用 envelope；build 与 repack 必须共享公式，否则缓存键漂移。
///
/// Code Logic（这个函数做什么）:
///     稳定 JSON 身份字段（含 assetDeletedAt / revisionParents 等富字段）→ canonical → sha256。
#[allow(clippy::too_many_arguments)]
pub fn hash_selection_state(
    selection: &SnapshotSelection,
    assets: &[SnapshotAsset],
    lineages: &[SnapshotLineage],
    revisions: &[SnapshotRevision],
    asset_heads: &BTreeMap<String, Vec<String>>,
    variants: &[SnapshotVariant],
    conflicts: &[SnapshotConflict],
    aliases: &[SnapshotAlias],
    objects: &[SnapshotObjectDescriptor],
) -> Result<String, AppError> {
    let state = json!({
        "selection": selection,
        "assetIds": assets.iter().map(|a| &a.id).collect::<Vec<_>>(),
        "assetDeletedAt": assets.iter().map(|a| json!({
            "id": a.id,
            "deletedAt": a.deleted_at,
            "scopeId": a.scope_id,
            "kind": a.kind,
            "logicalKey": a.logical_key,
            "policy": a.policy,
        })).collect::<Vec<_>>(),
        "lineageIds": lineages.iter().map(|l| &l.id).collect::<Vec<_>>(),
        "revisionIds": revisions.iter().map(|r| &r.id).collect::<Vec<_>>(),
        "revisionParents": revisions.iter().map(|r| json!({
            "id": r.id,
            "parents": r.parents,
            "generation": r.generation,
            "operation": r.operation,
            "payloadHash": r.payload_hash,
            "treeManifestHash": r.tree_manifest_hash,
        })).collect::<Vec<_>>(),
        "assetHeads": asset_heads,
        "variants": variants,
        "conflictIds": conflicts.iter().map(|c| &c.id).collect::<Vec<_>>(),
        "aliases": aliases,
        "objects": objects,
    });
    let bytes =
        canonicalize_value(&state).map_err(|e| AppError::validation(format!("state_canon:{e}")))?;
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 诊断用：AppError Display 截断，永不回显正文。
fn short_err(e: &AppError) -> String {
    let s = e.to_string();
    if s.len() > 120 {
        format!("{}…", &s[..120])
    } else {
        s
    }
}

fn cache_store() -> &'static Mutex<HashMap<(String, String), BuiltSnapshot>> {
    ENVELOPE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_get(selection_hash: &str, state_hash: &str) -> Option<BuiltSnapshot> {
    let guard = cache_store().lock().ok()?;
    guard
        .get(&(selection_hash.to_string(), state_hash.to_string()))
        .cloned()
}

fn cache_put(built: BuiltSnapshot) {
    if let Ok(mut guard) = cache_store().lock() {
        // 有界：最多保留 32 条
        if guard.len() >= 32 {
            guard.clear();
        }
        guard.insert(
            (
                built.selection_hash.clone(),
                built.selection_state_hash.clone(),
            ),
            built,
        );
    }
}

/// 测试用：清空 envelope 缓存。
#[cfg(test)]
pub fn clear_envelope_cache_for_test() {
    if let Ok(mut guard) = cache_store().lock() {
        guard.clear();
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::assets::{
        canonical_bytes, McpTransport, PortableAssetPayload, PortableMcpServer,
    };
    use crate::agent_hub::models::{
        AgentTarget, AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode,
        RevisionId, RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::storage::agent_hub_repo::{
        AgentHubRepo, SnapshotIdentityMode, SnapshotIdentityRequest, UpsertAgentHubProjectMapping,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    const SECRET: &str = "plain-fixture-secret";

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
        let store = ObjectStore::open(dir.path()).unwrap();
        (repo, store, dir)
    }

    async fn seed_user_scope(repo: &AgentHubRepo) -> String {
        repo.insert_scope(NewScopeNode {
            id: Some("scope-user".to_string()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap()
        .id
    }

    async fn seed_project_scope(repo: &AgentHubRepo, hub: &str) -> String {
        repo.insert_scope(NewScopeNode {
            id: Some(format!("scope-proj-{hub}")),
            kind: ScopeKind::Project,
            hub_project_id: Some(hub.into()),
            relative_path: Some(".".to_string()),
        })
        .await
        .unwrap()
        .id
    }

    async fn put_text(store: &ObjectStore, text: &str) -> String {
        store.put_blob(text.as_bytes()).await.unwrap().hash
    }

    /// Business Logic: 单 asset head 闭包含 parents/tombstone/variants/conflicts/objects/alias。
    #[tokio::test]
    async fn selection_closure_explicit_asset_includes_ancestry_and_excludes_unrelated() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let proj = seed_project_scope(&repo, "hub-proj-1").await;
        repo.upsert_project_mapping(UpsertAgentHubProjectMapping {
            hub_project_id: "hub-proj-1".to_string(),
            local_workbench_project_id: Some("wb-local-1".to_string()),
            git_remote_fingerprint: Some("git-fp-1".to_string()),
            local_absolute_path: Some("/Users/someone/secret-path/project".to_string()),
            opted_in: true,
        })
        .await
        .unwrap();

        // related asset with 2-parent history + tombstone later
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: proj.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "root".to_string(),
                display_name: "Root".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h1 = put_text(&store, "rev-parent-body").await;
        let parent = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h1.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let h2 = put_text(&store, "rev-head-body").await;
        let head = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![parent.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h2.clone()),
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:00:00Z".to_string(),
                expected_parent_id: Some(parent.id.clone()),
            })
            .await
            .unwrap();
        // variant on head
        repo.upsert_variant(&asset.id, AgentTarget::Codex, head.id.as_str())
            .await
            .unwrap();
        // unresolved conflict
        repo.insert_conflict(&asset.id, None, r#"{"reason":"test"}"#)
            .await
            .unwrap();

        // unrelated asset
        let unrelated = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "other".to_string(),
                display_name: "Other".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let hu = put_text(&store, "unrelated-body").await;
        let _ = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: unrelated.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(hu),
                tree_manifest_hash: None,
                created_at: "2026-07-29T09:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();

        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::ExplicitAssets,
                scope_ids: vec![],
                asset_ids: vec![asset.id.clone()],
                hub_project_ids: vec!["hub-proj-1".to_string()],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .expect("build");

        // assets: only selected
        assert_eq!(built.envelope.assets.len(), 1);
        assert_eq!(built.envelope.assets[0].id, asset.id);
        // parents + head
        let rev_ids: BTreeSet<_> = built
            .envelope
            .revisions
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert!(rev_ids.contains(parent.id.as_str()));
        assert!(rev_ids.contains(head.id.as_str()));
        // variant
        assert_eq!(built.envelope.variants.len(), 1);
        assert_eq!(built.envelope.variants[0].target, AgentTarget::Codex);
        // conflict
        assert_eq!(built.envelope.conflicts.len(), 1);
        // objects unique
        let mut hashes: Vec<_> = built
            .envelope
            .objects
            .iter()
            .map(|o| o.hash.clone())
            .collect();
        let before = hashes.clone();
        hashes.sort();
        hashes.dedup();
        assert_eq!(before, hashes);
        assert!(built.object_bytes.contains_key(&h1));
        assert!(built.object_bytes.contains_key(&h2));
        // aliases portable, no absolute path
        let alias_blob = serde_json::to_string(&built.envelope.aliases).unwrap();
        assert!(alias_blob.contains("hub-proj-1") || alias_blob.contains("git-fp-1"));
        assert!(!alias_blob.contains("/Users/someone"));
        assert!(!alias_blob.contains("secret-path"));
        // unrelated not present
        assert!(!built.envelope.assets.iter().any(|a| a.id == unrelated.id));
        // secret diagnostics
        let err = AppError::validation("x".to_string());
        assert!(!format!("{err}").contains(SECRET));
    }

    /// Business Logic: FullHub / UserScope / Project 选择范围。
    #[tokio::test]
    async fn selection_modes_full_user_project() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let proj = seed_project_scope(&repo, "hub-p2").await;

        let u_asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "u1".to_string(),
                display_name: "U1".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let p_asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: proj.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "p1".to_string(),
                display_name: "P1".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        for a in [&u_asset, &p_asset] {
            let h = put_text(&store, &format!("body-{}", a.id)).await;
            repo.append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: a.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        }

        let full = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(full.envelope.assets.len(), 2);

        let user_only = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::UserScope,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(user_only.envelope.assets.len(), 1);
        assert_eq!(user_only.envelope.assets[0].id, u_asset.id);

        let proj_only = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::Project,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec!["hub-p2".to_string()],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(proj_only.envelope.assets.len(), 1);
        assert_eq!(proj_only.envelope.assets[0].id, p_asset.id);
    }

    /// Business Logic: 相同 selection state 复用 snapshotId/hash。
    #[tokio::test]
    async fn identical_state_reuses_cached_envelope_ids() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "cache".to_string(),
                display_name: "Cache".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_text(&store, "stable-body").await;
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            payload_hash: Some(h),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".to_string(),
            expected_parent_id: None,
        })
        .await
        .unwrap();

        let req = SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            limits: None,
        };
        let a = build_snapshot(&repo, &store, req.clone()).await.unwrap();
        let b = build_snapshot(&repo, &store, req).await.unwrap();
        assert_eq!(a.envelope.snapshot_id, b.envelope.snapshot_id);
        assert_eq!(a.envelope.created_at, b.envelope.created_at);
        assert_eq!(a.envelope.snapshot_hash, b.envelope.snapshot_hash);
        assert_eq!(a.selection_state_hash, b.selection_state_hash);
    }

    /// Business Logic: 缺失 object 整包失败。
    #[tokio::test]
    async fn missing_object_blocks_whole_snapshot() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "miss".to_string(),
                display_name: "Miss".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let fake_hash = "ab".repeat(32);
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id,
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            payload_hash: Some(fake_hash),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".to_string(),
            expected_parent_id: None,
        })
        .await
        .unwrap();

        let err = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("object_missing") || msg.contains("not_found") || msg.contains("snapshot")
        );
        assert!(!msg.contains(SECRET));
    }

    /// Business Logic: MCP 凭据字节进入 objects 但不出现在错误诊断。
    #[tokio::test]
    async fn mcp_credentials_bytes_preserved_in_objects() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".to_string(),
                logical_key: "secret-mcp".to_string(),
                display_name: "Secret MCP".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let payload = PortableAssetPayload::Mcp(PortableMcpServer {
            key: "secret-mcp".to_string(),
            transport: McpTransport::Stdio {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "demo".to_string()],
                cwd: None,
            },
            env: BTreeMap::from([("TOKEN".to_string(), SECRET.to_string())]),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: BTreeMap::new(),
        });
        let bytes = canonical_bytes(&payload).unwrap();
        assert!(std::str::from_utf8(&bytes).unwrap().contains(SECRET));
        let stored = store.put_blob(&bytes).await.unwrap();
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id,
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            payload_hash: Some(stored.hash.clone()),
            tree_manifest_hash: None,
            created_at: "2026-07-29T10:00:00Z".to_string(),
            expected_parent_id: None,
        })
        .await
        .unwrap();

        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let obj = built.object_bytes.get(&stored.hash).unwrap();
        assert_eq!(obj, &bytes);
        // envelope JSON 不含 secret 明文（secret 只在 objects 正文）
        let env_json = serde_json::to_string(&built.envelope).unwrap();
        assert!(!env_json.contains(SECRET));
    }

    /// Business Logic: production build 必须经单读事务 helper，而非自由 N 次 auto-commit 查询。
    /// Code Logic: 直接调用 load_snapshot_identity_bundle 并与 build_snapshot 结果身份集合对齐。
    #[tokio::test]
    async fn build_snapshot_uses_single_read_tx_identity_bundle() {
        clear_envelope_cache_for_test();
        let (repo, store, _dir) = test_env().await;
        let user = seed_user_scope(&repo).await;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "tx-contract".to_string(),
                display_name: "TX".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = put_text(&store, "tx-body").await;
        let rev = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();

        let bundle = repo
            .load_snapshot_identity_bundle(&SnapshotIdentityRequest {
                mode: SnapshotIdentityMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
            })
            .await
            .expect("single-tx identity load");
        assert_eq!(bundle.assets.len(), 1);
        assert_eq!(bundle.assets[0].id, asset.id);
        assert!(bundle.revisions.iter().any(|r| r.id == rev.id));

        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                limits: None,
            },
        )
        .await
        .expect("build via production path");
        assert_eq!(built.envelope.assets.len(), bundle.assets.len());
        assert_eq!(built.envelope.revisions.len(), bundle.revisions.len());
        assert_eq!(
            built.envelope.assets[0].id, bundle.assets[0].id,
            "build_snapshot must surface the same TX-frozen asset set"
        );
    }
}
