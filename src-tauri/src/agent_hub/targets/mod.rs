//! agent_hub/targets — CLI target path probe 与 AssetAdapter 合同
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 需要按环境解析 Claude/Codex/OpenCode 配置根与可执行文件，
//!     扫描/渲染指令文档（Gate A）以及 Skill/Command/Agent/MCP 可移植资产（Gate B Task 3）。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AssetAdapter` trait 与共享 DTO；导出 paths 解析、portable 扫描 DTO 与三 target adapter。

pub mod claude;
pub mod codex;
pub mod opencode;
pub mod paths;
pub mod portable;
pub(crate) mod tree_metadata;

use crate::agent_hub::assets::PortableAssetPayload;
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use claude::ClaudeInstructionAdapter;
pub use codex::CodexInstructionAdapter;
pub use opencode::OpenCodeInstructionAdapter;
pub use paths::{
    compute_probe_fingerprint, is_non_empty_utf8_file, probe_cli_version, read_utf8_file,
    resolve_executable, OpenCodeHomePaths, TargetEnvironment, TargetHomePaths, TargetHomes,
    TargetPathResolver,
};
pub use portable::{
    scan_disabled_command_markdown_dir, scan_disabled_skill_dirs, scan_plugin_components_readonly,
    stamp_parent_plugin, AssetRenderContext, DiscoveredPortableAsset, PortableAssetOrigin,
    PortableDiscoveryStatus, PortableOriginKind, ProjectedAssetFile, TargetAssetProjection,
};

/// adapter 能力支持级别。
///
/// Business Logic（为什么需要这个枚举）:
///     版本未知或合同 evidence 不足时只能 scan-only，不能声称写/激活 supported。
///
/// Code Logic（这个枚举做什么）:
///     camelCase：`supported` / `scanOnly` / `unsupported`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AdapterSupportLevel {
    /// 可执行 + 可解析版本，指令 scan/render 可用
    Supported,
    /// 可扫描路径，但版本未知/解析失败或缺少可执行文件
    ScanOnly,
    /// 目标不可用
    Unsupported,
}

impl AdapterSupportLevel {
    /// 稳定 wire 字符串。
    ///
    /// Business Logic: probe 缓存与 materialization 失效比较依赖稳定 token。
    /// Code Logic: `supported` / `scanOnly` / `unsupported`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::ScanOnly => "scanOnly",
            Self::Unsupported => "unsupported",
        }
    }
}

/// CLI 探测结果。
///
/// Business Logic（为什么需要这个结构体）:
///     executable / version / configRoot 变化会使旧 materialization probe 失效。
///
/// Code Logic（这个结构体做什么）:
///     camelCase DTO：target、executable、version、configRoot、support、fingerprint。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetProbe {
    /// 目标 CLI
    pub target: AgentTarget,
    /// 可执行 realpath（缺失时为 None）
    pub executable: Option<PathBuf>,
    /// 解析出的版本字符串（未知/失败为 None）
    pub version: Option<String>,
    /// 解析后的配置根
    pub config_root: PathBuf,
    /// 支持级别
    pub support: AdapterSupportLevel,
    /// 环境指纹（exe+version+configRoot）
    pub fingerprint: String,
}

/// 本地 scope 到本机绝对路径的映射（scan 输入）。
///
/// Business Logic（为什么需要这个结构体）:
///     adapter 不能猜项目根；用户级与项目目录扫描入口必须显式注入。
///
/// Code Logic（这个结构体做什么）:
///     携带 scope_kind、absolute_path、project_root、relative_root 与 Codex fallback 文件名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalScopeMapping {
    /// user / project / directory
    pub scope_kind: ScopeKind,
    /// 当前扫描目录绝对路径（user scope 可为占位；实际用户文件走 config_root）
    pub absolute_path: PathBuf,
    /// 项目根（OpenCode 祖先 prelude 依赖；user scope 为 None）
    pub project_root: Option<PathBuf>,
    /// 相对项目根路径（空串表示项目根）
    pub relative_root: Option<String>,
    /// Codex 配置的 project_doc_fallback_filenames（同层额外候选）
    #[serde(default)]
    pub codex_fallback_filenames: Vec<String>,
}

