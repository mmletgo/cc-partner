//! storage/recovery_job_repo.rs — recovery_jobs 状态机
//!
//! Business Logic（为什么需要这个模块）:
//!     备份恢复可能在 pre-backup / applying 中崩溃；需要可判定的状态机
//!     （preparing/ready/applying/succeeded/failed/rolledBack），禁止猜测成功。
//!
//! Code Logic（这个模块做什么）:
//!     幂等建表 + CRUD；状态转移显式；备份路径/校验和/错误摘要落库。

use crate::error::AppError;
use crate::storage::maintenance_gate::{
    begin_write_with_permit, DatabaseMaintenanceGate, DatabaseWritePermit,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::sync::Arc;

/// recovery_jobs 合法状态（camelCase 序列化对齐前端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecoveryJobStatus {
    Preparing,
    Ready,
    Applying,
    Succeeded,
    Failed,
    #[serde(rename = "rolledBack")]
    RolledBack,
}

impl RecoveryJobStatus {
    /// 持久化字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Applying => "applying",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::RolledBack => "rolledBack",
        }
    }

    /// 解析持久化字符串。
    pub fn parse(s: &str) -> Result<Self, AppError> {
        match s {
            "preparing" => Ok(Self::Preparing),
            "ready" => Ok(Self::Ready),
            "applying" => Ok(Self::Applying),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "rolledBack" | "rolled_back" => Ok(Self::RolledBack),
            other => Err(AppError::generic(format!(
                "未知 recovery job 状态: {other}"
            ))),
        }
    }
}

/// 一条恢复任务记录。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryJobRow {
    pub id: String,
    pub status: RecoveryJobStatus,
    pub archive_path: Option<String>,
    pub pre_restore_backup_path: Option<String>,
    pub selected_domains_json: String,
    pub mode: String,
    pub error_summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// recovery_jobs 仓储。
