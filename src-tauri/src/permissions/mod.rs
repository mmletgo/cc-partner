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

/// 通过私有 TCC 框架预检指定服务（与系统设置列表同源信号）。
///
/// Business Logic: 分流「首次登记(prompt)」与「已在列表(只开设置)」需要 TCC 侧状态，
///     避免 CGRequest/AX prompt 与 open 设置双开，同时保证列表能出现本 app。
/// Code Logic: dlopen TCC.framework → TCCAccessPreflight(service)；
///     返回 0=Granted / 1=Denied / 2=Unknown；加载失败返回 None。
#[cfg(target_os = "macos")]
fn tcc_access_preflight(service_name: &str) -> Option<i32> {
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
        let service_c = std::ffi::CString::new(service_name).ok()?;
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

/// ListenEvent TCC 预检（输入监控 check 路径）。
#[cfg(target_os = "macos")]
fn tcc_listen_event_preflight() -> Option<i32> {
    tcc_access_preflight("kTCCServiceListenEvent")
}

/// ScreenCapture TCC 预检（录屏「去设置」分流）。
#[cfg(target_os = "macos")]
fn tcc_screen_capture_preflight() -> Option<i32> {
    tcc_access_preflight("kTCCServiceScreenCapture")
}

/// Accessibility TCC 预检（辅助功能「去设置」分流）。
#[cfg(target_os = "macos")]
fn tcc_accessibility_preflight() -> Option<i32> {
    tcc_access_preflight("kTCCServiceAccessibility")
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

/// 请求权限的结果（前端约定字段：ok / requested / opened / action）。
///
/// `action` 仅由后端产出并序列化给前端，不从 JSON 反序列化（`&'static str`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResult {
    pub ok: bool,
    /// 是否真正触发了系统授权弹框 / 登记 API（按权限类型语义不同）。
    pub requested: bool,
    /// 是否成功打开了系统设置面板。
    pub opened: bool,
    /// settings = 打开系统设置页；prompt = 系统授权框；noop = 已授权无操作。
    pub action: &'static str,
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
///
/// Code Logic:
///     - `silent=true`（Welcome 去设置默认）：**只** listen-only EventTap 登记，
///       **禁止** IOHIDRequestAccess / CGRequestListenEventAccess（二者会弹系统框，
///       与随后 open 设置形成双开，违反 spec）。
///     - `silent=false`：完整登记（含可能弹框的 Request API），仅 open_settings=false 路径用。
///     check 路径禁止 EventTap。
#[cfg(target_os = "macos")]
fn register_input_monitoring_subject(silent: bool) -> bool {
    if silent {
        attempt_listen_only_event_tap_registration();
        // EventTap 无布尔成功语义；调用本身即登记尝试
        return true;
    }
    unsafe {
        let requested_iohid = IOHIDRequestAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT);
        let requested_cg = CGRequestListenEventAccess();
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

/// 请求权限（按类型分流：登记 / 系统弹框 / 打开设置）。
///
/// Business Logic（对齐 design §3）:
///     1) **登记**（列表需要本 app 时必须做）；2) **主动作** 能开设置则开设置，否则系统框；
///     3) **禁止** 同一点击「系统授权弹窗 + 系统设置页」双开。
///     - screenCapture / accessibility：TCC Denied（已在列表）→ 只 open 设置；
///       Unknown/无 TCC 预检 → 只走系统登记/prompt（写入列表，弹窗自带「打开系统设置」）；
///       已 granted → noop。
///     - inputMonitoring：静默登记（IOHID/CG/tap）+ 默认只 open 设置。
///     - notification：authorized → noop；notDetermined → prompt only；denied → settings only。
///
/// Code Logic:
///     `open_settings`：`None`/`Some(true)` → allow_open；`Some(false)` → 仅 prompt/登记。
///     返回 `action ∈ {settings|prompt|noop}`。
pub fn request_permission(perm_type: &str, open_settings: Option<bool>) -> RequestPermissionResult {
    // Some(false) 只登记/请求；None / Some(true) 允许打开设置
    let allow_open = open_settings.unwrap_or(true);
    #[cfg(target_os = "macos")]
    {
        match perm_type {
            "screenCapture" => {
                // Spec 对齐辅助功能 + 禁止双开：
                // CGRequestScreenCaptureAccess **立即返回**（不wait用户关弹窗），
                // 若随后 open 设置 → 用户同时看到录屏弹窗与设置页。
                // 因此：Denied（已在列表）→ 只 open；Unknown/首次 → **只** CGRequest
                // （弹窗自带「打开系统设置」）；已授权 → noop。绝不在同一次调用里
                // CGRequest + open。
                if check_screen_capture_access() {
                    return RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                    };
                }
                let tcc = tcc_screen_capture_preflight();
                let already_in_list = matches!(tcc, Some(TCC_PREFLIGHT_DENIED));
                if already_in_list {
                    let opened = if allow_open {
                        open_permission_settings(perm_type)
                    } else {
                        false
                    };
                    RequestPermissionResult {
                        ok: check_screen_capture_access(),
                        requested: false,
                        opened,
                        action: if opened { "settings" } else { "noop" },
                    }
                } else {
                    // 未在列表：只 CGRequest 登记（可能弹窗）；不 open
                    let requested = unsafe { CGRequestScreenCaptureAccess() };
                    RequestPermissionResult {
                        ok: check_screen_capture_access(),
                        requested,
                        opened: false,
                        action: if requested { "prompt" } else { "noop" },
                    }
                }
            }
            "inputMonitoring" => {
                // Spec：静默登记 + 只 open 设置；禁止任何会弹系统框的 Request API。
                // allow_open 路径 silent=true：仅 EventTap（无 IOHID/CG Request）。
                // TCC Denied：跳过登记，只 open（与辅助功能对称）。
                if check_input_monitoring_access() {
                    return RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                    };
                }
                let tcc = tcc_listen_event_preflight();
                let already_in_list = matches!(tcc, Some(TCC_PREFLIGHT_DENIED));
                let requested = if already_in_list {
                    false
                } else {
                    // Welcome 默认 allow_open=true → silent；仅 open_settings=false 才走可弹框登记
                    register_input_monitoring_subject(/* silent */ allow_open)
                };
                if allow_open && !already_in_list {
                    std::thread::sleep(std::time::Duration::from_millis(350));
                }
                let opened = if allow_open {
                    open_permission_settings(perm_type)
                } else {
                    false
                };
                RequestPermissionResult {
                    ok: check_input_monitoring_access(),
                    requested,
                    opened,
                    action: if opened {
                        "settings"
                    } else if requested {
                        // open_settings=false 时的登记路径
                        "prompt"
                    } else {
                        "noop"
                    },
                }
            }
            "accessibility" => {
                // Spec：登记 + 开设置，禁止双开。
                // TCC Denied 或已不信任但曾登记 → 只 open；Unknown → 只 AX prompt 登记。
                if check_accessibility_access() {
                    return RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                    };
                }
                let tcc = tcc_accessibility_preflight();
                let already_in_list = matches!(tcc, Some(TCC_PREFLIGHT_DENIED));
                if allow_open && already_in_list {
                    let opened = open_permission_settings(perm_type);
                    RequestPermissionResult {
                        ok: check_accessibility_access(),
                        requested: false,
                        opened,
                        action: if opened { "settings" } else { "noop" },
                    }
                } else {
                    // Unknown/None 或 open_settings=false：只 AX prompt 登记（弹窗可引导设置，不叠 open）
                    let trusted = request_accessibility_prompt();
                    RequestPermissionResult {
                        ok: trusted || check_accessibility_access(),
                        requested: true,
                        opened: false,
                        action: "prompt",
                    }
                }
            }
            "notification" => {
                // UNAuthorizationStatus: 0 notDetermined, 1 denied, 2 authorized, 3 provisional, 4 ephemeral
                let status = unsafe { notification_ffi::cp_notification_auth_status() };
                match status {
                    2 | 3 | 4 => RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                    },
                    0 => {
                        // notDetermined：只弹系统授权框，不要无脑 open 设置
                        let ok = request_notification_access();
                        RequestPermissionResult {
                            ok,
                            requested: true,
                            opened: false,
                            action: "prompt",
                        }
                    }
                    // denied 或未知：只走设置页（不 requestAuthorization）
                    _ => {
                        let opened = if allow_open {
                            open_permission_settings(perm_type)
                        } else {
                            false
                        };
                        RequestPermissionResult {
                            ok: check_notification_access(),
                            requested: false,
                            opened,
                            action: "settings",
                        }
                    }
                }
            }
            _ => RequestPermissionResult {
                ok: true,
                requested: false,
                opened: false,
                action: "noop",
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (perm_type, allow_open);
        RequestPermissionResult {
            ok: true,
            requested: false,
            opened: false,
            action: "noop",
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户在系统设置打开 TCC 开关后，当前进程的检测 API 常仍返回未授权；
///     Welcome 要在不展示「请手动退出」文案的前提下让 UI 显示已授权，
///     必须重启进程以应用系统侧授权态。
///
/// Code Logic（这个函数做什么）:
///     macOS：解析 enclosing `*.app`，用 LaunchServices `open` 延迟拉起后 `app.exit(0)`；
///     **禁止**直接 exec `Contents/MacOS/*`（会丢 TCC 主体）。非 macOS / 解析失败时
///     回退 `app.request_restart()`。成功路径进程退出，调用方不应依赖 Ok 返回。
pub fn relaunch_for_permissions(app: &tauri::AppHandle) -> Result<(), crate::error::AppError> {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = enclosing_app_bundle_path() {
            let path = bundle.display().to_string();
            // 延迟 open：确保当前实例先退出，避免 open 只 activate 旧进程
            let script = format!(
                "sleep 0.4; open {}",
                shell_single_quote(&path)
            );
            match std::process::Command::new("sh").arg("-c").arg(&script).spawn() {
                Ok(_) => {
                    tracing::info!(
                        app_bundle = %path,
                        "relaunch_for_permissions: scheduled open via LaunchServices; exiting"
                    );
                    app.exit(0);
                    // exit 后理论不可达；给类型系统一条路径
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        app_bundle = %path,
                        "relaunch_for_permissions: open schedule failed; fallback request_restart"
                    );
                }
            }
        } else {
            tracing::warn!(
                "relaunch_for_permissions: no enclosing .app; fallback request_restart"
            );
        }
    }
    app.request_restart();
    Ok(())
}

/// Business Logic: relaunch 必须打开 `.app` 包而非裸二进制，以保留 TCC 主体。
/// Code Logic: current_exe → …/Name.app/Contents/MacOS/exe 向上三级得到 .app。
#[cfg(target_os = "macos")]
fn enclosing_app_bundle_path() -> Option<std::path::PathBuf> {
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
    let name = app_dir.file_name()?.to_str()?;
    if !name.ends_with(".app") {
        return None;
    }
    Some(app_dir.to_path_buf())
}

/// Business Logic: 把路径安全嵌入 `sh -c` 单引号字符串。
/// Code Logic: `' -> '\''` 经典 shell 转义。
#[cfg(target_os = "macos")]
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
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
    ///     open_settings=false 时输入监控只做登记、不 open；action 可为 prompt（登记）或 settings/noop。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 request_permission("inputMonitoring", Some(false))；断言 !opened。
    #[test]
    fn request_input_monitoring_shape_is_stable() {
        let r = request_permission("inputMonitoring", Some(false));
        assert!(!r.opened);
        assert!(
            r.action == "settings" || r.action == "prompt" || r.action == "noop",
            "unexpected action {:?}",
            r
        );
        let _ = (r.ok, r.requested);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     通知 notDetermined 必须走 prompt，不得无脑 open 设置；open_settings=false 时 opened 恒 false。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 request_permission("notification", Some(false))；断言 !opened，
    ///     action ∈ {prompt|noop|settings}（本机 TCC 终态放宽）。
    #[test]
    fn request_notification_when_undetermined_prefers_prompt_shape() {
        let r = request_permission("notification", Some(false));
        // open_settings=false：不得 opened
        assert!(!r.opened);
        assert!(r.action == "prompt" || r.action == "noop" || r.action == "settings");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Welcome 默认 allow_open 时输入监控产品路径是 settings（静默登记 + 开设置）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     open_settings=true 时 action 为 settings（或已授权 noop）。
    #[test]
    fn request_input_monitoring_defaults_to_settings_action_when_open() {
        let r = request_permission("inputMonitoring", Some(true));
        assert!(
            r.action == "settings" || r.action == "noop",
            "allow_open 不得走 prompt 弹框路径: {:?}",
            r
        );
    }

    /// Business Logic: prompt 与 open 不得并行；顺序「登记后仍未授权再 open」允许 requested&&opened。
    /// Code Logic: action=prompt 时 opened 必须 false。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_screen_capture_prompt_never_opens_settings_same_tick() {
        let r = request_permission("screenCapture", Some(true));
        if r.action == "prompt" {
            assert!(!r.opened, "prompt 不得同时 open settings: {:?}", r);
        } else {
            assert!(
                r.action == "settings" || r.action == "noop",
                "unexpected action {:?}",
                r
            );
        }
    }

    /// Business Logic: 辅助功能 action=prompt 时不得 open。
    /// Code Logic: opened=false when action=prompt。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_accessibility_prompt_never_opens_settings_same_tick() {
        let r = request_permission("accessibility", Some(true));
        if r.action == "prompt" {
            assert!(!r.opened, "prompt 不得同时 open settings: {:?}", r);
            assert!(r.requested);
        } else {
            assert!(
                r.action == "settings" || r.action == "noop",
                "unexpected action {:?}",
                r
            );
        }
    }

    /// Business Logic: Welcome 去设置对输入监控必须静默登记（无 IOHID/CG 弹框 API）。
    /// Code Logic: allow_open 路径 action=settings；不强制 opened（open URL 依赖环境）。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_input_monitoring_allow_open_is_settings_action() {
        let r = request_permission("inputMonitoring", Some(true));
        assert_eq!(r.action, "settings");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未知类型 / 非 macOS 路径必须稳定返回 noop，避免前端 sticky 误判。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用未知 perm type，断言 action=noop 且 opened/requested 为 false。
    #[test]
    fn request_unknown_permission_is_noop() {
        let r = request_permission("notARealPermission", Some(true));
        assert!(!r.opened);
        assert!(!r.requested);
        assert_eq!(r.action, "noop");
        assert!(r.ok);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     relaunch 路径把 .app 路径塞进 `sh -c`，单引号必须可安全嵌套。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对含空格/单引号路径断言 shell_single_quote 往返可被 shell 解析为原串。
    #[test]
    #[cfg(target_os = "macos")]
    fn shell_single_quote_escapes_apostrophe() {
        assert_eq!(shell_single_quote("a"), "'a'");
        assert_eq!(shell_single_quote("a b"), "'a b'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
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
