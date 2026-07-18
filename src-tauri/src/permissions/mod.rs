//! permissions — 桌面权限查询与显式用户操作。
//!
//! Business Logic（为什么需要这个模块）:
//!     截图、键鼠活动采样、窗口标题采样和通知分别依赖屏幕录制、输入监控、
//!     辅助功能与通知权限。macOS 的 TCC 记录绑定应用代码身份，因此输入监控只允许
//!     固定内部签名通道请求；社区/ad-hoc 构建必须明确报告 unavailable。
//!
//! Code Logic（这个模块做什么）:
//!     查询操作不产生副作用；Request、Open Settings、Reopen 是三条独立入口。
//!     输入监控只使用公开 IOHID API，不调用私有权限框架、系统重置或运行时重签。

use serde::{Deserialize, Serialize};

pub mod input_monitoring;

pub use input_monitoring::{InputMonitoringPermissionState, InputMonitoringState};

#[cfg(target_os = "macos")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

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
}

#[cfg(target_os = "macos")]
const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[cfg(target_os = "macos")]
mod notification_ffi {
    extern "C" {
        pub fn cp_notification_auth_status() -> i32;
        pub fn cp_notification_request_authorization() -> i32;
    }
}

/// 应用发行通道，用于前端隔离 onboarding key。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppFlavor {
    Dev,
    Release,
}

/// 当前应用身份。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIdentity {
    pub bundle_id: Option<String>,
    pub flavor: AppFlavor,
}

/// 单项布尔权限状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionState {
    pub granted: bool,
}

/// 全量权限状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsStatus {
    pub screen_capture: PermissionState,
    pub input_monitoring: InputMonitoringPermissionState,
    pub accessibility: PermissionState,
    pub notification: PermissionState,
}

/// 一次显式权限操作的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionOperation {
    Request,
    OpenSettings,
    Noop,
}

/// 权限操作结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionActionResult {
    pub permission: String,
    pub operation: PermissionOperation,
    pub before: String,
    pub after: String,
}

/// 从最小 plist XML 中解析 CFBundleIdentifier。
fn parse_cfbundle_identifier_plist_xml(xml: &str) -> Option<String> {
    const KEY: &str = "CFBundleIdentifier";
    let key_pos = xml.find(KEY)?;
    let after_key = &xml[key_pos + KEY.len()..];
    let string_open = after_key.find("<string>")?;
    let after_open = &after_key[string_open + "<string>".len()..];
    let string_close = after_open.find("</string>")?;
    let value = after_open[..string_close].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// 从 enclosing `.app/Contents/Info.plist` 回退读取 Bundle ID。
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
    if !app_dir.file_name()?.to_str()?.ends_with(".app") {
        return None;
    }
    let xml = std::fs::read_to_string(contents_dir.join("Info.plist")).ok()?;
    parse_cfbundle_identifier_plist_xml(&xml)
}

/// 读取当前主 Bundle ID；裸二进制返回 None。
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
                let mut buffer = vec![0i8; 512];
                if CFStringGetCString(
                    id_ref,
                    buffer.as_mut_ptr(),
                    buffer.len() as isize,
                    K_CFSTRING_ENCODING_UTF8,
                ) == 0
                {
                    None
                } else {
                    std::ffi::CStr::from_ptr(buffer.as_ptr())
                        .to_str()
                        .ok()
                        .map(ToOwned::to_owned)
                }
            }
        }
    };

    from_cf
        .or_else(bundle_id_from_enclosing_app_plist)
        .filter(|id| !id.is_empty() && id != "app")
}

#[cfg(not(target_os = "macos"))]
fn main_bundle_identifier() -> Option<String> {
    None
}

/// 返回当前开发/稳定发行通道。
pub fn app_flavor() -> AppFlavor {
    match main_bundle_identifier().as_deref() {
        Some(id) if id.ends_with(".dev") => AppFlavor::Dev,
        _ => AppFlavor::Release,
    }
}

/// 返回当前应用身份。
pub fn app_identity() -> AppIdentity {
    AppIdentity {
        bundle_id: main_bundle_identifier(),
        flavor: app_flavor(),
    }
}

/// 查询通知权限。
#[cfg(target_os = "macos")]
pub fn check_notification_access() -> bool {
    matches!(
        unsafe { notification_ffi::cp_notification_auth_status() },
        2 | 3 | 4
    )
}

#[cfg(not(target_os = "macos"))]
pub fn check_notification_access() -> bool {
    true
}

