//! backend/control_workbench.rs — loopback control 面的 Workbench 操作分发。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 不得自建第二份 Workbench runtime；全部 projects/files/Git/browser/session
//!     操作经 control client 代理到 sidecar owner，由 owner 独占 RemoteWorkbenchClient/bridge。
//!
//! Code Logic（这个模块做什么）:
//!     提供 metadata/data 两条 control 路由 handler：loopback+token 鉴权后按 `op` 分发到
//!     既有 owner helper；metadata 响应 ≤1 MiB，data 路由不强制响应体上限。

use crate::backend::control::{self, BackendControlFile};
use crate::backend::control_api::CONTROL_RESPONSE_BODY_LIMIT_BYTES;
use crate::commands::workbench;
use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::require_loopback_peer;
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_protocol::{
    RemoteCreatePathReq, RemoteDeletePathReq, RemotePreviewHtmlAssetReq, RemotePreviewSqliteReq,
    RemoteRenamePathReq,
};
use crate::workbench::sessions::kill_persisted_backend;
use axum::extract::{ConnectInfo, Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;

/// Workbench control 请求（token + op + payload）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 用统一信封携带鉴权令牌与操作名，payload 按 op 解释。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase：controlToken / op / payload（缺省为空对象）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkbenchRequest {
    pub control_token: String,
    pub op: String,
    #[serde(default)]
    pub payload: Value,
}

/// Workbench control 响应（带 owner 身份）。
///
/// Business Logic（为什么需要这个结构）:
///     调用方需确认当前 ownerInstanceId，并把 result 作为业务返回值。
///
/// Code Logic（这个结构做什么）:
///     序列化 camelCase：ownerInstanceId + result。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlWorkbenchResponse {
    pub owner_instance_id: String,
    pub result: Value,
}

/// metadata 路由：强制响应 ≤1 MiB。
///
/// Business Logic（为什么需要这个函数）:
///     列表/元数据 op 的响应应受 1 MiB 保护，避免意外膨胀。
///
/// Code Logic（这个函数做什么）:
///     鉴权后分发，并在返回前校验序列化体积。
pub async fn control_workbench(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlWorkbenchRequest>,
) -> P2pResult<Json<ControlWorkbenchResponse>> {
    handle_control_workbench(peer, &context, &state, request, true).await
}

/// data 路由：允许大响应（文件 open/save/preview 等）。
///
/// Business Logic（为什么需要这个函数）:
///     打开文件或预览内容可能超过 1 MiB，不能复用 metadata 响应上限。
///
/// Code Logic（这个函数做什么）:
///     与 metadata 共用分发逻辑，但不强制 1 MiB 响应限制。
pub async fn control_workbench_data(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlWorkbenchRequest>,
) -> P2pResult<Json<ControlWorkbenchResponse>> {
    handle_control_workbench(peer, &context, &state, request, false).await
}

/// 共享 handler：鉴权 → owner 分发 → 可选响应上限。
///
/// Business Logic（为什么需要这个函数）:
///     metadata/data 两条路由只在 body/response 上限策略上不同，业务分发必须一致。
///
/// Code Logic（这个函数做什么）:
///     loopback+token → dispatch_workbench_op → 组装 ownerInstanceId + result。
async fn handle_control_workbench(
    peer: SocketAddr,
    context: &P2pRequestContext,
    state: &AppState,
    request: ControlWorkbenchRequest,
    enforce_response_limit: bool,
) -> P2pResult<Json<ControlWorkbenchResponse>> {
    authorize_control_request(peer, context, &request.control_token)?;
    let result = dispatch_workbench_op(state, &request.op, request.payload)
        .await
        .map_err(|e| P2pError::from_app_error(e, context, "control.workbench"))?;
    let owner_instance_id = state.config_runtime.owner_instance_id().to_string();
    let body = ControlWorkbenchResponse {
        owner_instance_id,
        result,
    };
    if enforce_response_limit {
        ensure_response_within_limit(&body, context)?;
    }
    Ok(Json(body))
}

