//! 原子创建实验组与 candidate 任务。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户一次创建必须得到 2–8 个 candidate task + 组级记录，且幂等/可回滚，
//!     禁止拼 N 条普通 task 创建导致半成功。
//!
//! Code Logic（这个模块做什么）:
//!     校验候选数量与 maxParallel，计算 fingerprint，调用 repo 单事务写入。

use crate::error::AppError;
#[cfg(test)]
use crate::orchestrator::experiments::models::ExperimentCandidateSpec;
use crate::orchestrator::experiments::models::{
    CandidateOutcome, CreateExperimentRequest, ExperimentStatus, IdempotentCreateExperimentOutcome,
    OrchestratorExperimentCandidateRow, OrchestratorExperimentDto, OrchestratorExperimentRow,
    EXPERIMENT_SELECTION_POLICY_COMPARATIVE, EXPERIMENT_TASK_SOURCE,
};
use crate::orchestrator::models::{
    OrchestratorCreateAction, OrchestratorTaskRow, OrchestratorTaskStatus, SplitTaskState,
};
use crate::orchestrator::repo::OrchestratorRepo;
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// candidate 数量下限。
pub const MIN_CANDIDATES: usize = 2;
/// candidate 数量上限。
pub const MAX_CANDIDATES: usize = 8;

