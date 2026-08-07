//! portable_actions/targets/claude — Claude Code 本机动作执行
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude Plugin 必须固定 `--scope` argv；Skill/Command 走安全 move+backup；
//!     MCP 走 ownership-aware semantic patch，保留 unmanaged 字段与注释。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `TargetActionExecutor`；CLI 经 ProcessRunner；文件 mutation 复用 crate-visible helpers。

use super::{
    is_outcome_unknown_error, map_process_outcome, TargetActionContext, TargetActionExecutor,
    TargetActionRawOutcome,
};
use crate::agent_hub::packages::activator::ProcessSpec;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
    PortableAssetBackupPolicy,
};
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::claude_code_assets::{
    portable_backup_path, portable_claude_roots, portable_move_path, portable_remove_path,
    portable_remove_tree_with_backup, portable_set_command_enabled, portable_set_tree_enabled,
    ClaudeCodeAssetKind, PortableClaudeRoots,
};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// Claude target executor。
pub struct ClaudeTargetExecutor;

impl TargetActionExecutor for ClaudeTargetExecutor {
    fn execute_change(
        &self,
        ctx: &TargetActionContext,
        _plan: &PortableAssetActionPlanDto,
        change: &PortableAssetActionChangeDto,
        pre_item: Option<&PortableInventoryItemDto>,
    ) -> Result<TargetActionRawOutcome, AppError> {
        if !change.blocking_reasons.is_empty() {
            return Ok(TargetActionRawOutcome::Blocked {
                code: change
                    .blocking_reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "PORTABLE_ASSET_ACTION_BLOCKED".into()),
                message: "plan change blocked".into(),
            });
        }

        // changed-source fail closed（mutation 前）
        if let Some(expected) = change.expected_source_hash.as_deref() {
            if let Some(path) = change.path.as_deref() {
                let p = Path::new(path);
                if p.exists() {
                    if let Ok(actual) = crate::claude_code_assets::portable_content_hash_path(p) {
                        if actual != expected {
                            return Ok(TargetActionRawOutcome::Failed {
                                code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                                message: "source content changed since preview".into(),
                            });
                        }
                    }
                }
            }
        }

        let roots =
            portable_claude_roots(ctx.claude_config_dir.as_deref(), ctx.data_dir.as_deref())?;
        match change.kind {
            PortableAssetKind::Plugin => execute_plugin(ctx, change, pre_item),
            PortableAssetKind::Skill => execute_skill(ctx, &roots, change, pre_item),
            PortableAssetKind::Command => execute_command(ctx, &roots, change, pre_item),
            PortableAssetKind::Mcp => execute_mcp(ctx, &roots, change, pre_item),
        }
    }
}

fn execute_plugin(
    ctx: &TargetActionContext,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let scope = scope_arg(pre_item);
    let program = PathBuf::from("claude");
    let args = match ctx.action {
        PortableAssetActionKind::Enable => {
            vec![
                "plugin".into(),
                "enable".into(),
                id.clone(),
                "--scope".into(),
                scope,
            ]
        }
        PortableAssetActionKind::Disable => {
            vec![
                "plugin".into(),
                "disable".into(),
                id.clone(),
                "--scope".into(),
                scope,
            ]
        }
        PortableAssetActionKind::Uninstall => {
            let mut a = vec![
                "plugin".into(),
                "uninstall".into(),
                id.clone(),
                "--scope".into(),
                scope,
            ];
            if ctx.keep_data {
                a.push("--keep-data".into());
            }
            a
        }
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => {
            return Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_PLUGIN_ACTION_UNSUPPORTED".into(),
                message: format!("plugin action {} not executed here", ctx.action.as_str()),
            });
        }
    };
    match ctx.runner.run(&ProcessSpec { program, args }) {
        Ok(out) => Ok(map_process_outcome(out, "claude plugin")),
        Err(e) if is_outcome_unknown_error(&e) => Ok(TargetActionRawOutcome::OutcomeUnknown {
            code: "PORTABLE_ASSET_ACTION_SPAWN_UNKNOWN".into(),
            message: e.to_string(),
        }),
        Err(e) => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_CLI_ERROR".into(),
            message: e.to_string(),
        }),
    }
}

