//! backend/supervisor — Agent Hub 后台 owner 崩溃监督与退避重启。
//!
//! Business Logic（为什么需要这个模块）:
//!     登录自启动后的 Agent Hub backend 可能异常退出；用户期望同一登录会话内自动恢复，
//!     且正常 stop（exit 0）不再拉起。监督循环必须可单测，不能依赖真实 sleep。
//!
//! Code Logic（这个模块做什么）:
//!     提供可注入 `ChildRunner` / `Sleeper` 的监督循环：非零退出按 1/2/4/8/16/32/60s
//!     指数退避重启；存活满 10 分钟后下一次失败延迟重置为 1s；exit 0 结束监督。
//!     CLI `supervise` 以当前可执行文件直接 spawn `serve`（无 shell），转发
//!     `CC_PARTNER_DATA_DIR`，永不打印 control token。

use crate::error::AppError;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// 存活满该时长后，下一次失败的退避重置为 1 秒。
pub const HEALTHY_RESET_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// 退避序列：1、2、4、8、16、32、60 秒上限。
const BACKOFF_SECONDS: &[u64] = &[1, 2, 4, 8, 16, 32, 60];

/// 可注入的睡眠抽象，单元测试不 sleep 真实时间。
///
/// Business Logic（为什么需要这个 trait）:
///     监督退避测试必须断言 sleep 时长，不能在 CI 里真等分钟级。
///
/// Code Logic（这个 trait 做什么）:
///     生产用 `RealSleeper`；测试用记录式 fake。
pub trait Sleeper {
    /// 睡眠指定时长。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     非零退出后需要按退避表等待再重启。
    ///
    /// Code Logic（这个函数做什么）:
    ///     阻塞当前线程至少 `duration`；测试实现只记录请求。
    fn sleep(&mut self, duration: Duration);
}

/// 真实 `std::thread::sleep` 实现。
///
/// Business Logic（为什么需要这个结构）:
///     生产 `supervise` 命令需要真正等待退避间隔。
///
/// Code Logic（这个结构做什么）:
///     空结构体，直接调用 `std::thread::sleep`。
#[derive(Debug, Default)]
pub struct RealSleeper;

