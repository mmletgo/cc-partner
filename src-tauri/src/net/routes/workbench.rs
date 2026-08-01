//! net/routes/workbench.rs — Workbench 远端网关与移动端 HTTP 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网设备需要通过现有 P2P HTTP server 暴露 Workbench 远端能力，手机浏览器也需要通过本机 HTTP 入口操作 Workbench。
//!
//! Code Logic（这个模块做什么）:
//!     将 P2P local-only 网关、远端目录 helper 和 mobile remote-aware helper 包装为 axum handler。

use crate::backend::event_bus::BackendRuntimeCursor;
use crate::commands::prompt_optimizer::{
    local_stream_optimize_prompt_to_workbench_session,
    stream_optimize_prompt_to_workbench_session_for_state,
};
use crate::commands::workbench::{
    add_local_workbench_project_from_path, close_workbench_pane_for_state,
    close_workbench_session_for_state, commit_workbench_worktree_for_state,
    create_workbench_browser_preview_for_state, create_workbench_session_for_state,
    create_workbench_worktree_for_state, discover_workbench_browser_targets_for_state,
    focus_workbench_session_for_state, get_claude_session_preview_for_state,
    get_focused_workbench_session_for_state, get_workbench_path_info_for_state,
    list_workbench_dir_for_state, list_workbench_git_commits_for_state,
    list_workbench_sessions_for_state, list_workbench_worktrees_for_state,
    local_close_workbench_pane, local_close_workbench_session, local_commit_workbench_worktree,
    local_create_workbench_dir, local_create_workbench_file, local_create_workbench_session,
    local_create_workbench_worktree, local_delete_workbench_path, local_focus_workbench_session,
    local_get_workbench_path_info, local_get_workbench_worktree, local_list_workbench_dir,
    local_list_workbench_git_commits, local_list_workbench_sessions,
    local_list_workbench_worktrees, local_merge_workbench_worktree, local_open_workbench_file,
    local_preview_workbench_html_asset, local_preview_workbench_sqlite,
    local_push_workbench_worktree, local_remove_workbench_worktree, local_rename_workbench_path,
    local_rename_workbench_session, local_resize_workbench_session, local_save_workbench_text_file,
    local_select_workbench_pane_at, local_split_workbench_pane, local_switch_workbench_pane,
    local_write_workbench_session_input,
    local_zoom_workbench_pane, merge_workbench_worktree_for_state, open_workbench_file_for_state,
    owner_local_preflight_for_state, owner_local_safe_attach_for_state,
    push_workbench_worktree_for_state, remove_workbench_worktree_for_state,
    replay_workbench_session_for_state, resize_workbench_session_for_state,
    resume_claude_session_for_state, save_workbench_text_file_for_state,
    search_claude_sessions_for_state, split_workbench_pane_for_state,
    switch_workbench_pane_for_state, write_workbench_session_input_for_state,
    zoom_workbench_pane_for_state, WorkbenchMergeResultDto,
};
use crate::error::AppError;
use crate::net::error_response::{mark_response_as_passthrough, P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::net::routes::api_error_to_p2p;
use crate::state::AppState;
use crate::workbench::browser_models::{
    WorkbenchBrowserDiscoverReq, WorkbenchBrowserDiscovery, WorkbenchBrowserPreview,
    WorkbenchBrowserPreviewReq,
};
use crate::workbench::browser_proxy::{
    proxy_workbench_browser_request, DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
    MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
};
use crate::workbench::claude_sessions::{SessionPreview, SessionSearchResult};
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchHtmlAssetDto, WorkbenchOpenFileDto,
    WorkbenchPathInfo, WorkbenchProjectDto, WorkbenchProjectRow, WorkbenchRemoteDirectoryEntryDto,
    WorkbenchRemotePathInfoDto, WorkbenchRemoteRootDto, WorkbenchSaveTextResultDto,
    WorkbenchSessionDto, WorkbenchSqlitePreview, WorkbenchWorktreeDto,
};
use crate::workbench::remote_directory;
use crate::workbench::remote_events::encode_workbench_remote_relay_ndjson;
use crate::workbench::remote_protocol::{
    RemoteClaudeSessionReq, RemoteCommitWorktreeReq, RemoteCreatePathReq, RemoteCreateSessionReq,
    RemoteCreateWorktreeReq, RemoteDeletePathReq, RemoteFocusedSessionReq,
    RemoteFocusedSessionResp, RemoteGitCommitsReq, RemoteListDirReq, RemoteListSessionsReq,
    RemoteOpenFileReq, RemotePathInfoReq, RemotePreviewHtmlAssetReq, RemotePreviewSqliteReq,
    RemoteProjectReq, RemotePromptOptimizerReq, RemoteRemoveWorktreeReq, RemoteRenamePathReq,
    RemoteRenameSessionReq, RemoteReplaySessionReq, RemoteResizeSessionReq, RemoteSaveTextReq,
    RemoteSearchClaudeSessionsReq, RemoteSelectPaneAtReq, RemoteSessionReq, RemoteSplitPaneReq,
    RemoteWorktreeReq,
    RemoteWriteSessionInputReq, ResumeClaudeSessionResult,
};
use crate::workbench::remote_protocol::{RemoteSafeAttachReq, RemoteWorkspaceRestorePreflightReq};
use crate::workbench::sessions::WorkbenchSessionReplayDto;
use crate::workbench::workspace_restore::{SafeAttachResult, WorkspaceRestorePlan};
use axum::body::Body;
use axum::extract::{Extension, Path as AxumPath, Query, State};
use axum::http::header;
use axum::http::Request;
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use futures_util::stream;
use serde::Deserialize;
use serde_json::Value;
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

/// 远端路径请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     对端浏览目录、读取路径信息和打开项目时都只需要传递一个远端设备上的绝对路径。
///
/// Code Logic（这个结构体做什么）:
///     反序列化 camelCase JSON 请求体 `{path}`，供 axum handler 使用。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePathReq {
    pub path: String,
}

/// Business Logic（为什么需要这个函数）:
///     所有远端路径类接口都必须拒绝空输入，避免误把空串解释为当前工作目录。
///
/// Code Logic（这个函数做什么）:
///     检查 path trim 后是否为空；为空返回校验错误（HTTP 边界映射 400 validation_error），
///     否则保留原始路径字符串。
fn validate_remote_path(path: String) -> Result<String, AppError> {
    if path.trim().is_empty() {
        return Err(AppError::validation("路径不能为空"));
    }
    Ok(path)
}

