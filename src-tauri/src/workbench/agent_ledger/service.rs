//! workbench/agent_ledger/service — 终态写入与失败隔离
//!
//! Business Logic（为什么需要这个模块）:
//!     A1 runtime 首次进入终态时异步 finalize Ledger；写失败不得改变 runtime outcome，
//!     至多后台重试一次并累计有界 metric。
//!
//! Code Logic（这个模块做什么）:
//!     内存 cumulative usage 合并、record_terminal、reconcile、失败注入钩子（测试）。

use crate::error::AppError;
use crate::storage::agent_ledger_repo::AgentLedgerRepo;
use crate::storage::workbench_agent_session_repo::WorkbenchAgentSessionRepo;
use crate::workbench::agent_ledger::models::{
    merge_usage_monotonic, AgentLedgerFinalizeInput, AgentLedgerOutcome, ReliableUsageSnapshot,
};
use crate::workbench::agent_runtime::{AgentSessionPhase, AgentSessionRuntime};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Ledger 写入服务（metadata-only；失败不阻断 runtime）。
///
/// Business Logic（为什么需要这个结构体）:
///     集中 usage 缓存、幂等 finalize 与有界重试 metric。
///
/// Code Logic（这个结构体做什么）:
///     持有 repo、usage_cache、metrics；可选 fail 钩子供测试。
#[derive(Clone)]
pub struct AgentLedgerService {
    repo: AgentLedgerRepo,
    usage_cache: Arc<Mutex<HashMap<String, ReliableUsageSnapshot>>>,
    /// 后台重试次数累计
    retry_count: Arc<AtomicU64>,
    /// 最终失败次数累计
    failure_metric: Arc<AtomicU64>,
    /// 测试：强制 finalize 失败
    fail_writes: Arc<AtomicBool>,
}

