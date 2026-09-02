//! 全屏健康遮罩窗口规划：已有窗口复用/关闭。
//!
//! 显示器几何去重见 `crate::monitor_geom`（与截图选区共用）。

use crate::monitor_geom::{dedup_overlay_monitor_geoms, extra_prefixed_overlay_labels};

pub use crate::monitor_geom::OverlayMonitorGeom;

/// 健康遮罩窗口 label 前缀。
const HEALTH_OVERLAY_PREFIX: &str = "health-overlay-";

/// 需要创建或导航到新模板 URL 的遮罩窗。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayWindowOp {
    /// 窗口 label，形如 `health-overlay-{display}`。
    pub label: String,
    /// 写入 URL 的 display 序号。
    pub display: usize,
    /// 窗口几何。
    pub geom: OverlayMonitorGeom,
}

/// 一次开遮罩的窗口动作清单。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HealthOverlayWindowPlan {
    /// 尚不存在、需要新建的窗口。
    pub create: Vec<OverlayWindowOp>,
    /// 已存在、需要改 URL 的窗口。
    pub navigate: Vec<OverlayWindowOp>,
    /// 重叠/多余、需要关掉的已有 label。
    pub close: Vec<String>,
}

/// Business Logic（为什么需要这个函数）:
///     遮罩 URL 必须带 display + template，队列切下一项时也要同一格式。
///
/// Code Logic（这个函数做什么）:
///     拼 `/health-overlay?display={i}&template={id}`。
pub fn health_overlay_path(display: usize, template_id: &str) -> String {
    format!("/health-overlay?display={display}&template={template_id}")
}

/// Business Logic（为什么需要这个函数）:
///     已有遮罩窗必须切到新模板 URL，不能继续显示上一轮饮水/休息文案。
///
/// Code Logic（这个函数做什么）:
///     生成同 origin 的 `location.replace` 脚本；path 经 JSON 转义。
pub fn health_overlay_navigate_js(display: usize, template_id: &str) -> String {
    let path = health_overlay_path(display, template_id);
    let path_js = serde_json::Value::String(path);
    format!(
        "(function(){{var u=new URL(window.location.href);var p=new URL({path_js},u.origin);u.pathname=p.pathname;u.search=p.search;u.hash='';window.location.replace(u.toString());}})()"
    )
}

/// Business Logic（为什么需要这个函数）:
///     已有遮罩窗不能跳过（会留下旧模板），也不能按重复显示器再开一层。
///
/// Code Logic（这个函数做什么）:
///     先去重几何 → 每个唯一屏对应 `health-overlay-{i}`；已存在则 navigate，
///     否则 create；下标越界或无法解析的旧 label 列入 close。
pub fn plan_health_overlay_windows(
    monitors: &[OverlayMonitorGeom],
    existing_labels: &[String],
) -> HealthOverlayWindowPlan {
    let unique = dedup_overlay_monitor_geoms(monitors);
    let unique_count = unique.len();
    let mut plan = HealthOverlayWindowPlan::default();
    for (display, geom) in unique.into_iter().enumerate() {
        let label = overlay_label(display);
        let op = OverlayWindowOp {
            label: label.clone(),
            display,
            geom,
        };
        if existing_labels.iter().any(|item| item == &label) {
            plan.navigate.push(op);
        } else {
            plan.create.push(op);
        }
    }
    plan.close =
        extra_prefixed_overlay_labels(HEALTH_OVERLAY_PREFIX, unique_count, existing_labels);
    plan
}

fn overlay_label(display: usize) -> String {
    format!("{HEALTH_OVERLAY_PREFIX}{display}")
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
    fn health_overlay_path_includes_display_and_template() {
        assert_eq!(
            health_overlay_path(0, "water"),
            "/health-overlay?display=0&template=water"
        );
        let js = health_overlay_navigate_js(1, "water");
        assert!(js.contains("/health-overlay"));
        assert!(js.contains("template=water"));
        assert!(js.contains("display=1"));
    }

    #[test]
    fn plan_closes_extra_overlapping_windows_and_reuses_first() {
        let monitors = vec![geom(0, 0, 1920.0, 1080.0), geom(0, 0, 1920.0, 1080.0)];
        let existing = vec![
            "health-overlay-0".to_string(),
            "health-overlay-1".to_string(),
        ];
        let plan = plan_health_overlay_windows(&monitors, &existing);
        assert_eq!(plan.create.len(), 0);
        assert_eq!(plan.navigate.len(), 1);
        assert_eq!(plan.navigate[0].label, "health-overlay-0");
        assert_eq!(plan.close, vec!["health-overlay-1".to_string()]);
    }

    #[test]
    fn plan_creates_when_no_window_exists() {
        let monitors = vec![geom(0, 0, 1920.0, 1080.0)];
        let plan = plan_health_overlay_windows(&monitors, &[]);
        assert_eq!(plan.create.len(), 1);
        assert!(plan.navigate.is_empty());
        assert!(plan.close.is_empty());
        assert_eq!(plan.create[0].label, "health-overlay-0");
    }
}
