//! workbench/dependencies.rs — 工作台运行时依赖管理
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台的可恢复终端、window 与 pane 能力依赖 tmux；后端需要统一检测、展示安装命令并管理安装任务状态。
//!
//! Code Logic（这个模块做什么）:
//!     提供 tmux 探测、版本解析、平台安装命令选择、DTO 序列化与安装状态机。

use crate::error::AppError;
use chrono::Utc;
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command as StdCommand, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
use tokio_util::sync::CancellationToken;

const OUTPUT_LINE_LIMIT: usize = 24;
/// 单次外部依赖探测（tmux -V / 包管理器 --version）端到端硬超时。
/// Business Logic: WSL/tmux hang 不得永久阻塞 AppState 初始化与 GUI 启动。
/// Code Logic: 覆盖 try_wait 轮询、kill、wait/reap 与 stdout/stderr drain 的共享截止时间。
const PROBE_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
/// try_wait 轮询间隔，兼顾响应速度与 CPU。
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// kill/wait 与管道排空的额外宽限（在共享截止时间之后仍允许的小额回收窗口）。
/// Business Logic: 截止后仍需 best-effort 回收僵尸，但不得无限阻塞 AppState 初始化。
const PROBE_TERMINATE_GRACE: Duration = Duration::from_millis(250);
/// 管道非阻塞 drain 的截止宽限（在共享 deadline 之后仍允许的小额读取窗口）。
const PROBE_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(150);
/// 探测管道 stdout+stderr 合计读取预算，防止持续写入的逃逸后代无限扩张缓冲。
/// Business Logic: 依赖探测只需版本行/少量错误输出；无限泵出会拖死 AppState 初始化。
/// Code Logic: pump 在累计字节达此上限后立刻标记该侧 done 并视为超时。
const PROBE_PIPE_DRAIN_BYTE_BUDGET: usize = 256 * 1024;

/// Doctor/依赖探测的共享取消与进程树登记。
///
/// Business Logic（为什么需要这个结构）:
///     doctor 硬超时后若只 forget 采集线程，git/tmux/wsl/claude 子进程可能继续跑；
///     必须把剩余 deadline 与取消信号传入 probe，并在返回前有界 reap 已登记进程树。
///
/// Code Logic（这个结构做什么）:
///     持有 AtomicBool 取消标志，以及已 spawn 的 Unix pgid / 子进程 pid 列表；
///     Windows 额外登记 Job Object（KILL_ON_JOB_CLOSE），覆盖根进程已退出后的后代树；
///     overall deadline 由调用方传入 probe。
#[derive(Debug, Default)]
pub struct ProbeRuntimeGuard {
    cancel: std::sync::atomic::AtomicBool,
    /// Unix 进程组 id 列表（探测 spawn 时登记）。
    process_groups: Mutex<Vec<u32>>,
    /// 直接子进程 pid 列表（全平台）。
    child_pids: Mutex<Vec<u32>>,
    /// Windows：pid → Job Object；超时 cancel_and_reap 可终止整棵进程树。
    #[cfg(windows)]
    jobs: Mutex<Vec<(u32, Arc<WindowsProbeJob>)>>,
}

impl ProbeRuntimeGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     采集线程与主线程共享同一 guard，超时侧可取消并 reap，探测侧可读剩余预算。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回默认未取消、空登记的 guard。
    pub fn new() -> Self {
        Self::default()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     硬超时或上层取消时必须立刻让后续 probe 快速失败并停止 spawn。
    ///
    /// Code Logic（这个函数做什么）:
    ///     置 cancel=true（Release）。
    pub fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     probe 在 spawn 前/轮询中需要知道是否应提前放弃。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 cancel 标志（Acquire）。
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     探测子进程启动后必须可被 doctor 超时路径定位并 kill。
    ///
    /// Code Logic（这个函数做什么）:
    ///     登记 pid；Unix 同时登记 process group id（与 setpgid 后的 pgid 相同）。
    pub fn register_child(&self, pid: u32) {
        if let Ok(mut pids) = self.child_pids.lock() {
            pids.push(pid);
        }
        #[cfg(unix)]
        if let Ok(mut groups) = self.process_groups.lock() {
            groups.push(pid);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Windows 上根进程可能先退出，仅登记 pid 后 taskkill /T 无法可靠枚举树；
    ///     必须把 Job Object 句柄交给 guard，硬超时路径才能 TerminateJobObject。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 pid 登记 Arc<WindowsProbeJob>；同一 pid 重复登记时覆盖。
    #[cfg(windows)]
    pub fn register_job(&self, pid: u32, job: Arc<WindowsProbeJob>) {
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(existing) = jobs.iter_mut().find(|(p, _)| *p == pid) {
                existing.1 = job;
            } else {
                jobs.push((pid, job));
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     子进程正常退出后不再需要超时路径 kill，避免误伤 pid 复用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 pid / pgid / job 列表移除该 id（保留其它仍在跑的探测）。
    pub fn unregister_child(&self, pid: u32) {
        if let Ok(mut pids) = self.child_pids.lock() {
            pids.retain(|value| *value != pid);
        }
        #[cfg(unix)]
        if let Ok(mut groups) = self.process_groups.lock() {
            groups.retain(|value| *value != pid);
        }
        #[cfg(windows)]
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.retain(|(value, _)| *value != pid);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     doctor 硬超时返回前必须 best-effort 终止仍在跑的依赖探测进程树。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 cancel；Windows 优先 TerminateJobObject（覆盖已退出根下的后代）；
    ///     再 Unix killpg + 全平台对登记 pid 发 kill（taskkill 作无 job 时的 fallback）；
    ///     短 sleep 作为有界 reap 窗口。
    pub fn cancel_and_reap(&self) {
        self.cancel();
        #[cfg(windows)]
        {
            if let Ok(mut jobs) = self.jobs.lock() {
                for (_pid, job) in jobs.drain(..) {
                    if let Err(err) = job.terminate() {
                        tracing::debug!(error = %err, "probe job TerminateJobObject 未完全成功");
                    }
                }
            }
        }
        #[cfg(unix)]
        {
            if let Ok(groups) = self.process_groups.lock() {
                for pgid in groups.iter().copied() {
                    let _ = kill_probe_process_group(pgid);
                }
            }
        }
        if let Ok(pids) = self.child_pids.lock() {
            for pid in pids.iter().copied() {
                let _ = kill_probe_pid(pid);
            }
        }
        // 有界 reap 窗口：给内核回收僵尸的时间，但不 join 采集线程。
        std::thread::sleep(PROBE_TERMINATE_GRACE);
    }
}

/// Business Logic（为什么需要这个函数）:
///     非 Unix 或需要按 pid 兜底 kill 时，不能只依赖 process group。
///
/// Code Logic（这个函数做什么）:
///     Unix 使用 libc::kill(SIGKILL)；Windows 使用有界 spawn/try_wait/kill/reap 的 taskkill；
///     其它平台 no-op。Windows 绝不能无界 `.status()`，否则 doctor hard deadline 后仍可挂死。
fn kill_probe_pid(pid: u32) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        let raw: libc::pid_t = pid.try_into().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid out of range")
        })?;
        let result = unsafe { libc::kill(raw, libc::SIGKILL) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        kill_probe_pid_windows(pid, PROBE_TERMINATE_GRACE)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor 超时后 cancel_and_reap 仍必须在 grace 内返回；Windows taskkill 被安全软件挂起时
///     不能用无界 `Command::status()` 突破 hard deadline。
///
/// Code Logic（这个函数做什么）:
///     spawn `taskkill /PID /T /F`，再委托有界 wait；超时 kill taskkill 自身。
///     `cfg(test)` 可通过 hook 注入假挂起命令。
#[cfg(windows)]
fn kill_probe_pid_windows(pid: u32, grace: Duration) -> Result<(), std::io::Error> {
    let mut child = spawn_windows_taskkill(pid)?;
    match wait_child_bounded(&mut child, grace) {
        Ok(code) if code.success() => Ok(()),
        Ok(code) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("taskkill exit {code}"),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::TimedOut => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("taskkill /PID {pid} 超过 grace {grace:?}"),
        )),
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor hard deadline 之后的回收命令本身也必须有界；否则 Windows taskkill hang
///     会把整次 doctor 卡死在“已超时”之后。
///
/// Code Logic（这个函数做什么）:
///     在 `grace` 内轮询 `try_wait`；到期 kill 子进程并 best-effort try_wait，返回 TimedOut。
///     生产路径仅 Windows taskkill 使用；单元测试在所有平台注入挂起 helper 校验有界性。
#[cfg(any(windows, test))]
fn wait_child_bounded(
    child: &mut Child,
    grace: Duration,
) -> Result<std::process::ExitStatus, std::io::Error> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.try_wait();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("子进程回收超过 grace {grace:?}"),
                    ));
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
            }
            Err(err) => return Err(err),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Windows 回收命令需要可测试的 spawn 入口，才能注入“taskkill 挂起”故障而不依赖真实系统工具。
///
/// Code Logic（这个函数做什么）:
///     默认 spawn `taskkill /PID <pid> /T /F` 并丢弃 stdout/stderr；测试 hook 可替换实现。
#[cfg(windows)]
fn spawn_windows_taskkill(pid: u32) -> Result<std::process::Child, std::io::Error> {
    #[cfg(test)]
    {
        if let Some(hook) = TEST_WINDOWS_TASKKILL_SPAWN
            .lock()
            .expect("taskkill spawn hook 锁中毒")
            .as_ref()
        {
            return hook(pid);
        }
    }
    StdCommand::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(all(windows, test))]
static TEST_WINDOWS_TASKKILL_SPAWN: std::sync::Mutex<
    Option<Box<dyn Fn(u32) -> Result<std::process::Child, std::io::Error> + Send>>,
> = std::sync::Mutex::new(None);

/// Business Logic（为什么需要这个函数）:
///     Attention 与前端轮询需要稳定的依赖状态变更时间，不能在每次读取时漂移。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UTC 时刻的 RFC3339 字符串，用作进程内 `status_changed_at`。
/// Windows 探测 Job Object：根进程退出后仍可终止整棵后代树。
///
/// Business Logic（为什么需要这个结构）:
///     wrapper（如 wsl/cmd）常先启动持管道的后代再自行退出；此时仅对死根 PID
///     `taskkill /T` 不可靠。Job Object + KILL_ON_JOB_CLOSE 把整棵树绑在句柄生命周期上。
///     绑定必须在用户代码运行前完成：CREATE_SUSPENDED → AssignProcessToJobObject → ResumeThread，
///     否则短命 wrapper 可在 Assign 前派生后代并逃逸。
///
/// Code Logic（这个结构做什么）:
///     持有 CREATE 的 job HANDLE（OwnedHandle）；Drop/Close 触发 KILL_ON_JOB_CLOSE；
///     `terminate` 显式 TerminateJobObject 供超时路径使用；
///     `create` + `assign_child` 与 `resume_suspended_process` 组成原子进 job 路径。
#[cfg(windows)]
#[derive(Debug)]
pub struct WindowsProbeJob {
    handle: std::os::windows::io::OwnedHandle,
}

/// Windows CreateProcess 的 CREATE_SUSPENDED 标志。
/// Business Logic: 用户代码不得在进入 Job 前运行，否则后代可逃逸。
/// Code Logic: 0x4，与 CommandExt::creation_flags 组合使用。
#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[cfg(windows)]
impl WindowsProbeJob {
    /// Business Logic（为什么需要这个函数）:
    ///     必须先建好带 KILL_ON_JOB_CLOSE 的 Job，再挂起 spawn 的根进程，才能在 Resume 前完成绑定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CreateJobObjectW → 设置 JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE；不绑定进程。
    pub fn create() -> Result<Self, std::io::Error> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut core::ffi::c_void;

        const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: DWORD = 0x0000_2000;
        const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

        #[repr(C)]
        struct JobObjectBasicLimitInformation {
            per_process_user_time_limit: i64,
            per_job_user_time_limit: i64,
            limit_flags: DWORD,
            minimum_working_set_size: usize,
            maximum_working_set_size: usize,
            active_process_limit: DWORD,
            affinity: usize,
            priority_class: DWORD,
            scheduling_class: DWORD,
        }

        #[repr(C)]
        struct IoCounters {
            read_operation_count: u64,
            write_operation_count: u64,
            other_operation_count: u64,
            read_transfer_count: u64,
            write_transfer_count: u64,
            other_transfer_count: u64,
        }

        #[repr(C)]
        struct JobObjectExtendedLimitInformation {
            basic_limit_information: JobObjectBasicLimitInformation,
            io_info: IoCounters,
            process_memory_limit: usize,
            job_memory_limit: usize,
            peak_process_memory_used: usize,
            peak_job_memory_used: usize,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn CreateJobObjectW(
                lp_job_attributes: *mut core::ffi::c_void,
                lp_name: *const u16,
            ) -> HANDLE;
            fn SetInformationJobObject(
                h_job: HANDLE,
                job_object_information_class: i32,
                lp_job_object_information: *mut core::ffi::c_void,
                cb_job_object_information_length: DWORD,
            ) -> BOOL;
        }

        let raw = unsafe { CreateJobObjectW(core::ptr::null_mut(), core::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw as _) };

        let mut info = JobObjectExtendedLimitInformation {
            basic_limit_information: JobObjectBasicLimitInformation {
                per_process_user_time_limit: 0,
                per_job_user_time_limit: 0,
                limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                minimum_working_set_size: 0,
                maximum_working_set_size: 0,
                active_process_limit: 0,
                affinity: 0,
                priority_class: 0,
                scheduling_class: 0,
            },
            io_info: IoCounters {
                read_operation_count: 0,
                write_operation_count: 0,
                other_operation_count: 0,
                read_transfer_count: 0,
                write_transfer_count: 0,
                other_transfer_count: 0,
            },
            process_memory_limit: 0,
            job_memory_limit: 0,
            peak_process_memory_used: 0,
            peak_job_memory_used: 0,
        };

        let ok = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle() as HANDLE,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&mut info as *mut JobObjectExtendedLimitInformation).cast(),
                std::mem::size_of::<JobObjectExtendedLimitInformation>() as DWORD,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self { handle })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     挂起进程必须在 Resume 前进入 Job，后续派生的后代才会继承 Job 成员身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     AssignProcessToJobObject(self, child process handle)；失败返回 last_os_error。
    pub fn assign_child(&self, child: &Child) -> Result<(), std::io::Error> {
        use std::os::windows::io::AsRawHandle;

        type BOOL = i32;
        type HANDLE = *mut core::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn AssignProcessToJobObject(h_job: HANDLE, h_process: HANDLE) -> BOOL;
        }

        let process = child.as_raw_handle() as HANDLE;
        let ok =
            unsafe { AssignProcessToJobObject(self.handle.as_raw_handle() as HANDLE, process) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     超时/管道排空失败时必须立刻终止 job 内全部进程，不能等 Drop。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 TerminateJobObject(handle, 1)；失败返回 last_os_error。
    pub fn terminate(&self) -> Result<(), std::io::Error> {
        use std::os::windows::io::AsRawHandle;

        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut core::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn TerminateJobObject(h_job: HANDLE, u_exit_code: DWORD) -> BOOL;
        }

        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, 1) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     CREATE_SUSPENDED 后 std::process::Command 会关闭主线程句柄；必须再找到主线程并 Resume，
