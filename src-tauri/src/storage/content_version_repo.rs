//! storage/content_version_repo.rs — 同步冲突/历史正文版本仓库
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备并发编辑同一 Prompt/Scratchpad 时，LWW 只保留 winner 会静默丢掉 loser。
//!     N2 要求并发且正文不同时写入 `content_versions` conflict copy，并保留有限历史，
//!     供 UI 查看、复制与恢复为新版本。
//!
//! Code Logic（这个模块做什么）:
//!     - 幂等建表 `content_versions`（UNIQUE domain/item/source/hash）；
//!     - 提供事务内 insert / 幂等 insert、按 item 列表、按 id 查询；
//!     - `prune_retention`：history 保留最近 20 条且 30 天内（先达限为准），
//!       conflict 在 30 天内即使超出条数也保留。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use crate::storage::sync_request_ledger_repo::SyncRequestLedgerRepo;
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
use std::sync::Arc;

/// 版本 kind：冲突副本。
pub const KIND_CONFLICT: &str = "conflict";
/// 版本 kind：普通历史快照。
pub const KIND_HISTORY: &str = "history";

/// 每 item 保留的最大 history 条数（先达限之一）。
pub const RETENTION_MAX_VERSIONS: usize = 20;
/// 保留窗口天数（history 与 conflict 最短窗口）。
pub const RETENTION_MAX_DAYS: i64 = 30;

/// 一条 content_versions 记录。
///
/// Business Logic: UI 与恢复流程需要 domain/item/source/hash/时间/快照，以便还原 loser 正文。
/// Code Logic: 与表列一一对应；id 通常由 `SyncRequestLedgerRepo::conflict_row_id` 确定性生成。
///     camelCase 序列化供备份 ZIP `contentVersions/items.json` 往返。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentVersion {
    /// 主键（确定性 conflict id 或 UUID）
    pub id: String,
    /// 领域 token（prompts / scratchpad / ssh_target）
    pub domain: String,
    /// 领域内 item id
    pub item_id: String,
    /// 产生该版本的设备 id
    pub source_device: String,
    /// 正文 content hash
    pub content_hash: String,
    /// 创建时间 RFC3339
    pub created_at: String,
    /// `conflict` | `history`
    pub kind: String,
    /// 完整行快照 JSON（用于恢复）
    pub snapshot_json: String,
}

/// 内容版本仓库。
///
/// Business Logic: 同步 merge 与 UI 历史面板共用同一持久化，避免两套冲突副本逻辑。
/// Code Logic: 持有 SqlitePool + maintenance gate；建表由 ensure_schema 负责。
pub struct ContentVersionRepo {
    /// SQLite 连接池
    db: SqlitePool,
    /// 维护闸：非事务写路径经 shared lease
    gate: Arc<DatabaseMaintenanceGate>,
}

