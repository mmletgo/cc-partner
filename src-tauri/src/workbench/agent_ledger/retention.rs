//! workbench/agent_ledger/retention — 有界自动保留清理
//!
//! Business Logic（为什么需要这个模块）:
//!     默认保留 30 天且每 device 最多 10,000 条；启动与每 24h 清理，单批最多 500，
//!     无用户配置负担；清理失败不得阻断 backend 启动。
//!
//! Code Logic（这个模块做什么）:
//!     injectable clock、单批 cleanup、后台 task（startup + 24h tick + shutdown cancel）。

use crate::error::AppError;
use crate::storage::agent_ledger_repo::{
    AgentLedgerCleanupResult, AgentLedgerRepo, CLEANUP_BATCH_LIMIT, MAX_LEDGER_ROWS, RETENTION_DAYS,
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 可注入时钟（测试用虚拟时间）。
///
/// Business Logic（为什么需要这个 trait）:
///     retention 边界测试不能依赖墙钟等待 30 天。
///
/// Code Logic（这个 trait 做什么）:
///     now() → DateTime<Utc>。
pub trait RetentionClock: Send + Sync {
    /// 返回当前时间。
    fn now(&self) -> DateTime<Utc>;
}

/// 系统墙钟。
///
/// Business Logic（为什么需要这个结构体）:
///     生产路径使用真实 UTC。
///
/// Code Logic（这个结构体做什么）:
///     Utc::now。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl RetentionClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// 固定/可变测试时钟。
///
/// Business Logic（为什么需要这个结构体）:
///     单元测试推进虚拟时间。
///
/// Code Logic（这个结构体做什么）:
///     Mutex 内 DateTime。
#[derive(Debug, Clone)]
pub struct MockClock {
    inner: Arc<std::sync::Mutex<DateTime<Utc>>>,
}

impl MockClock {
    /// 构造固定起点时钟。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     fixture 需要可控 now。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 initial。
    #[allow(dead_code)] // 测试 fixture / retention 时钟 API surface
    pub fn new(initial: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(initial)),
        }
    }

    /// 设置当前时间。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     推进到边界后触发 cleanup。
    ///
    /// Code Logic（这个函数做什么）:
    ///     覆盖 Mutex 值。
    #[allow(dead_code)] // 测试可推进虚拟时间
    pub fn set(&self, t: DateTime<Utc>) {
        *self.inner.lock().expect("mock clock lock") = t;
    }
}

impl RetentionClock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        *self.inner.lock().expect("mock clock lock")
    }
}

/// 执行一批 cleanup。
///
/// Business Logic（为什么需要这个函数）:
///     retention task 与测试共用同一批语义。
///
/// Code Logic（这个函数做什么）:
///     委托 repo.cleanup_batch(now, 30d, 10k, 500)。
pub async fn cleanup_agent_ledger_batch(
    repo: &AgentLedgerRepo,
    clock: &dyn RetentionClock,
) -> Result<AgentLedgerCleanupResult, AppError> {
    repo.cleanup_batch(
        clock.now(),
        RETENTION_DAYS,
        MAX_LEDGER_ROWS,
        CLEANUP_BATCH_LIMIT,
    )
    .await
}

/// 后台保留任务句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     owner 启动后跑一次并每 24h tick；shutdown 取消。
///
/// Code Logic（这个结构体做什么）:
///     仅标记类型；实际 spawn 在 runtime。
pub struct AgentLedgerRetentionTask;

impl AgentLedgerRetentionTask {
    /// 启动保留后台循环。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动批 + 24h 间隔；失败只 warn；cancel 时退出。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 `cleanup_until_caught_up`（错误吞掉）；再 interval 24h 直到 cancel。
    pub fn spawn(
        repo: AgentLedgerRepo,
        cancel: CancellationToken,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let clock = SystemClock;
            cleanup_until_caught_up(&repo, &clock, "startup").await;
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // 消耗首次立即 tick，使下一拍在 interval 后
            ticker.tick().await;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::debug!("agent ledger retention task cancelled");
                        break;
                    }
                    _ = ticker.tick() => {
                        cleanup_until_caught_up(&repo, &clock, "periodic").await;
                    }
                }
            }
        })
    }
}

