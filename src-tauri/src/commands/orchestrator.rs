use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorEvidenceDto, OrchestratorProjectConfigDto, OrchestratorTaskDto,
    OrchestratorTaskRow, OrchestratorTaskStatus,
};
use crate::orchestrator::outbox::{
    create_pending_remote_task, is_remote_network_error, mirror_payload_from_task,
    open_remote_project_for_shortcut, sync_remote_task_mirror_for_project,
    OrchestratorRemoteOutboxDto, RemoteMirrorTask,
};
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::{parse_remote_entity_id, remote_entity_id};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, State};
use uuid::Uuid;

/// 创建 Orchestrator 任务的命令入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端创建编排任务时只提交用户可编辑字段，后端统一补齐 id、状态、关联执行信息和时间戳。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 接收 Tauri invoke 参数，并保留 priority 可选值用于默认优先级归一。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskRequest {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: Option<i64>,
}

/// Orchestrator 任务视图 DTO。
///
/// Business Logic（为什么需要这个枚举）:
///     Phase 6 前端需要在同一个任务列表中展示本机任务、远端真实任务和本机 pending remote outbox。
///
/// Code Logic（这个枚举做什么）:
///     使用 serde tag=`origin` 输出 discriminated union；旧命令仍返回 OrchestratorTaskDto 保持兼容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "camelCase")]
pub enum OrchestratorTaskViewDto {
    Local {
        task: OrchestratorTaskDto,
    },
    Remote {
        task: OrchestratorTaskDto,
        device_id: String,
        device_name: String,
    },
    PendingRemote {
        item: OrchestratorRemoteOutboxDto,
    },
}

/// 构造待持久化的 Orchestrator 任务 Row。
///
/// Business Logic（为什么需要这个函数）:
///     创建任务时需要在命令层统一做必填校验、清理用户输入，并初始化任务为 Draft 状态。
///
/// Code Logic（这个函数做什么）:
///     校验 project/title/goal 非空，生成 UUID 和 UTC 时间戳，返回完整 OrchestratorTaskRow。
pub(crate) fn build_orchestrator_task_row(
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskRow, AppError> {
    let project_id = request.project_id.trim();
    let title = request.title.trim();
    let goal = request.goal.trim();
    let acceptance_criteria = request.acceptance_criteria.trim();

    if project_id.is_empty() {
        return Err(AppError::generic("项目不能为空"));
    }
    if title.is_empty() {
        return Err(AppError::generic("任务标题不能为空"));
    }
    if goal.is_empty() {
        return Err(AppError::generic("任务目标不能为空"));
    }

    let now = Utc::now().to_rfc3339();
    Ok(OrchestratorTaskRow {
        id: Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        title: title.to_string(),
        goal: goal.to_string(),
        acceptance_criteria: acceptance_criteria.to_string(),
        status: OrchestratorTaskStatus::Draft,
        priority: request.priority.unwrap_or(0),
        branch_name: None,
        worktree_id: None,
        session_id: None,
        blocked_reason: None,
        attempt: 0,
        created_at: now.clone(),
        updated_at: now,
        started_at: None,
        finished_at: None,
    })
}

/// Business Logic（为什么需要这个函数）:
///     remote-aware 命令需要先确认 projectId 对应的 Workbench 项目，才能分流 local 与 remote shortcut。
///
/// Code Logic（这个函数做什么）:
///     从 workbench_project_repo 读取项目；缺失时返回 not_found。
async fn get_orchestrator_workbench_project(
    state: &AppState,
    project_id: &str,
) -> Result<WorkbenchProjectRow, AppError> {
    state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))
}

