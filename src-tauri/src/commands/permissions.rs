//! commands/permissions.rs — 权限查询/请求命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 `usePermissions` hook + `OnboardingGuard` 通过 invoke 调用本模块命令，
//!     查询 macOS 屏幕录制/输入监控/辅助功能/通知权限状态，并触发授权流程。
//!
//! Code Logic（这个模块做什么）:
//!     - `check_permissions`：spawn_blocking（通知查询带 semaphore）。
//!     - `request_permission`：**主线程**执行（macOS 系统授权弹窗/TCC 登记 API
//!       在后台线程常无弹窗、不写列表，表现为「代码改了行为不变」）。
//!     - `get_app_identity`：返回 Bundle ID + flavor（dev/release）。
//!     - `relaunch_for_permissions`：系统设置授权后让 TCC 在新进程生效。

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

/// 请求指定类型权限（触发系统弹框 / 打开设置面板）。
///
/// Business Logic: 用户在 Welcome/设置页点「去设置」时调用；**不得**在进入 Welcome 时自动调用。
/// Code Logic: type ∈ screenCapture|inputMonitoring|accessibility|notification；
///     open_settings 缺省 true；**必须主线程**执行 request_permission（CG/IOHID/AX 弹窗）。
#[tauri::command]
pub async fn request_permission(
    app: AppHandle,
    r#type: String,
    open_settings: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let r = permissions::request_permission(&r#type, open_settings);
        let _ = tx.send(r);
    })
    .map_err(|e| AppError::generic(format!("request_permission schedule main: {e}")))?;

    let r = rx
        .await
        .map_err(|_| AppError::generic("request_permission main-thread result dropped"))?;

    Ok(serde_json::json!({
        "ok": r.ok,
        "requested": r.requested,
        "opened": r.opened,
        "action": r.action,
    }))
}

/// 为应用 TCC 授权态而 relaunch（Welcome 从系统设置返回后调用）。
///
/// Business Logic:
///     用户在系统设置打开开关后，当前进程的 CG/AX/IOHID 检测常仍为未授权；
///     产品要让 Welcome **显示已授权**而不展示「请手动退出」文案，须重启进程。
///     macOS 必须经 LaunchServices `open` 拉起 `.app`，禁止直接 exec MacOS 二进制
///     （否则会丢 TCC 主体 / 假绿）。
///
/// Code Logic:
///     委托 `permissions::relaunch_for_permissions`；函数在成功路径不返回（进程退出）。
#[tauri::command]
pub fn relaunch_for_permissions(app: AppHandle) -> Result<(), AppError> {
    permissions::relaunch_for_permissions(&app)
}
