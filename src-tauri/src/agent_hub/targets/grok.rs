//! agent_hub/targets/grok — Grok Build instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Grok 同时加载仓库 `AGENTS.md`/`CLAUDE.md` 与 `.grok/rules/*.md`；公共槽不得再写一份
//!     会与 Codex/OpenCode 抢 `AGENTS.md` 的文件。专属语义只落到
//!     `.grok/rules/cc-partner.adapted.md` / `cc-partner.exclusive.md`。
//!     Claude 兼容目录 `~/.claude` / `.claude` 禁止当作 Grok native 输出。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `grok`；扫描 rules 与只读原生指令；portable 只扫
//!     `.grok/skills|commands` 与 `config.toml` 的 `[mcp_servers.*]`；render 落到 `.grok/rules/`。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::portable::{
    merge_discoveries, parse_codex_mcp_toml, render_portable_payload, scan_command_markdown_dir,
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
use std::fs;
use std::path::{Path, PathBuf};

/// Grok 受管 adapted 文件名。
pub(crate) const GROK_ADAPTED_FILE: &str = "cc-partner.adapted.md";
/// Grok 受管 exclusive 文件名。
pub(crate) const GROK_EXCLUSIVE_FILE: &str = "cc-partner.exclusive.md";

/// Grok 指令槽（公共不物化，专属写入 rules）。
///
/// Business Logic（为什么需要这个枚举）:
///     render / 单测必须按槽选择落点，禁止把 common 写成仓库根 `AGENTS.md`。
///
/// Code Logic（这个枚举做什么）:
///     `Common` / `Adapted` / `Exclusive` 三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrokInstructionSlot {
    /// 公共槽：不单独物化
    Common,
    /// 适配槽：`.grok/rules/cc-partner.adapted.md`
    Adapted,
    /// 独有槽：`.grok/rules/cc-partner.exclusive.md`
    Exclusive,
}

/// Grok 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service / inventory / portable scanner 通过统一 `AssetAdapter` 调用 Grok 路径语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct GrokInstructionAdapter;

impl AssetAdapter for GrokInstructionAdapter {
    /// 返回 Grok 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Grok`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Grok
    }

    /// 探测 Grok 可执行文件、版本与配置根。
    ///
    /// Business Logic: 版本未知只能 scan-only；`GROK_HOME` 覆盖默认 `~/.grok`。
    /// Code Logic: `resolve_all` + `resolve_executable("grok")` + `probe_cli_version` + `build_probe`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("grok", env);
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
        Ok(build_probe(
            AgentTarget::Grok,
            executable,
            version,
            homes.grok.config_root,
        ))
    }

    /// 扫描 Grok 指令源。
    ///
    /// Business Logic: 用户级扫 `config_root/rules/*.md` 与 grok home 内原生 AGENTS/CLAUDE；
    ///     项目级扫 `<root>/.grok/rules/cc-partner.*` 与目录内只读 AGENTS/CLAUDE；缺失不报错。
    /// Code Logic: 受管文件标 ManagedProjection；不把 `~/.claude` 列入 Grok 源。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError> {
        match scope.scope_kind {
            ScopeKind::User => scan_user_instructions(scope, env),
            ScopeKind::Project | ScopeKind::Directory => scan_project_instructions(scope),
        }
    }

    /// 渲染 Grok 受管指令。
    ///
    /// Business Logic: common 不得物化 `AGENTS.md`；compiler 只给一个 file_name 时落到
    ///     `.grok/rules/` 下该文件。
    /// Code Logic: `compile_render` 后把 file_name 改写为 `.grok/rules/<name>`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Grok,
            context,
        );
        let mut rendered = RenderedInstruction::from_compiled(compiled);
        rendered.file_name = grok_render_file_name(&rendered.file_name);
        Ok(rendered)
    }

    /// 扫描 Grok native Skill/Command 与 user MCP。
    ///
    /// Business Logic: 只扫 `.grok` 树与 `config.toml`；禁止把 Claude 目录当 Grok native。
    /// Code Logic: user=`config_root/{skills,commands,config.toml}`；project=`<root>/.grok/...`。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let base = grok_portable_root(scope, &homes);
        let mut parts: Vec<Vec<DiscoveredPortableAsset>> = Vec::new();
        parts.push(scan_skill_dirs(
            AgentTarget::Grok,
            scope.scope_kind,
            &base.join("skills"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_command_markdown_dir(
            AgentTarget::Grok,
            scope.scope_kind,
            &base.join("commands"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_grok_mcp(scope, &homes)?);
        Ok(merge_discoveries(parts))
    }

    /// Inventory 精确 kind 扫描。
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
        let base = grok_portable_root(scope, &homes);
        let mut parts = Vec::new();
        match kind {
            AssetKind::Skill => parts.push(scan_skill_dirs_manifest_only(
                AgentTarget::Grok,
                scope.scope_kind,
                &base.join("skills"),
                PortableOriginKind::Native,
            )?),
            AssetKind::Command => parts.push(scan_command_markdown_dir(
                AgentTarget::Grok,
                scope.scope_kind,
                &base.join("commands"),
                PortableOriginKind::Native,
            )?),
            AssetKind::Mcp => parts.push(scan_grok_mcp(scope, &homes)?),
            AssetKind::Agent | AssetKind::Instruction | AssetKind::Plugin | AssetKind::Hook => {}
        }
        Ok(merge_discoveries(parts))
    }

    /// 渲染 Grok portable 投影。
    ///
    /// Business Logic: 计划路径落到 skills/commands，由物化层放入 `.grok/`；不写盘。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Grok, asset)
    }
}

