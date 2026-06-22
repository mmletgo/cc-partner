//! commands/permissions.rs — 权限查询/请求命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 `usePermissions` hook + `OnboardingGuard` 通过 invoke 调用本模块命令，
//!     查询 macOS 屏幕录制/输入监控权限状态，并触发授权流程。对照 Python
//!     `permissions.py` 四函数，封装为两个 IPC 命令。
//!
//! Code Logic（这个模块做什么）:
//!     - `check_permissions`：无状态，直接调 `permissions::check_permissions`。
//!     - `request_permission`：按 type 调 `permissions::request_permission`，返回 JSON。

use crate::error::AppError;
use crate::permissions;

/// 查询当前权限状态（screenCapture / inputMonitoring）。
///
/// Business Logic: 前端权限状态徽标与 OnboardingGuard 初始化时调用。
#[tauri::command]
pub fn check_permissions() -> Result<permissions::PermissionsStatus, AppError> {
    Ok(permissions::check_permissions())
}

/// 请求指定类型权限（触发系统弹框 + 打开设置面板）。
///
/// Business Logic: 用户在 onboarding/设置页点「去授权」时调用。
/// Code Logic: type ∈ {"screenCapture","inputMonitoring"}，转发给 `permissions::request_permission`。
#[tauri::command]
pub fn request_permission(r#type: String) -> Result<serde_json::Value, AppError> {
    let r = permissions::request_permission(&r#type);
    Ok(serde_json::json!({
        "ok": r.ok,
        "requested": r.requested,
        "opened": r.opened,
    }))
}
