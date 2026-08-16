//! worktree/Git
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 中的本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helper。

use crate::claude_cli;
use crate::error::{AppError, AppErrorCategory};
use crate::net::protocol::CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1;
use crate::state::AppState;
use crate::workbench::hook_repair::{
    repair_local_worktree_hook_failure, RepairHookFailureDto, RepairHookFailureReq,
};
use crate::workbench::models::{
    WorkbenchDetectedFileType, WorkbenchGitCommitDto, WorkbenchOpenFileDto, WorkbenchProjectRow,
    WorkbenchTextContent, WorkbenchWorktreeDto, WorkbenchWorktreeRow,
};
use crate::workbench::operation_ledger::{
    canonical_collect_merge_payload, canonical_commit_payload, canonical_merge_payload,
    canonical_push_payload, canonical_remove_payload, hash_canonical_payload,
    normalize_client_operation_id, run_claimed_mutation, run_claimed_mutation_with_hook,
    ClaimOutcome, CollectMergeSource, MutationIntent, MutationKind, MutationState,
    MutationTransportClass, WorkbenchHookFailureDto, WorkbenchMutationEnvelopeDto,
    WorkbenchMutationLedger, WorkbenchMutationOperationDto,
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
use std::path::{Component, Path, PathBuf};
use tauri::State;

use super::common::*;
use super::projects::list_workbench_worktrees_for_state;

/// Business Logic: Timeout/Unavailable 映射为 unknown transport class。
/// Code Logic: classify() → Timeout/Unavailable，其它返回 None。
fn mutation_transport_class_from_error(err: &AppError) -> Option<MutationTransportClass> {
    match err.classify() {
        AppErrorCategory::Timeout => Some(MutationTransportClass::Timeout),
        AppErrorCategory::Unavailable => Some(MutationTransportClass::Network),
        _ => None,
    }
}

/// Business Logic: 旧 peer 无 capability 时 success→succeeded，uncertain→unknown。
/// Code Logic: Ok → succeeded envelope；Timeout/Unavailable → unknown；其它 Err。
fn map_legacy_mutation_result<T>(
    result: Result<T, AppError>,
    client_operation_id: &str,
) -> Result<WorkbenchMutationEnvelopeDto<T>, AppError> {
    match result {
        Ok(value) => Ok(WorkbenchMutationEnvelopeDto::succeeded(
            value,
            client_operation_id,
        )),
        Err(err) => {
            if let Some(transport) = mutation_transport_class_from_error(&err) {
                Ok(WorkbenchMutationEnvelopeDto::unknown(
                    client_operation_id,
                    Some(transport),
                ))
            } else {
                Err(err)
            }
        }
    }
}

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
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "worktrees.list",
        serde_json::json!({ "projectId": project_id.clone() }),
    )
    .await?
    {
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
            // opt-in 项目：binding 必须在 helper 返回前存在。
            refresh_agent_hub_bindings_best_effort(state, &project_id).await;
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
        refresh_agent_hub_bindings_best_effort(state, &project_id).await;
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
    // 新建 worktree：opt-in 时 binding 先于返回创建。
    refresh_agent_hub_bindings_best_effort(state, &project_id).await;
    Ok(worktree_to_dto(&row))
}

/// 幂等刷新 Agent Hub checkout bindings（失败仅 debug）。
///
/// Business Logic（为什么需要这个函数）:
///     create/remove worktree 后需同步 binding；未 opt-in 时 refresh 为空且零写入。
///
/// Code Logic（这个函数做什么）:
///     调用 project_scope::refresh_checkout_bindings；错误 tracing::debug。
async fn refresh_agent_hub_bindings_best_effort(state: &AppState, project_id: &str) {
    if let Err(err) =
        crate::agent_hub::project_scope::refresh_checkout_bindings(state, project_id).await
    {
        tracing::debug!(
            project_id = %project_id,
            error = %err,
            "agent_hub refresh_checkout_bindings best-effort failed"
        );
    }
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
            .with_expected_device_id(&context.device_id)
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
    // 兼容 orchestrator delivery 等无 operation id 的调用方：内部生成临时 id 走 ledger。
    let op_id = format!("compat-{}", uuid::Uuid::new_v4());
    match local_commit_workbench_worktree_with_ledger(state, worktree_id, message, op_id).await? {
        WorkbenchMutationEnvelopeDto::Succeeded { value, .. } => Ok(value),
        WorkbenchMutationEnvelopeDto::Unknown { .. } => Err(AppError::unavailable(
            "commit 结果未知（兼容路径）".to_string(),
        )),
        WorkbenchMutationEnvelopeDto::FailedHook { hook_failure, .. } => {
            Err(AppError::generic(format!(
                "{}\n{}",
                hook_failure.summary(),
                hook_failure.combined_output()
            )))
        }
    }
}

/// 带 ledger 的本机 commit。
///
/// Business Logic（为什么需要这个函数）:
///     执行前 claim client_operation_id 与 staged tree intent，timeout 后可精确对账。
///
/// Code Logic（这个函数做什么）:
///     stage → 捕获 beforeHead/expectedTree → claim → 执行 commit → 返回 envelope。
pub(crate) async fn local_commit_workbench_worktree_with_ledger(
    state: &AppState,
    worktree_id: String,
    message: Option<String>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    state.runtime_role.require_owner()?;
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    let path = Path::new(&row.path);
    let before_head = workbench_git::head_hash(path)?;
    let has_changes = workbench_git::stage_all_for_commit(path)?;
    let expected_tree = if has_changes {
        workbench_git::write_tree_hash(path)?
    } else {
        workbench_git::head_tree_hash(path)?.unwrap_or_default()
    };
    let intent = MutationIntent::Commit {
        project_id: row.project_id.clone(),
        worktree_id: worktree_id.clone(),
        before_head: before_head.clone(),
        expected_tree,
    };
    let payload = canonical_commit_payload(&worktree_id, &message);
    let payload_hash = hash_canonical_payload(&payload)?;
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    let claim = ledger
        .claim(&op_id, MutationKind::Commit, &payload_hash, &intent)
        .await?;
    let message_for_exec = message.clone();
    let row_for_exec = row.clone();
    run_claimed_mutation_with_hook(&ledger, &op_id, claim, move || {
        let state = state.clone();
        async move {
            let path = Path::new(&row_for_exec.path);
            if !has_changes {
                return Ok(worktree_to_dto(&row_for_exec));
            }
            match message_for_exec
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(manual_message) => {
                    // 钩子失败走 MutationExecError::Hook → failedHook envelope；其它失败透传 AppError。
                    workbench_git::commit_staged_checked(path, manual_message)?;
                }
                None => {
                    // index 已 stage；只生成 message 并 commit_staged_checked，避免二次 stage 漂移。
                    let changes = workbench_git::staged_changes_for_commit_message(path)?;
                    let (cli_path, model, provider_id) = {
                        let cfg = state.config.read().unwrap();
                        (
                            cfg.github_trending.claude_cli_path.clone(),
                            cfg.github_trending.claude_model.clone(),
                            cfg.internal_claude.provider_id.clone(),
                        )
                    };
                    let provider_dir =
                        crate::internal_claude::resolve_internal_provider_config_dir(
                            provider_id.as_deref(),
                        )
                        .await?;
                    let schema = workbench_commit_message_schema();
                    let instruction = build_commit_message_instruction(&changes);
                    let generated =
                        claude_cli::run_structured_json_with_cwd::<WorkbenchCommitMessageResponse>(
                            &cli_path,
                            &model,
                            provider_dir.as_deref(),
                            &schema.to_string(),
                            &instruction,
                            Some(path),
                            COMMIT_MESSAGE_TIMEOUT_SECS,
                            "生成 commit message",
                        )
                        .await?;
                    let msg = workbench_git::sanitize_commit_message(&generated.message)?;
                    workbench_git::commit_staged_checked(path, &msg)?;
                }
            }
            Ok(worktree_to_dto(&row_for_exec))
        }
    })
    .await
}

/// 提交当前 worktree 的全部改动（ledger + envelope）。
///
/// Business Logic（为什么需要这个函数）:
///     本机/远端 commit 共用 typed envelope；远端按 capability 传播 id/envelope。
///
/// Code Logic（这个函数做什么）:
///     Local 走 ledger；Remote 有 capability 则带 clientOperationId 并解析 envelope，否则 legacy 映射。
pub(crate) async fn commit_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
    message: Option<String>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&context.device_id);
            let supports = client
                .peer_supports_capability(
                    &context.base_url,
                    CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1,
                )
                .await
                .unwrap_or(false);
            if supports {
                let envelope = client
                    .commit_worktree_envelope(
                        &context.base_url,
                        RemoteCommitWorktreeReq {
                            worktree_id: context.inner_worktree_id,
                            message,
                            client_operation_id: Some(op_id.clone()),
                        },
                    )
                    .await?;
                Ok(map_remote_worktree_envelope(
                    &context.device_id,
                    &context.local_project_id,
                    envelope,
                ))
            } else {
                let legacy = client
                    .commit_worktree(
                        &context.base_url,
                        RemoteCommitWorktreeReq {
                            worktree_id: context.inner_worktree_id,
                            message,
                            client_operation_id: None,
                        },
                    )
                    .await
                    .and_then(|item| {
                        map_remote_worktree_dtos(
                            &context.device_id,
                            &context.local_project_id,
                            vec![item],
                        )
                        .into_iter()
                        .next()
                        .ok_or_else(|| AppError::generic("远端 worktree commit 结果为空"))
                    });
                map_legacy_mutation_result(legacy, &op_id)
            }
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_commit_workbench_worktree_with_ledger(state, local_worktree_id, message, op_id)
                .await
        }
    }
}

/// 提交当前 worktree 的全部改动。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Commit 按钮需要提交本机或远端项目 worktree 的全部改动，并返回 typed envelope。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 经 mutation proxy；owner 走 for_state。
#[tauri::command]
pub async fn commit_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    message: Option<String>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    if let Some(v) = proxy_workbench_mutation_if_gui(
        state.inner(),
        "worktrees.commit",
        serde_json::json!({
            "worktreeId": worktree_id.clone(),
            "message": message.clone(),
            "clientOperationId": client_operation_id.clone(),
        }),
        &client_operation_id,
    )
    .await?
    {
        return Ok(v);
    }
    commit_workbench_worktree_for_state(state.inner(), worktree_id, message, client_operation_id)
        .await
}

