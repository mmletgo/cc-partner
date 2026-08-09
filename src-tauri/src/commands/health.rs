//! 健康提醒命令层:状态查询 / 开关 / 推迟 / 跳过 / 配置 / 统计。
//!
//! Business Logic: 对应前端「健康提醒」设置页与状态展示的 `invoke('xxx')` 调用。
//!     前端轮询 `get_health_status` 展示当前工作/休息相位与开关，操作按钮触发
//!     `toggle_health_enabled`/`toggle_health_paused`/`snooze_reminder`/`skip_reminder`，
//!     配置项变更走 `update_health_config`，统计页用 `get_activity_stats` 拉活跃/闲置分钟数。
//!     喝水提醒与全屏遮罩随健康监测固定启用，旧配置中的关闭值会在命令边界归一。
//!
//! Code Logic: 通过 `State<'_, AppState>` 注入共享状态；DTO 一律 `#[serde(rename_all="camelCase")]`
//!     对齐前端 types。配置类命令经 `ConfigRuntime` 事务路径更新；
//!     运行时类命令（暂停/贪睡/跳过）操作 `HealthRuntime` 的原子标记与状态机。

use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::State;

use crate::backend::control_client::BackendControlClient;
use crate::config::HealthConfig;
#[cfg(test)]
use crate::config_runtime::{update_config_transactionally, ConfigRuntime};
use crate::config_runtime::{HealthRuntimePatch, RuntimeConfigPatch};
use crate::error::AppError;
use crate::health::state::MachineState;
use crate::health::validation::{
    checked_future_timestamp, checked_water_snooze_origin, validate_health_config,
};
use crate::health::HealthRuntime;
use crate::state::AppState;

/// 计算本地当日 0 点对应的 Unix 秒时间戳。
///
/// Business Logic: 「今日饮水/休息」统计必须按用户体感中的本地日切分（"今天"指本地时区的今天），
///     若用 UTC 0 点，非 UTC 时区用户看到的"今日"边界会错位（如东八区 UTC 0 点 = 本地 08:00，
///     00:00-08:00 的饮水会被算到昨天）。
/// Code Logic: 用 `chrono::Local` 取当前本地时间，构造当日 00:00:00，经本地时区转回 DateTime 后取 timestamp。
fn local_start_of_day_ts() -> i64 {
    use chrono::{Local, TimeZone};
    let now_local = Local::now();
    let today = now_local.date_naive();
    now_local
        .timezone()
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap()
        .timestamp()
}

/// 健康提醒配置 DTO（camelCase，对齐前端）。
///
/// Business Logic: 前端设置页用一份扁平结构展示/编辑全部健康配置。
/// Code Logic: 字段与 `HealthConfig` 基本对应，`From<HealthConfig>` 完成转换并把固定启用字段归一为 true。
#[derive(Debug, Serialize, Deserialize, Clone)]
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
    /// 喝水提醒历史开关；前端不再展示，命令返回固定 true。
    pub water_enabled: bool,
    /// 喝水提醒间隔（秒）。
    pub water_interval_seconds: i64,
    /// 全屏遮罩提醒历史开关；前端不再展示，命令返回固定 true。
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
            water_enabled: true,
            water_interval_seconds: h.water_interval_seconds,
            reminder_fullscreen: true,
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
    /// 当前「开始休息」遮罩倒计时的结束时间戳（秒）；None 表示未在遮罩休息。
    /// 多屏遮罩共享同一权威值，各窗口据此显示同步倒计时。
    pub overlay_rest_end_ts: Option<i64>,
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

/// 习惯统计返回:饮水 + 休息聚合,前端 HabitStatsCard 一次拉取所需数据。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HabitStatsDto {
    /// 今日饮水次数(本地当日 0 点起)。
    pub today_water_count: i64,
    /// 近 N 天(默认 7)每日饮水次数,索引 0 = N-1 天前,末位 = 今日。
    pub water_daily_counts: Vec<i64>,
    /// 距今最近一次饮水时间戳(Unix 秒),用于"距下次提醒"计算。无记录则 None。
    pub last_water_ts: Option<i64>,
    /// 今日完成休息次数。
    pub today_rest_count: i64,
    /// 今日完成休息总时长秒数。
    pub today_rest_total_seconds: i64,
    /// 今日久坐提醒触发次数。
    pub today_reminder_count: i64,
    /// 近 N 天每日完成休息次数。
    pub rest_daily_counts: Vec<i64>,
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