/// 按 op 字符串分发到 Workbench owner helper。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 作为 HeadlessOwner 独占执行 local/remote Workbench 逻辑。
///
/// Code Logic（这个函数做什么）:
///     先 `runtime_role.require_owner()`，再 match op 调 for_state / owner helper。
async fn dispatch_workbench_op(
    state: &AppState,
    op: &str,
    payload: Value,
) -> Result<Value, AppError> {
    state.runtime_role.require_owner()?;

    match op {
        // ---- projects ----
        "projects.list" => {
            let rows = state.workbench_project_repo.list().await?;
            let items: Vec<_> = rows.iter().map(WorkbenchProjectRow::to_dto).collect();
            Ok(serde_json::to_value(items)?)
        }
        "projects.add" => {
            let path = required_string(&payload, "path")?;
            let item = workbench::add_local_workbench_project_from_path(state, path).await?;
            Ok(serde_json::to_value(item)?)
        }
        "projects.remove" => {
            let project_id = required_string(&payload, "projectId")?;
            let _ = workbench::get_project(state, &project_id).await?;
            let session_rows = state.workbench_session_repo.list(Some(&project_id)).await?;
            for row in session_rows {
                let _ = state.workbench_sessions.close(&row.id);
                kill_persisted_backend(&row);
            }
            state
                .workbench_session_repo
                .delete_by_project(&project_id)
                .await?;
            state
                .workbench_worktree_repo
                .delete_by_project(&project_id)
                .await?;
            state.workbench_project_repo.delete(&project_id).await?;
            Ok(serde_json::json!({ "ok": true, "projectId": project_id }))
        }
        "projects.touch" => {
            let project_id = required_string(&payload, "projectId")?;
            let mut row = workbench::get_project(state, &project_id).await?;
            let now = workbench::now_iso();
            row.last_opened_at = now.clone();
            row.updated_at = now;
            state.workbench_project_repo.upsert(&row).await?;
            Ok(serde_json::to_value(row.to_dto())?)
        }
        "projects.remote_roots" => {
            let device_id = required_string(&payload, "deviceId")?;
            let base_url = workbench::device_base_url(state, &device_id)?;
            let items = RemoteWorkbenchClient::new().roots(&base_url).await?;
            Ok(serde_json::to_value(items)?)
        }
        "projects.remote_list_dir" => {
            let device_id = required_string(&payload, "deviceId")?;
            let path = required_string(&payload, "path")?;
            let base_url = workbench::device_base_url(state, &device_id)?;
            let items = RemoteWorkbenchClient::new()
                .list_dir(&base_url, &path)
                .await?;
            Ok(serde_json::to_value(items)?)
        }
        "projects.remote_path_info" => {
            let device_id = required_string(&payload, "deviceId")?;
            let path = required_string(&payload, "path")?;
            let base_url = workbench::device_base_url(state, &device_id)?;
            let info = RemoteWorkbenchClient::new()
                .path_info(&base_url, &path)
                .await?;
            Ok(serde_json::to_value(info)?)
        }
        "projects.remote_open" => {
            let device_id = required_string(&payload, "deviceId")?;
            let path = required_string(&payload, "path")?;
            let item = open_remote_project(state, device_id, path).await?;
            Ok(serde_json::to_value(item)?)
        }

        // ---- worktrees / git ----
        "worktrees.list" => {
            let project_id = required_string(&payload, "projectId")?;
            let items = workbench::list_workbench_worktrees_for_state(state, project_id).await?;
            Ok(serde_json::to_value(items)?)
        }
        "worktrees.create" => {
            let project_id = required_string(&payload, "projectId")?;
            let branch_name = required_string(&payload, "branchName")?;
            let base_branch = optional_string(&payload, "baseBranch");
            let item = workbench::create_workbench_worktree_for_state(
                state,
                project_id,
                branch_name,
                base_branch,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "worktrees.commit" => {
            let worktree_id = required_string(&payload, "worktreeId")?;
            let message = optional_string(&payload, "message");
            let client_operation_id = required_string(&payload, "clientOperationId")?;
            let item = workbench::commit_workbench_worktree_for_state(
                state,
                worktree_id,
                message,
                client_operation_id,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "worktrees.push" => {
            let worktree_id = required_string(&payload, "worktreeId")?;
            let client_operation_id = required_string(&payload, "clientOperationId")?;
            let item = workbench::push_workbench_worktree_for_state(
                state,
                worktree_id,
                client_operation_id,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "worktrees.merge" => {
            let worktree_id = required_string(&payload, "worktreeId")?;
            let client_operation_id = required_string(&payload, "clientOperationId")?;
            let item = workbench::merge_workbench_worktree_for_state(
                state,
                worktree_id,
                client_operation_id,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "worktrees.remove" => {
            let worktree_id = required_string(&payload, "worktreeId")?;
            let force = optional_bool(&payload, "force");
            let client_operation_id = required_string(&payload, "clientOperationId")?;
            let value = workbench::remove_workbench_worktree_for_state(
                state,
                worktree_id,
                force,
                client_operation_id,
            )
            .await?;
            Ok(serde_json::to_value(value)?)
        }
        "worktrees.mutation_operation" => {
            let client_operation_id = required_string(&payload, "clientOperationId")?;
            let item =
                workbench::get_workbench_mutation_operation_for_state(state, client_operation_id)
                    .await?;
            Ok(serde_json::to_value(item)?)
        }
        "git.commits" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let limit = payload
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let items = workbench::list_workbench_git_commits_for_state(
                state,
                project_id,
                worktree_id,
                limit,
            )
            .await?;
            Ok(serde_json::to_value(items)?)
        }

        // ---- files ----
        "files.list_dir" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = optional_string(&payload, "path");
            let items =
                workbench::list_workbench_dir_for_state(state, project_id, worktree_id, path)
                    .await?;
            Ok(serde_json::to_value(items)?)
        }
        "files.info" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let info =
                workbench::get_workbench_path_info_for_state(state, project_id, worktree_id, path)
                    .await?;
            Ok(serde_json::to_value(info)?)
        }
        "files.open" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let item =
                workbench::open_workbench_file_for_state(state, project_id, worktree_id, path)
                    .await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.save_text" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let content = required_string(&payload, "content")?;
            let base_hash = required_string(&payload, "baseHash")?;
            let item = workbench::save_workbench_text_file_for_state(
                state,
                project_id,
                worktree_id,
                path,
                content,
                base_hash,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.create_file" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let parent_path = required_string(&payload, "parentPath")?;
            let name = required_string(&payload, "name")?;
            let item =
                create_file_for_state(state, project_id, worktree_id, parent_path, name).await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.create_dir" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let parent_path = required_string(&payload, "parentPath")?;
            let name = required_string(&payload, "name")?;
            let item =
                create_dir_for_state(state, project_id, worktree_id, parent_path, name).await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.rename" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let new_name = required_string(&payload, "newName")?;
            let item =
                rename_path_for_state(state, project_id, worktree_id, path, new_name).await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.delete" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let item = delete_path_for_state(state, project_id, worktree_id, path).await?;
            Ok(item)
        }
        "files.preview_sqlite" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let path = required_string(&payload, "path")?;
            let table = optional_string(&payload, "table");
            let limit_rows = payload.get("limitRows").and_then(|v| v.as_i64());
            let item =
                preview_sqlite_for_state(state, project_id, worktree_id, path, table, limit_rows)
                    .await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.preview_html_asset" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let document_path = required_string(&payload, "documentPath")
                .or_else(|_| required_string(&payload, "path"))?;
            let asset_path = required_string(&payload, "assetPath")?;
            let item = preview_html_asset_for_state(
                state,
                project_id,
                worktree_id,
                document_path,
                asset_path,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "files.format" => {
            let content = required_string(&payload, "content")?;
            let kind = required_string(&payload, "kind")
                .or_else(|_| required_string(&payload, "fileType"))
                .or_else(|_| required_string(&payload, "detectedType"))?;
            let formatted =
                crate::workbench::file_content::format_structured_content(&kind, &content)?;
            Ok(serde_json::json!({ "formatted": formatted }))
        }

        // ---- browser ----
        "browser.discover" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let items = workbench::discover_workbench_browser_targets_for_state(
                state,
                project_id,
                worktree_id,
            )
            .await?;
            Ok(serde_json::to_value(items)?)
        }
        "browser.create_preview" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let target_url = required_string(&payload, "targetUrl")?;
            let item = workbench::create_workbench_browser_preview_for_state(
                state,
                project_id,
                worktree_id,
                target_url,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }

        // ---- workspace layout / safe restore ----
        "layout.get" => {
            let slot_key = optional_string(&payload, "slotKey")
                .unwrap_or_else(|| crate::workbench::workspace_layout::desktop_auto_slot_key().to_string());
            let item = workbench::get_workspace_layout_for_state(state, &slot_key).await?;
            Ok(serde_json::to_value(item)?)
        }
        "layout.save" => {
            let draft: crate::workbench::workspace_layout::WorkspaceLayoutDraft =
                serde_json::from_value(
                    payload
                        .get("draft")
                        .cloned()
                        .ok_or_else(|| AppError::validation("draft required".to_string()))?,
                )?;
            let expected_revision = payload
                .get("expectedRevision")
                .and_then(|v| v.as_u64());
            let item =
                workbench::save_workspace_layout_for_state(state, draft, expected_revision).await?;
            Ok(serde_json::to_value(item)?)
        }
        "layout.listNamed" => {
            let items = workbench::list_named_workspace_layouts_for_state(state).await?;
            Ok(serde_json::to_value(items)?)
        }
        "layout.deleteNamed" => {
            let id = required_string(&payload, "id")?;
            workbench::delete_named_workspace_layout_for_state(state, &id).await?;
            Ok(serde_json::json!({ "ok": true }))
        }
        "layout.preflight" => {
            let slot_key = optional_string(&payload, "slotKey");
            let layout_id = optional_string(&payload, "layoutId");
            let plan =
                workbench::preflight_workspace_restore_for_state(state, slot_key, layout_id).await?;
            Ok(serde_json::to_value(plan)?)
        }
        "layout.apply" => {
            let plan: crate::workbench::workspace_restore::WorkspaceRestorePlan =
                serde_json::from_value(
                    payload
                        .get("plan")
                        .cloned()
                        .ok_or_else(|| AppError::validation("plan required".to_string()))?,
                )?;
            let result = workbench::apply_workspace_restore_for_state(state, plan).await?;
            Ok(serde_json::to_value(result)?)
        }

        "browser.verification.start" => {
            let req: crate::commands::workbench::StartBrowserVerificationReq =
                serde_json::from_value(payload.clone()).map_err(|e| {
                    crate::error::AppError::validation(format!(
                        "invalid browser.verification.start: {e}"
                    ))
                })?;
            let run =
                crate::commands::workbench::start_browser_verification_for_state(state, req).await?;
            Ok(serde_json::to_value(run)?)
        }
        "browser.verification.get" => {
            let run_id = required_string(&payload, "runId")?;
            let run =
                crate::commands::workbench::get_browser_verification_for_state(state, run_id)
                    .await?;
            Ok(serde_json::to_value(run)?)
        }
        "browser.verification.cancel" => {
            let run_id = required_string(&payload, "runId")?;
            let run =
                crate::commands::workbench::cancel_browser_verification_for_state(state, run_id)
                    .await?;
            Ok(serde_json::to_value(run)?)
        }
        "browser.verification.artifact" => {
            let run_id = required_string(&payload, "runId")?;
            let artifact_id = required_string(&payload, "artifactId")?;
            let dto = crate::commands::workbench::get_browser_verification_artifact_for_state(
                state,
                run_id,
                artifact_id,
            )
            .await?;
            Ok(serde_json::to_value(dto)?)
        }
        // ---- agent runtime ----
        "agent_runtime.snapshot" => {
            let project_id = optional_string(&payload, "projectId");
            let snap = crate::workbench::agent_runtime::get_agent_runtime_snapshot_for_state(
                state, project_id,
            )
            .await?;
            Ok(serde_json::to_value(snap)?)
        }
        // ---- lan fleet ----
        "lan_fleet.snapshot" => {
            let snap =
                crate::commands::workbench::get_workbench_lan_fleet_for_state(state).await?;
            Ok(serde_json::to_value(snap)?)
        }
        // ---- agent metadata ledger ----
        "agent_ledger.list" => {
            let req: crate::commands::workbench::ListAgentLedgerReq =
                serde_json::from_value(payload.clone()).unwrap_or_default();
            let page = crate::commands::workbench::list_agent_ledger_for_state(state, req).await?;
            Ok(serde_json::to_value(page)?)
        }
        "agent_ledger.summarize" => {
            let window = required_string(&payload, "window")?;
            let project_id = optional_string(&payload, "projectId");
            let req = crate::commands::workbench::SummarizeAgentLedgerReq {
                window,
                project_id,
            };
            let summary =
                crate::commands::workbench::summarize_agent_ledger_for_state(state, req).await?;
            Ok(serde_json::to_value(summary)?)
        }
        "agent_ledger.clear" => {
            let deleted = crate::commands::workbench::clear_agent_ledger_for_state(state).await?;
            Ok(serde_json::to_value(deleted)?)
        }

        // ---- sessions ----
        "sessions.list" => {
            let project_id = optional_string(&payload, "projectId");
            let items = workbench::list_workbench_sessions_for_state(state, project_id).await?;
            Ok(serde_json::to_value(items)?)
        }
        "sessions.create" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let initial_cols = payload
                .get("initialCols")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16);
            let initial_rows = payload
                .get("initialRows")
                .and_then(|v| v.as_u64())
                .map(|v| v as u16);
            let item = workbench::create_workbench_session_for_state(
                state,
                project_id,
                worktree_id,
                initial_cols,
                initial_rows,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "sessions.replay" => {
            let session_id = required_string(&payload, "sessionId")?;
            let item = workbench::replay_workbench_session_for_state(state, session_id).await?;
            Ok(serde_json::to_value(item)?)
        }
        "sessions.write" => {
            let session_id = required_string(&payload, "sessionId")?;
            let data = required_string(&payload, "data")?;
            workbench::write_workbench_session_input_for_state(state, session_id, data).await
        }
        "sessions.resize" => {
            let session_id = required_string(&payload, "sessionId")?;
            let cols = payload
                .get("cols")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AppError::validation("cols 必填"))? as u16;
            let rows = payload
                .get("rows")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| AppError::validation("rows 必填"))? as u16;
            workbench::resize_workbench_session_for_state(state, session_id, cols, rows).await
        }
        "sessions.focus" => {
            let session_id = required_string(&payload, "sessionId")?;
            workbench::focus_workbench_session_for_state(state, session_id).await
        }
        "sessions.focused" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let item =
                workbench::get_focused_workbench_session_for_state(state, project_id, worktree_id)
                    .await?;
            Ok(serde_json::to_value(item)?)
        }
        "sessions.split" => {
            let session_id = required_string(&payload, "sessionId")?;
            let direction = required_string(&payload, "direction")?;
            workbench::split_workbench_pane_for_state(state, session_id, direction).await
        }
        "sessions.switch" => {
            let session_id = required_string(&payload, "sessionId")?;
            workbench::switch_workbench_pane_for_state(state, session_id).await
        }
        "sessions.zoom" => {
            let session_id = required_string(&payload, "sessionId")?;
            workbench::zoom_workbench_pane_for_state(state, session_id).await
        }
        "sessions.close_pane" => {
            let session_id = required_string(&payload, "sessionId")?;
            workbench::close_workbench_pane_for_state(state, session_id).await
        }
        "sessions.close" => {
            let session_id = required_string(&payload, "sessionId")?;
            workbench::close_workbench_session_for_state(state, session_id).await
        }
        "sessions.rename" => {
            let session_id = required_string(&payload, "sessionId")?;
            let name = required_string(&payload, "name")?;
            let item = rename_session_for_state(state, session_id, name).await?;
            Ok(serde_json::to_value(item)?)
        }

        // ---- claude sessions ----
        "claude.search" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let query = optional_string(&payload, "query").unwrap_or_default();
            let items = workbench::search_claude_sessions_for_state(
                state,
                &project_id,
                worktree_id.as_deref(),
                &query,
            )
            .await?;
            Ok(serde_json::to_value(items)?)
        }
        "claude.preview" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let session_id = required_string(&payload, "sessionId")?;
            let item = workbench::get_claude_session_preview_for_state(
                state,
                &project_id,
                worktree_id.as_deref(),
                &session_id,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }
        "claude.resume" => {
            let project_id = required_string(&payload, "projectId")?;
            let worktree_id = optional_string(&payload, "worktreeId");
            let session_id = required_string(&payload, "sessionId")?;
            let item = workbench::resume_claude_session_for_state(
                state,
                &project_id,
                worktree_id.as_deref(),
                &session_id,
            )
            .await?;
            Ok(serde_json::to_value(item)?)
        }

        other => Err(AppError::validation(format!(
            "未知 workbench control op: {other}"
        ))),
    }
}

// ---- owner-side helpers for ops lacking dedicated for_state ----

/// 打开远端项目并写入本机 shortcut。
///
/// Business Logic（为什么需要这个函数）:
///     control 面需要与 Tauri `open_workbench_remote_project` 等价的 owner 逻辑。
///
/// Code Logic（这个函数做什么）:
///     解析 device → open_project → `build_remote_project_shortcut_row` → upsert。
async fn open_remote_project(
    state: &AppState,
    device_id: String,
    path: String,
) -> Result<crate::workbench::models::WorkbenchProjectDto, AppError> {
    let base_url = workbench::device_base_url(state, &device_id)?;
    let current_device_name = workbench::device_name_from_state(state, &device_id);
    let remote = RemoteWorkbenchClient::new()
        .open_project(&base_url, &path)
        .await?;
    let remote_id = crate::workbench::remote_ids::remote_project_id(&device_id, &remote.path);
    let existing = state.workbench_project_repo.get(&remote_id).await?;
    let now = workbench::now_iso();
    let row = workbench::build_remote_project_shortcut_row(
        &device_id,
        current_device_name.as_deref(),
        &remote,
        existing.as_ref(),
        &now,
    );
    state.workbench_project_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 在项目内创建文件（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     create_file 尚无独立 for_state helper，control 面需复用 Tauri 命令同等分支。
///
/// Code Logic（这个函数做什么）:
///     remote → RemoteCreatePathReq(parent_path,name)；local → local_create_workbench_file。
async fn create_file_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<crate::workbench::models::WorkbenchPathInfo, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .create_file(
                &context.base_url,
                RemoteCreatePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    parent_path,
                    name,
                },
            )
            .await;
    }
    workbench::local_create_workbench_file(state, project_id, worktree_id, parent_path, name).await
}

