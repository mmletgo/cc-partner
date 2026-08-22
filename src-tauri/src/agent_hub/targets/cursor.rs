//! agent_hub/targets/cursor — Cursor CLI instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Cursor CLI（可执行 `agent`）同时加载仓库 `AGENTS.md`/`CLAUDE.md` 与
//!     `.cursor/rules/*.mdc`；公共槽不得再写一份会与 Codex/OpenCode 抢 `AGENTS.md`
//!     的文件。专属语义只落到 `.cursor/rules/cc-partner.adapted.mdc` /
//!     `cc-partner.exclusive.mdc`（必须带 alwaysApply frontmatter，纯 `.md` 会被忽略）。
//!     Claude 兼容目录 `~/.claude` / `.claude` 禁止当作 Cursor native 输出。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：按 support-manifest `commandNames` 顺序 probe
//!     `cursor-agent` 再 `agent`；扫描 `.mdc` rules 与只读原生指令；portable
//!     经 runtime-discovery 表扫 `.cursor` native 与 Claude/Codex/`.agents`
//!     兼容 skills；render 落到 `.cursor/rules/` 并包裹 YAML frontmatter。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version_in_env, resolve_first_executable, TargetPathResolver,
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
use crate::agent_hub::support::{builtin_support_manifest, find_target_record, scan_table_roots};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// Cursor 受管 adapted 文件名（`.mdc` 才能被 rules 系统加载）。
pub(crate) const CURSOR_ADAPTED_FILE: &str = "cc-partner.adapted.mdc";
/// Cursor 受管 exclusive 文件名。
pub(crate) const CURSOR_EXCLUSIVE_FILE: &str = "cc-partner.exclusive.mdc";
/// Cursor CLI 探测命令名回退顺序（与 support-manifest `commandNames` 对齐）。
const CURSOR_COMMAND_NAMES_FALLBACK: &[&str] = &["cursor-agent", "agent"];
/// `.mdc` alwaysApply frontmatter 描述（静态，禁止插入用户正文以免 YAML 注入）。
const CURSOR_MDC_DESCRIPTION: &str = "cc-partner Cursor CLI instructions";

/// Cursor 指令槽（公共不物化，专属写入 rules）。
///
/// Business Logic（为什么需要这个枚举）:
///     render / 单测必须按槽选择落点，禁止把 common 写成仓库根 `AGENTS.md`。
///
/// Code Logic（这个枚举做什么）:
///     `Common` / `Adapted` / `Exclusive` 三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorInstructionSlot {
    /// 公共槽：不单独物化
    Common,
    /// 适配槽：`.cursor/rules/cc-partner.adapted.mdc`
    Adapted,
    /// 独有槽：`.cursor/rules/cc-partner.exclusive.mdc`
    Exclusive,
}

/// Cursor CLI 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service / inventory / portable scanner 通过统一 `AssetAdapter` 调用 Cursor 路径语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct CursorInstructionAdapter;

impl AssetAdapter for CursorInstructionAdapter {
    /// 返回 Cursor 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Cursor`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Cursor
    }

    /// 探测 Cursor CLI 可执行文件、版本与配置根。
    ///
    /// Business Logic: 官方 CLI 现为 `cursor-agent`；旧名 `agent` 可能指向 Grok
    ///     symlink，必须后试。版本未知只能 scan-only；`CURSOR_HOME` 覆盖默认 `~/.cursor`。
    /// Code Logic: `resolve_all` + `cursor_command_names` + `resolve_first_executable`
    ///     + `probe_cli_version` + `build_probe`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let names = cursor_command_names();
        let executable = resolve_first_executable(names.iter().map(String::as_str), env);
        let version = executable
            .as_ref()
            .and_then(|p| probe_cli_version_in_env(p, env));
        Ok(build_probe(
            AgentTarget::Cursor,
            executable,
            version,
            homes.cursor.config_root,
        ))
    }

    /// 扫描 Cursor 指令源。
    ///
    /// Business Logic: 用户级扫 `config_root/rules/*.mdc`；项目级扫 `<root>/.cursor/rules/`
    ///     与目录内只读 `AGENTS.md`/`CLAUDE.md`/`.cursorrules`；缺失不报错。
    /// Code Logic: 受管文件标 ManagedProjection；不把 `~/.claude` 列入 Cursor 源。
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

    /// 渲染 Cursor 受管指令。
    ///
    /// Business Logic: common 不得物化 `AGENTS.md`；compiler 只给一个 file_name 时落到
    ///     `.cursor/rules/` 下该 `.mdc`，并包裹 alwaysApply frontmatter。
    /// Code Logic: `compile_render` 后改写 file_name 并同步 bytes。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Cursor,
            context,
        );
        let mut rendered = RenderedInstruction::from_compiled(compiled);
        rendered.file_name = cursor_render_file_name(&rendered.file_name);
        rendered.content = wrap_cursor_mdc(&rendered.content);
        rendered.bytes = rendered.content.as_bytes().to_vec();
        Ok(rendered)
    }

    /// 扫描 Cursor native Skill/Command/MCP 与兼容 skills。
    ///
    /// Business Logic: `.cursor` 为 native；`~/.claude|/.codex|/.agents/skills` 为兼容根，
    ///     不得成为 Cursor native 写出目标。
    /// Code Logic: 委托 `scan_table_roots`，由发现表 stamp owned_by / origin_kind。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        scan_table_roots(AgentTarget::Cursor, scope, env, None, false)
    }

    /// Inventory 精确 kind 扫描。
    ///
    /// Business Logic: Skill 过滤走 manifest-only，避免全树 hash；兼容根仍按表发现。
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
            AgentTarget::Cursor,
            scope,
            env,
            Some(kind),
            kind == AssetKind::Skill,
        )
    }

    /// 渲染 Cursor portable 投影。
    ///
    /// Business Logic: 计划路径落到 skills/commands，由物化层放入 `.cursor/`；不写盘。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Cursor, asset)
    }
}

