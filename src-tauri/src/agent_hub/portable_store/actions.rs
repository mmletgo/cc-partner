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
use crate::agent_hub::portable_actions::models::PortableAssetActionKind;
use crate::agent_hub::portable_actions::targets::TargetActionRawOutcome;
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

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
            if store_target.exists() {
                return Ok(TargetActionRawOutcome::Blocked {
                    code: "PORTABLE_STORE_MIGRATE_NAME_CONFLICT".into(),
                    message: "store already has this id; same name different hash is blocked"
                        .into(),
                });
            }
            migrate_native_into_store(native_path, &store_target)?;
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