///     否则探测命令永远挂起。
///
/// Code Logic（这个函数做什么）:
///     CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD) 枚举线程，找到 owner==pid 的首个线程，
///     OpenThread(THREAD_SUSPEND_RESUME) → ResumeThread → CloseHandle；找不到线程返回 NotFound。
#[cfg(windows)]
fn resume_suspended_process(pid: u32) -> Result<(), std::io::Error> {
    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut core::ffi::c_void;

    const TH32CS_SNAPTHREAD: DWORD = 0x0000_0004;
    const THREAD_SUSPEND_RESUME: DWORD = 0x0002;
    const INVALID_HANDLE_VALUE: HANDLE = !0usize as HANDLE;

    #[repr(C)]
    struct ThreadEntry32 {
        dw_size: DWORD,
        cnt_usage: DWORD,
        th32_thread_id: DWORD,
        th32_owner_process_id: DWORD,
        tp_base_pri: i32,
        tp_delta_pri: i32,
        dw_flags: DWORD,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(dw_flags: DWORD, th32_process_id: DWORD) -> HANDLE;
        fn Thread32First(h_snapshot: HANDLE, lpte: *mut ThreadEntry32) -> BOOL;
        fn Thread32Next(h_snapshot: HANDLE, lpte: *mut ThreadEntry32) -> BOOL;
        fn OpenThread(
            dw_desired_access: DWORD,
            b_inherit_handle: BOOL,
            dw_thread_id: DWORD,
        ) -> HANDLE;
        fn ResumeThread(h_thread: HANDLE) -> DWORD;
        fn CloseHandle(h_object: HANDLE) -> BOOL;
    }

    // 线程枚举可能短暂滞后于 CreateProcess；短重试避免误报 NotFound。
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut last_err = std::io::Error::new(
        ErrorKind::NotFound,
        format!("suspended process {pid} has no thread to resume"),
    );
    loop {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            last_err = std::io::Error::last_os_error();
        } else {
            let mut entry = ThreadEntry32 {
                dw_size: std::mem::size_of::<ThreadEntry32>() as DWORD,
                cnt_usage: 0,
                th32_thread_id: 0,
                th32_owner_process_id: 0,
                tp_base_pri: 0,
                tp_delta_pri: 0,
                dw_flags: 0,
            };
            let mut thread_id: Option<DWORD> = None;
            let mut has = unsafe { Thread32First(snapshot, &mut entry) };
            while has != 0 {
                if entry.th32_owner_process_id == pid {
                    thread_id = Some(entry.th32_thread_id);
                    break;
                }
                has = unsafe { Thread32Next(snapshot, &mut entry) };
            }
            unsafe {
                let _ = CloseHandle(snapshot);
            }

            if let Some(tid) = thread_id {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, tid) };
                if thread.is_null() {
                    last_err = std::io::Error::last_os_error();
                } else {
                    let prev = unsafe { ResumeThread(thread) };
                    unsafe {
                        let _ = CloseHandle(thread);
                    }
                    // ResumeThread 失败返回 u32::MAX。
                    if prev == DWORD::MAX {
                        return Err(std::io::Error::last_os_error());
                    }
                    return Ok(());
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(last_err);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Business Logic（为什么需要这个函数）:
///     挂起 spawn 后若 Assign/Resume 失败，必须立刻杀掉根进程，避免留下永不 resume 的僵尸。
///
/// Code Logic（这个函数做什么）:
///     child.kill() + 有界 try_wait 轮询；忽略已退出错误。
#[cfg(windows)]
fn kill_suspended_probe_child(child: &mut Child) {
    let _ = child.kill();
    let deadline = Instant::now() + PROBE_TERMINATE_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break,
        }
    }
}

fn now_status_changed_at() -> String {
    Utc::now().to_rfc3339()
}

const WORKBENCH_TMUX_CONF: &str =
    "set -s exit-empty off\nset -g destroy-unattached off\nset -g mouse off\n";

