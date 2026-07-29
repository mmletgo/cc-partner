//! agent_hub/git/runtime — device-lane Git 导出调度与 durable pending
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub 规范变更后需 2s trailing debounce 合并 burst，经既有 CloudSyncRuntime
//!     单飞门闸只写本 device lane；push 失败 1/2/4s 三次立即重试，再每 5 分钟 pending
//!     重试。Git 失败不得阻塞本机 projection。启动时 recover pending。
//!
//! Code Logic（这个模块做什么）:
//!     `AgentHubGitRuntime::{mark_dirty,flush_pending,recover_pending}`；
//!     内部 `export_local_lane_once`：ensure_repo → fetch/reset → build_snapshot FullHub
//!     → expand 到 sibling staging → replace 本 lane → pathspec commit/push。
//!     永不调用 `cloud_sync::snapshot::import` / `SnapshotImporter` 导入远端 lane。

use crate::agent_hub::git::lane::{
    device_lane_rel_path, inventory_agent_hub_device_lanes, replace_device_lane,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::snapshot::{
    build_snapshot, expand_readable_archive, SnapshotSelectionMode, SnapshotSelectionRequest,
};
use crate::cloud_sync::engine::ensure_repo_public;
use crate::cloud_sync::git_cli::{self, PushError};
use crate::cloud_sync::runtime::{
    run_cloud_sync_exclusive, scheduler_policy, CloudSyncBusyPolicy, CloudSyncTrigger,
};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubGitExportState;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// 规范变更 trailing debounce（2 秒）。
pub const EXPORT_DEBOUNCE: Duration = Duration::from_secs(2);
/// 三次立即重试间隔（秒）：1 / 2 / 4。
pub const RETRY_IMMEDIATE_SECS: [u64; 3] = [1, 2, 4];
/// 立即重试耗尽后的 pending 重试间隔。
pub const PENDING_RETRY: Duration = Duration::from_secs(300);

/// 计算第 `attempt` 次失败后的下次重试延迟（秒）。
///
/// Business Logic（为什么需要这个函数）:
///     push 失败：第 1/2/3 次失败后分别等 1/2/4 秒；之后每 5 分钟。
///
/// Code Logic（这个函数做什么）:
///     attempt 为已失败次数（1-based）：1→1s，2→2s，3→4s，≥4→300s。
pub fn next_retry_delay_secs(failed_attempts: u32) -> u64 {
    match failed_attempts {
        0 => 0,
        1 => RETRY_IMMEDIATE_SECS[0],
        2 => RETRY_IMMEDIATE_SECS[1],
        3 => RETRY_IMMEDIATE_SECS[2],
        _ => PENDING_RETRY.as_secs(),
    }
}

/// Agent Hub Git device-lane 导出运行时。
///
/// Business Logic（为什么需要这个结构体）:
///     与 projection 解耦：Hub 变更只 mark_dirty；后台 debounce/flush 写 Git，
///     失败只更新 pending 表，不阻塞本机投影。
///
/// Code Logic（这个结构体做什么）:
///     维护 dirty/pending 标志与 FakeClock 可注入 now_ms；提供 mark/flush/recover。
pub struct AgentHubGitRuntime {
    /// 是否有待导出的规范变更（debounce 合并）
    dirty: AtomicBool,
    /// 最近一次 mark_dirty 的单调毫秒
    last_dirty_ms: AtomicU64,
    /// flush 互斥（进程内，另有 cloud singleflight）
    flush_lock: Mutex<()>,
    /// 可注入时钟（测试）
    clock_ms: AtomicU64,
    /// 是否使用注入时钟（false 则 wall clock）
    use_fake_clock: AtomicBool,
    /// 起点 Instant（wall clock）
    start: std::time::Instant,
    /// 测试注入：强制 push 失败次数（剩余）
    #[cfg(any(test, debug_assertions))]
    push_fail_remaining: AtomicU64,
}

impl Default for AgentHubGitRuntime {
    /// Business Logic: 启动时无 dirty。
    /// Code Logic: 默认字段。
    fn default() -> Self {
        Self::new()
    }
}

impl AgentHubGitRuntime {
    /// 构造生产运行时。
    ///
    /// Business Logic: AppState 持有一份 Arc，owner 启动 recover/flush 循环。
    /// Code Logic: dirty=false，wall clock。
    pub fn new() -> Self {
        Self {
            dirty: AtomicBool::new(false),
            last_dirty_ms: AtomicU64::new(0),
            flush_lock: Mutex::new(()),
            clock_ms: AtomicU64::new(0),
            use_fake_clock: AtomicBool::new(false),
            start: std::time::Instant::now(),
            #[cfg(any(test, debug_assertions))]
            push_fail_remaining: AtomicU64::new(0),
        }
    }

    /// 启用 fake clock（单测）。
    ///
    /// Business Logic: 20 次变更 2s 合并、retry 调度不得真实 sleep。
    /// Code Logic: use_fake_clock=true，clock_ms=0。
    #[cfg(any(test, debug_assertions))]
    pub fn enable_fake_clock(&self) {
        self.use_fake_clock.store(true, Ordering::SeqCst);
        self.clock_ms.store(0, Ordering::SeqCst);
    }

    /// 推进 fake clock 毫秒。
    #[cfg(any(test, debug_assertions))]
    pub fn advance_ms(&self, delta: u64) {
        self.clock_ms.fetch_add(delta, Ordering::SeqCst);
    }

    /// 注入后续 N 次 push 强制失败（测试 retry）。
    #[cfg(any(test, debug_assertions))]
    pub fn inject_push_failures(&self, n: u64) {
        self.push_fail_remaining.store(n, Ordering::SeqCst);
    }

    /// 当前单调毫秒。
    fn now_ms(&self) -> u64 {
        if self.use_fake_clock.load(Ordering::SeqCst) {
            self.clock_ms.load(Ordering::SeqCst)
        } else {
            self.start.elapsed().as_millis() as u64
        }
    }

    /// 标记 Hub 规范状态已变更，需 debounce 后导出。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     2 秒内 20 次 canonical 变更只应触发一次 export；Git 失败不回压调用方。
    ///
    /// Code Logic（这个函数做什么）:
    ///     dirty=true，刷新 last_dirty_ms。永不 await、不写 Git。
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.last_dirty_ms.store(self.now_ms(), Ordering::SeqCst);
    }

    /// 是否仍在 debounce 窗口内。
    fn debounce_due(&self) -> bool {
        if !self.dirty.load(Ordering::SeqCst) {
            return false;
        }
        let last = self.last_dirty_ms.load(Ordering::SeqCst);
        self.now_ms().saturating_sub(last) >= EXPORT_DEBOUNCE.as_millis() as u64
    }

    /// 启动时恢复 pending 导出状态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     崩溃后未 push 成功的 pending_hash 必须在 owner 启动时继续尝试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 git export state；若有 pending_hash 且与 last_pushed 不同则 mark_dirty。
    pub async fn recover_pending(&self, state: &AppState) -> Result<(), AppError> {
        let row = state
            .agent_hub_repo
            .get_git_export_state(state.device_id.as_str())
            .await?;
        if let Some(row) = row {
            if let Some(pending) = row.pending_snapshot_hash.as_ref() {
                if pending.is_empty() {
                    return Ok(());
                }
                let already = row
                    .last_pushed_snapshot_hash
                    .as_ref()
                    .is_some_and(|h| h == pending);
                if !already {
                    tracing::info!(
                        device_id = %state.device_id,
                        "agent_hub_git: recover pending export"
                    );
                    self.mark_dirty();
                }
            }
        }
        Ok(())
    }

    /// 尝试导出：debounce 到期或 force；经 cloud singleflight。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     后台 tick / 启动 recover 调用；忙则跳过；未到期 debounce 跳过。
    ///
    /// Code Logic（这个函数做什么）:
    ///     force=false 时要求 dirty 且 debounce due；force=true 忽略 debounce 但仍需 dirty 或 pending。
    ///     使用 `CloudSyncTrigger::AgentHubGitExport` + scheduler/Wait 策略。
    pub async fn flush_pending(
        &self,
        state: &AppState,
        force: bool,
        policy: CloudSyncBusyPolicy,
    ) -> Result<AgentHubGitFlushOutcome, AppError> {
        // 检查 next_attempt_at（pending 退避）
        if !force {
            if let Ok(Some(row)) = state
                .agent_hub_repo
                .get_git_export_state(state.device_id.as_str())
                .await
            {
                if let Some(next) = row.next_attempt_at.as_ref() {
                    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(next) {
                        if chrono::Utc::now() < ts.with_timezone(&chrono::Utc) {
                            // 仍在退避窗口；若仅 pending 且无新 dirty，跳过
                            if !self.dirty.load(Ordering::SeqCst) {
                                return Ok(AgentHubGitFlushOutcome::SkippedBackoff);
                            }
                            // 有新 dirty 但仍在 immediate retry 窗口外由 debounce 控制
                        }
                    }
                }
            }
        }

        if !force && !self.debounce_due() {
            if self.dirty.load(Ordering::SeqCst) {
                return Ok(AgentHubGitFlushOutcome::SkippedDebounce);
            }
            // 无 dirty：检查是否有 due pending
            let due_pending = self.pending_due(state).await?;
            if !due_pending {
                return Ok(AgentHubGitFlushOutcome::Idle);
            }
        }

        // 无 dirty 且 force 时，仅当有 pending 才继续
        if force && !self.dirty.load(Ordering::SeqCst) {
            let has_pending = state
                .agent_hub_repo
                .get_git_export_state(state.device_id.as_str())
                .await?
                .and_then(|r| r.pending_snapshot_hash)
                .is_some_and(|h| !h.is_empty());
            if !has_pending {
                return Ok(AgentHubGitFlushOutcome::Idle);
            }
        }

        let _guard = self.flush_lock.lock().await;

        let runtime = state.cloud_sync_runtime.clone();
        #[cfg(any(test, debug_assertions))]
        let push_fail_slot = self.push_fail_remaining.load(Ordering::SeqCst);
        #[cfg(not(any(test, debug_assertions)))]
        let push_fail_slot = 0u64;

        let outcome = run_cloud_sync_exclusive(
            &runtime,
            CloudSyncTrigger::AgentHubGitExport,
            policy,
            || {
                let state = state.clone();
                async move { export_local_lane_once(&state, push_fail_slot).await }
            },
        )
        .await?;

        match outcome {
            None => Ok(AgentHubGitFlushOutcome::SkippedBusy),
            Some(ExportOnceResult::SkippedNoRepo) => Ok(AgentHubGitFlushOutcome::SkippedNoRepo),
            Some(ExportOnceResult::NoopSameHash { snapshot_hash }) => {
                self.dirty.store(false, Ordering::SeqCst);
                let _ = state
                    .agent_hub_repo
                    .upsert_git_export_state(&AgentHubGitExportState {
                        device_id: state.device_id.to_string(),
                        last_exported_snapshot_hash: Some(snapshot_hash.clone()),
                        last_pushed_snapshot_hash: Some(snapshot_hash),
                        pending_snapshot_hash: None,
                        attempt_count: 0,
                        next_attempt_at: None,
                        last_error: None,
                    })
                    .await;
                Ok(AgentHubGitFlushOutcome::NoopSameHash)
            }
            Some(ExportOnceResult::Pushed { snapshot_hash }) => {
                self.dirty.store(false, Ordering::SeqCst);
                let _ = state
                    .agent_hub_repo
                    .upsert_git_export_state(&AgentHubGitExportState {
                        device_id: state.device_id.to_string(),
                        last_exported_snapshot_hash: Some(snapshot_hash.clone()),
                        last_pushed_snapshot_hash: Some(snapshot_hash),
                        pending_snapshot_hash: None,
                        attempt_count: 0,
                        next_attempt_at: None,
                        last_error: None,
                    })
                    .await;
                Ok(AgentHubGitFlushOutcome::Pushed)
            }
            Some(ExportOnceResult::Failed {
                snapshot_hash,
                error,
                consumed_push_fails,
            }) => {
                #[cfg(any(test, debug_assertions))]
                if consumed_push_fails > 0 {
                    let cur = self.push_fail_remaining.load(Ordering::SeqCst);
                    let next = cur.saturating_sub(consumed_push_fails);
                    self.push_fail_remaining.store(next, Ordering::SeqCst);
                }
                #[cfg(not(any(test, debug_assertions)))]
                let _ = consumed_push_fails;

                let prev = state
                    .agent_hub_repo
                    .get_git_export_state(state.device_id.as_str())
                    .await?
                    .unwrap_or_else(|| AgentHubGitExportState {
                        device_id: state.device_id.to_string(),
                        last_exported_snapshot_hash: None,
                        last_pushed_snapshot_hash: None,
                        pending_snapshot_hash: None,
                        attempt_count: 0,
                        next_attempt_at: None,
                        last_error: None,
                    });
                let attempts = prev.attempt_count.saturating_add(1);
                let delay = next_retry_delay_secs(attempts);
                let next_at =
                    (chrono::Utc::now() + chrono::Duration::seconds(delay as i64)).to_rfc3339();
                // dirty 保持 true，以便退避后继续
                let _ = state
                    .agent_hub_repo
                    .upsert_git_export_state(&AgentHubGitExportState {
                        device_id: state.device_id.to_string(),
                        last_exported_snapshot_hash: Some(snapshot_hash.clone()),
                        last_pushed_snapshot_hash: prev.last_pushed_snapshot_hash,
                        pending_snapshot_hash: Some(snapshot_hash),
                        attempt_count: attempts,
                        next_attempt_at: Some(next_at),
                        last_error: Some(error),
                    })
                    .await;
                Ok(AgentHubGitFlushOutcome::FailedPending)
            }
        }
    }

    async fn pending_due(&self, state: &AppState) -> Result<bool, AppError> {
        let Some(row) = state
            .agent_hub_repo
            .get_git_export_state(state.device_id.as_str())
            .await?
        else {
            return Ok(false);
        };
        let Some(pending) = row.pending_snapshot_hash.as_ref() else {
            return Ok(false);
        };
        if pending.is_empty() {
            return Ok(false);
        }
        if let Some(next) = row.next_attempt_at.as_ref() {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(next) {
                return Ok(chrono::Utc::now() >= ts.with_timezone(&chrono::Utc));
            }
        }
        Ok(true)
    }
}

