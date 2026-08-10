//! workbench/auto_title_codex.rs — Codex `thread_name` → Workbench window 自动标题
//!
//! Business Logic（为什么需要这个模块）:
//!     Codex 在 `~/.codex/session_index.jsonl` 维护 `id + thread_name`；工作台 tab 应可跟随。
//!
//! Code Logic（这个模块做什么）:
//!     轮询 session_index（mtime/size 指纹 + 行偏移增量）；解析 JSONL；
//!     绑定 native_session_id 或 rollout session_meta.cwd；调用 auto_title rename。

use crate::state::AppState;
use crate::workbench::auto_title::{is_substantive_auto_title, try_auto_rename_by_native_session};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 轮询间隔（秒）。
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Business Logic（为什么需要这个函数）:
///     Codex 数据根可被 CODEX_HOME 覆盖。
///
/// Code Logic（这个函数做什么）:
///     `CODEX_HOME` 或 `~/.codex`。
pub fn codex_home_dir() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CODEX_HOME") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

/// Business Logic（为什么需要这个函数）:
///     标题权威源是 session_index.jsonl。
///
/// Code Logic（这个函数做什么）:
///     `{codex_home}/session_index.jsonl`。
pub fn session_index_path(home: &Path) -> PathBuf {
    home.join("session_index.jsonl")
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionIndexLine {
    id: String,
    #[serde(default)]
    thread_name: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// 解析单行 session_index；非法行返回 None。
pub fn parse_session_index_line(line: &str) -> Option<SessionIndexLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// Business Logic（为什么需要这个函数）:
///     增量扫描只需处理新增字节，避免每次全量读 100k+ 行。
///
/// Code Logic（这个函数做什么）:
///     从 offset 读到 EOF，解析行；返回 (新 offset, 行列表)。
pub fn read_session_index_from_offset(
    path: &Path,
    offset: u64,
) -> std::io::Result<(u64, Vec<SessionIndexLine>)> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let len = meta.len();
    if offset > len {
        // 文件被截断/轮转：从头读
        file.seek(SeekFrom::Start(0))?;
        let reader = BufReader::new(file);
        let mut lines = Vec::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Some(parsed) = parse_session_index_line(&l) {
                    lines.push(parsed);
                }
            }
        }
        return Ok((len, lines));
    }
    file.seek(SeekFrom::Start(offset))?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines() {
        if let Ok(l) = line {
            if let Some(parsed) = parse_session_index_line(&l) {
                lines.push(parsed);
            }
        }
    }
    Ok((len, lines))
}

/// 从 rollout jsonl 的首条 session_meta 取 cwd（best-effort）。
pub fn cwd_from_codex_rollout(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    for line in reader.lines().take(20) {
        let Ok(l) = line else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
            continue;
        }
        let cwd = v
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        return Some(cwd.to_string());
    }
    None
}

/// 在 sessions 树中按 session id 找 rollout 文件（限深、限文件数）。
pub fn find_rollout_for_session(home: &Path, session_id: &str) -> Option<PathBuf> {
    let sessions = home.join("sessions");
    if !sessions.is_dir() {
        return None;
    }
    let needle = session_id.trim();
    if needle.is_empty() {
        return None;
    }
    // 文件名通常含 uuid：rollout-...-{uuid}.jsonl
    let mut stack = vec![sessions];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            visited += 1;
            if visited > 50_000 {
                return None;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name()?.to_string_lossy();
            if name.contains(needle) && name.ends_with(".jsonl") {
                return Some(path);
            }
        }
    }
    None
}

