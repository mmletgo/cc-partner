//! worktree/Git
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 中的本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helper。

use crate::claude_cli;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::models::{
    WorkbenchDetectedFileType, WorkbenchGitCommitDto, WorkbenchOpenFileDto, WorkbenchProjectRow,
    WorkbenchTextContent, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use crate::workbench::sessions::kill_persisted_backend;
use crate::workbench::{
    file_content, file_preview, git as workbench_git,
    remote_client::RemoteWorkbenchClient,
    remote_events::{
        publish_workbench_remote_event_from_state, WorkbenchMergeProgressPayload,
        WorkbenchRemoteEvent,
    },
    remote_ids::remote_entity_id,
    remote_protocol::{RemoteCommitWorktreeReq, RemoteCreateWorktreeReq},
    sqlite_preview,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use tauri::State;

use super::common::*;
use super::projects::list_workbench_worktrees_for_state;

/// 列出项目下的 Git worktree。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Workbench 顶部需要列出本机或远端项目的主工作区和功能 worktree。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 只解包 State，再委托可供 HTTP mobile route 复用的 for_state helper。
#[tauri::command]
pub async fn list_workbench_worktrees(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.list", serde_json::json!({ "projectId": project_id.clone() })).await? {
        return Ok(v);
    }
    list_workbench_worktrees_for_state(state.inner(), project_id).await
}

/// 创建一个项目 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     用户希望在 Workbench 中直接从当前项目切出独立工作区，后续 terminal window、文件树和 Prompt 优化都绑定该路径。
///     Orchestrator Runner 在 Preparing 崩溃后 Retry 会再次使用相同确定性路径；若路径/元数据已属于本分支，
///     必须复用既有现场，否则永久 Blocked 并留下孤儿 worktree。
///
/// Code Logic（这个函数做什么）:
///     校验 Git 仓库和分支名，生成应用数据目录下的 worktree 路径；
///     若路径已存在，必须经 `git worktree list --porcelain` 确认 canonical path、owning repo、
///     实际分支全部匹配后才复用（拒绝 symlink/残留普通目录；分支不匹配 → conflict）；
///     有匹配 DB row 则复用其 id，否则重新登记；否则执行 `git worktree add -b` 并持久化 row。
pub(crate) async fn local_create_workbench_worktree(
    state: &AppState,
    project_id: String,
    branch_name: String,
    base_branch: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let branch = branch_name.trim();
    if branch.is_empty() {
        return Err(AppError::generic("分支名不能为空"));
    }
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    let worktree_path = worktree_storage_path(state, &project_id, branch);
    let base = base_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // Preparing 崩溃恢复 / Retry：路径存在时仅当 Git 注册 + 分支匹配才复用，禁止 is_dir 冒充。
    if path_exists_nofollow(&worktree_path)? {
        let verified = workbench_git::verify_registered_worktree(
            Path::new(&repo_root),
            &worktree_path,
            branch,
        )?;
        let now = now_iso();
        let path = verified.to_string_lossy().to_string();
        if let Some(mut existing) =
            find_reusable_worktree_row(state, &project_id, &verified, branch).await?
        {
            // 校正 path 为 canonical，避免历史相对/非规范路径。
            if existing.path != path {
                existing.path = path;
                existing.updated_at = now;
                state.workbench_worktree_repo.upsert(&existing).await?;
            }
            return Ok(worktree_to_dto(&existing));
        }
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
        return Ok(worktree_to_dto(&row));
    }

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
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

/// Business Logic（为什么需要这个函数）:
///     exists() 会跟随 symlink；复用决策前必须 no-follow 判断路径是否存在目录项。
///
/// Code Logic（这个函数做什么）:
///     symlink_metadata：Ok→存在；NotFound→不存在；其它 IO 错误上抛。
pub(crate) fn path_exists_nofollow(path: &Path) -> Result<bool, AppError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(AppError::from(err)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     Git 已确认可复用后，优先复用同 project+path 的既有 DB row，保持 worktree id 稳定。
///
/// Code Logic（这个函数做什么）:
///     列出 project 下 worktree，按规范化 path 匹配；要求 branch 与请求分支一致（或 DB branch 为空时用 name 兜底）。
pub(crate) async fn find_reusable_worktree_row(
    state: &AppState,
    project_id: &str,
    worktree_path: &Path,
    branch: &str,
) -> Result<Option<WorkbenchWorktreeRow>, AppError> {
    let expected = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let expected_str = expected.to_string_lossy();
    let rows = state
        .workbench_worktree_repo
        .list_by_project(project_id)
        .await?;
    for row in rows {
        if row.is_main {
            continue;
        }
        let row_path = Path::new(&row.path);
        let row_canon = row_path
            .canonicalize()
            .unwrap_or_else(|_| row_path.to_path_buf());
        if row_canon != expected
            && row.path != expected_str
            && row.path != worktree_path.to_string_lossy()
        {
            continue;
        }
        let row_branch = row
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(row.name.as_str());
        if row_branch == branch {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

/// 创建一个项目 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     用户希望在 Workbench 中直接从当前项目切出独立工作区，后续 terminal window、文件树和 Prompt 优化都绑定该路径。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把创建请求转发到远端设备并映射返回 ID；local 项目走原有本机 helper。
pub(crate) async fn create_workbench_worktree_for_state(
    state: &AppState,
    project_id: String,
    branch_name: String,
    base_branch: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let item = RemoteWorkbenchClient::new()
            .create_worktree(
                &context.base_url,
                RemoteCreateWorktreeReq {
                    project_id: context.inner_project_id,
                    branch_name,
                    base_branch,
                },
            )
            .await?;
        return map_remote_worktree_dtos(&context.device_id, &context.local_project_id, vec![item])
            .into_iter()
            .next()
            .ok_or_else(|| AppError::generic("远端 worktree 创建结果为空"));
    }
    local_create_workbench_worktree(state, project_id, branch_name, base_branch).await
}

/// 创建一个项目 Git worktree。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要从 Workbench 新建本机或远端项目的功能 worktree。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 只解包参数，再委托 for_state helper 与手机 HTTP 入口共享业务语义。
#[tauri::command]
pub async fn create_workbench_worktree(
    state: State<'_, AppState>,
    project_id: String,
    branch_name: String,
    base_branch: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.create", serde_json::json!({ "projectId": project_id.clone(), "branchName": branch_name.clone(), "baseBranch": base_branch.clone() })).await? {
        return Ok(v);
    }
    create_workbench_worktree_for_state(state.inner(), project_id, branch_name, base_branch).await
}

/// 提交当前 worktree 的全部改动。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要在 Workbench 中点击 Commit 后，由 Claude Code 根据项目上下文和 staged diff 生成提交信息并提交。
///
/// Code Logic（这个函数做什么）:
///     message 为空时 stage 全部改动、读取 staged diff、在 worktree cwd 下调用 Claude Code 生成 message 后提交；
///     message 非空时保留手写 message 兼容路径；无改动时返回最新 DTO，让前端刷新 stale 状态。
pub(crate) async fn local_commit_workbench_worktree(
    state: &AppState,
    worktree_id: String,
    message: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    state.runtime_role.require_owner()?;
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
        None => commit_worktree_with_generated_message(state, path).await?,
    };
    if !committed {
        return Ok(worktree_to_dto(&row));
    }
    Ok(worktree_to_dto(&row))
}

/// 提交当前 worktree 的全部改动。
///
/// Business Logic（为什么需要这个函数）:
///     本机 worktree 直接提交，remote worktree 必须转发到项目所在设备，不能误查本机 SQLite。
///
/// Code Logic（这个函数做什么）:
///     先按 worktreeId 解析 Local/Remote；Remote 通过 HTTP commit 后把返回 DTO 的 project/worktree id 映射回本机。
pub(crate) async fn commit_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
    message: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let item = RemoteWorkbenchClient::new()
                .commit_worktree(
                    &context.base_url,
                    RemoteCommitWorktreeReq {
                        worktree_id: context.inner_worktree_id,
                        message,
                    },
                )
                .await?;
            map_remote_worktree_dtos(&context.device_id, &context.local_project_id, vec![item])
                .into_iter()
                .next()
                .ok_or_else(|| AppError::generic("远端 worktree commit 结果为空"))
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_commit_workbench_worktree(state, local_worktree_id, message).await
        }
    }
}

