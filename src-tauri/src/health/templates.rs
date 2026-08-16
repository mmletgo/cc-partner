//! 每条健康提醒模板的独立运行时判定与遮罩 FIFO 队列。
//!
//! 共享工作窗口由 `HealthStateMachine` 维护；本模块只判断「这条模板该不该弹」，
//! 以及全屏遮罩互斥排队（同 id 去重）。

use crate::config::{HealthReminderTemplate, ReminderTrigger};
use crate::health::state::MachineState;
use std::collections::{HashMap, VecDeque};

/// 单条模板的内存态。
///
/// Business Logic（为什么需要这个结构）:
///     多条提醒共用时钟，但 pending / 本窗口已触发 / 间隔原点 / 贪睡必须彼此独立。
/// Code Logic（这个结构做什么）:
///     纯数据载体，由 daemon 与命令层在同一把 `templates` 锁内读写。
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateRuntime {
    /// 已弹出尚未响应，防止每 tick 重复入队。
    pub pending: bool,
    /// 间隔模板的计时原点（完成/跳过/贪睡/启动）。
    pub last_completed_ts: i64,
    /// sedentary 本共享窗口是否已触发过。
    pub reminded_this_window: bool,
    /// 该模板贪睡到期时间戳；None 表示未贪睡。
    pub snooze_until: Option<i64>,
}

impl TemplateRuntime {
    /// 用当前时间初始化：视为刚完成，避免开机即弹。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动或新加模板时不能立刻遮罩打断用户。
    /// Code Logic（这个函数做什么）:
    ///     last_completed_ts=now，其余清零。
    pub fn new(now_ts: i64) -> Self {
        Self {
            pending: false,
            last_completed_ts: now_ts,
            reminded_this_window: false,
            snooze_until: None,
        }
    }
}

/// 全屏遮罩互斥会话 + FIFO 队列。
///
/// Business Logic（为什么需要这个结构）:
///     同时到期的多条提醒只能占一块全屏；系统通知可同时出，遮罩必须排队。
/// Code Logic（这个结构做什么）:
///     current 是正在展示的 template_id；queue 为后续 FIFO，入队时同 id 去重。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayQueue {
    /// 当前遮罩对应的模板；None 表示无窗。
    pub current: Option<String>,
    /// 等待展示的模板 id（FIFO）。
    pub queue: VecDeque<String>,
}

/// 把一条命中的模板送进遮罩：无当前则成为当前，否则入队（同 id 去重）。
///
/// Business Logic（为什么需要这个函数）:
///     第二条提醒不能因窗口已存在而被静默丢掉。
/// Code Logic（这个函数做什么）:
///     current 为空 → 设 current；已是 current 或已在 queue → 忽略；否则 push_back。
///     返回 true 表示应立即打开/切换到该模板。
pub fn enqueue_overlay(queue: &mut OverlayQueue, template_id: &str) -> bool {
    if queue.current.as_deref() == Some(template_id) {
        return false;
    }
    if queue.current.is_none() {
        queue.current = Some(template_id.to_string());
        return true;
    }
    if queue.queue.iter().any(|id| id == template_id) {
        return false;
    }
    queue.queue.push_back(template_id.to_string());
    false
}

/// 结束当前遮罩，弹出队列下一项（若有）。
///
/// Business Logic（为什么需要这个函数）:
///     完成/跳过/ESC 后要展示下一条排队提醒，而不是直接关窗丢队。
/// Code Logic（这个函数做什么）:
///     清 current，再 pop_front 作为新 current；返回下一 id。
pub fn advance_overlay_queue(queue: &mut OverlayQueue) -> Option<String> {
    queue.current = None;
    let next = queue.queue.pop_front();
    queue.current = next.clone();
    next
}

/// 有效休息关窗时清全部 sedentary 的本窗口已触发标记。
///
/// Business Logic（为什么需要这个函数）:
///     共享时钟关窗后，各久坐模板应能在新窗口再次触发；间隔模板原点不能被动。
/// Code Logic（这个函数做什么）:
///     对配置里 trigger=sedentary 的 id，把 runtime.reminded_this_window 置 false。
pub fn clear_sedentary_window_flags(
    runtimes: &mut HashMap<String, TemplateRuntime>,
    templates: &[HealthReminderTemplate],
) {
    for tmpl in templates {
        if tmpl.trigger == ReminderTrigger::Sedentary {
            if let Some(rt) = runtimes.get_mut(&tmpl.id) {
                rt.reminded_this_window = false;
            }
        }
    }
}

