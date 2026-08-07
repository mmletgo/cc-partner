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

use super::paths::{probe_cli_version, read_utf8_file, resolve_executable, TargetPathResolver};
use super::portable::{
    claude_user_mcp_config_path, merge_discoveries, parse_json_or_jsonc,
    parse_mcp_servers_json_map, render_portable_payload, scan_agent_markdown_dir,
    scan_command_markdown_dir, scan_skill_dirs, AssetRenderContext, DiscoveredPortableAsset,
    PortableOriginKind, TargetAssetProjection,
};
use super::{
    build_probe, AssetAdapter, InstructionDocument, InstructionRenderContext, InstructionSource,
    InstructionSourceRole, LocalScopeMapping, RenderedInstruction, TargetEnvironment, TargetProbe,
};
use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, ScopeKind};
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
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
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

        // Plugin packages under config_root/plugins/<id>
        let plugins_root = base.join("plugins");
        if plugins_root.is_dir() {
            if let Ok(read) = std::fs::read_dir(&plugins_root) {
                for entry in read.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
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
