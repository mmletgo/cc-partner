//! 提醒辅助逻辑:免打扰时段判定(纯函数,可单测)。
//! 免打扰支持跨午夜:dnd_start=22:00, dnd_end=07:00 表示 22:00~次日 07:00 静默。

use chrono::{Local, NaiveTime, TimeZone, Timelike};

/// 判断 now_ts 的「本地」时分是否落在免打扰区间 [start, end)。
///
/// Business Logic（为什么需要这个函数）:
///     健康提醒 daemon（Task 6）在到达提醒触发点时，需要先判断当前是否处于
///     用户设定的免打扰时段（如夜间 22:00~07:00）。命中免打扰则不弹通知，
///     避免深夜打扰用户休息。免打扰时段是用户的「本地作息」概念，必须用本地
///     时区判定；用 UTC 会在东八区偏移 8 小时（22:00 本地变 14:00 UTC）。
///
/// Code Logic（这个函数做什么）:
///     - 接收当前 Unix 时间戳 now_ts（秒）与免打扰起止 "HH:MM" 字符串。
///     - 任一参数为 None 或解析失败 → 返回 false（不免打扰）。
///     - 用 `chrono::Local::from_timestamp(now_ts)` 把时间戳转成本地 NaiveDateTime，
///       取 `hour()*60 + minute()` 作 now_mins（系统本地时区，单人单机，不引 chrono-tz）。
///       start/end 同样换算成分钟数。
///     - 普通区间（start_mins <= end_mins）: 命中条件 now_mins ∈ [start, end)，
///       start inclusive、end exclusive。
///     - 跨午夜区间（start_mins > end_mins，如 22:00~07:00）: 命中条件为
///       now_mins ∈ [start, 24:00) ∪ [00:00, end)。
pub fn is_in_dnd(now_ts: i64, dnd_start: Option<&str>, dnd_end: Option<&str>) -> bool {
    let (Some(s), Some(e)) = (dnd_start, dnd_end) else { return false; };
    let (Ok(start), Ok(end)) = (NaiveTime::parse_from_str(s, "%H:%M"), NaiveTime::parse_from_str(e, "%H:%M"))
        else { return false; };
    let local = Local
        .timestamp_opt(now_ts, 0)
        .single()
        .expect("valid unix timestamp");
    let now_mins = local.hour() * 60 + local.minute();
    let start_mins = start.hour() * 60 + start.minute();
    let end_mins = end.hour() * 60 + end.minute();
    if start_mins <= end_mins {
        now_mins >= start_mins && now_mins < end_mins
    } else {
        now_mins >= start_mins || now_mins < end_mins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, NaiveDate};

    /// 把「本地某天 HH:MM」转成 Unix 时间戳（秒），使测试与运行机器的时区无关:
    /// 无论在东八区还是 UTC，构造「本地 12:00」的 ts，断言逻辑都应一致。
    fn local_ts(hour: u32, min: u32) -> i64 {
        let today = Local::now().naive_local().date();
        let date = NaiveDate::from_ymd_opt(today.year(), today.month(), today.day())
            .expect("valid today");
        let time = date.and_hms_opt(hour, min, 0).expect("valid hms");
        Local
            .from_local_datetime(&time)
            .single()
            .expect("non-ambiguous local time")
            .timestamp()
    }

    #[test]
    fn no_dnd_when_missing_bounds() {
        assert!(!is_in_dnd(local_ts(12, 0), None, None));           // 本地 12:00,无 dnd
        assert!(!is_in_dnd(local_ts(12, 0), Some("09:00"), None));  // 缺一端
    }
    #[test]
    fn normal_range_inclusive_start_exclusive_end() {
        assert!(is_in_dnd(local_ts(12, 0), Some("09:00"), Some("17:00")));   // 本地 12:00 in
        assert!(!is_in_dnd(local_ts(8, 0), Some("09:00"), Some("17:00")));   // 本地 08:00 out
        assert!(!is_in_dnd(local_ts(17, 0), Some("09:00"), Some("17:00")));  // 本地 17:00 out(不含)
    }
    #[test]
    fn overnight_range() {
        assert!(is_in_dnd(local_ts(22, 0), Some("22:00"), Some("07:00")));   // 本地 22:00 in
        assert!(is_in_dnd(local_ts(3, 0), Some("22:00"), Some("07:00")));    // 本地 03:00 in
        assert!(!is_in_dnd(local_ts(10, 0), Some("22:00"), Some("07:00")));  // 本地 10:00 out
    }
    #[test]
    fn invalid_format_is_not_dnd() {
        assert!(!is_in_dnd(local_ts(12, 0), Some("bad"), Some("17:00")));
    }
}