/// Business Logic（为什么需要这个函数）:
///     默认 tmux socket 跟 TMPDIR/`/tmp`，sidecar 被杀后 server 退出会留下坏 socket，
///     下次 restore 看到 tmux_target_missing。socket 必须落在 data_dir 里，和 TMPDIR 脱钩。
///
/// Code Logic（这个函数做什么）:
///     返回 `<data_dir>/tmux`，必要时创建目录。
pub(crate) fn workbench_tmux_runtime_dir() -> Result<PathBuf, AppError> {
    let dir = crate::config::data_dir()?.join("tmux");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Business Logic（为什么需要这个函数）:
///     单测需要在不碰真实 home 的情况下锁定 `-S/-f` 参数形状。
///
/// Code Logic（这个函数做什么）:
///     生成 `-S <dir>/cc-partner.sock -f <dir>/tmux.conf`。
pub(crate) fn workbench_tmux_isolation_args_for_dir(dir: &Path) -> Vec<String> {
    vec![
        "-S".to_string(),
        dir.join("cc-partner.sock").to_string_lossy().into_owned(),
        "-f".to_string(),
        dir.join("tmux.conf").to_string_lossy().into_owned(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     工作台 tmux 不得加载用户 `~/.tmux.conf`（可能 destroy-unattached on），
///     也不得使用进程 TMPDIR 下的默认 socket。
///
/// Code Logic（这个函数做什么）:
///     确保 conf 内容后返回 isolation args；失败则回退 `-L cc-partner`。
///     WSL 把 Windows data_dir 转成 `/mnt/<drive>/...`，转失败同样回退 `-L`。
fn workbench_tmux_isolation_args(cwd_mode: TmuxCwdMode) -> Vec<String> {
    match prepare_workbench_tmux_runtime() {
        Ok(dir) => {
            let args = workbench_tmux_isolation_args_for_dir(&dir);
            match cwd_mode {
                TmuxCwdMode::Native => args,
                TmuxCwdMode::Wsl => match convert_isolation_args_for_wsl(&args) {
                    Some(converted) => converted,
                    None => vec!["-L".to_string(), "cc-partner".to_string()],
                },
            }
        }
        Err(_) => vec!["-L".to_string(), "cc-partner".to_string()],
    }
}

fn prepare_workbench_tmux_runtime() -> Result<PathBuf, AppError> {
    let dir = workbench_tmux_runtime_dir()?;
    let conf = dir.join("tmux.conf");
    if std::fs::read_to_string(&conf).ok().as_deref() != Some(WORKBENCH_TMUX_CONF) {
        std::fs::write(&conf, WORKBENCH_TMUX_CONF)?;
    }
    Ok(dir)
}

fn convert_isolation_args_for_wsl(args: &[String]) -> Option<Vec<String>> {
    let mut converted = Vec::with_capacity(args.len());
    let mut pending_path = false;
    for arg in args {
        if pending_path {
            converted.push(crate::workbench::sessions::windows_path_to_wsl_path(arg)?);
            pending_path = false;
            continue;
        }
        if arg == "-S" || arg == "-f" {
            pending_path = true;
        }
        converted.push(arg.clone());
    }
    Some(converted)
}

/// Business Logic（为什么需要这个枚举）:
///     Windows 上的 tmux 运行在 WSL 内部，不能直接识别宿主 Windows 盘符路径。
///
/// Code Logic（这个枚举做什么）:
///     标记 tmux 命令应使用原生项目路径，还是先把 Windows 项目路径转换为 WSL mount 路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TmuxCwdMode {
    Native,
    Wsl,
}

/// 可用 tmux 命令描述。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台需要在 macOS/Linux 调用原生 tmux，也需要在 Windows 复用 WSL 中的 tmux 来保留终端上下文。
///
/// Code Logic（这个结构体做什么）:
///     保存可执行程序、固定前缀参数和 cwd 路径模式，统一生成 std::process::Command 与 portable-pty CommandBuilder。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TmuxCommand {
    pub(crate) program: String,
    pub(crate) prefix_args: Vec<String>,
    pub(crate) cwd_mode: TmuxCwdMode,
}

impl TmuxCommand {
    /// Business Logic（为什么需要这个函数）:
    ///     macOS/Linux 上的 tmux 可以直接用原生命令执行，并使用项目的原生文件系统路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 cwd 模式为 Native 的 tmux 命令，并带上 data_dir socket/config isolation 前缀。
    pub(crate) fn native(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            prefix_args: workbench_tmux_isolation_args(TmuxCwdMode::Native),
            cwd_mode: TmuxCwdMode::Native,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Windows 用户可把 tmux 安装在 WSL 中，工作台应通过 wsl.exe 调用它以获得可恢复上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 `wsl.exe --exec tmux` 命令描述，并标记 cwd 需要转换成 WSL mount 路径。
    pub(crate) fn wsl() -> Self {
        let mut prefix_args = vec!["--exec".to_string(), "tmux".to_string()];
        prefix_args.extend(workbench_tmux_isolation_args(TmuxCwdMode::Wsl));
        Self {
            program: "wsl.exe".to_string(),
            prefix_args,
            cwd_mode: TmuxCwdMode::Wsl,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     探测、创建、查询和销毁 tmux session 都需要使用同一套命令前缀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 std::process::Command，并预先附加固定前缀参数。
    pub(crate) fn std_command(&self) -> StdCommand {
        let mut command = StdCommand::new(&self.program);
        command.args(&self.prefix_args);
        // 新进程组即可与 sidecar 脱钩；禁止 setsid——探测路径随后还会 setpgid，
        // session leader 上 setpgid 会 EPERM，所有候选失败后 UI 误报「需要安装 tmux」。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command
    }

    /// Business Logic（为什么需要这个函数）:
    ///     「有没有安装 tmux」只应跑 `tmux -V`，不能带工作台 `-S/-f`，也不能 setsid。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native：`{program} -V`；WSL：`wsl.exe --exec tmux -V`。无 isolation 前缀、无 pre_exec。
    pub(crate) fn version_probe_command(&self) -> StdCommand {
        let mut command = StdCommand::new(&self.program);
        command.args(self.version_probe_args());
        command
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单测锁定版本探测参数不含 socket/config，避免探测失败被当成未安装。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 仅 `-V`；WSL 为 `--exec tmux -V`。
    pub(crate) fn version_probe_args(&self) -> Vec<String> {
        match self.cwd_mode {
            TmuxCwdMode::Native => vec!["-V".to_string()],
            TmuxCwdMode::Wsl => vec!["--exec".to_string(), "tmux".to_string(), "-V".to_string()],
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     PTY attach 需要通过 portable-pty 的 CommandBuilder 启动，并复用 tmux 命令前缀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 CommandBuilder，并逐个追加固定前缀参数。
    pub(crate) fn command_builder(&self) -> CommandBuilder {
        let mut command = CommandBuilder::new(&self.program);
        command.args(self.prefix_args.iter().map(String::as_str));
        command
    }

    /// Business Logic（为什么需要这个函数）:
    ///     创建 tmux session 时，`-c` 工作目录必须是 tmux 所在环境可识别的路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 模式原样返回项目路径；WSL 模式把 Windows 盘符路径转换为 `/mnt/<drive>/...`。
    pub(crate) fn project_cwd(&self, project_path: &str) -> Result<String, AppError> {
        match self.cwd_mode {
            TmuxCwdMode::Native => Ok(project_path.to_string()),
            TmuxCwdMode::Wsl => {
                super::sessions::windows_path_to_wsl_path(project_path).ok_or_else(|| {
                    AppError::generic(format!("项目路径无法转换为 WSL 路径: {project_path}"))
                })
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WSL 内的 tmux 应启动 Linux 默认 shell，而不能把 Windows 的 cmd.exe 当作 Linux 命令执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 模式返回传入 shell 命令；WSL 模式返回 None，让 tmux 使用 WSL 用户默认 shell。
    pub(crate) fn shell_command_for_new_session<'a>(
        &self,
        shell_command: &'a str,
    ) -> Option<&'a str> {
        match self.cwd_mode {
            TmuxCwdMode::Native => Some(shell_command),
            TmuxCwdMode::Wsl => None,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端会展示会话命令，Windows+WSL tmux 会话不应误显示为宿主 Windows 的 `cmd.exe`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 模式保留真实 shell 命令；WSL 模式展示实际 PTY attach 命令。
    pub(crate) fn display_command_for_session(
        &self,
        session_name: &str,
        window_target: Option<&str>,
        shell_command: &str,
    ) -> String {
        match self.cwd_mode {
            TmuxCwdMode::Native => shell_command.to_string(),
            TmuxCwdMode::Wsl => {
                let mut parts = Vec::with_capacity(self.prefix_args.len() + 4);
                parts.push(self.program.clone());
                parts.extend(self.prefix_args.clone());
                match window_target {
                    Some(target) => parts.extend(super::sessions::tmux_attach_window_args(
                        session_name,
                        target,
                    )),
                    None => {
                        parts.push("attach-session".to_string());
                        parts.push("-t".to_string());
                        parts.push(session_name.to_string());
                    }
                }
                parts.join(" ")
            }
        }
    }
}

/// Workbench 依赖状态枚举。
///
/// Business Logic（为什么需要这个枚举）:
///     前端需要区分可用、缺失、不支持、失败和安装中，用于展示不同操作入口。
///
/// Code Logic（这个枚举做什么）:
///     以小写字符串序列化到 IPC DTO，保持 TypeScript 联合类型契约。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkbenchDependencyState {
    Ready,
    Missing,
    Unsupported,
    Failed,
    Installing,
}

/// Workbench tmux 依赖状态 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 和设置页需要同一份后端状态，既能展示检测结果，也能展示安装进度与错误摘要；
///     Attention 还需要稳定的状态变更时间，避免轮询把环境条目的 updatedAt 不断刷新。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 camelCase，字段与前端 WorkbenchDependencyStatus 对齐；
///     `status_changed_at` 仅在语义状态枚举变化时由 dependency manager 更新，不落 SQLite。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchDependencyStatusDto {
    pub status: WorkbenchDependencyState,
    pub available: bool,
    pub version: Option<String>,
    pub backend: String,
    pub path: Option<String>,
    pub installable: bool,
    pub install_command_preview: Vec<String>,
    pub error: Option<String>,
    pub output: Vec<String>,
    /// 进程内最近一次语义状态枚举变化时间（RFC3339），序列化为 statusChangedAt。
    pub status_changed_at: String,
}

/// 依赖安装运行时状态。
///
/// Business Logic（为什么需要这个结构体）:
///     安装命令跨 invoke 调用运行，前端需要轮询状态、读取最近输出并能取消进行中的任务；
///     Attention 还需要依赖状态变更时间保持稳定，不能在每次探测/轮询时重置。
///
/// Code Logic（这个结构体做什么）:
///     用 Mutex 保存当前 DTO、后台任务句柄与取消令牌；状态写入统一走 `apply_status_transition`，
///     仅在语义 status 枚举变化时刷新 `status_changed_at`（进程内，不落 SQLite）。
pub struct WorkbenchDependencyInstallRuntime {
    inner: Mutex<DependencyInstallInner>,
}

struct DependencyInstallInner {
    status: WorkbenchDependencyStatusDto,
    task: Option<JoinHandle<()>>,
    cancel_token: Option<CancellationToken>,
}

impl WorkbenchDependencyInstallRuntime {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要一个空闲的依赖管理运行时，供所有命令共享；
    ///     启动后 Attention 可能立刻读取缓存，绝不能把“未探测”伪装成真实 missing。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造时同步执行一次真实 tmux 探测并写入 `status_changed_at`，
    ///     使桌面/headless/移动端冷启动都能拿到真实依赖状态，而不是 missing 占位。
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(DependencyInstallInner {
                status: stamp_initial_status(probe_workbench_dependency()),
                task: None,
                cancel_token: None,
            }),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端轮询状态时不能拿到内部锁或任务句柄，只需要当前 DTO 快照；
    ///     Attention source 也必须只读缓存，不能触发探测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆并返回当前状态（含稳定的 status_changed_at）。
    pub fn status(&self) -> WorkbenchDependencyStatusDto {
        self.inner
            .lock()
            .expect("workbench dependency 锁中毒")
            .status
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     检测命令需要把最新 tmux 状态写入共享运行时，供后续 status 读取。
    ///
    /// Code Logic（这个函数做什么）:
    ///     非安装中状态经 `apply_status_transition` 覆盖 DTO（同枚举保留时间戳）；
    ///     安装中时保留安装进度，避免 recheck 把 UI 状态打回 missing。
    pub fn set_checked_status(
        &self,
        status: WorkbenchDependencyStatusDto,
    ) -> WorkbenchDependencyStatusDto {
        let mut inner = self.inner.lock().expect("workbench dependency 锁中毒");
        if inner.status.status != WorkbenchDependencyState::Installing {
            inner.status = apply_status_transition(&inner.status, status);
        }
        inner.status.clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户点击安装后，前端应立即看到 installing 状态和执行命令预览。
    ///
    /// Code Logic（这个函数做什么）:
    ///     设置安装中 DTO；测试和真实命令启动都复用此方法。
    pub fn mark_installing(&self, command: Vec<String>) {
        self.replace_status(WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Installing,
            available: false,
            version: None,
            backend: backend_for_platform(current_platform()).to_string(),
            path: None,
            installable: false,
            install_command_preview: command,
            error: None,
            output: vec!["开始安装 tmux".to_string()],
            status_changed_at: String::new(),
        });
    }

    /// Business Logic（为什么需要这个函数）:
    ///     安装失败或取消时，用户需要看到失败原因和最近输出，而不是只看到缺失状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把状态置为 failed，保留输出摘要并清理任务句柄/取消令牌。
    pub fn mark_failed(&self, error: impl Into<String>, output: Vec<String>) {
        let error = error.into();
        let mut lines = output;
        if lines.is_empty() {
            lines.push(error.clone());
        }
        self.replace_status(WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Failed,
            available: false,
            version: None,
            backend: backend_for_platform(current_platform()).to_string(),
            path: None,
            installable: true,
            install_command_preview: actual_install_command_preview().unwrap_or_default(),
            error: Some(error),
            output: truncate_output_lines(lines),
            status_changed_at: String::new(),
        });
        self.clear_task();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户取消安装后，应停止后台命令并让前端知道这是人为取消。
    ///
    /// Code Logic（这个函数做什么）:
    ///     设置取消令牌，随后立即写入 failed/安装已取消 状态；后台任务收到令牌后会尝试 kill 子进程。
    pub fn cancel(&self) -> WorkbenchDependencyStatusDto {
        let token = {
            self.inner
                .lock()
                .expect("workbench dependency 锁中毒")
                .cancel_token
                .clone()
        };
        if let Some(token) = token {
            token.cancel();
            self.mark_cancelled();
        }
        self.status()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单元测试和取消流程都需要把安装中状态收敛为用户可理解的取消失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 failed 状态，并把“安装已取消”追加到输出摘要。
    pub fn mark_cancelled(&self) {
        self.mark_failed("安装已取消", vec!["安装已取消".to_string()]);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     安装命令需要异步运行，不能阻塞 Tauri IPC 线程。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存取消令牌与任务句柄；任务完成后按 exit status 更新 ready 或 failed。
    pub fn spawn_install(
        self: &Arc<Self>,
        command: Vec<String>,
    ) -> Result<WorkbenchDependencyStatusDto, AppError> {
        if self.status().status == WorkbenchDependencyState::Installing {
            return Ok(self.status());
        }
        let Some((program, args)) = command.split_first() else {
            return Err(AppError::generic("缺少安装命令"));
        };
        self.mark_installing(command.clone());
        let uses_sudo = install_command_uses_sudo(&command);
        let token = CancellationToken::new();
        let runtime = Arc::clone(self);
        let program = program.clone();
        let args = args.to_vec();
        let task_token = token.clone();
        let task = tauri::async_runtime::spawn(async move {
            let child = match tokio::process::Command::new(&program)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    runtime.mark_failed(format!("启动安装命令失败: {error}"), Vec::new());
                    return;
                }
            };

            let output_future = child.wait_with_output();
            tokio::pin!(output_future);

            tokio::select! {
                _ = task_token.cancelled() => {
                    runtime.mark_cancelled();
                }
                _ = async {
                    if uses_sudo {
                        tokio::time::sleep(SUDO_NO_TTY_TIMEOUT).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    runtime.mark_failed(
                        SUDO_NO_TTY_OPERABLE_ERROR,
                        vec![SUDO_NO_TTY_OPERABLE_ERROR.to_string()],
                    );
                }
                result = &mut output_future => {
                    match result {
                        Ok(output) if output.status.success() => {
                            let checked = probe_workbench_dependency();
                            runtime.set_checked_status(checked);
                            runtime.clear_task();
                        }
                        Ok(output) => {
                            let lines = output_lines(&output.stdout, &output.stderr);
                            runtime.mark_failed(format!("安装命令退出码: {}", output.status), lines);
                        }
                        Err(error) => {
                            runtime.mark_failed(format!("读取安装结果失败: {error}"), Vec::new());
                        }
                    }
                }
            }
        });
        let mut inner = self.inner.lock().expect("workbench dependency 锁中毒");
        inner.cancel_token = Some(token);
        inner.task = Some(task);
        Ok(inner.status.clone())
    }

    fn replace_status(&self, status: WorkbenchDependencyStatusDto) {
        let mut inner = self.inner.lock().expect("workbench dependency 锁中毒");
        inner.status = apply_status_transition(&inner.status, status);
    }

    fn clear_task(&self) {
        let mut inner = self.inner.lock().expect("workbench dependency 锁中毒");
        inner.task = None;
        inner.cancel_token = None;
    }
}

/// Business Logic（为什么需要这个函数）:
///     依赖探测结果本身不携带稳定变更时间；manager 需要在写入缓存时决定是否刷新时间戳。
///
/// Code Logic（这个函数做什么）:
///     若新旧语义 status 枚举相同，则保留旧 `status_changed_at`；否则写入当前 UTC RFC3339。
fn apply_status_transition(
    previous: &WorkbenchDependencyStatusDto,
    mut next: WorkbenchDependencyStatusDto,
) -> WorkbenchDependencyStatusDto {
    if previous.status == next.status {
        next.status_changed_at = previous.status_changed_at.clone();
    } else {
        next.status_changed_at = now_status_changed_at();
    }
    next
}

/// Business Logic（为什么需要这个函数）:
///     运行时首次构造时还没有“旧状态”，也必须给出非空的状态变更时间供 Attention 使用。
///
/// Code Logic（这个函数做什么）:
///     给初始 DTO 写入当前 UTC RFC3339 的 `status_changed_at`。
fn stamp_initial_status(mut status: WorkbenchDependencyStatusDto) -> WorkbenchDependencyStatusDto {
    status.status_changed_at = now_status_changed_at();
    status
}

impl Default for WorkbenchDependencyInstallRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// 依赖检测平台。
///
/// Business Logic（为什么需要这个枚举）:
///     tmux 在 macOS/Linux/Windows 的检测和安装入口不同，需要显式区分平台策略。
///
/// Code Logic（这个枚举做什么）:
///     提供可测试的平台分支，不直接依赖 cfg 宏散落在安装命令选择逻辑中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum DependencyPlatform {
    MacOs,
    Linux,
    Windows,
    Unsupported,
}

/// Business Logic（为什么需要这个函数）:
///     前端显示版本时只需要 tmux 自身版本号，不需要完整命令输出。
///
/// Code Logic（这个函数做什么）:
///     解析形如 `tmux 3.4` 的输出；格式不匹配返回 None。
pub(crate) fn parse_tmux_version(output: &str) -> Option<String> {
    let mut parts = output.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("tmux"), Some(version)) => Some(version.to_string()),
        _ => None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     现有工作台会话创建/恢复逻辑需要复用同一套 tmux 探测，避免依赖状态和真实会话行为分叉。
///
/// Code Logic（这个函数做什么）:
///     按当前平台候选顺序执行带超时的 `tmux -V`；成功时返回可用于 sessions 的 TmuxCommand。
pub(crate) fn available_tmux_command() -> Option<TmuxCommand> {
    match probe_tmux_command() {
        TmuxProbeOutcome::Ready(probe) => Some(probe.command),
        TmuxProbeOutcome::Missing | TmuxProbeOutcome::TimedOut => None,
    }
}

/// Business Logic（为什么需要这个函数）:
///     check 命令需要返回完整 DTO，包括可用性、版本、后端、路径和安装命令预览。
///
/// Code Logic（这个函数做什么）:
///     带硬超时探测 tmux；成功返回 ready；全候选失败返回 missing/unsupported；
///     任一候选超时且无成功 → failed（可 recheck），避免把 hang 伪装成 missing。
pub fn probe_workbench_dependency() -> WorkbenchDependencyStatusDto {
    match probe_tmux_command() {
        TmuxProbeOutcome::Ready(probe) => WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Ready,
            available: true,
            version: probe.version,
            backend: probe.backend,
            path: Some(probe.command.program),
            installable: false,
            install_command_preview: Vec::new(),
            error: None,
            output: Vec::new(),
            // 真实变更时间由 dependency manager 在写入缓存时按语义状态差决定。
            status_changed_at: String::new(),
        },
        TmuxProbeOutcome::TimedOut => WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Failed,
            available: false,
            version: None,
            backend: backend_for_platform(current_platform()).to_string(),
            path: None,
            installable: actual_install_command_preview().is_some(),
            install_command_preview: actual_install_command_preview().unwrap_or_default(),
            error: Some(format!(
                "tmux 探测超时（{} 秒），请稍后重新检测",
                PROBE_COMMAND_TIMEOUT.as_secs()
            )),
            output: vec!["依赖探测超时，已终止外部进程".to_string()],
            status_changed_at: String::new(),
        },
        TmuxProbeOutcome::Missing => missing_or_unsupported_status(
            actual_install_command_preview().unwrap_or_default(),
            None,
        ),
    }
}

/// Business Logic（为什么需要这个函数）:
///     install 命令需要使用与 check DTO 一致的命令预览，避免展示和真实执行不一致。
///
/// Code Logic（这个函数做什么）:
///     按当前平台与系统可见包管理器生成安装命令 argv。
/// sudo 无 TTY 时的可操作错误（headless / P2P 安装不得挂死等密码）。
pub const SUDO_NO_TTY_OPERABLE_ERROR: &str =
    "无 TTY 无法输入密码，请在对端本机执行 doctor 提示的命令";
const SUDO_NO_TTY_TIMEOUT: Duration = Duration::from_secs(12);

/// Business Logic（为什么需要这个函数）:
///     Linux/WSL 安装命令含 sudo 时，无 TTY 会永久阻塞；必须识别后加超时。
///
/// Code Logic（这个函数做什么）:
///     argv 任一段等于 `sudo` 或包含 `sudo ` 则 true。
pub fn install_command_uses_sudo(command: &[String]) -> bool {
    command
        .iter()
        .any(|part| part == "sudo" || part.contains("sudo "))
}

/// Business Logic（为什么需要这个函数）:
///     缺 capability 或旧路由时卡片为 unsupported，不能冒充 ready，也不能当成 tmux missing。
///
/// Code Logic（这个函数做什么）:
///     构造 installable=false 的 unsupported DTO。
pub fn unsupported_dependency_status(error: impl Into<String>) -> WorkbenchDependencyStatusDto {
    WorkbenchDependencyStatusDto {
        status: WorkbenchDependencyState::Unsupported,
        available: false,
        version: None,
        backend: backend_for_platform(current_platform()).to_string(),
        path: None,
        installable: false,
        install_command_preview: Vec::new(),
        error: Some(error.into()),
        output: Vec::new(),
        status_changed_at: now_status_changed_at(),
    }
}

pub fn actual_install_command_preview() -> Option<Vec<String>> {
    let tools = ["brew", "apt-get", "dnf", "pacman", "wsl.exe"]
        .iter()
        .copied()
        .filter(|tool| command_exists(tool))
        .collect::<Vec<_>>();
    install_command_preview_for_platform(current_platform(), &tools)
}

#[derive(Debug)]
struct TmuxProbe {
    command: TmuxCommand,
    version: Option<String>,
    backend: String,
}

/// tmux 探测结果（含超时区分）。
///
/// Business Logic（为什么需要这个枚举）:
///     hang 超时与“确实缺失”对 Attention/安装入口语义不同，不能一律当 missing。
///
/// Code Logic（这个枚举做什么）:
///     Ready=探测成功；TimedOut=至少一个候选超时且无成功；Missing=候选均快速失败。
#[derive(Debug)]
enum TmuxProbeOutcome {
    Ready(TmuxProbe),
    TimedOut,
    Missing,
}

/// Business Logic（为什么需要这个函数）:
///     依赖探测必须在 WSL/tmux hang 时仍能返回，否则 AppState 构造永久阻塞。
///
/// Code Logic（这个函数做什么）:
///     按平台候选顺序跑带超时的 `tmux -V`；成功即 Ready；若出现超时且无成功 → TimedOut；
///     否则 Missing。可注入 runner 供单测覆盖超时路径。
fn probe_tmux_command() -> TmuxProbeOutcome {
    probe_tmux_command_with(current_platform(), run_std_command_with_timeout)
}

/// Business Logic（为什么需要这个函数）:
///     单测需要注入挂起/失败 runner，避免真实 sleep 子进程拖慢 CI。
///
/// Code Logic（这个函数做什么）:
///     对每个候选构造 `tmux -V` 命令，调用 runner；汇总 Ready / TimedOut / Missing。
fn probe_tmux_command_with<F>(platform: DependencyPlatform, mut runner: F) -> TmuxProbeOutcome
where
    F: FnMut(StdCommand, Duration) -> Result<Output, ProbeCommandError>,
{
    let candidates = tmux_candidates_for_platform(platform);
    if candidates.is_empty() {
        return TmuxProbeOutcome::Missing;
    }
    let mut saw_timeout = false;
    for candidate in candidates {
        let command = candidate.command.version_probe_command();
        match runner(command, PROBE_COMMAND_TIMEOUT) {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return TmuxProbeOutcome::Ready(TmuxProbe {
                    version: parse_tmux_version(&stdout),
                    backend: candidate.backend.to_string(),
                    command: candidate.command,
                });
            }
            Ok(_) => {}
            Err(ProbeCommandError::TimedOut) => {
                saw_timeout = true;
            }
            Err(ProbeCommandError::SpawnOrIo) => {}
        }
    }
    if saw_timeout {
        TmuxProbeOutcome::TimedOut
    } else {
        TmuxProbeOutcome::Missing
    }
}

/// 外部探测命令错误。
///
/// Business Logic（为什么需要这个枚举）:
///     超时与启动失败需要不同收敛策略（超时保留 failed 可 recheck）。
///
/// Code Logic（这个枚举做什么）:
///     TimedOut=硬超时已 kill；SpawnOrIo=无法启动或 IO 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeCommandError {
    TimedOut,
    SpawnOrIo,
}

/// Business Logic（为什么需要这个函数）:
///     同步 `output()` 无超时，WSL/异常 wrapper 会永久卡住启动路径；
///     仅约束 try_wait 不够——kill/wait 失败或后代持有管道时仍会无限阻塞。
///
/// Code Logic（这个函数做什么）:
///     以共享 deadline 覆盖整个探测生命周期：Unix 上用独立进程组 spawn，
///     超时后 kill/killpg + 有界 wait/reap；stdout/stderr 在当前线程非阻塞 poll/read，
///     超时后关闭读端，绝不 spawn 可 forget 的 reader 线程。
fn run_std_command_with_timeout(
    command: StdCommand,
    timeout: Duration,
) -> Result<Output, ProbeCommandError> {
    run_std_command_with_timeout_guarded(command, timeout, None, None)
}

/// Business Logic（为什么需要这个函数）:
///     doctor 硬 deadline 必须压缩每个依赖 probe 的剩余预算，并在全局取消时立刻停止。
///
/// Code Logic（这个函数做什么）:
///     取 `min(timeout, remaining_overall)`；若已取消或剩余为 0 直接 TimedOut；
///     spawn 后登记到 guard，退出/超时时注销；轮询中检查 cancel。
fn run_std_command_with_timeout_guarded(
    mut command: StdCommand,
    timeout: Duration,
    overall_deadline: Option<Instant>,
    guard: Option<&ProbeRuntimeGuard>,
) -> Result<Output, ProbeCommandError> {
    if guard.is_some_and(ProbeRuntimeGuard::is_cancelled) {
        return Err(ProbeCommandError::TimedOut);
    }
    let mut effective = timeout;
    if let Some(overall) = overall_deadline {
        let remaining = overall.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProbeCommandError::TimedOut);
        }
        effective = effective.min(remaining);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Unix：独立进程组，便于超时 killpg 覆盖 wrapper 后代。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 0 = 子进程成为新进程组组长（setpgid(0,0)）。
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Windows：CREATE_SUSPENDED → AssignProcessToJobObject → ResumeThread，
    // 保证用户代码运行前根进程已在 Job 内，短命 wrapper 的后代不会逃逸。
    // Job 创建/配置失败必须 fail closed：spawn 前返回 SpawnOrIo，绝不启动未受 Job 约束的探测。
    #[cfg(windows)]
    let prepared_job = match WindowsProbeJob::create() {
        Ok(job) => job,
        Err(err) => {
            tracing::warn!(error = %err, "probe 未能创建 Job Object，拒绝启动未受约束探测");
            return Err(ProbeCommandError::SpawnOrIo);
        }
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_SUSPENDED);
    }
    let mut child = command.spawn().map_err(|_| ProbeCommandError::SpawnOrIo)?;
    let deadline = Instant::now() + effective;
    let process_group_id = child.id();
    #[cfg(windows)]
    let probe_job: Option<Arc<WindowsProbeJob>> = {
        // 挂起态绑定 Job；Assign/Resume 失败必须杀挂起根并返回错误，
        // 不能带着永不 resume 的僵尸继续探测。
        if let Err(err) = prepared_job.assign_child(&child) {
            tracing::warn!(
                pid = process_group_id,
                error = %err,
                "probe 未能 AssignProcessToJobObject"
            );
            kill_suspended_probe_child(&mut child);
            return Err(ProbeCommandError::SpawnOrIo);
        }
        if let Err(err) = resume_suspended_process(process_group_id) {
            tracing::warn!(
                pid = process_group_id,
                error = %err,
                "probe ResumeThread 失败，终止挂起根进程"
            );
            // Assign 已成功：用 job.terminate 清树，再 reap 根。
            let _ = prepared_job.terminate();
            kill_suspended_probe_child(&mut child);
            return Err(ProbeCommandError::SpawnOrIo);
        }
        let job = Arc::new(prepared_job);
        if let Some(guard) = guard {
            guard.register_job(process_group_id, Arc::clone(&job));
        }
        Some(job)
    };
    if let Some(guard) = guard {
        guard.register_child(process_group_id);
    }

    let status = loop {
        if guard.is_some_and(ProbeRuntimeGuard::is_cancelled) {
            terminate_probe_child(
                &mut child,
                process_group_id,
                deadline,
                #[cfg(windows)]
                probe_job.as_deref(),
            );
            if let Some(guard) = guard {
                guard.unregister_child(process_group_id);
            }
            return Err(ProbeCommandError::TimedOut);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    terminate_probe_child(
                        &mut child,
                        process_group_id,
                        deadline,
                        #[cfg(windows)]
                        probe_job.as_deref(),
                    );
                    if let Some(guard) = guard {
                        guard.unregister_child(process_group_id);
                    }
                    return Err(ProbeCommandError::TimedOut);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
            }
            Err(_) => {
                // try_wait 失败也走终止路径，避免留下僵尸。
                terminate_probe_child(
                    &mut child,
                    process_group_id,
                    deadline,
                    #[cfg(windows)]
                    probe_job.as_deref(),
                );
                if let Some(guard) = guard {
                    guard.unregister_child(process_group_id);
                }
                return Err(ProbeCommandError::SpawnOrIo);
            }
        }
    };

    // 正常退出：在共享截止时间（+ 小额 grace）内有界非阻塞 drain，避免后代持 pipe 时无界阻塞。
    let (stdout, stderr) = drain_probe_pipes(
        &mut child,
        deadline,
        #[cfg(windows)]
        probe_job.as_deref(),
    );
    if let Some(guard) = guard {
        guard.unregister_child(process_group_id);
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Business Logic（为什么需要这个函数）:
///     超时或 IO 失败后必须 best-effort 终止进程树并回收，但 kill 失败时不能无条件 child.wait() 永久挂起。
///     Windows 上 WSL/wrapper 会派生子孙进程：仅 Child::kill 会留下后代；根进程已退出时
///     taskkill /T 也不可靠，必须优先 TerminateJobObject。
///
/// Code Logic（这个函数做什么）:
///     Unix 先 killpg(SIGKILL) 再 child.kill()，killpg 失败时记 warning（含 ESRCH 外的真实错误）；
///     Windows 优先 TerminateJobObject；无 job 或失败时再有界 taskkill /T /F，最后 child.kill()；
///     随后在 deadline+PROBE_TERMINATE_GRACE 内轮询 try_wait，超时则丢弃 wait，并 drop 管道读端。
fn terminate_probe_child(
    child: &mut Child,
    process_group_id: u32,
    deadline: Instant,
    #[cfg(windows)] job: Option<&WindowsProbeJob>,
) {
    #[cfg(unix)]
    {
        if let Err(err) = kill_probe_process_group(process_group_id) {
            // ESRCH = 进程组已无成员，属预期；其它错误必须可见，便于排查 setsid 逃逸等场景。
            if err.raw_os_error() != Some(libc::ESRCH) {
                tracing::warn!(
                    process_group_id,
                    error = %err,
                    "probe killpg 失败，后续仅依赖 child.kill 与关闭管道读端"
                );
            }
        }
    }
    #[cfg(windows)]
    {
        // Business Logic: 根已退出时 taskkill /T 不可靠；job 才能覆盖持管道后代。
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let grace = remaining
            .min(PROBE_TERMINATE_GRACE)
            .max(Duration::from_millis(50));
        let mut job_ok = false;
        if let Some(job) = job {
            match job.terminate() {
                Ok(()) => job_ok = true,
                Err(err) => {
                    tracing::debug!(
                        pid = process_group_id,
                        error = %err,
                        "probe TerminateJobObject 失败，回退 taskkill /T"
                    );
                }
            }
        }
        if !job_ok {
            if let Err(err) = kill_probe_pid_windows(process_group_id, grace) {
                tracing::debug!(
                    pid = process_group_id,
                    error = %err,
                    "probe taskkill /T 未完全成功，继续 child.kill 兜底"
                );
            }
        }
    }
    if let Err(err) = child.kill() {
        // 子进程已退出时 kill 失败属常见路径，不必抬高日志级别。
        tracing::debug!(error = %err, "probe child.kill 未生效（可能已退出）");
    }

    let reap_deadline = deadline + PROBE_TERMINATE_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= reap_deadline {
                    // 截止：不再阻塞 wait；管道 handle 随 Child drop 关闭。
                    break;
                }
                let remaining = reap_deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
            }
            Err(_) => break,
        }
    }

    // 显式关闭读端：即使写端仍被逃逸后代持有，也不再保留 reader 资源。
    let _ = child.stdout.take();
    let _ = child.stderr.take();
}

/// Business Logic（为什么需要这个函数）:
///     Unix wrapper（如 wsl 包装脚本）可能留下持有 stdout 的孙进程，仅 kill 直接子进程不够。
///
/// Code Logic（这个函数做什么）:
///     对独立进程组发送 SIGKILL；pgid 非法时返回错误。
#[cfg(unix)]
fn kill_probe_process_group(process_group_id: u32) -> Result<(), std::io::Error> {
    let pgid: libc::pid_t = process_group_id.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "process group id out of range",
        )
    })?;
    // 负号表示进程组；SIGKILL 立即终止。
    let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Business Logic（为什么需要这个函数）:
