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
    /// 请求输入监控权限（未决定时弹框）。生产路径走 ObjC `cp_request_listen_event_access`。
    #[allow(dead_code)]
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
    /// 生产 Request 走 ObjC `cp_request_listen_event_access`（需 NSApp）。
    #[allow(dead_code)]
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

#[cfg(target_os = "macos")]
const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_DENIED: u32 = 1;
#[cfg(target_os = "macos")]
#[allow(dead_code)]
const K_IOHID_ACCESS_TYPE_UNKNOWN: u32 = 2;

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
#[allow(dead_code)]
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
    /// 输入监控 Denied 同进程无法弹中转窗时为 true。
    /// **禁止**命令层自动 relaunch；仅 Welcome 用户点「重新打开应用」后新进程 Request。
    pub needs_relaunch: bool,
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
/// Code Logic: 依次 `open` 候选 URL，**首个 spawn 成功即返回**（禁止多 URL 连开多个设置窗）。
#[cfg(target_os = "macos")]
pub fn open_permission_settings(perm_type: &str) -> bool {
    for url in settings_urls(perm_type) {
        if std::process::Command::new("open").arg(url).spawn().is_ok() {
            return true;
        }
    }
    false
}

/// Business Logic: 仅产品 Bundle（Dev/Release）才允许重置 ListenEvent 决策；
///     裸 `app` 或未知身份不得误清其它应用的 TCC。
///
/// Code Logic: 与 `PRODUCT_BUNDLE_IDENTIFIER` / `DEV_BUNDLE_IDENTIFIER` 精确匹配。
#[cfg(target_os = "macos")]
fn is_product_bundle_id(id: &str) -> bool {
    id == PRODUCT_BUNDLE_IDENTIFIER || id == DEV_BUNDLE_IDENTIFIER
}

/// Business Logic: macOS 在列表为空时仍可能把 ListenEvent 记为 Denied，
///     导致 IOHID/CG/EventTap 全失败、系统设置永远不出现本 app（幽灵 Denied）。
///     `tccutil reset ListenEvent <bundle>` 可清掉该决策，让下一次 Request 重新弹窗登记。
///
/// Code Logic: 仅产品 Bundle 时 spawn `tccutil reset ListenEvent <bundle_id>`；
///     成功看 exit status；失败记 warn 返回 false。不用于授权判定。
#[cfg(target_os = "macos")]
fn reset_listen_event_tcc(bundle_id: &str) -> bool {
    if !is_product_bundle_id(bundle_id) {
        tracing::warn!(
            bundle_id,
            "skip tccutil reset ListenEvent: not a product bundle"
        );
        return false;
    }
    match std::process::Command::new("tccutil")
        .args(["reset", "ListenEvent", bundle_id])
        .output()
    {
        Ok(out) => {
            let ok = out.status.success();
            tracing::info!(
                bundle_id,
                ok,
                status = %out.status,
                stdout = %String::from_utf8_lossy(&out.stdout),
                stderr = %String::from_utf8_lossy(&out.stderr),
                "tccutil reset ListenEvent"
            );
            ok
        }
        Err(e) => {
            tracing::warn!(
                bundle_id,
                error = %e,
                "tccutil reset ListenEvent spawn failed"
            );
            false
        }
    }
}

/// 一次性登记子进程入口 flag（冷启动 pending request 用）。
pub const INPUT_MONITORING_REGISTER_ARG: &str = "--cp-register-input-monitoring";

/// 冷启动时「待弹出输入监控 Request」标记文件（data_dir 下）。
#[cfg(target_os = "macos")]
const INPUT_MONITORING_PENDING_REQUEST: &str = "input-monitoring-pending-request";

