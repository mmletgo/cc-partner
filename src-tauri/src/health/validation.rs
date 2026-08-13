//! Health 配置与贪睡时间校验 + 检查算术。
//!
//! 将磁盘/DTO 边界与 daemon 共用同一套范围/DND/溢出规则：非法输入返回 Validation，
//! 且调用方须在校验通过前禁止改 config/runtime/DB。

use crate::config::{
    HealthConfig, HealthReminderTemplate, ReminderComplete, ReminderTrigger,
    HEALTH_REMINDER_KEGEL_ID, HEALTH_REMINDER_REST_ID, HEALTH_REMINDER_WATER_ID,
};
use crate::error::AppError;

/// 工作窗口下限（秒）：1 分钟。
pub const WORK_WINDOW_SECONDS_MIN: i64 = 60;
/// 工作窗口上限（秒）：8 小时。
pub const WORK_WINDOW_SECONDS_MAX: i64 = 28800;
/// 有效休息下限（秒）。
pub const BREAK_SECONDS_MIN: i64 = 30;
/// 有效休息上限（秒）：2 小时。
pub const BREAK_SECONDS_MAX: i64 = 7200;
/// 明细保留天数下限。
pub const RETAIN_DAYS_MIN: i64 = 1;
/// 明细保留天数上限（约 10 年）。
pub const RETAIN_DAYS_MAX: i64 = 3650;
/// 喝水间隔下限（秒）：5 分钟。
pub const WATER_INTERVAL_SECONDS_MIN: i64 = 300;
/// 喝水间隔上限（秒）：24 小时。
pub const WATER_INTERVAL_SECONDS_MAX: i64 = 86400;
/// 贪睡分钟下限。
pub const SNOOZE_MINUTES_MIN: i64 = 1;
/// 贪睡分钟上限：24 小时。
pub const SNOOZE_MINUTES_MAX: i64 = 1440;
/// 一天秒数（retain cutoff 用）。
pub const SECONDS_PER_DAY: i64 = 86400;
/// 一分钟秒数。
pub const SECONDS_PER_MINUTE: i64 = 60;
/// 模板会话倒计时下限（秒）。
pub const SESSION_SECONDS_MIN: i64 = 10;
/// 模板会话倒计时上限（秒）：2 小时。
pub const SESSION_SECONDS_MAX: i64 = 7200;
/// 提醒模板上限（含三条内置）。
pub const HEALTH_REMINDER_MAX_COUNT: usize = 12;
/// 模板显示名上限。
pub const REMINDER_NAME_MAX_CHARS: usize = 40;
/// 模板标题上限。
pub const REMINDER_TITLE_MAX_CHARS: usize = 40;
/// 模板正文上限。
pub const REMINDER_BODY_MAX_CHARS: usize = 200;
/// 按钮/单位文案上限。
pub const REMINDER_LABEL_MAX_CHARS: usize = 20;

/// 校验失败时的稳定字段码 + 中文消息（内部用，供 daemon 日志与 AppError 映射）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct HealthFieldError {
    /// 稳定字段码，如 `health.work_window_seconds` / `snooze.minutes`。
    field: &'static str,
    /// 面向用户/日志的完整消息（含字段码）。
    message: String,
}

impl HealthFieldError {
    /// Business Logic（为什么需要这个函数）:
    ///     校验失败需要同时带稳定字段码（daemon 日志）与可读消息（命令层 Validation）。
    /// Code Logic（这个函数做什么）:
    ///     组装 `field` 与 `"{field} {detail}"` 风格消息。
    fn new(field: &'static str, detail: impl AsRef<str>) -> Self {
        let detail = detail.as_ref();
        Self {
            field,
            message: format!("{field} {detail}"),
        }
    }
}

/// 校验 Health 配置范围与 DND，并返回归一化副本。
///
/// Business Logic（为什么需要这个函数）:
///     设置页保存与 daemon 每 tick 都需要拒绝超范围/半开 DND 配置，避免错误计时、永不提醒或溢出清理；
///     同时把历史 `water_enabled`/`reminder_fullscreen` 归一为 true，保证运行语义一致。
/// Code Logic（这个函数做什么）:
///     检查 work/break/retain/water 范围与 DND 对，成功则 clone 并强制 `water_enabled=true`、
///     `reminder_fullscreen=true` 后返回；失败映射为 `AppError::validation`（消息含稳定字段码）。
pub fn validate_health_config(input: &HealthConfig) -> Result<HealthConfig, AppError> {
    validate_health_config_inner(input).map_err(|e| AppError::validation(e.message))
}