/// 按当前配置补齐/裁剪模板运行时（不重置仍存在模板的计时）。
///
/// Business Logic（为什么需要这个函数）:
///     用户增删自定义模板后，下一拍必须认新 id，但不能把已有饮水/休息计时清零。
/// Code Logic（这个函数做什么）:
///     缺 id 用 now 初始化；多余自定义 id 删除；内置即使被关也保留 runtime。
pub fn reconcile_template_runtimes(
    runtimes: &mut HashMap<String, TemplateRuntime>,
    templates: &[HealthReminderTemplate],
    now_ts: i64,
) {
    let keep: std::collections::HashSet<&str> = templates.iter().map(|t| t.id.as_str()).collect();
    runtimes.retain(|id, _| keep.contains(id.as_str()));
    for tmpl in templates {
        runtimes
            .entry(tmpl.id.clone())
            .or_insert_with(|| TemplateRuntime::new(now_ts));
    }
}

/// 判定一条已启用模板此刻是否应触发。
///
/// Business Logic（为什么需要这个函数）:
///     间隔与久坐规则不同，且 pending/贪睡/本窗口已触发都必须挡住重复弹窗。
/// Code Logic（这个函数做什么）:
///     pending 或 snooze 未到期 → false；interval 看 last_completed；sedentary 仅 Working
///     且本窗口未提醒且窗口时长达阈值。
pub fn should_fire_template(
    template: &HealthReminderTemplate,
    runtime: &TemplateRuntime,
    machine: &MachineState,
    now_ts: i64,
) -> bool {
    if !template.enabled || runtime.pending {
        return false;
    }
    if runtime.snooze_until.is_some_and(|t| t > now_ts) {
        return false;
    }
    match template.trigger {
        ReminderTrigger::Interval => {
            let interval = template.interval_seconds.unwrap_or(i64::MAX);
            now_ts.saturating_sub(runtime.last_completed_ts) >= interval
        }
        ReminderTrigger::Sedentary => match machine {
            MachineState::Working(w) => {
                let threshold = template.threshold_seconds.unwrap_or(i64::MAX);
                !runtime.reminded_this_window
                    && now_ts.saturating_sub(w.window_start_ts) >= threshold
            }
            _ => false,
        },
    }
}