impl Sleeper for RealSleeper {
    /// 真实线程睡眠。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产监督循环必须按退避表等待，避免崩溃风暴。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `std::thread::sleep(duration)`。
    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// 可注入的 serve 子进程运行器。
///
/// Business Logic（为什么需要这个 trait）:
///     单测需要伪造 exit code 与存活时长，而不真正启动 backend。
///
/// Code Logic（这个 trait 做什么）:
///     `run_once` 同步运行一次 serve 尝试并返回退出码与存活时长。
pub trait ChildRunner {
    /// 运行一次 serve 子进程并等待退出。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     监督循环每次失败后要重启一次 owner，成功退出则结束会话监督。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `(exit_code, alive_duration)`；spawn/wait 失败映射为 AppError。
    fn run_once(&mut self) -> Result<(i32, Duration), AppError>;
}

/// 根据连续失败次数（从 0 起）返回下一次退避。
///
/// Business Logic（为什么需要这个函数）:
///     崩溃风暴需指数退避并封顶 60s，避免占满 CPU/日志。
///
/// Code Logic（这个函数做什么）:
///     `failure_index` 映射到 BACKOFF_SECONDS，超出取最后一项 60s。
pub fn next_backoff(failure_index: u32) -> Duration {
    let idx = failure_index as usize;
    let secs = if idx < BACKOFF_SECONDS.len() {
        BACKOFF_SECONDS[idx]
    } else {
        *BACKOFF_SECONDS.last().unwrap_or(&60)
    };
    Duration::from_secs(secs)
}

/// 运行监督循环直到子进程 exit 0 或 runner 报错。
///
/// Business Logic（为什么需要这个函数）:
///     登录会话内 Agent Hub owner 崩溃应自动恢复；用户正常 stop（exit 0）则结束监督。
///
/// Code Logic（这个函数做什么）:
///     循环 `run_once`：exit 0 返回 Ok；非零则 sleep 退避后重试。
///     若某次存活 ≥ `HEALTHY_RESET_THRESHOLD`，下一次失败的 `failure_index` 重置为 0（1s）。
///     不打印 control token 或子进程敏感输出。
pub fn run_supervision_loop<R, S>(runner: &mut R, sleeper: &mut S) -> Result<(), AppError>
where
    R: ChildRunner,
    S: Sleeper,
{
    let mut failure_index: u32 = 0;
    loop {
        let (code, alive) = runner.run_once()?;
        if code == 0 {
            return Ok(());
        }
        // 存活足够久视为健康：下一次失败从 1s 重新退避。
        if alive >= HEALTHY_RESET_THRESHOLD {
            failure_index = 0;
        }
        let delay = next_backoff(failure_index);
        sleeper.sleep(delay);
        failure_index = failure_index.saturating_add(1);
    }
}

/// 生产用：以当前可执行文件直接 spawn `serve` 的 runner。
///
/// Business Logic（为什么需要这个结构）:
///     CLI `supervise` 必须无 shell 地启动 headless owner，并转发隔离数据目录。
///
/// Code Logic（这个结构做什么）:
///     缓存可执行路径；每次 `run_once` spawn `exe serve`，stdio 置 null，继承 data dir。
#[derive(Debug, Clone)]
pub struct ServeChildRunner {
    /// 当前 backend 可执行文件路径。
    executable: PathBuf,
}

impl ServeChildRunner {
    /// 使用当前进程可执行文件构造 runner。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产入口需要锁定本二进制的 `serve` 子命令，而不是 PATH 上的同名命令。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `std::env::current_exe()`；失败映射 IO 错误。
    pub fn from_current_exe() -> Result<Self, AppError> {
        let executable = std::env::current_exe().map_err(AppError::from)?;
        Ok(Self { executable })
    }

    /// 测试/注入：指定可执行路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测可指向假二进制或既有 backend 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接保存 PathBuf。
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }
}

impl ChildRunner for ServeChildRunner {
    /// spawn `exe serve` 并等待退出。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     监督器每次重启必须直接启动 serve，不经 shell，避免注入与 token 泄漏。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Command::new(exe).arg("serve")，stdin/out/err=null，转发 CC_PARTNER_DATA_DIR；
    ///     记录 spawn Instant 到 wait 完成的时长；退出码未知时按 1 处理。
    fn run_once(&mut self) -> Result<(i32, Duration), AppError> {
        let mut command = Command::new(&self.executable);
        command
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        inherit_data_dir_env(&mut command);
        let started = Instant::now();
        let mut child = command.spawn().map_err(AppError::from)?;
        let status = child.wait().map_err(AppError::from)?;
        let alive = started.elapsed();
        Ok((exit_code_of(status), alive))
    }
}

/// 将 `CC_PARTNER_DATA_DIR` 显式写入子进程环境。
///
/// Business Logic（为什么需要这个函数）:
///     监督器 spawn 的 serve 必须与父进程共用同一隔离数据根，否则 control/db 会写回真实 home。
///
/// Code Logic（这个函数做什么）:
///     若当前进程设置了该变量则 `command.env`；未设置不改动。
fn inherit_data_dir_env(command: &mut Command) {
    if let Some(value) = std::env::var_os("CC_PARTNER_DATA_DIR") {
        command.env("CC_PARTNER_DATA_DIR", value);
    }
}

