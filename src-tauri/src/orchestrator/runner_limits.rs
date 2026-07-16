//! Runner max_turns 限额（development attempt 总数）。
//!
//! Business Logic（为什么需要这个模块）:
//!     创建下一 attempt 前必须检查 next_attempt <= max_turns；达到上限不得再开 worktree/session。
//!
//! Code Logic（这个模块做什么）:
//!     `check_next_attempt` 纯函数 + evidence code 常量；溢出时由 Runner/completion 写 Blocked。

use crate::error::AppError;
use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;

/// max_turns 溢出的 evidence / event 稳定 code。
pub const EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED: &str = "runner_max_turns_exceeded";

/// 检查是否允许创建 next_attempt。
///
/// Business Logic（为什么需要这个函数）:
///     max_turns 定义为 development attempt 总数（首轮+repair），在创建 session 前 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     next_attempt 必须 >0 且 `next_attempt <= policy.max_turns`；否则返回业务错误。
pub fn check_next_attempt(policy: &RunnerAttemptPolicy, next_attempt: i64) -> Result<(), AppError> {
    if next_attempt <= 0 {
        return Err(AppError::generic("任务尝试轮次必须大于 0"));
    }
    if next_attempt > policy.max_turns {
        return Err(AppError::generic(format!(
            "{EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED}: next_attempt={next_attempt} max_turns={}",
            policy.max_turns
        )));
    }
    Ok(())
}

/// 是否为 max_turns 溢出错误。
///
/// Business Logic（为什么需要这个函数）:
///     Runner/completion 需要区分 turn limit 与其它 prepare 失败，以写正确 evidence。
///
/// Code Logic（这个函数做什么）:
///     检查错误文案是否包含稳定 code。
pub fn is_max_turns_exceeded(error: &AppError) -> bool {
    error
        .to_string()
        .contains(EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::agent_adapter::types::{
        AgentCompletionContract, AgentProviderId, RunnerAttemptPolicy,
    };

    fn policy(max_turns: i64) -> RunnerAttemptPolicy {
        RunnerAttemptPolicy::new(
            AgentProviderId::ClaudeCodeVisible,
            max_turns,
            300_000,
            AgentCompletionContract::SentinelLine,
        )
        .unwrap()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_turns=1 时不得创建 attempt 2。
    ///
    /// Code Logic（这个测试做什么）:
    ///     check_next_attempt(1) ok，(2) err。
    #[test]
    fn max_one_blocks_before_second_session_is_created() {
        let p = policy(1);
        assert!(check_next_attempt(&p, 1).is_ok());
        let err = check_next_attempt(&p, 2).unwrap_err();
        assert!(is_max_turns_exceeded(&err));
        assert!(err.to_string().contains(EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_turns=4 允许 1..=4。
    ///
    /// Code Logic（这个测试做什么）:
    ///     循环 1..=4 ok，5 err。
    #[test]
    fn max_four_allows_four_attempts() {
        let p = policy(4);
        for n in 1..=4 {
            assert!(check_next_attempt(&p, n).is_ok(), "attempt {n}");
        }
        assert!(check_next_attempt(&p, 5).is_err());
    }
}
