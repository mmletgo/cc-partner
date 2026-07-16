//! workbench/agent_ledger/aggregation — 本机分页与时间窗聚合 helper
//!
//! Business Logic（为什么需要这个模块）:
//!     本机 drawer 与 summary 需要有界 query、coverage 语义与多货币桶；unknown 不转 0。
//!
//! Code Logic（这个模块做什么）:
//!     薄封装 repo.get_page / summarize / clear；规范化 limit；window 边界 helper。

use crate::error::AppError;
use crate::storage::agent_ledger_repo::{
    AgentLedgerRepo, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT,
};
use crate::workbench::agent_ledger::models::{
    AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary, LedgerWindow,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};

/// 规范化分页 limit。
///
/// Business Logic（为什么需要这个函数）:
///     默认 50、最大 200，防止无界拉取。
///
/// Code Logic（这个函数做什么）:
///     None→50；clamp 1..=200。
pub fn normalize_page_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT)
}

/// 计算 window 下界（含）RFC3339。
///
/// Business Logic（为什么需要这个函数）:
///     24h/7d/30d 精确边界测试与聚合共用。
///
/// Code Logic（这个函数做什么）:
///     now - window.duration_secs。
#[allow(dead_code)] // 供边界测试与后续 P2P summary 复用
pub fn window_start(now: DateTime<Utc>, window: LedgerWindow) -> DateTime<Utc> {
    now - ChronoDuration::seconds(window.duration_secs() as i64)
}

/// 本机分页查询。
///
/// Business Logic（为什么需要这个函数）:
///     命令层统一入口，应用 limit 规范化。
///
/// Code Logic（这个函数做什么）:
///     改写 query.limit 后 repo.get_page。
pub async fn list_entries(
    repo: &AgentLedgerRepo,
    mut query: AgentLedgerQuery,
) -> Result<AgentLedgerPage, AppError> {
    query.limit = Some(normalize_page_limit(query.limit));
    repo.get_page(query).await
}

