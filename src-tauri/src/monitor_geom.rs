//! 遮罩/截图共用的显示器几何去重。
//!
//! Ubuntu/X11 上 `xcap::Monitor::all()` 会把同一 RANDR monitor 下的多个 output
//! 都列出来，几何完全重叠。健康遮罩与截图选区都必须先去重，且 `display={i}`
//! 与抓屏 index 必须指向同一份去重后的列表。

use crate::error::AppError;

/// 一块遮罩窗要用的逻辑像素几何。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayMonitorGeom {
    /// 左上角 X（逻辑点）。
    pub x: i32,
    /// 左上角 Y（逻辑点）。
    pub y: i32,
    /// 宽度（逻辑点）。
    pub width: f64,
    /// 高度（逻辑点）。
    pub height: f64,
}

/// Business Logic（为什么需要这个函数）:
///     截图 overlay 的 `display={i}` 必须和 `capture_monitor(i)` 指向同一块去重后的屏。
///
/// Code Logic（这个函数做什么）:
///     丢掉宽高非正的项；与已保留矩形重叠面积 ≥ 较小者 50% 则跳过，返回保留项的原下标。
pub fn unique_monitor_indices(monitors: &[OverlayMonitorGeom]) -> Vec<usize> {
    let mut kept_geoms = Vec::new();
    let mut indices = Vec::new();
    for (index, monitor) in monitors.iter().enumerate() {
        if monitor.width <= 0.0 || monitor.height <= 0.0 {
            continue;
        }
        if kept_geoms
            .iter()
            .any(|existing| geoms_are_duplicates(existing, monitor))
        {
            continue;
        }
        kept_geoms.push(monitor.clone());
        indices.push(index);
    }
    indices
}

/// Business Logic（为什么需要这个函数）:
///     健康遮罩规划只需要去重后的几何，不需要 xcap Monitor 句柄。
///
/// Code Logic（这个函数做什么）:
///     按 `unique_monitor_indices` 抽出保留的几何。
pub fn dedup_overlay_monitor_geoms(monitors: &[OverlayMonitorGeom]) -> Vec<OverlayMonitorGeom> {
    unique_monitor_indices(monitors)
        .into_iter()
        .map(|index| monitors[index].clone())
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     去重后少了一块屏，上次留下的 `*-overlay-1` 必须关掉，否则仍会叠在主屏上。
///
/// Code Logic（这个函数做什么）:
///     前缀匹配；能解析出的 index ≥ unique_count，或无法解析的同前缀 label，列入关闭。
pub fn extra_prefixed_overlay_labels(
    prefix: &str,
    unique_count: usize,
    existing_labels: &[String],
) -> Vec<String> {
    existing_labels
        .iter()
        .filter(|label| {
            match label
                .strip_prefix(prefix)
                .and_then(|rest| rest.parse::<usize>().ok())
            {
                Some(index) => index >= unique_count,
                None => label.starts_with(prefix),
            }
        })
        .cloned()
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     健康遮罩与截图选区都要把 xcap 逻辑几何喂给 Tauri 窗口。
///
/// Code Logic（这个函数做什么）:
///     读 x/y/width/height，缺省回落 0,0 / 1920×1080。
pub fn geom_from_xcap(monitor: &xcap::Monitor) -> OverlayMonitorGeom {
    OverlayMonitorGeom {
        x: monitor.x().unwrap_or(0),
        y: monitor.y().unwrap_or(0),
        width: monitor.width().unwrap_or(1920) as f64,
        height: monitor.height().unwrap_or(1080) as f64,
    }
}

/// Business Logic（为什么需要这个函数）:
///     开窗与抓屏必须共用同一份去重后的显示器列表，避免 Ubuntu 上 display=1 抓到重复屏。
///
/// Code Logic（这个函数做什么）:
///     `Monitor::all()` 后按几何去重，保留每个唯一矩形的第一项。
pub fn list_unique_xcap_monitors() -> Result<Vec<xcap::Monitor>, AppError> {
    let monitors =
        xcap::Monitor::all().map_err(|e| AppError::Bad(format!("枚举显示器失败: {e}")))?;
    let geoms: Vec<OverlayMonitorGeom> = monitors.iter().map(geom_from_xcap).collect();
    Ok(unique_monitor_indices(&geoms)
        .into_iter()
        .map(|index| monitors[index].clone())
        .collect())
}

fn geoms_are_duplicates(left: &OverlayMonitorGeom, right: &OverlayMonitorGeom) -> bool {
    let overlap = overlap_area(left, right);
    let area_left = left.width * left.height;
    let area_right = right.width * right.height;
    let smaller = area_left.min(area_right);
    if smaller <= 0.0 {
        return false;
    }
    overlap / smaller >= 0.5
}

fn overlap_area(left: &OverlayMonitorGeom, right: &OverlayMonitorGeom) -> f64 {
    let left_r = left.x as f64 + left.width;
    let right_r = right.x as f64 + right.width;
    let left_b = left.y as f64 + left.height;
    let right_b = right.y as f64 + right.height;
    let w = left_r.min(right_r) - (left.x as f64).max(right.x as f64);
    let h = left_b.min(right_b) - (left.y as f64).max(right.y as f64);
    if w <= 0.0 || h <= 0.0 {
        0.0
    } else {
        w * h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(x: i32, y: i32, width: f64, height: f64) -> OverlayMonitorGeom {
        OverlayMonitorGeom {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn unique_indices_drop_duplicate_outputs_but_keep_later_real_screen() {
        let monitors = vec![
            geom(0, 0, 1920.0, 1080.0),
            geom(0, 0, 1920.0, 1080.0),
            geom(1920, 0, 1920.0, 1080.0),
        ];
        assert_eq!(unique_monitor_indices(&monitors), vec![0, 2]);
    }

    #[test]
    fn dedup_drops_identical_linux_outputs_on_same_screen() {
        let monitors = vec![geom(0, 0, 1920.0, 1080.0), geom(0, 0, 1920.0, 1080.0)];
        assert_eq!(
            dedup_overlay_monitor_geoms(&monitors),
            vec![geom(0, 0, 1920.0, 1080.0)]
        );
    }

    #[test]
    fn dedup_drops_nested_or_heavily_overlapping_rects() {
        let monitors = vec![geom(0, 0, 1920.0, 1080.0), geom(0, 0, 3840.0, 2160.0)];
        assert_eq!(
            dedup_overlay_monitor_geoms(&monitors),
            vec![geom(0, 0, 1920.0, 1080.0)]
        );
    }

    #[test]
    fn dedup_keeps_side_by_side_monitors() {
        let monitors = vec![geom(0, 0, 1920.0, 1080.0), geom(1920, 0, 1920.0, 1080.0)];
        assert_eq!(dedup_overlay_monitor_geoms(&monitors).len(), 2);
    }

    #[test]
    fn extra_screenshot_labels_close_stale_duplicate_windows() {
        let existing = vec![
            "screenshot-overlay-0".to_string(),
            "screenshot-overlay-1".to_string(),
        ];
        assert_eq!(
            extra_prefixed_overlay_labels("screenshot-overlay-", 1, &existing),
            vec!["screenshot-overlay-1".to_string()]
        );
    }
}
