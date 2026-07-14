//! 浏览器预览
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

use super::common::*;
use super::files::*;
use super::git::*;
use super::projects::*;
use super::sessions::*;

/// 发现 Workbench 浏览器预览目标。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Browser tab 需要通过 Tauri invoke 读取本机或远端项目的浏览器候选。
///
/// Code Logic（这个命令做什么）:
///     解包 State 和参数后委托 remote-aware helper，保持桌面与 mobile HTTP 入口语义一致。
#[tauri::command]
pub async fn discover_workbench_browser_targets(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<WorkbenchBrowserDiscovery, AppError> {
    discover_workbench_browser_targets_for_state(state.inner(), project_id, worktree_id).await
}

/// 创建 Workbench 浏览器预览。
///
/// Business Logic（为什么需要这个函数）:
///     用户选定 dev server 后，需要得到桌面端绝对 proxy URL 和手机端同源 proxy path；远端项目只能创建 relay。
///
/// Code Logic（这个函数做什么）:
///     先规范化 target URL；local 项目登记 Local session，remote 项目先在 owner 创 preview 再登记 RemoteRelay，并保存最近 target。
pub(crate) async fn create_workbench_browser_preview_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    target_url: String,
) -> Result<WorkbenchBrowserPreview, AppError> {
    let normalized_target_url = normalize_browser_target_url(&target_url)?;
    let project = get_project(state, &project_id).await?;
    let actual_http_port = state.actual_http_port.load(Ordering::SeqCst);
    if project.kind == "remote" {
        let local_worktree_id = worktree_id.clone();
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        let remote_preview = RemoteWorkbenchClient::new()
            .create_browser_preview(
                &context.base_url,
                &RemoteWorkbenchBrowserPreviewReq {
                    project_id: context.inner_project_id.clone(),
                    worktree_id: inner_worktree_id,
                    target_url: normalized_target_url,
                },
            )
            .await?;
        let preview = state.workbench_browser_previews.create_remote_relay(
            context.local_project_id.clone(),
            local_worktree_id,
            context.base_url,
            remote_preview.preview_id,
            remote_preview.target_url,
            actual_http_port,
        );
        state
            .workbench_browser_repo
            .upsert_target(
                &preview.project_id,
                preview.worktree_id.as_deref(),
                &preview.target_url,
            )
            .await?;
        return Ok(preview);
    }
    let preview = state.workbench_browser_previews.create_local(
        project_id.clone(),
        worktree_id.clone(),
        normalized_target_url,
        actual_http_port,
    );
    state
        .workbench_browser_repo
        .upsert_target(&project_id, worktree_id.as_deref(), &preview.target_url)
        .await?;
    Ok(preview)
}

/// 创建 Workbench 浏览器预览。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Browser tab 需要用 Tauri invoke 创建可嵌入 iframe 的 preview 会话。
///
/// Code Logic（这个命令做什么）:
///     解包 State 和参数后委托 remote-aware helper，返回 camelCase preview DTO。
#[tauri::command]
pub async fn create_workbench_browser_preview(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    target_url: String,
) -> Result<WorkbenchBrowserPreview, AppError> {
    create_workbench_browser_preview_for_state(state.inner(), project_id, worktree_id, target_url)
        .await
}