/// Business Logic: 把远端 worktree DTO envelope 的 id 映射为本机 remote: 前缀。
/// Code Logic: 仅转换 succeeded.value；unknown 原样。
fn map_remote_worktree_envelope(
    device_id: &str,
    local_project_id: &str,
    envelope: WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>,
) -> WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto> {
    match envelope {
        WorkbenchMutationEnvelopeDto::Succeeded {
            value,
            client_operation_id,
        } => {
            let mapped = map_remote_worktree_dtos(device_id, local_project_id, vec![value]);
            // map_remote_worktree_dtos 保持 1:1
            let value = mapped
                .into_iter()
                .next()
                .expect("map_remote_worktree_dtos preserves length");
            WorkbenchMutationEnvelopeDto::succeeded(value, client_operation_id)
        }
        other => other,
    }
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
    let op_id = format!("compat-{}", uuid::Uuid::new_v4());
    match local_push_workbench_worktree_with_ledger(state, worktree_id, op_id).await? {
        WorkbenchMutationEnvelopeDto::Succeeded { value, .. } => Ok(value),
        WorkbenchMutationEnvelopeDto::Unknown { .. } => Err(AppError::unavailable(
            "push 结果未知（兼容路径）".to_string(),
        )),
        WorkbenchMutationEnvelopeDto::FailedHook { hook_failure, .. } => {
            Err(AppError::generic(format!(
                "{}\n{}",
                hook_failure.summary(),
                hook_failure.combined_output()
            )))
        }
    }
}

/// 带 ledger 的本机 push。
///
/// Business Logic（为什么需要这个函数）:
///     推送前捕获 local/remote ref 与 local HEAD，timeout 后可确认 remote 是否到达。
///
/// Code Logic（这个函数做什么）:
///     捕获 intent → claim → push_branch → envelope。
pub(crate) async fn local_push_workbench_worktree_with_ledger(
    state: &AppState,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    state.runtime_role.require_owner()?;
    let op_id = normalize_client_operation_id(&client_operation_id)?;
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
    let (local_ref, remote_ref, local_head) =
        workbench_git::push_ref_identity(Path::new(&row.path), &branch)?;
    let intent = MutationIntent::Push {
        project_id: row.project_id.clone(),
        worktree_id: worktree_id.clone(),
        local_ref,
        remote_ref,
        local_head,
    };
    let payload = canonical_push_payload(&worktree_id);
    let payload_hash = hash_canonical_payload(&payload)?;
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    let claim = ledger
        .claim(&op_id, MutationKind::Push, &payload_hash, &intent)
        .await?;
    let row_for_exec = row.clone();
    let branch_for_exec = branch.clone();
    run_claimed_mutation_with_hook(&ledger, &op_id, claim, move || async move {
        // pre-push 钩子失败 → MutationExecError::Hook → failedHook envelope；远端拒绝/其它失败透传 AppError。
        workbench_git::push_branch_checked(Path::new(&row_for_exec.path), &branch_for_exec)?;
        Ok(worktree_to_dto(&row_for_exec))
    })
    .await
}

/// 推送当前 worktree 分支（ledger + envelope）。
///
/// Business Logic（为什么需要这个函数）:
///     remote/local push 共用 typed envelope。
///
/// Code Logic（这个函数做什么）:
///     Local ledger；Remote 按 capability 传播 envelope 或 legacy 映射。
pub(crate) async fn push_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&context.device_id);
            let supports = client
                .peer_supports_capability(
                    &context.base_url,
                    CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1,
                )
                .await
                .unwrap_or(false);
            if supports {
                let envelope = client
                    .push_worktree_envelope(
                        &context.base_url,
                        &context.inner_worktree_id,
                        Some(op_id.clone()),
                    )
                    .await?;
                Ok(map_remote_worktree_envelope(
                    &context.device_id,
                    &context.local_project_id,
                    envelope,
                ))
            } else {
                let legacy = client
                    .push_worktree(&context.base_url, &context.inner_worktree_id)
                    .await
                    .and_then(|item| {
                        map_remote_worktree_dtos(
                            &context.device_id,
                            &context.local_project_id,
                            vec![item],
                        )
                        .into_iter()
                        .next()
                        .ok_or_else(|| AppError::generic("远端 worktree push 结果为空"))
                    });
                map_legacy_mutation_result(legacy, &op_id)
            }
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_push_workbench_worktree_with_ledger(state, local_worktree_id, op_id).await
        }
    }
}

/// 推送当前 worktree 分支。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Push 返回 typed envelope。
///
/// Code Logic（这个命令做什么）:
///     GuiClient mutation proxy；owner for_state。
#[tauri::command]
pub async fn push_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
    if let Some(v) = proxy_workbench_mutation_if_gui(
        state.inner(),
        "worktrees.push",
        serde_json::json!({
            "worktreeId": worktree_id.clone(),
            "clientOperationId": client_operation_id.clone(),
        }),
        &client_operation_id,
    )
    .await?
    {
        return Ok(v);
    }
    push_workbench_worktree_for_state(state.inner(), worktree_id, client_operation_id).await
}

/// 修复 worktree 的 pre-commit/pre-push 钩子失败：在 worktree 终端启动可见 Claude agent。
///
/// Business Logic（为什么需要这个函数）:
///     本机/远端 commit/push 共用入口；V1 只支持本机 worktree，远端返回可操作错误。
///
/// Code Logic（这个函数做什么）:
///     Remote → 错误（V1）；Local → repair_local_worktree_hook_failure。
pub(crate) async fn repair_worktree_hook_failure_for_state(
    state: &AppState,
    worktree_id: String,
    hook_failure: WorkbenchHookFailureDto,
) -> Result<RepairHookFailureDto, AppError> {
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote { .. } => Err(AppError::generic(
            "钩子修复目前仅支持本机 worktree；远端 worktree 请在对端设备执行",
        )),
        WorktreeCommandTarget::Local(local_worktree_id) => {
            repair_local_worktree_hook_failure(
                state,
                RepairHookFailureReq {
                    worktree_id: local_worktree_id,
                    hook_failure,
                },
            )
            .await
        }
    }
}

/// 修复 pre-commit/pre-push 钩子失败。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 failedHook envelope 之后「让 AI 修复」按钮触发；在 worktree 可见终端启动 agent。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 经 workbench control 代理；owner 走 for_state。
#[tauri::command]
pub async fn repair_worktree_hook_failure(
    state: State<'_, AppState>,
    worktree_id: String,
    hook_failure: WorkbenchHookFailureDto,
) -> Result<RepairHookFailureDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui::<RepairHookFailureDto>(
        state.inner(),
        "worktrees.repairHookFailure",
        serde_json::json!({
            "worktreeId": worktree_id.clone(),
            "hookFailure": hook_failure,
        }),
    )
    .await?
    {
        return Ok(v);
    }
    repair_worktree_hook_failure_for_state(state.inner(), worktree_id, hook_failure).await
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
    let operation_id = format!("compat-{}", uuid::Uuid::new_v4());
    local_merge_workbench_worktree_for_operation(state, worktree_id, operation_id).await
}

/// 在隔离 integration worktree 中合并当前 worktree，再安全发布到主工作区。
///
/// Business Logic（为什么需要这个函数）:
///     真实主 worktree 中出现冲突文件会触发开发 watcher 重启，杀死尚在处理冲突的 Claude headless；
///     一键 merge 必须让长耗时与冲突写入远离真实主工作区，只保留短时、可核验的发布窗口。
///
/// Code Logic（这个函数做什么）:
///     按既有五阶段冻结 main/source OID；创建未入库的 detached integration worktree；在那里执行
///     `merge --no-ff <source_oid>` 和 Claude 解冲突；严格校验双父后确认真实 main 未漂移并 ff-only 发布；
///     任意失败 best-effort 清理隔离目录，只有发布成功后才删除源 worktree。
pub(crate) async fn local_merge_workbench_worktree_for_operation(
    state: &AppState,
    worktree_id: String,
    operation_id: String,
) -> Result<WorkbenchMergeResultDto, AppError> {
    local_merge_workbench_worktree_for_operation_with_frozen(state, worktree_id, operation_id, None)
        .await
}

