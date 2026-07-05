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
        (OrchestratorTaskStatus::Verifying, TaskStageOutcome::VerificationFailed) => {
            OrchestratorTaskStatus::Preparing
        }
        (OrchestratorTaskStatus::Verifying, TaskStageOutcome::VerificationInfraFailed) => {
            OrchestratorTaskStatus::Blocked
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

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 的正常任务执行链路必须能从草稿一路推进到完成，供后续调度器复用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐步输入 happy path 的阶段 outcome，并断言每一步状态都符合既定状态机。
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

    /// Business Logic（为什么需要这个函数）:
    ///     任务在任意执行阶段都可能遇到无法继续的问题，需要统一进入 Blocked 供人工处理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 Running 和 Delivering 两个阶段输入 Block outcome，断言状态机都返回 Blocked。
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

    /// Business Logic（为什么需要这个函数）:
    ///     用户或系统中止任务应从任意阶段进入 Aborted；无变化事件则不应扰动当前状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分别验证 Abort 的全局终止语义，以及 Noop 对当前状态的保持语义。
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

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 判定代码未满足任务目标时，任务应回到 Preparing，随后复用同一 worktree 启动修复 runner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 Verifying 输入 VerificationFailed，断言状态机返回 Preparing。
    #[test]
    fn verification_failed_returns_to_preparing() {
        assert_eq!(
            next_status(
                OrchestratorTaskStatus::Verifying,
                TaskStageOutcome::VerificationFailed
            ),
            OrchestratorTaskStatus::Preparing
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 基础设施失败无法自动修复，任务应阻塞等待用户处理 CLI、JSON 或 diff 读取问题。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 Verifying 输入 VerificationInfraFailed，断言状态机返回 Blocked。
    #[test]
    fn verification_infra_failed_blocks_task() {
        assert_eq!(
            next_status(
                OrchestratorTaskStatus::Verifying,
                TaskStageOutcome::VerificationInfraFailed
            ),
            OrchestratorTaskStatus::Blocked
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 判定通过仍应保持原有交付路径，进入 Delivering 后由 delivery pipeline 处理 Git 副作用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 Verifying 输入 VerificationPassed，断言状态机返回 Delivering。
    #[test]
    fn verification_passed_enters_delivering() {
        assert_eq!(
            next_status(
                OrchestratorTaskStatus::Verifying,
                TaskStageOutcome::VerificationPassed
            ),
            OrchestratorTaskStatus::Delivering
        );
    }
}
