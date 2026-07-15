//! claude_sessions — Workbench 终端 Claude session 搜索的扫描 + 索引 + 文件监听核心模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在 Workbench 某个 worktree 终端工作时，经常需要找回之前某个 Claude Code 会话继续对话。
//!     Claude Code 把每次会话 transcript 写到 `~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`，
//!     但文件名只有无意义的 UUID，用户无法检索。本模块扫描当前 worktree 对应目录下所有 jsonl，
//!     在内存构建可搜索索引（标题/user 文本/assistant 文本/最近消息），并用 notify 监听目录变化增量更新，
//!     让前端能在「当前 worktree 范围」内按标题或对话内容搜索目标 session，选中后一键 resume。
//!
//! Code Logic（这个模块做什么）:
//!     - `ClaudeIndexBudget` + 有界 `read_line_bounded`：限制文件数/单文件/行长/总字节/session 字符，
//!       候选按 mtime desc + path asc 排序后截断，diagnostics 暴露 stable reason token。
//!     - `build_session_index(_with_budget)`：单文件有界流式解析，提取 ClaudeSessionIndex（标题=lastPrompt
//!       回退首条 user、user/assistant 文本、最近 20 条消息、元信息），过滤 slash/bash，2 秒超时跳过。
//!     - `scan_worktree_sessions(_with_budget|_at)`：扫描 worktree 对应 encoded-cwd 目录，组装
//!       带 truncated/diagnostics 的 WorktreeSessionIndex。
//!     - `search_sessions` / `search_sessions_result`：spec 2.3 搜索；后者包装 SessionSearchResult DTO。
//!     - `ensure_worktree_session_index_scanned`（async）：singleflight + spawn_blocking 扫描 +
//!       notify 文件监听（debounce 500ms）；监听失败降级为每次重扫。
//!     - watcher 将 create/modify/delete/rename/uncertain 映射为 index upsert/remove/rename/bounded rescan；
//!       owner 侧 `ClaudeSessionWatcherRuntime` 聚合 watcher+cancel+handles；项目移除/shutdown 先 dispose。
//!     - `decode_session_search_response_body`：混部 dual-decode（v2 对象或 legacy 数组）。

use crate::cc::collector::claude_projects_dir;
use crate::state::AppState;
use crate::workbench::claude_path::encode_claude_project_path;
use chrono::Utc;
use notify::event::{ModifyKind, RemoveKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;

/// 单文件解析超时（2 秒）。活跃 session 可能几 MB，超时则跳过避免阻塞扫描。
const SINGLE_FILE_PARSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Diagnostics status：完整成功。
pub const DIAG_STATUS_OK: &str = "ok";
/// Diagnostics status：至少一个预算触发截断。
pub const DIAG_STATUS_TRUNCATED: &str = "truncated";
/// Diagnostics status：旧端/未知，无法提供预算诊断。
pub const DIAG_STATUS_UNAVAILABLE: &str = "unavailable";

/// 单次 Claude session 索引扫描的资源预算。
///
/// Business Logic（为什么需要这个结构体）:
///     用户 worktree 下可能堆积成千上万 jsonl、超大 transcript 或异常长行；无界扫描会拖垮
///     sidecar 内存与 tokio 响应。预算把扫描约束在可预测的资源上限内，并在 diagnostics 暴露截断原因。
///
/// Code Logic（这个结构体做什么）:
///     持有文件数、单文件字节、jsonl 行字节、总字节与 session 文本 Unicode scalar 上限；Default 给出生产默认。
#[derive(Debug, Clone, Copy)]
pub struct ClaudeIndexBudget {
    /// 最多索引的 jsonl 文件数（按 mtime desc + path asc 排序后截断）。
    pub max_files: usize,
    /// 单文件 metadata 长度上限；超过则整文件跳过。
    pub max_file_bytes: u64,
    /// 单行 jsonl 最大读取字节；超过则丢弃该行并记 reason。
    pub max_jsonl_line_bytes: usize,
    /// 一次扫描累计读取字节上限；下一个文件会超则停止。
    pub max_total_bytes: u64,
    /// 单个 session 的 title+user+assistant 文本 Unicode scalar 总预算。
    pub max_session_chars: usize,
}

impl Default for ClaudeIndexBudget {
    /// 生产默认预算。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     搜索/监听热路径与测试需要统一的默认上限，避免调用方到处硬编码。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 max_files=10_000、max_file_bytes=64MiB、max_jsonl_line_bytes=1MiB、
    ///     max_total_bytes=512MiB、max_session_chars=1_000_000。
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_file_bytes: 64 * 1024 * 1024,
            max_jsonl_line_bytes: 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_session_chars: 1_000_000,
        }
    }
}

/// 单文件解析时的预算命中结果。
///
/// Business Logic（为什么需要这个结构体）:
///     扫描层需要把「行过长 / session 文本截断 / 文件过大」等 reason 汇总到 diagnostics。
///
/// Code Logic（这个结构体做什么）:
///     记录 reasons、实际读取字节，以及是否因 max_file_bytes 整文件跳过。
#[derive(Debug, Clone, Default)]
pub struct SessionFileBudgetOutcome {
    pub reasons: Vec<String>,
    pub bytes_read: u64,
    pub skipped_entire_file: bool,
}

/// 搜索结果诊断信息（稳定 reason token）。
///
/// Business Logic（为什么需要这个结构体）:
///     前端/远端需要知道结果是否因预算截断，以及扫描计数，便于展示「部分结果」而非静默丢数。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化；status ∈ {ok, truncated, unavailable}；reasons 如 max_files 等稳定 token。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchDiagnostics {
    pub status: String,
    pub reasons: Vec<String>,
    pub files_considered: u64,
    pub files_indexed: u64,
    pub bytes_read: u64,
}

impl SessionSearchDiagnostics {
    /// 构造「完整成功」诊断。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     未触发任何预算时需要统一的 ok 诊断对象。
    ///
    /// Code Logic（这个函数做什么）:
    ///     status=ok，空 reasons，填入计数。
    pub fn ok(files_considered: u64, files_indexed: u64, bytes_read: u64) -> Self {
        Self {
            status: DIAG_STATUS_OK.to_string(),
            reasons: Vec::new(),
            files_considered,
            files_indexed,
            bytes_read,
        }
    }

    /// 构造「预算截断」诊断。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     任一预算命中时前端需展示 truncated 与 reason 列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     status=truncated，reasons 去重保序。
    pub fn truncated(
        reasons: Vec<String>,
        files_considered: u64,
        files_indexed: u64,
        bytes_read: u64,
    ) -> Self {
        Self {
            status: DIAG_STATUS_TRUNCATED.to_string(),
            reasons: dedupe_reasons(reasons),
            files_considered,
            files_indexed,
            bytes_read,
        }
    }

    /// 构造「旧端不可用」诊断。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     混部解码旧 `Vec<SessionSearchHit>` 时没有预算信息，需显式 unavailable。
    ///
    /// Code Logic（这个函数做什么）:
    ///     status=unavailable，空 reasons，计数全 0。
    pub fn unavailable() -> Self {
        Self {
            status: DIAG_STATUS_UNAVAILABLE.to_string(),
            reasons: Vec::new(),
            files_considered: 0,
            files_indexed: 0,
            bytes_read: 0,
        }
    }
}

/// 搜索 API 返回 DTO：命中列表 + 截断标记 + 诊断。
///
/// Business Logic（为什么需要这个结构体）:
///     仅返回 `Vec<SessionSearchHit>` 无法表达预算截断；新 DTO 让本机/远端/混部客户端统一消费。
///
/// Code Logic（这个结构体做什么）:
///     camelCase；items 为命中，truncated 与 diagnostics 反映索引扫描侧预算。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResult {
    pub items: Vec<SessionSearchHit>,
    pub truncated: bool,
    pub diagnostics: SessionSearchDiagnostics,
}

/// 把旧版 `Vec<SessionSearchHit>` 包装成新 DTO（无预算信息）。
///
/// Business Logic（为什么需要这个函数）:
///     新客户端对旧服务端时，body 仍是数组；需要合成 truncated=false + unavailable 诊断，避免解析失败。
///
/// Code Logic（这个函数做什么）:
///     包装 items，truncated=false，diagnostics=unavailable，计数 0。
pub fn synthesize_legacy_session_search_result(
    items: Vec<SessionSearchHit>,
) -> SessionSearchResult {
    SessionSearchResult {
        items,
        truncated: false,
        diagnostics: SessionSearchDiagnostics::unavailable(),
    }
}

/// 双形态解码搜索响应：新 DTO 对象或旧数组。
///
/// Business Logic（为什么需要这个函数）:
///     混部期间远端可能返回 v2 对象或 legacy 数组，客户端必须都能解析。
///
/// Code Logic（这个函数做什么）:
///     先 `SessionSearchResult`，失败再 `Vec<SessionSearchHit>` 并 synthesize；都失败返回 serde 错误。
pub fn decode_session_search_response_body(
    bytes: &[u8],
) -> Result<SessionSearchResult, serde_json::Error> {
    match serde_json::from_slice::<SessionSearchResult>(bytes) {
        Ok(v) => Ok(v),
        Err(first_err) => match serde_json::from_slice::<Vec<SessionSearchHit>>(bytes) {
            Ok(items) => Ok(synthesize_legacy_session_search_result(items)),
            Err(_) => Err(first_err),
        },
    }
}

/// 去重 reason 列表并保持首次出现顺序。
///
/// Business Logic（为什么需要这个函数）:
///     同一 reason 可能在多文件重复出现，diagnostics 只需稳定去重列表。
///
/// Code Logic（这个函数做什么）:
///     O(n) 线性去重。
fn dedupe_reasons(reasons: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for r in reasons {
        if !out.iter().any(|x| x == &r) {
            out.push(r);
        }
    }
    out
}

