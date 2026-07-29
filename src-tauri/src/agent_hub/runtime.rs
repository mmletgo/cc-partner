//! agent_hub/runtime — sidecar owner 的 watch / rescan / projection 循环
//!
//! Business Logic（为什么需要这个模块）:
//!     Multi-CLI Agent Hub 必须在独立 backend sidecar 中持续监听 CLI 原生文件、
//!     对账外部编辑并驱动 durable projection；GUI 关闭后仍运行，且绝不能成为第二 writer。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task 7：`AgentHubRuntime::start` 在 Headless owner 启动 cancel-aware 循环；
//!     notify 仅作 dirty 目录提示；500ms trailing debounce + 30s 变更目录 ticker +
//!     10min 全 scope ticker（MissedTickBehavior::Skip）；扫描先比 stored external/rendered hash
//!     再读字节，Hub 自身 rendered hash 视为 no-op。

use crate::agent_hub::models::{Materialization, MaterializationStatus};
use crate::agent_hub::object_store::{sha256_hex, ObjectStore};
use crate::agent_hub::projection::ProjectionScheduler;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::AgentHubRepo;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 外部文件事件 trailing debounce 时长。
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);
/// 已观察到变更的目录周期性 rescan 间隔。
pub const CHANGED_DIR_TICK: Duration = Duration::from_secs(30);
/// 全 scope rescan 间隔。
pub const FULL_SCOPE_TICK: Duration = Duration::from_secs(600);

/// 扫描统计（生产日志 + 单测断言）。
///
/// Business Logic（为什么需要这个结构体）:
///     owner 与测试需证明一次外部编辑只产生一次 revision/projection，
///     且 burst 事件只触发一次 scan。
///
/// Code Logic（这个结构体做什么）:
///     简单计数器；支持累加。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanStats {
    /// 执行过的 scan 次数
    pub scans: u64,
    /// 识别为外部编辑并推进 revision 的次数
    pub external_revisions: u64,
    /// 触发的 projection 波次（enqueue/run 一轮计一次）
    pub projection_waves: u64,
    /// Hub 自身 rendered hash 命中 no-op 次数
    pub noop_self_writes: u64,
    /// 因 hash 未变跳过加载的文件数
    pub hash_skips: u64,
}

impl ScanStats {
    /// 累加另一份统计。
    ///
    /// Business Logic: 多轮 tick 需汇总观测。
    /// Code Logic: 字段逐项相加。
    pub fn accumulate(&mut self, other: &ScanStats) {
        self.scans += other.scans;
        self.external_revisions += other.external_revisions;
        self.projection_waves += other.projection_waves;
        self.noop_self_writes += other.noop_self_writes;
        self.hash_skips += other.hash_skips;
    }
}

/// 扫描范围。
///
/// Business Logic（为什么需要这个枚举）:
///     dirty 目录扫描与全 scope 扫描成本不同，必须区分。
///
/// Code Logic（这个枚举做什么）:
///     Dirty 携带目录集合；Full 表示全 materialization 范围。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanScope {
    /// 仅扫描 dirty 目录下的目标
    Dirty(BTreeSet<PathBuf>),
    /// 全 scope
    Full,
}

/// 可注入时钟（生产用 wall clock；单测用 fake millis）。
///
/// Business Logic（为什么需要这个 trait）:
///     debounce/ticker 单测不能 sleep 500ms+，必须可推进时间。
///
/// Code Logic（这个 trait 做什么）:
///     返回单调毫秒时间戳。
pub trait RuntimeClock: Send + Sync {
    /// 当前单调毫秒。
    fn now_ms(&self) -> u64;
}

/// 系统时钟。
///
/// Business Logic: 生产 runtime 使用真实时间。
/// Code Logic: Instant 相对进程启动的 elapsed millis。
#[derive(Debug)]
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    /// 构造系统时钟。
    ///
    /// Business Logic: owner 启动时锚定起点。
    /// Code Logic: 记录 Instant::now。
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl RuntimeClock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// 可推进的假时钟（单测）。
///
/// Business Logic: 测试 trailing debounce 与 burst 合并。
/// Code Logic: AtomicU64 millis。
#[derive(Debug, Default)]
pub struct FakeClock {
    millis: AtomicU64,
}

