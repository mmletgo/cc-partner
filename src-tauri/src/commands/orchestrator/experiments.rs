//! Orchestrator 实验组 Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面/本机需要创建、列表、详情、批准 winner、取消与降级 quiesce 实验组。
//!
//! Code Logic（这个模块做什么）:
//!     封装 create_experiment_idempotently 与 delivery/select helpers；
//!     local 仅 owning device；remote shortcut 走 remote_client 或组级 outbox。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::error::AppError;
use crate::orchestrator::experiments::create::create_experiment_idempotently;
use crate::orchestrator::experiments::delivery::{
    mark_experiment_delivery_completed, recover_experiment_from_failed_delivery,
    select_experiment_winner, start_experiment_winner_delivery,
};
use crate::orchestrator::experiments::models::{
    CandidateOutcome, ComparativeConfidence, CreateExperimentRequest, ExperimentStatus,
    IdempotentCreateExperimentOutcome, OrchestratorExperimentDto,
};
use crate::orchestrator::experiments::outbox::ExperimentOutboxStatus;
use crate::orchestrator::experiments::remote_client::{
    approve_remote_experiment_winner, cancel_remote_experiment, create_remote_experiment,
    list_remote_experiments,
};
use crate::orchestrator::models::OrchestratorTaskStatus;
use crate::orchestrator::outbox::{
    is_remote_network_error, open_remote_project_for_shortcut, remote_device_base_url,
};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use tauri::State;

use super::common::{
    auto_delivery_enabled, get_orchestrator_workbench_project, run_delivery_for_task,
};

/// Business Logic（为什么需要这个函数）:
///     用户创建 2–8 candidate 实验组。
///     GuiClient 不得写本机空库或与 sidecar 双路径 dispatch。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → `BackendControlClient::create_orchestrator_experiment`；
///     HeadlessOwner → `create_orchestrator_experiment_for_state`。
#[tauri::command]
pub async fn create_orchestrator_experiment(
    state: State<'_, AppState>,
    request: CreateExperimentRequest,
) -> Result<OrchestratorExperimentDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .create_orchestrator_experiment(request)
            .await;
    }
    let outcome = create_orchestrator_experiment_for_state(&state, request).await?;
    Ok(outcome.experiment)
}