/// 在项目内创建目录（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     create_dir 与 create_file 同样缺少 for_state，需在 control 面实现 owner 分支。
///
/// Code Logic（这个函数做什么）:
///     remote → RemoteCreatePathReq；local → local_create_workbench_dir。
async fn create_dir_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<crate::workbench::models::WorkbenchPathInfo, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .create_dir(
                &context.base_url,
                RemoteCreatePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    parent_path,
                    name,
                },
            )
            .await;
    }
    workbench::local_create_workbench_dir(state, project_id, worktree_id, parent_path, name).await
}

/// 重命名项目内路径（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     rename 使用 path + newName（非 from/to），需与 Tauri 命令一致。
///
/// Code Logic（这个函数做什么）:
///     remote → RemoteRenamePathReq；local → local_rename_workbench_path。
async fn rename_path_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    new_name: String,
) -> Result<crate::workbench::models::WorkbenchPathInfo, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .rename_path(
                &context.base_url,
                RemoteRenamePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                    new_name,
                },
            )
            .await;
    }
    workbench::local_rename_workbench_path(state, project_id, worktree_id, path, new_name).await
}

/// 删除项目内路径（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     delete 尚无 for_state helper，control 面需复用 remote/local 分支。
///
/// Code Logic（这个函数做什么）:
///     remote → RemoteDeletePathReq；local → local_delete_workbench_path。
async fn delete_path_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<Value, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .delete_path(
                &context.base_url,
                RemoteDeletePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                },
            )
            .await;
    }
    workbench::local_delete_workbench_path(state, project_id, worktree_id, path).await
}