/// 启用中的 sedentary 模板最小阈值，供进度条；无人启用则回落 rest 模板阈值。
///
/// Business Logic（为什么需要这个函数）:
///     Health 页进度条仍要一条「最近的久坐阈值」，不能再死读 work_window_seconds。
/// Code Logic（这个函数做什么）:
///     取 enabled+sedentary 的最小 threshold；否则 rest 的 threshold；再否则 fallback。
pub fn progress_threshold_seconds(templates: &[HealthReminderTemplate], fallback: i64) -> i64 {
    let enabled_min = templates
        .iter()
        .filter(|t| t.enabled && t.trigger == ReminderTrigger::Sedentary)
        .filter_map(|t| t.threshold_seconds)
        .min();
    if let Some(v) = enabled_min {
        return v;
    }
    templates
        .iter()
        .find(|t| t.id == crate::config::HEALTH_REMINDER_REST_ID)
        .and_then(|t| t.threshold_seconds)
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ReminderComplete, HEALTH_REMINDER_REST_ID};
    use crate::health::state::WorkingState;

    fn interval_tmpl(id: &str, secs: i64) -> HealthReminderTemplate {
        HealthReminderTemplate {
            id: id.into(),
            builtin: false,
            enabled: true,
            name: id.into(),
            trigger: ReminderTrigger::Interval,
            interval_seconds: Some(secs),
            threshold_seconds: None,
            complete: ReminderComplete::Instant,
            session_seconds: None,
            title: "t".into(),
            body: "b".into(),
            confirm_label: "ok".into(),
            unit_label: "次".into(),
            credit_minutes: None,
            daily_cap: None,
        }
    }

    fn sit_tmpl(id: &str, threshold: i64) -> HealthReminderTemplate {
        HealthReminderTemplate {
            id: id.into(),
            builtin: id == HEALTH_REMINDER_REST_ID,
            enabled: true,
            name: id.into(),
            trigger: ReminderTrigger::Sedentary,
            interval_seconds: None,
            threshold_seconds: Some(threshold),
            complete: ReminderComplete::Session,
            session_seconds: Some(30),
            title: "t".into(),
            body: "b".into(),
            confirm_label: "开始".into(),
            unit_label: "次".into(),
            credit_minutes: None,
            daily_cap: None,
        }
    }

    fn working(start: i64) -> MachineState {
        MachineState::Working(WorkingState {
            window_start_ts: start,
            last_active_ts: start,
            reminded: false,
        })
    }

    #[test]
    fn interval_fires_after_origin_and_not_when_pending() {
        let tmpl = interval_tmpl("water", 3600);
        let mut rt = TemplateRuntime::new(0);
        assert!(!should_fire_template(&tmpl, &rt, &MachineState::Idle, 3599));
        assert!(should_fire_template(&tmpl, &rt, &MachineState::Idle, 3600));
        rt.pending = true;
        assert!(!should_fire_template(
            &tmpl,
            &rt,
            &MachineState::Idle,
            99999
        ));
    }

    #[test]
    fn two_sedentary_share_clock_independent_thresholds() {
        let short = sit_tmpl("rest", 120);
        let long = sit_tmpl("stretch", 240);
        let mut rest_rt = TemplateRuntime::new(0);
        let stretch_rt = TemplateRuntime::new(0);
        let machine = working(0);
        assert!(should_fire_template(&short, &rest_rt, &machine, 120));
        assert!(!should_fire_template(&long, &stretch_rt, &machine, 120));
        rest_rt.pending = true;
        rest_rt.reminded_this_window = true;
        // 完成短阈值不得清长阈值，也不重置共享窗口
        rest_rt.pending = false;
        assert!(!should_fire_template(&short, &rest_rt, &machine, 200));
        assert!(!should_fire_template(&long, &stretch_rt, &machine, 200));
        assert!(should_fire_template(&long, &stretch_rt, &machine, 240));
    }

    #[test]
    fn completing_interval_does_not_reset_other_interval() {
        let a = interval_tmpl("a", 100);
        let b = interval_tmpl("b", 100);
        let mut ra = TemplateRuntime::new(0);
        let rb = TemplateRuntime::new(0);
        ra.last_completed_ts = 1000;
        assert!(!should_fire_template(&a, &ra, &MachineState::Idle, 1099));
        assert!(should_fire_template(&b, &rb, &MachineState::Idle, 100));
    }

    #[test]
    fn closing_window_clears_only_sedentary_flags() {
        let templates = vec![sit_tmpl("rest", 120), interval_tmpl("water", 3600)];
        let mut map = HashMap::new();
        map.insert(
            "rest".into(),
            TemplateRuntime {
                pending: false,
                last_completed_ts: 10,
                reminded_this_window: true,
                snooze_until: None,
            },
        );
        map.insert(
            "water".into(),
            TemplateRuntime {
                pending: false,
                last_completed_ts: 10,
                reminded_this_window: true,
                snooze_until: None,
            },
        );
        clear_sedentary_window_flags(&mut map, &templates);
        assert!(!map["rest"].reminded_this_window);
        assert!(map["water"].reminded_this_window);
        assert_eq!(map["water"].last_completed_ts, 10);
    }

    #[test]
    fn overlay_queue_is_fifo_and_dedups() {
        let mut q = OverlayQueue::default();
        assert!(enqueue_overlay(&mut q, "water"));
        assert_eq!(q.current.as_deref(), Some("water"));
        assert!(!enqueue_overlay(&mut q, "water"));
        assert!(!enqueue_overlay(&mut q, "kegel"));
        assert!(!enqueue_overlay(&mut q, "kegel"));
        assert_eq!(q.queue.iter().cloned().collect::<Vec<_>>(), vec!["kegel"]);
        assert_eq!(advance_overlay_queue(&mut q).as_deref(), Some("kegel"));
        assert!(q.queue.is_empty());
        assert_eq!(advance_overlay_queue(&mut q), None);
        assert!(q.current.is_none());
    }

    #[test]
    fn reconcile_keeps_existing_and_drops_removed() {
        let mut map = HashMap::new();
        map.insert("water".into(), TemplateRuntime::new(1));
        map.insert("gone".into(), TemplateRuntime::new(1));
        let templates = vec![interval_tmpl("water", 3600), interval_tmpl("kegel", 7200)];
        reconcile_template_runtimes(&mut map, &templates, 99);
        assert_eq!(map["water"].last_completed_ts, 1);
        assert_eq!(map["kegel"].last_completed_ts, 99);
        assert!(!map.contains_key("gone"));
    }

    #[test]
    fn snooze_blocks_until_expiry() {
        let tmpl = interval_tmpl("water", 10);
        let mut rt = TemplateRuntime::new(0);
        rt.last_completed_ts = 0;
        rt.snooze_until = Some(50);
        assert!(!should_fire_template(&tmpl, &rt, &MachineState::Idle, 40));
        assert!(should_fire_template(&tmpl, &rt, &MachineState::Idle, 50));
    }

    #[test]
    fn progress_uses_min_enabled_sedentary() {
        let mut templates = vec![sit_tmpl("rest", 2700), sit_tmpl("custom", 900)];
        assert_eq!(progress_threshold_seconds(&templates, 2700), 900);
        templates[1].enabled = false;
        assert_eq!(progress_threshold_seconds(&templates, 2700), 2700);
    }
}
