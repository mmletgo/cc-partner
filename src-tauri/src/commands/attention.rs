//! commands/attention.rs — Attention 快照 Tauri 命令与共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面 Inbox 与 Mobile HTTP 必须消费同一聚合快照；v1 保持旧枚举，v2 增加 Agent 投影。
//!     Agent Hub conflict/blocked 同时进入 v1 与 v2。
//!
//! Code Logic（这个模块做什么）:
//!     `list_attention_items_for_state` = v1（Orchestrator + Dependency + AgentHub）；
//!     `list_attention_items_v2_for_state` 再追加 AgentRuntime + Experiment；
//!     Tauri commands 分别暴露。

use crate::attention::agent_hub_source::AgentHubAttentionSource;
use crate::attention::agent_runtime_source::AgentRuntimeAttentionSource;
use crate::attention::aggregator::aggregate_attention_sources;
use crate::attention::experiment_source::ExperimentAttentionSource;
use crate::attention::models::{AttentionCategory, AttentionSnapshotDto};
use crate::attention::orchestrator_source::OrchestratorAttentionSource;
use crate::attention::workbench_dependency_source::WorkbenchDependencyAttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::maintenance_gate::begin_shared_write;
use chrono::Utc;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     Tauri 与 Mobile HTTP v1 必须共享完全相同的 source 集合（无 Agent/Experiment，含 Agent Hub）。
///
/// Code Logic（这个函数做什么）:
///     固定注册 Orchestrator + WorkbenchDependency + AgentHub；不含 AgentRuntimeAttentionSource。
pub async fn list_attention_items_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let orchestrator = OrchestratorAttentionSource;
    let dependency = WorkbenchDependencyAttentionSource;
    let agent_hub = AgentHubAttentionSource;
    let sources: [&dyn crate::attention::source::AttentionSource; 3] =
        [&orchestrator, &dependency, &agent_hub];
    aggregate_attention_sources(state, &sources).await
}

/// Business Logic（为什么需要这个函数）:
///     attention.v2 在 v1 源基础上追加 Agent needsInput/failed 与 experiment NeedsDecision。
///
/// Code Logic（这个函数做什么）:
///     Orchestrator + Dependency + AgentRuntime + Experiment + AgentHub。
pub async fn list_attention_items_v2_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let orchestrator = OrchestratorAttentionSource;
    let dependency = WorkbenchDependencyAttentionSource;
    let agent = AgentRuntimeAttentionSource;
    let experiment = ExperimentAttentionSource;
    let agent_hub = AgentHubAttentionSource;
    let sources: [&dyn crate::attention::source::AttentionSource; 5] =
        [&orchestrator, &dependency, &agent, &experiment, &agent_hub];
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

