//! commands/workbench.rs — 工作台 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端工作台页面需要管理最近项目、在项目目录中开启多个普通终端，
//!     并在右侧检查器中交互式浏览和整理项目文件夹。
//!
//! Code Logic（这个模块做什么）:
//!     封装 workbench_projects 仓库、内存 PTY session registry 和本机文件系统 helper。
//!     项目与会话命令直接操作共享状态；文件系统命令用 spawn_blocking 包裹同步 IO，
//!     避免阻塞 Tauri async runtime。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchPathInfo, WorkbenchProjectDto, WorkbenchProjectRow,
    WorkbenchSessionDto,
};
use crate::workbench::sessions::{kill_persisted_backend, PaneCloseOutcome, PaneSplitDirection};
use crate::workbench::{fs as workbench_fs, projects};
use chrono::Utc;
use std::path::PathBuf;
use tauri::{AppHandle, State};

/// Business Logic（为什么需要这个函数）:
///     多个命令都需要用 project_id 查找最近项目记录，并在缺失时给前端明确错误。
///
/// Code Logic（这个函数做什么）:
///     从 WorkbenchProjectRepo 读取项目；None 转换为 AppError::not_found。
async fn get_project(state: &AppState, project_id: &str) -> Result<WorkbenchProjectRow, AppError> {
    state
        .workbench_project_repo
        .get(project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))
}

/// Business Logic（为什么需要这个函数）:
///     项目命令创建或更新时间时需要统一使用 UTC ISO 字符串，保持与其他模块字段一致。
///
/// Code Logic（这个函数做什么）:
///     返回当前 UTC 时间的 RFC3339 字符串。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 会话列表既要包含 SQLite 中待恢复的历史 tab，也要优先展示当前运行期 registry 的实时状态。
///
/// Code Logic（这个函数做什么）:
///     先把持久化 row 投影为 DTO，再用 registry 中的实时 DTO 按 id 覆盖同名项。
async fn merged_session_dtos(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    let mut sessions: Vec<WorkbenchSessionDto> = state
        .workbench_session_repo
        .list(project_id)
        .await?
        .iter()
        .map(|row| row.to_dto())
        .collect();
    for live in state.workbench_sessions.list(project_id) {
        if let Some(existing) = sessions.iter_mut().find(|session| session.id == live.id) {
            *existing = live;
        } else {
            sessions.push(live);
        }
    }
    Ok(sessions)
}

