//! storage/sync_request_ledger_repo.rs — 同步 push-batch 幂等 outcome ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt/SSH/Scratchpad 的 v2 push-batch 在响应丢失后会被客户端重放。服务端必须以
//!     `UNIQUE(claimed_device_id, domain, client_request_id)` 记录 payload hash 与 outcome：
//!     同 key/同 hash 直接返回原 outcome 且不重复写冲突/历史；同 key/不同 hash 返回 conflict。
//!     `claimed_device_id` 只是收敛标签，不是认证身份。
//!
//! Code Logic（这个模块做什么）:
//!     - 幂等建表 `sync_request_ledger`；
//!     - 在**单事务**内 claim ledger → 执行调用方 apply 闭包 → 写入 accepted outcome → commit；
//!     - 提供 deterministic conflict row id helper（供未来 content_versions 使用）。

use crate::error::AppError;
use crate::storage::maintenance_gate::{begin_shared_write, DatabaseMaintenanceGate};
use crate::sync::protocol::content_sha256_hex;
use chrono::Utc;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};
use std::sync::Arc;

/// 领域 token：Prompt 同步。
pub const DOMAIN_PROMPTS: &str = "prompts";
/// 领域 token：SSH 目标同步。
pub const DOMAIN_SSH_TARGET: &str = "ssh_target";
/// 领域 token：速记本同步。
pub const DOMAIN_SCRATCHPAD: &str = "scratchpad";

/// push-batch 落库 outcome（当前仅 accepted 条数；未来可扩展 failure 明细）。
///
/// Business Logic: 重放时必须精确回放首次结果，避免客户端误判 partial/成功计数。
/// Code Logic: JSON 序列化存入 ledger.outcome_json。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncBatchOutcome {
    /// 实际写入条数（merge 后 to_upsert 长度；可与请求 items 不同）。
    pub accepted: usize,
}

/// ledger 一行（测试/调试用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncRequestLedgerRow {
    pub claimed_device_id: String,
    pub domain: String,
    pub client_request_id: String,
    pub payload_hash: String,
    pub outcome: SyncBatchOutcome,
    pub created_at: String,
}

/// 同步请求 ledger 仓库。
pub struct SyncRequestLedgerRepo {
    /// SQLite 连接池（max_connections(1)，与其它 writer 共享单连接语义）
    db: SqlitePool,
    /// 维护闸：apply_batch 写事务经 shared lease
    gate: Arc<DatabaseMaintenanceGate>,
}

