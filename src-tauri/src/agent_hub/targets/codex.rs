//! agent_hub/targets/codex — Codex CLI instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex 与 OpenCode 都可能使用 AGENTS.md；Hub 用 `AGENTS.override.md` 作为受管投影，
//!     使 OpenCode 专属 AGENTS.md 保持 target-specific，同时扫描被遮蔽非空源。
//!     Gate B：解析 config.toml 中 MCP/agents；`.agents/skills` 仅作 legacy standalone。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `codex`；扫描 override / AGENTS.md / fallback；
//!     扫描 portable MCP/agents + legacy skills；render 输出。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::portable::{
    merge_discoveries, parse_codex_agents_toml, parse_codex_mcp_toml, render_portable_payload,
    scan_skill_dirs, scan_skill_dirs_manifest_only, AssetRenderContext, DiscoveredPortableAsset,
    PortableOriginKind, TargetAssetProjection,
};
use super::{
    build_probe, relative_path_string, AssetAdapter, InstructionDocument, InstructionRenderContext,
    InstructionSource, InstructionSourceRole, LocalScopeMapping, RenderedInstruction,
    TargetEnvironment, TargetProbe,
};
use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::error::AppError;
use std::path::{Path, PathBuf};

/// Codex 默认额外扫描的 fallback 文件名（当 scope 未注入时）。
const DEFAULT_CODEX_FALLBACKS: &[&str] = &["AGENTS.fallback.md", "agents.md"];

/// Codex 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     必须同时报告生效源与被遮蔽非空源，避免 override 投影静默丢弃用户文件。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexInstructionAdapter;

