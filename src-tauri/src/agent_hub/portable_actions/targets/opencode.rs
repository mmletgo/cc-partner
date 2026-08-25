//! portable_actions/targets/opencode — OpenCode / Grok / Gemini / Cursor / Pi
//!
//! Business Logic（为什么需要这个模块）:
//!     这些 target 的 CLI 写能力尚未认证，启停/卸载必须 fail-closed 且零 spawn。
//!     Skill/Command 仓库软链只改本机文件，不依赖 CLI，必须能附加/从此 Agent 卸下。
//!     Grok Plugin 与 Grok/Gemini/Cursor/OpenCode 自身 MCP 的 viewing 启停只改配置
//!     文件，同样零 spawn。
//!
//! Code Logic（这个模块做什么）:
//!     store 动作走 `execute_skill_or_command_store`；Grok Plugin Enable/Disable 写
//!     `config.toml` `[plugins]` 数组；自身 MCP Enable/Disable 翻 leaf `enabled`；
//!     借用 MCP 与其余动作仍返回 write-not-certified。

use super::{
    is_file_only_viewing_toggle, TargetActionContext, TargetActionExecutor, TargetActionRawOutcome,
};
use crate::agent_hub::config_patch::{
    apply_config_patch_atomically, JsoncConfigPatcher, ManagedConfigPatch, SemanticConfigPatcher,
    TomlConfigPatcher, CAS_EXPECT_ABSENT,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
};
use crate::agent_hub::portable_inventory::plugin_enablement::plugin_config_key_matches;
use crate::agent_hub::portable_inventory::plugin_paths::plugin_cli_selector;
use crate::agent_hub::portable_inventory::{
    PortableAssetKind, PortableInventoryItemDto, PortableOriginKind,
};
use crate::agent_hub::portable_store::{
    current_portable_store_root, execute_skill_or_command_store, is_under_portable_store,
};
use crate::agent_hub::targets::paths::{TargetEnvironment, TargetHomes, TargetPathResolver};
use crate::error::AppError;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// OpenCode 及共用 fail-closed executor（Grok/Gemini/Cursor/Pi 也走这里）。
pub struct OpenCodeTargetExecutor;

impl TargetActionExecutor for OpenCodeTargetExecutor {
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
        if ctx.action.is_portable_store_action()
            && matches!(
                change.kind,
                PortableAssetKind::Skill | PortableAssetKind::Command
            )
        {
            let id = native_id(change, pre_item);
            let native_path = native_store_mount(change.target, change.kind, &id, change);
            return execute_skill_or_command_store(
                change.target,
                ctx.action,
                change.kind,
                &id,
                &native_path,
                pre_item,
            );
        }
        if is_file_only_viewing_toggle(change.target, change.kind, ctx.action) {
            match change.kind {
                PortableAssetKind::Plugin => {
                    let config = grok_config_toml_path(pre_item, change);
                    let native =
                        pre_item.is_some_and(|item| item.origin_kind == PortableOriginKind::Native);
                    let source = change
                        .path
                        .as_deref()
                        .or_else(|| pre_item.and_then(|item| item.source_path.as_deref()));
                    let id = plugin_cli_selector(&native_id(change, pre_item), source);
                    return set_grok_plugin_enabled_in_config(
                        &config,
                        &id,
                        native,
                        matches!(ctx.action, PortableAssetActionKind::Enable),
                    );
                }
                PortableAssetKind::Mcp => {
                    if pre_item.is_some_and(|item| item.origin_kind != PortableOriginKind::Native) {
                        return Ok(TargetActionRawOutcome::Blocked {
                            code: "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into(),
                            message: "borrowed MCP enable/disable stays blocked on this executor"
                                .into(),
                        });
                    }
                    return execute_native_mcp_enabled_toggle(ctx.action, change, pre_item);
                }
                PortableAssetKind::Skill | PortableAssetKind::Command => {}
            }
        }
        Ok(TargetActionRawOutcome::Blocked {
            code: "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into(),
            message: "opencode portable mutation blocked until manifest evidence allows".into(),
        })
    }
}