/// 本机时间窗聚合。
///
/// Business Logic（为什么需要这个函数）:
///     desktop invoke 与 control 共用。
///
/// Code Logic（这个函数做什么）:
///     repo.summarize。
pub async fn summarize_window(
    repo: &AgentLedgerRepo,
    window: LedgerWindow,
    project_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<AgentLedgerSummary, AppError> {
    repo.summarize(window, project_id, now).await
}

/// 清除全部历史；返回删除数。
///
/// Business Logic（为什么需要这个函数）:
///     设置页一键清除；幂等。
///
/// Code Logic（这个函数做什么）:
///     repo.clear_all。
pub async fn clear_history(repo: &AgentLedgerRepo) -> Result<u64, AppError> {
    repo.clear_all().await
}

/// 对本机多个 project 做同一时间窗聚合（P2P / Fleet join）。
///
/// Business Logic（为什么需要这个函数）:
///     owning device 只返回 aggregate，控制设备再映射 remote id；单 project 失败不得拖垮批次。
///
/// Code Logic（这个函数做什么）:
///     校验 window；逐 project 调 summarize_window；保证 project_id 写回 summary。
pub async fn summarize_projects(
    repo: &AgentLedgerRepo,
    project_ids: &[String],
    window: LedgerWindow,
    now: DateTime<Utc>,
) -> Result<Vec<AgentLedgerSummary>, AppError> {
    let mut out = Vec::with_capacity(project_ids.len());
    for raw in project_ids {
        let pid = raw.trim();
        if pid.is_empty() {
            continue;
        }
        let mut summary = summarize_window(repo, window, Some(pid), now).await?;
        summary.project_id = Some(pid.to_string());
        out.push(summary);
    }
    Ok(out)
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::storage::agent_ledger_repo::AgentLedgerRepo;
    use crate::workbench::agent_ledger::models::{
        AgentLedgerFinalizeInput, AgentLedgerOutcome, LedgerUsageCoverage,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn fixture() -> AgentLedgerRepo {
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

    /// Business Logic（为什么需要这个测试）:
    ///     P2P batch 必须按 project 拆分聚合且不混入其它 project 的 session。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两 project 各 1 条 → summarize_projects 返回两条且 sessions=1。
    #[tokio::test]
    async fn summarize_projects_scopes_per_project() {
        let repo = fixture().await;
        let ended = "2026-07-15T12:00:00+00:00";
        for (id, pid) in [("a1", "p1"), ("a2", "p2")] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: pid.into(),
                worktree_id: None,
                provider_id: "claudeCodeVisible".into(),
                model_id: None,
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
            })
            .await
            .unwrap();
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let rows = summarize_projects(
            &repo,
            &["p1".into(), "p2".into()],
            LedgerWindow::Days7,
            now,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_id.as_deref(), Some("p1"));
        assert_eq!(rows[0].sessions, 1);
        assert_eq!(rows[1].project_id.as_deref(), Some("p2"));
        assert_eq!(rows[1].usage_coverage, LedgerUsageCoverage::Unavailable);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::agent_ledger_repo::{encode_ledger_cursor, AgentLedgerRepo};
    use crate::workbench::agent_ledger::models::{
        AgentLedgerFinalizeInput, AgentLedgerOutcome, LedgerUsageCoverage, ReliableUsageSnapshot,
    };
    use chrono::TimeZone;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;

    async fn fixture() -> AgentLedgerRepo {
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

    async fn put(
        repo: &AgentLedgerRepo,
        id: &str,
        ended_at: &str,
        usage: Option<ReliableUsageSnapshot>,
    ) {
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: id.into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: ended_at.into(),
            ended_at: ended_at.into(),
            outcome: AgentLedgerOutcome::Completed,
            usage,
        })
        .await
        .unwrap();
    }

    fn reliable(input: u64, output: u64) -> ReliableUsageSnapshot {
        ReliableUsageSnapshot {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        }
    }

    /// Business Logic: partial coverage 不把 unknown 当 0。
    #[tokio::test]
    async fn aggregate_marks_usage_partial_instead_of_converting_unknown_to_zero() {
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        put(
            &repo,
            "a1",
            &(now - ChronoDuration::hours(1)).to_rfc3339(),
            Some(reliable(10, 4)),
        )
        .await;
        put(
            &repo,
            "a2",
            &(now - ChronoDuration::hours(2)).to_rfc3339(),
            None,
        )
        .await;
        let summary = summarize_window(&repo, LedgerWindow::Days7, None, now)
            .await
            .unwrap();
        assert_eq!(summary.input_tokens, Some(10));
        assert_eq!(summary.usage_coverage, LedgerUsageCoverage::Partial);
        assert_eq!(summary.sessions, 2);
    }

    /// Business Logic: 默认 50 / 最大 200。
    #[tokio::test]
    async fn default_limit_fifty_max_two_hundred() {
        assert_eq!(normalize_page_limit(None), 50);
        assert_eq!(normalize_page_limit(Some(0)), 1);
        assert_eq!(normalize_page_limit(Some(999)), 200);
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 0, 0, 0).unwrap();
        for i in 0..60 {
            put(
                &repo,
                &format!("r{i}"),
                &(now - ChronoDuration::seconds(i)).to_rfc3339(),
                None,
            )
            .await;
        }
        let page = list_entries(
            &repo,
            AgentLedgerQuery {
                limit: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 50);
        assert!(page.next_cursor.is_some());
    }

    /// Business Logic: 非法 cursor 拒绝。
    #[tokio::test]
    async fn invalid_cursor_rejected() {
        let repo = fixture().await;
        let err = list_entries(
            &repo,
            AgentLedgerQuery {
                cursor: Some("not-a-cursor".into()),
                ..Default::default()
            },
        )
        .await;
        assert!(err.is_err());
    }

    /// Business Logic: 稳定 ended_at DESC,id 分页。
    #[tokio::test]
    async fn stable_pagination_order() {
        let repo = fixture().await;
        put(&repo, "b", "2026-07-10T00:00:00Z", None).await;
        put(&repo, "a", "2026-07-10T00:00:00Z", None).await;
        put(&repo, "c", "2026-07-11T00:00:00Z", None).await;
        let page = list_entries(
            &repo,
            AgentLedgerQuery {
                limit: Some(2),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items[0].agent_session_id, "c");
        // same ended_at: id DESC → b then a
        assert_eq!(page.items[1].agent_session_id, "b");
        let page2 = list_entries(
            &repo,
            AgentLedgerQuery {
                limit: Some(2),
                cursor: page.next_cursor,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page2.items[0].agent_session_id, "a");
        // encode roundtrip
        let cur = encode_ledger_cursor("2026-07-10T00:00:00Z", "b");
        assert!(!cur.is_empty());
    }

    /// Business Logic: project/provider/outcome/time filters。
    #[tokio::test]
    async fn filters_apply() {
        let repo = fixture().await;
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: "x".into(),
            project_id: "p2".into(),
            worktree_id: None,
            provider_id: "codexVisible".into(),
            model_id: None,
            started_at: "2026-07-01T00:00:00Z".into(),
            ended_at: "2026-07-01T01:00:00Z".into(),
            outcome: AgentLedgerOutcome::Failed,
            usage: None,
        })
        .await
        .unwrap();
        put(&repo, "y", "2026-07-01T02:00:00Z", None).await;
        let page = list_entries(
            &repo,
            AgentLedgerQuery {
                project_id: Some("p2".into()),
                provider_id: Some("codexVisible".into()),
                outcome: Some(AgentLedgerOutcome::Failed),
                ended_after: Some("2026-07-01T00:00:00Z".into()),
                ended_before: Some("2026-07-01T01:30:00Z".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].agent_session_id, "x");
    }

    /// Business Logic: 24h/7d/30d 边界。
    #[tokio::test]
    async fn window_boundaries() {
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        let inside_24h = (now - ChronoDuration::hours(23)).to_rfc3339();
        let outside_24h = (now - ChronoDuration::hours(25)).to_rfc3339();
        put(&repo, "in", &inside_24h, None).await;
        put(&repo, "out", &outside_24h, None).await;
        let s = summarize_window(&repo, LedgerWindow::Hours24, None, now)
            .await
            .unwrap();
        assert_eq!(s.sessions, 1);
        let s7 = summarize_window(&repo, LedgerWindow::Days7, None, now)
            .await
            .unwrap();
        assert_eq!(s7.sessions, 2);
    }

    /// Business Logic: complete / unavailable coverage。
    #[tokio::test]
    async fn coverage_complete_and_unavailable() {
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        put(
            &repo,
            "u1",
            &(now - ChronoDuration::hours(1)).to_rfc3339(),
            Some(reliable(1, 1)),
        )
        .await;
        let s = summarize_window(&repo, LedgerWindow::Days7, None, now)
            .await
            .unwrap();
        assert_eq!(s.usage_coverage, LedgerUsageCoverage::Complete);
        // empty window
        let empty = summarize_window(
            &repo,
            LedgerWindow::Hours24,
            Some("no-such-project"),
            now,
        )
        .await
        .unwrap();
        assert_eq!(empty.sessions, 0);
        assert_eq!(empty.usage_coverage, LedgerUsageCoverage::Unavailable);
        assert!(empty.input_tokens.is_none());
    }

    /// Business Logic: 多货币分桶不折算。
    #[tokio::test]
    async fn multi_currency_buckets() {
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        put(
            &repo,
            "usd",
            &(now - ChronoDuration::hours(1)).to_rfc3339(),
            Some(ReliableUsageSnapshot {
                cost_major: Some("1.00".into()),
                cost_currency: Some("USD".into()),
                input_tokens: Some(1),
                ..Default::default()
            }),
        )
        .await;
        put(
            &repo,
            "eur",
            &(now - ChronoDuration::hours(2)).to_rfc3339(),
            Some(ReliableUsageSnapshot {
                cost_major: Some("2.00".into()),
                cost_currency: Some("EUR".into()),
                input_tokens: Some(1),
                ..Default::default()
            }),
        )
        .await;
        let s = summarize_window(&repo, LedgerWindow::Days7, None, now)
            .await
            .unwrap();
        assert_eq!(s.cost_by_currency.len(), 2);
        assert!(s.cost_by_currency.iter().any(|c| c.currency == "USD" && c.minor_units == 100));
        assert!(s.cost_by_currency.iter().any(|c| c.currency == "EUR" && c.minor_units == 200));
    }

    /// Business Logic: clear 只删 ledger。
    #[tokio::test]
    async fn clear_only_ledger() {
        let repo = fixture().await;
        put(&repo, "z", "2026-07-01T00:00:00Z", None).await;
        assert_eq!(clear_history(&repo).await.unwrap(), 1);
        assert_eq!(clear_history(&repo).await.unwrap(), 0);
    }
}
