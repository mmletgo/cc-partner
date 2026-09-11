//! screenshot/overlay.rs — 选区覆盖窗口管理（对照 Python overlay.py + capture.py 多屏创建）
//!
//! Business Logic（为什么需要这个模块）:
//!     macOS 不允许单个窗口跨屏（Linux 窗管也把 fullscreen 限制到单屏），Python 版为每个 QScreen
//!     创建独立 ScreenshotOverlay。Tauri 版同理：枚举去重后的显示器，每个唯一屏建一个
//!     无边框透明置顶全屏窗口，加载同一个 React 选区页（带 `?display={i}` 参数）。
//!     窗口位置/尺寸均用该显示器逻辑几何（macOS 上 xcap 的 x/y/w/h 均为逻辑点），
//!     React 选区坐标相对该窗口。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_region_capture(app)`：枚举去重后的显示器 → 关掉多余旧窗 → 已有同名窗口则复用
//!       （改几何 + 重载页面重置状态 + show/focus），没有才新建（decorations(false)/transparent(true)/
//!       always_on_top(true)/focused(true)），url 指向 `/screenshot-overlay?display={i}`，
//!       label = `screenshot-overlay-{i}`，位置/尺寸均直接用 xcap 的 x/y/w/h（均为逻辑点）。
//!     - `hide_all_overlays(app)`：隐藏（不销毁）所有 label 前缀 `screenshot-overlay-` 的窗口。
//!     - macOS 26/27 WebKit 崩溃规避：macOS 27.0 系统 WebKit 存在 UI 进程 SIGSEGV
//!       （`ScrollingTree::takePendingScrollUpdates()` 经 display link `didRefreshDisplay` 解引用空指针，
//!       上游 main 已加 `page()` 空判断而 27.0 无保护）。选区窗口如果每次截图都 create+destroy，
//!       「display link 回调仍在跑时页面被销毁」的竞态被高频触发（实测崩溃报告 cc-partner-2026-09-11）。
//!       因此本模块把 overlay 改成**跨会话复用**：会话结束只 `hide()`（WKWebView 不可见后
//!       display link 随之暂停），下次截图改几何 + `location.reload()` 重置后复用；确需销毁
//!       （显示器减少留下的越界窗口）也先 `hide()` 再 `close()`，把销毁挪出 display link 活跃窗口。

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::error::AppError;
use crate::monitor_geom::{extra_prefixed_overlay_labels, list_unique_xcap_monitors};
use crate::screenshot::OVERLAY_LABEL_PREFIX;

