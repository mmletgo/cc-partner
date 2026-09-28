//! agent_hub/targets/zcode — ZCode CLI instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     ZCode 只注入用户全局 `<config_root>/AGENTS.md` 与仓库根 `AGENTS.md`。
//!     公共槽必须复用已有仓库根文件，适配器不得再写一份，也不得把正文追加进去。
//!     项目适配/独有槽在 Hub 可见，但渲染不得指向仓库根 `AGENTS.md` 或 `~/.claude`。
//!     不扫描 `~/.claude` / `~/.codex`，也不把 Claude marketplace 当成 ZCode 会加载的包。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe 只认可执行文件 `zcode`；配置根跟 `ZCODE_DATA_BASE_DIR`；
//!     portable 经 runtime-discovery 扫 native skills/commands 与 `mcp.servers`，
//!     skill 借用只认 `.agents/skills`。render 落到 `.zcode/`，renderInstruction 仍 blocked。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version_in_env, resolve_executable, TargetPathResolver,
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

/// ZCode 受管 adapted 文件名（项目 `.zcode/` 下，ZCode 本身不加载）。
pub(crate) const ZCODE_ADAPTED_FILE: &str = "cc-partner.adapted.md";
/// ZCode 受管 exclusive 文件名。
pub(crate) const ZCODE_EXCLUSIVE_FILE: &str = "cc-partner.exclusive.md";

/// ZCode 指令槽。
///
/// Business Logic（为什么需要这个枚举）:
///     公共槽不物化；适配/独有必须落到 `.zcode/`，禁止落到仓库根 `AGENTS.md`。
///
/// Code Logic（这个枚举做什么）:
///     `Common` / `Adapted` / `Exclusive` 三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZcodeInstructionSlot {
    /// 公共槽：复用仓库根 `AGENTS.md`，不另写
    Common,
    /// 适配槽：`.zcode/cc-partner.adapted.md`
    Adapted,
    /// 独有槽：`.zcode/cc-partner.exclusive.md`
    Exclusive,
}

/// ZCode 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service / inventory / portable scanner 通过统一 `AssetAdapter` 调用 ZCode 路径语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct ZcodeInstructionAdapter;

impl AssetAdapter for ZcodeInstructionAdapter {
    /// 返回 ZCode 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Zcode`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Zcode
    }

    /// 探测 `zcode` 可执行文件、版本与配置根。
    ///
    /// Business Logic: 只认 CLI 名 `zcode`，不启动 `ZCode.app`；配置根不认 `ZCODE_HOME`。
    /// Code Logic: `resolve_all` + `resolve_executable("zcode")` + `probe_cli_version`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("zcode", env);
        let version = executable
            .as_ref()
            .and_then(|p| probe_cli_version_in_env(p, env));
        Ok(build_probe(
            AgentTarget::Zcode,
            executable,
            version,
            homes.zcode.config_root,
        ))
    }

    /// 扫描 ZCode 指令源。
    ///
    /// Business Logic: 用户级只认 `<config_root>/AGENTS.md`；项目级只登记 `.zcode/` 受管草稿。
    ///     不把仓库根 `AGENTS.md` 列成可写源，也不扫 `~/.claude`。
    /// Code Logic: 缺失不报错；受管文件标 ManagedProjection。
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

    /// 渲染 ZCode 受管指令。
    ///
    /// Business Logic: common 不得物化仓库根 `AGENTS.md`；落点必须在 `.zcode/`，不得进 `~/.claude`。
    /// Code Logic: `compile_render` 后把 file_name 改写为 `.zcode/<name>`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Zcode,
            context,
        );
        let mut rendered = RenderedInstruction::from_compiled(compiled);
        rendered.file_name = zcode_render_file_name(&rendered.file_name);
        Ok(rendered)
    }

    /// 扫描 ZCode native 与借用资产。
    ///
    /// Business Logic: skill/command/mcp 根以 runtime-discovery 为准；不扫 Claude/Codex skills。
    /// Code Logic: 委托 `scan_table_roots`。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        scan_table_roots(AgentTarget::Zcode, scope, env, None, false)
    }

    /// Inventory 精确 kind 扫描。
    ///
    /// Business Logic: Skill 过滤走 manifest-only，避免把插件内部技能展开进独立清单。
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
            AgentTarget::Zcode,
            scope,
            env,
            Some(kind),
            kind == AssetKind::Skill,
        )
    }

    /// 渲染 ZCode portable 投影。
    ///
    /// Business Logic: 计划路径由物化层放入 native 根；本函数不写盘。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Zcode, asset)
    }
}

