//! screenshot/overlay.rs — 选区覆盖窗口管理（对照 Python overlay.py + capture.py 多屏创建）
//!
//! Business Logic（为什么需要这个模块）:
//!     macOS 不允许单个窗口跨屏（Linux 窗管也把 fullscreen 限制到单屏），Python 版为每个 QScreen
//!     创建独立 ScreenshotOverlay。Tauri 版同理：枚举 `xcap::Monitor::all()`，每个显示器建一个
//!     无边框透明置顶全屏窗口，加载同一个 React 选区页（带 `?display={i}` 参数）。
//!     窗口位置用该显示器逻辑原点、尺寸用物理分辨率÷scale（macOS 上 xcap x/y 为逻辑点、w/h 为物理像素），
//!     React 选区坐标相对该窗口。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_region_capture(app)`：枚举显示器 → 逐个 `WebviewWindowBuilder` 建 overlay 窗口
//!       （decorations(false)/transparent(true)/always_on_top(true)/focused(true)），
//!       url 指向 `/screenshot-overlay?display={i}`，label = `screenshot-overlay-{i}`，
//!       位置直接用 xcap 的 x/y（逻辑点），尺寸用 width/height ÷ scale_factor（物理像素→逻辑）。
//!     - `close_all_overlays(app)`：关闭所有 label 前缀 `screenshot-overlay-` 的窗口。

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::error::AppError;
use crate::screenshot::OVERLAY_LABEL_PREFIX;

/// 启动区域截图：为每个显示器创建一个透明置顶选区窗口。
///
/// Business Logic: 用户触发截图时需在每块屏幕上覆盖一个选区层。窗口透明、置顶、无边框，
///     载入 `/screenshot-overlay?display={i}` 页（React 渲染选区框）。
/// Code Logic: 先预检屏幕录制权限，未授权则显示主窗口 + emit `screenshot:permission-needed`
///     引导授权（不抓空白图）；已授权则枚举 `Monitor::all()`，逐个用 `WebviewWindowBuilder`
///     建窗口；macOS 上 xcap 的 x()/y() 来自 CGDisplayBounds 是逻辑点、width()/height() 是物理像素，
///     故位置直接用 x/y、尺寸用 width/height÷scale_factor 得逻辑像素后 set_position/set_size（Tauri 窗口几何按逻辑像素）。url 走 WebviewUrl::App 路径。
pub fn start_region_capture(app: &AppHandle) -> Result<(), AppError> {
    // 屏幕录制权限预检：未授权时 xcap 抓到空白图，改为显示主窗口 + emit 引导事件，不抓屏。
    // （此函数是命令层与 hotkey::screenshot_handler 的唯一入口，一处覆盖两条触发路径。）
    if !crate::permissions::check_screen_capture_access() {
        crate::tray::show_main_window(app);
        let _ = app.emit("screenshot:permission-needed", ());
        return Ok(());
    }

    let monitors = xcap::Monitor::all()
        .map_err(|e| AppError::Bad(format!("枚举显示器失败: {e}")))?;

    for (i, monitor) in monitors.into_iter().enumerate() {
        // macOS 单位：xcap 的 x()/y() 来自 CGDisplayBounds 是逻辑点（points），
        // width()/height() 是物理像素（backing pixels = 逻辑 × scale_factor）。
        let mx = monitor.x().unwrap_or(0);
        let my = monitor.y().unwrap_or(0);
        let mw = monitor.width().unwrap_or(1920) as f64;
        let mh = monitor.height().unwrap_or(1080) as f64;
        let scale = monitor.scale_factor().unwrap_or(1.0).max(0.0001) as f64;

        // Tauri 窗口几何按逻辑像素：位置直接用 x/y（已是逻辑点），尺寸用物理 ÷ scale。
        // （旧实现把 x/y 也除以 scale，混合 DPR 多屏下窗口位置会偏移/重叠到错误屏幕。）
        let logical_x = mx as f64;
        let logical_y = my as f64;
        let logical_w = mw / scale;
        let logical_h = mh / scale;

        tracing::info!(
            display = i,
            raw_x = mx, raw_y = my, raw_w = mw, raw_h = mh,
            scale, logical_x, logical_y, logical_w, logical_h,
            "截图选区窗口几何（raw_*: xcap 原值；logical_*: 喂给 Tauri 的逻辑像素）"
        );

        let label = format!("{OVERLAY_LABEL_PREFIX}{i}");
        let url = format!("/screenshot-overlay?display={i}");

        // 若已存在同名窗口（上次未清理），先关掉
        if let Some(existing) = app.get_webview_window(&label) {
            let _ = existing.close();
        }

        let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
            .title("Screenshot")
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .focused(true)
            .skip_taskbar(true)
            .resizable(false)
            .inner_size(logical_w, logical_h)
            .position(logical_x, logical_y);

        builder = builder.accept_first_mouse(true);

        builder
            .build()
            .map_err(|e| AppError::Bad(format!("创建选区窗口失败: {e}")))?;
    }

    Ok(())
}

/// 关闭所有选区覆盖窗口。
///
/// Business Logic: 截图完成（裁剪写剪贴板）或用户取消（ESC/右键）后必须清理所有 overlay 窗口。
/// Code Logic: 遍历 `app.webview_windows()`，label 以 `screenshot-overlay-` 前缀开头则 close()。
pub fn close_all_overlays(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with(OVERLAY_LABEL_PREFIX) {
            let _ = win.close();
        }
    }
}
