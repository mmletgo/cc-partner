//! storage/sync_watermark_repo.rs — 同步 peer/domain 删除 epoch 水位
//!
//! Business Logic（为什么需要这个模块）:
//!     Tombstone GC 必须确认所有活跃 peer 已经看到并应用了某次删除，否则压缩后
//!     长期离线 peer 可能用旧 live 行复活删除项。水位按 peer+domain 记录
//!     `acked_delete_epoch` 与 `last_seen_at`。
//!
//! Code Logic（这个模块做什么）:
//!     幂等建表 `sync_peer_watermarks`；touch_seen / advance_ack（只升不降）；
//!     list_active_peers 过滤最近 active_window_days（默认 90）内见过的 peer。

use crate::error::AppError;
use chrono::{DateTime, Duration, Utc};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};

/// 默认活跃 peer 窗口（天）。
pub const DEFAULT_ACTIVE_PEER_WINDOW_DAYS: i64 = 90;

/// 单个 peer 在某 domain 上的删除水位。
///
/// Business Logic: GC 需要知道 peer 已确认到哪个 delete epoch，以及是否仍活跃。
/// Code Logic: 对应 `sync_peer_watermarks` 主键 (peer_device_id, domain)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPeerWatermark {
    /// 对端设备 id（收敛标签，非认证）
    pub peer_device_id: String,
    /// 领域 token
    pub domain: String,
    /// 该 peer 已完整应用并确认的最高连续 delete epoch
    pub acked_delete_epoch: u64,
    /// 最近一次见过该 peer 的时间 RFC3339
    pub last_seen_at: String,
}

/// Peer 水位仓库。
pub struct SyncWatermarkRepo {
    /// SQLite 连接池
    db: SqlitePool,
}

impl SyncWatermarkRepo {
    /// 构造仓库。
    ///
    /// Business Logic: GC 与 manifest ack 路径共用。
    /// Code Logic: 持有 pool clone。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 返回内部 pool。
    ///
    /// Business Logic: 测试与事务组装需要同一 pool。
    /// Code Logic: `&SqlitePool`。
    #[allow(dead_code)] // 引擎 ack 接线前供事务组装
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 幂等创建 `sync_peer_watermarks` 表。
    ///
    /// Business Logic: 旧库升级无迁移框架也必须具备 watermark 表。
    /// Code Logic: CREATE TABLE IF NOT EXISTS。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sync_peer_watermarks (
                peer_device_id TEXT NOT NULL,
                domain TEXT NOT NULL,
                acked_delete_epoch INTEGER NOT NULL DEFAULT 0,
                last_seen_at TEXT NOT NULL,
                PRIMARY KEY (peer_device_id, domain)
            )",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 标记 peer 在 domain 上被见到（刷新 last_seen；不降 ack）。
    ///
    /// Business Logic: 健康检查/manifest 交互时更新活跃性，避免离线 peer 被误判仍活跃。
    /// Code Logic: UPSERT；已存在则只更新 last_seen_at，保留 acked_delete_epoch。
    pub async fn touch_seen(
        &self,
        peer_device_id: &str,
        domain: &str,
        now: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO sync_peer_watermarks (peer_device_id, domain, acked_delete_epoch, last_seen_at)
             VALUES (?, ?, 0, ?)
             ON CONFLICT(peer_device_id, domain) DO UPDATE SET
               last_seen_at = excluded.last_seen_at",
        )
        .bind(peer_device_id)
        .bind(domain)
        .bind(now)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 推进 peer 的 acked_delete_epoch（只升不降）并刷新 last_seen。
    ///
    /// Business Logic: 只有完整 manifest + delete/floor + batch 成功后才可提升水位。
    /// Code Logic: 委托 `advance_ack_on_tx` 在独立事务中执行。
    pub async fn advance_ack(
        &self,
        peer_device_id: &str,
        domain: &str,
        acked_delete_epoch: u64,
        now: &str,
    ) -> Result<(), AppError> {
        let mut tx = self.db.begin().await?;
        Self::advance_ack_on_tx(
            &mut tx,
            peer_device_id,
            domain,
            acked_delete_epoch,
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 在已开启事务内推进 peer 水位（只升不降）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     push-batch 的 watermark ack 必须与 active/conflict/ledger 同事务提交，
    ///     否则失败注入或中途错误会造成“已 ack 但未落库”的不一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPSERT；`acked_delete_epoch = MAX(old, new)`；刷新 `last_seen_at`。
    pub async fn advance_ack_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        peer_device_id: &str,
        domain: &str,
        acked_delete_epoch: u64,
        now: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO sync_peer_watermarks (peer_device_id, domain, acked_delete_epoch, last_seen_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(peer_device_id, domain) DO UPDATE SET
               acked_delete_epoch = MAX(sync_peer_watermarks.acked_delete_epoch, excluded.acked_delete_epoch),
               last_seen_at = excluded.last_seen_at",
        )
        .bind(peer_device_id)
        .bind(domain)
        .bind(acked_delete_epoch as i64)
        .bind(now)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 读取单个 watermark（测试/调试）。
    ///
    /// Business Logic: GC 决策与单测需要查看 peer 水位。
    /// Code Logic: SELECT by PK。
    pub async fn get(
        &self,
        peer_device_id: &str,
        domain: &str,
    ) -> Result<Option<SyncPeerWatermark>, AppError> {
        let row = sqlx::query(
            "SELECT peer_device_id, domain, acked_delete_epoch, last_seen_at
             FROM sync_peer_watermarks
             WHERE peer_device_id = ? AND domain = ?",
        )
        .bind(peer_device_id)
        .bind(domain)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_from_sqlite(&r)?)),
            None => Ok(None),
        }
    }

    /// 列出 domain 上最近 active_window_days 内见过的活跃 peer。
    ///
    /// Business Logic: GC 只要求活跃 peer 的 ack；离线超过窗口的 peer 不阻塞压缩，
    ///     但其回归时靠 deletion floor 拒绝复活。
    /// Code Logic: last_seen_at >= now - window；默认 90 天。
    pub async fn list_active_peers(
        &self,
        domain: &str,
        now: &str,
        active_window_days: i64,
    ) -> Result<Vec<SyncPeerWatermark>, AppError> {
        let now_dt = parse_rfc3339(now)?;
        let cutoff = now_dt - Duration::days(active_window_days);
        let cutoff_s = cutoff.to_rfc3339();
        let rows = sqlx::query(
            "SELECT peer_device_id, domain, acked_delete_epoch, last_seen_at
             FROM sync_peer_watermarks
             WHERE domain = ? AND last_seen_at >= ?
             ORDER BY peer_device_id ASC",
        )
        .bind(domain)
        .bind(cutoff_s)
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_from_sqlite).collect()
    }

    /// 判断 tombstone 是否可被活跃 peer 水位安全 GC。
    ///
    /// Business Logic: 任一活跃 peer 的 ack < tombstone_epoch 则阻塞压缩。
    /// Code Logic: list_active_peers 后检查 min(acked) >= epoch；无活跃 peer 时视为可 GC。
    pub async fn all_active_peers_acked(
        &self,
        domain: &str,
        tombstone_epoch: u64,
        now: &str,
        active_window_days: i64,
    ) -> Result<bool, AppError> {
        let peers = self
            .list_active_peers(domain, now, active_window_days)
            .await?;
        Ok(peers
            .iter()
            .all(|p| p.acked_delete_epoch >= tombstone_epoch))
    }

    /// SQLite 行映射。
    fn row_from_sqlite(row: &SqliteRow) -> Result<SyncPeerWatermark, AppError> {
        let epoch: i64 = row.try_get("acked_delete_epoch")?;
        Ok(SyncPeerWatermark {
            peer_device_id: row.try_get("peer_device_id")?,
            domain: row.try_get("domain")?,
            acked_delete_epoch: epoch.max(0) as u64,
            last_seen_at: row.try_get("last_seen_at")?,
        })
    }
}

