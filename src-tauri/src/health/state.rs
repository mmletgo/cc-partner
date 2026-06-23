//! 工作/休息状态机:每分钟喂入「是否活跃」推进状态,输出当前状态 + 是否应触发久坐提醒。
//!
//! 核心概念:
//! - 工作窗口:从首次键鼠活动起的连续工作时段
//! - 有效休息:连续无操作 ≥ break_seconds 才中断工作窗口;短暂停歇不中断

/// 状态机判定阈值(从 HealthConfig 投影而来,解耦配置结构)。
#[derive(Debug, Clone, Copy)]
pub struct HealthThresholds {
    /// 单个工作窗口的持续时长上限(秒);窗口自然时长达此值即触发久坐提醒。
    pub work_window_seconds: i64,
    /// 判定为「有效休息」所需的连续无操作秒数;不足则视为短暂停歇,不中断工作窗口。
    pub break_seconds: i64,
}

/// 工作窗口运行时状态。
#[derive(Debug, Clone, PartialEq)]
pub struct WorkingState {
    /// 当前工作窗口的起始时间戳(秒)。
    pub window_start_ts: i64,
    /// 最近一次键鼠活动的时间戳(秒);用于判断停歇是否已达有效休息阈值。
    pub last_active_ts: i64,
    /// 本窗口是否已触发过久坐提醒;同窗口不重复提醒。
    pub reminded: bool,
}

/// 状态机当前相位。
#[derive(Debug, Clone, PartialEq)]
pub enum MachineState {
    /// 初始空闲态:尚无任何键鼠活动。
    Idle,
    /// 工作态:正处于一个工作窗口中。
    Working(WorkingState),
    /// 休息态:连续无操作已达 break_seconds,工作窗口已关闭。
    Resting {
        /// 休息开始时间戳(秒)。
        rest_start_ts: i64,
    },
}

/// 一次推进的输出。
///
/// `state`/`reminder_closed_window` 当前 daemon 仅消费 `should_remind`；相位与被关闭窗口
/// 供后续统计/前端展示扩展使用，故整体 `#[allow(dead_code)]`。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StateOutcome {
    /// 推进后的状态机相位。
    pub state: MachineState,
    /// 转入 Resting 时,被关闭的工作窗口 (start_ts, end_ts),供入库统计;否则 None。
    pub reminder_closed_window: Option<(i64, i64)>,
    /// 本次推进是否应触发久坐提醒(窗口自然时长达标且本窗口未提醒过)。
    pub should_remind: bool,
}

/// 工作/休息状态机。无 IO、无时钟依赖,由外部每分钟喂入 (active, now_ts) 推进。
pub struct HealthStateMachine {
    /// 当前相位。
    pub state: MachineState,
}

impl HealthStateMachine {
    /// 构造一个处于 Idle 初态的状态机。
    pub fn new() -> Self {
        Self {
            state: MachineState::Idle,
        }
    }

    /// 推进一拍:根据本分钟是否有键鼠活动更新相位,并判定是否触发久坐提醒。
    ///
    /// Business Logic（为什么需要这个方法）:
    ///     久坐提醒依赖「连续工作时长」而非单次活动。daemon 每分钟采样键鼠活跃度,
    ///     把 (active, now_ts) 喂入状态机,由状态机维护工作窗口的起止、停歇累积、
    ///     提醒去重,从而把「是否久坐」这一时序判断从采样/时钟逻辑中彻底解耦——
    ///     采样层只负责上报,判定逻辑集中在此纯函数,便于单测。
    ///
    /// Code Logic（这个方法做什么）:
    ///     1) 相位流转:Idle/Resting + 活跃 → 开新工作窗口;Working + 活跃 → 续 last_active_ts;
    ///        Working + 停歇且 last_active 至今 ≥ break_seconds → 关闭窗口入 Resting(报告被关闭窗口);
    ///        其余(空闲态停歇 / 工作态短暂停歇)→ 保持原相位。
    ///     2) 提醒判定:仅在 Working 态,窗口自然时长(now_ts - window_start_ts)≥ work_window_seconds
    ///        且本窗口未提醒过 → 置 should_remind 并标记 reminded,同窗口不重复触发。
    ///     3) 写回 self.state 并返回 StateOutcome。
    pub fn advance(&mut self, active: bool, now_ts: i64, cfg: &HealthThresholds) -> StateOutcome {
        let mut closed_window: Option<(i64, i64)> = None;

        // 1) 相位流转
        let next = match (&self.state, active) {
            (MachineState::Idle, true) | (MachineState::Resting { .. }, true) => {
                MachineState::Working(WorkingState {
                    window_start_ts: now_ts,
                    last_active_ts: now_ts,
                    reminded: false,
                })
            }
            (MachineState::Working(w), true) => MachineState::Working(WorkingState {
                last_active_ts: now_ts,
                ..w.clone()
            }),
            (MachineState::Idle, false) | (MachineState::Resting { .. }, false) => {
                self.state.clone()
            }
            (MachineState::Working(w), false) => {
                if now_ts - w.last_active_ts >= cfg.break_seconds {
                    closed_window = Some((w.window_start_ts, now_ts));
                    MachineState::Resting {
                        rest_start_ts: now_ts,
                    }
                } else {
                    self.state.clone() // 短暂停歇,保持 Working
                }
            }
        };

        // 2) 提醒判定(仅 Working 态,窗口自然时长达标且本窗口未提醒过)
        let mut should_remind = false;
        let final_state = if let MachineState::Working(w) = &next {
            if !w.reminded && now_ts - w.window_start_ts >= cfg.work_window_seconds {
                should_remind = true;
                MachineState::Working(WorkingState {
                    reminded: true,
                    ..w.clone()
                })
            } else {
                next.clone()
            }
        } else {
            next
        };

        self.state = final_state.clone();
        StateOutcome {
            state: final_state,
            reminder_closed_window: closed_window,
            should_remind,
        }
    }
}