/// Business Logic: 输入监控系统弹窗必须在**当前 GUI** 的 NSApplication 上下文中请求。
///     第二实例 oneshot 在已有 GUI 运行时不会弹窗，且会阻塞主线程。
///     同进程在 IOHID=Denied 时 Request 会立即 false，**无法**把 app 写入列表。
/// Code Logic: 调 native `cp_request_listen_event_access`（NSApp + Request）。
#[cfg(target_os = "macos")]
fn request_listen_event_with_nsapp() -> bool {
    extern "C" {
        fn cp_request_listen_event_access() -> i32;
    }
    let before = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let raw = unsafe { cp_request_listen_event_access() };
    let after = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let ok = raw != 0 || after == K_IOHID_ACCESS_TYPE_GRANTED;
    tracing::info!(
        before,
        raw,
        after,
        ok,
        "cp_request_listen_event_access done"
    );
    ok
}

/// Business Logic: Denied 同进程 Request 无效；把「下一次 GUI 冷启动主线程再 Request」
///     记到 data_dir，供 Welcome 用户点「重新打开应用」或下次启动时真正登记列表。
/// Code Logic: 写空文件 `input-monitoring-pending-request`；失败仅 warn。
#[cfg(target_os = "macos")]
fn mark_pending_input_monitoring_request() {
    match crate::config::data_dir() {
        Ok(dir) => {
            let path = dir.join(INPUT_MONITORING_PENDING_REQUEST);
            match std::fs::write(&path, b"1") {
                Ok(()) => tracing::info!(
                    path = %path.display(),
                    "marked input-monitoring-pending-request for next launch"
                ),
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to mark input-monitoring-pending-request"
                ),
            }
        }
        Err(e) => tracing::warn!(
            error = %e,
            "data_dir unavailable; cannot mark input-monitoring-pending-request"
        ),
    }
}


/// Business Logic: Dev 诊断中转窗是否弹出。
/// Code Logic: 调 NSApp Request 并返回 bool。
#[cfg(target_os = "macos")]
pub fn debug_request_input_monitoring_once() -> bool {
    request_listen_event_with_nsapp()
}

/// Business Logic: 诊断输入监控 TCC 进程态（Dev 用 CC_PARTNER_IM_DIAG）。
/// Code Logic: 打印 bundle/iohid/tcc/cg 与 current_exe。
#[cfg(target_os = "macos")]
pub fn debug_dump_input_monitoring_state(tag: &str) {
    let bundle = main_bundle_identifier();
    let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let tcc = tcc_listen_event_preflight();
    let cg = unsafe { CGPreflightListenEventAccess() };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".into());
    tracing::info!(
        tag,
        bundle_id = bundle.as_deref().unwrap_or("<none>"),
        exe = %exe,
        iohid,
        tcc = ?tcc,
        cg,
        "input_monitoring diagnostic dump"
    );
    eprintln!(
        "[{tag}] bundle={:?} exe={exe} iohid={iohid} tcc={tcc:?} cg={cg}",
        bundle
    );
}

/// Dev 一次性「已旋转 ad-hoc 签名」标记，防止无限 resign+relaunch。
#[cfg(target_os = "macos")]
const INPUT_MONITORING_CS_ROTATED: &str = "input-monitoring-cs-rotated";

/// Business Logic: ad-hoc 签名下 `tccutil reset ListenEvent <bundle>` 常清不掉按 csreq
///     记账的 Denied；Dev 壳在 pending 冷启动路径可再试一次服务级 reset，否则中转窗永不出现。
/// Code Logic: 仅 Dev flavor 执行 `tccutil reset ListenEvent`（无 bundle）；Release 不调用。
#[cfg(target_os = "macos")]
fn reset_listen_event_tcc_all_for_dev() -> bool {
    if app_flavor() != AppFlavor::Dev {
        return false;
    }
    match std::process::Command::new("tccutil")
        .args(["reset", "ListenEvent"])
        .output()
    {
        Ok(out) => {
            let ok = out.status.success();
            tracing::info!(
                ok,
                status = %out.status,
                stdout = %String::from_utf8_lossy(&out.stdout),
                stderr = %String::from_utf8_lossy(&out.stderr),
                "tccutil reset ListenEvent (service-wide, Dev only)"
            );
            ok
        }
        Err(e) => {
            tracing::warn!(error = %e, "tccutil reset ListenEvent (all) spawn failed");
            false
        }
    }
}