/// 指令源文件角色。
///
/// Business Logic（为什么需要这个枚举）:
///     Codex override/原生/fallback 与 OpenCode 祖先 prelude 的导入语义不同。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 角色 token。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstructionSourceRole {
    /// Hub 受管投影（Codex `AGENTS.override.md`）
    ManagedProjection,
    /// 原生主文件（Claude `CLAUDE.md` / OpenCode·Codex `AGENTS.md`）
    NativePrimary,
    /// Codex fallback 文件
    Fallback,
    /// OpenCode 祖先 prelude 依赖
    AncestorPrelude,
}

impl InstructionSourceRole {
    /// 稳定 wire 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManagedProjection => "managedProjection",
            Self::NativePrimary => "nativePrimary",
            Self::Fallback => "fallback",
            Self::AncestorPrelude => "ancestorPrelude",
        }
    }
}

/// 扫描到的指令源。
///
/// Business Logic（为什么需要这个结构体）:
///     首次纳管 preview 必须列出生效源与被遮蔽非空源，禁止静默丢弃。
///
/// Code Logic（这个结构体做什么）:
///     记录 path/role/active/native_active/non_empty/diagnostics/相对路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSource {
    /// 所属 target
    pub target: AgentTarget,
    /// 文件绝对路径
    pub path: PathBuf,
    /// scope 种类
    pub scope_kind: ScopeKind,
    /// 源角色
    pub role: InstructionSourceRole,
    /// 是否为 CLI 当前生效文件
    pub active: bool,
    /// OpenCode 最近本地规则是否 native-active
    pub native_active: bool,
    /// 文件存在且 trim 后非空
    pub non_empty: bool,
    /// 相对项目根路径（祖先列表/显示用）
    pub relative_path: Option<String>,
    /// 诊断信息（如被 override 遮蔽）
    pub diagnostics: Vec<String>,
}

/// Adapter 层指令文档输入（可从完整块文档降级）。
///
/// Business Logic（为什么需要这个结构体）:
///     render_instruction 需要一份可渲染的正文与逻辑键，供 adapter 输出目标文件名与 prelude。
///
/// Code Logic（这个结构体做什么）:
///     持有 common_markdown 与 relative_key；Task 4 compiler 将其提升为块文档再渲染。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDocument {
    /// 共同 Markdown 正文
    pub common_markdown: String,
    /// 逻辑相对键（如 "" / "src-tauri"）
    pub relative_key: String,
}

impl InstructionDocument {
    /// 提升为完整块文档（整篇 shared）。
    ///
    /// Business Logic: adapter stub 输入进入 Task 4 compiler。
    /// Code Logic: 委托 `instructions::InstructionDocument::from_shared_markdown`。
    pub fn to_compiled_document(&self) -> crate::agent_hub::instructions::InstructionDocument {
        crate::agent_hub::instructions::InstructionDocument::from_shared_markdown(
            &self.relative_key,
            &self.common_markdown,
        )
    }
}

/// 渲染上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     OpenCode 需要祖先相对路径列表生成 prelude，不能复制祖先正文。
///
/// Code Logic（这个结构体做什么）:
///     提供 project_root / directory_relative / ancestor_agent_paths。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstructionRenderContext {
    /// 项目根
    pub project_root: Option<PathBuf>,
    /// 当前目录相对项目根（空=根）
    pub directory_relative: Option<String>,
    /// 从项目根到父目录的 AGENTS.md 相对路径列表（OpenCode prelude）
    pub ancestor_agent_paths: Vec<String>,
}

/// 渲染结果。
///
/// Business Logic（为什么需要这个结构体）:
///     projection 需要文件名、正文、prelude 与 materialization base block map。
///
/// Code Logic（这个结构体做什么）:
///     保存 target / file_name / content / prelude，以及 Task 4 的 bytes/block_map/managed_prefix_len/diagnostics。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedInstruction {
    /// 目标
    pub target: AgentTarget,
    /// 输出文件名
    pub file_name: String,
    /// 完整文件内容（含 prelude）
    pub content: String,
    /// 独立 prelude 段（OpenCode）；其它 target 为 None
    pub prelude: Option<String>,
    /// 完整文件字节（与 content 一致，UTF-8）
    #[serde(default)]
    pub bytes: Vec<u8>,
    /// 块区间映射（materialization base map）
    #[serde(default)]
    pub block_map: Vec<crate::agent_hub::instructions::RenderedBlockRange>,
    /// managed prelude 占用的前缀字节数
    #[serde(default)]
    pub managed_prefix_len: usize,
    /// 诊断
    #[serde(default)]
    pub diagnostics: Vec<crate::agent_hub::instructions::PortabilityDiagnostic>,
}

