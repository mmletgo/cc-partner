//! 健康提醒模块:键鼠监测 + 工作/休息状态机 + 提醒触发。
//!
//! 子模块:
//! - `state`:工作/休息状态机(纯算法)
//! - `monitor`:键鼠采样(跨平台)
//! - `reminder`:提醒生命周期 + 免打扰
//! - `validation`:配置范围/DND/贪睡检查算术
//! - daemon 入口 `start_health_daemon`(本文件)

pub mod monitor;
pub mod reminder;
pub mod state;
pub mod templates;
pub mod validation;
pub mod water;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::config::{
    HealthReminderTemplate, ReminderTrigger, HEALTH_REMINDER_REST_ID, HEALTH_REMINDER_WATER_ID,
};
use crate::error::AppError;
use crate::state::AppState;

use self::monitor::{ActivitySample, ActivitySampler, DeviceQuerySampler};
use self::reminder::is_in_dnd;
use self::state::{HealthStateMachine, HealthThresholds};
use self::templates::{
    advance_overlay_queue, clear_sedentary_window_flags, enqueue_overlay,
    reconcile_template_runtimes, should_fire_template, OverlayQueue, TemplateRuntime,
};

/// 一次「开始休息」遮罩倒计时会话:多屏共享同一份权威态。
///
/// Business Logic: 多屏时每块屏各有一个 `health-overlay-*` 窗口,用户在其中一屏点
///     「开始休息」必须让所有屏同步进入倒计时。后端持有唯一 `end_ts`(休息结束 Unix 秒),
///     经 `health:rest-started` 事件广播给全部遮罩窗口,各窗口基于同一 `end_ts` 显示倒计时。
///     `cancel` 用于在 ESC/关闭遮罩/再次开始休息时中止到点收尾 task(避免重复 record)。
/// Code Logic: `end_ts` 为权威结束时间戳;`cancel` 是该次会话专属的取消令牌,task 在
///     `select!` 中同时等待 `cancel.cancelled()` 与到点 sleep,任一先触发即决定是否收尾。
#[derive(Clone)]
pub struct OverlayRestSession {
    /// 当前倒计时所属模板。
    pub template_id: String,
    /// 休息结束时间戳(Unix 秒);所有遮罩窗口据此计算剩余秒数。
    pub end_ts: i64,
    /// 到点收尾 task 的取消令牌;cancel 后 task 不再 record/skip/close。
    pub cancel: CancellationToken,
}

/// 健康监测运行时共享状态(跨 daemon task 与命令层)。
pub struct HealthRuntime {
    /// 共享活跃时钟(每分钟由 daemon 推进一拍;不再持唯一 should_remind)。
    pub machine: Mutex<HealthStateMachine>,
    /// 是否整体暂停监测(paused 状态由命令层置位,daemon 采样时据此跳过提醒)。
    pub paused: AtomicBool,
    /// 每条模板独立 pending / 间隔原点 / 本窗口已触发 / 贪睡。
    pub templates: Mutex<HashMap<String, TemplateRuntime>>,
    /// 全屏遮罩 FIFO（同 id 去重）。
    pub overlay_queue: Mutex<OverlayQueue>,
    /// 当前 session 倒计时权威态(None=未在倒计时);多屏共享同一 end_ts。
    pub overlay_rest: Mutex<Option<OverlayRestSession>>,
}
impl HealthRuntime {
    /// Business Logic: daemon 与命令层需要共享时钟、模板态和遮罩队列。
    /// Code Logic: Idle 时钟 + 空模板 map + 空队列 + 未暂停。
    pub fn new() -> Self {
        Self {
            machine: Mutex::new(HealthStateMachine::new()),
            paused: AtomicBool::new(false),
            templates: Mutex::new(HashMap::new()),
            overlay_queue: Mutex::new(OverlayQueue::default()),
            overlay_rest: Mutex::new(None),
        }
    }
}

impl Default for HealthRuntime {
    /// Business Logic: 缺省即空闲初态。
    /// Code Logic: 委托 `new()`。
    fn default() -> Self {
        Self::new()
    }
}

