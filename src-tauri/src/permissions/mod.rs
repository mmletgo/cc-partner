//! permissions — macOS 权限检测/请求（对照 Python `ui/permissions.py`）
//!
//! Business Logic（为什么需要这个模块）:
//!     三条 macOS 权限的真实消费者：截图依赖「屏幕录制」；健康提醒键鼠采样（device_query，
//!     走 IOHIDManager）依赖「输入监控」；健康提醒活动窗口标题采样（active-win-pos-rs，走 AX
//!     API）依赖「辅助功能」。全局快捷键基于 RegisterEventHotKey，无需任何 TCC 权限。前端设置
//!     页需展示授权状态并引导前往系统设置开启。本模块提供检测（屏幕录制/输入监控/辅助功能）
//!     + 请求（弹系统框/打开设置面板）的 Rust 实现。
//!
//! Code Logic（这个模块做什么）:
//!     - macOS 下通过 FFI 调 CoreGraphics 的 `CGPreflightScreenCaptureAccess` /
//!       `CGRequestScreenCaptureAccess`（屏幕录制）与 `CGPreflightListenEventAccess` /
//!       `CGRequestListenEventAccess`（输入监控 / Privacy_ListenEvent，10.15+）。
//!     - **禁止**用 `CGEventTapCreate` 当输入监控判据：在仅有「辅助功能」时 tap 创建常成功，
//!       导致 UI 假绿，与系统设置「输入监控」列表不一致。
//!     - 辅助功能用 `AXIsProcessTrusted`（ApplicationServices）。
//!     - 非 macOS 一律视为已授权（与 Python 非打包行为一致；Tauri 不区分打包/开发）。
//!     - `open` 命令打开「系统设置 → 隐私与安全」对应面板（URL scheme 与 Python 一致）。

use serde::{Deserialize, Serialize};

// ── macOS CoreGraphics FFI ──────────────────────────────────────────────
// 仅 macOS 下声明屏幕录制/输入监控探测所需的 C 符号。
// 不显式 `#[link]`：CoreGraphics 作为 macOS framework 已被 Tauri 依赖链（core-graphics、
// xcap 等）通过 `-framework CoreGraphics` 链接进二进制，符号在链接期已可见。

#[cfg(target_os = "macos")]
extern "C" {
    /// 预检屏幕录制权限（不弹框）：已授权返回 true。10.15+。
    fn CGPreflightScreenCaptureAccess() -> bool;
    /// 请求屏幕录制权限：仅在「未决定」状态弹系统对话框；已被拒绝则返回 false 不弹框。
    fn CGRequestScreenCaptureAccess() -> bool;
    /// 预检输入监控（Listen Event / Privacy_ListenEvent）权限（不弹框）。10.15+。
    /// 与辅助功能（AXIsProcessTrusted）相互独立；不可用 CGEventTapCreate 代替。
    fn CGPreflightListenEventAccess() -> bool;
    /// 请求输入监控权限：仅在「未决定」状态弹系统对话框；已被拒绝则返回 false 不弹框。
    fn CGRequestListenEventAccess() -> bool;
}

// ── macOS ApplicationServices FFI（辅助功能权限）──────────────────────────
// AX* 符号位于 ApplicationServices/HIServices 子框架，未必被 Tauri 依赖链
// （core-graphics/xcap 只链 CoreGraphics）带入，故此处显式 link framework。
// 与上面 CG*「不写 #[link]」刻意区分。若编译器报 framework 已链接的 warning，可移除该 link。

// AXIsProcessTrusted：当前进程被加入「隐私 → 辅助功能」白名单返回 true（10.2+）。
#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// 单项权限的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub granted: bool,
}

/// 全量权限状态（前端 `PermissionsStatus` 结构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsStatus {
    pub screen_capture: PermissionState,
    pub input_monitoring: PermissionState,
    /// 辅助功能权限（健康提醒活动窗口标题采样依赖；macOS 需手动授权）。
    pub accessibility: PermissionState,
}

/// 请求权限的结果（前端约定字段：ok / requested / opened）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResult {
    pub ok: bool,
    /// 是否真正触发了系统授权弹框（仅 macOS 屏幕录制「未决定」时为 true）。
    pub requested: bool,
    /// 是否成功打开了系统设置面板。
    pub opened: bool,
}

/// macOS「系统设置 → 隐私与安全」面板 URL scheme（对照 Python `_PERMISSION_SETTINGS_URLS`）。
#[cfg(target_os = "macos")]
fn settings_url(perm_type: &str) -> Option<&'static str> {
    match perm_type {
        "screenCapture" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        "inputMonitoring" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        }
        "accessibility" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        _ => None,
    }
}

/// 检测屏幕录制权限（macOS 用 CGPreflightScreenCaptureAccess，非 macOS 一律 true）。
///
/// Business Logic: 截图前需确认已授权，未授权抓到空白图。对照 Python `check_screen_capture_access`。
#[cfg(target_os = "macos")]
pub fn check_screen_capture_access() -> bool {
    // 符号在 10.15+ 一定存在；FFI 声明即为存在性兜底。
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_screen_capture_access() -> bool {
    true
}

/// 检测输入监控权限（macOS 用 CGPreflightListenEventAccess；非 macOS 一律 true）。
///
/// Business Logic（为什么需要这个函数）:
///     健康提醒键鼠采样依赖「隐私 → 输入监控」。探测结果必须与系统设置列表一致，
///     不能在仅开启「辅助功能」时假绿，否则用户无法完成 L3 deny→grant 闭环。
///
/// Code Logic（这个函数做什么）:
///     FFI 调 CoreGraphics `CGPreflightListenEventAccess`（10.15+，不弹框，对应
///     Privacy_ListenEvent / TCC ListenEvent）。**不**调用 `CGEventTapCreate`：该 API
///     在已授辅助功能时经常返回非 NULL，造成与系统设置不一致的假阳性。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_access() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_input_monitoring_access() -> bool {
    true
}

