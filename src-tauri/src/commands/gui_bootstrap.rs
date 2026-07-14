//! commands/gui_bootstrap.rs — LAN 风险披露与延迟 sidecar 启动的 Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 App 级 gate 需要读取披露状态，并在用户确认后原子写 bootstrap 且启动 sidecar。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 `get_lan_disclosure_status` 与 `acknowledge_lan_disclosure_and_start_backend`，
//!     委托 `GuiStartupCoordinator`（managed state）。

use crate::error::AppError;
use crate::gui_startup::{
    GuiStartupCoordinator, LanDisclosureStartResult, LanDisclosureStatus, ProductionBackendLifecycle,
};
use tauri::State;

/// 读取 LAN 披露状态。
///
/// Business Logic（为什么需要这个函数）:
///     前端 gate 挂载时决定 loading/required/pass，并展示地址与已运行 CLI 信息。
///
/// Code Logic（这个函数做什么）:
///     委托 managed `GuiStartupCoordinator::get_status`。
#[tauri::command]
pub async fn get_lan_disclosure_status(
    coordinator: State<'_, GuiStartupCoordinator<ProductionBackendLifecycle>>,
) -> Result<LanDisclosureStatus, AppError> {
    coordinator.get_status(None).await
}

/// 确认 LAN 披露并启动后端。
///
/// Business Logic（为什么需要这个函数）:
///     用户显式确认后才 ensure sidecar 与 GUI browse 服务；并发确认只启动一次。
///
/// Code Logic（这个函数做什么）:
///     委托 `acknowledge_and_start`：写 bootstrap → ensure → start_gui_services（once gate）。
#[tauri::command]
pub async fn acknowledge_lan_disclosure_and_start_backend(
    coordinator: State<'_, GuiStartupCoordinator<ProductionBackendLifecycle>>,
) -> Result<LanDisclosureStartResult, AppError> {
    coordinator.acknowledge_and_start().await
}
