//! portable_actions/targets/codex — Codex CLI 本机动作执行
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex skill 位于 CODEX_HOME/skills 或 ~/.agents/skills；MCP 在 config.toml 的
//!     `mcp_servers`（原生用 leaf 内 `enabled` 开关，不是删表）；plugins 在
//!     CODEX_HOME/plugins。阶段一要求 certified pin 后真实写盘。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `TargetActionExecutor`：Skill/Command 用 active↔disabled move；
//!     MCP Enable/Disable 写 `mcp_servers.{id}.enabled`，leaf 缺失时 Enable 才从
//!     disabled snapshot 恢复；Plugin enable/disable 写 config.toml
//!     `[plugins."id@market"].enabled`，uninstall 仍备份并移除 package 目录。

use super::{TargetActionContext, TargetActionExecutor, TargetActionRawOutcome};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, value_content_hash, ManagedConfigPatch, SemanticConfigPatcher,
    TomlConfigPatcher, CAS_EXPECT_ABSENT,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
    PortableAssetBackupPolicy,
};
use crate::agent_hub::portable_inventory::plugin_enablement::plugin_config_key_matches;
use crate::agent_hub::portable_inventory::{
    hash_plugin_root, PortableAssetKind, PortableInventoryItemDto,
};
use crate::agent_hub::portable_store::{
    current_portable_store_root, execute_skill_or_command_store, is_under_portable_store,
    observed_or_native_store_mount, should_use_store_semantics,
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
        // hub `…/disabled/skills` or the portable-store 真树 as the active skills_dir.
        // 未附加仓库项的 source_path 就是 store/skills/<id>；若把那层当 native 根，
        // attach 会在真树上创建指向自己的软链并报 LINK_CONFLICT_REAL_PATH。
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
                !parent_is_disabled && !path_is_under_portable_store(a)
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

/// 路径是否落在 portable-store 真树内。
///
/// Business Logic: 仓库真树不能当 Codex native 挂载点，否则附加会覆盖真安装。
/// Code Logic: canonicalize 后 `is_under_portable_store`；失败则前缀匹配。
fn path_is_under_portable_store(path: &Path) -> bool {
    let Some(store_root) = current_portable_store_root() else {
        return false;
    };
    if let Ok(canonical) = fs::canonicalize(path) {
        return is_under_portable_store(&canonical, &store_root);
    }
    path.starts_with(&store_root)
}

/// Codex 上应挂/拆的 native 路径。
///
/// Business Logic: 已附加软链用观测路径；未附加仓库项的 source_path 是真树，必须落到 CODEX_HOME。
/// Code Logic: 观测路径是软链则用之；落在 store 内则回退 native 挂载点。
fn codex_native_store_mount(observed: Option<&str>, fallback: PathBuf) -> PathBuf {
    let mounted = observed_or_native_store_mount(observed, fallback.clone());
    if path_is_under_portable_store(&mounted) {
        return fallback;
    }
    mounted
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
    let native_link = codex_native_store_mount(change.path.as_deref(), roots.skills_dir.join(&id));
    if should_use_store_semantics(ctx.action, Some(&native_link), pre_item)
        || change
            .path
            .as_deref()
            .is_some_and(|p| should_use_store_semantics(ctx.action, Some(Path::new(p)), pre_item))
    {
        return execute_skill_or_command_store(
            AgentTarget::Codex,
            ctx.action,
            PortableAssetKind::Skill,
            &id,
            &native_link,
            pre_item,
        );
    }
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
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "adopt/install not wired for codex".into(),
        }),
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => execute_skill_or_command_store(
            AgentTarget::Codex,
            ctx.action,
            PortableAssetKind::Skill,
            &id,
            &native_link,
            pre_item,
        ),
    }
}

