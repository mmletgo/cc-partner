//! agent_cli/remote.rs — 显式 `--device id:<deviceId>` 远端 transport。
//!
//! Business Logic（为什么需要这个模块）:
//!     remote 必须显式设备 ID；业务 API 无调用者身份鉴权；不得发送本机 control token；
//!     不得自动挑选 peer。
//!
//! Code Logic（这个模块做什么）:
//!     从本机 mDNS device 表解析 address/port；health/capability gate；
//!     复用 RemoteWorkbenchClient / RemoteOrchestratorClient；映射 error envelope。

use crate::agent_cli::client::map_code_to_cli_error;
use crate::agent_cli::output::CliError;
use crate::agent_cli::protocol::{AgentControlMutation, AgentControlQuery};
use crate::agent_cli::selectors::{
    resolve_exact_project, resolve_exact_worktree, ProjectCandidate, ProjectSelector,
    WorktreeCandidate, WorktreeSelector,
};
use crate::commands::workbench::device_base_url;
use crate::error::AppError;
use crate::net::protocol::{
    PeerProtocolInfo, CAPABILITY_ATTENTION_V1, CAPABILITY_ERRORS_ENVELOPE_V1,
    CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1, CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1,
    CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1, CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1,
    CAPABILITY_WORKBENCH_LAN_FLEET_V1, PROTOCOL_VERSION_V1,
};
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::state::AppState;
use crate::workbench::browser_verification::BrowserVerificationCommand;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_ids::parse_remote_entity_id;
use crate::workbench::remote_protocol::{
    RemoteCreateWorktreeReq, RemoteWorkbenchBrowserDiscoverReq,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 解析远端设备 base URL。
///
/// Business Logic（为什么需要这个函数）:
///     必须用当前 owner 的 mDNS 表；缺失 not_found；禁止 auto pick。
///
/// Code Logic（这个函数做什么）:
///     委托 `device_base_url`；离线/缺失映射 CliError。
pub fn resolve_remote_device_base_url(
    state: &AppState,
    device_id: &str,
) -> Result<String, CliError> {
    if device_id.trim().is_empty() {
        return Err(CliError::usage(
            "invalid_device",
            "device id must be non-empty",
        ));
    }
    // 重复 ID 在 HashMap 键空间不可能；缺失 → not found / offline
    match device_base_url(state, device_id) {
        Ok(url) => Ok(url),
        Err(_) => {
            let devices = state.devices.read().map_err(|_| {
                CliError::internal("device table lock poisoned")
            })?;
            if devices.contains_key(device_id) {
                Err(CliError::unavailable(
                    "peer_offline",
                    "remote device is offline",
                ))
            } else {
                Err(CliError::not_found(
                    "remote device not found in discovery table",
                ))
            }
        }
    }
}

/// 远端 health + capability 门禁。
///
/// Business Logic（为什么需要这个函数）:
///     旧 peer / 缺能力必须 exit 6；offline exit 5。
///
/// Code Logic（这个函数做什么）:
///     GET /api/health；检查 protocol_version 与 capability token。
pub async fn require_remote_capability(
    base_url: &str,
    capability: &str,
) -> Result<PeerProtocolInfo, CliError> {
    let client = reqwest::Client::new();
    let url = format!("{base_url}/api/health");
    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| {
            CliError::unavailable("peer_offline", "remote peer health check failed")
        })?;
    if !response.status().is_success() {
        return Err(CliError::unavailable(
            "peer_offline",
            "remote peer health returned non-success",
        ));
    }
    let info: PeerProtocolInfo = response.json().await.map_err(|_| {
        CliError::unsupported("remote peer protocol info unparseable or pre-v1")
    })?;
    if info.protocol_version < PROTOCOL_VERSION_V1 {
        return Err(CliError::unsupported(
            "remote peer protocol version is below v1",
        ));
    }
    if !info.supports(capability) {
        return Err(CliError::unsupported(format!(
            "remote peer missing capability {capability}"
        )));
    }
    Ok(info)
}

/// 仅校验 v1 + errors envelope。
async fn health_v1(base_url: &str) -> Result<(), CliError> {
    let _ = require_remote_capability(base_url, CAPABILITY_ERRORS_ENVELOPE_V1).await?;
    Ok(())
}

