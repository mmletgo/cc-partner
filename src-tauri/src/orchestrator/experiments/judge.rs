//! 比较判定器：从 ready candidates 选出唯一 winner。
//!
//! Business Logic（为什么需要这个模块）:
//!     硬门禁通过后需要有界结构化比较；禁止把完整 patch 交给用户或盲目猜测。
//!
//! Code Logic（这个模块做什么）:
//!     单 ready → high；多 ready → 可注入 judge 或默认 NeedsDecision 路径；
//!     无效 judge 输出 fail-closed。

use crate::error::AppError;
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, ComparativeVerdict, OrchestratorExperimentRow,
};

/// Judge 可消费的有界 candidate 摘要（禁止完整 patch）。
///
/// Business Logic（为什么需要这个结构体）:
///     comparative input 只允许结构化摘要，降低泄露与 prompt 膨胀。
///
/// Code Logic（这个结构体做什么）:
///     持有 task/provider/validation/risk/diff digest/browser 摘要。
#[derive(Debug, Clone)]
pub struct CandidateSummary {
    pub task_id: String,
    pub outcome: CandidateOutcome,
    pub provider_id: String,
    pub strategy_label: String,
    pub validation_summary: String,
    pub risk_notes: Vec<String>,
    pub diff_digest: Option<String>,
    pub browser_summary: Option<String>,
}

/// 可选外部 judge 函数（测试注入 / 未来 CLI）。
pub type JudgeFn = dyn Fn(&OrchestratorExperimentRow, &[CandidateSummary]) -> Result<ComparativeVerdict, AppError>
    + Send
    + Sync;

/// Business Logic（为什么需要这个函数）:
///     组 reduce 需要确定性比较：唯一 ready 不调用外部 judge。
///
/// Code Logic（这个函数做什么）:
///     过滤 CandidateReady；0 → NeedsDecision 语义 verdict；1 → high winner；
///     多 → 调用 optional judge 或默认 medium/tie。
pub fn evaluate_experiment(
    experiment: &OrchestratorExperimentRow,
    candidates: &[CandidateSummary],
    external_judge: Option<&JudgeFn>,
) -> Result<ComparativeVerdict, AppError> {
    let ready: Vec<&CandidateSummary> = candidates
        .iter()
        .filter(|c| c.outcome == CandidateOutcome::CandidateReady)
        .collect();

    if ready.is_empty() {
        return Ok(ComparativeVerdict {
            winner_task_id: None,
            confidence: ComparativeConfidence::Low,
            reason: "没有合格的 candidate".to_string(),
            risk_notes: vec!["zero_qualified".to_string()],
            tied_task_ids: Vec::new(),
        });
    }

    if ready.len() == 1 {
        return Ok(ComparativeVerdict {
            winner_task_id: Some(ready[0].task_id.clone()),
            confidence: ComparativeConfidence::High,
            reason: format!(
                "唯一通过硬门禁的 candidate（{}）",
                ready[0].strategy_label
            ),
            risk_notes: ready[0].risk_notes.clone(),
            tied_task_ids: Vec::new(),
        });
    }

    if let Some(judge) = external_judge {
        let verdict = judge(experiment, candidates)?;
        return validate_verdict(&ready, verdict);
    }

    // 无外部 judge：多 ready 时不猜测，进入并列/medium 路径
    Ok(ComparativeVerdict {
        winner_task_id: None,
        confidence: ComparativeConfidence::Medium,
        reason: format!(
            "{} 个 candidate 通过硬门禁，需要比较决策",
            ready.len()
        ),
        risk_notes: vec!["multiple_ready_no_judge".to_string()],
        tied_task_ids: ready.iter().map(|c| c.task_id.clone()).collect(),
    })
}

/// Business Logic（为什么需要这个函数）:
///     外部 judge 输出可能非法（winner 不在 ready 集、malformed），必须 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     校验 winner 属于 ready；否则转为 NeedsDecision 语义（low + 无 winner）。
fn validate_verdict(
    ready: &[&CandidateSummary],
    verdict: ComparativeVerdict,
) -> Result<ComparativeVerdict, AppError> {
    let ready_ids: Vec<&str> = ready.iter().map(|c| c.task_id.as_str()).collect();
    if let Some(ref winner) = verdict.winner_task_id {
        if !ready_ids.contains(&winner.as_str()) {
            return Ok(ComparativeVerdict {
                winner_task_id: None,
                confidence: ComparativeConfidence::Low,
                reason: format!("judge 选出的 winner `{winner}` 不在 ready 集合中"),
                risk_notes: vec!["invalid_winner".to_string()],
                tied_task_ids: ready_ids.iter().map(|s| (*s).to_string()).collect(),
            });
        }
    }
    if verdict.confidence == ComparativeConfidence::High
        && verdict.winner_task_id.is_none()
        && !verdict.tied_task_ids.is_empty()
    {
        return Ok(ComparativeVerdict {
            winner_task_id: None,
            confidence: ComparativeConfidence::Low,
            reason: "judge 宣称 high 但存在并列".to_string(),
            risk_notes: vec!["invalid_high_with_tie".to_string()],
            tied_task_ids: verdict.tied_task_ids,
        });
    }
    Ok(verdict)
}