/// Codex 自动标题轮询主循环。
///
/// Business Logic（为什么需要这个函数）:
///     headless owner 后台持续把 Codex thread_name 同步到绑定终端。
///
/// Code Logic（这个函数做什么）:
///     每 2s 检查 session_index mtime/size；增量读行；对 thread_name 变更做 rename。
pub async fn run_codex_title_poller(state: AppState, cancel: CancellationToken) {
    let Some(home) = codex_home_dir() else {
        tracing::debug!("CODEX_HOME/home 不可用，跳过 Codex 自动标题");
        cancel.cancelled().await;
        return;
    };
    let index_path = session_index_path(&home);
    let mut last_len: u64 = 0;
    let mut last_mtime_ns: Option<u64> = None;
    let mut last_titles: HashMap<String, String> = HashMap::new();
    let mut offset: u64 = 0;
    // 首次：若文件已存在，跳到文件末尾，只跟踪增量（避免启动时批量 rename 历史会话）。
    if let Ok(meta) = fs::metadata(&index_path) {
        last_len = meta.len();
        offset = meta.len();
        last_mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64);
        tracing::debug!(
            path = %index_path.display(),
            offset,
            "Codex session_index 自动标题从 EOF 跟踪"
        );
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        if cancel.is_cancelled() {
            break;
        }
        if !index_path.is_file() {
            continue;
        }
        let meta = match fs::metadata(&index_path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64);
        let len = meta.len();
        if Some(len) == Some(last_len) && mtime_ns == last_mtime_ns {
            continue;
        }
        // 截断则重置
        if len < last_len {
            offset = 0;
            last_titles.clear();
        }
        last_len = len;
        last_mtime_ns = mtime_ns;

        let home_clone = home.clone();
        let path_clone = index_path.clone();
        let start_offset = offset;
        let read = tauri::async_runtime::spawn_blocking(move || {
            read_session_index_from_offset(&path_clone, start_offset)
        })
        .await;
        let (new_offset, lines) = match read {
            Ok(Ok(v)) => v,
            Ok(Err(err)) => {
                tracing::debug!("读取 Codex session_index 失败: {err}");
                continue;
            }
            Err(err) => {
                tracing::debug!("Codex session_index blocking 任务失败: {err}");
                continue;
            }
        };
        offset = new_offset;

        for line in lines {
            let id = line.id.trim().to_string();
            if id.is_empty() {
                continue;
            }
            let Some(title) = line
                .thread_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
            else {
                continue;
            };
            if !is_substantive_auto_title(&title) {
                continue;
            };
            if last_titles.get(&id).map(String::as_str) == Some(title.as_str()) {
                continue;
            }
            last_titles.insert(id.clone(), title.clone());

            let home_for_cwd = home_clone.clone();
            let session_id = id.clone();
            let cwd = tauri::async_runtime::spawn_blocking(move || {
                find_rollout_for_session(&home_for_cwd, &session_id)
                    .and_then(|p| cwd_from_codex_rollout(&p))
            })
            .await
            .ok()
            .flatten();

            let state_clone = state.clone();
            let native = id;
            let title_for_rename = title;
            let _ = tauri::async_runtime::spawn_blocking(move || {
                try_auto_rename_by_native_session(
                    &state_clone,
                    &native,
                    &title_for_rename,
                    cwd.as_deref(),
                    "codex.thread_name",
                )
            })
            .await;
        }
    }
    tracing::debug!("Codex 自动标题轮询已停止");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parse_session_index_line_ok() {
        let line = r#"{"id":"abc","thread_name":"修复滚动","updated_at":"2026-08-09T00:00:00Z"}"#;
        let parsed = parse_session_index_line(line).expect("parse");
        assert_eq!(parsed.id, "abc");
        assert_eq!(parsed.thread_name.as_deref(), Some("修复滚动"));
    }

    #[test]
    fn parse_session_index_line_rejects_garbage() {
        assert!(parse_session_index_line("not-json").is_none());
        assert!(parse_session_index_line("").is_none());
    }

    #[test]
    fn read_from_offset_incremental() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(
                f,
                r#"{{"id":"1","thread_name":"one","updated_at":"t1"}}"#
            )
            .unwrap();
        }
        let (off1, lines1) = read_session_index_from_offset(&path, 0).unwrap();
        assert_eq!(lines1.len(), 1);
        assert_eq!(lines1[0].id, "1");
        {
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(
                f,
                r#"{{"id":"2","thread_name":"two","updated_at":"t2"}}"#
            )
            .unwrap();
        }
        let (_off2, lines2) = read_session_index_from_offset(&path, off1).unwrap();
        assert_eq!(lines2.len(), 1);
        assert_eq!(lines2[0].id, "2");
    }

    #[test]
    fn cwd_from_rollout_reads_session_meta() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        {
            let mut f = File::create(&path).unwrap();
            writeln!(
                f,
                r#"{{"type":"session_meta","payload":{{"id":"x","cwd":"/tmp/proj"}}}}"#
            )
            .unwrap();
            writeln!(f, r#"{{"type":"event_msg","payload":{{}}}}"#).unwrap();
        }
        assert_eq!(
            cwd_from_codex_rollout(&path).as_deref(),
            Some("/tmp/proj")
        );
    }

    #[test]
    fn find_rollout_by_id_in_tree() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let nested = home.join("sessions/2026/08/09");
        fs::create_dir_all(&nested).unwrap();
        let id = "019fe714-f61c-7fa3-a88e-7a0be8272a9c";
        let file = nested.join(format!("rollout-2026-08-09T00-00-00-{id}.jsonl"));
        File::create(&file).unwrap();
        let found = find_rollout_for_session(home, id).expect("found");
        assert_eq!(found, file);
    }
}