/// 启动健康监测后台 daemon。返回 `CancellationToken`,供应用退出时取消。
///
/// 一个 `std::thread` 采样(线程局部持有非 Send 的 `DeviceState`)
/// + 一个 tokio task 处理(写库 + 推进状态机 + emit 提醒)。
///
/// 架构:复用 `cc/collector.rs` 的 `select!{cancel, rx.recv()}` 范式——
/// 采样放原生线程(持有非 Send 的设备句柄),跨线程只传 `ActivitySample`(Send 纯数据)。
pub fn start_health_daemon(app: AppHandle, state: std::sync::Arc<AppState>) -> CancellationToken {
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel::<ActivitySample>(8);

    // 采样线程(线程局部持有 sampler,无需 Send)
    let cancel_s = cancel.clone();
    std::thread::spawn(move || {
        let mut sampler = DeviceQuerySampler::new();
        loop {
            if cancel_s.is_cancelled() {
                break;
            }
            let sample = sampler.sample();
            // 处理 task 被取消/退出后 rx 端关闭,blocking_send 返回 Err → 退出采样线程。
            if tx.blocking_send(sample).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_secs(60));
        }
    });

    // 处理 task:消费 ActivitySample,写库 → 推进状态机 → 满足条件 emit。
    let app_h = app.clone();
    let state_h = state.clone();
    let cancel_h = cancel.clone();
    // 用 `tauri::async_runtime::spawn`（非 `tokio::spawn`）：本函数在 lib.rs setup 闭包的
    // 同步段（block_on 之外）被调用，主线程无 Tokio reactor，`tokio::spawn` 会 panic
    // "there is no reactor running"；走 Tauri 全局 runtime handle 不依赖当前线程上下文
    // （与 cc/collector.rs / commands/updater.rs 的 spawn 范式一致）。
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel_h.cancelled() => break,
                Some(sample) = rx.recv() => {
                    if let Err(e) = handle_sample(&app_h, &state_h, sample).await {
                        tracing::warn!("健康采样处理失败: {e}");
                    }
                }
            }
        }
    });
    cancel
}

