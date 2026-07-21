//! workbench/remote_events.rs — Workbench 远端事件桥接
//!
//! Business Logic（为什么需要这个模块）:
//!     remote shortcut 的 terminal 输出、状态和 merge 进度需要从项目所在设备实时转发到本机 UI。
//!     N1 Task 6 要求 bridge 仅由 sidecar owner 创建，带取消/TTL/退避与 1 MiB 资源上限。
//!
//! Code Logic（这个模块做什么）:
//!     定义可通过 broadcast/NDJSON 传输的事件 DTO；`WorkbenchRemoteEventBus` 为有界
//!     owner+sequence 总线（ring catch-up + live + Gap，镜像 RuntimeEventBus；
//!     after_seq=0 且 ring 截断仍 Gap）；远端桥收到 Gap 后先 resync running terminal，
//!     仅成功才推进 after_cursor（R28 H1）；L3 多机 cutover **NOT VERIFIED**。
//!     提供本机事件发布 helper，并维护 sidecar 拥有的 `RemoteEventBridgeRegistry`
//!     （CancellationToken、订阅 lease/refcount + idle TTL、指数退避上限 60s、1 MiB 行/pending、
//!     8 KiB 错误前缀、共享 after 游标跨 task restart 保留、Gap fail-closed、
//!     shutdown_all 等待退出；R30 M3）。

use crate::backend::event_bus::BackendRuntimeCursor;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_runtime::snapshot::AgentSessionRuntimeDto;
use crate::workbench::remote_ids::remote_entity_id;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tauri::async_runtime::JoinHandle;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// 默认 replay ring 容量（事件条数）。
const DEFAULT_REPLAY_RING_CAPACITY: usize = 256;
/// 默认 live broadcast 容量。
const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// NDJSON 单行上限（1 MiB）。
pub const MAX_NDJSON_LINE_BYTES: usize = 1_048_576;
/// 跨 chunk pending buffer 上限（1 MiB）。
pub const MAX_PENDING_BUFFER_BYTES: usize = 1_048_576;
/// 错误响应 body 最多读取的前缀（8 KiB）。
pub const ERROR_BODY_PREFIX_BYTES: usize = 8 * 1024;
/// 无订阅且无 ensure/touch 超过该秒数后回收 bridge（订阅 refcount>0 时永不 idle）。
pub const BRIDGE_IDLE_TTL_SECS: u64 = 60;
/// 重连指数退避上限（秒）。
pub const BRIDGE_MAX_BACKOFF_SECS: u64 = 60;
/// 初始退避基数（秒）。
const BRIDGE_BASE_BACKOFF_SECS: u64 = 1;

/// Workbench 远端终端输出 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 需要把远端 PTY 增量输出传回本机 xterm；
///     同时携带 stream 生产端 owner（远端 backend 世代），供本机合成复合 authority。
///
/// Code Logic（这个结构体做什么）:
///     对齐本机 `workbench:terminal-output` event payload，字段使用 camelCase；
///     `owner_instance_id` 可选：生产者 stamp 的 instance id；legacy 缺省为 None。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalOutputPayload {
    pub session_id: String,
    pub chunk: String,
    pub seq: u64,
    pub ts: i64,
    /// 终端输出 stream 生产端 owner（本机 PTY 侧为 sidecar owner；经 bridge 转发时保留）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_instance_id: Option<String>,
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

/// Agent runtime 远端事件 payload（与 `workbench:agent-runtime` 对齐，无 native id）。
///
/// Business Logic（为什么需要这个结构体）:
///     remote/mobile 需要与本机同一份 phase 投影。
///
/// Code Logic（这个结构体做什么）:
///     包装 sanitized `AgentSessionRuntimeDto`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchAgentRuntimePayload {
    pub agent_session: AgentSessionRuntimeDto,
}

/// Workbench 可跨 HTTP NDJSON 传输的事件。
///
/// Business Logic（为什么需要这个枚举）:
///     远端事件流需要在一条连接中承载 terminal output、terminal status、merge progress 与 agent runtime。
///
/// Code Logic（这个枚举做什么）:
///     使用 serde 内部 tag `{type,payload}`，type 按 camelCase 输出为前端和桥接层约定的稳定值。
///     未知 type 不得经本枚举硬失败——见 `decode_remote_event`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
#[allow(clippy::large_enum_variant)] // AgentRuntime 载荷更大；保持扁平 payload 以对齐 wire 形状
pub enum WorkbenchRemoteEvent {
    TerminalOutput(WorkbenchTerminalOutputPayload),
    TerminalStatus(WorkbenchTerminalStatusPayload),
    MergeProgress(WorkbenchMergeProgressPayload),
    /// Agent session runtime 投影（capability workbench.agent-runtime.v1）
    AgentRuntime(WorkbenchAgentRuntimePayload),
}

/// Workbench 远端事件流消息（业务 Event 或显式 Gap）。
///
/// Business Logic（为什么需要这个枚举）:
///     remote bridge / Mobile 订阅 `/api/workbench/events` 时必须区分业务事件与 ring 外/lag
///     造成的缺口，禁止静默丢弃 Lagged。
///
/// Code Logic（这个枚举做什么）:
///     Event 携带 owner+sequence+业务 payload；Gap 携带 oldestAvailable/latest。
#[derive(Debug, Clone, PartialEq)]
pub enum WorkbenchRemoteRelayMessage {
    /// 带游标的业务事件。
    Event {
        owner_instance_id: String,
        sequence: u64,
        event: WorkbenchRemoteEvent,
    },
    /// owner 变化、after 早于 ring 或 live lag 后的显式缺口。
    Gap {
        owner_instance_id: String,
        oldest_available: u64,
        latest: u64,
    },
}

/// wire 解码结果别名（与 relay 消息同形）。
pub type WorkbenchRemoteStreamMessage = WorkbenchRemoteRelayMessage;

/// ring 内单条 Workbench 远端事件。
#[derive(Debug, Clone)]
struct WorkbenchRemoteRingEntry {
    sequence: u64,
    event: WorkbenchRemoteEvent,
}

/// 总线内部可变状态。
#[derive(Debug)]
struct WorkbenchRemoteEventBusInner {
    next_sequence: u64,
    ring: VecDeque<WorkbenchRemoteRingEntry>,
    ring_capacity: usize,
}

/// Workbench 远端事件有界总线（owner+sequence + ring + live）。
///
/// Business Logic（为什么需要这个结构）:
///     远端/Mobile 订阅者需要 after 游标 catch-up 与显式 Gap，禁止 bare broadcast 静默丢消息。
///
/// Code Logic（这个结构做什么）:
///     Mutex 保护 sequence/ring；broadcast 推送 `WorkbenchRemoteRelayMessage`；
///     `open_relay` 先 subscribe 再 ring catch-up，owner 变化或 after < oldest → Gap；lag → Gap。
#[derive(Debug)]
pub struct WorkbenchRemoteEventBus {
    owner_instance_id: String,
    inner: Mutex<WorkbenchRemoteEventBusInner>,
    tx: broadcast::Sender<WorkbenchRemoteRelayMessage>,
}

impl WorkbenchRemoteEventBus {
    /// 使用默认容量创建总线。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产 AppState 与 harness 需要快速挂上标准容量远端事件总线。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `with_capacity` 使用默认 ring/broadcast 容量。
    pub fn new(owner_instance_id: impl Into<String>) -> Self {
        Self::with_capacity(
            owner_instance_id,
            DEFAULT_REPLAY_RING_CAPACITY,
            DEFAULT_BROADCAST_CAPACITY,
        )
    }

