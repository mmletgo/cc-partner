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
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// 单文件解析超时（2 秒）。活跃 session 可能几 MB，超时则跳过避免阻塞扫描。
const SINGLE_FILE_PARSE_TIMEOUT: Duration = Duration::from_secs(2);

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
///     并记录 encoded_cwd 与扫描时间供文件监听回调复用。
///
/// Code Logic（这个结构体做什么）:
///     不 Serialize（仅供内存索引使用）；sessions 以 session_id 为 key 便于单文件增量更新。
#[derive(Debug, Clone)]
pub struct WorktreeSessionIndex {
    // 保留 worktree_path 便于调试与未来扩展（当前索引内部未直接读取）。
    #[allow(dead_code)]
    pub worktree_path: PathBuf,
    pub encoded_cwd: String,
    pub sessions: HashMap<String, ClaudeSessionIndex>,
    pub last_scan_at: String,
}

/// 搜索命中结果（spec 3.2）。
///
/// Business Logic（为什么需要这个结构体）:
///     前端列表项需要知道命中在哪些字段，preview_snippets 提供命中上下文片段用于展示。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 序列化对齐前端 SessionSearchHit；title_hit/user_hit/assistant_hit 标记命中字段。
///     同时派生 Deserialize，因为 remote shortcut 命令需要反序列化远端设备的搜索响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// 从单个 jsonl 文件构建一个 ClaudeSessionIndex（spec 2.2）。
///
/// Business Logic（为什么需要这个函数）:
///     一个 jsonl 文件 = 一次 Claude 会话的完整 transcript。搜索索引需要从每个文件提取标题、
///     user 文本、assistant 文本、最近 20 条消息和首末活动时间。
///
/// Code Logic（这个函数做什么）:
///     流式 BufReader::lines() 逐行解析，单行失败跳过不阻断；每读一行检查 2 秒超时；
///     收集 last-prompt（取最后一条作 title）、所有 user 文本（过滤 slash/bash 命令）、
///     assistant 文本、(role,text,timestamp) 三元组；组装 ClaudeSessionIndex，
///     recent_messages 按 timestamp 升序取尾部 20 条，title 缺失回退首条 user 文本。
pub fn build_session_index(path: &Path) -> Option<ClaudeSessionIndex> {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let file = std::fs::File::open(path)
        .inspect_err(|err| {
            tracing::warn!("打开 Claude session transcript 失败 {:?}: {err}", path);
        })
        .ok()?;

    use std::io::{BufRead, BufReader};
    let reader = BufReader::new(file);
    let start = Instant::now();

    let mut last_prompt: Option<String> = None;
    let mut first_user_text: Option<String> = None;
    let mut user_parts: Vec<String> = Vec::new();
    let mut assistant_parts: Vec<String> = Vec::new();
    let mut messages: Vec<RecentMessage> = Vec::new();
    let mut timestamps: Vec<String> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut git_branch: Option<String> = None;
    let mut first_activity_at: String = String::new();
    let mut last_activity_at: String = String::new();

    for line_res in reader.lines() {
        if start.elapsed() > SINGLE_FILE_PARSE_TIMEOUT {
            tracing::warn!(
                "解析 Claude session transcript 超时（>{:?}），跳过 {:?}",
                SINGLE_FILE_PARSE_TIMEOUT,
                path
            );
            return None;
        }
        let line = match line_res {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!("读取 jsonl 行失败 {:?}: {err}", path);
                continue;
            }
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
                timestamps.push(ts.to_string());
            }
        }
    }

    let _ = &timestamps; // 仅用于明确意图，首末时间已直接计算
    let title = last_prompt.or(first_user_text).unwrap_or_default();
    let message_count = messages.len() as u32;

    // recent_messages：按 timestamp 升序取尾部 20 条（空字符串视为最早）
    messages.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let recent_messages: Vec<RecentMessage> = if messages.len() > RECENT_MESSAGES_MAX {
        messages.split_off(messages.len() - RECENT_MESSAGES_MAX)
    } else {
        messages
    };

    Some(ClaudeSessionIndex {
        session_id,
        title,
        transcript_path: path.to_path_buf(),
        first_activity_at,
        last_activity_at,
        message_count,
        user_text: user_parts.join("\n"),
        assistant_text: assistant_parts.join("\n"),
        recent_messages,
        cwd,
        git_branch,
    })
}

// ---------------------------------------------------------------------------
// worktree 扫描
// ---------------------------------------------------------------------------

