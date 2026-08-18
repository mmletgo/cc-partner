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
use crate::agent_hub::config_patch::{value_content_hash, CAS_EXPECT_ABSENT};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::packages::activator::{ProcessOutcome, ProcessSpec};
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
    PortableAssetBackupPolicy,
};
use crate::agent_hub::portable_inventory::hash_plugin_root;
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::agent_hub::portable_store::{
    execute_skill_or_command_store, should_use_store_semantics,
};
use crate::agent_hub::targets::portable::hash_skill_directory;
use crate::claude_code_assets::{
    portable_backup_path, portable_claude_roots, portable_move_path, portable_remove_path,
    portable_remove_tree_with_backup, portable_set_command_enabled, portable_set_tree_enabled,
    ClaudeCodeAssetKind, PortableClaudeRoots,
};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// 与 inventory `content_hash` 对齐的源内容 hash。
///
/// Business Logic: planner 与 apply recheck 必须同一 hash 域；Skill=SKILL.md-only，
/// Plugin 目录=inventory `hash_plugin_root` material，文件=整文件字节。
/// Code Logic: 禁止对真实 plugin 根目录使用 path-string sha（生产必 SOURCE_HASH_CHANGED）。
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
            } else if path.is_file() {
                // plugin.json 等文件路径：回落到父根或文件字节
                path.parent()
                    .filter(|p| p.is_dir())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            } else {
                return Err(AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"));
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
            } else if path.is_dir() {
                // Command 目录少见；inventory 对 markdown 文件用全文 hash。无统一 tree helper 时
                // 与文件分支一致 fail-closed，避免 path-string 伪 hash。
                Err(AppError::validation(
                    "PORTABLE_ASSET_ACTION_COMMAND_DIR_HASH_UNSUPPORTED",
                ))
            } else {
                Err(AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"))
            }
        }
        PortableAssetKind::Mcp => {
            // MCP expected_source_hash 存 leaf value_content_hash，不走路径整文件 hash
            Err(AppError::validation(
                "PORTABLE_ASSET_ACTION_MCP_HASH_VIA_LEAF",
            ))
        }
    }
}

/// 读取 MCP leaf 的 CAS hash（与 config_patch `value_content_hash` 一致）。
fn mcp_leaf_value_hash(config_bytes: &[u8], server_id: &str) -> Result<Option<String>, AppError> {
    let current = read_mcp_value(config_bytes, server_id)?;
    Ok(current.as_ref().map(value_content_hash))
}

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

        // changed-source fail closed（mutation 前）：与 inventory content_hash 同一 hash 域
        // MCP 的 expected_source_hash 是 leaf value hash，在 execute_mcp 内 CAS 校验。
        // hash 计算失败必须 fail-closed — 不得软跳过 recheck 后继续 mutation。
        if change.kind != PortableAssetKind::Mcp {
            if let Some(expected) = change.expected_source_hash.as_deref() {
                if let Some(path) = change.path.as_deref() {
                    let p = Path::new(path);
                    if p.exists() {
                        match inventory_content_hash_for_path(change.kind, p) {
                            Ok(actual) => {
                                if actual != expected {
                                    return Ok(TargetActionRawOutcome::Failed {
                                        code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                                        message: "source content changed since preview".into(),
                                    });
                                }
                            }
                            Err(_) => {
                                return Ok(TargetActionRawOutcome::Failed {
                                    code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_UNAVAILABLE".into(),
                                    message: "source content hash unavailable for recheck".into(),
                                });
                            }
                        }
                    }
                }
            }
        }

        let user_roots =
            portable_claude_roots(ctx.claude_config_dir.as_deref(), ctx.data_dir.as_deref())?;
        // Project-scoped inventory items must mutate under observed project roots
        // (source_path / project mapping), never silent-redirect to user ~/.claude.
        let roots = resolve_action_roots(&user_roots, pre_item, change)?;
        match change.kind {
            PortableAssetKind::Plugin => execute_plugin(ctx, change, pre_item),
            PortableAssetKind::Skill => execute_skill(ctx, &roots, change, pre_item),
            PortableAssetKind::Command => execute_command(ctx, &roots, change, pre_item),
            PortableAssetKind::Mcp => execute_mcp(ctx, &roots, change, pre_item),
        }
    }
}

