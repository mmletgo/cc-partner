//! workbench/auto_title_opencode.rs — OpenCode `session.title` → Workbench window 自动标题
//!
//! Business Logic（为什么需要这个模块）:
//!     OpenCode 在本地 SQLite `session` 表存 title/directory；runtime 可绑定 native_session_id。
//!
//! Code Logic（这个模块做什么）:
//!     轮询只读打开 opencode.db，读近期 session 行；按 native_session_id 或 directory≈cwd 绑定后 rename。

use crate::state::AppState;
use crate::workbench::auto_title::{is_substantive_auto_title, try_auto_rename_by_native_session};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::Row;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 每轮最多处理的 session 行数（按 time_updated 倒序）。
const MAX_ROWS_PER_TICK: i64 = 40;

/// Business Logic（为什么需要这个函数）:
///     OpenCode 数据目录因安装方式不同而异。
///
/// Code Logic（这个函数做什么）:
///     依次探测常见路径，返回首个存在的 `opencode.db`。
pub fn resolve_opencode_db_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(raw) = std::env::var("OPENCODE_DATA_DIR") {
        let p = PathBuf::from(raw.trim());
        if !p.as_os_str().is_empty() {
            candidates.push(p.join("opencode.db"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/share/opencode/opencode.db"));
        candidates.push(home.join(".config/opencode/opencode.db"));
        candidates.push(home.join(".opencode/opencode.db"));
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            candidates.push(PathBuf::from(xdg).join("opencode/opencode.db"));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeSessionTitleRow {
    pub id: String,
    pub title: String,
    pub directory: Option<String>,
    pub time_updated: i64,
}

/// Business Logic（为什么需要这个函数）:
///     角色/分支占位标题（如 `general-branch`）噪声大，自动改名价值低。
///
/// Code Logic（这个函数做什么）:
///     匹配 `*-branch` 或过短无空格英文 token 时返回 false。
pub fn is_useful_opencode_title(title: &str) -> bool {
    if !is_substantive_auto_title(title) {
        return false;
    }
    let t = title.trim();
    if t.is_empty() {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower.ends_with("-branch") {
        return false;
    }
    // 单段 slug（无空格/中文）且很短时通常是系统占位
    if !t.contains(char::is_whitespace)
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && t.chars().count() <= 16
    {
        return false;
    }
    true
}

/// 从 DB 读取最近更新的 session 标题行。
pub async fn fetch_recent_session_titles(
    db_path: &Path,
    limit: i64,
) -> Result<Vec<OpenCodeSessionTitleRow>, sqlx::Error> {
    let url = format!("sqlite:{}?mode=ro", db_path.display());
    let options = SqliteConnectOptions::from_str(&url)?.read_only(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query(
        "SELECT id, title, directory, time_updated FROM session \
         WHERE title IS NOT NULL AND TRIM(title) != '' \
         ORDER BY time_updated DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        let title: String = row.try_get("title")?;
        let directory: Option<String> = row.try_get("directory")?;
        let time_updated: i64 = row.try_get("time_updated")?;
        out.push(OpenCodeSessionTitleRow {
            id,
            title,
            directory,
            time_updated,
        });
    }
    pool.close().await;
    Ok(out)
}

/// OpenCode 自动标题轮询主循环。
///
/// Business Logic（为什么需要这个函数）:
///     在 owner 进程跟踪 OpenCode session.title 变化并同步 window 名。
///
/// Code Logic（这个函数做什么）:
///     每 3s 只读查询最近 session；过滤无用标题；按 id/directory 绑定 rename。
pub async fn run_opencode_title_poller(state: AppState, cancel: CancellationToken) {
    let mut last_seen: HashMap<String, String> = HashMap::new();
    let mut missing_db_logged = false;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
        if cancel.is_cancelled() {
            break;
        }

        let Some(db_path) = resolve_opencode_db_path() else {
            if !missing_db_logged {
                tracing::debug!("未找到 opencode.db，跳过 OpenCode 自动标题");
                missing_db_logged = true;
            }
            continue;
        };
        missing_db_logged = false;

        let rows = match fetch_recent_session_titles(&db_path, MAX_ROWS_PER_TICK).await {
            Ok(r) => r,
            Err(err) => {
                tracing::debug!(path = %db_path.display(), "读取 OpenCode session 失败: {err}");
                continue;
            }
        };

        for row in rows {
            if !is_useful_opencode_title(&row.title) {
                continue;
            }
            let id = row.id.trim().to_string();
            if id.is_empty() {
                continue;
            }
            if last_seen.get(&id).map(String::as_str) == Some(row.title.as_str()) {
                continue;
            }
            // 首次见到已有 title 也尝试一次（可能终端刚创建）；之后仅变更触发
            let first_seen = !last_seen.contains_key(&id);
            last_seen.insert(id.clone(), row.title.clone());
            if first_seen {
                // 启动时不批量刷历史：仅 time_updated 很新（5 分钟内）才应用
                let now_ms = chrono::Utc::now().timestamp_millis();
                if now_ms.saturating_sub(row.time_updated) > 5 * 60 * 1000 {
                    continue;
                }
            }

            let cwd = row
                .directory
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let state_clone = state.clone();
            let title = row.title.clone();
            let native = id;
            let _ = tauri::async_runtime::spawn_blocking(move || {
                try_auto_rename_by_native_session(
                    &state_clone,
                    &native,
                    &title,
                    cwd.as_deref(),
                    "opencode.session.title",
                )
            })
            .await;
        }
    }
    tracing::debug!("OpenCode 自动标题轮询已停止");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_branch_placeholder_titles() {
        assert!(!is_useful_opencode_title("general-branch"));
        assert!(!is_useful_opencode_title("mavis-branch"));
        assert!(!is_useful_opencode_title("quick-otter"));
        assert!(is_useful_opencode_title("修复移动端终端滚动"));
        assert!(is_useful_opencode_title("Deploy grilling skill"));
    }

    #[tokio::test]
    async fn fetch_from_temp_sqlite_if_schema_matches() {
        // 最小 schema 验证 SQL 形状；无真实 OpenCode 安装时用内存库。
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                title TEXT,
                directory TEXT,
                time_updated INTEGER
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO session (id, title, directory, time_updated) VALUES
             ('ses_1', '有用标题', '/tmp/p', 100),
             ('ses_2', 'general-branch', '/tmp/q', 200)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // 直接用同一 SQL 验证查询（fetch_recent 依赖路径文件）
        let rows = sqlx::query(
            "SELECT id, title, directory, time_updated FROM session \
             WHERE title IS NOT NULL AND TRIM(title) != '' \
             ORDER BY time_updated DESC LIMIT ?",
        )
        .bind(10i64)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].try_get::<String, _>("id").unwrap(), "ses_2");
    }
}