/// 远端执行 query（显式 device，经 AppState 解析 base URL）。
///
/// Business Logic（为什么需要这个函数）:
///     业务命令走 P2P 路由，不发 control token。
///
/// Code Logic（这个函数做什么）:
///     resolve device → `remote_query_with_base`。
pub async fn remote_query(
    state: &AppState,
    device_id: &str,
    op: AgentControlQuery,
) -> Result<Value, CliError> {
    let base_url = resolve_remote_device_base_url(state, device_id)?;
    remote_query_with_base(&base_url, device_id, op).await
}

/// 在已知 peer base URL 上执行 query。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 进程无 AppState 时，可先经本机 device 表解析 base URL 再调用。
///
/// Code Logic（这个函数做什么）:
///     capability gate → Remote*Client / raw HTTP；从不附带 control token。
pub async fn remote_query_with_base(
    base_url: &str,
    device_id: &str,
    op: AgentControlQuery,
) -> Result<Value, CliError> {
    match op {
        AgentControlQuery::ProjectList => {
            health_v1(&base_url).await?;
            let projects: Value =
                remote_get_json(&base_url, "/api/workbench/projects/list").await?;
            Ok(json!({ "items": projects }))
        }
        AgentControlQuery::ProjectInspect { selector } => {
            health_v1(&base_url).await?;
            let projects: Vec<Value> =
                remote_get_json(&base_url, "/api/workbench/projects/list").await?;
            let candidates = projects_to_candidates(&projects);
            let hit = resolve_exact_project(&selector, &candidates)?;
            projects
                .into_iter()
                .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(hit.id.as_str()))
                .ok_or_else(|| CliError::not_found("project not found"))
        }
        AgentControlQuery::WorktreeList { project } => {
            health_v1(&base_url).await?;
            let project_id = resolve_remote_project_id(&base_url, &project).await?;
            let items = RemoteWorkbenchClient::new()
                .list_worktrees(&base_url, &project_id)
                .await
                .map_err(app_error_to_cli)?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::SessionList { project, worktree } => {
            health_v1(&base_url).await?;
            let project_id = resolve_remote_project_id(&base_url, &project).await?;
            let mut items = RemoteWorkbenchClient::new()
                .list_sessions(&base_url, Some(project_id.as_str()))
                .await
                .map_err(app_error_to_cli)?;
            if let Some(wsel) = worktree {
                let wt_id = resolve_remote_worktree_id(&base_url, &project_id, &wsel).await?;
                items.retain(|s| s.worktree_id.as_deref() == Some(wt_id.as_str()));
            }
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::SessionRead {
            session_id,
            after_sequence,
        } => {
            health_v1(&base_url).await?;
            let inner = unwrap_session_id(device_id, &session_id)?;
            let mut replay = RemoteWorkbenchClient::new()
                .replay(&base_url, &inner)
                .await
                .map_err(app_error_to_cli)?;
            if let Some(after) = after_sequence {
                if replay.last_seq <= after {
                    replay.buffer.clear();
                    replay.truncated = false;
                }
            }
            serde_json::to_value(replay).map_err(|_| CliError::internal("serialize failed"))
        }
        AgentControlQuery::AgentList { project } => {
            require_remote_capability(&base_url, CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1).await?;
            let project_id = resolve_remote_project_id(&base_url, &project).await?;
            remote_post_json(
                &base_url,
                "/api/workbench/agent-runtime/snapshot",
                json!({ "projectId": project_id }),
            )
            .await
        }
        AgentControlQuery::AgentInspect { agent_session_id } => {
            require_remote_capability(&base_url, CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1).await?;
            let snap = remote_post_json(
                &base_url,
                "/api/workbench/agent-runtime/snapshot",
                json!({ "projectId": Value::Null }),
            )
            .await?;
            filter_agent_from_snapshot(&snap, &agent_session_id)
        }
        AgentControlQuery::AgentWait {
            agent_session_id,
            phase,
            timeout_ms,
        } => {
            require_remote_capability(&base_url, CAPABILITY_WORKBENCH_AGENT_RUNTIME_V1).await?;
            wait_agent_phase_remote(&base_url, &agent_session_id, &phase, timeout_ms).await
        }
        AgentControlQuery::TaskList { project } => {
            require_remote_capability(&base_url, CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1)
                .await?;
            let project_id = resolve_remote_project_id(&base_url, &project).await?;
            let items = RemoteOrchestratorClient::new()
                .list_tasks(&base_url, &project_id)
                .await
                .map_err(app_error_to_cli)?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::ExperimentInspect { experiment_id } => {
            require_remote_capability(&base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1).await?;
            remote_post_json(
                &base_url,
                "/api/orchestrator/experiments/get",
                json!({ "experimentId": experiment_id }),
            )
            .await
        }
        AgentControlQuery::AttentionList => {
            require_remote_capability(&base_url, CAPABILITY_ATTENTION_V1).await?;
            remote_get_json(&base_url, "/api/attention").await
        }
        AgentControlQuery::FleetSnapshot => {
            require_remote_capability(&base_url, CAPABILITY_WORKBENCH_LAN_FLEET_V1).await?;
            Err(CliError::unsupported(
                "fleet snapshot over recursive remote is unsupported",
            ))
        }
        AgentControlQuery::BrowserDiscover { project } => {
            health_v1(&base_url).await?;
            let project_id = resolve_remote_project_id(&base_url, &project).await?;
            let items = RemoteWorkbenchClient::new()
                .discover_browser_targets(
                    &base_url,
                    &RemoteWorkbenchBrowserDiscoverReq {
                        project_id,
                        worktree_id: None,
                    },
                )
                .await
                .map_err(app_error_to_cli)?;
            Ok(json!({ "items": items }))
        }
        AgentControlQuery::BrowserInspect { run_id } => {
            require_remote_capability(&base_url, CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1)
                .await?;
            let run = RemoteWorkbenchClient::new()
                .get_browser_verification(&base_url, &run_id)
                .await
                .map_err(app_error_to_cli)?;
            serde_json::to_value(run).map_err(|_| CliError::internal("serialize failed"))
        }
        // 仅本机 control plane 使用：解析 owner mDNS / 本机 create ledger。
        AgentControlQuery::DeviceResolve { .. }
        | AgentControlQuery::TaskByClientRequestId { .. } => Err(CliError::usage(
            "invalid_device",
            "DeviceResolve/TaskByClientRequestId are local control-only queries",
        )),
    }
}

/// 远端 mutation（经 AppState 解析 base URL）。
///
/// Business Logic（为什么需要这个函数）:
///     terminal send 等不得带 control token，连接丢失 → outcomeUnknown。
///
/// Code Logic（这个函数做什么）:
///     resolve device → `remote_mutate_with_base`。
pub async fn remote_mutate(
    state: &AppState,
    device_id: &str,
    op: AgentControlMutation,
) -> Result<Value, CliError> {
    let base_url = resolve_remote_device_base_url(state, device_id)?;
    remote_mutate_with_base(&base_url, device_id, op).await
}

/// 在已知 peer base URL 上执行 mutation。
///
/// Business Logic（为什么需要这个函数）:
///     CLI 直连 peer 时复用同一套 never-replay 语义。
///
/// Code Logic（这个函数做什么）:
///     health → 按 variant 调用 Remote*Client 一次。
pub async fn remote_mutate_with_base(
    base_url: &str,
    _device_id: &str,
    op: AgentControlMutation,
) -> Result<Value, CliError> {
    health_v1(base_url).await?;
    match op {
        AgentControlMutation::SessionSend { session_id, data } => {
            let inner = unwrap_session_id(_device_id, &session_id)?;
            RemoteWorkbenchClient::new()
                .write_input(base_url, &inner, &data)
                .await
                .map_err(|e| map_remote_mutation_error(e, true))?;
            Ok(json!({ "ok": true, "sessionId": session_id }))
        }
        AgentControlMutation::WorktreeCreate { project, payload } => {
            let project_id = resolve_remote_project_id(base_url, &project).await?;
            let branch_name = payload
                .get("branchName")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CliError::usage("invalid_input", "branchName required"))?
                .to_string();
            let base_branch = payload
                .get("baseBranch")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let item = RemoteWorkbenchClient::new()
                .create_worktree(
                    base_url,
                    RemoteCreateWorktreeReq {
                        project_id,
                        branch_name,
                        base_branch,
                    },
                )
                .await
                .map_err(|e| map_remote_mutation_error(e, true))?;
            serde_json::to_value(item).map_err(|_| CliError::internal("serialize failed"))
        }
        AgentControlMutation::TaskCreate {
            project,
            payload,
            client_request_id,
        } => {
            require_remote_capability(base_url, CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1)
                .await?;
            let project_id = resolve_remote_project_id(base_url, &project).await?;
            let req = build_remote_create_task_req(&project_id, &payload, client_request_id)?;
            let task = RemoteOrchestratorClient::new()
                .create_task(base_url, req)
                .await
                .map_err(|e| map_remote_mutation_error(e, false))?;
            serde_json::to_value(task).map_err(|_| CliError::internal("serialize failed"))
        }
        AgentControlMutation::TaskCancel {
            task_id,
            client_request_id: _,
        } => {
            let task = RemoteOrchestratorClient::new()
                .cancel_task(base_url, &task_id)
                .await
                .map_err(|e| map_remote_mutation_error(e, false))?;
            serde_json::to_value(task).map_err(|_| CliError::internal("serialize failed"))
        }
        AgentControlMutation::TaskRetry {
            task_id,
            client_request_id: _,
        } => {
            let task = RemoteOrchestratorClient::new()
                .retry_task(base_url, &task_id)
                .await
                .map_err(|e| map_remote_mutation_error(e, false))?;
            serde_json::to_value(task).map_err(|_| CliError::internal("serialize failed"))
        }
        AgentControlMutation::ExperimentCreate {
            project,
            payload,
            client_request_id,
        } => {
            require_remote_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1).await?;
            let project_id = resolve_remote_project_id(base_url, &project).await?;
            let mut body = payload;
            if let Some(obj) = body.as_object_mut() {
                obj.insert("projectId".into(), json!(project_id));
                if let Some(rid) = client_request_id {
                    obj.insert("clientRequestId".into(), json!(rid));
                }
            }
            remote_post_json(base_url, "/api/orchestrator/experiments/create", body).await
        }
        AgentControlMutation::ExperimentCancel { experiment_id } => {
            require_remote_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1).await?;
            remote_post_json(
                base_url,
                "/api/orchestrator/experiments/cancel",
                json!({ "experimentId": experiment_id }),
            )
            .await
        }
        AgentControlMutation::BrowserVerify { project: _, payload } => {
            require_remote_capability(base_url, CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1)
                .await?;
            let preview_id = payload
                .get("previewId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CliError::usage("invalid_input", "previewId required"))?
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
            let run = RemoteWorkbenchClient::new()
                .create_browser_verification(base_url, &preview_id, &request_id, &commands)
                .await
                .map_err(|e| map_remote_mutation_error(e, true))?;
            serde_json::to_value(run).map_err(|_| CliError::internal("serialize failed"))
        }
    }
}