/// 根据 inventory item scope 解析实际 mutation 根。
///
/// Business Logic: 已 opt-in 的项目级 Skill/Command/MCP 必须改项目路径；
///     unopted 由上层只读门禁拦截，此处再 fail-closed 一次。
/// Code Logic: project scope → 从 source_path 回推 .claude 根；否则用 user roots。
fn resolve_action_roots(
    user_roots: &PortableClaudeRoots,
    pre_item: Option<&PortableInventoryItemDto>,
    change: &PortableAssetActionChangeDto,
) -> Result<PortableClaudeRoots, AppError> {
    let Some(item) = pre_item else {
        return Ok(user_roots.clone());
    };
    if item.scope_kind != crate::agent_hub::models::ScopeKind::Project {
        return Ok(user_roots.clone());
    }
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
    let project_claude_root = source.and_then(infer_project_claude_root).ok_or_else(|| {
        AppError::validation("PORTABLE_ASSET_ACTION_PROJECT_ROOT_UNRESOLVED".to_string())
    })?;
    // Project disable/enable must stay project-scoped. Scanner inventories
    // `.claude/disabled/{skills,commands}` under project scope; using global
    // user_roots.disabled_* would promote the asset into user inventory.
    // Hub data_dir backup_root remains shared for recoverable uninstall only.
    Ok(PortableClaudeRoots {
        skills_dir: project_claude_root.join("skills"),
        commands_dir: project_claude_root.join("commands"),
        disabled_skills_dir: project_claude_root.join("disabled").join("skills"),
        disabled_commands_dir: project_claude_root.join("disabled").join("commands"),
        disabled_mcp_dir: project_claude_root.join("disabled").join("mcp"),
        backup_root: user_roots.backup_root.clone(),
        config_dir: project_claude_root.clone(),
        // Project MCP lives under project `.mcp.json` when source is that file;
        // default to project-local settings when unresolved later in execute_mcp.
        claude_json_path: infer_project_mcp_config_path(source, &project_claude_root),
    })
}

fn infer_project_claude_root(source: &Path) -> Option<PathBuf> {
    // Walk up looking for a `.claude` segment or a project root that owns `.claude`.
    let mut cur = if source.is_file() {
        source.parent().map(Path::to_path_buf)?
    } else {
        source.to_path_buf()
    };
    loop {
        if cur
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n == ".claude")
        {
            return Some(cur);
        }
        let candidate = cur.join(".claude");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

fn infer_project_mcp_config_path(source: Option<&Path>, project_claude_root: &Path) -> PathBuf {
    if let Some(p) = source {
        if p.is_file() {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == ".mcp.json"
                || name == "settings.local.json"
                || name.ends_with(".json")
                    && p.parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|s| s.to_str())
                        == Some(".claude")
            {
                return p.to_path_buf();
            }
        }
    }
    // Prefer project-root `.mcp.json` sibling of `.claude`.
    if let Some(project_root) = project_claude_root.parent() {
        let mcp = project_root.join(".mcp.json");
        if mcp.is_file() {
            return mcp;
        }
        return project_claude_root.join("settings.local.json");
    }
    project_claude_root.join("settings.local.json")
}