/// Grok 指令槽相对路径。
///
/// Business Logic（为什么需要这个函数）:
///     common 不单独物化；adapted/exclusive 必须落到 `.grok/rules/cc-partner.*`。
///
/// Code Logic（这个函数做什么）:
///     Common → None；其余返回固定相对路径。
pub fn grok_instruction_rel_path(slot: GrokInstructionSlot) -> Option<&'static str> {
    match slot {
        GrokInstructionSlot::Common => None,
        GrokInstructionSlot::Adapted => Some(".grok/rules/cc-partner.adapted.md"),
        GrokInstructionSlot::Exclusive => Some(".grok/rules/cc-partner.exclusive.md"),
    }
}

/// 把 compiler 文件名改写到 `.grok/rules/`，永不输出仓库根 `AGENTS.md`。
///
/// Business Logic（为什么需要这个函数）:
///     compiler 当前只给一个 file_name（多为 exclusive）；Hub 仍不得把 common 写成 AGENTS.md。
///
/// Code Logic（这个函数做什么）:
///     拒绝 `AGENTS.md`；缺省落到 `cc-partner.exclusive.md`；已含 `.grok/rules/` 则原样返回。
fn grok_render_file_name(compiler_name: &str) -> String {
    let name = compiler_name.replace('\\', "/");
    if name == "AGENTS.md" || name.ends_with("/AGENTS.md") || name.is_empty() {
        return ".grok/rules/cc-partner.exclusive.md".to_string();
    }
    if name.contains(".grok/rules/") {
        return name;
    }
    let file = Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(GROK_EXCLUSIVE_FILE);
    format!(".grok/rules/{file}")
}

/// 用户级 / 项目级 portable 根。
fn grok_portable_root(scope: &LocalScopeMapping, homes: &super::paths::TargetHomes) -> PathBuf {
    match scope.scope_kind {
        ScopeKind::User => homes.grok.config_root.clone(),
        ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".grok"),
    }
}

