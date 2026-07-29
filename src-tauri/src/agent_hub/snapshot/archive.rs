//! agent_hub/snapshot/archive — 可读 archive 展开与重打包
//!
//! Business Logic（为什么需要这个模块）:
//!     Git device lane 需要人类可读目录布局；expand→repack 必须字节稳定回到
//!     同一 canonical manifest 与 object hashes。可读文件是视图，importer 只信 envelope。
//!
//! Code Logic（这个模块做什么）:
//!     `expand_readable_archive` 写 snapshot.json + objects + history/user/projects 视图；
//!     `repack_readable_archive` 读回并校验路径安全/symlink/重复路径。

use crate::agent_hub::models::AssetKind;
use crate::agent_hub::snapshot::builder::BuiltSnapshot;
use crate::agent_hub::snapshot::canonical_json::canonicalize_value;
use crate::agent_hub::snapshot::envelope::{validate_snapshot, SnapshotEnvelopeV1, SnapshotLimits};
use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// 目录 Unix 权限（仅当前用户）。
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
/// 文件 Unix 权限。
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// 可读 archive 展开结果。
///
/// Business Logic: 调用方需要根路径与 envelope 引用以便后续 repack/断言。
/// Code Logic: root + envelope clone。
#[derive(Debug, Clone)]
pub struct ExpandedSnapshot {
    /// 展开根目录
    pub root: PathBuf,
    /// 已写入的 envelope
    pub envelope: SnapshotEnvelopeV1,
}

/// 将 BuiltSnapshot 展开为可读目录布局。
///
/// Business Logic（为什么需要这个函数）:
///     Git lane 需要 snapshot.json + objects + 人类可读 views；权限 0700/0600。
///
/// Code Logic（这个函数做什么）:
///     清空/创建 destination → 写 snapshot.json → objects/sha256/xx/hash →
///     history/user/projects 视图；拒绝 destination 内已有 symlink 根。
pub fn expand_readable_archive(
    snapshot: &BuiltSnapshot,
    destination: &Path,
) -> Result<ExpandedSnapshot, AppError> {
    if destination.exists() {
        // 拒绝把 symlink 根当 destination
        let meta = fs::symlink_metadata(destination).map_err(AppError::from)?;
        if meta.file_type().is_symlink() {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_symlink_root".to_string(),
            ));
        }
        if destination.is_dir() {
            // 已存在目录也强制 0700（含父级路径段）
            create_dir_mode(destination)?;
        } else {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_destination_not_dir".to_string(),
            ));
        }
    } else {
        create_dir_mode(destination)?;
    }

    // track normalized relative paths to detect duplicates
    let mut written: BTreeSet<String> = BTreeSet::new();

    // snapshot.json
    let env_json = envelope_canonical_json(&snapshot.envelope)?;
    write_file_mode(
        &destination.join("snapshot.json"),
        env_json.as_bytes(),
        &mut written,
        "snapshot.json",
    )?;

    // objects
    for (hash, bytes) in &snapshot.object_bytes {
        let rel = format!("objects/sha256/{}/{}", &hash[..2], hash);
        let path = destination
            .join("objects")
            .join("sha256")
            .join(&hash[..2])
            .join(hash);
        if let Some(parent) = path.parent() {
            create_dir_mode(parent)?;
        }
        write_file_mode(&path, bytes, &mut written, &rel)?;
    }

    // history/<asset-id>/<revision-id>/revision.json
    for rev in &snapshot.envelope.revisions {
        let rel = format!("history/{}/{}/revision.json", rev.asset_lineage_id, rev.id);
        let path = destination
            .join("history")
            .join(&rev.asset_lineage_id)
            .join(&rev.id)
            .join("revision.json");
        if let Some(parent) = path.parent() {
            create_dir_mode(parent)?;
        }
        let body = serde_json::to_vec_pretty(rev)
            .map_err(|e| AppError::generic(format!("revision_view_json:{e}")))?;
        write_file_mode(&path, &body, &mut written, &rel)?;
    }

    // user / projects readable views (indexed by envelope; importer ignores dir names)
    for asset in &snapshot.envelope.assets {
        let kind_dir = asset_kind_dir(asset.kind);
        // Heuristic: project scopes are named/aliased with hubProjectId; else user/
        let under_project = asset.scope_id.contains("proj")
            || snapshot.envelope.aliases.iter().any(|al| {
                al.kind == "hubProjectId" && asset.scope_id.contains(al.local_id.as_str())
            });

        let safe_key = sanitize_path_segment(&asset.logical_key)?;
        let (path, rel) = if under_project {
            let hub = snapshot
                .envelope
                .aliases
                .iter()
                .find(|al| al.kind == "hubProjectId")
                .map(|al| al.local_id.as_str())
                .unwrap_or("unknown-project");
            let hub_seg = sanitize_path_segment(hub)?;
            let rel = format!("projects/{hub_seg}/assets/{kind_dir}/{safe_key}.json");
            let path = destination
                .join("projects")
                .join(&hub_seg)
                .join("assets")
                .join(kind_dir)
                .join(format!("{safe_key}.json"));
            (path, rel)
        } else {
            let rel = format!("user/{kind_dir}/{safe_key}.json");
            let path = destination
                .join("user")
                .join(kind_dir)
                .join(format!("{safe_key}.json"));
            (path, rel)
        };
        if let Some(parent) = path.parent() {
            create_dir_mode(parent)?;
        }
        // view payload: asset identity + head revision ids (not CAS bodies)
        let heads = snapshot
            .envelope
            .asset_heads
            .get(&asset.id)
            .cloned()
            .unwrap_or_default();
        let view = serde_json::json!({
            "id": asset.id,
            "scopeId": asset.scope_id,
            "kind": asset.kind,
            "logicalKey": asset.logical_key,
            "displayName": asset.display_name,
            "policy": asset.policy,
            "deletedAt": asset.deleted_at,
            "heads": heads,
        });
        let body = serde_json::to_vec_pretty(&view)
            .map_err(|e| AppError::generic(format!("asset_view_json:{e}")))?;
        let rel_norm = normalize_rel_path(&rel)?;
        write_file_mode(&path, &body, &mut written, &rel_norm)?;
    }

    // project.json for each hub project alias
    for al in &snapshot.envelope.aliases {
        if al.kind != "hubProjectId" {
            continue;
        }
        let hub_seg = sanitize_path_segment(&al.local_id)?;
        let rel = format!("projects/{}/project.json", hub_seg);
        let path = destination
            .join("projects")
            .join(&hub_seg)
            .join("project.json");
        if let Some(parent) = path.parent() {
            create_dir_mode(parent)?;
        }
        let view = serde_json::json!({
            "hubProjectId": al.local_id,
            "externalId": al.external_id,
        });
        let body = serde_json::to_vec_pretty(&view)
            .map_err(|e| AppError::generic(format!("project_view_json:{e}")))?;
        write_file_mode(&path, &body, &mut written, &rel)?;
    }

    Ok(ExpandedSnapshot {
        root: destination.to_path_buf(),
        envelope: snapshot.envelope.clone(),
    })
}

