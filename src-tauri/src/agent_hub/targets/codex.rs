//! agent_hub/targets/codex — Codex CLI instruction adapter
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex 与 OpenCode 都可能使用 AGENTS.md；Hub 用 `AGENTS.override.md` 作为受管投影，
//!     使 OpenCode 专属 AGENTS.md 保持 target-specific，同时扫描被遮蔽非空源。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `AssetAdapter`：probe `codex`；扫描 override / AGENTS.md / fallback；
//!     生效优先级 override > AGENTS.md > 首个非空 fallback；render 输出 AGENTS.override.md。

use super::paths::{
    is_non_empty_utf8_file, probe_cli_version, resolve_executable, TargetPathResolver,
};
use super::{
    build_probe, relative_path_string, AssetAdapter, InstructionDocument, InstructionRenderContext,
    InstructionSource, InstructionSourceRole, LocalScopeMapping, RenderedInstruction,
    TargetEnvironment, TargetProbe,
};
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::error::AppError;
use std::path::{Path, PathBuf};

/// Codex 默认额外扫描的 fallback 文件名（当 scope 未注入时）。
const DEFAULT_CODEX_FALLBACKS: &[&str] = &["AGENTS.fallback.md", "agents.md"];

/// Codex 指令适配器（Gate A 仅指令）。
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
