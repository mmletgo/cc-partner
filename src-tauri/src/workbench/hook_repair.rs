//! workbench/hook_repair.rs — pre-commit/pre-push 钩子失败的 AI 修复入口
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台 commit/push 因钩子失败时，前端收到 `failedHook` envelope 后调用本模块，在该 worktree
//!     的可见终端启动 Claude agent 修复根因。用户观察 agent 工作后手动重试 commit/push。
//!
//! Code Logic（这个模块做什么）:
//!     `repair_local_worktree_hook_failure` 创建一个绑定 worktree 的 terminal session 与一个
//!     workbench-scoped agent runtime 行（不创建 OrchestratorTask），probe adapter fail-closed，
//!     渲染 launch plan 并把修复 prompt 写入终端；返回 agent/terminal id 供前端聚焦。
//!
//! 完成/重试语义:
//!     ClaudeCodeVisible 的完成判定是 SentinelLine 且 task-scoped；task-less 的修复 agent 不会自动
//!     mark Completed，因此 V1 不做后端自动重试——由用户在终端观察后点「重试 commit/push」。
//!     自动重试需要扩展 sentinel 处理器支持 task-less agent，留作后续增量。

use crate::commands::workbench::{
    local_create_workbench_session, local_write_workbench_session_input,
};
use crate::error::AppError;
use crate::orchestrator::agent_adapter::{
    render_terminal_command, AgentAdapterRegistry, AgentAvailability, AgentLaunchRequest,
    AgentProviderId, TerminalShellDialect,
};
use crate::state::AppState;
use crate::workbench::agent_runtime::{
    emit_agent_runtime_changed, AgentRuntimeReducer, AgentSessionPhase, CreateActiveAgentSession,
};
use crate::workbench::operation_ledger::{WorkbenchHookFailureDto, WorkbenchHookStage};

/// 默认修复 agent 的 max_turns（与 orchestrator clamp 上限一致，给足修复余量）。
const HOOK_REPAIR_MAX_TURNS: u32 = 20;

/// 修复请求（前端 failedHook envelope 之后发起）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairHookFailureReq {
    pub worktree_id: String,
    /// failedHook envelope 携带的结构化钩子输出，原样回传供修复 prompt 引用。
    pub hook_failure: WorkbenchHookFailureDto,
}

/// 修复启动结果：前端据此聚焦终端并展示「重试」入口。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairHookFailureDto {
    pub agent_session_id: String,
    pub terminal_session_id: String,
    pub worktree_id: String,
    pub project_id: String,
}