fn execute_skill(
    ctx: &TargetActionContext,
    roots: &PortableClaudeRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    match ctx.action {
        PortableAssetActionKind::Enable => {
            portable_set_tree_enabled(
                ClaudeCodeAssetKind::Skill,
                &id,
                true,
                &roots.skills_dir,
                &roots.disabled_skills_dir,
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Disable => {
            portable_set_tree_enabled(
                ClaudeCodeAssetKind::Skill,
                &id,
                false,
                &roots.skills_dir,
                &roots.disabled_skills_dir,
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Uninstall => {
            if change.backup_policy == PortableAssetBackupPolicy::RecoverableBeforeDelete
                || !ctx.keep_data
            {
                portable_remove_tree_with_backup(
                    ClaudeCodeAssetKind::Skill,
                    &id,
                    &[
                        roots.skills_dir.join(&id),
                        roots.disabled_skills_dir.join(&id),
                    ],
                    &roots.backup_root,
                )?;
            } else if let Some(path) = change.path.as_ref().map(PathBuf::from) {
                // keep_data：仅移入 disabled，不删
                if path.exists() {
                    let dest = roots.disabled_skills_dir.join(&id);
                    if dest.exists() {
                        portable_backup_path(
                            ClaudeCodeAssetKind::Skill,
                            &id,
                            &dest,
                            &roots.backup_root,
                        )?;
                        portable_remove_path(&dest)?;
                    }
                    portable_move_path(&path, &dest)?;
                }
            }
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Applied),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget deferred".into(),
        }),
    }
}

fn execute_command(
    ctx: &TargetActionContext,
    roots: &PortableClaudeRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    match ctx.action {
        PortableAssetActionKind::Enable => {
            portable_set_command_enabled(
                &id,
                true,
                &roots.commands_dir,
                &roots.disabled_commands_dir,
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Disable => {
            portable_set_command_enabled(
                &id,
                false,
                &roots.commands_dir,
                &roots.disabled_commands_dir,
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Uninstall => {
            let file = format!("{id}.md");
            portable_remove_tree_with_backup(
                ClaudeCodeAssetKind::Command,
                &id,
                &[
                    roots.commands_dir.join(&file),
                    roots.disabled_commands_dir.join(&file),
                ],
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Applied),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget deferred".into(),
        }),
    }
}

fn execute_mcp(
    ctx: &TargetActionContext,
    roots: &PortableClaudeRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    use crate::agent_hub::config_patch::{
        apply_config_patch_atomically, JsoncConfigPatcher, ManagedConfigPatch,
    };

    let id = native_id(change, pre_item);
    let config_path = roots.claude_json_path.clone();
    let bytes = if config_path.exists() {
        fs::read(&config_path)?
    } else {
        b"{}".to_vec()
    };

    match ctx.action {
        PortableAssetActionKind::Enable => {
            // 优先从 disabled 备份恢复原文（保留 credentials 字节）
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            let value = if disabled.exists() {
                let text = fs::read_to_string(&disabled)?;
                serde_json::from_str(&text)?
            } else if let Some(path) = change.path.as_ref() {
                // 已在文件中则 skip
                let _ = path;
                return Ok(TargetActionRawOutcome::Skipped);
            } else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_DISABLED_MISSING".into(),
                    message: "disabled MCP snapshot missing".into(),
                });
            };
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: Some(value),
                expected_base_hash: None,
            }];
            let prepared =
                apply_config_patch_atomically(&JsoncConfigPatcher, &config_path, &patches)?;
            if !matches!(
                prepared.patched.outcome,
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied
            ) {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED".into(),
                    message: format!("{:?}", prepared.patched.outcome),
                });
            }
            let _ = fs::remove_file(disabled);
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Disable => {
            // 读当前值，写 disabled，再 semantic remove
            let current = read_mcp_value(&bytes, &id)?;
            let Some(cfg) = current else {
                return Ok(TargetActionRawOutcome::Skipped);
            };
            fs::create_dir_all(&roots.disabled_mcp_dir)?;
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            if disabled.exists() {
                portable_backup_path(ClaudeCodeAssetKind::Mcp, &id, &disabled, &roots.backup_root)?;
            }
            // 原文写入 disabled（含 credentials；不进 DTO/日志）
            fs::write(&disabled, serde_json::to_vec_pretty(&cfg)?)?;
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: None,
                expected_base_hash: change.expected_source_hash.clone(),
            }];
            let prepared =
                apply_config_patch_atomically(&JsoncConfigPatcher, &config_path, &patches)?;
            match prepared.patched.outcome {
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
                    Ok(TargetActionRawOutcome::Applied)
                }
                crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
                    Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT".into(),
                        message: "mcp semantic patch CAS conflict".into(),
                    })
                }
                crate::agent_hub::config_patch::ConfigPatchOutcome::Blocked { reason } => {
                    Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_PATCH_BLOCKED".into(),
                        message: reason,
                    })
                }
            }
        }
        PortableAssetActionKind::Uninstall => {
            if let Ok(Some(cfg)) = read_mcp_value(&bytes, &id) {
                fs::create_dir_all(&roots.backup_root)?;
                let dst = roots
                    .backup_root
                    .join(chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string())
                    .join("mcp")
                    .join(format!("{id}.json"));
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(dst, serde_json::to_vec_pretty(&cfg)?)?;
            }
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: None,
                expected_base_hash: None,
            }];
            let prepared =
                apply_config_patch_atomically(&JsoncConfigPatcher, &config_path, &patches)?;
            let _ = fs::remove_file(roots.disabled_mcp_dir.join(format!("{id}.json")));
            if matches!(
                prepared.patched.outcome,
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied
            ) {
                Ok(TargetActionRawOutcome::Applied)
            } else {
                Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_UNINSTALL_FAILED".into(),
                    message: format!("{:?}", prepared.patched.outcome),
                })
            }
        }
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Applied),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget deferred".into(),
        }),
    }
}

fn read_mcp_value(bytes: &[u8], id: &str) -> Result<Option<serde_json::Value>, AppError> {
    use crate::agent_hub::config_patch::{JsoncConfigPatcher, SemanticConfigPatcher};
    let patcher = JsoncConfigPatcher;
    let owned = patcher.inspect(bytes, &["mcpServers".into(), id.to_string()])?;
    if owned.present {
        Ok(Some(owned.value))
    } else {
        Ok(None)
    }
}

fn native_id(
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> String {
    pre_item
        .map(|i| i.native_id.clone())
        .or_else(|| {
            change.path.as_ref().and_then(|p| {
                Path::new(p)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
        })
        .unwrap_or_else(|| change.inventory_item_id.clone())
}

fn scope_arg(pre_item: Option<&PortableInventoryItemDto>) -> String {
    match pre_item.map(|i| i.scope_kind) {
        Some(crate::agent_hub::models::ScopeKind::Project) => "project".into(),
        _ => "user".into(),
    }
}
