//! agent_hub/targets/pi — Pi Coding Agent instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Pi 同时加载仓库 `AGENTS.md`/`CLAUDE.md`（以及 `AGENTS.override.md`）与
//!     `~/.pi/agent/` / `.pi/` 下的专属文件；公共槽不得再写一份会与 Codex/OpenCode
//!     抢 `AGENTS.md` 的文件。专属语义只落到 `.pi/cc-partner.adapted.md` /
//!     `cc-partner.exclusive.md`。Pi 没有官方 rules 引擎，这些文件是 Hub 受管落点，
//!     原生写盘在 L3 evidence 前保持 blocked。Claude 兼容目录 `~/.claude` /
//!     `.claude` 禁止当作 Pi native 输出。Pi 故意不内建 MCP。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `pi`；扫描 `.pi` 受管文件与只读原生指令；
//!     portable 经 runtime-discovery 表扫 native skills、无条件 `.agents/skills`，
//!     以及 settings 点名后的 `~/.claude/skills`；render 落到 `.pi/`。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::portable::{
    render_portable_payload, AssetRenderContext, DiscoveredPortableAsset, TargetAssetProjection,
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

/// Pi 受管 adapted 文件名。
pub(crate) const PI_ADAPTED_FILE: &str = "cc-partner.adapted.md";
/// Pi 受管 exclusive 文件名。
pub(crate) const PI_EXCLUSIVE_FILE: &str = "cc-partner.exclusive.md";

/// Pi 指令槽（公共不物化，专属写入 `.pi/`）。
///
/// Business Logic（为什么需要这个枚举）:
///     render / 单测必须按槽选择落点，禁止把 common 写成仓库根 `AGENTS.md`。
///
/// Code Logic（这个枚举做什么）:
///     `Common` / `Adapted` / `Exclusive` 三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiInstructionSlot {
    /// 公共槽：不单独物化
    Common,
    /// 适配槽：`.pi/cc-partner.adapted.md`
    Adapted,
    /// 独有槽：`.pi/cc-partner.exclusive.md`
    Exclusive,
}

/// Pi 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service / inventory / portable scanner 通过统一 `AssetAdapter` 调用 Pi 路径语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct PiInstructionAdapter;

impl AssetAdapter for PiInstructionAdapter {
    /// 返回 Pi 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Pi`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Pi
    }

    /// 探测 Pi 可执行文件、版本与配置根。
    ///
    /// Business Logic: 官方 CLI 名为 `pi`；版本未知只能 scan-only；配置根为 `~/.pi/agent`。
    /// Code Logic: `resolve_all` + `resolve_executable("pi")` + `probe_cli_version` + `build_probe`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("pi", env);
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
        Ok(build_probe(
            AgentTarget::Pi,
            executable,
            version,
            homes.pi.config_root,
        ))
    }

    /// 扫描 Pi 指令源。
    ///
    /// Business Logic: 用户级扫 `~/.pi/agent` 受管文件与原生 AGENTS/CLAUDE；
    ///     项目级扫 `<root>/.pi/cc-partner.*` 与目录内只读 AGENTS/CLAUDE；缺失不报错。
    /// Code Logic: 受管文件标 ManagedProjection；不把 `~/.claude` 列入 Pi 源。
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

    /// 渲染 Pi 受管指令。
    ///
    /// Business Logic: common 不得物化 `AGENTS.md`；compiler 只给一个 file_name 时落到 `.pi/`。
    /// Code Logic: `compile_render` 后把 file_name 改写为 `.pi/<name>`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Pi,
            context,
        );
        let mut rendered = RenderedInstruction::from_compiled(compiled);
        rendered.file_name = pi_render_file_name(&rendered.file_name);
        Ok(rendered)
    }

    /// 扫描 Pi native Skill 与兼容 skills。
    ///
    /// Business Logic: `{piConfigRoot,project/.pi}/skills` 为 native；`~/.agents/skills`
    ///     始终兼容发现；`~/.claude/skills` 仅当 settings 点名。不伪造 MCP。
    /// Code Logic: 委托 `scan_table_roots`（含 `piSettingsSkills` gate）。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        scan_table_roots(AgentTarget::Pi, scope, env, None, false)
    }

    /// Inventory 精确 kind 扫描。
    ///
    /// Business Logic: Skill 过滤走 manifest-only；gate 与全量扫描一致。
    /// Code Logic: `kind=None` 回退全量；Skill 传 `manifest_only=true`。
    fn scan_portable_assets_filtered(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
        kind: Option<AssetKind>,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let Some(kind) = kind else {
            return self.scan_portable_assets(scope, env);
        };
        scan_table_roots(
            AgentTarget::Pi,
            scope,
            env,
            Some(kind),
            kind == AssetKind::Skill,
        )
    }

    /// 渲染 Pi portable 投影。
    ///
    /// Business Logic: 计划路径落到 skills，由物化层放入 `.pi/`；不写盘。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Pi, asset)
    }
}

