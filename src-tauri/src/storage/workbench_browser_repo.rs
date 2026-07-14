//! storage/workbench_browser_repo.rs — Workbench 浏览器预览目标仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     用户为项目或 worktree 选择过 dev server 后，下次打开浏览器预览应优先使用同一目标。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_browser_targets` 表读写；写路径经 `with_shared_write_lease`；
//!     使用运行期 sqlx::query，不使用 sqlx 宏。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::Row;
use sqlx::SqlitePool;
use std::sync::Arc;

/// Workbench browser target 仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     浏览器预览发现逻辑需要读取和保存项目/worktree 最近一次成功使用的目标 URL。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool + maintenance gate，后续方法通过运行期 sqlx query 访问 `workbench_browser_targets`。
#[derive(Clone)]
pub struct WorkbenchBrowserRepo {
    pool: SqlitePool,
    /// 维护屏障：写路径持 shared lease，restore exclusive 时阻塞。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchBrowserRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户为某个项目/worktree 选择过 dev server 后，下次打开预览应优先使用同一目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内部 `with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 全部 ordinary writer 与 restore 共用同一 gate。
    /// Code Logic: 保存 pool + Arc gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// 读取项目/worktree 最近一次预览目标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     自动发现需要把用户上次确认的 URL 放在候选第一位。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id + coalesced worktree_id 查询；worktree_id 为空时读取 project-level 目标。
    pub async fn get_target(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let row = sqlx::query(
            "SELECT target_url FROM workbench_browser_targets
             WHERE project_id = ?1 AND IFNULL(worktree_id, '') = IFNULL(?2, '')
             LIMIT 1",
        )
        .bind(project_id)
        .bind(worktree_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| row.get::<String, _>("target_url")))
    }

    /// 写入项目/worktree 最近一次预览目标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户创建预览后，后续自动发现应把该目标作为可信候选。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 shared write lease 后用 project_id + coalesced worktree_id 唯一键 upsert。
    pub async fn upsert_target(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
        target_url: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO workbench_browser_targets
                 (project_id, worktree_id, target_url, updated_at)
                 VALUES (?1, ?2, ?3, strftime('%s','now'))
                 ON CONFLICT(project_id, worktree_key)
                 DO UPDATE SET target_url = excluded.target_url, updated_at = excluded.updated_at",
            )
            .bind(project_id)
            .bind(worktree_id)
            .bind(target_url)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 创建测试仓库。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仓储测试必须使用隔离数据库，避免污染用户真实 Workbench 浏览器目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建内存 SQLite，初始化 `workbench_browser_targets` 表并返回 repo。
    async fn setup_repo() -> WorkbenchBrowserRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_browser_targets (\
             id INTEGER PRIMARY KEY AUTOINCREMENT,\
             project_id TEXT NOT NULL,\
             worktree_id TEXT,\
             worktree_key TEXT GENERATED ALWAYS AS (IFNULL(worktree_id, '')) STORED,\
             target_url TEXT NOT NULL,\
             updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),\
             UNIQUE(project_id, worktree_key))",
        )
        .execute(&pool)
        .await
        .unwrap();
        WorkbenchBrowserRepo::new(pool)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户在主项目上选择过 dev server 后，再进入同一项目应优先看到该目标。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 project-level target 后，用 None worktree_id 读取同一 URL。
    #[tokio::test]
    async fn upsert_and_get_project_level_target() {
        let repo = setup_repo().await;

        repo.upsert_target("project-1", None, "http://127.0.0.1:5173/")
            .await
            .unwrap();

        assert_eq!(
            repo.get_target("project-1", None).await.unwrap(),
            Some("http://127.0.0.1:5173/".to_string())
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一项目下不同 worktree 可能运行不同 dev server，不能互相覆盖。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别写入 project-level 和 worktree-level target，并断言读取结果互相隔离。
    #[tokio::test]
    async fn worktree_target_is_isolated_from_project_target() {
        let repo = setup_repo().await;

        repo.upsert_target("project-1", None, "http://127.0.0.1:5173/")
            .await
            .unwrap();
        repo.upsert_target("project-1", Some("worktree-1"), "http://127.0.0.1:3000/")
            .await
            .unwrap();

        assert_eq!(
            repo.get_target("project-1", None).await.unwrap(),
            Some("http://127.0.0.1:5173/".to_string())
        );
        assert_eq!(
            repo.get_target("project-1", Some("worktree-1"))
                .await
                .unwrap(),
            Some("http://127.0.0.1:3000/".to_string())
        );
    }
}
