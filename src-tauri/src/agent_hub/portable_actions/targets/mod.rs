//! portable_actions/targets — 按 AgentTarget 分发本机 mutation
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude / Codex / OpenCode 的启用、禁用、卸载语义不同；必须走 target adapter，
//!     禁止在 executor 内写死单一 CLI 命令。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `TargetActionContext` / `TargetActionExecutor` 合同；导出三端实现。

pub mod claude;
pub mod codex;
pub mod opencode;

pub use claude::ClaudeTargetExecutor;
pub use codex::CodexTargetExecutor;
pub use opencode::OpenCodeTargetExecutor;

use super::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::packages::activator::{ProcessOutcome, ProcessRunner};
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::error::AppError;
use std::path::PathBuf;
use std::sync::Arc;

/// 单条执行后的原始结果（尚未 rescan 对账）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetActionRawOutcome {
    /// 已执行且本地写入/CLI 返回成功
    Applied,
    /// 幂等跳过（状态已满足）
    Skipped,
    /// 前置阻断
    Blocked {
        /// 稳定错误码
        code: String,
        /// 说明（无 secret）
        message: String,
    },
    /// 明确失败
    Failed {
        /// 稳定错误码
        code: String,
        /// 说明
        message: String,
    },
    /// spawn/transport 不确定（不得标 succeeded）
    OutcomeUnknown {
        /// 稳定错误码
        code: String,
        /// 说明
        message: String,
    },
}

/// target 执行上下文（可注入 runner / 根路径）。
///
/// Business Logic（为什么需要这个结构体）:
///     单测用 FakeProcessRunner + 临时 CLAUDE_CONFIG_DIR；生产用真实 runner。
///
/// Code Logic（这个结构体做什么）:
///     持有 runner 与可选 config roots。
pub struct TargetActionContext {
    /// 进程运行器
    pub runner: Arc<dyn ProcessRunner>,
    /// Claude 配置根（对应 CLAUDE_CONFIG_DIR）；None 走默认解析
    pub claude_config_dir: Option<PathBuf>,
    /// cc-partner 数据根（disabled/backup）；None 走 config::config_dir
    pub data_dir: Option<PathBuf>,
    /// keepData（uninstall）
    pub keep_data: bool,
    /// 动作 kind
    pub action: PortableAssetActionKind,
}

/// target 执行器合同。
pub trait TargetActionExecutor: Send + Sync {
    /// 执行单条 change（不 rescan）。
    fn execute_change(
        &self,
        ctx: &TargetActionContext,
        plan: &PortableAssetActionPlanDto,
        change: &PortableAssetActionChangeDto,
        pre_item: Option<&PortableInventoryItemDto>,
    ) -> Result<TargetActionRawOutcome, AppError>;
}

/// 选择 target executor。
pub fn executor_for(target: AgentTarget) -> Box<dyn TargetActionExecutor> {
    match target {
        AgentTarget::Claude => Box::new(ClaudeTargetExecutor),
        AgentTarget::Codex => Box::new(CodexTargetExecutor),
        AgentTarget::OpenCode => Box::new(OpenCodeTargetExecutor),
        AgentTarget::Grok | AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
            Box::new(OpenCodeTargetExecutor)
        }
    }
}