impl AssetAdapter for CodexInstructionAdapter {
    /// 返回 Codex 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Codex`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Codex
    }

    /// 探测 Codex 可执行文件、版本与配置根。
    ///
    /// Business Logic: 版本未知只能 scan-only。
    /// Code Logic: CODEX_HOME → `~/.codex`；查找 `codex`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("codex", env);
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
        Ok(build_probe(
            AgentTarget::Codex,
            executable,
            version,
            homes.codex.config_root,
        ))
    }

    /// 扫描 Codex 指令源（override / AGENTS.md / fallback）。
    ///
    /// Business Logic: 当前生效文件按优先级导入；被遮蔽但非空的文件作为 inactive 诊断保留。
    /// Code Logic: user 用 config_root；project/directory 用 absolute_path。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError> {
        let base = match scope.scope_kind {
            ScopeKind::User => TargetPathResolver::resolve_all(env).codex.config_root,
            ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.clone(),
        };
        scan_codex_layer(&base, scope)
    }

    /// 渲染 Codex 受管投影 `AGENTS.override.md`。
    ///
    /// Business Logic: Hub 写入 override，避免污染 OpenCode 的 AGENTS.md；经 compiler 输出。
    /// Code Logic: `compile_render` → `RenderedInstruction::from_compiled`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Codex,
            context,
        );
        Ok(RenderedInstruction::from_compiled(compiled))
    }

    /// 扫描 Codex portable 资产。
    ///
    /// Business Logic: MCP/agents 来自 config.toml；`.agents/skills` 仅 legacy standalone。
    /// Code Logic: 读 CODEX_HOME/config.toml；skill_compat_root 用 LegacyStandalone。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let mut parts: Vec<Vec<DiscoveredPortableAsset>> = Vec::new();

        if scope.scope_kind == ScopeKind::User {
            let config_path = homes.codex.config_root.join("config.toml");
            if config_path.is_file() {
                let text = std::fs::read_to_string(&config_path)?;
                parts.push(parse_codex_mcp_toml(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &text,
                    &config_path,
                )?);
                parts.push(parse_codex_agents_toml(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &text,
                    &config_path,
                )?);
            }
            // Plugin-provided skills under config_root/plugins/** 若存在则 native/plugin
            let plugins = homes.codex.config_root.join("plugins");
            if plugins.is_dir() {
                parts.push(scan_codex_plugin_skills(scope.scope_kind, &plugins)?);
            }
            if let Some(compat) = &homes.codex.skill_compat_root {
                // skill_compat_root 指向 ~/.agents，skills 在 .agents/skills
                let skills_root = if compat.ends_with("skills") {
                    compat.clone()
                } else {
                    compat.join("skills")
                };
                parts.push(scan_skill_dirs(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &skills_root,
                    PortableOriginKind::LegacyStandalone,
                )?);
            }
        } else {
            // 项目级：可选 .codex/config.toml 与项目 .agents/skills
            let project_config = scope.absolute_path.join(".codex").join("config.toml");
            if project_config.is_file() {
                let text = std::fs::read_to_string(&project_config)?;
                parts.push(parse_codex_mcp_toml(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &text,
                    &project_config,
                )?);
                parts.push(parse_codex_agents_toml(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &text,
                    &project_config,
                )?);
            }
            parts.push(scan_skill_dirs(
                AgentTarget::Codex,
                scope.scope_kind,
                &scope.absolute_path.join(".agents").join("skills"),
                PortableOriginKind::LegacyStandalone,
            )?);
        }

        Ok(merge_discoveries(parts))
    }

    /// Inventory 精确 kind 扫描；避免 Skill 页解析 MCP/Agent 配置和无关组件树。
    fn scan_portable_assets_filtered(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
        kind: Option<AssetKind>,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let Some(kind) = kind else {
            return self.scan_portable_assets(scope, env);
        };
        let homes = TargetPathResolver::resolve_all(env);
        let mut parts = Vec::new();
        if scope.scope_kind == ScopeKind::User {
            if matches!(kind, AssetKind::Mcp | AssetKind::Agent) {
                let config_path = homes.codex.config_root.join("config.toml");
                if config_path.is_file() {
                    let text = std::fs::read_to_string(&config_path)?;
                    if kind == AssetKind::Mcp {
                        parts.push(parse_codex_mcp_toml(
                            AgentTarget::Codex,
                            scope.scope_kind,
                            &text,
                            &config_path,
                        )?);
                    } else {
                        parts.push(parse_codex_agents_toml(
                            AgentTarget::Codex,
                            scope.scope_kind,
                            &text,
                            &config_path,
                        )?);
                    }
                }
            }
            if kind == AssetKind::Skill {
                if let Some(compat) = &homes.codex.skill_compat_root {
                    let skills_root = if compat.ends_with("skills") {
                        compat.clone()
                    } else {
                        compat.join("skills")
                    };
                    parts.push(scan_skill_dirs_manifest_only(
                        AgentTarget::Codex,
                        scope.scope_kind,
                        &skills_root,
                        PortableOriginKind::LegacyStandalone,
                    )?);
                }
            }
        } else {
            if matches!(kind, AssetKind::Mcp | AssetKind::Agent) {
                let config_path = scope.absolute_path.join(".codex/config.toml");
                if config_path.is_file() {
                    let text = std::fs::read_to_string(&config_path)?;
                    if kind == AssetKind::Mcp {
                        parts.push(parse_codex_mcp_toml(
                            AgentTarget::Codex,
                            scope.scope_kind,
                            &text,
                            &config_path,
                        )?);
                    } else {
                        parts.push(parse_codex_agents_toml(
                            AgentTarget::Codex,
                            scope.scope_kind,
                            &text,
                            &config_path,
                        )?);
                    }
                }
            }
            if kind == AssetKind::Skill {
                parts.push(scan_skill_dirs_manifest_only(
                    AgentTarget::Codex,
                    scope.scope_kind,
                    &scope.absolute_path.join(".agents/skills"),
                    PortableOriginKind::LegacyStandalone,
                )?);
            }
        }
        Ok(merge_discoveries(parts))
    }

    /// 渲染 Codex portable 投影计划。
    ///
    /// Business Logic: 最终物化进受管 plugin；本方法只生成相对路径计划；
    /// Gate D plugin package render 复用同一 portable renderer。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Codex, asset)
    }
}

/// 扫描 Codex plugins 目录下的 skills/commands（若存在），并 stamp parent plugin id。
fn scan_codex_plugin_skills(
    scope_kind: ScopeKind,
    plugins_root: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    use super::portable::scan_plugin_components_readonly;
    use crate::agent_hub::plugins::decompose::discover_plugin_source_for_target;

    let mut out = Vec::new();
    let read = match std::fs::read_dir(plugins_root) {
        Ok(r) => r,
        Err(_) => return Ok(vec![]),
    };
    for entry in read {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let plugin_id =
            discover_plugin_source_for_target(AgentTarget::Codex, &path, "scan", scope_kind)
                .map(|s| s.plugin_id)
                .unwrap_or_else(|_| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("plugin")
                        .to_string()
                });
        out.extend(scan_plugin_components_readonly(
            AgentTarget::Codex,
            scope_kind,
            &path,
            &plugin_id,
        )?);
    }
    Ok(out)
}