/// 解析 viewing Agent 上应挂/拆的 native 路径。
///
/// Business Logic: 已附加项用库存观测路径；未附加的仓库真树不得当成挂载点。
/// Code Logic: change.path 不在 portable-store 内则用之，否则拼 config_root/skills|commands。
fn native_store_mount(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    change: &PortableAssetActionChangeDto,
) -> PathBuf {
    if let Some(path) = change.path.as_deref().map(Path::new) {
        // 已附加软链 canonicalize 会走进 store 真树；软链本身才是要拆的挂载点。
        if fs::symlink_metadata(path)
            .ok()
            .is_some_and(|m| m.file_type().is_symlink())
        {
            return path.to_path_buf();
        }
        let under_store =
            current_portable_store_root().is_some_and(|root| is_under_portable_store(path, &root));
        if !under_store {
            return path.to_path_buf();
        }
    }
    native_mount_from_homes(target, kind, native_id)
}

/// 按 target 配置根拼 native skills/commands 挂载点。
fn native_mount_from_homes(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
) -> PathBuf {
    let homes = TargetPathResolver::resolve_all(&TargetEnvironment::from_process());
    let root = config_root_for(target, &homes);
    match kind {
        PortableAssetKind::Command => root.join("commands").join(format!("{native_id}.md")),
        _ => root.join("skills").join(native_id),
    }
}

fn config_root_for(target: AgentTarget, homes: &TargetHomes) -> PathBuf {
    match target {
        AgentTarget::Claude => homes.claude.config_root.clone(),
        AgentTarget::Codex => homes.codex.config_root.clone(),
        AgentTarget::OpenCode => homes.opencode.config_root.clone(),
        AgentTarget::Grok => homes.grok.config_root.clone(),
        AgentTarget::Gemini => homes.gemini.config_root.clone(),
        AgentTarget::Cursor => homes.cursor.config_root.clone(),
        AgentTarget::Pi => homes.pi.config_root.clone(),
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

/// 解析 Grok `config.toml` 落点。
///
/// Business Logic（为什么需要这个函数）:
///     Plugin 启停只写 viewing Agent 的 Grok 配置，测试与项目 scope 必须跟观测路径走，
///     不得默默改开发者真实 `~/.grok`。
///
/// Code Logic（这个函数做什么）:
///     优先 `change.path` / `pre_item.source_path`：文件名是 `config.toml`、祖先名为
///     `.grok`、或祖先目录已有 `config.toml`；否则回落 `GROK_HOME|~/.grok/config.toml`。
fn grok_config_toml_path(
    pre_item: Option<&PortableInventoryItemDto>,
    change: &PortableAssetActionChangeDto,
) -> PathBuf {
    for raw in [
        change.path.as_deref(),
        pre_item.and_then(|item| item.source_path.as_deref()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(path) = grok_config_toml_from_observed(Path::new(raw)) {
            return path;
        }
    }
    TargetPathResolver::resolve_all(&TargetEnvironment::from_process())
        .grok
        .config_root
        .join("config.toml")
}

/// 从观测路径推断 `{grokConfigRoot}/config.toml`。
///
/// Business Logic（为什么需要这个函数）:
///     Plugin `source_path` 通常是包目录而不是配置文件；必须能从 `.grok/` 祖先回到 toml。
///
/// Code Logic（这个函数做什么）:
///     文件名 `config.toml` 直接返回；祖先名为 `.grok` 则拼 `config.toml`；
///     否则若某祖先已有该文件则用之。
fn grok_config_toml_from_observed(path: &Path) -> Option<PathBuf> {
    if path.file_name().is_some_and(|name| name == "config.toml") {
        return Some(path.to_path_buf());
    }
    for ancestor in path.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == ".grok") {
            return Some(ancestor.join("config.toml"));
        }
        let candidate = ancestor.join("config.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 在 Grok `config.toml` 写 `[plugins] enabled/disabled` 数组。
///
/// Business Logic（为什么需要这个函数）:
///     Grok 以 disabled 黑名单为主；native 另有可选 enabled 白名单。借用包 Enable
///     不得被写进白名单，否则扫描侧会把「没进 enabled」当成关。
///
/// Code Logic（这个函数做什么）:
///     Disable：id 合入 `disabled`（去重），并从 `enabled` 移除匹配项。Enable：从
///     `disabled` 移除；仅 `native && enabled 非空` 才合入 `enabled`。用
///     `TomlConfigPatcher` 原子 patch JSON 数组；缺 `[plugins]` 则创建。
fn set_grok_plugin_enabled_in_config(
    config_path: &Path,
    plugin_id: &str,
    native: bool,
    enabled: bool,
) -> Result<TargetActionRawOutcome, AppError> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_PLUGIN_ID_MISSING".into(),
            message: "grok plugin id missing".into(),
        });
    }
    let bytes = if config_path.exists() {
        fs::read(config_path)?
    } else {
        Vec::new()
    };
    let patcher = TomlConfigPatcher;
    let (mut disabled, disabled_hash, disabled_present) =
        inspect_toml_string_array(&patcher, &bytes, &["plugins".into(), "disabled".into()])?;
    let (mut enabled_list, enabled_hash, enabled_present) =
        inspect_toml_string_array(&patcher, &bytes, &["plugins".into(), "enabled".into()])?;
    let whitelist_mode = native && !enabled_list.is_empty();

    let mut patch_disabled = false;
    let mut patch_enabled = false;
    if enabled {
        let before = disabled.len();
        disabled.retain(|key| !plugin_config_key_matches(plugin_id, key));
        if disabled.len() != before {
            patch_disabled = true;
        }
        if whitelist_mode && !grok_plugin_array_contains(&enabled_list, plugin_id) {
            enabled_list.push(plugin_id.to_string());
            patch_enabled = true;
        }
    } else if !grok_plugin_array_contains(&disabled, plugin_id) {
        disabled.push(plugin_id.to_string());
        patch_disabled = true;
        let before = enabled_list.len();
        enabled_list.retain(|key| !plugin_config_key_matches(plugin_id, key));
        if enabled_list.len() != before {
            patch_enabled = true;
        }
    } else {
        let before = enabled_list.len();
        enabled_list.retain(|key| !plugin_config_key_matches(plugin_id, key));
        if enabled_list.len() != before {
            patch_enabled = true;
        }
    }
    if !patch_disabled && !patch_enabled {
        return Ok(TargetActionRawOutcome::Skipped);
    }

    let mut patches = Vec::new();
    if patch_disabled {
        patches.push(plugin_array_patch(
            plugin_id,
            "disabled",
            &disabled,
            disabled_present,
            disabled_hash,
        ));
    }
    if patch_enabled {
        patches.push(plugin_array_patch(
            plugin_id,
            "enabled",
            &enabled_list,
            enabled_present,
            enabled_hash,
        ));
    }
    config_flag_patch_outcome(
        apply_config_patch_atomically(&patcher, config_path, &patches)?,
        "PORTABLE_ASSET_ACTION_PLUGIN_CAS_CONFLICT",
        "grok plugin enable CAS conflict",
        "PORTABLE_ASSET_ACTION_PLUGIN_PATCH_FAILED",
    )
}

