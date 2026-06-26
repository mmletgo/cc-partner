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
    WorkbenchDetectedFileType, WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchGitStatusDto,
    WorkbenchHtmlAssetDto, WorkbenchOpenFileDto, WorkbenchPathInfo, WorkbenchProjectDto,
    WorkbenchProjectRow, WorkbenchSaveTextResultDto, WorkbenchSessionDto, WorkbenchSqlitePreview,
    WorkbenchTextContent, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use crate::workbench::sessions::{
    kill_persisted_backend, pane_count_for_row, PaneCloseOutcome, PaneSplitDirection,
};
use crate::workbench::{
    file_content, file_preview, fs as workbench_fs, git as workbench_git, html_assets, projects,
    sqlite_preview,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Component;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, State};

const COMMIT_MESSAGE_TIMEOUT_SECS: u64 = 180;
const MERGE_CONFLICT_RESOLUTION_TIMEOUT_SECS: u64 = 300;
const MERGE_STAGE_CHECK_SOURCE: &str = "checkSource";
const MERGE_STAGE_CLOSE_SESSIONS: &str = "closeSessions";
const MERGE_STAGE_MERGE_MAIN: &str = "mergeMain";
const MERGE_STAGE_RESOLVE_CONFLICTS: &str = "resolveConflicts";
const MERGE_STAGE_CLEANUP: &str = "cleanup";
const MERGE_STAGE_IDS: [&str; 5] = [
    MERGE_STAGE_CHECK_SOURCE,
    MERGE_STAGE_CLOSE_SESSIONS,
    MERGE_STAGE_MERGE_MAIN,
    MERGE_STAGE_RESOLVE_CONFLICTS,
    MERGE_STAGE_CLEANUP,
];

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

/// Workbench merge 命令返回 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要展示一键 merge 每个阶段的最终状态，而不只是一个布尔成功值。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{ok, worktreeId, stages}`，stages 内含固定 stage id/status/message。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchMergeResultDto {
    ok: bool,
    worktree_id: String,
    stages: Vec<WorkbenchMergeStageDto>,
}

/// Workbench merge 阶段 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端进度条需要知道当前阶段是等待、运行、完成、失败还是跳过。
///
/// Code Logic（这个结构体做什么）:
///     保存 stage id、status 和用户可读 message，字段名与前端约定保持一致。
#[derive(Debug, Clone, Serialize)]
pub struct WorkbenchMergeStageDto {
    id: String,
    status: String,
    message: String,
}

/// Workbench merge 进度事件 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     merge 是多阶段长操作，前端需要通过事件实时更新，而不是只等待命令返回。
///
/// Code Logic（这个结构体做什么）:
///     序列化 `{projectId, worktreeId, stage}` 并通过 `workbench:merge-progress` emit，
///     让多项目窗口只接收自己项目的进度。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkbenchMergeProgressEvent {
    project_id: String,
    worktree_id: String,
    stage: WorkbenchMergeStageDto,
}

/// Claude Code merge 冲突解决响应。
///
/// Business Logic（为什么需要这个结构体）:
///     自动冲突解决需要 Claude 返回每个冲突文件的完整解决后内容，后端才能安全写回。
///
/// Code Logic（这个结构体做什么）:
///     对齐 JSON schema 顶层 `files` 数组。
#[derive(Debug, Clone, Deserialize)]
struct WorkbenchMergeResolutionResponse {
    files: Vec<WorkbenchMergeResolvedFile>,
}

/// Claude Code 返回的单个已解决文件。
///
/// Business Logic（为什么需要这个结构体）:
///     每个冲突文件都需要独立校验相对路径和内容，防止模型输出越界路径或残留冲突标记。
///
/// Code Logic（这个结构体做什么）:
///     保存相对 path 与完整文件 content。
#[derive(Debug, Clone, Deserialize)]
struct WorkbenchMergeResolvedFile {
    path: String,
    content: String,
}

/// Workbench 结构化内容格式化结果 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端编辑 JSON/TOML 时需要后端返回权威格式化文本，用同一套解析器保证保存前校验一致。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{formatted}`，承载格式化后的 UTF-8 文本。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchFormatResult {
    formatted: String,
}

