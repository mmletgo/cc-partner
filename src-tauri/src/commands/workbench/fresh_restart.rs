//! commands/workbench/fresh_restart.rs — 设备级「全新启动连接」命令层
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在工作台对当前设备（本机或远端）触发"全新启动连接"时，前端经 Tauri
//!     invoke 进入本模块；GuiClient 必须代理到 sidecar owner，远端设备必须经 P2P
//!     在 owning device 就地执行，控制端绝不本地误杀。
//!
//! Code Logic（这个模块做什么）:
//!     preview/execute 两个 thin command：GuiClient 一律 `proxy_workbench_if_gui`
//!     到 sidecar（本机也不得在 GUI 进程直连 owner 核心）；sidecar/headless 的
//!     for_state 再按 deviceId 分流——外机 `RemoteWorkbenchClient` P2P，本机
//!     `workbench::fresh_restart` 核心（require_owner）。

use crate::commands::workbench::{device_base_url, proxy_workbench_if_gui};
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::fresh_restart::{
    preview_workbench_fresh_restart_for_state as preview_local_fresh_restart,
    run_workbench_fresh_restart_for_state as run_local_fresh_restart,
    WorkbenchFreshRestartPreviewDto, WorkbenchFreshRestartResultDto,
};
use crate::workbench::remote_client::RemoteWorkbenchClient;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     for_state 需要区分本机核心路径与对端 P2P；空或等于本机 id 视为本机。
///
/// Code Logic（这个函数做什么）:
///     空/等于本机 deviceId → false；否则 true。
fn is_foreign_device(state: &AppState, device_id: Option<&str>) -> bool {
    let device_id = device_id.unwrap_or("").trim();
    !device_id.is_empty() && device_id != state.device_id.as_str()
}

/// 预检「全新启动连接」影响面（只读）。
///
/// Business Logic（为什么需要这个函数）:
///     终止整台设备的工作台终端是不可逆动作，确认弹窗需要准确展示将被终止的
///     会话清单、非工作台 tmux 会话数与 loopback ssh 引导通道可用性。
///
/// Code Logic（这个函数做什么）:
///     GuiClient 一律代理到 sidecar；sidecar/headless 走 for_state。
#[tauri::command]
pub async fn preview_workbench_fresh_restart(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchFreshRestartPreviewDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "workbench.fresh-restart-preview",
        serde_json::json!({ "deviceId": device_id }),
    )
    .await?
    {
        return Ok(v);
    }
    preview_workbench_fresh_restart_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke / P2P route 共享预检；远端必须读 owning device 的权威清单。
///
/// Code Logic（这个函数做什么）:
///     外机 → `RemoteWorkbenchClient::fresh_restart_preview`；本机 → 核心预检。
pub async fn preview_workbench_fresh_restart_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchFreshRestartPreviewDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return client.fresh_restart_preview(&base_url).await;
    }
    preview_local_fresh_restart(state).await
}

/// 执行设备级「全新启动连接」。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认后关闭该设备全部工作台终端会话并以全新登录环境重启 tmux server，
///     让系统级变更（组/limits/profile）在新终端生效；结果如实标注降级原因。
///
/// Code Logic（这个函数做什么）:
///     GuiClient 一律代理到 sidecar；sidecar/headless 走 for_state。
#[tauri::command]
pub async fn run_workbench_fresh_restart(
    state: State<'_, AppState>,
    device_id: Option<String>,
    include_foreign_sessions: Option<bool>,
) -> Result<WorkbenchFreshRestartResultDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "workbench.fresh-restart",
        serde_json::json!({
            "deviceId": device_id,
            "includeForeignSessions": include_foreign_sessions.unwrap_or(false),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    run_workbench_fresh_restart_for_state(state.inner(), device_id, include_foreign_sessions).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke / P2P route 共享执行；远端必须在 owning device 就地杀会话。
///
/// Code Logic（这个函数做什么）:
///     外机 → `RemoteWorkbenchClient::fresh_restart`（VeryLong 超时）；本机 → 核心执行。
pub async fn run_workbench_fresh_restart_for_state(
    state: &AppState,
    device_id: Option<String>,
    include_foreign_sessions: Option<bool>,
) -> Result<WorkbenchFreshRestartResultDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return client
            .fresh_restart(&base_url, include_foreign_sessions.unwrap_or(false))
            .await;
    }
    run_local_fresh_restart(state, include_foreign_sessions.unwrap_or(false)).await
}

#[cfg(test)]
mod tests {
    /// 截取 Tauri command 函数体到下一个 `pub async fn`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GuiClient 必须无条件把 preview/execute 代理到 sidecar；只断言文件里
    ///     出现 `proxy_workbench_if_gui` 拦不住「仅外机才代理、本机 GUI 直跑」。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用带 `(` 的函数签名切到下一个 `pub async fn`。
    fn tauri_command_body<'a>(src: &'a str, fn_name: &str, next_fn: &str) -> &'a str {
        let needle = format!("pub async fn {fn_name}(");
        let start = src
            .find(&needle)
            .unwrap_or_else(|| panic!("missing {fn_name}"));
        let rest = &src[start..];
        let next = format!("pub async fn {next_fn}(");
        let end = rest
            .find(&next)
            .unwrap_or_else(|| panic!("missing {next_fn} after {fn_name}"));
        &rest[..end]
    }

    /// GuiClient 预检必须始终走 sidecar，本机也不得在 GUI 进程直连 owner 核心。
    #[test]
    fn preview_tauri_command_always_proxies_gui() {
        let src = include_str!("fresh_restart.rs");
        let body = tauri_command_body(
            src,
            "preview_workbench_fresh_restart",
            "preview_workbench_fresh_restart_for_state",
        );
        assert!(
            body.contains("proxy_workbench_if_gui"),
            "preview Tauri command 必须经 control 代理到 sidecar"
        );
        assert!(
            !body.contains("if is_foreign_device(state.inner()"),
            "GuiClient 本机也必须代理到 sidecar，禁止用 is_foreign_device 门闩跳过: {body}"
        );
    }

    /// GuiClient 执行必须始终走 sidecar，本机也不得在 GUI 进程杀 tmux。
    #[test]
    fn execute_tauri_command_always_proxies_gui() {
        let src = include_str!("fresh_restart.rs");
        let body = tauri_command_body(
            src,
            "run_workbench_fresh_restart",
            "run_workbench_fresh_restart_for_state",
        );
        assert!(
            body.contains("proxy_workbench_if_gui"),
            "execute Tauri command 必须经 control 代理到 sidecar"
        );
        assert!(
            !body.contains("if is_foreign_device(state.inner()"),
            "GuiClient 本机也必须代理到 sidecar，禁止用 is_foreign_device 门闩跳过: {body}"
        );
    }
}