/// 构造 `[plugins].{leaf}` 字符串数组 patch。
fn plugin_array_patch(
    plugin_id: &str,
    leaf: &str,
    values: &[String],
    present: bool,
    hash: Option<String>,
) -> ManagedConfigPatch {
    ManagedConfigPatch {
        owner_id: format!("portable-grok-plugin:{plugin_id}"),
        path: vec!["plugins".into(), leaf.into()],
        value: Some(serde_json::Value::Array(
            values
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        )),
        expected_base_hash: if present {
            hash
        } else {
            Some(CAS_EXPECT_ABSENT.to_string())
        },
    }
}

/// 读取 TOML 字符串数组；缺失视为空。
fn inspect_toml_string_array(
    patcher: &TomlConfigPatcher,
    bytes: &[u8],
    path: &[String],
) -> Result<(Vec<String>, Option<String>, bool), AppError> {
    let owned = patcher.inspect(bytes, path)?;
    if !owned.present {
        return Ok((Vec::new(), None, false));
    }
    Ok((json_string_list(&owned.value), owned.value_hash, true))
}

fn json_string_list(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

fn grok_plugin_array_contains(list: &[String], plugin_id: &str) -> bool {
    list.iter()
        .any(|key| plugin_config_key_matches(plugin_id, key))
}

/// 执行自身 MCP `enabled` 翻转。
///
/// Business Logic（为什么需要这个函数）:
///     Grok/Gemini/Cursor/OpenCode 的 native MCP 用 leaf `enabled` 开关；缺字段视为开。
///     不得删 leaf、不得走 Claude 那种 disabled 快照。
///
/// Code Logic（这个函数做什么）:
///     按 target 选 toml `mcp_servers.{id}.enabled` 或 jsonc `mcpServers.{id}.enabled`；
///     配置路径优先库存观测文件，否则回落各家 home。
fn execute_native_mcp_enabled_toggle(
    action: PortableAssetActionKind,
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> Result<TargetActionRawOutcome, AppError> {
    let id = native_id(change, pre_item);
    let enabled = matches!(action, PortableAssetActionKind::Enable);
    let path = mcp_config_path(change.target, pre_item, change);
    match change.target {
        AgentTarget::Grok => set_mcp_enabled_toml(&path, "mcp_servers", &id, enabled),
        AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::OpenCode => {
            set_mcp_enabled_jsonc(&path, "mcpServers", &id, enabled)
        }
        AgentTarget::Claude | AgentTarget::Codex | AgentTarget::Pi => {
            Ok(TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into(),
                message: "mcp enabled toggle is not file-only on this target".into(),
            })
        }
    }
}

/// 解析 MCP 配置文件路径。
///
/// Business Logic（为什么需要这个函数）:
///     scanner 把 MCP `source_path` 指到配置文件本身；测试用 tempfile 必须优先生效。
///
/// Code Logic（这个函数做什么）:
///     观测路径若是文件或匹配该 target 的配置文件名则用之；否则 Grok `config.toml`、
///     Gemini `settings.json`、Cursor `mcp.json`、OpenCode `opencode.json(c)`。
fn mcp_config_path(
    target: AgentTarget,
    pre_item: Option<&PortableInventoryItemDto>,
    change: &PortableAssetActionChangeDto,
) -> PathBuf {
    for raw in [
        pre_item.and_then(|item| item.source_path.as_deref()),
        change.path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuf::from(raw);
        if path.is_file()
            || path
                .file_name()
                .is_some_and(|name| is_mcp_config_filename(target, name))
        {
            return path;
        }
    }
    default_mcp_config_path(target)
}

fn is_mcp_config_filename(target: AgentTarget, name: &OsStr) -> bool {
    match target {
        AgentTarget::Grok => name == "config.toml",
        AgentTarget::Gemini => name == "settings.json",
        AgentTarget::Cursor => name == "mcp.json",
        AgentTarget::OpenCode => name == "opencode.json" || name == "opencode.jsonc",
        AgentTarget::Claude | AgentTarget::Codex | AgentTarget::Pi => false,
    }
}

fn default_mcp_config_path(target: AgentTarget) -> PathBuf {
    let homes = TargetPathResolver::resolve_all(&TargetEnvironment::from_process());
    match target {
        AgentTarget::Grok => homes.grok.config_root.join("config.toml"),
        AgentTarget::Gemini => homes.gemini.config_root.join("settings.json"),
        AgentTarget::Cursor => homes.cursor.config_root.join("mcp.json"),
        AgentTarget::OpenCode => {
            let jsonc = homes.opencode.config_root.join("opencode.jsonc");
            if jsonc.is_file() {
                return jsonc;
            }
            let json = homes.opencode.config_root.join("opencode.json");
            if json.is_file() {
                return json;
            }
            homes.opencode.config_file.clone()
        }
        other => config_root_for(other, &homes).join("config.toml"),
    }
}

/// 翻转 TOML MCP leaf `enabled`（缺字段视为 true；Disable 插入 false，不删 leaf）。
///
/// Business Logic（为什么需要这个函数）:
///     与 Codex `set_codex_mcp_enabled_flag` 同一语义，供 Grok `mcp_servers.{id}` 复用。
///
/// Code Logic（这个函数做什么）:
///     路径 `[table_key, id, "enabled"]`；父 leaf 不存在则 Skip。
fn set_mcp_enabled_toml(
    config_path: &Path,
    table_key: &str,
    id: &str,
    enabled: bool,
) -> Result<TargetActionRawOutcome, AppError> {
    set_mcp_leaf_enabled_flag(&TomlConfigPatcher, config_path, table_key, id, enabled, b"")
}

/// 翻转 JSONC MCP leaf `enabled`（Gemini settings.json / Cursor mcp.json / OpenCode）。
///
/// Business Logic（为什么需要这个函数）:
///     JSONC 必须走 `JsoncConfigPatcher` 才能保留注释；语义与 TOML MCP 开关一致。
///
/// Code Logic（这个函数做什么）:
///     路径 `[object_key, id, "enabled"]`；空文件按 `{}` 起步。
fn set_mcp_enabled_jsonc(
    config_path: &Path,
    object_key: &str,
    id: &str,
    enabled: bool,
) -> Result<TargetActionRawOutcome, AppError> {
    set_mcp_leaf_enabled_flag(
        &JsoncConfigPatcher,
        config_path,
        object_key,
        id,
        enabled,
        b"{}",
    )
}

/// 通用 MCP `enabled` 布尔 patch。
///
/// Business Logic（为什么需要这个函数）:
///     缺省无 `enabled` 视为开；Disable 必须插入 `false` 而不是删掉 server 对象。
///
/// Code Logic（这个函数做什么）:
///     先确认 servers.{id} 存在；再 CAS patch `.enabled`。已是目标值则 Skip。
fn set_mcp_leaf_enabled_flag(
    patcher: &dyn SemanticConfigPatcher,
    config_path: &Path,
    servers_key: &str,
    id: &str,
    enabled: bool,
    missing_seed: &[u8],
) -> Result<TargetActionRawOutcome, AppError> {
    let bytes = if config_path.exists() {
        fs::read(config_path)?
    } else {
        missing_seed.to_vec()
    };
    let parent = patcher.inspect(&bytes, &[servers_key.into(), id.to_string()])?;
    if !parent.present {
        return Ok(TargetActionRawOutcome::Skipped);
    }
    let path = vec![servers_key.into(), id.to_string(), "enabled".into()];
    let owned = patcher.inspect(&bytes, &path)?;
    let patch = if owned.present {
        if owned.value.as_bool() == Some(enabled) {
            return Ok(TargetActionRawOutcome::Skipped);
        }
        ManagedConfigPatch {
            owner_id: format!("portable-mcp:{id}"),
            path,
            value: Some(serde_json::Value::Bool(enabled)),
            expected_base_hash: owned.value_hash,
        }
    } else if enabled {
        return Ok(TargetActionRawOutcome::Skipped);
    } else {
        ManagedConfigPatch {
            owner_id: format!("portable-mcp:{id}"),
            path,
            value: Some(serde_json::Value::Bool(false)),
            expected_base_hash: Some(CAS_EXPECT_ABSENT.to_string()),
        }
    };
    config_flag_patch_outcome(
        apply_config_patch_atomically(patcher, config_path, &[patch])?,
        "PORTABLE_ASSET_ACTION_MCP_CAS_CONFLICT",
        if enabled {
            "mcp enable CAS conflict"
        } else {
            "mcp disable CAS conflict"
        },
        "PORTABLE_ASSET_ACTION_MCP_PATCH_FAILED",
    )
}

/// 把 config patch 结果收成 executor outcome。
///
/// Business Logic（为什么需要这个函数）:
///     CAS 冲突必须可区分，禁止当成成功。
///
/// Code Logic（这个函数做什么）:
///     Applied / Conflict / 其它失败码。
fn config_flag_patch_outcome(
    prepared: crate::agent_hub::config_patch::PreparedConfigProjection,
    conflict_code: &str,
    conflict_message: &str,
    fail_code: &str,
) -> Result<TargetActionRawOutcome, AppError> {
    match prepared.patched.outcome {
        crate::agent_hub::config_patch::ConfigPatchOutcome::Applied => {
            Ok(TargetActionRawOutcome::Applied)
        }
        crate::agent_hub::config_patch::ConfigPatchOutcome::Conflict { .. } => {
            Ok(TargetActionRawOutcome::Failed {
                code: conflict_code.into(),
                message: conflict_message.into(),
            })
        }
        other => Ok(TargetActionRawOutcome::Failed {
            code: fail_code.into(),
            message: format!("{other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::ScopeKind;
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    use crate::agent_hub::portable_actions::models::{
        PortableAssetBackupPolicy, PortableAssetCanonicalEffect, PortableAssetPlanOperation,
    };
    use crate::agent_hub::portable_inventory::{
        PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin, PortableOriginKind, PortableStoreFactDto,
    };
    use crate::agent_hub::portable_store::{
        attach_store_link, ensure_portable_store_layout, portable_store_root, store_skill_dir,
    };
    use std::sync::Arc;

    fn dummy_plan() -> PortableAssetActionPlanDto {
        PortableAssetActionPlanDto {
            plan_token: "tok".into(),
            expires_at: "now".into(),
            inventory_snapshot_hash: "hash".into(),
            action: PortableAssetActionKind::Detach,
            keep_data: false,
            conflict_policy:
                crate::agent_hub::portable_actions::models::PortableAssetConflictPolicy::SkipExisting,
            changes: vec![],
            blocking_reasons: vec![],
        }
    }

    fn sample_item(path: &str, attached: bool) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: "opencode-skill-media-use".into(),
            target: AgentTarget::OpenCode,
            loaded_by: AgentTarget::OpenCode,
            owned_by: PortableAssetOwner::PortableStore,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Skill,
            native_id: "media-use".into(),
            display_name: "media-use".into(),
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
            content_hash: Some("h".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::HubManaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_detach: true,
                ..PortableInventoryItemCapabilitiesDto::default()
            },
            warnings: vec![],
            mcp_credential: None,
            store: PortableStoreFactDto {
                store_id: Some("skill:media-use".into()),
                store_attached: attached,
                loaded_via_other_path: false,
                loaded_via_target: None,
            },
        }
    }

    /// Business Logic: OpenCode 仓库项必须能从此 Agent 卸下，不得被 CLI 未认证挡住。
    /// Code Logic: 在 opencode skills 根建 store 软链，Detach 后只剩仓库真树。
    #[test]
    fn detach_store_skill_unlinks_opencode_native_without_cli() {
        let _guard = crate::agent_hub::targets::portable::DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let oc = tmp.path().join("opencode");
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        std::env::set_var("OPENCODE_CONFIG_DIR", &oc);
        let store_root = ensure_portable_store_layout(&data).unwrap();
        let store_tree = store_skill_dir(&store_root, "media-use");
        std::fs::create_dir_all(&store_tree).unwrap();
        std::fs::write(store_tree.join("SKILL.md"), "---\nname: media-use\n---\n").unwrap();
        let native = oc.join("skills/media-use");
        attach_store_link(&store_tree, &native).unwrap();
        assert!(std::fs::symlink_metadata(&native)
            .unwrap()
            .file_type()
            .is_symlink());

        let item = sample_item(native.to_str().unwrap(), true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: AgentTarget::OpenCode,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Detach,
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
            action: PortableAssetActionKind::Detach,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data.clone()),
        };
        let out = OpenCodeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(
            matches!(out, TargetActionRawOutcome::Applied),
            "expected Applied, got {out:?}"
        );
        assert!(!native.exists(), "OpenCode native symlink must be removed");
        assert!(
            store_tree.join("SKILL.md").is_file(),
            "store tree must remain"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
        std::env::remove_var("OPENCODE_CONFIG_DIR");
        let _ = portable_store_root(&data);
    }

    /// Business Logic: 启停仍要求 CLI 写认证，不得假装成功。
    #[test]
    fn disable_stays_blocked_without_cli_certification() {
        let item = sample_item("/tmp/opencode/skills/media-use", true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: AgentTarget::OpenCode,
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
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: None,
        };
        let out = OpenCodeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(matches!(
            out,
            TargetActionRawOutcome::Blocked { ref code, .. }
                if code == "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED"
        ));
    }

    fn viewing_change(
        target: AgentTarget,
        kind: PortableAssetKind,
        native_id: &str,
        path: &str,
        operation: PortableAssetPlanOperation,
    ) -> PortableAssetActionChangeDto {
        PortableAssetActionChangeDto {
            inventory_item_id: format!("{target:?}-{native_id}"),
            target,
            kind,
            path: Some(path.into()),
            operation,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        }
    }

    fn viewing_item(
        target: AgentTarget,
        kind: PortableAssetKind,
        native_id: &str,
        path: &str,
        origin_kind: PortableOriginKind,
    ) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: format!("{target:?}-{native_id}"),
            target,
            loaded_by: target,
            owned_by: match origin_kind {
                PortableOriginKind::Native => PortableAssetOwner::from_target(target),
                _ => PortableAssetOwner::Claude,
            },
            origin_kind,
            native_output_candidate: origin_kind == PortableOriginKind::Native,
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
            store: PortableStoreFactDto::default(),
        }
    }

    fn viewing_ctx(action: PortableAssetActionKind) -> TargetActionContext {
        TargetActionContext {
            action,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: None,
        }
    }

    fn toml_string_array(doc: &toml_edit::DocumentMut, key: &str) -> Vec<String> {
        doc.get("plugins")
            .and_then(|item| item.as_table())
            .and_then(|plugins| plugins.get(key))
            .and_then(|item| item.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Business Logic: Disable 只追加 disabled，不得改写其它键或 native 白名单。
    /// Code Logic: tempfile config.toml enabled=["native-only"]，Disable superpowers@market。
    #[test]
    fn grok_plugin_disable_appends_disabled_without_touching_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            r#"
model = "keep-me"

[plugins]
enabled = ["native-only"]
disabled = []
keep_flag = true
"#,
        )
        .unwrap();
        let path = config.to_str().unwrap();
        let out =
            set_grok_plugin_enabled_in_config(&config, "superpowers@market", true, false).unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after = std::fs::read_to_string(&config).unwrap();
        let doc: toml_edit::DocumentMut = after.parse().unwrap();
        assert_eq!(
            doc.get("model").and_then(|item| item.as_str()),
            Some("keep-me")
        );
        assert_eq!(
            doc.get("plugins")
                .and_then(|item| item.as_table())
                .and_then(|plugins| plugins.get("keep_flag"))
                .and_then(|item| item.as_bool()),
            Some(true)
        );
        let disabled = toml_string_array(&doc, "disabled");
        let enabled = toml_string_array(&doc, "enabled");
        assert!(
            disabled.iter().any(|id| id == "superpowers@market"),
            "disabled must contain superpowers@market: {after}"
        );
        assert_eq!(enabled, vec!["native-only".to_string()]);

        let item = viewing_item(
            AgentTarget::Grok,
            PortableAssetKind::Plugin,
            "superpowers@market",
            path,
            PortableOriginKind::Native,
        );
        let change = viewing_change(
            AgentTarget::Grok,
            PortableAssetKind::Plugin,
            "superpowers@market",
            path,
            PortableAssetPlanOperation::Disable,
        );
        let again = OpenCodeTargetExecutor
            .execute_change(
                &viewing_ctx(PortableAssetActionKind::Disable),
                &dummy_plan(),
                &change,
                Some(&item),
            )
            .unwrap();
        assert_eq!(again, TargetActionRawOutcome::Skipped);
    }

    /// Business Logic: 借用包 Enable 只从 disabled 移除，不得写入 native 白名单。
    /// Code Logic: disabled=["ecc"] 且 native=false，Enable 后 disabled 空、enabled 不变。
    #[test]
    fn grok_plugin_enable_removes_from_disabled_skips_whitelist_for_borrowed() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            r#"
[plugins]
enabled = ["native-only"]
disabled = ["ecc"]
"#,
        )
        .unwrap();
        let out = set_grok_plugin_enabled_in_config(&config, "ecc", false, true).unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after = std::fs::read_to_string(&config).unwrap();
        let doc: toml_edit::DocumentMut = after.parse().unwrap();
        assert!(
            toml_string_array(&doc, "disabled").is_empty(),
            "disabled must be empty: {after}"
        );
        assert_eq!(
            toml_string_array(&doc, "enabled"),
            vec!["native-only".to_string()]
        );
    }

    /// Business Logic: Grok 自身 MCP Disable 只插 enabled=false，保留 sibling。
    /// Code Logic: tempfile `[mcp_servers.good-api]` 无 enabled 字段，经 executor Disable。
    #[test]
    fn grok_native_mcp_disable_sets_enabled_false() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            r#"