/// 请求通知权限；仅由显式 Request 操作调用。
#[cfg(target_os = "macos")]
fn request_notification_access() -> bool {
    let _ = unsafe { notification_ffi::cp_notification_request_authorization() };
    check_notification_access()
}

/// 查询屏幕录制权限。
#[cfg(target_os = "macos")]
pub fn check_screen_capture_access() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_screen_capture_access() -> bool {
    true
}

/// 查询输入监控是否已授权；兼容健康采样现有布尔判据。
pub fn check_input_monitoring_access() -> bool {
    input_monitoring::check_input_monitoring_state(main_bundle_identifier().as_deref()).granted
}

/// 查询辅助功能权限。
#[cfg(target_os = "macos")]
pub fn check_accessibility_access() -> bool {
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
pub fn check_accessibility_access() -> bool {
    true
}

/// 请求辅助功能权限；仅由显式 Request 操作调用。
#[cfg(target_os = "macos")]
fn request_accessibility_prompt() -> bool {
    extern "C" {
        fn cp_request_accessibility_prompt() -> bool;
    }
    unsafe { cp_request_accessibility_prompt() }
}

/// 查询四项权限，不产生授权、设置跳转或重启副作用。
pub fn check_permissions() -> PermissionsStatus {
    PermissionsStatus {
        screen_capture: PermissionState {
            granted: check_screen_capture_access(),
        },
        input_monitoring: input_monitoring::check_input_monitoring_state(
            main_bundle_identifier().as_deref(),
        ),
        accessibility: PermissionState {
            granted: check_accessibility_access(),
        },
        notification: PermissionState {
            granted: check_notification_access(),
        },
    }
}

fn bool_state(granted: bool) -> &'static str {
    if granted {
        "granted"
    } else {
        "denied"
    }
}

fn input_state(state: InputMonitoringState) -> &'static str {
    match state {
        InputMonitoringState::Granted => "granted",
        InputMonitoringState::Denied => "denied",
        InputMonitoringState::NotDetermined => "notDetermined",
        InputMonitoringState::Unavailable => "unavailable",
    }
}

/// 读取指定权限的字符串状态，不产生系统副作用。
fn permission_state(perm_type: &str) -> String {
    match perm_type {
        "screenCapture" => bool_state(check_screen_capture_access()).to_string(),
        "inputMonitoring" => input_state(
            input_monitoring::check_input_monitoring_state(main_bundle_identifier().as_deref())
                .state,
        )
        .to_string(),
        "accessibility" => bool_state(check_accessibility_access()).to_string(),
        "notification" => bool_state(check_notification_access()).to_string(),
        _ => "unavailable".to_string(),
    }
}

/// 显式请求指定权限，不打开系统设置、不重启应用。
pub fn request_permission(perm_type: &str) -> PermissionActionResult {
    #[cfg(target_os = "macos")]
    {
        match perm_type {
            "screenCapture" => {
                let before = check_screen_capture_access();
                let operation = if before {
                    PermissionOperation::Noop
                } else {
                    let _ = unsafe { CGRequestScreenCaptureAccess() };
                    PermissionOperation::Request
                };
                return PermissionActionResult {
                    permission: perm_type.to_string(),
                    operation,
                    before: bool_state(before).to_string(),
                    after: bool_state(check_screen_capture_access()).to_string(),
                };
            }
            "inputMonitoring" => {
                let result = input_monitoring::request_input_monitoring_access(
                    main_bundle_identifier().as_deref(),
                );
                return PermissionActionResult {
                    permission: perm_type.to_string(),
                    operation: match result.operation {
                        input_monitoring::InputMonitoringOperation::Request => {
                            PermissionOperation::Request
                        }
                        input_monitoring::InputMonitoringOperation::Noop => {
                            PermissionOperation::Noop
                        }
                    },
                    before: input_state(result.before).to_string(),
                    after: input_state(result.after).to_string(),
                };
            }
            "accessibility" => {
                let before = check_accessibility_access();
                let operation = if before {
                    PermissionOperation::Noop
                } else {
                    let _ = request_accessibility_prompt();
                    PermissionOperation::Request
                };
                return PermissionActionResult {
                    permission: perm_type.to_string(),
                    operation,
                    before: bool_state(before).to_string(),
                    after: bool_state(check_accessibility_access()).to_string(),
                };
            }
            "notification" => {
                let before = check_notification_access();
                let not_determined =
                    unsafe { notification_ffi::cp_notification_auth_status() } == 0;
                let operation = if before || !not_determined {
                    PermissionOperation::Noop
                } else {
                    let _ = request_notification_access();
                    PermissionOperation::Request
                };
                return PermissionActionResult {
                    permission: perm_type.to_string(),
                    operation,
                    before: bool_state(before).to_string(),
                    after: bool_state(check_notification_access()).to_string(),
                };
            }
            _ => {}
        }
    }

    PermissionActionResult {
        permission: perm_type.to_string(),
        operation: PermissionOperation::Noop,
        before: permission_state(perm_type),
        after: permission_state(perm_type),
    }
}