/// Business Logic（为什么需要这个函数）:
///     Workbench P2P 网关协议只接受对端本机 local projectId，不能把 remote shortcut 当成本机项目递归代理。
///
/// Code Logic（这个函数做什么）:
///     检查项目 row 的 kind 是否为 local；非 local 返回校验错误（HTTP 边界映射 400 validation_error）。
fn ensure_remote_gateway_local_project(project: &WorkbenchProjectRow) -> Result<(), AppError> {
    if project.kind != "local" {
        return Err(AppError::validation(
            "远端 Workbench 网关只接受对端本机项目",
        ));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     每个 Workbench 远端 worktree/Git/files handler 在调用本地 helper 前都必须先确认项目类型。
///
/// Code Logic（这个函数做什么）:
///     从 Workbench 项目仓库读取 projectId，缺失时返回协议错误，存在时复用 kind guard。
async fn ensure_remote_gateway_local_project_id(
    state: &AppState,
    project_id: &str,
) -> Result<(), AppError> {
    let project = state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Workbench 项目不存在"))?;
    ensure_remote_gateway_local_project(&project)
}

/// Business Logic（为什么需要这个函数）:
///     session-id-only 远端路由也必须避免把 remote shortcut session 递归代理回其他设备。
///
/// Code Logic（这个函数做什么）:
///     从 session row 读取 project_id，再复用项目 kind guard；会话缺失时返回 NotFound。
async fn ensure_remote_gateway_local_session_id(
    state: &AppState,
    session_id: &str,
) -> Result<(), AppError> {
    let row = state
        .workbench_session_repo
        .get(session_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Workbench 会话不存在"))?;
    ensure_remote_gateway_local_project_id(state, &row.project_id).await
}

/// Business Logic（为什么需要这个函数）:
///     worktree-id-only 远端路由也必须拒绝 remote shortcut worktree，避免网关递归代理。
///
/// Code Logic（这个函数做什么）:
///     从 worktree repo 读取 row，再确认其 project_id 指向本机 local 项目；缺失时返回 NotFound。
async fn ensure_remote_gateway_local_worktree_id(
    state: &AppState,
    worktree_id: &str,
) -> Result<(), AppError> {
    let row = state
        .workbench_worktree_repo
        .get(worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("远端 Workbench worktree 不存在"))?;
    ensure_remote_gateway_local_project_id(state, &row.project_id).await
}

/// 返回远端设备可浏览的目录根入口。
///
/// Business Logic（为什么需要这个函数）:
///     用户在另一台设备上添加项目时，需要先看到该设备上的 Home、下载、常用代码目录等入口。
///
/// Code Logic（这个函数做什么）:
///     调用 Workbench remote_directory helper 生成根目录 DTO，并包装为 axum Json。
pub async fn remote_roots() -> P2pResult<Json<Vec<WorkbenchRemoteRootDto>>> {
    Ok(Json(remote_directory::remote_roots()))
}

/// 列出远端设备某个目录下的一级条目。
///
/// Business Logic（为什么需要这个函数）:
///     远端项目选择器需要逐层浏览对端文件系统，直到用户选中项目目录。
///
/// Code Logic（这个函数做什么）:
///     校验 path 非空后调用 `list_remote_directory`，返回目录优先排序的条目列表。
pub async fn remote_list_dir(
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathReq>,
) -> P2pResult<Json<Vec<WorkbenchRemoteDirectoryEntryDto>>> {
    let path = validate_remote_path(req.path)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.fs.list"))?;
    let entries = remote_directory::list_remote_directory(Path::new(&path))
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.fs.list"))?;
    Ok(Json(entries))
}

/// 返回远端设备某个路径的详情。
///
/// Business Logic（为什么需要这个函数）:
///     用户选中目录后，前端需要知道它是否可读、是否是 Git 仓库以及建议项目名。
///
/// Code Logic（这个函数做什么）:
///     校验 path 非空后调用 `remote_path_info`，返回单个路径的元信息 DTO。
pub async fn remote_path_info(
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathReq>,
) -> P2pResult<Json<WorkbenchRemotePathInfoDto>> {
    let path = validate_remote_path(req.path)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.fs.info"))?;
    let info = remote_directory::remote_path_info(Path::new(&path))
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.fs.info"))?;
    Ok(Json(info))
}

/// 在远端设备上打开一个本地项目记录。
///
/// Business Logic（为什么需要这个函数）:
///     本机选择远端目录后，需要让远端设备先创建或复用它自己的 Workbench 项目记录。
///
/// Code Logic（这个函数做什么）:
///     校验 path 非空，随后复用本机 add-project 共享实现，返回远端设备上的 local 项目 DTO。
pub async fn open_remote_project(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathReq>,
) -> P2pResult<Json<WorkbenchProjectDto>> {
    let path = validate_remote_path(req.path)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.projects.open"))?;
    let project = add_local_workbench_project_from_path(&state, path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.projects.open"))?;
    Ok(Json(project))
}

/// 列出当前设备的最近 Workbench 项目。
///
/// Business Logic（为什么需要这个函数）:
///     移动端 `/mobile` 运行在普通浏览器中，不能使用 Tauri invoke，需要通过 HTTP 读取最近项目列表作为入口。
///
/// Code Logic（这个函数做什么）:
///     从 workbench_projects 仓库读取项目 row，并复用 WorkbenchProjectRow::to_dto 转成 camelCase DTO。
pub async fn list_projects(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<Vec<WorkbenchProjectDto>>> {
    let rows = state
        .workbench_project_repo
        .list()
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.projects.list"))?;
    Ok(Json(rows.iter().map(WorkbenchProjectRow::to_dto).collect()))
}

/// 发现远端设备本机项目的浏览器预览候选。
///
/// Business Logic（为什么需要这个函数）:
///     其他设备操作 remote shortcut 时，候选发现必须在项目 owning device 上执行。
///
/// Code Logic（这个函数做什么）:
///     先确认 projectId 属于本设备 local 项目，再调用 Task 1 browser discovery 并返回 discovery DTO。
pub async fn discover_browser_targets(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<WorkbenchBrowserDiscoverReq>,
) -> P2pResult<Json<WorkbenchBrowserDiscovery>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser.discover"))?;
    let discovery = crate::workbench::browser::discover_workbench_browser_targets(
        &state,
        req.project_id,
        req.worktree_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser.discover"))?;
    Ok(Json(discovery))
}

/// 创建远端设备本机项目的浏览器 preview。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 创建 preview 时，owner 设备必须先创建真实 preview session。
///
/// Code Logic（这个函数做什么）:
///     确认 projectId 是本设备 local 项目后，复用 commands helper 创建 local preview 并返回 DTO。
pub async fn create_browser_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<WorkbenchBrowserPreviewReq>,
) -> P2pResult<Json<WorkbenchBrowserPreview>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser.preview"))?;
    let preview = create_workbench_browser_preview_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.target_url,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.browser.preview"))?;
    Ok(Json(preview))
}

/// 代理桌面端浏览器 preview 请求。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端 iframe 必须通过本机 HTTP server 的同源 preview URL 访问 dev server。
///
/// Code Logic（这个函数做什么）:
///     从 path 提取 previewId 和 wildcard path，委托 browser_proxy 按 session 转发 HTTP/WebSocket。
///     透传响应（含上游 4xx/5xx 与流式 body）打上 `BrowserProxyPassthroughMarker`（Finding 1），
///     让 `envelope_fallback_middleware` 跳过信封包装，iframe 能看到 dev server 真实响应。
pub async fn proxy_browser_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    AxumPath((preview_id, path)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> P2pResult<Response> {
    let response = proxy_workbench_browser_request(
        state,
        preview_id,
        path,
        req,
        DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
    )
    .await
    .map_err(|e| api_error_to_p2p(e, &ctx))?;
    Ok(mark_response_as_passthrough(response))
}

/// 列出远端设备本机项目的 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     对端设备打开 remote shortcut 后，需要通过 HTTP 读取本设备上的 local project worktree 列表。
///
/// Code Logic（这个函数做什么）:
///     接收远端 local projectId，委托命令层本地 helper 返回 worktree DTO。
pub async fn list_worktrees(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteProjectReq>,
) -> P2pResult<Json<Vec<WorkbenchWorktreeDto>>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.list"))?;
    let worktrees = local_list_workbench_worktrees(&state, req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.list"))?;
    Ok(Json(worktrees))
}

/// 在远端设备本机项目中创建 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     用户在另一台设备上操作 remote shortcut 时，真实 `git worktree add` 应在项目所在设备执行。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/branchName/baseBranch，委托本地 create worktree helper 并返回 DTO。
pub async fn create_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateWorktreeReq>,
) -> P2pResult<Json<WorkbenchWorktreeDto>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.create"))?;
    let worktree =
        local_create_workbench_worktree(&state, req.project_id, req.branch_name, req.base_branch)
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.create"))?;
    Ok(Json(worktree))
}

/// 获取远端设备本机 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     调用方只有 remote worktree id 时，需要查询该 worktree 的远端 projectId 以恢复本机 shortcut 映射。
///
/// Code Logic（这个函数做什么）:
///     接收本机 local worktreeId，确认所属项目是 local 后返回 worktree DTO。
pub async fn get_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorktreeReq>,
) -> P2pResult<Json<WorkbenchWorktreeDto>> {
    ensure_remote_gateway_local_worktree_id(&state, &req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.get"))?;
    let worktree = local_get_workbench_worktree(&state, req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.get"))?;
    Ok(Json(worktree))
}

/// 提交远端设备本机 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 commit 动作需要在项目所在设备执行，并复用本机 commit message 生成逻辑。
///     旧 peer 不带 clientOperationId 时期望 raw worktree DTO；新 peer 带 id 时返回 mutation envelope。
///
/// Code Logic（这个函数做什么）:
///     确认 worktree 属于 local 项目；有 clientOperationId → ledger+envelope，否则 local_* raw DTO。
pub async fn commit_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCommitWorktreeReq>,
) -> P2pResult<Json<Value>> {
    ensure_remote_gateway_local_worktree_id(&state, &req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.commit"))?;
    let op_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    match op_id {
        Some(client_operation_id) => {
            let envelope = crate::commands::workbench::local_commit_workbench_worktree_with_ledger(
                &state,
                req.worktree_id,
                req.message,
                client_operation_id,
            )
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.commit"))?;
            Ok(Json(serde_json::to_value(envelope).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.commit",
                )
            })?))
        }
        None => {
            let worktree = local_commit_workbench_worktree(&state, req.worktree_id, req.message)
                .await
                .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.commit"))?;
            Ok(Json(serde_json::to_value(worktree).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.commit",
                )
            })?))
        }
    }
}

/// 推送远端设备本机 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 push 动作需要在项目所在设备执行真实 git push。
///     旧 peer 不带 clientOperationId 时期望 raw worktree DTO；新 peer 带 id 时返回 mutation envelope。
///
/// Code Logic（这个函数做什么）:
///     确认 worktree 属于 local 项目；有 clientOperationId → ledger+envelope，否则 local_* raw DTO。
pub async fn push_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorktreeReq>,
) -> P2pResult<Json<Value>> {
    ensure_remote_gateway_local_worktree_id(&state, &req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.push"))?;
    let op_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    match op_id {
        Some(client_operation_id) => {
            let envelope = crate::commands::workbench::local_push_workbench_worktree_with_ledger(
                &state,
                req.worktree_id,
                client_operation_id,
            )
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.push"))?;
            Ok(Json(serde_json::to_value(envelope).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.push",
                )
            })?))
        }
        None => {
            let worktree = local_push_workbench_worktree(&state, req.worktree_id)
                .await
                .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.push"))?;
            Ok(Json(serde_json::to_value(worktree).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.push",
                )
            })?))
        }
    }
}

/// 合并远端设备本机 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 merge 动作需要在项目所在设备推进阶段并发布本机 merge progress 事件。
///     旧 peer 不带 clientOperationId 时期望 raw merge DTO；新 peer 带 id 时返回 mutation envelope。
///
/// Code Logic（这个函数做什么）:
///     确认 worktree 属于 local 项目；有 clientOperationId → ledger+envelope，否则 local_* raw DTO。
pub async fn merge_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorktreeReq>,
) -> P2pResult<Json<Value>> {
    ensure_remote_gateway_local_worktree_id(&state, &req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.merge"))?;
    let op_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    match op_id {
        Some(client_operation_id) => {
            let envelope = crate::commands::workbench::local_merge_workbench_worktree_with_ledger(
                &state,
                req.worktree_id,
                client_operation_id,
            )
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.merge"))?;
            Ok(Json(serde_json::to_value(envelope).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.merge",
                )
            })?))
        }
        None => {
            let result = local_merge_workbench_worktree(&state, req.worktree_id)
                .await
                .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.merge"))?;
            Ok(Json(serde_json::to_value(result).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.merge",
                )
            })?))
        }
    }
}

/// 删除远端设备本机 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 删除 worktree 时，项目所在设备要执行 git worktree remove 并清理元数据。
///     旧 peer 不带 clientOperationId 时期望 raw `{ok,worktreeId}`；新 peer 带 id 时返回 mutation envelope。
///
/// Code Logic（这个函数做什么）:
///     确认 worktree 属于 local 项目；有 clientOperationId → ledger+envelope，否则 local_* raw JSON。
pub async fn remove_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteRemoveWorktreeReq>,
) -> P2pResult<Json<Value>> {
    ensure_remote_gateway_local_worktree_id(&state, &req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.remove"))?;
    let op_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty());
    match op_id {
        Some(client_operation_id) => {
            let envelope = crate::commands::workbench::local_remove_workbench_worktree_with_ledger(
                &state,
                req.worktree_id,
                req.force,
                client_operation_id,
            )
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.remove"))?;
            Ok(Json(serde_json::to_value(envelope).map_err(|e| {
                P2pError::from_app_error(
                    AppError::generic(e.to_string()),
                    &ctx,
                    "workbench.worktrees.remove",
                )
            })?))
        }
        None => {
            let value = local_remove_workbench_worktree(&state, req.worktree_id, req.force)
                .await
                .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.remove"))?;
            Ok(Json(value))
        }
    }
}