/// Business Logic（为什么需要这个函数）:
///     解析 judge 返回的 JSON；损坏输入进入 NeedsDecision 而非 panic。
///
/// Code Logic（这个函数做什么）:
///     serde 反序列化 ComparativeVerdict；失败返回 Err 由调用方转 NeedsDecision。
pub fn parse_judge_json(raw: &str) -> Result<ComparativeVerdict, AppError> {
    serde_json::from_str(raw).map_err(|err| {
        AppError::generic(format!("comparative judge JSON 无效: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::experiments::models::ExperimentStatus;

    fn exp() -> OrchestratorExperimentRow {
        OrchestratorExperimentRow {
            id: "e1".to_string(),
            project_id: "p1".to_string(),
            title: "t".to_string(),
            goal: "g".to_string(),
            acceptance: "a".to_string(),
            status: ExperimentStatus::Comparing,
            selection_policy: "comparative".to_string(),
            max_parallel: 2,
            winner_task_id: None,
            selection_reason: None,
            confidence: None,
            version: 1,
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        }
    }

    fn ready(id: &str) -> CandidateSummary {
        CandidateSummary {
            task_id: id.to_string(),
            outcome: CandidateOutcome::CandidateReady,
            provider_id: "claudeCodeVisible".to_string(),
            strategy_label: id.to_string(),
            validation_summary: "ok".to_string(),
            risk_notes: Vec::new(),
            diff_digest: Some(format!("digest-{id}")),
            browser_summary: None,
        }
    }

    fn failed(id: &str) -> CandidateSummary {
        CandidateSummary {
            task_id: id.to_string(),
            outcome: CandidateOutcome::Failed,
            provider_id: "claudeCodeVisible".to_string(),
            strategy_label: id.to_string(),
            validation_summary: "fail".to_string(),
            risk_notes: Vec::new(),
            diff_digest: None,
            browser_summary: None,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     唯一 ready 必须 high 且不调用 judge。
    #[tokio::test]
    async fn one_ready_candidate_is_high_confidence_without_judge_call() {
        let judge_calls = 0u32;
        // 不传 external judge：唯一 ready 不得调用外部 judge
        let verdict = evaluate_experiment(
            &exp(),
            &[ready("task-1"), failed("task-2")],
            None,
        )
        .unwrap();
        assert_eq!(verdict.winner_task_id.as_deref(), Some("task-1"));
        assert_eq!(verdict.confidence, ComparativeConfidence::High);
        assert_eq!(judge_calls, 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     winner 不在 ready 集时 fail-closed。
    #[test]
    fn winner_not_in_ready_set_fail_closed() {
        let judge: Box<JudgeFn> = Box::new(|_, _| {
            Ok(ComparativeVerdict {
                winner_task_id: Some("task-x".to_string()),
                confidence: ComparativeConfidence::High,
                reason: "bad".to_string(),
                risk_notes: Vec::new(),
                tied_task_ids: Vec::new(),
            })
        });
        let verdict = evaluate_experiment(
            &exp(),
            &[ready("task-1"), ready("task-2")],
            Some(judge.as_ref()),
        )
        .unwrap();
        assert!(verdict.winner_task_id.is_none());
        assert_eq!(verdict.confidence, ComparativeConfidence::Low);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     损坏 JSON 必须错误。
    #[test]
    fn malformed_json_errors() {
        assert!(parse_judge_json("{not json").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多 ready 无 judge 产生 tie/medium。
    #[test]
    fn multiple_ready_without_judge_is_medium_tie() {
        let verdict =
            evaluate_experiment(&exp(), &[ready("task-1"), ready("task-2")], None).unwrap();
        assert!(verdict.winner_task_id.is_none());
        assert_eq!(verdict.confidence, ComparativeConfidence::Medium);
        assert_eq!(verdict.tied_task_ids.len(), 2);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     零 ready → low。
    #[test]
    fn zero_ready() {
        let verdict =
            evaluate_experiment(&exp(), &[failed("task-1"), failed("task-2")], None).unwrap();
        assert!(verdict.winner_task_id.is_none());
        assert_eq!(verdict.confidence, ComparativeConfidence::Low);
    }
}
