//! agent_hub/support/runtime_discovery — 编译期 runtime 发现表与只读扫描
//!
//! Business Logic（为什么需要这个模块）:
//!     Grok/Cursor/Gemini/Pi 等适配器必须从同一张表扫描 native 与兼容根，
//!     兼容/legacy 目录不得成为 native 写出目标。
//!
//! Code Logic（这个模块做什么）:
//!     `include_str!` 嵌入 `runtime-discovery.json`；fail-closed 解析；
//!     `roots_for` / `resolve_path` / `scan_table_roots` 解析 token、套用 gate，
//!     并调用既有 skill/command/agent/MCP/plugin 扫描 helper 后 stamp origin。

use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::agent_hub::plugins::decompose::discover_plugin_source_for_target;
use crate::agent_hub::targets::portable::{
    parse_codex_mcp_toml, parse_json_or_jsonc, parse_mcp_servers_json_map, scan_agent_markdown_dir,
    scan_command_markdown_dir, scan_plugin_components_readonly, scan_skill_dirs,
    scan_skill_dirs_manifest_only, stamp_table_origin, DiscoveredPortableAsset, PortableAssetOwner,
    PortableOriginKind,
};
use crate::agent_hub::targets::{
    LocalScopeMapping, TargetEnvironment, TargetHomes, TargetPathResolver,
};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 编译期嵌入的 runtime-discovery.json 原文。
pub const RUNTIME_DISCOVERY_JSON: &str = include_str!("runtime-discovery.json");

/// 发现根资产类别（含 plugin 注册表/市场清单）。
///
/// Business Logic: 表必须区分目录扫描与 Claude 风格 registry/marketplace JSON。
/// Code Logic: camelCase；未知 kind 解析失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiscoveryRootKind {
    Skill,
    Command,
    Agent,
    Plugin,
    Mcp,
    PluginRegistry,
    PluginMarketplace,
}

impl DiscoveryRootKind {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Command => "command",
            Self::Agent => "agent",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
            Self::PluginRegistry => "pluginRegistry",
            Self::PluginMarketplace => "pluginMarketplace",
        }
    }

    /// 是否匹配 inventory/adapter 的 `AssetKind` 过滤。
    fn matches_asset_kind(self, kind: AssetKind) -> bool {
        match (self, kind) {
            (Self::Skill, AssetKind::Skill)
            | (Self::Command, AssetKind::Command)
            | (Self::Agent, AssetKind::Agent)
            | (Self::Mcp, AssetKind::Mcp)
            | (Self::Plugin | Self::PluginRegistry | Self::PluginMarketplace, AssetKind::Plugin) => {
                true
            }
            _ => false,
        }
    }
}

/// 扫描门闩（环境变量或 Pi settings 列表）。
///
/// Business Logic: 兼容根可能被 CLI 环境开关关掉；Pi 仅在 settings 点名 Claude skills 时才扫。
/// Code Logic: internally tagged `type`；`envUnset` 要求全部 names 未设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DiscoveryGate {
    /// 所列环境变量均未设置时才扫描
    EnvUnset {
        /// 环境变量名
        names: Vec<String>,
    },
    /// 仅当 Pi settings 列出该 Claude skills 路径时扫描
    PiSettingsSkills,
}

/// 单条发现根。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryRoot {
    /// 资产/注册表类别
    pub kind: DiscoveryRootKind,
    /// user / project
    pub scope: ScopeKind,
    /// 含 token 的路径模板
    pub path_pattern: String,
    /// 发现分类
    pub origin_kind: PortableOriginKind,
    /// 文件所有者
    pub owned_by: PortableAssetOwner,
    /// 可选门闩
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gated_by: Option<DiscoveryGate>,
}

/// 单 target 的发现表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryAgent {
    /// Hub target
    pub target: AgentTarget,
    /// 该 target 的根列表
    pub roots: Vec<DiscoveryRoot>,
}

/// 编译期发现表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeDiscoveryTable {
    /// schema 版本
    pub version: u32,
    /// 各 target 根
    pub agents: Vec<DiscoveryAgent>,
}

