//! workbench/sessions.rs — 工作台本机 PTY 会话注册表
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台允许用户在同一项目下开启多个本机项目终端，用户希望应用重启后终端 tab 与可重连上下文仍可恢复。
//!
//! Code Logic（这个模块做什么）:
//!     使用 portable-pty 创建 PTY；macOS/Linux 原生 tmux、Windows WSL tmux 可承载真实 shell 上下文，应用重启后重新 attach。
//!     内存保存运行期句柄，通过后端 UI adapter 推送终端输出和状态变化。

#![allow(dead_code)]

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_runtime::{
    try_enqueue_agent_mutation, AgentOscDecoder, AgentRuntimeMutation,
};
use crate::workbench::dependencies::{available_tmux_command, TmuxCommand};
use crate::workbench::models::{WorkbenchProjectRow, WorkbenchSessionDto, WorkbenchSessionRow};
use crate::workbench::remote_events::{
    publish_workbench_remote_event_from_state, WorkbenchRemoteEvent,
    WorkbenchTerminalOutputPayload, WorkbenchTerminalStatusPayload,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const DEFAULT_COLS: u16 = 98;
const DEFAULT_ROWS: u16 = 32;
const MIN_TERMINAL_COLS: u16 = 20;
const MIN_TERMINAL_ROWS: u16 = 6;
const TMUX_SESSION_ID_SUFFIX_LEN: usize = 12;
const SESSION_REPLAY_MAX_CHARS: usize = 120_000;
const RAW_PTY_BACKEND: &str = "pty";
const TMUX_BACKEND: &str = "tmux";
#[cfg(windows)]
const FALLBACK_TERMINAL_COMMAND: &str = "cmd.exe";
#[cfg(not(windows))]
const FALLBACK_TERMINAL_COMMAND: &str = "/bin/sh";

/// tmux pane 分屏方向。
///
/// Business Logic（为什么需要这个枚举）:
///     用户在工作台 window 内需要像 tmux 一样创建左右或上下 pane。
///
/// Code Logic（这个枚举做什么）:
///     将前端方向参数映射到 tmux split-window 的 `-h` / `-v` 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneSplitDirection {
    Right,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneClosePlan {
    KillPane,
    CloseWindow,
}

/// pane 关闭结果。
///
/// Business Logic（为什么需要这个枚举）:
///     分屏工具栏 X 关闭最后一个 pane 时，底层 tmux 会关闭整个 window，前端需要同步删除 tab。
///
/// Code Logic（这个枚举做什么）:
///     区分普通 pane 关闭与最后一个 pane 导致的 window 关闭，并在后者携带需要清理的 row。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum PaneCloseOutcome {
    PaneClosed,
    WindowClosed(WorkbenchSessionRow),
}

