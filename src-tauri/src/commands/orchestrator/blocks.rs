//! 任务块 create / append / reorder 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     看板 lane+ 与块卡片需要独立于单任务 create 的整组 API；GuiClient 必须走 sidecar，不得双写。
//!
//! Code Logic（这个模块做什么）:
//!     Tauri 入口按 RuntimeRole 分流；owner 路径写 repo；remote shortcut 转发或写 pending outbox。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::error::AppError;
use crate::orchestrator::models::{OrchestratorCreateAction, OrchestratorTaskBlockDto};
use crate::orchestrator::outbox::{is_remote_network_error, open_remote_project_for_shortcut};
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::{
    RemoteAppendTaskBlockMemberReq, RemoteCreateOrchestratorTaskBlockReq,
    RemoteReorderTaskBlockMembersReq, RemoteTaskBlockMemberReq,
};
use crate::orchestrator::repo::BlockMemberDraft;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use super::common::{
    dispatch_orchestrator_best_effort, get_orchestrator_workbench_project, local_task_view,
    upsert_remote_task_view, OrchestratorTaskViewDto,
};

/// 创建任务块的 Tauri/HTTP 入参。
///
/// Business Logic（为什么需要这个结构体）:
///     前端只提交块标题、2–8 个成员三字段和显式 createAction。
///
/// Code Logic（这个结构体做什么）:
///     camelCase：projectId/title/members/createAction/clientRequestId。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskBlockRequest {
    pub project_id: String,
    pub title: String,
    pub members: Vec<CreateOrchestratorTaskBlockMember>,
    #[serde(default)]
    pub create_action: OrchestratorCreateAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
}

/// 创建任务块成员入参。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOrchestratorTaskBlockMember {
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
}

/// 追加块成员入参。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendOrchestratorTaskBlockMemberRequest {
    pub project_id: String,
    pub block_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
}

/// 重排块成员入参。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReorderOrchestratorTaskBlockMembersRequest {
    pub project_id: String,
    pub block_id: String,
    pub ordered_task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
}

/// 看板创建块响应（带 origin 视图）。
///
/// Business Logic（为什么需要这个结构体）:
///     桌面/移动看板要立刻 upsert 全部成员视图。
///
/// Code Logic（这个结构体做什么）:
///     camelCase：block + task views。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorTaskBlockViewCreatedDto {
    pub block: OrchestratorTaskBlockDto,
    pub tasks: Vec<OrchestratorTaskViewDto>,
}