/// 校验 Health 配置；失败时只返回稳定字段码（供 daemon 日志）。
///
/// Business Logic（为什么需要这个函数）:
///     daemon 在非法磁盘配置时需 `tracing::warn!(field=...)` 跳过提醒/清理，不能 panic 或产生副作用。
/// Code Logic（这个函数做什么）:
///     复用内部校验；Ok 为归一化 `HealthConfig`，Err 为 `'static` 字段码。
pub fn validate_health_config_with_field(
    input: &HealthConfig,
) -> Result<HealthConfig, &'static str> {
    validate_health_config_inner(input).map_err(|e| e.field)
}

/// 仅校验范围/DND，不强制改写调用方持有的配置（供 `AppConfig::validate`）。
///
/// Business Logic（为什么需要这个函数）:
///     磁盘 load/save 路径只需要知道配置是否合法，不必在 validate 阶段改写 memory 中的 health 字段。
/// Code Logic（这个函数做什么）:
///     调用内部校验并丢弃归一化结果，错误映射为 `AppError::validation`。
pub fn validate_health_config_fields(health: &HealthConfig) -> Result<(), AppError> {
    validate_health_config_inner(health)
        .map(|_| ())
        .map_err(|e| AppError::validation(e.message))
}

/// 内部统一校验：范围 + DND，成功返回归一化 clone。
///
/// Business Logic（为什么需要这个函数）:
///     命令层、daemon、AppConfig 三处必须共享同一规则，避免边界不一致。
/// Code Logic（这个函数做什么）:
///     逐字段检查闭区间；校验 DND 对；空 reminders 从旧标量 seed；校验模板后镜像回写
///     work/water 标量；clone 后强制 water/fullscreen 为 true。
fn validate_health_config_inner(input: &HealthConfig) -> Result<HealthConfig, HealthFieldError> {
    if !(WORK_WINDOW_SECONDS_MIN..=WORK_WINDOW_SECONDS_MAX).contains(&input.work_window_seconds) {
        return Err(HealthFieldError::new(
            "health.work_window_seconds",
            format!("必须在 {WORK_WINDOW_SECONDS_MIN}..={WORK_WINDOW_SECONDS_MAX}"),
        ));
    }
    if !(BREAK_SECONDS_MIN..=BREAK_SECONDS_MAX).contains(&input.break_seconds) {
        return Err(HealthFieldError::new(
            "health.break_seconds",
            format!("必须在 {BREAK_SECONDS_MIN}..={BREAK_SECONDS_MAX}"),
        ));
    }
    if !(RETAIN_DAYS_MIN..=RETAIN_DAYS_MAX).contains(&input.retain_days) {
        return Err(HealthFieldError::new(
            "health.retain_days",
            format!("必须在 {RETAIN_DAYS_MIN}..={RETAIN_DAYS_MAX}"),
        ));
    }
    if !(WATER_INTERVAL_SECONDS_MIN..=WATER_INTERVAL_SECONDS_MAX)
        .contains(&input.water_interval_seconds)
    {
        return Err(HealthFieldError::new(
            "health.water_interval_seconds",
            format!("必须在 {WATER_INTERVAL_SECONDS_MIN}..={WATER_INTERVAL_SECONDS_MAX}"),
        ));
    }
    validate_dnd_pair_inner(input.dnd_start.as_deref(), input.dnd_end.as_deref())?;

    let mut out = input.clone();
    if out.reminders.is_empty() {
        out.ensure_reminders();
    }
    validate_and_mirror_reminders(&mut out)?;
    out.water_enabled = true;
    out.reminder_fullscreen = true;
    Ok(out)
}

