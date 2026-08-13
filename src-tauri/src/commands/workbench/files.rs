//! 文件内容
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 中的本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helper。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::claude_sessions::ensure_worktree_session_index_watcher;
use crate::workbench::models::{
    WorkbenchHtmlAssetDto, WorkbenchOpenFileDto, WorkbenchSaveTextResultDto, WorkbenchSessionDto,
    WorkbenchSqlitePreview,
};
use crate::workbench::{
    file_content, fs as workbench_fs, html_assets,
    remote_client::RemoteWorkbenchClient,
    remote_ids::remote_entity_id,
    remote_protocol::{RemotePreviewHtmlAssetReq, RemotePreviewSqliteReq, RemoteSaveTextReq},
    sqlite_preview,
};
use std::collections::HashSet;
use std::path::PathBuf;
use tauri::State;

use super::common::*;
use super::git::open_workbench_file_for_state;

/// 打开当前 worktree 内的文件。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端文件工作区需要打开本机或远端项目内文件。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn open_workbench_file(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchOpenFileDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.open", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone() })).await? {
        return Ok(v);
    }
    open_workbench_file_for_state(state.inner(), project_id, worktree_id, path).await
}

/// 保存当前 worktree 内的文本文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区编辑器需要安全保存 Markdown、代码、文本和结构化配置，同时防止覆盖外部修改。
///
/// Code Logic（这个函数做什么）:
///     先解析安全文件路径并以后端 metadata.name 重新检测真实类型；JSON/TOML 做语义校验但不强制格式化；
///     随后调用原子保存 helper，并返回最新 metadata 与 hash 基线。
pub(crate) async fn local_save_workbench_text_file(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    content: String,
    base_hash: String,
) -> Result<WorkbenchSaveTextResultDto, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let save_root = root.clone();
    let save_path = path.clone();
    let (opened_metadata, file_path) = resolve_workbench_file_path(root, path).await?;
    validate_save_file_type(&opened_metadata.name, &content)?;
    let base_hash = run_blocking_fs(move || {
        file_content::save_text_file_atomic(&file_path, &content, &base_hash)
    })
    .await?;
    let metadata = run_blocking_fs(move || workbench_fs::path_info(&save_root, &save_path)).await?;
    let base_modified_at = metadata.modified_at.clone();

    Ok(WorkbenchSaveTextResultDto {
        metadata,
        base_hash,
        base_modified_at,
    })
}

/// 保存当前 worktree 内的文本文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区编辑器需要安全保存 Markdown、代码、文本和结构化配置，同时防止覆盖外部修改。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把 content/baseHash 转发到远端设备执行；local 项目走原有本机保存 helper。
pub(crate) async fn save_workbench_text_file_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    content: String,
    base_hash: String,
) -> Result<WorkbenchSaveTextResultDto, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .save_text_file(
                &context.base_url,
                RemoteSaveTextReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                    content,
                    base_hash,
                },
            )
            .await;
    }
    local_save_workbench_text_file(state, project_id, worktree_id, path, content, base_hash).await
}

/// 保存当前 worktree 内的文本文件。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端文件编辑器需要保存本机或远端项目中的可编辑文本。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn save_workbench_text_file(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    content: String,
    base_hash: String,
) -> Result<WorkbenchSaveTextResultDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.save_text", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone(), "content": content.clone(), "baseHash": base_hash.clone() })).await? {
        return Ok(v);
    }
    save_workbench_text_file_for_state(
        state.inner(),
        project_id,
        worktree_id,
        path,
        content,
        base_hash,
    )
    .await
}

