//! commands/attention.rs — Attention 快照 Tauri 命令与共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面 Inbox 与 Mobile HTTP 必须消费同一聚合快照；v1 保持旧枚举，v2 增加 Agent 投影。
//!
//! Code Logic（这个模块做什么）:
//!     `list_attention_items_for_state` = v1（无 Agent source）；
//!     `list_attention_items_v2_for_state` 追加 AgentRuntimeAttentionSource；
//!     Tauri commands 分别暴露。

use crate::attention::agent_runtime_source::AgentRuntimeAttentionSource;
use crate::attention::aggregator::aggregate_attention_sources;
use crate::attention::experiment_source::ExperimentAttentionSource;
use crate::attention::models::AttentionSnapshotDto;
use crate::attention::orchestrator_source::OrchestratorAttentionSource;
use crate::attention::workbench_dependency_source::WorkbenchDependencyAttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     Tauri 与 Mobile HTTP v1 必须共享完全相同的 source 集合（无 Agent/Experiment）。
///
/// Code Logic（这个函数做什么）:
///     固定注册 Orchestrator + WorkbenchDependency；不含 AgentRuntimeAttentionSource。
pub async fn list_attention_items_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let orchestrator = OrchestratorAttentionSource;
    let dependency = WorkbenchDependencyAttentionSource;
    let sources: [&dyn crate::attention::source::AttentionSource; 2] = [&orchestrator, &dependency];
    aggregate_attention_sources(state, &sources).await
}

/// Business Logic（为什么需要这个函数）:
///     attention.v2 在 v1 源基础上追加 Agent needsInput/failed 与 experiment NeedsDecision。
///
/// Code Logic（这个函数做什么）:
///     Orchestrator + Dependency + AgentRuntime + Experiment。
pub async fn list_attention_items_v2_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let orchestrator = OrchestratorAttentionSource;
    let dependency = WorkbenchDependencyAttentionSource;
    let agent = AgentRuntimeAttentionSource;
    let experiment = ExperimentAttentionSource;
    let sources: [&dyn crate::attention::source::AttentionSource; 4] =
        [&orchestrator, &dependency, &agent, &experiment];
    aggregate_attention_sources(state, &sources).await
}

/// Business Logic（为什么需要这个函数）:
///     桌面前端默认 invoke v1 兼容旧路径；新客户端应调用 v2 命令。
///
/// Code Logic（这个函数做什么）:
///     委托 list_attention_items_for_state。
#[tauri::command]
pub async fn list_attention_items(
    state: State<'_, AppState>,
) -> Result<AttentionSnapshotDto, AppError> {
    list_attention_items_for_state(&state).await
}

/// Business Logic（为什么需要这个函数）:
///     桌面 Inbox 需要 Agent 异常投影时调用 v2。
///
/// Code Logic（这个函数做什么）:
///     委托 list_attention_items_v2_for_state。
#[tauri::command]
pub async fn list_attention_items_v2(
    state: State<'_, AppState>,
) -> Result<AttentionSnapshotDto, AppError> {
    list_attention_items_v2_for_state(&state).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attention::agent_runtime_source::project_agent_runtime_rows;
    use crate::attention::aggregator::aggregate_attention_item_batches;
    use crate::attention::models::{
        AttentionCategory, AttentionFreshness, AttentionItemDto, AttentionSourceKind,
        AttentionTargetDto,
    };
    use crate::workbench::agent_runtime::models::{AgentSessionPhase, AgentSessionRuntime};
    use std::collections::HashMap;

    fn agent_item(id_suffix: &str, phase: AgentSessionPhase) -> AttentionItemDto {
        let rows = [AgentSessionRuntime {
            id: id_suffix.to_string(),
            project_id: "p".into(),
            worktree_id: None,
            terminal_session_id: "t".into(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: None,
            phase,
            version: 1,
            started_at: "2026-07-15T00:00:00Z".into(),
            last_activity_at: "2026-07-15T00:00:00Z".into(),
            ended_at: None,
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: true,
        }];
        project_agent_runtime_rows(&rows, &HashMap::new(), "d", "dev")
            .into_iter()
            .next()
            .expect("agent item")
    }

    /// Business Logic: v1 批次不得含 agent: 前缀；v2 批次含。
    #[test]
    fn attention_v1_never_serializes_agent_variants() {
        let v1_only = vec![AttentionItemDto {
            id: "orchestrator:human-review:t1".into(),
            category: AttentionCategory::Decision,
            source_kind: AttentionSourceKind::OrchestratorHumanReview,
            title: "复核".into(),
            summary: "s".into(),
            updated_at: "2026-07-15T00:00:00Z".into(),
            freshness: AttentionFreshness::Live,
            cached_at: None,
            project: None,
            device: None,
            target: AttentionTargetDto::OrchestratorTask {
                project_id: "p".into(),
                task_id: "t1".into(),
            },
        }];
        let agent = agent_item("a1", AgentSessionPhase::NeedsInput);
        let v1 = aggregate_attention_item_batches(vec![Ok(v1_only.clone())]).unwrap();
        assert!(v1.items.iter().all(|item| !item.id.starts_with("agent:")));
        assert!(v1.items.iter().all(|item| !item.source_kind.is_v2_only()));

        let v2 = aggregate_attention_item_batches(vec![Ok(v1_only), Ok(vec![agent])]).unwrap();
        assert_eq!(
            v2.items
                .iter()
                .filter(|item| item.id.starts_with("agent:"))
                .count(),
            1
        );
        let json = serde_json::to_string(&v1).unwrap();
        assert!(!json.contains("agentNeedsInput"));
        assert!(!json.contains("agentSession"));
    }
}
