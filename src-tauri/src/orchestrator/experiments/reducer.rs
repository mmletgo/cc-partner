//! Candidate 验证结果的组级 reduce。
//!
//! Business Logic（为什么需要这个模块）:
//!     candidate 通过硬门禁后必须停在 CandidateReady，禁止进入普通 HumanReview/delivery；
//!     当组内全部 candidate 终态后触发比较。
//!
//! Code Logic（这个模块做什么）:
//!     `record_candidate_review` 更新 outcome；`reduce_experiment` 在全部就绪后调用 judge；
//!     Comparing 卡住可恢复；winner/status 同事务写入。

use crate::error::AppError;
use crate::orchestrator::experiments::judge::{evaluate_experiment, CandidateSummary};
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, ComparativeVerdict, ExperimentStatus,
    EXPERIMENT_EVIDENCE_KIND_SELECTION_REVIEW,
};
use crate::orchestrator::repo::OrchestratorRepo;

/// Business Logic（为什么需要这个函数）:
///     验证通过的 experiment child 必须记为 CandidateReady 而非交付。
///
/// Code Logic（这个函数做什么）:
///     更新 candidate outcome；若组内全部终态则 reduce。
pub async fn record_candidate_review(
    repo: &OrchestratorRepo,
    task_id: &str,
    passed: bool,
) -> Result<(), AppError> {
    let Some(cand) = repo.get_candidate_by_task(task_id).await? else {
        return Ok(());
    };
    let outcome = if passed {
        CandidateOutcome::CandidateReady
    } else {
        CandidateOutcome::Failed
    };
    // 已是终态则跳过
    if cand.outcome.is_terminal() || cand.outcome == CandidateOutcome::CandidateReady {
        if cand.outcome == CandidateOutcome::CandidateReady && passed {
            // already ready
        } else if cand.outcome.is_terminal() {
            return Ok(());
        }
    }
    repo.set_candidate_outcome(&cand.experiment_id, task_id, outcome)
        .await?;
    // 若 candidate 进入 Running 时也更新过；这里只关心 ready/failed
    let _ = reduce_experiment(repo, &cand.experiment_id).await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     标记 candidate 开始运行（**仅**在 prepare/launch 成功后调用）。
///     若在 provider probe/worktree 之前就标 Running，prepare 失败后 experiment
///     会永远把该 candidate 视为活跃而无法比较结束。
///
/// Code Logic（这个函数做什么）:
///     Pending → Running；并 CAS 组状态到 Running。
pub async fn mark_candidate_running(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<(), AppError> {
    let Some(cand) = repo.get_candidate_by_task(task_id).await? else {
        return Ok(());
    };
    if cand.outcome == CandidateOutcome::Pending {
        repo.set_candidate_outcome(&cand.experiment_id, task_id, CandidateOutcome::Running)
            .await?;
    }
    let exp = repo.get_experiment(&cand.experiment_id).await?;
    if exp.status == ExperimentStatus::Queued {
        let _ = repo
            .cas_experiment_status(
                &exp.id,
                exp.version,
                ExperimentStatus::Queued,
                ExperimentStatus::Running,
                None,
                None,
                None,
            )
            .await?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     prepare/launch 失败或 task 补偿为 Blocked 时，必须把 candidate 从 Pending/Running
///     标为 Failed，否则组级 reduce 永远等不到全部终态。
///
/// Code Logic（这个函数做什么）:
///     非终态且非 CandidateReady → Failed；再 reduce_experiment。
pub async fn mark_candidate_failed(repo: &OrchestratorRepo, task_id: &str) -> Result<(), AppError> {
    let Some(cand) = repo.get_candidate_by_task(task_id).await? else {
        return Ok(());
    };
    if cand.outcome.is_terminal() || cand.outcome == CandidateOutcome::CandidateReady {
        return Ok(());
    }
    repo.set_candidate_outcome(&cand.experiment_id, task_id, CandidateOutcome::Failed)
        .await?;
    let _ = reduce_experiment(repo, &cand.experiment_id).await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     用户中止 experiment candidate 时必须同步 Cancelled，否则 approve 仍可按
///     CandidateReady 复活任务并进入 Git 交付。
///
/// Code Logic（这个函数做什么）:
///     Pending/Running/CandidateReady → Cancelled；已终态跳过；再 reduce_experiment。
pub async fn mark_candidate_cancelled(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<(), AppError> {
    let Some(cand) = repo.get_candidate_by_task(task_id).await? else {
        return Ok(());
    };
    if cand.outcome.is_terminal() {
        return Ok(());
    }
    // CandidateReady 虽非 is_terminal，中止后也必须 Cancelled，阻断后续 select/approve。
    repo.set_candidate_outcome(&cand.experiment_id, task_id, CandidateOutcome::Cancelled)
        .await?;
    let _ = reduce_experiment(repo, &cand.experiment_id).await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     task 进入 Blocked/Aborted 时必须与 candidate outcome + reduce 统一，
///     避免验证基础设施失败 / repair bootstrap Ok(Blocked) 让组永久 Running。
///
/// Code Logic（这个函数做什么）:
///     Blocked → mark_candidate_failed；Aborted → mark_candidate_cancelled；其它状态 noop。
pub async fn sync_candidate_with_task_terminal(
    repo: &OrchestratorRepo,
    task_id: &str,
    task_status: crate::orchestrator::models::OrchestratorTaskStatus,
) -> Result<(), AppError> {
    use crate::orchestrator::models::OrchestratorTaskStatus;
    match task_status {
        OrchestratorTaskStatus::Blocked => mark_candidate_failed(repo, task_id).await,
        OrchestratorTaskStatus::Aborted => mark_candidate_cancelled(repo, task_id).await,
        _ => Ok(()),
    }
}

/// Business Logic（为什么需要这个函数）:
///     所有 candidate 到达终态或 ready 后，组需要比较或进入 NeedsDecision；
///     卡在 Comparing 时必须可恢复，禁止永久 early-return。
///
/// Code Logic（这个函数做什么）:
///     若仍有 Pending/Running 则 noop；CAS 到 Comparing（已 Comparing 则跳过 CAS）→ judge →
///     同事务写 outcomes + 状态。
pub async fn reduce_experiment(
    repo: &OrchestratorRepo,
    experiment_id: &str,
) -> Result<Option<ComparativeVerdict>, AppError> {
    let exp = repo.get_experiment(experiment_id).await?;
    if exp.status.is_terminal()
        || matches!(
            exp.status,
            ExperimentStatus::WinnerReady
                | ExperimentStatus::Delivering
                | ExperimentStatus::NeedsDecision
        )
    {
        return Ok(None);
    }

    let candidates = repo.list_experiment_candidates(experiment_id).await?;
    let still_active = candidates.iter().any(|c| {
        matches!(
            c.outcome,
            CandidateOutcome::Pending | CandidateOutcome::Running
        )
    });
    if still_active {
        return Ok(None);
    }

    // CAS into Comparing；若已 Comparing 则复用当前行做恢复 reduce（C-M1）。
    let comparing = if exp.status == ExperimentStatus::Comparing {
        exp
    } else {
        match repo
            .cas_experiment_status(
                experiment_id,
                exp.version,
                exp.status,
                ExperimentStatus::Comparing,
                None,
                None,
                None,
            )
            .await?
        {
            Some(row) => row,
            None => {
                // 并发 CAS 未命中：若已进入 Comparing 则继续恢复，否则退出
                let current = repo.get_experiment(experiment_id).await?;
                if current.status != ExperimentStatus::Comparing {
                    return Ok(None);
                }
                current
            }
        }
    };

    let summaries: Vec<CandidateSummary> = candidates
        .iter()
        .map(|c| CandidateSummary {
            task_id: c.task_id.clone(),
            outcome: c.outcome,
            provider_id: c.provider_id.clone(),
            strategy_label: c.strategy_label.clone(),
            validation_summary: match c.outcome {
                CandidateOutcome::CandidateReady => "passed hard gate".to_string(),
                CandidateOutcome::Failed => "failed hard gate".to_string(),
                CandidateOutcome::Cancelled => "cancelled".to_string(),
                other => other.as_str().to_string(),
            },
            risk_notes: Vec::new(),
            diff_digest: None,
            browser_summary: None,
        })
        .collect();

    let verdict = evaluate_experiment(&comparing, &summaries, None)?;

    let content = serde_json::to_string(&verdict).unwrap_or_default();
    if let Err(err) = repo
        .add_experiment_evidence(
            experiment_id,
            EXPERIMENT_EVIDENCE_KIND_SELECTION_REVIEW,
            "selection review",
            &verdict.reason,
            &content,
        )
        .await
    {
        tracing::warn!(
            experiment_id = %experiment_id,
            "experiment selection evidence 写入失败: {err}"
        );
    }

    let ready_count = summaries
        .iter()
        .filter(|s| s.outcome == CandidateOutcome::CandidateReady)
        .count();

    let (next_status, winner_for_status, outcome_updates) = if ready_count == 0 {
        (ExperimentStatus::NeedsDecision, None, Vec::new())
    } else if verdict.winner_task_id.is_some()
        && verdict.confidence == ComparativeConfidence::High
        && verdict.tied_task_ids.is_empty()
    {
        let winner = verdict.winner_task_id.clone();
        let mut updates = Vec::new();
        if let Some(ref w) = winner {
            for c in &candidates {
                if c.task_id == *w {
                    updates.push((c.task_id.clone(), CandidateOutcome::Winner));
                } else if c.outcome == CandidateOutcome::CandidateReady
                    || (!c.outcome.is_terminal() && c.outcome != CandidateOutcome::Winner)
                {
                    updates.push((c.task_id.clone(), CandidateOutcome::Loser));
                }
            }
        }
        (ExperimentStatus::WinnerReady, winner, updates)
    } else {
        // tie / medium / low / judge ambiguity
        (
            ExperimentStatus::NeedsDecision,
            verdict.winner_task_id.clone(),
            Vec::new(),
        )
    };

    match repo
        .apply_experiment_verdict(
            experiment_id,
            comparing.version,
            ExperimentStatus::Comparing,
            next_status,
            winner_for_status.as_deref(),
            Some(&verdict.reason),
            Some(verdict.confidence),
            &outcome_updates,
        )
        .await
    {
        Ok(Some(_)) => Ok(Some(verdict)),
        Ok(None) => {
            // 并发 CAS 未命中：读回状态；若已推进到目标态则仍返回 verdict
            let current = repo.get_experiment(experiment_id).await?;
            if current.status == next_status
                || matches!(
                    current.status,
                    ExperimentStatus::WinnerReady
                        | ExperimentStatus::NeedsDecision
                        | ExperimentStatus::Delivering
                        | ExperimentStatus::Completed
                )
            {
                Ok(Some(verdict))
            } else {
                Err(AppError::conflict(format!(
                    "experiment `{experiment_id}` Comparing 推进 CAS 未命中，状态仍为 {}",
                    current.status.as_str()
                )))
            }
        }
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> OrchestratorRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        OrchestratorRepo::new(pool)
    }

    async fn fixture_two(repo: &OrchestratorRepo) -> String {
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        exp.id
    }

    /// Business Logic（为什么需要这个测试）:
    ///     通过的 candidate 必须停在 CandidateReady 且不触发 delivery。
    #[tokio::test]
    async fn passed_candidate_stops_at_candidate_ready() {
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        // 标记 task-2 仍 pending，task-1 ready 后不应完成 reduce 到 WinnerReady（等全部终态）
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        let c = repo.get_candidate_by_task("task-1").await.unwrap().unwrap();
        assert_eq!(c.outcome, CandidateOutcome::CandidateReady);
        // 组仍 Running/Queued（task-2 pending）
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        assert!(!matches!(
            exp.status,
            ExperimentStatus::Delivering | ExperimentStatus::Completed
        ));
        assert!(exp.winner_task_id.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     一个 ready + 一个 failed 应自动 high confidence winner。
    #[tokio::test]
    async fn one_ready_one_failed_selects_winner() {
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", false)
            .await
            .unwrap();
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        assert_eq!(exp.status, ExperimentStatus::WinnerReady);
        assert_eq!(exp.winner_task_id.as_deref(), Some("task-1"));
        assert_eq!(exp.confidence, Some(ComparativeConfidence::High));
        let w = repo.get_candidate_by_task("task-1").await.unwrap().unwrap();
        assert_eq!(w.outcome, CandidateOutcome::Winner);
        let l = repo.get_candidate_by_task("task-2").await.unwrap().unwrap();
        assert!(matches!(
            l.outcome,
            CandidateOutcome::Failed | CandidateOutcome::Loser
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     零合格 candidate 进入 NeedsDecision。
    #[tokio::test]
    async fn zero_ready_needs_decision() {
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        record_candidate_review(&repo, "task-1", false)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", false)
            .await
            .unwrap();
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        assert_eq!(exp.status, ExperimentStatus::NeedsDecision);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     卡在 Comparing 后再次 reduce 必须能推进到终态。
    #[tokio::test]
    async fn reduce_recovers_from_stuck_comparing() {
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        repo.set_candidate_outcome(&exp_id, "task-1", CandidateOutcome::CandidateReady)
            .await
            .unwrap();
        repo.set_candidate_outcome(&exp_id, "task-2", CandidateOutcome::Failed)
            .await
            .unwrap();
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        // 强制写入 Comparing 模拟中断
        let comparing = repo
            .cas_experiment_status(
                &exp_id,
                exp.version,
                exp.status,
                ExperimentStatus::Comparing,
                None,
                None,
                None,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(comparing.status, ExperimentStatus::Comparing);
        reduce_experiment(&repo, &exp_id).await.unwrap();
        let final_exp = repo.get_experiment(&exp_id).await.unwrap();
        assert_eq!(final_exp.status, ExperimentStatus::WinnerReady);
        assert_eq!(final_exp.winner_task_id.as_deref(), Some("task-1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     验证基础设施失败把 task 置 Blocked 时，candidate 必须 Failed 并让组收敛。
    #[tokio::test]
    async fn validation_infra_blocked_converges_group() {
        use crate::orchestrator::models::OrchestratorTaskStatus;
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        // 模拟两 candidate 都在 Running，然后验证 infra 失败 → Blocked
        repo.set_candidate_outcome(&exp_id, "task-1", CandidateOutcome::Running)
            .await
            .unwrap();
        repo.set_candidate_outcome(&exp_id, "task-2", CandidateOutcome::Running)
            .await
            .unwrap();
        sync_candidate_with_task_terminal(&repo, "task-1", OrchestratorTaskStatus::Blocked)
            .await
            .unwrap();
        sync_candidate_with_task_terminal(&repo, "task-2", OrchestratorTaskStatus::Blocked)
            .await
            .unwrap();
        let c1 = repo.get_candidate_by_task("task-1").await.unwrap().unwrap();
        let c2 = repo.get_candidate_by_task("task-2").await.unwrap().unwrap();
        assert_eq!(c1.outcome, CandidateOutcome::Failed);
        assert_eq!(c2.outcome, CandidateOutcome::Failed);
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        assert_eq!(exp.status, ExperimentStatus::NeedsDecision);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     repair bootstrap 失败（Ok(Blocked)）时，一个 Failed + 一个 Ready 必须收敛出 winner。
    #[tokio::test]
    async fn repair_bootstrap_blocked_allows_winner_convergence() {
        use crate::orchestrator::models::OrchestratorTaskStatus;
        let repo = setup().await;
        let exp_id = fixture_two(&repo).await;
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        // task-2 仍 Running（模拟 repair 进行中），然后 bootstrap 失败 → Blocked
        repo.set_candidate_outcome(&exp_id, "task-2", CandidateOutcome::Running)
            .await
            .unwrap();
        sync_candidate_with_task_terminal(&repo, "task-2", OrchestratorTaskStatus::Blocked)
            .await
            .unwrap();
        let c2 = repo.get_candidate_by_task("task-2").await.unwrap().unwrap();
        assert_eq!(c2.outcome, CandidateOutcome::Failed);
        let exp = repo.get_experiment(&exp_id).await.unwrap();
        assert_eq!(exp.status, ExperimentStatus::WinnerReady);
        assert_eq!(exp.winner_task_id.as_deref(), Some("task-1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     中止 CandidateReady 任务必须把 outcome 置 Cancelled，阻断后续 approve。
    #[tokio::test]
    async fn abort_cancels_candidate_ready_outcome() {
        use crate::orchestrator::models::OrchestratorTaskStatus;
        let repo = setup().await;
        let _exp_id = fixture_two(&repo).await;
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        let before = repo.get_candidate_by_task("task-1").await.unwrap().unwrap();
        assert_eq!(before.outcome, CandidateOutcome::CandidateReady);
        sync_candidate_with_task_terminal(&repo, "task-1", OrchestratorTaskStatus::Aborted)
            .await
            .unwrap();
        let after = repo.get_candidate_by_task("task-1").await.unwrap().unwrap();
        assert_eq!(after.outcome, CandidateOutcome::Cancelled);
    }
}
