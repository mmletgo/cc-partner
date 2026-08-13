//! OpenCode SQLite → Prompt 历史采集
//!
//! Business Logic: 从 opencode.db 的 message/part 提取 user 文本。
//! Code Logic: 只读打开；db mtime/size 增量；id=`opencode:{session}:{message}`。

use crate::cc::models::{ClaudeHistoryRow, SOURCE_OPENCODE};
use crate::cc::project_identity::canonical_project_path;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::auto_title_opencode::resolve_opencode_db_path;
use chrono::{TimeZone, Utc};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

const SCAN_PREFIX: &str = "opencode-user-v1:";

fn scan_state_key(path: &Path) -> String {
    format!("{SCAN_PREFIX}{}", path.to_string_lossy())
}

fn ms_to_rfc3339(ms: i64) -> String {
    // OpenCode time_* 为毫秒
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    Utc.timestamp_opt(secs, nsecs)
        .single()
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339())
}

/// 从 message.data + 拼接的 text parts 构造用户 prompt。
fn user_text_from_parts(parts_json: &[String]) -> Option<String> {
    let mut texts = Vec::new();
    for raw in parts_json {
        let Ok(v) = serde_json::from_str::<Value>(raw) else {
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
        None
    } else {
        Some(texts.join("\n"))
    }
}

fn message_role(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    v.get("role")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
}

fn message_cwd(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    v.get("path")
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
        .or_else(|| {
            v.get("path")
                .and_then(|p| p.get("root"))
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
}

/// 扫描 OpenCode DB，返回新插入条数。
pub async fn scan(state: &AppState) -> Result<usize, AppError> {
    let Some(db_path) = resolve_opencode_db_path() else {
        tracing::debug!("未找到 opencode.db，跳过 OpenCode Prompt 采集");
        return Ok(0);
    };

    let md = match std::fs::metadata(&db_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!(path = %db_path.display(), "读取 opencode.db metadata 失败: {e}");
            return Ok(0);
        }
    };
    let size = md.len() as i64;
    let mtime_sec = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let key = scan_state_key(&db_path);
    if let Some((prev_mtime, prev_size)) = state
        .cc_history_repo
        .get_scan_states()
        .await?
        .get(&key)
        .cloned()
    {
        if prev_mtime == mtime_sec && prev_size == size {
            return Ok(0);
        }
    }

    let device_id: String = state.device_id.as_ref().clone();
    let path_for_blocking = db_path.clone();
    let rows = tokio::task::spawn_blocking(move || {
        // async runtime 外：用 block_on 读只读 sqlite
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("opencode runtime: {e}"))?;
        rt.block_on(async move {
            let url = format!("sqlite:{}?mode=ro", path_for_blocking.display());
            let options = SqliteConnectOptions::from_str(&url)
                .map_err(|e| format!("opencode connect options: {e}"))?
                .read_only(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .map_err(|e| format!("opencode connect: {e}"))?;

            // message + session directory；parts 按 message 聚合
            let messages = sqlx::query(
                "SELECT m.id AS message_id, m.session_id AS session_id, m.data AS data, \
                        m.time_created AS time_created, s.directory AS directory \
                 FROM message m \
                 LEFT JOIN session s ON s.id = m.session_id \
                 ORDER BY m.time_created DESC \
                 LIMIT 5000",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("opencode message query: {e}"))?;

            let now = Utc::now().to_rfc3339();
            let mut out: Vec<ClaudeHistoryRow> = Vec::new();
            let mut project_cache: HashMap<String, String> = HashMap::new();

            for row in messages {
                let message_id: String = row.try_get("message_id").map_err(|e| e.to_string())?;
                let session_id: String = row.try_get("session_id").map_err(|e| e.to_string())?;
                let data: String = row.try_get("data").map_err(|e| e.to_string())?;
                let time_created: i64 = row.try_get("time_created").unwrap_or(0);
                let directory: Option<String> = row.try_get("directory").ok();

                let Some(role) = message_role(&data) else {
                    continue;
                };
                if role != "user" {
                    continue;
                }

                let parts = sqlx::query(
                    "SELECT data FROM part WHERE message_id = ? ORDER BY time_created ASC",
                )
                .bind(&message_id)
                .fetch_all(&pool)
                .await
                .map_err(|e| format!("opencode part query: {e}"))?;
                let part_jsons: Vec<String> = parts
                    .iter()
                    .filter_map(|p| p.try_get::<String, _>("data").ok())
                    .collect();
                let Some(content) = user_text_from_parts(&part_jsons) else {
                    continue;
                };

                let cwd = message_cwd(&data)
                    .or(directory)
                    .unwrap_or_else(|| "/".to_string());
                let project_path = project_cache
                    .entry(cwd.clone())
                    .or_insert_with(|| canonical_project_path(&cwd))
                    .clone();
                let project_name = ClaudeHistoryRow::derive_project_name(&project_path);
                let mut vc = HashMap::new();
                vc.insert(device_id.clone(), 1u64);
                out.push(ClaudeHistoryRow {
                    id: format!("opencode:{session_id}:{message_id}"),
                    project_path,
                    project_name,
                    session_id,
                    content,
                    git_branch: None,
                    cc_version: None,
                    occurred_at: ms_to_rfc3339(time_created),
                    device_id: device_id.clone(),
                    vector_clock: vc,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    deleted: false,
                    source: SOURCE_OPENCODE.to_string(),
                });
            }
            pool.close().await;
            Ok::<_, String>(out)
        })
    })
    .await
    .map_err(|e| AppError::generic(format!("OpenCode 采集 join 失败: {e}")))?
    .map_err(AppError::generic)?;

    let inserted = state.cc_history_repo.bulk_ingest(&rows).await?;
    let scanned_at = Utc::now().to_rfc3339();
    if let Err(e) = state
        .cc_history_repo
        .update_scan_state(&key, mtime_sec, size, &scanned_at)
        .await
    {
        tracing::warn!("更新 OpenCode scan_state 失败 {key}: {e}");
    }
    if inserted > 0 {
        tracing::info!(
            "OpenCode Prompt 扫描：候选 {} 条，新入库 {}",
            rows.len(),
            inserted
        );
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_joins_text_parts() {
        let parts = vec![
            r#"{"type":"text","text":"hello"}"#.to_string(),
            r#"{"type":"tool","tool":"x"}"#.to_string(),
            r#"{"type":"text","text":"world"}"#.to_string(),
        ];
        assert_eq!(
            user_text_from_parts(&parts).as_deref(),
            Some("hello\nworld")
        );
    }

    #[test]
    fn role_from_message_data() {
        assert_eq!(
            message_role(r#"{"role":"user","path":{"cwd":"/tmp"}}"#).as_deref(),
            Some("user")
        );
        assert_eq!(
            message_role(r#"{"role":"assistant"}"#).as_deref(),
            Some("assistant")
        );
    }
}