impl PaneSplitDirection {
    /// Business Logic（为什么需要这个函数）:
    ///     前端通过字符串参数请求 pane 分屏方向，后端需要做显式校验。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 API 字符串 `right` / `down` 转成枚举；其他值返回 AppError。
    pub fn from_api(value: &str) -> Result<Self, AppError> {
        match value {
            "right" => Ok(Self::Right),
            "down" => Ok(Self::Down),
            _ => Err(AppError::generic("不支持的 pane 分屏方向")),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     pane 分屏命令必须使用 tmux 原生命令参数，避免 UI 表达和真实布局不一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Right 生成左右分屏 `-h`，Down 生成上下分屏 `-v`。
    fn tmux_flag(self) -> &'static str {
        match self {
            Self::Right => "-h",
            Self::Down => "-v",
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户点击分屏工具栏 X 时应关闭当前 active pane；只有最后一个 pane 时应关闭整个 window，不应报错。
///
/// Code Logic（这个函数做什么）:
///     根据当前 window 的 pane 数决定执行 kill-pane 还是关闭 window。
fn pane_close_plan(pane_count: usize) -> PaneClosePlan {
    if pane_count > 1 {
        PaneClosePlan::KillPane
    } else {
        PaneClosePlan::CloseWindow
    }
}

/// Business Logic（为什么需要这个函数）:
///     项目列表需要展示每个 terminal window 内的真实 pane 数，tmux 是该数据的权威来源。
///
/// Code Logic（这个函数做什么）:
///     解析 `tmux list-panes -F #{pane_id}` 输出，忽略空行后返回 pane id 行数。
fn pane_count_from_tmux_output(output: &str) -> usize {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
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

/// 工作台终端会话 replay 快照。
///
/// Business Logic（为什么需要这个结构体）:
///     移动端首次打开终端时无法收到历史 live event，需要通过 HTTP 拉取最近输出后再接增量事件。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 序列化 sessionId、最近输出 buffer、是否截断和最后 seq，供 Rust route 与前端类型对齐。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionReplayDto {
    pub session_id: String,
    pub buffer: String,
    pub truncated: bool,
    pub last_seq: u64,
}

/// 工作台终端最近输出 ring buffer。
///
/// Business Logic（为什么需要这个结构体）:
///     移动端进入远端终端时需要看到最近屏幕输出，而不是只能等待新事件。
///
/// Code Logic（这个结构体做什么）:
///     以字符数量为容量上限保存输出尾部，记录是否曾截断以及最新 terminal output seq。
#[derive(Debug, Clone)]
struct SessionReplayBuffer {
    max_chars: usize,
    buffer: String,
    truncated: bool,
    last_seq: u64,
}

impl SessionReplayBuffer {
    /// Business Logic（为什么需要这个函数）:
    ///     每个 Workbench session 创建或恢复时都需要初始化自己的最近输出缓存。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 buffer，并记录最大保留 Unicode scalar 数量。
    fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            buffer: String::new(),
            truncated: false,
            last_seq: 0,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终端 reader 每收到一个非空输出 chunk，都要让移动端 replay 能补上这段历史输出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     追加 UTF-8 文本、更新 last_seq，并在超过 max_chars 时按 char 边界保留尾部。
    fn append(&mut self, chunk: &str, seq: u64) {
        self.buffer.push_str(chunk);
        self.last_seq = seq;
        self.truncate_to_limit();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     replay 截断不能破坏中文或 emoji，否则移动端渲染可能出现乱码或 panic。
    ///
    /// Code Logic（这个函数做什么）:
    ///     当字符数超过上限时，从 chars 迭代器重建最后 max_chars 个字符，避免按字节切开 UTF-8。
    fn truncate_to_limit(&mut self) {
        let char_count = self.buffer.chars().count();
        if char_count <= self.max_chars {
            return;
        }

        self.truncated = true;
        if self.max_chars == 0 {
            self.buffer.clear();
            return;
        }

        let mut kept: Vec<char> = self.buffer.chars().rev().take(self.max_chars).collect();
        kept.reverse();
        self.buffer = kept.into_iter().collect();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     HTTP replay route 需要返回当前 session 的一致性快照，避免暴露内部可变 buffer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆当前 buffer 状态并补入调用方传入的 session_id。
    fn snapshot(&self, session_id: &str) -> WorkbenchSessionReplayDto {
        WorkbenchSessionReplayDto {
            session_id: session_id.to_string(),
            buffer: self.buffer.clone(),
            truncated: self.truncated,
            last_seq: self.last_seq,
        }
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
pub(crate) fn windows_path_to_wsl_path(path: &str) -> Option<String> {
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
///     worktree 级 tmux session 名会展示在 tmux status 中，需要既可读又规避 target 特殊字符。
///
/// Code Logic（这个函数做什么）:
///     保留 ASCII 字母数字并把其他字符折叠为 `-`；空值使用 `root` 兜底。
fn tmux_session_component(value: &str) -> String {
    let mut component = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            component.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            component.push('-');
            last_dash = true;
        }
    }
    let component = component.trim_matches('-').to_string();
    if component.is_empty() {
        "root".to_string()
    } else {
        component
    }
}

/// Business Logic（为什么需要这个函数）:
///     可读 session 名清洗后可能碰撞，仍需要携带短稳定 id 片段保持 worktree 隔离。
///
/// Code Logic（这个函数做什么）:
///     复用 session 组件清洗逻辑，只取末尾 ASCII 字母数字作为短后缀；空值使用 `root`。
fn tmux_session_id_suffix(value: &str) -> String {
    let component: String = tmux_session_component(value)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if component.is_empty() {
        return "root".to_string();
    }
    if component.len() <= TMUX_SESSION_ID_SUFFIX_LEN {
        component
    } else {
        component[component.len() - TMUX_SESSION_ID_SUFFIX_LEN..].to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     缺少 worktree_id 的旧 terminal window 应视为主工作区，保证恢复和聚焦过滤有稳定归属。
///
/// Code Logic（这个函数做什么）:
///     返回 row 或请求中的 worktree_id；空值 fallback 到 `{project_id}:main`。
fn effective_worktree_id(project_id: &str, worktree_id: Option<&str>) -> String {
    worktree_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{project_id}:main"))
}

/// Business Logic（为什么需要这个函数）:
///     不同 worktree 的 terminal window 必须互相隔离，同时用户在 tmux status 中应看到可读 worktree 名。
///
/// Code Logic（这个函数做什么）:
///     优先用 project/worktree 的展示名派生 session 名，并追加短 id 后缀保持唯一；缺少 worktree 名时使用确定性 worktree id fallback。
fn tmux_worktree_session_name(
    project_name: &str,
    project_id: &str,
    worktree_id: Option<&str>,
    worktree_name: Option<&str>,
) -> String {
    let worktree_id = effective_worktree_id(project_id, worktree_id);
    let display_worktree_name = worktree_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&worktree_id);
    format!(
        "{}-{}-{}",
        tmux_session_component(project_name),
        tmux_session_component(display_worktree_name),
        tmux_session_id_suffix(&worktree_id)
    )
}

/// Business Logic（为什么需要这个函数）:
///     focused-window 同步必须只在当前 active worktree 内映射，避免跨 worktree 抢占顶部 tab。
///
/// Code Logic（这个函数做什么）:
///     把 None worktree 归一化成主工作区后比较。
fn worktree_id_matches(
    project_id: &str,
    row_worktree_id: Option<&str>,
    target: Option<&str>,
) -> bool {
    effective_worktree_id(project_id, row_worktree_id) == effective_worktree_id(project_id, target)
}

/// Business Logic（为什么需要这个函数）:
///     后端 attach、split、kill-pane、rename-window 都需要指向 worktree tmux session 内的特定 window。
///
/// Code Logic（这个函数做什么）:
///     组合 tmux session 名与 window id，生成 `session:@window` target。
fn tmux_window_target(session_name: &str, window_id: &str) -> String {
    format!("{session_name}:{window_id}")
}

/// terminal / Agent adapter 共享的非敏感稳定上下文 ID。
///
/// Business Logic（为什么需要这个类型）:
///     Hook/OSC 需要 project/worktree/terminal/owner 关联，但不得注入 control token 或凭据。
///
/// Code Logic（这个类型做什么）:
///     四字段纯 ID；由 spawn 路径从 row + AppState 组装。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalAgentContextIds {
    /// 项目 id
    pub project_id: String,
    /// worktree id（可空串）
    pub worktree_id: String,
    /// terminal session id
    pub terminal_session_id: String,
    /// owner 实例 id
    pub owner_instance_id: String,
}

/// Business Logic（为什么需要这个函数）:
///     cc-partner 是 GUI 应用，父进程可能没有真实终端环境或继承 `TERM=dumb`，会破坏 tmux 客户端协商；
///     Agent adapter 还需要稳定非敏感 ID 环境变量。
///
/// Code Logic（这个函数做什么）:
///     设置 xterm TERM/COLORTERM/TERM_PROGRAM，以及可选的四条 `CC_PARTNER_*_ID`（无 token）。
fn apply_workbench_terminal_env(
    command: &mut CommandBuilder,
    agent_ctx: Option<&TerminalAgentContextIds>,
) {
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "cc-partner");
    if let Some(ctx) = agent_ctx {
        apply_agent_context_env(command, ctx);
    }
}

/// 注入四条非敏感 Agent 上下文环境变量。
///
/// Business Logic（为什么需要这个函数）:
///     tmux 与 raw PTY 的 pane/shell 必须能读到同一套 CC_PARTNER_*_ID，供 Hook 关联 session。
///
/// Code Logic（这个函数做什么）:
///     仅设置 PROJECT/WORKTREE/TERMINAL_SESSION/OWNER_INSTANCE_ID；不设置任何 token/credential。
fn apply_agent_context_env(command: &mut CommandBuilder, ctx: &TerminalAgentContextIds) {
    command.env("CC_PARTNER_PROJECT_ID", &ctx.project_id);
    command.env("CC_PARTNER_WORKTREE_ID", &ctx.worktree_id);
    command.env("CC_PARTNER_TERMINAL_SESSION_ID", &ctx.terminal_session_id);
    command.env("CC_PARTNER_OWNER_INSTANCE_ID", &ctx.owner_instance_id);
}

/// 生成 tmux `new-session` / `new-window` 的 `-e KEY=VAL` 参数（Agent 上下文）。
///
/// Business Logic（为什么需要这个函数）:
///     tmux pane 内 shell 继承创建时的 -e 环境；attach 客户端 env 不会进入已有 pane。
///
/// Code Logic（这个函数做什么）:
///     返回交错的 `-e` / `KEY=VAL` 列表，仅含四条 CC_PARTNER_*_ID。
fn tmux_agent_context_env_args(ctx: &TerminalAgentContextIds) -> Vec<String> {
    let pairs = [
        ("CC_PARTNER_PROJECT_ID", ctx.project_id.as_str()),
        ("CC_PARTNER_WORKTREE_ID", ctx.worktree_id.as_str()),
        (
            "CC_PARTNER_TERMINAL_SESSION_ID",
            ctx.terminal_session_id.as_str(),
        ),
        (
            "CC_PARTNER_OWNER_INSTANCE_ID",
            ctx.owner_instance_id.as_str(),
        ),
    ];
    let mut args = Vec::with_capacity(pairs.len() * 2);
    for (k, v) in pairs {
        args.push("-e".to_string());
        args.push(format!("{k}={v}"));
    }
    args
}

/// Business Logic（为什么需要这个函数）:
///     app 里的 terminal window 必须绑定到对应 tmux window，不能只 attach 到 worktree session 的当前 window。
///
/// Code Logic（这个函数做什么）:
///     构造 `attach-session -t <session> ; switch-client -t <session:@window>` 参数。
pub(crate) fn tmux_attach_window_args(session_name: &str, window_target: &str) -> Vec<String> {
    vec![
        "attach-session".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        ";".to_string(),
        "switch-client".to_string(),
        "-t".to_string(),
        window_target.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 的终端主题由前端 xterm 控制，tmux status bar 不能继承用户全局 `.tmux.conf` 中的固定深色背景。
///
/// Code Logic（这个函数做什么）:
///     生成一组浅色/深色都安全的 tmux status option 命令；保留 session/window 标签结构，但不保留全局 tmux 主题里的硬编码颜色。
fn tmux_status_theme_commands(session_name: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-style".to_string(),
            "fg=default,bg=default".to_string(),
        ],
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-left-style".to_string(),
            "fg=default,bg=default".to_string(),
        ],
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-right-style".to_string(),
            "fg=default,bg=default".to_string(),
        ],
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-left".to_string(),
            "#[bold]#S › ".to_string(),
        ],
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-right".to_string(),
            "%H:%M | %Y-%m-%d ".to_string(),
        ],
        vec![
            "set-window-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "window-status-style".to_string(),
            "fg=default,bg=default".to_string(),
        ],
        vec![
            "set-window-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "window-status-current-style".to_string(),
            "fg=black,bg=colour111,bold".to_string(),
        ],
        vec![
            "set-window-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "window-status-format".to_string(),
            " #I:#W#F ".to_string(),
        ],
        vec![
            "set-window-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "window-status-current-format".to_string(),
            " #I:#W#F ".to_string(),
        ],
        vec![
            "set-window-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "window-status-separator".to_string(),
            " ".to_string(),
        ],
    ]
}

/// Business Logic（为什么需要这个函数）:
///     用户切换应用浅色/深色主题后，tmux status bar 应随 xterm 默认色变化，而不是停留在用户 tmux 主题色。
///
/// Code Logic（这个函数做什么）:
///     对指定 worktree tmux session 逐条执行 Workbench status 样式命令；失败向上返回供调用方记录但不影响 PTY fallback。
fn apply_workbench_tmux_status_theme(
    tmux: &TmuxCommand,
    session_name: &str,
) -> Result<(), AppError> {
    for args in tmux_status_theme_commands(session_name) {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_tmux_command(tmux, &arg_refs)?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     app 顶部 tab 切换时，用户看到的 tmux 当前 window 必须同步切到该 tab 绑定的真实 window。
///
/// Code Logic（这个函数做什么）:
///     构造 `select-window -t <session:@window>` 参数列表，切换 worktree tmux session 的 current window。
fn tmux_select_window_args(window_target: &str) -> Vec<String> {
    vec![
        "select-window".to_string(),
        "-t".to_string(),
        window_target.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     用户可通过 tmux 底部状态栏或快捷键切换 window，cc-partner 需要读取真实 current window。
///
/// Code Logic（这个函数做什么）:
///     构造 `display-message -p -t <session> #{window_id}` 参数，查询 worktree tmux session 当前 window id。
fn tmux_current_window_args(session_name: &str) -> Vec<String> {
    vec![
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        "#{window_id}".to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     session 命名规则升级后，旧 tmux session 应尽量改名到新可读名称，保留原 shell 上下文。
///
/// Code Logic（这个函数做什么）:
///     构造 `rename-session -t <old> <new>` 参数列表。
fn tmux_rename_session_args(old_session_name: &str, new_session_name: &str) -> Vec<String> {
    vec![
        "rename-session".to_string(),
        "-t".to_string(),
        old_session_name.to_string(),
        new_session_name.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     后端读到 tmux current window 后，需要映射回前端顶部 app tab 的 sessionId。
///
/// Code Logic（这个函数做什么）:
///     在同一 project/worktree/backend_id 下按 backend_window_id 匹配当前 window，命中时返回 Workbench session id。
fn focused_session_id_for_tmux_window<'a>(
    rows: impl IntoIterator<Item = &'a WorkbenchSessionRow>,
    project_id: &str,
    worktree_id: Option<&str>,
    backend_id: &str,
    window_id: &str,
) -> Option<String> {
    rows.into_iter()
        .find(|row| {
            row.project_id == project_id
                && worktree_id_matches(project_id, row.worktree_id.as_deref(), worktree_id)
                && row.backend == TMUX_BACKEND
                && row.backend_id.as_deref() == Some(backend_id)
                && row.backend_window_id.as_deref() == Some(window_id)
        })
        .map(|row| row.id.clone())
}

/// Business Logic（为什么需要这个函数）:
///     创建 window 前需要知道 worktree 级 tmux session 是否已存在，存在则 new-window，不存在则 new-session。
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
///     恢复 window 时需要判断目标 window 是否仍存在，存在则 attach，不存在则重新创建。
///
/// Code Logic（这个函数做什么）:
///     执行 `tmux display-message -p -t <target> #{window_id}`；target 可为 session 或 session:@window。
fn tmux_target_exists(tmux: &TmuxCommand, target: &str) -> bool {
    tmux.std_command()
        .args(["display-message", "-p", "-t", target, "#{window_id}"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     恢复旧版本持久化会话时，应把仍存在的旧 tmux session 尽量迁移成新的可读名称，而不是丢弃上下文重建。
///
/// Code Logic（这个函数做什么）:
///     当前 session 存在、目标 session 不存在时执行 `rename-session`；失败则返回旧名以继续 attach 旧上下文。
fn migrate_tmux_session_name(
    tmux: &TmuxCommand,
    current_session_name: Option<&str>,
    desired_session_name: &str,
) -> String {
    let Some(current_session_name) = current_session_name else {
        return desired_session_name.to_string();
    };
    if current_session_name == desired_session_name {
        return desired_session_name.to_string();
    }
    if !tmux_target_exists(tmux, current_session_name) {
        return desired_session_name.to_string();
    }
    if tmux_target_exists(tmux, desired_session_name) {
        return desired_session_name.to_string();
    }

    let args = tmux_rename_session_args(current_session_name, desired_session_name);
    let args_ref = args.iter().map(String::as_str).collect::<Vec<_>>();
    match run_tmux_command(tmux, &args_ref) {
        Ok(_) => desired_session_name.to_string(),
        Err(error) => {
            tracing::warn!("迁移工作台 tmux session 名失败，继续使用旧 session: {error}");
            current_session_name.to_string()
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     新建或恢复 tab 时，需要在 worktree 级 tmux session 内创建一个 window 承载真实 shell 上下文。
///
/// Code Logic（这个函数做什么）:
///     session 不存在时执行 `tmux new-session -d -s <session> -n <window>`；存在时执行 `tmux new-window`；
///     两者都用 `-P -F #{window_id}` 读取真实 window id。
fn create_tmux_window(
    tmux: &TmuxCommand,
    session_name: &str,
    window_name: &str,
    cwd: &str,
    shell_command: &str,
    agent_ctx: Option<&TerminalAgentContextIds>,
) -> Result<String, AppError> {
    let tmux_cwd = tmux.project_cwd(cwd)?;
    let mut command = tmux.std_command();
    if tmux_has_session(tmux, session_name) {
        command.args([
            "new-window",
            "-d",
            "-t",
            session_name,
            "-n",
            window_name,
            "-c",
            &tmux_cwd,
            "-P",
            "-F",
            "#{window_id}",
        ]);
    } else {
        command.args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-n",
            window_name,
            "-c",
            &tmux_cwd,
            "-P",
            "-F",
            "#{window_id}",
        ]);
    }
    if let Some(ctx) = agent_ctx {
        for arg in tmux_agent_context_env_args(ctx) {
            command.arg(arg);
        }
    }
    if let Some(shell_command) = tmux.shell_command_for_new_session(shell_command) {
        command.arg(shell_command);
    }

    let output = command.output()?;
    if output.status.success() {
        let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if window_id.is_empty() {
            Err(AppError::generic("创建 tmux window 失败: 未返回 window_id"))
        } else {
            if let Err(error) = apply_workbench_tmux_status_theme(tmux, session_name) {
                tracing::debug!("应用工作台 tmux status 样式失败: {error}");
            }
            Ok(window_id)
        }
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if detail.is_empty() {
            "未知错误".to_string()
        } else {
            detail
        };
        Err(AppError::generic(format!(
            "创建 tmux window 失败: {message}"
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     关闭最后一个 pane 会关闭所属 window；worktree tmux session 只剩最后一个 window 时必须销毁整个 session。
///
/// Code Logic（这个函数做什么）:
///     根据 window_id 与当前 window_count 构造 kill-window 或 kill-session 参数。
fn tmux_destroy_backend_args(
    session_name: &str,
    window_id: Option<&str>,
    window_count: Option<usize>,
) -> Vec<String> {
    match (window_id, window_count) {
        (Some(window_id), Some(count)) if count > 1 => vec![
            "kill-window".to_string(),
            "-t".to_string(),
            tmux_window_target(session_name, window_id),
        ],
        _ => vec![
            "kill-session".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ],
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户关闭终端 tab 时，如果该 tab 使用 tmux 承载上下文，应销毁对应 tmux session，避免后台残留。
///
/// Code Logic（这个函数做什么）:
///     多 window 项目执行 `kill-window -t <session:window>`；最后一个 window 或旧记录退回 kill-session。
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
    let window_count = run_tmux_command(
        &tmux,
        &["list-windows", "-t", session_name, "-F", "#{window_id}"],
    )
    .ok()
    .map(|windows| {
        windows
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    });
    let args =
        tmux_destroy_backend_args(session_name, row.backend_window_id.as_deref(), window_count);
    let mut command = tmux.std_command();
    command.args(args.iter().map(String::as_str));
    let output = command.output();
    if let Err(error) = output {
        tracing::debug!("销毁工作台 tmux 会话失败: {error}");
    }
}

/// 从 session row 与 owner 实例 id 组装 Agent 上下文。
///
/// Business Logic（为什么需要这个函数）:
///     spawn/attach 需要把稳定 ID 注入 shell，Hook 才能关联 OSC。
///
/// Code Logic（这个函数做什么）:
///     worktree 缺省为空串；owner_instance_id 原样拷贝。
fn agent_context_from_row(
    row: &WorkbenchSessionRow,
    owner_instance_id: &str,
) -> TerminalAgentContextIds {
    TerminalAgentContextIds {
        project_id: row.project_id.clone(),
        worktree_id: row.worktree_id.clone().unwrap_or_default(),
        terminal_session_id: row.id.clone(),
        owner_instance_id: owner_instance_id.to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     portable-pty 启动命令需要统一构造，普通 PTY 和 tmux attach 仅命令及参数不同。
///
/// Code Logic（这个函数做什么）:
///     根据 row.backend/backend_id 构造 CommandBuilder；注入 TERM 与 CC_PARTNER_*_ID。
fn command_builder_for_row(row: &WorkbenchSessionRow, owner_instance_id: &str) -> CommandBuilder {
    let agent_ctx = agent_context_from_row(row, owner_instance_id);
    if row.backend == TMUX_BACKEND {
        if let (Some(tmux), Some(session_name)) =
            (available_tmux_command(), row.backend_id.as_deref())
        {
            let mut cmd = tmux.command_builder();
            let target = row
                .backend_window_id
                .as_deref()
                .map(|window_id| tmux_window_target(session_name, window_id))
                .unwrap_or_else(|| session_name.to_string());
            if row.backend_window_id.is_some() {
                let args = tmux_attach_window_args(session_name, &target);
                cmd.args(args.iter().map(String::as_str));
            } else {
                cmd.args(["attach-session", "-t", session_name]);
            }
            apply_workbench_terminal_env(&mut cmd, Some(&agent_ctx));
            return cmd;
        }
    }
    let mut cmd = CommandBuilder::new(row.command.clone());
    apply_workbench_terminal_env(&mut cmd, Some(&agent_ctx));
    cmd
}

/// Business Logic（为什么需要这个函数）:
///     tmux window/pane 操作都需要从持久化 row 找到精确 target。
///
/// Code Logic（这个函数做什么）:
///     对 tmux row 组合 `backend_id` 与 `backend_window_id`；缺少 window id 的旧记录退回 session target。
fn tmux_target_for_row(row: &WorkbenchSessionRow) -> Result<String, AppError> {
    if row.backend != TMUX_BACKEND {
        return Err(AppError::generic("当前终端后端不支持 tmux pane 操作"));
    }
    let Some(session_name) = row.backend_id.as_deref() else {
        return Err(AppError::generic("tmux 会话缺少 session 标识"));
    };
    Ok(row
        .backend_window_id
        .as_deref()
        .map(|window_id| tmux_window_target(session_name, window_id))
        .unwrap_or_else(|| session_name.to_string()))
}

/// Business Logic（为什么需要这个函数）:
///     pane/window 操作只应作用于真实 tmux window；旧记录缺 window id 时必须先迁移。
///
/// Code Logic（这个函数做什么）:
///     对 tmux row 要求同时存在 backend_id 和 backend_window_id，并返回 `session:@window` target。
fn tmux_window_target_for_row(row: &WorkbenchSessionRow) -> Result<String, AppError> {
    if row.backend != TMUX_BACKEND {
        return Err(AppError::generic("当前终端后端不支持 tmux pane 操作"));
    }
    let Some(session_name) = row.backend_id.as_deref() else {
        return Err(AppError::generic("tmux window 缺少 session 标识"));
    };
    let Some(window_id) = row.backend_window_id.as_deref() else {
        return Err(AppError::generic("tmux window 缺少 window 标识"));
    };
    Ok(tmux_window_target(session_name, window_id))
}

/// Business Logic（为什么需要这个函数）:
///     从旧版本升级来的 tmux row 可能只有 per-tab session，没有 window id，需要迁移到真实 window 模型。
///
/// Code Logic（这个函数做什么）:
///     返回 tmux row 是否缺少 backend_window_id。
fn tmux_row_requires_window_recreation(row: &WorkbenchSessionRow) -> bool {
    row.backend == TMUX_BACKEND && row.backend_window_id.is_none()
}

/// Business Logic（为什么需要这个函数）:
///     pane 操作失败时需要向前端返回可诊断错误，而不是静默无效。
///
/// Code Logic（这个函数做什么）:
///     执行 tmux 命令，成功返回 stdout，失败把 stderr 转为 AppError。
fn run_tmux_command(tmux: &TmuxCommand, args: &[&str]) -> Result<String, AppError> {
    let output = tmux.std_command().args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let message = if detail.is_empty() {
            "未知错误".to_string()
        } else {
            detail
        };
        Err(AppError::generic(format!("tmux 命令失败: {message}")))
    }
}

/// Business Logic（为什么需要这个函数）:
///     工作台项目卡片需要展示 window 下 pane 数，必须读取真实 tmux 状态而不是前端猜测。
///
/// Code Logic（这个函数做什么）:
///     对指定 tmux target 执行 `list-panes` 并解析非空 pane id 行数。
fn tmux_pane_count(tmux: &TmuxCommand, target: &str) -> Result<usize, AppError> {
    let output = run_tmux_command(tmux, &["list-panes", "-t", target, "-F", "#{pane_id}"])?;
    Ok(pane_count_from_tmux_output(&output))
}

/// Business Logic（为什么需要这个函数）:
///     会话 DTO 需要带 paneCount；tmux-backed window 应尽量返回真实 pane 数，raw/disconnected 会话也要有稳定兜底。
///
/// Code Logic（这个函数做什么）:
///     对 running tmux row 查询 pane 数；查询失败或非 tmux 后端时返回 1，避免统计 UI 被临时 tmux 错误清零。
pub fn pane_count_for_row(row: &WorkbenchSessionRow) -> usize {
    if row.status == "running" && row.backend == TMUX_BACKEND {
        if let (Some(tmux), Ok(target)) =
            (available_tmux_command(), tmux_window_target_for_row(row))
        {
            return tmux_pane_count(&tmux, &target).unwrap_or(1).max(1);
        }
    }
    1
}

/// Business Logic（为什么需要这个函数）:
///     分屏按钮创建的新 pane 必须从项目根目录启动，避免继承当前 pane 中用户 cd 后的位置。
///
/// Code Logic（这个函数做什么）:
///     构造 `tmux split-window <direction> -t <target> -c <cwd>` 参数列表。
fn tmux_split_window_args(direction: PaneSplitDirection, target: &str, cwd: &str) -> Vec<String> {
    vec![
        "split-window".to_string(),
        direction.tmux_flag().to_string(),
        "-t".to_string(),
        target.to_string(),
        "-c".to_string(),
        cwd.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     用户需要在当前 terminal window 内循环切换 active pane，操作范围必须锁定到该 tmux window。
///
/// Code Logic（这个函数做什么）:
///     构造 `tmux select-pane -t <window-target>.+` 参数列表。
fn tmux_select_next_pane_args(target: &str) -> Vec<String> {
    vec![
        "select-pane".to_string(),
        "-t".to_string(),
        format!("{target}.+"),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     移动端需要把当前 active pane 显示为单 pane 视图，但不能改变所属 tmux window。
///
/// Code Logic（这个函数做什么）:
///     构造 `tmux resize-pane -Z -t <window-target>` 参数列表；调用前必须已确认当前未 zoom，避免反向取消 zoom。
fn tmux_zoom_active_pane_args(target: &str) -> Vec<String> {
    vec![
        "resize-pane".to_string(),
        "-Z".to_string(),
        "-t".to_string(),
        target.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     ensure-zoom 需要先读取当前 tmux window 是否已经 zoom，避免重复调用 `resize-pane -Z` 反而取消 zoom。
///
/// Code Logic（这个函数做什么）:
///     构造 `display-message -p -t <target> #{window_zoomed_flag}` 参数列表。
fn tmux_window_zoomed_args(target: &str) -> Vec<String> {
    vec![
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        target.to_string(),
        "#{window_zoomed_flag}".to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     tmux 输出是文本协议，后端需要把 window_zoomed_flag 转为布尔值用于幂等 ensure-zoom。
///
/// Code Logic（这个函数做什么）:
///     任一非空行 trim 后等于 `1` 时返回 true，其它情况返回 false。
fn tmux_window_zoomed_from_output(output: &str) -> bool {
    output.lines().any(|line| line.trim() == "1")
}

/// Business Logic（为什么需要这个函数）:
///     移动端新增、切换或关闭 pane 后，要保证用户仍只看到当前 active pane。
///
/// Code Logic（这个函数做什么）:
///     查询 tmux window zoom flag 并返回布尔值，供 ensure-zoom 决策。
fn tmux_window_is_zoomed(tmux: &TmuxCommand, target: &str) -> Result<bool, AppError> {
    let args = tmux_window_zoomed_args(target);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_tmux_command(tmux, &arg_refs)?;
    Ok(tmux_window_zoomed_from_output(&output))
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
///     用 HashMap 保存 session_id 到会话句柄和 replay buffer 的映射；外层 Arc 允许后台读写线程更新状态。
///     Clone 廉价（内部全是 Arc），供 `SessionSpawnGuard` / `RestoreClaimGuard` 持有。
#[derive(Clone)]
pub struct WorkbenchSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
    /// 正在 restore 的 session_id 占位集合（Finding 5: TOCTOU 修复）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     `restore_persisted_sessions` 先 `contains()` 再异步 `resolve_worktree` 后 `restore()`，
    ///     两个并发的 sessions/list 请求都能通过 contains() 检查并各自 spawn 一次 PTY/tmux 窗口。
    ///     占位集合让"检查 + 占位"在同一个 Mutex 内原子完成：第一个 caller 拿到 claim，
    ///     第二个直接跳过。restore 完成后由 caller 释放 claim（成功路径 spawn_row 已写入 sessions，
    ///     contains 自然命中；失败路径释放后允许后续重试）。
    restoring: Arc<Mutex<HashSet<String>>>,
}

/// Session 创建后的 RAII 补偿守卫：repo 持久化失败时自动关闭 attach，禁止 ghost registry/child。
///
/// Business Logic（为什么需要这个结构体）:
///     create/restore 先 spawn PTY 再写 SQLite；若 upsert 失败必须回收运行期资源，
///     否则 sidecar 留下无元数据的 ghost 终端。
///
/// Code Logic（这个结构体做什么）:
///     持有 registry 与 session_id；未 `commit()` 时 Drop 调用 `close` 移除并 kill child。
pub struct SessionSpawnGuard {
    registry: WorkbenchSessionRegistry,
    session_id: String,
    committed: bool,
}

impl SessionSpawnGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     spawn 成功后立刻接管生命周期，后续任何 early return 都能自动补偿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 registry/session_id，`committed=false`。
    pub fn new(registry: WorkbenchSessionRegistry, session_id: String) -> Self {
        Self {
            registry,
            session_id,
            committed: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     SQLite upsert 成功后才允许会话进入正式运行期，不再被 Drop 回收。
    ///
    /// Code Logic（这个函数做什么）:
    ///     置 `committed=true`，Drop 变为 no-op。
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for SessionSpawnGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     任何未提交路径（含 panic）都必须关闭 attach，防止 ghost child/registry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 commit 时 best-effort `close(session_id)`（会话已不存在则忽略）。
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.registry.close(&self.session_id);
        }
    }
}

/// restore claim 的 RAII 守卫：任何 early return 都释放占位，避免永久跳过恢复。
///
/// Business Logic（为什么需要这个结构体）:
///     `try_claim_restore` 成功后若中途失败却未释放 claim，后续 list 永远不会再恢复该 session。
///
/// Code Logic（这个结构体做什么）:
///     Drop 时若未 `disarm`/`commit` 则调用 `release_restore_claim`。
pub struct RestoreClaimGuard {
    registry: WorkbenchSessionRegistry,
    session_id: String,
    armed: bool,
}

impl RestoreClaimGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     claim 成功后立刻接管释放责任。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 registry/session_id，armed=true。
    pub fn new(registry: WorkbenchSessionRegistry, session_id: String) -> Self {
        Self {
            registry,
            session_id,
            armed: true,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调用方已显式 release 时禁止 Drop 二次释放（幂等但仍避免重复日志路径）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     armed=false。
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RestoreClaimGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     restore 任意失败出口都必须释放 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     armed 时调用 `release_restore_claim`。
    fn drop(&mut self) {
        if self.armed {
            self.registry.release_restore_claim(&self.session_id);
        }
    }
}

impl WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的工作台会话注册表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空会话 HashMap 与 replay buffer HashMap，并包裹 Arc<Mutex<_>> 供命令和后台线程共享。
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            replay_buffers: Arc::new(Mutex::new(HashMap::new())),
            restoring: Arc::new(Mutex::new(HashSet::new())),
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
                    Some(
                        handle
                            .row
                            .to_dto_with_pane_count(pane_count_for_row(&handle.row)),
                    )
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
    ///     HTTP replay route 需要校验 session 是否仍在运行期 registry 中，但不应暴露私有句柄类型。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复用 contains 的只读 HashMap 检查，作为路由层的公开语义化 helper。
    pub fn session_exists(&self, session_id: &str) -> bool {
        self.contains(session_id)
    }

    /// 原子占位：声明"我即将 restore 这个 session"（Finding 5: TOCTOU 修复）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `restore_persisted_sessions` 的旧实现 `contains() → await resolve_worktree → restore()`
    ///     存在 TOCTOU：两个并发的 sessions/list 请求都能通过 contains() 检查，各自 spawn 一个
    ///     PTY/tmux 窗口，导致同一持久化 session 被恢复两次。本方法把"检查 sessions map + 写入
    ///     restoring 占位集合"放进同一个 `sessions` 锁内原子完成：第一个 caller 拿到 claim，
    ///     第二个并发 caller 看到占位直接跳过。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1. 持 sessions 锁；
    ///     2. 若 sessions map 已有该 session_id（已在运行期），返回 false（无需 restore）；
    ///     3. 若 restoring 集合已有该 session_id（另一个 caller 正在 restore），返回 false；
    ///     4. 否则写入 restoring 集合并返回 true（caller 独占 restore 责任）。
    ///
    /// 返回 true 表示 caller 必须 restore 并在完成后调用 `release_restore_claim`
    ///（或持有 `RestoreClaimGuard` 直至结束）。
    pub fn try_claim_restore(&self, session_id: &str) -> bool {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        if sessions.contains_key(session_id) {
            return false;
        }
        let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        if restoring.contains(session_id) {
            return false;
        }
        restoring.insert(session_id.to_string());
        true
    }

    /// 释放 restore 占位（Finding 5）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     restore 完成（成功或失败）后必须释放占位，否则后续 sessions/list 永远跳过该 session。
    ///     成功路径：spawn_row 已把 session 写入 sessions map，contains 自然命中；
    ///     失败路径：释放占位允许后续请求重试 restore。
    pub fn release_restore_claim(&self, session_id: &str) {
        let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        restoring.remove(session_id);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试与诊断需要知道运行期 registry 当前会话数（含 ghost 残留）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 sessions map 长度。
    pub fn registry_len(&self) -> usize {
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .len()
    }

    /// 列出运行期 registry 中的全部 session id。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Agent runtime reconcile 需要知道内存中仍存活的 terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 sessions map 的 key 集合。
    pub fn registry_session_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .keys()
            .cloned()
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     RAII 故障注入测试需要确认 Drop 后没有存活 child/fake 句柄。
    ///
    /// Code Logic（这个函数做什么）:
    ///     统计 status=running 的会话数（Fake 与 Pty 均计入）。
    pub fn live_child_count(&self) -> usize {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .values()
            .filter(|handle| {
                let handle = handle.lock().expect("workbench session 锁中毒");
                handle.row.status == "running"
            })
            .count()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试需要查询 restore claim 是否仍占用，验证 Drop 是否释放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     检查 restoring 集合是否包含 session_id。
    #[cfg(test)]
    pub fn is_restore_claim_held(&self, session_id: &str) -> bool {
        self.restoring
            .lock()
            .expect("restoring 集合锁中毒")
            .contains(session_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     移动端首次打开终端时需要取得该 session 最近输出，缺少历史时也应得到空快照。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 replay_buffers 克隆对应 session 快照；不存在时返回 lastSeq=0 的空 DTO。
    pub fn replay(&self, session_id: &str) -> WorkbenchSessionReplayDto {
        let buffers = self
            .replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒");
        buffers.get(session_id).map_or_else(
            || WorkbenchSessionReplayDto {
                session_id: session_id.to_string(),
                buffer: String::new(),
                truncated: false,
                last_seq: 0,
            },
            |buffer| buffer.snapshot(session_id),
        )
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在工作台中创建本机终端时，需要在当前 worktree 根目录中启动普通 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 portable-pty pair，cwd 指向 active worktree 路径，spawn 系统 shell，并启动输出与退出监听线程。
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        state: AppState,
        project: WorkbenchProjectRow,
        cwd: String,
        worktree_id: Option<String>,
        worktree_name: Option<String>,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let (cols, rows) = initial_terminal_size(initial_cols, initial_rows);
        let terminal_command = default_terminal_command();
        let agent_ctx = TerminalAgentContextIds {
            project_id: project.id.clone(),
            worktree_id: worktree_id.clone().unwrap_or_default(),
            terminal_session_id: session_id.clone(),
            owner_instance_id: state.config_runtime.owner_instance_id().to_string(),
        };
        let (backend, backend_id, backend_window_id, command) = match available_tmux_command() {
            Some(tmux) => {
                let worktree_tmux_id = tmux_worktree_session_name(
                    &project.name,
                    &project.id,
                    worktree_id.as_deref(),
                    worktree_name.as_deref(),
                );
                match create_tmux_window(
                    &tmux,
                    &worktree_tmux_id,
                    &project.name,
                    &cwd,
                    &terminal_command,
                    Some(&agent_ctx),
                ) {
                    Ok(window_id) => {
                        let target = tmux_window_target(&worktree_tmux_id, &window_id);
                        let display_command = tmux.display_command_for_session(
                            &worktree_tmux_id,
                            Some(&target),
                            &terminal_command,
                        );
                        (
                            TMUX_BACKEND.to_string(),
                            Some(worktree_tmux_id),
                            Some(window_id),
                            display_command,
                        )
                    }
                    Err(error) => {
                        tracing::warn!("工作台 tmux 后端不可用，回退普通 PTY: {error}");
                        (
                            RAW_PTY_BACKEND.to_string(),
                            None,
                            None,
                            terminal_command.clone(),
                        )
                    }
                }
            }
            None => (
                RAW_PTY_BACKEND.to_string(),
                None,
                None,
                terminal_command.clone(),
            ),
        };
        let row = WorkbenchSessionRow {
            id: session_id.clone(),
            project_id: project.id.clone(),
            worktree_id,
            name: project.name.clone(),
            command,
            cwd: cwd.clone(),
            status: "running".to_string(),
            cols,
            rows,
            started_at: now,
            exited_at: None,
            exit_code: None,
            backend,
            backend_id,
            backend_window_id,
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };

        self.spawn_row(state, row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用重启后，持久化的终端 tab 需要重新绑定运行期 PTY；tmux 后端可继续原 shell 上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据持久化 row.backend 恢复 tmux session，必要时把旧 session 改名为可读名称；失败则回退普通 PTY，然后用 row.cwd 启动 reader/exit watcher。
    pub fn restore(
        &self,
        state: AppState,
        project: WorkbenchProjectRow,
        mut row: WorkbenchSessionRow,
        worktree_name: Option<String>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        if row.cwd.trim().is_empty() {
            row.cwd = project.path.clone();
        }
        if row.backend == TMUX_BACKEND {
            if let Some(tmux) = available_tmux_command() {
                let desired_session_name = tmux_worktree_session_name(
                    &project.name,
                    &project.id,
                    row.worktree_id.as_deref(),
                    worktree_name.as_deref(),
                );
                let session_name = migrate_tmux_session_name(
                    &tmux,
                    row.backend_id.as_deref(),
                    &desired_session_name,
                );
                let terminal_command = default_terminal_command();
                let target_exists = if tmux_row_requires_window_recreation(&row) {
                    false
                } else {
                    row.backend_window_id
                        .as_deref()
                        .map(|window_id| {
                            tmux_target_exists(&tmux, &tmux_window_target(&session_name, window_id))
                        })
                        .unwrap_or_else(|| tmux_target_exists(&tmux, &session_name))
                };
                if !target_exists {
                    let agent_ctx = TerminalAgentContextIds {
                        project_id: row.project_id.clone(),
                        worktree_id: row.worktree_id.clone().unwrap_or_default(),
                        terminal_session_id: row.id.clone(),
                        owner_instance_id: state.config_runtime.owner_instance_id().to_string(),
                    };
                    match create_tmux_window(
                        &tmux,
                        &session_name,
                        &row.name,
                        &row.cwd,
                        &terminal_command,
                        Some(&agent_ctx),
                    ) {
                        Ok(window_id) => {
                            let target = tmux_window_target(&session_name, &window_id);
                            row.backend_id = Some(session_name.clone());
                            row.backend_window_id = Some(window_id);
                            row.command = tmux.display_command_for_session(
                                &session_name,
                                Some(&target),
                                &terminal_command,
                            );
                        }
                        Err(error) => {
                            tracing::warn!("恢复工作台 tmux 会话失败，回退普通 PTY: {error}");
                            row.backend = RAW_PTY_BACKEND.to_string();
                            row.backend_id = None;
                            row.backend_window_id = None;
                            row.command = default_terminal_command();
                        }
                    }
                } else if row.backend == TMUX_BACKEND {
                    if let Err(error) = apply_workbench_tmux_status_theme(&tmux, &session_name) {
                        tracing::debug!("恢复工作台终端时应用 tmux status 样式失败: {error}");
                    }
                    let target = row
                        .backend_window_id
                        .as_deref()
                        .map(|window_id| tmux_window_target(&session_name, window_id));
                    row.backend_id = Some(session_name);
                    row.command = tmux.display_command_for_session(
                        row.backend_id.as_deref().expect("tmux session name"),
                        target.as_deref(),
                        &terminal_command,
                    );
                }
            } else {
                tracing::warn!("恢复工作台终端时未找到 tmux，回退普通 PTY");
                row.backend = RAW_PTY_BACKEND.to_string();
                row.backend_id = None;
                row.backend_window_id = None;
                row.command = default_terminal_command();
            }
        }

        row.status = "running".to_string();
        row.exited_at = None;
        row.exit_code = None;
        row.updated_at = chrono::Utc::now().to_rfc3339();
        self.spawn_row(state, row)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建和恢复终端最终都要启动一个 PTY 客户端并注册输出/退出监听。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 row 中的命令/后端信息构造 CommandBuilder，spawn 子进程并写入内存 registry。
    fn spawn_row(
        &self,
        state: AppState,
        row: WorkbenchSessionRow,
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
        let mut cmd = command_builder_for_row(&row, state.config_runtime.owner_instance_id());
        cmd.cwd(PathBuf::from(&row.cwd));
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
        self.ensure_replay_buffer(&session_id);

        emit_status(&state, &session_id, "running", None);
        spawn_reader_thread(
            state.clone(),
            session_id.clone(),
            reader,
            self.replay_buffers.clone(),
        );
        spawn_exit_watcher(state, self.sessions.clone(), session_id.clone(), handle);

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
    ///     顶部 app tab 对应 tmux window；用户切换 tab 时，终端里的 tmux current window 必须同步切换。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 tmux-backed session 取出绑定 window target 并执行 `select-window -t`；raw PTY fallback 无需处理。
    pub fn focus_window(&self, session_id: &str) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let handle = handle.lock().expect("workbench session 锁中毒");
        if handle.row.status != "running" {
            return Err(AppError::generic("工作台会话未运行"));
        }
        if handle.row.backend != TMUX_BACKEND {
            return Ok(());
        }
        let target = tmux_window_target_for_row(&handle.row)?;
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法切换 window"));
        };
        if let Some(session_name) = handle.row.backend_id.as_deref() {
            if let Err(error) = apply_workbench_tmux_status_theme(&tmux, session_name) {
                tracing::debug!("切换工作台 tmux window 时应用 status 样式失败: {error}");
            }
        }
        let args = tmux_select_window_args(&target);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tmux_command(&tmux, &arg_refs)?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可在 tmux status bar 内切换 window，顶部 app tab 应跟随真实 tmux current window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     找出当前 worktree tmux session，读取当前 window id，并映射回 registry 中的 Workbench session id。
    pub fn focused_session_id(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let rows: Vec<WorkbenchSessionRow> = self
            .sessions
            .lock()
            .expect("workbench sessions 锁中毒")
            .values()
            .map(|handle| handle.lock().expect("workbench session 锁中毒").row.clone())
            .collect();
        let Some(backend_id) = rows
            .iter()
            .find(|row| {
                row.project_id == project_id
                    && worktree_id_matches(project_id, row.worktree_id.as_deref(), worktree_id)
                    && row.backend == TMUX_BACKEND
            })
            .and_then(|row| row.backend_id.clone())
        else {
            return Ok(None);
        };
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法读取当前 window"));
        };
        let args = tmux_current_window_args(&backend_id);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let window_id = run_tmux_command(&tmux, &arg_refs)?.trim().to_string();
        if window_id.is_empty() {
            return Ok(None);
        }
        Ok(focused_session_id_for_tmux_window(
            rows.iter(),
            project_id,
            worktree_id,
            &backend_id,
            &window_id,
        ))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户需要在当前 tmux window 内创建左右或上下 pane，复用 tmux 的真实布局能力。
    ///
    /// Code Logic（这个函数做什么）:
    ///     找到会话 row 的 tmux target，把 row.cwd 转换为 tmux cwd 后执行 `split-window -c`。
    pub fn split_pane(
        &self,
        session_id: &str,
        direction: PaneSplitDirection,
    ) -> Result<(), AppError> {
        let handle = self.get_handle(session_id)?;
        let handle = handle.lock().expect("workbench session 锁中毒");
        if handle.row.status != "running" {
            return Err(AppError::generic("工作台会话未运行"));
        }
        let target = tmux_window_target_for_row(&handle.row)?;
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法创建 pane"));
        };
        let tmux_cwd = tmux.project_cwd(&handle.row.cwd)?;
        let args = tmux_split_window_args(direction, &target, &tmux_cwd);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tmux_command(&tmux, &arg_refs)?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户需要从键盘或工具栏把当前 terminal window 的 active pane 切到下一个 pane。
    ///
    /// Code Logic（这个函数做什么）:
    ///     找到 running tmux-backed session 的 window target，并执行 `select-pane -t <target>.+`。
    pub fn switch_to_next_pane(&self, session_id: &str) -> Result<(), AppError> {
        let target = {
            let handle = self.get_handle(session_id)?;
            let handle = handle.lock().expect("workbench session 锁中毒");
            if handle.row.status != "running" {
                return Err(AppError::generic("工作台会话未运行"));
            }
            tmux_window_target_for_row(&handle.row)?
        };
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法切换 pane"));
        };
        let args = tmux_select_next_pane_args(&target);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tmux_command(&tmux, &arg_refs)?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     移动端 pane 操作后应始终只显示当前 active pane，而不是显示 tmux 左右/上下分屏布局。
    ///
    /// Code Logic（这个函数做什么）:
    ///     raw/disconnected session 直接 no-op；running tmux-backed session 单 pane 直接返回，多 pane 时仅在未 zoom 时执行 `resize-pane -Z`。
    pub fn ensure_active_pane_zoomed(&self, session_id: &str) -> Result<(), AppError> {
        let target = {
            let handle = self.get_handle(session_id)?;
            let handle = handle.lock().expect("workbench session 锁中毒");
            if handle.row.status != "running" {
                return Ok(());
            }
            if handle.row.backend != TMUX_BACKEND || handle.row.backend_window_id.is_none() {
                return Ok(());
            }
            tmux_window_target_for_row(&handle.row)?
        };
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法缩放 pane"));
        };
        if tmux_pane_count(&tmux, &target)? <= 1 || tmux_window_is_zoomed(&tmux, &target)? {
            return Ok(());
        }
        let args = tmux_zoom_active_pane_args(&target);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tmux_command(&tmux, &arg_refs)?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户点击分屏工具栏 X 时，需要关闭当前 active pane；最后一个 pane 则关闭整个 window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 `list-panes` 统计 pane 数；多于一个执行 `kill-pane -t <target>`，只有一个则关闭 session/window。
    pub fn close_active_pane(&self, session_id: &str) -> Result<PaneCloseOutcome, AppError> {
        let target = {
            let handle = self.get_handle(session_id)?;
            let handle = handle.lock().expect("workbench session 锁中毒");
            if handle.row.status != "running" {
                return Err(AppError::generic("工作台会话未运行"));
            }
            tmux_window_target_for_row(&handle.row)?
        };
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::generic("未找到 tmux，无法关闭 pane"));
        };
        let pane_count = tmux_pane_count(&tmux, &target)?;
        match pane_close_plan(pane_count) {
            PaneClosePlan::KillPane => {
                run_tmux_command(&tmux, &["kill-pane", "-t", &target])?;
                Ok(PaneCloseOutcome::PaneClosed)
            }
            PaneClosePlan::CloseWindow => {
                let row = self.close(session_id)?;
                Ok(PaneCloseOutcome::WindowClosed(row))
            }
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
        self.replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒")
            .remove(session_id);
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
        let next_name = name.trim().to_string();
        if handle.row.backend == TMUX_BACKEND {
            if let Some(tmux) = available_tmux_command() {
                if let Ok(target) = tmux_window_target_for_row(&handle.row) {
                    if let Err(error) =
                        run_tmux_command(&tmux, &["rename-window", "-t", &target, &next_name])
                    {
                        tracing::debug!("重命名 tmux window 失败: {error}");
                    }
                }
            }
        }
        handle.row.name = next_name;
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

    /// Business Logic（为什么需要这个函数）:
    ///     创建、恢复或测试插入会话时，需要确保 replay map 有对应 session 的缓存槽位。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 replay_buffers 中按 session_id 懒插入默认容量的 SessionReplayBuffer。
    fn ensure_replay_buffer(&self, session_id: &str) {
        self.replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒")
            .entry(session_id.to_string())
            .or_insert_with(|| SessionReplayBuffer::new(SESSION_REPLAY_MAX_CHARS));
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
            worktree_id: None,
            name: format!("session-{session_id}"),
            command: default_terminal_command_from_env(Some("/bin/sh".into())),
            cwd: "/tmp/project".to_string(),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: "2026-06-24T00:00:00Z".to_string(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.to_string(),
            backend_id: None,
            backend_window_id: None,
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
        self.ensure_replay_buffer(session_id);
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
///     前端终端需要持续接收 PTY stdout/stderr 合并后的输出；Agent OSC 不得进入 UI。
///
/// Code Logic（这个函数做什么）:
///     后台线程读 PTY → `AgentOscDecoder` 剥离 app-private OSC 并 enqueue mutation →
///     仅 visible 字节进入 `TerminalUtf8Decoder` → replay/emit。
fn spawn_reader_thread(
    state: AppState,
    session_id: String,
    mut reader: Box<dyn Read + Send>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut seq: u64 = 0;
        let mut utf8 = TerminalUtf8Decoder::default();
        let mut osc = AgentOscDecoder::default();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let decoded = osc.push(&buf[..n]);
                    apply_agent_osc_decode_result(
                        &state,
                        &session_id,
                        &mut seq,
                        &mut utf8,
                        &replay_buffers,
                        decoded,
                    );
                    // 突发 rate-limit 后若仍有 pending，在窗口到期时 idle flush，避免静默丢终态
                    while osc.has_pending_coalesce() {
                        if let Some(wait) = osc.duration_until_rate_window_end() {
                            if !wait.is_zero() {
                                thread::sleep(wait);
                            }
                        }
                        let flushed = osc.poll_flush();
                        if flushed.mutations.is_empty() && flushed.diagnostics.is_empty() {
                            break;
                        }
                        apply_agent_osc_decode_result(
                            &state,
                            &session_id,
                            &mut seq,
                            &mut utf8,
                            &replay_buffers,
                            flushed,
                        );
                    }
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出结束: {error}");
                    break;
                }
            }
        }
        // 会话结束：强制冲刷仍挂起的 coalesced mutation
        let flushed = osc.force_flush_pending();
        apply_agent_osc_decode_result(
            &state,
            &session_id,
            &mut seq,
            &mut utf8,
            &replay_buffers,
            flushed,
        );
        if let Some(chunk) = utf8.finish() {
            emit_terminal_output(&state, &session_id, &mut seq, chunk, &replay_buffers);
        }
    });
}

/// 处理一次 OSC 解码结果：enqueue mutation、记诊断、转发可见字节。
///
/// Business Logic（为什么需要这个函数）:
///     push 与 idle poll_flush / force_flush 共用同一出口，避免终态只在一种路径上入站。
///
/// Code Logic（这个函数做什么）:
///     forward mutations → debug diagnostics → 非空 visible 走 UTF-8/emit。
fn apply_agent_osc_decode_result(
    state: &AppState,
    session_id: &str,
    seq: &mut u64,
    utf8: &mut TerminalUtf8Decoder,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
    decoded: crate::workbench::agent_runtime::osc::AgentOscDecodeResult,
) {
    forward_agent_osc_mutations(state, session_id, decoded.mutations);
    for diag in &decoded.diagnostics {
        tracing::debug!(
            session_id = %session_id,
            code = diag.code,
            detail = ?diag.detail,
            "agent osc diagnostic"
        );
    }
    if !decoded.visible.is_empty() {
        emit_terminal_output(
            state,
            session_id,
            seq,
            utf8.decode(&decoded.visible),
            replay_buffers,
        );
    }
}

/// 把 OSC mutation 交给 owner ingress（有界 channel；未启动 reducer 时丢弃）。
///
/// Business Logic（为什么需要这个函数）:
///     剥离后的结构化事件必须离开 reader 热路径，由单一 worker 串行写库。
///
/// Code Logic（这个函数做什么）:
///     校验非空后调用 `try_enqueue_agent_mutation`；不在此执行 SQL。
fn forward_agent_osc_mutations(
    state: &AppState,
    terminal_session_id: &str,
    mutations: Vec<AgentRuntimeMutation>,
) {
    for mutation in mutations {
        if mutation.terminal_session_id != terminal_session_id {
            tracing::debug!(
                terminal = %terminal_session_id,
                claimed = %mutation.terminal_session_id,
                "agent osc terminal_session_id mismatch; still enqueue for reducer reject"
            );
        }
        try_enqueue_agent_mutation(state, mutation);
    }
}

/// Business Logic（为什么需要这个函数）:
///     终端输出事件需要统一递增 seq，且纯 pending chunk 未完成时不应发送空事件。
///
/// Code Logic（这个函数做什么）:
///     非空 chunk 才递增 seq、写入 replay buffer，并构造 `TerminalOutputEvent` 通过后端 UI adapter emit。
fn emit_terminal_output(
    state: &AppState,
    session_id: &str,
    seq: &mut u64,
    chunk: String,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) {
    if chunk.is_empty() {
        return;
    }
    *seq += 1;
    {
        let mut buffers = replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒");
        if let Some(buffer) = buffers.get_mut(session_id) {
            buffer.append(&chunk, *seq);
        }
    }
    let event = WorkbenchTerminalOutputPayload {
        session_id: session_id.to_string(),
        chunk,
        seq: *seq,
        ts: now_millis(),
    };
    publish_workbench_remote_event_from_state(
        state,
        WorkbenchRemoteEvent::TerminalOutput(event.clone()),
    );
    state.emit_event("workbench:terminal-output", event.clone());
    crate::orchestrator::completion::spawn_maybe_handle_session_output_for_state(
        state.clone(),
        session_id.to_string(),
        event.chunk.clone(),
    );
}

/// Business Logic（为什么需要这个函数）:
///     终端进程退出后，前端需要收到状态变化并保留退出码。
///
/// Code Logic（这个函数做什么）:
///     后台线程短轮询 child.try_wait，退出时更新 DTO 并 emit exited 状态事件。
fn spawn_exit_watcher(
    state: AppState,
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
                    emit_status(&state, &session_id, "exited", Some(exit_code));
                }
                break;
            }
            Some(Err(error)) => {
                tracing::warn!("查询工作台终端退出状态失败: {error}");
                emit_status(&state, &session_id, "disconnected", None);
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
///     构造 `TerminalStatusEvent` 并通过后端 UI adapter 发送。
fn emit_status(state: &AppState, session_id: &str, status: &str, exit_code: Option<i32>) {
    let event = WorkbenchTerminalStatusPayload {
        session_id: session_id.to_string(),
        status: status.to_string(),
        exit_code,
        ts: now_millis(),
    };
    publish_workbench_remote_event_from_state(
        state,
        WorkbenchRemoteEvent::TerminalStatus(event.clone()),
    );
    state.emit_event("workbench:terminal-status", event);
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

    /// Business Logic（为什么需要这个函数）:
    ///     tmux window 映射测试需要快速构造持久化 row，避免启动真实 PTY 或 tmux。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回一个 running tmux WorkbenchSessionRow，backend_id 使用 project_id + worktree_id 派生的 worktree session 名。
    fn fake_tmux_row(
        session_id: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        window_id: &str,
    ) -> WorkbenchSessionRow {
        WorkbenchSessionRow {
            id: session_id.to_string(),
            project_id: project_id.to_string(),
            worktree_id: worktree_id.map(str::to_string),
            name: session_id.to_string(),
            command: "/bin/sh".to_string(),
            cwd: "/tmp/project".to_string(),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: "2026-06-24T00:00:00Z".to_string(),
            exited_at: None,
            exit_code: None,
            backend: TMUX_BACKEND.to_string(),
            backend_id: Some(tmux_worktree_session_name(
                project_id,
                project_id,
                worktree_id,
                None,
            )),
            backend_window_id: Some(window_id.to_string()),
            created_at: "2026-06-24T00:00:00Z".to_string(),
            updated_at: "2026-06-24T00:00:00Z".to_string(),
        }
    }

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
    ///     repo upsert 失败时必须回收刚 spawn 的 attach，禁止 ghost registry/child。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert fake session → SessionSpawnGuard 不 commit → Drop → registry 空且无 live child。
    #[test]
    fn repo_failure_closes_spawned_session() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-spawn-1", "p1");
        assert_eq!(registry.registry_len(), 1);
        assert_eq!(registry.live_child_count(), 1);

        {
            let _guard = SessionSpawnGuard::new(registry.clone(), "s-spawn-1".to_string());
            // 模拟 upsert 失败：不 commit，离开作用域触发 Drop。
        }

        assert_eq!(registry.registry_len(), 0);
        assert_eq!(registry.live_child_count(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     upsert 成功后必须 commit，否则 Drop 会误杀合法 session。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert → guard.commit → drop → session 仍在 registry。
    #[test]
    fn session_spawn_guard_commit_keeps_session() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-keep", "p1");
        {
            let mut guard = SessionSpawnGuard::new(registry.clone(), "s-keep".to_string());
            guard.commit();
        }
        assert_eq!(registry.registry_len(), 1);
        assert!(registry.contains("s-keep"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     restore 中途失败时 Drop 必须释放 claim，允许后续重试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     try_claim → RestoreClaimGuard 不 disarm → Drop → claim 已释放，可再次 claim。
    #[test]
    fn restore_claim_guard_releases_on_drop() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-restore"));
        {
            let _guard = RestoreClaimGuard::new(registry.clone(), "s-restore".to_string());
        }
        assert!(!registry.is_restore_claim_held("s-restore"));
        assert!(registry.try_claim_restore("s-restore"));
        registry.release_restore_claim("s-restore");
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
    ///     Agent OSC 不得进入 replay buffer / terminal UI 字节流。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 AgentOscDecoder 处理夹杂 OSC 的输出，断言 visible 无 base64 载荷且含前后文。
    #[test]
    fn agent_osc_payload_never_enters_replay_visible_bytes() {
        use crate::workbench::agent_runtime::{encode_agent_osc_frame, AgentSessionPhase};
        let mut osc = crate::workbench::agent_runtime::AgentOscDecoder::default();
        let frame = encode_agent_osc_frame(
            "agent-1",
            "session-1",
            AgentSessionPhase::Working,
            2,
            "2026-07-15T00:00:00Z",
        );
        let mut input = b"hello ".to_vec();
        input.extend_from_slice(&frame);
        input.extend_from_slice(b"world");
        let decoded = osc.push(&input);
        assert_eq!(decoded.visible, b"hello world");
        assert!(
            !String::from_utf8_lossy(&decoded.visible).contains("agentSessionId"),
            "OSC JSON must not leak into visible"
        );
        assert_eq!(decoded.mutations.len(), 1);
        // 写入 replay 的只能是 visible 转 UTF-8 后的文本
        let mut buffer = SessionReplayBuffer::new(10_000);
        let text = String::from_utf8_lossy(&decoded.visible).into_owned();
        buffer.append(&text, 1);
        let snap = buffer.snapshot("session-1");
        assert_eq!(snap.buffer, "hello world");
        assert!(!snap.buffer.contains("cc-partner-agent-v1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端首次打开远端终端时需要拉取最近输出，且历史输出超过内存上限时只能保留尾部。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用小容量 replay buffer 追加超过上限的中文和 emoji 输出，断言按 char 边界截断、truncated 与 lastSeq 正确。
    #[test]
    fn session_replay_buffer_keeps_recent_output_with_last_seq() {
        let mut buffer = SessionReplayBuffer::new(3);

        buffer.append("hello", 1);
        buffer.append("世界🙂", 2);
        buffer.append("再见", 3);
        let snapshot = buffer.snapshot("session-1");

        assert_eq!(snapshot.session_id, "session-1");
        assert_eq!(snapshot.buffer, "🙂再见");
        assert!(snapshot.truncated);
        assert_eq!(snapshot.last_seq, 3);
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
            backend.display_command_for_session("cc-partner-session", None, "cmd.exe"),
            "wsl.exe --exec tmux attach-session -t cc-partner-session"
        );
        assert_eq!(
            backend.display_command_for_session(
                "cc-partner-session",
                Some("cc-partner-session:@7"),
                "cmd.exe"
            ),
            "wsl.exe --exec tmux attach-session -t cc-partner-session ; switch-client -t cc-partner-session:@7"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     真实 tmux 映射下，一个 worktree 应稳定对应一个 tmux session，worktree 内 tab 对应 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 project + worktree 派生出稳定 session 名，window target 使用 `session:@window` 语法。
    #[test]
    fn tmux_worktree_session_and_window_target_are_stable() {
        let worktree_session = tmux_worktree_session_name(
            "cc-partner",
            "project-1234-abcd",
            Some("project-1234-abcd:main"),
            Some("main"),
        );

        assert_eq!(worktree_session, "cc-partner-main-1234abcdmain");
        assert_eq!(
            tmux_window_target(&worktree_session, "@7"),
            "cc-partner-main-1234abcdmain:@7"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户会直接看到 tmux status 左侧的 session 名，内部 worktree id/hash 不应成为主要可读名称。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 session 名优先使用用户可见 worktree 名，并用 worktree id 的短组件保持稳定区分。
    #[test]
    fn tmux_worktree_session_prefers_readable_worktree_name() {
        let worktree_session = tmux_worktree_session_name(
            "cc-partner",
            "project-84b44f3d8e25",
            Some("internal-worktree-84b44f3d8e25"),
            Some("feature/PandoCanvas"),
        );

        assert_eq!(
            worktree_session,
            "cc-partner-feature-pandocanvas-84b44f3d8e25"
        );
        assert!(!worktree_session.starts_with("cc-partner-worktree-"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一项目下不同 worktree 的 tmux status/window 列表必须互相隔离。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言同一 project_id 搭配不同 worktree_id 会生成不同 backend_id。
    #[test]
    fn tmux_worktree_session_differs_between_worktrees() {
        let main_session = tmux_worktree_session_name(
            "cc-partner",
            "project-1",
            Some("project-1:main"),
            Some("main"),
        );
        let feature_session = tmux_worktree_session_name(
            "cc-partner",
            "project-1",
            Some("worktree-2"),
            Some("feature/ui"),
        );

        assert_ne!(main_session, feature_session);
        assert_eq!(main_session, "cc-partner-main-project1main");
        assert_eq!(feature_session, "cc-partner-feature-ui-worktree2");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     可读 worktree 名可能在清洗后相同，但底层 tmux session 仍必须按真实 worktree 隔离。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造两个显示名清洗后相同、内部 id 不同的 worktree，断言 session 名不会碰撞。
    #[test]
    fn tmux_worktree_session_keeps_worktree_isolation_when_names_collide() {
        let slash_session = tmux_worktree_session_name(
            "cc-partner",
            "project-1",
            Some("worktree-alpha-123456789abc"),
            Some("feature/ui"),
        );
        let dash_session = tmux_worktree_session_name(
            "cc-partner",
            "project-1",
            Some("worktree-beta-abcdef123456"),
            Some("feature-ui"),
        );

        assert_ne!(slash_session, dash_session);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 运行在 GUI/Tauri 环境时可能继承 `TERM=dumb`，tmux attach 会把终端响应错误送进 pane。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言所有工作台 PTY 命令都会显式声明 xterm 兼容终端环境和真彩色能力。
    #[test]
    fn workbench_terminal_env_overrides_dumb_parent_term() {
        let mut command = CommandBuilder::new("/bin/sh");
        apply_workbench_terminal_env(&mut command, None);

        assert_eq!(
            command.get_env("TERM").and_then(|value| value.to_str()),
            Some("xterm-256color")
        );
        assert_eq!(
            command
                .get_env("COLORTERM")
                .and_then(|value| value.to_str()),
            Some("truecolor")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Agent Hook 依赖四条非敏感 CC_PARTNER_*_ID；不得注入 control/device token。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply 带 agent ctx 后断言四 ID 存在，且无 *TOKEN* / credential 键。
    #[test]
    fn workbench_terminal_env_includes_stable_partner_ids_without_tokens() {
        let mut command = CommandBuilder::new("/bin/sh");
        let ctx = TerminalAgentContextIds {
            project_id: "proj-1".to_string(),
            worktree_id: "wt-1".to_string(),
            terminal_session_id: "term-1".to_string(),
            owner_instance_id: "owner-1".to_string(),
        };
        apply_workbench_terminal_env(&mut command, Some(&ctx));

        assert_eq!(
            command
                .get_env("CC_PARTNER_PROJECT_ID")
                .and_then(|v| v.to_str()),
            Some("proj-1")
        );
        assert_eq!(
            command
                .get_env("CC_PARTNER_WORKTREE_ID")
                .and_then(|v| v.to_str()),
            Some("wt-1")
        );
        assert_eq!(
            command
                .get_env("CC_PARTNER_TERMINAL_SESSION_ID")
                .and_then(|v| v.to_str()),
            Some("term-1")
        );
        assert_eq!(
            command
                .get_env("CC_PARTNER_OWNER_INSTANCE_ID")
                .and_then(|v| v.to_str()),
            Some("owner-1")
        );
        assert!(command.get_env("CC_PARTNER_CONTROL_TOKEN").is_none());
        assert!(command.get_env("CC_PARTNER_DEVICE_TOKEN").is_none());
        assert!(command.get_env("CC_PARTNER_AUTH_TOKEN").is_none());

        let tmux_args = tmux_agent_context_env_args(&ctx);
        assert_eq!(tmux_args.len() % 2, 0);
        for pair in tmux_args.chunks(2) {
            assert_eq!(pair[0], "-e");
            assert!(pair[1].starts_with("CC_PARTNER_"));
            assert!(!pair[1].contains("TOKEN"));
        }
        assert!(tmux_args
            .iter()
            .any(|a| a == "CC_PARTNER_PROJECT_ID=proj-1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     前端 terminal window 必须绑定到对应 tmux window，不能只 attach 到 worktree session 的当前 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 attach 参数先连接 worktree session，再用 switch-client 指向具体 `session:@window` target。
    #[test]
    fn tmux_attach_window_args_switch_client_to_window_target() {
        let args = tmux_attach_window_args(
            "cc-partner-project-project1234abcd",
            "cc-partner-project-project1234abcd:@7",
        );

        assert_eq!(
            args,
            vec![
                "attach-session",
                "-t",
                "cc-partner-project-project1234abcd",
                ";",
                "switch-client",
                "-t",
                "cc-partner-project-project1234abcd:@7",
            ]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台浅色/深色主题切换时，tmux 底部 status bar 不应继承用户 tmux 配置里的深色背景、彩色右侧时间或 underline。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Workbench 使用无内嵌颜色的 status/window format，并保留 session/window 标签的结构。
    #[test]
    fn tmux_status_theme_commands_use_light_safe_label_style() {
        let commands = tmux_status_theme_commands("cc-partner-project-project1234abcd");

        assert_eq!(
            commands,
            vec![
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-style",
                    "fg=default,bg=default",
                ],
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-left-style",
                    "fg=default,bg=default",
                ],
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-right-style",
                    "fg=default,bg=default",
                ],
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-left",
                    "#[bold]#S › ",
                ],
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-right",
                    "%H:%M | %Y-%m-%d ",
                ],
                vec![
                    "set-window-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "window-status-style",
                    "fg=default,bg=default",
                ],
                vec![
                    "set-window-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "window-status-current-style",
                    "fg=black,bg=colour111,bold",
                ],
                vec![
                    "set-window-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "window-status-format",
                    " #I:#W#F ",
                ],
                vec![
                    "set-window-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "window-status-current-format",
                    " #I:#W#F ",
                ],
                vec![
                    "set-window-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "window-status-separator",
                    " ",
                ],
            ]
        );
        let joined = commands
            .iter()
            .flat_map(|command| command.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!joined.contains("underscore"));
        assert!(!joined.contains("fg=#"));
        assert!(!joined.contains("bg=#"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     顶部 app tab 切换时，底部 tmux 当前 window 也必须跟着切换到 tab 绑定的真实 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 focus 操作使用 `select-window -t <session:@window>` 切 worktree tmux session 的 current window。
    #[test]
    fn tmux_select_window_args_targets_bound_window() {
        let args = tmux_select_window_args("cc-partner-project-project1234abcd:@7");

        assert_eq!(
            args,
            vec![
                "select-window",
                "-t",
                "cc-partner-project-project1234abcd:@7",
            ]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户在 tmux 底部状态栏切换 window 后，cc-partner 需要读取 worktree tmux session 的当前 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言查询 current window 使用 `display-message -p -t <session> #{window_id}`。
    #[test]
    fn tmux_current_window_args_read_session_current_window_id() {
        let args = tmux_current_window_args("cc-partner-project-project1234abcd");

        assert_eq!(
            args,
            vec![
                "display-message",
                "-p",
                "-t",
                "cc-partner-project-project1234abcd",
                "#{window_id}",
            ]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     session 命名规则升级时，仍存在的旧 tmux session 应通过 rename 保留 shell 上下文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言迁移使用 `rename-session -t <old> <new>` 参数。
    #[test]
    fn tmux_rename_session_args_preserve_existing_context() {
        let args =
            tmux_rename_session_args("cc-partner-worktree-old-id", "cc-partner-readable-name");

        assert_eq!(
            args,
            vec![
                "rename-session",
                "-t",
                "cc-partner-worktree-old-id",
                "cc-partner-readable-name",
            ]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     后端读到 tmux current window 后，需要映射回前端顶部应该选中的 app tab。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造同一 worktree tmux session 内两个 window row，断言 window id 命中第二个 sessionId。
    #[test]
    fn focused_session_id_matches_worktree_backend_window_id() {
        let first = fake_tmux_row("session-1", "project-1", Some("project-1:main"), "@1");
        let second = fake_tmux_row("session-2", "project-1", Some("project-1:main"), "@2");
        let other_project = fake_tmux_row("session-3", "project-2", Some("project-2:main"), "@2");
        let backend_id = second.backend_id.clone().expect("tmux backend id");

        let focused = focused_session_id_for_tmux_window(
            [&first, &second, &other_project],
            "project-1",
            Some("project-1:main"),
            &backend_id,
            "@2",
        );

        assert_eq!(focused, Some("session-2".to_string()));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户在 feature worktree 的 tmux status bar 切换 window 时，主工作区 tab 不应被误选中。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造同一项目、相同 tmux window id、不同 worktree/backend 的 row，断言 focused 映射按 worktree 过滤。
    #[test]
    fn focused_session_id_does_not_cross_worktree_scope() {
        let main = fake_tmux_row("main-session", "project-1", Some("project-1:main"), "@2");
        let feature = fake_tmux_row("feature-session", "project-1", Some("worktree-2"), "@2");
        let feature_backend = feature.backend_id.clone().expect("feature backend");

        let focused = focused_session_id_for_tmux_window(
            [&main, &feature],
            "project-1",
            Some("worktree-2"),
            &feature_backend,
            "@2",
        );

        assert_eq!(focused, Some("feature-session".to_string()));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     pane 操作应复用 tmux 原生命令，避免前端伪分屏和真实终端布局分裂。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 split right/down 会生成 tmux `split-window -h/-v` 参数。
    #[test]
    fn tmux_split_direction_maps_to_tmux_arguments() {
        assert_eq!(PaneSplitDirection::Right.tmux_flag(), "-h");
        assert_eq!(PaneSplitDirection::Down.tmux_flag(), "-v");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     通过分屏按钮创建的新 pane 应从项目根目录启动，不能继承当前 pane 里用户 cd 后的目录。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 split-window 参数包含 `-c <project_root>`，并保留方向与 target 参数。
    #[test]
    fn tmux_split_window_args_pin_project_root_cwd() {
        let args = tmux_split_window_args(
            PaneSplitDirection::Right,
            "cc-partner-project-p1:@2",
            "/Users/hans/project",
        );

        assert_eq!(
            args,
            vec![
                "split-window",
                "-h",
                "-t",
                "cc-partner-project-p1:@2",
                "-c",
                "/Users/hans/project",
            ]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户需要在同一个 terminal window 内循环切换到下一个 pane，不能创建新 pane 或跨 window 切换。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 next-pane 操作生成 `select-pane -t <window-target>.+` 参数。
    #[test]
    fn tmux_select_next_pane_args_targets_next_pane_in_current_window() {
        let args = tmux_select_next_pane_args("cc-partner-project-p1:@2");

        assert_eq!(
            args,
            vec!["select-pane", "-t", "cc-partner-project-p1:@2.+"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端需要始终只显示当前 active pane，后端必须能把当前 pane zoom 到整个 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 ensure zoom 操作生成 `resize-pane -Z -t <window-target>` 参数。
    #[test]
    fn tmux_zoom_active_pane_args_targets_current_window() {
        let args = tmux_zoom_active_pane_args("cc-partner-project-p1:@2");

        assert_eq!(
            args,
            vec!["resize-pane", "-Z", "-t", "cc-partner-project-p1:@2"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     分屏工具栏的 X 应关闭当前 active pane；只有最后一个 pane 时应关闭 window，而不是报错。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 pane 数为 1 或 0 时选择关闭 window，pane 数大于 1 时选择 kill-pane。
    #[test]
    fn single_pane_close_plan_closes_window_instead_of_error() {
        assert_eq!(pane_close_plan(0), PaneClosePlan::CloseWindow);
        assert_eq!(pane_close_plan(1), PaneClosePlan::CloseWindow);
        assert_eq!(pane_close_plan(2), PaneClosePlan::KillPane);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     项目列表需要展示真实 pane 数，后端必须能从 tmux `list-panes` 输出得到稳定计数。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言空行会被忽略，非空 pane id 行会累计为 paneCount。
    #[test]
    fn pane_count_from_tmux_output_ignores_empty_lines() {
        assert_eq!(pane_count_from_tmux_output("%1\n\n%2\n"), 2);
        assert_eq!(pane_count_from_tmux_output("\n"), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     关闭最后一个 pane 会关闭所属 window；如果它也是 worktree tmux session 的最后一个 window，必须销毁 session。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 window_count 为 1 时生成 kill-session，多 window 时才生成 kill-window。
    #[test]
    fn tmux_destroy_backend_args_kill_session_for_last_window() {
        assert_eq!(
            tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(1)),
            vec!["kill-session", "-t", "cc-partner-project-p1"]
        );
        assert_eq!(
            tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(2)),
            vec!["kill-window", "-t", "cc-partner-project-p1:@1"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧版本把 tab 映射成独立 tmux session；升级后应迁移到 worktree session 内的 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造缺少 backend_window_id 的 tmux row，断言恢复流程会判定它需要重建 window。
    #[test]
    fn old_tmux_rows_without_window_id_require_window_recreation() {
        let row = WorkbenchSessionRow {
            id: "s1".to_string(),
            project_id: "p1".to_string(),
            worktree_id: None,
            name: "Terminal".to_string(),
            command: "/bin/zsh".to_string(),
            cwd: "/tmp/project".to_string(),
            status: "running".to_string(),
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            started_at: "2026-06-24T00:00:00Z".to_string(),
            exited_at: None,
            exit_code: None,
            backend: TMUX_BACKEND.to_string(),
            backend_id: Some("cc-partner-legacy".to_string()),
            backend_window_id: None,
            created_at: "2026-06-24T00:00:00Z".to_string(),
            updated_at: "2026-06-24T00:00:00Z".to_string(),
        };

        assert!(tmux_row_requires_window_recreation(&row));
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

    /// Business Logic（Finding 5: 为什么需要这个测试）:
    ///     空 registry 上首次 claim 一个未运行也未被占位的 session 应成功，再次 claim 应失败
    ///     （占位生效），从而消除并发 sessions/list 的 TOCTOU。
    #[test]
    fn try_claim_restore_serializes_concurrent_restore_for_same_session() {
        let registry = WorkbenchSessionRegistry::new();

        let first = registry.try_claim_restore("s1");
        assert!(first, "首次 claim 应成功");

        let second = registry.try_claim_restore("s1");
        assert!(!second, "占位期间第二次 claim 应失败，避免重复 restore");

        // 释放占位后允许后续重试。
        registry.release_restore_claim("s1");
        let third = registry.try_claim_restore("s1");
        assert!(third, "释放占位后应允许重新 claim");
        registry.release_restore_claim("s1");
    }

    /// Business Logic（Finding 5: 为什么需要这个测试）:
    ///     session 已在运行期 registry 时，claim 应直接失败（contains 命中），不写入占位，
    ///     避免对活跃 session 做无意义的 restore。
    #[test]
    fn try_claim_restore_returns_false_when_session_already_live() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("live-1", "p1");

        let claimed = registry.try_claim_restore("live-1");
        assert!(!claimed, "session 已在运行期 registry 时 claim 应失败");
        // 释放不存在的占位应是 no-op，不应 panic。
        registry.release_restore_claim("live-1");
    }

    /// Business Logic（Finding 5: 为什么需要这个测试）:
    ///     不同 session_id 的 claim 互不干扰，确保并发恢复多个持久化 tab 不互相阻塞。
    #[test]
    fn try_claim_restore_independent_for_different_sessions() {
        let registry = WorkbenchSessionRegistry::new();

        let a = registry.try_claim_restore("s-a");
        let b = registry.try_claim_restore("s-b");
        assert!(a && b, "不同 session 的 claim 应互不干扰");

        registry.release_restore_claim("s-a");
        registry.release_restore_claim("s-b");
    }

    /// Business Logic（Finding 5: 为什么需要这个测试）:
    ///     release_restore_claim 对未占位的 session 必须是幂等 no-op，不应 panic，
    ///     因为 restore_persisted_sessions 在多个 early-return 路径上调用它。
    #[test]
    fn release_restore_claim_is_idempotent() {
        let registry = WorkbenchSessionRegistry::new();
        // 未 claim 直接 release — 不应 panic。
        registry.release_restore_claim("never-claimed");
        // 双重 release — 不应 panic。
        registry.try_claim_restore("s1");
        registry.release_restore_claim("s1");
        registry.release_restore_claim("s1");
    }
}