/// 预览 SQLite（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     control 面需在 owner 上读取本机或远端 SQLite 预览。
///
/// Code Logic（这个函数做什么）:
///     remote → `preview_sqlite_file` + RemotePreviewSqliteReq；local → local_preview。
async fn preview_sqlite_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    table: Option<String>,
    limit_rows: Option<i64>,
) -> Result<crate::workbench::models::WorkbenchSqlitePreview, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .preview_sqlite_file(
                &context.base_url,
                RemotePreviewSqliteReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                    table,
                    limit_rows,
                },
            )
            .await;
    }
    workbench::local_preview_workbench_sqlite(
        state,
        project_id,
        worktree_id,
        path,
        table,
        limit_rows,
    )
    .await
}

/// 预览 HTML asset（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     HTML/Markdown 预览资源读取需在 owner 上执行，避免 GUI 直连远端。
///
/// Code Logic（这个函数做什么）:
///     remote → preview_html_asset；local → local_preview_workbench_html_asset。
async fn preview_html_asset_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    document_path: String,
    asset_path: String,
) -> Result<crate::workbench::models::WorkbenchHtmlAssetDto, AppError> {
    let project = workbench::get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = workbench::ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            workbench::remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .preview_html_asset(
                &context.base_url,
                RemotePreviewHtmlAssetReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    document_path,
                    asset_path,
                },
            )
            .await;
    }
    workbench::local_preview_workbench_html_asset(
        state,
        project_id,
        worktree_id,
        document_path,
        asset_path,
    )
    .await
}