/// 读取健康提醒默认配置(供设置页「恢复默认」按钮)。
///
/// Business Logic: 设置页健康提醒 tab 的「恢复默认」需用后端权威默认值重置表单,
///                 与同步/AI tab 的 `get_default_*_config` 行为一致,避免前端硬编码默认值。
/// Code Logic: 返回 `HealthConfig::default()`(config.rs 中已定义,与 serde 单字段缺失回退一致),
///             经 `From<HealthConfig>` 转 DTO 返回;不依赖 State,默认值是纯常量。
#[tauri::command]
pub async fn get_default_health_config() -> Result<HealthConfigDto, AppError> {
    Ok(HealthConfig::default().into())
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
        overlay_rest_end_ts: state
            .health
            .overlay_rest
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.end_ts),
    })
}

#[cfg(test)]
/// 在 ConfigRuntime 上切换 health.enabled。
///
/// Business Logic（为什么需要这个函数）:
///     启用/停用监测必须事务落盘；抽出 helper 便于 save 失败回滚单测。
///
/// Code Logic（这个函数做什么）:
///     事务更新 candidate.health.enabled，返回提交后的 HealthConfigDto。
pub async fn toggle_health_enabled_for_runtime(
    runtime: &ConfigRuntime,
    enabled: bool,
) -> Result<HealthConfigDto, AppError> {
    let (_committed, dto) = update_config_transactionally(runtime, |cfg| {
        cfg.health.enabled = enabled;
        Ok(cfg.health.clone().into())
    })
    .await?;
    Ok(dto)
}