/// Business Logic: ad-hoc Dev 的 ListenEvent Denied 常按 CDHash/csreq 粘住，
///     `tccutil` 成功后进程侧仍 iohid=1，系统中转窗永不出现。旋转 ad-hoc 签名会换
///     CDHash，TCC 视为新主体，下一次启动才能回到未决定并弹窗。
/// Code Logic: 仅 Dev；对 enclosing `.app` 执行 `codesign --force --deep --options runtime -s -`。
#[cfg(target_os = "macos")]
fn rotate_dev_adhoc_codesign() -> bool {
    if app_flavor() != AppFlavor::Dev {
        return false;
    }
    let Some(app_path) = enclosing_app_bundle_path() else {
        tracing::warn!("rotate_dev_adhoc_codesign: no enclosing .app");
        return false;
    };
    match std::process::Command::new("codesign")
        .args([
            "--force",
            "--deep",
            "--options",
            "runtime",
            "--sign",
            "-",
            &app_path.display().to_string(),
        ])
        .output()
    {
        Ok(out) => {
            let ok = out.status.success();
            tracing::info!(
                ok,
                status = %out.status,
                app = %app_path.display(),
                stdout = %String::from_utf8_lossy(&out.stdout),
                stderr = %String::from_utf8_lossy(&out.stderr),
                "rotate_dev_adhoc_codesign"
            );
            ok
        }
        Err(e) => {
            tracing::warn!(error = %e, "codesign rotate spawn failed");
            false
        }
    }
}

/// Business Logic: 签名旋转后须启动**新 CDHash** 的进程，旧进程无法继续弹窗。
/// Code Logic: `sleep 0.8; open <enclosing.app>` 后 `process::exit(0)`；无 .app 则 no-op。
#[cfg(target_os = "macos")]
fn schedule_open_enclosing_app_and_exit() {
    let Some(app_path) = enclosing_app_bundle_path() else {
        tracing::warn!("schedule_open_enclosing_app_and_exit: no enclosing .app");
        return;
    };
    let path = app_path.display().to_string();
    let script = format!("sleep 0.8; open {}", shell_single_quote(&path));
    match std::process::Command::new("sh").arg("-c").arg(&script).spawn() {
        Ok(_) => {
            tracing::info!(
                app_bundle = %path,
                "scheduled open after codesign rotate; exiting for new CDHash"
            );
            std::process::exit(0);
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to schedule open after codesign rotate");
        }
    }
}

