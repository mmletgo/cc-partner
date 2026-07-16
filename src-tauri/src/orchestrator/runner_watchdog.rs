//! Runner stall watchdog（基于 active runtime last_activity）。
//!
//! Business Logic（为什么需要这个模块）:
//!     stall_timeout_ms 必须以 active Agent runtime 活动时间为准；超时后 CAS 终止并写 evidence。
//!
//! Code Logic（这个模块做什么）:
//!     扫描 stalled candidates、provider-specific liveness 对账、原子 task+attempt CAS 协调 interrupt 赢家。

use crate::error::AppError;
use crate::orchestrator::agent_adapter::types::{DEFAULT_STALL_TIMEOUT_MS, MIN_STALL_TIMEOUT_MS};
use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
use crate::state::AppState;
use chrono::{DateTime, Utc};

/// stall evidence 稳定 code。
pub const EVIDENCE_CODE_RUNNER_STALLED: &str = "runner_stalled";

/// 可能 stall 的 active runner 候选。
///
/// Business Logic（为什么需要这个结构体）:
///     scheduler 扫描后需要 task/attempt/session 与 deadline 信息做 CAS 终止。
///
/// Code Logic（这个结构体做什么）:
///     保存 task 标识、session、agent_session、超时毫秒与活动时间锚点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledRunnerCandidate {
    pub task_id: String,
    pub attempt: i64,
    pub session_id: String,
    pub agent_session_id: Option<String>,
    pub stall_timeout_ms: i64,
    pub activity_anchor_at: String,
}

/// 判断给定 now 是否超过 stall deadline。
///
/// Business Logic（为什么需要这个函数）:
///     虚拟时钟单测需要纯函数边界：恰好 deadline 才算 stall。
///
/// Code Logic（这个函数做什么）:
///     activity_anchor + stall_timeout_ms <= now。
pub fn is_stalled_at(activity_anchor_at: &str, stall_timeout_ms: i64, now: DateTime<Utc>) -> bool {
    let Ok(anchor) = DateTime::parse_from_rfc3339(activity_anchor_at) else {
        return false;
    };
    let anchor = anchor.with_timezone(&Utc);
    let timeout = stall_timeout_ms.max(MIN_STALL_TIMEOUT_MS);
    let deadline = anchor + chrono::Duration::milliseconds(timeout);
    now >= deadline
}

/// 从任务行提取 activity 锚点：last_activity_at 优先，否则 runtime_started_at。
///
/// Business Logic（为什么需要这个函数）:
///     Spec 要求无活动时使用 runtime_started_at。
///
/// Code Logic（这个函数做什么）:
///     返回非空 trim 字符串。
pub fn activity_anchor_for_task(task: &OrchestratorTaskRow) -> Option<&str> {
    task.last_activity_at
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| {
            task.runtime_started_at
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
        })
}

/// 从任务行解析 stall timeout（旧 NULL → 300000）。
///
/// Business Logic（为什么需要这个函数）:
///     任务级冻结 timeout 是 watchdog 真值。
///
/// Code Logic（这个函数做什么）:
///     Option 映射默认值。
pub fn stall_timeout_for_task(task: &OrchestratorTaskRow) -> i64 {
    task.runner_stall_timeout_ms
        .filter(|v| *v >= MIN_STALL_TIMEOUT_MS)
        .unwrap_or(DEFAULT_STALL_TIMEOUT_MS)
}

/// 列出已超时的 Running 任务候选（纯过滤，供单测注入任务列表）。
///
/// Business Logic（为什么需要这个函数）:
///     scheduler 每 10s 扫描前需要确定性候选集。
///
/// Code Logic（这个函数做什么）:
///     过滤 Running + 有 session + activity 超时。
pub fn select_stalled_runners(
    tasks: &[OrchestratorTaskRow],
    now: DateTime<Utc>,
) -> Vec<StalledRunnerCandidate> {
    let mut out = Vec::new();
    for task in tasks {
        if task.status != OrchestratorTaskStatus::Running {
            continue;
        }
        let Some(session_id) = task
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let Some(anchor) = activity_anchor_for_task(task) else {
            continue;
        };
        let stall_timeout_ms = stall_timeout_for_task(task);
        if !is_stalled_at(anchor, stall_timeout_ms, now) {
            continue;
        }
        out.push(StalledRunnerCandidate {
            task_id: task.id.clone(),
            attempt: task.attempt,
            session_id: session_id.to_string(),
            agent_session_id: task.agent_session_id.clone(),
            stall_timeout_ms,
            activity_anchor_at: anchor.to_string(),
        });
    }
    out
}