/// 读取 Cursor 探测命令名（与 support-manifest `commandNames` 同序）。
///
/// Business Logic（为什么需要这个函数）:
///     probe 必须与编译期 support 合同共用同一优先序，避免硬编码漂移到 Grok 的 `agent`。
///
/// Code Logic（这个函数做什么）:
///     解析内置 manifest 的 Cursor `commandNames`；空或缺记录时回退
///     `cursor-agent` → `agent`。
fn cursor_command_names() -> Vec<String> {
    builtin_support_manifest()
        .ok()
        .and_then(|manifest| {
            find_target_record(&manifest, AgentTarget::Cursor)
                .map(|record| record.executable_probe.command_names.clone())
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| {
            CURSOR_COMMAND_NAMES_FALLBACK
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        })
}

/// Cursor 指令槽相对路径。
///
/// Business Logic（为什么需要这个函数）:
///     common 不单独物化；adapted/exclusive 必须落到 `.cursor/rules/cc-partner.*`。
///
/// Code Logic（这个函数做什么）:
///     Common → None；其余返回固定相对路径。
pub fn cursor_instruction_rel_path(slot: CursorInstructionSlot) -> Option<&'static str> {
    match slot {
        CursorInstructionSlot::Common => None,
        CursorInstructionSlot::Adapted => Some(".cursor/rules/cc-partner.adapted.mdc"),
        CursorInstructionSlot::Exclusive => Some(".cursor/rules/cc-partner.exclusive.mdc"),
    }
}

/// 把 compiler 文件名改写到 `.cursor/rules/`，永不输出仓库根 `AGENTS.md`。
///
/// Business Logic（为什么需要这个函数）:
///     compiler 当前只给一个 file_name（多为 exclusive）；Hub 仍不得把 common 写成 AGENTS.md。
///
/// Code Logic（这个函数做什么）:
///     拒绝 `AGENTS.md`；缺省落到 `cc-partner.exclusive.mdc`；已含 `.cursor/rules/` 则原样返回。
fn cursor_render_file_name(compiler_name: &str) -> String {
    let name = compiler_name.replace('\\', "/");
    if name == "AGENTS.md" || name.ends_with("/AGENTS.md") || name.is_empty() {
        return ".cursor/rules/cc-partner.exclusive.mdc".to_string();
    }
    if name.contains(".cursor/rules/") {
        return ensure_mdc_extension(&name);
    }
    let file = Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(CURSOR_EXCLUSIVE_FILE);
    format!(".cursor/rules/{}", ensure_mdc_extension(file))
}

/// 确保 rules 文件使用 `.mdc` 扩展名。
fn ensure_mdc_extension(name: &str) -> String {
    if name.ends_with(".mdc") {
        name.to_string()
    } else if let Some(stem) = name.strip_suffix(".md") {
        format!("{stem}.mdc")
    } else {
        format!("{name}.mdc")
    }
}

