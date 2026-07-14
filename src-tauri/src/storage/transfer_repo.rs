//! storage/transfer_repo.rs — transfer_history 表 CRUD
//!
//! Business Logic（为什么需要这个模块）:
//!     传输历史（含已结束的发送/接收任务）需持久化，供前端传输面板 `list_transfers`
//!     返回"活跃任务 + 历史"合并列表。N5 起还需持久化 phase/failure、attempt 与
//!     logical/attempt/protocol/client_operation 身份，支撑 retry/resume/对账。
//!     表 schema 由 runtime CREATE IF NOT EXISTS + 本模块 ensure_schema 幂等升级。
//!
//! Code Logic（这个模块做什么）:
//!     - `ensure_schema`：PRAGMA table_info + ALTER ADD COLUMN + 部分唯一索引
//!     - `record(task)`：shared write lease 下按 id upsert（ON CONFLICT(id) DO UPDATE）；
//!       client_operation_id 唯一冲突上抛，绝不 REPLACE 掉其它 attempt
//!     - `list()` / `get_by_id()`：SELECT 全列；legacy NULL 身份 coalesce 到 id、attempt 默认 1
//!     - `update_status(...)`：shared write lease 下更新某任务的状态/进度/完成时间

use crate::error::AppError;
use crate::models::transfer::{
    TransferDirection, TransferFailure, TransferFailureStage, TransferPhase, TransferStatus,
    TransferTask,
};
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::sync::Arc;

/// SELECT 列清单（含 recovery 字段）。
const TRANSFER_SELECT_COLUMNS: &str = "id, filename, file_path, size, sha256, direction, \
     peer_device_id, status, transferred_bytes, created_at, completed_at, \
     phase, failure_stage, failure_code, failure_retryable, failure_message, \
     attempt, logical_transfer_id, attempt_id, protocol_transfer_id, \
     client_operation_id, operation_payload_hash";

