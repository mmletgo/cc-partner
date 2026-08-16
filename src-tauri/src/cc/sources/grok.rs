//! Grok Build session → Prompt 历史采集
//!
//! Business Logic: 从 `~/.grok/sessions/**/chat_history.jsonl` 提取用户直接输入。
//! Code Logic: 按文件 mtime 增量；id=`grok:{session}:{line}`。

use crate::cc::models::{ClaudeHistoryRow, SOURCE_GROK};
use crate::cc::project_identity::canonical_project_path;
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

const SCAN_PREFIX: &str = "grok-user-v1:";
const MAX_FILES: usize = 5_000;

fn scan_state_key(path: &Path) -> String {
    format!("{SCAN_PREFIX}{}", path.to_string_lossy())
}

fn grok_sessions_root() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GROK_HOME") {
        if !home.trim().is_empty() {
            return Some(PathBuf::from(home).join("sessions"));
        }
    }
    dirs::home_dir().map(|h| h.join(".grok").join("sessions"))
}

fn parse_user_line(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    let role = v
        .get("role")
        .or_else(|| v.pointer("/message/role"))
        .and_then(|r| r.as_str())
        .unwrap_or("");
    if role != "user" && role != "human" {
        return None;
    }
    let text = v
        .get("content")
        .and_then(|c| c.as_str())
        .or_else(|| v.pointer("/message/content").and_then(|c| c.as_str()))
        .or_else(|| v.get("text").and_then(|c| c.as_str()))
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() || text.starts_with('/') {
        None
    } else {
        Some(text)
    }
}

/// 扫描 Grok sessions，返回新插入条数。
pub async fn scan(state: &AppState) -> Result<usize, AppError> {
    let Some(root) = grok_sessions_root() else {
        return Ok(0);
    };
    if !root.is_dir() {
        return Ok(0);
    }
    let device_id = state.device_id.as_ref().clone();
    let scan_device_id = device_id.clone();
    let existing = state.cc_history_repo.get_scan_states().await?;
    let (rows, changed_files) = tokio::task::spawn_blocking(move || {
        let now = Utc::now().to_rfc3339();
        let mut rows = Vec::new();
        let mut changed_files = Vec::new();
        let mut considered = 0usize;
        let Ok(groups) = fs::read_dir(&root) else {
            return (rows, changed_files);
        };
        for group in groups.flatten() {
            let group_path = group.path();
            if !group_path.is_dir() {
                continue;
            }
            let Ok(sessions) = fs::read_dir(&group_path) else {
                continue;
            };
            for entry in sessions.flatten() {
                if considered >= MAX_FILES {
                    break;
                }
                let history = entry.path().join("chat_history.jsonl");
                if !history.is_file() {
                    continue;
                }
                considered += 1;
                let meta = match fs::metadata(&history) {
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
                let key = scan_state_key(&history);
                if let Some((prev_mtime, prev_size)) = existing.get(&key) {
                    if *prev_mtime == mtime_sec && *prev_size == size {
                        continue;
                    }
                }
                let session_id = entry
                    .file_name()
                    .to_string_lossy()
                    .into_owned();
                let cwd = fs::read_to_string(entry.path().join("summary.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| {
                        v.pointer("/info/cwd")
                            .and_then(|c| c.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "/".into());
                let project_path = canonical_project_path(&cwd);
                let project_name = ClaudeHistoryRow::derive_project_name(&project_path);
                let Ok(file) = File::open(&history) else {
                    continue;
                };
                for (idx, line_res) in BufReader::new(file).lines().enumerate() {
                    let Ok(line) = line_res else { continue };
                    let Some(text) = parse_user_line(&line) else {
                        continue;
                    };
                    let mut vc = HashMap::new();
                    vc.insert(scan_device_id.clone(), 1u64);
                    rows.push(ClaudeHistoryRow {
                        id: format!("grok:{session_id}:{idx}"),
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
                        source: SOURCE_GROK.to_string(),
                    });
                }
                changed_files.push((history, mtime_sec, size));
            }
        }
        (rows, changed_files)
    })
    .await
    .map_err(|e| AppError::generic(format!("Grok 采集 join 失败: {e}")))?;

    let inserted = state.cc_history_repo.bulk_ingest(&rows).await?;
    let scanned_at = Utc::now().to_rfc3339();
    for (path, mtime_sec, size) in &changed_files {
        let key = scan_state_key(path);
        if let Err(e) = state
            .cc_history_repo
            .update_scan_state(&key, *mtime_sec, *size, &scanned_at)
            .await
        {
            tracing::warn!("更新 Grok scan_state 失败 {key}: {e}");
        }
    }
    Ok(inserted)
}
