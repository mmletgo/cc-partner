//! Orchestrator task state transitions.

#![allow(dead_code)]

use crate::orchestrator::models::{OrchestratorTaskStatus, TaskStageOutcome};

/// Business Logic（为什么需要这个函数）:
///     自动编排任务只能按明确阶段推进，阻塞和中止可以从任意阶段进入终态。
///
/// Code Logic（这个函数做什么）:
///     接收当前任务状态和阶段输出，按白名单转换状态；非法转换保持原状态。
pub fn next_status(
    current: OrchestratorTaskStatus,
    outcome: TaskStageOutcome,
) -> OrchestratorTaskStatus {
    match (current, outcome) {
        (OrchestratorTaskStatus::Draft, TaskStageOutcome::Queue) => OrchestratorTaskStatus::Queued,
        (OrchestratorTaskStatus::Queued, TaskStageOutcome::StartPreparing) => {
            OrchestratorTaskStatus::Preparing
        }
        (OrchestratorTaskStatus::Preparing, TaskStageOutcome::RunnerReady) => {
            OrchestratorTaskStatus::Running
        }
        (OrchestratorTaskStatus::Running, TaskStageOutcome::AgentFinished) => {
            OrchestratorTaskStatus::Verifying
        }
        (OrchestratorTaskStatus::Verifying, TaskStageOutcome::VerificationPassed) => {
            OrchestratorTaskStatus::Delivering
        }
        (OrchestratorTaskStatus::Delivering, TaskStageOutcome::DeliveryPassed) => {
            OrchestratorTaskStatus::Done
        }
        (_, TaskStageOutcome::Block) => OrchestratorTaskStatus::Blocked,
        (_, TaskStageOutcome::Abort) => OrchestratorTaskStatus::Aborted,
        (status, TaskStageOutcome::Noop) => status,
        (status, _) => status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_reaches_done() {
        let status = next_status(OrchestratorTaskStatus::Draft, TaskStageOutcome::Queue);
        assert_eq!(status, OrchestratorTaskStatus::Queued);
        let status = next_status(status, TaskStageOutcome::StartPreparing);
        assert_eq!(status, OrchestratorTaskStatus::Preparing);
        let status = next_status(status, TaskStageOutcome::RunnerReady);
        assert_eq!(status, OrchestratorTaskStatus::Running);
        let status = next_status(status, TaskStageOutcome::AgentFinished);
        assert_eq!(status, OrchestratorTaskStatus::Verifying);
        let status = next_status(status, TaskStageOutcome::VerificationPassed);
        assert_eq!(status, OrchestratorTaskStatus::Delivering);
        let status = next_status(status, TaskStageOutcome::DeliveryPassed);
        assert_eq!(status, OrchestratorTaskStatus::Done);
    }

    #[test]
    fn any_status_can_block() {
        assert_eq!(
            next_status(OrchestratorTaskStatus::Running, TaskStageOutcome::Block),
            OrchestratorTaskStatus::Blocked
        );
        assert_eq!(
            next_status(OrchestratorTaskStatus::Delivering, TaskStageOutcome::Block),
            OrchestratorTaskStatus::Blocked
        );
    }

    #[test]
    fn abort_and_noop_are_global_outcomes() {
        assert_eq!(
            next_status(OrchestratorTaskStatus::Preparing, TaskStageOutcome::Abort),
            OrchestratorTaskStatus::Aborted
        );
        assert_eq!(
            next_status(OrchestratorTaskStatus::Running, TaskStageOutcome::Noop),
            OrchestratorTaskStatus::Running
        );
    }
}