/// 判断本机直管执行器是否真实覆盖指定动作。
///
/// Business Logic（为什么需要）:
///     本机库存启停/卸载与 canonical package 投影是两套能力；不能因为后者尚未完成
///     就把实现存在误当成运行时已认证；allowlist 只描述 adapter 覆盖面，最终写入仍须
///     通过 support manifest 的逐动作 capability 门禁。
///
/// Code Logic（做什么）:
///     以 target × kind × action 的显式 allowlist 对齐实际 executor；
///     Claude 与 Codex 四类 enable/disable/uninstall adapter 已实现；
///     OpenCode、Adopt、InstallToSourceTarget 仍 fail-closed。
pub fn supports_direct_local_action(
    target: AgentTarget,
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> bool {
    matches!(target, AgentTarget::Claude | AgentTarget::Codex)
        && matches!(
            kind,
            PortableAssetKind::Skill
                | PortableAssetKind::Command
                | PortableAssetKind::Plugin
                | PortableAssetKind::Mcp
        )
        && matches!(
            action,
            PortableAssetActionKind::Enable
                | PortableAssetActionKind::Disable
                | PortableAssetActionKind::Uninstall
        )
}

/// 判断 target 是否至少具备一个本机直管动作。
pub fn has_direct_local_actions(target: AgentTarget) -> bool {
    [
        PortableAssetKind::Skill,
        PortableAssetKind::Command,
        PortableAssetKind::Plugin,
        PortableAssetKind::Mcp,
    ]
    .into_iter()
    .any(|kind| supports_direct_local_action(target, kind, PortableAssetActionKind::Uninstall))
}

/// 判定 ProcessRunner 错误是否属于 spawn/transport 不确定。
pub(crate) fn is_outcome_unknown_error(err: &AppError) -> bool {
    let cat = err.ipc_category_code();
    matches!(cat, "unavailable" | "timeout" | "internal")
        || err.to_string().contains("spawn")
        || err.to_string().contains("transport")
}

/// 将 ProcessOutcome 映射为 raw outcome。
pub(crate) fn map_process_outcome(
    outcome: ProcessOutcome,
    ok_label: &str,
) -> TargetActionRawOutcome {
    if outcome.code == 0 {
        TargetActionRawOutcome::Applied
    } else {
        TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_CLI_FAILED".into(),
            message: format!(
                "{ok_label} exit={} stderr={}",
                outcome.code,
                truncate_no_secret(&outcome.stderr, 240)
            ),
        }
    }
}

fn truncate_no_secret(s: &str, max: usize) -> String {
    let cleaned = s
        .replace("token", "<redacted>")
        .replace("password", "<redacted>")
        .replace("apiKey", "<redacted>")
        .replace("secret", "<redacted>");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        cleaned.chars().take(max).collect::<String>() + "…"
    }
}

/// 变更期望的 actual_enabled（用于 rescan 对账）。
pub(crate) fn expected_enabled_after(
    action: PortableAssetActionKind,
    kind: PortableAssetKind,
    previous: Option<bool>,
) -> Option<bool> {
    match action {
        PortableAssetActionKind::Enable => Some(true),
        PortableAssetActionKind::Disable => Some(false),
        PortableAssetActionKind::Uninstall => None,
        PortableAssetActionKind::Adopt => previous,
        PortableAssetActionKind::InstallToSourceTarget => {
            if kind == PortableAssetKind::Skill || kind == PortableAssetKind::Command {
                Some(true)
            } else {
                previous.or(Some(true))
            }
        }
    }
}

#[cfg(test)]
mod direct_action_support_tests {
    use super::*;

    #[test]
    fn support_matrix_matches_real_target_executors() {
        for kind in [
            PortableAssetKind::Skill,
            PortableAssetKind::Command,
            PortableAssetKind::Plugin,
            PortableAssetKind::Mcp,
        ] {
            for action in [
                PortableAssetActionKind::Enable,
                PortableAssetActionKind::Disable,
                PortableAssetActionKind::Uninstall,
            ] {
                assert!(supports_direct_local_action(
                    AgentTarget::Claude,
                    kind,
                    action
                ));
                assert!(supports_direct_local_action(
                    AgentTarget::Codex,
                    kind,
                    action
                ));
                assert!(!supports_direct_local_action(
                    AgentTarget::OpenCode,
                    kind,
                    action
                ));
            }
            assert!(!supports_direct_local_action(
                AgentTarget::Claude,
                kind,
                PortableAssetActionKind::Adopt,
            ));
            assert!(!supports_direct_local_action(
                AgentTarget::Claude,
                kind,
                PortableAssetActionKind::InstallToSourceTarget,
            ));
        }
    }
}