/// 提交当前 worktree 的全部改动。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Commit 按钮需要提交本机或远端项目 worktree 的全部改动。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 只解包参数，再委托 for_state helper 复用 remote-aware 行为。
#[tauri::command]
pub async fn commit_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    message: Option<String>,
) -> Result<WorkbenchWorktreeDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.commit", serde_json::json!({ "worktreeId": worktree_id.clone(), "message": message.clone() })).await? {
        return Ok(v);
    }
    commit_workbench_worktree_for_state(state.inner(), worktree_id, message).await
}

/// Business Logic（为什么需要这个函数）:
///     Commit 按钮应自动根据当前 staged diff 生成 commit message，且让 Claude Code 读取项目上下文。
///
/// Code Logic（这个函数做什么）:
///     stage 全部改动；无改动返回 false；有改动时读取 diff，使用配置里的 Claude CLI 路径和模型，
///     在 worktree cwd 下执行项目上下文 headless JSON 调用，清洗 message 后提交 staged 内容。
pub(crate) async fn commit_worktree_with_generated_message(
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
pub(crate) fn workbench_commit_message_schema() -> Value {
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
pub(crate) fn build_commit_message_instruction(
    changes: &workbench_git::StagedCommitChanges,
) -> String {
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
pub(crate) async fn local_push_workbench_worktree(
    state: &AppState,
    worktree_id: String,
) -> Result<WorkbenchWorktreeDto, AppError> {
    state.runtime_role.require_owner()?;
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

/// 推送当前 worktree 分支。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 的 push 必须发生在远端设备，否则本机没有对应 worktree repo。
///
/// Code Logic（这个函数做什么）:
///     先解析 worktree 目标；Remote 通过 HTTP push 并映射返回 DTO，Local 复用本地 helper。
pub(crate) async fn push_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
) -> Result<WorkbenchWorktreeDto, AppError> {
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let item = RemoteWorkbenchClient::new()
                .push_worktree(&context.base_url, &context.inner_worktree_id)
                .await?;
            map_remote_worktree_dtos(&context.device_id, &context.local_project_id, vec![item])
                .into_iter()
                .next()
                .ok_or_else(|| AppError::generic("远端 worktree push 结果为空"))
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_push_workbench_worktree(state, local_worktree_id).await
        }
    }
}

/// 推送当前 worktree 分支。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Push 按钮需要推送本机或远端项目的 active worktree 分支。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 worktreeId 后委托 for_state helper。
#[tauri::command]
pub async fn push_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
) -> Result<WorkbenchWorktreeDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.push", serde_json::json!({ "worktreeId": worktree_id.clone() })).await? {
        return Ok(v);
    }
    push_workbench_worktree_for_state(state.inner(), worktree_id).await
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
pub(crate) async fn local_merge_workbench_worktree(
    state: &AppState,
    worktree_id: String,
) -> Result<WorkbenchMergeResultDto, AppError> {
    state.runtime_role.require_owner()?;
    let mut stages = initial_merge_stages();

    let row = match state.workbench_worktree_repo.get(&worktree_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(AppError::not_found("工作台 worktree 不存在")),
        Err(error) => return Err(error),
    };
    let project_id = row.project_id.clone();
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "running",
        "正在检查源 worktree 状态",
    );
    if row.is_main {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CHECK_SOURCE,
            AppError::generic("主工作区不需要合并到自己"),
        ));
    }
    let project = stage_result(
        get_project(state, &row.project_id).await,
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    let main = stage_result(
        ensure_main_worktree(state, &project).await,
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    let source_status = stage_result(
        workbench_git::status(Path::new(&row.path)),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    if !source_status.clean {
        return Err(fail_merge_stage(
            state,
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
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "completed",
        "源 worktree 已确认干净",
    );

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
        "running",
        "正在关闭该 worktree 下的终端窗口",
    );
    let closed_sessions = stage_result(
        close_sessions_for_worktree(state, &row.project_id, &row.id).await,
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
    )?;
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
        "completed",
        format!("已关闭 {closed_sessions} 个终端窗口"),
    );

    set_merge_stage(
        state,
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
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    if !main_status.clean {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            AppError::generic("主工作区有未提交改动，请先提交或清理后再合并"),
        ));
    }
    let merge_outcome = stage_result(
        workbench_git::merge_branch(main_path, &branch),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    match merge_outcome {
        workbench_git::MergeBranchOutcome::Merged => {
            set_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "主工作区 merge 已完成",
            );
            set_merge_stage(
                state,
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
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "merge 出现冲突，进入自动解决阶段",
            );
            set_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_RESOLVE_CONFLICTS,
                "running",
                "正在调用 Claude Code 尝试解决 merge 冲突",
            );
            if let Err(error) = resolve_merge_conflicts_with_claude(state, main_path).await {
                let message = abort_merge_after_failed_resolution(main_path, &error);
                return Err(fail_merge_stage(
                    state,
                    &project_id,
                    &worktree_id,
                    &mut stages,
                    MERGE_STAGE_RESOLVE_CONFLICTS,
                    AppError::generic(message),
                ));
            }
            set_merge_stage(
                state,
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
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "running",
        "正在删除 worktree 元数据、磁盘工作区和已合并分支",
    );
    stage_result(
        cleanup_merged_worktree(state, &project, &row).await,
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
    )?;
    set_merge_stage(
        state,
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

/// 合并当前 worktree 到主工作区。
///
/// Business Logic（为什么需要这个函数）:
///     remote worktree 合并必须在项目所在设备执行，且 merge progress 要能被本机 remote shortcut 页面接收。
///
/// Code Logic（这个函数做什么）:
///     先解析 worktree 目标；Remote 先建立事件桥和项目映射，再调用远端 merge 并映射返回 worktreeId。
pub(crate) async fn merge_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
) -> Result<WorkbenchMergeResultDto, AppError> {
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let value = RemoteWorkbenchClient::new()
                .merge_worktree(&context.base_url, &context.inner_worktree_id)
                .await?;
            map_remote_merge_result_value(&context.device_id, value)
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_merge_workbench_worktree(state, local_worktree_id).await
        }
    }
}

/// 合并当前 worktree 到主工作区。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Merge 按钮需要合并本机或远端项目 worktree，并接收阶段进度。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 只解包 State，再委托 for_state helper。
#[tauri::command]
pub async fn merge_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
) -> Result<WorkbenchMergeResultDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.merge", serde_json::json!({ "worktreeId": worktree_id.clone() })).await? {
        return Ok(v);
    }
    merge_workbench_worktree_for_state(state.inner(), worktree_id).await
}

