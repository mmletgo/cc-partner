//! agent_hub/cross_agent — 同机跨 Agent 手动同步与适配（阶段三）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户选择源 Agent 资产/指令 → 预览 shared/adapted/targetOnly/residual。
//!     真实 CLI 写盘证据未完成前，apply 固定 fail-closed；禁止 sidecar 因外部编辑自动跨 target 写盘。
//!
//! Code Logic（这个模块做什么）:
//!     指令：classify_import / block_needs_target_isolation / compile_render + 有界内容 diff；
//!     plugin 返回 partial residuals，不得宣称 full。

use crate::agent_hub::instructions::{
    block_needs_target_isolation, classify_import, compile_render, ImportScopeContext,
    InstructionBlockMode, TargetMarkdownSource,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::targets::{InstructionRenderContext, TargetEnvironment, TargetPathResolver};
use crate::agent_hub::user_instructions::{read_text_bounded, render_bounded_diff};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[cfg(test)]
use std::path::PathBuf;

const USER_SCOPE: &str = "user";

fn default_cross_agent_scope() -> String {
    USER_SCOPE.to_string()
}

/// 跨 Agent 资产类型（阶段三最小集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossAgentKind {
    Instruction,
    Skill,
    Command,
    Mcp,
    Plugin,
}

/// 适配结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CrossAgentAdaptMode {
    Shared,
    Adapted,
    TargetOnly,
    Residual,
}

/// 单目标预览行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentTargetPreview {
    pub destination: AgentTarget,
    pub mode: CrossAgentAdaptMode,
    pub path: String,
    pub rendered_hash: Option<String>,
    /// 预览时目标文件的精确 hash；None 表示文件当时不存在。
    pub observed_hash: Option<String>,
    pub unified_diff: Option<String>,
    pub partial_blockers: Vec<String>,
    pub can_apply: bool,
}

/// 跨 Agent preview 报告。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentPreviewReport {
    pub source: AgentTarget,
    pub kind: CrossAgentKind,
    pub destinations: Vec<CrossAgentTargetPreview>,
    pub needs_adaptation: bool,
    /// 绑定 source、scope、目标路径、渲染内容与预览时目标 hash。
    pub plan_hash: String,
}

/// 指令跨 Agent preview 请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewCrossAgentInstructionRequest {
    pub source: AgentTarget,
    pub destinations: Vec<AgentTarget>,
    pub source_markdown: String,
    /// 当前只实现用户级路径；项目级必须显式失败，不能回退到用户目录。
    #[serde(default = "default_cross_agent_scope")]
    pub scope: String,
    /// 用户级路径由 adapter 解析；可选显式覆盖（缺省空 map → 用 default path resolver）
    #[serde(default)]
    pub destination_paths: BTreeMap<AgentTarget, String>,
}

/// Apply 指令跨 Agent 请求（one-shot）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyCrossAgentInstructionRequest {
    pub source: AgentTarget,
    pub destinations: Vec<AgentTarget>,
    pub source_markdown: String,
    #[serde(default = "default_cross_agent_scope")]
    pub scope: String,
    /// 可选路径覆盖；IPC 可省略，缺省走 adapter 默认用户级路径
    #[serde(default)]
    pub destination_paths: BTreeMap<AgentTarget, String>,
    /// 必须来自 preview；apply 会按当前磁盘状态重建计划并精确比对。
    #[serde(default)]
    pub plan_hash: String,
    pub client_request_id: String,
}

/// 单目标 apply 结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossAgentApplyTargetResult {
    pub destination: AgentTarget,
    pub status: String,
    pub path: String,
    pub error_code: Option<String>,
}

