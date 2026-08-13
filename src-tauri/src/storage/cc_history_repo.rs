//! storage/cc_history_repo.rs — Claude Code 历史数据访问层
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的 Claude Code 用户输入 prompt 需要按项目归类持久化，供前端检索、
//!     并供同步引擎批量 upsert / 拉取同步摘要。采集与同步对"已存在行"的处理语义不同：
//!     采集必须 INSERT OR IGNORE（绝不覆盖已存在行，否则会把同步合并出的向量时钟因果历史
//!     打回 `{device_id:1}`）；同步 push 用 INSERT OR REPLACE（覆盖式写合并结果）。
//!     两者严格分离由不同方法承担。
//!     分页同步新增：manifest keyset 分页、按 ID 批量取正文、事务性 merged batch upsert，
//!     避免全量摘要/N+1 get/无事务半完成写入。
//!
//! Code Logic（这个模块做什么）:
//!     持有 `SqlitePool`，用运行期 `sqlx::query` 执行 SQL。
//!     JSON 字段（vector_clock）用 serde_json 序列化为紧凑 JSON 读写。
//!     datetime 字段以 String 透传（兼容有无时区格式）。
//!     deleted 为软删除（deleted=1）。
//!     scan_state 表记录每个 jsonl 文件的 (mtime_sec, size)，采集器据此增量跳过未变文件。
//!     `list_sync_manifest_page` 用 `WHERE id > ? ORDER BY id ASC LIMIT ?`（limit 1..=512）；
//!     `get_many_for_sync` 动态 IN（非空且 ≤128）；`upsert_merged_batch`/`bulk_ingest`
//!     经 `begin_shared_write` 显式 commit，任一行失败整批 rollback。

use crate::cc::models::{CcProjectDto, CcSyncSummary, ClaudeHistoryRow};
use crate::cc::project_identity::canonical_project_path;
use crate::error::AppError;
use crate::storage::maintenance_gate::{
    begin_shared_write, with_shared_write_lease, DatabaseMaintenanceGate,
};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 同步 manifest 分页 limit 上限（与协议 `CC_MANIFEST_PAGE_LIMIT_MAX` 对齐）。
const CC_MANIFEST_PAGE_LIMIT_MAX: u32 = 512;
/// 按 ID 批量取正文时最多绑定的 ID 数（与协议 `CC_ITEM_BATCH_LIMIT` 对齐）。
const CC_ITEM_BATCH_LIMIT: usize = 128;

/// Claude Code 历史仓库，封装所有 claude_history / claude_history_scan_state 表操作。
pub struct ClaudeHistoryRepo {
    /// SQLite 连接池（max_connections(1)，单连接语义）
    db: SqlitePool,
    /// 与 restore exclusive 共享的写屏障。
    gate: Arc<DatabaseMaintenanceGate>,
}