/// 传输历史仓库。
pub struct TransferRepo {
    db: SqlitePool,
    /// 维护屏障：写路径持 shared lease，restore exclusive 时阻塞。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl TransferRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: 单测无需共享 AppState.maintenance_gate。
    /// Code Logic: 内部 `with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 全部 ordinary writer 与 restore 共用同一 gate。
    /// Code Logic: 保存 pool + Arc gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 幂等升级 transfer_history：补 recovery 列与 client_operation_id 唯一索引。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧库只有 M5 基础列；N5 recovery 需要 phase/failure/attempt/身份列，且
    ///     clientOperationId 全局唯一。升级必须对已有库与重复启动幂等，且不用 sqlx::migrate!。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CREATE TABLE IF NOT EXISTS 保底 → PRAGMA table_info 缺列则 ALTER ADD COLUMN →
    ///     创建部分唯一索引 `idx_transfer_history_client_operation_id`。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
        )
        .execute(pool)
        .await?;

        let columns = sqlx::query("PRAGMA table_info(transfer_history)")
            .fetch_all(pool)
            .await?;
        let names: Vec<String> = columns
            .iter()
            .filter_map(|row| row.try_get::<String, _>("name").ok())
            .collect();

        /// 缺列时 ALTER ADD；已存在则跳过。
        async fn add_column_if_missing(
            pool: &SqlitePool,
            names: &[String],
            name: &str,
            ddl_suffix: &str,
        ) -> Result<(), AppError> {
            if names.iter().any(|n| n == name) {
                return Ok(());
            }
            let sql = format!("ALTER TABLE transfer_history ADD COLUMN {ddl_suffix}");
            sqlx::query(&sql).execute(pool).await?;
            Ok(())
        }

        add_column_if_missing(pool, &names, "phase", "phase TEXT").await?;
        add_column_if_missing(pool, &names, "failure_stage", "failure_stage TEXT").await?;
        add_column_if_missing(pool, &names, "failure_code", "failure_code TEXT").await?;
        add_column_if_missing(pool, &names, "failure_retryable", "failure_retryable INTEGER")
            .await?;
        add_column_if_missing(pool, &names, "failure_message", "failure_message TEXT").await?;
        add_column_if_missing(
            pool,
            &names,
            "attempt",
            "attempt INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        add_column_if_missing(pool, &names, "logical_transfer_id", "logical_transfer_id TEXT")
            .await?;
        add_column_if_missing(pool, &names, "attempt_id", "attempt_id TEXT").await?;
        add_column_if_missing(
            pool,
            &names,
            "protocol_transfer_id",
            "protocol_transfer_id TEXT",
        )
        .await?;
        add_column_if_missing(pool, &names, "client_operation_id", "client_operation_id TEXT")
            .await?;
        add_column_if_missing(
            pool,
            &names,
            "operation_payload_hash",
            "operation_payload_hash TEXT",
        )
        .await?;

        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_transfer_history_client_operation_id \
             ON transfer_history(client_operation_id) \
             WHERE client_operation_id IS NOT NULL",
        )
        .execute(pool)
        .await?;

        Ok(())
    }

    /// 将一行映射为 TransferTask；legacy 列缺失时给 recovery 默认值。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧行没有 attempt/身份/phase；读出后必须可被 retry 路径安全消费。
    ///
    /// Code Logic（这个函数做什么）:
    ///     try_get 各列；attempt 默认 1；logical/attempt/protocol id coalesce 到 id；
    ///     phase 未知 → None（不映射 Failed）；failure 四列齐全时组装 TransferFailure。
    fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> TransferTask {
        let direction_str: String = row.try_get("direction").unwrap_or_default();
        let status_str: String = row.try_get("status").unwrap_or_default();
        let transferred: i64 = row.try_get("transferred_bytes").unwrap_or(0);
        let id: String = row.try_get("id").unwrap_or_default();
        let status = TransferStatus::from_str_lossy(&status_str);

        let phase = row
            .try_get::<Option<String>, _>("phase")
            .ok()
            .flatten()
            .and_then(|s| TransferPhase::parse_optional(&s));

        let failure = {
            let stage: Option<String> = row.try_get("failure_stage").ok().flatten();
            let code: Option<String> = row.try_get("failure_code").ok().flatten();
            let retryable: Option<i64> = row.try_get("failure_retryable").ok().flatten();
            let message: Option<String> = row.try_get("failure_message").ok().flatten();
            match (stage, code, retryable, message) {
                (Some(stage), Some(code), Some(retryable), Some(message)) => Some(TransferFailure {
                    stage: TransferFailureStage::from_str_lossy(&stage),
                    code,
                    retryable: retryable != 0,
                    message,
                }),
                _ => None,
            }
        };

        let attempt = row
            .try_get::<Option<i64>, _>("attempt")
            .ok()
            .flatten()
            .map(|v| v.max(1) as u32)
            .unwrap_or(1);

        let coalesce_id = |col: &str| -> String {
            row.try_get::<Option<String>, _>(col)
                .ok()
                .flatten()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone())
        };

        let mut task = TransferTask {
            id: id.clone(),
            filename: row.try_get("filename").unwrap_or_default(),
            file_path: row.try_get("file_path").unwrap_or_default(),
            size: row.try_get::<i64, _>("size").unwrap_or(0) as u64,
            sha256: row.try_get("sha256").unwrap_or_default(),
            // chunk_size 不在表中，用默认 960KB
            chunk_size: 960 * 1024,
            direction: TransferDirection::from_str_lossy(&direction_str),
            peer_device_id: row.try_get("peer_device_id").unwrap_or_default(),
            status,
            transferred_bytes: transferred as u64,
            created_at: row.try_get("created_at").unwrap_or_default(),
            completed_at: row.try_get("completed_at").unwrap_or(None),
            phase,
            failure,
            attempt,
            logical_transfer_id: coalesce_id("logical_transfer_id"),
            attempt_id: coalesce_id("attempt_id"),
            protocol_transfer_id: coalesce_id("protocol_transfer_id"),
            client_operation_id: row.try_get("client_operation_id").ok().flatten(),
            operation_payload_hash: row.try_get("operation_payload_hash").ok().flatten(),
        };
        task.normalize_recovery_identity();
        task
    }

    /// 写入一条历史（按 id upsert，终态任务落库，含 recovery 字段）。
    ///
    /// Business Logic: 发送/接收终态后必须 durable 落库，供重启后 complete/status 收敛与 retry 对账。
    ///     同一 clientOperationId 不得被另一 task id 静默覆盖。
    /// Code Logic: 持 shared write lease 后 INSERT ... ON CONFLICT(id) DO UPDATE 全列。
    pub async fn record(&self, task: &TransferTask) -> Result<(), AppError> {
        let direction_str = task.direction.as_str();
        let status_str = task.status.as_str();
        let phase_str = task.phase.map(TransferPhase::as_str);
        let failure_stage = task.failure.as_ref().map(|f| f.stage.as_str());
        let failure_code = task.failure.as_ref().map(|f| f.code.as_str());
        let failure_retryable = task.failure.as_ref().map(|f| if f.retryable { 1_i64 } else { 0_i64 });
        let failure_message = task.failure.as_ref().map(|f| f.message.as_str());
        let attempt = task.attempt.max(1) as i64;
        let logical_transfer_id = if task.logical_transfer_id.is_empty() {
            task.id.as_str()
        } else {
            task.logical_transfer_id.as_str()
        };
        let attempt_id = if task.attempt_id.is_empty() {
            task.id.as_str()
        } else {
            task.attempt_id.as_str()
        };
        let protocol_transfer_id = if task.protocol_transfer_id.is_empty() {
            task.id.as_str()
        } else {
            task.protocol_transfer_id.as_str()
        };

        with_shared_write_lease(&self.gate, async {
            // 仅按主键 id upsert；client_operation_id 唯一冲突必须上抛，不能 REPLACE 删掉旧 attempt。
            sqlx::query(
                "INSERT INTO transfer_history \
                 (id, filename, file_path, size, sha256, direction, peer_device_id, status, \
                  transferred_bytes, created_at, completed_at, \
                  phase, failure_stage, failure_code, failure_retryable, failure_message, \
                  attempt, logical_transfer_id, attempt_id, protocol_transfer_id, \
                  client_operation_id, operation_payload_hash) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                   filename = excluded.filename, \
                   file_path = excluded.file_path, \
                   size = excluded.size, \
                   sha256 = excluded.sha256, \
                   direction = excluded.direction, \
                   peer_device_id = excluded.peer_device_id, \
                   status = excluded.status, \
                   transferred_bytes = excluded.transferred_bytes, \
                   created_at = excluded.created_at, \
                   completed_at = excluded.completed_at, \
                   phase = excluded.phase, \
                   failure_stage = excluded.failure_stage, \
                   failure_code = excluded.failure_code, \
                   failure_retryable = excluded.failure_retryable, \
                   failure_message = excluded.failure_message, \
                   attempt = excluded.attempt, \
                   logical_transfer_id = excluded.logical_transfer_id, \
                   attempt_id = excluded.attempt_id, \
                   protocol_transfer_id = excluded.protocol_transfer_id, \
                   client_operation_id = excluded.client_operation_id, \
                   operation_payload_hash = excluded.operation_payload_hash",
            )
            .bind(&task.id)
            .bind(&task.filename)
            .bind(&task.file_path)
            .bind(task.size as i64)
            .bind(&task.sha256)
            .bind(direction_str)
            .bind(&task.peer_device_id)
            .bind(status_str)
            .bind(task.transferred_bytes as i64)
            .bind(&task.created_at)
            .bind(&task.completed_at)
            .bind(phase_str)
            .bind(failure_stage)
            .bind(failure_code)
            .bind(failure_retryable)
            .bind(failure_message)
            .bind(attempt)
            .bind(logical_transfer_id)
            .bind(attempt_id)
            .bind(protocol_transfer_id)
            .bind(&task.client_operation_id)
            .bind(&task.operation_payload_hash)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 列出全部历史（按 created_at 倒序）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     传输面板合并活跃+历史时需要全量历史倒序列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 全列 ORDER BY created_at DESC，映射为 TransferTask。
    pub async fn list(&self) -> Result<Vec<TransferTask>, AppError> {
        let sql = format!(
            "SELECT {TRANSFER_SELECT_COLUMNS} FROM transfer_history ORDER BY created_at DESC"
        );
        let rows = sqlx::query(&sql).fetch_all(&self.db).await?;
        Ok(rows.iter().map(Self::row_to_task).collect())
    }

    /// 按 id 查询单条传输历史（接收端 complete/status 跨重启收敛用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     接收端 finalize 后 registry/墓碑都只在内存；进程重启后 complete/status 若只查内存，
    ///     会把已落盘的 Receive 任务误报为 unknown/success=false，发送端假失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE id=?`，命中则映射为 `TransferTask`；无行返回 `Ok(None)`。
    pub async fn get_by_id(&self, id: &str) -> Result<Option<TransferTask>, AppError> {
        let sql = format!(
            "SELECT {TRANSFER_SELECT_COLUMNS} FROM transfer_history WHERE id = ?"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.db)
            .await?;
        Ok(row.map(|r| Self::row_to_task(&r)))
    }

    /// 更新某任务的状态/进度/完成时间。
    ///
    /// Business Logic: 断点续传等场景可能需要局部改状态。
    /// Code Logic: 持 shared write lease 后 UPDATE。
    #[allow(dead_code)]
    pub async fn update_status(
        &self,
        id: &str,
        status: TransferStatus,
        transferred_bytes: u64,
        completed_at: Option<&str>,
    ) -> Result<(), AppError> {
        let status_str = status.as_str();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE transfer_history SET status = ?, transferred_bytes = ?, completed_at = ? WHERE id = ?",
            )
            .bind(status_str)
            .bind(transferred_bytes as i64)
            .bind(completed_at)
            .bind(id)
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 按发送端全局 clientOperationId 查询 attempt 行。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     幂等重放与对账都只认 clientOperationId，不认 requestId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `SELECT ... WHERE client_operation_id=?`；无行 `Ok(None)`。
    pub async fn get_by_client_operation_id(
        &self,
        client_operation_id: &str,
    ) -> Result<Option<TransferTask>, AppError> {
        let id = client_operation_id.trim();
        if id.is_empty() {
            return Ok(None);
        }
        let sql = format!(
            "SELECT {TRANSFER_SELECT_COLUMNS} FROM transfer_history WHERE client_operation_id = ?"
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.db)
            .await?;
        Ok(row.map(|r| Self::row_to_task(&r)))
    }

    /// 统计同一 logical_transfer_id 的 attempt 行数（含历史终态）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     并发幂等测试需要断言“同 op 只创建一个 attempt 增量”。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `COUNT(*) WHERE logical_transfer_id=?`。
    pub async fn count_attempts_for_logical(
        &self,
        logical_transfer_id: &str,
    ) -> Result<u32, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS c FROM transfer_history WHERE logical_transfer_id = ?",
        )
        .bind(logical_transfer_id)
        .fetch_one(&self.db)
        .await?;
        let c: i64 = row.try_get("c").unwrap_or(0);
        Ok(c.max(0) as u32)
    }

    /// 列出启动可恢复的 insert-before-spawn 行（Queued + Pending + Send + 有 client_operation_id）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     claim 落库后 spawn 前 crash 时，owner 重启必须恢复这些行，避免永远卡在 Queued。
    ///
    /// Code Logic（这个函数做什么）:
    ///     查 phase=queued（或 NULL 且 status=pending）且 direction=send 且 client_operation_id 非空。
    pub async fn list_recoverable_queued_sends(&self) -> Result<Vec<TransferTask>, AppError> {
        let sql = format!(
            "SELECT {TRANSFER_SELECT_COLUMNS} FROM transfer_history \
             WHERE direction = 'send' \
               AND status = 'pending' \
               AND client_operation_id IS NOT NULL \
               AND (phase = 'queued' OR phase IS NULL OR phase = '') \
             ORDER BY created_at ASC"
        );
        let rows = sqlx::query(&sql).fetch_all(&self.db).await?;
        Ok(rows.iter().map(Self::row_to_task).collect())
    }

    /// 发送端事务 claim：全局唯一 clientOperationId + payload hash。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同一用户意图（retry/resume）可能因超时被重复提交；必须保证只有一个 winner
    ///     能 spawn 网络工作，same id+same hash 回放，different hash 冲突。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) 校验 client_operation_id 非空；2) 已有行同 hash → Replay，异 hash → Conflict；
    ///     3) 无行 INSERT（unique index 并发下 rows_affected/冲突回读）；
    ///     4) Fresh 返回已落库 Queued 快照。
    pub async fn claim_sender_operation(
        &self,
        client_operation_id: &str,
        payload_hash: &str,
        task: &TransferTask,
    ) -> Result<SenderClaimOutcome, AppError> {
        let op_id = normalize_client_operation_id(client_operation_id)?;
        if payload_hash.trim().is_empty() {
            return Err(AppError::validation(
                "operation_payload_hash 不能为空".to_string(),
            ));
        }
        if let Some(existing) = self.get_by_client_operation_id(&op_id).await? {
            return Ok(classify_existing_claim(&existing, payload_hash));
        }

        let mut snapshot = task.clone();
        snapshot.client_operation_id = Some(op_id.clone());
        snapshot.operation_payload_hash = Some(payload_hash.to_string());
        if snapshot.phase.is_none() {
            snapshot.phase = Some(TransferPhase::Queued);
        }
        if snapshot.status != TransferStatus::Pending
            && snapshot.status != TransferStatus::Transferring
        {
            // claim 入口应是 Queued/Pending；防御性归一。
            snapshot.status = TransferStatus::Pending;
            snapshot.phase = Some(TransferPhase::Queued);
        }
        snapshot.normalize_recovery_identity();

        // 仅 INSERT（不用 ON CONFLICT 覆盖其它 attempt）。唯一索引冲突 → 回读分类。
        let insert_result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO transfer_history \
                 (id, filename, file_path, size, sha256, direction, peer_device_id, status, \
                  transferred_bytes, created_at, completed_at, \
                  phase, failure_stage, failure_code, failure_retryable, failure_message, \
                  attempt, logical_transfer_id, attempt_id, protocol_transfer_id, \
                  client_operation_id, operation_payload_hash) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&snapshot.id)
            .bind(&snapshot.filename)
            .bind(&snapshot.file_path)
            .bind(snapshot.size as i64)
            .bind(&snapshot.sha256)
            .bind(snapshot.direction.as_str())
            .bind(&snapshot.peer_device_id)
            .bind(snapshot.status.as_str())
            .bind(snapshot.transferred_bytes as i64)
            .bind(&snapshot.created_at)
            .bind(&snapshot.completed_at)
            .bind(snapshot.phase.map(TransferPhase::as_str))
            .bind(snapshot.failure.as_ref().map(|f| f.stage.as_str()))
            .bind(snapshot.failure.as_ref().map(|f| f.code.as_str()))
            .bind(
                snapshot
                    .failure
                    .as_ref()
                    .map(|f| if f.retryable { 1_i64 } else { 0_i64 }),
            )
            .bind(snapshot.failure.as_ref().map(|f| f.message.as_str()))
            .bind(snapshot.attempt.max(1) as i64)
            .bind(if snapshot.logical_transfer_id.is_empty() {
                snapshot.id.as_str()
            } else {
                snapshot.logical_transfer_id.as_str()
            })
            .bind(if snapshot.attempt_id.is_empty() {
                snapshot.id.as_str()
            } else {
                snapshot.attempt_id.as_str()
            })
            .bind(if snapshot.protocol_transfer_id.is_empty() {
                snapshot.id.as_str()
            } else {
                snapshot.protocol_transfer_id.as_str()
            })
            .bind(&snapshot.client_operation_id)
            .bind(&snapshot.operation_payload_hash)
            .execute(&self.db)
            .await
            .map_err(AppError::from)
        })
        .await;

        match insert_result {
            Ok(_) => {
                let loaded = self
                    .get_by_id(&snapshot.id)
                    .await?
                    .ok_or_else(|| AppError::generic("claim 后读取 attempt 失败"))?;
                Ok(SenderClaimOutcome::Fresh(loaded))
            }
            Err(e) => {
                // 并发 claim：唯一索引冲突 → 回读既有行。
                if let Some(existing) = self.get_by_client_operation_id(&op_id).await? {
                    return Ok(classify_existing_claim(&existing, payload_hash));
                }
                // 也可能是主键 id 冲突（极罕见）；上抛原错误。
                Err(e)
            }
        }
    }
}

