//! Workbench 命令共享 helper/DTO。
//!
//! Business Logic（为什么需要这个模块）:
//!     子模块共用项目解析与远端映射。
//!
//! Code Logic（这个模块做什么）:
//!     monofile 前部共享定义。

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

pub(crate) const COMMIT_MESSAGE_TIMEOUT_SECS: u64 = 180;
pub(crate) const MERGE_CONFLICT_RESOLUTION_TIMEOUT_SECS: u64 = 300;
pub(crate) const MERGE_STAGE_CHECK_SOURCE: &str = "checkSource";
pub(crate) const MERGE_STAGE_CLOSE_SESSIONS: &str = "closeSessions";
pub(crate) const MERGE_STAGE_MERGE_MAIN: &str = "mergeMain";
pub(crate) const MERGE_STAGE_RESOLVE_CONFLICTS: &str = "resolveConflicts";
pub(crate) const MERGE_STAGE_CLEANUP: &str = "cleanup";
pub(crate) const MERGE_STAGE_IDS: [&str; 5] = [
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
pub(crate) struct WorkbenchCommitMessageResponse {
    pub(crate) message: String,
}

/// Workbench merge 命令返回 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端需要展示一键 merge 每个阶段的最终状态，而不只是一个布尔成功值。
///
/// Code Logic（这个结构体做什么）:
///     使用 camelCase 序列化 `{ok, worktreeId, stages}`，stages 内含固定 stage id/status/message。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchMergeResultDto {
    pub(crate) ok: bool,
    pub(crate) worktree_id: String,
    pub(crate) stages: Vec<WorkbenchMergeStageDto>,
}

/// Workbench merge 阶段 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端进度条需要知道当前阶段是等待、运行、完成、失败还是跳过。
///
/// Code Logic（这个结构体做什么）:
///     保存 stage id、status 和用户可读 message，字段名与前端约定保持一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbenchMergeStageDto {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) message: String,
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
pub(crate) struct WorkbenchMergeProgressEvent {
    pub(crate) project_id: String,
    pub(crate) worktree_id: String,
    pub(crate) stage: WorkbenchMergeStageDto,
}

/// Claude Code merge 冲突解决响应。
///
/// Business Logic（为什么需要这个结构体）:
///     自动冲突解决需要 Claude 返回每个冲突文件的完整解决后内容，后端才能安全写回。
///
/// Code Logic（这个结构体做什么）:
///     对齐 JSON schema 顶层 `files` 数组。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WorkbenchMergeResolutionResponse {
    pub(crate) files: Vec<WorkbenchMergeResolvedFile>,
}

/// Claude Code 返回的单个已解决文件。
///
/// Business Logic（为什么需要这个结构体）:
///     每个冲突文件都需要独立校验相对路径和内容，防止模型输出越界路径或残留冲突标记。
///
/// Code Logic（这个结构体做什么）:
///     保存相对 path 与完整文件 content。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct WorkbenchMergeResolvedFile {
    pub(crate) path: String,
    pub(crate) content: String,
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
    pub(crate) formatted: String,
}

/// 传给 Claude Code 的单个冲突文件输入。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude 解决冲突时必须看到 Git 相对路径和带 conflict marker 的当前文件全文。
///
/// Code Logic（这个结构体做什么）:
///     在构造 prompt 前保存 path/content，便于测试 prompt 内容。
#[derive(Debug, Clone)]
pub(crate) struct MergeConflictFileInput {
    pub(crate) path: String,
    pub(crate) content: String,
}

