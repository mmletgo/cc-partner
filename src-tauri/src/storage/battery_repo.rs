//! battery_repo.rs — 充电模式状态与流水。
//!
//! Business Logic（为什么需要这个模块）:
//!     充电余额必须本机权威、可对账、可幂等入账；不能靠前端 localStorage。
//!
//! Code Logic（这个模块做什么）:
//!     幂等建 `battery_state` / `battery_ledger`；读写单行状态；按 source_id 幂等插流水。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::{Row, SqlitePool};
use std::sync::Arc;

const STATE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS battery_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    mode TEXT NOT NULL,
    remaining_ms INTEGER NOT NULL,
    welcome_granted INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_daily_reset_at INTEGER NOT NULL DEFAULT 0
)";

const LEDGER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS battery_ledger (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts INTEGER NOT NULL,
    kind TEXT NOT NULL,
    source_id TEXT,
    delta_ms INTEGER NOT NULL,
    balance_after_ms INTEGER NOT NULL,
    note TEXT
)";

const LEDGER_SOURCE_INDEX: &str = "CREATE UNIQUE INDEX IF NOT EXISTS idx_battery_ledger_source
    ON battery_ledger(source_id) WHERE source_id IS NOT NULL AND source_id <> ''";

/// 充电运行态单行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryStateRow {
    /// `charging` 或 `unlimited`。
    pub mode: String,
    /// 剩余毫秒，永不小于 0。
    pub remaining_ms: i64,
    /// 是否已发过欢迎赠送。
    pub welcome_granted: bool,
    /// 最近更新 unix 秒。
    pub updated_at: i64,
    /// 已重置到的每日 8 点边界（unix 秒）；0 = 从未重置。
    pub last_daily_reset_at: i64,
}

/// 流水行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryLedgerRow {
    /// 自增 id。
    pub id: i64,
    /// unix 秒。
    pub ts: i64,
    /// credit_health / credit_wordgame / credit_welcome / debit_tick / mode_change。
    pub kind: String,
    /// 幂等键；debit 可为空。
    pub source_id: Option<String>,
    /// 正负毫秒。
    pub delta_ms: i64,
    /// 本条之后的余额。
    pub balance_after_ms: i64,
    /// 可选备注。
    pub note: Option<String>,
}

