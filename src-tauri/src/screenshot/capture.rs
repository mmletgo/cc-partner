//! screenshot/capture.rs — 抓屏、裁剪、快照、剪贴板写入
//!
//! Business Logic（为什么需要这个模块）:
//!     区域截图的核心能力：抓屏（物理像素帧）→ 裁剪选区 → 编码 PNG / 写系统剪贴板。
//!     编辑工具条流程下，抓屏与剪贴板写入解耦：前端 canvas 合成「桌面+标注」PNG 后，
//!     由 save_clipboard_from_png 解码写剪贴板；capture_region 仅供前端取选区桌面快照。
//!
//! Code Logic（这个模块做什么）:
//!     - `capture_monitor(display_index)`：取去重后第 index 显示器抓整屏（物理像素）。
//!     - `clamp_crop_rect(...)`：逻辑坐标 ×dpr → 物理像素 rect，clamp 到帧边界（纯函数，单测覆盖）。
//!     - `capture_region(...)`：抓屏 + clamp_crop_rect + crop_imm，返回选区 RgbaImage。
//!     - `region_to_png_base64(...)`：capture_region → PNG → base64 data URL（前端 canvas 背景）。
//!     - `save_clipboard_from_png(data_url)`：剥 data URL 前缀 → 解码 → `clipboard::write_os_clipboard_image`。
//!     - `decode_image_data_url_to_rgba` / `read_clipboard_image_png_data_url`：终端图片粘贴读写剪贴板。

use std::io::Cursor;

use arboard::Clipboard;
use image::{DynamicImage, ImageFormat, RgbaImage};

use crate::screenshot::clipboard::{write_os_clipboard_image, ClipboardWriteFailure};
use xcap::Monitor;

/// 终端/截图写入剪贴板的解码后图片上限（8 MiB）。
pub const MAX_CLIPBOARD_IMAGE_BYTES: usize = 8 * 1024 * 1024;

use crate::error::AppError;

/// 取第 `display_index` 个显示器对象。
///
/// Business Logic: 前端 Overlay 的 `display={i}` 与抓屏必须指向同一块去重后的屏；
///     Ubuntu 上 raw `Monitor::all()` 含重叠 output，不能直接按 raw index 取。
/// Code Logic: 与开窗共用 `list_unique_xcap_monitors()`，按去重后 index 取，越界返回 Bad。
fn get_monitor(display_index: usize) -> Result<Monitor, AppError> {
    let monitors = crate::monitor_geom::list_unique_xcap_monitors()?;
    monitors
        .into_iter()
        .nth(display_index)
        .ok_or_else(|| AppError::Bad(format!("显示器 index {display_index} 不存在")))
}

/// 抓取指定显示器的整屏帧（物理像素）。
///
/// Business Logic: 区域截图先抓整屏作裁剪源。xcap capture_image 返回物理像素（Retina 为逻辑 ×scale）。
/// Code Logic: `monitor.capture_image()` 直接返回 `image::RgbaImage`（物理像素）。
pub fn capture_monitor(display_index: usize) -> Result<RgbaImage, AppError> {
    let monitor = get_monitor(display_index)?;
    monitor
        .capture_image()
        .map_err(|e| AppError::Bad(format!("抓屏失败: {e}")))
}

