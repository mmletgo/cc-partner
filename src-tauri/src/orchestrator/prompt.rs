//! Orchestrator task prompt generation.
//!
//! Business Logic（为什么需要这个模块）:
//!     可见 Runner 需要把任务目标、验收标准和项目路径写入 Claude Code 终端，让执行者在隔离
//!     worktree 中拿到完整任务上下文。
//!
//! Code Logic（这个模块做什么）:
//!     提供任务 Prompt 生成 helper，并用单测锁定目标、验收标准和路径必须出现在 Prompt 中。

use crate::orchestrator::models::OrchestratorTaskRow;

/// Claude Code 开发完成哨兵。
///
/// Business Logic（为什么需要这个常量）:
///     可见 Runner 完成开发后需要一个稳定、机器可识别的终端输出信号，自动触发 Orchestrator 验证/交付流程。
///
/// Code Logic（这个常量做什么）:
///     保存终端 completion detector 识别的固定字符串，Prompt 生成和输出检测共用同一来源。
pub const DEV_DONE_SENTINEL: &str = "ORCHESTRATOR_DEV_DONE";

/// 修复轮次 Prompt 上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     后续 verifier/repair loop 会把上一轮验证失败原因和修复指令传回 Claude Code，指导它在同一 worktree 中修复。
///
/// Code Logic（这个结构体做什么）:
///     持有 verifier_reason 与 repair_prompt 两段轻量文本，不依赖尚未落地的 verifier 模块。
#[derive(Debug, Clone, Copy)]
pub struct RepairPromptContext<'a> {
    pub verifier_reason: &'a str,
    pub repair_prompt: &'a str,
}

