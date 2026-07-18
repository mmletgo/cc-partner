//! permissions — macOS 权限检测/请求（对照 Python `ui/permissions.py`）
//!
//! Business Logic（为什么需要这个模块）:
//!     四条 macOS 权限的真实消费者：截图依赖「屏幕录制」；健康提醒键鼠采样（device_query，
//!     走 IOHIDManager）依赖「输入监控」；健康提醒活动窗口标题采样（active-win-pos-rs，走 AX
//!     API）依赖「辅助功能」；久坐/运营系统通知依赖「通知」。全局快捷键基于 RegisterEventHotKey，
//!     无需 TCC。Dev（`com.cc-partner.app.dev`）与 Release（`com.cc-partner.app`）按 Bundle 身份
//!     独立记账。本模块提供检测 + 请求（弹系统框/打开设置面板）的 Rust 实现。
//!
//! Code Logic（这个模块做什么）:
//!     - 屏幕录制：`CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`。
//!     - 输入监控（Privacy_ListenEvent）：**fail-closed 多信号**（bundle id + IOHID + TCC + CG）。
//!     - 辅助功能：仅 `AXIsProcessTrusted`（**不**带 prompt；禁止 `WithOptions` 自动弹框）。
//!     - 通知：`UNUserNotificationCenter`（native/macos/notification_auth.m）；**禁止**依赖
//!       tauri-plugin-notification 桌面 stub（恒 Granted，无法区分 Dev/Release）。
//!     - 非 macOS 一律视为已授权。
//!     - `open` 打开「系统设置 → 隐私与安全 / 通知」对应面板。

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

/// 发布版 Bundle Identifier（与 `tauri.conf.json` `identifier` 对齐）。
pub const PRODUCT_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app";
/// 开发版 Bundle Identifier（`scripts/prepare-macos-dev-app.mjs` 写入 Info.plist）。
pub const DEV_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app.dev";

/// 应用发行通道：开发壳 vs 发布包（前端 onboarding 存储分 key 用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppFlavor {
    /// `cc-partner-dev.app` / `com.cc-partner.app.dev`
    Dev,
    /// 正式安装包 / `com.cc-partner.app`
    Release,
}

/// 应用身份（Bundle ID + flavor），供前端隔离引导状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIdentity {
    /// 当前进程解析到的 CFBundleIdentifier；裸二进制可能为 null。
    pub bundle_id: Option<String>,
    /// 开发壳或发布包。
    pub flavor: AppFlavor,
}