/// 校验模板列表并回写 rest/water 兼容镜像。
///
/// Business Logic（为什么需要这个函数）:
///     内置三项不可删、条数/范围/文案必须统一拒绝，且旧标量要跟模板一致以便回滚。
/// Code Logic（这个函数做什么）:
///     检查上限、id、内置存在与 builtin 标记、触发/完成秒数与文案长度；
///     再把 rest 阈值、water 间隔写回 work_window_seconds / water_interval_seconds。
fn validate_and_mirror_reminders(cfg: &mut HealthConfig) -> Result<(), HealthFieldError> {
    if cfg.reminders.len() > HEALTH_REMINDER_MAX_COUNT {
        return Err(HealthFieldError::new(
            "health.reminders",
            format!("最多 {HEALTH_REMINDER_MAX_COUNT} 条"),
        ));
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (idx, template) in cfg.reminders.iter().enumerate() {
        validate_reminder_template(idx, template)?;
        if !seen.insert(template.id.clone()) {
            return Err(HealthFieldError::new(
                "health.reminders",
                format!("id 重复: {}", template.id),
            ));
        }
    }

    for required in [
        HEALTH_REMINDER_WATER_ID,
        HEALTH_REMINDER_REST_ID,
        HEALTH_REMINDER_KEGEL_ID,
    ] {
        match cfg.reminders.iter().find(|t| t.id == required) {
            None => {
                return Err(HealthFieldError::new(
                    "health.reminders",
                    format!("缺少内置模板 {required}"),
                ));
            }
            Some(t) if !t.builtin => {
                return Err(HealthFieldError::new(
                    "health.reminders",
                    format!("内置模板 {required} 的 builtin 不能改为 false"),
                ));
            }
            Some(_) => {}
        }
    }

    let rest_threshold = cfg
        .reminders
        .iter()
        .find(|t| t.id == HEALTH_REMINDER_REST_ID)
        .and_then(|t| t.threshold_seconds)
        .unwrap_or(cfg.work_window_seconds);
    let water_interval = cfg
        .reminders
        .iter()
        .find(|t| t.id == HEALTH_REMINDER_WATER_ID)
        .and_then(|t| t.interval_seconds)
        .unwrap_or(cfg.water_interval_seconds);
    cfg.work_window_seconds = rest_threshold;
    cfg.water_interval_seconds = water_interval;
    Ok(())
}

/// 校验单条模板的 id、触发/完成参数与文案长度。
///
/// Business Logic（为什么需要这个函数）:
///     非法秒数或空白 id 会让调度永不触发或每 tick 重复弹窗。
/// Code Logic（这个函数做什么）:
///     id 去空白后必须非空且无空白字符；interval/threshold/session 按触发/完成方式取范围；
///     name/title/body/label 按字符数上限拒绝。
fn validate_reminder_template(
    idx: usize,
    template: &HealthReminderTemplate,
) -> Result<(), HealthFieldError> {
    let id = template.id.trim();
    if id.is_empty() || id.chars().any(|c| c.is_whitespace()) {
        return Err(HealthFieldError::new(
            "health.reminders",
            format!("[{idx}] id 不能为空或含空白"),
        ));
    }
    if template.id != id {
        return Err(HealthFieldError::new(
            "health.reminders",
            format!("[{idx}] id 不能含首尾空白"),
        ));
    }

    match template.trigger {
        ReminderTrigger::Interval => {
            let secs = template.interval_seconds.ok_or_else(|| {
                HealthFieldError::new("health.reminders", format!("[{idx}] interval 必须提供间隔秒数"))
            })?;
            if !(WATER_INTERVAL_SECONDS_MIN..=WATER_INTERVAL_SECONDS_MAX).contains(&secs) {
                return Err(HealthFieldError::new(
                    "health.reminders",
                    format!(
                        "[{idx}] intervalSeconds 必须在 {WATER_INTERVAL_SECONDS_MIN}..={WATER_INTERVAL_SECONDS_MAX}"
                    ),
                ));
            }
        }
        ReminderTrigger::Sedentary => {
            let secs = template.threshold_seconds.ok_or_else(|| {
                HealthFieldError::new(
                    "health.reminders",
                    format!("[{idx}] sedentary 必须提供阈值秒数"),
                )
            })?;
            if !(WORK_WINDOW_SECONDS_MIN..=WORK_WINDOW_SECONDS_MAX).contains(&secs) {
                return Err(HealthFieldError::new(
                    "health.reminders",
                    format!(
                        "[{idx}] thresholdSeconds 必须在 {WORK_WINDOW_SECONDS_MIN}..={WORK_WINDOW_SECONDS_MAX}"
                    ),
                ));
            }
        }
    }

    match template.complete {
        ReminderComplete::Instant => {}
        ReminderComplete::Session => {
            let secs = template.session_seconds.ok_or_else(|| {
                HealthFieldError::new(
                    "health.reminders",
                    format!("[{idx}] session 必须提供倒计时秒数"),
                )
            })?;
            if !(SESSION_SECONDS_MIN..=SESSION_SECONDS_MAX).contains(&secs) {
                return Err(HealthFieldError::new(
                    "health.reminders",
                    format!(
                        "[{idx}] sessionSeconds 必须在 {SESSION_SECONDS_MIN}..={SESSION_SECONDS_MAX}"
                    ),
                ));
            }
        }
    }

    reject_too_long("name", &template.name, REMINDER_NAME_MAX_CHARS)?;
    reject_too_long("title", &template.title, REMINDER_TITLE_MAX_CHARS)?;
    reject_too_long("body", &template.body, REMINDER_BODY_MAX_CHARS)?;
    reject_too_long("confirmLabel", &template.confirm_label, REMINDER_LABEL_MAX_CHARS)?;
    reject_too_long("unitLabel", &template.unit_label, REMINDER_LABEL_MAX_CHARS)?;
    Ok(())
}

/// 按 Unicode 标量计数字符长度上限。
///
/// Business Logic（为什么需要这个函数）:
///     设置页文案过长会撑破遮罩与通知，必须在落盘前拒绝。
/// Code Logic（这个函数做什么）:
///     `chars().count()` 超过 max 则返回 `health.reminders` 校验错误。
fn reject_too_long(field: &str, value: &str, max_chars: usize) -> Result<(), HealthFieldError> {
    if value.chars().count() > max_chars {
        return Err(HealthFieldError::new(
            "health.reminders",
            format!("{field} 最多 {max_chars} 个字符"),
        ));
    }
    Ok(())
}

/// 校验免打扰起止时间对。
///
/// Business Logic（为什么需要这个函数）:
///     只填一端会让 DND 语义歧义；格式错误会导致运行时解析失败或静默永不生效。
/// Code Logic（这个函数做什么）:
///     两端皆空/None 通过；两端皆 present 且严格 `HH:MM`（两位小时 00-23、分钟 00-59）；
///     起止相等表示全天 DND（合法）。错误消息含 `health.dnd_start` / `health.dnd_end` 字段码。
pub fn validate_dnd_pair(start: Option<&str>, end: Option<&str>) -> Result<(), AppError> {
    validate_dnd_pair_inner(start, end).map_err(|e| AppError::validation(e.message))
}

/// DND 对内部校验（返回字段码错误）。
///
/// Business Logic（为什么需要这个函数）:
///     与 public API 共用同一规则，并保留字段码供 daemon 日志。
/// Code Logic（这个函数做什么）:
///     trim 后空串视为空；两端空 Ok；两端有值则各自 parse_strict_hhmm；半开报错。
fn validate_dnd_pair_inner(start: Option<&str>, end: Option<&str>) -> Result<(), HealthFieldError> {
    let start_v = start.map(str::trim).filter(|s| !s.is_empty());
    let end_v = end.map(str::trim).filter(|s| !s.is_empty());
    match (start_v, end_v) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) => {
            parse_strict_hhmm(s)
                .map_err(|m| HealthFieldError::new("health.dnd_start", format!("非法: {m}")))?;
            parse_strict_hhmm(e)
                .map_err(|m| HealthFieldError::new("health.dnd_end", format!("非法: {m}")))?;
            Ok(())
        }
        _ => Err(HealthFieldError::new(
            "health.dnd_start",
            "与 dnd_end 必须同时为空或同时为 HH:MM",
        )),
    }
}

