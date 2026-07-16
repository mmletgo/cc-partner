//! commands/workbench/fleet — LAN Agent Fleet Tauri 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端 Fleet 视图与 Project Rail 需要一次拉取跨设备只读摘要。
//!
//! Code Logic（这个模块做什么）:
//!     GuiClient 代理 `lan_fleet.snapshot`；HeadlessOwner 调 collector。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::lan_fleet::{collect_lan_fleet_for_state, LanFleetSnapshot};
use tauri::State;

use super::common::proxy_workbench_if_gui;

/// 获取控制设备 LAN Agent Fleet 快照。
///
/// Business Logic（为什么需要这个命令）:
///     前端 hook 在可见时 30s reconcile / event invalidation 后需要 owner 聚合摘要。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 代理；否则 `collect_lan_fleet_for_state`（仅 saved shortcuts）。
#[tauri::command]
pub async fn get_workbench_lan_fleet(
    state: State<'_, AppState>,
) -> Result<LanFleetSnapshot, AppError> {
    if let Some(v) =
        proxy_workbench_if_gui(state.inner(), "lan_fleet.snapshot", serde_json::json!({})).await?
    {
        return Ok(v);
    }
    collect_lan_fleet_for_state(state.inner()).await
}

/// HeadlessOwner / control 路径 helper。
///
/// Business Logic（为什么需要这个函数）:
///     control_workbench 与命令层共用同一聚合入口。
///
/// Code Logic（这个函数做什么）:
///     委托 `collect_lan_fleet_for_state`。
pub async fn get_workbench_lan_fleet_for_state(
    state: &AppState,
) -> Result<LanFleetSnapshot, AppError> {
    collect_lan_fleet_for_state(state).await
}
