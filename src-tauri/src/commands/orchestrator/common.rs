//! Orchestrator 命令共享类型与 helper。
//!
//! Business Logic（为什么需要这个模块）:
//!     领域子模块共享 DTO 与 remote/local helper。
//!
//! Code Logic（这个模块做什么）:
//!     monofile 前部共享定义。

use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::models::{
    OrchestratorAttemptPhase, OrchestratorCreateAction, OrchestratorEvidenceDto,
    OrchestratorRunState, OrchestratorTaskAttemptRow, OrchestratorTaskDto, OrchestratorTaskRow,
    OrchestratorTaskStatus, OrchestratorWorkflowState, SplitTaskState, EVIDENCE_KIND_DELIVERY,
    EVIDENCE_KIND_REPAIR_PROMPT, EVIDENCE_KIND_VERIFICATION_OUTPUT,
    EVIDENCE_KIND_VERIFICATION_REVIEW,
};
use crate::orchestrator::outbox::{
    is_remote_network_error, mirror_payload_from_task, open_remote_project_for_shortcut,
    OrchestratorRemoteOutboxDto, RemoteMirrorTask,
};
use crate::orchestrator::prompt::RepairPromptContext;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::orchestrator::repo::{OrchestratorRecentEventRow, OrchestratorRepo};
use crate::orchestrator::runner::prepare_repair_runner;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetrySnapshot;
use crate::orchestrator::verifier::VerifierReview;
use crate::orchestrator::workflow::{resolve_project_workflow, WorkflowSource};
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_ids::{parse_remote_entity_id, remote_entity_id};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

pub(crate) const RUNTIME_TASK_SUMMARY_LIMIT: i64 = 6;
pub(crate) const RUNTIME_EVENT_LIMIT: i64 = 8;

/// 创建 Orchestrator 任务的命令入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端创建编排任务时只提交用户可编辑字段，后端统一补齐 id、状态、关联执行信息和时间戳。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 接收 Tauri invoke 参数，并保留 priority、createAction 与 tracker 预留字段用于归一。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskRequest {
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub priority: Option<i64>,
    #[serde(default)]
    pub create_action: OrchestratorCreateAction,
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
///     Workbench 自动化状态条需要展示调度器、workflow、执行槽位和任务运行摘要，帮助用户判断自动化为何运行或停滞。
///     未来 owning-device P2P 路由（T2）会通过 HTTP 把本 DTO 返回给请求端，
///     请求端需要把 JSON 反序列化回同一 DTO，因此除了 Serialize 还需要 Deserialize。
///
/// Code Logic（这个结构体做什么）:
///     聚合设备级 Settings、scheduler telemetry、项目 workflow resolver、repo 槽位统计、最近任务事件和远端可用性状态。
///     同时派生 Serialize 与 Deserialize，保证 owning-device 路由的响应可被远端客户端用同一类型解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeSnapshotDto {
    pub project_id: String,
    pub project_kind: String,
    pub remote_status: String,
    pub generated_at: String,
    pub latest_tick_at: Option<String>,
    pub last_dispatch_at: Option<String>,
    pub last_dispatched_count: usize,
    pub scheduler_enabled: bool,
    pub workflow_source: String,
    pub workflow_valid: bool,
    pub workflow_error: Option<String>,
    pub max_concurrent_tasks: i64,
    pub slots_used: i64,
    pub slots_available: i64,
    pub latest_error: Option<String>,
    pub running_tasks: Vec<OrchestratorRuntimeTaskSummaryDto>,
    pub retrying_tasks: Vec<OrchestratorRuntimeTaskSummaryDto>,
    pub recent_events: Vec<OrchestratorRuntimeEventDto>,
}

/// Orchestrator runtime snapshot 任务摘要 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     状态条需要以低噪音方式展示正在运行和等待重试的任务，用户不必展开完整任务卡片也能判断现场。
///     作为 OrchestratorRuntimeSnapshotDto 的嵌套字段，需要随父 DTO 一起被远端客户端反序列化。
///
/// Code Logic（这个结构体做什么）:
///     从 OrchestratorTaskRow 投影用户可识别字段和 runner runtime 字段，使用 camelCase 序列化给前端。
///     派生 Deserialize 以支持 owning-device P2P 路由响应的客户端解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeTaskSummaryDto {
    pub task_id: String,
    pub title: String,
    pub workflow_state: OrchestratorWorkflowState,
    pub run_state: OrchestratorRunState,
    pub attempt_phase: Option<OrchestratorAttemptPhase>,
    pub session_id: Option<String>,
    pub worktree_id: Option<String>,
    pub last_runtime_message: Option<String>,
    pub last_activity_at: Option<String>,
}

/// Orchestrator runtime snapshot 最近事件 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     状态条需要展示最近 scheduler/runner 事件，帮助用户理解任务为何运行、阻塞或等待。
///     作为 OrchestratorRuntimeSnapshotDto 的嵌套字段，需要随父 DTO 一起被远端客户端反序列化。
///
/// Code Logic（这个结构体做什么）:
///     从 orchestrator_task_events join 查询行投影可展示字段，不暴露 payload_json 等内部调试细节。
///     派生 Deserialize 以支持 owning-device P2P 路由响应的客户端解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRuntimeEventDto {
    pub id: String,
    pub task_id: String,
    pub task_title: String,
    pub kind: String,
    pub message: String,
    pub created_at: String,
}

