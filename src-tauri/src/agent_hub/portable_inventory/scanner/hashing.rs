//! portable_inventory/scanner/hashing — 确定性树哈希
//!
//! Business Logic（为什么需要这个模块）:
//!     planner 在 mutation 前需要能够发现嵌套文件、空目录或 symlink 目标变化，
//!     仅比较根目录名或 manifest 字节会把 stale plan 错当成当前事实；
//!     列表扫描与动作校验必须共享同一 material 域的 content/tree hash。
//!
//! Code Logic（这个模块做什么）:
//!     递归枚举目录树，按规范化相对路径排序生成确定性 JSON 再求 SHA-256
//!     （hash_directory_tree / hash_plugin_root）；
//!     `hash_plugin_root_cached` 以元数据指纹为键做只读 inventory 专用增量缓存，
//!     mutation 校验继续走未缓存入口。

use crate::{agent_hub::object_store::sha256_hex, error::AppError};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

/// 目录树单一确定性 hash（相对路径/类型/内容；不跟随 symlink）。
///
/// Business Logic（为什么需要这个函数）:
///     planner 在 mutation 前需要能够发现嵌套文件、空目录或 symlink 目标变化，
///     仅比较根目录名会把 stale plan 错当成当前事实。
///
/// Code Logic（这个函数做什么）:
///     递归枚举目录，按规范化 `/` 相对路径排序；记录 directory/file/symlink 类型、
///     内容 SHA-256 与平台 executable 位，再对确定性 JSON 求 SHA-256。
pub fn hash_directory_tree(root: &Path) -> Result<String, AppError> {
    if !root.is_dir() {
        return Err(AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"));
    }
    let mut entries = Vec::new();
    collect_deterministic_tree_entries(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let bytes = serde_json::to_vec(&entries)
        .map_err(|e| AppError::generic(format!("portable tree hash serialize: {e}")))?;
    Ok(sha256_hex(&bytes))
}

#[derive(Debug, Serialize)]
struct DeterministicTreeEntry {
    path: String,
    entry_type: &'static str,
    content_hash: String,
    executable: bool,
}

fn collect_deterministic_tree_entries(
    root: &Path,
    current: &Path,
    entries: &mut Vec<DeterministicTreeEntry>,
) -> Result<(), AppError> {
    let mut children: Vec<_> = fs::read_dir(current)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name().to_string_lossy().into_owned());
    for entry in children {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let relative = deterministic_relative_posix(root, &path);
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            let target_text = target.to_string_lossy().replace('\\', "/");
            entries.push(DeterministicTreeEntry {
                path: relative,
                entry_type: "symlink",
                content_hash: sha256_hex(target_text.as_bytes()),
                executable: false,
            });
        } else if file_type.is_dir() {
            entries.push(DeterministicTreeEntry {
                path: relative.clone(),
                entry_type: "directory",
                content_hash: sha256_hex(&[]),
                executable: false,
            });
            collect_deterministic_tree_entries(root, &path, entries)?;
        } else if file_type.is_file() {
            let bytes = fs::read(&path)?;
            entries.push(DeterministicTreeEntry {
                path: relative,
                entry_type: "file",
                content_hash: sha256_hex(&bytes),
                executable: deterministic_is_executable(&metadata),
            });
        } else {
            return Err(AppError::validation(format!(
                "PORTABLE_ASSET_ACTION_UNSUPPORTED_TREE_ENTRY:{relative}"
            )));
        }
    }
    Ok(())
}

fn deterministic_relative_posix(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn deterministic_is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Plugin 根目录 content_hash + tree_hash（与 inventory 行同源）。
///
/// Business Logic: planner `expected_source_hash` 与 apply recheck 必须共享同一 material 域，
/// 禁止路径字符串 sha 与 manifest 字节 hash 混用导致生产 plugin 永远 SOURCE_HASH_CHANGED。
/// Code Logic: 优先 manifest 文件字节；无 manifest 才回落 path display（与历史 inventory 一致）；
/// tree_hash 为递归相对路径/类型/内容 hash。
pub fn hash_plugin_root(root: &Path) -> Result<(String, String), AppError> {
    let content_hash = hash_plugin_manifest(root)?;
    let tree_hash = hash_directory_tree(root)?;
    Ok((content_hash, tree_hash))
}

/// Plugin manifest 身份 hash；列表扫描使用，完整 tree 延迟到动作 preview。
pub(super) fn hash_plugin_manifest(root: &Path) -> Result<String, AppError> {
    let mut hasher_material = Vec::new();
    for rel in [
        ".claude-plugin/plugin.json",
        ".codex-plugin/plugin.json",
        "package.json",
    ] {
        let p = root.join(rel);
        if p.is_file() {
            let bytes = fs::read(&p)?;
            hasher_material.extend_from_slice(&bytes);
            break;
        }
    }
    Ok(if hasher_material.is_empty() {
        sha256_hex(root.display().to_string().as_bytes())
    } else {
        sha256_hex(&hasher_material)
    })
}

#[derive(Clone)]
struct CachedPluginHash {
    metadata_fingerprint: String,
    hashes: (String, String),
}

/// 只读 inventory 专用增量 hash；mutation 校验继续调用未缓存的 `hash_plugin_root`。
pub(super) fn hash_plugin_root_cached(root: &Path) -> Result<(String, String), AppError> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, CachedPluginHash>>> = OnceLock::new();
    let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let metadata_fingerprint =
        crate::agent_hub::targets::tree_metadata::tree_metadata_fingerprint(root)?;
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&key)
        .filter(|entry| entry.metadata_fingerprint == metadata_fingerprint)
        .cloned()
    {
        return Ok(hit.hashes);
    }
    let hashes = hash_plugin_root(root)?;
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= 512 && !guard.contains_key(&key) {
        guard.clear();
    }
    guard.insert(
        key,
        CachedPluginHash {
            metadata_fingerprint,
            hashes: hashes.clone(),
        },
    );
    Ok(hashes)
}
