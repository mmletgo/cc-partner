//! Orchestrator claim 三阶段编排：有界候选、事务外 workflow preflight、短 CAS 写事务。
//!
//! Business Logic（为什么需要这个模块）:
//!     全局 scheduler 在大量 Queued 任务下不能在 SQLite 事务内做无界 SELECT 或同步读 WORKFLOW.md，
//!     否则会长时间占用唯一连接并饿死其它读写。把文件 IO 移出事务后，仍需 CAS 与有界扫描保证正确性。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `CLAIM_CANDIDATE_LIMIT`/`CLAIM_PROJECT_LIMIT`、`ClaimScanCursor`、`ClaimCandidate`、
//!     `ClaimPreflight`；提供阶段 B `preflight_claim_candidates`（按项目去重、最多 64 次
//!     spawn_blocking 解析 workflow、按 active_states 过滤）。阶段 A/C 由 repo 实现。

use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskRow;
use crate::orchestrator::workflow::{resolve_project_workflow, ResolvedWorkflow};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 单次 claim 候选 SELECT 的硬上限（含 keyset 分页页大小）。
/// spawn_blocking 中可注入的 workflow resolver 函数类型别名（降低 clippy type_complexity）。
type WorkflowResolverFn = dyn Fn(&Path) -> Result<ResolvedWorkflow, AppError> + Send + Sync;

pub const CLAIM_CANDIDATE_LIMIT: u32 = 256;

/// 阶段 B 解析 WORKFLOW.md 时允许的最大不同 project 数。
pub const CLAIM_PROJECT_LIMIT: usize = 64;

/// scheduler tick 相对期望截止时间的延迟（毫秒）。
pub const METRIC_SCHEDULER_TICK_DELAY_MS: &str = "orchestrator.scheduler_tick_delay_ms";
/// 本 tick 扫描到的候选任务数。
pub const METRIC_CLAIM_CANDIDATES: &str = "orchestrator.claim_candidates";
/// 本 tick 实际解析 workflow 的项目数。
pub const METRIC_CLAIM_PROJECTS: &str = "orchestrator.claim_projects";
/// 本 tick CAS 成功领取数。
pub const METRIC_CLAIM_CLAIMED: &str = "orchestrator.claim_claimed";
/// 候选窗口是否触达 256 上限（1=exhausted）。
pub const METRIC_CLAIM_WINDOW_EXHAUSTED: &str = "orchestrator.claim_window_exhausted";
/// 本 tick CAS 未命中次数（并发竞争/状态已变）。
pub const METRIC_CLAIM_CAS_MISS: &str = "orchestrator.claim_cas_miss";

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

/// 阶段 B workflow preflight 结果。
///
/// Business Logic（为什么需要这个结构体）:
///     scheduler 需要知道本窗哪些任务可 claim、下一页从哪开始、窗口是否触达硬上限，
///     才能在不持有 DB 事务的情况下安全推进扫描与领取；project cap 命中时还需旋转
///     cursor 避免第 65+ 项目跨 tick 永久饥饿。
///
/// Code Logic（这个结构体做什么）:
///     `eligible` 为 active_states 过滤后的候选（保持原优先级顺序）；
///     `next_cursor` 为下一 tick 扫描起点；`exhausted` 表示输入达到 256 上限；
///     `advance_cursor` 为 true 时 scheduler 写回 next_cursor（满窗或 project-cap 旋转）。
#[derive(Debug, Clone)]
pub struct ClaimPreflight {
    pub eligible: Vec<ClaimCandidate>,
    pub next_cursor: Option<ClaimScanCursor>,
    pub exhausted: bool,
    /// 是否推进进程内扫描 cursor（满窗 exhausted 或 project cap 旋转）。
    pub advance_cursor: bool,
}

/// 阶段 C CAS 结果（领取行 + 未命中计数）。
///
/// Business Logic（为什么需要这个结构体）:
///     调度指标需要区分“容量不足未尝试”与“CAS 竞争未命中”，便于观察并发 claim 健康度。
///
/// Code Logic（这个结构体做什么）:
///     `claimed` 为 rows_affected==1 后重读的任务行；`cas_miss` 为 UPDATE 未命中次数。
#[derive(Debug, Clone)]
pub struct ClaimCasOutcome {
    pub claimed: Vec<OrchestratorTaskRow>,
    pub cas_miss: u64,
}

