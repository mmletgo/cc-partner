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
    scan_disabled_skill_dirs, scan_disabled_skill_dirs_manifest_only, AssetRenderContext,
    DiscoveredPortableAsset, PortableOriginKind, TargetAssetProjection,
};
use super::{
    build_probe, relative_path_string, AssetAdapter, InstructionDocument, InstructionRenderContext,
    InstructionSource, InstructionSourceRole, LocalScopeMapping, RenderedInstruction,
    TargetEnvironment, TargetProbe,
};
use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::agent_hub::support::scan_table_roots;
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
    /// Business Logic: 最近本地 AGENTS.md 为 native-active；用户级原生文件缺失时识别 Claude 兼容回退。
    /// Code Logic: user 扫 config_root + Claude fallback；directory/project 从当前目录向上至 project_root。
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

    /// 扫描 OpenCode portable 资产（表驱动 native/compat + 表外 extras）。
    ///
    /// Business Logic: `.claude/skills` / `.agents/skills` 标记 compatibility，非 native 输出；
    ///     MCP 与 `disabled/skills` 不在发现表，必须由适配器合并。
    /// Code Logic: `scan_table_roots` + disabled-skill helper + opencode.json(c) MCP。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let mut parts = vec![scan_table_roots(
            AgentTarget::OpenCode,
            scope,
            env,
            None,
            false,
        )?];
        parts.push(scan_opencode_disabled_skills(scope, env, false)?);
        parts.push(scan_opencode_mcp_extras(scope, env)?);
        Ok(merge_discoveries(parts))
    }

    /// Inventory 精确 kind 扫描；Plugin component 由 inventory 权威安装根扫描器补充。
    ///
    /// Business Logic: Skill 过滤走 manifest-only，并保留 disabled extras；MCP 仍走表外扫描。
    /// Code Logic: 表根 + disabled skills；Command/Agent/Plugin 仅表；MCP 仅 extras。
    fn scan_portable_assets_filtered(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
        kind: Option<AssetKind>,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let Some(kind) = kind else {
            return self.scan_portable_assets(scope, env);
        };
        let mut parts = Vec::new();
        match kind {
            AssetKind::Skill => {
                parts.push(scan_table_roots(
                    AgentTarget::OpenCode,
                    scope,
                    env,
                    Some(AssetKind::Skill),
                    true,
                )?);
                parts.push(scan_opencode_disabled_skills(scope, env, true)?);
            }
            AssetKind::Command | AssetKind::Agent | AssetKind::Plugin => {
                parts.push(scan_table_roots(
                    AgentTarget::OpenCode,
                    scope,
                    env,
                    Some(kind),
                    false,
                )?);
            }
            AssetKind::Mcp => parts.push(scan_opencode_mcp_extras(scope, env)?),
            AssetKind::Instruction | AssetKind::Hook => {}
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

/// 扫描表外 `disabled/skills`（发现表不含该根）。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 禁用技能仍需进入库存，但不能标成 compatibility 或改写 native 根。
///
/// Code Logic（这个函数做什么）:
///     user=`config_root/disabled/skills`；project=`.opencode/disabled/skills`。
fn scan_opencode_disabled_skills(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
    manifest_only: bool,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let native_root = match scope.scope_kind {
        ScopeKind::User => homes.opencode.config_root.clone(),
        ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".opencode"),
    };
    let root = native_root.join("disabled").join("skills");
    if manifest_only {
        scan_disabled_skill_dirs_manifest_only(
            AgentTarget::OpenCode,
            scope.scope_kind,
            &root,
            PortableOriginKind::Native,
        )
    } else {
        scan_disabled_skill_dirs(
            AgentTarget::OpenCode,
            scope.scope_kind,
            &root,
            PortableOriginKind::Native,
        )
    }
}

/// 扫描表外 OpenCode MCP（发现表不含 MCP 行）。
///
/// Business Logic（为什么需要这个函数）:
///     用户级读 OPENCODE_CONFIG / config_root json(c)；项目级读仓库根 opencode.json(c)。
///
/// Code Logic（这个函数做什么）:
///     委托既有 `scan_opencode_mcp_config` 与 `scan_mcp_file`。
fn scan_opencode_mcp_extras(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    if scope.scope_kind == ScopeKind::User {
        return scan_opencode_mcp_config(scope.scope_kind, env, &homes);
    }
    let mut out = Vec::new();
    for name in ["opencode.json", "opencode.jsonc"] {
        let path = scope.absolute_path.join(name);
        if path.is_file() {
            out.extend(scan_mcp_file(scope.scope_kind, &path)?);
        }
    }
    Ok(out)
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
/// Business Logic: OpenCode 原生 AGENTS.md 优先；缺失且兼容未禁用时回退到 Claude CLAUDE.md。
/// Code Logic: 同时列出原生源与可能的 fallback；两个禁用环境变量任一有值即不声称回退生效。
fn scan_user_scope(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let native_path = homes.opencode.config_root.join("AGENTS.md");
    let fallback_disabled = env.var("OPENCODE_DISABLE_CLAUDE_CODE").is_some()
        || env.var("OPENCODE_DISABLE_CLAUDE_CODE_PROMPT").is_some();
    let fallback_path = homes.claude.config_root.join("CLAUDE.md");
    let native_exists = native_path.exists();
    let mut sources = Vec::new();

    if native_exists {
        sources.push(InstructionSource {
            target: AgentTarget::OpenCode,
            path: native_path,
            scope_kind: ScopeKind::User,
            role: InstructionSourceRole::NativePrimary,
            active: true,
            native_active: true,
            non_empty: is_non_empty_utf8_file(&homes.opencode.config_root.join("AGENTS.md"))?,
            relative_path: scope.relative_root.clone(),
            diagnostics: vec![],
        });
    }

    if fallback_path.exists() {
        let active = !native_exists && !fallback_disabled;
        let diagnostics = if fallback_disabled {
            vec!["opencode_claude_fallback_disabled".to_string()]
        } else if native_exists {
            vec!["opencode_claude_fallback_shadowed_by_native".to_string()]
        } else {
            vec![]
        };
        sources.push(InstructionSource {
            target: AgentTarget::OpenCode,
            path: fallback_path.clone(),
            scope_kind: ScopeKind::User,
            role: InstructionSourceRole::Fallback,
            active,
            native_active: false,
            non_empty: is_non_empty_utf8_file(&fallback_path)?,
            relative_path: scope.relative_root.clone(),
            diagnostics,
        });
    }

    Ok(sources)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::portable::{PortableAssetOwner, PortableDiscoveryStatus};
    use std::collections::BTreeMap;
    use std::fs;

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn isolated_env(home: &Path) -> TargetEnvironment {
        let config_root = home.join(".opencode");
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::from([
                (
                    "OPENCODE_CONFIG_DIR".into(),
                    config_root.to_string_lossy().into_owned(),
                ),
                (
                    "OPENCODE_CONFIG".into(),
                    config_root
                        .join("opencode.json")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ]),
            path_entries: vec![],
        }
    }

    fn user_scope(home: &Path) -> LocalScopeMapping {
        LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        }
    }

    fn seed_opencode_fixture(home: &Path) {
        write_text(
            &home.join(".opencode/skills/oc-skill/SKILL.md"),
            "---\nname: oc-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".opencode/disabled/skills/off-skill/SKILL.md"),
            "---\nname: off-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".claude/skills/claude-skill/SKILL.md"),
            "---\nname: claude-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".agents/skills/shared-skill/SKILL.md"),
            "---\nname: shared-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".opencode/opencode.json"),
            r#"{
  "mcpServers": {
    "private-api": {
      "command": "uvx",
      "args": ["oc-srv"]
    }
  }
}"#,
        );
    }

    #[test]
    fn user_scan_finds_compat_skills_and_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_opencode_fixture(home);
        let env = isolated_env(home);
        let scope = user_scope(home);
        let found = OpenCodeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();

        let native = found
            .iter()
            .find(|asset| asset.semantic_name == "oc-skill")
            .expect("native opencode skill");
        assert_eq!(native.origin.origin_kind, PortableOriginKind::Native);
        assert!(native.origin.native_output_candidate);
        assert_eq!(native.origin.owned_by, PortableAssetOwner::OpenCode);

        let claude = found
            .iter()
            .find(|asset| asset.semantic_name == "claude-skill")
            .expect("claude compatibility skill");
        assert_eq!(claude.origin.origin_kind, PortableOriginKind::Compatibility);
        assert!(!claude.origin.native_output_candidate);
        assert_eq!(claude.origin.owned_by, PortableAssetOwner::Claude);

        let shared = found
            .iter()
            .find(|asset| asset.semantic_name == "shared-skill")
            .expect("shared agents compatibility skill");
        assert_eq!(shared.origin.origin_kind, PortableOriginKind::Compatibility);
        assert!(!shared.origin.native_output_candidate);
        assert_eq!(shared.origin.owned_by, PortableAssetOwner::SharedAgents);

        let disabled = found
            .iter()
            .find(|asset| asset.semantic_name == "off-skill")
            .expect("disabled skill extra");
        assert_eq!(disabled.origin.status, PortableDiscoveryStatus::Disabled);
        assert_eq!(disabled.origin.origin_kind, PortableOriginKind::Native);

        assert!(found
            .iter()
            .any(|asset| { asset.kind == AssetKind::Mcp && asset.semantic_name == "private-api" }));
    }

    #[test]
    fn disable_claude_code_skills_hides_compat_but_keeps_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_opencode_fixture(home);
        let mut env = isolated_env(home);
        env.vars
            .insert("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS".into(), "1".into());
        let scope = user_scope(home);
        let found = OpenCodeInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();

        assert!(
            found.iter().all(|asset| {
                let path = asset.origin.path.to_string_lossy().replace('\\', "/");
                !path.contains("/.claude/") && !path.contains("/.agents/skills/")
            }),
            "compat skills must hide when disable env is set: {found:?}"
        );
        assert!(found.iter().any(|asset| asset.semantic_name == "oc-skill"));
        assert!(found
            .iter()
            .any(|asset| { asset.kind == AssetKind::Mcp && asset.semantic_name == "private-api" }));
        assert!(found.iter().any(|asset| asset.semantic_name == "off-skill"));
    }
}