/// 执行可由 ledger intent 约束冻结输入的隔离 merge。
///
/// Business Logic（为什么需要这个函数）:
///     ledger 先于关闭终端持久化 main/source OID；执行阶段必须以该 intent 为权威，不能悄然重冻新 tip。
///
/// Code Logic（这个函数做什么）:
///     复用隔离 merge五阶段；若传入 expected_frozen，则在创建 integration worktree 前将现场快照与之精确比较。
pub(crate) async fn local_merge_workbench_worktree_for_operation_with_frozen(
    state: &AppState,
    worktree_id: String,
    operation_id: String,
    expected_frozen: Option<workbench_git::FrozenWorkbenchMerge>,
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
        source_status
            .branch
            .clone()
            .or_else(|| row.branch.clone())
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
        "正在隔离 integration worktree 合并冻结的源提交",
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
    let frozen = stage_result(
        workbench_git::freeze_workbench_merge(main_path, Path::new(&row.path)),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    if let Some(expected) = expected_frozen.as_ref() {
        stage_result(
            workbench_git::ensure_frozen_merge_unchanged(expected, &frozen),
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
        )?;
    }
    let repo_root = stage_result(
        workbench_git::repo_root(main_path).map(PathBuf::from),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    let integration_path = merge_integration_storage_path(state, &project_id, &operation_id);
    tracing::info!(
        project_id = %project_id,
        worktree_id = %worktree_id,
        stage = "integration_create",
        "workbench merge stage"
    );
    if let Err(error) = workbench_git::create_detached_integration_worktree_outside(
        &repo_root,
        &integration_path,
        &frozen.main_oid,
        &[main_path, Path::new(&row.path)],
    ) {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            error,
        ));
    }
    let merge_outcome = match workbench_git::merge_commit_oid(&integration_path, &frozen.source_oid)
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = cleanup_merge_integration_best_effort(
                &repo_root,
                &integration_path,
                &project_id,
                &worktree_id,
            );
            return Err(fail_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                error,
            ));
        }
    };
    let merge_result: Result<(), AppError> = match merge_outcome {
        workbench_git::MergeBranchOutcome::Merged => {
            set_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "隔离 integration merge 已生成候选提交",
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
            Ok(())
        }
        workbench_git::MergeBranchOutcome::Conflicted => {
            tracing::info!(
                project_id = %project_id,
                worktree_id = %worktree_id,
                stage = "integration_conflict",
                "workbench merge conflict detected"
            );
            set_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                "completed",
                "隔离 merge 出现冲突，进入自动解决阶段",
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
            tracing::info!(
                project_id = %project_id,
                worktree_id = %worktree_id,
                stage = "claude_start",
                "workbench merge Claude resolution started"
            );
            match resolve_merge_conflicts_with_claude(state, &integration_path).await {
                Ok(file_count) => {
                    tracing::info!(
                        project_id = %project_id,
                        worktree_id = %worktree_id,
                        stage = "claude_result",
                        resolved_files = file_count,
                        "workbench merge Claude resolution completed"
                    );
                    set_merge_stage(
                        state,
                        &project_id,
                        &worktree_id,
                        &mut stages,
                        MERGE_STAGE_RESOLVE_CONFLICTS,
                        "completed",
                        "Claude Code 已在隔离目录解决冲突并完成 merge commit",
                    );
                    Ok(())
                }
                Err(error) => {
                    tracing::warn!(
                        project_id = %project_id,
                        worktree_id = %worktree_id,
                        stage = "claude_failure",
                        error = %error,
                        "workbench merge Claude resolution failed"
                    );
                    Err(error)
                }
            }
        }
    };
    if let Err(error) = merge_result {
        let _ = workbench_git::abort_merge(&integration_path);
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_RESOLVE_CONFLICTS,
            error,
        ));
    }

    let merge_oid = match workbench_git::head_hash(&integration_path)
        .and_then(|oid| oid.ok_or_else(|| AppError::generic("隔离 merge 未生成 HEAD")))
        .and_then(|oid| {
            workbench_git::verify_strict_merge_commit(
                &integration_path,
                &oid,
                &frozen.main_oid,
                &frozen.source_oid,
            )?;
            Ok(oid)
        }) {
        Ok(oid) => oid,
        Err(error) => {
            let _ = cleanup_merge_integration_best_effort(
                &repo_root,
                &integration_path,
                &project_id,
                &worktree_id,
            );
            return Err(fail_merge_stage(
                state,
                &project_id,
                &worktree_id,
                &mut stages,
                MERGE_STAGE_MERGE_MAIN,
                error,
            ));
        }
    };
    tracing::info!(
        project_id = %project_id,
        worktree_id = %worktree_id,
        stage = "publish",
        "workbench merge publishing verified integration commit"
    );
    if let Err(error) =
        workbench_git::verify_source_unchanged_for_publish(Path::new(&row.path), &frozen, &branch)
    {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            error,
        ));
    }
    if let Err(error) = workbench_git::publish_integration_merge(main_path, &frozen, &merge_oid) {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            error,
        ));
    }
    if let Err(error) = cleanup_merge_integration_best_effort(
        &repo_root,
        &integration_path,
        &project_id,
        &worktree_id,
    ) {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            error,
        ));
    }
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
        "completed",
        "已安全发布隔离 merge commit 到主工作区",
    );

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "running",
        "正在删除 worktree 元数据、磁盘工作区和已合并分支",
    );
    let source_cleaned = stage_result(
        cleanup_published_source_if_unchanged(state, &project, &row, &frozen).await,
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
        if source_cleaned {
            "completed"
        } else {
            "skipped"
        },
        if source_cleaned {
            "已删除 worktree 元数据、磁盘工作区和已合并分支"
        } else {
            "发布后检测到源 worktree 已变化，已保留其新提交或改动"
        },
    );

    Ok(WorkbenchMergeResultDto {
        ok: true,
        worktree_id,
        stages,
    })
}

/// 已发布 merge 后仅在源仍停留冻结 OID 时执行破坏性 cleanup。
///
/// Business Logic（为什么需要这个函数）:
///     源 worktree 可能在发布门禁之后再次被外部工具推进；发布已完成时也绝不能删除用户的新提交或改动。
///
/// Code Logic（这个函数做什么）:
///     路径已不存在时视为物理 cleanup 已完成一半，直接进入幂等 cleanup（只 prune 登记，不重复 remove），
///     继续删除 sessions/branch/SQLite row；路径仍存在时读取 row branch 并调用 source frozen gate，
///     相等则执行 cleanup，漂移/dirty/缺分支时记录不含路径的 warn 并返回 false 保留源。
pub(crate) async fn cleanup_published_source_if_unchanged(
    state: &AppState,
    project: &WorkbenchProjectRow,
    row: &WorkbenchWorktreeRow,
    frozen: &workbench_git::FrozenWorkbenchMerge,
) -> Result<bool, AppError> {
    if !path_exists_nofollow(Path::new(&row.path))? {
        tracing::info!(
            project_id = %row.project_id,
            worktree_id = %row.id,
            stage = "source_cleanup_resume_after_git_remove",
            "published merge source path is already absent; resuming metadata cleanup"
        );
        cleanup_merged_worktree(state, project, row).await?;
        return Ok(true);
    }
    let Some(branch) = row.branch.as_deref() else {
        tracing::warn!(
            project_id = %row.project_id,
            worktree_id = %row.id,
            stage = "source_cleanup_preserved",
            "published merge source has no frozen branch; preserving source worktree"
        );
        return Ok(false);
    };
    if let Err(error) =
        workbench_git::verify_source_unchanged_for_publish(Path::new(&row.path), frozen, branch)
    {
        tracing::warn!(
            project_id = %row.project_id,
            worktree_id = %row.id,
            stage = "source_cleanup_preserved",
            error = %error,
            "published merge source changed; preserving source worktree"
        );
        return Ok(false);
    }
    cleanup_merged_worktree(state, project, row).await?;
    Ok(true)
}

/// best-effort 清理一次内部 integration worktree，并输出不含路径的诊断日志。
///
/// Business Logic（为什么需要这个函数）:
///     所有 merge 终止路径都必须回收 Git 登记与临时目录，但日志不能泄露完整本机路径。
///
/// Code Logic（这个函数做什么）:
///     调用 Git cleanup helper；成功记录 cleanup stage，失败记录 project/worktree/error 后上抛。
pub(crate) fn cleanup_merge_integration_best_effort(
    repo_root: &Path,
    integration_path: &Path,
    project_id: &str,
    worktree_id: &str,
) -> Result<(), AppError> {
    match workbench_git::remove_integration_worktree(repo_root, integration_path) {
        Ok(()) => {
            tracing::info!(
                project_id = %project_id,
                worktree_id = %worktree_id,
                stage = "integration_cleanup",
                "workbench merge integration cleanup completed"
            );
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                project_id = %project_id,
                worktree_id = %worktree_id,
                stage = "integration_cleanup_failure",
                error = %error,
                "workbench merge integration cleanup failed"
            );
            Err(error)
        }
    }
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
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&context.device_id);
            let supports = client
                .peer_supports_capability(
                    &context.base_url,
                    CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1,
                )
                .await
                .unwrap_or(false);
            if supports {
                let envelope = client
                    .merge_worktree_envelope(
                        &context.base_url,
                        &context.inner_worktree_id,
                        Some(op_id.clone()),
                    )
                    .await?;
                Ok(map_remote_merge_value_envelope(
                    &context.device_id,
                    envelope,
                )?)
            } else {
                let legacy = client
                    .merge_worktree(&context.base_url, &context.inner_worktree_id)
                    .await
                    .and_then(|value| map_remote_merge_result_value(&context.device_id, value));
                map_legacy_mutation_result(legacy, &op_id)
            }
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_merge_workbench_worktree_with_ledger(state, local_worktree_id, op_id).await
        }
    }
}

/// 带 ledger 的本机 merge。
///
/// Business Logic（为什么需要这个函数）:
///     merge 前捕获 source/main HEAD，timeout 后验证 main 是否包含 source 且 source 已清理。
///     主工作区走 collect-merge，把未占用的本地分支收进 home，而不是拒绝。
///
/// Code Logic（这个函数做什么）:
///     已有 CollectMerge intent 或主 worktree → collect-merge 路径；
///     否则读取 source/main head → claim → 调用既有 feature-worktree merge → envelope。
pub(crate) async fn local_merge_workbench_worktree_with_ledger(
    state: &AppState,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    state.runtime_role.require_owner()?;
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    ledger.ensure_schema().await?;
    if let Some(existing) = ledger.get(&op_id).await? {
        if matches!(existing.intent, MutationIntent::CollectMerge { .. }) {
            return local_collect_merge_main_worktree_with_ledger(state, worktree_id, op_id).await;
        }
        let payload = canonical_merge_payload(&worktree_id);
        let payload_hash = hash_canonical_payload(&payload)?;
        if existing.payload_hash != payload_hash {
            return Err(AppError::conflict(format!(
                "clientOperationId 已绑定不同 payload（existingHash={}）",
                existing.payload_hash
            )));
        }
        if merge_ledger_state_needs_published_recovery(existing.state) {
            let recovered = recover_pending_merge_after_publish(
                state,
                &ledger,
                &op_id,
                &worktree_id,
                &existing.intent,
            )
            .await;
            if !matches!(recovered, Ok(WorkbenchMutationEnvelopeDto::Unknown { .. })) {
                return recovered;
            }
        }
        if existing.state.is_pending() {
            return Ok(WorkbenchMutationEnvelopeDto::unknown(op_id, None));
        }
        return run_claimed_mutation(&ledger, &op_id, ClaimOutcome::Replay(existing), || async {
            Err(AppError::generic("终态 merge ledger 不应重新执行"))
        })
        .await;
    }
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if row.is_main {
        return local_collect_merge_main_worktree_with_ledger(state, worktree_id, op_id).await;
    }
    let source_head = workbench_git::head_hash(Path::new(&row.path))?
        .ok_or_else(|| AppError::generic("源 worktree 没有 HEAD".to_string()))?;
    let main = state
        .workbench_worktree_repo
        .list_by_project(&row.project_id)
        .await?
        .into_iter()
        .find(|wt| wt.is_main)
        .ok_or_else(|| AppError::not_found("主工作区不存在"))?;
    let main_head = workbench_git::head_hash(Path::new(&main.path))?
        .ok_or_else(|| AppError::generic("主工作区没有 HEAD".to_string()))?;
    let main_branch = workbench_git::current_branch(Path::new(&main.path))
        .ok_or_else(|| AppError::generic("主工作区没有当前分支".to_string()))?;
    let intent = MutationIntent::Merge {
        project_id: row.project_id.clone(),
        source_worktree_id: worktree_id.clone(),
        source_head: source_head.clone(),
        main_head: main_head.clone(),
    };
    let payload = canonical_merge_payload(&worktree_id);
    let payload_hash = hash_canonical_payload(&payload)?;
    let claim = ledger
        .claim(&op_id, MutationKind::Merge, &payload_hash, &intent)
        .await?;
    let state_for_exec = state.clone();
    let wt_for_exec = worktree_id.clone();
    let op_for_exec = op_id.clone();
    let frozen_for_exec = workbench_git::FrozenWorkbenchMerge {
        main_branch,
        main_oid: main_head,
        source_oid: source_head,
    };
    run_claimed_mutation(&ledger, &op_id, claim, move || async move {
        local_merge_workbench_worktree_for_operation_with_frozen(
            &state_for_exec,
            wt_for_exec,
            op_for_exec,
            Some(frozen_for_exec),
        )
        .await
    })
    .await
}

