//! storage/agent_ledger_repo — Agent Metadata Ledger 持久化
//!
//! Business Logic（为什么需要这个模块）:
//!     owning device 需要幂等落库 metadata-only Agent 历史：agent_session_id 唯一，
//!     终态重放只 null-fill 可靠 usage / 更正 endedAt，不新增 row。
//!
//! Code Logic（这个模块做什么）:
//!     封装 `agent_session_ledger`：ensure_schema、finalize、page、clear_all、cleanup_batch、
//!     exists、aggregate 辅助；写路径经 shared lease。

#![allow(dead_code)]

use crate::error::AppError;
use crate::storage::maintenance_gate::{
    begin_shared_write, with_shared_write_lease, DatabaseMaintenanceGate,
};
use crate::workbench::agent_ledger::models::{
    compute_duration_ms, convert_major_to_minor_units, merge_usage_monotonic,
    validate_currency_code, AgentLedgerEntry, AgentLedgerFilters, AgentLedgerFinalizeInput,
    AgentLedgerGroupRow, AgentLedgerOutcome, AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary,
    AgentLedgerTrendPoint, CurrencyAmount, LedgerUsageCoverage, LedgerWindow,
    ReliableUsageSnapshot, TrendBucket,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::collections::BTreeMap;
use std::sync::Arc;

/// agent_session_ledger 表 DDL。
pub const AGENT_SESSION_LEDGER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS agent_session_ledger (
    id TEXT PRIMARY KEY,
    agent_session_id TEXT NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    worktree_id TEXT,
    provider_id TEXT NOT NULL,
    model_id TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    outcome TEXT NOT NULL,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    cost_minor_units INTEGER,
    cost_currency TEXT,
    terminal_title TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)";

/// clear 隐私水位：单行 tombstone，记录最近一次 clear 的 RFC3339 时刻。
///
/// Business Logic（为什么需要这个表）:
///     用户 clear 只删 ledger 行，终态 runtime session 仍保留；若不持久化水位，
///     启动 reconcile 会把清除前的旧 session 重新写回 ledger，破坏隐私删除语义。
///
/// Code Logic（这个表做什么）:
///     id 固定为 1；cleared_before 为 clear 时刻；reconcile/finalize 排除 ended_at ≤ 该值的 session。
pub const AGENT_LEDGER_CLEAR_WATERMARK_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS agent_ledger_clear_watermark (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    cleared_before TEXT NOT NULL
)";

/// project + ended_at 索引。
pub const AGENT_SESSION_LEDGER_PROJECT_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_agent_session_ledger_project_ended \
     ON agent_session_ledger(project_id, ended_at DESC, id)";

/// provider + ended_at 索引。
pub const AGENT_SESSION_LEDGER_PROVIDER_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_agent_session_ledger_provider_ended \
     ON agent_session_ledger(provider_id, ended_at DESC, id)";

/// 全局 ended_at 索引（分页/清理）。
pub const AGENT_SESSION_LEDGER_ENDED_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_agent_session_ledger_ended \
     ON agent_session_ledger(ended_at ASC, id)";

/// 默认页大小。
pub const DEFAULT_PAGE_LIMIT: u32 = 50;
/// 最大页大小。
pub const MAX_PAGE_LIMIT: u32 = 200;
/// 清理单批上限。
pub const CLEANUP_BATCH_LIMIT: u64 = 500;
/// 默认保留天数。
pub const RETENTION_DAYS: i64 = 30;
/// 每 device 硬上限条数。
pub const MAX_LEDGER_ROWS: u64 = 10_000;

const SELECT_COLUMNS: &str =
    "id, agent_session_id, project_id, worktree_id, provider_id, model_id, \
    started_at, ended_at, duration_ms, outcome, input_tokens, output_tokens, \
    cache_read_tokens, cache_write_tokens, cost_minor_units, cost_currency, \
    terminal_title, created_at, updated_at";

/// opaque keyset cursor v1（ended_at DESC, id DESC）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerCursorV1 {
    v: u32,
    ended_at: String,
    id: String,
}

/// 清理批次结果。
///
/// Business Logic（为什么需要这个类型）:
///     retention task 需要知道删除数与是否还有剩余。
///
/// Code Logic（这个类型做什么）:
///     deleted + more_remaining。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentLedgerCleanupResult {
    /// 本批删除行数
    pub deleted: u64,
    /// 是否可能仍有超龄或超 cap 行
    pub more_remaining: bool,
}

/// Agent Metadata Ledger 仓库。
///
/// Business Logic（为什么需要这个结构体）:
///     service/命令/聚合共用同一幂等 finalize 与查询语义。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + maintenance gate。
#[derive(Clone)]
pub struct AgentLedgerRepo {
    pool: SqlitePool,
    gate: Arc<DatabaseMaintenanceGate>,
}