/// 从可读 archive 重打包 BuiltSnapshot。
///
/// Business Logic（为什么需要这个函数）:
///     expand→repack 必须得到同一 canonical manifest 与 object hashes。
///
/// Code Logic（这个函数做什么）:
///     读 snapshot.json 校验 → 按 envelope.objects 读 objects/ → re-hash →
///     拒绝 symlink/traversal/重复路径/未知必填元数据缺失。
pub fn repack_readable_archive(
    source: &Path,
    limits: &SnapshotLimits,
) -> Result<BuiltSnapshot, AppError> {
    // reject symlink root
    let meta = fs::symlink_metadata(source).map_err(AppError::from)?;
    if meta.file_type().is_symlink() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_symlink_root".to_string(),
        ));
    }
    if !source.is_dir() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_source_not_dir".to_string(),
        ));
    }

    // scan for unsafe paths first
    let mut seen_norm: BTreeSet<String> = BTreeSet::new();
    scan_archive_paths(source, source, &mut seen_norm)?;

    let snap_path = source.join("snapshot.json");
    if !snap_path.is_file() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_missing_snapshot_json".to_string(),
        ));
    }
    // reject symlink snapshot.json
    let sm = fs::symlink_metadata(&snap_path).map_err(AppError::from)?;
    if sm.file_type().is_symlink() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_symlink_file".to_string(),
        ));
    }
    let json_text = fs::read_to_string(&snap_path).map_err(AppError::from)?;
    let envelope = validate_snapshot(&json_text, limits)
        .map_err(|e| AppError::validation(format!("agent_hub_snapshot_archive_invalid:{e}")))?;

    // required metadata fields already enforced by validate_snapshot
    let mut object_bytes: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for desc in &envelope.objects {
        let path = source
            .join("objects")
            .join("sha256")
            .join(&desc.hash[..2])
            .join(&desc.hash);
        let pm = fs::symlink_metadata(&path).map_err(|_| {
            AppError::validation(format!(
                "agent_hub_snapshot_archive_object_missing:hash_len={}",
                desc.hash.len()
            ))
        })?;
        if pm.file_type().is_symlink() {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_symlink_object".to_string(),
            ));
        }
        let bytes = fs::read(&path).map_err(AppError::from)?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != desc.hash {
            return Err(AppError::validation(format!(
                "agent_hub_snapshot_archive_object_hash_mismatch:len={}",
                desc.hash.len()
            )));
        }
        let expected_size: u64 = desc.size.parse().map_err(|_| {
            AppError::validation("agent_hub_snapshot_archive_bad_object_size".to_string())
        })?;
        if bytes.len() as u64 != expected_size {
            return Err(AppError::validation(format!(
                "agent_hub_snapshot_archive_object_size_mismatch:actual={},expected={}",
                bytes.len(),
                expected_size
            )));
        }
        object_bytes.insert(desc.hash.clone(), bytes);
    }

    // selection hashes：与 build_snapshot 共用同一纯函数（富身份输入）
    let selection_hash = crate::agent_hub::snapshot::builder::hash_selection(&envelope.selection)?;
    let selection_state_hash = crate::agent_hub::snapshot::builder::hash_selection_state(
        &envelope.selection,
        &envelope.assets,
        &envelope.lineages,
        &envelope.revisions,
        &envelope.asset_heads,
        &envelope.variants,
        &envelope.conflicts,
        &envelope.aliases,
        &envelope.objects,
    )?;

    Ok(BuiltSnapshot {
        envelope,
        object_bytes,
        selection_hash,
        selection_state_hash,
    })
}