/// 查询 workbench mutation operation ledger。
///
/// Business Logic（为什么需要这个函数）:
///     unknown 后 peer/controller 按 clientOperationId 查询 owning device ledger。
///
/// Code Logic（这个函数做什么）:
///     POST body clientOperationId → Option<WorkbenchMutationOperationDto>。
pub async fn get_mutation_operation(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::workbench::remote_protocol::RemoteMutationOperationReq>,
) -> P2pResult<Json<Option<crate::workbench::operation_ledger::WorkbenchMutationOperationDto>>> {
    let item = crate::commands::workbench::get_workbench_mutation_operation_for_state(
        &state,
        req.client_operation_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.worktrees.mutation_operation"))?;
    Ok(Json(item))
}

/// 列出远端设备本机项目的 Git 提交。
///
/// Business Logic（为什么需要这个函数）:
///     本机查看远端项目 Git 历史时，需要让远端设备在自己的 worktree 路径下执行 Git 查询。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/limit，委托本地 Git commits helper，limit 归一到 1..100。
pub async fn list_git_commits(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteGitCommitsReq>,
) -> P2pResult<Json<Vec<WorkbenchGitCommitDto>>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.git.commits"))?;
    let limit = Some(req.limit.clamp(1, 100) as usize);
    let commits = local_list_workbench_git_commits(&state, req.project_id, req.worktree_id, limit)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.git.commits"))?;
    Ok(Json(commits))
}

/// 列出远端设备本机项目目录。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的文件树展开需要在项目所在设备上读取文件系统。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托本地 list_dir helper 返回文件节点。
pub async fn list_workbench_dir(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListDirReq>,
) -> P2pResult<Json<Vec<WorkbenchFileNode>>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.list_dir"))?;
    let nodes = local_list_workbench_dir(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.list_dir"))?;
    Ok(Json(nodes))
}

/// 查询远端设备本机项目内路径信息。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的详情面板需要读取项目所在设备上的 metadata。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托本地 path_info helper 返回统一 DTO。
pub async fn workbench_path_info(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathInfoReq>,
) -> P2pResult<Json<WorkbenchPathInfo>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.info"))?;
    let info = local_get_workbench_path_info(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.info"))?;
    Ok(Json(info))
}

/// 打开远端设备本机项目内文件。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 打开文件时，文件类型检测和预览必须在项目所在设备上执行。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托本地 open-file helper 返回完整文件打开 DTO。
pub async fn open_workbench_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteOpenFileReq>,
) -> P2pResult<Json<WorkbenchOpenFileDto>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.open"))?;
    let file = local_open_workbench_file(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.open"))?;
    Ok(Json(file))
}

/// 保存远端设备本机项目内文本文件。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 保存文件时，需要在项目所在设备上复用本地类型校验与原子保存。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path/content/baseHash，委托本地 save-text helper。
pub async fn save_workbench_text_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSaveTextReq>,
) -> P2pResult<Json<WorkbenchSaveTextResultDto>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.save_text"))?;
    let result = local_save_workbench_text_file(
        &state,
        req.project_id,
        req.worktree_id,
        req.path,
        req.content,
        req.base_hash,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.save_text"))?;
    Ok(Json(result))
}

/// 预览远端设备本机项目内 SQLite 文件。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 SQLite 换表预览必须在项目所在设备执行，不能回退到本机路径解析。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path/table/limitRows，委托本地 SQLite 预览 helper。
pub async fn preview_workbench_sqlite(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePreviewSqliteReq>,
) -> P2pResult<Json<WorkbenchSqlitePreview>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.preview_sqlite"))?;
    let preview = local_preview_workbench_sqlite(
        &state,
        req.project_id,
        req.worktree_id,
        req.path,
        req.table,
        req.limit_rows,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.preview_sqlite"))?;
    Ok(Json(preview))
}

/// 读取远端设备本机项目内 HTML/Markdown 预览资源。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 HTML/Markdown 相对资源必须从项目所在设备读取。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/documentPath/assetPath，委托本地 HTML asset helper。
pub async fn preview_workbench_html_asset(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePreviewHtmlAssetReq>,
) -> P2pResult<Json<WorkbenchHtmlAssetDto>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.preview_html_asset"))?;
    let asset = local_preview_workbench_html_asset(
        &state,
        req.project_id,
        req.worktree_id,
        req.document_path,
        req.asset_path,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.preview_html_asset"))?;
    Ok(Json(asset))
}

/// 在远端设备本机项目内创建文件。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 新建文件时，真实磁盘写入必须发生在项目所在设备。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/parentPath/name，委托本地 create-file helper。
pub async fn create_workbench_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreatePathReq>,
) -> P2pResult<Json<WorkbenchPathInfo>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.create_file"))?;
    let info = local_create_workbench_file(
        &state,
        req.project_id,
        req.worktree_id,
        req.parent_path,
        req.name,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.create_file"))?;
    Ok(Json(info))
}

/// 在远端设备本机项目内创建目录。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 新建目录时，真实磁盘写入必须发生在项目所在设备。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/parentPath/name，委托本地 create-dir helper。
pub async fn create_workbench_dir(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreatePathReq>,
) -> P2pResult<Json<WorkbenchPathInfo>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.create_dir"))?;
    let info = local_create_workbench_dir(
        &state,
        req.project_id,
        req.worktree_id,
        req.parent_path,
        req.name,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.create_dir"))?;
    Ok(Json(info))
}

/// 重命名远端设备本机项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 重命名文件/目录时，需要在项目所在设备上执行安全重命名。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path/newName，委托本地 rename helper。
pub async fn rename_workbench_path(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteRenamePathReq>,
) -> P2pResult<Json<WorkbenchPathInfo>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.rename"))?;
    let info = local_rename_workbench_path(
        &state,
        req.project_id,
        req.worktree_id,
        req.path,
        req.new_name,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.rename"))?;
    Ok(Json(info))
}