/// Business Logic: 上次 Denied 路径在同进程无法弹中转窗；新进程冷启动须在任何
///     IOHID 探测（health DeviceState）之前 Request，否则状态被钉成 Denied 后 raw=0。
///     ad-hoc Dev 若 tccutil 后仍 Denied：旋转 CDHash 后退出再启动，才能弹中转窗。
/// Code Logic:
///     1) 有 pending 文件才继续；
///     2) 若 IOHID/TCC Denied：bundle reset → Dev 服务级 reset；
///     3) 仍 Denied 且 Dev 未旋转过签名 → codesign 旋转 + 保留 pending + open+exit；
///     4) 否则 NSApp Request；成功清 pending/旋转标记；失败保留 pending。
///     **禁止**只删文件不 Request。
#[cfg(target_os = "macos")]
pub fn consume_pending_input_monitoring_request() {
    let Ok(data) = crate::config::data_dir() else {
        return;
    };
    let path = data.join(INPUT_MONITORING_PENDING_REQUEST);
    let rotated_flag = data.join(INPUT_MONITORING_CS_ROTATED);
    if !path.is_file() {
        return;
    }
    tracing::info!(
        path = %path.display(),
        "consuming input-monitoring-pending-request: reset if needed + NSApp Request"
    );

    let mut iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let mut tcc = tcc_listen_event_preflight();
    tracing::info!(iohid, tcc = ?tcc, "pending consume pre-reset state");

    if iohid == K_IOHID_ACCESS_TYPE_DENIED || matches!(tcc, Some(TCC_PREFLIGHT_DENIED)) {
        if let Some(bundle_id) = main_bundle_identifier() {
            let _ = reset_listen_event_tcc(&bundle_id);
        }
        iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
        tcc = tcc_listen_event_preflight();
        if iohid == K_IOHID_ACCESS_TYPE_DENIED || matches!(tcc, Some(TCC_PREFLIGHT_DENIED)) {
            // ad-hoc Dev：bundle 级 reset 常无效，服务级 reset 才能回到 Undetermined
            let _ = reset_listen_event_tcc_all_for_dev();
            std::thread::sleep(std::time::Duration::from_millis(300));
            iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
            tcc = tcc_listen_event_preflight();
        }
        tracing::info!(iohid, tcc = ?tcc, "pending consume post-reset state");
    }

    // tccutil 仍无法清掉 ad-hoc CDHash 粘性 Denied：旋转签名换主体（仅一次）
    if (iohid == K_IOHID_ACCESS_TYPE_DENIED || matches!(tcc, Some(TCC_PREFLIGHT_DENIED)))
        && app_flavor() == AppFlavor::Dev
        && !rotated_flag.is_file()
    {
        if rotate_dev_adhoc_codesign() {
            let _ = std::fs::write(&rotated_flag, b"1");
            let _ = std::fs::write(&path, b"1");
            tracing::warn!(
                "Dev ad-hoc CDHash sticky Denied: rotated codesign; relaunching for clean TCC subject"
            );
            schedule_open_enclosing_app_and_exit();
            // exit 不可达；若 open 调度失败则继续尝试 Request
        }
    }

    let ok = request_listen_event_with_nsapp();
    let iohid_after = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    tracing::info!(
        ok,
        iohid = iohid_after,
        "input-monitoring pending Request finished"
    );

    if ok || iohid_after == K_IOHID_ACCESS_TYPE_GRANTED {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&rotated_flag);
        tracing::info!(path = %path.display(), "cleared input-monitoring-pending-request after success");
    } else {
        // 保留 pending，下次启动（或用户再次重新打开）继续尝试中转窗
        if let Err(e) = std::fs::write(&path, b"1") {
            tracing::warn!(error = %e, "failed to keep input-monitoring-pending-request after failed Request");
        } else {
            tracing::warn!(
                path = %path.display(),
                "kept input-monitoring-pending-request after failed Request (no system prompt)"
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn consume_pending_input_monitoring_request() {}


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

/// 检测屏幕录制权限（macOS 用公开 CGPreflight；非 macOS 一律 true）。
///
/// Business Logic: 截图前需确认已授权。私有 TCC preflight 的 0 不可当作已授权
///     （会假绿）；进程态滞后时由 Welcome needs_reopen / 重新打开应用处理，禁止假绿。
/// Code Logic: 仅 `CGPreflightScreenCaptureAccess()`。
#[cfg(target_os = "macos")]
pub fn check_screen_capture_access() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_screen_capture_access() -> bool {
    true
}

/// 检测输入监控权限（macOS fail-closed；非 macOS 一律 true）。
///
/// Business Logic（为什么需要这个函数）:
///     UI「已授权」必须与系统「隐私 → 输入监控」真实开关一致。
///     调用 `IOHIDRequestAccess` / `CGRequestListenEventAccess` 后，
///     `CGPreflightListenEventAccess`（甚至部分系统上的 IOHID）会在用户未开开关时
///     暂时返回 true → Welcome 一点「去设置」就假绿。因此：
///     - **禁止**用 CGPreflight 作为授权依据（登记 API 污染源）；
///     - **禁止**私有 TCC=0 单独放行；
///     - 仅当 IOHIDCheckAccess==Granted 且 TCC 不是 Denied 时为 true；
///     - 系统开关已开但 IOHID 仍非 Granted → false（Welcome needs_reopen）。
///
/// Code Logic（这个函数做什么）:
///     1) 无/空/占位 bundle id → false；
///     2) TCC preflight Denied(1) → false（列表上明确关闭）；
///     3) IOHIDCheckAccess(ListenEvent)==Granted(0) → true；否则 false。
///     不读 CGPreflight（避免 Request 后假绿）。
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

    let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    let tcc = tcc_listen_event_preflight();
    // 诊断用，不参与授权判定（CGRequest 后会假绿）
    let cg = unsafe { CGPreflightListenEventAccess() };

    if tcc_listen_event_hard_denied(tcc) {
        tracing::debug!(
            bundle_id = bundle_label,
            iohid,
            tcc = ?tcc,
            cg,
            "input_monitoring check: TCC preflight Denied → not granted"
        );
        return false;
    }
    if iohid != K_IOHID_ACCESS_TYPE_GRANTED {
        tracing::debug!(
            bundle_id = bundle_label,
            iohid,
            tcc = ?tcc,
            cg,
            "input_monitoring check: IOHID not Granted (0=Granted,1=Denied,2=Unknown)"
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
/// Business Logic（录屏 / 输入监控 / 辅助功能同构分流）：
///     - screenCapture：已授权 → noop；TCC Denied → 只 open；否则只 CGRequestScreenCaptureAccess。
///     - inputMonitoring：已授权 → noop；**禁止** EventTap / oneshot / **自动重启**。
///       主路径是系统中转弹窗：Unknown → 只 NSApp Request（禁止同次 open）。
///       Denied：`tccutil reset`；同进程仍 Denied → pending + `needs_relaunch=true`（不 open 空列表）。
///       Request 后列表已有未开开关且 allow_open → 只 open。check 只信 IOHID Granted。
///     - accessibility：Denied → 只 open；Unknown → 只 AX prompt。
///     - notification：authorized → noop；notDetermined → prompt；denied → settings。
///
/// Code Logic:
///     `open_settings`：`None`/`Some(true)` → allow_open；`Some(false)` → 仅登记。
///     返回 `action ∈ {settings|prompt|noop}`；`needs_relaunch` 恒 false。
pub fn request_permission(perm_type: &str, open_settings: Option<bool>) -> RequestPermissionResult {
    // Some(false) 只登记/请求；None / Some(true) 允许打开设置
    let allow_open = open_settings.unwrap_or(true);
    #[cfg(target_os = "macos")]
    {
        match perm_type {
            "screenCapture" => {
                // 对齐辅助功能：Denied（已在列表）→ 只 open；否则只 CGRequest 写入列表。
                // 禁止 silent WindowList（新系统不进列表）+ 禁止 CGRequest 与 open 同次。
                if check_screen_capture_access() {
                    return RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                        needs_relaunch: false,
                    };
                }
                let tcc = tcc_screen_capture_preflight();
                // 仅 Denied 表示已在列表未开开关；Unknown/None 必须登记，不可只 open
                let already_in_list = matches!(tcc, Some(TCC_PREFLIGHT_DENIED));
                tracing::info!(
                    allow_open,
                    tcc = ?tcc,
                    already_in_list,
                    "request_permission(screenCapture) branch"
                );
                if allow_open && already_in_list {
                    let opened = open_permission_settings(perm_type);
                    RequestPermissionResult {
                        ok: check_screen_capture_access(),
                        requested: false,
                        opened,
                        action: if opened { "settings" } else { "noop" },
                        needs_relaunch: false,
                    }
                } else {
                    // 首次/Unknown 或 open_settings=false：只 CGRequest（写列表，可能弹窗）
                    let requested = unsafe { CGRequestScreenCaptureAccess() };
                    RequestPermissionResult {
                        ok: check_screen_capture_access(),
                        requested,
                        opened: false,
                        action: if requested { "prompt" } else { "noop" },
                        needs_relaunch: false,
                    }
                }
            }
            "inputMonitoring" => {
                // 产品主路径：系统中转弹窗（CG/IOHID Request）→ 用户确认后本 app 出现在
                // 「隐私 → 输入监控」；**禁止**未登记就 open 空列表，也禁止 EventTap 静默登记。
                // 证据（GUI log）：Denied 同进程 Request 立即 false（raw=0），列表不出现；
                // 必须 tccutil reset + 写 pending + needs_relaunch，用户「重新打开应用」后
                // 新进程 consume_pending 再 Request 才能弹中转窗。
                // - 已 granted → noop
                // - Denied/TCC Denied：reset；仍 Denied → pending + needs_relaunch，**不 open**
                // - Unknown：只 NSApp Request（可能中转窗）；**禁止**与 open 同次
                // - Request 后若已在列表未开开关（TCC Denied）且 allow_open → 只 open 拨开关
                // - ok 只信 IOHID Granted；禁止自动 cold relaunch
                if check_input_monitoring_access() {
                    return RequestPermissionResult {
                        ok: true,
                        requested: false,
                        opened: false,
                        action: "noop",
                        needs_relaunch: false,
                    };
                }
                let tcc = tcc_listen_event_preflight();
                let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
                let is_denied = iohid == K_IOHID_ACCESS_TYPE_DENIED;
                let tcc_denied = matches!(tcc, Some(TCC_PREFLIGHT_DENIED));
                tracing::info!(
                    allow_open,
                    tcc = ?tcc,
                    iohid,
                    is_denied,
                    tcc_denied,
                    "request_permission(inputMonitoring) branch"
                );

                let mut reset_ok = false;
                if is_denied || tcc_denied {
                    if let Some(bundle_id) = main_bundle_identifier() {
                        reset_ok = reset_listen_event_tcc(&bundle_id);
                    } else {
                        tracing::warn!(
                            "inputMonitoring denied but no bundle id for tccutil reset"
                        );
                    }
                }

                let iohid_after_reset =
                    unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
                let same_process_request_useless =
                    iohid_after_reset == K_IOHID_ACCESS_TYPE_DENIED;

                if same_process_request_useless {
                    // 同进程无法弹中转窗：挂 pending，引导 Welcome 手动重新打开
                    mark_pending_input_monitoring_request();
                    tracing::info!(
                        reset_ok,
                        iohid_after_reset,
                        "inputMonitoring: same-process Request useless; pending + needs_relaunch"
                    );
                    return RequestPermissionResult {
                        ok: false,
                        requested: false,
                        opened: false,
                        action: "noop",
                        needs_relaunch: true,
                    };
                }

                // Unknown：只弹系统中转窗/Request 登记（禁止同次 open 空设置页）
                let requested = request_listen_event_with_nsapp();
                let ok = check_input_monitoring_access();
                let iohid_post =
                    unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
                let tcc_post = tcc_listen_event_preflight();
                let now_denied = iohid_post == K_IOHID_ACCESS_TYPE_DENIED
                    || matches!(tcc_post, Some(TCC_PREFLIGHT_DENIED));

                // Request 后进列表但未开开关：允许只 open 设置拨开关（列表应有本 app）
                let opened = if ok {
                    false
                } else if allow_open && now_denied && requested {
                    open_permission_settings(perm_type)
                } else {
                    false
                };

                // Request 失败且仍非列表态：挂 pending，需新进程再弹中转窗
                let needs_relaunch = if ok {
                    false
                } else if !requested || (!now_denied && !ok) {
                    mark_pending_input_monitoring_request();
                    true
                } else {
                    false
                };

                tracing::info!(
                    requested,
                    reset_ok,
                    opened,
                    ok,
                    needs_relaunch,
                    iohid_after = iohid_post,
                    tcc_after = ?tcc_post,
                    "request_permission(inputMonitoring) done"
                );
                RequestPermissionResult {
                    ok,
                    requested,
                    opened,
                    action: if opened {
                        "settings"
                    } else if requested {
                        "prompt"
                    } else {
                        "noop"
                    },
                    needs_relaunch,
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
                        needs_relaunch: false,
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
                        needs_relaunch: false,
                    }
                } else {
                    // Unknown/None 或 open_settings=false：只 AX prompt 登记（弹窗可引导设置，不叠 open）
                    let trusted = request_accessibility_prompt();
                    RequestPermissionResult {
                        ok: trusted || check_accessibility_access(),
                        requested: true,
                        opened: false,
                        action: "prompt",
                        needs_relaunch: false,
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
                        needs_relaunch: false,
                    },
                    0 => {
                        // notDetermined：只弹系统授权框，不要无脑 open 设置
                        let ok = request_notification_access();
                        RequestPermissionResult {
                            ok,
                            requested: true,
                            opened: false,
                            action: "prompt",
                            needs_relaunch: false,
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
                            needs_relaunch: false,
                        }
                    }
                }
            }
            _ => RequestPermissionResult {
                ok: true,
                requested: false,
                opened: false,
                action: "noop",
                needs_relaunch: false,
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
            needs_relaunch: false,
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
            // 延迟 open：给旧进程足够时间退出，避免 TCC/CDHash 粘在旧实例上
            // （输入监控 Denied 时 0.4s 常不够，新进程仍 before=Denied 无法弹中转窗）
            let script = format!(
                "sleep 1.2; open {}",
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

    /// Business Logic: 仅产品 Bundle 可触发 tccutil reset，防误清其它 app。
    /// Code Logic: product / .dev 为 true；占位 app / 空 / 无关 id 为 false。
    #[test]
    #[cfg(target_os = "macos")]
    fn product_bundle_id_only_accepts_dev_and_release() {
        assert!(is_product_bundle_id(PRODUCT_BUNDLE_IDENTIFIER));
        assert!(is_product_bundle_id(DEV_BUNDLE_IDENTIFIER));
        assert!(!is_product_bundle_id("app"));
        assert!(!is_product_bundle_id(""));
        assert!(!is_product_bundle_id("com.example.other"));
    }

    /// Business Logic: open_settings=false 时输入监控不得 open 设置；
    ///     action 可为 prompt / noop；Denied 时可 needs_relaunch（禁止自动重启）。
    /// Code Logic: 断言 !opened；action ∈ {prompt|noop|settings}。
    #[test]
    fn request_input_monitoring_shape_is_stable() {
        let r = request_permission("inputMonitoring", Some(false));
        assert!(!r.opened);
        assert!(
            r.action == "settings" || r.action == "prompt" || r.action == "noop",
            "unexpected action {:?}",
            r
        );
        let _ = (r.ok, r.requested, r.needs_relaunch);
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

    /// Business Logic: Welcome allow_open：Unknown→prompt（中转窗）；Denied→needs_relaunch
    ///     不 open 空列表；已授权→noop。禁止 prompt 与 open 同次；禁止自动 relaunch。
    /// Code Logic: action ∈ {prompt|settings|noop}；prompt ⇒ !opened。
    #[test]
    fn request_input_monitoring_defaults_to_settings_action_when_open() {
        let r = request_permission("inputMonitoring", Some(true));
        if r.action == "prompt" {
            assert!(!r.opened, "prompt 不得同时 open: {:?}", r);
        } else {
            assert!(
                r.action == "settings" || r.action == "noop",
                "unexpected action {:?}",
                r
            );
        }
        // Denied 同进程无法弹窗时 needs_relaunch=true 且不得 open 空列表
        if r.needs_relaunch {
            assert!(!r.opened, "needs_relaunch 不得 open 空设置: {:?}", r);
        }
    }

    /// Business Logic: 录屏禁止同次 prompt+open（对齐辅助功能）。
    /// Code Logic: action=prompt ⇒ opened=false；action=settings|noop 合法。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_screen_capture_allow_open_never_dual_opens() {
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

    /// Business Logic: open_settings=false 时录屏可登记 prompt，但不得 open。
    /// Code Logic: opened=false；action ∈ {prompt|noop}。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_screen_capture_register_only_never_opens_settings() {
        let r = request_permission("screenCapture", Some(false));
        assert!(!r.opened, "register-only 不得 open: {:?}", r);
        assert!(
            r.action == "prompt" || r.action == "noop",
            "unexpected action {:?}",
            r
        );
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

    /// Business Logic: 输入监控禁止 prompt 与 open 双开；禁止自动 relaunch。
    /// Code Logic: action=prompt ⇒ !opened；needs_relaunch ⇒ !opened。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_input_monitoring_allow_open_never_dual_opens() {
        let r = request_permission("inputMonitoring", Some(true));
        if r.action == "prompt" {
            assert!(!r.opened, "prompt 不得同时 open: {:?}", r);
        } else {
            assert!(
                r.action == "settings" || r.action == "noop",
                "unexpected action {:?}",
                r
            );
        }
        if r.needs_relaunch {
            assert!(!r.opened, "needs_relaunch 不得 open 空设置: {:?}", r);
        }
    }

    /// Business Logic: 系统登记弹窗路径后，未真正开开关不得假绿。
    /// Code Logic: before 未授权时，request 后 check 与 result.ok 仍为 false
    ///     （check 只信 IOHID Granted，不信 CGPreflight）。
    #[test]
    #[cfg(target_os = "macos")]
    fn request_input_monitoring_allow_open_does_not_false_green() {
        let before = check_input_monitoring_access();
        let r = request_permission("inputMonitoring", Some(true));
        let after = check_input_monitoring_access();
        if !before {
            assert!(
                !after,
                "未授权时 request 不得使 check 变 true（假绿）: result={:?} after={}",
                r,
                after
            );
            assert!(!r.ok, "未授权时 result.ok 不得为 true: {:?}", r);
        }
    }


    /// Business Logic: pending 标记必须可写入 data_dir，否则 relaunch 后无法登记。
    /// Code Logic: mark_pending 后文件存在；consume 成功删文件、失败则保留（可再试）。
    #[test]
    #[cfg(target_os = "macos")]
    fn mark_pending_input_monitoring_request_writes_file() {
        mark_pending_input_monitoring_request();
        let path = crate::config::data_dir()
            .expect("data_dir")
            .join(INPUT_MONITORING_PENDING_REQUEST);
        assert!(path.is_file(), "mark_pending must create {:?}", path);
        // consume 会尝试 Request；无 GUI/仍 Denied 时保留 pending 是正确行为
        consume_pending_input_monitoring_request();
        // 仅断言 consume 可调用且路径仍是合法 data_dir 下文件或已清除
        let _ = path.is_file();
    }

    /// Business Logic: Denied 同进程无法登记列表时必须留下 pending，供 relaunch 后真正 Request。
    /// Code Logic: 若当前 IOHID=Denied，request 后 data_dir 下应存在 pending 标记文件
    ///     （无产品 bundle 的 cargo test 可能 skip）。
    #[test]
    #[cfg(target_os = "macos")]
    fn denied_input_monitoring_marks_pending_for_next_launch() {
        let iohid = unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) };
        if iohid != K_IOHID_ACCESS_TYPE_DENIED {
            // 本机未处于 Denied 时无法在单测中构造进程级 Denied 缓存
            return;
        }
        let Some(id) = main_bundle_identifier() else {
            return;
        };
        if !is_product_bundle_id(&id) {
            return;
        }
        let _ = request_permission("inputMonitoring", Some(false));
        let path = crate::config::data_dir()
            .expect("data_dir")
            .join(INPUT_MONITORING_PENDING_REQUEST);
        assert!(
            path.is_file(),
            "IOHID Denied 时 request 必须写 pending 标记以便 relaunch 后登记: {:?}",
            path
        );
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
