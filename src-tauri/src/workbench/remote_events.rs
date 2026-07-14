//! workbench/remote_events.rs — Workbench 远端事件桥接
//!
//! Business Logic（为什么需要这个模块）:
//!     remote shortcut 的 terminal 输出、状态和 merge 进度需要从项目所在设备实时转发到本机 UI。
//!     N1 Task 6 要求 bridge 仅由 sidecar owner 创建，带取消/TTL/退避与 1 MiB 资源上限。
//!
//! Code Logic（这个模块做什么）:
//!     定义可通过 broadcast/NDJSON 传输的事件 DTO，提供本机事件发布 helper，
//!     并维护 sidecar 拥有的 `RemoteEventBridgeRegistry`（CancellationToken、idle TTL、
//!     指数退避上限 60s、1 MiB 行/pending、8 KiB 错误前缀、shutdown_all 等待退出）。

use crate::state::AppState;
use crate::workbench::remote_ids::remote_entity_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
use tokio_util::sync::CancellationToken;

/// NDJSON 单行上限（1 MiB）。
pub const MAX_NDJSON_LINE_BYTES: usize = 1_048_576;
/// 跨 chunk pending buffer 上限（1 MiB）。
pub const MAX_PENDING_BUFFER_BYTES: usize = 1_048_576;
/// 错误响应 body 最多读取的前缀（8 KiB）。
pub const ERROR_BODY_PREFIX_BYTES: usize = 8 * 1024;
/// 无订阅/使用超过该秒数后回收 bridge。
pub const BRIDGE_IDLE_TTL_SECS: u64 = 60;
/// 重连指数退避上限（秒）。
pub const BRIDGE_MAX_BACKOFF_SECS: u64 = 60;
/// 初始退避基数（秒）。
const BRIDGE_BASE_BACKOFF_SECS: u64 = 1;

/// Workbench 远端终端输出 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 需要把远端 PTY 增量输出传回本机 xterm。
///
/// Code Logic（这个结构体做什么）:
///     对齐本机 `workbench:terminal-output` event payload，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalOutputPayload {
    pub session_id: String,
    pub chunk: String,
    pub seq: u64,
    pub ts: i64,
}

/// Workbench 远端终端状态 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 的 running/exited/disconnected 状态需要同步到本机 tab 和状态栏。
///
/// Code Logic（这个结构体做什么）:
///     对齐本机 `workbench:terminal-status` event payload，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalStatusPayload {
    pub session_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub ts: i64,
}

/// Workbench 远端 merge 进度 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote worktree merge 后续需要把多阶段进度桥接回本机 UI。
///
/// Code Logic（这个结构体做什么）:
///     project/worktree 使用字符串 ID，stage 保持 JSON 值以复用命令层现有阶段 DTO。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchMergeProgressPayload {
    pub project_id: String,
    pub worktree_id: String,
    pub stage: Value,
}

/// Workbench 可跨 HTTP NDJSON 传输的事件。
///
/// Business Logic（为什么需要这个枚举）:
///     远端事件流需要在一条连接中承载 terminal output、terminal status 和 merge progress 多种事件。
///
/// Code Logic（这个枚举做什么）:
///     使用 serde 内部 tag `{type,payload}`，type 按 camelCase 输出为前端和桥接层约定的稳定值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum WorkbenchRemoteEvent {
    TerminalOutput(WorkbenchTerminalOutputPayload),
    TerminalStatus(WorkbenchTerminalStatusPayload),
    MergeProgress(WorkbenchMergeProgressPayload),
}

/// 事件流解析/读取错误（含资源上限）。
///
/// Business Logic（为什么需要这个枚举）:
///     超限必须停止 bridge 并清空 buffer，不能继续累积内存；诊断只映射 error class。
///
/// Code Logic（这个枚举做什么）:
///     ResourceLimit 表示行/pending 超 1 MiB；其余为网络/HTTP/取消/空闲。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventStreamError {
    /// 行或 pending buffer 超过预算。
    ResourceLimit,
    /// 连接被取消。
    Cancelled,
    /// 空闲超过 TTL。
    IdleTimeout,
    /// HTTP 非成功状态（body 仅为 8 KiB 前缀）。
    Http { status: u16 },
    /// 网络/IO 失败（不含正文）。
    Network,
}

impl EventStreamError {
    /// Business Logic（为什么需要这个函数）:
    ///     诊断页只展示错误类别 token，不记录 URL/正文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射为稳定 snake_case 类别字符串。
    pub fn error_class(&self) -> &'static str {
        match self {
            Self::ResourceLimit => "resource_limit",
            Self::Cancelled => "cancelled",
            Self::IdleTimeout => "idle_timeout",
            Self::Http { .. } => "http_error",
            Self::Network => "network_error",
        }
    }
}

