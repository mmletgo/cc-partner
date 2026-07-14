//! Orchestrator global scheduler.
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器需要按设备级全局配置定期领取本机 Queued 任务，并把任务交给可见 Workbench Runner。
//!
//! Code Logic（这个模块做什么）:
//!     提供后台 scheduler runtime、单次 dispatch 入口和并发容量相关纯 helper。

use crate::backend::runtime_metrics::RuntimeMetrics;
use crate::config::OrchestratorAutomationConfig;
use crate::error::AppError;
use crate::orchestrator::claim::{
    preflight_claim_candidates, ClaimScanCursor, CLAIM_CANDIDATE_LIMIT, METRIC_CLAIM_CANDIDATES,
    METRIC_CLAIM_CAS_MISS, METRIC_CLAIM_CLAIMED, METRIC_CLAIM_PROJECTS,
    METRIC_CLAIM_WINDOW_EXHAUSTED, METRIC_SCHEDULER_TICK_DELAY_MS,
};
use crate::orchestrator::models::{OrchestratorTaskRow, OrchestratorTaskStatus};
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::runner::prepare_visible_runner;
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const SCHEDULER_TICK_SECS: u64 = 10;
/// claim 后进程崩溃留下的 Preparing 超过该 lease 才回收。
/// prepare 会在长步骤前后续租 updated_at；lease 需明显大于单次 git worktree/session 创建，
/// 避免仍在合法 prepare 的任务被并发 dispatch 误杀为 Blocked 并留下孤儿现场。
const PREPARING_STALE_LEASE_SECS: u64 = 600;

/// Orchestrator scheduler 运行时句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     应用启动后需要保存后台调度循环的取消令牌，退出时能优雅停止自动任务领取。
///
/// Code Logic（这个结构体做什么）:
///     包装 tokio CancellationToken，并提供轻量 clone 与读取 token 的方法。
#[derive(Clone)]
pub struct OrchestratorRuntime {
    cancel: CancellationToken,
}

