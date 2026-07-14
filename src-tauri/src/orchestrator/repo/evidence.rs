//! 事件与 evidence
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
use crate::storage::maintenance_gate::with_shared_write_lease;
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
use sqlx::Row;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     后续调度器需要记录任务生命周期事件，便于页面展示和问题排查。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成事件 id 和 UTC 时间戳，向 orchestrator_task_events 追加一行。
    pub async fn add_event(
        &self,
        task_id: &str,
        kind: &str,
        message: &str,
        payload_json: Option<&str>,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_task_events \
                 (id, task_id, kind, message, payload_json, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(task_id)
            .bind(kind)
            .bind(message)
            .bind(payload_json)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await
        }).await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后续验证流程需要保存命令输出、文件摘要等交付证据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成证据 id 和 UTC 时间戳，向 orchestrator_task_evidence 追加一行。
    pub async fn add_evidence(
        &self,
        task_id: &str,
        kind: &str,
        title: &str,
        summary: &str,
        content: &str,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_task_evidence \
                 (id, task_id, kind, title, summary, content, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(task_id)
            .bind(kind)
            .bind(title)
            .bind(summary)
            .bind(content)
            .bind(Utc::now().to_rfc3339())
            .execute(&self.pool)
            .await
        }).await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 详情页需要按任务读取验证输出和交付证据，且不能混入其它任务的记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id 查询 orchestrator_task_evidence，并按 created_at ASC、id ASC 稳定排序后转换 DTO。
    pub async fn list_evidence(
        &self,
        task_id: &str,
    ) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVIDENCE_COLUMNS} FROM orchestrator_task_evidence \
             WHERE task_id = ? ORDER BY created_at ASC, id ASC"
        ))
        .bind(task_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_evidence).collect()
    }
}