/// 预览指令跨 Agent 适配。
///
/// Business Logic: CLI 专属术语块默认 targetOnly + needsAdaptation；纯共享正文可 shared。
/// Code Logic: classify_import(user scope) → 按 destination compile_render → diff。
pub fn preview_cross_agent_instruction(
    request: &PreviewCrossAgentInstructionRequest,
    env: &TargetEnvironment,
) -> Result<CrossAgentPreviewReport, AppError> {
    ensure_user_scope(&request.scope)?;
    if !request.destination_paths.is_empty() {
        return Err(AppError::validation(
            "CROSS_AGENT_DESTINATION_PATH_OVERRIDE_UNAVAILABLE",
        ));
    }
    if request.destinations.is_empty() {
        return Err(AppError::validation("CROSS_AGENT_DESTINATIONS_REQUIRED"));
    }
    if request.destinations.contains(&request.source) {
        return Err(AppError::validation("CROSS_AGENT_DEST_EQUALS_SOURCE"));
    }
    let sources = [TargetMarkdownSource {
        target: request.source,
        markdown: request.source_markdown.clone(),
    }];
    // 跨 Agent 同步不用 user-scope「强制 targetOnly」：普通正文可 shared，CLI 术语仍隔离。
    let classified = classify_import("", ImportScopeContext::project_subdirectory(), &sources);
    let needs_adaptation = classified
        .diagnostics
        .iter()
        .any(|d| d.code == "needsAdaptation")
        || classified.document.blocks.iter().any(|b| {
            b.mode == InstructionBlockMode::TargetOnly
                || block_needs_target_isolation(b.common_markdown.as_deref().unwrap_or(""))
        });

    let homes = TargetPathResolver::resolve_all(env);
    let mut destinations = Vec::new();
    for dest in &request.destinations {
        let path = default_user_instruction_path(*dest, &homes);
        let compiled = compile_render(
            &classified.document,
            *dest,
            &InstructionRenderContext::default(),
        );
        let body = compiled.user_body();
        let mut partial_blockers = Vec::new();
        let mode = if body.trim().is_empty() && !request.source_markdown.trim().is_empty() {
            partial_blockers.push(format!("{}:targetOnly_empty_render", dest.as_str()));
            CrossAgentAdaptMode::TargetOnly
        } else if needs_adaptation {
            CrossAgentAdaptMode::Adapted
        } else {
            CrossAgentAdaptMode::Shared
        };
        // Plugin residual 语义：指令无 residual；skill/plugin 在其它入口
        let (before, observed_hash) = read_preview_target(&path)?;
        let after = body.to_string();
        let rendered_hash = if after.is_empty() {
            None
        } else {
            Some(sha256_hex(after.as_bytes()))
        };
        let (unified_diff, diff_truncated) = format_simple_diff(&before, &after);
        if diff_truncated {
            partial_blockers.push("CROSS_AGENT_PREVIEW_DIFF_TRUNCATED".into());
        }
        partial_blockers.push("CROSS_AGENT_PREVIEW_ONLY".into());
        destinations.push(CrossAgentTargetPreview {
            destination: *dest,
            mode,
            path,
            rendered_hash,
            observed_hash,
            unified_diff: Some(unified_diff),
            partial_blockers,
            can_apply: false,
        });
    }
    let plan_hash = compute_instruction_plan_hash(request, &destinations);
    Ok(CrossAgentPreviewReport {
        source: request.source,
        kind: CrossAgentKind::Instruction,
        destinations,
        needs_adaptation,
        plan_hash,
    })
}

/// 跨 Agent apply 兼容入口（真实 CLI 写盘认证前固定阻止）。
///
/// Business Logic（为什么需要这个函数）:
///     旧前端或混合版本后端仍可能调用 apply command；在 ownership、备份/回滚、durable
///     ledger 与真实 CLI L3 证据闭环前，任何请求都不得触碰目标文件。
///
/// Code Logic（这个函数做什么）:
///     保留请求/环境签名用于 N/N+1 兼容，但在任何 preview 重建或 writer 调用前返回稳定错误码。
pub fn apply_cross_agent_instruction(
    _request: &ApplyCrossAgentInstructionRequest,
    _env: &TargetEnvironment,
) -> Result<Vec<CrossAgentApplyTargetResult>, AppError> {
    Err(AppError::validation("CROSS_AGENT_APPLY_NOT_CERTIFIED"))
}

/// 把任意版本 sidecar 返回的 selective report 收紧为当前 preview-only 合同。
///
/// Business Logic（为什么需要这个函数）:
///     新 GUI 可能连接旧 sidecar；旧响应曾宣称 canApply=true，并把字符长度伪装成 diff。
///     GUI command 必须在跨版本边界再次降级，不能信任旧 owner 的 mutation 提示。
///
/// Code Logic（这个函数做什么）:
///     所有 destination 强制 canApply=false、补稳定 blocker；识别旧长度占位格式后丢弃
///     unifiedDiff 并补 diff-unavailable blocker。真实有界 diff 保留。
pub fn enforce_cross_agent_preview_only(report: &mut CrossAgentPreviewReport) {
    for destination in &mut report.destinations {
        destination.can_apply = false;
        if !destination
            .partial_blockers
            .iter()
            .any(|code| code == "CROSS_AGENT_PREVIEW_ONLY")
        {
            destination
                .partial_blockers
                .push("CROSS_AGENT_PREVIEW_ONLY".into());
        }
        if destination
            .unified_diff
            .as_deref()
            .is_some_and(is_legacy_length_placeholder_diff)
        {
            destination.unified_diff = None;
            if !destination
                .partial_blockers
                .iter()
                .any(|code| code == "CROSS_AGENT_PREVIEW_DIFF_UNAVAILABLE")
            {
                destination
                    .partial_blockers
                    .push("CROSS_AGENT_PREVIEW_DIFF_UNAVAILABLE".into());
            }
        }
    }
}