/// 充电账本仓库。
pub struct BatteryRepo {
    db: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl BatteryRepo {
    /// 测试构造：独立 gate。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 maintenance gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 幂等建表。
    ///
    /// Business Logic: 旧库无充电表时启动必须可升级，禁止 sqlx::migrate!。
    /// Code Logic: CREATE TABLE IF NOT EXISTS + 部分唯一索引；旧表缺 `last_daily_reset_at` 列时 PRAGMA 检查后 ALTER 补列。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(STATE_SCHEMA).execute(pool).await?;
        sqlx::query(LEDGER_SCHEMA).execute(pool).await?;
        sqlx::query(LEDGER_SOURCE_INDEX).execute(pool).await?;
        let columns = sqlx::query("PRAGMA table_info(battery_state)")
            .fetch_all(pool)
            .await?;
        let has_reset_at = columns.iter().any(|row| {
            row.try_get::<String, _>("name")
                .map(|name| name == "last_daily_reset_at")
                .unwrap_or(false)
        });
        if !has_reset_at {
            sqlx::query(
                "ALTER TABLE battery_state ADD COLUMN last_daily_reset_at INTEGER NOT NULL DEFAULT 0",
            )
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    /// 读取单行状态；尚未初始化返回 None。
    pub async fn get_state(&self) -> Result<Option<BatteryStateRow>, AppError> {
        let row = sqlx::query(
            "SELECT mode, remaining_ms, welcome_granted, updated_at, last_daily_reset_at FROM battery_state WHERE id = 1",
        )
        .fetch_optional(&self.db)
        .await?;
        Ok(row.map(|r| BatteryStateRow {
            mode: r.get("mode"),
            remaining_ms: r.get("remaining_ms"),
            welcome_granted: r.get::<i64, _>("welcome_granted") != 0,
            updated_at: r.get("updated_at"),
            last_daily_reset_at: r.get("last_daily_reset_at"),
        }))
    }

    /// 插入默认无限模式行（已存在则 no-op）。
    ///
    /// Business Logic: 首次读取必须有权威行，默认无限且余额 0。
    pub async fn ensure_default_state(&self, now: i64) -> Result<BatteryStateRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR IGNORE INTO battery_state (id, mode, remaining_ms, welcome_granted, updated_at, last_daily_reset_at)
                 VALUES (1, 'unlimited', 0, 0, ?, 0)",
            )
            .bind(now)
            .execute(&self.db)
            .await?;
            Ok::<(), AppError>(())
        })
        .await?;
        self.get_state()
            .await?
            .ok_or_else(|| AppError::generic("battery_state 初始化失败"))
    }

    /// 覆盖写单行状态。
    pub async fn upsert_state(&self, state: &BatteryStateRow) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO battery_state (id, mode, remaining_ms, welcome_granted, updated_at, last_daily_reset_at)
                 VALUES (1, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    mode = excluded.mode,
                    remaining_ms = excluded.remaining_ms,
                    welcome_granted = excluded.welcome_granted,
                    updated_at = excluded.updated_at,
                    last_daily_reset_at = excluded.last_daily_reset_at",
            )
            .bind(&state.mode)
            .bind(state.remaining_ms)
            .bind(i64::from(state.welcome_granted))
            .bind(state.updated_at)
            .bind(state.last_daily_reset_at)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 按 source_id 幂等插入流水。
    ///
    /// Business Logic: 同一健康记录 / 同一张闪卡不得双计入账。
    /// Code Logic: 有 source_id 时 INSERT OR IGNORE；返回是否新插入。
    pub async fn insert_ledger(&self, row: &BatteryLedgerRow) -> Result<bool, AppError> {
        with_shared_write_lease(&self.gate, async {
            let result = sqlx::query(
                "INSERT OR IGNORE INTO battery_ledger (ts, kind, source_id, delta_ms, balance_after_ms, note)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(row.ts)
            .bind(&row.kind)
            .bind(&row.source_id)
            .bind(row.delta_ms)
            .bind(row.balance_after_ms)
            .bind(&row.note)
            .execute(&self.db)
            .await?;
            Ok(result.rows_affected() > 0)
        })
        .await
    }

    /// 当日某来源前缀的入账次数（kind 匹配且 ts 落在 [day_start, day_end)）。
    pub async fn count_credits_today(
        &self,
        kind: &str,
        source_prefix: &str,
        day_start: i64,
        day_end: i64,
    ) -> Result<i64, AppError> {
        let like = format!("{source_prefix}%");
        let row = sqlx::query(
            "SELECT COUNT(*) AS n FROM battery_ledger
             WHERE kind = ? AND ts >= ? AND ts < ? AND source_id LIKE ?",
        )
        .bind(kind)
        .bind(day_start)
        .bind(day_end)
        .bind(like)
        .fetch_one(&self.db)
        .await?;
        Ok(row.get("n"))
    }

    /// 当日已充 / 已用毫秒（debit 取绝对值；`daily_reset` 不计入——今日已充只统计主动赚取）。
    pub async fn today_totals(&self, day_start: i64, day_end: i64) -> Result<(i64, i64), AppError> {
        let row = sqlx::query(
            "SELECT
                COALESCE(SUM(CASE WHEN delta_ms > 0 THEN delta_ms ELSE 0 END), 0) AS earned,
                COALESCE(SUM(CASE WHEN delta_ms < 0 THEN -delta_ms ELSE 0 END), 0) AS spent
             FROM battery_ledger WHERE ts >= ? AND ts < ? AND kind <> 'daily_reset'",
        )
        .bind(day_start)
        .bind(day_end)
        .fetch_one(&self.db)
        .await?;
        Ok((row.get("earned"), row.get("spent")))
    }

    /// 最近流水，新到旧。
    pub async fn list_ledger(&self, limit: i64) -> Result<Vec<BatteryLedgerRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, ts, kind, source_id, delta_ms, balance_after_ms, note
             FROM battery_ledger ORDER BY id DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| BatteryLedgerRow {
                id: r.get("id"),
                ts: r.get("ts"),
                kind: r.get("kind"),
                source_id: r.get("source_id"),
                delta_ms: r.get("delta_ms"),
                balance_after_ms: r.get("balance_after_ms"),
                note: r.get("note"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn repo() -> BatteryRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        BatteryRepo::ensure_schema(&pool).await.unwrap();
        BatteryRepo::new(pool)
    }

    #[tokio::test]
    async fn ensure_default_is_unlimited_zero() {
        let repo = repo().await;
        let state = repo.ensure_default_state(1_700_000_000).await.unwrap();
        assert_eq!(state.mode, "unlimited");
        assert_eq!(state.remaining_ms, 0);
        assert!(!state.welcome_granted);
        assert_eq!(state.last_daily_reset_at, 0);
        let again = repo.ensure_default_state(1_700_000_001).await.unwrap();
        assert_eq!(again.updated_at, 1_700_000_000);
    }

    #[tokio::test]
    async fn ensure_schema_adds_last_daily_reset_at_to_legacy_table() {
        let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 先建旧版无 last_daily_reset_at 列的表，模拟升级前旧库。
        sqlx::query(
            "CREATE TABLE battery_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                mode TEXT NOT NULL,
                remaining_ms INTEGER NOT NULL,
                welcome_granted INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO battery_state (id, mode, remaining_ms, welcome_granted, updated_at)
             VALUES (1, 'charging', 60000, 1, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();

        BatteryRepo::ensure_schema(&pool).await.unwrap();

        let repo = BatteryRepo::new(pool);
        let state = repo.get_state().await.unwrap().unwrap();
        assert_eq!(state.mode, "charging");
        assert_eq!(state.remaining_ms, 60_000);
        assert_eq!(state.last_daily_reset_at, 0);
    }

    #[tokio::test]
    async fn today_totals_excludes_daily_reset() {
        let repo = repo().await;
        for row in [
            BatteryLedgerRow {
                id: 0,
                ts: 100,
                kind: "daily_reset".into(),
                source_id: Some("daily_reset:100".into()),
                delta_ms: 14_400_000,
                balance_after_ms: 14_400_000,
                note: None,
            },
            BatteryLedgerRow {
                id: 0,
                ts: 101,
                kind: "credit_health".into(),
                source_id: Some("habit:1".into()),
                delta_ms: 480_000,
                balance_after_ms: 14_880_000,
                note: None,
            },
        ] {
            repo.insert_ledger(&row).await.unwrap();
        }
        let (earned, spent) = repo.today_totals(0, 200).await.unwrap();
        assert_eq!(earned, 480_000);
        assert_eq!(spent, 0);
    }

    #[tokio::test]
    async fn ledger_source_id_is_idempotent() {
        let repo = repo().await;
        let row = BatteryLedgerRow {
            id: 0,
            ts: 10,
            kind: "credit_health".into(),
            source_id: Some("habit:7".into()),
            delta_ms: 480_000,
            balance_after_ms: 480_000,
            note: None,
        };
        assert!(repo.insert_ledger(&row).await.unwrap());
        assert!(!repo.insert_ledger(&row).await.unwrap());
        let listed = repo.list_ledger(10).await.unwrap();
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn today_credit_count_uses_prefix() {
        let repo = repo().await;
        for id in ["habit:1", "habit:2", "wordgame:x"] {
            repo.insert_ledger(&BatteryLedgerRow {
                id: 0,
                ts: 100,
                kind: "credit_health".into(),
                source_id: Some(id.into()),
                delta_ms: 1,
                balance_after_ms: 1,
                note: None,
            })
            .await
            .unwrap();
        }
        let n = repo
            .count_credits_today("credit_health", "habit:", 0, 200)
            .await
            .unwrap();
        assert_eq!(n, 2);
    }
}
