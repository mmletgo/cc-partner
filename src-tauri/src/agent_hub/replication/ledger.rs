//! agent_hub/replication/ledger — LAN push 幂等请求/对象 outcome 表
//!
//! Business Logic（为什么需要这个模块）:
//!     同一 (sourceDeviceId, clientRequestId) 必须收敛：相同 selection+snapshot 重放原 outcome；
//!     不同 hash 冲突；无效 manifest 不得写入 active ledger。对象 verified 位支持断点复用。
//!
//! Code Logic（这个模块做什么）:
//!     表 agent_hub_push_requests / agent_hub_push_objects；append-only 语义的 upsert；
//!     sourceDeviceId/clientRequestId 仅为幂等标签，**不是**身份认证。

use crate::error::AppError;
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::sync::Arc;

/// 未验证 staging 保留时长（24h）；超时由 GC 删除，已验证 CAS 保留。
pub const MAX_STAGING_AGE: Duration = Duration::hours(24);

/// push 请求状态。
///
/// Business Logic: prepared 表示可收 object；committed 表示 import 已原子完成。
/// Code Logic: 存库字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PushRequestStatus {
    /// prepare 成功，等待 objects/commit
    Prepared,
    /// commit 成功，outcome 已持久化
    Committed,
}

impl PushRequestStatus {
    /// 解析存库字符串。
    ///
    /// Business Logic: 未知状态 fail-closed。
    /// Code Logic: prepared/committed 精确匹配。
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "prepared" => Ok(Self::Prepared),
            "committed" => Ok(Self::Committed),
            other => Err(AppError::generic(format!(
                "agent_hub_push_status_unknown:{other}"
            ))),
        }
    }

    /// 存库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
        }
    }
}

/// 一条 push 请求 ledger 行。
///
/// Business Logic: 绑定 source/request 与 selection/snapshot hash 及 transfer。
/// Code Logic: camelCase 可序列化字段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushRequestRow {
    /// 源设备 id（幂等标签，非认证）
    pub source_device_id: String,
    /// 客户端请求 id
    pub client_request_id: String,
    /// transfer id（staging 目录键）
    pub transfer_id: String,
    /// selection canonical SHA-256
    pub selection_hash: String,
    /// envelope.snapshotHash
    pub snapshot_hash: String,
    /// prepared | committed
    pub status: PushRequestStatus,
    /// prepare 时完整 envelope JSON（commit 重校验）
    pub envelope_json: String,
    /// commit outcome JSON（committed 时非空）
    pub outcome_json: Option<String>,
    /// 创建时间 RFC3339
    pub created_at: String,
    /// 更新时间 RFC3339
    pub updated_at: String,
}

/// 传输内 object 进度行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushObjectRow {
    pub transfer_id: String,
    pub object_hash: String,
    pub expected_size: u64,
    pub received_bytes: u64,
    /// 对象 SHA-256 已与 declared hash 对齐
    pub verified: bool,
    pub updated_at: String,
}

