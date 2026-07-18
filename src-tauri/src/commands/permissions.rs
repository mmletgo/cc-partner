//! commands/permissions.rs — 权限查询/请求命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 `usePermissions` hook + `OnboardingGuard` 通过 invoke 调用本模块命令，
//!     查询 macOS 屏幕录制/输入监控/辅助功能/通知权限状态，并触发授权流程。
//!
//! Code Logic（这个模块做什么）:
//!     - `check_permissions`：spawn_blocking（通知查询带 semaphore）。
//!     - `request_permission`：**主线程**执行公开 Request API。
//!     - `open_permission_settings`：独立打开系统设置，不夹带 Request。
//!     - `get_app_identity`：返回 Bundle ID + flavor（dev/release）。
//!     - `relaunch_for_permissions`：仅 Welcome 用户点「重新打开应用」后生效。

use crate::error::AppError;
use crate::permissions;
use tauri::AppHandle;

/// 查询当前权限状态（screenCapture / inputMonitoring / accessibility / notification）。
///
/// Business Logic: 前端权限状态徽标与 OnboardingGuard 初始化时调用。
/// Code Logic: spawn_blocking 执行同步 FFI/ObjC（通知查询带 semaphore）。
#[tauri::command]
pub async fn check_permissions() -> Result<permissions::PermissionsStatus, AppError> {
    tokio::task::spawn_blocking(permissions::check_permissions)
        .await
        .map_err(|e| AppError::generic(format!("check_permissions join: {e}")))
}

/// 查询当前应用身份（开发壳 vs 发布包）。
///
/// Business Logic: Welcome / OnboardingGuard 需按 flavor 隔离 onboarding 标记，
///     避免开发版与发布版共用「已引导」状态。
/// Code Logic: 委托 `permissions::app_identity`。
#[tauri::command]
pub fn get_app_identity() -> Result<permissions::AppIdentity, AppError> {
    Ok(permissions::app_identity())
}

/// 请求指定类型权限（仅触发公开 Request API）。
///
/// Business Logic: 用户在 Welcome/设置页点“请求授权”时调用；进入页面或回前台只查询。
/// Code Logic: type ∈ screenCapture|inputMonitoring|accessibility|notification；
///     必须主线程执行 CG/IOHID/AX Request，且绝不打开设置或自动重启。
#[tauri::command]
pub async fn request_permission(
    app: AppHandle,
    r#type: String,
) -> Result<permissions::PermissionActionResult, AppError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let r = permissions::request_permission(&r#type);
        let _ = tx.send(r);
    })
    .map_err(|e| AppError::generic(format!("request_permission schedule main: {e}")))?;

    let r = rx
        .await
        .map_err(|_| AppError::generic("request_permission main-thread result dropped"))?;

    Ok(r)
}

/// 打开指定权限的系统设置（不请求权限、不重启应用）。
///
/// Business Logic: Denied 状态只允许用户显式选择“打开系统设置”。
/// Code Logic: 委托纯设置跳转入口，返回操作前后状态供前端刷新。
#[tauri::command]
pub fn open_permission_settings(
    r#type: String,
) -> Result<permissions::PermissionActionResult, AppError> {
    Ok(permissions::open_permission_settings(&r#type))
}

/// 为应用 TCC 授权态而 relaunch（**仅** Welcome「重新打开应用」按钮）。
///
/// Business Logic:
///     用户在系统设置打开开关后，当前进程的 CG/AX/IOHID 检测常仍为未授权；
///     产品要让 Welcome **显示已授权**而不展示「请手动退出」文案，须用户主动重启进程。
///     macOS 必须经 LaunchServices `open` 拉起 `.app`，禁止直接 exec MacOS 二进制
///     （否则会丢 TCC 主体 / 假绿）。**禁止** request_permission 自动调用本命令。
///
/// Code Logic:
///     委托 `permissions::relaunch_for_permissions`；函数在成功路径不返回（进程退出）。
#[tauri::command]
pub fn relaunch_for_permissions(app: AppHandle) -> Result<(), AppError> {
    permissions::relaunch_for_permissions(&app)
}
