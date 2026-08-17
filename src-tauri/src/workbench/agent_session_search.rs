//! workbench/agent_session_search — Codex / OpenCode session 搜索与 resume 注入
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench Session Search 原先只扫 Claude Code jsonl；用户在同一 worktree 也跑
//!     Codex / OpenCode，需要按 agent 切换搜索历史 session 并一键在新终端 resume。
//!
//! Code Logic（这个模块做什么）:
//!     本机按 worktree path 过滤 Codex rollout / OpenCode SQLite session；搜索/preview 复用
//!     既有 `SessionSearchHit` / `SessionPreview` DTO；resume 命令字符串按 agent 构造。
//!     不做索引 watcher（按需扫描）；远端代理本模块不负责（命令层 local-only 门禁）。

use crate::agent_cli::selectors::normalize_path_for_match;
use crate::error::AppError;
use crate::workbench::auto_title_codex::{codex_home_dir, session_index_path};
use crate::workbench::auto_title_opencode::resolve_opencode_db_path;
use crate::workbench::claude_sessions::{
    RecentMessage, SessionPreview, SessionSearchDiagnostics, SessionSearchHit, SessionSearchResult,
};
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::time::Duration;
use tokio::process::Command;

const DEFAULT_LIMIT: usize = 50;
const PREVIEW_SNIPPET_RADIUS: usize = 30;
const PREVIEW_SNIPPET_MAX: usize = 3;
const MAX_CODEX_FILES: usize = 5_000;
const MAX_CODEX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SESSION_CHARS: usize = 200_000;
const MAX_RECENT: usize = 20;

/// Session Search 支持的 agent 源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSessionSource {
    Claude,
    Codex,
    OpenCode,
    Grok,
    Gemini,
    Cursor,
    Pi,
}

impl AgentSessionSource {
    /// 解析 wire/前端 source token。
    ///
    /// Business Logic: UI tab 与 API 共用 catalog sessionSource。
    /// Code Logic: 大小写不敏感；未知返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "claude" | "claude-code" | "claudecode" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" | "open-code" | "open_code" => Some(Self::OpenCode),
            "grok" | "grok-build" | "grokbuild" => Some(Self::Grok),
            "gemini" | "gemini-cli" | "geminicli" => Some(Self::Gemini),
            "cursor" | "cursor-cli" | "cursorcli" => Some(Self::Cursor),
            "pi" | "pi-coding-agent" | "picodingagent" => Some(Self::Pi),
            _ => None,
        }
    }

    /// wire token。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Grok => "grok",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Pi => "pi",
        }
    }
}

/// 比较两个路径是否同一 worktree（canonicalize 优先）。
fn paths_match(a: &str, b: &str) -> bool {
    if a.trim().is_empty() || b.trim().is_empty() {
        return false;
    }
    normalize_path_for_match(a) == normalize_path_for_match(b)
}

/// 截取命中片段（ASCII 小写定位，保留原文大小写）。
fn collect_snippets(text: &str, query_lower: &str, out: &mut Vec<String>) {
    if query_lower.is_empty() || out.len() >= PREVIEW_SNIPPET_MAX {
        return;
    }
    let chars_orig: Vec<char> = text.chars().collect();
    let chars_lower: Vec<char> = text.to_lowercase().chars().collect();
    let query_chars: Vec<char> = query_lower.chars().collect();
    if query_chars.is_empty() || chars_lower.len() < query_chars.len() {
        return;
    }
    let mut i = 0;
    while i + query_chars.len() <= chars_lower.len() && out.len() < PREVIEW_SNIPPET_MAX {
        if chars_lower[i..i + query_chars.len()] == query_chars[..] {
            let start = i.saturating_sub(PREVIEW_SNIPPET_RADIUS);
            let end = (i + query_chars.len() + PREVIEW_SNIPPET_RADIUS).min(chars_orig.len());
            let snippet: String = chars_orig[start..end].iter().collect();
            if !out.contains(&snippet) {
                out.push(snippet);
            }
            i += query_chars.len();
        } else {
            i += 1;
        }
    }
}

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

