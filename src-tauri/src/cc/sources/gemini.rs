//! Gemini CLI chats → Prompt 历史采集
//!
//! Business Logic: 从 `~/.gemini/tmp/*/chats/*.json` 提取用户输入。
//! Code Logic: 按文件 mtime 增量；id=`gemini:{session}:{idx}`。

use crate::cc::models::{ClaudeHistoryRow, SOURCE_GEMINI};
use crate::cc::project_identity::canonical_project_path;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const SCAN_PREFIX: &str = "gemini-user-v1:";
const MAX_FILES: usize = 5_000;

fn scan_state_key(path: &Path) -> String {
    format!("{SCAN_PREFIX}{}", path.to_string_lossy())
}

fn extract_user_texts(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let messages = value
        .get("messages")
        .or_else(|| value.get("history"))
        .and_then(|v| v.as_array());
    if let Some(arr) = messages {
        for msg in arr {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "user" && role != "human" {
                continue;
            }
            if let Some(t) = msg.get("content").and_then(|c| c.as_str()) {
                let trimmed = t.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('/') {
                    out.push(trimmed.to_string());
                }
            }
        }
    }
    out
}

/// 扫描 Gemini chats，返回新插入条数。
pub async fn scan(state: &AppState) -> Result<usize, AppError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(0);
    };
    let tmp = home.join(".gemini").join("tmp");
    if !tmp.is_dir() {
        return Ok(0);
    }
    let scan_device_id = state.device_id.as_ref().clone();
    let existing = state.cc_history_repo.get_scan_states().await?;
    let (rows, changed_files) = tokio::task::spawn_blocking(move || {
        let now = Utc::now().to_rfc3339();
        let mut rows = Vec::new();
        let mut changed_files = Vec::new();
        let mut considered = 0usize;
        let Ok(projects) = fs::read_dir(&tmp) else {
            return (rows, changed_files);
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
                if considered >= MAX_FILES {
                    break;
                }
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                considered += 1;
                let meta = match fs::metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let mtime_sec = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let size = meta.len() as i64;
                let key = scan_state_key(&path);
                if let Some((prev_mtime, prev_size)) = existing.get(&key) {
                    if *prev_mtime == mtime_sec && *prev_size == size {
                        continue;
                    }
                }
                let Ok(text) = fs::read_to_string(&path) else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                let session_id = value
                    .get("id")
                    .or_else(|| value.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                    })
                    .to_string();
                let cwd = value
                    .get("cwd")
                    .or_else(|| value.pointer("/session/cwd"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("/");
                let project_path = canonical_project_path(cwd);
                let project_name = ClaudeHistoryRow::derive_project_name(&project_path);
                for (idx, text) in extract_user_texts(&value).into_iter().enumerate() {
                    let mut vc = HashMap::new();
                    vc.insert(scan_device_id.clone(), 1u64);
                    rows.push(ClaudeHistoryRow {
                        id: format!("gemini:{session_id}:{idx}"),
                        project_path: project_path.clone(),
                        project_name: project_name.clone(),
                        session_id: session_id.clone(),
                        content: text,
                        git_branch: None,
                        cc_version: None,
                        occurred_at: now.clone(),
                        device_id: scan_device_id.clone(),
                        vector_clock: vc,
                        created_at: now.clone(),
                        updated_at: now.clone(),
                        deleted: false,
                        source: SOURCE_GEMINI.to_string(),
                    });
                }
                changed_files.push((path, mtime_sec, size));
            }
        }
        (rows, changed_files)
    })
    .await
    .map_err(|e| AppError::generic(format!("Gemini 采集 join 失败: {e}")))?;

    let inserted = state.cc_history_repo.bulk_ingest(&rows).await?;
    let scanned_at = Utc::now().to_rfc3339();
    for (path, mtime_sec, size) in &changed_files {
        let key = scan_state_key(path);
        if let Err(e) = state
            .cc_history_repo
            .update_scan_state(&key, *mtime_sec, *size, &scanned_at)
            .await
        {
            tracing::warn!("更新 Gemini scan_state 失败 {key}: {e}");
        }
    }
    Ok(inserted)
}
