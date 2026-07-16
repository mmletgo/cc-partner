//! backend/control_agent.rs — loopback Agent control query/mutate 路由。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent CLI 经本机 control token 访问共享 AppState；query 无副作用，
//!     mutation 遵循领域状态机且不直连 SQLite 从 CLI 进程。
//!
//! Code Logic（这个模块做什么）:
//!     POST `/api/backend/control/agent/query|mutate`；loopback+token 鉴权；
//!     分发到既有 for_state helpers；响应 ≤1MiB。

use crate::agent_cli::protocol::{
    AgentControlMutation, AgentControlQuery, AgentControlRequest, AgentControlResponse,
};
use crate::agent_cli::selectors::{
    resolve_exact_project, resolve_exact_worktree, ProjectCandidate, WorktreeCandidate,
};
use crate::backend::control::{self, BackendControlFile};
use crate::backend::control_api::CONTROL_RESPONSE_BODY_LIMIT_BYTES;
use crate::commands::attention::list_attention_items_v2_for_state;
use crate::commands::orchestrator::{
    cancel_orchestrator_experiment_for_state, create_orchestrator_experiment_for_state,
    create_orchestrator_task_view_for_state, get_orchestrator_experiment_for_state,
    list_orchestrator_task_views_for_state, CreateOrchestratorTaskRequest,
};
use crate::commands::workbench::{
    create_workbench_worktree_for_state, discover_workbench_browser_targets_for_state,
    get_browser_verification_for_state, get_workbench_lan_fleet_for_state,
    list_workbench_sessions_for_state, list_workbench_worktrees_for_state,
    replay_workbench_session_for_state, start_browser_verification_for_state,
    write_workbench_session_input_for_state, StartBrowserVerificationReq,
};
use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::require_loopback_peer;
use crate::net::request_context::P2pRequestContext;
use crate::orchestrator::experiments::CreateExperimentRequest;
use crate::state::AppState;
use crate::workbench::agent_runtime::snapshot::{
    get_agent_runtime_snapshot_for_state, AgentSessionRuntimeDto,
};
use crate::workbench::browser_verification::BrowserVerificationCommand;
use axum::extract::{ConnectInfo, Extension, State};
use axum::Json;
use serde_json::{json, Value};
use std::net::SocketAddr;
use uuid::Uuid;

