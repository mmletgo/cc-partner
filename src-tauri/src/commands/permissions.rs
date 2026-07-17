//! commands/permissions.rs — 权限查询/请求命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 `usePermissions` hook + `OnboardingGuard` 通过 invoke 调用本模块命令，
//!     查询 macOS 屏幕录制/输入监控权限状态，并触发授权流程。对照 Python
//!     `permissions.py` 四函数，封装为权限 IPC 命令。
//!
//! Code Logic（这个模块做什么）:
//!     - `check_permissions`：无状态，直接调 `permissions::check_permissions`。
//!     - `request_permission`：按 type 调 `permissions::request_permission`，返回 JSON。
//!     - `get_app_identity`：返回 Bundle ID + flavor（dev/release），供引导存储隔离。

use crate::error::AppError;
use crate::permissions;

/// 查询当前权限状态（screenCapture / inputMonitoring / accessibility）。
///
/// Business Logic: 前端权限状态徽标与 OnboardingGuard 初始化时调用。
#[tauri::command]
pub fn check_permissions() -> Result<permissions::PermissionsStatus, AppError> {
    Ok(permissions::check_permissions())
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
/// Business Logic: 用户在 onboarding/设置页点「去授权」时调用（open_settings 缺省=开面板兜底）；
///     启动 OnboardingGuard 主动引导时按类型差异化传 open_settings（screenCapture=false 仅弹框、
///     inputMonitoring=true 开面板）。
/// Code Logic: type ∈ {"screenCapture","inputMonitoring"}，open_settings 缺省视为 true，
///     转发给 `permissions::request_permission`。
#[tauri::command]
pub fn request_permission(
    r#type: String,
    open_settings: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    let r = permissions::request_permission(&r#type, open_settings);
    Ok(serde_json::json!({
        "ok": r.ok,
        "requested": r.requested,
        "opened": r.opened,
    }))
}
