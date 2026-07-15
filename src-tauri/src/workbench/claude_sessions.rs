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
//!     - `build_session_index`：单文件流式解析，提取 ClaudeSessionIndex（标题=lastPrompt 回退首条 user、
//!       user 文本、assistant 文本、最近 20 条消息、元信息），过滤 slash/bash 命令，2 秒超时跳过。
//!     - `scan_worktree_sessions`：扫描 worktree path 对应 encoded-cwd 目录，组装 WorktreeSessionIndex。
//!     - `search_sessions`：按 spec 2.3 语义搜索（空 query 全部倒序 / 关键词命中优先级 + limit + preview snippets）。
//!     - `ensure_worktree_session_index_scanned`：lazy 初始化内存索引 + 启动 notify 文件监听（debounce 500ms），
//!       监听失败降级为每次重扫。

use crate::cc::collector::claude_projects_dir;
use crate::state::AppState;
use crate::workbench::claude_path::encode_claude_project_path;
use chrono::Utc;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

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
pub fn synthesize_legacy_session_search_result(items: Vec<SessionSearchHit>) -> SessionSearchResult {
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
            tracing::warn!("读取 Claude session transcript metadata 失败 {:?}: {err}", path);
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
// AppState 集成 + 文件监听（Task 1.2）
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

/// 确保 worktree 的 session 索引已扫描并启动文件监听（lazy + singleflight + spawn_blocking）。
///
/// Business Logic（为什么需要这个函数）:
///     首次搜索某 worktree 时才建索引（lazy），避免启动时全量扫描拖慢。扫描可能阻塞数秒，
///     必须放在 spawn_blocking 以免卡住 tokio；并发请求必须 singleflight 共享一次扫描。
///
/// Code Logic（这个函数做什么）:
///     1. 快路径：indexes 读锁命中直接返回；
///     2. 锁 inflight map：已有 Receiver → 等待 Some；否则创建 watch(None) 并成为 leader；
///     3. leader：`spawn_blocking(scan_worktree_sessions_with_budget default)`；
///     4. 短暂写锁 double-check insert；释放后 spawn_session_watcher；
///     5. send Some(Ok/Err) 并 remove inflight。
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

    // singleflight：注册或加入
    let (is_leader, mut rx, tx_opt) = {
        let mut map = state.workbench_claude_session_index_inflight.lock().await;
        if let Some(existing_rx) = map.get(&key) {
            (false, existing_rx.clone(), None)
        } else {
            let (tx, rx) = tokio::sync::watch::channel(
                None::<Result<SharedWorktreeSessionIndex, String>>,
            );
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
        return finish_scan_and_insert(state, &key, &canonical).await;
    }

    // leader
    let shared = finish_scan_and_insert(state, &key, &canonical).await;
    if let Some(tx) = tx_opt {
        let _ = tx.send(Some(Ok(Arc::clone(&shared))));
    }
    {
        let mut map = state.workbench_claude_session_index_inflight.lock().await;
        map.remove(&key);
    }
    shared
}

/// leader/回退路径：spawn_blocking 扫描 + 写锁 insert + watcher。
///
/// Business Logic（为什么需要这个函数）:
///     singleflight leader 与 follower 失败回退共用同一插入路径，避免重复逻辑。
///
/// Code Logic（这个函数做什么）:
///     spawn_blocking 默认预算扫描；double-check insert；释放锁后 spawn_session_watcher。
async fn finish_scan_and_insert(
    state: &AppState,
    key: &str,
    canonical: &Path,
) -> SharedWorktreeSessionIndex {
    let canonical_owned = canonical.to_path_buf();
    let scan_result = tokio::task::spawn_blocking(move || scan_worktree_sessions(&canonical_owned)).await;

    let index = match scan_result {
        Ok(idx) => idx,
        Err(err) => {
            tracing::warn!("Claude session spawn_blocking 扫描 join 失败 key={key}: {err}");
            // 失败时返回空索引（仍可插入，避免永久卡死）
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
    let to_return = {
        let mut indexes = state
            .workbench_claude_session_indexes
            .write()
            .expect("session indexes 写锁中毒");
        if let Some(existing) = indexes.get(key) {
            Arc::clone(existing)
        } else {
            indexes.insert(key.to_string(), Arc::clone(&shared));
            Arc::clone(&shared)
        }
    };
    // 写锁已释放
    spawn_session_watcher(state, key, canonical, &to_return);
    to_return
}

/// 为指定 worktree 启动 notify 文件监听。
///
/// Business Logic（为什么需要这个函数）:
///     Claude session 在用户使用过程中会持续追加 jsonl，索引需要增量更新才能保证搜索结果新鲜。
///
/// Code Logic（这个函数做什么）:
///     用 notify::RecommendedWatcher 监听 ~/.claude/projects/<encoded_cwd>/ 目录（RecursiveMode::NonRecursive），
///     poll_interval=500ms 控制 poll backend 轮询频率；回调内再做一层应用层 **leading + trailing debounce**
///     （见 DEBOUNCE_INTERVAL 注释），对 Create/Modify 的 jsonl 文件 spawn_blocking 重扫并更新对应 HashMap entry。
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

    // 回调闭包捕获共享索引 Arc 与 worktree canonical（用于重扫新文件）
    let index_handle = Arc::clone(shared);
    let watch_dir_for_cb = watch_dir.clone();
    let worktree_canonical_owned = worktree_canonical.to_path_buf();

    // 应用层 debounce 计时器：记录"上次重扫时刻"。初始化为很久以前，保证首次事件走 leading 立即处理。
    let last_refresh = Arc::new(std::sync::Mutex::new(
        Instant::now() - DEBOUNCE_INTERVAL - Duration::from_secs(1),
    ));

    // trailing 兜底任务句柄：累积期内每次新事件都 abort 旧的并 spawn 新的，保证 trailing 永远是
    // 「最后一次事件后 DEBOUNCE_INTERVAL」。None 表示当前无 pending 的兜底任务。
    let pending_trailing: Arc<std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(None));

    let watcher: Result<RecommendedWatcher, notify::Error> = Watcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let event = match res {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!("Claude session 文件监听事件错误: {err}");
                    return;
                }
            };
            // 只关心 Create/Modify，且路径扩展名为 jsonl
            let is_relevant = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_));
            if !is_relevant {
                return;
            }
            let jsonl_paths: Vec<PathBuf> = event
                .paths
                .into_iter()
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
                .collect();
            if jsonl_paths.is_empty() {
                return;
            }

            // 应用层 leading + trailing debounce。
            // - leading：距上次重扫 ≥ DEBOUNCE_INTERVAL → 立即重扫，更新 last，并取消 pending trailing。
            // - trailing：< DEBOUNCE_INTERVAL → 不立即重扫，但 abort 旧的 trailing 任务并 spawn 新的
            //   「DEBOUNCE_INTERVAL 后重扫」任务，保证最后一次事件后 500ms 必定兜底重扫（不漏内容）。
            // 锁获取顺序固定为「先 last，后 pending」，避免与其它路径交叉死锁。
            let should_process_now = {
                let mut last = match last_refresh.lock() {
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
                // leading：立即处理。先取消 pending trailing（如果有），避免兜底任务紧随其后重复重扫。
                if let Ok(mut pending) = pending_trailing.lock() {
                    if let Some(handle) = pending.take() {
                        handle.abort();
                    }
                }
                let index_for_task = Arc::clone(&index_handle);
                let dir = watch_dir_for_cb.clone();
                let worktree = worktree_canonical_owned.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    refresh_sessions_from_paths(&index_for_task, &worktree, &dir, &jsonl_paths);
                });
            } else {
                // trailing：abort 旧的 trailing 任务（若有），再 spawn 一个 500ms 后执行的兜底重扫。
                let mut pending = match pending_trailing.lock() {
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
                let pending_clone = Arc::clone(&pending_trailing);
                let handle = tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(DEBOUNCE_INTERVAL).await;
                    // sleep 结束：先把自己的句柄从 pending 清掉（表示已执行，不再可 abort），
                    // 再 spawn_blocking 全量重扫（兜底，保证 debounce 窗口内所有变更最终都被刷新，
                    // 不依赖事件累积的路径集合），await 确保重扫完成。
                    if let Ok(mut p) = pending_clone.lock() {
                        p.take();
                    }
                    tauri::async_runtime::spawn_blocking(move || {
                        refresh_all_sessions(&index_clone, &dir_clone);
                    })
                    .await
                    .ok();
                });
                *pending = Some(handle);
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_millis(500)),
    );

    let mut watcher = match watcher {
        Ok(w) => w,
        Err(err) => {
            // 降级：移除已写入的索引，下次搜索会重扫（spec 5.1）
            remove_index_on_failure(state, key);
            tracing::warn!(
                "启动 Claude session 文件监听失败，已移除索引（下次搜索将重扫）key={key}: {err}"
            );
            return;
        }
    };

    if let Err(err) = watcher.watch(&watch_dir, RecursiveMode::NonRecursive) {
        // 降级：移除已写入的索引，下次搜索会重扫（spec 5.1）
        remove_index_on_failure(state, key);
        tracing::warn!(
            "监听 Claude session 目录失败，已移除索引（下次搜索将重扫）{:?}: {err}",
            watch_dir
        );
        return;
    }

    // 存入 watchers HashMap（key 同 indexes）
    let mut watchers = state
        .workbench_claude_session_watchers
        .lock()
        .expect("session watchers 锁中毒");
    watchers.insert(key.to_string(), watcher);
    tracing::info!(
        "已启动 Claude session 文件监听 key={key} dir={:?}",
        watch_dir
    );
}

