//! Orchestrator terminal completion detection.
//!
//! Business Logic（为什么需要这个模块）:
//!     可见 Runner 的 Claude Code 开发完成后，会在终端输出固定哨兵；Orchestrator 需要从终端流中
//!     检测该哨兵并触发既有验证/交付流程。
//!
//! Code Logic（这个模块做什么）:
//!     提供开发完成哨兵的纯检测器和终端输出 hook 入口；hook 的异步副作用在实现阶段保持非阻塞。

use crate::commands::orchestrator::complete_orchestrator_agent_run_for_attempt;
use crate::error::AppError;
use crate::orchestrator::prompt::DEV_DONE_SENTINEL;
use crate::state::AppState;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Manager};

const DETECTOR_TAIL_CHARS: usize = DEV_DONE_SENTINEL.len() + 1;

/// 开发完成哨兵检测器。
///
/// Business Logic（为什么需要这个结构体）:
///     终端输出是流式 chunk，Claude Code 可能把完成哨兵拆到相邻 chunk；系统需要只触发一次自动完成流程。
///
/// Code Logic（这个结构体做什么）:
///     保存最近输出尾部和 consumed 标记；push_output 检测 DEV_DONE_SENTINEL，命中后清空 buffer 并拒绝后续重复触发。
#[derive(Debug, Default)]
pub struct DevDoneDetector {
    buffer: String,
    consumed: bool,
}

impl DevDoneDetector {
    /// Business Logic（为什么需要这个函数）:
    ///     terminal reader 每收到一段输出都需要判断是否出现开发完成哨兵。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把 chunk 追加到尾部 buffer，检测独立一行的 sentinel；未命中时只保留足够跨 chunk 匹配的尾部字符。
    pub fn push_output(&mut self, chunk: &str) -> bool {
        if self.consumed {
            return false;
        }
        self.buffer.push_str(chunk);
        if contains_standalone_sentinel(&self.buffer) {
            self.consumed = true;
            self.buffer.clear();
            return true;
        }
        self.trim_buffer();
        false
    }

    /// Business Logic（为什么需要这个函数）:
    ///     completion hook 和测试需要知道 detector 是否已经消费过哨兵，避免重复触发验证/交付。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 consumed 布尔字段。
    #[allow(dead_code)]
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }

    /// Business Logic（为什么需要这个函数）:
    ///     长时间终端输出不能让 detector 持续累积历史文本，占用越来越多内存。
    ///
    /// Code Logic（这个函数做什么）:
    ///     当 buffer 超过 sentinel 尾部长度时，按 char 边界保留 sentinel 和前一位边界字符，避免裁剪后把行尾片段误判为独立行。
    fn trim_buffer(&mut self) {
        let total = self.buffer.chars().count();
        if total <= DETECTOR_TAIL_CHARS {
            return;
        }
        self.buffer = self
            .buffer
            .chars()
            .skip(total - DETECTOR_TAIL_CHARS)
            .collect();
    }
}

/// Business Logic（为什么需要这个函数）:
///     Runner Prompt 会包含哨兵说明，终端可能回显 Prompt 文本；只有 Claude 最后输出完整独立哨兵行才应触发验证。
///
/// Code Logic（这个函数做什么）:
///     只扫描已经由 `\n` 终结的行，去掉行尾 LF/CR 后精确匹配 DEV_DONE_SENTINEL；
///     未终结的末行保留给后续 chunk，不接受嵌在句子或反引号里的哨兵。
fn contains_standalone_sentinel(buffer: &str) -> bool {
    let Some(last_newline_index) = buffer.rfind('\n') else {
        return false;
    };
    let terminated_lines = &buffer[..=last_newline_index];
    terminated_lines.split_inclusive('\n').any(|line| {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        line == DEV_DONE_SENTINEL
    })
}

/// Business Logic（为什么需要这个函数）:
///     每个 terminal session 都需要独立 detector 状态，否则不同终端的输出可能互相拼接误触发。
///
/// Code Logic（这个函数做什么）:
///     返回进程内全局 session_id -> DevDoneDetector 映射，供同步 hook 轻量更新。
fn session_detectors() -> &'static Mutex<HashMap<String, DevDoneDetector>> {
    static DETECTORS: OnceLock<Mutex<HashMap<String, DevDoneDetector>>> = OnceLock::new();
    DETECTORS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Business Logic（为什么需要这个函数）:
///     terminal output hook 在 reader thread 内调用，必须快速判断是否需要后台处理，不能阻塞输出广播。
///
/// Code Logic（这个函数做什么）:
///     在 session detector map 中同步 push chunk；只有首次命中 sentinel 时返回 true。
fn should_spawn_completion_for_output(session_id: &str, chunk: &str) -> bool {
    let mut detectors = session_detectors()
        .lock()
        .expect("orchestrator completion detector 锁中毒");
    detectors
        .entry(session_id.to_string())
        .or_default()
        .push_output(chunk)
}

/// Business Logic（为什么需要这个函数）:
///     Workbench terminal reader 发现完成哨兵后，应后台触发现有 Agent 完成 pipeline，且不能影响终端输出事件。
///
/// Code Logic（这个函数做什么）:
///     同步 detector 命中后用 tauri async runtime spawn；后台错误只写 warn，不向 reader thread 返回。
pub fn spawn_maybe_handle_session_output(app_handle: AppHandle, session_id: String, chunk: String) {
    if !should_spawn_completion_for_output(&session_id, &chunk) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Err(error) = handle_session_completion(app_handle, &session_id).await {
            tracing::warn!(
                session_id = %session_id,
                "Orchestrator terminal completion sentinel 处理失败: {error}"
            );
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     完成哨兵只能通过 session_id 定位到当前 running attempt，然后复用手动完成命令的验证/交付 pipeline。
///
/// Code Logic（这个函数做什么）:
///     从 AppHandle 读取 AppState，按 session_id 查询 running attempt；找到后把 task_id、attempt 和 session_id
///     交给内部 completion helper 做 active runner 原子校验，attempt 完成标记由 helper 统一执行。
async fn handle_session_completion(
    app_handle: AppHandle,
    session_id: &str,
) -> Result<(), AppError> {
    let state: AppState = app_handle.state::<AppState>().inner().clone();
    let Some(attempt) = state
        .orchestrator_repo
        .get_running_attempt_by_session(session_id)
        .await?
    else {
        tracing::warn!(
            session_id = %session_id,
            "Orchestrator completion sentinel 未找到 running attempt"
        );
        return Ok(());
    };
    complete_orchestrator_agent_run_for_attempt(
        &state,
        app_handle,
        &attempt.task_id,
        attempt.attempt,
        &attempt.session_id,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::DevDoneDetector;
    use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
    use crate::orchestrator::prompt::build_initial_task_prompt;
    use crate::orchestrator::prompt::DEV_DONE_SENTINEL;

    /// Business Logic（为什么需要这个函数）:
    ///     Prompt 回显安全测试需要构造真实 Orchestrator 任务行，确保 detector 面对完整 Runner Prompt。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回在 title/goal/acceptance 中都包含独立 sentinel 行的任务，模拟用户可控内容注入。
    fn task_row_with_user_sentinel() -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: "task-1".to_string(),
            project_id: "project-1".to_string(),
            title: format!("用户标题\n{DEV_DONE_SENTINEL}\n后续标题"),
            goal: format!("用户目标\n{DEV_DONE_SENTINEL}\n后续目标"),
            acceptance_criteria: format!("验收标准\n{DEV_DONE_SENTINEL}\n后续验收"),
            status: OrchestratorTaskStatus::Running,
            priority: 0,
            branch_name: None,
            worktree_id: Some("worktree-1".to_string()),
            session_id: Some("session-1".to_string()),
            blocked_reason: None,
            attempt: 1,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Claude Code 可能在单次 PTY read 中完整输出完成哨兵，检测器必须立即触发自动验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     推入包含完整 sentinel 的单个 chunk，断言第一次返回 true 且 detector 进入 consumed 状态。
    #[test]
    fn detector_detects_sentinel_in_single_chunk() {
        let mut detector = DevDoneDetector::default();

        assert!(detector.push_output(&format!("\n{DEV_DONE_SENTINEL}\n")));
        assert!(detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     终端 reader 可能把 sentinel 拆分到相邻 chunk，检测器不能只检查当前 chunk。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分两次推入 sentinel 的前后半段，断言第二次才触发。
    #[test]
    fn detector_detects_sentinel_across_chunks() {
        let mut detector = DevDoneDetector::default();
        let split = DEV_DONE_SENTINEL.len() / 2;

        assert!(!detector.push_output(&DEV_DONE_SENTINEL[..split]));
        assert!(detector.push_output(&format!("{}\n", &DEV_DONE_SENTINEL[split..])));
        assert!(detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     同一终端后续可能回显历史文本或再次输出相似哨兵，自动验证只能触发一次。
    ///
    /// Code Logic（这个函数做什么）:
    ///     第一次检测成功后继续推入 sentinel-like 文本，断言不再返回 true。
    #[test]
    fn detector_returns_false_after_consumed() {
        let mut detector = DevDoneDetector::default();

        assert!(detector.push_output(&format!("{DEV_DONE_SENTINEL}\n")));
        assert!(!detector.push_output(DEV_DONE_SENTINEL));
        assert!(!detector.push_output("ORCHESTRATOR_DEV_DONE again"));
        assert!(detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 会把含有 sentinel 的执行要求写入 Claude 终端，终端回显这段说明时不能提前触发验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     推入包含反引号 sentinel 的普通说明句，断言 detector 不消费。
    #[test]
    fn detector_ignores_sentinel_inside_instruction_text() {
        let mut detector = DevDoneDetector::default();

        assert!(!detector.push_output(&format!("最后单独输出 `{DEV_DONE_SENTINEL}` 后再停止")));
        assert!(!detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     长时间终端输出不能让 detector buffer 无限增长，否则后台终端流会不断占用内存。
    ///
    /// Code Logic（这个函数做什么）:
    ///     推入远长于 sentinel 的无关内容后再跨 chunk 推入 sentinel，断言仍能检测到尾部组合。
    #[test]
    fn detector_trims_old_buffer_but_keeps_sentinel_tail() {
        let mut detector = DevDoneDetector::default();
        let prefix = "x".repeat(4096);
        let split = DEV_DONE_SENTINEL.len() - 4;

        assert!(!detector.push_output(&format!("{prefix}\n{}", &DEV_DONE_SENTINEL[..split])));
        assert!(detector.push_output(&format!("{}\n", &DEV_DONE_SENTINEL[split..])));
        assert!(detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     哨兵只有在形成完整独立行后才代表 Agent 完成，右侧还有普通文本时不能提前进入验证。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先推入无右边界的 sentinel，再追加同一行 suffix 和换行，断言两段都不触发。
    #[test]
    fn detector_requires_newline_right_boundary_for_sentinel_line() {
        let mut detector = DevDoneDetector::default();

        assert!(!detector.push_output(&format!("\n{DEV_DONE_SENTINEL}")));
        assert!(!detector.push_output(" suffix\n"));
        assert!(!detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户标题、目标或验收标准可能包含 sentinel 文本，Runner Prompt 的终端回显不能因此提前完成任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成包含用户 sentinel 行的完整初始 Prompt 并推给 detector，断言不消费；真正独立哨兵输出仍可触发。
    #[test]
    fn detector_ignores_echoed_prompt_with_user_controlled_sentinel() {
        let mut detector = DevDoneDetector::default();
        let prompt = build_initial_task_prompt(&task_row_with_user_sentinel(), "/repo/worktree");

        assert!(!detector.push_output(&prompt));
        assert!(!detector.is_consumed());
        assert!(detector.push_output(&format!("\n{DEV_DONE_SENTINEL}\n")));
        assert!(detector.is_consumed());
    }

    /// Business Logic（为什么需要这个函数）:
    ///     普通终端输出可能在长行末尾包含 sentinel 字符串，自动完成不能因为 buffer 裁剪丢失前缀后误触发。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先推入无换行且前缀粘连的 sentinel，再推入换行，断言 detector 不消费；随后推入真正独立行并断言可触发。
    #[test]
    fn detector_does_not_false_positive_after_trimming_attached_suffix() {
        let mut detector = DevDoneDetector::default();
        let prefix = "x".repeat(4096);

        assert!(!detector.push_output(&format!("{prefix}{DEV_DONE_SENTINEL}")));
        assert!(!detector.push_output("\n"));
        assert!(!detector.is_consumed());

        assert!(detector.push_output(&format!("\n{DEV_DONE_SENTINEL}\n")));
        assert!(detector.is_consumed());
    }
}