/// 处理一次采样:写库 → 校验配置 → 推进状态机 → 满足条件 emit `health:reminder`。
///
/// Business Logic（为什么需要这个函数）:
///     daemon 每分钟需要落库活动、在启用时推进久坐/喝水提醒并清理过期明细；
///     磁盘上若出现非法范围/DND，本 tick 必须跳过提醒与清理，避免错误计时或算术溢出。
/// Code Logic（这个函数做什么）:
///     跨 await 不持 RwLockReadGuard:开头 clone health 配置；写 activity 后若未启用/暂停则返回；
///     再用 `validate_health_config_with_field` 校验——失败 `warn(field=...)` 并跳过提醒/清理；
///     通过后用归一化 cfg 推进状态机/水提醒，并用 `checked_retain_cutoff` 做清理。
async fn handle_sample(
    app: &AppHandle,
    state: &AppState,
    sample: ActivitySample,
) -> Result<(), AppError> {
    let cfg = state.config.read().unwrap().health.clone();
    let now = Utc::now().timestamp();
    // 对齐到分钟桶(同分钟重采覆盖),取该分钟起始时间戳。
    let minute_ts = now - now.rem_euclid(60);
    let active_for_reminder = cfg.enabled && !state.health.paused.load(Ordering::Relaxed);

    // 写活动记录(record_window_title=false 时不记标题,降级到只记进程名/活跃态)
    let rec = crate::storage::health_repo::ActivityRecord {
        ts: minute_ts,
        is_active: sample.is_active,
        process_name: sample.process_name.clone(),
        window_title: if cfg.record_window_title {
            sample.window_title.clone()
        } else {
            None
        },
    };
    state.health_repo.insert_activity(&rec).await?;

    // 未启用 / 已暂停:仅写库不触发提醒。
    if !active_for_reminder {
        return Ok(());
    }

    // 非法配置:跳过本 tick 的提醒/清理(活动记录已写)。
    let cfg = match validation::validate_health_config_with_field(&cfg) {
        Ok(c) => c,
        Err(field_code) => {
            tracing::warn!(
                field = %field_code,
                "health.invalid_config skip reminder/cleanup"
            );
            return Ok(());
        }
    };

    // 共享时钟只负责相位/有效休息关窗；每条模板独立判定是否弹窗。
    let thresholds = HealthThresholds {
        work_window_seconds: cfg.work_window_seconds,
        break_seconds: cfg.break_seconds,
    };
    let closed_window = {
        let mut m = state.health.machine.lock().unwrap();
        m.advance(sample.is_active, now, &thresholds)
            .reminder_closed_window
            .is_some()
    };

    let fired = collect_fired_templates(&state.health, &cfg.reminders, closed_window, now);
    let dnd = is_in_dnd(now, cfg.dnd_start.as_deref(), cfg.dnd_end.as_deref());
    for fired_id in fired {
        let Some(tmpl) = cfg.reminders.iter().find(|t| t.id == fired_id) else {
            continue;
        };
        if let Err(e) = persist_habit_event(state, tmpl, now, "triggered", 0).await {
            tracing::warn!("写入习惯触发记录失败: {e}");
        }
        if dnd {
            continue;
        }
        if cfg.notify_enabled {
            let _ = app.emit(
                "health:reminder",
                serde_json::json!({
                    "templateId": tmpl.id,
                    "title": tmpl.title,
                    "body": tmpl.body,
                }),
            );
        }
        let should_open = {
            let mut q = state.health.overlay_queue.lock().unwrap();
            enqueue_overlay(&mut q, &tmpl.id)
        };
        if should_open {
            if let Err(e) = open_health_overlay(app, &tmpl.id) {
                tracing::warn!("打开全屏健康遮罩失败: {e}");
            }
        }
    }

    // 数据清理:检查算术 cutoff,溢出则跳过本 tick 清理(不 panic)。
    let cutoff = match validation::checked_retain_cutoff(now, cfg.retain_days) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "health.retain_cutoff overflow skip cleanup");
            return Ok(());
        }
    };
    if let Err(e) = state.health_repo.cleanup_older_than(cutoff).await {
        tracing::warn!("活动记录清理失败: {e}");
    }
    if let Err(e) = state.health_repo.cleanup_water_older_than(cutoff).await {
        tracing::warn!("清理过期饮水记录失败: {e}");
    }
    if let Err(e) = state.health_repo.cleanup_rest_older_than(cutoff).await {
        tracing::warn!("清理过期休息记录失败: {e}");
    }
    if let Err(e) = state.health_repo.cleanup_habit_older_than(cutoff).await {
        tracing::warn!("清理过期习惯记录失败: {e}");
    }
    Ok(())
}

/// 同步模板 runtime 并收集本拍应触发的模板 id。
///
/// Business Logic（为什么需要这个函数）:
///     多条模板共用时钟，必须独立 pending/阈值，关窗只清久坐已触发标记。
/// Code Logic（这个函数做什么）:
///     reconcile → 可选清 sedentary flags → 对 enabled 模板 should_fire，命中则置 pending
///     与 reminded_this_window。
fn collect_fired_templates(
    runtime: &HealthRuntime,
    templates: &[HealthReminderTemplate],
    closed_window: bool,
    now: i64,
) -> Vec<String> {
    let machine_state = runtime.machine.lock().unwrap().state.clone();
    let mut map = runtime.templates.lock().unwrap();
    reconcile_template_runtimes(&mut map, templates, now);
    if closed_window {
        clear_sedentary_window_flags(&mut map, templates);
    }
    let mut fired = Vec::new();
    for tmpl in templates {
        if !tmpl.enabled {
            continue;
        }
        let Some(rt) = map.get_mut(&tmpl.id) else {
            continue;
        };
        if should_fire_template(tmpl, rt, &machine_state, now) {
            rt.pending = true;
            if tmpl.trigger == ReminderTrigger::Sedentary {
                rt.reminded_this_window = true;
            }
            fired.push(tmpl.id.clone());
        }
    }
    fired
}

