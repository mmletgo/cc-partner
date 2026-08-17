//! storage/workbench_banner_repo.rs — Workbench 设备级标语仓库。
//!
//! Business Logic（为什么需要这个模块）:
//!     顶栏标语是 owning device 的全局 chrome，不是 per-project；必须落 SQLite 单行，
//!     不能再依赖控制端 localStorage。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_device_banner` 单行表；get 缺行返回 None；upsert 覆盖 markdown；
//!     上限按 UTF-16 计 280；写路径走 shared lease。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// 标语 UTF-16 长度上限（与前端 BANNER_MAX_CHARS 对齐）。
pub const WORKBENCH_BANNER_MAX_UTF16: usize = 280;

/// 设备标语表建表 SQL（幂等 CREATE TABLE IF NOT EXISTS）。
pub const WORKBENCH_DEVICE_BANNER_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS workbench_device_banner (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    markdown TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// 设备标语行。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层需要把本机标语正文与更新时间交给前端。
///
/// Code Logic（这个结构体做什么）:
///     承载 markdown / updated_at。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbenchBannerRow {
    /// 轻量 Markdown 正文。
    pub markdown: String,
    /// 最近保存时间（RFC3339）。
    pub updated_at: String,
}

/// Workbench 设备标语仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     get/save 需要统一入口，并与 restore exclusive 写屏障对齐。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + maintenance gate；写路径经 shared lease。
#[derive(Clone)]
pub struct WorkbenchBannerRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchBannerRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧库升级必须幂等补表，禁止 sqlx::migrate!。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 CREATE TABLE IF NOT EXISTS。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        sqlx::query(WORKBENCH_DEVICE_BANNER_SCHEMA)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     打开工作台时读取本机标语；无行表示空标语，前端可一次性灌入 legacy localStorage。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询 singleton=1；缺行返回 None。
    pub async fn get(&self) -> Result<Option<WorkbenchBannerRow>, AppError> {
        let row = sqlx::query(
            "SELECT markdown, updated_at FROM workbench_device_banner WHERE singleton = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_banner(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户编辑后需要覆盖本机唯一标语行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 UTF-16 上限后 upsert singleton=1，返回写入后的行。
    pub async fn upsert(&self, markdown: &str) -> Result<WorkbenchBannerRow, AppError> {
        if markdown.encode_utf16().count() > WORKBENCH_BANNER_MAX_UTF16 {
            return Err(AppError::validation(
                "workbench_banner_too_long".to_string(),
            ));
        }
        let now = chrono::Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO workbench_device_banner (singleton, markdown, updated_at)
                 VALUES (1, ?, ?)
                 ON CONFLICT(singleton) DO UPDATE SET
                   markdown = excluded.markdown,
                   updated_at = excluded.updated_at",
            )
            .bind(markdown)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(WorkbenchBannerRow {
                markdown: markdown.to_string(),
                updated_at: now,
            })
        })
        .await
    }
}

/// Business Logic（为什么需要这个函数）:
///     get 需要把 SQLite 行映射为结构体。
fn row_to_banner(row: &SqliteRow) -> Result<WorkbenchBannerRow, AppError> {
    Ok(WorkbenchBannerRow {
        markdown: row.try_get("markdown")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn memory_repo() -> WorkbenchBannerRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("memory sqlite")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("pool");
        let repo = WorkbenchBannerRepo::new(pool);
        repo.ensure_schema().await.expect("schema");
        repo
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空库必须返回 None，不能伪装成已保存空串。
    #[tokio::test]
    async fn get_missing_row_is_none() {
        let repo = memory_repo().await;
        assert_eq!(repo.get().await.expect("get"), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     覆盖保存后必须读回同一正文；超长 UTF-16 必须拒绝。
    #[tokio::test]
    async fn upsert_then_get_and_reject_overlong() {
        let repo = memory_repo().await;
        let saved = repo.upsert("**focus**").await.expect("upsert");
        assert_eq!(saved.markdown, "**focus**");
        assert_eq!(
            repo.get().await.expect("get").map(|row| row.markdown),
            Some("**focus**".to_string())
        );

        let overlong: String = "a".repeat(WORKBENCH_BANNER_MAX_UTF16 + 1);
        let err = repo.upsert(&overlong).await.expect_err("too long");
        assert_eq!(err.code(), "workbench_banner_too_long");
    }
}