/// 按 Unicode scalar 预算截断字符串（只在 char 边界切断）。
///
/// Business Logic（为什么需要这个函数）:
///     max_session_chars 以 Unicode scalar 计数；截断必须落在 char 边界，避免 panic/半字符。
///
/// Code Logic（这个函数做什么）:
///     chars().count() ≤ max 时原样 to_string，否则 take(max_chars) collect。
pub fn truncate_to_char_budget(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// 在 title → user → assistant 顺序上分配 session 字符预算并截断。
///
/// Business Logic（为什么需要这个函数）:
///     超大对话全文会撑爆内存索引；优先保留 title，再 user，最后 assistant。
///
/// Code Logic（这个函数做什么）:
///     顺序扣减 remaining，返回截断后三元组与是否发生截断。
fn apply_session_char_budget(
    title: String,
    user_text: String,
    assistant_text: String,
    max_session_chars: usize,
) -> (String, String, String, bool) {
    let mut remaining = max_session_chars;
    let mut truncated = false;

    let title_chars = title.chars().count();
    let title_out = if title_chars <= remaining {
        remaining = remaining.saturating_sub(title_chars);
        title
    } else {
        truncated = true;
        let out = truncate_to_char_budget(&title, remaining);
        remaining = 0;
        out
    };

    let user_chars = user_text.chars().count();
    let user_out = if user_chars <= remaining {
        remaining = remaining.saturating_sub(user_chars);
        user_text
    } else {
        truncated = true;
        let out = truncate_to_char_budget(&user_text, remaining);
        remaining = 0;
        out
    };

    let assistant_chars = assistant_text.chars().count();
    let assistant_out = if assistant_chars <= remaining {
        assistant_text
    } else {
        truncated = true;
        truncate_to_char_budget(&assistant_text, remaining)
    };

    (title_out, user_out, assistant_out, truncated)
}

/// 从 BufRead 精确 drain 到并包含下一个 '\\n'（不越过下一行）。
///
/// Business Logic（为什么需要这个函数）:
///     超长行丢弃时必须停在换行符，否则会吞掉后续合法 jsonl 行。
///
/// Code Logic（这个函数做什么）:
///     用 fill_buf/consume 只消耗到首个 '\\n'（含），返回丢弃字节数；永不整行大分配。
fn drain_until_newline<R: BufRead>(reader: &mut R) -> std::io::Result<u64> {
    let mut discarded = 0u64;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(discarded);
        }
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let n = pos + 1;
            reader.consume(n);
            discarded = discarded.saturating_add(n as u64);
            return Ok(discarded);
        }
        let n = buf.len();
        reader.consume(n);
        discarded = discarded.saturating_add(n as u64);
    }
}

/// 有界读取一行：最多分配 max_bytes+1，超长行丢弃内容并 drain 至换行。
///
/// Business Logic（为什么需要这个函数）:
///     异常/恶意 jsonl 可能含数 MiB 无换行长行；`BufRead::lines()` 会整行分配导致 OOM。
///
/// Code Logic（这个函数做什么）:
///     逐字节/缓冲累积至 max_bytes+1 或 '\\n'；超长则 drain_until_newline 丢弃剩余；
///     返回 (Option<String UTF-8 lossy 行>, 读取字节数, 是否超长)。
fn read_line_bounded<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<(Option<String>, u64, bool)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut bytes_read: u64 = 0;
    let cap = max_bytes.saturating_add(1);

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            // 本缓冲内含换行：只吃到换行
            let take_n = pos + 1;
            if buf.len() + take_n > cap {
                // 加上本段会超过 cap → 超长行
                // 先不把超长内容放进 buf；consume 到换行并记 overflow
                reader.consume(take_n);
                bytes_read = bytes_read.saturating_add(take_n as u64);
                return Ok((None, bytes_read, true));
            }
            buf.extend_from_slice(&chunk[..take_n]);
            reader.consume(take_n);
            bytes_read = bytes_read.saturating_add(take_n as u64);
            break;
        }
        // 本缓冲无换行
        let available = chunk.len();
        if buf.len() >= cap {
            // 已超 cap 且仍无换行：丢弃本缓冲并继续 drain
            reader.consume(available);
            bytes_read = bytes_read.saturating_add(available as u64);
            let extra = drain_until_newline(reader)?;
            bytes_read = bytes_read.saturating_add(extra);
            return Ok((None, bytes_read, true));
        }
        let room = cap - buf.len();
        if available <= room {
            buf.extend_from_slice(chunk);
            reader.consume(available);
            bytes_read = bytes_read.saturating_add(available as u64);
            // 继续读更多
        } else {
            // 只填到 cap，然后 drain 剩余至换行
            buf.extend_from_slice(&chunk[..room]);
            reader.consume(room);
            bytes_read = bytes_read.saturating_add(room as u64);
            let extra = drain_until_newline(reader)?;
            bytes_read = bytes_read.saturating_add(extra);
            return Ok((None, bytes_read, true));
        }
    }

    if bytes_read == 0 && buf.is_empty() {
        return Ok((None, 0, false));
    }
    // buf 可能以 \n 结尾；cap 溢出已在上面 return
    if buf.len() > max_bytes {
        // 整行（含 \n）刚好落在 max+1：仍视为超长
        return Ok((None, bytes_read, true));
    }
    if buf.ends_with(b"\n") {
        buf.pop();
        if buf.ends_with(b"\r") {
            buf.pop();
        }
    }
    let line = String::from_utf8_lossy(&buf).into_owned();
    Ok((Some(line), bytes_read, false))
}

/// 把 reason 追加到列表（若尚不存在）。
///
/// Business Logic（为什么需要这个函数）:
///     多处预算命中路径需要稳定地记录 reason token。
///
/// Code Logic（这个函数做什么）:
///     若不存在则 push。
fn push_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|r| r == reason) {
        reasons.push(reason.to_string());
    }
}

/// 搜索命中上下文片段单侧字符数。
const PREVIEW_SNIPPET_RADIUS: usize = 30;

/// 搜索命中上下文片段最大数量。
const PREVIEW_SNIPPET_MAX: usize = 3;

/// 默认搜索结果上限（spec 2.3）。
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// recent_messages 保留的最大条数（spec 3.1）。
pub const RECENT_MESSAGES_MAX: usize = 20;

/// 文件监听应用层 debounce 间隔（spec 5.1：500ms）。
///
/// notify::Config::with_poll_interval 只控制 poll backend 轮询频率，不是事件 debounce。
/// Claude 高频写 jsonl 会触发大量事件，每个都 spawn_blocking 重扫性能很差，故在此再做一层
/// 应用层 debounce：采用 **leading + trailing** 策略——
/// - leading：距上次重扫 ≥ 该间隔的事件立即处理（响应快）；
/// - trailing：距上次重扫 < 该间隔的事件累积，安排「最后一次事件后该间隔」的兜底重扫任务，
///   保证写入静默后最后一次内容必定被重扫到（不漏内容）。每次新 trailing 事件 abort 旧的延迟任务，
///   使 trailing 永远是「最后一次事件后该间隔」。
const DEBOUNCE_INTERVAL: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// 数据结构（spec 3.1 / 3.2）
// ---------------------------------------------------------------------------

/// 单条最近消息（用于 preview 面板）。
///
/// Business Logic（为什么需要这个结构体）:
///     用户选中某条 session 后需要在 preview 面板看到最近对话的纯文本摘要，role 标签帮助区分 user/assistant。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化对齐前端 SessionPreviewMessage；text 是已过滤 thinking/tool_use 的纯文本。
///     同时派生 Deserialize，因为它是 SessionPreview 的字段，需要随 preview 响应一起反序列化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentMessage {
    pub role: String,
    pub text: String,
    pub timestamp: String,
}

/// 单个 Claude session 的索引数据。
///
/// Business Logic（为什么需要这个结构体）:
///     搜索需要在内存中同时比对标题、user 文本、assistant 文本，preview 需要 recent_messages，
///     排序需要首末活动时间。一个结构体聚合避免反复读 jsonl。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化对齐前端 SessionPreview；user_text/assistant_text 是拼接后供子串搜索用的全文。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionIndex {
    pub session_id: String,
    pub title: String,
    pub transcript_path: PathBuf,
    pub first_activity_at: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub user_text: String,
    pub assistant_text: String,
    pub recent_messages: Vec<RecentMessage>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
}

/// 一个 worktree path 的完整索引。
///
/// Business Logic（为什么需要这个结构体）:
///     搜索范围限定在当前 active worktree，需要把该 worktree 下所有 session 聚合成一张表，
///     并记录 encoded_cwd 与扫描时间供文件监听回调复用；同时携带预算截断诊断供搜索 DTO 透传。
///
/// Code Logic（这个结构体做什么）:
///     不 Serialize（仅供内存索引使用）；sessions 以 session_id 为 key 便于单文件增量更新；
///     truncated + diagnostics 描述最近一次全量/预算扫描的结果。
#[derive(Debug, Clone)]
pub struct WorktreeSessionIndex {
    // 保留 worktree_path 便于调试与未来扩展（当前索引内部未直接读取）。
    #[allow(dead_code)]
    pub worktree_path: PathBuf,
    pub encoded_cwd: String,
    pub sessions: HashMap<String, ClaudeSessionIndex>,
    pub last_scan_at: String,
    /// 最近一次扫描是否触发任一预算截断。
    pub truncated: bool,
    /// 最近一次扫描诊断（status/reasons/计数）。
    pub diagnostics: SessionSearchDiagnostics,
}

/// 搜索命中结果（spec 3.2）。
///
/// Business Logic（为什么需要这个结构体）:
///     前端列表项需要知道命中在哪些字段，preview_snippets 提供命中上下文片段用于展示。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化对齐前端 SessionSearchHit；title_hit/user_hit/assistant_hit 标记命中字段。
///     同时派生 Deserialize，因为 remote shortcut 命令需要反序列化远端设备的搜索响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchHit {
    pub session_id: String,
    pub title: String,
    pub title_hit: bool,
    pub user_hit: bool,
    pub assistant_hit: bool,
    pub first_activity_at: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub preview_snippets: Vec<String>,
}

/// Claude session preview 数据（给前端 preview 面板用）。
///
/// Business Logic（为什么需要这个结构体）:
///     用户在搜索结果列表选中某条 session 后，preview 面板需要展示该 session 的标题、
///     cwd、git 分支、首末活动时间、消息总数以及最近对话消息（role + 纯文本 + 时间戳），
///     帮助用户在 resume 前确认这是目标会话。该数据由 jsonl transcript 解析得到。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化对齐前端 SessionPreview；recent_messages 是已过滤 thinking/tool_use 的纯文本消息。
///     同时派生 Deserialize，因为 remote shortcut 命令需要反序列化远端设备的响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPreview {
    pub session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub first_activity_at: String,
    pub last_activity_at: String,
    pub message_count: u32,
    pub recent_messages: Vec<RecentMessage>,
}

/// 把 ClaudeSessionIndex 转成 SessionPreview（给前端 preview 面板）。
///
/// Business Logic（为什么需要这个函数）:
///     内存索引中的 ClaudeSessionIndex 既用于搜索也用于 preview，但 preview 面板不需要
///     user_text/assistant_text 全文与 transcript_path 等内部字段，需要一个精简投影。
///
/// Code Logic（这个函数做什么）:
///     从 ClaudeSessionIndex 映射出 SessionPreview，丢弃 user_text/assistant_text/transcript_path，
///     保留 preview 面板需要的元信息与 recent_messages（克隆）。
pub fn to_session_preview(index: &ClaudeSessionIndex) -> SessionPreview {
    SessionPreview {
        session_id: index.session_id.clone(),
        title: index.title.clone(),
        cwd: index.cwd.clone(),
        git_branch: index.git_branch.clone(),
        first_activity_at: index.first_activity_at.clone(),
        last_activity_at: index.last_activity_at.clone(),
        message_count: index.message_count,
        recent_messages: index.recent_messages.clone(),
    }
}

