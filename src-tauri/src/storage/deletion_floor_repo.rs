//! storage/deletion_floor_repo.rs — 删除 floor 与 tombstone 安全压缩
//!
//! Business Logic（为什么需要这个模块）:
//!     完整 tombstone 在 age≥30 天且所有活跃 peer 已 ack 其 delete_epoch 后，可压缩为
//!     轻量 `sync_deletion_floors`，防止无限膨胀。floor 持久拒绝被其支配的旧 live 复活；
//!     并发 clock 保留历史副本但 active 仍 delete-wins。离线 180 天 peer 不能复活已压缩删除。
//!
//! Code Logic（这个模块做什么）:
//!     - 幂等建表；upsert/get/list；
//!     - `apply_deletion_floor` 用向量时钟比较决定 DeleteWins / KeepHistoryButDeleted / AcceptLive；
//!     - `tombstone_gc_eligible` / `compact_tombstones_to_floors` 纯结构 + 水位辅助。

use crate::error::AppError;
use crate::storage::maintenance_gate::{begin_shared_write, DatabaseMaintenanceGate};
use crate::storage::sync_watermark_repo::{
    SyncWatermarkRepo, DEFAULT_ACTIVE_PEER_WINDOW_DAYS,
};
use crate::sync::vector_clock::{compare, ClockOrder};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
use std::collections::HashMap;
use std::sync::Arc;

/// Tombstone 可压缩的最小年龄（天）。
pub const TOMBSTONE_GC_MIN_AGE_DAYS: i64 = 30;

/// 删除 floor 一行。
///
/// Business Logic: 压缩后仍需用 delete 向量时钟与 epoch 拒绝旧 live 复活。
/// Code Logic: delete_vector_clock 以 JSON 落库。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionFloor {
    /// 领域 token
    pub domain: String,
    /// 领域内 item id
    pub item_id: String,
    /// 删除事件向量时钟
    pub delete_vector_clock: HashMap<String, u64>,
    /// 铸造该删除时的 domain delete epoch
    pub delete_epoch: u64,
    /// 删除时正文 hash（审计）
    pub content_hash: String,
    /// floor 创建时间 RFC3339
    pub created_at: String,
}

/// 对 incoming live 应用 floor 的决策。
///
/// Business Logic: 引擎据此拒绝复活、保留冲突历史或（少见）接受严格领先 live。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionFloorDecision {
    /// incoming 被 floor 支配（落后或相等）→ 拒绝复活，回送 delete
    DeleteWins,
    /// 并发 → 可写 history/conflict，但 active 仍保持 deleted
    KeepHistoryButDeleted,
    /// incoming 严格领先 floor → 接受 live（显式新写入路径）
    AcceptLive,
}

/// GC 候选 tombstone 描述（测试与引擎共用结构）。
///
/// Business Logic: compact 前需要 age 与 epoch；不绑定具体领域表列名以外的逻辑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneGcCandidate {
    /// 领域
    pub domain: String,
    /// item id
    pub item_id: String,
    /// 删除向量时钟
    pub delete_vector_clock: HashMap<String, u64>,
    /// tombstone 上的 delete_epoch
    pub delete_epoch: u64,
    /// 正文 hash
    pub content_hash: String,
    /// tombstone.updated_at
    pub updated_at: String,
}

/// GC 运行结果摘要（测试用）。
///
/// Business Logic: 单测断言压缩条数，不依赖真实 prompts 表。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TombstoneGcResult {
    /// 成功压缩为 floor 的条数
    pub deleted: usize,
    /// 因水位/年龄被跳过的条数
    pub skipped: usize,
}

/// 删除 floor 仓库。
pub struct DeletionFloorRepo {
    /// SQLite 连接池
    db: SqlitePool,
    /// 维护闸：独立 upsert 事务经 shared lease
    gate: Arc<DatabaseMaintenanceGate>,
}