/// 删除远端设备本机项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 删除文件/目录时，需要在项目所在设备上执行安全删除。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托本地 delete helper 并返回 `{ok,path}`。
pub async fn delete_workbench_path(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteDeletePathReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.delete"))?;
    let result = local_delete_workbench_path(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.files.delete"))?;
    Ok(Json(result))
}

/// Workbench 事件流 heartbeat 间隔（秒）。
const WORKBENCH_EVENT_HEARTBEAT_INTERVAL_SECS: u64 = 15;

/// 构造 typed heartbeat NDJSON 行。
///
/// Business Logic（为什么需要这个函数）:
///     移动端半开连接需要周期性 heartbeat 重置 client watchdog；格式必须稳定可测。
///
/// Code Logic（这个函数做什么）:
///     输出 `{"type":"heartbeat","sentAt":"<RFC3339>"}\n`，sentAt 为传入时间戳。
pub fn workbench_event_heartbeat_line(sent_at: &str) -> String {
    format!(r#"{{"type":"heartbeat","sentAt":"{sent_at}"}}"#) + "\n"
}

/// `/api/workbench/events` 可选 after 游标查询参数。
///
/// Business Logic（为什么需要这个结构体）:
///     bridge/Mobile 重连需要 afterOwnerInstanceId + afterSequence 做 catch-up 或显式 Gap。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 查询字段；两者齐全且 owner 非空时构成 `BackendRuntimeCursor`。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchEventsQuery {
    pub after_owner_instance_id: Option<String>,
    pub after_sequence: Option<u64>,
}

/// 订阅本机 Workbench 远端事件流。
///
/// Business Logic（为什么需要这个函数）:
///     其他局域网设备需要持续接收本机 terminal 输出、终端状态和 merge 进度，用于 remote shortcut UI；
///     重连必须带 after 游标；lag/owner 变化/after 早于 ring 必须显式 Gap，禁止静默丢弃 Lagged；
///     同时每 15 秒发送 typed heartbeat，防止半开连接卡死。
///
/// Code Logic（这个函数做什么）:
///     解析 afterOwnerInstanceId/afterSequence → `open_relay`；将 Event/Gap 编码为 NDJSON，
///     与 interval heartbeat 合并为 stream。
pub async fn workbench_events(
    State(state): State<AppState>,
    Query(query): Query<WorkbenchEventsQuery>,
) -> Response<Body> {
    let after = match (
        query.after_owner_instance_id.as_deref(),
        query.after_sequence,
    ) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let relay = state.workbench_remote_events.open_relay(after.as_ref());
    let relay = Arc::new(Mutex::new(relay));
    let event_stream = stream::unfold(relay, |relay| async move {
        let msg = {
            let mut guard = relay.lock().await;
            guard.recv().await
        };
        match msg {
            Some(message) => match encode_workbench_remote_relay_ndjson(&message) {
                Ok(line) => Some((Ok::<String, Infallible>(format!("{line}\n")), relay)),
                Err(_) => {
                    tracing::debug!("Workbench 远端事件编码失败，跳过本条（无 body）");
                    Some((Ok::<String, Infallible>(String::new()), relay))
                }
            },
            None => None,
        }
    })
    .filter(|item| match item {
        Ok(line) => !line.is_empty(),
        Err(_) => true,
    });

    // 首个 tick 在 interval 后触发，避免连接瞬间立刻发 heartbeat 掩盖业务帧。
    let mut ticker = interval(Duration::from_secs(WORKBENCH_EVENT_HEARTBEAT_INTERVAL_SECS));
    ticker.tick().await;
    let heartbeat_stream = IntervalStream::new(ticker).map(|_| {
        let sent_at = Utc::now().to_rfc3339();
        Ok::<String, Infallible>(workbench_event_heartbeat_line(&sent_at))
    });

    let merged = stream::select(event_stream, heartbeat_stream);
    let mut response = Response::new(Body::from_stream(merged));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/x-ndjson"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// Agent runtime 有界 snapshot 查询（capability `workbench.agent-runtime.v1`）。
///
/// Business Logic（为什么需要这个函数）:
///     remote/mobile 在 Gap 后需要 owner active Agent baseline；不得暴露 native session id。
///     远端 wait/inspect 可通过 body.agentSessionId 强制纳入终态 session。
///
/// Code Logic（这个函数做什么）:
///     可选 body projectId / agentSessionId；委托
///     `get_agent_runtime_snapshot_for_state_with_include`。
///     **不**提供 LAN Hook ingestion 写路由。
pub async fn agent_runtime_snapshot(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    body: Option<Json<Value>>,
) -> P2pResult<Json<crate::workbench::agent_runtime::AgentRuntimeSnapshot>> {
    let project_id = body
        .as_ref()
        .and_then(|Json(v)| v.get("projectId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let include_agent_session_id = body
        .as_ref()
        .and_then(|Json(v)| v.get("agentSessionId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let snap = crate::workbench::agent_runtime::snapshot::get_agent_runtime_snapshot_for_state_with_include(
        &state,
        project_id,
        include_agent_session_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.agent_runtime.snapshot"))?;
    Ok(Json(snap))
}

/// Agent Metadata Ledger owner-local 时间窗聚合（capability `workbench.agent-ledger-summary.v1`）。
///
/// Business Logic（为什么需要这个函数）:
///     控制设备 Fleet join 只读 owning device 的 24h/7d/30d aggregate；
///     不得暴露 entry 列表、agentSessionId、prompt 或 path。
///
/// Code Logic（这个函数做什么）:
///     校验 window 与 project_ids 上限/ remote id；委托 aggregation::summarize_projects。
pub async fn agent_ledger_summary(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::workbench::agent_ledger::AgentLedgerSummaryBatchReq>,
) -> P2pResult<Json<crate::workbench::agent_ledger::AgentLedgerSummaryBatchResp>> {
    use crate::workbench::agent_ledger::aggregation::summarize_projects;
    use crate::workbench::agent_ledger::models::{
        AgentLedgerSummaryBatchResp, LedgerWindow, AGENT_LEDGER_SUMMARY_MAX_PROJECTS,
    };
    use crate::workbench::remote_ids::is_remote_id;

    let window = LedgerWindow::parse(&req.window).ok_or_else(|| {
        P2pError::from_app_error(
            AppError::validation(format!("invalid ledger window: {}", req.window)),
            &ctx,
            "workbench.agent_ledger.summary",
        )
    })?;

    if req.project_ids.len() > AGENT_LEDGER_SUMMARY_MAX_PROJECTS {
        return Err(P2pError::from_app_error(
            AppError::validation("resource_limit".to_string()),
            &ctx,
            "workbench.agent_ledger.summary",
        ));
    }

    for id in &req.project_ids {
        if is_remote_id(id) {
            return Err(P2pError::from_app_error(
                AppError::validation("local_project_required".to_string()),
                &ctx,
                "workbench.agent_ledger.summary",
            ));
        }
    }

    let projects = summarize_projects(
        &state.agent_ledger_repo,
        &req.project_ids,
        window,
        chrono::Utc::now(),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.agent_ledger.summary"))?;

    Ok(Json(AgentLedgerSummaryBatchResp { window, projects }))
}

/// LAN Agent Fleet owner-local batch 摘要（capability `workbench.lan-fleet.v1`）。
///
/// Business Logic（为什么需要这个函数）:
///     控制设备按 owning device 一次请求已保存 shortcut 对应的本机 project 摘要；
///     本路由只服务本机 local 项目，禁止 remote shortcut 递归与枚举全部项目。
///
/// Code Logic（这个函数做什么）:
///     解析 snake_case `project_ids`/`project_paths`；委托 `build_owner_device_summary`；
///     超 100 返回 resource_limit；remote id 返回 local_project_required。
pub async fn lan_fleet_snapshot(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::workbench::lan_fleet::LanFleetOwnerBatchReq>,
) -> P2pResult<Json<crate::workbench::lan_fleet::LanFleetOwnerBatchResp>> {
    let resp = crate::workbench::lan_fleet::build_owner_device_summary(&state, &req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.lan_fleet.snapshot"))?;
    // 防御：响应 projects 不得含绝对 path 形态 id
    for project in &resp.device.projects {
        if project.project_id.starts_with('/') || project.project_id.contains(":\\") {
            return Err(P2pError::from_app_error(
                AppError::generic("fleet_path_leak"),
                &ctx,
                "workbench.lan_fleet.snapshot",
            ));
        }
    }
    Ok(Json(resp))
}

/// 移动端 / 本机浏览器：控制设备全局 Fleet 聚合。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 不能 Tauri invoke，需要同源 HTTP 拉取已保存 shortcut 的 Fleet 摘要。
///
/// Code Logic（这个函数做什么）:
///     委托 `collect_lan_fleet_for_state`（含 remote fan-out）；非 P2P owner batch。
pub async fn mobile_lan_fleet(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<crate::workbench::lan_fleet::LanFleetSnapshot>> {
    let snap = crate::workbench::lan_fleet::collect_lan_fleet_for_state(&state)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.mobile.lan_fleet"))?;
    Ok(Json(snap))
}

/// 拉取远端设备本机终端最近输出。
///
/// Business Logic（为什么需要这个函数）:
///     移动端首次打开 terminal 时错过了历史事件，需要先 replay 最近输出，再接 `/api/workbench/events` 增量。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，确认它属于对端本机 local 项目；再以原子
///     `require_live_for_replay` 判定 Live|RestoreInProgress|Missing，
///     restore 中映射为 retryable unavailable，避免永久 not_found。
pub async fn replay_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteReplaySessionReq>,
) -> P2pResult<Json<WorkbenchSessionReplayDto>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.replay"))?;
    // R15 M1：统一原子 presence；RestoreInProgress → unavailable（retryable）。
    state
        .workbench_sessions
        .require_live_for_replay(&req.session_id)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.replay"))?;
    let mut replay = state.workbench_sessions.replay(&req.session_id);
    // P2P 对端 replay 必须携带本机 owner，便于调用方 cutover 按 authority 绑定。
    replay.owner_instance_id = Some(state.config_runtime.owner_instance_id().to_string());
    Ok(Json(replay))
}

/// 列出远端设备本机项目终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     对端 remote shortcut 打开后，需要通过 HTTP 拉取项目所在设备上的 terminal window。
///
/// Code Logic（这个函数做什么）:
///     接收可选 projectId；有 projectId 时先确认它是本机 local 项目，再委托本地 session helper。
pub async fn list_workbench_sessions(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListSessionsReq>,
) -> P2pResult<Json<Vec<WorkbenchSessionDto>>> {
    if let Some(project_id) = req.project_id.as_deref() {
        ensure_remote_gateway_local_project_id(&state, project_id)
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.list"))?;
    }
    let sessions = local_list_workbench_sessions(&state, req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.list"))?;
    Ok(Json(sessions))
}

/// 在远端设备本机项目中创建终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 新建 terminal window 时，真实 PTY/tmux 会话必须在项目所在设备启动。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/尺寸，确认 projectId 是 local 后委托本地 create session helper。
pub async fn create_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateSessionReq>,
) -> P2pResult<Json<WorkbenchSessionDto>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.create"))?;
    let session = local_create_workbench_session(
        &state,
        req.project_id,
        req.worktree_id,
        req.initial_cols,
        req.initial_rows,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.create"))?;
    Ok(Json(session))
}

/// 向远端设备本机终端写入输入。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 输入必须写入项目所在设备的 PTY writer。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 write helper。
pub async fn write_workbench_session_input(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWriteSessionInputReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.write"))?;
    let result = local_write_workbench_session_input(&state, req.session_id, req.data)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.write"))?;
    Ok(Json(result))
}

/// 调整远端设备本机终端尺寸。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal viewport 变化必须同步到项目所在设备的 PTY/tmux。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 resize helper。
pub async fn resize_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteResizeSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.resize"))?;
    let result = local_resize_workbench_session(&state, req.session_id, req.cols, req.rows)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.resize"))?;
    Ok(Json(result))
}

/// 聚焦远端设备本机终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     本机切换 remote terminal tab 时，项目所在设备上的 tmux current window 也要切换。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 focus helper。
pub async fn focus_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.focus"))?;
    let result = local_focus_workbench_session(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.focus"))?;
    Ok(Json(result))
}

/// 查询远端设备本机项目当前聚焦终端。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 用户可能在远端 tmux status bar 内切换 window，本机 UI 需要同步 active tab。
///
/// Code Logic（这个函数做什么）:
///     确认 projectId 是 local 后调用 registry focused 查询，并包装为 `{sessionId}`。
pub async fn focused_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteFocusedSessionReq>,
) -> P2pResult<Json<RemoteFocusedSessionResp>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.focused"))?;
    let session_id = state
        .workbench_sessions
        .focused_session_id(&req.project_id, req.worktree_id.as_deref())
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.focused"))?;
    Ok(Json(RemoteFocusedSessionResp { session_id }))
}

/// 分割远端设备本机终端 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 的 pane 分屏必须在项目所在设备上的 tmux window 内执行。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 split-pane helper。
pub async fn split_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSplitPaneReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.split_pane"))?;
    let result = local_split_workbench_pane(&state, req.session_id, req.direction)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.split_pane"))?;
    Ok(Json(result))
}

/// 切换远端设备本机终端到下一个 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 的 active pane 切换必须发生在项目所在设备的 tmux window 内。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 switch-pane helper。
pub async fn switch_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.switch_pane"))?;
    let result = local_switch_workbench_pane(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.switch_pane"))?;
    Ok(Json(result))
}

/// 按坐标选中远端设备本机终端的 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 的点击切换 pane 必须由项目所在设备的 tmux 做坐标命中并 select-pane。
///     该操作以绝对坐标定位，重复执行结果一致。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 select-pane-at helper。
pub async fn select_workbench_pane_at(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSelectPaneAtReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.select_pane_at"))?;
    let result = local_select_workbench_pane_at(&state, req.session_id, req.col, req.row)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.select_pane_at"))?;
    Ok(Json(result))
}

/// 确保远端设备本机终端 active pane 以单 pane 视图显示。
///
/// Business Logic（为什么需要这个函数）:
///     mobile terminal 在多 pane window 内也只应展示当前 active pane，zoom 操作必须发生在远端 tmux。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 zoom-pane helper。
pub async fn zoom_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.zoom_pane"))?;
    let result = local_zoom_workbench_pane(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.zoom_pane"))?;
    Ok(Json(result))
}

/// 关闭远端设备本机终端当前 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 关闭 pane 时，最后一个 pane 可能会关闭整个 window，本机 UI 需要知道结果。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 close-pane helper。
pub async fn close_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.close_pane"))?;
    let result = local_close_workbench_pane(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.close_pane"))?;
    Ok(Json(result))
}

/// 关闭远端设备本机终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal tab 关闭时，项目所在设备应清理真实 PTY/tmux 后端和 SQLite row。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 close session helper。
pub async fn close_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<serde_json::Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.close"))?;
    let result = local_close_workbench_session(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.close"))?;
    Ok(Json(result))
}

/// 重命名远端设备本机终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal tab 改名需要同步项目所在设备的 registry、SQLite 和 tmux window。
///
/// Code Logic（这个函数做什么）:
///     确认 session 属于本机 local 项目后调用本地 rename helper。
pub async fn rename_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteRenameSessionReq>,
) -> P2pResult<Json<WorkbenchSessionDto>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.rename"))?;
    let session = local_rename_workbench_session(&state, req.session_id, req.name)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.sessions.rename"))?;
    Ok(Json(session))
}

/// 在远端设备本机项目上下文中流式优化 Prompt 并写入终端。
///
/// Business Logic（为什么需要这个函数）:
///     本机 remote shortcut 触发 Prompt 优化时，真实 Claude CLI 和 terminal 写入都必须发生在项目所在设备。
///
/// Code Logic（这个函数做什么）:
///     接收远端 local sessionId 与可选远端工作目录，确认 session 属于本机 local 项目后复用本地流式优化 helper。
pub async fn stream_prompt_optimizer_to_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePromptOptimizerReq>,
) -> P2pResult<Json<Value>> {
    ensure_remote_gateway_local_session_id(&state, &req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.prompt_optimizer.stream"))?;
    let result = local_stream_optimize_prompt_to_workbench_session(
        &state,
        req.prompt,
        req.working_directory,
        req.target_language,
        req.session_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.prompt_optimizer.stream"))?;
    Ok(Json(result))
}

/// 手机端发现本机或远端项目的浏览器预览候选。
///
/// Business Logic（为什么需要这个函数）:
///     手机浏览器进入 Workbench Browser tab 时，也需要支持 remote shortcut 的二级代理候选发现。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId，委托 commands 层 remote-aware discover helper。
pub async fn mobile_discover_browser_targets(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<WorkbenchBrowserDiscoverReq>,
) -> P2pResult<Json<WorkbenchBrowserDiscovery>> {
    let discovery =
        discover_workbench_browser_targets_for_state(&state, req.project_id, req.worktree_id)
            .await
            .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.browser.discover"))?;
    Ok(Json(discovery))
}

/// 手机端创建本机或远端项目的浏览器 preview。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 iframe 只能使用本机 HTTP server 同源 proxy path；远端项目由本机创建 relay preview。
///
/// Code Logic（这个函数做什么）:
///     接收 project/worktree/targetUrl，委托 commands 层 remote-aware preview helper。
pub async fn mobile_create_browser_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<WorkbenchBrowserPreviewReq>,
) -> P2pResult<Json<WorkbenchBrowserPreview>> {
    let preview = create_workbench_browser_preview_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.target_url,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.browser.preview"))?;
    Ok(Json(preview))
}

/// 代理手机端浏览器 preview 请求。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` iframe 不能直接访问远端设备或 loopback target，必须通过当前设备同源 proxy path。
///
/// Code Logic（这个函数做什么）:
///     从 mobile proxy path 提取 previewId 和 wildcard path，复用 browser_proxy 转发逻辑。
pub async fn mobile_proxy_browser_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    AxumPath((preview_id, path)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> Result<Response, P2pError> {
    let response = proxy_workbench_browser_request(
        state,
        preview_id,
        path,
        req,
        MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
    )
    .await
    .map_err(|e| api_error_to_p2p(e, &ctx))?;
    Ok(mark_response_as_passthrough(response))
}

/// 列出手机端可管理的 Workbench 项目。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 需要看到本机项目和远端 project shortcut，作为二级代理链路的入口。
///
/// Code Logic（这个函数做什么）:
///     读取本机 Workbench 项目仓库并直接返回 DTO；远端快捷方式保持 remote kind。
pub async fn mobile_list_projects(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<Vec<WorkbenchProjectDto>>> {
    let rows = state
        .workbench_project_repo
        .list()
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.projects.list"))?;
    Ok(Json(rows.iter().map(WorkbenchProjectRow::to_dto).collect()))
}

/// Mobile Gap inventory 活跃 remote event bridge 设备列表 DTO（camelCase）。
///
/// Business Logic（为什么需要这个结构体）:
///     Mobile Gap recovery 只应对活跃 bridge 上的 remote shortcut fail-closed，
///     需要稳定可读的 deviceIds 列表（R39 M1）。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 `{ deviceIds: string[] }`；ids 已排序。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileActiveBridgeDevicesDto {
    pub device_ids: Vec<String>,
}

/// 手机端读取仍在运行的 remote event bridge 设备 id。
///
/// Business Logic（为什么需要这个函数）:
///     Mobile production Gap inventory 必须对齐桌面 `bridges.active_devices`：
///     仅对活跃 bridge 设备 fail-closed，offline remote 跳过（R39 M1）。
///
/// Code Logic（这个函数做什么）:
///     读取 `workbench_remote_event_bridges.active_device_ids()`，排序后返回
///     camelCase `{ deviceIds }`；body 忽略（与其它 mobile workbench POST 一致）。
pub async fn mobile_list_active_bridge_devices(
    State(state): State<AppState>,
    Extension(_ctx): Extension<P2pRequestContext>,
    Json(_req): Json<serde_json::Value>,
) -> P2pResult<Json<MobileActiveBridgeDevicesDto>> {
    let mut device_ids: Vec<String> = state
        .workbench_remote_event_bridges
        .active_device_ids()
        .into_iter()
        .collect();
    device_ids.sort();
    Ok(Json(MobileActiveBridgeDevicesDto { device_ids }))
}

/// Mobile Gap inventory 活跃 mapped local project 列表 DTO（camelCase）。
///
/// Business Logic（为什么需要这个结构体）:
///     R42 M2：同设备活跃 P1 + 失效 P2 时，仅按 device 枚举会 list 失败阻塞整次 resync；
///     必须返回 active bridge 上已映射的 local shortcut projectId。
///
/// Code Logic（这个结构体做什么）:
///     序列化为 `{ localProjectIds: string[] }`；ids 已排序去重。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileActiveMappedProjectsDto {
    pub local_project_ids: Vec<String>,
}

/// 手机端读取 active bridge 上已映射的 local shortcut project id。
///
/// Business Logic（为什么需要这个函数）:
///     Mobile Gap inventory 对齐桌面 `bridges.active_mapped_projects`（R41 M4 / R42 M2）：
///     仅 inventory 已映射活跃 project，空集跳过全部 remote。
///
/// Code Logic（这个函数做什么）:
///     复用 registry `active_mapped_local_project_ids()`，返回 camelCase `{ localProjectIds }`；
///     body 忽略。
pub async fn mobile_list_active_mapped_projects(
    State(state): State<AppState>,
    Extension(_ctx): Extension<P2pRequestContext>,
    Json(_req): Json<serde_json::Value>,
) -> P2pResult<Json<MobileActiveMappedProjectsDto>> {
    let local_project_ids = state
        .workbench_remote_event_bridges
        .active_mapped_local_project_ids();
    Ok(Json(MobileActiveMappedProjectsDto { local_project_ids }))
}

/// 手机端打开本机路径为 Workbench 项目。
///
/// Business Logic（为什么需要这个函数）:
///     移动端 HTTP adapter 需要与桌面项目打开 API 保持同形，便于后续从手机添加本机路径。
///
/// Code Logic（这个函数做什么）:
///     校验 path 非空后复用本机 add-project helper，返回项目 DTO。
pub async fn mobile_open_project(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathReq>,
) -> P2pResult<Json<WorkbenchProjectDto>> {
    let path = validate_remote_path(req.path)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.projects.open"))?;
    let project = add_local_workbench_project_from_path(&state, path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.projects.open"))?;
    Ok(Json(project))
}

/// 手机端列出本机或远端项目的 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机需要像桌面一级代理一样管理远端项目 worktree，而不只停留在自动化面板。
///
/// Code Logic（这个函数做什么）:
///     接收本机 projectId 或 remote shortcut projectId，委托 commands 层 remote-aware helper。
pub async fn mobile_list_worktrees(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteProjectReq>,
) -> P2pResult<Json<Vec<WorkbenchWorktreeDto>>> {
    let worktrees = list_workbench_worktrees_for_state(&state, req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.list"))?;
    Ok(Json(worktrees))
}

/// 手机端创建本机或远端项目 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机端远端项目应支持和本机项目一致的新建功能分支工作区。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/branchName/baseBranch，委托 commands 层 remote-aware helper。
pub async fn mobile_create_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateWorktreeReq>,
) -> P2pResult<Json<WorkbenchWorktreeDto>> {
    let worktree = create_workbench_worktree_for_state(
        &state,
        req.project_id,
        req.branch_name,
        req.base_branch,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.create"))?;
    Ok(Json(worktree))
}

/// 手机端提交本机或远端 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 Git 面板需要能提交远端设备项目的改动，保持与桌面 Workbench 一致。
///
/// Code Logic（这个函数做什么）:
///     接收 worktreeId 和可选 message，委托 commands 层按 local/remote 目标执行。
pub async fn mobile_commit_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCommitWorktreeReq>,
) -> P2pResult<
    Json<crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>>,
> {
    let client_operation_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("mobile-{}", uuid::Uuid::new_v4()));
    let envelope = commit_workbench_worktree_for_state(
        &state,
        req.worktree_id,
        req.message,
        client_operation_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.commit"))?;
    Ok(Json(envelope))
}

/// 手机端推送本机或远端 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机端完成提交后应能把远端设备上的真实分支推送到 Git remote。
///
/// Code Logic（这个函数做什么）:
///     接收 worktreeId + clientOperationId，委托 commands 层 remote-aware push helper。
pub async fn mobile_push_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorktreeReq>,
) -> P2pResult<
    Json<crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>>,
> {
    let client_operation_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("mobile-{}", uuid::Uuid::new_v4()));
    let envelope = push_workbench_worktree_for_state(&state, req.worktree_id, client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.push"))?;
    Ok(Json(envelope))
}

/// 手机端合并本机或远端 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机端应能触发远端设备上的 merge/cleanup 流程，并接收映射后的结果。
///
/// Code Logic（这个函数做什么）:
///     接收 worktreeId + clientOperationId，委托 commands 层 remote-aware merge helper。
pub async fn mobile_merge_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorktreeReq>,
) -> P2pResult<
    Json<crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>>,
