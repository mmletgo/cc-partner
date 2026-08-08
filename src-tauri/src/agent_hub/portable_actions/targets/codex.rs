//! portable_actions/targets/codex — Codex CLI 本机动作执行
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex skill 位于 CODEX_HOME/skills 或 ~/.agents/skills；MCP 在 config.toml 的
//!     `mcp_servers`；plugins 在 CODEX_HOME/plugins。阶段一要求 certified pin 后真实写盘。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `TargetActionExecutor`：Skill/Command 用 active↔disabled move；
//!     MCP 用 TomlConfigPatcher semantic patch；Plugin 目录 uninstall+backup。

use super::{TargetActionContext, TargetActionExecutor, TargetActionRawOutcome};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, value_content_hash, ManagedConfigPatch, SemanticConfigPatcher,
    TomlConfigPatcher, CAS_EXPECT_ABSENT,
};
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
    PortableAssetBackupPolicy,
};
use crate::agent_hub::portable_inventory::{
    hash_plugin_root, PortableAssetKind, PortableInventoryItemDto,
};
use crate::agent_hub::targets::portable::hash_skill_directory;
use crate::claude_code_assets::{
    portable_backup_path, portable_move_path, portable_remove_path,
    portable_remove_tree_with_backup, portable_set_command_enabled, portable_set_tree_enabled,
    ClaudeCodeAssetKind,
};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// Codex target executor（文件 + TOML semantic patch）。
pub struct CodexTargetExecutor;

impl TargetActionExecutor for CodexTargetExecutor {
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

        if change.kind != PortableAssetKind::Mcp {
            if let Some(expected) = change.expected_source_hash.as_deref() {
                if let Some(path) = change.path.as_deref() {
                    let p = Path::new(path);
                    if p.exists() {
                        match inventory_content_hash_for_path(change.kind, p) {
                            Ok(actual) if actual != expected => {
                                return Ok(TargetActionRawOutcome::Failed {
                                    code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                                    message: "source content changed since preview".into(),
                                });
                            }
                            Err(_) => {
                                return Ok(TargetActionRawOutcome::Failed {
                                    code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_UNAVAILABLE".into(),
                                    message: "source content hash unavailable for recheck".into(),
                                });
                            }
                            Ok(_) => {}
                        }
                    }
                }
            }
        }

        let roots = resolve_codex_roots(ctx, pre_item, change)?;
        match change.kind {
            PortableAssetKind::Skill => execute_skill(ctx, &roots, change, pre_item),
            PortableAssetKind::Command => execute_command(ctx, &roots, change, pre_item),
            PortableAssetKind::Mcp => execute_mcp(ctx, &roots, change, pre_item),
            PortableAssetKind::Plugin => execute_plugin(ctx, &roots, change, pre_item),
        }
    }
}

/// Codex 本机 mutation 根目录集合。
#[derive(Debug, Clone)]
struct CodexRoots {
    skills_dir: PathBuf,
    commands_dir: PathBuf,
    disabled_skills_dir: PathBuf,
    disabled_commands_dir: PathBuf,
    disabled_mcp_dir: PathBuf,
    plugins_dir: PathBuf,
    backup_root: PathBuf,
    config_toml: PathBuf,
}