impl AgentLedgerService {
    /// 构造服务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState / 测试 fixture 共享同一 service。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 repo 与空 cache/metrics。
    pub fn new(repo: AgentLedgerRepo) -> Self {
        Self {
            repo,
            usage_cache: Arc::new(Mutex::new(HashMap::new())),
            retry_count: Arc::new(AtomicU64::new(0)),
            failure_metric: Arc::new(AtomicU64::new(0)),
            fail_writes: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 返回底层 repo。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层分页/清除/聚合直接访问 repo。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone repo。
    pub fn repo(&self) -> AgentLedgerRepo {
        self.repo.clone()
    }

    /// 测试：强制后续 finalize 失败。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     验证 Ledger 失败不改变 runtime 终态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     设置 fail_writes 原子标志。
    #[cfg(test)]
    pub fn set_fail_writes(&self, fail: bool) {
        self.fail_writes.store(fail, Ordering::SeqCst);
    }

    /// 后台重试计数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     可观测有界重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     load AtomicU64。
    pub fn ledger_retry_count(&self) -> u64 {
        self.retry_count.load(Ordering::SeqCst)
    }

    /// 最终失败计数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     可观测吞掉的二次失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     load AtomicU64。
    pub fn ledger_failure_metric(&self) -> u64 {
        self.failure_metric.load(Ordering::SeqCst)
    }

    /// 记录/合并 adapter 可靠 cumulative usage（可在终态前后调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     usage 可能早于或晚于 terminal 到达；只能 structured merge，禁止 regex。
    ///
    /// Code Logic（这个函数做什么）:
    ///     cache 单调 merge；若已有 ledger 行则 finalize null-fill。
    pub async fn note_usage(
        &self,
        agent_session_id: &str,
        usage: ReliableUsageSnapshot,
    ) -> Result<(), AppError> {
        if !usage.has_any() {
            return Ok(());
        }
        let id = agent_session_id.trim();
        if id.is_empty() {
            return Ok(());
        }
        {
            let mut guard = self
                .usage_cache
                .lock()
                .map_err(|_| AppError::generic("usage_cache 锁中毒"))?;
            let entry = guard.entry(id.to_string()).or_default();
            *entry = merge_usage_monotonic(entry, &usage)?;
        }
        // 若已 finalize，尝试 null-fill（需要完整 identity —— 仅当行存在时用行上 identity）
        if let Some(existing) = self.repo.get_by_agent_session_id(id).await? {
            let cached = self
                .usage_cache
                .lock()
                .map_err(|_| AppError::generic("usage_cache 锁中毒"))?
                .get(id)
                .cloned()
                .unwrap_or_default();
            let input = AgentLedgerFinalizeInput {
                agent_session_id: existing.agent_session_id,
                project_id: existing.project_id,
                worktree_id: existing.worktree_id,
                provider_id: existing.provider_id,
                model_id: existing.model_id,
                started_at: existing.started_at,
                ended_at: existing.ended_at,
                outcome: existing.outcome,
                usage: Some(cached),
            };
            let _ = self.try_finalize_once(input).await;
        }
        Ok(())
    }

    /// 将 runtime phase + outcome_code 映射为 Ledger outcome。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     四态 outcome 与 A1 phase 对齐；cancelled 可从 outcome_code 识别。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Completed→Completed；Failed+cancel code→Cancelled；Disconnected→Disconnected。
    pub fn map_outcome(
        phase: AgentSessionPhase,
        outcome_code: Option<&str>,
    ) -> Option<AgentLedgerOutcome> {
        match phase {
            AgentSessionPhase::Completed => Some(AgentLedgerOutcome::Completed),
            AgentSessionPhase::Failed => {
                let code = outcome_code.unwrap_or("").to_ascii_lowercase();
                if code == "cancelled" || code == "canceled" || code == "cancel" {
                    Some(AgentLedgerOutcome::Cancelled)
                } else {
                    Some(AgentLedgerOutcome::Failed)
                }
            }
            AgentSessionPhase::Disconnected => Some(AgentLedgerOutcome::Disconnected),
            _ => None,
        }
    }

    /// 首次观察到终态时记录 ledger（同步路径；调用方应 spawn 以免阻塞）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     runtime 真值已落库后旁路写 ledger；失败只记 metric。
    ///
    /// Code Logic（这个函数做什么）:
    ///     组 FinalizeInput → try_finalize + 一次后台重试语义（调用方 async 重试）。
    pub async fn record_terminal(
        &self,
        session: &AgentSessionRuntime,
        previous_phase: Option<AgentSessionPhase>,
    ) {
        // 仅在首次进入终态时写
        let was_terminal = previous_phase.map(|p| p.is_terminal()).unwrap_or(false);
        if was_terminal || !session.phase.is_terminal() {
            return;
        }
        let Some(outcome) = Self::map_outcome(session.phase, session.outcome_code.as_deref())
        else {
            return;
        };
        let ended_at = session
            .ended_at
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| session.last_activity_at.clone());
        let usage = self
            .usage_cache
            .lock()
            .ok()
            .and_then(|g| g.get(&session.id).cloned());
        let input = AgentLedgerFinalizeInput {
            agent_session_id: session.id.clone(),
            project_id: session.project_id.clone(),
            worktree_id: session.worktree_id.clone(),
            provider_id: session.provider_id.clone(),
            model_id: None,
            started_at: session.started_at.clone(),
            ended_at,
            outcome,
            usage,
        };
        if self.try_finalize_with_retry(input).await.is_err() {
            // 已累计 metric；不向上传播
        }
    }

    /// 尝试 finalize；失败则计 retry 再试一次；二次失败计 failure_metric。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     有界重试；永不抛到 runtime 完成路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     try → on err retry_count++ 再 try → on err failure_metric++。
    pub async fn try_finalize_with_retry(
        &self,
        input: AgentLedgerFinalizeInput,
    ) -> Result<(), AppError> {
        match self.try_finalize_once(input.clone()).await {
            Ok(()) => Ok(()),
            Err(e1) => {
                self.retry_count.fetch_add(1, Ordering::SeqCst);
                tracing::debug!(
                    agent_session_id = %input.agent_session_id,
                    error = %e1,
                    "agent ledger finalize failed; retry once"
                );
                match self.try_finalize_once(input.clone()).await {
                    Ok(()) => Ok(()),
                    Err(e2) => {
                        self.failure_metric.fetch_add(1, Ordering::SeqCst);
                        tracing::warn!(
                            agent_session_id = %input.agent_session_id,
                            error = %e2,
                            "agent ledger finalize failed after retry; swallowed"
                        );
                        Err(e2)
                    }
                }
            }
        }
    }

    /// 单次 finalize（可被 fail 钩子拦截）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     统一写入口，便于测试注入失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     fail_writes 时 Err；否则 repo.finalize。
    async fn try_finalize_once(&self, input: AgentLedgerFinalizeInput) -> Result<(), AppError> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(AppError::generic("injected ledger write failure"));
        }
        // 日志/错误不得带 prompt/path：input 本身无这些字段
        self.repo.finalize(input).await?;
        Ok(())
    }

    /// 启动对账：扫描终态 runtime 缺失 ledger 行并补写。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 重启后需补齐未写 ledger 的终态 session；不得重开 transcript。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SQL 左连接缺失行 → record_terminal 等价 finalize。
    pub async fn reconcile_terminal_sessions(
        &self,
        agent_repo: &WorkbenchAgentSessionRepo,
    ) -> Result<u64, AppError> {
        // 直接通过 agent repo 的 pool 查询 terminal 且无 ledger 的行
        let pool = agent_repo.pool();
        let rows = sqlx::query(
            "SELECT s.id, s.project_id, s.worktree_id, s.provider_id, s.phase, \
                    s.started_at, s.last_activity_at, s.ended_at, s.outcome_code \
             FROM workbench_agent_sessions s \
             LEFT JOIN agent_session_ledger l ON l.agent_session_id = s.id \
             WHERE s.phase IN ('completed', 'failed', 'disconnected') \
               AND l.id IS NULL \
             LIMIT 500",
        )
        .fetch_all(&pool)
        .await?;

        let mut written = 0u64;
        for row in rows {
            use sqlx::Row;
            let id: String = row.try_get("id")?;
            let project_id: String = row.try_get("project_id")?;
            let worktree_id: Option<String> = row.try_get("worktree_id")?;
            let provider_id: String = row.try_get("provider_id")?;
            let phase_raw: String = row.try_get("phase")?;
            let started_at: String = row.try_get("started_at")?;
            let last_activity_at: String = row.try_get("last_activity_at")?;
            let ended_at: Option<String> = row.try_get("ended_at")?;
            let outcome_code: Option<String> = row.try_get("outcome_code")?;
            let phase = AgentSessionPhase::parse(&phase_raw).unwrap_or(AgentSessionPhase::Failed);
            let Some(outcome) = Self::map_outcome(phase, outcome_code.as_deref()) else {
                continue;
            };
            let ended = ended_at
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(last_activity_at);
            let usage = self
                .usage_cache
                .lock()
                .ok()
                .and_then(|g| g.get(&id).cloned());
            let input = AgentLedgerFinalizeInput {
                agent_session_id: id,
                project_id,
                worktree_id,
                provider_id,
                model_id: None,
                started_at,
                ended_at: ended,
                outcome,
                usage,
            };
            if self.try_finalize_with_retry(input).await.is_ok() {
                written += 1;
            }
        }
        Ok(written)
    }
}

