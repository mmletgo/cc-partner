//! commands/workbench_dependencies.rs — 工作台运行时依赖命令
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 页面和设置页需要通过 Tauri invoke 检测、安装、轮询和取消 tmux 依赖；
//!     Attention source 还需要只读读取缓存中的稳定状态变更时间。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 dependency manager 的四个 thin command，状态与任务句柄保存在 AppState；
//!     另提供 state helper 供 Attention 只读缓存状态，不触发探测。
//!     可选 deviceId：外机走 P2P，本机路径保持 GUI 进程缓存（Attention 仍只投影本机）。

use crate::commands::workbench::{device_base_url, proxy_workbench_if_gui};
use crate::error::{AppError, AppErrorCategory};
use crate::state::AppState;
use crate::workbench::dependencies::{
    actual_install_command_preview, probe_workbench_dependency, unsupported_dependency_status,
    WorkbenchDependencyState, WorkbenchDependencyStatusDto,
};
use crate::workbench::remote_client::RemoteWorkbenchClient;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     Settings/Attention 必须继续打本机；只有选中远端设备时才走 P2P。
///
/// Code Logic（这个函数做什么）:
///     空或等于本机 deviceId → false。
fn is_foreign_device(state: &AppState, device_id: Option<&str>) -> bool {
    let device_id = device_id.unwrap_or("").trim();
    !device_id.is_empty() && device_id != state.device_id.as_str()
}

/// Business Logic（为什么需要这个函数）:
///     旧 peer 可能未宣告 `workbench.dependency-install.v1`，或根本没有 status 路由。
///     这两种情况都不能当成「tmux 未安装」，更不能冒充 ready。
///
/// Code Logic（这个函数做什么）:
///     capability 缺失、404、405 → true。传输离线与其它业务错误仍 false。
fn is_unprobeable_remote_dependency_error(error: &AppError) -> bool {
    if error.classify() == AppErrorCategory::NotFound {
        return true;
    }
    let code = error.code();
    let text = error.to_string();
    code.contains("capability_unsupported")
        || text.contains("capability_unsupported")
        || code == "method_not_allowed"
        || text.contains("method_not_allowed")
}

/// Business Logic（为什么需要这个函数）:
///     无法探测或无法自动安装时卡片为 unsupported，不能把错误当成安装成功，
///     也不能把缺路由误报成 tmux missing。
///
/// Code Logic（这个函数做什么）:
///     不可探测错误 → unsupported DTO；其它错误原样上抛。
fn map_remote_dependency_result(
    result: Result<WorkbenchDependencyStatusDto, AppError>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    match result {
        Ok(status) => Ok(status),
        Err(error) if is_unprobeable_remote_dependency_error(&error) => {
            Ok(unsupported_dependency_status(error.to_string()))
        }
        Err(error) => Err(error),
    }
}

/// Business Logic（为什么需要这个函数）:
///     Attention 聚合依赖 tmux 环境条目时，必须读取进程内缓存的稳定 statusChangedAt，
///     且不能在 Inbox 刷新路径上触发探测/安装副作用。
///
/// Code Logic（这个函数做什么）:
///     直接返回 `AppState.workbench_dependency` 的当前 DTO 快照（含 status_changed_at）。
pub fn get_workbench_dependency_status_for_state(state: &AppState) -> WorkbenchDependencyStatusDto {
    state.workbench_dependency.status()
}

/// 检测 Workbench tmux 依赖状态。
///
/// Business Logic（为什么需要这个函数）:
///     进入 Workbench 或设置页时，前端需要知道 tmux 是否可用以及缺失时可执行的安装命令预览。
///
/// Code Logic（这个函数做什么）:
///     本机探测写入共享 runtime；外机经 sidecar P2P。
#[tauri::command]
pub async fn check_workbench_dependency(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "dependency.check",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    check_workbench_dependency_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享探测；远端必须读 owning device。
///
/// Code Logic（这个函数做什么）:
///     外机 → P2P status；本机 → 现有 probe + runtime 缓存（Attention 仍读本机）。
pub async fn check_workbench_dependency_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return map_remote_dependency_result(client.dependency_status(&base_url).await);
    }
    Ok(state
        .workbench_dependency
        .set_checked_status(probe_workbench_dependency()))
}

/// 启动 Workbench tmux 依赖安装。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认安装后，后端负责执行平台安装命令并让前端轮询状态；不做静默 sudo 密码注入。
///
/// Code Logic（这个函数做什么）:
///     若 tmux 已可用直接返回 ready；否则按平台预览命令 spawn 后台任务。
#[tauri::command]
pub async fn install_workbench_dependency(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "dependency.install",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    install_workbench_dependency_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     远端安装必须在 owning device（含 headless）执行，不得装到控制端。
///
/// Code Logic（这个函数做什么）:
///     外机 → P2P install；本机 → 现有 spawn_install。
pub async fn install_workbench_dependency_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return map_remote_dependency_result(client.dependency_install(&base_url).await);
    }
    let detected = probe_workbench_dependency();
    if detected.status == WorkbenchDependencyState::Ready {
        return Ok(state.workbench_dependency.set_checked_status(detected));
    }

    let command = actual_install_command_preview()
        .ok_or_else(|| AppError::generic("当前平台不支持自动安装 tmux"))?;
    state.workbench_dependency.spawn_install(command)
}

