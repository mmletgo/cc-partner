//! queue/dispatch/complete/retry/abort
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helpers。

use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorTaskDto, OrchestratorTaskRow, OrchestratorTaskStatus,
    EVIDENCE_KIND_VERIFICATION_OUTPUT,
};
use crate::orchestrator::outbox::OrchestratorRemoteOutboxDto;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::verifier;
use crate::state::AppState;
use std::path::{Path, PathBuf};
use tauri::State;

use super::common::*;

/// 将 Orchestrator 草稿任务加入队列。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认草稿任务后，需要把任务状态切换为 Queued；非草稿任务不能被回退入队。
///
/// Code Logic（这个函数做什么）:
///     调用 repo.queue_task 原子校验 Draft 状态并更新为 queued，再把完整任务 Row 转换为 DTO。
#[tauri::command]
pub async fn queue_orchestrator_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state.orchestrator_repo.queue_task(&task_id).await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// 手动触发一次 Orchestrator 调度。
///
/// Business Logic（为什么需要这个函数）:
///     用户或测试需要立即触发一次队列领取，而不是等待后台 scheduler 的 10 秒 tick。
///
/// Code Logic（这个函数做什么）:
///     调用 scheduler::dispatch_once 复用后台调度逻辑，并返回本次 dispatched 任务数。
#[tauri::command]
pub async fn dispatch_orchestrator_once(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, AppError> {
    let dispatched = crate::orchestrator::scheduler::dispatch_once(state.inner()).await?;
    Ok(build_dispatch_once_response(dispatched))
}

/// 标记 Agent 已完成并执行验证命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 Workbench 中看到 Claude Code 完成后，需要从 Orchestrator 触发项目验证；Phase 7 的终端哨兵也复用同一流程。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 只解包 State 和 String，再委托 complete_orchestrator_agent_run_for_state 执行内部 pipeline。
#[tauri::command]
pub async fn complete_orchestrator_agent_run(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    complete_orchestrator_agent_run_for_state(state.inner(), &task_id).await
}

/// Business Logic（为什么需要这个函数）:
///     手动完成按钮和 terminal completion sentinel 必须共用同一验证/交付 pipeline，避免状态机和 evidence 语义分叉。
///
/// Code Logic（这个函数做什么）:
///     用 expected-status 原子转移执行 Running->Verifying；之后读取全局验证命令和 worktree cwd，执行验证；
///     Verifying 后的可预期错误统一写 failed evidence 并置 Blocked，成功写 passed/skipped evidence 并推进 Delivering；
///     随后立即调用 delivery pipeline，返回最终 Done 或 Blocked 任务 DTO。
pub(crate) async fn complete_orchestrator_agent_run_for_state(
    state: &AppState,
    task_id: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .transition_task_status(
            task_id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Verifying,
            None,
        )
        .await?;

    complete_orchestrator_agent_run_after_verifying_transition(state, task).await
}

/// Business Logic（为什么需要这个函数）:
///     terminal sentinel 只代表产生该输出的具体 session/attempt 完成，旧 session 的迟到哨兵不得推进当前 active runner。
///
/// Code Logic（这个函数做什么）:
///     用 task_id + expected attempt + expected session 原子校验 Running runner 后切到 Verifying；未命中时返回当前任务 no-op。
pub(crate) async fn complete_orchestrator_agent_run_for_attempt(
    state: &AppState,
    task_id: &str,
    attempt: i64,
    session_id: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let Some(task) = state
        .orchestrator_repo
        .try_transition_running_attempt_to_verifying(task_id, attempt, session_id)
        .await?
    else {
        let current = state.orchestrator_repo.get_task(task_id).await?;
        return Ok(OrchestratorTaskDto::from(current));
    };

    complete_orchestrator_agent_run_after_verifying_transition(state, task).await
}

/// Business Logic（为什么需要这个函数）:
///     手动完成和 sentinel 完成在成功取得 Verifying 执行权后，后续 attempt 完成、验证、delivery 语义必须完全一致。
///
/// Code Logic（这个函数做什么）:
///     接收已被原子切到 Verifying 的任务 Row，标记 active attempt completed，执行验证命令并写 evidence；
///     非零验证输出交给 verifier Claude 裁决，passed=true 进入 delivery，passed=false 回 Preparing 启动修复 runner。
pub(crate) async fn complete_orchestrator_agent_run_after_verifying_transition(
    state: &AppState,
    task: OrchestratorTaskRow,
) -> Result<OrchestratorTaskDto, AppError> {
    if let Err(err) =
        mark_active_running_attempt_completed(state.orchestrator_repo.as_ref(), &task).await
    {
        return block_task_with_verification_error(state, &task.id, "标记任务尝试完成失败", err)
            .await;
    }

    let Some(worktree_id) = task.worktree_id.as_deref() else {
        return block_task_with_verification_failure(
            state,
            &task.id,
            "任务缺少 worktree，无法运行验证命令。",
        )
        .await;
    };

    let worktree = match state.workbench_worktree_repo.get(worktree_id).await {
        Ok(Some(worktree)) => worktree,
        Ok(None) => {
            return block_task_with_verification_failure(
                state,
                &task.id,
                &format!("找不到任务 worktree: {worktree_id}"),
            )
            .await;
        }
        Err(err) => {
            return block_task_with_verification_error(
                state,
                &task.id,
                "读取任务 worktree 失败",
                err,
            )
            .await;
        }
    };
    let cwd = PathBuf::from(&worktree.path);
    let project = match state.workbench_project_repo.get(&task.project_id).await {
        Ok(Some(project)) => project,
        Ok(None) => {
            return block_task_with_verification_failure(
                state,
                &task.id,
                &format!("找不到任务所属 Workbench 项目: {}", task.project_id),
            )
            .await;
        }
        Err(err) => {
            return block_task_with_verification_error(
                state,
                &task.id,
                "读取任务所属 Workbench 项目失败",
                err,
            )
            .await;
        }
    };
    let global_orchestrator_config = {
        let config = state.config.read().expect("config 读锁中毒");
        config.orchestrator.clone()
    };
    let verification_commands = match validation_commands_for_agent_completion(
        Path::new(&project.path),
        &global_orchestrator_config,
    ) {
        Ok(commands) => commands,
        Err(err) => {
            return block_task_with_verification_error(
                state,
                &task.id,
                "项目 WORKFLOW.md 解析失败",
                err,
            )
            .await;
        }
    };

    let validation_report =
        match crate::orchestrator::delivery::run_validation_commands_for_verifier(
            &cwd,
            &verification_commands,
        )
        .await
        {
            Ok(report) => report,
            Err(err) => {
                return block_task_with_verification_error(
                    state,
                    &task.id,
                    "验证命令基础设施失败",
                    err,
                )
                .await;
            }
        };
    state
        .orchestrator_repo
        .add_evidence(
            &task.id,
            EVIDENCE_KIND_VERIFICATION_OUTPUT,
            "验证命令",
            &validation_report.summary,
            &validation_report.content,
        )
        .await?;
    if let Some(current) =
        stop_verification_if_task_changed(state.orchestrator_repo.as_ref(), &task.id).await?
    {
        return Ok(current);
    }

    let diff_snapshot = match verifier::collect_worktree_diff(&cwd) {
        Ok(diff) => diff,
        Err(err) => {
            return block_task_with_verification_review_error(
                state,
                &task.id,
                "读取 worktree diff 失败",
                err,
            )
            .await;
        }
    };
    let review_digest = diff_snapshot.review_digest.clone();
    let diff = diff_snapshot.text;
    if let Some(current) =
        stop_verification_if_task_changed(state.orchestrator_repo.as_ref(), &task.id).await?
    {
        return Ok(current);
    }
    let review =
        match verifier::run_verifier_claude(state, &task, &cwd, &validation_report.content, &diff)
            .await
        {
            Ok(review) => review,
            Err(err) => {
                return block_task_with_verification_review_error(
                    state,
                    &task.id,
                    "Claude verifier 失败",
                    err,
                )
                .await;
            }
        };
    if let Some(current) =
        stop_verification_if_task_changed(state.orchestrator_repo.as_ref(), &task.id).await?
    {
        return Ok(current);
    }
    add_verification_evidence(state, &task.id, &verification_review_evidence(&review)).await?;
    if review.passed {
        // 所有 hard-gate 通过路径都持久化 review_digest，供 experiment 延后交付 rebind。
        crate::orchestrator::delivery::persist_review_digest_evidence(
            state.orchestrator_repo.as_ref(),
            &task.id,
            &review_digest,
        )
        .await?;

        // A4：experiment candidate 永不进入普通 HumanReview/delivery
        if task.delivery_suppressed || task.experiment_id.is_some() {
            // 将 Verifying 任务落到非交付终态：Done + InProgress/Idle，outcome=CandidateReady
            let repo = state.orchestrator_repo.as_ref();
            let split = crate::orchestrator::models::SplitTaskState {
                workflow_state: crate::orchestrator::models::OrchestratorWorkflowState::InProgress,
                run_state: crate::orchestrator::models::OrchestratorRunState::Idle,
            };
            let _ = repo
                .try_transition_task_split_state(
                    &task.id,
                    crate::orchestrator::models::OrchestratorTaskStatus::Verifying,
                    crate::orchestrator::models::OrchestratorTaskStatus::Done,
                    split.workflow_state,
                    split.run_state,
                    Some(crate::orchestrator::models::OrchestratorAttemptPhase::Succeeded),
                    None,
                )
                .await?;
            crate::orchestrator::experiments::reducer::record_candidate_review(
                repo, &task.id, true,
            )
            .await?;
            // high+full-auto 时尝试启动 winner delivery
            if let Some(exp_id) = task.experiment_id.as_deref() {
                maybe_auto_deliver_experiment_winner(state, exp_id).await?;
            }
            let current = repo.get_task(&task.id).await?;
            return Ok(OrchestratorTaskDto::from(current));
        }

        let should_auto_deliver = {
            let config = state.config.read().expect("config 读锁中毒");
            auto_delivery_enabled(&config.orchestrator)
        };
        if !should_auto_deliver {
            let review_transition = transition_verified_task_to_human_review(
                state.orchestrator_repo.as_ref(),
                &task.id,
            )
            .await?;
            if review_transition.transitioned {
                crate::orchestrator::notifications::emit_task_operational_notification(
                    state,
                    crate::orchestrator::models::OperationalNotificationKind::HumanReview,
                    &review_transition.task,
                );
            }
            return Ok(OrchestratorTaskDto::from(review_transition.task));
        }

        let delivery_transition =
            transition_verified_task_to_delivering(state.orchestrator_repo.as_ref(), &task.id)
                .await?;
        if !delivery_transition.transitioned {
            return Ok(OrchestratorTaskDto::from(delivery_transition.task));
        }
        // verifier→commit 内容 rebind：进入 Delivering 后、跑 pipeline 前再采一次 digest。
        if let Err(reason) =
            crate::orchestrator::delivery::enforce_expected_review_digest(&cwd, &review_digest)
        {
            return block_task_with_delivery_error(
                state,
                &delivery_transition.task.id,
                AppError::generic(reason),
            )
            .await;
        }
        // 将 verifier 审阅时的 digest 传入 delivery；pipeline 在 lock 后再次 recheck。
        return run_delivery_for_task(state, &delivery_transition.task.id, Some(review_digest))
            .await;
    }

    // A4：experiment candidate 可走 repair；若最终 Blocked/Aborted 由 start_repair_* /
    // block_task_* 统一 sync candidate Failed，禁止仅靠 is_err 分支（Ok(Blocked) 会漏）。
    if task.delivery_suppressed || task.experiment_id.is_some() {
        return start_repair_runner_for_failed_review(state, &task.id, &review).await;
    }

    start_repair_runner_for_failed_review(state, &task.id, &review).await
}

/// Business Logic（为什么需要这个函数）:
///     high confidence + full-auto 时实验 winner 应自动进入既有 delivery。
///
/// Code Logic（这个函数做什么）:
///     读实验状态；若 WinnerReady 且 confidence=high 且 full-auto，则 CAS Delivering 并 deliver_task；
///     必须加载 winner 持久化 review_digest，缺失则 fail closed 并 recover；
///     任务 CAS 未命中且非 Delivering、或交付落到 Blocked/Aborted 等时 recover；
///     若仍为 Delivering（丢锁/并发持有）则不 recover，留给持锁方完成（无启动崩溃恢复，NOT VERIFIED）。
async fn maybe_auto_deliver_experiment_winner(
    state: &crate::state::AppState,
    experiment_id: &str,
) -> Result<(), crate::error::AppError> {
    use crate::orchestrator::experiments::delivery::{
        mark_experiment_delivery_completed, recover_experiment_from_failed_delivery,
        start_experiment_winner_delivery,
    };
    use crate::orchestrator::experiments::models::{ComparativeConfidence, ExperimentStatus};
    use crate::orchestrator::models::OrchestratorTaskStatus;

    let repo = state.orchestrator_repo.as_ref();
    let exp = repo.get_experiment(experiment_id).await?;
    if exp.status != ExperimentStatus::WinnerReady {
        return Ok(());
    }
    if exp.confidence != Some(ComparativeConfidence::High) {
        return Ok(());
    }
    let should_auto = {
        let config = state.config.read().expect("config 读锁中毒");
        auto_delivery_enabled(&config.orchestrator)
    };
    if !should_auto {
        return Ok(());
    }
    let winner_id = start_experiment_winner_delivery(repo, experiment_id).await?;
    // 仅允许 CandidateReady 路径的 Done→Delivering CAS；中止/阻塞不得复活交付。
    let transitioned = repo
        .try_transition_task_status(
            &winner_id,
            OrchestratorTaskStatus::Done,
            OrchestratorTaskStatus::Delivering,
            None,
        )
        .await?;
    if transitioned.is_none() {
        let task = repo.get_task(&winner_id).await?;
        if task.status != OrchestratorTaskStatus::Delivering {
            // 组已进 Delivering 但任务无法交付 → 回收组状态供重试/取消。
            recover_experiment_from_failed_delivery(repo, experiment_id).await?;
            return Ok(());
        }
    }
    // fail closed：无持久化 digest 不得以 None 绕过 rebind。
    let Some(digest) =
        crate::orchestrator::delivery::load_persisted_review_digest(repo, &winner_id).await?
    else {
        tracing::warn!(
            experiment_id = %experiment_id,
            task_id = %winner_id,
            "experiment winner missing reviewDigest evidence; refuse auto-delivery"
        );
        recover_experiment_from_failed_delivery(repo, experiment_id).await?;
        return Ok(());
    };
    let delivered = match run_delivery_for_task(state, &winner_id, Some(digest)).await {
        Ok(dto) => dto,
        Err(err) => {
            tracing::debug!(
                experiment_id = %experiment_id,
                task_id = %winner_id,
                "experiment auto-delivery error: {err}"
            );
            recover_experiment_from_failed_delivery(repo, experiment_id).await?;
            return Ok(());
        }
    };
    if delivered.status == OrchestratorTaskStatus::Done {
        mark_experiment_delivery_completed(repo, experiment_id).await?;
    } else if delivered.status == OrchestratorTaskStatus::Delivering {
        // 丢锁/并发交付仍在进行：保持组 Delivering，禁止误 recover 到 WinnerReady。
        tracing::debug!(
            experiment_id = %experiment_id,
            task_id = %winner_id,
            "experiment auto-delivery returned while still Delivering; leave for lock holder"
        );
    } else {
        recover_experiment_from_failed_delivery(repo, experiment_id).await?;
    }
    Ok(())
}

/// 重试阻塞的 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户处理完 blocked 原因后，需要把任务重新放回队列，但不应立即 dispatch。
///
/// Code Logic（这个函数做什么）:
///     通过 repo expected-status 原子转移只允许 Blocked->Queued，并清空 blocked_reason；worktree/session 不做删除。
#[tauri::command]
pub async fn retry_orchestrator_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let updated = state
        .orchestrator_repo
        .transition_task_status(
            &task_id,
            OrchestratorTaskStatus::Blocked,
            OrchestratorTaskStatus::Queued,
            None,
        )
        .await?;
    Ok(OrchestratorTaskDto::from(updated))
}