/// Agent query control handler。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 本机 query 必须经 loopback+token，且 session read 不得 spawn terminal。
///
/// Code Logic（这个函数做什么）:
///     鉴权 → dispatch_query → 响应上限检查。
pub async fn control_agent_query(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<AgentControlRequest<AgentControlQuery>>,
) -> P2pResult<Json<AgentControlResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let data = dispatch_query(&state, request.op)
        .await
        .map_err(|e| map_cli_or_app_to_p2p(e, &context))?;
    let body = AgentControlResponse {
        owner_instance_id: state.config_runtime.owner_instance_id().to_string(),
        data,
    };
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// Agent mutate control handler。
///
/// Business Logic（为什么需要这个函数）:
///     CLI mutation 必须走同一 owner 进程，保持 claim/delivery 状态机。
///
/// Code Logic（这个函数做什么）:
///     鉴权 → dispatch_mutate → 响应上限检查。
pub async fn control_agent_mutate(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<AgentControlRequest<AgentControlMutation>>,
) -> P2pResult<Json<AgentControlResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let data = dispatch_mutate(&state, request.op)
        .await
        .map_err(|e| map_cli_or_app_to_p2p(e, &context))?;
    let body = AgentControlResponse {
        owner_instance_id: state.config_runtime.owner_instance_id().to_string(),
        data,
    };
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// 执行闭集 query。
///
/// Business Logic（为什么需要这个函数）:
///     统一本地领域读取，CLI 不复制业务逻辑。
///
/// Code Logic（这个函数做什么）:
///     match AgentControlQuery → for_state helpers。
pub async fn dispatch_query(state: &AppState, op: AgentControlQuery) -> Result<Value, AppError> {
    state.runtime_role.require_owner()?;
    match op {
        AgentControlQuery::ProjectList => {
            let rows = state.workbench_project_repo.list().await?;
            let items: Vec<_> = rows.iter().map(|r| r.to_dto()).collect();
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::ProjectInspect { selector } => {
            let rows = state.workbench_project_repo.list().await?;
            let candidates: Vec<ProjectCandidate> = rows
                .iter()
                .map(|r| ProjectCandidate {
                    id: r.id.clone(),
                    path: r.path.clone(),
                })
                .collect();
            let hit = resolve_exact_project(&selector, &candidates)
                .map_err(cli_to_app_error)?;
            let row = rows
                .iter()
                .find(|r| r.id == hit.id)
                .ok_or_else(|| AppError::not_found("project not found"))?;
            Ok(serde_json::to_value(row.to_dto())?)
        }
        AgentControlQuery::WorktreeList { project } => {
            let project_id = resolve_project_id(state, &project).await?;
            let items = list_workbench_worktrees_for_state(state, project_id).await?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::SessionList { project, worktree } => {
            let project_id = resolve_project_id(state, &project).await?;
            let mut items =
                list_workbench_sessions_for_state(state, Some(project_id.clone())).await?;
            if let Some(wsel) = worktree {
                let wt_id = resolve_worktree_id(state, &project_id, &wsel).await?;
                items.retain(|s| s.worktree_id.as_deref() == Some(wt_id.as_str()));
            }
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::SessionRead {
            session_id,
            after_sequence,
        } => {
            // 只读 replay buffer，不 create/restore terminal
            let mut replay = replay_workbench_session_for_state(state, session_id).await?;
            if let Some(after) = after_sequence {
                if replay.last_seq <= after {
                    replay.buffer.clear();
                    replay.truncated = false;
                }
            }
            Ok(serde_json::to_value(replay)?)
        }
        AgentControlQuery::AgentList { project } => {
            let project_id = resolve_project_id(state, &project).await?;
            let snap =
                get_agent_runtime_snapshot_for_state(state, Some(project_id)).await?;
            Ok(serde_json::to_value(snap)?)
        }
        AgentControlQuery::AgentInspect { agent_session_id } => {
            let row = state
                .workbench_agent_session_repo
                .get(&agent_session_id)
                .await?
                .ok_or_else(|| AppError::not_found("agent session not found"))?;
            let dto = AgentSessionRuntimeDto::from_runtime(&row);
            // 确认无 native 字段
            let mut v = serde_json::to_value(dto)?;
            if let Some(obj) = v.as_object_mut() {
                obj.remove("nativeSessionId");
                obj.remove("transcriptPath");
                obj.remove("launchEnvironment");
            }
            Ok(v)
        }
        AgentControlQuery::AgentWait {
            agent_session_id,
            phase,
            timeout_ms,
        } => wait_agent_phase_local(state, &agent_session_id, &phase, timeout_ms).await,
        AgentControlQuery::TaskList { project } => {
            let project_id = resolve_project_id(state, &project).await?;
            let items =
                list_orchestrator_task_views_for_state(state, Some(project_id)).await?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::ExperimentInspect { experiment_id } => {
            let exp = get_orchestrator_experiment_for_state(state, &experiment_id).await?;
            Ok(serde_json::to_value(exp)?)
        }
        AgentControlQuery::AttentionList => {
            let items = list_attention_items_v2_for_state(state).await?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::FleetSnapshot => {
            let snap = get_workbench_lan_fleet_for_state(state).await?;
            Ok(serde_json::to_value(snap)?)
        }
        AgentControlQuery::BrowserDiscover { project } => {
            let project_id = resolve_project_id(state, &project).await?;
            let items =
                discover_workbench_browser_targets_for_state(state, project_id, None).await?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::BrowserInspect { run_id } => {
            let run = get_browser_verification_for_state(state, run_id).await?;
            Ok(serde_json::to_value(run)?)
        }
    }
}

/// 执行闭集 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     mutation 保持领域幂等/状态机；CLI 不盲重放。
///
/// Code Logic（这个函数做什么）:
///     match AgentControlMutation → for_state helpers。
pub async fn dispatch_mutate(
    state: &AppState,
    op: AgentControlMutation,
) -> Result<Value, AppError> {
    state.runtime_role.require_owner()?;
    match op {
        AgentControlMutation::WorktreeCreate { project, payload } => {
            let project_id = resolve_project_id(state, &project).await?;
            let branch_name = payload
                .get("branchName")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::validation("branchName required"))?
                .to_string();
            let base_branch = payload
                .get("baseBranch")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let item = create_workbench_worktree_for_state(
                state,
                project_id,
                branch_name,
                base_branch,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        AgentControlMutation::SessionSend { session_id, data } => {
            write_workbench_session_input_for_state(state, session_id, data).await
        }
        AgentControlMutation::TaskCreate {
            project,
            payload,
            client_request_id: _,
        } => {
            let project_id = resolve_project_id(state, &project).await?;
            let request = CreateOrchestratorTaskRequest {
                project_id,
                title: payload
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                goal: payload
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                acceptance_criteria: payload
                    .get("acceptanceCriteria")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                priority: payload.get("priority").and_then(|v| v.as_i64()),
                create_action: Default::default(),
                source: payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                external_id: None,
                external_identifier: None,
                external_url: None,
                external_state: None,
                external_labels: None,
            };
            let view = create_orchestrator_task_view_for_state(state, request).await?;
            Ok(serde_json::to_value(view)?)
        }
        AgentControlMutation::TaskCancel {
            task_id,
            client_request_id: _,
        } => {
            // 自然幂等：已取消则返回当前视图
            let task = state.orchestrator_repo.cancel_task(&task_id).await?;
            Ok(serde_json::to_value(crate::orchestrator::models::OrchestratorTaskDto::from(task))?)
        }
        AgentControlMutation::TaskRetry {
            task_id,
            client_request_id: _,
        } => {
            let updated = state
                .orchestrator_repo
                .transition_task_status(
                    &task_id,
                    crate::orchestrator::models::OrchestratorTaskStatus::Blocked,
                    crate::orchestrator::models::OrchestratorTaskStatus::Queued,
                    None,
                )
                .await?;
            Ok(serde_json::to_value(
                crate::orchestrator::models::OrchestratorTaskDto::from(updated),
            )?)
        }
        AgentControlMutation::ExperimentCreate {
            project,
            payload,
            client_request_id,
        } => {
            let project_id = resolve_project_id(state, &project).await?;
            let req = CreateExperimentRequest {
                client_request_id: client_request_id
                    .or_else(|| {
                        payload
                            .get("clientRequestId")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                project_id,
                title: payload
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                goal: payload
                    .get("goal")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                acceptance: payload
                    .get("acceptance")
                    .or_else(|| payload.get("acceptanceCriteria"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                max_parallel: payload
                    .get("maxParallel")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2) as u32,
                candidates: payload
                    .get("candidates")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default(),
            };
            let outcome = create_orchestrator_experiment_for_state(state, req).await?;
            Ok(serde_json::to_value(outcome.experiment)?)
        }
        AgentControlMutation::ExperimentCancel { experiment_id } => {
            let exp =
                cancel_orchestrator_experiment_for_state(state, &experiment_id).await?;
            Ok(serde_json::to_value(exp)?)
        }
        AgentControlMutation::BrowserVerify { project: _, payload } => {
            let preview_id = payload
                .get("previewId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::validation("previewId required"))?
                .to_string();
            let request_id = payload
                .get("requestId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let commands: Vec<BrowserVerificationCommand> = payload
                .get("commands")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let run = start_browser_verification_for_state(
                state,
                StartBrowserVerificationReq {
                    preview_id,
                    request_id,
                    commands,
                },
            )
            .await?;
            Ok(serde_json::to_value(run)?)
        }
    }
}

async fn resolve_project_id(
    state: &AppState,
    selector: &crate::agent_cli::selectors::ProjectSelector,
) -> Result<String, AppError> {
    let rows = state.workbench_project_repo.list().await?;
    let candidates: Vec<ProjectCandidate> = rows
        .iter()
        .map(|r| ProjectCandidate {
            id: r.id.clone(),
            path: r.path.clone(),
        })
        .collect();
    Ok(resolve_exact_project(selector, &candidates)
        .map_err(cli_to_app_error)?
        .id
        .clone())
}

async fn resolve_worktree_id(
    state: &AppState,
    project_id: &str,
    selector: &crate::agent_cli::selectors::WorktreeSelector,
) -> Result<String, AppError> {
    let items = list_workbench_worktrees_for_state(state, project_id.to_string()).await?;
    let candidates: Vec<WorktreeCandidate> = items
        .iter()
        .map(|w| WorktreeCandidate {
            id: w.id.clone(),
            branch: w.branch.clone().unwrap_or_default(),
        })
        .collect();
    Ok(resolve_exact_worktree(selector, &candidates)
        .map_err(cli_to_app_error)?
        .id
        .clone())
}

async fn wait_agent_phase_local(
    state: &AppState,
    agent_id: &str,
    phase: &str,
    timeout_ms: u64,
) -> Result<Value, AppError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(1));
    loop {
        if let Some(row) = state.workbench_agent_session_repo.get(agent_id).await? {
            let dto = AgentSessionRuntimeDto::from_runtime(&row);
            let current = format!("{:?}", dto.phase);
            let current_str = dto.phase.as_str();
            if phase_matches(current_str, phase) || phase_matches(&current, phase) {
                return Ok(serde_json::to_value(dto)?);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(AppError::timeout("agent phase wait timed out"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn phase_matches(current: &str, want: &str) -> bool {
    let a = current
        .to_ascii_lowercase()
        .replace('_', "")
        .replace('-', "");
    let b = want.to_ascii_lowercase().replace('_', "").replace('-', "");
    a == b
}

fn authorize_control_request(
    peer: SocketAddr,
    context: &P2pRequestContext,
    request_token: &str,
) -> Result<(), P2pError> {
    require_loopback_peer(peer.ip(), context)?;
    let control = control::read_control_file()
        .map_err(|_| P2pError::from_code("控制文件不可读", P2pErrorCode::Unauthorized, context))?;
    if !control_token_matches(request_token, control.as_ref()) {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            context,
        ));
    }
    Ok(())
}

fn control_token_matches(request_token: &str, control: Option<&BackendControlFile>) -> bool {
    let Some(control) = control else {
        return false;
    };
    !request_token.is_empty() && request_token == control.control_token
}

fn ensure_response_within_limit<T: serde::Serialize>(
    body: &T,
    context: &P2pRequestContext,
) -> Result<(), P2pError> {
    let bytes = serde_json::to_vec(body).map_err(|_| {
        P2pError::from_code("响应序列化失败", P2pErrorCode::Internal, context)
    })?;
    if bytes.len() > CONTROL_RESPONSE_BODY_LIMIT_BYTES {
        return Err(P2pError::from_code(
            "control response exceeds 1MiB",
            P2pErrorCode::PayloadTooLarge,
            context,
        ));
    }
    Ok(())
}

fn cli_to_app_error(err: crate::agent_cli::output::CliError) -> AppError {
    match err.exit {
        crate::agent_cli::output::CliExitCode::NotFound => AppError::not_found(err.message),
        crate::agent_cli::output::CliExitCode::Conflict => AppError::conflict(err.message),
        crate::agent_cli::output::CliExitCode::Usage => AppError::validation(err.message),
        crate::agent_cli::output::CliExitCode::Unavailable => {
            AppError::unavailable(err.message)
        }
        crate::agent_cli::output::CliExitCode::Unsupported => {
            AppError::validation(err.message)
        }
        _ => AppError::generic(err.message),
    }
}

fn map_cli_or_app_to_p2p(err: AppError, context: &P2pRequestContext) -> P2pError {
    P2pError::from_app_error(err, context, "control.agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_cli::protocol::MutationReplayPolicy;

    #[test]
    fn mutation_policies_are_total() {
        let cases = [
            AgentControlMutation::SessionSend {
                session_id: "s".into(),
                data: "x".into(),
            },
            AgentControlMutation::WorktreeCreate {
                project: crate::agent_cli::selectors::ProjectSelector::Id("p".into()),
                payload: json!({}),
            },
            AgentControlMutation::TaskCancel {
                task_id: "t".into(),
                client_request_id: "r".into(),
            },
            AgentControlMutation::ExperimentCancel {
                experiment_id: "e".into(),
            },
        ];
        for m in cases {
            let _ = m.replay_policy();
        }
        assert_eq!(
            AgentControlMutation::SessionSend {
                session_id: "s".into(),
                data: "x".into(),
            }
            .replay_policy(),
            MutationReplayPolicy::NeverReplay
        );
    }

    #[test]
    fn phase_match_helper() {
        assert!(phase_matches("needs_input", "needsInput"));
        assert!(phase_matches("idle", "Idle"));
    }
}