/// 解析发现表（fail-closed）。
///
/// Business Logic（为什么需要这个函数）:
///     畸形表会导致适配器扫错根或把兼容目录当 native 写出，必须启动即失败。
///
/// Code Logic（这个函数做什么）:
///     serde 解析；version 必须为 1；拒绝空 agents。
pub fn load_runtime_discovery_from_str(raw: &str) -> Result<RuntimeDiscoveryTable, AppError> {
    let table: RuntimeDiscoveryTable = serde_json::from_str(raw)
        .map_err(|e| AppError::validation(format!("runtime_discovery_json_invalid:{e}")))?;
    if table.version != 1 {
        return Err(AppError::validation(format!(
            "runtime_discovery_unsupported_version:{}",
            table.version
        )));
    }
    if table.agents.is_empty() {
        return Err(AppError::validation("runtime_discovery_agents_empty"));
    }
    Ok(table)
}

/// 返回编译期内建表。
///
/// Business Logic: runtime 不得改写发现合同。
/// Code Logic: OnceLock 缓存解析结果；解析失败 panic（与 support-manifest 同级 fail-closed）。
pub fn builtin_runtime_discovery() -> &'static RuntimeDiscoveryTable {
    static TABLE: OnceLock<RuntimeDiscoveryTable> = OnceLock::new();
    TABLE.get_or_init(|| {
        load_runtime_discovery_from_str(RUNTIME_DISCOVERY_JSON)
            .expect("runtime-discovery.json must parse at startup")
    })
}

