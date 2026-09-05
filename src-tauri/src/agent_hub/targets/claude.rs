//! agent_hub/targets/claude — Claude Code instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude 用户级与项目级均使用 `CLAUDE.md`，但路径空间分离：
//!     用户文件在配置根，项目文件在目录本身；禁止混扫。
//!     Gate B 同时扫描 skills/commands/agents Markdown 与 user MCP JSON。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `claude` 可执行文件与 CLAUDE_CONFIG_DIR；
//!     scan 指令 + portable 资产；render 指令与 portable 投影。

use super::paths::{
    probe_cli_version_in_env, read_utf8_file, resolve_executable, TargetPathResolver,
};
use super::portable::{
    claude_user_mcp_config_path, merge_discoveries, parse_json_or_jsonc,
    parse_mcp_servers_json_map, render_portable_payload, scan_agent_markdown_dir,
    scan_command_markdown_dir, scan_skill_dirs, scan_skill_dirs_manifest_only, AssetRenderContext,
    DiscoveredPortableAsset, PortableOriginKind, TargetAssetProjection,
};
use super::{
    build_probe, AssetAdapter, InstructionDocument, InstructionRenderContext, InstructionSource,
    InstructionSourceRole, LocalScopeMapping, RenderedInstruction, TargetEnvironment, TargetProbe,
};
use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::error::AppError;
use std::path::PathBuf;

/// Claude 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service 层通过统一 `AssetAdapter` 调用 Claude 路径与渲染语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeInstructionAdapter;

