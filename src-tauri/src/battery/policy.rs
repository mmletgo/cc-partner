//! 充电额度与扣时纯策略（无 IO）。
//!
//! Business Logic（为什么需要这个模块）:
//!     入账分钟、日上限、余额钳制和多窗 1× 扣时必须可单测，不能绑 SQLite。
//!
//! Code Logic（这个模块做什么）:
//!     给定配置与当日已用次数，计算实际 credit/debit 毫秒。

use crate::config::{BatteryConfig, BatteryCreditSource};

/// 一分钟的毫秒数。
pub const MS_PER_MINUTE: i64 = 60_000;

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

/// 计算一次结算应扣除的毫秒。
///
/// Business Logic（为什么需要这个函数）:
///     充电模式且至少一扇消耗窗时按墙钟扣时；无限模式、无消耗窗或余额为 0 不扣。
///     多窗同时消耗仍只按一份墙钟，调用方不得把 elapsed 乘窗数。
///
/// Code Logic（这个函数做什么）:
///     charging && consuming && remaining>0 时取 min(elapsed, remaining)，否则 0。
pub fn debit_delta_ms(charging: bool, any_consuming_window: bool, remaining_ms: i64, elapsed_ms: i64) -> i64 {
    if !charging || !any_consuming_window || remaining_ms <= 0 || elapsed_ms <= 0 {
        return 0;
    }
    elapsed_ms.min(remaining_ms)
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
}
