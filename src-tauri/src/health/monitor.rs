//! 键鼠活动采样:trait ActivitySampler + device_query 跨平台真实实现。
//!
//! 每次采样对比上次鼠标坐标/按键数,得出「本分钟是否活跃」;活跃时取活动窗口标题/进程名。
//! 采样器抽象便于单元测试(Task 6 daemon 注入 MockSampler 即可驱动状态机,无需真实键鼠输入)。

use device_query::{DeviceQuery, DeviceState};

/// 单分钟活动采样结果。
///
/// 由采样器在每分钟 tick 时产出,喂给工作/休息状态机与提醒逻辑。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivitySample {
    /// 本分钟是否有键鼠活动(鼠标移动或按键)。
    pub is_active: bool,
    /// 活动时所处窗口的进程名(活跃时才查询;无活动或查询失败为 None)。
    pub process_name: Option<String>,
    /// 活动时所处窗口的标题(活跃时才查询;无活动或查询失败为 None)。
    pub window_title: Option<String>,
}

/// 活动采样器抽象。
///
/// 不加 Send bound:真实采样器持有非 Send 的 `DeviceState`,仅在采样线程内使用,
/// 不跨线程传递(Task 6 daemon 在专用线程持有采样器并轮询调用)。
pub trait ActivitySampler {
    /// 采样一次,返回当前分钟的活动结果。
    fn sample(&mut self) -> ActivitySample;
}

/// Mock 采样器(测试用):按预设 `seq` 序列循环返回,索引越界回退为 inactive。
///
/// 用于驱动状态机单测与 daemon 集成测试,避免依赖真实键鼠输入。
pub struct MockSampler {
    /// 预设的活跃序列;`sample()` 依次返回每个值,越界后恒为 false。
    pub seq: Vec<bool>,
    /// 当前消费到的序列下标。
    pub idx: usize,
}

impl MockSampler {
    /// Business Logic:测试需要一个可精确控的活动源,以验证状态机在「活跃/不活跃」不同组合下的推进。
    /// Code Logic:传入布尔序列构造,记录初始下标 0。
    pub fn new(seq: Vec<bool>) -> Self {
        Self { seq, idx: 0 }
    }
}

impl ActivitySampler for MockSampler {
    fn sample(&mut self) -> ActivitySample {
        let active = self.seq.get(self.idx).copied().unwrap_or(false);
        self.idx += 1;
        ActivitySample {
            is_active: active,
            process_name: None,
            window_title: None,
        }
    }
}

/// device_query 轮询采样器。
///
/// 维护上次鼠标坐标与按键数,每次采样比较得出是否活跃;活跃时同步查询活动窗口信息。
/// 真实采样器不参与单测(依赖系统键鼠与窗口管理器),仅保证编译通过。
pub struct DeviceQuerySampler {
    /// 上次采样的鼠标坐标(首次采样视为「无基线」→ 默认活跃)。
    last_mouse: Option<(i64, i64)>,
    /// 上次采样的按键数,用于检测按键数变化(按下/释放)。
    last_key_count: usize,
    /// device_query 设备状态句柄(非 Send,仅采样线程内持有)。
    state: DeviceState,
}

impl DeviceQuerySampler {
    /// Business Logic:Task 6 daemon 需要一个能查询真实键鼠状态的采样器实例。
    /// Code Logic:初始化无基线坐标、零按键数,并创建 DeviceState(首次采样必判为活跃)。
    pub fn new() -> Self {
        Self {
            last_mouse: None,
            last_key_count: 0,
            state: DeviceState::new(),
        }
    }
}

impl Default for DeviceQuerySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivitySampler for DeviceQuerySampler {
    fn sample(&mut self) -> ActivitySample {
        let mouse = self.state.get_mouse();
        let keys = self.state.get_keys();
        // device_query MouseState.coords 为 (i32, i32),用 as i64 兼容 i32/i64 两种坐标类型。
        let coords = (mouse.coords.0 as i64, mouse.coords.1 as i64);
        // 首次采样无基线,默认视为活跃(捕捉到设备即认为用户在场)。
        let moved = self
            .last_mouse
            .map_or(true, |(x, y)| coords.0 != x || coords.1 != y);
        let key_count = keys.len();
        // 按键活动:当前有键按下,或按键数相对上次有变化(松开也算活动)。
        let key_activity = key_count > 0 || key_count != self.last_key_count;
        self.last_mouse = Some(coords);
        self.last_key_count = key_count;
        let is_active = moved || key_activity;
        // 仅在活跃时查询活动窗口,减少无活动时的系统调用开销。
        let (process_name, window_title) = if is_active {
            active_window_info()
        } else {
            (None, None)
        };
        ActivitySample {
            is_active,
            process_name,
            window_title,
        }
    }
}

/// 取当前活动窗口的进程名/标题(active-win-pos-rs)。
///
/// Business Logic:用户活跃时记录「在哪个应用/窗口工作」,供久坐提醒上下文展示。
/// Code Logic:调用 `active_win_pos_rs::get_active_window()`,成功返回 `(app_name, title)`,
/// 失败返回 `(None, None)` 不阻断采样(窗口查询是非关键路径)。
fn active_window_info() -> (Option<String>, Option<String>) {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => (Some(w.app_name), Some(w.title)),
        Err(_) => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_sampler_cycles_sequence() {
        let mut m = MockSampler::new(vec![true, false, true]);
        assert!(m.sample().is_active);
        assert!(!m.sample().is_active);
        assert!(m.sample().is_active);
        assert!(!m.sample().is_active); // 越界回退
    }

    #[test]
    fn mock_sampler_default_inactive_when_empty() {
        let mut m = MockSampler::new(vec![]);
        let s = m.sample();
        assert!(!s.is_active);
        assert!(s.process_name.is_none());
        assert!(s.window_title.is_none());
    }
}
