//! portable_store/actions — Claude/Codex Skill/Command store 附加/卸下/迁移/销毁
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command 禁用不得 MOVE store 真树；MCP 不进仓库，启停仍改各家配置 leaf。
//!     彻底删除才清 store 真树与剩余软链。
//!
//! Code Logic（这个模块做什么）:
//!     建/拆软链；destroy 清 Claude/Codex 链。

use super::{
    attach_store_link, classify_store_link, current_portable_store_root,
    ensure_portable_store_layout, migrate_native_into_store, remove_manifest_attachment,
    remove_manifest_entry, store_command_file, store_id_for, store_skill_dir, unlink_if_store_link,
    upsert_manifest_entry, ManifestAttachment, PortableStoreKind, StoreLinkClass,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
use crate::agent_hub::portable_actions::targets::TargetActionRawOutcome;
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::agent_hub::targets::portable::{hash_skill_directory, parse_simple_frontmatter};
use crate::error::AppError;
use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 在 viewing Agent 的 native 根上执行 store Skill/Command 动作。
///
/// Business Logic: Enable/Attach 建链；Disable/Detach/Uninstall 只拆链；Destroy 删真树。
/// Code Logic: 按 kind 解析 store 路径与 native 挂载点。
pub fn execute_skill_or_command_store(
    viewing: AgentTarget,
    action: PortableAssetActionKind,
    kind: PortableAssetKind,
    native_id: &str,
    native_path: &Path,
    item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let data_dir = crate::config::data_dir()?;
    let store_root = ensure_portable_store_layout(&data_dir)?;
    let store_kind = match kind {
        PortableAssetKind::Skill => PortableStoreKind::Skill,
        PortableAssetKind::Command => PortableStoreKind::Command,
        _ => {
            return Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_STORE_KIND_UNSUPPORTED".into(),
                message: "store skill/command only".into(),
            });
        }
    };
    let store_target = match store_kind {
        PortableStoreKind::Skill => store_skill_dir(&store_root, native_id),
        PortableStoreKind::Command => store_command_file(&store_root, native_id),
        PortableStoreKind::Mcp => {
            return Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_STORE_KIND_UNSUPPORTED".into(),
                message: "store skill/command only".into(),
            });
        }
    };
    let store_id = item
        .and_then(|i| i.store.store_id.clone())
        .unwrap_or_else(|| store_id_for(store_kind, native_id));

    match action {
        PortableAssetActionKind::Enable | PortableAssetActionKind::Attach => {
            if !store_target.exists() {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_STORE_TARGET_MISSING".into(),
                    message: "store tree missing".into(),
                });
            }
            attach_store_link(&store_target, native_path)?;
            let _ = upsert_manifest_entry(
                &store_root,
                store_kind,
                native_id,
                item.and_then(|i| i.content_hash.clone()),
                Some(ManifestAttachment {
                    target: viewing,
                    path: native_path.display().to_string(),
                }),
            );
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Disable
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::Uninstall => {
            if unlink_if_store_link(native_path)? {
                let _ = remove_manifest_attachment(&store_root, &store_id, viewing);
                return Ok(TargetActionRawOutcome::Applied);
            }
            if !native_path.exists() {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_STORE_DISABLE_NOT_A_LINK".into(),
                message: "refusing to move a real tree out of store".into(),
            })
        }
        PortableAssetActionKind::MigrateToStore => {
            if matches!(
                classify_store_link(native_path),
                StoreLinkClass::StoreLink { .. }
            ) {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            // Disabled 项的真树在 hub disabled 目录，native 挂载点是空的。
            // 先把 inventory source_path 搬到 native，再迁入 store，软链落在 Agent 原路径。
            let Some(source) = resolve_migrate_source(native_path, item) else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_SOURCE_MISSING".into(),
                    message: "native skill/command tree is missing".into(),
                });
            };
            if source != native_path {
                crate::claude_code_assets::portable_move_path(&source, native_path)?;
            }
            let native_won = if store_target.exists() {
                match resolve_migrate_name_conflict(native_path, &store_target, kind)? {
                    MigrateNameConflict::SameContent | MigrateNameConflict::KeepStore => {
                        remove_real_tree(native_path)?;
                        attach_store_link(&store_target, native_path)?;
                        false
                    }
                    MigrateNameConflict::KeepNative => {
                        remove_real_tree(&store_target)?;
                        migrate_native_into_store(native_path, &store_target)?;
                        true
                    }
                }
            } else {
                migrate_native_into_store(native_path, &store_target)?;
                true
            };
            let content_hash = if native_won {
                item.and_then(|i| i.content_hash.clone())
            } else {
                None
            };
            let _ = upsert_manifest_entry(
                &store_root,
                store_kind,
                native_id,
                content_hash,
                Some(ManifestAttachment {
                    target: viewing,
                    path: native_path.display().to_string(),
                }),
            );
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::DestroyStore => {
            let _ = unlink_if_store_link(native_path);
            destroy_remaining_skill_command_links(store_kind, native_id, &store_target);
            if store_target.is_dir() {
                fs::remove_dir_all(&store_target)?;
            } else if store_target.is_file() {
                fs::remove_file(&store_target)?;
            }
            let _ = remove_manifest_entry(&store_root, &store_id);
            Ok(TargetActionRawOutcome::Applied)
        }
        _ => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_STORE_ACTION_UNSUPPORTED".into(),
            message: "unsupported store skill/command action".into(),
        }),
    }
}

