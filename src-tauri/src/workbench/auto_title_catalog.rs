//! workbench/auto_title_catalog.rs — Grok / Gemini / Cursor CLI / Pi 会话绑定
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude/Codex/OpenCode 已有标题轮询会把 native_session_id 写进 Idle 行，
//!     live usage 才能抽 CLI 会话文件。后四个已适配 Agent 以前没有等价绑定，
//!     状态卡「当前会话」会一直显示「未提供」。
//!
//! Code Logic（这个模块做什么）:
//!     每 2s 有界扫描各 CLI 已证实的会话文件，按 native id + cwd 调用
//!     `try_auto_rename_by_native_session`（空标题也绑定，不猜 Cursor IDE transcripts）。

use crate::state::AppState;
use crate::workbench::auto_title::try_auto_rename_by_native_session;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const BOOTSTRAP_RECENT_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_GROK_GROUP_DIRS: usize = 10_000;
const MAX_GROK_SESSION_DIRS: usize = 10_000;
const MAX_GEMINI_PROJECT_DIRS: usize = 10_000;
const MAX_GEMINI_CHAT_FILES: usize = 10_000;
const MAX_CURSOR_CHAT_GROUPS: usize = 10_000;
const MAX_CURSOR_CHAT_DIRS: usize = 10_000;
const MAX_PI_WALK_ENTRIES: usize = 10_000;
const MAX_HINT_JSON_BYTES: u64 = 4 * 1024 * 1024;

/// 一条可绑定到 Workbench 终端的 catalog 会话线索。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSessionHint {
    pub native_session_id: String,
    pub title: String,
    pub cwd: Option<String>,
    pub source_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    pub source_label: &'static str,
    /// 正在跑的 CLI 会话：cwd 兜底绑定时用“现在”，避免长会话 opened_at 早于新 window。
    pub live: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettledHint {
    title: String,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GrokActiveSession {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    opened_at: Option<String>,
}

/// Business Logic（为什么需要这个函数）:
///     Grok 数据根可被 GROK_HOME 覆盖。
///
/// Code Logic（这个函数做什么）:
///     非空 `GROK_HOME`，否则 `~/.grok`。
pub fn grok_home_dir() -> Option<PathBuf> {
    env_or_home("GROK_HOME", ".grok")
}

/// Business Logic（为什么需要这个函数）:
///     Gemini 数据根可被 GEMINI_HOME 覆盖。
///
/// Code Logic（这个函数做什么）:
///     非空 `GEMINI_HOME`，否则 `~/.gemini`。
pub fn gemini_home_dir() -> Option<PathBuf> {
    env_or_home("GEMINI_HOME", ".gemini")
}

/// Business Logic（为什么需要这个函数）:
///     Cursor CLI 数据根可被 CURSOR_HOME 覆盖。
///
/// Code Logic（这个函数做什么）:
///     非空 `CURSOR_HOME`，否则 `~/.cursor`。
pub fn cursor_home_dir() -> Option<PathBuf> {
    env_or_home("CURSOR_HOME", ".cursor")
}

/// Business Logic（为什么需要这个函数）:
///     Pi 官方目录是 `~/.pi/agent`；文档未提供覆盖 env，禁止臆造 `PI_HOME`。
///
/// Code Logic（这个函数做什么）:
///     `home/.pi/agent`。
pub fn pi_home_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".pi").join("agent"))
}

fn env_or_home(var: &str, dot_dir: &str) -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(var) {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(dot_dir))
}

fn parse_rfc3339(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw.trim())
        .ok()
        .map(|v| v.with_timezone(&chrono::Utc))
}

fn system_time_utc(ts: SystemTime) -> Option<chrono::DateTime<chrono::Utc>> {
    let dur = ts.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    chrono::DateTime::<chrono::Utc>::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
}

fn file_mtime_utc(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
    system_time_utc(fs::metadata(path).ok()?.modified().ok()?)
}