/// Replication ledger 访问器。
///
/// Business Logic: 与 AgentHubRepo 共享 maintenance gate，避免与 restore exclusive 冲突。
/// Code Logic: pool + gate。
#[derive(Clone)]
pub struct ReplicationLedger {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl ReplicationLedger {
    /// 构造 ledger。
    ///
    /// Business Logic: 生产路径与 AgentHubRepo 共享 gate。
    /// Code Logic: 保存 clone。
    pub fn new(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// 测试用独立 gate。
    pub fn new_standalone(pool: SqlitePool) -> Self {
        Self::new(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 底层 pool（测试）。
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// 按 (source, request) 读取请求。
    ///
    /// Business Logic: prepare 幂等查询入口。
    /// Code Logic: SELECT optional。
    pub async fn get_request(
        &self,
        source_device_id: &str,
        client_request_id: &str,
    ) -> Result<Option<PushRequestRow>, AppError> {
        let row = sqlx::query(
            "SELECT source_device_id, client_request_id, transfer_id, selection_hash, snapshot_hash,
                    status, envelope_json, outcome_json, created_at, updated_at
             FROM agent_hub_push_requests
             WHERE source_device_id = ? AND client_request_id = ?",
        )
        .bind(source_device_id)
        .bind(client_request_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_request).transpose()
    }

    /// 按 transfer_id 读取请求。
    ///
    /// Business Logic: object/commit 路径绑定 transfer。
    /// Code Logic: SELECT by transfer_id。
    pub async fn get_request_by_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<Option<PushRequestRow>, AppError> {
        let row = sqlx::query(
            "SELECT source_device_id, client_request_id, transfer_id, selection_hash, snapshot_hash,
                    status, envelope_json, outcome_json, created_at, updated_at
             FROM agent_hub_push_requests
             WHERE transfer_id = ?",
        )
        .bind(transfer_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_request).transpose()
    }

    /// 首次写入 prepared 请求（UNIQUE 冲突上抛）。
    ///
    /// Business Logic: 仅在校验通过后创建 active ledger。
    /// Code Logic: INSERT；unique → Conflict（调用方应 re-get 并按 hash 决定 replay/conflict）。
    pub async fn insert_prepared(
        &self,
        source_device_id: &str,
        client_request_id: &str,
        transfer_id: &str,
        selection_hash: &str,
        snapshot_hash: &str,
        envelope_json: &str,
    ) -> Result<PushRequestRow, AppError> {
        self.insert_prepared_with_objects(
            source_device_id,
            client_request_id,
            transfer_id,
            selection_hash,
            snapshot_hash,
            envelope_json,
            &[],
        )
        .await
    }

    /// 同一 SQLite 写事务写入 prepared 请求 + object 登记行。
    ///
    /// Business Logic:
    ///     prepare 中途崩溃不得留下「有 request 无 object 行」的半截 active ledger；
    ///     UNIQUE 冲突上抛 `agent_hub_push_request_conflict`，由调用方 re-get 判定
    ///     same-hash replay 或不同 hash conflict。
    ///
    /// Code Logic:
    ///     shared lease → BEGIN → INSERT request → INSERT objects → COMMIT；
    ///     unique → rollback + Conflict；object 用 INSERT OR IGNORE 保幂等。
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_prepared_with_objects(
        &self,
        source_device_id: &str,
        client_request_id: &str,
        transfer_id: &str,
        selection_hash: &str,
        snapshot_hash: &str,
        envelope_json: &str,
        objects: &[(String, u64)],
    ) -> Result<PushRequestRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = Utc::now().to_rfc3339();
            let mut tx = self.pool.begin().await?;
            let result = sqlx::query(
                "INSERT INTO agent_hub_push_requests
                 (source_device_id, client_request_id, transfer_id, selection_hash, snapshot_hash,
                  status, envelope_json, outcome_json, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, 'prepared', ?, NULL, ?, ?)",
            )
            .bind(source_device_id)
            .bind(client_request_id)
            .bind(transfer_id)
            .bind(selection_hash)
            .bind(snapshot_hash)
            .bind(envelope_json)
            .bind(&now)
            .bind(&now)
            .execute(&mut *tx)
            .await;
            match result {
                Ok(_) => {
                    for (object_hash, expected_size) in objects {
                        sqlx::query(
                            "INSERT OR IGNORE INTO agent_hub_push_objects
                             (transfer_id, object_hash, expected_size, received_bytes, verified, updated_at)
                             VALUES (?, ?, ?, 0, 0, ?)",
                        )
                        .bind(transfer_id)
                        .bind(object_hash)
                        .bind(*expected_size as i64)
                        .bind(&now)
                        .execute(&mut *tx)
                        .await?;
                    }
                    tx.commit().await?;
                    Ok(PushRequestRow {
                        source_device_id: source_device_id.to_string(),
                        client_request_id: client_request_id.to_string(),
                        transfer_id: transfer_id.to_string(),
                        selection_hash: selection_hash.to_string(),
                        snapshot_hash: snapshot_hash.to_string(),
                        status: PushRequestStatus::Prepared,
                        envelope_json: envelope_json.to_string(),
                        outcome_json: None,
                        created_at: now.clone(),
                        updated_at: now,
                    })
                }
                Err(e) => {
                    // 显式 rollback（Drop 也会，但错误路径更清晰）
                    let _ = tx.rollback().await;
                    if is_unique_violation(&e) {
                        Err(AppError::conflict(
                            "agent_hub_push_request_conflict".to_string(),
                        ))
                    } else {
                        Err(e.into())
                    }
                }
            }
        })
        .await
    }

