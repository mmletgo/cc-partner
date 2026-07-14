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
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimCandidate, ClaimCasOutcome, ClaimScanCursor,
    CLAIM_CANDIDATE_LIMIT,
};
use crate::orchestrator::claude_runtime::ClaudeRuntimeSummary;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorEvidenceDto,
    OrchestratorProjectConfigDto, OrchestratorRunState, OrchestratorTaskAttemptRow,
    OrchestratorTaskRow, OrchestratorTaskStatus, OrchestratorWorkflowState, SplitTaskState,
    EVIDENCE_KIND_REPAIR_PROMPT,
};
use crate::orchestrator::outbox::{
    OrchestratorRemoteOutboxRow, RemoteMirrorTask, RemoteOutboxStatus,
};
use chrono::Utc;
use sqlx::sqlite::{SqlitePool, SqliteRow};
use sqlx::{Acquire, Row};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     每一轮 Claude Code 开发尝试都需要持久化 prompt、worktree 和可见 terminal session，
    ///     后续 completion sentinel 与任务详情才能把输出映射回具体任务轮次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 attempt id 和 created_at，插入 orchestrator_task_attempts；task_id+attempt 唯一约束由 SQLite 保证。
    pub async fn add_attempt(
        &self,
        task_id: &str,
        attempt: i64,
        worktree_id: &str,
        session_id: &str,
        prompt: &str,
        status: &str,
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
        if status.trim().is_empty() {
            return Err(AppError::generic("任务尝试状态不能为空"));
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO orchestrator_task_attempts \
             (id, task_id, attempt, worktree_id, session_id, prompt, status, created_at, completed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(task_id.trim())
        .bind(attempt)
        .bind(worktree_id.trim())
        .bind(session_id.trim())
        .bind(prompt)
        .bind(status.trim())
        .bind(&now)
        .bind(Option::<&str>::None)
        .execute(&self.pool)
        .await?;

        Ok(OrchestratorTaskAttemptRow {
            id,
            task_id: task_id.trim().to_string(),
            attempt,
            worktree_id: worktree_id.trim().to_string(),
            session_id: session_id.trim().to_string(),
            prompt: prompt.to_string(),
            status: status.trim().to_string(),
            created_at: now,
            completed_at: None,
        })
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
        let result = sqlx::query(
            "UPDATE orchestrator_task_attempts SET status = ?, completed_at = ? \
             WHERE task_id = ? AND attempt = ? AND status = ?",
        )
        .bind("completed")
        .bind(&now)
        .bind(task_id)
        .bind(attempt)
        .bind("running")
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::not_found(format!(
                "Orchestrator 运行中任务尝试不存在: {task_id}#{attempt}"
            )));
        }

        let row = sqlx::query(&format!(
            "SELECT {ATTEMPT_COLUMNS} FROM orchestrator_task_attempts \
             WHERE task_id = ? AND attempt = ?"
        ))
        .bind(task_id)
        .bind(attempt)
        .fetch_one(&self.pool)
        .await?;
        row_to_attempt(&row)
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
        .bind("running")
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| row_to_attempt(&row)).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 创建出某一轮尝试的 worktree 和 terminal 后，需要把 active session、attempt 与可见运行器类型写回任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当任务仍为 Preparing 时写入 branch/worktree/session/attempt/runner_provider，把状态切到 Running；
    ///     同时清空上一轮 Claude runtime 字段并设置 runtime_started_at；未命中时返回当前任务。
    pub async fn mark_task_running_attempt(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
        attempt: i64,
        prepare_claim_token: &str,
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
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET status = ?, workflow_state = ?, run_state = ?, runner_provider = ?, branch_name = ?, worktree_id = ?, session_id = ?, attempt = ?, \
                 claude_session_id = ?, transcript_path = ?, runtime_started_at = ?, last_activity_at = ?, last_runtime_event = ?, last_runtime_message = ?, \
                 blocked_reason = ?, prepare_claim_token = NULL, started_at = COALESCE(started_at, ?), updated_at = ? \
             WHERE id = ? AND status = ? AND prepare_claim_token = ?",
        )
        .bind(OrchestratorTaskStatus::Running.as_str())
        .bind(split_state.workflow_state.as_str())
        .bind(split_state.run_state.as_str())
        .bind("claudeCodeVisible")
        .bind(branch_name)
        .bind(worktree_id)
        .bind(session_id)
        .bind(attempt)
        .bind(Option::<&str>::None)
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
        .await?;
        self.get_task(task_id).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 准备阶段需要持续反馈 PreparingWorkspace/BuildingPrompt/Streaming 等细分进度，方便用户判断当前卡点。
    ///     claim 世代隔离要求：旧 token 的 phase CAS 未命中时绝不能伪装成成功并让 runner 继续。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅在任务仍处于 Preparing 且 prepare_claim_token 匹配时更新 attempt_phase 和 updated_at；
    ///     命中返回 Some(row)；token/status 不匹配返回 None（任务存在性仍校验）；Running 后必须改用 active runner guard helper。
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
        let result = sqlx::query(
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
        let result = sqlx::query(
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
        .await?;
        if result.rows_affected() == 1 {
            return self.get_task(task_id).await.map(Some);
        }

        self.get_task(task_id).await?;
        Ok(None)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 创建出任务 worktree 和 terminal 后，需要把可接管现场持久化到任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅当任务仍为 Preparing 时写入 branch_name/worktree_id/session_id，把状态切到 Running，
    ///     清空 blocked_reason，并首次设置 started_at；未命中时返回当前任务且不覆盖 Abort/Block。
    pub async fn mark_task_running(
        &self,
        task_id: &str,
        branch_name: &str,
        worktree_id: &str,
        session_id: &str,
        prepare_claim_token: &str,
    ) -> Result<OrchestratorTaskRow, AppError> {
        self.mark_task_running_attempt(
            task_id,
            branch_name,
            worktree_id,
            session_id,
            1,
            prepare_claim_token,
        )
        .await
    }
}