/// 逻辑坐标 ×dpr → 物理像素 rect，clamp 到帧 `(img_w, img_h)` 边界。
///
/// Business Logic: 前端传逻辑像素 + dpr，xcap 帧是物理像素，需 ×dpr 换算；dpr 换算可能越界，clamp 防止
///     `crop_imm` panic。抽成纯函数便于单测。
/// Code Logic: 逐边 clamp：px>=img_w 收到 img_w-1；px+pw>img_w 截断 pw；pw/ph 为 0 返回 Err。
pub fn clamp_crop_rect(
    img_w: u32,
    img_h: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<(u32, u32, u32, u32), AppError> {
    let scale = |v: u32| -> u32 { (v as f64 * dpr).round().max(0.0) as u32 };
    let mut px = scale(x);
    let mut py = scale(y);
    let mut pw = scale(w);
    let mut ph = scale(h);
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
    Ok((px, py, pw, ph))
}

/// 抓指定显示器 + 按选区裁剪，返回选区 RgbaImage（物理像素）。
///
/// Business Logic: 编辑模式下前端需「该选区的纯桌面」作 canvas 背景；本函数返回裁剪后的选区帧。
/// Code Logic: `capture_monitor` → `clamp_crop_rect` → `crop_imm(...).to_image()`。
pub fn capture_region(
    display_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<RgbaImage, AppError> {
    let img = capture_monitor(display_index)?;
    let (px, py, pw, ph) = clamp_crop_rect(img.width(), img.height(), x, y, w, h, dpr)?;
    Ok(image::imageops::crop_imm(&img, px, py, pw, ph).to_image())
}

/// 抓指定显示器选区并编码成 PNG base64 data URL（前端 canvas 背景）。
///
/// Business Logic: 前端编辑模式 canvas 需桌面快照作底图（drawImage），所见即所得。
/// Code Logic: `capture_region` → PNG 编码到 `Cursor<Vec<u8>>` → `base64::STANDARD` → 拼 data URL。
pub fn region_to_png_base64(
    display_index: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dpr: f64,
) -> Result<String, AppError> {
    let img = capture_region(display_index, x, y, w, h, dpr)?;
    let mut buf = Cursor::new(Vec::with_capacity(512 * 1024));
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| AppError::Bad(format!("PNG 编码失败: {e}")))?;
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok(format!("data:image/png;base64,{b64}"))
}

/// 把图片 data URL 解码为 RGBA 像素。
///
/// Business Logic（为什么需要这个函数）:
///     截图确认与终端图片粘贴都要把浏览器/对端传来的 data URL 变成系统剪贴板能写的像素。
///
/// Code Logic（这个函数做什么）:
///     接受 `data:image/(png|jpeg|jpg);base64,...`；校验体积后 base64 解码并用 `image` crate 转 RGBA。
pub fn decode_image_data_url_to_rgba(data_url: &str) -> Result<(usize, usize, Vec<u8>), AppError> {
    let (header, b64) = data_url
        .split_once(',')
        .ok_or_else(|| AppError::Bad("无效的图片 data URL".into()))?;
    let header_lc = header.to_ascii_lowercase();
    if !header_lc.starts_with("data:image/") || !header_lc.contains("base64") {
        return Err(AppError::Bad(
            "无效的图片 data URL（需要 image/*;base64）".into(),
        ));
    }
    if b64.len() > MAX_CLIPBOARD_IMAGE_BYTES.saturating_mul(2) {
        return Err(AppError::Bad("粘贴图片过大".into()));
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| AppError::Bad(format!("base64 解码失败: {e}")))?;
    if bytes.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(AppError::Bad("粘贴图片过大".into()));
    }
    let img = image::load_from_memory(&bytes)
        .map_err(|e| AppError::Bad(format!("图片解码失败: {e}")))?
        .to_rgba8();
    Ok((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// 把前端 canvas 合成的 PNG data URL 解码后写入系统剪贴板。
///
/// Business Logic: 用户点「确认」后，前端把「桌面选区 + 标注」合成的 PNG 传过来写剪贴板，
///     可直接粘贴到 Claude Code。终端远端粘贴也复用同一写入路径。
/// Code Logic: 解码 data URL → RGBA → 经 `clipboard::write_os_clipboard_image` 写系统剪贴板。
pub fn save_clipboard_from_png(data_url: &str) -> Result<(), AppError> {
    save_clipboard_from_image_data_url(data_url)
}

/// 把任意受支持的图片 data URL 写入系统剪贴板。
///
/// Business Logic（为什么需要这个函数）:
///     区域截图确认与无回退的调用方需要「写失败就报错」；Agent 贴图走 `prepare_agent_image_paste`。
///
/// Code Logic（这个函数做什么）:
///     委托 `write_os_clipboard_image`；显示服务器不可用时返回明确错误。
pub fn save_clipboard_from_image_data_url(data_url: &str) -> Result<(), AppError> {
    match write_os_clipboard_image(data_url) {
        Ok(()) => Ok(()),
        Err(ClipboardWriteFailure::DisplayUnavailable) => Err(AppError::Bad(
            "打开剪贴板失败: 当前环境没有可用的图形剪贴板（X11/Wayland）".into(),
        )),
        Err(ClipboardWriteFailure::Other(msg)) => {
            Err(AppError::Bad(format!("打开剪贴板失败: {msg}")))
        }
    }
}

/// 读取系统剪贴板中的图片并编码为 PNG data URL。
///
/// Business Logic（为什么需要这个函数）:
///     macOS Ctrl+V 不会触发浏览器 paste 事件；GUI 进程必须从 OS pasteboard 取出图片再转发。
///
/// Code Logic（这个函数做什么）:
///     `Clipboard::get_image` 失败视为无图（Ok(None)）；成功则编码 PNG data URL，超限返回错误。
pub fn read_clipboard_image_png_data_url() -> Result<Option<String>, AppError> {
    let mut cb = Clipboard::new().map_err(|e| AppError::Bad(format!("打开剪贴板失败: {e}")))?;
    let img = match cb.get_image() {
        Ok(img) => img,
        Err(_) => return Ok(None),
    };
    let width = u32::try_from(img.width).map_err(|_| AppError::Bad("剪贴板图片宽度非法".into()))?;
    let height =
        u32::try_from(img.height).map_err(|_| AppError::Bad("剪贴板图片高度非法".into()))?;
    let rgba = RgbaImage::from_raw(width, height, img.bytes.into_owned())
        .ok_or_else(|| AppError::Bad("剪贴板图片尺寸与像素不匹配".into()))?;
    let mut buf = Cursor::new(Vec::with_capacity(64 * 1024));
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| AppError::Bad(format!("PNG 编码失败: {e}")))?;
    let png = buf.into_inner();
    if png.len() > MAX_CLIPBOARD_IMAGE_BYTES {
        return Err(AppError::Bad("剪贴板图片过大".into()));
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    Ok(Some(format!("data:image/png;base64,{b64}")))
}

#[cfg(test)]
mod tests {
    use super::clamp_crop_rect;

    #[test]
    fn clamp_normal_within_bounds() {
        // 100×100 帧，选区 (10,10,30,30)，dpr=2 → 物理 (20,20,60,60)，右下到 (80,80) 未越界
        let (x, y, w, h) = clamp_crop_rect(100, 100, 10, 10, 30, 30, 2.0).unwrap();
        assert_eq!((x, y, w, h), (20, 20, 60, 60));
    }

    #[test]
    fn clamp_overflow_to_frame_edge() {
        let (x, y, w, h) = clamp_crop_rect(100, 100, 45, 45, 20, 20, 2.0).unwrap();
        assert_eq!((x, y, w, h), (90, 90, 10, 10));
    }

    #[test]
    fn clamp_empty_returns_err() {
        assert!(clamp_crop_rect(100, 100, 0, 0, 0, 10, 1.0).is_err());
    }

    /// 1×1 透明 PNG data URL，供解码契约测试使用。
    const ONE_PX_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    /// Business Logic（为什么需要这个测试）:
    ///     终端粘贴与截图写剪贴板共用解码入口，非法/过大 payload 必须在写 pasteboard 前失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     合法 1×1 PNG 得到 1×1 RGBA；缺前缀与超大 base64 返回错误。
    #[test]
    fn decode_image_data_url_accepts_png_and_rejects_invalid() {
        let (w, h, raw) = super::decode_image_data_url_to_rgba(ONE_PX_PNG).expect("1px png");
        assert_eq!((w, h), (1, 1));
        assert_eq!(raw.len(), 4);
        assert!(super::decode_image_data_url_to_rgba("not-a-data-url").is_err());
        let oversized = format!(
            "data:image/png;base64,{}",
            "A".repeat(super::MAX_CLIPBOARD_IMAGE_BYTES * 2 + 8)
        );
        assert!(super::decode_image_data_url_to_rgba(&oversized).is_err());
    }
}
