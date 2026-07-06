use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorEvidenceDto, OrchestratorProjectConfigDto,
    OrchestratorRunState, OrchestratorTaskAttemptRow, OrchestratorTaskDto, OrchestratorTaskRow,
    OrchestratorTaskStatus, OrchestratorWorkflowState, EVIDENCE_KIND_DELIVERY,
    EVIDENCE_KIND_REPAIR_PROMPT, EVIDENCE_KIND_VERIFICATION_OUTPUT,
    EVIDENCE_KIND_VERIFICATION_REVIEW,
};
use crate::orchestrator::outbox::{
    create_pending_remote_task, is_remote_network_error, mirror_payload_from_task,
    open_remote_project_for_shortcut, sync_remote_task_mirror_for_project,
    OrchestratorRemoteOutboxDto, RemoteMirrorTask,
};
use crate::orchestrator::prompt::RepairPromptContext;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::runner::prepare_repair_runner;
use crate::orchestrator::verifier::{self, VerifierReview};
use crate::orchestrator::workflow::{resolve_project_workflow, WorkflowSource};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::{parse_remote_entity_id, remote_entity_id};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};
use uuid::Uuid;

/// 创建 Orchestrator 任务的命令入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端创建编排任务时只提交用户可编辑字段，后端统一补齐 id、状态、关联执行信息和时间戳。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 接收 Tauri invoke 参数，并保留 priority 与 tracker 预留字段的可选值用于默认归一。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskRequest {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_labels: Option<Vec<String>>,
}

/// Orchestrator 看板泳道移动入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端看板拖拽需要显式提交项目、任务和目标泳道，后端负责校验归属和相邻移动规则。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 接收 Tauri invoke 参数，targetState 反序列化为强类型 workflow state。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOrchestratorTaskWorkflowStateRequest {
    pub project_id: String,
    pub task_id: String,
    pub target_state: OrchestratorWorkflowState,
}

/// Orchestrator 项目运行时快照 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 自动化状态条需要展示调度器、workflow 解析和执行槽位信息，帮助用户判断自动化为何运行或停滞。
///
/// Code Logic（这个结构体做什么）:
///     聚合设备级 Settings、项目 workflow resolver、repo 槽位统计和最近错误，使用 camelCase 序列化给前端。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeSnapshotDto {
    pub project_id: String,
    pub generated_at: String,
    pub scheduler_enabled: bool,
    pub workflow_source: String,
    pub workflow_valid: bool,
    pub workflow_error: Option<String>,
    pub max_concurrent_tasks: i64,
    pub slots_used: i64,
    pub slots_available: i64,
    pub latest_error: Option<String>,
}

/// Orchestrator 任务视图 DTO。
///
/// Business Logic（为什么需要这个枚举）:
///     Phase 6 前端需要在同一个任务列表中展示本机任务、远端真实任务和本机 pending remote outbox。
///
/// Code Logic（这个枚举做什么）:
///     使用 serde tag=`origin` 输出 discriminated union；旧命令仍返回 OrchestratorTaskDto 保持兼容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "origin",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
///     校验 project/title/goal 非空，生成 UUID 和 UTC 时间戳，归一化 tracker 字段，返回完整 OrchestratorTaskRow。
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
    let source = request
        .source
        .as_deref()
        .and_then(non_empty_trimmed_string)
        .unwrap_or_else(|| "internal".to_string());
    Ok(OrchestratorTaskRow {
        id: Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        title: title.to_string(),
        goal: goal.to_string(),
        acceptance_criteria: acceptance_criteria.to_string(),
        status: OrchestratorTaskStatus::Draft,
        source,
        external_id: request
            .external_id
            .as_deref()
            .and_then(non_empty_trimmed_string),
        external_identifier: request
            .external_identifier
            .as_deref()
            .and_then(non_empty_trimmed_string),
        external_url: request
            .external_url
            .as_deref()
            .and_then(non_empty_trimmed_string),
        external_state: request
            .external_state
            .as_deref()
            .and_then(non_empty_trimmed_string),
        external_labels: request.external_labels,
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
        ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Draft)
    })
}

/// Business Logic（为什么需要这个函数）:
///     创建任务时 tracker 预留字符串字段允许缺失或空白，但不能把空白当成有效外部标识写入数据库。
///
/// Code Logic（这个函数做什么）:
///     trim 输入字符串；空白返回 None，非空返回新的 String。
fn non_empty_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
        source: request.source.clone(),
        external_id: request.external_id.clone(),
        external_identifier: request.external_identifier.clone(),
        external_url: request.external_url.clone(),
        external_state: request.external_state.clone(),
        external_labels: request.external_labels.clone(),
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
///     runtime snapshot 需要把强类型 workflow 来源转成稳定前端文案键，而不是让 UI 猜测 Rust enum 形状。
///
/// Code Logic（这个函数做什么）:
///     将 WorkflowSource 映射为 camelCase 字符串；解析失败时由调用方使用 invalidProjectOverride。
fn workflow_source_label(source: WorkflowSource) -> &'static str {
    match source {
        WorkflowSource::BuiltInDefault => "builtInDefault",
        WorkflowSource::ProjectOverride => "projectOverride",
    }
}