/// Pi 指令槽相对路径。
///
/// Business Logic（为什么需要这个函数）:
///     common 不单独物化；adapted/exclusive 必须落到 `.pi/cc-partner.*`。
///
/// Code Logic（这个函数做什么）:
///     Common → None；其余返回固定相对路径。
pub fn pi_instruction_rel_path(slot: PiInstructionSlot) -> Option<&'static str> {
    match slot {
        PiInstructionSlot::Common => None,
        PiInstructionSlot::Adapted => Some(".pi/cc-partner.adapted.md"),
        PiInstructionSlot::Exclusive => Some(".pi/cc-partner.exclusive.md"),
    }
}

/// 把 compiler 文件名改写到 `.pi/`，永不输出仓库根 `AGENTS.md`。
///
/// Business Logic（为什么需要这个函数）:
///     compiler 当前只给一个 file_name（多为 exclusive）；Hub 仍不得把 common 写成 AGENTS.md。
///
/// Code Logic（这个函数做什么）:
///     拒绝 `AGENTS.md`；缺省落到 `cc-partner.exclusive.md`；已含 `.pi/` 则原样返回。
fn pi_render_file_name(compiler_name: &str) -> String {
    let name = compiler_name.replace('\\', "/");
    if name == "AGENTS.md" || name.ends_with("/AGENTS.md") || name.is_empty() {
        return ".pi/cc-partner.exclusive.md".to_string();
    }
    if name.contains(".pi/") {
        return name;
    }
    let file = Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(PI_EXCLUSIVE_FILE);
    format!(".pi/{file}")
}