/// Business Logic（为什么需要这个函数）:
///     多个命令都需要用 project_id 查找最近项目记录，并在缺失时给前端明确错误。
///
/// Code Logic（这个函数做什么）:
///     从 WorkbenchProjectRepo 读取项目；None 转换为 AppError::not_found。
pub(crate) async fn get_project(
    state: &AppState,
    project_id: &str,
) -> Result<WorkbenchProjectRow, AppError> {
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
pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// Business Logic（为什么需要这个函数）:
///     每个工作台项目都要有一个稳定的主 worktree 记录，表示用户最初添加的项目路径。
///
/// Code Logic（这个函数做什么）:
///     用 project_id 派生确定性 id，避免重复创建主工作区记录。
pub(crate) fn main_worktree_id(project_id: &str) -> String {
    format!("{project_id}:main")
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 自动创建 worktree 时需要放在应用数据目录下，避免污染用户项目根目录。
///
/// Code Logic（这个函数做什么）:
///     基于 SQLite db_path 的父目录创建 worktrees/<project_id>/<branch_slug> 路径。
pub(crate) fn worktree_storage_path(state: &AppState, project_id: &str, branch: &str) -> PathBuf {
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
pub(crate) async fn ensure_main_worktree(
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
pub(crate) fn normalize_worktree_path(path: &str) -> String {
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
pub(crate) fn discovered_git_worktree_id(project_id: &str, path: &str) -> String {
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
pub(crate) fn discovered_git_worktree_name(parsed: &workbench_git::ParsedWorktree) -> String {
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
pub(crate) fn discovered_git_worktree_row(
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
pub(crate) async fn sync_git_worktrees(
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
pub(crate) fn worktree_to_dto(row: &WorkbenchWorktreeRow) -> WorkbenchWorktreeDto {
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
pub(crate) async fn resolve_worktree(
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
pub(crate) async fn merged_session_dtos(
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
///     读取持久化会话；用 `try_claim_restore` 原子占位避免并发重复恢复（Finding 5）；
///     项目存在时补齐可读 worktree 名再调用 registry.restore，成功后写回最新 row；
///     无论成功/失败都释放占位；项目缺失则删除孤儿会话。
pub(crate) async fn restore_persisted_sessions(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<(), AppError> {
    let rows = state.workbench_session_repo.list(project_id).await?;
    for row in rows {
        // Finding 5: 原子占位 — 把"已运行期 + 是否有其他 caller 在 restore"的检查合为单步，
        // 消除 contains() 与 restore() 之间的 TOCTOU 窗口。
        if !state.workbench_sessions.try_claim_restore(&row.id) {
            continue;
        }
        // 后续所有路径都必须释放占位，否则该 session 会被永久跳过。
        let Some(project) = state.workbench_project_repo.get(&row.project_id).await? else {
            state.workbench_session_repo.delete(&row.id).await?;
            state.workbench_sessions.release_restore_claim(&row.id);
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
        match state
            .workbench_sessions
            .restore(state.clone(), project, row.clone(), worktree_name)
        {
            Ok(restored) => {
                state.workbench_session_repo.upsert(&restored).await?;
            }
            Err(error) => {
                tracing::warn!("恢复工作台终端会话失败: {error}");
                let mut disconnected = row.clone();
                disconnected.status = "disconnected".to_string();
                disconnected.exited_at = Some(now_iso());
                disconnected.updated_at = now_iso();
                state.workbench_session_repo.upsert(&disconnected).await?;
            }
        }
        // 成功路径：spawn_row 已写入 sessions map，contains 自然命中；
        // 失败路径：释放占位允许后续请求重试 restore。
        state.workbench_sessions.release_restore_claim(&row.id);
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     文件系统操作是同步阻塞 IO；命令层必须把它们移到 blocking pool，避免卡住 async runtime。
///
/// Code Logic（这个函数做什么）:
///     包装 tauri::async_runtime::spawn_blocking，并把 JoinError 转换为 AppError。
pub(crate) async fn run_blocking_fs<T, F>(task: F) -> Result<T, AppError>
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
pub(crate) async fn resolve_workbench_file_path(
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
pub(crate) fn validate_save_file_type(
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

/// 从设备表解析远端设备 base URL。
///
/// Business Logic（为什么需要这个函数）:
///     远端 Workbench 命令必须先确认目标设备仍在 mDNS 发现表中，否则应提示设备离线。
///
/// Code Logic（这个函数做什么）:
///     从传入的设备 HashMap 按 device_id 查找设备，命中后调用 `Device::base_url`，缺失返回中文错误。
pub(crate) fn device_base_url_from_devices(
    devices: &HashMap<String, Device>,
    device_id: &str,
) -> Result<String, AppError> {
    let device = devices
        .get(device_id)
        .ok_or_else(|| AppError::generic("远端设备不在线"))?;
    Ok(device.base_url())
}

/// 从 AppState 解析远端设备 base URL。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri 远端 Workbench 命令只拿到 deviceId，需要通过当前发现设备表找到对端 HTTP 入口。
///
/// Code Logic（这个函数做什么）:
///     读取 `state.devices`，委托纯 helper 生成 base URL；只在同步代码段持有读锁，不跨 await。
pub(crate) fn device_base_url(state: &AppState, device_id: &str) -> Result<String, AppError> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    device_base_url_from_devices(&devices, device_id)
}

/// 读取当前发现设备名。
///
/// Business Logic（为什么需要这个函数）:
///     远端快捷方式应优先展示本机发现表中的最新设备名，对端返回值只作为兜底。
///
/// Code Logic（这个函数做什么）:
///     从 `state.devices` 读取设备名快照；设备缺失时返回 None，不跨 await 持锁。
pub(crate) fn device_name_from_state(state: &AppState, device_id: &str) -> Option<String> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    devices.get(device_id).map(|device| device.name.clone())
}

/// 构造本地远端项目快捷方式 row。
///
/// Business Logic（为什么需要这个函数）:
///     打开远端项目后，本机只保存一个最近项目快捷方式，后续操作再通过远端路径和设备 ID 解析。
///
/// Code Logic（这个函数做什么）:
///     用 `remote_project_id(device_id, remote.path)` 生成稳定 ID，kind 固定为 remote；
///     已存在同 ID row 时复用 id/created_at，只更新时间和展示字段。
pub(crate) fn build_remote_project_shortcut_row(
    device_id: &str,
    current_device_name: Option<&str>,
    remote: &WorkbenchProjectDto,
    existing: Option<&WorkbenchProjectRow>,
    now: &str,
) -> WorkbenchProjectRow {
    let id = existing
        .as_ref()
        .map(|project| project.id.clone())
        .unwrap_or_else(|| remote_project_id(device_id, &remote.path));
    let device_name = current_device_name
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| remote.device_name.clone());

    WorkbenchProjectRow {
        id,
        name: remote.name.clone(),
        kind: "remote".to_string(),
        device_id: device_id.to_string(),
        device_name,
        path: remote.path.clone(),
        last_opened_at: now.to_string(),
        created_at: existing
            .as_ref()
            .map(|project| project.created_at.clone())
            .unwrap_or_else(|| now.to_string()),
        updated_at: now.to_string(),
    }
}

/// 远端项目网关上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     本机 remote shortcut 只保存设备 ID 和远端真实路径；执行 worktree/Git/files 操作前需要恢复远端 local 项目 ID。
///
/// Code Logic（这个结构体做什么）:
///     保存设备 ID、对端 base URL、本机 shortcut projectId，以及远端设备上的 local projectId。
#[derive(Debug, Clone)]
pub(crate) struct RemoteWorkbenchProjectContext {
    pub(crate) device_id: String,
    pub(crate) base_url: String,
    pub(crate) local_project_id: String,
    pub(crate) inner_project_id: String,
}

/// 远端 worktree 网关上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     commit/push/merge/remove 只收到 worktreeId，也必须恢复该 worktree 所属远端项目与本机 shortcut。
///
/// Code Logic（这个结构体做什么）:
///     保存设备、base URL、本机 shortcut projectId、远端 projectId 以及远端 worktreeId。
#[derive(Debug, Clone)]
pub(crate) struct RemoteWorkbenchWorktreeContext {
    pub(crate) device_id: String,
    pub(crate) base_url: String,
    pub(crate) local_project_id: String,
    pub(crate) inner_project_id: String,
    pub(crate) inner_worktree_id: String,
}

/// worktree-id-only 命令目标。
///
/// Business Logic（为什么需要这个枚举）:
///     只接收 worktreeId 的命令需要先区分本机 worktree 和远端 worktree，避免 remote id 误查本机数据库。
///
/// Code Logic（这个枚举做什么）:
///     Local 保存原始本机 id；Remote 保存解析出的 deviceId 和远端 inner worktreeId。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeCommandTarget {
    Local(String),
    Remote {
        device_id: String,
        inner_worktree_id: String,
    },
}

/// Business Logic（为什么需要这个函数）:
///     每个 remote shortcut 操作都需要确保远端设备上存在对应的 local 项目记录，才能调用后续路由。
///
/// Code Logic（这个函数做什么）:
///     校验 project.kind 为 remote，按 device_id 找 base URL，调用远端 open-project 用 path 恢复/创建远端 local project。
pub(crate) async fn ensure_remote_project_context(
    state: &AppState,
    project: &WorkbenchProjectRow,
) -> Result<RemoteWorkbenchProjectContext, AppError> {
    if project.kind != "remote" {
        return Err(AppError::generic("当前项目不是远端项目"));
    }
    if project.device_id.trim().is_empty() {
        return Err(AppError::generic("远端项目缺少设备 ID"));
    }
    let base_url = device_base_url(state, &project.device_id)?;
    let remote = RemoteWorkbenchClient::new()
        .open_project(&base_url, &project.path)
        .await?;
    Ok(RemoteWorkbenchProjectContext {
        device_id: project.device_id.clone(),
        base_url,
        local_project_id: project.id.clone(),
        inner_project_id: remote.id,
    })
}

/// Business Logic（为什么需要这个函数）:
///     只接收 worktreeId 的命令无法从参数直接拿到本机 shortcut projectId，需要先向远端查询 worktree 所属项目。
///
/// Code Logic（这个函数做什么）:
///     通过 remote get-worktree 读取远端 DTO，再把 inner projectId 映射到本机 shortcut projectId，并注册事件桥项目映射。
pub(crate) async fn ensure_remote_worktree_context(
    state: &AppState,
    device_id: String,
    inner_worktree_id: String,
) -> Result<RemoteWorkbenchWorktreeContext, AppError> {
    let base_url = device_base_url(state, &device_id)?;
    let worktree = RemoteWorkbenchClient::new()
        .get_worktree(&base_url, &inner_worktree_id)
        .await?;
    let local_project_id = local_project_id_for_remote_inner_project(
        state,
        &device_id,
        &base_url,
        &worktree.project_id,
    )
    .await?;
    let context = RemoteWorkbenchWorktreeContext {
        device_id,
        base_url,
        local_project_id,
        inner_project_id: worktree.project_id,
        inner_worktree_id,
    };
    ensure_remote_event_bridge_for_worktree_context(state, &context);
    Ok(context)
}

/// Business Logic（为什么需要这个函数）:
///     远端事件桥可能已记录 inner projectId 映射；若未记录，则需要从本机 remote shortcut 列表恢复。
///
/// Code Logic（这个函数做什么）:
///     先查事件桥 registry；缺失时遍历同设备 remote shortcut，通过远端 open-project 对比 inner projectId。
pub(crate) async fn local_project_id_for_remote_inner_project(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    inner_project_id: &str,
) -> Result<String, AppError> {
    if let Some(project_id) = state
        .workbench_remote_event_bridges
        .local_project_id_for(device_id, inner_project_id)
    {
        return Ok(project_id);
    }

    let projects = state.workbench_project_repo.list().await?;
    for project in projects
        .iter()
        .filter(|project| project.kind == "remote" && project.device_id == device_id)
    {
        match RemoteWorkbenchClient::new()
            .open_project(base_url, &project.path)
            .await
        {
            Ok(remote) if remote.id == inner_project_id => return Ok(project.id.clone()),
            Ok(_) => {}
            Err(error) => {
                tracing::debug!("恢复远端 worktree 项目 shortcut 映射失败: {error}");
            }
        }
    }
    Err(AppError::not_found(
        "未找到远端 worktree 对应的本机项目快捷方式，请重新打开远端项目",
    ))
}

/// Business Logic（为什么需要这个函数）:
///     远端返回的 worktree id 只能在远端设备上使用，本机前端需要带设备前缀的统一 ID。
///
/// Code Logic（这个函数做什么）:
///     遍历远端 worktree DTO，把 id 映射为 `remote:<device_id>:<inner_id>`，project_id 改回本机 shortcut id。
pub(crate) fn map_remote_worktree_dtos(
    device_id: &str,
    local_project_id: &str,
    items: Vec<WorkbenchWorktreeDto>,
) -> Vec<WorkbenchWorktreeDto> {
    items
        .into_iter()
        .map(|mut item| {
            item.id = remote_entity_id(device_id, &item.id);
            item.project_id = local_project_id.to_string();
            item
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     远端返回的 terminal session id/worktree id 只能在远端设备内部使用，本机 UI 需要带设备前缀的统一 ID。
///
/// Code Logic（这个函数做什么）:
///     遍历远端 session DTO，把 session id 和可选 worktree id 映射为 `remote:<device_id>:<inner>`；
///     project_id 在项目上下文调用中改成本机 remote shortcut id，否则退化为远端 project 的 remote entity id。
pub(crate) fn map_remote_session_dtos(
    device_id: &str,
    local_project_id: &str,
    items: Vec<WorkbenchSessionDto>,
) -> Vec<WorkbenchSessionDto> {
    map_remote_session_dtos_with_project(device_id, Some(local_project_id), items)
}

/// Business Logic（为什么需要这个函数）:
///     session-id-only 命令没有本机 shortcut projectId，但返回 DTO 仍不能泄露裸远端 id。
///
/// Code Logic（这个函数做什么）:
///     将 session/worktree id 加设备前缀；project_id 有本机 shortcut 时使用 shortcut，否则用 remote entity id 兜底。
pub(crate) fn map_remote_session_dtos_with_project(
    device_id: &str,
    local_project_id: Option<&str>,
    items: Vec<WorkbenchSessionDto>,
) -> Vec<WorkbenchSessionDto> {
    items
        .into_iter()
        .map(|mut item| {
            item.id = remote_entity_id(device_id, &item.id);
            item.project_id = local_project_id
                .map(str::to_string)
                .unwrap_or_else(|| remote_entity_id(device_id, &item.project_id));
            item.worktree_id = item
                .worktree_id
                .map(|worktree_id| remote_entity_id(device_id, &worktree_id));
            item
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     本机前端传回的 remote worktreeId 需要剥掉设备前缀后才能发给远端设备。
///
/// Code Logic（这个函数做什么）:
///     None 表示远端主 worktree；Some 必须是当前 device_id 的 `remote:<device_id>:<inner>`，成功返回 inner id。
pub(crate) fn remote_inner_worktree_id(
    device_id: &str,
    worktree_id: Option<String>,
) -> Result<Option<String>, AppError> {
    let Some(worktree_id) = worktree_id else {
        return Ok(None);
    };
    let parsed = parse_remote_entity_id(&worktree_id)
        .ok_or_else(|| AppError::generic("远端 worktree ID 格式无效"))?;
    if parsed.device_id != device_id {
        return Err(AppError::generic("远端 worktree 不属于当前设备"));
    }
    Ok(Some(parsed.inner_id))
}

/// Business Logic（为什么需要这个函数）:
///     commit/push/merge/remove 这类只接收 worktreeId 的命令必须先识别 remote id，避免错误查询本机 repo。
///
/// Code Logic（这个函数做什么）:
///     若 worktree_id 是 `remote:<deviceId>:<inner>` 则返回 Remote 目标，否则原样返回 Local 目标。
pub(crate) fn worktree_command_target(
    worktree_id: &str,
) -> Result<WorktreeCommandTarget, AppError> {
    if let Some(parsed) = parse_remote_entity_id(worktree_id) {
        return Ok(WorktreeCommandTarget::Remote {
            device_id: parsed.device_id,
            inner_worktree_id: parsed.inner_id,
        });
    }
    Ok(WorktreeCommandTarget::Local(worktree_id.to_string()))
}

/// Business Logic（为什么需要这个函数）:
///     session-id-only terminal 命令需要从统一 remote sessionId 中取得远端真实 sessionId。
///
/// Code Logic（这个函数做什么）:
///     解析 `remote:<device_id>:<inner_session_id>`，并校验设备 ID 与当前转发目标一致。
pub(crate) fn remote_inner_session_id(
    device_id: &str,
    session_id: &str,
) -> Result<String, AppError> {
    let parsed = parse_remote_entity_id(session_id)
        .ok_or_else(|| AppError::generic("远端 session ID 格式无效"))?;
    if parsed.device_id != device_id {
        return Err(AppError::generic("远端 session 不属于当前设备"));
    }
    Ok(parsed.inner_id)
}

/// Business Logic（为什么需要这个函数）:
///     远端 terminal 输出依赖长连接事件桥，list/create 之后必须确保对应设备只有一个桥接任务在运行。
///
/// Code Logic（这个函数做什么）:
///     委托 AppState 中的 RemoteEventBridgeRegistry 按 device_id 去重启动 `/api/workbench/events` 连接。
pub(crate) fn ensure_remote_event_bridge_for_context(
    state: &AppState,
    context: &RemoteWorkbenchProjectContext,
) {
    ensure_remote_event_bridge_for_project_mapping(
        state,
        &context.device_id,
        &context.base_url,
        &context.inner_project_id,
        &context.local_project_id,
    );
}

/// Business Logic（为什么需要这个函数）:
///     id-only remote session/worktree 操作拿到 inner projectId 后，也要把事件桥项目映射补齐。
///
/// Code Logic（这个函数做什么）:
///     用指定 device/base_url 和 inner/local projectId 调 registry，确保桥接任务存在并更新 project 映射。
pub(crate) fn ensure_remote_event_bridge_for_project_mapping(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    inner_project_id: &str,
    local_project_id: &str,
) {
    state.workbench_remote_event_bridges.ensure_bridge(
        device_id.to_string(),
        base_url.to_string(),
        Some(RemoteEventBridgeProjectMapping {
            inner_project_id: inner_project_id.to_string(),
            local_project_id: local_project_id.to_string(),
        }),
        state.clone(),
    );
}

/// Business Logic（为什么需要这个函数）:
///     id-only remote terminal 命令只有 deviceId/baseUrl，仍应确保事件桥连接但不会新增项目映射。
///
/// Code Logic（这个函数做什么）:
///     委托 registry 以 None project mapping 启动或复用该设备事件桥。
pub(crate) fn ensure_remote_event_bridge_for_device(
    state: &AppState,
    device_id: &str,
    base_url: &str,
) {
    state.workbench_remote_event_bridges.ensure_bridge(
        device_id.to_string(),
        base_url.to_string(),
        None,
        state.clone(),
    );
}

/// Business Logic（为什么需要这个函数）:
///     remote worktree merge progress 需要在 merge 开始前知道 innerProjectId 到本机 shortcut 的映射。
///
/// Code Logic（这个函数做什么）:
///     从 worktree context 注册项目映射并确保对应设备事件桥连接。
pub(crate) fn ensure_remote_event_bridge_for_worktree_context(
    state: &AppState,
    context: &RemoteWorkbenchWorktreeContext,
) {
    ensure_remote_event_bridge_for_project_mapping(
        state,
        &context.device_id,
        &context.base_url,
        &context.inner_project_id,
        &context.local_project_id,
    );
}