/// 发送端 claim 结果。
///
/// Business Logic（为什么需要这个枚举）:
///     调用方必须在 Fresh 时才 spawn；Replay 回放已记录 task；Conflict 拒绝不同 payload。
///
/// Code Logic（这个枚举做什么）:
///     Fresh/Replay 携带 task 快照；Conflict 不携带另一 task 的敏感细节给远程。
#[derive(Debug, Clone)]
pub enum SenderClaimOutcome {
    /// 首次 claim 成功，允许 spawn。
    Fresh(TransferTask),
    /// 同 id + 同 payload，返回既有 attempt。
    Replay(TransferTask),
    /// 同 id + 不同 payload。
    Conflict { existing: TransferTask },
}

/// 规范化 clientOperationId（与 workbench ledger 同约束）。
///
/// Business Logic（为什么需要这个函数）:
///     空/过长/非 ASCII 幂等键会破坏唯一索引语义与跨端回放。
///
/// Code Logic（这个函数做什么）:
///     trim 后 1..=128 可打印 ASCII。
pub fn normalize_client_operation_id(raw: &str) -> Result<String, AppError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(AppError::validation(
            "clientOperationId 不能为空".to_string(),
        ));
    }
    if id.len() > 128 {
        return Err(AppError::validation(
            "clientOperationId 过长（最多 128 字节）".to_string(),
        ));
    }
    if !id.bytes().all(|b| (0x20..=0x7E).contains(&b)) {
        return Err(AppError::validation(
            "clientOperationId 仅允许可打印 ASCII".to_string(),
        ));
    }
    Ok(id.to_string())
}