/// 解析严格两位 `HH:MM`。
///
/// Business Logic（为什么需要这个函数）:
///     DND 只接受规范两位数字，避免 `9:00`/`24:00`/`23:60` 等歧义输入进入运行时。
/// Code Logic（这个函数做什么）:
///     要求长度 5、中间为 `:`，小时 00-23、分钟 00-59，全部为 ASCII 数字；返回 (hour, minute)。
fn parse_strict_hhmm(value: &str) -> Result<(u8, u8), String> {
    let value = value.trim();
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        return Err("必须为严格 HH:MM".into());
    }
    let (h, m) = value.split_at(2);
    let m = &m[1..];
    if !h.chars().all(|c| c.is_ascii_digit()) || !m.chars().all(|c| c.is_ascii_digit()) {
        return Err("时分必须为数字".into());
    }
    let hour: u8 = h.parse().map_err(|_| "小时解析失败".to_string())?;
    let minute: u8 = m.parse().map_err(|_| "分钟解析失败".to_string())?;
    if hour > 23 {
        return Err("小时必须在 00..=23".into());
    }
    if minute > 59 {
        return Err("分钟必须在 00..=59".into());
    }
    Ok((hour, minute))
}

/// 校验贪睡分钟并计算到期时间戳：`now + minutes*60`。
///
/// Business Logic（为什么需要这个函数）:
///     「稍后提醒」必须限制在 1..=1440 分钟，并用检查算术避免 i64 溢出改写运行时。
/// Code Logic（这个函数做什么）:
///     先校验 minutes 范围，再 `checked_mul(60)` + `checked_add(now)`；溢出 → Validation。
pub fn checked_future_timestamp(now: i64, minutes: i64) -> Result<i64, AppError> {
    if !(SNOOZE_MINUTES_MIN..=SNOOZE_MINUTES_MAX).contains(&minutes) {
        return Err(AppError::validation(format!(
            "snooze.minutes 必须在 {SNOOZE_MINUTES_MIN}..={SNOOZE_MINUTES_MAX}"
        )));
    }
    let secs = minutes
        .checked_mul(SECONDS_PER_MINUTE)
        .ok_or_else(|| AppError::validation("snooze.minutes 溢出"))?;
    now.checked_add(secs)
        .ok_or_else(|| AppError::validation("snooze.until 时间戳溢出"))
}

