//! 充电额度与扣时纯策略（无 IO）。
//!
//! Business Logic（为什么需要这个模块）:
//!     入账分钟、日上限、余额钳制和多窗 1× 扣时必须可单测，不能绑 SQLite。
//!
//! Code Logic（这个模块做什么）:
//!     给定配置与当日已用次数，计算实际 credit/debit 毫秒。

use crate::config::{BatteryConfig, BatteryCreditSource};
use chrono::{Local, TimeZone};

/// 一分钟的毫秒数。
pub const MS_PER_MINUTE: i64 = 60_000;

/// 每日余额重置的本地时刻（早晨 8 点）。
pub const DAILY_RESET_HOUR: u32 = 8;

/// 计算一次入账应增加的毫秒。
///
/// Business Logic（为什么需要这个函数）:
///     健康完成 / 闪卡答对应按配置充入，但必须遵守日次数上限与余额上限。
///
/// Code Logic（这个函数做什么）:
///     已达日上限 → 0；否则取来源分钟 × 60_000，再钳到 `max_balance - remaining`。
pub fn credit_delta_ms(
    config: &BatteryConfig,
    source: BatteryCreditSource,
    today_count: i64,
    remaining_ms: i64,
) -> i64 {
    let cap = config.daily_cap(source);
    if today_count >= cap {
        return 0;
    }
    let reward = config.reward_minutes(source).saturating_mul(MS_PER_MINUTE);
    let max_ms = config.max_balance_minutes.saturating_mul(MS_PER_MINUTE);
    let room = (max_ms - remaining_ms.max(0)).max(0);
    reward.min(room)
}

/// 按显式分钟入账（插件游戏）。不看日次数上限。
///
/// Business Logic（为什么需要这个函数）:
///     插件完成奖励写在 game.json，且产品要求完全信任、无日上限；仍钳余额上限。
///
/// Code Logic（这个函数做什么）:
///     minutes.max(0) * 60_000，再钳到 max_balance - remaining。
pub fn credit_delta_ms_explicit(config: &BatteryConfig, remaining_ms: i64, minutes: i64) -> i64 {
    let reward = minutes.max(0).saturating_mul(MS_PER_MINUTE);
    let max_ms = config.max_balance_minutes.saturating_mul(MS_PER_MINUTE);
    let room = (max_ms - remaining_ms.max(0)).max(0);
    reward.min(room)
}

/// 计算一次结算应扣除的毫秒。
///
/// Business Logic（为什么需要这个函数）:
///     充电模式且至少一扇消耗窗时按墙钟扣时；无限模式、无消耗窗或余额为 0 不扣。
///     多窗同时消耗仍只按一份墙钟，调用方不得把 elapsed 乘窗数。
///
/// Code Logic（这个函数做什么）:
///     charging && consuming && remaining>0 时取 min(elapsed, remaining)，否则 0。
pub fn debit_delta_ms(
    charging: bool,
    any_consuming_window: bool,
    remaining_ms: i64,
    elapsed_ms: i64,
) -> i64 {
    if !charging || !any_consuming_window || remaining_ms <= 0 || elapsed_ms <= 0 {
        return 0;
    }
    elapsed_ms.min(remaining_ms)
}

/// 计算时区 `tz` 下、`now_ts` 之前最近一个已过去的每日重置时刻。
///
/// Business Logic（为什么需要这个函数）:
///     每日 8 点重置需要「当前属于哪个重置周期」的权威边界；应用可能错过 8 点，
///     判定必须对任意时刻成立而不是只在整点触发。
///
/// Code Logic（这个函数做什么）:
///     构造本地日期 `DAILY_RESET_HOUR:00`；`now >= 该时刻` 取之，否则取前一日；
///     本地化失败（极端 DST）返回 None，调用方按未到期处理。
pub fn daily_reset_boundary_in<Tz: TimeZone>(now_ts: i64, tz: &Tz) -> Option<i64> {
    let local = tz.timestamp_opt(now_ts, 0).single()?;
    let today = local.date_naive().and_hms_opt(DAILY_RESET_HOUR, 0, 0)?;
    let today_ts = tz.from_local_datetime(&today).single()?.timestamp();
    if now_ts >= today_ts {
        return Some(today_ts);
    }
    let yesterday = today - chrono::Duration::days(1);
    tz.from_local_datetime(&yesterday)
        .single()
        .map(|dt| dt.timestamp())
}

