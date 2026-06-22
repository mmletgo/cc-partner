//! storage/prompt_repo.rs — Prompt 数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     Prompt 管理需要创建、修改、软删除、搜索、按标签筛选、列出标签等功能，
//!     同步引擎还需批量 upsert 和同步摘要。此模块对照 Python `prompt_repo.py`，
//!     逐方法实现等价逻辑，保证数据行为与旧版一致。
//!
//! Code Logic（这个模块做什么）:
//!     持有 `SqlitePool`，用运行期 `sqlx::query` 执行 SQL。
//!     JSON 字段（tags, vector_clock）用 serde_json 序列化为紧凑 JSON 读写，与 Python 互通。
//!     datetime 字段以 String 透传（兼容有无时区格式）。
//!     delete 为软删除（deleted=1），并同时推进 vector_clock 与 updated_at（修正 Python
//!     handler 自增 clock 却未落库的 bug）。

use crate::error::AppError;
use crate::models::prompt::PromptRow;
use sqlx::sqlite::SqlitePool;
use sqlx::Row;
use std::collections::HashMap;

/// Prompt 仓库，封装所有 prompts 表的数据库操作。
pub struct PromptRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
}

impl PromptRepo {
    /// 构造仓库。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 将数据库一行映射为 PromptRow（JSON 字段反序列化、deleted int→bool）。
    fn row_to_prompt(row: &sqlx::sqlite::SqliteRow) -> Result<PromptRow, AppError> {
        let tags_text: String = row.try_get("tags")?;
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let tags: Vec<String> = serde_json::from_str(&tags_text)?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        Ok(PromptRow {
            id: row.try_get("id")?,
            title: row.try_get("title")?,
            content: row.try_get("content")?,
            tags,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            deleted: deleted_int != 0,
        })
    }