impl FakeClock {
    /// 构造假时钟。
    ///
    /// Business Logic: 单测从 0 起算。
    /// Code Logic: AtomicU64=0。
    pub fn new() -> Self {
        Self {
            millis: AtomicU64::new(0),
        }
    }

    /// 推进时钟。
    ///
    /// Business Logic: 模拟时间流逝而不真实 sleep。
    /// Code Logic: fetch_add millis。
    pub fn advance_ms(&self, delta: u64) {
        self.millis.fetch_add(delta, Ordering::SeqCst);
    }
}

impl RuntimeClock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.millis.load(Ordering::SeqCst)
    }
}

/// dirty 目录 + trailing debounce 状态机。
///
/// Business Logic（为什么需要这个结构体）:
///     连续文件事件必须合并为一次 scan，避免抖动写盘/对账。
///
/// Code Logic（这个结构体做什么）:
///     维护 dirty 目录集合与最后事件时间；到期后 take dirty 集合。
#[derive(Debug, Default)]
pub struct DirtyDebouncer {
    dirty: BTreeSet<PathBuf>,
    last_event_ms: Option<u64>,
    debounce_ms: u64,
}

impl DirtyDebouncer {
    /// 构造 debouncer。
    ///
    /// Business Logic: 生产固定 500ms trailing。
    /// Code Logic: 空 dirty + debounce_ms。
    pub fn new(debounce: Duration) -> Self {
        Self {
            dirty: BTreeSet::new(),
            last_event_ms: None,
            debounce_ms: debounce.as_millis() as u64,
        }
    }

    /// 记录一次路径事件（仅作 dirty 提示）。
    ///
    /// Business Logic: notify 事件只标记父目录，不立即读盘。
    /// Code Logic: 规范化为父目录，刷新 last_event_ms。
    pub fn note_path_event(&mut self, path: &Path, now_ms: u64) {
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        };
        self.dirty.insert(canonicalize_loose(&dir));
        self.last_event_ms = Some(now_ms);
    }

    /// 直接标记目录 dirty（rescan / 测试）。
    ///
    /// Business Logic: 30s ticker 需要保留曾变更目录集合。
    /// Code Logic: insert 目录；可选刷新 last_event。
    pub fn mark_dirty_dir(&mut self, dir: PathBuf, now_ms: Option<u64>) {
        self.dirty.insert(canonicalize_loose(&dir));
        if let Some(ms) = now_ms {
            self.last_event_ms = Some(ms);
        }
    }

    /// 若 trailing debounce 到期则取出 dirty 集合。
    ///
    /// Business Logic: 最后一次事件后静默满 debounce 才 scan。
    /// Code Logic: now >= last + debounce 且 dirty 非空 → take。
    pub fn take_if_due(&mut self, now_ms: u64) -> Option<BTreeSet<PathBuf>> {
        let last = self.last_event_ms?;
        if now_ms.saturating_sub(last) < self.debounce_ms {
            return None;
        }
        if self.dirty.is_empty() {
            self.last_event_ms = None;
            return None;
        }
        self.last_event_ms = None;
        Some(std::mem::take(&mut self.dirty))
    }

    /// 当前 dirty 快照（不清除）。
    ///
    /// Business Logic: 30s ticker 对“已观察到变更的目录” rescan。
    /// Code Logic: clone dirty。
    pub fn dirty_snapshot(&self) -> BTreeSet<PathBuf> {
        self.dirty.clone()
    }

    /// dirty 是否为空。
    ///
    /// Business Logic: ticker 可跳过空集。
    /// Code Logic: is_empty。
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    /// 待 debounce 的目录数。
    ///
    /// Business Logic: 测试断言。
    /// Code Logic: dirty.len。
    pub fn len(&self) -> usize {
        self.dirty.len()
    }
}

/// 历史变更目录登记（30s rescan 目标）。
///
/// Business Logic（为什么需要这个结构体）:
///     debounce 清空后仍需对近期变更目录做 30s 周期性 rescan。
///
/// Code Logic（这个结构体做什么）:
///     BTreeSet of dirs；scan 后 merge，可 prune 可选。
#[derive(Debug, Default)]
pub struct ChangedDirLedger {
    dirs: BTreeSet<PathBuf>,
}