/// Business Logic（为什么需要这个函数）:
///     同一 experiment 不能在一轮中吃满全局槽位；普通 task 也不能被实验组饿死。
///
/// Code Logic（这个函数做什么）:
///     先稳定输出无 experiment_id 的普通任务（保持原序），再按 experiment 组 round-robin
///     每轮每组最多取一个 candidate，直到候选耗尽。
pub fn fair_order_claim_candidates(eligible: Vec<ClaimCandidate>) -> Vec<ClaimCandidate> {
    let mut ordinary: Vec<ClaimCandidate> = Vec::new();
    let mut by_experiment: std::collections::BTreeMap<String, Vec<ClaimCandidate>> =
        std::collections::BTreeMap::new();
    let mut experiment_order: Vec<String> = Vec::new();

    for candidate in eligible {
        match candidate.task.experiment_id.clone() {
            Some(exp_id) if !exp_id.is_empty() => {
                if !by_experiment.contains_key(&exp_id) {
                    experiment_order.push(exp_id.clone());
                }
                by_experiment.entry(exp_id).or_default().push(candidate);
            }
            _ => ordinary.push(candidate),
        }
    }

    let mut ordered = ordinary;
    // round-robin experiment groups
    loop {
        let mut progressed = false;
        for exp_id in &experiment_order {
            if let Some(queue) = by_experiment.get_mut(exp_id) {
                if !queue.is_empty() {
                    ordered.push(queue.remove(0));
                    progressed = true;
                }
            }
        }
        if !progressed {
            break;
        }
    }
    ordered
}