/// 递归扫描 archive 路径：拒绝 symlink / traversal / 重复 normalized 路径。
///
/// Business Logic: 恶意 lane 不得通过 symlink 逃逸或注入重复视图。
/// Code Logic: walkdir 手写 read_dir 递归；symlink_metadata。
fn scan_archive_paths(
    root: &Path,
    current: &Path,
    seen: &mut BTreeSet<String>,
) -> Result<(), AppError> {
    let entries = fs::read_dir(current).map_err(AppError::from)?;
    for entry in entries {
        let entry = entry.map_err(AppError::from)?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(AppError::from)?;
        if meta.file_type().is_symlink() {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_symlink_rejected".to_string(),
            ));
        }
        let rel = path.strip_prefix(root).map_err(|_| {
            AppError::validation("agent_hub_snapshot_archive_path_escape".to_string())
        })?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let norm = normalize_rel_path(&rel_str)?;
        if !seen.insert(norm) {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_duplicate_path".to_string(),
            ));
        }
        if meta.is_dir() {
            scan_archive_paths(root, &path, seen)?;
        }
    }
    Ok(())
}

/// 规范化相对路径：拒绝 `..` / 绝对 / 空组件 / 反斜杠逃逸。
///
/// Business Logic: zip-slip / traversal 防护。
/// Code Logic: Path components 仅 Normal。
fn normalize_rel_path(rel: &str) -> Result<String, AppError> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\0') {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_bad_path".to_string(),
        ));
    }
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_string_lossy();
                if s.is_empty() || s == "." || s == ".." {
                    return Err(AppError::validation(
                        "agent_hub_snapshot_archive_path_traversal".to_string(),
                    ));
                }
                parts.push(s.into_owned());
            }
            Component::CurDir => {}
            _ => {
                return Err(AppError::validation(
                    "agent_hub_snapshot_archive_path_traversal".to_string(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_bad_path".to_string(),
        ));
    }
    Ok(parts.join("/"))
}

/// 单段路径 sanitizer（logical_key / hub id）。
fn sanitize_path_segment(s: &str) -> Result<String, AppError> {
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..") || s.contains('\0') {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_bad_segment".to_string(),
        ));
    }
    // replace unsafe filename chars
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_empty_segment".to_string(),
        ));
    }
    Ok(cleaned)
}

fn asset_kind_dir(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Instruction => "instructions",
        AssetKind::Skill => "skills",
        AssetKind::Command => "commands",
        AssetKind::Agent => "agents",
        AssetKind::Mcp => "mcp",
        AssetKind::Plugin => "plugins",
        AssetKind::Hook => "hooks",
    }
}

