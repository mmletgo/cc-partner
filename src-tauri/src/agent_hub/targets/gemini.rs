//! agent_hub/targets/gemini — Gemini CLI instruction + portable-asset adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Gemini 不读 `AGENTS.md`；公共指令入口是 `GEMINI.md`。adapted/exclusive 只用侧车
//!     `.gemini/cc-partner.adapted.md` / `cc-partner.exclusive.md`，禁止与同文件分块双写。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `gemini`；扫描 GEMINI.md + 侧车；portable 扫
//!     `.gemini/skills|commands` 与 `settings.json` 的 `mcpServers`。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::portable::{
    merge_discoveries, parse_json_or_jsonc, parse_mcp_servers_json_map, render_portable_payload,
    scan_command_markdown_dir, scan_skill_dirs, scan_skill_dirs_manifest_only, AssetRenderContext,
    DiscoveredPortableAsset, PortableOriginKind, TargetAssetProjection,
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

/// Gemini 公共指令文件名。
pub(crate) const GEMINI_COMMON_FILE: &str = "GEMINI.md";
/// Gemini 受管 adapted 文件名。
pub(crate) const GEMINI_ADAPTED_FILE: &str = "cc-partner.adapted.md";
/// Gemini 受管 exclusive 文件名。
pub(crate) const GEMINI_EXCLUSIVE_FILE: &str = "cc-partner.exclusive.md";

/// Gemini 指令槽落点。
///
/// Business Logic（为什么需要这个枚举）:
///     common 写 `GEMINI.md`；adapted/exclusive 只允许侧车 md，禁止同一槽双写。
///
/// Code Logic（这个枚举做什么）:
///     `Common` / `Adapted` / `Exclusive` 三值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiInstructionSlot {
    /// 公共槽：`GEMINI.md`
    Common,
    /// 适配槽：`.gemini/cc-partner.adapted.md`
    Adapted,
    /// 独有槽：`.gemini/cc-partner.exclusive.md`
    Exclusive,
}

/// Gemini 指令/资产适配器。
///
/// Business Logic（为什么需要这个结构体）:
///     service / inventory / portable scanner 通过统一 `AssetAdapter` 调用 Gemini 路径语义。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct。
#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiInstructionAdapter;

impl AssetAdapter for GeminiInstructionAdapter {
    /// 返回 Gemini 目标。
    ///
    /// Business Logic: 调度按 target 分发。
    /// Code Logic: `AgentTarget::Gemini`。
    fn target(&self) -> AgentTarget {
        AgentTarget::Gemini
    }

