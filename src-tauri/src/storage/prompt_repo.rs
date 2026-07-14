//! storage/prompt_repo.rs — Prompt 数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt 管理需要创建、修改、软删除、搜索、按标签筛选、列出标签等功能，
//!     同步引擎还需批量 upsert 和同步摘要。此模块对照 Python `prompt_repo.py`，
//!     逐方法实现等价逻辑，保证数据行为与旧版一致。
//!
//! Code Logic（这个模块做什么）:
//!     持有 `SqlitePool`，用运行期 `sqlx::query` 执行 SQL。
//!     JSON 字段（tags, vector_clock）用 serde_json 序列化为紧凑 JSON 读写，与 Python 互通。
//!     datetime 字段以 String 透传（兼容有无时区格式）。
//!     delete 为软删除（deleted=1），并同时推进 vector_clock 与 updated_at（修正 Python
//!     handler 自增 clock 却未落库的 bug）。

use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::storage::maintenance_gate::{
    begin_shared_write, with_shared_write_lease, DatabaseMaintenanceGate,
};
use crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo;
use crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS;
use sqlx::sqlite::SqlitePool;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;
use std::sync::Arc;

/// Prompt 仓库，封装所有 prompts 表的数据库操作。
pub struct PromptRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
    /// 与 restore exclusive 共享的写屏障。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl PromptRepo {
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

    /// 暴露共享 gate（供 apply_merge 等同路径 ledger 对齐屏障）。
    ///
    /// Business Logic: push-batch ledger 必须与域 repo 使用同一 gate，否则 restore 独占可被旁路。
    /// Code Logic: clone Arc。
    pub fn gate(&self) -> Arc<DatabaseMaintenanceGate> {
        self.gate.clone()
    }

    /// 将数据库一行映射为 PromptRow（JSON 字段反序列化、deleted int→bool）。
    ///
    /// Business Logic: 同步与列表都需要从 SQLite 行还原完整 Prompt 实体。
    /// Code Logic: tags/vector_clock 反序列化；delete_epoch 缺列或读失败时默认 0。
    fn row_to_prompt(row: &sqlx::sqlite::SqliteRow) -> Result<PromptRow, AppError> {
        let tags_text: String = row.try_get("tags")?;
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let tags: Vec<String> = serde_json::from_str(&tags_text)?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        let delete_epoch = row
            .try_get::<i64, _>("delete_epoch")
            .unwrap_or(0)
            .max(0) as u64;
        Ok(PromptRow {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            content: row.try_get("content")?,
            tags,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            deleted: deleted_int != 0,
            delete_epoch,
        })
    }

    /// 列表查询：可选关键词搜索 / 单标签筛选，默认排除已删除，按 updated_at 降序。
    ///
    /// Business Logic: 前端列表页传 search 或 tag；无参数则返回全部未删除 Prompt。
    /// Code Logic: 对照 prompt_repo.py 的 get_all / search / filter_by_tags 三条路径分支。
    pub async fn list(
        &self,
        search: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<PromptRow>, AppError> {
        // 三种查询分支，分别对应 Python get_all / search / filter_by_tags
        if let Some(kw) = search {
            // search: title/content LIKE '%kw%'，排除已删除，updated_at DESC
            let pattern = format!("%{}%", kw);
            let rows = sqlx::query(
                "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
                 FROM prompts WHERE deleted = 0 AND (title LIKE ? OR content LIKE ?) ORDER BY updated_at DESC",
            )
            .bind(&pattern)
            .bind(&pattern)
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        } else if let Some(t) = tag {
            // tag: json_each 展开 tags，与给定标签交集匹配，DISTINCT 去重
            let rows = sqlx::query(
                "SELECT DISTINCT p.id, p.title, p.content, p.tags, p.created_at, p.updated_at, p.device_id, p.vector_clock, p.deleted, p.delete_epoch \
                 FROM prompts p, json_each(p.tags) AS t \
                 WHERE p.deleted = 0 AND t.value = ? ORDER BY p.updated_at DESC",
            )
            .bind(t)
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        } else {
            // 无参数：全部未删除，updated_at DESC
            let rows = sqlx::query(
                "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
                 FROM prompts WHERE deleted = 0 ORDER BY updated_at DESC",
            )
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        }
    }

    /// 按主键查询单条 Prompt（含已删除记录，与 Python get_by_id 一致）。
    pub async fn get(&self, id: &str) -> Result<Option<PromptRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM prompts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_prompt(&r)?)),
            None => Ok(None),
        }
    }

    /// 插入新 Prompt（tags/vector_clock 序列化为 JSON）。
    ///
    /// Business Logic: 新建 Prompt 必须在 maintenance shared lease 下写入，避免 restore 中途被覆盖。
    /// Code Logic: 序列化 JSON 后经 `with_shared_write_lease` 执行 INSERT。
    pub async fn create(&self, p: &PromptRow) -> Result<(), AppError> {
        let tags_text = serde_json::to_string(&p.tags)?;
        let vc_text = serde_json::to_string(&p.vector_clock)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO prompts (id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id)
            .bind(&p.title)
            .bind(&p.content)
            .bind(tags_text)
            .bind(&p.created_at)
            .bind(&p.updated_at)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(p.deleted as i64)
            .bind(p.delete_epoch as i64)
            .execute(&self.db)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }

    /// 全字段更新一条 Prompt（含 vector_clock / deleted / delete_epoch）。
    ///
    /// Business Logic: 更新必须经 shared lease，与 restore exclusive 互斥。
    /// Code Logic: 序列化 JSON 后经 `with_shared_write_lease` 执行 UPDATE。
    pub async fn update(&self, p: &PromptRow) -> Result<(), AppError> {
        let tags_text = serde_json::to_string(&p.tags)?;
        let vc_text = serde_json::to_string(&p.vector_clock)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE prompts SET title = ?, content = ?, tags = ?, updated_at = ?, device_id = ?, vector_clock = ?, deleted = ?, delete_epoch = ? WHERE id = ?",
            )
            .bind(&p.title)
            .bind(&p.content)
            .bind(tags_text)
            .bind(&p.updated_at)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(p.deleted as i64)
            .bind(p.delete_epoch as i64)
            .bind(&p.id)
            .execute(&self.db)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }

    /// 软删除：同事务铸造 delete_epoch 并写入 tombstone 字段。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CRDT 删除是一次写入，需推进本端 vector_clock 使对端感知；同时铸造单调
    ///     delete_epoch，供 peer 水位 ack 与安全 GC。epoch 与 tombstone 必须同事务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     begin_shared_write → mint_on_tx(DOMAIN_PROMPTS) → UPDATE deleted/updated_at/vector_clock/delete_epoch → commit。
    pub async fn soft_delete(
        &self,
        id: &str,
        now: &str,
        vector_clock: &HashMap<String, u64>,
    ) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(vector_clock)?;
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let epoch = SyncDeleteSequenceRepo::mint_on_tx(&mut tx, DOMAIN_PROMPTS).await?;
        sqlx::query(
            "UPDATE prompts SET deleted = 1, updated_at = ?, vector_clock = ?, delete_epoch = ? WHERE id = ?",
        )
        .bind(now)
        .bind(vc_text)
        .bind(epoch as i64)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        drop(permit);
        Ok(())
    }

    /// 返回所有 Prompt（含 deleted 软删除记录），用于跨设备同步。
    ///
    /// Business Logic: 同步必须传播删除事件，因此需读取含 deleted=1 的全部记录。
    ///     对照 Python `get_sync_summary` 的"含 deleted"语义，但本方法返回完整 PromptRow
    ///     （engine 既要 summary 也要完整数据，统一从此取，内存中再投影为 summary）。
    ///
    /// Code Logic: SELECT 全字段（无 deleted 过滤），不排序（同步用，顺序无关）。
    pub async fn get_all_for_sync(&self) -> Result<Vec<PromptRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch \
             FROM prompts",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_prompt).collect()
    }

    /// 批量插入/更新（按 id 主键），用于同步 push 落库。
    ///
    /// Business Logic: 同步引擎从对端拉取多条 Prompt 后需批量写入本地，已存在则覆盖。
    ///     对照 Python `bulk_upsert`。upsert 前不做合并决策（合并由 engine/merger 在调用前
    ///     决定），此处直接 INSERT OR REPLACE。整批必须原子：中途失败不得留下半批次。
    ///
    /// Code Logic: 空切片直接返回；否则显式 begin 事务后逐条 INSERT OR REPLACE，
    ///     全部成功 commit；任一行失败则 tx drop 回滚。
    pub async fn bulk_upsert(&self, prompts: &[PromptRow]) -> Result<(), AppError> {
        self.bulk_upsert_in_transaction(prompts, None).await
    }

    /// 事务性 bulk_upsert；可选注入失败点，供模块/quality 回归验证整批回滚。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试必须走与生产相同的事务边界验证 rollback，而不是另一套写路径。
    ///     本 seam 仅 debug/test-only：`cfg(test)` 或 `debug_assertions` 下编译，release 剥离。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `bulk_upsert_in_transaction(prompts, inject_fail_at)`。
    #[cfg(any(test, debug_assertions))]
    pub async fn bulk_upsert_inject_fail_at(
        &self,
        prompts: &[PromptRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        self.bulk_upsert_in_transaction(prompts, inject_fail_at)
            .await
    }

    /// 在自有事务中 bulk upsert（生产与 inject seam 共享）。
    ///
    /// Business Logic: 保证生产 `bulk_upsert` 与测试 inject 路径同一事务语义，并经 shared lease。
    /// Code Logic: begin_shared_write → `bulk_upsert_on_tx` → commit。
    async fn bulk_upsert_in_transaction(
        &self,
        prompts: &[PromptRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        if prompts.is_empty() {
            return Ok(());
        }
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        Self::bulk_upsert_on_tx(&mut tx, prompts, inject_fail_at).await?;
        tx.commit().await?;
        drop(permit);
        Ok(())
    }

    /// 在已开启事务上执行 bulk upsert 循环（供 push-batch ledger 同事务调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     v2 push-batch 要求 ledger claim 与领域写入同一事务；路由层持有 tx 时调用本方法，
    ///     不得再 begin 嵌套事务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐条 INSERT OR REPLACE；当 `inject_fail_at == Some(idx)` 命中时返回 Err（不 commit）。
    pub async fn bulk_upsert_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        prompts: &[PromptRow],
        inject_fail_at: Option<usize>,
    ) -> Result<(), AppError> {
        for (idx, p) in prompts.iter().enumerate() {
            if inject_fail_at == Some(idx) {
                return Err(AppError::generic("injected prompt bulk_upsert failure"));
            }
            let tags_text = serde_json::to_string(&p.tags)?;
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO prompts \
                 (id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted, delete_epoch) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id)
            .bind(&p.title)
            .bind(&p.content)
            .bind(tags_text)
            .bind(&p.created_at)
            .bind(&p.updated_at)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(p.deleted as i64)
            .bind(p.delete_epoch as i64)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// 返回底层 pool（供 push-batch ledger 与本仓库共享同一连接池）。
    ///
    /// Business Logic: 路由层需要同一 pool 开事务，把 ledger 与 bulk upsert 绑在一起。
    /// Code Logic: 返回 `SqlitePool` clone。
    pub fn pool(&self) -> SqlitePool {
        self.db.clone()
    }

    /// 列出所有未删除 Prompt 用过的去重标签（升序）。
    ///
    /// Business Logic: 前端标签筛选栏需动态展示可选标签。
    /// Code Logic: 对照 prompt_repo.py get_all_tags，用 json_each 展开后 DISTINCT。
    pub async fn list_tags(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT DISTINCT t.value AS tag FROM prompts p, json_each(p.tags) AS t \
             WHERE p.deleted = 0 ORDER BY t.value",
        )
        .fetch_all(&self.db)
        .await?;
        let mut tags = Vec::with_capacity(rows.len());
        for r in &rows {
            let tag: String = r.try_get("tag")?;
            tags.push(tag);
        }
        Ok(tags)
    }
}

