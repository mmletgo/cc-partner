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
//!     - 屏幕录制：`CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`。
//!     - 输入监控（Privacy_ListenEvent）：**fail-closed 多信号**：
//!         1) 无稳定 `CFBundleIdentifier`（如 `tauri dev` 裸 `target/debug/app`）→ 一律未授权
//!            （该进程不受 TCC 约束，系统 API 会假绿，且系统设置不会以 cc-partner 列出）；
//!         2) `IOHIDCheckAccess(ListenEvent)` 仅 `Granted` 通过；
//!         3) 若可加载私有 TCC 框架，则 `TCCAccessPreflight(kTCCServiceListenEvent)==0`；
//!         4) 最后 `CGPreflightListenEventAccess`。
//!       **禁止**单独用 `CGEventTapCreate` / 单独用 CGPreflight（在已授辅助功能时易假绿）。
//!     - 辅助功能：`AXIsProcessTrusted`。
//!     - 非 macOS 一律视为已授权。
//!     - `open` 打开「系统设置 → 隐私与安全」对应面板。

use serde::{Deserialize, Serialize};

// ── macOS CoreGraphics FFI ──────────────────────────────────────────────
// 不显式 `#[link]`：CoreGraphics 已被 Tauri 依赖链链接。

#[cfg(target_os = "macos")]
extern "C" {
    /// 预检屏幕录制权限（不弹框）：已授权返回 true。10.15+。
    fn CGPreflightScreenCaptureAccess() -> bool;
    /// 请求屏幕录制权限：仅在「未决定」状态弹系统对话框；已被拒绝则返回 false 不弹框。
    fn CGRequestScreenCaptureAccess() -> bool;
    /// 预检输入监控（Listen Event）。不得单独作为 UI 判据，须配合 bundle id + IOHID + TCC。
    fn CGPreflightListenEventAccess() -> bool;
    /// 请求输入监控权限（未决定时弹框）。
    fn CGRequestListenEventAccess() -> bool;
}