fn execute_plugin(
    ctx: &TargetActionContext,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = crate::agent_hub::portable_inventory::plugin_paths::plugin_cli_selector(
        &native_id(change, pre_item),
        pre_item
            .and_then(|i| i.source_path.as_deref())
            .or(change.path.as_deref()),
    );
    let scope = scope_arg(pre_item);
    // Claude resolves --scope project from process cwd; fail-closed if unresolved.
    let cwd = plugin_process_cwd(pre_item, change)?;
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
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => {
            return Ok(TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_PLUGIN_ACTION_UNSUPPORTED".into(),
                message: format!("plugin action {} not executed here", ctx.action.as_str()),
            });
        }
    };
    match ctx.runner.run(&ProcessSpec { program, args, cwd }) {
        Ok(out) => Ok(map_claude_plugin_process_outcome(ctx.action, out)),
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

/// 将 Claude plugin CLI 结果映射为 raw outcome。
///
/// Business Logic: `already disabled/enabled` 是幂等终态，不得标 CLI_FAILED。
/// Code Logic: exit 0 → Applied；stderr 已满足 → Skipped；其余走通用失败信封。
fn map_claude_plugin_process_outcome(
    action: PortableAssetActionKind,
    outcome: ProcessOutcome,
) -> TargetActionRawOutcome {
    if outcome.code == 0 {
        return TargetActionRawOutcome::Applied;
    }
    if claude_plugin_cli_already_satisfied(action, &outcome.stderr) {
        return TargetActionRawOutcome::Skipped;
    }
    map_process_outcome(outcome, "claude plugin")
}

/// 判定 Claude plugin CLI 是否报告目标状态已满足。
fn claude_plugin_cli_already_satisfied(action: PortableAssetActionKind, stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    match action {
        PortableAssetActionKind::Disable => lower.contains("already disabled"),
        PortableAssetActionKind::Enable => lower.contains("already enabled"),
        PortableAssetActionKind::Uninstall => {
            lower.contains("already uninstalled") || lower.contains("is not installed")
        }
        PortableAssetActionKind::Adopt
        | PortableAssetActionKind::InstallToSourceTarget
        | PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore
        | PortableAssetActionKind::ConfirmCurrentVersion
        | PortableAssetActionKind::MaterializeEscapeLink => false,
    }
}

/// Resolve cwd for Claude plugin CLI.
///
/// Business Logic: `--scope project` is relative to the observed project root.
/// Code Logic: project inventory → project root from source_path; user scope → None.
fn plugin_process_cwd(
    pre_item: Option<&PortableInventoryItemDto>,
    change: &PortableAssetActionChangeDto,
) -> Result<Option<PathBuf>, AppError> {
    let Some(item) = pre_item else {
        return Ok(None);
    };
    if item.scope_kind != crate::agent_hub::models::ScopeKind::Project {
        return Ok(None);
    }
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
    let project_claude = source.and_then(infer_project_claude_root).ok_or_else(|| {
        AppError::validation("PORTABLE_ASSET_ACTION_PROJECT_ROOT_UNRESOLVED".to_string())
    })?;
    let project_root = project_claude
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            AppError::validation("PORTABLE_ASSET_ACTION_PROJECT_ROOT_UNRESOLVED".to_string())
        })?;
    Ok(Some(project_root))
}

