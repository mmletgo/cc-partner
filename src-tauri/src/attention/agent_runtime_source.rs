//! attention/agent_runtime_source.rs — Agent needsInput/failed 的实时 Attention 投影（v2）。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要在 Inbox 看到等待输入与失败的 Agent，并只导航到既有 terminal；
//!     正常 working/idle/completed 不得制造噪音；不新增 Attention 持久表。
//!
//! Code Logic（这个模块做什么）:
//!     从 workbench_agent_session_repo.list_active 实时派生；
//!     稳定 ID `agent:needs-input:<id>` / `agent:failed:<id>`；
//!     ExperimentNeedsDecision 仅定义 contract helper，A2 不查询 experiment repo。

use crate::attention::models::{
    AttentionCategory, AttentionDeviceRef, AttentionFreshness, AttentionItemDto,
    AttentionProjectKind, AttentionProjectRef, AttentionSourceKind, AttentionTargetDto,
};
use crate::attention::source::AttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_runtime::models::{AgentSessionPhase, AgentSessionRuntime};
use crate::workbench::models::WorkbenchProjectRow;
use futures_util::future::BoxFuture;
use std::collections::HashMap;

/// Agent runtime Attention 投影源（仅应注册到 attention.v2 聚合）。
///
/// Business Logic（为什么需要这个结构体）:
///     聚合器通过统一 AttentionSource 收集 Agent 异常，页面只导航。
///
/// Code Logic（这个结构体做什么）:
///     无状态；collect 读 active sessions + projects 元数据。
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentRuntimeAttentionSource;

impl AttentionSource for AgentRuntimeAttentionSource {
    /// Business Logic（为什么需要这个函数）:
    ///     v2 Inbox 需要当前 needsInput/failed Agent 条目，phase 离开后自动消失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     list_active → 过滤 phase → 映射 project ref → AttentionItemDto。
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>> {
        Box::pin(async move { collect_agent_runtime_attention_items(state).await })
    }
}

/// Business Logic（为什么需要这个函数）:
///     桌面/Mobile v2 共用同一 Agent 投影入口。
///
/// Code Logic（这个函数做什么）:
///     拉取 active sessions 与 projects，调用纯投影。
pub async fn collect_agent_runtime_attention_items(
    state: &AppState,
) -> Result<Vec<AttentionItemDto>, AppError> {
    let rows = state
        .workbench_agent_session_repo
        .list_active(None, 1_000)
        .await?;
    let projects = state.workbench_project_repo.list().await?;
    let project_by_id: HashMap<String, WorkbenchProjectRow> = projects
        .into_iter()
        .map(|p| (p.id.clone(), p))
        .collect();
    let device_id = state.device_id.clone();
    let device_name = {
        let cfg = state.config.read().expect("config lock");
        cfg.device_name.clone()
    };
    Ok(project_agent_runtime_rows(
        &rows,
        &project_by_id,
        &device_id,
        &device_name,
    ))
}

/// Business Logic（为什么需要这个函数）:
///     纯函数便于单测 phase 过滤与稳定 ID，不依赖完整 AppState。
///
/// Code Logic（这个函数做什么）:
///     仅 NeedsInput/Failed；其余 phase 跳过。
pub fn project_agent_runtime_rows(
    rows: &[AgentSessionRuntime],
    project_by_id: &HashMap<String, WorkbenchProjectRow>,
    device_id: &str,
    device_name: &str,
) -> Vec<AttentionItemDto> {
    let mut items = Vec::new();
    for row in rows {
        if let Some(item) = project_single_agent_row(row, project_by_id, device_id, device_name) {
            items.push(item);
        }
    }
    items
}

