//! remote outbox
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

use crate::error::AppError;
use crate::orchestrator::outbox::OrchestratorRemoteOutboxDto;
use crate::state::AppState;
use tauri::State;

use super::actions::{
    discard_orchestrator_remote_outbox_for_state, retry_orchestrator_remote_outbox_for_state,
};

/// 重试失败的远端 outbox。
///
/// Business Logic（为什么需要这个函数）:
///     用户在原 Automation UI 对 failed outbox 点 Retry 时，应保留原 payload/clientRequestId 并回到 pending。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 解包 State 与字符串参数，委托 state helper。
#[tauri::command]
pub async fn retry_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    retry_orchestrator_remote_outbox_for_state(state.inner(), &project_id, &outbox_id).await
}

/// 放弃失败的远端 outbox。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认放弃后，failed outbox 进入 discarded 审计终态，不再参与 dispatcher/active 列表。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 解包 State 与字符串参数，委托 state helper。
#[tauri::command]
pub async fn discard_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    discard_orchestrator_remote_outbox_for_state(state.inner(), &project_id, &outbox_id).await
}
