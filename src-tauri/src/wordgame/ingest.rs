//! 从本机 Claude / Codex / OpenCode 落盘抽取 assistant 词频增量。
//!
//! Business Logic（为什么需要这个模块）:
//!     词频必须来自完整 assistant 正文，且同一 record 只能计一次。
//!
//! Code Logic（这个模块做什么）:
//!     扫描三家 provider 落盘，按 session 水位只处理新行；输出 lemma 计数和新水位。
//!     不回传原文。

use super::lexicon::count_lemmas_in_text;
use super::models::{IngestCursor, LemmaCount};
use crate::error::AppError;
use crate::workbench::auto_title_codex::codex_home_dir;
use crate::workbench::auto_title_opencode::resolve_opencode_db_path;
use crate::workbench::claude_sessions::extract_text_from_content;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 4_000;
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// 本机增量抽取结果。
#[derive(Debug, Clone, Default)]
pub struct ExtractDelta {
    pub lemmas: Vec<LemmaCount>,
    pub cursors: Vec<IngestCursor>,
}

/// 在本机扫描三家 provider，只处理水位之后的 assistant 记录。
///
/// Business Logic:
///     玩游戏的机器和远端 owner 共用同一抽取，远端只回计数。
pub async fn extract_local_delta(
    device_id: &str,
    known: &[IngestCursor],
) -> Result<ExtractDelta, AppError> {
    let device_id = device_id.to_string();
    let known = known.to_vec();
    let files_device = device_id.clone();
    let files_known = known.clone();
    let mut delta =
        tokio::task::spawn_blocking(move || extract_files_delta(&files_device, &files_known))
            .await
            .map_err(|e| AppError::generic(e.to_string()))??;
    match scan_opencode(&device_id, &known).await {
        Ok((counts, cursors)) => {
            merge_counts_into_delta(&mut delta, counts, cursors);
        }
        Err(err) => tracing::warn!("wordgame OpenCode 扫描失败: {err}"),
    }
    Ok(delta)
}

fn extract_files_delta(device_id: &str, known: &[IngestCursor]) -> Result<ExtractDelta, AppError> {
    let cursor_map = cursor_index(known, device_id);
    let mut totals: HashMap<String, i64> = HashMap::new();
    let mut new_cursors = Vec::new();
    scan_claude(device_id, &cursor_map, &mut totals, &mut new_cursors)?;
    scan_codex(device_id, &cursor_map, &mut totals, &mut new_cursors)?;
    Ok(delta_from_parts(totals, new_cursors))
}

fn cursor_index(known: &[IngestCursor], device_id: &str) -> HashMap<(String, String), String> {
    known
        .iter()
        .filter(|c| c.device_id == device_id)
        .map(|c| {
            (
                (c.provider.clone(), c.session_id.clone()),
                c.record_id.clone(),
            )
        })
        .collect()
}

fn delta_from_parts(totals: HashMap<String, i64>, cursors: Vec<IngestCursor>) -> ExtractDelta {
    let mut lemmas: Vec<LemmaCount> = totals
        .into_iter()
        .map(|(lemma, count)| LemmaCount { lemma, count })
        .collect();
    lemmas.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.lemma.cmp(&b.lemma)));
    ExtractDelta { lemmas, cursors }
}

fn merge_counts_into_delta(
    delta: &mut ExtractDelta,
    extra: HashMap<String, i64>,
    cursors: Vec<IngestCursor>,
) {
    let mut totals: HashMap<String, i64> = delta
        .lemmas
        .drain(..)
        .map(|item| (item.lemma, item.count))
        .collect();
    merge_counts(&mut totals, extra);
    *delta = delta_from_parts(totals, {
        let mut all = std::mem::take(&mut delta.cursors);
        all.extend(cursors);
        all
    });
}

