//! Workbench 命令共享 helper/DTO。
//!
//! Business Logic（为什么需要这个模块）:
//!     子模块共用项目解析与远端映射。
//!
//! Code Logic（这个模块做什么）:
//!     monofile 前部共享定义。

use super::git::path_exists_nofollow;
use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::MutationControlError;
use crate::error::{AppError, AppErrorCategory};
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

/// Workbench 内部 integration worktree 的存放根目录。
///
/// Business Logic（为什么需要这个函数）:
///     一键 merge 的内部隔离目录必须位于应用数据目录，且与用户可见 worktree 根明确分离，
///     以便发现/对账流程稳定排除它。
///
/// Code Logic（这个函数做什么）:
///     基于 SQLite db_path 父目录返回固定 `merge-integrations` 根目录。
pub(crate) fn merge_integration_storage_root(state: &AppState) -> PathBuf {
    let config = state.config.read().expect("config 读锁中毒");
    let db_parent = Path::new(&config.db_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    db_parent.join("merge-integrations")
}

/// 为一次 merge operation 生成确定性的隔离 worktree 路径。
///
/// Business Logic（为什么需要这个函数）:
///     owner 在发布短窗口重启后必须能按同一 clientOperationId 定位并清理残留隔离目录，
///     同时不能把未经处理的 operation id 直接拼进文件系统路径。
///
/// Code Logic（这个函数做什么）:
///     对 project_id 与 operation_id 做 SHA256，使用完整十六进制摘要作为根目录下唯一子目录名。
pub(crate) fn merge_integration_storage_path(
    state: &AppState,
    project_id: &str,
    operation_id: &str,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(project_id.as_bytes());
    hasher.update([0]);
    hasher.update(operation_id.as_bytes());
    let digest = hasher.finalize();
    merge_integration_storage_root(state).join(format!("{digest:x}"))
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
///     项目载入时应把 Git 已知 worktree 同步进工作台元数据，避免只显示主工作区；
///     同时清理由外部（AI `rm -rf` / `git worktree remove` 等）删除后残留的孤儿 row，
///     否则 Workbench 会继续展示已不存在的 worktree，并在后续 git/fs 操作上报
///     `No such file or directory (os error 2)`。
///
/// Code Logic（这个函数做什么）:
///     1) `git worktree list --porcelain` 取 Git 已知 worktree；
///     2) 先删除非主且磁盘路径已不存在的既有 row（含其下 terminal session 元数据）；
///     3) 再 upsert 磁盘上确实存在的发现项（rm -rf 后 porcelain 可能仍列出 prunable
///        worktree，必须按磁盘存在性二次过滤，避免把刚删的 row 又加回来）。
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

    // 对账删除：非主 worktree 的磁盘路径已被外部删除时，row 成为孤儿。先于 upsert 删除，
    // 避免后续 porcelain 仍列出 prunable worktree 时把孤儿重新登记。session 必须走
    // close_sessions_for_worktree 完整级联（registry close + kill backend + 删行），
    // 与 merge/remove 路径保持一致；只删 SQLite 行会让 registry live overlay 把仍存活的
    // session 重新暴露为 running，形成计入统计但任何 worktree 视图都不可见的幽灵终端。
    // main worktree 永不在此删除（项目根丢失应走移除项目）。
    for row in &existing_rows {
        if row.is_main {
            continue;
        }
        if !path_exists_nofollow(Path::new(&row.path))? {
            if let Err(error) = crate::commands::workbench::git::close_sessions_for_worktree(
                state,
                &row.project_id,
                &row.id,
            )
            .await
            {
                // 对账是项目加载路径，单个 session 的 runtime close/kill 失败不应阻断
                // 整个 sync；降级为仅清理元数据（原行为）并留日志，等待下次对账收敛。
                tracing::warn!(
                    project_id = %row.project_id,
                    worktree_id = %row.id,
                    error = %error,
                    "外部删除 worktree 对账关闭 session 失败，降级仅清理元数据"
                );
                let _ = state
                    .workbench_session_repo
                    .delete_by_worktree(&row.project_id, &row.id)
                    .await;
            }
            state.workbench_worktree_repo.delete(&row.id).await?;
        }
    }

    let integration_root = merge_integration_storage_root(state);
    let normalized_integration_root = integration_root
        .canonicalize()
        .unwrap_or_else(|_| integration_root.clone());
    for item in parsed.into_iter().filter(|item| !item.is_main) {
        let item_path = normalize_worktree_path(&item.path);
        // merge integration worktree 是内部 detached 临时目录，永不写入 SQLite 或显示给用户。
        let item_path_buf = PathBuf::from(&item_path);
        let normalized_item_path = item_path_buf
            .canonicalize()
            .unwrap_or_else(|_| item_path_buf.clone());
        if normalized_item_path.starts_with(&normalized_integration_root) {
            continue;
        }
        // rm -rf 后 Git 元数据残留，porcelain 可能仍列出该 worktree；磁盘不存在则跳过 upsert。
        if !path_exists_nofollow(Path::new(&item_path))? {
            continue;
        }
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
///     主工作区 Merge 按钮要在存在可收集本地分支时点亮；资格需要 sibling worktree
///     占用信息，不能塞进 git status 解析。
///
/// Code Logic（这个函数做什么）:
///     对每个 `is_main` DTO 探测 home、占用分支和可收集分支；成功则写入
///     `can_collect_merge` / `home_branch` / `collectible_branches`。
///     任一探测失败保留默认值，不让整个 list/get 失败。功能 worktree 保持默认。
pub(crate) fn apply_collect_merge_eligibility(dtos: &mut [WorkbenchWorktreeDto]) {
    for dto in dtos.iter_mut() {
        if !dto.is_main {
            continue;
        }
        let path = Path::new(&dto.path);
        let Ok(home) = workbench_git::detect_home_branch(path) else {
            continue;
        };
        let Ok(repo_root) = workbench_git::repo_root(path) else {
            continue;
        };
        let Ok(occupied) = workbench_git::occupied_worktree_branches(Path::new(&repo_root), path)
        else {
            continue;
        };
        let Ok(collectible) = workbench_git::list_collectible_branches(path, &home, &occupied)
        else {
            continue;
        };
        dto.home_branch = Some(home);
        dto.collectible_branches = collectible.into_iter().map(|item| item.name).collect();
        dto.can_collect_merge = !dto.collectible_branches.is_empty();
    }
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
///     R14/R18 M1 / R21 M2：仍在 restore claim 中的持久行、Provisional/Flushing runtime
///     都不得当作可立即 replay 的会话返回（Ready 前不可对外暴露 live）。
///
/// Code Logic（这个函数做什么）:
///     先把持久化 row 投影为 DTO（跳过 claim-held 与 runtime 非 Ready 的 id），
///     再用 registry `list` 的实时 DTO 覆盖（registry.list 仅 Ready 且非 claim-held），
///     live overlay 再双重跳过 claim-held 以防竞态。
pub(crate) async fn merged_session_dtos(
    state: &AppState,
    project_id: Option<&str>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    use crate::workbench::sessions::SessionRuntimePresence;
    let mut sessions: Vec<WorkbenchSessionDto> = state
        .workbench_session_repo
        .list(project_id)
        .await?
        .iter()
        .filter(|row| {
            // R21 M2：Provisional/Flushing runtime 不得靠 SQLite running 行伪装 Live。
            if state.workbench_sessions.is_restore_claim_held(&row.id) {
                return false;
            }
            !matches!(
                state.workbench_sessions.runtime_presence(&row.id),
                SessionRuntimePresence::RestoreInProgress
            )
        })
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
///     **R30 M1**：`Claimed` 后、spawn / disconnected upsert / worktree resolve 前
///     `workbench_session_repo.get` re-read durable；缺失（list 快照与 concurrent close
///     竞态）finish `PersistedDisconnected` 并 continue，**禁止**用 stale 快照复活已删 tab；
///     存在则用 re-read 行继续后续 restore（保留 R24–R29 barrier/lease/claim generation）。
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
    for snapshot in rows {
        // Finding 5 + R14/R16: 原子占位，区分 AlreadyLive / RestoreInProgress / Claimed。
        let claim_generation = match state.workbench_sessions.try_claim_restore(&snapshot.id) {
            RestoreClaimOutcome::AlreadyLive => continue,
            // R24 H2：Closing barrier 下不 claim；下一次 list re-read durable（已删除则不再 restore）。
            RestoreClaimOutcome::BarrierActive => continue,
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
            RestoreClaimOutcome::Claimed { generation } => generation,
        };
        // RAII：任意 early return / Err 路径 Drop 都会 generation-scoped finish Failed。
        let mut claim_guard = crate::workbench::sessions::RestoreClaimGuard::new(
            (*state.workbench_sessions).clone(),
            snapshot.id.clone(),
            claim_generation,
        );
        // R30 M1：list 快照可能在 claim 前被 concurrent close 删除；claim 成功后 re-read durable，
        // 缺失则 finish PersistedDisconnected 且不 project/worktree/spawn/upsert，禁止复活已关 tab。
        let Some(row) = state.workbench_session_repo.get(&snapshot.id).await? else {
            claim_guard.finish(SharedRestoreNotification::PersistedDisconnected);
            continue;
        };
        // R26 M1：project remove 窗口内不得继续 restore。
        if let Err(error) = state
            .workbench_sessions
            .require_project_not_closing(&row.project_id)
        {
            claim_guard.finish(SharedRestoreNotification::Failed(
                AppErrorCategory::Unavailable,
            ));
            return Err(error);
        }
        // R28 H4：project op lease 覆盖 restore→spawn→upsert→Ready/claim finish 全窗口。
        let _project_lease = match state
            .workbench_sessions
            .try_acquire_project_op_lease(&row.project_id)
        {
            Ok(lease) => lease,
            Err(error) => {
                claim_guard.finish(SharedRestoreNotification::Failed(
                    AppErrorCategory::Unavailable,
                ));
                return Err(error);
            }
        };
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
        match state.workbench_sessions.restore(
            state.clone(),
            project,
            row.clone(),
            worktree_name,
            Some(claim_generation),
        ) {
            Ok(restored) => {
                // spawn 成功后 upsert 失败也必须回收 attach，并广播 Failed + 返回 Err。
                let mut spawn_guard = crate::workbench::sessions::SessionSpawnGuard::new_with_state(
                    (*state.workbench_sessions).clone(),
                    restored.id.clone(),
                    state.clone(),
                );
                // R26 H1：running upsert 前 acquire persist lease + revalidate claim/project。
                let mut persist_lease = match state
                    .workbench_sessions
                    .try_acquire_restore_persist_lease(&restored.id, claim_generation)
                {
                    Some(lease) => lease,
                    None => {
                        drop(spawn_guard);
                        claim_guard.finish(SharedRestoreNotification::Failed(
                            AppErrorCategory::Unavailable,
                        ));
                        return Err(AppError::unavailable(
                            "session_restore_claim_revoked".to_string(),
                        ));
                    }
                };
                if let Err(error) = state
                    .workbench_sessions
                    .require_project_not_closing(&restored.project_id)
                {
                    drop(spawn_guard);
                    persist_lease.release();
                    claim_guard.finish(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    return Err(error);
                }
                if !persist_lease.is_active() {
                    drop(spawn_guard);
                    persist_lease.release();
                    claim_guard.finish(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    return Err(AppError::unavailable(
                        "session_restore_claim_revoked".to_string(),
                    ));
                }
                match state.workbench_session_repo.upsert(&restored).await {
                    Ok(()) => {
                        persist_lease.release();
                        // R26 H1：upsert 后再 revalidate；revoked 则 reclaim 并 Failed。
                        if !claim_guard.is_active()
                            || state
                                .workbench_sessions
                                .require_project_not_closing(&restored.project_id)
                                .is_err()
                        {
                            drop(spawn_guard);
                            claim_guard.finish(SharedRestoreNotification::Failed(
                                AppErrorCategory::Unavailable,
                            ));
                            return Err(AppError::unavailable(
                                "session_restore_claim_revoked".to_string(),
                            ));
                        }
                        // R20 M1：仅 generation CAS 真正 Ready 才 finish(Ready)；否则补偿并 Failed。
                        if spawn_guard.commit() {
                            claim_guard.finish(SharedRestoreNotification::Ready);
                        } else {
                            tracing::warn!(
                                session_id = %restored.id,
                                "restore upsert 后 mark Ready CAS 失败；补偿 close 并 Failed"
                            );
                            // commit 失败时 guard 未 committed，Drop 会 close；显式 drop 先 reclaim。
                            drop(spawn_guard);
                            // 并发 close 可能已删行；best-effort 标 disconnected，避免 zombie running 行。
                            let mut disconnected = restored.clone();
                            disconnected.status = "disconnected".to_string();
                            disconnected.exited_at = Some(now_iso());
                            disconnected.updated_at = now_iso();
                            if let Some(mut lease) =
                                state.workbench_sessions.try_acquire_restore_persist_lease(
                                    &disconnected.id,
                                    claim_generation,
                                )
                            {
                                let _ = state.workbench_session_repo.upsert(&disconnected).await;
                                lease.release();
                            }
                            claim_guard.finish(SharedRestoreNotification::Failed(
                                AppErrorCategory::Unavailable,
                            ));
                            return Err(AppError::unavailable(
                                "session_restore_ready_cas_miss".to_string(),
                            ));
                        }
                    }
                    Err(error) => {
                        persist_lease.release();
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
                // R24 H2 / R26：Closing/project barrier / revoked claim 不得 upsert disconnected 复活。
                let is_close_barrier = matches!(
                    &error,
                    AppError::Unavailable(msg)
                        if msg == "session_close_barrier_active"
                            || msg == "project_closing_barrier_active"
                            || msg == "session_restore_claim_revoked"
                );
                if is_close_barrier {
                    claim_guard.finish(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    return Err(error);
                }
                let mut disconnected = row.clone();
                disconnected.status = "disconnected".to_string();
                disconnected.exited_at = Some(now_iso());
                disconnected.updated_at = now_iso();
                // R26 H1：disconnected upsert 同样需要 lease + revalidate。
                let mut persist_lease = match state
                    .workbench_sessions
                    .try_acquire_restore_persist_lease(&disconnected.id, claim_generation)
                {
                    Some(lease) => lease,
                    None => {
                        claim_guard.finish(SharedRestoreNotification::Failed(
                            AppErrorCategory::Unavailable,
                        ));
                        return Err(AppError::unavailable(
                            "session_restore_claim_revoked".to_string(),
                        ));
                    }
                };
                if let Err(error) = state
                    .workbench_sessions
                    .require_project_not_closing(&disconnected.project_id)
                {
                    persist_lease.release();
                    claim_guard.finish(SharedRestoreNotification::Failed(
                        AppErrorCategory::Unavailable,
                    ));
                    return Err(error);
                }
                match state.workbench_session_repo.upsert(&disconnected).await {
                    Ok(()) => {
                        persist_lease.release();
                        if !claim_guard.is_active() {
                            claim_guard.finish(SharedRestoreNotification::Failed(
                                AppErrorCategory::Unavailable,
                            ));
                            return Err(AppError::unavailable(
                                "session_restore_claim_revoked".to_string(),
                            ));
                        }
                        // skip-missing 已落盘 disconnected：list 可合并该行，不可 live replay。
                        claim_guard.finish(SharedRestoreNotification::PersistedDisconnected);
                    }
                    Err(persist_error) => {
                        persist_lease.release();
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
///     R36 H4：生产 ensure 不得永久 retain `__device__` watch key，否则 close 只 release session key
///     时 subscribers 永不归零，offline bridge 挡 Gap inventory。
///
/// Code Logic（这个函数做什么）:
///     用指定 device/base_url 和 inner/local projectId 调 registry，确保桥接任务存在并更新 project 映射；
///     **不**调用 `ensure_watch_subscription`（session create/list 的 ensure_session_watch 持有 lease）。
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
    // R36 H4：生产 ensure_bridge 路径不得永久 retain `__device__`；
    // 订阅仅由 session create/list 的 ensure_session_watch 持有，最后一 session close
    // 才能把 subscribers 归零并允许 idle，避免 offline bridge 永久占 Gap inventory。
}

/// Business Logic（为什么需要这个函数）:
///     id-only remote terminal 命令只有 deviceId/baseUrl，仍应确保事件桥连接但不会新增项目映射。
///
/// Code Logic（这个函数做什么）:
///     委托 registry 以 None project mapping 启动或复用该设备事件桥。
///     R36 H4：不调用 ensure_watch_subscription（session watch 才持有 lease）。
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
    // R36 H4：不 retain `__device__`；见 ensure_remote_event_bridge_for_project_mapping。
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

/// Business Logic（为什么需要这个函数）:
///     R26 M1：desktop `remove_workbench_project` 与 control `projects.remove` 必须共享
///     project-scoped closing barrier，覆盖 snapshot→bulk delete 全窗口；否则 concurrent
///     create 可在 delete 期间 spawn orphan live session 并 INSERT OR REPLACE。
///
/// Code Logic（这个函数做什么）:
///     1) begin project closing barrier generation；
///     2) drain project op leases；
///     3) re-snapshot list sessions（R28 H4）；
///     4) 逐 session close / close intent + kill_persisted_backend（R35 M3：kill Err 立即返回，不 bulk delete）；
///     5) delete sessions/worktrees/project；
///     6) finish session cleanups + wait leases + finish project barrier。
pub(crate) async fn remove_local_workbench_project_with_barrier(
    state: &AppState,
    project_id: &str,
) -> Result<serde_json::Value, AppError> {
    use crate::workbench::sessions::kill_persisted_backend;

    state.runtime_role.require_owner()?;
    let project = get_project(state, project_id).await?;
    // R40 M2：先挂 project closing barrier，再 drain/close/delete；
    // watch/project_ids 清理仅在 DB delete 成功后提交（失败路径保留 watch 与映射）。
    // remote list/reconcile 见 require_project_not_closing，避免 clear 后被 list 重新 ensure。
    let remote_device_id_for_bridge = if project.kind == "remote" {
        let device_id = project.device_id.trim();
        if device_id.is_empty() {
            None
        } else {
            Some(device_id.to_string())
        }
    } else {
        None
    };
    let project_barrier = state
        .workbench_sessions
        .begin_project_closing_barrier(project_id);

    // R28 H4：barrier 后先 drain 既有 project op leases，再 re-snapshot，禁止 create 在途 upsert 越过 bulk delete。
    if !state
        .workbench_sessions
        .wait_project_op_leases_drained(project_id)
    {
        tracing::warn!(
            project_id = %project_id,
            "project op leases still in-flight before remove snapshot; retaining project barrier"
        );
        return Err(AppError::unavailable(
            "project_op_lease_drain_timeout".to_string(),
        ));
    }

    let session_rows = state.workbench_session_repo.list(Some(project_id)).await?;
    // R25 H2 / R26 M1 / R35 M3：先收集全部 SessionCloseCleanup；
    // kill 失败立即返回 Err（不 bulk delete、不 finish_cleanup，Drop 保留 barrier）。
    let mut cleanups = Vec::new();
    for row in session_rows {
        match state.workbench_sessions.close(&row.id) {
            Ok(cleanup) => {
                if let Err(error) = kill_persisted_backend(cleanup.row()) {
                    drop(cleanups);
                    // cleanup Drop 保留 session barrier；project barrier 仍 active。
                    return Err(error);
                }
                cleanups.push(cleanup);
            }
            Err(AppError::NotFound(_)) => {
                // R25 H1：无 live handle 也 install close intent，阻断 restore re-upsert。
                match state
                    .workbench_sessions
                    .begin_close_intent_for_missing_handle(&row.id, row.clone())
                {
                    Ok(cleanup) => {
                        if let Err(error) = kill_persisted_backend(cleanup.row()) {
                            drop(cleanups);
                            return Err(error);
                        }
                        cleanups.push(cleanup);
                    }
                    Err(AppError::Conflict(_)) => {
                        if let Ok(cleanup) = state.workbench_sessions.close(&row.id) {
                            if let Err(error) = kill_persisted_backend(cleanup.row()) {
                                drop(cleanups);
                                return Err(error);
                            }
                            cleanups.push(cleanup);
                        } else if let Err(error) = kill_persisted_backend(&row) {
                            drop(cleanups);
                            return Err(error);
                        }
                    }
                    Err(_) => {
                        if let Err(error) = kill_persisted_backend(&row) {
                            drop(cleanups);
                            return Err(error);
                        }
                    }
                }
            }
            Err(_) => {
                if let Err(error) = kill_persisted_backend(&row) {
                    drop(cleanups);
                    return Err(error);
                }
            }
        }
    }

    // bulk delete 期间 project barrier 仍 active；create/restore revalidate 失败。
    let delete_result = async {
        state
            .workbench_session_repo
            .delete_by_project(project_id)
            .await?;
        state
            .workbench_worktree_repo
            .delete_by_project(project_id)
            .await?;
        state.workbench_project_note_repo.delete(project_id).await?;
        state.workbench_project_repo.delete(project_id).await?;
        Ok::<(), AppError>(())
    }
    .await;

    match delete_result {
        Ok(()) => {
            // R40 M2：仅 DB 删除成功后提交 remote watch + project_ids 映射清理。
            if let Some(device_id) = remote_device_id_for_bridge.as_deref() {
                let _ = state
                    .workbench_remote_event_bridges
                    .clear_project_running_sessions(device_id, project_id);
                let _ = state
                    .workbench_remote_event_bridges
                    .remove_project_mapping_by_local_id(device_id, project_id);
            }
            for cleanup in cleanups {
                cleanup.finish_cleanup();
            }
            // R27 H4：finish project barrier 前 wait project op leases；超时 fail-closed 保留 barrier。
            if !state
                .workbench_sessions
                .wait_project_op_leases_drained(project_id)
            {
                tracing::warn!(
                    project_id = %project_id,
                    "project op leases still in-flight after remove; retaining project barrier"
                );
                return Err(AppError::unavailable(
                    "project_op_lease_drain_timeout".to_string(),
                ));
            }
            state
                .workbench_sessions
                .finish_project_closing_barrier(project_id, project_barrier);
            Ok(serde_json::json!({ "ok": true, "projectId": project_id }))
        }
        Err(error) => {
            // delete 失败：保留 session + project barrier + remote watches/mappings，
            // 禁止 orphan create/restore 与静默丢失 live。cleanup Drop 不清 barrier（R25 M2）。
            drop(cleanups);
            Err(error)
        }
    }
}

#[cfg(test)]
pub(super) mod restore_holder_fail_closed_tests {
    //! R17 M1：生产 list/restore 路径故障注入回归。
    //! 模块与 `build_restore_fail_state` 提升为 `pub(super)`，供同级 worktree 对账测试复用最小 owner AppState fixture。

    use super::*;
    use crate::backend::authority::RuntimeRole;
    use crate::backend::event_bus::RuntimeEventBus;
    use crate::backend::runtime_metrics::RuntimeMetrics;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::cloud_sync::CloudSyncRuntime;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, DatabaseMaintenanceGate, PromptRepo, ScratchpadRepo,
        SshTargetRepo, TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo,
        WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo,
        WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use crate::updater::UpdateRuntime;
    use crate::workbench::models::{WorkbenchProjectRow, WorkbenchSessionRow};
    use crate::workbench::sessions::{
        shared_restore_failed_error, RestoreClaimGuard, RestoreClaimOutcome,
        SharedRestoreNotification, SharedRestoreWaitResult,
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
    pub(super) async fn build_restore_fail_state() -> AppState {
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
             name_source TEXT NOT NULL DEFAULT 'default', \
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
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
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
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
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
            agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
            workbench_workspace_layout_repo: Arc::new(layout_repo),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
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
            workbench_remote_events: std::sync::Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
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
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
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
            name_source: "default".to_string(),
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
        assert!(!state
            .workbench_sessions
            .is_restore_claim_held("s-holder-fail"));
        // SQLite 仍可能是 running（upsert 未成功）；不得被当作成功 list DTO。
        let rows = state.workbench_session_repo.list(Some("p1")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "running");
        // 模拟若 holder 吞掉错误继续 merge：会返回 running DTO；生产路径禁止。
        let merged = merged_session_dtos(&state, Some("p1")).await.unwrap();
        // claim 已释放，merge 会看到 stale running 行——这正是 holder 必须 return Err 的原因。
        assert!(
            merged
                .iter()
                .any(|s| s.id == "s-holder-fail" && s.status == "running"),
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
        let holder =
            tokio::spawn(
                async move { restore_persisted_sessions(&holder_state, Some("p1")).await },
            );
        // 给 holder 一点时间 claim，确保 waiter 更可能走 RestoreInProgress。
        tokio::time::sleep(Duration::from_millis(5)).await;
        let waiter =
            tokio::spawn(
                async move { restore_persisted_sessions(&waiter_state, Some("p1")).await },
            );

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
        let rows = state.workbench_session_repo.list(Some("p1")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].status == "running" || rows[0].status == "disconnected",
            "unexpected status without body: {}",
            rows[0].status
        );
        let _ = SharedRestoreWaitResult::Failed(crate::error::AppErrorCategory::Internal);
        let _ = RestoreClaimOutcome::AlreadyLive;
    }

    /// Business Logic（R30 M1: 为什么需要这个测试）:
    ///     list 一次快照后 concurrent close 可先删 durable 行；若 restore 仍用 stale 快照
    ///     spawn/upsert，已关 tab 会被复活。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed → list 快照 → delete durable → 生产 list 路径 no-op；
    ///     再模拟 claim 后 re-read：缺失则 finish PersistedDisconnected、不 upsert/spawn，
    ///     断言 id 仍不存在且无 live/claim。
    #[tokio::test]
    async fn restore_claim_reread_skips_deleted_session_without_resurrection() {
        let state = build_restore_fail_state().await;
        seed_raw_pty_running(&state, "s-race-deleted").await;

        // 模拟 restore_persisted_sessions 的 list 快照。
        let snapshot = state.workbench_session_repo.list(Some("p1")).await.unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, "s-race-deleted");

        // concurrent close 在 claim/spawn 前删掉 durable 行（barrier 已清的竞态形状）。
        state
            .workbench_session_repo
            .delete("s-race-deleted")
            .await
            .unwrap();
        assert!(state
            .workbench_session_repo
            .get("s-race-deleted")
            .await
            .unwrap()
            .is_none());

        // delete 发生在 list 之前时：生产路径 list 为空，直接 Ok，不得复活。
        restore_persisted_sessions(&state, Some("p1"))
            .await
            .expect("empty durable list restore must succeed");
        assert!(state
            .workbench_session_repo
            .get("s-race-deleted")
            .await
            .unwrap()
            .is_none());
        assert!(!state.workbench_sessions.contains("s-race-deleted"));

        // 关键竞态：stale list 快照 + claim 后 re-read（与生产 Claimed 分支一致）。
        let stale = &snapshot[0];
        let claim_generation = match state.workbench_sessions.try_claim_restore(&stale.id) {
            RestoreClaimOutcome::Claimed { generation } => generation,
            _ => panic!("expected Claimed for deleted-but-stale session id"),
        };
        let mut claim_guard = RestoreClaimGuard::new(
            (*state.workbench_sessions).clone(),
            stale.id.clone(),
            claim_generation,
        );
        // 生产：claim 后 re-read durable；缺失则 finish PersistedDisconnected 并 continue。
        let durable = state
            .workbench_session_repo
            .get(&stale.id)
            .await
            .expect("get after delete must succeed");
        assert!(
            durable.is_none(),
            "re-read after concurrent delete must observe missing durable row"
        );
        // 禁止用 stale 快照走 restore/disconnected upsert。
        claim_guard.finish(SharedRestoreNotification::PersistedDisconnected);

        assert!(state
            .workbench_session_repo
            .get("s-race-deleted")
            .await
            .unwrap()
            .is_none());
        assert!(!state.workbench_sessions.contains("s-race-deleted"));
        assert!(!state
            .workbench_sessions
            .is_restore_claim_held("s-race-deleted"));
        let after = state.workbench_session_repo.list(Some("p1")).await.unwrap();
        assert!(
            after.is_empty(),
            "deleted session must not be resurrected by stale restore snapshot"
        );
    }
}

#[cfg(test)]
mod sync_git_worktrees_external_delete_tests {
    //! 外部删除 worktree（AI `rm -rf` / `git worktree remove`）后对账回归：
    //! `sync_git_worktrees` 必须把磁盘路径已不存在的非主 worktree row 删除，并清理其下 terminal session 元数据，
    //! 否则 Workbench 仍展示孤儿 row，且后续 git/fs 操作在死路径上抛 `No such file or directory (os error 2)`。

    use super::restore_holder_fail_closed_tests::build_restore_fail_state;
    use super::*;
    use crate::workbench::models::{
        WorkbenchProjectRow, WorkbenchSessionRow, WorkbenchWorktreeRow,
    };
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// 在 cwd 下执行 git，失败即 panic 并打印 stderr。
    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("应执行 git");
        assert!(
            output.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// 构造指向给定主仓库路径的 project row（id 固定 p1，复用 fixture 已建库）。
    fn project_row_for(main_path: &Path) -> WorkbenchProjectRow {
        WorkbenchProjectRow {
            id: "p1".to_string(),
            name: "demo".to_string(),
            kind: "local".to_string(),
            device_id: "d1".to_string(),
            device_name: "local".to_string(),
            path: main_path.to_string_lossy().into_owned(),
            last_opened_at: "t".to_string(),
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        }
    }

    async fn seed_feature_worktree_row(state: &AppState, id: &str, path: &str) {
        state
            .workbench_worktree_repo
            .upsert(&WorkbenchWorktreeRow {
                id: id.to_string(),
                project_id: "p1".to_string(),
                name: "feature".to_string(),
                branch: Some("feature".to_string()),
                base_branch: Some("main".to_string()),
                path: path.to_string(),
                is_main: false,
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            })
            .await
            .unwrap();
    }

    async fn seed_session_for_worktree(state: &AppState, session_id: &str, worktree_id: &str) {
        state
            .workbench_session_repo
            .upsert(&WorkbenchSessionRow {
                id: session_id.to_string(),
                project_id: "p1".to_string(),
                worktree_id: Some(worktree_id.to_string()),
                name: "term".to_string(),
                name_source: "default".to_string(),
                command: "/bin/sh".to_string(),
                cwd: "/tmp".to_string(),
                status: "disconnected".to_string(),
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
            })
            .await
            .unwrap();
    }

    /// Business Logic（为什么需要这个测试）:
    ///     隔离 merge 可能运行数分钟；此时普通 Workbench 刷新不能把内部 detached worktree
    ///     自动发现并写入 SQLite，否则用户会看到临时工作区并可能误操作。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 fixture 的 app data 根创建真实 detached integration worktree，执行 sync_git_worktrees，
    ///     断言项目下不存在指向该路径的非主 row，随后移除临时 worktree。
    #[tokio::test]
    async fn integration_worktree_is_never_discovered_into_sqlite() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main = temp.path().join("main");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["checkout", "-b", "main"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        let data_root = temp.path().join("app-data");
        std::fs::create_dir_all(&data_root).expect("create app data");
        state.config.write().expect("config write").db_path =
            data_root.join("data.db").to_string_lossy().into_owned();
        let integration = merge_integration_storage_path(&state, "p1", "operation-1");
        std::fs::create_dir_all(integration.parent().expect("integration parent"))
            .expect("create integration root");
        let head = workbench_git::head_hash(&main)
            .expect("head")
            .expect("some head");
        workbench_git::create_detached_integration_worktree(&main, &integration, &head)
            .expect("create detached integration");

        sync_git_worktrees(&state, &project)
            .await
            .expect("对账不应失败");
        let rows = state
            .workbench_worktree_repo
            .list_by_project("p1")
            .await
            .expect("list rows");
        let integration_canonical = integration.canonicalize().expect("integration canonical");
        assert!(rows.iter().all(|row| {
            Path::new(&row.path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(&row.path))
                != integration_canonical
        }));

        workbench_git::remove_integration_worktree(&main, &integration)
            .expect("cleanup integration");
        assert!(!integration.exists());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 可能在 `git worktree remove` 已成功、SQLite worktree/session row 尚未删除时崩溃；
    ///     发布后恢复必须继续收敛 metadata，不能把缺失路径误判为源漂移并留下幽灵工作区。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建并合并真实 source worktree，在 SQLite 登记 row/session；直接用 Git 删除 source 模拟 crash 点，
    ///     再调用发布后幂等 cleanup，断言不重复 remove、source branch/session/worktree row 均被清理。
    #[tokio::test]
    async fn published_cleanup_resumes_after_git_worktree_was_already_removed() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main = temp.path().join("main");
        let source = temp.path().join("source");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["checkout", "-b", "main"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                source.to_string_lossy().as_ref(),
            ],
        );
        std::fs::write(source.join("feature.txt"), "feature\n").expect("feature");
        git(&source, &["add", "feature.txt"]);
        git(&source, &["commit", "-m", "feature"]);
        let source_oid = workbench_git::head_hash(&source)
            .expect("source head")
            .expect("some source head");
        let main_before = workbench_git::head_hash(&main)
            .expect("main head")
            .expect("some main head");
        git(&main, &["merge", "--no-ff", "feature"]);

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        seed_feature_worktree_row(&state, "wt-crash", source.to_string_lossy().as_ref()).await;
        seed_session_for_worktree(&state, "sess-crash", "wt-crash").await;
        let row = state
            .workbench_worktree_repo
            .get("wt-crash")
            .await
            .expect("get row")
            .expect("source row");

        // crash 点：Git worktree 已移除，但 branch 与 SQLite metadata 尚在。
        git(
            &main,
            &["worktree", "remove", source.to_string_lossy().as_ref()],
        );
        assert!(!source.exists());
        assert!(state
            .workbench_worktree_repo
            .get("wt-crash")
            .await
            .expect("row before recovery")
            .is_some());

        let frozen = workbench_git::FrozenWorkbenchMerge {
            main_branch: "main".to_string(),
            main_oid: main_before,
            source_oid,
        };
        let cleaned = crate::commands::workbench::git::cleanup_published_source_if_unchanged(
            &state, &project, &row, &frozen,
        )
        .await
        .expect("resume cleanup");

        assert!(cleaned);
        assert!(state
            .workbench_worktree_repo
            .get("wt-crash")
            .await
            .expect("row after recovery")
            .is_none());
        assert!(state
            .workbench_session_repo
            .get("sess-crash")
            .await
            .expect("session after recovery")
            .is_none());
        let branch = Command::new("git")
            .args(["show-ref", "--verify", "--quiet", "refs/heads/feature"])
            .current_dir(&main)
            .status()
            .expect("query branch");
        assert!(!branch.success(), "已合并 source branch 应被幂等清理");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     AI 在 cc-partner 外部 `rm -rf` linked worktree 后，SQLite 里的 row 成为孤儿。
    ///     `sync_git_worktrees` 必须在下次对账时删掉该 row 及其下 terminal session，
    ///     且不能因 git porcelain 仍列出 prunable worktree 而重新登记。
    ///
    /// Code Logic（这个测试做什么）:
    ///     建 main + linked worktree → 登记到 SQLite + 关联 session → rm -rf linked →
    ///     调 sync_git_worktrees → 断言非主 worktree row 全部消失、关联 session 被清理。
    #[tokio::test]
    async fn external_rm_rf_linked_worktree_prunes_row_and_sessions() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main: PathBuf = temp.path().join("main");
        let linked: PathBuf = temp.path().join("linked");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                linked.to_string_lossy().as_ref(),
            ],
        );

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        seed_feature_worktree_row(&state, "wt-feat", linked.to_string_lossy().as_ref()).await;
        seed_session_for_worktree(&state, "sess-feat", "wt-feat").await;

        // 模拟 AI 外部删除：直接 rm -rf linked 目录（git 元数据残留，porcelain 可能仍列出）。
        std::fs::remove_dir_all(&linked).expect("应删除 linked worktree 目录");

        sync_git_worktrees(&state, &project)
            .await
            .expect("对账不应失败");

        let remaining: Vec<_> = state
            .workbench_worktree_repo
            .list_by_project("p1")
            .await
            .unwrap()
            .into_iter()
            .filter(|row| !row.is_main)
            .collect();
        assert!(
            remaining.is_empty(),
            "外部删除后孤儿 worktree row 必须被对账删除，实际残留: {:?}",
            remaining
                .iter()
                .map(|row| (row.id.clone(), row.path.clone()))
                .collect::<Vec<_>>()
        );

        let session = state
            .workbench_session_repo
            .get("sess-feat")
            .await
            .expect("session 查询不应失败");
        assert!(
            session.is_none(),
            "被删 worktree 下的 terminal session 元数据必须一并清理"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     外部删除的 worktree 下可能仍有 running session：registry 里挂着 live handle、
    ///     tmux backend 仍在运行。对账若只删 SQLite 行而不 close runtime handle，
    ///     `merged_session_dtos` 的 live overlay 会把该 session 重新暴露为 running，
    ///     形成"计入项目统计但任何 worktree 视图都不可见"的幽灵终端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     建 main + linked worktree → SQLite 登记 worktree row + session row → registry 插入
    ///     同 id 的 fake live handle → rm -rf linked → 调 sync_git_worktrees → 断言
    ///     SQLite session 行已删且 registry handle 已被关闭（再次 close 返回 NotFound）。
    #[tokio::test]
    async fn external_rm_rf_closes_live_session_runtime_handles() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main: PathBuf = temp.path().join("main");
        let linked: PathBuf = temp.path().join("linked");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                linked.to_string_lossy().as_ref(),
            ],
        );

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        seed_feature_worktree_row(&state, "wt-live", linked.to_string_lossy().as_ref()).await;
        seed_session_for_worktree(&state, "sess-live", "wt-live").await;
        // registry 中挂一个同 id 的 live handle（Fake process，不依赖 tmux），
        // 模拟该 worktree 下终端 window 仍在运行的真实场景。
        state
            .workbench_sessions
            .insert_fake_session_row_for_test(WorkbenchSessionRow {
                id: "sess-live".to_string(),
                project_id: "p1".to_string(),
                worktree_id: Some("wt-live".to_string()),
                name: "term".to_string(),
                name_source: "default".to_string(),
                command: "/bin/sh".to_string(),
                cwd: "/tmp".to_string(),
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
            });

        // 模拟 AI 外部删除：rm -rf linked 目录。
        std::fs::remove_dir_all(&linked).expect("应删除 linked worktree 目录");

        sync_git_worktrees(&state, &project)
            .await
            .expect("对账不应失败");

        assert!(
            state
                .workbench_session_repo
                .get("sess-live")
                .await
                .expect("session 查询不应失败")
                .is_none(),
            "外部删除 worktree 下的 session 元数据必须清理"
        );
        match state.workbench_sessions.close("sess-live") {
            Err(AppError::NotFound(_)) => {}
            Ok(cleanup) => {
                cleanup.finish_cleanup();
                panic!("对账必须关闭外部删除 worktree 下 live session 的 runtime handle，而不是只删 SQLite 行");
            }
            Err(other) => panic!("再次 close 应返回 NotFound，实际: {other}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对账删除必须只针对磁盘已不存在的 worktree；活 worktree 及其 session 不能被误删。
    ///
    /// Code Logic（这个测试做什么）:
    ///     linked worktree 保持存在 → 调 sync_git_worktrees → 断言 row 仍在（可能复用 wt-feat
    ///     或被 porcelain 发现项合并，但 project 下必须有指向该路径的非主 row）、session 仍在。
    #[tokio::test]
    async fn existing_linked_worktree_is_not_pruned() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main: PathBuf = temp.path().join("main");
        let linked: PathBuf = temp.path().join("linked");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                linked.to_string_lossy().as_ref(),
            ],
        );

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        seed_feature_worktree_row(&state, "wt-feat", linked.to_string_lossy().as_ref()).await;
        seed_session_for_worktree(&state, "sess-live", "wt-feat").await;

        // 不删除 linked 目录。
        sync_git_worktrees(&state, &project)
            .await
            .expect("对账不应失败");

        let non_main: Vec<_> = state
            .workbench_worktree_repo
            .list_by_project("p1")
            .await
            .unwrap()
            .into_iter()
            .filter(|row| !row.is_main)
            .collect();
        assert!(
            !non_main.is_empty(),
            "活 worktree 不应被对账清空，实际非主 row: {:?}",
            non_main
                .iter()
                .map(|row| (row.id.clone(), row.path.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            state
                .workbench_session_repo
                .get("sess-live")
                .await
                .expect("session 查询不应失败")
                .is_some(),
            "活 worktree 下的 session 元数据必须保留"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     孤儿 row 指向的路径可能从未存在（历史脏数据 / 路径迁移），对账同样必须清理。
    ///     此用例与 rm -rf 用例互补：不依赖 porcelain 是否列出，仅靠磁盘存在性判定。
    ///
    /// Code Logic（这个测试做什么）:
    ///     只建 main 仓库（无 linked worktree）→ 登记一条指向不存在路径的 row →
    ///     调 sync_git_worktrees → 断言该 row 被删除。
    #[tokio::test]
    async fn stale_row_pointing_at_missing_path_is_pruned() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main: PathBuf = temp.path().join("main");
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        git(&main, &["init"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["config", "user.email", "test@example.com"]);
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "-m", "init"]);

        let state = build_restore_fail_state().await;
        let project = project_row_for(&main);
        let bogus = temp.path().join("never-existed");
        seed_feature_worktree_row(&state, "wt-bogus", bogus.to_string_lossy().as_ref()).await;

        sync_git_worktrees(&state, &project)
            .await
            .expect("对账不应失败");

        assert!(
            state
                .workbench_worktree_repo
                .get("wt-bogus")
                .await
                .expect("worktree 查询不应失败")
                .is_none(),
            "指向不存在路径的孤儿 row 必须被对账删除"
        );
    }
}