fn execute_command(
    ctx: &TargetActionContext,
    roots: &CodexRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let native_link = codex_native_store_mount(
        change.path.as_deref(),
        roots.commands_dir.join(format!("{id}.md")),
    );
    if should_use_store_semantics(ctx.action, Some(&native_link), pre_item)
        || change
            .path
            .as_deref()
            .is_some_and(|p| should_use_store_semantics(ctx.action, Some(Path::new(p)), pre_item))
    {
        return execute_skill_or_command_store(
            AgentTarget::Codex,
            ctx.action,
            PortableAssetKind::Command,
            &id,
            &native_link,
            pre_item,
        );
    }
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
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "adopt/install not wired for codex".into(),
        }),
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => execute_skill_or_command_store(
            AgentTarget::Codex,
            ctx.action,
            PortableAssetKind::Command,
            &id,
            &native_link,
            pre_item,
        ),
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
            let owned = patcher.inspect(&bytes, &["mcp_servers".into(), id.clone()])?;
            if owned.present {
                if let Some(failed) =
                    mcp_leaf_hash_mismatch(&owned, change.expected_source_hash.as_deref())
                {
                    return Ok(failed);
                }
                let out = set_codex_mcp_enabled_flag(&patcher, &config_path, &bytes, &id, true)?;
                if matches!(out, TargetActionRawOutcome::Applied) {
                    let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
                    let _ = fs::remove_file(disabled);
                }
                return Ok(out);
            }
            // leaf 已不在 config：仅此时从 Hub Disable 留下的 snapshot 恢复。
            let disabled = roots.disabled_mcp_dir.join(format!("{id}.json"));
            let mut value = if disabled.exists() {
                let text = fs::read_to_string(&disabled)?;
                serde_json::from_str::<serde_json::Value>(&text)?
            } else {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_DISABLED_MISSING".into(),
                    message: "disabled MCP snapshot missing".into(),
                });
            };
            if let Some(obj) = value.as_object_mut() {
                obj.insert("enabled".into(), serde_json::Value::Bool(true));
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
            if let Some(failed) =
                mcp_leaf_hash_mismatch(&owned, change.expected_source_hash.as_deref())
            {
                return Ok(failed);
            }
            set_codex_mcp_enabled_flag(&patcher, &config_path, &bytes, &id, false)
        }
        PortableAssetActionKind::Uninstall => {
            let owned = patcher.inspect(&bytes, &["mcp_servers".into(), id.clone()])?;
            if owned.present {
                if let Some(failed) =
                    mcp_leaf_hash_mismatch(&owned, change.expected_source_hash.as_deref())
                {
                    return Ok(failed);
                }
                let leaf_hash = value_content_hash(&owned.value);
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
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "adopt/install not wired for codex".into(),
        }),
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_MCP_STORE_UNSUPPORTED".into(),
            message: "MCP stays a native config leaf; use enable/disable/uninstall or Pull".into(),
        }),
    }
}

/// 校验 MCP leaf 的 preview `expected_source_hash`。
///
/// Business Logic: planner 绑定整 leaf hash；apply 前必须拒绝并发改写。
/// Code Logic: 与 `value_content_hash` 同域；不匹配则 SOURCE_HASH_CHANGED。
fn mcp_leaf_hash_mismatch(
    owned: &crate::agent_hub::config_patch::OwnedConfigValue,
    expected: Option<&str>,
) -> Option<TargetActionRawOutcome> {
    let expected = expected?;
    let leaf_hash = value_content_hash(&owned.value);
    if expected == leaf_hash {
        None
    } else {
        Some(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
            message: "mcp leaf hash changed since preview".into(),
        })
    }
}

/// 翻转 Codex `mcp_servers.{id}.enabled`，不改 sibling 字段。
///
/// Business Logic: Codex 原生用 leaf 内 `enabled` 开关 MCP。`enabled = false` 的
///     server 仍在 config.toml，Enable 不得要求 Hub Disable snapshot。
/// Code Logic: 缺省无 enabled 字段视为 true；只 patch `.enabled` 保留注释与其它键。
fn set_codex_mcp_enabled_flag(
    patcher: &TomlConfigPatcher,
    config_path: &Path,
    bytes: &[u8],
    id: &str,
    enabled: bool,
) -> Result<TargetActionRawOutcome, AppError> {
    let path = vec!["mcp_servers".into(), id.to_string(), "enabled".into()];
    let owned = patcher.inspect(bytes, &path)?;
    let patch = if owned.present {
        if owned.value.as_bool() == Some(enabled) {
            return Ok(TargetActionRawOutcome::Skipped);
        }
        ManagedConfigPatch {
            owner_id: format!("portable-codex:{id}"),
            path,
            value: Some(serde_json::Value::Bool(enabled)),
            expected_base_hash: owned.value_hash,
        }
    } else if enabled {
        return Ok(TargetActionRawOutcome::Skipped);
    } else {
        ManagedConfigPatch {
            owner_id: format!("portable-codex:{id}"),
            path,
            value: Some(serde_json::Value::Bool(false)),
            expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
        }
    };
    let prepared = apply_config_patch_atomically(patcher, config_path, &[patch])?;
    match prepared.patched.outcome {
        crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
            Ok(TargetActionRawOutcome::Applied)
        }
        crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT".into(),
                message: if enabled {
                    "mcp enable CAS conflict".into()
                } else {
                    "mcp disable CAS conflict".into()
                },
            })
        }
        other => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED".into(),
            message: format!("{other:?}"),
        }),
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
            // 权威启用态在 config.toml `[plugins."id@market"] enabled`；
            // 目录仅表示已安装缓存，不能单独决定 enable/disable。
            if !path.exists() {
                return Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_PLUGIN_MISSING".into(),
                    message: "plugin path missing".into(),
                });
            }
            let want_enabled = matches!(ctx.action, PortableAssetActionKind::Enable);
            let source = change
                .path
                .as_deref()
                .or_else(|| pre_item.and_then(|item| item.source_path.as_deref()));
            set_codex_plugin_enabled_in_config(&roots.config_toml, &id, source, want_enabled)
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
            // 同步去掉 config 中启用项（若存在）
            let _ = set_codex_plugin_enabled_in_config(
                &roots.config_toml,
                &id,
                change.path.as_deref(),
                false,
            );
            Ok(TargetActionRawOutcome::Applied)
        }
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "adopt/install not wired for codex".into(),
        }),
    }
}