/// 取出某 target + scope 的发现根。
///
/// Business Logic（为什么需要这个函数）:
///     适配器只应扫描当前 scope 的根，避免把 user 兼容目录算进 project。
///
/// Code Logic（这个函数做什么）:
///     Directory 与 Project 共用 project 行；未知 target 返回空。
pub fn roots_for(target: AgentTarget, scope: ScopeKind) -> Vec<DiscoveryRoot> {
    let wanted = match scope {
        ScopeKind::User => ScopeKind::User,
        ScopeKind::Project | ScopeKind::Directory => ScopeKind::Project,
    };
    builtin_runtime_discovery()
        .agents
        .iter()
        .find(|agent| agent.target == target)
        .map(|agent| {
            agent
                .roots
                .iter()
                .filter(|root| root.scope == wanted)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// 把 pathPattern token 解析成绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     各 CLI 配置根随环境变量变化；表只写 token，解析必须与 probe 同一套 homes。
///
/// Code Logic（这个函数做什么）:
///     替换 `{home}` / `{project}` / `{configRoot}` 与各 `{*ConfigRoot}`；
///     project token 在 user scope 无法解析时返回 None。
pub fn resolve_path(
    pattern: &str,
    homes: &TargetHomes,
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
    target: AgentTarget,
) -> Option<PathBuf> {
    let config_root = config_root_for(Some(target), homes);
    let mut resolved = pattern.to_string();
    let replacements = [
        ("{home}", env.home.to_string_lossy().into_owned()),
        (
            "{claudeConfigRoot}",
            homes.claude.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{codexConfigRoot}",
            homes.codex.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{grokConfigRoot}",
            homes.grok.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{geminiConfigRoot}",
            homes.gemini.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{cursorConfigRoot}",
            homes.cursor.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{piConfigRoot}",
            homes.pi.config_root.to_string_lossy().into_owned(),
        ),
        (
            "{opencodeConfigRoot}",
            homes.opencode.config_root.to_string_lossy().into_owned(),
        ),
        ("{configRoot}", config_root.to_string_lossy().into_owned()),
    ];
    for (token, value) in replacements {
        resolved = resolved.replace(token, &value);
    }
    if resolved.contains("{project}") {
        if matches!(scope.scope_kind, ScopeKind::User) {
            return None;
        }
        let project = scope
            .project_root
            .as_ref()
            .unwrap_or(&scope.absolute_path)
            .to_string_lossy()
            .into_owned();
        resolved = resolved.replace("{project}", &project);
    }
    if resolved.contains('{') {
        return None;
    }
    Some(PathBuf::from(resolved))
}

fn config_root_for(hinted: Option<AgentTarget>, homes: &TargetHomes) -> PathBuf {
    match hinted.unwrap_or(AgentTarget::Claude) {
        AgentTarget::Claude => homes.claude.config_root.clone(),
        AgentTarget::Codex => homes.codex.config_root.clone(),
        AgentTarget::OpenCode => homes.opencode.config_root.clone(),
        AgentTarget::Grok => homes.grok.config_root.clone(),
        AgentTarget::Gemini => homes.gemini.config_root.clone(),
        AgentTarget::Cursor => homes.cursor.config_root.clone(),
        AgentTarget::Pi => homes.pi.config_root.clone(),
    }
}

/// 判断 gate 是否允许扫描该根。
///
/// Business Logic（为什么需要这个函数）:
///     环境开关关闭时不得把兼容目录算进库存；Pi 未点名 Claude skills 时必须跳过。
///
/// Code Logic（这个函数做什么）:
///     `envUnset`：全部 names 在注入 env 中为空；`piSettingsSkills`：settings 文本包含路径。
pub fn gate_allows(
    gate: Option<&DiscoveryGate>,
    env: &TargetEnvironment,
    homes: &TargetHomes,
    resolved: &Path,
) -> bool {
    match gate {
        None => true,
        Some(DiscoveryGate::EnvUnset { names }) => names.iter().all(|name| env.var(name).is_none()),
        Some(DiscoveryGate::PiSettingsSkills) => pi_settings_lists_path(homes, resolved),
    }
}

/// Pi settings 是否点名该 skills 路径（stub：未列出则跳过）。
fn pi_settings_lists_path(homes: &TargetHomes, resolved: &Path) -> bool {
    let settings = homes.pi.config_root.join("settings.json");
    let Ok(text) = fs::read_to_string(settings) else {
        return false;
    };
    let needle = resolved.to_string_lossy();
    text.contains(needle.as_ref()) || text.contains(".claude/skills")
}

/// 按发现表扫描匹配根并 stamp origin。
///
/// Business Logic（为什么需要这个函数）:
///     后续适配器应把 `scan_portable_assets` 的兼容/native 根交给本函数，
///     保证 ownedBy / originKind / nativeOutputCandidate 与表一致。
///
/// Code Logic（这个函数做什么）:
///     解析 matching roots → 套用 gate → 调既有扫描 helper → stamp 表字段。
///     `kind_filter` 为 None 扫全部；`manifest_only` 仅影响 skill 树 hash。
pub fn scan_table_roots(
    target: AgentTarget,
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
    kind_filter: Option<AssetKind>,
    manifest_only: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let mut out = Vec::new();
    for root in roots_for(target, scope.scope_kind) {
        if let Some(kind) = kind_filter {
            if !root.kind.matches_asset_kind(kind) {
                continue;
            }
        }
        let Some(path) = resolve_path(&root.path_pattern, &homes, scope, env, target) else {
            continue;
        };
        if !gate_allows(root.gated_by.as_ref(), env, &homes, &path) {
            continue;
        }
        let mut found = scan_one_root(target, scope.scope_kind, &root, &path, manifest_only)?;
        stamp_table_origin(&mut found, root.owned_by, root.origin_kind);
        out.extend(found);
    }
    Ok(out)
}

fn scan_one_root(
    target: AgentTarget,
    scope_kind: ScopeKind,
    root: &DiscoveryRoot,
    path: &Path,
    manifest_only: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    match root.kind {
        DiscoveryRootKind::Skill => {
            if manifest_only {
                scan_skill_dirs_manifest_only(target, scope_kind, path, root.origin_kind)
            } else {
                scan_skill_dirs(target, scope_kind, path, root.origin_kind)
            }
        }
        DiscoveryRootKind::Command => {
            scan_command_markdown_dir(target, scope_kind, path, root.origin_kind)
        }
        DiscoveryRootKind::Agent => {
            scan_agent_markdown_dir(target, scope_kind, path, root.origin_kind)
        }
        DiscoveryRootKind::Mcp => scan_mcp_path(target, scope_kind, path, root.origin_kind),
        DiscoveryRootKind::Plugin => scan_plugin_dir(target, scope_kind, path),
        DiscoveryRootKind::PluginRegistry => scan_plugin_registry(target, scope_kind, path),
        DiscoveryRootKind::PluginMarketplace => scan_plugin_marketplace(target, scope_kind, path),
    }
}

fn scan_mcp_path(
    target: AgentTarget,
    scope_kind: ScopeKind,
    path: &Path,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !path.is_file() {
        return Ok(vec![]);
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(vec![]),
    };
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext == "toml" {
        return parse_codex_mcp_toml(target, scope_kind, &text, path);
    }
    let Ok(value) = parse_json_or_jsonc(&text) else {
        return Ok(vec![]);
    };
    let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()).cloned() else {
        return Ok(vec![]);
    };
    Ok(parse_mcp_servers_json_map(
        target,
        scope_kind,
        &map,
        path,
        origin_kind,
        true,
    ))
}

fn scan_plugin_dir(
    target: AgentTarget,
    scope_kind: ScopeKind,
    path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !path.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut entries: Vec<_> = match fs::read_dir(path) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return Ok(vec![]),
    };
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let child = entry.path();
        if !child.is_dir() {
            continue;
        }
        let plugin_id = discover_plugin_source_for_target(target, &child, "scan", scope_kind)
            .map(|s| s.plugin_id)
            .unwrap_or_else(|_| {
                child
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("plugin")
                    .to_string()
            });
        out.extend(scan_plugin_components_readonly(
            target, scope_kind, &child, &plugin_id,
        )?);
    }
    Ok(out)
}