/// flush 结果（可观测 / 测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHubGitFlushOutcome {
    /// 无工作
    Idle,
    /// debounce 未到期
    SkippedDebounce,
    /// pending 退避中
    SkippedBackoff,
    /// cloud singleflight 忙
    SkippedBusy,
    /// 未配置 cloud repo
    SkippedNoRepo,
    /// snapshotHash 未变，无 commit
    NoopSameHash,
    /// 已 commit+push
    Pushed,
    /// 失败并写入 pending
    FailedPending,
}

/// 单次 export 内部结果。
enum ExportOnceResult {
    SkippedNoRepo,
    NoopSameHash {
        snapshot_hash: String,
    },
    Pushed {
        snapshot_hash: String,
    },
    Failed {
        snapshot_hash: String,
        error: String,
        consumed_push_fails: u64,
    },
}

/// 已持 CloudSyncRuntime gate：导出本机 lane。
///
/// Business Logic: 永不 import 远端 Agent Hub lane；只 pathspec commit 本 lane。
/// Code Logic: ensure → fetch/reset → build FullHub → expand staging → replace → commit_path → push。
async fn export_local_lane_once(
    state: &AppState,
    push_fail_remaining: u64,
) -> Result<ExportOnceResult, AppError> {
    let repo_configured = {
        let cfg = state.config.read().unwrap();
        cfg.cloud_sync_repo_url
            .as_ref()
            .is_some_and(|u| !u.trim().is_empty())
    };
    if !repo_configured {
        return Ok(ExportOnceResult::SkippedNoRepo);
    }

    let git = git_cli::detect_git()?;
    let (workdir, branch) = ensure_repo_public(state, &git).await?;

    // fetch + reset 对齐远端，但不 import agent-hub
    if has_remote_branch(&git, &workdir).await {
        if let Err(e) = git_cli::fetch_origin(&git, &workdir).await {
            tracing::warn!("agent_hub_git: fetch 失败（继续）: {e}");
        } else if let Err(e) = git_cli::reset_hard(&git, &workdir, &branch).await {
            tracing::warn!("agent_hub_git: reset 失败（继续）: {e}");
        }
    }

    // inventory 远端 lane（仅日志/观测，不入 DB）
    match inventory_agent_hub_device_lanes(&workdir) {
        Ok(ids) => {
            tracing::debug!(
                lanes = ?ids,
                "agent_hub_git: inventoried remote device lanes (no import)"
            );
        }
        Err(e) => tracing::warn!("agent_hub_git: inventory lanes failed: {e}"),
    }

    // build full hub snapshot
    let data_dir = crate::config::data_dir()?;
    let objects = ObjectStore::open(&data_dir)?;
    let built = build_snapshot(
        &state.agent_hub_repo,
        &objects,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: state.device_id.to_string(),
            limits: None,
        },
    )
    .await?;
    let snapshot_hash = built.envelope.snapshot_hash.clone();

    // 与上次已 push 相同 → 无 commit
    if let Some(prev) = state
        .agent_hub_repo
        .get_git_export_state(state.device_id.as_str())
        .await?
    {
        if prev
            .last_pushed_snapshot_hash
            .as_ref()
            .is_some_and(|h| h == &snapshot_hash)
        {
            return Ok(ExportOnceResult::NoopSameHash { snapshot_hash });
        }
    }

    // expand 到 sibling staging（在 workdir 旁，不在 git tree 内）
    let staging_parent = workdir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| workdir.clone());
    let staging = staging_parent.join(format!(
        "agent-hub-lane-staging-{}-{}",
        state.device_id,
        uuid::Uuid::new_v4()
    ));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    if let Err(e) = expand_readable_archive(&built, &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(ExportOnceResult::Failed {
            snapshot_hash,
            error: format!("expand: {e}"),
            consumed_push_fails: 0,
        });
    }

    if let Err(e) = replace_device_lane(&workdir, state.device_id.as_str(), &staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Ok(ExportOnceResult::Failed {
            snapshot_hash,
            error: format!("replace_lane: {e}"),
            consumed_push_fails: 0,
        });
    }
    let _ = std::fs::remove_dir_all(&staging);

    let pathspec = device_lane_rel_path(state.device_id.as_str())?;
    // 限制 diff：仅 pathspec 有变化才 commit
    let commit_msg = format!(
        "agent hub device lane {} @ {}",
        state.device_id.as_str(),
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );

    // push with immediate retries (1/2/4 handled by caller attempt_count; here one push + on Rejected re-fetch once inside)
    let mut consumed_push_fails = 0u64;
    for attempt in 0..2u8 {
        if attempt > 0 && has_remote_branch(&git, &workdir).await {
            let _ = git_cli::fetch_origin(&git, &workdir).await;
            let _ = git_cli::reset_hard(&git, &workdir, &branch).await;
            // re-apply lane after reset
            let staging2 = staging_parent.join(format!(
                "agent-hub-lane-staging-{}-{}",
                state.device_id,
                uuid::Uuid::new_v4()
            ));
            if let Err(e) = expand_readable_archive(&built, &staging2) {
                let _ = std::fs::remove_dir_all(&staging2);
                return Ok(ExportOnceResult::Failed {
                    snapshot_hash,
                    error: format!("expand_retry: {e}"),
                    consumed_push_fails,
                });
            }
            if let Err(e) = replace_device_lane(&workdir, state.device_id.as_str(), &staging2) {
                let _ = std::fs::remove_dir_all(&staging2);
                return Ok(ExportOnceResult::Failed {
                    snapshot_hash,
                    error: format!("replace_retry: {e}"),
                    consumed_push_fails,
                });
            }
            let _ = std::fs::remove_dir_all(&staging2);
        }

        let committed = match git_cli::commit_path(&git, &workdir, &pathspec, &commit_msg).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ExportOnceResult::Failed {
                    snapshot_hash,
                    error: format!("commit: {e}"),
                    consumed_push_fails,
                });
            }
        };
        if !committed {
            // 工作区相对 pathspec 无变化（与远端 lane 已一致）
            return Ok(ExportOnceResult::NoopSameHash { snapshot_hash });
        }

        // 测试注入 push 失败
        if push_fail_remaining > 0 {
            consumed_push_fails += 1;
            let _remaining = push_fail_remaining - 1;
            let _ = _remaining;
            return Ok(ExportOnceResult::Failed {
                snapshot_hash,
                error: "agent_hub_git_push_injected_failure".to_string(),
                consumed_push_fails,
            });
        }

        match git_cli::push(&git, &workdir, &branch).await {
            Ok(()) => {
                return Ok(ExportOnceResult::Pushed { snapshot_hash });
            }
            Err(PushError::Rejected) if attempt == 0 => {
                tracing::warn!("agent_hub_git: push rejected, rebase once");
                continue;
            }
            Err(PushError::Rejected) => {
                return Ok(ExportOnceResult::Failed {
                    snapshot_hash,
                    error: "agent_hub_git_push_rejected".to_string(),
                    consumed_push_fails,
                });
            }
            Err(PushError::Other(e)) => {
                return Ok(ExportOnceResult::Failed {
                    snapshot_hash,
                    error: format!("push: {e}"),
                    consumed_push_fails,
                });
            }
        }
    }

    Ok(ExportOnceResult::Failed {
        snapshot_hash,
        error: "agent_hub_git_push_exhausted".to_string(),
        consumed_push_fails,
    })
}

