//! commands/attention.rs — Attention 快照 Tauri 命令与共享 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面 Inbox 与 Mobile HTTP 必须消费同一聚合快照；共享 helper 保证两端序列化契约一致，
//!     并避免在 command/route 各自注册不同 source 列表。
//!
//! Code Logic（这个模块做什么）:
//!     `list_attention_items_for_state` 聚合 Orchestrator + Workbench dependency source；
//!     Tauri command `list_attention_items` 仅做 State 注入转发。

use crate::attention::aggregator::aggregate_attention_sources;
use crate::attention::models::AttentionSnapshotDto;
use crate::attention::orchestrator_source::OrchestratorAttentionSource;
use crate::attention::workbench_dependency_source::WorkbenchDependencyAttentionSource;
use crate::error::AppError;
use crate::state::AppState;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     Tauri 与 Mobile HTTP 必须共享完全相同的 source 集合与聚合语义，
///     否则桌面/手机 badge 与列表会漂移。
///
/// Code Logic（这个函数做什么）:
///     固定注册 OrchestratorAttentionSource 与 WorkbenchDependencyAttentionSource，
///     调用聚合器生成快照；任一 source 失败整次失败。
pub async fn list_attention_items_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError> {
    let orchestrator = OrchestratorAttentionSource;
    let dependency = WorkbenchDependencyAttentionSource;
    let sources: [&dyn crate::attention::source::AttentionSource; 2] =
        [&orchestrator, &dependency];
    aggregate_attention_sources(state, &sources).await
}

/// Business Logic（为什么需要这个函数）:
///     桌面前端通过 invoke 拉取全局 Inbox 快照，用于 Provider 与 badge。
///
/// Code Logic（这个函数做什么）:
///     委托 `list_attention_items_for_state`，返回 camelCase AttentionSnapshotDto。
#[tauri::command]
pub async fn list_attention_items(
    state: State<'_, AppState>,
) -> Result<AttentionSnapshotDto, AppError> {
    list_attention_items_for_state(&state).await
}