fn sort_and_limit(mut hits: Vec<SessionSearchHit>, limit: usize) -> Vec<SessionSearchHit> {
    hits.sort_by(|a, b| {
        let a_prio = hit_priority(a.title_hit, a.user_hit, a.assistant_hit);
        let b_prio = hit_priority(b.title_hit, b.user_hit, b.assistant_hit);
        match a_prio.cmp(&b_prio) {
            std::cmp::Ordering::Equal => b.last_activity_at.cmp(&a.last_activity_at),
            ord => ord,
        }
    });
    hits.truncate(limit.max(1));
    hits
}

fn push_capped(buf: &mut String, piece: &str, budget: &mut usize) {
    if *budget == 0 || piece.is_empty() {
        return;
    }
    let take = piece.chars().take(*budget).collect::<String>();
    *budget = budget.saturating_sub(take.chars().count());
    if !buf.is_empty() {
        buf.push('\n');
        *budget = budget.saturating_sub(1);
    }
    buf.push_str(&take);
}

fn ms_to_rfc3339(ms: i64) -> String {
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CodexSessionRecord {
    session_id: String,
    title: String,
    cwd: Option<String>,
    first_activity_at: String,
    last_activity_at: String,
    message_count: u32,
    user_text: String,
    assistant_text: String,
    recent_messages: Vec<RecentMessage>,
}

#[derive(Debug, Deserialize)]
struct CodexOuterLine {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    timestamp: Option<String>,
}

fn is_systemish_user_text(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<recommended_plugins>")
        || t.starts_with("<skill>")
        || t.starts_with("<app-context>")
        || t.starts_with("<environment_context>")
        || t.starts_with("<environment_info>")
        || t.starts_with("<system>")
        || t.starts_with("<developer_message>")
}

fn extract_codex_content_text(content: &Value, role: &str) -> Option<String> {
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let want = if role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        // Codex 也可能用 type=text
        if ty == want || ty == "text" {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed.to_string());
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn session_meta_from_rollout(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = File::open(path) else {
        return (None, None);
    };
    let reader = BufReader::new(file);
    let mut cwd = None;
    let mut session_id = None;
    for line in reader.lines().take(40) {
        let Ok(l) = line else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
            continue;
        }
        if let Some(p) = v.get("payload") {
            if cwd.is_none() {
                cwd = p
                    .get("cwd")
                    .and_then(|c| c.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
            if session_id.is_none() {
                session_id = p
                    .get("id")
                    .or_else(|| p.get("session_id"))
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
            }
        }
        if cwd.is_some() && session_id.is_some() {
            break;
        }
    }
    (cwd, session_id)
}

fn collect_codex_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 50_000 || out.len() >= MAX_CODEX_FILES {
                return out;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    out
}

fn load_codex_titles(home: &Path) -> HashMap<String, String> {
    let path = session_index_path(home);
    let Ok(file) = File::open(path) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = v.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        if let Some(name) = v
            .get("thread_name")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            map.insert(id.to_string(), name.to_string());
        }
    }
    map
}

fn parse_codex_rollout(
    path: &Path,
    titles: &HashMap<String, String>,
) -> Option<CodexSessionRecord> {
    let md = fs::metadata(path).ok()?;
    if md.len() > MAX_CODEX_FILE_BYTES {
        return None;
    }
    let (cwd, session_from_meta) = session_meta_from_rollout(path);
    let fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    // 文件名常含 uuid：rollout-...-{uuid}
    let session_id = session_from_meta.unwrap_or_else(|| {
        // 尝试从文件名提取 UUID 形态
        let name = fallback.clone();
        if let Some(idx) = name.rfind('-') {
            let tail = &name[idx + 1..];
            if tail.len() >= 8 {
                return tail.to_string();
            }
        }
        name
    });

    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut user_text = String::new();
    let mut assistant_text = String::new();
    let mut recent: Vec<RecentMessage> = Vec::new();
    let mut first_at: Option<String> = None;
    let mut last_at: Option<String> = None;
    let mut message_count = 0u32;
    let mut user_budget = MAX_SESSION_CHARS;
    let mut assistant_budget = MAX_SESSION_CHARS;
    let mut first_user: Option<String> = None;

    for line_res in reader.lines() {
        let Ok(line) = line_res else { continue };
        let Ok(outer) = serde_json::from_str::<CodexOuterLine>(&line) else {
            continue;
        };
        if outer.r#type != "response_item" {
            continue;
        }
        let Some(payload) = outer.payload else {
            continue;
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let Some(content) = payload.get("content") else {
            continue;
        };
        let Some(text) = extract_codex_content_text(content, role) else {
            continue;
        };
        if role == "user" && is_systemish_user_text(&text) {
            continue;
        }
        let ts = outer
            .timestamp
            .clone()
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        if first_at.is_none() {
            first_at = Some(ts.clone());
        }
        last_at = Some(ts.clone());
        message_count = message_count.saturating_add(1);

        if role == "user" {
            if first_user.is_none() {
                first_user = Some(text.chars().take(200).collect());
            }
            push_capped(&mut user_text, &text, &mut user_budget);
            recent.push(RecentMessage {
                role: "user".into(),
                text: text.chars().take(2000).collect(),
                timestamp: ts,
            });
        } else if role == "assistant" {
            push_capped(&mut assistant_text, &text, &mut assistant_budget);
            recent.push(RecentMessage {
                role: "assistant".into(),
                text: text.chars().take(2000).collect(),
                timestamp: ts,
            });
        }
        if recent.len() > MAX_RECENT {
            let drain = recent.len() - MAX_RECENT;
            recent.drain(0..drain);
        }
    }

    if message_count == 0 && first_user.is_none() {
        // 允许仅 meta 的空会话？不收录
        return None;
    }

    let title = titles
        .get(&session_id)
        .cloned()
        .or_else(|| first_user.clone())
        .unwrap_or_else(|| session_id.chars().take(8).collect::<String>());

    let first_activity_at = first_at.unwrap_or_else(|| Utc::now().to_rfc3339());
    let last_activity_at = last_at.unwrap_or_else(|| first_activity_at.clone());

    Some(CodexSessionRecord {
        session_id,
        title,
        cwd,
        first_activity_at,
        last_activity_at,
        message_count,
        user_text,
        assistant_text,
        recent_messages: recent,
    })
}

fn load_codex_sessions_for_worktree(worktree_path: &str) -> Vec<CodexSessionRecord> {
    let Some(home) = codex_home_dir() else {
        return Vec::new();
    };
    let sessions_dir = home.join("sessions");
    if !sessions_dir.is_dir() {
        return Vec::new();
    }
    let titles = load_codex_titles(&home);
    let mut out = Vec::new();
    for path in collect_codex_jsonl(&sessions_dir) {
        let Some(rec) = parse_codex_rollout(&path, &titles) else {
            continue;
        };
        let Some(cwd) = rec.cwd.as_deref() else {
            continue;
        };
        if paths_match(cwd, worktree_path) {
            out.push(rec);
        }
    }
    out
}

/// 搜索当前 worktree 下的 Codex session。
pub fn search_codex_sessions(
    worktree_path: &str,
    query: &str,
    limit: usize,
) -> SessionSearchResult {
    let records = load_codex_sessions_for_worktree(worktree_path);
    let files_considered = records.len() as u64;
    let query_trimmed = query.trim();
    let limit = if limit == 0 { DEFAULT_LIMIT } else { limit };
    let mut hits = Vec::new();

    if query_trimmed.is_empty() {
        let mut all = records;
        all.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        for rec in all.into_iter().take(limit) {
            hits.push(SessionSearchHit {
                session_id: rec.session_id,
                title: rec.title,
                title_hit: false,
                user_hit: false,
                assistant_hit: false,
                first_activity_at: rec.first_activity_at,
                last_activity_at: rec.last_activity_at,
                message_count: rec.message_count,
                preview_snippets: Vec::new(),
            });
        }
    } else {
        let q = query_trimmed.to_lowercase();
        for rec in records {
            let title_hit = rec.title.to_lowercase().contains(&q);
            let user_hit = rec.user_text.to_lowercase().contains(&q);
            let assistant_hit = rec.assistant_text.to_lowercase().contains(&q);
            if !title_hit && !user_hit && !assistant_hit {
                continue;
            }
            let mut snippets = Vec::new();
            if title_hit {
                collect_snippets(&rec.title, &q, &mut snippets);
            }
            if user_hit {
                collect_snippets(&rec.user_text, &q, &mut snippets);
            }
            if assistant_hit {
                collect_snippets(&rec.assistant_text, &q, &mut snippets);
            }
            hits.push(SessionSearchHit {
                session_id: rec.session_id,
                title: rec.title,
                title_hit,
                user_hit,
                assistant_hit,
                first_activity_at: rec.first_activity_at,
                last_activity_at: rec.last_activity_at,
                message_count: rec.message_count,
                preview_snippets: snippets,
            });
        }
        hits = sort_and_limit(hits, limit);
    }

    let indexed = hits.len() as u64;
    SessionSearchResult {
        items: hits,
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(
            files_considered,
            indexed.min(files_considered),
            0,
        ),
    }
}

/// Codex session preview。
pub fn preview_codex_session(
    worktree_path: &str,
    session_id: &str,
) -> Result<SessionPreview, AppError> {
    let records = load_codex_sessions_for_worktree(worktree_path);
    let rec = records
        .into_iter()
        .find(|r| r.session_id == session_id)
        .ok_or_else(|| AppError::not_found("Codex session 不存在"))?;
    Ok(SessionPreview {
        session_id: rec.session_id,
        title: rec.title,
        cwd: rec.cwd,
        git_branch: None,
        first_activity_at: rec.first_activity_at,
        last_activity_at: rec.last_activity_at,
        message_count: rec.message_count,
        recent_messages: rec.recent_messages,
    })
}

// ---------------------------------------------------------------------------
// OpenCode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct OpenCodeSessionRecord {
    session_id: String,
    title: String,
    cwd: Option<String>,
    first_activity_at: String,
    last_activity_at: String,
    message_count: u32,
    user_text: String,
    assistant_text: String,
    recent_messages: Vec<RecentMessage>,
}