/// Business Logic（为什么需要这个函数）:
///     远端 create 请求需要沿用用户输入的标题、目标、验收标准和优先级，但 projectId 会在远端 open-project 后替换。
///
/// Code Logic（这个函数做什么）:
///     从本地 Tauri create request 投影为 RemoteCreateOrchestratorTaskReq，queue 固定 false 保持草稿语义；
///     同时生成一次性稳定 clientRequestId，若在线响应超时后落入 pending outbox，后续投递仍复用该幂等键。
fn remote_create_request_from_local(
    request: &CreateOrchestratorTaskRequest,
) -> RemoteCreateOrchestratorTaskReq {
    RemoteCreateOrchestratorTaskReq {
        project_id: request.project_id.clone(),
        title: request.title.clone(),
        goal: request.goal.clone(),
        acceptance_criteria: request.acceptance_criteria.clone(),
        priority: request.priority.unwrap_or(0),
        queue: false,
        client_request_id: Some(Uuid::new_v4().to_string()),
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机任务视图需要统一包装成 discriminated union，供 Phase 6 UI 与远端视图合并展示。
///
/// Code Logic（这个函数做什么）:
///     把 OrchestratorTaskRow 转为 OrchestratorTaskDto 后包装为 Local 变体。
fn local_task_view(row: OrchestratorTaskRow) -> OrchestratorTaskViewDto {
    OrchestratorTaskViewDto::Local {
        task: OrchestratorTaskDto::from(row),
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端任务返回给本机前端时必须使用 Workbench remote id 通道，后续 queue/retry/abort/evidence 才能安全回到正确设备。
///
/// Code Logic（这个函数做什么）:
///     将远端裸 task/project/worktree/session id 映射为本机可识别的 remote:<deviceId>:<inner> id；
///     task.project_id 则替换成本机 remote shortcut project.id。
fn map_remote_task_for_shortcut(
    mut task: OrchestratorTaskDto,
    remote_shortcut: &WorkbenchProjectRow,
) -> OrchestratorTaskDto {
    task.id = remote_entity_id(&remote_shortcut.device_id, &task.id);
    task.project_id = remote_shortcut.id.clone();
    task.worktree_id = task
        .worktree_id
        .as_deref()
        .map(|id| remote_entity_id(&remote_shortcut.device_id, id));
    task.session_id = task
        .session_id
        .as_deref()
        .map(|id| remote_entity_id(&remote_shortcut.device_id, id));
    task
}

/// Business Logic（为什么需要这个函数）:
///     远端 evidence DTO 的 taskId 也会回传给前端，必须与任务详情页使用的 remote task id 保持一致。
///
/// Code Logic（这个函数做什么）:
///     克隆 evidence 列表并把每条 task_id 包装为 remote:<deviceId>:<inner>。
fn map_remote_evidence_for_shortcut(
    evidence: Vec<OrchestratorEvidenceDto>,
    remote_shortcut: &WorkbenchProjectRow,
) -> Vec<OrchestratorEvidenceDto> {
    evidence
        .into_iter()
        .map(|mut item| {
            item.task_id = remote_entity_id(&remote_shortcut.device_id, &item.task_id);
            item
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     远端任务视图必须带上设备 ID 与设备名，UI 才能标明任务由哪台设备自治执行。
///
/// Code Logic（这个函数做什么）:
///     先把远端裸任务 DTO 映射成本机 remote id，再与 remote shortcut 的设备信息包装为 Remote 变体。
fn remote_task_view(
    task: OrchestratorTaskDto,
    remote_shortcut: &WorkbenchProjectRow,
) -> OrchestratorTaskViewDto {
    OrchestratorTaskViewDto::Remote {
        task: map_remote_task_for_shortcut(task, remote_shortcut),
        device_id: remote_shortcut.device_id.clone(),
        device_name: remote_shortcut.device_name.clone(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机前端会把 remote:<deviceId>:<inner> task id 传回命令层，远端 HTTP API 只能接收 owning device 的裸 task id。
///
/// Code Logic（这个函数做什么）:
///     若 task_id 是 remote id，则校验 deviceId 与当前 shortcut 一致后返回 inner_id；裸 id 作为旧协议兼容直接返回。
fn remote_inner_task_id_for_shortcut(
    remote_shortcut: &WorkbenchProjectRow,
    task_id: &str,
) -> Result<String, AppError> {
    if let Some(parsed) = parse_remote_entity_id(task_id) {
        if parsed.device_id != remote_shortcut.device_id {
            return Err(AppError::generic("远端任务不属于当前设备"));
        }
        return Ok(parsed.inner_id);
    }
    Ok(task_id.to_string())
}

/// Business Logic（为什么需要这个函数）:
///     mirror cache 保存的是 JSON payload，命令返回前需要还原为任务 DTO 并附带远端设备信息。
///
/// Code Logic（这个函数做什么）:
///     逐条解析 RemoteMirrorTask.payload_json，转换为 Remote 视图；解析失败返回业务错误。
fn remote_mirror_views(
    mirrors: Vec<RemoteMirrorTask>,
    remote_shortcut: &WorkbenchProjectRow,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    mirrors
        .into_iter()
        .map(|mirror| {
            let task = serde_json::from_str::<OrchestratorTaskDto>(&mirror.payload_json)
                .map_err(|err| AppError::generic(format!("远端任务镜像解析失败: {err}")))?;
            Ok(remote_task_view(task, remote_shortcut))
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     远端项目列表需要同时显示未发送、发送中或已失败的 outbox item，避免用户误以为离线创建丢失。
///
/// Code Logic（这个函数做什么）:
///     从 repo 按 remote shortcut 的 device/path 读取 outbox 行，并包装为 PendingRemote 变体。
async fn pending_remote_task_views_for_project(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let items = state
        .orchestrator_repo
        .list_remote_outbox_items_for_project_path(
            &remote_shortcut.device_id,
            &remote_shortcut.path,
        )
        .await?;
    Ok(items
        .into_iter()
        .map(|item| OrchestratorTaskViewDto::PendingRemote {
            item: item.to_dto(),
        })
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     在线远端创建任务时，本机必须先确保远端项目打开，再把任务写到 owning device 的 Orchestrator 队列。
///
/// Code Logic（这个函数做什么）:
///     复用 Workbench remote open-project 规则取得远端 local projectId，调用 RemoteOrchestratorClient::create_task，
///     成功后 upsert mirror 并返回 Remote 视图。
async fn create_remote_orchestrator_task_online(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    mut request: RemoteCreateOrchestratorTaskReq,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let context = open_remote_project_for_shortcut(state, remote_shortcut).await?;
    request.project_id = context.remote_project_id.clone();
    let task = RemoteOrchestratorClient::new()
        .create_task(&context.base_url, request)
        .await?;
    upsert_remote_task_view(
        state,
        remote_shortcut,
        &context.remote_project_id,
        &context.remote_project_path,
        task,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     远端 queue/retry/abort 操作返回的最新任务状态也要刷新本机 mirror cache，保证离线前快照尽量新。
///
/// Code Logic（这个函数做什么）:
///     序列化任务 DTO 并按 `(device_id, remote_task_id)` upsert mirror，再返回 Remote 视图。
async fn upsert_remote_task_view(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    remote_project_id: &str,
    remote_project_path: &str,
    task: OrchestratorTaskDto,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let payload = mirror_payload_from_task(&task)?;
    state
        .orchestrator_repo
        .upsert_remote_task_mirror(
            &remote_shortcut.device_id,
            &remote_shortcut.device_name,
            remote_project_id,
            remote_project_path,
            &task.id,
            &payload,
        )
        .await?;
    Ok(remote_task_view(task, remote_shortcut))
}

/// Business Logic（为什么需要这个函数）:
///     远端任务状态命令共享同一套 open-project、远端 API 调用和 mirror upsert 逻辑，避免 queue/retry/abort 分叉。
///
/// Code Logic（这个函数做什么）:
///     打开远端项目后执行传入的 RemoteOrchestratorClient 方法，随后写 mirror 并返回 Remote 视图。
async fn update_remote_orchestrator_task_status<F, Fut>(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    task_id: &str,
    operation: F,
) -> Result<OrchestratorTaskViewDto, AppError>
where
    F: FnOnce(RemoteOrchestratorClient, String, String) -> Fut,
    Fut: std::future::Future<Output = Result<OrchestratorTaskDto, AppError>>,
{
    let context = open_remote_project_for_shortcut(state, remote_shortcut).await?;
    let remote_task_id = remote_inner_task_id_for_shortcut(remote_shortcut, task_id)?;
    let task = operation(
        RemoteOrchestratorClient::new(),
        context.base_url.clone(),
        remote_task_id,
    )
    .await?;
    upsert_remote_task_view(
        state,
        remote_shortcut,
        &context.remote_project_id,
        &context.remote_project_path,
        task,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     手动 dispatch 命令需要返回本次实际领取的任务数量，供 UI 和测试判断触发效果。
///
/// Code Logic（这个函数做什么）:
///     把 dispatched 数量包装成 `{ "dispatched": <usize> }` JSON Value。
fn build_dispatch_once_response(dispatched: usize) -> serde_json::Value {
    serde_json::json!({ "dispatched": dispatched })
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成后的验证命令已迁移为设备级全局设置，不能再读取 legacy 项目策略表。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorAutomationConfig 克隆 verification_commands，调用方可在 await 前释放 config 读锁。
fn verification_commands_for_agent_completion(
    config: &OrchestratorAutomationConfig,
) -> Vec<String> {
    config.verification_commands.clone()
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成按钮只能用于 Running 任务，避免草稿、排队、交付或终态任务被误推进验证阶段。
///
/// Code Logic（这个函数做什么）:
///     检查任务状态是否为 Running；不满足时返回中文业务错误。
#[cfg(test)]
fn ensure_task_can_complete_agent_run(task: &OrchestratorTaskRow) -> Result<(), AppError> {
    if task.status != OrchestratorTaskStatus::Running {
        return Err(AppError::generic(format!(
            "只有运行中的任务可以开始验证，当前状态为 {}",
            task.status.as_str()
        )));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Blocked UI 的重试按钮只应重新排队阻塞任务，避免运行中任务被强行回退。
///
/// Code Logic（这个函数做什么）:
///     当前状态为 Blocked 时返回 Queued；其它状态返回中文业务错误。
#[cfg(test)]
fn retry_orchestrator_task_target_status(
    status: OrchestratorTaskStatus,
) -> Result<OrchestratorTaskStatus, AppError> {
    if status == OrchestratorTaskStatus::Blocked {
        Ok(OrchestratorTaskStatus::Queued)
    } else {
        Err(AppError::generic(format!(
            "只有阻塞任务可以重试，当前状态为 {}",
            status.as_str()
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户终止任务时只需要进入 Aborted 状态，worktree/session 保留给用户人工处理。
///
/// Code Logic（这个函数做什么）:
///     返回固定 Aborted 目标状态，命令层负责持久化且不删除任何执行现场。
fn abort_orchestrator_task_target_status(
    _status: OrchestratorTaskStatus,
) -> OrchestratorTaskStatus {
    OrchestratorTaskStatus::Aborted
}

/// 验证 evidence 的写入参数。
///
/// Business Logic（为什么需要这个结构体）:
///     验证失败路径需要按同一 evidence 契约写入，避免前置失败和命令失败产生不同展示形态。
///
/// Code Logic（这个结构体做什么）:
///     保存 add_evidence 需要的 kind/title/summary/content 四个参数；静态字段用 &'static str，内容按 String 持有。
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationEvidence {
    kind: &'static str,
    title: &'static str,
    summary: &'static str,
    content: String,
}

/// 验证失败后需要落库的阻塞结果。
///
/// Business Logic（为什么需要这个结构体）:
///     任务进入 Verifying 后，任一可预期错误都必须同时产生 failed evidence 和 Blocked 原因，避免流程半停在 Verifying。
///
/// Code Logic（这个结构体做什么）:
///     保存最终 blocked_reason 与对应 verificationOutput evidence，便于 side-effect helper 统一落库。
#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationFailureOutcome {
    reason: String,
    evidence: VerificationEvidence,
}

/// 验证完成后进入交付阶段的条件转换结果。
///
/// Business Logic（为什么需要这个结构体）:
///     验证期间用户可能终止任务，命令层需要区分是否真正从 Verifying 取得 Delivering 执行权。
///
/// Code Logic（这个结构体做什么）:
///     transitioned=true 表示 Verifying->Delivering 原子转换命中；false 表示状态已变化，task 为当前最新任务。
struct ConditionalDeliveryTransition {
    transitioned: bool,
    task: OrchestratorTaskRow,
}

/// Business Logic（为什么需要这个函数）:
///     验证前置失败也必须落为 failed verificationOutput evidence，用户才能在任务详情看到可审计原因。
///
/// Code Logic（这个函数做什么）:
///     把失败原因转换为 add_evidence 的固定 kind/title/summary 参数与 content 文本。
fn verification_failure_evidence(reason: &str) -> VerificationEvidence {
    VerificationEvidence {
        kind: "verificationOutput",
        title: "验证命令",
        summary: "failed",
        content: reason.to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     已经进入 Verifying 的任务若遇到配置解析、worktree 读取或验证执行错误，需要统一生成 Blocked 原因与 failed evidence。
///
/// Code Logic（这个函数做什么）:
///     将业务上下文和 AppError 拼接成中文 reason，并复用 verification_failure_evidence 生成 evidence 内容。
fn verification_failure_outcome(context: &str, err: &AppError) -> VerificationFailureOutcome {
    let reason = format!("{context}: {err}");
    VerificationFailureOutcome {
        evidence: verification_failure_evidence(&reason),
        reason,
    }
}

/// Business Logic（为什么需要这个函数）:
///     缺少 worktree 这类已知失败原因没有底层错误对象，也要走同一 outcome 结构，保持 evidence/Blocked 契约一致。
///
/// Code Logic（这个函数做什么）:
///     直接把 reason 包装成 VerificationFailureOutcome，并让 evidence content 与 blocked_reason 完全一致。
fn verification_failure_outcome_from_reason(reason: &str) -> VerificationFailureOutcome {
    VerificationFailureOutcome {
        reason: reason.to_string(),
        evidence: verification_failure_evidence(reason),
    }
}

/// Business Logic（为什么需要这个函数）:
///     completion 流程需要确保 failed verification evidence 真正写入仓储，而不是只构造参数。
///
/// Code Logic（这个函数做什么）:
///     将 VerificationEvidence 拆成 OrchestratorRepo::add_evidence 参数并追加到当前任务。
async fn add_verification_evidence(
    state: &AppState,
    task_id: &str,
    evidence: &VerificationEvidence,
) -> Result<(), AppError> {
    state
        .orchestrator_repo
        .add_evidence(
            task_id,
            evidence.kind,
            evidence.title,
            evidence.summary,
            &evidence.content,
        )
        .await
}

/// Business Logic（为什么需要这个函数）:
///     验证失败、缺少 worktree 或找不到 worktree 时，任务应进入 Blocked 并保留明确原因。
///
/// Code Logic（这个函数做什么）:
///     仅当任务仍处于 Verifying 时写入 Blocked 和 blocked_reason，再转换为 DTO；已 Abort 时返回当前任务。
async fn block_task_with_reason(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .block_task_if_verifying(task_id, reason)
        .await?;
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     验证通过或跳过后只有仍处于 Verifying 的任务可以进入 Delivering，避免用户 Abort 被旧流程覆盖。
///
/// Code Logic（这个函数做什么）:
///     调用仓储 try_transition_task_status 做 Verifying->Delivering 条件写；未命中时读取当前任务并标记 skipped。
async fn transition_verified_task_to_delivering(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<ConditionalDeliveryTransition, AppError> {
    match repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Delivering,
            None,
        )
        .await?
    {
        Some(task) => Ok(ConditionalDeliveryTransition {
            transitioned: true,
            task,
        }),
        None => Ok(ConditionalDeliveryTransition {
            transitioned: false,
            task: repo.get_task(task_id).await?,
        }),
    }
}

/// Business Logic（为什么需要这个函数）:
///     命令层调用 delivery pipeline 时若遇到未被 pipeline 内部归一的错误，任务也不能永久停在 Delivering。
///
/// Code Logic（这个函数做什么）:
///     将错误转换为 delivery failed evidence + Blocked；状态已被 Abort 等流程改变时只返回当前任务，不覆盖。
async fn block_task_with_delivery_error(
    state: &AppState,
    task_id: &str,
    err: AppError,
) -> Result<OrchestratorTaskDto, AppError> {
    let reason = format!("自动交付失败: {err}");
    let task = state
        .orchestrator_repo
        .block_task_if_delivering(task_id, &reason)
        .await?;
    if task.status == OrchestratorTaskStatus::Blocked
        && task.blocked_reason.as_deref() == Some(reason.as_str())
    {
        state
            .orchestrator_repo
            .add_evidence(task_id, "delivery", "delivery", "failed", &reason)
            .await?;
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     验证完成后命令层需要运行自动交付，并对未预期错误做最终兜底。
///
/// Code Logic（这个函数做什么）:
///     构造 AppDeliveryContext 调用 delivery::deliver_task；成功返回 summary.task，失败时条件 Block 当前 Delivering 任务。
async fn run_delivery_for_task(
    state: &AppState,
    app_handle: AppHandle,
    task_id: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let delivery_context =
        crate::orchestrator::delivery::AppDeliveryContext::new(state, app_handle);
    match crate::orchestrator::delivery::deliver_task(&delivery_context, task_id).await {
        Ok(summary) => {
            tracing::debug!(task_id = %task_id, stages = ?summary.stages, "Orchestrator 自动交付完成");
            Ok(summary.task)
        }
        Err(err) => block_task_with_delivery_error(state, task_id, err).await,
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证命令无法启动或无法定位 worktree 时，任务既要 Blocked，也要保留 failed verification evidence。
///
/// Code Logic（这个函数做什么）:
///     先追加 outcome.evidence，再用 outcome.reason 写 Blocked，确保两处展示内容一致。
async fn block_task_with_verification_outcome(
    state: &AppState,
    task_id: &str,
    outcome: &VerificationFailureOutcome,
) -> Result<OrchestratorTaskDto, AppError> {
    add_verification_evidence(state, task_id, &outcome.evidence).await?;
    block_task_with_reason(state, task_id, &outcome.reason).await
}

/// Business Logic（为什么需要这个函数）:
///     验证命令无法启动或无法定位 worktree 时，任务既要 Blocked，也要保留 failed verification evidence。
///
/// Code Logic（这个函数做什么）:
///     把静态失败原因转换成 VerificationFailureOutcome，再复用统一 side-effect helper 落 evidence 和 Blocked。
async fn block_task_with_verification_failure(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let outcome = verification_failure_outcome_from_reason(reason);
    block_task_with_verification_outcome(state, task_id, &outcome).await
}

/// Business Logic（为什么需要这个函数）:
///     Verifying 后的 repo/config/delivery 错误都必须统一转成 failed evidence + Blocked，不能把错误直接抛出导致任务永久 Verifying。
///
/// Code Logic（这个函数做什么）:
///     使用 context + AppError 构造 VerificationFailureOutcome，再调用统一 side-effect helper 落库。
async fn block_task_with_verification_error(
    state: &AppState,
    task_id: &str,
    context: &str,
    err: AppError,
) -> Result<OrchestratorTaskDto, AppError> {
    let outcome = verification_failure_outcome(context, &err);
    block_task_with_verification_outcome(state, task_id, &outcome).await
}

/// 查询 Orchestrator 任务列表。
///
/// Business Logic（为什么需要这个函数）:
///     前端 Orchestrator 页面后续需要按项目读取任务队列，也需要支持全局列表调试和管理。
///
/// Code Logic（这个函数做什么）:
///     透传可选 project_id 给仓储 list_tasks，并把 Row 投影为 camelCase DTO。
#[tauri::command]
pub async fn list_orchestrator_tasks(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskDto>, AppError> {
    let rows = state
        .orchestrator_repo
        .list_tasks(project_id.as_deref())
        .await?;
    Ok(rows.into_iter().map(OrchestratorTaskDto::from).collect())
}

/// 创建 Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     用户在前端提交任务后，需要立即生成草稿任务并保存到 SQLite，供后续队列和调度器处理。
///
/// Code Logic（这个函数做什么）:
///     调用纯 helper 完成校验和 Row 初始化，再委托 OrchestratorRepo 插入并返回 DTO。
#[tauri::command]
pub async fn create_orchestrator_task(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskDto, AppError> {
    let row = build_orchestrator_task_row(request)?;
    state.orchestrator_repo.create_task(&row).await?;
    Ok(OrchestratorTaskDto::from(row))
}

/// 查询 remote-aware Orchestrator 任务视图列表。
///
/// Business Logic（为什么需要这个函数）:
///     Phase 6 前端需要在远端项目中展示远端真实任务、本机 pending outbox 和离线 mirror 快照。
///
/// Code Logic（这个函数做什么）:
///     local 项目读取本机任务并包装 Local；remote 项目在线时同步 mirror，离线时读最近 mirror；
///     最后追加 pending/sending/failed outbox 项。
#[tauri::command]
pub async fn list_orchestrator_task_views(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let Some(project_id) = project_id else {
        let rows = state.orchestrator_repo.list_tasks(None).await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    };

    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let rows = state
            .orchestrator_repo
            .list_tasks(Some(&project_id))
            .await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    }

    let mirrors = match sync_remote_task_mirror_for_project(state.inner(), &project).await {
        Ok(mirrors) => mirrors,
        Err(err) if is_remote_network_error(&err) => {
            state
                .orchestrator_repo
                .list_remote_task_mirrors_for_project_path(&project.device_id, &project.path)
                .await?
        }
        Err(err) => return Err(err),
    };
    let mut views = remote_mirror_views(mirrors, &project)?;
    views.extend(pending_remote_task_views_for_project(state.inner(), &project).await?);
    Ok(views)
}

/// 创建 remote-aware Orchestrator 任务视图。
///
/// Business Logic（为什么需要这个函数）:
///     local 项目应继续创建本机任务；remote 项目在线时创建远端权威任务，离线时写 pending outbox。
///
/// Code Logic（这个函数做什么）:
///     先按 projectId 读取 Workbench 项目；local 走旧 row builder + repo，remote 先尝试在线创建，
///     遇到网络/离线错误时创建 pending outbox 并返回 PendingRemote。
#[tauri::command]
pub async fn create_orchestrator_task_view(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &request.project_id).await?;
    if project.kind != "remote" {
        let row = build_orchestrator_task_row(request)?;
        state.orchestrator_repo.create_task(&row).await?;
        return Ok(local_task_view(row));
    }

    let remote_request = remote_create_request_from_local(&request);
    match create_remote_orchestrator_task_online(state.inner(), &project, remote_request.clone())
        .await
    {
        Ok(view) => Ok(view),
        Err(err) if is_remote_network_error(&err) => {
            let item = create_pending_remote_task(state.inner(), &project, remote_request).await?;
            Ok(OrchestratorTaskViewDto::PendingRemote {
                item: item.to_dto(),
            })
        }
        Err(err) => Err(err),
    }
}

/// 将 remote-aware Orchestrator 任务加入队列。
///
/// Business Logic（为什么需要这个函数）:
///     local 任务入队发生在本机；remote 任务入队必须转发给 owning device，pending item 不能被本机入队。
///
/// Code Logic（这个函数做什么）:
///     project 为 local 时复用 repo.queue_task；remote 时先识别 pending outbox id，否则调用远端 queue 并 upsert mirror。
#[tauri::command]
pub async fn queue_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let task = state.orchestrator_repo.queue_task(&task_id).await?;
        return Ok(local_task_view(task));
    }
    if let Some(item) = state
        .orchestrator_repo
        .get_remote_outbox_item(&task_id)
        .await?
    {
        return Ok(OrchestratorTaskViewDto::PendingRemote {
            item: item.to_dto(),
        });
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.queue_task(&base_url, &id).await },
    )
    .await
}

/// 重试 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的重试必须作用于远端权威任务，不能把本机 mirror 当作任务状态机处理。
///
/// Code Logic（这个函数做什么）:
///     local 项目复用 Blocked->Queued 原子转换；remote 项目调用 RemoteOrchestratorClient::retry_task 并刷新 mirror。
#[tauri::command]
pub async fn retry_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let updated = state
            .orchestrator_repo
            .transition_task_status(
                &task_id,
                OrchestratorTaskStatus::Blocked,
                OrchestratorTaskStatus::Queued,
                None,
            )
            .await?;
        return Ok(local_task_view(updated));
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.retry_task(&base_url, &id).await },
    )
    .await
}

/// 终止 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 上的 Abort 必须终止 owning device 上的真实任务，并保留远端现场。
///
/// Code Logic（这个函数做什么）:
///     local 项目复用 set_task_status；remote 项目调用 RemoteOrchestratorClient::abort_task 并刷新 mirror。
#[tauri::command]
pub async fn abort_orchestrator_task_view(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let task = state.orchestrator_repo.get_task(&task_id).await?;
        let target = abort_orchestrator_task_target_status(task.status);
        let updated = state
            .orchestrator_repo
            .set_task_status(&task.id, target, None)
            .await?;
        return Ok(local_task_view(updated));
    }
    update_remote_orchestrator_task_status(
        state.inner(),
        &project,
        &task_id,
        |client, base_url, id| async move { client.abort_task(&base_url, &id).await },
    )
    .await
}

/// 按项目读取 remote-aware Orchestrator evidence。
///
/// Business Logic（为什么需要这个函数）:
///     remote task 详情需要展示 owning device 上真实写入的 evidence；local 任务继续读本机 SQLite。
///
/// Code Logic（这个函数做什么）:
///     local 项目走 repo.list_evidence；remote 项目打开远端项目后调用 RemoteOrchestratorClient::get_evidence。
#[tauri::command]
pub async fn list_orchestrator_task_evidence_for_project(
    state: State<'_, AppState>,
    project_id: String,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        return state.orchestrator_repo.list_evidence(&task_id).await;
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project).await?;
    let remote_task_id = remote_inner_task_id_for_shortcut(&project, &task_id)?;
    let evidence = RemoteOrchestratorClient::new()
        .get_evidence(&context.base_url, &remote_task_id)
        .await?;
    Ok(map_remote_evidence_for_shortcut(evidence, &project))
}

/// 按项目读取 remote-aware Orchestrator 自动化配置。
///
/// Business Logic（为什么需要这个函数）:
///     remote 项目展示的自动化策略应来自远端设备 Settings，而不是本机 shortcut 的配置。
///
/// Code Logic（这个函数做什么）:
///     local 项目返回本机 AppConfig.orchestrator DTO；remote 项目打开远端后调用远端 config endpoint。
#[tauri::command]
pub async fn get_orchestrator_config_for_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorAutomationConfigDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    if project.kind != "remote" {
        let config = state
            .config
            .read()
            .expect("config 读锁中毒")
            .orchestrator
            .clone();
        return Ok(config.into());
    }
    let context = open_remote_project_for_shortcut(state.inner(), &project).await?;
    RemoteOrchestratorClient::new()
        .get_config(&context.base_url)
        .await
}

/// 查询项目 Orchestrator 策略。
///
/// Business Logic（为什么需要这个函数）:
///     legacy 策略卡仍可展示/调试当前 Workbench 项目的旧自动化策略，缺失时按默认策略初始化。
///     Phase 3 后 scheduler、验证和 delivery 运行时统一读取 AppConfig.orchestrator。
///
/// Code Logic（这个函数做什么）:
///     委托仓储 get_or_create_project_config，并返回 camelCase DTO。
#[tauri::command]
pub async fn get_orchestrator_project_config(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorProjectConfigDto, AppError> {
    state
        .orchestrator_repo
        .get_or_create_project_config(&project_id)
        .await
}

/// 查询 Orchestrator 任务证据列表。
///
/// Business Logic（为什么需要这个函数）:
///     任务详情右侧 evidence 卡需要读取当前任务的验证输出与交付证据。
///
/// Code Logic（这个函数做什么）:
///     透传 task_id 给仓储 list_evidence，并返回 camelCase DTO 列表。
#[tauri::command]
pub async fn list_orchestrator_task_evidence(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
    state.orchestrator_repo.list_evidence(&task_id).await
}

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
    app_handle: AppHandle,
) -> Result<serde_json::Value, AppError> {
    let dispatched =
        crate::orchestrator::scheduler::dispatch_once(state.inner(), app_handle).await?;
    Ok(build_dispatch_once_response(dispatched))
}

/// 标记 Agent 已完成并执行验证命令。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 Workbench 中看到 Claude Code 完成后，需要从 Orchestrator 触发项目验证，并把输出归档为 evidence。
///
/// Code Logic（这个函数做什么）:
///     用 expected-status 原子转移执行 Running->Verifying；之后读取项目验证命令和 worktree cwd，执行验证；
///     Verifying 后的可预期错误统一写 failed evidence 并置 Blocked，成功写 passed/skipped evidence 并推进 Delivering；
///     随后立即调用 delivery pipeline，返回最终 Done 或 Blocked 任务 DTO。
#[tauri::command]
pub async fn complete_orchestrator_agent_run(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .transition_task_status(
            &task_id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Verifying,
            None,
        )
        .await?;

    let Some(worktree_id) = task.worktree_id.as_deref() else {
        return block_task_with_verification_failure(
            state.inner(),
            &task.id,
            "任务缺少 worktree，无法运行验证命令。",
        )
        .await;
    };

    let worktree = match state.workbench_worktree_repo.get(worktree_id).await {
        Ok(Some(worktree)) => worktree,
        Ok(None) => {
            return block_task_with_verification_failure(
                state.inner(),
                &task.id,
                &format!("找不到任务 worktree: {worktree_id}"),
            )
            .await;
        }
        Err(err) => {
            return block_task_with_verification_error(
                state.inner(),
                &task.id,
                "读取任务 worktree 失败",
                err,
            )
            .await;
        }
    };
    let cwd = PathBuf::from(&worktree.path);
    let verification_commands = {
        let config = state.config.read().expect("config 读锁中毒");
        verification_commands_for_agent_completion(&config.orchestrator)
    };

    if verification_commands.is_empty() {
        state
            .orchestrator_repo
            .add_evidence(
                &task.id,
                "verificationOutput",
                "验证命令",
                "skipped",
                "未配置验证命令，跳过验证。",
            )
            .await?;
        let delivery_transition =
            transition_verified_task_to_delivering(state.orchestrator_repo.as_ref(), &task.id)
                .await?;
        if !delivery_transition.transitioned {
            return Ok(OrchestratorTaskDto::from(delivery_transition.task));
        }
        return run_delivery_for_task(state.inner(), app_handle, &delivery_transition.task.id)
            .await;
    }

    match crate::orchestrator::delivery::run_verification_commands(&cwd, &verification_commands)
        .await
    {
        Ok(output) => {
            state
                .orchestrator_repo
                .add_evidence(
                    &task.id,
                    "verificationOutput",
                    "验证命令",
                    "passed",
                    &output,
                )
                .await?;
            let delivery_transition =
                transition_verified_task_to_delivering(state.orchestrator_repo.as_ref(), &task.id)
                    .await?;
            if !delivery_transition.transitioned {
                return Ok(OrchestratorTaskDto::from(delivery_transition.task));
            }
            run_delivery_for_task(state.inner(), app_handle, &delivery_transition.task.id).await
        }
        Err(err) => {
            block_task_with_verification_error(state.inner(), &task.id, "验证失败", err).await
        }
    }
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
///     用户需要从 blocked UI 或队列中终止不再继续的任务，同时保留现场用于人工检查。
///
/// Code Logic（这个函数做什么）:
///     将任务状态设置为 Aborted，清空 blocked_reason，不删除 worktree/session。
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
    Ok(OrchestratorTaskDto::from(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorAutomationConfig;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     命令层测试需要 Orchestrator 仓储验证真实 SQLite 条件更新语义，避免只测纯函数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接内存 SQLite、初始化 Orchestrator schema，并返回可供命令 helper 使用的 repo。
    async fn setup_orchestrator_repo() -> OrchestratorRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite options")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("sqlite pool");
        OrchestratorRepo::init_schema(&pool)
            .await
            .expect("orchestrator schema");
        OrchestratorRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层状态转换测试需要稳定构造 Verifying/Aborted 任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回字段完整的测试任务 Row，调用方传入 id 和 status，其它字段使用固定值。
    fn command_task_row(id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: "project-1".to_string(),
            title: format!("Task {id}"),
            goal: "goal".to_string(),
            acceptance_criteria: "criteria".to_string(),
            status,
            priority: 0,
            branch_name: None,
            worktree_id: Some("worktree-1".to_string()),
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remote-aware helper 测试需要一个本机 remote shortcut，模拟前端当前打开的远端项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造完整 WorkbenchProjectRow，id 是本机 shortcut projectId，device_id 是远端设备。
    fn remote_shortcut_row() -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "shortcut-project-1".to_string(),
            name: "Remote Project".to_string(),
            kind: "remote".to_string(),
            device_id: "device-a".to_string(),
            device_name: "Mac mini".to_string(),
            path: "/Users/hans/remote-project".to_string(),
            last_opened_at: "2026-07-05T00:00:00Z".to_string(),
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 任务创建命令必须拒绝没有项目归属的任务，避免调度器后续无法定位工作台项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造空 project_id 请求并断言 row builder 返回错误。
    #[test]
    fn build_task_row_rejects_blank_project_id() {
        let request = CreateOrchestratorTaskRequest {
            project_id: "  ".to_string(),
            title: "实现任务".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };

        let result = build_orchestrator_task_row(request);

        assert!(result.is_err());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 任务必须有可展示标题和明确目标，否则任务列表与调度执行都缺少必要语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分别构造空标题和空目标请求，并断言 row builder 拒绝。
    #[test]
    fn build_task_row_rejects_blank_title_and_goal() {
        let blank_title = CreateOrchestratorTaskRequest {
            project_id: "project-1".to_string(),
            title: " ".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };
        let blank_goal = CreateOrchestratorTaskRequest {
            project_id: "project-1".to_string(),
            title: "实现任务".to_string(),
            goal: " ".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
        };

        assert!(build_orchestrator_task_row(blank_title).is_err());
        assert!(build_orchestrator_task_row(blank_goal).is_err());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     新建任务进入草稿态，用户输入的文本需要清理首尾空白，未显式设置优先级时按普通任务处理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造带空白和空 priority 的请求，断言生成 Row 的初始状态、trim 和默认字段。
    #[test]
    fn build_task_row_trims_fields_and_sets_defaults() {
        let request = CreateOrchestratorTaskRequest {
            project_id: "  project-1  ".to_string(),
            title: "  实现 API  ".to_string(),
            goal: "  暴露任务命令  ".to_string(),
            acceptance_criteria: "  测试通过  ".to_string(),
            priority: None,
        };

        let row = build_orchestrator_task_row(request).expect("row");

        assert_eq!(row.project_id, "project-1");
        assert_eq!(row.title, "实现 API");
        assert_eq!(row.goal, "暴露任务命令");
        assert_eq!(row.acceptance_criteria, "测试通过");
        assert_eq!(row.status, OrchestratorTaskStatus::Draft);
        assert_eq!(row.priority, 0);
        assert_eq!(row.attempt, 0);
        assert!(row.branch_name.is_none());
        assert!(row.worktree_id.is_none());
        assert!(row.session_id.is_none());
        assert!(row.blocked_reason.is_none());
        assert!(row.started_at.is_none());
        assert!(row.finished_at.is_none());
        assert!(!row.id.trim().is_empty());
        assert!(!row.created_at.trim().is_empty());
        assert_eq!(row.created_at, row.updated_at);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     手动 dispatch 命令需要给前端和测试返回稳定的 dispatched 数量，便于确认本次触发领取了多少任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用响应构造 helper，并断言 JSON 中的 dispatched 字段保留原始数量。
    #[test]
    fn dispatch_response_contains_dispatched_count() {
        let value = build_dispatch_once_response(2);

        assert_eq!(value["dispatched"], serde_json::json!(2));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     在线远端创建如果响应超时后落入 pending outbox，必须复用同一次请求生成的 clientRequestId 作为幂等键。
    ///
    /// Code Logic（这个测试做什么）:
    ///     从本机 create request 投影远端请求，断言生成了非空 client_request_id，且 clone 后保持同一个 key。
    #[test]
    fn remote_create_request_from_local_generates_stable_client_request_id() {
        let request = CreateOrchestratorTaskRequest {
            project_id: "shortcut-project-1".to_string(),
            title: "远端任务".to_string(),
            goal: "完成目标".to_string(),
            acceptance_criteria: "验收".to_string(),
            priority: Some(3),
        };

        let remote_request = remote_create_request_from_local(&request);
        let cloned_for_outbox = remote_request.clone();

        assert!(remote_request
            .client_request_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty()));
        assert_eq!(
            remote_request.client_request_id,
            cloned_for_outbox.client_request_id
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端任务返回给本机前端时必须使用 Workbench remote id 通道，否则后续按钮会把裸远端 id 当成本机 id。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含 worktree/session 的远端任务 DTO，映射后断言 task/project/worktree/session id 均符合本机 remote 规则。
    #[test]
    fn remote_task_mapping_wraps_ids_for_local_shortcut() {
        let shortcut = remote_shortcut_row();
        let mut task = OrchestratorTaskDto::from(command_task_row(
            "remote-task-1",
            OrchestratorTaskStatus::Running,
        ));
        task.project_id = "remote-project-1".to_string();
        task.worktree_id = Some("remote-worktree-1".to_string());
        task.session_id = Some("remote-session-1".to_string());

        let mapped = map_remote_task_for_shortcut(task, &shortcut);

        assert_eq!(mapped.id, "remote:device-a:remote-task-1");
        assert_eq!(mapped.project_id, "shortcut-project-1");
        assert_eq!(
            mapped.worktree_id.as_deref(),
            Some("remote:device-a:remote-worktree-1")
        );
        assert_eq!(
            mapped.session_id.as_deref(),
            Some("remote:device-a:remote-session-1")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 queue/retry/abort/evidence 入参可能是本机 UI 的 remote id，发送前必须剥离为 owning device 裸 id。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别传入匹配设备 remote id、裸 id 和错误设备 remote id，断言只接受前两类。
    #[test]
    fn remote_task_id_helper_strips_matching_remote_id_and_rejects_wrong_device() {
        let shortcut = remote_shortcut_row();

        let stripped =
            remote_inner_task_id_for_shortcut(&shortcut, "remote:device-a:task-1").unwrap();
        let raw = remote_inner_task_id_for_shortcut(&shortcut, "task-2").unwrap();
        let error = remote_inner_task_id_for_shortcut(&shortcut, "remote:device-b:task-3")
            .expect_err("wrong device must fail");

        assert_eq!(stripped, "task-1");
        assert_eq!(raw, "task-2");
        assert!(error.to_string().contains("远端任务不属于当前设备"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Agent 完成验证命令已迁移到全局 AppConfig，项目策略里的 legacy 命令不能再影响运行时验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造全局 Orchestrator 配置并断言命令层 helper 只返回全局 verification_commands。
    #[test]
    fn agent_completion_verification_commands_come_from_global_config() {
        let config = OrchestratorAutomationConfig {
            verification_commands: vec!["cargo test orchestrator::delivery --lib".to_string()],
            ..OrchestratorAutomationConfig::default()
        };

        let commands = verification_commands_for_agent_completion(&config);

        assert_eq!(
            commands,
            vec!["cargo test orchestrator::delivery --lib".to_string()]
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     验证通过后进入 Delivering 必须用 expected-status，不能覆盖验证期间用户点击 Abort 的结果。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Aborted 任务并调用命令层交付转换 helper，断言 transitioned=false 且数据库仍为 Aborted。
    #[tokio::test]
    async fn delivery_transition_skips_when_task_is_no_longer_verifying() {
        let repo = setup_orchestrator_repo().await;
        let task = command_task_row("task-aborted", OrchestratorTaskStatus::Aborted);
        repo.create_task(&task).await.expect("insert task");

        let transition = transition_verified_task_to_delivering(&repo, &task.id)
            .await
            .expect("transition result");
        let persisted = repo.get_task(&task.id).await.expect("persisted task");

        assert!(!transition.transitioned);
        assert_eq!(transition.task.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Agent 完成按钮只允许 Running 任务进入验证，避免用户把草稿或已结束任务误推进状态机。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Running 与 Draft 任务，断言完成校验 helper 只接受 Running。
    #[test]
    fn complete_agent_run_guard_accepts_only_running_tasks() {
        let running = OrchestratorTaskRow {
            id: "task-running".to_string(),
            project_id: "project-1".to_string(),
            title: "运行中".to_string(),
            goal: "goal".to_string(),
            acceptance_criteria: "criteria".to_string(),
            status: OrchestratorTaskStatus::Running,
            priority: 0,
            branch_name: None,
            worktree_id: Some("worktree-1".to_string()),
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
        };
        let draft = OrchestratorTaskRow {
            status: OrchestratorTaskStatus::Draft,
            ..running.clone()
        };

        assert!(ensure_task_can_complete_agent_run(&running).is_ok());
        let error = ensure_task_can_complete_agent_run(&draft).expect_err("draft must fail");
        assert!(error.to_string().contains("运行中"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     缺少 worktree 或找不到 worktree 这类验证前置失败，也必须作为 failed verification evidence 展示给用户。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用验证失败 evidence helper，断言 kind/title/summary/content 精确匹配 add_evidence 入参契约。
    #[test]
    fn verification_failure_evidence_uses_failed_verification_contract() {
        let evidence = verification_failure_evidence("任务缺少 worktree，无法运行验证命令。");

        assert_eq!(evidence.kind, "verificationOutput");
        assert_eq!(evidence.title, "验证命令");
        assert_eq!(evidence.summary, "failed");
        assert_eq!(evidence.content, "任务缺少 worktree，无法运行验证命令。");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     任务进入 Verifying 后，读取 worktree 失败这类可预期错误必须统一转成 failed evidence 和 Blocked 原因。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用验证失败 outcome helper，断言上下文和底层错误会进入 reason 与 evidence content。
    #[test]
    fn verification_failure_outcome_includes_context_error_and_failed_evidence() {
        let error = AppError::generic("数据库读取失败: locked");

        let outcome = verification_failure_outcome("读取任务 worktree 失败", &error);

        assert!(outcome.reason.contains("读取任务 worktree 失败"));
        assert!(outcome.reason.contains("locked"));
        assert_eq!(outcome.evidence.kind, "verificationOutput");
        assert_eq!(outcome.evidence.summary, "failed");
        assert_eq!(outcome.evidence.content, outcome.reason);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Blocked 任务的重试按钮只能把阻塞任务重新排队，不应允许 Running/Done 等状态被回退。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用重试状态 helper，断言 Blocked 返回 Queued，Running 返回业务错误。
    #[test]
    fn retry_status_guard_accepts_only_blocked_tasks() {
        assert_eq!(
            retry_orchestrator_task_target_status(OrchestratorTaskStatus::Blocked).unwrap(),
            OrchestratorTaskStatus::Queued
        );

        let error = retry_orchestrator_task_target_status(OrchestratorTaskStatus::Running)
            .expect_err("running retry must fail");
        assert!(error.to_string().contains("阻塞"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终止按钮只是把任务转为 Aborted，不删除 worktree 或 session。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用终止状态 helper，断言任意输入状态都会得到 Aborted。
    #[test]
    fn abort_status_helper_always_targets_aborted() {
        assert_eq!(
            abort_orchestrator_task_target_status(OrchestratorTaskStatus::Running),
            OrchestratorTaskStatus::Aborted
        );
        assert_eq!(
            abort_orchestrator_task_target_status(OrchestratorTaskStatus::Queued),
            OrchestratorTaskStatus::Aborted
        );
    }
}