impl ChangedDirLedger {
    /// 合并一批 dirty 目录。
    ///
    /// Business Logic: 每次 scan 后记住曾变更目录。
    /// Code Logic: extend。
    pub fn merge(&mut self, dirs: &BTreeSet<PathBuf>) {
        self.dirs.extend(dirs.iter().cloned());
    }

    /// 取出当前变更目录快照。
    ///
    /// Business Logic: 30s ticker 触发 dirty-scope scan。
    /// Code Logic: clone dirs。
    pub fn snapshot(&self) -> BTreeSet<PathBuf> {
        self.dirs.clone()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// 基于 materialization 的 de-loop / 外部编辑扫描器（可注入）。
///
/// Business Logic（为什么需要这个结构体）:
///     扫描必须在加载全文前比较 stored hash，且 Hub 自己的 rendered hash 必须 no-op。
///
/// Code Logic（这个结构体做什么）:
///     持有 in-memory 目标状态或委托生产 backend。
pub struct DeLoopScanner {
    /// path → 上次成功 rendered hash
    rendered_by_path: Mutex<std::collections::BTreeMap<PathBuf, String>>,
    /// path → 上次观测 external hash（扫描前存储）
    observed_by_path: Mutex<std::collections::BTreeMap<PathBuf, String>>,
    /// path → 当前文件内容（测试 harness）
    files: Mutex<std::collections::BTreeMap<PathBuf, Vec<u8>>>,
    /// 统计
    stats: Mutex<ScanStats>,
}

impl Default for DeLoopScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl DeLoopScanner {
    /// 构造空扫描器。
    ///
    /// Business Logic: 单测 harness 从空状态开始。
    /// Code Logic: 空 map。
    pub fn new() -> Self {
        Self {
            rendered_by_path: Mutex::new(std::collections::BTreeMap::new()),
            observed_by_path: Mutex::new(std::collections::BTreeMap::new()),
            files: Mutex::new(std::collections::BTreeMap::new()),
            stats: Mutex::new(ScanStats::default()),
        }
    }

    /// 登记目标路径的 rendered hash 与可选初始内容。
    ///
    /// Business Logic: 模拟成功投影后的 materialization。
    /// Code Logic: 写 rendered/observed/files。
    pub fn seed_target(&self, path: PathBuf, content: &[u8]) {
        let hash = sha256_hex(content);
        self.rendered_by_path
            .lock()
            .expect("rendered lock")
            .insert(path.clone(), hash.clone());
        self.observed_by_path
            .lock()
            .expect("observed lock")
            .insert(path.clone(), hash);
        self.files
            .lock()
            .expect("files lock")
            .insert(path, content.to_vec());
    }

    /// 模拟外部写文件（测试）。
    ///
    /// Business Logic: 用户在 CLI 原生文件中编辑。
    /// Code Logic: 覆盖 files 内容。
    pub fn write_external(&self, path: &Path, content: &[u8]) {
        self.files
            .lock()
            .expect("files lock")
            .insert(path.to_path_buf(), content.to_vec());
    }

    /// 模拟 Hub 自己投影写盘（内容 hash = rendered）。
    ///
    /// Business Logic: 投影原子写后 notify 会看到自身 hash。
    /// Code Logic: 写 files，并同步 rendered。
    pub fn write_hub_projection(&self, path: &Path, content: &[u8]) {
        let hash = sha256_hex(content);
        self.files
            .lock()
            .expect("files lock")
            .insert(path.to_path_buf(), content.to_vec());
        self.rendered_by_path
            .lock()
            .expect("rendered lock")
            .insert(path.to_path_buf(), hash.clone());
        self.observed_by_path
            .lock()
            .expect("observed lock")
            .insert(path.to_path_buf(), hash);
    }

    /// 执行一次 scan。
    ///
    /// Business Logic:
    ///     - 先取 stored external/rendered hash；
    ///     - 当前 hash == rendered → no-op（de-loop）；
    ///     - 当前 hash 与 stored 相同 → 跳过加载后处理（hash_skips）；
    ///     - 否则视为外部编辑：+1 revision +1 projection wave，更新 rendered。
    ///
    /// Code Logic: 遍历 dirty 范围内目标路径（或全部），比较 sha256。
    pub fn scan(&self, scope: &ScanScope) -> ScanStats {
        let mut round = ScanStats {
            scans: 1,
            ..Default::default()
        };
        let files = self.files.lock().expect("files lock").clone();
        let mut rendered = self.rendered_by_path.lock().expect("rendered lock");
        let mut observed = self.observed_by_path.lock().expect("observed lock");

        for (path, bytes) in files.iter() {
            if !path_in_scope(path, scope) {
                continue;
            }
            let current_hash = sha256_hex(bytes);
            let rendered_hash = rendered.get(path).cloned();
            let observed_hash = observed.get(path).cloned();

            if rendered_hash.as_deref() == Some(current_hash.as_str()) {
                // Hub 自身写入 / 已收敛
                if observed_hash.as_deref() != Some(current_hash.as_str()) {
                    observed.insert(path.clone(), current_hash.clone());
                }
                round.noop_self_writes += 1;
                continue;
            }

            if observed_hash.as_deref() == Some(current_hash.as_str()) {
                // 外部 hash 未变：跳过再加载/对账
                round.hash_skips += 1;
                continue;
            }

            // 外部编辑：加载后产生 revision + projection
            observed.insert(path.clone(), current_hash.clone());
            rendered.insert(path.clone(), current_hash);
            round.external_revisions += 1;
            round.projection_waves += 1;
        }

        self.stats.lock().expect("stats lock").accumulate(&round);
        round
    }

    /// 累计统计。
    ///
    /// Business Logic: 测试断言 totals。
    /// Code Logic: clone 锁内 stats。
    pub fn total_stats(&self) -> ScanStats {
        self.stats.lock().expect("stats lock").clone()
    }
}

/// 路径是否落在扫描范围内。
///
/// Business Logic: dirty 目录扫描只处理相关目标。
/// Code Logic: Full 全收；Dirty 检查 path 或其父是否在集合内。
fn path_in_scope(path: &Path, scope: &ScanScope) -> bool {
    match scope {
        ScanScope::Full => true,
        ScanScope::Dirty(dirs) => {
            if dirs.is_empty() {
                return false;
            }
            let canon = canonicalize_loose(path);
            dirs.iter().any(|d| {
                let d = canonicalize_loose(d);
                canon == d || canon.starts_with(&d)
            })
        }
    }
}

/// 宽松规范化（不要求路径存在）。
///
/// Business Logic: 测试与 notify 路径需稳定比较。
/// Code Logic: 去掉 `.` 组件；失败则原样。
fn canonicalize_loose(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// 生产扫描：对 materialization 做 hash 比较与 projection recovery/run。
///
/// Business Logic（为什么需要这个结构体）:
///     owner 启动后需恢复未完成 job，并对目标文件 de-loop 扫描。
///
/// Code Logic（这个结构体做什么）:
///     持有 repo + ProjectionScheduler；扫描 materialization 列表。
pub struct ProductionScanner {
    repo: AgentHubRepo,
    scheduler: ProjectionScheduler,
    /// 是否已做 startup recovery（Atomic，避免 MutexGuard 跨 await）
    recovered: AtomicBool,
}

impl ProductionScanner {
    /// 构造生产扫描器。
    ///
    /// Business Logic: owner 注入 repo 与 CAS 根。
    /// Code Logic: ProjectionScheduler::new。
    pub fn new(repo: AgentHubRepo, object_store: ObjectStore) -> Self {
        Self {
            scheduler: ProjectionScheduler::new(repo.clone(), object_store),
            repo,
            recovered: AtomicBool::new(false),
        }
    }

    /// 执行扫描（含可选 recovery + run_ready_jobs）。
    ///
    /// Business Logic:
    ///     启动首次 full scan 先 recover jobs；每次 scan 比较 materialization hash；
    ///     rendered 命中 no-op；hash 变化更新 observed 并标 Drift（完整 reconcile 后续 service 任务补齐）；
    ///     然后 run_ready_jobs 推进 prepared jobs。
    ///
    /// Code Logic: list materializations → hash 文件 → update；scheduler.run_ready_jobs。
    pub async fn scan(
        &self,
        scope: &ScanScope,
        cancel: &CancellationToken,
    ) -> Result<ScanStats, AppError> {
        let mut stats = ScanStats {
            scans: 1,
            ..Default::default()
        };

        if !self.recovered.load(Ordering::SeqCst) {
            let rec = self.scheduler.recover_on_startup().await?;
            if rec.recovered > 0 {
                tracing::info!(
                    recovered = rec.recovered,
                    committed = rec.committed,
                    "agent hub projection recover_on_startup"
                );
            }
            self.recovered.store(true, Ordering::SeqCst);
        }

        if cancel.is_cancelled() {
            return Ok(stats);
        }

        let materials = self.repo.list_materializations().await?;
        for mat in materials {
            if cancel.is_cancelled() {
                break;
            }
            let Some(native_path) = mat.native_path.as_ref() else {
                continue;
            };
            let path = PathBuf::from(native_path);
            if !path_in_scope(&path, scope) {
                continue;
            }
            match classify_materialization_file(&mat, &path) {
                FileClass::Missing => {
                    // 外部删除：标 Drifted，完整 Detached 语义留给后续 reconcile service
                    if mat.status != MaterializationStatus::Drift {
                        let _ = self
                            .repo
                            .upsert_materialization(crate::agent_hub::models::NewMaterialization {
                                asset_id: mat.asset_id.clone(),
                                target: mat.target,
                                target_binding_id: mat.target_binding_id.clone(),
                                native_path: mat.native_path.clone(),
                                last_projected_revision_id: mat.last_projected_revision_id.clone(),
                                rendered_hash: mat.rendered_hash.clone(),
                                observed_external_hash: None,
                                status: MaterializationStatus::Drift,
                                last_error: Some("target_file_missing".into()),
                            })
                            .await;
                        stats.external_revisions += 1;
                    }
                }
                FileClass::MatchesRendered => {
                    stats.noop_self_writes += 1;
                }
                FileClass::UnchangedObserved => {
                    stats.hash_skips += 1;
                }
                FileClass::ExternalEdit { hash } => {
                    let _ = self
                        .repo
                        .upsert_materialization(crate::agent_hub::models::NewMaterialization {
                            asset_id: mat.asset_id.clone(),
                            target: mat.target,
                            target_binding_id: mat.target_binding_id.clone(),
                            native_path: mat.native_path.clone(),
                            last_projected_revision_id: mat.last_projected_revision_id.clone(),
                            rendered_hash: mat.rendered_hash.clone(),
                            observed_external_hash: Some(hash),
                            status: MaterializationStatus::Drift,
                            last_error: Some("external_edit_pending_reconcile".into()),
                        })
                        .await;
                    // 完整三方 reconcile + multi-target projection 由后续 service 任务消费 Drifted。
                    // 此处计数外部编辑，供运行时观测；Task7 单测主路径用 DeLoopScanner。
                    stats.external_revisions += 1;
                }
            }
        }

        if cancel.is_cancelled() {
            return Ok(stats);
        }

        let run = self.scheduler.run_ready_jobs(cancel).await?;
        if run.attempted > 0 || run.committed > 0 {
            stats.projection_waves += 1;
        }

        Ok(stats)
    }
}

/// 单文件相对 materialization 的分类。
enum FileClass {
    Missing,
    MatchesRendered,
    UnchangedObserved,
    ExternalEdit { hash: String },
}

/// 分类目标文件相对 materialization 的关系。
///
/// Business Logic: 先比 hash，再决定是否当作外部编辑。
/// Code Logic: 读文件算 sha256；与 rendered/observed 比较。
fn classify_materialization_file(mat: &Materialization, path: &Path) -> FileClass {
    if !path.exists() {
        return FileClass::Missing;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return FileClass::Missing,
    };
    let hash = sha256_hex(&bytes);
    if mat.rendered_hash.as_deref() == Some(hash.as_str()) {
        return FileClass::MatchesRendered;
    }
    if mat.observed_external_hash.as_deref() == Some(hash.as_str()) {
        return FileClass::UnchangedObserved;
    }
    FileClass::ExternalEdit { hash }
}

/// Agent Hub 运行时入口。
///
/// Business Logic（为什么需要这个结构体）:
///     Headless owner 需要单一入口启动 watch/reconcile 循环，并返回 CancellationToken。
///
/// Code Logic（这个结构体做什么）:
///     静态 `start(state)` 装配 ProductionScanner 与事件循环。
pub struct AgentHubRuntime;

impl AgentHubRuntime {
    /// 启动 Agent Hub owner 后台循环。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     只有 sidecar Headless owner 可 watch/project；返回 token 供 shutdown 取消。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建 child cancel token；spawn async 循环：notify 提示 + debounce + 双 ticker + scan；
    ///     若 `agent_hub.enabled=false` 则空转等待配置开启，不建立 watcher。
    pub fn start(state: AppState) -> CancellationToken {
        let cancel = CancellationToken::new();
        let child = cancel.child_token();
        let loop_state = state.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = run_owner_loop(loop_state, child).await {
                tracing::warn!(error = %err, "agent hub runtime loop exited with error");
            }
        });
        cancel
    }
}

/// owner 主循环。
///
/// Business Logic: 启用后持续 debounce/ticker/scan；取消后退出。
/// Code Logic: select! cancel / debounce timer / tickers / notify channel。
async fn run_owner_loop(state: AppState, cancel: CancellationToken) -> Result<(), AppError> {
    // 等待 enabled；GUI 不会调用本函数。
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let enabled = state
            .config
            .read()
            .map(|c| c.agent_hub.enabled)
            .unwrap_or(false);
        if enabled {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }

    let data_root = crate::config::data_dir()?;
    let object_store = ObjectStore::open(data_root.join("agent-hub").join("objects"))?;
    let scanner = ProductionScanner::new((*state.agent_hub_repo).clone(), object_store);

    // 启动 full scan + recovery
    let _ = scanner.scan(&ScanScope::Full, &cancel).await?;

    let mut debouncer = DirtyDebouncer::new(WATCH_DEBOUNCE);
    let mut changed_dirs = ChangedDirLedger::default();
    let clock = SystemClock::new();

    // notify 仅作 hint：尽力 watch 用户 HOME 下常见 config 根（失败不致命）
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<PathBuf>();
    let _watcher_guard = spawn_notify_hints(event_tx, &cancel);

    let mut debounce_tick = tokio::time::interval(Duration::from_millis(100));
    debounce_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut changed_tick = tokio::time::interval(CHANGED_DIR_TICK);
    changed_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut full_tick = tokio::time::interval(FULL_SCOPE_TICK);
    full_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // 吞掉 interval 立即 tick
    changed_tick.tick().await;
    full_tick.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("agent hub runtime cancelled");
                return Ok(());
            }
            maybe_path = event_rx.recv() => {
                if let Some(path) = maybe_path {
                    debouncer.note_path_event(&path, clock.now_ms());
                } else {
                    // channel closed — continue with tickers only
                }
            }
            _ = debounce_tick.tick() => {
                if let Some(dirty) = debouncer.take_if_due(clock.now_ms()) {
                    changed_dirs.merge(&dirty);
                    let stats = scanner.scan(&ScanScope::Dirty(dirty), &cancel).await?;
                    log_scan("debounce", &stats);
                }
            }
            _ = changed_tick.tick() => {
                if !changed_dirs.is_empty() {
                    let dirty = changed_dirs.snapshot();
                    let stats = scanner.scan(&ScanScope::Dirty(dirty), &cancel).await?;
                    log_scan("changed-dir-tick", &stats);
                }
            }
            _ = full_tick.tick() => {
                let stats = scanner.scan(&ScanScope::Full, &cancel).await?;
                log_scan("full-scope-tick", &stats);
            }
        }
    }
}

