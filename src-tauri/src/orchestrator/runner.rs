//! Orchestrator visible Workbench runner.
//!
//! Business Logic（为什么需要这个模块）:
//!     调度器领取任务后，需要为任务准备用户可见的 Workbench worktree 和 terminal window，并把
//!     Claude Code 执行 Prompt 写入终端。
//!
//! Code Logic（这个模块做什么）:
//!     封装 runner 准备流程与分支名生成 helper；Git worktree 与 terminal session 实际创建复用
//!     Workbench 既有命令层本机 helper。

use crate::commands::workbench::{
    local_create_workbench_session, local_create_workbench_worktree,
    local_write_workbench_session_input,
};
use crate::error::AppError;
use crate::orchestrator::agent_adapter::{
    ensure_opencode_bridge_verified, opencode_preflight_block_reason, render_terminal_command,
    resolve_task_runner_policy, AgentAdapterRegistry, AgentAvailability, AgentLaunchRequest,
    AgentProviderId, TerminalShellDialect,
};
use crate::orchestrator::claude_runtime::associate_claude_runtime;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorAttemptStatus, OrchestratorTaskRow,
    OrchestratorTaskStatus, EVIDENCE_KIND_DEVELOPMENT_ATTEMPT,
};
use crate::orchestrator::prompt::{build_repair_task_prompt, RepairPromptContext};
use crate::orchestrator::runner_limits::{
    check_next_attempt, is_max_turns_exceeded, EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED,
};
use crate::orchestrator::workflow::{resolve_project_workflow, PromptTaskContext};
use crate::state::AppState;
use crate::workbench::agent_runtime::opencode_bridge::OpenCodeRuntimeBridge;
use std::path::Path;
use std::time::Duration;

const TASK_BRANCH_PREFIX: &str = "agent";
const TASK_BRANCH_MAX_LEN: usize = 80;
const DEFAULT_TERMINAL_COLS: u16 = 120;
const DEFAULT_TERMINAL_ROWS: u16 = 32;
const RUNTIME_ASSOCIATION_ATTEMPTS: usize = 5;
const RUNTIME_ASSOCIATION_RETRY_DELAY: Duration = Duration::from_millis(200);

/// Runner 使用的 worktree 现场。
///
/// Business Logic（为什么需要这个结构体）:
///     初始轮次会创建新 worktree，修复轮次会复用旧 worktree；后续创建 session 和生成 prompt 都只需要 id/path。
///
/// Code Logic（这个结构体做什么）:
///     将 Workbench DTO/Row 的 id 与 path 投影为 Runner 内部最小结构，避免公开依赖具体 Workbench 类型。
struct RunnerWorktree {
    id: String,
    path: String,
}

/// Runner 启动 attempt 时要写入的 developmentAttempt evidence。
///
/// Business Logic（为什么需要这个结构体）:
///     前端任务详情需要展示历史开发尝试，后端必须在每轮 runner 启动前留下可审计记录。
///
/// Code Logic（这个结构体做什么）:
///     保存 add_evidence 所需 title/summary/content，其中 summary 使用 running 表示该轮开发已挂账启动。
struct DevelopmentAttemptEvidence {
    title: String,
    summary: &'static str,
    content: String,
}

/// Business Logic（为什么需要这个函数）:
///     调度器领取任务后，需要创建隔离 worktree 和可见终端，并把 Claude Code 任务 Prompt 写入现场。
///
/// Code Logic（这个函数做什么）:
///     兼容旧 scheduler 调用点，转发到首轮 Runner 准备逻辑。
pub async fn prepare_visible_runner(
    state: &AppState,
    task: &OrchestratorTaskRow,
) -> Result<OrchestratorTaskRow, AppError> {
    prepare_initial_runner(state, task).await
}

/// Business Logic（为什么需要这个函数）:
///     首轮或 Blocked retry 调度需要创建/复用隔离 worktree 和可见终端，并注入初始任务 Prompt。
///
/// Code Logic（这个函数做什么）:
///     根据任务当前 attempt/worktree 选择本次 runner attempt；prompt 由 prepare_runner_attempt 在 worktree 路径确定后生成。
pub async fn prepare_initial_runner(
    state: &AppState,
    task: &OrchestratorTaskRow,
) -> Result<OrchestratorTaskRow, AppError> {
    let attempt = initial_runner_attempt(task)?;
    prepare_runner_attempt(state, task, String::new(), attempt).await
}

/// Business Logic（为什么需要这个函数）:
///     后续 verifier/repair loop 需要在同一 worktree 中开启新终端执行修复 Prompt，Phase 7 先预留该入口。
///
/// Code Logic（这个函数做什么）:
///     读取任务当前 worktree 路径生成 repair Prompt，并以 task.attempt+1 的修复轮次调用统一 attempt 准备流程。
#[allow(dead_code)]
pub async fn prepare_repair_runner(
    state: &AppState,
    task: &OrchestratorTaskRow,
    context: RepairPromptContext<'_>,
) -> Result<OrchestratorTaskRow, AppError> {
    let attempt = (task.attempt + 1).max(2);
    let worktree_id = worktree_seed_for_attempt(task, attempt)?
        .ok_or_else(|| AppError::generic("修复轮次缺少可复用 worktree"))?;
    let worktree = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("任务 worktree 不存在: {worktree_id}")))?;
    let prompt = build_repair_task_prompt(task, &worktree.path, &context);
    prepare_runner_attempt(state, task, prompt, attempt).await
}

