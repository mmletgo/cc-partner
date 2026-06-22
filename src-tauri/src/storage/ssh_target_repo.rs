//! storage/ssh_target_repo.rs — SSH 连接目标数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     SSH 页为每个连接目标保存的用户名/端口需持久化，并供同步引擎批量 upsert / 拉取同步摘要。
//!     模式对齐 cc_history_repo.rs：运行期 sqlx::query（非宏），JSON 字段用 serde_json，
//!     datetime 以 String 透传，deleted 软删除。
//!
//! Code Logic（这个模块做什么）:
//!     持有 SqlitePool，提供 list（不含删除）/ get（按 host）/ get_all_for_sync（含删除）/
//!     bulk_upsert（INSERT OR REPLACE，同步落库）/ upsert（单条）/ soft_delete。

use crate::error::AppError;
use crate::models::ssh_target::SshTargetRow;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::collections::HashMap;

/// SSH 目标仓库，封装所有 ssh_targets 表操作。
pub struct SshTargetRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
}

impl SshTargetRepo {
    /// 构造仓库。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 将数据库一行映射为 SshTargetRow（vector_clock JSON 反序列化、deleted int→bool）。
    fn row_to_ssh_target(row: &SqliteRow) -> Result<SshTargetRow, AppError> {
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        Ok(SshTargetRow {
            host: row.try_get("host")?,
            port: row.try_get("port")?,
            username: row.try_get("username")?,
            label: row.try_get("label")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            deleted: deleted_int != 0,
        })
    }

    /// 列出所有未删除的 SSH 目标，按 updated_at 降序。
    pub async fn list(&self) -> Result<Vec<SshTargetRow>, AppError> {
        let rows = sqlx::query(
            "SELECT host, port, username, label, device_id, vector_clock, created_at, updated_at, deleted \
             FROM ssh_targets WHERE deleted = 0 ORDER BY updated_at DESC",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_ssh_target).collect()
    }

    /// 按 host 主键查询单条（含已删除记录，供命令层判断存在性与软删除读取）。
    pub async fn get(&self, host: &str) -> Result<Option<SshTargetRow>, AppError> {
        let row = sqlx::query(
            "SELECT host, port, username, label, device_id, vector_clock, created_at, updated_at, deleted \
             FROM ssh_targets WHERE host = ?",
        )
        .bind(host)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_ssh_target(&r)?)),
            None => Ok(None),
        }
    }

    /// 返回全部目标（含 deleted 软删除记录），用于跨设备同步。
    pub async fn get_all_for_sync(&self) -> Result<Vec<SshTargetRow>, AppError> {
        let rows = sqlx::query(
            "SELECT host, port, username, label, device_id, vector_clock, created_at, updated_at, deleted \
             FROM ssh_targets",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_ssh_target).collect()
    }

    /// 批量插入/更新（按 host 主键，INSERT OR REPLACE），用于同步 push 落库。
    pub async fn bulk_upsert(&self, items: &[SshTargetRow]) -> Result<(), AppError> {
        if items.is_empty() {
            return Ok(());
        }
        for p in items {
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO ssh_targets \
                 (host, port, username, label, device_id, vector_clock, created_at, updated_at, deleted) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.host)
            .bind(p.port)
            .bind(&p.username)
            .bind(&p.label)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(&p.created_at)
            .bind(&p.updated_at)
            .bind(p.deleted as i64)
            .execute(&self.db)
            .await?;
        }
        Ok(())
    }

    /// 单条 upsert（命令层用，INSERT OR REPLACE）。
    pub async fn upsert(&self, row: &SshTargetRow) -> Result<(), AppError> {
        self.bulk_upsert(std::slice::from_ref(row)).await
    }

    /// 软删除：标记 deleted=1，更新 updated_at，并写入推进后的 vector_clock。
    pub async fn soft_delete(
        &self,
        host: &str,
        now: &str,
        vector_clock: &HashMap<String, u64>,
    ) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(vector_clock)?;
        sqlx::query(
            "UPDATE ssh_targets SET deleted = 1, updated_at = ?, vector_clock = ? WHERE host = ?",
        )
        .bind(now)
        .bind(vc_text)
        .bind(host)
        .execute(&self.db)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! ssh_target_repo 单测：用内存 SQLite 验证 upsert/get/list/soft_delete/get_all_for_sync。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存 SQLite 并建好 ssh_targets 表，返回仓库。
    async fn setup_repo() -> SshTargetRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_targets (\
             host TEXT PRIMARY KEY, port INTEGER NOT NULL DEFAULT 22, username TEXT NOT NULL, label TEXT, \
             device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        SshTargetRepo::new(pool)
    }

    /// 构造一条测试 Row。
    fn row(host: &str, username: &str, port: u16, vc_counter: u64) -> SshTargetRow {
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), vc_counter);
        SshTargetRow {
            host: host.to_string(),
            port,
            username: username.to_string(),
            label: None,
            device_id: "d1".to_string(),
            vector_clock: vc,
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: "2024-01-01T00:00:00+00:00".to_string(),
            deleted: false,
        }
    }

    #[tokio::test]
    async fn upsert_inserts_new_and_replaces_existing() {
        let repo = setup_repo().await;
        // 新增
        repo.upsert(&row("10.0.0.1", "alice", 22, 1)).await.unwrap();
        let got = repo.get("10.0.0.1").await.unwrap().unwrap();
        assert_eq!(got.username, "alice");
        assert_eq!(got.port, 22);
        // 同 host upsert 覆盖
        let mut changed = row("10.0.0.1", "bob", 2222, 2);
        changed.updated_at = "2024-01-02T00:00:00+00:00".to_string();
        repo.upsert(&changed).await.unwrap();
        let got2 = repo.get("10.0.0.1").await.unwrap().unwrap();
        assert_eq!(got2.username, "bob");
        assert_eq!(got2.port, 2222);
    }

    #[tokio::test]
    async fn list_excludes_deleted() {
        let repo = setup_repo().await;
        repo.upsert(&row("10.0.0.1", "alice", 22, 1)).await.unwrap();
        repo.upsert(&row("10.0.0.2", "bob", 22, 1)).await.unwrap();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 2u64);
        repo.soft_delete("10.0.0.1", "2024-01-02T00:00:00+00:00", &vc)
            .await
            .unwrap();
        let listed = repo.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].host, "10.0.0.2");
    }

    #[tokio::test]
    async fn soft_delete_marks_deleted_and_updates_clock() {
        let repo = setup_repo().await;
        repo.upsert(&row("10.0.0.1", "alice", 22, 1)).await.unwrap();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 2u64);
        repo.soft_delete("10.0.0.1", "2024-01-02T00:00:00+00:00", &vc)
            .await
            .unwrap();
        // get 能取到（含 deleted）
        let got = repo.get("10.0.0.1").await.unwrap().unwrap();
        assert!(got.deleted);
        assert_eq!(got.vector_clock.get("d1"), Some(&2));
    }

    #[tokio::test]
    async fn get_all_for_sync_includes_deleted() {
        let repo = setup_repo().await;
        repo.upsert(&row("10.0.0.1", "alice", 22, 1)).await.unwrap();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 2u64);
        repo.soft_delete("10.0.0.1", "2024-01-02T00:00:00+00:00", &vc)
            .await
            .unwrap();
        // 同步需传播删除，故仍含已删除记录
        let synced = repo.get_all_for_sync().await.unwrap();
        assert_eq!(synced.len(), 1);
        assert!(synced[0].deleted);
    }
}