/// 重命名会话（local/remote）。
///
/// Business Logic（为什么需要这个函数）:
///     remote tab 重命名需写远端 registry，并映射回本机 shortcut projectId。
///
/// Code Logic（这个函数做什么）:
///     与 `rename_workbench_session` 相同：remote entity id → rename + local_project 映射。
async fn rename_session_for_state(
    state: &AppState,
    session_id: String,
    name: String,
) -> Result<crate::workbench::models::WorkbenchSessionDto, AppError> {
    if let Some(parsed) = crate::workbench::remote_ids::parse_remote_entity_id(&session_id) {
        let base_url = workbench::device_base_url(state, &parsed.device_id)?;
        let inner = workbench::remote_inner_session_id(&parsed.device_id, &session_id)?;
        let item = RemoteWorkbenchClient::new()
            .rename_session(&base_url, &inner, &name)
            .await?;
        let local_project_id = workbench::local_project_id_for_remote_inner_project(
            state,
            &parsed.device_id,
            &base_url,
            &item.project_id,
        )
        .await?;
        workbench::ensure_remote_event_bridge_for_project_mapping(
            state,
            &parsed.device_id,
            &base_url,
            &item.project_id,
            &local_project_id,
        );
        return workbench::map_remote_session_dtos_with_project(
            &parsed.device_id,
            Some(&local_project_id),
            vec![item],
        )
        .into_iter()
        .next()
        .ok_or_else(|| AppError::generic("远端 session 重命名结果为空"));
    }
    workbench::local_rename_workbench_session(state, session_id, name).await
}

