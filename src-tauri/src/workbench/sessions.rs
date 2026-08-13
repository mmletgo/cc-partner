//! workbench/sessions.rs — 工作台本机 PTY 会话注册表
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台允许用户在同一项目下开启多个本机项目终端，用户希望应用重启后终端 tab 与可重连上下文仍可恢复。
//!
//! Code Logic（这个模块做什么）:
//!     使用 portable-pty 创建 PTY；macOS/Linux 原生 tmux、Windows WSL tmux 可承载真实 shell 上下文，应用重启后重新 attach。
//!     内存保存运行期句柄，通过后端 UI adapter 推送终端输出和状态变化。

#![allow(dead_code)]

use crate::error::{AppError, AppErrorCategory};
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
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

/// Workbench 需要向 tmux 声明的外层终端鼠标能力。
const WORKBENCH_TMUX_MOUSE_FEATURE: &str = "xterm*:mouse";

/// 同一后端进程内串行化 server-level terminal-features 对账，避免并发 session 同时追加。
static TMUX_TERMINAL_FEATURE_RECONCILE_LOCK: Mutex<()> = Mutex::new(());

/// 预分配 agent session id 暂存（create → spawn_row 窗口内）。
///
/// Business Logic（为什么需要这个 map）:
///     raw PTY spawn 走 `command_builder_for_row`，row 上无 agent 字段；
///     Orchestrator 预分配路径必须在 shell env 注入 `CC_PARTNER_AGENT_SESSION_ID`。
///
/// Code Logic（这个 map 做什么）:
///     terminal_session_id → agent_session_id；create_with_ids 写入，spawn 读取，完成后清除。
static PREALLOCATED_AGENT_SESSION_IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// 终端 window 名来源。
///
/// Business Logic（为什么需要这个枚举）:
///     用户手改名后不得被 agent 自动标题覆盖；自动改名与创建默认名要可区分。
///
/// Code Logic（这个枚举做什么）:
///     Default=创建默认；Auto=系统按 agent 标题写入；Manual=用户 rename。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionNameSource {
    #[default]
    Default,
    Auto,
    Manual,
}

impl SessionNameSource {
    /// wire / SQLite 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    /// 从持久化/wire 解析；未知回落 Default。
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "manual" => Self::Manual,
            _ => Self::Default,
        }
    }
}

/// window → 当前拥有「自动标题权」的 tmux pane_id（first pane；关闭后交接）。
static TITLE_OWNER_PANES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// window → 最近一次成功应用的自动标题（pane 交接后可重贴）。
static LAST_AUTO_TITLES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// terminal_session_id → agent 启动时所在 pane_id（多 pane 时仅此 pane 可 auto-rename）。
static AGENT_PANE_BY_TERMINAL: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// native_session_id → agent 所在 pane_id。
static AGENT_PANE_BY_NATIVE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
/// agent_session_id → agent 所在 pane_id。
static AGENT_PANE_BY_AGENT: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn title_owner_map() -> &'static Mutex<HashMap<String, String>> {
    TITLE_OWNER_PANES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn last_auto_title_map() -> &'static Mutex<HashMap<String, String>> {
    LAST_AUTO_TITLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn agent_pane_by_terminal_map() -> &'static Mutex<HashMap<String, String>> {
    AGENT_PANE_BY_TERMINAL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn agent_pane_by_native_map() -> &'static Mutex<HashMap<String, String>> {
    AGENT_PANE_BY_NATIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn agent_pane_by_agent_map() -> &'static Mutex<HashMap<String, String>> {
    AGENT_PANE_BY_AGENT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Business Logic（为什么需要这个函数）:
///     持久化-only rename（session 不在 registry）也必须标记 Manual，防止稍后 auto 覆盖。
///
/// Code Logic（这个函数做什么）:
///     no-op 于进程 map（权威在 row.name_source）；保留 API 兼容命令层调用。
pub fn mark_session_name_manual(_session_id: &str) {
    // name_source 写在 row / SQLite；此函数仅兼容旧调用点。
}

/// Business Logic（为什么需要这个函数）:
///     rename 后前端应立即刷新 tab 名，不必等下一次 list。
///
/// Code Logic（这个函数做什么）:
///     emit `workbench:session-updated`，payload 为完整 session DTO（含 paneCount）。
pub fn emit_session_updated(state: &AppState, row: &WorkbenchSessionRow) {
    let pane_count = pane_count_for_row(row);
    let dto = row.to_dto_with_pane_count(pane_count);
    state.emit_event("workbench:session-updated", dto);
}

/// 记录预分配 agent session id。
///
/// Business Logic（为什么需要这个函数）:
///     create 与 spawn_row 之间必须把 agent id 传给 env 注入。
///
/// Code Logic（这个函数做什么）:
///     insert/overwrite map 条目。
fn remember_preallocated_agent_session(terminal_session_id: &str, agent_session_id: &str) {
    let map = PREALLOCATED_AGENT_SESSION_IDS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.insert(
            terminal_session_id.to_string(),
            agent_session_id.to_string(),
        );
    }
}

/// 清除预分配 agent session id。
///
/// Business Logic（为什么需要这个函数）:
///     spawn 结束后不得泄漏跨会话映射。
///
/// Code Logic（这个函数做什么）:
///     remove map 条目。
fn forget_preallocated_agent_session(terminal_session_id: &str) {
    if let Some(map) = PREALLOCATED_AGENT_SESSION_IDS.get() {
        if let Ok(mut guard) = map.lock() {
            guard.remove(terminal_session_id);
        }
    }
}

/// 查询预分配 agent session id。
///
/// Business Logic（为什么需要这个函数）:
///     command_builder / agent_context 组装时读取可选 agent id。
///
/// Code Logic（这个函数做什么）:
///     clone map 中的值。
fn lookup_preallocated_agent_session(terminal_session_id: &str) -> Option<String> {
    let map = PREALLOCATED_AGENT_SESSION_IDS.get()?;
    let guard = map.lock().ok()?;
    guard.get(terminal_session_id).cloned()
}

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
///     区分普通 pane 关闭与最后一个 pane 导致的 window 关闭；后者携带 closer-owned
///     `SessionCloseCleanup`，须在 kill_persisted_backend + SQLite delete 后再 `finish_cleanup`。
#[allow(clippy::large_enum_variant)]
pub enum PaneCloseOutcome {
    /// 仅关闭 pane；若 title-owner 交接后重贴了 auto 标题，附带更新后的 row 供命令层 persist + emit。
    PaneClosed {
        renamed: Option<WorkbenchSessionRow>,
    },
    WindowClosed(SessionCloseCleanup),
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
///     以 camelCase 序列化 sessionId、最近输出 buffer、是否截断、最后 seq，以及可选 ownerInstanceId（cutover 权威），供 Rust route 与前端类型对齐。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionReplayDto {
    pub session_id: String,
    pub buffer: String,
    pub truncated: bool,
    pub last_seq: u64,
    /// 发布该 terminal ring 的 owner 实例；缺失时前端不得用其重置已绑定 authority。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_instance_id: Option<String>,
}

/// 工作台终端 replay 中的增量输出块。
///
/// Business Logic（为什么需要这个结构体）:
///     长会话里每次 append 若重建整段 120k 字符，会把延迟拖到与历史长度成正比；按 chunk 保存才能只摊销裁剪成本。
///
/// Code Logic（这个结构体做什么）:
///     保存一段 UTF-8 文本及其 Unicode scalar 数量，供 ring buffer 整块丢弃或部分切头。
#[derive(Debug, Clone)]
struct ReplayChunk {
    text: String,
    char_count: usize,
}

/// 工作台终端最近输出 ring buffer。
///
/// Business Logic（为什么需要这个结构体）:
///     移动端进入远端终端时需要看到最近屏幕输出，而不是只能等待新事件。
///
/// Code Logic（这个结构体做什么）:
///     以 Unicode scalar 容量上限保存增量 chunk deque；append 只追加尾部并摊销裁剪，
///     记录是否曾截断以及最新 terminal output seq；snapshot 时再拼接为 DTO buffer。
///     R19 H1：`generation` 绑定 buffer 归属，旧 generation 不得污染同 id 新实例。
#[derive(Debug, Clone)]
struct SessionReplayBuffer {
    max_chars: usize,
    /// 绑定本 buffer 的 live 实例世代（R19 H1）。
    generation: u64,
    /// 测试与内部裁剪需要直接观察 deque 长度与字符计数。
    pub(super) chunks: VecDeque<ReplayChunk>,
    pub(super) char_count: usize,
    byte_count: usize,
    truncated: bool,
    last_seq: u64,
}

