//! commands/workbench.rs — 工作台 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端工作台页面需要管理最近项目、在项目目录中开启多个 Claude Code 终端，
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

/// Business Logic（为什么需要这个函数）:
///     会话操作后需要返回最新 DTO，供前端刷新 tab 状态和右侧检查器。
///
/// Code Logic（这个函数做什么）:
///     从 registry 列表中按 session_id 查找会话，缺失时返回 not_found。
fn find_session(state: &AppState, session_id: &str) -> Result<WorkbenchSessionDto, AppError> {
    state
        .workbench_sessions
        .list(None)
        .into_iter()
        .find(|session| session.id == session_id)
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))
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
///     先关闭该项目下仍存在的内存会话，再删除 SQLite 项目记录，返回轻量 ok 对象。
#[tauri::command]
pub async fn remove_workbench_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<serde_json::Value, AppError> {
    let _ = get_project(&state, &project_id).await?;
    let session_ids: Vec<String> = state
        .workbench_sessions
        .list(Some(&project_id))
        .into_iter()
        .map(|session| session.id)
        .collect();
    for session_id in session_ids {
        let _ = state.workbench_sessions.close(&session_id);
    }
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
///     前端需要按项目查看当前运行期内的多个 Claude Code 终端，也需要在全局恢复 tab 列表。
///
/// Code Logic（这个函数做什么）:
///     从内存 registry 读取会话；project_id 为空则返回全部，否则按项目过滤。
#[tauri::command]
pub async fn list_workbench_sessions(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    Ok(state.workbench_sessions.list(project_id.as_deref()))
}

/// 在项目目录中创建一个 Claude Code PTY 会话。
///
/// Business Logic（为什么需要这个函数）:
///     用户在工作台中打开终端时，应在当前项目根目录启动本机 Claude Code CLI。
///
/// Code Logic（这个函数做什么）:
///     读取项目路径；从现有 GitHub Trending/Prompt 优化配置复用 claude_cli_path；
///     调用 session registry 创建 PTY，并通过 Tauri event 推送输出与状态。
#[tauri::command]
pub async fn create_workbench_session(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    project_id: String,
) -> Result<WorkbenchSessionDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let cli_path = {
        let config = state.config.read().expect("config 读锁中毒");
        config.github_trending.claude_cli_path.clone()
    };
    state
        .workbench_sessions
        .create(app_handle, project, cli_path)
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
///     更新 registry 中的 DTO 尺寸，并调用 MasterPty::resize。
#[tauri::command]
pub async fn resize_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<serde_json::Value, AppError> {
    state.workbench_sessions.resize(&session_id, cols, rows)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 停止工作台终端进程。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要终止当前 Claude Code 会话，但可以暂时保留 tab 查看退出状态。
///
/// Code Logic（这个函数做什么）:
///     调用 registry stop 终止进程，并返回当前会话 DTO；退出 watcher 随后会发 exited 事件。
#[tauri::command]
pub async fn stop_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<WorkbenchSessionDto, AppError> {
    state.workbench_sessions.stop(&session_id)?;
    find_session(&state, &session_id)
}

/// 重启工作台终端进程。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要在同一个项目上下文中重新启动 Claude Code 会话，以恢复退出或卡住的终端。
///
/// Code Logic（这个函数做什么）:
///     读取旧会话的项目和名称，关闭旧 PTY 后在同项目创建新会话，并把名称继承给新会话返回。
#[tauri::command]
pub async fn restart_workbench_session(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    session_id: String,
) -> Result<WorkbenchSessionDto, AppError> {
    let previous = find_session(&state, &session_id)?;
    let project = get_project(&state, &previous.project_id).await?;
    let cli_path = {
        let config = state.config.read().expect("config 读锁中毒");
        config.github_trending.claude_cli_path.clone()
    };
    state.workbench_sessions.close(&session_id)?;
    let restarted = state
        .workbench_sessions
        .create(app_handle, project, cli_path)?;
    state
        .workbench_sessions
        .rename(&restarted.id, &previous.name)
}

/// 关闭工作台终端 tab。
///
/// Business Logic（为什么需要这个函数）:
///     用户关闭 tab 后，该会话应从运行期 registry 中移除并释放 PTY 资源。
///
/// Code Logic（这个函数做什么）:
///     从 registry remove 会话并尽力 kill 仍在运行的子进程，返回轻量 ok 对象。
#[tauri::command]
pub async fn close_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.workbench_sessions.close(&session_id)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 重命名工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     同一项目可打开多个终端，用户需要给 tab 起名区分不同任务。
///
/// Code Logic（这个函数做什么）:
///     更新内存 DTO 的 name 字段并返回最新会话。
#[tauri::command]
pub async fn rename_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    name: String,
) -> Result<WorkbenchSessionDto, AppError> {
    state.workbench_sessions.rename(&session_id, &name)
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
