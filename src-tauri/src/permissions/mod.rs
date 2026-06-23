//! permissions — macOS 权限检测/请求（对照 Python `ui/permissions.py`）
//!
//! Business Logic（为什么需要这个模块）:
//!     截图功能依赖「屏幕录制」权限，全局快捷键依赖「输入监控」权限。前端设置页需要
//!     展示当前授权状态并引导用户前往系统设置开启。本模块提供与 Python `permissions.py`
//!     四个函数等价的 Rust 实现：检测屏幕录制 / 检测输入监控 / 请求屏幕录制 / 打开设置面板。
//!
//! Code Logic（这个模块做什么）:
//!     - macOS 下通过 FFI 调 CoreGraphics 的 `CGPreflightScreenCaptureAccess` /
//!       `CGRequestScreenCaptureAccess`（10.15+ 符号），并用 `CGEventTapCreate` 探测
//!       输入监控权限（返回 NULL 即无权限）。
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

/// CGEventTapLocation：在事件流中的插入位置（kCGHIDEventTap = 0）。
#[cfg(target_os = "macos")]
const CG_HID_EVENT_TAP: u32 = 0;
/// CGEventTapPlacement：kCGHeadInsertEventTap = 0。
#[cfg(target_os = "macos")]
const CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
/// CGEventTapOptions：kCGEventTapOptionListenOnly = 1（被动监听，仅用于权限探测）。
#[cfg(target_os = "macos")]
const CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
/// CGEventMask：监听 keyDown 事件位（kCGEventKeyDown = 10，CGEventMaskBit(k)=1<<k）。
#[cfg(target_os = "macos")]
const CG_EVENT_MASK_KEY_DOWN: u64 = 1u64 << 10;

/// CGEventTapCreate 的回调占位（探测权限用，不实际处理事件）。
#[cfg(target_os = "macos")]
extern "C" fn noop_event_tap(
    _proxy: *mut std::ffi::c_void,
    _etype: u32,
    event: *mut std::ffi::c_void,
    _refcon: *mut std::ffi::c_void,
) -> *mut std::ffi::c_void {
    event
}

#[cfg(target_os = "macos")]
extern "C" {
    /// 创建事件 tap；返回 NULL 表示缺少输入监控权限。用于探测权限。
    fn CGEventTapCreate(
        location: u32,
        placement: u32,
        options: u32,
        events_of_interest: u64,
        callback: extern "C" fn(
            *mut std::ffi::c_void,
            u32,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void,
        user_info: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    /// 释放（invalidate）一个由 CGEventTapCreate 创建的 tap（CFMachPort）。
    fn CFMachPortInvalidate(port: *mut std::ffi::c_void);
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

/// 检测输入监控权限（macOS 尝试创建 CGEventTap，NULL 即无权限；非 macOS 一律 true）。
///
/// Business Logic: 全局快捷键依赖输入监控；用「能否创建事件 tap」作为最准确的判定。对照 Python `check_input_monitoring_access`。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_access() -> bool {
    unsafe {
        let tap = CGEventTapCreate(
            CG_HID_EVENT_TAP,
            CG_HEAD_INSERT_EVENT_TAP,
            CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            CG_EVENT_MASK_KEY_DOWN,
            noop_event_tap,
            std::ptr::null_mut(),
        );
        if tap.is_null() {
            return false;
        }
        // 探测成功即释放，避免长期占用事件流
        CFMachPortInvalidate(tap);
        true
    }
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
///     - inputMonitoring：无系统 request API；仅 `open_settings=true`（默认）时打开设置面板。
///     - 启动主动引导按权限类型差异化传参：screenCapture 弹框即可（open_settings=false），
///       inputMonitoring 只能靠开面板引导（open_settings=true）。
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
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: check_input_monitoring_access(),
                    requested: false,
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