// ---------------------------------------------------------------------------
// jsonl 解析（spec 2.2）
// ---------------------------------------------------------------------------

/// jsonl 单行的宽松反序列化结构（未知字段忽略，缺失字段用 default）。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude Code jsonl 行字段会随版本演进变化，本模块只关心 type/message/timestamp/cwd/gitBranch/lastPrompt
///     等提取相关字段，其余一律忽略，避免字段变更导致反序列化失败。
///
/// Code Logic（这个结构体做什么）:
///     用 serde default 容错；last_prompt 仅 last-prompt 行有。
#[derive(Debug, Default, Deserialize)]
struct JsonlLine {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(default, rename = "lastPrompt")]
    last_prompt: Option<String>,
}

/// message 字段的宽松结构（仅关心 role 与 content）。
#[derive(Debug, Default, Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Option<serde_json::Value>,
}

/// 从 message.content 提取纯文本（spec 2.2）。
///
/// Business Logic（为什么需要这个函数）:
///     user 输入可能是纯字符串，也可能是 content blocks 数组；assistant 回复通常是含 text/thinking/tool_use
///     的数组。提取规则需要统一：string 直接返回，array 只取 type==text 的 text 字段，忽略 thinking/tool_use。
///
/// Code Logic（这个函数做什么）:
///     - content 是 String → trim 后非空返回；
///     - content 是 Array → 遍历 object 元素，type=="text" 的取 text 字段拼接，trim 后非空返回；
///     - 其它（null/number/object 等）→ None。
pub fn extract_text_from_content(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_json::Value::Array(items) => {
            let mut buf = String::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    if obj.get("type").and_then(|v| v.as_str()) == Some("text") {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            if !buf.is_empty() {
                                buf.push('\n');
                            }
                            buf.push_str(text);
                        }
                    }
                }
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

/// 判断文本是否为需要过滤的命令（slash `/` 或 bash `!` 开头）。
///
/// Business Logic（为什么需要这个函数）:
///     user 文本里 `/clear`、`!ls` 这类是 Claude Code 的命令而非真正对话内容，纳入搜索会污染结果。
///
/// Code Logic（这个函数做什么）:
///     trim 后以 `/` 或 `!` 开头返回 true。
fn is_command_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    trimmed.starts_with('/') || trimmed.starts_with('!')
}

/// 从单个 jsonl 文件构建一个 ClaudeSessionIndex（默认预算，兼容旧调用/测试）。
///
/// Business Logic（为什么需要这个函数）:
///     既有单测与增量刷新路径只需索引本身，不关心预算 outcome；保留无 budget 签名。
///
/// Code Logic（这个函数做什么）:
///     委托 `build_session_index_with_budget(path, &Default::default())` 并丢弃 outcome。
pub fn build_session_index(path: &Path) -> Option<ClaudeSessionIndex> {
    build_session_index_with_budget(path, &ClaudeIndexBudget::default()).map(|(idx, _)| idx)
}