/// 双写 habit_records 以及 water/rest 旧表。
///
/// Business Logic（为什么需要这个函数）:
///     新统计读 habit_records；回滚旧二进制仍能从 water/rest 表恢复饮水与休息。
/// Code Logic（这个函数做什么）:
///     先 insert_habit_record；water completed 再 insert_water；rest triggered/completed
///     再 insert_rest_record。
async fn persist_habit_event(
    state: &crate::state::AppState,
    template: &HealthReminderTemplate,
    now: i64,
    kind: &str,
    duration_seconds: i64,
) -> Result<i64, AppError> {
    persist_habit_event_by_id(state, &template.id, now, kind, duration_seconds).await
}

/// 仅凭 template_id 双写（旧命令包装无完整模板对象时用）。
///
/// Business Logic（为什么需要这个函数）:
///     record_water / record_rest 等旧入口仍要写入新表。
/// Code Logic（这个函数做什么）:
///     与 persist_habit_event 相同的双写规则。
pub(crate) async fn persist_habit_event_by_id(
    state: &crate::state::AppState,
    template_id: &str,
    now: i64,
    kind: &str,
    duration_seconds: i64,
) -> Result<i64, AppError> {
    let repo = &state.health_repo;
    let id = repo
        .insert_habit_record(template_id, now, kind, duration_seconds)
        .await?;
    if template_id == HEALTH_REMINDER_WATER_ID && kind == "completed" {
        let _ = repo.insert_water(now).await?;
    }
    if template_id == HEALTH_REMINDER_REST_ID {
        let rest_kind = if kind == "triggered" {
            "reminder"
        } else {
            "rest"
        };
        if kind == "triggered" || kind == "completed" {
            let _ = repo.insert_rest_record(now, rest_kind, duration_seconds).await?;
        }
    }
    if kind == "completed" {
        credit_health_completed(state, template_id, id, now).await;
    }
    Ok(id)
}

/// 健康 completed 入账；失败只记日志，不回滚习惯记录。
///
/// Business Logic: 打卡成功就该充电；账本故障不能让用户以为没完成健康行为。
/// Code Logic: BatteryRepo 现取；credit 后 emit `battery:changed`。
async fn credit_health_completed(
    state: &crate::state::AppState,
    template_id: &str,
    habit_id: i64,
    now: i64,
) {
    let config = state.config.read().unwrap().battery.clone();
    let repo = crate::storage::BatteryRepo::with_gate(
        state.db.clone(),
        state.maintenance_gate.clone(),
    );
    let source = crate::config::BatteryCreditSource::from_health_template_id(template_id);
    let source_id = crate::battery::habit_source_id(template_id, habit_id);
    match crate::battery::credit(&repo, &config, source, &source_id, now).await {
        Ok(snapshot) => state.emit_event("battery:changed", snapshot),
        Err(error) => tracing::warn!("充电入账失败: {error}"),
    }
}