/// 主工作区 collect-merge 冻结快照。
///
/// Business Logic（为什么需要这个结构体）:
///     claim 时必须钉死 home 与源分支 tip，执行期不能再读可能漂移的 live HEAD。
///
/// Code Logic（这个结构体做什么）:
///     保存 home 短名、home OID，以及按名称排序的源 name+oid。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenCollectMerge {
    home_branch: String,
    home_oid: String,
    sources: Vec<CollectMergeSource>,
}

/// 带 ledger 的主工作区 collect-merge。
///
/// Business Logic（为什么需要这个函数）:
///     主工作区 Merge 要把未占用的本地 agent 分支收进 home；同一 clientOperationId
///     换源必须 conflict，发布后崩溃则按冻结 intent 对账，不得重放 Claude。
///
/// Code Logic（这个函数做什么）:
///     已有行走 recovery/replay；fresh 则冻结 home/sources → claim CollectMerge →
///     隔离顺序 merge → 发布 home → 删除未占用源分支。
async fn local_collect_merge_main_worktree_with_ledger(
    state: &AppState,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    state.runtime_role.require_owner()?;
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    ledger.ensure_schema().await?;
    if let Some(existing) = ledger.get(&op_id).await? {
        match &existing.intent {
            MutationIntent::CollectMerge {
                worktree_id: stored_worktree_id,
                home_branch,
                home_oid,
                sources,
                ..
            } => {
                if stored_worktree_id != &worktree_id {
                    return Err(AppError::conflict(
                        "clientOperationId 的 worktree 与请求不一致".to_string(),
                    ));
                }
                if merge_ledger_state_needs_published_recovery(existing.state) {
                    let recovered = recover_pending_merge_after_publish(
                        state,
                        &ledger,
                        &op_id,
                        &worktree_id,
                        &existing.intent,
                    )
                    .await;
                    if !matches!(recovered, Ok(WorkbenchMutationEnvelopeDto::Unknown { .. })) {
                        return recovered;
                    }
                }
                if existing.state.is_pending() {
                    if let Ok(Some(row)) = state.workbench_worktree_repo.get(&worktree_id).await {
                        if let Ok(current) = freeze_collect_merge_sources(Path::new(&row.path)) {
                            let current_hash =
                                hash_canonical_payload(&canonical_collect_merge_payload(
                                    &worktree_id,
                                    &current.home_branch,
                                    &current.home_oid,
                                    &current.sources,
                                ))?;
                            let stored_hash =
                                hash_canonical_payload(&canonical_collect_merge_payload(
                                    stored_worktree_id,
                                    home_branch,
                                    home_oid,
                                    sources,
                                ))?;
                            if current_hash != stored_hash || current_hash != existing.payload_hash
                            {
                                return Err(AppError::conflict(format!(
                                    "clientOperationId 已绑定不同 payload（existingHash={}）",
                                    existing.payload_hash
                                )));
                            }
                        }
                    }
                    return Ok(WorkbenchMutationEnvelopeDto::unknown(op_id, None));
                }
                return run_claimed_mutation(
                    &ledger,
                    &op_id,
                    ClaimOutcome::Replay(existing),
                    || async {
                        Err(AppError::generic("终态 collect-merge ledger 不应重新执行"))
                    },
                )
                .await;
            }
            _ => {
                return Err(AppError::conflict(
                    "clientOperationId 已绑定非 collect-merge intent".to_string(),
                ));
            }
        }
    }

    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if !row.is_main {
        return Err(AppError::generic(
            "collect-merge 只能在主工作区执行".to_string(),
        ));
    }
    let frozen = freeze_collect_merge_sources(Path::new(&row.path))?;
    let payload = canonical_collect_merge_payload(
        &worktree_id,
        &frozen.home_branch,
        &frozen.home_oid,
        &frozen.sources,
    );
    let payload_hash = hash_canonical_payload(&payload)?;
    let intent = MutationIntent::CollectMerge {
        project_id: row.project_id.clone(),
        worktree_id: worktree_id.clone(),
        home_branch: frozen.home_branch.clone(),
        home_oid: frozen.home_oid.clone(),
        sources: frozen.sources.clone(),
    };
    let claim = ledger
        .claim(&op_id, MutationKind::Merge, &payload_hash, &intent)
        .await?;
    let state_for_exec = state.clone();
    let row_for_exec = row;
    let op_for_exec = op_id.clone();
    run_claimed_mutation(&ledger, &op_id, claim, move || async move {
        local_collect_merge_main_worktree(&state_for_exec, &row_for_exec, &op_for_exec, &frozen)
            .await
    })
    .await
}

/// 冻结主工作区 collect-merge 的 home 与可收集源。
///
/// Business Logic（为什么需要这个函数）:
///     claim 与 checkSource 必须使用同一套规则：home 引用 tip、未占用且未合入的本地分支、
///     live 工作区干净；不能把离 home 的 live HEAD 当成 home_oid。
///
/// Code Logic（这个函数做什么）:
///     detect_home_branch + rev_parse_ref(refs/heads/home) + occupied + list_collectible；
///     源为空或 live dirty 时失败。
fn freeze_collect_merge_sources(live_path: &Path) -> Result<FrozenCollectMerge, AppError> {
    let live_status = workbench_git::status(live_path)?;
    if !live_status.clean {
        return Err(AppError::generic(
            "主工作区有未提交改动，请先提交或清理后再收集合并".to_string(),
        ));
    }
    let home_branch = workbench_git::detect_home_branch(live_path)?;
    let home_oid =
        workbench_git::rev_parse_ref(live_path, &format!("refs/heads/{home_branch}"))?
            .ok_or_else(|| AppError::generic(format!("无法读取 home 分支 {home_branch} 的提交")))?;
    let occupied = workbench_git::occupied_worktree_branches(live_path, live_path)?;
    let collectible = workbench_git::list_collectible_branches(live_path, &home_branch, &occupied)?;
    if collectible.is_empty() {
        return Err(AppError::validation(
            "没有可收集到主分支的本地分支".to_string(),
        ));
    }
    Ok(FrozenCollectMerge {
        home_branch,
        home_oid,
        sources: collectible
            .into_iter()
            .map(|branch| CollectMergeSource {
                name: branch.name,
                oid: branch.oid,
            })
            .collect(),
    })
}

