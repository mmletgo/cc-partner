//! commands/workbench/agent_runtime — Agent runtime snapshot Tauri 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端 Gap 恢复与页面进入需要拉取 active Agent session baseline。
//!
//! Code Logic（这个模块做什么）:
//!     GUI 代理到 control；owner 本地调用 snapshot helper。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_runtime::snapshot::{
    get_agent_runtime_snapshot_for_state, AgentRuntimeSnapshot,
};
use tauri::State;

use super::common::proxy_workbench_if_gui;

/// 获取 Agent runtime 有界 snapshot。
///
/// Business Logic（为什么需要这个命令）:
///     前端在 Gap/进入项目时需要 owner 权威的 active Agent 列表。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 代理 `agent_runtime.snapshot`；HeadlessOwner 读本地 helper。
#[tauri::command]
pub async fn get_agent_runtime_snapshot(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<AgentRuntimeSnapshot, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "agent_runtime.snapshot",
        serde_json::json!({ "projectId": project_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_agent_runtime_snapshot_for_state(state.inner(), project_id).await
}