/// 解析 Claude 风格 `installed_plugins.json` 的 `plugins.*.[].installPath`。
///
/// Business Logic（为什么需要这个函数）:
///     Grok 等兼容扫描必须复用 Claude registry，而不是把 cache 一级目录当 package。
///
/// Code Logic（这个函数做什么）:
///     读 JSON → 收集存在的 installPath 目录。
pub fn parse_installed_plugin_paths(registry_path: &Path) -> Vec<PathBuf> {
    let Ok(raw) = fs::read_to_string(registry_path) else {
        return vec![];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return vec![];
    };
    let Some(plugins) = value.get("plugins").and_then(|v| v.as_object()) else {
        return vec![];
    };
    let mut paths = Vec::new();
    for installs in plugins.values() {
        let Some(installs) = installs.as_array() else {
            continue;
        };
        for install in installs {
            let Some(path) = install.get("installPath").and_then(|v| v.as_str()) else {
                continue;
            };
            let path = PathBuf::from(path);
            if path.is_dir() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn scan_plugin_registry(
    target: AgentTarget,
    scope_kind: ScopeKind,
    registry_path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut out = Vec::new();
    for path in parse_installed_plugin_paths(registry_path) {
        let plugin_id = discover_plugin_source_for_target(target, &path, "scan", scope_kind)
            .map(|s| s.plugin_id)
            .unwrap_or_else(|_| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("plugin")
                    .to_string()
            });
        out.extend(scan_plugin_components_readonly(
            target, scope_kind, &path, &plugin_id,
        )?);
    }
    Ok(out)
}

/// 解析 `known_marketplaces.json` 的 `installLocation` 目录。
///
/// Business Logic（为什么需要这个函数）:
///     Grok 等兼容扫描必须复用 Claude marketplace 安装位置，而不是把清单文件当 package。
///
/// Code Logic（这个函数做什么）:
///     递归收集存在的 `installLocation` 目录。
pub fn parse_marketplace_install_locations(path: &Path) -> Vec<PathBuf> {
    let Ok(raw) = fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return vec![];
    };
    let mut locations = Vec::new();
    collect_install_locations(&value, &mut locations);
    locations.sort();
    locations.dedup();
    locations
}

fn collect_install_locations(value: &serde_json::Value, out: &mut Vec<PathBuf>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(loc) = map.get("installLocation").and_then(|v| v.as_str()) {
                let path = PathBuf::from(loc);
                if path.is_dir() {
                    out.push(path);
                }
            }
            for child in map.values() {
                collect_install_locations(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_install_locations(child, out);
            }
        }
        _ => {}
    }
}

