//! storage/attention_read_repo.rs — Attention 全局 Inbox 的 per-device 已读标记仓储
//!
//! Business Logic（为什么需要这个模块）:
//!     Attention 全局 Inbox 当前完全不持久化条目（每次请求重算 5 个 source）。
//!     引入"永久已读 + 跨设备同步"后，必须把"本设备对哪些 item_id 标过 read"
//!     落到 SQLite，作为聚合阶段派生 `read_at` 与 `unread_*` 计数的依据。
//!     已读是 monotonic add-only per-device 状态，不走 CRDT（不是内容，无并发编辑）；
//!     撤销 = 删除当前 device_id 行即可，跨设备传播经 sync push-batch v2 通道。
//!
//! Code Logic（这个模块做什么）:
//!     - 幂等建表 `attention_read_by_device`（PRIMARY KEY(item_id, device_id)）；
//!     - 提供 `load_read_ids(device_id)` 聚合阶段查询；
//!     - `mark_read_on_tx` / `mark_unread_on_tx` 在调用方事务内成批写入或删除；
//!     - 单测覆盖：插入/查询/重复 INSERT OR IGNORE/删除/批量/跨设备隔离。

use crate::error::AppError;
use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
use sqlx::sqlite::SqlitePool;
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;
use std::sync::Arc;

/// Attention 已读状态仓储。
///
/// Business Logic: 持有 SQLite pool + maintenance gate；
///     写入必须在调用方事务内完成（与 sync_request_ledger、prompt_repo 等
///     共享同一份 maintenance_gate，避免与 restore 等独占写互不兼容）。
/// Code Logic: 字段 `db: SqlitePool` + `gate: Arc<DatabaseMaintenanceGate>`。
pub struct AttentionReadRepo {
    db: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl AttentionReadRepo {
    /// 测试 / fixture 用：独立 gate。
    #[allow(dead_code)] // intentional public API for tests
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：与 AppState.maintenance_gate 共享同一闸。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 暴露内部 pool（路由/单测拼事务时用）。
    #[allow(dead_code)] // intentional public API for tx assembly / tests
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 暴露共享 gate（供 push-batch ledger 与域仓储对齐屏障）。
    ///
    /// Business Logic: ledger 必须与 attention_read 写路径共用同一 gate，否则 restore 独占可被旁路。
    /// Code Logic: clone Arc。
    pub fn gate(&self) -> Arc<DatabaseMaintenanceGate> {
        self.gate.clone()
    }

    /// 幂等建表。
    ///
    /// Business Logic: 旧库升级必须无 sqlx::migrate! 框架即可获得本表。
    /// Code Logic: `CREATE TABLE IF NOT EXISTS` + UNIQUE 主键。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS attention_read_by_device (
                item_id   TEXT NOT NULL,
                device_id TEXT NOT NULL,
                read_at   TEXT NOT NULL,
                PRIMARY KEY (item_id, device_id)
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_attention_read_device
                ON attention_read_by_device(device_id, read_at DESC)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 加载本设备视角的所有 (item_id -> read_at) 映射。
    ///
    /// Business Logic: 聚合阶段调用；返回本设备已读集合。
    ///     跨设备隔离由 `WHERE device_id = ?` 保证（PRIMARY KEY 虽含 device_id 但需要索引覆盖）。
    /// Code Logic: 单 SELECT，row.item_id / row.read_at 收集到 HashMap。
    pub async fn load_read_ids(
        &self,
        device_id: &str,
    ) -> Result<HashMap<String, String>, AppError> {
        let rows = sqlx::query(
            "SELECT item_id, read_at FROM attention_read_by_device WHERE device_id = ?",
        )
        .bind(device_id)
        .fetch_all(&self.db)
        .await?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let item_id: String = row.try_get("item_id")?;
            let read_at: String = row.try_get("read_at")?;
            map.insert(item_id, read_at);
        }
        Ok(map)
    }

    /// 在调用方事务内把指定 item_ids 标记为本设备已读。
    ///
    /// Business Logic: mark-read 命令、push-batch apply 都走这里；item_id 列表由调用方提供
    ///     （命令侧从聚合快照拿；push-batch 侧从请求体拿）。同 item_id 重复 mark 视为 no-op
    ///     （INSERT OR IGNORE），不更新 read_at（避免破坏 monotonic 语义；如需"刷新 read_at"
    ///     应走 DELETE + INSERT，本接口不提供）。
    /// Code Logic: 单条 INSERT OR IGNORE 循环；返回实际新写入的行数。
    pub async fn mark_read_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        device_id: &str,
        item_ids: &[String],
        read_at: &str,
    ) -> Result<usize, AppError> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let mut written = 0usize;
        for item_id in item_ids {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO attention_read_by_device (item_id, device_id, read_at)
                 VALUES (?, ?, ?)",
            )
            .bind(item_id)
            .bind(device_id)
            .bind(read_at)
            .execute(&mut **tx)
            .await?;
            written += result.rows_affected() as usize;
        }
        Ok(written)
    }

    /// 在调用方事务内撤销指定 item_ids 的本设备已读标记。
    ///
    /// Business Logic: "标为未读"命令；只删本设备行，不影响其它设备。
    /// Code Logic: 单 DELETE WHERE device_id = ? AND item_id IN (...);
    ///     SQLite 单条 IN 列表用占位符展开，调用方传入的 item_ids 应去重。
    pub async fn mark_unread_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        device_id: &str,
        item_ids: &[String],
    ) -> Result<usize, AppError> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        // 去重避免冗余参数（不改变结果但减少 SQL 体积）
        let mut seen = std::collections::HashSet::new();
        let mut placeholders = String::new();
        let mut binds: Vec<&str> = Vec::with_capacity(item_ids.len() + 1);
        binds.push(device_id);
        for id in item_ids {
            if seen.insert(id.clone()) {
                if !placeholders.is_empty() {
                    placeholders.push(',');
                }
                placeholders.push('?');
                binds.push(id.as_str());
            }
        }
        let sql = format!(
            "DELETE FROM attention_read_by_device
             WHERE device_id = ? AND item_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql).bind(device_id);
        for b in &binds[1..] {
            q = q.bind(*b);
        }
        let result = q.execute(&mut **tx).await?;
        Ok(result.rows_affected() as usize)
    }
}