/// 计算喝水延迟后的 `last_drink_ts`：`now - interval + minutes*60`。
///
/// Business Logic（为什么需要这个函数）:
///     喝水「延迟 N 分钟」需把计时起点回拨，使距离下次阈值还差 N 分钟；非法 minutes 或算术溢出
///     不得改写 `WaterState`。
/// Code Logic（这个函数做什么）:
///     校验 minutes 1..=1440；interval 必须 >0（否则 Validation）；全程 checked_mul/add/sub。
pub fn checked_water_snooze_origin(now: i64, interval: i64, minutes: i64) -> Result<i64, AppError> {
    if !(SNOOZE_MINUTES_MIN..=SNOOZE_MINUTES_MAX).contains(&minutes) {
        return Err(AppError::validation(format!(
            "snooze.minutes 必须在 {SNOOZE_MINUTES_MIN}..={SNOOZE_MINUTES_MAX}"
        )));
    }
    if interval <= 0 {
        return Err(AppError::validation(
            "health.water_interval_seconds 必须为正数",
        ));
    }
    let add_secs = minutes
        .checked_mul(SECONDS_PER_MINUTE)
        .ok_or_else(|| AppError::validation("snooze.minutes 溢出"))?;
    let after_sub = now
        .checked_sub(interval)
        .ok_or_else(|| AppError::validation("snooze.water_origin 时间戳溢出"))?;
    after_sub
        .checked_add(add_secs)
        .ok_or_else(|| AppError::validation("snooze.water_origin 时间戳溢出"))
}