/// Business Logic（为什么需要这个函数）:
///     阶段 B 必须在 DB 事务外解析 WORKFLOW.md，并按项目 active_states 过滤可领取候选，
///     避免慢盘/YAML 解析占住唯一 SQLite 连接。
///
/// Code Logic（这个函数做什么）:
///     按候选顺序对 project_id 去重，最多解析 64 个项目；每个项目 `spawn_blocking` 一次
///     `resolve_project_workflow`；解析失败跳过该项目（不改任务状态）；按 active_states 过滤。
///     满窗时 `next_cursor` 取窗末；若 project 数超过 64，旋转 cursor 到首个未解析项目之前，
///     避免第 65+ 项目被推进越过而跨 tick 饥饿。
pub async fn preflight_claim_candidates(
    candidates: Vec<ClaimCandidate>,
) -> Result<ClaimPreflight, AppError> {
    preflight_claim_candidates_with_resolver(
        candidates,
        Arc::new(|path: &Path| resolve_project_workflow(path)),
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     集成/单元测试需要注入可阻塞的 resolver，以证明 preflight 不占用 DB 连接；
///     生产路径继续走真实 `resolve_project_workflow`。
///
/// Code Logic（这个函数做什么）:
///     与 `preflight_claim_candidates` 相同的分组/上限/过滤/project-cap 旋转逻辑，
///     但 workflow 解析委托给传入的 `resolver`（在 spawn_blocking 中调用）。
pub async fn preflight_claim_candidates_with_resolver(
    candidates: Vec<ClaimCandidate>,
    resolver: Arc<WorkflowResolverFn>,
) -> Result<ClaimPreflight, AppError> {
    let exhausted = (candidates.len() as u32) >= CLAIM_CANDIDATE_LIMIT;

    if candidates.is_empty() {
        return Ok(ClaimPreflight {
            eligible: Vec::new(),
            next_cursor: None,
            exhausted: false,
            advance_cursor: false,
        });
    }

    // 按候选出现顺序对 project_id 去重，保留首个 path。
    let mut project_order: Vec<String> = Vec::new();
    let mut project_paths: HashMap<String, PathBuf> = HashMap::new();
    for candidate in &candidates {
        if project_paths
            .insert(
                candidate.task.project_id.clone(),
                candidate.project_path.clone(),
            )
            .is_none()
        {
            project_order.push(candidate.task.project_id.clone());
        }
    }

    let project_cap_hit = project_order.len() > CLAIM_PROJECT_LIMIT;
    let projects_to_resolve: Vec<String> = project_order
        .iter()
        .take(CLAIM_PROJECT_LIMIT)
        .cloned()
        .collect();
    let resolved_set: std::collections::HashSet<String> =
        projects_to_resolve.iter().cloned().collect();

    // project_id -> Some(workflow) 成功；None 表示无效/跳过。
    let mut workflows: HashMap<String, Option<ResolvedWorkflow>> =
        HashMap::with_capacity(projects_to_resolve.len());

    for project_id in projects_to_resolve {
        let path = project_paths
            .get(&project_id)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(""));
        let resolver = Arc::clone(&resolver);
        let join = tokio::task::spawn_blocking(move || resolver(path.as_path()))
            .await
            .map_err(|err| AppError::generic(format!("workflow resolve join 失败: {err}")))?;
        match join {
            Ok(workflow) => {
                workflows.insert(project_id, Some(workflow));
            }
            Err(err) => {
                // 只记录脱敏 project_id，不写路径/正文。
                tracing::warn!(
                    project_id = %project_id,
                    "跳过无效 WORKFLOW.md 项目的 Orchestrator dispatch: {err}"
                );
                workflows.insert(project_id, None);
            }
        }
    }

    let mut eligible = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let Some(Some(workflow)) = workflows.get(&candidate.task.project_id) else {
            continue;
        };
        if workflow
            .active_states
            .contains(&candidate.task.workflow_state)
        {
            eligible.push(candidate.clone());
        }
    }

    // project cap 旋转：cursor 取「首个未解析项目」前一条，使下一 tick 从尾部项目继续，
    // 而不是推进到窗末把 65+ 项目整体跳过。无 cap 时仍用窗末（满窗）或 None。
    let (next_cursor, advance_cursor) = if project_cap_hit {
        let mut prev: Option<ClaimScanCursor> = None;
        for c in &candidates {
            if !resolved_set.contains(&c.task.project_id) {
                break;
            }
            prev = Some(c.to_scan_cursor());
        }
        (prev, true)
    } else if exhausted {
        (candidates.last().map(ClaimCandidate::to_scan_cursor), true)
    } else {
        (candidates.last().map(ClaimCandidate::to_scan_cursor), false)
    };

    Ok(ClaimPreflight {
        eligible,
        next_cursor,
        exhausted,
        advance_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{
        OrchestratorRunState, OrchestratorTaskStatus, OrchestratorWorkflowState,
    };
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::oneshot;

    /// Business Logic（为什么需要这个测试）:
    ///     游标编码必须稳定复用任务排序键，否则 keyset 翻页会跳行或重复。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造最小任务行，断言 from_task/to_scan_cursor 三字段与任务一致。
    #[test]
    fn claim_scan_cursor_from_task_copies_sort_keys() {
        let task = sample_task("task-z", "local-a", 7, "2026-07-05T00:00:01Z");
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

    /// Business Logic（为什么需要这个测试）:
    ///     公平序必须先输出普通任务，再 round-robin 各实验组，避免单组吃满。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 2 普通 + 2 组各 2 candidate，断言顺序。
    #[test]
    fn fair_order_puts_ordinary_first_then_round_robins_experiments() {
        let mut items = Vec::new();
        for id in ["o1", "o2"] {
            let mut t = sample_task(id, "p", 1, "t");
            t.experiment_id = None;
            items.push(ClaimCandidate {
                task: t,
                project_path: PathBuf::from("/tmp"),
            });
        }
        for (exp, ids) in [("e1", ["e1a", "e1b"]), ("e2", ["e2a", "e2b"])] {
            for id in ids {
                let mut t = sample_task(id, "p", 1, "t");
                t.experiment_id = Some(exp.to_string());
                items.push(ClaimCandidate {
                    task: t,
                    project_path: PathBuf::from("/tmp"),
                });
            }
        }
        let ordered = fair_order_claim_candidates(items);
        let ids: Vec<_> = ordered.iter().map(|c| c.task.id.as_str()).collect();
        assert_eq!(ids, vec!["o1", "o2", "e1a", "e2a", "e1b", "e2b"]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preflight 必须按 active_states 过滤，且无效 workflow 只跳过不改任务语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     注入 resolver：项目 a 仅 Todo 活跃，项目 b 解析失败；断言只留下 a 的 Todo 任务。
    #[tokio::test]
    async fn preflight_filters_active_states_and_skips_invalid_projects() {
        let c_todo = ClaimCandidate {
            task: sample_task("t1", "proj-a", 10, "2026-07-05T00:00:01Z"),
            project_path: PathBuf::from("/tmp/a"),
        };
        let mut rework = sample_task("t2", "proj-a", 9, "2026-07-05T00:00:02Z");
        rework.workflow_state = OrchestratorWorkflowState::Rework;
        let c_rework = ClaimCandidate {
            task: rework,
            project_path: PathBuf::from("/tmp/a"),
        };
        let c_bad = ClaimCandidate {
            task: sample_task("t3", "proj-b", 8, "2026-07-05T00:00:03Z"),
            project_path: PathBuf::from("/tmp/b"),
        };

        let resolver: Arc<WorkflowResolverFn> = Arc::new(|path: &Path| {
            if path.ends_with("b") {
                return Err(AppError::generic("invalid workflow fixture"));
            }
            let mut wf = ResolvedWorkflow::built_in_default();
            wf.active_states = vec![OrchestratorWorkflowState::Todo];
            Ok(wf)
        });

        let preflight =
            preflight_claim_candidates_with_resolver(vec![c_todo, c_rework, c_bad], resolver)
                .await
                .expect("preflight");

        assert_eq!(
            preflight
                .eligible
                .iter()
                .map(|c| c.task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["t1"]
        );
        assert_eq!(
            preflight.next_cursor.as_ref().map(|c| c.id.as_str()),
            Some("t3")
        );
        assert!(!preflight.exhausted);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     慢 workflow IO 绝不能阻塞 DB 读；否则单连接池上其它命令会卡死。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 oneshot 阻塞 resolver，并发执行“模拟 get_task 完成”的 future，
    ///     断言 100ms 内完成（证明 preflight 不占用调用方事件循环上的 DB 事务）。
    #[tokio::test]
    async fn orchestrator_claim_slow_preflight_does_not_block_concurrent_work() {
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let release_rx = Arc::new(Mutex::new(Some(release_rx)));

        let resolver: Arc<WorkflowResolverFn> = Arc::new(move |_path: &Path| {
            if let Some(rx) = release_rx.lock().expect("lock").take() {
                let _ = rx.blocking_recv();
            }
            Ok(ResolvedWorkflow::built_in_default())
        });

        let candidates = vec![ClaimCandidate {
            task: sample_task("slow-1", "proj-slow", 1, "2026-07-05T00:00:01Z"),
            project_path: PathBuf::from("/tmp/slow"),
        }];

        let preflight_handle = tokio::spawn(async move {
            preflight_claim_candidates_with_resolver(candidates, resolver).await
        });

        // 给 spawn_blocking 一点时间进入 resolver 阻塞点。
        tokio::time::sleep(Duration::from_millis(20)).await;

        let concurrent = tokio::time::timeout(Duration::from_millis(100), async {
            // 模拟并发 DB 读：不依赖真实 pool，只验证 preflight 不占用当前 runtime。
            tokio::task::yield_now().await;
            "ok"
        })
        .await
        .expect("concurrent work must complete within 100ms during slow preflight");
        assert_eq!(concurrent, "ok");

        let _ = release_tx.send(());
        let preflight = preflight_handle.await.expect("join").expect("preflight ok");
        assert_eq!(preflight.eligible.len(), 1);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     单元测试需要最小任务行构造，避免每个 case 手写全部字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Queued/Todo/Idle 任务行并覆盖 id/project/priority/created_at。
    fn sample_task(
        id: &str,
        project_id: &str,
        priority: i64,
        created_at: &str,
    ) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: project_id.to_string(),
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
            runner_max_turns: None,
            runner_stall_timeout_ms: None,
            claude_session_id: None,
            agent_session_id: None,
            transcript_path: None,
            runtime_started_at: None,
            experiment_id: None,
            delivery_suppressed: false,
            last_activity_at: None,
            last_runtime_event: None,
            last_runtime_message: None,
            priority,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 0,
            state_version: 0,
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
            started_at: None,
            finished_at: None,
        }
    }
}