/// Business Logic（为什么需要这个函数）:
///     开发壳与发布版必须在系统设置与前端引导状态上可区分。
///
/// Code Logic（这个函数做什么）:
///     Bundle ID 等于或后缀为 `.dev` / 等于 `DEV_BUNDLE_IDENTIFIER` → Dev，否则 Release。
pub fn app_flavor() -> AppFlavor {
    match main_bundle_identifier().as_deref() {
        Some(id) if id == DEV_BUNDLE_IDENTIFIER || id.ends_with(".dev") => AppFlavor::Dev,
        _ => AppFlavor::Release,
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端 OnboardingGuard / Welcome 需要按 flavor 隔离 localStorage key。
///
/// Code Logic（这个函数做什么）:
///     组合 `main_bundle_identifier` 与 `app_flavor`。
pub fn app_identity() -> AppIdentity {
    AppIdentity {
        bundle_id: main_bundle_identifier(),
        flavor: app_flavor(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     解析 Info.plist XML 中的 CFBundleIdentifier，供 CF API 失败时的路径回退。
///
/// Code Logic（这个函数做什么）:
///     在文本中定位 `<key>CFBundleIdentifier</key>` 后的第一个 `<string>...</string>`，
///     去空白后非空则返回；找不到返回 None。不依赖完整 plist 解析库。
fn parse_cfbundle_identifier_plist_xml(xml: &str) -> Option<String> {
    const KEY: &str = "CFBundleIdentifier";
    let key_pos = xml.find(KEY)?;
    let after_key = &xml[key_pos + KEY.len()..];
    let string_open = after_key.find("<string>")?;
    let after_open = &after_key[string_open + "<string>".len()..];
    let string_close = after_open.find("</string>")?;
    let value = after_open[..string_close].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Business Logic（为什么需要这个函数）:
///     `CFBundleGetIdentifier` 在部分启动路径（直接 exec MacOS 二进制、异常 main bundle）
///     会返回空，但进程实际仍在 `*.app/Contents/MacOS/` 内，是合法 TCC 主体。
///
/// Code Logic（这个函数做什么）:
///     从 `current_exe` 向上识别 `…/Name.app/Contents/MacOS/exe`，读取同级
///     `Contents/Info.plist` 的 CFBundleIdentifier。
#[cfg(target_os = "macos")]
fn bundle_id_from_enclosing_app_plist() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let macos_dir = exe.parent()?;
    if macos_dir.file_name()?.to_str()? != "MacOS" {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name()?.to_str()? != "Contents" {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    let app_name = app_dir.file_name()?.to_str()?;
    if !app_name.ends_with(".app") {
        return None;
    }
    let plist_path = contents_dir.join("Info.plist");
    let xml = std::fs::read_to_string(&plist_path).ok()?;
    parse_cfbundle_identifier_plist_xml(&xml)
}

/// 读取主 bundle 的 CFBundleIdentifier；裸二进制常为 None。
///
/// Business Logic: 无稳定 bundle id 时进程往往不是 TCC 主体，系统设置不会以产品名列出，
///     此时任何「已授权」展示都会与用户在系统设置中的观察冲突。
/// Code Logic: 先 `CFBundleGetMainBundle` + `CFBundleGetIdentifier`；失败则从
///     enclosing `.app/Contents/Info.plist` 解析；过滤空串与 tauri 占位 `app`。
#[cfg(target_os = "macos")]
fn main_bundle_identifier() -> Option<String> {
    let from_cf = unsafe {
        let bundle = CFBundleGetMainBundle();
        if bundle.is_null() {
            None
        } else {
            let id_ref = CFBundleGetIdentifier(bundle);
            if id_ref.is_null() {
                None
            } else {
                let mut buf = vec![0i8; 512];
                if CFStringGetCString(
                    id_ref,
                    buf.as_mut_ptr(),
                    buf.len() as isize,
                    K_CFSTRING_ENCODING_UTF8,
                ) == 0
                {
                    None
                } else {
                    std::ffi::CStr::from_ptr(buf.as_ptr())
                        .to_str()
                        .ok()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                }
            }
        }
    };

    let raw = from_cf.or_else(bundle_id_from_enclosing_app_plist);
    match raw.as_deref() {
        None | Some("") | Some("app") => {
            if let Ok(exe) = std::env::current_exe() {
                tracing::debug!(
                    exe = %exe.display(),
                    product = PRODUCT_BUNDLE_IDENTIFIER,
                    "bundle id unresolved (CF + Info.plist); bare/dev binary is not a TCC product subject"
                );
            }
            None
        }
        Some(_) => raw,
    }
}

/// TCCAccessPreflight 结果（私有 API 经验值；与 ScreenCapture 对照验证）。
#[cfg(target_os = "macos")]
const TCC_PREFLIGHT_GRANTED: i32 = 0;
#[cfg(target_os = "macos")]
const TCC_PREFLIGHT_DENIED: i32 = 1;
// Unknown=2：ListenEvent 上常见，不得单独当作「未授权」。

/// Business Logic（为什么需要这个函数）:
///     私有 TCC preflight 在 ListenEvent 上常返回 Unknown，不得把非 0 一律当未授权（假红）。
///
/// Code Logic（这个函数做什么）:
///     仅当 preflight 明确返回 Denied(1) 时返回 true（应 hard-fail）；None/Granted/Unknown 返回 false。
#[cfg(target_os = "macos")]
fn tcc_listen_event_hard_denied(tcc: Option<i32>) -> bool {
    matches!(tcc, Some(TCC_PREFLIGHT_DENIED))
}

/// 通过私有 TCC 框架预检 ListenEvent（与系统设置列表同源信号）。
///
/// Business Logic: 公开 CG/IOHID 在无 bundle / 仅辅助功能时会假绿；TCC preflight 可补充
///     「隐私 → 输入监控」侧信号。框架为私有，加载失败时返回 None 由调用方降级。
/// Code Logic: dlopen TCC.framework → TCCAccessPreflight("kTCCServiceListenEvent")；
///     返回 0=Granted / 1=Denied / 2=Unknown（或其它非 0/1 值按 Unknown 软处理）。
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
// WithOptions(prompt) 仅经 native `cp_request_accessibility_prompt` 在用户点击时调用。
#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Business Logic: 用户点「去设置」时把本 app 登记到「隐私 → 辅助功能」列表并可选弹系统引导。
/// Code Logic: 调用 native `cp_request_accessibility_prompt`（WithOptions prompt=true）；
///     仅 request 路径调用，check 路径仍用无 prompt 的 `AXIsProcessTrusted`。
#[cfg(target_os = "macos")]
fn request_accessibility_prompt() -> bool {
    extern "C" {
        fn cp_request_accessibility_prompt() -> bool;
    }
    unsafe { cp_request_accessibility_prompt() }
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
    /// 通知权限（按 Bundle 身份独立；权威源为 UNUserNotificationCenter，非 plugin stub）。
    pub notification: PermissionState,
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

/// macOS「系统设置 → 隐私与安全」面板 URL scheme（多候选，兼容旧/新系统设置）。
#[cfg(target_os = "macos")]
fn settings_urls(perm_type: &str) -> &'static [&'static str] {
    match perm_type {
        "screenCapture" => &[
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
        ],
        "inputMonitoring" => &[
            // 输入监控：旧 Security pane + Sequoia PrivacySecurity extension
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ListenEvent",
            "x-apple.systempreferences:com.apple.preference.security?Privacy_InputMonitoring",
        ],
        "accessibility" => &[
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility",
        ],
        "notification" => &[
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension",
            "x-apple.systempreferences:com.apple.preference.notifications",
        ],
        _ => &[],
    }
}

/// Business Logic: 用户点击「去设置」后应落到正确隐私子页，旧 URL 在新系统可能无效。
/// Code Logic: 依次 `open` 候选 URL，任一 spawn 成功即 true。
#[cfg(target_os = "macos")]
pub fn open_permission_settings(perm_type: &str) -> bool {
    let mut any = false;
    for url in settings_urls(perm_type) {
        if std::process::Command::new("open").arg(url).spawn().is_ok() {
            any = true;
            // 多 URL 都尝试一次：部分系统会忽略无效 scheme，有效者会前置正确面板
        }
    }
    any
}

/// Business Logic: 输入监控列表只有在进程「尝试监听输入」后才出现本 app；仅 open 设置不够。
/// Code Logic: IOHIDRequestAccess + CGRequestListenEventAccess，再尝试 listen-only
///     CGEventTapCreate（失败也登记 TCC 主体）；**仅 request 路径调用**，check 禁止 EventTap。
#[cfg(target_os = "macos")]
fn register_input_monitoring_subject() -> bool {
    unsafe {
        let requested_iohid = IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT);
        let requested_cg = CGRequestListenEventAccess();
        // listen-only event tap：未授权时返回 null，但会把签名身份写入「输入监控」列表
        attempt_listen_only_event_tap_registration();
        requested_iohid || requested_cg
    }
}

/// 尝试创建 listen-only event tap 以登记 TCC 主体（立即释放；不用于授权判定）。
#[cfg(target_os = "macos")]
fn attempt_listen_only_event_tap_registration() {
    // CGEventTapLocation / Placement / Options
    const K_CG_SESSION_EVENT_TAP: u32 = 1;
    const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
    const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    // CGEventMask bit for key down/up (kCGEventKeyDown=10, KeyUp=11)
    const KEY_MASK: u64 = (1u64 << 10) | (1u64 << 11);

    type TapCallback = Option<
        unsafe extern "C" fn(
            proxy: *mut std::ffi::c_void,
            event_type: u32,
            event: *mut std::ffi::c_void,
            user_info: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void,
    >;

    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: TapCallback,
            user_info: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }

    unsafe extern "C" fn passthrough_tap(
        _proxy: *mut std::ffi::c_void,
        _event_type: u32,
        event: *mut std::ffi::c_void,
        _user_info: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void {
        event
    }

    unsafe {
        let tap = CGEventTapCreate(
            K_CG_SESSION_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            KEY_MASK,
            Some(passthrough_tap),
            std::ptr::null_mut(),
        );
        if !tap.is_null() {
            CFRelease(tap);
        }
    }
}

// ── macOS 通知权限（UNUserNotificationCenter，按 Bundle 身份独立）────────────
#[cfg(target_os = "macos")]
mod notification_ffi {
    extern "C" {
        /// 见 native/macos/notification_auth.h
        pub fn cp_notification_auth_status() -> i32;
        pub fn cp_notification_request_authorization() -> i32;
    }
}

/// 检测通知权限（macOS 用 UNUserNotificationCenter；非 macOS 一律 true）。
///
/// Business Logic（为什么需要这个函数）:
///     Welcome/Onboarding 第四项；Dev 与 Release 必须各自授权，互不继承。
///     tauri-plugin-notification 桌面端 `permission_state` 恒 Granted，不得使用。
///
/// Code Logic（这个函数做什么）:
///     查询 authorizationStatus；Authorized/Provisional/Ephemeral → true；
///     NotDetermined/Denied/error → false（fail-closed，通知为引导必选项）。
#[cfg(target_os = "macos")]
pub fn check_notification_access() -> bool {
    // UNAuthorizationStatus: 0 notDetermined, 1 denied, 2 authorized, 3 provisional, 4 ephemeral
    let status = unsafe { notification_ffi::cp_notification_auth_status() };
    matches!(status, 2 | 3 | 4)
}

#[cfg(not(target_os = "macos"))]
pub fn check_notification_access() -> bool {
    true
}

/// 请求通知权限（仅 notDetermined 弹框；返回是否已授权）。
///
/// Business Logic: 用户点 Welcome「去设置」时触发。
/// Code Logic: `requestAuthorizationWithOptions`；再读一次 status。
#[cfg(target_os = "macos")]
pub fn request_notification_access() -> bool {
    let _ = unsafe { notification_ffi::cp_notification_request_authorization() };
    check_notification_access()
}

#[cfg(not(target_os = "macos"))]
pub fn request_notification_access() -> bool {
    true
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
///     CGPreflight/CGEventTap 也会假绿。必须 fail-closed。同时不得因私有
///     `TCCAccessPreflight==Unknown` 在用户已打开开关后假红。
///
/// Code Logic（这个函数做什么）:
///     1) 无/空/占位 bundle id → false；
///     2) IOHIDCheckAccess(ListenEvent) 必须 Granted(0)；
///     3) 若 TCCAccessPreflight 可用：仅 Denied(1) → false；Granted(0) 通过；
///        Unknown(2)/其它值软忽略（继续看 CG）；
///     4) CGPreflightListenEventAccess 必须 true。
///     任一步失败时 `info!` 打出 bundle/iohid/tcc/cg 便于对照系统设置。
#[cfg(target_os = "macos")]
pub fn check_input_monitoring_access() -> bool {
    let bundle_id = main_bundle_identifier();
    let bundle_label = bundle_id.as_deref().unwrap_or("<none>");
    match bundle_id.as_deref() {
        None | Some("") | Some("app") => {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "<unknown>".into());
            tracing::info!(
                bundle_id = bundle_label,
                exe = %exe,
                product = PRODUCT_BUNDLE_IDENTIFIER,
                dev_product = DEV_BUNDLE_IDENTIFIER,
                "input_monitoring check: no stable product bundle id → fail-closed (use release .app or ./start.sh dev → cc-partner-dev.app, not bare target/debug/app)"
            );
            return false;
        }
        Some(_) => {}
    }

    // 失败路径统一采样 iohid/tcc/cg，便于对照系统设置（Denied 时常需完全退出重开）。
    let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let tcc = tcc_listen_event_preflight();
    let cg = unsafe { CGPreflightListenEventAccess() };

    if iohid != K_IOHID_ACCESS_TYPE_GRANTED {
        // debug：Welcome 2s 轮询会刷屏；诊断时 RUST_LOG=app_lib::permissions=debug
        tracing::debug!(
            bundle_id = bundle_label,
            iohid,
            tcc = ?tcc,
            cg,
            "input_monitoring check: IOHID not Granted (0=Granted,1=Denied,2=Unknown); if System Settings toggle is on for this bundle, fully quit and reopen the app"
        );
        return false;
    }

    if tcc_listen_event_hard_denied(tcc) {
        tracing::debug!(
            bundle_id = bundle_label,
            iohid,
            tcc = ?tcc,
            cg,
            "input_monitoring check: TCC preflight Denied"
        );
        return false;
    }
    if let Some(code) = tcc {
        if code != TCC_PREFLIGHT_GRANTED {
            // Unknown 或未文档化返回值：不单独否决，交给 CG 终判
            tracing::debug!(
                bundle_id = bundle_label,
                iohid,
                tcc = code,
                cg,
                "input_monitoring check: TCC preflight soft (not Denied); continue"
            );
        }
    }

    if !cg {
        tracing::debug!(
            bundle_id = bundle_label,
            iohid,
            tcc = ?tcc,
            "input_monitoring check: CGPreflightListenEventAccess=false"
        );
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
///
/// Code Logic: 同步查询四项；通知走 UNUserNotificationCenter（调用方宜 spawn_blocking，
///     避免主线程与 completion 死锁）。debug 打一条四字段摘要（含 bundle id），
///     便于对照 Welcome「系统已开仍未授权」是否为需重启的 TCC 进程态。
pub fn check_permissions() -> PermissionsStatus {
    let status = PermissionsStatus {
        screen_capture: PermissionState {
            granted: check_screen_capture_access(),
        },
        input_monitoring: PermissionState {
            granted: check_input_monitoring_access(),
        },
        accessibility: PermissionState {
            granted: check_accessibility_access(),
        },
        notification: PermissionState {
            granted: check_notification_access(),
        },
    };
    #[cfg(target_os = "macos")]
    {
        let bundle = main_bundle_identifier();
        tracing::debug!(
            bundle_id = bundle.as_deref().unwrap_or("<none>"),
            screen = status.screen_capture.granted,
            accessibility = status.accessibility.granted,
            input_monitoring = status.input_monitoring.granted,
            notification = status.notification.granted,
            "check_permissions summary"
        );
    }
    status
}

/// 请求权限（对照 Python `request_screen_capture_access` + `open_permission_settings`）。
///
/// Business Logic:
///     - screenCapture：CGRequestScreenCaptureAccess + 可选打开设置；授权后常需完全退出重开。
///     - inputMonitoring：登记 TCC 主体（IOHID + CGRequest + listen-only EventTap 尝试）后
///       短暂等待再打开输入监控面板，确保列表出现本 app（Dev 显示名 cc-partner (Dev)）。
///     - accessibility：仅用户点击时 WithOptions(prompt=true) 登记列表 + 打开设置；
///       **禁止**在 check/挂载路径 prompt。
///     - notification：UNUserNotificationCenter requestAuthorization；可选打开通知设置。
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
                // 先登记主体，再给 TCC 一点时间写入列表，最后打开设置页
                let requested = register_input_monitoring_subject();
                if open_settings {
                    std::thread::sleep(std::time::Duration::from_millis(350));
                }
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
                // 仅用户点击：prompt 登记列表；检测路径永不调用
                let trusted = request_accessibility_prompt();
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: trusted || check_accessibility_access(),
                    requested: true,
                    opened,
                }
            }
            "notification" => {
                let before = check_notification_access();
                let ok = if before {
                    true
                } else {
                    request_notification_access()
                };
                let opened = if open_settings {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok,
                    requested: !before,
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
    ///     调用 check_permissions，断言四字段各自与检测函数一致且可序列化（含 notification）。
    #[test]
    fn check_permissions_reports_independent_fields() {
        let status = check_permissions();
        assert_eq!(
            status.input_monitoring.granted,
            check_input_monitoring_access()
        );
        assert_eq!(status.accessibility.granted, check_accessibility_access());
        assert_eq!(status.screen_capture.granted, check_screen_capture_access());
        assert_eq!(status.notification.granted, check_notification_access());
        let json = serde_json::to_value(&status).expect("serialize");
        assert!(json.get("inputMonitoring").is_some());
        assert!(json.get("accessibility").is_some());
        assert!(json.get("screenCapture").is_some());
        assert!(json.get("notification").is_some());
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

    /// Business Logic（为什么需要这个测试）:
    ///     TCC Unknown 不得把系统设置已开的输入监控判成未授权（假红）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言仅 Denied(1) 触发 hard deny；None/Granted(0)/Unknown(2)/其它值均不 hard deny。
    #[test]
    #[cfg(target_os = "macos")]
    fn tcc_listen_event_only_hard_denies_on_denied() {
        assert!(!tcc_listen_event_hard_denied(None));
        assert!(!tcc_listen_event_hard_denied(Some(TCC_PREFLIGHT_GRANTED)));
        assert!(tcc_listen_event_hard_denied(Some(TCC_PREFLIGHT_DENIED)));
        assert!(!tcc_listen_event_hard_denied(Some(2))); // Unknown
        assert!(!tcc_listen_event_hard_denied(Some(99)));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     CF API 失败时依赖 Info.plist 文本解析，解析器不得误读其它 key。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用最小 plist XML 断言读到 com.cc-partner.app；缺 key / 空 string 返回 None。
    #[test]
    fn parse_cfbundle_identifier_from_minimal_plist_xml() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>cc-partner</string>
  <key>CFBundleIdentifier</key><string>com.cc-partner.app</string>
  <key>CFBundleExecutable</key><string>cc-partner</string>
</dict></plist>"#;
        assert_eq!(
            parse_cfbundle_identifier_plist_xml(xml).as_deref(),
            Some("com.cc-partner.app")
        );
        assert_eq!(
            parse_cfbundle_identifier_plist_xml("<dict></dict>"),
            None
        );
        assert_eq!(
            parse_cfbundle_identifier_plist_xml(
                "<key>CFBundleIdentifier</key><string>  </string>"
            ),
            None
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     flavor 决定前端引导 key；Dev Bundle 不得被当成 Release。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在测试二进制上调用 app_identity；仅断言可序列化且 flavor 为枚举之一。
    #[test]
    fn app_identity_serializes_flavor() {
        let id = app_identity();
        let v = serde_json::to_value(&id).expect("serialize");
        assert!(v.get("flavor").is_some());
        assert!(matches!(id.flavor, AppFlavor::Dev | AppFlavor::Release));
    }
}