/// Business Logic（为什么需要这个函数）:
///     创建合同必须拒绝 1 个或 >8 个 candidate，以及非法 maxParallel。
///
/// Code Logic（这个函数做什么）:
///     校验 candidates 长度、字段非空、max_parallel ∈ [1, min(n, 设备 cap 由调用方再钳)]。
pub fn validate_create_request(
    request: &CreateExperimentRequest,
    device_max_concurrency: u32,
) -> Result<(), AppError> {
    let n = request.candidates.len();
    if !(MIN_CANDIDATES..=MAX_CANDIDATES).contains(&n) {
        return Err(AppError::generic(format!(
            "experiment candidates 数量必须在 {MIN_CANDIDATES}–{MAX_CANDIDATES}，当前为 {n}"
        )));
    }
    if request.client_request_id.trim().is_empty() {
        return Err(AppError::generic("clientRequestId 不能为空"));
    }
    if request.project_id.trim().is_empty() {
        return Err(AppError::generic("projectId 不能为空"));
    }
    if request.title.trim().is_empty() {
        return Err(AppError::generic("title 不能为空"));
    }
    if request.goal.trim().is_empty() {
        return Err(AppError::generic("goal 不能为空"));
    }
    if request.acceptance.trim().is_empty() {
        return Err(AppError::generic("acceptance 不能为空"));
    }
    let max_allowed = (n as u32).min(device_max_concurrency.max(1));
    if request.max_parallel == 0 || request.max_parallel > max_allowed {
        return Err(AppError::generic(format!(
            "maxParallel 必须在 1–{max_allowed}（候选数与设备并发上限的较小值）"
        )));
    }
    for (i, c) in request.candidates.iter().enumerate() {
        if c.provider_id.trim().is_empty() {
            return Err(AppError::generic(format!(
                "candidates[{i}].providerId 不能为空"
            )));
        }
        // fail-closed：未知 provider 不得创建 experiment candidate。
        crate::orchestrator::agent_adapter::AgentProviderId::parse(c.provider_id.trim())
            .map_err(|e| AppError::generic(format!("candidates[{i}].providerId 无效: {e}")))?;
        if c.strategy_label.trim().is_empty() {
            return Err(AppError::generic(format!(
                "candidates[{i}].strategyLabel 不能为空"
            )));
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     幂等指纹必须覆盖创建语义字段，同 key 不同内容 → conflict。
///
/// Code Logic（这个函数做什么）:
///     固定 key 顺序 JSON + SHA256 hex。
pub fn experiment_request_fingerprint(
    request: &CreateExperimentRequest,
) -> Result<String, AppError> {
    let payload = serde_json::json!({
        "project_id": request.project_id,
        "title": request.title,
        "goal": request.goal,
        "acceptance": request.acceptance,
        "max_parallel": request.max_parallel,
        "candidates": request.candidates.iter().map(|c| {
            serde_json::json!({
                "provider_id": c.provider_id,
                "strategy_label": c.strategy_label,
            })
        }).collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec(&payload)?;
    let digest = Sha256::digest(&encoded);
    Ok(format!("{digest:x}"))
}

/// Business Logic（为什么需要这个函数）:
///     命令层与 P2P route 共用同一原子创建入口。
///
/// Code Logic（这个函数做什么）:
///     校验 → fingerprint → 构造 experiment + candidate tasks → repo 事务。
pub async fn create_experiment_idempotently(
    repo: &OrchestratorRepo,
    request: &CreateExperimentRequest,
    device_max_concurrency: u32,
) -> Result<IdempotentCreateExperimentOutcome, AppError> {
    validate_create_request(request, device_max_concurrency)?;
    let fingerprint = experiment_request_fingerprint(request)?;
    let now = Utc::now().to_rfc3339();
    let experiment_id = Uuid::new_v4().to_string();
    let split = SplitTaskState::from_create_action(OrchestratorCreateAction::Todo);

    let experiment = OrchestratorExperimentRow {
        id: experiment_id.clone(),
        project_id: request.project_id.trim().to_string(),
        title: request.title.trim().to_string(),
        goal: request.goal.trim().to_string(),
        acceptance: request.acceptance.trim().to_string(),
        status: ExperimentStatus::Queued,
        selection_policy: EXPERIMENT_SELECTION_POLICY_COMPARATIVE.to_string(),
        max_parallel: request.max_parallel as i64,
        winner_task_id: None,
        selection_reason: None,
        confidence: None,
        version: 0,
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    let mut pairs: Vec<(OrchestratorExperimentCandidateRow, OrchestratorTaskRow)> =
        Vec::with_capacity(request.candidates.len());
    for (idx, spec) in request.candidates.iter().enumerate() {
        let task_id = Uuid::new_v4().to_string();
        let ordinal = (idx as i64) + 1;
        let mut task = OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Queued);
        task.id = task_id.clone();
        task.project_id = experiment.project_id.clone();
        task.title = format!("{} · {}", experiment.title, spec.strategy_label.trim());
        task.goal = experiment.goal.clone();
        task.acceptance_criteria = experiment.acceptance.clone();
        task.status = OrchestratorTaskStatus::Queued;
        task.workflow_state = split.workflow_state;
        task.run_state = split.run_state;
        task.source = EXPERIMENT_TASK_SOURCE.to_string();
        task.runner_provider = Some(spec.provider_id.trim().to_string());
        task.experiment_id = Some(experiment_id.clone());
        task.delivery_suppressed = true;
        task.created_at = now.clone();
        task.updated_at = now.clone();

        let cand = OrchestratorExperimentCandidateRow {
            experiment_id: experiment_id.clone(),
            task_id,
            ordinal,
            provider_id: spec.provider_id.trim().to_string(),
            strategy_label: spec.strategy_label.trim().to_string(),
            outcome: CandidateOutcome::Pending,
            selection_metadata_json: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        pairs.push((cand, task));
    }

    let (row, newly_created) = repo
        .create_experiment_transaction(
            request.client_request_id.trim(),
            &fingerprint,
            &experiment,
            &pairs,
        )
        .await?;

    // 首次创建写入初始 evidence
    if newly_created {
        let _ = repo
            .add_experiment_evidence(
                &row.id,
                "statusTransition",
                "experiment created",
                &format!("{} candidates queued", pairs.len()),
                &format!(
                    "maxParallel={} fingerprint={}",
                    row.max_parallel, fingerprint
                ),
            )
            .await;
    }

    let mut dto = OrchestratorExperimentDto::from(row);
    let cands = repo.list_experiment_candidates(&dto.id).await?;
    dto.candidates = Some(cands.into_iter().map(Into::into).collect());
    Ok(IdempotentCreateExperimentOutcome {
        experiment: dto,
        newly_created,
    })
}

/// Business Logic（为什么需要这个函数）:
///     测试需要从 specs 快速构造合法请求。
///
/// Code Logic（这个函数做什么）:
///     填充稳定测试字段。
#[cfg(test)]
pub fn request_with_candidates(n: usize) -> CreateExperimentRequest {
    CreateExperimentRequest {
        client_request_id: "req-1".to_string(),
        project_id: "proj-1".to_string(),
        title: "title".to_string(),
        goal: "goal".to_string(),
        acceptance: "accept".to_string(),
        max_parallel: 1,
        candidates: (0..n)
            .map(|i| ExperimentCandidateSpec {
                provider_id: "claudeCodeVisible".to_string(),
                strategy_label: format!("s{i}"),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup() -> OrchestratorRepo {
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
    ///     1 个 candidate 必须拒绝。
    #[tokio::test]
    async fn rejects_one_candidate() {
        let repo = setup().await;
        let err = create_experiment_idempotently(&repo, &request_with_candidates(1), 4)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("2–8")
                || err.to_string().contains("2-8")
                || err.to_string().contains("数量")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     9 个 candidate 必须拒绝。
    #[tokio::test]
    async fn rejects_nine_candidates() {
        let repo = setup().await;
        assert!(
            create_experiment_idempotently(&repo, &request_with_candidates(9), 8)
                .await
                .is_err()
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同 request + fingerprint 重放必须复用。
    #[tokio::test]
    async fn same_request_reuses_experiment() {
        let repo = setup().await;
        let req = request_with_candidates(3);
        let first = create_experiment_idempotently(&repo, &req, 4)
            .await
            .unwrap();
        assert!(first.newly_created);
        let second = create_experiment_idempotently(&repo, &req, 4)
            .await
            .unwrap();
        assert!(!second.newly_created);
        assert_eq!(first.experiment.id, second.experiment.id);
        let tasks = repo.list_tasks(Some("proj-1")).await.unwrap();
        assert_eq!(tasks.len(), 3);
        assert!(tasks.iter().all(|t| t.delivery_suppressed));
        assert!(tasks
            .iter()
            .all(|t| t.experiment_id.as_deref() == Some(&first.experiment.id)));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同 request id 不同 payload 必须 conflict。
    #[tokio::test]
    async fn same_request_key_different_payload_conflicts() {
        let repo = setup().await;
        let mut req = request_with_candidates(2);
        create_experiment_idempotently(&repo, &req, 4)
            .await
            .unwrap();
        req.title = "other title".to_string();
        assert!(create_experiment_idempotently(&repo, &req, 4)
            .await
            .is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     事务失败必须整组回滚（通过非法第二步模拟：先校验已在事务前完成，
    ///     这里验证 create 成功后计数一致性；真正中途失败由 DB 事务保证）。
    #[tokio::test]
    async fn successful_create_is_atomic_counts() {
        let repo = setup().await;
        let outcome = create_experiment_idempotently(&repo, &request_with_candidates(3), 4)
            .await
            .unwrap();
        assert_eq!(repo.list_experiments(None).await.unwrap().len(), 1);
        assert_eq!(
            repo.list_experiment_candidates(&outcome.experiment.id)
                .await
                .unwrap()
                .len(),
            3
        );
        assert_eq!(repo.list_tasks(Some("proj-1")).await.unwrap().len(), 3);
    }
}
