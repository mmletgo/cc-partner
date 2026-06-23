//! storage/scratchpad_repo.rs — 速记本单例数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     Scratchpad 从前端 localStorage 迁移到 Rust/SQLite 后，页面自动保存、局域网同步、
//!     GitHub 云同步都必须读写同一份权威数据。速记本是单例文本，全表只保留 id="scratchpad"。
//!
//! Code Logic（这个模块做什么）:
//!     持有 SqlitePool，提供 get_or_init / update_content / get_for_sync / upsert。
//!     vector_clock 以 JSON TEXT 存储；清空文本是普通 update，不走 deleted。

use crate::error::AppError;
use crate::models::scratchpad::{ScratchpadRow, SCRATCHPAD_ID};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::collections::HashMap;

/// 速记本仓库，封装 scratchpad 表的所有数据库操作。
pub struct ScratchpadRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
}

impl ScratchpadRepo {
    /// 构造仓库。
    ///
    /// Business Logic: AppState 初始化时注入共享 pool，命令层和同步层复用同一仓库实例。
    /// Code Logic: 保存 SqlitePool clone；SqlitePool 内部已是共享句柄。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 当前时间的 RFC3339 字符串。
    ///
    /// Business Logic: 新建和更新速记本时需要稳定的 LWW 时间戳参与冲突解决。
    /// Code Logic: 使用 UTC RFC3339，与 prompts/ssh_targets 的时间格式一致。
    fn now_iso() -> String {
        Utc::now().to_rfc3339()
    }

    /// 将数据库行映射为 ScratchpadRow。
    ///
    /// Business Logic: DB 中 vector_clock/deleted 以 TEXT/INTEGER 保存，业务层需要结构化数据。
    /// Code Logic: JSON 反序列化 vector_clock；deleted 0/1 转 bool。
    fn row_to_scratchpad(row: &SqliteRow) -> Result<ScratchpadRow, AppError> {
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        Ok(ScratchpadRow {
            id: row.try_get("id")?,
            content: row.try_get("content")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            deleted: deleted_int != 0,
        })
    }

    /// 查询当前速记本单例（含空内容）。
    ///
    /// Business Logic: 同步和页面加载都需要读取单例；未初始化时由 get_or_init 创建。
    /// Code Logic: 按固定 id 查询，返回 Option 方便 get_or_init 分支处理。
    pub async fn get(&self) -> Result<Option<ScratchpadRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, content, created_at, updated_at, device_id, vector_clock, deleted \
             FROM scratchpad WHERE id = ?",
        )
        .bind(SCRATCHPAD_ID)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_scratchpad(&r)?)),
            None => Ok(None),
        }
    }

    /// 获取速记本；若不存在则初始化空单例。
    ///
    /// Business Logic: 用户第一次打开速记本也应得到可自动保存的 DB 记录。空初始化不代表一次用户编辑，
    ///     因此 vector_clock 为空，避免把“未写内容”传播成有意义的远端更新。
    /// Code Logic: 查询 None 时插入 id="scratchpad"、content=""、deleted=false 的行。
    pub async fn get_or_init(&self, device_id: &str) -> Result<ScratchpadRow, AppError> {
        if let Some(row) = self.get().await? {
            return Ok(row);
        }
        let now = Self::now_iso();
        let row = ScratchpadRow {
            id: SCRATCHPAD_ID.to_string(),
            content: String::new(),
            created_at: now.clone(),
            updated_at: now,
            device_id: device_id.to_string(),
            vector_clock: HashMap::new(),
            deleted: false,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 更新速记本文本并推进本机向量时钟。
    ///
    /// Business Logic: 页面自动保存、清空都调用此方法；每次用户文本变更都必须被同步层感知。
    /// Code Logic: 读取旧行（无则初始化），保留 created_at，content 改为 next，updated_at=now，
    ///     vector_clock[device_id]+=1，deleted=false，然后 upsert。
    pub async fn update_content(
        &self,
        next: &str,
        device_id: &str,
    ) -> Result<ScratchpadRow, AppError> {
        let existing = self.get_or_init(device_id).await?;
        let mut vector_clock = existing.vector_clock.clone();
        let counter = vector_clock.entry(device_id.to_string()).or_insert(0);
        *counter += 1;
        let row = ScratchpadRow {
            id: SCRATCHPAD_ID.to_string(),
            content: next.to_string(),
            created_at: existing.created_at,
            updated_at: Self::now_iso(),
            device_id: device_id.to_string(),
            vector_clock,
            deleted: false,
        };
        self.upsert(&row).await?;
        Ok(row)
    }

    /// 返回速记本同步实体列表（单例 0/1 条）。
    ///
    /// Business Logic: cloud snapshot 和 P2P 同步按“列表”接口复用批量结构；速记本仍保持单例语义。
    /// Code Logic: 直接查询已有行，不主动初始化，避免纯后台流程制造无意义记录；调用方需要时可 get_or_init。
    pub async fn get_for_sync(&self) -> Result<Vec<ScratchpadRow>, AppError> {
        match self.get().await? {
            Some(row) => Ok(vec![row]),
            None => Ok(Vec::new()),
        }
    }

    /// 插入/替换速记本单例。
    ///
    /// Business Logic: 同步合并后需要把胜出版本落库；id 固定，INSERT OR REPLACE 足够表达覆盖。
    /// Code Logic: vector_clock 序列化为紧凑 JSON，deleted bool 转 0/1。
    pub async fn upsert(&self, row: &ScratchpadRow) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(&row.vector_clock)?;
        sqlx::query(
            "INSERT OR REPLACE INTO scratchpad \
             (id, content, created_at, updated_at, device_id, vector_clock, deleted) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.content)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .bind(&row.device_id)
        .bind(vc_text)
        .bind(row.deleted as i64)
        .execute(&self.db)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存 SQLite 并建好 scratchpad 表，返回仓库。
    async fn setup_repo() -> ScratchpadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scratchpad (\
             id TEXT PRIMARY KEY, content TEXT NOT NULL, created_at TEXT NOT NULL, \
             updated_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
             deleted INTEGER DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        ScratchpadRepo::new(pool)
    }

    /// 首次读取会创建空白单例，且不推进 vector_clock。
    #[tokio::test]
    async fn get_or_init_creates_empty_singleton_without_advancing_clock() {
        let repo = setup_repo().await;
        let row = repo.get_or_init("device-a").await.unwrap();

        assert_eq!(row.id, "scratchpad");
        assert_eq!(row.content, "");
        assert_eq!(row.device_id, "device-a");
        assert!(row.vector_clock.is_empty());
        assert!(!row.deleted);
    }

    /// 更新内容会推进本机向量时钟，并保留首次创建时间。
    #[tokio::test]
    async fn update_content_increments_current_device_clock_and_preserves_created_at() {
        let repo = setup_repo().await;
        let initial = repo.get_or_init("device-a").await.unwrap();

        let updated = repo.update_content("hello", "device-a").await.unwrap();

        assert_eq!(updated.id, "scratchpad");
        assert_eq!(updated.content, "hello");
        assert_eq!(updated.created_at, initial.created_at);
        assert_eq!(updated.device_id, "device-a");
        assert_eq!(updated.vector_clock.get("device-a"), Some(&1));
        assert!(!updated.deleted);
    }

    /// 同步读取返回单例行，便于 cloud/P2P 复用同一实体。
    #[tokio::test]
    async fn get_for_sync_returns_singleton_including_empty_content() {
        let repo = setup_repo().await;
        repo.get_or_init("device-a").await.unwrap();

        let rows = repo.get_for_sync().await.unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "scratchpad");
    }
}
