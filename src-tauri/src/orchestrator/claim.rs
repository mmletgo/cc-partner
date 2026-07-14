//! Orchestrator claim 候选快照与扫描游标（S5 三阶段 claim 的阶段 A 类型面）。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 scheduler 在大量 Queued 任务下不能无界 SELECT 候选，否则单 tick 占用唯一 SQLite 连接过久。
//!     阶段 A 只做有界 keyset 读取，把 workflow 文件 IO 与 CAS 写事务留给后续阶段。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `CLAIM_CANDIDATE_LIMIT`/`CLAIM_PROJECT_LIMIT`、`ClaimScanCursor`、`ClaimCandidate` 等稳定类型；
//!     实际有界 SELECT 由 `OrchestratorRepo::list_local_queued_claim_candidates` 实现。

use crate::orchestrator::models::OrchestratorTaskRow;
use std::path::PathBuf;

/// 单次 claim 候选 SELECT 的硬上限（含 keyset 分页页大小）。
pub const CLAIM_CANDIDATE_LIMIT: u32 = 256;

/// 阶段 B 解析 WORKFLOW.md 时允许的最大不同 project 数（本 task 只导出常量，T3 消费）。
#[allow(dead_code)]
pub const CLAIM_PROJECT_LIMIT: usize = 64;

/// 进程内 claim 扫描 keyset 游标（不持久化、不参与任务正确性）。
///
/// Business Logic（为什么需要这个结构体）:
///     当前 256 候选窗口被无效 workflow 项目占满时，下一 tick 必须从上次扫描边界继续，
///     否则窗口之后的合法任务会永久饥饿。
///
/// Code Logic（这个结构体做什么）:
///     保存排序键 `(priority DESC, created_at ASC, id ASC)` 中最后一行的三元组，供下一页 keyset 谓词使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimScanCursor {
    pub priority: i64,
    pub created_at: String,
    pub id: String,
}

impl ClaimScanCursor {
    /// Business Logic（为什么需要这个函数）:
    ///     扫描页末行需要稳定编码为下一页游标，避免调用方手写三字段赋值遗漏。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从候选任务行提取 priority/created_at/id 构造 `ClaimScanCursor`。
    pub fn from_task(task: &OrchestratorTaskRow) -> Self {
        Self {
            priority: task.priority,
            created_at: task.created_at.clone(),
            id: task.id.clone(),
        }
    }
}

/// 阶段 A 读出的本机 Queued/Idle 候选（任务行 + JOIN 得到的 project path）。
///
/// Business Logic（为什么需要这个结构体）:
///     后续 preflight 需要项目路径解析 WORKFLOW.md，但不能在事务内二次查询 project 表。
///
/// Code Logic（这个结构体做什么）:
///     一次 JOIN 快照：`task` 为完整 `OrchestratorTaskRow`，`project_path` 为 local Workbench 项目根路径。
#[derive(Debug, Clone)]
pub struct ClaimCandidate {
    pub task: OrchestratorTaskRow,
    pub project_path: PathBuf,
}

impl ClaimCandidate {
    /// Business Logic（为什么需要这个函数）:
    ///     分页后要把页末候选编码为扫描游标，供 scheduler 下一 tick 继续扫描。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `ClaimScanCursor::from_task` 从本候选任务行生成游标。
    pub fn to_scan_cursor(&self) -> ClaimScanCursor {
        ClaimScanCursor::from_task(&self.task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{
        OrchestratorRunState, OrchestratorTaskStatus, OrchestratorWorkflowState,
    };

    /// Business Logic（为什么需要这个测试）:
    ///     游标编码必须稳定复用任务排序键，否则 keyset 翻页会跳行或重复。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造最小任务行，断言 from_task/to_scan_cursor 三字段与任务一致。
    #[test]
    fn claim_scan_cursor_from_task_copies_sort_keys() {
        let task = OrchestratorTaskRow {
            id: "task-z".to_string(),
            project_id: "local-a".to_string(),
            title: "t".to_string(),
            goal: "g".to_string(),
            acceptance_criteria: "c".to_string(),
            status: OrchestratorTaskStatus::Queued,
            workflow_state: OrchestratorWorkflowState::Todo,
            run_state: OrchestratorRunState::Idle,
            attempt_phase: None,
            source: "internal".to_string(),
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
            runner_provider: None,
            claude_session_id: None,
            transcript_path: None,
            runtime_started_at: None,
            last_activity_at: None,
            last_runtime_event: None,
            last_runtime_message: None,
            priority: 7,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:01Z".to_string(),
            updated_at: "2026-07-05T00:00:01Z".to_string(),
            started_at: None,
            finished_at: None,
        };
        let cursor = ClaimScanCursor::from_task(&task);
        assert_eq!(cursor.priority, 7);
        assert_eq!(cursor.created_at, "2026-07-05T00:00:01Z");
        assert_eq!(cursor.id, "task-z");

        let candidate = ClaimCandidate {
            task: task.clone(),
            project_path: PathBuf::from("/tmp/local-a"),
        };
        assert_eq!(candidate.to_scan_cursor(), cursor);
        assert_eq!(CLAIM_CANDIDATE_LIMIT, 256);
        assert_eq!(CLAIM_PROJECT_LIMIT, 64);
    }
}
