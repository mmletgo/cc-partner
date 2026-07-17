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
#[allow(dead_code)]
pub struct MockSampler {
    /// 预设的活跃序列;`sample()` 依次返回每个值,越界后恒为 false。
    pub seq: Vec<bool>,
    /// 当前消费到的序列下标。
    pub idx: usize,
}

impl MockSampler {
    /// Business Logic:测试需要一个可精确控的活动源,以验证状态机在「活跃/不活跃」不同组合下的推进。
    /// Code Logic:传入布尔序列构造,记录初始下标 0。
    #[allow(dead_code)]
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
///
/// `state` 为 `Option` 以支持权限降级:macOS 缺辅助功能权限或 Linux 缺 X display 时,
/// `DeviceState` 构建会失败,此时 sampler 降级为恒 inactive,避免采样线程 panic。
pub struct DeviceQuerySampler {
    /// 上次采样的鼠标坐标(首次采样视为「无基线」→ 默认活跃)。
    last_mouse: Option<(i64, i64)>,
    /// 上次采样的按键数,用于检测按键数变化(按下/释放)。
    last_key_count: usize,
    /// device_query 设备状态句柄(非 Send,仅采样线程内持有)。None 表示权限/环境不可用,降级采样。
    state: Option<DeviceState>,
}

impl DeviceQuerySampler {
    /// Business Logic:Task 6 daemon 需要一个能查询真实键鼠状态的采样器实例;
    /// 但 macOS 缺辅助功能权限、Linux 缺 X display 时构建会 panic,必须降级而非崩溃。
    ///
    /// Code Logic:初始化无基线坐标、零按键数,并通过 `try_build_device_state()` 安全构建
    /// DeviceState;成功存入 `state`,失败(权限/环境不可用)存 None 进入降级模式。
    pub fn new() -> Self {
        let state = try_build_device_state();
        if state.is_none() {
            tracing::warn!(
                "健康监测采样器降级:无法构建 DeviceState(macOS 缺辅助功能权限或 Linux 缺 X display),采样将恒为 inactive"
            );
        }
        Self {
            last_mouse: None,
            last_key_count: 0,
            state,
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
        // 降级模式:无 DeviceState 时恒返回 inactive,不查询键鼠/窗口。
        let Some(state) = self.state.as_ref() else {
            return ActivitySample::default();
        };
        let mouse = state.get_mouse();
        let keys = state.get_keys();
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

/// 安全构建 DeviceState,权限/环境不可用时返回 None 而非 panic。
///
/// Business Logic(为什么需要这个函数):
///     `device_query` 在 macOS 上通过 `application_is_trusted_with_prompt()` 检测辅助功能，
///     **会自动弹出系统授权对话框**。进入 Welcome 仅检查权限时也不应弹框——弹框只能由用户
///     点击「去设置」触发。缺权限/环境时采样线程须降级而非崩溃或弹窗。
///
/// Code Logic(这个函数做什么):
///     macOS：先用本进程 `AXIsProcessTrusted`（无 prompt）预检；未授权直接 None，
///     **永不**调用 `DeviceState::checked_new/new`（二者内部 with_prompt）。已授权再
///     `checked_new`（此时 with_prompt 不会弹框）。其它平台 catch_unwind 包裹 `new()`。
fn try_build_device_state() -> Option<DeviceState> {
    #[cfg(target_os = "macos")]
    {
        // 禁止走 device_query 的 with_prompt 路径，否则 app 启动/进入 Welcome 即弹辅助功能授权框。
        if !crate::permissions::check_accessibility_access() {
            return None;
        }
        DeviceState::checked_new()
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux/Windows 无 checked_new;用 catch_unwind 兜底,DeviceState 用 AssertUnwindSafe 标记。
        use std::panic::{self, AssertUnwindSafe};
        let result = panic::catch_unwind(AssertUnwindSafe(DeviceState::new));
        result.ok()
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

    /// 验证降级模式(DeviceState 构建失败)时采样恒为 inactive,不 panic。
    ///
    /// Business Logic(为什么需要这个测试):
    ///     macOS 缺辅助功能权限或 Linux 缺 X display 时,采样器必须优雅降级而非崩溃;
    ///     降级后每分钟采样应恒为 inactive,让状态机正确判定用户不在场。
    ///
    /// Code Logic(这个测试做什么):
    ///     用 test-only 构造器创建 state=None 的降级 sampler,连续采样多次,断言全部 inactive 且无窗口信息。
    #[test]
    fn device_query_sampler_degrades_to_inactive_when_unavailable() {
        let mut sampler = DeviceQuerySampler::degraded_for_test();
        for _ in 0..3 {
            let sample = sampler.sample();
            assert!(!sample.is_active, "降级采样必须恒为 inactive");
            assert!(sample.process_name.is_none());
            assert!(sample.window_title.is_none());
        }
    }

    impl DeviceQuerySampler {
        /// 测试专用:构造无 DeviceState 的降级采样器,模拟权限/环境不可用。
        #[cfg(test)]
        fn degraded_for_test() -> Self {
            Self {
                last_mouse: None,
                last_key_count: 0,
                state: None,
            }
        }
    }
}