/// 扫描单层 Codex 指令候选。
///
/// Business Logic: 同层可能同时存在 override、AGENTS.md 与配置 fallback。
/// Code Logic: 收集存在文件；按优先级标记唯一 active；非空 inactive 写诊断。
fn scan_codex_layer(
    base: &Path,
    scope: &LocalScopeMapping,
) -> Result<Vec<InstructionSource>, AppError> {
    let override_path = base.join("AGENTS.override.md");
    let agents_path = base.join("AGENTS.md");

    let mut candidates: Vec<(PathBuf, InstructionSourceRole)> = Vec::new();
    if override_path.exists() {
        candidates.push((override_path, InstructionSourceRole::ManagedProjection));
    }
    if agents_path.exists() {
        candidates.push((agents_path, InstructionSourceRole::NativePrimary));
    }

    let fallback_names: Vec<String> = if scope.codex_fallback_filenames.is_empty() {
        DEFAULT_CODEX_FALLBACKS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        scope.codex_fallback_filenames.clone()
    };
    for name in fallback_names {
        // 跳过与主文件同名，避免重复
        if name == "AGENTS.md" || name == "AGENTS.override.md" {
            continue;
        }
        let path = base.join(&name);
        if path.exists() {
            candidates.push((path, InstructionSourceRole::Fallback));
        }
    }

    // 优先级：ManagedProjection > NativePrimary > Fallback（同角色按插入序）
    let active_index = candidates
        .iter()
        .position(|(_, role)| {
            matches!(
                role,
                InstructionSourceRole::ManagedProjection | InstructionSourceRole::NativePrimary
            )
        })
        .or_else(|| {
            candidates
                .iter()
                .position(|(_, role)| *role == InstructionSourceRole::Fallback)
        });

    let mut sources = Vec::with_capacity(candidates.len());
    for (idx, (path, role)) in candidates.into_iter().enumerate() {
        let non_empty = is_non_empty_utf8_file(&path)?;
        let active = Some(idx) == active_index;
        let mut diagnostics = Vec::new();
        if !active && non_empty {
            diagnostics.push(format!(
                "shadowed_by_higher_priority:{} 被更高优先级 Codex 指令源遮蔽（非空，导入 preview 保留）",
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
            ));
        }
        let relative_path = scope
            .project_root
            .as_ref()
            .and_then(|root| relative_path_string(root, &path));
        sources.push(InstructionSource {
            target: AgentTarget::Codex,
            path,
            scope_kind: scope.scope_kind,
            role,
            active,
            native_active: active && role == InstructionSourceRole::NativePrimary,
            non_empty,
            relative_path,
            diagnostics,
        });
    }
    Ok(sources)
}

/// Codex managed package 激活选择器（`plugin@cc-partner`）。
///
/// Business Logic: plugin add/remove 使用与 marketplace 一致的 selector。
/// Code Logic: 委托 packages::PLUGIN_SELECTOR。
pub fn codex_managed_plugin_selector() -> &'static str {
    crate::agent_hub::packages::PLUGIN_SELECTOR
}

/// 从 Codex Plugin 根目录构造 `DiscoveredPluginSource`（不扫描 child）。
///
/// Business Logic（为什么需要这个函数）:
///     Gate D 分解入口需要 target 侧稳定构造发现记录；实际 component 扫描在 `plugins::decompose`。
///
/// Code Logic（这个函数做什么）:
///     读取 `.codex-plugin/plugin.json` 的 name/version/description（若存在），否则用目录名。
pub fn discover_codex_plugin_source(
    root: &std::path::Path,
    scope_id: impl Into<String>,
    scope_kind: ScopeKind,
) -> Result<crate::agent_hub::plugins::DiscoveredPluginSource, AppError> {
    crate::agent_hub::plugins::decompose::discover_plugin_source_for_target(
        crate::agent_hub::models::AgentTarget::Codex,
        root,
        scope_id,
        scope_kind,
    )
}

/// Codex disable 策略：remove-with-binding-retained。
///
/// Business Logic: desiredEnabled=false 不生成 canonical tombstone，只 remove plugin。
/// Code Logic: 返回稳定策略 token。
pub fn codex_disable_strategy() -> &'static str {
    "remove_with_binding_retained"
}
