//! workbench/sessions.rs — 工作台本机 PTY 会话注册表
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台允许用户在同一项目下开启多个本机项目终端，用户希望应用重启后终端 tab 与可重连上下文仍可恢复。
//!
//! Code Logic（这个模块做什么）:
//!     使用 portable-pty 创建 PTY；macOS/Linux 原生 tmux、Windows WSL tmux 可承载真实 shell 上下文，应用重启后重新 attach。
//!     内存保存运行期句柄，通过 Tauri event 推送终端输出和状态变化。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::models::{WorkbenchProjectRow, WorkbenchSessionDto, WorkbenchSessionRow};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const DEFAULT_COLS: u16 = 98;
const DEFAULT_ROWS: u16 = 32;
const MIN_TERMINAL_COLS: u16 = 20;
const MIN_TERMINAL_ROWS: u16 = 6;
const RAW_PTY_BACKEND: &str = "pty";
const TMUX_BACKEND: &str = "tmux";
#[cfg(windows)]
const FALLBACK_TERMINAL_COMMAND: &str = "cmd.exe";
#[cfg(not(windows))]
const FALLBACK_TERMINAL_COMMAND: &str = "/bin/sh";

/// tmux 工作目录路径模式。
///
/// Business Logic（为什么需要这个枚举）:
///     Windows 上的 tmux 运行在 WSL 内部，不能直接识别宿主 Windows 盘符路径。
///
/// Code Logic（这个枚举做什么）:
///     标记 tmux 命令应使用原生项目路径，还是先把 Windows 项目路径转换为 WSL mount 路径。
#[derive(Debug, Clone, PartialEq, Eq)]
enum TmuxCwdMode {
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
struct TmuxCommand {
    program: String,
    prefix_args: Vec<String>,
    cwd_mode: TmuxCwdMode,
}

impl TmuxCommand {
    /// Business Logic（为什么需要这个函数）:
    ///     macOS/Linux 上的 tmux 可以直接用原生命令执行，并使用项目的原生文件系统路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造无固定前缀参数、cwd 模式为 Native 的 tmux 命令描述。
    fn native(program: impl Into<String>) -> Self {
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
    fn wsl() -> Self {
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
    fn std_command(&self) -> StdCommand {
        let mut command = StdCommand::new(&self.program);
        command.args(&self.prefix_args);
        command
    }

    /// Business Logic（为什么需要这个函数）:
    ///     PTY attach 需要通过 portable-pty 的 CommandBuilder 启动，并复用 tmux 命令前缀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 CommandBuilder，并逐个追加固定前缀参数。
    fn command_builder(&self) -> CommandBuilder {
        let mut command = CommandBuilder::new(&self.program);
        command.args(self.prefix_args.iter().map(String::as_str));
        command
    }

    /// Business Logic（为什么需要这个函数）:
    ///     创建 tmux session 时，`-c` 工作目录必须是 tmux 所在环境可识别的路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 模式原样返回项目路径；WSL 模式把 Windows 盘符路径转换为 `/mnt/<drive>/...`。
    fn project_cwd(&self, project_path: &str) -> Result<String, AppError> {
        match self.cwd_mode {
            TmuxCwdMode::Native => Ok(project_path.to_string()),
            TmuxCwdMode::Wsl => windows_path_to_wsl_path(project_path).ok_or_else(|| {
                AppError::generic(format!("项目路径无法转换为 WSL 路径: {project_path}"))
            }),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     WSL 内的 tmux 应启动 Linux 默认 shell，而不能把 Windows 的 cmd.exe 当作 Linux 命令执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Native 模式返回传入 shell 命令；WSL 模式返回 None，让 tmux 使用 WSL 用户默认 shell。
    fn shell_command_for_new_session<'a>(&self, shell_command: &'a str) -> Option<&'a str> {
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
    fn display_command_for_session(&self, session_name: &str, shell_command: &str) -> String {
        match self.cwd_mode {
            TmuxCwdMode::Native => shell_command.to_string(),
            TmuxCwdMode::Wsl => {
                let mut parts = Vec::with_capacity(self.prefix_args.len() + 4);
                parts.push(self.program.clone());
                parts.extend(self.prefix_args.clone());
                parts.push("attach-session".to_string());
                parts.push("-t".to_string());
                parts.push(session_name.to_string());
                parts.join(" ")
            }
        }
    }
}

/// 工作台终端输出事件 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     前端 xterm 需要按会话接收增量输出，并用 seq 维持调试和乱序排查能力。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 camelCase Tauri event payload，包含会话 ID、输出块、序号和毫秒时间戳。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutputEvent {
    session_id: String,
    chunk: String,
    seq: u64,
    ts: i64,
}

/// 工作台终端状态事件 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要知道会话何时进入运行、退出或断开状态，以更新工作台右侧状态和终端 tab。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 camelCase Tauri event payload，包含状态和可选退出码。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalStatusEvent {
    session_id: String,
    status: String,
    exit_code: Option<i32>,
    ts: i64,
}

/// 工作台终端 UTF-8 流式解码器。
///
/// Business Logic（为什么需要这个结构体）:
///     终端会输出中文、符号和交互式程序文本，PTY read 可能把一个 UTF-8 字符拆到两个 chunk。
///
/// Code Logic（这个结构体做什么）:
///     保存上次 chunk 末尾未完成的字节序列，下次 decode 时先拼回去；真实非法字节仍输出替换符。
#[derive(Debug, Default)]
struct TerminalUtf8Decoder {
    pending: Vec<u8>,
}

impl TerminalUtf8Decoder {
    /// Business Logic（为什么需要这个函数）:
    ///     PTY reader 每个会话启动时都需要一个新的解码状态容器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回没有 pending 字节的流式 UTF-8 解码器。
    fn new() -> Self {
        Self::default()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终端输出事件必须保持文本完整，否则前端 xterm 会显示 �，影响命令行状态栏阅读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将新字节与 pending 拼接后解码；遇到末尾不完整 UTF-8 时暂存，遇到非法字节时输出替换符。
    fn decode(&mut self, bytes: &[u8]) -> String {
        if self.pending.is_empty() {
            return decode_utf8_chunk(bytes, &mut self.pending);
        }

        let mut combined = Vec::with_capacity(self.pending.len() + bytes.len());
        combined.append(&mut self.pending);
        combined.extend_from_slice(bytes);
        decode_utf8_chunk(&combined, &mut self.pending)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     PTY 关闭前如果仍有残留字节，前端应收到可诊断的占位文本而不是静默丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     取出 pending 并用 lossy 解码；没有 pending 时返回 None。
    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let pending = std::mem::take(&mut self.pending);
        Some(String::from_utf8_lossy(&pending).into_owned())
    }
}

/// PTY 进程资源。
///
/// Business Logic（为什么需要这个枚举）:
///     真实会话需要持有 PTY 资源；单元测试只验证 registry 纯内存行为，不应启动真实 CLI。
///
/// Code Logic（这个枚举做什么）:
///     区分真实 PTY 句柄与测试 fake 会话，让 list/filter/rename/close 可在无 PTY 环境下测试。
enum SessionProcess {
    Pty {
        master: Box<dyn portable_pty::MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
    },
    #[allow(dead_code)]
    Fake,
}

/// 工作台终端会话运行态句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     每个会话需要同时保存前端展示 DTO 和可操作的 PTY 进程资源。
///
/// Code Logic（这个结构体做什么）:
///     将持久化 row 快照与 writer/master/child 聚合到单个 Mutex 保护的对象中，保证输入、resize、close 串行访问。
struct WorkbenchSessionHandle {
    row: WorkbenchSessionRow,
    process: SessionProcess,
}

/// Business Logic（为什么需要这个函数）:
///     工作台终端首屏需要按前端真实可见尺寸启动，避免交互式程序先按默认列宽绘制后错位。
///
/// Code Logic（这个函数做什么）:
///     对前端传入的可选 cols/rows 做下限裁剪；缺失时回退默认 PTY 尺寸。
fn initial_terminal_size(cols: Option<u16>, rows: Option<u16>) -> (u16, u16) {
    (
        cols.map(|value| value.max(MIN_TERMINAL_COLS))
            .unwrap_or(DEFAULT_COLS),
        rows.map(|value| value.max(MIN_TERMINAL_ROWS))
            .unwrap_or(DEFAULT_ROWS),
    )
}

/// Business Logic（为什么需要这个函数）:
///     工作台打开终端只应进入项目根目录的普通 shell，用户自己决定是否在里面运行 Claude Code 或其他命令。
///
/// Code Logic（这个函数做什么）:
///     按平台读取系统默认 shell 环境变量；缺失或不可用时回退到跨平台默认 shell 命令。
fn default_terminal_command() -> String {
    #[cfg(windows)]
    {
        default_terminal_command_from_env(std::env::var_os("ComSpec"))
    }
    #[cfg(not(windows))]
    {
        default_terminal_command_from_env(std::env::var_os("SHELL"))
    }
}

/// Business Logic（为什么需要这个函数）:
///     shell 解析逻辑需要可单测，避免工作台终端再次被误改为固定启动 Claude Code。
///
/// Code Logic（这个函数做什么）:
///     将环境变量 OsString 转成非空 UTF-8 字符串；无法转换或为空时使用平台 fallback。
fn default_terminal_command_from_env(command: Option<OsString>) -> String {
    command
        .and_then(|value| value.into_string().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| FALLBACK_TERMINAL_COMMAND.to_string())
}

/// Business Logic（为什么需要这个函数）:
///     Windows 宿主路径需要传给 WSL 内的 tmux，必须转换成 Linux 可识别的 mount 路径。
///
/// Code Logic（这个函数做什么）:
///     支持 `C:\dir`、`C:/dir` 和 `\\?\C:\dir` 三类常见绝对路径；UNC/相对路径返回 None。
fn windows_path_to_wsl_path(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    if path.starts_with('/') {
        return Some(path.to_string());
    }

    let without_extended_prefix = path.strip_prefix(r"\\?\").unwrap_or(path);
    if let Some(linux_path) = wsl_unc_path_to_linux_path(without_extended_prefix) {
        return Some(linux_path);
    }

    let bytes = without_extended_prefix.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' {
        return None;
    }

    let drive = bytes[0] as char;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    if bytes[2] != b'\\' && bytes[2] != b'/' {
        return None;
    }

    let rest = without_extended_prefix[3..].trim_start_matches(['\\', '/']);
    let rest = rest.replace('\\', "/");
    if rest.is_empty() {
        Some(format!("/mnt/{}", drive.to_ascii_lowercase()))
    } else {
        Some(format!("/mnt/{}/{}", drive.to_ascii_lowercase(), rest))
    }
}

/// Business Logic（为什么需要这个函数）:
///     Windows 用户可能通过资源管理器选择 WSL 文件系统路径，形式是 `\\wsl$\<distro>\...`。
///
/// Code Logic（这个函数做什么）:
///     识别 `\\wsl$\distro\path` 和 `\\wsl.localhost\distro\path`，丢弃 distro 段并转为 Linux 绝对路径。
fn wsl_unc_path_to_linux_path(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let prefix = if lower.starts_with(r"\\wsl$\") {
        r"\\wsl$\"
    } else if lower.starts_with(r"\\wsl.localhost\") {
        r"\\wsl.localhost\"
    } else {
        return None;
    };

    let rest = &path[prefix.len()..];
    if rest.is_empty() {
        return None;
    }
    let first_separator = rest.find(['\\', '/']);
    let path_in_distro = match first_separator {
        Some(index) => &rest[index + 1..],
        None => "",
    };
    let path_in_distro = path_in_distro
        .trim_start_matches(['\\', '/'])
        .replace('\\', "/");
    if path_in_distro.is_empty() {
        Some("/".to_string())
    } else {
        Some(format!("/{path_in_distro}"))
    }
}

/// Business Logic（为什么需要这个函数）:
///     重启应用后要继续已有终端上下文，普通 PTY 无法跨进程存活；可借助 tmux 保留 shell 进程。
///
/// Code Logic（这个函数做什么）:
///     Windows 探测 WSL 内的 `tmux`；Unix 上依次探测 PATH 与常见 Homebrew/Linux tmux 路径，返回命令描述。
fn available_tmux_command() -> Option<TmuxCommand> {
    #[cfg(windows)]
    {
        let candidate = TmuxCommand::wsl();
        if candidate
            .std_command()
            .args(["-V"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            Some(candidate)
        } else {
            None
        }
    }
    #[cfg(not(windows))]
    {
        let candidates = [
            "tmux",
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/usr/bin/tmux",
        ];
        candidates
            .iter()
            .find(|candidate| {
                StdCommand::new(*candidate)
                    .arg("-V")
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false)
            })
            .map(|candidate| TmuxCommand::native(*candidate))
    }
}

/// Business Logic（为什么需要这个函数）:
///     每个工作台终端 tab 需要稳定映射到一个 tmux session，才能跨应用重启重新 attach。
///
/// Code Logic（这个函数做什么）:
///     用 session_id 派生 tmux session 名称，并去掉 UUID 中的连字符减少 shell 工具兼容风险。
fn tmux_session_name(session_id: &str) -> String {
    format!("cc-partner-{}", session_id.replace('-', ""))
}

/// Business Logic（为什么需要这个函数）:
///     恢复会话时需要判断原 tmux session 是否仍存在，存在则 attach，不存在则按同一名称重建。
///
/// Code Logic（这个函数做什么）:
///     执行 `tmux has-session -t <name>`，返回 status 是否成功。
fn tmux_has_session(tmux: &TmuxCommand, session_name: &str) -> bool {
    tmux.std_command()
        .args(["has-session", "-t", session_name])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     新建或恢复终端时，如果 tmux session 不存在，需要在项目根目录中创建它以承载真实 shell 上下文。
///
/// Code Logic（这个函数做什么）:
///     执行 `tmux new-session -d -s <name> -c <cwd> <shell>`；失败转为 AppError 供上层 fallback。
fn create_tmux_session(
    tmux: &TmuxCommand,
    session_name: &str,
    cwd: &str,
    shell_command: &str,
) -> Result<(), AppError> {
    let tmux_cwd = tmux.project_cwd(cwd)?;
    let mut command = tmux.std_command();
    command.args(["new-session", "-d", "-s", session_name, "-c", &tmux_cwd]);
    if let Some(shell_command) = tmux.shell_command_for_new_session(shell_command) {
        command.arg(shell_command);
    }

    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if detail.is_empty() {
            "未知错误".to_string()
        } else {
            detail
        };
        Err(AppError::generic(format!("创建 tmux 会话失败: {message}")))
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户关闭终端 tab 时，如果该 tab 使用 tmux 承载上下文，应销毁对应 tmux session，避免后台残留。
///
/// Code Logic（这个函数做什么）:
///     执行 `tmux kill-session -t <name>`；session 已不存在视为成功，其他错误仅记录 debug。
pub fn kill_persisted_backend(row: &WorkbenchSessionRow) {
    if row.backend != TMUX_BACKEND {
        return;
    }
    let Some(session_name) = row.backend_id.as_deref() else {
        return;
    };
    let Some(tmux) = available_tmux_command() else {
        return;
    };
    let output = tmux
        .std_command()
        .args(["kill-session", "-t", session_name])
        .output();
    if let Err(error) = output {
        tracing::debug!("销毁工作台 tmux 会话失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     portable-pty 启动命令需要统一构造，普通 PTY 和 tmux attach 仅命令及参数不同。
///
/// Code Logic（这个函数做什么）:
///     根据 row.backend/backend_id 构造 CommandBuilder；tmux 行为是 attach，普通 PTY 直接启动 shell。
fn command_builder_for_row(row: &WorkbenchSessionRow) -> CommandBuilder {
    if row.backend == TMUX_BACKEND {
        if let (Some(tmux), Some(session_name)) =
            (available_tmux_command(), row.backend_id.as_deref())
        {
            let mut cmd = tmux.command_builder();
            cmd.args(["attach-session", "-t", session_name]);
            return cmd;
        }
    }
    CommandBuilder::new(row.command.clone())
}

/// Business Logic（为什么需要这个函数）:
///     用户关闭终端或应用退出清理时，终端子进程可能已经自然退出并被系统回收，此时 kill 返回 No such process 不应打扰用户。
///
/// Code Logic（这个函数做什么）:
///     将底层 child.kill() 的结果归一化；进程已不存在视为 Ok，其他 IO 错误继续转换为 AppError。
fn normalize_terminal_kill_result(result: std::io::Result<()>) -> Result<(), AppError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if is_terminal_process_already_gone(&error) => Ok(()),
        Err(error) => Err(AppError::from(error)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     不同平台或 portable-pty 后端对“进程不存在”可能给出 ErrorKind::NotFound 或原始 ESRCH 码。
///
/// Code Logic（这个函数做什么）:
///     检查 IO 错误是否表示目标进程已不存在；macOS/Linux 的 ESRCH 是 raw os error 3。
fn is_terminal_process_already_gone(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound) || error.raw_os_error() == Some(3)
}

/// Business Logic（为什么需要这个函数）:
///     PTY reader 只能拿到字节流，工作台事件需要发送 UTF-8 字符串给前端 xterm。
///
/// Code Logic（这个函数做什么）:
///     从给定字节切片中尽可能解出完整 UTF-8 文本，把末尾不完整序列写入 pending。
fn decode_utf8_chunk(bytes: &[u8], pending: &mut Vec<u8>) -> String {
    let mut output = String::new();
    let mut offset = 0;

    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(valid) => {
                output.push_str(valid);
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    let valid = std::str::from_utf8(&bytes[offset..offset + valid_up_to])
                        .expect("valid_up_to guarantees this prefix is valid UTF-8");
                    output.push_str(valid);
                    offset += valid_up_to;
                }

                match error.error_len() {
                    Some(invalid_len) => {
                        output.push('\u{FFFD}');
                        offset += invalid_len;
                    }
                    None => {
                        pending.extend_from_slice(&bytes[offset..]);
                        break;
                    }
                }
            }
        }
    }

    output
}

/// 工作台 PTY 会话注册表。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台会话的元数据持久化在 SQLite，但多个命令仍需要按 session_id 查找并操作当前 PTY attach。
///
/// Code Logic（这个结构体做什么）:
///     用 HashMap 保存 session_id 到会话句柄的映射；外层 Arc 允许后台读写线程更新状态。
pub struct WorkbenchSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
}

impl WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的工作台会话注册表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 HashMap，并包裹 Arc<Mutex<_>> 供命令和后台线程共享。
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端工作台需要列出全部会话，或只列出某个项目下的会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取内存 registry，按可选 project_id 过滤并克隆 DTO 返回。
    pub fn list(&self, project_id: Option<&str>) -> Vec<WorkbenchSessionDto> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .values()
            .filter_map(|handle| {
                let handle = handle.lock().expect("workbench session 锁中毒");
                if project_id
                    .map(|id| handle.row.project_id == id)
                    .unwrap_or(true)
                {
                    Some(handle.row.to_dto())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list 命令从 SQLite 恢复历史会话前，需要避免重复 attach 已在运行期 registry 的会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     检查内存 HashMap 是否已有 session_id。
    pub fn contains(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .contains_key(session_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在工作台中创建本机终端时，需要在项目根目录中启动普通 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 portable-pty pair，cwd 指向项目路径，spawn 系统 shell，并启动输出与退出监听线程。
    pub fn create(
        &self,
        app: AppHandle,
        project: WorkbenchProjectRow,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let (cols, rows) = initial_terminal_size(initial_cols, initial_rows);
        let terminal_command = default_terminal_command();
        let (backend, backend_id, command) = match available_tmux_command() {
            Some(tmux) => {
                let tmux_id = tmux_session_name(&session_id);
                match create_tmux_session(&tmux, &tmux_id, &project.path, &terminal_command) {
                    Ok(()) => {
                        let display_command =
                            tmux.display_command_for_session(&tmux_id, &terminal_command);
                        (TMUX_BACKEND.to_string(), Some(tmux_id), display_command)
                    }
                    Err(error) => {
                        tracing::warn!("工作台 tmux 后端不可用，回退普通 PTY: {error}");
                        (RAW_PTY_BACKEND.to_string(), None, terminal_command.clone())
                    }
                }
            }
            None => (RAW_PTY_BACKEND.to_string(), None, terminal_command.clone()),
        };
        let row = WorkbenchSessionRow {
            id: session_id.clone(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            command,
            status: "running".to_string(),
            cols,
            rows,
            started_at: now,
            exited_at: None,
            exit_code: None,
            backend,
            backend_id,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        self.spawn_row(app, row, &project.path)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用重启后，持久化的终端 tab 需要重新绑定运行期 PTY；tmux 后端可继续原 shell 上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据持久化 row.backend 恢复 tmux session 或回退普通 PTY，然后启动 reader/exit watcher。
    pub fn restore(
        &self,
        app: AppHandle,
        project: WorkbenchProjectRow,
        mut row: WorkbenchSessionRow,
    ) -> Result<WorkbenchSessionRow, AppError> {
        if row.backend == TMUX_BACKEND {
            if let Some(tmux) = available_tmux_command() {
                let session_name = row
                    .backend_id
                    .clone()
                    .unwrap_or_else(|| tmux_session_name(&row.id));
                if !tmux_has_session(&tmux, &session_name) {
                    if let Err(error) =
                        create_tmux_session(&tmux, &session_name, &project.path, &row.command)
                    {
                        tracing::warn!("恢复工作台 tmux 会话失败，回退普通 PTY: {error}");
                        row.backend = RAW_PTY_BACKEND.to_string();
                        row.backend_id = None;
                        row.command = default_terminal_command();
                    }
                }
                if row.backend == TMUX_BACKEND {
                    row.backend_id = Some(session_name);
                }
            } else {
                tracing::warn!("恢复工作台终端时未找到 tmux，回退普通 PTY");
                row.backend = RAW_PTY_BACKEND.to_string();
                row.backend_id = None;
                row.command = default_terminal_command();
            }
        }

        row.status = "running".to_string();
        row.exited_at = None;
        row.exit_code = None;
        row.updated_at = chrono::Utc::now().to_rfc3339();
        self.spawn_row(app, row, &project.path)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建和恢复终端最终都要启动一个 PTY 客户端并注册输出/退出监听。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 row 中的命令/后端信息构造 CommandBuilder，spawn 子进程并写入内存 registry。
    fn spawn_row(
        &self,
        app: AppHandle,
        row: WorkbenchSessionRow,
        project_path: &str,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let session_id = row.id.clone();
        let cols = row.cols;
        let rows = row.rows;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| AppError::generic(format!("创建 PTY 失败: {error}")))?;
        let mut cmd = command_builder_for_row(&row);
        cmd.cwd(PathBuf::from(project_path));
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|error| AppError::generic(format!("启动工作台终端失败: {error}")))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| AppError::generic(format!("创建 PTY reader 失败: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| AppError::generic(format!("创建 PTY writer 失败: {error}")))?;

        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: row.clone(),
            process: SessionProcess::Pty {
                master: pair.master,
                writer,
                child,
            },
        }));
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .insert(session_id.clone(), handle.clone());

        emit_status(&app, &session_id, "running", None);
        spawn_reader_thread(app.clone(), session_id.clone(), reader);
        spawn_exit_watcher(app, self.sessions.clone(), session_id.clone(), handle);

        Ok(row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在 xterm 中输入字符时，需要把输入发送给对应 PTY。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查找会话 writer，写入 UTF-8 字节并 flush；会话不存在或非运行态返回错误。
    pub fn write_input(&self, session_id: &str, data: &str) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        if handle.row.status != "running" {
            return Err(AppError::generic("工作台会话未运行"));
        }
        match &mut handle.process {
            SessionProcess::Pty { writer, .. } => {
                writer.write_all(data.as_bytes())?;
                writer.flush()?;
                Ok(())
            }
            SessionProcess::Fake => Ok(()),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端终端容器尺寸变化时，子进程需要收到新的 PTY 行列数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 row 尺寸，并调用 MasterPty::resize 通知底层 PTY。
    pub fn resize(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        handle.row.cols = cols;
        handle.row.rows = rows;
        handle.row.updated_at = chrono::Utc::now().to_rfc3339();
        match &mut handle.process {
            SessionProcess::Pty { master, .. } => {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .map_err(|error| AppError::generic(format!("调整 PTY 尺寸失败: {error}")))?;
            }
            SessionProcess::Fake => {}
        }
        Ok(handle.row.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭终端 tab 后，该会话应从内存 registry 中移除并释放 PTY 资源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 HashMap 删除句柄，尽力 kill 仍在运行的子进程；缺失会话返回错误。
    pub fn close(&self, session_id: &str) -> Result<WorkbenchSessionRow, AppError> {
        let handle = self
            .sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .remove(session_id)
            .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        let was_running = handle.row.status == "running";
        match &mut handle.process {
            SessionProcess::Pty { child, .. } => {
                if was_running {
                    if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                        tracing::debug!("关闭工作台终端时 kill 失败: {error}");
                    }
                }
            }
            SessionProcess::Fake => {}
        }
        handle.row.status = "disconnected".to_string();
        handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
        handle.row.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(handle.row.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用退出时，运行期 PTY attach 应被显式终止；tmux 后端的真实 shell 上下文要保留给下次重连。
    ///
    /// Code Logic（这个函数做什么）:
    ///     遍历 registry 中全部会话句柄，逐个尽力 kill 仍运行的 PTY child，并把内存状态标记为 disconnected。
    pub fn shutdown_all(&self) -> usize {
        let handles: Vec<Arc<Mutex<WorkbenchSessionHandle>>> = self
            .sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .values()
            .cloned()
            .collect();
        let count = handles.len();
        for handle in handles {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            let was_running = handle.row.status == "running";
            match &mut handle.process {
                SessionProcess::Pty { child, .. } => {
                    if was_running {
                        if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                            tracing::debug!("清理工作台终端时 kill 失败: {error}");
                        }
                    }
                }
                SessionProcess::Fake => {}
            }
            handle.row.status = "disconnected".to_string();
            handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
            handle.row.updated_at = chrono::Utc::now().to_rfc3339();
        }
        count
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可以给多个终端会话改名，以区分不同工作流。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查找会话并更新 row name，返回更新后的 row；缺失会话返回错误。
    pub fn rename(&self, session_id: &str, name: &str) -> Result<WorkbenchSessionRow, AppError> {
        let handle = self.get_handle(session_id)?;
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        handle.row.name = name.trim().to_string();
        handle.row.updated_at = chrono::Utc::now().to_rfc3339();
        Ok(handle.row.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个会话操作都需要统一处理 session_id 不存在的错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 registry 中克隆 Arc 句柄；缺失时返回 AppError::NotFound。
    fn get_handle(&self, session_id: &str) -> Result<Arc<Mutex<WorkbenchSessionHandle>>, AppError> {
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::not_found("工作台会话不存在"))
    }

    #[cfg(test)]
    /// Business Logic（为什么需要这个函数）:
    ///     list 过滤测试需要构造不同项目的会话，但不应启动真实 PTY 或依赖本机 Claude CLI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在测试编译时插入 fake 会话句柄，覆盖 list/filter 纯内存逻辑。
    fn insert_fake_session_for_test(&self, session_id: &str, project_id: &str) {
        let row = WorkbenchSessionRow {
            id: session_id.to_string(),
            project_id: project_id.to_string(),
            name: format!("session-{session_id}"),
            command: default_terminal_command_from_env(Some("/bin/sh".into())),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: "2026-06-24T00:00:00Z".to_string(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.to_string(),
            backend_id: None,
            created_at: "2026-06-24T00:00:00Z".to_string(),
            updated_at: "2026-06-24T00:00:00Z".to_string(),
        };
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .insert(
                session_id.to_string(),
                Arc::new(Mutex::new(WorkbenchSessionHandle {
                    row,
                    process: SessionProcess::Fake,
                })),
            );
    }
}

impl Default for WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     需要默认值的测试或未来装配代码可直接构造空 registry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `WorkbenchSessionRegistry::new()` 创建空注册表。
    fn default() -> Self {
        Self::new()
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端终端需要持续接收 PTY stdout/stderr 合并后的输出。
///
/// Code Logic（这个函数做什么）:
///     在后台线程循环读取 reader，把每个字节块转换为 UTF-8 lossless chunk 后 emit。
fn spawn_reader_thread(app: AppHandle, session_id: String, mut reader: Box<dyn Read + Send>) {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut seq: u64 = 0;
        let mut decoder = TerminalUtf8Decoder::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    emit_terminal_output(&app, &session_id, &mut seq, decoder.decode(&buf[..n]));
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出结束: {error}");
                    break;
                }
            }
        }
        if let Some(chunk) = decoder.finish() {
            emit_terminal_output(&app, &session_id, &mut seq, chunk);
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     终端输出事件需要统一递增 seq，且纯 pending chunk 未完成时不应发送空事件。
///
/// Code Logic（这个函数做什么）:
///     非空 chunk 才构造 `TerminalOutputEvent` 并通过 `workbench:terminal-output` emit。
fn emit_terminal_output(app: &AppHandle, session_id: &str, seq: &mut u64, chunk: String) {
    if chunk.is_empty() {
        return;
    }
    *seq += 1;
    let event = TerminalOutputEvent {
        session_id: session_id.to_string(),
        chunk,
        seq: *seq,
        ts: now_millis(),
    };
    if let Err(error) = app.emit("workbench:terminal-output", event) {
        tracing::warn!("发送工作台终端输出事件失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     终端进程退出后，前端需要收到状态变化并保留退出码。
///
/// Code Logic（这个函数做什么）:
///     后台线程短轮询 child.try_wait，退出时更新 DTO 并 emit exited 状态事件。
fn spawn_exit_watcher(
    app: AppHandle,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    session_id: String,
    handle: Arc<Mutex<WorkbenchSessionHandle>>,
) {
    thread::spawn(move || loop {
        let status = {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            match &mut handle.process {
                SessionProcess::Pty { child, .. } => match child.try_wait() {
                    Ok(Some(status)) => Some(Ok(status.exit_code() as i32)),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                },
                SessionProcess::Fake => Some(Ok(0)),
            }
        };

        match status {
            Some(Ok(exit_code)) => {
                let still_registered = sessions
                    .lock()
                    .expect("workbench sessions 锁中毒")
                    .contains_key(&session_id);
                if still_registered {
                    let mut handle = handle.lock().expect("workbench session 锁中毒");
                    handle.row.status = "exited".to_string();
                    handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
                    handle.row.exit_code = Some(exit_code);
                    handle.row.updated_at = chrono::Utc::now().to_rfc3339();
                    emit_status(&app, &session_id, "exited", Some(exit_code));
                }
                break;
            }
            Some(Err(error)) => {
                tracing::warn!("查询工作台终端退出状态失败: {error}");
                emit_status(&app, &session_id, "disconnected", None);
                break;
            }
            None => thread::sleep(Duration::from_millis(200)),
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     会话创建、退出和断开都需要以统一事件格式通知前端。
///
/// Code Logic（这个函数做什么）:
///     构造 `TerminalStatusEvent` 并通过 `workbench:terminal-status` 发送。
fn emit_status(app: &AppHandle, session_id: &str, status: &str, exit_code: Option<i32>) {
    let event = TerminalStatusEvent {
        session_id: session_id.to_string(),
        status: status.to_string(),
        exit_code,
        ts: now_millis(),
    };
    if let Err(error) = app.emit("workbench:terminal-status", event) {
        tracing::warn!("发送工作台终端状态事件失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     前端事件需要毫秒时间戳，用于排序、调试和状态展示。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UTC 时间的 Unix 毫秒时间戳。
fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     新启动应用时还没有工作台终端，前端会话列表应为空数组。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造空 registry 并断言 list(None) 返回空。
    #[test]
    fn list_empty_registry_returns_empty() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.list(None).is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户重命名不存在的会话时，前端需要得到明确错误而不是创建幽灵会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对缺失 session_id 调用 rename 并断言返回 Err。
    #[test]
    fn rename_missing_session_returns_error() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.rename("missing", "name").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户关闭不存在的会话时，应返回错误，避免前端误判关闭成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对缺失 session_id 调用 close 并断言返回 Err。
    #[test]
    fn close_missing_session_returns_error() {
        let registry = WorkbenchSessionRegistry::new();

        assert!(registry.close("missing").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终端交互式程序会输出中文和符号，PTY read 可能把多字节 UTF-8 拆到相邻 chunk。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个被拆开的中文字符串，断言流式解码器能跨 chunk 保留完整字符。
    #[test]
    fn terminal_utf8_decoder_preserves_split_multibyte_characters() {
        let mut decoder = TerminalUtf8Decoder::new();
        let text = "思考: xhigh\n";
        let bytes = text.as_bytes();
        let split_at = "思".len() + 1;

        let first = decoder.decode(&bytes[..split_at]);
        let second = decoder.decode(&bytes[split_at..]);

        assert_eq!(format!("{first}{second}"), text);
        assert_eq!(decoder.finish(), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台打开终端时需要先按前端可见区域启动 PTY，避免交互式程序首屏按默认列宽绘制后错位。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言初始终端尺寸优先使用前端传入值，并对过小或缺失值回退到安全默认值。
    #[test]
    fn initial_terminal_size_uses_frontend_size_with_safe_minimums() {
        assert_eq!(initial_terminal_size(Some(140), Some(42)), (140, 42));
        assert_eq!(
            initial_terminal_size(Some(2), Some(1)),
            (MIN_TERMINAL_COLS, MIN_TERMINAL_ROWS),
        );
        assert_eq!(
            initial_terminal_size(None, None),
            (DEFAULT_COLS, DEFAULT_ROWS)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台打开终端应进入项目根目录的普通 shell，不能替用户自动启动 Claude Code。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造系统 shell 环境值，断言工作台终端命令使用 shell 路径而不是固定的 claude。
    #[test]
    fn workbench_terminal_command_defaults_to_shell_instead_of_claude() {
        let command = default_terminal_command_from_env(Some("/bin/zsh".into()));

        assert_eq!(command, "/bin/zsh");
        assert_ne!(command, "claude");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 用户的项目目录通常是盘符路径，WSL 内的 tmux 只能识别 `/mnt/<drive>/...` 路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Windows 盘符路径、正斜杠路径和扩展长度路径都能转换成 WSL 可用路径。
    #[test]
    fn windows_project_paths_convert_to_wsl_mount_paths() {
        assert_eq!(
            windows_path_to_wsl_path(r"C:\Users\hans\web_project\cc-partner"),
            Some("/mnt/c/Users/hans/web_project/cc-partner".to_string())
        );
        assert_eq!(windows_path_to_wsl_path(r"C:\"), Some("/mnt/c".to_string()));
        assert_eq!(
            windows_path_to_wsl_path("D:/work/cc-partner"),
            Some("/mnt/d/work/cc-partner".to_string())
        );
        assert_eq!(
            windows_path_to_wsl_path(r"\\?\E:\repo with space\app"),
            Some("/mnt/e/repo with space/app".to_string())
        );
        assert_eq!(
            windows_path_to_wsl_path(r"\\wsl$\Ubuntu\home\hans\repo"),
            Some("/home/hans/repo".to_string())
        );
        assert_eq!(
            windows_path_to_wsl_path(r"\\wsl.localhost\Ubuntu\home\hans\repo"),
            Some("/home/hans/repo".to_string())
        );
        assert_eq!(windows_path_to_wsl_path(r"C:relative\path"), None);
        assert_eq!(windows_path_to_wsl_path(r"\\server\share\repo"), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 上应复用用户 WSL 里的 tmux，而不是因为宿主系统没有原生 tmux 就放弃上下文恢复。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 WSL tmux 后端描述，断言它通过 `wsl.exe --exec tmux` 调用且工作目录使用 WSL 路径。
    #[test]
    fn wsl_tmux_backend_invokes_tmux_through_wsl() {
        let backend = TmuxCommand::wsl();

        assert_eq!(backend.program, "wsl.exe");
        assert_eq!(backend.prefix_args, vec!["--exec", "tmux"]);
        assert_eq!(
            backend.project_cwd(r"C:\Users\hans\project").unwrap(),
            "/mnt/c/Users/hans/project"
        );
        assert_eq!(backend.shell_command_for_new_session("cmd.exe"), None);
        assert_eq!(
            backend.display_command_for_session("cc-partner-session", "cmd.exe"),
            "wsl.exe --exec tmux attach-session -t cc-partner-session"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户关闭终端或退出应用时，终端子进程可能已被系统回收，底层 kill 会返回 No such process。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 macOS/Linux 常见 ESRCH(os error 3)，断言终端 kill 归一化逻辑把它视为已停止。
    #[test]
    fn terminal_kill_treats_no_such_process_as_already_stopped() {
        let error = std::io::Error::from_raw_os_error(3);

        assert!(normalize_terminal_kill_result(Err(error)).is_ok());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台按项目切换时，只应展示当前项目下的终端会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入两个 fake 会话并断言 list(Some(project_id)) 只返回匹配项。
    #[test]
    fn list_filters_by_project_id() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s1", "p1");
        registry.insert_fake_session_for_test("s2", "p2");

        let listed = registry.list(Some("p1"));

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s1");
        assert_eq!(listed[0].project_id, "p1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     应用退出时必须停止运行期 PTY attach，但不能丢掉用户下次启动要恢复的会话元数据。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 fake 会话后调用 shutdown_all，断言返回清理数量且会话状态变为 disconnected。
    #[test]
    fn shutdown_all_marks_sessions_disconnected() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s1", "p1");
        registry.insert_fake_session_for_test("s2", "p2");

        let cleaned = registry.shutdown_all();

        assert_eq!(cleaned, 2);
        let listed = registry.list(None);
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .all(|session| session.status == "disconnected"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     应用退出后再次启动时，用户之前打开的工作台终端 tab 应能恢复，而不是因为退出清理被彻底遗忘。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 fake 会话并执行退出清理，断言会话元数据仍可列出且状态被标记为 disconnected。
    #[test]
    fn shutdown_all_preserves_session_metadata_for_restart_restore() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s1", "p1");

        let cleaned = registry.shutdown_all();
        let listed = registry.list(Some("p1"));

        assert_eq!(cleaned, 1);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s1");
        assert_eq!(listed[0].status, "disconnected");
        assert!(listed[0].exited_at.is_some());
    }
}