fn scan_claude(
    device_id: &str,
    cursor_map: &HashMap<(String, String), String>,
    totals: &mut HashMap<String, i64>,
    new_cursors: &mut Vec<IngestCursor>,
) -> Result<(), AppError> {
    let Some(root) = claude_projects_root() else {
        return Ok(());
    };
    if !root.is_dir() {
        return Ok(());
    }
    let mut files = collect_jsonl(&root);
    files.sort();
    files.truncate(MAX_FILES);
    for path in files {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() {
            continue;
        }
        let after = cursor_map
            .get(&("claude".into(), session_id.clone()))
            .cloned()
            .unwrap_or_default();
        let (counts, last) = scan_claude_file(&path, &after)?;
        merge_counts(totals, counts);
        if !last.is_empty() && last != after {
            new_cursors.push(IngestCursor {
                device_id: device_id.to_string(),
                provider: "claude".into(),
                session_id,
                record_id: last,
            });
        }
    }
    Ok(())
}

fn scan_claude_file(path: &Path, after: &str) -> Result<(HashMap<String, i64>, String), AppError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok((HashMap::new(), after.to_string())),
    };
    let reader = BufReader::new(file);
    let mut totals = HashMap::new();
    let mut last = after.to_string();
    let after_num = after.parse::<u64>().unwrap_or(0);
    for (idx, line) in reader.lines().enumerate() {
        let line_no = (idx as u64) + 1;
        let Ok(line) = line else { continue };
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        if line_no <= after_num {
            last = line_no.to_string();
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<ClaudeLine>(&line) else {
            last = line_no.to_string();
            continue;
        };
        last = line_no.to_string();
        if parsed.kind != "assistant" {
            continue;
        }
        let Some(message) = parsed.message else {
            continue;
        };
        if message.role != "assistant" {
            continue;
        }
        let Some(content) = message.content else {
            continue;
        };
        let Some(text) = extract_text_from_content(&content) else {
            continue;
        };
        accumulate(&mut totals, &text);
    }
    Ok((totals, last))
}

fn scan_codex(
    device_id: &str,
    cursor_map: &HashMap<(String, String), String>,
    totals: &mut HashMap<String, i64>,
    new_cursors: &mut Vec<IngestCursor>,
) -> Result<(), AppError> {
    let Some(root) = codex_home_dir().map(|home| home.join("sessions")) else {
        return Ok(());
    };
    if !root.is_dir() {
        return Ok(());
    }
    let mut files = Vec::new();
    collect_named_jsonl(&root, "rollout", &mut files);
    files.sort();
    files.truncate(MAX_FILES);
    for path in files {
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() {
            continue;
        }
        let after = cursor_map
            .get(&("codex".into(), session_id.clone()))
            .cloned()
            .unwrap_or_default();
        let (counts, last) = scan_codex_file(&path, &after)?;
        merge_counts(totals, counts);
        if !last.is_empty() && last != after {
            new_cursors.push(IngestCursor {
                device_id: device_id.to_string(),
                provider: "codex".into(),
                session_id,
                record_id: last,
            });
        }
    }
    Ok(())
}

fn scan_codex_file(path: &Path, after: &str) -> Result<(HashMap<String, i64>, String), AppError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok((HashMap::new(), after.to_string())),
    };
    let reader = BufReader::new(file);
    let mut totals = HashMap::new();
    let mut last = after.to_string();
    let after_num = after.parse::<u64>().unwrap_or(0);
    for (idx, line) in reader.lines().enumerate() {
        let line_no = (idx as u64) + 1;
        let Ok(line) = line else { continue };
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        if line_no <= after_num {
            last = line_no.to_string();
            continue;
        }
        last = line_no.to_string();
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("response_item") {
            continue;
        }
        let Some(payload) = v.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
            continue;
        }
        if payload.get("role").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(text) = extract_codex_assistant_text(payload.get("content")) {
            accumulate(&mut totals, &text);
        }
    }
    Ok((totals, last))
}