/// 读取 payload 必填字符串字段。
///
/// Business Logic（为什么需要这个函数）:
///     control op payload 为松散 JSON，需统一校验必填 camelCase 字段。
///
/// Code Logic（这个函数做什么）:
///     取 string；缺失/非 string → validation 错误。
fn required_string(payload: &Value, key: &str) -> Result<String, AppError> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::validation(format!("{key} 必填")))
}

/// 读取 payload 可选字符串字段。
///
/// Business Logic（为什么需要这个函数）:
///     可选字段（worktreeId/path 等）缺失时保持 None。
///
/// Code Logic（这个函数做什么）:
///     null 或缺省 → None；非 string 忽略为 None。
fn optional_string(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(|s| s.to_string())
        }
    })
}

/// 读取 payload 可选布尔字段。
///
/// Business Logic（为什么需要这个函数）:
///     worktrees.remove 的 force 为可选开关。
///
/// Code Logic（这个函数做什么）:
///     仅接受 JSON bool；其它类型视为 None。
fn optional_bool(payload: &Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(|v| v.as_bool())
}

/// loopback + control token 双重鉴权。
///
/// Business Logic（为什么需要这个函数）:
///     control API 不是 LAN 业务面：非本机 peer 即使持有 token 也必须 403；
///     token 不匹配返回 401；从不把 token 写入日志。
///
/// Code Logic（这个函数做什么）:
///     先 `require_loopback_peer`，再读控制文件比较 token；空 token 拒绝。
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

