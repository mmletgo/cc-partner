//! screenshot — 区域截图模块（M6）
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要在屏幕上框选区域截图，截图后写入剪贴板，可直接粘贴到 Claude Code。
//!     迁移自 Python `screenshot/overlay.py` + `capture.py`：选区交互从 Qt QWidget 自绘
//!     改为 Tauri 透明置顶窗口 + React 选区页；抓屏本体用跨平台的 `xcap` crate（物理像素）。
//!
//! Code Logic（这个模块做什么）:
//!     - `capture::capture_monitor`：抓取指定显示器的整屏帧（物理像素 RgbaImage）。
//!     - `capture::crop_and_copy`：按物理像素 rect 裁剪并写剪贴板。
//!     - `capture::snapshot_to_png_base64`：抓该屏帧编码成 PNG base64 返回前端作背景。
//!     - `overlay::start_region_capture`：枚举显示器，每个显示器创建一个透明置顶全屏窗口。
//!     - `overlay::close_all_overlays`：关闭所有选区窗口。

pub mod capture;
pub mod overlay;

/// 选区窗口 label 前缀；`overlay::start_region_capture` 按 `screenshot-overlay-{i}` 命名每个窗口，
/// `close_all_overlays` 与 `commands::screenshot` 关闭时按此前缀匹配。
pub const OVERLAY_LABEL_PREFIX: &str = "screenshot-overlay-";