/// 从 AppState 触发终态 ledger 写入（spawn 友好，吞错误）。
///
/// Business Logic（为什么需要这个函数）:
///     agent runtime worker 在 Applied 终态后旁路调用，不得 await 失败影响完成。
///
/// Code Logic（这个函数做什么）:
///     调 service.record_terminal。
pub async fn on_agent_runtime_terminal(
    service: &AgentLedgerService,
    session: &AgentSessionRuntime,
    previous_phase: Option<AgentSessionPhase>,
) {
    service.record_terminal(session, previous_phase).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::workbench_agent_session_repo::WorkbenchAgentSessionRepo;
    use crate::workbench::agent_runtime::{AgentSessionPhase, CreateActiveAgentSession};
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    async fn setup() -> (AgentLedgerService, WorkbenchAgentSessionRepo) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentLedgerRepo::ensure_schema(&pool).await.unwrap();
        WorkbenchAgentSessionRepo::ensure_schema(&pool)
            .await
            .unwrap();
        let ledger = AgentLedgerRepo::new(pool.clone());
        let agents = WorkbenchAgentSessionRepo::new(pool);
        (AgentLedgerService::new(ledger), agents)
    }

    fn runtime(id: &str, phase: AgentSessionPhase) -> AgentSessionRuntime {
        AgentSessionRuntime {
            id: id.into(),
            project_id: "p1".into(),
            worktree_id: None,
            terminal_session_id: "t1".into(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: "claudeCodeVisible".into(),
            native_session_id: None,
            phase,
            version: 2,
            started_at: "2026-07-01T10:00:00Z".into(),
            last_activity_at: "2026-07-01T10:05:00Z".into(),
            ended_at: Some("2026-07-01T10:05:00Z".into()),
            outcome_code: None,
            resumed_from_agent_session_id: None,
            is_active: false,
        }
    }

    /// Business Logic: Ledger 失败不改变 runtime 终态语义（由调用方保证）；metric 递增。
    #[tokio::test]
    async fn ledger_failure_never_changes_runtime_terminal_outcome() {
        let (svc, agents) = setup().await;
        // 创建 active 再 end 为 completed
        let created = agents
            .create_active(CreateActiveAgentSession {
                id: Some("a1".into()),
                project_id: "p1".into(),
                worktree_id: None,
                terminal_session_id: "t1".into(),
                orchestrator_task_id: None,
                orchestrator_attempt: None,
                provider_id: "claudeCodeVisible".into(),
                native_session_id: None,
                phase: AgentSessionPhase::Working,
                started_at: "2026-07-01T10:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        assert_eq!(created.phase, AgentSessionPhase::Working);

        svc.set_fail_writes(true);
        let terminal = runtime("a1", AgentSessionPhase::Completed);
        svc.record_terminal(&terminal, Some(AgentSessionPhase::Working))
            .await;
        assert_eq!(svc.ledger_retry_count(), 1);
        assert_eq!(svc.ledger_failure_metric(), 1);
        // runtime 行仍可由 agents 读到（未因 ledger 回滚）
        let row = agents.get("a1").await.unwrap().unwrap();
        // 尚未 apply completed mutation——fixture 只测 service 不改 agents
        assert_eq!(row.phase, AgentSessionPhase::Working);
        // 关键：service 失败被吞
        assert!(!svc.repo().exists_agent_session("a1").await.unwrap());
    }

    /// Business Logic: 各终态 outcome 映射。
    #[tokio::test]
    async fn maps_every_terminal_outcome() {
        assert_eq!(
            AgentLedgerService::map_outcome(AgentSessionPhase::Completed, None),
            Some(AgentLedgerOutcome::Completed)
        );
        assert_eq!(
            AgentLedgerService::map_outcome(AgentSessionPhase::Failed, None),
            Some(AgentLedgerOutcome::Failed)
        );
        assert_eq!(
            AgentLedgerService::map_outcome(AgentSessionPhase::Failed, Some("cancelled")),
            Some(AgentLedgerOutcome::Cancelled)
        );
        assert_eq!(
            AgentLedgerService::map_outcome(AgentSessionPhase::Disconnected, None),
            Some(AgentLedgerOutcome::Disconnected)
        );
        assert_eq!(
            AgentLedgerService::map_outcome(AgentSessionPhase::Working, None),
            None
        );
    }

    /// Business Logic: 非终态不写。
    #[tokio::test]
    async fn nonterminal_event_no_write() {
        let (svc, _) = setup().await;
        let row = runtime("a1", AgentSessionPhase::Working);
        svc.record_terminal(&row, Some(AgentSessionPhase::Launching))
            .await;
        assert!(!svc.repo().exists_agent_session("a1").await.unwrap());
    }

    /// Business Logic: 重复终态 version 不重复写。
    #[tokio::test]
    async fn duplicate_terminal_does_not_duplicate_row() {
        let (svc, _) = setup().await;
        let row = runtime("a1", AgentSessionPhase::Completed);
        svc.record_terminal(&row, Some(AgentSessionPhase::Working))
            .await;
        svc.record_terminal(&row, Some(AgentSessionPhase::Completed))
            .await;
        assert_eq!(svc.repo().count_all().await.unwrap(), 1);
    }

    /// Business Logic: usage 可在终态前到达。
    #[tokio::test]
    async fn usage_before_terminal_is_merged() {
        let (svc, _) = setup().await;
        svc.note_usage(
            "a1",
            ReliableUsageSnapshot {
                input_tokens: Some(10),
                output_tokens: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let row = runtime("a1", AgentSessionPhase::Completed);
        svc.record_terminal(&row, Some(AgentSessionPhase::Working))
            .await;
        let entry = svc
            .repo()
            .get_by_agent_session_id("a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.input_tokens, Some(10));
        assert_eq!(entry.output_tokens, Some(4));
    }

    /// Business Logic: usage 在终态后 null-fill。
    #[tokio::test]
    async fn usage_after_terminal_null_fills() {
        let (svc, _) = setup().await;
        let row = runtime("a1", AgentSessionPhase::Completed);
        svc.record_terminal(&row, Some(AgentSessionPhase::Working))
            .await;
        svc.note_usage(
            "a1",
            ReliableUsageSnapshot {
                input_tokens: Some(99),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let entry = svc
            .repo()
            .get_by_agent_session_id("a1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.input_tokens, Some(99));
    }

    /// Business Logic: 非结构化空 usage 保持 null。
    #[tokio::test]
    async fn empty_usage_stays_null() {
        let (svc, _) = setup().await;
        let row = runtime("a1", AgentSessionPhase::Completed);
        svc.record_terminal(&row, Some(AgentSessionPhase::Working))
            .await;
        let entry = svc
            .repo()
            .get_by_agent_session_id("a1")
            .await
            .unwrap()
            .unwrap();
        assert!(entry.input_tokens.is_none());
        assert!(entry.cost_minor_units.is_none());
    }

    /// Business Logic: counter 回退不写入 cache。
    #[tokio::test]
    async fn adapter_counter_rollback_rejected() {
        let (svc, _) = setup().await;
        svc.note_usage(
            "a1",
            ReliableUsageSnapshot {
                input_tokens: Some(100),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let err = svc
            .note_usage(
                "a1",
                ReliableUsageSnapshot {
                    input_tokens: Some(50),
                    ..Default::default()
                },
            )
            .await;
        assert!(err.is_err());
    }

    /// Business Logic: finalize 入参无 transcript/message/path 字段（类型保证 + JSON 扫描）。
    #[tokio::test]
    async fn no_transcript_fields_passed_to_service() {
        use crate::workbench::agent_ledger::models::scan_forbidden_ledger_field_names;
        let input = AgentLedgerFinalizeInput {
            agent_session_id: "a".into(),
            project_id: "p".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-07-01T10:00:00Z".into(),
            ended_at: "2026-07-01T10:01:00Z".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: None,
        };
        // FinalizeInput 不 Serialize；用 entry 形状等价检查
        let (svc, _) = setup().await;
        svc.try_finalize_with_retry(input).await.unwrap();
        let entry = svc
            .repo()
            .get_by_agent_session_id("a")
            .await
            .unwrap()
            .unwrap();
        let json = serde_json::to_value(&entry).unwrap();
        assert!(scan_forbidden_ledger_field_names(&json).is_empty());
    }

    /// Business Logic: reconcile 补写缺失 ledger。
    #[tokio::test]
    async fn restart_reconciliation_writes_missing_rows() {
        let (svc, agents) = setup().await;
        agents
            .create_active(CreateActiveAgentSession {
                id: Some("a9".into()),
                project_id: "p1".into(),
                worktree_id: None,
                terminal_session_id: "t9".into(),
                orchestrator_task_id: None,
                orchestrator_attempt: None,
                provider_id: "claudeCodeVisible".into(),
                native_session_id: None,
                phase: AgentSessionPhase::Working,
                started_at: "2026-07-01T10:00:00Z".into(),
                resumed_from_agent_session_id: None,
            })
            .await
            .unwrap();
        // 直接 SQL 标为 completed 终态（模拟崩溃前已终态未写 ledger）
        sqlx::query(
            "UPDATE workbench_agent_sessions SET phase='completed', is_active=0, \
             ended_at='2026-07-01T10:05:00Z' WHERE id='a9'",
        )
        .execute(&agents.pool())
        .await
        .unwrap();
        let n = svc.reconcile_terminal_sessions(&agents).await.unwrap();
        assert_eq!(n, 1);
        assert!(svc.repo().exists_agent_session("a9").await.unwrap());
    }
}