    /// 列表查询：可选关键词搜索 / 单标签筛选，默认排除已删除，按 updated_at 降序。
    ///
    /// Business Logic: 前端列表页传 search 或 tag；无参数则返回全部未删除 Prompt。
    /// Code Logic: 对照 prompt_repo.py 的 get_all / search / filter_by_tags 三条路径分支。
    pub async fn list(
        &self,
        search: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Vec<PromptRow>, AppError> {
        // 三种查询分支，分别对应 Python get_all / search / filter_by_tags
        if let Some(kw) = search {
            // search: title/content LIKE '%kw%'，排除已删除，updated_at DESC
            let pattern = format!("%{}%", kw);
            let rows = sqlx::query(
                "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted \
                 FROM prompts WHERE deleted = 0 AND (title LIKE ? OR content LIKE ?) ORDER BY updated_at DESC",
            )
            .bind(&pattern)
            .bind(&pattern)
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        } else if let Some(t) = tag {
            // tag: json_each 展开 tags，与给定标签交集匹配，DISTINCT 去重
            let rows = sqlx::query(
                "SELECT DISTINCT p.id, p.title, p.content, p.tags, p.created_at, p.updated_at, p.device_id, p.vector_clock, p.deleted \
                 FROM prompts p, json_each(p.tags) AS t \
                 WHERE p.deleted = 0 AND t.value = ? ORDER BY p.updated_at DESC",
            )
            .bind(t)
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        } else {
            // 无参数：全部未删除，updated_at DESC
            let rows = sqlx::query(
                "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted \
                 FROM prompts WHERE deleted = 0 ORDER BY updated_at DESC",
            )
            .fetch_all(&self.db)
            .await?;
            rows.iter().map(Self::row_to_prompt).collect()
        }
    }

    /// 按主键查询单条 Prompt（含已删除记录，与 Python get_by_id 一致）。
    pub async fn get(&self, id: &str) -> Result<Option<PromptRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted \
             FROM prompts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_prompt(&r)?)),
            None => Ok(None),
        }
    }

    /// 插入新 Prompt（tags/vector_clock 序列化为 JSON）。
    pub async fn create(&self, p: &PromptRow) -> Result<(), AppError> {
        let tags_text = serde_json::to_string(&p.tags)?;
        let vc_text = serde_json::to_string(&p.vector_clock)?;
        sqlx::query(
            "INSERT INTO prompts (id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&p.id)
        .bind(&p.title)
        .bind(&p.content)
        .bind(tags_text)
        .bind(&p.created_at)
        .bind(&p.updated_at)
        .bind(&p.device_id)
        .bind(vc_text)
        .bind(p.deleted as i64)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 全字段更新一条 Prompt（含 vector_clock / deleted）。
    pub async fn update(&self, p: &PromptRow) -> Result<(), AppError> {
        let tags_text = serde_json::to_string(&p.tags)?;
        let vc_text = serde_json::to_string(&p.vector_clock)?;
        sqlx::query(
            "UPDATE prompts SET title = ?, content = ?, tags = ?, updated_at = ?, device_id = ?, vector_clock = ?, deleted = ? WHERE id = ?",
        )
        .bind(&p.title)
        .bind(&p.content)
        .bind(tags_text)
        .bind(&p.updated_at)
        .bind(&p.device_id)
        .bind(vc_text)
        .bind(p.deleted as i64)
        .bind(&p.id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 软删除：标记 deleted=1，更新 updated_at，并写入推进后的 vector_clock。
    ///
    /// Business Logic: CRDT 删除是一次写入，需推进本端 vector_clock 使对端感知。
    ///     Python handler 自增了 clock 但只调 repo.delete(id)（未落库 clock），此处修正：
    ///     接收已自增的 vector_clock 参数一并写回。
    /// Code Logic: 对照 prompt_repo.py delete 的 deleted=1 + updated_at=now，
    ///     额外 SET vector_clock = ?。
    pub async fn soft_delete(
        &self,
        id: &str,
        now: &str,
        vector_clock: &HashMap<String, u64>,
    ) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(vector_clock)?;
        sqlx::query(
            "UPDATE prompts SET deleted = 1, updated_at = ?, vector_clock = ? WHERE id = ?",
        )
        .bind(now)
        .bind(vc_text)
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 返回所有 Prompt（含 deleted 软删除记录），用于跨设备同步。
    ///
    /// Business Logic: 同步必须传播删除事件，因此需读取含 deleted=1 的全部记录。
    ///     对照 Python `get_sync_summary` 的"含 deleted"语义，但本方法返回完整 PromptRow
    ///     （engine 既要 summary 也要完整数据，统一从此取，内存中再投影为 summary）。
    ///
    /// Code Logic: SELECT 全字段（无 deleted 过滤），不排序（同步用，顺序无关）。
    pub async fn get_all_for_sync(&self) -> Result<Vec<PromptRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted \
             FROM prompts",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_prompt).collect()
    }

    /// 批量插入/更新（按 id 主键），用于同步 push 落库。
    ///
    /// Business Logic: 同步引擎从对端拉取多条 Prompt 后需批量写入本地，已存在则覆盖。
    ///     对照 Python `bulk_upsert`。upsert 前不做合并决策（合并由 engine/merger 在调用前
    ///     决定），此处直接 INSERT OR REPLACE。
    ///
    /// Code Logic: 空切片直接返回；否则逐条 INSERT OR REPLACE（sqlx 无 executemany 的批量
    ///     原子语义，逐条执行；max_connections(1) 单连接语义下天然串行）。
    pub async fn bulk_upsert(&self, prompts: &[PromptRow]) -> Result<(), AppError> {
        if prompts.is_empty() {
            return Ok(());
        }
        for p in prompts {
            let tags_text = serde_json::to_string(&p.tags)?;
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO prompts \
                 (id, title, content, tags, created_at, updated_at, device_id, vector_clock, deleted) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id)
            .bind(&p.title)
            .bind(&p.content)
            .bind(tags_text)
            .bind(&p.created_at)
            .bind(&p.updated_at)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(p.deleted as i64)
            .execute(&self.db)
            .await?;
        }
        Ok(())
    }

    /// 列出所有未删除 Prompt 用过的去重标签（升序）。
    ///
    /// Business Logic: 前端标签筛选栏需动态展示可选标签。
    /// Code Logic: 对照 prompt_repo.py get_all_tags，用 json_each 展开后 DISTINCT。
    pub async fn list_tags(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT DISTINCT t.value AS tag FROM prompts p, json_each(p.tags) AS t \
             WHERE p.deleted = 0 ORDER BY t.value",
        )
        .fetch_all(&self.db)
        .await?;
        let mut tags = Vec::with_capacity(rows.len());
        for r in &rows {
            let tag: String = r.try_get("tag")?;
            tags.push(tag);
        }
        Ok(tags)
    }
}