/// Orchestrator 项目刷新结果 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     显式 refreshOrchestratorProject 动作需要告诉前端本次 best-effort 调度实际领取了多少任务。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 返回 projectId 与 dispatched 数量；remote 项目返回本机 shortcut projectId。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorProjectRefreshDto {
    pub project_id: String,
    pub dispatched: usize,
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
        prepare_claim_token: None,
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
pub(crate) fn non_empty_trimmed_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机 Tauri 创建入口也需要支持 Backlog/Todo/Start 三种动作，不能只让远端 HTTP 协议拥有该语义。
///
/// Code Logic（这个函数做什么）:
///     按 createAction 覆盖新任务 row 的 legacy status 与 split state，Start 与 Todo 入库语义一致。
pub(crate) fn apply_create_action_to_task_row(
    row: &mut OrchestratorTaskRow,
    create_action: OrchestratorCreateAction,
) {
    let split_state = SplitTaskState::from_create_action(create_action);
    row.status = create_action.initial_status();
    row.workflow_state = split_state.workflow_state;
    row.run_state = split_state.run_state;
    row.attempt_phase = None;
    row.blocked_reason = None;
}

/// Business Logic（为什么需要这个函数）:
///     Create and Start 应在创建成功后尝试触发 scheduler，但 Settings 关闭、容量不足或 runner 失败都不应让创建失败。
///
/// Code Logic（这个函数做什么）:
///     对 Start 调用 dispatch_once 并吞掉错误写日志，随后重读任务状态；非 Start 直接返回创建行。
pub(crate) async fn refresh_task_after_create_action(
    state: &AppState,
    row: OrchestratorTaskRow,
    create_action: OrchestratorCreateAction,
) -> Result<OrchestratorTaskRow, AppError> {
    if !create_action.should_dispatch_after_create() {
        return Ok(row);
    }

    let task_id = row.id.clone();
    if let Err(err) = crate::orchestrator::scheduler::dispatch_once(state).await {
        tracing::warn!(
            task_id = %task_id,
            error = %err,
            "orchestrator createAction=start dispatch failed after task creation"
        );
    }
    state.orchestrator_repo.get_task(&task_id).await.or(Ok(row))
}