/// 执行主工作区 collect-merge 五阶段。
///
/// Business Logic（为什么需要这个函数）:
///     用户在主工作区点 Merge 时，要把本机残留 agent 分支隔离合并进 home，
///     不能关闭主工作区终端，也不能删除主 worktree 行。
///
/// Code Logic（这个函数做什么）:
///     checkSource 校验冻结输入；closeSessions 跳过；在隔离 worktree 按源顺序
///     merge --no-ff 并校验双父；冲突才调 Claude；cleanup 发布 home 并删除未占用源分支；
///     成败都 best-effort 回收 isolation 目录。
async fn local_collect_merge_main_worktree(
    state: &AppState,
    row: &WorkbenchWorktreeRow,
    operation_id: &str,
    expected: &FrozenCollectMerge,
) -> Result<WorkbenchMergeResultDto, AppError> {
    let mut stages = initial_merge_stages();
    let project_id = row.project_id.clone();
    let worktree_id = row.id.clone();
    let live_path = PathBuf::from(&row.path);

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "running",
        "正在检查主工作区与可收集分支",
    );
    if !row.is_main {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CHECK_SOURCE,
            AppError::generic("collect-merge 只能在主工作区执行"),
        ));
    }
    let frozen = stage_result(
        freeze_collect_merge_sources(&live_path),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
    )?;
    if frozen != *expected {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CHECK_SOURCE,
            AppError::conflict("collect-merge 冻结的 home/源分支已变化".to_string()),
        ));
    }
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CHECK_SOURCE,
        "completed",
        format!(
            "已冻结 home {} 与 {} 个可收集分支",
            frozen.home_branch,
            frozen.sources.len()
        ),
    );

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLOSE_SESSIONS,
        "skipped",
        "主工作区收集合并不关闭终端",
    );

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
        "running",
        "正在隔离 integration worktree 顺序合并冻结源提交",
    );
    let repo_root = stage_result(
        workbench_git::repo_root(&live_path).map(PathBuf::from),
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
    )?;
    let integration_path = merge_integration_storage_path(state, &project_id, operation_id);
    if let Err(error) = workbench_git::create_detached_integration_worktree_outside(
        &repo_root,
        &integration_path,
        &frozen.home_oid,
        &[&live_path],
    ) {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_MERGE_MAIN,
            error,
        ));
    }

    let mut prev_oid = frozen.home_oid.clone();
    let mut had_conflict = false;
    for source in &frozen.sources {
        let merge_outcome = match workbench_git::merge_commit_oid(&integration_path, &source.oid) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = cleanup_merge_integration_best_effort(
                    &repo_root,
                    &integration_path,
                    &project_id,
                    &worktree_id,
                );
                return Err(fail_merge_stage(
                    state,
                    &project_id,
                    &worktree_id,
                    &mut stages,
                    MERGE_STAGE_MERGE_MAIN,
                    error,
                ));
            }
        };
        match merge_outcome {
            workbench_git::MergeBranchOutcome::Merged => {
                match verify_collect_merge_head(&integration_path, &prev_oid, &source.oid) {
                    Ok(new_head) => prev_oid = new_head,
                    Err(error) => {
                        let _ = cleanup_merge_integration_best_effort(
                            &repo_root,
                            &integration_path,
                            &project_id,
                            &worktree_id,
                        );
                        return Err(fail_merge_stage(
                            state,
                            &project_id,
                            &worktree_id,
                            &mut stages,
                            MERGE_STAGE_MERGE_MAIN,
                            error,
                        ));
                    }
                }
            }
            workbench_git::MergeBranchOutcome::Conflicted => {
                had_conflict = true;
                set_merge_stage(
                    state,
                    &project_id,
                    &worktree_id,
                    &mut stages,
                    MERGE_STAGE_MERGE_MAIN,
                    "completed",
                    format!("隔离合并 {} 出现冲突，进入自动解决阶段", source.name),
                );
                set_merge_stage(
                    state,
                    &project_id,
                    &worktree_id,
                    &mut stages,
                    MERGE_STAGE_RESOLVE_CONFLICTS,
                    "running",
                    "正在调用 Claude Code 尝试解决 collect-merge 冲突",
                );
                match resolve_merge_conflicts_with_claude(state, &integration_path).await {
                    Ok(_) => {
                        match verify_collect_merge_head(&integration_path, &prev_oid, &source.oid) {
                            Ok(new_head) => {
                                prev_oid = new_head;
                                set_merge_stage(
                                    state,
                                    &project_id,
                                    &worktree_id,
                                    &mut stages,
                                    MERGE_STAGE_RESOLVE_CONFLICTS,
                                    "completed",
                                    "Claude Code 已在隔离目录解决冲突并完成 merge commit",
                                );
                            }
                            Err(error) => {
                                let _ = workbench_git::abort_merge(&integration_path);
                                let _ = cleanup_merge_integration_best_effort(
                                    &repo_root,
                                    &integration_path,
                                    &project_id,
                                    &worktree_id,
                                );
                                return Err(fail_merge_stage(
                                    state,
                                    &project_id,
                                    &worktree_id,
                                    &mut stages,
                                    MERGE_STAGE_RESOLVE_CONFLICTS,
                                    error,
                                ));
                            }
                        }
                    }
                    Err(error) => {
                        let _ = workbench_git::abort_merge(&integration_path);
                        let _ = cleanup_merge_integration_best_effort(
                            &repo_root,
                            &integration_path,
                            &project_id,
                            &worktree_id,
                        );
                        return Err(fail_merge_stage(
                            state,
                            &project_id,
                            &worktree_id,
                            &mut stages,
                            MERGE_STAGE_RESOLVE_CONFLICTS,
                            error,
                        ));
                    }
                }
            }
        }
    }

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_MERGE_MAIN,
        "completed",
        "隔离 collect-merge 已生成候选提交",
    );
    if !had_conflict {
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

    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "running",
        "正在把 collect-merge 发布到 home 并清理源分支",
    );
    if let Err(error) = workbench_git::publish_collect_merge_to_home(
        &live_path,
        &frozen.home_branch,
        &frozen.home_oid,
        &prev_oid,
    ) {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CLEANUP,
            error,
        ));
    }
    let source_names = frozen
        .sources
        .iter()
        .map(|source| source.name.clone())
        .collect::<Vec<_>>();
    if let Err(error) =
        workbench_git::delete_local_branches_if_unoccupied(&live_path, &source_names)
    {
        let _ = cleanup_merge_integration_best_effort(
            &repo_root,
            &integration_path,
            &project_id,
            &worktree_id,
        );
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CLEANUP,
            error,
        ));
    }
    if let Err(error) = cleanup_merge_integration_best_effort(
        &repo_root,
        &integration_path,
        &project_id,
        &worktree_id,
    ) {
        return Err(fail_merge_stage(
            state,
            &project_id,
            &worktree_id,
            &mut stages,
            MERGE_STAGE_CLEANUP,
            error,
        ));
    }
    set_merge_stage(
        state,
        &project_id,
        &worktree_id,
        &mut stages,
        MERGE_STAGE_CLEANUP,
        "completed",
        "已发布到 home 并删除未占用的源分支",
    );

    Ok(WorkbenchMergeResultDto {
        ok: true,
        worktree_id,
        stages,
    })
}

/// 校验隔离 collect-merge 刚产生的双父提交。
///
/// Business Logic（为什么需要这个函数）:
///     每个源合入后都必须证明 new HEAD 的 parent 恰好是上一轮 tip 与该源 oid，
///     否则不能继续下一源或发布。
///
/// Code Logic（这个函数做什么）:
///     读 integration HEAD，调用 verify_strict_merge_commit 后返回新 HEAD。
fn verify_collect_merge_head(
    integration_path: &Path,
    prev_oid: &str,
    source_oid: &str,
) -> Result<String, AppError> {
    let new_head = workbench_git::head_hash(integration_path)?
        .ok_or_else(|| AppError::generic("隔离 collect-merge 未生成 HEAD"))?;
    workbench_git::verify_strict_merge_commit(integration_path, &new_head, prev_oid, source_oid)?;
    Ok(new_head)
}

/// 判断已有 merge ledger 是否需优先做“已发布”精确恢复。
///
/// Business Logic（为什么需要这个函数）:
///     发布后 owner 可能在 ledger 仍 running 或已被通用 runner 标 failed 时重启；两者都不能永久回放
///     unknown/failed，必须先确认真实 main 是否已有本次精确双父 merge。
///
/// Code Logic（这个函数做什么）:
///     claimed/running/failed 返回 true；succeeded 已有权威 outcome 返回 false。
pub(crate) fn merge_ledger_state_needs_published_recovery(state: MutationState) -> bool {
    state != MutationState::Succeeded
}

/// owner 重启后收敛已发布但 ledger 仍 pending 的 merge。
///
/// Business Logic（为什么需要这个函数）:
///     watcher 可能恰在真实 main ff-only 发布后、源 worktree cleanup 或 ledger success 落盘前重启 owner；
///     同一 clientOperationId 不能永久返回 unknown，也不能重新生成第二个 merge commit。
///
/// Code Logic（这个函数做什么）:
///     Merge：从持久 intent 读取冻结 main/source OID；在当前 main 历史中查找精确双父 merge commit；
///     未发布则保持 unknown 且不重放 Claude；已发布则幂等清理确定性 integration 路径和残留源 worktree，
///     构造兼容五阶段结果并 mark_succeeded。
///     CollectMerge：home tip 已是冻结 home 的后代且包含全部 source oid 则视为已发布，checkout home、
///     删除未占用源分支并 mark_succeeded，不重放 Claude。
pub(crate) async fn recover_pending_merge_after_publish(
    state: &AppState,
    ledger: &WorkbenchMutationLedger,
    operation_id: &str,
    requested_worktree_id: &str,
    intent: &MutationIntent,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    if let MutationIntent::CollectMerge { .. } = intent {
        return recover_pending_collect_merge_after_publish(
            state,
            ledger,
            operation_id,
            requested_worktree_id,
            intent,
        )
        .await;
    }
    let MutationIntent::Merge {
        project_id,
        source_worktree_id,
        source_head,
        main_head,
    } = intent
    else {
        return Err(AppError::conflict(
            "clientOperationId 已绑定非 merge intent".to_string(),
        ));
    };
    if source_worktree_id != requested_worktree_id {
        return Err(AppError::conflict(
            "clientOperationId 的 source worktree 与请求不一致".to_string(),
        ));
    }
    let project = get_project(state, project_id).await?;
    let main = ensure_main_worktree(state, &project).await?;
    let main_path = Path::new(&main.path);
    let Some(_published_merge_oid) =
        workbench_git::find_published_merge_commit(main_path, main_head, source_head)?
    else {
        return Ok(WorkbenchMutationEnvelopeDto::unknown(operation_id, None));
    };

    let repo_root = PathBuf::from(workbench_git::repo_root(main_path)?);
    let integration_path = merge_integration_storage_path(state, project_id, operation_id);
    cleanup_merge_integration_best_effort(
        &repo_root,
        &integration_path,
        project_id,
        source_worktree_id,
    )?;
    let mut source_cleaned = true;
    if let Some(row) = state
        .workbench_worktree_repo
        .get(source_worktree_id)
        .await?
    {
        let branch = row.branch.clone().unwrap_or_default();
        let frozen = workbench_git::FrozenWorkbenchMerge {
            main_branch: workbench_git::current_branch(main_path).unwrap_or_default(),
            main_oid: main_head.clone(),
            source_oid: source_head.clone(),
        };
        source_cleaned = if branch.is_empty() {
            false
        } else {
            cleanup_published_source_if_unchanged(state, &project, &row, &frozen).await?
        };
    }

    let mut stages = initial_merge_stages();
    for stage in &mut stages {
        stage.status = "completed".to_string();
        stage.message = "owner 重启后已确认并收敛已发布 merge".to_string();
    }
    if let Some(stage) = stages
        .iter_mut()
        .find(|stage| stage.id == MERGE_STAGE_RESOLVE_CONFLICTS)
    {
        stage.status = "skipped".to_string();
        stage.message = "恢复路径复用已发布 merge，不重复调用 Claude Code".to_string();
    }
    if !source_cleaned {
        if let Some(stage) = stages
            .iter_mut()
            .find(|stage| stage.id == MERGE_STAGE_CLEANUP)
        {
            stage.status = "skipped".to_string();
            stage.message = "源 worktree 已变化，恢复路径保留其新提交或改动".to_string();
        }
    }
    let result = WorkbenchMergeResultDto {
        ok: true,
        worktree_id: source_worktree_id.clone(),
        stages,
    };
    ledger.mark_succeeded(operation_id, &result).await?;
    tracing::info!(
        project_id = %project_id,
        worktree_id = %source_worktree_id,
        stage = "publish_recovery_cleanup",
        "workbench merge pending ledger recovered after publish"
    );
    Ok(WorkbenchMutationEnvelopeDto::succeeded(
        result,
        operation_id,
    ))
}