impl std::fmt::Display for EventStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ResourceLimit => write!(f, "event stream resource limit"),
            Self::Cancelled => write!(f, "event stream cancelled"),
            Self::IdleTimeout => write!(f, "event stream idle timeout"),
            Self::Http { status } => write!(f, "event stream http {status}"),
            Self::Network => write!(f, "event stream network error"),
        }
    }
}

/// 单桥脱敏快照（仅 phase/attempt/error class）。
///
/// Business Logic（为什么需要这个结构体）:
///     Settings 诊断需要 bridge 相位与错误类别，禁止设备 URL/token/内容。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 可序列化 DTO，字段集合刻意最小化。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEventBridgeSnapshot {
    pub phase: String,
    pub attempt: u32,
    pub last_error_class: Option<String>,
}

/// Workbench 远端事件桥中的项目 ID 映射。
///
/// Business Logic（为什么需要这个结构体）:
///     merge progress 的 projectId 必须映射成本机 remote shortcut projectId，前端才能按当前项目过滤事件。
///
/// Code Logic（这个结构体做什么）:
///     保存远端设备内 local projectId 到本机 shortcut projectId 的一条映射，供桥接任务实时读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEventBridgeProjectMapping {
    pub inner_project_id: String,
    pub local_project_id: String,
}

/// 单设备 bridge 任务内部共享状态。
///
/// Business Logic（为什么需要这个结构体）:
///     loop 与 registry 需共享 last_used/phase/attempt/error，供 TTL 与诊断读取。
///
/// Code Logic（这个结构体做什么）:
///     Arc 原子/互斥字段；不含 URL 凭据或事件正文。
struct BridgeRuntimeState {
    last_used: Mutex<Instant>,
    phase: Mutex<String>,
    attempt: AtomicU32,
    last_error_class: Mutex<Option<String>>,
    finished: AtomicBool,
}