fn within_recent_window(
    ts: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    window: Duration,
) -> bool {
    let Some(ts) = ts else {
        return false;
    };
    let delta = now.signed_duration_since(ts);
    delta <= chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::minutes(10))
        && delta >= chrono::Duration::zero() - chrono::Duration::minutes(1)
}

fn read_json_object_capped(path: &Path) -> Option<Value> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_HINT_JSON_BYTES {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    value.is_object().then_some(value)
}

fn push_hint(out: &mut Vec<CatalogSessionHint>, hint: CatalogSessionHint) {
    if hint.native_session_id.is_empty() {
        return;
    }
    if let Some(existing) = out.iter_mut().find(|row| {
        row.source_label == hint.source_label && row.native_session_id == hint.native_session_id
    }) {
        if hint.live {
            existing.live = true;
        }
        if existing.title.trim().is_empty() && !hint.title.trim().is_empty() {
            existing.title = hint.title;
        }
        if existing.cwd.is_none() {
            existing.cwd = hint.cwd;
        }
        if hint.source_updated_at > existing.source_updated_at {
            existing.source_updated_at = hint.source_updated_at;
        }
        return;
    }
    out.push(hint);
}

/// 解析 Grok `active_sessions.json`（正在跑的会话：session_id + cwd）。
///
/// Business Logic（为什么需要这个函数）:
///     这是 Grok 当前会话的权威活信号，不能等 generated_title。
///
/// Code Logic（这个函数做什么）:
///     读数组；非法/空 id 丢弃。
pub fn parse_grok_active_sessions(text: &str) -> Vec<CatalogSessionHint> {
    let Ok(rows) = serde_json::from_str::<Vec<GrokActiveSession>>(text) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter_map(|row| {
            let id = row.session_id.trim().to_string();
            if id.is_empty() {
                return None;
            }
            Some(CatalogSessionHint {
                native_session_id: id,
                title: String::new(),
                cwd: row
                    .cwd
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                source_updated_at: row.opened_at.as_deref().and_then(parse_rfc3339),
                source_label: "grok.session.title",
                live: true,
            })
        })
        .collect()
}