fn resolve_codex_roots(
    ctx: &TargetActionContext,
    pre_item: Option<&PortableInventoryItemDto>,
    change: &PortableAssetActionChangeDto,
) -> Result<CodexRoots, AppError> {
    let data = ctx
        .data_dir
        .clone()
        .unwrap_or_else(|| crate::config::config_dir().expect("config_dir"));
    let backup_root = data.join("codex-assets").join("backups");
    let hub_disabled = data.join("codex-assets").join("disabled");

    // Project scope: mutate under observed project roots.
    if let Some(item) = pre_item {
        if item.scope_kind == crate::agent_hub::models::ScopeKind::Project {
            if !item.project_opted_in {
                return Err(AppError::validation(
                    "PORTABLE_ASSET_ACTION_PROJECT_NOT_OPTED_IN".to_string(),
                ));
            }
            let source = item
                .source_path
                .as_deref()
                .or(change.path.as_deref())
                .map(Path::new);
            let project_root = infer_project_root(source).ok_or_else(|| {
                AppError::validation("PORTABLE_ASSET_ACTION_PROJECT_ROOT_UNRESOLVED".to_string())
            })?;
            let codex_dir = project_root.join(".codex");
            let agents_skills = project_root.join(".agents").join("skills");
            return Ok(CodexRoots {
                skills_dir: agents_skills,
                commands_dir: codex_dir.join("commands"),
                disabled_skills_dir: codex_dir.join("disabled").join("skills"),
                disabled_commands_dir: codex_dir.join("disabled").join("commands"),
                disabled_mcp_dir: codex_dir.join("disabled").join("mcp"),
                plugins_dir: codex_dir.join("plugins"),
                backup_root,
                config_toml: codex_dir.join("config.toml"),
            });
        }
    }

    // User scope: CODEX_HOME / ~/.codex；skills 兼容 ~/.agents/skills 与 CODEX_HOME/skills。
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            // Prefer config parent of observed source path when available.
            change.path.as_deref().map(Path::new).and_then(|p| {
                p.ancestors()
                    .find(|a| {
                        a.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n == ".codex" || n == "codex")
                    })
                    .map(Path::to_path_buf)
            })
        })
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codex")
        });

    let skills_dir = if let Some(path) = change.path.as_deref().map(Path::new) {
        // Prefer active skills root under CODEX_HOME (…/skills/<id>), but never treat
        // hub `…/disabled/skills` as the active skills_dir.
        path.ancestors()
            .find(|a| {
                let name = a.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name != "skills" {
                    return false;
                }
                let parent_is_disabled = a
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    == Some("disabled");
                !parent_is_disabled
            })
            .map(Path::to_path_buf)
            .unwrap_or_else(|| codex_home.join("skills"))
    } else {
        codex_home.join("skills")
    };

    Ok(CodexRoots {
        skills_dir,
        commands_dir: codex_home.join("commands"),
        disabled_skills_dir: hub_disabled.join("skills"),
        disabled_commands_dir: hub_disabled.join("commands"),
        disabled_mcp_dir: hub_disabled.join("mcp"),
        plugins_dir: codex_home.join("plugins"),
        backup_root,
        config_toml: codex_home.join("config.toml"),
    })
}

fn infer_project_root(source: Option<&Path>) -> Option<PathBuf> {
    let mut cur = source.map(|p| {
        if p.is_file() {
            p.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| p.to_path_buf())
        } else {
            p.to_path_buf()
        }
    })?;
    loop {
        if cur.join(".git").exists() || cur.join(".codex").is_dir() || cur.join(".agents").is_dir()
        {
            return Some(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

fn inventory_content_hash_for_path(
    kind: PortableAssetKind,
    path: &Path,
) -> Result<String, AppError> {
    match kind {
        PortableAssetKind::Skill => {
            let dir = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };
            let (skill_hash, _, _, _) = hash_skill_directory(&dir)?;
            Ok(skill_hash)
        }
        PortableAssetKind::Plugin => {
            let dir = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .filter(|p| p.is_dir())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };
            if dir.is_dir() {
                let (content_hash, _) = hash_plugin_root(&dir)?;
                Ok(content_hash)
            } else if dir.is_file() {
                Ok(sha256_hex(&fs::read(dir)?))
            } else {
                Err(AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"))
            }
        }
        PortableAssetKind::Command => {
            if path.is_file() {
                Ok(sha256_hex(&fs::read(path)?))
            } else {
                Err(AppError::validation(
                    "PORTABLE_ASSET_ACTION_COMMAND_DIR_HASH_UNSUPPORTED",
                ))
            }
        }
        PortableAssetKind::Mcp => Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_MCP_HASH_VIA_LEAF",
        )),
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

fn execute_skill(
    ctx: &TargetActionContext,
    roots: &CodexRoots,
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
                if path.exists() {
                    let dest = roots.disabled_skills_dir.join(&id);
                    fs::create_dir_all(&roots.disabled_skills_dir)?;
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
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
                message: "adopt/install not wired for codex".into(),
            })
        }
    }
}

fn execute_command(
    ctx: &TargetActionContext,
    roots: &CodexRoots,
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
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
                message: "adopt/install not wired for codex".into(),
            })
        }
    }
}

