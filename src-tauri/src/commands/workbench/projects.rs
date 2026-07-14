//! 项目与远端打开
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 中的本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helper。

#![allow(dead_code)]
#![allow(unused_imports)]

use crate::claude_cli;
use crate::error::AppError;
use crate::models::device::Device;
use crate::state::AppState;
use crate::workbench::browser::{
    discover_workbench_browser_targets as discover_local_workbench_browser_targets,
    normalize_browser_target_url,
};
use crate::workbench::browser_models::{WorkbenchBrowserDiscovery, WorkbenchBrowserPreview};
use crate::workbench::claude_sessions::{
    ensure_worktree_session_index_scanned, search_sessions, to_session_preview, ClaudeSessionIndex,
    SessionPreview, SessionSearchHit,
};
use crate::workbench::models::{
    WorkbenchDetectedFileType, WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchGitStatusDto,
    WorkbenchHtmlAssetDto, WorkbenchOpenFileDto, WorkbenchPathInfo, WorkbenchProjectDto,
    WorkbenchProjectRow, WorkbenchRemoteDirectoryEntryDto, WorkbenchRemotePathInfoDto,
    WorkbenchRemoteRootDto, WorkbenchSaveTextResultDto, WorkbenchSessionDto, WorkbenchSessionRow,
    WorkbenchSqlitePreview, WorkbenchTextContent, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use crate::workbench::sessions::{
    kill_persisted_backend, pane_count_for_row, PaneCloseOutcome, PaneSplitDirection,
    WorkbenchSessionReplayDto,
};
use crate::workbench::{
    file_content, file_preview, fs as workbench_fs, git as workbench_git, html_assets, projects,
    remote_client::RemoteWorkbenchClient,
    remote_events::{
        publish_workbench_remote_event_from_state, RemoteEventBridgeProjectMapping,
        WorkbenchMergeProgressPayload, WorkbenchRemoteEvent,
    },
    remote_ids::{parse_remote_entity_id, remote_entity_id, remote_project_id},
    remote_protocol::{
        RemoteClaudeSessionReq, RemoteCommitWorktreeReq, RemoteCreatePathReq,
        RemoteCreateSessionReq, RemoteCreateWorktreeReq, RemoteDeletePathReq,
        RemotePreviewHtmlAssetReq, RemotePreviewSqliteReq, RemoteRenamePathReq, RemoteSaveTextReq,
        RemoteSearchClaudeSessionsReq, RemoteWorkbenchBrowserDiscoverReq,
        RemoteWorkbenchBrowserPreviewReq, ResumeClaudeSessionResult,
    },
    sqlite_preview,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tauri::State;

use super::browser::*;
use super::common::*;
use super::files::*;
use super::git::*;
use super::sessions::*;

/// 列出工作台最近项目。
///
/// Business Logic（为什么需要这个函数）:
///     工作台左侧项目区需要在应用重启后恢复最近项目列表。
///
/// Code Logic（这个函数做什么）:
///     从 SQLite workbench_projects 按 last_opened_at 倒序读取，并转换为 camelCase DTO。
#[tauri::command]
pub async fn list_workbench_projects(
    state: State<'_, AppState>,
) -> Result<Vec<WorkbenchProjectDto>, AppError> {
    let rows = state.workbench_project_repo.list().await?;
    Ok(rows.iter().map(WorkbenchProjectRow::to_dto).collect())
}

/// 发现 Workbench 浏览器预览目标。
///
/// Business Logic（为什么需要这个函数）:
///     Browser tab 打开时需要展示当前项目或 worktree 可用的 dev server 候选；远端项目必须由 owning device 发现。
///
/// Code Logic（这个函数做什么）:
///     local 项目直接调用 browser discovery；remote shortcut 先恢复远端 local projectId，再调用远端 browser discover 并映射 project/worktree id。
pub(crate) async fn discover_workbench_browser_targets_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let local_worktree_id = worktree_id.clone();
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        let mut discovery = RemoteWorkbenchClient::new()
            .discover_browser_targets(
                &context.base_url,
                &RemoteWorkbenchBrowserDiscoverReq {
                    project_id: context.inner_project_id.clone(),
                    worktree_id: inner_worktree_id,
                },
            )
            .await?;
        discovery.project_id = context.local_project_id;
        discovery.worktree_id = local_worktree_id;
        return Ok(discovery);
    }
    discover_local_workbench_browser_targets(state, project_id, worktree_id).await
}

