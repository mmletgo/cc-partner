//! storage/workbench_session_repo.rs — 工作台终端会话元数据仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台终端 tab 需要在应用重启后恢复；PTY 句柄是运行期资源，但会话名称、项目、尺寸和可重连后端
//!     必须持久化到 SQLite。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_sessions` 表 CRUD；使用运行期 sqlx::query，不依赖编译期 DATABASE_URL。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::models::WorkbenchSessionRow;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;

/// 工作台终端会话仓库，封装所有 workbench_sessions 表操作。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层和会话恢复流程需要复用同一套会话元数据持久化逻辑。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool，并提供 list/get/upsert/delete/delete_by_project 五类方法。
#[derive(Clone)]
pub struct WorkbenchSessionRepo {
    pool: SqlitePool,
}

impl WorkbenchSessionRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造会话仓库，供命令层共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SqlitePool clone；pool 内部是 Arc，clone 廉价。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     数据库相关修改必须兼容旧库；旧版本 workbench_sessions 没有 tmux window 标识列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 PRAGMA table_info 检查列名，缺失时 ALTER TABLE ADD COLUMN backend_window_id TEXT。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let columns = sqlx::query("PRAGMA table_info(workbench_sessions)")
            .fetch_all(&self.pool)
            .await?;
        let has_backend_window_id = columns
            .iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .any(|name| name == "backend_window_id");
        if !has_backend_window_id {
            sqlx::query("ALTER TABLE workbench_sessions ADD COLUMN backend_window_id TEXT")
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 页面进入项目时，需要列出该项目所有未关闭的历史终端 tab。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询未删除会话；project_id 为空时返回全部，排序按 started_at 保持创建顺序。
    pub async fn list(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<WorkbenchSessionRow>, AppError> {
        let rows = if let Some(project_id) = project_id {
            sqlx::query(
                "SELECT id, project_id, name, command, status, cols, rows, started_at, exited_at, \
                 exit_code, backend, backend_id, backend_window_id, created_at, updated_at \
                 FROM workbench_sessions WHERE project_id = ? ORDER BY started_at ASC",
            )
            .bind(project_id)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, project_id, name, command, status, cols, rows, started_at, exited_at, \
                 exit_code, backend, backend_id, backend_window_id, created_at, updated_at \
                 FROM workbench_sessions ORDER BY started_at ASC",
            )
            .fetch_all(&self.pool)
            .await?
        };
        rows.iter().map(row_to_session).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     close/rename/resize 等命令需要按 session_id 找到持久化会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询单条记录，不存在返回 None。
    pub async fn get(&self, id: &str) -> Result<Option<WorkbenchSessionRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, project_id, name, command, status, cols, rows, started_at, exited_at, \
             exit_code, backend, backend_id, backend_window_id, created_at, updated_at \
             FROM workbench_sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_session(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建、恢复、重命名和 resize 后都需要保存会话元数据，供下次启动恢复。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 INSERT OR REPLACE 写入完整 row。
    pub async fn upsert(&self, row: &WorkbenchSessionRow) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR REPLACE INTO workbench_sessions \
             (id, project_id, name, command, status, cols, rows, started_at, exited_at, exit_code, \
              backend, backend_id, backend_window_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.project_id)
        .bind(&row.name)
        .bind(&row.command)
        .bind(&row.status)
        .bind(i64::from(row.cols))
        .bind(i64::from(row.rows))
        .bind(&row.started_at)
        .bind(&row.exited_at)
        .bind(row.exit_code)
        .bind(&row.backend)
        .bind(&row.backend_id)
        .bind(&row.backend_window_id)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭终端 tab 后，该 tab 不应在应用重启后再次出现。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 删除会话记录。
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM workbench_sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户移除工作台项目时，该项目关联的历史终端 tab 也应一起移除，避免孤儿会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 删除全部会话记录。
    pub async fn delete_by_project(&self, project_id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM workbench_sessions WHERE project_id = ?")
            .bind(project_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     sqlx Row 字段读取逻辑在 list/get 中复用，避免字段顺序出错。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取列并构造 WorkbenchSessionRow，同时把 SQLite INTEGER 行列数裁剪为 u16。
fn row_to_session(row: &SqliteRow) -> Result<WorkbenchSessionRow, AppError> {
    let cols: i64 = row.try_get("cols")?;
    let rows: i64 = row.try_get("rows")?;
    Ok(WorkbenchSessionRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        name: row.try_get("name")?,
        command: row.try_get("command")?,
        status: row.try_get("status")?,
        cols: cols.clamp(i64::from(u16::MIN), i64::from(u16::MAX)) as u16,
        rows: rows.clamp(i64::from(u16::MIN), i64::from(u16::MAX)) as u16,
        started_at: row.try_get("started_at")?,
        exited_at: row.try_get("exited_at")?,
        exit_code: row.try_get("exit_code")?,
        backend: row.try_get("backend")?,
        backend_id: row.try_get("backend_id")?,
        backend_window_id: row.try_get("backend_window_id")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     仓库测试需要隔离的临时数据库，避免污染用户真实数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建内存 SQLite、初始化 workbench_sessions 表并返回 repo。
    async fn setup_repo() -> WorkbenchSessionRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_sessions (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, command TEXT NOT NULL, \
             status TEXT NOT NULL, cols INTEGER NOT NULL, rows INTEGER NOT NULL, started_at TEXT NOT NULL, \
             exited_at TEXT, exit_code INTEGER, backend TEXT NOT NULL, backend_id TEXT, \
             backend_window_id TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        WorkbenchSessionRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个测试都需要构造会话记录，统一 helper 可减少样板并突出断言差异。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 id/project_id/started_at 生成完整 WorkbenchSessionRow。
    fn row(id: &str, project_id: &str, started_at: &str) -> WorkbenchSessionRow {
        WorkbenchSessionRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
            name: format!("Terminal {id}"),
            command: "/bin/zsh".to_string(),
            status: "running".to_string(),
            cols: 120,
            rows: 34,
            started_at: started_at.to_string(),
            exited_at: None,
            exit_code: None,
            backend: "tmux".to_string(),
            backend_id: Some(format!("cc-partner-{id}")),
            backend_window_id: Some(format!("@{id}")),
            created_at: started_at.to_string(),
            updated_at: started_at.to_string(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     应用重启后需要从 SQLite 恢复之前打开的终端 tab。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入会话记录后用同一 pool 构造第二个 repo，断言仍可按项目读出该记录。
    #[tokio::test]
    async fn sessions_persist_across_repo_instances() {
        let repo = setup_repo().await;
        repo.upsert(&row("s1", "p1", "2026-06-24T00:00:00Z"))
            .await
            .unwrap();
        let reopened = WorkbenchSessionRepo::new(repo.pool.clone());

        let listed = reopened.list(Some("p1")).await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s1");
        assert_eq!(listed[0].project_id, "p1");
        assert_eq!(listed[0].backend, "tmux");
        assert_eq!(listed[0].backend_window_id.as_deref(), Some("@s1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 分项目展示时不能把其他项目的终端 tab 混入当前项目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入两个项目的会话，断言 list(Some(project_id)) 只返回匹配项目。
    #[tokio::test]
    async fn list_filters_by_project_id() {
        let repo = setup_repo().await;
        repo.upsert(&row("s1", "p1", "2026-06-24T00:00:00Z"))
            .await
            .unwrap();
        repo.upsert(&row("s2", "p2", "2026-06-24T00:01:00Z"))
            .await
            .unwrap();

        let listed = repo.list(Some("p2")).await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s2");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户关闭终端 tab 后，该 tab 不应在重启后恢复。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入后删除会话，断言 get 返回 None。
    #[tokio::test]
    async fn delete_removes_session_record() {
        let repo = setup_repo().await;
        repo.upsert(&row("s1", "p1", "2026-06-24T00:00:00Z"))
            .await
            .unwrap();

        repo.delete("s1").await.unwrap();

        assert!(repo.get("s1").await.unwrap().is_none());
    }
}