/// 从 ExitStatus 提取 i32 退出码；信号终止等未知情况按 1。
///
/// Business Logic（为什么需要这个函数）:
///     监督循环只关心零/非零；被信号杀掉应视为失败并退避重启。
///
/// Code Logic（这个函数做什么）:
///     `status.code().unwrap_or(1)`。
fn exit_code_of(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// 生产入口：监督当前可执行文件的 `serve` 直至 exit 0。
///
/// Business Logic（为什么需要这个函数）:
///     `cc-partner-backend supervise` 是登录自启动的稳定入口；崩溃后自动恢复。
///
/// Code Logic（这个函数做什么）:
///     `ServeChildRunner::from_current_exe` + `RealSleeper` + `run_supervision_loop`。
pub fn supervise() -> Result<(), AppError> {
    let mut runner = ServeChildRunner::from_current_exe()?;
    let mut sleeper = RealSleeper;
    run_supervision_loop(&mut runner, &mut sleeper)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 记录 sleep 请求的假 sleeper。
    struct RecordingSleeper {
        sleeps: Vec<Duration>,
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&mut self, duration: Duration) {
            self.sleeps.push(duration);
        }
    }

    /// 按预设序列返回 exit code 与存活时长的假 runner。
    struct ScriptedRunner {
        outcomes: VecDeque<(i32, Duration)>,
    }

    impl ChildRunner for ScriptedRunner {
        fn run_once(&mut self) -> Result<(i32, Duration), AppError> {
            self.outcomes
                .pop_front()
                .ok_or_else(|| AppError::generic("ScriptedRunner: 预设结果已耗尽"))
        }
    }

    /// 失败三次再成功：退避 1s/2s/4s，exit 0 后停止。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     监督契约要求指数退避并在正常退出后结束，不能无限重启。
    ///
    /// Code Logic（这个测试做什么）:
    ///     outcomes=[1,1,1,0]；断言 sleeps=[1s,2s,4s] 且循环 Ok 返回。
    #[test]
    fn backoff_delays_then_stop_on_exit_zero() {
        let mut runner = ScriptedRunner {
            outcomes: VecDeque::from([
                (1, Duration::from_secs(1)),
                (1, Duration::from_secs(1)),
                (1, Duration::from_secs(1)),
                (0, Duration::from_secs(1)),
            ]),
        };
        let mut sleeper = RecordingSleeper { sleeps: vec![] };
        run_supervision_loop(&mut runner, &mut sleeper).unwrap();
        assert_eq!(
            sleeper.sleeps,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ]
        );
    }

    /// 存活满 10 分钟后失败，下一次失败退避重置为 1s。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     长期健康运行后的偶发崩溃不应沿用高退避，应重新从 1s 开始。
    ///
    /// Code Logic（这个测试做什么）:
    ///     第一次失败（短存活）sleep 1s；第二次成功存活 10min 后失败 → 因 healthy reset，
    ///     下一次 sleep 仍为 1s（而非 2s）；再 exit 0。
    #[test]
    fn healthy_run_resets_next_failure_delay_to_one_second() {
        let ten_min = HEALTHY_RESET_THRESHOLD;
        let mut runner = ScriptedRunner {
            outcomes: VecDeque::from([
                // 第一次失败：短存活 → failure_index 0 → sleep 1s，然后 index=1
                (1, Duration::from_secs(5)),
                // 第二次：长存活后失败 → 进入循环时先 reset index=0 → sleep 1s
                (1, ten_min),
                (0, Duration::from_secs(1)),
            ]),
        };
        let mut sleeper = RecordingSleeper { sleeps: vec![] };
        run_supervision_loop(&mut runner, &mut sleeper).unwrap();
        assert_eq!(
            sleeper.sleeps,
            vec![Duration::from_secs(1), Duration::from_secs(1)]
        );
    }

    /// 退避表封顶 60s。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     连续崩溃不得无限加倍等待；60s 是产品上限。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 next_backoff(0..7) 序列与 1,2,4,8,16,32,60,60。
    #[test]
    fn backoff_sequence_caps_at_sixty_seconds() {
        let expected = [1u64, 2, 4, 8, 16, 32, 60, 60];
        for (i, secs) in expected.iter().enumerate() {
            assert_eq!(next_backoff(i as u32), Duration::from_secs(*secs));
        }
    }
}