> {
    let client_operation_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("mobile-{}", uuid::Uuid::new_v4()));
    let envelope = merge_workbench_worktree_for_state(&state, req.worktree_id, client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.merge"))?;
    Ok(Json(envelope))
}

/// 手机端删除本机或远端 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 worktree 面板需要清理远端设备上的废弃功能工作区。
///
/// Code Logic（这个函数做什么）:
///     接收 worktreeId/force/clientOperationId，委托 commands 层 remote-aware remove helper。
/// 手机端查询 workbench mutation operation ledger。
///
/// Business Logic（为什么需要这个函数）:
///     Mobile unknown envelope 后必须按 clientOperationId 查询 owning ledger intent 再对账。
///
/// Code Logic（这个函数做什么）:
///     复用 P2P get_mutation_operation；本机 ledger 查询（远端 ledger 缺失保持 unknown）。
pub async fn mobile_get_mutation_operation(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<crate::workbench::remote_protocol::RemoteMutationOperationReq>,
) -> P2pResult<Json<Option<crate::workbench::operation_ledger::WorkbenchMutationOperationDto>>> {
    get_mutation_operation(State(state), Extension(ctx), Json(req)).await
}

pub async fn mobile_remove_worktree(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteRemoveWorktreeReq>,
) -> P2pResult<Json<crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto<Value>>> {
    let client_operation_id = req
        .client_operation_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("mobile-{}", uuid::Uuid::new_v4()));
    let envelope = remove_workbench_worktree_for_state(
        &state,
        req.worktree_id,
        req.force,
        client_operation_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.worktrees.remove"))?;
    Ok(Json(envelope))
}