fn build_remote_create_task_req(
    project_id: &str,
    payload: &Value,
    client_request_id: String,
) -> Result<RemoteCreateOrchestratorTaskReq, CliError> {
    let client_request_id = client_request_id.trim().to_string();
    if client_request_id.is_empty() {
        return Err(CliError::usage(
            "invalid_input",
            "clientRequestId required for task create",
        ));
    }
    Ok(RemoteCreateOrchestratorTaskReq {
        project_id: project_id.to_string(),
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
        priority: payload.get("priority").and_then(|v| v.as_i64()).unwrap_or(0),
        create_action: Default::default(),
        client_request_id: Some(client_request_id),
        source: payload
            .get("source")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        external_id: None,
        external_identifier: None,
        external_url: None,
        external_state: None,
        external_labels: None,
    })
}

async fn resolve_remote_project_id(
    base_url: &str,
    selector: &ProjectSelector,
) -> Result<String, CliError> {
    let projects: Vec<Value> = remote_get_json(base_url, "/api/workbench/projects/list").await?;
    let candidates = projects_to_candidates(&projects);
    Ok(resolve_exact_project(selector, &candidates)?.id.clone())
}

async fn resolve_remote_worktree_id(
    base_url: &str,
    project_id: &str,
    selector: &WorktreeSelector,
) -> Result<String, CliError> {
    let items = RemoteWorkbenchClient::new()
        .list_worktrees(base_url, project_id)
        .await
        .map_err(app_error_to_cli)?;
    let candidates: Vec<WorktreeCandidate> = items
        .iter()
        .map(|w| WorktreeCandidate {
            id: w.id.clone(),
            branch: w.branch.clone().unwrap_or_default(),
        })
        .collect();
    Ok(resolve_exact_worktree(selector, &candidates)?.id.clone())
}

