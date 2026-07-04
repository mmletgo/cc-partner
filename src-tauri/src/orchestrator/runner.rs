//! Orchestrator visible Workbench runner.
//!
//! Business Logic（为什么需要这个模块）:
//!     调度器领取任务后，需要为任务准备用户可见的 Workbench worktree 和 terminal window，并把
//!     Claude Code 执行 Prompt 写入终端。
//!
//! Code Logic（这个模块做什么）:
//!     封装 runner 准备流程与分支名生成 helper；Git worktree 与 terminal session 实际创建复用
//!     Workbench 既有命令层本机 helper。

use crate::commands::workbench::{
    local_create_workbench_session, local_create_workbench_worktree,
    local_write_workbench_session_input,
};
use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskRow;
use crate::orchestrator::prompt::build_task_prompt;
use crate::state::AppState;
use tauri::AppHandle;

const TASK_BRANCH_PREFIX: &str = "agent";
const TASK_BRANCH_MAX_LEN: usize = 80;
const DEFAULT_TERMINAL_COLS: u16 = 120;
const DEFAULT_TERMINAL_ROWS: u16 = 32;

/// Business Logic（为什么需要这个函数）:
///     调度器领取任务后，需要创建隔离 worktree 和可见终端，并把 Claude Code 任务 Prompt 写入现场。
///
/// Code Logic（这个函数做什么）:
///     校验任务绑定本机 Workbench 项目，生成安全分支名，复用 Workbench 本机 helper 创建 worktree/session，
///     写入 `claude` 与任务 Prompt，最后把任务状态和现场 id 持久化为 Running。
pub async fn prepare_visible_runner(
    state: &AppState,
    app_handle: AppHandle,
    task: &OrchestratorTaskRow,
) -> Result<OrchestratorTaskRow, AppError> {
    let project = state
        .workbench_project_repo
        .get(&task.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台项目不存在"))?;
    if project.kind != "local" {
        return Err(AppError::generic(
            "Orchestrator Runner 目前只支持本机 Workbench 项目",
        ));
    }

    let branch_name = task_branch_name(&task.id, &task.title);
    let worktree =
        local_create_workbench_worktree(state, task.project_id.clone(), branch_name.clone(), None)
            .await?;
    let session = local_create_workbench_session(
        state,
        app_handle,
        task.project_id.clone(),
        Some(worktree.id.clone()),
        Some(DEFAULT_TERMINAL_COLS),
        Some(DEFAULT_TERMINAL_ROWS),
    )
    .await?;
    local_write_workbench_session_input(state, session.id.clone(), "claude\n".to_string()).await?;
    let prompt = build_task_prompt(task, &worktree.path);
    local_write_workbench_session_input(state, session.id.clone(), format!("{prompt}\n")).await?;

    state
        .orchestrator_repo
        .mark_task_running(&task.id, &branch_name, &worktree.id, &session.id)
        .await
}

/// Business Logic（为什么需要这个函数）:
///     每个任务都需要稳定、可追溯且 Git branch 安全的分支名，便于 Workbench 创建独立 worktree。
///
/// Code Logic（这个函数做什么）:
///     使用 `agent/<task-id-short>-<slug-title>` 格式，清洗标题并把总长度限制在 80 字符内。
pub(crate) fn task_branch_name(task_id: &str, title: &str) -> String {
    let task_id_short = task_id_short(task_id);
    let slug = slug_title(title);
    let prefix = format!("{TASK_BRANCH_PREFIX}/{task_id_short}-");
    let slug_len = TASK_BRANCH_MAX_LEN.saturating_sub(prefix.len()).max(1);
    format!("{prefix}{}", truncate_slug(&slug, slug_len))
}

/// Business Logic（为什么需要这个函数）:
///     任务 id 通常是 UUID，分支名只需要短前缀即可定位任务，同时避免分支名过长。
///
/// Code Logic（这个函数做什么）:
///     保留 task_id 中前 8 个 ASCII 字母数字字符；若清洗后为空则回退为 `task`。
fn task_id_short(task_id: &str) -> String {
    let value: String = task_id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if value.is_empty() {
        "task".to_string()
    } else {
        value
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户标题可能包含空格、中文、标点或 Git branch 不安全字符，需要归一为可用于分支的 slug。
///
/// Code Logic（这个函数做什么）:
///     ASCII 字母数字转小写保留，其它字符折叠为单个 `-`，首尾分隔符移除，空结果回退为 `task`。
fn slug_title(title: &str) -> String {
    let mut output = String::new();
    let mut previous_dash = false;
    for ch in title.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }
    let trimmed = output.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Business Logic（为什么需要这个函数）:
///     长标题应尽量保留开头语义，但不能让分支名超过本项目约定长度。
///
/// Code Logic（这个函数做什么）:
///     截取 slug 的前 max_len 个字符并移除尾部 `-`，若截断后为空则回退为 `task`。
fn truncate_slug(slug: &str, max_len: usize) -> String {
    let truncated: String = slug.chars().take(max_len).collect();
    let trimmed = truncated.trim_matches('-');
    if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 自动创建的任务分支必须是 Git branch 安全字符串，避免标题中的空格和标点
    ///     破坏 `git worktree add -b`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用含空格、标点和大小写的标题生成分支名，并断言前缀、slug 与非法字符清理结果。
    #[test]
    fn task_branch_name_sanitizes_title() {
        let branch =
            super::task_branch_name("550e8400-e29b-41d4-a716-446655440000", "Fix: UI / Login!");

        assert_eq!(branch, "agent/550e8400-fix-ui-login");
        assert!(!branch.contains(' '));
        assert!(!branch.contains(':'));
        assert!(!branch.contains('!'));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     长任务标题不能生成过长分支名，否则不同 Git 平台或本地路径可能出现不可操作的分支/目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用超长标题生成分支名，断言总长度被限制且尾部不会保留分隔符。
    #[test]
    fn task_branch_name_truncates_long_title() {
        let long_title =
            "Implement very long orchestrator visible workbench runner title ".repeat(4);
        let branch = super::task_branch_name("task-abcdef", &long_title);

        assert!(branch.len() <= 80);
        assert!(!branch.ends_with('-'));
        assert!(branch.starts_with("agent/taskabcd-"));
    }
}