/// Business Logic（为什么需要这个函数）:
///     每一轮 Runner attempt 都要创建新的 terminal session，并记录 prompt/worktree/session 到 attempt history。
///
/// Code Logic（这个函数做什么）:
///     校验本机项目；attempt=1 创建/复用确定性 worktree，attempt>1 复用 task.worktree_id；长步骤前后 touch Preparing lease；
///     随后创建 session，用 Preparing 条件更新任务 active runner 字段；写 attempt/terminal 前重读 active runner，
///     避免 Abort 后仍启动旧 Claude。
pub async fn prepare_runner_attempt(
    state: &AppState,
    task: &OrchestratorTaskRow,
    prompt: String,
    attempt: i64,
) -> Result<OrchestratorTaskRow, AppError> {
    let claim_token = task
        .prepare_claim_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AppError::generic("Preparing 任务缺少 claim token，拒绝继续 prepare"))?
        .to_string();

    // phase CAS 必须返回 hit/miss：miss 时绝不能采用 DB 中另一世代的 token 继续 prepare。
    let Some(preparing_task) = state
        .orchestrator_repo
        .update_task_attempt_phase(
            &task.id,
            OrchestratorAttemptPhase::PreparingWorkspace,
            &claim_token,
        )
        .await?
    else {
        // claim 已过期/被回收/重新签发：旧 runner 立即终止。
        return state.orchestrator_repo.get_task(&task.id).await;
    };
    if preparing_task.status != OrchestratorTaskStatus::Preparing {
        return Ok(preparing_task);
    }
    // 命中后继续使用本轮 claim_token（与 CAS 入参一致），禁止采用返回行中可能不同的 token。
    // A4：candidate Running 仅在 prepare/launch 全部成功后标记（见函数末尾），
    // 禁止在 probe/worktree 前标 Running，否则失败后 experiment 永久卡住。

    let project = state
        .workbench_project_repo
        .get(&task.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind != "local" {
        return Err(AppError::generic(
            "Orchestrator Runner 目前只支持本机 Workbench 项目",
        ));
    }

    let branch_name = task
        .branch_name
        .clone()
        .unwrap_or_else(|| task_branch_name(&task.id, &task.title));
    // 长 git worktree 创建前续租，避免调度 lease 误回收仍在 prepare 的任务。
    if !state
        .orchestrator_repo
        .touch_preparing_lease(&task.id, &claim_token)
        .await?
    {
        return state.orchestrator_repo.get_task(&task.id).await;
    }
    // 创建 worktree/session 前冻结 runner policy：已挂账 task 字段优先，否则读 WORKFLOW。
    let workflow_for_policy = resolve_project_workflow(Path::new(&project.path))?;
    let runner_policy = resolve_task_runner_policy(
        &workflow_for_policy.runner.provider,
        workflow_for_policy.runner.max_turns,
        workflow_for_policy.runner.stall_timeout_ms,
        task.runner_provider.as_deref(),
        task.runner_max_turns,
        task.runner_stall_timeout_ms,
    )?;
    // max_turns 在创建 worktree/session/attempt 之前强制检查。
    if let Err(error) = check_next_attempt(&runner_policy, attempt) {
        if is_max_turns_exceeded(&error) {
            // 仅 CAS 赢家写 evidence + 可选 attempt 终态，避免 lease 回收后重复 evidence。
            if let Ok(Some(blocked)) = state
                .orchestrator_repo
                .try_transition_task_status(
                    &task.id,
                    OrchestratorTaskStatus::Preparing,
                    OrchestratorTaskStatus::Blocked,
                    Some(EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED),
                )
                .await
            {
                // 若上一 attempt 仍 running，CAS 到 TurnLimitReached（不存在则忽略）。
                if attempt > 1 {
                    let _ = state
                        .orchestrator_repo
                        .mark_attempt_turn_limit_reached(&task.id, attempt - 1)
                        .await;
                }
                let _ = state
                    .orchestrator_repo
                    .add_evidence(
                        &task.id,
                        "runnerLimit",
                        "Runner max turns exceeded",
                        EVIDENCE_CODE_RUNNER_MAX_TURNS_EXCEEDED,
                        &format!(
                            "next_attempt={attempt}\nmax_turns={}\nprovider={}",
                            runner_policy.max_turns,
                            runner_policy.provider.as_str()
                        ),
                    )
                    .await;
                return Ok(blocked);
            }
            return state.orchestrator_repo.get_task(&task.id).await;
        }
        return Err(error);
    }

    // owner probe：adapter 不可用时 fail-closed，禁止创建 worktree/session。
    let adapter_registry = {
        let config = state
            .config
            .read()
            .map_err(|_| AppError::generic("读取 AppConfig 失败（锁损坏）"))?;
        AgentAdapterRegistry::from_app_config(&config)
    };
    let probe = adapter_registry.probe_cached(runner_policy.provider)?;
    if probe.availability != AgentAvailability::Available {
        let reason = probe
            .reason_code
            .as_deref()
            .unwrap_or("provider_unavailable");
        return block_preparing_for_adapter(state, &task.id, runner_policy.provider, reason).await;
    }

    // OpenCode：worktree/session 前 fail-closed（opt-in / bridge / L3）。
    // 禁止缺 bridge 时创建现场或回落 Sentinel。
    let mut opencode_opted_in = false;
    if runner_policy.provider == AgentProviderId::OpenCodeVisible {
        let mapping = state
            .agent_hub_repo
            .get_project_mapping_by_local_workbench_id(&task.project_id)
            .await?;
        opencode_opted_in = mapping.as_ref().map(|m| m.opted_in).unwrap_or(false);
        let project_root = Path::new(&project.path);
        let bridge_preview = OpenCodeRuntimeBridge::preview(project_root, opencode_opted_in);
        if let Err(reason) = open_code_preflight_gate(&probe, &bridge_preview) {
            return block_preparing_for_adapter(state, &task.id, runner_policy.provider, reason)
                .await;
        }
    }

    let worktree = prepare_worktree_for_attempt(state, task, &branch_name, attempt).await?;
    if !state
        .orchestrator_repo
        .touch_preparing_lease(&task.id, &claim_token)
        .await?
    {
        return state.orchestrator_repo.get_task(&task.id).await;
    }

    // OpenCode：新 worktree 必须 materialize+verify bridge 后才写输入。
    if runner_policy.provider == AgentProviderId::OpenCodeVisible {
        if let Err(err) =
            ensure_opencode_bridge_verified(Path::new(&worktree.path), opencode_opted_in)
        {
            let reason = err.to_string();
            let code = if reason.contains("externalCollision") {
                "externalCollision"
            } else if reason.contains("runtimeBridgeRequired") {
                "runtimeBridgeRequired"
            } else {
                "runtime_bridge_verify_failed"
            };
            return block_preparing_for_adapter(state, &task.id, runner_policy.provider, code)
                .await;
        }
    }

    // OpenCode 需要预分配 terminal/agent UUID，以便 shell env 与 runtime 行一致。
    let (session, agent_session_id_prealloc) =
        if runner_policy.provider == AgentProviderId::OpenCodeVisible {
            let terminal_id = uuid::Uuid::new_v4().to_string();
            let agent_id = uuid::Uuid::new_v4().to_string();
            let session = local_create_workbench_session_with_preallocated_ids(
                state,
                task.project_id.clone(),
                Some(worktree.id.clone()),
                Some(DEFAULT_TERMINAL_COLS),
                Some(DEFAULT_TERMINAL_ROWS),
                terminal_id,
                agent_id.clone(),
            )
            .await?;
            (session, Some(agent_id))
        } else {
            let session = local_create_workbench_session(
                state,
                task.project_id.clone(),
                Some(worktree.id.clone()),
                Some(DEFAULT_TERMINAL_COLS),
                Some(DEFAULT_TERMINAL_ROWS),
            )
            .await?;
            (session, None)
        };

    // A1/A3：按冻结 policy 的 provider 创建 Agent runtime（Launching）
    // OpenCode 完成契约仅为 HookEvent：runtime 行缺失则 OSC Completed 无人收，
    // 不得继续写 terminal prompt；Claude/Codex 仍可 Sentinel 降级继续。
    let agent_runtime = match crate::orchestrator::agent_runtime_bridge::create_launching_agent_for_runner_with_provider_and_id(
        state,
        &task.project_id,
        Some(&worktree.id),
        &session.id,
        &task.id,
        attempt as u32,
        runner_policy.provider,
        None,
        agent_session_id_prealloc.as_deref(),
    )
    .await
    {
        Ok(row) => Some(row),
        Err(error) => {
            if should_fail_closed_on_agent_runtime_create(runner_policy.provider) {
                tracing::warn!(
                    task_id = %task.id,
                    session_id = %session.id,
                    provider = %runner_policy.provider.as_str(),
                    "创建 Agent runtime 失败（OpenCode fail-closed）: {error}"
                );
                return block_preparing_for_adapter(
                    state,
                    &task.id,
                    runner_policy.provider,
                    "agent_runtime_create_failed",
                )
                .await;
            }
            tracing::warn!(
                task_id = %task.id,
                session_id = %session.id,
                provider = %runner_policy.provider.as_str(),
                "创建 Agent runtime 失败（继续 adapter launch）: {error}"
            );
            None
        }
    };
    let agent_session_id = agent_runtime
        .as_ref()
        .map(|r| r.id.clone())
        .or(agent_session_id_prealloc);
    // session 创建后再续租一次，覆盖慢盘/hook 场景，再 CAS 进 Running。
    if !state
        .orchestrator_repo
        .touch_preparing_lease(&task.id, &claim_token)
        .await?
    {
        return state.orchestrator_repo.get_task(&task.id).await;
    }

    let running_task = state
        .orchestrator_repo
        .mark_task_running_attempt(
            &task.id,
            &branch_name,
            &worktree.id,
            &session.id,
            attempt,
            &claim_token,
            agent_session_id.as_deref(),
            &runner_policy,
        )
        .await?;
    if running_task.status != OrchestratorTaskStatus::Running {
        return Ok(running_task);
    }

    let mut running_task = state
        .orchestrator_repo
        .update_active_runner_attempt_phase(
            &task.id,
            attempt,
            &session.id,
            OrchestratorAttemptPhase::BuildingPrompt,
        )
        .await?;
    if !active_runner_matches(&running_task, attempt, &session.id) {
        return Ok(running_task);
    }

    let prompt = if prompt.trim().is_empty() {
        let workflow = resolve_project_workflow(Path::new(&project.path))?;
        let prompt_task = PromptTaskContext {
            title: task.title.clone(),
            goal: task.goal.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
        };
        workflow.render_runner_prompt(&prompt_task, attempt, &worktree.path)?
    } else {
        prompt
    };
    if let Some(current) =
        stop_runner_input_if_task_changed(state, &task.id, attempt, &session.id).await?
    {
        return Ok(current);
    }
    state
        .orchestrator_repo
        .add_attempt(
            &task.id,
            attempt,
            &worktree.id,
            &session.id,
            &prompt,
            &runner_policy,
            agent_session_id.as_deref(),
            OrchestratorAttemptStatus::Running,
        )
        .await?;
    if let Some(current) =
        stop_runner_input_if_task_changed(state, &task.id, attempt, &session.id).await?
    {
        return Ok(current);
    }
    let attempt_evidence =
        development_attempt_evidence(attempt, &branch_name, &worktree.id, &session.id);
    state
        .orchestrator_repo
        .add_evidence(
            &task.id,
            EVIDENCE_KIND_DEVELOPMENT_ATTEMPT,
            &attempt_evidence.title,
            attempt_evidence.summary,
            &attempt_evidence.content,
        )
        .await?;
    if let Some(current) =
        stop_runner_input_if_task_changed(state, &task.id, attempt, &session.id).await?
    {
        return Ok(current);
    }
    // claim 失配时绝不能向 terminal 注入任何 agent 输入（characterization）。
    // registry 必须带 owner generic allowlist，与 catalog/probe 一致。
    let adapter_registry = {
        let config = state
            .config
            .read()
            .map_err(|_| AppError::generic("读取 AppConfig 失败（锁损坏）"))?;
        AgentAdapterRegistry::from_app_config(&config)
    };
    let adapter = adapter_registry.get(runner_policy.provider)?;
    let launch_request = AgentLaunchRequest {
        agent_session_id: agent_session_id.clone().unwrap_or_default(),
        terminal_session_id: session.id.clone(),
        cwd: worktree.path.clone(),
        prompt: prompt.clone(),
        native_session_id: None,
        max_turns: runner_policy.max_turns.clamp(1, 20) as u32,
        stall_timeout_ms: runner_policy.stall_timeout_ms.max(0) as u64,
    };
    let launch_plan = adapter.build_launch_plan(&launch_request)?;
    if let Some(current) =
        stop_runner_input_if_task_changed(state, &task.id, attempt, &session.id).await?
    {
        return Ok(current);
    }
    let dialect = TerminalShellDialect::from_command(&session.command);
    let terminal_input = render_terminal_command(&launch_plan, dialect)?;
    local_write_workbench_session_input(state, session.id.clone(), terminal_input).await?;

    running_task = state
        .orchestrator_repo
        .update_active_runner_attempt_phase(
            &task.id,
            attempt,
            &session.id,
            OrchestratorAttemptPhase::Streaming,
        )
        .await?;
    if let Some(current) =
        stop_runner_input_if_task_changed(state, &task.id, attempt, &session.id).await?
    {
        return Ok(current);
    }
    let runtime_started_at = running_task.runtime_started_at.clone();
    if let Some(runtime_task) = associate_runner_runtime(
        state,
        &task.id,
        attempt,
        &session.id,
        &worktree.path,
        runtime_started_at.as_deref(),
    )
    .await?
    {
        running_task = runtime_task;
    }

    // A4：prepare + adapter launch 全部成功后才把 experiment candidate 标 Running。
    if running_task.experiment_id.is_some()
        && running_task.status == OrchestratorTaskStatus::Running
    {
        if let Err(err) = crate::orchestrator::experiments::reducer::mark_candidate_running(
            state.orchestrator_repo.as_ref(),
            &running_task.id,
        )
        .await
        {
            tracing::debug!(
                task_id = %running_task.id,
                "mark_candidate_running 失败（best-effort）: {err}"
            );
        }
    }

    Ok(running_task)
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code visible Runner 启动后，任务详情应尽量展示 Claude 自身 session/transcript 信息；但关联失败不能影响执行。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中用 home 目录扫描 Claude JSONL transcript，匹配 worktree cwd 后按 active runner guard 写 runtime summary；
///     只接受本轮 runtime_started_at 之后的 transcript；transcript 可能晚于 terminal input 创建，因此做短时间 bounded retry。
async fn associate_runner_runtime(
    state: &AppState,
    task_id: &str,
    attempt: i64,
    session_id: &str,
    worktree_path: &str,
    runtime_started_at: Option<&str>,
) -> Result<Option<OrchestratorTaskRow>, AppError> {
    let home_dir = dirs::home_dir();
    let worktree_path = worktree_path.to_string();
    let runtime_started_at = runtime_started_at.map(str::to_string);
    for scan_attempt in 1..=RUNTIME_ASSOCIATION_ATTEMPTS {
        let home_dir = home_dir.clone();
        let scan_worktree_path = worktree_path.clone();
        let scan_runtime_started_at = runtime_started_at.clone();
        let scan_result = tauri::async_runtime::spawn_blocking(move || {
            associate_claude_runtime(
                home_dir.as_deref(),
                &scan_worktree_path,
                scan_runtime_started_at.as_deref(),
            )
        })
        .await;

        match scan_result {
            Ok(Ok(Some(summary))) => {
                return state
                    .orchestrator_repo
                    .update_active_runner_runtime_summary(task_id, attempt, session_id, &summary)
                    .await;
            }
            Ok(Ok(None)) if scan_attempt < RUNTIME_ASSOCIATION_ATTEMPTS => {
                tokio::time::sleep(RUNTIME_ASSOCIATION_RETRY_DELAY).await;
            }
            Ok(Ok(None)) => {
                tracing::debug!(
                    task_id = task_id,
                    worktree_path = worktree_path,
                    attempts = scan_attempt,
                    "未找到可关联的 Claude Code runtime transcript"
                );
                return Ok(None);
            }
            Ok(Err(error)) => {
                tracing::debug!(
                    task_id = task_id,
                    worktree_path = worktree_path,
                    error = %error,
                    "关联 Claude Code runtime 失败，继续保留 Runner 执行"
                );
                return Ok(None);
            }
            Err(error) => {
                tracing::debug!(
                    task_id = task_id,
                    worktree_path = worktree_path,
                    error = %error,
                    "关联 Claude Code runtime blocking task 失败，继续保留 Runner 执行"
                );
                return Ok(None);
            }
        }
    }

    Ok(None)
}

/// Business Logic（为什么需要这个函数）:
///     Runner 挂账为 Running 后用户仍可能立即 Abort；此时不能继续写 attempt 或向 terminal 注入 Claude prompt。
///
/// Code Logic（这个函数做什么）:
///     重读任务并校验 status/attempt/session 仍是当前 runner；失配时返回当前任务，命中时返回 None 允许继续。
async fn stop_runner_input_if_task_changed(
    state: &AppState,
    task_id: &str,
    attempt: i64,
    session_id: &str,
) -> Result<Option<OrchestratorTaskRow>, AppError> {
    let current = state.orchestrator_repo.get_task(task_id).await?;
    if active_runner_matches(&current, attempt, session_id) {
        Ok(None)
    } else {
        Ok(Some(current))
    }
}

/// Business Logic（为什么需要这个函数）:
///     active runner 的状态校验需要在多个 side effect 前复用，避免不同调用点条件不一致。
///
/// Code Logic（这个函数做什么）:
///     判断任务仍为 Running，attempt 相同，且 session_id 仍指向本次刚创建的 terminal session。
fn active_runner_matches(task: &OrchestratorTaskRow, attempt: i64, session_id: &str) -> bool {
    task.status == OrchestratorTaskStatus::Running
        && task.attempt == attempt
        && task.session_id.as_deref() == Some(session_id)
}

/// Business Logic（为什么需要这个函数）:
///     每轮开发 Claude 启动都需要可审计 evidence，方便前端展示 attempt 历史并定位 active session。
///
/// Code Logic（这个函数做什么）:
///     使用 attempt/branch/worktree/session 生成 developmentAttempt evidence 文本，不复制完整 prompt。
fn development_attempt_evidence(
    attempt: i64,
    branch_name: &str,
    worktree_id: &str,
    session_id: &str,
) -> DevelopmentAttemptEvidence {
    DevelopmentAttemptEvidence {
        title: format!("开发尝试 {attempt}"),
        summary: "running",
        content: format!(
            "attempt: {attempt}\nbranch: {branch_name}\nworktreeId: {worktree_id}\nsessionId: {session_id}"
        ),
    }
}

/// Business Logic（为什么需要这个函数）:
///     Blocked 任务 retry 后再次被 scheduler 领取时，不能重复使用已经写入 attempt history 的轮次号。
///
/// Code Logic（这个函数做什么）:
///     task.attempt<=0 说明首轮尚未成功挂账，返回 1；已有 attempt 且有 worktree 时返回 attempt+1；
///     已有 attempt 但缺少 worktree 视为现场不完整并返回业务错误。
fn initial_runner_attempt(task: &OrchestratorTaskRow) -> Result<i64, AppError> {
    if task.attempt <= 0 {
        return Ok(1);
    }
    if task
        .worktree_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(task.attempt + 1);
    }
    Err(AppError::generic(
        "任务已有 attempt 但缺少 worktree，无法继续调度",
    ))
}