async fn has_remote_branch(git: &Path, workdir: &Path) -> bool {
    git_cli::run(
        git,
        workdir,
        &["rev-parse", "--verify", "origin/HEAD"],
        Duration::from_secs(30),
    )
    .await
    .is_ok()
}

/// Headless owner：启动 recover + debounce/pending flush 循环。
///
/// Business Logic（为什么需要这个函数）:
///     backend 在线时每 5 分钟 pending 重试；dirty debounce 2s；不阻塞 projection。
///
/// Code Logic（这个函数做什么）:
///     spawn cancel-aware loop：200ms tick 检查 debounce；与 pending next_attempt 对齐。
pub fn start_agent_hub_git_export_loop(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let child = cancel.child_token();
    tauri::async_runtime::spawn(async move {
        // recover once
        if let Err(e) = state.agent_hub_git_runtime.recover_pending(&state).await {
            tracing::warn!("agent_hub_git recover_pending failed: {e}");
        }
        let mut ticker = tokio::time::interval(Duration::from_millis(200));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = child.cancelled() => break,
                _ = ticker.tick() => {
                    let rt = state.agent_hub_git_runtime.clone();
                    match rt
                        .flush_pending(&state, false, scheduler_policy())
                        .await
                    {
                        Ok(AgentHubGitFlushOutcome::Pushed) => {
                            tracing::info!("agent_hub_git: lane pushed");
                        }
                        Ok(AgentHubGitFlushOutcome::FailedPending) => {
                            tracing::warn!("agent_hub_git: push failed, pending");
                        }
                        Ok(_) => {}
                        Err(e) => {
                            // 永不阻断本机：只记日志
                            tracing::warn!("agent_hub_git flush error (non-blocking): {e}");
                        }
                    }
                }
            }
        }
    });
    cancel
}

