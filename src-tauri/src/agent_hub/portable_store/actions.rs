//! portable_store/actions — Claude/Codex store 附加/卸下/迁移/销毁
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command 禁用不得 MOVE store 真树；MCP 卸下只改 viewing Agent leaf；
//!     彻底删除才清 store 与 Claude/Codex 附加。
//!
//! Code Logic（这个模块做什么）:
//!     建/拆软链；MCP 用现有 JSONC/TOML patcher；destroy 清 Claude+Codex。

use super::{
    attach_store_link, classify_store_link, current_portable_store_root,
    ensure_portable_store_layout, migrate_native_into_store, read_mcp_store_json,
    remove_manifest_attachment, remove_manifest_entry, store_command_file, store_id_for,
    store_mcp_file, store_skill_dir, unlink_if_store_link, upsert_manifest_entry,
    write_mcp_store_json, ManifestAttachment, PortableStoreKind, StoreLinkClass,
};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, value_content_hash, JsoncConfigPatcher, ManagedConfigPatch,
    SemanticConfigPatcher, TomlConfigPatcher, CAS_EXPECT_ABSENT,
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
        PortableStoreKind::Mcp => store_mcp_file(&store_root, native_id),
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

/// MCP：附加/卸下只改 viewing leaf；迁移复制进 store；销毁清 Claude+Codex。
pub fn execute_mcp_store(
    viewing: AgentTarget,
    action: PortableAssetActionKind,
    native_id: &str,
    viewing_config: &Path,
    viewing_is_toml: bool,
    item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let data_dir = crate::config::data_dir()?;
    let store_root = ensure_portable_store_layout(&data_dir)?;
    let store_file = store_mcp_file(&store_root, native_id);
    let store_id = store_id_for(PortableStoreKind::Mcp, native_id);
    let key = if viewing_is_toml {
        vec!["mcp_servers".into(), native_id.to_string()]
    } else {
        vec!["mcpServers".into(), native_id.to_string()]
    };

    match action {
        PortableAssetActionKind::Enable | PortableAssetActionKind::Attach => {
            let value = read_mcp_store_json(&store_file)?;
            upsert_mcp_leaf(viewing_config, viewing_is_toml, &key, Some(value))?;
            let _ = upsert_manifest_entry(
                &store_root,
                PortableStoreKind::Mcp,
                native_id,
                item.and_then(|i| i.content_hash.clone()),
                Some(ManifestAttachment {
                    target: viewing,
                    path: viewing_config.display().to_string(),
                }),
            );
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Disable
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::Uninstall => {
            upsert_mcp_leaf(viewing_config, viewing_is_toml, &key, None)?;
            let _ = remove_manifest_attachment(&store_root, &store_id, viewing);
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::MigrateToStore => {
            if store_file.exists() {
                return Ok(TargetActionRawOutcome::Blocked {
                    code: "PORTABLE_STORE_MIGRATE_NAME_CONFLICT".into(),
                    message: "store MCP id already exists".into(),
                });
            }
            let bytes = if viewing_config.exists() {
                fs::read(viewing_config)?
            } else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_STORE_MCP_CONFIG_MISSING".into(),
                    message: "viewing MCP config missing".into(),
                });
            };
            let current = inspect_mcp_leaf(&bytes, viewing_is_toml, &key)?;
            let Some(value) = current else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_STORE_MCP_LEAF_MISSING".into(),
                    message: "MCP leaf missing in viewing config".into(),
                });
            };
            write_mcp_store_json(&store_file, &value)?;
            let _ = upsert_manifest_entry(
                &store_root,
                PortableStoreKind::Mcp,
                native_id,
                Some(value_content_hash(&value)),
                Some(ManifestAttachment {
                    target: viewing,
                    path: viewing_config.display().to_string(),
                }),
            );
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::DestroyStore => {
            if store_file.exists() {
                fs::remove_file(&store_file)?;
            }
            clear_claude_and_codex_mcp_leaves(native_id)?;
            let _ = remove_manifest_entry(&store_root, &store_id);
            Ok(TargetActionRawOutcome::Applied)
        }
        _ => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_STORE_ACTION_UNSUPPORTED".into(),
            message: "unsupported store MCP action".into(),
        }),
    }
}

/// 当前路径是否应按 store 语义处理（已是链，或动作是 store 专用）。
pub fn should_use_store_semantics(
    action: PortableAssetActionKind,
    path: Option<&Path>,
    item: Option<&PortableInventoryItemDto>,
) -> bool {
    if matches!(
        action,
        PortableAssetActionKind::Attach
            | PortableAssetActionKind::Detach
            | PortableAssetActionKind::DestroyStore
            | PortableAssetActionKind::MigrateToStore
    ) {
        return true;
    }
    if item.and_then(|i| i.store.store_id.as_ref()).is_some() {
        return true;
    }
    path.is_some_and(|p| matches!(classify_store_link(p), StoreLinkClass::StoreLink { .. }))
}

fn upsert_mcp_leaf(
    config_path: &Path,
    is_toml: bool,
    key: &[String],
    value: Option<serde_json::Value>,
) -> Result<(), AppError> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !config_path.exists() {
        fs::write(config_path, if is_toml { b"" as &[u8] } else { b"{}" })?;
    }
    let expected = if value.is_some() {
        Some(CAS_EXPECT_ABSENT.to_string())
    } else {
        let bytes = fs::read(config_path)?;
        inspect_mcp_leaf(&bytes, is_toml, key)?
            .as_ref()
            .map(value_content_hash)
    };
    let patches = [ManagedConfigPatch {
        owner_id: format!("portable-store:{}", key.last().cloned().unwrap_or_default()),
        path: key.to_vec(),
        value,
        expected_base_hash: expected,
    }];
    if is_toml {
        apply_config_patch_atomically(&TomlConfigPatcher, config_path, &patches)?;
    } else {
        apply_config_patch_atomically(&JsoncConfigPatcher, config_path, &patches)?;
    }
    Ok(())
}

fn inspect_mcp_leaf(
    bytes: &[u8],
    is_toml: bool,
    key: &[String],
) -> Result<Option<serde_json::Value>, AppError> {
    let owned = if is_toml {
        TomlConfigPatcher.inspect(bytes, key)?
    } else {
        JsoncConfigPatcher.inspect(bytes, key)?
    };
    Ok(if owned.present {
        Some(owned.value)
    } else {
        None
    })
}

fn clear_claude_and_codex_mcp_leaves(native_id: &str) -> Result<(), AppError> {
    let claude = crate::claude_code_assets::portable_claude_roots(None, None)?;
    if claude.claude_json_path.exists() {
        let _ = upsert_mcp_leaf(
            &claude.claude_json_path,
            false,
            &["mcpServers".into(), native_id.to_string()],
            None,
        );
    }
    if let Some(home) = dirs::home_dir() {
        let toml = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"))
            .join("config.toml");
        if toml.exists() {
            let _ = upsert_mcp_leaf(
                &toml,
                true,
                &["mcp_servers".into(), native_id.to_string()],
                None,
            );
        }
    }
    Ok(())
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