impl AssetAdapter for ClaudeInstructionAdapter {
    /// 返回 Claude 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Claude`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Claude
    }

    /// 探测 Claude 可执行文件、版本与配置根。
    ///
    /// Business Logic: 版本未知只能 scan-only；配置根变化使旧 probe 失效。
    /// Code Logic: 解析 homes.claude.config_root；查找 `claude`；读 `--version`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("claude", env);
        let version = executable
            .as_ref()
            .and_then(|p| probe_cli_version_in_env(p, env));
        Ok(build_probe(
            AgentTarget::Claude,
            executable,
            version,
            homes.claude.config_root,
        ))
    }

    /// 扫描 Claude 指令源。
    ///
    /// Business Logic: 用户级只看配置根 CLAUDE.md；项目/目录只看该目录 CLAUDE.md，路径隔离。
    /// Code Logic: 不写盘；缺失文件返回空列表。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError> {
        let path = match scope.scope_kind {
            ScopeKind::User => {
                let homes = TargetPathResolver::resolve_all(env);
                homes.claude.config_root.join("CLAUDE.md")
            }
            ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join("CLAUDE.md"),
        };
        let content = read_utf8_file(&path)?;
        let Some(text) = content else {
            return Ok(vec![]);
        };
        let non_empty = !text.trim().is_empty();
        Ok(vec![InstructionSource {
            target: AgentTarget::Claude,
            path,
            scope_kind: scope.scope_kind,
            role: InstructionSourceRole::NativePrimary,
            active: true,
            native_active: true,
            non_empty,
            relative_path: scope.relative_root.clone(),
            diagnostics: vec![],
        }])
    }

    /// 渲染 Claude `CLAUDE.md`。
    ///
    /// Business Logic: 经 Instruction Compiler 输出用户正文；无 managed prelude。
    /// Code Logic: `compile_render` → `RenderedInstruction::from_compiled`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Claude,
            context,
        );
        Ok(RenderedInstruction::from_compiled(compiled))
    }

    /// 扫描 Claude native Skill/Command/Agent 与 user MCP。
    ///
    /// Business Logic: 用户 scope 用 config_root；项目/目录用 absolute_path 下的 .claude 子树。
    /// Code Logic: skills/commands/agents 目录 + `.claude.json` mcpServers。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        use super::portable::{
            scan_disabled_command_markdown_dir, scan_disabled_skill_dirs,
            scan_plugin_components_readonly,
        };
        use crate::agent_hub::plugins::decompose::discover_plugin_source_for_target;

        let homes = TargetPathResolver::resolve_all(env);
        let base = match scope.scope_kind {
            ScopeKind::User => homes.claude.config_root.clone(),
            ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".claude"),
        };
        let mut parts: Vec<Vec<DiscoveredPortableAsset>> = Vec::new();
        parts.push(scan_skill_dirs(
            AgentTarget::Claude,
            scope.scope_kind,
            &base.join("skills"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_disabled_skill_dirs(
            AgentTarget::Claude,
            scope.scope_kind,
            &base.join("disabled").join("skills"),
            PortableOriginKind::Native,
        )?);
        // Hub portable executor disables user skills/commands under
        // <data_dir>/claude-assets/disabled/{skills,commands} — inventory must
        // observe those paths or rescan after Disable reports "missing".
        if scope.scope_kind == ScopeKind::User {
            if let Ok(data) = crate::config::data_dir() {
                let hub_disabled = data.join("claude-assets").join("disabled");
                parts.push(scan_disabled_skill_dirs(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &hub_disabled.join("skills"),
                    PortableOriginKind::Native,
                )?);
                parts.push(scan_disabled_command_markdown_dir(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &hub_disabled.join("commands"),
                    PortableOriginKind::Native,
                )?);
                parts.push(scan_hub_disabled_mcp_snapshots(
                    scope.scope_kind,
                    &hub_disabled.join("mcp"),
                )?);
            }
        }
        parts.push(scan_command_markdown_dir(
            AgentTarget::Claude,
            scope.scope_kind,
            &base.join("commands"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_disabled_command_markdown_dir(
            AgentTarget::Claude,
            scope.scope_kind,
            &base.join("disabled").join("commands"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_agent_markdown_dir(
            AgentTarget::Claude,
            scope.scope_kind,
            &base.join("agents"),
            PortableOriginKind::Native,
        )?);
        let mut mcp = Vec::new();
        if scope.scope_kind == ScopeKind::User {
            mcp = scan_claude_user_mcp(scope.scope_kind, env)?;
        } else {
            // 项目级：`.mcp.json` 或 `.claude/settings` 中的 mcp 若存在则扫描
            for candidate in [
                scope.absolute_path.join(".mcp.json"),
                scope
                    .absolute_path
                    .join(".claude")
                    .join("settings.local.json"),
            ] {
                if candidate.is_file() {
                    mcp.extend(scan_mcp_json_file(
                        AgentTarget::Claude,
                        scope.scope_kind,
                        &candidate,
                        PortableOriginKind::Native,
                    )?);
                }
            }
        }
        parts.push(mcp);

        // Plugin components：user scope 复用 inventory 权威 package 根；project 仅直装 manifest
        if scope.scope_kind == ScopeKind::User {
            for path in
                crate::agent_hub::portable_inventory::scanner::user_plugin_package_root_paths(
                    AgentTarget::Claude,
                    &base,
                )
            {
                let plugin_id = discover_plugin_source_for_target(
                    AgentTarget::Claude,
                    &path,
                    "scan",
                    scope.scope_kind,
                )
                .map(|s| s.plugin_id)
                .ok()
                .or_else(|| {
                    crate::agent_hub::portable_inventory::plugin_paths::plugin_id_from_path(Some(
                        &path.display().to_string(),
                    ))
                })
                .unwrap_or_else(|| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("plugin")
                        .to_string()
                });
                if crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_name(
                    &plugin_id,
                ) {
                    continue;
                }
                parts.push(scan_plugin_components_readonly(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &path,
                    &plugin_id,
                )?);
            }
        } else {
            let plugins_root = base.join("plugins");
            if plugins_root.is_dir() {
                for entry in std::fs::read_dir(&plugins_root)
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let path = entry.path();
                    if !path.is_dir()
                        || !path.join(".claude-plugin/plugin.json").is_file()
                        || crate::agent_hub::portable_inventory::plugin_paths::is_plugin_infrastructure_path(
                            &path,
                        )
                    {
                        continue;
                    }
                    let plugin_id = discover_plugin_source_for_target(
                        AgentTarget::Claude,
                        &path,
                        "scan",
                        scope.scope_kind,
                    )
                    .map(|s| s.plugin_id)
                    .unwrap_or_else(|_| {
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("plugin")
                            .to_string()
                    });
                    parts.push(scan_plugin_components_readonly(
                        AgentTarget::Claude,
                        scope.scope_kind,
                        &path,
                        &plugin_id,
                    )?);
                }
            }
        }
        Ok(merge_discoveries(parts))
    }

    /// Inventory 精确 kind 扫描；Plugin component 由 inventory 的权威安装根扫描器补充。
    fn scan_portable_assets_filtered(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
        kind: Option<AssetKind>,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let Some(kind) = kind else {
            return self.scan_portable_assets(scope, env);
        };
        use super::portable::{
            scan_disabled_command_markdown_dir, scan_disabled_skill_dirs_manifest_only,
        };
        let homes = TargetPathResolver::resolve_all(env);
        let base = match scope.scope_kind {
            ScopeKind::User => homes.claude.config_root.clone(),
            ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".claude"),
        };
        let mut parts = Vec::new();
        match kind {
            AssetKind::Skill => {
                parts.push(scan_skill_dirs_manifest_only(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &base.join("skills"),
                    PortableOriginKind::Native,
                )?);
                parts.push(scan_disabled_skill_dirs_manifest_only(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &base.join("disabled/skills"),
                    PortableOriginKind::Native,
                )?);
                if scope.scope_kind == ScopeKind::User {
                    if let Ok(data) = crate::config::data_dir() {
                        parts.push(scan_disabled_skill_dirs_manifest_only(
                            AgentTarget::Claude,
                            scope.scope_kind,
                            &data.join("claude-assets/disabled/skills"),
                            PortableOriginKind::Native,
                        )?);
                    }
                }
            }
            AssetKind::Command => {
                parts.push(scan_command_markdown_dir(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &base.join("commands"),
                    PortableOriginKind::Native,
                )?);
                parts.push(scan_disabled_command_markdown_dir(
                    AgentTarget::Claude,
                    scope.scope_kind,
                    &base.join("disabled/commands"),
                    PortableOriginKind::Native,
                )?);
                if scope.scope_kind == ScopeKind::User {
                    if let Ok(data) = crate::config::data_dir() {
                        parts.push(scan_disabled_command_markdown_dir(
                            AgentTarget::Claude,
                            scope.scope_kind,
                            &data.join("claude-assets/disabled/commands"),
                            PortableOriginKind::Native,
                        )?);
                    }
                }
            }
            AssetKind::Agent => parts.push(scan_agent_markdown_dir(
                AgentTarget::Claude,
                scope.scope_kind,
                &base.join("agents"),
                PortableOriginKind::Native,
            )?),
            AssetKind::Mcp => {
                if scope.scope_kind == ScopeKind::User {
                    parts.push(scan_claude_user_mcp(scope.scope_kind, env)?);
                    if let Ok(data) = crate::config::data_dir() {
                        parts.push(scan_hub_disabled_mcp_snapshots(
                            scope.scope_kind,
                            &data.join("claude-assets/disabled/mcp"),
                        )?);
                    }
                } else {
                    for candidate in [
                        scope.absolute_path.join(".mcp.json"),
                        scope.absolute_path.join(".claude/settings.local.json"),
                    ] {
                        if candidate.is_file() {
                            parts.push(scan_mcp_json_file(
                                AgentTarget::Claude,
                                scope.scope_kind,
                                &candidate,
                                PortableOriginKind::Native,
                            )?);
                        }
                    }
                }
            }
            AssetKind::Instruction | AssetKind::Plugin | AssetKind::Hook => {}
        }
        Ok(merge_discoveries(parts))
    }

    /// 渲染 Claude portable 投影。
    ///
    /// Business Logic: 投影到 skills/commands/agents/mcp 相对路径计划，不写盘；
    /// Gate D plugin package render 复用同一 `render_portable_payload` 入口。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Claude, asset)
    }
}

