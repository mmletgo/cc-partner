//! storage/transfer_repo.rs — transfer_history 表 CRUD
//!
//! Business Logic（为什么需要这个模块）:
//!     传输历史（含已结束的发送/接收任务）需持久化，供前端传输面板 `list_transfers`
//!     返回"活跃任务 + 历史"合并列表。表 schema 由 lib.rs 建表（CREATE TABLE IF NOT EXISTS）。
//!
//! Code Logic（这个模块做什么）:
//!     - `record(task)`：INSERT OR REPLACE 写入一条历史（终态任务落库）。
//!     - `list()`：按 created_at 倒序返回全部历史。
//!     - `update_status(...)`：更新某任务的状态/进度/完成时间（断点续传场景可能用到）。

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use sqlx::sqlite::SqlitePool;
use sqlx::Row;

/// 传输历史仓库。
pub struct TransferRepo {
    db: SqlitePool,
}

impl TransferRepo {
    /// 构造仓库。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
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

    /// 更新某任务的状态/进度/完成时间。
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
    }
}