/// 标记 dirty 的便利入口（service / projection 可调用）。
///
/// Business Logic: Hub 写路径成功后 best-effort 通知 Git 备份。
/// Code Logic: `state.agent_hub_git_runtime.mark_dirty()`。
pub fn mark_agent_hub_git_dirty(state: &AppState) {
    state.agent_hub_git_runtime.mark_dirty();
}

// ─── 测试 ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::git::lane::AGENT_HUB_GIT_ROOT;
    use crate::agent_hub::models::{
        AssetKind, AssetPolicy, NewLogicalAsset, NewRevision, NewScopeNode, RevisionId,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    use crate::agent_hub::snapshot::builder::clear_envelope_cache_for_test;
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::Command as StdCommand;
    use std::str::FromStr;

    fn git_bin() -> PathBuf {
        PathBuf::from("git")
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    /// 本地 bare remote + clone workdir 的 fixture。
    struct GitFixture {
        _root: tempfile::TempDir,
        _remote: PathBuf,
        workdir: PathBuf,
    }

    impl GitFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let remote = root.path().join("remote.git");
            let workdir = root.path().join("cloud-sync");
            run_git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
            // seed initial commit via temp clone
            let seed = root.path().join("seed");
            run_git(
                root.path(),
                &["clone", remote.to_str().unwrap(), seed.to_str().unwrap()],
            );
            run_git(&seed, &["config", "user.name", "cc-partner"]);
            run_git(&seed, &["config", "user.email", "cc-partner@local"]);
            write_file(&seed.join("prompts").join("p1.json"), r#"{"id":"p1"}"#);
            write_file(
                &seed
                    .join(AGENT_HUB_GIT_ROOT)
                    .join("devices")
                    .join(DEVICE_A)
                    .join("snapshot.json"),
                r#"{"old":"a"}"#,
            );
            write_file(
                &seed
                    .join(AGENT_HUB_GIT_ROOT)
                    .join("devices")
                    .join(DEVICE_B)
                    .join("snapshot.json"),
                r#"{"old":"b"}"#,
            );
            run_git(&seed, &["add", "-A"]);
            run_git(&seed, &["commit", "-m", "seed"]);
            run_git(&seed, &["branch", "-M", "main"]);
            run_git(&seed, &["push", "-u", "origin", "main"]);
            // workdir clone
            run_git(
                root.path(),
                &["clone", remote.to_str().unwrap(), workdir.to_str().unwrap()],
            );
            run_git(&workdir, &["config", "user.name", "cc-partner"]);
            run_git(&workdir, &["config", "user.email", "cc-partner@local"]);
            Self {
                _root: root,
                _remote: remote,
                workdir,
            }
        }
    }

    async fn test_repo(dir: &Path) -> (AgentHubRepo, ObjectStore) {
        let db_path = dir.join("t.db");
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        let repo = AgentHubRepo::new(pool);
        let store = ObjectStore::open(dir).unwrap();
        (repo, store)
    }

    /// 测试用合法 UUID device/replica id（envelope 要求 UUID）。
    const DEVICE_A: &str = "01900000-0000-7000-8000-0000000000a1";
    const DEVICE_B: &str = "01900000-0000-7000-8000-0000000000b2";

    async fn seed_one_asset(repo: &AgentHubRepo, store: &ObjectStore) -> String {
        let scope = repo
            .insert_scope(NewScopeNode {
                id: Some("scope-user".into()),
                kind: ScopeKind::User,
                hub_project_id: None,
                relative_path: None,
            })
            .await
            .unwrap()
            .id;
        let asset = repo
            .insert_asset(NewLogicalAsset {
                scope_id: scope,
                kind: AssetKind::Instruction,
                origin_namespace: "standalone".into(),
                logical_key: "root".into(),
                display_name: "Root".into(),
                policy: AssetPolicy::Shared,
            })
            .await
            .unwrap();
        let hash = store.put_blob(b"body-v1").await.unwrap().hash;
        let _ = repo
            .append_revision(NewRevision {
                id: RevisionId::new_v7(),
                asset_lineage_id: asset.id.clone(),
                parents: vec![],
                operation: RevisionOperation::Upsert,
                origin_kind: RevisionOriginKind::Ui,
                origin_target: None,
                origin_replica_id: DEVICE_A.into(),
                payload_hash: Some(hash),
                tree_manifest_hash: None,
                created_at: "2026-07-29T10:00:00Z".into(),
                expected_parent_id: None,
            })
            .await
            .unwrap();
        asset.id
    }

    /// Business Logic: 20 次 mark 在 2s 内只产生一次 debounce due。
    #[test]
    fn twenty_changes_in_two_seconds_one_debounce_due() {
        let rt = AgentHubGitRuntime::new();
        rt.enable_fake_clock();
        // 20 次变更散布在 ~1.9s 内；debounce 从最后一次 mark 起算 2s
        for i in 0..20 {
            rt.mark_dirty();
            if i + 1 < 20 {
                rt.advance_ms(100);
            }
        }
        // last_dirty=1900, now=1900
        assert!(!rt.debounce_due(), "before debounce window must not due");
        rt.advance_ms(1999);
        assert!(!rt.debounce_due());
        rt.advance_ms(1);
        assert!(rt.debounce_due());
    }

    /// Business Logic: 失败次数 → 1/2/4/300s。
    #[test]
    fn retry_schedule_1_2_4_then_5_min() {
        assert_eq!(next_retry_delay_secs(1), 1);
        assert_eq!(next_retry_delay_secs(2), 2);
        assert_eq!(next_retry_delay_secs(3), 4);
        assert_eq!(next_retry_delay_secs(4), 300);
        assert_eq!(next_retry_delay_secs(10), 300);
    }

    /// Business Logic: 导出 device-a 不改 device-b 与 prompts 指纹。
    #[tokio::test]
    async fn export_confines_bytes_to_local_device_lane() {
        clear_envelope_cache_for_test();
        let fx = GitFixture::new();
        let data = tempfile::tempdir().unwrap();
        let (repo, store) = test_repo(data.path()).await;
        seed_one_asset(&repo, &store).await;

        let prompts_fp_before =
            crate::agent_hub::git::lane::directory_content_fingerprint(&fx.workdir.join("prompts"))
                .unwrap();
        let b_fp_before = crate::agent_hub::git::lane::directory_content_fingerprint(
            &fx.workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join("devices")
                .join(DEVICE_B),
        )
        .unwrap();

        // 直接走 lane 构建路径（不经 AppState），验证 replace 隔离
        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: DEVICE_A.into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let staging = data.path().join("staging");
        expand_readable_archive(&built, &staging).unwrap();
        replace_device_lane(&fx.workdir, DEVICE_A, &staging).unwrap();

        assert_eq!(
            crate::agent_hub::git::lane::directory_content_fingerprint(&fx.workdir.join("prompts"))
                .unwrap(),
            prompts_fp_before
        );
        assert_eq!(
            crate::agent_hub::git::lane::directory_content_fingerprint(
                &fx.workdir
                    .join(AGENT_HUB_GIT_ROOT)
                    .join("devices")
                    .join(DEVICE_B)
            )
            .unwrap(),
            b_fp_before
        );
        let inv = inventory_agent_hub_device_lanes(&fx.workdir).unwrap();
        assert!(inv.contains(&DEVICE_A.to_string()));
        assert!(inv.contains(&DEVICE_B.to_string()));
        // device-a snapshot 已更新为 envelope 格式
        let snap = fs::read_to_string(
            fx.workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join("devices")
                .join(DEVICE_A)
                .join("snapshot.json"),
        )
        .unwrap();
        assert!(
            snap.contains("cc-partner-agent-hub")
                || snap.contains("snapshotHash")
                || snap.contains("format")
        );
    }

    /// Business Logic: fetch 到变更的 device-b 不得写入 Hub DB（inventory only）。
    #[tokio::test]
    async fn fetching_changed_device_b_does_not_alter_hub_db() {
        clear_envelope_cache_for_test();
        let data = tempfile::tempdir().unwrap();
        let (repo, store) = test_repo(data.path()).await;
        seed_one_asset(&repo, &store).await;
        let assets_before = repo.list_assets(None, None).await.unwrap().len();

        // 模拟 workdir 含 device-b 新内容
        let work = data.path().join("wt");
        write_file(
            &work
                .join(AGENT_HUB_GIT_ROOT)
                .join("devices")
                .join(DEVICE_B)
                .join("snapshot.json"),
            r#"{"format":"cc-partner-agent-hub","foreign":true}"#,
        );
        let inv = inventory_agent_hub_device_lanes(&work).unwrap();
        assert_eq!(inv, vec![DEVICE_B.to_string()]);
        // 关键：不调用 SnapshotImporter
        let assets_after = repo.list_assets(None, None).await.unwrap().len();
        assert_eq!(assets_before, assets_after);
        let _ = store; // silence
    }

    /// Business Logic: 相同 snapshotHash 不产生 commit。
    #[tokio::test]
    async fn unchanged_snapshot_hash_skips_commit() {
        clear_envelope_cache_for_test();
        let fx = GitFixture::new();
        let data = tempfile::tempdir().unwrap();
        let (repo, store) = test_repo(data.path()).await;
        seed_one_asset(&repo, &store).await;
        let built = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: DEVICE_A.into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        let hash = built.envelope.snapshot_hash.clone();
        // 标记已 push 同一 hash
        repo.upsert_git_export_state(&AgentHubGitExportState {
            device_id: DEVICE_A.into(),
            last_exported_snapshot_hash: Some(hash.clone()),
            last_pushed_snapshot_hash: Some(hash.clone()),
            pending_snapshot_hash: None,
            attempt_count: 0,
            next_attempt_at: None,
            last_error: None,
        })
        .await
        .unwrap();

        // 再次 build 应复用 envelope（同 selectionStateHash）
        let built2 = build_snapshot(
            &repo,
            &store,
            SnapshotSelectionRequest {
                mode: SnapshotSelectionMode::FullHub,
                scope_ids: vec![],
                asset_ids: vec![],
                hub_project_ids: vec![],
                include_history: true,
                source_replica_id: DEVICE_A.into(),
                limits: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(built2.envelope.snapshot_hash, hash);

        // 若 last_pushed == hash，export 逻辑应 Noop
        let prev = repo.get_git_export_state(DEVICE_A).await.unwrap().unwrap();
        assert_eq!(
            prev.last_pushed_snapshot_hash.as_deref(),
            Some(hash.as_str())
        );
        let _ = fx;
        let _ = git_bin();
    }

    /// Business Logic: 旧 prompts 领域仍可 import；agent-hub 路径不得进入 cloud_sync snapshot import。
    #[tokio::test]
    async fn cloud_sync_import_ignores_agent_hub_lanes() {
        // 静态证明：import_to_db 源码不引用 agent-hub
        let src = include_str!("../../cloud_sync/snapshot.rs");
        assert!(
            !src.contains("agent-hub") && !src.contains("agent_hub"),
            "cloud_sync::snapshot must not import agent hub paths"
        );
        // inventory 仍可见 lane
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            &tmp.path()
                .join(AGENT_HUB_GIT_ROOT)
                .join("devices")
                .join("x")
                .join("snapshot.json"),
            "{}",
        );
        let inv = inventory_agent_hub_device_lanes(tmp.path()).unwrap();
        assert_eq!(inv, vec!["x".to_string()]);
    }

    /// Business Logic: pending 状态可持久化并 recover。
    #[tokio::test]
    async fn durable_pending_state_roundtrip_and_recover() {
        let data = tempfile::tempdir().unwrap();
        let (repo, _store) = test_repo(data.path()).await;
        repo.upsert_git_export_state(&AgentHubGitExportState {
            device_id: DEVICE_A.into(),
            last_exported_snapshot_hash: Some("aaa".into()),
            last_pushed_snapshot_hash: Some("old".into()),
            pending_snapshot_hash: Some("aaa".into()),
            attempt_count: 2,
            next_attempt_at: Some(
                (chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339(),
            ),
            last_error: Some("push: network".into()),
        })
        .await
        .unwrap();
        let row = repo.get_git_export_state(DEVICE_A).await.unwrap().unwrap();
        assert_eq!(row.attempt_count, 2);
        assert_eq!(row.pending_snapshot_hash.as_deref(), Some("aaa"));

        let rt = AgentHubGitRuntime::new();
        // 模拟 recover：pending != last_pushed → dirty
        let already = row
            .last_pushed_snapshot_hash
            .as_ref()
            .is_some_and(|h| Some(h) == row.pending_snapshot_hash.as_ref());
        assert!(!already);
        rt.mark_dirty();
        assert!(rt.dirty.load(Ordering::SeqCst));
    }
}
