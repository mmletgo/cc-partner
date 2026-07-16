//! attempt/runtime
//!
//! Business Logic（为什么需要这个模块）:
//!     从 monofile 按职责拆分 OrchestratorRepo 方法，SQL 与公共签名不变。
//!
//! Code Logic（这个模块做什么）:
//!     为 `OrchestratorRepo` 提供对应 `impl` 方法块。

#![allow(dead_code)]
#![allow(unused_imports)]

use super::helpers::*;
use super::OrchestratorRepo;
use crate::error::AppError;
use crate::orchestrator::agent_adapter::types::RunnerAttemptPolicy;
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimCandidate, ClaimCasOutcome, ClaimScanCursor,
    CLAIM_CANDIDATE_LIMIT,
};
use crate::orchestrator::claude_runtime::ClaudeRuntimeSummary;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorAttemptStatus, OrchestratorCreateAction,
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorRunState,
    OrchestratorTaskAttemptRow, OrchestratorTaskRow, OrchestratorTaskStatus,
    OrchestratorWorkflowState, SplitTaskState, EVIDENCE_KIND_REPAIR_PROMPT,
};
use crate::orchestrator::outbox::{
    OrchestratorRemoteOutboxRow, RemoteMirrorTask, RemoteOutboxStatus,
};
use crate::storage::maintenance_gate::{begin_shared_write, with_shared_write_lease};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::Row;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     每一轮开发尝试都需要持久化 prompt、worktree、terminal 与不可变 runner policy 快照，
    ///     后续 completion/stall/max_turns 才能映射回具体任务轮次且不随 WORKFLOW 漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入 orchestrator_task_attempts，写入 policy 列与 agent_session_id；task_id+attempt 唯一。
    #[allow(clippy::too_many_arguments)] // attempt 行与 policy 字段需一次写入
    pub async fn add_attempt(
        &self,
        task_id: &str,
        attempt: i64,
        worktree_id: &str,
        session_id: &str,
        prompt: &str,
        policy: &RunnerAttemptPolicy,
        agent_session_id: Option<&str>,
        status: OrchestratorAttemptStatus,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        if task_id.trim().is_empty() {
            return Err(AppError::generic("任务不能为空"));
        }
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        if worktree_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 worktree"));
        }
        if session_id.trim().is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let provider = policy.provider.as_str().to_string();
        let completion = policy.completion_contract.as_str().to_string();
        let agent_session_id = agent_session_id
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_task_attempts \
                 (id, task_id, attempt, worktree_id, session_id, prompt, status, \
                  runner_provider, agent_session_id, max_turns, stall_timeout_ms, completion_contract, \
                  created_at, completed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(task_id.trim())
            .bind(attempt)
            .bind(worktree_id.trim())
            .bind(session_id.trim())
            .bind(prompt)
            .bind(status.as_str())
            .bind(&provider)
            .bind(&agent_session_id)
            .bind(policy.max_turns)
            .bind(policy.stall_timeout_ms)
            .bind(&completion)
            .bind(&now)
            .bind(Option::<&str>::None)
            .execute(&self.pool)
            .await
        })
        .await?;

        Ok(OrchestratorTaskAttemptRow {
            id,
            task_id: task_id.trim().to_string(),
            attempt,
            worktree_id: worktree_id.trim().to_string(),
            session_id: session_id.trim().to_string(),
            prompt: prompt.to_string(),
            status: status.as_str().to_string(),
            runner_provider: provider,
            agent_session_id,
            max_turns: policy.max_turns,
            stall_timeout_ms: policy.stall_timeout_ms,
            completion_contract: completion,
            created_at: now,
            completed_at: None,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单测与 max_turns/stall 对账需要按 task+attempt 读取冻结 policy。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT attempt 行；缺失 not_found；NULL policy 列由 row_to_attempt 映射 Claude 默认。
    pub async fn get_attempt(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM orchestrator_task_attempts \
             WHERE task_id = ? AND attempt = ?"
        ))
        .bind(task_id)
        .bind(attempt)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row_to_attempt(&row),
            None => Err(AppError::not_found(format!(
                "Orchestrator 任务尝试不存在: {task_id}#{attempt}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     开发 terminal 输出 completion sentinel 后，系统需要标记对应尝试已完成，避免同一轮重复触发验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id+attempt 且当前 status=running 更新 status=completed 和 completed_at；找不到运行中记录时返回 not_found。
    pub async fn mark_attempt_completed(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
                 WHERE task_id = ? AND attempt = ? AND status = ?",
            )
            .bind(OrchestratorAttemptStatus::Completed.as_str())
            .bind(&now)
            .bind(task_id)
            .bind(attempt)
            .bind(OrchestratorAttemptStatus::Running.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 运行中任务尝试不存在: {task_id}#{attempt}"
            )));
        }

        self.get_attempt(task_id, attempt).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     max_turns 溢出时要把 running attempt CAS 到 TurnLimitReached，并避免重复写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     status=running → turnLimitReached + completed_at；命中返回更新后行，未命中 not_found。
    pub async fn mark_attempt_turn_limit_reached(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
                 WHERE task_id = ? AND attempt = ? AND status = ?",
            )
            .bind(OrchestratorAttemptStatus::TurnLimitReached.as_str())
            .bind(&now)
            .bind(task_id)
            .bind(attempt)
            .bind(OrchestratorAttemptStatus::Running.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 运行中任务尝试不存在: {task_id}#{attempt}"
            )));
        }
        self.get_attempt(task_id, attempt).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     stall watchdog CAS 赢家需要把 attempt 标 stalled。
    ///
    /// Code Logic（这个函数做什么）:
    ///     running → stalled；未命中 not_found。
    pub async fn mark_attempt_stalled(
        &self,
        task_id: &str,
        attempt: i64,
    ) -> Result<OrchestratorTaskAttemptRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
                 WHERE task_id = ? AND attempt = ? AND status = ?",
            )
            .bind(OrchestratorAttemptStatus::Stalled.as_str())
            .bind(&now)
            .bind(task_id)
            .bind(attempt)
            .bind(OrchestratorAttemptStatus::Running.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 运行中任务尝试不存在: {task_id}#{attempt}"
            )));
        }
        self.get_attempt(task_id, attempt).await
    }

    /// 原子 stall：仅当 task 仍为 Running 且 attempt/session 匹配时 Blocked，并仅赢家标记 attempt stalled。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     禁止先 mark attempt=stalled 再 task CAS 失败，留下 Verifying+stalled 污染合法完成。
    ///
    /// Code Logic（这个函数做什么）:
    ///     单事务：先 `UPDATE tasks … WHERE status=Running AND attempt AND session_id`，
    ///     仅 rows_affected=1 时再 `UPDATE attempts … status=running → stalled` 并 commit；返回 Some(task) 或 None。
    pub async fn try_cas_running_attempt_to_stalled_blocked(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        blocked_reason: &str,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Blocked);
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;
        let task_result = sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, attempt_phase = ?, \
                 blocked_reason = ?, state_version = state_version + 1, updated_at = ? \
             WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
        )
        .bind(OrchestratorTaskStatus::Blocked.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind(OrchestratorAttemptPhase::Stalled.as_str())
        .bind(blocked_reason)
        .bind(&now)
        .bind(task_id)
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(attempt)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

        if task_result.rows_affected() != 1 {
            tx.rollback().await?;
            self.get_task(task_id).await?;
            return Ok(None);
        }

        // 仅 task CAS 赢家才写 attempt 终态；attempt 行缺失/已完成时不回滚 task。
        let _ = sqlx::query(
            "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
             WHERE task_id = ? AND attempt = ? AND status = ?",
        )
        .bind(OrchestratorAttemptStatus::Stalled.as_str())
        .bind(&now)
        .bind(task_id)
        .bind(attempt)
        .bind(OrchestratorAttemptStatus::Running.as_str())
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(&format!(
            "SELECT {TASK_COLUMNS} FROM orchestrator_tasks WHERE id = ?"
        ))
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Some(row_to_task(&row)?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     terminal completion hook 只能拿到 Workbench session_id，必须反查当前 running attempt 才能定位 task_id。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 session_id 和 status='running' 查询最新 attempt；缺失返回 None，存在时转换为 OrchestratorTaskAttemptRow。
    pub async fn get_running_attempt_by_session(
        &self,
        session_id: &str,
    ) -> Result<Option<OrchestratorTaskAttemptRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM orchestrator_task_attempts \
             WHERE session_id = ? AND status = ? \
             ORDER BY attempt DESC, created_at DESC LIMIT 1"
        ))
        .bind(session_id.trim())
        .bind(OrchestratorAttemptStatus::Running.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_attempt(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 创建出某一轮尝试的 worktree 和 terminal 后，需要把 active session、attempt 与冻结 policy 写回任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当任务仍为 Preparing 且 claim token 匹配时写入 branch/worktree/session/attempt/policy，
    ///     切到 Running；同时清空上一轮 Claude runtime 字段并写 agent_session_id。
    #[allow(clippy::too_many_arguments)] // running attempt CAS 需完整 claim 上下文
    pub async fn mark_task_running_attempt(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
        attempt: i64,
        prepare_claim_token: &str,
        agent_session_id: Option<&str>,
        policy: &RunnerAttemptPolicy,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let token = prepare_claim_token.trim();
        if token.is_empty() {
            return Err(AppError::generic("Preparing claim token 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let split_state = SplitTaskState::from_legacy_status(OrchestratorTaskStatus::Running);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET status = ?, workflow_state = ?, run_state = ?, runner_provider = ?, \
                     runner_max_turns = ?, runner_stall_timeout_ms = ?, \
                     branch_name = ?, worktree_id = ?, session_id = ?, attempt = ?, \
                     claude_session_id = ?, agent_session_id = ?, transcript_path = ?, \
                     runtime_started_at = ?, last_activity_at = ?, last_runtime_event = ?, last_runtime_message = ?, \
                     blocked_reason = ?, prepare_claim_token = NULL, started_at = COALESCE(started_at, ?), updated_at = ? \
                 WHERE id = ? AND status = ? AND prepare_claim_token = ?",
            )
            .bind(OrchestratorTaskStatus::Running.as_str())
            .bind(split_state.workflow_state.as_str())
            .bind(split_state.run_state.as_str())
            .bind(policy.provider.as_str())
            .bind(policy.max_turns)
            .bind(policy.stall_timeout_ms)
            .bind(branch_name)
            .bind(worktree_id)
            .bind(session_id)
            .bind(attempt)
            .bind(Option::<&str>::None)
            .bind(agent_session_id)
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(Option::<&str>::None)
            .bind(&now)
            .bind(now.clone())
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(token)
            .execute(&self.pool)
            .await
        })
        .await?;
        // CAS miss：返回当前行（通常非 Running），调用方不得继续注入 stdin。
        if result.rows_affected() != 1 {
            tracing::debug!(
                task_id = %task_id,
                attempt,
                "mark_task_running_attempt CAS miss（claim/status 不匹配）"
            );
        }
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 准备阶段需要持续反馈 PreparingWorkspace/BuildingPrompt/Streaming 等细分进度。
    ///     claim 世代隔离要求：旧 token 的 phase CAS 未命中时绝不能伪装成成功并让 runner 继续。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在任务仍处于 Preparing 且 prepare_claim_token 匹配时更新 attempt_phase 和 updated_at；
    ///     命中返回 Some(row)；token/status 不匹配返回 None。
    pub async fn update_task_attempt_phase(
        &self,
        task_id: &str,
        phase: OrchestratorAttemptPhase,
        prepare_claim_token: &str,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        let token = prepare_claim_token.trim();
        if token.is_empty() {
            return Err(AppError::generic("Preparing claim token 不能为空"));
        }
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET attempt_phase = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND prepare_claim_token = ?",
            )
            .bind(phase.as_str())
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Preparing.as_str())
            .bind(token)
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }
        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 挂账 Running 后的阶段更新可能与用户 Abort 或新 attempt 并发，旧 runner 迟到更新不能覆盖当前任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 task_id、status=Running、attempt 和 session_id 全部匹配时写入 attempt_phase；未命中返回当前任务。
    pub async fn update_active_runner_attempt_phase(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        phase: OrchestratorAttemptPhase,
    ) -> Result<OrchestratorTaskRow, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET attempt_phase = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
            )
            .bind(phase.as_str())
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Running.as_str())
            .bind(attempt)
            .bind(session_id)
            .execute(&self.pool)
            .await
        })
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude Code visible runtime 关联是可选增强；旧 runner 迟到关联不能覆盖新 attempt 的 session/transcript。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当 task_id、status=Running、attempt 和 session_id 全部匹配时写入 ClaudeRuntimeSummary；
    ///     guard 未命中时返回 None，命中时返回更新后的任务行。
    pub async fn update_active_runner_runtime_summary(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        summary: &ClaudeRuntimeSummary,
    ) -> Result<Option<OrchestratorTaskRow>, AppError> {
        if attempt <= 0 {
            return Err(AppError::generic("任务尝试轮次必须大于 0"));
        }
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(AppError::generic("任务尝试缺少 session"));
        }

        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET claude_session_id = ?, transcript_path = ?, last_activity_at = ?, \
                     last_runtime_event = ?, last_runtime_message = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
            )
            .bind(summary.claude_session_id.as_deref())
            .bind(summary.transcript_path.as_deref())
            .bind(summary.last_activity_at.as_deref())
            .bind(summary.last_runtime_event.as_deref())
            .bind(summary.last_runtime_message.as_deref())
            .bind(now)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Running.as_str())
            .bind(attempt)
            .bind(session_id)
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     兼容旧调用点：无 policy 时按 Claude 默认快照 mark running。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 mark_task_running_attempt(attempt=1, Claude default policy)。
    pub async fn mark_task_running(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
        prepare_claim_token: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        let policy = RunnerAttemptPolicy::claude_default();
        self.mark_task_running_attempt(
            task_id,
            branch_name,
            worktree_id,
            session_id,
            1,
            prepare_claim_token,
            None,
            &policy,
        )
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     OSC/Hook 活动需要刷新 task.last_activity_at 供 stall watchdog 使用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅 Running + attempt + session 匹配时更新 last_activity_at/updated_at。
    pub async fn touch_task_last_activity(
        &self,
        task_id: &str,
        attempt: i64,
        session_id: &str,
        occurred_at: &str,
    ) -> Result<bool, AppError> {
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_tasks \
                 SET last_activity_at = ?, updated_at = ? \
                 WHERE id = ? AND status = ? AND attempt = ? AND session_id = ?",
            )
            .bind(occurred_at)
            .bind(occurred_at)
            .bind(task_id)
            .bind(OrchestratorTaskStatus::Running.as_str())
            .bind(attempt)
            .bind(session_id)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