///     正常退出后仍可能因孙进程持有 pipe 导致阻塞读永不返回；旧实现用后台线程 + mem::forget
///     会在 setsid/killpg 失败时永久累积 reader 线程。
///
/// Code Logic（这个函数做什么）:
///     在当前线程把 stdout/stderr 设为 nonblocking，poll/select 到 deadline 为止增量读取；
///     超时则 killpg/job+kill child 并 drop 读端，绝不 spawn/detach 线程。
fn drain_probe_pipes(
    child: &mut Child,
    deadline: Instant,
    #[cfg(windows)] job: Option<&WindowsProbeJob>,
) -> (Vec<u8>, Vec<u8>) {
    let process_group_id = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let drain_deadline = deadline + PROBE_PIPE_DRAIN_GRACE;

    let (stdout_buf, stderr_buf, timed_out) =
        drain_child_pipes_nonblocking(stdout, stderr, drain_deadline);

    if timed_out {
        // 写端可能仍被后代持有：终止进程组/job/子进程，读端已在本函数作用域 drop。
        terminate_probe_child(
            child,
            process_group_id,
            Instant::now(),
            #[cfg(windows)]
            job,
        );
    }

    (stdout_buf, stderr_buf)
}

/// Business Logic（为什么需要这个函数）:
///     把 stdout/stderr 有界排空抽成纯 IO 逻辑，便于单测「deadline 内读完 / 超时关闭读端」。
///
/// Code Logic（这个函数做什么）:
///     Unix 走 nonblocking + poll；其它平台 best-effort 非阻塞轮询读；返回 (stdout, stderr, timed_out)。
fn drain_child_pipes_nonblocking(
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    deadline: Instant,
) -> (Vec<u8>, Vec<u8>, bool) {
    #[cfg(unix)]
    {
        drain_child_pipes_nonblocking_unix(stdout, stderr, deadline)
    }
    #[cfg(not(unix))]
    {
        drain_child_pipes_nonblocking_fallback(stdout, stderr, deadline)
    }
}