/// 在 Codex config.toml 中设置 `[plugins."…"] enabled`。
///
/// Business Logic: short id（browser）匹配 `browser@market`。cache 里已有但未登记的
///     native 包（`codex_plugin_not_in_config`）Enable 必须写入 `id@market`，不能失败。
///     Disable 对未登记项视为已关闭 skip。
/// Code Logic: 已有 key 则翻 enabled；Enable 且无 key 时按 cache 路径插入新表。
fn set_codex_plugin_enabled_in_config(
    config_path: &Path,
    plugin_id: &str,
    source_path: Option<&str>,
    enabled: bool,
) -> Result<TargetActionRawOutcome, AppError> {
    use crate::agent_hub::config_patch::{
        apply_config_patch_atomically, ManagedConfigPatch, SemanticConfigPatcher, TomlConfigPatcher,
    };
    use crate::agent_hub::portable_inventory::plugin_paths::plugin_cli_selector;

    if !config_path.is_file() {
        return Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_PLUGIN_CONFIG_MISSING".into(),
            message: "codex config.toml missing".into(),
        });
    }
    let bytes = fs::read(config_path)?;
    let text = String::from_utf8_lossy(&bytes);
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::validation(format!("codex_config_toml_invalid:{e}")))?;
    let keys: Vec<String> = doc
        .get("plugins")
        .and_then(|i| i.as_table())
        .map(|plugins| {
            plugins
                .iter()
                .map(|(k, _)| k.to_string())
                .filter(|k| plugin_config_key_matches(plugin_id, k))
                .collect()
        })
        .unwrap_or_default();

    let patcher = TomlConfigPatcher;
    if keys.is_empty() {
        if !enabled {
            return Ok(TargetActionRawOutcome::Skipped);
        }
        let key = plugin_cli_selector(plugin_id, source_path);
        let patches = [ManagedConfigPatch {
            owner_id: format!("portable-codex-plugin:{key}"),
            path: vec!["plugins".into(), key, "enabled".into()],
            value: Some(serde_json::Value::Bool(true)),
            expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
        }];
        return plugin_config_patch_outcome(apply_config_patch_atomically(
            &patcher,
            config_path,
            &patches,
        )?);
    }

    let mut patches = Vec::new();
    for key in keys {
        let path = vec!["plugins".into(), key.clone(), "enabled".into()];
        let owned = patcher.inspect(&bytes, &path)?;
        if owned.present {
            if owned.value.as_bool() == Some(enabled) {
                continue;
            }
            patches.push(ManagedConfigPatch {
                owner_id: format!("portable-codex-plugin:{key}"),
                path,
                value: Some(serde_json::Value::Bool(enabled)),
                expected_base_hash: owned.value_hash,
            });
        } else {
            // enabled 字段缺失：按 Codex 默认 true；若目标值相同则 skip
            if enabled {
                continue;
            }
            patches.push(ManagedConfigPatch {
                owner_id: format!("portable-codex-plugin:{key}"),
                path,
                value: Some(serde_json::Value::Bool(false)),
                expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
            });
        }
    }
    if patches.is_empty() {
        return Ok(TargetActionRawOutcome::Skipped);
    }
    plugin_config_patch_outcome(apply_config_patch_atomically(
        &patcher,
        config_path,
        &patches,
    )?)
}