fn extract_codex_assistant_text(content: Option<&Value>) -> Option<String> {
    let arr = content?.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "output_text" || ty == "text" {
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

async fn scan_opencode(
    device_id: &str,
    known: &[IngestCursor],
) -> Result<(HashMap<String, i64>, Vec<IngestCursor>), AppError> {
    let Some(db_path) = resolve_opencode_db_path() else {
        return Ok((HashMap::new(), Vec::new()));
    };
    if !db_path.is_file() {
        return Ok((HashMap::new(), Vec::new()));
    }
    let url = format!("sqlite://{}", db_path.display());
    let cursor_map = cursor_index(known, device_id);
    scan_opencode_async(device_id, &url, &cursor_map).await
}

async fn scan_opencode_async(
    device_id: &str,
    url: &str,
    cursor_map: &HashMap<(String, String), String>,
) -> Result<(HashMap<String, i64>, Vec<IngestCursor>), AppError> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Row;
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str(url)
        .map_err(|e| AppError::generic(e.to_string()))?
        .read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| AppError::generic(format!("opencode db: {e}")))?;
    let sessions = sqlx::query("SELECT id FROM session LIMIT 2000")
        .fetch_all(&pool)
        .await
        .map_err(|e| AppError::generic(format!("opencode session: {e}")))?;
    let mut totals = HashMap::new();
    let mut new_cursors = Vec::new();
    for session in sessions {
        let session_id: String = session.try_get("id").unwrap_or_default();
        if session_id.is_empty() {
            continue;
        }
        let after = cursor_map
            .get(&("opencode".into(), session_id.clone()))
            .cloned()
            .unwrap_or_default();
        let messages = sqlx::query(
            "SELECT id, data, time_created FROM message WHERE session_id = ? ORDER BY time_created ASC LIMIT 800",
        )
        .bind(&session_id)
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
        let mut last = after.clone();
        let after_ts = after.parse::<i64>().unwrap_or(0);
        for message in messages {
            let message_id: String = message.try_get("id").unwrap_or_default();
            let ts: i64 = message.try_get("time_created").unwrap_or(0);
            if ts <= after_ts {
                last = ts.to_string();
                continue;
            }
            last = ts.to_string();
            let data: String = message.try_get("data").unwrap_or_default();
            let role = serde_json::from_str::<Value>(&data)
                .ok()
                .and_then(|v| v.get("role").and_then(|r| r.as_str()).map(str::to_string))
                .unwrap_or_default();
            if role != "assistant" {
                continue;
            }
            let parts = sqlx::query("SELECT data FROM part WHERE message_id = ?")
                .bind(&message_id)
                .fetch_all(&pool)
                .await
                .unwrap_or_default();
            let mut texts = Vec::new();
            for part in parts {
                let raw: String = part.try_get("data").unwrap_or_default();
                let Ok(v) = serde_json::from_str::<Value>(&raw) else {
                    continue;
                };
                if v.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        texts.push(trimmed.to_string());
                    }
                }
            }
            if !texts.is_empty() {
                accumulate(&mut totals, &texts.join("\n"));
            }
        }
        if last != after {
            new_cursors.push(IngestCursor {
                device_id: device_id.to_string(),
                provider: "opencode".into(),
                session_id,
                record_id: last,
            });
        }
    }
    Ok((totals, new_cursors))
}

fn accumulate(totals: &mut HashMap<String, i64>, text: &str) {
    for (lemma, count) in count_lemmas_in_text(text) {
        *totals.entry(lemma).or_insert(0) += count;
    }
}

fn merge_counts(dst: &mut HashMap<String, i64>, src: HashMap<String, i64>) {
    for (lemma, count) in src {
        *dst.entry(lemma).or_insert(0) += count;
    }
}

fn claude_projects_root() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("projects"))
}

fn collect_jsonl(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_named_jsonl(root, "", &mut out);
    out
}

fn collect_named_jsonl(root: &Path, needle: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_named_jsonl(&path, needle, out);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        if !needle.is_empty() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !name.contains(needle) {
                continue;
            }
        }
        out.push(path);
        if out.len() >= MAX_FILES {
            return;
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeLine {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<ClaudeMessage>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn claude_file_skips_seen_lines_and_counts_new() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sess.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"Please implement the feature"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":"Please implement another feature"}}}}"#
        )
        .unwrap();
        let (first, last) = scan_claude_file(&path, "").unwrap();
        assert!(first.get("implement").copied().unwrap_or(0) >= 2);
        assert_eq!(last, "2");
        let (second, _) = scan_claude_file(&path, "2").unwrap();
        assert!(second.is_empty());
    }
}
