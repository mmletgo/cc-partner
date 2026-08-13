//! storage/workbench_project_note_repo.rs — Workbench 项目笔记仓库。
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台右侧「项目笔记」按项目 ID 保存在本机 SQLite，重开应用不丢；
//!     不写仓库文件、不参与 LAN/GitHub 同步或备份域。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_project_notes` 表；get 缺行返回 None；upsert 覆盖正文；
//!     delete 按 project_id 删除；写路径走 shared lease。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// 项目笔记正文上限（UTF-8 字节）。
pub const WORKBENCH_PROJECT_NOTE_MAX_BYTES: usize = 1024 * 1024;

/// 项目笔记表建表 SQL（幂等 CREATE TABLE IF NOT EXISTS）。
pub const WORKBENCH_PROJECT_NOTE_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS workbench_project_notes (
    project_id TEXT PRIMARY KEY NOT NULL,
    content TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// 项目笔记行。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层需要把本机笔记正文与更新时间交给前端。
///
/// Code Logic（这个结构体做什么）:
///     承载 project_id / content / updated_at。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchProjectNoteRow {
    /// Workbench 项目 ID（本机或远端 shortcut）。
    pub project_id: String,
    /// Markdown 正文。
    pub content: String,
    /// 最近保存时间（RFC3339）。
    pub updated_at: String,
}

/// Workbench 项目笔记仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     get/save/delete 需要统一入口，并与 restore exclusive 写屏障对齐。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + maintenance gate；写路径经 shared lease。
#[derive(Clone)]
pub struct WorkbenchProjectNoteRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchProjectNoteRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测需要隔离内存库与独立 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 与其它 ordinary writer 共用 restore exclusive 屏障。
    /// Code Logic: 保存 pool + Arc gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧库升级必须幂等补表，禁止 sqlx::migrate!。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 CREATE TABLE IF NOT EXISTS。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        sqlx::query(WORKBENCH_PROJECT_NOTE_SCHEMA)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     打开笔记 tab 时读取已保存正文；无行表示空笔记。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 查询；缺行返回 None。
    pub async fn get(&self, project_id: &str) -> Result<Option<WorkbenchProjectNoteRow>, AppError> {
        let row = sqlx::query(
            "SELECT project_id, content, updated_at FROM workbench_project_notes WHERE project_id = ?",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_note(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户编辑后需要覆盖保存同一项目的笔记。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验非空 project_id 与 1 MiB 上限后 upsert，返回写入后的行。
    pub async fn upsert(
        &self,
        project_id: &str,
        content: &str,
    ) -> Result<WorkbenchProjectNoteRow, AppError> {
        let project_id = project_id.trim();
        if project_id.is_empty() {
            return Err(AppError::validation(
                "workbench_project_note_project_id_required".to_string(),
            ));
        }
        if content.len() > WORKBENCH_PROJECT_NOTE_MAX_BYTES {
            return Err(AppError::validation(
                "workbench_project_note_too_large".to_string(),
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO workbench_project_notes (project_id, content, updated_at)
                 VALUES (?, ?, ?)
                 ON CONFLICT(project_id) DO UPDATE SET
                   content = excluded.content,
                   updated_at = excluded.updated_at",
            )
            .bind(project_id)
            .bind(content)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(WorkbenchProjectNoteRow {
                project_id: project_id.to_string(),
                content: content.to_string(),
                updated_at: now,
            })
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     删除 Workbench 项目记录时必须清掉本机笔记，避免孤儿行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `DELETE FROM workbench_project_notes WHERE project_id = ?`；缺行视为成功。
    pub async fn delete(&self, project_id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query("DELETE FROM workbench_project_notes WHERE project_id = ?")
                .bind(project_id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }
}

/// Business Logic（为什么需要这个函数）:
///     get 需要把 SQLite 行映射为结构体。
///
/// Code Logic（这个函数做什么）:
///     读取 project_id / content / updated_at。
fn row_to_note(row: &SqliteRow) -> Result<WorkbenchProjectNoteRow, AppError> {
    Ok(WorkbenchProjectNoteRow {
        project_id: row.try_get("project_id")?,
        content: row.try_get("content")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     仓储测试必须使用隔离内存库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     打开共享缓存内存 SQLite，建表后返回 repo。
    async fn test_repo() -> WorkbenchProjectNoteRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("memory sqlite")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("connect memory sqlite");
        let repo = WorkbenchProjectNoteRepo::new(pool);
        repo.ensure_schema().await.expect("ensure schema");
        repo
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let repo = test_repo().await;
        assert_eq!(repo.get("p1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_overwrites_same_project() {
        let repo = test_repo().await;
        let first = repo.upsert("p1", "# hello").await.unwrap();
        assert_eq!(first.project_id, "p1");
        assert_eq!(first.content, "# hello");
        let second = repo.upsert("p1", "# updated").await.unwrap();
        assert_eq!(second.content, "# updated");
        let loaded = repo.get("p1").await.unwrap().expect("row");
        assert_eq!(loaded.content, "# updated");
    }

    #[tokio::test]
    async fn upsert_rejects_empty_project_and_oversized_content() {
        let repo = test_repo().await;
        let empty = repo.upsert("  ", "x").await.unwrap_err();
        assert!(empty.to_string().contains("project_id_required"));
        let too_big = "a".repeat(WORKBENCH_PROJECT_NOTE_MAX_BYTES + 1);
        let oversized = repo.upsert("p1", &too_big).await.unwrap_err();
        assert!(oversized.to_string().contains("too_large"));
    }

    #[tokio::test]
    async fn delete_removes_row_and_is_idempotent() {
        let repo = test_repo().await;
        repo.upsert("p1", "keep me").await.unwrap();
        repo.delete("p1").await.unwrap();
        assert_eq!(repo.get("p1").await.unwrap(), None);
        repo.delete("p1").await.unwrap();
    }
}