/// 格式化 JSON 或 TOML 内容。
///
/// Business Logic（为什么需要这个函数）:
///     前端编辑结构化配置时应复用后端保存前校验的同一套解析器，避免前后端格式化结果不一致。
///
/// Code Logic（这个函数做什么）:
///     根据 kind 调用 file_content::format_structured_content，并把格式化文本包装为 `{formatted}`。
#[tauri::command]
pub async fn format_workbench_structured_content(
    state: State<'_, AppState>,
    kind: String,
    content: String,
) -> Result<WorkbenchFormatResult, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "files.format",
        serde_json::json!({ "kind": kind.clone(), "content": content.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    let formatted =
        run_blocking_fs(move || file_content::format_structured_content(&kind, &content)).await?;
    Ok(WorkbenchFormatResult { formatted })
}

/// 预览当前 worktree 内的 SQLite 文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户切换 SQLite 表或调整预览行数时，需要重新读取只读预览，而不重新打开整个文件工作区。
///
/// Code Logic（这个函数做什么）:
///     解析安全文件路径后调用 SQLite 只读预览 helper；只允许枚举表和 LIMIT 查询，不执行用户 SQL。
pub(crate) async fn local_preview_workbench_sqlite(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    table: Option<String>,
    limit_rows: Option<i64>,
) -> Result<WorkbenchSqlitePreview, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let (_, file_path) = resolve_workbench_file_path(root, path).await?;
    sqlite_preview::preview_sqlite_file(&file_path, table, limit_rows.unwrap_or(100)).await
}

/// 预览当前 worktree 内的 SQLite 文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户切换 SQLite 表或调整预览行数时，本机/远端项目都应读取项目所在设备上的数据库。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把请求转发到远端设备；local 项目走本机 SQLite 只读预览 helper。
#[tauri::command]
pub async fn preview_workbench_sqlite(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    table: Option<String>,
    limit_rows: Option<i64>,
) -> Result<WorkbenchSqlitePreview, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.preview_sqlite", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone(), "table": table.clone(), "limitRows": limit_rows.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
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
    local_preview_workbench_sqlite(&state, project_id, worktree_id, path, table, limit_rows).await
}

/// 读取 HTML/Markdown 预览所需的项目内相对资源。
///
/// Business Logic（为什么需要这个函数）:
///     HTML sandbox iframe 和 Markdown WYSIWYG 预览不能直接访问 worktree 文件，需要后端把当前文档引用的相对资源安全内联。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 根、当前文档路径和资源相对路径，在 blocking pool 中只读读取根内文件并返回 data URL。
pub(crate) async fn local_preview_workbench_html_asset(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    document_path: String,
    asset_path: String,
) -> Result<WorkbenchHtmlAssetDto, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || html_assets::preview_html_asset(&root, &document_path, &asset_path))
        .await
}

/// 读取 HTML/Markdown 预览所需的项目内相对资源。
///
/// Business Logic（为什么需要这个函数）:
///     HTML sandbox iframe 和 Markdown WYSIWYG 预览不能直接访问 worktree 文件，本机/远端项目都要从项目所在设备读取资源。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把资源请求转发到远端设备；local 项目复用本机 HTML asset helper。
#[tauri::command]
pub async fn preview_workbench_html_asset(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    document_path: String,
    asset_path: String,
) -> Result<WorkbenchHtmlAssetDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.preview_html_asset", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "documentPath": document_path.clone(), "assetPath": asset_path.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
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
    local_preview_workbench_html_asset(&state, project_id, worktree_id, document_path, asset_path)
        .await
}

/// 列出工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     前端需要按项目查看当前运行期内的多个终端，也需要在全局恢复 tab 列表。
///
/// Code Logic（这个函数做什么）:
///     先从 SQLite 按需恢复缺失会话，再合并持久化列表和 registry 实时状态返回。
pub(crate) async fn local_list_workbench_sessions(
    state: &AppState,
    project_id: Option<String>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    state.runtime_role.require_owner()?;
    restore_persisted_sessions(state, project_id.as_deref()).await?;
    let sessions = merged_session_dtos(state, project_id.as_deref()).await?;
    let worktree_paths: HashSet<PathBuf> = sessions
        .iter()
        .filter(|session| session.status == "running")
        .map(|session| PathBuf::from(&session.cwd))
        .collect();
    for worktree_path in worktree_paths {
        ensure_worktree_session_index_watcher(state, worktree_path);
    }
    Ok(sessions)
}

