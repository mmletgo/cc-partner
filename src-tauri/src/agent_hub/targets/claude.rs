//! agent_hub/targets/claude — Claude Code instruction adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude 用户级与项目级均使用 `CLAUDE.md`，但路径空间分离：
//!     用户文件在配置根，项目文件在目录本身；禁止混扫。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `claude` 可执行文件与 CLAUDE_CONFIG_DIR；
//!     scan 仅返回对应 scope 的 CLAUDE.md；render 输出 `CLAUDE.md` 正文。

use super::paths::{probe_cli_version, read_utf8_file, resolve_executable, TargetPathResolver};
use super::{
    build_probe, AssetAdapter, InstructionDocument, InstructionRenderContext, InstructionSource,
    InstructionSourceRole, LocalScopeMapping, RenderedInstruction, TargetEnvironment, TargetProbe,
};
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::error::AppError;

/// Claude 指令适配器（Gate A 仅指令）。
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
    /// Business Logic: Gate A 仅输出共同正文，完整块编译在 Task 4。
    /// Code Logic: file_name=CLAUDE.md，content=common_markdown。
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        _context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError> {
        Ok(RenderedInstruction {
            target: AgentTarget::Claude,
            file_name: "CLAUDE.md".into(),
            content: document.common_markdown.clone(),
            prelude: None,
        })
    }
}
