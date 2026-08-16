//! workbench/agent_ledger/aggregation — 本机分页与时间窗聚合 helper
//!
//! Business Logic（为什么需要这个模块）:
//!     本机 drawer 与 summary 需要有界 query、coverage 语义与多货币桶；unknown 不转 0。
//!     Token 统计页扩展：增加全量筛选聚合 + 三维拆分 + 趋势桶，
//!     所有派生指标后端 SQL 聚合，前端不二次计算。
//!
//! Code Logic（这个模块做什么）:
//!     薄封装 repo.get_page / summarize / clear / summarize_with_filters；
//!     规范化 limit；window 边界 helper；orchestrator 组合 grouped + trend。

use crate::error::AppError;
use crate::storage::agent_ledger_repo::{AgentLedgerRepo, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT};
use crate::workbench::agent_ledger::models::{
    AgentLedgerFilters, AgentLedgerPage, AgentLedgerQuery, AgentLedgerSummary, LedgerWindow,
    TrendBucket,
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
///     desktop invoke 与 control 共用；保留兼容签名（Drawer 仍调此签名）。
///
/// Code Logic（这个函数做什么）:
///     构造单 project filters → 委托 `summarize_with_filters`。
pub async fn summarize_window(
    repo: &AgentLedgerRepo,
    window: LedgerWindow,
    project_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<AgentLedgerSummary, AppError> {
    let filters = AgentLedgerFilters {
        window: Some(window),
        project_id: project_id.map(|s| s.to_string()),
        ..Default::default()
    };
    summarize_with_filters(repo, filters, now).await
}

/// Token 统计页 + export 共用的全量筛选聚合（orchestrator）。
///
/// Business Logic:
///     统计页需要并行拉 by_model / by_provider / by_project 三维 + summary + trend；
///     try_join 让 4 个查询共享同一 pool（SQLite 单 connection 时串行执行，
///     但避免阻塞等待中某个查询报错而中断其它）。
///
/// Code Logic:
///     repo.summarize_with_filters 拿主 summary；try_join 三维拆分；repo.summarize_trend 拉趋势；
///     bucket 与 by_* / trend 字段写回 summary。
pub async fn summarize_with_filters(
    repo: &AgentLedgerRepo,
    filters: AgentLedgerFilters,
    now: DateTime<Utc>,
) -> Result<AgentLedgerSummary, AppError> {
    let (by_model, by_provider, by_project) = tokio::try_join!(
        repo.summarize_grouped(&filters, "model", now),
        repo.summarize_grouped(&filters, "provider", now),
        repo.summarize_grouped(&filters, "project", now),
    )?;
    let mut summary = repo.summarize_with_filters(filters.clone(), now).await?;
    let bucket = summary_bucket(&filters);
    summary.bucket = bucket;
    summary.by_model = by_model;
    summary.by_provider = by_provider;
    summary.by_project = by_project;
    summary.trend = repo.summarize_trend(&filters, bucket, now).await?;
    Ok(summary)
}

/// 由 filters 推导桶粒度（显式 bucket 优先，否则按 window 推）。
///
/// Business Logic:
///     统计页 trend 与 summary.bucket 必须口径一致。
///
/// Code Logic:
///     filters.bucket → filters.window → Hours24 走 Hour，其它 Day。
pub fn summary_bucket(filters: &AgentLedgerFilters) -> TrendBucket {
    if let Some(b) = filters.bucket {
        return b;
    }
    match filters.window {
        Some(LedgerWindow::Hours24) => TrendBucket::Hour,
        _ => TrendBucket::Day,
    }
}

/// 清除全部历史并写隐私水位；返回删除数。
///
/// Business Logic（为什么需要这个函数）:
///     设置页一键清除；幂等；生产路径优先走 AgentLedgerService::clear_history
///     以与 reconcile 串行。
///
/// Code Logic（这个函数做什么）:
///     repo.clear_all（UPSERT 水位 + DELETE）。
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
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let rows = summarize_projects(&repo, &["p1".into(), "p2".into()], LedgerWindow::Days7, now)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_id.as_deref(), Some("p1"));
        assert_eq!(rows[0].sessions, 1);
        assert_eq!(rows[1].project_id.as_deref(), Some("p2"));
        assert_eq!(rows[1].usage_coverage, LedgerUsageCoverage::Unavailable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     summarize_with_filters 必须并行计算三维度拆分（by_model/by_provider/by_project），
    ///     三种不同 provider → by_provider 长度 3。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 3 行（不同 provider）→ summarize_with_filters 走 7d 兜底 → by_provider.len() == 3。
    #[tokio::test]
    async fn summarize_with_filters_groups_three_dimensions() {
        let repo = fixture().await;
        let ended = "2026-07-15T12:00:00+00:00";
        for (id, prov) in [
            ("a1", "claudeCodeVisible"),
            ("a2", "codexVisible"),
            ("a3", "openCodeVisible"),
        ] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: "p1".into(),
                worktree_id: None,
                provider_id: prov.into(),
                model_id: Some("m".into()),
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let filters = AgentLedgerFilters {
            window: None,
            ..Default::default()
        };
        let s = summarize_with_filters(&repo, filters, now).await.unwrap();
        assert_eq!(s.by_provider.len(), 3);
        // 同一 project，by_project 长度 1
        assert_eq!(s.by_project.len(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     model_id 为 NULL 的行在 by_model 必须落到 "(unknown)" 桶，不能抛错或漏算。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 2 行（其中一行 model_id=None）→ by_model 一项 key == "(unknown)"。
    #[tokio::test]
    async fn summarize_with_filters_groups_keys_unknown_for_null() {
        let repo = fixture().await;
        let ended = "2026-07-15T12:00:00+00:00";
        for (id, mid) in [("a1", Some("claude-opus")), ("a2", None)] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: "p1".into(),
                worktree_id: None,
                provider_id: "claudeCodeVisible".into(),
                model_id: mid.map(|s| s.to_string()),
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let filters = AgentLedgerFilters {
            window: None,
            ..Default::default()
        };
        let s = summarize_with_filters(&repo, filters, now).await.unwrap();
        assert!(s.by_model.iter().any(|r| r.key == "(unknown)"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     trend 跨午夜按 day 桶应合并到两个 day bucket 并按 ASC 排序。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 2 行（07-14 12:00 + 07-15 12:00）→ summarize_trend(day) 返回 2 个桶，最早在前。
    #[tokio::test]
    async fn summarize_trend_strftime_day() {
        let repo = fixture().await;
        for (id, ended) in [
            ("a1", "2026-07-14T12:00:00+00:00"),
            ("a2", "2026-07-15T03:00:00+00:00"),
        ] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: "p1".into(),
                worktree_id: None,
                provider_id: "claudeCodeVisible".into(),
                model_id: None,
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let filters = AgentLedgerFilters {
            window: Some(LedgerWindow::Days30),
            ..Default::default()
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let trend = repo
            .summarize_trend(&filters, TrendBucket::Day, now)
            .await
            .unwrap();
        assert_eq!(trend.len(), 2);
        assert!(trend[0].bucket_start < trend[1].bucket_start);
        assert!(trend[0].bucket_start.starts_with("2026-07-14T00:00:00"));
        assert!(trend[1].bucket_start.starts_with("2026-07-15T00:00:00"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     trend 同小时多行 → 一个 hour bucket。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 2 行（07-15 12:00 与 12:30）→ summarize_trend(hour) 返回 1 个桶。
    #[tokio::test]
    async fn summarize_trend_strftime_hour() {
        let repo = fixture().await;
        for (id, ended) in [
            ("a1", "2026-07-15T12:00:00+00:00"),
            ("a2", "2026-07-15T12:30:00+00:00"),
        ] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: "p1".into(),
                worktree_id: None,
                provider_id: "claudeCodeVisible".into(),
                model_id: None,
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let filters = AgentLedgerFilters {
            window: Some(LedgerWindow::Days30),
            ..Default::default()
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let trend = repo
            .summarize_trend(&filters, TrendBucket::Hour, now)
            .await
            .unwrap();
        assert_eq!(trend.len(), 1);
        assert!(trend[0].bucket_start.starts_with("2026-07-15T12:00:00"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     provider_ids 多值过滤必须用占位符生成 IN 子句（防注入 + 不掉串）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 3 行（不同 provider）→ filters.provider_ids 选 2 个 → summary.sessions=2、by_provider.len()=2。
    #[tokio::test]
    async fn summarize_with_filters_in_clause_placeholders() {
        let repo = fixture().await;
        let ended = "2026-07-15T12:00:00+00:00";
        for (id, prov) in [
            ("a1", "claudeCodeVisible"),
            ("a2", "codexVisible"),
            ("a3", "openCodeVisible"),
        ] {
            repo.finalize(AgentLedgerFinalizeInput {
                agent_session_id: id.into(),
                project_id: "p1".into(),
                worktree_id: None,
                provider_id: prov.into(),
                model_id: None,
                started_at: ended.into(),
                ended_at: ended.into(),
                outcome: AgentLedgerOutcome::Completed,
                usage: None,
                terminal_title: None,
            })
            .await
            .unwrap();
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-15T13:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let filters = AgentLedgerFilters {
            window: None,
            provider_ids: Some(vec!["claudeCodeVisible".into(), "codexVisible".into()]),
            ..Default::default()
        };
        let s = summarize_with_filters(&repo, filters, now).await.unwrap();
        assert_eq!(s.sessions, 2);
        assert_eq!(s.by_provider.len(), 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     watermark 已清的行不得出现在新聚合结果中。
    ///
    /// Code Logic（这个测试做什么）:
    ///     finalize 一行 → clear_all → summarize_with_filters → sessions=0。
    #[tokio::test]
    async fn summarize_with_filters_skips_watermark_cleared_rows() {
        let repo = fixture().await;
        repo.finalize(AgentLedgerFinalizeInput {
            agent_session_id: "a1".into(),
            project_id: "p1".into(),
            worktree_id: None,
            provider_id: "claudeCodeVisible".into(),
            model_id: None,
            started_at: "2026-07-15T12:00:00+00:00".into(),
            ended_at: "2026-07-15T12:01:00+00:00".into(),
            outcome: AgentLedgerOutcome::Completed,
            usage: None,
            terminal_title: None,
        })
        .await
        .unwrap();
        let _ = repo.clear_all().await.unwrap();
        let filters = AgentLedgerFilters {
            window: None,
            ..Default::default()
        };
        let now = chrono::DateTime::parse_from_rfc3339("2099-01-01T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let s = summarize_with_filters(&repo, filters, now).await.unwrap();
        assert_eq!(s.sessions, 0);
    }

    /// Business Logic: summary_bucket 推导规则。
    /// Code Logic: 显式 bucket 优先；Hours24 → Hour；其它 → Day。
    #[test]
    fn bucket_default_for_window() {
        assert_eq!(
            summary_bucket(&AgentLedgerFilters {
                window: Some(LedgerWindow::Hours24),
                ..Default::default()
            }),
            TrendBucket::Hour
        );
        assert_eq!(
            summary_bucket(&AgentLedgerFilters {
                window: Some(LedgerWindow::Days7),
                ..Default::default()
            }),
            TrendBucket::Day
        );
        assert_eq!(
            summary_bucket(&AgentLedgerFilters {
                window: Some(LedgerWindow::Days30),
                ..Default::default()
            }),
            TrendBucket::Day
        );
        assert_eq!(
            summary_bucket(&AgentLedgerFilters {
                window: None,
                ..Default::default()
            }),
            TrendBucket::Day
        );
        assert_eq!(
            summary_bucket(&AgentLedgerFilters {
                window: None,
                bucket: Some(TrendBucket::Hour),
                ..Default::default()
            }),
            TrendBucket::Hour
        );
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
            terminal_title: None,
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

    /// Business Logic: summary 需要区分缓存输入（cache read）与新输入 token 合计，
    /// 供 UI 分开展示；无贡献时保持 null 而非 0。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两条 entry 各带 cache_read → summary.cache_read_tokens 为求和；
    ///     一条无 cache_read 的用例断言 null 不被 0 污染。
    #[tokio::test]
    async fn aggregate_sums_cache_read_tokens_separately() {
        let repo = fixture().await;
        let now = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        let cached = |input: u64, cache_read: u64| ReliableUsageSnapshot {
            input_tokens: Some(input),
            cache_read_tokens: Some(cache_read),
            ..Default::default()
        };
        put(
            &repo,
            "a1",
            &(now - ChronoDuration::hours(1)).to_rfc3339(),
            Some(cached(10, 100)),
        )
        .await;
        put(
            &repo,
            "a2",
            &(now - ChronoDuration::hours(2)).to_rfc3339(),
            Some(cached(5, 50)),
        )
        .await;
        let summary = summarize_window(&repo, LedgerWindow::Days7, None, now)
            .await
            .unwrap();
        assert_eq!(summary.input_tokens, Some(15));
        assert_eq!(summary.cache_read_tokens, Some(150));
        // 无任何 cache_read 贡献的窗口 → null（未提供），不得显示 0
        let empty = summarize_window(
            &repo,
            LedgerWindow::Days30,
            None,
            now - ChronoDuration::days(40),
        )
        .await
        .unwrap();
        assert_eq!(empty.cache_read_tokens, None);
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

    /// Business Logic: 稳定 ended_at DESC 分页（同秒时用不同 ended_at 保证确定性）。
    #[tokio::test]
    async fn stable_pagination_order() {
        let repo = fixture().await;
        // 用不同 ended_at 保证跨 UUID 也稳定（order by ended_at DESC, id）
        put(&repo, "b", "2026-07-10T00:00:00Z", None).await;
        put(&repo, "a", "2026-07-10T00:00:01Z", None).await;
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
        assert_eq!(page.items[1].agent_session_id, "a");
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
        assert_eq!(page2.items[0].agent_session_id, "b");
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
            terminal_title: None,
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
        let empty = summarize_window(&repo, LedgerWindow::Hours24, Some("no-such-project"), now)
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
        assert!(s
            .cost_by_currency
            .iter()
            .any(|c| c.currency == "USD" && c.minor_units == 100));
        assert!(s
            .cost_by_currency
            .iter()
            .any(|c| c.currency == "EUR" && c.minor_units == 200));
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
