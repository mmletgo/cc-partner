//! 喝水提醒计时状态。pending_remind 防止未响应前每 tick 重复 emit。

/// 喝水运行时状态(放 HealthRuntime,跨 daemon 与命令共享)。
///
/// Business Logic（为什么需要这个结构）:
///     久坐办公的用户容易忘记喝水,系统需每隔固定间隔提醒一次。为避免每分钟采样都重复
///     提醒直到用户响应,用 `pending_remind` 标记「已有未响应提醒」,用户点击喝水按钮
///     (`record_water` 命令)后才清零并重置计时。该状态跨 daemon 采样 task 与命令层共享。
///
/// Code Logic（这个结构做什么）:
///     纯数据载体:`last_drink_ts` 记录上次喝水秒级时间戳,`pending_remind` 标记是否有未
///     响应的喝水提醒。`new` 用当前时间初始化(开机即视为刚喝过,首个间隔后才提醒)。
pub struct WaterState {
    /// 上次喝水(或开机初始化)的秒级 unix 时间戳。
    pub last_drink_ts: i64,
    /// 是否已有未响应的喝水提醒(置 true 后,在用户 `record_water` 前不再重复 emit)。
    pub pending_remind: bool,
}
impl WaterState {
    /// Business Logic: 应用启动时需要一个初始喝水状态——视作「刚喝过水」,
    ///                  这样开机后需等待一个完整间隔才触发首次提醒,而非开机即提醒。
    /// Code Logic: 以传入的 `now_ts` 作 last_drink_ts,pending_remind 置 false。
    pub fn new(now_ts: i64) -> Self {
        Self {
            last_drink_ts: now_ts,
            pending_remind: false,
        }
    }
}

/// 是否该提醒喝水:启用 + 超过间隔 + 无未响应提醒。
///
/// Business Logic: 仅当喝水提醒开启、距上次喝水已达设定间隔、且当前没有未响应的
///                  喝水提醒时,才应触发新提醒(三者缺一不可,避免打扰/重复)。
/// Code Logic: 纯函数判定 `enabled && !pending && (now - last_drink) >= interval`。
pub fn should_remind_water(state: &WaterState, now_ts: i64, enabled: bool, interval: i64) -> bool {
    enabled && !state.pending_remind && (now_ts - state.last_drink_ts) >= interval
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remind_after_interval_when_enabled() {
        let s = WaterState::new(0);
        assert!(!should_remind_water(&s, 100, true, 3600)); // 未到间隔
        assert!(should_remind_water(&s, 3600, true, 3600)); // 到间隔
    }
    #[test]
    fn no_remind_when_pending() {
        let mut s = WaterState::new(0);
        s.pending_remind = true;
        assert!(!should_remind_water(&s, 99999, true, 3600));
    }
    #[test]
    fn no_remind_when_disabled() {
        let s = WaterState::new(0);
        assert!(!should_remind_water(&s, 99999, false, 3600));
    }
}