impl ClaudeHistoryRepo {
    /// 兼容构造：测试/局部 fixture 用独立 gate。
    ///
    /// Business Logic: 单测与局部 fixture 不依赖 AppState 共享 gate。
    /// Code Logic: 新建独立 DatabaseMaintenanceGate。
    pub fn new(db: SqlitePool) -> Self {
        Self::with_gate(db, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic: 所有生产 writer 与 restore exclusive 共享屏障。
    /// Code Logic: 保存 pool + gate。
    pub fn with_gate(db: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { db, gate }
    }

    /// 幂等补齐 `source` 列与索引（旧库升级）。
    ///
    /// Business Logic: 多 Agent 采集需要区分来源；旧库无列时不得启动失败。
    /// Code Logic: PRAGMA table_info 缺 source 则 ALTER DEFAULT claude；再建 source 索引。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        let columns = sqlx::query("PRAGMA table_info(claude_history)")
            .fetch_all(pool)
            .await?;
        let mut has_source = false;
        for col in &columns {
            let name: String = col.try_get("name")?;
            if name == "source" {
                has_source = true;
                break;
            }
        }
        if !has_source {
            sqlx::query(
                "ALTER TABLE claude_history ADD COLUMN source TEXT NOT NULL DEFAULT 'claude'",
            )
            .execute(pool)
            .await?;
        }
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_ch_source ON claude_history(source)")
            .execute(pool)
            .await?;
        Ok(())
    }

    /// 将数据库一行映射为 ClaudeHistoryRow（vector_clock JSON 反序列化、deleted int→bool）。
    fn row_to_claude_history(row: &SqliteRow) -> Result<ClaudeHistoryRow, AppError> {
        let vc_text: String = row.try_get("vector_clock")?;
        let deleted_int: i64 = row.try_get("deleted")?;
        let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
        let source: String = row
            .try_get::<String, _>("source")
            .unwrap_or_else(|_| crate::cc::models::SOURCE_CLAUDE.to_string());
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
            source,
        })
    }

    /// 持久迁移历史记录中的 cwd 为稳定主项目路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧版本按原始 cwd 存储历史，导致仓库子目录、当前或已删除 worktree 各自成为项目。
    ///     读取历史项目前必须一次性修正这些旧记录，并推进本机向量时钟让变更可同步收敛。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取 distinct project_path，在 blocking pool 中解析 Git 主项目路径；
    ///     有变更时开启 shared write transaction，逐行更新 project_path/project_name/updated_at，
    ///     并递增 `vector_clock[device_id]`。返回实际迁移行数。
    async fn normalize_project_paths(&self, device_id: &str) -> Result<usize, AppError> {
        let path_rows = sqlx::query("SELECT DISTINCT project_path FROM claude_history")
            .fetch_all(&self.db)
            .await?;
        let project_paths = path_rows
            .iter()
            .map(|row| row.try_get::<String, _>("project_path"))
            .collect::<Result<Vec<_>, _>>()?;
        let replacements = tokio::task::spawn_blocking(move || {
            project_paths
                .into_iter()
                .filter_map(|source_path| {
                    let target_path = canonical_project_path(&source_path);
                    (target_path != source_path).then_some((source_path, target_path))
                })
                .collect::<HashMap<_, _>>()
        })
        .await
        .map_err(|error| AppError::generic(format!("Claude 历史项目路径迁移失败: {error}")))?;
        if replacements.is_empty() {
            return Ok(0);
        }

        let now = chrono::Utc::now().to_rfc3339();
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let history_rows = sqlx::query("SELECT id, project_path, vector_clock FROM claude_history")
            .fetch_all(&mut *tx)
            .await?;
        let mut migrated = 0usize;
        for history_row in &history_rows {
            let source_path: String = history_row.try_get("project_path")?;
            let Some(target_path) = replacements.get(&source_path) else {
                continue;
            };
            let id: String = history_row.try_get("id")?;
            let vector_clock_text: String = history_row.try_get("vector_clock")?;
            let mut vector_clock: HashMap<String, u64> = serde_json::from_str(&vector_clock_text)?;
            *vector_clock.entry(device_id.to_string()).or_insert(0) += 1;
            let target_name = ClaudeHistoryRow::derive_project_name(target_path);
            let updated_vector_clock = serde_json::to_string(&vector_clock)?;
            sqlx::query(
                "UPDATE claude_history \
                 SET project_path = ?, project_name = ?, updated_at = ?, vector_clock = ? \
                 WHERE id = ?",
            )
            .bind(target_path)
            .bind(target_name)
            .bind(&now)
            .bind(updated_vector_clock)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            migrated += 1;
        }
        tx.commit().await?;
        drop(permit);
        Ok(migrated)
    }

    /// 按主项目聚合列表（排除已删除），按最近活动时间降序。
    ///
    /// Business Logic:
    ///     前端项目侧边栏应把同一个 Git 项目的主工作区与全部 worktree 视为一个项目，
    ///     避免每创建一个 worktree 就出现一个新的 Claude 历史项目。
    ///
    /// Code Logic:
    ///     先用 Git worktree 清单持久迁移可解析路径，再仅按规范化主路径执行
    ///     COUNT + MAX(occurred_at)，按最近活动时间降序。
    pub async fn list_projects(
        &self,
        local_device_id: &str,
        owner_device_id: &str,
        source: Option<&str>,
    ) -> Result<Vec<CcProjectDto>, AppError> {
        self.normalize_project_paths(local_device_id).await?;
        let rows = if let Some(src) = source {
            sqlx::query(
                "SELECT h.project_path AS project_path, MAX(h.project_name) AS project_name, \
                        COUNT(*) AS cnt, MAX(h.occurred_at) AS last_at \
                 FROM claude_history h \
                 WHERE h.deleted = 0 AND h.device_id = ? AND h.source = ? \
                 GROUP BY h.project_path \
                 ORDER BY last_at DESC",
            )
            .bind(owner_device_id)
            .bind(src)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query(
                "SELECT h.project_path AS project_path, MAX(h.project_name) AS project_name, \
                        COUNT(*) AS cnt, MAX(h.occurred_at) AS last_at \
                 FROM claude_history h \
                 WHERE h.deleted = 0 AND h.device_id = ? \
                 GROUP BY h.project_path \
                 ORDER BY last_at DESC",
            )
            .bind(owner_device_id)
            .fetch_all(&self.db)
            .await?
        };
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

    /// 列出未删除历史中出现过的设备 id，按 id 稳定排序。
    ///
    /// Business Logic:
    ///     前端设备筛选器必须覆盖已同步但当前离线的历史来源，不能只依赖 mDNS 在线设备。
    ///
    /// Code Logic:
    ///     对未删除 claude_history 执行 DISTINCT device_id；显示名由命令层结合运行时设备表补全。
    pub async fn list_device_ids(&self) -> Result<Vec<String>, AppError> {
        let rows = sqlx::query(
            "SELECT DISTINCT device_id FROM claude_history \
             WHERE deleted = 0 ORDER BY device_id ASC",
        )
        .fetch_all(&self.db)
        .await?;
        rows.iter()
            .map(|row| {
                row.try_get::<String, _>("device_id")
                    .map_err(AppError::from)
            })
            .collect()
    }

    /// 按主项目列出历史 prompt（排除已删除），可选内容搜索，按 occurred_at 降序，限 500 条。
    ///
    /// Business Logic:
    ///     用户进入一个 Claude 历史项目后，需要同时看到 Git 报告的主工作区与全部
    ///     linked worktree 中产生的 prompt。
    ///
    /// Code Logic:
    ///     先把可解析的历史 cwd 持久迁移到 Git worktree 清单首项（主工作区），
    ///     再按规范化后的 project_path 精确筛选。
    pub async fn list_by_project(
        &self,
        project_path: &str,
        search: Option<&str>,
        local_device_id: &str,
        owner_device_id: &str,
        source: Option<&str>,
    ) -> Result<Vec<ClaudeHistoryRow>, AppError> {
        self.normalize_project_paths(local_device_id).await?;
        let rows = match (search, source) {
            (Some(kw), Some(src)) => {
                let pattern = format!("%{}%", kw);
                sqlx::query(
                    "SELECT h.id, h.project_path, h.project_name, h.session_id, h.content, \
                            h.git_branch, h.cc_version, h.occurred_at, h.device_id, h.vector_clock, \
                            h.created_at, h.updated_at, h.deleted, h.source \
                     FROM claude_history h \
                     WHERE h.project_path = ? AND h.device_id = ? AND h.deleted = 0 \
                       AND h.source = ? AND h.content LIKE ? \
                     ORDER BY h.occurred_at DESC LIMIT 500",
                )
                .bind(project_path)
                .bind(owner_device_id)
                .bind(src)
                .bind(&pattern)
                .fetch_all(&self.db)
                .await?
            }
            (Some(kw), None) => {
                let pattern = format!("%{}%", kw);
                sqlx::query(
                    "SELECT h.id, h.project_path, h.project_name, h.session_id, h.content, \
                            h.git_branch, h.cc_version, h.occurred_at, h.device_id, h.vector_clock, \
                            h.created_at, h.updated_at, h.deleted, h.source \
                     FROM claude_history h \
                     WHERE h.project_path = ? AND h.device_id = ? AND h.deleted = 0 AND h.content LIKE ? \
                     ORDER BY h.occurred_at DESC LIMIT 500",
                )
                .bind(project_path)
                .bind(owner_device_id)
                .bind(&pattern)
                .fetch_all(&self.db)
                .await?
            }
            (None, Some(src)) => {
                sqlx::query(
                    "SELECT h.id, h.project_path, h.project_name, h.session_id, h.content, \
                            h.git_branch, h.cc_version, h.occurred_at, h.device_id, h.vector_clock, \
                            h.created_at, h.updated_at, h.deleted, h.source \
                     FROM claude_history h \
                     WHERE h.project_path = ? AND h.device_id = ? AND h.deleted = 0 AND h.source = ? \
                     ORDER BY h.occurred_at DESC LIMIT 500",
                )
                .bind(project_path)
                .bind(owner_device_id)
                .bind(src)
                .fetch_all(&self.db)
                .await?
            }
            (None, None) => {
                sqlx::query(
                    "SELECT h.id, h.project_path, h.project_name, h.session_id, h.content, \
                            h.git_branch, h.cc_version, h.occurred_at, h.device_id, h.vector_clock, \
                            h.created_at, h.updated_at, h.deleted, h.source \
                     FROM claude_history h \
                     WHERE h.project_path = ? AND h.device_id = ? AND h.deleted = 0 \
                     ORDER BY h.occurred_at DESC LIMIT 500",
                )
                .bind(project_path)
                .bind(owner_device_id)
                .fetch_all(&self.db)
                .await?
            }
        };
        rows.iter().map(Self::row_to_claude_history).collect()
    }

    /// 按主键查询单条历史（含已删除记录，供命令层判断存在性与软删除读取）。
    pub async fn get(&self, id: &str) -> Result<Option<ClaudeHistoryRow>, AppError> {
        let row = sqlx::query(
            "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
             occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source \
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
             occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source \
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
    /// Code Logic: 空切片直接返回；否则在 shared lease 下逐条 INSERT OR REPLACE。
    pub async fn bulk_upsert(&self, items: &[ClaudeHistoryRow]) -> Result<(), AppError> {
        if items.is_empty() {
            return Ok(());
        }
        with_shared_write_lease(&self.gate, async {
            for p in items {
                let vc_text = serde_json::to_string(&p.vector_clock)?;
                sqlx::query(
                    "INSERT OR REPLACE INTO claude_history \
                     (id, project_path, project_name, session_id, content, git_branch, cc_version, \
                      occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                .bind(&p.source)
                .execute(&self.db)
                .await?;
            }
            Ok::<(), AppError>(())
        })
        .await
    }

    /// 批量采集入库（INSERT OR IGNORE），返回本次实际新插入条数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     采集器解析 jsonl 后调用此方法。已存在的 id（同 session+uuid）
    ///     必须跳过——绝不覆盖，否则会把同步合并出的向量时钟因果历史打回 `{device_id:1}`。
    ///     累加每条 rows_affected（IGNORE 时为 0，新增为 1）得到新插入总数。
    ///     整批包在同一显式事务中，避免半完成写入，但语义仍是 IGNORE 而非 REPLACE。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空切片直接返回 0；否则 `begin_shared_write`，逐条跳过 content>1MiB 的毒丸后
    ///     INSERT OR IGNORE 累加 rows_affected，全部成功后 `commit`；任一行失败则事务 drop 回滚。
    pub async fn bulk_ingest(&self, items: &[ClaudeHistoryRow]) -> Result<usize, AppError> {
        if items.is_empty() {
            return Ok(0);
        }
        // 与 paged 协议 content 上限对齐，阻止采集路径写入毒丸（避免 peer items 整批 422）。
        const CONTENT_MAX_BYTES: usize = 1024 * 1024;
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let mut inserted: usize = 0;
        for p in items {
            if p.content.len() > CONTENT_MAX_BYTES {
                // 不写 id/正文；仅记字节数便于本地诊断。
                tracing::warn!(
                    content_bytes = p.content.len(),
                    "bulk_ingest 跳过超限 content（>1MiB）"
                );
                continue;
            }
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            let res = sqlx::query(
                "INSERT OR IGNORE INTO claude_history \
                 (id, project_path, project_name, session_id, content, git_branch, cc_version, \
                      occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(&p.source)
            .execute(&mut *tx)
            .await?;
            inserted += res.rows_affected() as usize;
        }
        tx.commit().await?;
        drop(permit);
        Ok(inserted)
    }

    /// 按主键 keyset 分页返回同步摘要（仅 id + vector_clock）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新分页同步协议先交换摘要再按需拉正文。客户端用 after_id 游标翻页，
    ///     不得一次返回全部摘要以免 10k+ 行时 body/内存无界。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 `limit ∈ 1..=512`；`after_id=None` 时 `ORDER BY id ASC LIMIT ?`，
    ///     否则 `WHERE id > ? ORDER BY id ASC LIMIT ?`。含 deleted 行（同步需传播删除）。
    ///     仅 SELECT id/vector_clock，反序列化后组装 `CcSyncSummary`。
    pub async fn list_sync_manifest_page(
        &self,
        after_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CcSyncSummary>, AppError> {
        if !(1..=CC_MANIFEST_PAGE_LIMIT_MAX).contains(&limit) {
            return Err(AppError::validation(format!(
                "manifest 分页 limit 必须在 1..={CC_MANIFEST_PAGE_LIMIT_MAX}，收到 {limit}"
            )));
        }
        let rows = if let Some(after) = after_id {
            sqlx::query(
                "SELECT id, vector_clock FROM claude_history \
                 WHERE id > ? ORDER BY id ASC LIMIT ?",
            )
            .bind(after)
            .bind(limit as i64)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query(
                "SELECT id, vector_clock FROM claude_history \
                 ORDER BY id ASC LIMIT ?",
            )
            .bind(limit as i64)
            .fetch_all(&self.db)
            .await?
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows {
            let id: String = r.try_get("id")?;
            let vc_text: String = r.try_get("vector_clock")?;
            let vector_clock: HashMap<String, u64> = serde_json::from_str(&vc_text)?;
            out.push(CcSyncSummary { id, vector_clock });
        }
        Ok(out)
    }

    /// 按 ID 批量读取完整历史行（含 deleted），供同步 items 路由使用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     客户端比较 manifest 后需要按批拉取远端领先/并发/本地缺失的完整 rows。
    ///     单次最多 128 个 ID，避免 N+1 `get(id)` 与无界 IN 列表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空切片返回空 HashMap；长度 >128 返回 Validation。
    ///     动态构造 `WHERE id IN (?,?,...)`，绑定全部 id，按 id 建 HashMap；
    ///     缺失 id 不出现在 map 中（由调用方对照请求列表计算 missing）。
    pub async fn get_many_for_sync(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, ClaudeHistoryRow>, AppError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        if ids.len() > CC_ITEM_BATCH_LIMIT {
            return Err(AppError::validation(format!(
                "get_many_for_sync 最多 {CC_ITEM_BATCH_LIMIT} 个 id，收到 {}",
                ids.len()
            )));
        }
        let placeholders = (0..ids.len()).map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT id, project_path, project_name, session_id, content, git_branch, cc_version, \
             occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source \
             FROM claude_history WHERE id IN ({placeholders})"
        );
        let mut query = sqlx::query(&sql);
        for id in ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(&self.db).await?;
        let mut out = HashMap::with_capacity(rows.len());
        for r in &rows {
            let row = Self::row_to_claude_history(r)?;
            out.insert(row.id.clone(), row);
        }
        Ok(out)
    }

    /// 将已合并的历史批次事务性 REPLACE 入库，返回写入条数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同步 push-batch 在调用方完成 `merge_cc_history` 后，需要把合并结果一次性落库。
    ///     任一行失败必须整批 rollback，禁止半完成 accepted（与逐条无事务 bulk_upsert 不同）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空切片返回 0；否则显式 begin，逐条 INSERT OR REPLACE，全部成功后 commit；
    ///     中途 Err 时 Transaction drop 自动 rollback，调用方看到错误且 earlier rows 未提交。
    ///     语义与 bulk_upsert 相同（REPLACE），与 bulk_ingest 的 IGNORE 严格分离。
    pub async fn upsert_merged_batch(&self, items: &[ClaudeHistoryRow]) -> Result<usize, AppError> {
        self.upsert_merged_batch_inner(items, None).await
    }

    /// 事务性 REPLACE；可选注入失败点，供 scale_safety / quality_faults 等 rollback 回归。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     集成测试必须调用与生产相同的 `upsert_merged_batch` 事务边界，
    ///     而不能手写 begin+INSERT 假装覆盖了产品写路径。
    ///     本 inject seam 仅 debug/test-only：`cfg(test)` 或 `debug_assertions` 下编译，
    ///     release 构建（`not(debug_assertions)` 且无 test）会剥离，禁止生产路径注入失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `upsert_merged_batch_inner(items, inject_fail_at)`；
    ///     生产 `upsert_merged_batch` 始终可用且固定传 `None`。
    #[cfg(any(test, debug_assertions))]
    pub async fn upsert_merged_batch_inject_fail_at(
        &self,
        items: &[ClaudeHistoryRow],
        inject_fail_at: Option<usize>,
    ) -> Result<usize, AppError> {
        self.upsert_merged_batch_inner(items, inject_fail_at).await
    }

    /// 事务性 REPLACE 实现；`inject_fail_at` 仅测试用于模拟中途失败。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产路径与注入失败路径共享同一事务边界，保证单测验证的 rollback 语义
    ///     就是真实 `upsert_merged_batch` 的语义，而不是另一套写路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     begin_shared_write → 逐条序列化 vector_clock + INSERT OR REPLACE；
    ///     当 `inject_fail_at == Some(idx)` 且 idx 命中时返回错误（模拟序列化/写入失败），
    ///     不 commit，tx drop 回滚；否则 commit 并返回写入条数。
    async fn upsert_merged_batch_inner(
        &self,
        items: &[ClaudeHistoryRow],
        inject_fail_at: Option<usize>,
    ) -> Result<usize, AppError> {
        if items.is_empty() {
            return Ok(0);
        }
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let mut written: usize = 0;
        for (idx, p) in items.iter().enumerate() {
            if inject_fail_at == Some(idx) {
                // 模拟 vector_clock 序列化/行写入失败；不 commit，tx drop 回滚已写行。
                return Err(AppError::generic(
                    "injected invalid vector_clock serialization failure",
                ));
            }
            let vc_text = serde_json::to_string(&p.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO claude_history \
                 (id, project_path, project_name, session_id, content, git_branch, cc_version, \
                      occurred_at, device_id, vector_clock, created_at, updated_at, deleted, source) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(&p.source)
            .execute(&mut *tx)
            .await?;
            written += 1;
        }
        tx.commit().await?;
        drop(permit);
        Ok(written)
    }

    /// 软删除：标记 deleted=1，更新 updated_at，并写入推进后的 vector_clock。
    ///
    /// Business Logic: 用户在前端删除某条历史是一次写入，需推进本端 vector_clock 使对端感知。
    /// Code Logic: shared lease 下 UPDATE deleted=1, updated_at=?, vector_clock=? WHERE id=?。
    pub async fn soft_delete(
        &self,
        id: &str,
        now: &str,
        vector_clock: &HashMap<String, u64>,
    ) -> Result<(), AppError> {
        let vc_text = serde_json::to_string(vector_clock)?;
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE claude_history SET deleted = 1, updated_at = ?, vector_clock = ? WHERE id = ?",
            )
            .bind(now)
            .bind(vc_text)
            .bind(id)
            .execute(&self.db)
            .await?;
            Ok::<(), AppError>(())
        })
        .await
    }

    /// 批量软删除被采集器确认由 Claude 生成的历史项。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧采集规则曾把子 Agent 指令、系统通知和压缩摘要当成用户 prompt 入库；
    ///     新规则重扫 transcript 后必须清理存量数据，并以墓碑同步到其它设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单事务读取未删除历史，仅处理命中 ids 的行；逐条递增本机 vector_clock，
    ///     写 deleted=1 与统一更新时间，返回实际清理数量。
    pub async fn soft_delete_generated_ids(
        &self,
        ids: &HashSet<String>,
        device_id: &str,
    ) -> Result<usize, AppError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let (permit, mut tx) = begin_shared_write(&self.db, &self.gate).await?;
        let rows = sqlx::query("SELECT id, vector_clock FROM claude_history WHERE deleted = 0")
            .fetch_all(&mut *tx)
            .await?;
        let mut removed = 0usize;
        for row in &rows {
            let id: String = row.try_get("id")?;
            if !ids.contains(&id) {
                continue;
            }
            let vector_clock_text: String = row.try_get("vector_clock")?;
            let mut vector_clock: HashMap<String, u64> = serde_json::from_str(&vector_clock_text)?;
            *vector_clock.entry(device_id.to_string()).or_insert(0) += 1;
            sqlx::query(
                "UPDATE claude_history \
                 SET deleted = 1, updated_at = ?, vector_clock = ? \
                 WHERE id = ?",
            )
            .bind(&now)
            .bind(serde_json::to_string(&vector_clock)?)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            removed += 1;
        }
        tx.commit().await?;
        drop(permit);
        Ok(removed)
    }

    /// 更新某 jsonl 文件的扫描状态（mtime/size/scanned_at），用于增量去重。
    ///
    /// Business Logic: 采集器每扫完一个文件记录其 (mtime, size)，下次扫描比对，未变则跳过。
    /// Code Logic: shared lease 下 INSERT OR REPLACE（file_path 主键）。
    pub async fn update_scan_state(
        &self,
        file_path: &str,
        mtime_sec: i64,
        size: i64,
        scanned_at: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
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
            Ok::<(), AppError>(())
        })
        .await
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
    //! bulk_upsert / upsert_merged_batch (REPLACE 覆盖) 的关键差异，
    //! list_sync_manifest_page keyset 分页、get_many_for_sync 批量取正文、
    //! 事务 rollback，以及 list_projects / list_by_project / soft_delete 基本行为。

    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::process::Command;
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
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
             source TEXT NOT NULL DEFAULT 'claude')",
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
            source: crate::cc::models::SOURCE_CLAUDE.to_string(),
        }
    }

    /// 持有真实 Git 主工作区与任意外置 linked worktree 的测试夹具。
    struct GitWorktreeFixture {
        _temp: tempfile::TempDir,
        main: PathBuf,
        linked: PathBuf,
    }

    impl GitWorktreeFixture {
        /// 初始化提交并通过 `git worktree add` 创建 linked worktree。
        fn create() -> Self {
            let temp = tempfile::tempdir().expect("应创建临时目录");
            let main = temp.path().join("main");
            let linked = temp.path().join("linked-anywhere");
            let main_text = main.to_string_lossy().into_owned();
            let linked_text = linked.to_string_lossy().into_owned();
            std::fs::create_dir_all(&main).expect("应创建主仓库目录");
            for args in [
                vec!["init", main_text.as_str()],
                vec!["-C", main_text.as_str(), "config", "user.name", "Test"],
                vec![
                    "-C",
                    main_text.as_str(),
                    "config",
                    "user.email",
                    "test@example.com",
                ],
            ] {
                let output = Command::new("git").args(args).output().expect("应执行 git");
                assert!(output.status.success());
            }
            std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
            for args in [
                vec!["-C", main_text.as_str(), "add", "README.md"],
                vec!["-C", main_text.as_str(), "commit", "-m", "init"],
                vec![
                    "-C",
                    main_text.as_str(),
                    "worktree",
                    "add",
                    "-b",
                    "feature",
                    linked_text.as_str(),
                ],
            ] {
                let output = Command::new("git").args(args).output().expect("应执行 git");
                assert!(output.status.success());
            }
            Self {
                _temp: temp,
                main: main.canonicalize().expect("主仓库应可规范化"),
                linked: linked.canonicalize().expect("linked worktree 应可规范化"),
            }
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
        let projects = repo.list_projects("d1", "d1", None).await.unwrap();
        // 两个项目
        assert_eq!(projects.len(), 2);
        // 找到 p1 的聚合 count=2
        let p1 = projects.iter().find(|p| p.project_path == "/p1").unwrap();
        assert_eq!(p1.count, 2);
        let p2 = projects.iter().find(|p| p.project_path == "/p2").unwrap();
        assert_eq!(p2.count, 1);
    }

    #[tokio::test]
    async fn list_projects_and_prompts_filter_by_owner_device() {
        let repo = setup_repo().await;
        let local = row("local", "/shared", "local prompt", 1);
        let mut remote = row("remote", "/shared", "remote prompt", 1);
        remote.device_id = "d2".to_string();
        remote.vector_clock = HashMap::from([("d2".to_string(), 1)]);
        repo.bulk_ingest(&[local, remote]).await.unwrap();

        let device_ids = repo.list_device_ids().await.unwrap();
        assert_eq!(device_ids, vec!["d1", "d2"]);

        let local_projects = repo.list_projects("d1", "d1", None).await.unwrap();
        let remote_projects = repo.list_projects("d1", "d2", None).await.unwrap();
        assert_eq!(local_projects[0].count, 1);
        assert_eq!(remote_projects[0].count, 1);

        let local_prompts = repo
            .list_by_project("/shared", None, "d1", "d1", None)
            .await
            .unwrap();
        let remote_prompts = repo
            .list_by_project("/shared", None, "d1", "d2", None)
            .await
            .unwrap();
        assert_eq!(local_prompts[0].id, "local");
        assert_eq!(remote_prompts[0].id, "remote");
    }

    #[tokio::test]
    async fn list_projects_groups_git_discovered_worktrees_under_main_project() {
        let repo = setup_repo().await;
        let git = GitWorktreeFixture::create();
        let main = git.main.to_string_lossy().into_owned();
        let linked = git.linked.to_string_lossy().into_owned();
        repo.bulk_ingest(&[
            row("main", &main, "main prompt", 1),
            row("feature", &linked, "feature prompt", 1),
        ])
        .await
        .unwrap();

        let projects = repo.list_projects("d1", "d1", None).await.unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project_path, main);
        assert_eq!(projects[0].count, 2);
        let migrated = repo.get("feature").await.unwrap().unwrap();
        assert_eq!(migrated.project_path, projects[0].project_path);
        assert_eq!(migrated.vector_clock.get("d1"), Some(&2));
    }

    #[tokio::test]
    async fn list_projects_does_not_guess_removed_worktree_identity_from_path_name() {
        let repo = setup_repo().await;
        repo.bulk_ingest(&[
            row("main", "/projects/repo", "main prompt", 1),
            row(
                "removed-worktree",
                "/projects/repo/.worktrees/feature-a/apps/api",
                "worktree prompt",
                1,
            ),
        ])
        .await
        .unwrap();

        let projects = repo.list_projects("d1", "d1", None).await.unwrap();
        let migrated = repo.get("removed-worktree").await.unwrap().unwrap();

        assert_eq!(projects.len(), 2);
        assert_eq!(
            migrated.project_path,
            "/projects/repo/.worktrees/feature-a/apps/api"
        );
        assert_eq!(migrated.vector_clock.get("d1"), Some(&1));
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
        let all = repo
            .list_by_project("/p", None, "d1", "d1", None)
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        // 搜索 hello：1 条
        let filtered = repo
            .list_by_project("/p", Some("hello"), "d1", "d1", None)
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "a");
    }

    #[tokio::test]
    async fn list_by_project_includes_prompts_from_git_discovered_worktrees() {
        let repo = setup_repo().await;
        let git = GitWorktreeFixture::create();
        let main = git.main.to_string_lossy().into_owned();
        let linked = git.linked.to_string_lossy().into_owned();
        repo.bulk_ingest(&[
            row("main", &main, "shared keyword", 1),
            row("feature", &linked, "shared keyword", 1),
            row("other", "/projects/other", "shared keyword", 1),
        ])
        .await
        .unwrap();

        let all = repo
            .list_by_project(&main, None, "d1", "d1", None)
            .await
            .unwrap();
        let filtered = repo
            .list_by_project(&main, Some("shared"), "d1", "d1", None)
            .await
            .unwrap();

        let mut all_ids = all.iter().map(|item| item.id.as_str()).collect::<Vec<_>>();
        all_ids.sort_unstable();
        let mut filtered_ids = filtered
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        filtered_ids.sort_unstable();
        assert_eq!(all_ids, vec!["feature", "main"]);
        assert_eq!(filtered_ids, vec!["feature", "main"]);
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
        let listed = repo
            .list_by_project("/p", None, "d1", "d1", None)
            .await
            .unwrap();
        assert!(listed.is_empty());
        // get_all_for_sync 仍含已删除（同步需传播删除）
        let synced = repo.get_all_for_sync().await.unwrap();
        assert_eq!(synced.len(), 1);
    }

    #[tokio::test]
    async fn soft_delete_generated_ids_only_removes_matching_history() {
        let repo = setup_repo().await;
        repo.bulk_ingest(&[
            row("user", "/p", "用户输入", 1),
            row("generated", "/p", "Agent 生成", 1),
        ])
        .await
        .unwrap();
        let ids = HashSet::from(["generated".to_string(), "missing".to_string()]);

        let removed = repo.soft_delete_generated_ids(&ids, "d1").await.unwrap();

        assert_eq!(removed, 1);
        let user = repo.get("user").await.unwrap().unwrap();
        let generated = repo.get("generated").await.unwrap().unwrap();
        assert!(!user.deleted);
        assert!(generated.deleted);
        assert_eq!(generated.vector_clock.get("d1"), Some(&2));
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

    /// 插入 10,001 行后按 after_id keyset 分页，断言精确并集且无重复。
    #[tokio::test]
    async fn list_sync_manifest_page_keyset_covers_10001_without_dupes() {
        let repo = setup_repo().await;
        const TOTAL: usize = 10_001;
        let mut items = Vec::with_capacity(TOTAL);
        for i in 0..TOTAL {
            // 零填充保证字典序与数值序一致
            let id = format!("id-{i:05}");
            items.push(row(&id, "/p", &format!("c{i}"), 1));
        }
        // 分批 ingest 避免单次切片过大（事务仍是逐批）
        for chunk in items.chunks(500) {
            let n = repo.bulk_ingest(chunk).await.unwrap();
            assert_eq!(n, chunk.len());
        }

        let page_limit: u32 = 256;
        let mut after: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        let mut pages = 0usize;
        loop {
            let page = repo
                .list_sync_manifest_page(after.as_deref(), page_limit)
                .await
                .unwrap();
            pages += 1;
            assert!(page.len() <= page_limit as usize);
            if page.is_empty() {
                break;
            }
            // 页内 id 升序
            for w in page.windows(2) {
                assert!(
                    w[0].id < w[1].id,
                    "page not sorted: {} >= {}",
                    w[0].id,
                    w[1].id
                );
            }
            for s in &page {
                assert!(
                    seen.insert(s.id.clone()),
                    "duplicate id across pages: {}",
                    s.id
                );
                assert_eq!(s.vector_clock.get("d1"), Some(&1));
            }
            after = page.last().map(|s| s.id.clone());
            if page.len() < page_limit as usize {
                break;
            }
        }
        assert_eq!(seen.len(), TOTAL, "union must cover all rows");
        // 10_001 / 256 ≈ 40 页，确认没有单次无界全量返回
        assert!(pages >= 40, "expected multi-page walk, got {pages}");
        assert!(pages <= 41, "too many pages: {pages}");
    }

    /// limit 越界返回 Validation。
    #[tokio::test]
    async fn list_sync_manifest_page_rejects_invalid_limit() {
        let repo = setup_repo().await;
        let err0 = repo.list_sync_manifest_page(None, 0).await.unwrap_err();
        assert!(
            matches!(err0, AppError::Validation(_)),
            "limit=0 must be Validation, got {err0:?}"
        );
        let err513 = repo.list_sync_manifest_page(None, 513).await.unwrap_err();
        assert!(
            matches!(err513, AppError::Validation(_)),
            "limit=513 must be Validation, got {err513:?}"
        );
    }

    /// 128 个混合存在/缺失 ID 的 get_many_for_sync。
    #[tokio::test]
    async fn get_many_for_sync_handles_128_mixed_ids() {
        let repo = setup_repo().await;
        // 插入 80 条存在的
        let mut existing = Vec::new();
        for i in 0..80 {
            let id = format!("ex-{i:03}");
            existing.push(row(&id, "/p", &format!("body-{i}"), (i as u64) + 1));
        }
        repo.bulk_ingest(&existing).await.unwrap();

        // 请求 128 个：80 存在 + 48 缺失
        let mut ids = Vec::with_capacity(128);
        for i in 0..80 {
            ids.push(format!("ex-{i:03}"));
        }
        for i in 0..48 {
            ids.push(format!("miss-{i:03}"));
        }
        assert_eq!(ids.len(), 128);

        let map = repo.get_many_for_sync(&ids).await.unwrap();
        assert_eq!(map.len(), 80);
        for i in 0..80 {
            let id = format!("ex-{i:03}");
            let got = map
                .get(&id)
                .unwrap_or_else(|| panic!("missing existing {id}"));
            assert_eq!(got.content, format!("body-{i}"));
            assert_eq!(got.vector_clock.get("d1"), Some(&((i as u64) + 1)));
        }
        for i in 0..48 {
            let id = format!("miss-{i:03}");
            assert!(!map.contains_key(&id), "missing id should not appear: {id}");
        }

        // >128 拒绝
        ids.push("one-more".to_string());
        let err = repo.get_many_for_sync(&ids).await.unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));

        // 空切片 → 空 map
        let empty = repo.get_many_for_sync(&[]).await.unwrap();
        assert!(empty.is_empty());
    }

    /// 注入中途失败：earlier rows 不得提交。
    #[tokio::test]
    async fn upsert_merged_batch_rolls_back_on_injected_failure() {
        let repo = setup_repo().await;
        // 预置一条无关行，失败后仍应存在
        repo.bulk_ingest(&[row("keep", "/p", "keep-content", 1)])
            .await
            .unwrap();

        let batch = vec![
            row("u1", "/p", "first", 2),
            row("u2", "/p", "second", 3),
            row("u3", "/p", "third", 4),
        ];
        // 在 index=1（第二条）注入失败：u1 已写入事务但未 commit
        let err = repo
            .upsert_merged_batch_inner(&batch, Some(1))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("injected"),
            "expected injected failure, got {err}"
        );

        // 整批回滚：u1/u2/u3 都不应存在
        assert!(repo.get("u1").await.unwrap().is_none());
        assert!(repo.get("u2").await.unwrap().is_none());
        assert!(repo.get("u3").await.unwrap().is_none());
        // 预置行仍在
        let keep = repo.get("keep").await.unwrap().unwrap();
        assert_eq!(keep.content, "keep-content");

        // 正常路径：整批提交
        let n = repo.upsert_merged_batch(&batch).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(repo.get("u1").await.unwrap().unwrap().content, "first");
        assert_eq!(repo.get("u3").await.unwrap().unwrap().content, "third");
    }

    /// ingest IGNORE 不覆盖已由 upsert 合并写入的行。
    #[tokio::test]
    async fn bulk_ingest_ignore_does_not_overwrite_merged_rows() {
        let repo = setup_repo().await;
        // 同步路径 REPLACE 写入合并后的时钟/内容
        repo.upsert_merged_batch(&[row("m1", "/p", "merged-content", 9)])
            .await
            .unwrap();
        let before = repo.get("m1").await.unwrap().unwrap();
        assert_eq!(before.content, "merged-content");
        assert_eq!(before.vector_clock.get("d1"), Some(&9));

        // 采集路径 IGNORE：同 id 新内容/时钟不得覆盖
        let n = repo
            .bulk_ingest(&[row("m1", "/p", "collector-raw", 1)])
            .await
            .unwrap();
        assert_eq!(n, 0);
        let after = repo.get("m1").await.unwrap().unwrap();
        assert_eq!(after.content, "merged-content");
        assert_eq!(after.vector_clock.get("d1"), Some(&9));

        // 新 id 仍可 IGNORE 插入
        let n2 = repo
            .bulk_ingest(&[row("m2", "/p", "fresh", 1)])
            .await
            .unwrap();
        assert_eq!(n2, 1);
        assert_eq!(repo.get("m2").await.unwrap().unwrap().content, "fresh");
    }
}