fn projects_to_candidates(projects: &[Value]) -> Vec<ProjectCandidate> {
    projects
        .iter()
        .filter_map(|p| {
            Some(ProjectCandidate {
                id: p.get("id")?.as_str()?.to_string(),
                path: p
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn unwrap_session_id(device_id: &str, session_id: &str) -> Result<String, CliError> {
    if let Some(parsed) = parse_remote_entity_id(session_id) {
        if parsed.device_id != device_id {
            return Err(CliError::conflict(
                "session device id does not match --device",
            ));
        }
        return Ok(parsed.inner_id);
    }
    Ok(session_id.to_string())
}

fn filter_agent_from_snapshot(snap: &Value, agent_id: &str) -> Result<Value, CliError> {
    let sessions = snap
        .get("sessions")
        .or_else(|| snap.get("items"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut hit = sessions
        .into_iter()
        .find(|s| s.get("id").and_then(|v| v.as_str()) == Some(agent_id))
        .ok_or_else(|| CliError::not_found("agent session not found"))?;
    if let Some(obj) = hit.as_object_mut() {
        obj.remove("nativeSessionId");
        obj.remove("native_session_id");
        obj.remove("transcriptPath");
        obj.remove("transcript_path");
        obj.remove("launchEnvironment");
        obj.remove("launch_environment");
    }
    Ok(hit)
}

async fn wait_agent_phase_remote(
    base_url: &str,
    agent_id: &str,
    phase: &str,
    timeout_ms: u64,
) -> Result<Value, CliError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(1));
    loop {
        let snap = remote_post_json(
            base_url,
            "/api/workbench/agent-runtime/snapshot",
            json!({ "projectId": Value::Null }),
        )
        .await?;
        if let Ok(agent) = filter_agent_from_snapshot(&snap, agent_id) {
            let current = agent.get("phase").and_then(|v| v.as_str()).unwrap_or("");
            if phase_matches(current, phase) {
                return Ok(agent);
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(CliError::unavailable(
                "timeout",
                "agent phase wait timed out",
            ));
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

async fn remote_get_json<T: serde::de::DeserializeOwned>(
    base_url: &str,
    path: &str,
) -> Result<T, CliError> {
    let client = reqwest::Client::new();
    let url = format!("{base_url}{path}");
    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                CliError::unavailable("peer_offline", "remote connection failed")
            } else {
                CliError::outcome_unknown("remote transport failed")
            }
        })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| CliError::outcome_unknown("remote body unreadable"))?;
    if status.is_success() {
        serde_json::from_slice(&bytes).map_err(|_| CliError::internal("remote json parse failed"))
    } else {
        Err(map_remote_http_error(&bytes, status.as_u16()))
    }
}

async fn remote_post_json(base_url: &str, path: &str, body: Value) -> Result<Value, CliError> {
    let client = reqwest::Client::new();
    let url = format!("{base_url}{path}");
    let response = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(60))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                CliError::unavailable("peer_offline", "remote connection failed")
            } else {
                CliError::outcome_unknown("remote transport failed after dispatch")
            }
        })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| CliError::outcome_unknown("remote body unreadable"))?;
    if status.is_success() {
        serde_json::from_slice(&bytes).map_err(|_| CliError::internal("remote json parse failed"))
    } else {
        Err(map_remote_http_error(&bytes, status.as_u16()))
    }
}