/// 为 Cursor rules 包裹 alwaysApply YAML frontmatter。
///
/// Business Logic（为什么需要这个函数）:
///     `.cursor/rules` 下没有 frontmatter 的文件会被忽略；Hub 受管投影必须始终生效。
///
/// Code Logic（这个函数做什么）:
///     已有 `---` 开头则原样返回，避免双包。
fn wrap_cursor_mdc(body: &str) -> String {
    let trimmed = body.trim_start();
    if trimmed.starts_with("---") {
        return body.to_string();
    }
    format!("---\ndescription: {CURSOR_MDC_DESCRIPTION}\nalwaysApply: true\n---\n\n{body}")
}

/// 扫描用户级 Cursor 指令。
///
/// Business Logic: 只读 cursor home 内 rules；不扫 `~/.claude`。
/// Code Logic: rules/*.mdc。
fn scan_user_instructions(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let root = &homes.cursor.config_root;
    let mut sources = Vec::new();
    push_rules_dir(&mut sources, &root.join("rules"), scope)?;
    Ok(sources)
}

/// 扫描项目/目录级 Cursor 指令。
///
/// Business Logic: 受管文件在 `.cursor/rules/`；目录内 AGENTS/CLAUDE/.cursorrules 只读，
///     不把 common 写成新 AGENTS.md。
/// Code Logic: 扫 rules 目录 + 当前目录原生文件。
fn scan_project_instructions(
    scope: &LocalScopeMapping,
) -> Result<Vec<InstructionSource>, AppError> {
    let mut sources = Vec::new();
    push_rules_dir(
        &mut sources,
        &scope.absolute_path.join(".cursor").join("rules"),
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
    push_existing(
        &mut sources,
        scope.absolute_path.join(".cursorrules"),
        scope,
        InstructionSourceRole::Fallback,
    )?;
    Ok(sources)
}

/// 扫描 `.cursor/rules/*.mdc`。
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
                path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("mdc")
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
        let role = if name == CURSOR_ADAPTED_FILE || name == CURSOR_EXCLUSIVE_FILE {
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
        target: AgentTarget::Cursor,
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

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn isolated_env(home: &Path) -> TargetEnvironment {
        let cursor = home.join(".cursor");
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::from([("CURSOR_HOME".into(), cursor.to_string_lossy().into_owned())]),
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
        let rendered = CursorInstructionAdapter
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
            cursor_instruction_rel_path(CursorInstructionSlot::Common).is_none(),
            "common must not materialize a dedicated file"
        );
        assert!(rendered.content.contains("alwaysApply: true"));
        assert!(rendered.content.starts_with("---\n"));
    }

    #[test]
    fn adapted_and_exclusive_paths_live_under_cursor_rules() {
        let adapted = cursor_instruction_rel_path(CursorInstructionSlot::Adapted).expect("adapted");
        let exclusive =
            cursor_instruction_rel_path(CursorInstructionSlot::Exclusive).expect("exclusive");
        assert!(
            adapted.contains(".cursor/rules/cc-partner."),
            "adapted={adapted}"
        );
        assert!(
            exclusive.contains(".cursor/rules/cc-partner."),
            "exclusive={exclusive}"
        );
        assert!(adapted.ends_with(".mdc"));
        assert!(exclusive.ends_with(".mdc"));
        let rendered = CursorInstructionAdapter
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
                .contains(".cursor/rules/cc-partner."),
            "render file_name={}",
            rendered.file_name
        );
        assert!(rendered.file_name.ends_with(".mdc"));
    }

    #[test]
    fn scan_does_not_mark_claude_home_as_cursor_native() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(&home.join(".claude/CLAUDE.md"), "claude user rules\n");
        write_text(
            &home.join(".claude/skills/review/SKILL.md"),
            "---\nname: review\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".cursor/rules/cc-partner.exclusive.mdc"),
            "---\ndescription: exclusive\nalwaysApply: true\n---\ncursor exclusive\n",
        );
        write_text(
            &home.join(".cursor/skills/cursor-skill/SKILL.md"),
            "---\nname: cursor-skill\ndescription: d\n---\nbody\n",
        );
        let env = isolated_env(home);
        let scope = user_scope(home);

        let sources = CursorInstructionAdapter
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
                    .contains("cc-partner.exclusive.mdc")
        }));

        let found = CursorInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();
        assert!(
            found.iter().all(|asset| {
                let path = asset.origin.path.to_string_lossy().replace('\\', "/");
                !path.contains("/.claude/")
                    || asset.origin.origin_kind != PortableOriginKind::Native
            }),
            "portable scan marked ~/.claude as Cursor native: {found:?}"
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
            .expect("claude compatibility skill");
        assert_eq!(claude.semantic_name, "review");
        assert_eq!(claude.origin.origin_kind, PortableOriginKind::Compatibility);
        assert!(!claude.origin.native_output_candidate);
        assert_eq!(claude.origin.owned_by, PortableAssetOwner::Claude);
        assert!(found.iter().any(|asset| {
            asset.semantic_name == "cursor-skill"
                && asset.origin.origin_kind == PortableOriginKind::Native
                && asset.origin.native_output_candidate
        }));
        assert!(found
            .iter()
            .all(|asset| asset.origin.target == AgentTarget::Cursor));
    }

    #[test]
    fn user_scan_finds_native_cursor_and_compatibility_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(
            &home.join(".cursor/skills/cursor-skill/SKILL.md"),
            "---\nname: cursor-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".claude/skills/claude-skill/SKILL.md"),
            "---\nname: claude-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".agents/skills/shared-skill/SKILL.md"),
            "---\nname: shared-skill\ndescription: d\n---\nbody\n",
        );
        let env = isolated_env(home);
        let scope = user_scope(home);
        let found = CursorInstructionAdapter
            .scan_portable_assets(&scope, &env)
            .unwrap();

        let native = found
            .iter()
            .find(|asset| asset.semantic_name == "cursor-skill")
            .expect("native cursor skill");
        assert_eq!(native.origin.origin_kind, PortableOriginKind::Native);
        assert!(native.origin.native_output_candidate);
        assert_eq!(native.origin.owned_by, PortableAssetOwner::Cursor);
        assert!(native
            .origin
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .contains("/.cursor/skills/"));

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
    }

    #[test]
    fn project_scan_reads_agents_md_but_render_never_writes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let project = home.join("proj");
        write_text(&project.join("AGENTS.md"), "shared agents\n");
        write_text(
            &project.join(".cursor/rules/cc-partner.adapted.mdc"),
            "---\ndescription: adapted\nalwaysApply: true\n---\nadapted\n",
        );
        let env = isolated_env(home);
        let project_scope = LocalScopeMapping {
            scope_kind: ScopeKind::Project,
            absolute_path: project.clone(),
            project_root: Some(project),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec![],
        };
        let sources = CursorInstructionAdapter
            .scan_instruction_sources(&project_scope, &env)
            .unwrap();
        assert!(sources.iter().any(|s| {
            s.role == InstructionSourceRole::NativePrimary
                && s.path.file_name().and_then(|n| n.to_str()) == Some("AGENTS.md")
        }));
        let rendered = CursorInstructionAdapter
            .render_instruction(
                &InstructionDocument {
                    common_markdown: "x".into(),
                    relative_key: String::new(),
                },
                &InstructionRenderContext::default(),
            )
            .unwrap();
        assert!(!rendered.file_name.contains("AGENTS.md"));
    }

    #[test]
    fn command_names_match_support_manifest_and_prefer_cursor_agent() {
        let names = cursor_command_names();
        assert_eq!(names, ["cursor-agent", "agent"]);
        let manifest = crate::agent_hub::support::builtin_support_manifest().unwrap();
        let record = crate::agent_hub::support::find_target_record(&manifest, AgentTarget::Cursor)
            .expect("cursor support record");
        assert_eq!(record.executable_probe.command_names, names);
    }

    #[cfg(unix)]
    #[test]
    fn probe_prefers_cursor_agent_over_agent_symlink() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let bin = home.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let write_cli = |name: &str, stamp: &str| {
            let path = bin.join(name);
            fs::write(&path, format!("#!/bin/sh\necho '{stamp}'\n")).unwrap();
            let mut perms = fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).unwrap();
        };
        write_cli("cursor-agent", "cursor-agent 1.2.3");
        write_cli("agent", "grok-agent 9.9.9");
        let mut env = isolated_env(home);
        env.path_entries = vec![bin];
        let probe = CursorInstructionAdapter.probe(&env).unwrap();
        let exe = probe.executable.expect("cursor executable");
        assert!(
            exe.file_name().and_then(|s| s.to_str()) == Some("cursor-agent"),
            "probe must prefer cursor-agent, got {exe:?}"
        );
        assert!(
            probe
                .version
                .as_deref()
                .is_some_and(|v| v.contains("cursor-agent")),
            "version={:?}",
            probe.version
        );
    }
}