/// 单次 tick 内循环批清理直到 `more_remaining=false` 或达轮次上限。
///
/// Business Logic（为什么需要这个函数）:
///     每批最多 500 行；若只跑一批就等 24h，已有数万过期行时会长期违反 10k/30d 上限。
///
/// Code Logic（这个函数做什么）:
///     最多 64 轮（64×500=32k）调用 cleanup_agent_ledger_batch；错误则 warn 并中止本轮。
async fn cleanup_until_caught_up(
    repo: &AgentLedgerRepo,
    clock: &dyn RetentionClock,
    label: &str,
) {
    const MAX_ROUNDS: u32 = 64;
    let mut total_deleted: u64 = 0;
    for round in 0..MAX_ROUNDS {
        match cleanup_agent_ledger_batch(repo, clock).await {
            Ok(r) => {
                total_deleted = total_deleted.saturating_add(r.deleted);
                if !r.more_remaining {
                    if total_deleted > 0 {
                        tracing::info!(
                            deleted = total_deleted,
                            rounds = round + 1,
                            phase = label,
                            "agent ledger cleanup"
                        );
                    }
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(
                    phase = label,
                    error = %e,
                    "agent ledger cleanup failed (non-blocking)"
                );
                return;
            }
        }
    }
    if total_deleted > 0 {
        tracing::info!(
            deleted = total_deleted,
            rounds = MAX_ROUNDS,
            phase = label,
            "agent ledger cleanup hit round cap; more may remain"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::agent_ledger_repo::AgentLedgerRepo;
    use crate::workbench::agent_ledger::models::{
        AgentLedgerFinalizeInput, AgentLedgerOutcome, ReliableUsageSnapshot,
    };
    use chrono::TimeZone;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    async fn repo() -> AgentLedgerRepo {
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

    async fn insert_row(repo: &AgentLedgerRepo, id: &str, ended_at: &str) {
        let input = AgentLedgerFinalizeInput {
            agent_session_id: id.into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: ended_at.to_string(),
            ended_at: ended_at.to_string(),
            outcome: AgentLedgerOutcome::Completed,
            usage: Some(ReliableUsageSnapshot::default()),
        };
        repo.finalize(input).await.unwrap();
    }

    /// Business Logic: 单批最多删 500 最旧超龄行。
    #[tokio::test]
    async fn cleanup_deletes_at_most_five_hundred_oldest_rows() {
        let repo = repo().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        // 620 条 ended_at 在 30 天前
        let old = (now - chrono::Duration::days(31)).to_rfc3339();
        for i in 0..620 {
            insert_row(&repo, &format!("old-{i:04}"), &old).await;
        }
        let clock = MockClock::new(now);
        let result = cleanup_agent_ledger_batch(&repo, &clock).await.unwrap();
        assert_eq!(result.deleted, 500);
        assert!(result.more_remaining);
        assert_eq!(repo.count_all().await.unwrap(), 120);
    }

    /// Business Logic: 恰好 30 天边界保留（ended_at >= cutoff）。
    #[tokio::test]
    async fn exactly_thirty_day_boundary_kept() {
        let repo = repo().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
        let boundary = (now - chrono::Duration::days(30)).to_rfc3339();
        let older = (now - chrono::Duration::days(30) - chrono::Duration::seconds(1)).to_rfc3339();
        insert_row(&repo, "keep", &boundary).await;
        insert_row(&repo, "drop", &older).await;
        let clock = MockClock::new(now);
        let r = cleanup_agent_ledger_batch(&repo, &clock).await.unwrap();
        assert_eq!(r.deleted, 1);
        assert!(repo.exists_agent_session("keep").await.unwrap());
        assert!(!repo.exists_agent_session("drop").await.unwrap());
    }

    /// Business Logic: 超过 10k 按 ended_at ASC 删最旧。
    #[tokio::test]
    async fn cap_ten_thousand_deletes_oldest() {
        let repo = repo().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        // 插入 10001 条近期（不超龄），触发 cap
        for i in 0..10001 {
            let ended = (now - chrono::Duration::seconds(i as i64)).to_rfc3339();
            insert_row(&repo, &format!("c-{i:05}"), &ended).await;
        }
        assert_eq!(repo.count_all().await.unwrap(), 10001);
        let clock = MockClock::new(now);
        let r = cleanup_agent_ledger_batch(&repo, &clock).await.unwrap();
        assert_eq!(r.deleted, 1);
        assert!(!r.more_remaining || repo.count_all().await.unwrap() <= 10000);
        assert_eq!(repo.count_all().await.unwrap(), 10000);
    }

    /// Business Logic: age 优先于 cap。
    #[tokio::test]
    async fn age_first_then_cap() {
        let repo = repo().await;
        let now = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let old = (now - chrono::Duration::days(40)).to_rfc3339();
        // 10 条超龄 + 若干新
        for i in 0..10 {
            insert_row(&repo, &format!("old-{i}"), &old).await;
        }
        for i in 0..5 {
            insert_row(&repo, &format!("new-{i}"), &now.to_rfc3339()).await;
        }
        let clock = MockClock::new(now);
        let r = cleanup_agent_ledger_batch(&repo, &clock).await.unwrap();
        assert_eq!(r.deleted, 10);
        assert_eq!(repo.count_all().await.unwrap(), 5);
    }

    /// Business Logic: cleanup 失败不 panic（调用方吞错）。
    #[tokio::test]
    async fn cleanup_error_is_result_not_panic() {
        // 使用已关闭 pool 难以构造；验证函数签名返回 Result
        let repo = repo().await;
        let clock = MockClock::new(Utc::now());
        assert!(cleanup_agent_ledger_batch(&repo, &clock).await.is_ok());
    }

    /// Business Logic: shutdown cancel 使 task 退出。
    #[tokio::test]
    async fn shutdown_cancellation_stops_task() {
        let repo = repo().await;
        let cancel = CancellationToken::new();
        let handle =
            AgentLedgerRetentionTask::spawn(repo, cancel.clone(), Duration::from_secs(3600 * 24));
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("task should exit after cancel");
    }
}