/// 打开全屏健康提醒遮罩窗口(每屏一个,复用截图透明窗口构建模式)。
///
/// Business Logic: 健康监测启用后,久坐/喝水提醒触发时需在每块屏幕覆盖
///     一个透明置顶遮罩窗口强制打断,展示推迟/跳过(久坐另有「开始休息」倒计时;喝水另有
///     「已饮水」/延迟/跳过)按钮。macOS 单窗口不能跨屏(与截图同理),故枚举每块显示器建独立窗口。
/// Code Logic: 枚举 `xcap::Monitor::all()`,每个显示器用 `WebviewWindowBuilder` 建
///     decorations(false)/transparent(true)/always_on_top(true)/focused(true)/
///     skip_taskbar(true)/resizable(false) 的窗口,label = `health-overlay-{i}`,
///     url = `/health-overlay?display={i}&template={template_id}`。
///     窗口几何直接用 xcap 的 x()/y()/width()/height()(逻辑点)。已存在同名窗口则跳过。
pub fn open_health_overlay(app: &AppHandle, template_id: &str) -> Result<(), AppError> {
    let monitors =
        xcap::Monitor::all().map_err(|e| AppError::Bad(format!("枚举显示器失败: {e}")))?;

    for (i, monitor) in monitors.into_iter().enumerate() {
        let label = format!("health-overlay-{i}");
        // 已存在同名窗口(上次未清理)则跳过,避免重复创建报错。
        if app.get_webview_window(&label).is_some() {
            continue;
        }
        // macOS: xcap 的 x()/y()/width()/height() 均为逻辑点,直接喂窗口几何,不除 scale。
        let mx = monitor.x().unwrap_or(0);
        let my = monitor.y().unwrap_or(0);
        let mw = monitor.width().unwrap_or(1920) as f64;
        let mh = monitor.height().unwrap_or(1080) as f64;

        tracing::info!(
            display = i,
            x = mx,
            y = my,
            w = mw,
            h = mh,
            "健康提醒遮罩窗口几何(逻辑点)"
        );

        WebviewWindowBuilder::new(
            app,
            &label,
            WebviewUrl::App(
                format!("/health-overlay?display={i}&template={template_id}").into(),
            ),
        )
        .title("健康提醒")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .focused(true)
        .skip_taskbar(true)
        .resizable(false)
        .inner_size(mw, mh)
        .position(mx as f64, my as f64)
        .build()
        .map_err(|e| AppError::Bad(format!("创建健康遮罩窗口失败: {e}")))?;
    }
    Ok(())
}

/// 关闭所有全屏健康提醒遮罩窗口(纯关窗,不触碰休息态)。
///
/// Business Logic: 用户在遮罩上点击推迟/跳过/已饮水,或休息倒计时到点后,需关闭全部遮罩
///     窗口恢复桌面。是否取消进行中的休息 task 由调用方(`close_health_overlay` 命令 /
///     到点收尾 task)按语义决定,本函数只负责关窗。
/// Code Logic: 遍历 `app.webview_windows()`,label 以 `health-overlay-` 前缀开头则 close()。
pub fn close_all_health_overlay_windows(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with("health-overlay-") {
            let _ = win.close();
        }
    }
}

/// 开始一次遮罩休息会话(纯逻辑,不依赖 AppHandle,便于单测):取消旧会话、写入新会话,
/// 返回 `(end_ts, cancel)`。`end_ts = now + break_seconds`;`cancel` 为本次会话专属令牌。
///
/// Business Logic: 用户点「开始休息」时后端成为多屏共享的权威源。若上一次休息会话仍在进行
///     (极端竞态),先 cancel 其到点 task 再开新会话,保证同一时刻只有一份休息态。
fn begin_overlay_rest(
    runtime: &HealthRuntime,
    template_id: &str,
    now: i64,
    session_seconds: i64,
) -> (i64, CancellationToken) {
    let end_ts = now + session_seconds;
    let mut guard = runtime.overlay_rest.lock().unwrap();
    if let Some(old) = guard.take() {
        old.cancel.cancel();
    }
    let cancel = CancellationToken::new();
    *guard = Some(OverlayRestSession {
        template_id: template_id.to_string(),
        end_ts,
        cancel: cancel.clone(),
    });
    (end_ts, cancel)
}

/// 取消进行中的遮罩休息会话(ESC/关闭遮罩/跳过/推迟用):take session 并 cancel 其到点 task。
///
/// Business Logic: 用户中途退出休息(ESC)或用其他按钮结束本次提醒交互时,不应记录一次完整
///     休息(与原「只有自然到 0 才 record」语义一致)。cancel 让到点 task 从 select 的
///     cancelled 分支返回、不执行 record/skip/close。
/// Code Logic: 锁 session,take 出则 cancel 并返回 true;原本就 None 返回 false。
pub(crate) fn cancel_overlay_rest(runtime: &HealthRuntime) -> bool {
    let mut guard = runtime.overlay_rest.lock().unwrap();
    match guard.take() {
        Some(s) => {
            s.cancel.cancel();
            true
        }
        None => false,
    }
}