impl RenderedInstruction {
    /// 从 compiler 输出构造。
    ///
    /// Business Logic: 三 adapter 共用 compiler，保留 content/prelude 兼容字段。
    /// Code Logic: bytes→content；managed_prefix→prelude。
    pub fn from_compiled(
        compiled: crate::agent_hub::instructions::CompiledRenderedInstruction,
    ) -> Self {
        let content = compiled.content_str().to_string();
        let prelude = compiled.managed_prelude().map(|s| s.to_string());
        Self {
            target: compiled.target,
            file_name: compiled.file_name,
            content,
            prelude,
            bytes: compiled.bytes,
            block_map: compiled.block_map,
            managed_prefix_len: compiled.managed_prefix_len,
            diagnostics: compiled.diagnostics,
        }
    }
}

/// 目标资产适配器合同（指令 + Gate B portable 资产 scan/render）。
///
/// Business Logic（为什么需要这个 trait）:
///     每个 CLI 的路径、生效优先级与渲染措辞不同，但 service 层只依赖统一合同。
///
/// Code Logic（这个 trait 做什么）:
///     target / probe / scan_instruction_sources / render_instruction /
///     scan_portable_assets / render_portable_asset。
pub trait AssetAdapter: Send + Sync {
    /// 返回适配的目标枚举。
    ///
    /// Business Logic: 调度与绑定按 target 分发。
    /// Code Logic: 返回 `AgentTarget`。
    fn target(&self) -> AgentTarget;

    /// 探测可执行文件、版本与配置根。
    ///
    /// Business Logic: 环境变化使旧 probe 失效；版本未知时只能 scan-only。
    /// Code Logic: 使用注入 `TargetEnvironment`，不改 process env。
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError>;

    /// 扫描指令源文件（无写入）。
    ///
    /// Business Logic: 首次纳管与 reconcile 需要完整 active/inactive 源列表。
    /// Code Logic: 仅读磁盘；缺失文件不报错，可返回空或 non_empty=false 条目策略由实现决定。
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError>;

    /// 渲染指令目标文件内容（Gate A stub：不做完整块编译）。
    ///
    /// Business Logic: projection 需要目标文件名与正文；OpenCode 需 prelude。
    /// Code Logic: 返回 `RenderedInstruction`，不写盘。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError>;