fn map_remote_http_error(bytes: &[u8], status: u16) -> CliError {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Env {
        error: Option<EnvErr>,
        code: Option<String>,
        message: Option<String>,
        request_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EnvErr {
        code: Option<String>,
        message: Option<String>,
        #[serde(default)]
        retryable: bool,
        request_id: Option<String>,
    }
    if let Ok(env) = serde_json::from_slice::<Env>(bytes) {
        if let Some(err) = env.error {
            let code = err.code.unwrap_or_else(|| "internal".into());
            let msg = err.message.unwrap_or_else(|| "remote error".into());
            return map_code_to_cli_error(&code, &msg, err.retryable, false, err.request_id);
        }
        if let Some(code) = env.code {
            let msg = env.message.unwrap_or_else(|| "remote error".into());
            return map_code_to_cli_error(&code, &msg, false, false, env.request_id);
        }
    }
    match status {
        404 => CliError::not_found("remote resource not found"),
        409 => CliError::conflict("remote conflict"),
        400 => CliError::usage("validation", "remote validation failed"),
        503 => CliError::unavailable("unavailable", "remote unavailable"),
        _ => CliError::internal("remote request failed"),
    }
}

fn map_remote_mutation_error(err: AppError, never_replay: bool) -> CliError {
    let cli = app_error_to_cli(err);
    if never_replay && !cli.outcome_unknown_flag() {
        // health 已通过后的 mutation 路径：Unavailable/Timeout 视为 dispatch 后 transport 丢失。
        // 不得仅因中文 Display 文案缺关键字而落入 Internal 并跳过 outcomeUnknown。
        if cli.exit == crate::agent_cli::output::CliExitCode::Unavailable
            && cli.code() != "peer_offline"
            && cli.code() != "backend_offline"
        {
            return CliError::outcome_unknown(cli.message).with_request_id(cli.request_id);
        }
    }
    cli
}

/// AppError → CliError（按 `classify()` / `remote_meta()`，不解析本地化 message）。
///
/// Business Logic（为什么需要这个函数）:
///     Spec：remote error 从结构化 envelope/分类映射；中文 Display 不得驱动 exit/code。
///
/// Code Logic（这个函数做什么）:
///     优先 Remote 信封 code/request_id/retryable；否则 `classify()` → 稳定 CliError；
///     message 使用通用英文短句，不回显业务中文。
pub fn app_error_to_cli(err: AppError) -> CliError {
    use crate::error::AppErrorCategory;

    let request_id = err
        .remote_meta()
        .map(|m| m.request_id.clone())
        .filter(|s| !s.is_empty());
    let retryable_from_meta = err.remote_meta().map(|m| m.retryable);
    let code_from_meta = err.remote_meta().map(|m| m.code.clone());

    let category = err.classify();
    let (code, message, mut cli) = match category {
        AppErrorCategory::NotFound => (
            "not_found".to_string(),
            "resource not found",
            CliError::not_found("resource not found"),
        ),
        AppErrorCategory::Conflict => (
            "conflict".to_string(),
            "conflict",
            CliError::conflict("conflict"),
        ),
        AppErrorCategory::Validation => (
            "validation".to_string(),
            "validation failed",
            CliError::usage("validation", "validation failed"),
        ),
        AppErrorCategory::Unavailable => (
            "unavailable".to_string(),
            "backend or peer unavailable",
            CliError::unavailable("unavailable", "backend or peer unavailable"),
        ),
        AppErrorCategory::Timeout => (
            "timeout".to_string(),
            "operation timed out",
            CliError::unavailable("timeout", "operation timed out"),
        ),
        AppErrorCategory::Internal => (
            "internal".to_string(),
            "operation failed",
            CliError::internal("operation failed"),
        ),
    };
    let _ = message;
    if let Some(code) = code_from_meta {
        // Remote 信封 code 覆盖默认 token（如 not_found/conflict/unavailable）。
        cli = cli.with_code(code);
    } else if category == AppErrorCategory::Internal {
        cli = cli.with_code(code);
    }
    let retryable = retryable_from_meta.unwrap_or_else(|| {
        matches!(
            category,
            AppErrorCategory::Unavailable | AppErrorCategory::Timeout
        )
    });
    cli.with_retryable(retryable).with_request_id(request_id)
}

/// 模拟 peer：用于 remote 单测 hit count / no control token。
#[derive(Clone)]
pub struct MockPeer {
    hits: Arc<AtomicUsize>,
    mode: Arc<Mutex<MockPeerMode>>,
    last_headers: Arc<Mutex<HashMap<String, String>>>,
}

impl Default for MockPeer {
    /// Business Logic（为什么需要这个函数）:
    ///     默认模拟连接丢失，覆盖 never-replay 测试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 drop_after_apply()。
    fn default() -> Self {
        Self::drop_after_apply()
    }
}

#[derive(Clone)]
enum MockPeerMode {
    DropAfterApply,
    Ok,
}

impl MockPeer {
    /// Business Logic（为什么需要这个函数）:
    ///     remote terminal send 连接丢失不得重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     mode=DropAfterApply。
    pub fn drop_after_apply() -> Self {
        Self {
            hits: Arc::new(AtomicUsize::new(0)),
            mode: Arc::new(Mutex::new(MockPeerMode::DropAfterApply)),
            last_headers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     断言 hit count。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 AtomicUsize。
    pub fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     断言未发送 control token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 headers map。
    pub fn last_headers(&self) -> HashMap<String, String> {
        self.last_headers.lock().expect("headers").clone()
    }

    /// 模拟一次 terminal send。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单元测试不启动 HTTP peer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     记录 headers，按 mode 返回。
    pub fn send_terminal(
        &self,
        _session: &str,
        _data: &[u8],
        headers: HashMap<String, String>,
    ) -> Result<(), CliError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        *self.last_headers.lock().expect("headers") = headers;
        match *self.mode.lock().expect("mode") {
            MockPeerMode::DropAfterApply => Err(CliError::outcome_unknown(
                "connection lost after mutation dispatch",
            )),
            MockPeerMode::Ok => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_terminal_send_does_not_receive_control_token_or_retry() {
        let peer = MockPeer::drop_after_apply();
        let mut headers = HashMap::new();
        headers.insert("content-type".into(), "application/json".into());
        let error = peer
            .send_terminal("remote:d:s", b"ls\n", headers)
            .unwrap_err();
        assert_eq!(peer.hit_count(), 1);
        assert!(!peer
            .last_headers()
            .contains_key("x-cc-partner-control-token"));
        assert!(error.outcome_unknown_flag());
    }

    #[test]
    fn phase_match_accepts_snake_and_camel() {
        assert!(phase_matches("needs_input", "needsInput"));
        assert!(phase_matches("Idle", "idle"));
        assert!(!phase_matches("working", "idle"));
    }

    #[test]
    fn map_remote_error_preserves_request_id() {
        let bytes = br#"{"error":{"code":"not_found","message":"missing","requestId":"req-9"}}"#;
        let err = map_remote_http_error(bytes, 404);
        assert_eq!(err.code(), "not_found");
        assert_eq!(err.request_id.as_deref(), Some("req-9"));
    }

    #[test]
    fn app_error_not_found_maps_exit_three() {
        let err = app_error_to_cli(AppError::not_found("工作台会话不存在"));
        assert_eq!(err.exit, crate::agent_cli::output::CliExitCode::NotFound);
        assert!(!err.message.contains("工作台"));
    }

    #[test]
    fn app_error_unavailable_chinese_without_keywords_maps_unavailable() {
        // 真实 RemoteWorkbenchClient 失败文案不含 unavail/offline/不可用
        let err = app_error_to_cli(AppError::unavailable("远端 Workbench 请求失败: connection reset"));
        assert_eq!(err.exit, crate::agent_cli::output::CliExitCode::Unavailable);
        assert_eq!(err.code(), "unavailable");
        assert!(!err.message.contains("Workbench"));
    }

    #[test]
    fn never_replay_promotes_unavailable_transport_to_outcome_unknown() {
        let err = map_remote_mutation_error(
            AppError::unavailable("远端 Workbench 请求失败: broken pipe"),
            true,
        );
        assert!(err.outcome_unknown_flag());
        assert_eq!(err.exit, crate::agent_cli::output::CliExitCode::Unavailable);
        assert_eq!(err.code(), "outcome_unknown");
    }

    #[test]
    fn app_error_remote_meta_preserves_request_id() {
        let err = app_error_to_cli(AppError::remote(
            "peer says no",
            crate::error::RemoteErrorMeta {
                code: "not_found".into(),
                status: 404,
                retryable: false,
                request_id: "req-remote-1".into(),
                details: serde_json::json!({}),
            },
        ));
        assert_eq!(err.exit, crate::agent_cli::output::CliExitCode::NotFound);
        assert_eq!(err.code(), "not_found");
        assert_eq!(err.request_id.as_deref(), Some("req-remote-1"));
    }
}