async fn load_opencode_sessions_for_worktree_async(
    worktree_path: &str,
) -> Result<Vec<OpenCodeSessionRecord>, AppError> {
    let Some(db_path) = resolve_opencode_db_path() else {
        return Ok(Vec::new());
    };
    let url = format!("sqlite:{}?mode=ro", db_path.display());
    let options = SqliteConnectOptions::from_str(&url)
        .map_err(|e| AppError::generic(format!("opencode connect options: {e}")))?
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| AppError::generic(format!("opencode connect: {e}")))?;

    let sessions = sqlx::query(
        "SELECT id, title, directory, time_created, time_updated \
         FROM session \
         ORDER BY time_updated DESC \
         LIMIT 2000",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| AppError::generic(format!("opencode session query: {e}")))?;

    let mut out = Vec::new();
    for row in sessions {
        let id: String = row
            .try_get("id")
            .map_err(|e| AppError::generic(e.to_string()))?;
        let title: Option<String> = row.try_get("title").ok();
        let directory: Option<String> = row.try_get("directory").ok();
        let time_created: i64 = row.try_get("time_created").unwrap_or(0);
        let time_updated: i64 = row.try_get("time_updated").unwrap_or(time_created);

        let Some(dir) = directory
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !paths_match(dir, worktree_path) {
            continue;
        }

        let messages = sqlx::query(
            "SELECT id, data, time_created FROM message \
             WHERE session_id = ? ORDER BY time_created ASC LIMIT 500",
        )
        .bind(&id)
        .fetch_all(&pool)
        .await
        .map_err(|e| AppError::generic(format!("opencode message query: {e}")))?;

        let mut user_text = String::new();
        let mut assistant_text = String::new();
        let mut recent: Vec<RecentMessage> = Vec::new();
        let mut message_count = 0u32;
        let mut user_budget = MAX_SESSION_CHARS;
        let mut assistant_budget = MAX_SESSION_CHARS;
        let mut first_user: Option<String> = None;

        for m in messages {
            let message_id: String = m.try_get("id").unwrap_or_default();
            let data: String = m.try_get("data").unwrap_or_default();
            let ts_ms: i64 = m.try_get("time_created").unwrap_or(0);
            let role = serde_json::from_str::<Value>(&data)
                .ok()
                .and_then(|v| {
                    v.get("role")
                        .and_then(|r| r.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if role != "user" && role != "assistant" {
                continue;
            }

            let parts =
                sqlx::query("SELECT data FROM part WHERE message_id = ? ORDER BY time_created ASC")
                    .bind(&message_id)
                    .fetch_all(&pool)
                    .await
                    .unwrap_or_default();
            let mut texts = Vec::new();
            for p in parts {
                let raw: String = p.try_get("data").unwrap_or_default();
                let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                if v.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        texts.push(trimmed.to_string());
                    }
                }
            }
            if texts.is_empty() {
                continue;
            }
            let text = texts.join("\n");
            let ts = ms_to_rfc3339(ts_ms);
            message_count = message_count.saturating_add(1);
            if role == "user" {
                if first_user.is_none() {
                    first_user = Some(text.chars().take(200).collect());
                }
                push_capped(&mut user_text, &text, &mut user_budget);
            } else {
                push_capped(&mut assistant_text, &text, &mut assistant_budget);
            }
            recent.push(RecentMessage {
                role: role.clone(),
                text: text.chars().take(2000).collect(),
                timestamp: ts,
            });
            if recent.len() > MAX_RECENT {
                let drain = recent.len() - MAX_RECENT;
                recent.drain(0..drain);
            }
        }

        let title = title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or(first_user)
            .unwrap_or_else(|| id.chars().take(8).collect());

        out.push(OpenCodeSessionRecord {
            session_id: id,
            title,
            cwd: Some(dir.to_string()),
            first_activity_at: ms_to_rfc3339(time_created),
            last_activity_at: ms_to_rfc3339(time_updated),
            message_count,
            user_text,
            assistant_text,
            recent_messages: recent,
        });
    }

    pool.close().await;
    Ok(out)
}