/// Business Logic（为什么需要这个函数）:
///     HTTP/route 与 Tauri 共用创建入口；调用方需要 `newly_created` 做幂等分支。
///
/// Code Logic（这个函数做什么）:
///     local project → 本机原子创建；remote shortcut → 在线 remote_client / 离线组级 outbox；
///     禁止在 remote shortcut 上写本机 candidate tasks。
pub async fn create_orchestrator_experiment_for_state(
    state: &AppState,
    request: CreateExperimentRequest,
) -> Result<IdempotentCreateExperimentOutcome, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind == "remote" {
        return create_remote_orchestrator_experiment(state, &project, request).await;
    }
    if project.kind != "local" {
        return Err(AppError::generic(
            "仅本机 owning 项目或远端 shortcut 可创建实验组",
        ));
    }
    create_local_orchestrator_experiment(state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     owning device 上创建必须拒绝非 local 项目，与 P2P `require_local_project_by_id` 对齐。
///
/// Code Logic（这个函数做什么）:
///     device max concurrency 来自全局 config；create 后仅 newly_created 时 best-effort dispatch。
pub async fn create_local_orchestrator_experiment(
    state: &AppState,
    request: CreateExperimentRequest,
) -> Result<IdempotentCreateExperimentOutcome, AppError> {
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
    Ok(outcome)
}

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 必须整组转发 owner，离线写一条 experiment outbox，禁止 N 条 task outbox。
///
/// Code Logic（这个函数做什么）:
///     open remote project → create_remote_experiment；网络错误 enqueue_experiment_outbox 并返回
///     合成 DTO（selectionReason 标记 pending outbox）；capability 失败 fail-closed。
async fn create_remote_orchestrator_experiment(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    mut request: CreateExperimentRequest,
) -> Result<IdempotentCreateExperimentOutcome, AppError> {
    match open_remote_project_for_shortcut(state, remote_shortcut, None).await {
        Ok(context) => {
            request.project_id = context.remote_project_id.clone();
            let http = reqwest::Client::new();
            match create_remote_experiment(
                state.peer_client.as_ref(),
                &http,
                &context.base_url,
                &request,
            )
            .await
            {
                Ok(resp) => {
                    if let Ok(payload) = serde_json::to_string(&resp.experiment) {
                        let _ = state
                            .orchestrator_repo
                            .upsert_experiment_mirror(
                                &remote_shortcut.device_id,
                                &remote_shortcut.device_name,
                                &context.remote_project_id,
                                &context.remote_project_path,
                                &resp.experiment.id,
                                &payload,
                            )
                            .await;
                    }
                    Ok(IdempotentCreateExperimentOutcome {
                        experiment: resp.experiment,
                        newly_created: resp.newly_created,
                    })
                }
                Err(err) if is_remote_network_error(&err) => {
                    enqueue_pending_remote_experiment(state, remote_shortcut, &request).await
                }
                Err(err) => Err(err),
            }
        }
        Err(err) if is_remote_network_error(&err) => {
            enqueue_pending_remote_experiment(state, remote_shortcut, &request).await
        }
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     owner 离线时必须保留整组 create 请求，等待 dispatcher 原子投递。
///
/// Code Logic（这个函数做什么）:
///     enqueue_experiment_outbox；返回合成 experiment DTO（id=outbox 行 id，status=queued）。
async fn enqueue_pending_remote_experiment(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: &CreateExperimentRequest,
) -> Result<IdempotentCreateExperimentOutcome, AppError> {
    let item = state
        .orchestrator_repo
        .enqueue_experiment_outbox(
            &remote_shortcut.device_id,
            &remote_shortcut.device_name,
            &remote_shortcut.path,
            None,
            request,
        )
        .await?;
    let now = chrono::Utc::now().to_rfc3339();
    let dto = OrchestratorExperimentDto {
        id: item.id.clone(),
        project_id: remote_shortcut.id.clone(),
        title: request.title.clone(),
        goal: request.goal.clone(),
        acceptance: request.acceptance.clone(),
        status: ExperimentStatus::Queued,
        selection_policy:
            crate::orchestrator::experiments::models::EXPERIMENT_SELECTION_POLICY_COMPARATIVE
                .to_string(),
        max_parallel: request.max_parallel as i64,
        winner_task_id: None,
        selection_reason: Some(format!(
            "pending remote experiment outbox ({})",
            ExperimentOutboxStatus::Pending.as_str()
        )),
        confidence: None,
        version: 0,
        created_at: now.clone(),
        updated_at: now,
        candidates: Some(
            request
                .candidates
                .iter()
                .enumerate()
                .map(|(idx, c)| {
                    crate::orchestrator::experiments::models::OrchestratorExperimentCandidateDto {
                        experiment_id: item.id.clone(),
                        task_id: format!("pending-{}-{}", item.id, idx + 1),
                        ordinal: (idx as i64) + 1,
                        provider_id: c.provider_id.clone(),
                        strategy_label: c.strategy_label.clone(),
                        outcome: CandidateOutcome::Pending,
                        selection_metadata_json: None,
                        created_at: item.created_at.clone(),
                        updated_at: item.updated_at.clone(),
                    }
                })
                .collect(),
        ),
    };
    Ok(IdempotentCreateExperimentOutcome {
        experiment: dto,
        newly_created: true,
    })
}

/// Business Logic（为什么需要这个函数）:
///     看板需要列出项目实验组。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control list；HeadlessOwner → `list_orchestrator_experiments_for_state`。
#[tauri::command]
pub async fn list_orchestrator_experiments(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .list_orchestrator_experiments(project_id.as_deref())
            .await;
    }
    list_orchestrator_experiments_for_state(&state, project_id.as_deref()).await
}

/// Business Logic（为什么需要这个函数）:
///     共享列表入口；remote shortcut 读 owning device 或 mirror。
///
/// Code Logic（这个函数做什么）:
///     local → 本机 list + candidates；remote → list_remote_experiments，离线读 mirror。
pub async fn list_orchestrator_experiments_for_state(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    if let Some(pid) = project_id {
        let project = get_orchestrator_workbench_project(state, pid).await?;
        if project.kind == "remote" {
            return list_remote_orchestrator_experiments(state, &project).await;
        }
    }
    let rows = state.orchestrator_repo.list_experiments(project_id).await?;
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
///     远端项目看板需要展示 owning device 上的实验组。
///
/// Code Logic（这个函数做什么）:
///     open remote → list_remote_experiments；网络错误时 list experiment mirrors。
async fn list_remote_orchestrator_experiments(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    match open_remote_project_for_shortcut(state, remote_shortcut, None).await {
        Ok(context) => {
            let http = reqwest::Client::new();
            match list_remote_experiments(
                state.peer_client.as_ref(),
                &http,
                &context.base_url,
                &context.remote_project_id,
            )
            .await
            {
                Ok(experiments) => {
                    for exp in &experiments {
                        if let Ok(payload) = serde_json::to_string(exp) {
                            let _ = state
                                .orchestrator_repo
                                .upsert_experiment_mirror(
                                    &remote_shortcut.device_id,
                                    &remote_shortcut.device_name,
                                    &context.remote_project_id,
                                    &context.remote_project_path,
                                    &exp.id,
                                    &payload,
                                )
                                .await;
                        }
                    }
                    Ok(experiments)
                }
                Err(err) if is_remote_network_error(&err) => {
                    state
                        .orchestrator_repo
                        .list_experiment_mirrors_for_project_path(
                            &remote_shortcut.device_id,
                            &remote_shortcut.path,
                        )
                        .await
                }
                Err(err) => Err(err),
            }
        }
        Err(err) if is_remote_network_error(&err) => {
            state
                .orchestrator_repo
                .list_experiment_mirrors_for_project_path(
                    &remote_shortcut.device_id,
                    &remote_shortcut.path,
                )
                .await
        }
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     详情页需要完整实验 + candidates。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control get；HeadlessOwner → `get_orchestrator_experiment_for_state`。
#[tauri::command]
pub async fn get_orchestrator_experiment(
    state: State<'_, AppState>,
    experiment_id: String,
) -> Result<OrchestratorExperimentDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .get_orchestrator_experiment(&experiment_id)
            .await;
    }
    get_orchestrator_experiment_for_state(&state, &experiment_id).await
}

/// Business Logic（为什么需要这个函数）:
///     共享详情入口。
///
/// Code Logic（这个函数做什么）:
///     本机 get + candidates；若 id 在本机缺失则尝试按 mirror 返回（remote 场景）。
pub async fn get_orchestrator_experiment_for_state(
    state: &AppState,
    experiment_id: &str,
) -> Result<OrchestratorExperimentDto, AppError> {
    match state.orchestrator_repo.get_experiment(experiment_id).await {
        Ok(row) => {
            let mut dto = OrchestratorExperimentDto::from(row);
            let cands = state
                .orchestrator_repo
                .list_experiment_candidates(experiment_id)
                .await?;
            dto.candidates = Some(cands.into_iter().map(Into::into).collect());
            Ok(dto)
        }
        Err(err) => {
            if let Ok(Some(mirrored)) = state
                .orchestrator_repo
                .get_experiment_mirror_by_remote_id(experiment_id)
                .await
            {
                return Ok(mirrored);
            }
            Err(err)
        }
    }
}

/// 批准/采用推荐 winner。
///
/// Business Logic（为什么需要这个函数）:
///     NeedsDecision 或 WinnerReady（非 full-auto）时用户可采用推荐或选择另一 ready candidate。
///     GuiClient 不得在本进程跑 full-auto delivery（commit/push/merge）或持有 process-local delivery lock。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → `BackendControlClient::approve_orchestrator_experiment_winner`（超时 360s）；
///     HeadlessOwner → `approve_orchestrator_experiment_winner_for_state`。
#[tauri::command]
pub async fn approve_orchestrator_experiment_winner(
    state: State<'_, AppState>,
    experiment_id: String,
    winner_task_id: String,
    reason: Option<String>,
) -> Result<OrchestratorExperimentDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .approve_orchestrator_experiment_winner(
                &experiment_id,
                &winner_task_id,
                reason.as_deref(),
            )
            .await;
    }
    approve_orchestrator_experiment_winner_for_state(
        &state,
        &experiment_id,
        &winner_task_id,
        reason.as_deref(),
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     共享批准入口；remote 实验转发 owner。
///
/// Code Logic（这个函数做什么）:
///     本机 CAS 选 winner 并可选 auto deliver；mirror-only id 走 remote approve。
pub async fn approve_orchestrator_experiment_winner_for_state(
    state: &AppState,
    experiment_id: &str,
    winner_task_id: &str,
    reason: Option<&str>,
) -> Result<OrchestratorExperimentDto, AppError> {
    if state
        .orchestrator_repo
        .get_experiment(experiment_id)
        .await
        .is_err()
    {
        if let Some(dto) =
            try_approve_remote_experiment_by_mirror(state, experiment_id, winner_task_id, reason)
                .await?
        {
            return Ok(dto);
        }
    }
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
        let winner =
            match start_experiment_winner_delivery(state.orchestrator_repo.as_ref(), experiment_id)
                .await
            {
                Ok(id) => id,
                Err(err) => {
                    tracing::debug!(
                        experiment_id = %experiment_id,
                        "start_experiment_winner_delivery failed after select: {err}"
                    );
                    return get_orchestrator_experiment_for_state(state, experiment_id).await;
                }
            };
        // 仅允许 CandidateReady 路径的 Done→Delivering 条件 CAS。
        // 禁止无条件 set_task_status：否则 Abort 后的 Aborted 会被复活并进入 Git 交付。
        let transitioned = state
            .orchestrator_repo
            .try_transition_task_status(
                &winner,
                OrchestratorTaskStatus::Done,
                OrchestratorTaskStatus::Delivering,
                None,
            )
            .await?;
        let may_deliver = match transitioned {
            Some(_) => true,
            None => {
                let current = state.orchestrator_repo.get_task(&winner).await?;
                current.status == OrchestratorTaskStatus::Delivering
            }
        };
        if !may_deliver {
            tracing::debug!(
                task_id = %winner,
                experiment_id = %experiment_id,
                "approve winner: task not Done/Delivering after select; recover experiment"
            );
            // 组可能已 CAS 到 Delivering；任务无法交付时回收，避免永久卡死且无法 cancel。
            let _ = recover_experiment_from_failed_delivery(
                state.orchestrator_repo.as_ref(),
                experiment_id,
            )
            .await?;
            return get_orchestrator_experiment_for_state(state, experiment_id).await;
        }
        // fail closed：winner 必须持有 verifier 持久化的 reviewDigest，禁止 None 绕过 rebind。
        let digest = match crate::orchestrator::delivery::load_persisted_review_digest(
            state.orchestrator_repo.as_ref(),
            &winner,
        )
        .await?
        {
            Some(d) => d,
            None => {
                tracing::warn!(
                    experiment_id = %experiment_id,
                    task_id = %winner,
                    "approve winner missing reviewDigest evidence; refuse delivery"
                );
                let _ = recover_experiment_from_failed_delivery(
                    state.orchestrator_repo.as_ref(),
                    experiment_id,
                )
                .await?;
                return Err(AppError::generic(
                    "experiment winner 缺少 reviewDigest evidence，拒绝交付（fail closed）",
                ));
            }
        };
        let delivered = match run_delivery_for_task(state, &winner, Some(digest)).await {
            Ok(dto) => dto,
            Err(err) => {
                tracing::debug!(
                    experiment_id = %experiment_id,
                    task_id = %winner,
                    "approve winner delivery error: {err}"
                );
                let _ = recover_experiment_from_failed_delivery(
                    state.orchestrator_repo.as_ref(),
                    experiment_id,
                )
                .await?;
                return get_orchestrator_experiment_for_state(state, experiment_id).await;
            }
        };
        if delivered.status == OrchestratorTaskStatus::Done {
            mark_experiment_delivery_completed(state.orchestrator_repo.as_ref(), experiment_id)
                .await?;
        } else if delivered.status == OrchestratorTaskStatus::Delivering {
            // 丢锁/并发持有：保持组 Delivering，禁止误 recover 到 WinnerReady。
            // 残差：进程崩溃后启动无自动回收 Delivering 组 — NOT VERIFIED。
            tracing::debug!(
                experiment_id = %experiment_id,
                task_id = %winner,
                "approve winner delivery still Delivering; leave for lock holder"
            );
        } else {
            recover_experiment_from_failed_delivery(
                state.orchestrator_repo.as_ref(),
                experiment_id,
            )
            .await?;
        }
    }
    get_orchestrator_experiment_for_state(state, experiment_id).await
}

/// Business Logic（为什么需要这个函数）:
///     桌面在 remote shortcut 上批准时权威状态在 owner 设备。
///
/// Code Logic（这个函数做什么）:
///     查 mirror 得 device_id/path → base_url → approve_remote_experiment_winner。
async fn try_approve_remote_experiment_by_mirror(
    state: &AppState,
    experiment_id: &str,
    winner_task_id: &str,
    reason: Option<&str>,
) -> Result<Option<OrchestratorExperimentDto>, AppError> {
    let Some(meta) = state
        .orchestrator_repo
        .get_experiment_mirror_meta(experiment_id)
        .await?
    else {
        return Ok(None);
    };
    let base_url = remote_device_base_url(state, &meta.device_id)?;
    let http = reqwest::Client::new();
    let dto = approve_remote_experiment_winner(
        state.peer_client.as_ref(),
        &http,
        &base_url,
        experiment_id,
        winner_task_id,
        reason.map(str::to_string),
    )
    .await?;
    if let Ok(payload) = serde_json::to_string(&dto) {
        let _ = state
            .orchestrator_repo
            .upsert_experiment_mirror(
                &meta.device_id,
                &meta.device_name,
                &meta.remote_project_id,
                &meta.remote_project_path,
                &dto.id,
                &payload,
            )
            .await;
    }
    Ok(Some(dto))
}

/// 取消整组实验。
///
/// Business Logic（为什么需要这个函数）:
///     用户可取消整组；running agents 由既有 abort 路径处理。
///     GuiClient 必须代理到 owner，禁止本机空库 cancel 误成功/无效。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → `BackendControlClient::cancel_orchestrator_experiment`；
///     HeadlessOwner → `cancel_orchestrator_experiment_for_state`。
#[tauri::command]
pub async fn cancel_orchestrator_experiment(
    state: State<'_, AppState>,
    experiment_id: String,
) -> Result<OrchestratorExperimentDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .cancel_orchestrator_experiment(&experiment_id)
            .await;
    }
    cancel_orchestrator_experiment_for_state(&state, &experiment_id).await
}

