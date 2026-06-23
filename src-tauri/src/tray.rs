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
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

/// 截图菜单项 id。
const MENU_SHOW: &str = "tray_show";
/// 截图菜单项 id。
const MENU_SCREENSHOT: &str = "tray_screenshot";
/// 暂停/恢复健康监测菜单项 id（toggle：点击切换 paused 原子标记）。
const MENU_PAUSE: &str = "tray_pause";
/// 退出菜单项 id。
const MENU_QUIT: &str = "tray_quit";

/// 构造托盘图标。
///
/// Business Logic: 用户会把托盘作为后台常驻入口；macOS 菜单栏需要单色 template icon，
///     彩色 Dock 图标缩小后会显得突兀，也无法自动适配深浅色菜单栏。
/// Code Logic: 优先从编译期嵌入的 `icons/tray-icon.png` 解码为 Tauri Image；若解码失败，
///     回退到默认窗口图标，避免图标资源异常时阻断托盘创建。
fn app_tray_icon(app: &AppHandle) -> Result<Image<'static>, AppError> {
    match image::load_from_memory(include_bytes!("../icons/tray-icon.png")) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            Ok(Image::new_owned(rgba.into_raw(), width, height))
        }
        Err(e) => {
            tracing::warn!("读取内嵌托盘图标失败，回退默认窗口图标: {e}");
            app.default_window_icon()
                .cloned()
                .map(Image::to_owned)
                .ok_or_else(|| AppError::generic("缺少默认窗口图标，无法创建托盘"))
        }
    }
}

/// 显示主窗口（若已隐藏则 show + set_focus）。
///
/// Business Logic: 托盘「显示主窗口」/ 双击托盘时调用；截图预检发现屏幕录制未授权时复用
///     本函数唤起主窗口跳转引导页（规则 9 复用，避免 overlay 重复实现显窗逻辑）。
pub(crate) fn show_main_window(app: &AppHandle) {
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
    let pause_item = MenuItem::with_id(app, MENU_PAUSE, "暂停/恢复监测", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, MENU_QUIT, "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &shot_item, &pause_item, &quit_item])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app_tray_icon(app)?)
        .icon_as_template(true)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("cc-partner")
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_SCREENSHOT => {
                if let Err(e) = overlay::start_region_capture(app) {
                    tracing::error!("托盘触发截图失败: {e}");
                }
            }
            MENU_PAUSE => {
                // toggle 健康监测暂停标记（AtomicBool，不落盘，重启失效）。
                // 对照 commands/health.rs::toggle_health_paused 的语义，复用同一份运行时标记。
                use std::sync::atomic::Ordering;
                let state: tauri::State<crate::state::AppState> = app.state();
                let cur = state.health.paused.load(Ordering::Relaxed);
                state.health.paused.store(!cur, Ordering::Relaxed);
                tracing::info!("健康监测 {}", if !cur { "已暂停" } else { "已恢复" });
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