fn envelope_canonical_json(envelope: &SnapshotEnvelopeV1) -> Result<String, AppError> {
    let value = serde_json::to_value(envelope)
        .map_err(|e| AppError::generic(format!("envelope_to_value:{e}")))?;
    let bytes = canonicalize_value(&value)
        .map_err(|e| AppError::validation(format!("envelope_canon:{e}")))?;
    String::from_utf8(bytes).map_err(|e| AppError::generic(format!("envelope_utf8:{e}")))
}

/// 逐段创建缺失目录并把本路径上**新创建的每一级**及最终 path 设为 0700。
///
/// Business Logic（为什么需要这个函数）:
///     凭据对象树不得落在 umask 留下的 0755 中间目录下；预存在 destination 也必须收紧。
///
/// Code Logic（这个函数做什么）:
///     自叶向根收集缺失段 → 自浅到深 create_dir 并 chmod 0700；path 已存在则校验非
///     symlink 目录并 chmod 0700。**不**改写更浅的已存在系统祖先（避免 chmod `/`）。
fn create_dir_mode(path: &Path) -> Result<(), AppError> {
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if cur.as_os_str().is_empty() {
            break;
        }
        if cur.exists() {
            let meta = fs::symlink_metadata(&cur).map_err(AppError::from)?;
            if meta.file_type().is_symlink() {
                return Err(AppError::validation(
                    "agent_hub_snapshot_archive_symlink_dir".to_string(),
                ));
            }
            if !meta.is_dir() {
                return Err(AppError::validation(
                    "agent_hub_snapshot_archive_path_not_dir".to_string(),
                ));
            }
            break;
        }
        missing.push(cur.clone());
        match cur.parent() {
            Some(parent) if parent != cur.as_path() => cur = parent.to_path_buf(),
            _ => break,
        }
    }
    for p in missing.into_iter().rev() {
        fs::create_dir(&p).map_err(AppError::from)?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(DIR_MODE);
            fs::set_permissions(&p, perms).map_err(AppError::from)?;
        }
    }
    // 最终 path 已存在（含仅 chmod 预存在 destination）时也强制 0700
    if path.exists() {
        let meta = fs::symlink_metadata(path).map_err(AppError::from)?;
        if meta.file_type().is_symlink() {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_symlink_dir".to_string(),
            ));
        }
        if !meta.is_dir() {
            return Err(AppError::validation(
                "agent_hub_snapshot_archive_path_not_dir".to_string(),
            ));
        }
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(DIR_MODE);
            fs::set_permissions(path, perms).map_err(AppError::from)?;
        }
    }
    Ok(())
}