/// 检测辅助功能权限（macOS 用 AXIsProcessTrusted；非 macOS 一律 true）。
///
/// Business Logic: 健康提醒的活动窗口标题/进程名采样依赖辅助功能权限（active-win-pos-rs
///     底层走 AX API）。未授权时采不到窗口标题，需引导用户前往「隐私 → 辅助功能」开启。
/// Code Logic: FFI 调 ApplicationServices 的 `AXIsProcessTrusted`（仅查询不弹框）。
#[cfg(target_os = "macos")]
pub fn check_accessibility_access() -> bool {
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_accessibility_access() -> bool {
    true
}

/// 查询全量权限状态（供 `check_permissions` 命令调用）。
///
/// Business Logic: 前端 usePermissions hook 初始化与轮询时调用。
pub fn check_permissions() -> PermissionsStatus {
    PermissionsStatus {
        screen_capture: PermissionState {
            granted: check_screen_capture_access(),
        },
        input_monitoring: PermissionState {
            granted: check_input_monitoring_access(),
        },
        accessibility: PermissionState {
            granted: check_accessibility_access(),
        },
    }
}

/// 打开 macOS 系统设置对应面板（对照 Python `open_permission_settings`）。
///
/// Business Logic: 用户被拒绝或忽略授权弹框后，需直接跳转到对应面板手动开启。
/// Code Logic: 非阻塞 `open <url-scheme>`。macOS-only——唯一调用方 `request_permission` 的
///     调用点全在 `#[cfg(target_os = "macos")]` 块内，非 macOS 调不到，故整函数 mac-only。
#[cfg(target_os = "macos")]
pub fn open_permission_settings(perm_type: &str) -> bool {
    let Some(url) = settings_url(perm_type) else {
        return false;
    };
    std::process::Command::new("open").arg(url).spawn().is_ok()
}

/// 请求权限（对照 Python `request_screen_capture_access` + `open_permission_settings`）。
///
/// Business Logic:
///     - screenCapture：先调 CGRequestScreenCaptureAccess（仅「未决定」弹框）；
///       `open_settings=true`（默认）时再打开设置面板兜底。
///     - inputMonitoring：先调 CGRequestListenEventAccess（仅「未决定」弹框）；
///       `open_settings=true`（默认）时再打开 Privacy_ListenEvent 面板兜底。
///       不得依赖辅助功能侧信道或 CGEventTap 假阳性。
///     - accessibility：无系统 request API（AXIsProcessTrusted 仅查询），只能 open 设置面板。
///     - 非 macOS：返回 `{ok:true, requested:false, opened:false}`。
pub fn request_permission(perm_type: &str, open_settings: Option<bool>) -> RequestPermissionResult {
    let open_settings = open_settings.unwrap_or(true);
    #[cfg(target_os = "macos")]
    {
        match perm_type {
            "screenCapture" => {
                // requested=true 仅当系统弹了授权对话框（CGRequest 返回值不代表最终授权）
                let requested = unsafe { CGRequestScreenCaptureAccess() };
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: check_screen_capture_access(),
                    requested,
                    opened,
                }
            }
            "inputMonitoring" => {
                // 与屏幕录制同形：先 request 弹框（未决定时），再可选打开设置面板。
                let requested = unsafe { CGRequestListenEventAccess() };
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: check_input_monitoring_access(),
                    requested,
                    opened,
                }
            }
            "accessibility" => {
                // 无系统 request API（AXIsProcessTrusted 仅查询），只能 open 设置面板引导
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: check_accessibility_access(),
                    requested: false,
                    opened,
                }
            }
            _ => RequestPermissionResult {
                ok: true,
                requested: false,
                opened: false,
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (perm_type, open_settings);
        RequestPermissionResult {
            ok: true,
            requested: false,
            opened: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     输入监控与辅助功能必须可独立报告；假绿回归会让 L3 清单与系统设置错位。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 check_permissions，断言三字段均为 bool 且结构可序列化；在 macOS 上额外断言
    ///     input_monitoring.granted == check_input_monitoring_access() 且
    ///     accessibility.granted == check_accessibility_access()（两路 API 各自一致，
    ///     不强制二者相等——那正是假绿 bug 的错误假设）。
    #[test]
    fn check_permissions_reports_independent_fields() {
        let status = check_permissions();
        assert_eq!(
            status.input_monitoring.granted,
            check_input_monitoring_access()
        );
        assert_eq!(status.accessibility.granted, check_accessibility_access());
        assert_eq!(
            status.screen_capture.granted,
            check_screen_capture_access()
        );
        let json = serde_json::to_value(&status).expect("serialize");
        assert!(json.get("inputMonitoring").is_some());
        assert!(json.get("accessibility").is_some());
        assert!(json.get("screenCapture").is_some());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     输入监控 request 路径不得再写死 requested=false（应能触发 ListenEvent 系统框）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 request_permission("inputMonitoring", Some(false)) 不强制 open 设置；
    ///     返回体含 ok/requested/opened 字段且类型稳定（不断言本机 TCC 终态）。
    #[test]
    fn request_input_monitoring_shape_is_stable() {
        let r = request_permission("inputMonitoring", Some(false));
        // opened 在 open_settings=false 时必须为 false
        assert!(!r.opened);
        let _ = (r.ok, r.requested);
    }
}