/// 切换监测总开关（写 sidecar 权威 config.health.enabled）。
///
/// Business Logic: 前端「启用/停用久坐监测」开关；关闭后 daemon 仅写库不触发提醒。
/// Code Logic: BackendControlClient 提交 health.enabled patch；刷新本地缓存。
#[tauri::command]
pub async fn toggle_health_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<HealthConfigDto, AppError> {
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            health: Some(HealthRuntimePatch {
                enabled: Some(enabled),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(resp.snapshot.health.into())
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

/// 计算久坐贪睡到期时间戳（纯函数，供命令与单测复用）。
///
/// Business Logic（为什么需要这个函数）:
///     「稍后提醒」必须在改写运行时前拒绝非法 minutes 与溢出，避免静默错误到期。
/// Code Logic（这个函数做什么）:
///     委托 `checked_future_timestamp(now, minutes)`，返回 `now + minutes*60`。
fn prepare_snooze_until(now: i64, minutes: i64) -> Result<i64, AppError> {
    checked_future_timestamp(now, minutes)
}

/// 计算喝水延迟后的 last_drink_ts（纯函数）。
///
/// Business Logic（为什么需要这个函数）:
///     喝水「延迟 N 分钟」需在持锁改 WaterState 前完成范围与检查算术校验。
/// Code Logic（这个函数做什么）:
///     委托 `checked_water_snooze_origin(now, interval, minutes)`。
fn prepare_water_snooze(now: i64, interval: i64, minutes: i64) -> Result<i64, AppError> {
    checked_water_snooze_origin(now, interval, minutes)
}

/// 把前端 DTO 映射为待写入的 `HealthConfig` 并校验归一化。
///
/// Business Logic（为什么需要这个函数）:
///     设置页保存必须在拿到 config 写锁/落盘前拒绝超范围与非法 DND，保证失败无副作用。
/// Code Logic（这个函数做什么）:
///     从 DTO 构造 HealthConfig（water/fullscreen 先置 true），再 `validate_health_config`。
fn prepare_health_config_update(dto: HealthConfigDto) -> Result<HealthConfig, AppError> {
    let mapped = HealthConfig {
        enabled: dto.enabled,
        work_window_seconds: dto.work_window_seconds,
        break_seconds: dto.break_seconds,
        record_window_title: dto.record_window_title,
        retain_days: dto.retain_days,
        notify_enabled: dto.notify_enabled,
        dnd_start: dto.dnd_start,
        dnd_end: dto.dnd_end,
        water_enabled: true,
        water_interval_seconds: dto.water_interval_seconds,
        reminder_fullscreen: true,
    };
    validate_health_config(&mapped)
}

/// 校验后写入久坐贪睡到期时间（测试可注入 HealthRuntime）。
///
/// Business Logic（为什么需要这个函数）:
///     非法 minutes 不得改写 `snooze_until`。
/// Code Logic（这个函数做什么）:
///     `prepare_snooze_until` 成功后再锁 `snooze_until` 写入。
fn apply_snooze_reminder_for_runtime(
    health: &HealthRuntime,
    now: i64,
    minutes: i64,
) -> Result<(), AppError> {
    let until = prepare_snooze_until(now, minutes)?;
    *health.snooze_until.lock().unwrap() = Some(until);
    Ok(())
}

/// 校验后写入喝水延迟起点（测试可注入 HealthRuntime）。
///
/// Business Logic（为什么需要这个函数）:
///     非法 minutes/interval 或溢出不得改写 `WaterState`。
/// Code Logic（这个函数做什么）:
///     `prepare_water_snooze` 成功后再锁 water 写 `last_drink_ts` 并清 pending。
fn apply_snooze_water_reminder_for_runtime(
    health: &HealthRuntime,
    now: i64,
    interval: i64,
    minutes: i64,
) -> Result<(), AppError> {
    let origin = prepare_water_snooze(now, interval, minutes)?;
    let mut w = health.water.lock().unwrap();
    w.last_drink_ts = origin;
    w.pending_remind = false;
    Ok(())
}

/// 贪睡 N 分钟（设置贪睡到期时间戳，期间提醒静默）。
///
/// Business Logic: 前端「稍后提醒」；到期前 daemon 不 emit 提醒事件。
/// Code Logic: 先校验 minutes 1..=1440 与检查算术，再写入 HealthRuntime.snooze_until。
#[tauri::command]
pub async fn snooze_reminder(state: State<'_, AppState>, minutes: i64) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    apply_snooze_reminder_for_runtime(&state.health, now, minutes)
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

#[cfg(test)]
/// 在 ConfigRuntime 上整体覆盖 health 配置。
///
/// Business Logic（为什么需要这个函数）:
///     设置页保存健康配置必须先校验再事务落盘；非法输入拒绝且失败回滚，helper 便于单测与命令层共用。
///
/// Code Logic（这个函数做什么）:
///     先 `prepare_health_config_update`（范围/DND + 归一化）——失败不进事务；
///     通过后 `update_config_transactionally` 覆盖 `cfg.health`，返回提交后的 DTO。
pub async fn update_health_config_for_runtime(
    runtime: &ConfigRuntime,
    config: HealthConfigDto,
) -> Result<HealthConfigDto, AppError> {
    let normalized = prepare_health_config_update(config)?;
    let (_committed, dto) = update_config_transactionally(runtime, move |cfg| {
        cfg.health = normalized;
        Ok(cfg.health.clone().into())
    })
    .await?;
    Ok(dto)
}

/// 更新健康提醒配置（整体覆盖写 sidecar 权威 config.health）。
///
/// Business Logic: 前端设置页「保存」；非法输入拒绝且不改配置；合法值经 owner CAS 持久化。
/// Code Logic: 本地先 validate 归一化，再经 BackendControlClient 提交 HealthRuntimePatch；刷新缓存。
#[tauri::command]
pub async fn update_health_config(
    state: State<'_, AppState>,
    config: HealthConfigDto,
) -> Result<HealthConfigDto, AppError> {
    let normalized = prepare_health_config_update(config)?;
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            health: Some(HealthRuntimePatch {
                enabled: Some(normalized.enabled),
                work_window_seconds: Some(normalized.work_window_seconds),
                break_seconds: Some(normalized.break_seconds),
                record_window_title: Some(normalized.record_window_title),
                retain_days: Some(normalized.retain_days),
                notify_enabled: Some(normalized.notify_enabled),
                dnd_start: Some(normalized.dnd_start.clone()),
                dnd_end: Some(normalized.dnd_end.clone()),
                water_interval_seconds: Some(normalized.water_interval_seconds),
            }),
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(resp.snapshot.health.into())
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
///             pending_remind=false,再 `insert_water(now)` 落库(自增 id 主键,返回值忽略)。
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

/// 跳过当前喝水提醒(推进喝水计时起点 + 清未响应提醒,不入库)。
///
/// Business Logic: 前端喝水遮罩「跳过」按钮;用户暂时不想喝水,需要把下次提醒推迟一个完整间隔,
///                  避免下一 tick 立即再次提醒。与「已饮水」区别在于不落库 water_records
///                  (没有真实喝水行为,不应污染统计)。
/// Code Logic: 拿当前 UTC 时间戳,更新 `HealthRuntime.water.last_drink_ts = now` 并置
///             pending_remind=false,使 daemon 在 `water_interval_seconds` 后才能再次提醒。
///             对照 `record_water` 但去掉 `insert_water` 落库。
#[tauri::command]
pub async fn skip_water_reminder(state: State<'_, AppState>) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    {
        let mut w = state.health.water.lock().unwrap();
        w.last_drink_ts = now;
        w.pending_remind = false;
    }
    Ok(())
}

/// 延迟 N 分钟再提醒喝水(把下次提醒推迟 minutes 分钟 + 清未响应提醒,不入库)。
///
/// Business Logic: 前端喝水遮罩「延迟 5/10 分钟」按钮;用户想稍后再被提醒喝水,需要把下次提醒
///                  推迟指定分钟数(而非一个完整间隔)。不落库(没有真实喝水行为)。非法 minutes 无副作用。
/// Code Logic: 先读 config 取 `water_interval_seconds` 并释放读锁,再 `prepare_water_snooze`
///             校验/检查算术,成功后才写 `last_drink_ts` 并清 pending。
#[tauri::command]
pub async fn snooze_water_reminder(
    state: State<'_, AppState>,
    minutes: i64,
) -> Result<(), AppError> {
    // 先读 interval 并释放 config 读锁,避免跨 await 持 RwLockReadGuard(非 Send)。
    let interval = state.config.read().unwrap().health.water_interval_seconds;
    let now = chrono::Utc::now().timestamp();
    apply_snooze_water_reminder_for_runtime(&state.health, now, interval, minutes)
}

/// 关闭所有全屏健康提醒遮罩窗口,并取消进行中的「开始休息」倒计时。
///
/// Business Logic: 用户在遮罩上点击推迟/跳过/已饮水,或在休息中按 ESC 后需关闭全部遮罩恢复
///     桌面。若此时有进行中的休息倒计时,应一并取消其到点 task——既避免到点重复 record/skip/
///     close,也保留「中途退出(ESC)不记录完整休息」的原语义(只有自然到 0 才 record)。
/// Code Logic: 先 `cancel_overlay_rest(&state.health)` 取消休息到点 task 并清会话,再
///             `close_all_health_overlay_windows(&app)` 关闭全部 `health-overlay-*` 窗口。
#[tauri::command]
pub async fn close_health_overlay(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), AppError> {
    crate::health::cancel_overlay_rest(&state.health);
    crate::health::close_all_health_overlay_windows(&app);
    Ok(())
}

/// 「开始休息」倒计时启动结果 DTO(camelCase,对齐前端)。
///
/// Business Logic: 前端发起窗口点击「开始休息」后立即用 `end_ts` 进入倒计时态(无需等
///     `health:rest-started` 事件往返);同时后端广播事件让其他屏同步。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestStartDto {
    /// 休息结束时间戳(Unix 秒);前端据此显示倒计时。
    pub end_ts: i64,
}

/// 启动「开始休息」全屏遮罩倒计时(后端权威,多屏同步)。
///
/// Business Logic: 多屏每块屏各有一个 reminder 遮罩窗口,在其中一屏点「开始休息」必须让所有
///     屏同步进入倒计时。后端写入权威 `end_ts` 并广播 `health:rest-started` 事件给全部遮罩
///     窗口,各窗口基于同一 `end_ts` 显示;到点后端统一 record + skip + 关闭所有窗口。
/// Code Logic: 读 `config.health.break_seconds` → `now` → `health::start_overlay_rest`
///             (写会话 + emit + spawn 到点收尾 task) → 返回 `{ end_ts }`。
#[tauri::command]
pub async fn start_health_rest(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<RestStartDto, AppError> {
    let break_seconds = state.config.read().unwrap().health.break_seconds;
    let now = chrono::Utc::now().timestamp();
    let end_ts = crate::health::start_overlay_rest(&app, &state, now, break_seconds);
    Ok(RestStartDto { end_ts })
}

/// Business Logic: 用户在习惯统计卡片点「+1 杯」手动加计饮水,无需等提醒。
/// Code Logic: 写入 water_records(自增 id 主键)+ 重置喝水状态机 last_drink_ts/pending,
///             返回新插入记录的自增 id(前端撤销 deleteWaterRecord(id) 需要它)。
#[tauri::command]
pub async fn add_water_manual(state: State<'_, AppState>) -> Result<i64, AppError> {
    let now = chrono::Utc::now().timestamp();
    let id = state.health_repo.insert_water(now).await?;
    {
        let mut w = state.health.water.lock().unwrap();
        w.last_drink_ts = now;
        w.pending_remind = false;
    }
    Ok(id)
}

/// Business Logic: 用户误点"+1 杯"后撤销,按自增 id 删除指定饮水记录。
/// Code Logic: 转发 health_repo.delete_water(id),返回是否实际删除。
#[tauri::command]
pub async fn delete_water_record(state: State<'_, AppState>, id: i64) -> Result<bool, AppError> {
    state.health_repo.delete_water(id).await
}

/// Business Logic: 用户完成休息倒计时后记录一次完整休息,用于习惯统计。
/// Code Logic: duration 取配置 break_seconds(与前端倒计时口径一致),写入 rest_records。
#[tauri::command]
pub async fn record_rest_completed(state: State<'_, AppState>) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let duration = state.config.read().unwrap().health.break_seconds;
    state
        .health_repo
        .insert_rest_record(now, "rest", duration)
        .await?;
    Ok(())
}

/// Business Logic: 习惯统计卡片一次拉取饮水+休息聚合数据,减少前端多次 invoke。
/// Code Logic: 串行查询 water/rest 各聚合方法(单连接池语义下并行无收益),组装 HabitStatsDto。
///             days 默认 7,clamp 到 1..=31(产品最多展示月视图级别,避免过大 days 累加溢出)。
#[tauri::command]
pub async fn get_habit_stats(
    state: State<'_, AppState>,
    days: Option<i64>,
) -> Result<HabitStatsDto, AppError> {
    let days = days.unwrap_or(7).clamp(1, 31) as usize;
    // 今日起点:本地当日 0 点(非 UTC 0 点),保证非 UTC 时区用户看到的"今日"边界正确。
    let today_start = local_start_of_day_ts();
    let trend_start = today_start - ((days as i64) - 1) * 86400;

    let today_water_count = state.health_repo.count_water_since(today_start).await?;
    let water_daily_counts = state
        .health_repo
        .get_daily_water_counts(trend_start, days)
        .await?;
    let last_water_ts = state.health_repo.get_last_water_ts().await?;
    let today_rest_count = state
        .health_repo
        .count_rest_since(today_start, "rest")
        .await?;
    let today_rest_total_seconds = state
        .health_repo
        .sum_rest_duration_since(today_start)
        .await?;
    let today_reminder_count = state
        .health_repo
        .count_rest_since(today_start, "reminder")
        .await?;
    let rest_daily_counts = state
        .health_repo
        .get_daily_rest_counts(trend_start, days, "rest")
        .await?;

    Ok(HabitStatsDto {
        today_water_count,
        water_daily_counts,
        last_water_ts,
        today_rest_count,
        today_rest_total_seconds,
        today_reminder_count,
        rest_daily_counts,
    })
}

#[cfg(test)]
mod default_config_tests {
    use super::*;

    #[test]
    fn default_health_config_dto_matches_documented_defaults() {
        let dto: HealthConfigDto = HealthConfig::default().into();
        assert!(dto.enabled, "默认开启久坐监测");
        assert_eq!(dto.work_window_seconds, 45 * 60);
        assert_eq!(dto.break_seconds, 5 * 60);
        assert!(dto.record_window_title);
        assert_eq!(dto.retain_days, 90);
        assert!(dto.notify_enabled);
        assert_eq!(dto.dnd_start, None);
        assert_eq!(dto.dnd_end, None);
        assert!(dto.water_enabled);
        assert_eq!(dto.water_interval_seconds, 60 * 60);
        assert!(dto.reminder_fullscreen);
    }
}

#[cfg(test)]
mod habit_stats_tests {
    use super::*;

    /// 验证 local_start_of_day_ts 返回本地当日 0 点:必须 <= now,且在一天之内,
    /// 且与"现在"相差的秒数恰好是该日内已过的整秒数(即对齐到本地 00:00:00)。
    #[test]
    fn local_start_of_day_is_local_midnight() {
        use chrono::{Local, Timelike};
        let now_local = Local::now();
        let today_start = local_start_of_day_ts();
        let now_ts = now_local.timestamp();
        // 起点 <= 当前,且不超过 24 小时之内
        assert!(today_start <= now_ts, "本地 0 点不应晚于现在");
        assert!(now_ts - today_start < 86400, "起点应在今天之内");
        // 已过秒数 == 当前本地时间的 H*3600 + M*60 + S
        let elapsed = now_ts - today_start;
        let expected = now_local.num_seconds_from_midnight() as i64;
        assert_eq!(elapsed, expected, "起点应对齐到本地 00:00:00");
    }

    /// days 参数边界:None→7, 0→1, 100→31, 负数→1。
    ///
    /// 本测试显式演示 `Option::unwrap_or` 与 `clamp` 的边界语义，
    /// clippy::unnecessary_literal_unwrap 对常量 None/Some 的告警在此处为预期。
    #[test]
    #[allow(clippy::unnecessary_literal_unwrap)]
    fn days_unwrap_or_default_is_seven() {
        assert_eq!(None::<i64>.unwrap_or(7), 7);
        assert_eq!((Some(0i64)).unwrap_or(7).clamp(1, 31), 1);
        assert_eq!((Some(100i64)).unwrap_or(7).clamp(1, 31), 31);
        assert_eq!((Some(-5i64)).unwrap_or(7).clamp(1, 31), 1);
    }
}

#[cfg(test)]
mod config_writer_tests {
    use super::*;
    use crate::config::{
        AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use std::sync::Arc;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-health-1".into(),
            device_name: "health-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig {
                enabled: true,
                ..HealthConfig::default()
            },
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        }
    }

    /// 验证 health 配置 save 失败时回滚。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     切换监测开关失败时，前端不得看到半提交的 enabled。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fail_next_save 后 toggle enabled=false，断言 Err 且 snapshot 仍 true。
    #[tokio::test]
    async fn save_failure_rolls_back() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store.clone());

        let err = toggle_health_enabled_for_runtime(&runtime, false)
            .await
            .expect_err("should fail");
        assert!(err.to_string().contains("注入故障"));
        assert!(runtime.snapshot().unwrap().health.enabled);
        assert!(store.snapshot().unwrap().health.enabled);
    }

    /// 非法 health 输入在进入 ConfigRuntime 事务前被拒绝，配置不变。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     超范围 work_window 不得落盘或改内存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用非法 DTO 调 update helper，断言 Err 且 snapshot 仍默认 work_window。
    #[tokio::test]
    async fn rejects_invalid_update_without_side_effect() {
        let initial = sample_config();
        let before = initial.health.work_window_seconds;
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::new(initial, store.clone());
        let mut dto: HealthConfigDto = HealthConfig::default().into();
        dto.work_window_seconds = 59;
        let err = update_health_config_for_runtime(&runtime, dto)
            .await
            .expect_err("invalid should fail");
        assert!(err.to_string().contains("health.work_window_seconds"));
        assert_eq!(
            runtime.snapshot().unwrap().health.work_window_seconds,
            before
        );
        assert_eq!(store.snapshot().unwrap().health.work_window_seconds, before);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::validation::{checked_retain_cutoff, validate_health_config_with_field};

    /// 构造合法默认 DTO。
    fn valid_dto() -> HealthConfigDto {
        HealthConfig::default().into()
    }

    #[test]
    fn rejects_invalid_work_window_without_side_effect_on_prepare() {
        let mut dto = valid_dto();
        dto.work_window_seconds = 59;
        assert!(prepare_health_config_update(dto).is_err());
        let mut dto = valid_dto();
        dto.work_window_seconds = 28801;
        assert!(prepare_health_config_update(dto).is_err());
    }

    #[test]
    fn rejects_invalid_dnd_half_pair() {
        let mut dto = valid_dto();
        dto.dnd_start = Some("22:00".into());
        dto.dnd_end = None;
        let err = prepare_health_config_update(dto).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("health.dnd_start") || msg.contains("dnd_end"),
            "msg={msg}"
        );
    }

    #[test]
    fn rejects_invalid_snooze_minutes_without_changing_runtime() {
        let rt = HealthRuntime::new();
        *rt.snooze_until.lock().unwrap() = Some(42);
        let before = *rt.snooze_until.lock().unwrap();
        assert!(apply_snooze_reminder_for_runtime(&rt, 1_000, 0).is_err());
        assert!(apply_snooze_reminder_for_runtime(&rt, 1_000, 1441).is_err());
        assert!(apply_snooze_reminder_for_runtime(&rt, 1_000, i64::MAX).is_err());
        assert_eq!(*rt.snooze_until.lock().unwrap(), before);
    }

    #[test]
    fn rejects_invalid_water_snooze_without_changing_water_state() {
        let rt = HealthRuntime::new();
        {
            let mut w = rt.water.lock().unwrap();
            w.last_drink_ts = 12345;
            w.pending_remind = true;
        }
        let (before_ts, before_pending) = {
            let w = rt.water.lock().unwrap();
            (w.last_drink_ts, w.pending_remind)
        };
        assert!(apply_snooze_water_reminder_for_runtime(&rt, 10_000, 3600, 0).is_err());
        assert!(apply_snooze_water_reminder_for_runtime(&rt, 10_000, 3600, 2000).is_err());
        let w = rt.water.lock().unwrap();
        assert_eq!(w.last_drink_ts, before_ts);
        assert_eq!(w.pending_remind, before_pending);
    }

    #[test]
    fn accepts_valid_snooze_and_water_helpers() {
        let rt = HealthRuntime::new();
        apply_snooze_reminder_for_runtime(&rt, 1_000, 10).unwrap();
        assert_eq!(*rt.snooze_until.lock().unwrap(), Some(1_000 + 600));

        apply_snooze_water_reminder_for_runtime(&rt, 10_000, 3600, 5).unwrap();
        let w = rt.water.lock().unwrap();
        assert_eq!(w.last_drink_ts, 10_000 - 3600 + 300);
        assert!(!w.pending_remind);
    }

    #[test]
    fn rejects_invalid_config_prepare_preserves_normalized_flags_on_ok() {
        let mut dto = valid_dto();
        dto.water_enabled = false;
        dto.reminder_fullscreen = false;
        let cfg = prepare_health_config_update(dto).unwrap();
        assert!(cfg.water_enabled);
        assert!(cfg.reminder_fullscreen);
    }

    /// 模拟 daemon：非法 retain/work_window 时跳过 cutoff，且 checked 算术不 panic。
    #[test]
    fn rejects_invalid_daemon_config_skips_overflowing_cutoff() {
        let bad = HealthConfig {
            work_window_seconds: 59,
            retain_days: 0,
            ..HealthConfig::default()
        };
        let field = validate_health_config_with_field(&bad).unwrap_err();
        assert!(field.starts_with("health."));
        // 即使强行用非法 retain，checked 也应 Err 而非 panic
        assert!(checked_retain_cutoff(100, i64::MAX).is_err());
    }
}