fn write_file_mode(
    path: &Path,
    bytes: &[u8],
    written: &mut BTreeSet<String>,
    rel: &str,
) -> Result<(), AppError> {
    let norm = normalize_rel_path(rel)?;
    if !written.insert(norm) {
        return Err(AppError::validation(
            "agent_hub_snapshot_archive_duplicate_path".to_string(),
        ));
    }
    if let Some(parent) = path.parent() {
        create_dir_mode(parent)?;
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        opts.mode(FILE_MODE);
    }
    let mut f = opts.open(path).map_err(AppError::from)?;
    f.write_all(bytes).map_err(AppError::from)?;
    f.sync_all().map_err(AppError::from)?;
    #[cfg(unix)]
    {
        let perms = fs::Permissions::from_mode(FILE_MODE);
        fs::set_permissions(path, perms).map_err(AppError::from)?;
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::assets::{
        canonical_bytes, McpTransport, PortableAssetPayload, PortableMcpServer,
    };
    use crate::agent_hub::models::{
        AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode, RevisionId,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::agent_hub::object_store::{ObjectStore, TreeEntry, TreeEntryType, TreeManifest};
    use crate::agent_hub::snapshot::builder::{
        build_snapshot, clear_envelope_cache_for_test, SnapshotSelectionMode,
        SnapshotSelectionRequest,
    };
    use crate::agent_hub::snapshot::envelope::{
        canonicalize_snapshot_without_hash, default_snapshot_limits,
    };
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::BTreeMap as Map;
    use std::os::unix::fs::PermissionsExt;
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

    /// Business Logic: expand→repack 字节稳定（instruction + skill tree + mcp + 2 heads + tombstone）。
    #[tokio::test]
    async fn readable_archive_round_trip_byte_stable() {
        clear_envelope_cache_for_test();
        let (repo, store, tmp) = test_env().await;
        let user = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".to_string()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap()
            .id;

        // instruction asset with two heads (parent + child)
        let instr = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "CLAUDE".to_string(),
                display_name: "CLAUDE.md".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h1 = store.put_blob(b"# parent instruction").await.unwrap().hash;
        let parent = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: instr.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h1),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        let h2 = store.put_blob(b"# head instruction").await.unwrap().hash;
        let _head = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: instr.id.clone(),
                parents: vec![parent.id.clone()],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(h2),
                tree_manifest_hash: None,
                created_at: "2026-07-29T11:00:00Z".to_string(),
                expected_parent_id: Some(parent.id.clone()),
            })
            .await
            .unwrap();

        // skill with tree (binary-ish file)
        let skill_md = store.put_blob(b"# Skill\n").await.unwrap().hash;
        let bin = store
            .put_blob(&[0x00, 0x01, 0xff, 0x7f, b'S', b'K'])
            .await
            .unwrap()
            .hash;
        let tree = TreeManifest {
            entries: vec![
                TreeEntry {
                    path: "SKILL.md".to_string(),
                    blob_hash: skill_md.clone(),
                    entry_type: TreeEntryType::File,
                    executable: false,
                },
                TreeEntry {
                    path: "bin/tool".to_string(),
                    blob_hash: bin.clone(),
                    entry_type: TreeEntryType::File,
                    executable: true,
                },
            ],
        };
        let tree_obj = store.put_tree(&tree).await.unwrap();
        let skill = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Skill,
                origin_namespace: "standalone".to_string(),
                logical_key: "demo-skill".to_string(),
                display_name: "Demo Skill".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        // minimal skill payload referencing tree
        let skill_payload = serde_json::json!({
            "skill": {
                "name": "demo-skill",
                "description": "d",
                "markdownHash": skill_md,
                "treeManifestHash": tree_obj.hash,
                "targetExtensions": {}
            }
        });
        let skill_bytes = serde_json::to_vec(&skill_payload).unwrap();
        let skill_hash = store.put_blob(&skill_bytes).await.unwrap().hash;
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: skill.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            payload_hash: Some(skill_hash),
            tree_manifest_hash: Some(tree_obj.hash.clone()),
            created_at: "2026-07-29T10:30:00Z".to_string(),
            expected_parent_id: None,
        })
        .await
        .unwrap();

        // MCP credentials
        let mcp_payload = PortableAssetPayload::Mcp(PortableMcpServer {
            key: "secret-mcp".to_string(),
            transport: McpTransport::Http {
                url: format!("https://example.invalid/mcp?token={SECRET}"),
                headers: Map::from([("Authorization".to_string(), format!("Bearer {SECRET}"))]),
            },
            env: Map::from([("API_TOKEN".to_string(), SECRET.into())]),
            enabled: true,
            tool_allow: vec![],
            tool_deny: vec![],
            target_extensions: Map::new(),
        });
        let mcp_bytes = canonical_bytes(&mcp_payload).unwrap();
        let mcp_hash = store.put_blob(&mcp_bytes).await.unwrap().hash;
        let mcp = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Mcp,
                origin_namespace: "standalone".to_string(),
                logical_key: "secret-mcp".to_string(),
                display_name: "Secret MCP".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let mcp_rev = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: mcp.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(mcp_hash),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:45:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();

        // tombstone delete revision on a dedicated asset
        let doomed = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user.clone(),
                kind: AssetKind::Command,
                origin_namespace: "standalone".to_string(),
                logical_key: "gone".to_string(),
                display_name: "Gone".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let d0 = store.put_blob(b"alive").await.unwrap().hash;
        let d_parent = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: doomed.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
                payload_hash: Some(d0),
                tree_manifest_hash: None,
                created_at: "2026-07-29T09:00:00Z".to_string(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: doomed.id.clone(),
            parents: vec![d_parent.id.clone()],
            operation: RevisionOperation::Delete,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "01900000-0000-7000-8000-0000000000b1".to_string(),
            payload_hash: None,
            tree_manifest_hash: None,
            created_at: "2026-07-29T12:00:00Z".to_string(),
            expected_parent_id: Some(d_parent.id),
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
        .expect("build");

        // tombstone present
        assert!(built
            .envelope
            .assets
            .iter()
            .any(|a| a.id == doomed.id && a.deleted_at.is_some()));
        // two heads worth of instruction revisions
        assert!(built.envelope.revisions.len() >= 2);
        // mcp secret only in object body
        let env_json = serde_json::to_string(&built.envelope).unwrap();
        assert!(!env_json.contains(SECRET));
        assert!(built.object_bytes.values().any(|b| std::str::from_utf8(b)
            .map(|s| s.contains(SECRET))
            .unwrap_or(false)));

        let dest = tmp.path().join("archive-out");
        let expanded = expand_readable_archive(&built, &dest).expect("expand");
        assert_eq!(
            expanded.envelope.snapshot_hash,
            built.envelope.snapshot_hash
        );

        #[cfg(unix)]
        {
            let dir_meta = fs::metadata(&dest).unwrap();
            assert_eq!(dir_meta.permissions().mode() & 0o777, 0o700);
            let snap_meta = fs::metadata(dest.join("snapshot.json")).unwrap();
            assert_eq!(snap_meta.permissions().mode() & 0o777, 0o600);
            // object file + intermediate dirs 0700
            let any_hash = built.envelope.objects[0].hash.clone();
            let objects_dir = dest.join("objects");
            let sha_dir = objects_dir.join("sha256");
            let prefix_dir = sha_dir.join(&any_hash[..2]);
            let obj_path = prefix_dir.join(&any_hash);
            for d in [&objects_dir, &sha_dir, &prefix_dir] {
                let m = fs::metadata(d).unwrap();
                assert_eq!(
                    m.permissions().mode() & 0o777,
                    0o700,
                    "intermediate dir must be 0700: {}",
                    d.display()
                );
            }
            let obj_meta = fs::metadata(obj_path).unwrap();
            assert_eq!(obj_meta.permissions().mode() & 0o777, 0o600);
            // history intermediate dirs
            if let Some(rev) = built.envelope.revisions.first() {
                let hist = dest
                    .join("history")
                    .join(&rev.asset_lineage_id)
                    .join(&rev.id);
                let m = fs::metadata(&hist).unwrap();
                assert_eq!(m.permissions().mode() & 0o777, 0o700);
            }
        }

        let repacked = repack_readable_archive(&dest, &default_snapshot_limits()).expect("repack");
        let a = canonicalize_snapshot_without_hash(&built.envelope).unwrap();
        let b = canonicalize_snapshot_without_hash(&repacked.envelope).unwrap();
        assert_eq!(a, b, "canonical manifest must match");
        assert_eq!(
            built.envelope.snapshot_hash,
            repacked.envelope.snapshot_hash
        );
        assert_eq!(built.object_bytes, repacked.object_bytes);
        // Important #2: expand→repack 必须保留 builder 的 selection_state_hash 公式
        assert_eq!(
            built.selection_state_hash, repacked.selection_state_hash,
            "selection_state_hash must match after repack"
        );
        assert_eq!(built.selection_hash, repacked.selection_hash);

        // unused mcp_rev silence
        let _ = mcp_rev;
    }

    /// Business Logic: symlink 拒绝。
    #[tokio::test]
    #[cfg(unix)]
    async fn reject_symlink_in_archive() {
        clear_envelope_cache_for_test();
        let (repo, store, tmp) = test_env().await;
        let user = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".to_string()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap()
            .id;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: user,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".to_string(),
                logical_key: "x".to_string(),
                display_name: "X".to_string(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let h = store.put_blob(b"body").await.unwrap().hash;
        repo.append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id,
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
        let dest = tmp.path().join("arch");
        expand_readable_archive(&built, &dest).unwrap();
        // plant symlink
        std::os::unix::fs::symlink("/etc/passwd", dest.join("evil-link")).unwrap();
        let err = repack_readable_archive(&dest, &default_snapshot_limits()).unwrap_err();
        assert!(err.to_string().contains("symlink"));
        assert!(!err.to_string().contains(SECRET));
    }

    /// Business Logic: path traversal 规范化拒绝。
    #[test]
    fn normalize_rejects_traversal() {
        assert!(normalize_rel_path("../etc/passwd").is_err());
        assert!(normalize_rel_path("/abs").is_err());
        assert!(normalize_rel_path("a/../../b").is_err());
        assert_eq!(normalize_rel_path("a/b").unwrap(), "a/b");
    }
}