/// 扫描 Claude 用户级 MCP（`.claude.json` 或 home `.claude.json`）。
///
/// Business Logic: 与 legacy claude_code_assets 路径规则对齐，便于 N/N+1 façade。
/// Code Logic: 读 JSON → mcpServers map。
fn scan_claude_user_mcp(
    scope_kind: ScopeKind,
    env: &TargetEnvironment,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let path = claude_user_mcp_config_path(env);
    scan_mcp_json_file(
        AgentTarget::Claude,
        scope_kind,
        &path,
        PortableOriginKind::Native,
    )
}

/// 扫描 hub portable 执行器写入的 disabled MCP 快照（单 server JSON 文件）。
///
/// Business Logic: Disable 把 leaf 原文落到 data_dir/claude-assets/disabled/mcp；
///     rescan 必须标 actualEnabled=false，否则 Enable 动作不可达。
/// Code Logic: 每个 `*.json` 作为独立 disabled MCP discovery。
fn scan_hub_disabled_mcp_snapshots(
    scope_kind: ScopeKind,
    disabled_mcp_dir: &std::path::Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    use super::portable::{
        DiscoveredPortableAsset, PortableAssetOrigin, PortableDiscoveryStatus, PortableOriginKind,
    };
    use crate::agent_hub::assets::{McpTransport, PortableAssetPayload, PortableMcpServer};
    use crate::agent_hub::models::AssetKind;
    use crate::agent_hub::object_store::sha256_hex;
    use std::collections::BTreeMap;
    use std::fs;

    if !disabled_mcp_dir.is_dir() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let rd = match fs::read_dir(disabled_mcp_dir) {
        Ok(r) => r,
        Err(_) => return Ok(vec![]),
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let key = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("mcp")
            .to_string();
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let mut env_map = BTreeMap::new();
        if let Some(env_obj) = obj.get("env").and_then(|v| v.as_object()) {
            for (k, v) in env_obj {
                if let Some(s) = v.as_str() {
                    env_map.insert(k.clone(), s.to_string());
                }
            }
        }
        let mut headers = BTreeMap::new();
        if let Some(h) = obj.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in h {
                if let Some(s) = v.as_str() {
                    headers.insert(k.clone(), s.to_string());
                }
            }
        }
        let transport = if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
            McpTransport::Http {
                url: url.to_string(),
                headers,
            }
        } else {
            let command = obj
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("npx")
                .to_string();
            let args = obj
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_string);
            McpTransport::Stdio { command, args, cwd }
        };
        let enabled = obj
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        out.push(DiscoveredPortableAsset {
            kind: AssetKind::Mcp,
            semantic_name: key.clone(),
            scope_kind,
            payload: PortableAssetPayload::Mcp(PortableMcpServer {
                key: key.clone(),
                transport,
                env: env_map,
                enabled,
                tool_allow: vec![],
                tool_deny: vec![],
                target_extensions: BTreeMap::new(),
            }),
            origin: PortableAssetOrigin {
                target: AgentTarget::Claude,
                path: path.clone(),
                origin_kind: PortableOriginKind::Native,
                native_id: key,
                content_hash: sha256_hex(&raw),
                tree_hash: None,
                status: PortableDiscoveryStatus::Disabled,
                native_output_candidate: true,
                owned_by: crate::agent_hub::targets::portable::PortableAssetOwner::from_target(
                    AgentTarget::Claude,
                ),
                parent_plugin_id: None,
            },
            diagnostics: vec![],
        });
    }
    Ok(out)
}