/// 监听失败降级时从 indexes 缓存移除指定 worktree 的索引（spec 5.1）。
///
/// Business Logic（为什么需要这个函数）:
///     watcher 创建/启动失败时，索引已写入 workbench_claude_session_indexes，若不移除，
///     下次 ensure_worktree_session_index_scanned 会命中缓存直接返回旧索引，永不重扫。
///     spec 5.1 要求监听失败降级为每次搜索重扫，故必须删除该 key。
///
/// Code Logic（这个函数做什么）:
///     持写锁删除 indexes 中 key 对应的 entry；失败（如锁中毒）只 warn 不 panic。
fn remove_index_on_failure(state: &AppState, key: &str) {
    let mut indexes = match state.workbench_claude_session_indexes.write() {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("移除 session 索引时写锁中毒（key={key}）: {err}");
            return;
        }
    };
    if indexes.remove(key).is_some() {
        tracing::info!("监听失败已移除 worktree session 索引缓存（下次搜索重扫）key={key}");
    }
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

#[cfg(test)]
mod tests {
    //! claude_sessions 单测：覆盖 jsonl 解析、worktree 扫描、搜索语义、文件监听降级。

    use super::*;
    use std::fs;
    use std::io::Write;

    /// 生成唯一临时目录路径（避免并发测试竞争，参考 Phase 0 flaky test 教训）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多个测试用同一固定路径会并发竞争，必须每个测试用唯一路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     temp_dir + 函数名 + 进程 id + 纳秒时间组合，保证跨测试跨运行唯一。
    fn unique_temp_dir(test_name: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "cc-partner-claude-sessions-{}-{}-{}",
            test_name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        fs::create_dir_all(&dir).expect("创建临时目录失败");
        dir
    }

    /// 写一个 jsonl 文件（每行一个 JSON 对象），返回文件路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试需要构造 Claude transcript 文件来验证解析逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 dir 下创建 session_id.jsonl，逐行写入 lines。
    fn write_jsonl(dir: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut f = fs::File::create(&path).expect("创建 jsonl 失败");
        for line in lines {
            writeln!(f, "{line}").expect("写入 jsonl 行失败");
        }
        path
    }