fn is_legacy_length_placeholder_diff(diff: &str) -> bool {
    let mut lines = diff.lines();
    if lines.next() != Some("--- before") || lines.next() != Some("+++ after") {
        return false;
    }
    let Some(counts) = lines.next() else {
        return false;
    };
    if lines.next().is_some() {
        return false;
    }
    let Some(rest) = counts.strip_prefix('-') else {
        return false;
    };
    let Some((before, after)) = rest.split_once('+') else {
        return false;
    };
    !before.is_empty()
        && !after.is_empty()
        && before.chars().all(|c| c.is_ascii_digit())
        && after.chars().all(|c| c.is_ascii_digit())
}

/// Plugin 跨 Agent 永远 partial（阶段三诚实降级）。
pub fn preview_cross_agent_plugin_residual(
    source: AgentTarget,
    destination: AgentTarget,
) -> CrossAgentTargetPreview {
    CrossAgentTargetPreview {
        destination,
        mode: CrossAgentAdaptMode::Residual,
        path: String::new(),
        rendered_hash: None,
        observed_hash: None,
        unified_diff: None,
        partial_blockers: vec![
            format!(
                "{}→{}:hook_runtime_not_auto_translated",
                source.as_str(),
                destination.as_str()
            ),
            format!(
                "{}→{}:plugin_package_residual",
                source.as_str(),
                destination.as_str()
            ),
        ],
        can_apply: false,
    }
}

/// 默认用户级指令路径。
fn default_user_instruction_path(
    target: AgentTarget,
    homes: &crate::agent_hub::targets::paths::TargetHomes,
) -> String {
    match target {
        AgentTarget::Claude => homes.claude.config_root.join("CLAUDE.md"),
        AgentTarget::Codex => homes.codex.config_root.join("AGENTS.md"),
        AgentTarget::OpenCode => homes.opencode.config_root.join("AGENTS.md"),
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor => {
            homes.default_user_instruction_path(target)
        }
    }
    .to_string_lossy()
    .into_owned()
}

fn format_simple_diff(before: &str, after: &str) -> (String, bool) {
    if before == after {
        return (String::new(), false);
    }
    render_bounded_diff(before, after)
}

/// 读取预览目标，保留“存在但为空”与“不存在”的区别。
fn read_preview_target(path: &str) -> Result<(String, Option<String>), AppError> {
    let Some(text) = read_text_bounded(path)? else {
        return Ok((String::new(), None));
    };
    let hash = sha256_hex(text.as_bytes());
    Ok((text, Some(hash)))
}

/// 当前阶段只允许用户级跨 Agent 写入，避免项目 scope 静默落到用户目录。
fn ensure_user_scope(scope: &str) -> Result<(), AppError> {
    if scope.trim() == USER_SCOPE {
        Ok(())
    } else {
        Err(AppError::validation(
            "CROSS_AGENT_PROJECT_SCOPE_UNAVAILABLE",
        ))
    }
}

/// 计算 selective preview 的确定性绑定 hash。
fn compute_instruction_plan_hash(
    request: &PreviewCrossAgentInstructionRequest,
    destinations: &[CrossAgentTargetPreview],
) -> String {
    let mut lines = vec![format!(
        "source={}|scope={}|sourceHash={}",
        request.source.as_str(),
        request.scope.trim(),
        sha256_hex(request.source_markdown.as_bytes())
    )];
    for row in destinations {
        lines.push(format!(
            "{}|{}|{}|{}|{}|{}|{}",
            row.destination.as_str(),
            adapt_mode_wire(row.mode),
            row.path,
            row.observed_hash.as_deref().unwrap_or("<missing>"),
            row.rendered_hash.as_deref().unwrap_or("<empty>"),
            row.can_apply,
            row.partial_blockers.join("\u{1f}"),
        ));
    }
    sha256_hex(lines.join("\n").as_bytes())
}

fn adapt_mode_wire(mode: CrossAgentAdaptMode) -> &'static str {
    match mode {
        CrossAgentAdaptMode::Shared => "shared",
        CrossAgentAdaptMode::Adapted => "adapted",
        CrossAgentAdaptMode::TargetOnly => "targetOnly",
        CrossAgentAdaptMode::Residual => "residual",
    }
}