/// 从 JSON 文件读 mcpServers。
fn scan_mcp_json_file(
    target: AgentTarget,
    scope_kind: ScopeKind,
    path: &PathBuf,
    origin_kind: PortableOriginKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !path.is_file() {
        return Ok(vec![]);
    }
    let text = std::fs::read_to_string(path)?;
    let value = parse_json_or_jsonc(&text)?;
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

/// Claude managed package 激活选择器（`plugin@cc-partner`）。
///
/// Business Logic: 安装/列表检查必须使用稳定 selector，禁止猜测 marketplace 名。
/// Code Logic: 委托 packages::PLUGIN_SELECTOR。
pub fn claude_managed_plugin_selector() -> &'static str {
    crate::agent_hub::packages::PLUGIN_SELECTOR
}

/// 从 Claude Plugin 根目录构造 `DiscoveredPluginSource`（不扫描 child）。
///
/// Business Logic（为什么需要这个函数）:
///     Gate D 分解入口需要 target 侧稳定构造发现记录；实际 component 扫描在 `plugins::decompose`。
///
/// Code Logic（这个函数做什么）:
///     读取 `.claude-plugin/plugin.json` 的 name/version/description（若存在），否则用目录名。
pub fn discover_claude_plugin_source(
    root: &std::path::Path,
    scope_id: impl Into<String>,
    scope_kind: ScopeKind,
) -> Result<crate::agent_hub::plugins::DiscoveredPluginSource, AppError> {
    crate::agent_hub::plugins::decompose::discover_plugin_source_for_target(
        crate::agent_hub::models::AgentTarget::Claude,
        root,
        scope_id,
        scope_kind,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::portable::PortableDiscoveryStatus;
    use std::fs;

    #[test]
    fn hub_disabled_skill_is_discovered_under_data_dir() {
        // install_data_dir_env 的 guard 已持有统一的 data_dir 测试锁并负责恢复环境，
        // 不再叠加外层 DATA_DIR_ENV_LOCK（同一把非重入锁会自死锁）。
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        let claude = home.join(".claude");
        fs::create_dir_all(claude.join("skills")).unwrap();
        let disabled = data.join("claude-assets/disabled/skills/was-active");
        fs::create_dir_all(&disabled).unwrap();
        fs::write(
            disabled.join("SKILL.md"),
            "---\nname: was-active\ndescription: d\n---\nbody\n",
        )
        .unwrap();
        // Isolate data_dir via env override used by config::data_dir()
        let _data_dir_guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let mut vars = std::collections::BTreeMap::new();
        // Point Claude config at isolated home so user-level scan does not touch real ~/.claude
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            claude.to_string_lossy().into_owned(),
        );
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![],
        };
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.clone(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let found = ClaudeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .expect("scan");
        let skill = found.iter().find(|d| {
            matches!(
                d.payload,
                crate::agent_hub::assets::PortableAssetPayload::Skill(_)
            ) && d.origin.native_id == "was-active"
        });
        let skill = skill.expect("hub disabled skill must be inventoried");
        assert_eq!(skill.origin.status, PortableDiscoveryStatus::Disabled);
        assert!(skill
            .origin
            .path
            .to_string_lossy()
            .contains("claude-assets/disabled/skills/was-active"));
    }

    #[test]
    fn filtered_skill_scan_does_not_parse_unrequested_mcp_config() {
        // 同上：install_data_dir_env guard 自身持锁，禁止叠加外层锁自死锁。
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = tmp.path().join("data");
        let claude = home.join(".claude");
        fs::create_dir_all(claude.join("skills/review")).unwrap();
        fs::write(
            claude.join("skills/review/SKILL.md"),
            "---\nname: review\n---\nbody\n",
        )
        .unwrap();
        fs::write(claude.join(".claude.json"), "{ invalid json").unwrap();
        let _data_dir_guard =
            crate::config::install_data_dir_env(Some(data.to_str().expect("utf8 data dir")));
        let env = TargetEnvironment {
            home: home.clone(),
            vars: std::collections::BTreeMap::from([(
                "CLAUDE_CONFIG_DIR".into(),
                claude.to_string_lossy().into_owned(),
            )]),
            path_entries: vec![],
        };
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home,
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let result = ClaudeInstructionAdapter.scan_portable_assets_filtered(
            &scope,
            &env,
            Some(AssetKind::Skill),
        );
        let found = result.expect("skill-only scan must ignore invalid MCP config");
        assert!(found
            .iter()
            .any(|asset| asset.kind == AssetKind::Skill && asset.semantic_name == "review"));
        assert!(found.iter().all(|asset| asset.kind == AssetKind::Skill));
    }
}
