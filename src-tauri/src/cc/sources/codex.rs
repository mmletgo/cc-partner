//! Codex rollout jsonl → Prompt 历史采集
//!
//! Business Logic: 从 `~/.codex/sessions/**/*.jsonl` 提取用户自然语言输入。
//! Code Logic: mtime/size 增量；过滤 system/skill 注入；id=`codex:{session}:{msg_id}`。

use crate::cc::models::{ClaudeHistoryRow, SOURCE_CODEX};
use crate::cc::project_identity::canonical_project_path;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::auto_title_codex::codex_home_dir;
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const SCAN_PREFIX: &str = "codex-user-v1:";
const MAX_FILES: usize = 5_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct OuterLine {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    timestamp: Option<String>,
}

/// 判断文本是否为系统/技能注入而非用户直接输入。
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

/// 从 content 数组抽取 input_text 拼接。
fn extract_input_text(content: &Value) -> Option<String> {
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        if item.get("type").and_then(|t| t.as_str()) == Some("input_text") {
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

/// 解析一行 rollout，若为用户消息则产出 (session_id, msg_id, text, ts_opt)。
fn parse_user_message(line: &str) -> Option<(String, String, String, Option<String>)> {
    let outer: OuterLine = serde_json::from_str(line).ok()?;
    if outer.r#type != "response_item" {
        return None;
    }
    let payload = outer.payload?;
    if payload.get("type").and_then(|t| t.as_str()) != Some("message") {
        return None;
    }
    if payload.get("role").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    let msg_id = payload
        .get("id")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())?
        .to_string();
    let content = payload.get("content")?;
    let text = extract_input_text(content)?;
    if is_systemish_user_text(&text) {
        return None;
    }
    // session id 由调用方从文件 meta 注入；此处占位，外层替换
    Some((String::new(), msg_id, text, outer.timestamp))
}

fn session_meta_cwd_and_id(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(file) = fs::File::open(path) else {
        return (None, None);
    };
    let reader = BufReader::new(file);
    let mut cwd = None;
    let mut session_id = None;
    for line in reader.lines().take(30) {
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
                    .map(str::to_string);
            }
        }
        if cwd.is_some() && session_id.is_some() {
            break;
        }
    }
    (cwd, session_id)
}

fn collect_jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > 50_000 || out.len() >= MAX_FILES {
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

fn scan_state_key(path: &Path) -> String {
    format!("{SCAN_PREFIX}{}", path.to_string_lossy())
}

/// 扫描 Codex sessions，返回新插入条数。
pub async fn scan(state: &AppState) -> Result<usize, AppError> {
    let Some(home) = codex_home_dir() else {
        tracing::debug!("CODEX_HOME/home 不可用，跳过 Codex Prompt 采集");
        return Ok(0);
    };
    let sessions = home.join("sessions");
    if !sessions.is_dir() {
        tracing::debug!("Codex sessions 目录不存在: {:?}", sessions);
        return Ok(0);
    }

    let device_id: String = state.device_id.as_ref().clone();
    let scan_device_id = device_id.clone();
    let scan_states = state.cc_history_repo.get_scan_states().await?;

    let (rows, changed_files) = tokio::task::spawn_blocking(move || {
        let mut rows: Vec<ClaudeHistoryRow> = Vec::new();
        let mut changed_files: Vec<(PathBuf, i64, i64)> = Vec::new();
        let now = Utc::now().to_rfc3339();
        let mut project_cache: HashMap<String, String> = HashMap::new();

        for path in collect_jsonl_files(&sessions) {
            let Ok(md) = fs::metadata(&path) else {
                continue;
            };
            if md.len() > MAX_FILE_BYTES {
                continue;
            }
            let size = md.len() as i64;
            let mtime_sec = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let key = scan_state_key(&path);
            if let Some((prev_mtime, prev_size)) = scan_states.get(&key) {
                if *prev_mtime == mtime_sec && *prev_size == size {
                    continue;
                }
            }

            let (cwd_opt, session_from_meta) = session_meta_cwd_and_id(&path);
            let fallback_session = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let session_id = session_from_meta.unwrap_or(fallback_session);
            let cwd = cwd_opt.unwrap_or_else(|| "/".to_string());
            let project_path = project_cache
                .entry(cwd.clone())
                .or_insert_with(|| canonical_project_path(&cwd))
                .clone();
            let project_name = ClaudeHistoryRow::derive_project_name(&project_path);

            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            let reader = BufReader::new(file);
            for line_res in reader.lines() {
                let Ok(line) = line_res else { continue };
                let Some((_sid, msg_id, text, ts)) = parse_user_message(&line) else {
                    continue;
                };
                let mut vc = HashMap::new();
                vc.insert(scan_device_id.clone(), 1u64);
                let occurred = ts.unwrap_or_else(|| now.clone());
                rows.push(ClaudeHistoryRow {
                    id: format!("codex:{session_id}:{msg_id}"),
                    project_path: project_path.clone(),
                    project_name: project_name.clone(),
                    session_id: session_id.clone(),
                    content: text,
                    git_branch: None,
                    cc_version: None,
                    occurred_at: occurred,
                    device_id: scan_device_id.clone(),
                    vector_clock: vc,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    deleted: false,
                    source: SOURCE_CODEX.to_string(),
                });
            }
            changed_files.push((path, mtime_sec, size));
        }
        (rows, changed_files)
    })
    .await
    .map_err(|e| AppError::generic(format!("Codex 采集 join 失败: {e}")))?;

    let inserted = state.cc_history_repo.bulk_ingest(&rows).await?;
    let scanned_at = Utc::now().to_rfc3339();
    for (path, mtime_sec, size) in &changed_files {
        let key = scan_state_key(path);
        if let Err(e) = state
            .cc_history_repo
            .update_scan_state(&key, *mtime_sec, *size, &scanned_at)
            .await
        {
            tracing::warn!("更新 Codex scan_state 失败 {key}: {e}");
        }
    }
    if inserted > 0 || !changed_files.is_empty() {
        tracing::info!(
            "Codex Prompt 扫描：候选 {} 条，新入库 {}，文件 {}",
            rows.len(),
            inserted,
            changed_files.len()
        );
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_skill_and_plugins() {
        assert!(is_systemish_user_text("<skill>\nfoo"));
        assert!(is_systemish_user_text("  <recommended_plugins>x"));
        assert!(!is_systemish_user_text("帮我修一下编译错误"));
    }

    #[test]
    fn parses_user_input_text() {
        let line = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"response_item","payload":{"type":"message","id":"msg_1","role":"user","content":[{"type":"input_text","text":"hello world"}]}}"#;
        let parsed = parse_user_message(line).unwrap();
        assert_eq!(parsed.1, "msg_1");
        assert_eq!(parsed.2, "hello world");
    }

    #[test]
    fn skips_assistant() {
        let line = r#"{"type":"response_item","payload":{"type":"message","id":"m","role":"assistant","content":[{"type":"input_text","text":"x"}]}}"#;
        assert!(parse_user_message(line).is_none());
    }
}
