//! agent_hub/targets/opencode — OpenCode instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     OpenCode 从 cwd 向上找本地 AGENTS.md 并采用最近命中；Hub 不能声称原生拼接祖先链，
//!     必须把祖先规则列为显式 prelude 依赖，并在渲染时写入 target-only contract。
//!     Gate B：原生 `.opencode`/config-root Skills/Commands/Agents/MCP；
//!     `.claude/skills` 与 `.agents/skills` 仅 compatibility origins。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `opencode` 与 OPENCODE_* 路径；scan 指令与 portable；
//!     render 指令 prelude 与 portable 投影。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::portable::{
    merge_discoveries, parse_json_or_jsonc, parse_mcp_servers_json_map, render_portable_payload,
    scan_agent_markdown_dir, scan_command_markdown_dir, scan_skill_dirs, AssetRenderContext,
    DiscoveredPortableAsset, PortableOriginKind, TargetAssetProjection,
};
use super::{
    build_probe, relative_path_string, AssetAdapter, InstructionDocument, InstructionRenderContext,
    InstructionSource, InstructionSourceRole, LocalScopeMapping, RenderedInstruction,
    TargetEnvironment, TargetProbe,
};
use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::error::AppError;
use std::path::{Path, PathBuf};

/// OpenCode 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     嵌套目录必须列出祖先规则相对路径，不复制祖先正文。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenCodeInstructionAdapter;

impl AssetAdapter for OpenCodeInstructionAdapter {
    /// 返回 OpenCode 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::OpenCode`。
    fn target(&self) -> AgentTarget {
        AgentTarget::OpenCode
    }

    /// 探测 OpenCode 可执行文件、版本与配置根。
    ///
    /// Business Logic: 版本未知只能 scan-only。
    /// Code Logic: OPENCODE_CONFIG_DIR / XDG / 默认；查找 `opencode`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("opencode", env);
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
        Ok(build_probe(
            AgentTarget::OpenCode,
            executable,
            version,
            homes.opencode.config_root,
        ))
    }

    /// 扫描 OpenCode 指令源。
    ///
    /// Business Logic: 最近本地 AGENTS.md 为 native-active；祖先作为 prelude 依赖返回。
    /// Code Logic: user 扫 config_root；directory/project 从当前目录向上至 project_root。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError> {
        match scope.scope_kind {
            ScopeKind::User => scan_user_scope(scope, env),
            ScopeKind::Project | ScopeKind::Directory => scan_project_chain(scope),
        }
    }

    /// 渲染 OpenCode `AGENTS.md`，前置祖先 prelude contract。
    ///
    /// Business Logic: 明确相对路径列表，不复制祖先正文、不反向进入 shared。
    /// Code Logic: Instruction Compiler 写入 managed_prefix + 用户 body。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::OpenCode,
            context,
        );
        Ok(RenderedInstruction::from_compiled(compiled))
    }

    /// 扫描 OpenCode portable 资产（native + compatibility）。
    ///
    /// Business Logic: `.claude/skills` / `.agents/skills` 标记 compatibility，非 native 输出。
    /// Code Logic: config_root 原生树 + home 兼容 skills + opencode.json(c) MCP。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let mut parts: Vec<Vec<DiscoveredPortableAsset>> = Vec::new();

        let native_root = match scope.scope_kind {
            ScopeKind::User => homes.opencode.config_root.clone(),
            ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".opencode"),
        };

        parts.push(scan_skill_dirs(
            AgentTarget::OpenCode,
            scope.scope_kind,
            &native_root.join("skills"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_command_markdown_dir(
            AgentTarget::OpenCode,
            scope.scope_kind,
            &native_root.join("commands"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_agent_markdown_dir(
            AgentTarget::OpenCode,
            scope.scope_kind,
            &native_root.join("agents"),
            PortableOriginKind::Native,
        )?);

        // MCP from OPENCODE_CONFIG / opencode.json(c)
        if scope.scope_kind == ScopeKind::User {
            parts.push(scan_opencode_mcp_config(scope.scope_kind, env, &homes)?);
            // Compatibility skill roots (user home)
            parts.push(scan_skill_dirs(
                AgentTarget::OpenCode,
                scope.scope_kind,
                &env.home.join(".claude").join("skills"),
                PortableOriginKind::Compatibility,
            )?);
            parts.push(scan_skill_dirs(
                AgentTarget::OpenCode,
                scope.scope_kind,
                &env.home.join(".agents").join("skills"),
                PortableOriginKind::Compatibility,
            )?);
        } else {
            // 项目级兼容路径
            parts.push(scan_skill_dirs(
                AgentTarget::OpenCode,
                scope.scope_kind,
                &scope.absolute_path.join(".claude").join("skills"),
                PortableOriginKind::Compatibility,
            )?);
            parts.push(scan_skill_dirs(
                AgentTarget::OpenCode,
                scope.scope_kind,
                &scope.absolute_path.join(".agents").join("skills"),
                PortableOriginKind::Compatibility,
            )?);
            for name in ["opencode.json", "opencode.jsonc"] {
                let p = scope.absolute_path.join(name);
                if p.is_file() {
                    parts.push(scan_mcp_file(scope.scope_kind, &p)?);
                }
            }
        }

        Ok(merge_discoveries(parts))
    }

    /// 渲染 OpenCode portable 投影。
    ///
    /// Business Logic: 只写入原生 `.opencode`/config-root 计划路径；
    /// Gate D plugin package render 复用同一 portable renderer，residual 默认 source-only。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::OpenCode, asset)
    }
}

/// 扫描 OpenCode 用户 MCP 配置文件。
fn scan_opencode_mcp_config(
    scope_kind: ScopeKind,
    env: &TargetEnvironment,
    homes: &super::paths::TargetHomes,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let mut candidates = vec![homes.opencode.config_file.clone()];
    // 常见额外文件名
    candidates.push(homes.opencode.config_root.join("opencode.jsonc"));
    candidates.push(homes.opencode.config_root.join("opencode.json"));
    // 测试/用户可能把 OPENCODE_CONFIG 指到 home 根
    if let Some(p) = env.var("OPENCODE_CONFIG") {
        candidates.insert(0, PathBuf::from(p));
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let key = path.to_string_lossy().to_string();
        if !seen.insert(key) {
            continue;
        }
        out.extend(scan_mcp_file(scope_kind, &path)?);
    }
    Ok(out)
}

fn scan_mcp_file(
    scope_kind: ScopeKind,
    path: &Path,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let text = std::fs::read_to_string(path)?;
    let value = match parse_json_or_jsonc(&text) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()).cloned() else {
        return Ok(vec![]);
    };
    Ok(parse_mcp_servers_json_map(
        AgentTarget::OpenCode,
        scope_kind,
        &map,
        path,
        PortableOriginKind::Native,
        true,
    ))
}