/// 列出工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     前端需要按项目查看本机或远端 terminal window；未选项目时保持本机列表，避免轮询全部远端设备。
///     R37 H3：远端 list 只能为 running 持有 session watch；非 running 释放，避免 leaked lease 挡 Gap inventory。
///     R38 M2：list 只处理返回行时，peer 上消失的 session（无 status 事件）会永远占 watch；
///     在 map 返回前按 project 做 last-seen running reconcile，释放 previous−running 差集。
///
/// Code Logic（这个函数做什么）:
///     project_id 指向 remote shortcut 时：
///     R42 H2：先 `try_acquire_project_op_lease(project_id)`，lease 内 re-get durable project，
///     再建立事件桥与项目映射、转发远端 list 并映射 session/worktree id；
///     R41 M1：lease 持有至 RPC + bridge + watch reconcile 结束；RPC 后 revalidate project barrier。
///     R42 M1：list 开始时捕获 project watch epoch；仅 epoch 未变时 reconcile。
///     list 响应后对 running ensure_session_watch、非 running release_session_watch；
///     收集本响应 running composite remote session ids，调用
///     `reconcile_session_watches_for_project`；否则调用本地 helper。
pub(crate) async fn list_workbench_sessions_for_state(
    state: &AppState,
    project_id: Option<String>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    if let Some(project_id_value) = project_id.as_deref() {
        // R42 H2：先 lease 再读 project，覆盖“get_project 后 remove 完成再 lease”的竞态。
        let project_lease = state
            .workbench_sessions
            .try_acquire_project_op_lease(project_id_value)?;
        let project = get_project(state, project_id_value).await?;
        if project.kind == "remote" {
            let _project_lease = project_lease;
            let context = ensure_remote_project_context(state, &project).await?;
            ensure_remote_event_bridge_for_context(state, &context);
            // R42 M1：捕获 list 起点的 project watch epoch；create/note 会 bump，迟到的空 list 不得覆盖。
            let list_epoch = state
                .workbench_remote_event_bridges
                .project_watch_epoch(&context.device_id, &context.local_project_id);
            let items = RemoteWorkbenchClient::new()
                .with_expected_device_id(&context.device_id)
                .list_sessions(&context.base_url, Some(&context.inner_project_id))
                .await?;
            // R40/R41：RPC 后 revalidate；remove 已完成则不得 ensure/reconcile。
            state
                .workbench_sessions
                .require_project_not_closing(project_id_value)?;
            // R37 H3：仅 running session ensure watch；listed 非 running 释放残留 lease，
            // 避免 exited/disconnected 永久占 subscribers 导致 Gap inventory incomplete。
            // R38 M2：同时收集 running composite ids，供 project-scoped disappear reconcile。
            let mut running_ids: Vec<String> = Vec::new();
            for item in &items {
                let remote_session_id = remote_entity_id(&context.device_id, &item.id);
                let status = item.status.trim();
                if status.eq_ignore_ascii_case("running") {
                    let _ = state
                        .workbench_remote_event_bridges
                        .ensure_session_watch(&context.device_id, &remote_session_id);
                    running_ids.push(remote_session_id);
                } else {
                    let _ = state
                        .workbench_remote_event_bridges
                        .release_session_watch(&context.device_id, &remote_session_id);
                }
            }
            // R38 M2 / R42 M1：epoch 未变时才 reconcile；变则跳过，防迟到空 list 撤销 create watch。
            let _ = state
                .workbench_remote_event_bridges
                .reconcile_session_watches_for_project_if_epoch(
                    &context.device_id,
                    &context.local_project_id,
                    &running_ids,
                    list_epoch,
                );
            return Ok(map_remote_session_dtos(
                &context.device_id,
                &context.local_project_id,
                items,
            ));
        }
        drop(project_lease);
    }
    local_list_workbench_sessions(state, project_id).await
}