#[cfg(test)]
mod tests {
    //! attention_read_repo 单测：建表、加载、INSERT OR IGNORE 幂等、删除、跨设备隔离。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> AttentionReadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AttentionReadRepo::ensure_schema(&pool).await.unwrap();
        AttentionReadRepo::new(pool)
    }

    #[tokio::test]
    async fn ensure_schema_is_idempotent() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AttentionReadRepo::ensure_schema(&pool).await.unwrap();
        AttentionReadRepo::ensure_schema(&pool).await.unwrap(); // 第二次不得失败
    }

    #[tokio::test]
    async fn load_read_ids_returns_empty_for_unknown_device() {
        let repo = setup_repo().await;
        let map = repo.load_read_ids("device-x").await.unwrap();
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn mark_read_inserts_and_load_returns_map() {
        let repo = setup_repo().await;
        let mut tx = repo.pool().begin().await.unwrap();
        let written = AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-a",
            &["item-1".to_string(), "item-2".to_string()],
            "2026-08-16T10:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(written, 2);

        let map = repo.load_read_ids("device-a").await.unwrap();
        assert_eq!(map.get("item-1"), Some(&"2026-08-16T10:00:00Z".to_string()));
        assert_eq!(map.get("item-2"), Some(&"2026-08-16T10:00:00Z".to_string()));
    }

    #[tokio::test]
    async fn mark_read_is_noop_on_duplicate() {
        let repo = setup_repo().await;
        let mut tx = repo.pool().begin().await.unwrap();
        AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-a",
            &["item-1".to_string()],
            "2026-08-16T10:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // 第二次 mark 不应修改 read_at，也不再算新写入
        let mut tx = repo.pool().begin().await.unwrap();
        let written = AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-a",
            &["item-1".to_string()],
            "2026-08-16T11:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(written, 0, "重复 mark-read 不得返回新写入");

        let map = repo.load_read_ids("device-a").await.unwrap();
        assert_eq!(
            map.get("item-1"),
            Some(&"2026-08-16T10:00:00Z".to_string()),
            "read_at 必须保留首次值，不被后续 mark 覆盖"
        );
    }

    #[tokio::test]
    async fn mark_unread_deletes_specified_ids_only() {
        let repo = setup_repo().await;
        let mut tx = repo.pool().begin().await.unwrap();
        AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-a",
            &[
                "item-1".to_string(),
                "item-2".to_string(),
                "item-3".to_string(),
            ],
            "2026-08-16T10:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        let removed = AttentionReadRepo::mark_unread_on_tx(
            &mut tx,
            "device-a",
            &["item-1".to_string(), "item-99".to_string()],
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(removed, 1, "只删除存在的本设备行");

        let map = repo.load_read_ids("device-a").await.unwrap();
        assert!(!map.contains_key("item-1"));
        assert!(map.contains_key("item-2"));
        assert!(map.contains_key("item-3"));
    }

    #[tokio::test]
    async fn read_state_is_isolated_per_device() {
        let repo = setup_repo().await;
        let mut tx = repo.pool().begin().await.unwrap();
        AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-a",
            &["item-1".to_string()],
            "2026-08-16T10:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut tx = repo.pool().begin().await.unwrap();
        AttentionReadRepo::mark_read_on_tx(
            &mut tx,
            "device-b",
            &["item-1".to_string()],
            "2026-08-16T10:00:00Z",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let map_a = repo.load_read_ids("device-a").await.unwrap();
        let map_b = repo.load_read_ids("device-b").await.unwrap();
        assert!(map_a.contains_key("item-1"));
        assert!(map_b.contains_key("item-1"));

        // device-a 撤销不影响 device-b
        let mut tx = repo.pool().begin().await.unwrap();
        AttentionReadRepo::mark_unread_on_tx(&mut tx, "device-a", &["item-1".to_string()])
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let map_a = repo.load_read_ids("device-a").await.unwrap();
        let map_b = repo.load_read_ids("device-b").await.unwrap();
        assert!(!map_a.contains_key("item-1"));
        assert!(map_b.contains_key("item-1"), "跨设备隔离必须保留");
    }

    #[tokio::test]
    async fn empty_item_ids_is_noop() {
        let repo = setup_repo().await;
        let mut tx = repo.pool().begin().await.unwrap();
        let written =
            AttentionReadRepo::mark_read_on_tx(&mut tx, "device-a", &[], "2026-08-16T10:00:00Z")
                .await
                .unwrap();
        let removed = AttentionReadRepo::mark_unread_on_tx(&mut tx, "device-a", &[])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(written, 0);
        assert_eq!(removed, 0);
    }
}