/// 按预算从单个 jsonl 构建 ClaudeSessionIndex。
///
/// Business Logic（为什么需要这个函数）:
///     一个 jsonl 文件 = 一次 Claude 会话的完整 transcript。搜索索引需要从每个文件提取标题、
///     user 文本、assistant 文本、最近 20 条消息和首末活动时间；同时遵守文件/行/字符预算避免 OOM。
///
/// Code Logic（这个函数做什么）:
///     1. metadata 超过 max_file_bytes → None + skipped_entire_file + reason max_file_bytes；
///     2. 有界 read_line_bounded 逐行，超长行丢弃并记 max_jsonl_line_bytes；
///     3. 2 秒超时跳过整文件；
///     4. 组装后按 title→user→assistant 顺序应用 max_session_chars；
///     5. recent_messages 仍最多 20。
pub fn build_session_index_with_budget(
    path: &Path,
    budget: &ClaudeIndexBudget,
) -> Option<(ClaudeSessionIndex, SessionFileBudgetOutcome)> {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let mut outcome = SessionFileBudgetOutcome::default();

    let meta = std::fs::metadata(path)
        .inspect_err(|err| {
            tracing::warn!(
                "读取 Claude session transcript metadata 失败 {:?}: {err}",
                path
            );
        })
        .ok()?;
    if meta.len() > budget.max_file_bytes {
        push_reason(&mut outcome.reasons, "max_file_bytes");
        outcome.skipped_entire_file = true;
        return None;
    }

    let file = std::fs::File::open(path)
        .inspect_err(|err| {
            tracing::warn!("打开 Claude session transcript 失败 {:?}: {err}", path);
        })
        .ok()?;

    let mut reader = BufReader::new(file);
    let start = Instant::now();

    let mut last_prompt: Option<String> = None;
    let mut first_user_text: Option<String> = None;
    let mut user_parts: Vec<String> = Vec::new();
    let mut assistant_parts: Vec<String> = Vec::new();
    let mut messages: Vec<RecentMessage> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut first_activity_at: String = String::new();
    let mut last_activity_at: String = String::new();

    loop {
        if start.elapsed() > SINGLE_FILE_PARSE_TIMEOUT {
            tracing::warn!(
                "解析 Claude session transcript 超时（>{:?}），跳过 {:?}",
                SINGLE_FILE_PARSE_TIMEOUT,
                path
            );
            return None;
        }
        let (line_opt, line_bytes, overflow) =
            match read_line_bounded(&mut reader, budget.max_jsonl_line_bytes) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!("读取 jsonl 行失败 {:?}: {err}", path);
                    break;
                }
            };
        outcome.bytes_read = outcome.bytes_read.saturating_add(line_bytes);
        if line_opt.is_none() && line_bytes == 0 {
            break; // EOF
        }
        if overflow {
            push_reason(&mut outcome.reasons, "max_jsonl_line_bytes");
            continue;
        }
        let line = match line_opt {
            Some(l) => l,
            None => continue,
        };
        let parsed: JsonlLine = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => continue, // malformed 行跳过，不阻断整文件
        };

        // 收集 cwd/gitBranch（取最后一条非空）
        if let Some(c) = parsed.cwd.as_deref().filter(|s| !s.is_empty()) {
            cwd = Some(c.to_string());
        }
        if let Some(b) = parsed.git_branch.as_deref().filter(|s| !s.is_empty()) {
            git_branch = Some(b.to_string());
        }

        // last-prompt 行：取最后一条作 title 来源
        if parsed.kind == "last-prompt" {
            if let Some(lp) = parsed.last_prompt.as_deref() {
                let trimmed = lp.trim();
                if !trimmed.is_empty() {
                    last_prompt = Some(trimmed.to_string());
                }
            }
            continue;
        }

        // user 文本
        if parsed.kind == "user" {
            if let Some(msg) = parsed.message.as_ref() {
                if msg.role == "user" {
                    if let Some(content) = msg.content.as_ref() {
                        if let Some(text) = extract_text_from_content(content) {
                            if !is_command_text(&text) {
                                if first_user_text.is_none() {
                                    first_user_text = Some(text.clone());
                                }
                                user_parts.push(text.clone());
                                messages.push(RecentMessage {
                                    role: "user".to_string(),
                                    text: text.clone(),
                                    timestamp: parsed.timestamp.clone().unwrap_or_default(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // assistant 文本
        if parsed.kind == "assistant" {
            if let Some(msg) = parsed.message.as_ref() {
                if msg.role == "assistant" {
                    if let Some(content) = msg.content.as_ref() {
                        if let Some(text) = extract_text_from_content(content) {
                            assistant_parts.push(text.clone());
                            messages.push(RecentMessage {
                                role: "assistant".to_string(),
                                text: text.clone(),
                                timestamp: parsed.timestamp.clone().unwrap_or_default(),
                            });
                        }
                    }
                }
            }
        }

        // 收集 timestamp 计算首末活动时间
        if let Some(ts) = parsed.timestamp.as_deref() {
            if !ts.is_empty() {
                if first_activity_at.is_empty() || ts < first_activity_at.as_str() {
                    first_activity_at = ts.to_string();
                }
                if last_activity_at.as_str() < ts {
                    last_activity_at = ts.to_string();
                }
            }
        }
    }

    let title = last_prompt.or(first_user_text).unwrap_or_default();
    let message_count = messages.len() as u32;

    // recent_messages：按 timestamp 升序取尾部 20 条（空字符串视为最早）
    messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let recent_messages: Vec<RecentMessage> = if messages.len() > RECENT_MESSAGES_MAX {
        messages.split_off(messages.len() - RECENT_MESSAGES_MAX)
    } else {
        messages
    };

    let user_text = user_parts.join("\n");
    let assistant_text = assistant_parts.join("\n");
    let (title, user_text, assistant_text, char_truncated) =
        apply_session_char_budget(title, user_text, assistant_text, budget.max_session_chars);
    if char_truncated {
        push_reason(&mut outcome.reasons, "max_session_chars");
    }

    Some((
        ClaudeSessionIndex {
            session_id,
            title,
            transcript_path: path.to_path_buf(),
            first_activity_at,
            last_activity_at,
            message_count,
            user_text,
            assistant_text,
            recent_messages,
            cwd,
            git_branch,
        },
        outcome,
    ))
}

// ---------------------------------------------------------------------------
// worktree 扫描
// ---------------------------------------------------------------------------

/// 扫描指定 worktree path 对应的 Claude session 索引（默认预算）。
///
/// Business Logic（为什么需要这个函数）:
///     搜索范围限定在当前 active worktree，需要把该 worktree 下所有 session jsonl 解析成内存索引。
///
/// Code Logic（这个函数做什么）:
///     委托 `scan_worktree_sessions_with_budget(path, &Default::default())`。
pub fn scan_worktree_sessions(worktree_path: &Path) -> WorktreeSessionIndex {
    scan_worktree_sessions_with_budget(worktree_path, &ClaudeIndexBudget::default())
}

/// 按预算扫描 worktree 对应 Claude session 目录。
///
/// Business Logic（为什么需要这个函数）:
///     生产热路径与单测都需要可注入预算的扫描，以在超大目录下有界返回并暴露 truncated diagnostics。
///
/// Code Logic（这个函数做什么）:
///     解析 ~/.claude/projects/<encoded> 后委托 `scan_worktree_sessions_at`。
pub fn scan_worktree_sessions_with_budget(
    worktree_path: &Path,
    budget: &ClaudeIndexBudget,
) -> WorktreeSessionIndex {
    let projects = claude_projects_dir();
    scan_worktree_sessions_at(worktree_path, projects.as_deref(), budget)
}

/// 可注入 projects 根目录的扫描入口（测试与默认路径共用）。
///
/// Business Logic（为什么需要这个函数）:
///     单测不能污染真实 `~/.claude/projects`，需要把 projects 根指到临时目录；
///     生产路径则传入 `claude_projects_dir()`。
///
/// Code Logic（这个函数做什么）:
///     1. canonicalize worktree；2. encode cwd；3. 列 *.jsonl 候选 (mtime, path)；
///     4. 排序 mtime desc + path asc；5. 应用 max_files；
///     6. 逐文件检查 max_file_bytes / max_total_bytes，有界 parse；
///     7. 组装 sessions + truncated + diagnostics。
pub fn scan_worktree_sessions_at(
    worktree_path: &Path,
    projects_dir: Option<&Path>,
    budget: &ClaudeIndexBudget,
) -> WorktreeSessionIndex {
    let canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().to_string();
    let encoded_cwd = encode_claude_project_path(&canonical_str);

    let mut sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut files_considered: u64 = 0;
    let mut files_indexed: u64 = 0;
    let mut bytes_read: u64 = 0;
    let mut truncated = false;

    if let Some(projects_dir) = projects_dir {
        let target_dir = projects_dir.join(&encoded_cwd);
        if target_dir.is_dir() {
            // 收集候选：(mtime, path)
            let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&target_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let mtime = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    candidates.push((mtime, path));
                }
            }
            files_considered = candidates.len() as u64;

            // mtime desc, path asc
            candidates.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.to_string_lossy().cmp(&b.1.to_string_lossy()))
            });

            if candidates.len() > budget.max_files {
                candidates.truncate(budget.max_files);
                push_reason(&mut reasons, "max_files");
                truncated = true;
            }

            for (_mtime, path) in candidates {
                let meta_len = match std::fs::metadata(&path) {
                    Ok(m) => m.len(),
                    Err(_) => continue,
                };
                if meta_len > budget.max_file_bytes {
                    push_reason(&mut reasons, "max_file_bytes");
                    truncated = true;
                    continue;
                }
                if bytes_read.saturating_add(meta_len) > budget.max_total_bytes {
                    push_reason(&mut reasons, "max_total_bytes");
                    truncated = true;
                    break;
                }

                match build_session_index_with_budget(&path, budget) {
                    Some((index, outcome)) => {
                        bytes_read = bytes_read.saturating_add(outcome.bytes_read.max(meta_len));
                        for r in outcome.reasons {
                            push_reason(&mut reasons, &r);
                            truncated = true;
                        }
                        sessions.insert(index.session_id.clone(), index);
                        files_indexed = files_indexed.saturating_add(1);
                    }
                    None => {
                        // 超时/打开失败/整文件跳过：若 skipped 会在 build 内记 reason，但 None 丢 outcome。
                        // 再检一次 metadata 以记录 max_file_bytes（双重保险已在上面处理）。
                        if meta_len > budget.max_file_bytes {
                            push_reason(&mut reasons, "max_file_bytes");
                            truncated = true;
                        }
                    }
                }
            }
        }
    }

    let diagnostics = if truncated {
        SessionSearchDiagnostics::truncated(reasons, files_considered, files_indexed, bytes_read)
    } else {
        SessionSearchDiagnostics::ok(files_considered, files_indexed, bytes_read)
    };

    WorktreeSessionIndex {
        worktree_path: canonical,
        encoded_cwd,
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
        truncated,
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// 搜索（spec 2.3）
// ---------------------------------------------------------------------------

/// 从指定文本中收集 query（小写）的命中上下文片段（spec 3.2 preview_snippets）。
///
/// Business Logic（为什么需要这个函数）:
///     前端列表项需要展示命中处前后各 30 字符的上下文片段，帮助用户判断是否是目标 session。
///     片段必须保留原文大小写（用户看到的预览要和真实 transcript 一致），故匹配用小写、截取用原文。
///
/// Code Logic（这个函数做什么）:
///     把原始文本转成 char 数组（chars_orig）及其 ASCII 小写镜像（chars_lower，用 to_ascii_lowercase
///     保证 1:1 映射避免 Unicode lower 展开导致的下标错位），在 chars_lower 上定位 query_lower 的命中位置，
///     再用同样的下标区间从 chars_orig 截取片段，去重后追加到 out（最多 max_total 段）。
fn collect_snippets(
    text_original: &str,
    query_lower: &str,
    radius: usize,
    out: &mut Vec<String>,
    max_total: usize,
) {
    if query_lower.is_empty() || out.len() >= max_total {
        return;
    }
    // 原文 char 数组：片段截取来源，保留原始大小写
    let chars_orig: Vec<char> = text_original.chars().collect();
    // ASCII 小写镜像：仅用于命中位置比对，1:1 映射保证下标与原文对齐
    let chars_lower: Vec<char> = chars_orig.iter().map(|c| c.to_ascii_lowercase()).collect();
    let query_chars: Vec<char> = query_lower.chars().collect();
    if query_chars.is_empty() || query_chars.len() > chars_lower.len() {
        return;
    }
    let mut i = 0;
    while i + query_chars.len() <= chars_lower.len() && out.len() < max_total {
        if chars_lower[i..i + query_chars.len()] == query_chars[..] {
            let start = i.saturating_sub(radius);
            let end = (i + query_chars.len() + radius).min(chars_orig.len());
            // 从原文数组截取，保留大小写
            let snippet: String = chars_orig[start..end].iter().collect();
            if !out.contains(&snippet) {
                out.push(snippet);
            }
            // 跳过本次命中，继续往后找
            i += query_chars.len();
        } else {
            i += 1;
        }
    }
}

/// 在指定 worktree 的索引里搜索（spec 2.3）。
///
/// Business Logic（为什么需要这个函数）:
///     前端搜索面板需要按关键词返回命中 session，空关键词返回最近活动 session。
///
/// Code Logic（这个函数做什么）:
///     - query trim 后为空 → 返回全部 session，按 last_activity_at 倒序（ISO 字符串字典序 = 时间序），limit 截断；
///     - query 非空 → title/user/assistant 任一命中才保留，按 title_hit>user_hit>assistant_hit 优先级 +
///       last_activity_at 倒序排序，limit 截断；preview_snippets 在 title/user_text/assistant_text 找命中片段（最多 3 段）。
pub fn search_sessions(
    index: &WorktreeSessionIndex,
    query: &str,
    limit: usize,
) -> Vec<SessionSearchHit> {
    let query_trimmed = query.trim();
    let limit = if limit == 0 {
        DEFAULT_SEARCH_LIMIT
    } else {
        limit
    };

    let mut hits: Vec<SessionSearchHit> = Vec::new();

    if query_trimmed.is_empty() {
        // 空 query：全部按 last_activity_at 倒序
        let mut all: Vec<&ClaudeSessionIndex> = index.sessions.values().collect();
        all.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        for session in all.into_iter().take(limit) {
            hits.push(make_hit(session, false, false, false));
        }
        return hits;
    }

    let query_lower = query_trimmed.to_lowercase();

    for session in index.sessions.values() {
        let title_lower = session.title.to_lowercase();
        let user_lower = session.user_text.to_lowercase();
        let assistant_lower = session.assistant_text.to_lowercase();

        let title_hit = title_lower.contains(&query_lower);
        let user_hit = user_lower.contains(&query_lower);
        let assistant_hit = assistant_lower.contains(&query_lower);

        if !title_hit && !user_hit && !assistant_hit {
            continue;
        }

        // preview_snippets：title 优先，其次 user，其次 assistant，最多 3 段
        // 传原始文本（保留大小写），collect_snippets 内部用 ASCII 小写镜像定位命中
        let mut snippets: Vec<String> = Vec::new();
        if title_hit {
            collect_snippets(
                &session.title,
                &query_lower,
                PREVIEW_SNIPPET_RADIUS,
                &mut snippets,
                PREVIEW_SNIPPET_MAX,
            );
        }
        if user_hit {
            collect_snippets(
                &session.user_text,
                &query_lower,
                PREVIEW_SNIPPET_RADIUS,
                &mut snippets,
                PREVIEW_SNIPPET_MAX,
            );
        }
        if assistant_hit {
            collect_snippets(
                &session.assistant_text,
                &query_lower,
                PREVIEW_SNIPPET_RADIUS,
                &mut snippets,
                PREVIEW_SNIPPET_MAX,
            );
        }

        hits.push(make_hit(session, title_hit, user_hit, assistant_hit));
        // snippets 已在 make_hit 之外算好，但为保持签名简洁这里通过闭包重算；用 helper 直接注入
        if let Some(last) = hits.last_mut() {
            last.preview_snippets = snippets;
        }
    }

    // 排序：title_hit 在前，其次 user_hit，其次 assistant_hit；同优先级按 last_activity_at 倒序
    hits.sort_by(|a, b| {
        // 优先级数值越小越靠前：title=0, user=1, assistant=2（取命中的最高优先级）
        let a_prio = hit_priority(a.title_hit, a.user_hit, a.assistant_hit);
        let b_prio = hit_priority(b.title_hit, b.user_hit, b.assistant_hit);
        match a_prio.cmp(&b_prio) {
            std::cmp::Ordering::Equal => b.last_activity_at.cmp(&a.last_activity_at),
            ord => ord,
        }
    });

    hits.truncate(limit);
    hits
}

/// 在索引上搜索并包装为带 diagnostics 的 SessionSearchResult。
///
/// Business Logic（为什么需要这个函数）:
///     命令层/远端路由需要返回 `{items, truncated, diagnostics}`，截断语义来自索引扫描而非搜索 limit。
///
/// Code Logic（这个函数做什么）:
///     调用 search_sessions，复制 index.truncated 与 index.diagnostics。
pub fn search_sessions_result(
    index: &WorktreeSessionIndex,
    query: &str,
    limit: usize,
) -> SessionSearchResult {
    SessionSearchResult {
        items: search_sessions(index, query, limit),
        truncated: index.truncated,
        diagnostics: index.diagnostics.clone(),
    }
}

/// 计算命中的优先级数值（越小越靠前）。
///
/// Business Logic（为什么需要这个函数）:
///     排序规则是 title 命中优先于 user 命中优先于 assistant 命中，需要一个可比的数值。
///
/// Code Logic（这个函数做什么）:
///     title 命中→0，否则 user 命中→1，否则 assistant 命中→2。
fn hit_priority(title_hit: bool, user_hit: bool, assistant_hit: bool) -> u8 {
    if title_hit {
        0
    } else if user_hit {
        1
    } else if assistant_hit {
        2
    } else {
        3
    }
}

/// 从 session 构造一个 SessionSearchHit（不含 preview_snippets，调用方填充）。
///
/// Business Logic（为什么需要这个函数）:
///     空 query 和关键词命中两条路径都需要构造 hit，避免重复字段拷贝。
///
/// Code Logic（这个函数做什么）:
///     映射 ClaudeSessionIndex 字段到 SessionSearchHit，preview_snippets 留空由调用方注入。
fn make_hit(
    session: &ClaudeSessionIndex,
    title_hit: bool,
    user_hit: bool,
    assistant_hit: bool,
) -> SessionSearchHit {
    SessionSearchHit {
        session_id: session.session_id.clone(),
        title: session.title.clone(),
        title_hit,
        user_hit,
        assistant_hit,
        first_activity_at: session.first_activity_at.clone(),
        last_activity_at: session.last_activity_at.clone(),
        message_count: session.message_count,
        preview_snippets: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// AppState 集成 + 文件监听（Task 1.2 / N7 Task 5 lifecycle）
// ---------------------------------------------------------------------------

/// worktree session 索引的内存共享句柄类型别名。
pub type SharedWorktreeSessionIndex = Arc<RwLock<WorktreeSessionIndex>>;

/// singleflight 进行中扫描的 watch 接收端。
///
/// Business Logic（为什么需要这个类型）:
///     同一 worktree 并发搜索时只应跑一次阻塞扫描；后续调用者等待同一结果。
///
/// Code Logic（这个类型做什么）:
///     `watch::Receiver<Option<Result<SharedWorktreeSessionIndex, String>>>`，None=进行中，Some=完成。
pub type ClaudeSessionIndexInflightRx =
    tokio::sync::watch::Receiver<Option<Result<SharedWorktreeSessionIndex, String>>>;

/// 单个 worktree 的 Claude session 文件监听运行时。
///
/// Business Logic（为什么需要这个结构体）:
///     watcher 不能只存 RecommendedWatcher：trailing debounce 与 spawn_blocking 扫描是后台任务，
///     项目移除/进程退出时必须 cancel+abort 后才能 drop，否则会幽灵写回已删除索引。
///
/// Code Logic（这个结构体做什么）:
///     聚合 watcher、CancellationToken、debounce 计时器与 trailing/scan JoinHandle；
///     `force_dispose` 取消令牌、abort 全部句柄并 drop watcher。
pub struct ClaudeSessionWatcherRuntime {
    /// notify 文件监听器（drop 即停止监听）
    watcher: RecommendedWatcher,
    /// 后台 trailing/scan 任务共享取消令牌
    cancel: CancellationToken,
    /// 应用层 debounce 上次处理时刻（与回调共享 Arc；字段保留所有权共置）
    #[allow(dead_code)]
    last_refresh: Arc<Mutex<Instant>>,
    /// trailing 延迟重扫句柄（最多一个）
    pending_trailing: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>>,
    /// 进行中的 leading/scan spawn 句柄
    pending_scans: Arc<Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>>,
}

impl ClaudeSessionWatcherRuntime {
    /// 取消并清理该运行时的全部后台任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     项目移除或 shutdown 时必须停止监听与 pending 重扫，避免写回陈旧/已移除索引。
    ///
    /// Code Logic（这个函数做什么）:
    ///     cancel 令牌 → abort trailing → abort pending scans → drop self（watcher）。
    pub fn force_dispose(self) {
        self.cancel.cancel();
        if let Ok(mut pending) = self.pending_trailing.lock() {
            if let Some(handle) = pending.take() {
                handle.abort();
            }
        }
        if let Ok(mut scans) = self.pending_scans.lock() {
            for handle in scans.drain(..) {
                handle.abort();
            }
        }
        // drop watcher at end of scope
        drop(self.watcher);
    }

    /// 测试/诊断：是否已发出取消。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生命周期测试需要确认 dispose 后 token 已 cancel。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 cancel.is_cancelled()。
    #[cfg(test)]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// watcher 事件映射后的索引动作。
///
/// Business Logic（为什么需要这个枚举）:
///     notify 事件种类与路径语义复杂（delete/rename/uncertain）；先映射为有限动作再应用，
///     才能用纯函数测试 delete/rename/rescan 语义。
///
/// Code Logic（这个枚举做什么）:
///     Upsert 重扫路径；Remove 按 session_id 删除；Rename=删旧+加新；BoundedRescan 全量有界重扫；Ignore 忽略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionWatchPlan {
    /// 对指定 jsonl 路径增量重索引
    Upsert(Vec<PathBuf>),
    /// 按 session_id（jsonl stem）从索引删除
    Remove(Vec<String>),
    /// rename：先删旧 id，再 upsert 新路径
    Rename {
        remove_ids: Vec<String>,
        upsert_paths: Vec<PathBuf>,
    },
    /// 不确定事件：对 watch root 做一次有界全量重扫
    BoundedRescan,
    /// 忽略（域外路径 / access 等）
    Ignore,
}

/// 把路径规范化到 owning watch root 内；域外返回 None。
///
/// Business Logic（为什么需要这个函数）:
///     notify 可能上报任意路径；只处理当前 worktree transcript 目录内的文件，避免误改索引。
///
/// Code Logic（这个函数做什么）:
///     优先 canonicalize；删除后不存在则 parent canonicalize + file_name；`starts_with(root)` 校验。
pub fn normalize_path_inside_root(path: &Path, root: &Path) -> Option<PathBuf> {
    let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let normalized = if path.exists() {
        path.canonicalize().ok()?
    } else {
        let parent = path.parent()?;
        let file_name = path.file_name()?;
        let parent_canon = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        parent_canon.join(file_name)
    };
    if normalized.starts_with(&root_canon) {
        Some(normalized)
    } else {
        None
    }
}

/// 从 jsonl 路径提取 session_id（文件 stem）。
///
/// Business Logic（为什么需要这个函数）:
///     delete/rename-from 事件需要按 session_id 从内存索引移除。
///
/// Code Logic（这个函数做什么）:
///     扩展名须为 jsonl；stem 非空 trim 后返回。
fn session_id_from_jsonl_path(path: &Path) -> Option<String> {
    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
        return None;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 过滤并规范化 watch root 内的 jsonl 路径。
///
/// Business Logic（为什么需要这个函数）:
///     Create/Modify/Rename 目标只关心 root 内 jsonl。
///
/// Code Logic（这个函数做什么）:
///     normalize_path_inside_root + 扩展名 jsonl，保序去重。
fn filter_jsonl_inside_root(paths: &[PathBuf], root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        let Some(normalized) = normalize_path_inside_root(path, root) else {
            continue;
        };
        if normalized.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if !out.iter().any(|p| p == &normalized) {
            out.push(normalized);
        }
    }
    out
}

/// 过滤 root 内路径并提取 session_id 列表。
///
/// Business Logic（为什么需要这个函数）:
///     Remove/Rename-from 需要 session_id 列表。
///
/// Code Logic（这个函数做什么）:
///     normalize 后取 stem，保序去重。
fn filter_session_ids_inside_root(paths: &[PathBuf], root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        let Some(normalized) = normalize_path_inside_root(path, root) else {
            continue;
        };
        let Some(id) = session_id_from_jsonl_path(&normalized) else {
            continue;
        };
        if !out.iter().any(|x| x == &id) {
            out.push(id);
        }
    }
    out
}

/// 将 notify 事件映射为 SessionWatchPlan。
///
/// Business Logic（为什么需要这个函数）:
///     delete 必须删索引、rename 必须删旧+加新、不确定事件只触发一次有界 rescan；
///     这是 watcher 正确性的核心，必须可单测。
///
/// Code Logic（这个函数做什么）:
///     规范化路径到 root 内；Create/Data/Metadata → Upsert；Remove → Remove；
///     Rename From/To/Both → 对应 Remove/Upsert/Rename；Any/Other/Modify(Any|Other) → BoundedRescan；
///     Access 与空有效路径 → Ignore。
pub fn classify_session_watch_event(
    kind: &EventKind,
    paths: &[PathBuf],
    watch_root: &Path,
) -> SessionWatchPlan {
    match kind {
        EventKind::Access(_) => SessionWatchPlan::Ignore,
        EventKind::Create(_) => {
            let upsert = filter_jsonl_inside_root(paths, watch_root);
            if upsert.is_empty() {
                SessionWatchPlan::Ignore
            } else {
                SessionWatchPlan::Upsert(upsert)
            }
        }
        EventKind::Remove(RemoveKind::Any | RemoveKind::File | RemoveKind::Other) => {
            let ids = filter_session_ids_inside_root(paths, watch_root);
            if ids.is_empty() {
                // 目录删除或无法识别：有界 rescan 收敛
                if paths
                    .iter()
                    .any(|p| normalize_path_inside_root(p, watch_root).is_some())
                {
                    SessionWatchPlan::BoundedRescan
                } else {
                    SessionWatchPlan::Ignore
                }
            } else {
                SessionWatchPlan::Remove(ids)
            }
        }
        EventKind::Remove(RemoveKind::Folder) => {
            if paths
                .iter()
                .any(|p| normalize_path_inside_root(p, watch_root).is_some())
            {
                SessionWatchPlan::BoundedRescan
            } else {
                SessionWatchPlan::Ignore
            }
        }
        EventKind::Modify(ModifyKind::Data(_)) | EventKind::Modify(ModifyKind::Metadata(_)) => {
            let upsert = filter_jsonl_inside_root(paths, watch_root);
            if upsert.is_empty() {
                SessionWatchPlan::Ignore
            } else {
                SessionWatchPlan::Upsert(upsert)
            }
        }
        EventKind::Modify(ModifyKind::Name(mode)) => {
            classify_rename_event(*mode, paths, watch_root)
        }
        EventKind::Modify(ModifyKind::Any)
        | EventKind::Modify(ModifyKind::Other)
        | EventKind::Any
        | EventKind::Other => {
            // 不确定事件：只要有 root 内路径或 paths 为空（后端未给路径）都请求有界 rescan
            if paths.is_empty()
                || paths
                    .iter()
                    .any(|p| normalize_path_inside_root(p, watch_root).is_some())
            {
                SessionWatchPlan::BoundedRescan
            } else {
                SessionWatchPlan::Ignore
            }
        }
    }
}

/// 映射 rename 子事件。
///
/// Business Logic（为什么需要这个函数）:
///     rename 在不同 OS 上拆成 From/To/Both；必须稳定收敛为 remove old + index new。
///
/// Code Logic（这个函数做什么）:
///     From→Remove；To→Upsert；Both→Rename(paths[0], paths[1])；Any/Other 按路径存在性推断。
fn classify_rename_event(
    mode: RenameMode,
    paths: &[PathBuf],
    watch_root: &Path,
) -> SessionWatchPlan {
    match mode {
        RenameMode::From => {
            let ids = filter_session_ids_inside_root(paths, watch_root);
            if ids.is_empty() {
                SessionWatchPlan::Ignore
            } else {
                SessionWatchPlan::Remove(ids)
            }
        }
        RenameMode::To => {
            let upsert = filter_jsonl_inside_root(paths, watch_root);
            if upsert.is_empty() {
                SessionWatchPlan::Ignore
            } else {
                SessionWatchPlan::Upsert(upsert)
            }
        }
        RenameMode::Both => {
            let from = paths.first().cloned();
            let to = paths.get(1).cloned();
            let remove_ids = from
                .as_ref()
                .map(|p| filter_session_ids_inside_root(std::slice::from_ref(p), watch_root))
                .unwrap_or_default();
            let upsert_paths = to
                .as_ref()
                .map(|p| filter_jsonl_inside_root(std::slice::from_ref(p), watch_root))
                .unwrap_or_default();
            if remove_ids.is_empty() && upsert_paths.is_empty() {
                SessionWatchPlan::Ignore
            } else if remove_ids.is_empty() {
                SessionWatchPlan::Upsert(upsert_paths)
            } else if upsert_paths.is_empty() {
                SessionWatchPlan::Remove(remove_ids)
            } else {
                SessionWatchPlan::Rename {
                    remove_ids,
                    upsert_paths,
                }
            }
        }
        RenameMode::Any | RenameMode::Other => {
            // 不确定 rename：两路径 → Both 语义；单路径按存在性；否则 BoundedRescan
            if paths.len() >= 2 {
                return classify_rename_event(RenameMode::Both, paths, watch_root);
            }
            if let Some(path) = paths.first() {
                let Some(normalized) = normalize_path_inside_root(path, watch_root) else {
                    return SessionWatchPlan::Ignore;
                };
                if normalized.exists() {
                    let upsert = filter_jsonl_inside_root(&[normalized], watch_root);
                    if upsert.is_empty() {
                        SessionWatchPlan::Ignore
                    } else {
                        SessionWatchPlan::Upsert(upsert)
                    }
                } else {
                    let ids = filter_session_ids_inside_root(&[normalized], watch_root);
                    if ids.is_empty() {
                        SessionWatchPlan::BoundedRescan
                    } else {
                        SessionWatchPlan::Remove(ids)
                    }
                }
            } else {
                SessionWatchPlan::BoundedRescan
            }
        }
    }
}

/// 将 SessionWatchPlan 应用到内存索引（阻塞，供 spawn_blocking / 测试调用）。
///
/// Business Logic（为什么需要这个函数）:
///     事件映射与索引更新解耦；delete/rename/rescan 语义在此落地。
///
/// Code Logic（这个函数做什么）:
///     Remove 写锁删 id；Upsert 走 refresh_sessions_from_paths；Rename 先删后 upsert；
///     BoundedRescan 走 refresh_all_sessions。
pub fn apply_session_watch_plan(
    shared: &SharedWorktreeSessionIndex,
    worktree_canonical: &Path,
    watch_dir: &Path,
    plan: SessionWatchPlan,
) {
    match plan {
        SessionWatchPlan::Ignore => {}
        SessionWatchPlan::Remove(ids) => {
            let mut guard = match shared.write() {
                Ok(g) => g,
                Err(err) => {
                    tracing::warn!("session index 写锁中毒，跳过 delete: {err}");
                    return;
                }
            };
            for id in ids {
                guard.sessions.remove(&id);
            }
            guard.last_scan_at = Utc::now().to_rfc3339();
        }
        SessionWatchPlan::Upsert(paths) => {
            if paths.is_empty() {
                return;
            }
            refresh_sessions_from_paths(shared, worktree_canonical, watch_dir, &paths);
        }
        SessionWatchPlan::Rename {
            remove_ids,
            upsert_paths,
        } => {
            {
                let mut guard = match shared.write() {
                    Ok(g) => g,
                    Err(err) => {
                        tracing::warn!("session index 写锁中毒，跳过 rename remove: {err}");
                        return;
                    }
                };
                for id in remove_ids {
                    guard.sessions.remove(&id);
                }
                guard.last_scan_at = Utc::now().to_rfc3339();
            }
            if !upsert_paths.is_empty() {
                refresh_sessions_from_paths(shared, worktree_canonical, watch_dir, &upsert_paths);
            }
        }
        SessionWatchPlan::BoundedRescan => {
            refresh_all_sessions(shared, watch_dir);
        }
    }
}

/// 读取指定 worktree key 的 dispose 世代。
///
/// Business Logic（为什么需要这个函数）:
///     ensure 扫描前后需要比对世代，识别“扫描期间项目已被移除”。
///
/// Code Logic（这个函数做什么）:
///     读 `workbench_claude_session_index_dispose_epochs`；缺失 key 视为 0；锁中毒时返回 0 并 warn。
fn dispose_epoch_for_key(state: &AppState, key: &str) -> u64 {
    match state.workbench_claude_session_index_dispose_epochs.lock() {
        Ok(map) => map.get(key).copied().unwrap_or(0),
        Err(err) => {
            tracing::warn!("session dispose epochs 锁中毒，按 0 处理 key={key}: {err}");
            0
        }
    }
}

/// 提升 key 的 dispose 世代（单调 +1）。
///
/// Business Logic（为什么需要这个函数）:
///     dispose 必须让任何 in-flight scan 在完成后看到世代变化，从而拒绝写回。
///
/// Code Logic（这个函数做什么）:
///     写锁 entry.or_insert(0) 后 saturating_add(1)。
fn bump_dispose_epoch(state: &AppState, key: &str) {
    match state.workbench_claude_session_index_dispose_epochs.lock() {
        Ok(mut map) => {
            let entry = map.entry(key.to_string()).or_insert(0);
            *entry = entry.saturating_add(1);
        }
        Err(err) => {
            tracing::warn!("session dispose epochs 写锁中毒，无法 bump key={key}: {err}");
        }
    }
}

/// 仅清 watcher / indexes / inflight 工件，不 bump dispose 世代。
///
/// Business Logic（为什么需要这个函数）:
///     finish 路径在 insert 后发现已被 dispose 时需撤回刚写入的工件，但世代已由 dispose 提升，不可再 bump。
///
/// Code Logic（这个函数做什么）:
///     force_dispose watcher；indexes.remove；try_lock 清 inflight entry。
fn purge_session_index_artifacts(state: &AppState, key: &str) {
    if let Ok(mut watchers) = state.workbench_claude_session_watchers.lock() {
        if let Some(runtime) = watchers.remove(key) {
            runtime.force_dispose();
        }
    }
    if let Ok(mut indexes) = state.workbench_claude_session_indexes.write() {
        indexes.remove(key);
    }
    if let Ok(mut inflight) = state.workbench_claude_session_index_inflight.try_lock() {
        inflight.remove(key);
    }
}

/// 按 worktree 路径列表清理 session 索引与 watcher 运行时。
///
/// Business Logic（为什么需要这个函数）:
///     用户移除项目时，相关 worktree 的索引与后台监听应立即停止，避免继续占用资源或写回。
///
/// Code Logic（这个函数做什么）:
///     对每个 path canonicalize 为 key，bump dispose epoch，force_dispose watcher，
///     并清 indexes/inflight 条目（raw key 亦尝试一次，防止 canonicalize 前后不一致）。
pub fn dispose_session_indexes_for_worktree_paths(state: &AppState, worktree_paths: &[PathBuf]) {
    for path in worktree_paths {
        let key = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string();
        dispose_session_index_by_key(state, &key);
        // 也尝试 raw 字符串 key（防止 canonicalize 前后不一致的历史条目）
        let raw = path.to_string_lossy().to_string();
        if raw != key {
            dispose_session_index_by_key(state, &raw);
        }
    }
}

/// 关闭全部 Claude session 索引与 watcher（进程 shutdown）。
///
/// Business Logic（为什么需要这个函数）:
///     后端退出时必须 cancel 所有 debounce/scan 任务并 drop watcher，避免幽灵任务与句柄泄漏。
///
/// Code Logic（这个函数做什么）:
///     drain watchers 并 force_dispose；clear indexes / dispose epochs；best-effort 清 inflight（try_lock）。
pub fn shutdown_all_claude_session_indexes(state: &AppState) {
    let runtimes: Vec<ClaudeSessionWatcherRuntime> = {
        let mut map = match state.workbench_claude_session_watchers.lock() {
            Ok(g) => g,
            Err(err) => {
                tracing::warn!("shutdown session watchers 锁中毒: {err}");
                return;
            }
        };
        map.drain().map(|(_, runtime)| runtime).collect()
    };
    for runtime in runtimes {
        runtime.force_dispose();
    }
    if let Ok(mut indexes) = state.workbench_claude_session_indexes.write() {
        indexes.clear();
    } else {
        tracing::warn!("shutdown session indexes 写锁中毒");
    }
    if let Ok(mut epochs) = state.workbench_claude_session_index_dispose_epochs.lock() {
        epochs.clear();
    } else {
        tracing::warn!("shutdown session dispose epochs 锁中毒");
    }
    // inflight 是 tokio Mutex；shutdown 路径 best-effort try_lock，避免阻塞退出
    if let Ok(mut inflight) = state.workbench_claude_session_index_inflight.try_lock() {
        inflight.clear();
    }
}

/// 按 key 移除单个索引 + runtime，并提升 dispose 世代。
///
/// Business Logic（为什么需要这个函数）:
///     单 worktree 清理与失败降级共用同一移除语义；必须让并发 ensure 无法写回幽灵索引。
///
/// Code Logic（这个函数做什么）:
///     bump dispose epoch → purge watcher/indexes/inflight。
fn dispose_session_index_by_key(state: &AppState, key: &str) {
    bump_dispose_epoch(state, key);
    purge_session_index_artifacts(state, key);
}

/// 确保 worktree 的 session 索引已扫描并启动文件监听（lazy + singleflight + spawn_blocking）。
///
/// Business Logic（为什么需要这个函数）:
///     首次搜索某 worktree 时才建索引（lazy），避免启动时全量扫描拖慢。扫描可能阻塞数秒，
///     必须放在 spawn_blocking 以免卡住 tokio；并发请求必须 singleflight 共享一次扫描。
///
/// Code Logic（这个函数做什么）:
///     1. 快路径：indexes 读锁命中直接返回；
///     2. 捕获 dispose start_epoch；
///     3. 锁 inflight map：已有 Receiver → 等待 Some；否则创建 watch(None) 并成为 leader；
///     4. leader/回退：`finish_scan_and_insert(start_epoch)`（spawn_blocking + epoch 守卫 insert）；
///     5. leader send Some(Ok) 并 remove inflight。
///
/// **为何写锁不活到函数末尾（防死锁）**：
///     spawn_session_watcher 失败分支会再取同一 indexes 写锁；标准库 RwLock 不支持重入。
pub async fn ensure_worktree_session_index_scanned(
    state: &AppState,
    worktree_path: &Path,
) -> SharedWorktreeSessionIndex {
    let canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let key = canonical.to_string_lossy().to_string();

    // 快速路径
    {
        let indexes = state
            .workbench_claude_session_indexes
            .read()
            .expect("session indexes 读锁中毒");
        if let Some(existing) = indexes.get(&key) {
            return Arc::clone(existing);
        }
    }

    // 在进入 inflight 前捕获 dispose 世代；扫描期间 dispose 会 bump，finish 据此 no-op
    let start_epoch = dispose_epoch_for_key(state, &key);

    // singleflight：注册或加入
    let (is_leader, mut rx, tx_opt) = {
        let mut map = state.workbench_claude_session_index_inflight.lock().await;
        if let Some(existing_rx) = map.get(&key) {
            (false, existing_rx.clone(), None)
        } else {
            let (tx, rx) =
                tokio::sync::watch::channel(None::<Result<SharedWorktreeSessionIndex, String>>);
            map.insert(key.clone(), rx.clone());
            (true, rx, Some(tx))
        }
    };

    if !is_leader {
        // follower：等待 leader 结果；失败则回退自扫
        loop {
            let current = rx.borrow().clone();
            if let Some(result) = current {
                match result {
                    Ok(shared) => return shared,
                    Err(err) => {
                        tracing::warn!(
                            "Claude session singleflight 扫描失败，follower 回退自扫 key={key}: {err}"
                        );
                        break;
                    }
                }
            }
            if rx.changed().await.is_err() {
                // sender dropped
                break;
            }
        }
        // 回退：再看缓存，否则自己扫（仍 spawn_blocking）
        {
            let indexes = state
                .workbench_claude_session_indexes
                .read()
                .expect("session indexes 读锁中毒");
            if let Some(existing) = indexes.get(&key) {
                return Arc::clone(existing);
            }
        }
        // 回退路径重新捕获 epoch（dispose 可能已发生）
        let fallback_epoch = dispose_epoch_for_key(state, &key);
        return finish_scan_and_insert(state, &key, &canonical, fallback_epoch).await;
    }

    // leader
    let shared = finish_scan_and_insert(state, &key, &canonical, start_epoch).await;
    if let Some(tx) = tx_opt {
        let _ = tx.send(Some(Ok(Arc::clone(&shared))));
    }
    {
        let mut map = state.workbench_claude_session_index_inflight.lock().await;
        map.remove(&key);
    }
    shared
}

/// 构造未写入缓存的空索引句柄（dispose 竞态 no-op 返回值）。
///
/// Business Logic（为什么需要这个函数）:
///     扫描期间 key 已被 dispose 时，调用方仍需要一个 Shared 句柄，但绝不能写入 indexes 或启 watcher。
///
/// Code Logic（这个函数做什么）:
///     返回 Arc 包装的空 WorktreeSessionIndex（truncated=false，diagnostics=unavailable）。
fn empty_uncached_session_index(canonical: &Path) -> SharedWorktreeSessionIndex {
    Arc::new(RwLock::new(WorktreeSessionIndex {
        worktree_path: canonical.to_path_buf(),
        encoded_cwd: encode_claude_project_path(&canonical.to_string_lossy()),
        sessions: HashMap::new(),
        last_scan_at: Utc::now().to_rfc3339(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::unavailable(),
    }))
}

/// leader/回退路径：spawn_blocking 扫描 + dispose-epoch 守卫的 insert + watcher。
///
/// Business Logic（为什么需要这个函数）:
///     singleflight leader 与 follower 失败回退共用同一插入路径；若扫描期间项目已被 dispose，
///     必须禁止写回索引/启动 watcher，否则会留下幽灵资源。
///
/// Code Logic（这个函数做什么）:
///     1. （测试）可选 mid-scan barrier；
///     2. spawn_blocking 默认扫描；
///     3. 写锁下比对 start_epoch 与当前 dispose epoch：变化则不 insert；
///     4. double-check 已有 entry 则复用且不重复 spawn watcher；
///     5. 仅本路径新 insert 时 spawn watcher，并在 spawn 前后再检 epoch，必要时 purge。
async fn finish_scan_and_insert(
    state: &AppState,
    key: &str,
    canonical: &Path,
    start_epoch: u64,
) -> SharedWorktreeSessionIndex {
    // 测试钩子：让 dispose 能在 scan 进行中介入
    #[cfg(test)]
    {
        scan_test_hooks_wait_before_scan().await;
    }

    // 扫描前再检一次：若已 dispose，跳过阻塞扫描直接返回
    if dispose_epoch_for_key(state, key) != start_epoch {
        tracing::debug!(
            "Claude session 扫描前检测到 dispose，跳过 insert key={key} start_epoch={start_epoch}"
        );
        return empty_uncached_session_index(canonical);
    }

    let canonical_owned = canonical.to_path_buf();
    let scan_result =
        tokio::task::spawn_blocking(move || scan_worktree_sessions(&canonical_owned)).await;

    let index = match scan_result {
        Ok(idx) => idx,
        Err(err) => {
            tracing::warn!("Claude session spawn_blocking 扫描 join 失败 key={key}: {err}");
            // 失败时返回空索引（若未 dispose 仍可插入，避免永久卡死）
            WorktreeSessionIndex {
                worktree_path: canonical.to_path_buf(),
                encoded_cwd: encode_claude_project_path(&canonical.to_string_lossy()),
                sessions: HashMap::new(),
                last_scan_at: Utc::now().to_rfc3339(),
                truncated: false,
                diagnostics: SessionSearchDiagnostics::unavailable(),
            }
        }
    };

    let shared = Arc::new(RwLock::new(index));
    let mut inserted = false;
    let to_return = {
        // 写锁内原子判定 dispose + double-check
        if dispose_epoch_for_key(state, key) != start_epoch {
            tracing::debug!(
                "Claude session 扫描后 dispose 世代变化，跳过 insert key={key} start_epoch={start_epoch}"
            );
            return empty_uncached_session_index(canonical);
        }
        let mut indexes = state
            .workbench_claude_session_indexes
            .write()
            .expect("session indexes 写锁中毒");
        // 再检 epoch（dispose 可能在等写锁时发生）
        if dispose_epoch_for_key(state, key) != start_epoch {
            tracing::debug!(
                "Claude session 写锁内 dispose 世代变化，跳过 insert key={key} start_epoch={start_epoch}"
            );
            return empty_uncached_session_index(canonical);
        }
        if let Some(existing) = indexes.get(key) {
            Arc::clone(existing)
        } else {
            indexes.insert(key.to_string(), Arc::clone(&shared));
            inserted = true;
            Arc::clone(&shared)
        }
    };
    // 写锁已释放

    // 仅本路径新写入缓存时启动 watcher；复用已有 index 不 thrash watcher
    if inserted {
        if dispose_epoch_for_key(state, key) != start_epoch {
            // insert 与 dispose 竞态：撤回缓存与可能已注册的 watcher
            purge_session_index_artifacts(state, key);
            return empty_uncached_session_index(canonical);
        }
        spawn_session_watcher(state, key, canonical, &to_return);
        if dispose_epoch_for_key(state, key) != start_epoch {
            purge_session_index_artifacts(state, key);
            return empty_uncached_session_index(canonical);
        }
    }
    to_return
}

#[cfg(test)]
#[path = "claude_sessions_scan_test_hooks.rs"]
mod scan_test_hooks;

#[cfg(test)]
async fn scan_test_hooks_wait_before_scan() {
    scan_test_hooks::wait_before_scan().await;
}

/// 为指定 worktree 启动 notify 文件监听。
///
/// Business Logic（为什么需要这个函数）:
///     Claude session 在用户使用过程中会持续追加 jsonl，索引需要增量更新才能保证搜索结果新鲜。
///
/// Code Logic（这个函数做什么）:
///     用 notify::RecommendedWatcher 监听 ~/.claude/projects/<encoded_cwd>/ 目录（RecursiveMode::NonRecursive），
///     poll_interval=500ms 控制 poll backend 轮询频率；回调内 classify_session_watch_event 映射
///     create/modify/delete/rename/uncertain，再做 **leading + trailing debounce** 后
///     spawn_blocking 应用 plan。watcher + cancel + debounce/scan handles 一并写入
///     `ClaudeSessionWatcherRuntime`。
///     **降级语义（spec 5.1）**：watcher 创建失败或 watch 失败时，必须从 workbench_claude_session_indexes
///     移除该 key 的索引，保证下次 ensure_worktree_session_index_scanned 命中不到缓存而重走慢路径重扫。
fn spawn_session_watcher(
    state: &AppState,
    key: &str,
    worktree_canonical: &Path,
    shared: &SharedWorktreeSessionIndex,
) {
    let encoded_cwd = {
        let guard = shared.read().expect("session index 读锁中毒");
        guard.encoded_cwd.clone()
    };

    let watch_dir = match claude_projects_dir() {
        Some(d) => d.join(&encoded_cwd),
        None => {
            tracing::warn!("无法获取 home 目录，跳过 Claude session 文件监听 key={key}");
            remove_index_on_failure(state, key);
            return;
        }
    };

    if !watch_dir.is_dir() {
        tracing::debug!(
            "Claude session transcript 目录不存在，暂不监听: {:?}",
            watch_dir
        );
        remove_index_on_failure(state, key);
        return;
    }

    let index_handle = Arc::clone(shared);
    let watch_dir_for_cb = watch_dir.clone();
    let worktree_canonical_owned = worktree_canonical.to_path_buf();

    let cancel = CancellationToken::new();
    let last_refresh = Arc::new(Mutex::new(
        Instant::now() - DEBOUNCE_INTERVAL - Duration::from_secs(1),
    ));
    let pending_trailing: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>> =
        Arc::new(Mutex::new(None));
    let pending_scans: Arc<Mutex<Vec<tauri::async_runtime::JoinHandle<()>>>> =
        Arc::new(Mutex::new(Vec::new()));

    let cancel_cb = cancel.clone();
    let last_refresh_cb = Arc::clone(&last_refresh);
    let pending_trailing_cb = Arc::clone(&pending_trailing);
    let pending_scans_cb = Arc::clone(&pending_scans);

    let watcher: Result<RecommendedWatcher, notify::Error> = Watcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if cancel_cb.is_cancelled() {
                return;
            }
            let event = match res {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!("Claude session 文件监听事件错误: {err}");
                    return;
                }
            };
            let plan = classify_session_watch_event(&event.kind, &event.paths, &watch_dir_for_cb);
            if matches!(plan, SessionWatchPlan::Ignore) {
                return;
            }

            // Remove/Rename 立即应用（索引正确性优先）；Upsert/BoundedRescan 走 leading+trailing debounce。
            let immediate = matches!(
                plan,
                SessionWatchPlan::Remove(_) | SessionWatchPlan::Rename { .. }
            );

            if immediate {
                if let Ok(mut pending) = pending_trailing_cb.lock() {
                    if let Some(handle) = pending.take() {
                        handle.abort();
                    }
                }
                let index_for_task = Arc::clone(&index_handle);
                let dir = watch_dir_for_cb.clone();
                let worktree = worktree_canonical_owned.clone();
                let cancel_task = cancel_cb.clone();
                let scans = Arc::clone(&pending_scans_cb);
                let handle = tauri::async_runtime::spawn_blocking(move || {
                    if cancel_task.is_cancelled() {
                        return;
                    }
                    apply_session_watch_plan(&index_for_task, &worktree, &dir, plan);
                });
                if let Ok(mut list) = scans.lock() {
                    // tauri JoinHandle 无 is_finished；dispose 时统一 abort，此处只登记。
                    list.push(handle);
                }
                return;
            }

            // 应用层 leading + trailing debounce（Upsert / BoundedRescan）。
            let should_process_now = {
                let mut last = match last_refresh_cb.lock() {
                    Ok(g) => g,
                    Err(err) => {
                        tracing::warn!("debounce 计时器锁中毒，跳本次重扫: {err}");
                        return;
                    }
                };
                if last.elapsed() >= DEBOUNCE_INTERVAL {
                    *last = Instant::now();
                    true
                } else {
                    false
                }
            };

            if should_process_now {
                if let Ok(mut pending) = pending_trailing_cb.lock() {
                    if let Some(handle) = pending.take() {
                        handle.abort();
                    }
                }
                let index_for_task = Arc::clone(&index_handle);
                let dir = watch_dir_for_cb.clone();
                let worktree = worktree_canonical_owned.clone();
                let cancel_task = cancel_cb.clone();
                let scans = Arc::clone(&pending_scans_cb);
                let handle = tauri::async_runtime::spawn_blocking(move || {
                    if cancel_task.is_cancelled() {
                        return;
                    }
                    apply_session_watch_plan(&index_for_task, &worktree, &dir, plan);
                });
                if let Ok(mut list) = scans.lock() {
                    list.push(handle);
                };
            } else {
                // trailing：不确定/高频 upsert 合并为一次有界全量 rescan（debounce 窗口末尾）
                let mut pending = match pending_trailing_cb.lock() {
                    Ok(g) => g,
                    Err(err) => {
                        tracing::warn!("debounce trailing 锁中毒，跳本次兜底安排: {err}");
                        return;
                    }
                };
                if let Some(old) = pending.take() {
                    old.abort();
                }
                let index_clone = Arc::clone(&index_handle);
                let dir_clone = watch_dir_for_cb.clone();
                let worktree_clone = worktree_canonical_owned.clone();
                let pending_clone = Arc::clone(&pending_trailing_cb);
                let cancel_task = cancel_cb.clone();
                let scans = Arc::clone(&pending_scans_cb);
                // trailing 统一有界 rescan，保证窗口内 delete+create 混合事件最终一致
                let trailing_plan = SessionWatchPlan::BoundedRescan;
                let handle = tauri::async_runtime::spawn(async move {
                    tokio::select! {
                        _ = cancel_task.cancelled() => return,
                        _ = tokio::time::sleep(DEBOUNCE_INTERVAL) => {}
                    }
                    if cancel_task.is_cancelled() {
                        return;
                    }
                    if let Ok(mut p) = pending_clone.lock() {
                        p.take();
                    }
                    let cancel_scan = cancel_task.clone();
                    let scan_handle = tauri::async_runtime::spawn_blocking(move || {
                        if cancel_scan.is_cancelled() {
                            return;
                        }
                        apply_session_watch_plan(
                            &index_clone,
                            &worktree_clone,
                            &dir_clone,
                            trailing_plan,
                        );
                    });
                    if let Ok(mut list) = scans.lock() {
                        list.push(scan_handle);
                    }
                });
                *pending = Some(handle);
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_millis(500)),
    );

    let mut watcher = match watcher {
        Ok(w) => w,
        Err(err) => {
            remove_index_on_failure(state, key);
            tracing::warn!(
                "启动 Claude session 文件监听失败，已移除索引（下次搜索将重扫）key={key}: {err}"
            );
            return;
        }
    };

    if let Err(err) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        remove_index_on_failure(state, key);
        tracing::warn!(
            "监听 Claude session 目录失败，已移除索引（下次搜索将重扫）{:?}: {err}",
            watch_dir
        );
        return;
    }

    let runtime = ClaudeSessionWatcherRuntime {
        watcher,
        cancel,
        last_refresh,
        pending_trailing,
        pending_scans,
    };
    let mut watchers = state
        .workbench_claude_session_watchers
        .lock()
        .expect("session watchers 锁中毒");
    // 同 key 旧 runtime（极少见）先 dispose
    if let Some(old) = watchers.insert(key.to_string(), runtime) {
        old.force_dispose();
    }
    tracing::info!(
        "已启动 Claude session 文件监听 key={key} dir={:?}",
        watch_dir
    );
}

/// 监听失败降级时从 indexes/watchers 缓存移除指定 worktree 的索引（spec 5.1）。
///
/// Business Logic（为什么需要这个函数）:
///     watcher 创建/启动失败时，索引已写入 workbench_claude_session_indexes，若不移除，
///     下次 ensure_worktree_session_index_scanned 会命中缓存直接返回旧索引，永不重扫。
///     spec 5.1 要求监听失败降级为每次搜索重扫，故必须删除该 key。
///
/// Code Logic（这个函数做什么）:
///     dispose_session_index_by_key：清 runtime（若有）与 indexes entry。
fn remove_index_on_failure(state: &AppState, key: &str) {
    dispose_session_index_by_key(state, key);
    tracing::info!("监听失败已移除 worktree session 索引缓存（下次搜索重扫）key={key}");
}

/// 重扫指定 jsonl 路径并更新内存索引。
///
/// Business Logic（为什么需要这个函数）:
///     notify 回调收到文件变化事件后，需要把对应 session 的索引刷新，保证搜索结果新鲜。
///
/// Code Logic（这个函数做什么）:
///     **先在写锁外**用默认预算 parse 变更路径与兜底新文件，再短暂持写锁原子 apply；
///     成功 upsert、失败按 stem 移除；更新 last_scan_at。
fn refresh_sessions_from_paths(
    shared: &SharedWorktreeSessionIndex,
    worktree_canonical: &Path,
    dir: &Path,
    changed_paths: &[PathBuf],
) {
    // 锁外解析变更路径（默认预算）
    let mut parsed: Vec<(String, Option<ClaudeSessionIndex>)> = Vec::new();
    for path in changed_paths {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let index = build_session_index(path);
        parsed.push((stem, index));
    }

    // 锁外收集「可能新增」的 jsonl（不查现有 map，写锁内再过滤）
    let mut extras: Vec<ClaudeSessionIndex> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) if !s.trim().is_empty() => s.trim().to_string(),
                _ => continue,
            };
            // 已在 changed_paths 处理过的跳过
            if parsed.iter().any(|(s, _)| s == &stem) {
                continue;
            }
            if let Some(index) = build_session_index(&p) {
                extras.push(index);
            }
        }
    }

    let mut guard = match shared.write() {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("session index 写锁中毒，跳过增量更新: {err}");
            return;
        }
    };

    for (stem, index_opt) in parsed {
        match index_opt {
            Some(index) => {
                guard.sessions.insert(index.session_id.clone(), index);
            }
            None => {
                if !stem.is_empty() {
                    guard.sessions.remove(&stem);
                }
            }
        }
    }
    for index in extras {
        if !guard.sessions.contains_key(&index.session_id) {
            guard.sessions.insert(index.session_id.clone(), index);
        }
    }

    guard.last_scan_at = Utc::now().to_rfc3339();
    let _ = worktree_canonical;
}