/// Business Logic（为什么需要这个函数）:
///     共享取消入口；remote mirror 转发 owner cancel。
///
/// Code Logic（这个函数做什么）:
///     cancel group + candidates + abort child tasks；本机缺失时 remote cancel。
pub async fn cancel_orchestrator_experiment_for_state(
    state: &AppState,
    experiment_id: &str,
) -> Result<OrchestratorExperimentDto, AppError> {
    let exp = match state.orchestrator_repo.get_experiment(experiment_id).await {
        Ok(exp) => exp,
        Err(_) => {
            if let Some(dto) = try_cancel_remote_experiment_by_mirror(state, experiment_id).await? {
                return Ok(dto);
            }
            return Err(AppError::not_found(format!(
                "Orchestrator 实验不存在: {experiment_id}"
            )));
        }
    };
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

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的取消必须落到 owning device。
///
/// Code Logic（这个函数做什么）:
///     mirror meta → cancel_remote_experiment。
async fn try_cancel_remote_experiment_by_mirror(
    state: &AppState,
    experiment_id: &str,
) -> Result<Option<OrchestratorExperimentDto>, AppError> {
    let Some(meta) = state
        .orchestrator_repo
        .get_experiment_mirror_meta(experiment_id)
        .await?
    else {
        return Ok(None);
    };
    let base_url = remote_device_base_url(state, &meta.device_id)?;
    let http = reqwest::Client::new();
    let dto = cancel_remote_experiment(state.peer_client.as_ref(), &http, &base_url, experiment_id)
        .await?;
    if let Ok(payload) = serde_json::to_string(&dto) {
        let _ = state
            .orchestrator_repo
            .upsert_experiment_mirror(
                &meta.device_id,
                &meta.device_name,
                &meta.remote_project_id,
                &meta.remote_project_path,
                &dto.id,
                &payload,
            )
            .await;
    }
    Ok(Some(dto))
}

/// 降级前 quiesce：取消非终态实验组。
///
/// Business Logic（为什么需要这个函数）:
///     关闭 experiments 能力前必须停止 active groups，否则旧版本可能把 candidate 当普通 task 交付。
///     GuiClient 不得在本进程扫/改 owner 仓储，否则与 sidecar 双路径 cancel 漂移。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → `BackendControlClient::prepare_experiment_downgrade`；
///     HeadlessOwner → `prepare_experiment_downgrade_for_state`。
#[tauri::command]
pub async fn prepare_experiment_downgrade(state: State<'_, AppState>) -> Result<u32, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .prepare_experiment_downgrade()
            .await;
    }
    prepare_experiment_downgrade_for_state(&state).await
}

