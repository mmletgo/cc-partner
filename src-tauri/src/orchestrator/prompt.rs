//! Orchestrator task prompt generation.
//!
//! Business Logic（为什么需要这个模块）:
//!     可见 Runner 需要把任务目标、验收标准和项目路径写入 Claude Code 终端，让执行者在隔离
//!     worktree 中拿到完整任务上下文。
//!
//! Code Logic（这个模块做什么）:
//!     提供任务 Prompt 生成 helper，并用单测锁定目标、验收标准和路径必须出现在 Prompt 中。

use crate::orchestrator::models::OrchestratorTaskRow;

/// Business Logic（为什么需要这个函数）:
///     可见 Runner 启动 Claude Code 后，需要把任务边界、验收标准和项目位置一次性交给执行 worker。
///
/// Code Logic（这个函数做什么）:
///     从 OrchestratorTaskRow 提取标题、目标和验收标准，并拼接项目路径及执行约束文本返回。
pub fn build_task_prompt(task: &OrchestratorTaskRow, project_path: &str) -> String {
    format!(
        "请在当前项目中完成 Orchestrator 任务。\n\n\
任务标题：{}\n\n\
任务目标：\n{}\n\n\
验收标准：\n{}\n\n\
项目路径：{}\n\n\
执行要求：\n\
1. 先阅读并遵守项目根目录 AGENTS.md；进入子目录时继续遵守该目录的 AGENTS.md，若没有 AGENTS.md 但有 CLAUDE.md，则遵守 CLAUDE.md。\n\
2. 严格围绕本任务目标和验收标准实现，不要扩大到未要求的功能或无关变更。\n\
3. 完成后说明你运行过的验证方式、仍未验证的风险和需要人工关注的风险。\n\
4. 不要自行清理、删除、合并当前 worktree，也不要自动提交或推送；保留现场供 Orchestrator/Workbench 接管。\n",
        task.title.trim(),
        task.goal.trim(),
        task.acceptance_criteria.trim(),
        project_path.trim()
    )
}

#[cfg(test)]
mod tests {
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
}
