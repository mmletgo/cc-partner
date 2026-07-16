//! 实验组 / candidate / 组 evidence / create 幂等 仓储。
//!
//! Business Logic（为什么需要这个模块）:
//!     实验组创建、唯一 winner CAS、candidate outcome 与组级 evidence 必须走同一
//!     OrchestratorRepo + maintenance gate，才能与普通 task 事务隔离一致。
//!
//! Code Logic（这个模块做什么）:
//!     为 `OrchestratorRepo` 提供 experiment CRUD、outcome CAS、evidence 与 schema SQL。

#![allow(dead_code)]
#![allow(unused_imports)]

use super::helpers::*;
use super::OrchestratorRepo;
use crate::error::AppError;
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, ExperimentStatus, OrchestratorExperimentCandidateRow,
    OrchestratorExperimentCreateRequestRow, OrchestratorExperimentEvidenceRow,
    OrchestratorExperimentRow,
};
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorTaskRow, OrchestratorTaskStatus,
};
use crate::storage::maintenance_gate::{begin_shared_write, with_shared_write_lease};
use chrono::Utc;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use uuid::Uuid;

/// 实验组表 schema。
pub const ORCHESTRATOR_EXPERIMENT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS orchestrator_experiments (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  title TEXT NOT NULL,
  goal TEXT NOT NULL,
  acceptance TEXT NOT NULL,
  status TEXT NOT NULL,
  selection_policy TEXT NOT NULL DEFAULT 'comparative',
  max_parallel INTEGER NOT NULL DEFAULT 1,
  winner_task_id TEXT,
  selection_reason TEXT,
  confidence TEXT,
  version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)";

/// candidate 链接表 schema。
pub const ORCHESTRATOR_EXPERIMENT_CANDIDATE_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_experiment_candidates (
  experiment_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  provider_id TEXT NOT NULL,
  strategy_label TEXT NOT NULL,
  outcome TEXT NOT NULL DEFAULT 'pending',
  selection_metadata_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (experiment_id, task_id)
)";

/// 同一 experiment 最多一个 winner 的 partial unique index。
pub const ORCHESTRATOR_EXPERIMENT_CANDIDATE_WINNER_INDEX: &str =
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_orchestrator_experiment_candidates_one_winner \
     ON orchestrator_experiment_candidates(experiment_id) \
     WHERE outcome = 'winner'";

pub const ORCHESTRATOR_EXPERIMENT_CANDIDATE_TASK_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_experiment_candidates_task \
     ON orchestrator_experiment_candidates(task_id)";

pub const ORCHESTRATOR_EXPERIMENT_PROJECT_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_experiments_project \
     ON orchestrator_experiments(project_id, status, updated_at)";

/// 组级 evidence schema。
pub const ORCHESTRATOR_EXPERIMENT_EVIDENCE_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_experiment_evidence (
  id TEXT PRIMARY KEY,
  experiment_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  title TEXT NOT NULL,
  summary TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL
)";

pub const ORCHESTRATOR_EXPERIMENT_EVIDENCE_INDEX: &str =
    "CREATE INDEX IF NOT EXISTS idx_orchestrator_experiment_evidence_exp \
     ON orchestrator_experiment_evidence(experiment_id, created_at)";

/// 本机/远端 create 幂等表。
pub const ORCHESTRATOR_EXPERIMENT_CREATE_REQUEST_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_experiment_create_requests (
  request_id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  experiment_id TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
)";

/// 远端 experiment outbox。
pub const ORCHESTRATOR_REMOTE_EXPERIMENT_OUTBOX_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_remote_experiment_outbox (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_project_id TEXT,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL,
  remote_experiment_id TEXT,
  last_error TEXT,
  state_version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  sent_at TEXT
)";

/// 远端 experiment mirror。
pub const ORCHESTRATOR_REMOTE_EXPERIMENT_MIRROR_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS orchestrator_remote_experiment_mirrors (
  id TEXT PRIMARY KEY,
  device_id TEXT NOT NULL,
  device_name TEXT NOT NULL,
  remote_project_id TEXT NOT NULL,
  remote_project_path TEXT NOT NULL,
  remote_experiment_id TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  last_synced_at TEXT NOT NULL,
  UNIQUE(device_id, remote_experiment_id)
)";