/// Business Logic（为什么需要这个函数）:
///     Runner attempt 的 worktree 策略必须稳定：首轮新建，修复轮复用，避免修复丢失上一轮文件改动。
///
/// Code Logic（这个函数做什么）:
///     attempt=1 返回 None 表示需要新建 worktree；attempt>1 返回 task.worktree_id，缺失时给业务错误。
fn worktree_seed_for_attempt(
    task: &OrchestratorTaskRow,
    attempt: i64,
) -> Result<Option<String>, AppError> {
    if attempt <= 0 {
        return Err(AppError::generic("任务尝试轮次必须大于 0"));
    }
    if attempt == 1 {
        return Ok(None);
    }
    task.worktree_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| AppError::generic("修复轮次缺少已有 worktree，无法继续执行"))
}

/// Business Logic（为什么需要这个函数）:
///     prepare_runner_attempt 需要把 attempt worktree 选择转换为可创建 session 和生成 prompt 的 id/path。
///
/// Code Logic（这个函数做什么）:
///     首轮调用 Workbench helper 创建新 worktree；修复轮按 task.worktree_id 读取已有 worktree row 并投影为 RunnerWorktree。
async fn prepare_worktree_for_attempt(
    state: &AppState,
    task: &OrchestratorTaskRow,
    branch_name: &str,
    attempt: i64,
) -> Result<RunnerWorktree, AppError> {
    match worktree_seed_for_attempt(task, attempt)? {
        None => {
            let worktree = local_create_workbench_worktree(
                state,
                task.project_id.clone(),
                branch_name.to_string(),
                None,
            )
            .await?;
            Ok(RunnerWorktree {
                id: worktree.id,
                path: worktree.path,
            })
        }
        Some(worktree_id) => {
            let worktree = state
                .workbench_worktree_repo
                .get(&worktree_id)
                .await?
                .ok_or_else(|| {
                    AppError::not_found(format!("任务 worktree 不存在: {worktree_id}"))
                })?;
            Ok(RunnerWorktree {
                id: worktree.id,
                path: worktree.path,
            })
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     每个任务都需要稳定、可追溯且 Git branch 安全的分支名，便于 Workbench 创建独立 worktree。
///
/// Code Logic（这个函数做什么）:
///     使用 `agent/<task-id-short>-<slug-title>` 格式，清洗标题并把总长度限制在 80 字符内。
pub(crate) fn task_branch_name(task_id: &str, title: &str) -> String {
    let task_id_short = task_id_short(task_id);
    let slug = slug_title(title);
    let prefix = format!("{TASK_BRANCH_PREFIX}/{task_id_short}-");
    let slug_len = TASK_BRANCH_MAX_LEN.saturating_sub(prefix.len()).max(1);
    format!("{prefix}{}", truncate_slug(&slug, slug_len))
}

/// Business Logic（为什么需要这个函数）:
///     任务 id 通常是 UUID，分支名只需要短前缀即可定位任务，同时避免分支名过长。
///
/// Code Logic（这个函数做什么）:
///     保留 task_id 中前 8 个 ASCII 字母数字字符；若清洗后为空则回退为 `task`。
fn task_id_short(task_id: &str) -> String {
    let value: String = task_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if value.is_empty() {
        "task".to_string()
    } else {
        value
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户标题可能包含空格、中文、标点或 Git branch 不安全字符，需要归一为可用于分支的 slug。
///
/// Code Logic（这个函数做什么）:
///     ASCII 字母数字转小写保留，其它字符折叠为单个 `-`，首尾分隔符移除，空结果回退为 `task`。
fn slug_title(title: &str) -> String {
    let mut output = String::new();
    let mut previous_dash = false;
    for ch in title.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }
    let trimmed = output.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     长标题应尽量保留开头语义，但不能让分支名超过本项目约定长度。
///
/// Code Logic（这个函数做什么）:
///     截取 slug 的前 max_len 个字符并移除尾部 `-`，若截断后为空则回退为 `task`。
fn truncate_slug(slug: &str, max_len: usize) -> String {
    let truncated: String = slug.chars().take(max_len).collect();
    let trimmed = truncated.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed.to_string()
    }
}

/// OpenCode 在创建 worktree/session 前的门槛。
///
/// Business Logic（为什么需要这个函数）:
///     unopted / collision / unsupported CLI / 缺 L3 时必须稳定阻断，且不得创建 worktree/terminal。
///
/// Code Logic（这个函数做什么）:
///     委托 `opencode_preflight_block_reason`；Some(reason) 时调用方禁止 prepare_worktree。
fn open_code_preflight_gate(
    probe: &crate::orchestrator::agent_adapter::AgentProbeResult,
    bridge_preview: &crate::workbench::agent_runtime::opencode_bridge::OpenCodeBridgeOutcome,
) -> Result<(), &'static str> {
    match opencode_preflight_block_reason(probe, bridge_preview) {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// HookEvent-only provider 在 agent runtime 创建失败时必须 fail-closed。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode 无 Sentinel 降级；runtime 行缺失会使 OSC Completed 无人接收。
///
/// Code Logic（这个函数做什么）:
///     仅 `OpenCodeVisible` 返回 true。
fn should_fail_closed_on_agent_runtime_create(provider: AgentProviderId) -> bool {
    provider == AgentProviderId::OpenCodeVisible
}

/// Preparing 任务因 adapter/preflight 失败而 Blocked。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode/probe 失败必须在创建 worktree 前写稳定 reason + evidence。
///
/// Code Logic（这个函数做什么）:
///     CAS Preparing→Blocked + runnerAdapter evidence。
async fn block_preparing_for_adapter(
    state: &AppState,
    task_id: &str,
    provider: AgentProviderId,
    reason: &str,
) -> Result<OrchestratorTaskRow, AppError> {
    if let Ok(Some(blocked)) = state
        .orchestrator_repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Preparing,
            OrchestratorTaskStatus::Blocked,
            Some(reason),
        )
        .await
    {
        let _ = state
            .orchestrator_repo
            .add_evidence(
                task_id,
                "runnerAdapter",
                "Agent adapter unavailable",
                reason,
                &format!(
                    "provider={}\navailability=unavailable\nreason={reason}",
                    provider.as_str()
                ),
            )
            .await;
        return Ok(blocked);
    }
    state.orchestrator_repo.get_task(task_id).await
}

/// 带预分配 terminal/agent id 创建本机 Workbench session。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode bridge 要求 shell 启动前 `CC_PARTNER_AGENT_SESSION_ID` 已存在。
///
/// Code Logic（这个函数做什么）:
///     委托 `local_create_workbench_session_with_preallocated_ids`。
async fn local_create_workbench_session_with_preallocated_ids(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
    terminal_session_id: String,
    agent_session_id: String,
) -> Result<crate::workbench::models::WorkbenchSessionDto, AppError> {
    crate::commands::workbench::local_create_workbench_session_with_preallocated_ids(
        state,
        project_id,
        worktree_id,
        initial_cols,
        initial_rows,
        terminal_session_id,
        agent_session_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use crate::orchestrator::agent_adapter::{
        AgentAvailability, AgentProbeResult, AgentProviderId, REASON_EXTERNAL_COLLISION,
        REASON_L3_RUNTIME_EVIDENCE_MISSING, REASON_RUNTIME_BRIDGE_REQUIRED,
        REASON_UNSUPPORTED_CLI_VERSION,
    };
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
    use crate::workbench::agent_runtime::opencode_bridge::OpenCodeBridgeOutcome;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Business Logic（为什么需要这个函数）:
    ///     Runner attempt 规则测试需要完整任务 Row，避免只测试零散字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造带可选 worktree_id 和 attempt 的 Preparing 任务。
    fn runner_task_row(worktree_id: Option<&str>, attempt: i64) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Fix runner attempts".to_string(),
            goal: "goal".to_string(),
            acceptance_criteria: "criteria".to_string(),
            status: OrchestratorTaskStatus::Preparing,
            priority: 0,
            branch_name: Some("agent/task-1".to_string()),
            worktree_id: worktree_id.map(str::to_string),
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Preparing)
        }
    }

    /// 模拟 prepare_worktree/session 创建计数，证明 preflight 阻断后零调用。
    struct WorktreeTerminalCounters {
        worktree_creates: AtomicUsize,
        terminal_creates: AtomicUsize,
    }

    impl WorktreeTerminalCounters {
        fn new() -> Self {
            Self {
                worktree_creates: AtomicUsize::new(0),
                terminal_creates: AtomicUsize::new(0),
            }
        }

        /// Code Logic（这个函数做什么）:
        ///     与 prepare 路径相同：preflight gate 失败则不递增任何 create 计数。
        fn run_preflight_then_maybe_prepare(
            &self,
            probe: &AgentProbeResult,
            bridge: &OpenCodeBridgeOutcome,
        ) -> Result<(), &'static str> {
            super::open_code_preflight_gate(probe, bridge)?;
            self.worktree_creates.fetch_add(1, Ordering::SeqCst);
            self.terminal_creates.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn worktree_creates(&self) -> usize {
            self.worktree_creates.load(Ordering::SeqCst)
        }

        fn terminal_creates(&self) -> usize {
            self.terminal_creates.load(Ordering::SeqCst)
        }
    }

    fn available_probe() -> AgentProbeResult {
        AgentProbeResult {
            provider_id: AgentProviderId::OpenCodeVisible,
            availability: AgentAvailability::Available,
            executable: Some("opencode".into()),
            version: Some("1.0.0".into()),
            reason_code: None,
        }
    }

    fn unavailable_probe(reason: &str) -> AgentProbeResult {
        AgentProbeResult {
            provider_id: AgentProviderId::OpenCodeVisible,
            availability: AgentAvailability::Unavailable,
            executable: None,
            version: None,
            reason_code: Some(reason.into()),
        }
    }

    fn bridge_required() -> OpenCodeBridgeOutcome {
        OpenCodeBridgeOutcome::RuntimeBridgeRequired {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
        }
    }

    fn bridge_collision() -> OpenCodeBridgeOutcome {
        OpenCodeBridgeOutcome::ExternalCollision {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
            absolute_path: "/tmp/x".into(),
            current_hash: Some("def".into()),
        }
    }

    fn bridge_verified() -> OpenCodeBridgeOutcome {
        OpenCodeBridgeOutcome::Verified {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
            absolute_path: "/tmp/x".into(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 自动创建的任务分支必须是 Git branch 安全字符串，避免标题中的空格和标点
    ///     破坏 `git worktree add -b`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用含空格、标点和大小写的标题生成分支名，并断言前缀、slug 与非法字符清理结果。
    #[test]
    fn task_branch_name_sanitizes_title() {
        let branch =
            super::task_branch_name("550e8400-e29b-41d4-a716-446655440000", "Fix: UI / Login!");

        assert_eq!(branch, "agent/550e8400-fix-ui-login");
        assert!(!branch.contains(' '));
        assert!(!branch.contains(':'));
        assert!(!branch.contains('!'));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     长任务标题不能生成过长分支名，否则不同 Git 平台或本地路径可能出现不可操作的分支/目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用超长标题生成分支名，断言总长度被限制且尾部不会保留分隔符。
    #[test]
    fn task_branch_name_truncates_long_title() {
        let long_title =
            "Implement very long orchestrator visible workbench runner title ".repeat(4);
        let branch = super::task_branch_name("task-abcdef", &long_title);

        assert!(branch.len() <= 80);
        assert!(!branch.ends_with('-'));
        assert!(branch.starts_with("agent/taskabcd-"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     首轮 Runner 必须创建新 worktree，而修复轮必须复用任务已有 worktree，避免修复在新目录里丢失上一轮改动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 attempt worktree 选择 helper，断言 attempt=1 返回 None，attempt>1 返回已有 worktree。
    #[test]
    fn runner_attempt_worktree_seed_reuses_existing_worktree_after_first_attempt() {
        let first = runner_task_row(None, 0);
        let repair = runner_task_row(Some("worktree-1"), 1);

        assert_eq!(super::worktree_seed_for_attempt(&first, 1).unwrap(), None);
        assert_eq!(
            super::worktree_seed_for_attempt(&repair, 2).unwrap(),
            Some("worktree-1".to_string())
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     修复轮缺少 task.worktree_id 时不能悄悄创建新 worktree，否则验证/交付会脱离上一轮现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 attempt>1 且没有 worktree_id 的任务调用 helper，断言返回中文业务错误。
    #[test]
    fn runner_attempt_worktree_seed_requires_worktree_after_first_attempt() {
        let task = runner_task_row(None, 1);

        let error = super::worktree_seed_for_attempt(&task, 2).expect_err("missing worktree");

        assert!(error.to_string().contains("worktree"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Blocked 任务 retry 后会保留已有 worktree 与上一轮 attempt，再次调度必须使用下一轮 attempt 避免唯一约束冲突。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 bootstrap 前 attempt=0 仍选择首轮，已有 worktree 的 retry 选择 attempt+1，数据不完整时返回业务错误。
    #[test]
    fn initial_runner_attempt_selects_next_attempt_for_retry_with_worktree() {
        let bootstrap = runner_task_row(None, 0);
        let retry = runner_task_row(Some("worktree-1"), 1);
        let invalid_retry = runner_task_row(None, 1);

        assert_eq!(super::initial_runner_attempt(&bootstrap).unwrap(), 1);
        assert_eq!(super::initial_runner_attempt(&retry).unwrap(), 2);
        let error = super::initial_runner_attempt(&invalid_retry).expect_err("missing worktree");
        assert!(error.to_string().contains("worktree"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     claim token 取消后不得向 terminal 写任何 adapter 输入。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 active_runner_matches 在 Abort 时为 false（输入路径前置守卫）。
    #[test]
    fn claim_token_cancellation_blocks_terminal_input_guard() {
        let mut task = runner_task_row(Some("worktree-1"), 1);
        task.status = OrchestratorTaskStatus::Running;
        task.session_id = Some("session-1".to_string());
        assert!(super::active_runner_matches(&task, 1, "session-1"));
        task.status = OrchestratorTaskStatus::Aborted;
        assert!(!super::active_runner_matches(&task, 1, "session-1"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 挂账后若用户 Abort 或 active session 被替换，旧异步流程不能继续写 terminal 启动 Claude。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Running/Aborted/session mismatch 三种任务行，断言 active runner 守卫只接受完全匹配的当前 runner。
    #[test]
    fn active_runner_matches_requires_running_attempt_and_session() {
        let mut task = runner_task_row(Some("worktree-1"), 2);
        task.status = OrchestratorTaskStatus::Running;
        task.session_id = Some("session-1".to_string());

        assert!(super::active_runner_matches(&task, 2, "session-1"));
        assert!(!super::active_runner_matches(&task, 3, "session-1"));
        assert!(!super::active_runner_matches(&task, 2, "session-2"));

        task.status = OrchestratorTaskStatus::Aborted;
        assert!(!super::active_runner_matches(&task, 2, "session-1"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     前端 attempt 历史依赖 developmentAttempt evidence，title/summary/content 契约必须稳定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 attempt evidence，断言标题、summary 和 session/worktree 内容可被详情页展示。
    #[test]
    fn development_attempt_evidence_records_runner_context() {
        let evidence =
            super::development_attempt_evidence(3, "agent/task-fix", "worktree-1", "session-1");

        assert_eq!(evidence.title, "开发尝试 3");
        assert_eq!(evidence.summary, "running");
        assert!(evidence.content.contains("branch: agent/task-fix"));
        assert!(evidence.content.contains("worktreeId: worktree-1"));
        assert!(evidence.content.contains("sessionId: session-1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     unopted project（runtimeBridgeRequired）不得创建 worktree/terminal。
    ///
    /// Code Logic（这个测试做什么）:
    ///     preflight gate Err → counters 保持 0。
    #[test]
    fn opencode_preflight_unopted_creates_no_worktree_or_terminal() {
        let counters = WorktreeTerminalCounters::new();
        let err = counters
            .run_preflight_then_maybe_prepare(&available_probe(), &bridge_required())
            .expect_err("unopted must block");
        assert_eq!(err, REASON_RUNTIME_BRIDGE_REQUIRED);
        assert_eq!(counters.worktree_creates(), 0);
        assert_eq!(counters.terminal_creates(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     bridge collision 必须 fail-closed 且零 worktree/terminal。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ExternalCollision → counters 0。
    #[test]
    fn opencode_preflight_collision_creates_no_worktree_or_terminal() {
        let counters = WorktreeTerminalCounters::new();
        let err = counters
            .run_preflight_then_maybe_prepare(&available_probe(), &bridge_collision())
            .expect_err("collision must block");
        assert_eq!(err, REASON_EXTERNAL_COLLISION);
        assert_eq!(counters.worktree_creates(), 0);
        assert_eq!(counters.terminal_creates(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     unsupported CLI / 缺 L3 证据不得创建现场。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Unavailable probe 两种 reason → counters 0。
    #[test]
    fn opencode_preflight_unsupported_or_missing_l3_creates_no_worktree_or_terminal() {
        for reason in [
            REASON_UNSUPPORTED_CLI_VERSION,
            REASON_L3_RUNTIME_EVIDENCE_MISSING,
        ] {
            let counters = WorktreeTerminalCounters::new();
            let err = counters
                .run_preflight_then_maybe_prepare(&unavailable_probe(reason), &bridge_verified())
                .expect_err("unsupported/missing L3 must block");
            assert_eq!(err, reason);
            assert_eq!(counters.worktree_creates(), 0);
            assert_eq!(counters.terminal_creates(), 0);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preflight 通过后才允许 prepare 路径递增 worktree/terminal 计数。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Available + Verified → Ok 且 counters=1。
    #[test]
    fn opencode_preflight_pass_allows_worktree_and_terminal_create() {
        let counters = WorktreeTerminalCounters::new();
        counters
            .run_preflight_then_maybe_prepare(&available_probe(), &bridge_verified())
            .expect("verified bridge must pass");
        assert_eq!(counters.worktree_creates(), 1);
        assert_eq!(counters.terminal_creates(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     OpenCode runtime create 失败必须 fail-closed；Claude 可继续。
    ///
    /// Code Logic（这个测试做什么）:
    ///     should_fail_closed_on_agent_runtime_create 仅 OpenCodeVisible。
    #[test]
    fn opencode_runtime_create_failure_is_fail_closed_unlike_claude() {
        assert!(super::should_fail_closed_on_agent_runtime_create(
            AgentProviderId::OpenCodeVisible
        ));
        assert!(!super::should_fail_closed_on_agent_runtime_create(
            AgentProviderId::ClaudeCodeVisible
        ));
        assert!(!super::should_fail_closed_on_agent_runtime_create(
            AgentProviderId::CodexVisible
        ));
    }
}