/// 在 worktree 终端启动可见 Claude agent 修复钩子失败（owner 本机）。
///
/// Business Logic（为什么需要这个函数）:
///     failedHook envelope 把结构化钩子输出交给前端；用户点「让 AI 修复」后调用本函数。
///     只支持本机 worktree；远端 worktree 需 P2P 路由在对端设备执行同一入口（V1 不覆盖）。
///
/// Code Logic（这个函数做什么）:
///     require_owner → 解析 worktree + 校验本机项目 → 创建 terminal session → 创建 workbench-scoped
///     agent runtime(Launching) → probe adapter fail-closed → build_launch_plan → render terminal
///     command → write 输入；返回 agent/terminal id 供前端聚焦终端。
pub(crate) async fn repair_local_worktree_hook_failure(
    state: &AppState,
    req: RepairHookFailureReq,
) -> Result<RepairHookFailureDto, AppError> {
    state.runtime_role.require_owner()?;
    // stage 合法性校验（防止前端伪造任意值）。
    let stage = WorkbenchHookStage::parse(req.hook_failure.stage.as_str())?;
    let row = state
        .workbench_worktree_repo
        .get(&req.worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    let project = state
        .workbench_project_repo
        .get(&row.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind != "local" {
        return Err(AppError::generic(
            "钩子修复目前仅支持本机 worktree；远端 worktree 请在对端设备执行",
        ));
    }

    // 1) 在该 worktree 下创建一个 terminal session（绑定 worktree cwd）。
    let session = local_create_workbench_session(
        state,
        row.project_id.clone(),
        Some(row.id.clone()),
        Some(80),
        Some(24),
    )
    .await?;

    // 2) 创建 workbench-scoped agent runtime 行（无 OrchestratorTask），用于 Agent 统计/Attention/未来自动完成。
    let provider = AgentProviderId::ClaudeCodeVisible;
    let reducer = AgentRuntimeReducer::new((*state.workbench_agent_session_repo).clone());
    let now = chrono::Utc::now().to_rfc3339();
    let outcome = reducer
        .start_or_replace_active(CreateActiveAgentSession {
            id: None,
            project_id: row.project_id.clone(),
            worktree_id: Some(row.id.clone()),
            terminal_session_id: session.id.clone(),
            orchestrator_task_id: None,
            orchestrator_attempt: None,
            provider_id: provider.as_str().to_string(),
            native_session_id: None,
            phase: AgentSessionPhase::Launching,
            started_at: now,
            resumed_from_agent_session_id: None,
        })
        .await?;
    if let Some(ended) = &outcome.ended {
        emit_agent_runtime_changed(state, ended, None);
    }
    emit_agent_runtime_changed(state, &outcome.active, None);
    let agent_session_id = outcome.active.id.clone();

    // 3) probe adapter（fail-closed）：CLI 不可用时不注入 prompt，给出可操作错误。
    let adapter_registry = {
        let config = state
            .config
            .read()
            .map_err(|_| AppError::generic("读取 AppConfig 失败（锁损坏）"))?;
        AgentAdapterRegistry::from_app_config(&config)
    };
    let probe = adapter_registry.probe_cached(provider)?;
    if probe.availability != AgentAvailability::Available {
        return Err(AppError::generic(format!(
            "修复 Agent 不可用（{}），请先在系统设置中配置 Claude Code CLI 后重试",
            probe
                .reason_code
                .as_deref()
                .unwrap_or("provider_unavailable")
        )));
    }
    let adapter = adapter_registry.get(provider)?;

    // 4) 构造修复 prompt + launch plan + 写入终端。
    let prompt = build_hook_repair_prompt(stage, req.hook_failure.clone());
    let launch_request = AgentLaunchRequest {
        agent_session_id: agent_session_id.clone(),
        terminal_session_id: session.id.clone(),
        cwd: row.path.clone(),
        prompt,
        native_session_id: None,
        max_turns: HOOK_REPAIR_MAX_TURNS,
        stall_timeout_ms: 0,
    };
    let launch_plan = adapter.build_launch_plan(&launch_request)?;
    let dialect = TerminalShellDialect::from_command(&session.command);
    let terminal_input = render_terminal_command(&launch_plan, dialect)?;
    local_write_workbench_session_input(state, session.id.clone(), terminal_input).await?;

    tracing::info!(
        worktree_id = %row.id,
        agent_session_id = %agent_session_id,
        stage = stage.as_str(),
        "已启动钩子修复 agent"
    );

    Ok(RepairHookFailureDto {
        agent_session_id,
        terminal_session_id: session.id,
        worktree_id: row.id,
        project_id: row.project_id,
    })
}

/// 构造修复 prompt：注入钩子原始输出 + 硬约束（禁止 `--no-verify`、禁止 `git push`）。
///
/// Business Logic（为什么需要这个函数）:
///     agent 需要看到钩子真实输出才能定位根因；硬约束保证不绕过钩子、不替用户 push。
fn build_hook_repair_prompt(stage: WorkbenchHookStage, failure: WorkbenchHookFailureDto) -> String {
    let stage_word = match stage {
        WorkbenchHookStage::PreCommit => "pre-commit",
        WorkbenchHookStage::PrePush => "pre-push",
    };
    let output = failure.combined_output();
    let output_block = if output.trim().is_empty() {
        "(hook produced no captured output)".to_string()
    } else {
        output
    };
    // pre-push 场景：未推送的 commit 已存在，允许 amend/fixup 折叠修复（禁止 push / --no-verify）。
    let push_specific = match stage {
        WorkbenchHookStage::PrePush => indented_docs_push_note(),
        WorkbenchHookStage::PreCommit => String::new(),
    };
    format!(
        "The `{stage_word}` hook for this worktree just failed and blocked the `{blocked}`.\n\
         \n\
         Hook output (stderr/stdout):\n\
         -----\n\
         {output_block}\n\
         -----\n\
         \n\
         Your job: fix the ROOT CAUSE so the {stage_word} hook passes, then verify locally.\n\
         {push_specific}\
         \n\
         HARD RULES (violating any aborts the repair):\n\
         - NEVER pass `--no-verify` to git. The hook must pass for real.\n\
         - NEVER run `git push`. Only fix and verify locally; the workbench will retry the push.\n\
         - You MAY edit, create, or delete files in the working tree to make the hook pass.\n\
         - You MAY run the project's lint / format / typecheck / test commands and the hook itself to verify.\n\
         - Stay focused on the failing hook's signal. Do not refactor unrelated code or rewrite history beyond what the fix requires.\n\
         \n\
         When the hook passes (or you are certain you cannot fix it), stop.",
        stage_word = stage_word,
        blocked = match stage {
            WorkbenchHookStage::PreCommit => "git commit",
            WorkbenchHookStage::PrePush => "git push",
        },
        output_block = output_block,
        push_specific = push_specific,
    )
}

fn indented_docs_push_note() -> String {
    "Note: the commits being pushed already exist locally and are NOT yet on the remote. \
     If the fix must change committed content, you MAY amend the unpushed tip \
     (`git commit --amend --no-edit`) or add a fixup commit — that is safe because nothing has been pushed.\n\
     \n"
        .to_string()
}