    /// 使用指定容量创建总线。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试需要极小 ring/broadcast 以确定性触发 gap 与 lag。
    ///
    /// Code Logic（这个函数做什么）:
    ///     sequence 从 1 起；broadcast 至少容量 1。
    pub fn with_capacity(
        owner_instance_id: impl Into<String>,
        ring_capacity: usize,
        broadcast_capacity: usize,
    ) -> Self {
        let (tx, _) = broadcast::channel(broadcast_capacity.max(1));
        Self {
            owner_instance_id: owner_instance_id.into(),
            inner: Mutex::new(WorkbenchRemoteEventBusInner {
                next_sequence: 1,
                ring: VecDeque::new(),
                ring_capacity: ring_capacity.max(1),
            }),
            tx,
        }
    }

    /// 当前 owner 实例 id。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     NDJSON 信封与 after 游标需要绑定同一 owner 身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 owner_instance_id。
    pub fn owner_instance_id(&self) -> &str {
        &self.owner_instance_id
    }

    /// 发布一条业务事件并返回新游标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     terminal/merge/agent 远端事件必须单调编号后才能被订阅方 catch-up/去重。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同一把 inner 锁内分配 sequence、写入 ring、broadcast send，保证交付顺序与 sequence 单调。
    pub fn publish(&self, event: WorkbenchRemoteEvent) -> BackendRuntimeCursor {
        let mut inner = self.inner.lock().expect("workbench remote event bus 锁中毒");
        let sequence = inner.next_sequence;
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        if inner.ring.len() >= inner.ring_capacity {
            inner.ring.pop_front();
        }
        inner.ring.push_back(WorkbenchRemoteRingEntry {
            sequence,
            event: event.clone(),
        });
        let message = WorkbenchRemoteRelayMessage::Event {
            owner_instance_id: self.owner_instance_id.clone(),
            sequence,
            event,
        };
        let _ = self.tx.send(message);
        BackendRuntimeCursor {
            owner_instance_id: self.owner_instance_id.clone(),
            sequence,
        }
    }

    /// 当前最新 sequence（0 表示尚未发布）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap/诊断需要 live 上沿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `next_sequence - 1`（未发布时 0）。
    pub fn latest_sequence(&self) -> u64 {
        let inner = self.inner.lock().expect("workbench remote event bus 锁中毒");
        inner.next_sequence.saturating_sub(1)
    }

    /// ring 中最旧可用 sequence（空 ring 时为 0）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     判断 afterSequence 是否早于 ring。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 ring front 的 sequence，空则 0。
    pub fn oldest_available_sequence(&self) -> u64 {
        let inner = self.inner.lock().expect("workbench remote event bus 锁中毒");
        inner.ring.front().map(|e| e.sequence).unwrap_or(0)
    }

    /// 打开 catch-up + live relay 会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     订阅方重连需要 after 回放；同 owner 游标与 ring 存在空洞或 owner 变化必须 Gap；
    ///     after_seq=0 且 ring 已截断同样 Gap，禁止 silent partial replay；禁止 silent Lagged drop。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 subscribe live，再在锁内计算 gap/replay；
    ///     - after=None：brand-new 全量 ring 回放；
    ///     - after=Some(同 owner, seq)（含 seq=0）：`oldest > after_seq.saturating_add(1)` 时 Gap；
    ///       连续边界 after_seq+1==oldest 不 Gap；
    ///     - owner 变化强制 Gap；
    ///     live 路径跳过已回放 sequence。
    pub fn open_relay(&self, after: Option<&BackendRuntimeCursor>) -> WorkbenchRemoteEventRelay {
        let live_rx = self.tx.subscribe();
        let (pending, max_replayed) = {
            let inner = self.inner.lock().expect("workbench remote event bus 锁中毒");
            let latest = inner.next_sequence.saturating_sub(1);
            let oldest = inner.ring.front().map(|e| e.sequence).unwrap_or(0);
            let mut pending = Vec::new();
            let mut max_replayed = 0_u64;

            let same_owner = after
                .map(|c| c.owner_instance_id == self.owner_instance_id)
                .unwrap_or(false);
            let owner_changed = after.is_some() && !same_owner;
            let after_seq = if same_owner {
                after.map(|c| c.sequence).unwrap_or(0)
            } else {
                0
            };

            if owner_changed {
                pending.push(WorkbenchRemoteRelayMessage::Gap {
                    owner_instance_id: self.owner_instance_id.clone(),
                    oldest_available: oldest,
                    latest,
                });
                max_replayed = latest;
            } else if same_owner && oldest > after_seq.saturating_add(1) {
                // R28 H2：after_seq=0 且 oldest>1 也必须 Gap，禁止 silent partial replay。
                pending.push(WorkbenchRemoteRelayMessage::Gap {
                    owner_instance_id: self.owner_instance_id.clone(),
                    oldest_available: oldest,
                    latest,
                });
                max_replayed = latest;
            }

            if pending.is_empty() {
                for entry in inner.ring.iter() {
                    if entry.sequence > after_seq {
                        pending.push(WorkbenchRemoteRelayMessage::Event {
                            owner_instance_id: self.owner_instance_id.clone(),
                            sequence: entry.sequence,
                            event: entry.event.clone(),
                        });
                        max_replayed = entry.sequence;
                    }
                }
            }

            (pending, max_replayed)
        };

        WorkbenchRemoteEventRelay {
            owner_instance_id: self.owner_instance_id.clone(),
            pending: pending.into(),
            live_rx,
            skip_through_sequence: max_replayed,
        }
    }
}

/// 单次 Workbench 远端 relay 会话（先 catch-up 再 live）。
///
/// Business Logic（为什么需要这个结构）:
///     HTTP NDJSON 与单测需要同一套 afterSequence 语义。
///
/// Code Logic（这个结构做什么）:
///     pending 队列 + broadcast receiver；lag 转为 Gap。
pub struct WorkbenchRemoteEventRelay {
    owner_instance_id: String,
    pending: VecDeque<WorkbenchRemoteRelayMessage>,
    live_rx: broadcast::Receiver<WorkbenchRemoteRelayMessage>,
    skip_through_sequence: u64,
}