/// owner 重启后收敛已发布但 ledger 仍 pending 的 collect-merge。
///
/// Business Logic（为什么需要这个函数）:
///     发布到 home 后、删源分支或 mark_succeeded 前可能崩溃；同一 clientOperationId
///     不能重放 Claude，只能按冻结 home/sources 判断是否已经发布。
///
/// Code Logic（这个函数做什么）:
///     读 live home ref；home tip 是冻结 home_oid 的后代且每个 source oid 都是 home tip
///     的祖先则视为已发布；必要时 checkout home，删除未占用源分支，清理 isolation，
///     mark_succeeded。未发布返回 unknown。
async fn recover_pending_collect_merge_after_publish(
    state: &AppState,
    ledger: &WorkbenchMutationLedger,
    operation_id: &str,
    requested_worktree_id: &str,
    intent: &MutationIntent,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    let MutationIntent::CollectMerge {
        project_id,
        worktree_id,
        home_branch,
        home_oid,
        sources,
    } = intent
    else {
        return Err(AppError::conflict(
            "clientOperationId 已绑定非 collect-merge intent".to_string(),
        ));
    };
    if worktree_id != requested_worktree_id {
        return Err(AppError::conflict(
            "clientOperationId 的 worktree 与请求不一致".to_string(),
        ));
    }
    let project = get_project(state, project_id).await?;
    let main = ensure_main_worktree(state, &project).await?;
    let live_path = Path::new(&main.path);
    let home_ref = format!("refs/heads/{home_branch}");
    let Some(home_tip) = workbench_git::rev_parse_ref(live_path, &home_ref)? else {
        return Ok(WorkbenchMutationEnvelopeDto::unknown(operation_id, None));
    };
    if !workbench_git::is_ancestor(live_path, home_oid, &home_tip)? {
        return Ok(WorkbenchMutationEnvelopeDto::unknown(operation_id, None));
    }
    for source in sources {
        if !workbench_git::is_ancestor(live_path, &source.oid, &home_tip)? {
            return Ok(WorkbenchMutationEnvelopeDto::unknown(operation_id, None));
        }
    }

    if workbench_git::current_branch(live_path).as_deref() != Some(home_branch.as_str()) {
        checkout_local_branch(live_path, home_branch)?;
    }
    let source_names = sources
        .iter()
        .map(|source| source.name.clone())
        .collect::<Vec<_>>();
    workbench_git::delete_local_branches_if_unoccupied(live_path, &source_names)?;

    let repo_root = PathBuf::from(workbench_git::repo_root(live_path)?);
    let integration_path = merge_integration_storage_path(state, project_id, operation_id);
    let _ = cleanup_merge_integration_best_effort(
        &repo_root,
        &integration_path,
        project_id,
        worktree_id,
    );

    let mut stages = initial_merge_stages();
    for stage in &mut stages {
        stage.status = "completed".to_string();
        stage.message = "owner 重启后已确认并收敛已发布 collect-merge".to_string();
    }
    if let Some(stage) = stages
        .iter_mut()
        .find(|stage| stage.id == MERGE_STAGE_CLOSE_SESSIONS)
    {
        stage.status = "skipped".to_string();
        stage.message = "主工作区收集合并不关闭终端".to_string();
    }
    if let Some(stage) = stages
        .iter_mut()
        .find(|stage| stage.id == MERGE_STAGE_RESOLVE_CONFLICTS)
    {
        stage.status = "skipped".to_string();
        stage.message = "恢复路径复用已发布 collect-merge，不重复调用 Claude Code".to_string();
    }
    let result = WorkbenchMergeResultDto {
        ok: true,
        worktree_id: worktree_id.clone(),
        stages,
    };
    ledger.mark_succeeded(operation_id, &result).await?;
    tracing::info!(
        project_id = %project_id,
        worktree_id = %worktree_id,
        stage = "collect_publish_recovery_cleanup",
        "workbench collect-merge pending ledger recovered after publish"
    );
    Ok(WorkbenchMutationEnvelopeDto::succeeded(
        result,
        operation_id,
    ))
}

/// 把主工作区切回指定本地分支。
///
/// Business Logic（为什么需要这个函数）:
///     collect-merge 发布后 live 可能仍停在 agent 分支；恢复路径只需 checkout，
///     不能再用冻结 home_oid 做 CAS 发布。
///
/// Code Logic（这个函数做什么）:
///     执行 `git checkout <branch>`，失败返回包含 stderr 的 generic 错误。
fn checkout_local_branch(path: &Path, branch: &str) -> Result<(), AppError> {
    let output = std::process::Command::new("git")
        .args(["checkout", branch])
        .current_dir(path)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::generic(format!(
        "切换到 {branch} 失败: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// 合并当前 worktree 到主工作区。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 Merge 返回 typed envelope。
///
/// Code Logic（这个命令做什么）:
///     GuiClient mutation proxy；owner for_state。
#[tauri::command]
pub async fn merge_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    if let Some(v) = proxy_workbench_mutation_if_gui(
        state.inner(),
        "worktrees.merge",
        serde_json::json!({
            "worktreeId": worktree_id.clone(),
            "clientOperationId": client_operation_id.clone(),
        }),
        &client_operation_id,
    )
    .await?
    {
        return Ok(v);
    }
    merge_workbench_worktree_for_state(state.inner(), worktree_id, client_operation_id).await
}

/// Business Logic: 映射远端 merge envelope（Value）的 worktreeId 并转为 DTO。
/// Code Logic: succeeded 时 map_remote_merge_result_value；unknown 原样。
fn map_remote_merge_value_envelope(
    device_id: &str,
    envelope: WorkbenchMutationEnvelopeDto<Value>,
) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchMergeResultDto>, AppError> {
    match envelope {
        WorkbenchMutationEnvelopeDto::Succeeded {
            value,
            client_operation_id,
        } => {
            let dto = map_remote_merge_result_value(device_id, value)?;
            Ok(WorkbenchMutationEnvelopeDto::succeeded(
                dto,
                client_operation_id,
            ))
        }
        WorkbenchMutationEnvelopeDto::Unknown {
            client_operation_id,
            transport_class,
        } => Ok(WorkbenchMutationEnvelopeDto::Unknown {
            client_operation_id,
            transport_class,
        }),
        // 远端 merge 不会产生 failedHook（merge 走 run_claimed_mutation），防御性透传。
        WorkbenchMutationEnvelopeDto::FailedHook {
            client_operation_id,
            hook_failure,
        } => Ok(WorkbenchMutationEnvelopeDto::failed_hook(
            client_operation_id,
            hook_failure,
        )),
    }
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
        // R24 H1 / R25 H1：persist cleanup 成功前保持 Closing barrier；missing handle 也装 intent。
        match state.workbench_sessions.close(&row.id) {
            Ok(cleanup) => {
                // R35 M3：kill 失败不 delete、不 finish（Drop 保留 barrier）。
                kill_persisted_backend(cleanup.row())?;
                state.workbench_session_repo.delete(&row.id).await?;
                cleanup.finish_cleanup();
            }
            Err(AppError::NotFound(_)) => {
                let cleanup = match state
                    .workbench_sessions
                    .begin_close_intent_for_missing_handle(&row.id, row.clone())
                {
                    Ok(cleanup) => cleanup,
                    Err(AppError::Conflict(_)) => match state.workbench_sessions.close(&row.id) {
                        Ok(cleanup) => cleanup,
                        Err(AppError::NotFound(_)) => state
                            .workbench_sessions
                            .begin_close_intent_for_missing_handle(&row.id, row.clone())?,
                        Err(error) => return Err(error),
                    },
                    Err(error) => return Err(error),
                };
                kill_persisted_backend(cleanup.row())?;
                state.workbench_session_repo.delete(&row.id).await?;
                cleanup.finish_cleanup();
            }
            Err(error) => return Err(error),
        }
        closed += 1;
    }
    Ok(closed)
}

/// Business Logic（为什么需要这个函数）:
///     merge 成功后，已合并 worktree 不应继续占用 terminal metadata、SQLite worktree row 或磁盘 worktree。
///     R36 H3 / R41 M6：merge 耗时期间可能并发 create 新 session；仅 bulk `delete_by_worktree`
///     会留下未 close 的 live/tmux 孤儿。必须在 project closing barrier 下 drain lease、
///     re-snapshot close，再 bulk delete，防止 create 在 close 快照之后 upsert 再被只删 SQLite。
///
/// Code Logic（这个函数做什么）:
///     begin_project_closing_barrier → wait project op leases → close_sessions_for_worktree
///     （再次 re-snapshot）→ delete_by_worktree → 路径存在则 git remove、路径不存在则只 prune 残留登记
///     → branch → worktree row delete →
///     wait leases → finish barrier。任何 close/kill/delete 失败保留 barrier fail-closed。
pub(crate) async fn cleanup_merged_worktree(
    state: &AppState,
    project: &WorkbenchProjectRow,
    row: &WorkbenchWorktreeRow,
) -> Result<(), AppError> {
    // R41 M6：复用 project closing barrier + lease drain，覆盖 close→bulk delete 窗口。
    let project_barrier = state
        .workbench_sessions
        .begin_project_closing_barrier(&row.project_id);
    if !state
        .workbench_sessions
        .wait_project_op_leases_drained(&row.project_id)
    {
        tracing::warn!(
            project_id = %row.project_id,
            worktree_id = %row.id,
            "project op leases still in-flight before merge cleanup; retaining project barrier"
        );
        return Err(AppError::unavailable(
            "project_op_lease_drain_timeout".to_string(),
        ));
    }
    // barrier 下 re-snapshot close，拦截 merge 期间并发 create。
    close_sessions_for_worktree(state, &row.project_id, &row.id).await?;
    // 二次 close：close_sessions 与 bulk delete 之间仍可能有竞态窗口内的新行
    // （barrier 下 create 应失败；再扫一次防御 SQLite 残留）。
    close_sessions_for_worktree(state, &row.project_id, &row.id).await?;
    state
        .workbench_session_repo
        .delete_by_worktree(&row.project_id, &row.id)
        .await?;
    let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
    if path_exists_nofollow(Path::new(&row.path))? {
        workbench_git::remove_worktree(Path::new(&repo_root), Path::new(&row.path), false)?;
    } else {
        workbench_git::prune_missing_worktree_registration(
            Path::new(&repo_root),
            Path::new(&row.path),
        )?;
    }
    if let Some(branch) = row.branch.as_deref() {
        workbench_git::delete_local_branch_if_merged(Path::new(&repo_root), branch, "HEAD")?;
    }
    state.workbench_worktree_repo.delete(&row.id).await?;
    if !state
        .workbench_sessions
        .wait_project_op_leases_drained(&row.project_id)
    {
        tracing::warn!(
            project_id = %row.project_id,
            worktree_id = %row.id,
            "project op leases still in-flight after merge cleanup; retaining project barrier"
        );
        return Err(AppError::unavailable(
            "project_op_lease_drain_timeout".to_string(),
        ));
    }
    state
        .workbench_sessions
        .finish_project_closing_barrier(&row.project_id, project_barrier);
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     merge 冲突时，后端需要调用本机 Claude Code CLI 在隔离 integration worktree 项目上下文下尝试生成解决结果。
///
/// Code Logic（这个函数做什么）:
///     校验未解决冲突路径，调用只开放文件读写工具的 Claude CLI 直接编辑隔离 worktree，确认所有原冲突文件
///     不再含 marker 且 Git index 无 unresolved entry 后 stage all，最后使用 Git 默认 merge message 完成 commit。
pub(crate) async fn resolve_merge_conflicts_with_claude(
    state: &AppState,
    integration_path: &Path,
) -> Result<usize, AppError> {
    let conflict_paths = workbench_git::unresolved_conflict_files(integration_path)?;
    if conflict_paths.is_empty() {
        return Ok(0);
    }
    validate_merge_conflict_files_for_agent(integration_path, &conflict_paths)?;
    let (cli_path, model, provider_id) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
            cfg.internal_claude.provider_id.clone(),
        )
    };
    let provider_dir =
        crate::internal_claude::resolve_internal_provider_config_dir(provider_id.as_deref())
            .await?;
    let instruction = build_merge_conflict_edit_instruction(&conflict_paths);
    claude_cli::run_project_edit_with_cwd(
        &cli_path,
        &model,
        provider_dir.as_deref(),
        &instruction,
        integration_path,
        MERGE_CONFLICT_RESOLUTION_TIMEOUT_SECS,
        "解决 merge 冲突",
    )
    .await?;
    ensure_only_conflict_files_modified(integration_path, &conflict_paths)?;
    ensure_conflict_markers_removed(integration_path, &conflict_paths)?;
    workbench_git::stage_all_merge_resolution(integration_path)?;
    let remaining = workbench_git::unresolved_conflict_files(integration_path)?;
    if !remaining.is_empty() {
        return Err(AppError::generic(format!(
            "Claude Code 处理后仍有未解决冲突: {}",
            remaining.join(", ")
        )));
    }
    workbench_git::commit_merge_no_edit(integration_path)?;
    Ok(conflict_paths.len())
}

