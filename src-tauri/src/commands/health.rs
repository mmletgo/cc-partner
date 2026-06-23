//! 健康提醒命令层:状态查询 / 开关 / 推迟 / 跳过 / 配置 / 统计。
//!
//! Business Logic: 对应前端「健康提醒」设置页与状态展示的 `invoke('xxx')` 调用。
//!     前端轮询 `get_health_status` 展示当前工作/休息相位与开关，操作按钮触发
//!     `toggle_health_enabled`/`toggle_health_paused`/`snooze_reminder`/`skip_reminder`，
//!     配置项变更走 `update_health_config`，统计页用 `get_activity_stats` 拉活跃/闲置分钟数。
//!
//! Code Logic: 通过 `State<'_, AppState>` 注入共享状态；DTO 一律 `#[serde(rename_all="camelCase")]`
//!     对齐前端 types。配置类命令直接读写 `state.config`（RwLock）并 `cfg.save()` 落盘；
//!     运行时类命令（暂停/贪睡/跳过）操作 `HealthRuntime` 的原子标记与状态机。

use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::State;

use crate::config::HealthConfig;
use crate::error::AppError;
use crate::health::state::MachineState;
use crate::state::AppState;

/// 健康提醒配置 DTO（camelCase，对齐前端）。
///
/// Business Logic: 前端设置页用一份扁平结构展示/编辑全部健康配置。
/// Code Logic: 字段与 `HealthConfig` 一一对应，`From<HealthConfig>` 完成转换。
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct HealthConfigDto {
    /// 久坐监测总开关。
    pub enabled: bool,
    /// 单个工作窗口长度（秒）。
    pub work_window_seconds: i64,
    /// 有效休息判定时长（秒）。
    pub break_seconds: i64,
    /// 是否记录前台窗口标题（最细粒度统计）。
    pub record_window_title: bool,
    /// 明细保留天数。
    pub retain_days: i64,
    /// 系统通知提醒开关。
    pub notify_enabled: bool,
    /// 免打扰开始 "HH:MM"（含），None 表示无免打扰。
    pub dnd_start: Option<String>,
    /// 免打扰结束 "HH:MM"（不含），支持跨午夜。
    pub dnd_end: Option<String>,
    /// 喝水提醒开关。
    pub water_enabled: bool,
    /// 喝水提醒间隔（秒）。
    pub water_interval_seconds: i64,
    /// 全屏遮罩提醒开关(Plan 2);开启后触发久坐提醒时每屏弹透明置顶遮罩窗口。
    pub reminder_fullscreen: bool,
}
impl From<HealthConfig> for HealthConfigDto {
    /// 把磁盘配置 `HealthConfig` 转成前端可用的 camelCase DTO。
    fn from(h: HealthConfig) -> Self {
        Self {
            enabled: h.enabled,
            work_window_seconds: h.work_window_seconds,
            break_seconds: h.break_seconds,
            record_window_title: h.record_window_title,
            retain_days: h.retain_days,
            notify_enabled: h.notify_enabled,
            dnd_start: h.dnd_start,
            dnd_end: h.dnd_end,
            water_enabled: h.water_enabled,
            water_interval_seconds: h.water_interval_seconds,
            reminder_fullscreen: h.reminder_fullscreen,
        }
    }
}

/// 健康提醒运行时状态 DTO（camelCase，对齐前端）。
///
/// Business Logic: 前端首页/托盘需展示「当前是工作中/休息中、是否暂停、何时贪睡到期」。
/// Code Logic: 从 `HealthRuntime` 读取状态机相位 + 原子暂停标记 + 贪睡到期时间戳。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthStatusDto {
    /// 监测总开关（来自配置）。
    pub enabled: bool,
    /// 是否手动暂停监测。
    pub paused: bool,
    /// 当前相位："idle" / "working" / "resting"。
    pub phase: String,
    /// 当前工作窗口起始时间戳（仅 working 相位有值）。
    pub window_start_ts: Option<i64>,
    /// 工作窗口长度（秒）。
    pub work_window_seconds: i64,
    /// 有效休息判定时长（秒）。
    pub break_seconds: i64,
    /// 贪睡到期时间戳（秒）；None 或 <= now 表示未贪睡。
    pub snooze_until: Option<i64>,
}

/// 活跃/闲置统计 DTO（camelCase，对齐前端）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityStatsDto {
    /// 统计窗口内的活跃分钟数。
    pub active_minutes: i64,
    /// 统计窗口内的闲置分钟数。
    pub idle_minutes: i64,
}

/// 单个 app 的活跃分钟数排行项（camelCase，对齐前端 AppUsageItem）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUsageItem {
    /// 进程名（process_name）。
    pub name: String,
    /// 统计窗口内该 app 的活跃分钟数。
    pub minutes: i64,
}