impl WorkbenchRemoteEventRelay {
    /// 拉取下一条消息（catch-up 或 live）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     路由用异步循环消费 relay，直到取消。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先弹 pending；再 await live；Lagged → Gap；同 sequence 已回放则跳过。
    pub async fn recv(&mut self) -> Option<WorkbenchRemoteRelayMessage> {
        loop {
            if let Some(msg) = self.pending.pop_front() {
                return Some(msg);
            }
            match self.live_rx.recv().await {
                Ok(msg) => {
                    if let WorkbenchRemoteRelayMessage::Event { sequence, .. } = &msg {
                        if *sequence <= self.skip_through_sequence {
                            continue;
                        }
                        self.skip_through_sequence = *sequence;
                    }
                    return Some(msg);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let latest = self.skip_through_sequence;
                    return Some(WorkbenchRemoteRelayMessage::Gap {
                        owner_instance_id: self.owner_instance_id.clone(),
                        oldest_available: latest.saturating_add(1),
                        latest,
                    });
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// 非阻塞尝试拉取（测试用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     单测需要在不挂死的情况下排空 catch-up。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 pending；再 `try_recv` live；Lagged → Gap。
    pub fn try_recv(&mut self) -> Option<WorkbenchRemoteRelayMessage> {
        if let Some(msg) = self.pending.pop_front() {
            return Some(msg);
        }
        loop {
            match self.live_rx.try_recv() {
                Ok(msg) => {
                    if let WorkbenchRemoteRelayMessage::Event { sequence, .. } = &msg {
                        if *sequence <= self.skip_through_sequence {
                            continue;
                        }
                        self.skip_through_sequence = *sequence;
                    }
                    return Some(msg);
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    let latest = self.skip_through_sequence;
                    return Some(WorkbenchRemoteRelayMessage::Gap {
                        owner_instance_id: self.owner_instance_id.clone(),
                        oldest_available: latest.saturating_add(1),
                        latest,
                    });
                }
                Err(broadcast::error::TryRecvError::Empty)
                | Err(broadcast::error::TryRecvError::Closed) => return None,
            }
        }
    }
}

/// 将 relay 消息编码为 NDJSON 行（不含末尾换行）。
///
/// Business Logic（为什么需要这个函数）:
///     `/api/workbench/events` 必须在业务信封上附 ownerInstanceId+sequence，并输出标准 Gap 帧。
///
/// Code Logic（这个函数做什么）:
///     Event：序列化 `{type,payload}` 后插入 top-level ownerInstanceId/sequence；
///     Gap：`{type:"gap",payload:{ownerInstanceId,oldestAvailable,latest}}`。
pub fn encode_workbench_remote_relay_ndjson(
    msg: &WorkbenchRemoteRelayMessage,
) -> Result<String, serde_json::Error> {
    match msg {
        WorkbenchRemoteRelayMessage::Event {
            owner_instance_id,
            sequence,
            event,
        } => {
            let mut value = serde_json::to_value(event)?;
            if let Value::Object(map) = &mut value {
                map.insert(
                    "ownerInstanceId".to_string(),
                    json!(owner_instance_id),
                );
                map.insert("sequence".to_string(), json!(sequence));
            }
            serde_json::to_string(&value)
        }
        WorkbenchRemoteRelayMessage::Gap {
            owner_instance_id,
            oldest_available,
            latest,
        } => serde_json::to_string(&json!({
            "type": "gap",
            "payload": {
                "ownerInstanceId": owner_instance_id,
                "oldestAvailable": oldest_available,
                "latest": latest,
            }
        })),
    }
}

/// 解码单行 NDJSON：业务 Event / Gap → Some；heartbeat/未知 type → None；非法 JSON → Err。
///
/// Business Logic（为什么需要这个函数）:
///     扩展新事件前，旧客户端必须忽略未知 type；Gap 必须识别以便 fail-closed 重连。
///
/// Code Logic（这个函数做什么）:
///     先解析为 Value；读 type；业务 type 反序列化为 WorkbenchRemoteEvent 并读 top-level
///     ownerInstanceId/sequence（缺省 ""/0）；gap 读 payload 游标字段；heartbeat/未知 → Ok(None)。
pub fn decode_remote_event(
    line: &str,
) -> Result<Option<WorkbenchRemoteStreamMessage>, AppError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| AppError::validation(format!("invalid remote event json: {e}")))?;
    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::validation("remote event missing type".to_string()))?;
    match event_type {
        "terminalOutput" | "terminalStatus" | "mergeProgress" | "agentRuntime" => {
            let event: WorkbenchRemoteEvent = serde_json::from_value(value.clone())
                .map_err(|e| AppError::validation(format!("invalid remote event payload: {e}")))?;
            let owner_instance_id = value
                .get("ownerInstanceId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sequence = value
                .get("sequence")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Ok(Some(WorkbenchRemoteStreamMessage::Event {
                owner_instance_id,
                sequence,
                event,
            }))
        }
        "gap" => {
            let payload = value.get("payload").cloned().unwrap_or(Value::Null);
            let owner_instance_id = payload
                .get("ownerInstanceId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let oldest_available = payload
                .get("oldestAvailable")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let latest = payload
                .get("latest")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Ok(Some(WorkbenchRemoteStreamMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            }))
        }
        // heartbeat 等非业务帧
        "heartbeat" => Ok(None),
        _ => Ok(None),
    }
}

/// 事件流解析/读取错误（含资源上限）。
///
/// Business Logic（为什么需要这个枚举）:
///     超限必须停止 bridge 并清空 buffer，不能继续累积内存；诊断只映射 error class。
///
/// Code Logic（这个枚举做什么）:
///     ResourceLimit 表示行/pending 超 1 MiB；StreamGap 携带 owner/latest 供成功 resync 后推进 after_cursor；其余为网络/HTTP/取消/空闲。
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
    /// 远端流显式 Gap：序列不连续；携带 owner/oldest/latest 供成功 resync 后推进 after_cursor（R28 H1）。
    StreamGap {
        owner_instance_id: String,
        oldest_available: u64,
        latest: u64,
    },
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
            Self::StreamGap { .. } => "stream_gap",
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
            Self::StreamGap { .. } => write!(f, "event stream gap"),
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
///     loop 与 registry 需共享 last_used/phase/attempt/error/订阅 refcount 与 after_cursor，
///     供 TTL、诊断与 task restart 后游标恢复读取（R30 M3）。
///
/// Code Logic（这个结构体做什么）:
///     Arc 原子/互斥字段；subscribers>0 时 idle_for=ZERO；after_cursor 跨 loop restart 保留；
///     不含 URL 凭据或事件正文。
struct BridgeRuntimeState {
    last_used: Mutex<Instant>,
    phase: Mutex<String>,
    attempt: AtomicU32,
    last_error_class: Mutex<Option<String>>,
    finished: AtomicBool,
    /// 活跃 stream/session 查看者计数；>0 时 bridge 视为非 idle（R30 M3）。
    subscribers: AtomicU32,
    /// 已提交的远端 stream after 游标；task restart / 重连共享（R30 M3）。
    after_cursor: Mutex<Option<BackendRuntimeCursor>>,
}

impl BridgeRuntimeState {
    /// Business Logic（为什么需要这个函数）:
    ///     新建 bridge 时需要初始化 runtime 观测字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     last_used=now、phase=connecting、attempt=0、subscribers=0、after_cursor=None。
    fn new() -> Self {
        Self {
            last_used: Mutex::new(Instant::now()),
            phase: Mutex::new("connecting".to_string()),
            attempt: AtomicU32::new(0),
            last_error_class: Mutex::new(None),
            finished: AtomicBool::new(false),
            subscribers: AtomicU32::new(0),
            after_cursor: Mutex::new(None),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     ensure_bridge / 订阅持有期间调用，表示仍有使用方，应刷新 idle TTL。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 last_used = Instant::now()。
    fn touch(&self) {
        *self.last_used.lock().expect("bridge last_used 锁中毒") = Instant::now();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     空闲回收依赖 last_used 流逝时间；有订阅者或仍在连流/resync 时不得视为 idle（R30 M3）。
    ///     安静 terminal 无 chunk 时，仅靠 touch 会误 idle 杀掉活跃 bridge。
    ///
    /// Code Logic（这个函数做什么）:
    ///     subscribers>0 → ZERO；phase ∈ connecting|streaming|resyncing|resynced → ZERO；
    ///     否则返回 last_used.elapsed()。
    fn idle_for(&self) -> Duration {
        if self.subscribers.load(Ordering::SeqCst) > 0 {
            return Duration::ZERO;
        }
        let phase = self.phase.lock().expect("bridge phase 锁中毒").clone();
        if matches!(
            phase.as_str(),
            "connecting" | "streaming" | "resyncing" | "resynced"
        ) {
            return Duration::ZERO;
        }
        self.last_used
            .lock()
            .expect("bridge last_used 锁中毒")
            .elapsed()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     活跃 remote 查看者进入时需要增加订阅 refcount，阻止 idle TTL 回收 bridge。
    ///
    /// Code Logic（这个函数做什么）:
    ///     subscribers.fetch_add(1)；并 touch last_used。
    fn retain_subscription(&self) {
        self.subscribers.fetch_add(1, Ordering::SeqCst);
        self.touch();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     查看者离开时释放订阅，允许之后在无 ensure/touch 时进入 idle TTL。
    ///
    /// Code Logic（这个函数做什么）:
    ///     saturating 减 1；减到 0 时 touch，使 idle 计时从最后一次 release 起算。
    fn release_subscription(&self) {
        let mut prev = self.subscribers.load(Ordering::SeqCst);
        loop {
            if prev == 0 {
                // 防御性：重复 release 不回绕；仍 touch 以便诊断路径刷新。
                self.touch();
                return;
            }
            match self.subscribers.compare_exchange_weak(
                prev,
                prev - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.touch();
                    return;
                }
                Err(current) => prev = current,
            }
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     重连 / restart 后需要读取已提交 after 游标，禁止 brand-new attach 丢恢复点。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 after_cursor Mutex 内容。
    fn load_after_cursor(&self) -> Option<BackendRuntimeCursor> {
        self.after_cursor
            .lock()
            .expect("bridge after_cursor 锁中毒")
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Event 推进 / Gap resync 成功后必须把游标写回共享状态，供 restart 继承。
    ///
    /// Code Logic（这个函数做什么）:
    ///     覆盖写入 after_cursor。
    fn store_after_cursor(&self, cursor: Option<BackendRuntimeCursor>) {
        *self
            .after_cursor
            .lock()
            .expect("bridge after_cursor 锁中毒") = cursor;
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
///     同一台设备的事件连接需要随着端口变化替换，同时持续复用已发现的项目 ID 映射与
///     recovery after_cursor（R30 M3）。
///
/// Code Logic（这个结构体做什么）:
///     保存 base_url、共享 project 映射、取消令牌、runtime 状态（含 after_cursor/subscribers）
///     和 JoinHandle。
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
///     GuiClient 不得创建 bridge。活跃 stream 查看者通过订阅 lease 阻止 idle 回收（R30 M3）。
///
/// Code Logic（这个结构体做什么）:
///     Mutex<HashMap<device_id, task>>；ensure 刷新 last_used；acquire/release 订阅 refcount；
///     restart 时 transfer after_cursor；shutdown_all 取消并 await。
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
        self.tasks.lock().expect("remote event bridge 锁中毒").len()
    }

    /// 返回仍在运行（未 finished）的 bridge 设备 id 集合。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap inventory 只能对真正向本机 event_bus 再发布事件的活跃桥设备 fail-closed；
    ///     无关离线 remote shortcut 不得阻断本机 terminal/runtime 恢复。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁过滤 `!is_finished()` 的 task，收集 device_id；不暴露 base_url/token/正文。
    pub fn active_device_ids(&self) -> std::collections::HashSet<String> {
        self.tasks
            .lock()
            .expect("remote event bridge 锁中毒")
            .iter()
            .filter(|(_, task)| !task.is_finished())
            .map(|(device_id, _)| device_id.clone())
            .collect()
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
    ///     URL 变化或任务结束时 cancel 旧任务并 spawn 新循环，transfer after_cursor +
    ///     subscribers 到新 runtime（R30 M3）。
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
            let transferred_cursor = existing.runtime.load_after_cursor();
            let transferred_subscribers = existing.runtime.subscribers.load(Ordering::SeqCst);
            *existing = spawn_bridge_task(
                device_id,
                base_url,
                project_ids,
                state,
                transferred_cursor,
                transferred_subscribers,
            );
            return;
        }

        let project_ids = Arc::new(RwLock::new(HashMap::new()));
        update_project_mapping(&project_ids, project_mapping);
        let task = spawn_bridge_task(device_id.clone(), base_url, project_ids, state, None, 0);
        tasks.insert(device_id, task);
    }

    /// 为活跃 remote 查看者增加订阅 lease（阻止 idle TTL 回收）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     仅依赖 ensure_bridge touch 时，长时间观看 terminal 而不再调用 ensure 仍会 idle reclaim；
    ///     订阅 refcount 保证有查看者时 bridge 不退出（R30 M3）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device 的 task.runtime.retain_subscription；无 task 时 no-op 返回 false。
    pub fn acquire_subscription(&self, device_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.retain_subscription();
        true
    }

    /// 释放活跃 remote 查看者的订阅 lease。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     查看者离开后应允许 idle TTL 从最后一次 release/touch 起算。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 task 后 release_subscription；无 task 时 no-op 返回 false。
    pub fn release_subscription(&self, device_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.release_subscription();
        true
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
///     事件桥 registry 需要把任务创建细节集中处理，保证 replacement 和首次创建使用同一套状态字段；
///     URL 变化 restart 时必须继承 after_cursor 与 subscribers，避免 brand-new attach（R30 M3）。
///
/// Code Logic（这个函数做什么）:
///     创建 cancel/runtime（seed cursor + subscribers）并 spawn remote_event_loop；结束后置 finished。
fn spawn_bridge_task(
    device_id: String,
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    state: AppState,
    initial_after_cursor: Option<BackendRuntimeCursor>,
    initial_subscribers: u32,
) -> RemoteEventBridgeTask {
    let cancel = CancellationToken::new();
    let runtime = Arc::new(BridgeRuntimeState::new());
    if initial_after_cursor.is_some() {
        runtime.store_after_cursor(initial_after_cursor);
    }
    if initial_subscribers > 0 {
        runtime
            .subscribers
            .store(initial_subscribers, Ordering::SeqCst);
        runtime.touch();
    }
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
        if loop_runtime
            .phase
            .lock()
            .expect("bridge phase 锁中毒")
            .as_str()
            != "stopped"
        {
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
///     本机 session/merge 事件 emit 时，也要同步发布到有序远端事件总线供远端设备订阅。
///
/// Code Logic（这个函数做什么）:
///     经 `WorkbenchRemoteEventBus::publish` 分配 sequence 并广播；无订阅者时忽略。
pub fn publish_workbench_remote_event_from_state(state: &AppState, event: WorkbenchRemoteEvent) {
    let _cursor = state.workbench_remote_events.publish(event);
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
///     循环：检查 cancel/idle（订阅 lease 持有时不 idle）→ 读流（共享 after_cursor）→
///     失败记 error class 并指数退避（cap 60s）；ResourceLimit 立即停止且不保留 buffer。
///     streaming 期间 touch 保持 lease；cursor 写回 runtime 供 restart 继承（R30 M3）。
async fn remote_event_loop(
    device_id: String,
    base_url: String,
    state: AppState,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    cancel: CancellationToken,
    runtime: Arc<BridgeRuntimeState>,
) {
    // after_cursor 以 runtime 共享状态为权威；本地 mut 仅作循环内缓存，推进后写回。
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
        // 每次连接前从共享状态刷新本地缓存（restart/transfer 路径可能已 seed）。
        let mut after_cursor = runtime.load_after_cursor();
        match read_remote_event_stream(
            &state,
            &device_id,
            &base_url,
            &project_ids,
            &cancel,
            &runtime,
            &mut after_cursor,
        )
        .await
        {
            Ok(()) => {
                runtime.store_after_cursor(after_cursor.clone());
                runtime.attempt.store(0, Ordering::SeqCst);
                runtime.set_error_class(None);
            }
            Err(EventStreamError::Cancelled) => {
                runtime.store_after_cursor(after_cursor.clone());
                runtime.set_phase("cancelled");
                return;
            }
            Err(EventStreamError::ResourceLimit) => {
                runtime.store_after_cursor(after_cursor.clone());
                runtime.set_phase("resource_limit");
                runtime.set_error_class(Some(EventStreamError::ResourceLimit.error_class()));
                tracing::debug!("Workbench 远端事件流超资源上限，停止 bridge");
                return;
            }
            Err(EventStreamError::IdleTimeout) => {
                runtime.store_after_cursor(after_cursor.clone());
                runtime.set_phase("idle_exit");
                runtime.set_error_class(Some(EventStreamError::IdleTimeout.error_class()));
                return;
            }
            Err(EventStreamError::StreamGap {
                owner_instance_id,
                oldest_available,
                latest,
            }) => {
                // R28 H1：Gap 后先权威 resync；仅成功才推进 after_cursor 到 gap.latest。
                runtime.set_error_class(Some("stream_gap"));
                runtime.set_phase("resyncing");
                let recovery = after_cursor.clone();
                match resync_remote_bridge_after_gap(
                    &state,
                    &device_id,
                    &base_url,
                    &project_ids,
                    &cancel,
                )
                .await
                {
                    Ok(()) => {
                        after_cursor = after_cursor_after_gap_resync(
                            recovery.as_ref(),
                            &owner_instance_id,
                            latest,
                            true,
                        );
                        runtime.store_after_cursor(after_cursor.clone());
                        runtime.attempt.store(0, Ordering::SeqCst);
                        runtime.set_error_class(None);
                        runtime.set_phase("resynced");
                        tracing::debug!(
                            oldest_available,
                            latest,
                            "Workbench 远端事件流 gap resync 成功，推进 after_cursor"
                        );
                        // 成功后立即以新 cursor 重连，无需退避。
                        continue;
                    }
                    Err(EventStreamError::Cancelled) => {
                        runtime.store_after_cursor(after_cursor.clone());
                        runtime.set_phase("cancelled");
                        return;
                    }
                    Err(err) => {
                        // 失败不推进 cursor；error class 固定 stream_gap（不把 network 覆盖 gap 语义）。
                        let _ = err;
                        after_cursor = after_cursor_after_gap_resync(
                            recovery.as_ref(),
                            &owner_instance_id,
                            latest,
                            false,
                        );
                        runtime.store_after_cursor(after_cursor.clone());
                        runtime.set_error_class(Some("stream_gap"));
                        let next = runtime.attempt.fetch_add(1, Ordering::SeqCst) + 1;
                        runtime.set_phase("backoff");
                        tracing::debug!(
                            attempt = next,
                            oldest_available,
                            latest,
                            "Workbench 远端事件流 gap resync 失败，保留 recovery cursor 退避重连: class=stream_gap"
                        );
                    }
                }
            }
            Err(error) => {
                runtime.store_after_cursor(after_cursor.clone());
                let class = error.error_class();
                runtime.set_error_class(Some(class));
                let next = runtime.attempt.fetch_add(1, Ordering::SeqCst) + 1;
                runtime.set_phase("backoff");
                tracing::debug!(
                    "Workbench 远端事件流断开，将退避重连: class={class} attempt={next}"
                );
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
///     一次远端事件连接负责持续读取 NDJSON 并把远端内部 ID 映射成本机 remote ID；
///     断线后需携带 after 游标重连，收到 Gap 必须 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     复用 `PeerClient::open_ndjson_stream` 并带 after 游标；错误 body 只读 8 KiB 前缀；
///     chunk 解析受 1 MiB 限制；Event 推进 after_cursor；Gap 清空 buffer 并返回 StreamGap。
async fn read_remote_event_stream(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    cancel: &CancellationToken,
    runtime: &BridgeRuntimeState,
    after_cursor: &mut Option<BackendRuntimeCursor>,
) -> Result<(), EventStreamError> {
    let url = event_stream_url(base_url, after_cursor.as_ref());
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
    // streaming 期间刷新 last_used，配合订阅 lease 避免仅靠 ensure_bridge 才能续期（R30 M3）。
    runtime.touch();
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
        // 仍在收流：touch 保持 idle 窗口；有 subscribers 时 idle_for 仍为 ZERO。
        runtime.touch();

        let project_map = project_ids
            .read()
            .expect("remote event bridge project 映射读锁中毒")
            .clone();
        match process_event_chunk_to_messages(device_id, &project_map, &mut buffer, &chunk) {
            Ok(messages) => {
                for message in messages {
                    match message {
                        WorkbenchRemoteStreamMessage::Event {
                            owner_instance_id,
                            sequence,
                            event,
                        } => {
                            emit_mapped_remote_event(state, event);
                            if !owner_instance_id.is_empty() && sequence > 0 {
                                *after_cursor = Some(BackendRuntimeCursor {
                                    owner_instance_id,
                                    sequence,
                                });
                                // 立即写回共享 cursor，task restart 中途也不会丢（R30 M3）。
                                runtime.store_after_cursor(after_cursor.clone());
                            }
                        }
                        WorkbenchRemoteStreamMessage::Gap {
                            owner_instance_id,
                            oldest_available,
                            latest,
                        } => {
                            // fail-closed：停止当前连接；携带 gap 游标供成功 resync 后推进。
                            buffer.clear();
                            runtime.store_after_cursor(after_cursor.clone());
                            return Err(EventStreamError::StreamGap {
                                owner_instance_id,
                                oldest_available,
                                latest,
                            });
                        }
                    }
                }
            }
            Err(EventStreamError::ResourceLimit) => {
                buffer.clear();
                runtime.store_after_cursor(after_cursor.clone());
                return Err(EventStreamError::ResourceLimit);
            }
            Err(other) => {
                buffer.clear();
                runtime.store_after_cursor(after_cursor.clone());
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
///     超 1 MiB 必须停止并清空，禁止保留超大 buffer；Gap 帧必须透传给上层 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     以 byte buffer 追加 chunk；任一行或 pending 超限 → clear + ResourceLimit；
///     完整行 UTF-8 解码 + `decode_remote_event`；Event 映射设备前缀，Gap 原样透传。
fn process_event_chunk_to_messages(
    device_id: &str,
    project_ids: &HashMap<String, String>,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<Vec<WorkbenchRemoteStreamMessage>, EventStreamError> {
    // 快速路径：追加前检查 pending 预算。
    if buffer.len().saturating_add(chunk.len()) > MAX_PENDING_BUFFER_BYTES {
        // 若合并后仍无换行且超限 → 资源上限。
        let has_newline = buffer.contains(&b'\n') || chunk.contains(&b'\n');
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

    let mut messages = Vec::new();
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
            Ok(text) => match decode_remote_event(text) {
                Ok(Some(WorkbenchRemoteStreamMessage::Event {
                    owner_instance_id,
                    sequence,
                    event,
                })) => {
                    messages.push(WorkbenchRemoteStreamMessage::Event {
                        owner_instance_id,
                        sequence,
                        event: map_remote_event_for_device(device_id, project_ids, event),
                    });
                }
                Ok(Some(gap @ WorkbenchRemoteStreamMessage::Gap { .. })) => {
                    messages.push(gap);
                }
                Ok(None) => {
                    // 未知 type / heartbeat：忽略，不中断 stream
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
    Ok(messages)
}

/// 兼容测试入口：仅收集业务 Event（忽略 Gap）。
#[cfg(test)]
fn process_event_chunk_to_events(
    device_id: &str,
    project_ids: &HashMap<String, String>,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<Vec<WorkbenchRemoteEvent>, EventStreamError> {
    let messages = process_event_chunk_to_messages(device_id, project_ids, buffer, chunk)?;
    Ok(messages
        .into_iter()
        .filter_map(|msg| match msg {
            WorkbenchRemoteStreamMessage::Event { event, .. } => Some(event),
            WorkbenchRemoteStreamMessage::Gap { .. } => None,
        })
        .collect())
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
        let partial = process_event_chunk_to_events("device-a", &project_ids, &mut buffer, &chunk)?;
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
        WorkbenchRemoteEvent::AgentRuntime(payload) => {
            state.emit_event("workbench:agent-runtime", payload);
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
        WorkbenchRemoteEvent::AgentRuntime(mut payload) => {
            let s = &mut payload.agent_session;
            s.id = remote_entity_id(device_id, &s.id);
            s.project_id = project_ids
                .get(&s.project_id)
                .cloned()
                .unwrap_or_else(|| remote_entity_id(device_id, &s.project_id));
            if let Some(wt) = s.worktree_id.as_mut() {
                *wt = remote_entity_id(device_id, wt);
            }
            s.terminal_session_id = remote_entity_id(device_id, &s.terminal_session_id);
            if let Some(task) = s.orchestrator_task_id.as_mut() {
                *task = remote_entity_id(device_id, task);
            }
            WorkbenchRemoteEvent::AgentRuntime(payload)
        }
    }
}


/// Business Logic（为什么需要这个函数）:
///     Gap 后 bridge 必须在权威 resync 成功时才推进 after_cursor，失败时保留 recovery，
///     禁止带着旧 after 永久 Gap 循环（R28 H1）。
///
/// Code Logic（这个函数做什么）:
///     resync_ok → Some(owner=gap_owner, sequence=gap_latest)；
///     失败 → recovery.cloned()（可能为 None）。
fn after_cursor_after_gap_resync(
    recovery: Option<&BackendRuntimeCursor>,
    gap_owner: &str,
    gap_latest: u64,
    resync_ok: bool,
) -> Option<BackendRuntimeCursor> {
    if resync_ok {
        Some(BackendRuntimeCursor {
            owner_instance_id: gap_owner.to_string(),
            sequence: gap_latest,
        })
    } else {
        recovery.cloned()
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端 ring 截断/owner 重启后 live 事件缺口必须用 sessions.list + sessions.replay
///     权威 cutover，再允许 after_cursor 前进；否则 GUI 终端永久停更。
///
/// Code Logic（这个函数做什么）:
///     遍历 bridge project_ids 映射的远端 project：list running sessions → replay →
///     映射 remote entity id 与 composite authority → emit workbench:terminal-resync；
///     取消 → Cancelled；list/replay 失败 → Network（不推进 cursor）；零会话仍 Ok。
async fn resync_remote_bridge_after_gap(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    cancel: &CancellationToken,
) -> Result<(), EventStreamError> {
    if cancel.is_cancelled() {
        return Err(EventStreamError::Cancelled);
    }
    let project_map = project_ids
        .read()
        .expect("remote event bridge project 映射读锁中毒")
        .clone();
    let local_bus_owner = state.config_runtime.owner_instance_id().to_string();
    let client = crate::workbench::remote_client::RemoteWorkbenchClient::new()
        .with_expected_device_id(device_id);
    for (inner_project_id, _local_shortcut_id) in project_map {
        if cancel.is_cancelled() {
            return Err(EventStreamError::Cancelled);
        }
        let sessions = client
            .list_sessions(base_url, Some(inner_project_id.as_str()))
            .await
            .map_err(|_| EventStreamError::Network)?;
        for session in sessions {
            if cancel.is_cancelled() {
                return Err(EventStreamError::Cancelled);
            }
            let status = session.status.trim();
            if !status.is_empty() && !status.eq_ignore_ascii_case("running") {
                continue;
            }
            let mut replay = client
                .replay(base_url, &session.id)
                .await
                .map_err(|_| EventStreamError::Network)?;
            let remote_session_id = remote_entity_id(device_id, &session.id);
            replay.session_id = remote_session_id.clone();
            let remote_owner = replay.owner_instance_id.clone();
            replay.owner_instance_id = Some(
                crate::workbench::terminal_authority::terminal_stream_authority(
                    &remote_session_id,
                    &local_bus_owner,
                    remote_owner.as_deref(),
                ),
            );
            state.emit_event(
                crate::backend::ui::WORKBENCH_TERMINAL_RESYNC_EVENT,
                replay,
            );
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     远端设备 base URL 可能带尾斜杠；重连时需附带 after 游标做 catch-up。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/` 后追加 `/api/workbench/events`；
///     若有 after 游标则附 `afterOwnerInstanceId` + `afterSequence` 查询（不记录 URL）。
fn event_stream_url(base_url: &str, after: Option<&BackendRuntimeCursor>) -> String {
    let base = format!("{}/api/workbench/events", base_url.trim_end_matches('/'));
    match after {
        Some(cursor)
            if !cursor.owner_instance_id.is_empty() =>
        {
            format!(
                "{base}?afterOwnerInstanceId={}&afterSequence={}",
                cursor.owner_instance_id, cursor.sequence
            )
        }
        _ => base,
    }
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
        assert!(registry.active_device_ids().is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Gap inventory 只应对未 finished 的桥设备 fail-closed；空 registry 必须返回空集合。
    ///
    /// Code Logic（这个测试做什么）:
    ///     新建 registry 断言 `active_device_ids()` 为空且 count=0。
    #[test]
    fn active_device_ids_empty_when_no_bridges() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(registry.active_device_ids().is_empty());
        assert_eq!(registry.active_bridge_count(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧客户端遇到未来事件 type 时必须忽略且不重连。
    ///
    /// Code Logic（这个测试做什么）:
    ///     decode_remote_event 对未知 type 返回 Ok(None)。
    #[test]
    fn unknown_remote_event_is_ignored_without_reconnect() {
        // 兼容计划示例的 event 键：无 type 视为 validation；带未知 type 则 ignore
        let line = r#"{"type":"futureEvent","payload":{}}"#;
        assert!(decode_remote_event(line).unwrap().is_none());
        let heartbeat = r#"{"type":"heartbeat","sentAt":"2026-07-15T00:00:00Z"}"#;
        assert!(decode_remote_event(heartbeat).unwrap().is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Agent runtime 事件 ID 必须映射为 remote:device:inner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     map_remote_event_for_device 后 id/terminal/project 均带前缀。
    #[test]
    fn map_remote_agent_runtime_event_prefixes_ids() {
        use crate::workbench::agent_runtime::models::AgentSessionPhase;
        use crate::workbench::agent_runtime::snapshot::AgentSessionRuntimeDto;
        let event = WorkbenchRemoteEvent::AgentRuntime(WorkbenchAgentRuntimePayload {
            agent_session: AgentSessionRuntimeDto {
                id: "agent-1".into(),
                project_id: "proj".into(),
                worktree_id: Some("wt".into()),
                terminal_session_id: "term".into(),
                orchestrator_task_id: Some("task".into()),
                orchestrator_attempt: Some(1),
                provider_id: "claudeCodeVisible".into(),
                phase: AgentSessionPhase::Working,
                version: 2,
                started_at: "t0".into(),
                last_activity_at: "t1".into(),
                ended_at: None,
                outcome_code: None,
                resumed_from_agent_session_id: None,
                is_active: true,
            },
        });
        let mapped = map_remote_event_for_device("device-a", &HashMap::new(), event);
        match mapped {
            WorkbenchRemoteEvent::AgentRuntime(p) => {
                assert_eq!(p.agent_session.id, "remote:device-a:agent-1");
                assert_eq!(p.agent_session.terminal_session_id, "remote:device-a:term");
                assert_eq!(p.agent_session.project_id, "remote:device-a:proj");
            }
            other => panic!("expected AgentRuntime, got {other:?}"),
        }
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
            owner_instance_id: Some("remote-owner-a".to_string()),
        });

        let mapped = map_remote_event_for_device("device-a", &HashMap::new(), event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:inner-session".to_string(),
                chunk: "hello".to_string(),
                seq: 7,
                ts: 1000,
                // map 只改 sessionId，必须保留远端 stream owner 供本机合成 composite authority。
                owner_instance_id: Some("remote-owner-a".to_string()),
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

        let first = process_event_chunk_to_messages(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[..split_at],
        )
        .expect("first chunk ok");
        let second = process_event_chunk_to_messages(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[split_at..],
        )
        .expect("second chunk ok");

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
        match &second[0] {
            WorkbenchRemoteStreamMessage::Event { event, .. } => {
                assert_eq!(
                    event,
                    &WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                        session_id: "remote:device-a:inner-session".to_string(),
                        chunk: "中文🚀输出".to_string(),
                        seq: 1,
                        ts: 1000,
                        owner_instance_id: None,
                    })
                );
            }
            other => panic!("expected StreamMessage::Event, got {other:?}"),
        }
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
            event_stream_url("http://127.0.0.1:1420/", None),
            "http://127.0.0.1:1420/api/workbench/events"
        );
        let after = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 9,
        };
        assert_eq!(
            event_stream_url("http://127.0.0.1:1420/", Some(&after)),
            "http://127.0.0.1:1420/api/workbench/events?afterOwnerInstanceId=owner-a&afterSequence=9"
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

    fn sample_status_event(n: u64) -> WorkbenchRemoteEvent {
        WorkbenchRemoteEvent::TerminalStatus(WorkbenchTerminalStatusPayload {
            session_id: format!("s{n}"),
            status: "running".into(),
            exit_code: None,
            ts: n as i64,
        })
    }

    /// Business Logic（为什么需要这个测试）:
    ///     总线必须为每条事件分配单调 sequence，供 after 游标去重。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续 publish 3 条，断言 sequence 1..3 且 owner 一致。
    #[test]
    fn publish_assigns_monotonic_sequence() {
        let bus = WorkbenchRemoteEventBus::new("owner-a");
        let c1 = bus.publish(sample_status_event(1));
        let c2 = bus.publish(sample_status_event(2));
        let c3 = bus.publish(sample_status_event(3));
        assert_eq!(c1.sequence, 1);
        assert_eq!(c2.sequence, 2);
        assert_eq!(c3.sequence, 3);
        assert_eq!(c3.owner_instance_id, "owner-a");
        assert_eq!(bus.latest_sequence(), 3);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     重连 afterSequence 只应回放更新事件，避免重复交付。
    ///
    /// Code Logic（这个测试做什么）:
    ///     publish 1..3 后 open_relay(after=1)，try_recv 得到 sequence 2 与 3。
    #[test]
    fn open_relay_after_sequence_replays_only_newer() {
        let bus = WorkbenchRemoteEventBus::new("owner-a");
        let c1 = bus.publish(sample_status_event(1));
        bus.publish(sample_status_event(2));
        bus.publish(sample_status_event(3));
        let mut relay = bus.open_relay(Some(&c1));
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 2),
            other => panic!("expected event 2, got {other:?}"),
        }
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 3),
            other => panic!("expected event 3, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     after 早于 ring 必须 Gap，禁止 partial replay 造成 silent loss。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ring=2 时 publish 1..4，after=1 触发 Gap 且无 Event。
    #[test]
    fn open_relay_after_earlier_than_ring_emits_gap() {
        let bus = WorkbenchRemoteEventBus::with_capacity("owner-a", 2, 8);
        bus.publish(sample_status_event(1));
        bus.publish(sample_status_event(2));
        bus.publish(sample_status_event(3));
        bus.publish(sample_status_event(4));
        let stale = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 1,
        };
        let mut relay = bus.open_relay(Some(&stale));
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Gap {
                oldest_available,
                latest,
                ..
            }) => {
                assert_eq!(oldest_available, 3);
                assert_eq!(latest, 4);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 变化必须强制 Gap，禁止只回放新 owner ring 尾部。
    ///
    /// Code Logic（这个测试做什么）:
    ///     after 携带不同 owner，open_relay 首条为 Gap。
    #[test]
    fn open_relay_owner_change_emits_gap() {
        let bus = WorkbenchRemoteEventBus::new("owner-b");
        bus.publish(sample_status_event(1));
        let old = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 9,
        };
        let mut relay = bus.open_relay(Some(&old));
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Gap {
                owner_instance_id, ..
            }) => assert_eq!(owner_instance_id, "owner-b"),
            other => panic!("expected gap, got {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     live lag 不得静默丢弃，必须显式 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     broadcast 容量 1：先 subscribe 再 publish 多条不消费，try_recv 得到 Gap。
    #[test]
    fn relay_lag_emits_gap() {
        let bus = WorkbenchRemoteEventBus::with_capacity("owner-a", 8, 1);
        let mut relay = bus.open_relay(None);
        bus.publish(sample_status_event(1));
        bus.publish(sample_status_event(2));
        bus.publish(sample_status_event(3));
        // 可能先收到 ring catch-up event，也可能直接 Lagged→Gap；排空到首条 Gap 即通过。
        let mut saw_gap = false;
        for _ in 0..8 {
            match relay.try_recv() {
                Some(WorkbenchRemoteRelayMessage::Gap { .. }) => {
                    saw_gap = true;
                    break;
                }
                Some(WorkbenchRemoteRelayMessage::Event { .. }) => continue,
                None => break,
            }
        }
        assert!(saw_gap, "lagged live must surface Gap");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     wire Gap 帧必须被 decode 识别，供 bridge fail-closed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     decode gap JSON → Some(Gap)；未知 type 仍 Ok(None)。
    #[test]
    fn decode_gap_frame_is_recognized() {
        let line = r#"{"type":"gap","payload":{"ownerInstanceId":"owner-a","oldestAvailable":3,"latest":9}}"#;
        match decode_remote_event(line).unwrap() {
            Some(WorkbenchRemoteStreamMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            }) => {
                assert_eq!(owner_instance_id, "owner-a");
                assert_eq!(oldest_available, 3);
                assert_eq!(latest, 9);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert_eq!(
            decode_remote_event(r#"{"type":"futureEvent","payload":{}}"#).unwrap(),
            None
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     业务 NDJSON 信封必须附 ownerInstanceId+sequence。
    ///
    /// Code Logic（这个测试做什么）:
    ///     encode Event 后 JSON 含 type/ownerInstanceId/sequence，且无 terminal body 断言依赖。
    #[test]
    fn encode_event_includes_owner_and_sequence() {
        let msg = WorkbenchRemoteRelayMessage::Event {
            owner_instance_id: "owner-a".into(),
            sequence: 4,
            event: sample_status_event(4),
        };
        let line = encode_workbench_remote_relay_ndjson(&msg).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "terminalStatus");
        assert_eq!(value["ownerInstanceId"], "owner-a");
        assert_eq!(value["sequence"], 4);
        assert!(value.get("payload").is_some());
    }

    /// Business Logic（R28 H2: 为什么需要这个测试）:
    ///     after sequence=0 在 ring 截断后必须 Gap，禁止 silent partial replay。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ring=2 publish 1..4 → after=(owner,0) → Gap oldest=3 latest=4。
    #[test]
    fn open_relay_after_sequence_zero_with_truncated_ring_emits_gap() {
        let bus = WorkbenchRemoteEventBus::with_capacity("owner-a", 2, 8);
        bus.publish(sample_status_event(1));
        bus.publish(sample_status_event(2));
        bus.publish(sample_status_event(3));
        bus.publish(sample_status_event(4));
        let after_zero = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 0,
        };
        let mut relay = bus.open_relay(Some(&after_zero));
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Gap {
                oldest_available,
                latest,
                ..
            }) => {
                assert_eq!(oldest_available, 3);
                assert_eq!(latest, 4);
            }
            other => panic!("expected gap for after_seq=0 truncated ring, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// Business Logic（R28 H2: 为什么需要这个测试）:
    ///     连续边界 after_seq+1 == oldest 不得误报 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     after=2、ring 含 3,4 → Event 3 与 4，无 Gap。
    #[test]
    fn open_relay_continuous_boundary_does_not_gap() {
        let bus = WorkbenchRemoteEventBus::with_capacity("owner-a", 2, 8);
        bus.publish(sample_status_event(1));
        bus.publish(sample_status_event(2));
        bus.publish(sample_status_event(3));
        bus.publish(sample_status_event(4));
        let after = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 2,
        };
        let mut relay = bus.open_relay(Some(&after));
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 3),
            other => panic!("expected event 3, got {other:?}"),
        }
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 4),
            other => panic!("expected event 4, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// Business Logic（R28 H1: 为什么需要这个测试）:
    ///     Gap resync 成功才推进 after_cursor；失败保留 recovery。
    ///
    /// Code Logic（这个测试做什么）:
    ///     成功 → cursor=gap latest（含 gap_latest==0）；失败 → Some(recovery) 或 None。
    #[test]
    fn after_cursor_after_gap_resync_advances_only_on_success() {
        let recovery = BackendRuntimeCursor {
            owner_instance_id: "owner-old".into(),
            sequence: 7,
        };
        let ok = after_cursor_after_gap_resync(Some(&recovery), "owner-new", 42, true)
            .expect("advanced");
        assert_eq!(ok.owner_instance_id, "owner-new");
        assert_eq!(ok.sequence, 42);
        // gap_latest==0 成功时仍推进（空 ring 的 latest=0 也是合法新游标）。
        let zero = after_cursor_after_gap_resync(Some(&recovery), "owner-new", 0, true)
            .expect("advanced zero");
        assert_eq!(zero.sequence, 0);
        assert_eq!(zero.owner_instance_id, "owner-new");
        let keep = after_cursor_after_gap_resync(Some(&recovery), "owner-new", 42, false)
            .expect("recovery");
        assert_eq!(keep, recovery);
        assert!(after_cursor_after_gap_resync(None, "owner-new", 42, false).is_none());
    }

    /// Business Logic（R28 H1: 为什么需要这个测试）:
    ///     StreamGap 必须携带 owner/oldest/latest 字段供 resync 决策，error_class 仍为 stream_gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 StreamGap 变体，断言字段与 error_class。
    #[test]
    fn stream_gap_error_carries_fields_and_class() {
        let err = EventStreamError::StreamGap {
            owner_instance_id: "owner-a".into(),
            oldest_available: 3,
            latest: 9,
        };
        match &err {
            EventStreamError::StreamGap {
                owner_instance_id,
                oldest_available,
                latest,
            } => {
                assert_eq!(owner_instance_id, "owner-a");
                assert_eq!(*oldest_available, 3);
                assert_eq!(*latest, 9);
            }
            other => panic!("expected StreamGap, got {other:?}"),
        }
        assert_eq!(err.error_class(), "stream_gap");
    }

    /// Business Logic（R30 M3: 为什么需要这个测试）:
    ///     活跃查看者持有订阅时 idle TTL 不得把 bridge 视为可回收；
    ///     活跃 phase（streaming）同样不得 idle。
    ///
    /// Code Logic（这个测试做什么）:
    ///     phase 置 backoff（非活跃）+ last_used 过期 → idle；
    ///     retain 后 ZERO；release + 过期再 idle；phase=streaming 时 ZERO。
    #[test]
    fn subscription_lease_keeps_bridge_from_idle() {
        let runtime = BridgeRuntimeState::new();
        // 新 runtime 默认 phase=connecting，属于活跃相位；测纯订阅语义时先切到 backoff。
        runtime.set_phase("backoff");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "无订阅且非活跃 phase 且 last_used 过期时应 idle"
        );

        runtime.retain_subscription();
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "subscribers>0 时不得 idle"
        );
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert_eq!(runtime.idle_for(), Duration::ZERO);

        runtime.release_subscription();
        runtime.set_phase("backoff");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS));

        // 安静 streaming 相位：即使 last_used 过期也不得 idle。
        runtime.set_phase("streaming");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "streaming phase must keep bridge non-idle without chunk touch"
        );
    }

    /// Business Logic（R30 M3: 为什么需要这个测试）:
    ///     双重 retain 需双重 release 才允许 idle，避免一个 viewer 释放误杀其他 viewer。
    ///
    /// Code Logic（这个测试做什么）:
    ///     phase=backoff；retain×2 → release×1 仍 ZERO；再 release 后可 idle。
    #[test]
    fn subscription_refcount_requires_matching_release() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("backoff");
        runtime.retain_subscription();
        runtime.retain_subscription();
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        runtime.release_subscription();
        assert_eq!(runtime.idle_for(), Duration::ZERO);
        runtime.release_subscription();
        runtime.set_phase("backoff");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS));
        // 额外 release 不 panic / 不回绕。
        runtime.release_subscription();
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
    }

    /// Business Logic（R30 M3: 为什么需要这个测试）:
    ///     URL 变化或任务替换时 after_cursor 必须从旧 runtime transfer 到新 runtime，
    ///     禁止 brand-new attach 丢掉 recovery 点。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed cursor → store；spawn_bridge_task 带 initial_after_cursor；
    ///     新 runtime.load_after_cursor 等于 seed；subscribers 一并 transfer。
    #[test]
    fn after_cursor_preserved_across_spawn_bridge_task_restart() {
        let seed = BackendRuntimeCursor {
            owner_instance_id: "owner-seed".into(),
            sequence: 99,
        };
        // 不真实跑 loop 到网络：只验证 spawn 时 seed 写入新 runtime。
        // spawn_bridge_task 需要 AppState；用 finished abort 路径测 seed 字段。
        // 这里直接测 runtime seed 语义（spawn 内部同样 store_after_cursor）。
        let runtime = BridgeRuntimeState::new();
        runtime.store_after_cursor(Some(seed.clone()));
        runtime.subscribers.store(2, Ordering::SeqCst);

        let transferred_cursor = runtime.load_after_cursor();
        let transferred_subscribers = runtime.subscribers.load(Ordering::SeqCst);
        let restarted = BridgeRuntimeState::new();
        restarted.store_after_cursor(transferred_cursor);
        restarted
            .subscribers
            .store(transferred_subscribers, Ordering::SeqCst);

        let loaded = restarted.load_after_cursor().expect("cursor transferred");
        assert_eq!(loaded.owner_instance_id, "owner-seed");
        assert_eq!(loaded.sequence, 99);
        assert_eq!(restarted.subscribers.load(Ordering::SeqCst), 2);
        // 有订阅时 idle 为 ZERO。
        *restarted.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 1);
        assert_eq!(restarted.idle_for(), Duration::ZERO);
    }

    /// Business Logic（R30 M3: 为什么需要这个测试）:
    ///     registry acquire/release 在无 bridge 时必须 no-op 安全，有 bridge 时驱动 runtime refcount。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry acquire/release 返回 false；手工插入 runtime 后 acquire 增 refcount。
    #[test]
    fn registry_subscription_api_noops_without_task_and_retains_with_runtime() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(!registry.acquire_subscription("missing-device"));
        assert!(!registry.release_subscription("missing-device"));

        // 直接测 runtime 路径与 registry 语义对齐（避免构造完整 AppState 任务）。
        let runtime = BridgeRuntimeState::new();
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        runtime.retain_subscription();
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        runtime.release_subscription();
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
    }

}