/// 判定每日重置是否到期；到期时返回应推进到的边界时刻。
///
/// Business Logic（为什么需要这个函数）:
///     重置幂等取决于「上次已重置到哪个边界」；`last_daily_reset_at` 落后于当前
///     周期边界即应重置一次（含从未重置过的 0），同周期内重复判定不得再触发。
///
/// Code Logic（这个函数做什么）:
///     `last_daily_reset_at < boundary(now)` → `Some(boundary)`；边界解析失败 → None（本轮跳过）。
pub fn evaluate_daily_reset(last_daily_reset_at: i64, now_ts: i64) -> Option<i64> {
    let boundary = daily_reset_boundary_in(now_ts, &Local)?;
    (last_daily_reset_at < boundary).then_some(boundary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BatteryConfig {
        BatteryConfig::default()
    }

    #[test]
    fn water_credit_uses_default_eight_minutes() {
        let delta = credit_delta_ms(&cfg(), BatteryCreditSource::Water, 0, 0);
        assert_eq!(delta, 8 * MS_PER_MINUTE);
    }

    #[test]
    fn rest_credit_uses_default_twenty_minutes() {
        let delta = credit_delta_ms(&cfg(), BatteryCreditSource::Rest, 0, 0);
        assert_eq!(delta, 20 * MS_PER_MINUTE);
    }

    #[test]
    fn kegel_and_custom_and_flashcard_defaults() {
        let c = cfg();
        assert_eq!(
            credit_delta_ms(&c, BatteryCreditSource::Kegel, 0, 0),
            10 * MS_PER_MINUTE
        );
        assert_eq!(
            credit_delta_ms(&c, BatteryCreditSource::Custom, 0, 0),
            10 * MS_PER_MINUTE
        );
        assert_eq!(
            credit_delta_ms(&c, BatteryCreditSource::Flashcard, 0, 0),
            3 * MS_PER_MINUTE
        );
    }

    #[test]
    fn daily_cap_blocks_further_water_credits() {
        let delta = credit_delta_ms(&cfg(), BatteryCreditSource::Water, 6, 0);
        assert_eq!(delta, 0);
    }

    #[test]
    fn daily_cap_allows_count_just_below() {
        let delta = credit_delta_ms(&cfg(), BatteryCreditSource::Water, 5, 0);
        assert_eq!(delta, 8 * MS_PER_MINUTE);
    }

    #[test]
    fn credit_clamps_to_max_balance() {
        let c = cfg();
        let almost_full = c.max_balance_minutes * MS_PER_MINUTE - MS_PER_MINUTE;
        let delta = credit_delta_ms(&c, BatteryCreditSource::Rest, 0, almost_full);
        assert_eq!(delta, MS_PER_MINUTE);
    }

    #[test]
    fn credit_is_zero_when_already_at_cap() {
        let c = cfg();
        let full = c.max_balance_minutes * MS_PER_MINUTE;
        assert_eq!(credit_delta_ms(&c, BatteryCreditSource::Water, 0, full), 0);
    }

    #[test]
    fn game_plugin_has_no_daily_cap_and_uses_explicit_minutes() {
        let c = cfg();
        let delta = credit_delta_ms_explicit(&c, 0, 5);
        assert_eq!(delta, 5 * MS_PER_MINUTE);
        let many_today = credit_delta_ms(&c, BatteryCreditSource::GamePlugin, 10_000, 0);
        assert_eq!(
            many_today, 0,
            "GamePlugin 不走 rewards 表，分钟数必须显式传入"
        );
    }

    #[test]
    fn game_plugin_clamps_to_max_balance() {
        let c = cfg();
        let full = c.max_balance_minutes * MS_PER_MINUTE;
        assert_eq!(credit_delta_ms_explicit(&c, full, 5), 0);
    }

    #[test]
    fn debit_only_when_charging_and_consuming() {
        assert_eq!(debit_delta_ms(true, true, 10_000, 1_000), 1_000);
        assert_eq!(debit_delta_ms(false, true, 10_000, 1_000), 0);
        assert_eq!(debit_delta_ms(true, false, 10_000, 1_000), 0);
        assert_eq!(debit_delta_ms(true, true, 0, 1_000), 0);
        assert_eq!(debit_delta_ms(true, true, 10_000, 0), 0);
    }

    #[test]
    fn debit_does_not_go_negative() {
        assert_eq!(debit_delta_ms(true, true, 400, 1_000), 400);
    }

    #[test]
    fn two_windows_still_use_one_elapsed() {
        // 调用方把 any_consuming_window=true 一次，不得把 elapsed 乘 2。
        assert_eq!(debit_delta_ms(true, true, 60_000, 1_000), 1_000);
    }

    /// 用固定 +08:00 偏移做确定性每日重置边界断言，避免测试依赖宿主时区。
    mod daily_reset {
        use super::super::*;
        use chrono::FixedOffset;

        fn cst() -> FixedOffset {
            FixedOffset::east_opt(8 * 3600).unwrap()
        }

        fn at(hour: u32, min: u32, sec: u32) -> i64 {
            cst()
                .with_ymd_and_hms(2026, 8, 14, hour, min, sec)
                .unwrap()
                .timestamp()
        }

        #[test]
        fn boundary_at_exactly_eight_is_today() {
            let boundary = daily_reset_boundary_in(at(8, 0, 0), &cst()).unwrap();
            assert_eq!(boundary, at(8, 0, 0));
        }

        #[test]
        fn boundary_after_eight_is_today() {
            let boundary = daily_reset_boundary_in(at(15, 30, 0), &cst()).unwrap();
            assert_eq!(boundary, at(8, 0, 0));
        }

        #[test]
        fn boundary_before_eight_is_yesterday() {
            let boundary = daily_reset_boundary_in(at(0, 30, 0), &cst()).unwrap();
            let yesterday_eight = cst()
                .with_ymd_and_hms(2026, 8, 13, 8, 0, 0)
                .unwrap()
                .timestamp();
            assert_eq!(boundary, yesterday_eight);
        }

        #[test]
        fn evaluate_due_when_last_reset_behind_boundary() {
            let boundary = daily_reset_boundary_in(at(15, 0, 0), &cst()).unwrap();
            assert_eq!(evaluate_daily_reset(0, at(15, 0, 0)), Some(boundary));
            assert_eq!(
                evaluate_daily_reset(boundary - 1, at(15, 0, 0)),
                Some(boundary)
            );
        }

        #[test]
        fn evaluate_not_due_within_same_period() {
            let boundary = daily_reset_boundary_in(at(15, 0, 0), &cst()).unwrap();
            // 当天 8 点后已重置过 → 同周期内不再触发。
            assert_eq!(evaluate_daily_reset(boundary, at(15, 0, 0)), None);
            assert_eq!(evaluate_daily_reset(boundary + 60, at(15, 1, 0)), None);
        }

        #[test]
        fn evaluate_before_eight_uses_yesterday_boundary() {
            let yesterday_eight = cst()
                .with_ymd_and_hms(2026, 8, 13, 8, 0, 0)
                .unwrap()
                .timestamp();
            // 昨天已重置 → 今晨 0:30 未到期；从未重置（0）→ 应补昨日边界。
            assert_eq!(evaluate_daily_reset(yesterday_eight, at(0, 30, 0)), None);
            assert_eq!(evaluate_daily_reset(0, at(0, 30, 0)), Some(yesterday_eight));
        }
    }
}