/// 活动明细统计 DTO（camelCase，对齐前端 ActivityDetail）:app 排行 + 24 小时分布。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityDetailDto {
    /// 按活跃分钟倒序的 app 使用时长排行。
    pub app_usage: Vec<AppUsageItem>,
    /// 长度恒为 24 的数组,下标为 UTC 小时(0-23),值为该小时活跃分钟数。
    pub hourly: Vec<i64>,
}

/// 读取完整健康提醒配置（全部字段，供前端配置表单初始化）。
///
/// Business Logic: 前端设置页的完整配置表单需要一个命令一次性拉到当前所有配置项
///                 (工作窗口/休息/通知/全屏/记录标题/喝水/免打扰/保留天数)。
///                 `get_health_status` 只含运行时相位 + 阈值,不含全量配置字段,
///                 故补此命令避免前端拼凑配置。
/// Code Logic: 读 `state.config` 的 health 拷贝,`From<HealthConfig>` 转 DTO 返回。
#[tauri::command]
pub async fn get_health_config(state: State<'_, AppState>) -> Result<HealthConfigDto, AppError> {
    Ok(state.config.read().unwrap().health.clone().into())
}

/// 读取健康提醒当前状态（配置开关 + 运行时相位/暂停/贪睡）。
///
/// Business Logic: 前端轮询展示「工作中/休息中、是否暂停、贪睡何时到期」。
/// Code Logic: 读 config.health 拷贝 + 读 HealthRuntime 的状态机/原子暂停/贪睡标记组装 DTO。
#[tauri::command]
pub async fn get_health_status(state: State<'_, AppState>) -> Result<HealthStatusDto, AppError> {
    let cfg = state.config.read().unwrap().health.clone();
    let (phase, window_start_ts) = {
        let m = state.health.machine.lock().unwrap();
        match &m.state {
            MachineState::Idle => ("idle".to_string(), None),
            MachineState::Working(w) => ("working".to_string(), Some(w.window_start_ts)),
            MachineState::Resting { .. } => ("resting".to_string(), None),
        }
    };
    Ok(HealthStatusDto {
        enabled: cfg.enabled,
        paused: state.health.paused.load(Ordering::Relaxed),
        phase,
        window_start_ts,
        work_window_seconds: cfg.work_window_seconds,
        break_seconds: cfg.break_seconds,
        snooze_until: *state.health.snooze_until.lock().unwrap(),
    })
}

/// 切换监测总开关（写 config.health.enabled 并落盘）。
///
/// Business Logic: 前端「启用/停用久坐监测」开关；关闭后 daemon 仅写库不触发提醒。
/// Code Logic: 拿 config 写锁改字段后 `cfg.save()`，返回更新后的配置 DTO。
#[tauri::command]
pub async fn toggle_health_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<HealthConfigDto, AppError> {
    {
        let mut cfg = state.config.write().unwrap();
        cfg.health.enabled = enabled;
        cfg.save()?;
    }
    Ok(state.config.read().unwrap().health.clone().into())
}

/// 切换暂停标记（运行时原子标记，不落盘）。
///
/// Business Logic: 前端「暂时暂停」按钮；置位后 daemon 采样时跳过提醒（仍写库）。
/// Code Logic: 直接 `store` 原子布尔，无需持久化（重启即失效）。
#[tauri::command]
pub async fn toggle_health_paused(
    state: State<'_, AppState>,
    paused: bool,
) -> Result<(), AppError> {
    state.health.paused.store(paused, Ordering::Relaxed);
    Ok(())
}

/// 贪睡 N 分钟（设置贪睡到期时间戳，期间提醒静默）。
///
/// Business Logic: 前端「稍后提醒」；到期前 daemon 不 emit 提醒事件。
/// Code Logic: 以当前 UTC 时间戳 + minutes*60 设贪睡到期，写入 HealthRuntime.snooze_until。
#[tauri::command]
pub async fn snooze_reminder(state: State<'_, AppState>, minutes: i64) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    *state.health.snooze_until.lock().unwrap() = Some(now + minutes * 60);
    Ok(())
}

/// 跳过当前提醒（重置状态机回到 Idle 初态，清空贪睡）。
///
/// Business Logic: 前端「跳过本次」；结束当前工作窗口并清除贪睡，等待下一次活动重新开窗。
/// Code Logic: 用 `HealthStateMachine::new()` 覆盖状态机，snooze_until 置 None。
#[tauri::command]
pub async fn skip_reminder(state: State<'_, AppState>) -> Result<(), AppError> {
    *state.health.machine.lock().unwrap() = crate::health::state::HealthStateMachine::new();
    *state.health.snooze_until.lock().unwrap() = None;
    Ok(())
}