#[cfg(test)]
mod tests {
    //! prompt_repo 单测：事务 bulk_upsert 中途失败必须整批回滚。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存 SQLite 并建好 prompts 表，返回仓库。
    async fn setup_repo() -> PromptRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompts (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, \
             tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
             device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
             delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        PromptRepo::new(pool)
    }

    /// 构造测试 PromptRow。
    fn prompt(id: &str) -> PromptRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("d1".to_string(), 1u64);
        PromptRow {
            id: id.to_string(),
            title: format!("t-{id}"),
            content: format!("c-{id}"),
            tags: vec![],
            created_at: "2026-07-14T00:00:00Z".to_string(),
            updated_at: "2026-07-14T00:00:00Z".to_string(),
            device_id: "d1".to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: 0,
        }
    }

    /// soft_delete 铸造 delete_epoch > 0。
    #[tokio::test]
    async fn soft_delete_mints_delete_epoch() {
        let repo = setup_repo().await;
        repo.create(&prompt("p1")).await.unwrap();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 2u64);
        repo.soft_delete("p1", "2026-07-14T01:00:00Z", &vc)
            .await
            .unwrap();
        let got = repo.get("p1").await.unwrap().unwrap();
        assert!(got.deleted);
        assert!(got.delete_epoch > 0);
        assert_eq!(got.vector_clock.get("d1"), Some(&2));
    }

    /// 中途注入失败时整批回滚，不得留下半批次。
    #[tokio::test]
    async fn bulk_failure_rolls_back() {
        let repo = setup_repo().await;
        repo.bulk_upsert_inject_fail_at(&[prompt("a"), prompt("b")], Some(1))
            .await
            .unwrap_err();
        assert!(repo.get_all_for_sync().await.unwrap().is_empty());
    }

    /// 生产 bulk_upsert 成功路径写入全部条目。
    #[tokio::test]
    async fn bulk_upsert_commits_all_items() {
        let repo = setup_repo().await;
        repo.bulk_upsert(&[prompt("a"), prompt("b")]).await.unwrap();
        let all = repo.get_all_for_sync().await.unwrap();
        assert_eq!(all.len(), 2);
    }
}
