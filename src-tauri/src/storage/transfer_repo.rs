//! storage/transfer_repo.rs — transfer_history 表 CRUD
//!
//! Business Logic（为什么需要这个模块）:
//!     传输历史（含已结束的发送/接收任务）需持久化，供前端传输面板 `list_transfers`
//!     返回"活跃任务 + 历史"合并列表。表 schema 由 lib.rs 建表（CREATE TABLE IF NOT EXISTS）。
//!
//! Code Logic（这个模块做什么）:
//!     - `record(task)`：shared write lease 下 INSERT OR REPLACE 写入一条历史（终态任务落库）。
//!     - `list()`：按 created_at 倒序返回全部历史。
//!     - `update_status(...)`：shared write lease 下更新某任务的状态/进度/完成时间。

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::storage::maintenance_gate::{with_shared_write_lease, DatabaseMaintenanceGate};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::sync::Arc;

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

    /// 将一行映射为 TransferTask。
    fn row_to_task(row: &sqlx::sqlite::SqliteRow) -> TransferTask {
        let direction_str: String = row.try_get("direction").unwrap_or_default();
        let status_str: String = row.try_get("status").unwrap_or_default();
        let transferred: i64 = row.try_get("transferred_bytes").unwrap_or(0);
        TransferTask {
            id: row.try_get("id").unwrap_or_default(),
            filename: row.try_get("filename").unwrap_or_default(),
            file_path: row.try_get("file_path").unwrap_or_default(),
            size: row.try_get::<i64, _>("size").unwrap_or(0) as u64,
            sha256: row.try_get("sha256").unwrap_or_default(),
            // chunk_size 不在表中，用默认 960KB
            chunk_size: 960 * 1024,
            direction: TransferDirection::from_str_lossy(&direction_str),
            peer_device_id: row.try_get("peer_device_id").unwrap_or_default(),
            status: TransferStatus::from_str_lossy(&status_str),
            transferred_bytes: transferred as u64,
            created_at: row.try_get("created_at").unwrap_or_default(),
            completed_at: row.try_get("completed_at").unwrap_or(None),
        }
    }

    /// 写入一条历史（INSERT OR REPLACE，终态任务落库）。
    ///
    /// Business Logic: 发送/接收终态后必须 durable 落库，供重启后 complete/status 收敛。
    /// Code Logic: 持 shared write lease 后 INSERT OR REPLACE。
    pub async fn record(&self, task: &TransferTask) -> Result<(), AppError> {
        let direction_str = match task.direction {
            TransferDirection::Send => "send",
            TransferDirection::Receive => "receive",
        };
        let status_str = match task.status {
            TransferStatus::Pending => "pending",
            TransferStatus::Transferring => "transferring",
            TransferStatus::Completed => "completed",
            TransferStatus::Failed => "failed",
            TransferStatus::Cancelled => "cancelled",
        };
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT OR REPLACE INTO transfer_history \
                 (id, filename, file_path, size, sha256, direction, peer_device_id, status, transferred_bytes, created_at, completed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .execute(&self.db)
            .await?;
            Ok(())
        })
        .await
    }

    /// 列出全部历史（按 created_at 倒序）。
    pub async fn list(&self) -> Result<Vec<TransferTask>, AppError> {
        let rows = sqlx::query(
            "SELECT id, filename, file_path, size, sha256, direction, peer_device_id, status, transferred_bytes, created_at, completed_at \
             FROM transfer_history ORDER BY created_at DESC",
        )
        .fetch_all(&self.db)
        .await?;
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
        let row = sqlx::query(
            "SELECT id, filename, file_path, size, sha256, direction, peer_device_id, status, transferred_bytes, created_at, completed_at \
             FROM transfer_history WHERE id = ?",
        )
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
        let status_str = match status {
            TransferStatus::Pending => "pending",
            TransferStatus::Transferring => "transferring",
            TransferStatus::Completed => "completed",
            TransferStatus::Failed => "failed",
            TransferStatus::Cancelled => "cancelled",
        };
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
}