/// 读取 Workbench tmux 依赖安装状态。
///
/// Business Logic（为什么需要这个函数）:
///     安装命令可能运行较久，前端需要轮询当前状态和最近输出摘要。
///
/// Code Logic（这个函数做什么）:
///     返回 AppState 中 dependency runtime 的当前 DTO 快照（含稳定 statusChangedAt）。
#[tauri::command]
pub async fn get_workbench_dependency_install_status(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "dependency.status",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    get_workbench_dependency_install_status_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     远端安装轮询必须读对端 runtime，不能读控制端本机缓存。
///
/// Code Logic（这个函数做什么）:
///     外机 → P2P status；本机 → 现有缓存快照。
pub async fn get_workbench_dependency_install_status_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return map_remote_dependency_result(client.dependency_status(&base_url).await);
    }
    Ok(get_workbench_dependency_status_for_state(state))
}

/// 取消正在进行的 Workbench tmux 依赖安装。
///
/// Business Logic（为什么需要这个函数）:
///     用户不想继续等待安装时，应能停止后台安装命令并看到取消状态。
///
/// Code Logic（这个函数做什么）:
///     触发 runtime 取消令牌并返回取消后的状态快照。
#[tauri::command]
pub async fn cancel_workbench_dependency_install(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "dependency.cancel",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    cancel_workbench_dependency_install_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     取消必须落到对端安装任务。
///
/// Code Logic（这个函数做什么）:
///     外机 → P2P cancel；本机 → 现有 runtime cancel。
pub async fn cancel_workbench_dependency_install_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchDependencyStatusDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        let client = RemoteWorkbenchClient::new().with_expected_device_id(device_id.trim());
        return map_remote_dependency_result(client.dependency_cancel(&base_url).await);
    }
    Ok(state.workbench_dependency.cancel())
}

#[cfg(test)]
mod tests {
    use super::{is_foreign_device, map_remote_dependency_result};
    use crate::error::AppError;
    use crate::workbench::dependencies::{
        WorkbenchDependencyInstallRuntime, WorkbenchDependencyState, WorkbenchDependencyStatusDto,
    };
    use std::sync::Arc;

    /// Business Logic（为什么需要这个测试）:
    ///     Attention 与前端 status 命令必须共享同一份缓存，重复读取不能重置 statusChangedAt。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 runtime 上写入 ready，连续两次 status 读取，断言 status_changed_at 稳定。
    #[test]
    fn cached_status_read_preserves_status_changed_at() {
        let runtime = Arc::new(WorkbenchDependencyInstallRuntime::new());
        let first = runtime.set_checked_status(WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Ready,
            available: true,
            version: Some("3.4".into()),
            backend: "native".into(),
            path: Some("/usr/bin/tmux".into()),
            installable: false,
            install_command_preview: Vec::new(),
            error: None,
            output: Vec::new(),
            status_changed_at: String::new(),
        });
        let second = runtime.status();
        let third = runtime.status();

        assert_eq!(first.status, WorkbenchDependencyState::Ready);
        assert!(!first.status_changed_at.is_empty());
        assert_eq!(second.status_changed_at, first.status_changed_at);
        assert_eq!(third.status_changed_at, first.status_changed_at);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺 capability 或旧路由 404 必须变成卡片 unsupported，不能冒充安装成功，也不能当成 tmux missing。
    ///
    /// Code Logic（这个测试做什么）:
    ///     map_remote_dependency_result 把 capability_unsupported 与 not_found 收成 unsupported DTO。
    #[test]
    fn missing_capability_maps_to_unsupported_status() {
        let mapped = map_remote_dependency_result(Err(AppError::unavailable(
            "capability_unsupported:workbench.dependency-install.v1".to_string(),
        )))
        .expect("mapped");
        assert_eq!(mapped.status, WorkbenchDependencyState::Unsupported);
        assert!(!mapped.installable);
        assert!(!mapped.available);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 peer 没有 dependency/status 路由时，404 也不能占成「需要安装 tmux」。
    ///
    /// Code Logic（这个测试做什么）:
    ///     not_found 同样收成 unsupported。
    #[test]
    fn missing_status_route_maps_to_unsupported_status() {
        let mapped = map_remote_dependency_result(Err(AppError::not_found(
            "远端 Workbench 请求失败: HTTP 404".to_string(),
        )))
        .expect("mapped");
        assert_eq!(mapped.status, WorkbenchDependencyState::Unsupported);
        assert!(!mapped.available);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 capability 错误必须原样失败，不能吞成 unsupported。
    ///
    /// Code Logic（这个测试做什么）:
    ///     generic 错误仍是 Err。
    #[test]
    fn other_remote_errors_stay_errors() {
        let _ = is_foreign_device;
        let other = map_remote_dependency_result(Err(AppError::generic("boom".to_string())));
        assert!(other.is_err());
    }
}
