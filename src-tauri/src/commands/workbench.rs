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

use crate::claude_cli;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchGitStatusDto, WorkbenchPathInfo,
    WorkbenchProjectDto, WorkbenchProjectRow, WorkbenchSessionDto, WorkbenchWorktreeDto,
    WorkbenchWorktreeRow,
};
use crate::workbench::sessions::{
    kill_persisted_backend, pane_count_for_row, PaneCloseOutcome, PaneSplitDirection,
};
use crate::workbench::{fs as workbench_fs, git as workbench_git, projects};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, State};

const COMMIT_MESSAGE_TIMEOUT_SECS: u64 = 180;

/// Claude Code 生成的 Workbench commit message 结构化响应。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench Commit 按钮需要从 Claude Code 获得可直接用于 git commit 的提交信息。
///
/// Code Logic（这个结构体做什么）:
///     对齐 JSON schema 的 `message` 字段，供 serde 从 Claude CLI 结构化输出反序列化。
#[derive(Debug, Clone, Deserialize)]
struct WorkbenchCommitMessageResponse {
    message: String,
}

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
///     每个工作台项目都要有一个稳定的主 worktree 记录，表示用户最初添加的项目路径。
///
/// Code Logic（这个函数做什么）:
///     用 project_id 派生确定性 id，避免重复创建主工作区记录。
fn main_worktree_id(project_id: &str) -> String {
    format!("{project_id}:main")
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 自动创建 worktree 时需要放在应用数据目录下，避免污染用户项目根目录。
///
/// Code Logic（这个函数做什么）:
///     基于 SQLite db_path 的父目录创建 worktrees/<project_id>/<branch_slug> 路径。
fn worktree_storage_path(state: &AppState, project_id: &str, branch: &str) -> PathBuf {
    let config = state.config.read().expect("config 读锁中毒");
    let db_parent = Path::new(&config.db_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    db_parent
        .join("worktrees")
        .join(project_id)
        .join(workbench_git::branch_slug(branch))
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 顶部 worktree strip 即使在非 Git 项目中也需要稳定展示主工作区。
///
/// Code Logic（这个函数做什么）:
///     确保主 worktree row 存在并与项目路径同步；Git branch 读取失败时保留 None。
async fn ensure_main_worktree(
    state: &AppState,
    project: &WorkbenchProjectRow,
) -> Result<WorkbenchWorktreeRow, AppError> {
    let id = main_worktree_id(&project.id);
    let existing = state.workbench_worktree_repo.get(&id).await?;
    let now = now_iso();
    let branch = workbench_git::current_branch(Path::new(&project.path));
    let row = WorkbenchWorktreeRow {
        id,
        project_id: project.id.clone(),
        name: branch.clone().unwrap_or_else(|| "main".to_string()),
        branch,
        base_branch: None,
        path: project.path.clone(),
        is_main: true,
        created_at: existing
            .as_ref()
            .map(|row| row.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    state.workbench_worktree_repo.upsert(&row).await?;
    Ok(row)
}

/// Business Logic（为什么需要这个函数）:
///     Git worktree 输出路径和 SQLite 持久化路径可能只有结尾分隔符不同，不能因此重复显示。
///
/// Code Logic（这个函数做什么）:
///     修剪首尾空白与结尾 `/`、`\`，返回用于比较和持久化的路径字符串。
fn normalize_worktree_path(path: &str) -> String {
    let trimmed = path.trim();
    let normalized = trimmed.trim_end_matches(['/', '\\']);
    if normalized.is_empty() {
        trimmed.to_string()
    } else {
        normalized.to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     从 Git 发现的外部 worktree 没有 cc-partner UUID，需要稳定 id 以便后续刷新覆盖同一行。
///
/// Code Logic（这个函数做什么）:
///     对 project_id 和规范化 path 做 SHA256，截取前 16 字节作为确定性 id 后缀。
fn discovered_git_worktree_id(project_id: &str, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update([0]);
    hasher.update(normalize_worktree_path(path).as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{project_id}:git:{suffix}")
}

/// Business Logic（为什么需要这个函数）:
///     顶部 worktree chip 需要优先显示分支名；detached 或无分支 worktree 也要有可读名称。
///
/// Code Logic（这个函数做什么）:
///     优先返回 parsed.branch，否则取路径末段，最后使用 `worktree` 兜底。
fn discovered_git_worktree_name(parsed: &workbench_git::ParsedWorktree) -> String {
    parsed
        .branch
        .clone()
        .or_else(|| {
            Path::new(&parsed.path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "worktree".to_string())
}

/// Business Logic（为什么需要这个函数）:
///     选择已有 Git 项目时，磁盘上已经存在的 worktree 也应出现在 Workbench 顶部切换栏。
///
/// Code Logic（这个函数做什么）:
///     将 `git worktree list --porcelain` 的非主工作区项转换为可持久化 row；若 path 已存在则复用原 row id。
fn discovered_git_worktree_row(
    project: &WorkbenchProjectRow,
    parsed: &workbench_git::ParsedWorktree,
    existing: Option<&WorkbenchWorktreeRow>,
    now: &str,
) -> WorkbenchWorktreeRow {
    let path = normalize_worktree_path(&parsed.path);
    WorkbenchWorktreeRow {
        id: existing
            .map(|row| row.id.clone())
            .unwrap_or_else(|| discovered_git_worktree_id(&project.id, &path)),
        project_id: project.id.clone(),
        name: discovered_git_worktree_name(parsed),
        branch: parsed.branch.clone(),
        base_branch: existing.and_then(|row| row.base_branch.clone()),
        path,
        is_main: false,
        created_at: existing
            .map(|row| row.created_at.clone())
            .unwrap_or_else(|| now.to_string()),
        updated_at: now.to_string(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     项目载入时应把 Git 已知 worktree 同步进工作台元数据，避免只显示主工作区。
///
/// Code Logic（这个函数做什么）:
///     调用 `git worktree list --porcelain`，对非主 worktree 按 path 复用/新增 row 并 upsert。
async fn sync_git_worktrees(
    state: &AppState,
    project: &WorkbenchProjectRow,
) -> Result<(), AppError> {
    let repo_root = match workbench_git::repo_root(Path::new(&project.path)) {
        Ok(root) => root,
        Err(error) => {
            tracing::debug!("项目不是 Git 仓库，跳过 worktree 发现: {error}");
            return Ok(());
        }
    };
    let parsed = match workbench_git::list_worktrees(Path::new(&repo_root), &repo_root) {
        Ok(items) => items,
        Err(error) => {
            tracing::debug!("读取 Git worktree 列表失败，跳过自动发现: {error}");
            return Ok(());
        }
    };
    let existing_rows = state
        .workbench_worktree_repo
        .list_by_project(&project.id)
        .await?;
    let now = now_iso();
    for item in parsed.into_iter().filter(|item| !item.is_main) {
        let item_path = normalize_worktree_path(&item.path);
        let existing = existing_rows
            .iter()
            .find(|row| normalize_worktree_path(&row.path) == item_path);
        let item = workbench_git::ParsedWorktree {
            path: item_path,
            ..item
        };
        let row = discovered_git_worktree_row(project, &item, existing, &now);
        state.workbench_worktree_repo.upsert(&row).await?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Worktree DTO 需要附带实时 Git 状态；Git 读取失败不应让整个工作台无法打开。
///
/// Code Logic（这个函数做什么）:
///     查询 `git status`，失败时返回 clean fallback 并保留 row.branch。
fn worktree_to_dto(row: &WorkbenchWorktreeRow) -> WorkbenchWorktreeDto {
    let status =
        workbench_git::status(Path::new(&row.path)).unwrap_or_else(|_| WorkbenchGitStatusDto {
            branch: row.branch.clone(),
            clean: true,
            ..WorkbenchGitStatusDto::default()
        });
    row.to_dto(status)
}

/// Business Logic（为什么需要这个函数）:
///     会话和文件树命令需要把可选 worktree_id 解析成真实磁盘根路径。
///
/// Code Logic（这个函数做什么）:
///     worktree_id 为空时返回主 worktree；非空时读取对应 row 并校验 project_id 匹配。
async fn resolve_worktree(
    state: &AppState,
    project: &WorkbenchProjectRow,
    worktree_id: Option<&str>,
) -> Result<WorkbenchWorktreeRow, AppError> {
    let Some(worktree_id) = worktree_id else {
        return ensure_main_worktree(state, project).await;
    };
    if worktree_id == main_worktree_id(&project.id) {
        return ensure_main_worktree(state, project).await;
    }
    let row = state
        .workbench_worktree_repo
        .get(worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if row.project_id != project.id {
        return Err(AppError::generic("worktree 不属于当前项目"));
    }
    Ok(row)
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
        .map(|row| row.to_dto_with_pane_count(pane_count_for_row(row)))
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
///     读取持久化会话；registry 已有则跳过；项目存在时补齐可读 worktree 名再调用 registry.restore，
///     成功后写回最新 row，项目缺失则删除孤儿会话。
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
        let worktree_name =
            match resolve_worktree(state, &project, row.worktree_id.as_deref()).await {
                Ok(worktree) => Some(worktree.name),
                Err(error) => {
                    tracing::debug!(
                        "恢复工作台终端时无法解析 worktree 名称，使用内部 id 兜底: {error}"
                    );
                    None
                }
            };
        match state.workbench_sessions.restore(
            app_handle.clone(),
            project,
            row.clone(),
            worktree_name,
        ) {
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
#[tauri::command]
pub async fn list_workbench_worktrees(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
    let project = get_project(&state, &project_id).await?;
    ensure_main_worktree(&state, &project).await?;
    sync_git_worktrees(&state, &project).await?;
    let rows = state
        .workbench_worktree_repo
        .list_by_project(&project_id)
        .await?;
    Ok(rows.iter().map(worktree_to_dto).collect())
}

/// 创建一个项目 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     用户希望在 Workbench 中直接从当前项目切出独立工作区，后续 terminal window、文件树和 Prompt 优化都绑定该路径。
///
/// Code Logic（这个函数做什么）:
///     校验 Git 仓库和分支名，生成应用数据目录下的 worktree 路径，执行 `git worktree add -b` 并持久化 row。
#[tauri::command]
pub async fn create_workbench_worktree(
    state: State<'_, AppState>,
    project_id: String,
    branch_name: String,
    base_branch: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let branch = branch_name.trim();
    if branch.is_empty() {
        return Err(AppError::generic("分支名不能为空"));
    }
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    let worktree_path = worktree_storage_path(&state, &project_id, branch);
    if worktree_path.exists() {
        return Err(AppError::generic("目标 worktree 目录已存在"));
    }
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let base = base_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    workbench_git::create_worktree(Path::new(&repo_root), &worktree_path, branch, base)?;
    let now = now_iso();
    let path = worktree_path
        .canonicalize()
        .unwrap_or(worktree_path)
        .to_string_lossy()
        .to_string();
    let row = WorkbenchWorktreeRow {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: project_id.clone(),
        name: branch.to_string(),
        branch: Some(branch.to_string()),
        base_branch: base.map(str::to_string),
        path,
        is_main: false,
        created_at: now.clone(),
        updated_at: now,
    };
    state.workbench_worktree_repo.upsert(&row).await?;
    Ok(worktree_to_dto(&row))
}

/// 提交当前 worktree 的全部改动。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要在 Workbench 中点击 Commit 后，由 Claude Code 根据项目上下文和 staged diff 生成提交信息并提交。
///
/// Code Logic（这个函数做什么）:
///     message 为空时 stage 全部改动、读取 staged diff、在 worktree cwd 下调用 Claude Code 生成 message 后提交；
///     message 非空时保留手写 message 兼容路径；无改动时返回最新 DTO，让前端刷新 stale 状态。
#[tauri::command]
pub async fn commit_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    message: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    let path = Path::new(&row.path);
    let committed = match message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(manual_message) => workbench_git::commit_all(path, manual_message)?,
        None => commit_worktree_with_generated_message(&state, path).await?,
    };
    if !committed {
        return Ok(worktree_to_dto(&row));
    }
    Ok(worktree_to_dto(&row))
}

/// Business Logic（为什么需要这个函数）:
///     Commit 按钮应自动根据当前 staged diff 生成 commit message，且让 Claude Code 读取项目上下文。
///
/// Code Logic（这个函数做什么）:
///     stage 全部改动；无改动返回 false；有改动时读取 diff，使用配置里的 Claude CLI 路径和模型，
///     在 worktree cwd 下执行项目上下文 headless JSON 调用，清洗 message 后提交 staged 内容。
async fn commit_worktree_with_generated_message(
    state: &AppState,
    path: &Path,
) -> Result<bool, AppError> {
    if !workbench_git::stage_all_for_commit(path)? {
        return Ok(false);
    }
    let changes = workbench_git::staged_changes_for_commit_message(path)?;
    let (cli_path, model) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
        )
    };
    let schema = workbench_commit_message_schema();
    let instruction = build_commit_message_instruction(&changes);
    let generated = claude_cli::run_structured_json_with_cwd::<WorkbenchCommitMessageResponse>(
        &cli_path,
        &model,
        &schema.to_string(),
        &instruction,
        Some(path),
        COMMIT_MESSAGE_TIMEOUT_SECS,
        "生成 commit message",
    )
    .await?;
    let message = workbench_git::sanitize_commit_message(&generated.message)?;
    workbench_git::commit_staged(path, &message)?;
    Ok(true)
}

/// Business Logic（为什么需要这个函数）:
///     Claude CLI 结构化输出需要固定 schema，避免自由文本或解释性内容进入 git commit。
///
/// Code Logic（这个函数做什么）:
///     返回只允许 `{message:string}` 的 JSON schema，message 是最终 git commit 文本。
fn workbench_commit_message_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["message"],
        "properties": {
            "message": {
                "type": "string",
                "minLength": 1,
                "description": "A ready-to-use git commit message. It may contain a concise subject and an optional body."
            }
        }
    })
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 需要明确知道本次 commit 的 staged diff、输出格式和提交信息风格要求。
///
/// Code Logic（这个函数做什么）:
///     把 staged stat/diff 组装为英文任务指令；diff 被截断时显式告知模型只能基于可见内容概括。
fn build_commit_message_instruction(changes: &workbench_git::StagedCommitChanges) -> String {
    let truncated_note = if changes.truncated {
        "\n注意：下面的 diff 已被截断，请基于可见内容和文件摘要生成准确但保守的 commit message。"
    } else {
        ""
    };
    format!(
        "You are generating a git commit message for the staged changes in the current Claude Code project context.\n\
         Use the repository context available from the current working directory, but base the message on the staged diff below.\n\
         Requirements:\n\
         - Return only the structured JSON object required by the schema.\n\
         - The `message` value must be ready for `git commit -m`.\n\
         - Prefer a concise Conventional Commit style subject when the change type is clear.\n\
         - Keep the first line under 72 characters when possible.\n\
         - Add a short body only if it materially clarifies a multi-part change.\n\
         - Do not wrap the message in Markdown fences, quotes, or explanations.{truncated_note}\n\n\
         Staged file summary:\n\
         ```text\n{}\n```\n\n\
         Staged diff:\n\
         ```diff\n{}\n```",
        changes.stat, changes.diff
    )
}

/// 推送当前 worktree 分支。
///
/// Business Logic（为什么需要这个函数）:
///     用户提交后需要把功能分支推送到 Git remote，以便备份或协作。
///
/// Code Logic（这个函数做什么）:
///     获取 row.branch 或当前 Git 分支，委托 workbench_git 按 upstream/origin 选择推送目标。
#[tauri::command]
pub async fn push_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
) -> Result<WorkbenchWorktreeDto, AppError> {
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    let branch = row
        .branch
        .clone()
        .or_else(|| workbench_git::current_branch(Path::new(&row.path)))
        .ok_or_else(|| AppError::generic("当前 worktree 没有可推送的分支"))?;
    workbench_git::push_branch(Path::new(&row.path), &branch)?;
    Ok(worktree_to_dto(&row))
}

/// 合并当前 worktree 到主工作区。
///
/// Business Logic（为什么需要这个函数）:
///     用户完成功能 worktree 后，需要一键回到主工作区执行 merge，但必须先避免脏工作区和冲突状态。
///
/// Code Logic（这个函数做什么）:
///     校验目标非主 worktree、源/主工作区干净且无冲突，再在主工作区执行 `git merge --no-ff <branch>`。
#[tauri::command]
pub async fn merge_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
) -> Result<serde_json::Value, AppError> {
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if row.is_main {
        return Err(AppError::generic("主工作区不需要合并到自己"));
    }
    let project = get_project(&state, &row.project_id).await?;
    let main = ensure_main_worktree(&state, &project).await?;
    let source_status = workbench_git::status(Path::new(&row.path))?;
    if !source_status.clean {
        return Err(AppError::generic("请先提交或清理当前 worktree 的改动"));
    }
    let main_status = workbench_git::status(Path::new(&main.path))?;
    if !main_status.clean {
        return Err(AppError::generic("主工作区有未提交改动，不能合并"));
    }
    let branch = row
        .branch
        .clone()
        .or(source_status.branch)
        .ok_or_else(|| AppError::generic("当前 worktree 没有可合并的分支"))?;
    workbench_git::merge_branch(Path::new(&main.path), &branch)?;
    Ok(serde_json::json!({ "ok": true, "worktreeId": worktree_id }))
}

/// 删除一个非主 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     已合并或废弃的功能 worktree 应能从 Workbench 清理，避免工作区列表膨胀。
///
/// Code Logic（这个函数做什么）:
///     阻止删除主 worktree 和仍有关联 terminal window 的 worktree；随后执行 git worktree remove 并删除元数据。
#[tauri::command]
pub async fn remove_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    force: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if row.is_main {
        return Err(AppError::generic("不能删除主工作区"));
    }
    let sessions = state
        .workbench_session_repo
        .list(Some(&row.project_id))
        .await?;
    if sessions
        .iter()
        .any(|session| session.worktree_id.as_deref() == Some(&worktree_id))
    {
        return Err(AppError::generic("请先关闭该 worktree 下的终端窗口"));
    }
    let project = get_project(&state, &row.project_id).await?;
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    workbench_git::remove_worktree(
        Path::new(&repo_root),
        Path::new(&row.path),
        force.unwrap_or(false),
    )?;
    state.workbench_worktree_repo.delete(&worktree_id).await?;
    Ok(serde_json::json!({ "ok": true, "worktreeId": worktree_id }))
}

/// 列出当前 worktree 的最近 Git 提交。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 右侧 Git 历史 tab 需要展示 active worktree 的提交历史，辅助用户确认 commit/merge 结果。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 根路径，按 limit 调用 `git log` helper；limit 默认 30，最大 100。
#[tauri::command]
pub async fn list_workbench_git_commits(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let limit = limit.unwrap_or(30).clamp(1, 100);
    workbench_git::list_commits(Path::new(&worktree.path), limit)
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
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
) -> Result<WorkbenchSessionDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let row = state.workbench_sessions.create(
        app_handle,
        project,
        worktree.path.clone(),
        Some(worktree.id.clone()),
        Some(worktree.name.clone()),
        initial_cols,
        initial_rows,
    )?;
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

/// 获取当前 worktree 聚焦的工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在 tmux 底部 status bar 或快捷键中切换 window，顶部 app tab 需要跟随真实 tmux current window。
///
/// Code Logic（这个函数做什么）:
///     校验项目存在，读取当前 worktree tmux session 当前 window id，并映射成 Workbench sessionId 返回。
#[tauri::command]
pub async fn get_focused_workbench_session(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<serde_json::Value, AppError> {
    let _ = get_project(&state, &project_id).await?;
    let session_id = state
        .workbench_sessions
        .focused_session_id(&project_id, worktree_id.as_deref())?;
    Ok(serde_json::json!({ "sessionId": session_id }))
}

/// 分割当前 tmux window 的 pane。
///
/// Business Logic（为什么需要这个函数）:
///     工作台采用真实 tmux 映射后，用户需要在当前 window 内创建左右或上下 pane。
///
/// Code Logic（这个函数做什么）:
///     校验 direction 字符串，读取会话 row，调用 registry 按 row.cwd 执行带 cwd 的 tmux split-window。
#[tauri::command]
pub async fn split_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
    direction: String,
) -> Result<serde_json::Value, AppError> {
    let split_direction = PaneSplitDirection::from_api(&direction)?;
    let _row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    state
        .workbench_sessions
        .split_pane(&session_id, split_direction)?;
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
    worktree_id: Option<String>,
    path: Option<String>,
) -> Result<Vec<WorkbenchFileNode>, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
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
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
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
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
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
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
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
    worktree_id: Option<String>,
    path: String,
    new_name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
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
    worktree_id: Option<String>,
    path: String,
) -> Result<serde_json::Value, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let deleted_path = path.clone();
    run_blocking_fs(move || workbench_fs::delete_path(&root, &path)).await?;
    Ok(serde_json::json!({ "ok": true, "path": deleted_path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench AI commit 必须让 Claude 基于 staged diff 生成提交信息，而不是泛泛猜测。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 staged changes，断言生成指令包含 stat、diff、截断提示和只返回 commit message 的约束。
    #[test]
    fn commit_message_instruction_contains_staged_diff_and_output_contract() {
        let changes = workbench_git::StagedCommitChanges {
            stat: "README.md | 1 +".to_string(),
            diff: "+hello".to_string(),
            truncated: true,
        };

        let instruction = build_commit_message_instruction(&changes);

        assert!(instruction.contains("README.md | 1 +"));
        assert!(instruction.contains("+hello"));
        assert!(instruction.contains("diff 已被截断"));
        assert!(instruction.contains("Return only"));
        assert!(instruction.contains("message"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude CLI 结构化输出必须稳定落到单个 commit message 字段，避免前端解析自由文本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     读取 schema JSON，断言 required 包含 message 且 message 类型为 string。
    #[test]
    fn commit_message_schema_requires_message_string() {
        let schema = workbench_commit_message_schema();

        assert_eq!(schema["required"][0], "message");
        assert_eq!(schema["properties"]["message"]["type"], "string");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户选择已有 Git 项目后，Workbench 顶部必须自动显示磁盘上已有的 Git worktree。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 `git worktree list` 解析项，断言导入 row 使用稳定 id、分支名和路径。
    #[test]
    fn discovered_git_worktree_row_uses_stable_metadata() {
        let project = WorkbenchProjectRow {
            id: "project-1".to_string(),
            name: "Repo".to_string(),
            kind: "local".to_string(),
            device_id: "local".to_string(),
            device_name: "Mac".to_string(),
            path: "/repo/main".to_string(),
            last_opened_at: "2026-06-26T00:00:00Z".to_string(),
            created_at: "2026-06-26T00:00:00Z".to_string(),
            updated_at: "2026-06-26T00:00:00Z".to_string(),
        };
        let parsed = workbench_git::ParsedWorktree {
            path: "/repo/worktrees/feature-a".to_string(),
            branch: Some("feature/a".to_string()),
            is_main: false,
        };

        let first = discovered_git_worktree_row(&project, &parsed, None, "2026-06-26T01:00:00Z");
        let second = discovered_git_worktree_row(&project, &parsed, None, "2026-06-26T02:00:00Z");

        assert_eq!(first.id, second.id);
        assert_eq!(first.project_id, "project-1");
        assert_eq!(first.name, "feature/a");
        assert_eq!(first.branch.as_deref(), Some("feature/a"));
        assert_eq!(first.path, "/repo/worktrees/feature-a");
        assert!(!first.is_main);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     已经由 cc-partner 创建过的 worktree 再次被 Git 发现时不能换 id，否则会重复显示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造相同 path 的既有 row，断言导入时复用既有 id 和 created_at。
    #[test]
    fn discovered_git_worktree_row_reuses_existing_row_for_same_path() {
        let project = WorkbenchProjectRow {
            id: "project-1".to_string(),
            name: "Repo".to_string(),
            kind: "local".to_string(),
            device_id: "local".to_string(),
            device_name: "Mac".to_string(),
            path: "/repo/main".to_string(),
            last_opened_at: "2026-06-26T00:00:00Z".to_string(),
            created_at: "2026-06-26T00:00:00Z".to_string(),
            updated_at: "2026-06-26T00:00:00Z".to_string(),
        };
        let existing = WorkbenchWorktreeRow {
            id: "existing-row".to_string(),
            project_id: "project-1".to_string(),
            name: "old name".to_string(),
            branch: Some("old".to_string()),
            base_branch: Some("main".to_string()),
            path: "/repo/worktrees/feature-a".to_string(),
            is_main: false,
            created_at: "2026-06-25T00:00:00Z".to_string(),
            updated_at: "2026-06-25T00:00:00Z".to_string(),
        };
        let parsed = workbench_git::ParsedWorktree {
            path: "/repo/worktrees/feature-a/".to_string(),
            branch: Some("feature/a".to_string()),
            is_main: false,
        };

        let row =
            discovered_git_worktree_row(&project, &parsed, Some(&existing), "2026-06-26T01:00:00Z");

        assert_eq!(row.id, "existing-row");
        assert_eq!(row.created_at, "2026-06-25T00:00:00Z");
        assert_eq!(row.updated_at, "2026-06-26T01:00:00Z");
        assert_eq!(row.name, "feature/a");
        assert_eq!(row.branch.as_deref(), Some("feature/a"));
        assert_eq!(row.path, "/repo/worktrees/feature-a");
    }
}