/// 扫描指定 worktree path 对应的 Claude session 索引（spec 3.1）。
///
/// Business Logic（为什么需要这个函数）:
///     搜索范围限定在当前 active worktree，需要把该 worktree 下所有 session jsonl 解析成内存索引。
///
/// Code Logic（这个函数做什么）:
///     1. canonicalize worktree_path（失败用原 path）；
///     2. encoded_cwd = encode_claude_project_path(canonical)；
///     3. claude_projects_dir = ~/.claude/projects；
///     4. target_dir = claude_projects_dir.join(encoded_cwd)，不存在返回空索引；
///     5. 逐文件 build_session_index（单文件超时跳过），组装 WorktreeSessionIndex。
pub fn scan_worktree_sessions(worktree_path: &Path) -> WorktreeSessionIndex {
    let canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let canonical_str = canonical.to_string_lossy().to_string();
    let encoded_cwd = encode_claude_project_path(&canonical_str);

    let mut sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();

    if let Some(projects_dir) = claude_projects_dir() {
        let target_dir = projects_dir.join(&encoded_cwd);
        if target_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&target_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }
                    if let Some(index) = build_session_index(&path) {
                        sessions.insert(index.session_id.clone(), index);
                    }
                }
            }
        }
    }

    WorktreeSessionIndex {
        worktree_path: canonical,
        encoded_cwd,
        sessions,
        last_scan_at: Utc::now().to_rfc3339(),
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

/// 确保 worktree 的 session 索引已扫描并启动文件监听（lazy 初始化）。
///
/// Business Logic（为什么需要这个函数）:
///     首次搜索某 worktree 时才建索引（lazy），避免启动时全量扫描拖慢。建索引后启动 notify 监听，
///     jsonl 文件新增/修改时 debounce 500ms 后增量更新内存索引；监听失败降级为下次搜索时重扫。
///
/// Code Logic（这个函数做什么）:
///     1. 取 worktree_path canonical string 作为 key；
///     2. 读 indexes HashMap，已有 → 直接返回 clone；
///     3. 无 → scan_worktree_sessions 建索引（**不持锁**，扫描可能耗时）；
///     4. 短暂持写锁 insert 并 double-check（防并发重复扫描），写锁立即释放；
///     5. **不持任何锁**调 spawn_session_watcher 启动文件监听（失败降级）。
///
/// **为何写锁不活到函数末尾（防死锁）**：
///     spawn_session_watcher 的失败分支会调 remove_index_on_failure，后者再次获取同一个
///     `RwLock` 的写锁。标准库 RwLock 不支持写锁重入，若本函数在调用 spawn_session_watcher 时
///     仍持有写锁，将必然死锁。故写锁只在 insert 时短暂持有，调用 spawn_session_watcher 前已释放。
pub fn ensure_worktree_session_index_scanned(
    state: &AppState,
    worktree_path: &Path,
) -> SharedWorktreeSessionIndex {
    let canonical = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let key = canonical.to_string_lossy().to_string();

    // 快速路径：读锁查缓存，已有索引直接返回
    {
        let indexes = state
            .workbench_claude_session_indexes
            .read()
            .expect("session indexes 读锁中毒");
        if let Some(existing) = indexes.get(&key) {
            return Arc::clone(existing);
        }
    }

    // 慢路径：先建索引（不持锁，扫描可能耗时）
    let index = scan_worktree_sessions(&canonical);
    let shared = Arc::new(RwLock::new(index));

    // 短暂持写锁 insert 并 double-check（防止并发重复扫描时两个线程都建索引）
    {
        let mut indexes = state
            .workbench_claude_session_indexes
            .write()
            .expect("session indexes 写锁中毒");
        if let Some(existing) = indexes.get(&key) {
            // 另一个线程已经建好索引，用它，丢弃我们刚建的（让 Arc 析构）
            return Arc::clone(existing);
        }
        indexes.insert(key.clone(), Arc::clone(&shared));
    }
    // 写锁已释放

    // 启动文件监听（不持锁，失败时 remove_index_on_failure 自己获取写锁不会重入死锁）
    spawn_session_watcher(state, &key, &canonical, &shared);

    shared
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
///     对每个路径调 build_session_index（成功则 upsert，失败则从索引移除该 session）；
///     同时扫描目录里可能新增的其它 jsonl 文件（兜底），更新 last_scan_at。
fn refresh_sessions_from_paths(
    shared: &SharedWorktreeSessionIndex,
    worktree_canonical: &Path,
    dir: &Path,
    changed_paths: &[PathBuf],
) {
    let mut guard = match shared.write() {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("session index 写锁中毒，跳过增量更新: {err}");
            return;
        }
    };

    for path in changed_paths {
        match build_session_index(path) {
            Some(index) => {
                guard.sessions.insert(index.session_id.clone(), index);
            }
            None => {
                // 文件可能被删除或解析失败，按 stem 移除
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let stem = stem.trim().to_string();
                    if !stem.is_empty() {
                        guard.sessions.remove(&stem);
                    }
                }
            }
        }
    }

    // 兜底：扫描目录里可能新增但未在事件 paths 里的 jsonl
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
            if guard.sessions.contains_key(&stem) {
                continue;
            }
            if let Some(index) = build_session_index(&p) {
                guard.sessions.insert(index.session_id.clone(), index);
            }
        }
    }

    // 保持 worktree_path / encoded_cwd 不变（重扫不改根），只刷新 last_scan_at
    guard.last_scan_at = Utc::now().to_rfc3339();
    let _ = worktree_canonical; // 留作未来按 canonical 校验 cwd 用
}

/// 全量重扫指定目录下所有 jsonl，重建 sessions HashMap（trailing 兜底专用）。
///
/// Business Logic（为什么需要这个函数）:
///     debounce 窗口内可能先后有多个不同 jsonl 文件变更，若 trailing 只重扫最后一批事件路径，
///     较早变更的文件会被漏掉（它们已在 sessions HashMap 里，增量逻辑会跳过）。
///     trailing 兜底必须全量重扫，保证 debounce 窗口内所有变更最终都被刷新。
///
/// Code Logic（这个函数做什么）:
///     持写锁后枚举 dir 下所有 *.jsonl，逐个 build_session_index，**整体替换** sessions HashMap
///     （而非增量 upsert），确保被删除的 session 也能被清理；最后更新 last_scan_at。
fn refresh_all_sessions(shared: &SharedWorktreeSessionIndex, dir: &Path) {
    let mut guard = match shared.write() {
        Ok(g) => g,
        Err(err) => {
            tracing::warn!("session index 写锁中毒，跳过全量重扫: {err}");
            return;
        }
    };

    let mut new_sessions: HashMap<String, ClaudeSessionIndex> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(index) = build_session_index(&p) {
                new_sessions.insert(index.session_id.clone(), index);
            }
        }
    }

    guard.sessions = new_sessions;
    guard.last_scan_at = Utc::now().to_rfc3339();
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
}