    /// 扫描 Skill/Command/Agent/MCP 可移植资产（只读，无写盘）。
    ///
    /// Business Logic: 每个 origin 独立发现；兼容路径不得标为 native 输出候选。
    /// Code Logic: 返回 `DiscoveredPortableAsset` 列表（含 path/hash/status/diagnostics）。
    fn scan_portable_assets(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError>;

    /// 按 kind 在目录遍历前裁剪扫描；默认实现仅为第三方 adapter 兼容。
    fn scan_portable_assets_filtered(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
        kind: Option<crate::agent_hub::models::AssetKind>,
    ) -> Result<Vec<DiscoveredPortableAsset>, AppError> {
        let mut assets = self.scan_portable_assets(scope, env)?;
        if let Some(kind) = kind {
            assets.retain(|asset| asset.kind == kind);
        }
        Ok(assets)
    }

    /// 渲染可移植资产为目标投影计划（不写盘）。
    ///
    /// Business Logic: projection/package 任务消费 relative files；本方法只生成字节计划。
    /// Code Logic: `PortableAssetPayload` → `TargetAssetProjection`。
    fn render_portable_asset(
        &self,
        asset: &PortableAssetPayload,
        context: &AssetRenderContext,
    ) -> Result<TargetAssetProjection, AppError>;
}

/// 根据可执行与版本决定支持级别。
///
/// Business Logic: 未知/解析失败版本 = scan-only。
/// Code Logic: exe+version → Supported；否则 ScanOnly。
pub(crate) fn support_from_probe(
    executable: Option<&std::path::Path>,
    version: Option<&str>,
) -> AdapterSupportLevel {
    match (executable, version) {
        (Some(_), Some(v)) if !v.trim().is_empty() => AdapterSupportLevel::Supported,
        _ => AdapterSupportLevel::ScanOnly,
    }
}

/// 构造统一 TargetProbe。
///
/// Business Logic: 三 adapter 共享 fingerprint / support 规则。
/// Code Logic: 打包字段并计算 fingerprint。
pub(crate) fn build_probe(
    target: AgentTarget,
    executable: Option<PathBuf>,
    version: Option<String>,
    config_root: PathBuf,
) -> TargetProbe {
    let support = support_from_probe(executable.as_deref(), version.as_deref());
    let fingerprint = compute_probe_fingerprint(
        target.as_str(),
        executable.as_deref(),
        version.as_deref(),
        &config_root,
    );
    TargetProbe {
        target,
        executable,
        version,
        config_root,
        support,
        fingerprint,
    }
}

/// 计算相对路径字符串（正斜杠）。
///
/// Business Logic: OpenCode prelude 与诊断显示需要稳定相对路径。
/// Code Logic: strip_prefix 后 to_string_lossy 并把 `\` 换成 `/`。
pub(crate) fn relative_path_string(
    root: &std::path::Path,
    path: &std::path::Path,
) -> Option<String> {
    path.strip_prefix(root).ok().map(|p| {
        let s = p.to_string_lossy().replace('\\', "/");
        if s.is_empty() {
            ".".to_string()
        } else {
            s
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::ScopeKind;
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn write_exec(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    fn write_text(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn base_env(home: &Path, path_entries: Vec<PathBuf>) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries,
        }
    }

    #[test]
    fn probe_marks_unknown_version_scan_only() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        // 非零退出 → 版本解析失败
        write_exec(&bin, "#!/bin/sh\nexit 1\n");
        let mut env = base_env(dir.path(), vec![dir.path().to_path_buf()]);
        env.vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            dir.path().join("claude-home").to_string_lossy().into(),
        );
        let probe = ClaudeInstructionAdapter.probe(&env).unwrap();
        assert_eq!(probe.support, AdapterSupportLevel::ScanOnly);
        assert!(probe.version.is_none());
        assert!(probe.executable.is_some());
    }

    #[test]
    fn codex_scan_reports_active_override_and_inactive_non_empty_sources() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("proj");
        write_text(&project.join("AGENTS.override.md"), "override content\n");
        write_text(&project.join("AGENTS.md"), "native agents\n");
        write_text(&project.join("FALLBACK.md"), "fallback body\n");

        let env = base_env(root.path(), vec![]);
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::Directory,
            absolute_path: project.clone(),
            project_root: Some(project.clone()),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec!["FALLBACK.md".into()],
        };
        let sources = CodexInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        assert!(sources.len() >= 3, "sources={sources:?}");

        let override_src = sources
            .iter()
            .find(|s| s.role == InstructionSourceRole::ManagedProjection)
            .expect("override");
        assert!(override_src.active);
        assert!(override_src.non_empty);
        assert_eq!(override_src.path.file_name().unwrap(), "AGENTS.override.md");

        let agents = sources
            .iter()
            .find(|s| {
                s.role == InstructionSourceRole::NativePrimary
                    && s.path.file_name().unwrap() == "AGENTS.md"
            })
            .expect("agents");
        assert!(!agents.active);
        assert!(agents.non_empty);
        assert!(
            agents
                .diagnostics
                .iter()
                .any(|d| d.contains("shadowed") || d.contains("遮蔽")),
            "diag={:?}",
            agents.diagnostics
        );

        let fallback = sources
            .iter()
            .find(|s| s.role == InstructionSourceRole::Fallback)
            .expect("fallback");
        assert!(!fallback.active);
        assert!(fallback.non_empty);
        assert!(
            fallback.diagnostics.iter().any(|d| !d.is_empty()),
            "fallback diag empty"
        );
    }

    #[test]
    fn opencode_scan_marks_nearest_native_active_and_lists_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("repo");
        write_text(&project.join("AGENTS.md"), "root rules\n");
        write_text(&project.join("a").join("AGENTS.md"), "a rules\n");
        write_text(&project.join("a").join("b").join("AGENTS.md"), "b rules\n");