/// 添加或重新打开一个本机项目文件夹的共享实现。
///
/// Business Logic（为什么需要这个函数）:
///     本机 Tauri 命令和远端 HTTP open-project 路由都需要在执行设备上创建或复用本地项目记录。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 输入路径并要求是目录；同路径已有记录则复用 id/created_at，只更新时间；
///     新路径生成 UUID 项目 id，kind 固定为 local，设备信息来自 AppState/config。
pub async fn add_local_workbench_project_from_path(
    state: &AppState,
    path: String,
) -> Result<WorkbenchProjectDto, AppError> {
    let root = run_blocking_fs(move || projects::canonical_project_root(&path)).await?;
    let canonical_path = root.to_string_lossy().to_string();
    let existing = state
        .workbench_project_repo
        .list()
        .await?
        .into_iter()
        .find(|project| project.path == canonical_path);
    let now = now_iso();
    let device_name = {
        let config = state.config.read().expect("config 读锁中毒");
        config.device_name.clone()
    };

    let row = WorkbenchProjectRow {
        id: existing
            .as_ref()
            .map(|project| project.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: projects::infer_project_name(&root),
        kind: "local".to_string(),
        device_id: state.device_id.as_ref().clone(),
        device_name,
        path: canonical_path,
        last_opened_at: now.clone(),
        created_at: existing
            .as_ref()
            .map(|project| project.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    state.workbench_project_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 添加或重新打开一个本机项目文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     用户指定本机或已挂载局域网文件夹后，工作台需要保存它并在该目录中启动终端与文件树。
///
/// Code Logic（这个函数做什么）:
///     Tauri invoke thin wrapper，委托共享实现处理 canonicalize、复用已有项目和写库。
#[tauri::command]
pub async fn add_workbench_project(
    state: State<'_, AppState>,
    path: String,
) -> Result<WorkbenchProjectDto, AppError> {
    add_local_workbench_project_from_path(&state, path).await
}

/// 列出远端设备可浏览的根目录。
///
/// Business Logic（为什么需要这个函数）:
///     用户从局域网设备添加项目时，需要先选择远端设备上的常用目录入口。
///
/// Code Logic（这个函数做什么）:
///     根据 deviceId 解析 base URL，调用远端 Workbench client 的 roots 接口并返回 DTO 列表。
#[tauri::command]
pub async fn list_workbench_remote_roots(
    state: State<'_, AppState>,
    device_id: String,
) -> Result<Vec<WorkbenchRemoteRootDto>, AppError> {
    let base_url = device_base_url(&state, &device_id)?;
    RemoteWorkbenchClient::new().roots(&base_url).await
}

/// 列出远端设备某个目录下的一级条目。
///
/// Business Logic（为什么需要这个函数）:
///     远端项目选择器需要逐层浏览对端文件系统，直到用户选中目标项目目录。
///
/// Code Logic（这个函数做什么）:
///     根据 deviceId 解析 base URL，POST path 到远端 list-dir 接口并返回目录条目 DTO。
#[tauri::command]
pub async fn list_workbench_remote_dir(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError> {
    let base_url = device_base_url(&state, &device_id)?;
    RemoteWorkbenchClient::new()
        .list_dir(&base_url, &path)
        .await
}

/// 获取远端设备路径信息。
///
/// Business Logic（为什么需要这个函数）:
///     用户选中远端路径时，前端需要展示是否可读、是否为 Git 仓库以及建议项目名。
///
/// Code Logic（这个函数做什么）:
///     根据 deviceId 解析 base URL，POST path 到远端 info 接口并返回路径信息 DTO。
#[tauri::command]
pub async fn get_workbench_remote_path_info(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<WorkbenchRemotePathInfoDto, AppError> {
    let base_url = device_base_url(&state, &device_id)?;
    RemoteWorkbenchClient::new()
        .path_info(&base_url, &path)
        .await
}

/// 打开远端项目并保存本地快捷方式。
///
/// Business Logic（为什么需要这个函数）:
///     用户在本机选择另一台设备上的项目目录后，需要在本机最近项目列表中出现一个 remote 项目入口。
///
/// Code Logic（这个函数做什么）:
///     解析 deviceId → 调远端 open-project → 用远端规范路径构造稳定 remote 项目 row → upsert 本地 SQLite。
#[tauri::command]
pub async fn open_workbench_remote_project(
    state: State<'_, AppState>,
    device_id: String,
    path: String,
) -> Result<WorkbenchProjectDto, AppError> {
    let base_url = device_base_url(&state, &device_id)?;
    let current_device_name = device_name_from_state(&state, &device_id);
    let remote = RemoteWorkbenchClient::new()
        .open_project(&base_url, &path)
        .await?;
    let remote_id = remote_project_id(&device_id, &remote.path);
    let existing = state.workbench_project_repo.get(&remote_id).await?;
    let now = now_iso();
    let row = build_remote_project_shortcut_row(
        &device_id,
        current_device_name.as_deref(),
        &remote,
        existing.as_ref(),
        &now,
    );
    state.workbench_project_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 从工作台最近项目中移除记录。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台列表移除项目，但这不应删除磁盘上的真实项目文件夹。
///
/// Code Logic（这个函数做什么）:
///     先关闭该项目下仍存在的会话并销毁可重连后端，再删除 SQLite 项目与会话记录，返回轻量 ok 对象。
#[tauri::command]
pub async fn remove_workbench_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<serde_json::Value, AppError> {
    let _ = get_project(&state, &project_id).await?;
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

/// 更新项目最近打开时间。
///
/// Business Logic（为什么需要这个函数）:
///     用户切换或打开项目时，最近项目列表需要把当前项目提升到顶部。
///
/// Code Logic（这个函数做什么）:
///     读取现有 row，更新 last_opened_at/updated_at 后 upsert，返回最新 DTO。
#[tauri::command]
pub async fn touch_workbench_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<WorkbenchProjectDto, AppError> {
    let mut row = get_project(&state, &project_id).await?;
    let now = now_iso();
    row.last_opened_at = now.clone();
    row.updated_at = now;
    state.workbench_project_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 列出项目下的 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 顶部需要用 worktree 管理层替代项目路径说明，让用户在主工作区和功能 worktree 间切换。
///
/// Code Logic（这个函数做什么）:
///     确保主 worktree 存在，同步 Git 已有 worktree 到 SQLite，再注入实时 Git 状态 DTO。
pub(crate) async fn local_list_workbench_worktrees(
    state: &AppState,
    project_id: String,
) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
    let project = get_project(state, &project_id).await?;
    ensure_main_worktree(state, &project).await?;
    sync_git_worktrees(state, &project).await?;
    let rows = state
        .workbench_worktree_repo
        .list_by_project(&project_id)
        .await?;
    Ok(rows.iter().map(worktree_to_dto).collect())
}

/// 获取单个本机 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     远端 HTTP 网关需要通过 worktreeId 读取本机 worktree 所属 projectId，供调用方恢复 remote shortcut 映射。
///
/// Code Logic（这个函数做什么）:
///     从 worktree repo 读取 row，缺失返回 NotFound；存在则注入实时 Git 状态并转为 DTO。
pub(crate) async fn local_get_workbench_worktree(
    state: &AppState,
    worktree_id: String,
) -> Result<WorkbenchWorktreeDto, AppError> {
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    Ok(worktree_to_dto(&row))
}

/// 列出项目下的 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 顶部需要用 worktree 管理层替代项目路径说明，让用户在主工作区和功能 worktree 间切换。
///
/// Code Logic（这个函数做什么）:
///     remote 项目先恢复远端 local project id 并转发到对端；local 项目走原有本机 helper。
pub(crate) async fn list_workbench_worktrees_for_state(
    state: &AppState,
    project_id: String,
) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let items = RemoteWorkbenchClient::new()
            .list_worktrees(&context.base_url, &context.inner_project_id)
            .await?;
        return Ok(map_remote_worktree_dtos(
            &context.device_id,
            &context.local_project_id,
            items,
        ));
    }
    local_list_workbench_worktrees(state, project_id).await
}
