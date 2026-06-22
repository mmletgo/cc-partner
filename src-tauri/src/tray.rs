//! tray.rs — 系统托盘（对照 Python `ui/tray.py`）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户最小化主窗口后仍需通过托盘快速访问「显示主窗口 / 截图 / 退出」，
//!     并通过 tooltip 了解应用名。本模块用 Tauri 2 的 tray API 构建。
//!
//! Code Logic（这个模块做什么）:
//!     `build_tray(app)` 创建托盘图标 + 右键菜单，注册菜单项与双击事件处理。
//!     菜单项：显示主窗口 / 截图（直接调 screenshot overlay 逻辑）/ 退出。

use crate::error::AppError;
use crate::screenshot::overlay;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// 截图菜单项 id。
const MENU_SHOW: &str = "tray_show";
/// 截图菜单项 id。
const MENU_SCREENSHOT: &str = "tray_screenshot";
/// 退出菜单项 id。
const MENU_QUIT: &str = "tray_quit";

/// 显示主窗口（若已隐藏则 show + set_focus）。
///
/// Business Logic: 托盘「显示主窗口」/ 双击托盘时调用。
fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// 创建并安装系统托盘（含图标、菜单、事件处理）。
///
/// Business Logic: 应用启动时在 setup 中调用一次。
/// Code Logic: 用 TrayIconBuilder 装配图标、tooltip、菜单；on_tray_icon_event 处理双击显窗；
///             on_menu_event 分发显示/截图/退出。退出直接 app.exit(0)。
pub fn build_tray(app: &AppHandle) -> Result<(), AppError> {
    let show_item = MenuItem::with_id(app, MENU_SHOW, "显示主窗口", true, None::<&str>)?;
    let shot_item = MenuItem::with_id(app, MENU_SCREENSHOT, "截图", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &shot_item, &quit_item])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            AppError::generic("缺少默认窗口图标，无法创建托盘")
        })?)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Claude Partner")
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_SCREENSHOT => {
                if let Err(e) = overlay::start_region_capture(app) {
                    tracing::error!("托盘触发截图失败: {e}");
                }
            }
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 双击托盘显示主窗口（对照 Python tray.py _on_activated DoubleClick）
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