/// 启动区域截图：为每个显示器复用或创建一个透明置顶选区窗口。
///
/// Business Logic: 用户触发截图时需在每块屏上盖一个选区层。窗口透明、置顶、无边框，
///     加载 `/screenshot-overlay?display={i}` 页（React 渲染选区框）。
/// Code Logic: 先预检屏幕录制权限，未授权则显示主窗口 + emit `screenshot:permission-needed`
///     引导授权（不抓空白图）；已授权则枚举去重后的显示器，先隐藏并关闭越界旧窗
///     （显示器减少的场景），再逐屏处理：已有同名窗口则复用（更新几何 + 重载页面重置 React
///     状态 + show/focus），否则 `WebviewWindowBuilder` 新建；macOS 上 xcap 的 x()/y()/width()/height()
///     均为逻辑点，直接喂窗口几何（Tauri 窗口几何按逻辑像素）；只有 capture_image() 返回的帧
///     才是物理像素（裁剪时前端 ×dpr 换算）。url 走 WebviewUrl::App 路径。
///     复用而非新建是为了规避 macOS 27.0 WebKit「display link 回调 × 页面销毁」竞态 SIGSEGV
///     （见模块头注释）。
pub fn start_region_capture(app: &AppHandle) -> Result<(), AppError> {
    // 屏幕录制权限预检：未授权时 xcap 抓到空白图，改为显示主窗口 + emit 引导事件，不抓屏。
    // （此函数是命令层与 hotkey::screenshot_handler 的唯一入口，一处覆盖两条触发路径。）
    if !crate::permissions::check_screen_capture_access() {
        crate::tray::show_main_window(app);
        let _ = app.emit("screenshot:permission-needed", ());
        return Ok(());
    }

    let monitors = list_unique_xcap_monitors()?;
    let existing: Vec<String> = app
        .webview_windows()
        .into_keys()
        .filter(|label| label.starts_with(OVERLAY_LABEL_PREFIX))
        .collect();
    for label in extra_prefixed_overlay_labels(OVERLAY_LABEL_PREFIX, monitors.len(), &existing) {
        if let Some(win) = app.get_webview_window(&label) {
            hide_then_close(&win);
        }
    }

    for (i, monitor) in monitors.into_iter().enumerate() {
        // macOS 单位：xcap 的 x()/y()/width()/height() 均为**逻辑点**（points）；
        // 只有 capture_image() 返回的帧才是物理像素（逻辑 × scale_factor）。
        let mx = monitor.x().unwrap_or(0);
        let my = monitor.y().unwrap_or(0);
        let mw = monitor.width().unwrap_or(1920) as f64;
        let mh = monitor.height().unwrap_or(1080) as f64;
        let scale = monitor.scale_factor().unwrap_or(1.0).max(0.0001) as f64;

        // Tauri 窗口几何按逻辑像素：x/y/w/h 都已是逻辑点，直接用，不除 scale。
        // （曾误把 w/h 当物理像素除以 scale，scale>1 的屏窗口尺寸减半 → 遮罩只盖半屏「缩略」；已修。）
        let logical_x = mx as f64;
        let logical_y = my as f64;
        let logical_w = mw;
        let logical_h = mh;

        tracing::info!(
            display = i,
            raw_x = mx,
            raw_y = my,
            raw_w = mw,
            raw_h = mh,
            scale,
            logical_x,
            logical_y,
            logical_w,
            logical_h,
            "截图选区窗口几何（raw_*: xcap 原值；logical_*: 喂给 Tauri 的逻辑像素）"
        );

        let label = format!("{OVERLAY_LABEL_PREFIX}{i}");
        let url = format!("/screenshot-overlay?display={i}");

        // macOS 27 WebKit 崩溃规避：已有同名窗口（上次会话隐藏留用）直接复用，不再 close+rebuild。
        if let Some(win) = app.get_webview_window(&label) {
            reuse_overlay(&win, logical_x, logical_y, logical_w, logical_h);
            continue;
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

/// 复用一个已存在的选区窗口开启新会话。
///
/// Business Logic: macOS 27.0 WebKit 在 display link 回调与页面销毁竞态时会 SIGSEGV，
///     选区窗口必须跨截图会话复用而不是每次新建，复用时要做到用户无感（几何/状态与新建一致）。
/// Code Logic: 先按当前显示器逻辑几何 set_position/set_size（显示器布局可能已变），
///     再 `location.reload()` 重载 React 选区页重置状态机（等价新建时的初始 idle 态），
///     最后 show + set_focus 置顶抢焦点（等价新建的 focused(true)）。
fn reuse_overlay(
    win: &tauri::WebviewWindow,
    logical_x: f64,
    logical_y: f64,
    logical_w: f64,
    logical_h: f64,
) {
    let _ = win.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(
        logical_x, logical_y,
    )));
    let _ = win.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
        logical_w, logical_h,
    )));
    // 窗口此刻处于隐藏态，重载页面重置选区状态；完成后 show/focus 进入新会话。
    let _ = win.eval("location.reload()");
    let _ = win.show();
    let _ = win.set_focus();
}

/// 销毁一个不再需要的选区窗口：先隐藏再关闭。
///
/// Business Logic: 显示器减少时越界的旧选区窗口必须销毁；但 macOS 27.0 WebKit 在
///     可见窗口的 display link 回调仍活跃时直接 close 会触发「回调 × 页面销毁」竞态 SIGSEGV。
/// Code Logic: 先 `hide()`——WKWebView 不可见后 display link 随之暂停；再 `close()` 销毁。
///     两个操作按序投递到主线程事件循环，close 处理时 display link 已停，规避竞态。
fn hide_then_close(win: &tauri::WebviewWindow) {
    let _ = win.hide();
    let _ = win.close();
}

/// 结束选区会话：隐藏（不销毁）所有选区覆盖窗口。
///
/// Business Logic: 截图完成（裁剪写剪贴板）或用户取消（ESC/右键）后，所有 overlay 必须
///     立即从屏幕消失；但 macOS 27.0 WebKit 存在「display link 回调 × 页面销毁」竞态 SIGSEGV
///     （见模块头注释），不能像旧版那样每次销毁窗口，改为隐藏留待 `start_region_capture` 复用。
/// Code Logic: 遍历 `app.webview_windows()`，label 以 `screenshot-overlay-` 前缀开头则 hide()。
pub fn hide_all_overlays(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with(OVERLAY_LABEL_PREFIX) {
            let _ = win.hide();
        }
    }
}