fn execute_mcp(
    ctx: &TargetActionContext,
    roots: &CodexRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let config_path = roots.config_toml.clone();
    let bytes = if config_path.exists() {
        fs::read(&config_path)?
    } else {
        b"".to_vec()
    };
    let patcher = TomlConfigPatcher;

    match ctx.action {
        PortableAssetActionKind::Enable => {
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            let value = if disabled.exists() {
                let text = fs::read_to_string(&disabled)?;
                serde_json::from_str(&text)?
            } else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_DISABLED_MISSING".into(),
                    message: "disabled MCP snapshot missing".into(),
                });
            };
            let owned = patcher.inspect(&bytes, &["mcp_servers".into(), id.clone()])?;
            if owned.present {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable-codex:{id}"),
                path: vec!["mcp_servers".into(), id.clone()],
                value: Some(value),
                expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
            }];
            let prepared = apply_config_patch_atomically(&patcher, &config_path, &patches)?;
            match prepared.patched.outcome {
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
                    let _ = fs::remove_file(disabled);
                    Ok(TargetActionRawOutcome::Applied)
                }
                crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
                    Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT".into(),
                        message: "mcp enable CAS conflict".into(),
                    })
                }
                other => Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED".into(),
                    message: format!("{other:?}"),
                }),
            }
        }
        PortableAssetActionKind::Disable => {
            let owned = patcher.inspect(&bytes, &["mcp_servers".into(), id.clone()])?;
            if !owned.present {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            let leaf_hash = value_content_hash(&owned.value);
            if let Some(expected) = change.expected_source_hash.as_deref() {
                if expected != leaf_hash {
                    return Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                        message: "mcp leaf hash changed since preview".into(),
                    });
                }
            }
            fs::create_dir_all(&roots.disabled_mcp_dir)?;
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            if disabled.exists() {
                portable_backup_path(ClaudeCodeAssetKind::Mcp, &id, &disabled, &roots.backup_root)?;
            }
            fs::write(&disabled, serde_json::to_vec_pretty(&owned.value)?)?;
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable-codex:{id}"),
                path: vec!["mcp_servers".into(), id.clone()],
                value: None,
                expected_base_hash: Some(leaf_hash),
            }];
            let prepared = apply_config_patch_atomically(&patcher, &config_path, &patches)?;
            match prepared.patched.outcome {
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
                    Ok(TargetActionRawOutcome::Applied)
                }
                crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
                    Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT".into(),
                        message: "mcp disable CAS conflict".into(),
                    })
                }
                other => Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED".into(),
                    message: format!("{other:?}"),
                }),
            }
        }
        PortableAssetActionKind::Uninstall => {
            let owned = patcher.inspect(&bytes, &["mcp_servers".into(), id.clone()])?;
            if owned.present {
                let leaf_hash = value_content_hash(&owned.value);
                if let Some(expected) = change.expected_source_hash.as_deref() {
                    if expected != leaf_hash {
                        return Ok(TargetActionRawOutcome::Failed {
                            code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                            message: "mcp leaf hash changed since preview".into(),
                        });
                    }
                }
                if !ctx.keep_data {
                    fs::create_dir_all(&roots.backup_root)?;
                    let snap = roots.backup_root.join(format!("mcp-{id}.json"));
                    fs::write(&snap, serde_json::to_vec_pretty(&owned.value)?)?;
                }
                let patches = [ManagedConfigPatch {
                    owner_id: format!("portable-codex:{id}"),
                    path: vec!["mcp_servers".into(), id.clone()],
                    value: None,
                    expected_base_hash: Some(leaf_hash),
                }];
                let prepared = apply_config_patch_atomically(&patcher, &config_path, &patches)?;
                if !matches!(
                    prepared.patched.outcome,
                    crate::agent_hub::config_patch::ConfigPatchOutcome::Applied
                ) {
                    return Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_UNINSTALL_FAILED".into(),
                        message: format!("{:?}", prepared.patched.outcome),
                    });
                }
            }
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            if disabled.exists() {
                let _ = fs::remove_file(disabled);
            }
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
                message: "adopt/install not wired for codex".into(),
            })
        }
    }
}

