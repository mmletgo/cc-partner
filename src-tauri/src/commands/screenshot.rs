//! commands/screenshot.rs — 区域截图命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端通过 invoke 触发区域截图流程：开选区窗口、进编辑模式取选区快照、确认后写剪贴板、取消。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_region_capture(app)`：每屏复用或新建透明置顶选区窗口（macOS 27 WebKit
//!       「display link × 页面销毁」竞态 SIGSEGV 规避：窗口跨会话复用，不再每次销毁）。
//!     - `get_region_snapshot(display, x, y, w, h, dpr)`：抓该屏纯桌面选区，返回 PNG base64。
//!     - `save_clipboard_image(app, dataUrl)`：把前端合成的 PNG data URL 写剪贴板 + 隐藏全部 overlay。
//!     - `read_clipboard_image()`：GUI 进程读取 OS 剪贴板图片，返回 PNG data URL 或 null。
//!     - `cancel_region_capture(app)`：emit `region-capture:result` {cancelled:true}，隐藏全部 overlay。

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::error::AppError;
use crate::screenshot::{capture, overlay};

/// 启动区域截图：为每个显示器创建选区窗口。
///
/// Business Logic: 用户按截图快捷键 / 点托盘「截图」/ 点前端按钮，要在每块屏上拉起透明置顶的选区窗口让用户框选。
///     若屏幕录制权限未授权，不抓空白图，而是显示主窗口 + emit `screenshot:permission-needed` 引导用户授权。
/// Code Logic: 委托 `overlay::start_region_capture(&app)`（内部先预检权限，未授权引导；已授权则枚举 Monitor::all 每屏建 overlay 窗口）。
#[tauri::command]
pub async fn start_region_capture(app: AppHandle) -> Result<(), AppError> {
    overlay::start_region_capture(&app)
}

/// 获取指定显示器选区的纯桌面快照（PNG base64），供前端编辑模式 canvas 作背景。
///
/// Business Logic: 用户框选确定进编辑模式时，需「该选区不含 overlay 的纯桌面」作 canvas 底图
///     （前端在 invoke 前已 hiding 隐藏 overlay，故 Rust 抓到的是纯桌面）。
/// Code Logic: 调 `capture::region_to_png_base64`，返回 `data:image/png;base64,...`。
#[tauri::command]
pub async fn get_region_snapshot(
    display: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<String, AppError> {
    capture::region_to_png_base64(display, x, y, w, h, dpr)
}

/// 把前端 canvas 合成的「桌面+标注」PNG 写入剪贴板，并结束本次选区会话（隐藏所有 overlay）。
///
/// Business Logic: 用户点「确认」后，前端把 canvas.toDataURL（桌面选区 + 标注）传过来，
///     Rust 解码写剪贴板（可直接粘贴到 Claude Code），成功后 overlay 从屏幕消失。
/// Code Logic: `capture::save_clipboard_from_png` → emit `region-capture:result` {ok:true} →
///     `overlay::hide_all_overlays`（隐藏而非销毁：macOS 27 WebKit「display link × 页面销毁」
///     竞态 SIGSEGV 规避，窗口留待下次截图复用）。
#[tauri::command]
pub async fn save_clipboard_image(app: AppHandle, data_url: String) -> Result<(), AppError> {
    capture::save_clipboard_from_png(&data_url)?;
    let _ = app.emit("region-capture:result", json!({ "ok": true }));
    overlay::hide_all_overlays(&app);
    Ok(())
}

/// 读取本机 GUI 进程看到的系统剪贴板图片。
///
/// Business Logic（为什么需要这个命令）:
///     macOS Ctrl+V 不触发 paste 事件；必须在用户复制图片的那台机器上读 pasteboard，
///     不能让 sidecar/远端去读空剪贴板。
///
/// Code Logic（这个命令做什么）:
///     在 GUI 进程调用 `read_clipboard_image_png_data_url`，无图返回 null。
#[tauri::command]
pub fn read_clipboard_image() -> Result<Option<String>, AppError> {
    capture::read_clipboard_image_png_data_url()
}

/// 取消区域截图。
///
/// Business Logic: 用户在选区窗口按 ESC / 右键 / 点工具条「取消」，或框选选区过小，要中止本次截图流程并让全部 overlay 从屏幕消失。
/// Code Logic: emit `region-capture:result` {cancelled:true}（前端据此清状态）→ `overlay::hide_all_overlays`
///     （按 label 前缀 `screenshot-overlay-` 隐藏所有选区窗口而非销毁：macOS 27 WebKit
///     「display link × 页面销毁」竞态 SIGSEGV 规避，窗口留待下次截图复用）。
#[tauri::command]
pub async fn cancel_region_capture(app: AppHandle) -> Result<(), AppError> {
    let _ = app.emit("region-capture:result", json!({ "cancelled": true }));
    overlay::hide_all_overlays(&app);
    Ok(())
}
