//! storage/cc_history_repo.rs — Claude Code 历史数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的 Claude Code 用户输入 prompt 需要按项目归类持久化，供前端检索、
//!     并供同步引擎批量 upsert / 拉取同步摘要。采集与同步对"已存在行"的处理语义不同：
//!     采集必须 INSERT OR IGNORE（绝不覆盖已存在行，否则会把同步合并出的向量时钟因果历史
//!     打回 `{device_id:1}`）；同步 push 用 INSERT OR REPLACE（覆盖式写合并结果）。
//!     两者严格分离由不同方法承担。
//!
//! Code Logic（这个模块做什么）:
//!     持有 `SqlitePool`，用运行期 `sqlx::query` 执行 SQL。
//!     JSON 字段（vector_clock）用 serde_json 序列化为紧凑 JSON 读写。
//!     datetime 字段以 String 透传（兼容有无时区格式）。
//!     deleted 为软删除（deleted=1）。
//!     scan_state 表记录每个 jsonl 文件的 (mtime_sec, size)，采集器据此增量跳过未变文件。

use crate::cc::models::{CcProjectDto, ClaudeHistoryRow};
use crate::error::AppError;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::collections::HashMap;

/// Claude Code 历史仓库，封装所有 claude_history / claude_history_scan_state 表操作。
pub struct ClaudeHistoryRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
}

impl ClaudeHistoryRepo {
    /// 构造仓库。
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    /// 将数据库一行映射为 ClaudeHistoryRow（vector_clock JSON 反序列化、deleted int→bool）。
    fn row_to_claude_history(row: &SqliteRow) -> Result<ClaudeHistoryRow, AppError> {
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        Ok(ClaudeHistoryRow {
            id: row.try_get("id")?,
            project_path: row.try_get("project_path")?,
            project_name: row.try_get("project_name")?,
            session_id: row.try_get("session_id")?,
            content: row.try_get("content")?,
            git_branch: row.try_get("git_branch")?,
            cc_version: row.try_get("cc_version")?,
            occurred_at: row.try_get("occurred_at")?,
            device_id: row.try_get("device_id")?,
            vector_clock,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            deleted: deleted_int != 0,
        })
    }

