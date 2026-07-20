//! Workbench 命令共享 helper/DTO。
//!
//! Business Logic（为什么需要这个模块）:
//!     子模块共用项目解析与远端映射。
//!
//! Code Logic（这个模块做什么）:
//!     monofile 前部共享定义。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::MutationControlError;
use crate::error::AppError;
use crate::models::device::Device;
use crate::state::AppState;
use crate::workbench::models::{
    WorkbenchDetectedFileType, WorkbenchGitStatusDto, WorkbenchPathInfo, WorkbenchProjectDto,
    WorkbenchProjectRow, WorkbenchSessionDto, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto;
use crate::workbench::sessions::pane_count_for_row;
use crate::workbench::{
    file_content, file_preview, fs as workbench_fs, git as workbench_git, projects,
    remote_client::RemoteWorkbenchClient,
    remote_events::RemoteEventBridgeProjectMapping,
    remote_ids::{parse_remote_entity_id, remote_entity_id, remote_project_id},
};
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// GuiClient 时经缓存 control client 代理 workbench op；否则返回 None 让调用方走本地 owner 路径。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 GUI 进程是 GuiClient，不得直接执行 Workbench owner 逻辑（PTY/tmux/RemoteClient/bridge）；
///     必须把完整 op 代理到 sidecar HeadlessOwner，保证唯一 runtime owner。
///
/// Code Logic（这个函数做什么）:
///     若 `runtime_role != GuiClient` 返回 `Ok(None)`；否则经
///     `state.backend_control_client_runtime.workbench_op` 复用缓存 client，成功时 `Ok(Some(T))`。
pub(crate) async fn proxy_workbench_if_gui<T: DeserializeOwned>(
    state: &AppState,
    op: &str,
    payload: impl Serialize,
) -> Result<Option<T>, AppError> {
    if state.runtime_role != RuntimeRole::GuiClient {
        return Ok(None);
    }
    Ok(Some(
        state
            .backend_control_client_runtime
            .workbench_op(op, payload)
            .await?,
    ))
}

/// GuiClient 时代理 workbench mutation 并保留 uncertain→unknown envelope。
///
/// Business Logic（为什么需要这个函数）:
///     GuiClient 不得在本地执行 workbench mutation；代理到 sidecar 时若传输不确定，
///     必须保留 unknown envelope（禁止自动重放），不能把 Uncertain 折叠成普通 AppError。
///
/// Code Logic（这个函数做什么）:
///     非 GuiClient 返回 `Ok(None)`；GuiClient 经 runtime `workbench_mutation_op_value`
///     （失败仅失效缓存，不重放当前 mutation）：Ok→反序列化 envelope；
///     Uncertain→`WorkbenchMutationEnvelopeDto::unknown`；Failed→Err。
pub(crate) async fn proxy_workbench_mutation_if_gui<T: DeserializeOwned>(
    state: &AppState,
    op: &str,
    payload: impl Serialize,
    client_operation_id: &str,
) -> Result<Option<WorkbenchMutationEnvelopeDto<T>>, AppError> {
    if state.runtime_role != RuntimeRole::GuiClient {
        return Ok(None);
    }
    match state
        .backend_control_client_runtime
        .workbench_mutation_op_value(op, payload)
        .await
    {
        Ok(value) => {
            let env: WorkbenchMutationEnvelopeDto<T> =
                serde_json::from_value(value).map_err(|e| {
                    AppError::generic(format!("mutation envelope 解析失败 ({op}): {e}"))
                })?;
            Ok(Some(env))
        }
        Err(MutationControlError::Uncertain { transport }) => Ok(Some(
            WorkbenchMutationEnvelopeDto::unknown(client_operation_id, Some(transport)),
        )),
        Err(MutationControlError::Failed(e)) => Err(e),
    }
}

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
///     R14/R18 M1：仍在 restore claim 中的持久行与 provisional live 都不得当作可立即 replay
///     的会话返回（Ready 前不可对外暴露 live）。
///
/// Code Logic（这个函数做什么）:
///     先把持久化 row 投影为 DTO（跳过 `is_restore_claim_held` 的 id），
///     再用 registry `list` 的实时 DTO 覆盖（registry.list 已过滤 claim-held），
///     live overlay 再双重跳过 claim-held 以防竞态。
pub(crate) async fn merged_session_dtos(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    let mut sessions: Vec<WorkbenchSessionDto> = state
        .workbench_session_repo
        .list(project_id)
        .await?
        .iter()
        .filter(|row| !state.workbench_sessions.is_restore_claim_held(&row.id))
        .map(|row| row.to_dto_with_pane_count(pane_count_for_row(row)))
        .collect();
    for live in state.workbench_sessions.list(project_id) {
        // R18 M1：live overlay 也不得把 claim-held provisional 暴露出去。
        if state.workbench_sessions.is_restore_claim_held(&live.id) {
            continue;
        }
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
///     A8：list/open 路径默认 **skip-missing**——仅 attach 已存在 tmux target；
///     缺失 target / raw PTY 不创建 shell（只有用户显式新建终端才 `create_tmux_window`）。
///     R14–R17：并发 list 等待 in-flight restore 的**结果**，不得把恢复中行当 ready，
///     也不得把 holder 持久化失败当成功合并不可 replay 会话（holder 必须 `return Err`）。
///
/// Code Logic（这个函数做什么）:
///     读取持久化会话；用 `try_claim_restore` 原子占位（Finding 5 + R14–R17）：
///     `Claimed` 独占 restore；`AlreadyLive` 跳过；`RestoreInProgress` await watch 结果。
///     Ready/PersistedDisconnected → continue；Failed → 映射稳定错误返回；
///     TimedOut → `timeout(session_restore_wait_timeout)`；禁止部分成功清单。
///     项目存在时补齐可读 worktree 名再调用 registry.restore（内部 skip-missing）。
///     成功写回最新 row 并 finish Ready。
///     restore 成功但 upsert 失败：先显式 `drop(SessionSpawnGuard)` 回收 attach，
///     再 finish Failed 并 **return 原始 Err**（禁止先放 claim 再 Drop spawn，
///     否则第三方并发 list 可能短暂 AlreadyLive）。
///     restore Err 时尝试持久化 disconnected，成功 finish PersistedDisconnected；
///     失败 finish Failed 并 **return 原始 Err**。
///     project 查询/删除 `?` 失败由 guard Drop 广播 Failed 并向上传播。
pub(crate) async fn restore_persisted_sessions(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<(), AppError> {
    use crate::workbench::sessions::{
        shared_restore_failed_error, wait_for_shared_restore, RestoreClaimOutcome,
        SharedRestoreNotification, SharedRestoreWaitResult,
    };

    state.runtime_role.require_owner()?;
    let rows = state.workbench_session_repo.list(project_id).await?;
    for row in rows {
        // Finding 5 + R14/R16: 原子占位，区分 AlreadyLive / RestoreInProgress / Claimed。
        match state.workbench_sessions.try_claim_restore(&row.id) {
            RestoreClaimOutcome::AlreadyLive => continue,
            RestoreClaimOutcome::RestoreInProgress(rx) => {
                // 并发 list：共享 in-flight restore 结果；Failed/超时 fail closed。
                match wait_for_shared_restore(rx).await {
                    SharedRestoreWaitResult::Ready
                    | SharedRestoreWaitResult::PersistedDisconnected => continue,
                    SharedRestoreWaitResult::Failed(category) => {
                        return Err(shared_restore_failed_error(category));
                    }
                    SharedRestoreWaitResult::TimedOut => {
                        return Err(AppError::timeout(
                            "session_restore_wait_timeout".to_string(),
                        ));
                    }
                }
            }
            RestoreClaimOutcome::Claimed => {}
        }
        // RAII：任意 early return / Err 路径 Drop 都会 finish Failed 并通知 waiters。
        let mut claim_guard = crate::workbench::sessions::RestoreClaimGuard::new(
            (*state.workbench_sessions).clone(),
            row.id.clone(),
        );
        let Some(project) = state.workbench_project_repo.get(&row.project_id).await? else {
            state.workbench_session_repo.delete(&row.id).await?;
            // 孤儿删除成功：无会话可合并；广播 PersistedDisconnected 让 waiter 安全 continue。
            claim_guard.finish(SharedRestoreNotification::PersistedDisconnected);
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
                // spawn 成功后 upsert 失败也必须回收 attach，并广播 Failed + 返回 Err。
                let mut spawn_guard = crate::workbench::sessions::SessionSpawnGuard::new(
                    (*state.workbench_sessions).clone(),
                    restored.id.clone(),
                );
                match state.workbench_session_repo.upsert(&restored).await {
                    Ok(()) => {
                        spawn_guard.commit();
                        claim_guard.finish(SharedRestoreNotification::Ready);
                    }
                    Err(error) => {
                        tracing::warn!("恢复工作台终端后持久化失败，已回收 attach: {error}");
                        let category = error.classify();
                        // R17 M1：必须先回收 spawn，再放 claim；否则第三方 list 会短暂 AlreadyLive。
                        drop(spawn_guard);
                        claim_guard.finish(SharedRestoreNotification::Failed(category));
                        // 持有 claim 的 list 不得落到 Ok(merged_session_dtos) 返回不可 replay 成功清单。
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                tracing::warn!("恢复工作台终端会话失败: {error}");
                let mut disconnected = row.clone();
                disconnected.status = "disconnected".to_string();
                disconnected.exited_at = Some(now_iso());
                disconnected.updated_at = now_iso();
                match state.workbench_session_repo.upsert(&disconnected).await {
                    Ok(()) => {
                        // skip-missing 已落盘 disconnected：list 可合并该行，不可 live replay。
                        claim_guard.finish(SharedRestoreNotification::PersistedDisconnected);
                    }
                    Err(persist_error) => {
                        tracing::warn!(
                            "恢复失败后写入 disconnected 状态失败，会话可能不可 replay: {persist_error}"
                        );
                        // R17 M1：disconnected upsert 失败必须返回 Err，禁止 holder 吞掉后 Ok 合并 running 行。
                        let category = persist_error.classify();
                        claim_guard.finish(SharedRestoreNotification::Failed(category));
                        return Err(persist_error);
                    }
                }
            }
        }
        // 若上面未显式 finish，Drop 默认 Failed(Internal)。
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
    state.runtime_role.require_owner()?;
    if project.kind != "remote" {
        return Err(AppError::generic("当前项目不是远端项目"));
    }
    if project.device_id.trim().is_empty() {
        return Err(AppError::generic("远端项目缺少设备 ID"));
    }
    let base_url = device_base_url(state, &project.device_id)?;
    let remote = RemoteWorkbenchClient::new()
        .with_expected_device_id(&project.device_id)
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
    state.runtime_role.require_owner()?;
    let base_url = device_base_url(state, &device_id)?;
    let worktree = RemoteWorkbenchClient::new()
        .with_expected_device_id(&device_id)
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
            .with_expected_device_id(device_id)
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
    if state.runtime_role.require_owner().is_err() {
        return;
    }
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
    if state.runtime_role.require_owner().is_err() {
        return;
    }
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
    if state.runtime_role.require_owner().is_err() {
        return;
    }
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
    if state.runtime_role.require_owner().is_err() {
        return;
    }
    ensure_remote_event_bridge_for_project_mapping(
        state,
        &context.device_id,
        &context.base_url,
        &context.inner_project_id,
        &context.local_project_id,
    );
}

#[cfg(test)]
mod restore_holder_fail_closed_tests {
    //! R17 M1：生产 list/restore 路径故障注入回归。

    use super::*;
    use crate::backend::authority::RuntimeRole;
    use crate::backend::event_bus::RuntimeEventBus;
    use crate::backend::runtime_metrics::RuntimeMetrics;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::cloud_sync::CloudSyncRuntime;
    use crate::config::{
        AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate, PromptRepo, ScratchpadRepo,
        SshTargetRepo, TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo,
        WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorktreeRepo,
        WorkbenchWorkspaceLayoutRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use crate::updater::UpdateRuntime;
    use crate::workbench::models::{WorkbenchProjectRow, WorkbenchSessionRow};
    use crate::workbench::sessions::{
        shared_restore_failed_error, RestoreClaimOutcome, SharedRestoreWaitResult,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Duration;

    /// Business Logic（为什么需要这个函数）:
    ///     R17 集成测试需要最小 owner AppState，覆盖生产 list/restore 与 inject upsert 失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存 SQLite 建 projects/sessions/worktrees 表，构造 HeadlessOwner AppState。
    async fn build_restore_fail_state() -> AppState {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE workbench_worktrees (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, name TEXT NOT NULL, branch TEXT, \
             base_branch TEXT, path TEXT NOT NULL, is_main INTEGER NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE workbench_sessions (\
             id TEXT PRIMARY KEY, project_id TEXT NOT NULL, worktree_id TEXT, name TEXT NOT NULL, \
             command TEXT NOT NULL, cwd TEXT, status TEXT NOT NULL, cols INTEGER NOT NULL, \
             rows INTEGER NOT NULL, started_at TEXT NOT NULL, exited_at TEXT, exit_code INTEGER, \
             backend TEXT NOT NULL, backend_id TEXT, backend_window_id TEXT, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let project_repo = WorkbenchProjectRepo::new(pool.clone());
        let worktree_repo = WorkbenchWorktreeRepo::new(pool.clone());
        let session_repo = WorkbenchSessionRepo::new(pool.clone());
        let layout_repo = WorkbenchWorkspaceLayoutRepo::new(pool.clone());
        layout_repo.ensure_schema().await.unwrap();

        project_repo
            .upsert(&WorkbenchProjectRow {
                id: "p1".to_string(),
                name: "demo".to_string(),
                kind: "local".to_string(),
                device_id: "d1".to_string(),
                device_name: "local".to_string(),
                path: "/tmp/demo".to_string(),
                last_opened_at: "t".to_string(),
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            })
            .await
            .unwrap();

        let config = AppConfig {
            device_id: "d1".to_string(),
            device_name: "test".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            db_path: ":memory:".to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        };
        let store = Arc::new(MemoryConfigStore::with_config(config.clone()));
        let config_runtime = Arc::new(ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());
        let owner = uuid::Uuid::new_v4().to_string();
        let event_bus = Arc::new(RuntimeEventBus::new(owner));

        AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: maintenance_gate.clone(),
            prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("d1".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(std::path::PathBuf::from("/tmp"))),
            update_runtime: Arc::new(UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(project_repo),
            workbench_session_repo: Arc::new(session_repo),
            workbench_worktree_repo: Arc::new(worktree_repo),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            workbench_workspace_layout_repo: Arc::new(layout_repo),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::path::PathBuf::from("/tmp/browser-verification-r17"),
                    "test-owner".into(),
                )
                .expect("browser verification fixture"),
            ),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: {
                let (tx, _) = tokio::sync::broadcast::channel(8);
                tx
            },
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::default(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            runtime_metrics: Arc::new(RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus,
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     注入 raw PTY running 行以触发 restore skip-missing → disconnected upsert 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 backend=pty 的 running 会话元数据（无正文断言）。
    async fn seed_raw_pty_running(state: &AppState, session_id: &str) {
        let row = WorkbenchSessionRow {
            id: session_id.to_string(),
            project_id: "p1".to_string(),
            worktree_id: None,
            name: "term".to_string(),
            command: "/bin/sh".to_string(),
            cwd: "/tmp/demo".to_string(),
            status: "running".to_string(),
            cols: 80,
            rows: 24,
            started_at: "t".to_string(),
            exited_at: None,
            exit_code: None,
            backend: "pty".to_string(),
            backend_id: None,
            backend_window_id: None,
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        };
        state.workbench_session_repo.upsert(&row).await.unwrap();
    }

    /// Business Logic（R17 M1: 为什么需要这个测试）:
    ///     claim holder 在 disconnected upsert 失败时若仍 Ok，会合并出 running 但无 registry 的清单，
    ///     后续 replay 永久 not_found。
    ///
    /// Code Logic（这个测试做什么）:
    ///     生产 `restore_persisted_sessions` + inject upsert 失败：holder Err，无 live/claim，
    ///     若错误地 merge 会得到 running；断言 merge 不该被当作成功 list。
    #[tokio::test]
    async fn production_list_holder_returns_err_on_disconnected_upsert_failure() {
        let state = build_restore_fail_state().await;
        seed_raw_pty_running(&state, "s-holder-fail").await;
        // 下一次 upsert（disconnected 落盘）失败。
        state.workbench_session_repo.inject_fail_next_upserts(1);

        let err = restore_persisted_sessions(&state, Some("p1"))
            .await
            .expect_err("holder must return Err on disconnected upsert failure");
        assert_eq!(err.code(), "workbench_session_upsert_injected_failure");
        assert_eq!(err.ipc_category_code(), "internal");
        assert!(!state.workbench_sessions.contains("s-holder-fail"));
        assert!(!state.workbench_sessions.is_restore_claim_held("s-holder-fail"));
        // SQLite 仍可能是 running（upsert 未成功）；不得被当作成功 list DTO。
        let rows = state
            .workbench_session_repo
            .list(Some("p1"))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "running");
        // 模拟若 holder 吞掉错误继续 merge：会返回 running DTO；生产路径禁止。
        let merged = merged_session_dtos(&state, Some("p1")).await.unwrap();
        // claim 已释放，merge 会看到 stale running 行——这正是 holder 必须 return Err 的原因。
        assert!(
            merged.iter().any(|s| s.id == "s-holder-fail" && s.status == "running"),
            "stale running row still in sqlite; success path would leak non-replayable DTO"
        );
        assert!(
            state
                .workbench_sessions
                .require_live_for_replay("s-holder-fail")
                .is_err(),
            "session must not be live-replayable after failed restore"
        );
    }

    /// Business Logic（R17 M1: 为什么需要这个测试）:
    ///     并发 list：holder 持久化失败时 waiter 必须 Failed；清理后第三方不得 AlreadyLive。
    ///
    /// Code Logic（这个测试做什么）:
    ///     双任务并发 `restore_persisted_sessions` + inject 1 次 upsert 失败：
    ///     holder Err；waiter 为 shared Failed 或晚到后落盘 disconnected；
    ///     无 registry live；第三方 try_claim 为 Claimed。
    #[tokio::test]
    async fn production_list_holder_and_waiter_fail_without_already_live_window() {
        let state = build_restore_fail_state().await;
        seed_raw_pty_running(&state, "s-concurrent-fail").await;
        // 仅失败一次：holder 的 disconnected upsert 命中 inject。
        state.workbench_session_repo.inject_fail_next_upserts(1);

        let holder_state = state.clone();
        let waiter_state = state.clone();
        let holder = tokio::spawn(async move {
            restore_persisted_sessions(&holder_state, Some("p1")).await
        });
        // 给 holder 一点时间 claim，确保 waiter 更可能走 RestoreInProgress。
        tokio::time::sleep(Duration::from_millis(5)).await;
        let waiter = tokio::spawn(async move {
            restore_persisted_sessions(&waiter_state, Some("p1")).await
        });

        let holder_res = holder.await.expect("holder join");
        let waiter_res = waiter.await.expect("waiter join");

        // 至少有一方必须失败（inject 只影响第一次 upsert）。
        assert!(
            holder_res.is_err() || waiter_res.is_err(),
            "at least one concurrent restore must fail closed"
        );

        // 若 holder 失败，错误应是 inject failure，而非 Ok 成功清单。
        if let Err(h) = &holder_res {
            assert_eq!(h.code(), "workbench_session_upsert_injected_failure");
        }
        if let Err(w) = &waiter_res {
            assert!(
                w.code() == "session_restore_shared_failed"
                    || w.code() == "workbench_session_upsert_injected_failure",
                "waiter err code unexpected: {}",
                w.code()
            );
            let _ = shared_restore_failed_error(crate::error::AppErrorCategory::Internal);
        }
        if holder_res.is_ok() && waiter_res.is_ok() {
            panic!("both succeeded; inject should have forced fail-closed");
        }

        // 无 live registry；不得留下 claim。
        assert!(!state.workbench_sessions.contains("s-concurrent-fail"));
        assert!(!state
            .workbench_sessions
            .is_restore_claim_held("s-concurrent-fail"));

        // 第三方清理后不得 AlreadyLive（spawn 已回收）。
        assert!(
            state
                .workbench_sessions
                .try_claim_restore("s-concurrent-fail")
                .is_claimed(),
            "third request after cleanup must Claimed, not AlreadyLive"
        );
        state
            .workbench_sessions
            .release_restore_claim("s-concurrent-fail");

        // 若最终 SQLite 仍 running，说明 upsert 失败且无人写 disconnected；
        // 生产路径禁止把该状态当成功 list（holder/waiter 已 Err）。
        let rows = state
            .workbench_session_repo
            .list(Some("p1"))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].status == "running" || rows[0].status == "disconnected",
            "unexpected status without body: {}",
            rows[0].status
        );
        let _ = SharedRestoreWaitResult::Failed(crate::error::AppErrorCategory::Internal);
        let _ = RestoreClaimOutcome::AlreadyLive;
    }
}