const EXPERIMENT_COLUMNS: &str = "id, project_id, title, goal, acceptance, status, selection_policy, \
    max_parallel, winner_task_id, selection_reason, confidence, version, created_at, updated_at";

const CANDIDATE_COLUMNS: &str = "experiment_id, task_id, ordinal, provider_id, strategy_label, \
    outcome, selection_metadata_json, created_at, updated_at";

/// Business Logic（为什么需要这个函数）:
///     仓储读取实验行时必须 fail-closed 解析 status/confidence。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 组装 OrchestratorExperimentRow。
pub(crate) fn row_to_experiment(row: &SqliteRow) -> Result<OrchestratorExperimentRow, AppError> {
    let status_text: String = row.try_get("status")?;
    let confidence_text: Option<String> = row.try_get("confidence")?;
    Ok(OrchestratorExperimentRow {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        title: row.try_get("title")?,
        goal: row.try_get("goal")?,
        acceptance: row.try_get("acceptance")?,
        status: ExperimentStatus::from_str(&status_text)?,
        selection_policy: row.try_get("selection_policy")?,
        max_parallel: row.try_get("max_parallel")?,
        winner_task_id: row.try_get("winner_task_id")?,
        selection_reason: row.try_get("selection_reason")?,
        confidence: confidence_text
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(ComparativeConfidence::from_str)
            .transpose()?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     candidate 链接读取需统一 outcome 解析。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 组装 OrchestratorExperimentCandidateRow。
pub(crate) fn row_to_experiment_candidate(
    row: &SqliteRow,
) -> Result<OrchestratorExperimentCandidateRow, AppError> {
    let outcome_text: String = row.try_get("outcome")?;
    Ok(OrchestratorExperimentCandidateRow {
        experiment_id: row.try_get("experiment_id")?,
        task_id: row.try_get("task_id")?,
        ordinal: row.try_get("ordinal")?,
        provider_id: row.try_get("provider_id")?,
        strategy_label: row.try_get("strategy_label")?,
        outcome: CandidateOutcome::from_str(&outcome_text)?,
        selection_metadata_json: row.try_get("selection_metadata_json")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Business Logic（为什么需要这个函数）:
///     组级 evidence 需要统一投影。
///
/// Code Logic（这个函数做什么）:
///     从 SqliteRow 组装 evidence 行。
pub(crate) fn row_to_experiment_evidence(
    row: &SqliteRow,
) -> Result<OrchestratorExperimentEvidenceRow, AppError> {
    Ok(OrchestratorExperimentEvidenceRow {
        id: row.try_get("id")?,
        experiment_id: row.try_get("experiment_id")?,
        kind: row.try_get("kind")?,
        title: row.try_get("title")?,
        summary: row.try_get("summary")?,
        content: row.try_get("content")?,
        created_at: row.try_get("created_at")?,
    })
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     应用启动/测试需确保 experiment 相关表与 partial unique index 存在。
    ///
    /// Code Logic（这个函数做什么）:
    ///     执行 CREATE TABLE/INDEX IF NOT EXISTS，并 ensure task 上 experiment_id/delivery_suppressed 列。
    pub async fn init_experiment_schema(
        pool: &sqlx::sqlite::SqlitePool,
    ) -> Result<(), AppError> {
        for statement in [
            ORCHESTRATOR_EXPERIMENT_SCHEMA,
            ORCHESTRATOR_EXPERIMENT_CANDIDATE_SCHEMA,
            ORCHESTRATOR_EXPERIMENT_CANDIDATE_WINNER_INDEX,
            ORCHESTRATOR_EXPERIMENT_CANDIDATE_TASK_INDEX,
            ORCHESTRATOR_EXPERIMENT_PROJECT_INDEX,
            ORCHESTRATOR_EXPERIMENT_EVIDENCE_SCHEMA,
            ORCHESTRATOR_EXPERIMENT_EVIDENCE_INDEX,
            ORCHESTRATOR_EXPERIMENT_CREATE_REQUEST_SCHEMA,
            ORCHESTRATOR_REMOTE_EXPERIMENT_OUTBOX_SCHEMA,
            ORCHESTRATOR_REMOTE_EXPERIMENT_MIRROR_SCHEMA,
        ] {
            sqlx::query(statement).execute(pool).await?;
        }
        ensure_column(pool, "orchestrator_tasks", "experiment_id", "TEXT").await?;
        ensure_column(
            pool,
            "orchestrator_tasks",
            "delivery_suppressed",
            "INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     详情/reduce/delivery 需要按 id 读取权威实验行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     SELECT 实验表；缺失返回 not_found。
    pub async fn get_experiment(
        &self,
        experiment_id: &str,
    ) -> Result<OrchestratorExperimentRow, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments WHERE id = ?"
        ))
        .bind(experiment_id)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => row_to_experiment(&row),
            None => Err(AppError::not_found(format!(
                "Orchestrator 实验不存在: {experiment_id}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目看板需要列出本机实验组。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 project 筛选，updated_at DESC。
    pub async fn list_experiments(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<OrchestratorExperimentRow>, AppError> {
        let rows = match project_id {
            Some(project_id) => {
                sqlx::query(&format!(
                    "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments \
                     WHERE project_id = ? ORDER BY updated_at DESC, id ASC"
                ))
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments \
                     ORDER BY updated_at DESC, id ASC"
                ))
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(row_to_experiment).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     组详情与 reduce 需要全部 candidate 链接。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 ordinal ASC 返回 candidate 行。
    pub async fn list_experiment_candidates(
        &self,
        experiment_id: &str,
    ) -> Result<Vec<OrchestratorExperimentCandidateRow>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {CANDIDATE_COLUMNS} FROM orchestrator_experiment_candidates \
             WHERE experiment_id = ? ORDER BY ordinal ASC, task_id ASC"
        ))
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_experiment_candidate).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证完成后需从 task_id 反查所属实验。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id 查 candidate 链接；无则 Ok(None)。
    pub async fn get_candidate_by_task(
        &self,
        task_id: &str,
    ) -> Result<Option<OrchestratorExperimentCandidateRow>, AppError> {
        let row = sqlx::query(&format!(
            "SELECT {CANDIDATE_COLUMNS} FROM orchestrator_experiment_candidates WHERE task_id = ?"
        ))
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_experiment_candidate).transpose()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Attention 只投影 NeedsDecision 实验。
    ///
    /// Code Logic（这个函数做什么）:
    ///     列出 status=needs_decision 的实验。
    pub async fn list_experiments_needing_decision(
        &self,
    ) -> Result<Vec<OrchestratorExperimentRow>, AppError> {
        let rows = sqlx::query(&format!(
            "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments \
             WHERE status = ? ORDER BY updated_at DESC, id ASC"
        ))
        .bind(ExperimentStatus::NeedsDecision.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_experiment).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     claim 事务需要统计某 experiment 当前 active candidate 数（Preparing/Running/Verifying）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     JOIN candidates 与 tasks，按 run_state 计数。
    pub async fn count_active_experiment_candidates(
        &self,
        experiment_id: &str,
    ) -> Result<i64, AppError> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS count FROM orchestrator_experiment_candidates c \
             INNER JOIN orchestrator_tasks t ON t.id = c.task_id \
             WHERE c.experiment_id = ? AND t.run_state IN (?, ?, ?, ?)",
        )
        .bind(experiment_id)
        .bind(crate::orchestrator::models::OrchestratorRunState::Preparing.as_str())
        .bind(crate::orchestrator::models::OrchestratorRunState::Running.as_str())
        .bind(crate::orchestrator::models::OrchestratorRunState::Verifying.as_str())
        .bind(crate::orchestrator::models::OrchestratorRunState::Delivering.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("count")?)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     fixture/测试与 create 服务需要插入完整实验行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT orchestrator_experiments 全字段。
    pub async fn insert_experiment(
        &self,
        row: &OrchestratorExperimentRow,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_experiments \
                 (id, project_id, title, goal, acceptance, status, selection_policy, max_parallel, \
                  winner_task_id, selection_reason, confidence, version, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.id)
            .bind(&row.project_id)
            .bind(&row.title)
            .bind(&row.goal)
            .bind(&row.acceptance)
            .bind(row.status.as_str())
            .bind(&row.selection_policy)
            .bind(row.max_parallel)
            .bind(&row.winner_task_id)
            .bind(&row.selection_reason)
            .bind(row.confidence.map(ComparativeConfidence::as_str))
            .bind(row.version)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     创建服务需要写入 candidate 链接。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT candidate 行。
    pub async fn insert_experiment_candidate(
        &self,
        row: &OrchestratorExperimentCandidateRow,
    ) -> Result<(), AppError> {
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_experiment_candidates \
                 (experiment_id, task_id, ordinal, provider_id, strategy_label, outcome, \
                  selection_metadata_json, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&row.experiment_id)
            .bind(&row.task_id)
            .bind(row.ordinal)
            .bind(&row.provider_id)
            .bind(&row.strategy_label)
            .bind(row.outcome.as_str())
            .bind(&row.selection_metadata_json)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     唯一 winner 依赖 DB partial unique + 仓储方法；测试与 reduce 共用此入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE outcome；若 unique 冲突则返回错误；rows_affected!=1 也报错。
    pub async fn set_candidate_outcome(
        &self,
        experiment_id: &str,
        task_id: &str,
        outcome: CandidateOutcome,
    ) -> Result<(), AppError> {
        let now = Utc::now().to_rfc3339();
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_experiment_candidates \
                 SET outcome = ?, updated_at = ? \
                 WHERE experiment_id = ? AND task_id = ?",
            )
            .bind(outcome.as_str())
            .bind(&now)
            .bind(experiment_id)
            .bind(task_id)
            .execute(&self.pool)
            .await
        })
        .await;
        match result {
            Ok(r) if r.rows_affected() == 1 => Ok(()),
            Ok(_) => Err(AppError::not_found(format!(
                "experiment candidate 不存在: {experiment_id}/{task_id}"
            ))),
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("UNIQUE") || msg.contains("unique") {
                    Err(AppError::conflict(format!(
                        "experiment `{experiment_id}` 已存在 winner，拒绝第二个 winner"
                    )))
                } else {
                    Err(AppError::from(err))
                }
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     比较完成后需要 CAS 推进实验状态与 winner 身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     UPDATE WHERE id AND version AND expected_status；成功后 version+1。
    pub async fn cas_experiment_status(
        &self,
        experiment_id: &str,
        expected_version: i64,
        expected_status: ExperimentStatus,
        next_status: ExperimentStatus,
        winner_task_id: Option<&str>,
        selection_reason: Option<&str>,
        confidence: Option<ComparativeConfidence>,
    ) -> Result<Option<OrchestratorExperimentRow>, AppError> {
        let now = Utc::now().to_rfc3339();
        let next_version = expected_version.saturating_add(1);
        let result = with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "UPDATE orchestrator_experiments \
                 SET status = ?, winner_task_id = ?, selection_reason = ?, confidence = ?, \
                     version = ?, updated_at = ? \
                 WHERE id = ? AND version = ? AND status = ?",
            )
            .bind(next_status.as_str())
            .bind(winner_task_id)
            .bind(selection_reason)
            .bind(confidence.map(ComparativeConfidence::as_str))
            .bind(next_version)
            .bind(&now)
            .bind(experiment_id)
            .bind(expected_version)
            .bind(expected_status.as_str())
            .execute(&self.pool)
            .await
        })
        .await?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        Ok(Some(self.get_experiment(experiment_id).await?))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     组级比较/决策需要可审计 evidence，禁止存完整 patch。
    ///
    /// Code Logic（这个函数做什么）:
    ///     INSERT experiment evidence 行。
    pub async fn add_experiment_evidence(
        &self,
        experiment_id: &str,
        kind: &str,
        title: &str,
        summary: &str,
        content: &str,
    ) -> Result<OrchestratorExperimentEvidenceRow, AppError> {
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339();
        with_shared_write_lease(&self.gate, async {
            sqlx::query(
                "INSERT INTO orchestrator_experiment_evidence \
                 (id, experiment_id, kind, title, summary, content, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(experiment_id)
            .bind(kind)
            .bind(title)
            .bind(summary)
            .bind(content)
            .bind(&created_at)
            .execute(&self.pool)
            .await
        })
        .await?;
        Ok(OrchestratorExperimentEvidenceRow {
            id,
            experiment_id: experiment_id.to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            summary: summary.to_string(),
            content: content.to_string(),
            created_at,
        })
    }

    /// Business Logic（为什么需要这个函数）:
    ///     详情页需要列出组级 evidence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 created_at ASC 返回 evidence。
    pub async fn list_experiment_evidence(
        &self,
        experiment_id: &str,
    ) -> Result<Vec<OrchestratorExperimentEvidenceRow>, AppError> {
        let rows = sqlx::query(
            "SELECT id, experiment_id, kind, title, summary, content, created_at \
             FROM orchestrator_experiment_evidence \
             WHERE experiment_id = ? ORDER BY created_at ASC, id ASC",
        )
        .bind(experiment_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_experiment_evidence).collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     create 服务在单事务内写入 experiment + candidates + tasks + fingerprint。
    ///
    /// Code Logic（这个函数做什么）:
    ///     begin_shared_write；幂等命中返回既有；否则 insert 全组并 commit。
    pub async fn create_experiment_transaction(
        &self,
        request_id: &str,
        fingerprint: &str,
        experiment: &OrchestratorExperimentRow,
        candidates: &[(OrchestratorExperimentCandidateRow, OrchestratorTaskRow)],
    ) -> Result<(OrchestratorExperimentRow, bool), AppError> {
        let (_permit, mut tx) = begin_shared_write(&self.pool, &self.gate).await?;

        if let Some(existing) = sqlx::query(
            "SELECT project_id, experiment_id, request_fingerprint \
             FROM orchestrator_experiment_create_requests WHERE request_id = ?",
        )
        .bind(request_id)
        .fetch_optional(&mut *tx)
        .await?
        {
            let mapped_project: String = existing.try_get("project_id")?;
            let mapped_exp: String = existing.try_get("experiment_id")?;
            let mapped_fp: String = existing.try_get("request_fingerprint")?;
            if mapped_project != experiment.project_id {
                return Err(AppError::conflict(format!(
                    "clientRequestId `{request_id}` 已绑定项目 `{mapped_project}`"
                )));
            }
            if mapped_fp != fingerprint {
                return Err(AppError::conflict(format!(
                    "clientRequestId `{request_id}` 已用于不同实验内容"
                )));
            }
            let row = sqlx::query(&format!(
                "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments WHERE id = ?"
            ))
            .bind(&mapped_exp)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((row_to_experiment(&row)?, false));
        }

        let now = Utc::now().to_rfc3339();
        let insert_req = sqlx::query(
            "INSERT OR IGNORE INTO orchestrator_experiment_create_requests \
             (request_id, project_id, experiment_id, request_fingerprint, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(request_id)
        .bind(&experiment.project_id)
        .bind(&experiment.id)
        .bind(fingerprint)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        if insert_req.rows_affected() != 1 {
            // 并发竞争：回读
            let existing = sqlx::query(
                "SELECT project_id, experiment_id, request_fingerprint \
                 FROM orchestrator_experiment_create_requests WHERE request_id = ?",
            )
            .bind(request_id)
            .fetch_one(&mut *tx)
            .await?;
            let mapped_fp: String = existing.try_get("request_fingerprint")?;
            let mapped_exp: String = existing.try_get("experiment_id")?;
            if mapped_fp != fingerprint {
                return Err(AppError::conflict(format!(
                    "clientRequestId `{request_id}` 已用于不同实验内容"
                )));
            }
            let row = sqlx::query(&format!(
                "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments WHERE id = ?"
            ))
            .bind(&mapped_exp)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((row_to_experiment(&row)?, false));
        }

        sqlx::query(
            "INSERT INTO orchestrator_experiments \
             (id, project_id, title, goal, acceptance, status, selection_policy, max_parallel, \
              winner_task_id, selection_reason, confidence, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&experiment.id)
        .bind(&experiment.project_id)
        .bind(&experiment.title)
        .bind(&experiment.goal)
        .bind(&experiment.acceptance)
        .bind(experiment.status.as_str())
        .bind(&experiment.selection_policy)
        .bind(experiment.max_parallel)
        .bind(&experiment.winner_task_id)
        .bind(&experiment.selection_reason)
        .bind(experiment.confidence.map(ComparativeConfidence::as_str))
        .bind(experiment.version)
        .bind(&experiment.created_at)
        .bind(&experiment.updated_at)
        .execute(&mut *tx)
        .await?;

        for (cand, task) in candidates {
            let external_labels_json = serialize_external_labels(&task.external_labels)?;
            sqlx::query(
                "INSERT INTO orchestrator_tasks \
                 (id, project_id, title, goal, acceptance_criteria, status, priority, branch_name, \
                  workflow_state, run_state, attempt_phase, source, external_id, external_identifier, \
                  external_url, external_state, external_labels_json, runner_provider, runner_max_turns, \
                  runner_stall_timeout_ms, claude_session_id, agent_session_id, transcript_path, \
                  runtime_started_at, last_activity_at, last_runtime_event, last_runtime_message, \
                  worktree_id, session_id, prepare_claim_token, blocked_reason, attempt, \
                  state_version, created_at, updated_at, started_at, finished_at, \
                  experiment_id, delivery_suppressed) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&task.id)
            .bind(&task.project_id)
            .bind(&task.title)
            .bind(&task.goal)
            .bind(&task.acceptance_criteria)
            .bind(task.status.as_str())
            .bind(task.priority)
            .bind(&task.branch_name)
            .bind(task.workflow_state.as_str())
            .bind(task.run_state.as_str())
            .bind(task.attempt_phase.map(OrchestratorAttemptPhase::as_str))
            .bind(&task.source)
            .bind(&task.external_id)
            .bind(&task.external_identifier)
            .bind(&task.external_url)
            .bind(&task.external_state)
            .bind(&external_labels_json)
            .bind(&task.runner_provider)
            .bind(task.runner_max_turns)
            .bind(task.runner_stall_timeout_ms)
            .bind(&task.claude_session_id)
            .bind(&task.agent_session_id)
            .bind(&task.transcript_path)
            .bind(&task.runtime_started_at)
            .bind(&task.last_activity_at)
            .bind(&task.last_runtime_event)
            .bind(&task.last_runtime_message)
            .bind(&task.worktree_id)
            .bind(&task.session_id)
            .bind(&task.prepare_claim_token)
            .bind(&task.blocked_reason)
            .bind(task.attempt)
            .bind(task.state_version)
            .bind(&task.created_at)
            .bind(&task.updated_at)
            .bind(&task.started_at)
            .bind(&task.finished_at)
            .bind(&task.experiment_id)
            .bind(if task.delivery_suppressed { 1i64 } else { 0i64 })
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO orchestrator_experiment_candidates \
                 (experiment_id, task_id, ordinal, provider_id, strategy_label, outcome, \
                  selection_metadata_json, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&cand.experiment_id)
            .bind(&cand.task_id)
            .bind(cand.ordinal)
            .bind(&cand.provider_id)
            .bind(&cand.strategy_label)
            .bind(cand.outcome.as_str())
            .bind(&cand.selection_metadata_json)
            .bind(&cand.created_at)
            .bind(&cand.updated_at)
            .execute(&mut *tx)
            .await?;
        }

        let row = sqlx::query(&format!(
            "SELECT {EXPERIMENT_COLUMNS} FROM orchestrator_experiments WHERE id = ?"
        ))
        .bind(&experiment.id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((row_to_experiment(&row)?, true))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试 fixture 需要快速插入含 N 个 candidate 的实验组。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入 experiment + N 个 candidate 链接（不插 task，除非调用方已插）。
    pub async fn insert_experiment_fixture(
        &self,
        candidate_count: usize,
    ) -> Result<OrchestratorExperimentRow, AppError> {
        let now = Utc::now().to_rfc3339();
        let exp_id = Uuid::new_v4().to_string();
        let exp = OrchestratorExperimentRow {
            id: exp_id.clone(),
            project_id: "project-fixture".to_string(),
            title: "fixture experiment".to_string(),
            goal: "goal".to_string(),
            acceptance: "acceptance".to_string(),
            status: ExperimentStatus::Running,
            selection_policy: "comparative".to_string(),
            max_parallel: 1,
            winner_task_id: None,
            selection_reason: None,
            confidence: None,
            version: 0,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        self.insert_experiment(&exp).await?;
        for i in 0..candidate_count {
            let task_id = format!("task-{}", i + 1);
            let mut task = OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Queued);
            task.id = task_id.clone();
            task.project_id = exp.project_id.clone();
            task.title = format!("candidate {}", i + 1);
            task.goal = exp.goal.clone();
            task.acceptance_criteria = exp.acceptance.clone();
            task.source = "experiment".to_string();
            task.experiment_id = Some(exp_id.clone());
            task.delivery_suppressed = true;
            task.created_at = now.clone();
            task.updated_at = now.clone();
            self.create_task(&task).await?;
            self.insert_experiment_candidate(&OrchestratorExperimentCandidateRow {
                experiment_id: exp_id.clone(),
                task_id,
                ordinal: (i as i64) + 1,
                provider_id: "claudeCodeVisible".to_string(),
                strategy_label: format!("s{}", i + 1),
                outcome: CandidateOutcome::Pending,
                selection_metadata_json: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await?;
        }
        self.get_experiment(&exp_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     仓储单测需要隔离内存库 + 完整 schema。
    ///
    /// Code Logic（这个函数做什么）:
    ///     初始化 orchestrator + experiment schema 并返回 repo。
    async fn experiment_repo_fixture() -> OrchestratorRepo {
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
    ///     数据库层必须拒绝同一 experiment 的两个 winner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 2 candidate fixture，设第一个 winner，第二个必须失败。
    #[tokio::test]
    async fn database_rejects_two_winners_for_one_experiment() {
        let repo = experiment_repo_fixture().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        repo.set_candidate_outcome(&exp.id, "task-1", CandidateOutcome::Winner)
            .await
            .unwrap();
        assert!(repo
            .set_candidate_outcome(&exp.id, "task-2", CandidateOutcome::Winner)
            .await
            .is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧库升级后必须补齐 experiment_id/delivery_suppressed 且普通任务默认不抑制交付。
    ///
    /// Code Logic（这个测试做什么）:
    ///     建旧任务表 → init_schema → 插入任务 → 断言列存在且 delivery_suppressed=0。
    #[tokio::test]
    async fn old_db_upgrade_adds_experiment_columns_with_safe_defaults() {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        // 最小旧任务表（无 experiment 列）
        sqlx::query(
            "CREATE TABLE orchestrator_tasks (
              id TEXT PRIMARY KEY, project_id TEXT NOT NULL, title TEXT NOT NULL,
              goal TEXT NOT NULL, acceptance_criteria TEXT NOT NULL, status TEXT NOT NULL,
              workflow_state TEXT NOT NULL DEFAULT 'backlog', run_state TEXT NOT NULL DEFAULT 'idle',
              attempt_phase TEXT, source TEXT NOT NULL DEFAULT 'internal',
              priority INTEGER NOT NULL DEFAULT 0, attempt INTEGER NOT NULL DEFAULT 0,
              state_version INTEGER NOT NULL DEFAULT 0,
              created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info('orchestrator_tasks') ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(cols.iter().any(|c| c == "experiment_id"));
        assert!(cols.iter().any(|c| c == "delivery_suppressed"));
        // winner index 存在
        let idx: Option<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='index' AND name=?",
        )
        .bind("idx_orchestrator_experiment_candidates_one_winner")
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(idx.is_some());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未知 experiment status 必须 fail-closed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接写入非法 status 再 get_experiment，断言错误。
    #[tokio::test]
    async fn unknown_experiment_status_fail_closed() {
        let repo = experiment_repo_fixture().await;
        let exp = repo.insert_experiment_fixture(2).await.unwrap();
        sqlx::query("UPDATE orchestrator_experiments SET status = 'weird' WHERE id = ?")
            .bind(&exp.id)
            .execute(repo.pool())
            .await
            .unwrap();
        assert!(repo.get_experiment(&exp.id).await.is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     枚举 as_str 必须与 DB 写入一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 WinnerReady 后读回并断言。
    #[tokio::test]
    async fn stable_enum_serialization_roundtrip() {
        let repo = experiment_repo_fixture().await;
        let mut exp = repo.insert_experiment_fixture(2).await.unwrap();
        let updated = repo
            .cas_experiment_status(
                &exp.id,
                exp.version,
                ExperimentStatus::Running,
                ExperimentStatus::WinnerReady,
                Some("task-1"),
                Some("only ready"),
                Some(ComparativeConfidence::High),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ExperimentStatus::WinnerReady);
        assert_eq!(updated.winner_task_id.as_deref(), Some("task-1"));
        assert_eq!(updated.confidence, Some(ComparativeConfidence::High));
        assert_eq!(updated.version, exp.version + 1);
        exp = updated;
        // CAS miss when version wrong
        assert!(repo
            .cas_experiment_status(
                &exp.id,
                0,
                ExperimentStatus::WinnerReady,
                ExperimentStatus::Delivering,
                Some("task-1"),
                None,
                Some(ComparativeConfidence::High),
            )
            .await
            .unwrap()
            .is_none());
    }
}