/// Business Logic（为什么需要这个函数）:
///     单 session 映射到 0/1 条 Attention。
///
/// Code Logic（这个函数做什么）:
///     NeedsInput → decision/agentNeedsInput；Failed → blocked/agentFailed。
fn project_single_agent_row(
    row: &AgentSessionRuntime,
    project_by_id: &HashMap<String, WorkbenchProjectRow>,
    device_id: &str,
    device_name: &str,
) -> Option<AttentionItemDto> {
    let (source_kind, category, title, summary, id_prefix) = match row.phase {
        AgentSessionPhase::NeedsInput => (
            AttentionSourceKind::AgentNeedsInput,
            AttentionCategory::Decision,
            "Agent 等待输入".to_string(),
            "有 Agent 会话正在等待你的输入".to_string(),
            "agent:needs-input:",
        ),
        AgentSessionPhase::Failed => (
            AttentionSourceKind::AgentFailed,
            AttentionCategory::Blocked,
            "Agent 运行失败".to_string(),
            "有 Agent 会话运行失败，请到终端查看".to_string(),
            "agent:failed:",
        ),
        AgentSessionPhase::Launching
        | AgentSessionPhase::Working
        | AgentSessionPhase::Idle
        | AgentSessionPhase::Completed
        | AgentSessionPhase::Disconnected => return None,
    };

    let project = project_by_id.get(&row.project_id).map(|p| AttentionProjectRef {
        id: p.id.clone(),
        name: p.name.clone(),
        kind: if p.kind == "remote" {
            AttentionProjectKind::Remote
        } else {
            AttentionProjectKind::Local
        },
    });

    Some(AttentionItemDto {
        id: format!("{id_prefix}{}", row.id),
        category,
        source_kind,
        title,
        summary,
        updated_at: row.last_activity_at.clone(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project,
        device: Some(AttentionDeviceRef {
            id: device_id.to_string(),
            name: device_name.to_string(),
        }),
        target: AttentionTargetDto::AgentSession {
            project_id: row.project_id.clone(),
            worktree_id: row.worktree_id.clone(),
            terminal_session_id: row.terminal_session_id.clone(),
            agent_session_id: row.id.clone(),
        },
    })
}

/// Business Logic（为什么需要这个函数）:
///     A4 注册 experiment source 前，A2 需锁定稳定 ID 合同 `experiment:decision:<id>`。
///
/// Code Logic（这个函数做什么）:
///     纯构造 ExperimentNeedsDecision 条目形状（不查库）；供 A4 与契约测试复用。
pub fn experiment_decision_item_contract(
    project_id: &str,
    experiment_id: &str,
    project_name: &str,
    updated_at: &str,
) -> AttentionItemDto {
    AttentionItemDto {
        id: format!("experiment:decision:{experiment_id}"),
        category: AttentionCategory::Decision,
        source_kind: AttentionSourceKind::ExperimentNeedsDecision,
        title: "实验需要决策".to_string(),
        summary: "有实验无法产生唯一结果，需要你选择".to_string(),
        updated_at: updated_at.to_string(),
        freshness: AttentionFreshness::Live,
        cached_at: None,
        project: Some(AttentionProjectRef {
            id: project_id.to_string(),
            name: project_name.to_string(),
            kind: AttentionProjectKind::Local,
        }),
        device: None,
        target: AttentionTargetDto::Experiment {
            project_id: project_id.to_string(),
            experiment_id: experiment_id.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::agent_runtime::models::AgentSessionPhase;

    /// Business Logic: 构造最小 Agent runtime 行。
    fn row(phase: AgentSessionPhase, id: &str) -> AgentSessionRuntime {
        AgentSessionRuntime {
            id: id.to_string(),
            project_id: "p1".to_string(),
            worktree_id: Some("wt1".to_string()),
            terminal_session_id: "t1".to_string(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".to_string(),
            native_session_id: Some("native-secret".to_string()),
            phase,
            version: 2,
            started_at: "2026-07-15T00:00:00Z".to_string(),
            last_activity_at: "2026-07-15T01:00:00Z".to_string(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: !phase.is_terminal() || matches!(phase, AgentSessionPhase::Failed),
        }
    }

    #[test]
    fn only_needs_input_and_failed_project() {
        let projects = HashMap::new();
        let items = project_agent_runtime_rows(
            &[
                row(AgentSessionPhase::Working, "a-work"),
                row(AgentSessionPhase::NeedsInput, "a-input"),
                row(AgentSessionPhase::Failed, "a-fail"),
                row(AgentSessionPhase::Completed, "a-done"),
                row(AgentSessionPhase::Idle, "a-idle"),
            ],
            &projects,
            "dev",
            "Local",
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "agent:needs-input:a-input");
        assert_eq!(items[0].source_kind, AttentionSourceKind::AgentNeedsInput);
        assert_eq!(items[1].id, "agent:failed:a-fail");
        assert_eq!(items[1].source_kind, AttentionSourceKind::AgentFailed);
        // 不得泄漏 native session
        let json = serde_json::to_string(&items).unwrap();
        assert!(!json.contains("native-secret"));
    }

    #[test]
    fn experiment_contract_stable_id() {
        let item = experiment_decision_item_contract("p", "e1", "demo", "2026-07-15T00:00:00Z");
        assert_eq!(item.id, "experiment:decision:e1");
        assert_eq!(
            item.source_kind,
            AttentionSourceKind::ExperimentNeedsDecision
        );
        assert!(matches!(
            item.target,
            AttentionTargetDto::Experiment {
                experiment_id,
                ..
            } if experiment_id == "e1"
        ));
    }
}