/// 解析 RFC3339。
fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| AppError::generic(format!("非法时间戳 {s}: {e}")))
}

#[cfg(test)]
mod tests {
    //! sync_watermark 单测：touch/advance 单调、活跃 peer 过滤、低水位阻塞 GC。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> SyncWatermarkRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        SyncWatermarkRepo::new(pool)
    }

    #[tokio::test]
    async fn sync_watermark_advance_only_increases() {
        let repo = setup_repo().await;
        let now = "2024-06-01T00:00:00+00:00";
        repo.advance_ack("peer-a", "prompts", 5, now)
            .await
            .unwrap();
        repo.advance_ack("peer-a", "prompts", 3, now)
            .await
            .unwrap();
        let row = repo.get("peer-a", "prompts").await.unwrap().unwrap();
        assert_eq!(row.acked_delete_epoch, 5);
    }

    #[tokio::test]
    async fn sync_watermark_list_active_excludes_stale() {
        let repo = setup_repo().await;
        repo.touch_seen("fresh", "prompts", "2024-06-01T00:00:00+00:00")
            .await
            .unwrap();
        repo.touch_seen("stale", "prompts", "2024-01-01T00:00:00+00:00")
            .await
            .unwrap();
        let active = repo
            .list_active_peers("prompts", "2024-06-15T00:00:00+00:00", 90)
            .await
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].peer_device_id, "fresh");
    }

    /// 活跃 peer 水位为 0 时阻塞 epoch=5 的 tombstone GC。
    #[tokio::test]
    async fn active_peer_without_watermark_blocks_tombstone_gc() {
        let repo = setup_repo().await;
        let now = "2024-06-01T00:00:00+00:00";
        // peer 已见过但从未 advance ack → acked=0
        repo.touch_seen("peer-active", "prompts", now)
            .await
            .unwrap();
        let ok = repo
            .all_active_peers_acked("prompts", 5, now, DEFAULT_ACTIVE_PEER_WINDOW_DAYS)
            .await
            .unwrap();
        assert!(
            !ok,
            "acked=0 的活跃 peer 必须阻塞 epoch=5 的 tombstone GC"
        );

        // 推进后放行
        repo.advance_ack("peer-active", "prompts", 5, now)
            .await
            .unwrap();
        let ok2 = repo
            .all_active_peers_acked("prompts", 5, now, DEFAULT_ACTIVE_PEER_WINDOW_DAYS)
            .await
            .unwrap();
        assert!(ok2);
    }
}