/// 手机端列出本机或远端项目提交历史。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 Git 面板需要读取远端 active worktree 的真实提交历史。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/limit，委托 commands 层 remote-aware Git helper。
pub async fn mobile_list_git_commits(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteGitCommitsReq>,
) -> P2pResult<Json<Vec<WorkbenchGitCommitDto>>> {
    let commits = list_workbench_git_commits_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        Some(req.limit.clamp(1, 100) as usize),
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.git.commits"))?;
    Ok(Json(commits))
}

/// 手机端列出本机或远端项目目录。
///
/// Business Logic（为什么需要这个函数）:
///     手机端文件面板需要浏览远端设备项目目录，而不是只能操作本机项目。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托 commands 层 remote-aware 文件树 helper。
pub async fn mobile_list_workbench_dir(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListDirReq>,
) -> P2pResult<Json<Vec<WorkbenchFileNode>>> {
    let entries = list_workbench_dir_for_state(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.files.list_dir"))?;
    Ok(Json(entries))
}

/// 手机端读取本机或远端项目路径信息。
///
/// Business Logic（为什么需要这个函数）:
///     手机端文件面板选中远端路径后需要显示类型、大小和可读性等 metadata。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托 commands 层 remote-aware path info helper。
pub async fn mobile_workbench_path_info(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePathInfoReq>,
) -> P2pResult<Json<WorkbenchPathInfo>> {
    let info = get_workbench_path_info_for_state(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.files.info"))?;
    Ok(Json(info))
}

/// 手机端打开本机或远端项目文件。
///
/// Business Logic（为什么需要这个函数）:
///     手机端文件面板需要打开远端设备上的文本、图片、CSV 和 SQLite 预览。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/path，委托 commands 层 remote-aware open file helper。
pub async fn mobile_open_workbench_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteOpenFileReq>,
) -> P2pResult<Json<WorkbenchOpenFileDto>> {
    let file = open_workbench_file_for_state(&state, req.project_id, req.worktree_id, req.path)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.files.open"))?;
    Ok(Json(file))
}

/// 手机端保存本机或远端项目文本文件。
///
/// Business Logic（为什么需要这个函数）:
///     手机端编辑远端项目文本文件时，保存必须发生在项目所在设备并沿用 baseHash 乐观锁。
///
/// Code Logic（这个函数做什么）:
///     接收保存请求，委托 commands 层 remote-aware save-text helper。
pub async fn mobile_save_workbench_text_file(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSaveTextReq>,
) -> P2pResult<Json<WorkbenchSaveTextResultDto>> {
    let result = save_workbench_text_file_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.path,
        req.content,
        req.base_hash,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.files.save_text"))?;
    Ok(Json(result))
}

/// 手机端列出本机或远端项目 terminal window。
///
/// Business Logic（为什么需要这个函数）:
///     手机端进入远端项目后需要看到项目所在设备上的真实 terminal window 列表。
///
/// Code Logic（这个函数做什么）:
///     接收可选 projectId，委托 commands 层 remote-aware session list helper。
pub async fn mobile_list_workbench_sessions(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteListSessionsReq>,
) -> P2pResult<Json<Vec<WorkbenchSessionDto>>> {
    let sessions = list_workbench_sessions_for_state(&state, req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.list"))?;
    Ok(Json(sessions))
}

/// 手机端创建本机或远端 terminal window。
///
/// Business Logic（为什么需要这个函数）:
///     手机端应能在远端设备项目 worktree 中新建真实 shell/tmux 会话。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId/初始尺寸，委托 commands 层 remote-aware create session helper。
pub async fn mobile_create_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteCreateSessionReq>,
) -> P2pResult<Json<WorkbenchSessionDto>> {
    let session = create_workbench_session_for_state(
        &state,
        req.project_id,
        req.worktree_id,
        req.initial_cols,
        req.initial_rows,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.create"))?;
    Ok(Json(session))
}

/// 手机端 replay 本机或远端 terminal 输出。
///
/// Business Logic（为什么需要这个函数）:
///     手机端首次打开远端终端时需要恢复最近屏幕内容，再接 live NDJSON 事件。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware replay helper 并返回映射后的 sessionId。
pub async fn mobile_replay_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteReplaySessionReq>,
) -> P2pResult<Json<WorkbenchSessionReplayDto>> {
    let replay = replay_workbench_session_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.replay"))?;
    Ok(Json(replay))
}

/// 手机端写入本机或远端 terminal 输入。
///
/// Business Logic（为什么需要这个函数）:
///     手机键盘输入需要写入当前项目真实所在设备的 PTY/tmux。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId/data，委托 commands 层 remote-aware write helper。
pub async fn mobile_write_workbench_session_input(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWriteSessionInputReq>,
) -> P2pResult<Json<Value>> {
    let value = write_workbench_session_input_for_state(&state, req.session_id, req.data)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.write"))?;
    Ok(Json(value))
}

/// 手机端调整本机或远端 terminal 尺寸。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 terminal viewport 变化要同步到远端设备 tmux，避免交互式 UI 换行错乱。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId/cols/rows，委托 commands 层 remote-aware resize helper。
pub async fn mobile_resize_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteResizeSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = resize_workbench_session_for_state(&state, req.session_id, req.cols, req.rows)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.resize"))?;
    Ok(Json(value))
}

/// 手机端聚焦本机或远端 terminal window。
///
/// Business Logic（为什么需要这个函数）:
///     手机端切换 terminal tab 时，远端设备上的 tmux current window 也要同步。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware focus helper。
pub async fn mobile_focus_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = focus_workbench_session_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.focus"))?;
    Ok(Json(value))
}

/// 手机端查询本机或远端当前聚焦 terminal。
///
/// Business Logic（为什么需要这个函数）:
///     手机端需要跟随远端 tmux status bar 内发生的 window 切换。
///
/// Code Logic（这个函数做什么）:
///     接收 projectId/worktreeId，委托 commands 层 remote-aware focused helper。
pub async fn mobile_focused_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteFocusedSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = get_focused_workbench_session_for_state(&state, req.project_id, req.worktree_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.focused"))?;
    Ok(Json(value))
}

/// 手机端新增本机或远端 terminal pane。
///
/// Business Logic（为什么需要这个函数）:
///     手机端 pane 工具栏需要在远端设备上的真实 tmux window 中新增 pane。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId/direction，委托 commands 层 remote-aware split-pane helper。
pub async fn mobile_split_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSplitPaneReq>,
) -> P2pResult<Json<Value>> {
    let value = split_workbench_pane_for_state(&state, req.session_id, req.direction)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.split_pane"))?;
    Ok(Json(value))
}

/// 手机端切换本机或远端 terminal pane。
///
/// Business Logic（为什么需要这个函数）:
///     手机端单 pane 视图需要能循环切换远端 tmux active pane。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware switch-pane helper。
pub async fn mobile_switch_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = switch_workbench_pane_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.switch_pane"))?;
    Ok(Json(value))
}

/// 手机端确保本机或远端 terminal pane zoom。
///
/// Business Logic（为什么需要这个函数）:
///     手机屏幕空间有限，远端多 pane window 也必须只显示当前 active pane。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware zoom-pane helper。
pub async fn mobile_zoom_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = zoom_workbench_pane_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.zoom_pane"))?;
    Ok(Json(value))
}

/// 手机端关闭本机或远端 terminal pane。
///
/// Business Logic（为什么需要这个函数）:
///     手机端关闭 pane 时，最后一个 pane 会关闭远端 window，前端需要知道 closedWindow。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware close-pane helper。
pub async fn mobile_close_workbench_pane(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = close_workbench_pane_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.close_pane"))?;
    Ok(Json(value))
}

