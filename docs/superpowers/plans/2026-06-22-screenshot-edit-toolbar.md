# 截图编辑工具条 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 区域截图框选后进入编辑模式，工具条提供矩形/箭头标注（6 色板 + 固定线宽）+ 撤销，确认后前端 canvas 合成「桌面+标注」复制剪贴板。

**Architecture:** 方案 A —— 前端 canvas 绘制 + 前端合成（所见即所得）。框选确定后 Rust 抓「选区纯桌面快照」base64 传前端；前端 canvas 画快照 + 标注；确认 `canvas.toDataURL` 合成传 Rust 写剪贴板。Rust 只抓快照/写剪贴板，不画标注。

**Tech Stack:** Tauri 2 + Rust（xcap 抓屏 / image 裁剪编解码 / arboard 剪贴板 / base64）/ React 19 + TypeScript（canvas 2D / CSS Modules）

## Global Constraints

- **执行隔离**：在 git worktree 开发（项目规则 14），完成后合并回 master。worktree 用 `superpowers:using-git-worktrees` 在执行时创建，基于 origin/master（已含基线 `26b58cb` + spec `9b40fb2`）。
- **Rust 注释**：所有函数含中文 Business/Code Logic 注释（项目规则）；非数据库改动不向后兼容（规则 15）。
- **前端**：hooks 必须在所有 early return 之前（项目规则 20）；组件内部样式用 CSS Modules（web 约定）；无前端单元测试框架，前端任务用 `npx tsc --noEmit` + 手动验证步骤替代 failing-test。
- **serde**：返回前端的 struct 用 `#[serde(rename_all="camelCase")]`（本计划命令参数为原始类型，无 struct）。
- **依赖**：`base64` crate（Cargo.toml 已有，曾用于已删的 snapshot）、`image`（已有）、`arboard`（已有）；不引入新依赖。

## 文件结构

**Rust（src-tauri/src/）**
- `screenshot/capture.rs` —— 重构：拆 `crop_and_copy` 为 `capture_region -> RgbaImage` + `clamp_crop_rect`（纯函数，TDD）+ `region_to_png_base64` + `save_clipboard_from_png`
- `commands/screenshot.rs` —— 新增 `get_region_snapshot` / `save_clipboard_image` 命令；移除 `crop_and_copy`
- `lib.rs` —— invoke_handler 注册更新
- `screenshot/mod.rs` —— 模块注释更新

**前端（web/src/pages/Screenshot/）**
- `useAnnotationCanvas.ts` —— **新建**：canvas 重绘 hook（drawImage 快照 + 标注，矩形/箭头绘制）
- `ScreenshotToolbar.tsx` + `ScreenshotToolbar.module.css` —— **新建**：工具条组件（矩形/箭头/6色板/撤销/确认/取消）
- `Overlay.tsx` —— 重构：状态机（idle/selecting/editing）+ 集成 hook/toolbar + 进编辑/确认/取消流程
- `Overlay.module.css` —— 新增 editing 布局（canvas/工具条定位）

**文档**
- `src-tauri/CLAUDE.md` M6 节、`web/CLAUDE.md` Overlay 条目

---

## Task 1: Rust capture.rs 重构 + clamp 纯函数 TDD

**Files:**
- Modify: `src-tauri/src/screenshot/capture.rs`
- Test: `src-tauri/src/screenshot/capture.rs`（`#[cfg(test)]` 模块）

**Interfaces:**
- Produces: `pub fn capture_region(display_index: usize, x: u32, y: u32, w: u32, h: u32, dpr: f64) -> Result<RgbaImage, AppError>`；`pub fn region_to_png_base64(...) -> Result<String, AppError>`；`pub fn save_clipboard_from_png(data_url: &str) -> Result<(), AppError>`。删除 `pub fn crop_and_copy(...)`。

- [ ] **Step 1: 写 clamp_crop_rect 的失败测试**

