//! commands/screenshot.rs — 区域截图命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端通过 invoke 触发区域截图流程：开选区窗口、取背景、框选裁剪写剪贴板、取消。
//!     对照 Python `ScreenshotManager.take_screenshot` 及覆盖层的 screenshot_taken/screenshot_cancelled 信号。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_region_capture(app)`：每屏建透明置顶选区窗口。
//!     - `get_display_snapshot(display)`：返回该屏 PNG base64（前端 Overlay 背景）。
//!     - `crop_and_copy(app, display, x, y, w, h, dpr)`：逻辑坐标×dpr 裁剪写剪贴板，
//!       emit `region-capture:result` {ok:true}，并关全部 overlay。
//!     - `cancel_region_capture(app)`：emit `region-capture:result` {cancelled:true}，关全部 overlay。

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::error::AppError;
use crate::screenshot::{capture, overlay};

/// 启动区域截图：为每个显示器创建选区窗口。
///
/// Business Logic: 用户点截图按钮/快捷键时触发（快捷键在 M7 实现）。对应 Python `take_screenshot`。
/// Code Logic: 调 `overlay::start_region_capture`。M6 不做 macOS 屏幕录制权限预检（M7 实现），
///     直接尝试，未授权会抓到空白图——由 M7 权限模块兜底。
#[tauri::command]
pub async fn start_region_capture(app: AppHandle) -> Result<(), AppError> {
    overlay::start_region_capture(&app)
}

/// 获取指定显示器的 PNG base64 背景图。
///
/// Business Logic: Overlay 透明，需把桌面截图作背景让用户"像在直接框选屏幕"。
/// Code Logic: 调 `capture::snapshot_to_png_base64`。返回 `data:image/png;base64,...` 形式供前端 `<img>`。
#[tauri::command]
pub async fn get_display_snapshot(display: usize) -> Result<String, AppError> {
    capture::snapshot_to_png_base64(display)
}

/// 裁剪用户框选区域并写入剪贴板。
///
/// Business Logic: 用户在 Overlay 上 mouseup 后调用，坐标是逻辑像素，dpr 一起传 Rust 换算物理像素，
///     裁剪写剪贴板，成功后通知前端并关所有 overlay。对应 Python `mouseReleaseEvent` + `_on_screenshot_taken`。
/// Code Logic: `capture::crop_and_copy` → emit `region-capture:result` {ok:true} → `overlay::close_all_overlays`。
#[tauri::command]
pub async fn crop_and_copy(
    app: AppHandle,
    display: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<(), AppError> {
    capture::crop_and_copy(display, x, y, w, h, dpr)?;
    let _ = app.emit("region-capture:result", json!({ "ok": true }));
    overlay::close_all_overlays(&app);
    Ok(())
}

/// 取消区域截图。
///
/// Business Logic: ESC/右键/点空白时调用，通知前端取消并关所有 overlay。
///     对应 Python `keyPressEvent`(ESC) / 无效选区 → `screenshot_cancelled` 信号 + `_cleanup`。
/// Code Logic: emit `region-capture:result` {cancelled:true} → `overlay::close_all_overlays`。
#[tauri::command]
pub async fn cancel_region_capture(app: AppHandle) -> Result<(), AppError> {
    let _ = app.emit("region-capture:result", json!({ "cancelled": true }));
    overlay::close_all_overlays(&app);
    Ok(())
}