[mcp_servers.good-api]
command = "uvx"
token = "keep-sibling"

[mcp_servers.other]
command = "echo"
"#,
        )
        .unwrap();
        let path = config.to_str().unwrap();
        let item = viewing_item(
            AgentTarget::Grok,
            PortableAssetKind::Mcp,
            "good-api",
            path,
            PortableOriginKind::Native,
        );
        let change = viewing_change(
            AgentTarget::Grok,
            PortableAssetKind::Mcp,
            "good-api",
            path,
            PortableAssetPlanOperation::Disable,
        );
        let out = OpenCodeTargetExecutor
            .execute_change(
                &viewing_ctx(PortableAssetActionKind::Disable),
                &dummy_plan(),
                &change,
                Some(&item),
            )
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after = std::fs::read_to_string(&config).unwrap();
        assert!(
            after.contains("enabled = false") || after.contains("enabled=false"),
            "disable must insert enabled=false: {after}"
        );
        assert!(after.contains("keep-sibling"));
        assert!(after.contains("other"));
        assert!(after.contains("echo"));
    }

    /// Business Logic: Gemini native MCP Disable 写 settings.json mcpServers.*.enabled。
    /// Code Logic: tempfile settings.json，经 executor Disable 插入 enabled:false。
    #[test]
    fn gemini_native_mcp_disable_sets_enabled_false() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("settings.json");
        std::fs::write(
            &config,
            r#"{
  "theme": "dark",
  "mcpServers": {
    "g": { "command": "x" }
  }
}
"#,
        )
        .unwrap();
        let path = config.to_str().unwrap();
        let item = viewing_item(
            AgentTarget::Gemini,
            PortableAssetKind::Mcp,
            "g",
            path,
            PortableOriginKind::Native,
        );
        let change = viewing_change(
            AgentTarget::Gemini,
            PortableAssetKind::Mcp,
            "g",
            path,
            PortableAssetPlanOperation::Disable,
        );
        let out = OpenCodeTargetExecutor
            .execute_change(
                &viewing_ctx(PortableAssetActionKind::Disable),
                &dummy_plan(),
                &change,
                Some(&item),
            )
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["mcpServers"]["g"]["command"], "x");
        assert_eq!(after["mcpServers"]["g"]["enabled"], false);
    }

    /// Business Logic: Cursor native MCP 同样只翻 jsonc enabled，不删 leaf。
    /// Code Logic: tempfile mcp.json Disable cursor-api。
    #[test]
    fn cursor_native_mcp_disable_sets_enabled_false() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("mcp.json");
        std::fs::write(
            &config,
            r#"{
  // keep comment
  "mcpServers": {
    "cursor-api": { "command": "npx" }
  }
}
"#,
        )
        .unwrap();
        let path = config.to_str().unwrap();
        let item = viewing_item(
            AgentTarget::Cursor,
            PortableAssetKind::Mcp,
            "cursor-api",
            path,
            PortableOriginKind::Native,
        );
        let change = viewing_change(
            AgentTarget::Cursor,
            PortableAssetKind::Mcp,
            "cursor-api",
            path,
            PortableAssetPlanOperation::Disable,
        );
        let out = OpenCodeTargetExecutor
            .execute_change(
                &viewing_ctx(PortableAssetActionKind::Disable),
                &dummy_plan(),
                &change,
                Some(&item),
            )
            .unwrap();
        assert_eq!(out, TargetActionRawOutcome::Applied);
        let after = std::fs::read_to_string(&config).unwrap();
        assert!(after.contains("keep comment"), "jsonc comment must survive");
        assert!(
            after.contains("\"enabled\":false") || after.contains("\"enabled\": false"),
            "cursor disable must set enabled false: {after}"
        );
        assert!(after.contains("npx"));
    }

    /// Business Logic: 借用/兼容 MCP 执行器双门闩，仍 fail-closed。
    /// Code Logic: Grok + compatibility MCP Disable → WRITE_NOT_CERTIFIED。
    #[test]
    fn borrowed_mcp_execute_stays_blocked_on_grok_executor() {
        let item = viewing_item(
            AgentTarget::Grok,
            PortableAssetKind::Mcp,
            "claude-mcp",
            "/tmp/claude/.mcp.json",
            PortableOriginKind::Compatibility,
        );
        let change = viewing_change(
            AgentTarget::Grok,
            PortableAssetKind::Mcp,
            "claude-mcp",
            "/tmp/claude/.mcp.json",
            PortableAssetPlanOperation::Disable,
        );
        let out = OpenCodeTargetExecutor
            .execute_change(
                &viewing_ctx(PortableAssetActionKind::Disable),
                &dummy_plan(),
                &change,
                Some(&item),
            )
            .unwrap();
        assert!(
            matches!(
                out,
                TargetActionRawOutcome::Blocked { ref code, .. }
                    if code == "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED"
            ),
            "expected WRITE_NOT_CERTIFIED, got {out:?}"
        );
    }
}