/// 迁入真树：优先 native 挂载点；否则用库存 `source_path`（disabled 副本）。
///
/// Business Logic: 已停用的 Skill/Command 真树在 hub disabled 目录，不在 Agent native 根。
///     迁入仍应成功，并在 native 路径留下 store 软链（与 Enable 后的挂载点一致）。
/// Code Logic: native 上已有非软链真树则用之；否则 `item.source_path` 存在且不是软链才采用。
fn resolve_migrate_source(
    native_path: &Path,
    item: Option<&PortableInventoryItemDto>,
) -> Option<PathBuf> {
    if exists_as_real_tree(native_path) {
        return Some(native_path.to_path_buf());
    }
    let source = item.and_then(|i| i.source_path.as_deref()).map(Path::new)?;
    if source != native_path && exists_as_real_tree(source) {
        Some(source.to_path_buf())
    } else {
        None
    }
}

/// 路径上是否有可迁入的真文件/目录（不是软链、不是缺失）。
fn exists_as_real_tree(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .is_some_and(|meta| !meta.file_type().is_symlink() && (meta.is_dir() || meta.is_file()))
}

/// 同名迁入冲突的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrateNameConflict {
    /// 内容相同，只把 native 换成软链
    SameContent,
    /// native 更新，覆盖仓库真树
    KeepNative,
    /// 仓库已有更新副本，丢掉 native 真树
    KeepStore,
}

/// 同名不同内容时保留版本较新的一份；无版本则比 mtime；旧树直接删除。
///
/// Business Logic: 一键迁入不得因同名阻断；用户要的是本机一份最新真树。
/// Code Logic: hash 相同 → SameContent；双方 frontmatter version 可比则更高者赢；否则 mtime，并列偏 native。
fn resolve_migrate_name_conflict(
    native: &Path,
    store_dest: &Path,
    kind: PortableAssetKind,
) -> Result<MigrateNameConflict, AppError> {
    let native_fp = content_fingerprint(native, kind)?;
    if let Ok(store_fp) = content_fingerprint(store_dest, kind) {
        if store_fp == native_fp {
            return Ok(MigrateNameConflict::SameContent);
        }
    }
    let native_ver = asset_frontmatter_version(native, kind);
    let store_ver = asset_frontmatter_version(store_dest, kind);
    if let (Some(native_ver), Some(store_ver)) = (native_ver.as_deref(), store_ver.as_deref()) {
        match cmp_dot_version(native_ver, store_ver) {
            Ordering::Greater => return Ok(MigrateNameConflict::KeepNative),
            Ordering::Less => return Ok(MigrateNameConflict::KeepStore),
            Ordering::Equal => {}
        }
    }
    let native_mtime = newest_mtime(native)?;
    let store_mtime = newest_mtime(store_dest)?;
    if native_mtime >= store_mtime {
        Ok(MigrateNameConflict::KeepNative)
    } else {
        Ok(MigrateNameConflict::KeepStore)
    }
}

/// Skill 用目录树 hash，Command 用文件字节 hash。
fn content_fingerprint(path: &Path, kind: PortableAssetKind) -> Result<String, AppError> {
    match kind {
        PortableAssetKind::Skill => hash_skill_directory(path).map(|(_, tree, _, _)| tree),
        PortableAssetKind::Command => {
            let bytes = fs::read(path)?;
            Ok(sha256_hex(&bytes))
        }
        _ => Err(AppError::validation(
            "PORTABLE_STORE_KIND_UNSUPPORTED".to_string(),
        )),
    }
}