/// 扫描用户级 Grok 指令。
///
/// Business Logic: 只读 grok home 内 rules 与原生文件；不扫 `~/.claude`。
/// Code Logic: rules/*.md + config_root 下 AGENTS.md / CLAUDE.md 变体。
fn scan_user_instructions(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let root = &homes.grok.config_root;
    let mut sources = Vec::new();
    push_rules_dir(&mut sources, &root.join("rules"), scope)?;
    push_existing(
        &mut sources,
        root.join("AGENTS.md"),
        scope,
        InstructionSourceRole::NativePrimary,
    )?;
    for name in ["CLAUDE.md", "Claude.md", "CLAUDE.local.md"] {
        push_existing(
            &mut sources,
            root.join(name),
            scope,
            InstructionSourceRole::Fallback,
        )?;
    }
    Ok(sources)
}

/// 扫描项目/目录级 Grok 指令。
///
/// Business Logic: 受管文件在 `.grok/rules/`；目录内 AGENTS/CLAUDE 只读，不把 common 写成新 AGENTS.md。
/// Code Logic: 扫 rules 目录 + 当前目录原生文件。
fn scan_project_instructions(
    scope: &LocalScopeMapping,
) -> Result<Vec<InstructionSource>, AppError> {
    let mut sources = Vec::new();
    push_rules_dir(
        &mut sources,
        &scope.absolute_path.join(".grok").join("rules"),
        scope,
    )?;
    push_existing(
        &mut sources,
        scope.absolute_path.join("AGENTS.md"),
        scope,
        InstructionSourceRole::NativePrimary,
    )?;
    for name in ["CLAUDE.md", "Claude.md", "CLAUDE.local.md"] {
        push_existing(
            &mut sources,
            scope.absolute_path.join(name),
            scope,
            InstructionSourceRole::Fallback,
        )?;
    }
    Ok(sources)
}

/// 扫描 `.grok/rules/*.md`。
///
/// Business Logic: 受管固定文件名便于 ownership；其它用户 rules 只读登记，不得改写。
/// Code Logic: 按文件名分配 ManagedProjection / NativePrimary。
fn push_rules_dir(
    sources: &mut Vec<InstructionSource>,
    rules_dir: &Path,
    scope: &LocalScopeMapping,
) -> Result<(), AppError> {
    if !rules_dir.is_dir() {
        return Ok(());
    }
    let mut files: Vec<PathBuf> = match fs::read_dir(rules_dir) {
        Ok(rd) => rd
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md")
            })
            .collect(),
        Err(_) => return Ok(()),
    };
    files.sort();
    for path in files {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let role = if name == GROK_ADAPTED_FILE || name == GROK_EXCLUSIVE_FILE {
            InstructionSourceRole::ManagedProjection
        } else {
            InstructionSourceRole::NativePrimary
        };
        push_existing(sources, path, scope, role)?;
    }
    Ok(())
}

/// 文件存在则登记为指令源。
fn push_existing(
    sources: &mut Vec<InstructionSource>,
    path: PathBuf,
    scope: &LocalScopeMapping,
    role: InstructionSourceRole,
) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    if sources.iter().any(|source| source.path == path) {
        return Ok(());
    }
    let non_empty = is_non_empty_utf8_file(&path)?;
    let relative_path = scope
        .project_root
        .as_ref()
        .and_then(|root| relative_path_string(root, &path))
        .or_else(|| scope.relative_root.clone());
    sources.push(InstructionSource {
        target: AgentTarget::Grok,
        path,
        scope_kind: scope.scope_kind,
        role,
        active: true,
        native_active: role == InstructionSourceRole::NativePrimary,
        non_empty,
        relative_path,
        diagnostics: vec![],
    });
    Ok(())
}