/// Business Logic（为什么需要这个函数）:
///     应用重启后，进入工作台项目时应自动恢复之前打开的终端 tab 和可重连上下文。
///
/// Code Logic（这个函数做什么）:
///     读取持久化会话；registry 已有则跳过；项目存在则调用 registry.restore，成功后写回最新 row，
///     项目缺失则删除孤儿会话。
async fn restore_persisted_sessions(
    state: &AppState,
    app_handle: AppHandle,
    project_id: Option<&str>,
) -> Result<(), AppError> {
    let rows = state.workbench_session_repo.list(project_id).await?;
    for row in rows {
        if state.workbench_sessions.contains(&row.id) {
            continue;
        }
        let Some(project) = state.workbench_project_repo.get(&row.project_id).await? else {
            state.workbench_session_repo.delete(&row.id).await?;
            continue;
        };
        match state
            .workbench_sessions
            .restore(app_handle.clone(), project, row.clone())
        {
            Ok(restored) => {
                state.workbench_session_repo.upsert(&restored).await?;
            }
            Err(error) => {
                tracing::warn!("恢复工作台终端会话失败: {error}");
                let mut disconnected = row;
                disconnected.status = "disconnected".to_string();
                disconnected.exited_at = Some(now_iso());
                disconnected.updated_at = now_iso();
                state.workbench_session_repo.upsert(&disconnected).await?;
            }
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     文件系统操作是同步阻塞 IO；命令层必须把它们移到 blocking pool，避免卡住 async runtime。
///
/// Code Logic（这个函数做什么）:
///     包装 tauri::async_runtime::spawn_blocking，并把 JoinError 转换为 AppError。
async fn run_blocking_fs<T, F>(task: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AppError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| AppError::generic(format!("工作台文件任务执行失败: {error}")))?
}

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

/// 添加或重新打开一个本机项目文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     用户指定本机或已挂载局域网文件夹后，工作台需要保存它并在该目录中启动终端与文件树。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 输入路径并要求是目录；同路径已有记录则复用 id/created_at，只更新时间；
///     新路径生成 UUID 项目 id，kind 固定为 local，设备信息来自 AppState/config。
#[tauri::command]
pub async fn add_workbench_project(
    state: State<'_, AppState>,
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

/// 列出工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     前端需要按项目查看当前运行期内的多个终端，也需要在全局恢复 tab 列表。
///
/// Code Logic（这个函数做什么）:
///     先从 SQLite 按需恢复缺失会话，再合并持久化列表和 registry 实时状态返回。
#[tauri::command]
pub async fn list_workbench_sessions(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    project_id: Option<String>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    restore_persisted_sessions(&state, app_handle, project_id.as_deref()).await?;
    merged_session_dtos(&state, project_id.as_deref()).await
}

/// 在项目目录中创建一个普通 PTY 终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     用户在工作台中打开终端时，应只进入当前项目根目录的 shell，不自动运行 Claude Code。
///
/// Code Logic（这个函数做什么）:
///     读取项目路径；调用 session registry 按前端初始尺寸创建 shell/tmux 会话，写入 SQLite，
///     并通过 Tauri event 推送输出与状态。
#[tauri::command]
pub async fn create_workbench_session(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    project_id: String,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
) -> Result<WorkbenchSessionDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let row = state
        .workbench_sessions
        .create(app_handle, project, initial_cols, initial_rows)?;
    state.workbench_session_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 向工作台终端写入输入。
///
/// Business Logic（为什么需要这个函数）:
///     xterm 捕获到用户键盘输入后，需要把字节流转发给对应 PTY。
///
/// Code Logic（这个函数做什么）:
///     查找 session writer，写入 UTF-8 字符串并 flush，成功返回 sessionId。
#[tauri::command]
pub async fn write_workbench_session_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<serde_json::Value, AppError> {
    state.workbench_sessions.write_input(&session_id, &data)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 调整工作台终端尺寸。
///
/// Business Logic（为什么需要这个函数）:
///     终端面板尺寸变化时，PTY 子进程需要收到新的 cols/rows，避免输出换行错乱。
///
/// Code Logic（这个函数做什么）:
///     更新 registry 中的 row 尺寸，调用 MasterPty::resize，并写回 SQLite。
#[tauri::command]
pub async fn resize_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<serde_json::Value, AppError> {
    let row = state.workbench_sessions.resize(&session_id, cols, rows)?;
    state.workbench_session_repo.upsert(&row).await?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 聚焦工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     顶部 app tab 与真实 tmux window 一一绑定，用户切换 tab 时终端内容也必须切到对应 window。
///
/// Code Logic（这个函数做什么）:
///     调用 registry 对 tmux-backed 会话执行 select-window；raw PTY fallback 直接视为成功。
#[tauri::command]
pub async fn focus_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.workbench_sessions.focus_window(&session_id)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 获取项目当前聚焦的工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在 tmux 底部 status bar 或快捷键中切换 window，顶部 app tab 需要跟随真实 tmux current window。
///
/// Code Logic（这个函数做什么）:
///     校验项目存在，读取项目 tmux session 当前 window id，并映射成 Workbench sessionId 返回。
#[tauri::command]
pub async fn get_focused_workbench_session(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<serde_json::Value, AppError> {
    let _ = get_project(&state, &project_id).await?;
    let session_id = state.workbench_sessions.focused_session_id(&project_id)?;
    Ok(serde_json::json!({ "sessionId": session_id }))
}

/// 分割当前 tmux window 的 pane。
///
/// Business Logic（为什么需要这个函数）:
///     工作台采用真实 tmux 映射后，用户需要在当前 window 内创建左右或上下 pane。
///
/// Code Logic（这个函数做什么）:
///     校验 direction 字符串，读取会话所属项目根路径，调用 registry 执行带 cwd 的 tmux split-window。
#[tauri::command]
pub async fn split_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
    direction: String,
) -> Result<serde_json::Value, AppError> {
    let split_direction = PaneSplitDirection::from_api(&direction)?;
    let row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    let project = get_project(&state, &row.project_id).await?;
    state
        .workbench_sessions
        .split_pane(&session_id, split_direction, &project.path)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "direction": direction }))
}

/// 关闭当前 tmux pane。
///
/// Business Logic（为什么需要这个函数）:
///     用户点击分屏工具栏 X 时，需要关闭当前 active pane；最后一个 pane 会关闭整个 window。
///
/// Code Logic（这个函数做什么）:
///     调用 registry 关闭当前 active pane；若关闭了 window，则销毁持久后端并删除 SQLite row。
#[tauri::command]
pub async fn close_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    match state.workbench_sessions.close_active_pane(&session_id)? {
        PaneCloseOutcome::PaneClosed => {
            Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "closedWindow": false }))
        }
        PaneCloseOutcome::WindowClosed(row) => {
            kill_persisted_backend(&row);
            state.workbench_session_repo.delete(&session_id).await?;
            Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "closedWindow": true }))
        }
    }
}

