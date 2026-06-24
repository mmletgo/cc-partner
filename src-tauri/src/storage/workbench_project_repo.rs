//! storage/workbench_project_repo.rs — 工作台项目记录仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     用户添加过的本机项目需要在重启后保留，用于工作台最近项目列表。
//!
//! Code Logic（这个模块做什么）:
//!     封装 workbench_projects 表 CRUD；使用运行期 sqlx::query，不依赖编译期 DATABASE_URL。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::models::WorkbenchProjectRow;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;

/// 工作台项目仓库，封装所有 workbench_projects 表操作。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台命令层需要复用同一套项目持久化逻辑，避免直接散落 SQL。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool，并提供 list/get/upsert/delete 四类 CRUD 方法。
#[derive(Clone)]
pub struct WorkbenchProjectRepo {
    pool: SqlitePool,
}

impl WorkbenchProjectRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造项目仓库，供命令层共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SqlitePool clone；pool 内部是 Arc，clone 廉价。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     工作台最近项目列表需要按最近打开顺序展示项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询全部项目，按 last_opened_at DESC 排序，转换为 Row。
    pub async fn list(&self) -> Result<Vec<WorkbenchProjectRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects ORDER BY last_opened_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_project).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     会话和文件系统命令需要用 project_id 找到项目根路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询单条记录，不存在返回 None。
    pub async fn get(&self, id: &str) -> Result<Option<WorkbenchProjectRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_project(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户添加项目或重新打开项目时，需要保存/覆盖项目记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 INSERT OR REPLACE 写入完整 row。
    pub async fn upsert(&self, row: &WorkbenchProjectRow) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR REPLACE INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.name)
        .bind(&row.kind)
        .bind(&row.device_id)
        .bind(&row.device_name)
        .bind(&row.path)
        .bind(&row.last_opened_at)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可以从工作台最近项目列表移除项目；移除不删除磁盘文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 删除项目记录。
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        sqlx::query("DELETE FROM workbench_projects WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     sqlx Row 字段读取逻辑在 list/get 中复用，避免字段顺序出错。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取列并构造 WorkbenchProjectRow。
fn row_to_project(row: &SqliteRow) -> Result<WorkbenchProjectRow, AppError> {
    Ok(WorkbenchProjectRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        device_id: row.try_get("device_id")?,
        device_name: row.try_get("device_name")?,
        path: row.try_get("path")?,
        last_opened_at: row.try_get("last_opened_at")?,
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
    ///     创建内存 SQLite、初始化 workbench_projects 表并返回 repo。
    async fn setup_repo() -> WorkbenchProjectRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        WorkbenchProjectRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个测试都需要构造项目记录，统一 helper 可减少样板并突出断言差异。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 id 和 last_opened_at 生成完整 WorkbenchProjectRow。
    fn row(id: &str, last_opened_at: &str) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: id.to_string(),
            name: format!("Project {id}"),
            kind: "local".to_string(),
            device_id: "device-1".to_string(),
            device_name: "MacBook".to_string(),
            path: format!("/tmp/{id}"),
            last_opened_at: last_opened_at.to_string(),
            created_at: "2026-06-24T00:00:00Z".to_string(),
            updated_at: "2026-06-24T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     最近项目列表必须让用户先看到最后打开的项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入两条不同 last_opened_at 的记录，并断言 list 返回倒序。
    #[tokio::test]
    async fn list_orders_by_last_opened_desc() {
        let repo = setup_repo().await;
        repo.upsert(&row("p1", "2026-06-24T01:00:00Z"))
            .await
            .unwrap();
        repo.upsert(&row("p2", "2026-06-24T02:00:00Z"))
            .await
            .unwrap();

        let listed = repo.list().await.unwrap();

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "p2");
        assert_eq!(listed[1].id, "p1");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户移除项目记录时不应再在最近项目列表中出现。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入后按 id delete，再断言 get 返回 None。
    #[tokio::test]
    async fn delete_removes_project_record_only() {
        let repo = setup_repo().await;
        repo.upsert(&row("p1", "2026-06-24T01:00:00Z"))
            .await
            .unwrap();

        repo.delete("p1").await.unwrap();

        assert!(repo.get("p1").await.unwrap().is_none());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层需要区分存在项目和不存在项目，便于给前端明确错误或空状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入一条记录，分别查询存在 id 和缺失 id。
    #[tokio::test]
    async fn get_returns_existing_project_and_none_for_missing() {
        let repo = setup_repo().await;
        repo.upsert(&row("p1", "2026-06-24T01:00:00Z"))
            .await
            .unwrap();

        let existing = repo.get("p1").await.unwrap();
        let missing = repo.get("missing").await.unwrap();

        assert_eq!(existing.unwrap().id, "p1");
        assert!(missing.is_none());
    }
}