/// 读 SKILL.md / command markdown 的 frontmatter `version`。
fn asset_frontmatter_version(path: &Path, kind: PortableAssetKind) -> Option<Vec<u64>> {
    let md = match kind {
        PortableAssetKind::Skill => path.join("SKILL.md"),
        PortableAssetKind::Command => path.to_path_buf(),
        _ => return None,
    };
    let text = fs::read_to_string(md).ok()?;
    let (fields, _, _) = parse_simple_frontmatter(&text);
    parse_dot_version(fields.get("version")?)
}

/// `1.2.3` / `v1.2` 切成数字段；非数字则 None。
fn parse_dot_version(raw: &str) -> Option<Vec<u64>> {
    let trimmed = raw.trim().trim_start_matches(['v', 'V']);
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in trimmed.split('.') {
        parts.push(part.parse::<u64>().ok()?);
    }
    Some(parts)
}

fn cmp_dot_version(left: &[u64], right: &[u64]) -> Ordering {
    let len = left.len().max(right.len());
    for i in 0..len {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        match a.cmp(&b) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// 不跟随 symlink 的最新 mtime（目录取子文件最大值）。
fn newest_mtime(path: &Path) -> Result<SystemTime, AppError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(meta.modified().unwrap_or(UNIX_EPOCH));
    }
    if meta.is_file() {
        return Ok(meta.modified().unwrap_or(UNIX_EPOCH));
    }
    let mut best = meta.modified().unwrap_or(UNIX_EPOCH);
    walk_newest_mtime(path, &mut best)?;
    Ok(best)
}

fn walk_newest_mtime(dir: &Path, best: &mut SystemTime) -> Result<(), AppError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(AppError::from(err)),
    };
    for entry in entries {
        let entry = entry?;
        let child = entry.path();
        let meta = fs::symlink_metadata(&child)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if modified > *best {
            *best = modified;
        }
        if meta.is_dir() {
            walk_newest_mtime(&child, best)?;
        }
    }
    Ok(())
}

/// 删除真文件/目录；拒绝跟随 symlink，避免误清 store 或逃逸目标。
fn remove_real_tree(path: &Path) -> Result<(), AppError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(AppError::from(err)),
    };
    if meta.file_type().is_symlink() {
        return Err(AppError::validation(
            "PORTABLE_STORE_REFUSE_REPLACE_SYMLINK".to_string(),
        ));
    }
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// 当前路径是否应按 store 语义处理（已是链，或动作是 store 专用）。
pub fn should_use_store_semantics(
    action: PortableAssetActionKind,
    path: Option<&Path>,
    item: Option<&PortableInventoryItemDto>,
) -> bool {
    if action.is_portable_store_action() {
        return true;
    }
    if item.and_then(|i| i.store.store_id.as_ref()).is_some() {
        return true;
    }
    path.is_some_and(|p| matches!(classify_store_link(p), StoreLinkClass::StoreLink { .. }))
}

fn destroy_remaining_skill_command_links(
    kind: PortableStoreKind,
    native_id: &str,
    store_target: &Path,
) {
    let Ok(canonical) = fs::canonicalize(store_target) else {
        return;
    };
    let mut candidates = Vec::new();
    if let Ok(roots) = crate::claude_code_assets::portable_claude_roots(None, None) {
        match kind {
            PortableStoreKind::Skill => candidates.push(roots.skills_dir.join(native_id)),
            PortableStoreKind::Command => {
                candidates.push(roots.commands_dir.join(format!("{native_id}.md")))
            }
            PortableStoreKind::Mcp => {}
        }
    }
    if let Some(home) = dirs::home_dir() {
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        match kind {
            PortableStoreKind::Skill => {
                candidates.push(codex.join("skills").join(native_id));
                candidates.push(home.join(".agents").join("skills").join(native_id));
            }
            PortableStoreKind::Command => {
                candidates.push(codex.join("commands").join(format!("{native_id}.md")));
            }
            PortableStoreKind::Mcp => {}
        }
    }
    for path in candidates {
        if let Ok(existing) = fs::canonicalize(&path) {
            if existing == canonical {
                let _ = unlink_if_store_link(&path);
            }
        }
    }
    let _ = current_portable_store_root();
}