// ── macOS CoreFoundation + IOKit（bundle 身份 / 输入监控）────────────────
#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFBundleGetMainBundle() -> *mut std::ffi::c_void;
    fn CFBundleGetIdentifier(bundle: *mut std::ffi::c_void) -> *const std::ffi::c_void;
    fn CFStringGetCString(
        the_string: *const std::ffi::c_void,
        buffer: *mut std::os::raw::c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringCreateWithCString(
        alloc: *const std::ffi::c_void,
        c_str: *const std::os::raw::c_char,
        encoding: u32,
    ) -> *const std::ffi::c_void;
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    /// kIOHIDRequestTypeListenEvent = 1；返回 kIOHIDAccessTypeGranted=0 / Denied=1 / Unknown=2。
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

#[cfg(target_os = "macos")]
const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;

/// 读取主 bundle 的 CFBundleIdentifier；裸二进制常为 None。
///
/// Business Logic: 无稳定 bundle id 时进程往往不是 TCC 主体，系统设置不会以产品名列出，
///     此时任何「已授权」展示都会与用户在系统设置中的观察冲突。
/// Code Logic: CFBundleGetMainBundle + CFBundleGetIdentifier + CFStringGetCString。
#[cfg(target_os = "macos")]
fn main_bundle_identifier() -> Option<String> {
    unsafe {
        let bundle = CFBundleGetMainBundle();
        if bundle.is_null() {
            return None;
        }
        let id_ref = CFBundleGetIdentifier(bundle);
        if id_ref.is_null() {
            return None;
        }
        let mut buf = vec![0i8; 512];
        if CFStringGetCString(
            id_ref,
            buf.as_mut_ptr(),
            buf.len() as isize,
            K_CFSTRING_ENCODING_UTF8,
        ) == 0
        {
            return None;
        }
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_str()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    }
}

/// 通过私有 TCC 框架预检 ListenEvent（与系统设置列表同源信号）。
///
/// Business Logic: 公开 CG/IOHID 在无 bundle / 仅辅助功能时会假绿；TCC preflight 更贴近
///     「隐私 → 输入监控」列表。框架为私有，加载失败时返回 None 由调用方降级。
/// Code Logic: dlopen TCC.framework → TCCAccessPreflight("kTCCServiceListenEvent")；
///     经验值 0=granted、非 0=not granted（与 ScreenCapture 对照验证）。
#[cfg(target_os = "macos")]
fn tcc_listen_event_preflight() -> Option<i32> {
    unsafe {
        extern "C" {
            fn dlopen(filename: *const std::os::raw::c_char, flags: i32) -> *mut std::ffi::c_void;
            fn dlsym(
                handle: *mut std::ffi::c_void,
                symbol: *const std::os::raw::c_char,
            ) -> *mut std::ffi::c_void;
        }
        const RTLD_LAZY: i32 = 1;
        let path =
            std::ffi::CString::new("/System/Library/PrivateFrameworks/TCC.framework/TCC").ok()?;
        let handle = dlopen(path.as_ptr(), RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let sym = std::ffi::CString::new("TCCAccessPreflight").ok()?;
        let f = dlsym(handle, sym.as_ptr());
        if f.is_null() {
            return None;
        }
        type PreflightFn =
            unsafe extern "C" fn(*const std::ffi::c_void, *const std::ffi::c_void) -> i32;
        let preflight: PreflightFn = std::mem::transmute(f);
        let service_c = std::ffi::CString::new("kTCCServiceListenEvent").ok()?;
        let service = CFStringCreateWithCString(
            std::ptr::null(),
            service_c.as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        );
        if service.is_null() {
            return None;
        }
        let result = preflight(service, std::ptr::null());
        CFRelease(service);
        Some(result)
    }
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

/// 检测输入监控权限（macOS fail-closed 多信号；非 macOS 一律 true）。
///
/// Business Logic（为什么需要这个函数）:
///     UI「已授权」必须与系统设置「隐私 → 输入监控」一致。`tauri dev` 裸二进制无
///     CFBundleIdentifier 时不受 TCC 约束，公开 API 会假绿；仅开辅助功能时
///     CGPreflight/CGEventTap 也会假绿。必须 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     1) 无/空/占位 bundle id → false；
///     2) IOHIDCheckAccess(ListenEvent) 必须 Granted；
///     3) 若 TCCAccessPreflight 可用，必须返回 0（granted）；
///     4) CGPreflightListenEventAccess 必须 true。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_access() -> bool {
    let bundle_id = main_bundle_identifier();
    match bundle_id.as_deref() {
        None => {
            tracing::debug!("input_monitoring check: no CFBundleIdentifier → fail-closed");
            return false;
        }
        // tauri dev 偶发 placeholder / 空标识
        Some("") | Some("app") => {
            tracing::debug!(
                bundle_id = bundle_id.as_deref(),
                "input_monitoring check: unstable bundle id → fail-closed"
            );
            return false;
        }
        Some(_) => {}
    }

    let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    if iohid != K_IOHID_ACCESS_TYPE_GRANTED {
        tracing::debug!(iohid, "input_monitoring check: IOHID not Granted");
        return false;
    }

    if let Some(tcc) = tcc_listen_event_preflight() {
        if tcc != 0 {
            tracing::debug!(tcc, "input_monitoring check: TCC preflight not granted");
            return false;
        }
    }

    let cg = unsafe { CGPreflightListenEventAccess() };
    if !cg {
        tracing::debug!("input_monitoring check: CGPreflightListenEventAccess=false");
        return false;
    }
    true
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
                // IOHID + CG 双 request（未决定时弹框），再可选打开 Privacy_ListenEvent。
                let requested_iohid =
                    unsafe { IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
                let requested_cg = unsafe { CGRequestListenEventAccess() };
                let requested = requested_iohid || requested_cg;
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
        assert_eq!(status.screen_capture.granted, check_screen_capture_access());
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

    /// Business Logic（为什么需要这个测试）:
    ///     cargo test / 无产品 bundle 的进程不得假绿输入监控。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在测试二进制（通常无 com.cc-partner.app bundle id）上调用 check；
    ///     若 bundle id 缺失或为占位，必须返回 false。
    #[test]
    #[cfg(target_os = "macos")]
    fn input_monitoring_fail_closed_without_product_bundle() {
        let id = main_bundle_identifier();
        let granted = check_input_monitoring_access();
        match id.as_deref() {
            None | Some("") | Some("app") => {
                assert!(
                    !granted,
                    "无稳定 bundle id 时输入监控必须 fail-closed，got granted=true id={id:?}"
                );
            }
            Some(other) => {
                // 测试环境若被注入了真实 bundle，仅要求函数可调用且与自身一致
                let _ = other;
                assert_eq!(granted, check_input_monitoring_access());
            }
        }
    }
}