/// Orchestrator scheduler 可观测状态快照。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 状态条需要解释最近一次调度 tick 何时发生、是否报错以及调度了多少任务。
///
/// Code Logic（这个结构体做什么）:
///     保存内存 telemetry 的只读克隆结果，供命令层组装 runtime snapshot DTO。
#[derive(Debug, Clone, Default)]
pub struct OrchestratorSchedulerTelemetrySnapshot {
    pub latest_tick_at: Option<String>,
    pub last_dispatch_at: Option<String>,
    pub last_dispatched_count: usize,
    pub latest_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct OrchestratorSchedulerTelemetryState {
    latest_tick_at: Option<String>,
    last_dispatch_at: Option<String>,
    last_dispatched_count: usize,
    latest_error: Option<String>,
    /// 上次 tick 实际开始时刻，用于计算下一 tick 相对期望截止的延迟。
    last_tick_started_at: Option<Instant>,
}

/// Orchestrator scheduler 运行时可观测状态。
///
/// Business Logic（为什么需要这个结构体）:
///     scheduler 在后台运行，不一定有任务事件可写；状态条仍需要知道最近一次 dispatch_once 的时间和结果。
///     进程内 claim 扫描游标也由 scheduler 拥有，避免无效 workflow 窗口饿死后合法任务。
///
/// Code Logic（这个结构体做什么）:
///     用 Arc<RwLock> 保存最近一次调度结果；用 Arc<Mutex> 保存 claim keyset 游标；Clone 只复制 Arc。
#[derive(Debug, Clone, Default)]
pub struct OrchestratorSchedulerTelemetry {
    inner: Arc<RwLock<OrchestratorSchedulerTelemetryState>>,
    claim_scan_cursor: Arc<Mutex<Option<ClaimScanCursor>>>,
}

impl OrchestratorSchedulerTelemetry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化和单元测试都需要创建空的 scheduler telemetry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回包含默认状态的 OrchestratorSchedulerTelemetry。
    pub fn new() -> Self {
        Self::default()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次 dispatch_once 完成后都要记录 tick 结果，方便 runtime snapshot 解释最近调度时间。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入的 UTC 时间、dispatch 数量和可选错误覆盖内存状态；空白错误会归一为 None。
    pub fn record_dispatch_result(
        &self,
        tick_at: String,
        dispatched_count: usize,
        error: Option<String>,
    ) {
        let latest_error = error
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let mut state = self.inner.write().expect("orchestrator telemetry 写锁中毒");
        state.latest_tick_at = Some(tick_at.clone());
        state.last_dispatch_at = Some(tick_at);
        state.last_dispatched_count = dispatched_count;
        state.latest_error = latest_error;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runtime snapshot 命令需要读取可观测状态，但不能持有锁跨 await。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆当前 telemetry 字段，返回独立快照。
    pub fn snapshot(&self) -> OrchestratorSchedulerTelemetrySnapshot {
        let state = self.inner.read().expect("orchestrator telemetry 读锁中毒");
        OrchestratorSchedulerTelemetrySnapshot {
            latest_tick_at: state.latest_tick_at.clone(),
            last_dispatch_at: state.last_dispatch_at.clone(),
            last_dispatched_count: state.last_dispatched_count,
            latest_error: state.latest_error.clone(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次 tick 开始时需要知道相对“上次开始 + 10s”的延迟，用于本地性能指标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读取上次 tick Instant；若存在且 now 晚于期望截止，返回延迟 Duration，否则 0；
    ///     随后把 last_tick_started_at 更新为 now。
    pub fn mark_tick_start_and_delay(&self, now: Instant) -> Duration {
        let mut state = self.inner.write().expect("orchestrator telemetry 写锁中毒");
        let delay = state
            .last_tick_started_at
            .map(|prev| {
                let expected = prev + Duration::from_secs(SCHEDULER_TICK_SECS);
                now.saturating_duration_since(expected)
            })
            .unwrap_or(Duration::ZERO);
        state.last_tick_started_at = Some(now);
        delay
    }

    /// Business Logic（为什么需要这个函数）:
    ///     三阶段 claim 扫描游标由 scheduler 进程内持有，tick 之间需要读写同一份边界。
    ///
    /// Code Logic（这个函数做什么）:
    ///     加锁取出当前 `Option<ClaimScanCursor>` 克隆。
    pub fn claim_scan_cursor(&self) -> Option<ClaimScanCursor> {
        self.claim_scan_cursor
            .lock()
            .expect("claim scan cursor 锁中毒")
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     有界扫描成功后推进或回绕游标；DB 错误路径不得调用本方法，避免跳过未扫描区。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用传入值覆盖进程内游标（None 表示从头部重新扫描）。
    pub fn set_claim_scan_cursor(&self, cursor: Option<ClaimScanCursor>) {
        *self
            .claim_scan_cursor
            .lock()
            .expect("claim scan cursor 锁中毒") = cursor;
    }
}

impl OrchestratorRuntime {
    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 启动时需要创建一份新的取消令牌供后台循环和 AppState 共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造包含 CancellationToken::new() 的 OrchestratorRuntime。
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     lib.rs 需要拿到取消令牌并存入 AppState，后台 task 也需要持有同一令牌。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone 内部 CancellationToken；clone 后 cancel 会广播到所有持有者。
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Business Logic（为什么需要这个函数）:
///     应用启动后应自动轮询 Orchestrator 队列，无需用户保持页面打开。
///
/// Code Logic（这个函数做什么）:
///     使用 tauri async runtime 启动后台循环，每 10 秒调用 dispatch_once，收到 cancel 后退出。
pub fn start_orchestrator_scheduler(state: AppState) -> CancellationToken {
    let runtime = OrchestratorRuntime::new();
    let cancel = runtime.cancel_token();
    let task_cancel = runtime.cancel_token();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    tracing::info!("Orchestrator scheduler 已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(SCHEDULER_TICK_SECS)) => {
                    if let Err(err) = dispatch_once(&state).await {
                        tracing::error!("Orchestrator scheduler dispatch 失败: {err}");
                    }
                }
            }
        }
    });
    cancel
}

/// Business Logic（为什么需要这个函数）:
///     scheduler tick 和手动命令都需要执行同一套领取逻辑，避免 UI 手动触发与后台行为不一致。
///
/// Code Logic（这个函数做什么）:
///     每次从 AppState 读取全局 Orchestrator 配置，在 repo 事务内按全局本机容量批量 claim 可执行泳道任务并交给 runner；
///     runner 失败时仅在任务仍为 Preparing 或 bootstrap Running 时把任务置为 Blocked 并追加事件，成功时计入 dispatched。
pub async fn dispatch_once(state: &AppState) -> Result<usize, AppError> {
    let tick_at = Utc::now().to_rfc3339();
    let result = dispatch_once_inner(state).await;
    match &result {
        Ok(dispatched) => {
            state
                .orchestrator_scheduler_telemetry
                .record_dispatch_result(tick_at, *dispatched, None);
        }
        Err(err) => {
            state
                .orchestrator_scheduler_telemetry
                .record_dispatch_result(tick_at, 0, Some(err.to_string()));
        }
    }
    result
}

/// Business Logic（为什么需要这个函数）:
///     dispatch_once 需要在外层统一记录可观测 tick，同时保留原调度流程的错误传播。
///
/// Code Logic（这个函数做什么）:
///     记录 tick 延迟指标；回收过期 Preparing；三阶段 claim（cursor + metrics）；
///     再逐任务启动 runner；runner 失败写 Blocked 补偿，返回成功派发数量。
async fn dispatch_once_inner(state: &AppState) -> Result<usize, AppError> {
    let tick_delay = state
        .orchestrator_scheduler_telemetry
        .mark_tick_start_and_delay(Instant::now());
    state
        .runtime_metrics
        .record_duration(METRIC_SCHEDULER_TICK_DELAY_MS, tick_delay);
    if tick_delay > Duration::from_secs(SCHEDULER_TICK_SECS.saturating_mul(2)) {
        state
            .runtime_metrics
            .warn_metric(METRIC_SCHEDULER_TICK_DELAY_MS);
    }

    let config = state
        .config
        .read()
        .expect("config 读锁中毒")
        .orchestrator
        .clone();
    let mut cursor = state.orchestrator_scheduler_telemetry.claim_scan_cursor();
    let tasks = claim_tasks_for_dispatch(
        state.orchestrator_repo.as_ref(),
        &config,
        &mut cursor,
        Some(state.runtime_metrics.as_ref()),
    )
    .await?;
    // 仅在 claim 路径成功返回后写回游标（DB 错误不会走到这里）。
    state
        .orchestrator_scheduler_telemetry
        .set_claim_scan_cursor(cursor);

    let mut dispatched = 0usize;
    for task in tasks {
        match prepare_visible_runner(state, &task).await {
            Ok(_) => {
                dispatched += 1;
            }
            Err(err) => {
                let reason = err.to_string();
                // 单任务补偿失败不得中断后续任务：记录错误后继续，避免一个失败让其余 Preparing 永久占槽。
                if let Err(compensate_err) =
                    record_runner_failure(&state.orchestrator_repo, &task.id, &reason).await
                {
                    tracing::error!(
                        task_id = %task.id,
                        "Runner 失败补偿写入 Blocked 失败: {compensate_err}（原始失败: {reason}）"
                    );
                }
            }
        }
    }
    Ok(dispatched)
}

/// Business Logic（为什么需要这个函数）:
///     后台 scheduler 和手动 dispatch 应共享全局开关与容量解释，测试也需要在不构造 GUI 句柄的情况下验证领取语义。
///     无效 workflow 占满 256 窗时，必须用进程内 cursor 继续扫描，避免合法任务永久饥饿。
///
/// Code Logic（这个函数做什么）:
///     先回收过期 Preparing；enabled=false 返回空。enabled=true 时：有界 list 候选 → 事务外
///     preflight → 短 CAS 写事务；仅在 list/preflight 成功后推进/回绕 cursor（DB 错误不推进）。
///     写入候选/项目/领取/exhausted/CAS miss 固定名指标。
async fn claim_tasks_for_dispatch(
    repo: &OrchestratorRepo,
    config: &OrchestratorAutomationConfig,
    cursor: &mut Option<ClaimScanCursor>,
    metrics: Option<&RuntimeMetrics>,
) -> Result<Vec<OrchestratorTaskRow>, AppError> {
    let recovered = repo
        .recover_stale_local_preparing_tasks(Duration::from_secs(PREPARING_STALE_LEASE_SECS))
        .await?;
    if recovered > 0 {
        tracing::warn!("已回收 {recovered} 个过期 Preparing 任务，释放调度容量");
    }
    if !config.enabled {
        return Ok(Vec::new());
    }

    let candidates = repo
        .list_local_queued_claim_candidates(cursor.as_ref(), CLAIM_CANDIDATE_LIMIT)
        .await?;

    if candidates.is_empty() {
        // 扫到尾部：回绕到头部，下一 tick 从 None 开始。
        *cursor = None;
        if let Some(metrics) = metrics {
            metrics.record_count(METRIC_CLAIM_CANDIDATES, 0);
            metrics.record_count(METRIC_CLAIM_PROJECTS, 0);
            metrics.record_count(METRIC_CLAIM_CLAIMED, 0);
            metrics.record_count(METRIC_CLAIM_WINDOW_EXHAUSTED, 0);
            metrics.record_count(METRIC_CLAIM_CAS_MISS, 0);
        }
        return Ok(Vec::new());
    }

    let candidate_count = candidates.len() as u64;
    let project_count = {
        let mut seen = HashSet::new();
        for candidate in &candidates {
            seen.insert(candidate.task.project_id.clone());
        }
        seen.len().min(crate::orchestrator::claim::CLAIM_PROJECT_LIMIT) as u64
    };

    let preflight = preflight_claim_candidates(candidates).await?;

    // 成功有界扫描后才推进：满窗继续 next_cursor，未满窗视为触尾回绕。
    if preflight.exhausted {
        *cursor = preflight.next_cursor.clone();
    } else {
        *cursor = None;
    }

    let outcome = repo
        .claim_preflighted_candidates_with_global_capacity(
            config.max_concurrent_tasks,
            &preflight.eligible,
        )
        .await?;

    if let Some(metrics) = metrics {
        metrics.record_count(METRIC_CLAIM_CANDIDATES, candidate_count);
        metrics.record_count(METRIC_CLAIM_PROJECTS, project_count);
        metrics.record_count(METRIC_CLAIM_CLAIMED, outcome.claimed.len() as u64);
        metrics.record_count(
            METRIC_CLAIM_WINDOW_EXHAUSTED,
            if preflight.exhausted { 1 } else { 0 },
        );
        metrics.record_count(METRIC_CLAIM_CAS_MISS, outcome.cas_miss);
        if preflight.exhausted {
            metrics.warn_metric(METRIC_CLAIM_WINDOW_EXHAUSTED);
        }
    }

    Ok(outcome.claimed)
}

/// Business Logic（为什么需要这个函数）:
///     Runner 准备失败可能发生在挂账 Running 前后，也可能与用户 Abort 并发发生；失败补偿必须尊重用户终止。
///
/// Code Logic（这个函数做什么）:
///     使用 expected-status 原子转移仅允许 Preparing/Running→Blocked；命中后追加 blocked event，
///     未命中时读取并返回当前任务，不写 blocked event。
async fn record_runner_failure(
    repo: &OrchestratorRepo,
    task_id: &str,
    reason: &str,
) -> Result<OrchestratorTaskRow, AppError> {
    let blocked_task = if let Some(task) = repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Preparing,
            OrchestratorTaskStatus::Blocked,
            Some(reason),
        )
        .await?
    {
        task
    } else if let Some(task) = repo
        .try_transition_task_status(
            task_id,
            OrchestratorTaskStatus::Running,
            OrchestratorTaskStatus::Blocked,
            Some(reason),
        )
        .await?
    {
        task
    } else {
        return repo.get_task(task_id).await;
    };

    repo.add_event(
        task_id,
        "blocked",
        &format!("Runner 准备失败: {reason}"),
        None,
    )
    .await?;
    Ok(blocked_task)
}

/// Business Logic（为什么需要这个函数）:
///     项目配置的并发上限可能被设为 0 或负数，调度器需要安全地把它解释为不调度。
///
/// Code Logic（这个函数做什么）:
///     返回 max_concurrent_tasks - active_count 的非负剩余容量；非正上限直接返回 0。
#[cfg(test)]
pub(crate) fn dispatch_capacity(max_concurrent_tasks: i64, active_count: i64) -> i64 {
    if max_concurrent_tasks <= 0 {
        return 0;
    }
    (max_concurrent_tasks - active_count).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorAutomationConfig;
    use crate::orchestrator::models::{
        OrchestratorAttemptPhase, OrchestratorRunState, OrchestratorTaskRow,
        OrchestratorTaskStatus, OrchestratorWorkflowState,
    };
    use crate::orchestrator::repo::OrchestratorRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::Row;
    use sqlx::SqlitePool;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 测试需要隔离 SQLite，避免 runner 失败路径污染用户真实任务表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接内存数据库，初始化 Orchestrator schema 并返回 repo。
    async fn setup_repo() -> (SqlitePool, OrchestratorRepo) {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        OrchestratorRepo::init_schema(&pool).await.unwrap();
        let repo = OrchestratorRepo::new(pool.clone());
        (pool, repo)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler Phase 3 只领取本机 local Workbench 项目的任务，单测需要最小项目表模拟 local/remote。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 claim 查询依赖的 workbench_projects 表。
    async fn create_workbench_projects_table(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS workbench_projects (\
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, device_id TEXT NOT NULL, \
             device_name TEXT NOT NULL, path TEXT NOT NULL, last_opened_at TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度测试需要显式声明项目是 local 还是 remote，避免误把远端快捷方式当成本机执行目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入一条 Workbench 项目记录，除 kind/id 外字段使用稳定测试值。
    async fn insert_workbench_project_with_path(
        pool: &SqlitePool,
        id: &str,
        kind: &str,
        path: &Path,
    ) {
        sqlx::query(
            "INSERT INTO workbench_projects \
             (id, name, kind, device_id, device_name, path, last_opened_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'device-test', 'Device Test', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(format!("Project {id}"))
        .bind(kind)
        .bind(path.to_string_lossy().to_string())
        .bind("2026-07-05T00:00:00Z")
        .bind("2026-07-05T00:00:00Z")
        .bind("2026-07-05T00:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     大多数 scheduler 测试只关心 local/remote 类型，不需要真实项目目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用稳定的 /tmp/<id> 路径调用 insert_workbench_project_with_path。
    async fn insert_workbench_project(pool: &SqlitePool, id: &str, kind: &str) {
        insert_workbench_project_with_path(pool, id, kind, &PathBuf::from(format!("/tmp/{id}")))
            .await;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     runner 失败路径测试只关心任务状态和 id，但 repo 插入需要完整任务行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造字段完整、时间戳稳定的 OrchestratorTaskRow。
    fn task_row(id: &str, status: OrchestratorTaskStatus) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            id: id.to_string(),
            project_id: "project-1".to_string(),
            title: format!("Task {id}"),
            goal: format!("Goal {id}"),
            acceptance_criteria: format!("Criteria {id}"),
            status,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            ..OrchestratorTaskRow::default_for_status(status)
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 全局容量测试需要在多个项目之间分布任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复用基础任务构造，再覆盖 project_id。
    fn task_row_for_project(
        id: &str,
        project_id: &str,
        status: OrchestratorTaskStatus,
    ) -> OrchestratorTaskRow {
        OrchestratorTaskRow {
            project_id: project_id.to_string(),
            ..task_row(id, status)
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     split state 调度测试需要构造 legacy status 与新 workflow/run state 不完全一致的任务，覆盖迁移后的真实选择条件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接更新测试任务的 workflow_state、run_state 和 blocked_reason，避免通过 legacy status 间接推导。
    async fn set_task_split_state(
        pool: &SqlitePool,
        task_id: &str,
        workflow_state: OrchestratorWorkflowState,
        run_state: OrchestratorRunState,
        blocked_reason: Option<&str>,
    ) {
        sqlx::query(
            "UPDATE orchestrator_tasks \
             SET workflow_state = ?, run_state = ?, blocked_reason = ? WHERE id = ?",
        )
        .bind(workflow_state.as_str())
        .bind(run_state.as_str())
        .bind(blocked_reason)
        .bind(task_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调度测试只关心 enabled 和全局并发上限，其它自动交付开关使用默认值即可。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 OrchestratorAutomationConfig，并覆盖 enabled/max_concurrent_tasks。
    fn global_config(enabled: bool, max_concurrent_tasks: i64) -> OrchestratorAutomationConfig {
        OrchestratorAutomationConfig {
            enabled,
            max_concurrent_tasks,
            ..OrchestratorAutomationConfig::default()
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目并发上限为 0 或负数时必须视为不调度，避免错误配置触发自动执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用容量 helper，断言非正上限和已满项目都返回 0，未满项目返回剩余容量。
    #[test]
    fn dispatch_capacity_clamps_non_positive_limits() {
        assert_eq!(super::dispatch_capacity(0, 0), 0);
        assert_eq!(super::dispatch_capacity(-2, 0), 0);
        assert_eq!(super::dispatch_capacity(2, 2), 0);
        assert_eq!(super::dispatch_capacity(3, 1), 2);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局 Orchestrator 开关关闭时，后台 scheduler 即使存在 queued local 任务也必须 dispatch 0。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入 local queued 任务后用 enabled=false 配置执行领取 helper，断言返回空且任务仍为 Queued。
    #[tokio::test]
    async fn global_disabled_config_claims_no_tasks() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "project-1", "local").await;
        repo.create_task(&task_row("task-queued", OrchestratorTaskStatus::Queued))
            .await
            .unwrap();

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(false, 4),
            &mut cursor,
            None,
        )
        .await
            .unwrap();
        let persisted = repo.get_task("task-queued").await.unwrap();

        assert!(claimed.is_empty());
        assert_eq!(persisted.status, OrchestratorTaskStatus::Queued);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     全局并发上限应在所有本机 local 项目之间共享，而不是每个项目各自拥有一份容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     制造 1 个 active local 任务和 3 个 queued local 任务，max=3 时只能再领取 2 个。
    #[tokio::test]
    async fn global_capacity_is_shared_across_local_projects() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        insert_workbench_project(&pool, "local-b", "local").await;
        repo.create_task(&task_row_for_project(
            "active",
            "local-a",
            OrchestratorTaskStatus::Running,
        ))
        .await
        .unwrap();
        for (id, project_id, priority) in [
            ("queued-high", "local-b", 30_i64),
            ("queued-mid", "local-a", 20_i64),
            ("queued-low", "local-b", 10_i64),
        ] {
            repo.create_task(&task_row_for_project(
                id,
                project_id,
                OrchestratorTaskStatus::Queued,
            ))
            .await
            .unwrap();
            sqlx::query("UPDATE orchestrator_tasks SET priority = ? WHERE id = ?")
                .bind(priority)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            set_task_split_state(
                &pool,
                id,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Idle,
                None,
            )
            .await;
        }

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 3),
            &mut cursor,
            None,
        )
        .await
            .unwrap();

        assert_eq!(
            claimed
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["queued-high", "queued-mid"]
        );
        assert_eq!(
            repo.get_task("queued-low").await.unwrap().status,
            OrchestratorTaskStatus::Queued
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     split state 后，scheduler 只领取 **Queued + Idle** 且 active workflow 上的任务（Todo/Rework）；
    ///     Backlog 不自动启动，仅拖入 Todo 的 Draft 不启动，Blocked 必须经用户 retry 回到 Idle 后才能再 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Backlog/Draft idle、Queued Todo idle、Queued Rework idle 与 Blocked Rework 四个本机任务，
    ///     断言只领取 Queued 的 Todo/Rework Idle 项，Blocked 保持阻塞。
    #[tokio::test]
    async fn scheduler_claims_todo_and_rework_only() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        repo.create_task(&task_row_for_project(
            "backlog-idle",
            "local-a",
            OrchestratorTaskStatus::Draft,
        ))
        .await
        .unwrap();
        for id in ["todo-idle", "rework-idle"] {
            repo.create_task(&task_row_for_project(
                id,
                "local-a",
                OrchestratorTaskStatus::Queued,
            ))
            .await
            .unwrap();
        }
        repo.create_task(&task_row_for_project(
            "rework-blocked",
            "local-a",
            OrchestratorTaskStatus::Blocked,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "todo-idle",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        set_task_split_state(
            &pool,
            "rework-idle",
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        set_task_split_state(
            &pool,
            "rework-blocked",
            OrchestratorWorkflowState::Rework,
            OrchestratorRunState::Blocked,
            Some("验证失败"),
        )
        .await;

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 3),
            &mut cursor,
            None,
        )
        .await
            .unwrap();
        let backlog = repo.get_task("backlog-idle").await.unwrap();
        let rework_blocked = repo.get_task("rework-blocked").await.unwrap();

        let claimed_ids = claimed
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(claimed_ids.len(), 2);
        assert!(claimed_ids.contains(&"todo-idle"));
        assert!(claimed_ids.contains(&"rework-idle"));
        assert!(!claimed_ids.contains(&"rework-blocked"));
        assert!(claimed.iter().all(|task| {
            task.status == OrchestratorTaskStatus::Preparing
                && task.workflow_state == OrchestratorWorkflowState::InProgress
                && task.run_state == OrchestratorRunState::Preparing
                && task.attempt_phase == Some(OrchestratorAttemptPhase::PreparingWorkspace)
                && task.blocked_reason.is_none()
        }));
        assert_eq!(backlog.workflow_state, OrchestratorWorkflowState::Backlog);
        assert_eq!(backlog.run_state, OrchestratorRunState::Idle);
        assert_eq!(rework_blocked.run_state, OrchestratorRunState::Blocked);
        assert_eq!(rework_blocked.blocked_reason.as_deref(), Some("验证失败"));
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户仅把 Draft 拖到 Todo 泳道时不得隐式启动 Runner；只有 queue/start/createAction 产生的 Queued 才可 claim。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Draft 任务后 move_task_workflow_state 到 Todo，再跑 claim helper，断言不被领取且仍为 Draft/Todo/Idle。
    #[tokio::test]
    async fn draft_dragged_to_todo_is_not_claimed_by_scheduler() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        let created = task_row_for_project("draft-drag", "local-a", OrchestratorTaskStatus::Draft);
        repo.create_task(&created).await.unwrap();
        let moved = repo
            .move_task_workflow_state(&created.id, OrchestratorWorkflowState::Todo)
            .await
            .unwrap();
        assert_eq!(moved.status, OrchestratorTaskStatus::Draft);
        assert_eq!(moved.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(moved.run_state, OrchestratorRunState::Idle);

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 3),
            &mut cursor,
            None,
        )
        .await
            .unwrap();
        let persisted = repo.get_task(&created.id).await.unwrap();

        assert!(claimed.is_empty(), "仅拖拽 Draft→Todo 不得 claim");
        assert_eq!(persisted.status, OrchestratorTaskStatus::Draft);
        assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     claim 后进程崩溃留下的 Preparing 必须在下一 tick 回收，否则会永久占满全局容量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     插入过期 Preparing 任务（updated_at 早于 lease）与一个 Queued 任务；claim helper 应先回收 Preparing 再领取 Queued。
    #[tokio::test]
    async fn stale_preparing_is_reclaimed_before_claim_frees_capacity() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        repo.create_task(&task_row_for_project(
            "stale-prep",
            "local-a",
            OrchestratorTaskStatus::Preparing,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "stale-prep",
            OrchestratorWorkflowState::InProgress,
            OrchestratorRunState::Preparing,
            None,
        )
        .await;
        // 回拨 updated_at 到 lease 之外，模拟 claim 后崩溃。
        sqlx::query(
            "UPDATE orchestrator_tasks SET updated_at = '2020-01-01T00:00:00Z' WHERE id = ?",
        )
        .bind("stale-prep")
        .execute(&pool)
        .await
        .unwrap();
        repo.create_task(&task_row_for_project(
            "queued-next",
            "local-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "queued-next",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;

        // max=1：若不回收 stale Preparing，queued-next 永远无法领取。
        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 1),
            &mut cursor,
            None,
        )
        .await
            .unwrap();
        let stale = repo.get_task("stale-prep").await.unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "queued-next");
        assert_eq!(stale.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(stale.run_state, OrchestratorRunState::Blocked);
        assert!(
            stale
                .blocked_reason
                .as_deref()
                .unwrap_or("")
                .contains("Preparing"),
            "回收后应写入中文阻塞原因"
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目 WORKFLOW.md 写错时必须阻止该项目新任务 dispatch，避免错误策略下提前创建 worktree/terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建带非法 WORKFLOW.md 的本机项目和 Todo/Idle 任务，执行领取 helper 后断言任务没有进入 Preparing。
    #[tokio::test]
    async fn invalid_project_workflow_blocks_new_dispatch_before_claim() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        let dir = tempfile::tempdir().expect("创建临时项目目录成功");
        std::fs::write(dir.path().join("WORKFLOW.md"), "---\n[\n---\nBody")
            .expect("写入非法 WORKFLOW.md 成功");
        insert_workbench_project_with_path(&pool, "local-a", "local", dir.path()).await;
        repo.create_task(&task_row_for_project(
            "todo-invalid-workflow",
            "local-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "todo-invalid-workflow",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 1),
            &mut cursor,
            None,
        )
        .await
            .unwrap();
        let persisted = repo.get_task("todo-invalid-workflow").await.unwrap();

        assert!(claimed.is_empty());
        assert_eq!(persisted.status, OrchestratorTaskStatus::Queued);
        assert_eq!(persisted.workflow_state, OrchestratorWorkflowState::Todo);
        assert_eq!(persisted.run_state, OrchestratorRunState::Idle);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     项目 workflow 可以调整 active states，scheduler 必须消费解析结果而不是固定 Todo/Rework。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 active_states=[backlog] 的 WORKFLOW.md，断言 Backlog/Idle 被领取而 Todo/Idle 留在队列中。
    #[tokio::test]
    async fn scheduler_consumes_project_workflow_active_states() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        let dir = tempfile::tempdir().expect("创建临时项目目录成功");
        std::fs::write(
            dir.path().join("WORKFLOW.md"),
            "---\nworkflow:\n  active_states:\n    - backlog\n---\nCustom prompt",
        )
        .expect("写入 WORKFLOW.md 成功");
        insert_workbench_project_with_path(&pool, "local-a", "local", dir.path()).await;
        for (id, workflow_state) in [
            ("backlog-active", OrchestratorWorkflowState::Backlog),
            ("todo-inactive", OrchestratorWorkflowState::Todo),
        ] {
            repo.create_task(&task_row_for_project(
                id,
                "local-a",
                OrchestratorTaskStatus::Queued,
            ))
            .await
            .unwrap();
            set_task_split_state(&pool, id, workflow_state, OrchestratorRunState::Idle, None).await;
        }

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 2),
            &mut cursor,
            None,
        )
        .await
            .unwrap();

        assert_eq!(
            claimed
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["backlog-active"]
        );
        assert_eq!(
            repo.get_task("todo-inactive").await.unwrap().status,
            OrchestratorTaskStatus::Queued
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     没有 WORKFLOW.md 的项目必须继续使用内置默认 workflow，不能因为项目未配置而停止自动化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建无 WORKFLOW.md 的真实项目目录和 Todo/Idle 任务，断言 scheduler 仍按默认 active states 领取。
    #[tokio::test]
    async fn missing_project_workflow_uses_built_in_default_for_dispatch() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        let dir = tempfile::tempdir().expect("创建临时项目目录成功");
        insert_workbench_project_with_path(&pool, "local-a", "local", dir.path()).await;
        repo.create_task(&task_row_for_project(
            "todo-default-workflow",
            "local-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "todo-default-workflow",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 1),
            &mut cursor,
            None,
        )
        .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "todo-default-workflow");
        assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench shortcut 不能由本机 scheduler 启动 Runner，否则会在错误设备上创建 worktree/terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同时插入 remote 高优先级任务和 local 低优先级任务，断言只领取 local。
    #[tokio::test]
    async fn remote_workbench_projects_are_skipped_by_global_claim() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "local-a", "local").await;
        insert_workbench_project(&pool, "remote-a", "remote").await;
        repo.create_task(&task_row_for_project(
            "remote-high",
            "remote-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        repo.create_task(&task_row_for_project(
            "local-low",
            "local-a",
            OrchestratorTaskStatus::Queued,
        ))
        .await
        .unwrap();
        set_task_split_state(
            &pool,
            "remote-high",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        set_task_split_state(
            &pool,
            "local-low",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;
        sqlx::query("UPDATE orchestrator_tasks SET priority = 100 WHERE id = 'remote-high'")
            .execute(&pool)
            .await
            .unwrap();

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 5),
            &mut cursor,
            None,
        )
        .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "local-low");
        assert_eq!(
            repo.get_task("remote-high").await.unwrap().status,
            OrchestratorTaskStatus::Queued
        );
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Phase 3 runtime 不再读取 legacy project_config；旧表里的 disabled/max=0 不能阻止全局开启后的 dispatch。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 disabled 且 max=0 的 legacy 配置，再用全局 enabled/max=1 领取任务，断言任务仍被领取。
    #[tokio::test]
    async fn legacy_project_config_does_not_affect_global_dispatch_claim() {
        let (pool, repo) = setup_repo().await;
        create_workbench_projects_table(&pool).await;
        insert_workbench_project(&pool, "project-1", "local").await;
        repo.get_or_create_project_config("project-1")
            .await
            .unwrap();
        sqlx::query(
            "UPDATE orchestrator_project_config SET enabled = 0, max_concurrent_tasks = 0 WHERE project_id = ?",
        )
        .bind("project-1")
        .execute(&pool)
        .await
        .unwrap();
        repo.create_task(&task_row("task-queued", OrchestratorTaskStatus::Queued))
            .await
            .unwrap();
        set_task_split_state(
            &pool,
            "task-queued",
            OrchestratorWorkflowState::Todo,
            OrchestratorRunState::Idle,
            None,
        )
        .await;

        let mut cursor = None;
        let claimed = claim_tasks_for_dispatch(
            &repo,
            &global_config(true, 1),
            &mut cursor,
            None,
        )
        .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, "task-queued");
        assert_eq!(claimed[0].status, OrchestratorTaskStatus::Preparing);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     用户可能在 runner 返回错误前终止 Preparing 任务，失败补偿不得把 Aborted 覆盖为 Blocked 或追加误导事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Preparing 任务后模拟 Abort，再执行 runner 失败记录 helper，断言返回/持久化状态仍为 Aborted 且 event 表为空。
    #[tokio::test]
    async fn runner_failure_after_abort_does_not_block_or_write_event() {
        let (pool, repo) = setup_repo().await;
        let task = task_row("task-1", OrchestratorTaskStatus::Preparing);
        repo.create_task(&task).await.unwrap();
        repo.set_task_status(&task.id, OrchestratorTaskStatus::Aborted, None)
            .await
            .unwrap();

        let returned = record_runner_failure(&repo, &task.id, "runner failed")
            .await
            .unwrap();
        let persisted = repo.get_task(&task.id).await.unwrap();
        let event_count = sqlx::query("SELECT COUNT(*) AS count FROM orchestrator_task_events")
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get::<i64, _>("count")
            .unwrap();

        assert_eq!(returned.status, OrchestratorTaskStatus::Aborted);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Aborted);
        assert!(persisted.blocked_reason.is_none());
        assert_eq!(event_count, 0);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Runner 已挂账为 Running 后写入终端仍可能失败，此时任务应进入 Blocked 并保留可接管现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 Running 任务后执行 runner 失败记录 helper，断言状态变为 Blocked、原因保留且 event 只写一条。
    #[tokio::test]
    async fn runner_failure_after_running_blocks_and_writes_event() {
        let (pool, repo) = setup_repo().await;
        let mut task = task_row("task-1", OrchestratorTaskStatus::Running);
        task.branch_name = Some("agent/task-1-test".to_string());
        task.worktree_id = Some("worktree-1".to_string());
        task.session_id = Some("session-1".to_string());
        repo.create_task(&task).await.unwrap();

        let returned = record_runner_failure(&repo, &task.id, "prompt write failed")
            .await
            .unwrap();
        let persisted = repo.get_task(&task.id).await.unwrap();
        let event_count = sqlx::query("SELECT COUNT(*) AS count FROM orchestrator_task_events")
            .fetch_one(&pool)
            .await
            .unwrap()
            .try_get::<i64, _>("count")
            .unwrap();

        assert_eq!(returned.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(persisted.status, OrchestratorTaskStatus::Blocked);
        assert_eq!(
            persisted.blocked_reason.as_deref(),
            Some("prompt write failed")
        );
        assert_eq!(persisted.worktree_id.as_deref(), Some("worktree-1"));
        assert_eq!(persisted.session_id.as_deref(), Some("session-1"));
        assert_eq!(event_count, 1);
    }
}
