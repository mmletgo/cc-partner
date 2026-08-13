//! Workbench 项目笔记命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面右侧「项目笔记」按 Workbench 项目 ID 读写本机 SQLite；
//!     GuiClient 必须代理到 sidecar，不得在 GUI 进程自建第二套写路径。
//!
//! Code Logic（这个模块做什么）:
//!     `get_workbench_project_note` / `save_workbench_project_note` + `*_for_state`；
//!     缺行返回空正文 DTO，不预插行。

use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

use super::common::proxy_workbench_if_gui;

/// 项目笔记 DTO（camelCase，对齐前端）。
///
/// Business Logic（为什么需要这个类型）:
///     前端编辑器需要 projectId、正文与最近保存时间。
///
/// Code Logic（这个类型做什么）:
///     序列化 camelCase；updatedAt 缺行时为空串。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchProjectNoteDto {
    /// Workbench 项目 ID。
    pub project_id: String,
    /// Markdown 正文。
    pub content: String,
    /// 最近保存时间（RFC3339）；从未保存时为空串。
    pub updated_at: String,
}

/// 读取项目笔记。
///
/// Business Logic（为什么需要这个命令）:
///     打开笔记 tab 时加载已保存正文；无记录视为空笔记。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理 `notes.get`；owner 读 repo，缺行返回空正文。
#[tauri::command]
pub async fn get_workbench_project_note(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "notes.get",
        serde_json::json!({ "projectId": project_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_workbench_project_note_for_state(state.inner(), project_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享读取逻辑。
///
/// Code Logic（这个函数做什么）:
///     校验 project_id；repo.get 缺行合成空 DTO。
pub async fn get_workbench_project_note_for_state(
    state: &AppState,
    project_id: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    let project_id = project_id.trim().to_string();
    if project_id.is_empty() {
        return Err(AppError::validation(
            "workbench_project_note_project_id_required".to_string(),
        ));
    }
    match state.workbench_project_note_repo.get(&project_id).await? {
        Some(row) => Ok(WorkbenchProjectNoteDto {
            project_id: row.project_id,
            content: row.content,
            updated_at: row.updated_at,
        }),
        None => Ok(WorkbenchProjectNoteDto {
            project_id,
            content: String::new(),
            updated_at: String::new(),
        }),
    }
}

/// 保存项目笔记。
///
/// Business Logic（为什么需要这个命令）:
///     用户编辑后覆盖本机 SQLite；关应用前 flush 也走此路径。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理 `notes.save`（data 通道）；owner upsert。
#[tauri::command]
pub async fn save_workbench_project_note(
    state: State<'_, AppState>,
    project_id: String,
    content: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "notes.save",
        serde_json::json!({
            "projectId": project_id.clone(),
            "content": content.clone(),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    save_workbench_project_note_for_state(state.inner(), project_id, content).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享写入逻辑。
///
/// Code Logic（这个函数做什么）:
///     委托 repo.upsert，映射 DTO。
pub async fn save_workbench_project_note_for_state(
    state: &AppState,
    project_id: String,
    content: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    let row = state
        .workbench_project_note_repo
        .upsert(&project_id, &content)
        .await?;
    Ok(WorkbenchProjectNoteDto {
        project_id: row.project_id,
        content: row.content,
        updated_at: row.updated_at,
    })
}