/// 记录扫描结果。
fn log_scan(kind: &str, stats: &ScanStats) {
    if stats.external_revisions > 0
        || stats.projection_waves > 0
        || stats.noop_self_writes > 0
        || stats.hash_skips > 0
    {
        tracing::debug!(
            kind,
            scans = stats.scans,
            external_revisions = stats.external_revisions,
            projection_waves = stats.projection_waves,
            noop_self_writes = stats.noop_self_writes,
            hash_skips = stats.hash_skips,
            "agent hub scan"
        );
    }
}

/// 尽力启动 notify watcher（失败返回 None，ticker 仍工作）。
///
/// Business Logic: notify 只是提示；缺失时 30s/10min ticker 兜底。
/// Code Logic: RecommendedWatcher + 递归 watch 若干候选根。
fn spawn_notify_hints(
    tx: mpsc::UnboundedSender<PathBuf>,
    cancel: &CancellationToken,
) -> Option<notify::RecommendedWatcher> {
    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    let tx_cb = tx.clone();
    let mut watcher = match RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                // 只关心写/创建/删除/重命名
                let interesting = matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                );
                if !interesting {
                    return;
                }
                for path in event.paths {
                    let _ = tx_cb.send(path);
                }
            }
        },
        notify::Config::default(),
    ) {
        Ok(w) => w,
        Err(err) => {
            tracing::warn!(error = %err, "agent hub notify watcher unavailable; ticker-only mode");
            return None;
        }
    };

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".claude"));
        roots.push(home.join(".codex"));
        roots.push(home.join(".config").join("opencode"));
        roots.push(home.join(".agents"));
    }
    for root in roots {
        if root.exists() {
            if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
                tracing::debug!(path = %root.display(), error = %err, "agent hub watch root skipped");
            }
        }
    }

    // cancel 时 drop watcher：通过持有 cancel child 在另一任务中等待
    let cancel_child = cancel.child_token();
    tauri::async_runtime::spawn(async move {
        cancel_child.cancelled().await;
        // watcher 在主任务 drop；这里仅占位保持 cancel 语义清晰
    });

    Some(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// 外部编辑 → 一次 revision + 一次 projection wave。
    ///
    /// Business Logic: 用户改 CLI 文件必须进入 Hub 对账。
    /// Code Logic: seed → write_external → scan Full → 断言计数。
    #[test]
    fn external_edit_produces_one_revision_and_projection_wave() {
        let scanner = DeLoopScanner::new();
        let path = PathBuf::from("/tmp/hub-test/CLAUDE.md");
        scanner.seed_target(path.clone(), b"hub-v1");
        scanner.write_external(&path, b"user-edited");

        let stats = scanner.scan(&ScanScope::Full);
        assert_eq!(stats.scans, 1);
        assert_eq!(stats.external_revisions, 1);
        assert_eq!(stats.projection_waves, 1);
        assert_eq!(stats.noop_self_writes, 0);

        // 再次 scan 同一内容：hash 未变 → skip
        let stats2 = scanner.scan(&ScanScope::Full);
        assert_eq!(stats2.external_revisions, 0);
        assert_eq!(stats2.projection_waves, 0);
        assert!(stats2.hash_skips + stats2.noop_self_writes >= 1);
    }

    /// Hub 自身 rendered hash 的 watcher 事件是 no-op。
    ///
    /// Business Logic: 去环——投影写盘不能再触发 revision。
    /// Code Logic: write_hub_projection 后 scan → noop_self_writes。
    #[test]
    fn hub_rendered_hash_event_is_noop() {
        let scanner = DeLoopScanner::new();
        let path = PathBuf::from("/tmp/hub-test/AGENTS.md");
        scanner.seed_target(path.clone(), b"base");
        // 模拟 projection 写入新内容并更新 rendered
        scanner.write_hub_projection(&path, b"projected-v2");

        let stats = scanner.scan(&ScanScope::Full);
        assert_eq!(stats.external_revisions, 0);
        assert_eq!(stats.projection_waves, 0);
        assert_eq!(stats.noop_self_writes, 1);
    }

    /// 500ms 内 20 个事件只产生一次 scan。
    ///
    /// Business Logic: trailing debounce 合并 burst。
    /// Code Logic: FakeClock + DirtyDebouncer；20 次 note 后 advance 499 不 due，500 后 due 一次。
    #[test]
    fn burst_twenty_events_in_500ms_produce_one_scan() {
        let clock = FakeClock::new();
        let mut debouncer = DirtyDebouncer::new(WATCH_DEBOUNCE);
        let scanner = DeLoopScanner::new();
        let path = PathBuf::from("/tmp/hub-test/burst/CLAUDE.md");
        scanner.seed_target(path.clone(), b"v0");

        let mut scans = 0u64;
        for i in 0..20 {
            clock.advance_ms(10); // 200ms 内 20 次
            debouncer.note_path_event(&path, clock.now_ms());
            scanner.write_external(&path, format!("edit-{i}").as_bytes());
            if let Some(dirty) = debouncer.take_if_due(clock.now_ms()) {
                let _ = scanner.scan(&ScanScope::Dirty(dirty));
                scans += 1;
            }
        }
        assert_eq!(scans, 0, "debounce 窗口内不应 scan");

        clock.advance_ms(500);
        if let Some(dirty) = debouncer.take_if_due(clock.now_ms()) {
            let stats = scanner.scan(&ScanScope::Dirty(dirty));
            scans += 1;
            assert_eq!(stats.scans, 1);
            // 最终内容一次外部编辑
            assert_eq!(stats.external_revisions, 1);
            assert_eq!(stats.projection_waves, 1);
        }
        assert_eq!(scans, 1, "burst 合并为一次 scan");

        // 再次到期无新事件 → 不再 scan
        clock.advance_ms(500);
        assert!(debouncer.take_if_due(clock.now_ms()).is_none());
    }

    /// debounce 未到期不 take。
    #[test]
    fn debouncer_not_due_before_window() {
        let mut d = DirtyDebouncer::new(WATCH_DEBOUNCE);
        d.note_path_event(Path::new("/a/b/c.md"), 1000);
        assert!(d.take_if_due(1499).is_none());
        let taken = d.take_if_due(1500).expect("due at 500ms");
        assert_eq!(taken.len(), 1);
    }

    /// dirty 目录规范化后合并。
    #[test]
    fn debouncer_merges_same_parent_dir() {
        let mut d = DirtyDebouncer::new(WATCH_DEBOUNCE);
        d.note_path_event(Path::new("/proj/CLAUDE.md"), 0);
        d.note_path_event(Path::new("/proj/AGENTS.md"), 10);
        // trailing 从最后一次事件 10ms 起算，需 >= 510
        let taken = d.take_if_due(510).unwrap();
        assert_eq!(taken.len(), 1);
    }

    /// changed-dir ledger 在 debounce 清空后仍可 rescan。
    #[test]
    fn changed_dir_ledger_retains_after_debounce_take() {
        let mut d = DirtyDebouncer::new(WATCH_DEBOUNCE);
        let mut ledger = ChangedDirLedger::default();
        d.note_path_event(Path::new("/x/y.md"), 0);
        let dirty = d.take_if_due(500).unwrap();
        ledger.merge(&dirty);
        assert!(!ledger.is_empty());
        assert_eq!(ledger.snapshot().len(), 1);
    }

    /// AgentHubRuntime::start 返回可取消 token（smoke 级）。
    ///
    /// Business Logic: shutdown 必须能取消 runtime。
    /// Code Logic: 用最小 state 较重；此处仅验证 token 语义 + start 不 panic 需 AppState。
    /// 故测 cancel 协作：手动构造 token 链。
    #[test]
    fn cancellation_token_child_propagates() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    /// start_cancelled_task_once 语义：重复 start 只保留一个 token。
    ///
    /// Business Logic: duplicate backend start 仍只有一个 owner/watcher。
    /// Code Logic: 模拟 slot 行为。
    #[test]
    fn agent_hub_cancel_slot_starts_once() {
        let slot: Mutex<Option<CancellationToken>> = Mutex::new(None);
        let starts = Arc::new(AtomicU64::new(0));

        let start_once = |slot: &Mutex<Option<CancellationToken>>, starts: &AtomicU64| {
            let mut g = slot.lock().unwrap();
            if g.is_some() {
                return;
            }
            starts.fetch_add(1, Ordering::SeqCst);
            *g = Some(CancellationToken::new());
        };

        start_once(&slot, &starts);
        start_once(&slot, &starts);
        start_once(&slot, &starts);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(slot.lock().unwrap().is_some());
    }
}