/// 传给 Claude Code 的单个冲突文件输入。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude 解决冲突时必须看到 Git 相对路径和带 conflict marker 的当前文件全文。
///
/// Code Logic（这个结构体做什么）:
///     在构造 prompt 前保存 path/content，便于测试 prompt 内容。
#[derive(Debug, Clone)]
struct MergeConflictFileInput {
    path: String,
    content: String,
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

/// Business Logic（为什么需要这个函数）:
///     打开、保存和 SQLite 预览都必须先确认目标是当前 worktree 根内的既有文件，不能读取目录或越界路径。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中先读取 path_info，再用 resolve_project_path 取得 canonical 文件路径；
///     非 file 类型返回业务错误，成功时返回 metadata 与安全绝对路径。
async fn resolve_workbench_file_path(
    root: PathBuf,
    path: String,
) -> Result<(WorkbenchPathInfo, PathBuf), AppError> {
    run_blocking_fs(move || {
        let metadata = workbench_fs::path_info(&root, &path)?;
        if metadata.kind != "file" {
            return Err(AppError::generic(
                "只能打开项目内文件，不能把目录作为文件处理",
            ));
        }
        let file_path = projects::resolve_project_path(&root, &path)?;
        Ok((metadata, file_path))
    })
    .await
}

/// Business Logic（为什么需要这个函数）:
///     保存文件时不能信任前端声明的文件类型，必须以后端检测出的真实文件名为准，避免只读文件被伪装成文本覆盖。
///
/// Code Logic（这个函数做什么）:
///     按文件名检测类型；只允许 Markdown/HTML/Code/Text/Json/Toml/Yaml，且结构化文件会先解析校验但不改变用户内容。
fn validate_save_file_type(
    metadata_name: &str,
    content: &str,
) -> Result<WorkbenchDetectedFileType, AppError> {
    let detected_type = file_preview::detect_file_type(metadata_name);
    match detected_type {
        WorkbenchDetectedFileType::Json => {
            file_content::format_structured_content("json", content)?;
        }
        WorkbenchDetectedFileType::Toml => {
            file_content::format_structured_content("toml", content)?;
        }
        WorkbenchDetectedFileType::Yaml => {
            file_content::format_structured_content("yaml", content)?;
        }
        WorkbenchDetectedFileType::Markdown
        | WorkbenchDetectedFileType::Html
        | WorkbenchDetectedFileType::Code
        | WorkbenchDetectedFileType::Text => {}
        WorkbenchDetectedFileType::Image
        | WorkbenchDetectedFileType::Csv
        | WorkbenchDetectedFileType::Sqlite
        | WorkbenchDetectedFileType::Binary
        | WorkbenchDetectedFileType::Unsupported => {
            return Err(AppError::generic("此文件类型不支持文本保存"));
        }
    }
    Ok(detected_type)
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
///     用户完成功能 worktree 后，需要一键合并回主工作区；后端应自动处理源工作区检查、终端关闭、
///     主工作区 merge、Claude Code 冲突解决和 worktree 清理，并持续给前端阶段进度。
///
/// Code Logic（这个函数做什么）:
///     按 checkSource/closeSessions/mergeMain/resolveConflicts/cleanup 五阶段推进；每阶段开始/完成/失败
///     emit `workbench:merge-progress`，成功返回 `{ok, worktreeId, stages}`，失败先 emit failed 再返回 AppError。
#[tauri::command]
pub async fn merge_workbench_worktree(
    app: AppHandle,
    state: State<'_, AppState>,
    worktree_id: String,
) -> Result<WorkbenchMergeResultDto, AppError> {
    let mut stages = initial_merge_stages();

    let row = match state.workbench_worktree_repo.get(&worktree_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(AppError::not_found("工作台 worktree 不存在")),
        Err(error) => return Err(error),
    };
    let project_id = row.project_id.clone();
    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "running",
        "正在检查源 worktree 状态",
    );
    if row.is_main {
        return Err(fail_merge_stage(
            &app,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CHECK_SOURCE,
            AppError::generic("主工作区不需要合并到自己"),
        ));
    }
    let project = stage_result(
        get_project(&state, &row.project_id).await,
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    let main = stage_result(
        ensure_main_worktree(&state, &project).await,
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    let source_status = stage_result(
        workbench_git::status(Path::new(&row.path)),
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    if !source_status.clean {
        return Err(fail_merge_stage(
            &app,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CHECK_SOURCE,
            AppError::generic("源 worktree 有未提交改动，请先提交或清理后再合并"),
        ));
    }
    let branch = stage_result(
        row.branch
            .clone()
            .or(source_status.branch)
            .ok_or_else(|| AppError::generic("当前 worktree 没有可合并的分支")),
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "completed",
        "源 worktree 已确认干净",
    );

    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
        "running",
        "正在关闭该 worktree 下的终端窗口",
    );
    let closed_sessions = stage_result(
        close_sessions_for_worktree(&state, &row.project_id, &row.id).await,
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
    )?;
    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
        "completed",
        format!("已关闭 {closed_sessions} 个终端窗口"),
    );

    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
        "running",
        "正在主工作区执行 git merge --no-ff",
    );
    let main_path = Path::new(&main.path);
    let main_status = stage_result(
        workbench_git::status(main_path),
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    if !main_status.clean {
        return Err(fail_merge_stage(
            &app,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            AppError::generic("主工作区有未提交改动，请先提交或清理后再合并"),
        ));
    }
    let merge_outcome = stage_result(
        workbench_git::merge_branch(main_path, &branch),
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    match merge_outcome {
        workbench_git::MergeBranchOutcome::Merged => {
            set_merge_stage(
                &app,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "主工作区 merge 已完成",
            );
            set_merge_stage(
                &app,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_RESOLVE_CONFLICTS,
                "skipped",
                "merge 未产生冲突，跳过自动冲突解决",
            );
        }
        workbench_git::MergeBranchOutcome::Conflicted => {
            set_merge_stage(
                &app,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "merge 出现冲突，进入自动解决阶段",
            );
            set_merge_stage(
                &app,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_RESOLVE_CONFLICTS,
                "running",
                "正在调用 Claude Code 尝试解决 merge 冲突",
            );
            if let Err(error) = resolve_merge_conflicts_with_claude(&state, main_path).await {
                let message = abort_merge_after_failed_resolution(main_path, &error);
                return Err(fail_merge_stage(
                    &app,
                    &project_id,
                    &worktree_id,
                    &mut stages,
                    MERGE_STAGE_RESOLVE_CONFLICTS,
                    AppError::generic(message),
                ));
            }
            set_merge_stage(
                &app,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_RESOLVE_CONFLICTS,
                "completed",
                "Claude Code 已解决冲突并完成 merge commit",
            );
        }
    }

    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "running",
        "正在删除 worktree 元数据、磁盘工作区和已合并分支",
    );
    stage_result(
        cleanup_merged_worktree(&state, &project, &row).await,
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
    )?;
    set_merge_stage(
        &app,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "completed",
        "已删除 worktree 元数据、磁盘工作区和已合并分支",
    );

    Ok(WorkbenchMergeResultDto {
        ok: true,
        worktree_id,
        stages,
    })
}

/// Business Logic（为什么需要这个函数）:
///     merge 命令和进度事件都需要同一份固定阶段列表，避免前端收到未知或缺失阶段。
///
/// Code Logic（这个函数做什么）:
///     按前端约定的五个 stage id 生成 pending 初始状态。
fn initial_merge_stages() -> Vec<WorkbenchMergeStageDto> {
    MERGE_STAGE_IDS
        .iter()
        .map(|id| WorkbenchMergeStageDto {
            id: (*id).to_string(),
            status: "pending".to_string(),
            message: "等待执行".to_string(),
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     前端需要实时看到 merge 阶段开始、完成、跳过和失败状态，不能只等命令返回。
///
/// Code Logic（这个函数做什么）:
///     更新本地 stages 中对应项，并 emit `workbench:merge-progress` 事件；emit 失败只记录日志，不中断 merge。
fn set_merge_stage(
    app: &AppHandle,
    project_id: &str,
    worktree_id: &str,
    stages: &mut [WorkbenchMergeStageDto],
    stage_id: &str,
    status: &str,
    message: impl Into<String>,
) {
    let message = message.into();
    let stage = stages
        .iter_mut()
        .find(|stage| stage.id == stage_id)
        .expect("merge stage id 必须来自固定列表");
    stage.status = status.to_string();
    stage.message = message;
    let event = WorkbenchMergeProgressEvent {
        project_id: project_id.to_string(),
        worktree_id: worktree_id.to_string(),
        stage: stage.clone(),
    };
    if let Err(error) = app.emit("workbench:merge-progress", event) {
        tracing::warn!("发送 Workbench merge 进度事件失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     merge 阶段内部任一错误都应先通知前端 failed stage，再通过 Tauri command 返回 AppError。
///
/// Code Logic（这个函数做什么）:
///     将 Result::Err 映射为 fail_merge_stage，Result::Ok 原样返回。
fn stage_result<T>(
    result: Result<T, AppError>,
    app: &AppHandle,
    project_id: &str,
    worktree_id: &str,
    stages: &mut [WorkbenchMergeStageDto],
    stage_id: &str,
) -> Result<T, AppError> {
    result.map_err(|error| fail_merge_stage(app, project_id, worktree_id, stages, stage_id, error))
}

/// Business Logic（为什么需要这个函数）:
///     失败路径需要统一把真实错误消息同步到进度事件，前端才能在对应阶段展示可读失败原因。
///
/// Code Logic（这个函数做什么）:
///     把 stage 标记为 failed 并返回原 AppError，保持命令错误语义不变。
fn fail_merge_stage(
    app: &AppHandle,
    project_id: &str,
    worktree_id: &str,
    stages: &mut [WorkbenchMergeStageDto],
    stage_id: &str,
    error: AppError,
) -> AppError {
    let message = error.to_string();
    set_merge_stage(
        app,
        project_id,
        worktree_id,
        stages,
        stage_id,
        "failed",
        message,
    );
    error
}

/// Business Logic（为什么需要这个函数）:
///     merge 源 worktree 前，后端要自动关闭该 worktree 下所有 terminal window/pane，
///     用户不应再被要求手动关闭。
///
/// Code Logic（这个函数做什么）:
///     读取该 worktree 的持久化 session row；优先关闭运行期 registry 句柄，再销毁 tmux/window 后端，
///     最后删除 SQLite row。registry 缺失但 row 存在时仍清理持久后端。
async fn close_sessions_for_worktree(
    state: &AppState,
    project_id: &str,
    worktree_id: &str,
) -> Result<usize, AppError> {
    let sessions = state
        .workbench_session_repo
        .list_by_worktree(project_id, worktree_id)
        .await?;
    let mut closed = 0_usize;
    for row in sessions {
        match state.workbench_sessions.close(&row.id) {
            Ok(closed_row) => {
                kill_persisted_backend(&closed_row);
            }
            Err(AppError::NotFound(_)) => {
                kill_persisted_backend(&row);
            }
            Err(error) => return Err(error),
        }
        state.workbench_session_repo.delete(&row.id).await?;
        closed += 1;
    }
    Ok(closed)
}

/// Business Logic（为什么需要这个函数）:
///     merge 成功后，已合并 worktree 不应继续占用 terminal metadata、SQLite worktree row 或磁盘 worktree。
///
/// Code Logic（这个函数做什么）:
///     再次删除该 worktree 下残留 session row，执行 `git worktree remove`，最后删除 worktree 元数据。
async fn cleanup_merged_worktree(
    state: &AppState,
    project: &WorkbenchProjectRow,
    row: &WorkbenchWorktreeRow,
) -> Result<(), AppError> {
    state
        .workbench_session_repo
        .delete_by_worktree(&row.project_id, &row.id)
        .await?;
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    workbench_git::remove_worktree(Path::new(&repo_root), Path::new(&row.path), false)?;
    if let Some(branch) = row.branch.as_deref() {
        workbench_git::delete_local_branch_if_merged(Path::new(&repo_root), branch, "HEAD")?;
    }
    state.workbench_worktree_repo.delete(&row.id).await?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     merge 冲突时，后端需要调用本机 Claude Code CLI 在主 worktree 项目上下文下尝试生成解决结果。
///
/// Code Logic（这个函数做什么）:
///     读取 Git 未解决冲突文件，调用结构化 Claude CLI，校验并写回结果，确认无 conflict marker 后 stage all，
///     最后使用 Git 默认 merge message 完成 merge commit。
async fn resolve_merge_conflicts_with_claude(
    state: &AppState,
    main_path: &Path,
) -> Result<usize, AppError> {
    let conflict_paths = workbench_git::unresolved_conflict_files(main_path)?;
    if conflict_paths.is_empty() {
        return Ok(0);
    }
    let conflict_inputs = read_merge_conflict_files(main_path, &conflict_paths)?;
    let (cli_path, model) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
        )
    };
    let schema = merge_conflict_resolution_schema();
    let instruction = build_merge_conflict_resolution_instruction(&conflict_inputs);
    let response = claude_cli::run_structured_json_with_cwd::<WorkbenchMergeResolutionResponse>(
        &cli_path,
        &model,
        &schema.to_string(),
        &instruction,
        Some(main_path),
        MERGE_CONFLICT_RESOLUTION_TIMEOUT_SECS,
        "解决 merge 冲突",
    )
    .await?;
    apply_merge_resolution_files(main_path, &conflict_paths, response.files)?;
    ensure_conflict_markers_removed(main_path, &conflict_paths)?;
    workbench_git::stage_all_merge_resolution(main_path)?;
    let remaining = workbench_git::unresolved_conflict_files(main_path)?;
    if !remaining.is_empty() {
        return Err(AppError::generic(format!(
            "Claude Code 处理后仍有未解决冲突: {}",
            remaining.join(", ")
        )));
    }
    workbench_git::commit_merge_no_edit(main_path)?;
    Ok(conflict_inputs.len())
}

/// Business Logic（为什么需要这个函数）:
///     自动解决冲突失败后，主工作区应尽量回到 merge 前状态，避免留下半合并工作区。
///
/// Code Logic（这个函数做什么）:
///     尝试执行 `git merge --abort`，返回包含原始错误和 abort 结果的用户可读消息。
fn abort_merge_after_failed_resolution(main_path: &Path, error: &AppError) -> String {
    let original = error.to_string();
    match workbench_git::abort_merge(main_path) {
        Ok(()) => format!("{original}；已尝试执行 git merge --abort 回滚主工作区"),
        Err(abort_error) => format!(
            "{original}；同时执行 git merge --abort 失败，请手动检查主工作区: {abort_error}"
        ),
    }
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 解决冲突前必须看到当前冲突文件全文，尤其是 Git conflict marker 两侧内容。
///
/// Code Logic（这个函数做什么）:
///     校验 Git 相对路径安全后读取 UTF-8 文本；非文本或读取失败返回可读错误。
fn read_merge_conflict_files(
    root: &Path,
    paths: &[String],
) -> Result<Vec<MergeConflictFileInput>, AppError> {
    paths
        .iter()
        .map(|path| {
            validate_merge_resolution_path(path)?;
            let full_path = safe_merge_resolution_path(root, path)?;
            let content = std::fs::read_to_string(&full_path).map_err(|error| {
                AppError::generic(format!(
                    "读取冲突文件 {} 失败（仅支持 UTF-8 文本冲突自动解决）: {error}",
                    path
                ))
            })?;
            Ok(MergeConflictFileInput {
                path: path.clone(),
                content,
            })
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     Claude 输出是模型生成内容，后端写回前必须确认路径属于本次冲突文件，且内容不含残留冲突标记。
///
/// Code Logic（这个函数做什么）:
///     建立允许 path 集合；逐个校验 path/content 后写入主 worktree 文件，并要求所有冲突文件都有返回。
fn apply_merge_resolution_files(
    root: &Path,
    conflict_paths: &[String],
    files: Vec<WorkbenchMergeResolvedFile>,
) -> Result<(), AppError> {
    let allowed = conflict_paths.iter().cloned().collect::<HashSet<_>>();
    let mut applied = HashSet::new();
    for file in files {
        validate_merge_resolution_path(&file.path)?;
        if !allowed.contains(&file.path) {
            return Err(AppError::generic(format!(
                "Claude Code 返回了非本次冲突文件路径: {}",
                file.path
            )));
        }
        if content_has_conflict_markers(&file.content) {
            return Err(AppError::generic(format!(
                "Claude Code 返回的 {} 仍包含 merge 冲突标记",
                file.path
            )));
        }
        let full_path = safe_merge_resolution_path(root, &file.path)?;
        std::fs::write(full_path, file.content)?;
        applied.insert(file.path);
    }
    let missing = conflict_paths
        .iter()
        .filter(|path| !applied.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(AppError::generic(format!(
            "Claude Code 未返回以下冲突文件的解决内容: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     即使 Claude 返回了所有文件，后端也要在 git add 前复查磁盘内容，避免把 conflict marker 提交进仓库。
///
/// Code Logic（这个函数做什么）:
///     逐个读取原冲突文件；存在文本内容且含 marker 时返回错误，文件已被删除则交给 git add -A 处理。
fn ensure_conflict_markers_removed(root: &Path, paths: &[String]) -> Result<(), AppError> {
    for path in paths {
        let full_path = safe_merge_resolution_path(root, path)?;
        if !full_path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&full_path)
            .map_err(|error| AppError::generic(format!("复查冲突文件 {} 失败: {error}", path)))?;
        if content_has_conflict_markers(&content) {
            return Err(AppError::generic(format!("{} 仍包含 merge 冲突标记", path)));
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Claude CLI 结构化输出需要固定契约，确保后端拿到可写回的文件路径和完整内容。
///
/// Code Logic（这个函数做什么）:
///     返回只允许 `{files:[{path,content}]}` 的 JSON schema。
fn merge_conflict_resolution_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["files"],
        "properties": {
            "files": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "content"],
                    "properties": {
                        "path": {
                            "type": "string",
                            "minLength": 1,
                            "description": "Repository-relative path for one conflicted file."
                        },
                        "content": {
                            "type": "string",
                            "description": "The complete resolved file content with all conflict markers removed."
                        }
                    }
                }
            }
        }
    })
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 需要明确知道这是在当前项目上下文中解决 Git merge 冲突，并且只能返回结构化文件内容。
///
/// Code Logic（这个函数做什么）:
///     把每个冲突文件 path/content 组装进英文任务指令，要求返回完整内容且不得保留 conflict marker。
fn build_merge_conflict_resolution_instruction(files: &[MergeConflictFileInput]) -> String {
    let mut sections = String::new();
    for file in files {
        sections.push_str(&format!(
            "\nFile: {}\n```text\n{}\n```\n",
            file.path, file.content
        ));
    }
    format!(
        "You are resolving Git merge conflicts in the current Claude Code project context.\n\
         Use the repository instructions and code context available from the current working directory.\n\
         Requirements:\n\
         - Return only the structured JSON object required by the schema.\n\
         - The `files` array must include every conflicted file listed below.\n\
         - Each `content` value must be the complete final file content, not a patch.\n\
         - Do not leave conflict markers such as <<<<<<<, |||||||, =======, or >>>>>>>.\n\
         - Preserve user intent from both sides when possible; when unsure, make the smallest coherent resolution.\n\
         - Do not include Markdown fences, explanations, or extra properties in JSON.\n\n\
         Conflicted files:\n{sections}"
    )
}

/// Business Logic（为什么需要这个函数）:
///     Claude 输出的路径不能被直接信任，否则可能越过主 worktree 根目录覆盖任意文件。
///
/// Code Logic（这个函数做什么）:
///     拒绝空路径、绝对路径、Windows prefix/root 和 `..`；普通相对路径返回 Ok。
fn validate_merge_resolution_path(path: &str) -> Result<(), AppError> {
    if path.trim().is_empty() {
        return Err(AppError::generic("冲突文件路径不能为空"));
    }
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err(AppError::generic("冲突文件路径不能是绝对路径"));
    }
    let mut has_normal = false;
    for component in relative.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::generic("冲突文件路径不能越过工作区根目录"));
            }
        }
    }
    if !has_normal {
        return Err(AppError::generic("冲突文件路径不能为空"));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 自动写回冲突文件时，不能通过 symlink 父目录或 symlink 文件越过 worktree 根目录。
///
/// Code Logic（这个函数做什么）:
///     先做相对路径语法校验，再 canonicalize root 和父目录，要求父目录仍在 root 内；
///     若目标已存在且是 symlink，则拒绝自动写回。
fn safe_merge_resolution_path(root: &Path, path: &str) -> Result<PathBuf, AppError> {
    validate_merge_resolution_path(path)?;
    let root = root
        .canonicalize()
        .map_err(|error| AppError::generic(format!("解析主工作区路径失败: {error}")))?;
    let full_path = root.join(path);
    let parent = full_path
        .parent()
        .ok_or_else(|| AppError::generic("冲突文件路径缺少父目录"))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| AppError::generic(format!("解析冲突文件父目录失败: {error}")))?;
    if !parent.starts_with(&root) {
        return Err(AppError::generic("冲突文件路径不能越过工作区根目录"));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(&full_path) {
        if metadata.file_type().is_symlink() {
            return Err(AppError::generic(format!(
                "冲突文件路径不能是符号链接: {}",
                path
            )));
        }
    }
    Ok(full_path)
}

/// Business Logic（为什么需要这个函数）:
///     Git 允许用户把仍含 conflict marker 的文件 `git add`，自动流程必须主动阻止这类错误提交。
///
/// Code Logic（这个函数做什么）:
///     按行识别常见 Git conflict marker：`<<<<<<<`、`|||||||`、单独 `=======`、`>>>>>>>`。
fn content_has_conflict_markers(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_end();
        trimmed.starts_with("<<<<<<<")
            || trimmed.starts_with("|||||||")
            || trimmed == "======="
            || trimmed.starts_with(">>>>>>>")
    })
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

/// 打开当前 worktree 内的文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区需要一次拿到文件 metadata、类型能力和可用的内容/预览数据，供前端打开 tab。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 和安全文件路径，按后端检测类型分发到文本（含 Markdown/HTML）、图片、CSV 或 SQLite 预览；
///     内容超限、非 UTF-8 或预览失败时返回 notice，不让一次预览失败阻断文件 tab 打开。
#[tauri::command]
pub async fn open_workbench_file(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchOpenFileDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let (metadata, file_path) = resolve_workbench_file_path(root, path).await?;
    let detected_type = file_preview::detect_file_type(&metadata.name);
    let capabilities = file_preview::capabilities_for_type(&detected_type);
    let mut response = WorkbenchOpenFileDto {
        metadata,
        detected_type: detected_type.clone(),
        capabilities,
        text: None,
        image: None,
        csv: None,
        sqlite: None,
        truncated: false,
        notice: None,
    };

    match detected_type {
        WorkbenchDetectedFileType::Markdown
        | WorkbenchDetectedFileType::Html
        | WorkbenchDetectedFileType::Code
        | WorkbenchDetectedFileType::Json
        | WorkbenchDetectedFileType::Toml
        | WorkbenchDetectedFileType::Yaml
        | WorkbenchDetectedFileType::Text => {
            let base_modified_at = response.metadata.modified_at.clone();
            let read_path = file_path.clone();
            match run_blocking_fs(move || file_content::read_text_file(&read_path)).await {
                Ok((content, base_hash)) => {
                    response.text = Some(WorkbenchTextContent {
                        content,
                        base_hash,
                        base_modified_at,
                    });
                }
                Err(error) => {
                    response.notice = Some(error.to_string());
                }
            }
        }
        WorkbenchDetectedFileType::Image => {
            let preview_path = file_path.clone();
            match run_blocking_fs(move || file_preview::preview_image_file(&preview_path)).await {
                Ok(image) => {
                    response.image = Some(image);
                }
                Err(error) => {
                    response.notice = Some(error.to_string());
                }
            }
        }
        WorkbenchDetectedFileType::Csv => {
            let preview_path = file_path.clone();
            match run_blocking_fs(move || file_preview::preview_csv_file(&preview_path, 100)).await
            {
                Ok(csv) => {
                    response.truncated = csv.truncated;
                    response.csv = Some(csv);
                }
                Err(error) => {
                    response.notice = Some(error.to_string());
                }
            }
        }
        WorkbenchDetectedFileType::Sqlite => {
            match sqlite_preview::preview_sqlite_file(&file_path, None, 100).await {
                Ok(sqlite) => {
                    response.truncated = sqlite.truncated;
                    response.sqlite = Some(sqlite);
                }
                Err(error) => {
                    response.notice = Some(error.to_string());
                }
            }
        }
        WorkbenchDetectedFileType::Binary | WorkbenchDetectedFileType::Unsupported => {
            response.notice = Some("此文件类型暂不支持 Workbench 预览".to_string());
        }
    }

    Ok(response)
}

/// 保存当前 worktree 内的文本文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区编辑器需要安全保存 Markdown、代码、文本和结构化配置，同时防止覆盖外部修改。
///
/// Code Logic（这个函数做什么）:
///     先解析安全文件路径并以后端 metadata.name 重新检测真实类型；JSON/TOML 做语义校验但不强制格式化；
///     随后调用原子保存 helper，并返回最新 metadata 与 hash 基线。
#[tauri::command]
pub async fn save_workbench_text_file(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    content: String,
    base_hash: String,
) -> Result<WorkbenchSaveTextResultDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
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

/// 格式化 JSON 或 TOML 内容。
///
/// Business Logic（为什么需要这个函数）:
///     前端编辑结构化配置时应复用后端保存前校验的同一套解析器，避免前后端格式化结果不一致。
///
/// Code Logic（这个函数做什么）:
///     根据 kind 调用 file_content::format_structured_content，并把格式化文本包装为 `{formatted}`。
#[tauri::command]
pub async fn format_workbench_structured_content(
    kind: String,
    content: String,
) -> Result<WorkbenchFormatResult, AppError> {
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
#[tauri::command]
pub async fn preview_workbench_sqlite(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    table: Option<String>,
    limit_rows: Option<i64>,
) -> Result<WorkbenchSqlitePreview, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let (_, file_path) = resolve_workbench_file_path(root, path).await?;
    sqlite_preview::preview_sqlite_file(&file_path, table, limit_rows.unwrap_or(100)).await
}

/// 读取 HTML 预览所需的项目内相对资源。
///
/// Business Logic（为什么需要这个函数）:
///     sandbox iframe 的 srcDoc 无法直接访问 worktree 文件，HTML 预览需要后端把当前文档引用的相对资源安全内联。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 根、HTML 文档路径和资源相对路径，在 blocking pool 中只读读取根内文件并返回 data URL。
#[tauri::command]
pub async fn preview_workbench_html_asset(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    document_path: String,
    asset_path: String,
) -> Result<WorkbenchHtmlAssetDto, AppError> {
    let project = get_project(&state, &project_id).await?;
    let worktree = resolve_worktree(&state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || html_assets::preview_html_asset(&root, &document_path, &asset_path))
        .await
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
    ///     保存命令必须按后端真实文件名判断能力，防止调用者把只读 CSV 伪装成 text 后覆盖。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接校验保存类型 helper，断言 data.csv 被识别为 Csv 并拒绝文本保存。
    #[test]
    fn validate_save_file_type_rejects_csv_even_if_caller_wanted_text() {
        let error =
            validate_save_file_type("data.csv", "a,b\n1,2\n").expect_err("csv should be readonly");

        assert!(error.to_string().contains("不支持文本保存"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     JSON 配置保存前必须由后端做语义校验，避免写入无效配置导致项目工具链损坏。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以 .json 文件名触发结构化校验，断言非法 JSON 被拒绝。
    #[test]
    fn validate_save_file_type_rejects_invalid_json() {
        let error = validate_save_file_type("config.json", "{bad json")
            .expect_err("invalid json should be rejected");

        assert!(error.to_string().contains("JSON 格式无效"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     YAML 配置保存前必须由后端做语义校验，避免 Workbench 写入无效结构化配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以 .yaml 文件名触发结构化校验，断言非法 YAML 被拒绝。
    #[test]
    fn validate_save_file_type_rejects_invalid_yaml() {
        let error = validate_save_file_type("config.yaml", "name: [")
            .expect_err("invalid yaml should be rejected");

        assert!(error.to_string().contains("YAML"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Markdown 是 Workbench 文件编辑器的可保存文本类型，应继续允许正常保存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以 .md 文件名调用保存类型校验，断言返回 Markdown 且无错误。
    #[test]
    fn validate_save_file_type_allows_markdown() {
        let detected_type = validate_save_file_type("note.md", "# Note\n").expect("markdown ok");

        assert_eq!(detected_type, WorkbenchDetectedFileType::Markdown);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTML 文件在 Workbench 中既要能源码编辑也要能渲染预览，因此保存命令必须允许写回原始源码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以 .html 文件名调用保存类型校验，断言返回 Html 且不触发结构化格式化校验。
    #[test]
    fn validate_save_file_type_allows_html() {
        let detected_type =
            validate_save_file_type("index.html", "<!doctype html><title>Preview</title>")
                .expect("html ok");

        assert_eq!(detected_type, WorkbenchDetectedFileType::Html);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTML 预览中的 `../assets/logo.png` 这类资源必须按当前 HTML 文件位置解析，并以内联 data URL 返回给前端 iframe。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 docs/page.html 与 assets/logo.png，调用 HTML 资源预览 helper，断言路径、MIME 和 data URL 正确。
    #[test]
    fn html_preview_asset_resolves_relative_to_document_path() {
        let root =
            std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("docs")).expect("create docs");
        std::fs::create_dir_all(root.join("assets")).expect("create assets");
        std::fs::write(
            root.join("docs/page.html"),
            "<img src=\"../assets/logo.png\">",
        )
        .expect("write html");
        std::fs::write(root.join("assets/logo.png"), [0x89, b'P', b'N', b'G'])
            .expect("write image");

        let asset = crate::workbench::html_assets::preview_html_asset(
            &root,
            "docs/page.html",
            "../assets/logo.png",
        )
        .expect("preview html asset");

        assert_eq!(asset.path, "assets/logo.png");
        assert_eq!(asset.mime, "image/png");
        assert!(asset.data_url.starts_with("data:image/png;base64,"));

        let _ = std::fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     HTML 预览资源只能来自 worktree 根内相对文件，不能把 http/data/blob 或绝对路径交给后端读取。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造一个 HTML 文档后，用多种非项目内资源路径调用 helper，断言全部被拒绝。
    #[test]
    fn html_preview_asset_rejects_external_urls_and_absolute_paths() {
        let root =
            std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("docs")).expect("create docs");
        std::fs::write(
            root.join("docs/page.html"),
            "<link rel=\"stylesheet\" href=\"style.css\">",
        )
        .expect("write html");

        for asset_path in [
            "https://example.com/style.css",
            "data:text/css,body{}",
            "blob:https://example.com/id",
            "#local-fragment",
            "/etc/passwd",
            "\\\\server\\share\\secret.css",
            "\\windows-root\\secret.css",
            "C:\\Users\\hans\\secret.txt",
        ] {
            let error = crate::workbench::html_assets::preview_html_asset(
                &root,
                "docs/page.html",
                asset_path,
            )
            .expect_err("unsafe asset path should be rejected");
            assert!(
                error.to_string().contains("相对路径")
                    || error.to_string().contains("项目目录之外"),
                "unexpected error for {asset_path}: {error}"
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     项目内 symlink 若指向 worktree 根外，HTML 预览不能跟随读取，否则会泄露用户磁盘其他文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 Unix 上创建指向根外 CSS 文件的 symlink，断言 HTML 资源 helper 拒绝读取。
    #[cfg(unix)]
    #[test]
    fn html_preview_asset_rejects_symlink_pointing_outside_root() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("cc-partner-html-asset-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!(
            "cc-partner-html-asset-outside-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("docs")).expect("create docs");
        std::fs::write(
            root.join("docs/page.html"),
            "<link rel=\"stylesheet\" href=\"leak.css\">",
        )
        .expect("write html");
        std::fs::write(&outside, "body{color:red}").expect("write outside");
        symlink(&outside, root.join("docs/leak.css")).expect("create symlink");

        let error =
            crate::workbench::html_assets::preview_html_asset(&root, "docs/page.html", "leak.css")
                .expect_err("outside symlink should be rejected");

        assert!(error.to_string().contains("项目目录之外"));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

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
    ///     Claude Code 自动解决 merge 冲突时，后端需要稳定 JSON 契约来接收完整文件内容。
    ///
    /// Code Logic（这个测试做什么）:
    ///     读取 schema JSON，断言顶层 required files，且每个 item 必须包含 path/content。
    #[test]
    fn merge_conflict_resolution_schema_requires_files_with_content() {
        let schema = merge_conflict_resolution_schema();

        assert_eq!(schema["required"][0], "files");
        assert_eq!(schema["properties"]["files"]["type"], "array");
        assert_eq!(
            schema["properties"]["files"]["items"]["required"][0],
            "path"
        );
        assert_eq!(
            schema["properties"]["files"]["items"]["required"][1],
            "content"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     前端需要按 projectId 过滤 merge 进度事件，防止其他项目的后台 merge 污染当前 UI。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造事件 payload 并序列化为 JSON，断言 serde camelCase 输出包含 projectId/worktreeId。
    #[test]
    fn merge_progress_event_serializes_project_id_for_frontend_filtering() {
        let event = WorkbenchMergeProgressEvent {
            project_id: "project-1".to_string(),
            worktree_id: "worktree-1".to_string(),
            stage: WorkbenchMergeStageDto {
                id: MERGE_STAGE_CHECK_SOURCE.to_string(),
                status: "running".to_string(),
                message: "checking".to_string(),
            },
        };

        let value = serde_json::to_value(event).expect("serialize event");

        assert_eq!(value["projectId"], "project-1");
        assert_eq!(value["worktreeId"], "worktree-1");
        assert_eq!(value["stage"]["id"], MERGE_STAGE_CHECK_SOURCE);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude Code 需要看到每个冲突文件的相对路径和带 conflict marker 的原文，
    ///     才能返回可直接写回的解决后完整内容。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造冲突文件输入，断言 prompt 包含路径、内容和禁止保留 conflict marker 的约束。
    #[test]
    fn merge_conflict_instruction_contains_files_and_output_contract() {
        let files = vec![MergeConflictFileInput {
            path: "README.md".to_string(),
            content: "<<<<<<< HEAD\nmain\n=======\nfeature\n>>>>>>> branch\n".to_string(),
        }];

        let instruction = build_merge_conflict_resolution_instruction(&files);

        assert!(instruction.contains("README.md"));
        assert!(instruction.contains("<<<<<<< HEAD"));
        assert!(instruction.contains("Return only"));
        assert!(instruction.contains("files"));
        assert!(instruction.contains("Do not leave conflict markers"));
        assert!(instruction.contains("|||||||"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude 输出的 path 来自模型，后端写文件前必须防止绝对路径或 `..` 越界覆盖用户其他文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     校验相对普通路径可用，绝对路径和父目录路径被拒绝。
    #[test]
    fn validate_merge_resolution_path_rejects_unsafe_paths() {
        assert!(validate_merge_resolution_path("src/lib.rs").is_ok());
        assert!(validate_merge_resolution_path("/tmp/evil").is_err());
        assert!(validate_merge_resolution_path("../evil").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     自动冲突解决会写回 Claude Code 生成的文件内容，必须保证普通相对路径仍解析在 worktree 内。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造临时根目录和普通文件，断言 safe_merge_resolution_path 返回 root 下路径。
    #[test]
    fn safe_merge_resolution_path_accepts_normal_path_inside_root() {
        let root =
            std::env::temp_dir().join(format!("cc-partner-safe-merge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("create test root");
        std::fs::write(root.join("src/lib.rs"), "fn main() {}\n").expect("write file");

        let resolved = safe_merge_resolution_path(&root, "src/lib.rs").expect("resolve path");

        assert_eq!(resolved, root.canonicalize().unwrap().join("src/lib.rs"));

        let _ = std::fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     冲突文件若是 symlink，直接写回会跟随链接覆盖工作区外文件，自动流程必须拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在 Unix 上创建指向外部文件的 symlink，断言 safe_merge_resolution_path 拒绝该路径。
    #[cfg(unix)]
    #[test]
    fn safe_merge_resolution_path_rejects_symlink_file() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("cc-partner-safe-merge-{}", uuid::Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!(
            "cc-partner-safe-merge-outside-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("create test root");
        std::fs::write(&outside, "outside\n").expect("write outside");
        symlink(&outside, root.join("conflicted.txt")).expect("create symlink");

        let error = safe_merge_resolution_path(&root, "conflicted.txt")
            .expect_err("symlink should be rejected");

        assert!(error.to_string().contains("符号链接"));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_file(outside);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Git 只要 `git add` 就可能把仍含 conflict marker 的文本标为已解决，后端必须先拦截。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言常见 conflict marker 行会被识别，普通 Markdown 分隔线不会误判。
    #[test]
    fn content_has_conflict_markers_detects_git_markers() {
        assert!(content_has_conflict_markers("<<<<<<< HEAD\nx\n"));
        assert!(content_has_conflict_markers("||||||| base\nx\n"));
        assert!(content_has_conflict_markers("=======\n"));
        assert!(content_has_conflict_markers(">>>>>>> feature\n"));
        assert!(!content_has_conflict_markers("title\n---\nbody\n"));
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