/// Mark-read 前置：去重 item_ids 并校验非空。
///
/// Business Logic（为什么需要这个函数）:
///     4 个 mark-read 命令（mark_items_read / mark_items_unread / mark_all_read /
///     mark_category_read）都需要校验 item_ids 列表，避免空请求触发无效仓储调用。
///
/// Code Logic（字段说明）:
///     非空 → 校验；返回 Ok 继续；空 → AppError::validation，向上抛 400。
fn validate_item_ids(item_ids: &[String]) -> Result<Vec<String>, AppError> {
    if item_ids.is_empty() {
        return Err(AppError::validation("item_ids 不能为空"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::with_capacity(item_ids.len());
    for id in item_ids {
        if !id.is_empty() && seen.insert(id.clone()) {
            deduped.push(id.clone());
        }
    }
    if deduped.is_empty() {
        return Err(AppError::validation("item_ids 全部为空"));
    }
    Ok(deduped)
}

/// 在事务内写已读/未读标记（mark all / mark category 也用此 helper）。
///
/// Business Logic（为什么需要这个函数）:
///     mark-read 是状态变更，必须在 single-flight + shared write 事务内完成；
///     写完后立即 re-aggregate 返回新 snapshot，调用方无需 refresh。
///
/// Code Logic（字段说明）:
///     `op`: `mark_read` / `mark_unread`；`item_ids` 必填；先 take shared write lease
///     再开 tx，写完后 commit、aggregate_attention_items_v2 走 v1 fallback 复用
///     既有 source 链（命令侧不区分 v1/v2，返回的 snapshot 包含完整 items + counts）。
async fn write_attention_read_state(
    state: &AppState,
    op: MarkReadOp,
    item_ids: &[String],
) -> Result<AttentionSnapshotDto, AppError> {
    let device_id = state.device_id.as_str().to_string();
    let deduped = validate_item_ids(item_ids)?;
    let (permit, mut tx) = begin_shared_write(&state.db, &state.maintenance_gate).await?;
    let read_at = Utc::now().to_rfc3339();
    let written = match op {
        MarkReadOp::MarkRead => {
            crate::storage::AttentionReadRepo::mark_read_on_tx(
                &mut tx, &device_id, &deduped, &read_at,
            )
            .await?
        }
        MarkReadOp::MarkUnread => {
            crate::storage::AttentionReadRepo::mark_unread_on_tx(&mut tx, &device_id, &deduped)
                .await?
        }
    };
    let _ = written; // 写入行数仅用于观测；事务成功即视为已写
    tx.commit().await?;
    drop(permit);
    let outbound = crate::sync::attention_read_apply::local_attention_read_items(
        &device_id,
        &deduped,
        match op {
            MarkReadOp::MarkRead => crate::sync::attention_read_apply::AttentionReadOp::Read,
            MarkReadOp::MarkUnread => crate::sync::attention_read_apply::AttentionReadOp::Unread,
        },
        &read_at,
    );
    crate::sync::attention_read_apply::spawn_push_attention_read_to_peers(state.clone(), outbound);
    // 重新聚合；写已读成功后再走一遍整套 source 链（含 plugin 投影、远端 mirror）
    // 让本次变化的 read_at 反映在 snapshot 上。
    list_attention_items_v2_for_state(state).await
}

#[derive(Debug, Clone, Copy)]
enum MarkReadOp {
    MarkRead,
    MarkUnread,
}

/// Business Logic（为什么需要这个函数）:
///     Tauri 与 Mobile HTTP 必须共享同一套 mark-read 写路径。
///
/// Code Logic（这个函数做什么）:
///     委托 write_attention_read_state。
pub async fn mark_attention_items_read_for_state(
    state: &AppState,
    item_ids: Vec<String>,
) -> Result<AttentionSnapshotDto, AppError> {
    write_attention_read_state(state, MarkReadOp::MarkRead, &item_ids).await
}

/// Business Logic（为什么需要这个函数）:
///     撤销已读与标已读走同一事务 helper。
///
/// Code Logic（这个函数做什么）:
///     委托 write_attention_read_state(MarkUnread)。
pub async fn mark_attention_items_unread_for_state(
    state: &AppState,
    item_ids: Vec<String>,
) -> Result<AttentionSnapshotDto, AppError> {
    write_attention_read_state(state, MarkReadOp::MarkUnread, &item_ids).await
}

/// Business Logic（为什么需要这个函数）:
///     全部已读先聚合再写，避免误删历史 read_set。
///
/// Code Logic（这个函数做什么）:
///     v2 聚合后对全部 item_id 调 mark_read。
pub async fn mark_all_attention_items_read_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let snapshot = list_attention_items_v2_for_state(state).await?;
    let item_ids: Vec<String> = snapshot.items.iter().map(|it| it.id.clone()).collect();
    write_attention_read_state(state, MarkReadOp::MarkRead, &item_ids).await
}

/// Business Logic（为什么需要这个函数）:
///     按分类已读只覆盖当前快照该分类的稳定 ID。
///
/// Code Logic（这个函数做什么）:
///     过滤 category 后 mark_read。
pub async fn mark_attention_category_read_for_state(
    state: &AppState,
    category: AttentionCategory,
) -> Result<AttentionSnapshotDto, AppError> {
    let snapshot = list_attention_items_v2_for_state(state).await?;
    let item_ids: Vec<String> = snapshot
        .items
        .iter()
        .filter(|it| it.category == category)
        .map(|it| it.id.clone())
        .collect();
    write_attention_read_state(state, MarkReadOp::MarkRead, &item_ids).await
}

/// 业务逻辑：标记指定 item_ids 为本设备已读。
#[tauri::command]
pub async fn mark_attention_items_read(
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Result<AttentionSnapshotDto, AppError> {
    mark_attention_items_read_for_state(&state, item_ids).await
}

/// 业务逻辑：撤销本设备对指定 item_ids 的已读标记。
#[tauri::command]
pub async fn mark_attention_items_unread(
    state: State<'_, AppState>,
    item_ids: Vec<String>,
) -> Result<AttentionSnapshotDto, AppError> {
    mark_attention_items_unread_for_state(&state, item_ids).await
}

/// 业务逻辑：标记当前聚合快照全部条目为已读。
/// 必须先聚合一次拿当前 item_ids 集合（不能直接 "DELETE FROM read_by_device
/// WHERE device_id" 撤销历史），保证行为可逆。
#[tauri::command]
pub async fn mark_all_attention_items_read(
    state: State<'_, AppState>,
) -> Result<AttentionSnapshotDto, AppError> {
    mark_all_attention_items_read_for_state(&state).await
}

#[tauri::command]
pub async fn mark_attention_category_read(
    state: State<'_, AppState>,
    category: AttentionCategory,
) -> Result<AttentionSnapshotDto, AppError> {
    mark_attention_category_read_for_state(&state, category).await
}

#[cfg(test)]
mod mark_read_tests {
    use super::*;
    use crate::attention::models::{
        AttentionCategory, AttentionFreshness, AttentionItemDto, AttentionSourceKind,
        AttentionTargetDto,
    };

    /// 业务逻辑：去重 helper 拒绝空列表与重复项。
    #[test]
    fn validate_item_ids_rejects_empty_and_dedupes() {
        assert!(validate_item_ids(&[]).is_err());
        assert!(validate_item_ids(&["".to_string()]).is_err());
        let ok = validate_item_ids(&["a".into(), "b".into(), "a".into(), "".into()]).unwrap();
        assert_eq!(ok, vec!["a".to_string(), "b".to_string()]);
    }

    /// 业务逻辑：构造 sample item 用于序列化断言。
    fn sample_item(id: &str) -> AttentionItemDto {
        AttentionItemDto {
            id: id.to_string(),
            category: AttentionCategory::Decision,
            source_kind: AttentionSourceKind::OrchestratorHumanReview,
            title: "x".into(),
            summary: "y".into(),
            updated_at: "2026-08-16T00:00:00Z".into(),
            freshness: AttentionFreshness::Live,
            cached_at: None,
            project: None,
            device: None,
            target: AttentionTargetDto::OrchestratorTask {
                project_id: "p".into(),
                task_id: id.to_string(),
            },
            read_at: None,
        }
    }

    /// 序列化契约：read_at 字段以 camelCase 序列化为 `readAt`，None 时省略。
    #[test]
    fn read_at_serializes_camel_case_and_omits_none() {
        let item = sample_item("a");
        let json = serde_json::to_string(&item).unwrap();
        assert!(!json.contains("readAt"), "None 应省略,实际 json={json}");
        let mut read = sample_item("b");
        read.read_at = Some("2026-08-16T10:00:00Z".to_string());
        let json2 = serde_json::to_string(&read).unwrap();
        assert!(json2.contains("\"readAt\":\"2026-08-16T10:00:00Z\""));
    }
}

#[cfg(test)]
mod tests {
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
            read_at: None,
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
