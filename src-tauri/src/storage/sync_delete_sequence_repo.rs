//! storage/sync_delete_sequence_repo.rs — 每 domain 单调 deleteEpoch 序列
//!
//! Business Logic（为什么需要这个模块）:
//!     本地删除或首次接纳远端 tombstone 时，需要在同一事务中铸造单调递增的
//!     `delete_epoch` 写入 tombstone/manifest，供 peer 水位 ack 与安全 GC。
//!
//! Code Logic（这个模块做什么）:
//!     表 `sync_domain_delete_sequences(domain PK, next_epoch)`；
//!     `mint_on_tx` 在事务内读取 next_epoch、写回 +1、返回刚铸造的 epoch。

use crate::error::AppError;
use crate::storage::maintenance_gate::{begin_shared_write, DatabaseMaintenanceGate};
use sqlx::sqlite::SqlitePool;
use sqlx::{Sqlite, Transaction};
use std::sync::Arc;

/// Domain 删除序号仓库。
pub struct SyncDeleteSequenceRepo {
    /// SQLite 连接池
    #[allow(dead_code)] // intentional API; mint_on_tx is primary production path
    db: SqlitePool,
    /// 维护闸：独立 mint 事务经 shared lease
    #[allow(dead_code)] // intentional API for independent mint()
    gate: Arc<DatabaseMaintenanceGate>,
}

impl SyncDeleteSequenceRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: soft_delete / merge adopt-delete 路径需要铸造 epoch。
    /// Code Logic: 委托 `with_gate` + 新建独立 gate。
    #[allow(dead_code)] // intentional public API / tests
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: epoch mint 写必须与 restore 互斥，与其它 writer 共享 gate。
    /// Code Logic: 持有 pool clone + `Arc<DatabaseMaintenanceGate>`。
    #[allow(dead_code)] // intentional public API for AppState wiring
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 返回内部 pool。
    ///
    /// Business Logic: 测试与事务组装。
    /// Code Logic: `&SqlitePool`。
    #[allow(dead_code)] // intentional public API for tx assembly / tests
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 幂等创建 `sync_domain_delete_sequences` 表。
    ///
    /// Business Logic: 旧库升级即可获得 per-domain 序号。
    /// Code Logic: CREATE TABLE IF NOT EXISTS，next_epoch 默认 1。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sync_domain_delete_sequences (
                domain TEXT PRIMARY KEY,
                next_epoch INTEGER NOT NULL DEFAULT 1
            )",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 在事务内铸造下一个 delete epoch（原子 next_epoch++）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     删除与 epoch 必须同一事务，避免崩溃导致 tombstone 无 epoch 或 epoch 空洞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若 domain 无行则插入 next_epoch=2 并返回 1；
    ///     否则 SELECT next_epoch → UPDATE next_epoch+1 → 返回旧值。
    pub async fn mint_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        domain: &str,
    ) -> Result<u64, AppError> {
        let existing: Option<i64> = sqlx::query_scalar(
            "SELECT next_epoch FROM sync_domain_delete_sequences WHERE domain = ?",
        )
        .bind(domain)
        .fetch_optional(&mut **tx)
        .await?;

        match existing {
            Some(next) => {
                let minted = next.max(1) as u64;
                sqlx::query(
                    "UPDATE sync_domain_delete_sequences SET next_epoch = ? WHERE domain = ?",
                )
                .bind((minted as i64) + 1)
                .bind(domain)
                .execute(&mut **tx)
                .await?;
                Ok(minted)
            }
            None => {
                // 首次：返回 1，写 next=2
                sqlx::query(
                    "INSERT INTO sync_domain_delete_sequences (domain, next_epoch) VALUES (?, 2)",
                )
                .bind(domain)
                .execute(&mut **tx)
                .await?;
                Ok(1)
            }
        }
    }

    /// 便捷：在独立事务中铸造（测试/非合并路径）。
    ///
    /// Business Logic: 单测与不需与其它写操作同事务时使用。
    /// Code Logic: `begin_shared_write` → mint_on_tx → commit。
    #[allow(dead_code)] // intentional public API for non-tx mint path
    pub async fn mint(&self, domain: &str) -> Result<u64, AppError> {
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let epoch = Self::mint_on_tx(&mut tx, domain).await?;
        tx.commit().await?;
        drop(permit);
        Ok(epoch)
    }
}

#[cfg(test)]
mod tests {
    //! sync_delete_sequence 单测：单调递增与顺序铸造。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> SyncDeleteSequenceRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        SyncDeleteSequenceRepo::new(pool)
    }

    #[tokio::test]
    async fn sync_delete_sequence_is_monotonic() {
        let repo = setup_repo().await;
        let a = repo.mint("prompts").await.unwrap();
        let b = repo.mint("prompts").await.unwrap();
        let c = repo.mint("prompts").await.unwrap();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        assert!(a < b && b < c);
    }

    #[tokio::test]
    async fn sync_delete_sequence_domains_are_independent() {
        let repo = setup_repo().await;
        let p = repo.mint("prompts").await.unwrap();
        let s = repo.mint("scratchpad").await.unwrap();
        assert_eq!(p, 1);
        assert_eq!(s, 1);
    }

    /// 同事务内多次铸造仍顺序递增（模拟并发-ish 顺序写）。
    #[tokio::test]
    async fn sync_delete_sequence_sequential_in_one_tx() {
        let repo = setup_repo().await;
        let (permit, mut tx) = begin_shared_write(repo.pool(), &repo.gate).await.unwrap();
        let e1 = SyncDeleteSequenceRepo::mint_on_tx(&mut tx, "prompts")
            .await
            .unwrap();
        let e2 = SyncDeleteSequenceRepo::mint_on_tx(&mut tx, "prompts")
            .await
            .unwrap();
        let e3 = SyncDeleteSequenceRepo::mint_on_tx(&mut tx, "prompts")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        drop(permit);
        assert_eq!((e1, e2, e3), (1, 2, 3));
    }
}
