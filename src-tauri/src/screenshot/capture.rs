//! screenshot/capture.rs — 抓屏、裁剪、剪贴板写入（对照 Python capture.py）
//!
//! Business Logic（为什么需要这个模块）:
//!     区域截图的核心：抓屏（物理像素帧）→ 裁剪用户框选的 rect → 写系统剪贴板。
//!     Python 用 Qt QPixmap + QGuiApplication.clipboard()，Rust 侧用 xcap 抓屏 +
//!     image crate 裁剪 + arboard 写剪贴板。所有像素均为物理像素（Retina 下 ×dpr），
//!     与 Python 中 `_screenshot.copy(device_rect)` 的 DPR 换算语义一致。
//!
//! Code Logic（这个模块做什么）:
//!     - `capture_monitor(display_index)`：取 `xcap::Monitor::all()` 第 index 个显示器抓整屏。
//!     - `crop_and_copy(display_index, x, y, w, h, dpr)`：重抓该屏帧，按 ×dpr 换算物理像素 rect，
//!       `image::imageops::crop_imm` 裁剪，`arboard::Clipboard::set_image` 写剪贴板。

use arboard::{Clipboard, ImageData};
use image::RgbaImage;
use xcap::Monitor;

use crate::error::AppError;

/// 取第 `display_index` 个显示器对象。
///
/// Business Logic: `xcap::Monitor::all()` 的顺序在单次进程内稳定，前端 Overlay 用同一 index
///     取背景与裁剪，保证两处指向同一台显示器。
/// Code Logic: `Monitor::all()?` 枚举全部显示器，按 index 取，越界返回 Bad 错误。
fn get_monitor(display_index: usize) -> Result<Monitor, AppError> {
    let monitors = Monitor::all().map_err(|e| AppError::Bad(format!("枚举显示器失败: {e}")))?;
    monitors
        .into_iter()
        .nth(display_index)
        .ok_or_else(|| AppError::Bad(format!("显示器 index {display_index} 不存在")))
}

/// 抓取指定显示器的整屏帧（物理像素）。
///
/// Business Logic: 区域截图先抓整屏作背景/裁剪源。xcap 返回物理像素（Retina 为逻辑尺寸 ×dpr），
///     与 Python `screen.grabWindow(0)` 的 devicePixelRatio 行为对齐。
/// Code Logic: `monitor.capture_image()` 直接返回 `image::RgbaImage`（物理像素）。
pub fn capture_monitor(display_index: usize) -> Result<RgbaImage, AppError> {
    let monitor = get_monitor(display_index)?;
    monitor
        .capture_image()
        .map_err(|e| AppError::Bad(format!("抓屏失败: {e}")))
}

/// 裁剪指定显示器上的选区并写入系统剪贴板。
///
/// Business Logic: 用户在 Overlay 上框选（逻辑像素）后，前端把逻辑坐标 + `window.devicePixelRatio`
///     一起传过来；裁剪必须用物理像素（xcap 帧即物理像素），所以这里 ×dpr 换算后裁剪，
///     再写剪贴板，让用户可直接粘贴到 Claude Code。对应 Python `mouseReleaseEvent` 的 copy + clipboard.setPixmap。
///
/// Code Logic:
///     1. 重抓该显示器帧（避免背景图经过 PNG 往返的损耗）。
///     2. 逻辑坐标 ×dpr 四舍五入成物理像素 rect。
///     3. clamp 到帧边界，`crop_imm` 裁剪。
///     4. RGBA bytes 直接喂 `arboard::ImageData`，`Clipboard::new()?.set_image(...)` 写剪贴板。
pub fn crop_and_copy(
    display_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<(), AppError> {
    let img = capture_monitor(display_index)?;
    let img_w = img.width();
    let img_h = img.height();

    // 逻辑像素 → 物理像素（×dpr 四舍五入），与 Python `int(selection.x() * dpr)` 一致
    let scale = |v: u32| -> u32 { (v as f64 * dpr).round().max(0.0) as u32 };
    let mut px = scale(x);
    let mut py = scale(y);
    let mut pw = scale(w);
    let mut ph = scale(h);

    // clamp 到帧边界（防止 dpr 换算后越界触发 crop_imm panic）
    if px >= img_w {
        px = img_w.saturating_sub(1);
    }
    if py >= img_h {
        py = img_h.saturating_sub(1);
    }
    if px + pw > img_w {
        pw = img_w - px;
    }
    if py + ph > img_h {
        ph = img_h - py;
    }
    if pw == 0 || ph == 0 {
        return Err(AppError::Bad("裁剪区域为空（选区过小或越界）".into()));
    }

    // image 0.25 的 crop_imm 直接返回 SubImage（非 Result），to_image 拷贝出独立 RgbaImage。
    // 因上方已 clamp 到帧边界，此处不会越界。
    let cropped = image::imageops::crop_imm(&img, px, py, pw, ph).to_image();

    // `to_image()` 返回独立 RgbaImage，`into_raw()` 即连续 RGBA 字节缓冲，可直接喂 arboard。
    let (w_out, h_out) = (cropped.width() as usize, cropped.height() as usize);
    let bytes = cropped.into_raw();
    let img_data = ImageData {
        width: w_out,
        height: h_out,
        bytes: bytes.into(),
    };

    let mut cb = Clipboard::new()
        .map_err(|e| AppError::Bad(format!("打开剪贴板失败: {e}")))?;
    cb.set_image(img_data)
        .map_err(|e| AppError::Bad(format!("写入剪贴板失败: {e}")))?;
    Ok(())
}