/// 扫描用户级 Pi 指令。
///
/// Business Logic: 只读 `~/.pi/agent` 内受管文件与原生文件；不扫 `~/.claude`。
/// Code Logic: cc-partner.* + AGENTS.md / CLAUDE.md / AGENTS.override.md / SYSTEM.md。
fn scan_user_instructions(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let root = &homes.pi.config_root;
    let mut sources = Vec::new();
    push_managed_files(&mut sources, root, scope)?;
    push_existing(
        &mut sources,
        root.join("AGENTS.md"),
        scope,
        InstructionSourceRole::NativePrimary,
    )?;
    push_existing(
        &mut sources,
        root.join("AGENTS.override.md"),
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
    for name in ["SYSTEM.md", "APPEND_SYSTEM.md"] {
        push_existing(
            &mut sources,
            root.join(name),
            scope,
            InstructionSourceRole::NativePrimary,
        )?;
    }
    Ok(sources)
}

/// 扫描项目/目录级 Pi 指令。
///
/// Business Logic: 受管文件在 `.pi/`；目录内 AGENTS/CLAUDE 只读，不把 common 写成新 AGENTS.md。
/// Code Logic: 扫 `.pi/cc-partner.*` + 当前目录原生文件。
fn scan_project_instructions(
    scope: &LocalScopeMapping,
) -> Result<Vec<InstructionSource>, AppError> {
    let mut sources = Vec::new();
    push_managed_files(&mut sources, &scope.absolute_path.join(".pi"), scope)?;
    push_existing(
        &mut sources,
        scope.absolute_path.join("AGENTS.md"),
        scope,
        InstructionSourceRole::NativePrimary,
    )?;
    push_existing(
        &mut sources,
        scope.absolute_path.join("AGENTS.override.md"),
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
    for name in ["SYSTEM.md", "APPEND_SYSTEM.md"] {
        push_existing(
            &mut sources,
            scope.absolute_path.join(".pi").join(name),
            scope,
            InstructionSourceRole::NativePrimary,
        )?;
    }
    Ok(sources)
}

/// 登记 Hub 受管 adapted/exclusive 文件（存在才列入）。
fn push_managed_files(
    sources: &mut Vec<InstructionSource>,
    dir: &Path,
    scope: &LocalScopeMapping,
) -> Result<(), AppError> {
    for name in [PI_ADAPTED_FILE, PI_EXCLUSIVE_FILE] {
        push_existing(
            sources,
            dir.join(name),
            scope,
            InstructionSourceRole::ManagedProjection,
        )?;
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
        target: AgentTarget::Pi,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::targets::portable::{PortableAssetOwner, PortableOriginKind};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn isolated_env(home: &Path) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
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
        let rendered = PiInstructionAdapter
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
            pi_instruction_rel_path(PiInstructionSlot::Common).is_none(),
            "common must not materialize a dedicated file"
        );
    }

    #[test]
    fn adapted_and_exclusive_paths_live_under_pi_dir() {
        let adapted = pi_instruction_rel_path(PiInstructionSlot::Adapted).expect("adapted");
        let exclusive = pi_instruction_rel_path(PiInstructionSlot::Exclusive).expect("exclusive");
        assert!(adapted.contains(".pi/cc-partner."), "adapted={adapted}");
        assert!(
            exclusive.contains(".pi/cc-partner."),
            "exclusive={exclusive}"
        );
        let rendered = PiInstructionAdapter
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
                .contains(".pi/cc-partner."),
            "render file_name={}",
            rendered.file_name
        );
    }

    #[test]
    fn scan_does_not_mark_claude_home_as_pi_native() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(&home.join(".claude/CLAUDE.md"), "claude user rules\n");
        write_text(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".pi/agent/cc-partner.exclusive.md"),
            "pi exclusive\n",
        );
        write_text(
            &home.join(".pi/agent/skills/pi-skill/SKILL.md"),
            "---\nname: pi-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".pi/agent/settings.json"),
            r#"{"skills": [".claude/skills"]}"#,
        );
        write_text(
            &home.join(".agents/skills/shared-skill/SKILL.md"),
            "---\nname: shared-skill\ndescription: d\n---\nbody\n",
        );
        let env = isolated_env(home);
        let scope = user_scope(home);

        let sources = PiInstructionAdapter
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

        let found = PiInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(
            found.iter().all(|asset| {
                let path = asset.origin.path.to_string_lossy().replace('\\', "/");
                !path.contains("/.claude/")
                    || asset.origin.origin_kind != PortableOriginKind::Native
            }),
            "portable scan marked ~/.claude as Pi native: {found:?}"
        );
        let claude = found
            .iter()
            .find(|asset| {
                asset
                    .origin
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .contains("/.claude/")
            })
            .expect("claude compatibility skill when settings list it");
        assert_eq!(claude.semantic_name, "review");
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
        assert!(found.iter().any(|asset| {
            asset.semantic_name == "pi-skill"
                && asset.origin.origin_kind == PortableOriginKind::Native
        }));
        assert!(found
            .iter()
            .all(|asset| asset.origin.target == AgentTarget::Pi));
    }

    #[test]
    fn user_scan_skips_claude_skills_unless_settings_list_them() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".agents/skills/shared-skill/SKILL.md"),
            "---\nname: shared-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".pi/agent/skills/pi-skill/SKILL.md"),
            "---\nname: pi-skill\ndescription: d\n---\nbody\n",
        );
        let env = isolated_env(home);
        let scope = user_scope(home);

        let without_settings = PiInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(
            without_settings.iter().all(|asset| {
                !asset
                    .origin
                    .path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .contains("/.claude/")
            }),
            "claude skills must stay hidden without settings: {without_settings:?}"
        );
        assert!(without_settings
            .iter()
            .any(|asset| asset.semantic_name == "shared-skill"
                && asset.origin.origin_kind == PortableOriginKind::Compatibility
                && asset.origin.owned_by == PortableAssetOwner::SharedAgents
                && !asset.origin.native_output_candidate));
        assert!(without_settings
            .iter()
            .any(|asset| asset.semantic_name == "pi-skill"));

        write_text(
            &home.join(".pi/agent/settings.json"),
            r#"{"skills": [".claude/skills"]}"#,
        );
        let with_settings = PiInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        let claude = with_settings
            .iter()
            .find(|asset| asset.semantic_name == "review")
            .expect("claude skill after settings mention");
        assert_eq!(claude.origin.origin_kind, PortableOriginKind::Compatibility);
        assert!(!claude.origin.native_output_candidate);
        assert_eq!(claude.origin.owned_by, PortableAssetOwner::Claude);
    }
}
