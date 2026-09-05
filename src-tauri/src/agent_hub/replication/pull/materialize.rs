//! agent_hub/replication/pull/materialize — CAS 树安全物化落盘
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Plugin 目录资产的 replaceAfterPreview 安装必须整体替换旧目录（不残留旧文件），
//!     且远端传来的 tree entry 路径不可信任：相对路径逃逸、symlink 中间组件都要 fail-closed。
//!
//! Code Logic（这个模块做什么）:
//!     safe_tree_dest 路径校验（拒绝绝对路径/../symlink 组件）；
//!     materialize_tree_atomic_replace 先写临时 staging 目录再原子 rename 覆盖 dest；
//!     apply_executable_bit 按 TreeManifest 还原 Unix +x。

use crate::agent_hub::object_store::{ObjectStore, TreeEntryType};
use crate::error::AppError;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

/// 校验 tree entry 路径安全：相对、无 `..`、无绝对路径，最终仍在 `dir` 下；
/// 且 dest 上每一级既有路径组件不得是 symlink（防 dir/assets→/tmp/outside 逃逸）。
pub(super) fn safe_tree_dest(dir: &Path, entry_path: &str) -> Result<PathBuf, AppError> {
    let rel = Path::new(entry_path);
    if rel.is_absolute() {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    for c in rel.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir => {}
            _ => {
                return Err(AppError::validation(
                    "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
                ));
            }
        }
    }
    if entry_path.contains('\0') {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    let dest = dir.join(rel);
    // 前缀检查：在 create 前用逻辑路径判断（dir 未必已 canonicalize）
    let dir_norm = dir.components().collect::<Vec<_>>();
    let dest_norm = dest.components().collect::<Vec<_>>();
    if dest_norm.len() < dir_norm.len() || dest_norm[..dir_norm.len()] != dir_norm[..] {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    // no-follow：拒绝 dest 路径上任何既有 symlink 组件（含中间目录）
    refuse_symlink_components_under(dir, rel)?;
    Ok(dest)
}

/// 从 `dir` 起沿 `rel` 逐级 `symlink_metadata`，任一级已是 symlink → fail-closed。
fn refuse_symlink_components_under(dir: &Path, rel: &Path) -> Result<(), AppError> {
    let mut cur = dir.to_path_buf();
    // 目标根本身若是 symlink，写盘会跟随逃逸
    if path_is_symlink(&cur) {
        return Err(AppError::validation(
            "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
        ));
    }
    for c in rel.components() {
        let Component::Normal(name) = c else {
            continue;
        };
        cur.push(name);
        if path_is_symlink(&cur) {
            return Err(AppError::validation(
                "PORTABLE_PULL_UNSAFE_TREE_PATH".to_string(),
            ));
        }
    }
    Ok(())
}

fn path_is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Materialize a CAS tree into a fresh temp dir under parent, then atomically replace `dest`.
///
/// Business Logic: replaceAfterPreview must not leave stale files from the old tree.
/// Code Logic: write into `.cc-partner-pull-staging-*`, restore executable bits, rename over dest.
pub(super) async fn materialize_tree_atomic_replace(
    store: &ObjectStore,
    dest: &Path,
    manifest: &crate::agent_hub::object_store::TreeManifest,
) -> Result<(), AppError> {
    let parent = dest
        .parent()
        .ok_or_else(|| AppError::validation("PORTABLE_PULL_INSTALL_DEST_INVALID".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".cc-partner-pull-staging-{}", Uuid::now_v7()));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let cleanup = |path: &Path| {
        if path.exists() {
            let _ = std::fs::remove_dir_all(path);
        }
    };
    if let Err(e) = materialize_tree_into(store, &staging, manifest).await {
        cleanup(&staging);
        return Err(e);
    }
    // Replace dest: move old aside then rename staging → dest; restore old on failure.
    let backup = if dest.exists() {
        let b = parent.join(format!(".cc-partner-pull-backup-{}", Uuid::now_v7()));
        if let Err(e) = std::fs::rename(dest, &b) {
            cleanup(&staging);
            return Err(AppError::from(e));
        }
        Some(b)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(&staging, dest) {
        cleanup(&staging);
        if let Some(b) = backup.as_ref() {
            let _ = std::fs::rename(b, dest);
        }
        return Err(AppError::from(e));
    }
    if let Some(b) = backup {
        let _ = std::fs::remove_dir_all(b);
    }
    Ok(())
}

/// Write tree entries under `dir` (must be empty/new), restoring executable bits.
async fn materialize_tree_into(
    store: &ObjectStore,
    dir: &Path,
    manifest: &crate::agent_hub::object_store::TreeManifest,
) -> Result<(), AppError> {
    std::fs::create_dir_all(dir)?;
    for entry in &manifest.entries {
        let dest = safe_tree_dest(dir, &entry.path)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match entry.entry_type {
            TreeEntryType::File => {
                let blob = store.get_blob(&entry.blob_hash).await?;
                std::fs::write(&dest, blob)?;
                apply_executable_bit(&dest, entry.executable)?;
            }
            TreeEntryType::Symlink => {
                // 不跟随外链；仅跳过
                let _ = entry;
            }
        }
    }
    Ok(())
}

/// Restore Unix +x from TreeManifest.executable; no-op on non-Unix.
pub(super) fn apply_executable_bit(path: &Path, executable: bool) -> Result<(), AppError> {
    if !executable {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)?;
        let mut perms = meta.permissions();
        let mode = perms.mode();
        // Preserve existing bits; ensure owner/group/other execute when any read is set,
        // but at minimum owner +x (0o111).
        perms.set_mode(mode | 0o111);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