/// Business Logic（为什么需要这个函数）:
///     local-only 降级 helper；仅 owner 可执行组级 cancel。
///
/// Code Logic（这个函数做什么）:
///     扫描全部实验；Delivering 拒绝；其余非终态 cancel。
pub async fn prepare_experiment_downgrade_for_state(state: &AppState) -> Result<u32, AppError> {
    let all = state.orchestrator_repo.list_experiments(None).await?;
    if all.iter().any(|e| e.status == ExperimentStatus::Delivering) {
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
        let outcome = create_experiment_idempotently(&repo, &request_with_candidates(2), 4)
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

    /// Business Logic（为什么需要这个测试）:
    ///     GuiClient 必须代理 experiment mutation/list/get/downgrade 到 sidecar，否则双路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源码断言六个 Tauri wrapper 均含 RuntimeRole::GuiClient 分支与 BackendControlClient。
    #[test]
    fn experiment_tauri_wrappers_proxy_gui_client_to_control() {
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/commands/orchestrator/experiments.rs"
        ));
        for marker in [
            "create_orchestrator_experiment",
            "list_orchestrator_experiments",
            "get_orchestrator_experiment",
            "approve_orchestrator_experiment_winner",
            "cancel_orchestrator_experiment",
            "prepare_experiment_downgrade",
        ] {
            assert!(src.contains(marker), "experiments.rs 必须定义 {marker}");
        }
        assert!(
            src.matches("RuntimeRole::GuiClient").count() >= 6,
            "六个 Tauri experiment 入口均需 GuiClient 分支"
        );
        assert!(
            src.contains("BackendControlClient::from_control_file"),
            "GuiClient 必须经 BackendControlClient 代理"
        );
        assert!(
            src.contains(".create_orchestrator_experiment(request)"),
            "create 必须走 control client"
        );
        assert!(
            src.contains(".approve_orchestrator_experiment_winner("),
            "approve 必须走 control client"
        );
        assert!(
            src.contains(".cancel_orchestrator_experiment(&experiment_id)"),
            "cancel 必须走 control client"
        );
        assert!(
            src.contains(".list_orchestrator_experiments(project_id.as_deref())"),
            "list 必须走 control client"
        );
        assert!(
            src.contains(".get_orchestrator_experiment(&experiment_id)"),
            "get 必须走 control client"
        );
        assert!(
            src.contains(".prepare_experiment_downgrade()"),
            "prepare_experiment_downgrade 必须走 control client"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     control 路由与 client path 漂移会导致 GUI 代理 404。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 http_server / control_client 含 experiments 六条 control 路径字面量。
    #[test]
    fn experiment_control_routes_registered_in_http_and_client() {
        let http = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/net/http_server.rs"
        ));
        let client = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/backend/control_client.rs"
        ));
        for path in [
            "/api/backend/control/orchestrator/experiments/create",
            "/api/backend/control/orchestrator/experiments/list",
            "/api/backend/control/orchestrator/experiments/get",
            "/api/backend/control/orchestrator/experiments/approve-winner",
            "/api/backend/control/orchestrator/experiments/cancel",
            "/api/backend/control/orchestrator/experiments/prepare-downgrade",
        ] {
            assert!(http.contains(path), "http_server 必须挂载 {path}");
        }
        for path in [
            "orchestrator/experiments/create",
            "orchestrator/experiments/list",
            "orchestrator/experiments/get",
            "orchestrator/experiments/approve-winner",
            "orchestrator/experiments/cancel",
            "orchestrator/experiments/prepare-downgrade",
        ] {
            assert!(client.contains(path), "control_client 必须调用 {path}");
        }
    }
}