    /// 按项目聚合列表（排除已删除），按最近活动时间降序。
    ///
    /// Business Logic: 前端项目侧边栏展示所有有过 Claude Code 历史的项目及数量。
    /// Code Logic: GROUP BY project_path，COUNT + MAX(occurred_at)，ORDER BY last_at DESC。
    pub async fn list_projects(&self) -> Result<Vec<CcProjectDto>, AppError> {
        let rows = sqlx::query(
            "SELECT project_path, project_name, COUNT(*) AS cnt, MAX(occurred_at) AS last_at \
             FROM claude_history WHERE deleted = 0 \
             GROUP BY project_path ORDER BY last_at DESC",
        )
        .fetch_all(&self.db)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let cnt: i64 = r.try_get("cnt")?;
            out.push(CcProjectDto {
                project_path: r.try_get("project_path")?,
                project_name: r.try_get("project_name")?,
                count: cnt as u64,
                last_occurred_at: r.try_get("last_at")?,
            });
        }
        Ok(out)
    }

    /// 按项目列出历史 prompt（排除已删除），可选内容搜索，按 occurred_at 降序，限 500 条。
    ///
    /// Business Logic: 前端进入某项目后展示该项目的 prompt 列表，支持关键词过滤。
    /// Code Logic: WHERE project_path=? AND deleted=0 [AND content LIKE ?] ORDER BY occurred_at DESC LIMIT 500。
    pub async fn list_by_project(
        &self,
        project_path: &str,
        search: Option<&str>,
    ) -> Result<Vec<ClaudeHistoryRow>, AppError> {
        let rows = if let Some(kw) = search {
            let pattern = format!("%{}%", kw);
            sqlx::query(
                "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
                 occurred_at, device_id, vector_clock, created_at, updated_at, deleted \
                 FROM claude_history WHERE project_path = ? AND deleted = 0 AND content LIKE ? \
                 ORDER BY occurred_at DESC LIMIT 500",
            )
            .bind(project_path)
            .bind(&pattern)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query(
                "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
                 occurred_at, device_id, vector_clock, created_at, updated_at, deleted \
                 FROM claude_history WHERE project_path = ? AND deleted = 0 \
                 ORDER BY occurred_at DESC LIMIT 500",
            )
            .bind(project_path)
            .fetch_all(&self.db)
            .await?
        };
        rows.iter().map(Self::row_to_claude_history).collect()
    }

    /// 按主键查询单条历史（含已删除记录，供命令层判断存在性与软删除读取）。
    pub async fn get(&self, id: &str) -> Result<Option<ClaudeHistoryRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
             occurred_at, device_id, vector_clock, created_at, updated_at, deleted \
             FROM claude_history WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await?;
        match row {
            Some(r) => Ok(Some(Self::row_to_claude_history(&r)?)),
            None => Ok(None),
        }
    }

    /// 返回全部历史（含 deleted 软删除记录），用于跨设备同步。
    ///
    /// Business Logic: 同步必须传播删除事件，故需读取含 deleted=1 的全部记录。
    /// Code Logic: SELECT 全字段（无 deleted 过滤），不排序（同步用，顺序无关）。
    pub async fn get_all_for_sync(&self) -> Result<Vec<ClaudeHistoryRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
             occurred_at, device_id, vector_clock, created_at, updated_at, deleted \
             FROM claude_history",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter().map(Self::row_to_claude_history).collect()
    }

    /// 批量插入/更新（按 id 主键，INSERT OR REPLACE），用于同步 push 落库。
    ///
    /// Business Logic: 同步引擎从对端拉取并合并后的历史需批量写入本地，已存在则覆盖
    ///     （合并决策已由 merger 在调用前完成，此处直接 REPLACE）。
    /// Code Logic: 空切片直接返回；否则逐条 INSERT OR REPLACE。
    pub async fn bulk_upsert(&self, items: &[ClaudeHistoryRow]) -> Result<(), AppError> {
        if items.is_empty() {
            return Ok(());
        }
        for p in items {
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO claude_history \
                 (id, project_path, project_name, session_id, content, git_branch, cc_version, \
                  occurred_at, device_id, vector_clock, created_at, updated_at, deleted) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id)
            .bind(&p.project_path)
            .bind(&p.project_name)
            .bind(&p.session_id)
            .bind(&p.content)
            .bind(&p.git_branch)
            .bind(&p.cc_version)
            .bind(&p.occurred_at)
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

    /// 批量采集入库（INSERT OR IGNORE），返回本次实际新插入条数。
    ///
    /// Business Logic: 采集器解析 jsonl 后调用此方法。已存在的 id（同 session+uuid）
    ///     必须跳过——绝不覆盖，否则会把同步合并出的向量时钟因果历史打回 `{device_id:1}`。
    ///     累加每条 rows_affected（IGNORE 时为 0，新增为 1）得到新插入总数。
    /// Code Logic: 空切片直接返回 0；否则逐条 INSERT OR IGNORE，累加 rows_affected。
    pub async fn bulk_ingest(&self, items: &[ClaudeHistoryRow]) -> Result<usize, AppError> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut inserted: usize = 0;
        for p in items {
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            let res = sqlx::query(
                "INSERT OR IGNORE INTO claude_history \
                 (id, project_path, project_name, session_id, content, git_branch, cc_version, \
                  occurred_at, device_id, vector_clock, created_at, updated_at, deleted) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&p.id)
            .bind(&p.project_path)
            .bind(&p.project_name)
            .bind(&p.session_id)
            .bind(&p.content)
            .bind(&p.git_branch)
            .bind(&p.cc_version)
            .bind(&p.occurred_at)
            .bind(&p.device_id)
            .bind(vc_text)
            .bind(&p.created_at)
            .bind(&p.updated_at)
            .bind(p.deleted as i64)
            .execute(&self.db)
            .await?;
            inserted += res.rows_affected() as usize;
        }
        Ok(inserted)
    }

    /// 软删除：标记 deleted=1，更新 updated_at，并写入推进后的 vector_clock。
    ///
    /// Business Logic: 用户在前端删除某条历史是一次写入，需推进本端 vector_clock 使对端感知。
    /// Code Logic: UPDATE deleted=1, updated_at=?, vector_clock=? WHERE id=?。
    pub async fn soft_delete(
        &self,
        id: &str,
        now: &str,
        vector_clock: &HashMap<String, u64>,
    ) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(vector_clock)?;
        sqlx::query(
            "UPDATE claude_history SET deleted = 1, updated_at = ?, vector_clock = ? WHERE id = ?",
        )
        .bind(now)
        .bind(vc_text)
        .bind(id)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 更新某 jsonl 文件的扫描状态（mtime/size/scanned_at），用于增量去重。
    ///
    /// Business Logic: 采集器每扫完一个文件记录其 (mtime, size)，下次扫描比对，未变则跳过。
    /// Code Logic: INSERT OR REPLACE（file_path 主键）。
    pub async fn update_scan_state(
        &self,
        file_path: &str,
        mtime_sec: i64,
        size: i64,
        scanned_at: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT OR REPLACE INTO claude_history_scan_state \
             (file_path, mtime_sec, size, scanned_at) VALUES (?, ?, ?, ?)",
        )
        .bind(file_path)
        .bind(mtime_sec)
        .bind(size)
        .bind(scanned_at)
        .execute(&self.db)
        .await?;
        Ok(())
    }

    /// 读取全部扫描状态，返回 {file_path: (mtime_sec, size)}，供采集器增量比对。
    pub async fn get_scan_states(&self) -> Result<HashMap<String, (i64, i64)>, AppError> {
        let rows = sqlx::query("SELECT file_path, mtime_sec, size FROM claude_history_scan_state")
            .fetch_all(&self.db)
            .await?;
        let mut out = HashMap::with_capacity(rows.len());
        for r in &rows {
            let file_path: String = r.try_get("file_path")?;
            let mtime_sec: i64 = r.try_get("mtime_sec")?;
            let size: i64 = r.try_get("size")?;
            out.insert(file_path, (mtime_sec, size));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    //! cc_history_repo 单测：用内存 SQLite 验证 bulk_ingest (IGNORE 不覆盖) 与
    //! bulk_upsert (REPLACE 覆盖) 的关键差异，以及 list_projects / list_by_project /
    //! soft_delete / get_all_for_sync 的基本行为。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;

    /// 构造内存 SQLite 并建好 claude_history + scan_state 表，返回仓库。
    async fn setup_repo() -> ClaudeHistoryRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS claude_history (\
             id TEXT PRIMARY KEY, project_path TEXT NOT NULL, project_name TEXT NOT NULL, \
             session_id TEXT NOT NULL, content TEXT NOT NULL, git_branch TEXT, cc_version TEXT, \
             occurred_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS claude_history_scan_state (\
             file_path TEXT PRIMARY KEY, mtime_sec INTEGER NOT NULL, size INTEGER NOT NULL, \
             scanned_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        ClaudeHistoryRepo::new(pool)
    }

    /// 构造一条测试 Row。
    fn row(id: &str, project: &str, content: &str, vc_counter: u64) -> ClaudeHistoryRow {
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), vc_counter);
        ClaudeHistoryRow {
            id: id.to_string(),
            project_path: project.to_string(),
            project_name: project.to_string(),
            session_id: "s1".to_string(),
            content: content.to_string(),
            git_branch: None,
            cc_version: None,
            occurred_at: "2024-01-01T00:00:00+00:00".to_string(),
            device_id: "d1".to_string(),
            vector_clock: vc,
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: "2024-01-01T00:00:00+00:00".to_string(),
            deleted: false,
        }
    }

    #[tokio::test]
    async fn bulk_ingest_inserts_new_and_ignores_existing() {
        // 首次入库 2 条 → 返回 2
        let repo = setup_repo().await;
        let items = vec![row("a", "/p", "hello", 1), row("b", "/p", "world", 1)];
        let n = repo.bulk_ingest(&items).await.unwrap();
        assert_eq!(n, 2);

        // 再次 ingest 同 id（即便 content/vc 不同）→ IGNORE，返回 0，原内容不被覆盖
        let items2 = vec![row("a", "/p", "CHANGED", 9)];
        let n2 = repo.bulk_ingest(&items2).await.unwrap();
        assert_eq!(n2, 0);
        let got = repo.get("a").await.unwrap().unwrap();
        assert_eq!(got.content, "hello"); // 仍是原始内容
        assert_eq!(got.vector_clock.get("d1"), Some(&1)); // 时钟未被覆盖
    }

    #[tokio::test]
    async fn bulk_upsert_replaces_existing() {
        // upsert 已存在 id → 覆盖内容与时钟
        let repo = setup_repo().await;
        repo.bulk_ingest(&[row("a", "/p", "hello", 1)])
            .await
            .unwrap();
        repo.bulk_upsert(&[row("a", "/p", "CHANGED", 9)])
            .await
            .unwrap();
        let got = repo.get("a").await.unwrap().unwrap();
        assert_eq!(got.content, "CHANGED");
        assert_eq!(got.vector_clock.get("d1"), Some(&9));
    }

    #[tokio::test]
    async fn list_projects_aggregates_counts() {
        let repo = setup_repo().await;
        repo.bulk_ingest(&[
            row("a", "/p1", "x", 1),
            row("b", "/p1", "y", 1),
            row("c", "/p2", "z", 1),
        ])
        .await
        .unwrap();
        let projects = repo.list_projects().await.unwrap();
        // 两个项目
        assert_eq!(projects.len(), 2);
        // 找到 p1 的聚合 count=2
        let p1 = projects.iter().find(|p| p.project_path == "/p1").unwrap();
        assert_eq!(p1.count, 2);
        let p2 = projects.iter().find(|p| p.project_path == "/p2").unwrap();
        assert_eq!(p2.count, 1);
    }

    #[tokio::test]
    async fn list_by_project_supports_search() {
        let repo = setup_repo().await;
        repo.bulk_ingest(&[
            row("a", "/p", "hello world", 1),
            row("b", "/p", "foo bar", 1),
        ])
        .await
        .unwrap();
        // 无搜索：2 条
        let all = repo.list_by_project("/p", None).await.unwrap();
        assert_eq!(all.len(), 2);
        // 搜索 hello：1 条
        let filtered = repo.list_by_project("/p", Some("hello")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }

    #[tokio::test]
    async fn soft_delete_marks_deleted_and_updates_clock() {
        let repo = setup_repo().await;
        repo.bulk_ingest(&[row("a", "/p", "hello", 1)])
            .await
            .unwrap();
        let mut vc = HashMap::new();
        vc.insert("d1".to_string(), 2u64);
        repo.soft_delete("a", "2024-01-02T00:00:00+00:00", &vc)
            .await
            .unwrap();
        // get 能取到（含 deleted），list_by_project 过滤掉
        let got = repo.get("a").await.unwrap().unwrap();
        assert!(got.deleted);
        assert_eq!(got.vector_clock.get("d1"), Some(&2));
        let listed = repo.list_by_project("/p", None).await.unwrap();
        assert!(listed.is_empty());
        // get_all_for_sync 仍含已删除（同步需传播删除）
        let synced = repo.get_all_for_sync().await.unwrap();
        assert_eq!(synced.len(), 1);
    }

    #[tokio::test]
    async fn scan_state_roundtrip() {
        let repo = setup_repo().await;
        repo.update_scan_state("/a.jsonl", 100, 2048, "2024-01-01T00:00:00+00:00")
            .await
            .unwrap();
        let states = repo.get_scan_states().await.unwrap();
        assert_eq!(states.get("/a.jsonl"), Some(&(100, 2048)));
        // 更新同 file_path → REPLACE
        repo.update_scan_state("/a.jsonl", 200, 4096, "2024-01-02T00:00:00+00:00")
            .await
            .unwrap();
        let states2 = repo.get_scan_states().await.unwrap();
        assert_eq!(states2.get("/a.jsonl"), Some(&(200, 4096)));
    }
}