/// 到点收尾 task 专用:取出并清空 session(不 cancel,task 自身即将结束),返回其 end_ts。
///
/// Business Logic: 到点 task 在 record/skip 之后清掉权威态,使 `get_health_status` 不再
///     报告进行中的休息,并让后续 `cancel_overlay_rest` 成为 no-op(避免重复处理已收尾的会话)。
fn take_overlay_rest_for_finalize(runtime: &HealthRuntime) -> Option<(String, i64)> {
    runtime
        .overlay_rest
        .lock()
        .unwrap()
        .take()
        .map(|s| (s.template_id, s.end_ts))
}

/// 开始一次「开始休息」遮罩倒计时:写入权威 end_ts → 广播 `health:rest-started` 给所有遮罩
/// 窗口 → spawn 到点收尾 task。返回 end_ts 供命令层回传发起窗口(事件也会到达)。
///
/// Business Logic: 多屏遮罩需同步进入同一次休息倒计时。后端作为权威源持 end_ts,广播事件
///     让每块屏基于同一 end_ts 显示;到点后后端统一 record + skip + 关闭全部窗口,即使某屏
///     窗口中途崩溃也不影响收尾与统计。
/// Code Logic: `begin_overlay_rest` 设会话 → emit `health:rest-started`（含 templateId）→
///     spawn 到点 task：只 complete 该模板，不重置共享时钟；关窗后弹出队列下一项。
pub fn start_overlay_rest(app: &AppHandle, state: &AppState, now: i64, break_seconds: i64) -> i64 {
    start_overlay_session(app, state, HEALTH_REMINDER_REST_ID, now, break_seconds)
}

/// 启动任意模板的 session 倒计时。
///
/// Business Logic（为什么需要这个函数）:
///     休息 5 分钟与提肛 30 秒共用同一套权威 end_ts / 到点收尾，只是模板不同。
/// Code Logic（这个函数做什么）:
///     写入 overlay_rest → emit health:rest-started → 到点 persist completed、清 pending、
///     advance 队列；有下一项则改 URL，否则关窗。
pub fn start_overlay_session(
    app: &AppHandle,
    state: &AppState,
    template_id: &str,
    now: i64,
    session_seconds: i64,
) -> i64 {
    let (end_ts, cancel) = begin_overlay_rest(&state.health, template_id, now, session_seconds);
    let _ = app.emit(
        "health:rest-started",
        serde_json::json!({ "templateId": template_id, "endTs": end_ts }),
    );

    let health = state.health.clone();
    let owned_id = template_id.to_string();
    let app_h = app.clone();
    let battery_state = state.clone();
    tauri::async_runtime::spawn(async move {
        let now0 = Utc::now().timestamp();
        let remaining_secs = u64::try_from((end_ts - now0).max(0)).unwrap_or(0);
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(remaining_secs)) => {}
        }
        if cancel.is_cancelled() {
            return;
        }
        let fin_now = Utc::now().timestamp();
        if let Err(e) =
            persist_habit_event_by_id(&battery_state, &owned_id, fin_now, "completed", session_seconds)
                .await
        {
            tracing::warn!("模板会话结束记录失败: {e}");
        }
        acknowledge_template_runtime(&health, &owned_id, fin_now, false);
        take_overlay_rest_for_finalize(&health);
        let next = {
            let mut q = health.overlay_queue.lock().unwrap();
            advance_overlay_queue(&mut q)
        };
        if let Some(next_id) = next {
            if let Err(e) = open_health_overlay(&app_h, &next_id) {
                tracing::warn!("打开排队遮罩失败: {e}");
            }
        } else {
            close_all_health_overlay_windows(&app_h);
        }
    });
    end_ts
}

/// 完成/跳过/贪睡后只改该模板 runtime，不重置共享时钟。
///
/// Business Logic（为什么需要这个函数）:
///     完成一条久坐不得清其它久坐的本窗口计时。
/// Code Logic（这个函数做什么）:
///     清 pending；推进 last_completed；可选写入 snooze_until。
pub(crate) fn acknowledge_template_runtime(
    runtime: &HealthRuntime,
    template_id: &str,
    now: i64,
    keep_pending: bool,
) {
    let mut map = runtime.templates.lock().unwrap();
    let entry = map
        .entry(template_id.to_string())
        .or_insert_with(|| TemplateRuntime::new(now));
    if !keep_pending {
        entry.pending = false;
    }
    entry.last_completed_ts = now;
    entry.snooze_until = None;
}