/// 扫描用户级 OpenCode AGENTS.md。
///
/// Business Logic: 用户级配置根下的 AGENTS.md 是唯一用户源。
/// Code Logic: 缺失返回空。
fn scan_user_scope(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let path = homes.opencode.config_root.join("AGENTS.md");
    if !path.exists() {
        return Ok(vec![]);
    }
    let non_empty = is_non_empty_utf8_file(&path)?;
    Ok(vec![InstructionSource {
        target: AgentTarget::OpenCode,
        path,
        scope_kind: ScopeKind::User,
        role: InstructionSourceRole::NativePrimary,
        active: true,
        native_active: true,
        non_empty,
        relative_path: scope.relative_root.clone(),
        diagnostics: vec![],
    }])
}

/// 从当前目录向上到项目根扫描 AGENTS.md 链。
///
/// Business Logic: 最近命中 native-active；祖先 explicit prelude，不 active。
/// Code Logic: 先收集存在文件，最近目录为 active；其余 AncestorPrelude。
fn scan_project_chain(scope: &LocalScopeMapping) -> Result<Vec<InstructionSource>, AppError> {
    let project_root = scope
        .project_root
        .clone()
        .unwrap_or_else(|| scope.absolute_path.clone());
    let project_root = canonicalize_or_clone(&project_root);
    let mut current = canonicalize_or_clone(&scope.absolute_path);

    let mut found: Vec<PathBuf> = Vec::new();
    loop {
        let candidate = current.join("AGENTS.md");
        if candidate.exists() {
            found.push(candidate);
        }
        if paths_equal(&current, &project_root) {
            break;
        }
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => {
                current = parent.to_path_buf();
            }
            _ => break,
        }
        // 防止越界：若已不在 project_root 下则停止
        if !current.starts_with(&project_root) && !paths_equal(&current, &project_root) {
            break;
        }
    }

    if found.is_empty() {
        return Ok(vec![]);
    }

    // found[0] 是最近目录
    let mut sources = Vec::with_capacity(found.len());
    for (idx, path) in found.into_iter().enumerate() {
        let non_empty = is_non_empty_utf8_file(&path)?;
        let is_nearest = idx == 0;
        let relative_path = relative_path_string(&project_root, &path);
        sources.push(InstructionSource {
            target: AgentTarget::OpenCode,
            path,
            scope_kind: scope.scope_kind,
            role: if is_nearest {
                InstructionSourceRole::NativePrimary
            } else {
                InstructionSourceRole::AncestorPrelude
            },
            active: is_nearest,
            native_active: is_nearest,
            non_empty,
            relative_path,
            diagnostics: if is_nearest {
                vec![]
            } else {
                vec![
                    "ancestor_prelude_dependency:须作为 OpenCode prelude 显式读取，不复制正文"
                        .into(),
                ]
            },
        });
    }
    Ok(sources)
}

/// canonicalize 失败则 clone。
fn canonicalize_or_clone(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// 比较两个路径（canonicalize 后或字符串）。
fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// OpenCode 受管输出使用原生 skills/commands/agents，而非 plugin CLI。
///
/// Business Logic: 激活 = 原子 native-path 投影 + scanner 验证。
/// Code Logic: 返回策略 token。
pub fn opencode_activation_strategy() -> &'static str {
    "native_path_projection"
}

/// 从 OpenCode 本地 Plugin 根目录构造 `DiscoveredPluginSource`（不扫描 child）。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 原生 JS/TS/npm plugin 仍进入同一分解路径；runtime 默认 source residual。
///
/// Code Logic（这个函数做什么）:
///     优先 `package.json` name/version/description，否则用目录名。
pub fn discover_opencode_plugin_source(
    root: &std::path::Path,
    scope_id: impl Into<String>,
    scope_kind: ScopeKind,
) -> Result<crate::agent_hub::plugins::DiscoveredPluginSource, AppError> {
    crate::agent_hub::plugins::decompose::discover_plugin_source_for_target(
        crate::agent_hub::models::AgentTarget::OpenCode,
        root,
        scope_id,
        scope_kind,
    )
}
