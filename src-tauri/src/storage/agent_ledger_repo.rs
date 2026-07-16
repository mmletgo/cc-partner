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
    validate_currency_code, AgentLedgerEntry, AgentLedgerFinalizeInput, AgentLedgerOutcome,
    AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary, CurrencyAmount, LedgerUsageCoverage,
    LedgerWindow, ReliableUsageSnapshot,
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
    created_at, updated_at";

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
    ///     旧库无迁移框架；CREATE IF NOT EXISTS 即可升级。
    ///
    /// Code Logic（这个函数做什么）:
    ///     ledger 表 + clear watermark 表 + 三个索引；bootstrap 不经 write lease。
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
                  created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
    ///     校验 project/provider/outcome；合并 usage；later ended_at 重算 duration。
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
            },
            &ReliableUsageSnapshot {
                model_id: incoming.model_id.clone().or(input.model_id.clone()),
                input_tokens: incoming.input_tokens,
                output_tokens: incoming.output_tokens,
                cache_read_tokens: incoming.cache_read_tokens,
                cache_write_tokens: incoming.cache_write_tokens,
                cost_major: None,
                cost_currency: incoming.cost_currency.clone(),
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
             cost_minor_units = ?, cost_currency = ?, updated_at = ? \
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

        let cost_by_currency = cost_map
            .into_iter()
            .map(|(currency, minor_units)| CurrencyAmount {
                currency,
                minor_units,
            })
            .collect();

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
            cost_by_currency,
            usage_coverage,
        })
    }
}

/// 动态绑定值。
enum BindValue {
    Text(String),
    Int(i64),
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