impl Default for HealthStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thr() -> HealthThresholds {
        HealthThresholds {
            work_window_seconds: 45 * 60,
            break_seconds: 5 * 60,
        }
    }

    #[test]
    fn idle_to_working_on_first_activity() {
        let mut m = HealthStateMachine::new();
        let out = m.advance(true, 1000, &thr());
        assert!(matches!(out.state, MachineState::Working(_)));
        assert!(!out.should_remind);
    }

    #[test]
    fn short_pause_does_not_break_window() {
        let mut m = HealthStateMachine::new();
        m.advance(true, 1000, &thr()); // 开始工作
        m.advance(true, 1000 + 60, &thr()); // 1 分钟后活跃
                                            // 停 3 分钟(< break 5 分钟):应仍 Working
        let out = m.advance(false, 1000 + 60 + 180, &thr());
        assert!(matches!(out.state, MachineState::Working(_)));
        assert!(out.reminder_closed_window.is_none());
    }

    #[test]
    fn long_pause_closes_window_and_enters_resting() {
        let mut m = HealthStateMachine::new();
        m.advance(true, 1000, &thr());
        m.advance(true, 1060, &thr());
        // 停 5 分钟(>= break):应进 Resting 并关闭窗口
        let out = m.advance(false, 1060 + 300, &thr());
        assert!(matches!(out.state, MachineState::Resting { .. }));
        assert!(
            out.reminder_closed_window.is_some(),
            "应报告被关闭的工作窗口"
        );
    }

    #[test]
    fn resting_to_working_starts_new_window() {
        let mut m = HealthStateMachine::new();
        m.advance(true, 1000, &thr());
        m.advance(false, 1000 + 300, &thr()); // 进 Resting
        let out = m.advance(true, 1000 + 600, &thr()); // 重新活跃
        assert!(matches!(out.state, MachineState::Working(_)));
    }

    #[test]
    fn remind_when_window_exceeds_threshold_without_rest() {
        let mut m = HealthStateMachine::new();
        let t = HealthThresholds {
            work_window_seconds: 120,
            break_seconds: 300,
        };
        m.advance(true, 0, &t);
        // 连续活跃到窗口满 120s
        let out = m.advance(true, 120, &t);
        assert!(out.should_remind, "窗口满且未休息应触发提醒");
    }

    #[test]
    fn do_not_remind_twice_in_same_window() {
        let mut m = HealthStateMachine::new();
        let t = HealthThresholds {
            work_window_seconds: 120,
            break_seconds: 300,
        };
        m.advance(true, 0, &t);
        let _ = m.advance(true, 120, &t); // 已提醒
        let out = m.advance(true, 200, &t);
        assert!(!out.should_remind, "同窗口不重复提醒");
    }

    #[test]
    fn remind_again_after_rest_and_new_window() {
        let mut m = HealthStateMachine::new();
        let t = HealthThresholds {
            work_window_seconds: 120,
            break_seconds: 300,
        };
        m.advance(true, 0, &t);
        let _ = m.advance(true, 120, &t); // 提醒 1
        m.advance(false, 120 + 300, &t); // 有效休息
        let _ = m.advance(true, 120 + 600, &t); // 新窗口
        let out = m.advance(true, 120 + 600 + 120, &t); // 新窗口满
        assert!(out.should_remind, "新窗口应能再次提醒");
    }
}