/// Business Logic（为什么需要这个函数）:
///     用户在 Backlog/Todo 点 + 创建串行任务块；GuiClient 不得写本机空库。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control sidecar；owner → `create_orchestrator_task_block_view_for_state`。
#[tauri::command]
pub async fn create_orchestrator_task_block_view(
    state: State<'_, AppState>,
    request: CreateOrchestratorTaskBlockRequest,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .create_orchestrator_task_block(request)
            .await;
    }
    create_orchestrator_task_block_view_for_state(&state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     HTTP/P2P 与 Tauri 共用创建入口，必须保留 clientRequestId 幂等。
///
/// Code Logic（这个函数做什么）:
///     local → repo 事务建块；remote → 在线转发或 pending outbox。
pub async fn create_orchestrator_task_block_view_for_state(
    state: &AppState,
    request: CreateOrchestratorTaskBlockRequest,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind == "remote" {
        return create_remote_task_block(state, &project, request).await;
    }
    if project.kind != "local" {
        return Err(AppError::generic(
            "仅本机 owning 项目或远端 shortcut 可创建任务块",
        ));
    }
    create_local_task_block(state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     owning device 上必须整组落库，Start 仅首次插入后 best-effort dispatch。
///
/// Code Logic（这个函数做什么）:
///     映射成员 draft → repo.create_task_block_idempotent；newly_created 且 start 时唤醒调度。
pub async fn create_local_task_block(
    state: &AppState,
    request: CreateOrchestratorTaskBlockRequest,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    let drafts = drafts_from_members(&request.members)?;
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let outcome = state
        .orchestrator_repo
        .create_task_block_idempotent(
            client_request_id,
            &request.project_id,
            &request.title,
            &drafts,
            request.create_action,
        )
        .await?;
    if outcome.newly_created && request.create_action.should_dispatch_after_create() {
        let state_bg = state.clone();
        tokio::spawn(async move {
            let _ = dispatch_orchestrator_best_effort(&state_bg).await;
        });
    }
    Ok(OrchestratorTaskBlockViewCreatedDto {
        block: OrchestratorTaskBlockDto::from(outcome.block),
        tasks: outcome.tasks.into_iter().map(local_task_view).collect(),
    })
}

/// Business Logic（为什么需要这个函数）:
///     P2P/mobile 入站必须强制非空 clientRequestId，才能与 ledger 对齐。
///
/// Code Logic（这个函数做什么）:
///     校验 request id 后委托 for_state。
pub async fn create_orchestrator_task_block_view_for_http(
    state: &AppState,
    req: RemoteCreateOrchestratorTaskBlockReq,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::generic("创建任务块缺少 clientRequestId"))?;
    let created = create_orchestrator_task_block_view_for_state(
        state,
        CreateOrchestratorTaskBlockRequest {
            project_id: req.project_id,
            title: req.title,
            members: req
                .members
                .into_iter()
                .map(|member| CreateOrchestratorTaskBlockMember {
                    title: member.title,
                    goal: member.goal,
                    acceptance_criteria: member.acceptance_criteria,
                })
                .collect(),
            create_action: req.create_action,
            client_request_id: Some(client_request_id.to_string()),
        },
    )
    .await?;
    Ok(created)
}

/// Business Logic（为什么需要这个函数）:
///     用户在块末尾追加下一步；复核/交付后拒绝。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → sidecar；否则 for_state。
#[tauri::command]
pub async fn append_orchestrator_task_block_member_view(
    state: State<'_, AppState>,
    request: AppendOrchestratorTaskBlockMemberRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .append_orchestrator_task_block_member(request)
            .await;
    }
    append_orchestrator_task_block_member_view_for_state(&state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     HTTP 与 Tauri 共用追加入口。
///
/// Code Logic（这个函数做什么）:
///     local → repo append；remote → 转发或 pending outbox。
pub async fn append_orchestrator_task_block_member_view_for_state(
    state: &AppState,
    request: AppendOrchestratorTaskBlockMemberRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind == "remote" {
        return append_remote_block_member(state, &project, request).await;
    }
    if project.kind != "local" {
        return Err(AppError::generic(
            "仅本机 owning 项目或远端 shortcut 可追加任务块成员",
        ));
    }
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let outcome = state
        .orchestrator_repo
        .append_task_block_member_idempotent(
            client_request_id,
            &request.project_id,
            &request.block_id,
            &request.title,
            &request.goal,
            &request.acceptance_criteria,
        )
        .await?;
    Ok(local_task_view(outcome.task))
}

/// Business Logic（为什么需要这个函数）:
///     P2P 追加必须带非空 clientRequestId。
///
/// Code Logic（这个函数做什么）:
///     校验后委托 for_state，原样返回 Local/Remote/PendingRemote 视图。
pub async fn append_orchestrator_task_block_member_for_http(
    state: &AppState,
    req: RemoteAppendTaskBlockMemberReq,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::generic("追加任务块成员缺少 clientRequestId"))?;
    let view = append_orchestrator_task_block_member_view_for_state(
        state,
        AppendOrchestratorTaskBlockMemberRequest {
            project_id: req.project_id,
            block_id: req.block_id,
            title: req.title,
            goal: req.goal,
            acceptance_criteria: req.acceptance_criteria,
            client_request_id: Some(client_request_id.to_string()),
        },
    )
    .await?;
    Ok(view)
}

/// Business Logic（为什么需要这个函数）:
///     Backlog/Todo 空闲块允许调整顺序；GuiClient 走 sidecar。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control；owner → for_state。
#[tauri::command]
pub async fn reorder_orchestrator_task_block_members_view(
    state: State<'_, AppState>,
    request: ReorderOrchestratorTaskBlockMembersRequest,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .reorder_orchestrator_task_block_members(request)
            .await;
    }
    reorder_orchestrator_task_block_members_view_for_state(&state, request).await
}

/// Business Logic（为什么需要这个函数）:
///     HTTP 与 Tauri 共用重排入口。
///
/// Code Logic（这个函数做什么）:
///     local → repo reorder；remote → 转发或 pending outbox。
pub async fn reorder_orchestrator_task_block_members_view_for_state(
    state: &AppState,
    request: ReorderOrchestratorTaskBlockMembersRequest,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let project = get_orchestrator_workbench_project(state, &request.project_id).await?;
    if project.kind == "remote" {
        return reorder_remote_block_members(state, &project, request).await;
    }
    if project.kind != "local" {
        return Err(AppError::generic(
            "仅本机 owning 项目或远端 shortcut 可重排任务块",
        ));
    }
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (_block, tasks, _newly) = state
        .orchestrator_repo
        .reorder_task_block_members_idempotent(
            client_request_id,
            &request.project_id,
            &request.block_id,
            &request.ordered_task_ids,
        )
        .await?;
    Ok(tasks.into_iter().map(local_task_view).collect())
}

/// Business Logic（为什么需要这个函数）:
///     P2P 重排必须带非空 clientRequestId。
///
/// Code Logic（这个函数做什么）:
///     校验后委托 for_state，原样返回成员视图列表。
pub async fn reorder_orchestrator_task_block_members_for_http(
    state: &AppState,
    req: RemoteReorderTaskBlockMembersReq,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let client_request_id = req
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::generic("重排任务块成员缺少 clientRequestId"))?;
    let views = reorder_orchestrator_task_block_members_view_for_state(
        state,
        ReorderOrchestratorTaskBlockMembersRequest {
            project_id: req.project_id,
            block_id: req.block_id,
            ordered_task_ids: req.ordered_task_ids,
            client_request_id: Some(client_request_id.to_string()),
        },
    )
    .await?;
    Ok(views)
}