    /// UNIQUE 冲突后 re-get：同 hash 返回 existing；不同 hash 保持 conflict。
    ///
    /// Business Logic: 并发同 key prepare 必须收敛到 prior outcome，不得硬冲突。
    /// Code Logic: get_request；hash 匹配 → Ok(Some)；不匹配 → Conflict；无行 → Ok(None)。
    pub async fn resolve_after_insert_conflict(
        &self,
        source_device_id: &str,
        client_request_id: &str,
        selection_hash: &str,
        snapshot_hash: &str,
    ) -> Result<Option<PushRequestRow>, AppError> {
        match self
            .get_request(source_device_id, client_request_id)
            .await?
        {
            Some(existing)
                if existing.selection_hash == selection_hash
                    && existing.snapshot_hash == snapshot_hash =>
            {
                Ok(Some(existing))
            }
            Some(_) => Err(AppError::conflict(
                "agent_hub_push_idempotency_hash_conflict".to_string(),
            )),
            None => Ok(None),
        }
    }

    /// 标记 committed 并写入 outcome JSON。
    ///
    /// Business Logic: 仅 prepared→committed；已 committed 同 outcome 幂等。
    /// Code Logic: UPDATE WHERE status=prepared OR 已 committed 同 hash。
    pub async fn mark_committed(
        &self,
        source_device_id: &str,
        client_request_id: &str,
        outcome_json: &str,
    ) -> Result<PushRequestRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = Utc::now().to_rfc3339();
            let updated = sqlx::query(
                "UPDATE agent_hub_push_requests
                 SET status = 'committed', outcome_json = ?, updated_at = ?
                 WHERE source_device_id = ? AND client_request_id = ?
                   AND (status = 'prepared'
                        OR (status = 'committed' AND outcome_json = ?))",
            )
            .bind(outcome_json)
            .bind(&now)
            .bind(source_device_id)
            .bind(client_request_id)
            .bind(outcome_json)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if updated == 0 {
                // 已 committed 且 outcome 不同，或行不存在
                return Err(AppError::conflict(
                    "agent_hub_push_commit_state_conflict".to_string(),
                ));
            }
            self.get_request(source_device_id, client_request_id)
                .await?
                .ok_or_else(|| AppError::not_found("agent_hub_push_request_missing".to_string()))
        })
        .await
    }

    /// 登记/刷新 object 期望 size（prepare 时批量）。
    ///
    /// Business Logic: 仅 missing 对象写入；已 verified 保留。
    /// Code Logic: INSERT OR IGNORE + size 一致性校验。
    pub async fn ensure_object(
        &self,
        transfer_id: &str,
        object_hash: &str,
        expected_size: u64,
    ) -> Result<PushObjectRow, AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = Utc::now().to_rfc3339();
            if let Some(existing) = self.get_object(transfer_id, object_hash).await? {
                if existing.expected_size != expected_size {
                    return Err(AppError::conflict(format!(
                        "agent_hub_push_object_size_conflict:{object_hash}"
                    )));
                }
                return Ok(existing);
            }
            sqlx::query(
                "INSERT INTO agent_hub_push_objects
                 (transfer_id, object_hash, expected_size, received_bytes, verified, updated_at)
                 VALUES (?, ?, ?, 0, 0, ?)",
            )
            .bind(transfer_id)
            .bind(object_hash)
            .bind(expected_size as i64)
            .bind(&now)
            .execute(&self.pool)
            .await?;
            Ok(PushObjectRow {
                transfer_id: transfer_id.to_string(),
                object_hash: object_hash.to_string(),
                expected_size,
                received_bytes: 0,
                verified: false,
                updated_at: now,
            })
        })
        .await
    }

    /// 读取 object 进度。
    pub async fn get_object(
        &self,
        transfer_id: &str,
        object_hash: &str,
    ) -> Result<Option<PushObjectRow>, AppError> {
        let row = sqlx::query(
            "SELECT transfer_id, object_hash, expected_size, received_bytes, verified, updated_at
             FROM agent_hub_push_objects
             WHERE transfer_id = ? AND object_hash = ?",
        )
        .bind(transfer_id)
        .bind(object_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| PushObjectRow {
            transfer_id: r.get("transfer_id"),
            object_hash: r.get("object_hash"),
            expected_size: r.get::<i64, _>("expected_size") as u64,
            received_bytes: r.get::<i64, _>("received_bytes") as u64,
            verified: r.get::<i64, _>("verified") != 0,
            updated_at: r.get("updated_at"),
        }))
    }

    /// 更新 object 接收进度。
    ///
    /// Business Logic: verified 后不得回退。
    /// Code Logic: UPDATE received_bytes / verified。
    pub async fn update_object_progress(
        &self,
        transfer_id: &str,
        object_hash: &str,
        received_bytes: u64,
        verified: bool,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            let now = Utc::now().to_rfc3339();
            let n = sqlx::query(
                "UPDATE agent_hub_push_objects
                 SET received_bytes = ?, verified = ?, updated_at = ?
                 WHERE transfer_id = ? AND object_hash = ?
                   AND (verified = 0 OR ? = 1)",
            )
            .bind(received_bytes as i64)
            .bind(if verified { 1i64 } else { 0i64 })
            .bind(&now)
            .bind(transfer_id)
            .bind(object_hash)
            .bind(if verified { 1i64 } else { 0i64 })
            .execute(&self.pool)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(AppError::not_found(format!(
                    "agent_hub_push_object_missing:{object_hash}"
                )));
            }
            Ok(())
        })
        .await
    }

    /// 列出 transfer 下全部 object 行。
    pub async fn list_objects(&self, transfer_id: &str) -> Result<Vec<PushObjectRow>, AppError> {
        let rows = sqlx::query(
            "SELECT transfer_id, object_hash, expected_size, received_bytes, verified, updated_at
             FROM agent_hub_push_objects WHERE transfer_id = ?
             ORDER BY object_hash ASC",
        )
        .bind(transfer_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| PushObjectRow {
                transfer_id: r.get("transfer_id"),
                object_hash: r.get("object_hash"),
                expected_size: r.get::<i64, _>("expected_size") as u64,
                received_bytes: r.get::<i64, _>("received_bytes") as u64,
                verified: r.get::<i64, _>("verified") != 0,
                updated_at: r.get("updated_at"),
            })
            .collect())
    }

    /// 列出 prepared 且 updated_at 早于 cutoff 的 transfer（供 GC）。
    ///
    /// Business Logic: 仅 abandoned prepared；committed 保留 ledger 行。
    /// Code Logic: SELECT transfer_id WHERE status=prepared AND updated_at < cutoff。
    pub async fn list_stale_prepared_transfers(
        &self,
        cutoff_rfc3339: &str,
    ) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT transfer_id FROM agent_hub_push_requests
             WHERE status = 'prepared' AND updated_at < ?
             ORDER BY transfer_id ASC",
        )
        .bind(cutoff_rfc3339)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("transfer_id"))
            .collect())
    }

    /// 删除 transfer 的 object 行与请求行（GC staging 后）。
    ///
    /// Business Logic: 只删 prepared；verified CAS 不在此删。
    /// Code Logic: DELETE objects + request。
    pub async fn delete_prepared_transfer(&self, transfer_id: &str) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "DELETE FROM agent_hub_push_objects WHERE transfer_id = ?
                 AND transfer_id IN (
                    SELECT transfer_id FROM agent_hub_push_requests
                    WHERE transfer_id = ? AND status = 'prepared'
                 )",
            )
            .bind(transfer_id)
            .bind(transfer_id)
            .execute(&self.pool)
            .await?;
            sqlx::query(
                "DELETE FROM agent_hub_push_requests
                 WHERE transfer_id = ? AND status = 'prepared'",
            )
            .bind(transfer_id)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }
}

