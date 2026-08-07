//! storage/workbench_project_repo.rs — 工作台项目记录仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     用户添加过的本机项目需要在重启后保留，用于工作台最近项目列表。
//!
//! Code Logic（这个模块做什么）:
//!     封装 workbench_projects 表 CRUD；写路径经 `with_shared_write_lease`；
//!     使用运行期 sqlx::query，不依赖编译期 DATABASE_URL。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::project_order::{
    apply_project_order, normalize_ordered_ids, prepend_project_id, remove_project_id,
    ProjectOrderDocument,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// 工作台项目仓库，封装所有 workbench_projects 表操作。
///
/// Business Logic（为什么需要这个结构体）:
///     工作台命令层需要复用同一套项目持久化逻辑，避免直接散落 SQL。
///
/// Code Logic（这个结构体做什么）:
///     持有 SQLite pool + maintenance gate，并提供 list/get/upsert/delete 四类 CRUD 方法。
#[derive(Clone)]
pub struct WorkbenchProjectRepo {
    pool: SqlitePool,
    /// 维护屏障：写路径持 shared lease，restore exclusive 时阻塞。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl WorkbenchProjectRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造项目仓库，供命令层共享。
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
    ///     侧栏项目列表是用户的空间记忆锚点：选中项目不得让它跳到列表顶部。
    ///     默认顺序为「添加时间倒序」（新添加的在最上），与 last_opened_at 解耦；
    ///     若用户拖拽保存了自定义顺序，则按顺序文档投影（未入表的本地项目仍置顶）。
    ///     「最近打开」语义只保留给 list_recent（启动摘要「继续工作」）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查询全部项目按 created_at DESC，再读顺序单例并用 `apply_project_order` 投影。
    pub async fn list(&self) -> Result<Vec<WorkbenchProjectRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects ORDER BY created_at DESC, id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let projects: Vec<WorkbenchProjectRow> = rows.iter().map(row_to_project).collect::<Result<_, _>>()?;
        let order = self.get_order().await?;
        let ordered_ids = order
            .as_ref()
            .map(|doc| doc.ordered_ids.as_slice())
            .unwrap_or(&[]);
        Ok(apply_project_order(projects, ordered_ids, |p| p.id.as_str()))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧库升级与测试夹具需要幂等创建顺序单例表，禁止 sqlx::migrate!。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CREATE TABLE IF NOT EXISTS workbench_project_order（id 固定 'default'）。
    pub async fn ensure_order_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_project_order (\
             id TEXT PRIMARY KEY NOT NULL, \
             ordered_ids_json TEXT NOT NULL, \
             updated_at TEXT NOT NULL, \
             device_id TEXT NOT NULL)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list / reorder / 跨设备 LWW 同步都需要读取当前顺序文档。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 id='default' 行；缺表/缺行返回 None。
    pub async fn get_order(&self) -> Result<Option<ProjectOrderDocument>, AppError> {
        let row = match sqlx::query(
            "SELECT ordered_ids_json, updated_at, device_id FROM workbench_project_order WHERE id = 'default'",
        )
        .fetch_optional(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(sqlx::Error::Database(db_err))
                if db_err.message().contains("no such table") =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };
        let Some(row) = row else {
            return Ok(None);
        };
        let json: String = row.try_get("ordered_ids_json")?;
        let ordered_ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
        Ok(Some(ProjectOrderDocument {
            ordered_ids: normalize_ordered_ids(ordered_ids),
            updated_at: row.try_get("updated_at")?,
            device_id: row.try_get("device_id")?,
        }))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     拖拽结束与 LWW 同步胜出时需整表覆盖顺序文档。
    ///
    /// Code Logic（这个函数做什么）:
    ///     ensure schema 后 INSERT OR REPLACE id='default'。
    pub async fn set_order(&self, doc: &ProjectOrderDocument) -> Result<(), AppError> {
        let ordered_ids = normalize_ordered_ids(doc.ordered_ids.clone());
        let json = serde_json::to_string(&ordered_ids)?;
        with_shared_write_lease(&self.gate, async {
            Self::ensure_order_schema(&self.pool).await?;
            sqlx::query(
                "INSERT OR REPLACE INTO workbench_project_order \
                 (id, ordered_ids_json, updated_at, device_id) VALUES ('default', ?, ?, ?)",
            )
            .bind(&json)
            .bind(&doc.updated_at)
            .bind(&doc.device_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新项目默认置顶，并写入顺序文档以便跨设备对齐相对序。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读现有顺序 → prepend id → set_order（无文档时仅写 [id]）。
    pub async fn prepend_order_id(
        &self,
        project_id: &str,
        updated_at: &str,
        device_id: &str,
    ) -> Result<(), AppError> {
        let current = self.get_order().await?;
        let ordered_ids = match current {
            Some(doc) => prepend_project_id(&doc.ordered_ids, project_id),
            None => vec![project_id.to_string()],
        };
        self.set_order(&ProjectOrderDocument {
            ordered_ids,
            updated_at: updated_at.to_string(),
            device_id: device_id.to_string(),
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     移除项目后清理顺序文档中的 id，避免文档无限膨胀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     有文档则过滤 id 并写回；无文档 no-op。
    pub async fn remove_order_id(
        &self,
        project_id: &str,
        updated_at: &str,
        device_id: &str,
    ) -> Result<(), AppError> {
        let Some(doc) = self.get_order().await? else {
            return Ok(());
        };
        let ordered_ids = remove_project_id(&doc.ordered_ids, project_id);
        if ordered_ids == doc.ordered_ids {
            return Ok(());
        }
        self.set_order(&ProjectOrderDocument {
            ordered_ids,
            updated_at: updated_at.to_string(),
            device_id: device_id.to_string(),
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Workbench 启动摘要只展示最近打开的少量项目，避免全表加载膨胀。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 last_opened_at DESC 单查询 LIMIT，limit 裁剪到 0..=5。
    pub async fn list_recent(&self, limit: i64) -> Result<Vec<WorkbenchProjectRow>, AppError> {
        let limit = limit.clamp(0, 5);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at \
             FROM workbench_projects ORDER BY last_opened_at DESC LIMIT ?",
        )
        .bind(limit)
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
    ///     持 shared write lease 后用 INSERT OR REPLACE 写入完整 row。
    pub async fn upsert(&self, row: &WorkbenchProjectRow) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
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
        })
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可以从工作台最近项目列表移除项目；移除不删除磁盘文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 shared write lease 后按 id 删除项目记录。
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query("DELETE FROM workbench_projects WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
            Ok(())
        })
        .await
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
        WorkbenchProjectRepo::ensure_order_schema(&pool).await.unwrap();
        WorkbenchProjectRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个测试都需要构造项目记录，统一 helper 可减少样板并突出断言差异。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 id 和 last_opened_at 生成完整 WorkbenchProjectRow。
    fn row(id: &str, last_opened_at: &str) -> WorkbenchProjectRow {
        row_created_at(id, last_opened_at, "2026-06-24T00:00:00Z")
    }

    /// Business Logic（为什么需要这个函数）:
    ///     列表顺序由 created_at 决定，排序断言必须能独立控制添加时间与最近打开时间。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 id、last_opened_at 和 created_at 生成完整 WorkbenchProjectRow。
    fn row_created_at(id: &str, last_opened_at: &str, created_at: &str) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: id.to_string(),
            name: format!("Project {id}"),
            kind: "local".to_string(),
            device_id: "device-1".to_string(),
            device_name: "MacBook".to_string(),
            path: format!("/tmp/{id}"),
            last_opened_at: last_opened_at.to_string(),
            created_at: created_at.to_string(),
            updated_at: "2026-06-24T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     侧栏项目顺序必须稳定：新添加的项目在最上，且选中项目不得置顶。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入两条 created_at 不同的记录，断言 list 按 created_at 倒序返回。
    #[tokio::test]
    async fn list_orders_by_created_at_desc() {
        let repo = setup_repo().await;
        repo.upsert(&row_created_at(
            "p1",
            "2026-06-24T01:00:00Z",
            "2026-06-20T00:00:00Z",
        ))
        .await
        .unwrap();
        repo.upsert(&row_created_at(
            "p2",
            "2026-06-24T02:00:00Z",
            "2026-06-21T00:00:00Z",
        ))
        .await
        .unwrap();

        let listed = repo.list().await.unwrap();

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "p2");
        assert_eq!(listed[1].id, "p1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     touch_workbench_project 只更新 last_opened_at；选中项目后列表顺序必须保持不变。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入两条记录后把较旧项目的 last_opened_at 推到最新，断言 list 顺序未变。
    #[tokio::test]
    async fn touching_last_opened_at_does_not_reorder_list() {
        let repo = setup_repo().await;
        repo.upsert(&row_created_at(
            "p1",
            "2026-06-24T01:00:00Z",
            "2026-06-20T00:00:00Z",
        ))
        .await
        .unwrap();
        repo.upsert(&row_created_at(
            "p2",
            "2026-06-24T02:00:00Z",
            "2026-06-21T00:00:00Z",
        ))
        .await
        .unwrap();

        // 模拟用户选中较早添加的 p1：只刷新 last_opened_at，created_at 不变。
        repo.upsert(&row_created_at(
            "p1",
            "2026-06-24T09:00:00Z",
            "2026-06-20T00:00:00Z",
        ))
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

    /// Business Logic（为什么需要这个测试）:
    ///     workbench_launch_summary 项目区只展示最近 5 项，必须在 SQL 层 LIMIT。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 6 条后 list_recent(5) 返回 5 条且最旧不在列表。
    #[tokio::test]
    async fn workbench_launch_summary_list_recent_is_bounded() {
        let repo = setup_repo().await;
        for i in 0..6 {
            repo.upsert(&row(&format!("p{i}"), &format!("2026-06-24T0{i}:00:00Z")))
                .await
                .unwrap();
        }
        let listed = repo.list_recent(5).await.unwrap();
        assert_eq!(listed.len(), 5);
        assert_eq!(listed[0].id, "p5");
        assert!(!listed.iter().any(|p| p.id == "p0"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     拖拽保存的顺序必须驱动 list；未知 id 忽略；未入表本地项目置顶。
    #[tokio::test]
    async fn list_applies_custom_order_and_tops_missing_local() {
        let repo = setup_repo().await;
        repo.upsert(&row_created_at("p1", "2026-06-24T01:00:00Z", "2026-06-20T00:00:00Z"))
            .await
            .unwrap();
        repo.upsert(&row_created_at("p2", "2026-06-24T02:00:00Z", "2026-06-21T00:00:00Z"))
            .await
            .unwrap();
        repo.upsert(&row_created_at("p3", "2026-06-24T03:00:00Z", "2026-06-22T00:00:00Z"))
            .await
            .unwrap();

        repo.set_order(&ProjectOrderDocument {
            ordered_ids: vec!["p1".into(), "p2".into(), "ghost".into()],
            updated_at: "2026-06-25T00:00:00Z".into(),
            device_id: "device-1".into(),
        })
        .await
        .unwrap();

        let listed = repo.list().await.unwrap();
        // default created_at DESC is p3,p2,p1；p3 未入表 → 置顶，其后 p1,p2
        assert_eq!(
            listed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["p3", "p1", "p2"]
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     prepend/remove 必须整表更新顺序文档。
    #[tokio::test]
    async fn prepend_and_remove_order_ids() {
        let repo = setup_repo().await;
        repo.prepend_order_id("p1", "2026-06-25T00:00:00Z", "d1")
            .await
            .unwrap();
        repo.prepend_order_id("p2", "2026-06-25T00:01:00Z", "d1")
            .await
            .unwrap();
        let doc = repo.get_order().await.unwrap().unwrap();
        assert_eq!(doc.ordered_ids, vec!["p2".to_string(), "p1".to_string()]);

        repo.remove_order_id("p2", "2026-06-25T00:02:00Z", "d1")
            .await
            .unwrap();
        let doc = repo.get_order().await.unwrap().unwrap();
        assert_eq!(doc.ordered_ids, vec!["p1".to_string()]);
    }
}
