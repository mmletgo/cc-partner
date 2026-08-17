//! Workbench 项目笔记命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面右侧「项目笔记」按 owning device 的 inner project id 读写 SQLite；
//!     控制端只发网络请求并 remap 返回的 projectId。
//!
//! Code Logic（这个模块做什么）:
//!     `get/save` + `*_for_state`：remote → P2P；owner-local helper 拒绝 `remote:`。

use crate::error::AppError;
use crate::net::protocol::CAPABILITY_WORKBENCH_PROJECT_NOTES_V1;
use crate::state::AppState;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_ids::is_remote_id;
use crate::workbench::remote_protocol::RemoteProjectNoteSaveReq;
use tauri::State;

pub use crate::workbench::remote_protocol::WorkbenchProjectNoteDto;

use super::common::{ensure_remote_project_context, get_project, proxy_workbench_if_gui};

/// Business Logic（为什么需要这个函数）:
///     owner 路由与本机 helper 不得把 `remote:` shortcut 当成本机主键，避免递归代理。
///
/// Code Logic（这个函数做什么）:
///     `is_remote_id` → `local_project_required`。
pub(crate) fn reject_remote_project_id(project_id: &str) -> Result<(), AppError> {
    if is_remote_id(project_id) {
        return Err(AppError::validation("local_project_required".to_string()));
    }
    Ok(())
}

/// 读取项目笔记。
///
/// Business Logic（为什么需要这个命令）:
///     打开笔记 tab 时加载已保存正文；无记录视为空笔记。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理 `notes.get`；owner 走 for_state（remote 则 P2P）。
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
///     control 与 invoke 共享读取逻辑；远端必须读 owning device。
///
/// Code Logic（这个函数做什么）:
///     remote 项目 → inner id P2P get，返回 DTO 的 projectId remap 成 shortcut；
///     否则走 owner-local get。
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
    if let Ok(project) = get_project(state, &project_id).await {
        if project.kind == "remote" {
            let context = ensure_remote_project_context(state, &project).await?;
            let mut note = RemoteWorkbenchClient::new()
                .with_expected_device_id(&context.device_id)
                .get_project_note(&context.base_url, &context.inner_project_id)
                .await?;
            note.project_id = project.id;
            return Ok(note);
        }
    }
    local_get_workbench_project_note(state, project_id).await
}

/// Business Logic（为什么需要这个函数）:
///     P2P owner 路由只能读本机 inner project，禁止 `remote:` 递归。
///
/// Code Logic（这个函数做什么）:
///     reject remote id 后 repo.get，缺行合成空 DTO。
pub(crate) async fn local_get_workbench_project_note(
    state: &AppState,
    project_id: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    reject_remote_project_id(&project_id)?;
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
///     用户编辑后覆盖 owning device SQLite；关应用前 flush 也走此路径。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理 `notes.save`；owner 走 for_state。
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
///     control 与 invoke 共享写入逻辑；远端必须写 owning device。
///
/// Code Logic（这个函数做什么）:
///     remote 项目 → inner id P2P save，返回 projectId remap 成 shortcut；
///     否则走 owner-local upsert。
pub async fn save_workbench_project_note_for_state(
    state: &AppState,
    project_id: String,
    content: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    let project_id = project_id.trim().to_string();
    if project_id.is_empty() {
        return Err(AppError::validation(
            "workbench_project_note_project_id_required".to_string(),
        ));
    }
    if let Ok(project) = get_project(state, &project_id).await {
        if project.kind == "remote" {
            let context = ensure_remote_project_context(state, &project).await?;
            let mut note = RemoteWorkbenchClient::new()
                .with_expected_device_id(&context.device_id)
                .save_project_note(
                    &context.base_url,
                    RemoteProjectNoteSaveReq {
                        project_id: context.inner_project_id,
                        content,
                    },
                )
                .await?;
            note.project_id = project.id;
            return Ok(note);
        }
    }
    local_save_workbench_project_note(state, project_id, content).await
}

/// Business Logic（为什么需要这个函数）:
///     P2P owner 路由只能写本机 inner project。
///
/// Code Logic（这个函数做什么）:
///     reject remote id 后 repo.upsert。
pub(crate) async fn local_save_workbench_project_note(
    state: &AppState,
    project_id: String,
    content: String,
) -> Result<WorkbenchProjectNoteDto, AppError> {
    reject_remote_project_id(&project_id)?;
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

/// 文档锚点：capability 与路由同名，避免未引用告警。
#[allow(dead_code)]
const fn notes_capability_token() -> &'static str {
    CAPABILITY_WORKBENCH_PROJECT_NOTES_V1
}

#[cfg(test)]
mod tests {
    use super::{notes_capability_token, reject_remote_project_id};

    /// Business Logic（为什么需要这个测试）:
    ///     owner 路径必须拒绝 `remote:`，稳定 code 供 P2P 信封使用。
    #[test]
    fn reject_remote_project_id_uses_stable_code() {
        let err = reject_remote_project_id("remote:dev-1:inner").expect_err("remote");
        assert_eq!(err.code(), "local_project_required");
        assert!(reject_remote_project_id("local-project").is_ok());
        assert_eq!(notes_capability_token(), "workbench.project-notes.v1");
    }
}