fn row_to_request(r: sqlx::sqlite::SqliteRow) -> Result<PushRequestRow, AppError> {
    let status_raw: String = r.get("status");
    Ok(PushRequestRow {
        source_device_id: r.get("source_device_id"),
        client_request_id: r.get("client_request_id"),
        transfer_id: r.get("transfer_id"),
        selection_hash: r.get("selection_hash"),
        snapshot_hash: r.get("snapshot_hash"),
        status: PushRequestStatus::parse(&status_raw)?,
        envelope_json: r.get("envelope_json"),
        outcome_json: r.get("outcome_json"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            let code = db.code().map(|c| c.to_string()).unwrap_or_default();
            code == "2067" || code == "1555" || db.message().contains("UNIQUE")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::AgentHubRepo;

    async fn test_ledger() -> ReplicationLedger {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        ReplicationLedger::new_standalone(pool)
    }

    #[tokio::test]
    async fn insert_prepared_and_idempotent_lookup() {
        let ledger = test_ledger().await;
        let row = ledger
            .insert_prepared(
                "src-a",
                "req-1",
                "xfer-1",
                "sel-hash",
                "snap-hash",
                r#"{"format":"cc-partner-agent-hub"}"#,
            )
            .await
            .unwrap();
        assert_eq!(row.status, PushRequestStatus::Prepared);
        let again = ledger.get_request("src-a", "req-1").await.unwrap().unwrap();
        assert_eq!(again.transfer_id, "xfer-1");
        let err = ledger
            .insert_prepared("src-a", "req-1", "xfer-2", "other", "other", "{}")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("conflict") || err.to_string().contains("Conflict"));
    }

    #[tokio::test]
    async fn mark_committed_persists_outcome() {
        let ledger = test_ledger().await;
        ledger
            .insert_prepared("s", "r", "t", "sel", "snap", "{}")
            .await
            .unwrap();
        let committed = ledger
            .mark_committed("s", "r", r#"{"ok":true}"#)
            .await
            .unwrap();
        assert_eq!(committed.status, PushRequestStatus::Committed);
        assert_eq!(committed.outcome_json.as_deref(), Some(r#"{"ok":true}"#));
        // 同 outcome 再 mark 成功
        let again = ledger
            .mark_committed("s", "r", r#"{"ok":true}"#)
            .await
            .unwrap();
        assert_eq!(again.status, PushRequestStatus::Committed);
    }

    /// Business Logic: 请求与 object 行必须同事务；UNIQUE 后 re-get 同 hash 可 replay。
    /// Code Logic: insert_prepared_with_objects 登记 objects；二次 insert conflict 经
    /// resolve_after_insert_conflict 返回 first transfer。
    #[tokio::test]
    async fn insert_prepared_with_objects_atomic_and_unique_reread() {
        let ledger = test_ledger().await;
        let objects = vec![("a".repeat(64), 12u64), ("b".repeat(64), 34u64)];
        let row = ledger
            .insert_prepared_with_objects(
                "src-a",
                "req-u",
                "xfer-1",
                "sel-hash",
                "snap-hash",
                r#"{"format":"cc-partner-agent-hub"}"#,
                &objects,
            )
            .await
            .unwrap();
        assert_eq!(row.transfer_id, "xfer-1");
        let listed = ledger.list_objects("xfer-1").await.unwrap();
        assert_eq!(listed.len(), 2);

        let err = ledger
            .insert_prepared_with_objects(
                "src-a",
                "req-u",
                "xfer-2",
                "sel-hash",
                "snap-hash",
                r#"{"format":"cc-partner-agent-hub"}"#,
                &objects,
            )
            .await
            .unwrap_err();
        assert_eq!(err.ipc_category_code(), "conflict");

        let resolved = ledger
            .resolve_after_insert_conflict("src-a", "req-u", "sel-hash", "snap-hash")
            .await
            .unwrap()
            .expect("same-hash unique should re-read existing");
        assert_eq!(resolved.transfer_id, "xfer-1");

        let differ = ledger
            .resolve_after_insert_conflict("src-a", "req-u", "other-sel", "other-snap")
            .await
            .unwrap_err();
        assert_eq!(differ.ipc_category_code(), "conflict");
    }
}