        let env = base_env(root.path(), vec![]);
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::Directory,
            absolute_path: project.join("a").join("b"),
            project_root: Some(project.clone()),
            relative_root: Some("a/b".into()),
            codex_fallback_filenames: vec![],
        };
        let sources = OpenCodeInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        let nearest = sources
            .iter()
            .find(|s| s.native_active)
            .expect("nearest native-active");
        assert!(nearest.active);
        assert!(nearest
            .path
            .ends_with(Path::new("a").join("b").join("AGENTS.md")));

        let ancestors: Vec<_> = sources
            .iter()
            .filter(|s| s.role == InstructionSourceRole::AncestorPrelude)
            .collect();
        assert_eq!(ancestors.len(), 2, "ancestors={ancestors:?}");
        assert!(ancestors.iter().all(|s| !s.native_active && !s.active));
        let rels: Vec<_> = ancestors
            .iter()
            .filter_map(|s| s.relative_path.as_deref())
            .collect();
        assert!(rels
            .iter()
            .any(|r| *r == "AGENTS.md" || *r == "./AGENTS.md" || r.ends_with("AGENTS.md")));
        assert!(rels.iter().any(|r| r.contains("a/AGENTS.md")));
    }

    /// Business Logic: OpenCode 用户原生规则缺失时会继承 Claude；原生文件出现后必须遮蔽 fallback。
    /// Code Logic: 隔离 HOME 分别断言 fallback active 与 native-active 优先级。
    #[test]
    fn opencode_user_scan_resolves_claude_fallback_and_native_priority() {
        let root = tempfile::tempdir().unwrap();
        let opencode_home = root.path().join("opencode-home");
        let claude_home = root.path().join("claude-home");
        write_text(&claude_home.join("CLAUDE.md"), "fallback rules\n");
        let mut env = base_env(root.path(), vec![]);
        env.vars.insert(
            "OPENCODE_CONFIG_DIR".into(),
            opencode_home.to_string_lossy().into(),
        );
        env.vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            claude_home.to_string_lossy().into(),
        );
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: root.path().to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let inherited = OpenCodeInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        assert_eq!(inherited.len(), 1);
        assert_eq!(inherited[0].role, InstructionSourceRole::Fallback);
        assert!(inherited[0].active);

        write_text(&opencode_home.join("AGENTS.md"), "native rules\n");
        let native = OpenCodeInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        let active = native.iter().find(|source| source.active).expect("active");
        assert_eq!(active.role, InstructionSourceRole::NativePrimary);
        let fallback = native
            .iter()
            .find(|source| source.role == InstructionSourceRole::Fallback)
            .expect("fallback");
        assert!(!fallback.active);
        assert!(fallback
            .diagnostics
            .iter()
            .any(|reason| reason == "opencode_claude_fallback_shadowed_by_native"));
    }

    /// Business Logic: 两个 OpenCode 禁用开关任一存在时都不能声称 Claude fallback 生效。
    /// Code Logic: 对两个环境变量分别扫描并断言 fallback inactive + 稳定诊断。
    #[test]
    fn opencode_user_scan_honors_both_claude_fallback_disable_envs() {
        for key in [
            "OPENCODE_DISABLE_CLAUDE_CODE",
            "OPENCODE_DISABLE_CLAUDE_CODE_PROMPT",
        ] {
            let root = tempfile::tempdir().unwrap();
            let claude_home = root.path().join("claude-home");
            write_text(&claude_home.join("CLAUDE.md"), "fallback rules\n");
            let mut env = base_env(root.path(), vec![]);
            env.vars.insert(
                "CLAUDE_CONFIG_DIR".into(),
                claude_home.to_string_lossy().into(),
            );
            env.vars.insert(key.into(), "1".into());
            let scope = LocalScopeMapping {
                scope_kind: ScopeKind::User,
                absolute_path: root.path().to_path_buf(),
                project_root: None,
                relative_root: None,
                codex_fallback_filenames: vec![],
            };
            let sources = OpenCodeInstructionAdapter
                .scan_instruction_sources(&scope, &env)
                .unwrap();
            let fallback = sources.first().expect("fallback visible");
            assert!(!fallback.active, "{key} must disable fallback");
            assert!(fallback
                .diagnostics
                .iter()
                .any(|reason| reason == "opencode_claude_fallback_disabled"));
        }
    }

    #[test]
    fn claude_keeps_user_and_project_paths_separate() {
        let root = tempfile::tempdir().unwrap();
        let claude_home = root.path().join("claude-home");
        let project = root.path().join("proj");
        write_text(&claude_home.join("CLAUDE.md"), "user rules\n");
        write_text(&project.join("CLAUDE.md"), "project rules\n");

        let mut env = base_env(root.path(), vec![]);
        env.vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            claude_home.to_string_lossy().into(),
        );

        let user_scope = LocalScopeMapping {
            scope_kind: ScopeKind::User,
            absolute_path: root.path().to_path_buf(),
            project_root: None,
            relative_root: None,
            codex_fallback_filenames: vec![],
        };
        let user_sources = ClaudeInstructionAdapter
            .scan_instruction_sources(&user_scope, &env)
            .unwrap();
        assert_eq!(user_sources.len(), 1);
        assert_eq!(user_sources[0].path, claude_home.join("CLAUDE.md"));
        assert!(user_sources[0].active);
        assert_eq!(user_sources[0].scope_kind, ScopeKind::User);

        let project_scope = LocalScopeMapping {
            scope_kind: ScopeKind::Directory,
            absolute_path: project.clone(),
            project_root: Some(project.clone()),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec![],
        };
        let project_sources = ClaudeInstructionAdapter
            .scan_instruction_sources(&project_scope, &env)
            .unwrap();
        assert_eq!(project_sources.len(), 1);
        assert_eq!(project_sources[0].path, project.join("CLAUDE.md"));
        assert_eq!(project_sources[0].scope_kind, ScopeKind::Directory);
        assert_ne!(project_sources[0].path, user_sources[0].path);
    }

    #[test]
    fn render_stubs_are_target_specific_and_opencode_lists_ancestors() {
        let doc = InstructionDocument {
            common_markdown: "# rules\n".into(),
            relative_key: "src-tauri".into(),
        };
        let ctx = InstructionRenderContext {
            project_root: Some(PathBuf::from("/repo")),
            directory_relative: Some("src-tauri".into()),
            ancestor_agent_paths: vec!["AGENTS.md".into(), "web/AGENTS.md".into()],
        };

        let claude = ClaudeInstructionAdapter
            .render_instruction(&doc, &ctx)
            .unwrap();
        assert_eq!(claude.file_name, "CLAUDE.md");
        assert!(claude.content.contains("# rules"));
        assert!(claude.prelude.is_none());

        let codex = CodexInstructionAdapter
            .render_instruction(&doc, &ctx)
            .unwrap();
        assert_eq!(codex.file_name, "AGENTS.override.md");
        assert!(codex.prelude.is_none());

        let oc = OpenCodeInstructionAdapter
            .render_instruction(&doc, &ctx)
            .unwrap();
        assert_eq!(oc.file_name, "AGENTS.md");
        let prelude = oc.prelude.expect("prelude");
        assert!(prelude.contains("AGENTS.md"));
        assert!(prelude.contains("web/AGENTS.md"));
        assert!(oc.content.starts_with(&prelude) || oc.content.contains(&prelude));
        assert!(oc.content.contains("# rules"));
    }

    #[test]
    fn scan_and_render_do_not_write_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("p");
        fs::create_dir_all(&project).unwrap();
        // 不创建任何指令文件
        let env = base_env(root.path(), vec![]);
        let scope = LocalScopeMapping {
            scope_kind: ScopeKind::Directory,
            absolute_path: project.clone(),
            project_root: Some(project.clone()),
            relative_root: Some(String::new()),
            codex_fallback_filenames: vec!["FALLBACK.md".into()],
        };
        let _ = ClaudeInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        let _ = CodexInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        let _ = OpenCodeInstructionAdapter
            .scan_instruction_sources(&scope, &env)
            .unwrap();
        let doc = InstructionDocument {
            common_markdown: "x".into(),
            relative_key: String::new(),
        };
        let _ = ClaudeInstructionAdapter
            .render_instruction(&doc, &InstructionRenderContext::default())
            .unwrap();
        assert!(!project.join("CLAUDE.md").exists());
        assert!(!project.join("AGENTS.md").exists());
        assert!(!project.join("AGENTS.override.md").exists());
    }
}