/// 手机端关闭本机或远端 terminal window。
///
/// Business Logic（为什么需要这个函数）:
///     手机端关闭远端 terminal tab 时，真实清理必须发生在项目所在设备。
///
/// Code Logic（这个函数做什么）:
///     接收 sessionId，委托 commands 层 remote-aware close session helper。
pub async fn mobile_close_workbench_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSessionReq>,
) -> P2pResult<Json<Value>> {
    let value = close_workbench_session_for_state(&state, req.session_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.sessions.close"))?;
    Ok(Json(value))
}

/// 手机端把 Prompt 优化后写入本机或远端 terminal。
///
/// Business Logic（为什么需要这个函数）:
///     手机端远端项目的 Prompt 面板需要在远端设备执行 Claude CLI，并把结果写入远端 terminal。
///
/// Code Logic（这个函数做什么）:
///     接收 prompt/workingDirectory/targetLanguage/sessionId，委托 commands 层 remote-aware prompt helper。
pub async fn mobile_stream_prompt_optimizer_to_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemotePromptOptimizerReq>,
) -> P2pResult<Json<Value>> {
    let value = stream_optimize_prompt_to_workbench_session_for_state(
        &state,
        req.prompt,
        req.working_directory,
        req.target_language,
        req.session_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.prompt_optimizer.stream"))?;
    Ok(Json(value))
}

/// 搜索远端设备本机 worktree 内的 Claude Code 历史 session。
///
/// Business Logic（为什么需要这个函数）:
///     对端设备在 remote shortcut 上搜索历史 Claude 会话时，transcript 索引扫描必须在项目所在设备完成。
///
/// Code Logic（这个函数做什么）:
///     接收远端 local projectId/worktreeId/query，确认 projectId 是本机 local 后委托命令层
///     search_claude_sessions_for_state（local 分支），返回 `SessionSearchResult` DTO
///     （items + truncated + diagnostics；sessionId 为 Claude transcript UUID，无需包装）。
pub async fn search_claude_sessions(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSearchClaudeSessionsReq>,
) -> P2pResult<Json<SessionSearchResult>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.search"))?;
    let result = search_claude_sessions_for_state(
        &state,
        &req.project_id,
        req.worktree_id.as_deref(),
        &req.query,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.search"))?;
    Ok(Json(result))
}

/// 读取远端单个 Claude session 的 preview 详情。
///
/// Business Logic（为什么需要这个函数）:
///     对端设备的 preview 面板需要展示远端会话最近消息、cwd 等，只能由项目所在设备解析 transcript。
///
/// Code Logic（这个函数做什么）:
///     接收远端 local projectId/worktreeId/sessionId，确认 projectId 是本机 local 后委托命令层
///     get_claude_session_preview_for_state（local 分支）返回 SessionPreview。
pub async fn get_claude_session_preview(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteClaudeSessionReq>,
) -> P2pResult<Json<SessionPreview>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.preview"))?;
    let preview = get_claude_session_preview_for_state(
        &state,
        &req.project_id,
        req.worktree_id.as_deref(),
        &req.session_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.preview"))?;
    Ok(Json(preview))
}

/// 在远端设备 resume 一个历史 Claude session。
///
/// Business Logic（为什么需要这个函数）:
///     对端设备选中历史会话后，真实 terminal + `claude --resume` 必须在项目所在设备启动。
///
/// Code Logic（这个函数做什么）:
///     接收远端 local projectId/worktreeId/sessionId，确认 projectId 是本机 local 后委托命令层
///     resume_claude_session_for_state（local 分支）；返回的 sessionId 是本机新建 terminal 的 inner id，
///     **不**包装 remote: 前缀（由发起方命令层包装）。
pub async fn resume_claude_session(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteClaudeSessionReq>,
) -> P2pResult<Json<ResumeClaudeSessionResult>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.resume"))?;
    let result = resume_claude_session_for_state(
        &state,
        &req.project_id,
        req.worktree_id.as_deref(),
        &req.session_id,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.claude_sessions.resume"))?;
    Ok(Json(result))
}

/// owner-local workspace restore preflight。
///
/// Business Logic（为什么需要这个函数）:
///     控制设备把 inner project/worktree/session 发给 owning device 做纯读 preflight；
///     禁止 remote shortcut 递归代理。
///
/// Code Logic（这个函数做什么）:
///     校验 project 为 local 后委托 `owner_local_preflight_for_state`。
pub async fn workspace_restore_preflight(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteWorkspaceRestorePreflightReq>,
) -> P2pResult<Json<WorkspaceRestorePlan>> {
    ensure_remote_gateway_local_project_id(&state, &req.project_id)
        .await
        .map_err(|e| {
            // 稳定 code：remote shortcut 递归
            if e.to_string().contains("只接受对端本机") {
                P2pError::from_app_error(
                    AppError::validation("local_project_required".to_string()),
                    &ctx,
                    "workbench.workspace.restore.preflight",
                )
            } else {
                P2pError::from_app_error(e, &ctx, "workbench.workspace.restore.preflight")
            }
        })?;
    let plan = owner_local_preflight_for_state(
        &state,
        req.project_id,
        req.active_worktree_id,
        req.active_session_id,
        req.workspace_view,
        req.inspector_tab,
        req.browser_target_url,
    )
    .await
    .map_err(|e| P2pError::from_app_error(e, &ctx, "workbench.workspace.restore.preflight"))?;
    Ok(Json(plan))
}

/// owner-local safe attach。
///
/// Business Logic（为什么需要这个函数）:
///     owning device 对已有 tmux target 做幂等 attach；禁止创建 shell。
///
/// Code Logic（这个函数做什么）:
///     session 所属 project 必须为 local；委托 `owner_local_safe_attach_for_state`。
pub async fn workspace_restore_safe_attach(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<RemoteSafeAttachReq>,
) -> P2pResult<Json<SafeAttachResult>> {
    let result = owner_local_safe_attach_for_state(&state, req.session_id)
        .await
        .map_err(|e| {
            P2pError::from_app_error(e, &ctx, "workbench.workspace.restore.safe_attach")
        })?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::WorkbenchProjectRow;
    use crate::workbench::remote_protocol::{
        RemoteListDirReq, RemoteReplaySessionReq, RemoteSaveTextReq,
    };

    /// Business Logic（为什么需要这个函数）:
    ///     route guard 测试只关心项目 kind，不需要真实数据库项目。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造最小 WorkbenchProjectRow，并允许测试覆盖 kind 字段。
    fn project_row_with_kind(kind: &str) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "project-1".to_string(),
            name: "Project".to_string(),
            kind: kind.to_string(),
            device_id: "local".to_string(),
            device_name: "Local".to_string(),
            path: "/tmp/project".to_string(),
            last_opened_at: "2026-01-01T00:00:00Z".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Mobile 半开连接依赖 typed heartbeat；wire 形状漂移会导致 client watchdog 永不重置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 heartbeat 行，断言是以换行结尾的 NDJSON，且 JSON type/sentAt 字段正确。
    #[test]
    fn workbench_event_heartbeat_line_is_typed_ndjson() {
        let sent_at = "2026-07-15T00:00:00+00:00";
        let line = workbench_event_heartbeat_line(sent_at);
        assert!(line.ends_with('\n'), "heartbeat must be NDJSON line");
        let value: serde_json::Value =
            serde_json::from_str(line.trim()).expect("heartbeat line must be valid JSON");
        assert_eq!(value["type"], "heartbeat");
        assert_eq!(value["sentAt"], sent_at);
        assert!(
            value.get("payload").is_none(),
            "heartbeat has no payload wrapper"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端目录浏览不能接受空路径，否则对端可能误读当前进程目录或返回不可预测结果。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用 list-dir handler，断言空白 path 在进入文件系统 helper 前被拒绝，
    ///     且错误被映射到边界信封 validation_error（400）。
    #[tokio::test]
    async fn remote_list_dir_rejects_blank_path() {
        let ctx = P2pRequestContext {
            request_id: "req-test".to_string(),
        };
        let error = remote_list_dir(
            Extension(ctx),
            Json(RemotePathReq {
                path: "   ".to_string(),
            }),
        )
        .await
        .expect_err("blank path should be rejected");

        assert_eq!(error.envelope().error, "路径不能为空");
        assert_eq!(error.envelope().code, "validation_error");
        assert_eq!(error.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端路径详情与目录列表使用同一用户输入，空路径也必须一致拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用 path-info handler，断言空白 path 返回中文业务错误并落入边界信封 validation_error。
    #[tokio::test]
    async fn remote_path_info_rejects_blank_path() {
        let ctx = P2pRequestContext {
            request_id: "req-test".to_string(),
        };
        let error = remote_path_info(
            Extension(ctx),
            Json(RemotePathReq {
                path: "\n\t".to_string(),
            }),
        )
        .await
        .expect_err("blank path should be rejected");

        assert_eq!(error.envelope().error, "路径不能为空");
        assert_eq!(error.envelope().code, "validation_error");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端文件保存路由是跨设备文件编辑的写入入口，请求体必须明确携带项目、worktree、路径、内容和 baseHash。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 camelCase JSON 反序列化 save-text 请求体，断言字段进入共享请求 DTO。
    #[test]
    fn remote_save_text_req_accepts_camel_case_body() {
        let req: RemoteSaveTextReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1",
            "worktreeId": "worktree-1",
            "path": "docs/note.md",
            "content": "# Note\n",
            "baseHash": "old-hash"
        }))
        .unwrap();

        assert_eq!(req.project_id, "project-1");
        assert_eq!(req.worktree_id.as_deref(), Some("worktree-1"));
        assert_eq!(req.base_hash, "old-hash");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 SQLite 预览路由需要接收前端命名的 table/limitRows 字段，才能完整代理换表操作。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 camelCase JSON 反序列化 preview-sqlite 请求体，断言字段进入共享请求 DTO。
    #[test]
    fn remote_preview_sqlite_req_accepts_camel_case_body() {
        let req: RemotePreviewSqliteReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1",
            "worktreeId": "worktree-1",
            "path": "data/app.sqlite",
            "table": "notes",
            "limitRows": 100
        }))
        .unwrap();

        assert_eq!(req.project_id, "project-1");
        assert_eq!(req.worktree_id.as_deref(), Some("worktree-1"));
        assert_eq!(req.table.as_deref(), Some("notes"));
        assert_eq!(req.limit_rows, Some(100));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTML/Markdown 远端资源预览必须把当前文档路径和资源引用都传给远端设备解析。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 camelCase JSON 反序列化 preview-html-asset 请求体，断言字段名与前端 invoke 参数一致。
    #[test]
    fn remote_preview_html_asset_req_accepts_camel_case_body() {
        let req: RemotePreviewHtmlAssetReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1",
            "worktreeId": "worktree-1",
            "documentPath": "docs/index.html",
            "assetPath": "./style.css"
        }))
        .unwrap();

        assert_eq!(req.project_id, "project-1");
        assert_eq!(req.worktree_id.as_deref(), Some("worktree-1"));
        assert_eq!(req.document_path, "docs/index.html");
        assert_eq!(req.asset_path, "./style.css");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端文件树列表既支持项目根，也支持子目录；path 为空时应由命令 helper 解释为项目根。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化只有 projectId 的 list-dir 请求，断言可选 worktreeId/path 都保持 None。
    #[test]
    fn remote_list_dir_req_allows_project_root_without_worktree_or_path() {
        let req: RemoteListDirReq = serde_json::from_value(serde_json::json!({
            "projectId": "project-1"
        }))
        .unwrap();

        assert_eq!(req.project_id, "project-1");
        assert!(req.worktree_id.is_none());
        assert!(req.path.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端连接远端终端前需要通过 HTTP 指定 sessionId 拉取最近输出。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 camelCase JSON 反序列化 replay 请求体，断言字段进入共享请求 DTO。
    #[test]
    fn remote_replay_session_req_accepts_camel_case_body() {
        let req: RemoteReplaySessionReq = serde_json::from_value(serde_json::json!({
            "sessionId": "session-1"
        }))
        .unwrap();

        assert_eq!(req.session_id, "session-1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench P2P 网关协议只接受对端本机 local projectId，不能把 remote shortcut 再当成本机项目执行文件或 Git 操作。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接校验 route-level project kind guard：local 通过，remote 返回校验错误
    ///     （分类 Validation，HTTP 边界映射 400 validation_error）。
    #[test]
    fn remote_gateway_project_guard_rejects_non_local_project() {
        assert!(ensure_remote_gateway_local_project(&project_row_with_kind("local")).is_ok());

        let error = ensure_remote_gateway_local_project(&project_row_with_kind("remote"))
            .expect_err("remote shortcut rows must be rejected by P2P route guard");

        assert_eq!(error.to_string(), "远端 Workbench 网关只接受对端本机项目");
        assert_eq!(error.classify(), crate::error::AppErrorCategory::Validation);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     workspace restore owner 路由必须拒绝 remote shortcut 递归。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 kind=remote 触发 local_project_required 稳定 code 路径。
    #[test]
    fn remote_restore_rejects_remote_shortcut_recursion() {
        let error = ensure_remote_gateway_local_project(&project_row_with_kind("remote"))
            .expect_err("remote shortcut must be rejected");
        assert!(error.to_string().contains("本机"));
        let stable = AppError::validation("local_project_required".to_string());
        assert_eq!(stable.code(), "local_project_required");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Fleet owner batch 请求体 snake_case 反序列化与稳定错误 code 常量对齐。
    ///     真实 build_owner_device_summary 校验见 `lan_fleet::collector::tests`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化 snake_case body；断言 resource_limit / local_project_required code token。
    #[test]
    fn owner_batch_request_shape_and_error_codes() {
        use crate::workbench::lan_fleet::LanFleetOwnerBatchReq;
        let parsed: LanFleetOwnerBatchReq = serde_json::from_value(serde_json::json!({
            "project_ids": ["remote:d:p"],
            "project_paths": []
        }))
        .unwrap();
        assert_eq!(parsed.project_ids, vec!["remote:d:p".to_string()]);
        assert!(crate::workbench::remote_ids::is_remote_id("remote:d:p"));
        assert_eq!(
            AppError::validation("resource_limit").code(),
            "resource_limit"
        );
        assert_eq!(
            AppError::validation("local_project_required").code(),
            "local_project_required"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     health 必须宣告 workbench.lan-fleet.v1 才能让控制设备 capability 门控。
    ///
    /// Code Logic（这个测试做什么）:
    ///     server_protocol_info supports CAPABILITY_WORKBENCH_LAN_FLEET_V1。
    #[test]
    fn server_advertises_lan_fleet_capability() {
        use crate::net::protocol::{server_protocol_info, CAPABILITY_WORKBENCH_LAN_FLEET_V1};
        assert!(server_protocol_info().supports(CAPABILITY_WORKBENCH_LAN_FLEET_V1));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     health 必须宣告 agent-ledger-summary.v1 才能 Fleet join 并拒绝旧 peer 伪 0。
    ///
    /// Code Logic（这个测试做什么）:
    ///     server_protocol_info supports CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1。
    #[test]
    fn server_advertises_agent_ledger_summary_capability() {
        use crate::net::protocol::{
            server_protocol_info, CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1,
        };
        assert!(server_protocol_info().supports(CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote summary 序列化不得含 entries/agentSessionId，且必须含 sessions 聚合字段。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 AgentLedgerSummaryBatchResp → JSON；断言无 entries/agentSessionId，有 sessions。
    #[test]
    fn remote_summary_never_serializes_ledger_entries() {
        use crate::workbench::agent_ledger::models::{
            AgentLedgerSummary, AgentLedgerSummaryBatchResp, LedgerUsageCoverage, LedgerWindow,
        };
        let response = AgentLedgerSummaryBatchResp {
            window: LedgerWindow::Days7,
            projects: vec![AgentLedgerSummary {
                window: LedgerWindow::Days7,
                project_id: Some("p-local".into()),
                sessions: 2,
                completed: 1,
                failed: 1,
                cancelled: 0,
                disconnected: 0,
                duration_ms: 1000,
                input_tokens: Some(10),
                output_tokens: None,
                cost_by_currency: vec![],
                usage_coverage: LedgerUsageCoverage::Partial,
            }],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert!(json.get("entries").is_none());
        let text = json.to_string();
        assert!(text.contains("sessions"));
        assert!(!text.contains("agentSessionId"));
        assert!(!text.contains("entries"));
        assert!(!text.contains("prompt"));
        assert!(!text.contains("transcriptPath"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote-wrapped project id 与超限/非法 window 必须稳定拒绝，不得静默汇总。
    ///
    /// Code Logic（这个测试做什么）:
    ///     校验 remote id / resource_limit / window parse 的本地前置条件。
    #[test]
    fn agent_ledger_summary_request_guards() {
        use crate::workbench::agent_ledger::models::{
            AgentLedgerSummaryBatchReq, LedgerWindow, AGENT_LEDGER_SUMMARY_MAX_PROJECTS,
        };
        use crate::workbench::remote_ids::is_remote_id;

        assert!(is_remote_id("remote:d:p"));
        assert!(!is_remote_id("local-uuid"));
        assert!(LedgerWindow::parse("7d").is_some());
        assert!(LedgerWindow::parse("bogus").is_none());
        assert_eq!(AGENT_LEDGER_SUMMARY_MAX_PROJECTS, 100);

        let parsed: AgentLedgerSummaryBatchReq = serde_json::from_value(serde_json::json!({
            "project_ids": ["a", "b"],
            "window": "24h"
        }))
        .unwrap();
        assert_eq!(parsed.project_ids.len(), 2);
        assert_eq!(parsed.window, "24h");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 读路由（list/info/open/preview 等）的错误必须经 P2pError::from_app_error
    ///     映射到信封；本测试覆盖 400（校验）、404（缺失）、503（暂不可用）三类在 read 域的映射。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 validation/not_found/unavailable 三类 AppError 构造 P2pError，断言 status 与 code token
    ///     与 read 路由信封契约一致，且 request_id 取自 context。
    #[test]
    fn read_routes_map_app_error_classes_to_envelope() {
        use crate::error::{AppError, AppErrorCategory};
        use crate::net::error_response::P2pError;
        let ctx = P2pRequestContext {
            request_id: "req-read".to_string(),
        };
        let cases: Vec<(AppError, AppErrorCategory, &str, axum::http::StatusCode)> = vec![
            (
                AppError::validation("路径不能为空"),
                AppErrorCategory::Validation,
                "validation_error",
                axum::http::StatusCode::BAD_REQUEST,
            ),
            (
                AppError::not_found("工作台会话不存在"),
                AppErrorCategory::NotFound,
                "not_found",
                axum::http::StatusCode::NOT_FOUND,
            ),
            (
                AppError::unavailable("项目锁占用"),
                AppErrorCategory::Unavailable,
                "unavailable",
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
            ),
        ];
        for (app, category, code, status) in cases {
            assert_eq!(app.classify(), category, "AppError 分类应匹配");
            let p2p = P2pError::from_app_error(app, &ctx, "workbench.read");
            assert_eq!(p2p.status(), status, "状态码应匹配 code 约定");
            assert_eq!(p2p.envelope().code, code, "code token 应匹配");
            assert_eq!(p2p.envelope().request_id, "req-read");
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 写路由（create/save/rename/delete/session write/commit/push/merge/
    ///     remove/resume）的错误必须经 P2pError::from_app_error 映射到信封；本测试覆盖
    ///     409（冲突，如 baseHash 乐观锁失败、worktree 已存在）与 503（暂不可用，如 tmux/PTY 容量上限）
    ///     两类在 write 域的映射。
    ///
    ///     关于 retryable（Finding 2）：`unavailable`/`timeout` 分类现在默认 retryable=true，
    ///     因为它们属“暂态错误”——错误本身是暂时的，客户端**可以**退避后重试。
    ///     但这是“错误暂态性”信号，不是“该路由应自动重试”的许可：写路由的最终重试决策仍由
    ///     路由的幂等类（docs/p2p-protocol.md）决定。例如 `sessions/write` 是 no-transport-retry，
    ///     即使收到 503 也不应自动重放（重放会重复键入）。客户端应同时参考 retryable 与路由幂等类。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 conflict/unavailable 两类 AppError 构造 P2pError，断言 status/code 与信封契约一致；
    ///     conflict retryable=false（非暂态），unavailable retryable=true（暂态，但写路由不应据此自动重试）。
    #[test]
    fn write_routes_map_conflict_and_unavailable_to_envelope() {
        use crate::error::AppError;
        use crate::net::error_response::P2pError;
        let ctx = P2pRequestContext {
            request_id: "req-write".to_string(),
        };
        // (app, code, status, expected_retryable)
        let cases: Vec<(AppError, &str, axum::http::StatusCode, bool)> = vec![
            (
                AppError::conflict("baseHash 过期，文件已被修改"),
                "conflict",
                axum::http::StatusCode::CONFLICT,
                false,
            ),
            (
                AppError::conflict("worktree 分支已存在"),
                "conflict",
                axum::http::StatusCode::CONFLICT,
                false,
            ),
            (
                AppError::unavailable("tmux 会话数已达上限"),
                "unavailable",
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                true,
            ),
        ];
        for (app, code, status, expected_retryable) in cases {
            let p2p = P2pError::from_app_error(app, &ctx, "workbench.write");
            assert_eq!(p2p.status(), status, "状态码应匹配 code 约定");
            assert_eq!(p2p.envelope().code, code, "code token 应匹配");
            assert_eq!(p2p.envelope().request_id, "req-write");
            assert_eq!(
                p2p.envelope().retryable,
                expected_retryable,
                "retryable 应匹配分类暂态性策略"
            );
            // domain_code 必须写入 details.domain_code（Finding 2）。
            assert_eq!(
                p2p.envelope()
                    .details
                    .get("domain_code")
                    .and_then(|v| v.as_str()),
                Some("workbench.write"),
                "domain_code 应写入 details.domain_code"
            );
        }
    }
}