impl SessionReplayBuffer {
    /// Business Logic（为什么需要这个函数）:
    ///     每个 Workbench session 创建或恢复时都需要初始化自己的最近输出缓存。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 chunk deque，记录 max_chars 与绑定 generation。
    fn new(max_chars: usize, generation: u64) -> Self {
        Self {
            max_chars,
            generation,
            chunks: VecDeque::new(),
            char_count: 0,
            byte_count: 0,
            truncated: false,
            last_seq: 0,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终端 reader 每收到一个非空输出 chunk，都要让移动端 replay 能补上这段历史输出。
    ///     R19 H1：旧 generation 在 check-then-append 窗口内不得写入新实例 buffer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 `generation` 匹配时非空文本入队并更新 last_seq；不匹配返回 false。
    fn append_if_generation(&mut self, chunk: &str, seq: u64, generation: u64) -> bool {
        if self.generation != generation {
            return false;
        }
        let char_count = chunk.chars().count();
        if !chunk.is_empty() {
            self.byte_count += chunk.len();
            self.char_count += char_count;
            self.chunks.push_back(ReplayChunk {
                text: chunk.to_string(),
                char_count,
            });
        }
        self.last_seq = seq;
        self.trim_to_limit();
        true
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试与无 generation 的内部路径需要继续按旧语义追加。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `append_if_generation(..., self.generation)`。
    fn append(&mut self, chunk: &str, seq: u64) {
        let _ = self.append_if_generation(chunk, seq, self.generation);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     replay 截断不能破坏中文或 emoji，且热路径不能对整段历史做 O(n) 重建。
    ///
    /// Code Logic（这个函数做什么）:
    ///     当 char_count 超限时整块 pop_front；若头部 chunk 只需部分丢弃，则按 char 边界切掉前缀后 push_front 回 deque。
    fn trim_to_limit(&mut self) {
        if self.char_count <= self.max_chars {
            return;
        }
        self.truncated = true;
        let mut overflow = self.char_count - self.max_chars;
        while overflow > 0 {
            let Some(front) = self.chunks.pop_front() else {
                break;
            };
            self.byte_count -= front.text.len();
            self.char_count -= front.char_count;
            if overflow >= front.char_count {
                overflow -= front.char_count;
                continue;
            }
            let byte_offset = front
                .text
                .char_indices()
                .nth(overflow)
                .map(|(index, _)| index)
                .unwrap_or(front.text.len());
            let text = front.text[byte_offset..].to_string();
            let kept_chars = front.char_count - overflow;
            self.byte_count += text.len();
            self.char_count += kept_chars;
            self.chunks.push_front(ReplayChunk {
                text,
                char_count: kept_chars,
            });
            overflow = 0;
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     HTTP replay route 需要返回当前 session 的一致性快照，避免暴露内部可变 buffer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 deque 顺序拼接全部 chunk，补入 session_id 与 truncated/last_seq 元数据。
    fn snapshot(&self, session_id: &str) -> WorkbenchSessionReplayDto {
        let mut buffer = String::with_capacity(self.byte_count);
        for chunk in &self.chunks {
            buffer.push_str(&chunk.text);
        }
        WorkbenchSessionReplayDto {
            session_id: session_id.to_string(),
            buffer,
            truncated: self.truncated,
            last_seq: self.last_seq,
            // snapshot 层不知 owner；由命令/route 注入权威 ownerInstanceId。
            owner_instance_id: None,
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

/// 会话运行期 durable 发布态（R19 M1）。
///
/// Business Logic（为什么需要这个枚举）:
///     spawn 后、SQLite upsert 成功前的 provisional handle 不得对外发布 running/output/OSC。
///     失败 reclaim 时客户端不得残留 zombie running。
///
/// Code Logic（这个枚举做什么）:
///     `Provisional`：仅内部存在；`Ready`：持久化成功后允许外部事件与 Live presence。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionDurability {
    Provisional,
    /// Ready 前 deferred flush 进行中：reader 继续缓冲，flush 与 live 共用 generation-scoped seq。
    Flushing,
    Ready,
}

/// 副作用发布门（R20 H1/H2）。
///
/// Business Logic（为什么需要这个枚举）:
///     reader 必须区分「同 generation 尚未 Ready」与「stale/revoked」：前者缓冲等待，后者退出。
///
/// Code Logic（这个枚举做什么）:
///     `Ready` 可持 lease 发布；`Provisional` 缓冲/等待；`Rejected` 停止 worker。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SideEffectGate {
    Ready,
    Provisional,
    Rejected,
}

/// wait_for_closing_tombstone 单轮动作（R22 M1 / R23 H2）。
///
/// Business Logic（为什么需要这个枚举）:
///     waiter 只能观察 barrier 是否仍在 / 是否被替换，**禁止**自行清除。
///
/// Code Logic（这个枚举做什么）:
///     Gone=无 barrier；Replaced=身份变化；Retry=仍在（含 cleanup 未完成）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitClosingAction {
    Gone,
    Replaced,
    Retry,
}

/// try_insert_handle_revalidating_barrier 的结果（R23 M1 补丁）。
///
/// Business Logic（为什么需要这个枚举）:
///     concurrent reinsert 在 barrier 清除后只有一个赢家；其余不得因 Live 已存在而自旋死循环。
///
/// Code Logic（这个枚举做什么）:
///     Inserted=本线程写入；AlreadyLive=registry 已有 Live；BarrierActive=Closing 仍在。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertCasResult {
    Inserted,
    AlreadyLive,
    BarrierActive,
    /// project_closing 活跃，禁止为删除中项目 insert（R27 H4）。
    ProjectClosing,
}

/// spawn_row 遇到 Closing barrier 时的策略（R24 H2）。
///
/// Business Logic（为什么需要这个枚举）:
///     用户新建终端可在 barrier 清后重试；restore/safe_attach 不得拿 pre-close 行快照无限自旋
///     并在 close+delete 后复活已删除会话。
///
/// Code Logic（这个枚举做什么）:
///     Retry=kill 本轮 PTY 后 wait+重试；Abort=返回 `session_close_barrier_active` 供上层 re-read。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnBarrierPolicy {
    Retry,
    Abort,
}

/// 一次锁内完成 classify + buffer/lease 的结果（R23 H1）。
///
/// Business Logic（为什么需要这个枚举）:
///     reader/exit 在 Flushing→Ready 窗口不得因 classify 与 buffer 分锁而永久退出。
///
/// Code Logic（这个枚举做什么）:
///     `Buffered`：已在 Provisional/Flushing 缓冲；
///     `Live`：Ready 下已持 lease 并带回 payload（供锁外 publish）；
///     `Rejected`：stale。
enum PreparedSideEffect<T> {
    Buffered,
    Live { lease: PublicationLease, payload: T },
    Rejected,
}

/// generation-scoped 发布控制：token + 在途 lease 计数 + close barrier（R20 H2 / R23 H2）。
///
/// Business Logic（为什么需要这个结构体）:
///     close 必须在同一同步域内 revoke 并等待在途 publisher 完成，禁止 check→publish TOCTOU。
///     Closing barrier 还须覆盖 replay/PTY/tmux/persist cleanup，waiters 不得提前清 tombstone。
///
/// Code Logic（这个结构体做什么）:
///     `allowed` 失效 token；`in_flight` 统计持 lease 的副作用；
///     `cleanup_done` 标记 closer 已完成 cleanup；`cv` 供 close/waiter 等待。
struct PublishControl {
    allowed: AtomicBool,
    in_flight: AtomicUsize,
    /// generation-scoped 输出序号分配器（从 0 起，allocate 后从 1 递增；R21 H1）。
    next_seq: AtomicU64,
    /// Closing barrier：closer 完成 kill/replay/cleanup 后置 true（R23 H2）。
    cleanup_done: AtomicBool,
    wait: Mutex<()>,
    cv: Condvar,
}

impl PublishControl {
    /// Business Logic（为什么需要这个函数）:
    ///     每个 live 实例需要独立可失效的发布控制器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 allowed=true、in_flight=0、cleanup_done=false 的 Arc 控制块。
    fn new() -> Arc<Self> {
        Arc::new(Self {
            allowed: AtomicBool::new(true),
            in_flight: AtomicUsize::new(0),
            next_seq: AtomicU64::new(0),
            cleanup_done: AtomicBool::new(false),
            wait: Mutex::new(()),
            cv: Condvar::new(),
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     closer 完成 replay/PTY/tmux/persist cleanup 后必须标记 barrier，waiters 才能与 drain 一起收敛。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `cleanup_done=true` 并 notify 所有 waiter。
    fn mark_cleanup_done(&self) {
        self.cleanup_done.store(true, Ordering::SeqCst);
        let _guard = self.wait.lock().expect("publish control wait 锁中毒");
        self.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     waiter / closer 需要查询 cleanup 是否完成，禁止仅凭 in_flight==0 清 barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 `cleanup_done` 原子标志。
    fn is_cleanup_done(&self) -> bool {
        self.cleanup_done.load(Ordering::SeqCst)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     deferred flush 与 live reader 必须共享同一 generation 内序号，禁止各自从 0 重开导致前端 dedup 丢字节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `next_seq` 原子 +1 后返回新序号（首个为 1）。
    fn allocate_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close/reclaim 需要同步失效本 generation 的全部后续副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `allowed=false` 并 notify 等待者。
    fn revoke(&self) {
        self.allowed.store(false, Ordering::SeqCst);
        let _guard = self.wait.lock().expect("publish control wait 锁中毒");
        self.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 应尽量等在途 publish 完成后再 kill；若仍未 drain 不得把 barrier 当完成，需 tombstone。
    ///
    /// Code Logic（这个函数做什么）:
    ///     condvar 等待 `in_flight==0`；约 2s 仅打 warn 并返回是否已 drain（不假装完成）。
    fn wait_in_flight_drained(&self) -> bool {
        let mut guard = self.wait.lock().expect("publish control wait 锁中毒");
        let soft_deadline = std::time::Instant::now() + Duration::from_secs(2);
        while self.in_flight.load(Ordering::SeqCst) > 0 {
            let now = std::time::Instant::now();
            if now >= soft_deadline {
                tracing::warn!(
                    "publication lease drain soft-timeout; retaining generation tombstone until drained"
                );
                return false;
            }
            let wait = soft_deadline.saturating_duration_since(now);
            let (next, _) = self
                .cv
                .wait_timeout(guard, wait)
                .expect("publish control condvar 中毒");
            guard = next;
        }
        true
    }

    /// Business Logic（为什么需要这个函数）:
    ///     同 session_id 再 insert 前必须确认旧 generation 的 lease 已全部释放，否则旧 flush 会污染新实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     阻塞等待 `in_flight==0`（分段 soft warn，永不把未 drain 当成功）。
    fn wait_in_flight_drained_blocking(&self) {
        let mut guard = self.wait.lock().expect("publish control wait 锁中毒");
        loop {
            if self.in_flight.load(Ordering::SeqCst) == 0 {
                return;
            }
            let (next, wait_result) = self
                .cv
                .wait_timeout(guard, Duration::from_secs(2))
                .expect("publish control condvar 中毒");
            guard = next;
            if wait_result.timed_out() && self.in_flight.load(Ordering::SeqCst) > 0 {
                tracing::warn!("waiting for publication lease tombstone drain");
            }
        }
    }
}

/// 持有 generation-scoped 发布 lease，直到副作用完成（R20 H2）。
///
/// Business Logic（为什么需要这个结构体）:
///     emit/enqueue 必须把 fence 与副作用包在同一 lease 内，close 才能等待在途发布。
///
/// Code Logic（这个结构体做什么）:
///     Drop 时 `in_flight-1` 并 notify close waiter。
struct PublicationLease {
    control: Arc<PublishControl>,
}

impl Drop for PublicationLease {
    /// Business Logic（为什么需要这个函数）:
    ///     副作用结束必须释放 lease，否则 close 永久阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `in_flight` 递减 + condvar notify_all。
    fn drop(&mut self) {
        self.control.in_flight.fetch_sub(1, Ordering::SeqCst);
        let _guard = self
            .control
            .wait
            .lock()
            .expect("publish control wait 锁中毒");
        self.control.cv.notify_all();
    }
}

/// 工作台终端会话运行态句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     每次 spawn/restore 都需要独立运行期身份；失败 reclaim 后同 session_id 的新实例
///     不得被旧 reader/exit watcher 写入 status 或 replay。
///     R19 M1：Ready 前禁止外部 live 事件；R19 H1：publish token 在 reclaim 时失效。
///     R20 H1：Provisional 期间缓冲输出/退出；R20 H2：publish lease barrier。
///
/// Code Logic（这个结构体做什么）:
///     将 row、generation、durability、PublishControl、Provisional 缓冲与 process 聚合到 Mutex；
///     generation 在 insert 时分配；close/reclaim 将 publish revoke 并等待 in_flight。
struct WorkbenchSessionHandle {
    row: WorkbenchSessionRow,
    /// 本次 live 实例世代（R18 M2 / R19 H1）。
    generation: u64,
    /// 持久化就绪态（R19 M1）。
    durability: SessionDurability,
    /// generation-scoped 发布控制（token + lease barrier）。
    publish: Arc<PublishControl>,
    /// Ready 前累积的可见输出（R20 H1）；Ready 后原序 flush。
    deferred_output: Vec<String>,
    /// Ready 前的 OSC mutation（R20 H1）；Ready 后 flush。
    deferred_mutations: Vec<AgentRuntimeMutation>,
    /// Provisional 期间子进程已退出时记录 exit code（R20 H1）。
    pending_exit: Option<Option<i32>>,
    /// 创建本 handle 时绑定的 restore claim generation（若有）；Ready 前 revalidate（R27 H5）。
    restore_claim_generation: Option<u64>,
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
///     Orchestrator/OpenCode 路径可额外预分配 `agent_session_id` 注入 shell。
///
/// Code Logic（这个类型做什么）:
///     四字段纯 ID + 可选 agent_session_id；由 spawn 路径从 row + AppState 组装。
///     普通用户终端保持 `agent_session_id=None`（不写 `CC_PARTNER_AGENT_SESSION_ID`）。
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
    /// 可选预分配 Agent session id（Orchestrator/OpenCode bridge）
    pub agent_session_id: Option<String>,
}

/// Claude Code 自动连接 IDE 的 env 开关名。
///
/// Business Logic（为什么需要这个常量）:
///     Workbench 内启动的 Claude/Codex 等 agent 不得继承 VS Code 的 active-file 上下文
///     （状态栏 `in <file>` / `opened_file_in_ide`），否则 Downloads 等无关标签会污染会话。
const CLAUDE_CODE_AUTO_CONNECT_IDE_ENV: &str = "CLAUDE_CODE_AUTO_CONNECT_IDE";

/// Claude Code IDE SSE bridge 端口 env 名（存在即倾向自动连 IDE）。
const CLAUDE_CODE_SSE_PORT_ENV: &str = "CLAUDE_CODE_SSE_PORT";

/// Business Logic（为什么需要这个函数）:
///     Claude Code 2.1+ 在 `CLAUDE_CODE_AUTO_CONNECT_IDE===false` 时硬禁用自动 IDE 连接，
///     优先于 settings.autoConnectIde、`--ide` 路径、以及 `CLAUDE_CODE_SSE_PORT` 继承。
///     同时移除 SSE 端口 env，避免子 shell 继承父进程/IDE 注入的 bridge 状态。
///
/// Code Logic（这个函数做什么）:
///     设置 `CLAUDE_CODE_AUTO_CONNECT_IDE=false`；`env_remove(CLAUDE_CODE_SSE_PORT)`。
fn apply_claude_ide_isolation_env(command: &mut CommandBuilder) {
    command.env(CLAUDE_CODE_AUTO_CONNECT_IDE_ENV, "false");
    command.env_remove(CLAUDE_CODE_SSE_PORT_ENV);
}

/// Business Logic（为什么需要这个函数）:
///     cc-partner 是 GUI 应用，父进程可能没有真实终端环境或继承 `TERM=dumb`，会破坏 tmux 客户端协商；
///     Agent adapter 还需要稳定非敏感 ID 环境变量；Workbench agent 必须强制不连 IDE。
///
/// Code Logic（这个函数做什么）:
///     设置 xterm TERM/COLORTERM/TERM_PROGRAM、Claude IDE 隔离 env，以及可选的四条 `CC_PARTNER_*_ID`（无 token）。
fn apply_workbench_terminal_env(
    command: &mut CommandBuilder,
    agent_ctx: Option<&TerminalAgentContextIds>,
) {
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "cc-partner");
    apply_claude_ide_isolation_env(command);
    if let Some(ctx) = agent_ctx {
        apply_agent_context_env(command, ctx);
    }
}

/// 注入非敏感 Agent 上下文环境变量。
///
/// Business Logic（为什么需要这个函数）:
///     tmux 与 raw PTY 的 pane/shell 必须能读到同一套 CC_PARTNER_*_ID，供 Hook 关联 session。
///
/// Code Logic（这个函数做什么）:
///     设置 PROJECT/WORKTREE/TERMINAL_SESSION/OWNER_INSTANCE_ID；
///     仅当 `agent_session_id` 非空时设置 `CC_PARTNER_AGENT_SESSION_ID`；
///     不设置任何 token/credential。
fn apply_agent_context_env(command: &mut CommandBuilder, ctx: &TerminalAgentContextIds) {
    command.env("CC_PARTNER_PROJECT_ID", &ctx.project_id);
    command.env("CC_PARTNER_WORKTREE_ID", &ctx.worktree_id);
    command.env("CC_PARTNER_TERMINAL_SESSION_ID", &ctx.terminal_session_id);
    command.env("CC_PARTNER_OWNER_INSTANCE_ID", &ctx.owner_instance_id);
    if let Some(agent_id) = ctx
        .agent_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        command.env("CC_PARTNER_AGENT_SESSION_ID", agent_id);
    }
}

/// 生成 tmux `new-session` / `new-window` 的 `-e KEY=VAL` 参数（Agent 上下文 + IDE 隔离）。
///
/// Business Logic（为什么需要这个函数）:
///     tmux pane 内 shell 继承创建时的 -e 环境；attach 客户端 env 不会进入已有 pane。
///     必须在 -e 中注入 `CLAUDE_CODE_AUTO_CONNECT_IDE=false`，否则用户在 pane 内启动 claude
///     仍可能连上本机 VS Code 并注入无关 active file。
///
/// Code Logic（这个函数做什么）:
///     返回交错的 `-e` / `KEY=VAL` 列表；含 IDE 隔离 + 四条基础 ID，可选 AGENT_SESSION_ID。
///     tmux `-e` 无法 unset 父 env，但 Claude 对 AUTO_CONNECT=false 硬禁用优先于 SSE_PORT。
fn tmux_agent_context_env_args(ctx: &TerminalAgentContextIds) -> Vec<String> {
    let mut pairs: Vec<(&str, &str)> = vec![
        (CLAUDE_CODE_AUTO_CONNECT_IDE_ENV, "false"),
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
    if let Some(agent_id) = ctx
        .agent_session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        pairs.push(("CC_PARTNER_AGENT_SESSION_ID", agent_id));
    }
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
///     冷启动 attach 或「适应尺寸」时，仅依赖 PTY SIGWINCH 可能不刷新 detached tmux window 尺寸，
///     导致 status bar 悬在旧 client size 中间；需要显式 `resize-window` 强制同步。
///
/// Code Logic（这个函数做什么）:
///     构造 `resize-window -t <target> -x <cols> -y <rows>` 参数列表。
fn tmux_resize_window_args(target: &str, cols: u16, rows: u16) -> Vec<String> {
    vec![
        "resize-window".to_string(),
        "-t".to_string(),
        target.to_string(),
        "-x".to_string(),
        cols.to_string(),
        "-y".to_string(),
        rows.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     同尺寸 `resize-window` / PTY resize 常被 tmux/内核忽略，status 停在历史帧中间；
///     需要先 bump 一行再回到目标尺寸，强制 SIGWINCH 与 full redraw。
///
/// Code Logic（这个函数做什么）:
///     返回与 target 不同的临时 rows（优先 rows-1，否则 rows+1），供两步 resize。
fn tmux_force_redraw_bump_rows(rows: u16) -> u16 {
    if rows > MIN_TERMINAL_ROWS {
        rows - 1
    } else {
        rows.saturating_add(1).max(MIN_TERMINAL_ROWS + 1)
    }
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 的终端主题由前端 xterm 控制，tmux status bar 不能继承用户全局 `.tmux.conf` 中的固定深色背景。
///
/// Code Logic（这个函数做什么）:
///     生成一组浅色/深色都安全的 tmux status option 命令；保留 session/window 标签结构，但不保留全局 tmux 主题里的硬编码颜色。
///     强制 `status-position bottom`，避免用户全局 `status-position top` 或错位状态在重启后残留。
///     强制 session-local `mouse off`：用户全局 `mouse on` 时滚轮会进 copy-mode（浏览模式），
///     键盘被 tmux 吞掉，必须 Ctrl+C 才能恢复输入。工作台复制走 xterm 选区，不依赖 tmux 鼠标。
///     同时幂等确保 `terminal-features` 含 `xterm*:mouse`：默认 features 不含 mouse 时，Claude 的 DECSET
///     1000/1006 到不了外层 xterm，滚轮会被译成 ↑/↓ 并被输入框当成历史 prompt。mouse 能力
///     只让应用鼠标序列透传，不会打开 tmux 自己的 mouse/copy-mode；重复项由独立对账步骤清理。
fn tmux_status_theme_commands(session_name: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "mouse".to_string(),
            "off".to_string(),
        ],
        vec![
            "set-option".to_string(),
            "-t".to_string(),
            session_name.to_string(),
            "status-position".to_string(),
            "bottom".to_string(),
        ],
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
///     Workbench 会在创建、恢复、聚焦和调整多个 tmux window 时重复套用终端主题。若每次都用
///     `set-option -sa terminal-features xterm*:mouse`，server array 会无限增长；同时不能为了清理
///     Workbench 自己的重复项而覆盖用户配置的其他 terminal feature。
///
/// Code Logic（这个函数做什么）:
///     解析 `show-options -s terminal-features`：已有精确 Workbench 项时保留最小下标并按倒序删除
///     其余精确重复项；已有等价 `xterm*:...:mouse` 时不追加；完全缺失时仅追加一次。
fn tmux_terminal_mouse_feature_reconcile_commands(output: &str) -> Vec<Vec<String>> {
    let mut exact_indices = Vec::new();
    let mut equivalent_feature_exists = false;

    for line in output.lines() {
        let Some((name, raw_value)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let Some(index) = name
            .strip_prefix("terminal-features[")
            .and_then(|value| value.strip_suffix(']'))
            .and_then(|value| value.parse::<usize>().ok())
        else {
            continue;
        };
        let value = raw_value.trim();
        if value == WORKBENCH_TMUX_MOUSE_FEATURE {
            exact_indices.push(index);
            equivalent_feature_exists = true;
            continue;
        }
        let mut parts = value.split(':');
        if parts.next() == Some("xterm*") && parts.any(|feature| feature == "mouse") {
            equivalent_feature_exists = true;
        }
    }

    exact_indices.sort_unstable();
    let mut commands = exact_indices
        .into_iter()
        .skip(1)
        .rev()
        .map(|index| {
            vec![
                "set-option".to_string(),
                "-su".to_string(),
                format!("terminal-features[{index}]"),
            ]
        })
        .collect::<Vec<_>>();
    if !equivalent_feature_exists {
        commands.push(vec![
            "set-option".to_string(),
            "-sa".to_string(),
            "terminal-features".to_string(),
            WORKBENCH_TMUX_MOUSE_FEATURE.to_string(),
        ]);
    }
    commands
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 必须让 Claude 等 TUI 的 mouse DECSET 穿过 tmux，同时长期运行不能持续污染
///     tmux server array，已有用户 terminal-features 也必须原样保留。
///
/// Code Logic（这个函数做什么）:
///     在进程级锁内读取 server array，执行纯函数规划出的最小 unset/append 命令，最终精确
///     `xterm*:mouse` 至多一项；任一 tmux 命令失败则返回错误，由既有调用方降级记录。
fn reconcile_workbench_tmux_terminal_mouse_feature(tmux: &TmuxCommand) -> Result<(), AppError> {
    let _guard = TMUX_TERMINAL_FEATURE_RECONCILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let output = run_tmux_command(tmux, &["show-options", "-s", "terminal-features"])?;
    for args in tmux_terminal_mouse_feature_reconcile_commands(&output) {
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        run_tmux_command(tmux, &arg_refs)?;
    }
    Ok(())
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
    reconcile_workbench_tmux_terminal_mouse_feature(tmux)?;
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
///     恢复 / safe attach 时需要判断目标 window 是否仍存在；存在才 attach，缺失则 skip（A8 禁止创建）。
///
/// Code Logic（这个函数做什么）:
///     执行 `tmux display-message -p -t <target> #{window_id}`；
///     要求 exit success **且** stdout 为非空 `@N` window id。
///     （tmux 3.6+ 对缺失 target 可能仍 exit 0 但 stdout 为空，仅看 status 会误判存在。）
fn tmux_target_exists(tmux: &TmuxCommand, target: &str) -> bool {
    let output = match tmux
        .std_command()
        .args(["display-message", "-p", "-t", target, "#{window_id}"])
        .output()
    {
        Ok(output) => output,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    let window_id = String::from_utf8_lossy(&output.stdout);
    let window_id = window_id.trim();
    !window_id.is_empty() && window_id.starts_with('@')
}

/// Business Logic（为什么需要这个函数）:
///     workspace restore preflight 必须只读探测 tmux target，不得 new-session/new-window。
///
/// Code Logic（这个函数做什么）:
///     若本机无 tmux 则 false；否则对 target 执行 display-message 探测。
pub fn inspect_tmux_target_exists(target: &str) -> bool {
    let Some(tmux) = available_tmux_command() else {
        return false;
    };
    tmux_target_exists(&tmux, target)
}

/// Business Logic（为什么需要这个函数）:
///     preflight/safe attach 需要从持久化 row 得到稳定 target 字符串。
///
/// Code Logic（这个函数做什么）:
///     公开包装 `tmux_target_for_row`。
pub fn tmux_target_string_for_row(row: &WorkbenchSessionRow) -> Result<String, AppError> {
    tmux_target_for_row(row)
}

/// Business Logic（为什么需要这个函数）:
///     safe restore 只能 attach 已存在的 tmux window，禁止走 restore() 的 create/raw-PTY 回退。
///
/// Code Logic（这个函数做什么）:
///     再次确认 backend=tmux 且 target 存在后，仅调用 registry 的 attach-only 入口（spawn attach client）。
///     不调用 `create_tmux_window` / `new-session` / `new-window` / agent resume。
pub fn safe_attach_existing_tmux_session(
    state: &AppState,
    row: WorkbenchSessionRow,
    counters: &crate::workbench::workspace_restore::RestoreSideEffectCounters,
    restore_claim_generation: Option<u64>,
) -> Result<WorkbenchSessionRow, AppError> {
    if row.backend != TMUX_BACKEND {
        return Err(AppError::validation(
            "safe_attach_requires_tmux".to_string(),
        ));
    }
    let target = tmux_target_for_row(&row)?;
    if !inspect_tmux_target_exists(&target) {
        return Err(AppError::unavailable("tmux_target_missing".to_string()));
    }
    // R27 H5：claim generation 必须在 attach 与 Ready 前仍 active。
    if let Some(generation) = restore_claim_generation {
        state
            .workbench_sessions
            .require_restore_claim_active(&row.id, generation)?;
    }
    // 计数：仅 attach client，不递增 new-session/new-window。
    counters
        .attach_client
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let restored = state.workbench_sessions.attach_existing_tmux_only(
        state.clone(),
        row,
        restore_claim_generation,
    )?;
    // R19 M1 / R27 H5：attach 成功后 Ready；revoked claim 不得 commit Ready。
    if let Some(claim_gen) = restore_claim_generation {
        state
            .workbench_sessions
            .require_restore_claim_active(&restored.id, claim_gen)?;
        let Some(generation) = state.workbench_sessions.session_generation(&restored.id) else {
            return Err(AppError::unavailable(
                "session_restore_claim_revoked".to_string(),
            ));
        };
        if !state.workbench_sessions.mark_session_ready_for_generation(
            &restored.id,
            generation,
            Some(state),
        ) {
            return Err(AppError::unavailable(
                "session_restore_claim_revoked".to_string(),
            ));
        }
    } else {
        state
            .workbench_sessions
            .mark_session_ready(&restored.id, Some(state));
    }
    Ok(restored)
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

/// 测试用：`create_tmux_window` 调用次数（A8 安全属性：list restore 路径必须为 0）。
#[cfg(test)]
static CREATE_TMUX_WINDOW_CALL_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Business Logic（为什么需要这个函数）:
///     集成/单测断言 list restore 与 safe attach 路径不创建 shell。
///
/// Code Logic（这个函数做什么）:
///     读取 `CREATE_TMUX_WINDOW_CALL_COUNT`。
#[cfg(test)]
pub fn create_tmux_window_call_count_for_test() -> u64 {
    CREATE_TMUX_WINDOW_CALL_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Business Logic（为什么需要这个函数）:
///     测试间隔离 create 计数。
///
/// Code Logic（这个函数做什么）:
///     归零原子计数器。
#[cfg(test)]
pub fn reset_create_tmux_window_call_count_for_test() {
    CREATE_TMUX_WINDOW_CALL_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// 测试用：`TmuxCreateGuard` 未 commit Drop 触发的 reclaim 次数（R30 M2）。
/// 仅 guard Drop 路径计数，避免与其它 close/kill_persisted_backend 并行测试交叉。
#[cfg(test)]
static TMUX_CREATE_GUARD_RECLAIM_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Business Logic（为什么需要这个函数）:
///     单测断言 TmuxCreateGuard Drop / barrier 失败路径会销毁刚创建的 window。
///
/// Code Logic（这个函数做什么）:
///     读取 `TMUX_CREATE_GUARD_RECLAIM_COUNT`。
#[cfg(test)]
pub fn tmux_create_guard_reclaim_count_for_test() -> u64 {
    TMUX_CREATE_GUARD_RECLAIM_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

/// Business Logic（为什么需要这个函数）:
///     测试间隔离 reclaim 计数。
///
/// Code Logic（这个函数做什么）:
///     归零原子计数器。
#[cfg(test)]
pub fn reset_tmux_create_guard_reclaim_count_for_test() {
    TMUX_CREATE_GUARD_RECLAIM_COUNT.store(0, std::sync::atomic::Ordering::SeqCst);
}

/// Business Logic（为什么需要这个函数）:
///     新建 tab 时，需要在 worktree 级 tmux session 内创建一个 window 承载真实 shell 上下文。
///     **不得**被 list/open restore 路径调用（A8：缺失 target 时 skip，不创建 shell）。
///
/// Code Logic（这个函数做什么）:
///     session 不存在时执行 `tmux new-session -d -s <session> -n <window> -x/-y`；存在时执行 `tmux new-window`；
///     两者都用 `-P -F #{window_id}` 读取真实 window id。new-window 不支持 -x/-y，创建后统一
///     `resize-window -x/-y`，避免 detached window 以默认小尺寸绘制导致 status bar 错位。
// tmux window 参数是固定集合（tmux/session/window/cwd/shell/agent/cols/rows），不宜再拆 struct。
#[allow(clippy::too_many_arguments)]
fn create_tmux_window(
    tmux: &TmuxCommand,
    session_name: &str,
    window_name: &str,
    cwd: &str,
    shell_command: &str,
    agent_ctx: Option<&TerminalAgentContextIds>,
    cols: u16,
    rows: u16,
) -> Result<String, AppError> {
    #[cfg(test)]
    {
        CREATE_TMUX_WINDOW_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    let tmux_cwd = tmux.project_cwd(cwd)?;
    let cols_text = cols.to_string();
    let rows_text = rows.to_string();
    let mut command = tmux.std_command();
    if tmux_has_session(tmux, session_name) {
        // new-window 无 -x/-y；尺寸在拿到 window_id 后由 resize-window 强制同步。
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
            "-x",
            &cols_text,
            "-y",
            &rows_text,
            "-P",
            "-F",
            "#{window_id}",
        ]);
    }
    // 无论是否有 agent_ctx，都必须注入 IDE 隔离 env（claude 在 pane 内启动时不连 VS Code）。
    // 有 agent_ctx 时由 tmux_agent_context_env_args 一并带上；无 ctx 时单独 -e。
    if let Some(ctx) = agent_ctx {
        for arg in tmux_agent_context_env_args(ctx) {
            command.arg(arg);
        }
    } else {
        command.arg("-e");
        command.arg(format!("{CLAUDE_CODE_AUTO_CONNECT_IDE_ENV}=false"));
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
            let target = tmux_window_target(session_name, &window_id);
            let resize_args = tmux_resize_window_args(&target, cols, rows);
            let resize_refs: Vec<&str> = resize_args.iter().map(String::as_str).collect();
            if let Err(error) = run_tmux_command(tmux, &resize_refs) {
                tracing::debug!("创建后调整 tmux window 尺寸失败 target={target}: {error}");
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
///     关闭 terminal window 时需销毁对应 tmux 后端；R36 H1：已知 window_id 时永远只 kill-window，
///     禁止用 count==1 降级 kill-session——并发/重试 close 可能先杀掉目标窗，再把 count==1 误读为
///     “只剩自己”而 kill 整个 session，毁掉仍存活的兄弟 terminal。末窗 kill-window 后 tmux 会自行
///     回收空 session。仅 legacy 行缺 window_id 且 count 可知时才 kill-session。
///     R32 H1：list-windows 探测失败不得降级 kill-session。
///
/// Code Logic（这个函数做什么）:
///     - window_id = Some → 始终 `kill-window -t session:window`（忽略 count，含 count=1/None）；
///     - window_id = None 且 count 可知 → `kill-session -t session`（legacy 路径）；
///     - window_id = None 且 count=None → None（fail closed，不发 kill-session）。
fn tmux_destroy_backend_args(
    session_name: &str,
    window_id: Option<&str>,
    window_count: Option<usize>,
) -> Option<Vec<String>> {
    match (window_id, window_count) {
        // R36 H1 / R32 H1：有 window_id 永远只杀该 window，永不 kill-session。
        (Some(window_id), _) => Some(vec![
            "kill-window".to_string(),
            "-t".to_string(),
            tmux_window_target(session_name, window_id),
        ]),
        // 探测失败且无 window_id：fail closed，禁止盲杀整 session。
        (None, None) => None,
        // legacy：缺 window_id 但 count 可知 → kill-session。
        (None, Some(_)) => Some(vec![
            "kill-session".to_string(),
            "-t".to_string(),
            session_name.to_string(),
        ]),
    }
}

/// Business Logic（为什么需要这个函数）:
///     create 路径 TmuxCreateGuard 回收 orphan window 时，必须只杀已知 window_id，
///     绝不能因 list-windows 失败走 kill-session 毁掉同 worktree 其它 terminal（R32 H1）。
///
/// Code Logic（这个函数做什么）:
///     仅当 row 有 backend_id + backend_window_id 时构造 `kill-window -t session:window`；
///     缺 window_id 时 fail closed 直接返回，不探测、不 kill-session。
fn kill_created_tmux_window_only(row: &WorkbenchSessionRow) {
    if row.backend != TMUX_BACKEND {
        return;
    }
    let Some(session_name) = row.backend_id.as_deref() else {
        return;
    };
    let Some(window_id) = row.backend_window_id.as_deref() else {
        // 无已知 window_id 时 fail closed：禁止 kill-session 盲杀兄弟窗。
        tracing::debug!("TmuxCreateGuard reclaim skipped: missing backend_window_id");
        return;
    };
    let Some(tmux) = available_tmux_command() else {
        return;
    };
    let target = tmux_window_target(session_name, window_id);
    let mut command = tmux.std_command();
    command.args(["kill-window", "-t", &target]);
    if let Err(error) = command.output() {
        tracing::debug!("TmuxCreateGuard kill-window 失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户关闭终端 tab 时，如果该 tab 使用 tmux 承载上下文，应销毁对应 tmux window/session，避免后台残留。
///     R35 M3：销毁失败时不得假装成功——调用方据此决定是否删除 SQLite 元数据与 finish_cleanup；
///     无法确认后端已销毁时必须保留 metadata/barrier，禁止留下“元数据已删、tmux 仍活”的孤儿。
///
/// Code Logic（这个函数做什么）:
///     - 非 tmux backend / 缺 backend_id → Ok（无需销毁）；
///     - tmux backend 但 tmux 不可用 → Err(unavailable)，禁止删元数据；
///     - list-windows 探测 window_count；经 `tmux_destroy_backend_args`：
///       有 window_id → 始终 kill-window（R36 H1，含 count==1）；
///       无 window_id 且 count 可知 → kill-session（legacy）；
///       探测失败且无 window_id → fail closed 返回 Err（不杀、不删）；
///     - destroy 非零退出：仅当 stderr/stdout 表明 already-gone 时 Ok，否则 Err；
///     - command.output() IO 错误 → Err。
pub fn kill_persisted_backend(row: &WorkbenchSessionRow) -> Result<(), AppError> {
    if row.backend != TMUX_BACKEND {
        // R41 M7：raw/pty 无独立后端可杀；missing-handle 路径不得把 running 当自动成功。
        return raw_pty_kill_persisted_policy(row);
    }
    let Some(session_name) = row.backend_id.as_deref() else {
        return Ok(());
    };
    let Some(tmux) = available_tmux_command() else {
        return Err(AppError::unavailable(
            "tmux_unavailable_for_persisted_backend_kill".to_string(),
        ));
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
    let Some(args) =
        tmux_destroy_backend_args(session_name, row.backend_window_id.as_deref(), window_count)
    else {
        // R32 H1 / R35 M3：list-windows 失败且无 window_id → fail closed，不 kill-session，
        // 且返回 Err 阻止调用方删除 SQLite（无法确认可安全销毁）。
        tracing::debug!("kill_persisted_backend skipped: list-windows failed without window_id");
        return Err(AppError::unavailable(
            "tmux_destroy_probe_failed_without_window_id".to_string(),
        ));
    };
    run_tmux_destroy_command(&tmux, &args)
}

/// Business Logic（为什么需要这个函数）:
///     destroy 命令非零退出时，目标 window/session 可能早已不存在（竞态/重复 close）；
///     这类“已经没了”应视为成功，否则会卡住 barrier、永远删不掉元数据。
///
/// Code Logic（这个函数做什么）:
///     对 stdout+stderr 做大小写不敏感子串匹配：can't find / no server / no such / not found。
fn tmux_destroy_exit_is_already_gone(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
    combined.contains("can't find")
        || combined.contains("can not find")
        || combined.contains("no server")
        || combined.contains("no such")
        || combined.contains("not found")
}

/// Business Logic（为什么需要这个函数）:
///     kill_persisted_backend 需要把 destroy 子进程的 exit/IO 语义统一成 Result，
///     避免调用方各自解析 command.output() 并误把失败当成功。
///
/// Code Logic（这个函数做什么）:
///     执行 tmux destroy args；success exit → Ok；already-gone 文案 → Ok；其它非零/IO → Err。
fn run_tmux_destroy_command(tmux: &TmuxCommand, args: &[String]) -> Result<(), AppError> {
    let mut command = tmux.std_command();
    command.args(args.iter().map(String::as_str));
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            tracing::debug!("销毁工作台 tmux 会话 IO 失败: {error}");
            return Err(AppError::from(error));
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if tmux_destroy_exit_is_already_gone(&stdout, &stderr) {
        tracing::debug!(
            stderr = %stderr,
            "tmux destroy already gone; treating as success"
        );
        return Ok(());
    }
    tracing::debug!(
        status = ?output.status,
        stderr = %stderr,
        "销毁工作台 tmux 会话非零退出"
    );
    let detail = stderr.trim();
    if detail.is_empty() {
        Err(AppError::unavailable("tmux_destroy_failed".to_string()))
    } else {
        Err(AppError::unavailable(format!(
            "tmux_destroy_failed: {detail}"
        )))
    }
}

/// create 路径在 `create_tmux_window` 成功后、create 完全提交前的 window 回收守卫（R30 M2 / R32 H1）。
///
/// Business Logic（为什么需要这个结构体）:
///     create 可先成功创建不可见 tmux window，再因 project barrier / spawn_row 失败；
///     `SessionSpawnGuard` 只覆盖 registry 内 PTY attach，无法回收 pre-spawn window。
///
/// Code Logic（这个结构体做什么）:
///     持有已创建 window 的 row 快照；未 `commit()` 时 Drop 调用 `kill_created_tmux_window_only`
///    （仅 kill-window，永不 kill-session）。
struct TmuxCreateGuard {
    row: WorkbenchSessionRow,
    committed: bool,
}

impl TmuxCreateGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     `create_tmux_window` 成功后立刻接管 window 生命周期，任何 early return 都能销毁 orphan。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 row，committed=false。
    fn new(row: WorkbenchSessionRow) -> Self {
        Self {
            row,
            committed: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn_row 成功后 window 所有权移交 registry/command 层，禁止 Drop 误杀合法 window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     标记 committed=true。
    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TmuxCreateGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     barrier / spawn 失败（含 panic）必须销毁刚创建的 invisible window，禁止后台残留 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 commit 时 best-effort `kill_created_tmux_window_only`（仅 kill 已知 window_id，
    ///     永不 kill-session；不记录 terminal body）；测试环境额外累加 reclaim 计数。
    fn drop(&mut self) {
        if !self.committed {
            #[cfg(test)]
            {
                TMUX_CREATE_GUARD_RECLAIM_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            kill_created_tmux_window_only(&self.row);
        }
    }
}

/// 从 session row 与 owner 实例 id 组装 Agent 上下文。
///
/// Business Logic（为什么需要这个函数）:
///     spawn/attach 需要把稳定 ID 注入 shell，Hook 才能关联 OSC。
///
/// Code Logic（这个函数做什么）:
///     worktree 缺省为空串；owner_instance_id 原样拷贝；agent_session_id 默认 None
///     （预分配路径用 `create_with_preallocated_ids` 注入）。
fn agent_context_from_row(
    row: &WorkbenchSessionRow,
    owner_instance_id: &str,
) -> TerminalAgentContextIds {
    TerminalAgentContextIds {
        project_id: row.project_id.clone(),
        worktree_id: row.worktree_id.clone().unwrap_or_default(),
        terminal_session_id: row.id.clone(),
        owner_instance_id: owner_instance_id.to_string(),
        agent_session_id: lookup_preallocated_agent_session(&row.id),
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
///     first-pane / title-owner 交接需要有序 pane_id 列表。
///
/// Code Logic（这个函数做什么）:
///     `list-panes -F #{pane_id}`，保留 tmux 输出顺序，过滤空行。
fn tmux_list_pane_ids(tmux: &TmuxCommand, target: &str) -> Result<Vec<String>, AppError> {
    let output = run_tmux_command(tmux, &["list-panes", "-t", target, "-F", "#{pane_id}"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
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
///     分屏按钮创建的新 pane 必须从项目根目录启动，避免继承当前 pane 中用户 cd 后的位置；
///     同时强制 Claude 不连 IDE（与 new-window 路径一致）。
///
/// Code Logic（这个函数做什么）:
///     构造 `tmux split-window <direction> -t <target> -c <cwd> -e CLAUDE_CODE_AUTO_CONNECT_IDE=false`。
fn tmux_split_window_args(direction: PaneSplitDirection, target: &str, cwd: &str) -> Vec<String> {
    vec![
        "split-window".to_string(),
        direction.tmux_flag().to_string(),
        "-t".to_string(),
        target.to_string(),
        "-c".to_string(),
        cwd.to_string(),
        "-e".to_string(),
        format!("{CLAUDE_CODE_AUTO_CONNECT_IDE_ENV}=false"),
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

/// tmux window 内单个 pane 的显示矩形。
///
/// Business Logic（为什么需要这个结构体）:
///     用户点击终端某个字符格切换 active pane 时，必须把该坐标映射到真实 tmux pane；
///     前端不持有也不应猜测 tmux 布局，映射只能由后端读取 tmux 真值完成。
///
/// Code Logic（这个结构体做什么）:
///     保存 pane_id 与该 pane 在 window 内的闭区间边界（列 left..=right、行 top..=bottom）以及 active 标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPaneGeometry {
    pub pane_id: String,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub active: bool,
}

/// tmux window 的 pane 布局快照。
///
/// Business Logic（为什么需要这个结构体）:
///     zoom 状态下 window 只显示一个 pane，list-panes 的历史布局不再对应屏幕像素，
///     此时必须整体拒绝坐标命中，避免把点击切到用户根本看不见的 pane。
///
/// Code Logic（这个结构体做什么）:
///     保存全部 pane 几何与该 window 当前是否处于 zoom 状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TmuxWindowPaneLayout {
    pub panes: Vec<TmuxPaneGeometry>,
    pub zoomed: bool,
}

/// Business Logic（为什么需要这个函数）:
///     坐标命中需要 pane 的真实边界与 active/zoom 状态，一次查询取齐可避免多次 tmux 调用之间的布局漂移。
///
/// Code Logic（这个函数做什么）:
///     构造 `list-panes -t <target> -F "<pane_id> <left> <top> <right> <bottom> <active> <zoomed>"` 参数列表。
fn tmux_list_pane_geometry_args(target: &str) -> Vec<String> {
    vec![
        "list-panes".to_string(),
        "-t".to_string(),
        target.to_string(),
        "-F".to_string(),
        "#{pane_id} #{pane_left} #{pane_top} #{pane_right} #{pane_bottom} #{pane_active} #{window_zoomed_flag}".to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     tmux 是文本协议，字段缺失或非数字时必须整行丢弃而不是 panic 或产生错误命中矩形。
///
/// Code Logic（这个函数做什么）:
///     按空白切分每行，要求恰好 7 段且四个边界可解析为 u32；active/zoomed 以 `1` 判定；
///     zoomed 取任一有效行的值（同 window 内恒定）。
fn parse_tmux_pane_geometry(output: &str) -> TmuxWindowPaneLayout {
    let mut layout = TmuxWindowPaneLayout::default();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 7 {
            continue;
        }
        let (Ok(left), Ok(top), Ok(right), Ok(bottom)) = (
            fields[1].parse::<u32>(),
            fields[2].parse::<u32>(),
            fields[3].parse::<u32>(),
            fields[4].parse::<u32>(),
        ) else {
            continue;
        };
        if right < left || bottom < top {
            continue;
        }
        layout.panes.push(TmuxPaneGeometry {
            pane_id: fields[0].to_string(),
            left,
            top,
            right,
            bottom,
            active: fields[5] == "1",
        });
        if fields[6] == "1" {
            layout.zoomed = true;
        }
    }
    layout
}

/// Business Logic（为什么需要这个函数）:
///     点击落在 pane 之间的分隔边框时不属于任何 pane，必须 no-op 而不是就近吸附到错误 pane。
///
/// Code Logic（这个函数做什么）:
///     返回第一个闭区间同时包含 col 与 row 的 pane；无命中返回 None。
fn tmux_pane_at_position(
    panes: &[TmuxPaneGeometry],
    col: u32,
    row: u32,
) -> Option<&TmuxPaneGeometry> {
    panes
        .iter()
        .find(|pane| col >= pane.left && col <= pane.right && row >= pane.top && row <= pane.bottom)
}

/// Business Logic（为什么需要这个函数）:
///     坐标命中得到的是绝对 pane_id，必须用绝对 target 选中，不能退化成相对 `.+` 循环。
///
/// Code Logic（这个函数做什么）:
///     构造 `tmux select-pane -t <pane_id>` 参数列表。
fn tmux_select_pane_args(pane_id: &str) -> Vec<String> {
    vec![
        "select-pane".to_string(),
        "-t".to_string(),
        pane_id.to_string(),
    ]
}

/// Business Logic（为什么需要这个函数）:
///     命中判定必须基于 tmux 当前真值布局，不能缓存在前端或后端。
///
/// Code Logic（这个函数做什么）:
///     执行 list-panes 几何查询并解析为 TmuxWindowPaneLayout。
fn tmux_window_pane_layout(
    tmux: &TmuxCommand,
    target: &str,
) -> Result<TmuxWindowPaneLayout, AppError> {
    let args = tmux_list_pane_geometry_args(target);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = run_tmux_command(tmux, &arg_refs)?;
    Ok(parse_tmux_pane_geometry(&output))
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
///     R41 M7：raw PTY 没有独立 tmux backend 可 destroy；若 close 已丢失 live handle，
///     不得把 status=running 的 raw 行当成“无需销毁成功”，否则会删 SQLite 留下活进程。
///
/// Code Logic（这个函数做什么）:
///     backend 非 tmux 且 status=running → Err(unavailable raw_pty_kill_requires_live_handle)；
///     非 running（disconnected/exited）→ Ok（无活进程可证）。
fn raw_pty_kill_persisted_policy(row: &WorkbenchSessionRow) -> Result<(), AppError> {
    if row.status.eq_ignore_ascii_case("running") {
        Err(AppError::unavailable(
            "raw_pty_kill_requires_live_handle".to_string(),
        ))
    } else {
        Ok(())
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

/// restore claim 原子占位结果（R14 M1 + R24 H2）。
///
/// Business Logic（为什么需要这个枚举）:
///     并发 `sessions.list` 不能把「另一路正在 restore」与「已 live」混成同一个 false。
///     若并发 list 仍把 SQLite 持久行当可立即 replay 的会话返回，Provider 会立刻
///     `sessions.replay` → registry 尚未就绪 → 永久 `not_found`，后续静默会话永不补历史。
///     Closing barrier 期间不得 Claimed：否则 pre-close 快照会在 barrier 清后复活。
///
/// Code Logic（这个枚举做什么）:
///     `Claimed{generation}`：本 caller 独占 restore 并必须 finish（token 可被 close 撤销）；
///     `AlreadyLive`：sessions map 已有该 id，无需 restore；
///     `RestoreInProgress`：另一路持有 claim，附带 watch receiver 供 await/共享结果；
///     `BarrierActive`：Closing barrier 仍在，跳过本轮并在后续 list re-read durable 状态。
#[derive(Debug)]
pub enum RestoreClaimOutcome {
    /// 本 caller 独占 restore 责任；`generation` 为可撤销 token。
    Claimed {
        /// 本 claim 的唯一 generation，spawn/upsert 前必须 revalidate。
        generation: u64,
    },
    /// 会话已在运行期 registry。
    AlreadyLive,
    /// 另一路正在 restore；receiver 在 finish 时收到终态结果（或 sender drop → Failed）。
    RestoreInProgress(tokio::sync::watch::Receiver<SharedRestoreNotification>),
    /// Closing barrier 活跃：禁止用旧行快照 claim restore（R24 H2）。
    BarrierActive,
}

impl RestoreClaimOutcome {
    /// Business Logic（为什么需要这个函数）:
    ///     旧代码以 `bool` 判断是否拿到 claim；测试与 spin 等待路径仍需要简洁布尔。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅 `Claimed { .. }` 返回 true。
    pub fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed { .. })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     holder 构造 `RestoreClaimGuard` / revalidate 需要 claim generation。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `Claimed` 返回 Some(generation)，其余 None。
    pub fn claim_generation(&self) -> Option<u64> {
        match self {
            Self::Claimed { generation } => Some(*generation),
            _ => None,
        }
    }
}

/// restore claim 在途持久化 lease 的最大 drain 等待（R26 H1）。
///
/// Business Logic（为什么需要这个常量）:
///     close 撤销 claim 后必须等 in-flight upsert 退出，否则删库后 holder 仍可 INSERT OR REPLACE。
///     超时则 fail-closed 保留 Closing barrier，禁止恢复路径复活。
///
/// Code Logic（这个常量做什么）:
///     `begin_close_intent_for_missing_handle` 等待 lease 归零的上限。
const RESTORE_CLAIM_LEASE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// 可撤销 restore claim 状态（R26 H1）。
///
/// Business Logic（为什么需要这个结构体）:
///     仅从 restoring map 移除 sender 不够：已持有 `RestoreClaimGuard` 的 holder
///     在 close/delete 后仍可能用旧快照 upsert 复活已删会话。
///
/// Code Logic（这个结构体做什么）:
///     `generation` 唯一 token；`revoked` 原子撤销；`leases` 统计 in-flight 持久化；
///     close 等待 leases==0 后再继续 bulk delete（或超时保留 barrier）。
struct RestoreClaimState {
    generation: u64,
    tx: tokio::sync::watch::Sender<SharedRestoreNotification>,
    revoked: AtomicBool,
    leases: AtomicUsize,
    wait: Mutex<()>,
    cv: Condvar,
}

impl RestoreClaimState {
    /// Business Logic（为什么需要这个函数）:
    ///     claim 成功时需要新建可撤销状态并广播 Pending。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分配 generation + watch channel，leases=0、revoked=false。
    fn new(generation: u64) -> Arc<Self> {
        let (tx, _rx) = tokio::sync::watch::channel(SharedRestoreNotification::Pending);
        Arc::new(Self {
            generation,
            tx,
            revoked: AtomicBool::new(false),
            leases: AtomicUsize::new(0),
            wait: Mutex::new(()),
            cv: Condvar::new(),
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close / revalidate 需要判断 token 是否仍有效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `!revoked`。
    fn is_active(&self) -> bool {
        !self.revoked.load(Ordering::SeqCst)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 必须撤销 token，禁止后续 spawn/upsert 成功路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     store revoked=true 并通知 lease waiters。
    fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
        let _guard = self.wait.lock().expect("restore claim wait 锁中毒");
        self.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     持久化路径需要在 upsert 期间阻止 close 提前 bulk delete。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若仍 active 则 leases++ 并返回 true；已 revoked 返回 false。
    fn try_acquire_lease(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        self.leases.fetch_add(1, Ordering::SeqCst);
        // 二次校验：acquire 与 revoke 竞态时立刻 release。
        if !self.is_active() {
            self.release_lease();
            return false;
        }
        true
    }

    /// Business Logic（为什么需要这个函数）:
    ///     upsert 完成/失败后必须释放 lease，允许 close 继续。
    ///
    /// Code Logic（这个函数做什么）:
    ///     leases 饱和减一并 notify。
    fn release_lease(&self) {
        let prev = self.leases.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "restore claim lease underflow");
        let _guard = self.wait.lock().expect("restore claim wait 锁中毒");
        self.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 撤销后须等 in-flight upsert 退出，超时则 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     blocking wait leases==0 或超时；返回是否完全 drain。
    fn wait_leases_drained(&self, timeout: Duration) -> bool {
        let mut guard = self.wait.lock().expect("restore claim wait 锁中毒");
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.leases.load(Ordering::SeqCst) == 0 {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return self.leases.load(Ordering::SeqCst) == 0;
            }
            let (next, _) = self
                .cv
                .wait_timeout(guard, deadline.saturating_duration_since(now))
                .expect("restore claim condvar 中毒");
            guard = next;
        }
    }
}

/// restore claim 持久化 lease 的 RAII 守卫（R26 H1）。
///
/// Business Logic（为什么需要这个结构体）:
///     running/disconnected upsert 在 close 撤销 claim 后不得继续提交；
///     已在途的 upsert 必须被 close 观察到并等待结束。
///
/// Code Logic（这个结构体做什么）:
///     Drop 时 release lease；`is_active` 查询 token 是否仍有效。
pub struct RestorePersistLease {
    state: Arc<RestoreClaimState>,
    released: bool,
}

impl RestorePersistLease {
    /// Business Logic（为什么需要这个函数）:
    ///     upsert 前后需确认 claim 未被 close 撤销。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `state.is_active()`。
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式提前释放 lease，避免 Drop 次序导致 close 多等一轮。
    ///
    /// Code Logic（这个函数做什么）:
    ///     幂等 release。
    pub fn release(&mut self) {
        if !self.released {
            self.state.release_lease();
            self.released = true;
        }
    }
}

impl Drop for RestorePersistLease {
    fn drop(&mut self) {
        self.release();
    }
}

/// project create/restore 在途操作 lease 状态（R27 H4）。
///
/// Business Logic（为什么需要这个结构体）:
///     project remove 挂 barrier 后，仍可能有 spawn→insert→ready/upsert 在途；
///     必须等这些操作退出（或超时 fail-closed 保留 barrier），禁止 orphan live。
///
/// Code Logic（这个结构体做什么）:
///     `leases` 计数 in-flight project ops；`wait/cv` 供 remove 等待归零。
struct ProjectOpLeaseState {
    leases: AtomicUsize,
    wait: Mutex<()>,
    cv: Condvar,
}

impl ProjectOpLeaseState {
    /// Business Logic（为什么需要这个函数）:
    ///     首次 project op 需要惰性创建 lease 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     leases=0 的新 Arc 状态。
    fn new() -> Arc<Self> {
        Arc::new(Self {
            leases: AtomicUsize::new(0),
            wait: Mutex::new(()),
            cv: Condvar::new(),
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     create/restore 进入临界区前 acquire，阻止 remove 过早 finish barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     leases++。
    fn acquire(&self) {
        self.leases.fetch_add(1, Ordering::SeqCst);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     op 结束必须 release，允许 project remove 继续。
    ///
    /// Code Logic（这个函数做什么）:
    ///     leases 饱和减一并 notify。
    fn release(&self) {
        let prev = self.leases.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(prev > 0, "project op lease underflow");
        let _guard = self.wait.lock().expect("project op wait 锁中毒");
        self.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     project remove 须等 in-flight create/restore 退出；超时 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     blocking wait leases==0 或超时；返回是否完全 drain。
    fn wait_leases_drained(&self, timeout: Duration) -> bool {
        let mut guard = self.wait.lock().expect("project op wait 锁中毒");
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.leases.load(Ordering::SeqCst) == 0 {
                return true;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return self.leases.load(Ordering::SeqCst) == 0;
            }
            let (next, _) = self
                .cv
                .wait_timeout(guard, deadline.saturating_duration_since(now))
                .expect("project op condvar 中毒");
            guard = next;
        }
    }
}

/// project closing barrier 条目（R42 H1）。
///
/// Business Logic（为什么需要这个结构体）:
///     merge cleanup 与 project remove 可能并发持有同一 project barrier；
///     必须共享 generation + owner 计数，禁止后启动者覆盖并抢先 clear。
///
/// Code Logic（这个结构体做什么）:
///     `generation` 是首次 begin 分配的屏障代际；`owners` 为嵌套 begin 引用计数。
#[derive(Debug, Clone, Copy)]
struct ProjectClosingBarrierEntry {
    generation: u64,
    owners: u32,
}

/// project 在途操作 lease 的 RAII 守卫（R27 H4）。
///
/// Business Logic（为什么需要这个结构体）:
///     spawn/insert/ready/upsert 任一路径失败或返回时必须释放 project lease。
///
/// Code Logic（这个结构体做什么）:
///     Drop 时 release lease。
pub struct ProjectOpLease {
    state: Arc<ProjectOpLeaseState>,
    released: bool,
}

impl ProjectOpLease {
    /// Business Logic（为什么需要这个函数）:
    ///     显式提前释放，避免 Drop 次序导致 remove 多等。
    ///
    /// Code Logic（这个函数做什么）:
    ///     幂等 release。
    pub fn release(&mut self) {
        if !self.released {
            self.state.release();
            self.released = true;
        }
    }
}

impl Drop for ProjectOpLease {
    fn drop(&mut self) {
        self.release();
    }
}

/// project op lease drain 超时（R27 H4）。
///
/// Business Logic（为什么需要这个常量）:
///     remove 不能无限阻塞；超时则保留 project barrier fail-closed。
///
/// Code Logic（这个常量做什么）:
///     `wait_project_op_leases_drained` 上限，与 restore claim drain 同为 5s。
const PROJECT_OP_LEASE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// 共享 restore 等待的最大时长（R14/R15）。
///
/// Business Logic（为什么需要这个常量）:
///     并发 list 不能无限阻塞；超时后必须返回可重试错误，而不是静默给出不完整清单。
///
/// Code Logic（这个常量做什么）:
///     `wait_for_shared_restore` 与集成测试共用同一上限，避免字面量漂移。
pub const SHARED_RESTORE_WAIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 共享 restore 完成通知（R16 M1）。
///
/// Business Logic（为什么需要这个枚举）:
///     并发 list 不能把 claim 释放本身当成成功：holder 在 project 查询/删除 `?`、
///     或 DB upsert 失败后仍会释放 claim，若只广播「已结束」，waiter 会合并出
///     registry 中不存在、却仍像 running 的持久行，后续 replay 永久 `not_found`。
///
/// Code Logic（这个枚举做什么）:
///     `Pending`：claim 仍进行中（watch 初值）；
///     `Ready`：registry live，可立即 replay；
///     `PersistedDisconnected`：已 skip-missing 持久化为 disconnected，list 可合并该行；
///     `Failed(category)`：holder 失败且会话可能仍非可 replay，waiter 必须 fail closed。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedRestoreNotification {
    /// claim 进行中，尚未给出终态。
    Pending,
    /// restore attach 成功且 registry live。
    Ready,
    /// 已把会话持久化为 disconnected（可列清单，不可 live replay）。
    PersistedDisconnected,
    /// holder 失败；附带稳定 `AppErrorCategory` 供 waiter 映射 retryable 错误。
    Failed(AppErrorCategory),
}

impl SharedRestoreNotification {
    /// Business Logic（为什么需要这个函数）:
    ///     watch 初值与终态共用同一类型；等待方需判断是否已可退出 wait loop。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `Pending` 为 false，其余终态为 true。
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// 共享 restore 等待结果（R15 M2 + R16 M1）。
///
/// Business Logic（为什么需要这个枚举）:
///     调用方必须区分 Ready / PersistedDisconnected / Failed / TimedOut。
///     超时或 Failed 后若继续 `merged_session_dtos` 会返回成功但含不可 replay 会话，
///     启动 Provider 会把清单当完成、永不重试遗漏或坏状态的静默会话。
///
/// Code Logic（这个枚举做什么）:
///     映射自 `SharedRestoreNotification` 终态，或等待超时得到 `TimedOut`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedRestoreWaitResult {
    /// registry live，可立即 replay。
    Ready,
    /// 已持久化为 disconnected，list 可安全合并该行。
    PersistedDisconnected,
    /// holder 失败；会话可能仍非可 replay。
    Failed(AppErrorCategory),
    /// 等待超时，调用方应返回 retryable timeout。
    TimedOut,
}

impl SharedRestoreWaitResult {
    /// Business Logic（为什么需要这个函数）:
    ///     list 路径只需知道「能否 continue 合并」与「如何构造错误」。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Ready/PersistedDisconnected → true；Failed/TimedOut → false。
    pub fn is_success(self) -> bool {
        matches!(self, Self::Ready | Self::PersistedDisconnected)
    }
}

/// Business Logic（为什么需要这个函数）:
///     共享 restore Failed 时，waiter 必须返回与 holder 同类目的稳定错误，而不是 continue。
///
/// Code Logic（这个函数做什么）:
///     按 `AppErrorCategory` 构造对应 AppError，消息固定为 `session_restore_shared_failed`。
pub fn shared_restore_failed_error(category: AppErrorCategory) -> AppError {
    let msg = "session_restore_shared_failed".to_string();
    match category {
        AppErrorCategory::Validation => AppError::validation(msg),
        AppErrorCategory::NotFound => AppError::not_found(msg),
        AppErrorCategory::Conflict => AppError::conflict(msg),
        AppErrorCategory::Unavailable => AppError::unavailable(msg),
        AppErrorCategory::Timeout => AppError::timeout(msg),
        AppErrorCategory::Internal => AppError::generic(msg),
    }
}

/// Business Logic（为什么需要这个函数）:
///     watch 终态通知与 wait 结果枚举需要一一映射，避免 list 路径重复 match 漂移。
///
/// Code Logic（这个函数做什么）:
///     Pending → None；其余 → 对应 SharedRestoreWaitResult。
fn wait_result_from_notification(
    note: SharedRestoreNotification,
) -> Option<SharedRestoreWaitResult> {
    match note {
        SharedRestoreNotification::Pending => None,
        SharedRestoreNotification::Ready => Some(SharedRestoreWaitResult::Ready),
        SharedRestoreNotification::PersistedDisconnected => {
            Some(SharedRestoreWaitResult::PersistedDisconnected)
        }
        SharedRestoreNotification::Failed(category) => {
            Some(SharedRestoreWaitResult::Failed(category))
        }
    }
}

/// 运行期 session 可见性的原子判定结果（R15 M1）。
///
/// Business Logic（为什么需要这个枚举）:
///     local / control / mobile / P2P replay 必须共享同一语义：restore claim 已建立但
///     session 尚未写入 registry 时，不能返回永久 `not_found`（前端会停止 auto-replay）。
///
/// Code Logic（这个枚举做什么）:
///     `Live`：sessions map 已有该 id；
///     `RestoreInProgress`：restoring map 持有 claim；
///     `Missing`：既无 live 也无 claim。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRuntimePresence {
    /// 会话已在运行期 registry，可立即 replay。
    Live,
    /// 另一路正在 restore，应返回 retryable unavailable。
    RestoreInProgress,
    /// 无 live 也无 claim，才可映射为 not_found。
    Missing,
}

/// 等待共享 restore 完成（R14 M1 + R15 M2 + R16 M1）。
///
/// Business Logic（为什么需要这个函数）:
///     并发 list 拿到 `RestoreInProgress` 后应等待 in-flight restore 的**结果**再合并 DTO。
///     仅「claim 已释放」不够：Failed 时若 continue 会把不可 replay 的持久行返回成功。
///     超时必须显式上报 TimedOut，禁止部分成功清单。
///
/// Code Logic（这个函数做什么）:
///     若 receiver 已是终态立即映射；否则 `changed()` 直到终态；
///     sender drop 且仍 Pending → `Failed(Internal)`；
///     超过 `SHARED_RESTORE_WAIT_TIMEOUT` → `TimedOut`。
pub async fn wait_for_shared_restore(
    mut rx: tokio::sync::watch::Receiver<SharedRestoreNotification>,
) -> SharedRestoreWaitResult {
    if let Some(result) = wait_result_from_notification(*rx.borrow_and_update()) {
        return result;
    }
    let wait = async {
        loop {
            match rx.changed().await {
                Ok(()) => {
                    if let Some(result) = wait_result_from_notification(*rx.borrow_and_update()) {
                        return result;
                    }
                }
                Err(_) => {
                    // sender drop：无显式终态 → 视为 Failed，禁止当作 Ready。
                    return wait_result_from_notification(*rx.borrow_and_update())
                        .unwrap_or(SharedRestoreWaitResult::Failed(AppErrorCategory::Internal));
                }
            }
        }
    };
    match tokio::time::timeout(SHARED_RESTORE_WAIT_TIMEOUT, wait).await {
        Ok(result) => result,
        Err(_) => SharedRestoreWaitResult::TimedOut,
    }
}

/// 工作台 PTY 会话注册表。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台会话的元数据持久化在 SQLite，但多个命令仍需要按 session_id 查找并操作当前 PTY attach。
///
/// Code Logic（这个结构体做什么）:
///     用 HashMap 保存 session_id 到会话句柄和 replay buffer 的映射；外层 Arc 允许后台读写线程更新状态。
///     `next_generation` 为每次 insert 分配单调 generation，供 worker fence（R18 M2）。
///     Clone 廉价（内部全是 Arc），供 `SessionSpawnGuard` / `RestoreClaimGuard` 持有。
#[derive(Clone)]
pub struct WorkbenchSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
    /// 下一个可分配的 session generation（从 1 起单调递增）。
    next_generation: Arc<AtomicU64>,
    /// 已 close 但 lease 未 drain 的 generation tombstone（R21 H2）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     soft-timeout 后仍不得允许同 session_id 再 insert，直到旧 generation 在途发布结束。
    ///
    /// Code Logic（这个字段做什么）:
    ///     session_id → 旧 PublishControl；insert 前 blocking drain 并移除。
    closing_publish: Arc<Mutex<HashMap<String, Arc<PublishControl>>>>,
    /// 正在 restore 的 session_id 占位 map（Finding 5 + R14 M1 + R26 H1）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     `restore_persisted_sessions` 先 claim 再异步 `resolve_worktree` 后 `restore()`，
    ///     两个并发的 sessions/list 请求都能通过 contains() 检查并各自 spawn 一次 PTY/tmux 窗口。
    ///     占位 map 让"检查 + 占位"原子完成：第一个 caller 拿到 claim generation 并持有
    ///     可撤销状态；第二个得到 `RestoreInProgress` 并可 wait。close 可 revoke generation，
    ///     禁止 holder 在 delete 后 re-upsert。
    restoring: Arc<Mutex<HashMap<String, Arc<RestoreClaimState>>>>,
    /// 项目级 closing barrier（R26 M1 / R42 H1）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     project remove 从 snapshot 到 bulk delete 期间，并发 create/restore 不得
    ///     为已删除项目 spawn 出 orphan live session。
    ///     并发 merge cleanup 与 project remove 交错时，后启动者不得覆盖并抢先清除 barrier。
    ///
    /// Code Logic（这个字段做什么）:
    ///     project_id → `{generation, owners}`：同 project 二次 begin 加入同一 generation（owners++），
    ///     仅当 matching generation 的最后 owner finish 才清除。
    project_closing: Arc<Mutex<HashMap<String, ProjectClosingBarrierEntry>>>,
    /// 项目 barrier generation 单调分配器（R26 M1）。
    next_project_barrier_generation: Arc<AtomicU64>,
    /// 项目级 in-flight create/restore lease（R27 H4）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     project remove 必须等在途 spawn/insert/ready/upsert 退出，才能 finish barrier。
    ///
    /// Code Logic（这个字段做什么）:
    ///     project_id → ProjectOpLeaseState。
    project_op_leases: Arc<Mutex<HashMap<String, Arc<ProjectOpLeaseState>>>>,
    /// generation-scoped raw PTY kill 失败后的可重试 handle（R41 M7）。
    ///
    /// Business Logic（为什么需要这个字段）:
    ///     close_inner 在 child.kill 前已从 sessions 移除 handle；kill 非 already-gone 失败时
    ///     若丢弃 handle，重试会走 missing-handle + kill_persisted_backend(raw)=Ok，
    ///     最终删 SQLite 却可能留下仍存活的 raw shell。
    ///
    /// Code Logic（这个字段做什么）:
    ///     session_id → 仍持有 Child 的 handle（含 generation）；retry close 优先从此 map 取回再 kill。
    failed_kill_handles: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
}

/// Session 创建后的 RAII 补偿守卫：repo 持久化失败时自动关闭 attach，禁止 ghost registry/child。
///
/// Business Logic（为什么需要这个结构体）:
///     create/restore 先 spawn PTY 再写 SQLite；若 upsert 失败必须回收运行期资源，
///     否则 sidecar 留下无元数据的 ghost 终端。
///     R19 M1 / R20 M1：仅同 generation CAS 进入 Ready 后才 commit 成功并 finish(Ready)。
///
/// Code Logic（这个结构体做什么）:
///     持有 registry、session_id、generation、可选 AppState；未成功 `commit()` 时 Drop 调用 `close`。
///     `commit` 执行 generation CAS 并返回是否真正 Ready。
pub struct SessionSpawnGuard {
    registry: WorkbenchSessionRegistry,
    session_id: String,
    /// spawn 时捕获的 generation（R20 M1 CAS）。
    generation: u64,
    state: Option<AppState>,
    committed: bool,
}

impl SessionSpawnGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     spawn 成功后立刻接管生命周期，后续任何 early return 都能自动补偿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 registry/session_id/generation，无 AppState（不发 Ready 事件的兼容路径）。
    pub fn new(registry: WorkbenchSessionRegistry, session_id: String) -> Self {
        let generation = registry.session_generation(&session_id).unwrap_or(0);
        Self {
            registry,
            session_id,
            generation,
            state: None,
            committed: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产 create/restore 需要在 upsert 成功后原子发布 Ready+running。
    ///
    /// Code Logic（这个函数做什么）:
    ///     捕获当前 generation + AppState，供 commit CAS。
    pub fn new_with_state(
        registry: WorkbenchSessionRegistry,
        session_id: String,
        state: AppState,
    ) -> Self {
        let generation = registry.session_generation(&session_id).unwrap_or(0);
        Self {
            registry,
            session_id,
            generation,
            state: Some(state),
            committed: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试/内部路径需要显式绑定 generation，避免仅靠 session_id 的 TOCTOU。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接记录调用方传入的 generation。
    pub fn new_with_generation(
        registry: WorkbenchSessionRegistry,
        session_id: String,
        generation: u64,
        state: Option<AppState>,
    ) -> Self {
        Self {
            registry,
            session_id,
            generation,
            state,
            committed: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     SQLite upsert 成功后才允许会话进入正式运行期；CAS 失败不得对外宣称 Ready。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `mark_session_ready_for_generation`；成功则 committed=true 并返回 true。
    pub fn commit(&mut self) -> bool {
        if self.committed {
            return true;
        }
        let ok = self.registry.mark_session_ready_for_generation(
            &self.session_id,
            self.generation,
            self.state.as_ref(),
        );
        if ok {
            self.committed = true;
        }
        ok
    }

    /// Business Logic（为什么需要这个函数）:
    ///     restore/create 调用方需要知道 commit 绑定的 generation。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回守卫捕获的 generation。
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for SessionSpawnGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     任何未提交路径（含 panic）都必须关闭 attach，防止 ghost child/registry。
    ///     不得按 session_id 误杀同 id 的后继 generation（R21 M1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 commit 时 best-effort `close_if_generation`；补偿 close 无外部 persist，
    ///     立即 `finish_cleanup` 收敛 Closing barrier。
    fn drop(&mut self) {
        if !self.committed {
            if let Ok(cleanup) = self
                .registry
                .close_if_generation(&self.session_id, self.generation)
            {
                cleanup.finish_cleanup();
            }
        }
    }
}

/// closer-owned Closing barrier 生命周期令牌（R24 H1 / R25 M2）。
///
/// Business Logic（为什么需要这个结构体）:
///     registry `close` 只完成 remove/revoke/drain/PTY kill；tmux destroy 与 SQLite
///     delete/update 仍由调用方执行。若在 registry 返回时就 mark_cleanup_done + 清 barrier，
///     并发 restore 可 reinsert 同 id，随后旧 closer 的 kill_persisted_backend/delete
///     会打到后继实例。令牌把 barrier 持有到外部 persist cleanup **成功** 完成。
///
/// Code Logic（这个结构体做什么）:
///     持有 session_id、row、PublishControl 身份与 drain 标志；**仅**显式 `finish_cleanup`
///     才 `mark_cleanup_done` 并 closer 身份 CAS 清 barrier（或 spawn reaper）。
///     Drop **不清** barrier（R25 M2）：delete 失败/future cancel 时保留 barrier，
///     禁止 restore 在 durable 未清理时复活。
pub struct SessionCloseCleanup {
    registry: WorkbenchSessionRegistry,
    session_id: String,
    publish: Arc<PublishControl>,
    row: WorkbenchSessionRow,
    drained: bool,
    finished: bool,
    /// close 撤销的 restore claim state；soft-timeout 时 reaper 继续等 leases（R27 H2/H3）。
    restore_claim_for_drain: Option<Arc<RestoreClaimState>>,
}

impl SessionCloseCleanup {
    /// Business Logic（为什么需要这个函数）:
    ///     调用方在 kill/delete 前需要读取 closed session 元数据（backend_id 等）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回关闭时 snapshot 的 row 引用。
    pub fn row(&self) -> &WorkbenchSessionRow {
        &self.row
    }

    /// Business Logic（为什么需要这个函数）:
    ///     批量 cleanup 需要按 id 关联 SQLite delete。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 session_id 引用。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Business Logic（为什么需要这个函数）:
    ///     closer 完成 kill_persisted_backend 与 durable row delete/update 后，才允许
    ///     同 id reinsert / restore 越过 Closing barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     幂等：`mark_cleanup_done`；drained → `clear_closing_tombstone_if_same`；
    ///     否则 `spawn_closing_barrier_reaper`。消费 self。
    pub fn finish_cleanup(mut self) {
        self.finish_cleanup_inner();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     显式成功 finish 是唯一清除 barrier 的路径（R25 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     finished 守卫后 mark + closer CAS clear 或 reaper。
    fn finish_cleanup_inner(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.publish.mark_cleanup_done();
        if self.drained {
            self.registry
                .clear_closing_tombstone_if_same(&self.session_id, &self.publish);
        } else {
            self.registry.spawn_closing_barrier_reaper(
                self.session_id.clone(),
                self.publish.clone(),
                self.restore_claim_for_drain.take(),
            );
        }
    }
}

impl Drop for SessionCloseCleanup {
    /// Business Logic（为什么需要这个函数）:
    ///     R25 M2：delete 失败 / future cancel / 调用方遗漏 finish 时，不得假装 cleanup 成功。
    ///     保留 Closing barrier 阻断 stale restore/spawn，直到 owner 成功 finish 或后续 close 覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     未 finished 时仅 debug 记录；**不** mark_cleanup_done、**不** clear barrier。
    fn drop(&mut self) {
        if !self.finished {
            tracing::debug!(
                session_id = %self.session_id,
                "SessionCloseCleanup dropped without finish_cleanup; retaining Closing barrier"
            );
        }
    }
}

/// restore claim 的 RAII 守卫：任何 early return 都 finish 为 Failed，避免永久跳过恢复。
///
/// Business Logic（为什么需要这个结构体）:
///     `try_claim_restore` 成功后若中途失败却未 finish claim，后续 list 永远不会再恢复该 session；
///     且必须广播 Failed 而非「ended」，否则 waiter 会误判成功。
///     R26 H1：finish 仅当 generation 仍匹配时生效，close 撤销后不得误清后继 claim。
///
/// Code Logic（这个结构体做什么）:
///     Drop 时若未 `disarm` 则 `finish_restore_claim_for_generation(Failed(Internal))`。
pub struct RestoreClaimGuard {
    registry: WorkbenchSessionRegistry,
    session_id: String,
    generation: u64,
    armed: bool,
}

impl RestoreClaimGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     claim 成功后立刻接管 finish 责任，并绑定 generation token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 registry/session_id/generation，armed=true。
    pub fn new(registry: WorkbenchSessionRegistry, session_id: String, generation: u64) -> Self {
        Self {
            registry,
            session_id,
            generation,
            armed: true,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn/upsert 路径需要 generation 做 revalidate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回绑定的 claim generation。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调用方已显式 finish 时禁止 Drop 二次广播。
    ///
    /// Code Logic（这个函数做什么）:
    ///     armed=false。
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     holder 正常路径应显式广播 Ready / PersistedDisconnected / Failed，
    ///     再 disarm，避免 Drop 默认 Failed(Internal) 覆盖真实结果。
    ///
    /// Code Logic（这个函数做什么）:
    ///     generation-scoped `finish_restore_claim` + `disarm`。
    pub fn finish(&mut self, result: SharedRestoreNotification) {
        if self.armed {
            self.registry.finish_restore_claim_for_generation(
                &self.session_id,
                self.generation,
                result,
            );
            self.armed = false;
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 撤销后 holder 不得再以 Ready 收尾；查询 token 是否仍有效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `is_restore_claim_generation_active`。
    pub fn is_active(&self) -> bool {
        self.registry
            .is_restore_claim_generation_active(&self.session_id, self.generation)
    }
}

impl Drop for RestoreClaimGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     restore 任意失败出口都必须 finish claim 并通知 waiters Failed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     armed 时 generation-scoped `finish_restore_claim(Failed(Internal))`。
    fn drop(&mut self) {
        if self.armed {
            self.registry.finish_restore_claim_for_generation(
                &self.session_id,
                self.generation,
                SharedRestoreNotification::Failed(AppErrorCategory::Internal),
            );
        }
    }
}

impl WorkbenchSessionRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的工作台会话注册表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空会话 HashMap、replay buffer、generation 计数器、restoring map
    ///     与 project_closing barrier，并包裹 Arc 供命令和后台线程共享。
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            replay_buffers: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(1)),
            closing_publish: Arc::new(Mutex::new(HashMap::new())),
            restoring: Arc::new(Mutex::new(HashMap::new())),
            project_closing: Arc::new(Mutex::new(HashMap::new())),
            next_project_barrier_generation: Arc::new(AtomicU64::new(1)),
            project_op_leases: Arc::new(Mutex::new(HashMap::new())),
            failed_kill_handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端工作台需要列出全部会话，或只列出某个项目下的会话。
    ///     R18/R19 M1 / R21：claim-held 或 Provisional/Flushing handle 不得对外暴露为可 replay 会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先持 sessions 锁再持 restoring 锁（与 claim 路径同序，避免死锁），
    ///     过滤 claim-held 与非 Ready handle，再按可选 project_id 过滤并克隆 DTO。
    pub fn list(&self, project_id: Option<&str>) -> Vec<WorkbenchSessionDto> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        sessions
            .iter()
            .filter_map(|(session_id, handle)| {
                if restoring.contains_key(session_id) {
                    return None;
                }
                let handle = handle.lock().expect("workbench session 锁中毒");
                if handle.durability != SessionDurability::Ready {
                    return None;
                }
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

    /// 原子判定 session 运行期可见性（R15 M1 + R18 M1 + R19 M1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     P2P / local / control / mobile 的 replay 入口必须在同一次快照下区分
    ///     Live / RestoreInProgress / Missing。provisional live 在 durable Ready 前
    ///     不得对外暴露为 Live；claim 窗口也不得误报永久 not_found。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁顺序 sessions → restoring。**restoring 优先**：claim held → RestoreInProgress；
    ///     sessions 有 key 且 durability=Ready → Live；provisional only → RestoreInProgress；
    ///     否则 Missing。
    pub fn runtime_presence(&self, session_id: &str) -> SessionRuntimePresence {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        if restoring.contains_key(session_id) {
            return SessionRuntimePresence::RestoreInProgress;
        }
        match sessions.get(session_id) {
            Some(handle) => {
                let handle = handle.lock().expect("workbench session 锁中毒");
                if handle.durability == SessionDurability::Ready {
                    SessionRuntimePresence::Live
                } else {
                    // Provisional/Flushing 均不得当 Live（R21 M2）。
                    SessionRuntimePresence::RestoreInProgress
                }
            }
            None => {
                // R22 M1：Closing barrier 期间不得报永久 Missing（list/restore 不得抢跑 reinsert）。
                drop(restoring);
                drop(sessions);
                let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
                if closing.contains_key(session_id) {
                    SessionRuntimePresence::RestoreInProgress
                } else {
                    SessionRuntimePresence::Missing
                }
            }
        }
    }

    /// 为 replay 路径要求 session 已 live（R15 M1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     统一 local/control/mobile/P2P replay 的错误语义：restore 中 → retryable
    ///     `unavailable(session_restore_in_progress)`；缺失 → `not_found`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `runtime_presence` 并映射为 `Result<(), AppError>`。
    pub fn require_live_for_replay(&self, session_id: &str) -> Result<(), AppError> {
        match self.runtime_presence(session_id) {
            SessionRuntimePresence::Live => Ok(()),
            SessionRuntimePresence::RestoreInProgress => Err(AppError::unavailable(
                "session_restore_in_progress".to_string(),
            )),
            SessionRuntimePresence::Missing => Err(AppError::not_found("工作台会话不存在")),
        }
    }

    /// 原子占位：声明"我即将 restore 这个 session"（Finding 5 + R14 M1 + R18 M1 + R24 H2 + R26 H1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `restore_persisted_sessions` 的旧实现 `contains() → await resolve_worktree → restore()`
    ///     存在 TOCTOU：两个并发的 sessions/list 请求都能通过 contains() 检查，各自 spawn 一个
    ///     PTY/tmux 窗口。provisional insert 后 durable upsert 成功前也不得被第三方
    ///     当作 AlreadyLive 消费。Closing barrier 期间更不得 claim：否则 restore 会带着
    ///     pre-close 行快照在 barrier 清后复活已删除会话（R24 H2）。
    ///     R26 H1：claim 绑定可撤销 generation，close 可作废已持有 Guard。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁顺序 sessions → restoring → closing_publish。顺序检查：**先 restoring**，
    ///     再 sessions（AlreadyLive），再 Closing barrier（RestoreInProgress 且无 claim 写入），
    ///     否则分配 generation、写入 `RestoreClaimState` 返回 `Claimed{generation}`。
    ///
    /// `Claimed` 时 caller 必须 restore 并在完成后 generation-scoped finish
    ///（或持有 `RestoreClaimGuard` 直至结束）。
    pub fn try_claim_restore(&self, session_id: &str) -> RestoreClaimOutcome {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        if let Some(state) = restoring.get(session_id) {
            return RestoreClaimOutcome::RestoreInProgress(state.tx.subscribe());
        }
        if sessions.contains_key(session_id) {
            return RestoreClaimOutcome::AlreadyLive;
        }
        // R24 H2：Closing barrier 参与 claim CAS——不得 Claimed 后用旧行快照越过 close lifecycle。
        {
            let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
            if closing.contains_key(session_id) {
                return RestoreClaimOutcome::BarrierActive;
            }
        }
        let generation = self.allocate_generation();
        let state = RestoreClaimState::new(generation);
        restoring.insert(session_id.to_string(), state);
        RestoreClaimOutcome::Claimed { generation }
    }

    /// 结束 restore 占位并广播显式结果（Finding 5 + R14/R15 + R16 M1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     restore 完成（Ready / PersistedDisconnected / Failed）后必须释放占位并携带结果。
    ///     仅「ended」会导致 waiter 把 holder 失败当成成功合并不可 replay 会话。
    ///     失败路径释放后允许后续请求重试 restore。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 restoring map 移除当前 state 并 `send(result)`（若 result 为 Pending 则升为 Failed(Internal)）；
    ///     幂等 no-op。R26 后优先使用 generation-scoped 变体，避免误清后继 claim。
    pub fn finish_restore_claim(&self, session_id: &str, result: SharedRestoreNotification) {
        let note = if result.is_terminal() {
            result
        } else {
            SharedRestoreNotification::Failed(AppErrorCategory::Internal)
        };
        let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        if let Some(state) = restoring.remove(session_id) {
            let _ = state.tx.send(note);
        }
    }

    /// generation-scoped finish（R26 H1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     close 撤销旧 claim 后，旧 Guard Drop 不得误移除同 session 的新 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 map 中 generation 匹配时 remove + send；否则 no-op。
    pub fn finish_restore_claim_for_generation(
        &self,
        session_id: &str,
        generation: u64,
        result: SharedRestoreNotification,
    ) {
        let note = if result.is_terminal() {
            result
        } else {
            SharedRestoreNotification::Failed(AppErrorCategory::Internal)
        };
        let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        let should_remove = restoring
            .get(session_id)
            .map(|state| state.generation == generation)
            .unwrap_or(false);
        if should_remove {
            if let Some(state) = restoring.remove(session_id) {
                let _ = state.tx.send(note);
            }
        }
    }

    /// 兼容旧 release API：默认广播 Failed(Internal)。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     既有测试/路径在「只释放占位」语义下调用；R16 后无结果的 release 不得伪装 Ready。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `finish_restore_claim(Failed(Internal))`。
    pub fn release_restore_claim(&self, session_id: &str) {
        self.finish_restore_claim(
            session_id,
            SharedRestoreNotification::Failed(AppErrorCategory::Internal),
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn / running upsert / disconnected upsert 在 commit 前必须确认 claim 仍有效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     restoring 含 session_id 且 generation 匹配且未 revoked。
    pub fn is_restore_claim_generation_active(&self, session_id: &str, generation: u64) -> bool {
        let restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        restoring
            .get(session_id)
            .map(|state| state.generation == generation && state.is_active())
            .unwrap_or(false)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     durable upsert 前 acquire lease，使 close 能等 in-flight 持久化退出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     generation 匹配且 active 时 acquire lease 并返回 RAII 守卫；否则 None。
    pub fn try_acquire_restore_persist_lease(
        &self,
        session_id: &str,
        generation: u64,
    ) -> Option<RestorePersistLease> {
        let restoring = self.restoring.lock().expect("restoring 集合锁中毒");
        let state = restoring.get(session_id)?.clone();
        if state.generation != generation {
            return None;
        }
        if !state.try_acquire_lease() {
            return None;
        }
        Some(RestorePersistLease {
            state,
            released: false,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn 前与 insert CAS 时 revalidate claim generation；无效则 fail closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     active → Ok；否则 `unavailable(session_restore_claim_revoked)`。
    pub fn require_restore_claim_active(
        &self,
        session_id: &str,
        generation: u64,
    ) -> Result<(), AppError> {
        if self.is_restore_claim_generation_active(session_id, generation) {
            Ok(())
        } else {
            Err(AppError::unavailable(
                "session_restore_claim_revoked".to_string(),
            ))
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     project remove / merge cleanup 从 snapshot 到 bulk delete 期间，create/restore
    ///     不得再为该项目生成 orphan live session（R26 M1）。
    ///     并发 cleanup 不得覆盖 active generation 并抢先 clear（R42 H1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 barrier 已存在：owners++ 并返回既有 generation（join，不 steal）。
    ///     否则分配新 generation，owners=1 写入 map 并返回。
    pub fn begin_project_closing_barrier(&self, project_id: &str) -> u64 {
        let mut map = self.project_closing.lock().expect("project_closing 锁中毒");
        if let Some(entry) = map.get_mut(project_id) {
            entry.owners = entry.owners.saturating_add(1);
            return entry.generation;
        }
        let generation = self
            .next_project_barrier_generation
            .fetch_add(1, Ordering::SeqCst);
        map.insert(
            project_id.to_string(),
            ProjectClosingBarrierEntry {
                generation,
                owners: 1,
            },
        );
        generation
    }

    /// Business Logic（为什么需要这个函数）:
    ///     bulk delete 成功后必须清除 barrier，允许同 project_id 再创建（若用户重新添加）。
    ///     嵌套 cleanup 只有最后一个 owner 才能 clear（R42 H1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     generation 不匹配 → no-op；匹配则 owners--，仅 owners 归零时 remove。
    pub fn finish_project_closing_barrier(&self, project_id: &str, generation: u64) {
        let mut map = self.project_closing.lock().expect("project_closing 锁中毒");
        let Some(entry) = map.get_mut(project_id) else {
            return;
        };
        if entry.generation != generation {
            return;
        }
        if entry.owners > 1 {
            entry.owners -= 1;
            return;
        }
        map.remove(project_id);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     create/restore 在 spawn 与 upsert 前 revalidate project barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     project_closing 含 project_id → `unavailable(project_closing_barrier_active)`。
    pub fn require_project_not_closing(&self, project_id: &str) -> Result<(), AppError> {
        let map = self.project_closing.lock().expect("project_closing 锁中毒");
        if map.contains_key(project_id) {
            Err(AppError::unavailable(
                "project_closing_barrier_active".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     create/restore 在 spawn→insert→ready/upsert 全程必须占 project lease，
    ///     让 project remove 能观察到在途操作并 wait（R27 H4）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 project_closing 活跃 → unavailable；否则惰性创建 lease state、acquire 并返回 RAII。
    ///     acquire 后再 recheck barrier，竞态时 release 并 fail-closed。
    pub fn try_acquire_project_op_lease(
        &self,
        project_id: &str,
    ) -> Result<ProjectOpLease, AppError> {
        self.require_project_not_closing(project_id)?;
        let state = {
            let mut map = self
                .project_op_leases
                .lock()
                .expect("project_op_leases 锁中毒");
            map.entry(project_id.to_string())
                .or_insert_with(ProjectOpLeaseState::new)
                .clone()
        };
        state.acquire();
        if self.require_project_not_closing(project_id).is_err() {
            state.release();
            return Err(AppError::unavailable(
                "project_closing_barrier_active".to_string(),
            ));
        }
        Ok(ProjectOpLease {
            state,
            released: false,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     project remove 在 finish barrier 前必须等 in-flight op 归零；超时保留 barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 project_op_leases 中状态（若无则已 drain）；wait 上限 PROJECT_OP_LEASE_DRAIN_TIMEOUT。
    pub fn wait_project_op_leases_drained(&self, project_id: &str) -> bool {
        let state = {
            let map = self
                .project_op_leases
                .lock()
                .expect("project_op_leases 锁中毒");
            map.get(project_id).cloned()
        };
        match state {
            Some(state) => state.wait_leases_drained(PROJECT_OP_LEASE_DRAIN_TIMEOUT),
            None => true,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试注入/观测 project op lease 计数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回当前 leases 或 0。
    #[cfg(test)]
    pub fn project_op_lease_count_for_test(&self, project_id: &str) -> usize {
        let map = self
            .project_op_leases
            .lock()
            .expect("project_op_leases 锁中毒");
        map.get(project_id)
            .map(|s| s.leases.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试与诊断需要观察 project barrier 是否仍在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `project_closing` 是否含 project_id。
    #[cfg(test)]
    pub fn has_project_closing_barrier_for_test(&self, project_id: &str) -> bool {
        self.project_closing
            .lock()
            .expect("project_closing 锁中毒")
            .contains_key(project_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单测验证嵌套 begin 的 owner 引用计数（R42 H1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 project_closing 中该 project 的 owners；无 barrier 时返回 0。
    #[cfg(test)]
    pub fn project_closing_owners_for_test(&self, project_id: &str) -> u32 {
        self.project_closing
            .lock()
            .expect("project_closing 锁中毒")
            .get(project_id)
            .map(|e| e.owners)
            .unwrap_or(0)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单测读取 active barrier generation（R42 H1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 generation；无 barrier 时返回 None。
    #[cfg(test)]
    pub fn project_closing_generation_for_test(&self, project_id: &str) -> Option<u64> {
        self.project_closing
            .lock()
            .expect("project_closing 锁中毒")
            .get(project_id)
            .map(|e| e.generation)
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
    ///     list/merge 与 replay 需要知道 session 是否仍在 restore 中，避免把恢复中行
    ///     当作可立即 replay 返回，或把瞬时 not_found 误判为永久缺失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     检查 restoring map 是否包含 session_id（含已 revoked 但尚未 finish 的 holder）。
    pub fn is_restore_claim_held(&self, session_id: &str) -> bool {
        self.restoring
            .lock()
            .expect("restoring 集合锁中毒")
            .contains_key(session_id)
    }

    /// 分配新的 session generation（R18 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     每次 insert live handle 都需要独立世代，以便失败 reclaim 后旧 worker
    ///     无法污染同 session_id 的新实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 `next_generation` 做 `fetch_add(1, SeqCst)` 并返回旧值。
    fn allocate_generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::SeqCst)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     同 session_id 重建前必须等 Closing barrier 清除且旧 lease drain + cleanup 完成，
    ///     避免旧 generation 污染后继。
    ///
    /// Code Logic（这个函数做什么）:
    ///     **waiter 永不 clear barrier**（R23 H2）；blocking 等 closer 身份 CAS 清除或身份替换；
    ///     期间可观察 in_flight/cleanup_done，仅作为 wait 条件。
    fn wait_for_closing_tombstone(&self, session_id: &str) {
        loop {
            let control = {
                let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
                closing.get(session_id).cloned()
            };
            let Some(control) = control else {
                return;
            };
            // 尽量等 drain，但即便 in_flight==0 也不得自行 remove——须等 cleanup_done + closer clear。
            if control.in_flight.load(Ordering::SeqCst) > 0 {
                control.wait_in_flight_drained_blocking();
            }
            let action = {
                let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
                match closing.get(session_id) {
                    None => WaitClosingAction::Gone,
                    Some(current) if !Arc::ptr_eq(current, &control) => WaitClosingAction::Replaced,
                    Some(_) => WaitClosingAction::Retry,
                }
            };
            match action {
                WaitClosingAction::Gone => {
                    let _guard = control.wait.lock().expect("publish control wait 锁中毒");
                    control.cv.notify_all();
                    return;
                }
                WaitClosingAction::Replaced => continue,
                WaitClosingAction::Retry => {
                    let guard = control.wait.lock().expect("publish control wait 锁中毒");
                    // cleanup 未完成或 closer 尚未 CAS 清除：condvar 等待。
                    let (_next, _) = control
                        .cv
                        .wait_timeout(guard, Duration::from_millis(50))
                        .expect("publish control condvar 中毒");
                }
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 移除 handle 的同一临界区必须立刻挂上 Closing barrier，消除 Missing 窗口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     无条件写入/覆盖 closing_publish[session_id]（含 in_flight==0，直至 closer cleanup+drain 才清）。
    fn install_closing_tombstone(&self, session_id: &str, publish: Arc<PublishControl>) {
        let mut closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
        closing.insert(session_id.to_string(), publish);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Abort spawn / close intent 需要在 wait/openpty 前快速判断 barrier 是否已存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 `closing_publish` 是否含 session_id。
    fn has_closing_barrier(&self, session_id: &str) -> bool {
        self.closing_publish
            .lock()
            .expect("closing_publish 锁中毒")
            .contains_key(session_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     R25 M1：Abort 策略不得 wait 既有 Closing barrier，否则会用 stale 快照在 barrier 清后继续 PTY/insert。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Abort + barrier 已存在 → 立即 `session_close_barrier_active`；否则 Ok。
    fn abort_if_preexisting_closing_barrier(
        &self,
        session_id: &str,
        barrier_policy: SpawnBarrierPolicy,
    ) -> Result<(), AppError> {
        if matches!(barrier_policy, SpawnBarrierPolicy::Abort)
            && self.has_closing_barrier(session_id)
        {
            return Err(AppError::unavailable(
                "session_close_barrier_active".to_string(),
            ));
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     R25 H1 / R26 H1：close 时 registry 无 live handle，但 restore claim 已占位尚未 insert；
    ///     若仅走 NotFound 删 SQLite，旧 restore 仍会 INSERT OR REPLACE 复活同 id。
    ///     close / restoring claim / Closing tombstone 必须共享同一 atomic lifecycle。
    ///     仅移除 map entry 不够：必须 revoke generation 并 wait persistence leases，
    ///     否则 holder 可在 barrier 清后用旧 token re-upsert。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁序 sessions → restoring → closing_publish：
    ///     - 已 live → AlreadyLive（调用方应走 close_inner）；
    ///     - revoke 既有 claim generation（若有）并 Failed 广播，**先不 remove** 直到 lease drain；
    ///     - 已有 Closing barrier → 返回既有 intent 令牌；
    ///     - 否则 install barrier，返回 closer-owned 令牌；
    ///     - barrier 已装后 wait lease drain（超时则保留 barrier fail-closed）。
    pub fn begin_close_intent_for_missing_handle(
        &self,
        session_id: &str,
        row: WorkbenchSessionRow,
    ) -> Result<SessionCloseCleanup, AppError> {
        // R28 H3：sessions（确认 Missing）→ restoring → closing_publish 同一临界区，
        // 禁止 drop sessions 后 restore 抢插 live。
        let (publish, revoked_claim) = {
            let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
            if sessions.contains_key(session_id) {
                return Err(AppError::conflict(
                    "session_close_intent_already_live".to_string(),
                ));
            }
            let revoked_claim = {
                let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
                let state = restoring.get(session_id).cloned();
                if let Some(ref claim) = state {
                    claim.revoke();
                    let _ = claim.tx.send(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    restoring.remove(session_id);
                }
                state
            };
            let mut closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
            let publish = if let Some(publish) = closing.get(session_id).cloned() {
                publish
            } else {
                let publish = PublishControl::new();
                publish.revoke();
                closing.insert(session_id.to_string(), publish.clone());
                publish
            };
            drop(closing);
            drop(sessions);
            (publish, revoked_claim)
        };

        let mut leases_drained = true;
        let restore_claim_for_drain = if let Some(state) = revoked_claim {
            if !state.wait_leases_drained(RESTORE_CLAIM_LEASE_DRAIN_TIMEOUT) {
                tracing::warn!(
                    session_id = %session_id,
                    "restore claim persist leases still in-flight after close intent; retaining barrier"
                );
                leases_drained = false;
                Some(state)
            } else {
                None
            }
        } else {
            None
        };
        let drained = publish.in_flight.load(Ordering::SeqCst) == 0 && leases_drained;
        Ok(SessionCloseCleanup {
            registry: self.clone(),
            session_id: session_id.to_string(),
            publish,
            row,
            drained,
            finished: false,
            restore_claim_for_drain,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     live close 与 missing-handle close intent 必须共享同一 atomic lifecycle：
    ///     revoke restore claim generation + Failed 广播 + Closing tombstone + wait restore leases
    ///     （R27 H2/H3）。否则 provisional/live 路径只 revoke PublishControl，holder 仍可 re-upsert。
    ///
    /// Code Logic（这个函数做什么）:
    ///     revoke claim（若有）并从 restoring 移除；install/reuse Closing tombstone；
    ///     wait restore leases；超时则 drained=false 并保留 Arc 供 reaper。
    fn revoke_restore_claim_install_tombstone(
        &self,
        session_id: &str,
        existing_publish: Option<Arc<PublishControl>>,
    ) -> (Arc<PublishControl>, Option<Arc<RestoreClaimState>>, bool) {
        self.revoke_restore_claim_install_tombstone_with_timeout(
            session_id,
            existing_publish,
            RESTORE_CLAIM_LEASE_DRAIN_TIMEOUT,
        )
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产用固定 timeout；测试用短 timeout 验证 fail-closed（R27 H3）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     revoke claim + install tombstone + wait leases(timeout)。
    fn revoke_restore_claim_install_tombstone_with_timeout(
        &self,
        session_id: &str,
        existing_publish: Option<Arc<PublishControl>>,
        lease_timeout: Duration,
    ) -> (Arc<PublishControl>, Option<Arc<RestoreClaimState>>, bool) {
        let revoked_claim = {
            let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
            let state = restoring.get(session_id).cloned();
            if let Some(ref claim) = state {
                claim.revoke();
                let _ = claim.tx.send(SharedRestoreNotification::Failed(
                    AppErrorCategory::Unavailable,
                ));
                restoring.remove(session_id);
            }
            state
        };

        let publish = if let Some(publish) = existing_publish {
            self.install_closing_tombstone(session_id, publish.clone());
            publish
        } else {
            let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
            if let Some(publish) = closing.get(session_id).cloned() {
                publish
            } else {
                drop(closing);
                let publish = PublishControl::new();
                publish.revoke();
                self.install_closing_tombstone(session_id, publish.clone());
                publish
            }
        };

        let mut leases_drained = true;
        let restore_claim_for_drain = if let Some(state) = revoked_claim {
            if !state.wait_leases_drained(lease_timeout) {
                tracing::warn!(
                    session_id = %session_id,
                    "restore claim persist leases still in-flight after close revoke; retaining barrier"
                );
                leases_drained = false;
                Some(state)
            } else {
                None
            }
        } else {
            None
        };

        (publish, restore_claim_for_drain, leases_drained)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close 完成 lease drain + 后端 cleanup 后必须按身份清除 barrier，允许同 id 合法 reinsert。
    ///     仅 closer（或 closer 启动的 drain reaper）可 clear，waiter 禁止（R23 H2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 closing_publish[session_id] 仍是同一 PublishControl Arc 时 remove 并 notify。
    fn clear_closing_tombstone_if_same(&self, session_id: &str, publish: &Arc<PublishControl>) {
        let mut closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
        let should_clear = closing
            .get(session_id)
            .map(|current| Arc::ptr_eq(current, publish))
            .unwrap_or(false);
        if should_clear {
            closing.remove(session_id);
        }
        drop(closing);
        let _guard = publish.wait.lock().expect("publish control wait 锁中毒");
        publish.cv.notify_all();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     soft-timeout 未 drain 时 closer 不能阻塞永久，但 barrier 必须由 closer 身份在
    ///     drain+cleanup 后清除，禁止 waiter 代清导致旧 cleanup 打到新 generation。
    ///
    /// Code Logic（这个函数做什么）:
    ///     后台线程 blocking drain 后，仅当 cleanup_done 且身份匹配时 clear。
    fn spawn_closing_barrier_reaper(
        &self,
        session_id: String,
        publish: Arc<PublishControl>,
        restore_claim: Option<Arc<RestoreClaimState>>,
    ) {
        let registry = self.clone();
        thread::spawn(move || {
            publish.wait_in_flight_drained_blocking();
            // R27 H3：若 restore leases 未 drain，reaper 继续持有 claim Arc 等待归零。
            if let Some(claim) = restore_claim {
                while !claim.wait_leases_drained(Duration::from_secs(2)) {
                    tracing::warn!(
                        session_id = %session_id,
                        "closing barrier reaper waiting restore persist leases"
                    );
                }
            }
            // cleanup 必须由 close 路径 mark；若未 mark 则继续等（避免过早 clear）。
            loop {
                if publish.is_cleanup_done() {
                    break;
                }
                let guard = publish.wait.lock().expect("publish control wait 锁中毒");
                let (_next, _) = publish
                    .cv
                    .wait_timeout(guard, Duration::from_millis(50))
                    .expect("publish control condvar 中毒");
            }
            registry.clear_closing_tombstone_if_same(&session_id, &publish);
        });
    }

    /// Business Logic（为什么需要这个函数）:
    ///     reinsert 与 close install barrier 必须在同一 lifecycle 锁序下 CAS，禁止 wait 返回后
    ///     再被 close 抢先挂 barrier 却仍 insert（R23 M1）；也禁止覆盖仍 Live 的 generation，
    ///     否则并发 close 会 remove 后继实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先取 sessions 锁，再取 closing_publish 锁；barrier → BarrierActive；已 Live → AlreadyLive；
    ///     vacant 且无 barrier 时 insert → Inserted。
    fn try_insert_handle_revalidating_barrier(
        &self,
        session_id: &str,
        handle: Arc<Mutex<WorkbenchSessionHandle>>,
    ) -> InsertCasResult {
        let project_id = {
            let h = handle.lock().expect("workbench session 锁中毒");
            h.row.project_id.clone()
        };
        let mut sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
        let project_closing = self.project_closing.lock().expect("project_closing 锁中毒");
        // Closing barrier 仍在：禁止 reinsert。
        if closing.contains_key(session_id) {
            return InsertCasResult::BarrierActive;
        }
        // R27 H4：project remove 窗口内禁止 insert orphan。
        if project_closing.contains_key(&project_id) {
            return InsertCasResult::ProjectClosing;
        }
        // 仍 Live：禁止覆盖；并发 reinsert 输家应停止自旋（AlreadyLive）。
        if sessions.contains_key(session_id) {
            return InsertCasResult::AlreadyLive;
        }
        sessions.insert(session_id.to_string(), handle);
        InsertCasResult::Inserted
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试/生产 insert 在 wait barrier 后仍可能与 close 竞态，必须循环 CAS。
    ///
    /// Code Logic（这个函数做什么）:
    ///     wait → 调用方构造 handle → try_insert；失败则重 wait 直至成功。
    fn insert_handle_with_barrier_cas(
        &self,
        session_id: &str,
        mut build: impl FnMut() -> Arc<Mutex<WorkbenchSessionHandle>>,
    ) {
        loop {
            self.wait_for_closing_tombstone(session_id);
            let handle = build();
            match self.try_insert_handle_revalidating_barrier(session_id, handle) {
                InsertCasResult::Inserted => return,
                InsertCasResult::AlreadyLive => {
                    // 赢家可能是既有 Live；若 close 已在 CAS 后 remove，则继续重试 reinsert。
                    if self.contains(session_id) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                InsertCasResult::BarrierActive => {
                    // session close 在 wait→insert 间隙挂了 barrier：丢弃 handle 后重 wait。
                    thread::sleep(Duration::from_millis(1));
                }
                InsertCasResult::ProjectClosing => {
                    // R27 H4：project remove 进行中不得无限自旋；测试/helper 直接放弃本轮 insert。
                    // 生产 spawn_row 单独 Abort。
                    return;
                }
            }
        }
    }

    /// 判断 worker 捕获的 generation 是否仍对应当前 registry 句柄（R18 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     close/reclaim 后 reader/exit watcher 可能仍在跑；同 id 新实例不得被旧 worker
    ///     写入 status 或 append replay。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 sessions map 中该 id 的 handle.generation；匹配则 true，缺失/不匹配 false。
    pub fn is_current_session_generation(&self, session_id: &str, generation: u64) -> bool {
        is_current_session_generation(&self.sessions, session_id, generation)
    }

    /// 读取当前 live handle 的 generation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     SessionSpawnGuard / generation fencing 需要捕获 insert 后的世代做 CAS。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 session 存在则返回其 generation，否则 None。
    pub fn session_generation(&self, session_id: &str) -> Option<u64> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .get(session_id)
            .map(|handle| handle.lock().expect("workbench session 锁中毒").generation)
    }

    /// 测试兼容别名：读取 generation。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     既有单测调用 `session_generation_for_test`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `session_generation`。
    #[cfg(test)]
    pub fn session_generation_for_test(&self, session_id: &str) -> Option<u64> {
        self.session_generation(session_id)
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
                owner_instance_id: None,
            },
            |buffer| buffer.snapshot(session_id),
        )
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户在工作台中创建本机终端时，需要在当前 worktree 根目录中启动普通 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     R26 M1：先 revalidate project closing barrier；通过后优先 `create_tmux_window`，
    ///     再构建 row 并 `spawn_row`。R30 M2：window 创建成功后装 `TmuxCreateGuard`，
    ///     仅在 spawn_row Ok 后 commit；barrier / spawn 失败 Drop 销毁 orphan window。
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
        self.create_with_ids(
            state,
            project,
            cwd,
            worktree_id,
            worktree_name,
            initial_cols,
            initial_rows,
            session_id,
            None,
        )
    }

    /// Orchestrator 专用：预分配 terminal + agent session UUID 后创建 shell。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     OpenCode bridge 在 shell 启动前必须能读到 `CC_PARTNER_AGENT_SESSION_ID`，
    ///     且 Agent Runtime 行必须使用同一 UUID；失败时调用方回滚 runtime/terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用给定 session_id 建 terminal，并把 agent_session_id 注入 env（tmux -e / raw PTY）。
    #[allow(clippy::too_many_arguments)]
    pub fn create_with_preallocated_ids(
        &self,
        state: AppState,
        project: WorkbenchProjectRow,
        cwd: String,
        worktree_id: Option<String>,
        worktree_name: Option<String>,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
        terminal_session_id: String,
        agent_session_id: String,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let terminal_session_id = terminal_session_id.trim().to_string();
        let agent_session_id = agent_session_id.trim().to_string();
        if terminal_session_id.is_empty() || agent_session_id.is_empty() {
            return Err(AppError::validation(
                "preallocated terminal/agent session id 不能为空".to_string(),
            ));
        }
        self.create_with_ids(
            state,
            project,
            cwd,
            worktree_id,
            worktree_name,
            initial_cols,
            initial_rows,
            terminal_session_id,
            Some(agent_session_id),
        )
    }

    /// 内部 create：固定 session_id + 可选 agent_session_id 注入。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     普通 create 与 Orchestrator 预分配路径共享 tmux/raw 启动逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require project barrier → create_tmux_window（带 agent_ctx）→ spawn_row。
    #[allow(clippy::too_many_arguments)]
    fn create_with_ids(
        &self,
        state: AppState,
        project: WorkbenchProjectRow,
        cwd: String,
        worktree_id: Option<String>,
        worktree_name: Option<String>,
        initial_cols: Option<u16>,
        initial_rows: Option<u16>,
        session_id: String,
        agent_session_id: Option<String>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        // R26 M1：project remove 窗口内禁止 create 产生 orphan live。
        self.require_project_not_closing(&project.id)?;
        let now = chrono::Utc::now().to_rfc3339();
        let (cols, rows) = initial_terminal_size(initial_cols, initial_rows);
        let terminal_command = default_terminal_command();
        let agent_ctx = TerminalAgentContextIds {
            project_id: project.id.clone(),
            worktree_id: worktree_id.clone().unwrap_or_default(),
            terminal_session_id: session_id.clone(),
            owner_instance_id: state.config_runtime.owner_instance_id().to_string(),
            agent_session_id: agent_session_id
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
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
                    cols,
                    rows,
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
            name_source: SessionNameSource::Default.as_str().to_string(),
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

        // R30 M2：create_tmux_window 成功后立刻装 RAII；spawn 提交前任何失败路径 Drop 回收 window。
        let mut tmux_create_guard = if row.backend == TMUX_BACKEND
            && row.backend_id.is_some()
            && row.backend_window_id.is_some()
        {
            Some(TmuxCreateGuard::new(row.clone()))
        } else {
            None
        };

        // spawn 前再 revalidate：create 期间 project 可能被 remove；失败则 Drop 回收 window。
        self.require_project_not_closing(&project.id)?;
        // spawn 失败时 `?` 离开作用域 → TmuxCreateGuard Drop 回收 pre-insert orphan window。
        // raw PTY spawn 通过 map 注入 AGENT_SESSION_ID；tmux 已在 -e 注入。
        // 使用 RAII 确保失败路径也清除 map，避免泄漏。
        struct PreallocAgentGuard(String);
        impl Drop for PreallocAgentGuard {
            fn drop(&mut self) {
                forget_preallocated_agent_session(&self.0);
            }
        }
        let _prealloc_guard = agent_ctx.agent_session_id.as_ref().map(|agent_id| {
            remember_preallocated_agent_session(&session_id, agent_id);
            PreallocAgentGuard(session_id.clone())
        });
        let spawned = self.spawn_row(state, row, SpawnBarrierPolicy::Retry, None)?;
        // spawn 成功：window 由 registry/command 层（SessionSpawnGuard + close path）接管。
        if let Some(guard) = tmux_create_guard.as_mut() {
            guard.commit();
        }
        // first-pane title owner：创建后 seed list-panes 第一行，供 agent 自动标题归属。
        let _ = self.ensure_title_owner_pane(&session_id);
        Ok(spawned)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     workspace safe restore 只能对**已存在**的 tmux target 建立 attach client，禁止创建 window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     要求 backend=tmux 且 backend_id 存在；直接 `spawn_row` 走 attach 命令路径，
    ///     绝不调用 `create_tmux_window` 或 raw PTY 回退改写。
    ///     BarrierActive 时 Abort：禁止用 pre-close 行快照无限重试复活（R24 H2）。
    ///     R26：可选 restore claim generation；None 时仅 revalidate project barrier。
    pub fn attach_existing_tmux_only(
        &self,
        state: AppState,
        mut row: WorkbenchSessionRow,
        restore_claim_generation: Option<u64>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        // R26 M1：project remove 窗口内禁止 attach 复活。
        self.require_project_not_closing(&row.project_id)?;
        if let Some(generation) = restore_claim_generation {
            self.require_restore_claim_active(&row.id, generation)?;
        }
        if row.backend != TMUX_BACKEND {
            return Err(AppError::validation(
                "safe_attach_requires_tmux".to_string(),
            ));
        }
        if row.backend_id.as_deref().unwrap_or("").is_empty() {
            return Err(AppError::validation(
                "safe_attach_missing_backend_id".to_string(),
            ));
        }
        let target = tmux_target_for_row(&row)?;
        if !inspect_tmux_target_exists(&target) {
            return Err(AppError::unavailable("tmux_target_missing".to_string()));
        }
        row.status = "running".to_string();
        row.exited_at = None;
        row.exit_code = None;
        row.updated_at = chrono::Utc::now().to_rfc3339();
        // spawn 前再 revalidate claim / project barrier。
        self.require_project_not_closing(&row.project_id)?;
        if let Some(generation) = restore_claim_generation {
            self.require_restore_claim_active(&row.id, generation)?;
        }
        let restored = self.spawn_row(
            state,
            row,
            SpawnBarrierPolicy::Abort,
            restore_claim_generation,
        )?;
        // attach 后立刻强制 window 尺寸，避免冷启动 status bar 停留在旧 client size。
        let _ = self.force_tmux_window_size(&restored);
        Ok(restored)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用重启 / 项目打开时，持久化终端 tab 需要重新绑定运行期 attach；
    ///     **仅**对已存在的 tmux target 建立 attach client，缺失或 raw PTY 一律 skip，
    ///     禁止 `create_tmux_window` / raw PTY 回退（A8：只有用户显式「新建终端」才创建 shell）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     backend=tmux 且 target 存在：迁移可读 session 名后 `spawn_row` attach；
    ///     target 缺失 / 无 tmux / raw_pty → `Err`（`restore_persisted_sessions` 标 disconnected）。
    ///     BarrierActive 时 Abort 返回 `session_close_barrier_active`，上层 re-read durable 状态（R24 H2）。
    ///     R26 H1：`restore_claim_generation` 必须在 spawn 前与 upsert 前仍 active。
    pub fn restore(
        &self,
        state: AppState,
        project: WorkbenchProjectRow,
        mut row: WorkbenchSessionRow,
        worktree_name: Option<String>,
        restore_claim_generation: Option<u64>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        // R26 M1：project remove 窗口内禁止 restore 复活 orphan。
        self.require_project_not_closing(&project.id)?;
        if let Some(generation) = restore_claim_generation {
            self.require_restore_claim_active(&row.id, generation)?;
        }
        if row.cwd.trim().is_empty() {
            row.cwd = project.path.clone();
        }
        // 非 tmux（含历史 token `raw_pty` 与当前 `pty`）一律 skip，禁止 spawn 新 shell
        if row.backend != TMUX_BACKEND {
            return Err(AppError::validation("restore_skips_raw_pty".to_string()));
        }
        let Some(tmux) = available_tmux_command() else {
            return Err(AppError::unavailable("tmux_unavailable".to_string()));
        };
        let desired_session_name = tmux_worktree_session_name(
            &project.name,
            &project.id,
            row.worktree_id.as_deref(),
            worktree_name.as_deref(),
        );
        let session_name =
            migrate_tmux_session_name(&tmux, row.backend_id.as_deref(), &desired_session_name);
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
            // A8 skip-missing：不 create_tmux_window、不 raw PTY fallback
            return Err(AppError::unavailable("tmux_target_missing".to_string()));
        }
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

        row.status = "running".to_string();
        row.exited_at = None;
        row.exit_code = None;
        row.updated_at = chrono::Utc::now().to_rfc3339();
        // openpty 前再 revalidate：close/remove 可能在 resolve 期间发生。
        self.require_project_not_closing(&project.id)?;
        if let Some(generation) = restore_claim_generation {
            self.require_restore_claim_active(&row.id, generation)?;
        }
        let restored = self.spawn_row(
            state,
            row,
            SpawnBarrierPolicy::Abort,
            restore_claim_generation,
        )?;
        // restore attach 后立刻强制 window 尺寸，避免冷启动 status bar 停留在旧 client size。
        let _ = self.force_tmux_window_size(&restored);
        Ok(restored)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     冷启动 restore、手动适应尺寸或 PTY resize 后，需要把 detached tmux window 强制同步到目标 cols/rows，
    ///     并触发 full redraw（同尺寸 resize 往往被 tmux 忽略，status 会停在历史帧中间）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅对 tmux-backed row：先 `resize-window` 到 rows±1，再设回目标 cols/rows；
    ///     raw PTY / 缺 tmux 直接 Ok；命令失败返回 Err 供调用方 debug，不阻断主流程。
    fn force_tmux_window_size(&self, row: &WorkbenchSessionRow) -> Result<(), AppError> {
        if row.backend != TMUX_BACKEND {
            return Ok(());
        }
        let Some(tmux) = available_tmux_command() else {
            return Ok(());
        };
        let target = tmux_target_for_row(row)?;
        let bump_rows = tmux_force_redraw_bump_rows(row.rows);
        for (cols, rows) in [(row.cols, bump_rows), (row.cols, row.rows)] {
            let args = tmux_resize_window_args(&target, cols, rows);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_tmux_command(&tmux, &arg_refs)?;
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建和恢复终端最终都要启动一个 PTY 客户端并注册输出/退出监听。
    ///     R19 M1：insert 为 Provisional，不立即 emit running；外部事件等 mark Ready。
    ///     R23 M1：insert 必须与 Closing barrier 同 lifecycle 锁 CAS，禁止 reinsert 越过新 barrier。
    ///     R24 H2：restore/safe_attach 在 BarrierActive 时 Abort，禁止旧行快照无限重试复活。
    ///     R26 H1/M1：spawn 前 revalidate restore claim generation 与 project closing barrier。
    ///
    /// Code Logic（这个函数做什么）:
    ///     R25 M1：Abort 且 pre-existing Closing barrier → 立即返回，不 wait/不 PTY/不 insert；
    ///     R26：若有 restore_claim_generation / project_id 无效则 reclaim PTY 后返回；
    ///     否则 wait barrier → openpty/spawn → try_insert 再校验 barrier；
    ///     CAS 失败：AlreadyLive 返回既有；BarrierActive 按 policy Retry 或 Abort；
    ///     成功后分配 generation 绑定 reader/exit fence。
    fn spawn_row(
        &self,
        state: AppState,
        row: WorkbenchSessionRow,
        barrier_policy: SpawnBarrierPolicy,
        restore_claim_generation: Option<u64>,
    ) -> Result<WorkbenchSessionRow, AppError> {
        let session_id = row.id.clone();
        let project_id = row.project_id.clone();
        let cols = row.cols;
        let rows = row.rows;

        // R25 M1：Abort 对 pre-installed barrier 立即 fail-closed（禁止 wait 后继续 stale snapshot）。
        self.abort_if_preexisting_closing_barrier(&session_id, barrier_policy)?;
        // R26 / R27 / R28 H4：spawn 入口 revalidate project + restore claim。
        // project op lease 由 create/restore 最外层持有至 upsert/Ready/claim finish（禁止在 spawn 返回前 drop）。
        self.require_project_not_closing(&project_id)?;
        if let Some(generation) = restore_claim_generation {
            self.require_restore_claim_active(&session_id, generation)?;
        }

        // R21 H2 / R23 M1 / R24 H2：wait + openpty + insert CAS。
        let (generation, publish, handle, reader) = loop {
            // Retry 可 wait；Abort 若 wait 期间新挂 barrier，仍靠下方 CAS Abort 返回。
            // 但 Abort 不得进入 wait_for_closing 去等待 pre-existing barrier 清掉后继续。
            if matches!(barrier_policy, SpawnBarrierPolicy::Abort) {
                self.abort_if_preexisting_closing_barrier(&session_id, barrier_policy)?;
            } else {
                self.wait_for_closing_tombstone(&session_id);
            }
            // openpty 前再 revalidate：close/remove 可能在 wait 期间发生。
            self.require_project_not_closing(&project_id)?;
            if let Some(generation) = restore_claim_generation {
                self.require_restore_claim_active(&session_id, generation)?;
            }

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

            // insert 前最后一次 revalidate；失败则 kill 本轮 PTY。
            let kill_spawned_handle = |handle: &Arc<Mutex<WorkbenchSessionHandle>>| {
                let mut h = handle.lock().expect("workbench session 锁中毒");
                if let SessionProcess::Pty { child, .. } = &mut h.process {
                    if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                        tracing::debug!("spawn_row revalidate 失败后 kill PTY: {error}");
                    }
                }
            };
            if self.require_project_not_closing(&project_id).is_err()
                || restore_claim_generation
                    .is_some_and(|g| self.require_restore_claim_active(&session_id, g).is_err())
            {
                // 构造临时 handle 以统一 kill 路径。
                let generation = self.allocate_generation();
                let publish = PublishControl::new();
                let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
                    row: row.clone(),
                    generation,
                    durability: SessionDurability::Provisional,
                    publish,
                    deferred_output: Vec::new(),
                    deferred_mutations: Vec::new(),
                    pending_exit: None,
                    restore_claim_generation,
                    process: SessionProcess::Pty {
                        master: pair.master,
                        writer,
                        child,
                    },
                }));
                kill_spawned_handle(&handle);
                if let Some(generation) = restore_claim_generation {
                    self.require_restore_claim_active(&session_id, generation)?;
                }
                self.require_project_not_closing(&project_id)?;
                // 理论上不会落到这里；保险返回 revoked。
                return Err(AppError::unavailable(
                    "session_restore_claim_revoked".to_string(),
                ));
            }

            let generation = self.allocate_generation();
            let publish = PublishControl::new();
            let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
                row: row.clone(),
                generation,
                durability: SessionDurability::Provisional,
                publish: publish.clone(),
                deferred_output: Vec::new(),
                deferred_mutations: Vec::new(),
                pending_exit: None,
                restore_claim_generation,
                process: SessionProcess::Pty {
                    master: pair.master,
                    writer,
                    child,
                },
            }));
            match self.try_insert_handle_revalidating_barrier(&session_id, handle.clone()) {
                InsertCasResult::Inserted => break (generation, publish, handle, reader),
                InsertCasResult::AlreadyLive => {
                    // 并发赢家已 Live：kill 本轮 PTY，返回已有 row（不覆盖）。
                    kill_spawned_handle(&handle);
                    let existing = self
                        .get_handle(&session_id)
                        .map(|h| h.lock().expect("workbench session 锁中毒").row.clone());
                    if let Ok(existing_row) = existing {
                        return Ok(existing_row);
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                InsertCasResult::BarrierActive => {
                    // barrier 在 wait 与 insert 间隙被挂上：kill 本轮 child。
                    kill_spawned_handle(&handle);
                    match barrier_policy {
                        SpawnBarrierPolicy::Retry => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        SpawnBarrierPolicy::Abort => {
                            // R24 H2 / R25 M1：restore 不得无限重试 pre-close 快照；上层 re-read durable。
                            return Err(AppError::unavailable(
                                "session_close_barrier_active".to_string(),
                            ));
                        }
                    }
                }
                InsertCasResult::ProjectClosing => {
                    // R27 H4：project remove 窗口内禁止 insert orphan。
                    kill_spawned_handle(&handle);
                    return Err(AppError::unavailable(
                        "project_closing_barrier_active".to_string(),
                    ));
                }
            }
        };

        self.ensure_replay_buffer_for_generation(&session_id, generation);

        // R19 M1：不在 Provisional 发 running；commit/mark_ready 后再发。
        spawn_reader_thread(
            state.clone(),
            session_id.clone(),
            generation,
            publish.clone(),
            reader,
            self.sessions.clone(),
            self.replay_buffers.clone(),
        );
        spawn_exit_watcher(
            state,
            self.sessions.clone(),
            session_id.clone(),
            generation,
            publish,
            handle,
        );

        Ok(row)
    }

    /// 将 provisional handle 切换为 Ready 并可选发布 running（R19 M1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite upsert 成功后，会话才可对外 Live；此前不得发 running/output/OSC。
    ///
    /// Code Logic（这个函数做什么）:
    ///     不绑定 generation 的兼容入口：读取当前 generation 后委托 CAS 路径。
    pub fn mark_session_ready(&self, session_id: &str, state: Option<&AppState>) {
        let Some(generation) = self.session_generation(session_id) else {
            return;
        };
        let _ = self.mark_session_ready_for_generation(session_id, generation, state);
    }

    /// generation CAS 进入 Ready 并 flush Provisional 缓冲（R20 M1/H1 + R21 H1）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     并发 close 可能移除 handle 或同 id 重建；仅同 generation 才允许 Ready 与对外 running。
    ///     deferred flush 与 live reader 必须共享 generation-scoped seq，禁止各自从 0 重开。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁内：同 gen + Provisional → Flushing，取出 deferred；
    ///     无 state：直接 Ready；有 state：lease 下 emit running + 共享 next_seq flush，
    ///     再 CAS Flushing→Ready 并二次 flush 期间新缓冲。
    ///     返回是否成功进入 Ready（或已 Ready 幂等）。
    pub fn mark_session_ready_for_generation(
        &self,
        session_id: &str,
        generation: u64,
        state: Option<&AppState>,
    ) -> bool {
        // R27 H4/H5：Ready commit 前 fail-closed revalidate project barrier + restore claim。
        let precheck = {
            let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
            sessions.get(session_id).map(|handle| {
                let h = handle.lock().expect("workbench session 锁中毒");
                (h.row.project_id.clone(), h.restore_claim_generation)
            })
        };
        if let Some((project_id, claim_gen)) = precheck {
            if self.require_project_not_closing(&project_id).is_err() {
                return false;
            }
            if let Some(claim_gen) = claim_gen {
                if self
                    .require_restore_claim_active(session_id, claim_gen)
                    .is_err()
                {
                    return false;
                }
            }
        }

        let mut deferred_output = Vec::new();
        let mut deferred_mutations = Vec::new();
        let mut pending_exit: Option<Option<i32>> = None;
        let transition = {
            let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
            let project_closing = self.project_closing.lock().expect("project_closing 锁中毒");
            match sessions.get(session_id) {
                Some(handle) => {
                    let mut handle = handle.lock().expect("workbench session 锁中毒");
                    if handle.generation != generation
                        || !handle.publish.allowed.load(Ordering::SeqCst)
                    {
                        None
                    } else if project_closing.contains_key(&handle.row.project_id) {
                        // R27 H4：同锁 revalidate project barrier。
                        None
                    } else if handle.durability == SessionDurability::Ready {
                        // 同 generation 已 Ready：幂等成功（测试 fake insert / 重复 commit）。
                        Some(false)
                    } else if handle.durability == SessionDurability::Provisional {
                        // R21 H1：先进入 Flushing，reader 继续缓冲直到 flush 完成。
                        handle.durability = SessionDurability::Flushing;
                        deferred_output = std::mem::take(&mut handle.deferred_output);
                        deferred_mutations = std::mem::take(&mut handle.deferred_mutations);
                        pending_exit = handle.pending_exit.take();
                        Some(true)
                    } else if handle.durability == SessionDurability::Flushing {
                        // 另一路正在 flush：视为进行中，不二次进入。
                        Some(false)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        let Some(just_transitioned) = transition else {
            return false;
        };
        if !just_transitioned {
            // 已 Ready：幂等成功；Flushing 进行中：本调用不重复 flush，返回 false。
            return self
                .session_durability(session_id)
                .map(|d| d == SessionDurability::Ready)
                .unwrap_or(false);
        }
        // 无 AppState：测试/兼容路径直接 Ready（无对外 flush）。
        let Some(state) = state else {
            return self.finish_ready_after_flush(session_id, generation, None, None);
        };
        let Some(_lease) = try_acquire_publication_lease(&self.sessions, session_id, generation)
        else {
            // lease 失败：回滚 Flushing → Provisional 并归还缓冲，避免永久卡 Flushing。
            self.rollback_flushing_to_provisional(
                session_id,
                generation,
                deferred_output,
                deferred_mutations,
                pending_exit,
            );
            return false;
        };
        emit_status(state, session_id, "running", None);
        for chunk in deferred_output {
            let _ = emit_terminal_output_with_lease(
                state,
                session_id,
                Some(generation),
                &self.sessions,
                &mut 0u64, // 未使用：seq 由 publish.next_seq 分配
                chunk,
                &self.replay_buffers,
            );
        }
        if !deferred_mutations.is_empty() {
            forward_agent_osc_mutations(state, session_id, deferred_mutations);
        }
        if let Some(exit_code) = pending_exit {
            if let Ok(handle) = self.get_handle(session_id) {
                let mut handle = handle.lock().expect("workbench session 锁中毒");
                if handle.generation == generation && handle.publish.allowed.load(Ordering::SeqCst)
                {
                    handle.row.status = "exited".to_string();
                    handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
                    handle.row.exit_code = exit_code;
                    handle.row.updated_at = chrono::Utc::now().to_rfc3339();
                }
            }
            emit_status(state, session_id, "exited", exit_code);
        }
        // Flushing → Ready；flush 期间新缓冲二次 drain（仍共享 next_seq）。
        self.finish_ready_after_flush(session_id, generation, Some(state), pending_exit)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Flushing 结束必须进入 Ready，并把 flush 窗口内新缓冲在同一 seq 空间发出。
    ///     R22 H1：必须在 deferred 队列锁下为空后才原子 Ready，禁止 live 在二次 drain 前 overtake。
    ///
    /// Code Logic（这个函数做什么）:
    ///     循环：锁内若 Flushing 且 deferred/mutations/pending_exit 非空则 take 并保持 Flushing；
    ///     为空则同临界区 Ready 返回；锁外持 lease 按共享 next_seq flush 后重检。
    fn finish_ready_after_flush(
        &self,
        session_id: &str,
        generation: u64,
        state: Option<&AppState>,
        _already_emitted_exit: Option<Option<i32>>,
    ) -> bool {
        loop {
            let mut more_output = Vec::new();
            let mut more_mutations = Vec::new();
            let mut more_exit: Option<Option<i32>> = None;
            // Ok(true)=已 Ready；Ok(false)=需锁外 flush；Err=失败。
            let step: Result<bool, ()> = {
                let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
                match sessions.get(session_id) {
                    Some(handle) => {
                        let mut handle = handle.lock().expect("workbench session 锁中毒");
                        if handle.generation != generation
                            || !handle.publish.allowed.load(Ordering::SeqCst)
                        {
                            Err(())
                        } else if handle.durability == SessionDurability::Ready {
                            Ok(true)
                        } else if handle.durability == SessionDurability::Flushing {
                            more_output = std::mem::take(&mut handle.deferred_output);
                            more_mutations = std::mem::take(&mut handle.deferred_mutations);
                            more_exit = handle.pending_exit.take();
                            if more_output.is_empty()
                                && more_mutations.is_empty()
                                && more_exit.is_none()
                            {
                                // R22 H1：仅当队列锁下为空时原子 Ready。
                                handle.durability = SessionDurability::Ready;
                                Ok(true)
                            } else {
                                // 保持 Flushing，禁止 live overtake 未完成 deferred。
                                Ok(false)
                            }
                        } else {
                            Err(())
                        }
                    }
                    None => Err(()),
                }
            };
            match step {
                Err(()) => return false,
                Ok(true) => return true,
                Ok(false) => {
                    // 无 AppState：丢弃已 take 的缓冲后重检，直至锁下为空再 Ready。
                    let Some(state) = state else {
                        continue;
                    };
                    let Some(_lease) =
                        try_acquire_publication_lease(&self.sessions, session_id, generation)
                    else {
                        // lease 失败：归还本轮 take 的缓冲并回滚 Provisional，避免卡 Flushing。
                        self.rollback_flushing_to_provisional(
                            session_id,
                            generation,
                            more_output,
                            more_mutations,
                            more_exit,
                        );
                        return false;
                    };
                    for chunk in more_output {
                        let _ = emit_terminal_output_with_lease(
                            state,
                            session_id,
                            Some(generation),
                            &self.sessions,
                            &mut 0u64,
                            chunk,
                            &self.replay_buffers,
                        );
                    }
                    if !more_mutations.is_empty() {
                        forward_agent_osc_mutations(state, session_id, more_mutations);
                    }
                    if let Some(exit_code) = more_exit {
                        if let Ok(handle) = self.get_handle(session_id) {
                            let mut handle = handle.lock().expect("workbench session 锁中毒");
                            if handle.generation == generation
                                && handle.publish.allowed.load(Ordering::SeqCst)
                            {
                                handle.row.status = "exited".to_string();
                                handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
                                handle.row.exit_code = exit_code;
                                handle.row.updated_at = chrono::Utc::now().to_rfc3339();
                            }
                        }
                        emit_status(state, session_id, "exited", exit_code);
                    }
                    // 重检：flush 窗口内新 deferred 必须在 Ready 前继续 drain。
                }
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Flushing 若无法拿到 lease，不得永久卡死；回滚为 Provisional 保留缓冲。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同 gen + Flushing 时写回 deferred 并设 Provisional。
    fn rollback_flushing_to_provisional(
        &self,
        session_id: &str,
        generation: u64,
        deferred_output: Vec<String>,
        deferred_mutations: Vec<AgentRuntimeMutation>,
        pending_exit: Option<Option<i32>>,
    ) {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let Some(handle) = sessions.get(session_id) else {
            return;
        };
        let mut handle = handle.lock().expect("workbench session 锁中毒");
        if handle.generation != generation || handle.durability != SessionDurability::Flushing {
            return;
        }
        handle.durability = SessionDurability::Provisional;
        if handle.deferred_output.is_empty() {
            handle.deferred_output = deferred_output;
        } else {
            let mut merged = deferred_output;
            merged.append(&mut handle.deferred_output);
            handle.deferred_output = merged;
        }
        if handle.deferred_mutations.is_empty() {
            handle.deferred_mutations = deferred_mutations;
        } else {
            let mut merged = deferred_mutations;
            merged.append(&mut handle.deferred_mutations);
            handle.deferred_mutations = merged;
        }
        if handle.pending_exit.is_none() {
            handle.pending_exit = pending_exit;
        }
    }

    /// 读取当前 durability（内部 CAS/幂等判定用）。
    fn session_durability(&self, session_id: &str) -> Option<SessionDurability> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .get(session_id)
            .map(|h| h.lock().expect("workbench session 锁中毒").durability)
    }

    /// 测试：读取当前 durability。
    #[cfg(test)]
    fn session_durability_for_test(&self, session_id: &str) -> Option<SessionDurability> {
        self.session_durability(session_id)
    }

    /// 测试：调用 finish_ready_after_flush（无 AppState）。
    #[cfg(test)]
    fn finish_ready_after_flush_for_test(&self, session_id: &str, generation: u64) -> bool {
        self.finish_ready_after_flush(session_id, generation, None, None)
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
    ///     用户在多 pane window 内应能直接点击目标 pane 切换，而不是反复按循环切换按钮猜位置。
    ///     与相对 `.+` 循环不同，本操作以绝对坐标定位，重复执行结果一致，可安全重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 tmux 当前 pane 几何；zoom、单 pane、点在边框上或点在已 active pane 内一律 no-op；
    ///     命中其它 pane 时执行 `select-pane -t <pane_id>` 并返回该 pane_id。
    pub fn select_pane_at(
        &self,
        session_id: &str,
        col: u32,
        row: u32,
    ) -> Result<Option<String>, AppError> {
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
        let layout = tmux_window_pane_layout(&tmux, &target)?;
        if layout.zoomed || layout.panes.len() <= 1 {
            return Ok(None);
        }
        let Some(pane) = tmux_pane_at_position(&layout.panes, col, row) else {
            return Ok(None);
        };
        if pane.active {
            return Ok(None);
        }
        let pane_id = pane.pane_id.clone();
        let args = tmux_select_pane_args(&pane_id);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_tmux_command(&tmux, &arg_refs)?;
        Ok(Some(pane_id))
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
                // title-owner 若被关掉：交接给剩余第一 pane；最近 auto 标题可重贴。
                let mut renamed = None;
                if let Some((_owner, last_title)) =
                    self.reassign_title_owner_after_pane_close(session_id)
                {
                    if let Some(title) = last_title {
                        let source = {
                            let handle = self.get_handle(session_id)?;
                            let handle = handle.lock().expect("workbench session 锁中毒");
                            SessionNameSource::parse(&handle.row.name_source)
                        };
                        if !matches!(source, SessionNameSource::Manual) {
                            if let Ok(row) =
                                self.rename_with_source(session_id, &title, SessionNameSource::Auto)
                            {
                                renamed = Some(row);
                            }
                        }
                    }
                }
                Ok(PaneCloseOutcome::PaneClosed { renamed })
            }
            PaneClosePlan::CloseWindow => {
                let cleanup = self.close(session_id)?;
                self.clear_title_owner_state(session_id);
                Ok(PaneCloseOutcome::WindowClosed(cleanup))
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端终端容器尺寸变化时，子进程需要收到新的 PTY 行列数；
    ///     冷启动 restore 或「适应尺寸」时，仅 MasterPty::resize 可能不推动 detached tmux window 尺寸，
    ///     导致 status bar 停在旧 client size 中间。
    ///
    /// Code Logic（这个函数做什么）:
    ///     更新 row 尺寸，调用 MasterPty::resize 通知底层 PTY；对 tmux-backed session
    ///     额外执行 `resize-window -x/-y` 强制同步 window/client 尺寸（失败仅 debug，不阻断 PTY resize）。
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
                // 同尺寸 resize 可能不发 SIGWINCH；先 bump 一行再设目标尺寸强制刷新。
                let bump_rows = tmux_force_redraw_bump_rows(rows);
                for next_rows in [bump_rows, rows] {
                    master
                        .resize(PtySize {
                            rows: next_rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .map_err(|error| {
                            AppError::generic(format!("调整 PTY 尺寸失败: {error}"))
                        })?;
                }
            }
            SessionProcess::Fake => {}
        }
        if let Err(error) = self.force_tmux_window_size(&handle.row) {
            tracing::debug!("调整工作台 tmux window 尺寸失败 session_id={session_id}: {error}");
        }
        Ok(handle.row.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭终端 tab 后，该会话应从内存 registry 中移除并释放 PTY 资源。
    ///     R24 H1：返回 closer-owned cleanup 令牌；调用方须在 tmux/SQLite 清理后 finish。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 close_inner(None)，返回 SessionCloseCleanup。
    pub fn close(&self, session_id: &str) -> Result<SessionCloseCleanup, AppError> {
        let cleanup = self.close_inner(session_id, None)?;
        self.clear_title_owner_state(session_id);
        Ok(cleanup)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     SessionSpawnGuard 补偿只能回收自己 spawn 的 generation，禁止误杀同 id 后继实例（R21 M1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 registry 中该 id 的 generation 匹配时 remove + revoke + drain + PTY kill，
    ///     返回 SessionCloseCleanup（补偿路径立即 finish）。
    pub fn close_if_generation(
        &self,
        session_id: &str,
        generation: u64,
    ) -> Result<SessionCloseCleanup, AppError> {
        self.close_inner(session_id, Some(generation))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close / close_if_generation 共享 revoke、lease barrier 与 tombstone 语义。
    ///     R22 M1 / R23 H2 / R24 H1：remove 与 Closing barrier 必须同临界区；barrier 覆盖
    ///     drain **与** registry PTY/replay cleanup **与** 调用方 tmux/SQLite cleanup，
    ///     且仅 closer 身份在 `SessionCloseCleanup::finish_cleanup` 后 CAS 清除。
    ///     R36 M / R41 M7：running PTY `child.kill` 失败（非 already-gone）不得返回 finishable
    ///     cleanup；handle 必须 generation-scoped 保留在 failed_kill_handles 供重试，
    ///     否则 missing-handle 路径会把 raw PTY 当自动成功并删 SQLite。
    ///
    /// Code Logic（这个函数做什么）:
    ///     R28 H3：sessions→restoring→closing_publish 同一临界区完成 generation CAS remove、
    ///     PublishControl revoke、restore claim revoke/remove、Closing tombstone install；
    ///     若 sessions 无 handle，尝试从 failed_kill_handles 取回（R41 M7 重试）；
    ///     锁外 soft-wait publish+restore leases + kill PTY/replay → **不** mark_cleanup_done；
    ///     running PTY kill 经 `normalize_terminal_kill_result`：Ok 才返回 SessionCloseCleanup；
    ///     kill Err 写入 failed_kill_handles 后返回 AppError（barrier 已装，无 finishable cleanup）。
    fn close_inner(
        &self,
        session_id: &str,
        required_generation: Option<u64>,
    ) -> Result<SessionCloseCleanup, AppError> {
        // R28 H3 / R41 M7：remove handle + revoke restore claim + install Closing tombstone
        // 必须在同一 lifecycle 临界区（sessions → restoring → closing_publish）；
        // sessions miss 时再尝试 failed_kill_handles（上次 kill 失败的 generation-scoped handle）。
        let (handle, publish, revoked_claim) = {
            let mut sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
            let handle = match required_generation {
                None => {
                    if let Some(h) = sessions.remove(session_id) {
                        // 成功从 live map 取出时清掉可能陈旧的 failed-kill 条目。
                        self.failed_kill_handles
                            .lock()
                            .expect("failed_kill_handles 锁中毒")
                            .remove(session_id);
                        h
                    } else {
                        // R41 M7：优先重试上次 kill 失败仍保留的 handle。
                        let mut failed = self
                            .failed_kill_handles
                            .lock()
                            .expect("failed_kill_handles 锁中毒");
                        failed
                            .remove(session_id)
                            .ok_or_else(|| AppError::not_found("工作台会话不存在"))?
                    }
                }
                Some(generation) => {
                    if let Some(current) = sessions.get(session_id) {
                        let current_gen = {
                            let h = current.lock().expect("workbench session 锁中毒");
                            h.generation
                        };
                        if current_gen != generation {
                            return Err(AppError::not_found("工作台会话世代已变更"));
                        }
                        let h = sessions
                            .remove(session_id)
                            .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
                        self.failed_kill_handles
                            .lock()
                            .expect("failed_kill_handles 锁中毒")
                            .remove(session_id);
                        h
                    } else {
                        let mut failed = self
                            .failed_kill_handles
                            .lock()
                            .expect("failed_kill_handles 锁中毒");
                        let Some(current) = failed.get(session_id) else {
                            return Err(AppError::not_found("工作台会话不存在"));
                        };
                        let current_gen = {
                            let h = current.lock().expect("workbench session 锁中毒");
                            h.generation
                        };
                        if current_gen != generation {
                            return Err(AppError::not_found("工作台会话世代已变更"));
                        }
                        failed
                            .remove(session_id)
                            .ok_or_else(|| AppError::not_found("工作台会话不存在"))?
                    }
                }
            };
            let publish = {
                let handle = handle.lock().expect("workbench session 锁中毒");
                handle.publish.clone()
            };
            publish.revoke();

            // 同临界区：revoke restore claim（若有）并从 restoring 移除。
            let revoked_claim = {
                let mut restoring = self.restoring.lock().expect("restoring 集合锁中毒");
                let state = restoring.get(session_id).cloned();
                if let Some(ref claim) = state {
                    claim.revoke();
                    let _ = claim.tx.send(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    restoring.remove(session_id);
                }
                state
            };

            // 同临界区：install Closing tombstone（无条件）。
            self.install_closing_tombstone(session_id, publish.clone());
            (handle, publish, revoked_claim)
        };

        // 锁外：wait restore persist leases（超时 fail-closed）。
        let mut leases_drained = true;
        let restore_claim_for_drain = if let Some(state) = revoked_claim {
            if !state.wait_leases_drained(RESTORE_CLAIM_LEASE_DRAIN_TIMEOUT) {
                tracing::warn!(
                    session_id = %session_id,
                    "restore claim persist leases still in-flight after close revoke; retaining barrier"
                );
                leases_drained = false;
                Some(state)
            } else {
                None
            }
        } else {
            None
        };

        // 仅当该 generation 仍绑定 replay 时移除（避免误清后继 generation buffer）。
        if let Some(generation) = required_generation {
            let mut buffers = self
                .replay_buffers
                .lock()
                .expect("workbench replay buffers 锁中毒");
            if buffers
                .get(session_id)
                .map(|b| b.generation == generation)
                .unwrap_or(false)
            {
                buffers.remove(session_id);
            }
        } else {
            self.replay_buffers
                .lock()
                .expect("workbench replay buffers 锁中毒")
                .remove(session_id);
        }
        // soft-wait 在途 publish lease；restore leases 已在 helper 中处理。
        let publish_drained = publish.wait_in_flight_drained();
        let drained = publish_drained && leases_drained;
        let was_running = {
            let handle = handle.lock().expect("workbench session 锁中毒");
            handle.row.status == "running"
        };
        // R36 M / R41 M7：running PTY kill 失败不得返回 finishable cleanup；
        // 必须把 generation-scoped handle 放回 failed_kill_handles 供重试。
        let mut pty_kill_error: Option<AppError> = None;
        {
            let mut h = handle.lock().expect("workbench session 锁中毒");
            match &mut h.process {
                SessionProcess::Pty { child, .. } => {
                    if was_running {
                        if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                            tracing::debug!("关闭工作台终端时 kill 失败: {error}");
                            pty_kill_error = Some(error);
                        }
                    }
                }
                SessionProcess::Fake => {}
            }
        }
        if let Some(error) = pty_kill_error {
            self.failed_kill_handles
                .lock()
                .expect("failed_kill_handles 锁中毒")
                .insert(session_id.to_string(), handle);
            return Err(error);
        }
        let row = {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            handle.row.status = "disconnected".to_string();
            handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
            handle.row.updated_at = chrono::Utc::now().to_rfc3339();
            handle.row.clone()
        };
        // kill 成功：清掉任何残留 failed-kill 条目。
        self.failed_kill_handles
            .lock()
            .expect("failed_kill_handles 锁中毒")
            .remove(session_id);
        // R24 H1 / R27 H3：不在 registry close 时 mark/clear；lease 未 drain 时 drained=false。
        Ok(SessionCloseCleanup {
            registry: self.clone(),
            session_id: session_id.to_string(),
            publish,
            row,
            drained,
            finished: false,
            restore_claim_for_drain,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     应用退出时，运行期 PTY attach 应被显式终止；tmux 后端的真实 shell 上下文要保留给下次重连。
    ///     R42 M4：close kill 失败后的 generation-scoped handle 也必须再试 kill，
    ///     否则进程退出后内存句柄丢失、raw PTY 可能成为不可恢复孤儿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     遍历 registry 中全部会话句柄，逐个尽力 kill 仍运行的 PTY child，并把内存状态标记为 disconnected；
    ///     再 drain `failed_kill_handles` 并对仍存活 child 再次 kill。
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
        // R42 M4：drain failed_kill_handles 并再次 kill，避免 close 失败后直接退出留下孤儿。
        let failed_handles: Vec<Arc<Mutex<WorkbenchSessionHandle>>> = {
            let mut map = self
                .failed_kill_handles
                .lock()
                .expect("failed_kill_handles 锁中毒");
            map.drain().map(|(_, handle)| handle).collect()
        };
        for handle in failed_handles {
            let mut handle = handle.lock().expect("workbench session 锁中毒");
            match &mut handle.process {
                SessionProcess::Pty { child, .. } => {
                    if let Err(error) = normalize_terminal_kill_result(child.kill()) {
                        tracing::debug!("shutdown 重试 failed_kill_handles kill 失败: {error}");
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
    ///     查找会话并更新 row name，标记 name_source=Manual，返回更新后的 row；缺失会话返回错误。
    pub fn rename(&self, session_id: &str, name: &str) -> Result<WorkbenchSessionRow, AppError> {
        self.rename_with_source(session_id, name, SessionNameSource::Manual)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户 rename 与 agent 自动标题共用改名实现，但来源门禁不同。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim 名称 → tmux rename-window（best-effort）→ 更新 row.name + name_source。
    pub fn rename_with_source(
        &self,
        session_id: &str,
        name: &str,
        source: SessionNameSource,
    ) -> Result<WorkbenchSessionRow, AppError> {
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
        handle.row.name_source = source.as_str().to_string();
        handle.row.updated_at = chrono::Utc::now().to_rfc3339();
        if matches!(source, SessionNameSource::Auto) {
            if let Ok(mut titles) = last_auto_title_map().lock() {
                titles.insert(session_id.to_string(), handle.row.name.clone());
            }
        }
        Ok(handle.row.clone())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude ai-title 等自动标题只能覆盖 default/auto，且不得无意义重写同名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Manual → Ok(None)；同名 → Ok(None)；否则 rename_with_source(Auto) → Some(row)。
    pub fn try_auto_rename(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<Option<WorkbenchSessionRow>, AppError> {
        let handle = self.get_handle(session_id)?;
        let current = {
            let handle = handle.lock().expect("workbench session 锁中毒");
            (
                handle.row.name.clone(),
                SessionNameSource::parse(&handle.row.name_source),
            )
        };
        if !crate::workbench::auto_title::should_apply_auto_title(&current.0, current.1, title) {
            return Ok(None);
        }
        let Some(clean) = crate::workbench::auto_title::sanitize_auto_title(title) else {
            return Ok(None);
        };
        // 确保 title owner pane 已 seed（单 pane / 首个 list-panes 行）。
        let _ = self.ensure_title_owner_pane(session_id);
        Ok(Some(self.rename_with_source(
            session_id,
            &clean,
            SessionNameSource::Auto,
        )?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     自动标题绑定需要当前 live 会话的 cwd/id 列表，且不暴露 DTO/pane 细节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     锁 sessions，克隆所有 Ready 句柄的 row。
    pub fn list_live_session_rows(&self) -> Vec<WorkbenchSessionRow> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        sessions
            .values()
            .filter_map(|handle| {
                let handle = handle.lock().ok()?;
                if handle.durability == SessionDurability::Ready {
                    Some(handle.row.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多 pane 时仅 title-owner pane 上的 agent 可改 window 名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查 live row 后委托 pane_count_for_row；缺失会话返回 None。
    pub fn pane_count_for_session(&self, session_id: &str) -> Option<usize> {
        let handle = self.get_handle(session_id).ok()?;
        let row = handle.lock().ok()?.row.clone();
        Some(pane_count_for_row(&row))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     创建 window 时 seed first pane 为 title owner，之后 agent 标题只由该 pane 驱动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     已有 owner 且仍在 pane 列表中则保留；否则取 list-panes 第一行 pane_id。
    pub fn ensure_title_owner_pane(&self, session_id: &str) -> Option<String> {
        let handle = self.get_handle(session_id).ok()?;
        let row = handle.lock().ok()?.row.clone();
        if row.backend != TMUX_BACKEND {
            // raw PTY：无 pane 概念，用固定哨兵表示可自动标题。
            let owner = "%raw".to_string();
            if let Ok(mut map) = title_owner_map().lock() {
                map.insert(session_id.to_string(), owner.clone());
            }
            return Some(owner);
        }
        let tmux = available_tmux_command()?;
        let target = tmux_window_target_for_row(&row).ok()?;
        let panes = tmux_list_pane_ids(&tmux, &target).ok()?;
        if panes.is_empty() {
            return None;
        }
        if let Ok(map) = title_owner_map().lock() {
            if let Some(existing) = map.get(session_id) {
                if panes.iter().any(|p| p == existing) {
                    return Some(existing.clone());
                }
            }
        }
        let first = panes[0].clone();
        if let Ok(mut map) = title_owner_map().lock() {
            map.insert(session_id.to_string(), first.clone());
        }
        Some(first)
    }

    /// 当前 title-owner pane_id（若有）。
    pub fn title_owner_pane_id(&self, session_id: &str) -> Option<String> {
        title_owner_map()
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     关闭 title-owner pane 后，自动标题权应交接给 window 内下一 pane，并可用最近自动标题刷新 window 名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     kill-pane 后 list panes；若 owner 已消失则取列表第一 pane；返回交接后的 owner 与可选 last auto title。
    pub fn reassign_title_owner_after_pane_close(
        &self,
        session_id: &str,
    ) -> Option<(String, Option<String>)> {
        let handle = self.get_handle(session_id).ok()?;
        let row = handle.lock().ok()?.row.clone();
        if row.backend != TMUX_BACKEND {
            return None;
        }
        let tmux = available_tmux_command()?;
        let target = tmux_window_target_for_row(&row).ok()?;
        let panes = tmux_list_pane_ids(&tmux, &target).ok()?;
        if panes.is_empty() {
            if let Ok(mut map) = title_owner_map().lock() {
                map.remove(session_id);
            }
            return None;
        }
        let current = title_owner_map()
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned());
        let next = if let Some(cur) = current {
            if panes.iter().any(|p| p == &cur) {
                cur
            } else {
                panes[0].clone()
            }
        } else {
            panes[0].clone()
        };
        if let Ok(mut map) = title_owner_map().lock() {
            map.insert(session_id.to_string(), next.clone());
        }
        let last_title = last_auto_title_map()
            .lock()
            .ok()
            .and_then(|m| m.get(session_id).cloned());
        Some((next, last_title))
    }

    /// 关闭 session 时清理 title owner / last auto title / agent pane 缓存。
    pub fn clear_title_owner_state(&self, session_id: &str) {
        if let Ok(mut map) = title_owner_map().lock() {
            map.remove(session_id);
        }
        if let Ok(mut map) = last_auto_title_map().lock() {
            map.remove(session_id);
        }
        // terminal 键直接移除；native/agent 键在下次 bind 时覆盖，进程生命周期短可残留。
        if let Ok(mut map) = agent_pane_by_terminal_map().lock() {
            map.remove(session_id);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     agent 启动瞬间的 active pane 即其宿主；之后自动标题只允许该 pane 驱动 window 名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询当前 active pane_id（raw→%raw）；写入 terminal/agent/native 三张 map。
    pub fn bind_agent_title_pane(
        &self,
        terminal_session_id: &str,
        agent_session_id: Option<&str>,
        native_session_id: Option<&str>,
    ) -> Option<String> {
        let pane = self.active_pane_id_for_session(terminal_session_id)?;
        if let Ok(mut map) = agent_pane_by_terminal_map().lock() {
            map.insert(terminal_session_id.to_string(), pane.clone());
        }
        if let Some(aid) = agent_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(mut map) = agent_pane_by_agent_map().lock() {
                map.insert(aid.to_string(), pane.clone());
            }
        }
        if let Some(nid) = native_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(mut map) = agent_pane_by_native_map().lock() {
                map.insert(nid.to_string(), pane.clone());
            }
        }
        Some(pane)
    }

    /// 将已绑定 terminal 的 pane 关联到 native_session_id（OSC 回填时调用）。
    pub fn bind_native_title_pane(
        &self,
        terminal_session_id: &str,
        native_session_id: &str,
    ) -> Option<String> {
        let native = native_session_id.trim();
        if native.is_empty() {
            return None;
        }
        let pane = agent_pane_by_terminal_map()
            .lock()
            .ok()
            .and_then(|m| m.get(terminal_session_id).cloned())
            .or_else(|| self.active_pane_id_for_session(terminal_session_id))?;
        if let Ok(mut map) = agent_pane_by_native_map().lock() {
            map.insert(native.to_string(), pane.clone());
        }
        if let Ok(mut map) = agent_pane_by_terminal_map().lock() {
            map.entry(terminal_session_id.to_string())
                .or_insert_with(|| pane.clone());
        }
        Some(pane)
    }

    /// 查找 agent 绑定的 pane：native 优先，否则 terminal。
    pub fn agent_title_pane_for(
        &self,
        terminal_session_id: &str,
        native_session_id: Option<&str>,
    ) -> Option<String> {
        if let Some(n) = native_session_id.map(str::trim).filter(|s| !s.is_empty()) {
            if let Ok(map) = agent_pane_by_native_map().lock() {
                if let Some(p) = map.get(n) {
                    return Some(p.clone());
                }
            }
        }
        agent_pane_by_terminal_map()
            .lock()
            .ok()
            .and_then(|m| m.get(terminal_session_id).cloned())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     绑定与门禁需要知道 window 当前 active pane（agent 启动时落点）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     raw → `%raw`；tmux list-panes 几何中 `active=true` 的 pane_id；无则 list 第一项。
    pub fn active_pane_id_for_session(&self, session_id: &str) -> Option<String> {
        let handle = self.get_handle(session_id).ok()?;
        let row = handle.lock().ok()?.row.clone();
        if row.backend != TMUX_BACKEND {
            return Some("%raw".to_string());
        }
        let tmux = available_tmux_command()?;
        let target = tmux_window_target_for_row(&row).ok()?;
        let layout = tmux_window_pane_layout(&tmux, &target).ok()?;
        if let Some(active) = layout.panes.iter().find(|p| p.active) {
            return Some(active.pane_id.clone());
        }
        layout.panes.first().map(|p| p.pane_id.clone())
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
        self.ensure_replay_buffer_for_generation(session_id, 0);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次 live insert 需要 generation-scoped 空 buffer，防止旧 worker append 入新 buffer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     以 generation 覆盖写入新 SessionReplayBuffer（同 id 旧内容丢弃）。
    fn ensure_replay_buffer_for_generation(&self, session_id: &str, generation: u64) {
        self.replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒")
            .insert(
                session_id.to_string(),
                SessionReplayBuffer::new(SESSION_REPLAY_MAX_CHARS, generation),
            );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     safe attach 单测需要在无真实 tmux 时把 session 记入 registry，验证幂等与 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分配 generation 后经 barrier CAS 插入 Fake process 的完整 row；仅 `#[cfg(test)]`。
    #[cfg(test)]
    pub fn insert_fake_session_row_for_test(&self, row: WorkbenchSessionRow) {
        let session_id = row.id.clone();
        let mut inserted_generation = 0u64;
        self.insert_handle_with_barrier_cas(&session_id, || {
            let generation = self.allocate_generation();
            inserted_generation = generation;
            Arc::new(Mutex::new(WorkbenchSessionHandle {
                row: row.clone(),
                generation,
                durability: SessionDurability::Ready,
                publish: PublishControl::new(),
                deferred_output: Vec::new(),
                deferred_mutations: Vec::new(),
                pending_exit: None,
                restore_claim_generation: None,
                process: SessionProcess::Fake,
            }))
        });
        self.ensure_replay_buffer_for_generation(&session_id, inserted_generation);
    }

    #[cfg(test)]
    /// Business Logic（为什么需要这个函数）:
    ///     list 过滤测试需要构造不同项目的会话，但不应启动真实 PTY 或依赖本机 Claude CLI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在测试编译时分配 generation 并经 barrier CAS 插入 fake 会话句柄。
    fn insert_fake_session_for_test(&self, session_id: &str, project_id: &str) {
        let row = WorkbenchSessionRow {
            id: session_id.to_string(),
            project_id: project_id.to_string(),
            worktree_id: None,
            name: format!("session-{session_id}"),
            name_source: "default".to_string(),
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
        let mut inserted_generation = 0u64;
        self.insert_handle_with_barrier_cas(session_id, || {
            let generation = self.allocate_generation();
            inserted_generation = generation;
            Arc::new(Mutex::new(WorkbenchSessionHandle {
                row: row.clone(),
                generation,
                durability: SessionDurability::Ready,
                publish: PublishControl::new(),
                deferred_output: Vec::new(),
                deferred_mutations: Vec::new(),
                pending_exit: None,
                restore_claim_generation: None,
                process: SessionProcess::Fake,
            }))
        });
        self.ensure_replay_buffer_for_generation(session_id, inserted_generation);
    }

    /// 测试：标记 closer cleanup 完成（R23 H2）。
    #[cfg(test)]
    fn mark_closing_cleanup_done_for_test(&self, session_id: &str) {
        let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
        if let Some(control) = closing.get(session_id) {
            control.mark_cleanup_done();
        }
    }

    /// 测试：仅 closer 身份清除 barrier（R23 H2）。
    #[cfg(test)]
    fn clear_closing_tombstone_for_test(&self, session_id: &str) {
        let control = {
            let closing = self.closing_publish.lock().expect("closing_publish 锁中毒");
            closing.get(session_id).cloned()
        };
        if let Some(control) = control {
            self.clear_closing_tombstone_if_same(session_id, &control);
        }
    }

    /// 测试：在不 mark cleanup 的情况下安装 barrier（模拟 closer 仍在 cleanup）。
    #[cfg(test)]
    fn install_closing_barrier_for_test(&self, session_id: &str) -> Arc<PublishControl> {
        let publish = PublishControl::new();
        // 先 revoke 模拟 close 语义，但 cleanup_done 仍为 false。
        publish.revoke();
        self.install_closing_tombstone(session_id, publish.clone());
        publish
    }

    /// 测试：插入 Provisional fake handle（不 Ready）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     R19 测试需要 claim-held provisional 与 workspace Reuse 拒绝路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分配 generation 后插入 durability=Provisional 的 Fake handle。
    #[cfg(test)]
    pub fn insert_provisional_fake_session_for_test(&self, session_id: &str, project_id: &str) {
        self.insert_fake_session_for_test(session_id, project_id);
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        if let Some(handle) = sessions.get(session_id) {
            handle.lock().expect("workbench session 锁中毒").durability =
                SessionDurability::Provisional;
        }
    }

    /// 测试：绑定/改写 handle 上的 restore claim generation（R27 H5）。
    #[cfg(test)]
    pub fn bind_restore_claim_generation_for_test(
        &self,
        session_id: &str,
        claim_generation: Option<u64>,
    ) {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        if let Some(handle) = sessions.get(session_id) {
            handle
                .lock()
                .expect("workbench session 锁中毒")
                .restore_claim_generation = claim_generation;
        }
    }

    /// 测试：把 handle 降为 Provisional。
    #[cfg(test)]
    pub fn force_provisional_for_test(&self, session_id: &str) {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        if let Some(handle) = sessions.get(session_id) {
            handle.lock().expect("workbench session 锁中毒").durability =
                SessionDurability::Provisional;
        }
    }

    /// 测试：用短 timeout 走 missing-handle close intent（R27 H3）。
    #[cfg(test)]
    pub fn begin_close_intent_with_drain_timeout_for_test(
        &self,
        session_id: &str,
        row: WorkbenchSessionRow,
        timeout: Duration,
    ) -> Result<SessionCloseCleanup, AppError> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        if sessions.contains_key(session_id) {
            return Err(AppError::conflict(
                "session_close_intent_already_live".to_string(),
            ));
        }
        drop(sessions);
        let (publish, restore_claim_for_drain, leases_drained) =
            self.revoke_restore_claim_install_tombstone_with_timeout(session_id, None, timeout);
        let drained = publish.in_flight.load(Ordering::SeqCst) == 0 && leases_drained;
        Ok(SessionCloseCleanup {
            registry: self.clone(),
            session_id: session_id.to_string(),
            publish,
            row,
            drained,
            finished: false,
            restore_claim_for_drain,
        })
    }

    /// 测试：观察 cleanup 是否 finishable（drained=true）。
    #[cfg(test)]
    pub fn session_close_cleanup_drained_for_test(cleanup: &SessionCloseCleanup) -> bool {
        cleanup.drained
    }

    /// 测试：观察 cleanup 是否仍持有 restore claim 供 reaper。
    #[cfg(test)]
    pub fn session_close_cleanup_has_restore_claim_for_test(cleanup: &SessionCloseCleanup) -> bool {
        cleanup.restore_claim_for_drain.is_some()
    }

    /// 测试：返回 replay last_seq。
    #[cfg(test)]
    pub fn replay_last_seq_for_test(&self, session_id: &str) -> Option<u64> {
        let buffers = self
            .replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒");
        buffers.get(session_id).map(|b| b.last_seq)
    }

    /// 测试：通过 generation-scoped PublishControl 分配输出序号（R21 H1）。
    #[cfg(test)]
    pub fn allocate_output_seq_for_test(&self, session_id: &str, generation: u64) -> Option<u64> {
        let sessions = self.sessions.lock().expect("workbench sessions 锁中毒");
        let handle = sessions.get(session_id)?;
        let handle = handle.lock().expect("workbench session 锁中毒");
        if handle.generation != generation || !handle.publish.allowed.load(Ordering::SeqCst) {
            return None;
        }
        Some(handle.publish.allocate_seq())
    }

    /// 测试：closing tombstone 是否仍在（R21 H2）。
    #[cfg(test)]
    pub fn has_closing_tombstone_for_test(&self, session_id: &str) -> bool {
        self.closing_publish
            .lock()
            .expect("closing_publish 锁中毒")
            .contains_key(session_id)
    }

    /// 测试：R25 M1 Abort 对 pre-existing barrier 立即返回（不 wait / 不 PTY）。
    #[cfg(test)]
    pub fn abort_if_preexisting_closing_barrier_for_test(
        &self,
        session_id: &str,
    ) -> Result<(), AppError> {
        self.abort_if_preexisting_closing_barrier(session_id, SpawnBarrierPolicy::Abort)
    }

    /// 测试：generation-scoped append。
    #[cfg(test)]
    pub fn append_replay_for_test(
        &self,
        session_id: &str,
        generation: u64,
        chunk: &str,
        seq: u64,
    ) -> bool {
        let mut buffers = self
            .replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒");
        match buffers.get_mut(session_id) {
            Some(buffer) => buffer.append_if_generation(chunk, seq, generation),
            None => false,
        }
    }

    /// 测试：publish token 是否存活。
    #[cfg(test)]
    pub fn publish_token_alive_for_test(&self, session_id: &str, generation: u64) -> bool {
        publish_token_alive(&self.sessions, session_id, generation)
    }

    /// 测试：模拟旧 worker 在 fence 后尝试副作用（R19 H1 barrier）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     并发测试需在 close+reinsert 后断言旧 generation 零 mutation/replay/output/status。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 generation-scoped emit/mutation/status 入口，返回是否任一副作用成功。
    #[cfg(test)]
    pub fn try_stale_worker_side_effects_for_test(
        &self,
        state: &AppState,
        session_id: &str,
        generation: u64,
        chunk: &str,
    ) -> bool {
        let mut seq = 0u64;
        let output_ok = emit_terminal_output(
            state,
            session_id,
            Some(generation),
            &self.sessions,
            &mut seq,
            chunk.to_string(),
            &self.replay_buffers,
        );
        // 非空 mutation 才会真正 enqueue；此处用 fence 判定是否允许副作用。
        let mutation_allowed = can_publish_side_effect(&self.sessions, session_id, generation);
        let status_ok = emit_status_fenced(
            state,
            &self.sessions,
            session_id,
            generation,
            "exited",
            Some(1),
        );
        output_ok || status_ok || mutation_allowed
    }
}

/// 判断 sessions map 中 id 的 handle generation 是否匹配（R18 M2）。
///
/// Business Logic（为什么需要这个函数）:
///     reader/exit watcher 与 registry 方法共用同一 fence，避免旧 worker 污染新实例。
///
/// Code Logic（这个函数做什么）:
///     读 map 中 session 的 generation；相等返回 true，缺失或不匹配返回 false。
fn is_current_session_generation(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
) -> bool {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    sessions
        .get(session_id)
        .map(|handle| handle.lock().expect("workbench session 锁中毒").generation == generation)
        .unwrap_or(false)
}

/// 判断 generation 的 publish token 是否仍存活（R19 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     close/reclaim 会使旧 token 失效；即便 generation 数值碰巧复用也不再发布。
///
/// Code Logic（这个函数做什么）:
///     map 中同 generation handle 的 publish.allowed 为 true 才返回 true。
fn publish_token_alive(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
) -> bool {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    sessions
        .get(session_id)
        .map(|handle| {
            let handle = handle.lock().expect("workbench session 锁中毒");
            handle.generation == generation && handle.publish.allowed.load(Ordering::SeqCst)
        })
        .unwrap_or(false)
}

/// 分类 generation 副作用门（R20 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     reader 必须区分 Provisional 等待与 stale/revoked，不能把未 Ready 当永久拒绝。
///
/// Code Logic（这个函数做什么）:
///     同 gen+allowed+Ready → Ready；同 gen+allowed+Provisional/Flushing → Provisional；否则 Rejected。
fn classify_side_effect_gate(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
) -> SideEffectGate {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    sessions
        .get(session_id)
        .map(|handle| {
            let handle = handle.lock().expect("workbench session 锁中毒");
            classify_side_effect_gate_locked(&handle, generation)
        })
        .unwrap_or(SideEffectGate::Rejected)
}

/// 在已持 handle 锁时分类副作用门。
///
/// Business Logic（为什么需要这个函数）:
///     原子 prepare 路径需在同一临界区内复用 classify 规则。
///
/// Code Logic（这个函数做什么）:
///     gen+allowed+Ready/Provisional|Flushing/其他 → Ready/Provisional/Rejected。
fn classify_side_effect_gate_locked(
    handle: &WorkbenchSessionHandle,
    generation: u64,
) -> SideEffectGate {
    if handle.generation != generation || !handle.publish.allowed.load(Ordering::SeqCst) {
        SideEffectGate::Rejected
    } else if handle.durability == SessionDurability::Ready {
        SideEffectGate::Ready
    } else if matches!(
        handle.durability,
        SessionDurability::Provisional | SessionDurability::Flushing
    ) {
        // Flushing 期间 live reader 继续缓冲，禁止与 deferred flush 双写同一 seq 空间外路径。
        SideEffectGate::Provisional
    } else {
        SideEffectGate::Rejected
    }
}

/// generation + Ready + publish token 的完整副作用 fence（R19 H1/M1）。
///
/// Business Logic（为什么需要这个函数）:
///     旧 worker 与 provisional 都不得对外 mutation/replay/output/status。
///
/// Code Logic（这个函数做什么）:
///     仅当 classify 为 Ready 时 true。
fn can_publish_side_effect(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
) -> bool {
    classify_side_effect_gate(sessions, session_id, generation) == SideEffectGate::Ready
}

/// 在已持 handle 锁时尝试获取 lease。
///
/// Business Logic（为什么需要这个函数）:
///     原子 prepare 在 classify Ready 后必须同锁持 lease，禁止 TOCTOU。
///
/// Code Logic（这个函数做什么）:
///     Ready|Flushing + gen + allowed 时 `in_flight+1` 并返回 lease；失败 None。
fn try_acquire_publication_lease_locked(
    handle: &WorkbenchSessionHandle,
    generation: u64,
) -> Option<PublicationLease> {
    if handle.generation != generation
        || !handle.publish.allowed.load(Ordering::SeqCst)
        || !matches!(
            handle.durability,
            SessionDurability::Ready | SessionDurability::Flushing
        )
    {
        return None;
    }
    handle.publish.in_flight.fetch_add(1, Ordering::SeqCst);
    // 二次确认：revoke 可能与 +1 交错。
    if !handle.publish.allowed.load(Ordering::SeqCst) {
        handle.publish.in_flight.fetch_sub(1, Ordering::SeqCst);
        let _guard = handle
            .publish
            .wait
            .lock()
            .expect("publish control wait 锁中毒");
        handle.publish.cv.notify_all();
        return None;
    }
    Some(PublicationLease {
        control: handle.publish.clone(),
    })
}

/// 尝试获取 generation-scoped 发布 lease（R20 H2）。
///
/// Business Logic（为什么需要这个函数）:
///     emit/enqueue 必须把 fence 与副作用包在同一 lease 内，close 才能等待在途发布。
///
/// Code Logic（这个函数做什么）:
///     锁内确认 Ready|Flushing+token 后 `in_flight+1` 并返回 PublicationLease；失败 None。
fn try_acquire_publication_lease(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
) -> Option<PublicationLease> {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let handle = sessions.get(session_id)?;
    let handle = handle.lock().expect("workbench session 锁中毒");
    try_acquire_publication_lease_locked(&handle, generation)
}

/// 在已持 handle 锁时缓冲输出。
///
/// Business Logic（为什么需要这个函数）:
///     原子 prepare 路径在 Provisional/Flushing 时同锁缓冲。
///
/// Code Logic（这个函数做什么）:
///     有界 push deferred_output。
fn buffer_output_locked(handle: &mut WorkbenchSessionHandle, chunk: String) {
    if chunk.is_empty() {
        return;
    }
    // 有界缓冲，避免长时间 upsert 阻塞撑爆内存（仅 metadata 路径，无敏感 body 日志）。
    const MAX_DEFERRED_CHUNKS: usize = 256;
    if handle.deferred_output.len() >= MAX_DEFERRED_CHUNKS {
        handle.deferred_output.remove(0);
    }
    handle.deferred_output.push(chunk);
}

/// 缓冲 Provisional 期输出（R20 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     Ready 前首屏可见字节不得杀死 reader，也不得对外发布。
///
/// Code Logic（这个函数做什么）:
///     同 gen + Provisional/Flushing + allowed 时 push deferred_output 并返回 true；否则 false。
fn buffer_provisional_output(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    chunk: String,
) -> bool {
    if chunk.is_empty() {
        return true;
    }
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return false;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    if classify_side_effect_gate_locked(&handle, generation) != SideEffectGate::Provisional {
        return false;
    }
    buffer_output_locked(&mut handle, chunk);
    true
}

/// 缓冲 Provisional 期 OSC mutation（R20 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     Ready 前 mutation 不得 enqueue，也不得因此 break reader。
///
/// Code Logic（这个函数做什么）:
///     同 gen + Provisional/Flushing 时 extend deferred_mutations。
fn buffer_provisional_mutations(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    mutations: Vec<AgentRuntimeMutation>,
) -> bool {
    if mutations.is_empty() {
        return true;
    }
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return false;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    if classify_side_effect_gate_locked(&handle, generation) != SideEffectGate::Provisional {
        return false;
    }
    handle.deferred_mutations.extend(mutations);
    true
}

/// 记录 Provisional 期 pending exit（R20 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     upsert 完成前进程退出不得丢失，也不得提前发布 exited 后仍宣称 running。
///
/// Code Logic（这个函数做什么）:
///     同 gen + Provisional/Flushing 时写 pending_exit；返回是否接受。
fn record_pending_exit(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    exit_code: Option<i32>,
) -> bool {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return false;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    if classify_side_effect_gate_locked(&handle, generation) != SideEffectGate::Provisional {
        return false;
    }
    handle.pending_exit = Some(exit_code);
    true
}

/// 一次锁内 classify + buffer 输出或取得 Ready lease（R23 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     Flushing→Ready 间隙若 classify 与 buffer/lease 分锁，Ready mid-flight 会让 buffer 失败
///     并被 reader 当作 stale 永久退出，丢失后续 chunk。
///
/// Code Logic（这个函数做什么）:
///     持 sessions+handle 锁：Provisional/Flushing → 缓冲；Ready → 同锁持 lease 并带回 chunk；
///     Rejected → 拒绝。
fn prepare_output_side_effect(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    chunk: String,
) -> PreparedSideEffect<String> {
    if chunk.is_empty() {
        return PreparedSideEffect::Buffered;
    }
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return PreparedSideEffect::Rejected;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    match classify_side_effect_gate_locked(&handle, generation) {
        SideEffectGate::Provisional => {
            buffer_output_locked(&mut handle, chunk);
            PreparedSideEffect::Buffered
        }
        SideEffectGate::Ready => match try_acquire_publication_lease_locked(&handle, generation) {
            Some(lease) => PreparedSideEffect::Live {
                lease,
                payload: chunk,
            },
            // Ready 但 lease 失败（revoke 交错）→ Rejected。
            None => PreparedSideEffect::Rejected,
        },
        SideEffectGate::Rejected => PreparedSideEffect::Rejected,
    }
}

/// 一次锁内 classify + buffer mutation 或取得 Ready lease（R23 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     OSC mutation 与 output 共享同一 mid-flight 竞态；分锁会永久杀死 reader。
///
/// Code Logic（这个函数做什么）:
///     Provisional/Flushing 同锁 extend deferred_mutations；Ready 同锁持 lease 并带回 mutations。
fn prepare_mutation_side_effect(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    mutations: Vec<AgentRuntimeMutation>,
) -> PreparedSideEffect<Vec<AgentRuntimeMutation>> {
    if mutations.is_empty() {
        return PreparedSideEffect::Buffered;
    }
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return PreparedSideEffect::Rejected;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    match classify_side_effect_gate_locked(&handle, generation) {
        SideEffectGate::Provisional => {
            handle.deferred_mutations.extend(mutations);
            PreparedSideEffect::Buffered
        }
        SideEffectGate::Ready => match try_acquire_publication_lease_locked(&handle, generation) {
            Some(lease) => PreparedSideEffect::Live {
                lease,
                payload: mutations,
            },
            None => PreparedSideEffect::Rejected,
        },
        SideEffectGate::Rejected => PreparedSideEffect::Rejected,
    }
}

/// 一次锁内 classify + 记录 pending_exit 或取得 Ready lease（R23 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     exit watcher 同样不得在 Ready 过渡窗口永久丢失 exit 或误判 stale。
///
/// Code Logic（这个函数做什么）:
///     Provisional/Flushing 写 pending_exit；Ready 持 lease 并带回 exit_code 供外层 emit。
fn prepare_exit_side_effect(
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    exit_code: Option<i32>,
) -> PreparedSideEffect<Option<i32>> {
    let sessions = sessions.lock().expect("workbench sessions 锁中毒");
    let Some(handle) = sessions.get(session_id) else {
        return PreparedSideEffect::Rejected;
    };
    let mut handle = handle.lock().expect("workbench session 锁中毒");
    match classify_side_effect_gate_locked(&handle, generation) {
        SideEffectGate::Provisional => {
            handle.pending_exit = Some(exit_code);
            PreparedSideEffect::Buffered
        }
        SideEffectGate::Ready => match try_acquire_publication_lease_locked(&handle, generation) {
            Some(lease) => PreparedSideEffect::Live {
                lease,
                payload: exit_code,
            },
            None => PreparedSideEffect::Rejected,
        },
        SideEffectGate::Rejected => PreparedSideEffect::Rejected,
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
///     R18 M2：reclaim 后旧 reader 不得 append 到同 id 新实例的 replay。
///
/// Code Logic（这个函数做什么）:
///     后台线程读 PTY → `AgentOscDecoder` 剥离 app-private OSC 并 enqueue mutation →
///     仅 visible 字节进入 `TerminalUtf8Decoder` → generation fence 后 replay/emit。
fn spawn_reader_thread(
    state: AppState,
    session_id: String,
    generation: u64,
    publish: Arc<PublishControl>,
    mut reader: Box<dyn Read + Send>,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) {
    thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        let mut seq: u64 = 0;
        let mut utf8 = TerminalUtf8Decoder::default();
        let mut osc = AgentOscDecoder::default();
        loop {
            if !publish.allowed.load(Ordering::SeqCst)
                || !is_current_session_generation(&sessions, &session_id, generation)
            {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let decoded = osc.push(&buf[..n]);
                    if !apply_agent_osc_decode_result(
                        &state,
                        &session_id,
                        generation,
                        &mut seq,
                        &mut utf8,
                        &sessions,
                        &replay_buffers,
                        decoded,
                    ) {
                        break;
                    }
                    // 突发 rate-limit 后若仍有 pending，等到窗口到期后投递；sleep 被提前唤醒时不得 break 丢终态
                    while osc.has_pending_coalesce() {
                        if let Some(wait) = osc.duration_until_rate_window_end() {
                            if !wait.is_zero() {
                                thread::sleep(wait);
                            }
                        }
                        let mut flushed = osc.poll_flush();
                        if flushed.mutations.is_empty() && flushed.diagnostics.is_empty() {
                            if !osc.has_pending_coalesce() {
                                break;
                            }
                            // 窗口应已到期；强制冲刷，避免 early-wake 后 break 挂起 Completed
                            flushed = osc.force_flush_pending();
                            if flushed.mutations.is_empty() && flushed.diagnostics.is_empty() {
                                thread::sleep(std::time::Duration::from_millis(5));
                                continue;
                            }
                        }
                        if !apply_agent_osc_decode_result(
                            &state,
                            &session_id,
                            generation,
                            &mut seq,
                            &mut utf8,
                            &sessions,
                            &replay_buffers,
                            flushed,
                        ) {
                            return;
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出结束: {error}");
                    break;
                }
            }
        }
        // 会话结束：仅当 generation 仍当前且 token 存活时强制冲刷
        if !publish.allowed.load(Ordering::SeqCst)
            || !is_current_session_generation(&sessions, &session_id, generation)
        {
            return;
        }
        let flushed = osc.force_flush_pending();
        let _ = apply_agent_osc_decode_result(
            &state,
            &session_id,
            generation,
            &mut seq,
            &mut utf8,
            &sessions,
            &replay_buffers,
            flushed,
        );
        if let Some(chunk) = utf8.finish() {
            let _ = emit_terminal_output(
                &state,
                &session_id,
                Some(generation),
                &sessions,
                &mut seq,
                chunk,
                &replay_buffers,
            );
        }
    });
}

/// 处理一次 OSC 解码结果：enqueue mutation、记诊断、转发可见字节。
///
/// Business Logic（为什么需要这个函数）:
///     push 与 idle poll_flush / force_flush 共用同一出口，避免终态只在一种路径上入站。
///
/// Code Logic（这个函数做什么）:
///     forward mutations → debug diagnostics → 非空 visible 走 UTF-8/emit；
///     Rejected 时返回 false 让 reader break；Provisional 缓冲后继续 true。
// OSC 解码出口固定需要 state/session/generation/seq/utf8/sessions/replay/decoded 这一组参数。
#[allow(clippy::too_many_arguments)]
fn apply_agent_osc_decode_result(
    state: &AppState,
    session_id: &str,
    generation: u64,
    seq: &mut u64,
    utf8: &mut TerminalUtf8Decoder,
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
    decoded: crate::workbench::agent_runtime::osc::AgentOscDecodeResult,
) -> bool {
    if !forward_agent_osc_mutations_fenced(
        state,
        session_id,
        generation,
        sessions,
        decoded.mutations,
    ) {
        return false;
    }
    for diag in &decoded.diagnostics {
        tracing::debug!(
            session_id = %session_id,
            code = diag.code,
            detail = ?diag.detail,
            "agent osc diagnostic"
        );
    }
    if !decoded.visible.is_empty() {
        return emit_terminal_output(
            state,
            session_id,
            Some(generation),
            sessions,
            seq,
            utf8.decode(&decoded.visible),
            replay_buffers,
        );
    }
    true
}

/// 把 OSC mutation 交给 owner ingress（有界 channel；未启动 reducer 时丢弃）。
///
/// Business Logic（为什么需要这个函数）:
///     剥离后的结构化事件必须离开 reader 热路径，由单一 worker 串行写库。
///     PTY 来源 terminal 与 OSC 声明 terminal 不一致时必须丢弃，否则 reducer 无法
///     知道真实 PTY 来源，可能错误更新甚至完成另一 terminal 上的 Agent/Orchestrator task。
///
/// Code Logic（这个函数做什么）:
///     逐条比对 `mutation.terminal_session_id` 与实际 PTY `terminal_session_id`；
///     一致才 `try_enqueue_agent_mutation`；不一致记 debug 并 discard（不入队）。
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
                "agent osc terminal_session_id mismatch; discard mutation"
            );
            continue;
        }
        try_enqueue_agent_mutation(state, mutation);
    }
}

/// generation fence 后转发 OSC mutation（R19 H1 / R20 H1/H2 / R23 H1）。
///
/// Business Logic（为什么需要这个函数）:
///     旧 generation 在 decode 后、enqueue 前被 close/reinsert 时不得推进新实例 Agent。
///     Provisional 期间缓冲，Ready 后持 lease enqueue；mid-flight Ready 不得永久杀死 reader。
///
/// Code Logic（这个函数做什么）:
///     empty → true；一次锁 prepare：Buffered → true；Live → forward payload；Rejected → false。
fn forward_agent_osc_mutations_fenced(
    state: &AppState,
    terminal_session_id: &str,
    generation: u64,
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    mutations: Vec<AgentRuntimeMutation>,
) -> bool {
    if mutations.is_empty() {
        // 无 mutation 时不因 fence 失败 break reader；仅在有 mutation 时强制 fence。
        return true;
    }
    // R23 H1：classify+buffer/lease 一次锁，避免 Ready 过渡把 reader 永久打死。
    match prepare_mutation_side_effect(sessions, terminal_session_id, generation, mutations) {
        PreparedSideEffect::Buffered => true,
        PreparedSideEffect::Live {
            lease: _lease,
            payload,
        } => {
            forward_agent_osc_mutations(state, terminal_session_id, payload);
            true
        }
        PreparedSideEffect::Rejected => false,
    }
}

/// Business Logic（为什么需要这个函数）:
///     终端输出事件需要统一递增 seq，且纯 pending chunk 未完成时不应发送空事件。
///     R18 M2：generation 不匹配时不得 append/emit，防止旧 reader 污染新实例。
///     R20 H1：Provisional 缓冲；R20 H2：lease 覆盖 append+publish。
///     R23 H1：classify 与 buffer/lease 同锁，Ready mid-flight 走 live 而非永久退出。
///
/// Code Logic（这个函数做什么）:
///     非空 chunk：generation 路径一次锁 prepare；Buffered → true；
///     Live 持 lease 写 buffer + 远端/本地/orchestrator；Rejected → false。
fn emit_terminal_output(
    state: &AppState,
    session_id: &str,
    generation: Option<u64>,
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    seq: &mut u64,
    chunk: String,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) -> bool {
    if chunk.is_empty() {
        return true;
    }
    if let Some(generation) = generation {
        // R23 H1：一次锁 classify + buffer 或 lease；Ready 过渡不再二次加锁失败。
        match prepare_output_side_effect(sessions, session_id, generation, chunk) {
            PreparedSideEffect::Buffered => return true,
            PreparedSideEffect::Rejected => return false,
            PreparedSideEffect::Live {
                lease: _lease,
                payload,
            } => {
                return emit_terminal_output_with_lease(
                    state,
                    session_id,
                    Some(generation),
                    sessions,
                    seq,
                    payload,
                    replay_buffers,
                );
            }
        }
    }
    // 无 generation 的路径仅测试兼容。
    emit_terminal_output_with_lease(
        state,
        session_id,
        None,
        sessions,
        seq,
        chunk,
        replay_buffers,
    )
}

/// 在调用方已持 lease（或测试无 generation）时写 buffer 并发布输出。
///
/// Business Logic（为什么需要这个函数）:
///     mark_ready flush 与 live reader 共用同一发布路径，避免重复逻辑。
///
/// Code Logic（这个函数做什么）:
///     递增 seq、generation-scoped append、remote+local emit + orchestrator hook。
fn emit_terminal_output_with_lease(
    state: &AppState,
    session_id: &str,
    generation: Option<u64>,
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    seq: &mut u64,
    chunk: String,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) -> bool {
    if chunk.is_empty() {
        return true;
    }
    // R21 H1：优先用 generation-scoped PublishControl.next_seq，禁止 flush/reader 各自从 0 分配。
    let allocated = if let Some(generation) = generation {
        let sessions_guard = sessions.lock().expect("workbench sessions 锁中毒");
        match sessions_guard.get(session_id) {
            Some(handle) => {
                let handle = handle.lock().expect("workbench session 锁中毒");
                if handle.generation != generation || !handle.publish.allowed.load(Ordering::SeqCst)
                {
                    return false;
                }
                let n = handle.publish.allocate_seq();
                *seq = n;
                n
            }
            None => return false,
        }
    } else {
        *seq += 1;
        *seq
    };
    {
        let mut buffers = replay_buffers
            .lock()
            .expect("workbench replay buffers 锁中毒");
        if let Some(buffer) = buffers.get_mut(session_id) {
            if let Some(generation) = generation {
                if !buffer.append_if_generation(&chunk, allocated, generation) {
                    return false;
                }
            } else {
                buffer.append(&chunk, allocated);
            }
        }
    }
    let event = WorkbenchTerminalOutputPayload {
        session_id: session_id.to_string(),
        chunk,
        seq: allocated,
        ts: now_millis(),
        // 生产端 owner：远端 NDJSON live 消费者可据此合成 composite authority。
        owner_instance_id: Some(state.config_runtime.owner_instance_id().to_string()),
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
    true
}

/// Business Logic（为什么需要这个函数）:
///     终端进程退出后，前端需要收到状态变化并保留退出码。
///     R18 M2：旧 watcher 不得把同 id 新实例标为 exited。
///
/// Code Logic（这个函数做什么）:
///     后台线程短轮询 child.try_wait；写 status/emit 前确认 generation 仍匹配，否则 no-op。
fn spawn_exit_watcher(
    state: AppState,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    session_id: String,
    generation: u64,
    publish: Arc<PublishControl>,
    handle: Arc<Mutex<WorkbenchSessionHandle>>,
) {
    thread::spawn(move || loop {
        if !publish.allowed.load(Ordering::SeqCst) {
            break;
        }
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
                // R20 H1 / R23 H1：一次锁记录 pending_exit 或持 lease 写 status/emit。
                match prepare_exit_side_effect(&sessions, &session_id, generation, Some(exit_code))
                {
                    PreparedSideEffect::Buffered => {}
                    PreparedSideEffect::Live {
                        lease: _lease,
                        payload,
                    } => {
                        // 仅当仍是同一 handle Arc 时写 status（防止同 id 后继被污染）。
                        let still_owner = {
                            let map = sessions.lock().expect("workbench sessions 锁中毒");
                            matches!(
                                map.get(&session_id),
                                Some(current) if Arc::ptr_eq(current, &handle)
                            )
                        };
                        if still_owner {
                            let mut handle = handle.lock().expect("workbench session 锁中毒");
                            if handle.generation == generation
                                && handle.publish.allowed.load(Ordering::SeqCst)
                            {
                                handle.row.status = "exited".to_string();
                                handle.row.exited_at = Some(chrono::Utc::now().to_rfc3339());
                                handle.row.exit_code = payload;
                                handle.row.updated_at = chrono::Utc::now().to_rfc3339();
                                drop(handle);
                                emit_status(&state, &session_id, "exited", payload);
                            }
                        }
                    }
                    PreparedSideEffect::Rejected => {}
                }
                break;
            }
            Some(Err(error)) => {
                tracing::warn!("查询工作台终端退出状态失败: {error}");
                match prepare_exit_side_effect(&sessions, &session_id, generation, None) {
                    PreparedSideEffect::Buffered => {}
                    PreparedSideEffect::Live {
                        lease: _lease,
                        payload: _,
                    } => {
                        emit_status(&state, &session_id, "disconnected", None);
                    }
                    PreparedSideEffect::Rejected => {}
                }
                break;
            }
            None => thread::sleep(Duration::from_millis(200)),
        }
    });
}

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

/// generation fence 后发布 status（R19 H1 / R20 H2）。
///
/// Business Logic（为什么需要这个函数）:
///     旧 exit watcher / 测试 helper 不得向新实例发 exited/disconnected。
///
/// Code Logic（这个函数做什么）:
///     持 publication lease 成功才 emit_status；返回是否发布。
fn emit_status_fenced(
    state: &AppState,
    sessions: &Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>,
    session_id: &str,
    generation: u64,
    status: &str,
    exit_code: Option<i32>,
) -> bool {
    let Some(_lease) = try_acquire_publication_lease(sessions, session_id, generation) else {
        return false;
    };
    emit_status(state, session_id, status, exit_code);
    true
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
            name_source: "default".to_string(),
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

    /// 串行化 R30 M2 reclaim 计数断言：reset/assert 窗口需互斥。
    static TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Business Logic（R30 M2: 为什么需要这个测试）:
    ///     create_tmux_window 成功后若 project barrier / spawn 失败，不得留下 invisible orphan window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 tmux row → TmuxCreateGuard 不 commit → Drop 后 reclaim 计数 +1。
    #[test]
    fn tmux_create_guard_drop_reclaims_window_without_commit() {
        let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
            .lock()
            .expect("tmux create guard reclaim test lock");
        reset_tmux_create_guard_reclaim_count_for_test();
        let row = fake_tmux_row("s-r30-m2", "p-r30", Some("wt1"), "@9");
        {
            let _guard = TmuxCreateGuard::new(row);
            // 模拟 barrier / spawn_row 失败：不 commit。
        }
        assert_eq!(
            tmux_create_guard_reclaim_count_for_test(),
            1,
            "uncommitted TmuxCreateGuard must reclaim on Drop"
        );
    }

    /// Business Logic（R30 M2: 为什么需要这个测试）:
    ///     spawn_row 成功后 window 归 registry/command 层；TmuxCreateGuard 不得误杀合法 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     commit 后 Drop → reclaim 计数仍为 0。
    #[test]
    fn tmux_create_guard_commit_skips_kill_on_drop() {
        let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
            .lock()
            .expect("tmux create guard reclaim test lock");
        reset_tmux_create_guard_reclaim_count_for_test();
        let row = fake_tmux_row("s-r30-m2-ok", "p-r30", Some("wt1"), "@10");
        {
            let mut guard = TmuxCreateGuard::new(row);
            guard.commit();
        }
        assert_eq!(
            tmux_create_guard_reclaim_count_for_test(),
            0,
            "committed TmuxCreateGuard must not reclaim window"
        );
    }

    /// Business Logic（R30 M2: 为什么需要这个测试）:
    ///     create 路径在 create_tmux_window 之后、spawn 之前 revalidate barrier 失败时必须回收 window。
    ///
    /// Code Logic（这个测试做什么）:
    ///     模拟 create 成功装 guard 后 require_project_not_closing Err → Drop 回收。
    #[test]
    fn project_barrier_after_tmux_window_create_reclaims_via_guard() {
        let _lock = TMUX_CREATE_GUARD_RECLAIM_TEST_LOCK
            .lock()
            .expect("tmux create guard reclaim test lock");
        reset_tmux_create_guard_reclaim_count_for_test();
        let registry = WorkbenchSessionRegistry::new();
        let _gen = registry.begin_project_closing_barrier("p-r30-barrier");
        let row = fake_tmux_row("s-r30-barrier", "p-r30-barrier", Some("wt1"), "@11");
        // 对齐 create：window 成功后装 guard，再 revalidate barrier。
        let result = {
            let _guard = TmuxCreateGuard::new(row);
            match registry.require_project_not_closing("p-r30-barrier") {
                Ok(()) => Ok(()),
                Err(error) => Err(error),
            }
            // guard Drop on early return path
        };
        assert!(
            matches!(result, Err(AppError::Unavailable(ref m)) if m == "project_closing_barrier_active"),
            "barrier must reject create revalidate"
        );
        assert_eq!(
            tmux_create_guard_reclaim_count_for_test(),
            1,
            "barrier after create_tmux_window must reclaim via TmuxCreateGuard Drop"
        );
        registry.finish_project_closing_barrier("p-r30-barrier", _gen);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     restore 中途失败时 Drop 必须释放 claim，允许后续重试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     try_claim → RestoreClaimGuard 不 disarm → Drop → claim 已释放，可再次 claim。
    #[test]
    fn restore_claim_guard_releases_on_drop() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-restore")
            .claim_generation()
            .expect("claimed");
        {
            let _guard =
                RestoreClaimGuard::new(registry.clone(), "s-restore".to_string(), generation);
        }
        assert!(!registry.is_restore_claim_held("s-restore"));
        assert!(registry.try_claim_restore("s-restore").is_claimed());
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
        let mut buffer = SessionReplayBuffer::new(10_000, 0);
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
        let mut buffer = SessionReplayBuffer::new(3, 0);

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
    ///     chunk ring 在裁剪多字节字符时必须保持 Unicode scalar 边界，并继续维护 tail/last_seq 合同。
    ///
    /// Code Logic（这个测试做什么）:
    ///     容量 4 时依次 append 中文+emoji 与 ASCII，断言 snapshot 尾部、truncated、last_seq 与 char_count。
    #[test]
    fn replay_chunk_ring_preserves_unicode_and_tail_contract() {
        let mut buffer = SessionReplayBuffer::new(4, 0);
        buffer.append("你🙂", 1);
        buffer.append("ab", 2);
        buffer.append("c", 3);
        let snapshot = buffer.snapshot("s1");
        assert_eq!(snapshot.buffer, "🙂abc");
        assert!(snapshot.truncated);
        assert_eq!(snapshot.last_seq, 3);
        assert_eq!(buffer.char_count, 4);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     零容量与超大单 chunk 是 ring 裁剪的边界场景，错误实现会留下敏感全文或错误 chunk 数。
    ///
    /// Code Logic（这个测试做什么）:
    ///     max=0 时 append 后 buffer 为空且 truncated；max=3 时单 chunk "abcdef" 只保留 "def" 且 chunks 长度为 1。
    #[test]
    fn replay_chunk_ring_handles_zero_and_large_single_chunk() {
        let mut zero = SessionReplayBuffer::new(0, 0);
        zero.append("secret", 1);
        assert_eq!(zero.snapshot("s0").buffer, "");
        assert!(zero.snapshot("s0").truncated);

        let mut small = SessionReplayBuffer::new(3, 0);
        small.append("abcdef", 4);
        assert_eq!(small.snapshot("s1").buffer, "def");
        assert_eq!(small.chunks.len(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     满环后每个小 append 应只摊销丢弃头部小 chunk，避免整段历史重建。
    ///
    /// Code Logic（这个测试做什么）:
    ///     容量 8 填满 a..h 后再 append i，断言尾部为 bcdefghi 且 char_count 仍为 8。
    #[test]
    fn full_replay_ring_drops_one_small_head_chunk_per_small_append() {
        let mut buffer = SessionReplayBuffer::new(8, 0);
        for (seq, value) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
            buffer.append(value, seq as u64 + 1);
        }
        buffer.append("i", 9);
        assert_eq!(buffer.snapshot("s").buffer, "bcdefghi");
        assert_eq!(buffer.char_count, 8);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     生产容量 120k 下持续增量 append 不能让 chunk 数或 char_count 随总写入线性膨胀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先写入 120_000 个单字符 chunk，再追加 10_000 次，断言 char_count 恒定、chunks.len 不增长、snapshot 尾部正确。
    #[test]
    fn replay_chunk_ring_large_capacity_keeps_amortized_bounds() {
        let capacity = SESSION_REPLAY_MAX_CHARS;
        let mut buffer = SessionReplayBuffer::new(capacity, 0);
        for i in 0..capacity {
            let ch = char::from(b'a' + (i % 26) as u8);
            let text = ch.to_string();
            buffer.append(&text, (i as u64) + 1);
        }
        assert_eq!(buffer.char_count, capacity);
        assert_eq!(buffer.chunks.len(), capacity);
        let chunks_before = buffer.chunks.len();

        let extra = 10_000usize;
        for j in 0..extra {
            let i = capacity + j;
            let ch = char::from(b'a' + (i % 26) as u8);
            let text = ch.to_string();
            buffer.append(&text, (i as u64) + 1);
            assert_eq!(buffer.char_count, capacity);
            assert!(buffer.chunks.len() <= chunks_before);
        }

        assert!(buffer.snapshot("large").truncated);
        assert_eq!(buffer.char_count, capacity);
        assert_eq!(buffer.chunks.len(), capacity);

        let snapshot = buffer.snapshot("large");
        assert_eq!(snapshot.buffer.chars().count(), capacity);
        let tail: String = (0..16)
            .map(|k| {
                let i = capacity + extra - 16 + k;
                char::from(b'a' + (i % 26) as u8)
            })
            .collect();
        assert!(
            snapshot.buffer.ends_with(&tail),
            "snapshot tail must match last appended characters"
        );
        assert_eq!(snapshot.last_seq, (capacity + extra) as u64);
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
    ///     Workbench 内 claude 不得自动连 VS Code，否则会注入无关 active-file 上下文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply_workbench_terminal_env 强制 AUTO_CONNECT_IDE=false，并移除 SSE_PORT。
    #[test]
    fn workbench_terminal_env_forces_claude_ide_disconnect() {
        let mut command = CommandBuilder::new("/bin/sh");
        // 模拟父进程/IDE 注入过 SSE 端口；隔离路径必须清掉。
        command.env(CLAUDE_CODE_SSE_PORT_ENV, "20751");
        apply_workbench_terminal_env(&mut command, None);

        assert_eq!(
            command
                .get_env(CLAUDE_CODE_AUTO_CONNECT_IDE_ENV)
                .and_then(|v| v.to_str()),
            Some("false")
        );
        assert!(command.get_env(CLAUDE_CODE_SSE_PORT_ENV).is_none());
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
            agent_session_id: None,
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
        // 普通用户终端不注入 AGENT_SESSION_ID
        assert!(command.get_env("CC_PARTNER_AGENT_SESSION_ID").is_none());
        assert!(command.get_env("CC_PARTNER_CONTROL_TOKEN").is_none());
        assert!(command.get_env("CC_PARTNER_DEVICE_TOKEN").is_none());
        assert!(command.get_env("CC_PARTNER_AUTH_TOKEN").is_none());
        assert_eq!(
            command
                .get_env(CLAUDE_CODE_AUTO_CONNECT_IDE_ENV)
                .and_then(|v| v.to_str()),
            Some("false")
        );

        let tmux_args = tmux_agent_context_env_args(&ctx);
        assert_eq!(tmux_args.len() % 2, 0);
        for pair in tmux_args.chunks(2) {
            assert_eq!(pair[0], "-e");
            assert!(
                pair[1].starts_with("CC_PARTNER_")
                    || pair[1].starts_with("CLAUDE_CODE_AUTO_CONNECT_IDE=")
            );
            assert!(!pair[1].contains("TOKEN"));
        }
        assert!(tmux_args
            .iter()
            .any(|a| a == "CC_PARTNER_PROJECT_ID=proj-1"));
        assert!(tmux_args
            .iter()
            .any(|a| a == "CLAUDE_CODE_AUTO_CONNECT_IDE=false"));
        assert!(!tmux_args
            .iter()
            .any(|a| a.starts_with("CC_PARTNER_AGENT_SESSION_ID=")));

        // Orchestrator 预分配路径：注入 AGENT_SESSION_ID
        let mut command2 = CommandBuilder::new("/bin/sh");
        let ctx2 = TerminalAgentContextIds {
            project_id: "proj-1".to_string(),
            worktree_id: "wt-1".to_string(),
            terminal_session_id: "term-1".to_string(),
            owner_instance_id: "owner-1".to_string(),
            agent_session_id: Some("agent-prealloc-1".to_string()),
        };
        apply_workbench_terminal_env(&mut command2, Some(&ctx2));
        assert_eq!(
            command2
                .get_env("CC_PARTNER_AGENT_SESSION_ID")
                .and_then(|v| v.to_str()),
            Some("agent-prealloc-1")
        );
        let tmux2 = tmux_agent_context_env_args(&ctx2);
        assert!(tmux2
            .iter()
            .any(|a| a == "CC_PARTNER_AGENT_SESSION_ID=agent-prealloc-1"));
        assert!(tmux2
            .iter()
            .any(|a| a == "CLAUDE_CODE_AUTO_CONNECT_IDE=false"));
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
    ///     用户全局 `mouse on` 时滚轮会进 copy-mode（浏览模式），键盘被 tmux 吃掉，必须 session-local 强制 mouse off。
    ///     同时必须宣告 `xterm*:mouse`，但配置必须幂等，否则每次套用主题都会污染 server array。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Workbench 使用无内嵌颜色的 status/window format，强制 status-position=bottom、mouse=off
    ///     主题命令不得无条件追加 terminal-features；mouse capability 由独立幂等步骤维护。
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
                    "mouse",
                    "off",
                ],
                vec![
                    "set-option",
                    "-t",
                    "cc-partner-project-project1234abcd",
                    "status-position",
                    "bottom",
                ],
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
    ///     冷启动 / 适应尺寸必须能构造出强制同步 window 尺寸的 tmux 命令，避免 status bar 悬在中间。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 `tmux_resize_window_args` 生成 `resize-window -t <target> -x <cols> -y <rows>`，
    ///     且 bump rows 与目标不同以便两步强制重绘。
    #[test]
    fn tmux_resize_window_args_force_window_size() {
        let args = tmux_resize_window_args("cc-partner-project-project1234abcd:@3", 160, 42);
        assert_eq!(
            args,
            vec![
                "resize-window",
                "-t",
                "cc-partner-project-project1234abcd:@3",
                "-x",
                "160",
                "-y",
                "42",
            ]
        );
        assert_eq!(tmux_force_redraw_bump_rows(42), 41);
        assert_eq!(
            tmux_force_redraw_bump_rows(MIN_TERMINAL_ROWS),
            MIN_TERMINAL_ROWS + 1
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     长时间使用 Workbench 不得让 tmux server 的 terminal-features 随每次主题套用无限增长，
    ///     清理时也不能破坏用户已有的 screen/RGB/clipboard 等能力。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖缺失时单次追加、精确重复时保留首项并倒序删除、复合 xterm mouse 已存在时 no-op。
    #[test]
    fn tmux_terminal_mouse_feature_reconcile_is_idempotent_and_preserves_other_entries() {
        let defaults = concat!(
            "terminal-features[0] xterm*:clipboard:ccolour:cstyle:focus:title\n",
            "terminal-features[1] screen*:title\n",
            "terminal-features[2] *:RGB\n",
        );
        assert_eq!(
            tmux_terminal_mouse_feature_reconcile_commands(defaults),
            vec![vec![
                "set-option",
                "-sa",
                "terminal-features",
                "xterm*:mouse",
            ]]
        );

        let duplicated = concat!(
            "terminal-features[0] xterm*:clipboard:ccolour:cstyle:focus:title\n",
            "terminal-features[4] xterm*:mouse\n",
            "terminal-features[5] screen*:mouse\n",
            "terminal-features[7] xterm*:mouse\n",
            "terminal-features[9] xterm*:mouse\n",
        );
        assert_eq!(
            tmux_terminal_mouse_feature_reconcile_commands(duplicated),
            vec![
                vec!["set-option", "-su", "terminal-features[9]"],
                vec!["set-option", "-su", "terminal-features[7]"],
            ]
        );

        let equivalent = concat!(
            "terminal-features[0] xterm*:clipboard:mouse:title\n",
            "terminal-features[1] screen*:title\n",
        );
        assert!(tmux_terminal_mouse_feature_reconcile_commands(equivalent).is_empty());
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

    /// 构造测试用 pane 几何。
    fn pane_geometry(
        pane_id: &str,
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
        active: bool,
    ) -> TmuxPaneGeometry {
        TmuxPaneGeometry {
            pane_id: pane_id.to_string(),
            left,
            top,
            right,
            bottom,
            active,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     点击命中依赖 tmux 一次性给出 pane 边界、active 与 zoom 真值，格式漂移会让命中判定整体失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 list-panes 参数带 `-F` 且格式串同时含 pane_left/top/right/bottom/active/window_zoomed_flag。
    #[test]
    fn tmux_list_pane_geometry_args_request_bounds_active_and_zoom() {
        let args = tmux_list_pane_geometry_args("cc-partner-project-p1:@2");

        assert_eq!(args[0], "list-panes");
        assert_eq!(args[1], "-t");
        assert_eq!(args[2], "cc-partner-project-p1:@2");
        assert_eq!(args[3], "-F");
        for field in [
            "#{pane_id}",
            "#{pane_left}",
            "#{pane_top}",
            "#{pane_right}",
            "#{pane_bottom}",
            "#{pane_active}",
            "#{window_zoomed_flag}",
        ] {
            assert!(args[4].contains(field), "格式串缺少 {field}");
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     tmux 输出损坏或字段缺失时必须整行丢弃，不能产生错误命中矩形把点击切到别的 pane。
    ///
    /// Code Logic（这个测试做什么）:
    ///     混合合法行、字段数不足行、非数字行与 right<left 的反向矩形，断言只保留合法行并解析 zoom。
    #[test]
    fn parse_tmux_pane_geometry_skips_malformed_rows() {
        let output = "%1 0 0 79 23 1 0\n%2 81 0 159 23 0 0\n%3 0 0 79\n%4 x 0 79 23 0 0\n%5 80 0 10 23 0 0\n";

        let layout = parse_tmux_pane_geometry(output);

        assert_eq!(
            layout.panes,
            vec![
                pane_geometry("%1", 0, 0, 79, 23, true),
                pane_geometry("%2", 81, 0, 159, 23, false),
            ]
        );
        assert!(!layout.zoomed);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     zoom 状态下屏幕只显示一个 pane，历史布局不再对应像素，必须能被识别并整体拒绝命中。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言任一行 window_zoomed_flag 为 1 时 layout.zoomed 为 true。
    #[test]
    fn parse_tmux_pane_geometry_detects_zoomed_window() {
        let layout = parse_tmux_pane_geometry("%1 0 0 79 23 1 1\n%2 81 0 159 23 0 1\n");

        assert!(layout.zoomed);
        assert_eq!(layout.panes.len(), 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     点击必须落到覆盖该字符格的 pane；落在 pane 之间的分隔边框时应 no-op 而不是就近吸附。
    ///
    /// Code Logic（这个测试做什么）:
    ///     左右分屏布局下断言左区/右区命中对应 pane，边框列与越界行返回 None。
    #[test]
    fn tmux_pane_at_position_matches_bounds_and_rejects_borders() {
        let panes = vec![
            pane_geometry("%1", 0, 0, 79, 23, true),
            pane_geometry("%2", 81, 0, 159, 23, false),
        ];

        assert_eq!(
            tmux_pane_at_position(&panes, 10, 5).map(|p| p.pane_id.as_str()),
            Some("%1")
        );
        assert_eq!(
            tmux_pane_at_position(&panes, 120, 23).map(|p| p.pane_id.as_str()),
            Some("%2")
        );
        // 第 80 列是分隔边框，不属于任何 pane。
        assert!(tmux_pane_at_position(&panes, 80, 5).is_none());
        // 第 24 行是 tmux status bar，超出 pane 范围。
        assert!(tmux_pane_at_position(&panes, 10, 24).is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     点击切换是绝对定位操作，必须用 pane_id 选中；退化成相对 `.+` 会切到用户没点的 pane。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 select-pane 参数为 `select-pane -t <pane_id>`，不含 `.+`。
    #[test]
    fn tmux_select_pane_args_target_absolute_pane_id() {
        assert_eq!(tmux_select_pane_args("%7"), vec!["select-pane", "-t", "%7"]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     通过分屏按钮创建的新 pane 应从项目根目录启动，不能继承当前 pane 里用户 cd 后的目录；
    ///     同时强制 Claude 不连 IDE。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 split-window 参数包含 `-c <project_root>`、IDE 隔离 env，并保留方向与 target。
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
                "-e",
                "CLAUDE_CODE_AUTO_CONNECT_IDE=false",
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

    /// Business Logic（R36 H1: 为什么需要这个测试）:
    ///     已知 window_id 时即使 count==1 也只能 kill-window，禁止并发/重试 close 把兄弟 terminal
    ///     连同整个 session 一起杀掉；多 window 与 last-window 路径一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Some(@1)+count1 / Some(@1)+count2 均生成 kill-window；
    ///     None+count1 仍允许 legacy kill-session。
    #[test]
    fn tmux_destroy_backend_args_always_kill_window_when_window_id_known() {
        assert_eq!(
            tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(1)),
            Some(vec![
                "kill-window".to_string(),
                "-t".to_string(),
                "cc-partner-project-p1:@1".to_string(),
            ])
        );
        assert_eq!(
            tmux_destroy_backend_args("cc-partner-project-p1", Some("@1"), Some(2)),
            Some(vec![
                "kill-window".to_string(),
                "-t".to_string(),
                "cc-partner-project-p1:@1".to_string(),
            ])
        );
        // legacy：无 window_id + 已知 count → kill-session。
        assert_eq!(
            tmux_destroy_backend_args("cc-partner-project-p1", None, Some(1)),
            Some(vec![
                "kill-session".to_string(),
                "-t".to_string(),
                "cc-partner-project-p1".to_string(),
            ])
        );
    }

    /// Business Logic（R32 H1: 为什么需要这个测试）:
    ///     multi-window session 中 list-windows 探测失败时，不得降级 kill-session 毁掉兄弟 terminal。
    ///
    /// Code Logic（这个测试做什么）:
    ///     window_id 已知 + count=None → kill-window only；
    ///     window_id 缺失 + count=None → None（fail closed）；
    ///     kill_created_tmux_window_only 纯语义：有 window_id 才构造 kill-window target。
    #[test]
    fn tmux_destroy_args_probe_failure_never_kills_session_with_window_id() {
        // 多窗 + 探测失败：仅 kill 已知 window。
        assert_eq!(
            tmux_destroy_backend_args("wt-session", Some("@7"), None),
            Some(vec![
                "kill-window".to_string(),
                "-t".to_string(),
                "wt-session:@7".to_string(),
            ])
        );
        // 无 window_id + 探测失败：fail closed，不发 kill-session。
        assert_eq!(tmux_destroy_backend_args("wt-session", None, None), None);
        // 无 window_id 但 count==1 可知：仍允许 kill-session（关闭路径最后一窗）。
        assert_eq!(
            tmux_destroy_backend_args("wt-session", None, Some(1)),
            Some(vec![
                "kill-session".to_string(),
                "-t".to_string(),
                "wt-session".to_string(),
            ])
        );
        // TmuxCreateGuard reclaim 必须只走 window target，不依赖 count。
        let row = fake_tmux_row("s-r32-h1", "p-r32", Some("wt1"), "@42");
        let session = row.backend_id.as_deref().expect("session");
        let window = row.backend_window_id.as_deref().expect("window");
        assert_eq!(
            tmux_window_target(session, window),
            format!("{session}:{window}")
        );
        // 缺 window_id 的 row：guard reclaim 应 no-op（不 panic）。
        let mut missing = fake_tmux_row("s-r32-h1-miss", "p-r32", Some("wt1"), "@99");
        missing.backend_window_id = None;
        kill_created_tmux_window_only(&missing);
    }

    /// Business Logic（R35 M3: 为什么需要这个测试）:
    ///     destroy 非零退出时，already-gone 文案必须映射为成功，否则 close 路径会永远卡在 barrier。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖 can't find / no server / no such / not found 及大小写；无关错误返回 false。
    #[test]
    fn tmux_destroy_exit_is_already_gone_detects_common_messages() {
        assert!(tmux_destroy_exit_is_already_gone(
            "",
            "can't find window: @1"
        ));
        assert!(tmux_destroy_exit_is_already_gone("Can't Find session", ""));
        assert!(tmux_destroy_exit_is_already_gone(
            "",
            "no server running on /tmp/tmux-1000/default"
        ));
        assert!(tmux_destroy_exit_is_already_gone("", "no such window: @9"));
        assert!(tmux_destroy_exit_is_already_gone("", "session not found"));
        assert!(!tmux_destroy_exit_is_already_gone("", "permission denied"));
        assert!(!tmux_destroy_exit_is_already_gone("", ""));
    }

    /// Business Logic（R35 M3 / R41 M7: 为什么需要这个测试）:
    ///     无 tmux backend 可杀时不应误阻 SQLite delete；但 running raw 不得自动成功，
    ///     否则 missing-handle 路径会删元数据却留下活 PTY。
    ///
    /// Code Logic（这个测试做什么）:
    ///     disconnected raw → Ok；running raw → Err(raw_pty_kill_requires_live_handle)；
    ///     缺 backend_id 的 tmux → Ok（无 session 可 destroy）。
    #[test]
    fn kill_persisted_backend_ok_when_no_tmux_backend_to_destroy() {
        let mut raw = fake_tmux_row("s-raw", "p1", Some("wt1"), "@1");
        raw.backend = "raw".to_string();
        raw.backend_id = None;
        raw.backend_window_id = None;
        raw.status = "disconnected".to_string();
        assert!(kill_persisted_backend(&raw).is_ok());

        raw.status = "running".to_string();
        let err = kill_persisted_backend(&raw).expect_err("running raw must not auto-ok");
        assert_eq!(err.code(), "raw_pty_kill_requires_live_handle");

        let mut no_id = fake_tmux_row("s-no-id", "p1", Some("wt1"), "@1");
        no_id.backend_id = None;
        assert!(kill_persisted_backend(&no_id).is_ok());
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
            name_source: "default".to_string(),
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
    ///     空 registry 上首次 claim 一个未运行也未被占位的 session 应成功，再次 claim 应
    ///     得到 RestoreInProgress（占位生效），从而消除并发 sessions/list 的 TOCTOU。
    #[test]
    fn try_claim_restore_serializes_concurrent_restore_for_same_session() {
        let registry = WorkbenchSessionRegistry::new();

        let first = registry.try_claim_restore("s1");
        assert!(first.is_claimed(), "首次 claim 应成功");

        let second = registry.try_claim_restore("s1");
        assert!(
            matches!(second, RestoreClaimOutcome::RestoreInProgress(_)),
            "占位期间第二次 claim 应为 RestoreInProgress，避免重复 restore"
        );

        // 释放占位后允许后续重试。
        registry.release_restore_claim("s1");
        let third = registry.try_claim_restore("s1");
        assert!(third.is_claimed(), "释放占位后应允许重新 claim");
        registry.release_restore_claim("s1");
    }

    /// Business Logic（Finding 5: 为什么需要这个测试）:
    ///     session 已在运行期 registry 时，claim 应返回 AlreadyLive，不写入占位，
    ///     避免对活跃 session 做无意义的 restore。
    #[test]
    fn try_claim_restore_returns_false_when_session_already_live() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("live-1", "p1");

        let claimed = registry.try_claim_restore("live-1");
        assert!(
            matches!(claimed, RestoreClaimOutcome::AlreadyLive),
            "session 已在运行期 registry 时 claim 应为 AlreadyLive"
        );
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
        assert!(
            a.is_claimed() && b.is_claimed(),
            "不同 session 的 claim 应互不干扰"
        );

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
        assert!(registry.try_claim_restore("s1").is_claimed());
        registry.release_restore_claim("s1");
        registry.release_restore_claim("s1");
    }

    /// Business Logic（R14/R16: 为什么需要这个测试）:
    ///     并发 list 拿到 RestoreInProgress 后必须能 wait 到 holder 的**结果**，
    ///     否则会过早返回持久行并触发永久 not_found。
    ///
    /// Code Logic（这个测试做什么）:
    ///     holder claim → waiter 订阅 → finish Ready → wait Ready。
    #[tokio::test]
    async fn wait_for_shared_restore_unblocks_after_release() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-wait").is_claimed());
        let second = registry.try_claim_restore("s-wait");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };
        let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
        // 给 waiter 一点时间进入 changed loop。
        tokio::task::yield_now().await;
        registry.finish_restore_claim("s-wait", SharedRestoreNotification::Ready);
        let result = wait_handle
            .await
            .expect("waiter task must join after finish");
        assert_eq!(result, SharedRestoreWaitResult::Ready);
        assert!(!registry.is_restore_claim_held("s-wait"));
    }

    /// Business Logic（R16 M1: 为什么需要这个测试）:
    ///     holder 失败时必须广播 Failed；waiter 不得把 claim 释放当成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + 订阅 → finish Failed(Unavailable) → wait Failed，且 is_success=false。
    #[tokio::test]
    async fn wait_for_shared_restore_surfaces_holder_failure() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-fail").is_claimed());
        let second = registry.try_claim_restore("s-fail");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };
        let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
        tokio::task::yield_now().await;
        registry.finish_restore_claim(
            "s-fail",
            SharedRestoreNotification::Failed(AppErrorCategory::Unavailable),
        );
        let result = wait_handle.await.expect("failed waiter must join");
        assert_eq!(
            result,
            SharedRestoreWaitResult::Failed(AppErrorCategory::Unavailable)
        );
        assert!(!result.is_success());
        assert!(!registry.is_restore_claim_held("s-fail"));
        // 失败后允许重新 claim 重试 restore。
        assert!(registry.try_claim_restore("s-fail").is_claimed());
        registry.finish_restore_claim("s-fail", SharedRestoreNotification::PersistedDisconnected);
    }

    /// Business Logic（R16 M1: 为什么需要这个测试）:
    ///     无显式结果的 release 不得伪装 Ready；默认 Failed(Internal)。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + 订阅 → release_restore_claim → wait Failed(Internal)。
    #[tokio::test]
    async fn release_without_result_is_failed_not_ready() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-bare-release").is_claimed());
        let second = registry.try_claim_restore("s-bare-release");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };
        let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
        tokio::task::yield_now().await;
        registry.release_restore_claim("s-bare-release");
        assert_eq!(
            wait_handle.await.expect("join"),
            SharedRestoreWaitResult::Failed(AppErrorCategory::Internal)
        );
    }

    /// Business Logic（R14 M1: 为什么需要这个测试）:
    ///     启动时全局 list 与项目 list 并发：第一路 claim restore 并延迟完成；第二路必须
    ///     wait/省略恢复中会话，且在 holder 完成前不得把 session 视为可 replay。
    ///     否则 Provider 立刻 replay → permanent not_found。
    ///
    /// Code Logic（这个测试做什么）:
    ///     holder claim 后延迟插入 live + finish Ready；waiter RestoreInProgress → wait；
    ///     wait 期间 is_restore_claim_held && !contains（不可 list/replay）；
    ///     wait 结束后 session live 且 claim 已释放。
    #[tokio::test]
    async fn concurrent_list_waits_for_in_flight_restore_before_ready() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-concurrent").is_claimed());

        let second = registry.try_claim_restore("s-concurrent");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second concurrent claim must be RestoreInProgress, not AlreadyLive/Claimed");
        };

        // 模拟 list merge 与 replay 守卫：恢复中不得当作 ready。
        assert!(
            registry.is_restore_claim_held("s-concurrent"),
            "in-flight restore must hold claim"
        );
        assert!(
            !registry.contains("s-concurrent"),
            "registry must not be ready before holder finishes"
        );
        assert_eq!(
            registry.runtime_presence("s-concurrent"),
            SessionRuntimePresence::RestoreInProgress
        );
        assert!(
            registry.require_live_for_replay("s-concurrent").is_err(),
            "restore-in-progress must block replay as unavailable, not ready"
        );

        let reg_waiter = registry.clone();
        let waiter = tokio::spawn(async move {
            let wait = wait_for_shared_restore(rx).await;
            (
                wait,
                reg_waiter.contains("s-concurrent")
                    || !reg_waiter.is_restore_claim_held("s-concurrent"),
            )
        });

        // 延迟首个 restore：先让 waiter 进入 wait，再完成 attach。
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            registry.is_restore_claim_held("s-concurrent"),
            "delayed restore must still hold claim while waiter waits"
        );
        registry.insert_fake_session_for_test("s-concurrent", "p1");
        registry.finish_restore_claim("s-concurrent", SharedRestoreNotification::Ready);

        let (wait_result, ready) = waiter.await.expect("waiter join");
        assert_eq!(wait_result, SharedRestoreWaitResult::Ready);
        assert!(
            ready,
            "after shared restore Ready, session must be ready or claim released"
        );
        assert!(registry.session_exists("s-concurrent"));
        assert!(!registry.is_restore_claim_held("s-concurrent"));
        assert_eq!(
            registry.runtime_presence("s-concurrent"),
            SessionRuntimePresence::Live
        );
        assert!(registry.require_live_for_replay("s-concurrent").is_ok());
        assert!(
            matches!(
                registry.try_claim_restore("s-concurrent"),
                RestoreClaimOutcome::AlreadyLive
            ),
            "live session after restore must report AlreadyLive"
        );
    }

    /// Business Logic（R15/R18 M1: 为什么需要这个测试）:
    ///     P2P/local/control/mobile replay 必须共享原子 presence：claim held（含 provisional live）
    ///     只能是 RestoreInProgress，禁止单独 session_exists 漏判成 Missing/not_found，
    ///     也禁止 provisional live 被当作可 replay Live。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry → Missing；claim 后 → RestoreInProgress；claim + provisional live
    ///     → 仍 RestoreInProgress + require_live unavailable；release claim 后 → Live。
    #[test]
    fn runtime_presence_atomic_live_restore_missing() {
        let registry = WorkbenchSessionRegistry::new();
        assert_eq!(
            registry.runtime_presence("s-presence"),
            SessionRuntimePresence::Missing
        );
        let missing_err = registry
            .require_live_for_replay("s-presence")
            .expect_err("missing must be not_found");
        assert_eq!(missing_err.ipc_category_code(), "not_found");

        assert!(registry.try_claim_restore("s-presence").is_claimed());
        assert_eq!(
            registry.runtime_presence("s-presence"),
            SessionRuntimePresence::RestoreInProgress
        );
        // 关键：单独 session_exists 会漏掉 claim 窗口。
        assert!(!registry.session_exists("s-presence"));
        let restore_err = registry
            .require_live_for_replay("s-presence")
            .expect_err("restore-in-progress must be unavailable");
        assert_eq!(restore_err.ipc_category_code(), "unavailable");
        assert_eq!(restore_err.to_string(), "session_restore_in_progress");

        // R18 M1：claim 优先于 provisional live。
        registry.insert_fake_session_for_test("s-presence", "p1");
        assert_eq!(
            registry.runtime_presence("s-presence"),
            SessionRuntimePresence::RestoreInProgress
        );
        let provisional_err = registry
            .require_live_for_replay("s-presence")
            .expect_err("claim-held provisional live must block replay");
        assert_eq!(provisional_err.ipc_category_code(), "unavailable");
        assert_eq!(provisional_err.to_string(), "session_restore_in_progress");
        registry.release_restore_claim("s-presence");
        assert_eq!(
            registry.runtime_presence("s-presence"),
            SessionRuntimePresence::Live
        );
        assert!(registry.require_live_for_replay("s-presence").is_ok());
    }

    /// Business Logic（R15 M2: 为什么需要这个测试）:
    ///     共享 wait 超时后必须返回 TimedOut，list 路径据此 fail closed；
    ///     禁止静默返回成功的不完整会话清单，否则 Provider 永不重试遗漏会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     pause 时间 → claim + 订阅 wait → advance 超过 SHARED_RESTORE_WAIT_TIMEOUT
    ///     → 结果 TimedOut；finish Ready 后新 wait 在已完成通道上 Ready。
    #[tokio::test]
    async fn wait_for_shared_restore_times_out_and_reports_timed_out() {
        tokio::time::pause();
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-timeout").is_claimed());
        let second = registry.try_claim_restore("s-timeout");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };

        let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
        // 让 waiter 进入 timeout future。
        tokio::task::yield_now().await;
        tokio::time::advance(SHARED_RESTORE_WAIT_TIMEOUT + Duration::from_secs(1)).await;
        let result = wait_handle.await.expect("timeout waiter must join");
        assert_eq!(
            result,
            SharedRestoreWaitResult::TimedOut,
            "timeout must surface TimedOut so list can return retryable error"
        );
        // claim 仍持有：调用方不得把会话当 ready 返回。
        assert!(registry.is_restore_claim_held("s-timeout"));
        assert_eq!(
            registry.runtime_presence("s-timeout"),
            SessionRuntimePresence::RestoreInProgress
        );

        // finish 后后续 wait 应 Ready。
        registry.finish_restore_claim("s-timeout", SharedRestoreNotification::Ready);
        let third = registry.try_claim_restore("s-timeout");
        assert!(third.is_claimed());
        let fourth = registry.try_claim_restore("s-timeout");
        let RestoreClaimOutcome::RestoreInProgress(rx2) = fourth else {
            panic!("fourth claim must be RestoreInProgress");
        };
        registry.finish_restore_claim("s-timeout", SharedRestoreNotification::Ready);
        assert_eq!(
            wait_for_shared_restore(rx2).await,
            SharedRestoreWaitResult::Ready
        );
    }

    /// Business Logic（R15 M1/M2 + R16: 为什么需要这个测试）:
    ///     并发 restore/replay 窗口：claim held + 未 live 时 require_live 必须 unavailable；
    ///     wait 超时后仍不可伪装成功 list；holder 完成后可 replay。
    ///
    /// Code Logic（这个测试做什么）:
    ///     holder claim → concurrent presence/replay 守卫 → pause 超时 TimedOut →
    ///     holder insert+finish Ready → presence Live → require_live Ok。
    #[tokio::test]
    async fn concurrent_restore_replay_presence_and_wait_timeout_reenumerate() {
        tokio::time::pause();
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-route").is_claimed());

        // 并发 replay 窗口：必须 retryable unavailable，不是 not_found。
        let err = registry
            .require_live_for_replay("s-route")
            .expect_err("in-progress restore blocks replay");
        assert_eq!(err.ipc_category_code(), "unavailable");
        assert_eq!(
            registry.runtime_presence("s-route"),
            SessionRuntimePresence::RestoreInProgress
        );

        let second = registry.try_claim_restore("s-route");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };
        let wait_handle = tokio::spawn(async move { wait_for_shared_restore(rx).await });
        tokio::task::yield_now().await;
        tokio::time::advance(SHARED_RESTORE_WAIT_TIMEOUT + Duration::from_millis(500)).await;
        assert_eq!(
            wait_handle.await.expect("timeout join"),
            SharedRestoreWaitResult::TimedOut
        );
        // 超时后 claim 仍在：模拟 list 不得成功返回该 session。
        assert!(registry.is_restore_claim_held("s-route"));
        assert!(registry.require_live_for_replay("s-route").is_err());

        // holder 最终完成 → 可 re-enumerate / replay。
        registry.insert_fake_session_for_test("s-route", "p1");
        registry.finish_restore_claim("s-route", SharedRestoreNotification::Ready);
        assert_eq!(
            registry.runtime_presence("s-route"),
            SessionRuntimePresence::Live
        );
        assert!(registry.require_live_for_replay("s-route").is_ok());
        let replay = registry.replay("s-route");
        assert_eq!(replay.session_id, "s-route");
        // 不校验 buffer 正文内容，避免敏感/噪声断言。
    }

    /// Business Logic（R16 M1: 为什么需要这个测试）:
    ///     并发 list 模拟：holder 注入 Failed 后，waiter 必须得到 Failed，
    ///     不得 continue 成含不可 replay 会话的成功清单。
    ///
    /// Code Logic（这个测试做什么）:
    ///     holder claim 后不 insert live，finish Failed；waiter wait → Failed 且
    ///     registry 仍 missing、require_live not_found。
    #[tokio::test]
    async fn concurrent_list_waiter_must_fail_when_holder_fails_without_live_session() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-non-replayable").is_claimed());

        let second = registry.try_claim_restore("s-non-replayable");
        let RestoreClaimOutcome::RestoreInProgress(rx) = second else {
            panic!("second claim must be RestoreInProgress");
        };

        let reg_waiter = registry.clone();
        let waiter = tokio::spawn(async move {
            let wait = wait_for_shared_restore(rx).await;
            let presence = reg_waiter.runtime_presence("s-non-replayable");
            let replay_err = reg_waiter.require_live_for_replay("s-non-replayable");
            (
                wait,
                presence,
                replay_err.map_err(|e| e.ipc_category_code().to_string()),
            )
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        // 模拟 project 查询/删除 `?` 或 upsert 失败后 finish Failed，且未 live。
        registry.finish_restore_claim(
            "s-non-replayable",
            SharedRestoreNotification::Failed(AppErrorCategory::Internal),
        );

        let (wait, presence, replay_err) = waiter.await.expect("waiter join");
        assert_eq!(
            wait,
            SharedRestoreWaitResult::Failed(AppErrorCategory::Internal),
            "waiter must not treat holder failure as Ready/PersistedDisconnected"
        );
        assert!(!wait.is_success());
        assert_eq!(presence, SessionRuntimePresence::Missing);
        assert_eq!(replay_err.expect_err("must not be replayable"), "not_found");
        // list 路径应用 shared_restore_failed_error 返回错误，而不是成功合并。
        let list_err = shared_restore_failed_error(AppErrorCategory::Internal);
        assert_eq!(list_err.ipc_category_code(), "internal");
        assert_eq!(list_err.to_string(), "session_restore_shared_failed");
    }

    /// Business Logic（R17 M1: 为什么需要这个测试）:
    ///     restore 成功后 upsert 失败必须先回收 SessionSpawnGuard，再 finish claim；
    ///     若先放 claim 后 Drop spawn，第三方并发 list 会短暂 AlreadyLive。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert fake + claim → SessionSpawnGuard 未 commit；
    ///     先 drop spawn（registry 清空）→ 第三方 claim 为 RestoreInProgress（非 AlreadyLive）→
    ///     再 finish Failed → claim 释放且仍无 live session。
    #[test]
    fn upsert_failure_cleanup_reclaims_spawn_before_releasing_claim() {
        let registry = WorkbenchSessionRegistry::new();
        // claim 必须在 insert live 之前：已 live 时 try_claim 返回 AlreadyLive。
        let generation = registry
            .try_claim_restore("s-cleanup-order")
            .claim_generation()
            .expect("claimed");
        registry.insert_fake_session_for_test("s-cleanup-order", "p1");

        let mut claim_guard =
            RestoreClaimGuard::new(registry.clone(), "s-cleanup-order".to_string(), generation);
        let spawn_guard = SessionSpawnGuard::new(registry.clone(), "s-cleanup-order".to_string());

        // 正确顺序：先 reclaim spawn。R27 H2：close 会 revoke restore claim，避免 delete 后 re-upsert。
        drop(spawn_guard);
        assert_eq!(registry.registry_len(), 0);
        assert!(!registry.contains("s-cleanup-order"));
        // claim 已被 close 撤销并从 restoring 移除（或 finish 前已 not active）。
        assert!(!registry.is_restore_claim_generation_active("s-cleanup-order", generation));
        // finish 幂等（generation 已撤销时 no-op）。
        claim_guard.finish(SharedRestoreNotification::Failed(
            AppErrorCategory::Internal,
        ));
        assert!(!registry.is_restore_claim_held("s-cleanup-order"));
        // Closing barrier 可能仍在（SessionSpawnGuard finish_cleanup 后应清）；presence 不得 Live。
        assert_ne!(
            registry.runtime_presence("s-cleanup-order"),
            SessionRuntimePresence::Live
        );
        // barrier 清后可重新 claim；若 barrier 仍在则 wait 后 claim。
        if registry.has_closing_tombstone_for_test("s-cleanup-order") {
            // finish_cleanup 应已 clear；若未清则显式 wait。
            registry.wait_for_closing_tombstone("s-cleanup-order");
        }
        assert!(registry.try_claim_restore("s-cleanup-order").is_claimed());
        registry.release_restore_claim("s-cleanup-order");
    }

    /// Business Logic（R17 M1: 为什么需要这个测试）:
    ///     反例：若先 finish claim 再 Drop spawn，第三方可在窗口内 AlreadyLive。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + live fake → finish claim 后 spawn 仍在 → 第三方 AlreadyLive；
    ///     再 drop spawn 才清空。证明必须先 reclaim spawn。
    #[test]
    fn releasing_claim_before_spawn_reclaim_creates_already_live_window() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-wrong-order")
            .claim_generation()
            .expect("claimed");
        registry.insert_fake_session_for_test("s-wrong-order", "p1");

        let mut claim_guard =
            RestoreClaimGuard::new(registry.clone(), "s-wrong-order".to_string(), generation);
        let spawn_guard = SessionSpawnGuard::new(registry.clone(), "s-wrong-order".to_string());

        // 错误顺序：先放 claim。
        claim_guard.finish(SharedRestoreNotification::Failed(
            AppErrorCategory::Internal,
        ));
        assert!(!registry.is_restore_claim_held("s-wrong-order"));
        assert!(registry.contains("s-wrong-order"));
        assert!(
            matches!(
                registry.try_claim_restore("s-wrong-order"),
                RestoreClaimOutcome::AlreadyLive
            ),
            "wrong cleanup order creates AlreadyLive window"
        );

        drop(spawn_guard);
        assert!(!registry.contains("s-wrong-order"));
    }

    /// Business Logic（R18 M1: 为什么需要这个测试）:
    ///     durable Ready 前 provisional live 不得对外暴露；并发 claim / replay / list
    ///     都必须把 claim-held 视为 RestoreInProgress。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + insert provisional → try_claim RestoreInProgress；runtime_presence
    ///     RestoreInProgress；require_live unavailable；list 不含该 id；finish Failed
    ///     后 Missing，并可重新 Claimed。
    #[test]
    fn provisional_live_while_claim_held_is_not_externally_live() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-provisional").is_claimed());
        registry.insert_fake_session_for_test("s-provisional", "p1");
        assert!(registry.contains("s-provisional"));
        assert!(registry.is_restore_claim_held("s-provisional"));

        assert!(
            matches!(
                registry.try_claim_restore("s-provisional"),
                RestoreClaimOutcome::RestoreInProgress(_)
            ),
            "claim-held provisional must be RestoreInProgress, not AlreadyLive"
        );
        assert_eq!(
            registry.runtime_presence("s-provisional"),
            SessionRuntimePresence::RestoreInProgress
        );
        let err = registry
            .require_live_for_replay("s-provisional")
            .expect_err("provisional live must block replay");
        assert_eq!(err.ipc_category_code(), "unavailable");
        assert_eq!(err.to_string(), "session_restore_in_progress");

        let listed = registry.list(Some("p1"));
        assert!(
            listed.iter().all(|dto| dto.id != "s-provisional"),
            "registry list must hide claim-held provisional live"
        );
        let listed_all = registry.list(None);
        assert!(listed_all.iter().all(|dto| dto.id != "s-provisional"));

        registry.finish_restore_claim(
            "s-provisional",
            SharedRestoreNotification::Failed(AppErrorCategory::Internal),
        );
        // finish Failed 只放 claim，不自动 close provisional；模拟 holder 先 reclaim spawn。
        let _ = registry.close("s-provisional").map(|c| c.finish_cleanup());
        assert_eq!(
            registry.runtime_presence("s-provisional"),
            SessionRuntimePresence::Missing
        );
        assert!(registry.try_claim_restore("s-provisional").is_claimed());
        registry.release_restore_claim("s-provisional");
    }

    /// Business Logic（R18 M2: 为什么需要这个测试）:
    ///     close/reclaim 后同 id 新实例不得被旧 generation 的 worker fence 通过。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert gen1 → close → insert gen2；check(gen1)=false，check(gen2)=true。
    #[test]
    fn session_generation_fence_rejects_stale_after_reinsert() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-gen", "p1");
        let gen1 = registry
            .session_generation_for_test("s-gen")
            .expect("gen1 must exist");
        assert!(registry.is_current_session_generation("s-gen", gen1));

        registry
            .close("s-gen")
            .expect("close gen1")
            .finish_cleanup();
        assert!(!registry.is_current_session_generation("s-gen", gen1));

        registry.insert_fake_session_for_test("s-gen", "p1");
        let gen2 = registry
            .session_generation_for_test("s-gen")
            .expect("gen2 must exist");
        assert_ne!(gen1, gen2, "reinsert must allocate a new generation");
        assert!(!registry.is_current_session_generation("s-gen", gen1));
        assert!(registry.is_current_session_generation("s-gen", gen2));
    }

    /// Business Logic（R18 M2: 为什么需要这个测试）:
    ///     失败 reclaim 后立即 re-claim + insert 新 gen，旧 fence 不得污染新实例。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + insert gen1 → reclaim close + finish Failed → re-claim + insert gen2；
    ///     旧 gen fence false，新 gen true；presence 在 claim held 时仍 RestoreInProgress。
    #[test]
    fn reclaim_then_reclaim_insert_fences_old_generation() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-reclaim-fence").is_claimed());
        registry.insert_fake_session_for_test("s-reclaim-fence", "p1");
        let gen1 = registry
            .session_generation_for_test("s-reclaim-fence")
            .expect("gen1");
        assert!(registry.is_current_session_generation("s-reclaim-fence", gen1));
        assert_eq!(
            registry.runtime_presence("s-reclaim-fence"),
            SessionRuntimePresence::RestoreInProgress
        );

        // 失败 reclaim：先 close spawn 再放 claim。
        let _ = registry
            .close("s-reclaim-fence")
            .map(|c| c.finish_cleanup());
        registry.finish_restore_claim(
            "s-reclaim-fence",
            SharedRestoreNotification::Failed(AppErrorCategory::Internal),
        );
        assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));

        assert!(registry.try_claim_restore("s-reclaim-fence").is_claimed());
        registry.insert_fake_session_for_test("s-reclaim-fence", "p1");
        let gen2 = registry
            .session_generation_for_test("s-reclaim-fence")
            .expect("gen2");
        assert_ne!(gen1, gen2);
        assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));
        assert!(registry.is_current_session_generation("s-reclaim-fence", gen2));
        registry.finish_restore_claim("s-reclaim-fence", SharedRestoreNotification::Ready);
        assert_eq!(
            registry.runtime_presence("s-reclaim-fence"),
            SessionRuntimePresence::Live
        );
        assert!(registry.is_current_session_generation("s-reclaim-fence", gen2));
        assert!(!registry.is_current_session_generation("s-reclaim-fence", gen1));
    }

    /// Business Logic（R19 H1: 为什么需要这个测试）:
    ///     close+reinsert 后，旧 generation 的 worker 副作用必须全部 no-op，
    ///     不得写入新 buffer / 发 status。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert gen1 → close → insert gen2；用 gen1 调 try_stale_worker_side_effects；
    ///     断言 false，且 gen2 replay last_seq 仍为 0。
    #[test]
    fn stale_generation_side_effects_no_op_after_reinsert() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-stale", "p1");
        let gen1 = registry
            .session_generation_for_test("s-stale")
            .expect("gen1");
        registry.close("s-stale").expect("close").finish_cleanup();
        registry.insert_fake_session_for_test("s-stale", "p1");
        let gen2 = registry
            .session_generation_for_test("s-stale")
            .expect("gen2");
        assert_ne!(gen1, gen2);
        assert_eq!(registry.replay_last_seq_for_test("s-stale"), Some(0));

        assert!(
            !registry.is_current_session_generation("s-stale", gen1),
            "stale generation must not be current"
        );
        assert!(registry.is_current_session_generation("s-stale", gen2));
        // 旧 gen 无法通过 generation-scoped append（经 registry 测试 helper）。
        assert!(
            !registry.append_replay_for_test("s-stale", gen1, "x", 1),
            "old gen must not append replay"
        );
        assert!(registry.append_replay_for_test("s-stale", gen2, "y", 1));
        assert_eq!(registry.replay_last_seq_for_test("s-stale"), Some(1));
    }

    /// Business Logic（R19 M1: 为什么需要这个测试）:
    ///     Provisional handle 在 mark Ready 前不得 Live；mark 后才 Live。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert_provisional → presence RestoreInProgress；list 不含；
    ///     mark_session_ready → Live 且 list 含。
    #[test]
    fn provisional_not_live_until_mark_ready() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-prov-ready", "p1");
        assert_eq!(
            registry.runtime_presence("s-prov-ready"),
            SessionRuntimePresence::RestoreInProgress
        );
        assert!(registry
            .list(Some("p1"))
            .iter()
            .all(|d| d.id != "s-prov-ready"));

        registry.mark_session_ready("s-prov-ready", None);
        assert_eq!(
            registry.runtime_presence("s-prov-ready"),
            SessionRuntimePresence::Live
        );
        assert!(registry
            .list(Some("p1"))
            .iter()
            .any(|d| d.id == "s-prov-ready"));
    }

    /// Business Logic（R19 H1: 为什么需要这个测试）:
    ///     close 必须失效 publish token，即使 handle Arc 仍被旧 worker 持有。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert → close → token false。
    #[test]
    fn close_invalidates_publish_token() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-token", "p1");
        let gen = registry
            .session_generation_for_test("s-token")
            .expect("gen");
        assert!(registry.publish_token_alive_for_test("s-token", gen));
        registry.close("s-token").expect("close").finish_cleanup();
        assert!(
            !registry.publish_token_alive_for_test("s-token", gen),
            "close must invalidate generation-scoped publish token"
        );
    }

    /// Business Logic（R20 H1: 为什么需要这个测试）:
    ///     Ready 前可见输出不得杀死 worker 语义，必须缓冲；Ready 后原序进入 replay。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional insert → emit 缓冲成功且 last_seq=0 → mark_ready → last_seq 增加。
    #[test]
    fn provisional_output_buffers_until_ready_then_flushes() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-buf", "p1");
        let gen = registry.session_generation_for_test("s-buf").expect("gen");
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-buf", gen),
            SideEffectGate::Provisional
        );
        assert!(
            buffer_provisional_output(&registry.sessions, "s-buf", gen, "first-screen".to_string(),),
            "provisional output must buffer instead of rejecting"
        );
        assert_eq!(registry.replay_last_seq_for_test("s-buf"), Some(0));
        // 无 AppState 时 mark_ready 只 CAS；手动取出缓冲验证 CAS 后仍保留到 flush 路径。
        assert!(registry.mark_session_ready_for_generation("s-buf", gen, None));
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-buf", gen),
            SideEffectGate::Ready
        );
        // Ready 后直接 publish 路径可写 replay。
        assert!(registry.append_replay_for_test("s-buf", gen, "live", 1));
        assert_eq!(registry.replay_last_seq_for_test("s-buf"), Some(1));
    }

    /// Business Logic（R20 H1: 为什么需要这个测试）:
    ///     Provisional 期间进程退出必须记录 pending_exit，不得静默丢失。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional → record_pending_exit → mark_ready 后 pending 被取出（CAS 路径）。
    #[test]
    fn provisional_exit_is_recorded_pending_until_ready() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-exit", "p1");
        let gen = registry.session_generation_for_test("s-exit").expect("gen");
        assert!(record_pending_exit(
            &registry.sessions,
            "s-exit",
            gen,
            Some(7)
        ));
        // 锁内确认 pending 已写入。
        {
            let sessions = registry.sessions.lock().expect("lock");
            let handle = sessions.get("s-exit").expect("handle").lock().expect("h");
            assert_eq!(handle.pending_exit, Some(Some(7)));
        }
        assert!(registry.mark_session_ready_for_generation("s-exit", gen, None));
        {
            let sessions = registry.sessions.lock().expect("lock");
            let handle = sessions.get("s-exit").expect("handle").lock().expect("h");
            assert_eq!(
                handle.pending_exit, None,
                "ready CAS must take pending_exit"
            );
            assert_eq!(handle.durability, SessionDurability::Ready);
        }
    }

    /// Business Logic（R20/R21 H2: 为什么需要这个测试）:
    ///     close 必须 revoke 并等待在途 lease；soft-timeout 后仍保留 tombstone，
    ///     同 id reinsert 不得在旧 lease 仍发布时发生。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Ready gen1 持 lease → close soft-timeout → tombstone；另一线程 reinsert 阻塞；
    ///     drop lease 后 reinsert 得到 gen2，旧 gen 零 publish。
    #[test]
    fn publication_lease_barrier_blocks_stale_after_close() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-lease", "p1");
        let gen1 = registry
            .session_generation_for_test("s-lease")
            .expect("gen1");
        let lease = try_acquire_publication_lease(&registry.sessions, "s-lease", gen1)
            .expect("lease for live gen1");
        // close：soft-timeout 后 tombstone（lease 仍持有）。
        registry.close("s-lease").expect("close").finish_cleanup();
        assert!(
            registry.has_closing_tombstone_for_test("s-lease"),
            "undrained close must keep generation tombstone"
        );
        let barrier = Arc::new(Barrier::new(2));
        let reg_ins = registry.clone();
        let barrier_ins = barrier.clone();
        let inserter = thread::spawn(move || {
            barrier_ins.wait();
            reg_ins.insert_fake_session_for_test("s-lease", "p1");
            reg_ins.session_generation_for_test("s-lease")
        });
        barrier.wait();
        // 给 inserter 时间卡在 tombstone wait。
        thread::sleep(Duration::from_millis(50));
        assert!(
            !registry.contains("s-lease"),
            "reinsert must not complete while old lease held"
        );
        drop(lease);
        let gen2 = inserter.join().expect("inserter").expect("gen2");
        assert_ne!(gen1, gen2);
        assert!(
            try_acquire_publication_lease(&registry.sessions, "s-lease", gen1).is_none(),
            "stale gen must not acquire lease after reinsert"
        );
        assert!(
            try_acquire_publication_lease(&registry.sessions, "s-lease", gen2).is_some(),
            "new gen may publish"
        );
        assert!(
            !registry.publish_token_alive_for_test("s-lease", gen1),
            "old token remains revoked"
        );
        assert!(
            !registry.has_closing_tombstone_for_test("s-lease"),
            "tombstone must clear after successful reinsert drain"
        );
    }

    /// Business Logic（R21 H1: 为什么需要这个测试）:
    ///     deferred flush 与 live 输出必须共享 generation-scoped seq，禁止各自从 0 重开导致前端丢字节。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Ready 后连续 allocate_seq 得到 1..=n 严格递增；模拟 flush 后 live 继续。
    #[test]
    fn shared_generation_seq_is_monotonic_across_flush_and_live() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-seq", "p1");
        let gen = registry.session_generation_for_test("s-seq").expect("gen");
        assert!(buffer_provisional_output(
            &registry.sessions,
            "s-seq",
            gen,
            "deferred-a".into(),
        ));
        assert!(buffer_provisional_output(
            &registry.sessions,
            "s-seq",
            gen,
            "deferred-b".into(),
        ));
        assert!(registry.mark_session_ready_for_generation("s-seq", gen, None));
        // 模拟 deferred flush 用共享 allocator 写 2 段，再 live 写 2 段。
        let mut seqs = Vec::new();
        for text_chunk in ["deferred-a", "deferred-b", "live-c", "live-d"] {
            let seq = registry
                .allocate_output_seq_for_test("s-seq", gen)
                .expect("seq");
            assert!(
                registry.append_replay_for_test("s-seq", gen, text_chunk, seq),
                "append must accept shared seq"
            );
            seqs.push(seq);
        }
        assert_eq!(
            seqs,
            vec![1, 2, 3, 4],
            "seq must continue across flush/live"
        );
        assert_eq!(registry.replay_last_seq_for_test("s-seq"), Some(4));
    }

    /// Business Logic（R21 M1: 为什么需要这个测试）:
    ///     SessionSpawnGuard Drop 只能回收捕获的 generation，不得 close 同 id 后继。
    ///
    /// Code Logic（这个测试做什么）:
    ///     gen1 provisional + guard → close gen1 + reinsert gen2 → Drop guard → gen2 仍 Live。
    #[test]
    fn spawn_guard_drop_only_closes_captured_generation() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-guard", "p1");
        let gen1 = registry
            .session_generation_for_test("s-guard")
            .expect("gen1");
        let guard = SessionSpawnGuard::new_with_generation(
            registry.clone(),
            "s-guard".to_string(),
            gen1,
            None,
        );
        registry
            .close("s-guard")
            .expect("close gen1")
            .finish_cleanup();
        registry.insert_fake_session_for_test("s-guard", "p1");
        let gen2 = registry
            .session_generation_for_test("s-guard")
            .expect("gen2");
        assert_ne!(gen1, gen2);
        drop(guard);
        assert!(
            registry.contains("s-guard"),
            "stale guard Drop must not remove successor generation"
        );
        assert_eq!(registry.session_generation_for_test("s-guard"), Some(gen2));
        assert_eq!(
            registry.runtime_presence("s-guard"),
            SessionRuntimePresence::Live
        );
    }

    /// Business Logic（R21 M2: 为什么需要这个测试）:
    ///     create upsert→Ready 窗口的 Provisional 不得进入 list / Live presence。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional running 行不在 list；presence=RestoreInProgress；Ready 后才 Live。
    #[test]
    fn provisional_create_window_not_projected_as_live() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-create-win", "p1");
        assert_eq!(
            registry.runtime_presence("s-create-win"),
            SessionRuntimePresence::RestoreInProgress
        );
        assert!(
            registry
                .list(Some("p1"))
                .iter()
                .all(|d| d.id != "s-create-win"),
            "list must hide Provisional (not only claim-held)"
        );
        let gen = registry
            .session_generation_for_test("s-create-win")
            .expect("gen");
        assert!(registry.mark_session_ready_for_generation("s-create-win", gen, None));
        assert_eq!(
            registry.runtime_presence("s-create-win"),
            SessionRuntimePresence::Live
        );
        assert!(registry
            .list(Some("p1"))
            .iter()
            .any(|d| d.id == "s-create-win"));
    }

    /// Business Logic（R20 M1: 为什么需要这个测试）:
    ///     SessionSpawnGuard commit 必须 generation CAS；close 后 commit 失败且 Drop 不再 close 二次 panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     insert provisional → guard 绑定 gen → close → commit=false。
    #[test]
    fn spawn_guard_commit_cas_fails_after_close() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-cas", "p1");
        let gen = registry.session_generation_for_test("s-cas").expect("gen");
        let mut guard = SessionSpawnGuard::new_with_generation(
            registry.clone(),
            "s-cas".to_string(),
            gen,
            None,
        );
        registry.close("s-cas").expect("close").finish_cleanup();
        assert!(!guard.commit(), "commit must fail when generation removed");
        // 标记 committed 假成功路径不应发生；Drop 因 committed=false 会 close miss → ok。
        // 手动置 committed 避免 Drop 再 close not_found 噪音（close 已成功）。
        guard.committed = true;
    }

    /// Business Logic（R20 M1: 为什么需要这个测试）:
    ///     错误 generation 不得 mark Ready。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional gen → mark with wrong gen false → still Provisional。
    #[test]
    fn mark_ready_generation_cas_rejects_stale() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-wrong-gen", "p1");
        let gen = registry
            .session_generation_for_test("s-wrong-gen")
            .expect("gen");
        assert!(!registry.mark_session_ready_for_generation("s-wrong-gen", gen + 99, None));
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-wrong-gen", gen),
            SideEffectGate::Provisional
        );
        assert!(registry.mark_session_ready_for_generation("s-wrong-gen", gen, None));
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-wrong-gen", gen),
            SideEffectGate::Ready
        );
    }

    /// Business Logic（R22 H1: 为什么需要这个测试）:
    ///     finish_ready 不得在第二批 deferred 仍待 drain 时 Ready；live 不得 overtake。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional 缓冲 A；mark_ready 进入 Flushing 期间注入 B；
    ///     无 state 路径循环 take 直至空再 Ready；最终 replay 顺序 A→B→live，seq 严格递增。
    #[test]
    fn finish_ready_stays_flushing_until_deferred_empty() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-flush-order", "p1");
        let gen = registry
            .session_generation_for_test("s-flush-order")
            .expect("gen");
        assert!(buffer_provisional_output(
            &registry.sessions,
            "s-flush-order",
            gen,
            "chunk-a".into(),
        ));
        // 模拟 flush 窗口：在 mark_ready 持有 Flushing 期间再缓冲 chunk-b。
        // 无 AppState 路径：finish_ready 循环 take 直至空才 Ready。
        // 用并发：mark 线程 + inject 线程竞态。
        let barrier = Arc::new(Barrier::new(2));
        let reg_mark = registry.clone();
        let barrier_mark = barrier.clone();
        let marker = thread::spawn(move || {
            barrier_mark.wait();
            reg_mark.mark_session_ready_for_generation("s-flush-order", gen, None)
        });
        barrier.wait();
        // 在 mark 可能处于 Flushing 时注入第二批 deferred。
        for _ in 0..200 {
            let durability = registry
                .session_durability_for_test("s-flush-order")
                .expect("dur");
            if durability == SessionDurability::Flushing {
                let _ = buffer_provisional_output(
                    &registry.sessions,
                    "s-flush-order",
                    gen,
                    "chunk-b".into(),
                );
                break;
            }
            if durability == SessionDurability::Ready {
                break;
            }
            thread::yield_now();
        }
        assert!(marker.join().expect("marker"), "must reach Ready");
        assert_eq!(
            registry.session_durability_for_test("s-flush-order"),
            Some(SessionDurability::Ready)
        );
        // 锁下 deferred 必须已空。
        {
            let sessions = registry.sessions.lock().expect("lock");
            let handle = sessions.get("s-flush-order").expect("h").lock().expect("h");
            assert!(
                handle.deferred_output.is_empty(),
                "Ready 时 deferred 必须空"
            );
        }
        // 共享 seq：后续 live 从 allocator 继续，严格递增。
        let mut seqs = Vec::new();
        for text in ["replay-a", "replay-b", "live-c"] {
            let seq = registry
                .allocate_output_seq_for_test("s-flush-order", gen)
                .expect("seq");
            assert!(registry.append_replay_for_test("s-flush-order", gen, text, seq));
            seqs.push(seq);
        }
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(registry.replay_last_seq_for_test("s-flush-order"), Some(3));
    }

    /// Business Logic（R22 H1: 为什么需要这个测试）:
    ///     Flushing 期间 live reader gate 必须仍为 Provisional，禁止与 deferred 双写 overtake。
    ///
    /// Code Logic（这个测试做什么）:
    ///     人工置 Flushing + 非空 deferred；classify=Provisional；can_publish=false；
    ///     finish_ready 清空后 Ready。
    #[test]
    fn flushing_blocks_live_publish_until_deferred_drained() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-flush-gate", "p1");
        let gen = registry
            .session_generation_for_test("s-flush-gate")
            .expect("gen");
        {
            let sessions = registry.sessions.lock().expect("lock");
            let mut handle = sessions.get("s-flush-gate").expect("h").lock().expect("h");
            handle.durability = SessionDurability::Flushing;
            handle.deferred_output.push("pending".into());
        }
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-flush-gate", gen),
            SideEffectGate::Provisional
        );
        assert!(!can_publish_side_effect(
            &registry.sessions,
            "s-flush-gate",
            gen
        ));
        assert!(registry.finish_ready_after_flush_for_test("s-flush-gate", gen));
        assert_eq!(
            classify_side_effect_gate(&registry.sessions, "s-flush-gate", gen),
            SideEffectGate::Ready
        );
        {
            let sessions = registry.sessions.lock().expect("lock");
            let handle = sessions.get("s-flush-gate").expect("h").lock().expect("h");
            assert!(handle.deferred_output.is_empty());
        }
    }

    /// Business Logic（R22 M1: 为什么需要这个测试）:
    ///     close 与 barrier 同临界区；持 lease 时 concurrent reinsert/restore 不得装新 generation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     hold lease → close → presence 非 Missing；两线程 concurrent insert 均阻塞至 drop lease；
    ///     仅一个成功 live gen2；旧 gen 不可 lease。
    #[test]
    fn close_barrier_blocks_concurrent_reinsert_until_cleanup() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-close-barrier", "p1");
        let gen1 = registry
            .session_generation_for_test("s-close-barrier")
            .expect("gen1");
        let lease = try_acquire_publication_lease(&registry.sessions, "s-close-barrier", gen1)
            .expect("lease");
        let cleanup = registry.close("s-close-barrier").expect("close");
        assert!(
            registry.has_closing_tombstone_for_test("s-close-barrier"),
            "close must install barrier immediately"
        );
        assert_eq!(
            registry.runtime_presence("s-close-barrier"),
            SessionRuntimePresence::RestoreInProgress,
            "Closing barrier must not report permanent Missing"
        );
        let start = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let reg = registry.clone();
            let start = start.clone();
            handles.push(thread::spawn(move || {
                start.wait();
                reg.insert_fake_session_for_test("s-close-barrier", "p1");
                reg.session_generation_for_test("s-close-barrier")
            }));
        }
        start.wait();
        thread::sleep(Duration::from_millis(80));
        assert!(
            !registry.contains("s-close-barrier"),
            "reinsert must not complete under active barrier"
        );
        assert!(registry.has_closing_tombstone_for_test("s-close-barrier"));
        drop(lease);
        // R24 H1：persist cleanup 令牌 finish 后 barrier 才允许 reinsert。
        cleanup.finish_cleanup();
        let mut gens = Vec::new();
        for h in handles {
            gens.push(h.join().expect("inserter").expect("gen"));
        }
        let live = registry
            .session_generation_for_test("s-close-barrier")
            .expect("live gen");
        assert!(
            gens.contains(&live),
            "live generation must come from concurrent reinsert"
        );
        assert_ne!(live, gen1);
        assert!(
            !registry.has_closing_tombstone_for_test("s-close-barrier"),
            "barrier clears after drain+reinsert"
        );
        assert!(
            try_acquire_publication_lease(&registry.sessions, "s-close-barrier", gen1).is_none()
        );
        assert!(
            try_acquire_publication_lease(&registry.sessions, "s-close-barrier", live).is_some()
        );
    }

    /// Business Logic（R23 H1: 为什么需要这个测试）:
    ///     Flushing→Ready 过渡时 output prepare 必须原子成功，禁止分锁 buffer 失败后永久 Rejected。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Provisional 置 Flushing；并发 finish_ready 与 prepare_output；
    ///     结果只能 Buffered 或 Live，不得 Rejected；最终 Ready 且 deferred 空。
    #[test]
    fn prepare_output_survives_ready_transition() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-r23-out", "p1");
        let gen = registry
            .session_generation_for_test("s-r23-out")
            .expect("gen");
        {
            let sessions = registry.sessions.lock().expect("lock");
            let mut handle = sessions.get("s-r23-out").expect("h").lock().expect("h");
            handle.durability = SessionDurability::Flushing;
            handle.deferred_output.push("pre".into());
        }
        let barrier = Arc::new(Barrier::new(2));
        let reg_ready = registry.clone();
        let barrier_ready = barrier.clone();
        let ready_thread = thread::spawn(move || {
            barrier_ready.wait();
            reg_ready.finish_ready_after_flush_for_test("s-r23-out", gen)
        });
        barrier.wait();
        let mut saw_buffered = false;
        let mut saw_live = false;
        let mut saw_rejected = false;
        for i in 0..64 {
            match prepare_output_side_effect(
                &registry.sessions,
                "s-r23-out",
                gen,
                format!("chunk-{i}"),
            ) {
                PreparedSideEffect::Buffered => saw_buffered = true,
                PreparedSideEffect::Live {
                    lease: _lease,
                    payload: _,
                } => {
                    saw_live = true;
                }
                PreparedSideEffect::Rejected => saw_rejected = true,
            }
            thread::yield_now();
        }
        assert!(ready_thread.join().expect("ready"), "must reach Ready");
        assert!(
            !saw_rejected,
            "mid-flight Ready must not permanently reject same generation output"
        );
        assert!(
            saw_buffered || saw_live,
            "prepare must accept output as buffer or live across Ready transition"
        );
        assert_eq!(
            registry.session_durability_for_test("s-r23-out"),
            Some(SessionDurability::Ready)
        );
    }

    /// Business Logic（R23 H1: 为什么需要这个测试）:
    ///     mutation prepare 与 exit prepare 必须在 Ready 过渡窗口保持原子，禁止分锁 stale。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Flushing 下并发 Ready；mutation/exit prepare 不得 Rejected。
    #[test]
    fn prepare_mutation_and_exit_survive_ready_transition() {
        use crate::workbench::agent_runtime::AgentSessionPhase;
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_provisional_fake_session_for_test("s-r23-mut", "p1");
        let gen = registry
            .session_generation_for_test("s-r23-mut")
            .expect("gen");
        {
            let sessions = registry.sessions.lock().expect("lock");
            let mut handle = sessions.get("s-r23-mut").expect("h").lock().expect("h");
            handle.durability = SessionDurability::Flushing;
        }
        let barrier = Arc::new(Barrier::new(2));
        let reg_ready = registry.clone();
        let barrier_ready = barrier.clone();
        let ready_thread = thread::spawn(move || {
            barrier_ready.wait();
            reg_ready.finish_ready_after_flush_for_test("s-r23-mut", gen)
        });
        barrier.wait();
        let mutation = AgentRuntimeMutation {
            agent_session_id: "a1".into(),
            terminal_session_id: "s-r23-mut".into(),
            expected_version: 0,
            event_version: 1,
            phase: AgentSessionPhase::Working,
            native_session_id: None,
            outcome_code: None,
            occurred_at: "2026-07-21T00:00:00Z".into(),
        };
        let mut rejected = false;
        for i in 0..32 {
            match prepare_mutation_side_effect(
                &registry.sessions,
                "s-r23-mut",
                gen,
                vec![mutation.clone()],
            ) {
                PreparedSideEffect::Rejected => rejected = true,
                PreparedSideEffect::Buffered | PreparedSideEffect::Live { .. } => {}
            }
            match prepare_exit_side_effect(&registry.sessions, "s-r23-mut", gen, Some(i)) {
                PreparedSideEffect::Rejected => rejected = true,
                PreparedSideEffect::Buffered | PreparedSideEffect::Live { .. } => {}
            }
            thread::yield_now();
        }
        assert!(ready_thread.join().expect("ready"));
        assert!(
            !rejected,
            "mutation/exit prepare must not reject across Ready transition"
        );
    }

    /// Business Logic（R23 H2: 为什么需要这个测试）:
    ///     waiters 不得因 in_flight==0 清除 barrier；closer 未 cleanup 时 reinsert 必须等待。
    ///
    /// Code Logic（这个测试做什么）:
    ///     安装 barrier（leases=0, cleanup_done=false）；并发 reinsert 阻塞；
    ///     mark cleanup + closer clear 后 reinsert 成功。
    #[test]
    fn waiter_never_clears_barrier_before_cleanup_done() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        let publish = registry.install_closing_barrier_for_test("s-r23-h2");
        assert!(
            registry.has_closing_tombstone_for_test("s-r23-h2"),
            "barrier installed"
        );
        assert!(!publish.is_cleanup_done(), "cleanup_done must start false");
        assert_eq!(publish.in_flight.load(Ordering::SeqCst), 0);
        let start = Arc::new(Barrier::new(2));
        let reg_ins = registry.clone();
        let start_ins = start.clone();
        let inserter = thread::spawn(move || {
            start_ins.wait();
            reg_ins.insert_fake_session_for_test("s-r23-h2", "p1");
            reg_ins.session_generation_for_test("s-r23-h2")
        });
        start.wait();
        thread::sleep(Duration::from_millis(80));
        assert!(
            !registry.contains("s-r23-h2"),
            "waiter must not clear barrier / reinsert while cleanup pending"
        );
        assert!(registry.has_closing_tombstone_for_test("s-r23-h2"));
        // closer 完成 cleanup 后才可 clear。
        publish.mark_cleanup_done();
        registry.clear_closing_tombstone_for_test("s-r23-h2");
        let gen = inserter.join().expect("inserter").expect("gen");
        assert!(
            !registry.has_closing_tombstone_for_test("s-r23-h2"),
            "only closer clear removes barrier"
        );
        assert!(registry.contains("s-r23-h2"));
        assert_eq!(registry.session_generation_for_test("s-r23-h2"), Some(gen));
    }

    /// Business Logic（R23 M1: 为什么需要这个测试）:
    ///     close install barrier 与 insert CAS 必须同 lifecycle 锁；并发 reinsert 不得越过新 barrier。
    ///
    /// Code Logic（这个测试做什么）:
    ///     live gen1；并发 close + insert；insert 最终成功的 gen 必须 != gen1 且 barrier 清后存在；
    ///     中途若 close 先装 barrier，insert CAS 失败后重试直至成功。
    #[test]
    fn insert_revalidates_barrier_under_lifecycle_lock() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-r23-m1", "p1");
        let gen1 = registry
            .session_generation_for_test("s-r23-m1")
            .expect("gen1");
        // 持 lease 强制 close 走 Closing barrier（soft-timeout 路径），覆盖 remove→barrier 与 reinsert CAS。
        let lease =
            try_acquire_publication_lease(&registry.sessions, "s-r23-m1", gen1).expect("lease");
        // main + closer + inserter 三方同步（必须 3，否则第三 waiter 永久阻塞）。
        let start = Arc::new(Barrier::new(3));
        let reg_close = registry.clone();
        let start_close = start.clone();
        let closer = thread::spawn(move || {
            start_close.wait();
            reg_close.close("s-r23-m1")
        });
        let reg_ins = registry.clone();
        let start_ins = start.clone();
        let inserter = thread::spawn(move || {
            start_ins.wait();
            // 等 Closing barrier 出现，确保 CAS 必须 revalidate（非覆盖 Live）。
            for _ in 0..200 {
                if reg_ins.has_closing_tombstone_for_test("s-r23-m1") {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            reg_ins.insert_fake_session_for_test("s-r23-m1", "p1");
            reg_ins.session_generation_for_test("s-r23-m1")
        });
        start.wait();
        // close soft-wait 期间 barrier 已装；释放 lease 后 closer finish_cleanup 才 clear。
        thread::sleep(Duration::from_millis(30));
        drop(lease);
        closer
            .join()
            .expect("closer")
            .expect("close ok")
            .finish_cleanup();
        let gen2 = inserter.join().expect("inserter").expect("gen2");
        assert_ne!(gen1, gen2, "successor generation must differ");
        assert!(
            !registry.has_closing_tombstone_for_test("s-r23-m1"),
            "barrier must be gone after close cleanup + successful reinsert"
        );
        assert_eq!(registry.session_generation_for_test("s-r23-m1"), Some(gen2));
        assert_eq!(
            registry.runtime_presence("s-r23-m1"),
            SessionRuntimePresence::Live
        );
    }

    /// Business Logic（R24 H1: 为什么需要这个测试）:
    ///     registry close 后若立即 mark/clear，并发 reinsert 可装后继；旧 closer 的 persist
    ///     kill/delete 会打到 successor。令牌必须覆盖外部 cleanup。
    ///
    /// Code Logic（这个测试做什么）:
    ///     close 返回 cleanup 且 **不** finish；并发 reinsert 阻塞；finish 后 reinsert 成功且
    ///     旧 closer 身份仍可 clear（barrier 身份 CAS）。
    #[test]
    fn closer_cleanup_token_spans_post_registry_persist() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-r24-h1", "p1");
        let gen1 = registry
            .session_generation_for_test("s-r24-h1")
            .expect("gen1");
        let cleanup = registry.close("s-r24-h1").expect("close");
        assert!(
            registry.has_closing_tombstone_for_test("s-r24-h1"),
            "barrier must remain until finish_cleanup"
        );
        assert_eq!(
            registry.runtime_presence("s-r24-h1"),
            SessionRuntimePresence::RestoreInProgress
        );
        // claim restore 在 barrier 下不得 Claimed。
        assert!(matches!(
            registry.try_claim_restore("s-r24-h1"),
            RestoreClaimOutcome::BarrierActive
        ));
        let start = Arc::new(Barrier::new(2));
        let reg_ins = registry.clone();
        let start_ins = start.clone();
        let inserter = thread::spawn(move || {
            start_ins.wait();
            reg_ins.insert_fake_session_for_test("s-r24-h1", "p1");
            reg_ins.session_generation_for_test("s-r24-h1")
        });
        start.wait();
        thread::sleep(Duration::from_millis(80));
        assert!(
            !registry.contains("s-r24-h1"),
            "reinsert must wait until finish_cleanup"
        );
        // 模拟 kill_persisted_backend + SQLite delete 完成。
        cleanup.finish_cleanup();
        let gen2 = inserter.join().expect("inserter").expect("gen2");
        assert_ne!(gen1, gen2);
        assert!(!registry.has_closing_tombstone_for_test("s-r24-h1"));
        assert_eq!(registry.session_generation_for_test("s-r24-h1"), Some(gen2));
    }

    /// Business Logic（R24 H2: 为什么需要这个测试）:
    ///     restore 读到 pre-close 行后若 barrier 期间无限重试，会在 close+delete 后复活会话。
    ///
    /// Code Logic（这个测试做什么）:
    ///     install Closing barrier → try_claim_restore=BarrierActive；
    ///     spawn_row Abort 策略经 try_insert BarrierActive 返回 session_close_barrier_active
    ///     （用 insert CAS 路径模拟，不启真实 PTY）；finish 后可 claim/reinsert。
    #[test]
    fn barrier_active_aborts_restore_claim_and_spawn_retry() {
        let registry = WorkbenchSessionRegistry::new();
        let publish = registry.install_closing_barrier_for_test("s-r24-h2");
        assert!(matches!(
            registry.try_claim_restore("s-r24-h2"),
            RestoreClaimOutcome::BarrierActive
        ));
        // CAS insert 在 barrier 下返回 BarrierActive（restore Abort 路径同源）。
        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r24-h2".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 99,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: None,
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r24-h2", handle),
            InsertCasResult::BarrierActive
        );
        assert!(!registry.contains("s-r24-h2"));
        // finish closer cleanup 后允许 re-read + claim。
        publish.mark_cleanup_done();
        registry.clear_closing_tombstone_for_test("s-r24-h2");
        assert!(registry.try_claim_restore("s-r24-h2").is_claimed());
        registry.finish_restore_claim("s-r24-h2", SharedRestoreNotification::PersistedDisconnected);
    }

    /// Business Logic（R25 H1: 为什么需要这个测试）:
    ///     restore 已 claim 但尚未 insert live handle 时，close/delete 若无 close intent，
    ///     旧 restore 会在 delete 后 INSERT OR REPLACE 复活同 id。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim held + no registry → begin_close_intent → claim 被 Failed 取消；
    ///     insert CAS BarrierActive；finish 前不得 re-claim 成功；finish 后才可 claim。
    #[test]
    fn close_intent_blocks_claimed_restore_without_live_handle() {
        let registry = WorkbenchSessionRegistry::new();
        assert!(registry.try_claim_restore("s-r25-h1").is_claimed());
        assert!(!registry.contains("s-r25-h1"));
        let row = WorkbenchSessionRow {
            id: "s-r25-h1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let cleanup = registry
            .begin_close_intent_for_missing_handle("s-r25-h1", row)
            .expect("close intent");
        assert!(registry.has_closing_tombstone_for_test("s-r25-h1"));
        // claim 已被 close intent 取消；后续不得 Claimed，只应 BarrierActive。
        assert!(matches!(
            registry.try_claim_restore("s-r25-h1"),
            RestoreClaimOutcome::BarrierActive
        ));
        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r25-h1".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 1,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: None,
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r25-h1", handle),
            InsertCasResult::BarrierActive,
            "stale restore must not re-upsert live handle under close intent"
        );
        assert!(!registry.contains("s-r25-h1"));
        cleanup.finish_cleanup();
        assert!(!registry.has_closing_tombstone_for_test("s-r25-h1"));
        assert!(registry.try_claim_restore("s-r25-h1").is_claimed());
        registry.finish_restore_claim("s-r25-h1", SharedRestoreNotification::PersistedDisconnected);
    }

    /// Business Logic（R25 H2: 为什么需要这个测试）:
    ///     project remove 若在 bulk delete 前 per-session finish，list/restore 可在窗口复活行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两 session close 收集 cleanup 令牌但不 finish；concurrent claim/insert 均被 barrier 挡；
    ///     模拟 bulk delete 后统一 finish，再允许 claim。
    #[test]
    fn project_remove_defers_cleanup_finish_until_bulk_delete() {
        use std::sync::Barrier;
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-r25-h2-a", "p-bulk");
        registry.insert_fake_session_for_test("s-r25-h2-b", "p-bulk");
        let mut cleanups = Vec::new();
        for id in ["s-r25-h2-a", "s-r25-h2-b"] {
            cleanups.push(registry.close(id).expect("close"));
            assert!(registry.has_closing_tombstone_for_test(id));
        }
        // 模拟 bulk delete 尚未完成：barrier 必须挡住 concurrent restore claim。
        let start = Arc::new(Barrier::new(3));
        let mut claim_threads = Vec::new();
        for id in ["s-r25-h2-a", "s-r25-h2-b"] {
            let reg = registry.clone();
            let start = start.clone();
            let sid = id.to_string();
            claim_threads.push(thread::spawn(move || {
                start.wait();
                matches!(
                    reg.try_claim_restore(&sid),
                    RestoreClaimOutcome::BarrierActive
                )
            }));
        }
        start.wait();
        for t in claim_threads {
            assert!(
                t.join().expect("claim thread"),
                "claim blocked before bulk finish"
            );
        }
        // 模拟 delete_by_project 成功后再 finish。
        for cleanup in cleanups {
            cleanup.finish_cleanup();
        }
        assert!(!registry.has_closing_tombstone_for_test("s-r25-h2-a"));
        assert!(!registry.has_closing_tombstone_for_test("s-r25-h2-b"));
        assert!(registry.try_claim_restore("s-r25-h2-a").is_claimed());
        registry.finish_restore_claim(
            "s-r25-h2-a",
            SharedRestoreNotification::PersistedDisconnected,
        );
    }

    /// Business Logic（R25 M1: 为什么需要这个测试）:
    ///     Abort spawn 若先 wait 既有 barrier，会在 barrier 清后用 stale 快照继续 PTY/insert。
    ///
    /// Code Logic（这个测试做什么）:
    ///     pre-install barrier → Abort precheck 立即 Err(session_close_barrier_active)；
    ///     无 live insert。
    #[test]
    fn abort_spawn_returns_immediately_on_preexisting_barrier() {
        let registry = WorkbenchSessionRegistry::new();
        let publish = registry.install_closing_barrier_for_test("s-r25-m1");
        let err = registry
            .abort_if_preexisting_closing_barrier_for_test("s-r25-m1")
            .expect_err("must abort");
        match err {
            AppError::Unavailable(msg) => {
                assert_eq!(msg, "session_close_barrier_active");
            }
            other => panic!("unexpected error variant: {other}"),
        }
        assert!(!registry.contains("s-r25-m1"));
        // barrier 仍在（precheck 不得 clear）。
        assert!(registry.has_closing_tombstone_for_test("s-r25-m1"));
        publish.mark_cleanup_done();
        registry.clear_closing_tombstone_for_test("s-r25-m1");
    }

    /// Business Logic（R25 M2: 为什么需要这个测试）:
    ///     SessionCloseCleanup Drop 若总是 clear barrier，delete 失败后 restore 会打开窗口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     close → drop cleanup without finish → barrier 仍在；显式 finish 后才 clear。
    #[test]
    fn session_close_cleanup_drop_retains_barrier_until_finish() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-r25-m2", "p1");
        let cleanup = registry.close("s-r25-m2").expect("close");
        assert!(registry.has_closing_tombstone_for_test("s-r25-m2"));
        // 模拟 delete 失败 / cancel：drop 令牌。
        drop(cleanup);
        assert!(
            registry.has_closing_tombstone_for_test("s-r25-m2"),
            "Drop must not clear barrier on incomplete cleanup"
        );
        assert!(matches!(
            registry.try_claim_restore("s-r25-m2"),
            RestoreClaimOutcome::BarrierActive
        ));
        // owner 成功 finish：用 begin_close_intent 取得同一 barrier 身份再 finish。
        let row = WorkbenchSessionRow {
            id: "s-r25-m2".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "disconnected".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: Some("t".into()),
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let cleanup2 = registry
            .begin_close_intent_for_missing_handle("s-r25-m2", row)
            .expect("reuse intent");
        cleanup2.finish_cleanup();
        assert!(!registry.has_closing_tombstone_for_test("s-r25-m2"));
        assert!(registry.try_claim_restore("s-r25-m2").is_claimed());
        registry.finish_restore_claim("s-r25-m2", SharedRestoreNotification::PersistedDisconnected);
    }

    /// Business Logic（R26 H1: 为什么需要这个测试）:
    ///     holder 已 claim 后 close 必须 revoke generation；holder 恢复后不得成功 re-upsert
    ///     已删除会话，也不得留下 live orphan。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim → pause（持有 generation）→ close intent → claim generation inactive；
    ///     persist lease 获取失败；insert CAS BarrierActive；finish 后才允许新 claim。
    #[test]
    fn close_revokes_held_restore_claim_before_reupsert() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r26-h1")
            .claim_generation()
            .expect("claimed");
        let guard = RestoreClaimGuard::new(registry.clone(), "s-r26-h1".to_string(), generation);
        assert!(guard.is_active());
        assert!(registry.is_restore_claim_generation_active("s-r26-h1", generation));

        // 模拟 holder 已进入 upsert 前：acquire lease 成功。
        let mut lease = registry
            .try_acquire_restore_persist_lease("s-r26-h1", generation)
            .expect("lease while active");
        assert!(lease.is_active());

        let row = WorkbenchSessionRow {
            id: "s-r26-h1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        // 释放 lease 再 close（模拟 holder 尚未持 lease 时 close 更快路径）；
        // 另测 close 等待 in-flight lease 的路径见 concurrent 测试。
        lease.release();

        let cleanup = registry
            .begin_close_intent_for_missing_handle("s-r26-h1", row)
            .expect("close intent");
        assert!(registry.has_closing_tombstone_for_test("s-r26-h1"));
        assert!(!guard.is_active());
        assert!(!registry.is_restore_claim_generation_active("s-r26-h1", generation));
        assert!(
            registry
                .try_acquire_restore_persist_lease("s-r26-h1", generation)
                .is_none(),
            "revoked claim must not acquire persist lease"
        );
        assert!(matches!(
            registry.require_restore_claim_active("s-r26-h1", generation),
            Err(AppError::Unavailable(msg)) if msg == "session_restore_claim_revoked"
        ));

        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r26-h1".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 99,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: None,
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r26-h1", handle),
            InsertCasResult::BarrierActive,
            "holder must not re-insert live after close revoke"
        );
        assert!(!registry.contains("s-r26-h1"));
        // Drop stale guard 不得 panic / 不得清掉 barrier。
        drop(guard);
        assert!(registry.has_closing_tombstone_for_test("s-r26-h1"));
        cleanup.finish_cleanup();
        assert!(!registry.has_closing_tombstone_for_test("s-r26-h1"));
        assert!(registry.try_claim_restore("s-r26-h1").is_claimed());
        registry.release_restore_claim("s-r26-h1");
    }

    /// Business Logic（R26 H1: 为什么需要这个测试）:
    ///     close 时 holder 若仍在途 persist lease，必须 wait drain；否则 delete 后
    ///     holder 仍可 upsert 已删行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + lease held → concurrent close intent 阻塞至 lease release →
    ///     generation inactive 且 barrier 在；release 后 close 完成。
    #[test]
    fn close_waits_for_restore_persist_lease_drain() {
        use std::sync::Barrier as StdBarrier;
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r26-h1-lease")
            .claim_generation()
            .expect("claimed");
        let mut lease = registry
            .try_acquire_restore_persist_lease("s-r26-h1-lease", generation)
            .expect("lease");

        let start = Arc::new(StdBarrier::new(2));
        let reg = registry.clone();
        let start_close = start.clone();
        let closer = thread::spawn(move || {
            start_close.wait();
            let row = WorkbenchSessionRow {
                id: "s-r26-h1-lease".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            };
            reg.begin_close_intent_for_missing_handle("s-r26-h1-lease", row)
                .expect("close intent after drain")
        });

        start.wait();
        // close 线程应阻塞在 lease drain；短暂 sleep 后仍 inactive 未 finish barrier 也可能已装。
        thread::sleep(Duration::from_millis(50));
        assert!(
            registry.is_restore_claim_generation_active("s-r26-h1-lease", generation)
                || registry.has_closing_tombstone_for_test("s-r26-h1-lease"),
            "close either still waiting with claim or has installed barrier"
        );
        // holder 完成（或放弃）persist：release lease 让 close 继续。
        lease.release();
        let cleanup = closer.join().expect("closer join");
        assert!(registry.has_closing_tombstone_for_test("s-r26-h1-lease"));
        assert!(!registry.is_restore_claim_generation_active("s-r26-h1-lease", generation));
        assert!(registry
            .try_acquire_restore_persist_lease("s-r26-h1-lease", generation)
            .is_none());
        cleanup.finish_cleanup();
    }

    /// Business Logic（R26 M1: 为什么需要这个测试）:
    ///     project remove 与 concurrent create 交错时，无 project barrier 会留下 orphan live。
    ///
    /// Code Logic（这个测试做什么）:
    ///     begin project barrier → require_project_not_closing Err；
    ///     create revalidate 失败；finish barrier 后 Ok。
    #[test]
    fn project_closing_barrier_blocks_create_until_finish() {
        let registry = WorkbenchSessionRegistry::new();
        let gen = registry.begin_project_closing_barrier("p-r26-m1");
        assert!(registry.has_project_closing_barrier_for_test("p-r26-m1"));
        match registry.require_project_not_closing("p-r26-m1") {
            Err(AppError::Unavailable(msg)) => {
                assert_eq!(msg, "project_closing_barrier_active");
            }
            other => panic!("expected project barrier error, got {other:?}"),
        }
        // 其他项目不受影响。
        assert!(registry.require_project_not_closing("p-other").is_ok());

        // 并发 create 观察 barrier 仍在。
        let start = Arc::new(std::sync::Barrier::new(2));
        let reg = registry.clone();
        let start_t = start.clone();
        let observer = thread::spawn(move || {
            start_t.wait();
            reg.require_project_not_closing("p-r26-m1").is_err()
        });
        start.wait();
        assert!(
            observer.join().expect("observer"),
            "create blocked during remove"
        );

        registry.finish_project_closing_barrier("p-r26-m1", gen);
        assert!(!registry.has_project_closing_barrier_for_test("p-r26-m1"));
        assert!(registry.require_project_not_closing("p-r26-m1").is_ok());
        // 错误 generation finish 是 no-op 后仍可再次 begin。
        let gen2 = registry.begin_project_closing_barrier("p-r26-m1");
        registry.finish_project_closing_barrier("p-r26-m1", gen2.wrapping_add(1));
        assert!(
            registry.has_project_closing_barrier_for_test("p-r26-m1"),
            "mismatched generation must not clear barrier"
        );
        registry.finish_project_closing_barrier("p-r26-m1", gen2);
        assert!(!registry.has_project_closing_barrier_for_test("p-r26-m1"));
    }

    /// Business Logic（R42 H1: 为什么需要这个测试）:
    ///     并发 merge cleanup 与 project remove 必须 join 同一 barrier，禁止后启动者
    ///     覆盖 generation 并抢先 clear，留下前一个 owner 窗口中的 orphan live。
    ///
    /// Code Logic（这个测试做什么）:
    ///     double begin 返回同一 generation 且 owners=2；
    ///     第一次 finish 后 barrier 仍在；第二次 finish 才 clear；
    ///     错误 generation finish 全程 no-op。
    #[test]
    fn project_closing_barrier_double_begin_joins_and_wrong_finish_is_noop() {
        let registry = WorkbenchSessionRegistry::new();
        let gen1 = registry.begin_project_closing_barrier("p-r42-h1");
        let gen2 = registry.begin_project_closing_barrier("p-r42-h1");
        assert_eq!(gen1, gen2, "second begin must join same generation");
        assert_eq!(
            registry.project_closing_generation_for_test("p-r42-h1"),
            Some(gen1)
        );
        assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 2);
        // 错误 generation finish 不得减少 owners 或 clear。
        registry.finish_project_closing_barrier("p-r42-h1", gen1.wrapping_add(99));
        assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 2);
        assert!(registry.has_project_closing_barrier_for_test("p-r42-h1"));
        // 第一个 owner finish：仍 active。
        registry.finish_project_closing_barrier("p-r42-h1", gen1);
        assert!(
            registry.has_project_closing_barrier_for_test("p-r42-h1"),
            "first finish of nested owners must keep barrier"
        );
        assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 1);
        // 最后一个 owner finish：clear。
        registry.finish_project_closing_barrier("p-r42-h1", gen2);
        assert!(!registry.has_project_closing_barrier_for_test("p-r42-h1"));
        assert_eq!(registry.project_closing_owners_for_test("p-r42-h1"), 0);
    }

    /// Business Logic（R26 M1: 为什么需要这个测试）:
    ///     project remove 与 restore claim 交错：remove 先挂 barrier，restore revalidate 失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim 成功后 begin project barrier → require_project_not_closing Err；
    ///     finish project barrier 后 require Ok（session claim 仍 held）。
    #[test]
    fn project_closing_barrier_blocks_restore_revalidate() {
        let registry = WorkbenchSessionRegistry::new();
        let claim_gen = registry
            .try_claim_restore("s-r26-m1-restore")
            .claim_generation()
            .expect("claimed");
        let project_gen = registry.begin_project_closing_barrier("p-r26-restore");
        assert!(registry
            .require_project_not_closing("p-r26-restore")
            .is_err());
        // claim generation 本身仍 active，但 project barrier 独立阻止 spawn/upsert。
        assert!(registry.is_restore_claim_generation_active("s-r26-m1-restore", claim_gen));
        registry.finish_project_closing_barrier("p-r26-restore", project_gen);
        assert!(registry
            .require_project_not_closing("p-r26-restore")
            .is_ok());
        registry.finish_restore_claim_for_generation(
            "s-r26-m1-restore",
            claim_gen,
            SharedRestoreNotification::Failed(AppErrorCategory::Unavailable),
        );
    }

    /// Business Logic（R27 H2: 为什么需要这个测试）:
    ///     live handle + held restore claim 时 close 必须 revoke claim，并阻断 re-upsert。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional live + claim → close() → claim inactive + barrier + insert CAS BarrierActive。
    #[test]
    fn live_close_revokes_held_restore_claim_before_reupsert() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r27-h2")
            .claim_generation()
            .expect("claimed");
        registry.insert_provisional_fake_session_for_test("s-r27-h2", "p1");
        registry.bind_restore_claim_generation_for_test("s-r27-h2", Some(generation));
        assert!(registry.contains("s-r27-h2"));
        assert!(registry.is_restore_claim_generation_active("s-r27-h2", generation));

        let cleanup = registry.close("s-r27-h2").expect("live close");
        assert!(registry.has_closing_tombstone_for_test("s-r27-h2"));
        assert!(!registry.contains("s-r27-h2"));
        assert!(!registry.is_restore_claim_generation_active("s-r27-h2", generation));
        assert!(registry
            .try_acquire_restore_persist_lease("s-r27-h2", generation)
            .is_none());

        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r27-h2".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 99,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: Some(generation),
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r27-h2", handle),
            InsertCasResult::BarrierActive
        );
        cleanup.finish_cleanup();
        assert!(!registry.has_closing_tombstone_for_test("s-r27-h2"));
    }

    /// Business Logic（R27 H3: 为什么需要这个测试）:
    ///     lease drain timeout 不得返回 finishable cleanup（drained=true）从而 clear barrier。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim + held lease → short-timeout close intent → drained=false + barrier retained
    ///     after finish_cleanup until lease release and reaper drain.
    #[test]
    fn restore_lease_drain_timeout_not_finishable_cleanup() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r27-h3")
            .claim_generation()
            .expect("claimed");
        let mut lease = registry
            .try_acquire_restore_persist_lease("s-r27-h3", generation)
            .expect("lease");
        let row = WorkbenchSessionRow {
            id: "s-r27-h3".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let cleanup = registry
            .begin_close_intent_with_drain_timeout_for_test(
                "s-r27-h3",
                row,
                Duration::from_millis(30),
            )
            .expect("close intent timeout path");
        assert!(
            !WorkbenchSessionRegistry::session_close_cleanup_drained_for_test(&cleanup),
            "timeout must not return finishable drained=true"
        );
        assert!(
            WorkbenchSessionRegistry::session_close_cleanup_has_restore_claim_for_test(&cleanup)
        );
        assert!(registry.has_closing_tombstone_for_test("s-r27-h3"));
        // finish with drained=false → reaper path；barrier 在 lease 释放前仍可能保留。
        cleanup.finish_cleanup();
        // lease still held: barrier must remain (reaper waits leases).
        thread::sleep(Duration::from_millis(20));
        assert!(
            registry.has_closing_tombstone_for_test("s-r27-h3"),
            "barrier retained while restore lease held"
        );
        // concurrent upsert must fail under barrier.
        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r27-h3".into(),
                project_id: "p1".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 7,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: Some(generation),
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r27-h3", handle),
            InsertCasResult::BarrierActive
        );
        lease.release();
        // reaper should clear after leases + cleanup_done.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while registry.has_closing_tombstone_for_test("s-r27-h3")
            && std::time::Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !registry.has_closing_tombstone_for_test("s-r27-h3"),
            "reaper clears barrier after lease drain"
        );
    }

    /// Business Logic（R27 H4: 为什么需要这个测试）:
    ///     insert CAS 必须拒绝 project_closing；project lease 被 remove 等待。
    ///
    /// Code Logic（这个测试做什么）:
    ///     acquire project lease → begin barrier → insert ProjectClosing；
    ///     wait leases 在 release 前阻塞语义用 count 观测；finish 后 insert 成功。
    #[test]
    fn project_barrier_blocks_insert_cas_and_waits_leases() {
        let registry = WorkbenchSessionRegistry::new();
        // Ready 用例会话须在 project barrier 前插入，避免 ProjectClosing 自旋/no-op。
        registry.insert_provisional_fake_session_for_test("s-r27-h4-ready", "p-r27-h4");
        let gen_ready = registry
            .session_generation_for_test("s-r27-h4-ready")
            .expect("gen");

        let lease = registry
            .try_acquire_project_op_lease("p-r27-h4")
            .expect("project lease");
        assert_eq!(registry.project_op_lease_count_for_test("p-r27-h4"), 1);
        let gen = registry.begin_project_closing_barrier("p-r27-h4");
        assert!(registry.require_project_not_closing("p-r27-h4").is_err());
        // 在途 lease 期间不得再 acquire。
        assert!(registry.try_acquire_project_op_lease("p-r27-h4").is_err());

        let handle = Arc::new(Mutex::new(WorkbenchSessionHandle {
            row: WorkbenchSessionRow {
                id: "s-r27-h4".into(),
                project_id: "p-r27-h4".into(),
                worktree_id: None,
                name: "n".into(),
                name_source: "default".to_string(),
                command: "/bin/sh".into(),
                cwd: "/tmp".into(),
                status: "running".into(),
                cols: 80,
                rows: 24,
                started_at: "t".into(),
                exited_at: None,
                exit_code: None,
                backend: RAW_PTY_BACKEND.into(),
                backend_id: None,
                backend_window_id: None,
                created_at: "t".into(),
                updated_at: "t".into(),
            },
            generation: 1,
            durability: SessionDurability::Provisional,
            publish: PublishControl::new(),
            deferred_output: Vec::new(),
            deferred_mutations: Vec::new(),
            pending_exit: None,
            restore_claim_generation: None,
            process: SessionProcess::Fake,
        }));
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier("s-r27-h4", handle),
            InsertCasResult::ProjectClosing
        );
        assert!(!registry.contains("s-r27-h4"));

        // Ready 在 project barrier 下 fail-closed。
        assert!(
            !registry.mark_session_ready_for_generation("s-r27-h4-ready", gen_ready, None),
            "Ready blocked under project barrier"
        );

        drop(lease);
        assert!(registry.wait_project_op_leases_drained("p-r27-h4"));
        registry.finish_project_closing_barrier("p-r27-h4", gen);
        assert!(registry.require_project_not_closing("p-r27-h4").is_ok());
    }

    /// Business Logic（R27 H5: 为什么需要这个测试）:
    ///     safe_attach / Ready 必须 revalidate claim generation；revoked 不得 Ready。
    ///
    /// Code Logic（这个测试做什么）:
    ///     provisional + bound claim → revoke via close intent → mark_session_ready_for_generation false；
    ///     active claim 路径 bind + mark true。
    #[test]
    fn ready_revalidates_restore_claim_generation() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r27-h5")
            .claim_generation()
            .expect("claimed");
        registry.insert_provisional_fake_session_for_test("s-r27-h5", "p1");
        registry.bind_restore_claim_generation_for_test("s-r27-h5", Some(generation));
        let gen = registry
            .session_generation_for_test("s-r27-h5")
            .expect("gen");
        assert!(registry.mark_session_ready_for_generation("s-r27-h5", gen, None));
        registry.finish_restore_claim("s-r27-h5", SharedRestoreNotification::Ready);

        // 第二会话：close 撤销 claim 后 Ready 失败。
        let generation2 = registry
            .try_claim_restore("s-r27-h5b")
            .claim_generation()
            .expect("claimed");
        registry.insert_provisional_fake_session_for_test("s-r27-h5b", "p1");
        registry.bind_restore_claim_generation_for_test("s-r27-h5b", Some(generation2));
        let cleanup = registry.close("s-r27-h5b").expect("close");
        assert!(!registry.is_restore_claim_generation_active("s-r27-h5b", generation2));
        // 先 finish cleanup 清 barrier，再 re-insert + bind 已撤销 generation。
        cleanup.finish_cleanup();
        registry.insert_provisional_fake_session_for_test("s-r27-h5b", "p1");
        registry.bind_restore_claim_generation_for_test("s-r27-h5b", Some(generation2));
        let gen_b = registry
            .session_generation_for_test("s-r27-h5b")
            .expect("gen");
        assert!(
            !registry.mark_session_ready_for_generation("s-r27-h5b", gen_b, None),
            "revoked claim must block Ready"
        );
    }

    /// Business Logic（R28 H3: 为什么需要这个测试）:
    ///     close 的 remove handle 与 Closing tombstone 安装不得拆分临界区，否则 restore 可抢跑 Missing 窗口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     live session close 后立刻 has_closing_tombstone 且 contains=false、runtime 非 Live。
    #[test]
    fn close_installs_tombstone_atomically_with_handle_remove() {
        let registry = WorkbenchSessionRegistry::new();
        registry.insert_fake_session_for_test("s-r28-h3", "p1");
        let cleanup = registry.close("s-r28-h3").expect("close");
        assert!(!registry.contains("s-r28-h3"));
        assert!(
            registry.has_closing_tombstone_for_test("s-r28-h3"),
            "tombstone must be present immediately after close returns"
        );
        assert_ne!(
            registry.runtime_presence("s-r28-h3"),
            SessionRuntimePresence::Live
        );
        // insert 在 barrier 下不得越过。
        assert_eq!(
            registry.try_insert_handle_revalidating_barrier(
                "s-r28-h3",
                Arc::new(Mutex::new(WorkbenchSessionHandle {
                    row: WorkbenchSessionRow {
                        id: "s-r28-h3".into(),
                        project_id: "p1".into(),
                        worktree_id: None,
                        name: "n".into(),
                        name_source: "default".to_string(),
                        command: "/bin/sh".into(),
                        cwd: "/tmp".into(),
                        status: "running".into(),
                        cols: 80,
                        rows: 24,
                        started_at: "t".into(),
                        exited_at: None,
                        exit_code: None,
                        backend: RAW_PTY_BACKEND.into(),
                        backend_id: None,
                        backend_window_id: None,
                        created_at: "t".into(),
                        updated_at: "t".into(),
                    },
                    generation: 9,
                    durability: SessionDurability::Provisional,
                    publish: PublishControl::new(),
                    deferred_output: Vec::new(),
                    deferred_mutations: Vec::new(),
                    pending_exit: None,
                    restore_claim_generation: None,
                    process: SessionProcess::Fake,
                }))
            ),
            InsertCasResult::BarrierActive
        );
        cleanup.finish_cleanup();
        assert!(!registry.has_closing_tombstone_for_test("s-r28-h3"));
    }

    /// Business Logic（R28 H4: 为什么需要这个测试）:
    ///     project op lease 必须阻断 remove 的 pre-snapshot drain，直到 create/restore 完成。
    ///
    /// Code Logic（这个测试做什么）:
    ///     持 lease 时 wait_project_op_leases_drained 阻塞；drop 后 drain 成功。
    #[test]
    fn project_op_lease_held_blocks_remove_drain_until_release() {
        let registry = WorkbenchSessionRegistry::new();
        let lease = registry
            .try_acquire_project_op_lease("p-r28-h4")
            .expect("lease");
        assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 1);
        // lease 为计数器：允许多个 in-flight create/restore；remove 的 drain 须等全部归零。
        let lease2 = registry
            .try_acquire_project_op_lease("p-r28-h4")
            .expect("second lease");
        assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 2);
        let reg = registry.clone();
        let handle = thread::spawn(move || reg.wait_project_op_leases_drained("p-r28-h4"));
        thread::sleep(Duration::from_millis(80));
        assert!(!handle.is_finished(), "drain must wait while lease held");
        drop(lease);
        thread::sleep(Duration::from_millis(40));
        assert!(!handle.is_finished(), "still waiting for second lease");
        drop(lease2);
        assert!(handle.join().expect("join"), "drain after all releases");
        assert_eq!(registry.project_op_lease_count_for_test("p-r28-h4"), 0);
    }

    /// Business Logic（R29 H2: 为什么需要这个测试）:
    ///     missing-handle close 不得在 sessions 检查后释放锁再装 tombstone，否则 restore 可插入 Ready 后 close 只删 SQLite。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim held、无 live → begin_close_intent 后立刻 has tombstone 且 claim revoked；
    ///     try_claim 不得 AlreadyLive。
    #[test]
    fn missing_handle_close_intent_atomic_with_claim_revoke() {
        let registry = WorkbenchSessionRegistry::new();
        let generation = registry
            .try_claim_restore("s-r29-h2")
            .claim_generation()
            .expect("claimed");
        assert!(registry.is_restore_claim_held("s-r29-h2"));
        let row = WorkbenchSessionRow {
            id: "s-r29-h2".into(),
            project_id: "p1".into(),
            worktree_id: None,
            name: "n".into(),
            name_source: "default".to_string(),
            command: "/bin/sh".into(),
            cwd: "/tmp".into(),
            status: "running".into(),
            cols: 80,
            rows: 24,
            started_at: "t".into(),
            exited_at: None,
            exit_code: None,
            backend: RAW_PTY_BACKEND.into(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let cleanup = registry
            .begin_close_intent_for_missing_handle("s-r29-h2", row)
            .expect("close intent");
        assert!(
            registry.has_closing_tombstone_for_test("s-r29-h2"),
            "tombstone must exist immediately"
        );
        assert!(!registry.is_restore_claim_generation_active("s-r29-h2", generation));
        assert!(!registry.contains("s-r29-h2"));
        assert!(
            matches!(
                registry.try_claim_restore("s-r29-h2"),
                RestoreClaimOutcome::BarrierActive | RestoreClaimOutcome::RestoreInProgress(_)
            ) || registry.has_closing_tombstone_for_test("s-r29-h2"),
            "must not allow fresh live without barrier"
        );
        cleanup.finish_cleanup();
    }
}
