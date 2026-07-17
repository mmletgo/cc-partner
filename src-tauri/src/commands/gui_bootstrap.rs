//! commands/gui_bootstrap.rs — LAN 风险披露与延迟 sidecar 启动的 Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 App 级 gate 需要读取披露状态，并在用户确认后原子写 bootstrap 且启动 sidecar；
//!     设置页「重置首次启动引导」需清 bootstrap 并停止 sidecar。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 `get_lan_disclosure_status`、`acknowledge_lan_disclosure_and_start_backend`
//!     与 `reset_onboarding_gates`；前两者委托 `GuiStartupCoordinator`，重置走 bootstrap + stop。

use crate::backend::control::BackendStatusKind;
use crate::error::AppError;
use crate::gui_bootstrap;
use crate::gui_startup::{
    GuiStartupCoordinator, LanDisclosureStartResult, LanDisclosureStatus,
    ProductionBackendLifecycle,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

/// 重置 onboarding 门闩的结果。
///
/// Business Logic（为什么需要这个结构）:
///     设置页需展示是否清了 LAN 披露、是否停止了 backend。
///
/// Code Logic（这个结构做什么）:
///     camelCase DTO：ok / lanDisclosureReset / backendStopped。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResetOnboardingGatesResult {
    pub ok: bool,
    pub lan_disclosure_reset: bool,
    pub backend_stopped: bool,
}

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

/// 重置 onboarding 门闩：清 LAN 披露确认并停止 backend。
///
/// Business Logic（为什么需要这个函数）:
///     用户希望下次启动复现首次 LAN 披露与权限 Welcome，但不删除 Prompt/速记本等业务数据。
///
/// Code Logic（这个函数做什么）:
///     1) `reset_lan_disclosure` 原子写 default bootstrap（失败则不 stop）；
///     2) 调用 `stop_backend_process`（幂等）；
///     3) 返回 `{ok, lanDisclosureReset, backendStopped}`。不碰 data.db / localStorage。
#[tauri::command]
pub async fn reset_onboarding_gates(app: AppHandle) -> Result<ResetOnboardingGatesResult, AppError> {
    gui_bootstrap::reset_lan_disclosure()?;
    let status = crate::commands::backend::stop_backend_process(app).await?;
    let backend_stopped = matches!(status.kind, BackendStatusKind::Stopped);
    Ok(ResetOnboardingGatesResult {
        ok: true,
        lan_disclosure_reset: true,
        backend_stopped,
    })
}