/// Business Logic（为什么需要这个函数）:
///     Unix 探测路径必须在 setsid 逃逸/killpg 失败时仍能有界返回并关闭读端，
///     不能依赖可遗忘的阻塞 reader 线程；持续写入的后代也不能让 pump 越过 deadline。
///
/// Code Logic（这个函数做什么）:
///     fcntl O_NONBLOCK + poll(2) 等待可读；fcntl 失败立即关闭该侧（绝不阻塞 read）；
///     pump 在每次 read 前检查 deadline 与字节预算；超时/预算耗尽 drop 读端返回。
#[cfg(unix)]
fn drain_child_pipes_nonblocking_unix(
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    deadline: Instant,
) -> (Vec<u8>, Vec<u8>, bool) {
    use std::os::fd::{AsRawFd, RawFd};

    enum PipeKind {
        Stdout,
        Stderr,
    }

    struct PipeSide {
        kind: PipeKind,
        reader: Box<dyn Read>,
        fd: RawFd,
        buf: Vec<u8>,
        done: bool,
    }

    /// Business Logic: 探测管道必须非阻塞；fcntl 失败时绝不能继续阻塞 read。
    /// Code Logic: F_GETFL/F_SETFL 叠加 O_NONBLOCK；任一失败返回 Err，调用方立刻关该侧。
    fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Business Logic: 从单条管道尽量排空当前可读字节，但不得越过 deadline 或字节预算。
    /// Code Logic: 每次 read 前检查 deadline/预算；0=EOF；WouldBlock 停本轮；预算/超时标记 done。
    /// 返回 true 表示因 deadline 或字节预算结束（调用方应记 timed_out）。
    fn pump_side(side: &mut PipeSide, deadline: Instant, total_read: &mut usize) -> bool {
        if side.done {
            return false;
        }
        let mut chunk = [0u8; 4096];
        loop {
            if Instant::now() >= deadline {
                side.done = true;
                return true;
            }
            if *total_read >= PROBE_PIPE_DRAIN_BYTE_BUDGET {
                side.done = true;
                return true;
            }
            let remaining_budget = PROBE_PIPE_DRAIN_BYTE_BUDGET - *total_read;
            let to_read = chunk.len().min(remaining_budget);
            match side.reader.read(&mut chunk[..to_read]) {
                Ok(0) => {
                    side.done = true;
                    return false;
                }
                Ok(n) => {
                    side.buf.extend_from_slice(&chunk[..n]);
                    *total_read = total_read.saturating_add(n);
                    if *total_read >= PROBE_PIPE_DRAIN_BYTE_BUDGET {
                        side.done = true;
                        return true;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => return false,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    side.done = true;
                    return false;
                }
            }
        }
    }

    let mut sides: Vec<PipeSide> = Vec::with_capacity(2);
    let mut timed_out = false;
    if let Some(out) = stdout {
        let fd = out.as_raw_fd();
        match set_nonblocking(fd) {
            Ok(()) => sides.push(PipeSide {
                kind: PipeKind::Stdout,
                reader: Box::new(out),
                fd,
                buf: Vec::new(),
                done: false,
            }),
            // fcntl 失败：立刻丢弃读端，绝不进入可能阻塞的 read。
            Err(_) => {
                drop(out);
                timed_out = true;
            }
        }
    }
    if let Some(err) = stderr {
        let fd = err.as_raw_fd();
        match set_nonblocking(fd) {
            Ok(()) => sides.push(PipeSide {
                kind: PipeKind::Stderr,
                reader: Box::new(err),
                fd,
                buf: Vec::new(),
                done: false,
            }),
            Err(_) => {
                drop(err);
                timed_out = true;
            }
        }
    }

    let mut total_read = 0usize;

    // 先排空已有缓冲，避免 poll 前丢数据；每次 pump 都受 deadline/预算约束。
    for side in &mut sides {
        if pump_side(side, deadline, &mut total_read) {
            timed_out = true;
        }
    }

    while sides.iter().any(|s| !s.done) {
        let now = Instant::now();
        if now >= deadline || total_read >= PROBE_PIPE_DRAIN_BYTE_BUDGET {
            timed_out = true;
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;

        let mut pollfds: Vec<libc::pollfd> = sides
            .iter()
            .filter(|s| !s.done)
            .map(|s| libc::pollfd {
                fd: s.fd,
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            })
            .collect();

        if pollfds.is_empty() {
            break;
        }

        let rc = unsafe {
            libc::poll(
                pollfds.as_mut_ptr(),
                pollfds.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if rc < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            // poll 失败时不再阻塞；关闭读端并返回已读内容。
            timed_out = true;
            break;
        }
        if rc == 0 {
            timed_out = true;
            break;
        }

        // 按仍活跃 side 顺序对齐 revents。
        let mut poll_idx = 0usize;
        for side in &mut sides {
            if side.done {
                continue;
            }
            let revents = pollfds[poll_idx].revents;
            poll_idx += 1;
            if revents == 0 {
                continue;
            }
            if pump_side(side, deadline, &mut total_read) {
                timed_out = true;
            }
        }
        if timed_out {
            // 预算/截止触发后立即停止，drop 剩余读端。
            break;
        }
    }

    // 超时/预算耗尽：标记未完成侧 done，随后 drop 关闭 fd。
    if timed_out {
        for side in &mut sides {
            side.done = true;
        }
    }

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    for side in sides {
        match side.kind {
            PipeKind::Stdout => stdout_buf = side.buf,
            PipeKind::Stderr => stderr_buf = side.buf,
        }
    }

    // drop sides → 关闭读端；即使写端仍开着也不会留下 reader 线程。
    (stdout_buf, stderr_buf, timed_out)
}

/// Windows 管道有界读取规划。
///
/// Business Logic（为什么需要这个枚举）:
///     Windows 阻塞 `read` 无法被 deadline 打断；必须先按 PeekNamedPipe 结果决定下一步，
///     才能保证探测路径在父进程退出、后代仍持写端时仍有界返回。
///
/// Code Logic（这个枚举做什么）:
///     Wait=暂无字节，禁止 read；Read(n)=最多读取 n 字节；Eof=写端已断，结束该侧。
///     在 Unix 上仅供单测编译（`cfg(test)`），生产路径由 Unix nonblocking drain 接管。
#[cfg(any(not(unix), test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsPipeReadPlan {
    Wait,
    Read(usize),
    Eof,
}

/// Business Logic（为什么需要这个函数）:
///     把 PeekNamedPipe 的可用字节数映射成“是否允许调用阻塞 read”，便于跨平台单测
///     锁住“零字节绝不 read”的回归约束。
///
/// Code Logic（这个函数做什么）:
///     available=None 表示管道断开→Eof；Some(0)→Wait；Some(n>0)→Read(min(n, cap))。
#[cfg(any(not(unix), test))]
fn plan_windows_pipe_read(available: Option<u32>, cap: usize) -> WindowsPipeReadPlan {
    match available {
        None => WindowsPipeReadPlan::Eof,
        Some(0) => WindowsPipeReadPlan::Wait,
        Some(n) => {
            if cap == 0 {
                WindowsPipeReadPlan::Wait
            } else {
                WindowsPipeReadPlan::Read((n as usize).min(cap))
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     非 Unix 平台同样不能用可 forget 的阻塞 reader 线程排空探测管道；
///     Windows 上还禁止“先检查 deadline 再无界 read”——read 一旦阻塞就再也不看 deadline。
///
/// Code Logic（这个函数做什么）:
///     用 PeekNamedPipe 查询可用字节，仅在 available>0 时按预算 read；无数据则 sleep 到
///     deadline；超时立刻 drop 读端返回已读缓冲，绝无无界阻塞 read。
#[cfg(not(unix))]
fn drain_child_pipes_nonblocking_fallback(
    mut stdout: Option<ChildStdout>,
    mut stderr: Option<ChildStderr>,
    deadline: Instant,
) -> (Vec<u8>, Vec<u8>, bool) {
    use std::os::windows::io::{AsRawHandle, RawHandle};

    /// Business Logic: PeekNamedPipe 失败（含 ERROR_BROKEN_PIPE）表示写端已断，应结束该侧。
    /// Code Logic: 成功返回 Some(avail)；失败（含断开）返回 None，调用方按 Eof 处理。
    fn peek_named_pipe_available(handle: RawHandle) -> Option<u32> {
        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut core::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn PeekNamedPipe(
                h_named_pipe: HANDLE,
                lp_buffer: *mut u8,
                n_buffer_size: DWORD,
                lp_bytes_read: *mut DWORD,
                lp_total_bytes_avail: *mut DWORD,
                lp_bytes_left_this_message: *mut DWORD,
            ) -> BOOL;
        }

        let mut available: DWORD = 0;
        let ok = unsafe {
            PeekNamedPipe(
                handle as HANDLE,
                core::ptr::null_mut(),
                0,
                core::ptr::null_mut(),
                &mut available,
                core::ptr::null_mut(),
            )
        };
        if ok == 0 {
            None
        } else {
            Some(available)
        }
    }

    /// Business Logic: 单侧管道只能在 Peek 确认有字节后才 read，避免 WSL 后代挂死启动。
    /// Code Logic: Peek→plan→有预算才 read；EOF/错误标记 done；Wait 不推进 progress。
    fn pump_windows_side<R: Read + AsRawHandle>(
        pipe: &mut R,
        chunk: &mut [u8],
        buf: &mut Vec<u8>,
    ) -> WindowsPipeReadPlan {
        let plan =
            plan_windows_pipe_read(peek_named_pipe_available(pipe.as_raw_handle()), chunk.len());
        match plan {
            WindowsPipeReadPlan::Wait | WindowsPipeReadPlan::Eof => plan,
            WindowsPipeReadPlan::Read(max_n) => match pipe.read(&mut chunk[..max_n]) {
                Ok(0) => WindowsPipeReadPlan::Eof,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    WindowsPipeReadPlan::Read(n)
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => WindowsPipeReadPlan::Wait,
                Err(err) if err.kind() == ErrorKind::WouldBlock => WindowsPipeReadPlan::Wait,
                Err(_) => WindowsPipeReadPlan::Eof,
            },
        }
    }

    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut stdout_done = stdout.is_none();
    let mut stderr_done = stderr.is_none();
    let mut timed_out = false;
    let mut chunk = [0u8; 4096];

    while !(stdout_done && stderr_done) {
        // deadline 必须在任何可能阻塞的系统调用之前检查，并在 Wait 路径再次校验。
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }

        let mut progress = false;
        if !stdout_done {
            if let Some(out) = stdout.as_mut() {
                match pump_windows_side(out, &mut chunk, &mut stdout_buf) {
                    WindowsPipeReadPlan::Eof => {
                        stdout_done = true;
                        progress = true;
                    }
                    WindowsPipeReadPlan::Read(n) if n > 0 => progress = true,
                    WindowsPipeReadPlan::Read(_) | WindowsPipeReadPlan::Wait => {}
                }
            } else {
                stdout_done = true;
            }
        }
        if !stderr_done {
            if let Some(err_pipe) = stderr.as_mut() {
                match pump_windows_side(err_pipe, &mut chunk, &mut stderr_buf) {
                    WindowsPipeReadPlan::Eof => {
                        stderr_done = true;
                        progress = true;
                    }
                    WindowsPipeReadPlan::Read(n) if n > 0 => progress = true,
                    WindowsPipeReadPlan::Read(_) | WindowsPipeReadPlan::Wait => {}
                }
            } else {
                stderr_done = true;
            }
        }

        if !progress {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                timed_out = true;
                break;
            }
            std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
        }
    }

    // 显式 drop 读端：超时后不再保留任何 reader，也不再进入阻塞 read。
    drop(stdout);
    drop(stderr);
    (stdout_buf, stderr_buf, timed_out)
}

struct TmuxCandidate {
    command: TmuxCommand,
    backend: &'static str,
}

fn tmux_candidates_for_platform(platform: DependencyPlatform) -> Vec<TmuxCandidate> {
    match platform {
        DependencyPlatform::MacOs => vec![
            TmuxCandidate {
                command: TmuxCommand::native("/opt/homebrew/bin/tmux"),
                backend: "native",
            },
            TmuxCandidate {
                command: TmuxCommand::native("/usr/local/bin/tmux"),
                backend: "native",
            },
            TmuxCandidate {
                command: TmuxCommand::native("tmux"),
                backend: "native",
            },
        ],
        DependencyPlatform::Linux => vec![TmuxCandidate {
            command: TmuxCommand::native("tmux"),
            backend: "native",
        }],
        DependencyPlatform::Windows => vec![TmuxCandidate {
            command: TmuxCommand::wsl(),
            backend: "wsl",
        }],
        DependencyPlatform::Unsupported => Vec::new(),
    }
}

fn install_command_preview_for_platform(
    platform: DependencyPlatform,
    available_tools: &[&str],
) -> Option<Vec<String>> {
    match platform {
        DependencyPlatform::MacOs if available_tools.contains(&"brew") => {
            Some(vec!["brew".into(), "install".into(), "tmux".into()])
        }
        DependencyPlatform::Linux if available_tools.contains(&"apt-get") => Some(vec![
            "sudo".into(),
            "apt-get".into(),
            "install".into(),
            "-y".into(),
            "tmux".into(),
        ]),
        DependencyPlatform::Linux if available_tools.contains(&"dnf") => Some(vec![
            "sudo".into(),
            "dnf".into(),
            "install".into(),
            "-y".into(),
            "tmux".into(),
        ]),
        DependencyPlatform::Linux if available_tools.contains(&"pacman") => Some(vec![
            "sudo".into(),
            "pacman".into(),
            "-S".into(),
            "--noconfirm".into(),
            "tmux".into(),
        ]),
        DependencyPlatform::Windows if available_tools.contains(&"wsl.exe") => Some(vec![
            "wsl.exe".into(),
            "--exec".into(),
            "sh".into(),
            "-lc".into(),
            "sudo apt-get update && sudo apt-get install -y tmux".into(),
        ]),
        _ => None,
    }
}

fn current_platform() -> DependencyPlatform {
    #[cfg(target_os = "macos")]
    {
        DependencyPlatform::MacOs
    }
    #[cfg(target_os = "linux")]
    {
        DependencyPlatform::Linux
    }
    #[cfg(target_os = "windows")]
    {
        DependencyPlatform::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        DependencyPlatform::Unsupported
    }
}

fn backend_for_platform(platform: DependencyPlatform) -> &'static str {
    match platform {
        DependencyPlatform::Windows => "wsl",
        DependencyPlatform::MacOs | DependencyPlatform::Linux => "native",
        DependencyPlatform::Unsupported => "unsupported",
    }
}

fn missing_or_unsupported_status(
    install_command_preview: Vec<String>,
    error: Option<String>,
) -> WorkbenchDependencyStatusDto {
    let platform = current_platform();
    let installable = !install_command_preview.is_empty();
    WorkbenchDependencyStatusDto {
        status: if platform == DependencyPlatform::Unsupported {
            WorkbenchDependencyState::Unsupported
        } else {
            WorkbenchDependencyState::Missing
        },
        available: false,
        version: None,
        backend: backend_for_platform(platform).to_string(),
        path: None,
        installable,
        install_command_preview,
        error,
        output: Vec::new(),
        // 真实变更时间由 dependency manager 在写入缓存时按语义状态差决定。
        status_changed_at: String::new(),
    }
}

/// 带硬超时探测 PATH 中的命令是否可执行（非安装、不改 PATH）。
///
/// Business Logic（为什么需要这个函数）:
///     doctor 与 install 预览需要同一套有界探测，避免 WSL/坏 PATH 导致 hang。
///
/// Code Logic（这个函数做什么）:
///     对 program 跑 `--version`，超时或 spawn 失败视为不可用；有 exit code 即视为找到可执行文件。
pub(crate) fn command_exists(program: &str) -> bool {
    let mut command = StdCommand::new(program);
    command.arg("--version");
    match run_std_command_with_timeout(command, PROBE_COMMAND_TIMEOUT) {
        Ok(output) => output.status.success() || output.status.code().is_some(),
        Err(_) => false,
    }
}

/// 可选依赖探测结果（非 mutating）。
///
/// Business Logic（为什么需要这个结构）:
///     doctor 需要把 Git/tmux/WSL/Claude CLI 的存在性映射为 ok/warning/info，
///     且平台不适用时不得伪装成缺失警告。
///
/// Code Logic（这个结构做什么）:
///     available=探测到可用命令；applicable=当前平台相关；version 可选；detail 供 summary。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionalDependencyProbe {
    pub available: bool,
    pub applicable: bool,
    pub version: Option<String>,
    pub detail: String,
}

/// Business Logic（为什么需要这个函数）:
///     doctor 采集必须把剩余全局 deadline 传给 git 探测，避免硬超时后仍留下 git 子进程。
///
/// Code Logic（这个函数做什么）:
///     带 3s 超时执行 `git --version`；成功解析版本字符串，失败标记 unavailable；使用 guarded runner 与可选 overall deadline/guard。
pub fn probe_git_non_mutating_with_budget(
    overall_deadline: Option<Instant>,
    guard: Option<&ProbeRuntimeGuard>,
) -> OptionalDependencyProbe {
    let mut command = StdCommand::new("git");
    command.arg("--version");
    match run_std_command_with_timeout_guarded(
        command,
        PROBE_COMMAND_TIMEOUT,
        overall_deadline,
        guard,
    ) {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            OptionalDependencyProbe {
                available: true,
                applicable: true,
                version: if version.is_empty() {
                    None
                } else {
                    Some(version)
                },
                detail: "git is available".to_string(),
            }
        }
        _ => OptionalDependencyProbe {
            available: false,
            applicable: true,
            version: None,
            detail: "git is missing or not runnable".to_string(),
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor 需要把剩余 deadline/取消信号传入 tmux 探测，防止 hard timeout 后遗留 tmux wrapper。
///
/// Code Logic（这个函数做什么）:
///     使用 guarded runner 调用 `probe_tmux_command_with`（Ready→available；TimedOut/Missing→unavailable）。
pub fn probe_tmux_non_mutating_with_budget(
    overall_deadline: Option<Instant>,
    guard: Option<&ProbeRuntimeGuard>,
) -> OptionalDependencyProbe {
    let outcome = probe_tmux_command_with(current_platform(), |command, timeout| {
        run_std_command_with_timeout_guarded(command, timeout, overall_deadline, guard)
    });
    match outcome {
        TmuxProbeOutcome::Ready(probe) => OptionalDependencyProbe {
            available: true,
            applicable: true,
            version: probe.version,
            detail: "tmux is available".to_string(),
        },
        TmuxProbeOutcome::TimedOut => OptionalDependencyProbe {
            available: false,
            applicable: true,
            version: None,
            detail: "tmux probe timed out".to_string(),
        },
        TmuxProbeOutcome::Missing => OptionalDependencyProbe {
            available: false,
            applicable: true,
            version: None,
            detail: "tmux is missing".to_string(),
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor 在 Windows 上探测 WSL 时必须尊重全局 deadline，避免 hard timeout 后留下 wsl.exe。
///
/// Code Logic（这个函数做什么）:
///     非 Windows 直接 not-applicable；Windows 用 guarded runner 跑 `--status` / `--version`。
pub fn probe_wsl_non_mutating_with_budget(
    overall_deadline: Option<Instant>,
    guard: Option<&ProbeRuntimeGuard>,
) -> OptionalDependencyProbe {
    if current_platform() != DependencyPlatform::Windows {
        return OptionalDependencyProbe {
            available: false,
            applicable: false,
            version: None,
            detail: "WSL is not applicable on this platform".to_string(),
        };
    }
    // 仅探测 wsl.exe 是否存在且能返回版本/状态，绝不 start 发行版或改配置。
    let mut command = StdCommand::new("wsl.exe");
    command.arg("--status");
    match run_std_command_with_timeout_guarded(
        command,
        PROBE_COMMAND_TIMEOUT,
        overall_deadline,
        guard,
    ) {
        Ok(output) if output.status.success() || output.status.code().is_some() => {
            OptionalDependencyProbe {
                available: true,
                applicable: true,
                version: None,
                detail: "WSL is available".to_string(),
            }
        }
        _ => {
            // --status 在部分发行版不可用时回退 --version
            let mut fallback = StdCommand::new("wsl.exe");
            fallback.arg("--version");
            match run_std_command_with_timeout_guarded(
                fallback,
                PROBE_COMMAND_TIMEOUT,
                overall_deadline,
                guard,
            ) {
                Ok(output) if output.status.success() || output.status.code().is_some() => {
                    OptionalDependencyProbe {
                        available: true,
                        applicable: true,
                        version: None,
                        detail: "WSL is available".to_string(),
                    }
                }
                _ => OptionalDependencyProbe {
                    available: false,
                    applicable: true,
                    version: None,
                    detail: "WSL is missing".to_string(),
                },
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     doctor 探测自定义 Claude wrapper 时必须共享全局 deadline，防止 hard timeout 后遗留 wrapper 进程。
///
/// Code Logic（这个函数做什么）:
///     对 cli_path（空则 `claude`）用 guarded runner 执行 `--version`。
pub fn probe_claude_cli_non_mutating_with_budget(
    cli_path: &str,
    overall_deadline: Option<Instant>,
    guard: Option<&ProbeRuntimeGuard>,
) -> OptionalDependencyProbe {
    // 与 github_trending / Prompt 优化一致：打包 GUI PATH 不含 ~/.local/bin 时仍能命中 Claude CLI。
    let program = crate::claude_cli::resolve_cli_path(cli_path);
    let mut command = StdCommand::new(&program);
    command.env("PATH", crate::claude_cli::cli_command_path_env());
    command.arg("--version");
    match run_std_command_with_timeout_guarded(
        command,
        PROBE_COMMAND_TIMEOUT,
        overall_deadline,
        guard,
    ) {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            OptionalDependencyProbe {
                available: true,
                applicable: true,
                version: if version.is_empty() {
                    None
                } else {
                    Some(version)
                },
                detail: "claude CLI is available".to_string(),
            }
        }
        _ => OptionalDependencyProbe {
            available: false,
            applicable: true,
            version: None,
            detail: "claude CLI is missing or not runnable".to_string(),
        },
    }
}

fn output_lines(stdout: &[u8], stderr: &[u8]) -> Vec<String> {
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(stdout));
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&String::from_utf8_lossy(stderr));
    }
    truncate_output_lines(
        combined
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect(),
    )
}

fn truncate_output_lines(lines: Vec<String>) -> Vec<String> {
    let count = lines.len();
    lines
        .into_iter()
        .skip(count.saturating_sub(OUTPUT_LINE_LIMIT))
        .collect()
}

// ---------------------------------------------------------------------------
// 单测：`dependencies_test.rs`（文件名含 test，module-boundary 门禁排除）
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "dependencies_test.rs"]
mod tests;