/// ZCode 指令槽相对路径。
///
/// Business Logic（为什么需要这个函数）:
///     common 不单独物化；adapted/exclusive 必须落到 `.zcode/cc-partner.*`。
///
/// Code Logic（这个函数做什么）:
///     Common → None；其余返回固定相对路径。
pub fn zcode_instruction_rel_path(slot: ZcodeInstructionSlot) -> Option<&'static str> {
    match slot {
        ZcodeInstructionSlot::Common => None,
        ZcodeInstructionSlot::Adapted => Some(".zcode/cc-partner.adapted.md"),
        ZcodeInstructionSlot::Exclusive => Some(".zcode/cc-partner.exclusive.md"),
    }
}

/// 把 compiler 文件名改写到 `.zcode/`，永不输出仓库根 `AGENTS.md` 或 `~/.claude`。
///
/// Business Logic（为什么需要这个函数）:
///     compiler 当前只给一个 file_name；Hub 仍不得把 common 写成 AGENTS.md，也不得写进 Claude 目录。
///
/// Code Logic（这个函数做什么）:
///     拒绝 `AGENTS.md` 与任何 `.claude` 路径；缺省落到 `cc-partner.exclusive.md`；已含 `.zcode/` 则原样返回。
pub(crate) fn zcode_render_file_name(compiler_name: &str) -> String {
    let name = compiler_name.replace('\\', "/");
    if name == "AGENTS.md"
        || name.ends_with("/AGENTS.md")
        || name.is_empty()
        || name.contains("/.claude/")
        || name.starts_with(".claude/")
        || name.contains("/.claude")
    {
        return ".zcode/cc-partner.exclusive.md".to_string();
    }
    if name.contains(".zcode/") {
        return name;
    }
    let file = Path::new(&name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|file| *file != "AGENTS.md")
        .unwrap_or(ZCODE_EXCLUSIVE_FILE);
    format!(".zcode/{file}")
}

/// 扫描用户级 ZCode 指令。
///
/// Business Logic: 只读 `<config_root>/AGENTS.md` 与 `.zcode` 受管草稿；不扫 `~/.claude`，不读 `cli/config.json`。
/// Code Logic: 存在才登记。
fn scan_user_instructions(
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<InstructionSource>, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let root = &homes.zcode.config_root;
    let mut sources = Vec::new();
    push_existing(
        &mut sources,
        root.join("AGENTS.md"),
        scope,
        InstructionSourceRole::NativePrimary,
    )?;
    push_managed_files(&mut sources, root, scope)?;
    Ok(sources)
}

/// 扫描项目/目录级 ZCode 指令。
///
/// Business Logic: 仓库根 `AGENTS.md` 由公共槽编辑器负责，适配器扫描不得把它当成可写源。
///     适配/独有草稿只出现在 `.zcode/`。
/// Code Logic: 只登记 `.zcode/cc-partner.*`。
fn scan_project_instructions(
    scope: &LocalScopeMapping,
) -> Result<Vec<InstructionSource>, AppError> {
    let mut sources = Vec::new();
    push_managed_files(&mut sources, &scope.absolute_path.join(".zcode"), scope)?;
    Ok(sources)
}