/// Business Logic（为什么需要这个函数）:
///     远端 merge result 返回的是对端本机 worktreeId，本机前端需要继续使用 remote worktreeId。
///
/// Code Logic（这个函数做什么）:
///     修改 JSON 中的 `worktreeId` 为 `remote:<deviceId>:<inner>` 后，反序列化为本机命令返回 DTO。
pub(crate) fn map_remote_merge_result_value(
    device_id: &str,
    mut value: Value,
) -> Result<WorkbenchMergeResultDto, AppError> {
    if let Some(worktree_id) = value.get("worktreeId").and_then(Value::as_str) {
        value["worktreeId"] = Value::String(remote_entity_id(device_id, worktree_id));
    }
    serde_json::from_value(value)
        .map_err(|error| AppError::generic(format!("远端 merge 结果解析失败: {error}")))
}

/// Business Logic（为什么需要这个函数）:
///     merge 命令和进度事件都需要同一份固定阶段列表，避免前端收到未知或缺失阶段。
///
/// Code Logic（这个函数做什么）:
///     按前端约定的五个 stage id 生成 pending 初始状态。
pub(crate) fn initial_merge_stages() -> Vec<WorkbenchMergeStageDto> {
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
pub(crate) fn set_merge_stage(
    state: &AppState,
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
    publish_workbench_remote_event_from_state(
        state,
        WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
            project_id: project_id.to_string(),
            worktree_id: worktree_id.to_string(),
            stage: serde_json::to_value(stage.clone()).unwrap_or(Value::Null),
        }),
    );
    state.emit_event("workbench:merge-progress", event);
}

