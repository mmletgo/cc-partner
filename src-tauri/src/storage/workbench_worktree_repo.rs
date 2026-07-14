//! storage/workbench_worktree_repo.rs — 工作台 Git worktree 元数据仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     用户创建的 Git worktree 需要在应用重启后继续出现在 Workbench 的 worktree 管理层。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_worktrees` 表 CRUD；写路径经 `with_shared_write_lease`；
//!     运行期 git 状态由 workbench/git.rs 动态查询。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::workbench::models::WorkbenchWorktreeRow;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// 工作台 Git worktree 仓库，封装 workbench_worktrees 表操作。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 需要把用户创建的 worktree 作为项目下的工作区长期保存，供重启后恢复。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool + maintenance gate，并提供 list/get/upsert/delete/delete_by_project 方法。
#[derive(Clone)]
pub struct WorkbenchWorktreeRepo {
    pool: SqlitePool,
    /// 维护屏障：写路径持 shared lease，restore exclusive 时阻塞。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchWorktreeRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一 SQLite pool 构造 worktree 仓库，供命令层共享。
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

    /// Business Logic（为什么需要这个函数）:
    ///     旧版本应用没有 workbench_worktrees 表，升级后必须自动补建而不影响用户现有项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 CREATE TABLE IF NOT EXISTS，保持幂等；schema bootstrap 不经 write lease。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_worktrees (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT, \
             base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 进入项目时需要列出该项目的主工作区和所有用户创建的 worktree。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project_id 查询；主工作区优先，其余按创建时间和名称稳定排序。
    pub async fn list_by_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<WorkbenchWorktreeRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, project_id, name, branch, base_branch, path, is_main, created_at, updated_at \
             FROM workbench_worktrees WHERE project_id = ? \
             ORDER BY is_main DESC, created_at ASC, name ASC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_worktree).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     commit/push/merge/remove 等命令需要按 worktree_id 精确找到对应磁盘路径和分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 id 查询单条记录，不存在时返回 None。
    pub async fn get(&self, id: &str) -> Result<Option<WorkbenchWorktreeRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, project_id, name, branch, base_branch, path, is_main, created_at, updated_at \
             FROM workbench_worktrees WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_worktree(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建 worktree、刷新主工作区或 Git 操作后，需要把最新元数据保存到 SQLite。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 shared write lease 后用 INSERT OR REPLACE 写入完整 row。
    pub async fn upsert(&self, row: &WorkbenchWorktreeRow) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR REPLACE INTO workbench_worktrees \
                 (id, project_id, name, branch, base_branch, path, is_main, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.id)
            .bind(&row.project_id)
            .bind(&row.name)
            .bind(&row.branch)
            .bind(&row.base_branch)
            .bind(&row.path)
            .bind(if row.is_main { 1_i64 } else { 0_i64 })
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户删除已合并或废弃的 worktree 后，管理层不应继续展示该记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 shared write lease 后按 id 删除 worktree 记录。
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query("DELETE FROM workbench_worktrees WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     移除工作台项目时，项目下的 worktree 元数据也应一起清理，避免孤儿记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 shared write lease 后按 project_id 删除全部 worktree 记录；磁盘真实 worktree 不在此删除。
    pub async fn delete_by_project(&self, project_id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query("DELETE FROM workbench_worktrees WHERE project_id = ?")
                .bind(project_id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }
}

/// Business Logic（为什么需要这个函数）:
///     sqlx Row 字段读取逻辑在 list/get 中复用，避免 SQL 字段顺序变化造成映射错误。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 读取列并构造 WorkbenchWorktreeRow，同时把 INTEGER is_main 转为 bool。
fn row_to_worktree(row: &SqliteRow) -> Result<WorkbenchWorktreeRow, AppError> {
    let is_main: i64 = row.try_get("is_main")?;
    Ok(WorkbenchWorktreeRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        name: row.try_get("name")?,
        branch: row.try_get("branch")?,
        base_branch: row.try_get("base_branch")?,
        path: row.try_get("path")?,
        is_main: is_main != 0,
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
    ///     仓库测试需要隔离 SQLite，避免影响用户真实 worktree 记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建内存数据库、初始化 workbench_worktrees 表并返回 repo。
    async fn setup_repo() -> WorkbenchWorktreeRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_worktrees (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT, \
             base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        WorkbenchWorktreeRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个测试需要构造 worktree row，统一 helper 减少样板。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 id/project_id/path 生成完整 WorkbenchWorktreeRow。
    fn row(id: &str, project_id: &str, path: &str, is_main: bool) -> WorkbenchWorktreeRow {
        WorkbenchWorktreeRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
            name: id.to_string(),
            branch: Some(id.to_string()),
            base_branch: Some("main".to_string()),
            path: path.to_string(),
            is_main,
            created_at: "2026-06-25T00:00:00Z".to_string(),
            updated_at: "2026-06-25T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 进入项目时要按创建顺序恢复 worktree 管理层。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入主 worktree 和功能 worktree，断言 list_by_project 返回两条且顺序稳定。
    #[tokio::test]
    async fn list_by_project_returns_worktrees_in_creation_order() {
        let repo = setup_repo().await;
        repo.upsert(&row("main", "p1", "/repo", true))
            .await
            .unwrap();
        repo.upsert(&row("feature", "p1", "/repo-feature", false))
            .await
            .unwrap();

        let listed = repo.list_by_project("p1").await.unwrap();

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "main");
        assert_eq!(listed[1].id, "feature");
    }
}