fn scan_plugin_marketplace(
    target: AgentTarget,
    scope_kind: ScopeKind,
    marketplace_path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut out = Vec::new();
    for location in parse_marketplace_install_locations(marketplace_path) {
        out.extend(scan_plugin_dir(target, scope_kind, &location)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn builtin_table_parses_and_lists_required_grok_and_opencode_roots() {
        let table = load_runtime_discovery_from_str(RUNTIME_DISCOVERY_JSON).unwrap();
        assert_eq!(table.version, 1);
        let grok = table
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::Grok)
            .expect("grok agent");
        assert!(
            grok.roots.iter().any(|r| {
                r.kind == DiscoveryRootKind::PluginRegistry
                    && r.path_pattern.contains("installed_plugins.json")
                    && r.origin_kind == PortableOriginKind::Compatibility
                    && r.owned_by == PortableAssetOwner::Claude
            }),
            "Grok must list Claude pluginRegistry"
        );
        assert!(
            grok.roots.iter().any(|r| {
                r.kind == DiscoveryRootKind::Plugin
                    && r.path_pattern.contains("installed-plugins")
                    && r.origin_kind == PortableOriginKind::Native
                    && r.owned_by == PortableAssetOwner::Grok
            }),
            "Grok must list native installed-plugins"
        );
        let opencode = table
            .agents
            .iter()
            .find(|a| a.target == AgentTarget::OpenCode)
            .expect("opencode agent");
        let compat: Vec<_> = opencode
            .roots
            .iter()
            .filter(|r| r.origin_kind == PortableOriginKind::Compatibility)
            .collect();
        assert!(!compat.is_empty());
        assert!(compat
            .iter()
            .all(|r| r.origin_kind == PortableOriginKind::Compatibility));
        assert!(compat
            .iter()
            .any(|r| r.path_pattern.contains(".claude/skills")));
        assert!(compat
            .iter()
            .any(|r| r.path_pattern.contains(".agents/skills")));
    }

    #[test]
    fn resolve_path_expands_config_and_project_tokens() {
        let home = PathBuf::from("/tmp/runtime-home");
        let env = TargetEnvironment {
            home: home.clone(),
            vars: BTreeMap::from([("GROK_HOME".into(), "/tmp/runtime-home/.grok".into())]),
            path_entries: vec![],
        };
        let homes = TargetPathResolver::resolve_all(&env);
        let user_scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.clone(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let path = resolve_path(
            "{grokConfigRoot}/skills",
            &homes,
            &user_scope,
            &env,
            AgentTarget::Grok,
        )
        .unwrap();
        assert_eq!(path, PathBuf::from("/tmp/runtime-home/.grok/skills"));
        assert!(resolve_path(
            "{project}/.grok/skills",
            &homes,
            &user_scope,
            &env,
            AgentTarget::Grok,
        )
        .is_none());

        let project_scope = LocalScopeMapping {
            scope_kind: ScopeKind::Project,
            absolute_path: PathBuf::from("/tmp/proj"),
            project_root: Some(PathBuf::from("/tmp/proj")),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec![],
        };
        let project = resolve_path(
            "{project}/.claude/skills",
            &homes,
            &project_scope,
            &env,
            AgentTarget::Grok,
        )
        .unwrap();
        assert_eq!(project, PathBuf::from("/tmp/proj/.claude/skills"));
    }

    #[test]
    fn env_unset_gate_skips_when_any_name_is_set() {
        let env = TargetEnvironment {
            home: PathBuf::from("/tmp"),
            vars: BTreeMap::from([("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS".into(), "1".into())]),
            path_entries: vec![],
        };
        let homes = TargetPathResolver::resolve_all(&env);
        let gate = DiscoveryGate::EnvUnset {
            names: vec!["OPENCODE_DISABLE_CLAUDE_CODE_SKILLS".into()],
        };
        assert!(!gate_allows(
            Some(&gate),
            &env,
            &homes,
            Path::new("/tmp/.claude/skills")
        ));
        let empty = TargetEnvironment {
            home: PathBuf::from("/tmp"),
            vars: BTreeMap::new(),
            path_entries: vec![],
        };
        assert!(gate_allows(
            Some(&gate),
            &empty,
            &homes,
            Path::new("/tmp/.claude/skills")
        ));
    }

    #[test]
    fn scan_table_roots_stamps_compatibility_and_blocks_native_output() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let skill = home.join(".claude/skills/borrowed");
        fs::create_dir_all(&skill).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: borrowed\ndescription: compat\n---\n# Borrowed\n",
        )
        .unwrap();
        let env = TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries: vec![],
        };
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let found = scan_table_roots(
            AgentTarget::OpenCode,
            &scope,
            &env,
            Some(AssetKind::Skill),
            true,
        )
        .unwrap();
        let compat = found
            .iter()
            .find(|d| d.origin.native_id == "borrowed")
            .expect("compat skill");
        assert_eq!(compat.origin.origin_kind, PortableOriginKind::Compatibility);
        assert!(!compat.origin.native_output_candidate);
        assert_eq!(compat.origin.owned_by, PortableAssetOwner::Claude);
        assert_eq!(compat.origin.target, AgentTarget::OpenCode);
    }
}