/// 对单个 stalled runner 做原子 CAS 终止：仅赢家写 attempt stalled + task Blocked + evidence。
///
/// Business Logic（为什么需要这个函数）:
///     仅 CAS 赢家可 interrupt，避免对已替换 session 发 Ctrl-C；绝不能在 completion 已进 Verifying 后污染 attempt。
///
/// Code Logic（这个函数做什么）:
///     1) 重读 task 校验 Running/attempt/session；
///     2) provider-specific liveness：读 A1 agent session last_activity_at，若更新则 dual-write task 并放弃；
///     3) 原子 try_cas_running_attempt_to_stalled_blocked（task 先于 attempt）；
///     4) 仅赢家写 evidence。返回是否成为 CAS 赢家。
pub async fn reconcile_stalled_runner(
    state: &AppState,
    candidate: &StalledRunnerCandidate,
) -> Result<bool, AppError> {
    let task = state.orchestrator_repo.get_task(&candidate.task_id).await?;
    if task.status != OrchestratorTaskStatus::Running
        || task.attempt != candidate.attempt
        || task.session_id.as_deref() != Some(candidate.session_id.as_str())
    {
        return Ok(false);
    }

    // 再做 task 级 liveness：若 last_activity 已更新则放弃
    if let Some(anchor) = activity_anchor_for_task(&task) {
        if anchor != candidate.activity_anchor_at {
            return Ok(false);
        }
    }

    // provider-specific liveness：以 A1 agent runtime last_activity 为权威活动源。
    if let Some(agent_id) = candidate
        .agent_session_id
        .as_deref()
        .or(task.agent_session_id.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if let Ok(Some(agent)) = state.workbench_agent_session_repo.get(agent_id).await {
            let agent_activity = agent.last_activity_at.trim();
            if !agent_activity.is_empty() && agent_activity != candidate.activity_anchor_at {
                // dual-write 刷新 task 锚点，避免下一 tick 再误选
                let _ = state
                    .orchestrator_repo
                    .touch_task_last_activity(
                        &candidate.task_id,
                        candidate.attempt,
                        &candidate.session_id,
                        agent_activity,
                    )
                    .await;
                // 若 agent 活动仍未过 deadline，则明确不是 stall
                if !is_stalled_at(agent_activity, candidate.stall_timeout_ms, Utc::now()) {
                    return Ok(false);
                }
                // agent 活动本身也已超时：继续 CAS（用更新后的锚点写 evidence）
            }
        }
    }

    let Some(_blocked) = state
        .orchestrator_repo
        .try_cas_running_attempt_to_stalled_blocked(
            &candidate.task_id,
            candidate.attempt,
            &candidate.session_id,
            EVIDENCE_CODE_RUNNER_STALLED,
        )
        .await?
    else {
        // task CAS 失败（可能已 Verifying）：不得再写 attempt stalled
        return Ok(false);
    };

    state
        .orchestrator_repo
        .add_evidence(
            &candidate.task_id,
            "runnerWatchdog",
            "Runner stalled",
            EVIDENCE_CODE_RUNNER_STALLED,
            &format!(
                "stall_timeout_ms={}\nactivity_anchor_at={}\nattempt={}\nsessionId={}",
                candidate.stall_timeout_ms,
                candidate.activity_anchor_at,
                candidate.attempt,
                candidate.session_id
            ),
        )
        .await?;

    Ok(true)
}

/// 扫描并 reconcile 全部 stalled runners；返回成为 CAS 赢家的候选（供 interrupt）。
///
/// Business Logic（为什么需要这个函数）:
///     scheduler tick 入口。
///
/// Code Logic（这个函数做什么）:
///     list running tasks → select → reconcile；收集 winners。
pub async fn list_and_reconcile_stalled_active_runners(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<Vec<StalledRunnerCandidate>, AppError> {
    let running = state
        .orchestrator_repo
        .list_tasks(None)
        .await?
        .into_iter()
        .filter(|t| t.status == OrchestratorTaskStatus::Running)
        .collect::<Vec<_>>();
    let candidates = select_stalled_runners(&running, now);
    let mut winners = Vec::new();
    for candidate in candidates {
        if reconcile_stalled_runner(state, &candidate).await? {
            winners.push(candidate);
        }
    }
    Ok(winners)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::OrchestratorTaskRow;

    fn running_task(anchor: &str, stall_ms: i64) -> OrchestratorTaskRow {
        let mut row = OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Running);
        row.id = "task-stall".into();
        row.session_id = Some("sess-1".into());
        row.attempt = 1;
        row.runtime_started_at = Some(anchor.into());
        row.last_activity_at = None;
        row.runner_stall_timeout_ms = Some(stall_ms);
        row
    }

    /// Business Logic（为什么需要这个测试）:
    ///     stall 必须在配置 deadline 到达时才触发，299999ms 不得误杀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用固定 anchor + now 偏移断言边界。
    #[test]
    fn runner_stalls_at_configured_deadline_only() {
        let anchor = "2026-07-15T00:00:00Z";
        let task = running_task(anchor, 300_000);
        let base = DateTime::parse_from_rfc3339(anchor)
            .unwrap()
            .with_timezone(&Utc);

        let before = base + chrono::Duration::milliseconds(299_999);
        assert!(select_stalled_runners(std::slice::from_ref(&task), before).is_empty());

        let at = base + chrono::Duration::milliseconds(300_000);
        assert_eq!(select_stalled_runners(&[task], at).len(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     last_activity_at 优先于 runtime_started_at。
    ///
    /// Code Logic（这个测试做什么）:
    ///     设置 last_activity 更晚，now 介于 started 与 activity+timeout 之间时不 stall。
    #[test]
    fn prefers_last_activity_over_runtime_started() {
        let mut task = running_task("2026-07-15T00:00:00Z", 300_000);
        task.last_activity_at = Some("2026-07-15T00:04:00Z".into());
        let now = DateTime::parse_from_rfc3339("2026-07-15T00:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // activity at 00:04 + 5min = 00:09 → 00:05 不 stall
        assert!(select_stalled_runners(&[task], now).is_empty());
    }
}