在 `capture.rs` 末尾加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::clamp_crop_rect;

    #[test]
    fn clamp_normal_within_bounds() {
        // 100×100 帧，选区 (10,20,30,40)，dpr=2 → 物理 (20,40,60,80)，未越界
        let (x, y, w, h) = clamp_crop_rect(100, 100, 10, 20, 30, 40, 2.0).unwrap();
        assert_eq!((x, y, w, h), (20, 40, 60, 80));
    }

    #[test]
    fn clamp_overflow_to_frame_edge() {
        // 选区右下超出帧：物理 (90,90,40,40) → clamp 到 (90,90,10,10)
        let (x, y, w, h) = clamp_crop_rect(100, 100, 45, 45, 20, 20, 2.0).unwrap();
        assert_eq!((x, y, w, h), (90, 90, 10, 10));
    }

    #[test]
    fn clamp_empty_returns_err() {
        // 零宽选区 → Err
        assert!(clamp_crop_rect(100, 100, 0, 0, 0, 10, 1.0).is_err());
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --manifest-path src-tauri/Cargo.toml capture::tests -- --nocapture`
Expected: 编译失败（`clamp_crop_rect` 未定义）

- [ ] **Step 3: 实现 clamp_crop_rect + 重构 capture.rs**

整个 `capture.rs` 替换为：

```rust
//! screenshot/capture.rs — 抓屏、裁剪、快照、剪贴板写入
//!
//! Business Logic（为什么需要这个模块）:
//!     区域截图的核心能力：抓屏（物理像素帧）→ 裁剪选区 → 编码 PNG / 写系统剪贴板。
//!     编辑工具条流程下，抓屏与剪贴板写入解耦：前端 canvas 合成「桌面+标注」PNG 后，
//!     由 save_clipboard_from_png 解码写剪贴板；capture_region 仅供前端取选区桌面快照。
//!
//! Code Logic（这个模块做什么）:
//!     - `capture_monitor(display_index)`：取 xcap 第 index 显示器抓整屏（物理像素）。
//!     - `clamp_crop_rect(...)`：逻辑坐标 ×dpr → 物理像素 rect，clamp 到帧边界（纯函数，单测覆盖）。
//!     - `capture_region(...)`：抓屏 + clamp_crop_rect + crop_imm，返回选区 RgbaImage。
//!     - `region_to_png_base64(...)`：capture_region → PNG → base64 data URL（前端 canvas 背景）。
//!     - `save_clipboard_from_png(data_url)`：剥 data URL 前缀 → base64 解码 → image 解码 → arboard 写剪贴板。

use std::io::Cursor;

use arboard::{Clipboard, ImageData};
use image::RgbaImage;
use xcap::Monitor;

use crate::error::AppError;

/// 取第 `display_index` 个显示器对象。
///
/// Business Logic: `xcap::Monitor::all()` 顺序单进程内稳定，前端 Overlay 用同一 index 取快照/裁剪，
///     保证两处指向同一台显示器。
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

/// 把前端 canvas 合成的 PNG data URL 解码后写入系统剪贴板。
///
/// Business Logic: 用户点「确认」后，前端把「桌面选区 + 标注」合成的 PNG 传过来写剪贴板，
///     可直接粘贴到 Claude Code。
/// Code Logic: 剥 `data:image/png;base64,` 前缀 → base64 解码 → `image::load_from_memory` →
///     `to_rgba8()` → `arboard::ImageData` → `Clipboard::new()?.set_image(...)`。
pub fn save_clipboard_from_png(data_url: &str) -> Result<(), AppError> {
    let b64 = data_url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| AppError::Bad("无效的 PNG data URL（缺少前缀）".into()))?;
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| AppError::Bad(format!("base64 解码失败: {e}")))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| AppError::Bad(format!("PNG 解码失败: {e}")))?
        .to_rgba8();
    let (w_out, h_out) = (img.width() as usize, img.height() as usize);
    let img_data = ImageData {
        width: w_out,
        height: h_out,
        bytes: img.into_raw().into(),
    };
    let mut cb = Clipboard::new().map_err(|e| AppError::Bad(format!("打开剪贴板失败: {e}")))?;
    cb.set_image(img_data)
        .map_err(|e| AppError::Bad(format!("写入剪贴板失败: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::clamp_crop_rect;

    #[test]
    fn clamp_normal_within_bounds() {
        let (x, y, w, h) = clamp_crop_rect(100, 100, 10, 20, 30, 40, 2.0).unwrap();
        assert_eq!((x, y, w, h), (20, 40, 60, 80));
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
}
```

- [ ] **Step 4: 跑测试 + 编译**

Run: `cargo test --manifest-path src-tauri/Cargo.toml capture::tests`
Expected: 3 tests passed

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过（此时 `crop_and_copy` 已删，commands 层仍引用会报错——Task 2 修复）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/screenshot/capture.rs
git commit -m "refactor(screenshot): capture.rs 拆分 capture_region/clamp/快照/剪贴板，移除 crop_and_copy"
```

---

## Task 2: Rust 命令层 + lib.rs 注册更新

**Files:**
- Modify: `src-tauri/src/commands/screenshot.rs`
- Modify: `src-tauri/src/lib.rs`（invoke_handler）
- Modify: `src-tauri/src/screenshot/mod.rs`（注释）

**Interfaces:**
- Consumes: Task 1 的 `capture::region_to_png_base64` / `capture::save_clipboard_from_png`
- Produces: 命令 `get_region_snapshot` / `save_clipboard_image`（前端 invoke 名）

- [ ] **Step 1: 重写 commands/screenshot.rs**

整个文件替换为：

```rust
//! commands/screenshot.rs — 区域截图命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端通过 invoke 触发区域截图流程：开选区窗口、进编辑模式取选区快照、确认后写剪贴板、取消。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_region_capture(app)`：每屏建透明置顶选区窗口。
//!     - `get_region_snapshot(display, x, y, w, h, dpr)`：抓该屏纯桌面选区，返回 PNG base64。
//!     - `save_clipboard_image(app, dataUrl)`：把前端合成的 PNG data URL 写剪贴板 + 关全部 overlay。
//!     - `cancel_region_capture(app)`：emit `region-capture:result` {cancelled:true}，关全部 overlay。

use serde_json::json;
use tauri::{AppHandle, Emitter};

use crate::error::AppError;
use crate::screenshot::{capture, overlay};

/// 启动区域截图：为每个显示器创建选区窗口。
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

/// 把前端 canvas 合成的「桌面+标注」PNG 写入剪贴板，并关闭所有 overlay。
///
/// Business Logic: 用户点「确认」后，前端把 canvas.toDataURL（桌面选区 + 标注）传过来，
///     Rust 解码写剪贴板（可直接粘贴到 Claude Code），成功后关 overlay。
/// Code Logic: `capture::save_clipboard_from_png` → emit `region-capture:result` {ok:true} → `overlay::close_all_overlays`。
#[tauri::command]
pub async fn save_clipboard_image(app: AppHandle, data_url: String) -> Result<(), AppError> {
    capture::save_clipboard_from_png(&data_url)?;
    let _ = app.emit("region-capture:result", json!({ "ok": true }));
    overlay::close_all_overlays(&app);
    Ok(())
}

/// 取消区域截图。
#[tauri::command]
pub async fn cancel_region_capture(app: AppHandle) -> Result<(), AppError> {
    let _ = app.emit("region-capture:result", json!({ "cancelled": true }));
    overlay::close_all_overlays(&app);
    Ok(())
}
```

- [ ] **Step 2: 更新 lib.rs invoke_handler**

在 `src-tauri/src/lib.rs` 的 `generate_handler!` 中：删除 `screenshot_cmd::crop_and_copy,` 一行，把原位置替换为新增两命令。最终该段为：

```rust
            screenshot_cmd::start_region_capture,
            screenshot_cmd::get_region_snapshot,
            screenshot_cmd::save_clipboard_image,
            screenshot_cmd::cancel_region_capture,
```

- [ ] **Step 3: 更新 mod.rs 注释**

`src-tauri/src/screenshot/mod.rs` 的 Code Logic 块，把 `capture::crop_and_copy` 描述改为：

```rust
//!     - `capture::capture_region`：抓指定显示器 + 按选区裁剪，返回 RgbaImage。
//!     - `capture::region_to_png_base64`：选区快照编码 PNG base64（前端 canvas 背景）。
//!     - `capture::save_clipboard_from_png`：PNG data URL 解码写剪贴板。
```

- [ ] **Step 4: cargo build + clippy**

Run: `cargo build --manifest-path src-tauri/Cargo.toml`
Expected: 编译通过、无 `unused` / `unresolved import` 警告（crop_and_copy 引用已全部清除）

Run: `cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings 2>&1 | tail -5`
Expected: 无 warning（若有 base64/image/arboard unused 则说明 Task 1 漏改）

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/screenshot.rs src-tauri/src/lib.rs src-tauri/src/screenshot/mod.rs
git commit -m "feat(screenshot): get_region_snapshot + save_clipboard_image 命令，移除 crop_and_copy"
```

---

## Task 3: 前端 useAnnotationCanvas hook

**Files:**
- Create: `web/src/pages/Screenshot/useAnnotationCanvas.ts`

**Interfaces:**
- Produces: `interface Annotation { tool: 'rect' | 'arrow'; color: string; x1: number; y1: number; x2: number; y2: number }`；`function useAnnotationCanvas(canvasRef: RefObject<HTMLCanvasElement>, snapshot: HTMLImageElement | null, annotations: Annotation[], logicalW: number, logicalH: number, dpr: number): void`（内部 useEffect 在 snapshot/annotations 变化时重绘）

- [ ] **Step 1: 创建 useAnnotationCanvas.ts**

```ts
/**
 * useAnnotationCanvas - 截图编辑模式的 canvas 重绘 hook
 *
 * Business Logic: 编辑模式 canvas 要同时画「桌面快照底图」+「所有标注」（矩形/箭头），
 *   且标注增删/撤销时实时重绘，所见即所得（canvas 内容 = 最终合成图）。
 *
 * Code Logic: 监听 snapshot / annotations 变化的 useEffect，每次重绘：清空 → drawImage(快照)
 *   → 遍历标注 strokeRect 或画箭头（主线 + 终点三角头）。线宽 = 3×dpr 物理清晰。
 */

import { useEffect, type RefObject } from 'react';

/** 单个标注（选区内逻辑坐标） */
export interface Annotation {
  tool: 'rect' | 'arrow';
  color: string; // #RRGGBB
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

/** 画箭头：主线 (x1,y1)→(x2,y2) + 终点三角头（按角度旋转） */
function drawArrow(ctx: CanvasRenderingContext2D, x1: number, y1: number, x2: number, y2: number, headLen: number) {
  const angle = Math.atan2(y2 - y1, x2 - x1);
  ctx.beginPath();
  ctx.moveTo(x1, y1);
  ctx.lineTo(x2, y2);
  // 三角头两条边
  ctx.lineTo(x2 - headLen * Math.cos(angle - Math.PI / 6), y2 - headLen * Math.sin(angle - Math.PI / 6));
  ctx.moveTo(x2, y2);
  ctx.lineTo(x2 - headLen * Math.cos(angle + Math.PI / 6), y2 - headLen * Math.sin(angle + Math.PI / 6));
  ctx.stroke();
}

/** 全量重绘 canvas：快照底图 + 全部标注 */
function redraw(
  ctx: CanvasRenderingContext2D,
  snapshot: HTMLImageElement,
  annotations: Annotation[],
  logicalW: number,
  logicalH: number,
  dpr: number,
) {
  ctx.clearRect(0, 0, logicalW, logicalH);
  ctx.drawImage(snapshot, 0, 0, logicalW, logicalH);
  ctx.lineWidth = 3 * dpr;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  const headLen = 12 * dpr;
  for (const a of annotations) {
    ctx.strokeStyle = a.color;
    ctx.fillStyle = a.color;
    if (a.tool === 'rect') {
      ctx.strokeRect(
        Math.min(a.x1, a.x2),
        Math.min(a.y1, a.y2),
        Math.abs(a.x2 - a.x1),
        Math.abs(a.y2 - a.y1),
      );
    } else {
      drawArrow(ctx, a.x1, a.y1, a.x2, a.y2, headLen);
    }
  }
}

/**
 * 监听 snapshot/annotations 变化重绘 canvas。
 * canvas 物理缓冲由调用方设置（canvas.width = logicalW*dpr），本 hook 只负责绘制内容。
 */
export function useAnnotationCanvas(
  canvasRef: RefObject<HTMLCanvasElement>,
  snapshot: HTMLImageElement | null,
  annotations: Annotation[],
  logicalW: number,
  logicalH: number,
  dpr: number,
): void {
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !snapshot) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    // canvas 物理缓冲 = 逻辑×dpr；ctx.scale 后用逻辑坐标绘制
    canvas.width = Math.max(1, Math.round(logicalW * dpr));
    canvas.height = Math.max(1, Math.round(logicalH * dpr));
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.scale(dpr, dpr);
    redraw(ctx, snapshot, annotations, logicalW, logicalH, dpr);
  }, [canvasRef, snapshot, annotations, logicalW, logicalH, dpr]);
}
```

- [ ] **Step 2: tsc 验证**

Run: `cd web && npx tsc --noEmit`
Expected: exit 0（无类型错误）

- [ ] **Step 3: Commit**

```bash
git add web/src/pages/Screenshot/useAnnotationCanvas.ts
git commit -m "feat(screenshot): useAnnotationCanvas hook（canvas 快照+标注重绘）"
```

---

## Task 4: 前端 ScreenshotToolbar 组件 + CSS

**Files:**
- Create: `web/src/pages/Screenshot/ScreenshotToolbar.tsx`
- Create: `web/src/pages/Screenshot/ScreenshotToolbar.module.css`

**Interfaces:**
- Consumes: 无（纯展示组件）
- Produces: `interface Tool { kind: 'rect' | 'arrow' }`；`const COLORS: string[]`；组件 props（见 Step 1）

- [ ] **Step 1: 创建 ScreenshotToolbar.tsx**

```tsx
/**
 * ScreenshotToolbar - 截图编辑工具条
 *
 * Business Logic: 用户框选后进入编辑模式，用工具条选标注工具（矩形/箭头）+ 颜色，撤销最后一个标注，
 *   确认合成写剪贴板或取消。布局微信截图风格。
 *
 * Code Logic: 受控组件——当前工具/颜色由父组件管理，本组件只负责展示 + 回调。
 */

import styles from './ScreenshotToolbar.module.css';

export type ToolKind = 'rect' | 'arrow';

/** 预设 6 色板（红/黄/绿/蓝/白/黑），固定线宽由 canvas 绘制层控制 */
export const COLORS = ['#FF3B30', '#FFCC00', '#34C759', '#007AFF', '#FFFFFF', '#000000'];

interface ScreenshotToolbarProps {
  tool: ToolKind;
  onToolChange: (tool: ToolKind) => void;
  color: string;
  onColorChange: (color: string) => void;
  onUndo: () => void;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ScreenshotToolbar({
  tool,
  onToolChange,
  color,
  onColorChange,
  onUndo,
  onConfirm,
  onCancel,
}: ScreenshotToolbarProps) {
  return (
    <div className={styles.toolbar} role="toolbar">
      <button
        type="button"
        className={tool === 'rect' ? styles.toolBtnActive : styles.toolBtn}
        onClick={() => onToolChange('rect')}
        title="矩形"
      >
        ▭
      </button>
      <button
        type="button"
        className={tool === 'arrow' ? styles.toolBtnActive : styles.toolBtn}
        onClick={() => onToolChange('arrow')}
        title="箭头"
      >
        →
      </button>
      <span className={styles.divider} />
      <div className={styles.colors}>
        {COLORS.map((c) => (
          <button
            key={c}
            type="button"
            className={color === c ? styles.colorBtnActive : styles.colorBtn}
            style={{ backgroundColor: c }}
            onClick={() => onColorChange(c)}
            title={c}
          />
        ))}
      </div>
      <span className={styles.divider} />
      <button type="button" className={styles.toolBtn} onClick={onUndo} title="撤销">
        ↶
      </button>
      <button type="button" className={styles.confirmBtn} onClick={onConfirm} title="确认">
        ✓
      </button>
      <button type="button" className={styles.cancelBtn} onClick={onCancel} title="取消">
        ✕
      </button>
    </div>
  );
}
```

- [ ] **Step 2: 创建 ScreenshotToolbar.module.css**

```css
/* 工具条容器：水平排列、圆角、半透明深色底（保证任意桌面背景下可见） */
.toolbar {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 6px 8px;
  background-color: rgba(30, 30, 30, 0.92);
  border-radius: 8px;
  box-shadow: 0 4px 16px rgba(0, 0, 0, 0.3);
  user-select: none;
}

.toolBtn,
.toolBtnActive,
.confirmBtn,
.cancelBtn {
  min-width: 30px;
  height: 30px;
  padding: 0 8px;
  border: none;
  border-radius: 6px;
  background-color: transparent;
  color: #fff;
  font-size: 16px;
  cursor: pointer;
}

.toolBtn:hover,
.toolBtnActive {
  background-color: rgba(255, 255, 255, 0.12);
}

.toolBtnActive {
  background-color: rgba(0, 122, 255, 0.6);
}

.divider {
  width: 1px;
  height: 20px;
  margin: 0 4px;
  background-color: rgba(255, 255, 255, 0.2);
}

.colors {
  display: flex;
  align-items: center;
  gap: 4px;
}

.colorBtn,
.colorBtnActive {
  width: 20px;
  height: 20px;
  padding: 0;
  border: 2px solid rgba(255, 255, 255, 0.5);
  border-radius: 50%;
  cursor: pointer;
}

.colorBtnActive {
  border-color: #fff;
  box-shadow: 0 0 0 2px rgba(0, 122, 255, 0.8);
}

.confirmBtn {
  color: #34c759;
  font-size: 18px;
}

.confirmBtn:hover {
  background-color: rgba(52, 199, 89, 0.2);
}

.cancelBtn {
  color: #ff3b30;
  font-size: 16px;
}

.cancelBtn:hover {
  background-color: rgba(255, 59, 48, 0.2);
}
```

- [ ] **Step 3: tsc 验证**

Run: `cd web && npx tsc --noEmit`
Expected: exit 0

- [ ] **Step 4: Commit**

```bash
git add web/src/pages/Screenshot/ScreenshotToolbar.tsx web/src/pages/Screenshot/ScreenshotToolbar.module.css
git commit -m "feat(screenshot): ScreenshotToolbar 组件 + CSS（矩形/箭头/6色板/撤销/确认/取消）"
```

---

## Task 5: 前端 Overlay.tsx 重构（状态机 + 集成 canvas/toolbar）

**Files:**
- Modify: `web/src/pages/Screenshot/Overlay.tsx`
- Modify: `web/src/pages/Screenshot/Overlay.module.css`

**Interfaces:**
- Consumes: Task 2 命令（`get_region_snapshot` / `save_clipboard_image` / `cancel_region_capture`）、Task 3 `useAnnotationCanvas` + `Annotation`、Task 4 `ScreenshotToolbar` + `ToolKind` + `COLORS`

- [ ] **Step 1: 重写 Overlay.tsx**

```tsx
/**
 * Overlay - 区域截图选区页（独立于主 AppShell）
 *
 * Business Logic: 微信截图风格。三态：
 *   - idle：整屏半透明遮罩
 *   - selecting：拖拽框选（四块遮罩 + 蓝虚线边框）
 *   - editing：选区确定，canvas 画桌面快照 + 标注，工具条选矩形/箭头/颜色/撤销/确认/取消
 *   确认 → canvas.toDataURL 合成 → save_clipboard_image 写剪贴板。ESC/取消 → 关闭。
 *
 * Code Logic: 状态机 + selectionRef（mouseup 读最新选区）+ hiding（进编辑/确认前隐藏遮罩与边框，
 *   让 Rust 抓纯桌面 / canvas 合成不含遮罩）。hooks 在所有 early return 之前（项目规则 20）。
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@/api/client';
import { useAnnotationCanvas, type Annotation } from './useAnnotationCanvas';
import { ScreenshotToolbar, COLORS, type ToolKind } from './ScreenshotToolbar';
import styles from './Overlay.module.css';

type Mode = 'idle' | 'selecting' | 'editing';

interface Selection {
  startX: number;
  startY: number;
  x: number;
  y: number;
  w: number;
  h: number;
}

function parseDisplay(): number {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get('display');
  const n = Number(raw);
  return Number.isFinite(n) && n >= 0 ? Math.floor(n) : 0;
}

export function Overlay() {
  const [mode, setMode] = useState<Mode>('idle');
  const [selection, setSelection] = useState<Selection | null>(null);
  const [hiding, setHiding] = useState(false);
  // editing 状态
  const [snapshot, setSnapshot] = useState<HTMLImageElement | null>(null);
  const [annotations, setAnnotations] = useState<Annotation[]>([]);
  const [draft, setDraft] = useState<Annotation | null>(null); // 正在画的标注预览
  const [tool, setTool] = useState<ToolKind>('rect');
  const [color, setColor] = useState<string>(COLORS[0]);
  const [busy, setBusy] = useState(false); // 抓快照/写剪贴板进行中，禁重复触发

  const displayRef = useRef<number>(parseDisplay());
  const draggingRef = useRef<boolean>(false);
  const selectionRef = useRef<Selection | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dprRef = useRef<number>(window.devicePixelRatio || 1);

  // 强制 html/body 透明（覆盖全局 reset.css 的 var(--bg)，防 transparent 窗口白屏）
  useEffect(() => {
    const html = document.documentElement;
    const body = document.body;
    const prevHtml = html.style.background;
    const prevBody = body.style.background;
    html.style.background = 'transparent';
    body.style.background = 'transparent';
    return () => {
      html.style.background = prevHtml;
      body.style.background = prevBody;
    };
  }, []);

  const cancel = useCallback(async () => {
    try {
      await invoke('cancel_region_capture');
    } catch {
      // ignore
    }
  }, []);

  // ESC 取消
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void cancel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [cancel]);

  // canvas 重绘（editing 时快照 + 已有标注 + 草稿预览）
  useAnnotationCanvas(
    canvasRef,
    snapshot,
    draft ? [...annotations, draft] : annotations,
    selection?.w ?? 0,
    selection?.h ?? 0,
    dprRef.current,
  );

  // === selecting 阶段：框选 ===
  const onMouseDown = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (mode !== 'idle' || e.button !== 0) return;
      draggingRef.current = true;
      const sel: Selection = { startX: e.clientX, startY: e.clientY, x: e.clientX, y: e.clientY, w: 0, h: 0 };
      selectionRef.current = sel;
      setSelection(sel);
      setMode('selecting');
    },
    [mode],
  );

  const onMouseMoveSelect = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!draggingRef.current) return;
      setSelection((prev) => {
        if (!prev) return prev;
        const next: Selection = {
          ...prev,
          x: Math.min(prev.startX, e.clientX),
          y: Math.min(prev.startY, e.clientY),
          w: Math.abs(e.clientX - prev.startX),
          h: Math.abs(e.clientY - prev.startY),
        };
        selectionRef.current = next;
        return next;
      });
    },
    [],
  );

  // mouseup 有效选区 → 进 editing（抓快照）
  const enterEditing = useCallback(async () => {
    const sel = selectionRef.current;
    if (!sel || sel.w < 10 || sel.h < 10) {
      void cancel();
      return;
    }
    setBusy(true);
    setHiding(true); // 隐藏遮罩/边框，Rust 抓纯桌面
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    try {
      const dataUrl = await invoke<string>('get_region_snapshot', {
        display: displayRef.current,
        x: Math.round(sel.x),
        y: Math.round(sel.y),
        w: Math.round(sel.w),
        h: Math.round(sel.h),
        dpr: dprRef.current,
      });
      const img = new Image();
      await new Promise<void>((resolve, reject) => {
        img.onload = () => resolve();
        img.onerror = () => reject(new Error('快照加载失败'));
        img.src = dataUrl;
      });
      setSnapshot(img);
      setHiding(false);
      setMode('editing');
    } catch {
      setHiding(false);
      setMode('idle'); // 快照失败回 idle 让用户重选
    } finally {
      setBusy(false);
    }
  }, [cancel]);

  const onMouseUpSelect = useCallback(() => {
    if (!draggingRef.current) return;
    draggingRef.current = false;
    void enterEditing();
  }, [enterEditing]);

  // === editing 阶段：在 canvas 上画标注 ===
  const onCanvasMouseDown = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!snapshot) return;
      const rect = e.currentTarget.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      draggingRef.current = true;
      setDraft({ tool, color, x1: x, y1: y, x2: x, y2: y });
    },
    [tool, color, snapshot],
  );

  const onCanvasMouseMove = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!draggingRef.current || !draft) return;
      const rect = e.currentTarget.getBoundingClientRect();
      setDraft({ ...draft, x2: e.clientX - rect.left, y2: e.clientY - rect.top });
    },
    [draft],
  );

  const onCanvasMouseUp = useCallback(() => {
    if (!draggingRef.current || !draft) return;
    draggingRef.current = false;
    // 仅保留尺寸非零的标注
    if (Math.abs(draft.x2 - draft.x1) >= 2 || Math.abs(draft.y2 - draft.y1) >= 2) {
      setAnnotations((prev) => [...prev, draft]);
    }
    setDraft(null);
  }, [draft]);

  // 工具条回调
  const undo = useCallback(() => setAnnotations((prev) => prev.slice(0, -1)), []);

  const confirm = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    const canvas = canvasRef.current;
    try {
      const dataUrl = canvas?.toDataURL('image/png');
      if (!dataUrl) throw new Error('canvas 合成失败');
      await invoke('save_clipboard_image', { dataUrl });
      // 成功后 Rust 已关 overlay；失败抛出
    } catch {
      setBusy(false); // 保留 editing 让用户重试
    }
  }, [busy]);

  const showSelection = selection && selection.w > 0 && selection.h > 0;

  // editing 工具条位置：默认选区下方居中，贴近下边则翻到上方
  const tbH = 44;
  const winH = typeof window !== 'undefined' ? window.innerHeight : 9999;
  const tbBelow = selection && selection.y + selection.h + 8 + tbH <= winH;
  const toolbarStyle: React.CSSProperties = selection
    ? {
        left: selection.x,
        top: tbBelow ? selection.y + selection.h + 8 : Math.max(0, selection.y - tbH - 8),
        width: selection.w,
      }
    : {};

  return (
    <div
      className={styles.overlay}
      onMouseDown={mode === 'idle' ? onMouseDown : undefined}
      onMouseMove={mode === 'selecting' ? onMouseMoveSelect : undefined}
      onMouseUp={mode === 'selecting' ? onMouseUpSelect : undefined}
      onContextMenu={(e) => {
        e.preventDefault();
        void cancel();
      }}
    >
      {/* hiding=true 时只透出桌面（进编辑/确认时不显示遮罩/边框/canvas） */}
      {!hiding &&
        (mode === 'editing' && snapshot ? (
          <>
            {/* 选区外四块遮罩 */}
            <div className={styles.mask} style={{ left: 0, top: 0, right: 0, bottom: `calc(100% - ${selection!.y}px)` }} />
            <div className={styles.mask} style={{ left: 0, top: `${selection!.y + selection!.h}px`, right: 0, bottom: 0 }} />
            <div className={styles.mask} style={{ left: 0, top: `${selection!.y}px`, width: `${selection!.x}px`, height: `${selection!.h}px` }} />
            <div className={styles.mask} style={{ left: `${selection!.x + selection!.w}px`, top: `${selection!.y}px`, right: 0, height: `${selection!.h}px` }} />
            {/* canvas：选区内，画快照 + 标注 */}
            <canvas
              ref={canvasRef}
              className={styles.canvas}
              style={{ left: selection!.x, top: selection!.y, width: selection!.w, height: selection!.h }}
              onMouseDown={onCanvasMouseDown}
              onMouseMove={onCanvasMouseMove}
              onMouseUp={onCanvasMouseUp}
            />
            {/* 工具条 */}
            <div className={styles.toolbarWrap} style={toolbarStyle}>
              <ScreenshotToolbar
                tool={tool}
                onToolChange={setTool}
                color={color}
                onColorChange={setColor}
                onUndo={undo}
                onConfirm={confirm}
                onCancel={() => void cancel()}
              />
            </div>
          </>
        ) : showSelection ? (
          <>
            {/* selecting：四块遮罩 + 蓝虚线边框 */}
            <div className={styles.mask} style={{ left: 0, top: 0, right: 0, bottom: `calc(100% - ${selection!.y}px)` }} />
            <div className={styles.mask} style={{ left: 0, top: `${selection!.y + selection!.h}px`, right: 0, bottom: 0 }} />
            <div className={styles.mask} style={{ left: 0, top: `${selection!.y}px`, width: `${selection!.x}px`, height: `${selection!.h}px` }} />
            <div className={styles.mask} style={{ left: `${selection!.x + selection!.w}px`, top: `${selection!.y}px`, right: 0, height: `${selection!.h}px` }} />
            <div className={styles.selection} style={{ left: selection!.x, top: selection!.y, width: selection!.w, height: selection!.h }} />
          </>
        ) : (
          /* idle：整屏遮罩 */
          <div className={styles.mask} style={{ inset: 0 }} />
        ))}
    </div>
  );
}
```

- [ ] **Step 2: 更新 Overlay.module.css（新增 canvas / toolbarWrap）**

在 `Overlay.module.css` 现有 `.overlay` / `.mask` / `.selection` 之后追加：

```css
/* editing：选区内的 canvas（画桌面快照 + 标注） */
.canvas {
  position: absolute;
  cursor: crosshair;
}

/* 工具条外层定位容器（居中于选区宽度） */
.toolbarWrap {
  position: absolute;
  display: flex;
  justify-content: center;
  pointer-events: none;
}

.toolbarWrap > * {
  pointer-events: auto;
}
```

- [ ] **Step 3: tsc 验证**

Run: `cd web && npx tsc --noEmit`
Expected: exit 0

- [ ] **Step 4: 手动验证（dev 实测）**

Run: `./start.sh`（dev），按截图快捷键：
- 框选 → 进入编辑模式（选区内显示桌面快照、选区外暗、工具条出现在选区下方/上方）
- 选矩形/箭头 + 颜色，画多个标注（实时预览），撤销去掉最后一个
- ✓ 确认 → 粘贴到任意输入框，图为「桌面+标注」、无遮罩/边框
- ✕ / ESC → 正常关闭

Expected: 以上全部符合

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Screenshot/Overlay.tsx web/src/pages/Screenshot/Overlay.module.css
git commit -m "feat(screenshot): Overlay 状态机 idle/selecting/editing + canvas 标注 + 工具条集成"
```

---

## Task 6: 文档更新（CLAUDE.md ×2）

**Files:**
- Modify: `src-tauri/CLAUDE.md` M6 节
- Modify: `web/CLAUDE.md` Overlay 条目

- [ ] **Step 1: 更新 src-tauri/CLAUDE.md M6 节**

把「命令层」段（注册 3 个）改为注册 4 个、用新流程替换 crop_and_copy；把「前端选区页」段改为三态状态机 + 编辑模式 + canvas 标注。关键改动：

- 命令层段：`start_region_capture` / `get_region_snapshot(display,x,y,w,h,dpr)→PNG base64` / `save_clipboard_image(app,dataUrl)→写剪贴板+关overlay` / `cancel_region_capture`（移除 crop_and_copy）
- capture.rs 段：`capture_region`（抓+裁，返回 RgbaImage）+ `clamp_crop_rect`（纯函数单测）+ `region_to_png_base64` + `save_clipboard_from_png`
- 前端选区页段：三态 `idle`(整屏遮罩)/`selecting`(框选)/`editing`(canvas 快照+标注 + 工具条)；进编辑先 hiding 抓纯桌面快照；确认 canvas.toDataURL → save_clipboard_image（所见即所得，Rust 不画标注）；工具条 = 矩形/箭头 + 6 色板 + 撤销/确认/取消

- [ ] **Step 2: 更新 web/CLAUDE.md Overlay 条目**

把 Overlay 条目改为：三态状态机（idle/selecting/editing），editing 用 `useAnnotationCanvas` hook（canvas 画快照+标注）+ `ScreenshotToolbar` 组件（矩形/箭头/6 色/撤销/确认/取消）；确认 canvas 合成 → `save_clipboard_image`；新增 `useAnnotationCanvas.ts` / `ScreenshotToolbar.tsx` 文件。

- [ ] **Step 3: Commit**

```bash
git add src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: 截图编辑工具条 M6/Overlay 文档（三态 + canvas 标注 + 工具条）"
```

---

## Task 7: 集成验证（三屏实测）

**Files:** 无（验证任务）

- [ ] **Step 1: Rust 编译 + 单测 + clippy**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && cargo build --manifest-path src-tauri/Cargo.toml`
Expected: 3 capture tests passed；编译通过无警告

- [ ] **Step 2: 前端类型 + lint**

Run: `cd web && npx tsc --noEmit && npm run lint`
Expected: exit 0

- [ ] **Step 3: 三屏 dev 实测**

Run: `./start.sh`，按截图快捷键，逐项确认：
- [ ] 三屏各自进入遮罩（微信风格整屏暗）
- [ ] 框选 → 进编辑（选区桌面快照 + 工具条 + 选区外暗）
- [ ] 矩形/箭头标注可画、颜色可切、多标注叠加、撤销去最后一个
- [ ] ✓ 确认 → 粘贴图 = 桌面+标注，无遮罩/边框
- [ ] ✕ / ESC → 关闭
- [ ] 三屏独立编辑（各屏 overlay 独立 canvas + 工具条）

- [ ] **Step 4: 合并回 master（项目规则 14）**

```bash
# 在 worktree 完成上述 commit 后：
# 回主 repo 合并（worktree 用 using-git-worktrees 创建，此处为其产出）
git -C /Users/hans/python_project/claude-partner merge --ff-only <worktree-branch>
# 清理 worktree（ExitWorktree remove 或 git worktree remove + git branch -d）
```

---

## Self-Review（计划完成后自查结果）

- **Spec 覆盖**：状态机(T5)、布局(T5)、工具条(T4)、标注数据与绘制(T3+T5)、canvas 物理清晰(T3)、Rust 命令(T1+T2)、时序(T5 enterEditing/confirm)、错误处理(T5 catch 回 idle/保留 editing)、capabilities(无改动，T2 注释)、文件清单(全覆盖)、验证(T7)、YAGNI 边界(遵守)。✓
- **占位扫描**：无 TBD/TODO；每步含实际代码或命令。✓
- **类型一致性**：`Annotation`（T3 定义）↔ T5 使用一致；`ToolKind`/`COLORS`（T4 定义）↔ T5 使用一致；`clamp_crop_rect`（T1）签名 ↔ 测试一致；命令名 `get_region_snapshot`/`save_clipboard_image`（T2）↔ T5 invoke 一致。✓