    /// 探测 Gemini 可执行文件、版本与配置根。
    ///
    /// Business Logic: 版本未知只能 scan-only；`GEMINI_HOME` 覆盖默认 `~/.gemini`。
    /// Code Logic: `resolve_all` + `resolve_executable("gemini")` + `probe_cli_version` + `build_probe`。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let executable = resolve_executable("gemini", env);
        let version = executable.as_ref().and_then(|p| probe_cli_version(p));
        Ok(build_probe(
            AgentTarget::Gemini,
            executable,
            version,
            homes.gemini.config_root,
        ))
    }

    /// 扫描 Gemini 指令源。
    ///
    /// Business Logic: 用户级 `config_root/GEMINI.md`；项目级目录 `GEMINI.md`；
    ///     侧车只用 `.gemini/cc-partner.*`（用户级相对 config_root）。
    /// Code Logic: 缺失文件不报错；不登记 `AGENTS.md`。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let (common, adapted, exclusive) = match scope.scope_kind {
            ScopeKind::User => (
                homes.gemini.config_root.join(GEMINI_COMMON_FILE),
                homes.gemini.config_root.join(GEMINI_ADAPTED_FILE),
                homes.gemini.config_root.join(GEMINI_EXCLUSIVE_FILE),
            ),
            ScopeKind::Project | ScopeKind::Directory => (
                scope.absolute_path.join(GEMINI_COMMON_FILE),
                scope
                    .absolute_path
                    .join(".gemini")
                    .join(GEMINI_ADAPTED_FILE),
                scope
                    .absolute_path
                    .join(".gemini")
                    .join(GEMINI_EXCLUSIVE_FILE),
            ),
        };
        let mut sources = Vec::new();
        push_existing(
            &mut sources,
            common,
            scope,
            InstructionSourceRole::NativePrimary,
        )?;
        push_existing(
            &mut sources,
            adapted,
            scope,
            InstructionSourceRole::ManagedProjection,
        )?;
        push_existing(
            &mut sources,
            exclusive,
            scope,
            InstructionSourceRole::ManagedProjection,
        )?;
        Ok(sources)
    }

    /// 渲染 Gemini 指令。
    ///
    /// Business Logic: common 写入 `GEMINI.md`；compiler 只给一个 file_name 时保持该名，
    ///     不得改写成 `AGENTS.md`。adapted/exclusive 落点由 `gemini_instruction_rel_path` 锁定侧车。
    /// Code Logic: `compile_render` 后拒绝 `AGENTS.md`。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        let compiled = crate::agent_hub::instructions::compile_render(
            &document.to_compiled_document(),
            AgentTarget::Gemini,
            context,
        );
        let mut rendered = RenderedInstruction::from_compiled(compiled);
        rendered.file_name = gemini_render_file_name(&rendered.file_name);
        Ok(rendered)
    }

    /// 扫描 Gemini Skill/Command 与 MCP。
    ///
    /// Business Logic: 用户 `~/.gemini/skills|commands`；项目 `.gemini/skills|commands`；
    ///     MCP 读 `settings.json` 的 `mcpServers`。
    /// Code Logic: JSONC helper 解析 settings；不扫 `AGENTS.md`。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let homes = TargetPathResolver::resolve_all(env);
        let base = gemini_portable_root(scope, &homes);
        let mut parts: Vec<Vec<DiscoveredPortableAsset>> = Vec::new();
        parts.push(scan_skill_dirs(
            AgentTarget::Gemini,
            scope.scope_kind,
            &base.join("skills"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_command_markdown_dir(
            AgentTarget::Gemini,
            scope.scope_kind,
            &base.join("commands"),
            PortableOriginKind::Native,
        )?);
        parts.push(scan_gemini_mcp(scope, &homes)?);
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
        let base = gemini_portable_root(scope, &homes);
        let mut parts = Vec::new();
        match kind {
            AssetKind::Skill => parts.push(scan_skill_dirs_manifest_only(
                AgentTarget::Gemini,
                scope.scope_kind,
                &base.join("skills"),
                PortableOriginKind::Native,
            )?),
            AssetKind::Command => parts.push(scan_command_markdown_dir(
                AgentTarget::Gemini,
                scope.scope_kind,
                &base.join("commands"),
                PortableOriginKind::Native,
            )?),
            AssetKind::Mcp => parts.push(scan_gemini_mcp(scope, &homes)?),
            AssetKind::Agent | AssetKind::Instruction | AssetKind::Plugin | AssetKind::Hook => {}
        }
        Ok(merge_discoveries(parts))
    }

    /// 渲染 Gemini portable 投影。
    ///
    /// Business Logic: 计划路径落到 skills/commands，由物化层放入 `.gemini/`；不写盘。
    /// Code Logic: 委托 `render_portable_payload`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        _context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError> {
        render_portable_payload(AgentTarget::Gemini, asset)
    }
}

/// Gemini 指令槽相对路径。
///
/// Business Logic（为什么需要这个函数）:
///     锁定单一落点：common=`GEMINI.md`，adapted/exclusive=侧车 md，禁止 `AGENTS.md`。
///
/// Code Logic（这个函数做什么）:
///     返回固定相对路径字符串。
pub fn gemini_instruction_rel_path(slot: GeminiInstructionSlot) -> &'static str {
    match slot {
        GeminiInstructionSlot::Common => GEMINI_COMMON_FILE,
        GeminiInstructionSlot::Adapted => ".gemini/cc-partner.adapted.md",
        GeminiInstructionSlot::Exclusive => ".gemini/cc-partner.exclusive.md",
    }
}

/// 把 compiler 文件名规范为 Gemini common 落点。
///
/// Business Logic（为什么需要这个函数）:
///     Gemini 公共正文必须是 `GEMINI.md`，不得回退到 `AGENTS.md`。
///
/// Code Logic（这个函数做什么）:
///     `AGENTS.md` / 空名 → `GEMINI.md`；否则保留 compiler 文件名。
fn gemini_render_file_name(compiler_name: &str) -> String {
    let name = compiler_name.replace('\\', "/");
    if name == "AGENTS.md" || name.ends_with("/AGENTS.md") || name.is_empty() {
        return GEMINI_COMMON_FILE.to_string();
    }
    name
}