/// Business Logic（为什么需要这个函数）:
///     merge 阶段内部任一错误都应先通知前端 failed stage，再通过 Tauri command 返回 AppError。
///
/// Code Logic（这个函数做什么）:
///     将 Result::Err 映射为 fail_merge_stage，Result::Ok 原样返回。
pub(crate) fn stage_result<T>(
    result: Result<T, AppError>,
    state: &AppState,
    project_id: &str,
    worktree_id: &str,
    stages: &mut [WorkbenchMergeStageDto],
    stage_id: &str,
) -> Result<T, AppError> {
    result
        .map_err(|error| fail_merge_stage(state, project_id, worktree_id, stages, stage_id, error))
}

/// Business Logic（为什么需要这个函数）:
///     失败路径需要统一把真实错误消息同步到进度事件，前端才能在对应阶段展示可读失败原因。
///
/// Code Logic（这个函数做什么）:
///     把 stage 标记为 failed 并返回原 AppError，保持命令错误语义不变。
pub(crate) fn fail_merge_stage(
    state: &AppState,
    project_id: &str,
    worktree_id: &str,
    stages: &mut [WorkbenchMergeStageDto],
    stage_id: &str,
    error: AppError,
) -> AppError {
    let message = error.to_string();
    set_merge_stage(
        state,
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
pub(crate) async fn close_sessions_for_worktree(
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
pub(crate) async fn cleanup_merged_worktree(
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
pub(crate) async fn resolve_merge_conflicts_with_claude(
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
pub(crate) fn abort_merge_after_failed_resolution(main_path: &Path, error: &AppError) -> String {
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
pub(crate) fn read_merge_conflict_files(
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
pub(crate) fn apply_merge_resolution_files(
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
pub(crate) fn ensure_conflict_markers_removed(
    root: &Path,
    paths: &[String],
) -> Result<(), AppError> {
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
pub(crate) fn merge_conflict_resolution_schema() -> Value {
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
pub(crate) fn build_merge_conflict_resolution_instruction(
    files: &[MergeConflictFileInput],
) -> String {
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
pub(crate) fn validate_merge_resolution_path(path: &str) -> Result<(), AppError> {
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
pub(crate) fn safe_merge_resolution_path(root: &Path, path: &str) -> Result<PathBuf, AppError> {
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
pub(crate) fn content_has_conflict_markers(content: &str) -> bool {
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
pub(crate) async fn local_remove_workbench_worktree(
    state: &AppState,
    worktree_id: String,
    force: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
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
    let project = get_project(state, &row.project_id).await?;
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    workbench_git::remove_worktree(
        Path::new(&repo_root),
        Path::new(&row.path),
        force.unwrap_or(false),
    )?;
    state.workbench_worktree_repo.delete(&worktree_id).await?;
    Ok(serde_json::json!({ "ok": true, "worktreeId": worktree_id }))
}

/// 删除一个非主 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     remote shortcut 删除 worktree 时，真实磁盘和 Git metadata 清理必须发生在远端设备。
///
/// Code Logic（这个函数做什么）:
///     先解析 Local/Remote 目标；Remote 通过 HTTP remove，并把返回 JSON 的 worktreeId 映射回 remote id。
pub(crate) async fn remove_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
    force: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let value = RemoteWorkbenchClient::new()
                .remove_worktree(&context.base_url, &context.inner_worktree_id, force)
                .await?;
            Ok(map_remote_worktree_json_value(&context.device_id, value))
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_remove_workbench_worktree(state, local_worktree_id, force).await
        }
    }
}

/// 删除一个非主 worktree。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要删除本机或远端项目中不再需要的非主 worktree。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn remove_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    force: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "worktrees.remove", serde_json::json!({ "worktreeId": worktree_id.clone(), "force": force.clone() })).await? {
        return Ok(v);
    }
    remove_workbench_worktree_for_state(state.inner(), worktree_id, force).await
}

/// Business Logic（为什么需要这个函数）:
///     远端轻量 JSON 响应可能包含 worktreeId，本机前端不能收到裸远端 inner ID。
///
/// Code Logic（这个函数做什么）:
///     若 JSON object 中有 string `worktreeId`，则替换为 `remote:<deviceId>:<inner>`；其他字段原样保留。
pub(crate) fn map_remote_worktree_json_value(device_id: &str, mut value: Value) -> Value {
    if let Some(worktree_id) = value.get("worktreeId").and_then(Value::as_str) {
        value["worktreeId"] = Value::String(remote_entity_id(device_id, worktree_id));
    }
    value
}

/// 列出当前 worktree 的最近 Git 提交。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 右侧 Git 历史 tab 需要展示 active worktree 的提交历史，辅助用户确认 commit/merge 结果。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 根路径，按 limit 调用 `git log` helper；limit 默认 30，最大 100。
pub(crate) async fn local_list_workbench_git_commits(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let limit = limit.unwrap_or(30).clamp(1, 100);
    workbench_git::list_commits(Path::new(&worktree.path), limit)
}

/// 列出当前 worktree 的最近 Git 提交。
///
/// Business Logic（为什么需要这个函数）:
///     Workbench 右侧 Git 历史 tab 需要展示 active worktree 的提交历史，辅助用户确认 commit/merge 结果。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把 worktreeId 去掉远端前缀后转发；local 项目走原有本机 Git helper。
pub(crate) async fn list_workbench_git_commits_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        let limit = limit.unwrap_or(30).clamp(1, 100) as i64;
        return RemoteWorkbenchClient::new()
            .list_git_commits(
                &context.base_url,
                &context.inner_project_id,
                inner_worktree_id.as_deref(),
                limit,
            )
            .await;
    }
    local_list_workbench_git_commits(state, project_id, worktree_id, limit).await
}

/// 列出当前 worktree 的最近 Git 提交。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Git 历史面板需要读取本机或远端 active worktree 的提交。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn list_workbench_git_commits(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "git.commits", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "limit": limit.clone() })).await? {
        return Ok(v);
    }
    list_workbench_git_commits_for_state(state.inner(), project_id, worktree_id, limit).await
}

/// 打开当前 worktree 内的文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区需要一次拿到文件 metadata、类型能力和可用的内容/预览数据，供前端打开 tab。
///
/// Code Logic（这个函数做什么）:
///     解析 project/worktree 和安全文件路径，按后端检测类型分发到文本（含 Markdown/HTML）、图片、CSV 或 SQLite 预览；
///     内容超限、非 UTF-8 或预览失败时返回 notice，不让一次预览失败阻断文件 tab 打开。
pub(crate) async fn local_open_workbench_file(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchOpenFileDto, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
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

/// 打开当前 worktree 内的文件。
///
/// Business Logic（为什么需要这个函数）:
///     文件工作区需要一次拿到文件 metadata、类型能力和可用的内容/预览数据，供前端打开 tab。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把请求转发到远端设备；local 项目走原有本机文件打开 helper。
pub(crate) async fn open_workbench_file_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchOpenFileDto, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .open_file(
                &context.base_url,
                &context.inner_project_id,
                inner_worktree_id.as_deref(),
                &path,
            )
            .await;
    }
    local_open_workbench_file(state, project_id, worktree_id, path).await
}