pub struct RecoveryJobRepo {
    db: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl RecoveryJobRepo {
    /// 构造。
    ///
    /// Business Logic: restore 管道与 Settings 列表共享。
    /// Code Logic: 持有 pool + gate。
    pub fn new(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 幂等建表（startup schema bootstrap，inventory 白名单）。
    ///
    /// Business Logic: 旧库升级时补 recovery_jobs。
    /// Code Logic: CREATE TABLE IF NOT EXISTS。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS recovery_jobs (
                id TEXT PRIMARY KEY NOT NULL,
                status TEXT NOT NULL,
                archive_path TEXT,
                pre_restore_backup_path TEXT,
                selected_domains_json TEXT NOT NULL DEFAULT '[]',
                mode TEXT NOT NULL DEFAULT 'merge',
                error_summary TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_recovery_jobs_updated
             ON recovery_jobs(updated_at DESC)",
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    fn row_to_job(row: &SqliteRow) -> Result<RecoveryJobRow, AppError> {
        Ok(RecoveryJobRow {
            id: row.try_get("id")?,
            status: RecoveryJobStatus::parse(&row.try_get::<String, _>("status")?)?,
            archive_path: row.try_get("archive_path")?,
            pre_restore_backup_path: row.try_get("pre_restore_backup_path")?,
            selected_domains_json: row.try_get("selected_domains_json")?,
            mode: row.try_get("mode")?,
            error_summary: row.try_get("error_summary")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }

    /// 插入 preparing 任务。
    ///
    /// Business Logic: restore 入口先落盘状态再做重活，崩溃可判定。
    /// Code Logic: shared begin → INSERT。
    pub async fn insert_preparing(
        &self,
        id: &str,
        archive_path: Option<&str>,
        selected_domains_json: &str,
        mode: &str,
        now: &str,
    ) -> Result<RecoveryJobRow, AppError> {
        let lease = self.gate.acquire_shared().await;
        let permit = DatabaseWritePermit::Shared(lease);
        let mut tx = begin_write_with_permit(&self.db, &permit).await?;
        sqlx::query(
            "INSERT INTO recovery_jobs
             (id, status, archive_path, pre_restore_backup_path, selected_domains_json, mode, error_summary, created_at, updated_at)
             VALUES (?, 'preparing', ?, NULL, ?, ?, NULL, ?, ?)",
        )
        .bind(id)
        .bind(archive_path)
        .bind(selected_domains_json)
        .bind(mode)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        drop(permit);
        self.get(id)
            .await?
            .ok_or_else(|| AppError::generic("recovery job insert 后读回失败"))
    }

    /// 在已持有 exclusive permit 下写状态（restore 路径）。
    ///
    /// Business Logic: restore 独占期更新状态不得再抢 shared。
    /// Code Logic: begin_write_with_permit(MaintenanceExclusive)。
    pub async fn update_status_with_permit(
        &self,
        permit: &DatabaseWritePermit,
        id: &str,
        status: RecoveryJobStatus,
        pre_restore_backup_path: Option<&str>,
        error_summary: Option<&str>,
        now: &str,
    ) -> Result<(), AppError> {
        let mut tx = begin_write_with_permit(&self.db, permit).await?;
        sqlx::query(
            "UPDATE recovery_jobs SET status = ?, pre_restore_backup_path = COALESCE(?, pre_restore_backup_path),
             error_summary = ?, updated_at = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(pre_restore_backup_path)
        .bind(error_summary)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 普通路径更新状态（shared）。
    pub async fn update_status(
        &self,
        id: &str,
        status: RecoveryJobStatus,
        pre_restore_backup_path: Option<&str>,
        error_summary: Option<&str>,
        now: &str,
    ) -> Result<(), AppError> {
        let lease = self.gate.acquire_shared().await;
        let permit = DatabaseWritePermit::Shared(lease);
        self.update_status_with_permit(
            &permit,
            id,
            status,
            pre_restore_backup_path,
            error_summary,
            now,
        )
        .await
    }

    /// 按 id 读取。
    pub async fn get(&self, id: &str) -> Result<Option<RecoveryJobRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, status, archive_path, pre_restore_backup_path, selected_domains_json, mode, error_summary, created_at, updated_at
             FROM recovery_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_job(&r)?)),
            None => Ok(None),
        }
    }

    /// 最近任务列表（默认 50）。
    pub async fn list_recent(&self, limit: i64) -> Result<Vec<RecoveryJobRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, status, archive_path, pre_restore_backup_path, selected_domains_json, mode, error_summary, created_at, updated_at
             FROM recovery_jobs ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_job).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> RecoveryJobRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        RecoveryJobRepo::ensure_schema(&pool).await.unwrap();
        RecoveryJobRepo::new(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    #[tokio::test]
    async fn preparing_to_failed_state_machine() {
        let repo = setup().await;
        let job = repo
            .insert_preparing("j1", Some("/tmp/a.zip"), "[\"prompts\"]", "merge", "t0")
            .await
            .unwrap();
        assert_eq!(job.status, RecoveryJobStatus::Preparing);
        repo.update_status("j1", RecoveryJobStatus::Failed, None, Some("boom"), "t1")
            .await
            .unwrap();
        let job = repo.get("j1").await.unwrap().unwrap();
        assert_eq!(job.status, RecoveryJobStatus::Failed);
        assert_eq!(job.error_summary.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn exclusive_permit_updates_without_shared() {
        let repo = setup().await;
        repo.insert_preparing("j2", None, "[]", "replace", "t0")
            .await
            .unwrap();
        let exclusive = repo.gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        repo.update_status_with_permit(
            &permit,
            "j2",
            RecoveryJobStatus::Applying,
            Some("/backups/pre.zip"),
            None,
            "t1",
        )
        .await
        .unwrap();
        drop(permit);
        drop(exclusive);
        let job = repo.get("j2").await.unwrap().unwrap();
        assert_eq!(job.status, RecoveryJobStatus::Applying);
        assert_eq!(
            job.pre_restore_backup_path.as_deref(),
            Some("/backups/pre.zip")
        );
    }
}
