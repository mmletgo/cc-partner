//! schema 初始化
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
    ///     应用启动时必须确保 Orchestrator 所需表和索引存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐条执行 CREATE TABLE/CREATE INDEX IF NOT EXISTS，兼容旧库且不使用 sqlx 迁移宏。
    pub async fn init_schema(pool: &SqlitePool) -> Result<(), AppError> {
        sqlx::query(ORCHESTRATOR_TASK_SCHEMA).execute(pool).await?;
        let added_workflow_state = ensure_column(
            pool,
            "orchestrator_tasks",
            "workflow_state",
            "TEXT NOT NULL DEFAULT 'backlog'",
        )
        .await?;
        let added_run_state = ensure_column(
            pool,
            "orchestrator_tasks",
            "run_state",
            "TEXT NOT NULL DEFAULT 'idle'",
        )
        .await?;
        ensure_column(pool, "orchestrator_tasks", "attempt_phase", "TEXT").await?;
        ensure_column(
            pool,
            "orchestrator_tasks",
            "source",
            "TEXT NOT NULL DEFAULT 'internal'",
        )
        .await?;
        ensure_column(pool, "orchestrator_tasks", "external_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_identifier", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_url", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_state", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "external_labels_json", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "runner_provider", "TEXT").await?;
        // A3：任务级冻结的 runner policy 限额
        ensure_column(pool, "orchestrator_tasks", "runner_max_turns", "INTEGER").await?;
        ensure_column(pool, "orchestrator_tasks", "runner_stall_timeout_ms", "INTEGER").await?;
        ensure_column(pool, "orchestrator_tasks", "claude_session_id", "TEXT").await?;
        // A1：统一 Agent session 引用（与 claude_session_id dual-write 一个版本）
        ensure_column(pool, "orchestrator_tasks", "agent_session_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "transcript_path", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "runtime_started_at", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_activity_at", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_runtime_event", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "last_runtime_message", "TEXT").await?;
        ensure_column(pool, "orchestrator_tasks", "prepare_claim_token", "TEXT").await?;
        ensure_column(
            pool,
            "orchestrator_tasks",
            "state_version",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        if added_workflow_state || added_run_state {
            backfill_split_state_from_legacy_status(pool).await?;
        }
        normalize_queued_split_state(pool).await?;

        for statement in [
            ORCHESTRATOR_TASKS_PROJECT_STATUS_INDEX,
            ORCHESTRATOR_TASKS_STATUS_INDEX,
            ORCHESTRATOR_PROJECT_CONFIG_SCHEMA,
            ORCHESTRATOR_EVENT_SCHEMA,
            ORCHESTRATOR_TASK_EVENTS_INDEX,
            ORCHESTRATOR_EVIDENCE_SCHEMA,
            ORCHESTRATOR_TASK_EVIDENCE_INDEX,
            ORCHESTRATOR_TASK_ATTEMPT_SCHEMA,
            ORCHESTRATOR_TASK_ATTEMPTS_SESSION_INDEX,
            ORCHESTRATOR_REMOTE_OUTBOX_SCHEMA,
            ORCHESTRATOR_REMOTE_OUTBOX_STATUS_INDEX,
            ORCHESTRATOR_REMOTE_OUTBOX_PROJECT_INDEX,
            ORCHESTRATOR_REMOTE_TASK_MIRROR_SCHEMA,
            ORCHESTRATOR_REMOTE_TASK_MIRROR_PROJECT_INDEX,
            ORCHESTRATOR_REMOTE_TASK_CREATE_REQUEST_SCHEMA,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        // A3：attempt 级不可变 policy 快照列（旧库 CREATE IF NOT EXISTS 不会补列）
        ensure_column(pool, "orchestrator_task_attempts", "runner_provider", "TEXT").await?;
        ensure_column(pool, "orchestrator_task_attempts", "agent_session_id", "TEXT").await?;
        ensure_column(pool, "orchestrator_task_attempts", "max_turns", "INTEGER").await?;
        ensure_column(
            pool,
            "orchestrator_task_attempts",
            "stall_timeout_ms",
            "INTEGER",
        )
        .await?;
        ensure_column(
            pool,
            "orchestrator_task_attempts",
            "completion_contract",
            "TEXT",
        )
        .await?;
        // 旧 outbox 表缺 state_version 时补列，默认 0。
        ensure_column(
            pool,
            "orchestrator_remote_outbox",
            "state_version",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // 旧库仅有 request_id 主键时补齐 project_id / request_fingerprint，并把 project_id 回填为任务归属。
        migrate_remote_task_create_request_scope(pool).await?;
        // A4：实验组表、唯一 winner 索引、task.experiment_id / delivery_suppressed
        OrchestratorRepo::init_experiment_schema(pool).await?;
        Ok(())
    }
}