/// 返回 macOS 系统设置的候选 URL；用于纯函数测试与显式 Open Settings 操作。
#[cfg(target_os = "macos")]
fn settings_urls(perm_type: &str) -> &'static [&'static str] {
    match perm_type {
        "screenCapture" => &[
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
        ],
        "inputMonitoring" => &[
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

/// 显式打开指定权限的系统设置；不请求权限、不重启应用。
pub fn open_permission_settings(perm_type: &str) -> PermissionActionResult {
    let before = permission_state(perm_type);
    #[cfg(target_os = "macos")]
    let opened = settings_urls(perm_type)
        .iter()
        .any(|url| std::process::Command::new("open").arg(url).spawn().is_ok());
    #[cfg(not(target_os = "macos"))]
    let opened = false;

    PermissionActionResult {
        permission: perm_type.to_string(),
        operation: if opened {
            PermissionOperation::OpenSettings
        } else {
            PermissionOperation::Noop
        },
        before,
        after: permission_state(perm_type),
    }
}

/// 用户显式选择后通过 `.app` 重新打开，使已修改的 TCC 状态进入新进程。
pub fn relaunch_for_permissions(app: &tauri::AppHandle) -> Result<(), crate::error::AppError> {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle) = enclosing_app_bundle_path() {
            let path = bundle.display().to_string();
            let script = format!("sleep 1.2; open {}", shell_single_quote(&path));
            if std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .spawn()
                .is_ok()
            {
                app.exit(0);
                return Ok(());
            }
        }
    }
    app.request_restart();
    Ok(())
}

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
    app_dir
        .file_name()?
        .to_str()?
        .ends_with(".app")
        .then(|| app_dir.to_path_buf())
}

#[cfg(target_os = "macos")]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询结果必须携带输入监控四态并可序列化为 camelCase。
    #[test]
    fn check_permissions_serializes_input_monitoring_state() {
        let value = serde_json::to_value(check_permissions()).expect("serialize permissions");
        assert!(value["inputMonitoring"]["granted"].is_boolean());
        assert!(value["inputMonitoring"]["state"].is_string());
    }

    /// 未知权限不得触发任何系统动作。
    #[test]
    fn request_unknown_permission_is_noop() {
        let result = request_permission("notARealPermission");
        assert_eq!(result.operation, PermissionOperation::Noop);
        assert_eq!(result.before, "unavailable");
        assert_eq!(result.after, "unavailable");
    }

    /// Info.plist 回退解析器必须只读取 CFBundleIdentifier。
    #[test]
    fn parses_cfbundle_identifier_from_minimal_plist() {
        let xml = r#"<dict><key>CFBundleName</key><string>cc-partner</string>
<key>CFBundleIdentifier</key><string>com.cc-partner.app.internal</string></dict>"#;
        assert_eq!(
            parse_cfbundle_identifier_plist_xml(xml).as_deref(),
            Some(input_monitoring::INTERNAL_BUNDLE_IDENTIFIER)
        );
        assert_eq!(parse_cfbundle_identifier_plist_xml("<dict></dict>"), None);
    }

    /// 应用身份必须稳定携带 flavor 字段。
    #[test]
    fn app_identity_serializes_flavor() {
        let value = serde_json::to_value(app_identity()).expect("serialize identity");
        assert!(value.get("flavor").is_some());
    }

    /// relaunch 的 shell 参数必须安全包裹空格和单引号。
    #[test]
    #[cfg(target_os = "macos")]
    fn shell_single_quote_escapes_apostrophe() {
        assert_eq!(shell_single_quote("a b"), "'a b'");
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
    }

    /// 设置 URL 必须覆盖输入监控且未知权限为空。
    #[test]
    #[cfg(target_os = "macos")]
    fn input_monitoring_settings_url_is_explicit() {
        assert!(settings_urls("inputMonitoring")
            .iter()
            .any(|url| url.contains("Privacy_ListenEvent")));
        assert!(settings_urls("unknown").is_empty());
    }
}