/// 搜索当前 worktree 下的 OpenCode session（async，只读 DB）。
pub async fn search_opencode_sessions(
    worktree_path: &str,
    query: &str,
    limit: usize,
) -> Result<SessionSearchResult, AppError> {
    let records = load_opencode_sessions_for_worktree_async(worktree_path).await?;
    let files_considered = records.len() as u64;
    let query_trimmed = query.trim();
    let limit = if limit == 0 { DEFAULT_LIMIT } else { limit };
    let mut hits = Vec::new();

    if query_trimmed.is_empty() {
        let mut all = records;
        all.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        for rec in all.into_iter().take(limit) {
            hits.push(SessionSearchHit {
                session_id: rec.session_id,
                title: rec.title,
                title_hit: false,
                user_hit: false,
                assistant_hit: false,
                first_activity_at: rec.first_activity_at,
                last_activity_at: rec.last_activity_at,
                message_count: rec.message_count,
                preview_snippets: Vec::new(),
            });
        }
    } else {
        let q = query_trimmed.to_lowercase();
        for rec in records {
            let title_hit = rec.title.to_lowercase().contains(&q);
            let user_hit = rec.user_text.to_lowercase().contains(&q);
            let assistant_hit = rec.assistant_text.to_lowercase().contains(&q);
            if !title_hit && !user_hit && !assistant_hit {
                continue;
            }
            let mut snippets = Vec::new();
            if title_hit {
                collect_snippets(&rec.title, &q, &mut snippets);
            }
            if user_hit {
                collect_snippets(&rec.user_text, &q, &mut snippets);
            }
            if assistant_hit {
                collect_snippets(&rec.assistant_text, &q, &mut snippets);
            }
            hits.push(SessionSearchHit {
                session_id: rec.session_id,
                title: rec.title,
                title_hit,
                user_hit,
                assistant_hit,
                first_activity_at: rec.first_activity_at,
                last_activity_at: rec.last_activity_at,
                message_count: rec.message_count,
                preview_snippets: snippets,
            });
        }
        hits = sort_and_limit(hits, limit);
    }

    let indexed = hits.len() as u64;
    Ok(SessionSearchResult {
        items: hits,
        truncated: false,
        diagnostics: SessionSearchDiagnostics::ok(
            files_considered,
            indexed.min(files_considered),
            0,
        ),
    })
}