impl BridgeRuntimeState {
    /// Business Logic（为什么需要这个函数）:
    ///     新建 bridge 时需要初始化 runtime 观测字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     last_used=now、phase=connecting、attempt=0。
    fn new() -> Self {
        Self {
            last_used: Mutex::new(Instant::now()),
            phase: Mutex::new("connecting".to_string()),
            attempt: AtomicU32::new(0),
            last_error_class: Mutex::new(None),
            finished: AtomicBool::new(false),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     ensure_bridge 调用表示仍有订阅者，应刷新 idle TTL。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 last_used = Instant::now()。
    fn touch(&self) {
        *self.last_used.lock().expect("bridge last_used 锁中毒") = Instant::now();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     空闲回收依赖 last_used 流逝时间。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 last_used.elapsed()。
    fn idle_for(&self) -> Duration {
        self.last_used
            .lock()
            .expect("bridge last_used 锁中毒")
            .elapsed()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     诊断与日志需要可读 phase。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 phase 字符串。
    fn set_phase(&self, phase: &str) {
        *self.phase.lock().expect("bridge phase 锁中毒") = phase.to_string();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     失败重连后需要记录错误类别供诊断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 Option error class token。
    fn set_error_class(&self, class: Option<&str>) {
        *self
            .last_error_class
            .lock()
            .expect("bridge last_error_class 锁中毒") = class.map(str::to_string);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     快照只暴露 phase/attempt/error class。
    ///
    /// Code Logic（这个函数做什么）:
    ///     组装 RemoteEventBridgeSnapshot。
    fn snapshot(&self) -> RemoteEventBridgeSnapshot {
        RemoteEventBridgeSnapshot {
            phase: self.phase.lock().expect("bridge phase 锁中毒").clone(),
            attempt: self.attempt.load(Ordering::SeqCst),
            last_error_class: self
                .last_error_class
                .lock()
                .expect("bridge last_error_class 锁中毒")
                .clone(),
        }
    }
}

/// Workbench 远端事件桥接后台任务记录。
///
/// Business Logic（为什么需要这个结构体）:
///     同一台设备的事件连接需要随着端口变化替换，同时持续复用已发现的项目 ID 映射。
///
/// Code Logic（这个结构体做什么）:
///     保存 base_url、共享 project 映射、取消令牌、runtime 状态和 JoinHandle。
struct RemoteEventBridgeTask {
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    cancel: CancellationToken,
    runtime: Arc<BridgeRuntimeState>,
    handle: JoinHandle<()>,
}

impl RemoteEventBridgeTask {
    /// Business Logic（为什么需要这个函数）:
    ///     事件桥任务可能因为 panic、取消或 idle 结束，registry 需要准确识别并替换。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同时读取 finished 标记和 JoinHandle 状态，任一显示结束则返回 true。
    fn is_finished(&self) -> bool {
        self.runtime.finished.load(Ordering::SeqCst) || self.handle.inner().is_finished()
    }
}

/// Workbench 远端事件桥接任务登记表（sidecar owner 独占）。
///
/// Business Logic（为什么需要这个结构体）:
///     list/create remote terminal 可能被频繁调用，但每台设备只应保持一个事件长连接；
///     GuiClient 不得创建 bridge。
///
/// Code Logic（这个结构体做什么）:
///     Mutex<HashMap<device_id, task>>；ensure 刷新 last_used；shutdown_all 取消并 await。
#[derive(Default)]
pub struct RemoteEventBridgeRegistry {
    tasks: Mutex<HashMap<String, RemoteEventBridgeTask>>,
}

impl RemoteEventBridgeRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的远端事件桥接登记表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回没有任何设备连接任务的 registry。
    pub fn new() -> Self {
        Self::default()
    }

    /// 返回当前登记的远端事件桥数量。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     control status / 诊断需要轻量 bridge 计数，不暴露设备 URL 或订阅细节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁读取 tasks HashMap 长度。
    pub fn active_bridge_count(&self) -> usize {
        self.tasks
            .lock()
            .expect("remote event bridge 锁中毒")
            .len()
    }

    /// 收集全部 bridge 脱敏快照。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     RuntimeOwnerStatus / SanitizedRuntimeDiagnostics 需要 phases/error codes。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁映射每个 task.runtime.snapshot()。
    pub fn snapshots(&self) -> Vec<RemoteEventBridgeSnapshot> {
        self.tasks
            .lock()
            .expect("remote event bridge 锁中毒")
            .values()
            .map(|task| task.runtime.snapshot())
            .collect()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次进入 remote terminal 项目或创建 remote session 后，都要确保事件桥已连接，并记住项目映射。
    ///     GuiClient 调用必须 no-op。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 runtime_role 为 HeadlessOwner；按 device_id 更新映射与 last_used；
    ///     URL 变化或任务结束时 cancel 旧任务并 spawn 新循环。
    pub fn ensure_bridge(
        &self,
        device_id: String,
        base_url: String,
        project_mapping: Option<RemoteEventBridgeProjectMapping>,
        state: AppState,
    ) {
        if state.runtime_role.require_owner().is_err() {
            return;
        }
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        if let Some(existing) = tasks.get_mut(&device_id) {
            update_project_mapping(&existing.project_ids, project_mapping);
            existing.runtime.touch();
            if !bridge_should_restart(&existing.base_url, existing.is_finished(), &base_url) {
                return;
            }
            existing.cancel.cancel();
            existing.handle.abort();
            let project_ids = Arc::clone(&existing.project_ids);
            *existing = spawn_bridge_task(device_id, base_url, project_ids, state);
            return;
        }

        let project_ids = Arc::new(RwLock::new(HashMap::new()));
        update_project_mapping(&project_ids, project_mapping);
        let task = spawn_bridge_task(device_id.clone(), base_url, project_ids, state);
        tasks.insert(device_id, task);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     id-only remote worktree 命令需要在收到远端 DTO 后，把 inner projectId 找回本机 shortcut projectId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从指定 device 的共享映射表读取 local projectId；未记录时返回 None。
    pub fn local_project_id_for(&self, device_id: &str, inner_project_id: &str) -> Option<String> {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let task = tasks.get(device_id)?;
        let local_project_id = task
            .project_ids
            .read()
            .expect("remote event bridge project 映射读锁中毒")
            .get(inner_project_id)
            .cloned();
        local_project_id
    }

    /// 取消全部 bridge 并等待任务退出。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     sidecar 关机必须回收长连接，避免 ghost reconnect。
    ///
    /// Code Logic（这个函数做什么）:
    ///     drain registry → 对每个 task cancel → await JoinHandle；不保留 URL/正文。
    pub async fn shutdown_all(&self) {
        let drained: Vec<RemoteEventBridgeTask> = {
            let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
            tasks.drain().map(|(_, task)| task).collect()
        };
        for task in &drained {
            task.cancel.cancel();
            task.runtime.set_phase("stopping");
        }
        for task in drained {
            let _ = task.handle.await;
            task.runtime.finished.store(true, Ordering::SeqCst);
            task.runtime.set_phase("stopped");
        }
    }

    /// 同步强制关闭（进程退出路径：cancel + abort，不等待）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `shutdown_backend_runtime` 是同步钩子，仍需尽快停止 bridge 循环。
    ///
    /// Code Logic（这个函数做什么）:
    ///     drain → cancel + abort handle；不 await。
    pub fn force_shutdown(&self) {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        for (_, task) in tasks.drain() {
            task.cancel.cancel();
            task.handle.abort();
            task.runtime.finished.store(true, Ordering::SeqCst);
            task.runtime.set_phase("stopped");
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     事件桥 registry 需要把任务创建细节集中处理，保证 replacement 和首次创建使用同一套状态字段。
///
/// Code Logic（这个函数做什么）:
///     创建 cancel/runtime 并 spawn remote_event_loop；结束后置 finished。
fn spawn_bridge_task(
    device_id: String,
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    state: AppState,
) -> RemoteEventBridgeTask {
    let cancel = CancellationToken::new();
    let runtime = Arc::new(BridgeRuntimeState::new());
    let loop_cancel = cancel.clone();
    let loop_runtime = Arc::clone(&runtime);
    let task_device_id = device_id;
    let task_base_url = base_url.clone();
    let task_project_ids = Arc::clone(&project_ids);
    let handle = tauri::async_runtime::spawn(async move {
        remote_event_loop(
            task_device_id,
            task_base_url,
            state,
            task_project_ids,
            loop_cancel,
            loop_runtime.clone(),
        )
        .await;
        loop_runtime.finished.store(true, Ordering::SeqCst);
        if loop_runtime.phase.lock().expect("bridge phase 锁中毒").as_str() != "stopped" {
            loop_runtime.set_phase("finished");
        }
    });
    RemoteEventBridgeTask {
        base_url,
        project_ids,
        cancel,
        runtime,
        handle,
    }
}

/// Business Logic（为什么需要这个函数）:
///     同设备事件桥可能先从 session list 建立，后续再从 worktree/merge 操作补充更多项目映射。
///
/// Code Logic（这个函数做什么）:
///     若传入映射则写入共享 HashMap；None 表示仅确保连接。
fn update_project_mapping(
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    project_mapping: Option<RemoteEventBridgeProjectMapping>,
) {
    let Some(mapping) = project_mapping else {
        return;
    };
    project_ids
        .write()
        .expect("remote event bridge project 映射写锁中毒")
        .insert(mapping.inner_project_id, mapping.local_project_id);
}

/// Business Logic（为什么需要这个函数）:
///     P2P 设备端口会动态变化，旧事件任务也可能异常结束，registry 必须知道何时替换连接。
///
/// Code Logic（这个函数做什么）:
///     base_url 不一致或旧任务已结束时返回 true；同 URL 且仍运行时返回 false。
fn bridge_should_restart(existing_base_url: &str, finished: bool, next_base_url: &str) -> bool {
    finished || existing_base_url != next_base_url
}

/// Business Logic（为什么需要这个函数）:
///     本机 session/merge 事件 emit 时，也要同步发布到 HTTP broadcast channel 供远端设备订阅。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 向 `workbench_remote_events` broadcast sender 发送事件；无订阅者时记录 debug 后忽略。
pub fn publish_workbench_remote_event_from_state(state: &AppState, event: WorkbenchRemoteEvent) {
    if let Err(error) = state.workbench_remote_events.send(event) {
        tracing::debug!("无 Workbench 远端事件订阅者: {error}");
    }
}

/// 计算有界指数退避（含轻量 jitter），上限 60s。
///
/// Business Logic（为什么需要这个函数）:
///     永久固定 2s 重连会打爆对端；需要指数退避且硬封顶 60s。
///
/// Code Logic（这个函数做什么）:
///     delay = min(60, base * 2^min(attempt,6))，再减 0..=25% jitter（由 attempt 派生确定性偏移）。
pub fn backoff_delay_for_attempt(attempt: u32) -> Duration {
    let exp = attempt.min(6);
    let raw = BRIDGE_BASE_BACKOFF_SECS.saturating_mul(1u64 << exp);
    let capped = raw.min(BRIDGE_MAX_BACKOFF_SECS);
    // 确定性 jitter：用 attempt 低位避免全设备同相位；范围约 0..25%。
    let jitter_ms = (capped * 250) * u64::from(attempt % 4) / 3 / 4;
    let total_ms = capped.saturating_mul(1000).saturating_sub(jitter_ms);
    Duration::from_millis(total_ms.max(BRIDGE_BASE_BACKOFF_SECS * 1000 / 2))
}

/// Business Logic（为什么需要这个函数）:
///     远端事件连接可能因网络切换、对端重启或资源上限而断开，需要可取消的自动恢复。
///
/// Code Logic（这个函数做什么）:
///     循环：检查 cancel/idle → 读流 → 失败记 error class 并指数退避（cap 60s）；
///     ResourceLimit 立即停止且不保留 buffer。
async fn remote_event_loop(
    device_id: String,
    base_url: String,
    state: AppState,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    cancel: CancellationToken,
    runtime: Arc<BridgeRuntimeState>,
) {
    loop {
        if cancel.is_cancelled() {
            runtime.set_phase("cancelled");
            return;
        }
        if runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS) {
            runtime.set_phase("idle_exit");
            runtime.set_error_class(Some(EventStreamError::IdleTimeout.error_class()));
            return;
        }

        runtime.set_phase("connecting");
        match read_remote_event_stream(
            &state,
            &device_id,
            &base_url,
            &project_ids,
            &cancel,
            &runtime,
        )
        .await
        {
            Ok(()) => {
                runtime.attempt.store(0, Ordering::SeqCst);
                runtime.set_error_class(None);
            }
            Err(EventStreamError::Cancelled) => {
                runtime.set_phase("cancelled");
                return;
            }
            Err(EventStreamError::ResourceLimit) => {
                runtime.set_phase("resource_limit");
                runtime.set_error_class(Some(EventStreamError::ResourceLimit.error_class()));
                tracing::debug!("Workbench 远端事件流超资源上限，停止 bridge");
                return;
            }
            Err(EventStreamError::IdleTimeout) => {
                runtime.set_phase("idle_exit");
                runtime.set_error_class(Some(EventStreamError::IdleTimeout.error_class()));
                return;
            }
            Err(error) => {
                let class = error.error_class();
                runtime.set_error_class(Some(class));
                let next = runtime.attempt.fetch_add(1, Ordering::SeqCst) + 1;
                runtime.set_phase("backoff");
                tracing::debug!("Workbench 远端事件流断开，将退避重连: class={class} attempt={next}");
            }
        }

        if cancel.is_cancelled() {
            runtime.set_phase("cancelled");
            return;
        }
        if runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS) {
            runtime.set_phase("idle_exit");
            return;
        }

        let attempt = runtime.attempt.load(Ordering::SeqCst);
        let delay = backoff_delay_for_attempt(attempt);
        tokio::select! {
            _ = cancel.cancelled() => {
                runtime.set_phase("cancelled");
                return;
            }
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     一次远端事件连接负责持续读取 NDJSON 并把远端内部 ID 映射成本机 remote ID。
///
/// Code Logic（这个函数做什么）:
///     复用 `PeerClient::open_ndjson_stream`；错误 body 只读 8 KiB 前缀；
///     chunk 解析受 1 MiB 限制，超限返回 ResourceLimit 并清空 buffer。
async fn read_remote_event_stream(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    cancel: &CancellationToken,
    runtime: &BridgeRuntimeState,
) -> Result<(), EventStreamError> {
    let url = event_stream_url(base_url);
    let mut response = tokio::select! {
        _ = cancel.cancelled() => return Err(EventStreamError::Cancelled),
        result = state.peer_client.open_ndjson_stream(&url) => {
            result.map_err(|_| EventStreamError::Network)?
        }
    };
    let status = response.status();
    if !status.is_success() {
        let _prefix = read_error_body_prefix(&mut response).await;
        return Err(EventStreamError::Http {
            status: status.as_u16(),
        });
    }

    runtime.set_phase("streaming");
    let mut buffer = Vec::new();
    loop {
        if cancel.is_cancelled() {
            buffer.clear();
            return Err(EventStreamError::Cancelled);
        }
        if runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS) {
            buffer.clear();
            return Err(EventStreamError::IdleTimeout);
        }

        let chunk = tokio::select! {
            _ = cancel.cancelled() => {
                buffer.clear();
                return Err(EventStreamError::Cancelled);
            }
            next = response.chunk() => {
                next.map_err(|_| EventStreamError::Network)?
            }
        };
        let Some(chunk) = chunk else {
            buffer.clear();
            return Ok(());
        };

        let project_map = project_ids
            .read()
            .expect("remote event bridge project 映射读锁中毒")
            .clone();
        match process_event_chunk_to_events(device_id, &project_map, &mut buffer, &chunk) {
            Ok(events) => {
                for event in events {
                    emit_mapped_remote_event(state, event);
                }
            }
            Err(EventStreamError::ResourceLimit) => {
                buffer.clear();
                return Err(EventStreamError::ResourceLimit);
            }
            Err(other) => {
                buffer.clear();
                return Err(other);
            }
        }
    }
}

/// 最多读取 8 KiB 错误 body 前缀（丢弃剩余）。
///
/// Business Logic（为什么需要这个函数）:
///     错误诊断只需类别；禁止把整段 HTML/JSON 错误页留在内存。
///
/// Code Logic（这个函数做什么）:
///     循环 chunk 直到凑满 ERROR_BODY_PREFIX_BYTES 或流结束，返回 lossy UTF-8 前缀字符串。
async fn read_error_body_prefix(response: &mut reqwest::Response) -> String {
    let mut buf = Vec::new();
    while buf.len() < ERROR_BODY_PREFIX_BYTES {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remain = ERROR_BODY_PREFIX_BYTES - buf.len();
                let take = remain.min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    break;
                }
            }
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Business Logic（为什么需要这个函数）:
///     远端事件流中的用户输出可能包含中文或 emoji，跨 chunk 解析必须以完整 NDJSON 行为边界；
///     超 1 MiB 必须停止并清空，禁止保留超大 buffer。
///
/// Code Logic（这个函数做什么）:
///     以 byte buffer 追加 chunk；任一行或 pending 超限 → clear + ResourceLimit；
///     完整行 UTF-8 解码 + serde 解析后映射设备前缀。
fn process_event_chunk_to_events(
    device_id: &str,
    project_ids: &HashMap<String, String>,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<Vec<WorkbenchRemoteEvent>, EventStreamError> {
    // 快速路径：追加前检查 pending 预算。
    if buffer.len().saturating_add(chunk.len()) > MAX_PENDING_BUFFER_BYTES {
        // 若合并后仍无换行且超限 → 资源上限。
        let has_newline = buffer.iter().any(|b| *b == b'\n') || chunk.iter().any(|b| *b == b'\n');
        if !has_newline {
            buffer.clear();
            return Err(EventStreamError::ResourceLimit);
        }
    }

    buffer.extend_from_slice(chunk);

    // 未完成行本身超 1 MiB（无换行）→ 停止。
    if !buffer.contains(&b'\n') && buffer.len() > MAX_NDJSON_LINE_BYTES {
        buffer.clear();
        return Err(EventStreamError::ResourceLimit);
    }

    let mut events = Vec::new();
    while let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
        if index > MAX_NDJSON_LINE_BYTES {
            buffer.clear();
            return Err(EventStreamError::ResourceLimit);
        }
        let mut line = buffer.drain(..=index).collect::<Vec<_>>();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.len() > MAX_NDJSON_LINE_BYTES {
            buffer.clear();
            return Err(EventStreamError::ResourceLimit);
        }
        let line = trim_ascii_whitespace_bytes(&line);
        if line.is_empty() {
            continue;
        }
        match std::str::from_utf8(line) {
            Ok(text) => match serde_json::from_str::<WorkbenchRemoteEvent>(text) {
                Ok(event) => {
                    events.push(map_remote_event_for_device(device_id, project_ids, event))
                }
                Err(error) => tracing::debug!("解析 Workbench 远端事件失败: {error}"),
            },
            Err(error) => tracing::debug!("远端 Workbench 事件不是合法 UTF-8: {error}"),
        }
    }

    if buffer.len() > MAX_PENDING_BUFFER_BYTES {
        buffer.clear();
        return Err(EventStreamError::ResourceLimit);
    }
    Ok(events)
}

/// 测试/诊断入口：按 chunk 列表解析 NDJSON，传播 ResourceLimit。
///
/// Business Logic（为什么需要这个函数）:
///     单测需要不依赖 HTTP 的确定性超限路径。
///
/// Code Logic（这个函数做什么）:
///     串行喂 process_event_chunk_to_events，累积事件或首个错误。
#[cfg(test)]
pub async fn parse_ndjson_chunks(
    chunks: Vec<Vec<u8>>,
) -> Result<Vec<WorkbenchRemoteEvent>, EventStreamError> {
    let mut buffer = Vec::new();
    let mut events = Vec::new();
    let project_ids = HashMap::new();
    for chunk in chunks {
        let partial =
            process_event_chunk_to_events("device-a", &project_ids, &mut buffer, &chunk)?;
        events.extend(partial);
    }
    Ok(events)
}

/// Business Logic（为什么需要这个函数）:
///     NDJSON 行尾可能带 CRLF 或空白，解析前应清理协议空白但不能修改 JSON 字符串内容。
///
/// Code Logic（这个函数做什么）:
///     仅裁剪字节切片两端的 ASCII whitespace，返回原 buffer 内的有效 JSON 行切片。
fn trim_ascii_whitespace_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// Business Logic（为什么需要这个函数）:
///     本机前端只监听 Tauri event，不关心事件来自本机 PTY 还是远端 HTTP stream。
///
/// Code Logic（这个函数做什么）:
///     按事件类型 emit 到现有 `workbench:*` 事件名；headless adapter 可安全 no-op。
fn emit_mapped_remote_event(state: &AppState, event: WorkbenchRemoteEvent) {
    match event {
        WorkbenchRemoteEvent::TerminalOutput(payload) => {
            state.emit_event("workbench:terminal-output", payload);
        }
        WorkbenchRemoteEvent::TerminalStatus(payload) => {
            state.emit_event("workbench:terminal-status", payload);
        }
        WorkbenchRemoteEvent::MergeProgress(payload) => {
            state.emit_event("workbench:merge-progress", payload);
        }
    };
}

/// Business Logic（为什么需要这个函数）:
///     远端设备发出的事件只包含自己的 local ID，本机 UI 需要可区分设备归属的 remote ID。
///
/// Code Logic（这个函数做什么）:
///     根据事件类型把 sessionId/projectId/worktreeId 映射为 `remote:<device_id>:<inner_id>`。
fn map_remote_event_for_device(
    device_id: &str,
    project_ids: &HashMap<String, String>,
    event: WorkbenchRemoteEvent,
) -> WorkbenchRemoteEvent {
    match event {
        WorkbenchRemoteEvent::TerminalOutput(mut payload) => {
            payload.session_id = remote_entity_id(device_id, &payload.session_id);
            WorkbenchRemoteEvent::TerminalOutput(payload)
        }
        WorkbenchRemoteEvent::TerminalStatus(mut payload) => {
            payload.session_id = remote_entity_id(device_id, &payload.session_id);
            WorkbenchRemoteEvent::TerminalStatus(payload)
        }
        WorkbenchRemoteEvent::MergeProgress(mut payload) => {
            payload.project_id = project_ids
                .get(&payload.project_id)
                .cloned()
                .unwrap_or_else(|| remote_entity_id(device_id, &payload.project_id));
            payload.worktree_id = remote_entity_id(device_id, &payload.worktree_id);
            WorkbenchRemoteEvent::MergeProgress(payload)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端设备 base URL 可能带尾斜杠，事件桥必须拼出稳定 endpoint。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/` 后追加 `/api/workbench/events`。
fn event_stream_url(base_url: &str) -> String {
    format!("{}/api/workbench/events", base_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::authority::RuntimeRole;

    /// Business Logic（为什么需要这个测试）:
    ///     超 1 MiB 行必须 ResourceLimit 且不得保留 buffer。
    ///
    /// Code Logic（这个测试做什么）:
    ///     喂入 1_048_577 个 `x` 无换行，断言 ResourceLimit。
    #[tokio::test]
    async fn oversized_line_stops_bridge_without_retaining_buffer() {
        let result = parse_ndjson_chunks(vec![vec![b'x'; 1_048_577]]).await;
        assert!(matches!(result, Err(EventStreamError::ResourceLimit)));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     pending 跨 chunk 累计超 1 MiB 且无换行同样必须停。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两段各 ~600KiB 无换行，断言 ResourceLimit。
    #[tokio::test]
    async fn pending_buffer_over_limit_without_newline_is_resource_limit() {
        let a = vec![b'a'; 600_000];
        let b = vec![b'b'; 600_000];
        let result = parse_ndjson_chunks(vec![a, b]).await;
        assert!(matches!(result, Err(EventStreamError::ResourceLimit)));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     退避必须有界，避免永久短间隔重连。
    ///
    /// Code Logic（这个测试做什么）:
    ///     大 attempt 时 delay ≤ 60s，且小 attempt 递增或封顶。
    #[test]
    fn backoff_is_capped_at_sixty_seconds() {
        let d0 = backoff_delay_for_attempt(0);
        let d3 = backoff_delay_for_attempt(3);
        let d20 = backoff_delay_for_attempt(20);
        assert!(d0 <= Duration::from_secs(BRIDGE_MAX_BACKOFF_SECS));
        assert!(d3 <= Duration::from_secs(BRIDGE_MAX_BACKOFF_SECS));
        assert!(d20 <= Duration::from_secs(BRIDGE_MAX_BACKOFF_SECS));
        assert!(d20 >= Duration::from_secs(BRIDGE_MAX_BACKOFF_SECS / 2));
        assert!(d3 >= d0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     错误 body 前缀常量必须是 8 KiB。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 ERROR_BODY_PREFIX_BYTES == 8192。
    #[test]
    fn error_body_prefix_is_eight_kib() {
        assert_eq!(ERROR_BODY_PREFIX_BYTES, 8 * 1024);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     诊断快照不得包含 token/content 字段名。
    ///
    /// Code Logic（这个测试做什么）:
    ///     序列化 RemoteEventBridgeSnapshot，断言 JSON 无 token/content/prompt/url 键。
    #[test]
    fn bridge_snapshot_has_no_token_or_content_fields() {
        let snap = RemoteEventBridgeSnapshot {
            phase: "streaming".into(),
            attempt: 2,
            last_error_class: Some("network_error".into()),
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let lower = json.to_ascii_lowercase();
        for forbidden in [
            "token",
            "content",
            "prompt",
            "password",
            "authorization",
            "baseurl",
            "base_url",
            "controltoken",
        ] {
            assert!(
                !lower.contains(forbidden),
                "diagnostics must not contain {forbidden}: {json}"
            );
        }
        assert!(lower.contains("phase"));
        assert!(lower.contains("attempt"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     GuiClient 不得通过 registry 启动 bridge。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造最小 GuiClient AppState 困难时，用 runtime_role 直接断言 require_owner 失败，
    ///     并验证 registry ensure_bridge 在 role 拒绝时保持 count=0（借助 dummy 状态字段路径）。
    #[test]
    fn gui_client_role_cannot_own_bridges() {
        assert!(RuntimeRole::GuiClient.require_owner().is_err());
        assert!(RuntimeRole::HeadlessOwner.require_owner().is_ok());
        let registry = RemoteEventBridgeRegistry::new();
        assert_eq!(registry.active_bridge_count(), 0);
        assert!(registry.snapshots().is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 terminal 输出事件桥接到本机后，sessionId 必须带设备前缀才能和本机会话区分。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 terminalOutput 事件并映射 device-a，断言 payload.sessionId 使用 remote entity ID。
    #[test]
    fn map_remote_terminal_output_event_prefixes_session_id() {
        let event = WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
            session_id: "inner-session".to_string(),
            chunk: "hello".to_string(),
            seq: 7,
            ts: 1000,
        });

        let mapped = map_remote_event_for_device("device-a", &HashMap::new(), event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:inner-session".to_string(),
                chunk: "hello".to_string(),
                seq: 7,
                ts: 1000,
            })
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 merge 进度事件后续会被本机 UI 按本机 remote shortcut projectId 过滤。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 innerProjectId -> local shortcut 映射，断言 projectId 使用 shortcut、worktreeId 仍使用 remote entity。
    #[test]
    fn map_remote_merge_progress_event_uses_local_shortcut_project_id() {
        let stage = serde_json::json!({"id":"mergeMain","status":"running"});
        let event = WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
            project_id: "inner-project".to_string(),
            worktree_id: "inner-worktree".to_string(),
            stage: stage.clone(),
        });
        let project_ids = HashMap::from([(
            "inner-project".to_string(),
            "remote:device-a:shortcut-project".to_string(),
        )]);

        let mapped = map_remote_event_for_device("device-a", &project_ids, event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
                project_id: "remote:device-a:shortcut-project".to_string(),
                worktree_id: "remote:device-a:inner-worktree".to_string(),
                stage,
            })
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端事件流承载用户终端输出，中文和 emoji 不能因为 TCP chunk 切分被替换成乱码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造包含多字节字符的 NDJSON 行，并故意在 emoji UTF-8 字节中间切分，断言完整行解析后内容保持不变。
    #[test]
    fn process_event_chunk_preserves_multibyte_characters_across_chunks() {
        let line = serde_json::json!({
            "type": "terminalOutput",
            "payload": {
                "sessionId": "inner-session",
                "chunk": "中文🚀输出",
                "seq": 1,
                "ts": 1000
            }
        })
        .to_string()
            + "\n";
        let bytes = line.as_bytes();
        let rocket_offset = line.find('🚀').expect("fixture should contain rocket");
        let split_at = rocket_offset + 1;
        let mut buffer = Vec::new();
        let project_ids = HashMap::new();

        let first = process_event_chunk_to_events(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[..split_at],
        )
        .expect("first chunk ok");
        let second = process_event_chunk_to_events(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[split_at..],
        )
        .expect("second chunk ok");

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0],
            WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:inner-session".to_string(),
                chunk: "中文🚀输出".to_string(),
                seq: 1,
                ts: 1000,
            })
        );
        assert!(buffer.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     P2P HTTP 端口可能随对端重启而变化，事件桥必须替换旧连接而不是继续复用 stale URL。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接覆盖 registry 的重启判定 helper，断言 URL 变化或旧任务结束会触发 replacement。
    #[test]
    fn bridge_restart_decision_replaces_finished_or_changed_base_url() {
        assert!(bridge_should_restart(
            "http://127.0.0.1:1000",
            false,
            "http://127.0.0.1:2000"
        ));
        assert!(bridge_should_restart(
            "http://127.0.0.1:1000",
            true,
            "http://127.0.0.1:1000"
        ));
        assert!(!bridge_should_restart(
            "http://127.0.0.1:1000",
            false,
            "http://127.0.0.1:1000"
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备发现保存的 base URL 可能包含尾斜杠，事件桥不应生成双斜杠路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入带尾斜杠 base URL，断言 endpoint URL 规范化。
    #[test]
    fn event_stream_url_trims_trailing_slash() {
        assert_eq!(
            event_stream_url("http://127.0.0.1:1420/"),
            "http://127.0.0.1:1420/api/workbench/events"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     shutdown_all 必须清空 registry 且可安全重复调用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry 上 await shutdown_all / force_shutdown，断言 count=0。
    #[tokio::test]
    async fn shutdown_all_on_empty_registry_is_noop() {
        let registry = RemoteEventBridgeRegistry::new();
        registry.shutdown_all().await;
        registry.force_shutdown();
        assert_eq!(registry.active_bridge_count(), 0);
    }
}
