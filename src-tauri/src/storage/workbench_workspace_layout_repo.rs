//! storage/workbench_workspace_layout_repo.rs — Workbench 工作现场 layout 仓库（CAS）。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端自动保存与命名 snapshot 需要在控制设备本地 SQLite 持久化结构 metadata，
//!     并通过 revision CAS 防止并发 selection 互相覆盖。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `workbench_workspace_layouts` 表；`save_cas` 在单事务内按 expectedRevision 更新；
//!     创建要求 slot 不存在；named 删除仅允许 kind=named。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::workbench::workspace_layout::{
    ensure_known_schema_version, validate_and_normalize_draft, InspectorTab, WorkspaceLayout,
    WorkspaceLayoutDraft, WorkspaceLayoutKind, WorkspaceView, WORKSPACE_LAYOUT_SCHEMA_VERSION,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// layout 表建表 SQL（幂等 CREATE TABLE IF NOT EXISTS）。
pub const WORKBENCH_WORKSPACE_LAYOUT_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS workbench_workspace_layouts (
    id TEXT PRIMARY KEY NOT NULL,
    slot_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    name TEXT,
    schema_version INTEGER NOT NULL,
    project_id TEXT NOT NULL,
    active_worktree_id TEXT,
    active_session_id TEXT,
    workspace_view TEXT NOT NULL,
    inspector_tab TEXT NOT NULL,
    browser_target_url TEXT,
    revision INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// Workbench workspace layout 仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     命令层与 restore 协调器需要统一的 get/save_cas/list/delete 入口。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + maintenance gate；写路径经 shared lease。
#[derive(Clone)]
pub struct WorkbenchWorkspaceLayoutRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchWorkspaceLayoutRepo {
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
        sqlx::query(WORKBENCH_WORKSPACE_LAYOUT_SCHEMA)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     autosave/preflight 需要按 slot 读取当前 layout 与 revision。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 slot_key 查询；解析枚举；未知 schema fail-closed。
    pub async fn get_by_slot(&self, slot_key: &str) -> Result<Option<WorkspaceLayout>, AppError> {
        let row = sqlx::query(
            "SELECT id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
             active_session_id, workspace_view, inspector_tab, browser_target_url, revision, \
             created_at, updated_at FROM workbench_workspace_layouts WHERE slot_key = ?",
        )
        .bind(slot_key)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_layout(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     apply 与冲突对账需要按 id 读取权威 layout。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按主键 id 查询。
    pub async fn get_by_id(&self, id: &str) -> Result<Option<WorkspaceLayout>, AppError> {
        let row = sqlx::query(
            "SELECT id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
             active_session_id, workspace_view, inspector_tab, browser_target_url, revision, \
             created_at, updated_at FROM workbench_workspace_layouts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| row_to_layout(&r)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命名 snapshot 列表需要展示用户保存的结构现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `WHERE kind='named' ORDER BY updated_at DESC`。
    pub async fn list_named(&self) -> Result<Vec<WorkspaceLayout>, AppError> {
        let rows = sqlx::query(
            "SELECT id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
             active_session_id, workspace_view, inspector_tab, browser_target_url, revision, \
             created_at, updated_at FROM workbench_workspace_layouts \
             WHERE kind = 'named' ORDER BY updated_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_layout).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     稳定 selection 变化后需要 CAS 写入 layout，防止并发覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单事务：slot 不存在则 create（expected 必须 None）；存在则 `WHERE revision = expected` 更新。
    ///     冲突返回 `workspace_layout_conflict`。
    pub async fn save_cas(
        &self,
        draft: WorkspaceLayoutDraft,
        expected_revision: Option<u64>,
    ) -> Result<WorkspaceLayout, AppError> {
        let draft = validate_and_normalize_draft(draft)?;
        with_shared_write_lease(&self.gate, async {
            let mut tx = self.pool.begin().await?;
            let existing = sqlx::query(
                "SELECT id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
                 active_session_id, workspace_view, inspector_tab, browser_target_url, revision, \
                 created_at, updated_at FROM workbench_workspace_layouts WHERE slot_key = ?",
            )
            .bind(&draft.slot_key)
            .fetch_optional(&mut *tx)
            .await?;

            let now = chrono::Utc::now().to_rfc3339();
            let result = if let Some(existing_row) = existing {
                let current = row_to_layout(&existing_row)?;
                let expected = expected_revision.ok_or_else(|| {
                    AppError::conflict("workspace_layout_conflict".to_string())
                })?;
                if current.revision != expected {
                    return Err(AppError::conflict("workspace_layout_conflict".to_string()));
                }
                let next_revision = current.revision.saturating_add(1);
                let affected = sqlx::query(
                    "UPDATE workbench_workspace_layouts SET kind = ?, name = ?, schema_version = ?, \
                     project_id = ?, active_worktree_id = ?, active_session_id = ?, workspace_view = ?, \
                     inspector_tab = ?, browser_target_url = ?, revision = ?, updated_at = ? \
                     WHERE slot_key = ? AND revision = ?",
                )
                .bind(draft.kind.as_str())
                .bind(&draft.name)
                .bind(i64::from(WORKSPACE_LAYOUT_SCHEMA_VERSION))
                .bind(&draft.project_id)
                .bind(&draft.active_worktree_id)
                .bind(&draft.active_session_id)
                .bind(draft.workspace_view.as_str())
                .bind(draft.inspector_tab.as_str())
                .bind(&draft.browser_target_url)
                .bind(next_revision as i64)
                .bind(&now)
                .bind(&draft.slot_key)
                .bind(expected as i64)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if affected != 1 {
                    return Err(AppError::conflict("workspace_layout_conflict".to_string()));
                }
                WorkspaceLayout {
                    schema_version: WORKSPACE_LAYOUT_SCHEMA_VERSION,
                    id: current.id,
                    slot_key: draft.slot_key,
                    kind: draft.kind,
                    name: draft.name,
                    project_id: draft.project_id,
                    active_worktree_id: draft.active_worktree_id,
                    active_session_id: draft.active_session_id,
                    workspace_view: draft.workspace_view,
                    inspector_tab: draft.inspector_tab,
                    browser_target_url: draft.browser_target_url,
                    revision: next_revision,
                    created_at: current.created_at,
                    updated_at: now,
                }
            } else {
                if expected_revision.is_some() {
                    return Err(AppError::conflict("workspace_layout_conflict".to_string()));
                }
                let id = uuid::Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO workbench_workspace_layouts \
                     (id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
                      active_session_id, workspace_view, inspector_tab, browser_target_url, \
                      revision, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&draft.slot_key)
                .bind(draft.kind.as_str())
                .bind(&draft.name)
                .bind(i64::from(WORKSPACE_LAYOUT_SCHEMA_VERSION))
                .bind(&draft.project_id)
                .bind(&draft.active_worktree_id)
                .bind(&draft.active_session_id)
                .bind(draft.workspace_view.as_str())
                .bind(draft.inspector_tab.as_str())
                .bind(&draft.browser_target_url)
                .bind(1_i64)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                WorkspaceLayout {
                    schema_version: WORKSPACE_LAYOUT_SCHEMA_VERSION,
                    id,
                    slot_key: draft.slot_key,
                    kind: draft.kind,
                    name: draft.name,
                    project_id: draft.project_id,
                    active_worktree_id: draft.active_worktree_id,
                    active_session_id: draft.active_session_id,
                    workspace_view: draft.workspace_view,
                    inspector_tab: draft.inspector_tab,
                    browser_target_url: draft.browser_target_url,
                    revision: 1,
                    created_at: now.clone(),
                    updated_at: now,
                }
            };
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户删除命名 snapshot；禁止删除 auto slot。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅 `kind=named` 行可删；按 id 删除。
    pub async fn delete_named(&self, id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let row = sqlx::query("SELECT kind FROM workbench_workspace_layouts WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
            let Some(row) = row else {
                return Err(AppError::not_found(
                    "workspace_layout_not_found".to_string(),
                ));
            };
            let kind: String = row.try_get("kind")?;
            if kind != WorkspaceLayoutKind::Named.as_str() {
                return Err(AppError::validation(
                    "workspace_layout_delete_named_only".to_string(),
                ));
            }
            sqlx::query("DELETE FROM workbench_workspace_layouts WHERE id = ? AND kind = 'named'")
                .bind(id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
    }
}

/// Business Logic（为什么需要这个函数）:
///     list/get/save 共享 row→结构体映射。
///
/// Code Logic（这个函数做什么）:
///     读取列、校验 schema、解析枚举。
fn row_to_layout(row: &SqliteRow) -> Result<WorkspaceLayout, AppError> {
    let schema_version = row.try_get::<i64, _>("schema_version")? as u32;
    ensure_known_schema_version(schema_version)?;
    let kind = WorkspaceLayoutKind::parse(&row.try_get::<String, _>("kind")?)?;
    let workspace_view = WorkspaceView::parse(&row.try_get::<String, _>("workspace_view")?)?;
    let inspector_tab = InspectorTab::parse(&row.try_get::<String, _>("inspector_tab")?)?;
    let revision = row.try_get::<i64, _>("revision")? as u64;
    Ok(WorkspaceLayout {
        schema_version,
        id: row.try_get("id")?,
        slot_key: row.try_get("slot_key")?,
        kind,
        name: row.try_get("name")?,
        project_id: row.try_get("project_id")?,
        active_worktree_id: row.try_get("active_worktree_id")?,
        active_session_id: row.try_get("active_session_id")?,
        workspace_view,
        inspector_tab,
        browser_target_url: row.try_get("browser_target_url")?,
        revision,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::workspace_layout::{
        desktop_auto_slot_key, new_named_slot_key, InspectorTab, WorkspaceLayoutDraft,
        WorkspaceLayoutKind, WorkspaceView,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     仓库测试需要隔离内存库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     建表并返回 repo。
    async fn layout_repo() -> WorkbenchWorkspaceLayoutRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let repo = WorkbenchWorkspaceLayoutRepo::new(pool);
        repo.ensure_schema().await.unwrap();
        repo
    }

    /// Business Logic（为什么需要这个函数）:
    ///     构造 auto draft。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回指定 project 的 auto draft。
    fn auto_draft(project_id: &str) -> WorkspaceLayoutDraft {
        WorkspaceLayoutDraft {
            slot_key: desktop_auto_slot_key().to_string(),
            kind: WorkspaceLayoutKind::Auto,
            name: None,
            project_id: project_id.to_string(),
            active_worktree_id: Some("w1".to_string()),
            active_session_id: Some("s1".to_string()),
            workspace_view: WorkspaceView::Terminal,
            inspector_tab: InspectorTab::Files,
            browser_target_url: None,
        }
    }

    #[tokio::test]
    async fn stale_layout_revision_cannot_overwrite_newer_selection() {
        let repo = layout_repo().await;
        let first = repo.save_cas(auto_draft("p1"), None).await.unwrap();
        let second = repo
            .save_cas(auto_draft("p2"), Some(first.revision))
            .await
            .unwrap();
        let error = repo
            .save_cas(auto_draft("p3"), Some(first.revision))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "workspace_layout_conflict");
        assert_eq!(
            repo.get_by_slot("desktop:auto")
                .await
                .unwrap()
                .unwrap()
                .project_id,
            "p2"
        );
        assert_eq!(second.revision, first.revision + 1);
    }

    #[tokio::test]
    async fn create_requires_no_existing_slot_and_unique_slot() {
        let repo = layout_repo().await;
        let first = repo.save_cas(auto_draft("p1"), None).await.unwrap();
        assert_eq!(first.revision, 1);
        let err = repo.save_cas(auto_draft("p2"), None).await.unwrap_err();
        assert_eq!(err.code(), "workspace_layout_conflict");
    }

    #[tokio::test]
    async fn named_layout_list_and_delete() {
        let repo = layout_repo().await;
        let slot = new_named_slot_key();
        let draft = WorkspaceLayoutDraft {
            slot_key: slot,
            kind: WorkspaceLayoutKind::Named,
            name: Some("Morning".to_string()),
            project_id: "p1".to_string(),
            active_worktree_id: None,
            active_session_id: None,
            workspace_view: WorkspaceView::Files,
            inspector_tab: InspectorTab::Git,
            browser_target_url: Some("http://127.0.0.1:5173/".to_string()),
        };
        let saved = repo.save_cas(draft, None).await.unwrap();
        let listed = repo.list_named().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name.as_deref(), Some("Morning"));

        // auto 不可经 delete_named 删除
        let auto = repo.save_cas(auto_draft("p1"), None).await.unwrap();
        let err = repo.delete_named(&auto.id).await.unwrap_err();
        assert_eq!(err.code(), "workspace_layout_delete_named_only");

        repo.delete_named(&saved.id).await.unwrap();
        assert!(repo.list_named().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_browser_url_rejected_before_persist() {
        let repo = layout_repo().await;
        let mut draft = auto_draft("p1");
        draft.browser_target_url = Some("https://evil.example".to_string());
        assert!(repo.save_cas(draft, None).await.is_err());
    }

    #[tokio::test]
    async fn unknown_schema_row_fails_closed_on_read() {
        let repo = layout_repo().await;
        sqlx::query(
            "INSERT INTO workbench_workspace_layouts \
             (id, slot_key, kind, name, schema_version, project_id, active_worktree_id, \
              active_session_id, workspace_view, inspector_tab, browser_target_url, \
              revision, created_at, updated_at) \
             VALUES ('x','desktop:auto','auto',NULL,99,'p1',NULL,NULL,'terminal','files',NULL,1,'t','t')",
        )
        .execute(&repo.pool)
        .await
        .unwrap();
        let err = repo.get_by_slot("desktop:auto").await.unwrap_err();
        assert!(err.code().contains("workspace_layout_unknown_schema"));
    }

    #[tokio::test]
    async fn ensure_schema_is_idempotent_on_existing_database() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 模拟已有其它表的旧库
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (id TEXT PRIMARY KEY, name TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let repo = WorkbenchWorkspaceLayoutRepo::new(pool);
        repo.ensure_schema().await.unwrap();
        repo.ensure_schema().await.unwrap();
        let saved = repo.save_cas(auto_draft("p-upgrade"), None).await.unwrap();
        assert_eq!(saved.project_id, "p-upgrade");
    }
}