/// 负向合同：external enqueue 过滤器不得包含其它 target。
///
/// Business Logic: D4 锁定无后台跨 target 写。
/// Code Logic: pure helper 供 unit 测试与 runtime 注释对齐。
pub fn should_enqueue_cross_target_on_external_edit(
    origin: AgentTarget,
    binding_target: AgentTarget,
) -> bool {
    origin == binding_target
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    fn temp_env(home: &Path) -> TargetEnvironment {
        let mut vars = BTreeMap::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into(),
        );
        vars.insert(
            "CODEX_HOME".into(),
            home.join(".codex").to_string_lossy().into(),
        );
        vars.insert(
            "OPENCODE_CONFIG_DIR".into(),
            home.join(".opencode").to_string_lossy().into(),
        );
        TargetEnvironment {
            home: home.to_path_buf(),
            vars,
            path_entries: vec![],
        }
    }

    #[test]
    fn external_edit_filter_blocks_other_targets() {
        assert!(should_enqueue_cross_target_on_external_edit(
            AgentTarget::Claude,
            AgentTarget::Claude
        ));
        assert!(!should_enqueue_cross_target_on_external_edit(
            AgentTarget::Claude,
            AgentTarget::Codex
        ));
        assert!(!should_enqueue_cross_target_on_external_edit(
            AgentTarget::Codex,
            AgentTarget::OpenCode
        ));
    }

    #[test]
    fn shared_instruction_preview_is_read_only_and_cli_term_stays_target_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());

        // Shared plain text → can preview真实 diff，但不能 apply 到 Codex。
        let preview = preview_cross_agent_instruction(
            &PreviewCrossAgentInstructionRequest {
                source: AgentTarget::Claude,
                destinations: vec![AgentTarget::Codex],
                source_markdown: "Always run tests before commit.\n".into(),
                scope: "user".into(),
                destination_paths: BTreeMap::new(),
            },
            &env,
        )
        .unwrap();
        assert_eq!(preview.destinations.len(), 1);
        assert!(!preview.destinations[0].can_apply);
        assert!(preview.destinations[0]
            .partial_blockers
            .iter()
            .any(|code| code == "CROSS_AGENT_PREVIEW_ONLY"));
        assert!(preview.destinations[0]
            .unified_diff
            .as_deref()
            .is_some_and(|diff| diff.contains("+Always run tests before commit.")));
        assert!(!matches!(
            preview.destinations[0].mode,
            CrossAgentAdaptMode::Residual
        ));

        let error = apply_cross_agent_instruction(
            &ApplyCrossAgentInstructionRequest {
                source: AgentTarget::Claude,
                destinations: vec![AgentTarget::Codex],
                source_markdown: "Always run tests before commit.\n".into(),
                scope: "user".into(),
                destination_paths: BTreeMap::new(),
                plan_hash: preview.plan_hash.clone(),
                client_request_id: "req-1".into(),
            },
            &env,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_APPLY_NOT_CERTIFIED"));
        assert!(!PathBuf::from(&preview.destinations[0].path).exists());

        // CLI term → targetOnly empty or needs adaptation; must not dirty-write wrong full claim
        let cli_preview = preview_cross_agent_instruction(
            &PreviewCrossAgentInstructionRequest {
                source: AgentTarget::Claude,
                destinations: vec![AgentTarget::Codex],
                source_markdown: "Read CLAUDE.md and use PreToolUse hooks under .claude/\n".into(),
                scope: "user".into(),
                destination_paths: BTreeMap::new(),
            },
            &env,
        )
        .unwrap();
        assert!(
            cli_preview.needs_adaptation
                || !cli_preview.destinations[0].can_apply
                || matches!(
                    cli_preview.destinations[0].mode,
                    CrossAgentAdaptMode::Adapted | CrossAgentAdaptMode::TargetOnly
                )
        );
    }

    #[test]
    fn plugin_cross_agent_is_always_residual_partial() {
        let row = preview_cross_agent_plugin_residual(AgentTarget::Claude, AgentTarget::Codex);
        assert!(!row.can_apply);
        assert_eq!(row.mode, CrossAgentAdaptMode::Residual);
        assert!(row.partial_blockers.iter().any(|b| b.contains("residual")));
    }

    /// Business Logic: 前端可省略 destinationPaths；IPC 不得因此反序列化失败。
    /// Code Logic: 驱动 shipped serde 合同，缺字段时 default 为空 BTreeMap。
    #[test]
    fn ipc_request_deserializes_without_destination_paths() {
        let preview_json = r#"{
            "source": "claude",
            "destinations": ["codex"],
            "sourceMarkdown": "Always run tests."
        }"#;
        let preview: PreviewCrossAgentInstructionRequest =
            serde_json::from_str(preview_json).expect("preview without destinationPaths");
        assert!(preview.destination_paths.is_empty());
        assert_eq!(preview.scope, "user");
        assert_eq!(preview.source, AgentTarget::Claude);
        assert_eq!(preview.destinations, vec![AgentTarget::Codex]);

        let apply_json = r#"{
            "source": "claude",
            "destinations": ["codex"],
            "sourceMarkdown": "Always run tests.",
            "clientRequestId": "req-1"
        }"#;
        let apply: ApplyCrossAgentInstructionRequest =
            serde_json::from_str(apply_json).expect("apply without destinationPaths");
        assert!(apply.destination_paths.is_empty());
        assert_eq!(apply.scope, "user");
        assert!(apply.plan_hash.is_empty());
        assert_eq!(apply.client_request_id, "req-1");
    }

    #[test]
    fn apply_is_blocked_and_preserves_external_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        fs::create_dir_all(tmp.path().join(".codex")).unwrap();
        let env = temp_env(tmp.path());
        let preview_request = PreviewCrossAgentInstructionRequest {
            source: AgentTarget::Claude,
            destinations: vec![AgentTarget::Codex],
            source_markdown: "Always run tests.\n".into(),
            scope: "user".into(),
            destination_paths: BTreeMap::new(),
        };
        let preview = preview_cross_agent_instruction(&preview_request, &env).unwrap();
        let target = PathBuf::from(&preview.destinations[0].path);
        fs::write(&target, "external edit\n").unwrap();

        let error = apply_cross_agent_instruction(
            &ApplyCrossAgentInstructionRequest {
                source: preview_request.source,
                destinations: preview_request.destinations,
                source_markdown: preview_request.source_markdown,
                scope: preview_request.scope,
                destination_paths: preview_request.destination_paths,
                plan_hash: preview.plan_hash,
                client_request_id: "req-stale".into(),
            },
            &env,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_APPLY_NOT_CERTIFIED"));
        assert_eq!(fs::read_to_string(target).unwrap(), "external edit\n");
    }

    #[test]
    fn caller_controlled_destination_path_is_rejected_before_read() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = temp_env(tmp.path());
        let sensitive = tmp.path().join("not-an-agent-file");
        fs::write(&sensitive, "private\n").unwrap();
        let error = preview_cross_agent_instruction(
            &PreviewCrossAgentInstructionRequest {
                source: AgentTarget::Claude,
                destinations: vec![AgentTarget::Codex],
                source_markdown: "Always run tests.\n".into(),
                scope: "user".into(),
                destination_paths: BTreeMap::from([(
                    AgentTarget::Codex,
                    sensitive.to_string_lossy().into_owned(),
                )]),
            },
            &env,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_DESTINATION_PATH_OVERRIDE_UNAVAILABLE"));
    }

    #[test]
    fn mixed_version_preview_is_scrubbed_before_ui() {
        let mut report = CrossAgentPreviewReport {
            source: AgentTarget::Claude,
            kind: CrossAgentKind::Instruction,
            destinations: vec![CrossAgentTargetPreview {
                destination: AgentTarget::Codex,
                mode: CrossAgentAdaptMode::Shared,
                path: "/tmp/target".into(),
                rendered_hash: Some("hash".into()),
                observed_hash: None,
                unified_diff: Some("--- before\n+++ after\n-12+34".into()),
                partial_blockers: vec![],
                can_apply: true,
            }],
            needs_adaptation: false,
            plan_hash: "legacy".into(),
        };
        enforce_cross_agent_preview_only(&mut report);
        let row = &report.destinations[0];
        assert!(!row.can_apply);
        assert!(row.unified_diff.is_none());
        assert!(row
            .partial_blockers
            .iter()
            .any(|code| code == "CROSS_AGENT_PREVIEW_ONLY"));
        assert!(row
            .partial_blockers
            .iter()
            .any(|code| code == "CROSS_AGENT_PREVIEW_DIFF_UNAVAILABLE"));
    }

    #[test]
    fn project_scope_fails_closed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let env = temp_env(tmp.path());
        let error = preview_cross_agent_instruction(
            &PreviewCrossAgentInstructionRequest {
                source: AgentTarget::Claude,
                destinations: vec![AgentTarget::Codex],
                source_markdown: "Always run tests.\n".into(),
                scope: "project:demo".into(),
                destination_paths: BTreeMap::new(),
            },
            &env,
        )
        .unwrap_err();
        assert!(format!("{error:?}").contains("CROSS_AGENT_PROJECT_SCOPE_UNAVAILABLE"));
    }
}