impl DeletionFloorRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: merge / GC / 导出恢复共用 floor 读写。
    /// Code Logic: 委托 `with_gate` + 新建独立 gate。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: floor 写必须与 restore 互斥，与其它 writer 共享 gate。
    /// Code Logic: 持有 pool clone + `Arc<DatabaseMaintenanceGate>`。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 返回内部 pool。
    ///
    /// Business Logic: 事务与 watermark 共享 pool。
    /// Code Logic: `&SqlitePool`。
    #[allow(dead_code)] // GC/引擎接线前供事务组装
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 幂等创建 `sync_deletion_floors` 表。
    ///
    /// Business Logic: 旧库升级即可拒绝已压缩删除的复活。
    /// Code Logic: CREATE TABLE IF NOT EXISTS，PK (domain, item_id)。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sync_deletion_floors (
                domain TEXT NOT NULL,
                item_id TEXT NOT NULL,
                delete_vector_clock TEXT NOT NULL,
                delete_epoch INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (domain, item_id)
            )",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 在事务内 upsert floor。
    ///
    /// Business Logic: GC 在删除完整 tombstone 的同一事务写入 floor。
    /// Code Logic: INSERT OR REPLACE。
    pub async fn upsert_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        floor: &DeletionFloor,
    ) -> Result<(), AppError> {
        let vc_json = serde_json::to_string(&floor.delete_vector_clock)?;
        sqlx::query(
            "INSERT OR REPLACE INTO sync_deletion_floors \
             (domain, item_id, delete_vector_clock, delete_epoch, content_hash, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&floor.domain)
        .bind(&floor.item_id)
        .bind(vc_json)
        .bind(floor.delete_epoch as i64)
        .bind(&floor.content_hash)
        .bind(&floor.created_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 非事务 upsert（测试/单写路径）。
    ///
    /// Business Logic: 单测与不需与 tombstone 删除同事务时使用。
    /// Code Logic: `begin_shared_write` → upsert_on_tx → commit。
    pub async fn upsert(&self, floor: &DeletionFloor) -> Result<(), AppError> {
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        Self::upsert_on_tx(&mut tx, floor).await?;
        tx.commit().await?;
        drop(permit);
        Ok(())
    }

    /// 按 domain+item 读取 floor。
    ///
    /// Business Logic: merge incoming live 前查询是否已被压缩删除。
    /// Code Logic: SELECT by PK。
    pub async fn get(
        &self,
        domain: &str,
        item_id: &str,
    ) -> Result<Option<DeletionFloor>, AppError> {
        let row = sqlx::query(
            "SELECT domain, item_id, delete_vector_clock, delete_epoch, content_hash, created_at \
             FROM sync_deletion_floors WHERE domain = ? AND item_id = ?",
        )
        .bind(domain)
        .bind(item_id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_from_sqlite(&r)?)),
            None => Ok(None),
        }
    }

    /// 列出 domain 下全部 floor。
    ///
    /// Business Logic: 完整 manifest 对账与导出需要全量 floor。
    /// Code Logic: SELECT WHERE domain ORDER BY item_id。
    #[allow(dead_code)] // manifest/导出接线使用
    pub async fn list_for_domain(&self, domain: &str) -> Result<Vec<DeletionFloor>, AppError> {
        let rows = sqlx::query(
            "SELECT domain, item_id, delete_vector_clock, delete_epoch, content_hash, created_at \
             FROM sync_deletion_floors WHERE domain = ? ORDER BY item_id ASC",
        )
        .bind(domain)
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_from_sqlite).collect()
    }

    /// 用 floor 决策 incoming live 的命运。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     压缩删除后旧 live 不得复活；并发版本可进 history，但 active 保持 deleted。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `compare(incoming_vc, floor.delete_vector_clock)`：
    ///     - Before/Equal → DeleteWins；
    ///     - Concurrent → KeepHistoryButDeleted；
    ///     - After → AcceptLive。
    pub fn apply_deletion_floor(
        floor: &DeletionFloor,
        incoming_live_vector_clock: &HashMap<String, u64>,
    ) -> DeletionFloorDecision {
        match compare(incoming_live_vector_clock, &floor.delete_vector_clock) {
            ClockOrder::Before | ClockOrder::Equal => DeletionFloorDecision::DeleteWins,
            ClockOrder::Concurrent => DeletionFloorDecision::KeepHistoryButDeleted,
            ClockOrder::After => DeletionFloorDecision::AcceptLive,
        }
    }

    /// 判断单条 tombstone 是否满足 age 条件（≥30 天）。
    ///
    /// Business Logic: GC 第一门槛是年龄，避免过早压缩仍在传播的删除。
    /// Code Logic: now - updated_at >= 30 days。
    pub fn tombstone_age_eligible(updated_at: &str, now: &str) -> Result<bool, AppError> {
        let updated = parse_rfc3339(updated_at)?;
        let now_dt = parse_rfc3339(now)?;
        let age = now_dt.signed_duration_since(updated);
        Ok(age >= Duration::days(TOMBSTONE_GC_MIN_AGE_DAYS))
    }

    /// 判断 tombstone 是否可安全压缩（age + 全部活跃 peer ack）。
    ///
    /// Business Logic: 两条件同时满足才可在同一事务中用 floor 替换完整 tombstone。
    /// Code Logic: age 检查 + SyncWatermarkRepo::all_active_peers_acked。
    pub async fn tombstone_gc_eligible(
        &self,
        watermarks: &SyncWatermarkRepo,
        candidate: &TombstoneGcCandidate,
        now: &str,
        active_window_days: i64,
    ) -> Result<bool, AppError> {
        if !Self::tombstone_age_eligible(&candidate.updated_at, now)? {
            return Ok(false);
        }
        watermarks
            .all_active_peers_acked(
                &candidate.domain,
                candidate.delete_epoch,
                now,
                active_window_days,
            )
            .await
    }

    /// 将合格 tombstone 压缩为 floor（结构/测试 helper）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GC 必须在单事务中：写 floor → 调用方删除完整 tombstone；本 helper 负责
    ///     资格判断与 floor upsert，供单测与未来引擎共用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对每个 candidate 检查 eligible；合格则 upsert floor 并计入 deleted；
    ///     不合格 skipped。不直接改领域表（tombstone 行由调用方删除）。
    pub async fn compact_tombstones_to_floors(
        &self,
        watermarks: &SyncWatermarkRepo,
        candidates: &[TombstoneGcCandidate],
        now: &str,
    ) -> Result<TombstoneGcResult, AppError> {
        let mut result = TombstoneGcResult::default();
        for candidate in candidates {
            let eligible = self
                .tombstone_gc_eligible(
                    watermarks,
                    candidate,
                    now,
                    DEFAULT_ACTIVE_PEER_WINDOW_DAYS,
                )
                .await?;
            if !eligible {
                result.skipped += 1;
                continue;
            }
            let floor = DeletionFloor {
                domain: candidate.domain.clone(),
                item_id: candidate.item_id.clone(),
                delete_vector_clock: candidate.delete_vector_clock.clone(),
                delete_epoch: candidate.delete_epoch,
                content_hash: candidate.content_hash.clone(),
                created_at: now.to_string(),
            };
            self.upsert(&floor).await?;
            result.deleted += 1;
        }
        Ok(result)
    }

    /// SQLite 行 → DeletionFloor。
    fn row_from_sqlite(row: &SqliteRow) -> Result<DeletionFloor, AppError> {
        let vc_text: String = row.try_get("delete_vector_clock")?;
        let delete_vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        let epoch: i64 = row.try_get("delete_epoch")?;
        Ok(DeletionFloor {
            domain: row.try_get("domain")?,
            item_id: row.try_get("item_id")?,
            delete_vector_clock,
            delete_epoch: epoch.max(0) as u64,
            content_hash: row.try_get("content_hash")?,
            created_at: row.try_get("created_at")?,
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
    //! deletion_floor 单测：GC 水位阻塞、离线 peer 无法复活、决策枚举。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> (DeletionFloorRepo, SyncWatermarkRepo) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        (
            DeletionFloorRepo::new(pool.clone()),
            SyncWatermarkRepo::new(pool),
        )
    }

    fn vc(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// 活跃 peer 未 ack 时 GC 删除数为 0。
    #[tokio::test]
    async fn active_peer_without_watermark_blocks_tombstone_gc() {
        let (floors, watermarks) = setup().await;
        let now = "2024-06-01T00:00:00+00:00";
        // 活跃 peer 仅 touch，acked=0
        watermarks
            .touch_seen("peer-active", "prompts", now)
            .await
            .unwrap();
        let candidate = TombstoneGcCandidate {
            domain: "prompts".into(),
            item_id: "p1".into(),
            delete_vector_clock: vc(&[("d1", 2)]),
            delete_epoch: 5,
            content_hash: "h".into(),
            // 年龄 > 30 天
            updated_at: "2024-04-01T00:00:00+00:00".into(),
        };
        let result = floors
            .compact_tombstones_to_floors(&watermarks, &[candidate], now)
            .await
            .unwrap();
        assert_eq!(result.deleted, 0);
        assert_eq!(result.skipped, 1);
        assert!(floors.get("prompts", "p1").await.unwrap().is_none());
    }

    /// 离线 180 天 peer 携带旧 live 不得复活已压缩删除。
    #[tokio::test]
    async fn peer_offline_for_180_days_cannot_resurrect_compacted_delete() {
        let (floors, _wm) = setup().await;
        let floor = DeletionFloor {
            domain: "prompts".into(),
            item_id: "p-gone".into(),
            delete_vector_clock: vc(&[("d1", 3)]),
            delete_epoch: 7,
            content_hash: "deleted-hash".into(),
            created_at: "2024-01-01T00:00:00+00:00".into(),
        };
        floors.upsert(&floor).await.unwrap();

        // 离线 peer 的旧 live：clock 落后于 floor
        let incoming_old_live = vc(&[("d1", 1)]);
        let decision = DeletionFloorRepo::apply_deletion_floor(&floor, &incoming_old_live);
        assert_eq!(decision, DeletionFloorDecision::DeleteWins);

        // 即使 peer 自己有并发分支，active 仍 delete-wins
        let concurrent_live = vc(&[("offline", 5)]);
        let decision2 = DeletionFloorRepo::apply_deletion_floor(&floor, &concurrent_live);
        assert_eq!(decision2, DeletionFloorDecision::KeepHistoryButDeleted);
    }

    #[tokio::test]
    async fn deletion_floor_gc_succeeds_when_age_and_acks_ok() {
        let (floors, watermarks) = setup().await;
        let now = "2024-06-01T00:00:00+00:00";
        watermarks
            .advance_ack("peer-a", "prompts", 5, now)
            .await
            .unwrap();
        let candidate = TombstoneGcCandidate {
            domain: "prompts".into(),
            item_id: "p2".into(),
            delete_vector_clock: vc(&[("d1", 2)]),
            delete_epoch: 5,
            content_hash: "h2".into(),
            updated_at: "2024-04-01T00:00:00+00:00".into(),
        };
        let result = floors
            .compact_tombstones_to_floors(&watermarks, &[candidate], now)
            .await
            .unwrap();
        assert_eq!(result.deleted, 1);
        let floor = floors.get("prompts", "p2").await.unwrap().unwrap();
        assert_eq!(floor.delete_epoch, 5);
    }

    #[test]
    fn apply_deletion_floor_after_accepts_live() {
        let floor = DeletionFloor {
            domain: "prompts".into(),
            item_id: "p3".into(),
            delete_vector_clock: vc(&[("d1", 1)]),
            delete_epoch: 1,
            content_hash: "h".into(),
            created_at: "2024-01-01T00:00:00+00:00".into(),
        };
        let newer = vc(&[("d1", 2)]);
        assert_eq!(
            DeletionFloorRepo::apply_deletion_floor(&floor, &newer),
            DeletionFloorDecision::AcceptLive
        );
    }

    #[test]
    fn tombstone_younger_than_30_days_not_age_eligible() {
        let ok = DeletionFloorRepo::tombstone_age_eligible(
            "2024-05-20T00:00:00+00:00",
            "2024-06-01T00:00:00+00:00",
        )
        .unwrap();
        assert!(!ok);
        let ok2 = DeletionFloorRepo::tombstone_age_eligible(
            "2024-04-01T00:00:00+00:00",
            "2024-06-01T00:00:00+00:00",
        )
        .unwrap();
        assert!(ok2);
    }
}