fn execute_skill(
    ctx: &TargetActionContext,
    roots: &PortableClaudeRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let native_link = roots.skills_dir.join(&id);
    if should_use_store_semantics(ctx.action, Some(&native_link), pre_item)
        || change
            .path
            .as_deref()
            .is_some_and(|p| should_use_store_semantics(ctx.action, Some(Path::new(p)), pre_item))
    {
        return execute_skill_or_command_store(
            AgentTarget::Claude,
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
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "explicit adopt ownership write is not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::ConfirmCurrentVersion => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_LEDGER_ONLY".into(),
            message: "confirmCurrentVersion is hub-ledger only".into(),
        }),
        PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ESCAPE_REPAIR_INTERCEPT".into(),
            message: "materializeEscapeLink is executed by the shared executor".into(),
        }),
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => execute_skill_or_command_store(
            AgentTarget::Claude,
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
    roots: &PortableClaudeRoots,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let native_link = roots.commands_dir.join(format!("{id}.md"));
    if should_use_store_semantics(ctx.action, Some(&native_link), pre_item)
        || change
            .path
            .as_deref()
            .is_some_and(|p| should_use_store_semantics(ctx.action, Some(Path::new(p)), pre_item))
    {
        return execute_skill_or_command_store(
            AgentTarget::Claude,
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
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "explicit adopt ownership write is not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::ConfirmCurrentVersion => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_LEDGER_ONLY".into(),
            message: "confirmCurrentVersion is hub-ledger only".into(),
        }),
        PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ESCAPE_REPAIR_INTERCEPT".into(),
            message: "materializeEscapeLink is executed by the shared executor".into(),
        }),
        PortableAssetActionKind::Attach
        | PortableAssetActionKind::Detach
        | PortableAssetActionKind::DestroyStore
        | PortableAssetActionKind::MigrateToStore => execute_skill_or_command_store(
            AgentTarget::Claude,
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
            // enable 绑定「当前 leaf 不存在」：预检 + apply 内 CAS_EXPECT_ABSENT 双重关闭 TOCTOU
            let current_hash = mcp_leaf_value_hash(&bytes, &id)?;
            if current_hash.is_some() {
                return Ok(TargetActionRawOutcome::Skipped);
            }
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: Some(value),
                // apply 时 re-read leaf：若并发插入则 Conflict（check_cas 要求 absence）
                expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
            }];
            let prepared =
                apply_config_patch_atomically(&JsoncConfigPatcher, &config_path, &patches)?;
            match prepared.patched.outcome {
                crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
                    let _ = fs::remove_file(disabled);
                    Ok(TargetActionRawOutcome::Applied)
                }
                crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
                    Ok(TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT".into(),
                        message: "mcp enable CAS conflict: leaf appeared before apply".into(),
                    })
                }
                other => Ok(TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED".into(),
                    message: format!("{other:?}"),
                }),
            }
        }
        PortableAssetActionKind::Disable => {
            // 读当前值，写 disabled，再 semantic remove
            let current = read_mcp_value(&bytes, &id)?;
            let Some(cfg) = current else {
                return Ok(TargetActionRawOutcome::Skipped);
            };
            let leaf_hash = value_content_hash(&cfg);
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
            // 原文写入 disabled（含 credentials；不进 DTO/日志）
            fs::write(&disabled, serde_json::to_vec_pretty(&cfg)?)?;
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: None,
                expected_base_hash: Some(leaf_hash),
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
            let current = read_mcp_value(&bytes, &id)?;
            if let Some(cfg) = current.as_ref() {
                let leaf_hash = value_content_hash(cfg);
                if let Some(expected) = change.expected_source_hash.as_deref() {
                    if expected != leaf_hash {
                        return Ok(TargetActionRawOutcome::Failed {
                            code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                            message: "mcp leaf hash changed since preview".into(),
                        });
                    }
                }
                fs::create_dir_all(&roots.backup_root)?;
                let dst = roots
                    .backup_root
                    .join(chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string())
                    .join("mcp")
                    .join(format!("{id}.json"));
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(dst, serde_json::to_vec_pretty(cfg)?)?;
            }
            let expected = change
                .expected_source_hash
                .clone()
                .or_else(|| current.as_ref().map(value_content_hash));
            let patches = [ManagedConfigPatch {
                owner_id: format!("portable:{id}"),
                path: vec!["mcpServers".into(), id.clone()],
                value: None,
                expected_base_hash: expected,
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
        PortableAssetActionKind::Adopt => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into(),
            message: "explicit adopt ownership write is not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::InstallToSourceTarget => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_INSTALL_NOT_WIRED".into(),
            message: "installToSourceTarget not wired; refuse fake success".into(),
        }),
        PortableAssetActionKind::ConfirmCurrentVersion => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_LEDGER_ONLY".into(),
            message: "confirmCurrentVersion is hub-ledger only".into(),
        }),
        PortableAssetActionKind::MaterializeEscapeLink => Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ESCAPE_REPAIR_INTERCEPT".into(),
            message: "materializeEscapeLink is executed by the shared executor".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::ScopeKind;
    use crate::agent_hub::portable_actions::models::{
        PortableAssetCanonicalEffect, PortableAssetPlanOperation,
    };
    use crate::agent_hub::portable_inventory::{
        PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin, PortableOriginKind,
    };

    fn sample_item(scope_kind: ScopeKind, path: &str, opted: bool) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: "id-proj-skill".into(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            loaded_by: crate::agent_hub::models::AgentTarget::Claude,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Skill,
            native_id: "proj-skill".into(),
            display_name: "proj-skill".into(),
            description: None,
            version: None,
            scope_id: "project:hub-1".into(),
            scope_kind,
            project_id: Some("hub-1".into()),
            project_opted_in: opted,
            source_path: Some(path.into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("h".into()),
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
    fn project_scope_skill_roots_use_project_path_not_user() {
        let tmp = tempfile::tempdir().unwrap();
        let user_claude = tmp.path().join("user-claude");
        let data = tmp.path().join("data");
        let project = tmp.path().join("repo");
        let project_claude = project.join(".claude");
        let skill = project_claude.join("skills/proj-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# p\n").unwrap();
        std::fs::create_dir_all(&user_claude).unwrap();
        let user_roots = portable_claude_roots(Some(&user_claude), Some(&data)).unwrap();
        let item = sample_item(ScopeKind::Project, skill.to_str().unwrap(), true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: item.content_hash.clone(),
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::RecoverableBeforeDelete,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let roots = resolve_action_roots(&user_roots, Some(&item), &change).unwrap();
        assert_eq!(roots.skills_dir, project_claude.join("skills"));
        assert_ne!(roots.skills_dir, user_roots.skills_dir);
        // Project disable stays under project .claude/disabled — never promote to user.
        assert_eq!(
            roots.disabled_skills_dir,
            project_claude.join("disabled").join("skills")
        );
        assert_ne!(roots.disabled_skills_dir, user_roots.disabled_skills_dir);
    }

    #[test]
    fn unopted_project_action_roots_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let user_claude = tmp.path().join("user-claude");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&user_claude).unwrap();
        let user_roots = portable_claude_roots(Some(&user_claude), Some(&data)).unwrap();
        let item = sample_item(ScopeKind::Project, "/tmp/x/.claude/skills/a", false);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let err = resolve_action_roots(&user_roots, Some(&item), &change).unwrap_err();
        assert!(err
            .to_string()
            .contains("PORTABLE_ASSET_ACTION_PROJECT_NOT_OPTED_IN"));
    }

    #[test]
    fn infer_project_claude_root_from_skill_path() {
        let p = PathBuf::from("/work/demo/.claude/skills/foo/SKILL.md");
        let root = infer_project_claude_root(&p).unwrap();
        assert_eq!(root, PathBuf::from("/work/demo/.claude"));
    }

    /// Business Logic: project skill Disable must stay under project .claude/disabled;
    /// Enable restores the same project skills path (never user hub disabled).
    #[test]
    fn project_skill_disable_enable_stays_project_scoped() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::{
            TargetActionContext, TargetActionRawOutcome,
        };
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let user_claude = tmp.path().join("user-claude");
        let data = tmp.path().join("data");
        let project = tmp.path().join("repo");
        let project_claude = project.join(".claude");
        let skill = project_claude.join("skills/proj-skill");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "# project skill\n").unwrap();
        std::fs::create_dir_all(&user_claude).unwrap();

        let item = sample_item(ScopeKind::Project, skill.to_str().unwrap(), true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::RecoverableBeforeDelete,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let runner = Arc::new(FakeProcessRunner::new());
        let disable_ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner: runner.clone(),
            claude_config_dir: Some(user_claude.clone()),
            data_dir: Some(data.clone()),
        };
        let out = ClaudeTargetExecutor
            .execute_change(&disable_ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(matches!(out, TargetActionRawOutcome::Applied));

        let disabled = project_claude.join("disabled/skills/proj-skill");
        assert!(
            disabled.is_dir(),
            "disabled skill should live under project .claude/disabled"
        );
        assert!(!skill.exists(), "active project skill path should be empty");
        let user_disabled = data.join("claude-assets/disabled/skills/proj-skill");
        assert!(
            !user_disabled.exists(),
            "must not promote project disable into user hub disabled"
        );

        let enable_change = PortableAssetActionChangeDto {
            operation: PortableAssetPlanOperation::Enable,
            path: Some(disabled.to_string_lossy().into_owned()),
            ..change.clone()
        };
        let mut disabled_item = item.clone();
        disabled_item.source_path = Some(disabled.to_string_lossy().into_owned());
        disabled_item.actual_enabled = Some(false);
        let enable_ctx = TargetActionContext {
            action: PortableAssetActionKind::Enable,
            keep_data: false,
            runner: runner.clone(),
            claude_config_dir: Some(user_claude.clone()),
            data_dir: Some(data.clone()),
        };
        let out = ClaudeTargetExecutor
            .execute_change(
                &enable_ctx,
                &dummy_plan(),
                &enable_change,
                Some(&disabled_item),
            )
            .unwrap();
        assert!(matches!(out, TargetActionRawOutcome::Applied));
        assert!(
            skill.is_dir(),
            "enable must restore under project skills path"
        );
        assert!(
            skill.join("SKILL.md").is_file(),
            "project skill content restored"
        );
        assert!(!disabled.exists());
    }

    /// Business Logic: 已停用 Skill 的真树在 claude-assets/disabled，native 根是空的。
    ///     迁入便携仓库必须从 disabled 搬出，并在 ~/.claude/skills/<id> 留下软链。
    #[test]
    fn migrate_disabled_user_skill_moves_tree_and_attaches_native_link() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::{
            TargetActionContext, TargetActionRawOutcome,
        };
        use crate::agent_hub::portable_store::{portable_store_root, store_skill_dir};
        use crate::agent_hub::targets::portable::DATA_DIR_ENV_LOCK;
        use std::sync::Arc;

        let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let user_claude = tmp.path().join("user-claude");
        let data = tmp.path().join("data");
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        std::fs::create_dir_all(&user_claude).unwrap();
        let disabled = data.join("claude-assets/disabled/skills/foo");
        std::fs::create_dir_all(&disabled).unwrap();
        std::fs::write(
            disabled.join("SKILL.md"),
            "---\nname: foo\nversion: 1.0.0\n---\nfrom-disabled\n",
        )
        .unwrap();

        let mut item = sample_item(ScopeKind::User, disabled.to_str().unwrap(), false);
        item.native_id = "foo".into();
        item.display_name = "foo".into();
        item.actual_enabled = Some(false);
        item.project_id = None;
        item.scope_id = "user".into();
        item.content_hash = None;

        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::MigrateToStore,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::MigrateToStore,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: Some(user_claude.clone()),
            data_dir: Some(data.clone()),
        };
        let out = ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(
            matches!(out, TargetActionRawOutcome::Applied),
            "expected Applied, got {out:?}"
        );

        let native = user_claude.join("skills/foo");
        assert!(
            std::fs::symlink_metadata(&native)
                .unwrap()
                .file_type()
                .is_symlink(),
            "native path should become a store symlink"
        );
        let store_tree = store_skill_dir(&portable_store_root(&data), "foo");
        let text = std::fs::read_to_string(store_tree.join("SKILL.md")).unwrap();
        assert!(text.contains("from-disabled"));
        assert!(
            !disabled.exists(),
            "disabled copy must be moved into the store"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
    }

    /// Business Logic: project-scope plugin CLI must run with project root cwd.
    #[test]
    fn project_plugin_cli_sets_process_cwd_to_project_root() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::TargetActionContext;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("my-repo");
        let plugin_path = project.join(".claude/plugins/demo");
        std::fs::create_dir_all(&plugin_path).unwrap();
        let mut item = sample_item(ScopeKind::Project, plugin_path.to_str().unwrap(), true);
        item.kind = PortableAssetKind::Plugin;
        item.native_id = "demo@local".into();
        item.display_name = "demo".into();
        item.inventory_item_id = "id-proj-plugin".into();

        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Plugin,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner: runner.clone(),
            claude_config_dir: None,
            data_dir: None,
        };
        ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cwd.as_deref(), Some(project.as_path()));
        let scope_idx = calls[0].args.iter().position(|a| a == "--scope").unwrap();
        assert_eq!(calls[0].args[scope_idx + 1], "project");
    }

    /// Business Logic: 短 native_id 必须还原成 id@marketplace，否则 CLI 只卸掉其中一个源。
    #[test]
    fn plugin_uninstall_uses_marketplace_qualified_selector() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::TargetActionContext;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let plugin_path = tmp
            .path()
            .join("plugins/cache/claude-plugins-official/superpowers/6.3.0");
        std::fs::create_dir_all(&plugin_path).unwrap();
        let mut item = sample_item(ScopeKind::User, plugin_path.to_str().unwrap(), true);
        item.kind = PortableAssetKind::Plugin;
        item.native_id = "superpowers".into();
        item.display_name = "superpowers".into();
        item.inventory_item_id = "id-superpowers".into();

        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Plugin,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Uninstall,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Uninstall,
            keep_data: false,
            runner: runner.clone(),
            claude_config_dir: Some(tmp.path().join("claude")),
            data_dir: Some(tmp.path().join("data")),
        };
        ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        let args = &runner.calls()[0].args;
        assert!(args.iter().any(|a| a == "uninstall"));
        assert!(
            args.iter()
                .any(|a| a == "superpowers@claude-plugins-official"),
            "uninstall argv must use qualified selector, got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "superpowers"),
            "short id must not be passed when path encodes marketplace, got {args:?}"
        );
    }

    /// Business Logic: Claude CLI 对已禁用插件返回 exit 1，Hub 必须当成幂等跳过。
    #[test]
    fn plugin_disable_already_disabled_is_skipped() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::{
            TargetActionContext, TargetActionRawOutcome,
        };
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let plugin_path = tmp
            .path()
            .join("cache/claude-plugins-official/superpowers/6.3.0");
        std::fs::create_dir_all(&plugin_path).unwrap();
        let mut item = sample_item(ScopeKind::User, plugin_path.to_str().unwrap(), true);
        item.kind = PortableAssetKind::Plugin;
        item.native_id = "superpowers".into();
        item.display_name = "superpowers".into();
        item.inventory_item_id = "id-superpowers".into();

        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Plugin,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_err(
            1,
            r#"✘ Failed to disable plugin "superpowers": Plugin "superpowers" is already disabled at user scope"#,
        );
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner,
            claude_config_dir: Some(tmp.path().join("claude")),
            data_dir: Some(tmp.path().join("data")),
        };
        let out = ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(
            matches!(out, TargetActionRawOutcome::Skipped),
            "already-disabled CLI must skip, got {out:?}"
        );
    }

    #[test]
    fn plugin_disable_other_cli_error_still_fails() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::{
            TargetActionContext, TargetActionRawOutcome,
        };
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let plugin_path = tmp.path().join("plugins/demo");
        std::fs::create_dir_all(&plugin_path).unwrap();
        let mut item = sample_item(ScopeKind::User, plugin_path.to_str().unwrap(), true);
        item.kind = PortableAssetKind::Plugin;
        item.native_id = "demo".into();

        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Plugin,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_err(1, "claude plugin: marketplace unreachable");
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner,
            claude_config_dir: Some(tmp.path().join("claude")),
            data_dir: Some(tmp.path().join("data")),
        };
        let out = ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        match out {
            TargetActionRawOutcome::Failed { code, .. } => {
                assert_eq!(code, "PORTABLE_ASSET_ACTION_CLI_FAILED");
            }
            other => panic!("expected CLI_FAILED, got {other:?}"),
        }
    }

    fn dummy_plan() -> PortableAssetActionPlanDto {
        PortableAssetActionPlanDto {
            plan_token: "tok".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
            action: PortableAssetActionKind::Disable,
            inventory_snapshot_hash: "h".into(),
            keep_data: false,
            conflict_policy: crate::agent_hub::portable_actions::models::PortableAssetConflictPolicy::SkipExisting,
            changes: vec![],
            blocking_reasons: vec![],
        }
    }

    /// R5-M4: source-hash recheck must fail-closed when hash computation errors.
    #[test]
    fn source_hash_recheck_fails_closed_when_hash_unavailable() {
        use crate::agent_hub::packages::activator::FakeProcessRunner;
        use crate::agent_hub::portable_actions::targets::{
            TargetActionContext, TargetActionRawOutcome,
        };
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        // Directory without SKILL.md → hash_skill_directory Err → must not mutate
        let skill_dir = tmp.path().join("skills").join("broken-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        // no SKILL.md

        let change = PortableAssetActionChangeDto {
            inventory_item_id: "id-broken".into(),
            target: crate::agent_hub::models::AgentTarget::Claude,
            kind: PortableAssetKind::Skill,
            path: Some(skill_dir.to_string_lossy().into_owned()),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: Some("expected-hash".into()),
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let ctx = TargetActionContext {
            runner: Arc::new(FakeProcessRunner::default()),
            claude_config_dir: Some(tmp.path().join("claude")),
            data_dir: Some(tmp.path().join("data")),
            keep_data: false,
            action: PortableAssetActionKind::Disable,
        };
        let out = ClaudeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, None)
            .unwrap();
        match out {
            TargetActionRawOutcome::Failed { code, .. } => {
                assert_eq!(
                    code, "PORTABLE_ASSET_ACTION_SOURCE_HASH_UNAVAILABLE",
                    "hash Err must fail-closed with UNAVAILABLE, got {code}"
                );
            }
            other => panic!("expected Failed(UNAVAILABLE), got {other:?}"),
        }
        // Soft-skip contract must not remain in production source (exclude this tests module).
        let src = include_str!("claude.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("production source before tests");
        let soft_skip = format!(
            "{}{}",
            "if let Ok(actual) = ", "inventory_content_hash_for_path"
        );
        assert!(
            !prod.contains(&soft_skip),
            "soft Ok-skip of hash recheck must be removed"
        );
        assert!(prod.contains("PORTABLE_ASSET_ACTION_SOURCE_HASH_UNAVAILABLE"));
        assert!(prod.contains("match inventory_content_hash_for_path"));
    }
}