/// Business Logic（为什么需要这个函数）:
///     多个本机创建入口都需要相同的校验、状态映射、落库和 Start 刷新语义。
///
/// Code Logic（这个函数做什么）:
///     构造任务 row，应用 createAction，插入 repo，并在 Start 时 best-effort dispatch 后返回最新 row。
pub(crate) async fn create_local_task_with_action(
    state: &AppState,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskRow, AppError> {
    let create_action = request.create_action;
    let mut row = build_orchestrator_task_row(request)?;
    apply_create_action_to_task_row(&mut row, create_action);
    state.orchestrator_repo.create_task(&row).await?;
    refresh_task_after_create_action(state, row, create_action).await
}

/// Business Logic（为什么需要这个函数）:
///     HTTP/mobile 本机创建必须同时支持 clientRequestId 幂等和三种 createAction，避免重试产生重复任务。
///     Start 动作的全局 dispatch 只能在首次插入后执行，重放不得再调度其他排队任务。
///
/// Code Logic（这个函数做什么）:
///     构造本机任务 row，调用 repo 幂等创建入口按 createAction 落库；
///     仅 `newly_created=true` 时对 Start 做 best-effort dispatch 并刷新，命中重放直接返回既有任务。
pub(crate) async fn create_local_task_for_client_request(
    state: &AppState,
    client_request_id: &str,
    request: CreateOrchestratorTaskRequest,
) -> Result<OrchestratorTaskRow, AppError> {
    let create_action = request.create_action;
    let row = build_orchestrator_task_row(request)?;
    let outcome = state
        .orchestrator_repo
        .create_remote_task_for_client_request(client_request_id, &row, create_action)
        .await?;
    if !outcome.newly_created {
        return Ok(outcome.task);
    }
    refresh_task_after_create_action(state, outcome.task, create_action).await
}

/// Business Logic（为什么需要这个函数）:
///     remote-aware 命令需要先确认 projectId 对应的 Workbench 项目，才能分流 local 与 remote shortcut。
///
/// Code Logic（这个函数做什么）:
///     从 workbench_project_repo 读取项目；缺失时返回 not_found。
pub(crate) async fn get_orchestrator_workbench_project(
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
///     从本地 Tauri create request 投影为 RemoteCreateOrchestratorTaskReq，保留 createAction；
///     同时生成一次性稳定 clientRequestId，若在线响应超时后落入 pending outbox，后续投递仍复用该幂等键。
pub(crate) fn remote_create_request_from_local(
    request: &CreateOrchestratorTaskRequest,
) -> RemoteCreateOrchestratorTaskReq {
    RemoteCreateOrchestratorTaskReq {
        project_id: request.project_id.clone(),
        title: request.title.clone(),
        goal: request.goal.clone(),
        acceptance_criteria: request.acceptance_criteria.clone(),
        priority: request.priority.unwrap_or(0),
        create_action: request.create_action,
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
///     runtime snapshot 需要展示任务摘要，而不是把完整任务 DTO 全量塞入状态条。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorTaskRow 克隆任务 id、标题、split state、执行现场和最近 runtime 文本字段。
pub(crate) fn runtime_task_summary_from_row(
    task: OrchestratorTaskRow,
) -> OrchestratorRuntimeTaskSummaryDto {
    OrchestratorRuntimeTaskSummaryDto {
        task_id: task.id,
        title: task.title,
        workflow_state: task.workflow_state,
        run_state: task.run_state,
        attempt_phase: task.attempt_phase,
        session_id: task.session_id,
        worktree_id: task.worktree_id,
        last_runtime_message: task.last_runtime_message,
        last_activity_at: task.last_activity_at,
    }
}

/// Business Logic（为什么需要这个函数）:
///     runtime snapshot 的 recentEvents 应只暴露前端易展示字段，避免 UI 解析数据库 payload。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorRecentEventRow 投影为 camelCase DTO。
pub(crate) fn runtime_event_from_row(
    row: OrchestratorRecentEventRow,
) -> OrchestratorRuntimeEventDto {
    OrchestratorRuntimeEventDto {
        id: row.id,
        task_id: row.task_id,
        task_title: row.task_title,
        kind: row.kind,
        message: row.message,
        created_at: row.created_at,
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端项目的 runtime snapshot 不能用本机 scheduler/config/workflow 冒充；
///     当对端不支持、离线或业务不可用时，必须返回明确状态的空快照，而不是伪装本机数据。
///
/// Code Logic（这个函数做什么）:
///     构造仅含 project 元信息与错误文案的空 snapshot；`remote_status` 与 `latest_error` 由调用方传入，
///     槽位/任务/事件清零，不读取本机 scheduler/config/workflow。
pub(crate) fn remote_runtime_snapshot_empty(
    project: &WorkbenchProjectRow,
    remote_status: &str,
    latest_error: &str,
) -> OrchestratorRuntimeSnapshotDto {
    OrchestratorRuntimeSnapshotDto {
        project_id: project.id.clone(),
        project_kind: project.kind.clone(),
        remote_status: remote_status.to_string(),
        generated_at: Utc::now().to_rfc3339(),
        latest_tick_at: None,
        last_dispatch_at: None,
        last_dispatched_count: 0,
        scheduler_enabled: false,
        workflow_source: "remoteUnavailable".to_string(),
        workflow_valid: false,
        workflow_error: Some("远端运行时快照暂不可用".to_string()),
        max_concurrent_tasks: 0,
        slots_used: 0,
        slots_available: 0,
        latest_error: Some(latest_error.to_string()),
        running_tasks: Vec::new(),
        retrying_tasks: Vec::new(),
        recent_events: Vec::new(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     兼容既有“capability 缺失 / unsupported”空快照入口，避免调用方重复拼装文案。
///
/// Code Logic（这个函数做什么）:
///     委托 `remote_runtime_snapshot_empty`，固定 remoteStatus=unsupported 与既有中文提示。
pub(crate) fn remote_runtime_snapshot_unavailable(
    project: &WorkbenchProjectRow,
) -> OrchestratorRuntimeSnapshotDto {
    remote_runtime_snapshot_empty(
        project,
        "unsupported",
        "远端项目暂不支持运行时快照；请在所属设备查看自动化状态",
    )
}

/// Business Logic（为什么需要这个函数）:
///     远端 runtime snapshot 的四态必须由 `PeerCallError` 变体类型驱动，
///     不能解析本地化错误文案，也不能把 404 当成 capability 判断。
///
/// Code Logic（这个函数做什么）:
///     按变体映射：Unsupported→unsupported，Network→offline，InvalidResponse/Remote→unavailable，
///     并返回对应空 snapshot（不读取本机运行时数据）。
pub(crate) fn remote_runtime_snapshot_from_peer_error(
    project: &WorkbenchProjectRow,
    error: crate::net::peer_error::PeerCallError,
) -> OrchestratorRuntimeSnapshotDto {
    use crate::net::peer_error::PeerCallError;
    match error {
        PeerCallError::Unsupported { .. } => remote_runtime_snapshot_unavailable(project),
        PeerCallError::Network { .. } => remote_runtime_snapshot_empty(
            project,
            "offline",
            "远端设备离线，暂时无法获取运行时快照",
        ),
        PeerCallError::InvalidResponse { .. } | PeerCallError::Remote { .. } => {
            remote_runtime_snapshot_empty(project, "unavailable", "远端运行时快照暂时不可用")
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     owning device 返回的 runtime snapshot 使用远端裸 ID 与 local projectId；
///     本机 shortcut 表面必须映射为 remote 实体 ID，并标记 live，才能与前端 remote-aware 通道一致。
///
/// Code Logic（这个函数做什么）:
///     仅改写身份/表面元数据：outer projectId=本机 shortcut id、projectKind=remote、remoteStatus=live，
///     并把 running/retrying/events 中的 task/worktree/session id 包成 `remote:<device>:<inner>`。
///     保留 owner 的 generatedAt/tick/slots/attempt/events/workflow 等字段，不替换本机 telemetry。
pub(crate) fn map_remote_runtime_snapshot_for_shortcut(
    mut snapshot: OrchestratorRuntimeSnapshotDto,
    remote_shortcut: &WorkbenchProjectRow,
) -> OrchestratorRuntimeSnapshotDto {
    snapshot.project_id = remote_shortcut.id.clone();
    snapshot.project_kind = "remote".to_string();
    snapshot.remote_status = "live".to_string();

    let map_task = |task: &mut OrchestratorRuntimeTaskSummaryDto| {
        task.task_id = remote_entity_id(&remote_shortcut.device_id, &task.task_id);
        task.worktree_id = task
            .worktree_id
            .as_deref()
            .map(|id| remote_entity_id(&remote_shortcut.device_id, id));
        task.session_id = task
            .session_id
            .as_deref()
            .map(|id| remote_entity_id(&remote_shortcut.device_id, id));
    };
    for task in &mut snapshot.running_tasks {
        map_task(task);
    }
    for task in &mut snapshot.retrying_tasks {
        map_task(task);
    }
    for event in &mut snapshot.recent_events {
        event.task_id = remote_entity_id(&remote_shortcut.device_id, &event.task_id);
    }
    snapshot
}

/// Business Logic（为什么需要这个函数）:
///     open-project/设备查找失败时不能把含 owner base URL 的 AppError 直接抛给 mobile/Tauri 调用方，
///     必须映射回四态空快照，保持 cold offline 与 unavailable 语义。
///
/// Code Logic（这个函数做什么）:
///     仅真实传输离线（设备缺失 / 本机 Network 类 Unavailable|Timeout，如连接中断）→ offline；
///     `AppError::Remote` 是在线对端返回的业务信封（含 unavailable/timeout code）→ unavailable，
///     不得把 503/504 业务响应误判为离线并展示陈旧缓存。
///     展示文案固定中文提示，不拼接 base_url/IP/端口，也不用中文 contains 判定四态。
pub(crate) fn remote_runtime_snapshot_from_open_error(
    project: &WorkbenchProjectRow,
    error: AppError,
) -> OrchestratorRuntimeSnapshotDto {
    // Remote 业务信封来自已可达的对端，协议/业务不可用 ≠ 设备传输离线。
    let is_remote_envelope = matches!(error, AppError::Remote { .. });
    if !is_remote_envelope && is_remote_network_error(&error) {
        return remote_runtime_snapshot_empty(
            project,
            "offline",
            "远端设备离线，暂时无法获取运行时快照",
        );
    }
    // 脱敏：业务/协议失败也绝不把 owner URL 写入 latest_error。
    let sanitized = error.to_string();
    let looks_like_url = sanitized.contains("http://") || sanitized.contains("https://");
    let message = if looks_like_url || sanitized.trim().is_empty() {
        "远端运行时快照暂时不可用".to_string()
    } else {
        sanitized
    };
    remote_runtime_snapshot_empty(project, "unavailable", &message)
}

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 runtime snapshot 必须向 owning device 拉取权威数据，
///     不能用本机 scheduler/config/workflow 冒充远端状态。
///
/// Code Logic（这个函数做什么）:
///     通过 open_remote_project_for_shortcut 解析 base_url 与远端 local projectId；
///     open/device preflight 失败映射 offline/unavailable 空快照（不泄漏 owner URL）；
///     调用 RemoteOrchestratorClient::runtime_snapshot 成功则映射 shortcut 身份字段，
///     peer 失败则按 PeerCallError 变体回落到 unsupported/offline/unavailable 空快照。
pub(crate) async fn get_remote_orchestrator_runtime_snapshot(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    forwarded_request_id: Option<&str>,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
    let context = match open_remote_project_for_shortcut(
        state,
        remote_shortcut,
        forwarded_request_id,
    )
    .await
    {
        Ok(context) => context,
        Err(error) => {
            return Ok(remote_runtime_snapshot_from_open_error(
                remote_shortcut,
                error,
            ));
        }
    };
    // Business Logic: 多跳代理（mobile→本机→owner）必须复用入站 request_id；Tauri IPC 无入站 ID 时由客户端生成。
    // 原样保留入站 request_id（含首尾空格），仅空串回落生成新 UUID。
    let request_id = forwarded_request_id
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(crate::net::request_context::new_request_id);
    let mut client = RemoteOrchestratorClient::new();
    // 原样转发入站 request_id（含首尾空格）；middleware 接受可打印 ASCII，trim 会破坏跨跳关联。
    if let Some(request_id) = forwarded_request_id.filter(|value| !value.is_empty()) {
        client = client.with_forwarded_request_id(request_id);
    }
    match client
        .runtime_snapshot(&context.base_url, &context.remote_project_id, &request_id)
        .await
    {
        Ok(owner_snapshot) => Ok(map_remote_runtime_snapshot_for_shortcut(
            owner_snapshot,
            remote_shortcut,
        )),
        Err(error) => Ok(remote_runtime_snapshot_from_peer_error(
            remote_shortcut,
            error,
        )),
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机任务视图需要统一包装成 discriminated union，供 Phase 6 UI 与远端视图合并展示。
///
/// Code Logic（这个函数做什么）:
///     把 OrchestratorTaskRow 转为 OrchestratorTaskDto 后包装为 Local 变体。
pub(crate) fn local_task_view(row: OrchestratorTaskRow) -> OrchestratorTaskViewDto {
    OrchestratorTaskViewDto::Local {
        task: OrchestratorTaskDto::from(row),
    }
}

/// Business Logic（为什么需要这个函数）:
///     runtime snapshot 需要把强类型 workflow 来源转成稳定前端文案键，而不是让 UI 猜测 Rust enum 形状。
///
/// Code Logic（这个函数做什么）:
///     将 WorkflowSource 映射为 camelCase 字符串；解析失败时由调用方使用 invalidProjectOverride。
pub(crate) fn workflow_source_label(source: WorkflowSource) -> &'static str {
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
pub(crate) fn invalid_workflow_source_label(project_path: &Path) -> &'static str {
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
///     本函数是本机命令与未来 owning-device P2P 路由（T2）共享的唯一构造入口，
///     两端必须使用同一份本地快照构造逻辑，避免远端设备状态条与本机状态条出现分叉。
///
/// Code Logic（这个函数做什么）:
///     纯本地快照构造：解析 WORKFLOW.md、统计槽位、任务摘要和最近事件，组装 remoteStatus=local 的 DTO。
///     本函数不再内部分支远端 shortcut——调用方必须先解析并校验 project.kind，
///     远端 shortcut 应由命令/路由层先调用 remote_runtime_snapshot_unavailable 提前返回，
///     再把已校验的本地 WorkbenchProject 传入本函数。这样 P2P owning-device 路由可以
///     直接复用本构造逻辑而无需复制代码或处理与己无关的远端语义。
pub(crate) async fn get_orchestrator_runtime_snapshot_for_project(
    repo: &OrchestratorRepo,
    config: &OrchestratorAutomationConfig,
    project: &WorkbenchProjectRow,
    scheduler_snapshot: &OrchestratorSchedulerTelemetrySnapshot,
) -> Result<OrchestratorRuntimeSnapshotDto, AppError> {
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
    let latest_error = repo
        .latest_blocked_reason_for_project(&project.id)
        .await?
        .or_else(|| scheduler_snapshot.latest_error.clone());
    let running_tasks = repo
        .list_active_runtime_tasks_for_project(&project.id, RUNTIME_TASK_SUMMARY_LIMIT)
        .await?
        .into_iter()
        .map(runtime_task_summary_from_row)
        .collect();
    let retrying_tasks = repo
        .list_retrying_runtime_tasks_for_project(&project.id, RUNTIME_TASK_SUMMARY_LIMIT)
        .await?
        .into_iter()
        .map(runtime_task_summary_from_row)
        .collect();
    let recent_events = repo
        .list_recent_events_for_project(&project.id, RUNTIME_EVENT_LIMIT)
        .await?
        .into_iter()
        .map(runtime_event_from_row)
        .collect();

    Ok(OrchestratorRuntimeSnapshotDto {
        project_id: project.id.clone(),
        project_kind: project.kind.clone(),
        remote_status: "local".to_string(),
        generated_at: Utc::now().to_rfc3339(),
        latest_tick_at: scheduler_snapshot.latest_tick_at.clone(),
        last_dispatch_at: scheduler_snapshot.last_dispatch_at.clone(),
        last_dispatched_count: scheduler_snapshot.last_dispatched_count,
        scheduler_enabled: config.enabled,
        workflow_source,
        workflow_valid,
        workflow_error,
        max_concurrent_tasks: max_slots,
        slots_used,
        slots_available,
        latest_error,
        running_tasks,
        retrying_tasks,
        recent_events,
    })
}

/// Business Logic（为什么需要这个函数）:
///     远端任务返回给本机前端时必须使用 Workbench remote id 通道，后续 queue/retry/abort/evidence 才能安全回到正确设备。
///
/// Code Logic（这个函数做什么）:
///     将远端裸 task/project/worktree/session id 映射为本机可识别的 remote:<deviceId>:<inner> id；
///     task.project_id 则替换成本机 remote shortcut project.id。
pub(crate) fn map_remote_task_for_shortcut(
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
pub(crate) fn map_remote_evidence_for_shortcut(
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
pub(crate) fn remote_task_view(
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
pub(crate) fn remote_inner_task_id_for_shortcut(
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
pub(crate) fn remote_mirror_views(
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
pub(crate) async fn pending_remote_task_views_for_project(
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
pub(crate) async fn create_remote_orchestrator_task_online(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    mut request: RemoteCreateOrchestratorTaskReq,
    forwarded_request_id: Option<&str>,
) -> Result<OrchestratorTaskViewDto, AppError> {
    // Business Logic: mobile→本机→owner 创建任务必须复用同一 request_id，便于多跳排障。
    let context =
        open_remote_project_for_shortcut(state, remote_shortcut, forwarded_request_id).await?;
    request.project_id = context.remote_project_id.clone();
    let mut client = RemoteOrchestratorClient::new();
    // 原样转发入站 request_id（含首尾空格）；middleware 接受可打印 ASCII，trim 会破坏跨跳关联。
    if let Some(request_id) = forwarded_request_id.filter(|value| !value.is_empty()) {
        client = client.with_forwarded_request_id(request_id);
    }
    let task = client.create_task(&context.base_url, request).await?;
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
pub(crate) async fn upsert_remote_task_view(
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
pub(crate) async fn update_remote_orchestrator_task_status<F, Fut>(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    task_id: &str,
    operation: F,
) -> Result<OrchestratorTaskViewDto, AppError>
where
    F: FnOnce(RemoteOrchestratorClient, String, String) -> Fut,
    Fut: std::future::Future<Output = Result<OrchestratorTaskDto, AppError>>,
{
    let context = open_remote_project_for_shortcut(state, remote_shortcut, None).await?;
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
pub(crate) fn build_dispatch_once_response(dispatched: usize) -> serde_json::Value {
    serde_json::json!({ "dispatched": dispatched })
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成后的验证命令优先来自项目 WORKFLOW.md；项目未声明时才使用 Settings 全局默认。
///
/// Code Logic（这个函数做什么）:
///     每次 completion 动态解析项目 workflow；validation.commands 非空则返回项目命令，否则克隆全局 verification_commands。
pub(crate) fn validation_commands_for_agent_completion(
    project_path: &Path,
    config: &OrchestratorAutomationConfig,
) -> Result<Vec<String>, AppError> {
    let workflow = resolve_project_workflow(project_path)?;
    if workflow.validation_commands.is_empty() {
        Ok(config.verification_commands.clone())
    } else {
        Ok(workflow.validation_commands)
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证通过后是否自动交付由全局自动化开关和四个交付开关共同决定；默认关闭自动化时必须停在人工复核。
///
/// Code Logic（这个函数做什么）:
///     对 enabled、auto_commit、auto_push_task_branch、auto_merge_to_main、auto_push_main 执行 AND 判断。
pub(crate) fn auto_delivery_enabled(config: &OrchestratorAutomationConfig) -> bool {
    config.enabled
        && config.auto_commit
        && config.auto_push_task_branch
        && config.auto_merge_to_main
        && config.auto_push_main
}

/// Business Logic（为什么需要这个函数）:
///     显式 deliverReviewedTask 入口必须和自动 completion pipeline 使用同一 Settings 总闸，避免 UI 按钮绕过交付策略。
///
/// Code Logic（这个函数做什么）:
///     复用 auto_delivery_enabled 判断 enabled 与四个 delivery flag；未全部开启时返回可读业务错误。
pub(crate) fn ensure_reviewed_delivery_allowed(
    config: &OrchestratorAutomationConfig,
) -> Result<(), AppError> {
    if auto_delivery_enabled(config) {
        Ok(())
    } else {
        Err(AppError::generic(
            "Settings 未允许 full-auto delivery，无法交付人工复核任务",
        ))
    }
}

/// Business Logic（为什么需要这个函数）:
///     start/refresh 是显式用户动作，但调度 Runner 仍应尽量沿用后台 scheduler 的统一逻辑，且不能因为一次调度失败破坏任务入队。
///
/// Code Logic（这个函数做什么）:
///     调用 scheduler::dispatch_once；成功返回 dispatched 数量，失败仅记录 warn 并返回 0。
pub(crate) async fn dispatch_orchestrator_best_effort(state: &AppState) -> usize {
    match crate::orchestrator::scheduler::dispatch_once(state).await {
        Ok(dispatched) => dispatched,
        Err(err) => {
            tracing::warn!(error = %err, "Orchestrator 显式刷新调度失败");
            0
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     显式 task action 不能作用在 pending remote outbox 上，因为它还没有 owning device 上的真实 taskId。
///
/// Code Logic（这个函数做什么）:
///     按 taskId 查询 remote outbox；存在则返回业务错误，不存在时允许后续 remote id 解析继续。
pub(crate) async fn reject_pending_remote_task_action(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<(), AppError> {
    if repo.get_remote_outbox_item(task_id).await?.is_some() {
        return Err(AppError::generic("远端待发送任务尚未创建，不能执行该动作"));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     本机显式 action 必须确认任务属于当前 Workbench 项目，避免 stale drawer 操作误改其它项目任务。
///
/// Code Logic（这个函数做什么）:
///     读取任务并比较 project_id；匹配时返回任务行，缺失或不匹配时返回统一业务错误。
pub(crate) async fn get_local_project_task_for_action(
    repo: &OrchestratorRepo,
    project_id: &str,
    task_id: &str,
) -> Result<OrchestratorTaskRow, AppError> {
    let task = repo.get_task(task_id).await?;
    if task.project_id != project_id {
        return Err(AppError::generic("任务不属于当前项目"));
    }
    Ok(task)
}

/// Business Logic（为什么需要这个函数）:
///     Agent 完成按钮只能用于 Running 任务，避免草稿、排队、交付或终态任务被误推进验证阶段。
///
/// Code Logic（这个函数做什么）:
///     检查任务状态是否为 Running；不满足时返回中文业务错误。
#[cfg(test)]
pub(crate) fn ensure_task_can_complete_agent_run(
    task: &OrchestratorTaskRow,
) -> Result<(), AppError> {
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
pub(crate) fn retry_orchestrator_task_target_status(
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
pub(crate) fn abort_orchestrator_task_target_status(
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
pub(crate) struct VerificationEvidence {
    pub(crate) kind: &'static str,
    pub(crate) title: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) content: String,
}

/// 验证失败后需要落库的阻塞结果。
///
/// Business Logic（为什么需要这个结构体）:
///     任务进入 Verifying 后，任一可预期错误都必须同时产生 failed evidence 和 Blocked 原因，避免流程半停在 Verifying。
///
/// Code Logic（这个结构体做什么）:
///     保存最终 blocked_reason 与对应 verificationOutput evidence，便于 side-effect helper 统一落库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerificationFailureOutcome {
    pub(crate) reason: String,
    pub(crate) evidence: VerificationEvidence,
}

/// 验证完成后进入交付阶段的条件转换结果。
///
/// Business Logic（为什么需要这个结构体）:
///     验证期间用户可能终止任务，命令层需要区分是否真正从 Verifying 取得 Delivering 执行权。
///
/// Code Logic（这个结构体做什么）:
///     transitioned=true 表示 Verifying->Delivering 原子转换命中；false 表示状态已变化，task 为当前最新任务。
pub(crate) struct ConditionalDeliveryTransition {
    pub(crate) transitioned: bool,
    pub(crate) task: OrchestratorTaskRow,
}

/// 验证失败后回到修复准备阶段的条件转换结果。
///
/// Business Logic（为什么需要这个结构体）:
///     verifier 判定失败后，任务可能已被用户 Abort；命令层必须知道是否真正取得 Verifying->Preparing 执行权。
///
/// Code Logic（这个结构体做什么）:
///     transitioned=true 表示原子转换命中；false 表示状态已变化，task 为当前最新任务。
pub(crate) struct ConditionalRepairTransition {
    pub(crate) transitioned: bool,
    pub(crate) task: OrchestratorTaskRow,
}

/// Business Logic（为什么需要这个函数）:
///     验证前置失败也必须落为 failed verificationOutput evidence，用户才能在任务详情看到可审计原因。
///
/// Code Logic（这个函数做什么）:
///     把失败原因转换为 add_evidence 的固定 kind/title/summary 参数与 content 文本。
pub(crate) fn verification_failure_evidence(reason: &str) -> VerificationEvidence {
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
pub(crate) fn verification_review_evidence(review: &VerifierReview) -> VerificationEvidence {
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
pub(crate) fn verification_review_failure_outcome(
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
pub(crate) fn repair_prompt_evidence(repair_prompt: &str) -> VerificationEvidence {
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
pub(crate) fn verification_failure_outcome(
    context: &str,
    err: &AppError,
) -> VerificationFailureOutcome {
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
pub(crate) fn verification_failure_outcome_from_reason(reason: &str) -> VerificationFailureOutcome {
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
pub(crate) async fn add_verification_evidence(
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
///     验证失败、缺少 worktree 或找不到 worktree 时，任务应进入 Blocked 并保留明确原因；
///     experiment candidate 必须同步 Failed，否则组永久 Running。
///
/// Code Logic（这个函数做什么）:
///     仅当任务仍处于 Verifying 时写入 Blocked 和 blocked_reason；
///     若为 experiment 且最终 Blocked/Aborted，调用 sync_candidate_with_task_terminal。
pub(crate) async fn block_task_with_reason(
    state: &AppState,
    task_id: &str,
    reason: &str,
) -> Result<OrchestratorTaskDto, AppError> {
    let task = state
        .orchestrator_repo
        .block_task_if_verifying(task_id, reason)
        .await?;
    if task.status == OrchestratorTaskStatus::Blocked
        && task.blocked_reason.as_deref() == Some(reason)
    {
        crate::orchestrator::notifications::emit_task_operational_notification(
            state,
            crate::orchestrator::models::OperationalNotificationKind::Blocked,
            &task,
        );
    }
    if task.experiment_id.is_some()
        && matches!(
            task.status,
            OrchestratorTaskStatus::Blocked | OrchestratorTaskStatus::Aborted
        )
    {
        if let Err(err) =
            crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                state.orchestrator_repo.as_ref(),
                task_id,
                task.status,
            )
            .await
        {
            tracing::debug!(
                task_id = %task_id,
                "sync_candidate_with_task_terminal after block: {err}"
            );
        }
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     验证通过或跳过后只有仍处于 Verifying 的任务可以进入 Delivering，避免用户 Abort 被旧流程覆盖。
///
/// Code Logic（这个函数做什么）:
///     调用仓储 try_transition_task_status 做 Verifying->Delivering 条件写；未命中时读取当前任务并标记 skipped。
pub(crate) async fn transition_verified_task_to_delivering(
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
pub(crate) async fn transition_verified_task_to_human_review(
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
///     修复轮 prepare 强制要求非空 claim token，转换时必须原子签发。
///
/// Code Logic（这个函数做什么）:
///     调用专用 Verifying→Preparing helper，同时写入 Rework/Preparing/Failed 并签发 prepare_claim_token；
///     未命中时读取当前任务并标记未转换。
pub(crate) async fn transition_failed_verification_task_to_preparing(
    repo: &OrchestratorRepo,
    task_id: &str,
) -> Result<ConditionalRepairTransition, AppError> {
    match repo
        .try_transition_verifying_to_preparing_with_claim(task_id)
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
pub(crate) async fn stop_verification_if_task_changed(
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
pub(crate) async fn block_task_with_delivery_error(
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
        crate::orchestrator::notifications::emit_task_operational_notification(
            state,
            crate::orchestrator::models::OperationalNotificationKind::Blocked,
            &task,
        );
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     手动完成和 terminal sentinel 共用 completion pipeline；只要任务成功从 Running 进入 Verifying，
///     当前 active running attempt 就应标记为 completed，避免重复 sentinel 或重复点击再次定位到旧 attempt。
///
/// Code Logic（这个函数做什么）:
///     使用任务行上的 active attempt 编号调用 OrchestratorRepo::mark_attempt_completed；缺少 attempt 时返回业务错误。
pub(crate) async fn mark_active_running_attempt_completed(
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
///     人工复核交付若在 commit 边界检测到 review digest 漂移，必须回退 Human Review 并向上抛 Conflict，
///     不能折叠成 Blocked，否则前端无法强制重新审阅。
///
/// Code Logic（这个函数做什么）:
///     构造 AppDeliveryContext 调用 delivery::deliver_task(expected_review_digest)；
///     成功返回 summary.task；`review_diff_changed` 时若仍 Delivering 则 CAS 回 Done+HumanReview+Idle，
///     不写 delivery evidence，并传播原 Conflict；其它错误条件 Block 当前 Delivering 任务。
pub(crate) async fn run_delivery_for_task(
    state: &AppState,
    task_id: &str,
    expected_review_digest: Option<&str>,
) -> Result<OrchestratorTaskDto, AppError> {
    let delivery_context = crate::orchestrator::delivery::AppDeliveryContext::new(state);
    match crate::orchestrator::delivery::deliver_task(
        &delivery_context,
        task_id,
        expected_review_digest,
    )
    .await
    {
        Ok(summary) => {
            tracing::debug!(task_id = %task_id, stages = ?summary.stages, "Orchestrator 自动交付完成");
            // delivery 成功后可能是 Done 或中途 Blocked；从权威 row 读 state_version 再发运营通知。
            if let Ok(row) = state.orchestrator_repo.get_task(task_id).await {
                if row.status == OrchestratorTaskStatus::Done
                    && row.workflow_state
                        == crate::orchestrator::models::OrchestratorWorkflowState::Done
                {
                    crate::orchestrator::notifications::emit_task_operational_notification(
                        state,
                        crate::orchestrator::models::OperationalNotificationKind::TaskDone,
                        &row,
                    );
                } else if row.status == OrchestratorTaskStatus::Blocked {
                    crate::orchestrator::notifications::emit_task_operational_notification(
                        state,
                        crate::orchestrator::models::OperationalNotificationKind::Blocked,
                        &row,
                    );
                }
            }
            Ok(summary.task)
        }
        Err(err) if err.code() == super::REVIEW_DIFF_CHANGED_CODE => {
            // commit 边界 digest 漂移：回退 Human Review，保留 Conflict 供前端强制 re-review。
            let _ = state
                .orchestrator_repo
                .revert_delivery_to_human_review(task_id)
                .await?;
            Err(err)
        }
        Err(err) => block_task_with_delivery_error(state, task_id, err).await,
    }
}

/// Business Logic（为什么需要这个函数）:
///     验证命令无法启动或无法定位 worktree 时，任务既要 Blocked，也要保留 failed verification evidence。
///
/// Code Logic（这个函数做什么）:
///     先追加 outcome.evidence，再用 outcome.reason 写 Blocked，确保两处展示内容一致。
pub(crate) async fn block_task_with_verification_outcome(
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
pub(crate) async fn block_task_with_verification_failure(
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
pub(crate) async fn block_task_with_verification_error(
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
pub(crate) async fn block_task_with_verification_review_error(
    state: &AppState,
    task_id: &str,
    context: &str,
    err: AppError,
) -> Result<OrchestratorTaskDto, AppError> {
    let outcome = verification_review_failure_outcome(context, &err);
    block_task_with_verification_outcome(state, task_id, &outcome).await
}

/// Business Logic（为什么需要这个函数）:
///     修复 runner 准备失败时任务不能永久停在 Preparing 或 runner bootstrap 的 Running 状态；
///     experiment candidate 同步 Failed + reduce，避免 Ok(Blocked) 永久卡住组。
///
/// Code Logic（这个函数做什么）:
///     按 scheduler 语义尝试 Preparing->Blocked，再尝试 Running->Blocked；
///     命中后写 event/通知；experiment 任务调用 sync_candidate_with_task_terminal。
pub(crate) async fn block_task_with_repair_runner_error(
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
        crate::orchestrator::notifications::emit_task_operational_notification(
            state,
            crate::orchestrator::models::OperationalNotificationKind::Blocked,
            &task,
        );
        task
    } else {
        repo.get_task(task_id).await?
    };
    if task.experiment_id.is_some()
        && matches!(
            task.status,
            OrchestratorTaskStatus::Blocked | OrchestratorTaskStatus::Aborted
        )
    {
        if let Err(sync_err) =
            crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                repo,
                task_id,
                task.status,
            )
            .await
        {
            tracing::debug!(
                task_id = %task_id,
                "sync_candidate after repair block: {sync_err}"
            );
        }
    }
    Ok(OrchestratorTaskDto::from(task))
}

/// Business Logic（为什么需要这个函数）:
///     verifier 判定失败后应写 repairPrompt evidence，并在任务仍 Verifying 时回到 Preparing 启动同 worktree 修复轮。
///
/// Code Logic（这个函数做什么）:
///     先做 Verifying->Preparing 条件转换；命中后追加 repairPrompt evidence 并调用 prepare_repair_runner，
///     准备失败时按 runner failure 语义条件阻塞。
pub(crate) async fn start_repair_runner_for_failed_review(
    state: &AppState,
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
    match prepare_repair_runner(state, &repair_transition.task, repair_context).await {
        Ok(task) => {
            // prepare 可能返回 Ok(Blocked)（max turns / provider 等），experiment 必须 Failed。
            if task.experiment_id.is_some()
                && matches!(
                    task.status,
                    OrchestratorTaskStatus::Blocked | OrchestratorTaskStatus::Aborted
                )
            {
                if let Err(err) =
                    crate::orchestrator::experiments::reducer::sync_candidate_with_task_terminal(
                        state.orchestrator_repo.as_ref(),
                        task_id,
                        task.status,
                    )
                    .await
                {
                    tracing::debug!(
                        task_id = %task_id,
                        "sync_candidate after repair Ok(Blocked): {err}"
                    );
                }
            }
            Ok(OrchestratorTaskDto::from(task))
        }
        Err(err) => block_task_with_repair_runner_error(state, task_id, err).await,
    }
}