/// Business Logic（为什么需要这个函数）:
///     remote shortcut 必须整组转发 owner；离线写一条带 mutationKind 的 outbox。
///
/// Code Logic（这个函数做什么）:
///     在线 POST create-block；网络错误 enqueue pending；capability 失败 fail-closed。
async fn create_remote_task_block(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: CreateOrchestratorTaskBlockRequest,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut remote_req = RemoteCreateOrchestratorTaskBlockReq {
        project_id: request.project_id.clone(),
        title: request.title.clone(),
        members: request
            .members
            .iter()
            .map(|member| RemoteTaskBlockMemberReq {
                title: member.title.clone(),
                goal: member.goal.clone(),
                acceptance_criteria: member.acceptance_criteria.clone(),
            })
            .collect(),
        create_action: request.create_action,
        client_request_id: Some(client_request_id),
        mutation_kind: Some("createBlock".to_string()),
    };
    match open_remote_project_for_shortcut(state, remote_shortcut, None).await {
        Ok(context) => {
            remote_req.project_id = context.remote_project_id.clone();
            match RemoteOrchestratorClient::new()
                .with_expected_device_id(&remote_shortcut.device_id)
                .create_task_block(&context.base_url, remote_req.clone())
                .await
            {
                Ok(created) => {
                    let mut views = Vec::new();
                    for task in created.tasks {
                        views.push(
                            upsert_remote_task_view(
                                state,
                                remote_shortcut,
                                &context.remote_project_id,
                                &context.remote_project_path,
                                task,
                            )
                            .await?,
                        );
                    }
                    Ok(OrchestratorTaskBlockViewCreatedDto {
                        block: created.block,
                        tasks: views,
                    })
                }
                Err(err) if is_remote_network_error(&err) => {
                    enqueue_pending_block_mutation(state, remote_shortcut, &remote_req).await
                }
                Err(err) => Err(err),
            }
        }
        Err(err) if is_remote_network_error(&err) => {
            enqueue_pending_block_mutation(state, remote_shortcut, &remote_req).await
        }
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端离线时追加也必须保留一条 outbox，恢复后按同一 fingerprint 投递。
///
/// Code Logic（这个函数做什么）:
///     在线 POST append；网络错误写 pending。
async fn append_remote_block_member(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: AppendOrchestratorTaskBlockMemberRequest,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut remote_req = RemoteAppendTaskBlockMemberReq {
        project_id: request.project_id.clone(),
        block_id: request.block_id.clone(),
        title: request.title.clone(),
        goal: request.goal.clone(),
        acceptance_criteria: request.acceptance_criteria.clone(),
        client_request_id: Some(client_request_id),
        mutation_kind: Some("appendBlockMember".to_string()),
    };
    match open_remote_project_for_shortcut(state, remote_shortcut, None).await {
        Ok(context) => {
            remote_req.project_id = context.remote_project_id.clone();
            match RemoteOrchestratorClient::new()
                .with_expected_device_id(&remote_shortcut.device_id)
                .append_task_block_member(&context.base_url, remote_req.clone())
                .await
            {
                Ok(task) => {
                    upsert_remote_task_view(
                        state,
                        remote_shortcut,
                        &context.remote_project_id,
                        &context.remote_project_path,
                        task,
                    )
                    .await
                }
                Err(err) if is_remote_network_error(&err) => {
                    enqueue_pending_append(state, remote_shortcut, &remote_req).await
                }
                Err(err) => Err(err),
            }
        }
        Err(err) if is_remote_network_error(&err) => {
            enqueue_pending_append(state, remote_shortcut, &remote_req).await
        }
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端离线重排必须整组排队，不能只改本机 mirror 顺序。
///
/// Code Logic（这个函数做什么）:
///     在线 POST reorder；网络错误写 pending 并返回当前已知成员（可能为空）。
async fn reorder_remote_block_members(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: ReorderOrchestratorTaskBlockMembersRequest,
) -> Result<Vec<OrchestratorTaskViewDto>, AppError> {
    let client_request_id = request
        .client_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut remote_req = RemoteReorderTaskBlockMembersReq {
        project_id: request.project_id.clone(),
        block_id: request.block_id.clone(),
        ordered_task_ids: request.ordered_task_ids.clone(),
        client_request_id: Some(client_request_id),
        mutation_kind: Some("reorderBlockMembers".to_string()),
    };
    match open_remote_project_for_shortcut(state, remote_shortcut, None).await {
        Ok(context) => {
            remote_req.project_id = context.remote_project_id.clone();
            match RemoteOrchestratorClient::new()
                .with_expected_device_id(&remote_shortcut.device_id)
                .reorder_task_block_members(&context.base_url, remote_req.clone())
                .await
            {
                Ok(tasks) => {
                    let mut views = Vec::new();
                    for task in tasks {
                        views.push(
                            upsert_remote_task_view(
                                state,
                                remote_shortcut,
                                &context.remote_project_id,
                                &context.remote_project_path,
                                task,
                            )
                            .await?,
                        );
                    }
                    Ok(views)
                }
                Err(err) if is_remote_network_error(&err) => {
                    enqueue_pending_reorder(state, remote_shortcut, &remote_req).await?;
                    Ok(Vec::new())
                }
                Err(err) => Err(err),
            }
        }
        Err(err) if is_remote_network_error(&err) => {
            enqueue_pending_reorder(state, remote_shortcut, &remote_req).await?;
            Ok(Vec::new())
        }
        Err(err) => Err(err),
    }
}

/// Business Logic（为什么需要这个函数）:
///     离线建块要复用现有 task outbox 表，用 mutationKind 区分投递路径。
///
/// Code Logic（这个函数做什么）:
///     序列化 RemoteCreateOrchestratorTaskBlockReq 写入 pending outbox，返回合成 PendingRemote 视图。
async fn enqueue_pending_block_mutation(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: &RemoteCreateOrchestratorTaskBlockReq,
) -> Result<OrchestratorTaskBlockViewCreatedDto, AppError> {
    let request_json = serde_json::to_string(request)?;
    let item = enqueue_raw_outbox(state, remote_shortcut, &request_json).await?;
    let now = chrono::Utc::now().to_rfc3339();
    Ok(OrchestratorTaskBlockViewCreatedDto {
        block: OrchestratorTaskBlockDto {
            id: item.id.clone(),
            project_id: remote_shortcut.id.clone(),
            title: request.title.clone(),
            shared_worktree_id: None,
            shared_branch_name: None,
            created_at: now.clone(),
            updated_at: now,
        },
        tasks: vec![OrchestratorTaskViewDto::PendingRemote {
            item: item.to_dto(),
        }],
    })
}

/// Business Logic（为什么需要这个函数）:
///     离线追加复用同一 outbox，dispatcher 按 mutationKind 投递。
///
/// Code Logic（这个函数做什么）:
///     写入 pending 并返回 PendingRemote 视图。
async fn enqueue_pending_append(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: &RemoteAppendTaskBlockMemberReq,
) -> Result<OrchestratorTaskViewDto, AppError> {
    let request_json = serde_json::to_string(request)?;
    let item = enqueue_raw_outbox(state, remote_shortcut, &request_json).await?;
    Ok(OrchestratorTaskViewDto::PendingRemote {
        item: item.to_dto(),
    })
}

/// Business Logic（为什么需要这个函数）:
///     离线重排也必须进 outbox，恢复后按同一排列投递。
///
/// Code Logic（这个函数做什么）:
///     写入 pending outbox。
async fn enqueue_pending_reorder(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request: &RemoteReorderTaskBlockMembersReq,
) -> Result<(), AppError> {
    let request_json = serde_json::to_string(request)?;
    let _ = enqueue_raw_outbox(state, remote_shortcut, &request_json).await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     块 mutation 与单任务 create 共用 outbox 表，避免再开一套 dispatcher。
///
/// Code Logic（这个函数做什么）:
///     借用 create_pending_remote_task 的 insert，但 request_json 已是块 mutation。
async fn enqueue_raw_outbox(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    request_json: &str,
) -> Result<crate::orchestrator::outbox::OrchestratorRemoteOutboxRow, AppError> {
    state
        .orchestrator_repo
        .insert_remote_outbox_pending(
            &remote_shortcut.device_id,
            &remote_shortcut.device_name,
            &remote_shortcut.path,
            None,
            request_json,
        )
        .await
}

/// Business Logic（为什么需要这个函数）:
///     成员三字段必须 trim 后交给仓储，避免空白绕过 2–8 校验。
///
/// Code Logic（这个函数做什么）:
///     映射为 BlockMemberDraft。
fn drafts_from_members(
    members: &[CreateOrchestratorTaskBlockMember],
) -> Result<Vec<BlockMemberDraft>, AppError> {
    Ok(members
        .iter()
        .map(|member| BlockMemberDraft {
            title: member.title.trim().to_string(),
            goal: member.goal.trim().to_string(),
            acceptance_criteria: member.acceptance_criteria.trim().to_string(),
        })
        .collect())
}
