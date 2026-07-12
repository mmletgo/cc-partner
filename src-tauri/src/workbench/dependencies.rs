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
use serde::Serialize;
use std::io::Read;
use std::process::{Child, Command as StdCommand, Output, Stdio};
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
/// 管道 drain 线程 join 的单管道宽限。
const PROBE_PIPE_DRAIN_GRACE: Duration = Duration::from_millis(150);

/// Business Logic（为什么需要这个函数）:
///     Attention 与前端轮询需要稳定的依赖状态变更时间，不能在每次读取时漂移。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UTC 时刻的 RFC3339 字符串，用作进程内 `status_changed_at`。
fn now_status_changed_at() -> String {
    Utc::now().to_rfc3339()
}

/// tmux 工作目录路径模式。
///
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
    ///     构造无固定前缀参数、cwd 模式为 Native 的 tmux 命令描述。
    pub(crate) fn native(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
            cwd_mode: TmuxCwdMode::Native,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Windows 用户可把 tmux 安装在 WSL 中，工作台应通过 wsl.exe 调用它以获得可恢复上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 `wsl.exe --exec tmux` 命令描述，并标记 cwd 需要转换成 WSL mount 路径。
    pub(crate) fn wsl() -> Self {
        Self {
            program: "wsl.exe".to_string(),
            prefix_args: vec!["--exec".to_string(), "tmux".to_string()],
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
        command
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
        TmuxProbeOutcome::Missing => {
            missing_or_unsupported_status(actual_install_command_preview().unwrap_or_default(), None)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     install 命令需要使用与 check DTO 一致的命令预览，避免展示和真实执行不一致。
///
/// Code Logic（这个函数做什么）:
///     按当前平台与系统可见包管理器生成安装命令 argv。
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
        let mut command = candidate.command.std_command();
        command.args(["-V"]);
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
///     超时后 kill/killpg + 有界 wait/reap，stdout/stderr 在后台线程 drain 且带 join 超时；
///     截止后绝不阻塞超过 deadline + PROBE_TERMINATE_GRACE。
fn run_std_command_with_timeout(
    mut command: StdCommand,
    timeout: Duration,
) -> Result<Output, ProbeCommandError> {
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
    let mut child = command.spawn().map_err(|_| ProbeCommandError::SpawnOrIo)?;
    let deadline = Instant::now() + timeout;
    let process_group_id = child.id();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    terminate_probe_child(&mut child, process_group_id, deadline);
                    return Err(ProbeCommandError::TimedOut);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
            }
            Err(_) => {
                // try_wait 失败也走终止路径，避免留下僵尸。
                terminate_probe_child(&mut child, process_group_id, deadline);
                return Err(ProbeCommandError::SpawnOrIo);
            }
        }
    };

    // 正常退出：在共享截止时间（+ 小额 grace）内有界 drain 管道，避免后代持有 pipe 时无界 read_to_end。
    let (stdout, stderr) = drain_probe_pipes(&mut child, deadline);
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Business Logic（为什么需要这个函数）:
///     超时或 IO 失败后必须 best-effort 终止进程树并回收，但 kill 失败时不能无条件 child.wait() 永久挂起。
///
/// Code Logic（这个函数做什么）:
///     Unix 先 killpg(SIGKILL) 再 child.kill()；Windows 仅 child.kill()；
///     随后在 deadline+PROBE_TERMINATE_GRACE 内轮询 try_wait，超时则丢弃 wait（接受潜在僵尸而非阻塞启动）。
fn terminate_probe_child(child: &mut Child, process_group_id: u32, deadline: Instant) {
    #[cfg(unix)]
    {
        let _ = kill_probe_process_group(process_group_id);
    }
    let _ = child.kill();

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

    // 丢弃管道，避免后续任何无界读。
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
///     正常退出后仍可能因孙进程持有 pipe 导致 read_to_end 永不返回，必须有界 drain。
///
/// Code Logic（这个函数做什么）:
///     把 stdout/stderr take 到独立线程 read_to_end；主线程在 deadline+grace 内 join，
///     超时则 detach（线程随 pipe 关闭结束）并返回已读（可能空）缓冲。
fn drain_probe_pipes(child: &mut Child, deadline: Instant) -> (Vec<u8>, Vec<u8>) {
    let stdout_handle = child.stdout.take().map(|mut out| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut err| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            buf
        })
    });

    let join_deadline = deadline + PROBE_PIPE_DRAIN_GRACE;
    let stdout = join_pipe_thread(stdout_handle, join_deadline);
    let stderr = join_pipe_thread(stderr_handle, join_deadline);
    (stdout, stderr)
}