/// Business Logic（为什么需要这个函数）:
///     workflow 解析失败时，UI 需要知道失败来自项目覆盖而不是内置默认逻辑。
///
/// Code Logic（这个函数做什么）:
///     根据项目根目录是否存在 WORKFLOW.md 返回失败来源标签。
fn invalid_workflow_source_label(project_path: &Path) -> &'static str {
    if project_path.join("WORKFLOW.md").exists() {
        "invalidProjectOverride"
    } else {
        "builtInDefault"
    }
}

/// Business Logic（为什么需要这个函数）:
///     看板拖拽必须只作用于当前本机项目的真实任务，不能误改远端 mirror 或其它项目任务。
///
/// Code Logic（这个函数做什么）:
///     校验请求 projectId、项目 kind 和任务归属后委托 repo 相邻泳道移动，返回 Local task view。
pub(crate) async fn move_orchestrator_task_workflow_state_for_project(
    repo: &OrchestratorRepo,
    project: &WorkbenchProjectRow,
    request: MoveOrchestratorTaskWorkflowStateRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    if request.project_id != project.id {
        return Err(AppError::generic("请求项目与当前项目不一致"));
    }
    if project.kind == "remote" {
        return Err(AppError::generic("远端项目暂不支持拖拽移动"));
    }

    let current = repo.get_task(&request.task_id).await?;
    if current.project_id != project.id {
        return Err(AppError::generic("任务不属于当前项目"));
    }

    let moved = repo
        .move_task_workflow_state(&request.task_id, request.target_state)
        .await?;
    Ok(local_task_view(moved))
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 状态条需要项目级 runtime snapshot，但实际数据分散在 Settings、workflow resolver 和 repo 统计中。
///
/// Code Logic（这个函数做什么）:
///     解析项目 WORKFLOW.md，统计当前项目 active run_state 槽位和最近 blocked 原因，生成前端 DTO。
pub(crate) async fn get_orchestrator_runtime_snapshot_for_project(
    repo: &OrchestratorRepo,
    config: &OrchestratorAutomationConfig,
    project: &WorkbenchProjectRow,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    if project.kind == "remote" {
        return Err(AppError::generic("远端项目暂不支持运行时快照"));
    }
    let project_path = Path::new(&project.path);
    let (workflow_source, workflow_valid, workflow_error) =
        match resolve_project_workflow(project_path) {
            Ok(workflow) => (
                workflow_source_label(workflow.source).to_string(),
                true,
                None,
            ),
            Err(error) => (
                invalid_workflow_source_label(project_path).to_string(),
                false,
                Some(error.to_string()),
            ),
        };
    let slots_used = repo
        .count_active_run_states_for_project(&project.id)
        .await?;
    let max_slots = config.max_concurrent_tasks.max(0);
    let slots_available = (max_slots - slots_used).max(0);
    let latest_error = repo.latest_blocked_reason_for_project(&project.id).await?;

    Ok(OrchestratorRuntimeSnapshotDto {
        project_id: project.id.clone(),
        generated_at: Utc::now().to_rfc3339(),
        scheduler_enabled: config.enabled,
        workflow_source,
        workflow_valid,
        workflow_error,
        max_concurrent_tasks: max_slots,
        slots_used,
        slots_available,
        latest_error,
    })
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
///     Agent 完成后的验证命令已迁移为设备级全局设置，不能再读取 legacy 项目配置表。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorAutomationConfig 克隆 verification_commands，调用方可在 await 前释放 config 读锁。
fn verification_commands_for_agent_completion(
    config: &OrchestratorAutomationConfig,
) -> Vec<String> {
    config.verification_commands.clone()
}

/// Business Logic（为什么需要这个函数）:
///     验证通过后是否自动交付由全局自动化开关和四个交付开关共同决定；默认关闭自动化时必须停在人工复核。
///
/// Code Logic（这个函数做什么）:
///     对 enabled、auto_commit、auto_push_task_branch、auto_merge_to_main、auto_push_main 执行 AND 判断。
fn auto_delivery_enabled(config: &OrchestratorAutomationConfig) -> bool {
    config.enabled
        && config.auto_commit
        && config.auto_push_task_branch
        && config.auto_merge_to_main
        && config.auto_push_main
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

/// 验证失败后回到修复准备阶段的条件转换结果。
///
/// Business Logic（为什么需要这个结构体）:
///     verifier 判定失败后，任务可能已被用户 Abort；命令层必须知道是否真正取得 Verifying->Preparing 执行权。
///
/// Code Logic（这个结构体做什么）:
///     transitioned=true 表示原子转换命中；false 表示状态已变化，task 为当前最新任务。
struct ConditionalRepairTransition {
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
        kind: EVIDENCE_KIND_VERIFICATION_OUTPUT,
        title: "验证命令",
        summary: "failed",
        content: reason.to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     verifier 审查结果需要作为独立 evidence 展示，让用户区分命令输出和模型裁决。
///
/// Code Logic（这个函数做什么）:
///     将 VerifierReview 转成 verificationReview evidence；summary 固定为 passed/failed。
fn verification_review_evidence(review: &VerifierReview) -> VerificationEvidence {
    let summary = if review.passed { "passed" } else { "failed" };
    let repair_prompt = review.repair_prompt.as_deref().unwrap_or("(none)");
    let risk_notes = if review.risk_notes.is_empty() {
        "(none)".to_string()
    } else {
        review.risk_notes.join("\n- ")
    };
    VerificationEvidence {
        kind: EVIDENCE_KIND_VERIFICATION_REVIEW,
        title: "Claude 验证器",
        summary,
        content: format!(
            "passed: {}\nreason: {}\nriskNotes: {}\nrepairPrompt: {}",
            review.passed, review.reason, risk_notes, repair_prompt
        ),
    }
}

/// Business Logic（为什么需要这个函数）:
///     verifier 基础设施失败也要留下 failed verification evidence，避免任务只显示 Blocked 而没有诊断内容。
///
/// Code Logic（这个函数做什么）:
///     把 context 和底层错误拼成 verificationReview failed evidence 与 blocked_reason。
fn verification_review_failure_outcome(
    context: &str,
    err: &AppError,
) -> VerificationFailureOutcome {
    let reason = format!("{context}: {err}");
    VerificationFailureOutcome {
        reason: reason.clone(),
        evidence: VerificationEvidence {
            kind: EVIDENCE_KIND_VERIFICATION_REVIEW,
            title: "Claude 验证器",
            summary: "failed",
            content: reason,
        },
    }
}

/// Business Logic（为什么需要这个函数）:
///     verifier 失败时交给下一轮 Claude 的 repair prompt 必须持久化，便于用户审计自动修复方向。
///
/// Code Logic（这个函数做什么）:
///     构造 repairPrompt evidence，summary 使用 failed 表示本次验证未通过。
fn repair_prompt_evidence(repair_prompt: &str) -> VerificationEvidence {
    VerificationEvidence {
        kind: EVIDENCE_KIND_REPAIR_PROMPT,
        title: "修复指令",
        summary: "failed",
        content: repair_prompt.to_string(),
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
///     verifier 通过但自动交付未完全开启时，任务需要交给用户人工复核，避免继续进入 delivery pipeline。
///
/// Code Logic（这个函数做什么）:
///     用 Verifying expected-status 原子守卫写入 legacy Done，同时指定 HumanReview/Idle/Succeeded split state。
async fn transition_verified_task_to_human_review(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<ConditionalDeliveryTransition, AppError> {
    match repo
        .try_transition_task_split_state(
            task_id,
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Done,
            OrchestratorWorkflowState::HumanReview,
            OrchestratorRunState::Idle,
            Some(OrchestratorAttemptPhase::Succeeded),
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
///     verifier 判定失败后只有仍处于 Verifying 的任务可以回到 Preparing，避免用户 Abort 被旧流程覆盖。
///
/// Code Logic（这个函数做什么）:
///     调用仓储 split-state 条件 helper 做 Verifying->Preparing/Rework/Preparing/Failed；未命中时读取当前任务并标记未转换。
async fn transition_failed_verification_task_to_preparing(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<ConditionalRepairTransition, AppError> {
    match repo
        .try_transition_task_split_state(
            task_id,
            OrchestratorTaskStatus::Verifying,
            OrchestratorTaskStatus::Preparing,
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Preparing,
            Some(OrchestratorAttemptPhase::Failed),
            None,
        )
        .await?
    {
        Some(task) => Ok(ConditionalRepairTransition {
            transitioned: true,
            task,
        }),
        None => Ok(ConditionalRepairTransition {
            transitioned: false,
            task: repo.get_task(task_id).await?,
        }),
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户可能在验证命令执行后、verifier CLI 启动前终止任务；此时不应继续消耗 Claude CLI 或启动修复。
///
/// Code Logic（这个函数做什么）:
///     重读任务状态；仍为 Verifying 返回 None，状态已变化则返回当前任务 DTO。
async fn stop_verification_if_task_changed(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<Option<OrchestratorTaskDto>, AppError> {
    let current = repo.get_task(task_id).await?;
    if current.status == OrchestratorTaskStatus::Verifying {
        Ok(None)
    } else {
        Ok(Some(OrchestratorTaskDto::from(current)))
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
            .add_evidence(
                task_id,
                EVIDENCE_KIND_DELIVERY,
                "delivery",
                "failed",
                &reason,
            )
            .await?;
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     手动完成和 terminal sentinel 共用 completion pipeline；只要任务成功从 Running 进入 Verifying，
///     当前 active running attempt 就应标记为 completed，避免重复 sentinel 或重复点击再次定位到旧 attempt。
///
/// Code Logic（这个函数做什么）:
///     使用任务行上的 active attempt 编号调用 OrchestratorRepo::mark_attempt_completed；缺少 attempt 时返回业务错误。
async fn mark_active_running_attempt_completed(
    repo: &OrchestratorRepo,
    task: &OrchestratorTaskRow,
) -> Result<OrchestratorTaskAttemptRow, AppError> {
    if task.attempt <= 0 {
        return Err(AppError::generic("任务缺少 active attempt，无法标记完成"));
    }
    repo.mark_attempt_completed(&task.id, task.attempt).await
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

/// Business Logic（为什么需要这个函数）:
///     verifier CLI、JSON、diff 读取等基础设施失败都应写 failed review evidence，并只在任务仍 Verifying 时阻塞。
///
/// Code Logic（这个函数做什么）:
///     构造 verificationReview failed outcome，复用统一 evidence + Blocked side-effect helper。
async fn block_task_with_verification_review_error(
    state: &AppState,
    task_id: &str,
    context: &str,
    err: AppError,
) -> Result<OrchestratorTaskDto, AppError> {
    let outcome = verification_review_failure_outcome(context, &err);
    block_task_with_verification_outcome(state, task_id, &outcome).await
}

/// Business Logic（为什么需要这个函数）:
///     修复 runner 准备失败时任务不能永久停在 Preparing 或 runner bootstrap 的 Running 状态。
///
/// Code Logic（这个函数做什么）:
///     按 scheduler 语义尝试 Preparing->Blocked，再尝试 Running->Blocked；未命中时返回当前任务，不覆盖 Abort/Block。
async fn block_task_with_repair_runner_error(
    state: &AppState,
    task_id: &str,
    err: AppError,
) -> Result<OrchestratorTaskDto, AppError> {
    let reason = format!("修复 Runner 准备失败: {err}");
    let repo = state.orchestrator_repo.as_ref();
    let blocked = if let Some(task) = repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Preparing,
            OrchestratorTaskStatus::Blocked,
            Some(&reason),
        )
        .await?
    {
        Some(task)
    } else {
        repo.try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Blocked,
            Some(&reason),
        )
        .await?
    };
    let task = if let Some(task) = blocked {
        repo.add_event(task_id, "blocked", &reason, None).await?;
        task
    } else {
        repo.get_task(task_id).await?
    };
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     verifier 判定失败后应写 repairPrompt evidence，并在任务仍 Verifying 时回到 Preparing 启动同 worktree 修复轮。
///
/// Code Logic（这个函数做什么）:
///     先做 Verifying->Preparing 条件转换；命中后追加 repairPrompt evidence 并调用 prepare_repair_runner，
///     准备失败时按 runner failure 语义条件阻塞。
async fn start_repair_runner_for_failed_review(
    state: &AppState,
    app_handle: AppHandle,
    task_id: &str,
    review: &VerifierReview,
) -> Result<OrchestratorTaskDto, AppError> {
    let repair_prompt = review
        .repair_prompt
        .as_deref()
        .ok_or_else(|| AppError::generic("verifier failed review 缺少 repairPrompt"))?;
    let repair_transition =
        transition_failed_verification_task_to_preparing(state.orchestrator_repo.as_ref(), task_id)
            .await?;
    if !repair_transition.transitioned {
        return Ok(OrchestratorTaskDto::from(repair_transition.task));
    }
    add_verification_evidence(state, task_id, &repair_prompt_evidence(repair_prompt)).await?;

    let repair_context = RepairPromptContext {
        verifier_reason: &review.reason,
        repair_prompt,
    };
    match prepare_repair_runner(state, app_handle, &repair_transition.task, repair_context).await {
        Ok(task) => Ok(OrchestratorTaskDto::from(task)),
        Err(err) => block_task_with_repair_runner_error(state, task_id, err).await,
    }
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
pub(crate) async fn list_orchestrator_task_views_for_state(
    state: &AppState,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let Some(project_id) = project_id else {
        let rows = state.orchestrator_repo.list_tasks(None).await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    };

    let project = get_orchestrator_workbench_project(state, &project_id).await?;
    if project.kind != "remote" {
        let rows = state
            .orchestrator_repo
            .list_tasks(Some(&project_id))
            .await?;
        return Ok(rows.into_iter().map(local_task_view).collect());
    }

    let mirrors = match sync_remote_task_mirror_for_project(state, &project).await {
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
    views.extend(pending_remote_task_views_for_project(state, &project).await?);
    Ok(views)
}

#[tauri::command]
pub async fn list_orchestrator_task_views(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    list_orchestrator_task_views_for_state(&state, project_id).await
}

/// 创建 remote-aware Orchestrator 任务视图。
///
/// Business Logic（为什么需要这个函数）:
///     local 项目应继续创建本机任务；remote 项目在线时创建远端权威任务，离线时写 pending outbox。
///
/// Code Logic（这个函数做什么）:
///     先按 projectId 读取 Workbench 项目；local 走旧 row builder + repo，remote 先尝试在线创建，
///     遇到网络/离线错误时创建 pending outbox 并返回 PendingRemote。
pub(crate) async fn create_orchestrator_task_view_for_state(
    state: &AppState,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind != "remote" {
        let row = build_orchestrator_task_row(request)?;
        state.orchestrator_repo.create_task(&row).await?;
        return Ok(local_task_view(row));
    }

    let remote_request = remote_create_request_from_local(&request);
    match create_remote_orchestrator_task_online(state, &project, remote_request.clone()).await {
        Ok(view) => Ok(view),
        Err(err) if is_remote_network_error(&err) => {
            let item = create_pending_remote_task(state, &project, remote_request).await?;
            Ok(OrchestratorTaskViewDto::PendingRemote {
                item: item.to_dto(),
            })
        }
        Err(err) => Err(err),
    }
}

#[tauri::command]
pub async fn create_orchestrator_task_view(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    create_orchestrator_task_view_for_state(&state, request).await
}

/// 移动 Orchestrator 任务工作流泳道。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 自动化看板需要通过拖拽调整任务所在业务泳道，但移动规则必须由后端统一校验。
///
/// Code Logic（这个函数做什么）:
///     读取 projectId 对应 Workbench 项目，委托本机项目 helper 校验 remote/归属/相邻移动并返回 task view。
#[tauri::command]
pub async fn move_orchestrator_task_workflow_state(
    state: State<'_, AppState>,
    request: MoveOrchestratorTaskWorkflowStateRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &request.project_id).await?;
    move_orchestrator_task_workflow_state_for_project(
        state.orchestrator_repo.as_ref(),
        &project,
        request,
    )
    .await
}

/// 获取 Orchestrator 项目运行时快照。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 自动化状态条需要一个轻量观测接口展示 scheduler、workflow 和槽位状态。
///
/// Code Logic（这个函数做什么）:
///     读取 Workbench 项目和设备级 Settings 后，构造 runtime snapshot DTO。
#[tauri::command]
pub async fn get_orchestrator_runtime_snapshot(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    let project = get_orchestrator_workbench_project(state.inner(), &project_id).await?;
    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    get_orchestrator_runtime_snapshot_for_project(
        state.orchestrator_repo.as_ref(),
        &config,
        &project,
    )
    .await
}

/// 通过 HTTP task-view 协议创建 remote-aware Orchestrator 任务。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 创建任务需要保留旧移动端语义：create 请求默认直接入队，同时还要支持 remote shortcut 代理。
///
/// Code Logic（这个函数做什么）:
///     local 项目用 clientRequestId 幂等创建并按 queue 决定是否入队；remote 项目保持同一 requestId 转发到 owning device，
///     网络失败时写 pending outbox 并返回 PendingRemote。
pub(crate) async fn create_orchestrator_task_view_for_http(
    state: &AppState,
    req: RemoteCreateOrchestratorTaskReq,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &req.project_id).await?;
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::generic("移动端创建任务缺少 clientRequestId"))?;

    if project.kind != "remote" {
        let row = build_orchestrator_task_row(CreateOrchestratorTaskRequest {
            project_id: req.project_id,
            title: req.title,
            goal: req.goal,
            acceptance_criteria: req.acceptance_criteria,
            priority: Some(req.priority),
            source: req.source,
            external_id: req.external_id,
            external_identifier: req.external_identifier,
            external_url: req.external_url,
            external_state: req.external_state,
            external_labels: req.external_labels,
        })?;
        let created = state
            .orchestrator_repo
            .create_remote_task_for_client_request(&client_request_id, &row, req.queue)
            .await?;
        return Ok(local_task_view(created));
    }

    let remote_request = RemoteCreateOrchestratorTaskReq {
        client_request_id: Some(client_request_id),
        ..req
    };
    match create_remote_orchestrator_task_online(state, &project, remote_request.clone()).await {
        Ok(view) => Ok(view),
        Err(err) if is_remote_network_error(&err) => {
            let item = create_pending_remote_task(state, &project, remote_request).await?;
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

/// 查询 legacy Orchestrator 项目配置。
///
/// Business Logic（为什么需要这个函数）:
///     历史版本写入过项目级自动化配置，后端仍保留兼容/调试读取能力。
///     用户可见配置入口已经收敛到 Settings 自动化 tab，scheduler、验证和 delivery 统一读取 AppConfig.orchestrator。
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
///     用户在 Workbench 中看到 Claude Code 完成后，需要从 Orchestrator 触发项目验证；Phase 7 的终端哨兵也复用同一流程。
///
/// Code Logic（这个函数做什么）:
///     Tauri command 只解包 State 和 String，再委托 complete_orchestrator_agent_run_for_state 执行内部 pipeline。
#[tauri::command]
pub async fn complete_orchestrator_agent_run(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    task_id: String,
) -> Result<OrchestratorTaskDto, AppError> {
    complete_orchestrator_agent_run_for_state(state.inner(), app_handle, &task_id).await
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
    app_handle: AppHandle,
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

    complete_orchestrator_agent_run_after_verifying_transition(state, app_handle, task).await
}

/// Business Logic（为什么需要这个函数）:
///     terminal sentinel 只代表产生该输出的具体 session/attempt 完成，旧 session 的迟到哨兵不得推进当前 active runner。
///
/// Code Logic（这个函数做什么）:
///     用 task_id + expected attempt + expected session 原子校验 Running runner 后切到 Verifying；未命中时返回当前任务 no-op。
pub(crate) async fn complete_orchestrator_agent_run_for_attempt(
    state: &AppState,
    app_handle: AppHandle,
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

    complete_orchestrator_agent_run_after_verifying_transition(state, app_handle, task).await
}

/// Business Logic（为什么需要这个函数）:
///     手动完成和 sentinel 完成在成功取得 Verifying 执行权后，后续 attempt 完成、验证、delivery 语义必须完全一致。
///
/// Code Logic（这个函数做什么）:
///     接收已被原子切到 Verifying 的任务 Row，标记 active attempt completed，执行验证命令并写 evidence；
///     非零验证输出交给 verifier Claude 裁决，passed=true 进入 delivery，passed=false 回 Preparing 启动修复 runner。
async fn complete_orchestrator_agent_run_after_verifying_transition(
    state: &AppState,
    app_handle: AppHandle,
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
    let verification_commands = {
        let config = state.config.read().expect("config 读锁中毒");
        verification_commands_for_agent_completion(&config.orchestrator)
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

    let diff = match verifier::collect_worktree_diff(&cwd) {
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
            return Ok(OrchestratorTaskDto::from(review_transition.task));
        }

        let delivery_transition =
            transition_verified_task_to_delivering(state.orchestrator_repo.as_ref(), &task.id)
                .await?;
        if !delivery_transition.transitioned {
            return Ok(OrchestratorTaskDto::from(delivery_transition.task));
        }
        return run_delivery_for_task(state, app_handle, &delivery_transition.task.id).await;
    }

    start_repair_runner_for_failed_review(state, app_handle, &task.id, &review).await
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
    use std::fs;
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
            ..OrchestratorTaskRow::default_for_status(status)
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
    ///     新增本机-only 拖拽命令和 runtime snapshot 都需要 Workbench 本机项目上下文，测试应复用稳定项目行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 kind=local 的 WorkbenchProjectRow，path 由调用方传入以便 workflow resolver 读取临时目录。
    fn local_project_row(path: String) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "project-1".to_string(),
            name: "Local Project".to_string(),
            kind: "local".to_string(),
            device_id: "device-local".to_string(),
            device_name: "MacBook".to_string(),
            path,
            last_opened_at: "2026-07-05T00:00:00Z".to_string(),
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     workflow snapshot 测试需要一个真实项目目录，避免依赖用户机器上的固定路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在系统临时目录下创建唯一文件夹并返回路径字符串，调用方负责按需写 WORKFLOW.md。
    fn temp_project_dir(name: &str) -> String {
        let path = std::env::temp_dir().join(format!("{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temp project dir");
        path.to_string_lossy().to_string()
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
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
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
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
        };
        let blank_goal = CreateOrchestratorTaskRequest {
            project_id: "project-1".to_string(),
            title: "实现任务".to_string(),
            goal: " ".to_string(),
            acceptance_criteria: "测试通过".to_string(),
            priority: None,
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
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
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
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

    /// Business Logic（为什么需要这个函数）:
    ///     本机拖拽命令需要校验项目归属后返回 Local task view，供前端直接刷新看板卡片。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建本机 Backlog 任务，调用命令层 helper 移到 Todo，断言返回 Local DTO 且 workflow_state 更新。
    #[tokio::test]
    async fn move_workflow_state_for_local_project_returns_local_view() {
        let repo = setup_orchestrator_repo().await;
        let task = command_task_row("task-local-move", OrchestratorTaskStatus::Draft);
        repo.create_task(&task).await.unwrap();
        let project = local_project_row(temp_project_dir("orch-local-move"));

        let view = move_orchestrator_task_workflow_state_for_project(
            &repo,
            &project,
            MoveOrchestratorTaskWorkflowStateRequest {
                project_id: "project-1".to_string(),
                task_id: task.id.clone(),
                target_state: OrchestratorWorkflowState::Todo,
            },
        )
        .await
        .unwrap();

        match view {
            OrchestratorTaskViewDto::Local { task } => {
                assert_eq!(task.id, "task-local-move");
                assert_eq!(task.workflow_state, OrchestratorWorkflowState::Todo);
            }
            other => panic!("expected local view, got {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     本轮拖拽动作只支持本机项目，remote shortcut 必须被明确拒绝，避免本机误改 mirror 或远端权威状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 remote WorkbenchProjectRow 调用命令层 helper，断言返回中文 remote 拒绝错误。
    #[tokio::test]
    async fn move_workflow_state_rejects_remote_project() {
        let repo = setup_orchestrator_repo().await;
        let project = remote_shortcut_row();

        let error = move_orchestrator_task_workflow_state_for_project(
            &repo,
            &project,
            MoveOrchestratorTaskWorkflowStateRequest {
                project_id: project.id.clone(),
                task_id: "remote:device-a:task-1".to_string(),
                target_state: OrchestratorWorkflowState::Todo,
            },
        )
        .await
        .expect_err("remote drag must be rejected");

        assert!(error.to_string().contains("远端项目暂不支持拖拽移动"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     拖拽命令必须防止前端请求 projectId 与当前项目上下文不一致时误改任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用本机项目上下文和不匹配 request.projectId 调用 helper，断言返回项目不一致错误。
    #[tokio::test]
    async fn move_workflow_state_rejects_project_id_mismatch() {
        let repo = setup_orchestrator_repo().await;
        let project = local_project_row(temp_project_dir("orch-project-mismatch"));

        let error = move_orchestrator_task_workflow_state_for_project(
            &repo,
            &project,
            MoveOrchestratorTaskWorkflowStateRequest {
                project_id: "other-project".to_string(),
                task_id: "task-1".to_string(),
                target_state: OrchestratorWorkflowState::Todo,
            },
        )
        .await
        .expect_err("project mismatch must be rejected");

        assert!(error.to_string().contains("请求项目与当前项目不一致"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     拖拽命令必须确认任务属于当前 Workbench 项目，避免跨项目卡片被误移动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 project-2 的任务，在 project-1 上下文中请求移动，断言返回任务归属错误且状态不变。
    #[tokio::test]
    async fn move_workflow_state_rejects_task_from_another_project() {
        let repo = setup_orchestrator_repo().await;
        let task = OrchestratorTaskRow {
            project_id: "project-2".to_string(),
            ..command_task_row("task-other-project", OrchestratorTaskStatus::Draft)
        };
        repo.create_task(&task).await.unwrap();
        let project = local_project_row(temp_project_dir("orch-task-project-mismatch"));

        let error = move_orchestrator_task_workflow_state_for_project(
            &repo,
            &project,
            MoveOrchestratorTaskWorkflowStateRequest {
                project_id: project.id.clone(),
                task_id: task.id.clone(),
                target_state: OrchestratorWorkflowState::Todo,
            },
        )
        .await
        .expect_err("foreign task must be rejected");
        let persisted = repo.get_task(&task.id).await.unwrap();

        assert!(error.to_string().contains("任务不属于当前项目"));
        assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Backlog);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 需要向 UI 暴露当前项目使用内置 workflow 还是项目覆盖，方便用户诊断自动化行为。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用没有 WORKFLOW.md 的临时项目目录构建 snapshot，断言 source 为 builtInDefault 且 workflow_valid=true。
    #[tokio::test]
    async fn runtime_snapshot_reports_built_in_workflow_source() {
        let repo = setup_orchestrator_repo().await;
        let project = local_project_row(temp_project_dir("orch-snapshot-valid"));
        let config = OrchestratorAutomationConfig {
            enabled: true,
            max_concurrent_tasks: 2,
            ..OrchestratorAutomationConfig::default()
        };

        let snapshot = get_orchestrator_runtime_snapshot_for_project(&repo, &config, &project)
            .await
            .unwrap();

        assert_eq!(snapshot.project_id, "project-1");
        assert!(snapshot.scheduler_enabled);
        assert_eq!(snapshot.workflow_source, "builtInDefault");
        assert!(snapshot.workflow_valid);
        assert_eq!(snapshot.max_concurrent_tasks, 2);
        assert_eq!(snapshot.slots_used, 0);
        assert_eq!(snapshot.slots_available, 2);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目 WORKFLOW.md 解析失败时，状态条必须展示无效 workflow，而不是假装内置默认可用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入非法 YAML front matter，构建 runtime snapshot 并断言 invalidProjectOverride 与错误信息存在。
    #[tokio::test]
    async fn runtime_snapshot_reports_invalid_project_workflow() {
        let repo = setup_orchestrator_repo().await;
        let project_dir = temp_project_dir("orch-snapshot-invalid");
        fs::write(
            std::path::Path::new(&project_dir).join("WORKFLOW.md"),
            "---\nrunner: [invalid\n---\n",
        )
        .expect("write workflow");
        let project = local_project_row(project_dir);

        let snapshot = get_orchestrator_runtime_snapshot_for_project(
            &repo,
            &OrchestratorAutomationConfig::default(),
            &project,
        )
        .await
        .unwrap();

        assert_eq!(snapshot.workflow_source, "invalidProjectOverride");
        assert!(!snapshot.workflow_valid);
        assert_eq!(snapshot.max_concurrent_tasks, 1);
        assert!(snapshot
            .workflow_error
            .as_deref()
            .unwrap_or_default()
            .contains("WORKFLOW.md"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端项目的 runtime snapshot 不能展示本机 scheduler/config/workflow，否则用户会误判远端自动化状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 remote shortcut 调用 snapshot helper，断言本轮明确拒绝而不是读取本机状态。
    #[tokio::test]
    async fn runtime_snapshot_rejects_remote_project() {
        let repo = setup_orchestrator_repo().await;
        let project = remote_shortcut_row();

        let error = get_orchestrator_runtime_snapshot_for_project(
            &repo,
            &OrchestratorAutomationConfig::default(),
            &project,
        )
        .await
        .expect_err("remote snapshot must be rejected");

        assert!(error.to_string().contains("远端项目暂不支持运行时快照"));
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
            source: None,
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
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
    ///     前端 remote-aware API 直接消费 OrchestratorTaskViewDto，设备字段必须保持 camelCase。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 Remote 视图并断言 origin、deviceId、deviceName 字段符合 Tauri invoke 契约。
    #[test]
    fn remote_task_view_serializes_fields_as_camel_case() {
        let shortcut = remote_shortcut_row();
        let task = OrchestratorTaskDto::from(command_task_row(
            "remote-task-1",
            OrchestratorTaskStatus::Running,
        ));
        let view = remote_task_view(task, &shortcut);

        let value = serde_json::to_value(view).expect("serialize task view");

        assert_eq!(value["origin"], "remote");
        assert_eq!(value["deviceId"], "device-a");
        assert_eq!(value["deviceName"], "Mac mini");
        assert!(value.get("device_id").is_none());
        assert!(value.get("device_name").is_none());
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
    ///     Agent 完成验证命令已迁移到全局 AppConfig，legacy 项目配置里的命令不能再影响运行时验证。
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
    ///     verifier 通过但 full-auto delivery 关闭时，任务应进入人工复核泳道，而不是继续自动交付或阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Verifying 任务并调用 HumanReview 条件转换 helper，断言 legacy status 与 split state 精确落库。
    #[tokio::test]
    async fn human_review_transition_sets_split_state_when_delivery_disabled() {
        let repo = setup_orchestrator_repo().await;
        let task = command_task_row("task-human-review", OrchestratorTaskStatus::Verifying);
        repo.create_task(&task).await.expect("insert task");

        let transition = transition_verified_task_to_human_review(&repo, &task.id)
            .await
            .expect("transition result");
        let persisted = repo.get_task(&task.id).await.expect("persisted task");

        assert!(transition.transitioned);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Done);
        assert_eq!(
            persisted.workflow_state,
            OrchestratorWorkflowState::HumanReview
        );
        assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
        assert_eq!(
            persisted.attempt_phase,
            Some(OrchestratorAttemptPhase::Succeeded)
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 未通过时，任务必须明确进入 Rework 并标记本轮 failed，同时保持 Preparing 以便立即启动修复 Runner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Verifying 任务并调用 repair 条件转换 helper，断言 workflow/run/attempt phase 与修复准备语义一致。
    #[tokio::test]
    async fn repair_transition_sets_rework_preparing_failed_phase() {
        let repo = setup_orchestrator_repo().await;
        let task = command_task_row("task-rework", OrchestratorTaskStatus::Verifying);
        repo.create_task(&task).await.expect("insert task");

        let transition = transition_failed_verification_task_to_preparing(&repo, &task.id)
            .await
            .expect("transition result");
        let persisted = repo.get_task(&task.id).await.expect("persisted task");

        assert!(transition.transitioned);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Preparing);
        assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Rework);
        assert_eq!(persisted.run_state, OrchestratorRunState::Preparing);
        assert_eq!(
            persisted.attempt_phase,
            Some(OrchestratorAttemptPhase::Failed)
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     只有全局自动化和四个自动交付开关全部打开时才应进入 Delivering，默认配置应转人工复核。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造默认关闭、全部开启和单项关闭配置，断言 helper 对所有开关执行 AND 语义。
    #[test]
    fn auto_delivery_enabled_requires_all_flags() {
        assert!(!auto_delivery_enabled(
            &OrchestratorAutomationConfig::default()
        ));

        let enabled = OrchestratorAutomationConfig {
            enabled: true,
            ..OrchestratorAutomationConfig::default()
        };
        assert!(auto_delivery_enabled(&enabled));

        let disabled = OrchestratorAutomationConfig {
            enabled: true,
            auto_push_main: false,
            ..OrchestratorAutomationConfig::default()
        };
        assert!(!auto_delivery_enabled(&disabled));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     手动完成按钮与 sentinel 共用 completion helper，Running->Verifying 成功后必须把当前 active attempt 标为 completed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Running 任务和 running attempt，模拟 helper 的状态转移后调用 active attempt 完成 helper，
    ///     断言 session 反查不再返回 running attempt。
    #[tokio::test]
    async fn completion_helper_marks_active_running_attempt_completed_after_verifying() {
        let repo = setup_orchestrator_repo().await;
        let mut task = command_task_row("task-running", OrchestratorTaskStatus::Running);
        task.attempt = 1;
        task.worktree_id = Some("worktree-1".to_string());
        task.session_id = Some("session-1".to_string());
        repo.create_task(&task).await.expect("insert task");
        repo.add_attempt(&task.id, 1, "worktree-1", "session-1", "prompt", "running")
            .await
            .expect("insert attempt");

        let verifying = repo
            .transition_task_status(
                &task.id,
                OrchestratorTaskStatus::Running,
                OrchestratorTaskStatus::Verifying,
                None,
            )
            .await
            .expect("transition to verifying");
        let completed = mark_active_running_attempt_completed(&repo, &verifying)
            .await
            .expect("complete active attempt");
        let running = repo
            .get_running_attempt_by_session("session-1")
            .await
            .expect("query running attempt");

        assert_eq!(completed.status, "completed");
        assert_eq!(completed.completed_at.is_some(), true);
        assert!(running.is_none());
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
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Running)
        };
        let draft = OrchestratorTaskRow {
            status: OrchestratorTaskStatus::Draft,
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Draft)
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
    ///     verifier 审查结果会直接驱动 delivery 或 repair，evidence summary 必须稳定反映 passed 布尔值。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分别构造 passed/failed VerifierReview，断言 evidence kind 和 summary 对齐 Phase8 契约。
    #[test]
    fn verification_review_evidence_uses_pass_fail_summary() {
        let passed = VerifierReview {
            passed: true,
            reason: "满足验收".to_string(),
            repair_prompt: None,
            risk_notes: Vec::new(),
        };
        let failed = VerifierReview {
            passed: false,
            reason: "测试失败".to_string(),
            repair_prompt: Some("修复测试失败".to_string()),
            risk_notes: vec!["需要复跑 cargo test".to_string()],
        };

        let passed_evidence = verification_review_evidence(&passed);
        let failed_evidence = verification_review_evidence(&failed);

        assert_eq!(passed_evidence.kind, EVIDENCE_KIND_VERIFICATION_REVIEW);
        assert_eq!(passed_evidence.summary, "passed");
        assert_eq!(failed_evidence.summary, "failed");
        assert!(failed_evidence.content.contains("修复测试失败"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 失败时 repairPrompt evidence 是下一轮自动修复的审计入口，kind/summary 不能与验证输出混淆。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 repair prompt evidence helper，断言 kind、summary 和 content 精确匹配。
    #[test]
    fn repair_prompt_evidence_uses_repair_prompt_contract() {
        let evidence = repair_prompt_evidence("只修复 orchestrator completion 流程");

        assert_eq!(evidence.kind, EVIDENCE_KIND_REPAIR_PROMPT);
        assert_eq!(evidence.title, "修复指令");
        assert_eq!(evidence.summary, "failed");
        assert_eq!(evidence.content, "只修复 orchestrator completion 流程");
    }

    /// Business Logic（为什么需要这个函数）:
    ///     verifier 判定失败后，如果用户已经 Abort，系统不能把任务回退到 Preparing 并启动新 runner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Aborted 任务并调用 repair 转换 helper，断言 transitioned=false 且数据库仍为 Aborted。
    #[tokio::test]
    async fn repair_transition_skips_when_task_is_no_longer_verifying() {
        let repo = setup_orchestrator_repo().await;
        let task = command_task_row("task-aborted-repair", OrchestratorTaskStatus::Aborted);
        repo.create_task(&task).await.expect("insert task");

        let transition = transition_failed_verification_task_to_preparing(&repo, &task.id)
            .await
            .expect("transition result");
        let persisted = repo.get_task(&task.id).await.expect("persisted task");

        assert!(!transition.transitioned);
        assert_eq!(transition.task.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
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
