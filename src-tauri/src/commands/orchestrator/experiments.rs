//! Orchestrator 实验组 Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面/本机需要创建、列表、详情、批准 winner、取消与降级 quiesce 实验组。
//!
//! Code Logic（这个模块做什么）:
//!     封装 create_experiment_idempotently 与 delivery/select helpers。

use crate::error::AppError;
use crate::orchestrator::experiments::create::create_experiment_idempotently;
use crate::orchestrator::experiments::delivery::{
    mark_experiment_delivery_completed, select_experiment_winner, start_experiment_winner_delivery,
};
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, CreateExperimentRequest, ExperimentStatus,
    OrchestratorExperimentDto,
};
use crate::orchestrator::models::OrchestratorTaskStatus;
use crate::state::AppState;
use tauri::State;

use super::common::{auto_delivery_enabled, run_delivery_for_task};

/// Business Logic（为什么需要这个函数）:
///     用户创建 2–8 candidate 实验组。
///
/// Code Logic（这个函数做什么）:
///     校验本机项目后原子创建；可选 best-effort dispatch。
#[tauri::command]
pub async fn create_orchestrator_experiment(
    state: State<'_, AppState>,
    request: CreateExperimentRequest,
) -> Result<OrchestratorExperimentDto, AppError> {
    create_orchestrator_experiment_for_state(&state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     HTTP/route 与 Tauri 共用创建入口。
///
/// Code Logic（这个函数做什么）:
///     device max concurrency 来自全局 config；create 后 best-effort dispatch。
pub async fn create_orchestrator_experiment_for_state(
    state: &AppState,
    request: CreateExperimentRequest,
) -> Result<OrchestratorExperimentDto, AppError> {
    let device_cap = {
        let config = state.config.read().expect("config 读锁中毒");
        config.orchestrator.max_concurrent_tasks.max(1) as u32
    };
    let outcome =
        create_experiment_idempotently(state.orchestrator_repo.as_ref(), &request, device_cap)
            .await?;
    if outcome.newly_created {
        let _ = super::common::dispatch_orchestrator_best_effort(state).await;
    }
    Ok(outcome.experiment)
}

/// Business Logic（为什么需要这个函数）:
///     看板需要列出项目实验组。
///
/// Code Logic（这个函数做什么）:
///     list_experiments + 附带 candidates。
#[tauri::command]
pub async fn list_orchestrator_experiments(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    list_orchestrator_experiments_for_state(&state, project_id.as_deref()).await
}

/// Business Logic（为什么需要这个函数）:
///     共享列表入口。
///
/// Code Logic（这个函数做什么）:
///     投影 DTO 并填充 candidates。
pub async fn list_orchestrator_experiments_for_state(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    let rows = state
        .orchestrator_repo
        .list_experiments(project_id)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut dto = OrchestratorExperimentDto::from(row);
        let cands = state
            .orchestrator_repo
            .list_experiment_candidates(&dto.id)
            .await?;
        dto.candidates = Some(cands.into_iter().map(Into::into).collect());
        out.push(dto);
    }
    Ok(out)
}

/// Business Logic（为什么需要这个函数）:
///     详情页需要完整实验 + candidates。
///
/// Code Logic（这个函数做什么）:
///     get_experiment + list candidates。
#[tauri::command]
pub async fn get_orchestrator_experiment(
    state: State<'_, AppState>,
    experiment_id: String,
) -> Result<OrchestratorExperimentDto, AppError> {
    get_orchestrator_experiment_for_state(&state, &experiment_id).await
}

/// Business Logic（为什么需要这个函数）:
///     共享详情入口。
///
/// Code Logic（这个函数做什么）:
///     get + candidates。
pub async fn get_orchestrator_experiment_for_state(
    state: &AppState,
    experiment_id: &str,
) -> Result<OrchestratorExperimentDto, AppError> {
    let row = state.orchestrator_repo.get_experiment(experiment_id).await?;
    let mut dto = OrchestratorExperimentDto::from(row);
    let cands = state
        .orchestrator_repo
        .list_experiment_candidates(experiment_id)
        .await?;
    dto.candidates = Some(cands.into_iter().map(Into::into).collect());
    Ok(dto)
}

/// 批准/采用推荐 winner。
///
/// Business Logic（为什么需要这个函数）:
///     NeedsDecision 或 WinnerReady（非 full-auto）时用户可采用推荐或选择另一 ready candidate。
///
/// Code Logic（这个函数做什么）:
///     select_experiment_winner → 若 full-auto 或 force_deliver 则 start delivery。
#[tauri::command]
pub async fn approve_orchestrator_experiment_winner(
    state: State<'_, AppState>,
    experiment_id: String,
    winner_task_id: String,
    reason: Option<String>,
) -> Result<OrchestratorExperimentDto, AppError> {
    approve_orchestrator_experiment_winner_for_state(
        &state,
        &experiment_id,
        &winner_task_id,
        reason.as_deref(),
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     共享批准入口。
///
/// Code Logic（这个函数做什么）:
///     CAS 选 winner；auto deliver 时启动交付。
pub async fn approve_orchestrator_experiment_winner_for_state(
    state: &AppState,
    experiment_id: &str,
    winner_task_id: &str,
    reason: Option<&str>,
) -> Result<OrchestratorExperimentDto, AppError> {
    let reason = reason.unwrap_or("用户采用推荐 winner");
    let should_auto = {
        let config = state.config.read().expect("config 读锁中毒");
        auto_delivery_enabled(&config.orchestrator)
    };
    let selected = select_experiment_winner(
        state.orchestrator_repo.as_ref(),
        experiment_id,
        winner_task_id,
        reason,
        ComparativeConfidence::High,
        should_auto,
    )
    .await?;
    if should_auto && selected.selected {
        let winner = start_experiment_winner_delivery(
            state.orchestrator_repo.as_ref(),
            experiment_id,
        )
        .await
        .unwrap_or_else(|_| winner_task_id.to_string());
        let _ = state
            .orchestrator_repo
            .try_transition_task_status(
                &winner,
                OrchestratorTaskStatus::Done,
                OrchestratorTaskStatus::Delivering,
                None,
            )
            .await?;
        // 也可能已是其他状态，再尝试从 Verifying/Done
        let task = state.orchestrator_repo.get_task(&winner).await?;
        if task.status != OrchestratorTaskStatus::Delivering {
            let _ = state
                .orchestrator_repo
                .set_task_status(&winner, OrchestratorTaskStatus::Delivering, None)
                .await;
        }
        let delivered = run_delivery_for_task(state, &winner, None).await?;
        if delivered.status == OrchestratorTaskStatus::Done {
            mark_experiment_delivery_completed(state.orchestrator_repo.as_ref(), experiment_id)
                .await?;
        }
    }
    get_orchestrator_experiment_for_state(state, experiment_id).await
}

/// 取消整组实验。
///
/// Business Logic（为什么需要这个函数）:
///     用户可取消整组；running agents 由既有 abort 路径处理。
///
/// Code Logic（这个函数做什么）:
///     组 CAS Cancelled；非终态 candidates → Cancelled；child tasks abort。
#[tauri::command]
pub async fn cancel_orchestrator_experiment(
    state: State<'_, AppState>,
    experiment_id: String,
) -> Result<OrchestratorExperimentDto, AppError> {
    cancel_orchestrator_experiment_for_state(&state, &experiment_id).await
}

/// Business Logic（为什么需要这个函数）:
///     共享取消入口。
///
/// Code Logic（这个函数做什么）:
///     cancel group + candidates + abort child tasks。
pub async fn cancel_orchestrator_experiment_for_state(
    state: &AppState,
    experiment_id: &str,
) -> Result<OrchestratorExperimentDto, AppError> {
    let exp = state.orchestrator_repo.get_experiment(experiment_id).await?;
    if exp.status == ExperimentStatus::Delivering {
        return Err(AppError::generic(
            "实验正在交付中，不能取消；请等待交付完成",
        ));
    }
    if !exp.status.is_terminal() {
        let _ = state
            .orchestrator_repo
            .cas_experiment_status(
                experiment_id,
                exp.version,
                exp.status,
                ExperimentStatus::Cancelled,
                exp.winner_task_id.as_deref(),
                Some("cancelled by user"),
                exp.confidence,
            )
            .await?;
    }
    let cands = state
        .orchestrator_repo
        .list_experiment_candidates(experiment_id)
        .await?;
    for c in cands {
        if !c.outcome.is_terminal() {
            let _ = state
                .orchestrator_repo
                .set_candidate_outcome(experiment_id, &c.task_id, CandidateOutcome::Cancelled)
                .await;
            let _ = state
                .orchestrator_repo
                .set_task_status(&c.task_id, OrchestratorTaskStatus::Aborted, None)
                .await;
        }
    }
    get_orchestrator_experiment_for_state(state, experiment_id).await
}

/// 降级前 quiesce：取消非终态实验组。
///
/// Business Logic（为什么需要这个函数）:
///     关闭 experiments 能力前必须停止 active groups，否则旧版本可能把 candidate 当普通 task 交付。
///
/// Code Logic（这个函数做什么）:
///     拒绝若存在 Delivering；否则取消全部非终态组。
#[tauri::command]
pub async fn prepare_experiment_downgrade(
    state: State<'_, AppState>,
) -> Result<u32, AppError> {
    prepare_experiment_downgrade_for_state(&state).await
}

/// Business Logic（为什么需要这个函数）:
///     local-only 降级 helper。
///
/// Code Logic（这个函数做什么）:
///     扫描全部实验；Delivering 拒绝；其余非终态 cancel。
pub async fn prepare_experiment_downgrade_for_state(state: &AppState) -> Result<u32, AppError> {
    let all = state.orchestrator_repo.list_experiments(None).await?;
    if all
        .iter()
        .any(|e| e.status == ExperimentStatus::Delivering)
    {
        return Err(AppError::generic(
            "存在正在交付的实验组，无法降级；请等待完成",
        ));
    }
    let mut cancelled = 0u32;
    for exp in all {
        if exp.status.is_terminal() {
            continue;
        }
        cancel_orchestrator_experiment_for_state(state, &exp.id).await?;
        cancelled = cancelled.saturating_add(1);
    }
    Ok(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::experiments::create::request_with_candidates;
    use crate::orchestrator::repo::OrchestratorRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> OrchestratorRepo {
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

    #[tokio::test]
    async fn create_via_service_produces_candidates() {
        let repo = setup_repo().await;
        let outcome =
            create_experiment_idempotently(&repo, &request_with_candidates(2), 4)
                .await
                .unwrap();
        assert!(outcome.newly_created);
        assert_eq!(
            outcome
                .experiment
                .candidates
                .as_ref()
                .map(|c| c.len())
                .unwrap_or(0),
            2
        );
    }
}