/// 把 plugin config patch 结果收成 executor outcome。
///
/// Business Logic: CAS 冲突必须可区分，禁止当成成功。
/// Code Logic: Applied / Conflict / 其它失败码。
fn plugin_config_patch_outcome(
    prepared: crate::agent_hub::config_patch::PreparedConfigProjection,
) -> Result<TargetActionRawOutcome, AppError> {
    match prepared.patched.outcome {
        crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
            Ok(TargetActionRawOutcome::Applied)
        }
        crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
            Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_PLUGIN_CAS_CONFLICT".into(),
                message: "plugin enable CAS conflict".into(),
            })
        }
        other => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_PLUGIN_PATCH_FAILED".into(),
            message: format!("{other:?}"),
        }),
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
        PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin, PortableOriginKind, PortableStoreFactDto,
    };
    use crate::agent_hub::portable_store::{
        ensure_portable_store_layout, portable_store_root, store_skill_dir,
    };
    use crate::agent_hub::targets::portable::DATA_DIR_ENV_LOCK;
    use std::sync::{Arc, Mutex, OnceLock};
    use tempfile::TempDir;

    /// CODEX_HOME 是进程全局 env；凡依赖它的单测必须串行。
    fn codex_home_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

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
            loaded_by: AgentTarget::Codex,
            owned_by: PortableAssetOwner::Codex,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
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
            store: Default::default(),
        }
    }

    #[test]
    fn codex_skill_disable_moves_to_disabled_and_enable_restores() {
        let _guard = codex_home_lock();
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

    /// Business Logic: 仓库附加必须在 CODEX_HOME/skills 建软链，不得把 store 真树当挂载点。
    /// Code Logic: change.path 指向 portable-store/skills/<id> 时 attach 仍落到 ~/.codex/skills。
    #[test]
    fn attach_store_skill_links_codex_home_not_store_tree() {
        let _data_guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _codex_guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");
        let codex_home = tmp.path().join("codex-home");
        fs::create_dir_all(codex_home.join("skills")).unwrap();
        std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
        std::env::set_var("CODEX_HOME", &codex_home);
        let store_root = ensure_portable_store_layout(&data_dir).unwrap();
        let store_tree = store_skill_dir(&store_root, "web-video-presentation");
        fs::create_dir_all(&store_tree).unwrap();
        fs::write(
            store_tree.join("SKILL.md"),
            "---\nname: web-video-presentation\n---\n",
        )
        .unwrap();

        let store_path = store_tree.to_string_lossy().into_owned();
        let mut item = sample_item(
            PortableAssetKind::Skill,
            "web-video-presentation",
            &store_path,
        );
        item.owned_by = PortableAssetOwner::PortableStore;
        item.actual_enabled = Some(false);
        item.store = PortableStoreFactDto {
            store_id: Some("skill:web-video-presentation".into()),
            store_attached: false,
            loaded_via_other_path: false,
            loaded_via_target: None,
        };
        let change = base_change(
            PortableAssetKind::Skill,
            "id-web-video-presentation",
            &store_path,
            PortableAssetPlanOperation::Attach,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir.clone()),
            keep_data: false,
            action: PortableAssetActionKind::Attach,
        };
        let plan = empty_plan(PortableAssetActionKind::Attach, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied, "attach must succeed");
        let native = codex_home.join("skills").join("web-video-presentation");
        let meta = fs::symlink_metadata(&native).expect("native mount");
        assert!(
            meta.file_type().is_symlink(),
            "CODEX_HOME/skills must receive the store symlink"
        );
        assert_eq!(
            fs::canonicalize(&native).unwrap(),
            fs::canonicalize(&store_tree).unwrap()
        );
        assert!(
            !fs::symlink_metadata(&store_tree)
                .unwrap()
                .file_type()
                .is_symlink(),
            "store tree must remain a real directory"
        );
        let _ = portable_store_root(&data_dir);
        std::env::remove_var("CODEX_HOME");
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    #[test]
    fn codex_mcp_disable_sets_enabled_false_preserving_sibling() {
        let _guard = codex_home_lock();
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
        assert!(after.contains("drop-me"));
        assert!(after.contains("secret-cmd"));
        assert!(
            after.contains("enabled = false") || after.contains("enabled=false"),
            "disable must flip native enabled flag: {after}"
        );
        std::env::remove_var("CODEX_HOME");
    }

    /// preview expected_source_hash（含 int 的 Toml leaf）必须让 disable apply 通过 CAS。
    #[test]
    fn codex_mcp_disable_accepts_toml_leaf_hash_with_integer_fields() {
        use crate::agent_hub::config_patch::{SemanticConfigPatcher, TomlConfigPatcher};

        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        let body = r#"
[mcp_servers.node_repl]
command = "node"
startup_timeout_sec = 120
args = ["mcp"]
"#;
        fs::write(&config, body).unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let leaf = TomlConfigPatcher
            .inspect(body.as_bytes(), &["mcp_servers".into(), "node_repl".into()])
            .unwrap();
        let expected = leaf.value_hash.expect("hash");

        // 错误的 string-only hash 必须被拒绝
        let mut bad = base_change(
            PortableAssetKind::Mcp,
            "id-node_repl",
            &config.to_string_lossy(),
            PortableAssetPlanOperation::Disable,
        );
        bad.expected_source_hash = Some(crate::agent_hub::config_patch::value_content_hash(
            &serde_json::json!({
                "command": "node",
                "args": ["mcp"],
            }),
        ));
        let item = sample_item(
            PortableAssetKind::Mcp,
            "node_repl",
            &config.to_string_lossy(),
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir.clone()),
            keep_data: false,
            action: PortableAssetActionKind::Disable,
        };
        let plan_bad = empty_plan(PortableAssetActionKind::Disable, vec![bad.clone()]);
        let out_bad = CodexTargetExecutor
            .execute_change(&ctx, &plan_bad, &bad, Some(&item))
            .unwrap();
        assert!(
            matches!(
                &out_bad,
                TargetActionRawOutcome::Failed { code, .. }
                    if code == "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED"
            ),
            "incomplete hash must fail CAS: {out_bad:?}"
        );

        // 正确 Toml leaf hash 必须成功，且只翻转 enabled、保留 int 字段
        let mut good = base_change(
            PortableAssetKind::Mcp,
            "id-node_repl",
            &config.to_string_lossy(),
            PortableAssetPlanOperation::Disable,
        );
        good.expected_source_hash = Some(expected);
        let plan_good = empty_plan(PortableAssetActionKind::Disable, vec![good.clone()]);
        let out_good = CodexTargetExecutor
            .execute_change(&ctx, &plan_good, &good, Some(&item))
            .unwrap();
        assert_eq!(out_good, TargetActionRawOutcome::Applied);
        let after = fs::read_to_string(&config).unwrap();
        assert!(after.contains("node_repl"));
        assert!(after.contains("startup_timeout_sec"));
        assert!(
            after.contains("enabled = false") || after.contains("enabled=false"),
            "disable must keep leaf and set enabled=false: {after}"
        );
        std::env::remove_var("CODEX_HOME");
    }

    /// Codex 原生 `enabled = false` 的 MCP 必须直接翻回 true，不要求 disabled snapshot。
    #[test]
    fn codex_mcp_enable_flips_native_enabled_false_without_snapshot() {
        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[mcp_servers.computer-use]
command = "./Codex Computer Use.app/Contents/MacOS/SkyComputerUseClient"
args = ["mcp"]
cwd = "."
enabled = false
"#,
        )
        .unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Mcp,
            "computer-use",
            &config.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Mcp,
            "id-computer-use",
            &config.to_string_lossy(),
            PortableAssetPlanOperation::Enable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
            action: PortableAssetActionKind::Enable,
        };
        let plan = empty_plan(PortableAssetActionKind::Enable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied, "{out:?}");
        let after = fs::read_to_string(&config).unwrap();
        assert!(after.contains("computer-use"));
        assert!(after.contains("cwd"));
        assert!(
            after.contains("enabled = true") || after.contains("enabled=true"),
            "enable must flip native flag without snapshot: {after}"
        );
        assert!(
            !after.contains("enabled = false") && !after.contains("enabled=false"),
            "enabled=false must not remain: {after}"
        );
        std::env::remove_var("CODEX_HOME");
    }

    /// leaf 已被删时，Enable 仍从 Hub disabled snapshot 恢复。
    #[test]
    fn codex_mcp_enable_restores_disabled_snapshot_when_leaf_absent() {
        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[mcp_servers.keep-me]
command = "echo"
"#,
        )
        .unwrap();
        let data_dir = tmp.path().join("data");
        let disabled_dir = data_dir.join("codex-assets").join("disabled").join("mcp");
        fs::create_dir_all(&disabled_dir).unwrap();
        fs::write(
            disabled_dir.join("computer-use.json"),
            r#"{"command":"cua","args":["mcp"],"cwd":"."}"#,
        )
        .unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Mcp,
            "computer-use",
            &config.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Mcp,
            "id-computer-use",
            &config.to_string_lossy(),
            PortableAssetPlanOperation::Enable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
            action: PortableAssetActionKind::Enable,
        };
        let plan = empty_plan(PortableAssetActionKind::Enable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied, "{out:?}");
        let after = fs::read_to_string(&config).unwrap();
        assert!(after.contains("keep-me"));
        assert!(after.contains("computer-use"));
        assert!(after.contains("cua"));
        std::env::remove_var("CODEX_HOME");
    }

    /// disable 写回 config.toml plugins enabled=false，而不是目录 no-op。
    #[test]
    fn codex_plugin_disable_sets_config_enabled_false() {
        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[plugins."browser@openai-bundled"]
enabled = true
"#,
        )
        .unwrap();
        let plugin_root = tmp
            .path()
            .join("plugins/cache/openai-bundled/browser/26.803.61601");
        let manifest = plugin_root.join(".codex-plugin/plugin.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, r#"{"name":"browser"}"#).unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Plugin,
            "browser",
            &plugin_root.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Plugin,
            "id-browser",
            &plugin_root.to_string_lossy(),
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
        assert_eq!(out, TargetActionRawOutcome::Applied, "{out:?}");
        let after = fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("enabled = false") || after.contains("enabled=false"),
            "config must flip enabled: {after}"
        );
        assert!(plugin_root.exists(), "cache dir remains after disable");
        std::env::remove_var("CODEX_HOME");
    }

    /// cache 已有但 config 未登记的 native 包，Enable 必须写入 `id@market`。
    #[test]
    fn codex_plugin_enable_registers_cache_package_not_in_config() {
        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[plugins."browser@openai-bundled"]
enabled = true
"#,
        )
        .unwrap();
        let plugin_root = tmp
            .path()
            .join("plugins/cache/openai-curated-remote/product-design/0.1.52");
        let manifest = plugin_root.join(".codex-plugin/plugin.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, r#"{"name":"product-design","version":"0.1.52"}"#).unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Plugin,
            "product-design",
            &plugin_root.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Plugin,
            "id-product-design",
            &plugin_root.to_string_lossy(),
            PortableAssetPlanOperation::Enable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
            action: PortableAssetActionKind::Enable,
        };
        let plan = empty_plan(PortableAssetActionKind::Enable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied, "{out:?}");
        let after = fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("browser@openai-bundled"),
            "sibling plugin must remain: {after}"
        );
        assert!(
            after.contains("product-design@openai-curated-remote"),
            "enable must register cache marketplace key: {after}"
        );
        assert!(
            after.contains("enabled = true") || after.contains("enabled=true"),
            "new plugin must be enabled: {after}"
        );
        std::env::remove_var("CODEX_HOME");
    }

    /// scanner 的 native_id 已是 `id@market` 时，Enable 仍须写入同一键，不能二次加 `@`。
    #[test]
    fn codex_plugin_enable_registers_qualified_native_id() {
        let _guard = codex_home_lock();
        let tmp = TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[plugins."browser@openai-bundled"]
enabled = true
"#,
        )
        .unwrap();
        let plugin_root = tmp
            .path()
            .join("plugins/cache/openai-curated-remote/product-design/0.1.52");
        let manifest = plugin_root.join(".codex-plugin/plugin.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        fs::write(&manifest, r#"{"name":"product-design","version":"0.1.52"}"#).unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        std::env::set_var("CODEX_HOME", tmp.path());

        let item = sample_item(
            PortableAssetKind::Plugin,
            "product-design@openai-curated-remote",
            &plugin_root.to_string_lossy(),
        );
        let change = base_change(
            PortableAssetKind::Plugin,
            "id-product-design",
            &plugin_root.to_string_lossy(),
            PortableAssetPlanOperation::Enable,
        );
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data_dir),
            keep_data: false,
            action: PortableAssetActionKind::Enable,
        };
        let plan = empty_plan(PortableAssetActionKind::Enable, vec![change.clone()]);
        let out = CodexTargetExecutor
            .execute_change(&ctx, &plan, &change, Some(&item))
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied, "{out:?}");
        let after = fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("product-design@openai-curated-remote"),
            "enable must register scanner native_id: {after}"
        );
        assert!(
            !after.contains("product-design@openai-curated-remote@"),
            "must not append a second @market: {after}"
        );
        std::env::remove_var("CODEX_HOME");
    }
}