/// 校验请求 token 是否匹配控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     与 stop/status route 一致：空 token 或缺失控制文件一律失败。
///
/// Code Logic（这个函数做什么）:
///     控制文件存在且请求 token 非空并与 `control_token` 完全一致。
fn control_token_matches(request_token: &str, control: Option<&BackendControlFile>) -> bool {
    let Some(control) = control else {
        return false;
    };
    !request_token.is_empty() && request_token == control.control_token
}

/// 序列化后检查响应不超过 1 MiB。
///
/// Business Logic（为什么需要这个函数）:
///     metadata control 响应有独立 1 MiB 上限，防止列表/DTO 意外膨胀。
///
/// Code Logic（这个函数做什么）:
///     serde_json 序列化后比长度；超限返回 413。
fn ensure_response_within_limit<T: Serialize>(
    value: &T,
    context: &P2pRequestContext,
) -> Result<(), P2pError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| P2pError::from_code("控制响应序列化失败", P2pErrorCode::Internal, context))?;
    if encoded.len() > CONTROL_RESPONSE_BODY_LIMIT_BYTES {
        return Err(P2pError::from_code(
            "控制响应超过 1 MiB 限制",
            P2pErrorCode::PayloadTooLarge,
            context,
        ));
    }
    Ok(())
}