/// OpenCode session preview。
pub async fn preview_opencode_session(
    worktree_path: &str,
    session_id: &str,
) -> Result<SessionPreview, AppError> {
    let records = load_opencode_sessions_for_worktree_async(worktree_path).await?;
    let rec = records
        .into_iter()
        .find(|r| r.session_id == session_id)
        .ok_or_else(|| AppError::not_found("OpenCode session 不存在"))?;
    Ok(SessionPreview {
        session_id: rec.session_id,
        title: rec.title,
        cwd: rec.cwd,
        git_branch: None,
        first_activity_at: rec.first_activity_at,
        last_activity_at: rec.last_activity_at,
        message_count: rec.message_count,
        recent_messages: rec.recent_messages,
    })
}

// ---------------------------------------------------------------------------
// Grok / Gemini catalog session search
// ---------------------------------------------------------------------------

/// 按身份目录 source 搜索（Grok 读 summary.json；Gemini 暂扫 chats 目录）。
pub fn search_catalog_sessions(
    source: AgentSessionSource,
    worktree_path: &str,
    query: &str,
    limit: usize,
) -> Result<SessionSearchResult, AppError> {
    match source {
        AgentSessionSource::Grok => search_grok_sessions(worktree_path, query, limit),
        AgentSessionSource::Gemini => search_gemini_sessions(worktree_path, query, limit),
        AgentSessionSource::Cursor => search_cursor_sessions(worktree_path, query, limit),
        _ => Ok(SessionSearchResult {
            items: Vec::new(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::unavailable(),
        }),
    }
}

/// 预览 catalog session。
pub fn preview_catalog_session(
    source: AgentSessionSource,
    worktree_path: &str,
    session_id: &str,
) -> Result<SessionPreview, AppError> {
    let result = search_catalog_sessions(source, worktree_path, "", 200)?;
    let hit = result
        .items
        .into_iter()
        .find(|h| h.session_id == session_id)
        .ok_or_else(|| AppError::not_found("session 不存在"))?;
    Ok(SessionPreview {
        session_id: hit.session_id,
        title: hit.title,
        cwd: None,
        git_branch: None,
        first_activity_at: hit.first_activity_at,
        last_activity_at: hit.last_activity_at,
        message_count: hit.message_count,
        recent_messages: Vec::new(),
    })
}

fn grok_sessions_root() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GROK_HOME") {
        if !home.trim().is_empty() {
            return Some(PathBuf::from(home).join("sessions"));
        }
    }
    dirs::home_dir().map(|h| h.join(".grok").join("sessions"))
}