/// 只读解析 Grok `config.toml` 的 `[mcp_servers.*]`。
///
/// Business Logic: 复用 Codex TOML helper；仅扫 Grok 配置根，不读 Claude JSON。
/// Code Logic: user=`config_root/config.toml`；project=`.grok/config.toml`。
fn scan_grok_mcp(
    scope: &LocalScopeMapping,
    homes: &super::paths::TargetHomes,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let path = match scope.scope_kind {
        ScopeKind::User => homes.grok.config_root.join("config.toml"),
        ScopeKind::Project | ScopeKind::Directory => {
            scope.absolute_path.join(".grok").join("config.toml")
        }
    };
    if !path.is_file() {
        return Ok(vec![]);
    }
    let text = fs::read_to_string(&path)?;
    parse_codex_mcp_toml(AgentTarget::Grok, scope.scope_kind, &text, &path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::portable::PortableOriginKind;
    use std::collections::BTreeMap;
    use std::fs;

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn isolated_env(home: &Path) -> TargetEnvironment {
        let grok = home.join(".grok");
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::from([("GROK_HOME".into(), grok.to_string_lossy().into_owned())]),
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

    #[test]
    fn common_render_does_not_emit_agents_md() {
        let doc = InstructionDocument {
            common_markdown: "# shared rules\n".into(),
            relative_key: String::new(),
        };
        let rendered = GrokInstructionAdapter
            .render_instruction(&doc, &InstructionRenderContext::default())
            .unwrap();
        let name = rendered.file_name.replace('\\', "/");
        assert_ne!(
            Path::new(&name).file_name().and_then(|s| s.to_str()),
            Some("AGENTS.md")
        );
        assert!(!name.ends_with("/AGENTS.md"));
        assert_ne!(name, "AGENTS.md");
        assert!(
            grok_instruction_rel_path(GrokInstructionSlot::Common).is_none(),
            "common must not materialize a dedicated file"
        );
    }

    #[test]
    fn adapted_and_exclusive_paths_live_under_grok_rules() {
        let adapted = grok_instruction_rel_path(GrokInstructionSlot::Adapted).expect("adapted");
        let exclusive =
            grok_instruction_rel_path(GrokInstructionSlot::Exclusive).expect("exclusive");
        assert!(
            adapted.contains(".grok/rules/cc-partner."),
            "adapted={adapted}"
        );
        assert!(
            exclusive.contains(".grok/rules/cc-partner."),
            "exclusive={exclusive}"
        );
        let rendered = GrokInstructionAdapter
            .render_instruction(
                &InstructionDocument {
                    common_markdown: "x".into(),
                    relative_key: String::new(),
                },
                &InstructionRenderContext::default(),
            )
            .unwrap();
        assert!(
            rendered
                .file_name
                .replace('\\', "/")
                .contains(".grok/rules/cc-partner."),
            "render file_name={}",
            rendered.file_name
        );
    }

    #[test]
    fn scan_does_not_mark_claude_home_as_grok_native() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(&home.join(".claude/CLAUDE.md"), "claude user rules\n");
        write_text(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".grok/rules/cc-partner.exclusive.md"),
            "grok exclusive\n",
        );
        write_text(
            &home.join(".grok/skills/grok-skill/SKILL.md"),
            "---\nname: grok-skill\ndescription: d\n---\nbody\n",
        );
        let env = isolated_env(home);
        let scope = user_scope(home);

        let sources = GrokInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        assert!(
            sources.iter().all(|source| {
                !source
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .contains("/.claude/")
            }),
            "instruction sources leaked ~/.claude: {sources:?}"
        );
        assert!(sources.iter().any(|source| {
            source.role == InstructionSourceRole::ManagedProjection
                && source
                    .path
                    .to_string_lossy()
                    .contains("cc-partner.exclusive.md")
        }));

        let found = GrokInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(
            found.iter().all(|asset| {
                let path = asset.origin.path.to_string_lossy().replace('\\', "/");
                !path.contains("/.claude/")
                    || asset.origin.origin_kind != PortableOriginKind::Native
            }),
            "portable scan marked ~/.claude as Grok native: {found:?}"
        );
        assert!(found
            .iter()
            .any(|asset| asset.semantic_name == "grok-skill"));
        assert!(found
            .iter()
            .all(|asset| asset.origin.target == AgentTarget::Grok));
    }
}