/// 从 Grok `summary.json` 取 id / cwd / 标题。
///
/// Business Logic（为什么需要这个函数）:
///     active_sessions 没有标题；summary 的 generated_title 可改 window 名。
///
/// Code Logic（这个函数做什么）:
///     读 `info.id` / `info.cwd` / `generated_title`；缺 id 返回 None。
pub fn parse_grok_summary(value: &Value) -> Option<CatalogSessionHint> {
    let id = value
        .pointer("/info/id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let title = value
        .get("generated_title")
        .and_then(Value::as_str)
        .or_else(|| value.get("session_summary").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let cwd = value
        .pointer("/info/cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let source_updated_at = value
        .get("updated_at")
        .or_else(|| value.get("last_active_at"))
        .and_then(Value::as_str)
        .and_then(parse_rfc3339);
    Some(CatalogSessionHint {
        native_session_id: id,
        title,
        cwd,
        source_updated_at,
        source_label: "grok.session.title",
        live: false,
    })
}

/// 从 Gemini chat JSON 取 id / cwd / 标题。
pub fn parse_gemini_chat_hint(value: &Value, file_stem: &str) -> Option<CatalogSessionHint> {
    let id = value
        .get("id")
        .or_else(|| value.get("sessionId"))
        .or_else(|| value.pointer("/session/id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let stem = file_stem.trim();
            (!stem.is_empty()).then(|| stem.to_string())
        })?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let cwd = value
        .get("cwd")
        .or_else(|| value.pointer("/session/cwd"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(CatalogSessionHint {
        native_session_id: id,
        title,
        cwd,
        source_updated_at: None,
        source_label: "gemini.session.title",
        live: false,
    })
}

/// 从 Cursor CLI `meta.json` 取 cwd；native id 是 chat 目录名。
///
/// Business Logic（为什么需要这个函数）:
///     Cursor CLI 把 cwd 写在 `~/.cursor/chats/<hash>/<chatId>/meta.json`；
///     不得把 IDE `agent-transcripts` 当成 CLI 会话。
///
/// Code Logic（这个函数做什么）:
///     读 cwd / updatedAtMs；chat_id 空则 None。
pub fn parse_cursor_meta(value: &Value, chat_id: &str) -> Option<CatalogSessionHint> {
    let id = chat_id.trim();
    if id.is_empty() {
        return None;
    }
    let cwd = value
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let source_updated_at = value
        .get("updatedAtMs")
        .or_else(|| value.get("createdAtMs"))
        .and_then(Value::as_i64)
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis);
    Some(CatalogSessionHint {
        native_session_id: id.to_string(),
        title: String::new(),
        cwd,
        source_updated_at,
        source_label: "cursor.session.title",
        live: false,
    })
}

/// 从 Pi session JSONL 头部取 id / cwd / 名称。
///
/// Business Logic（为什么需要这个函数）:
///     Pi 把会话写在 `~/.pi/agent/sessions/**/<timestamp>_<uuid>.jsonl`。
///
/// Code Logic（这个函数做什么）:
///     读前 20 行 `session` / `session_info`；id 缺席时用文件名里的 uuid。
pub fn parse_pi_session_header(path: &Path) -> Option<CatalogSessionHint> {
    let file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if meta.len() > 64 * 1024 * 1024 {
        return None;
    }
    let reader = BufReader::new(file);
    let mut id = None;
    let mut cwd = None;
    let mut title = String::new();
    for line in reader.lines().take(20) {
        let Ok(line) = line else { continue };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
        if !matches!(ty, "session" | "session_info" | "header") && id.is_some() {
            continue;
        }
        if id.is_none() {
            id = value
                .get("id")
                .or_else(|| value.get("sessionId"))
                .or_else(|| value.pointer("/session/id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if cwd.is_none() {
            cwd = value
                .get("cwd")
                .or_else(|| value.pointer("/session/cwd"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
        }
        if title.is_empty() {
            title = value
                .get("name")
                .or_else(|| value.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        if id.is_some() && cwd.is_some() {
            break;
        }
    }
    if id.is_none() {
        id = pi_id_from_filename(path);
    }
    Some(CatalogSessionHint {
        native_session_id: id?,
        title,
        cwd,
        source_updated_at: file_mtime_utc(path),
        source_label: "pi.session.title",
        live: false,
    })
}

fn pi_id_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty() {
        return None;
    }
    let id = stem
        .rsplit_once('_')
        .map(|(_, rest)| rest)
        .unwrap_or(stem)
        .trim();
    (!id.is_empty() && !id.contains('/') && !id.contains("..")).then(|| id.to_string())
}

/// 收集 Grok 活会话 + 近期 summary。
pub fn collect_grok_hints(
    home: &Path,
    now: chrono::DateTime<chrono::Utc>,
    window: Duration,
) -> Vec<CatalogSessionHint> {
    let mut out = Vec::new();
    let active_path = home.join("active_sessions.json");
    if let Ok(text) = fs::read_to_string(&active_path) {
        for hint in parse_grok_active_sessions(&text) {
            push_hint(&mut out, hint);
        }
    }
    let live_ids: Vec<String> = out.iter().map(|h| h.native_session_id.clone()).collect();
    let sessions = home.join("sessions");
    let Ok(groups) = fs::read_dir(&sessions) else {
        return out;
    };
    let mut checked = 0usize;
    for (i, group) in groups.enumerate() {
        if i >= MAX_GROK_GROUP_DIRS {
            break;
        }
        let Ok(group) = group else { continue };
        if !group.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(entries) = fs::read_dir(group.path()) else {
            continue;
        };
        for entry in entries {
            if checked >= MAX_GROK_SESSION_DIRS {
                return out;
            }
            checked += 1;
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let summary = entry.path().join("summary.json");
            let Some(value) = read_json_object_capped(&summary) else {
                continue;
            };
            let Some(mut hint) = parse_grok_summary(&value) else {
                continue;
            };
            if live_ids.iter().any(|id| id == &hint.native_session_id) {
                hint.live = true;
                push_hint(&mut out, hint);
                continue;
            }
            if !within_recent_window(hint.source_updated_at, now, window) {
                continue;
            }
            push_hint(&mut out, hint);
        }
    }
    out
}

/// 收集近期 Gemini chat JSON。
pub fn collect_gemini_hints(
    home: &Path,
    now: chrono::DateTime<chrono::Utc>,
    window: Duration,
) -> Vec<CatalogSessionHint> {
    let mut out = Vec::new();
    let tmp = home.join("tmp");
    let Ok(projects) = fs::read_dir(&tmp) else {
        return out;
    };
    let mut checked = 0usize;
    for (i, project) in projects.enumerate() {
        if i >= MAX_GEMINI_PROJECT_DIRS {
            break;
        }
        let Ok(project) = project else { continue };
        if !project.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let chats = project.path().join("chats");
        let Ok(entries) = fs::read_dir(&chats) else {
            continue;
        };
        for entry in entries {
            if checked >= MAX_GEMINI_CHAT_FILES {
                return out;
            }
            checked += 1;
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let mtime = file_mtime_utc(&path);
            if !within_recent_window(mtime, now, window) {
                continue;
            }
            let Some(value) = read_json_object_capped(&path) else {
                continue;
            };
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let Some(mut hint) = parse_gemini_chat_hint(&value, stem) else {
                continue;
            };
            hint.source_updated_at = mtime;
            push_hint(&mut out, hint);
        }
    }
    out
}

/// 收集近期 Cursor CLI chat `meta.json`（不含 IDE transcripts）。
pub fn collect_cursor_hints(
    home: &Path,
    now: chrono::DateTime<chrono::Utc>,
    window: Duration,
) -> Vec<CatalogSessionHint> {
    let mut out = Vec::new();
    let chats = home.join("chats");
    let Ok(groups) = fs::read_dir(&chats) else {
        return out;
    };
    let mut checked = 0usize;
    for (i, group) in groups.enumerate() {
        if i >= MAX_CURSOR_CHAT_GROUPS {
            break;
        }
        let Ok(group) = group else { continue };
        if !group.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(entries) = fs::read_dir(group.path()) else {
            continue;
        };
        for entry in entries {
            if checked >= MAX_CURSOR_CHAT_DIRS {
                return out;
            }
            checked += 1;
            let Ok(entry) = entry else { continue };
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let chat_id = entry.file_name().to_string_lossy().to_string();
            let meta_path = entry.path().join("meta.json");
            let Some(value) = read_json_object_capped(&meta_path) else {
                continue;
            };
            let Some(hint) = parse_cursor_meta(&value, &chat_id) else {
                continue;
            };
            if !within_recent_window(hint.source_updated_at, now, window) {
                continue;
            }
            push_hint(&mut out, hint);
        }
    }
    out
}

/// 收集近期 Pi session JSONL。
pub fn collect_pi_hints(
    home: &Path,
    now: chrono::DateTime<chrono::Utc>,
    window: Duration,
) -> Vec<CatalogSessionHint> {
    let mut out = Vec::new();
    let root = home.join("sessions");
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_PI_WALK_ENTRIES {
                return out;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = file_mtime_utc(&path);
            if !within_recent_window(mtime, now, window) {
                continue;
            }
            let Some(hint) = parse_pi_session_header(&path) else {
                continue;
            };
            push_hint(&mut out, hint);
        }
    }
    out
}

fn collect_all_hints() -> Vec<CatalogSessionHint> {
    let now = chrono::Utc::now();
    let window = BOOTSTRAP_RECENT_WINDOW;
    let mut out = Vec::new();
    if let Some(home) = grok_home_dir() {
        out.extend(collect_grok_hints(&home, now, window));
    }
    if let Some(home) = gemini_home_dir() {
        out.extend(collect_gemini_hints(&home, now, window));
    }
    if let Some(home) = cursor_home_dir() {
        out.extend(collect_cursor_hints(&home, now, window));
    }
    if let Some(home) = pi_home_dir() {
        out.extend(collect_pi_hints(&home, now, window));
    }
    out
}

fn hint_key(hint: &CatalogSessionHint) -> String {
    format!("{}:{}", hint.source_label, hint.native_session_id)
}

fn apply_hint(
    state: &AppState,
    hint: &CatalogSessionHint,
) -> crate::workbench::auto_title::AutoTitleSyncResult {
    let source_updated_at = if hint.live {
        Some(chrono::Utc::now())
    } else {
        hint.source_updated_at
    };
    try_auto_rename_by_native_session(
        state,
        &hint.native_session_id,
        &hint.title,
        hint.cwd.as_deref(),
        source_updated_at,
        hint.source_label,
    )
}

/// Grok/Gemini/Cursor/Pi native-id 绑定轮询主循环。
///
/// Business Logic（为什么需要这个函数）:
///     owner 进程必须持续把这四家 CLI 的会话 id 写进 Idle 行，live usage 才能抽文件。
///
/// Code Logic（这个函数做什么）:
///     每 2s 扫描已证实布局；RetryableMiss 有界重试 10 分钟。
pub async fn run_catalog_title_poller(state: AppState, cancel: CancellationToken) {
    let mut last_settled: HashMap<String, SettledHint> = HashMap::new();
    let mut pending_since: HashMap<String, std::time::Instant> = HashMap::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        if cancel.is_cancelled() {
            break;
        }

        let hints = tauri::async_runtime::spawn_blocking(collect_all_hints)
            .await
            .unwrap_or_default();

        for hint in hints {
            let key = hint_key(&hint);
            let fingerprint = SettledHint {
                title: hint.title.clone(),
                cwd: hint.cwd.clone(),
            };
            if last_settled.get(&key) == Some(&fingerprint) {
                pending_since.remove(&key);
                continue;
            }
            if pending_since
                .get(&key)
                .is_some_and(|first| first.elapsed() > BOOTSTRAP_RECENT_WINDOW)
            {
                last_settled.insert(key.clone(), fingerprint.clone());
                pending_since.remove(&key);
                continue;
            }
            pending_since
                .entry(key.clone())
                .or_insert_with(std::time::Instant::now);

            let state_clone = state.clone();
            let hint_clone = hint.clone();
            let result =
                tauri::async_runtime::spawn_blocking(move || apply_hint(&state_clone, &hint_clone))
                    .await
                    .ok();
            if result.is_some_and(|value| value.is_settled()) {
                last_settled.insert(key.clone(), fingerprint);
                pending_since.remove(&key);
            }
        }
    }
    tracing::debug!("catalog 自动标题轮询已停止");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parse_grok_active_sessions_reads_id_and_cwd() {
        let text = r#"[{"session_id":"abc-1","pid":1,"cwd":"/tmp/proj","opened_at":"2026-08-26T04:24:56.182644Z"}]"#;
        let hints = parse_grok_active_sessions(text);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].native_session_id, "abc-1");
        assert_eq!(hints[0].cwd.as_deref(), Some("/tmp/proj"));
        assert!(hints[0].live);
        assert_eq!(hints[0].source_label, "grok.session.title");
    }

    #[test]
    fn parse_grok_summary_reads_generated_title() {
        let value = serde_json::json!({
            "info": {"id": "sess-g", "cwd": "/tmp/proj"},
            "generated_title": "Fix clipboard fallback",
            "updated_at": "2026-08-26T12:46:01.216292Z"
        });
        let hint = parse_grok_summary(&value).expect("summary");
        assert_eq!(hint.native_session_id, "sess-g");
        assert_eq!(hint.title, "Fix clipboard fallback");
        assert_eq!(hint.cwd.as_deref(), Some("/tmp/proj"));
        assert!(!hint.live);
    }

    #[test]
    fn collect_grok_merges_active_session_with_summary_title() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        fs::write(
            home.join("active_sessions.json"),
            r#"[{"session_id":"sess-g","cwd":"/tmp/proj","opened_at":"2026-08-26T04:24:56Z"}]"#,
        )
        .unwrap();
        let session = home.join("sessions/encoded/sess-g");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("summary.json"),
            serde_json::json!({
                "info": {"id": "sess-g", "cwd": "/tmp/proj"},
                "generated_title": "Fix clipboard fallback",
                "updated_at": "2020-01-01T00:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
        let hints = collect_grok_hints(home, chrono::Utc::now(), Duration::from_secs(60));
        assert_eq!(hints.len(), 1);
        assert!(hints[0].live);
        assert_eq!(hints[0].title, "Fix clipboard fallback");
    }

    #[test]
    fn parse_gemini_chat_hint_uses_session_id() {
        let value = serde_json::json!({
            "sessionId": "sess-gem",
            "cwd": "/tmp/g",
            "title": "Ask about rust"
        });
        let hint = parse_gemini_chat_hint(&value, "chat-001").expect("gemini");
        assert_eq!(hint.native_session_id, "sess-gem");
        assert_eq!(hint.cwd.as_deref(), Some("/tmp/g"));
        assert_eq!(hint.title, "Ask about rust");
    }

    #[test]
    fn parse_cursor_meta_uses_directory_name() {
        let value = serde_json::json!({
            "cwd": "/Users/hans/web_project/cc-partner",
            "updatedAtMs": 1786949194156i64
        });
        let hint =
            parse_cursor_meta(&value, "fd40a409-8ec6-478a-a39b-542006b9e6ff").expect("cursor");
        assert_eq!(
            hint.native_session_id,
            "fd40a409-8ec6-478a-a39b-542006b9e6ff"
        );
        assert_eq!(
            hint.cwd.as_deref(),
            Some("/Users/hans/web_project/cc-partner")
        );
        assert_eq!(hint.source_label, "cursor.session.title");
    }

    #[test]
    fn parse_pi_header_reads_session_line() {
        let dir = tempdir().unwrap();
        let path = dir
            .path()
            .join("2026-08-26T00-00-00_11111111-2222-3333-4444-555555555555.jsonl");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(
                f,
                r#"{{"type":"session","id":"11111111-2222-3333-4444-555555555555","cwd":"/tmp/pi","name":"Investigate usage"}}"#
            )
            .unwrap();
            writeln!(f, r#"{{"type":"message","message":{{"role":"user"}}}}"#).unwrap();
        }
        let hint = parse_pi_session_header(&path).expect("pi");
        assert_eq!(
            hint.native_session_id,
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(hint.cwd.as_deref(), Some("/tmp/pi"));
        assert_eq!(hint.title, "Investigate usage");
        assert_eq!(hint.source_label, "pi.session.title");
    }

    #[test]
    fn collect_cursor_skips_stale_meta() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let chat = home.join("chats/hash/chat-old");
        fs::create_dir_all(&chat).unwrap();
        fs::write(
            chat.join("meta.json"),
            serde_json::json!({
                "cwd": "/tmp/x",
                "updatedAtMs": 1
            })
            .to_string(),
        )
        .unwrap();
        let hints = collect_cursor_hints(home, chrono::Utc::now(), Duration::from_secs(60));
        assert!(hints.is_empty());
    }
}