/// 计算明细清理 cutoff：`now - retain_days * 86400`。
///
/// Business Logic（为什么需要这个函数）:
///     daemon 清理过期活动/饮水/休息记录时，retain_days 过大或 now 过小会导致 i64 下溢 panic/错误删除。
/// Code Logic（这个函数做什么）:
///     使用 checked_mul(86400) + checked_sub；溢出 → Validation（调用方应跳过 cleanup）。
pub fn checked_retain_cutoff(now: i64, retain_days: i64) -> Result<i64, AppError> {
    let secs = retain_days
        .checked_mul(SECONDS_PER_DAY)
        .ok_or_else(|| AppError::validation("health.retain_days 溢出"))?;
    now.checked_sub(secs)
        .ok_or_else(|| AppError::validation("health.retain_cutoff 溢出"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cfg() -> HealthConfig {
        HealthConfig::default()
    }

    #[test]
    fn work_window_boundaries() {
        let mut c = base_cfg();
        c.work_window_seconds = WORK_WINDOW_SECONDS_MIN - 1;
        assert!(validate_health_config(&c).is_err());
        c.work_window_seconds = WORK_WINDOW_SECONDS_MIN;
        assert!(validate_health_config(&c).is_ok());
        c.work_window_seconds = WORK_WINDOW_SECONDS_MAX;
        assert!(validate_health_config(&c).is_ok());
        c.work_window_seconds = WORK_WINDOW_SECONDS_MAX + 1;
        assert!(validate_health_config(&c).is_err());
        c.work_window_seconds = i64::MIN;
        assert!(validate_health_config(&c).is_err());
        c.work_window_seconds = i64::MAX;
        assert!(validate_health_config(&c).is_err());
    }

    #[test]
    fn break_seconds_boundaries() {
        let mut c = base_cfg();
        c.break_seconds = BREAK_SECONDS_MIN - 1;
        assert!(validate_health_config(&c).is_err());
        c.break_seconds = BREAK_SECONDS_MIN;
        assert!(validate_health_config(&c).is_ok());
        c.break_seconds = BREAK_SECONDS_MAX;
        assert!(validate_health_config(&c).is_ok());
        c.break_seconds = BREAK_SECONDS_MAX + 1;
        assert!(validate_health_config(&c).is_err());
        c.break_seconds = i64::MIN;
        assert!(validate_health_config(&c).is_err());
        c.break_seconds = i64::MAX;
        assert!(validate_health_config(&c).is_err());
    }

    #[test]
    fn retain_days_boundaries() {
        let mut c = base_cfg();
        c.retain_days = RETAIN_DAYS_MIN - 1;
        assert!(validate_health_config(&c).is_err());
        c.retain_days = RETAIN_DAYS_MIN;
        assert!(validate_health_config(&c).is_ok());
        c.retain_days = RETAIN_DAYS_MAX;
        assert!(validate_health_config(&c).is_ok());
        c.retain_days = RETAIN_DAYS_MAX + 1;
        assert!(validate_health_config(&c).is_err());
        c.retain_days = i64::MIN;
        assert!(validate_health_config(&c).is_err());
        c.retain_days = i64::MAX;
        assert!(validate_health_config(&c).is_err());
    }

    #[test]
    fn water_interval_boundaries() {
        let mut c = base_cfg();
        c.water_interval_seconds = WATER_INTERVAL_SECONDS_MIN - 1;
        assert!(validate_health_config(&c).is_err());
        c.water_interval_seconds = WATER_INTERVAL_SECONDS_MIN;
        assert!(validate_health_config(&c).is_ok());
        c.water_interval_seconds = WATER_INTERVAL_SECONDS_MAX;
        assert!(validate_health_config(&c).is_ok());
        c.water_interval_seconds = WATER_INTERVAL_SECONDS_MAX + 1;
        assert!(validate_health_config(&c).is_err());
        c.water_interval_seconds = i64::MIN;
        assert!(validate_health_config(&c).is_err());
        c.water_interval_seconds = i64::MAX;
        assert!(validate_health_config(&c).is_err());
    }

    #[test]
    fn normalize_water_and_fullscreen_flags() {
        let mut c = base_cfg();
        c.water_enabled = false;
        c.reminder_fullscreen = false;
        let out = validate_health_config(&c).expect("valid ranges");
        assert!(out.water_enabled);
        assert!(out.reminder_fullscreen);
    }

    #[test]
    fn dnd_pair_rules() {
        assert!(validate_dnd_pair(None, None).is_ok());
        assert!(validate_dnd_pair(Some(""), Some("  ")).is_ok());
        assert!(validate_dnd_pair(Some("22:00"), None).is_err());
        assert!(validate_dnd_pair(None, Some("07:00")).is_err());
        assert!(validate_dnd_pair(Some("7:00"), Some("08:00")).is_err());
        assert!(validate_dnd_pair(Some("24:00"), Some("08:00")).is_err());
        assert!(validate_dnd_pair(Some("23:60"), Some("08:00")).is_err());
        assert!(validate_dnd_pair(Some("22:00"), Some("07:00")).is_ok()); // 跨午夜
        assert!(validate_dnd_pair(Some("12:00"), Some("12:00")).is_ok()); // 全天 DND
    }

    #[test]
    fn field_code_on_invalid_work_window() {
        let mut c = base_cfg();
        c.work_window_seconds = 0;
        // HealthConfig 未 derive PartialEq，不能对 Result 整体 assert_eq。
        match validate_health_config_with_field(&c) {
            Err(field) => assert_eq!(field, "health.work_window_seconds"),
            Ok(_) => panic!("expected validation error"),
        }
    }

    #[test]
    fn snooze_minutes_and_future_timestamp() {
        assert!(checked_future_timestamp(1_000, 0).is_err());
        assert!(checked_future_timestamp(1_000, 1441).is_err());
        assert!(checked_future_timestamp(1_000, i64::MIN).is_err());
        assert!(checked_future_timestamp(1_000, i64::MAX).is_err());
        assert_eq!(checked_future_timestamp(1_000, 1).unwrap(), 1_060);
        assert_eq!(
            checked_future_timestamp(1_000, 1440).unwrap(),
            1_000 + 1440 * 60
        );
        // 接近 i64::MAX 的 now 会溢出
        assert!(checked_future_timestamp(i64::MAX - 10, 1).is_err());
    }

    #[test]
    fn water_snooze_origin_checked() {
        let now = 10_000;
        let interval = 3600;
        assert_eq!(
            checked_water_snooze_origin(now, interval, 5).unwrap(),
            now - interval + 5 * 60
        );
        assert!(checked_water_snooze_origin(now, interval, 0).is_err());
        assert!(checked_water_snooze_origin(now, 0, 5).is_err());
        assert!(checked_water_snooze_origin(now, -1, 5).is_err());
        assert!(checked_water_snooze_origin(i64::MIN + 1, interval, 5).is_err());
    }

    #[test]
    fn retain_cutoff_checked() {
        assert_eq!(
            checked_retain_cutoff(1_000_000, 1).unwrap(),
            1_000_000 - 86400
        );
        assert!(checked_retain_cutoff(100, i64::MAX).is_err());
        assert!(checked_retain_cutoff(i64::MIN + 1, 2).is_err());
    }

    #[test]
    fn daemon_skip_path_does_not_panic_on_invalid_or_overflow() {
        // 模拟 daemon：非法配置 → 跳过 reminder/cleanup；不 panic
        let mut bad = base_cfg();
        bad.retain_days = 0;
        bad.work_window_seconds = 59;
        match validate_health_config_with_field(&bad) {
            Ok(_) => panic!("expected invalid"),
            Err(field) => {
                assert!(field.starts_with("health."), "stable field code: {field}");
                // 不计算 overflowing cutoff
            }
        }
        // 合法 retain 用 checked 算术
        let ok = base_cfg();
        let cutoff = checked_retain_cutoff(1_700_000_000, ok.retain_days).unwrap();
        assert!(cutoff < 1_700_000_000);
    }

    fn template_by_id<'a>(
        cfg: &'a crate::config::HealthConfig,
        id: &str,
    ) -> &'a crate::config::HealthReminderTemplate {
        cfg.reminders
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("missing template {id}"))
    }

    #[test]
    fn default_health_config_seeds_three_builtin_templates() {
        let cfg = HealthConfig::default();
        let ids: Vec<&str> = cfg.reminders.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["water", "rest", "kegel"]);

        let water = template_by_id(&cfg, "water");
        assert!(water.builtin && water.enabled);
        assert_eq!(water.trigger, crate::config::ReminderTrigger::Interval);
        assert_eq!(water.complete, crate::config::ReminderComplete::Instant);
        assert_eq!(water.interval_seconds, Some(3600));
        assert_eq!(water.name, "饮水");
        assert_eq!(water.confirm_label, "已喝水");
        assert_eq!(water.unit_label, "杯");

        let rest = template_by_id(&cfg, "rest");
        assert!(rest.builtin && rest.enabled);
        assert_eq!(rest.trigger, crate::config::ReminderTrigger::Sedentary);
        assert_eq!(rest.complete, crate::config::ReminderComplete::Session);
        assert_eq!(rest.threshold_seconds, Some(2700));
        assert_eq!(rest.session_seconds, Some(300));
        assert_eq!(rest.confirm_label, "开始休息");

        let kegel = template_by_id(&cfg, "kegel");
        assert!(kegel.builtin && kegel.enabled);
        assert_eq!(kegel.trigger, crate::config::ReminderTrigger::Interval);
        assert_eq!(kegel.complete, crate::config::ReminderComplete::Session);
        assert_eq!(kegel.interval_seconds, Some(7200));
        assert_eq!(kegel.session_seconds, Some(30));
        assert_eq!(kegel.title, "该活动一下了");
        assert_eq!(kegel.confirm_label, "开始");
        assert_eq!(kegel.unit_label, "次");
        assert!(
            !kegel.body.contains("提肛") && !kegel.name.contains("凯格尔"),
            "出厂文案不得写医学/解剖细节"
        );
    }

    #[test]
    fn missing_reminders_seed_from_legacy_fields() {
        let cfg: HealthConfig = serde_json::from_str(
            r#"{
                "enabled": true,
                "work_window_seconds": 1800,
                "break_seconds": 300,
                "record_window_title": true,
                "retain_days": 90,
                "notify_enabled": true,
                "water_enabled": true,
                "water_interval_seconds": 1800,
                "reminder_fullscreen": true
            }"#,
        )
        .expect("legacy health json");
        assert!(
            cfg.reminders.is_empty(),
            "缺 reminders 字段应反序列化为空数组，再由校验 seed"
        );

        let out = validate_health_config(&cfg).expect("legacy config should seed");
        assert_eq!(out.reminders.len(), 3);
        assert_eq!(
            template_by_id(&out, "rest").threshold_seconds,
            Some(1800),
            "rest 阈值应从旧 work_window_seconds 迁移"
        );
        assert_eq!(
            template_by_id(&out, "water").interval_seconds,
            Some(1800),
            "water 间隔应从旧 water_interval_seconds 迁移"
        );
        assert_eq!(template_by_id(&out, "kegel").interval_seconds, Some(7200));
        assert_eq!(template_by_id(&out, "kegel").session_seconds, Some(30));
    }

    #[test]
    fn validate_mirrors_legacy_fields_from_builtin_templates() {
        let mut cfg = base_cfg();
        for t in &mut cfg.reminders {
            if t.id == "rest" {
                t.threshold_seconds = Some(1200);
            }
            if t.id == "water" {
                t.interval_seconds = Some(900);
            }
        }
        let out = validate_health_config(&cfg).expect("valid templates");
        assert_eq!(out.work_window_seconds, 1200);
        assert_eq!(out.water_interval_seconds, 900);
    }

    #[test]
    fn rejects_deleting_or_unmarking_builtin_templates() {
        let mut missing_kegel = base_cfg();
        missing_kegel.reminders.retain(|t| t.id != "kegel");
        let err = validate_health_config(&missing_kegel).unwrap_err().to_string();
        assert!(
            err.contains("health.reminders") && err.contains("kegel"),
            "msg={err}"
        );

        let mut not_builtin = base_cfg();
        template_by_id_mut(&mut not_builtin, "water").builtin = false;
        let err = validate_health_config(&not_builtin).unwrap_err().to_string();
        assert!(err.contains("health.reminders"), "msg={err}");
    }

    fn template_by_id_mut<'a>(
        cfg: &'a mut HealthConfig,
        id: &str,
    ) -> &'a mut crate::config::HealthReminderTemplate {
        cfg.reminders
            .iter_mut()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("missing template {id}"))
    }

    #[test]
    fn rejects_more_than_twelve_templates() {
        let mut cfg = base_cfg();
        for i in 0..10 {
            cfg.reminders.push(custom_interval_instant(&format!("c{i}")));
        }
        assert_eq!(cfg.reminders.len(), 13);
        let err = validate_health_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("health.reminders"), "msg={err}");
    }

    fn custom_interval_instant(id: &str) -> crate::config::HealthReminderTemplate {
        crate::config::HealthReminderTemplate {
            id: id.into(),
            builtin: false,
            enabled: true,
            name: "自定义".into(),
            trigger: crate::config::ReminderTrigger::Interval,
            interval_seconds: Some(3600),
            threshold_seconds: None,
            complete: crate::config::ReminderComplete::Instant,
            session_seconds: None,
            title: "该活动一下了".into(),
            body: "活动一下再继续。".into(),
            confirm_label: "完成".into(),
            unit_label: "次".into(),
        }
    }

    #[test]
    fn rejects_duplicate_or_blank_ids() {
        let mut dup = base_cfg();
        dup.reminders.push(custom_interval_instant("water"));
        let err = validate_health_config(&dup).unwrap_err().to_string();
        assert!(err.contains("health.reminders"), "msg={err}");

        let mut blank = base_cfg();
        blank.reminders.push(custom_interval_instant("  "));
        let err = validate_health_config(&blank).unwrap_err().to_string();
        assert!(err.contains("health.reminders"), "msg={err}");
    }

    #[test]
    fn rejects_out_of_range_template_seconds_and_copy() {
        let mut interval = base_cfg();
        template_by_id_mut(&mut interval, "water").interval_seconds = Some(299);
        assert!(validate_health_config(&interval).is_err());

        let mut threshold = base_cfg();
        template_by_id_mut(&mut threshold, "rest").threshold_seconds = Some(59);
        assert!(validate_health_config(&threshold).is_err());

        let mut session = base_cfg();
        template_by_id_mut(&mut session, "kegel").session_seconds = Some(9);
        assert!(validate_health_config(&session).is_err());

        let mut name = base_cfg();
        template_by_id_mut(&mut name, "water").name = "x".repeat(41);
        let err = validate_health_config(&name).unwrap_err().to_string();
        assert!(err.contains("health.reminders"), "msg={err}");
    }

    #[test]
    fn health_runtime_patch_accepts_reminders_allowlist() {
        let json = r#"{
            "reminders": [{
                "id": "water",
                "builtin": true,
                "enabled": true,
                "name": "饮水",
                "trigger": "interval",
                "intervalSeconds": 3600,
                "complete": "instant",
                "title": "该喝水啦",
                "body": "记得补充水分。",
                "confirmLabel": "已喝水",
                "unitLabel": "杯"
            }]
        }"#;
        let patch: crate::config_runtime::HealthRuntimePatch =
            serde_json::from_str(json).expect("patch should accept reminders");
        assert!(
            patch.reminders.is_some(),
            "HealthRuntimePatch 必须允许 reminders"
        );
    }
}