/// 更新健康提醒配置（整体覆盖写 config.health 并落盘）。
///
/// Business Logic: 前端设置页「保存」；把 DTO 写回磁盘配置并持久化。
/// Code Logic: 拿 config 写锁逐字段覆盖 + `cfg.save()`，返回更新后的配置 DTO。
#[tauri::command]
pub async fn update_health_config(
    state: State<'_, AppState>,
    config: HealthConfigDto,
) -> Result<HealthConfigDto, AppError> {
    {
        let mut cfg = state.config.write().unwrap();
        cfg.health.enabled = config.enabled;
        cfg.health.work_window_seconds = config.work_window_seconds;
        cfg.health.break_seconds = config.break_seconds;
        cfg.health.record_window_title = config.record_window_title;
        cfg.health.retain_days = config.retain_days;
        cfg.health.notify_enabled = config.notify_enabled;
        cfg.health.dnd_start = config.dnd_start.clone();
        cfg.health.dnd_end = config.dnd_end.clone();
        cfg.health.water_enabled = config.water_enabled;
        cfg.health.water_interval_seconds = config.water_interval_seconds;
        cfg.health.reminder_fullscreen = config.reminder_fullscreen;
        cfg.save()?;
    }
    Ok(state.config.read().unwrap().health.clone().into())
}

/// 查询 [since_ts, +∞) 区间内的活跃/闲置分钟数。
///
/// Business Logic: 前端统计页展示「最近 N 分钟活跃多久、闲置多久」。
/// Code Logic: 委托 `HealthRepo::aggregate_minutes`（SQL 层 SUM(CASE WHEN ...)）。
#[tauri::command]
pub async fn get_activity_stats(
    state: State<'_, AppState>,
    since_ts: i64,
) -> Result<ActivityStatsDto, AppError> {
    let (active, idle) = state.health_repo.aggregate_minutes(since_ts).await?;
    Ok(ActivityStatsDto {
        active_minutes: active,
        idle_minutes: idle,
    })
}

/// 查询 [since_ts, +∞) 区间内的活动明细统计(app 使用时长排行 + 24 小时活跃分布)。
///
/// Business Logic: 前端统计页用 recharts 柱状图展示「app 使用时长排行(top8)」和
///                 「一天 24 小时活跃分布」,帮助用户了解屏幕使用习惯。
/// Code Logic: 委托 `HealthRepo::get_app_usage`(按 process_name 聚合倒序) +
///             `HealthRepo::get_hourly_activity`(长度 24 的活跃分钟数组)组装 DTO。
#[tauri::command]
pub async fn get_activity_detail(
    state: State<'_, AppState>,
    since_ts: i64,
) -> Result<ActivityDetailDto, AppError> {
    let app_usage = state
        .health_repo
        .get_app_usage(since_ts)
        .await?
        .into_iter()
        .map(|(n, m)| AppUsageItem {
            name: n,
            minutes: m,
        })
        .collect();
    let hourly = state.health_repo.get_hourly_activity(since_ts).await?;
    Ok(ActivityDetailDto { app_usage, hourly })
}

/// 记录一次喝水(更新喝水计时状态 + 清未响应提醒 + 落库 water_records)。
///
/// Business Logic: 前端「我喝了水」按钮(或收到 `health:water` 提醒后响应);重置下次喝水
///                  计时起点,并清除 pending_remind,使 daemon 在下一间隔后才能再次提醒。
/// Code Logic: 拿当前 UTC 时间戳,更新 `HealthRuntime.water` 的 last_drink_ts 并置
///             pending_remind=false,再 `insert_water(now)` 落库(INSERT OR REPLACE 幂等)。
#[tauri::command]
pub async fn record_water(state: State<'_, AppState>) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    {
        let mut w = state.health.water.lock().unwrap();
        w.last_drink_ts = now;
        w.pending_remind = false;
    }
    state.health_repo.insert_water(now).await?;
    Ok(())
}

/// 关闭所有全屏健康提醒遮罩窗口(供前端遮罩页「推迟/跳过」按钮调用)。
///
/// Business Logic: 用户在全屏遮罩上点击推迟/跳过后需关闭遮罩,恢复正常桌面使用。
/// Code Logic: 委托 `crate::health::close_health_overlay`,遍历 webview_windows 关闭
///             label 前缀 `health-overlay-` 的全部窗口。
#[tauri::command]
pub async fn close_health_overlay(app: tauri::AppHandle) -> Result<(), AppError> {
    crate::health::close_health_overlay(&app);
    Ok(())
}