/// 关闭工作台终端 tab。
///
/// Business Logic（为什么需要这个函数）:
///     用户关闭 tab 后，该会话应从运行期 registry 和 SQLite 中移除，并释放 PTY/tmux 资源。
///
/// Code Logic（这个函数做什么）:
///     优先关闭 registry 中的运行期句柄；若 registry 已无该会话但 SQLite 仍有记录，则清理持久后端并删除记录。
#[tauri::command]
pub async fn close_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    match state.workbench_sessions.close(&session_id) {
        Ok(row) => {
            kill_persisted_backend(&row);
        }
        Err(AppError::NotFound(_)) => {
            let row = state
                .workbench_session_repo
                .get(&session_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
            kill_persisted_backend(&row);
        }
        Err(error) => return Err(error),
    }
    state.workbench_session_repo.delete(&session_id).await?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 重命名工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     同一项目可打开多个终端，用户需要给 tab 起名区分不同任务。
///
/// Code Logic（这个函数做什么）:
///     更新运行期 row 或持久化 row 的 name 字段并返回最新会话。
#[tauri::command]
pub async fn rename_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    name: String,
) -> Result<WorkbenchSessionDto, AppError> {
    match state.workbench_sessions.rename(&session_id, &name) {
        Ok(row) => {
            state.workbench_session_repo.upsert(&row).await?;
            Ok(row.to_dto())
        }
        Err(AppError::NotFound(_)) => {
            let mut row = state
                .workbench_session_repo
                .get(&session_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
            row.name = name.trim().to_string();
            row.updated_at = now_iso();
            state.workbench_session_repo.upsert(&row).await?;
            Ok(row.to_dto())
        }
        Err(error) => Err(error),
    }
}

/// 列出项目目录下的一级文件节点。
///
/// Business Logic（为什么需要这个函数）:
///     右侧检查器需要交互式展开项目文件夹，本期先提供文件树，后续再做文件预览。
///
/// Code Logic（这个函数做什么）:
///     读取项目根路径，把阻塞 list_dir 放入 spawn_blocking 执行；path 为空表示项目根。
#[tauri::command]
pub async fn list_workbench_dir(
    state: State<'_, AppState>,
    project_id: String,
    path: Option<String>,
) -> Result<Vec<WorkbenchFileNode>, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    let relative = path.unwrap_or_default();
    run_blocking_fs(move || workbench_fs::list_dir(&root, &relative)).await
}

/// 查询项目内某个路径的信息。
///
/// Business Logic（为什么需要这个函数）:
///     前端选中文件或文件夹后，需要在检查器里显示类型、大小和更新时间。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用 path_info，并保留项目根路径边界检查。
#[tauri::command]
pub async fn get_workbench_path_info(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    run_blocking_fs(move || workbench_fs::path_info(&root, &path)).await
}

/// 在项目内创建文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台快速创建项目文件，为后续代码或文档编辑打基础。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中验证父路径与单个文件名，create_new 空文件后返回 PathInfo。
#[tauri::command]
pub async fn create_workbench_file(
    state: State<'_, AppState>,
    project_id: String,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    run_blocking_fs(move || workbench_fs::create_file(&root, &parent_path, &name)).await
}

/// 在项目内创建文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台整理项目结构，新建文件夹承载代码、素材或文档。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中验证父路径与单个目录名，创建目录后返回 PathInfo。
#[tauri::command]
pub async fn create_workbench_dir(
    state: State<'_, AppState>,
    project_id: String,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    run_blocking_fs(move || workbench_fs::create_dir(&root, &parent_path, &name)).await
}

/// 重命名项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中重命名文件或文件夹，但不能覆盖已有路径或逃出项目根目录。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用安全 rename_path，保留 Phase B 的 symlink/path 边界检查。
#[tauri::command]
pub async fn rename_workbench_path(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
    new_name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    run_blocking_fs(move || workbench_fs::rename_path(&root, &path, &new_name)).await
}

/// 删除项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中删除项目内文件或文件夹；删除项目根目录被明确拒绝。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用 delete_path；symlink 删除只删除链接本身，不删除目标文件。
#[tauri::command]
pub async fn delete_workbench_path(
    state: State<'_, AppState>,
    project_id: String,
    path: String,
) -> Result<serde_json::Value, AppError> {
    let project = get_project(&state, &project_id).await?;
    let root = PathBuf::from(project.path);
    let deleted_path = path.clone();
    run_blocking_fs(move || workbench_fs::delete_path(&root, &path)).await?;
    Ok(serde_json::json!({ "ok": true, "path": deleted_path }))
}