/// 只给该模板写入贪睡，不改共享时钟。
///
/// Business Logic（为什么需要这个函数）:
///     「稍后提醒」只推迟当前这条。
/// Code Logic（这个函数做什么）:
///     pending=false，snooze_until=until，last_completed 保持或按调用方已写好的值。
pub(crate) fn snooze_template_runtime(
    runtime: &HealthRuntime,
    template_id: &str,
    now: i64,
    until: i64,
) {
    let mut map = runtime.templates.lock().unwrap();
    let entry = map
        .entry(template_id.to_string())
        .or_insert_with(|| TemplateRuntime::new(now));
    entry.pending = false;
    entry.snooze_until = Some(until);
}

/// 关闭当前遮罩并弹出队列下一项；无下一项才关全部窗。
///
/// Business Logic（为什么需要这个函数）:
///     跳过/完成即时模板后不能把还在排队的提醒一起关掉。
/// Code Logic（这个函数做什么）:
///     cancel 当前 session → advance queue → 有 next 则 open，否则 close all。
pub fn dismiss_current_overlay(app: &AppHandle, runtime: &HealthRuntime) {
    cancel_overlay_rest(runtime);
    let next = {
        let mut q = runtime.overlay_queue.lock().unwrap();
        advance_overlay_queue(&mut q)
    };
    if let Some(next_id) = next {
        if let Err(e) = open_health_overlay(app, &next_id) {
            tracing::warn!("打开排队遮罩失败: {e}");
        }
    } else {
        close_all_health_overlay_windows(app);
    }
}

#[cfg(test)]
mod overlay_rest_tests {
    use super::*;

    #[test]
    fn begin_sets_end_ts_and_session() {
        let rt = HealthRuntime::new();
        let (end_ts, _cancel) = begin_overlay_rest(&rt, "rest", 1000, 300);
        assert_eq!(end_ts, 1300);
        let session = rt.overlay_rest.lock().unwrap();
        assert_eq!(session.as_ref().unwrap().end_ts, 1300);
        assert_eq!(session.as_ref().unwrap().template_id, "rest");
    }

    #[test]
    fn begin_cancels_previous_session() {
        let rt = HealthRuntime::new();
        let (_, old_cancel) = begin_overlay_rest(&rt, "rest", 1000, 300);
        assert!(!old_cancel.is_cancelled());
        // 再次开始休息应取消上一次会话的到点 task,保证同时刻只有一份休息态。
        let _ = begin_overlay_rest(&rt, "kegel", 2000, 30);
        assert!(
            old_cancel.is_cancelled(),
            "二次开始休息必须取消上一次的到点 task"
        );
    }

    #[test]
    fn cancel_clears_session_and_signals_token() {
        let rt = HealthRuntime::new();
        let (_, cancel) = begin_overlay_rest(&rt, "rest", 1000, 300);
        assert!(!cancel.is_cancelled());
        assert!(cancel_overlay_rest(&rt), "曾有会话应返回 true");
        assert!(cancel.is_cancelled(), "cancel 应触发到点 task 的令牌");
        assert!(
            rt.overlay_rest.lock().unwrap().is_none(),
            "cancel 后会话应清空"
        );
        assert!(!cancel_overlay_rest(&rt), "无会话时再次 cancel 返回 false");
    }

    #[test]
    fn take_for_finalize_clears_session() {
        let rt = HealthRuntime::new();
        begin_overlay_rest(&rt, "rest", 1000, 300);
        assert_eq!(
            take_overlay_rest_for_finalize(&rt),
            Some(("rest".into(), 1300))
        );
        assert!(
            rt.overlay_rest.lock().unwrap().is_none(),
            "收尾取出后会话应为 None"
        );
        assert_eq!(
            take_overlay_rest_for_finalize(&rt),
            None,
            "无会话时取出返回 None"
        );
    }
}