impl ContentVersionRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: routes/测试与其它 writer 共享同一 pool，保证单连接语义。
    /// Code Logic: 委托 `with_gate` + 新建独立 gate。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: conflict/history 写必须与 restore 互斥，与其它 writer 共享 gate。
    /// Code Logic: 持有 pool clone + `Arc<DatabaseMaintenanceGate>`。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 返回内部 pool 引用。
    ///
    /// Business Logic: 调用方需要与其它 repo 共享事务时取同一 pool。
    /// Code Logic: 返回 `&SqlitePool`。
    #[allow(dead_code)] // apply_merge_batch 接线前供事务组装
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 幂等创建 `content_versions` 表与 item 索引。
    ///
    /// Business Logic: 旧库升级必须无迁移框架即可获得冲突/历史表。
    /// Code Logic: `CREATE TABLE/INDEX IF NOT EXISTS`。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS content_versions (
                id TEXT PRIMARY KEY,
                domain TEXT NOT NULL,
                item_id TEXT NOT NULL,
                source_device TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                kind TEXT NOT NULL,
                snapshot_json TEXT NOT NULL,
                UNIQUE(domain, item_id, source_device, content_hash)
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_content_versions_item
             ON content_versions(domain, item_id, created_at)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 在已开启事务上插入版本行。
    ///
    /// Business Logic: merge batch 必须与 active winner 同事务写入 conflict copy。
    /// Code Logic: INSERT；调用方负责 id 与 UNIQUE 冲突处理。
    #[allow(dead_code)] // 引擎 apply_merge_batch 接线使用
    pub async fn insert_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        version: &ContentVersion,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO content_versions \
             (id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&version.id)
        .bind(&version.domain)
        .bind(&version.item_id)
        .bind(&version.source_device)
        .bind(&version.content_hash)
        .bind(&version.created_at)
        .bind(&version.kind)
        .bind(&version.snapshot_json)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 幂等插入：UNIQUE 冲突时视为成功 no-op。
    ///
    /// Business Logic: 同批重放/重试不得因 conflict 行已存在而失败。
    /// Code Logic: 经 `with_shared_write_lease` 执行 INSERT OR IGNORE；返回是否新插入。
    pub async fn insert_idempotent(&self, version: &ContentVersion) -> Result<bool, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO content_versions \
                 (id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&version.id)
            .bind(&version.domain)
            .bind(&version.item_id)
            .bind(&version.source_device)
            .bind(&version.content_hash)
            .bind(&version.created_at)
            .bind(&version.kind)
            .bind(&version.snapshot_json)
            .execute(&self.db)
            .await?;
            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// 在事务上幂等插入。
    ///
    /// Business Logic: apply_merge_batch 同事务写入时也需幂等。
    /// Code Logic: INSERT OR IGNORE on tx。
    #[allow(dead_code)] // 引擎 apply_merge_batch 接线使用
    pub async fn insert_idempotent_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        version: &ContentVersion,
    ) -> Result<bool, AppError> {
        let result = sqlx::query(
            "INSERT OR IGNORE INTO content_versions \
             (id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&version.id)
        .bind(&version.domain)
        .bind(&version.item_id)
        .bind(&version.source_device)
        .bind(&version.content_hash)
        .bind(&version.created_at)
        .bind(&version.kind)
        .bind(&version.snapshot_json)
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 列出某 item 的全部版本，最新在前。
    ///
    /// Business Logic: 版本历史 UI 按时间倒序展示。
    /// Code Logic: ORDER BY created_at DESC, id DESC。
    pub async fn list_versions(
        &self,
        domain: &str,
        item_id: &str,
    ) -> Result<Vec<ContentVersion>, AppError> {
        let rows = sqlx::query(
            "SELECT id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json \
             FROM content_versions \
             WHERE domain = ? AND item_id = ? \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(domain)
        .bind(item_id)
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_from_sqlite).collect()
    }

    /// 导出全部 content_versions（备份/恢复往返）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     备份若不携带 conflict/history 副本，恢复后 UI 与合并证据会静默丢失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 全表按 created_at ASC, id ASC 稳定排序，供 `contentVersions/items.json`。
    pub async fn list_all(&self) -> Result<Vec<ContentVersion>, AppError> {
        let rows = sqlx::query(
            "SELECT id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json \
             FROM content_versions \
             ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_from_sqlite).collect()
    }

    /// 按主键查询版本。
    ///
    /// Business Logic: 恢复/复制冲突内容需要按 id 取完整快照。
    /// Code Logic: SELECT by id。
    pub async fn get(&self, id: &str) -> Result<Option<ContentVersion>, AppError> {
        let row = sqlx::query(
            "SELECT id, domain, item_id, source_device, content_hash, created_at, kind, snapshot_json \
             FROM content_versions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_from_sqlite(&r)?)),
            None => Ok(None),
        }
    }

    /// 按保留策略裁剪某 item 的历史版本。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Prompt/Scratchpad 不能无限堆积版本；保留最近 20 条 **且** 30 天内（先达限为准），
    ///     conflict 在 30 天内即使超出条数也必须保留，以保证冲突可追溯。
    ///
    /// Code Logic（这个函数做什么）:
    ///     取全部版本（最新在前）；对每条计算 age_days：
    ///     - conflict 且 age < 30 → 保留；
    ///     - 非 conflict：仅当 index < 20 **且** age < 30 时保留（先达限）；
    ///     - 其余 DELETE。
    ///     返回删除条数。
    pub async fn prune_retention(
        &self,
        domain: &str,
        item_id: &str,
        now: &str,
    ) -> Result<usize, AppError> {
        let now_dt = parse_rfc3339(now)?;
        let versions = self.list_versions(domain, item_id).await?;
        let mut to_delete: Vec<String> = Vec::new();
        for (index, version) in versions.iter().enumerate() {
            let created = parse_rfc3339(&version.created_at)?;
            let age = now_dt.signed_duration_since(created);
            let age_days = age.num_milliseconds() as f64 / 86_400_000.0;
            let keep = if version.kind == KIND_CONFLICT {
                age_days < RETENTION_MAX_DAYS as f64
            } else {
                index < RETENTION_MAX_VERSIONS && age_days < RETENTION_MAX_DAYS as f64
            };
            if !keep {
                to_delete.push(version.id.clone());
            }
        }
        with_shared_write_lease(&self.gate, async {
            let mut deleted = 0usize;
            for id in &to_delete {
                let result = sqlx::query("DELETE FROM content_versions WHERE id = ?")
                    .bind(id)
                    .execute(&self.db)
                    .await?;
                deleted += result.rows_affected() as usize;
            }
            Ok(deleted)
        })
        .await
    }

    /// 从 domain/item/source/hash 构造确定性版本 id。
    ///
    /// Business Logic: 重放 merge 不得产生重复 conflict 行。
    /// Code Logic: 委托 SyncRequestLedgerRepo::conflict_row_id。
    pub fn deterministic_id(
        domain: &str,
        item_id: &str,
        source_device: &str,
        content_hash: &str,
    ) -> String {
        SyncRequestLedgerRepo::conflict_row_id(domain, item_id, source_device, content_hash)
    }

    /// SQLite 行 → ContentVersion。
    fn row_from_sqlite(row: &SqliteRow) -> Result<ContentVersion, AppError> {
        Ok(ContentVersion {
            id: row.try_get("id")?,
            domain: row.try_get("domain")?,
            item_id: row.try_get("item_id")?,
            source_device: row.try_get("source_device")?,
            content_hash: row.try_get("content_hash")?,
            created_at: row.try_get("created_at")?,
            kind: row.try_get("kind")?,
            snapshot_json: row.try_get("snapshot_json")?,
        })
    }
}

/// 解析 RFC3339 时间戳。
///
/// Business Logic: 保留策略依赖可靠的 age 计算。
/// Code Logic: chrono DateTime::parse_from_rfc3339 → Utc。
fn parse_rfc3339(s: &str) -> Result<DateTime<Utc>, AppError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| AppError::generic(format!("非法时间戳 {s}: {e}")))
}

