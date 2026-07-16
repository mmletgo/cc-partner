//! 实验组唯一 winner 交付防御。
//!
//! Business Logic（为什么需要这个模块）:
//!     只有实验 winner 可进入既有 delivery；loser 永不 commit/push/merge；
//!     四层唯一性：partial unique index、CAS、delivery 前检查、per-task delivery lock。
//!
//! Code Logic（这个模块做什么）:
//!     `select_experiment_winner` CAS 选 winner；`assert_task_may_deliver` 在 deliver_task 前拦截；
//!     `start_experiment_winner_delivery` 推进 Delivering。

use crate::error::AppError;
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, ExperimentStatus, OrchestratorExperimentRow,
};
use crate::orchestrator::repo::OrchestratorRepo;

/// winner 选择结果。
///
/// Business Logic（为什么需要这个结构体）:
///     并发 reduce/approve 可能竞争，调用方需知道本次是否赢得 CAS。
///
/// Code Logic（这个结构体做什么）:
///     selected + 最新 experiment 行。
#[derive(Debug, Clone)]
pub struct SelectWinnerOutcome {
    pub selected: bool,
    pub experiment: OrchestratorExperimentRow,
}

/// Business Logic（为什么需要这个函数）:
///     人工“采用推荐”或 high+full-auto 路径必须 CAS 指定唯一 winner。
///
/// Code Logic（这个函数做什么）:
///     校验 candidate 为 ready/winner；CAS Comparing|NeedsDecision|WinnerReady → WinnerReady/Delivering；
///     写 winner/loser outcomes。
pub async fn select_experiment_winner(
    repo: &OrchestratorRepo,
    experiment_id: &str,
    winner_task_id: &str,
    reason: &str,
    confidence: ComparativeConfidence,
    advance_to_delivering: bool,
) -> Result<SelectWinnerOutcome, AppError> {
    let exp = repo.get_experiment(experiment_id).await?;
    if exp.status.is_terminal() || exp.status == ExperimentStatus::Delivering {
        return Ok(SelectWinnerOutcome {
            selected: false,
            experiment: exp,
        });
    }
    // 若已有 winner 且相同
    if exp.winner_task_id.as_deref() == Some(winner_task_id)
        && matches!(
            exp.status,
            ExperimentStatus::WinnerReady | ExperimentStatus::Delivering
        )
    {
        if advance_to_delivering && exp.status == ExperimentStatus::WinnerReady {
            if let Some(updated) = repo
                .cas_experiment_status(
                    experiment_id,
                    exp.version,
                    ExperimentStatus::WinnerReady,
                    ExperimentStatus::Delivering,
                    Some(winner_task_id),
                    Some(reason),
                    Some(confidence),
                )
                .await?
            {
                return Ok(SelectWinnerOutcome {
                    selected: true,
                    experiment: updated,
                });
            }
        }
        return Ok(SelectWinnerOutcome {
            selected: exp.winner_task_id.as_deref() == Some(winner_task_id),
            experiment: exp,
        });
    }
    if exp.winner_task_id.is_some() && exp.winner_task_id.as_deref() != Some(winner_task_id) {
        return Ok(SelectWinnerOutcome {
            selected: false,
            experiment: exp,
        });
    }

    let candidates = repo.list_experiment_candidates(experiment_id).await?;
    let winner_cand = candidates.iter().find(|c| c.task_id == winner_task_id);
    let Some(winner_cand) = winner_cand else {
        return Err(AppError::generic(format!(
            "task `{winner_task_id}` 不是 experiment `{experiment_id}` 的 candidate"
        )));
    };
    if !matches!(
        winner_cand.outcome,
        CandidateOutcome::CandidateReady | CandidateOutcome::Winner
    ) {
        return Err(AppError::generic(
            "只能选择已经通过硬门禁的 candidate 作为 winner",
        ));
    }

    let next_status = if advance_to_delivering {
        ExperimentStatus::Delivering
    } else {
        ExperimentStatus::WinnerReady
    };

    // 允许从 Comparing / NeedsDecision / WinnerReady / Running 选择
    let expected = exp.status;
    let mut outcome_updates: Vec<(String, CandidateOutcome)> = Vec::new();
    for c in &candidates {
        if c.task_id == winner_task_id {
            outcome_updates.push((c.task_id.clone(), CandidateOutcome::Winner));
        } else if matches!(
            c.outcome,
            CandidateOutcome::CandidateReady
                | CandidateOutcome::Pending
                | CandidateOutcome::Running
        ) {
            outcome_updates.push((c.task_id.clone(), CandidateOutcome::Loser));
        }
    }

    let Some(updated) = repo
        .apply_experiment_verdict(
            experiment_id,
            exp.version,
            expected,
            next_status,
            Some(winner_task_id),
            Some(reason),
            Some(confidence),
            &outcome_updates,
        )
        .await?
    else {
        let current = repo.get_experiment(experiment_id).await?;
        return Ok(SelectWinnerOutcome {
            selected: current.winner_task_id.as_deref() == Some(winner_task_id),
            experiment: current,
        });
    };

    Ok(SelectWinnerOutcome {
        selected: true,
        experiment: updated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     deliver_task 在任何 Git 操作前必须确认：非 experiment 任务放行；
///     experiment 任务仅当自己是 winner 且组为 Delivering。
///
/// Code Logic（这个函数做什么）:
///     读 task.experiment_id；无则 Ok；有则校验 winner + 组状态。
pub async fn assert_task_may_deliver(
    repo: &OrchestratorRepo,
    task_id: &str,
    experiment_id: Option<&str>,
    delivery_suppressed: bool,
) -> Result<(), AppError> {
    let Some(exp_id) = experiment_id else {
        if delivery_suppressed {
            return Err(AppError::generic(
                "delivery_suppressed 任务禁止交付（缺少 experiment_id）",
            ));
        }
        return Ok(());
    };
    let exp = repo.get_experiment(exp_id).await?;
    if exp.winner_task_id.as_deref() != Some(task_id) {
        return Err(AppError::generic(format!(
            "candidate `{task_id}` 不是 experiment `{exp_id}` 的 winner，禁止交付"
        )));
    }
    // 四层防御：组必须已 CAS 到 Delivering（WinnerReady/Completed 不得直接交付）。
    if exp.status != ExperimentStatus::Delivering {
        return Err(AppError::generic(format!(
            "experiment `{exp_id}` 状态为 {}，禁止交付（仅 Delivering 允许）",
            exp.status.as_str()
        )));
    }
    // 即便 delivery_suppressed=true，winner 在组 Delivering 时允许
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     high+full-auto 或用户批准后，组进入 Delivering 并返回 winner task id。
///
/// Code Logic（这个函数做什么）:
///     CAS WinnerReady → Delivering；返回 winner task id。
pub async fn start_experiment_winner_delivery(
    repo: &OrchestratorRepo,
    experiment_id: &str,
) -> Result<String, AppError> {
    let exp = repo.get_experiment(experiment_id).await?;
    let winner = exp
        .winner_task_id
        .clone()
        .ok_or_else(|| AppError::generic("experiment 尚无 winner，不能开始交付"))?;
    if exp.status == ExperimentStatus::Delivering {
        return Ok(winner);
    }
    if exp.status != ExperimentStatus::WinnerReady {
        return Err(AppError::generic(format!(
            "experiment 状态 {} 不能开始交付",
            exp.status.as_str()
        )));
    }
    let Some(_) = repo
        .cas_experiment_status(
            experiment_id,
            exp.version,
            ExperimentStatus::WinnerReady,
            ExperimentStatus::Delivering,
            Some(&winner),
            exp.selection_reason.as_deref(),
            exp.confidence,
        )
        .await?
    else {
        let current = repo.get_experiment(experiment_id).await?;
        if current.status == ExperimentStatus::Delivering {
            return Ok(winner);
        }
        return Err(AppError::conflict("experiment 交付 CAS 未命中"));
    };
    Ok(winner)
}

/// Business Logic（为什么需要这个函数）:
///     winner 交付成功后组进入 Completed。
///
/// Code Logic（这个函数做什么）:
///     CAS Delivering → Completed。
pub async fn mark_experiment_delivery_completed(
    repo: &OrchestratorRepo,
    experiment_id: &str,
) -> Result<(), AppError> {
    let exp = repo.get_experiment(experiment_id).await?;
    if exp.status == ExperimentStatus::Completed {
        return Ok(());
    }
    let _ = repo
        .cas_experiment_status(
            experiment_id,
            exp.version,
            ExperimentStatus::Delivering,
            ExperimentStatus::Completed,
            exp.winner_task_id.as_deref(),
            exp.selection_reason.as_deref(),
            exp.confidence,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::experiments::reducer::record_candidate_review;
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

    /// Business Logic（为什么需要这个测试）:
    ///     并发 select 只能有一个成功。
    #[tokio::test]
    async fn concurrent_reducers_can_select_only_one_winner() {
        let repo = setup().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        // 两个都 ready
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        // force both ready without auto-winner from single-ready path:
        // after task-1 ready, task-2 still pending — mark task-2 ready without full reduce
        // re-open by setting both ready then reduce
        // Actually after first ready, second pending — set second ready
        record_candidate_review(&repo, "task-2", true)
            .await
            .unwrap();
        // multi-ready → NeedsDecision
        let exp = repo.get_experiment(&exp.id).await.unwrap();
        assert_eq!(exp.status, ExperimentStatus::NeedsDecision);

        let (a, b) = tokio::join!(
            select_experiment_winner(
                &repo,
                &exp.id,
                "task-1",
                "pick 1",
                ComparativeConfidence::High,
                false
            ),
            select_experiment_winner(
                &repo,
                &exp.id,
                "task-2",
                "pick 2",
                ComparativeConfidence::High,
                false
            )
        );
        let a = a.unwrap();
        let b = b.unwrap();
        let selected = [a.selected, b.selected].into_iter().filter(|v| *v).count();
        assert_eq!(selected, 1);
        let final_exp = repo.get_experiment(&exp.id).await.unwrap();
        assert!(final_exp.winner_task_id.is_some());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     loser 直接交付必须被拒绝。
    #[tokio::test]
    async fn loser_direct_delivery_blocked() {
        let repo = setup().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", false)
            .await
            .unwrap();
        let exp = repo.get_experiment(&exp.id).await.unwrap();
        assert_eq!(exp.winner_task_id.as_deref(), Some("task-1"));
        let err = assert_task_may_deliver(&repo, "task-2", Some(&exp.id), true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("禁止交付") || err.to_string().contains("winner"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     winner 在 Delivering 时可放行。
    #[tokio::test]
    async fn winner_may_deliver_when_group_delivering() {
        let repo = setup().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", false)
            .await
            .unwrap();
        start_experiment_winner_delivery(&repo, &exp.id)
            .await
            .unwrap();
        assert_task_may_deliver(&repo, "task-1", Some(&exp.id), true)
            .await
            .unwrap();
    }

    /// Business Logic（为什么需要这个测试）:
    ///     中止 CandidateReady 后 approve 不得再选该 winner。
    #[tokio::test]
    async fn abort_then_approve_rejects_cancelled_candidate() {
        use crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal;
        use crate::orchestrator::models::OrchestratorTaskStatus;
        let repo = setup().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", true)
            .await
            .unwrap();
        // NeedsDecision + 中止 task-1
        sync_candidate_with_task_terminal(&repo, "task-1", OrchestratorTaskStatus::Aborted)
            .await
            .unwrap();
        let err = select_experiment_winner(
            &repo,
            &exp.id,
            "task-1",
            "user pick aborted",
            ComparativeConfidence::High,
            true,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("硬门禁") || err.to_string().contains("candidate"),
            "unexpected error: {err}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     并发 abort 与 Done→Delivering CAS：中止赢得 CAS 后不得交付。
    #[tokio::test]
    async fn concurrent_abort_blocks_done_to_delivering_cas() {
        use crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal;
        use crate::orchestrator::models::OrchestratorTaskStatus;
        let repo = setup().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        record_candidate_review(&repo, "task-1", true)
            .await
            .unwrap();
        record_candidate_review(&repo, "task-2", false)
            .await
            .unwrap();
        // WinnerReady：把 winner task 标为 Done（CandidateReady 路径）
        repo.set_task_status("task-1", OrchestratorTaskStatus::Done, None)
            .await
            .unwrap();
        // 模拟 approve 已 select（组 Delivering）与 abort 竞争
        start_experiment_winner_delivery(&repo, &exp.id)
            .await
            .unwrap();
        // abort 先完成
        repo.set_task_status("task-1", OrchestratorTaskStatus::Aborted, None)
            .await
            .unwrap();
        sync_candidate_with_task_terminal(&repo, "task-1", OrchestratorTaskStatus::Aborted)
            .await
            .unwrap();
        // approve 侧仅允许 Done→Delivering CAS
        let cas = repo
            .try_transition_task_status(
                "task-1",
                OrchestratorTaskStatus::Done,
                OrchestratorTaskStatus::Delivering,
                None,
            )
            .await
            .unwrap();
        assert!(cas.is_none(), "Aborted 不得被 CAS 到 Delivering");
        let task = repo.get_task("task-1").await.unwrap();
        assert_eq!(task.status, OrchestratorTaskStatus::Aborted);
    }
}