impl SyncRequestLedgerRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: routes/测试用同一 pool 打开 ledger，保证与 bulk upsert 同库。
    /// Code Logic: 委托 `with_gate` + 新建独立 gate。
    #[allow(dead_code)] // intentional public API / tests
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: push-batch ledger 写必须与 restore 互斥，与其它 writer 共享 gate。
    /// Code Logic: 持有 pool clone + `Arc<DatabaseMaintenanceGate>`。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 返回内部 pool 引用（测试/路由组装用）。
    ///
    /// Business Logic: 调用方需要把 ledger 与领域 repo 绑到同一 pool 的事务上。
    /// Code Logic: 返回 `&SqlitePool`。
    #[allow(dead_code)] // intentional public API for tx assembly / tests
    pub fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// 幂等创建 `sync_request_ledger` 表。
    ///
    /// Business Logic: 旧库升级必须无迁移框架即可获得 ledger；表缺失时 push-batch 幂等不可用。
    /// Code Logic: `CREATE TABLE IF NOT EXISTS` + UNIQUE(claimed_device_id, domain, client_request_id)。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sync_request_ledger (
                claimed_device_id TEXT NOT NULL,
                domain TEXT NOT NULL,
                client_request_id TEXT NOT NULL,
                payload_hash TEXT NOT NULL,
                outcome_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(claimed_device_id, domain, client_request_id)
            )",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// 为未来 content_versions conflict copy 生成确定性主键。
    ///
    /// Business Logic: 同批重放不得插入重复 conflict/history 行；冲突副本 id 必须由
    ///     domain/item/sourceDevice/hash 稳定导出，便于幂等插入。
    /// Code Logic: SHA-256(domain\\0item\\0source\\0hash) 十六进制。
    pub fn conflict_row_id(
        domain: &str,
        item_id: &str,
        source_device: &str,
        content_hash: &str,
    ) -> String {
        content_sha256_hex(&[
            domain.as_bytes(),
            b"\0",
            item_id.as_bytes(),
            b"\0",
            source_device.as_bytes(),
            b"\0",
            content_hash.as_bytes(),
        ])
    }

    /// 在已开启事务上查询 ledger 行。
    ///
    /// Business Logic: claim 前需判断同 key 是否已有 outcome，以决定重放或冲突。
    /// Code Logic: SELECT by UNIQUE key；无行返回 None。
    pub async fn get_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        claimed_device_id: &str,
        domain: &str,
        client_request_id: &str,
    ) -> Result<Option<SyncRequestLedgerRow>, AppError> {
        let row = sqlx::query(
            "SELECT claimed_device_id, domain, client_request_id, payload_hash, outcome_json, created_at \
             FROM sync_request_ledger \
             WHERE claimed_device_id = ? AND domain = ? AND client_request_id = ?",
        )
        .bind(claimed_device_id)
        .bind(domain)
        .bind(client_request_id)
        .fetch_optional(&mut **tx)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_from_sqlite(&r)?)),
            None => Ok(None),
        }
    }

    /// 在已开启事务上插入 ledger outcome（首次 claim）。
    ///
    /// Business Logic: 首次成功 apply 后必须在同一事务记录 exact outcome，供重放返回。
    /// Code Logic: INSERT；UNIQUE 冲突上抛（pool=1 下正常路径不应并发抢写）。
    pub async fn insert_on_tx(
        tx: &mut Transaction<'_, Sqlite>,
        claimed_device_id: &str,
        domain: &str,
        client_request_id: &str,
        payload_hash: &str,
        outcome: &SyncBatchOutcome,
    ) -> Result<(), AppError> {
        let outcome_json = serde_json::to_string(outcome)?;
        let created_at = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO sync_request_ledger \
             (claimed_device_id, domain, client_request_id, payload_hash, outcome_json, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(claimed_device_id)
        .bind(domain)
        .bind(client_request_id)
        .bind(payload_hash)
        .bind(outcome_json)
        .bind(created_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 单事务幂等 apply：claim ledger → 执行 apply → 记录 outcome → commit。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     push-batch 必须在**同一事务**完成 ledger claim 与领域 bulk 写入，保证：
    ///     1) 同 key/同 hash 重放返回原 accepted，且不重复 apply；
    ///     2) 同 key/不同 hash 返回 conflict，不写半批次；
    ///     3) apply 中途失败整事务回滚（含未写入的 ledger）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `begin_shared_write` → get_on_tx；命中同 hash 则 commit 前直接返回 outcome（无 apply）；
    ///     命中不同 hash → conflict；未命中则调用 apply 闭包（`for<'a> → BoxFuture<'a>`），
    ///     insert_on_tx 后 commit。使用 `BoxFuture` 绑定 tx 与 future 生命周期，避免
    ///     普通 `async move` 闭包无法表达的 HRTB 约束。
    pub async fn apply_batch_idempotent<F>(
        &self,
        claimed_device_id: &str,
        domain: &str,
        client_request_id: &str,
        payload_hash: &str,
        apply: F,
    ) -> Result<SyncBatchOutcome, AppError>
    where
        F: for<'a> FnOnce(
            &'a mut Transaction<'_, Sqlite>,
        ) -> BoxFuture<'a, Result<SyncBatchOutcome, AppError>>,
    {
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        if let Some(existing) =
            Self::get_on_tx(&mut tx, claimed_device_id, domain, client_request_id).await?
        {
            if existing.payload_hash != payload_hash {
                return Err(AppError::conflict(format!(
                    "同步 batch ledger 冲突：client_request_id={client_request_id} 已绑定不同 payload"
                )));
            }
            // 同 key/同 hash：直接返回记录的 outcome，不 re-apply。
            // 事务无写操作，commit 仅为释放连接（SQLite 只读事务也可 commit）。
            tx.commit().await?;
            drop(permit);
            return Ok(existing.outcome);
        }

        let outcome = apply(&mut tx).await?;
        Self::insert_on_tx(
            &mut tx,
            claimed_device_id,
            domain,
            client_request_id,
            payload_hash,
            &outcome,
        )
        .await?;
        tx.commit().await?;
        drop(permit);
        Ok(outcome)
    }

    /// 将 SQLite 行映射为 SyncRequestLedgerRow。
    fn row_from_sqlite(row: &SqliteRow) -> Result<SyncRequestLedgerRow, AppError> {
        let outcome_json: String = row.try_get("outcome_json")?;
        let outcome: SyncBatchOutcome = serde_json::from_str(&outcome_json)?;
        Ok(SyncRequestLedgerRow {
            claimed_device_id: row.try_get("claimed_device_id")?,
            domain: row.try_get("domain")?,
            client_request_id: row.try_get("client_request_id")?,
            payload_hash: row.try_get("payload_hash")?,
            outcome,
            created_at: row.try_get("created_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    //! sync_request_ledger 单测：claim / 重放 / 不同 hash 冲突 / conflict_row_id 确定性。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存库并建 ledger 表。
    async fn setup_repo() -> SyncRequestLedgerRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        SyncRequestLedgerRepo::ensure_schema(&pool).await.unwrap();
        SyncRequestLedgerRepo::new(pool)
    }

    /// 首次 apply 写入 outcome；同 key/同 hash 重放返回原 outcome 且 apply 不再执行。
    #[tokio::test]
    async fn same_key_same_hash_replays_recorded_outcome_without_reapply() {
        use futures_util::FutureExt;
        let repo = setup_repo().await;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_ref = calls.clone();
        let outcome1 = repo
            .apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-1", "hash-aaa", |tx| {
                let calls = calls_ref.clone();
                async move {
                    let _ = tx;
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(SyncBatchOutcome { accepted: 3 })
                }
                .boxed()
            })
            .await
            .unwrap();
        assert_eq!(outcome1.accepted, 3);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let calls_ref2 = calls.clone();
        let outcome2 = repo
            .apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-1", "hash-aaa", |_tx| {
                let calls = calls_ref2.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(SyncBatchOutcome { accepted: 99 })
                }
                .boxed()
            })
            .await
            .unwrap();
        assert_eq!(outcome2.accepted, 3);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "重放不得再次 apply"
        );
    }

    /// 同 key 不同 payload hash → conflict。
    #[tokio::test]
    async fn same_key_different_hash_is_conflict() {
        use futures_util::FutureExt;
        let repo = setup_repo().await;
        repo.apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-1", "hash-aaa", |_tx| {
            async { Ok(SyncBatchOutcome { accepted: 1 }) }.boxed()
        })
        .await
        .unwrap();

        let err = repo
            .apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-1", "hash-bbb", |_tx| {
                async { Ok(SyncBatchOutcome { accepted: 2 }) }.boxed()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, AppError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
    }

    /// apply 失败时 ledger 不得留下半成品记录，重试可再次 claim。
    #[tokio::test]
    async fn apply_failure_rolls_back_ledger_claim() {
        use futures_util::FutureExt;
        let repo = setup_repo().await;
        let err = repo
            .apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-fail", "h1", |_tx| {
                async { Err(AppError::generic("inject apply fail")) }.boxed()
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Bad(_)));

        let outcome = repo
            .apply_batch_idempotent("peer-a", DOMAIN_PROMPTS, "req-fail", "h1", |_tx| {
                async { Ok(SyncBatchOutcome { accepted: 5 }) }.boxed()
            })
            .await
            .unwrap();
        assert_eq!(outcome.accepted, 5);
    }

    /// conflict_row_id 对相同输入稳定，对不同输入不同。
    #[test]
    fn conflict_row_id_is_deterministic() {
        let a = SyncRequestLedgerRepo::conflict_row_id("prompts", "id-1", "d1", "abc");
        let b = SyncRequestLedgerRepo::conflict_row_id("prompts", "id-1", "d1", "abc");
        let c = SyncRequestLedgerRepo::conflict_row_id("prompts", "id-1", "d1", "xyz");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }
}