    /// Business Logic（为什么需要这个测试）:
    ///     WorktreeSessionIndex 的 encoded_cwd 必须复用 Phase 0 的 encode_claude_project_path 共享 helper，
    ///     否则扫描会落到错误的 transcript 目录。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个不存在的 worktree path，扫描后断言 encoded_cwd 字段等于 helper 直接编码结果。
    #[test]
    fn encode_uses_shared_helper() {
        let tmp = unique_temp_dir("encode_uses_shared_helper");
        let worktree = tmp.join("my-project");
        let index = scan_worktree_sessions(&worktree);
        let canonical = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
        let expected = encode_claude_project_path(&canonical.to_string_lossy());
        assert_eq!(index.encoded_cwd, expected);
        assert!(index.sessions.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     session 标题应取 lastPrompt（最后一条 last-prompt 行），让用户看到最近一次输入的摘要。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含两条 last-prompt 行的 jsonl，断言 title = 最后一条 lastPrompt。
    #[test]
    fn parse_extracts_last_prompt_as_title() {
        let tmp = unique_temp_dir("parse_extracts_last_prompt_as_title");
        let path = write_jsonl(
            &tmp,
            "sess-1",
            &[
                r#"{"type":"user","message":{"role":"user","content":"first prompt"},"timestamp":"2026-01-01T00:00:00Z","cwd":"/tmp/p"}"#,
                r#"{"type":"last-prompt","lastPrompt":"earlier summary"}"#,
                r#"{"type":"last-prompt","lastPrompt":"final summary"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert_eq!(index.title, "final summary");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无 last-prompt 行时标题应回退为第一条有效 user 文本，保证无 lastPrompt 的旧 transcript 也有可读标题。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造无 last-prompt 行的 jsonl，断言 title = 第一条 user 文本。
    #[test]
    fn parse_falls_back_to_first_user_when_no_last_prompt() {
        let tmp = unique_temp_dir("parse_falls_back_to_first_user_when_no_last_prompt");
        let path = write_jsonl(
            &tmp,
            "sess-2",
            &[
                r#"{"type":"user","message":{"role":"user","content":"first user text"},"timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]},"timestamp":"2026-01-01T00:01:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert_eq!(index.title, "first user text");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     user 的 content 是纯字符串时必须正确提取文本进 user_text 和 recent_messages。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 content 为 string 的 user 行，断言 user_text 含该文本。
    #[test]
    fn parse_extracts_user_text_from_string_content() {
        let tmp = unique_temp_dir("parse_extracts_user_text_from_string_content");
        let path = write_jsonl(
            &tmp,
            "sess-3",
            &[
                r#"{"type":"user","message":{"role":"user","content":"hello from string"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert!(index.user_text.contains("hello from string"));
        assert_eq!(index.message_count, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     user 的 content 是数组时（带 text 块）必须只取 type==text 块拼接，忽略 tool_result 等其它块。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 content 为含 text 和 tool_result 块的数组，断言 user_text 只含 text 块内容。
    #[test]
    fn parse_extracts_user_text_from_array_text_blocks() {
        let tmp = unique_temp_dir("parse_extracts_user_text_from_array_text_blocks");
        let path = write_jsonl(
            &tmp,
            "sess-4",
            &[
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"array user text"},{"type":"tool_result","content":"noise"}]},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert!(index.user_text.contains("array user text"));
        assert!(!index.user_text.contains("noise"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     assistant 文本只应包含 text 块，thinking 和 tool_use 必须被忽略，避免内部推理噪声进搜索。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 assistant 行含 text/thinking/tool_use 块，断言 assistant_text 只含 text 块。
    #[test]
    fn parse_extracts_assistant_text_ignoring_thinking_and_tool_use() {
        let tmp = unique_temp_dir("parse_extracts_assistant_text_ignoring_thinking_and_tool_use");
        let path = write_jsonl(
            &tmp,
            "sess-5",
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"internal"},{"type":"text","text":"visible reply"},{"type":"tool_use","name":"Bash","input":{}}]},"timestamp":"2026-01-01T00:01:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert!(index.assistant_text.contains("visible reply"));
        assert!(!index.assistant_text.contains("internal"));
        assert!(!index.assistant_text.contains("tool_use"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/help`、`!ls` 这类命令不应进入 user_text 和 recent_messages，避免污染搜索结果。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含 slash 和 bash 命令的 user 行，断言它们不进 user_text 与 recent_messages。
    #[test]
    fn parse_ignores_slash_and_bash_commands() {
        let tmp = unique_temp_dir("parse_ignores_slash_and_bash_commands");
        let path = write_jsonl(
            &tmp,
            "sess-6",
            &[
                r#"{"type":"user","message":{"role":"user","content":"/clear"},"timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"user","message":{"role":"user","content":"!ls -la"},"timestamp":"2026-01-01T00:01:00Z"}"#,
                r#"{"type":"user","message":{"role":"user","content":"real question"},"timestamp":"2026-01-01T00:02:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert!(!index.user_text.contains("/clear"));
        assert!(!index.user_text.contains("!ls"));
        assert!(index.user_text.contains("real question"));
        // 只有 real question 一条进了 recent
        assert_eq!(index.message_count, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     jsonl 可能含 malformed 行（被截断、非法 JSON），解析必须跳过它们不 panic，正常行仍要解析。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含非法 JSON 行的 jsonl，断言不 panic 且正常 user 文本仍被提取。
    #[test]
    fn parse_skips_malformed_lines_without_panicking() {
        let tmp = unique_temp_dir("parse_skips_malformed_lines_without_panicking");
        let path = write_jsonl(
            &tmp,
            "sess-7",
            &[
                r#"{"type":"user","message":{"role":"user","content":"good line"},"timestamp":"2026-01-01T00:00:00Z"}"#,
                "this is not json {{{",
                "",
                r#"{"type":"user","message":{"role":"user","content":"another good"},"timestamp":"2026-01-01T00:01:00Z"}"#,
            ],
        );
        let index = build_session_index(&path).expect("应解析成功");
        assert!(index.user_text.contains("good line"));
        assert!(index.user_text.contains("another good"));
        assert_eq!(index.message_count, 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 query 应返回全部 session 并按 last_activity_at 倒序，让最近活动的 session 排在最前。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个含 3 个不同活动时间 session 的 WorktreeSessionIndex，空 query 搜索断言顺序为最新到最旧。
    #[test]
    fn search_empty_query_returns_all_sorted_by_last_activity_desc() {
        let tmp = unique_temp_dir("search_empty_query_returns_all_sorted_by_last_activity_desc");
        let _p1 = write_jsonl(
            &tmp,
            "old",
            &[
                r#"{"type":"user","message":{"role":"user","content":"old"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );
        let _p2 = write_jsonl(
            &tmp,
            "mid",
            &[
                r#"{"type":"user","message":{"role":"user","content":"mid"},"timestamp":"2026-06-01T00:00:00Z"}"#,
            ],
        );
        let _p3 = write_jsonl(
            &tmp,
            "new",
            &[
                r#"{"type":"user","message":{"role":"user","content":"new"},"timestamp":"2026-07-01T00:00:00Z"}"#,
            ],
        );

        let mut sessions = HashMap::new();
        for entry in fs::read_dir(&tmp).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                if let Some(idx) = build_session_index(&p) {
                    sessions.insert(idx.session_id.clone(), idx);
                }
            }
        }
        let index = WorktreeSessionIndex {
            worktree_path: tmp.clone(),
            encoded_cwd: "test".to_string(),
            sessions,
            last_scan_at: Utc::now().to_rfc3339(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
        };

        let hits = search_sessions(&index, "", 50);
        assert_eq!(hits.len(), 3);
        // 最新的 new 排第一
        assert_eq!(hits[0].session_id, "new");
        assert_eq!(hits[1].session_id, "mid");
        assert_eq!(hits[2].session_id, "old");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     关键词命中应按 title_hit > user_hit > assistant_hit 优先级排序，帮助用户更快定位。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造三个 session 分别在 title/user/assistant 命中同一关键词，断言排序为 title、user、assistant。
    #[test]
    fn search_keyword_prioritizes_title_hit_over_user_over_assistant() {
        let tmp = unique_temp_dir("search_keyword_prioritizes_title");
        // s-title：title（lastPrompt）命中 "fix"，user 文本不含 fix
        let _p1 = write_jsonl(
            &tmp,
            "s-title",
            &[
                r#"{"type":"last-prompt","lastPrompt":"fix auth bug"}"#,
                r#"{"type":"user","message":{"role":"user","content":"misc question"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );
        // s-user：title（lastPrompt）不含 fix，user 文本命中 "fix"
        let _p2 = write_jsonl(
            &tmp,
            "s-user",
            &[
                r#"{"type":"last-prompt","lastPrompt":"deploy notes"}"#,
                r#"{"type":"user","message":{"role":"user","content":"please fix the login"},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );
        // s-assistant：title 与 user 都不含 fix，assistant 文本命中 "fix"
        let _p3 = write_jsonl(
            &tmp,
            "s-assistant",
            &[
                r#"{"type":"last-prompt","lastPrompt":"random topic"}"#,
                r#"{"type":"user","message":{"role":"user","content":"hello there"},"timestamp":"2026-01-01T00:00:00Z"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I will fix that now"}]},"timestamp":"2026-01-01T00:00:00Z"}"#,
            ],
        );

        let mut sessions = HashMap::new();
        for entry in fs::read_dir(&tmp).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                if let Some(idx) = build_session_index(&p) {
                    sessions.insert(idx.session_id.clone(), idx);
                }
            }
        }
        let index = WorktreeSessionIndex {
            worktree_path: tmp.clone(),
            encoded_cwd: "test".to_string(),
            sessions,
            last_scan_at: Utc::now().to_rfc3339(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
        };

        let hits = search_sessions(&index, "fix", 50);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].session_id, "s-title");
        assert!(hits[0].title_hit);
        assert_eq!(hits[1].session_id, "s-user");
        assert!(!hits[1].title_hit);
        assert!(hits[1].user_hit);
        assert_eq!(hits[2].session_id, "s-assistant");
        assert!(hits[2].assistant_hit);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     limit 应截断结果数量，防止超长列表。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 3 个 session，limit=2 断言只返回 2 条。
    #[test]
    fn search_respects_limit() {
        let tmp = unique_temp_dir("search_respects_limit");
        for i in 0..3 {
            let line = format!(
                r#"{{"type":"user","message":{{"role":"user","content":"item {i}"}},"timestamp":"2026-01-0{i}T00:00:00Z"}}"#
            );
            let _ = write_jsonl(&tmp, &format!("s{i}"), &[line.as_str()]);
        }

        let mut sessions = HashMap::new();
        for entry in fs::read_dir(&tmp).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                if let Some(idx) = build_session_index(&p) {
                    sessions.insert(idx.session_id.clone(), idx);
                }
            }
        }
        let index = WorktreeSessionIndex {
            worktree_path: tmp.clone(),
            encoded_cwd: "test".to_string(),
            sessions,
            last_scan_at: Utc::now().to_rfc3339(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
        };

        let hits = search_sessions(&index, "", 2);
        assert_eq!(hits.len(), 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview_snippets 应取命中位置前后各 30 字符的上下文片段，帮助用户预判是否是目标 session。
    ///     且片段必须保留原文大小写（不能返回全小写文本，否则用户预览失真）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一段含大小写的 user 文本（"foo Target bar"），用小写关键词 "target" 搜索，
    ///     断言 preview_snippets 非空、含关键词、且保留原文大写 "Target"。
    #[test]
    fn search_preview_snippets_extract_context_around_hit() {
        let tmp = unique_temp_dir("search_preview_snippets_extract_context_around_hit");
        // 构造一段足够长的文本，关键词在中间，且含大小写以验证片段保留原文大小写
        let prefix = "x".repeat(40);
        let suffix = "y".repeat(40);
        let content = format!("{prefix}foo Target bar{suffix}");
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
            serde_json::Value::String(content)
        );
        let path = write_jsonl(&tmp, "s-snippet", &[&line]);

        let mut sessions = HashMap::new();
        if let Some(idx) = build_session_index(&path) {
            sessions.insert(idx.session_id.clone(), idx);
        }
        let index = WorktreeSessionIndex {
            worktree_path: tmp.clone(),
            encoded_cwd: "test".to_string(),
            sessions,
            last_scan_at: Utc::now().to_rfc3339(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::ok(0, 0, 0),
        };

        // 用小写关键词搜索，应命中大写的 "Target"
        let hits = search_sessions(&index, "target", 50);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].user_hit);
        assert!(!hits[0].preview_snippets.is_empty());
        let snippet = &hits[0].preview_snippets[0];
        // 片段应包含关键词（小写匹配命中大写原文）
        assert!(snippet.to_lowercase().contains("target"));
        // 关键：片段保留原文大小写，不是全小写
        assert!(
            snippet.contains("Target"),
            "snippet 应保留原文大小写，实际: {snippet}"
        );
        // 片段长度应 <= 关键词长度 + 前后各 30 字符
        assert!(
            snippet.chars().count()
                <= "foo Target bar".chars().count() + 2 * PREVIEW_SNIPPET_RADIUS
        );
        // 应包含关键词前面的部分上下文
        assert!(snippet.contains('x'));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     recent_messages 上限为 20（spec 3.1），超过时只保留按时间排序的尾部 20 条，
    ///     保证 preview 面板数据量可控。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个含 25 条 user/assistant 交替消息的 jsonl（带递增 timestamp），调用
    ///     build_session_index，断言 recent_messages.len() == 20，message_count == 25，
    ///     且 recent_messages 恰好是最后 20 条（首条文本含序号 6，末条含序号 25）。
    #[test]
    fn recent_messages_capped_at_twenty() {
        let tmp = unique_temp_dir("recent_messages_capped_at_twenty");
        let mut lines: Vec<String> = Vec::new();
        // 25 条 user/assistant 交替消息，timestamp 递增，文本带序号便于断言尾部
        for i in 1..=25 {
            let role = if i % 2 == 1 { "user" } else { "assistant" };
            let text = format!("msg-{i:02}");
            let ts = format!("2026-01-01T00:{:02}:00Z", i); // 每分钟一条，严格递增
            let content = if role == "user" {
                format!(r#""{}""#, text)
            } else {
                format!(r#"[{{"type":"text","text":"{}"}}]"#, text)
            };
            lines.push(format!(
                r#"{{"type":"{role}","message":{{"role":"{role}","content":{content}}},"timestamp":"{ts}"}}"#
            ));
        }
        let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let path = write_jsonl(&tmp, "sess-many", &line_refs);

        let index = build_session_index(&path).expect("应解析成功");
        // message_count 记录全部有效消息（25 条，user 13 + assistant 12）
        assert_eq!(index.message_count, 25, "message_count 应为全部消息数");
        // recent_messages 被截断为 20
        assert_eq!(
            index.recent_messages.len(),
            RECENT_MESSAGES_MAX,
            "recent_messages 应被截断为 {}",
            RECENT_MESSAGES_MAX
        );
        // 首条应是序号 6（25-20+1=6），末条是序号 25
        assert_eq!(
            index.recent_messages[0].text, "msg-06",
            "recent_messages 首条应是按时间排序的第 6 条"
        );
        assert_eq!(
            index.recent_messages[19].text, "msg-25",
            "recent_messages 末条应是最后一条"
        );
        // 首末时间戳也应是第 6 条和第 25 条的时间
        assert_eq!(index.recent_messages[0].timestamp, "2026-01-01T00:06:00Z");
        assert_eq!(index.recent_messages[19].timestamp, "2026-01-01T00:25:00Z");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     N7 性能基线：在引入 spawn_blocking / 文件数 / 字节 / 行长 / 缓存文本预算之前，
    ///     必须可重复记录「当前同步全量索引」的耗时、处理字节、会话数与截断语义，
    ///     后续任务才能证明预算化与非阻塞改造真正改善了热点，而不是仅改代码结构。
    ///
    /// Code Logic（这个测试做什么）:
    ///     1. 用 temp JSONL fixture（非用户目录）生成多 session + 长正文 + 超 20 条消息；
    ///     2. 同步调用 build_session_index 扫描全部文件，累计 wall 时间与读取字节；
    ///     3. 并行 heartbeat 线程每 2ms 自增，记录索引期间心跳次数（表征调用线程占用）；
    ///     4. 断言当前行为：全量入库（无 file/byte budget 截断）、user/assistant 全文缓存、
    ///        recent_messages 仅截到 20；经 eprintln! 输出可复现基线指标。
    #[test]
    fn index_budget_baseline() {
        let tmp = unique_temp_dir("index_budget_baseline");

        // 构造 8 个 session：前 6 个中等大小，第 7 个含超长 user 正文，第 8 个 30 条消息
        // 触发 recent_messages 截断（当前唯一稳定的截断语义）。
        let long_body = "L".repeat(8_192);
        let mut session_ids: Vec<String> = Vec::new();

        for i in 0..6 {
            let sid = format!("budget-sess-{i:02}");
            let user = format!(
                r#"{{"type":"user","message":{{"role":"user","content":"baseline prompt {i}"}},"timestamp":"2026-07-0{}T0{}:00:00Z","cwd":"/tmp/budget"}}"#,
                (i % 9) + 1,
                i % 9
            );
            let asst = format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}},"timestamp":"2026-07-0{}T0{}:01:00Z"}}"#,
                (i % 9) + 1,
                i % 9
            );
            let _ = write_jsonl(&tmp, &sid, &[user.as_str(), asst.as_str()]);
            session_ids.push(sid);
        }

        // 超长正文 session：当前实现会把整段写入 user_text（无 1M scalar / 行长预算）
        {
            let sid = "budget-sess-long".to_string();
            let content = format!("prefix-{long_body}-suffix");
            let line = format!(
                r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-07-14T12:00:00Z"}}"#,
                serde_json::Value::String(content.clone())
            );
            let path = write_jsonl(&tmp, &sid, &[line.as_str()]);
            let _ = path;
            session_ids.push(sid);
        }

        // 30 条消息 session：recent_messages 截到 20，message_count 仍为 30
        {
            let sid = "budget-sess-many".to_string();
            let mut lines: Vec<String> = Vec::new();
            for i in 1..=30 {
                let role = if i % 2 == 1 { "user" } else { "assistant" };
                let text = format!("m{i:02}");
                let ts = format!("2026-07-14T13:{:02}:00Z", i);
                let content = if role == "user" {
                    format!(r#""{text}""#)
                } else {
                    format!(r#"[{{"type":"text","text":"{text}"}}]"#)
                };
                lines.push(format!(
                    r#"{{"type":"{role}","message":{{"role":"{role}","content":{content}}},"timestamp":"{ts}"}}"#
                ));
            }
            let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            let _ = write_jsonl(&tmp, &sid, &refs);
            session_ids.push(sid);
        }

        // 汇总 fixture 字节（当前实现会完整读入这些字节；后续预算化后可能截断）
        let mut total_fixture_bytes: u64 = 0;
        for entry in fs::read_dir(&tmp).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                total_fixture_bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            }
        }

        // Heartbeat 线程：索引期间每 2ms 自增，用于表征「调用线程被同步解析占用」时
        // 仍可观测的并发心跳次数（非生产 watcher heartbeat；仅基线证据）。
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::thread;
        let stop = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU64::new(0));
        let stop_hb = Arc::clone(&stop);
        let ticks_hb = Arc::clone(&ticks);
        let hb = thread::spawn(move || {
            while !stop_hb.load(Ordering::Relaxed) {
                ticks_hb.fetch_add(1, Ordering::Relaxed);
                thread::sleep(Duration::from_millis(2));
            }
        });

        let wall_start = Instant::now();
        let mut sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();
        let mut indexed_bytes: u64 = 0;
        for entry in fs::read_dir(&tmp).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let file_len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if let Some(idx) = build_session_index(&p) {
                indexed_bytes += file_len;
                sessions.insert(idx.session_id.clone(), idx);
            }
        }
        let elapsed = wall_start.elapsed();
        stop.store(true, Ordering::Relaxed);
        let _ = hb.join();
        let heartbeat_ticks = ticks.load(Ordering::Relaxed);

        // --- 当前行为断言（characterization，Task 4/5 优化后会改语义） ---
        // 1) 无 file/byte budget：全部 8 个 fixture 都应入库
        assert_eq!(
            sessions.len(),
            session_ids.len(),
            "当前实现应对全部 fixture session 建索引（无截断预算）"
        );

        // 2) 超长正文全文缓存进 user_text（无 per-session scalar 截断）
        let long = sessions
            .get("budget-sess-long")
            .expect("long session should be indexed");
        assert!(
            long.user_text.contains(&long_body),
            "当前实现缓存完整 user_text，不含截断标记"
        );
        assert!(
            long.user_text.len() >= long_body.len(),
            "user_text 应保留完整长正文"
        );

        // 3) recent_messages 是当前唯一稳定截断：30 条 → 20
        let many = sessions
            .get("budget-sess-many")
            .expect("many-messages session should be indexed");
        assert_eq!(many.message_count, 30);
        assert_eq!(many.recent_messages.len(), RECENT_MESSAGES_MAX);
        assert_eq!(many.recent_messages[0].text, "m11");
        assert_eq!(many.recent_messages[19].text, "m30");

        // 4) 中等 session 的 assistant 全文也缓存
        let mid = sessions
            .get("budget-sess-00")
            .expect("mid session should be indexed");
        assert!(mid.assistant_text.contains("reply 0"));
        assert!(mid.user_text.contains("baseline prompt 0"));

        // 可重复基线输出（cargo test ... -- --nocapture）
        eprintln!(
            "[perf-baseline] claude_sessions index_budget_baseline: \
             sessions={} fixture_bytes={} indexed_bytes={} elapsed_ms={} heartbeat_ticks={} \
             truncation=recent_messages_only(max={}) full_text_cache=true file_budget=none",
            sessions.len(),
            total_fixture_bytes,
            indexed_bytes,
            elapsed.as_millis(),
            heartbeat_ticks,
            RECENT_MESSAGES_MAX,
        );

        // 基本健全性：应处理完 fixture 字节且耗时有限（避免无限挂起）
        assert_eq!(indexed_bytes, total_fixture_bytes);
        assert!(
            elapsed < Duration::from_secs(5),
            "fixture 索引应在 5s 内完成，实际 {:?}",
            elapsed
        );
        // heartbeat 线程在同步索引期间应至少跳动过（证明测量面可观测；次数随机器变化）
        assert!(
            heartbeat_ticks > 0,
            "heartbeat 线程应在索引期间至少 tick 一次"
        );
    }

    /// 准备临时 projects 布局：返回 (worktree, projects_dir, session_dir)。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     预算扫描测试需把 jsonl 放到 `projects/<encoded-cwd>/`，不能污染真实 ~/.claude。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 worktree 与 projects/<encoded> 目录。
    fn prepare_scan_fixture(test_name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = unique_temp_dir(test_name);
        let worktree = root.join("wt");
        fs::create_dir_all(&worktree).unwrap();
        let projects = root.join("projects");
        let canonical = worktree.canonicalize().unwrap_or_else(|_| worktree.clone());
        let encoded = encode_claude_project_path(&canonical.to_string_lossy());
        let session_dir = projects.join(&encoded);
        fs::create_dir_all(&session_dir).unwrap();
        (worktree, projects, session_dir)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_files 预算必须截断候选并标记 truncated + reason=max_files。
    ///
    /// Code Logic（这个测试做什么）:
    ///     5 个 jsonl，budget.max_files=2，断言 indexed=2、truncated、reasons 含 max_files。
    #[test]
    fn budget_max_files_truncates() {
        let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_files");
        for i in 0..5 {
            let _ = write_jsonl(
                &session_dir,
                &format!("f{i}"),
                &[&format!(
                    r#"{{"type":"user","message":{{"role":"user","content":"file {i}"}},"timestamp":"2026-01-01T00:0{i}:00Z"}}"#
                )],
            );
        }
        let budget = ClaudeIndexBudget {
            max_files: 2,
            ..ClaudeIndexBudget::default()
        };
        let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
        assert_eq!(index.sessions.len(), 2);
        assert!(index.truncated);
        assert_eq!(index.diagnostics.status, DIAG_STATUS_TRUNCATED);
        assert!(index.diagnostics.reasons.iter().any(|r| r == "max_files"));
        assert_eq!(index.diagnostics.files_considered, 5);
        assert_eq!(index.diagnostics.files_indexed, 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超过 max_file_bytes 的文件必须整文件跳过。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写一个较大 jsonl，设 max_file_bytes 很小，断言 0 indexed + reason max_file_bytes。
    #[test]
    fn budget_max_file_bytes_skips_oversized() {
        let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_file_bytes");
        let big = "X".repeat(2000);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
            serde_json::Value::String(big)
        );
        let _ = write_jsonl(&session_dir, "huge", &[line.as_str()]);
        let budget = ClaudeIndexBudget {
            max_file_bytes: 100,
            ..ClaudeIndexBudget::default()
        };
        let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
        assert!(index.sessions.is_empty());
        assert!(index.truncated);
        assert!(index
            .diagnostics
            .reasons
            .iter()
            .any(|r| r == "max_file_bytes"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超长 jsonl 行不得整行分配进内存；应跳过该行并记 max_jsonl_line_bytes。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写一行 > budget 的 payload + 一行正常 user；断言完成、reason 命中、正常行仍可能入库。
    #[test]
    fn budget_max_jsonl_line_bytes_drops_long_line_without_allocating_all() {
        let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_jsonl_line");
        let long_content = "Z".repeat(500);
        // 故意构造一行远超 max_jsonl_line_bytes=80 的 JSON 行
        let long_line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
            serde_json::Value::String(long_content)
        );
        let good = r#"{"type":"user","message":{"role":"user","content":"short ok"},"timestamp":"2026-01-01T00:01:00Z"}"#;
        let path = session_dir.join("mixed.jsonl");
        {
            let mut f = fs::File::create(&path).unwrap();
            writeln!(f, "{long_line}").unwrap();
            writeln!(f, "{good}").unwrap();
        }
        // 短行约 100 字节，长行远超 120；预算取中间值
        let budget = ClaudeIndexBudget {
            max_jsonl_line_bytes: 120,
            ..ClaudeIndexBudget::default()
        };
        let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
        assert!(index.truncated);
        assert!(index
            .diagnostics
            .reasons
            .iter()
            .any(|r| r == "max_jsonl_line_bytes"));
        // 短行应仍被索引
        let sess = index.sessions.get("mixed").expect("session should exist");
        assert!(
            sess.user_text.contains("short ok"),
            "short line should be indexed, user_text={:?}",
            sess.user_text
        );
        assert!(!sess.user_text.contains("ZZZZ"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_total_bytes 必须在累计字节将超时停止后续文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     多个中等文件 + 极小 max_total_bytes，断言 partial index + reason。
    #[test]
    fn budget_max_total_bytes_stops_early() {
        let (worktree, projects, session_dir) = prepare_scan_fixture("budget_max_total");
        for i in 0..4 {
            let body = "Y".repeat(300);
            let line = format!(
                r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:0{i}:00Z"}}"#,
                serde_json::Value::String(body)
            );
            let _ = write_jsonl(&session_dir, &format!("t{i}"), &[line.as_str()]);
        }
        let budget = ClaudeIndexBudget {
            max_total_bytes: 400,
            ..ClaudeIndexBudget::default()
        };
        let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
        assert!(index.sessions.len() < 4);
        assert!(index.truncated);
        assert!(index
            .diagnostics
            .reasons
            .iter()
            .any(|r| r == "max_total_bytes"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_session_chars 必须以 Unicode scalar 截断，且只在 char 边界切断（含中文/emoji）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     title/user/assistant 含中文与 emoji，小 budget 截断后 len 合法、无 panic。
    #[test]
    fn budget_max_session_chars_truncates_at_char_boundary() {
        let tmp = unique_temp_dir("budget_max_session_chars");
        let text = "你好世界🌍🚀测试文本额外内容";
        // 短 title 占 2 scalar，剩余预算给 user_text
        let user_line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
            serde_json::Value::String(text.to_string())
        );
        let path = write_jsonl(
            &tmp,
            "uni",
            &[
                r#"{"type":"last-prompt","lastPrompt":"标题"}"#,
                user_line.as_str(),
            ],
        );
        let budget = ClaudeIndexBudget {
            max_session_chars: 7, // 标题 2 + user 5
            ..ClaudeIndexBudget::default()
        };
        let (idx, outcome) = build_session_index_with_budget(&path, &budget).expect("ok");
        assert!(outcome.reasons.iter().any(|r| r == "max_session_chars"));
        assert_eq!(idx.title.chars().count(), 2);
        assert_eq!(idx.user_text.chars().count(), 5);
        // 截断结果必须是合法 UTF-8 且为原文前缀
        assert!(
            text.starts_with(&idx.user_text),
            "user_text should be a prefix of original, got {:?}",
            idx.user_text
        );
        // 明确 char 边界：重新 collect 应相等
        let recomposed: String = idx.user_text.chars().collect();
        assert_eq!(recomposed, idx.user_text);
        // emoji 截断不 panic：只取前 5 个 scalar（可能含不完整语义但合法 UTF-8）
        assert!(!idx.user_text.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     候选排序必须 mtime desc 再 path asc，保证 max_files 截断确定性。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写 3 个文件并 sleep 拉开 mtime，max_files=2，断言保留最新两个。
    #[test]
    fn scan_orders_by_mtime_desc_then_path_asc() {
        let (worktree, projects, session_dir) = prepare_scan_fixture("scan_order");
        let _a = write_jsonl(
            &session_dir,
            "a-old",
            &[r#"{"type":"user","message":{"role":"user","content":"a"},"timestamp":"2026-01-01T00:00:00Z"}"#],
        );
        std::thread::sleep(Duration::from_millis(1100));
        let _b = write_jsonl(
            &session_dir,
            "b-mid",
            &[r#"{"type":"user","message":{"role":"user","content":"b"},"timestamp":"2026-01-01T00:00:00Z"}"#],
        );
        std::thread::sleep(Duration::from_millis(1100));
        let _c = write_jsonl(
            &session_dir,
            "c-new",
            &[r#"{"type":"user","message":{"role":"user","content":"c"},"timestamp":"2026-01-01T00:00:00Z"}"#],
        );
        let budget = ClaudeIndexBudget {
            max_files: 2,
            ..ClaudeIndexBudget::default()
        };
        let index = scan_worktree_sessions_at(&worktree, Some(&projects), &budget);
        assert_eq!(index.sessions.len(), 2);
        assert!(index.sessions.contains_key("c-new"));
        assert!(index.sessions.contains_key("b-mid"));
        assert!(!index.sessions.contains_key("a-old"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     初始扫描必须在 spawn_blocking 中运行，不阻塞 tokio 心跳。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造多文件 fixture；spawn interval heartbeat + spawn_blocking 紧预算扫描；
    ///     断言 heartbeat≥3 且 truncated。
    #[tokio::test]
    async fn initial_scan_does_not_block_tokio_heartbeat() {
        let (worktree, projects, session_dir) =
            prepare_scan_fixture("initial_scan_heartbeat");
        // 多个中等文件，制造可观测扫描耗时
        // 多个多行 transcript + 故意在 blocking 侧做足量解析工作
        for i in 0..12 {
            let mut lines: Vec<String> = Vec::new();
            for j in 0..1500 {
                lines.push(format!(
                    r#"{{"type":"user","message":{{"role":"user","content":{}}},"timestamp":"2026-01-01T00:00:00Z"}}"#,
                    serde_json::Value::String(format!("hb-{i}-{j}-{}", "p".repeat(128)))
                ));
            }
            let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            let _ = write_jsonl(&session_dir, &format!("hb{i:02}"), &refs);
        }
        let budget = ClaudeIndexBudget {
            max_files: 6,
            max_jsonl_line_bytes: 64 * 1024,
            ..ClaudeIndexBudget::default()
        };
        let worktree2 = worktree.clone();
        let projects2 = projects.clone();
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU32::new(0));
        let stop_hb = Arc::clone(&stop);
        let ticks_hb = Arc::clone(&ticks);
        // 先启动 heartbeat，确保与 blocking 扫描重叠
        let hb = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(2));
            interval.tick().await; // 跳过立即完成的首 tick
            while !stop_hb.load(Ordering::Relaxed) {
                interval.tick().await;
                ticks_hb.fetch_add(1, Ordering::Relaxed);
            }
        });
        // 让 heartbeat 至少先走一拍，再启动扫描
        tokio::time::sleep(Duration::from_millis(5)).await;

        let index = tokio::task::spawn_blocking(move || {
            scan_worktree_sessions_at(&worktree2, Some(&projects2), &budget)
        })
        .await
        .expect("join");
        stop.store(true, Ordering::Relaxed);
        let _ = hb.await;
        let beats = ticks.load(Ordering::Relaxed);
        assert!(index.truncated, "紧预算应 truncated");
        assert!(
            beats >= 3,
            "扫描期间 tokio heartbeat 应 >=3，实际 {beats}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     singleflight 必须让并发 ensure 共享一次扫描（AtomicUsize 计数=1）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 watch + Mutex map 模拟 inflight；两个任务并发进入，work 只应执行一次。
    #[tokio::test]
    async fn singleflight_shares_one_scan() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::sync::{Mutex, Notify};

        let scans = Arc::new(AtomicUsize::new(0));
        type Slot = Arc<(
            Notify,
            std::sync::Mutex<Option<Result<u32, String>>>,
        )>;
        let map: Arc<Mutex<HashMap<String, Slot>>> = Arc::new(Mutex::new(HashMap::new()));

        async fn ensure(
            map: Arc<Mutex<HashMap<String, Slot>>>,
            scans: Arc<AtomicUsize>,
            key: &str,
        ) -> u32 {
            // fast path 省略
            let (slot, is_leader) = {
                let mut g = map.lock().await;
                if let Some(s) = g.get(key) {
                    (Arc::clone(s), false)
                } else {
                    let s = Arc::new((
                        Notify::new(),
                        std::sync::Mutex::new(None::<Result<u32, String>>),
                    ));
                    g.insert(key.to_string(), Arc::clone(&s));
                    (s, true)
                }
            };
            if is_leader {
                let scans2 = Arc::clone(&scans);
                let value = tokio::task::spawn_blocking(move || {
                    scans2.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                    42u32
                })
                .await
                .unwrap();
                *slot.1.lock().unwrap() = Some(Ok(value));
                slot.0.notify_waiters();
                let mut g = map.lock().await;
                g.remove(key);
                value
            } else {
                loop {
                    if let Some(r) = slot.1.lock().unwrap().clone() {
                        return r.unwrap();
                    }
                    slot.0.notified().await;
                }
            }
        }

        let m1 = Arc::clone(&map);
        let s1 = Arc::clone(&scans);
        let m2 = Arc::clone(&map);
        let s2 = Arc::clone(&scans);
        let (a, b) = tokio::join!(
            ensure(m1, s1, "k"),
            ensure(m2, s2, "k"),
        );
        assert_eq!(a, 42);
        assert_eq!(b, 42);
        assert_eq!(scans.load(Ordering::SeqCst), 1, "只应扫描一次");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     新客户端必须能解码旧服务端返回的 `Vec<SessionSearchHit>`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化数组 body，decode 得 truncated=false + unavailable diagnostics。
    #[test]
    fn decode_legacy_array_body_synthesizes_unavailable() {
        let items = vec![SessionSearchHit {
            session_id: "s1".into(),
            title: "t".into(),
            title_hit: true,
            user_hit: false,
            assistant_hit: false,
            first_activity_at: "a".into(),
            last_activity_at: "b".into(),
            message_count: 1,
            preview_snippets: vec![],
        }];
        let bytes = serde_json::to_vec(&items).unwrap();
        let result = decode_session_search_response_body(&bytes).expect("decode");
        assert_eq!(result.items.len(), 1);
        assert!(!result.truncated);
        assert_eq!(result.diagnostics.status, DIAG_STATUS_UNAVAILABLE);
        assert!(result.diagnostics.reasons.is_empty());
        assert_eq!(result.diagnostics.files_indexed, 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧/新客户端都必须能解码 v2 对象 DTO。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 SessionSearchResult 对象，decode 字段完整保留。
    #[test]
    fn decode_v2_object_body_preserves_diagnostics() {
        let dto = SessionSearchResult {
            items: vec![],
            truncated: true,
            diagnostics: SessionSearchDiagnostics::truncated(
                vec!["max_files".into()],
                10,
                2,
                100,
            ),
        };
        let bytes = serde_json::to_vec(&dto).unwrap();
        let result = decode_session_search_response_body(&bytes).expect("decode");
        assert!(result.truncated);
        assert_eq!(result.diagnostics.status, DIAG_STATUS_TRUNCATED);
        assert_eq!(result.diagnostics.files_considered, 10);
        assert_eq!(result.diagnostics.files_indexed, 2);
        assert_eq!(result.diagnostics.bytes_read, 100);
        assert!(result.diagnostics.reasons.iter().any(|r| r == "max_files"));
    }
}