/// Business Logic（为什么需要这个函数）:
///     Claude 获得文件写工具前，后端必须确认所有冲突目标都是隔离 worktree 内的普通 UTF-8 文件，
///     防止 symlink/越界路径被工具跟随，也让二进制冲突明确失败。
///
/// Code Logic（这个函数做什么）:
///     对每个 Git 相对路径复用 safe path 校验，并读取为 UTF-8；内容不拼进 prompt，仅用于 fail-closed 验证。
pub(crate) fn validate_merge_conflict_files_for_agent(
    root: &Path,
    paths: &[String],
) -> Result<(), AppError> {
    for path in paths {
        validate_merge_resolution_path(path)?;
        let full_path = safe_merge_resolution_path(root, path)?;
        std::fs::read_to_string(&full_path).map_err(|error| {
            AppError::generic(format!(
                "读取冲突文件 {} 失败（仅支持 UTF-8 文本冲突自动解决）: {error}",
                path
            ))
        })?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     受限工具权限是第一道边界；CLI 返回后仍必须用 Git 事实校验，防止模型或本机配置改动了
///     本次冲突清单以外的文件并被后续 `git add -A` 一并提交。
///
/// Code Logic（这个函数做什么）:
///     读取 integration worktree 的 unstaged/untracked 路径，要求每一项都属于原始 conflict_paths。
pub(crate) fn ensure_only_conflict_files_modified(
    root: &Path,
    conflict_paths: &[String],
) -> Result<(), AppError> {
    let modified = workbench_git::unstaged_or_untracked_files(root)?;
    let unexpected = modified
        .into_iter()
        .filter(|path| !conflict_paths.contains(path))
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(AppError::generic(format!(
            "Claude Code 修改了本次冲突清单外的文件，已拒绝提交: {}",
            unexpected.join(", ")
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
///     Claude Code 需要知道它正在可回收的隔离 worktree 内解决冲突，并只编辑后端授权的原冲突文件；
///     prompt 不能再内嵌大文件全文，否则输入和完整 JSON 输出会在真实多文件冲突中稳定超时。
///
/// Code Logic（这个函数做什么）:
///     只把 JSON 转义后的相对路径数组写入短指令，要求 Claude 用 Read/Edit/Write 直接修改并复查文件。
pub(crate) fn build_merge_conflict_edit_instruction(paths: &[String]) -> String {
    let paths = serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_string());
    format!(
        "Resolve the existing Git merge conflicts directly in the current isolated integration worktree.\n\
         Use the repository instructions and the Read tool to inspect the listed files and any necessary project context.\n\
         Requirements:\n\
         - Edit every path in the exact JSON array below to produce coherent final source that preserves both sides' intent.\n\
         - Edit, write, delete, or rename no file outside that array.\n\
         - Do not invoke Git or shell commands; they are intentionally unavailable.\n\
         - Do not leave conflict markers such as <<<<<<<, |||||||, =======, or >>>>>>>.\n\
         - For large files, read focused chunks around each marker plus enough surrounding context before editing.\n\
         - Before finishing, re-read every listed file and confirm all conflict markers are gone.\n\n\
         Exact conflicted path array:\n{paths}"
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
    let op_id = format!("compat-{}", uuid::Uuid::new_v4());
    match local_remove_workbench_worktree_with_ledger(state, worktree_id, force, op_id).await? {
        WorkbenchMutationEnvelopeDto::Succeeded { value, .. } => Ok(value),
        WorkbenchMutationEnvelopeDto::Unknown { .. } => Err(AppError::unavailable(
            "remove 结果未知（兼容路径）".to_string(),
        )),
        // remove 走 run_claimed_mutation，不会产生 failedHook；防御性 Err。
        WorkbenchMutationEnvelopeDto::FailedHook { hook_failure, .. } => {
            Err(AppError::generic(hook_failure.summary()))
        }
    }
}

/// 带 ledger 的本机 remove。
///
/// Business Logic（为什么需要这个函数）:
///     remove 前捕获 exact worktree identity，timeout 后确认身份缺席。
///
/// Code Logic（这个函数做什么）:
///     intent=path+branch → claim → remove → envelope。
pub(crate) async fn local_remove_workbench_worktree_with_ledger(
    state: &AppState,
    worktree_id: String,
    force: Option<bool>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<serde_json::Value>, AppError> {
    state.runtime_role.require_owner()?;
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    let row = state
        .workbench_worktree_repo
        .get(&worktree_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台 worktree 不存在"))?;
    if row.is_main {
        return Err(AppError::generic("不能删除主工作区"));
    }
    let intent = MutationIntent::Remove {
        project_id: row.project_id.clone(),
        worktree_id: worktree_id.clone(),
        path: row.path.clone(),
        branch: row.branch.clone(),
    };
    let force_flag = force.unwrap_or(false);
    let payload = canonical_remove_payload(&worktree_id, force_flag);
    let payload_hash = hash_canonical_payload(&payload)?;
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    let claim = ledger
        .claim(&op_id, MutationKind::Remove, &payload_hash, &intent)
        .await?;
    let state_for_exec = state.clone();
    let wt_for_exec = worktree_id.clone();
    let row_for_exec = row.clone();
    run_claimed_mutation(&ledger, &op_id, claim, move || async move {
        let sessions = state_for_exec
            .workbench_session_repo
            .list(Some(&row_for_exec.project_id))
            .await?;
        if sessions
            .iter()
            .any(|session| session.worktree_id.as_deref() == Some(&wt_for_exec))
        {
            return Err(AppError::generic("请先关闭该 worktree 下的终端窗口"));
        }
        let project = get_project(&state_for_exec, &row_for_exec.project_id).await?;
        let repo_root = workbench_git::repo_root(Path::new(&project.path))?;
        workbench_git::remove_worktree(
            Path::new(&repo_root),
            Path::new(&row_for_exec.path),
            force_flag,
        )?;
        state_for_exec
            .workbench_worktree_repo
            .delete(&wt_for_exec)
            .await?;
        // 删除后 refresh：对应 binding 标记 detached，不 tombstone Hub 资产。
        if let Err(err) = crate::agent_hub::project_scope::refresh_checkout_bindings(
            &state_for_exec,
            &row_for_exec.project_id,
        )
        .await
        {
            tracing::debug!(
                project_id = %row_for_exec.project_id,
                worktree_id = %wt_for_exec,
                error = %err,
                "agent_hub refresh_checkout_bindings after remove worktree failed"
            );
        }
        Ok(serde_json::json!({ "ok": true, "worktreeId": wt_for_exec }))
    })
    .await
}

/// 删除一个非主 worktree（ledger + envelope）。
///
/// Business Logic（为什么需要这个函数）:
///     local/remote remove 共用 typed envelope。
///
/// Code Logic（这个函数做什么）:
///     Local ledger；Remote capability 或 legacy 映射。
pub(crate) async fn remove_workbench_worktree_for_state(
    state: &AppState,
    worktree_id: String,
    force: Option<bool>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<serde_json::Value>, AppError> {
    let op_id = normalize_client_operation_id(&client_operation_id)?;
    match worktree_command_target(&worktree_id)? {
        WorktreeCommandTarget::Remote {
            device_id,
            inner_worktree_id,
        } => {
            let context =
                ensure_remote_worktree_context(state, device_id, inner_worktree_id).await?;
            let client = RemoteWorkbenchClient::new().with_expected_device_id(&context.device_id);
            let supports = client
                .peer_supports_capability(
                    &context.base_url,
                    CAPABILITY_WORKBENCH_MUTATION_OUTCOME_V1,
                )
                .await
                .unwrap_or(false);
            if supports {
                let envelope = client
                    .remove_worktree_envelope(
                        &context.base_url,
                        &context.inner_worktree_id,
                        force,
                        Some(op_id.clone()),
                    )
                    .await?;
                Ok(match envelope {
                    WorkbenchMutationEnvelopeDto::Succeeded {
                        value,
                        client_operation_id,
                    } => WorkbenchMutationEnvelopeDto::succeeded(
                        map_remote_worktree_json_value(&context.device_id, value),
                        client_operation_id,
                    ),
                    other => other,
                })
            } else {
                let legacy = client
                    .remove_worktree(&context.base_url, &context.inner_worktree_id, force)
                    .await
                    .map(|value| map_remote_worktree_json_value(&context.device_id, value));
                map_legacy_mutation_result(legacy, &op_id)
            }
        }
        WorktreeCommandTarget::Local(local_worktree_id) => {
            local_remove_workbench_worktree_with_ledger(state, local_worktree_id, force, op_id)
                .await
        }
    }
}

/// 删除一个非主 worktree。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 remove 返回 typed envelope。
///
/// Code Logic（这个命令做什么）:
///     GuiClient mutation proxy；owner for_state。
#[tauri::command]
pub async fn remove_workbench_worktree(
    state: State<'_, AppState>,
    worktree_id: String,
    force: Option<bool>,
    client_operation_id: String,
) -> Result<WorkbenchMutationEnvelopeDto<serde_json::Value>, AppError> {
    if let Some(v) = proxy_workbench_mutation_if_gui(
        state.inner(),
        "worktrees.remove",
        serde_json::json!({
            "worktreeId": worktree_id.clone(),
            "force": force,
            "clientOperationId": client_operation_id.clone(),
        }),
        &client_operation_id,
    )
    .await?
    {
        return Ok(v);
    }
    remove_workbench_worktree_for_state(state.inner(), worktree_id, force, client_operation_id)
        .await
}

/// 查询 mutation operation ledger 状态。
///
/// Business Logic（为什么需要这个命令）:
///     unknown 后 controller 按 clientOperationId 查询 owning ledger 取得 intent/state。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 代理；owner 读 SQLite ledger。
#[tauri::command]
pub async fn get_workbench_mutation_operation(
    state: State<'_, AppState>,
    client_operation_id: String,
) -> Result<Option<WorkbenchMutationOperationDto>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "worktrees.mutation_operation",
        serde_json::json!({ "clientOperationId": client_operation_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_workbench_mutation_operation_for_state(state.inner(), client_operation_id).await
}

/// Business Logic: owner 路径查询 ledger。
/// Code Logic: WorkbenchMutationLedger::get。
pub(crate) async fn get_workbench_mutation_operation_for_state(
    state: &AppState,
    client_operation_id: String,
) -> Result<Option<WorkbenchMutationOperationDto>, AppError> {
    let ledger = WorkbenchMutationLedger::new(state.db.clone());
    // 幂等建表：旧库 / 测试库无 migration 时 query 路径也可用
    ledger.ensure_schema().await?;
    ledger.get(&client_operation_id).await
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
            .with_expected_device_id(&context.device_id)
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
            .with_expected_device_id(&context.device_id)
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

#[cfg(test)]
mod collect_merge_tests {
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
    use crate::workbench::models::WorkbenchProjectRow;
    use crate::workbench::operation_ledger::WorkbenchMutationEnvelopeDto;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    const COLLECT_PROJECT_ID: &str = "p-collect";

    struct CollectMergeRepo {
        root: PathBuf,
        repo: PathBuf,
        agent_a_oid: String,
        agent_b_oid: String,
    }

    /// Business Logic（为什么需要这个函数）:
    ///     collect-merge 命令测试需要真实 Git 仓库：main + 两个无冲突 agent 分支，live 停在 agent/a。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 root 下 init 仓库，提交 base / agent/a / agent/b，最后 checkout agent/a。
    fn setup_collect_merge_repo(root: &Path) -> CollectMergeRepo {
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_cmd(&repo, &["init", "-b", "main"]);
        git_cmd(&repo, &["config", "user.email", "test@example.com"]);
        git_cmd(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_cmd(&repo, &["add", "README.md"]);
        git_cmd(&repo, &["commit", "-m", "initial"]);

        git_cmd(&repo, &["checkout", "-b", "agent/a"]);
        fs::write(repo.join("agent-a.txt"), "a\n").expect("write agent a");
        git_cmd(&repo, &["add", "agent-a.txt"]);
        git_cmd(&repo, &["commit", "-m", "agent a work"]);
        let agent_a_oid = git_cmd(&repo, &["rev-parse", "HEAD"]).trim().to_string();

        git_cmd(&repo, &["checkout", "main"]);
        git_cmd(&repo, &["checkout", "-b", "agent/b"]);
        fs::write(repo.join("agent-b.txt"), "b\n").expect("write agent b");
        git_cmd(&repo, &["add", "agent-b.txt"]);
        git_cmd(&repo, &["commit", "-m", "agent b work"]);
        let agent_b_oid = git_cmd(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        git_cmd(&repo, &["checkout", "agent/a"]);

        CollectMergeRepo {
            root: root.to_path_buf(),
            repo,
            agent_a_oid,
            agent_b_oid,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     无源分支场景必须在 checkSource 失败，而不是走进隔离 merge。
    ///
    /// Code Logic（这个函数做什么）:
    ///     只创建 main 上的一次提交。
    fn setup_main_only_repo(root: &Path) -> PathBuf {
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_cmd(&repo, &["init", "-b", "main"]);
        git_cmd(&repo, &["config", "user.email", "test@example.com"]);
        git_cmd(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_cmd(&repo, &["add", "README.md"]);
        git_cmd(&repo, &["commit", "-m", "initial"]);
        repo
    }

    fn git_cmd(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).to_string();
        }
        panic!(
            "git {:?} failed in {}:\nstdout:\n{}\nstderr:\n{}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     collect-merge 集成测试必须把 db_path 放到临时目录下，避免 merge-integrations 落到仓库 cwd。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复制 restore-fail AppState 最小字段，db_path=temp/data.db，并 upsert 真实 git 路径的主 worktree。
    async fn build_collect_merge_state(temp_dir: &Path, project_path: &Path) -> AppState {
        let db_path = temp_dir.join("data.db");
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

        let project_path = project_path.to_string_lossy().to_string();
        project_repo
            .upsert(&WorkbenchProjectRow {
                id: COLLECT_PROJECT_ID.to_string(),
                name: "collect".to_string(),
                kind: "local".to_string(),
                device_id: "d1".to_string(),
                device_name: "local".to_string(),
                path: project_path.clone(),
                last_opened_at: "t".to_string(),
                created_at: "t".to_string(),
                updated_at: "t".to_string(),
            })
            .await
            .unwrap();
        worktree_repo
            .upsert(&WorkbenchWorktreeRow {
                id: main_worktree_id(COLLECT_PROJECT_ID),
                project_id: COLLECT_PROJECT_ID.to_string(),
                name: "main".to_string(),
                branch: Some("main".to_string()),
                base_branch: None,
                path: project_path,
                is_main: true,
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
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: db_path.to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_optimizer_provider: "claude".into(),
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
            attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
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
                    temp_dir.join("browser-verification-collect"),
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

    /// Business Logic（为什么需要这个测试）:
    ///     主工作区 Merge 应把 agent/a、agent/b 收进 home，回到 main，并保留主 worktree 行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     live 停在 agent/a 时调用 ledger merge；断言 envelope 成功、HEAD 含两源、源分支删除、
    ///     closeSessions=skipped。
    #[tokio::test]
    async fn collect_merge_happy_path_merges_local_branches_into_home() {
        let temp = tempfile::tempdir().expect("temp dir");
        let fixture = setup_collect_merge_repo(temp.path());
        let state = build_collect_merge_state(temp.path(), &fixture.repo).await;
        let main_id = main_worktree_id(COLLECT_PROJECT_ID);
        let op_id = uuid::Uuid::new_v4().to_string();

        let envelope = local_merge_workbench_worktree_with_ledger(&state, main_id.clone(), op_id)
            .await
            .expect("collect-merge should succeed");
        let WorkbenchMutationEnvelopeDto::Succeeded { value, .. } = envelope else {
            panic!("expected succeeded envelope, got {envelope:?}");
        };
        assert!(value.ok);
        let close = value
            .stages
            .iter()
            .find(|stage| stage.id == MERGE_STAGE_CLOSE_SESSIONS)
            .expect("closeSessions stage");
        assert_eq!(close.status, "skipped");
        assert!(close.message.contains("主工作区收集合并不关闭终端"));

        assert_eq!(
            workbench_git::current_branch(&fixture.repo).as_deref(),
            Some("main")
        );
        let head = workbench_git::head_hash(&fixture.repo)
            .unwrap()
            .expect("head after collect-merge");
        assert!(
            workbench_git::is_ancestor(&fixture.repo, &fixture.agent_a_oid, &head).unwrap(),
            "main should contain agent/a"
        );
        assert!(
            workbench_git::is_ancestor(&fixture.repo, &fixture.agent_b_oid, &head).unwrap(),
            "main should contain agent/b"
        );
        assert!(
            git_cmd(&fixture.repo, &["branch", "--list", "agent/a"])
                .trim()
                .is_empty(),
            "agent/a should be deleted"
        );
        assert!(
            git_cmd(&fixture.repo, &["branch", "--list", "agent/b"])
                .trim()
                .is_empty(),
            "agent/b should be deleted"
        );
        let main_row = state
            .workbench_worktree_repo
            .get(&main_id)
            .await
            .unwrap()
            .expect("main worktree row must remain");
        assert!(main_row.is_main);
        let _ = fixture.root;
    }

    /// Business Logic（为什么需要这个测试）:
    ///     live 主工作区有未提交改动时绝不能开始收集合并，否则会弄丢用户工作。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入未跟踪文件后调用 ledger merge，断言 checkSource 失败。
    #[tokio::test]
    async fn collect_merge_dirty_live_fails_check_source() {
        let temp = tempfile::tempdir().expect("temp dir");
        let fixture = setup_collect_merge_repo(temp.path());
        fs::write(fixture.repo.join("dirty.txt"), "dirty\n").expect("write dirty file");
        let state = build_collect_merge_state(temp.path(), &fixture.repo).await;
        let error = local_merge_workbench_worktree_with_ledger(
            &state,
            main_worktree_id(COLLECT_PROJECT_ID),
            uuid::Uuid::new_v4().to_string(),
        )
        .await
        .expect_err("dirty live must fail collect-merge");
        let message = error.to_string();
        assert!(
            message.contains("未提交改动"),
            "expected dirty checkSource error, got {message}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     只有 home、没有可收集分支时不能凭空跑 isolation merge。
    ///
    /// Code Logic（这个测试做什么）:
    ///     仅 main 分支的仓库调用 ledger merge，断言 validation/generic 错误。
    #[tokio::test]
    async fn collect_merge_without_collectible_branches_fails() {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = setup_main_only_repo(temp.path());
        let state = build_collect_merge_state(temp.path(), &repo).await;
        let error = local_merge_workbench_worktree_with_ledger(
            &state,
            main_worktree_id(COLLECT_PROJECT_ID),
            uuid::Uuid::new_v4().to_string(),
        )
        .await
        .expect_err("empty collectible set must fail");
        let message = error.to_string();
        assert!(
            message.contains("没有可收集") || message.contains("可收集"),
            "expected no-collectible error, got {message}"
        );
    }
}