/// 终止 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要从 blocked UI 或队列中终止不再继续的任务，同时保留现场用于人工检查；
///     experiment candidate 必须同步 Cancelled，防止后续 approve 复活交付。
///
/// Code Logic（这个函数做什么）:
///     将任务状态设置为 Aborted，清空 blocked_reason；若有 experiment_id 则
///     mark_candidate_cancelled；不删除 worktree/session。
#[tauri::command]
pub async fn abort_orchestrator_task(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state.orchestrator_repo.get_task(&task_id).await?;
    let target = abort_orchestrator_task_target_status(task.status);
    let updated = state
        .orchestrator_repo
        .set_task_status(&task.id, target, None)
        .await?;
    if updated.experiment_id.is_some() {
        if let Err(err) =
            crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                state.orchestrator_repo.as_ref(),
                &updated.id,
                updated.status,
            )
            .await
        {
            tracing::debug!(
                task_id = %updated.id,
                "sync_candidate after abort: {err}"
            );
        }
    }
    Ok(OrchestratorTaskDto::from(updated))
}

/// Business Logic（为什么需要这个函数）:
///     失败 outbox 的 Retry/Discard 必须在本机 remote shortcut 上下文中执行，outbox 行只存在于当前设备，
///     不能递归代理到 owning device，也不能操作不属于当前 shortcut 的条目。
///
/// Code Logic（这个函数做什么）:
///     读取 Workbench 项目，要求 kind=remote；读取 outbox 并校验 device_id/path 与 shortcut 一致；
///     再调用仓储 failed-only 原子转移，返回 camelCase DTO。
pub(crate) async fn retry_orchestrator_remote_outbox_for_repos(
    orchestrator_repo: &OrchestratorRepo,
    workbench_project_repo: &crate::storage::WorkbenchProjectRepo,
    project_id: &str,
    outbox_id: &str,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    mutate_failed_remote_outbox_for_repos(
        orchestrator_repo,
        workbench_project_repo,
        project_id,
        outbox_id,
        RemoteOutboxMutation::Retry,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     用户确认放弃失败 outbox 后，本机应把该条目标为 discarded 审计终态，且不转发远端。
///
/// Code Logic（这个函数做什么）:
///     复用项目归属与 outbox 归属校验，再调用 discard_failed_remote_outbox_item。
pub(crate) async fn discard_orchestrator_remote_outbox_for_repos(
    orchestrator_repo: &OrchestratorRepo,
    workbench_project_repo: &crate::storage::WorkbenchProjectRepo,
    project_id: &str,
    outbox_id: &str,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    mutate_failed_remote_outbox_for_repos(
        orchestrator_repo,
        workbench_project_repo,
        project_id,
        outbox_id,
        RemoteOutboxMutation::Discard,
    )
    .await
}

/// 本机 failed outbox 人工动作。
///
/// Business Logic（为什么需要这个枚举）:
///     Retry 与 Discard 共享项目/outbox 归属校验，但最终仓储转移不同。
///
/// Code Logic（这个枚举做什么）:
///     区分 retry 与 discard 两条 failed-only 原子路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteOutboxMutation {
    Retry,
    Discard,
}

/// Business Logic（为什么需要这个函数）:
///     Retry/Discard 的项目归属、outbox 归属与 failed-only 约束必须集中，避免 Tauri 与 HTTP 路径分叉。
///
/// Code Logic（这个函数做什么）:
///     校验 project 存在且为 remote shortcut，校验 outbox 存在且 device_id/path 匹配，再调用对应仓储方法。
pub(crate) async fn mutate_failed_remote_outbox_for_repos(
    orchestrator_repo: &OrchestratorRepo,
    workbench_project_repo: &crate::storage::WorkbenchProjectRepo,
    project_id: &str,
    outbox_id: &str,
    mutation: RemoteOutboxMutation,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    let project_id = project_id.trim();
    let outbox_id = outbox_id.trim();
    if project_id.is_empty() {
        return Err(AppError::validation("项目 ID 不能为空"));
    }
    if outbox_id.is_empty() {
        return Err(AppError::validation("远端 outbox ID 不能为空"));
    }

    let project = workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind != "remote" {
        return Err(AppError::validation(
            "只有远端项目快捷方式可以操作本机 remote outbox",
        ));
    }

    let item = orchestrator_repo
        .get_remote_outbox_item(outbox_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("远端 outbox 不存在: {outbox_id}")))?;
    if item.device_id != project.device_id || item.remote_project_path != project.path {
        return Err(AppError::validation("远端 outbox 不属于当前项目快捷方式"));
    }

    let updated = match mutation {
        RemoteOutboxMutation::Retry => {
            orchestrator_repo
                .retry_failed_remote_outbox_item(outbox_id)
                .await?
        }
        RemoteOutboxMutation::Discard => {
            orchestrator_repo
                .discard_failed_remote_outbox_item(outbox_id)
                .await?
        }
    };
    Ok(updated.to_dto())
}

/// Business Logic（为什么需要这个函数）:
///     桌面 Automation UI 需要对本机 failed outbox 执行 Retry，且不得代理到远端 owner。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 取仓储，委托 retry_orchestrator_remote_outbox_for_repos。
pub(crate) async fn retry_orchestrator_remote_outbox_for_state(
    state: &AppState,
    project_id: &str,
    outbox_id: &str,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    retry_orchestrator_remote_outbox_for_repos(
        state.orchestrator_repo.as_ref(),
        state.workbench_project_repo.as_ref(),
        project_id,
        outbox_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     桌面 Automation UI 需要对本机 failed outbox 执行 Discard，且不得代理到远端 owner。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 取仓储，委托 discard_orchestrator_remote_outbox_for_repos。
pub(crate) async fn discard_orchestrator_remote_outbox_for_state(
    state: &AppState,
    project_id: &str,
    outbox_id: &str,
) -> Result<OrchestratorRemoteOutboxDto, AppError> {
    discard_orchestrator_remote_outbox_for_repos(
        state.orchestrator_repo.as_ref(),
        state.workbench_project_repo.as_ref(),
        project_id,
        outbox_id,
    )
    .await
}