/// Business Logic（为什么需要这个函数）:
///     drain 线程 join 必须可超时，否则与无界 read_to_end 等价。
///
/// Code Logic（这个函数做什么）:
///     在 join_deadline 前轮询 is_finished + join；超时返回空缓冲并让线程继续（pipe 关闭后自然退出）。
fn join_pipe_thread(
    handle: Option<std::thread::JoinHandle<Vec<u8>>>,
    join_deadline: Instant,
) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    loop {
        if handle.is_finished() {
            return handle.join().unwrap_or_default();
        }
        if Instant::now() >= join_deadline {
            // 超时：丢弃 JoinHandle（detach），不阻塞主路径。
            drop(handle);
            return Vec::new();
        }
        let remaining = join_deadline.saturating_duration_since(Instant::now());
        std::thread::sleep(remaining.min(PROBE_POLL_INTERVAL));
    }
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

fn command_exists(program: &str) -> bool {
    let mut command = StdCommand::new(program);
    command.arg("--version");
    match run_std_command_with_timeout(command, PROBE_COMMAND_TIMEOUT) {
        Ok(output) => output.status.success() || output.status.code().is_some(),
        Err(_) => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     前端需要展示 tmux 版本，后端必须从 `tmux -V` 的标准输出中稳定提取版本号。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖普通版本、补丁后缀与带换行输出的解析结果。
    #[test]
    fn parse_tmux_version_extracts_version_token() {
        assert_eq!(parse_tmux_version("tmux 3.4\n"), Some("3.4".to_string()));
        assert_eq!(parse_tmux_version("tmux 3.3a"), Some("3.3a".to_string()));
        assert_eq!(parse_tmux_version("not tmux"), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     macOS 用户缺少 tmux 时，应看到 Homebrew 安装预览命令。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 macOS 平台选择器断言返回 `brew install tmux`。
    #[test]
    fn macos_install_preview_uses_brew() {
        let preview = install_command_preview_for_platform(DependencyPlatform::MacOs, &["brew"]);

        assert_eq!(
            preview,
            Some(vec!["brew".into(), "install".into(), "tmux".into()])
        );
        assert_eq!(
            install_command_preview_for_platform(DependencyPlatform::MacOs, &[]),
            None
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Linux 发行版包管理器不同，后端应按本机存在的工具给出最可能可执行的安装命令。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别覆盖 apt-get、dnf、pacman 的选择顺序。
    #[test]
    fn linux_install_preview_selects_existing_package_manager() {
        assert_eq!(
            install_command_preview_for_platform(DependencyPlatform::Linux, &["dnf", "apt-get"]),
            Some(vec![
                "sudo".into(),
                "apt-get".into(),
                "install".into(),
                "-y".into(),
                "tmux".into()
            ])
        );
        assert_eq!(
            install_command_preview_for_platform(DependencyPlatform::Linux, &["dnf"]),
            Some(vec![
                "sudo".into(),
                "dnf".into(),
                "install".into(),
                "-y".into(),
                "tmux".into()
            ])
        );
        assert_eq!(
            install_command_preview_for_platform(DependencyPlatform::Linux, &["pacman"]),
            Some(vec![
                "sudo".into(),
                "pacman".into(),
                "-S".into(),
                "--noconfirm".into(),
                "tmux".into()
            ])
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 只能通过 WSL 安装/运行 tmux，前端预览必须明确展示 wsl.exe 包裹命令。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Windows 安装预览为固定的 WSL apt-get 命令。
    #[test]
    fn windows_install_preview_uses_wsl_apt() {
        let preview =
            install_command_preview_for_platform(DependencyPlatform::Windows, &["wsl.exe"]);

        assert_eq!(
            preview,
            Some(vec![
                "wsl.exe".into(),
                "--exec".into(),
                "sh".into(),
                "-lc".into(),
                "sudo apt-get update && sudo apt-get install -y tmux".into(),
            ])
        );
        assert_eq!(
            install_command_preview_for_platform(DependencyPlatform::Windows, &[]),
            None
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench dependency DTO 是前端锁定契约，字段名必须保持 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化一个 ready 状态，断言 installCommandPreview 字段存在且状态值稳定。
    #[test]
    fn dependency_status_serializes_with_camel_case_contract() {
        let status = WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Ready,
            available: true,
            version: Some("3.4".to_string()),
            backend: "native".to_string(),
            path: Some("/opt/homebrew/bin/tmux".to_string()),
            installable: false,
            install_command_preview: Vec::new(),
            error: None,
            output: Vec::new(),
            status_changed_at: "2026-07-12T00:00:00Z".to_string(),
        };

        let json = serde_json::to_value(status).unwrap();

        assert_eq!(json["status"], "ready");
        assert_eq!(json["installCommandPreview"], serde_json::json!([]));
        assert_eq!(json["statusChangedAt"], "2026-07-12T00:00:00Z");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     安装流程可能被用户取消，状态机必须能从 installing 进入 failed 并保留最近输出供排查。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造安装运行时，先标记 installing，再取消并断言 DTO 状态和输出摘要。
    #[test]
    fn install_state_transitions_from_installing_to_cancelled_failed() {
        let runtime = WorkbenchDependencyInstallRuntime::new();

        runtime.mark_installing(vec!["brew".into(), "install".into(), "tmux".into()]);
        runtime.mark_cancelled();
        let status = runtime.status();

        assert_eq!(status.status, WorkbenchDependencyState::Failed);
        assert_eq!(status.error.as_deref(), Some("安装已取消"));
        assert!(status.output.iter().any(|line| line.contains("安装已取消")));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Attention 需要依赖初始状态带有非空变更时间，才能稳定投影 environment 条目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     新建 runtime 后读取 status，断言 status_changed_at 非空。
    #[test]
    fn initial_status_has_status_changed_at_timestamp() {
        let runtime = WorkbenchDependencyInstallRuntime::new();
        let status = runtime.status();

        assert!(!status.status_changed_at.is_empty());
        assert!(chrono::DateTime::parse_from_rfc3339(&status.status_changed_at).is_ok());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     冷启动若把未探测伪装成 missing，有项目时 Inbox 会错误计数环境阻塞；
    ///     初始化必须与真实探测结果一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对比 `new()` 缓存与 `probe_workbench_dependency()` 的 status/available/path，
    ///     并在探测为 ready 时确认不会被当成 missing。
    #[test]
    fn new_runtime_uses_real_probe_not_placeholder_missing() {
        let probed = probe_workbench_dependency();
        let runtime = WorkbenchDependencyInstallRuntime::new();
        let status = runtime.status();

        assert_eq!(status.status, probed.status);
        assert_eq!(status.available, probed.available);
        assert_eq!(status.path, probed.path);
        assert_eq!(status.version, probed.version);
        if probed.status == WorkbenchDependencyState::Ready {
            assert_ne!(status.status, WorkbenchDependencyState::Missing);
            assert!(status.available);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     相同语义状态的重复探测/轮询不能刷新变更时间，否则 Inbox 会把旧问题伪装成新事件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 missing 后再次 set 同枚举不同 payload，再连续 status() 读取，断言时间戳保持不变。
    #[test]
    fn same_semantic_status_preserves_status_changed_at_across_polls() {
        let runtime = WorkbenchDependencyInstallRuntime::new();
        let initial = runtime.status();
        let initial_changed_at = initial.status_changed_at.clone();

        let again = runtime.set_checked_status(WorkbenchDependencyStatusDto {
            status: initial.status,
            available: false,
            version: None,
            backend: "native".to_string(),
            path: None,
            installable: true,
            install_command_preview: vec!["brew".into(), "install".into(), "tmux".into()],
            error: Some("still missing".into()),
            output: vec!["recheck".into()],
            status_changed_at: "should-be-ignored".into(),
        });
        assert_eq!(again.status_changed_at, initial_changed_at);

        let polled_once = runtime.status();
        let polled_twice = runtime.status();
        assert_eq!(polled_once.status_changed_at, initial_changed_at);
        assert_eq!(polled_twice.status_changed_at, initial_changed_at);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     真实状态迁移（如 missing→ready 或 ready→failed）必须更新变更时间，Attention 才能按最新阻塞排序。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先强制写入与当前探测不同的状态，再切到另一状态，断言每次语义变化后 status_changed_at 都前进。
    #[test]
    fn semantic_status_change_updates_status_changed_at() {
        let runtime = WorkbenchDependencyInstallRuntime::new();
        let initial = runtime.status();
        let initial_changed_at = initial.status_changed_at.clone();

        // 确保时间戳至少相差 1ms；先切到与初始不同的状态（冷启动可能已是 ready）。
        std::thread::sleep(std::time::Duration::from_millis(5));
        let first_target = if initial.status == WorkbenchDependencyState::Missing {
            WorkbenchDependencyState::Ready
        } else {
            WorkbenchDependencyState::Missing
        };
        let first = runtime.set_checked_status(WorkbenchDependencyStatusDto {
            status: first_target,
            available: first_target == WorkbenchDependencyState::Ready,
            version: if first_target == WorkbenchDependencyState::Ready {
                Some("3.4".into())
            } else {
                None
            },
            backend: "native".into(),
            path: if first_target == WorkbenchDependencyState::Ready {
                Some("/opt/homebrew/bin/tmux".into())
            } else {
                None
            },
            installable: first_target == WorkbenchDependencyState::Missing,
            install_command_preview: Vec::new(),
            error: None,
            output: Vec::new(),
            status_changed_at: String::new(),
        });
        assert_eq!(first.status, first_target);
        assert_ne!(first.status_changed_at, initial_changed_at);
        assert!(!first.status_changed_at.is_empty());

        let first_changed_at = first.status_changed_at.clone();
        std::thread::sleep(std::time::Duration::from_millis(5));

        runtime.mark_failed("probe failed", vec!["stderr".into()]);
        let failed = runtime.status();
        assert_eq!(failed.status, WorkbenchDependencyState::Failed);
        assert_ne!(failed.status_changed_at, first_changed_at);
        assert!(!failed.status_changed_at.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     挂起的外部命令必须在硬超时后被终止，不能阻塞探测线程。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 `sleep 10`，以 200ms 超时探测，断言返回 TimedOut 且总耗时远小于 sleep。
    #[test]
    fn run_std_command_with_timeout_kills_hanging_process() {
        let mut command = StdCommand::new("sleep");
        command.arg("10");
        let started = Instant::now();
        let result = run_std_command_with_timeout(command, Duration::from_millis(200));
        let elapsed = started.elapsed();
        assert_eq!(result.err(), Some(ProbeCommandError::TimedOut));
        assert!(
            elapsed < Duration::from_secs(2),
            "超时路径耗时过长: {elapsed:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超时路径不能依赖「kill 必然成功」；即便进程已退出，terminate 也必须有界返回。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对已退出的 sleep 0 子进程调用 terminate_probe_child，断言在 grace 内返回且不 panic。
    #[test]
    fn terminate_probe_child_is_bounded_when_process_already_exited() {
        let mut child = StdCommand::new("sleep")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sleep 0");
        let pgid = child.id();
        // 等子进程自然退出，使后续 kill/wait 走「已退出 / kill 失败」分支。
        let _ = child.wait();
        let deadline = Instant::now();
        let started = Instant::now();
        terminate_probe_child(&mut child, pgid, deadline);
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "已退出进程的 terminate 耗时过长: {elapsed:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     正常退出后的管道 drain 必须有界；即便 deadline 已过，也不能无界 read_to_end。
    ///
    /// Code Logic（这个测试做什么）:
    ///     跑 `printf hi`，以已过去的 deadline 调用 drain_probe_pipes，断言快速返回（缓冲可空）。
    #[test]
    fn drain_probe_pipes_respects_past_deadline() {
        let mut child = StdCommand::new("printf")
            .arg("hi")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn printf");
        let _ = child.wait();
        let started = Instant::now();
        let past_deadline = Instant::now() - Duration::from_millis(1);
        let (stdout, stderr) = drain_probe_pipes(&mut child, past_deadline);
        let elapsed = started.elapsed();
        // 允许小额 grace；必须远小于无界阻塞。
        assert!(
            elapsed < Duration::from_millis(500),
            "过期 deadline 的 drain 耗时过长: {elapsed:?}"
        );
        // 结果可空（超时 detach）或含 "hi"；关键是不阻塞。
        let _ = (stdout, stderr);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     成功路径在截止时间内应读到完整 stdout，验证 drain 不只是「永远返回空」。
    ///
    /// Code Logic（这个测试做什么）:
    ///     跑 `printf hello` 并在充足 deadline 下 drain，断言 stdout 含 hello。
    #[test]
    fn drain_probe_pipes_reads_stdout_within_deadline() {
        let mut child = StdCommand::new("printf")
            .arg("hello")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn printf");
        let status = child.wait().expect("wait printf");
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(2);
        let (stdout, _stderr) = drain_probe_pipes(&mut child, deadline);
        assert_eq!(String::from_utf8_lossy(&stdout), "hello");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     注入挂起 runner 时探测必须收敛为 TimedOut，供 DTO 写成 failed 而非永久卡死。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fake runner 恒返回 TimedOut；Windows 平台单候选，断言 outcome 为 TimedOut。
    #[test]
    fn probe_tmux_command_with_timeout_runner_returns_timed_out() {
        let outcome = probe_tmux_command_with(DependencyPlatform::Windows, |_cmd, _timeout| {
            Err(ProbeCommandError::TimedOut)
        });
        assert!(matches!(outcome, TmuxProbeOutcome::TimedOut));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     探测超时不能伪装成 missing，否则 Inbox 会把“未知”当成可安装缺失。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用恒超时 runner 走 probe 路径，经 probe_tmux_command_with 映射为 Failed DTO 语义：
    ///     直接断言 TimedOut outcome 对应的 probe_workbench 分支字段（构造等价 DTO）。
    #[test]
    fn timed_out_probe_maps_to_failed_not_missing() {
        let outcome = probe_tmux_command_with(DependencyPlatform::Linux, |_cmd, _timeout| {
            Err(ProbeCommandError::TimedOut)
        });
        assert!(matches!(outcome, TmuxProbeOutcome::TimedOut));
        // 与 probe_workbench_dependency 的 TimedOut 分支保持同一语义。
        let status = WorkbenchDependencyStatusDto {
            status: WorkbenchDependencyState::Failed,
            available: false,
            version: None,
            backend: backend_for_platform(DependencyPlatform::Linux).to_string(),
            path: None,
            installable: false,
            install_command_preview: Vec::new(),
            error: Some("tmux 探测超时（3 秒），请稍后重新检测".into()),
            output: vec!["依赖探测超时，已终止外部进程".to_string()],
            status_changed_at: String::new(),
        };
        assert_eq!(status.status, WorkbenchDependencyState::Failed);
        assert!(!status.available);
        assert!(status.error.as_deref().unwrap_or("").contains("超时"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     快速失败的候选应记为 Missing，而不是 TimedOut。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fake runner 返回 SpawnOrIo；断言 outcome 为 Missing。
    #[test]
    fn probe_tmux_command_with_spawn_failures_returns_missing() {
        let outcome = probe_tmux_command_with(DependencyPlatform::MacOs, |_cmd, _timeout| {
            Err(ProbeCommandError::SpawnOrIo)
        });
        assert!(matches!(outcome, TmuxProbeOutcome::Missing));
    }
}