#[cfg(test)]
mod tests {
    //! content_version 单测：建表、幂等插入、列表顺序、保留策略。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 内存库 + schema。
    async fn setup_repo() -> ContentVersionRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        ContentVersionRepo::new(pool)
    }

    /// 构造测试版本。
    fn version(
        id: &str,
        item: &str,
        source: &str,
        hash: &str,
        created_at: &str,
        kind: &str,
    ) -> ContentVersion {
        ContentVersion {
            id: id.to_string(),
            domain: "prompts".to_string(),
            item_id: item.to_string(),
            source_device: source.to_string(),
            content_hash: hash.to_string(),
            created_at: created_at.to_string(),
            kind: kind.to_string(),
            snapshot_json: format!(r#"{{"content":"{hash}"}}"#),
        }
    }

    #[tokio::test]
    async fn content_version_insert_and_list_newest_first() {
        let repo = setup_repo().await;
        let older = version(
            "v1",
            "p1",
            "d1",
            "h1",
            "2024-01-01T00:00:00+00:00",
            KIND_HISTORY,
        );
        let newer = version(
            "v2",
            "p1",
            "d2",
            "h2",
            "2024-01-02T00:00:00+00:00",
            KIND_CONFLICT,
        );
        assert!(repo.insert_idempotent(&older).await.unwrap());
        assert!(repo.insert_idempotent(&newer).await.unwrap());
        // 重放 no-op
        assert!(!repo.insert_idempotent(&newer).await.unwrap());

        let listed = repo.list_versions("prompts", "p1").await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, "v2");
        assert_eq!(listed[1].id, "v1");

        let got = repo.get("v2").await.unwrap().unwrap();
        assert_eq!(got.kind, KIND_CONFLICT);
    }

    #[tokio::test]
    async fn content_version_prune_keeps_young_conflict_beyond_count() {
        let repo = setup_repo().await;
        let now = "2024-02-01T00:00:00+00:00";
        // 25 条 history，全部 10 天内 → 只保留 20
        for i in 0..25 {
            let day = 20 - (i % 10);
            let v = version(
                &format!("h{i}"),
                "p1",
                "d1",
                &format!("hh{i}"),
                &format!("2024-01-{day:02}T00:00:00+00:00"),
                KIND_HISTORY,
            );
            repo.insert_idempotent(&v).await.unwrap();
        }
        // 一条超出条数窗口的 conflict，但仍 < 30 天
        let conflict = version(
            "c-young",
            "p1",
            "d-loser",
            "conflict-hash",
            "2024-01-15T00:00:00+00:00",
            KIND_CONFLICT,
        );
        repo.insert_idempotent(&conflict).await.unwrap();

        let deleted = repo.prune_retention("prompts", "p1", now).await.unwrap();
        assert!(
            deleted >= 5,
            "应裁掉超出 20 条的 history，deleted={deleted}"
        );
        let remaining = repo.list_versions("prompts", "p1").await.unwrap();
        assert!(
            remaining.iter().any(|v| v.id == "c-young"),
            "年轻 conflict 即使超出条数也必须保留"
        );
        let history_count = remaining.iter().filter(|v| v.kind == KIND_HISTORY).count();
        assert!(
            history_count <= RETENTION_MAX_VERSIONS,
            "history 不得超过 20，got {history_count}"
        );
    }

    #[tokio::test]
    async fn content_version_prune_drops_old_conflict_after_30_days() {
        let repo = setup_repo().await;
        let now = "2024-03-15T00:00:00+00:00";
        let old_conflict = version(
            "c-old",
            "p1",
            "d1",
            "old",
            "2024-01-01T00:00:00+00:00",
            KIND_CONFLICT,
        );
        repo.insert_idempotent(&old_conflict).await.unwrap();
        let deleted = repo.prune_retention("prompts", "p1", now).await.unwrap();
        assert_eq!(deleted, 1);
        assert!(repo.get("c-old").await.unwrap().is_none());
    }

    #[test]
    fn content_version_deterministic_id_stable() {
        let a = ContentVersionRepo::deterministic_id("prompts", "id-1", "d1", "abc");
        let b = ContentVersionRepo::deterministic_id("prompts", "id-1", "d1", "abc");
        assert_eq!(a, b);
        assert_eq!(
            a,
            SyncRequestLedgerRepo::conflict_row_id("prompts", "id-1", "d1", "abc")
        );
    }
}