/// 登记 Hub 受管 adapted/exclusive 文件（存在才列入）。
fn push_managed_files(
    sources: &mut Vec<InstructionSource>,
    dir: &Path,
    scope: &LocalScopeMapping,
) -> Result<(), AppError> {
    for name in [ZCODE_ADAPTED_FILE, ZCODE_EXCLUSIVE_FILE] {
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
        target: AgentTarget::Zcode,
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

    fn env_with(home: &Path, vars: BTreeMap<String, String>) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars,
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

    fn project_scope(root: &Path) -> LocalScopeMapping {
        LocalScopeMapping {
            scope_kind: ScopeKind::Project,
            absolute_path: root.to_path_buf(),
            project_root: Some(root.to_path_buf()),
            relative_root: None,
            codex_fallback_filenames: vec![],
        }
    }

    #[test]
    fn config_root_uses_data_base_dir_and_ignores_zcode_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let base = tmp.path().join("base");
        let mut vars = BTreeMap::new();
        vars.insert(
            "ZCODE_DATA_BASE_DIR".into(),
            base.to_string_lossy().into_owned(),
        );
        vars.insert(
            "ZCODE_HOME".into(),
            tmp.path().join("not-zcode").to_string_lossy().into_owned(),
        );
        let env = env_with(&home, vars);
        let probe = ZcodeInstructionAdapter.probe(&env).unwrap();
        assert_eq!(probe.config_root, base.join(".zcode"));
        let homes = TargetPathResolver::resolve_all(&env);
        assert_eq!(
            homes.default_user_instruction_path(AgentTarget::Zcode),
            base.join(".zcode").join("AGENTS.md")
        );
        assert!(!probe.config_root.starts_with(tmp.path().join("not-zcode")));
    }

    #[test]
    fn blank_data_base_dir_falls_back_to_home_dot_zcode() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let mut vars = BTreeMap::new();
        vars.insert("ZCODE_DATA_BASE_DIR".into(), "  ".into());
        vars.insert("ZCODE_HOME".into(), "/tmp/should-not-be-used".into());
        let env = env_with(&home, vars);
        let probe = ZcodeInstructionAdapter.probe(&env).unwrap();
        assert_eq!(probe.config_root, home.join(".zcode"));
    }

    #[test]
    fn render_never_emits_repo_agents_md_or_claude_home() {
        let rendered = ZcodeInstructionAdapter
            .render_instruction(
                &InstructionDocument {
                    common_markdown: "# shared rules\n".into(),
                    relative_key: String::new(),
                },
                &InstructionRenderContext::default(),
            )
            .unwrap();
        let name = rendered.file_name.replace('\\', "/");
        assert_ne!(name, "AGENTS.md");
        assert!(!name.ends_with("/AGENTS.md"));
        assert!(name.starts_with(".zcode/"), "render file_name={name}");
        assert!(!name.contains(".claude"));
        assert!(zcode_instruction_rel_path(ZcodeInstructionSlot::Common).is_none());
        assert_eq!(
            zcode_render_file_name("AGENTS.md"),
            ".zcode/cc-partner.exclusive.md"
        );
        assert_eq!(
            zcode_render_file_name(".claude/CLAUDE.md"),
            ".zcode/cc-partner.exclusive.md"
        );
        assert_eq!(
            zcode_render_file_name("/Users/someone/.claude/CLAUDE.md"),
            ".zcode/cc-partner.exclusive.md"
        );
    }

    #[test]
    fn adapted_and_exclusive_paths_live_under_dot_zcode() {
        let adapted = zcode_instruction_rel_path(ZcodeInstructionSlot::Adapted).unwrap();
        let exclusive = zcode_instruction_rel_path(ZcodeInstructionSlot::Exclusive).unwrap();
        assert_eq!(adapted, ".zcode/cc-partner.adapted.md");
        assert_eq!(exclusive, ".zcode/cc-partner.exclusive.md");
        assert!(!adapted.ends_with("AGENTS.md"));
    }

    #[test]
    fn user_scan_reads_agents_md_and_skips_claude_and_provider_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_text(&home.join(".zcode/AGENTS.md"), "user rules\n");
        write_text(&home.join(".claude/CLAUDE.md"), "claude rules\n");
        write_text(
            &home.join(".zcode/cli/config.json"),
            r#"{"provider":"example","model":"demo"}"#,
        );
        write_text(
            &home.join(".zcode/v2/provider.json"),
            r#"{"key":"not-a-real-secret"}"#,
        );
        let env = env_with(&home, BTreeMap::new());
        let sources = ZcodeInstructionAdapter
            .scan_instruction_sources(&user_scope(&home), &env)
            .unwrap();
        let paths: Vec<_> = sources
            .iter()
            .map(|source| source.path.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(paths.iter().any(|path| path.ends_with("/.zcode/AGENTS.md")));
        assert!(paths.iter().all(|path| !path.contains("/.claude/")));
        assert!(paths.iter().all(|path| !path.contains("/cli/config.json")));
        assert!(paths.iter().all(|path| !path.contains("/v2/")));
    }

    #[test]
    fn project_scan_does_not_claim_repo_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("repo");
        write_text(&project.join("AGENTS.md"), "shared repo\n");
        write_text(&project.join("CLAUDE.md"), "claude repo\n");
        write_text(&project.join(".zcode/cc-partner.exclusive.md"), "draft\n");
        let env = env_with(tmp.path(), BTreeMap::new());
        let sources = ZcodeInstructionAdapter
            .scan_instruction_sources(&project_scope(&project), &env)
            .unwrap();
        assert_eq!(sources.len(), 1);
        let path = sources[0].path.to_string_lossy().replace('\\', "/");
        assert!(path.ends_with("/.zcode/cc-partner.exclusive.md"));
        assert_ne!(
            sources[0].path.file_name().and_then(|name| name.to_str()),
            Some("AGENTS.md")
        );
    }

    #[test]
    fn portable_scan_borrows_agents_skills_not_claude_or_codex() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_text(
            &home.join(".zcode/skills/native/SKILL.md"),
            "---\nname: native-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared-skill\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".claude/skills/claude-only/SKILL.md"),
            "---\nname: claude-only\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".codex/skills/codex-only/SKILL.md"),
            "---\nname: codex-only\ndescription: d\n---\nbody\n",
        );
        write_text(
            &home.join(".agents/commands/nope.md"),
            "not a zcode command\n",
        );
        write_text(&home.join(".zcode/commands/ship.md"), "ship\n");
        let env = env_with(&home, BTreeMap::new());
        let found = ZcodeInstructionAdapter
            .scan_portable_assets(&user_scope(&home), &env)
            .unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|item| item.semantic_name.as_str())
            .collect();
        assert!(names.contains(&"native-skill"), "{names:?}");
        assert!(names.contains(&"shared-skill"), "{names:?}");
        assert!(names.contains(&"ship"), "{names:?}");
        assert!(!names.contains(&"claude-only"), "{names:?}");
        assert!(!names.contains(&"codex-only"), "{names:?}");
        assert!(!names.contains(&"nope"), "{names:?}");
        let shared = found
            .iter()
            .find(|item| item.semantic_name == "shared-skill")
            .unwrap();
        assert_eq!(shared.origin.origin_kind, PortableOriginKind::Compatibility);
        assert_eq!(shared.origin.owned_by, PortableAssetOwner::SharedAgents);
        let native = found
            .iter()
            .find(|item| item.semantic_name == "native-skill")
            .unwrap();
        assert_eq!(native.origin.origin_kind, PortableOriginKind::Native);
        assert_eq!(native.origin.owned_by, PortableAssetOwner::Zcode);
    }

    #[test]
    fn mcp_borrow_hidden_when_native_servers_nonempty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_text(
            &home.join(".zcode/cli/config.json"),
            r#"{"provider":"example","model":"demo","mcp":{"servers":{"native":{"command":"echo","args":["ok"]}}},"skills":{}}"#,
        );
        write_text(
            &home.join(".agents/mcp.json"),
            r#"{"mcpServers":{"borrowed":{"command":"echo","args":["no"]}}}"#,
        );
        let env = env_with(&home, BTreeMap::new());
        let found = ZcodeInstructionAdapter
            .scan_portable_assets(&user_scope(&home), &env)
            .unwrap();
        let mcp: Vec<_> = found
            .iter()
            .filter(|item| item.kind == AssetKind::Mcp)
            .map(|item| item.origin.native_id.as_str())
            .collect();
        assert_eq!(mcp, vec!["native"]);
    }

    #[test]
    fn mcp_borrow_appears_when_native_servers_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_text(
            &home.join(".zcode/cli/config.json"),
            r#"{"provider":"example","mcp":{"servers":{}}}"#,
        );
        write_text(
            &home.join(".agents/mcp.json"),
            r#"{"mcpServers":{"borrowed":{"command":"echo","args":["ok"]}}}"#,
        );
        let env = env_with(&home, BTreeMap::new());
        let found = ZcodeInstructionAdapter
            .scan_portable_assets(&user_scope(&home), &env)
            .unwrap();
        let borrowed = found
            .iter()
            .find(|item| item.kind == AssetKind::Mcp && item.origin.native_id == "borrowed")
            .expect("borrowed mcp");
        assert_eq!(
            borrowed.origin.origin_kind,
            PortableOriginKind::Compatibility
        );
        assert_eq!(borrowed.origin.owned_by, PortableAssetOwner::SharedAgents);
    }
}