/// 用户级 / 项目级 portable 根。
fn gemini_portable_root(scope: &LocalScopeMapping, homes: &super::paths::TargetHomes) -> PathBuf {
    match scope.scope_kind {
        ScopeKind::User => homes.gemini.config_root.clone(),
        ScopeKind::Project | ScopeKind::Directory => scope.absolute_path.join(".gemini"),
    }
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
    let non_empty = is_non_empty_utf8_file(&path)?;
    let relative_path = scope
        .project_root
        .as_ref()
        .and_then(|root| relative_path_string(root, &path))
        .or_else(|| scope.relative_root.clone());
    sources.push(InstructionSource {
        target: AgentTarget::Gemini,
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

/// 扫描 Gemini `settings.json` 的 `mcpServers`。
///
/// Business Logic: 复用 Claude JSONC helper；用户读 config_root，项目读 `.gemini/settings.json`。
/// Code Logic: 解析 JSON/JSONC → `parse_mcp_servers_json_map`。
fn scan_gemini_mcp(
    scope: &LocalScopeMapping,
    homes: &super::paths::TargetHomes,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    let path = match scope.scope_kind {
        ScopeKind::User => homes.gemini.config_root.join("settings.json"),
        ScopeKind::Project | ScopeKind::Directory => {
            scope.absolute_path.join(".gemini").join("settings.json")
        }
    };
    scan_mcp_json_file(&path, scope.scope_kind)
}

/// 从 JSON/JSONC 文件读 `mcpServers`。
fn scan_mcp_json_file(
    path: &Path,
    scope_kind: ScopeKind,
) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
    if !path.is_file() {
        return Ok(vec![]);
    }
    let text = std::fs::read_to_string(path)?;
    let value = match parse_json_or_jsonc(&text) {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    let Some(map) = value.get("mcpServers").and_then(|v| v.as_object()).cloned() else {
        return Ok(vec![]);
    };
    Ok(parse_mcp_servers_json_map(
        AgentTarget::Gemini,
        scope_kind,
        &map,
        path,
        PortableOriginKind::Native,
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn isolated_env(home: &Path) -> TargetEnvironment {
        let gemini = home.join(".gemini");
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::from([("GEMINI_HOME".into(), gemini.to_string_lossy().into_owned())]),
            path_entries: vec![],
        }
    }

    #[test]
    fn common_render_uses_gemini_md_not_agents_md() {
        let rendered = GeminiInstructionAdapter
            .render_instruction(
                &InstructionDocument {
                    common_markdown: "# gemini common\n".into(),
                    relative_key: String::new(),
                },
                &InstructionRenderContext::default(),
            )
            .unwrap();
        assert_eq!(rendered.file_name, "GEMINI.md");
        assert_ne!(rendered.file_name, "AGENTS.md");
        assert_eq!(
            gemini_instruction_rel_path(GeminiInstructionSlot::Common),
            "GEMINI.md"
        );
    }

    #[test]
    fn adapted_and_exclusive_use_sidecar_under_gemini_dir() {
        let adapted = gemini_instruction_rel_path(GeminiInstructionSlot::Adapted);
        let exclusive = gemini_instruction_rel_path(GeminiInstructionSlot::Exclusive);
        assert_eq!(adapted, ".gemini/cc-partner.adapted.md");
        assert_eq!(exclusive, ".gemini/cc-partner.exclusive.md");
        assert!(adapted.contains(".gemini/cc-partner."));
        assert!(exclusive.contains(".gemini/cc-partner."));
        assert!(!adapted.contains("AGENTS.md"));
        assert!(!exclusive.contains("AGENTS.md"));
        assert_ne!(adapted, exclusive);
    }

    #[test]
    fn scan_and_render_never_emit_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_text(&home.join(".gemini/GEMINI.md"), "user gemini\n");
        write_text(&home.join(".gemini/cc-partner.adapted.md"), "adapted\n");
        write_text(&home.join(".gemini/cc-partner.exclusive.md"), "exclusive\n");
        write_text(
            &home.join("proj/AGENTS.md"),
            "should not be gemini native\n",
        );
        write_text(&home.join("proj/GEMINI.md"), "project gemini\n");
        write_text(
            &home.join("proj/.gemini/cc-partner.exclusive.md"),
            "proj exclusive\n",
        );
        let env = isolated_env(home);
        let user_scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: home.to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let project = home.join("proj");
        let project_scope = LocalScopeMapping {
            scope_kind: ScopeKind::Project,
            absolute_path: project.clone(),
            project_root: Some(project),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec![],
        };

        let user_sources = GeminiInstructionAdapter
            .scan_instruction_sources(&user_scope, &env)
            .unwrap();
        let project_sources = GeminiInstructionAdapter
            .scan_instruction_sources(&project_scope, &env)
            .unwrap();
        for source in user_sources.iter().chain(project_sources.iter()) {
            let name = source
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            assert_ne!(
                name, "AGENTS.md",
                "Gemini must not treat AGENTS.md as a source"
            );
        }
        assert!(user_sources.iter().any(|s| {
            s.role == InstructionSourceRole::NativePrimary
                && s.path.file_name().and_then(|n| n.to_str()) == Some("GEMINI.md")
        }));
        assert!(user_sources.iter().any(|s| {
            s.role == InstructionSourceRole::ManagedProjection
                && s.path.to_string_lossy().contains("cc-partner.adapted.md")
        }));
        assert!(project_sources.iter().any(|s| {
            s.path
                .to_string_lossy()
                .replace('\\', "/")
                .contains(".gemini/cc-partner.exclusive.md")
        }));

        let rendered = GeminiInstructionAdapter
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
}