/// 全量重扫指定目录下所有 jsonl，重建 sessions HashMap（trailing 兜底专用）。
///
/// Business Logic（为什么需要这个函数）:
///     debounce 窗口内可能先后有多个不同 jsonl 文件变更，若 trailing 只重扫最后一批事件路径，
///     较早变更的文件会被漏掉。trailing 兜底必须全量重扫。
///
/// Code Logic（这个函数做什么）:
///     读锁取 worktree_path/encoded_cwd；**锁外**用默认预算扫描目录构建新 HashMap + diagnostics；
///     再写锁原子 swap sessions/truncated/diagnostics/last_scan_at。
fn refresh_all_sessions(shared: &SharedWorktreeSessionIndex, dir: &Path) {
    let (worktree_path, encoded_cwd) = {
        let guard = match shared.read() {
            Ok(g) => g,
            Err(err) => {
                tracing::warn!("session index 读锁中毒，跳过全量重扫: {err}");
                return;
            }
        };
        (guard.worktree_path.clone(), guard.encoded_cwd.clone())
    };

    let budget = ClaudeIndexBudget::default();
    // 直接扫 dir（dir 已是 projects/<encoded>）；构造伪 projects 父目录以复用排序/预算逻辑
    // 更直接：在 dir 上复刻候选排序 + 预算
    let mut candidates: Vec<(SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push((mtime, p));
        }
    }
    let files_considered = candidates.len() as u64;
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.to_string_lossy().cmp(&b.1.to_string_lossy()))
    });

    let mut reasons: Vec<String> = Vec::new();
    let mut truncated = false;
    if candidates.len() > budget.max_files {
        candidates.truncate(budget.max_files);
        push_reason(&mut reasons, "max_files");
        truncated = true;
    }

    let mut new_sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();
    let mut files_indexed: u64 = 0;
    let mut bytes_read: u64 = 0;
    for (_mtime, path) in candidates {
        let meta_len = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(_) => continue,
        };
        if meta_len > budget.max_file_bytes {
            push_reason(&mut reasons, "max_file_bytes");
            truncated = true;
            continue;
        }
        if bytes_read.saturating_add(meta_len) > budget.max_total_bytes {
            push_reason(&mut reasons, "max_total_bytes");
            truncated = true;
            break;
        }
        if let Some((index, outcome)) = build_session_index_with_budget(&path, &budget) {
            bytes_read = bytes_read.saturating_add(outcome.bytes_read.max(meta_len));
            for r in outcome.reasons {
                push_reason(&mut reasons, &r);
                truncated = true;
            }
            new_sessions.insert(index.session_id.clone(), index);
            files_indexed = files_indexed.saturating_add(1);
        }
    }

    let diagnostics = if truncated {
        SessionSearchDiagnostics::truncated(reasons, files_considered, files_indexed, bytes_read)
    } else {
        SessionSearchDiagnostics::ok(files_considered, files_indexed, bytes_read)
    };

    let mut guard = match shared.write() {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("session index 写锁中毒，跳过全量重扫: {err}");
            return;
        }
    };
    guard.sessions = new_sessions;
    guard.truncated = truncated;
    guard.diagnostics = diagnostics;
    guard.last_scan_at = Utc::now().to_rfc3339();
    // 保持 path/encoded 不变
    let _ = (worktree_path, encoded_cwd);
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 单测：`claude_sessions_test.rs`（文件名含 test，module-boundary 门禁排除）
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "claude_sessions_test.rs"]
mod tests;