impl AgentLedgerRepo {
    /// 测试/局部 fixture 构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测不需要跨进程 maintenance 锁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     with_gate(pool, 新 gate)。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// 生产构造：共享 AppState.maintenance_gate。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     全部 ordinary writer 与 restore 共用同一 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 pool + Arc gate。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// 返回底层 pool。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     fixture 跨实例验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone pool。
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// 幂等创建表与索引。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧库无迁移框架；CREATE IF NOT EXISTS 即可升级；
    ///     数据库相关修改必须兼容旧库——旧版本 agent_session_ledger 没有 terminal_title 列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     ledger 表 + clear watermark 表 + 三个索引；随后 PRAGMA table_info 检查
    ///     terminal_title 列，缺失时 ALTER TABLE ADD COLUMN（可空，向后兼容）；
    ///     schema bootstrap 不经 write lease。
    pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(AGENT_SESSION_LEDGER_SCHEMA)
            .execute(pool)
            .await?;
        sqlx::query(AGENT_LEDGER_CLEAR_WATERMARK_SCHEMA)
            .execute(pool)
            .await?;
        sqlx::query(AGENT_SESSION_LEDGER_PROJECT_INDEX)
            .execute(pool)
            .await?;
        sqlx::query(AGENT_SESSION_LEDGER_PROVIDER_INDEX)
            .execute(pool)
            .await?;
        sqlx::query(AGENT_SESSION_LEDGER_ENDED_INDEX)
            .execute(pool)
            .await?;
        let columns = sqlx::query("PRAGMA table_info(agent_session_ledger)")
            .fetch_all(pool)
            .await?;
        let has_terminal_title = columns
            .iter()
            .any(|row| row.try_get::<String, _>("name").ok().as_deref() == Some("terminal_title"));
        if !has_terminal_title {
            sqlx::query("ALTER TABLE agent_session_ledger ADD COLUMN terminal_title TEXT")
                .execute(pool)
                .await?;
        }
        Ok(())
    }

    /// 读取最近一次 clear 隐私水位（RFC3339）；从未 clear 则 None。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     reconcile 与 finalize 需排除 clear 之前结束的 session，防止历史复活。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT cleared_before FROM agent_ledger_clear_watermark WHERE id=1。
    pub async fn get_clear_watermark(&self) -> Result<Option<String>, AppError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT cleared_before FROM agent_ledger_clear_watermark WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.filter(|s| !s.trim().is_empty()))
    }

    /// 判断 ended_at 是否允许写入 ledger（未 clear 或 ended 严格晚于水位）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     clear 后旧终态 session 不得被 record_terminal / reconcile 重新写入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     无水位 → true；ended_at 空白 → false；否则 ended_at > cleared_before（RFC3339 字典序）。
    pub async fn is_ended_after_clear_watermark(&self, ended_at: &str) -> Result<bool, AppError> {
        let Some(watermark) = self.get_clear_watermark().await? else {
            return Ok(true);
        };
        let ended = ended_at.trim();
        if ended.is_empty() {
            return Ok(false);
        }
        // RFC3339 在固定 Z/偏移格式下字典序与时间序一致（本库写入均为 to_rfc3339）。
        Ok(ended > watermark.as_str())
    }

    /// 幂等 finalize：首次 INSERT；冲突时 null-fill / 更晚 endedAt / 单调 counters。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     终态重放不得重复建行；只允许可靠 null-fill 与合法时间更正。
    ///
    /// Code Logic（这个函数做什么）:
    ///     shared lease 内 SELECT → INSERT 或 UPDATE；校验 identity/outcome/currency/rollback。
    pub async fn finalize(
        &self,
        input: AgentLedgerFinalizeInput,
    ) -> Result<AgentLedgerEntry, AppError> {
        let agent_session_id = input.agent_session_id.trim().to_string();
        if agent_session_id.is_empty() {
            return Err(AppError::validation("agent_session_id 不能为空"));
        }
        let project_id = input.project_id.trim().to_string();
        if project_id.is_empty() {
            return Err(AppError::validation("project_id 不能为空"));
        }
        let provider_id = input.provider_id.trim().to_string();
        if provider_id.is_empty() {
            return Err(AppError::validation("provider_id 不能为空"));
        }
        let started_at = input.started_at.trim().to_string();
        let ended_at = input.ended_at.trim().to_string();
        let duration_ms = compute_duration_ms(&started_at, &ended_at)?;
        let usage = input.usage.unwrap_or_default();
        let model_from_usage = usage
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let model_id = input
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or(model_from_usage);

        let (cost_minor, cost_currency) =
            match (usage.cost_major.as_deref(), usage.cost_currency.as_deref()) {
                (Some(major), Some(cur)) => match convert_major_to_minor_units(major, cur) {
                    Ok(m) => (Some(m), Some(validate_currency_code(cur)?)),
                    Err(_) => (None, None),
                },
                _ => (None, None),
            };

        with_shared_write_lease(&self.gate, async {
            if let Some(existing) = self.get_by_agent_session_id(&agent_session_id).await? {
                return self
                    .apply_null_fill(
                        existing,
                        &AgentLedgerFinalizeInput {
                            agent_session_id: agent_session_id.clone(),
                            project_id: project_id.clone(),
                            worktree_id: input.worktree_id.clone(),
                            provider_id: provider_id.clone(),
                            model_id: model_id.clone(),
                            started_at: started_at.clone(),
                            ended_at: ended_at.clone(),
                            outcome: input.outcome,
                            usage: Some(usage),
                            terminal_title: input.terminal_title.clone(),
                        },
                        cost_minor,
                        cost_currency,
                    )
                    .await;
            }

            // 与 clear 同一 critical section 语义：INSERT 前再查水位，禁止复活已清除历史
            if !self.is_ended_after_clear_watermark(&ended_at).await? {
                return Err(AppError::conflict("agent_ledger_cleared_before_ended_at"));
            }

            let id = uuid::Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();
            let outcome = input.outcome.as_str();
            sqlx::query(
                "INSERT INTO agent_session_ledger \
                 (id, agent_session_id, project_id, worktree_id, provider_id, model_id, \
                  started_at, ended_at, duration_ms, outcome, input_tokens, output_tokens, \
                  cache_read_tokens, cache_write_tokens, cost_minor_units, cost_currency, \
                  terminal_title, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&agent_session_id)
            .bind(&project_id)
            .bind(&input.worktree_id)
            .bind(&provider_id)
            .bind(&model_id)
            .bind(&started_at)
            .bind(&ended_at)
            .bind(i64_from_u64(duration_ms)?)
            .bind(outcome)
            .bind(opt_i64(usage.input_tokens)?)
            .bind(opt_i64(usage.output_tokens)?)
            .bind(opt_i64(usage.cache_read_tokens)?)
            .bind(opt_i64(usage.cache_write_tokens)?)
            .bind(opt_i64(cost_minor)?)
            .bind(&cost_currency)
            .bind(&input.terminal_title)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                // 并发 INSERT 唯一冲突 → 回读并 null-fill
                if is_unique_violation(&e) {
                    AppError::conflict(format!(
                        "agent_session_id 并发 finalize: {agent_session_id}"
                    ))
                } else {
                    AppError::from(e)
                }
            })?;

            // 并发冲突时回读 merge
            if let Ok(Some(existing)) = self.get_by_agent_session_id(&agent_session_id).await {
                if existing.id != id {
                    return self
                        .apply_null_fill(
                            existing,
                            &AgentLedgerFinalizeInput {
                                agent_session_id,
                                project_id,
                                worktree_id: input.worktree_id,
                                provider_id,
                                model_id,
                                started_at,
                                ended_at,
                                outcome: input.outcome,
                                usage: Some(usage),
                                terminal_title: input.terminal_title,
                            },
                            cost_minor,
                            cost_currency,
                        )
                        .await;
                }
            }

            self.get_by_agent_session_id(&agent_session_id)
                .await?
                .ok_or_else(|| AppError::generic("finalize 后读取失败"))
        })
        .await
    }

    /// 对已有行做单调 null-fill / endedAt 更正。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同一 agent_session_id 重放只允许可靠补齐，禁止改 identity。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 project/provider/outcome；合并 usage；later ended_at 重算 duration；
    ///     terminal_title 已有值不覆盖、null 可补。
    async fn apply_null_fill(
        &self,
        existing: AgentLedgerEntry,
        input: &AgentLedgerFinalizeInput,
        cost_minor: Option<u64>,
        cost_currency: Option<String>,
    ) -> Result<AgentLedgerEntry, AppError> {
        if existing.project_id != input.project_id.trim() {
            return Err(AppError::conflict(format!(
                "project_id 冲突: {} vs {}",
                existing.project_id, input.project_id
            )));
        }
        if existing.provider_id != input.provider_id.trim() {
            return Err(AppError::conflict(format!(
                "provider_id 冲突: {} vs {}",
                existing.provider_id, input.provider_id
            )));
        }
        if existing.outcome != input.outcome {
            return Err(AppError::conflict(format!(
                "outcome 冲突: {} vs {}",
                existing.outcome.as_str(),
                input.outcome.as_str()
            )));
        }
        // worktree：已有非空且新值不同 → 冲突；空则可填
        let worktree_id = match (
            existing.worktree_id.as_deref(),
            input
                .worktree_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ) {
            (Some(a), Some(b)) if a != b => {
                return Err(AppError::conflict(format!("worktree_id 冲突: {a} vs {b}")));
            }
            (Some(a), _) => Some(a.to_string()),
            (None, Some(b)) => Some(b.to_string()),
            (None, None) => None,
        };

        // terminal_title：与 worktree 同语义——已有非空不覆盖（含冲突不报错，保留旧值），null 可补
        let terminal_title = match (
            existing.terminal_title.as_deref(),
            input
                .terminal_title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ) {
            (Some(a), _) => Some(a.to_string()),
            (None, Some(b)) => Some(b.to_string()),
            (None, None) => None,
        };

        let base_usage = ReliableUsageSnapshot {
            model_id: existing.model_id.clone(),
            input_tokens: existing.input_tokens,
            output_tokens: existing.output_tokens,
            cache_read_tokens: existing.cache_read_tokens,
            cache_write_tokens: existing.cache_write_tokens,
            cost_major: existing.cost_minor_units.map(|m| {
                // 仅用于 merge 比较；保留 major 字符串形态时用 minor 编码
                m.to_string()
            }),
            cost_currency: existing.cost_currency.clone(),
            ..Default::default()
        };

        let incoming = input.usage.clone().unwrap_or_default();
        // merge tokens via merge_usage_monotonic，但对 cost 使用 minor units 直接比较
        let merged = merge_usage_monotonic(
            &ReliableUsageSnapshot {
                model_id: base_usage.model_id.clone(),
                input_tokens: base_usage.input_tokens,
                output_tokens: base_usage.output_tokens,
                cache_read_tokens: base_usage.cache_read_tokens,
                cache_write_tokens: base_usage.cache_write_tokens,
                cost_major: None,
                cost_currency: base_usage.cost_currency.clone(),
                ..Default::default()
            },
            &ReliableUsageSnapshot {
                model_id: incoming.model_id.clone().or(input.model_id.clone()),
                input_tokens: incoming.input_tokens,
                output_tokens: incoming.output_tokens,
                cache_read_tokens: incoming.cache_read_tokens,
                cache_write_tokens: incoming.cache_write_tokens,
                cost_major: None,
                cost_currency: incoming.cost_currency.clone(),
                ..Default::default()
            },
        )?;

        // cost minor 单调
        let (new_cost_minor, new_cost_currency) = match (
            existing.cost_minor_units,
            existing.cost_currency.as_deref(),
            cost_minor,
            cost_currency.as_deref(),
        ) {
            (Some(_a), Some(ca), Some(_b), Some(cb)) if ca != cb => {
                return Err(AppError::conflict(format!(
                    "cost_currency 冲突: {ca} vs {cb}"
                )));
            }
            (Some(a), _cur, Some(b), _) if b < a => {
                return Err(AppError::validation(format!("usage cost 回退: {b} < {a}")));
            }
            (Some(_a), cur, Some(b), new_cur) if b >= existing.cost_minor_units.unwrap_or(0) => (
                Some(b),
                new_cur
                    .map(|s| s.to_string())
                    .or_else(|| cur.map(|s| s.to_string())),
            ),
            (Some(a), cur, None, _) => (Some(a), cur.map(|s| s.to_string())),
            (None, _, Some(b), cur) => (Some(b), cur.map(|s| s.to_string())),
            (None, cur, None, _) => (None, cur.map(|s| s.to_string())),
            (Some(a), cur, Some(_), _) => (Some(a), cur.map(|s| s.to_string())),
        };
        let _ = &merged; // tokens already in merged
        let model_id = merged.model_id.clone().or(existing.model_id.clone());

        // ended_at：取更晚者；相等保留
        let (ended_at, duration_ms) = if input.ended_at.trim() > existing.ended_at.as_str() {
            let d = compute_duration_ms(&existing.started_at, input.ended_at.trim())?;
            (input.ended_at.trim().to_string(), d)
        } else {
            (existing.ended_at.clone(), existing.duration_ms)
        };

        // started_at 不得改变 identity 时间语义：首次写入为准
        let _started_at = existing.started_at.clone();

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE agent_session_ledger SET \
             worktree_id = ?, model_id = ?, ended_at = ?, duration_ms = ?, \
             input_tokens = ?, output_tokens = ?, cache_read_tokens = ?, cache_write_tokens = ?, \
             cost_minor_units = ?, cost_currency = ?, terminal_title = ?, updated_at = ? \
             WHERE agent_session_id = ?",
        )
        .bind(&worktree_id)
        .bind(&model_id)
        .bind(&ended_at)
        .bind(i64_from_u64(duration_ms)?)
        .bind(opt_i64(merged.input_tokens)?)
        .bind(opt_i64(merged.output_tokens)?)
        .bind(opt_i64(merged.cache_read_tokens)?)
        .bind(opt_i64(merged.cache_write_tokens)?)
        .bind(opt_i64(new_cost_minor)?)
        .bind(&new_cost_currency)
        .bind(&terminal_title)
        .bind(&now)
        .bind(&existing.agent_session_id)
        .execute(&self.pool)
        .await?;

        self.get_by_agent_session_id(&existing.agent_session_id)
            .await?
            .ok_or_else(|| AppError::generic("null-fill 后读取失败"))
    }

    /// 按 agent_session_id 读取一行。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     finalize 幂等与 reconcile 判断缺失 entry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 映射 DTO。
    pub async fn get_by_agent_session_id(
        &self,
        agent_session_id: &str,
    ) -> Result<Option<AgentLedgerEntry>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {SELECT_COLUMNS} FROM agent_session_ledger WHERE agent_session_id = ?"
        ))
        .bind(agent_session_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(map_row(r)?)),
            None => Ok(None),
        }
    }

    /// 是否存在 agent_session_id。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     reconcile 快速判断缺失。
    ///
    /// Code Logic（这个函数做什么）:
    ///     EXISTS 查询。
    pub async fn exists_agent_session(&self, agent_session_id: &str) -> Result<bool, AppError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(1) FROM agent_session_ledger WHERE agent_session_id = ?",
        )
        .bind(agent_session_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n > 0)
    }

    /// 有界分页（ended_at DESC, id DESC）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机明细 drawer 需要稳定 keyset 翻页与封闭 filter。
    ///
    /// Code Logic（这个函数做什么）:
    ///     规范化 limit；解码 cursor；动态 WHERE；limit+1 探 has_more。
    pub async fn get_page(&self, query: AgentLedgerQuery) -> Result<AgentLedgerPage, AppError> {
        let limit = query
            .limit
            .unwrap_or(DEFAULT_PAGE_LIMIT)
            .clamp(1, MAX_PAGE_LIMIT);
        let fetch = limit as i64 + 1;

        let mut sql = format!("SELECT {SELECT_COLUMNS} FROM agent_session_ledger WHERE 1=1");
        let mut binds: Vec<BindValue> = Vec::new();

        if let Some(pid) = query
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND project_id = ?");
            binds.push(BindValue::Text(pid.to_string()));
        }
        if let Some(prov) = query
            .provider_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND provider_id = ?");
            binds.push(BindValue::Text(prov.to_string()));
        }
        if let Some(outcome) = query.outcome {
            sql.push_str(" AND outcome = ?");
            binds.push(BindValue::Text(outcome.as_str().to_string()));
        }
        if let Some(after) = query
            .ended_after
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND ended_at >= ?");
            binds.push(BindValue::Text(after.to_string()));
        }
        if let Some(before) = query
            .ended_before
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            sql.push_str(" AND ended_at <= ?");
            binds.push(BindValue::Text(before.to_string()));
        }
        if let Some(cursor_raw) = query
            .cursor
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let cur = decode_ledger_cursor(cursor_raw)
                .map_err(|_| AppError::validation("invalid ledger cursor"))?;
            sql.push_str(" AND (ended_at < ? OR (ended_at = ? AND id < ?))");
            binds.push(BindValue::Text(cur.ended_at.clone()));
            binds.push(BindValue::Text(cur.ended_at));
            binds.push(BindValue::Text(cur.id));
        }
        sql.push_str(" ORDER BY ended_at DESC, id DESC LIMIT ?");
        binds.push(BindValue::Int(fetch));

        let mut q = sqlx::query(&sql);
        for b in &binds {
            q = match b {
                BindValue::Text(s) => q.bind(s),
                BindValue::Int(i) => q.bind(*i),
            };
        }
        let rows = q.fetch_all(&self.pool).await?;
        let has_more = rows.len() as u32 > limit;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for row in rows.into_iter().take(limit as usize) {
            items.push(map_row(row)?);
        }
        let next_cursor = if has_more {
            items
                .last()
                .map(|e| encode_ledger_cursor(&e.ended_at, &e.id))
        } else {
            None
        };
        Ok(AgentLedgerPage { items, next_cursor })
    }

    /// 清除全部 ledger 行并写入隐私水位；返回删除数。幂等。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     设置页一键清除；不影响 runtime/task/terminal；必须持久化水位，
    ///     否则启动 reconcile 会把清除前终态 session 重新写回。
    ///     watermark + DELETE 必须同一事务，禁止半清除窗口被 finalize 插入复活。
    ///
    /// Code Logic（这个函数做什么）:
    ///     begin_shared_write 事务：UPSERT cleared_before=now + DELETE ledger 全表；commit。
    pub async fn clear_all(&self) -> Result<u64, AppError> {
        let (permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO agent_ledger_clear_watermark (id, cleared_before) VALUES (1, ?) \
             ON CONFLICT(id) DO UPDATE SET cleared_before = excluded.cleared_before",
        )
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        let res = sqlx::query("DELETE FROM agent_session_ledger")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        drop(permit);
        Ok(res.rows_affected())
    }

    /// 统计总行数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     retention cap 与测试断言。
    ///
    /// Code Logic（这个函数做什么）:
    ///     COUNT(*)。
    pub async fn count_all(&self) -> Result<u64, AppError> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(1) FROM agent_session_ledger")
            .fetch_one(&self.pool)
            .await?;
        Ok(n as u64)
    }

    /// 有界清理：先删超龄，再删超 cap 最旧；单批 ≤500。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     自动保留 30 天与 10k 上限，避免用户维护。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 now 上计算 cutoff；age DELETE LIMIT；剩余预算做 cap DELETE。
    pub async fn cleanup_batch(
        &self,
        now: DateTime<Utc>,
        max_age_days: i64,
        max_rows: u64,
        batch_limit: u64,
    ) -> Result<AgentLedgerCleanupResult, AppError> {
        let batch_limit = batch_limit.clamp(1, CLEANUP_BATCH_LIMIT);
        let cutoff = (now - ChronoDuration::days(max_age_days)).to_rfc3339();

        with_shared_write_lease(&self.gate, async {
            let mut deleted: u64 = 0;
            // age-first
            let age_res = sqlx::query(
                "DELETE FROM agent_session_ledger WHERE id IN ( \
                   SELECT id FROM agent_session_ledger \
                   WHERE ended_at < ? \
                   ORDER BY ended_at ASC, id ASC \
                   LIMIT ? \
                 )",
            )
            .bind(&cutoff)
            .bind(batch_limit as i64)
            .execute(&self.pool)
            .await?;
            deleted += age_res.rows_affected();
            let mut budget = batch_limit.saturating_sub(deleted);

            if budget > 0 {
                let total = self.count_all().await?;
                if total > max_rows {
                    let overflow = total - max_rows;
                    let take = overflow.min(budget);
                    let cap_res = sqlx::query(
                        "DELETE FROM agent_session_ledger WHERE id IN ( \
                           SELECT id FROM agent_session_ledger \
                           ORDER BY ended_at ASC, id ASC \
                           LIMIT ? \
                         )",
                    )
                    .bind(take as i64)
                    .execute(&self.pool)
                    .await?;
                    deleted += cap_res.rows_affected();
                    budget = budget.saturating_sub(cap_res.rows_affected());
                }
            }
            let _ = budget;

            // more_remaining：仍有超龄或超 cap
            let still_old: i64 =
                sqlx::query_scalar("SELECT COUNT(1) FROM agent_session_ledger WHERE ended_at < ?")
                    .bind(&cutoff)
                    .fetch_one(&self.pool)
                    .await?;
            let total_after = self.count_all().await?;
            let more_remaining = still_old > 0 || total_after > max_rows;

            Ok(AgentLedgerCleanupResult {
                deleted,
                more_remaining,
            })
        })
        .await
    }

    /// 时间窗聚合。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 summary 与后续 P2P aggregate 共用同一覆盖度语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     ended_at >= now-window；token 只 sum 非 null；coverage=complete/partial/unavailable。
    pub async fn summarize(
        &self,
        window: LedgerWindow,
        project_id: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<AgentLedgerSummary, AppError> {
        let start = now - ChronoDuration::seconds(window.duration_secs() as i64);
        let start_s = start.to_rfc3339();
        let now_s = now.to_rfc3339();

        let mut sql = format!(
            "SELECT {SELECT_COLUMNS} FROM agent_session_ledger \
             WHERE ended_at >= ? AND ended_at <= ?"
        );
        if project_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some()
        {
            sql.push_str(" AND project_id = ?");
        }
        let mut q = sqlx::query(&sql).bind(&start_s).bind(&now_s);
        if let Some(pid) = project_id.map(str::trim).filter(|s| !s.is_empty()) {
            q = q.bind(pid);
        }
        let rows = q.fetch_all(&self.pool).await?;
        let mut sessions = 0u64;
        let mut completed = 0u64;
        let mut failed = 0u64;
        let mut cancelled = 0u64;
        let mut disconnected = 0u64;
        let mut duration_ms = 0u64;
        let mut input_sum: Option<u64> = None;
        let mut output_sum: Option<u64> = None;
        let mut cache_read_sum: Option<u64> = None;
        let mut cache_write_sum: Option<u64> = None;
        let mut usage_contributors = 0u64;
        let mut cost_map: BTreeMap<String, u64> = BTreeMap::new();

        for row in rows {
            let entry = map_row(row)?;
            sessions += 1;
            duration_ms = duration_ms.saturating_add(entry.duration_ms);
            match entry.outcome {
                AgentLedgerOutcome::Completed => completed += 1,
                AgentLedgerOutcome::Failed => failed += 1,
                AgentLedgerOutcome::Cancelled => cancelled += 1,
                AgentLedgerOutcome::Disconnected => disconnected += 1,
            }
            let has_usage = entry.input_tokens.is_some()
                || entry.output_tokens.is_some()
                || entry.cache_read_tokens.is_some()
                || entry.cache_write_tokens.is_some()
                || entry.cost_minor_units.is_some();
            if has_usage {
                usage_contributors += 1;
            }
            if let Some(t) = entry.input_tokens {
                input_sum = Some(input_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.output_tokens {
                output_sum = Some(output_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.cache_read_tokens {
                cache_read_sum = Some(cache_read_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.cache_write_tokens {
                cache_write_sum = Some(cache_write_sum.unwrap_or(0).saturating_add(t));
            }
            if let (Some(m), Some(c)) = (entry.cost_minor_units, entry.cost_currency.as_ref()) {
                let e = cost_map.entry(c.clone()).or_insert(0);
                *e = e.saturating_add(m);
            }
        }

        let usage_coverage = if sessions == 0 || usage_contributors == 0 {
            LedgerUsageCoverage::Unavailable
        } else if usage_contributors == sessions {
            LedgerUsageCoverage::Complete
        } else {
            LedgerUsageCoverage::Partial
        };

        let cost_by_currency: Vec<CurrencyAmount> = cost_map
            .into_iter()
            .map(|(currency, minor_units)| CurrencyAmount {
                currency,
                minor_units,
            })
            .collect();

        let real_consumed_tokens = match (input_sum, output_sum, cache_write_sum) {
            (Some(i), Some(o), Some(cw)) => Some(i.saturating_add(o).saturating_add(cw)),
            _ => None,
        };
        let cache_hit_rate = match (cache_read_sum, input_sum) {
            (Some(cr), Some(i)) => {
                let denom = cr.saturating_add(i);
                if denom == 0 {
                    None
                } else {
                    Some((cr as f64 / denom as f64) as f32)
                }
            }
            _ => None,
        };

        Ok(AgentLedgerSummary {
            window,
            project_id: project_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            sessions,
            completed,
            failed,
            cancelled,
            disconnected,
            duration_ms,
            input_tokens: input_sum,
            output_tokens: output_sum,
            cache_read_tokens: cache_read_sum,
            cache_write_tokens: cache_write_sum,
            real_consumed_tokens,
            cache_hit_rate,
            requests_count: sessions,
            total_cost_by_currency: cost_by_currency.clone(),
            cost_by_currency,
            by_model: Vec::new(),
            by_provider: Vec::new(),
            by_project: Vec::new(),
            trend: Vec::new(),
            bucket: bucket_for_window(window),
            usage_coverage,
        })
    }

    /// 全量筛选聚合（Token 统计页 + export 共用）。
    ///
    /// Business Logic:
    ///     既有 `summarize(window, project_id, now)` 仅覆盖单 project + 24h/7d/30d；
    ///     统计页需要 provider/model/project/worktree 多值过滤 + 自定义时间窗 + cache_write。
    ///
    /// Code Logic:
    ///     WHERE 共享 `build_where`；token 累加语义与 `summarize` 完全一致；
    ///     real_consumed / cache_hit_rate 同 `summarize` 派生；bucket 按 filters 推导。
    pub async fn summarize_with_filters(
        &self,
        filters: AgentLedgerFilters,
        now: DateTime<Utc>,
    ) -> Result<AgentLedgerSummary, AppError> {
        let (where_sql, binds) = build_where(&filters, &[], now);
        let sql = format!("SELECT {SELECT_COLUMNS} FROM agent_session_ledger WHERE {where_sql}");
        let mut q = sqlx::query(&sql);
        for b in &binds {
            q = match b {
                BindValue::Text(s) => q.bind(s),
                BindValue::Int(i) => q.bind(*i),
            };
        }
        let rows = q.fetch_all(&self.pool).await?;
        let mut sessions = 0u64;
        let mut completed = 0u64;
        let mut failed = 0u64;
        let mut cancelled = 0u64;
        let mut disconnected = 0u64;
        let mut duration_ms = 0u64;
        let mut input_sum: Option<u64> = None;
        let mut output_sum: Option<u64> = None;
        let mut cache_read_sum: Option<u64> = None;
        let mut cache_write_sum: Option<u64> = None;
        let mut usage_contributors = 0u64;
        let mut cost_map: BTreeMap<String, u64> = BTreeMap::new();

        for row in rows {
            let entry = map_row(row)?;
            sessions += 1;
            duration_ms = duration_ms.saturating_add(entry.duration_ms);
            match entry.outcome {
                AgentLedgerOutcome::Completed => completed += 1,
                AgentLedgerOutcome::Failed => failed += 1,
                AgentLedgerOutcome::Cancelled => cancelled += 1,
                AgentLedgerOutcome::Disconnected => disconnected += 1,
            }
            let has_usage = entry.input_tokens.is_some()
                || entry.output_tokens.is_some()
                || entry.cache_read_tokens.is_some()
                || entry.cache_write_tokens.is_some()
                || entry.cost_minor_units.is_some();
            if has_usage {
                usage_contributors += 1;
            }
            if let Some(t) = entry.input_tokens {
                input_sum = Some(input_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.output_tokens {
                output_sum = Some(output_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.cache_read_tokens {
                cache_read_sum = Some(cache_read_sum.unwrap_or(0).saturating_add(t));
            }
            if let Some(t) = entry.cache_write_tokens {
                cache_write_sum = Some(cache_write_sum.unwrap_or(0).saturating_add(t));
            }
            if let (Some(m), Some(c)) = (entry.cost_minor_units, entry.cost_currency.as_ref()) {
                let e = cost_map.entry(c.clone()).or_insert(0);
                *e = e.saturating_add(m);
            }
        }

        let usage_coverage = if sessions == 0 || usage_contributors == 0 {
            LedgerUsageCoverage::Unavailable
        } else if usage_contributors == sessions {
            LedgerUsageCoverage::Complete
        } else {
            LedgerUsageCoverage::Partial
        };

        let cost_by_currency: Vec<CurrencyAmount> = cost_map
            .into_iter()
            .map(|(currency, minor_units)| CurrencyAmount {
                currency,
                minor_units,
            })
            .collect();

        let real_consumed_tokens = match (input_sum, output_sum, cache_write_sum) {
            (Some(i), Some(o), Some(cw)) => Some(i.saturating_add(o).saturating_add(cw)),
            _ => None,
        };
        let cache_hit_rate = match (cache_read_sum, input_sum) {
            (Some(cr), Some(i)) => {
                let denom = cr.saturating_add(i);
                if denom == 0 {
                    None
                } else {
                    Some((cr as f64 / denom as f64) as f32)
                }
            }
            _ => None,
        };

        let project_id = filters
            .project_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let window = filters.window.unwrap_or(LedgerWindow::Days7);

        Ok(AgentLedgerSummary {
            window,
            project_id,
            sessions,
            completed,
            failed,
            cancelled,
            disconnected,
            duration_ms,
            input_tokens: input_sum,
            output_tokens: output_sum,
            cache_read_tokens: cache_read_sum,
            cache_write_tokens: cache_write_sum,
            real_consumed_tokens,
            cache_hit_rate,
            requests_count: sessions,
            total_cost_by_currency: cost_by_currency.clone(),
            cost_by_currency,
            by_model: Vec::new(),
            by_provider: Vec::new(),
            by_project: Vec::new(),
            trend: Vec::new(),
            bucket: derive_bucket(&filters),
            usage_coverage,
        })
    }

    /// 三维拆分聚合（by_model / by_provider / by_project）。
    ///
    /// Business Logic:
    ///     统计页需要按维度拆分；同一 SQL 与 WHERE 共享，与 summary 一致语义。
    ///
    /// Code Logic:
    ///     dimension ∈ {model, provider, project}；NULL 值渲染为 "(unknown)"；
    ///     cost 展开为多行 GROUP BY (dim_key, currency)；
    ///     排序：sessions DESC，input_tokens DESC NULLS LAST（SQLite 用 CASE WHEN）, key ASC。
    pub async fn summarize_grouped(
        &self,
        filters: &AgentLedgerFilters,
        dimension: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentLedgerGroupRow>, AppError> {
        let (dim_select, dim_group) = match dimension {
            "model" => ("model_id", "model_id"),
            "provider" => ("provider_id", "provider_id"),
            "project" => ("project_id", "project_id"),
            other => {
                return Err(AppError::validation(format!(
                    "summarize_grouped 非法 dimension: {other}"
                )));
            }
        };

        let (where_sql, binds) = build_where(filters, &[], now);
        let sql = format!(
            "SELECT {dim_select} AS dim_key, \
             COUNT(1) AS sessions, \
             SUM(CASE WHEN outcome='completed' THEN 1 ELSE 0 END) AS completed, \
             SUM(CASE WHEN outcome='failed' THEN 1 ELSE 0 END) AS failed, \
             SUM(CASE WHEN outcome='cancelled' THEN 1 ELSE 0 END) AS cancelled, \
             SUM(CASE WHEN outcome='disconnected' THEN 1 ELSE 0 END) AS disconnected, \
             SUM(input_tokens) AS input_sum, \
             SUM(output_tokens) AS output_sum, \
             SUM(cache_read_tokens) AS cache_read_sum, \
             SUM(cache_write_tokens) AS cache_write_sum, \
             cost_minor_units, cost_currency \
             FROM agent_session_ledger \
             WHERE {where_sql} \
             GROUP BY {dim_group}, cost_currency \
             ORDER BY sessions DESC, input_sum DESC"
        );
        let mut q = sqlx::query(&sql);
        for b in &binds {
            q = match b {
                BindValue::Text(s) => q.bind(s),
                BindValue::Int(i) => q.bind(*i),
            };
        }
        let rows = q.fetch_all(&self.pool).await?;

        // 聚合 cost 桶到 dim_key
        let mut by_key: BTreeMap<String, AgentLedgerGroupRow> = BTreeMap::new();
        let mut order: Vec<(String, u64, Option<i64>)> = Vec::new();
        for row in rows {
            let raw_key: Option<String> = row.try_get("dim_key").ok();
            let key = raw_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(unknown)".to_string());
            let sessions: i64 = row.try_get("sessions").unwrap_or(0);
            let completed: i64 = row.try_get("completed").unwrap_or(0);
            let failed: i64 = row.try_get("failed").unwrap_or(0);
            let cancelled: i64 = row.try_get("cancelled").unwrap_or(0);
            let disconnected: i64 = row.try_get("disconnected").unwrap_or(0);
            let input_sum: Option<i64> = row.try_get("input_sum").ok();
            let output_sum: Option<i64> = row.try_get("output_sum").ok();
            let cache_read_sum: Option<i64> = row.try_get("cache_read_sum").ok();
            let cache_write_sum: Option<i64> = row.try_get("cache_write_sum").ok();
            let cost_minor: Option<i64> = row.try_get("cost_minor_units").ok();
            let cost_currency: Option<String> = row.try_get("cost_currency").ok();

            let entry = by_key
                .entry(key.clone())
                .or_insert_with(|| AgentLedgerGroupRow {
                    label: None,
                    sessions: 0,
                    completed: 0,
                    failed: 0,
                    cancelled: 0,
                    disconnected: 0,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost_by_currency: Vec::new(),
                    key: key.clone(),
                });
            entry.sessions = entry.sessions.saturating_add(sessions.max(0) as u64);
            entry.completed = entry.completed.saturating_add(completed.max(0) as u64);
            entry.failed = entry.failed.saturating_add(failed.max(0) as u64);
            entry.cancelled = entry.cancelled.saturating_add(cancelled.max(0) as u64);
            entry.disconnected = entry
                .disconnected
                .saturating_add(disconnected.max(0) as u64);
            merge_optional_sum(&mut entry.input_tokens, input_sum);
            merge_optional_sum(&mut entry.output_tokens, output_sum);
            merge_optional_sum(&mut entry.cache_read_tokens, cache_read_sum);
            merge_optional_sum(&mut entry.cache_write_tokens, cache_write_sum);
            if let (Some(m), Some(c)) = (cost_minor, cost_currency.as_deref()) {
                if m > 0 && !c.trim().is_empty() {
                    if let Some(slot) = entry.cost_by_currency.iter_mut().find(|x| x.currency == c)
                    {
                        slot.minor_units = slot.minor_units.saturating_add(m as u64);
                    } else {
                        entry.cost_by_currency.push(CurrencyAmount {
                            currency: c.to_string(),
                            minor_units: m.max(0) as u64,
                        });
                    }
                }
            }
            order.push((key.clone(), sessions.max(0) as u64, input_sum));
        }

        // 排序：sessions DESC → input_tokens DESC（NULL 排最后）→ key ASC 兜底
        order.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| match (a.2, b.2) {
                    (Some(x), Some(y)) => y.cmp(&x),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut out = Vec::with_capacity(order.len());
        for (k, _, _) in order {
            if let Some(row) = by_key.remove(&k) {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// 趋势桶聚合。
    ///
    /// Business Logic:
    ///     统计页趋势图 x 轴 = bucket_start；按桶粒度（hour|day）聚合。
    ///
    /// Code Logic:
    ///     `strftime('%Y-%m-%dT%H:00:00Z', ended_at)` 或 `'%Y-%m-%dT00:00:00Z'`；
    ///     与 grouped 同语义聚合 cost 桶；按 bucket_start ASC 排序。
    pub async fn summarize_trend(
        &self,
        filters: &AgentLedgerFilters,
        bucket: TrendBucket,
        now: DateTime<Utc>,
    ) -> Result<Vec<AgentLedgerTrendPoint>, AppError> {
        let (where_sql, binds) = build_where(filters, &[], now);
        let bucket_expr = match bucket {
            TrendBucket::Hour => "strftime('%Y-%m-%dT%H:00:00Z', ended_at)",
            TrendBucket::Day => "strftime('%Y-%m-%dT00:00:00Z', ended_at)",
        };
        let sql = format!(
            "SELECT {bucket_expr} AS bucket_start, \
             SUM(input_tokens) AS input_sum, \
             SUM(output_tokens) AS output_sum, \
             SUM(cache_read_tokens) AS cache_read_sum, \
             SUM(cache_write_tokens) AS cache_write_sum, \
             cost_minor_units, cost_currency \
             FROM agent_session_ledger \
             WHERE {where_sql} \
             GROUP BY bucket_start, cost_currency \
             ORDER BY bucket_start ASC"
        );
        let mut q = sqlx::query(&sql);
        for b in &binds {
            q = match b {
                BindValue::Text(s) => q.bind(s),
                BindValue::Int(i) => q.bind(*i),
            };
        }
        let rows = q.fetch_all(&self.pool).await?;

        let mut by_bucket: BTreeMap<String, AgentLedgerTrendPoint> = BTreeMap::new();
        for row in rows {
            let bucket_start: String = row.try_get("bucket_start").unwrap_or_default();
            let input_sum: Option<i64> = row.try_get("input_sum").ok();
            let output_sum: Option<i64> = row.try_get("output_sum").ok();
            let cache_read_sum: Option<i64> = row.try_get("cache_read_sum").ok();
            let cache_write_sum: Option<i64> = row.try_get("cache_write_sum").ok();
            let cost_minor: Option<i64> = row.try_get("cost_minor_units").ok();
            let cost_currency: Option<String> = row.try_get("cost_currency").ok();

            let entry =
                by_bucket
                    .entry(bucket_start.clone())
                    .or_insert_with(|| AgentLedgerTrendPoint {
                        bucket_start: bucket_start.clone(),
                        input_tokens: None,
                        output_tokens: None,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        cost_by_currency: Vec::new(),
                    });
            merge_optional_sum(&mut entry.input_tokens, input_sum);
            merge_optional_sum(&mut entry.output_tokens, output_sum);
            merge_optional_sum(&mut entry.cache_read_tokens, cache_read_sum);
            merge_optional_sum(&mut entry.cache_write_tokens, cache_write_sum);
            if let (Some(m), Some(c)) = (cost_minor, cost_currency.as_deref()) {
                if m > 0 && !c.trim().is_empty() {
                    if let Some(slot) = entry.cost_by_currency.iter_mut().find(|x| x.currency == c)
                    {
                        slot.minor_units = slot.minor_units.saturating_add(m as u64);
                    } else {
                        entry.cost_by_currency.push(CurrencyAmount {
                            currency: c.to_string(),
                            minor_units: m.max(0) as u64,
                        });
                    }
                }
            }
        }

        Ok(by_bucket.into_values().collect())
    }
}

/// 动态绑定值。
enum BindValue {
    Text(String),
    Int(i64),
}

/// 由 window 推导桶粒度（24h→Hour，其它→Day）。
///
/// Business Logic:
///     统计页 trends 默认按 window 推桶；24h 太长用 day 失去细节。
///
/// Code Logic:
///     match window。
fn bucket_for_window(window: LedgerWindow) -> TrendBucket {
    match window {
        LedgerWindow::Hours24 => TrendBucket::Hour,
        _ => TrendBucket::Day,
    }
}

/// 由 filters 推导桶粒度；显式 bucket 优先，否则按 window 推。
///
/// Business Logic:
///     export 与统计页都通过 filters 推导同一桶粒度；保持口径一致。
///
/// Code Logic:
///     filters.bucket → filters.window → Hours24 走 Hour，其它 Day。
fn derive_bucket(filters: &AgentLedgerFilters) -> TrendBucket {
    if let Some(b) = filters.bucket {
        return b;
    }
    match filters.window {
        Some(LedgerWindow::Hours24) => TrendBucket::Hour,
        _ => TrendBucket::Day,
    }
}

/// 构建共享 WHERE 子句与绑定参数。
///
/// Business Logic:
///     summarize_with_filters / summarize_grouped / summarize_trend 共用同一筛选项；
///     任何 IN 子句必须用占位符以防注入。
///
/// Code Logic:
///     1) 预设 window→ended_at；若 started_after/before 非空则改用 ended_at 自定义区间
///        （不再 AND 默认 7d，避免自定义被裁剪）；2) project_id；
///     3) worktree_id；4) outcome；5) provider_ids / model_ids / project_ids IN(?,?,…)；
///     6) clear watermark；末尾追加 extra_clauses。
fn build_where(
    filters: &AgentLedgerFilters,
    extra_clauses: &[&str],
    now: DateTime<Utc>,
) -> (String, Vec<BindValue>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<BindValue> = Vec::new();

    let custom_after = filters
        .started_after
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let custom_before = filters
        .started_before
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let has_custom_range = custom_after.is_some() || custom_before.is_some();

    // 1) 预设窗用 ended_at；自定义区间同样落 ended_at，与 list/export 对齐。
    if has_custom_range {
        if let Some(after) = custom_after {
            clauses.push("ended_at >= ?".into());
            binds.push(BindValue::Text(after.to_string()));
        }
        if let Some(before) = custom_before {
            clauses.push("ended_at <= ?".into());
            binds.push(BindValue::Text(before.to_string()));
        }
    } else {
        let window = filters.window.unwrap_or(LedgerWindow::Days7);
        let start = now - ChronoDuration::seconds(window.duration_secs() as i64);
        clauses.push("ended_at >= ?".into());
        binds.push(BindValue::Text(start.to_rfc3339()));
        clauses.push("ended_at <= ?".into());
        binds.push(BindValue::Text(now.to_rfc3339()));
    }

    // 3) project_id（与 project_ids 互斥，多值优先）
    if let Some(pids) = filters.project_ids.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = std::iter::repeat("?")
            .take(pids.len())
            .collect::<Vec<_>>()
            .join(",");
        clauses.push(format!("project_id IN ({placeholders})"));
        for pid in pids {
            binds.push(BindValue::Text(pid.trim().to_string()));
        }
    } else if let Some(pid) = filters
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push("project_id = ?".into());
        binds.push(BindValue::Text(pid.to_string()));
    }

    // 4) worktree_id
    if let Some(wid) = filters
        .worktree_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push("worktree_id = ?".into());
        binds.push(BindValue::Text(wid.to_string()));
    }

    // 5) outcome
    if let Some(o) = filters.outcome {
        clauses.push("outcome = ?".into());
        binds.push(BindValue::Text(o.as_str().to_string()));
    }

    // 6) provider_ids IN
    if let Some(p) = filters.provider_ids.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = std::iter::repeat("?")
            .take(p.len())
            .collect::<Vec<_>>()
            .join(",");
        clauses.push(format!("provider_id IN ({placeholders})"));
        for pid in p {
            binds.push(BindValue::Text(pid.trim().to_string()));
        }
    }
    // model_ids IN
    if let Some(m) = filters.model_ids.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = std::iter::repeat("?")
            .take(m.len())
            .collect::<Vec<_>>()
            .join(",");
        clauses.push(format!("model_id IN ({placeholders})"));
        for mid in m {
            binds.push(BindValue::Text(mid.trim().to_string()));
        }
    }

    // 7) clear watermark：SQL 端过滤（避免历史复活）
    clauses.push(
        "ended_at > COALESCE((SELECT cleared_before FROM agent_ledger_clear_watermark WHERE id=1), '')"
            .into(),
    );

    for c in extra_clauses {
        clauses.push((*c).to_string());
    }

    let sql = clauses.join(" AND ");
    (sql, binds)
}

/// 合并 Option<u64> += Option<i64>（None 跳过；双 None → None）。
///
/// Business Logic:
///     SQL SUM 对 NULL 行返回 NULL；分项累加保持 unknown 不转 0 语义。
///
/// Code Logic:
///     Some 优先 unwrap_or(0) → saturating_add；都 None → None。
fn merge_optional_sum(target: &mut Option<u64>, incoming: Option<i64>) {
    if let Some(v) = incoming {
        *target = Some(target.unwrap_or(0).saturating_add(v.max(0) as u64));
    }
}

/// 编码 opaque ledger cursor。
///
/// Business Logic（为什么需要这个函数）:
///     客户端不得解析/发明 cursor。
///
/// Code Logic（这个函数做什么）:
///     base64url({v:1,ended_at,id})。
pub fn encode_ledger_cursor(ended_at: &str, id: &str) -> String {
    let payload = LedgerCursorV1 {
        v: 1,
        ended_at: ended_at.to_string(),
        id: id.to_string(),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

/// 解码 opaque ledger cursor。
///
/// Business Logic（为什么需要这个函数）:
///     非法 cursor 在访问 DB 前拒绝。
///
/// Code Logic（这个函数做什么）:
///     base64url 解码 → v==1。
fn decode_ledger_cursor(cursor: &str) -> Result<LedgerCursorV1, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor.as_bytes()).map_err(|_| ())?;
    let payload: LedgerCursorV1 = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if payload.v != 1 || payload.ended_at.is_empty() || payload.id.is_empty() {
        return Err(());
    }
    Ok(payload)
}

/// 映射 SQLite 行。
///
/// Business Logic（为什么需要这个函数）:
///     统一列 → DTO，避免各查询重复。
///
/// Code Logic（这个函数做什么）:
///     try_get 各列；outcome parse fail → Err。
fn map_row(row: SqliteRow) -> Result<AgentLedgerEntry, AppError> {
    let outcome_raw: String = row.try_get("outcome")?;
    let outcome = AgentLedgerOutcome::parse(&outcome_raw)
        .ok_or_else(|| AppError::generic(format!("ledger outcome 损坏: {outcome_raw}")))?;
    Ok(AgentLedgerEntry {
        id: row.try_get("id")?,
        agent_session_id: row.try_get("agent_session_id")?,
        project_id: row.try_get("project_id")?,
        worktree_id: row.try_get("worktree_id")?,
        provider_id: row.try_get("provider_id")?,
        model_id: row.try_get("model_id")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
        duration_ms: row.try_get::<i64, _>("duration_ms")? as u64,
        outcome,
        input_tokens: row
            .try_get::<Option<i64>, _>("input_tokens")?
            .map(|v| v as u64),
        output_tokens: row
            .try_get::<Option<i64>, _>("output_tokens")?
            .map(|v| v as u64),
        cache_read_tokens: row
            .try_get::<Option<i64>, _>("cache_read_tokens")?
            .map(|v| v as u64),
        cache_write_tokens: row
            .try_get::<Option<i64>, _>("cache_write_tokens")?
            .map(|v| v as u64),
        cost_minor_units: row
            .try_get::<Option<i64>, _>("cost_minor_units")?
            .map(|v| v as u64),
        cost_currency: row.try_get("cost_currency")?,
        terminal_title: row.try_get("terminal_title")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// u64 → i64 检查。
fn i64_from_u64(v: u64) -> Result<i64, AppError> {
    i64::try_from(v).map_err(|_| AppError::validation(format!("整数值溢出 i64: {v}")))
}

/// Option<u64> → Option<i64>。
fn opt_i64(v: Option<u64>) -> Result<Option<i64>, AppError> {
    match v {
        Some(x) => Ok(Some(i64_from_u64(x)?)),
        None => Ok(None),
    }
}

/// 是否 UNIQUE 约束冲突。
fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db) => {
            let msg = db.message().to_ascii_lowercase();
            msg.contains("unique") || db.code().as_deref() == Some("2067")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::agent_ledger::models::{
        scan_forbidden_ledger_field_names, AgentLedgerFinalizeInput, AgentLedgerOutcome,
        ReliableUsageSnapshot,
    };
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    /// 内存库 + schema。
    async fn ledger_repo() -> AgentLedgerRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        AgentLedgerRepo::new(pool)
    }

    fn usage(input: u64, output: u64, currency: &str, major: &str) -> ReliableUsageSnapshot {
        ReliableUsageSnapshot {
            model_id: Some("m1".into()),
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_major: Some(major.into()),
            cost_currency: Some(currency.into()),
            ..Default::default()
        }
    }

    fn finalize(id: &str, u: Option<ReliableUsageSnapshot>) -> AgentLedgerFinalizeInput {
        AgentLedgerFinalizeInput {
            agent_session_id: id.into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-07-01T10:00:00Z".into(),
            ended_at: "2026-07-01T10:05:00Z".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: u,
            terminal_title: None,
        }
    }

    fn default_query() -> AgentLedgerQuery {
        AgentLedgerQuery {
            limit: Some(50),
            ..Default::default()
        }
    }

    /// Business Logic: 终态重放补可靠 usage 且不重复行。
    #[tokio::test]
    async fn terminal_replay_fills_reliable_usage_without_duplicate_entry() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", None)).await.unwrap();
        repo.finalize(finalize("a1", Some(usage(120, 40, "USD", "0.03"))))
            .await
            .unwrap();
        let rows = repo.get_page(default_query()).await.unwrap().items;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, Some(120));
        assert_eq!(rows[0].cost_currency.as_deref(), Some("USD"));
        assert_eq!(rows[0].cost_minor_units, Some(3));
    }

    /// Business Logic: agent_session_id 唯一。
    #[tokio::test]
    async fn agent_session_id_is_unique() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", None)).await.unwrap();
        assert!(repo.exists_agent_session("a1").await.unwrap());
        let page = repo.get_page(default_query()).await.unwrap();
        assert_eq!(page.items.len(), 1);
    }

    /// Business Logic: 旧库 ensure_schema 幂等。
    #[tokio::test]
    async fn ensure_schema_is_idempotent_on_existing_database() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS workbench_projects (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentLedgerRepo::new(pool);
        repo.finalize(finalize("x", None)).await.unwrap();
        assert_eq!(repo.count_all().await.unwrap(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧版本 agent_session_ledger 无 terminal_title 列；ensure_schema 必须
    ///     ALTER 兼容旧库且不破坏既有行，升级后可读写标题。
    ///
    /// Code Logic（这个测试做什么）:
    ///     手工建不含 terminal_title 的旧 schema 表 + 一行数据 → ensure_schema →
    ///     断言列存在、旧行可读、新 finalize 带标题可落库回读。
    #[tokio::test]
    async fn ensure_schema_adds_terminal_title_column_to_legacy_table() {
        use sqlx::Row;
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 旧 schema（无 terminal_title）
        sqlx::query(
            "CREATE TABLE agent_session_ledger (\
             id TEXT PRIMARY KEY, agent_session_id TEXT NOT NULL UNIQUE, \
             project_id TEXT NOT NULL, worktree_id TEXT, provider_id TEXT NOT NULL, \
             model_id TEXT, started_at TEXT NOT NULL, ended_at TEXT NOT NULL, \
             duration_ms INTEGER NOT NULL, outcome TEXT NOT NULL, input_tokens INTEGER, \
             output_tokens INTEGER, cache_read_tokens INTEGER, cache_write_tokens INTEGER, \
             cost_minor_units INTEGER, cost_currency TEXT, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO agent_session_ledger (id, agent_session_id, project_id, provider_id, \
             started_at, ended_at, duration_ms, outcome, created_at, updated_at) \
             VALUES ('old-id', 'old-a1', 'p1', 'claudeCodeVisible', \
             '2026-07-01T10:00:00Z', '2026-07-01T10:05:00Z', 300000, 'completed', \
             '2026-07-01T10:05:00Z', '2026-07-01T10:05:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        // 列已补
        let columns = sqlx::query("PRAGMA table_info(agent_session_ledger)")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(columns.iter().any(|r| {
            r.try_get::<String, _>("name")
                .map(|n| n == "terminal_title")
                .unwrap_or(false)
        }));
        let repo = AgentLedgerRepo::new(pool);
        // 旧行可读且标题为 null
        let legacy = repo
            .get_by_agent_session_id("old-a1")
            .await
            .unwrap()
            .unwrap();
        assert!(legacy.terminal_title.is_none());
        // 新行带标题可写
        let mut input = finalize("a1", None);
        input.terminal_title = Some("fix: 登录崩溃".into());
        let entry = repo.finalize(input).await.unwrap();
        assert_eq!(entry.terminal_title.as_deref(), Some("fix: 登录崩溃"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     「最近会话」明细行需要展示终端窗口标题；finalize 写入后 get/get_page 必须一致回读。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 带 terminal_title → get_by_agent_session_id 与 get_page 均返回同值。
    #[tokio::test]
    async fn terminal_title_round_trips_through_get_and_page() {
        let repo = ledger_repo().await;
        let mut input = finalize("a1", None);
        input.terminal_title = Some("refactor: 抽取 repo 层".into());
        let entry = repo.finalize(input).await.unwrap();
        assert_eq!(
            entry.terminal_title.as_deref(),
            Some("refactor: 抽取 repo 层")
        );
        let got = repo.get_by_agent_session_id("a1").await.unwrap().unwrap();
        assert_eq!(
            got.terminal_title.as_deref(),
            Some("refactor: 抽取 repo 层")
        );
        let page = repo.get_page(default_query()).await.unwrap();
        assert_eq!(
            page.items[0].terminal_title.as_deref(),
            Some("refactor: 抽取 repo 层")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终态重放（null-fill）不得清掉已落库的终端标题；首次无标题时后续可补。
    ///
    /// Code Logic（这个测试做什么）:
    ///     行 A：先带 title 再 None 重放 → title 保留；行 B：先 None 再带 title → title 补齐。
    #[tokio::test]
    async fn terminal_title_null_fill_keeps_existing_and_fills_missing() {
        let repo = ledger_repo().await;
        // 已有 title：后续 finalize None 不清空
        let mut first = finalize("a1", None);
        first.terminal_title = Some("keep-me".into());
        repo.finalize(first).await.unwrap();
        repo.finalize(finalize("a1", None)).await.unwrap();
        let kept = repo.get_by_agent_session_id("a1").await.unwrap().unwrap();
        assert_eq!(kept.terminal_title.as_deref(), Some("keep-me"));
        // 首次无 title：后续 finalize 可补
        repo.finalize(finalize("a2", None)).await.unwrap();
        let mut later = finalize("a2", None);
        later.terminal_title = Some("fill-me".into());
        repo.finalize(later).await.unwrap();
        let filled = repo.get_by_agent_session_id("a2").await.unwrap().unwrap();
        assert_eq!(filled.terminal_title.as_deref(), Some("fill-me"));
    }

    /// Business Logic: 非法 outcome 不得入库（通过类型系统；parse 失败在 map）。
    #[tokio::test]
    async fn invalid_outcome_rejected_at_parse() {
        assert!(AgentLedgerOutcome::parse("running").is_none());
    }

    /// Business Logic: 小写货币拒绝。
    #[tokio::test]
    async fn lowercase_currency_rejected() {
        let repo = ledger_repo().await;
        // finalize 会把非法 currency 的 cost 置 null 而非拒绝整行（cost 可选）
        // 但 validate 路径：usage 带小写 → cost 不写
        let mut u = usage(1, 1, "usd", "1.00");
        // convert fails → cost null
        let entry = repo
            .finalize(finalize("c1", Some(u.clone())))
            .await
            .unwrap();
        assert!(entry.cost_minor_units.is_none());
        // 直接 validate
        assert!(validate_currency_code("usd").is_err());
        let _ = &mut u;
    }

    /// Business Logic: 负/溢出 token 在 i64 边界拒绝。
    #[tokio::test]
    async fn overflow_tokens_rejected() {
        let repo = ledger_repo().await;
        let u = ReliableUsageSnapshot {
            input_tokens: Some(u64::MAX),
            ..Default::default()
        };
        let err = repo.finalize(finalize("ov", Some(u))).await;
        assert!(err.is_err());
    }

    /// Business Logic: counter 回退拒绝。
    #[tokio::test]
    async fn counter_rollback_rejected() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", Some(usage(100, 10, "USD", "0.10"))))
            .await
            .unwrap();
        let err = repo
            .finalize(finalize("a1", Some(usage(50, 10, "USD", "0.10"))))
            .await;
        assert!(err.is_err());
    }

    /// Business Logic: provider/project 冲突拒绝。
    #[tokio::test]
    async fn conflicting_provider_or_project_rejected() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", None)).await.unwrap();
        let mut other = finalize("a1", None);
        other.project_id = "p2".into();
        assert!(repo.finalize(other).await.is_err());
        let mut other2 = finalize("a1", None);
        other2.provider_id = "codexVisible".into();
        assert!(repo.finalize(other2).await.is_err());
    }

    /// Business Logic: 更晚 endedAt 更正 duration。
    #[tokio::test]
    async fn later_ended_at_corrects_duration() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", None)).await.unwrap();
        let mut later = finalize("a1", None);
        later.ended_at = "2026-07-01T10:10:00Z".into();
        let entry = repo.finalize(later).await.unwrap();
        assert_eq!(entry.ended_at, "2026-07-01T10:10:00Z");
        assert_eq!(entry.duration_ms, 600_000);
    }

    /// Business Logic: DTO 字段扫描无禁止名。
    #[tokio::test]
    async fn dto_field_scan_for_forbidden_names() {
        let repo = ledger_repo().await;
        let entry = repo
            .finalize(finalize("a1", Some(usage(1, 2, "USD", "0.01"))))
            .await
            .unwrap();
        let json = serde_json::to_value(&entry).unwrap();
        assert!(scan_forbidden_ledger_field_names(&json).is_empty());
    }

    /// Business Logic: clear_all 幂等且只清 ledger。
    #[tokio::test]
    async fn clear_all_is_idempotent() {
        let repo = ledger_repo().await;
        repo.finalize(finalize("a1", None)).await.unwrap();
        assert_eq!(repo.clear_all().await.unwrap(), 1);
        assert_eq!(repo.clear_all().await.unwrap(), 0);
        assert_eq!(repo.count_all().await.unwrap(), 0);
    }

    /// Business Logic: clear 必须持久化水位，供 reconcile 排除旧 session。
    #[tokio::test]
    async fn clear_all_persists_watermark() {
        let repo = ledger_repo().await;
        assert!(repo.get_clear_watermark().await.unwrap().is_none());
        repo.finalize(finalize("a1", None)).await.unwrap();
        repo.clear_all().await.unwrap();
        let wm = repo.get_clear_watermark().await.unwrap().unwrap();
        assert!(!wm.is_empty());
        // clear 之前的 ended_at 不得再写入
        assert!(!repo
            .is_ended_after_clear_watermark("2020-01-01T00:00:00Z")
            .await
            .unwrap());
        // 水位之后的 ended_at 仍允许
        assert!(repo
            .is_ended_after_clear_watermark("2099-01-01T00:00:00Z")
            .await
            .unwrap());
    }
}