fn search_grok_sessions(
    worktree_path: &str,
    query: &str,
    limit: usize,
) -> Result<SessionSearchResult, AppError> {
    let Some(root) = grok_sessions_root() else {
        return Ok(SessionSearchResult {
            items: Vec::new(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::unavailable(),
        });
    };
    if !root.is_dir() {
        return Ok(SessionSearchResult {
            items: Vec::new(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::unavailable(),
        });
    }
    let query_lower = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut files_considered = 0u64;
    let mut files_indexed = 0u64;
    let groups = match fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(_) => {
            return Ok(SessionSearchResult {
                items: Vec::new(),
                truncated: false,
                diagnostics: SessionSearchDiagnostics::unavailable(),
            });
        }
    };
    for group in groups.flatten() {
        let group_path = group.path();
        if !group_path.is_dir() {
            continue;
        }
        let sessions = match fs::read_dir(&group_path) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for entry in sessions.flatten() {
            let session_dir = entry.path();
            let summary_path = session_dir.join("summary.json");
            if !summary_path.is_file() {
                continue;
            }
            files_considered += 1;
            let Ok(text) = fs::read_to_string(&summary_path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let cwd = value
                .pointer("/info/cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !cwd.is_empty() && !paths_match(cwd, worktree_path) {
                continue;
            }
            files_indexed += 1;
            let session_id = value
                .pointer("/info/id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if session_id.is_empty() {
                continue;
            }
            let title = value
                .get("generated_title")
                .and_then(|v| v.as_str())
                .or_else(|| value.get("session_summary").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let last = value
                .get("updated_at")
                .or_else(|| value.get("last_active_at"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let first = value
                .get("created_at")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| last.clone());
            let message_count = value
                .get("num_chat_messages")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let title_hit = !query_lower.is_empty() && title.to_lowercase().contains(&query_lower);
            let user_hit = title_hit;
            if !query_lower.is_empty() && !title_hit {
                continue;
            }
            hits.push(SessionSearchHit {
                session_id,
                title,
                title_hit,
                user_hit,
                assistant_hit: false,
                first_activity_at: first,
                last_activity_at: last,
                message_count,
                preview_snippets: Vec::new(),
            });
        }
    }
    Ok(SessionSearchResult {
        items: sort_and_limit(hits, limit),
        truncated: files_considered > files_indexed,
        diagnostics: SessionSearchDiagnostics {
            status: "ok".into(),
            reasons: Vec::new(),
            files_considered,
            files_indexed,
            bytes_read: 0,
        },
    })
}

fn search_gemini_sessions(
    worktree_path: &str,
    query: &str,
    limit: usize,
) -> Result<SessionSearchResult, AppError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(SessionSearchResult {
            items: Vec::new(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::unavailable(),
        });
    };
    let tmp = home.join(".gemini").join("tmp");
    if !tmp.is_dir() {
        return Ok(SessionSearchResult {
            items: Vec::new(),
            truncated: false,
            diagnostics: SessionSearchDiagnostics::unavailable(),
        });
    }
    let query_lower = query.trim().to_lowercase();
    let mut hits = Vec::new();
    let mut files_considered = 0u64;
    let mut files_indexed = 0u64;
    let projects = match fs::read_dir(&tmp) {
        Ok(rd) => rd,
        Err(_) => {
            return Ok(SessionSearchResult {
                items: Vec::new(),
                truncated: false,
                diagnostics: SessionSearchDiagnostics::unavailable(),
            });
        }
    };
    for project in projects.flatten() {
        let chats = project.path().join("chats");
        if !chats.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&chats) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            files_considered += 1;
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let cwd = value
                .get("cwd")
                .or_else(|| value.pointer("/session/cwd"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !cwd.is_empty() && !paths_match(cwd, worktree_path) {
                continue;
            }
            files_indexed += 1;
            let session_id = value
                .get("id")
                .or_else(|| value.get("sessionId"))
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or(""))
                .to_string();
            if session_id.is_empty() {
                continue;
            }
            let title = value
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&session_id)
                .to_string();
            if !query_lower.is_empty() && !title.to_lowercase().contains(&query_lower) {
                continue;
            }
            hits.push(SessionSearchHit {
                session_id,
                title,
                title_hit: !query_lower.is_empty(),
                user_hit: false,
                assistant_hit: false,
                first_activity_at: String::new(),
                last_activity_at: String::new(),
                message_count: 0,
                preview_snippets: Vec::new(),
            });
        }
    }
    Ok(SessionSearchResult {
        items: sort_and_limit(hits, limit),
        truncated: false,
        diagnostics: SessionSearchDiagnostics {
            status: "ok".into(),
            reasons: Vec::new(),
            files_considered,
            files_indexed,
            bytes_read: 0,
        },
    })
}

/// Cursor CLI 会话布局尚未作为合同 evidence 固化；v1 保持 fail-closed 空结果。
///
/// Business Logic: Hub/Runtime 已登记 `cursor` session source，搜索不得猜路径或误扫 Claude jsonl。
/// Code Logic: 返回 unavailable 诊断，不遍历磁盘。
fn search_cursor_sessions(
    _worktree_path: &str,
    _query: &str,
    _limit: usize,
) -> Result<SessionSearchResult, AppError> {
    Ok(SessionSearchResult {
        items: Vec::new(),
        truncated: false,
        diagnostics: SessionSearchDiagnostics::unavailable(),
    })
}

// ---------------------------------------------------------------------------
// Resume 命令与 CLI 探测
// ---------------------------------------------------------------------------

/// 构造注入终端的 resume 命令行（含尾部换行）。
pub fn build_resume_command(source: AgentSessionSource, session_id: &str) -> String {
    let id = session_id.trim();
    match source {
        AgentSessionSource::Claude => {
            // Claude 路径由调用方用配置 CLI 覆盖；此处仅 fallback
            format!("claude --dangerously-skip-permissions --resume {id}\n")
        }
        AgentSessionSource::Codex => format!("codex resume {id}\n"),
        AgentSessionSource::OpenCode => format!("opencode --session {id}\n"),
        AgentSessionSource::Grok => format!("grok --resume {id}\n"),
        AgentSessionSource::Gemini => format!("gemini --resume {id}\n"),
        AgentSessionSource::Cursor => format!("agent --resume {id}\n"),
        AgentSessionSource::Pi => format!("pi --session {id}\n"),
    }
}

/// 检测 PATH 上 CLI 是否可用（`--version`，2s）。
pub async fn check_agent_cli_available(source: AgentSessionSource) -> Result<(), AppError> {
    let exe = match source {
        AgentSessionSource::Claude => "claude",
        AgentSessionSource::Codex => "codex",
        AgentSessionSource::OpenCode => "opencode",
        AgentSessionSource::Grok => "grok",
        AgentSessionSource::Gemini => "gemini",
        AgentSessionSource::Cursor => "agent",
        AgentSessionSource::Pi => "pi",
    };
    let mut child = Command::new(exe)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            AppError::generic(format!(
                "{exe} CLI 不可用：未找到可执行文件，请确认已安装并配置 PATH"
            ))
        })?;
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(_)) => Ok(()), // 部分 CLI --version 非 0 仍算存在
        Ok(Err(e)) => Err(AppError::generic(format!("{exe} CLI 探测失败：{e}"))),
        Err(_) => {
            let _ = child.kill().await;
            Err(AppError::generic(format!("{exe} CLI 探测超时")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_tokens() {
        assert_eq!(
            AgentSessionSource::parse("claude"),
            Some(AgentSessionSource::Claude)
        );
        assert_eq!(
            AgentSessionSource::parse("CODEX"),
            Some(AgentSessionSource::Codex)
        );
        assert_eq!(
            AgentSessionSource::parse("openCode"),
            Some(AgentSessionSource::OpenCode)
        );
        assert_eq!(
            AgentSessionSource::parse("gemini"),
            Some(AgentSessionSource::Gemini)
        );
        assert_eq!(
            AgentSessionSource::parse("grok"),
            Some(AgentSessionSource::Grok)
        );
        assert_eq!(
            AgentSessionSource::parse("cursor"),
            Some(AgentSessionSource::Cursor)
        );
        assert_eq!(
            AgentSessionSource::parse("pi"),
            Some(AgentSessionSource::Pi)
        );
        assert_eq!(AgentSessionSource::parse("antigravity"), None);
    }

    #[test]
    fn resume_command_shapes() {
        assert!(
            build_resume_command(AgentSessionSource::Codex, "abc").starts_with("codex resume abc")
        );
        assert!(build_resume_command(AgentSessionSource::OpenCode, "s1").contains("--session s1"));
    }

    #[test]
    fn paths_match_normalizes() {
        // 相对与绝对可能因 canonicalize 失败走字面比较；至少相同字面应匹配
        assert!(paths_match("/tmp/foo", "/tmp/foo"));
        assert!(!paths_match("/tmp/foo", "/tmp/bar"));
    }
}