/// 已有行与请求 hash 分类。
///
/// Business Logic: same id 必须按 payload fingerprint 区分回放与冲突。
/// Code Logic: hash 相等 → Replay；否则 Conflict。
fn classify_existing_claim(existing: &TransferTask, payload_hash: &str) -> SenderClaimOutcome {
    match existing.operation_payload_hash.as_deref() {
        Some(h) if h == payload_hash => SenderClaimOutcome::Replay(existing.clone()),
        // legacy 空 hash：拒绝当作通配符，按冲突处理（fail-closed）。
        _ => SenderClaimOutcome::Conflict {
            existing: existing.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    //! transfer_history recovery schema / legacy 默认 / 唯一索引测试。

    use super::*;
    use crate::models::transfer::{
        TransferDirection, TransferFailure, TransferFailureStage, TransferPhase, TransferStatus,
        TransferTask,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 仅含 M5 旧列的 schema（模拟升级前库）。
    const LEGACY_TRANSFER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS transfer_history (
        id TEXT PRIMARY KEY,
        filename TEXT NOT NULL,
        file_path TEXT NOT NULL,
        size INTEGER NOT NULL,
        sha256 TEXT NOT NULL,
        direction TEXT NOT NULL,
        peer_device_id TEXT NOT NULL,
        status TEXT NOT NULL,
        transferred_bytes INTEGER DEFAULT 0,
        created_at TEXT NOT NULL,
        completed_at TEXT
    )";

    /// 内存库 + 旧表 + ensure_schema，返回仓库。
    ///
    /// Business Logic: 单测需在无 migrate 框架下验证幂等升级。
    /// Code Logic: sqlite::memory + legacy CREATE + TransferRepo::ensure_schema。
    async fn setup_upgraded_repo() -> TransferRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(LEGACY_TRANSFER_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();
        TransferRepo::ensure_schema(&pool).await.unwrap();
        TransferRepo::new(pool)
    }

    /// 插入仅旧列的 fixture 行（绕过 record，模拟升级前写入）。
    ///
    /// Business Logic: legacy 行缺 recovery 列值，load 后必须默认 attempt=1 与 id 身份。
    /// Code Logic: 直接 INSERT 旧列集合。
    async fn insert_legacy_row(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO transfer_history \
             (id, filename, file_path, size, sha256, direction, peer_device_id, status, \
              transferred_bytes, created_at, completed_at) \
             VALUES (?, 'legacy.bin', '/tmp/legacy.bin', 42, 'abc', 'send', 'peer', 'completed', \
              42, '2026-07-01T00:00:00Z', '2026-07-01T00:01:00Z')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// 旧行加载后 attempt=1，logical/attempt/protocol id 等于 id。
    #[tokio::test]
    async fn legacy_transfer_defaults_to_attempt_one() {
        let repo = setup_upgraded_repo().await;
        insert_legacy_row(&repo.db, "legacy-task-1").await;

        let task = repo
            .get_by_id("legacy-task-1")
            .await
            .unwrap()
            .expect("legacy row should load");
        assert_eq!(task.attempt, 1);
        assert_eq!(task.logical_transfer_id, task.id);
        assert_eq!(task.attempt_id, task.id);
        assert_eq!(task.protocol_transfer_id, task.id);
        assert!(task.phase.is_none());
        assert!(task.failure.is_none());
        assert!(task.client_operation_id.is_none());
    }

    /// ensure_schema 连续跑两次不得失败。
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
        sqlx::query(LEGACY_TRANSFER_SCHEMA)
            .execute(&pool)
            .await
            .unwrap();
        TransferRepo::ensure_schema(&pool).await.unwrap();
        TransferRepo::ensure_schema(&pool).await.unwrap();

        let cols = sqlx::query("PRAGMA table_info(transfer_history)")
            .fetch_all(&pool)
            .await
            .unwrap();
        let names: Vec<String> = cols
            .iter()
            .filter_map(|r| r.try_get::<String, _>("name").ok())
            .collect();
        for expected in [
            "phase",
            "failure_stage",
            "failure_code",
            "failure_retryable",
            "failure_message",
            "attempt",
            "logical_transfer_id",
            "attempt_id",
            "protocol_transfer_id",
            "client_operation_id",
            "operation_payload_hash",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing column {expected}"
            );
        }
    }

    /// 未知 phase 列值 load 后为 None；effective_phase 回落 status 而非 Failed。
    #[tokio::test]
    async fn unknown_phase_does_not_map_to_failed() {
        let repo = setup_upgraded_repo().await;
        sqlx::query(
            "INSERT INTO transfer_history \
             (id, filename, file_path, size, sha256, direction, peer_device_id, status, \
              transferred_bytes, created_at, completed_at, phase, attempt) \
             VALUES ('p1', 'a.bin', '/tmp/a.bin', 1, 'x', 'send', 'peer', 'transferring', \
              0, '2026-07-14T00:00:00Z', NULL, 'brand_new_phase', 1)",
        )
        .execute(&repo.db)
        .await
        .unwrap();

        let task = repo.get_by_id("p1").await.unwrap().unwrap();
        assert!(task.phase.is_none(), "unknown phase must stay None, not Failed");
        assert_eq!(task.status, TransferStatus::Transferring);
        assert_eq!(task.effective_phase(), TransferPhase::Transferring);
        assert_ne!(task.effective_phase(), TransferPhase::Failed);
    }

    /// record 往返保留 recovery 字段；同 client_operation_id 二次插入冲突。
    #[tokio::test]
    async fn record_roundtrip_and_unique_client_operation_id() {
        let repo = setup_upgraded_repo().await;
        let task = TransferTask {
            filename: "r.bin".into(),
            file_path: "/tmp/r.bin".into(),
            size: 9,
            sha256: "sha".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer-a".into(),
            status: TransferStatus::Failed,
            transferred_bytes: 4,
            created_at: "2026-07-14T01:00:00Z".into(),
            completed_at: Some("2026-07-14T01:01:00Z".into()),
            phase: Some(TransferPhase::Failed),
            failure: Some(TransferFailure {
                stage: TransferFailureStage::Connect,
                code: "peer_unreachable".into(),
                retryable: true,
                message: "对端不可达".into(),
            }),
            attempt: 2,
            logical_transfer_id: "logical-1".into(),
            attempt_id: "attempt-2".into(),
            protocol_transfer_id: "proto-1".into(),
            client_operation_id: Some("op-unique-1".into()),
            operation_payload_hash: Some("payload-hash".into()),
            ..TransferTask::recovery_defaults("task-new-1")
        };
        repo.record(&task).await.unwrap();
        let loaded = repo.get_by_id("task-new-1").await.unwrap().unwrap();
        assert_eq!(loaded.attempt, 2);
        assert_eq!(loaded.logical_transfer_id, "logical-1");
        assert_eq!(loaded.attempt_id, "attempt-2");
        assert_eq!(loaded.protocol_transfer_id, "proto-1");
        assert_eq!(loaded.phase, Some(TransferPhase::Failed));
        assert_eq!(
            loaded.failure.as_ref().map(|f| f.code.as_str()),
            Some("peer_unreachable")
        );
        assert_eq!(loaded.client_operation_id.as_deref(), Some("op-unique-1"));

        let conflict = TransferTask {
            client_operation_id: Some("op-unique-1".into()),
            ..TransferTask::recovery_defaults("task-new-2")
        };
        let err = repo.record(&conflict).await.expect_err("unique op id");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("UNIQUE") || msg.contains("unique") || msg.contains("constraint"),
            "expected unique constraint error, got {msg}"
        );
    }


    /// 同 client_operation_id + 同 payload → Replay，不同 payload → Conflict。
    #[tokio::test]
    async fn claim_same_id_same_payload_replays_different_conflicts() {
        let repo = setup_upgraded_repo().await;
        let task = TransferTask {
            filename: "a.bin".into(),
            file_path: "/tmp/a.bin".into(),
            size: 1,
            sha256: "aa".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T02:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "logical-x".into(),
            attempt_id: "attempt-x".into(),
            protocol_transfer_id: "proto-x".into(),
            client_operation_id: Some("op-claim-1".into()),
            operation_payload_hash: Some("hash-aaa".into()),
            ..TransferTask::recovery_defaults("attempt-x")
        };
        let fresh = repo
            .claim_sender_operation("op-claim-1", "hash-aaa", &task)
            .await
            .unwrap();
        assert!(matches!(fresh, super::SenderClaimOutcome::Fresh(_)));

        let replay = repo
            .claim_sender_operation("op-claim-1", "hash-aaa", &task)
            .await
            .unwrap();
        match replay {
            super::SenderClaimOutcome::Replay(t) => {
                assert_eq!(t.id, "attempt-x");
                assert_eq!(t.client_operation_id.as_deref(), Some("op-claim-1"));
            }
            other => panic!("expected Replay, got {other:?}"),
        }

        let conflict = repo
            .claim_sender_operation("op-claim-1", "hash-bbb", &task)
            .await
            .unwrap();
        assert!(matches!(conflict, super::SenderClaimOutcome::Conflict { .. }));
    }

    /// 并发同 id claim 仅一条 Fresh winner。
    #[tokio::test]
    async fn concurrent_unique_claim_only_one_fresh() {
        let repo = std::sync::Arc::new(setup_upgraded_repo().await);
        let make = |id: &str| TransferTask {
            filename: "c.bin".into(),
            file_path: "/tmp/c.bin".into(),
            size: 2,
            sha256: "bb".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T03:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "logical-c".into(),
            attempt_id: id.into(),
            protocol_transfer_id: "proto-c".into(),
            client_operation_id: Some("op-concurrent".into()),
            operation_payload_hash: Some("hash-c".into()),
            ..TransferTask::recovery_defaults(id)
        };
        let r1 = repo.clone();
        let r2 = repo.clone();
        let (a, b) = tokio::join!(
            async move { r1.claim_sender_operation("op-concurrent", "hash-c", &make("id-a")).await.unwrap() },
            async move { r2.claim_sender_operation("op-concurrent", "hash-c", &make("id-b")).await.unwrap() },
        );
        let fresh_count = [&a, &b]
            .iter()
            .filter(|o| matches!(o, super::SenderClaimOutcome::Fresh(_)))
            .count();
        let replay_count = [&a, &b]
            .iter()
            .filter(|o| matches!(o, super::SenderClaimOutcome::Replay(_)))
            .count();
        assert_eq!(fresh_count, 1, "exactly one Fresh");
        assert_eq!(replay_count, 1, "other must Replay");
        assert_eq!(
            repo.count_attempts_for_logical("logical-c").await.unwrap(),
            1
        );
    }

    /// get_by_client_operation_id 与 list_recoverable_queued_sends。
    #[tokio::test]
    async fn get_by_client_op_and_list_recoverable_queued() {
        let repo = setup_upgraded_repo().await;
        let task = TransferTask {
            filename: "q.bin".into(),
            file_path: "/tmp/q.bin".into(),
            size: 3,
            sha256: "cc".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T04:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "logical-q".into(),
            attempt_id: "attempt-q".into(),
            protocol_transfer_id: "proto-q".into(),
            client_operation_id: Some("op-q".into()),
            operation_payload_hash: Some("hash-q".into()),
            ..TransferTask::recovery_defaults("attempt-q")
        };
        repo.claim_sender_operation("op-q", "hash-q", &task)
            .await
            .unwrap();
        let by_op = repo.get_by_client_operation_id("op-q").await.unwrap().unwrap();
        assert_eq!(by_op.id, "attempt-q");
        let recoverable = repo.list_recoverable_queued_sends().await.unwrap();
        assert!(recoverable.iter().any(|t| t.id == "attempt-q"));
    }
}