/// Business Logic（为什么需要这个函数）:
///     任务标题、目标、验收标准和修复上下文都来自用户或模型输出，不能让其中的 sentinel 原样形成独立终端行。
///
/// Code Logic（这个函数做什么）:
///     对用户可控文本 trim 后逐行加 Markdown 引用前缀 `> `；空文本输出占位行，保持 Prompt 可读且避免裸哨兵行。
pub(crate) fn render_user_block(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "> （未填写）".to_string();
    }
    trimmed
        .lines()
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            format!("> {line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Business Logic（为什么需要这个函数）:
///     Prompt 生成器和 workflow 模板都必须避免把完成哨兵作为独立行写入终端回显，否则会提前触发验证。
///
/// Code Logic（这个函数做什么）:
///     按行检查去掉 CR 后是否精确等于 DEV_DONE_SENTINEL，命中返回 true。
pub(crate) fn contains_standalone_dev_done_sentinel(value: &str) -> bool {
    value
        .lines()
        .any(|line| line.strip_suffix('\r').unwrap_or(line) == DEV_DONE_SENTINEL)
}

/// Business Logic（为什么需要这个函数）:
///     可见 Runner 启动 Claude Code 后，需要把任务边界、验收标准和项目位置一次性交给执行 worker。
///
/// Code Logic（这个函数做什么）:
///     兼容旧调用点，直接转发到 build_initial_task_prompt。
#[allow(dead_code)]
pub fn build_task_prompt(task: &OrchestratorTaskRow, project_path: &str) -> String {
    build_initial_task_prompt(task, project_path)
}

/// Business Logic（为什么需要这个函数）:
///     首轮可见 Runner 启动 Claude Code 后，需要把任务边界、验收标准、worktree 路径和完成哨兵协议一次性交给执行 worker。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorTaskRow 提取标题、目标和验收标准，并拼接项目路径、执行约束和 DEV_DONE_SENTINEL 输出规则。
pub fn build_initial_task_prompt(task: &OrchestratorTaskRow, worktree_path: &str) -> String {
    format!(
        "请在当前项目中完成 Orchestrator 任务。\n\n\
任务标题：\n{}\n\n\
任务目标：\n{}\n\n\
验收标准：\n{}\n\n\
项目路径：{}\n\n\
执行要求：\n\
1. 先阅读并遵守项目根目录 AGENTS.md；进入子目录时继续遵守该目录的 AGENTS.md，若没有 AGENTS.md 但有 CLAUDE.md，则遵守 CLAUDE.md。\n\
2. 严格围绕本任务目标和验收标准实现，不要扩大到未要求的功能或无关变更。\n\
3. 完成后说明你运行过的验证方式、仍未验证的风险和需要人工关注的风险。\n\
4. 不要自行清理、删除、合并当前 worktree，也不要自动提交或推送；保留现场供 Orchestrator/Workbench 接管。\n\
5. 只有在你已经完成代码、更改过的相关测试/验证、并给出必要证据说明后，最后单独输出 `{}`。\n\
6. 未完成代码、未运行必要测试/验证或还没有证据说明时，绝对不要输出 `{}`。\n",
        render_user_block(&task.title),
        render_user_block(&task.goal),
        render_user_block(&task.acceptance_criteria),
        worktree_path.trim(),
        DEV_DONE_SENTINEL,
        DEV_DONE_SENTINEL
    )
}

/// Business Logic（为什么需要这个函数）:
///     修复轮次需要在同一任务上下文中明确上一轮 verifier 失败原因和本轮修复指令，让 Claude Code 聚焦修复。
///
/// Code Logic（这个函数做什么）:
///     复用任务标题、目标、验收标准和 worktree 路径，同时追加 previous verifier reason、repair prompt 与完成哨兵规则。
pub fn build_repair_task_prompt(
    task: &OrchestratorTaskRow,
    worktree_path: &str,
    context: &RepairPromptContext<'_>,
) -> String {
    format!(
        "请在当前项目中修复 Orchestrator 任务。\n\n\
任务标题：\n{}\n\n\
任务目标：\n{}\n\n\
验收标准：\n{}\n\n\
项目路径：{}\n\n\
Previous verifier reason：\n{}\n\n\
Repair prompt：\n{}\n\n\
执行要求：\n\
1. 先阅读并遵守项目根目录 AGENTS.md；进入子目录时继续遵守该目录的 AGENTS.md，若没有 AGENTS.md 但有 CLAUDE.md，则遵守 CLAUDE.md。\n\
2. 严格围绕 previous verifier reason 与 repair prompt 修复，不要扩大到未要求的功能或无关变更。\n\
3. 完成后说明你运行过的验证方式、仍未验证的风险和需要人工关注的风险。\n\
4. 不要自行清理、删除、合并当前 worktree，也不要自动提交或推送；保留现场供 Orchestrator/Workbench 接管。\n\
5. 只有在你已经完成代码、更改过的相关测试/验证、并给出必要证据说明后，最后单独输出 `{}`。\n\
6. 未完成代码、未运行必要测试/验证或还没有证据说明时，绝对不要输出 `{}`。\n",
        render_user_block(&task.title),
        render_user_block(&task.goal),
        render_user_block(&task.acceptance_criteria),
        worktree_path.trim(),
        render_user_block(context.verifier_reason),
        render_user_block(context.repair_prompt),
        DEV_DONE_SENTINEL,
        DEV_DONE_SENTINEL
    )
}

#[cfg(test)]
mod tests {
    use super::{build_initial_task_prompt, build_repair_task_prompt, RepairPromptContext};
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};

    /// Business Logic（为什么需要这个函数）:
    ///     Prompt 测试需要构造完整任务 Row，确保生成器读取的是真实数据库行字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回包含稳定 id、项目、标题、目标和验收标准的 Queued 任务 Row。
    fn task_row() -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Fix screenshot clipboard".to_string(),
            goal: "修复截图保存失败".to_string(),
            acceptance_criteria: "截图可复制到剪贴板".to_string(),
            status: OrchestratorTaskStatus::Queued,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(OrchestratorTaskStatus::Queued)
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude Code 执行任务时必须看到目标、验收标准和当前 worktree 路径，否则无法判断完成边界。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 Prompt 生成器，并断言输出包含 goal、acceptance criteria 和 project path。
    #[test]
    fn prompt_contains_goal_and_acceptance() {
        let task = task_row();
        let prompt = super::build_task_prompt(&task, "/repo/worktree");

        assert!(prompt.contains("修复截图保存失败"));
        assert!(prompt.contains("截图可复制到剪贴板"));
        assert!(prompt.contains("/repo/worktree"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 7 依赖开发终端最后输出固定哨兵自动触发验证，初始任务 Prompt 必须明确给 Claude Code 这个完成协议。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成初始任务 Prompt，断言包含 sentinel，并明确要求只有完成代码、验证和证据说明后才单独输出。
    #[test]
    fn initial_prompt_contains_completion_sentinel_and_guardrail() {
        let task = task_row();
        let prompt = build_initial_task_prompt(&task, "/repo/worktree");

        assert!(prompt.contains(super::DEV_DONE_SENTINEL));
        assert!(prompt.contains("最后单独输出"));
        assert!(prompt.contains("完成代码"));
        assert!(prompt.contains("测试/验证"));
        assert!(prompt.contains("必要证据说明"));
        assert!(prompt.contains("未完成"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     后续 Phase 8 的修复轮次需要把上一轮 verifier 失败原因与修复指令传给 Claude Code，避免重复盲修。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用轻量上下文生成 repair Prompt，断言 verifier_reason、repair_prompt 和完成 sentinel 协议都在输出中。
    #[test]
    fn repair_prompt_contains_verifier_reason_repair_prompt_and_sentinel() {
        let task = task_row();
        let context = RepairPromptContext {
            verifier_reason: "测试失败：cargo test orchestrator::completion --lib 超时",
            repair_prompt: "只修复 completion detector 的跨 chunk 状态",
        };
        let prompt = build_repair_task_prompt(&task, "/repo/worktree", &context);

        assert!(prompt.contains(context.verifier_reason));
        assert!(prompt.contains(context.repair_prompt));
        assert!(prompt.contains(super::DEV_DONE_SENTINEL));
        assert!(prompt.contains("完成代码"));
        assert!(prompt.contains("测试/验证"));
        assert!(prompt.contains("必要证据说明"));
        assert!(prompt.contains("未完成"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可控任务文本和 repair 上下文可能包含 sentinel，Prompt 回显不能生成原样独立哨兵行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 title/goal/acceptance/verifier_reason/repair_prompt 都包含独立 sentinel 行的 Prompt，
    ///     断言输出中不存在 trim 后等于 sentinel 的行，同时保留带引用前缀的可读文本。
    #[test]
    fn user_controlled_prompt_blocks_do_not_emit_standalone_sentinel_lines() {
        let mut task = task_row();
        task.title = format!("标题\n{}\n后续标题", super::DEV_DONE_SENTINEL);
        task.goal = format!("目标\n{}\n后续目标", super::DEV_DONE_SENTINEL);
        task.acceptance_criteria = format!("验收\n{}\n后续验收", super::DEV_DONE_SENTINEL);
        let context = RepairPromptContext {
            verifier_reason: concat!("验证失败\n", "ORCHESTRATOR_DEV_DONE", "\n重新检查"),
            repair_prompt: concat!("修复要求\n", "ORCHESTRATOR_DEV_DONE", "\n只改相关文件"),
        };

        let initial = build_initial_task_prompt(&task, "/repo/worktree");
        let repair = build_repair_task_prompt(&task, "/repo/worktree", &context);

        for prompt in [&initial, &repair] {
            assert!(
                !prompt
                    .lines()
                    .any(|line| line.strip_suffix('\r').unwrap_or(line) == super::DEV_DONE_SENTINEL),
                "prompt must not contain a raw standalone sentinel line:\n{prompt}"
            );
            assert!(prompt.contains("> ORCHESTRATOR_DEV_DONE"));
        }
    }
}