fn execute_plugin(
    ctx: &TargetActionContext,
    roots: &CodexRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let path = change
        .path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| roots.plugins_dir.join(&id));
    match ctx.action {
        PortableAssetActionKind::Enable | PortableAssetActionKind::Disable => {
            // Codex plugins 无统一 CLI enable；以目录存在为 installed，无独立 disabled 语义时
            // enable/disable 以 no-op skip 诚实处理（避免伪成功）。
            if path.exists() {
                Ok(TargetActionRawOutcome::Skipped)
            } else {
                Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_PLUGIN_MISSING".into(),
                    message: "plugin path missing".into(),
                })
            }
        }
        PortableAssetActionKind::Uninstall => {
            if !path.exists() {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            portable_remove_tree_with_backup(
                ClaudeCodeAssetKind::Plugin,
                &id,
                &[path],
                &roots.backup_root,
            )?;
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Adopt | PortableAssetActionKind::InstallToSourceTarget => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
                message: "adopt/install not wired for codex".into(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, ScopeKind};
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    use crate::agent_hub::portable_actions::models::{
        PortableAssetCanonicalEffect, PortableAssetConflictPolicy, PortableAssetPlanOperation,
    };
    use crate::agent_hub::portable_inventory::{
        PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    fn empty_plan(
        action: PortableAssetActionKind,
        changes: Vec<PortableAssetActionChangeDto>,
    ) -> PortableAssetActionPlanDto {
        PortableAssetActionPlanDto {
            plan_token: "t".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            inventory_snapshot_hash: "h".into(),
            action,
            keep_data: false,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            changes,
            blocking_reasons: vec![],
        }
    }

    fn base_change(
        kind: PortableAssetKind,
        id: &str,
        path: &str,
        operation: PortableAssetPlanOperation,
    ) -> PortableAssetActionChangeDto {
        PortableAssetActionChangeDto {
            inventory_item_id: id.into(),
            target: AgentTarget::Codex,
            kind,
            path: Some(path.into()),
            operation,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::RecoverableBeforeDelete,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        }
    }

    fn sample_item(
        kind: PortableAssetKind,
        native_id: &str,
        path: &str,
    ) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: format!("id-{native_id}"),
            target: AgentTarget::Codex,
            kind,
            native_id: native_id.into(),
            display_name: native_id.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(path.into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: None,
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto::default(),
            warnings: vec![],
            mcp_credential: None,
        }
    }

    #[test]
    fn codex_skill_disable_moves_to_disabled_and_enable_restores() {
        let tmp = TempDir::new().unwrap();
        let skills = tmp.path().join("skills");
        let skill_dir = skills.join("demo-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# demo\n").unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Skill,
            "demo-skill",
            &skill_dir.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Skill,
            "id-demo-skill",
            &skill_dir.to_string_lossy(),
            PortableAssetPlanOperation::Disable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir.clone()),
            keep_data: false,
            action: PortableAssetActionKind::Disable,
        };
        let plan = empty_plan(PortableAssetActionKind::Disable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        assert!(!skill_dir.exists());
        let disabled = data_dir
            .join("codex-assets")
            .join("disabled")
            .join("skills")
            .join("demo-skill");
        assert!(disabled.is_dir());

        let enable_change = PortableAssetActionChangeDto {
            operation: PortableAssetPlanOperation::Enable,
            path: Some(disabled.to_string_lossy().into()),
            ..change
        };
        let ctx_en = TargetActionContext {
            action: PortableAssetActionKind::Enable,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
        };
        let plan_en = empty_plan(PortableAssetActionKind::Enable, vec![enable_change.clone()]);
        let out2 = CodexTargetExecutor
            .execute_change(&ctx_en, &plan_en, &enable_change, Some(&item))
            .unwrap();
        assert_eq!(out2, TargetActionRawOutcome::Applied);
        assert!(skill_dir.exists());
        std::env::remove_var("CODEX_HOME");
    }

    #[test]
    fn codex_mcp_disable_removes_toml_leaf_preserving_sibling() {
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[mcp_servers.keep-me]
command = "echo"

[mcp_servers.drop-me]
command = "secret-cmd"
env = { TOKEN = "plain-secret-value" }
"#,
        )
        .unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(PortableAssetKind::Mcp, "drop-me", &config.to_string_lossy());
        let change = base_change(
            PortableAssetKind::Mcp,
            "id-drop-me",
            &config.to_string_lossy(),
            PortableAssetPlanOperation::Disable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
            action: PortableAssetActionKind::Disable,
        };
        let plan = empty_plan(PortableAssetActionKind::Disable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after = fs::read_to_string(&config).unwrap();
        assert!(after.contains("keep-me"));
        assert!(!after.contains("drop-me"));
        std::env::remove_var("CODEX_HOME");
    }
}
