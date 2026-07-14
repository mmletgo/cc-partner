//! storage/scratchpad_repo.rs — 速记本多页面数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     Scratchpad 从单个自动保存文本升级为多页面自动保存。页面、局域网同步和 GitHub 云同步
//!     都必须读写同一份 SQLite 权威数据；旧单例内容保留为 id="scratchpad" 的默认页。
//!
//! Code Logic（这个模块做什么）:
//!     持有 SqlitePool，提供 schema 兼容迁移、页面 CRUD、同步全量读取和批量 upsert。
//!     vector_clock 以 JSON TEXT 存储；删除为软删除，清空文本是普通 content="" 更新。

use crate::error::AppError;
use crate::models::scratchpad::{ScratchpadRow, SCRATCHPAD_ID};
use crate::storage::maintenance_gate::{
    begin_shared_write, with_shared_write_lease, DatabaseMaintenanceGate,
};
use crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo;
use crate::storage::sync_request_ledger_repo::DOMAIN_SCRATCHPAD;
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// 速记本仓库，封装 scratchpad 表的所有数据库操作。
pub struct ScratchpadRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
    /// 与 restore exclusive 共享的写屏障。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl ScratchpadRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: 单测与局部 fixture 不依赖 AppState 共享 gate。
    /// Code Logic: 新建独立 DatabaseMaintenanceGate。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 所有生产 writer 与 restore exclusive 共享屏障。
    /// Code Logic: 保存 pool + gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 暴露共享 gate（供 apply_merge ledger 对齐屏障）。
    ///
    /// Business Logic: push-batch ledger 必须与域 repo 使用同一 gate。
    /// Code Logic: clone Arc。
    pub fn gate(&self) -> Arc<DatabaseMaintenanceGate> {
        self.gate.clone()
    }

    /// 当前时间的 RFC3339 字符串。
    ///
    /// Business Logic: 新建和更新页面时需要稳定的 LWW 时间戳参与冲突解决。
    /// Code Logic: 使用 UTC RFC3339，与 prompts/ssh_targets 的时间格式一致。
    fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    /// 归一化页面标题。
    ///
    /// Business Logic: 用户可以输入空标题，但列表中必须有可展示名称。
    /// Code Logic: trim 后为空返回“未命名”，否则返回 trim 后文本。
    fn normalize_title(title: &str) -> String {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            "未命名".to_string()
        } else {
            trimmed.to_string()
        }
    }

    /// 确保旧库 schema 拥有 title 列。
    ///
    /// Business Logic: 已安装用户的旧 scratchpad 表没有 title；升级后必须无损迁移为默认页标题“速记本”。
    /// Code Logic: PRAGMA table_info 检查列名，缺失时 ALTER TABLE ADD COLUMN title TEXT NOT NULL DEFAULT '速记本'。
    pub async fn ensure_schema(&self) -> Result<(), AppError> {
        let columns = sqlx::query("PRAGMA table_info(scratchpad)")
            .fetch_all(&self.db)
            .await?;
        let has_title = columns.iter().any(|row| {
            row.try_get::<String, _>("name")
                .map(|name| name == "title")
                .unwrap_or(false)
        });
        if !has_title {
            sqlx::query("ALTER TABLE scratchpad ADD COLUMN title TEXT NOT NULL DEFAULT '速记本'")
                .execute(&self.db)
                .await?;
        }
        Ok(())
    }

    /// 将数据库行映射为 ScratchpadRow。
    ///
    /// Business Logic: DB 中 vector_clock/deleted 以 TEXT/INTEGER 保存，业务层需要结构化数据。
    /// Code Logic: JSON 反序列化 vector_clock；deleted 0/1 转 bool；delete_epoch 缺列默认 0。
    fn row_to_scratchpad(row: &SqliteRow) -> Result<ScratchpadRow, AppError> {
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        let delete_epoch = row.try_get::<i64, _>("delete_epoch").unwrap_or(0).max(0) as u64;
        Ok(ScratchpadRow {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            content: row.try_get("content")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            deleted: deleted_int != 0,
            delete_epoch,
        })
    }

    /// 列出所有未删除页面，按 updated_at 降序。
    ///
    /// Business Logic: 前端侧栏只展示当前有效页面，最近编辑的页面应排在前面。
    /// Code Logic: 查询 deleted=0 的完整行；命令层再投影为 summary DTO。
    pub async fn list_pages(&self) -> Result<Vec<ScratchpadRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM scratchpad WHERE deleted = 0 ORDER BY updated_at DESC",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_scratchpad).collect()
    }

    /// 按页面 id 查询一页（含已删除记录，供同步和删除判断使用）。
    ///
    /// Business Logic: 页面详情、同步合并和软删除都需要按 id 精确读取。
    /// Code Logic: id 作为主键查询，返回 Option 由调用方决定 not-found 行为。
    pub async fn get(&self, page_id: &str) -> Result<Option<ScratchpadRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM scratchpad WHERE id = ?",
        )
        .bind(page_id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_scratchpad(&r)?)),
            None => Ok(None),
        }
    }

    /// 在已开启事务内按页面 id 读取 scratchpad（含 deleted）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     merge plan 必须在写事务内读 local，避免 max_connections(1) 下事务外 pool.get
    ///     与 begin_shared_write 死锁，并保证 plan/write 同一快照。
    ///
    /// Code Logic（这个函数做什么）:
    ///     与 `get` 相同 SELECT，经 `fetch_optional(&mut **tx)` 执行。
    pub async fn get_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        page_id: &str,
    ) -> Result<Option<ScratchpadRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM scratchpad WHERE id = ?",
        )
        .bind(page_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_scratchpad(&r)?)),
            None => Ok(None),
        }
    }

    /// 获取默认页；若不存在则创建空白默认页。
    ///
    /// Business Logic: 旧速记本入口需要稳定落到默认页，首次启动也应有可自动保存页面。
    ///     空初始化不代表一次用户编辑，因此 vector_clock 为空，避免传播无意义变更。
    /// Code Logic: id 固定为 "scratchpad"，title 固定为“速记本”，content 为空。
    pub async fn get_or_create_default_page(
        &self,
        device_id: &str,
    ) -> Result<ScratchpadRow, AppError> {
        if let Some(row) = self.get(SCRATCHPAD_ID).await? {
            return Ok(row);
        }
        let now = Self::now_iso();
        let row = ScratchpadRow {
            id: SCRATCHPAD_ID.to_string(),
            title: "速记本".to_string(),
            content: String::new(),
            created_at: now.clone(),
            updated_at: now,
            device_id: device_id.to_string(),
            vector_clock: HashMap::new(),
            deleted: false,
            delete_epoch: 0,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 创建普通新页面。
    ///
    /// Business Logic: 用户新增页面时应立即生成可保存、可同步的独立页面；空标题统一展示为“未命名”。
    /// Code Logic: UUID v4 作为 id，vector_clock 初始化为 {device_id:1}，支持测试传入固定 updated_at。
    pub async fn create_page(
        &self,
        title: &str,
        content: &str,
        device_id: &str,
        updated_at_override: Option<&str>,
    ) -> Result<ScratchpadRow, AppError> {
        let now = updated_at_override
            .map(ToString::to_string)
            .unwrap_or_else(Self::now_iso);
        let mut vector_clock = HashMap::new();
        vector_clock.insert(device_id.to_string(), 1);
        let row = ScratchpadRow {
            id: Uuid::new_v4().to_string(),
            title: Self::normalize_title(title),
            content: content.to_string(),
            created_at: now.clone(),
            updated_at: now,
            device_id: device_id.to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: 0,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 更新页面内容并推进本机向量时钟。
    ///
    /// Business Logic: 页面自动保存、清空都调用此方法；每次用户文本变更都必须被同步层感知。
    /// Code Logic: 读取旧行，保留 title/created_at/delete_epoch，content 改为 next，updated_at=now，
    ///     vector_clock[device_id]+=1，deleted=false，然后 upsert。
    pub async fn update_page_content(
        &self,
        page_id: &str,
        next: &str,
        device_id: &str,
    ) -> Result<ScratchpadRow, AppError> {
        let existing = self
            .get(page_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?;
        let mut vector_clock = existing.vector_clock.clone();
        let counter = vector_clock.entry(device_id.to_string()).or_insert(0);
        *counter += 1;
        let row = ScratchpadRow {
            id: existing.id,
            title: existing.title,
            content: next.to_string(),
            created_at: existing.created_at,
            updated_at: Self::now_iso(),
            device_id: device_id.to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: existing.delete_epoch,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 重命名页面并推进本机向量时钟。
    ///
    /// Business Logic: 标题是同步字段，重命名需要传播到局域网与云端。
    /// Code Logic: 读取旧行，归一化 title，更新时间与 device_id，vector_clock[device_id]+=1。
    pub async fn rename_page(
        &self,
        page_id: &str,
        title: &str,
        device_id: &str,
    ) -> Result<ScratchpadRow, AppError> {
        let existing = self
            .get(page_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?;
        let mut vector_clock = existing.vector_clock.clone();
        let counter = vector_clock.entry(device_id.to_string()).or_insert(0);
        *counter += 1;
        let row = ScratchpadRow {
            id: existing.id,
            title: Self::normalize_title(title),
            content: existing.content,
            created_at: existing.created_at,
            updated_at: Self::now_iso(),
            device_id: device_id.to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: existing.delete_epoch,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 软删除页面：同事务铸造 delete_epoch 并写入 tombstone 字段。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     删除需要传播到其他设备和云端，因此不能物理删除；同时铸造单调
    ///     delete_epoch，供 peer 水位 ack 与安全 GC。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取旧行推进 vector_clock；begin_shared_write → mint_on_tx(DOMAIN_SCRATCHPAD) →
    ///     UPDATE deleted/updated_at/device_id/vector_clock/delete_epoch → commit，返回完整 Row。
    pub async fn soft_delete_page(
        &self,
        page_id: &str,
        device_id: &str,
    ) -> Result<ScratchpadRow, AppError> {
        let existing = self
            .get(page_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?;
        let mut vector_clock = existing.vector_clock.clone();
        let counter = vector_clock.entry(device_id.to_string()).or_insert(0);
        *counter += 1;
        let updated_at = Self::now_iso();
        let vc_text = serde_json::to_string(&vector_clock)?;
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let delete_epoch = SyncDeleteSequenceRepo::mint_on_tx(&mut tx, DOMAIN_SCRATCHPAD).await?;
        sqlx::query(
            "UPDATE scratchpad SET deleted = 1, updated_at = ?, device_id = ?, vector_clock = ?, delete_epoch = ? WHERE id = ?",
        )
        .bind(&updated_at)
        .bind(device_id)
        .bind(vc_text)
        .bind(delete_epoch as i64)
        .bind(page_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        drop(permit);
        Ok(ScratchpadRow {
            id: existing.id,
            title: existing.title,
            content: existing.content,
            created_at: existing.created_at,
            updated_at,
            device_id: device_id.to_string(),
            vector_clock,
            deleted: true,
            delete_epoch,
        })
    }

    /// 返回全部页面（含 deleted 软删除记录），用于跨设备同步和云同步。
    pub async fn get_all_for_sync(&self) -> Result<Vec<ScratchpadRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM scratchpad",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_scratchpad).collect()
    }

    /// 批量插入/替换页面。
    ///
    /// Business Logic: 同步合并后需要批量落库胜出版本；每页以 id 为主键。
    ///     整批必须原子：中途失败不得留下半批次。
    /// Code Logic: 委托事务性 `bulk_upsert_in_transaction`（无 inject）。
    pub async fn bulk_upsert(&self, rows: &[ScratchpadRow]) -> Result<(), AppError> {
        self.bulk_upsert_in_transaction(rows, None).await
    }

    /// 事务性 bulk_upsert，可选注入失败点，供 quality_faults L2 / 模块测试验证 rollback。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试必须调用与生产相同的 `bulk_upsert` 事务边界。本 seam 仅 debug/test-only
    ///     （`cfg(test)` 或 `debug_assertions`），release 剥离。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `bulk_upsert_in_transaction(rows, inject_fail_at)`。
    #[cfg(any(test, debug_assertions))]
    pub async fn bulk_upsert_inject_fail_at(
        &self,
        rows: &[ScratchpadRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        self.bulk_upsert_in_transaction(rows, inject_fail_at).await
    }

    /// 在自有事务中 bulk upsert。
    ///
    /// Business Logic: 生产与 inject 路径共享事务语义，并经 shared lease。
    /// Code Logic: begin_shared_write → on_tx → commit。
    async fn bulk_upsert_in_transaction(
        &self,
        rows: &[ScratchpadRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        if rows.is_empty() {
            return Ok(());
        }
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        Self::bulk_upsert_on_tx(&mut tx, rows, inject_fail_at).await?;
        tx.commit().await?;
        drop(permit);
        Ok(())
    }

    /// 在已开启事务上执行 bulk upsert 循环（供 push-batch ledger 同事务调用）。
    ///
    /// Business Logic: ledger claim 与领域写入必须同一事务。
    /// Code Logic: 逐条 INSERT OR REPLACE；inject 命中返回 Err。
    pub async fn bulk_upsert_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        rows: &[ScratchpadRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        for (idx, row) in rows.iter().enumerate() {
            if inject_fail_at == Some(idx) {
                return Err(AppError::generic("injected scratchpad bulk_upsert failure"));
            }
            let vc_text = serde_json::to_string(&row.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO scratchpad \
                 (id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.id)
            .bind(&row.title)
            .bind(&row.content)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .bind(&row.device_id)
            .bind(vc_text)
            .bind(row.deleted as i64)
            .bind(row.delete_epoch as i64)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// 返回底层 pool（供 push-batch ledger 共享）。
    ///
    /// Business Logic: 路由层需要同一 pool 开事务。
    /// Code Logic: clone SqlitePool。
    pub fn pool(&self) -> SqlitePool {
        self.db.clone()
    }

    /// 插入/替换单页。
    ///
    /// Business Logic: 单页写入必须经 shared lease，与 restore exclusive 互斥。
    /// Code Logic: 序列化 vector_clock 后经 `with_shared_write_lease` 执行 INSERT OR REPLACE。
    pub async fn upsert(&self, row: &ScratchpadRow) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(&row.vector_clock)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR REPLACE INTO scratchpad \
                 (id, title, content, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.id)
            .bind(&row.title)
            .bind(&row.content)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .bind(&row.device_id)
            .bind(vc_text)
            .bind(row.deleted as i64)
            .bind(row.delete_epoch as i64)
            .execute(&self.db)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存 SQLite 并建好指定 scratchpad schema，返回仓库。
    async fn setup_repo_with_schema(schema: &str) -> ScratchpadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(schema).execute(&pool).await.unwrap();
        SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        // 测试用：幂等补齐本表 delete_epoch（不调用全域 helper，避免缺表）
        let cols = sqlx::query("PRAGMA table_info(scratchpad)")
            .fetch_all(&pool)
            .await
            .unwrap();
        let has = cols.iter().any(|r| {
            use sqlx::Row;
            r.try_get::<String, _>("name")
                .map(|n| n == "delete_epoch")
                .unwrap_or(false)
        });
        if !has {
            sqlx::query(
                "ALTER TABLE scratchpad ADD COLUMN delete_epoch INTEGER NOT NULL DEFAULT 0",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let repo = ScratchpadRepo::new(pool);
        repo.ensure_schema().await.unwrap();
        repo
    }

    /// 构造内存 SQLite 并建好多页面 scratchpad 表，返回仓库。
    async fn setup_repo() -> ScratchpadRepo {
        setup_repo_with_schema(
            "CREATE TABLE IF NOT EXISTS scratchpad (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
             vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .await
    }

    /// 旧单例表迁移时会补 title 列，并把已有内容保留为“速记本”页面。
    #[tokio::test]
    async fn ensure_schema_adds_title_to_legacy_singleton_table() {
        let repo = setup_repo_with_schema(
            "CREATE TABLE IF NOT EXISTS scratchpad (\
             id TEXT PRIMARY KEY, content TEXT NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
             deleted INTEGER DEFAULT 0)",
        )
        .await;
        let now = ScratchpadRepo::now_iso();
        let legacy = ScratchpadRow {
            id: "scratchpad".to_string(),
            title: "速记本".to_string(),
            content: "legacy content".to_string(),
            created_at: now.clone(),
            updated_at: now,
            device_id: "device-a".to_string(),
            vector_clock: HashMap::new(),
            deleted: false,
            delete_epoch: 0,
        };
        repo.upsert(&legacy).await.unwrap();

        let listed = repo.list_pages().await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "scratchpad");
        assert_eq!(listed[0].title, "速记本");
        assert_eq!(listed[0].content, "legacy content");
    }

    /// 首次读取会创建空白默认页面，且不推进 vector_clock。
    #[tokio::test]
    async fn get_or_create_default_page_creates_empty_page_without_advancing_clock() {
        let repo = setup_repo().await;
        let row = repo.get_or_create_default_page("device-a").await.unwrap();

        assert_eq!(row.id, "scratchpad");
        assert_eq!(row.title, "速记本");
        assert_eq!(row.content, "");
        assert_eq!(row.device_id, "device-a");
        assert!(row.vector_clock.is_empty());
        assert!(!row.deleted);
    }

    /// 创建多个页面后，列表只返回未删除页面，并按 updated_at 降序。
    #[tokio::test]
    async fn create_and_list_pages_excludes_deleted_and_orders_by_updated_at_desc() {
        let repo = setup_repo().await;
        let first = repo
            .create_page("first", "", "device-a", Some("2024-01-01T00:00:00+00:00"))
            .await
            .unwrap();
        let second = repo
            .create_page("second", "", "device-a", Some("2024-01-02T00:00:00+00:00"))
            .await
            .unwrap();
        repo.soft_delete_page(&first.id, "device-a").await.unwrap();

        let listed = repo.list_pages().await.unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, second.id);
        assert_eq!(listed[0].title, "second");
    }

    /// 更新内容会推进本机向量时钟，并保留首次创建时间。
    #[tokio::test]
    async fn update_page_content_increments_current_device_clock_and_preserves_created_at() {
        let repo = setup_repo().await;
        let initial = repo.get_or_create_default_page("device-a").await.unwrap();

        let updated = repo
            .update_page_content(&initial.id, "hello", "device-a")
            .await
            .unwrap();

        assert_eq!(updated.id, "scratchpad");
        assert_eq!(updated.title, "速记本");
        assert_eq!(updated.content, "hello");
        assert_eq!(updated.created_at, initial.created_at);
        assert_eq!(updated.device_id, "device-a");
        assert_eq!(updated.vector_clock.get("device-a"), Some(&1));
        assert!(!updated.deleted);
    }

    /// 重命名页面会推进本机向量时钟，并把空标题归一化为“未命名”。
    #[tokio::test]
    async fn rename_page_increments_clock_and_normalizes_blank_title() {
        let repo = setup_repo().await;
        let initial = repo.get_or_create_default_page("device-a").await.unwrap();

        let renamed = repo
            .rename_page(&initial.id, "   ", "device-a")
            .await
            .unwrap();

        assert_eq!(renamed.title, "未命名");
        assert_eq!(renamed.vector_clock.get("device-a"), Some(&1));
    }

    /// 删除页面是软删除，同步读取仍包含删除记录，且 delete_epoch > 0。
    #[tokio::test]
    async fn soft_delete_page_marks_deleted_and_get_all_for_sync_includes_it() {
        let repo = setup_repo().await;
        let initial = repo.get_or_create_default_page("device-a").await.unwrap();

        let deleted = repo
            .soft_delete_page(&initial.id, "device-a")
            .await
            .unwrap();
        assert!(deleted.delete_epoch > 0);

        let listed = repo.list_pages().await.unwrap();
        let synced = repo.get_all_for_sync().await.unwrap();
        assert!(listed.is_empty());
        assert_eq!(synced.len(), 1);
        assert!(synced[0].deleted);
        assert_eq!(synced[0].vector_clock.get("device-a"), Some(&1));
        assert!(synced[0].delete_epoch > 0);
    }

    /// 同步读取返回全部页面行，便于 cloud/P2P 复用同一实体。
    #[tokio::test]
    async fn get_all_for_sync_returns_all_pages_including_empty_content() {
        let repo = setup_repo().await;
        repo.get_or_create_default_page("device-a").await.unwrap();
        repo.create_page("second", "", "device-a", None)
            .await
            .unwrap();

        let rows = repo.get_all_for_sync().await.unwrap();

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.id == "scratchpad"));
        assert!(rows.iter().any(|row| row.title == "second"));
    }

    /// 中途注入失败时整批回滚，不得留下半批次。
    #[tokio::test]
    async fn bulk_failure_rolls_back() {
        let repo = setup_repo().await;
        let now = "2026-07-14T00:00:00Z".to_string();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 1u64);
        let a = ScratchpadRow {
            id: "a".to_string(),
            title: "A".to_string(),
            content: "ca".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
            device_id: "d1".to_string(),
            vector_clock: vc.clone(),
            deleted: false,
            delete_epoch: 0,
        };
        let b = ScratchpadRow {
            id: "b".to_string(),
            title: "B".to_string(),
            content: "cb".to_string(),
            created_at: now.clone(),
            updated_at: now,
            device_id: "d1".to_string(),
            vector_clock: vc,
            deleted: false,
            delete_epoch: 0,
        };
        repo.bulk_upsert_inject_fail_at(&[a, b], Some(1))
            .await
            .unwrap_err();
        assert!(repo.get_all_for_sync().await.unwrap().is_empty());
    }
}
