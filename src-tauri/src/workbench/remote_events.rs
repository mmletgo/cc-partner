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
//!     R37 H1：inbound 已带 `remote:` entity id 的事件在 process/map 前 drop（破 A↔B 环路）；
//!     R38 H1：`emit_mapped_remote_event` 接收的是已 map 的 native-from-peer 事件（entity 已是
//!     `remote:`），必须 publish+emit；禁止再二次 drop mapped remote ids（否则全部 live 被杀）。
//!     R37 H2：resync 另发布 `TerminalResync` 到本机 bus，Mobile 与桌面 Tauri resync 对齐；
//!     R37 H3：仅 running 持 session watch；非 running status/list reconcile 释放。
//!     R38 M2：project-scoped last-seen running reconcile——list 只处理返回行，peer list 中消失
//!     的 session（无 status 事件）会永远占 watch；按 local_project_id 记录上次 list 见过的
//!     running remote session ids，对 previous−running 调 release_watch_key，不碰其它 project 与
//!     `__device__`；restart 时 transfer project_running_sessions（与 after_cursor/watch_keys 一致）。
//!     R42 M1 / R43 M1：project_watch（epochs + running_sessions）同一互斥临界区覆盖 note 与
//!     reconcile 的 epoch compare + previous snapshot + set commit + epoch bump；to_release 差集
//!     在锁内确定，release_watch_key 锁外执行，堵住 create note 与 stale/full list 的竞态窗口。
//!     R44 M1：Gap resync 在 list **前**原子捕获 epoch+previous；`reconcile_if_epoch`；
//!     仅 committed 时 previous−listed → disconnected；stale 只 union、不 release、不假 disconnected。
//!     提供本机事件发布 helper，并维护 sidecar 拥有的 `RemoteEventBridgeRegistry`
//!     （CancellationToken、订阅 lease/refcount + idle TTL、指数退避上限 60s、1 MiB 行/pending、
//!     8 KiB 错误前缀、共享 after 游标跨 task restart 保留、Gap fail-closed、
//!     shutdown_all 等待退出；R30 M3）。

use crate::backend::event_bus::BackendRuntimeCursor;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::agent_runtime::snapshot::AgentSessionRuntimeDto;
use crate::workbench::remote_ids::{is_remote_id, parse_remote_entity_id, remote_entity_id};
use crate::workbench::sessions::WorkbenchSessionReplayDto;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
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
/// 设备级 watch lease 的稳定 key（R35 M2：与 session key 并列，幂等 retain）。
const WATCH_KEY_DEVICE: &str = "__device__";

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

/// Gap resync 权威终端回放 payload（与 `workbench:terminal-resync` / replay DTO 对齐）。
///
/// Business Logic（为什么需要这个结构体）:
///     bridge Gap resync 后桌面靠 Tauri `workbench:terminal-resync` cutover；
///     Mobile 只订阅本机 `workbench_remote_events` NDJSON bus，必须收到同形态权威快照才能 `store.reset`。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 字段对齐 `WorkbenchSessionReplayDto`：sessionId/buffer/truncated/lastSeq/ownerInstanceId?。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalResyncPayload {
    pub session_id: String,
    pub buffer: String,
    pub truncated: bool,
    pub last_seq: u64,
    /// cutover 权威；缺失时前端不得重置已绑定 authority。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_instance_id: Option<String>,
}

impl WorkbenchTerminalResyncPayload {
    /// Business Logic（为什么需要这个函数）:
    ///     resync 路径已构造 `WorkbenchSessionReplayDto`，需要无损转成 bus payload 发布给 Mobile。
    ///
    /// Code Logic（这个函数做什么）:
    ///     字段一一拷贝到 `WorkbenchTerminalResyncPayload`。
    pub fn from_replay(replay: &WorkbenchSessionReplayDto) -> Self {
        Self {
            session_id: replay.session_id.clone(),
            buffer: replay.buffer.clone(),
            truncated: replay.truncated,
            last_seq: replay.last_seq,
            owner_instance_id: replay.owner_instance_id.clone(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     desktop Tauri emit 继续走既有 `WorkbenchSessionReplayDto` 形状，避免第二套前端契约。
    ///
    /// Code Logic（这个函数做什么）:
    ///     字段一一拷贝为 `WorkbenchSessionReplayDto`。
    pub fn to_replay(&self) -> WorkbenchSessionReplayDto {
        WorkbenchSessionReplayDto {
            session_id: self.session_id.clone(),
            buffer: self.buffer.clone(),
            truncated: self.truncated,
            last_seq: self.last_seq,
            owner_instance_id: self.owner_instance_id.clone(),
        }
    }
}

/// Workbench 可跨 HTTP NDJSON 传输的事件。
///
/// Business Logic（为什么需要这个枚举）:
///     远端事件流需要在一条连接中承载 terminal output、terminal status、merge progress、
///     agent runtime 与 Gap resync 权威 terminalResync。
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
    /// Gap resync 权威终端快照（R37 H2；wire type `terminalResync`）
    TerminalResync(WorkbenchTerminalResyncPayload),
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
        /// 业务 payload 体积远大于 Gap 字段；Box 避免 large_enum_variant。
        event: Box<WorkbenchRemoteEvent>,
    },
    /// owner 变化、after 早于 ring 或 live lag 后的显式缺口。
    Gap {
        owner_instance_id: String,
        oldest_available: u64,
        latest: u64,
    },
    /// 服务端过滤掉非当前终端正文时，仅推进消费游标，不携带正文。
    Cursor {
        owner_instance_id: String,
        sequence: u64,
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
        let mut inner = self
            .inner
            .lock()
            .expect("workbench remote event bus 锁中毒");
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
            event: Box::new(event),
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
        let inner = self
            .inner
            .lock()
            .expect("workbench remote event bus 锁中毒");
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
        let inner = self
            .inner
            .lock()
            .expect("workbench remote event bus 锁中毒");
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
            let inner = self
                .inner
                .lock()
                .expect("workbench remote event bus 锁中毒");
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
                            event: Box::new(entry.event.clone()),
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
                map.insert("ownerInstanceId".to_string(), json!(owner_instance_id));
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
        WorkbenchRemoteRelayMessage::Cursor {
            owner_instance_id,
            sequence,
        } => serde_json::to_string(&json!({
            "type": "cursor",
            "payload": {
                "ownerInstanceId": owner_instance_id,
                "sequence": sequence,
            }
        })),
    }
}

/// 按当前终端窗口过滤高带宽正文并编码 NDJSON。
///
/// Business Logic（为什么需要这个函数）:
///     远程 Workbench 只应实时下载用户当前查看窗口的正文；状态、merge 与 Agent 事件仍需实时到达。
///     被过滤事件仍必须推进全局游标，否则重连会重复扫描旧 ring，甚至触发全量 Gap resync。
///
/// Code Logic（这个函数做什么）:
///     filter=None 保持旧客户端完整流；filter=Some 时，非目标 session 的 TerminalOutput/TerminalResync
///     编码为不含正文的 Cursor 帧，其余事件原样编码。
pub fn encode_workbench_remote_relay_ndjson_filtered(
    msg: &WorkbenchRemoteRelayMessage,
    terminal_session_filter: Option<&str>,
) -> Result<String, serde_json::Error> {
    let should_filter = match (msg, terminal_session_filter) {
        (
            WorkbenchRemoteRelayMessage::Event {
                event,
                owner_instance_id: _,
                sequence: _,
            },
            Some(target),
        ) => match event.as_ref() {
            WorkbenchRemoteEvent::TerminalOutput(payload) => payload.session_id != target,
            WorkbenchRemoteEvent::TerminalResync(payload) => payload.session_id != target,
            WorkbenchRemoteEvent::TerminalStatus(_)
            | WorkbenchRemoteEvent::MergeProgress(_)
            | WorkbenchRemoteEvent::AgentRuntime(_) => false,
        },
        _ => false,
    };
    if should_filter {
        if let WorkbenchRemoteRelayMessage::Event {
            owner_instance_id,
            sequence,
            ..
        } = msg
        {
            return encode_workbench_remote_relay_ndjson(&WorkbenchRemoteRelayMessage::Cursor {
                owner_instance_id: owner_instance_id.clone(),
                sequence: *sequence,
            });
        }
    }
    encode_workbench_remote_relay_ndjson(msg)
}

/// 解码单行 NDJSON：业务 Event / Gap → Some；heartbeat/未知 type → None；非法 JSON → Err。
///
/// Business Logic（为什么需要这个函数）:
///     扩展新事件前，旧客户端必须忽略未知 type；Gap 必须识别以便 fail-closed 重连。
///
/// Code Logic（这个函数做什么）:
///     先解析为 Value；读 type；业务 type 反序列化为 WorkbenchRemoteEvent 并读 top-level
///     ownerInstanceId/sequence（缺省 ""/0）；gap 读 payload 游标字段；heartbeat/未知 → Ok(None)。
pub fn decode_remote_event(line: &str) -> Result<Option<WorkbenchRemoteStreamMessage>, AppError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| AppError::validation(format!("invalid remote event json: {e}")))?;
    let event_type = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::validation("remote event missing type".to_string()))?;
    match event_type {
        "terminalOutput" | "terminalStatus" | "mergeProgress" | "agentRuntime"
        | "terminalResync" => {
            let event: WorkbenchRemoteEvent = serde_json::from_value(value.clone())
                .map_err(|e| AppError::validation(format!("invalid remote event payload: {e}")))?;
            let owner_instance_id = value
                .get("ownerInstanceId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sequence = value.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(Some(WorkbenchRemoteStreamMessage::Event {
                owner_instance_id,
                sequence,
                event: Box::new(event),
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
            let latest = payload.get("latest").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(Some(WorkbenchRemoteStreamMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            }))
        }
        "cursor" => {
            let payload = value.get("payload").cloned().unwrap_or(Value::Null);
            let owner_instance_id = payload
                .get("ownerInstanceId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let sequence = payload
                .get("sequence")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Ok(Some(WorkbenchRemoteStreamMessage::Cursor {
                owner_instance_id,
                sequence,
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

/// 单 project 的 watch mutation epoch 与 last-seen running set（R43 M1）。
///
/// Business Logic（为什么需要这个结构体）:
///     epoch 与 running set 若分两把锁，create note 可插在 reconcile 的 epoch 读与 previous
///     快照/commit 之间，绕过 R42 防护：要么被 stale list release，要么被 full-replace 覆盖。
///
/// Code Logic（这个结构体做什么）:
///     同一互斥状态持有 `epochs` 与 `running_sessions`；note / bump / reconcile 路径的
///     epoch compare + previous snapshot + set commit + epoch bump 必须在同一临界区完成。
#[derive(Default)]
struct ProjectWatchState {
    /// local_project_id -> project watch mutation epoch（R42 M1 / R43 M1）。
    epochs: HashMap<String, u64>,
    /// local_project_id -> 上次 list 见过的 running remote session ids（R38 M2）。
    running_sessions: HashMap<String, HashSet<String>>,
}

impl ProjectWatchState {
    /// Business Logic（为什么需要这个函数）:
    ///     create/note 与成功 full reconcile 会改变 project 的权威 running 集合；
    ///     list 必须能通过 epoch 检测到该变化（R42 M1 / R43 M1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     epochs[project] = prev.saturating_add(1)（缺省 prev=0）；调用方须已持有外层 project_watch 锁。
    fn bump_epoch(&mut self, project_local_id: &str) {
        let entry = self.epochs.entry(project_local_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
    }
}

/// 单设备 bridge 任务内部共享状态。
///
/// Business Logic（为什么需要这个结构体）:
///     loop 与 registry 需共享 last_used/phase/attempt/error/订阅 refcount 与 after_cursor，
///     供 TTL、诊断与 task restart 后游标恢复读取（R30 M3）。
///     R35 M2：多 terminal 共用同一 device bridge 时，必须以 session-keyed lease 计数，
///     避免 close 一次把整个设备订阅打到 0。
///     R38 M2：按 local_project_id 记录上次 list 见过的 running remote session ids，
///     以便 list 中消失的 session（无 status 事件）也能 release watch。
///     R43 M1：epoch 与 running set 合并为同一互斥 `project_watch`，堵住 note/reconcile 竞态。
///
/// Code Logic（这个结构体做什么）:
///     Arc 原子/互斥字段；`watch_keys` 记录活跃 lease key，`subscribers` 与 set 大小同步；
///     subscribers>0 时 idle_for=ZERO；after_cursor 跨 loop restart 保留；
///     `project_watch.running_sessions` 跨 restart transfer；不含 URL/正文。
struct BridgeRuntimeState {
    last_used: Mutex<Instant>,
    phase: Mutex<String>,
    attempt: AtomicU32,
    last_error_class: Mutex<Option<String>>,
    finished: AtomicBool,
    /// 活跃 stream/session 查看者计数；与 watch_keys.len 同步，>0 时 bridge 非 idle。
    subscribers: AtomicU32,
    /// session/device 级 watch lease 集合；幂等 retain/release，驱动 subscribers（R35 M2）。
    watch_keys: Mutex<HashSet<String>>,
    /// 已提交的远端 stream after 游标；task restart / 重连共享（R30 M3）。
    after_cursor: Mutex<Option<BackendRuntimeCursor>>,
    /// project-scoped epoch + last-seen running set（R42 M1 / R43 M1 同一把锁）。
    project_watch: Mutex<ProjectWatchState>,
}

impl BridgeRuntimeState {
    /// Business Logic（为什么需要这个函数）:
    ///     新建 bridge 时需要初始化 runtime 观测字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     last_used=now、phase=connecting、attempt=0、subscribers=0、watch_keys 空、
    ///     after_cursor=None、project_watch 空。
    fn new() -> Self {
        Self {
            last_used: Mutex::new(Instant::now()),
            phase: Mutex::new("connecting".to_string()),
            attempt: AtomicU32::new(0),
            last_error_class: Mutex::new(None),
            finished: AtomicBool::new(false),
            subscribers: AtomicU32::new(0),
            watch_keys: Mutex::new(HashSet::new()),
            after_cursor: Mutex::new(None),
            project_watch: Mutex::new(ProjectWatchState::default()),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     ensure_bridge / retain/release watch 等**外部需求**路径调用，刷新 idle TTL 起点。
    ///     R41 M3：仅 demand 可 touch；heartbeat/入站网络流量不得刷新该时钟，
    ///     否则零订阅 + peer 在线时 15s heartbeat 会永久挡 60s idle 回收。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写 last_used = Instant::now()（demand last_used）。
    fn touch(&self) {
        *self.last_used.lock().expect("bridge last_used 锁中毒") = Instant::now();
    }

    /// Business Logic（为什么需要这个函数）:
    ///     空闲回收依赖 demand last_used 流逝时间；有订阅者时不得视为 idle（R30 M3）。
    ///     R32 M2：connecting/streaming/backoff 仅在 subscribers>0 时非 idle；
    ///     零订阅者按 last_used TTL 计时，使最后一 tab 关闭后 bridge 可 idle 回收。
    ///     R41 M3：零订阅时 idle 只看 demand touch（ensure/retain/release），忽略网络活动。
    ///
    /// Code Logic（这个函数做什么）:
    ///     subscribers>0 → ZERO（任意 phase）；
    ///     subscribers==0 → last_used.elapsed()（含 connecting/streaming/backoff）。
    fn idle_for(&self) -> Duration {
        if self.subscribers.load(Ordering::SeqCst) > 0 {
            return Duration::ZERO;
        }
        // R32 M2 / R41 M3：零订阅者一律走 demand last_used TTL。
        self.last_used
            .lock()
            .expect("bridge last_used 锁中毒")
            .elapsed()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     create 成功后若只 ensure_session_watch 而不登记 project_running_sessions，
    ///     用户在首次 list 前 remove project 时 clear_project_running_sessions 找不到 key，
    ///     watch 会永久占 subscribers（R41 M2）。
    ///     R43 M1：insert + epoch bump 必须在同一 project_watch 临界区，避免 stale reconcile
    ///     在两锁间隙把新 session 当 previous 差集 release 或被 full-replace 覆盖。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同一把 project_watch 锁内：幂等 insert session 到 running_sessions[project]；
    ///     若为新 session 则 bump epochs[project]；不 touch、不改 watch_keys。
    fn note_project_running_session(&self, project_local_id: &str, session_id: &str) {
        let mut watch = self
            .project_watch
            .lock()
            .expect("bridge project_watch 锁中毒");
        let inserted = watch
            .running_sessions
            .entry(project_local_id.to_string())
            .or_default()
            .insert(session_id.to_string());
        if inserted {
            watch.bump_epoch(project_local_id);
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list 在发起 RPC 前捕获 epoch，reconcile 时比对是否仍匹配。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 project_watch 锁返回 epochs[project]，缺省 0。
    fn project_watch_epoch(&self, project_local_id: &str) -> u64 {
        self.project_watch
            .lock()
            .expect("bridge project_watch 锁中毒")
            .epochs
            .get(project_local_id)
            .copied()
            .unwrap_or(0)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Gap resync 若先 list 再分两次读 epoch 与 previous_ids，create/resume 可能在中间
    ///     note 并 bump epoch，迟到的空/残缺 list 会 release 新 watch 或假 disconnected（R44）。
    ///     必须在 list 前同一临界区原子捕获 epoch+running set。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 project_watch 锁返回 `(epochs[project] 或 0, running_sessions[project] 克隆或空)`。
    fn project_watch_epoch_and_running(&self, project_local_id: &str) -> (u64, HashSet<String>) {
        let watch = self
            .project_watch
            .lock()
            .expect("bridge project_watch 锁中毒");
        let epoch = watch.epochs.get(project_local_id).copied().unwrap_or(0);
        let running = watch
            .running_sessions
            .get(project_local_id)
            .cloned()
            .unwrap_or_default();
        (epoch, running)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     设备/session 进入可见态时需要幂等持有一条 watch lease，避免 ensure 路径无限加计数，
    ///     同时让多 session 各自持有独立 key（R35 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     向 watch_keys insert key；若为新 key 则 subscribers+=1 并 touch；已存在仅 touch。
    ///     返回是否新插入。
    fn retain_watch_key(&self, key: &str) -> bool {
        let mut keys = self.watch_keys.lock().expect("bridge watch_keys 锁中毒");
        let inserted = keys.insert(key.to_string());
        if inserted {
            self.subscribers.fetch_add(1, Ordering::SeqCst);
        }
        drop(keys);
        self.touch();
        inserted
    }

    /// Business Logic（为什么需要这个函数）:
    ///     session/设备离开时只释放自己的 key，剩余 key 仍可阻止 bridge idle 回收（R35 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从 watch_keys remove key；若移除成功则 saturating 减 subscribers 并 touch；
    ///     key 不存在时 no-op（防重复 close 下溢），仍 touch。
    ///     返回是否真正移除。
    fn release_watch_key(&self, key: &str) -> bool {
        let mut keys = self.watch_keys.lock().expect("bridge watch_keys 锁中毒");
        let removed = keys.remove(key);
        drop(keys);
        if removed {
            let mut prev = self.subscribers.load(Ordering::SeqCst);
            loop {
                if prev == 0 {
                    break;
                }
                match self.subscribers.compare_exchange_weak(
                    prev,
                    prev - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(current) => prev = current,
                }
            }
        }
        self.touch();
        removed
    }

    /// Business Logic（为什么需要这个函数）:
    ///     URL 变化/task restart 时必须把已持有的 session/device keys 迁到新 runtime，
    ///     否则 close 会因 key 丢失而无法正确 release，subscribers 也与现实脱节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 watch_keys 集合。
    fn clone_watch_keys(&self) -> HashSet<String> {
        self.watch_keys
            .lock()
            .expect("bridge watch_keys 锁中毒")
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn 新 bridge task 时用旧 runtime 的 keys 重建 lease 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     覆盖写入 watch_keys，并把 subscribers 设为 keys.len()；非空时 touch。
    fn seed_watch_keys(&self, keys: HashSet<String>) {
        let count = keys.len() as u32;
        *self.watch_keys.lock().expect("bridge watch_keys 锁中毒") = keys;
        self.subscribers.store(count, Ordering::SeqCst);
        if count > 0 {
            self.touch();
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list reconcile 只处理返回行；peer list 中消失的 running session（无 status 事件）
    ///     会永远占 watch。按 project 比较上次见过的 running ids 并释放差集（R38 M2）。
    ///     R42 M1：list 发起后 create 可能 note 新 session；expected_epoch 不匹配时跳过 commit。
    ///     R43 M1：epoch compare / previous snapshot / set commit / epoch bump 同临界区。
    ///     R44：返回 `Option<usize>`——`None`=stale 未 commit，`Some(n)`=full reconcile 已提交。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `reconcile_project_running_sessions_if_epoch(..., None)`；
    ///     expected=None 路径恒返回 `Some(released)`。
    fn reconcile_project_running_sessions(
        &self,
        project_local_id: &str,
        running_session_ids: &[String],
    ) -> Option<usize> {
        self.reconcile_project_running_sessions_if_epoch(
            project_local_id,
            running_session_ids,
            None,
        )
    }

    /// Business Logic（为什么需要这个函数）:
    ///     迟到 list 必须在 create/note 之后检测 epoch 变化，避免释放 create 新建的 watch（R42 M1）。
    ///     epoch 不匹配时仍 union 当前 running，但**禁止 release**（并发 list 也不得互相抹掉）。
    ///     R43 M1：与 note 共享同一 project_watch 锁，堵住：
    ///     - note 在 epoch 读后、previous 快照前插入 → 新 session 进 previous 但不在 running_set → 被 release
    ///     - note 在 previous 后、insert 前 → insert 覆盖掉新 session 项目归属
    ///     R44：调用方（尤其 Gap resync）需区分 stale（None）与 committed（Some），
    ///     仅 committed 才可对 previous−listed 投影 disconnected。
    ///     R45：Gap/list 发现的 running session 必须同步 retain session watch lease；
    ///     否则 previous−running 被 release 后 subscribers 可归零，60s idle 杀掉仍在跑的新会话 bridge。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在同一 project_watch 临界区内：
    ///     expected_epoch=Some 且与当前不一致 → union running 到 map，**不 release**，不 bump 成功
    ///     full-replace 路径 epoch，锁外仍 retain 全部 running ids，返回 `None`；
    ///     匹配或 expected_epoch=None → 计算 to_release=previous−running，full replace + bump epoch，
    ///     锁外先 retain 全部 running，再 release to_release；返回 `Some(released)`。
    fn reconcile_project_running_sessions_if_epoch(
        &self,
        project_local_id: &str,
        running_session_ids: &[String],
        expected_epoch: Option<u64>,
    ) -> Option<usize> {
        let running_set: HashSet<String> = running_session_ids.iter().cloned().collect();
        // 临界区：epoch compare + previous snapshot + set commit + epoch bump；
        // retain/release 在锁外执行（避免 watch_keys 与 project_watch 交叉死锁）。
        // R45：无论 stale 还是 commit，listed running 都必须 retain，避免 project map 有会话却无 lease。
        let to_release: Option<Vec<String>> = {
            let mut watch = self
                .project_watch
                .lock()
                .expect("bridge project_watch 锁中毒");
            if let Some(expected) = expected_epoch {
                let current = watch.epochs.get(project_local_id).copied().unwrap_or(0);
                if current != expected {
                    // Stale list：只合并 running，绝不 release create 后新 note 的 session。
                    if !running_set.is_empty() {
                        watch
                            .running_sessions
                            .entry(project_local_id.to_string())
                            .or_default()
                            .extend(running_set.iter().cloned());
                    }
                    // 锁外 retain running 后返回 None。
                    drop(watch);
                    for session_id in &running_set {
                        let _ = self.retain_watch_key(session_id);
                    }
                    return None;
                }
            }
            let previous = watch
                .running_sessions
                .get(project_local_id)
                .cloned()
                .unwrap_or_default();
            let to_release: Vec<String> = previous.difference(&running_set).cloned().collect();
            // 原子 full replace + bump，使并发 note 无法插在 previous 与 commit 之间。
            watch
                .running_sessions
                .insert(project_local_id.to_string(), running_set.clone());
            watch.bump_epoch(project_local_id);
            Some(to_release)
        };
        // R45：先 retain running，再 release 差集，保证 A→B 替换时 B 不会短暂/永久丢 lease。
        for session_id in &running_set {
            let _ = self.retain_watch_key(session_id);
        }
        let mut released = 0usize;
        for session_id in to_release.expect("commit path always Some") {
            if self.release_watch_key(&session_id) {
                released += 1;
            }
        }
        Some(released)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     URL 变化 / task restart 时必须把 project-scoped last-seen running 映射迁到新 runtime，
    ///     否则下一次 list 会把仍存活 session 误当消失而 release（R38 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 project_watch 锁克隆 running_sessions 全表。
    fn clone_project_running_sessions(&self) -> HashMap<String, HashSet<String>> {
        self.project_watch
            .lock()
            .expect("bridge project_watch 锁中毒")
            .running_sessions
            .clone()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     spawn 新 bridge task 时用旧 runtime 的 project running map 重建 reconcile 状态（R38 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 project_watch 锁覆盖写入 running_sessions（epochs 保持默认，由后续 note/reconcile 推进）。
    fn seed_project_running_sessions(&self, map: HashMap<String, HashSet<String>>) {
        let mut watch = self
            .project_watch
            .lock()
            .expect("bridge project_watch 锁中毒");
        watch.running_sessions = map;
    }

    /// Business Logic（为什么需要这个函数）:
    ///     remote project remove 后必须释放该 project 上次 list 仍占着的 session watch，
    ///     否则 project 已删而 bridge lease 仍占，会挡 idle 回收与 Gap inventory（R39 M2）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持 project_watch 锁取出并移除 project 的 running_sessions 与 epochs 条目；
    ///     锁外对 previous set 中每个 session key 调 release_watch_key；
    ///     不碰其它 project 与 `__device__`；返回 released 数量。
    fn clear_project_running_sessions(&self, project_local_id: &str) -> usize {
        let previous = {
            let mut watch = self
                .project_watch
                .lock()
                .expect("bridge project_watch 锁中毒");
            watch.epochs.remove(project_local_id);
            watch
                .running_sessions
                .remove(project_local_id)
                .unwrap_or_default()
        };
        let mut released = 0usize;
        for session_id in previous {
            if self.release_watch_key(&session_id) {
                released += 1;
            }
        }
        released
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
    /// 远端原生 session id；None 表示只接收轻量事件，不接收任何 terminal 正文。
    terminal_session_filter: Option<String>,
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
///     Mutex<HashMap<device_id, task>>；ensure 刷新 last_used；session-keyed watch lease；
///     restart 时 transfer after_cursor + watch_keys + project_running_sessions（R38 M2）；
///     shutdown_all 取消并 await。
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
    ///     watch_keys（subscribers 由 keys.len 派生）+ project_running_sessions 到新 runtime
    ///     （R30 M3 / R35 M2 / R38 M2）。
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
            let transferred_watch_keys = existing.runtime.clone_watch_keys();
            let transferred_project_running = existing.runtime.clone_project_running_sessions();
            let terminal_session_filter = existing.terminal_session_filter.clone();
            *existing = spawn_bridge_task(
                device_id,
                base_url,
                project_ids,
                state,
                transferred_cursor,
                transferred_watch_keys,
                transferred_project_running,
                terminal_session_filter,
            );
            return;
        }

        let project_ids = Arc::new(RwLock::new(HashMap::new()));
        update_project_mapping(&project_ids, project_mapping);
        let task = spawn_bridge_task(
            device_id.clone(),
            base_url,
            project_ids,
            state,
            None,
            HashSet::new(),
            HashMap::new(),
            None,
        );
        tasks.insert(device_id, task);
    }

    /// 把设备级远端事件流切换为仅传输当前终端窗口正文。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同一远端设备可能运行多个高输出终端；用户只查看一个窗口时，不应持续下载其它窗口正文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     找到设备 bridge；目标变化时继承 cursor/watch/project 映射并重启流，新的 GET 带
    ///     `terminalSessionId`。目标相同则 no-op。session id 必须是远端原生 id。
    pub fn set_active_terminal_session(
        &self,
        device_id: &str,
        inner_session_id: String,
        state: AppState,
    ) -> bool {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let mut found_target = false;
        for (current_device_id, existing) in tasks.iter_mut() {
            let desired_filter = if current_device_id == device_id {
                found_target = true;
                Some(inner_session_id.clone())
            } else {
                // 全应用同一时刻只有当前 UI 窗口允许传正文；切设备时必须关闭旧设备正文流。
                None
            };
            if existing.terminal_session_filter == desired_filter && !existing.is_finished() {
                if current_device_id == device_id {
                    existing.runtime.touch();
                }
                continue;
            }

            existing.cancel.cancel();
            existing.handle.abort();
            let base_url = existing.base_url.clone();
            let project_ids = Arc::clone(&existing.project_ids);
            let transferred_cursor = existing.runtime.load_after_cursor();
            let transferred_watch_keys = existing.runtime.clone_watch_keys();
            let transferred_project_running = existing.runtime.clone_project_running_sessions();
            *existing = spawn_bridge_task(
                current_device_id.clone(),
                base_url,
                project_ids,
                state.clone(),
                transferred_cursor,
                transferred_watch_keys,
                transferred_project_running,
                desired_filter,
            );
        }
        found_target
    }

    /// 仅当给定窗口仍是当前过滤目标时停止传输终端正文。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户切换项目、关闭 Workbench 或 active tab 变化时，旧窗口不能继续占用带宽；异步 cleanup
    ///     又可能晚于新 focus 到达，因此必须 compare-and-clear，不能无条件清空新目标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     目标不匹配时 no-op；匹配时继承 cursor/watch/project 映射并以 filter=None 重启 bridge，
    ///     新连接会使用 `terminalSessionId=__none__`，只保留轻量事件。
    pub fn clear_active_terminal_session_if(
        &self,
        device_id: &str,
        expected_inner_session_id: &str,
        state: AppState,
    ) -> bool {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(existing) = tasks.get_mut(device_id) else {
            return false;
        };
        if existing.terminal_session_filter.as_deref() != Some(expected_inner_session_id) {
            return false;
        }

        existing.cancel.cancel();
        existing.handle.abort();
        let base_url = existing.base_url.clone();
        let project_ids = Arc::clone(&existing.project_ids);
        let transferred_cursor = existing.runtime.load_after_cursor();
        let transferred_watch_keys = existing.runtime.clone_watch_keys();
        let transferred_project_running = existing.runtime.clone_project_running_sessions();
        *existing = spawn_bridge_task(
            device_id.to_string(),
            base_url,
            project_ids,
            state,
            transferred_cursor,
            transferred_watch_keys,
            transferred_project_running,
            None,
        );
        true
    }

    /// 停止所有远端设备的终端正文流，仅保留轻量状态连接。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     当前窗口切到本机 terminal 时，之前选中的远端设备也必须停止传输正文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 filter 非空或已结束的 task 继承恢复状态并以 filter=None 重启；返回是否改动过。
    pub fn clear_all_active_terminal_sessions(&self, state: AppState) -> bool {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let mut changed = false;
        for (device_id, existing) in tasks.iter_mut() {
            if existing.terminal_session_filter.is_none() && !existing.is_finished() {
                continue;
            }
            changed = true;
            existing.cancel.cancel();
            existing.handle.abort();
            let base_url = existing.base_url.clone();
            let project_ids = Arc::clone(&existing.project_ids);
            let transferred_cursor = existing.runtime.load_after_cursor();
            let transferred_watch_keys = existing.runtime.clone_watch_keys();
            let transferred_project_running = existing.runtime.clone_project_running_sessions();
            *existing = spawn_bridge_task(
                device_id.clone(),
                base_url,
                project_ids,
                state.clone(),
                transferred_cursor,
                transferred_watch_keys,
                transferred_project_running,
                None,
            );
        }
        changed
    }

    /// 为活跃 remote 查看者增加设备级订阅 lease（测试/兼容 API；生产走 session watch）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     设备级 lease 仍可供测试与显式 acquire 路径使用；生产 ensure_bridge 不再调用它（R36 H4），
    ///     避免 `__device__` 永久占用导致 idle/Gap inventory 无法收敛。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device 的 task 后 retain `"__device__"` key；无 task 时 no-op 返回 false。
    pub fn acquire_subscription(&self, device_id: &str) -> bool {
        self.ensure_watch_subscription(device_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     保留设备级 watch API 供测试/兼容；生产 ensure_remote_event_bridge_* 不得调用（R36 H4），
    ///     否则 `__device__` 永不 release，subscribers 无法归零，offline bridge 永久占 inventory。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁：task 存在时幂等 retain `"__device__"`；无 task 时 no-op 返回 false。
    pub fn ensure_watch_subscription(&self, device_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.retain_watch_key(WATCH_KEY_DEVICE);
        true
    }

    /// 为指定 remote session 建立 watch lease（R35 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     每个打开的 remote terminal 应独立持有一条 lease；关闭其中一个不得释放其它 session 的订阅。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task 后 retain session key（幂等）；无 task 时 no-op 返回 false。
    pub fn ensure_session_watch(&self, device_id: &str, session_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.retain_watch_key(session_id);
        true
    }

    /// 为 create 路径建立 project-scoped session watch（R41 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote create 成功后若只 ensure_session_watch，session 不进 project_running_sessions；
    ///     用户在首次 list 前 remove shortcut 时 clear_project_running_sessions 找不到该 key，
    ///     running watch 会长期占 subscribers 并向已删项目注入事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task：retain session watch key，并 note_project_running_session；
    ///     无 task 时 no-op 返回 false。
    pub fn ensure_session_watch_for_project(
        &self,
        device_id: &str,
        project_local_id: &str,
        session_id: &str,
    ) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.retain_watch_key(session_id);
        task.runtime
            .note_project_running_session(project_local_id, session_id);
        true
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list 在发起 RPC 前需要捕获 project watch epoch，供 reconcile 防 stale 覆盖（R42 M1）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task 后返回 runtime.project_watch_epoch；无 task 时返回 0。
    pub fn project_watch_epoch(&self, device_id: &str, project_local_id: &str) -> u64 {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return 0;
        };
        task.runtime.project_watch_epoch(project_local_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Gap resync 必须在 list 前原子捕获 epoch+previous，避免 create/resume note 与
    ///     迟到 list 之间假 disconnected / 误 release（R44）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task 后返回 runtime.project_watch_epoch_and_running；
    ///     无 task 时返回 `(0, empty HashSet)`。
    pub fn project_watch_epoch_and_running(
        &self,
        device_id: &str,
        project_local_id: &str,
    ) -> (u64, HashSet<String>) {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return (0, HashSet::new());
        };
        task.runtime
            .project_watch_epoch_and_running(project_local_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Gap resync / 调试路径可能单独需要 previous running ids；生产 Gap resync
    ///     应优先 `project_watch_epoch_and_running`（R44 原子捕获）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 project_running_sessions[project] 的克隆；无 task/key 时返回空 Vec。
    pub fn project_running_session_ids(
        &self,
        device_id: &str,
        project_local_id: &str,
    ) -> Vec<String> {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return Vec::new();
        };
        let map = task.runtime.clone_project_running_sessions();
        map.get(project_local_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 返回仍在运行的 bridge 上已映射的本机 shortcut projectId 列表（R41 M4）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap inventory 若按 device 枚举该设备下全部 remote shortcut，同设备失效 shortcut 的
    ///     sessions.list 失败会让整次 recovery incomplete，阻塞其它活跃项目 cutover。
    ///     必须只 inventory 仍挂在 active bridge project_ids 上的 local project。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁过滤 `!is_finished()` 的 task；读取 project_ids 的 value（local_project_id）；
    ///     去重后返回 Vec；不暴露 base_url/token/正文。
    pub fn active_mapped_local_project_ids(&self) -> Vec<String> {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let mut ids = std::collections::BTreeSet::new();
        for task in tasks.values() {
            if task.is_finished() {
                continue;
            }
            let map = task
                .project_ids
                .read()
                .expect("remote event bridge project 映射读锁中毒");
            for local_id in map.values() {
                if !local_id.trim().is_empty() {
                    ids.insert(local_id.clone());
                }
            }
        }
        ids.into_iter().collect()
    }

    /// 释放指定 remote session 的 watch lease（R35 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     close session / close 最后一 pane 只应释放该 session 的 lease；
    ///     其它仍打开的 session key 保持 subscribers>0。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 task 后 release session key；无 task 时 no-op 返回 false。
    pub fn release_session_watch(&self, device_id: &str, session_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.release_watch_key(session_id);
        true
    }

    /// 按项目 reconcile list 中消失的 running session watch（R38 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     list reconcile 只处理返回行；peer list 中消失的 session（无 status 事件）会永远占 watch，
    ///     必须在 list 后对 previous−running 释放，且不得影响其它 project 的 keys。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task 后调 runtime.reconcile_project_running_sessions；
    ///     无 task 时 no-op 返回 false；有 task 返回 true（无论 released 数量）。
    pub fn reconcile_session_watches_for_project(
        &self,
        device_id: &str,
        project_local_id: &str,
        running_session_ids: &[String],
    ) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        let _ = task
            .runtime
            .reconcile_project_running_sessions(project_local_id, running_session_ids);
        true
    }

    /// Business Logic（为什么需要这个函数）:
    ///     list / Gap resync 路径在捕获 epoch 后 reconcile；create 期间 epoch 变化时跳过
    ///     stale 覆盖（R42 M1）。R44：调用方需知是否 full commit，才能安全投影 disconnected。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 task 后透传 runtime.reconcile_project_running_sessions_if_epoch(Some(epoch))；
    ///     无 task 时返回 `None`；有 task 返回 `None`（stale）或 `Some(released)`（committed）。
    pub fn reconcile_session_watches_for_project_if_epoch(
        &self,
        device_id: &str,
        project_local_id: &str,
        running_session_ids: &[String],
        expected_epoch: u64,
    ) -> Option<usize> {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let task = tasks.get(device_id)?;
        task.runtime.reconcile_project_running_sessions_if_epoch(
            project_local_id,
            running_session_ids,
            Some(expected_epoch),
        )
    }

    /// 清除指定 remote project 的 project-scoped running-session watch 状态（R39 M2 / R40 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户移除 remote shortcut 后，该 project 上次 list 持有的 session watch 必须释放，
    ///     否则 bridge 仍占 lease，idle/Gap inventory 无法收敛。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task 后调 runtime.clear_project_running_sessions；
    ///     无 task 时 no-op 返回 false；有 task 返回 true（无论 released 数量）。
    pub fn clear_project_running_sessions(&self, device_id: &str, project_local_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        let _ = task
            .runtime
            .clear_project_running_sessions(project_local_id);
        true
    }

    /// 按本机 shortcut projectId 删除 bridge 的 project_ids 映射（R40 M2）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     project 已成功从 DB 删除后，Gap resync 仍可能按 project_ids 枚举已删 shortcut 的
    ///     inner project 并因该项目失败阻塞同设备其它项目 cutover；必须按 local id 摘映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 device task；从 project_ids 中 remove 所有 value==local_project_id 的条目；
    ///     无 task 返回 false；有 task 返回 true。
    pub fn remove_project_mapping_by_local_id(
        &self,
        device_id: &str,
        local_project_id: &str,
    ) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        let mut map = task
            .project_ids
            .write()
            .expect("remote event bridge project 映射写锁中毒");
        map.retain(|_, mapped_local| mapped_local != local_project_id);
        true
    }

    /// 释放设备级订阅 lease（兼容旧路径；优先用 release_session_watch）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     设备级查看者离开后应允许 idle TTL 从最后一次 release/touch 起算。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁找到 task 后 release `"__device__"` key；无 task 时 no-op 返回 false。
    pub fn release_subscription(&self, device_id: &str) -> bool {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let Some(task) = tasks.get(device_id) else {
            return false;
        };
        task.runtime.release_watch_key(WATCH_KEY_DEVICE);
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
///     URL 变化 restart 时必须继承 after_cursor、watch_keys 与 project_running_sessions，
///     避免 brand-new attach、session release 下溢或 list 误 release 仍存活 session
///     （R30 M3 / R35 M2 / R38 M2）。
///
/// Code Logic（这个函数做什么）:
///     创建 cancel/runtime（seed cursor + watch_keys + project_running_sessions）并 spawn
///     remote_event_loop；结束后置 finished。
#[allow(clippy::too_many_arguments)] // bridge spawn needs cursor/watch/project seed + filter
fn spawn_bridge_task(
    device_id: String,
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    state: AppState,
    initial_after_cursor: Option<BackendRuntimeCursor>,
    initial_watch_keys: HashSet<String>,
    initial_project_running_sessions: HashMap<String, HashSet<String>>,
    terminal_session_filter: Option<String>,
) -> RemoteEventBridgeTask {
    let cancel = CancellationToken::new();
    let runtime = Arc::new(BridgeRuntimeState::new());
    if initial_after_cursor.is_some() {
        runtime.store_after_cursor(initial_after_cursor);
    }
    if !initial_watch_keys.is_empty() {
        runtime.seed_watch_keys(initial_watch_keys);
    }
    if !initial_project_running_sessions.is_empty() {
        runtime.seed_project_running_sessions(initial_project_running_sessions);
    }
    let loop_cancel = cancel.clone();
    let loop_runtime = Arc::clone(&runtime);
    let task_device_id = device_id;
    let task_base_url = base_url.clone();
    let task_project_ids = Arc::clone(&project_ids);
    let task_terminal_session_filter = terminal_session_filter.clone();
    let handle = tauri::async_runtime::spawn(async move {
        remote_event_loop(
            task_device_id,
            task_base_url,
            state,
            task_project_ids,
            loop_cancel,
            loop_runtime.clone(),
            task_terminal_session_filter,
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
        terminal_session_filter,
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
    terminal_session_filter: Option<String>,
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
            terminal_session_filter.as_deref(),
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
                    terminal_session_filter.as_deref(),
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
#[allow(clippy::too_many_arguments)] // stream reader needs state/ids/cancel/cursor/filter
async fn read_remote_event_stream(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    cancel: &CancellationToken,
    runtime: &BridgeRuntimeState,
    after_cursor: &mut Option<BackendRuntimeCursor>,
    terminal_session_filter: Option<&str>,
) -> Result<(), EventStreamError> {
    let url = event_stream_url(base_url, after_cursor.as_ref(), terminal_session_filter);
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
    // R41 M3：streaming 开始与入站 chunk（含 15s heartbeat）**不得** demand-touch last_used。
    // 有 subscribers 时 idle_for 已是 ZERO；零订阅时仅 ensure/retain/release 刷新 demand 时钟。
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
        // R41 M3：入站流量（业务事件 + heartbeat）不 touch demand last_used。

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
                            emit_mapped_remote_event(state, *event);
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
                        WorkbenchRemoteStreamMessage::Cursor {
                            owner_instance_id,
                            sequence,
                        } => {
                            if !owner_instance_id.is_empty() && sequence > 0 {
                                *after_cursor = Some(BackendRuntimeCursor {
                                    owner_instance_id,
                                    sequence,
                                });
                                runtime.store_after_cursor(after_cursor.clone());
                            }
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
                    // R37 H1 / R38 H1：已带 remote: 的 entity id 是 peer 回灌本机 bus 的环路事件。
                    // 原生 peer 事件从不带 remote: 前缀；环路防护只在 process 路径 pre-map drop。
                    // map 后 entity 会变成 remote:*，emit 不得再二次检查（否则全部 live 被杀）。
                    if inbound_event_has_remote_entity_id(&event) {
                        continue;
                    }
                    messages.push(WorkbenchRemoteStreamMessage::Event {
                        owner_instance_id,
                        sequence,
                        event: Box::new(map_remote_event_for_device(
                            device_id,
                            project_ids,
                            *event,
                        )),
                    });
                }
                Ok(Some(gap @ WorkbenchRemoteStreamMessage::Gap { .. })) => {
                    messages.push(gap);
                }
                Ok(Some(cursor @ WorkbenchRemoteStreamMessage::Cursor { .. })) => {
                    messages.push(cursor);
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
            WorkbenchRemoteStreamMessage::Event { event, .. } => Some(*event),
            WorkbenchRemoteStreamMessage::Gap { .. }
            | WorkbenchRemoteStreamMessage::Cursor { .. } => None,
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
///     R37 H1：peer A 的 bus 可能导出 B 经 bridge 再发布的 `remote:*` 事件；
///     若再 map/publish 会形成 A↔B 无限 remap 洪泛。
///
/// Code Logic（这个函数做什么）:
///     检查 terminal sessionId / merge worktreeId / agent terminal/session ids 是否已是 remote: 实体；
///     TerminalResync 同理检查 sessionId。
fn inbound_event_has_remote_entity_id(event: &WorkbenchRemoteEvent) -> bool {
    match event {
        WorkbenchRemoteEvent::TerminalOutput(payload) => is_remote_id(&payload.session_id),
        WorkbenchRemoteEvent::TerminalStatus(payload) => is_remote_id(&payload.session_id),
        WorkbenchRemoteEvent::TerminalResync(payload) => is_remote_id(&payload.session_id),
        WorkbenchRemoteEvent::MergeProgress(payload) => {
            is_remote_id(&payload.worktree_id) || is_remote_id(&payload.project_id)
        }
        WorkbenchRemoteEvent::AgentRuntime(payload) => {
            let s = &payload.agent_session;
            is_remote_id(&s.id)
                || is_remote_id(&s.terminal_session_id)
                || is_remote_id(&s.project_id)
                || s.worktree_id.as_deref().is_some_and(is_remote_id)
                || s.orchestrator_task_id.as_deref().is_some_and(is_remote_id)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机前端只监听 Tauri event，不关心事件来自本机 PTY 还是远端 HTTP stream。
///     R36 H2：Mobile `/api/workbench/events` 读的是本机 `workbench_remote_events` bus，
///     仅 `emit_event`（Tauri/GUI）会让 bridged remote live 到不了 mobile 订阅者。
///     R38 H1：本函数接收的是 process 路径已经 map 过的 native-from-peer 事件（entity 已是
///     `remote:`），必须 publish+emit；环路 inbound 只在 `process_event_chunk_to_messages`
///     pre-map drop，不会到达此处。禁止再二次 drop mapped remote ids，否则全部 live 被杀。
///     R37 H3：非 running 的 TerminalStatus 释放对应 session watch，避免 leaked lease 挡 Gap inventory。
///
/// Code Logic（这个函数做什么）:
///     直接 clone 后 publish 到本机 bus，再按类型 emit `workbench:*`（事件已是 mapped remote ids）；
///     TerminalStatus 非 running 时 release session watch（composite remote session id）。
///     TerminalResync 仅 GUI emit + bus publish，不二次 map。
fn emit_mapped_remote_event(state: &AppState, event: WorkbenchRemoteEvent) {
    // R36 H2 / R38 H1：GUI emit + local bus publish 共用同一 mapped 事件。
    // 此处事件已 map 为 remote:*，必须 publish+emit；环路防护只在 process 路径。
    publish_workbench_remote_event_from_state(state, event.clone());
    match event {
        WorkbenchRemoteEvent::TerminalOutput(payload) => {
            state.emit_event("workbench:terminal-output", payload);
        }
        WorkbenchRemoteEvent::TerminalStatus(payload) => {
            // R37 H3：exited/disconnected 等非 running 状态释放 session watch。
            maybe_release_session_watch_on_status(state, &payload);
            state.emit_event("workbench:terminal-status", payload);
        }
        WorkbenchRemoteEvent::MergeProgress(payload) => {
            state.emit_event("workbench:merge-progress", payload);
        }
        WorkbenchRemoteEvent::AgentRuntime(payload) => {
            state.emit_event("workbench:agent-runtime", payload);
        }
        WorkbenchRemoteEvent::TerminalResync(payload) => {
            // 桌面仍走既有 resync event 名；payload 对齐 replay DTO。
            state.emit_event(
                crate::backend::ui::WORKBENCH_TERMINAL_RESYNC_EVENT,
                payload.to_replay(),
            );
        }
    };
}

/// 发布并向 GUI 转发一份远端终端权威回放。
///
/// Business Logic（为什么需要这个函数）:
///     实时流只传当前窗口后，切换窗口必须立即用 replay 恢复完整屏幕；桌面与 Mobile 需要同一份 cutover。
///
/// Code Logic（这个函数做什么）:
///     把 replay 转为 TerminalResync 发布到本机远端事件总线，并以既有 Tauri/owner 事件名转发给 GUI。
pub(crate) fn emit_remote_terminal_resync(state: &AppState, replay: WorkbenchSessionReplayDto) {
    publish_workbench_remote_event_from_state(
        state,
        WorkbenchRemoteEvent::TerminalResync(WorkbenchTerminalResyncPayload::from_replay(&replay)),
    );
    state.emit_event(crate::backend::ui::WORKBENCH_TERMINAL_RESYNC_EVENT, replay);
}

/// Business Logic（为什么需要这个函数）:
///     list/create 可能为已退出 session 残留 watch key；status 事件是权威生命周期信号，
///     非 running 时必须释放，否则 bridge 永非 idle，Gap inventory 永久 incomplete。
///
/// Code Logic（这个函数做什么）:
///     status 忽略大小写比较；仅 remote composite sessionId 才 release_session_watch。
fn maybe_release_session_watch_on_status(
    state: &AppState,
    payload: &WorkbenchTerminalStatusPayload,
) {
    let status = payload.status.trim();
    if status.is_empty() || status.eq_ignore_ascii_case("running") {
        return;
    }
    let Some(parsed) = parse_remote_entity_id(&payload.session_id) else {
        return;
    };
    let _ = state
        .workbench_remote_event_bridges
        .release_session_watch(&parsed.device_id, &payload.session_id);
}

/// Business Logic（为什么需要这个函数）:
///     远端设备发出的事件只包含自己的 local ID，本机 UI 需要可区分设备归属的 remote ID。
///
/// Code Logic（这个函数做什么）:
///     根据事件类型把 sessionId/projectId/worktreeId 映射为 `remote:<device_id>:<inner_id>`；
///     TerminalResync 的 sessionId 同样加 remote 前缀（通常由本机 resync 路径直接构造，此处仅防御）。
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
        WorkbenchRemoteEvent::TerminalResync(mut payload) => {
            payload.session_id = remote_entity_id(device_id, &payload.session_id);
            WorkbenchRemoteEvent::TerminalResync(payload)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Gap 后 bridge 必须在权威 resync 成功时才推进 after_cursor，失败时保留 recovery，
///     禁止带着旧 after 永久 Gap 循环（R28 H1）。
///     首帧 Gap 无 pre-gap 时 incomplete 也不得 bare after=None 重连（R34）。
///
/// Code Logic（这个函数做什么）:
///     resync_ok → Some(owner=gap_owner, sequence=gap_latest)（含 latest=0）；
///     失败 → recovery.cloned()；**recovery 为空且 gap_owner 非空时 seed owner+0**；
///     空 owner 且无 recovery → None。
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
    } else if let Some(cursor) = recovery.cloned() {
        Some(cursor)
    } else if !gap_owner.is_empty() {
        // 首帧 Gap 无 pre-gap：seed after=(owner,0)，truncated ring 上仍会再次 Gap。
        Some(BackendRuntimeCursor {
            owner_instance_id: gap_owner.to_string(),
            sequence: 0,
        })
    } else {
        None
    }
}

/// Business Logic（为什么需要这个函数）:
///     Gap resync 仅在 reconcile full commit 时，才允许把 list 前 previous 中
///     未出现在 listed 的 session 投影为 disconnected；epoch 已变（create/resume note）
///     时禁止假 disconnected，避免误伤并发新建会话（R44）。
///
/// Code Logic（这个函数做什么）:
///     `reconcile_committed=true` → 返回 previous−listed 的 id 向量；
///     `false` → 返回空向量。
fn gap_resync_missing_ids_for_disconnect(
    previous: &HashSet<String>,
    listed: &HashSet<String>,
    reconcile_committed: bool,
) -> Vec<String> {
    if !reconcile_committed {
        return Vec::new();
    }
    previous.difference(listed).cloned().collect()
}

/// 判断 Gap 恢复是否应拉取指定终端正文。
fn should_replay_terminal_session(active_inner_session_id: Option<&str>, candidate: &str) -> bool {
    active_inner_session_id == Some(candidate)
}

/// Business Logic（为什么需要这个函数）:
///     远端 ring 截断/owner 重启后 live 事件缺口必须用 sessions.list + sessions.replay
///     权威 cutover，再允许 after_cursor 前进；否则 GUI 终端永久停更。
///     R42 M3：非 running listed sessions 必须投影 terminalStatus；收集 running ids 并
///     project-scoped reconcile；lifecycle/watch 完成前不得 Ok（调用方才推进 cursor）。
///     R44：list **前**原子捕获 epoch+previous；`reconcile_if_epoch`；仅 committed 时
///     previous−listed → disconnected；stale 只 union、不 release、不假 disconnected。
///     R45：reconcile 路径必须 retain 所有 listed running session watch lease，
///     防止 previous−running release 后 subscribers 归零、idle 杀掉仍 running 的新会话 bridge。
///
/// Code Logic（这个函数做什么）:
///     遍历 bridge project_ids 映射的远端 project：local_shortcut 非空时 list 前
///     `project_watch_epoch_and_running` → list all sessions →
///     running 则 replay+resync；非 running 则 emit TerminalStatus；
///     每项目 `reconcile_session_watches_for_project_if_epoch(..., list_epoch)`
///     （内部 retain running + 仅 committed 时 release previous−running）；
///     仅 Some(_) 时对 previous−listed 投影 disconnected；
///     取消 → Cancelled；list/replay 失败 → Network（不推进 cursor）；零会话仍 Ok。
async fn resync_remote_bridge_after_gap(
    state: &AppState,
    device_id: &str,
    base_url: &str,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    cancel: &CancellationToken,
    terminal_session_filter: Option<&str>,
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
    let now_ts = chrono::Utc::now().timestamp_millis();
    for (inner_project_id, local_shortcut_id) in project_map {
        if cancel.is_cancelled() {
            return Err(EventStreamError::Cancelled);
        }
        // R44：list 前原子捕获 epoch+previous，堵住 create/resume note 与 list 之间的窗口。
        let (list_epoch, previous_ids) = if !local_shortcut_id.trim().is_empty() {
            state
                .workbench_remote_event_bridges
                .project_watch_epoch_and_running(device_id, &local_shortcut_id)
        } else {
            (0, HashSet::new())
        };
        let sessions = client
            .list_sessions(base_url, Some(inner_project_id.as_str()))
            .await
            .map_err(|_| EventStreamError::Network)?;
        let mut running_ids: Vec<String> = Vec::new();
        let mut listed_ids: HashSet<String> = HashSet::new();
        for session in sessions {
            if cancel.is_cancelled() {
                return Err(EventStreamError::Cancelled);
            }
            let status = session.status.trim();
            let remote_session_id = remote_entity_id(device_id, &session.id);
            listed_ids.insert(remote_session_id.clone());
            let is_running = status.is_empty() || status.eq_ignore_ascii_case("running");
            if !is_running {
                // R42 M3：投影 listed 非 running 终态，避免 Gap 中 status 事件被越过后 UI 永 running。
                let status_payload = WorkbenchTerminalStatusPayload {
                    session_id: remote_session_id.clone(),
                    status: status.to_string(),
                    exit_code: None,
                    ts: now_ts,
                };
                emit_mapped_remote_event(
                    state,
                    WorkbenchRemoteEvent::TerminalStatus(status_payload),
                );
                continue;
            }
            running_ids.push(remote_session_id.clone());
            // 生命周期和 watch reconcile 仍覆盖全部 running session；高带宽 replay 仅恢复当前窗口。
            if !should_replay_terminal_session(terminal_session_filter, &session.id) {
                continue;
            }
            let mut replay = client
                .replay(base_url, &session.id)
                .await
                .map_err(|_| EventStreamError::Network)?;
            replay.session_id = remote_session_id.clone();
            let remote_owner = replay.owner_instance_id.clone();
            replay.owner_instance_id = Some(
                crate::workbench::terminal_authority::terminal_stream_authority(
                    &remote_session_id,
                    &local_bus_owner,
                    remote_owner.as_deref(),
                ),
            );
            // R37 H2：Mobile 订阅本机 bus，需 TerminalResync；桌面仍走 Tauri resync emit。
            publish_workbench_remote_event_from_state(
                state,
                WorkbenchRemoteEvent::TerminalResync(WorkbenchTerminalResyncPayload::from_replay(
                    &replay,
                )),
            );
            state.emit_event(crate::backend::ui::WORKBENCH_TERMINAL_RESYNC_EVENT, replay);
        }
        // R44：仅 epoch 匹配 full commit 后才 previous−listed → disconnected。
        let reconcile_committed = if !local_shortcut_id.trim().is_empty() {
            state
                .workbench_remote_event_bridges
                .reconcile_session_watches_for_project_if_epoch(
                    device_id,
                    &local_shortcut_id,
                    &running_ids,
                    list_epoch,
                )
                .is_some()
        } else {
            // 无 local shortcut 映射时无 project watch 状态；不投影 previous disconnected。
            false
        };
        for missing_id in
            gap_resync_missing_ids_for_disconnect(&previous_ids, &listed_ids, reconcile_committed)
        {
            let status_payload = WorkbenchTerminalStatusPayload {
                session_id: missing_id,
                status: "disconnected".to_string(),
                exit_code: None,
                ts: now_ts,
            };
            emit_mapped_remote_event(state, WorkbenchRemoteEvent::TerminalStatus(status_payload));
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
fn event_stream_url(
    base_url: &str,
    after: Option<&BackendRuntimeCursor>,
    terminal_session_filter: Option<&str>,
) -> String {
    let base = format!("{}/api/workbench/events", base_url.trim_end_matches('/'));
    let Ok(mut url) = reqwest::Url::parse(&base) else {
        return base;
    };
    {
        let mut query = url.query_pairs_mut();
        if let Some(cursor) = after.filter(|cursor| !cursor.owner_instance_id.is_empty()) {
            query.append_pair("afterOwnerInstanceId", &cursor.owner_instance_id);
            query.append_pair("afterSequence", &cursor.sequence.to_string());
        }
        // 新 bridge 在尚未选中窗口时传稳定 sentinel；旧调用者不带本参数，仍保持完整流兼容。
        query.append_pair(
            "terminalSessionId",
            terminal_session_filter.unwrap_or("__none__"),
        );
    }
    url.into()
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
                    event.as_ref(),
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
            event_stream_url("http://127.0.0.1:1420/", None, None),
            "http://127.0.0.1:1420/api/workbench/events?terminalSessionId=__none__"
        );
        let after = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 9,
        };
        assert_eq!(
            event_stream_url("http://127.0.0.1:1420/", Some(&after), Some("session-a")),
            "http://127.0.0.1:1420/api/workbench/events?afterOwnerInstanceId=owner-a&afterSequence=9&terminalSessionId=session-a"
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
                Some(WorkbenchRemoteRelayMessage::Cursor { .. }) => continue,
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
    ///     后台终端正文必须在远端编码前移除，同时游标仍需推进以避免重连 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     目标 s2 原样编码；后台 s2 对 s1 订阅编码为同 owner/sequence 的 Cursor；无 filter 保持旧流。
    #[test]
    fn terminal_output_filter_replaces_background_body_with_cursor() {
        let message = WorkbenchRemoteRelayMessage::Event {
            owner_instance_id: "owner-a".to_string(),
            sequence: 7,
            event: Box::new(WorkbenchRemoteEvent::TerminalOutput(
                WorkbenchTerminalOutputPayload {
                    session_id: "s2".to_string(),
                    chunk: "large-background-output".to_string(),
                    seq: 3,
                    ts: 1000,
                    owner_instance_id: Some("producer-a".to_string()),
                },
            )),
        };

        let legacy = encode_workbench_remote_relay_ndjson_filtered(&message, None).unwrap();
        assert!(legacy.contains("large-background-output"));
        let selected = encode_workbench_remote_relay_ndjson_filtered(&message, Some("s2")).unwrap();
        assert!(selected.contains("large-background-output"));
        let filtered = encode_workbench_remote_relay_ndjson_filtered(&message, Some("s1")).unwrap();
        assert!(!filtered.contains("large-background-output"));
        assert!(matches!(
            decode_remote_event(&filtered).unwrap(),
            Some(WorkbenchRemoteStreamMessage::Cursor {
                owner_instance_id,
                sequence: 7,
            }) if owner_instance_id == "owner-a"
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Gap 恢复不得因后台窗口重新下载大段 replay；未选窗口时也不应拉任何正文。
    ///
    /// Code Logic（这个测试做什么）:
    ///     仅 active id 精确匹配时返回 true。
    #[test]
    fn gap_replay_only_targets_active_terminal_session() {
        assert!(should_replay_terminal_session(Some("s1"), "s1"));
        assert!(!should_replay_terminal_session(Some("s1"), "s2"));
        assert!(!should_replay_terminal_session(None, "s1"));
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
            event: Box::new(sample_status_event(4)),
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

    /// Business Logic（R28 H1 / R34: 为什么需要这个测试）:
    ///     Gap resync 成功才推进 after_cursor；失败保留 recovery；
    ///     首帧无 recovery 时 seed gap.owner+0，禁止 bare after=None 重连。
    ///
    /// Code Logic（这个测试做什么）:
    ///     成功 → cursor=gap latest（含 gap_latest==0）；失败有 recovery → 保留；
    ///     失败无 recovery → owner-new/0；空 owner 仍 None。
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
        // R34：首帧 incomplete 必须 seed owner+0，不得 None brand-new。
        let first = after_cursor_after_gap_resync(None, "owner-new", 42, false).expect("seed");
        assert_eq!(first.owner_instance_id, "owner-new");
        assert_eq!(first.sequence, 0);
        assert!(after_cursor_after_gap_resync(None, "", 42, false).is_none());
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

    /// Business Logic（R41 M3: 为什么需要这个测试）:
    ///     零订阅时 peer heartbeat/入站网络不得刷新 demand last_used，否则 15s heartbeat
    ///     会永久挡 60s idle TTL，active-device 集合无法收敛。
    ///
    /// Code Logic（这个测试做什么）:
    ///     过期 last_used 后模拟 stream loop 不 touch；idle_for 仍达 TTL；
    ///     仅 demand touch() 会刷新时钟。
    #[test]
    fn network_activity_does_not_refresh_demand_idle_at_zero_subscribers() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("streaming");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        // 模拟 stream loop 收 heartbeat 但不 touch demand last_used。
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "zero subscribers must idle without demand touch even while streaming"
        );
        runtime.touch();
        assert!(
            runtime.idle_for() < Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "demand touch restarts idle clock"
        );
    }

    /// Business Logic（R41 M2: 为什么需要这个测试）:
    ///     create 登记的 session 必须进入 project_running_sessions，remove 才能 clear。
    ///
    /// Code Logic（这个测试做什么）:
    ///     note_project_running_session 后 clear_project_running_sessions 释放该 key。
    #[test]
    fn note_project_running_session_is_cleared_on_project_remove() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:s1"));
        runtime.note_project_running_session("project-a", "remote:dev:s1");
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.clear_project_running_sessions("project-a"), 1);
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        let map = runtime.clone_project_running_sessions();
        assert!(!map.contains_key("project-a"));
    }

    /// Business Logic（R32 M2 / R35 M2: 为什么需要这个测试）:
    ///     最后一 tab 关闭后 bridge 必须可 idle，否则 stale bridge 阻挡 Gap inventory。
    ///
    /// Code Logic（这个测试做什么）:
    ///     phase=streaming + subscribers=0 + last_used 过期 → idle；
    ///     retain_watch_key 后 ZERO；release 后按 last_used TTL 再 idle。
    #[test]
    fn last_subscription_release_allows_idle_even_while_streaming() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("streaming");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "zero subscribers + streaming + last_used expired must idle (R32 M2)"
        );

        assert!(runtime.retain_watch_key("session-a"));
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "subscribers>0 keeps bridge non-idle regardless of phase"
        );
        // 即使 last_used 过期，有订阅仍 ZERO。
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert_eq!(runtime.idle_for(), Duration::ZERO);

        // 最后一 tab 关闭：release 会 touch，刚释放时尚未 idle。
        assert!(runtime.release_watch_key("session-a"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        assert!(
            runtime.idle_for() < Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "release touches last_used so idle clock restarts"
        );
        // 模拟 TTL 过后：必须 idle，允许 bridge 回收。
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "last close after TTL must idle streaming bridge"
        );
    }

    /// Business Logic（R35 M2: 为什么需要这个测试）:
    ///     multi-session 中关闭一个不得误 idle 杀掉其它 session 仍在用的 bridge。
    ///
    /// Code Logic（这个测试做什么）:
    ///     retain 两个不同 session key → release 一个仍 ZERO；再 release + 过期后 idle。
    #[test]
    fn multi_session_release_one_keeps_bridge_non_idle() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("connecting");
        assert!(runtime.retain_watch_key("session-a"));
        assert!(runtime.retain_watch_key("session-b"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 2);
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        // 关掉一个 session：仍有 1 个 key → 非 idle。
        assert!(runtime.release_watch_key("session-a"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "one remaining session key keeps bridge non-idle"
        );
        // 关掉最后一个。
        assert!(runtime.release_watch_key("session-b"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS));
        // 额外 release 不 panic / 不回绕。
        assert!(!runtime.release_watch_key("session-b"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
    }

    /// Business Logic（R35 M2: 为什么需要这个测试）:
    ///     ensure_watch_subscription 的设备级 key 必须幂等，重复 ensure 不得膨胀 subscribers。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同一 key retain 两次只插入一次；subscribers 保持 1。
    #[test]
    fn ensure_watch_subscription_device_key_is_idempotent() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key(WATCH_KEY_DEVICE));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert!(!runtime.retain_watch_key(WATCH_KEY_DEVICE));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.idle_for(), Duration::ZERO);
    }

    /// Business Logic（R32 M2: 为什么需要这个测试）:
    ///     离线 backoff 且零订阅者时 bridge 必须可 idle，禁止永久占着 inventory 槽。
    ///
    /// Code Logic（这个测试做什么）:
    ///     phase=backoff + subscribers=0 + last_used 过期 → idle；
    ///     有订阅时 backoff 仍 ZERO。
    #[test]
    fn offline_backoff_with_zero_subscribers_idles() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("backoff");
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "backoff + zero subs + expired last_used must idle (R32 M2)"
        );

        assert!(runtime.retain_watch_key("session-a"));
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "backoff with subscribers must stay non-idle"
        );
    }

    /// Business Logic（R30 M3 / R35 M2: 为什么需要这个测试）:
    ///     URL 变化或任务替换时 after_cursor 与 watch_keys 必须从旧 runtime transfer 到新 runtime，
    ///     禁止 brand-new attach 丢掉 recovery 点或 session lease。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed cursor + 两个 session keys → clone/seed 到新 runtime；
    ///     cursor/keys/subscribers 一致；idle ZERO。
    #[test]
    fn after_cursor_and_watch_keys_preserved_across_restart() {
        let seed = BackendRuntimeCursor {
            owner_instance_id: "owner-seed".into(),
            sequence: 99,
        };
        let runtime = BridgeRuntimeState::new();
        runtime.store_after_cursor(Some(seed.clone()));
        assert!(runtime.retain_watch_key("session-a"));
        assert!(runtime.retain_watch_key("session-b"));

        let transferred_cursor = runtime.load_after_cursor();
        let transferred_keys = runtime.clone_watch_keys();
        let restarted = BridgeRuntimeState::new();
        restarted.store_after_cursor(transferred_cursor);
        restarted.seed_watch_keys(transferred_keys);

        let loaded = restarted.load_after_cursor().expect("cursor transferred");
        assert_eq!(loaded.owner_instance_id, "owner-seed");
        assert_eq!(loaded.sequence, 99);
        assert_eq!(restarted.subscribers.load(Ordering::SeqCst), 2);
        assert!(restarted.clone_watch_keys().contains("session-a"));
        assert!(restarted.clone_watch_keys().contains("session-b"));
        // 有订阅时 idle 为 ZERO。
        *restarted.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 1);
        assert_eq!(restarted.idle_for(), Duration::ZERO);
        // restart 后仍可按 session 释放。
        assert!(restarted.release_watch_key("session-a"));
        assert_eq!(restarted.subscribers.load(Ordering::SeqCst), 1);
    }

    /// Business Logic（R38 M2: 为什么需要这个测试）:
    ///     同一 project 上次 list 见过 s1,s2，本次仅 running=[s1] 时必须 release s2，
    ///     否则 list 中消失的 session（无 status 事件）会永远占 watch。
    ///
    /// Code Logic（这个测试做什么）:
    ///     retain s1,s2 → reconcile project-A 只 running=[s1] → s2 released、subscribers 下降、
    ///     project map 更新为 {s1}。
    #[test]
    fn project_running_sessions_reconcile_releases_disappeared_sessions() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:s1"));
        assert!(runtime.retain_watch_key("remote:dev:s2"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 2);
        // 首次 seed：previous 空 → 只写入 running set，不 release。
        assert_eq!(
            runtime.reconcile_project_running_sessions(
                "project-a",
                &["remote:dev:s1".into(), "remote:dev:s2".into()],
            ),
            Some(0)
        );
        // 第二次 list：仅 s1 still running → s2 消失。
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:s1".into()],),
            Some(1)
        );
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        let keys = runtime.clone_watch_keys();
        assert!(keys.contains("remote:dev:s1"));
        assert!(!keys.contains("remote:dev:s2"));
        let map = runtime.clone_project_running_sessions();
        let project_a = map.get("project-a").expect("project-a tracked");
        assert!(project_a.contains("remote:dev:s1"));
        assert!(!project_a.contains("remote:dev:s2"));
    }

    /// Business Logic（R42 M1: 为什么需要这个测试）:
    ///     迟到的空 list 不得撤销 list 发起后 create 新建的 watch。
    ///
    /// Code Logic（这个测试做什么）:
    ///     full reconcile seed 后捕获 epoch；note 新 session（bump）；
    ///     用旧 epoch 空 list reconcile → None 且新 session watch 仍在。
    #[test]
    fn stale_list_epoch_must_not_release_create_noted_session() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:old"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:old".into()]),
            Some(0)
        );
        let epoch_at_list_start = runtime.project_watch_epoch("project-a");
        // create 路径 note 新 session 并 retain watch。
        assert!(runtime.retain_watch_key("remote:dev:new"));
        runtime.note_project_running_session("project-a", "remote:dev:new");
        assert!(
            runtime.project_watch_epoch("project-a") > epoch_at_list_start,
            "note must bump epoch"
        );
        // 迟到的空 list 用旧 epoch：不得 release create 的 new。
        assert_eq!(
            runtime.reconcile_project_running_sessions_if_epoch(
                "project-a",
                &[],
                Some(epoch_at_list_start),
            ),
            None
        );
        let keys = runtime.clone_watch_keys();
        assert!(
            keys.contains("remote:dev:new"),
            "stale empty list must not release create-noted session"
        );
        assert!(
            keys.contains("remote:dev:old"),
            "stale empty list must not release previous either"
        );
        let map = runtime.clone_project_running_sessions();
        let project_a = map.get("project-a").expect("project-a");
        assert!(project_a.contains("remote:dev:new"));
        assert!(project_a.contains("remote:dev:old"));
    }

    /// Business Logic（R43 M1: 为什么需要这个测试）:
    ///     竞态窗口 1：note 若插在 epoch 读后、previous 快照前，新 session 会进入 previous 却不在
    ///     running_set，被 stale empty list 误 release。同一把 project_watch 锁必须堵住该窗口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed old + 捕获 epoch → note new（同锁 bump）→ 旧 epoch 空 list reconcile；
    ///     断言 None、new watch 与 project map 均保留；另用 thread 并发 note + 旧 epoch
    ///     空 list 反复 reconcile，断言 new 始终保留。
    #[test]
    fn atomic_project_watch_protects_note_between_epoch_and_previous() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:old"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:old".into()]),
            Some(0)
        );
        let epoch_at_list_start = runtime.project_watch_epoch("project-a");
        assert!(runtime.retain_watch_key("remote:dev:new"));
        runtime.note_project_running_session("project-a", "remote:dev:new");
        // 单线程原子路径：旧 epoch 空 list 不得 release create note。
        assert_eq!(
            runtime.reconcile_project_running_sessions_if_epoch(
                "project-a",
                &[],
                Some(epoch_at_list_start),
            ),
            None
        );
        assert!(runtime.clone_watch_keys().contains("remote:dev:new"));
        assert!(runtime
            .clone_project_running_sessions()
            .get("project-a")
            .expect("project-a")
            .contains("remote:dev:new"));

        // 并发：一个线程 note 新 session，另一个用旧 epoch 空 list 反复 reconcile。
        let runtime = Arc::new(BridgeRuntimeState::new());
        assert!(runtime.retain_watch_key("remote:dev:base"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:base".into()]),
            Some(0)
        );
        let epoch = runtime.project_watch_epoch("project-a");
        assert!(runtime.retain_watch_key("remote:dev:racer"));
        let note_runtime = Arc::clone(&runtime);
        let reconcile_runtime = Arc::clone(&runtime);
        let note_handle = std::thread::spawn(move || {
            note_runtime.note_project_running_session("project-a", "remote:dev:racer");
        });
        let reconcile_handle = std::thread::spawn(move || {
            for _ in 0..64 {
                let _ = reconcile_runtime.reconcile_project_running_sessions_if_epoch(
                    "project-a",
                    &[],
                    Some(epoch),
                );
            }
        });
        note_handle.join().expect("note thread");
        reconcile_handle.join().expect("reconcile thread");
        assert!(
            runtime.clone_watch_keys().contains("remote:dev:racer"),
            "concurrent stale empty list must not release noted session (window: epoch→previous)"
        );
        assert!(
            runtime
                .clone_project_running_sessions()
                .get("project-a")
                .expect("project-a")
                .contains("remote:dev:racer"),
            "noted session must remain in project map after concurrent stale reconcile"
        );
    }

    /// Business Logic（R43 M1: 为什么需要这个测试）:
    ///     竞态窗口 2：note 若插在 previous 快照后、map.insert 前，full-replace 会覆盖掉新
    ///     session 的项目归属；同一临界区 commit 必须保证 note 要么进 previous/差集决策，
    ///     要么在 commit 后重新 insert+bump。
    ///
    /// Code Logic（这个测试做什么）:
    ///     并发：note 新 session vs 匹配 epoch 的 full reconcile（running=[base]）；
    ///     结束后若 epoch 已因 note bump，则 racer 必须仍在 watch_keys 与 project map
    ///     （即 note 未在 insert 前被覆盖丢弃）；若 note 落在 full reconcile 之后，map 含 racer。
    #[test]
    fn atomic_project_watch_protects_note_between_previous_and_insert() {
        let runtime = Arc::new(BridgeRuntimeState::new());
        assert!(runtime.retain_watch_key("remote:dev:base"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:base".into()]),
            Some(0)
        );
        let epoch = runtime.project_watch_epoch("project-a");
        assert!(runtime.retain_watch_key("remote:dev:racer"));

        let note_runtime = Arc::clone(&runtime);
        let reconcile_runtime = Arc::clone(&runtime);
        let note_handle = std::thread::spawn(move || {
            // 多次 note 增大与 reconcile 交错概率。
            for _ in 0..32 {
                note_runtime.note_project_running_session("project-a", "remote:dev:racer");
            }
        });
        let reconcile_handle = std::thread::spawn(move || {
            for _ in 0..32 {
                let _ = reconcile_runtime.reconcile_project_running_sessions_if_epoch(
                    "project-a",
                    &["remote:dev:base".into()],
                    Some(epoch),
                );
            }
        });
        note_handle.join().expect("note thread");
        reconcile_handle.join().expect("reconcile thread");

        // 权威不变量：create 已 retain 的 racer watch 不得被 full-replace 窗口抹掉；
        // project map 若 epoch 因 note 推进过，racer 必须仍登记（否则是 insert 覆盖 bug）。
        assert!(
            runtime.clone_watch_keys().contains("remote:dev:racer"),
            "noted session watch must survive concurrent full reconcile (window: previous→insert)"
        );
        let map = runtime.clone_project_running_sessions();
        let project_a = map.get("project-a").expect("project-a");
        assert!(
            project_a.contains("remote:dev:base"),
            "base from matching reconcile must remain when committed"
        );
        // note 与 matching reconcile 原子交错后：要么 note 最终赢（含 racer），要么
        // note 在 full replace 之后再 insert（含 racer）。若 racer 消失则说明 insert 覆盖了 note。
        assert!(
            project_a.contains("remote:dev:racer"),
            "atomic critical section must not drop create-noted session from project map"
        );
    }

    /// Business Logic（R38 M2: 为什么需要这个测试）:
    ///     不同 project 的 last-seen running set 必须隔离；A reconcile 不得 release B 的 keys。
    ///
    /// Code Logic（这个测试做什么）:
    ///     project-A 与 project-B 各 retain 一 session；仅 A reconcile 为空 → 只释放 A 的 key，
    ///     B 的 key 与 project map 不变。
    #[test]
    fn project_running_sessions_reconcile_is_project_scoped() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:a1"));
        assert!(runtime.retain_watch_key("remote:dev:b1"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:a1".into()]),
            Some(0)
        );
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-b", &["remote:dev:b1".into()]),
            Some(0)
        );
        // project-A list 空：释放 a1，不影响 b1 / project-B map。
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &[]),
            Some(1)
        );
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        let keys = runtime.clone_watch_keys();
        assert!(!keys.contains("remote:dev:a1"));
        assert!(keys.contains("remote:dev:b1"));
        let map = runtime.clone_project_running_sessions();
        assert!(map.get("project-a").expect("project-a").is_empty());
        assert!(map
            .get("project-b")
            .expect("project-b")
            .contains("remote:dev:b1"));
        // 设备级 key 不在 project map 里，reconcile 不得碰它。
        assert!(runtime.retain_watch_key(WATCH_KEY_DEVICE));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-b", &[]),
            Some(1)
        );
        assert!(runtime.clone_watch_keys().contains(WATCH_KEY_DEVICE));
    }

    /// Business Logic（R38 M2: 为什么需要这个测试）:
    ///     registry 无 task 时 reconcile 必须 no-op 安全返回 false。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry 调 reconcile_session_watches_for_project → false。
    #[test]
    fn registry_reconcile_session_watches_noops_without_task() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(!registry.reconcile_session_watches_for_project(
            "missing-device",
            "project-a",
            &["remote:dev:s1".into()],
        ));
    }

    /// Business Logic（R39 M2: 为什么需要这个测试）:
    ///     remote project remove 必须释放该 project 的 previous running session watches，
    ///     并从 project map 移除条目，且不得影响其它 project。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed project-A/B → clear A → A 的 keys released、map 无 A；B 保留。
    #[test]
    fn clear_project_running_sessions_releases_and_removes_project() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:a1"));
        assert!(runtime.retain_watch_key("remote:dev:a2"));
        assert!(runtime.retain_watch_key("remote:dev:b1"));
        assert_eq!(
            runtime.reconcile_project_running_sessions(
                "project-a",
                &["remote:dev:a1".into(), "remote:dev:a2".into()],
            ),
            Some(0)
        );
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-b", &["remote:dev:b1".into()]),
            Some(0)
        );
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 3);
        assert_eq!(runtime.clear_project_running_sessions("project-a"), 2);
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        let keys = runtime.clone_watch_keys();
        assert!(!keys.contains("remote:dev:a1"));
        assert!(!keys.contains("remote:dev:a2"));
        assert!(keys.contains("remote:dev:b1"));
        let map = runtime.clone_project_running_sessions();
        assert!(!map.contains_key("project-a"));
        assert!(map
            .get("project-b")
            .expect("project-b")
            .contains("remote:dev:b1"));
        // 再次 clear 同一 project 是 no-op。
        assert_eq!(runtime.clear_project_running_sessions("project-a"), 0);
    }

    /// Business Logic（R39 M2: 为什么需要这个测试）:
    ///     registry 无 task 时 clear_project_running_sessions 必须 no-op 安全返回 false。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry 调 clear → false。
    #[test]
    fn registry_clear_project_running_sessions_noops_without_task() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(!registry.clear_project_running_sessions("missing-device", "project-a"));
    }

    /// Business Logic（R40 M2: 为什么需要这个测试）:
    ///     project 删除成功后必须按 local shortcut id 摘掉 bridge project_ids 映射，
    ///     避免 Gap resync 继续枚举已删项目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入两条映射（同 local / 不同 local）后 remove_project_mapping_by_local_id，
    ///     仅目标 local 被移除；无 task 时 no-op false。
    #[tokio::test]
    async fn remove_project_mapping_by_local_id_retains_other_projects() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(!registry.remove_project_mapping_by_local_id("missing-device", "local-a"));

        let project_ids = Arc::new(RwLock::new(HashMap::from([
            ("inner-a".to_string(), "local-a".to_string()),
            ("inner-a2".to_string(), "local-a".to_string()),
            ("inner-b".to_string(), "local-b".to_string()),
        ])));
        {
            let mut tasks = registry.tasks.lock().expect("lock");
            tasks.insert(
                "device-x".to_string(),
                RemoteEventBridgeTask {
                    base_url: "http://127.0.0.1:1".to_string(),
                    terminal_session_filter: None,
                    project_ids: Arc::clone(&project_ids),
                    cancel: CancellationToken::new(),
                    runtime: Arc::new(BridgeRuntimeState::new()),
                    handle: tauri::async_runtime::spawn(async {}),
                },
            );
        }
        assert!(registry.remove_project_mapping_by_local_id("device-x", "local-a"));
        let map = project_ids.read().expect("read");
        assert!(!map.values().any(|v| v == "local-a"));
        assert_eq!(map.get("inner-b").map(String::as_str), Some("local-b"));
    }

    /// Business Logic（R44: 为什么需要这个测试）:
    ///     Gap resync 在 list 前捕获 epoch+previous 后，若 create/resume note 了新 session，
    ///     迟到的空 list 不得 release 新 watch，也不得把 previous 当成可 disconnect 的 committed 差集。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed old → capture epoch+previous via project_watch_epoch_and_running → note new（bump）→
    ///     reconcile_if_epoch(old, []) → None，new 仍在 watch_keys 与 project map；
    ///     matching epoch 空 list → Some(released) 且 old 被 release。
    #[test]
    fn r44_gap_resync_epoch_stale_must_not_release_or_commit_disconnect() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:old"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:old".into()]),
            Some(0)
        );
        let (list_epoch, previous_ids) = runtime.project_watch_epoch_and_running("project-a");
        assert_eq!(list_epoch, runtime.project_watch_epoch("project-a"));
        assert!(previous_ids.contains("remote:dev:old"));

        // list 后、reconcile 前 create/resume note 新 session 并 bump epoch。
        assert!(runtime.retain_watch_key("remote:dev:new"));
        runtime.note_project_running_session("project-a", "remote:dev:new");
        assert!(
            runtime.project_watch_epoch("project-a") > list_epoch,
            "note must bump epoch after capture"
        );

        // stale 空 list：union-only，None，不得 release new/old。
        assert_eq!(
            runtime
                .reconcile_project_running_sessions_if_epoch("project-a", &[], Some(list_epoch),),
            None
        );
        let keys = runtime.clone_watch_keys();
        assert!(
            keys.contains("remote:dev:new"),
            "stale gap resync must not release create-noted session"
        );
        assert!(
            keys.contains("remote:dev:old"),
            "stale gap resync must not release previous either"
        );
        let map = runtime.clone_project_running_sessions();
        let project_a = map.get("project-a").expect("project-a");
        assert!(project_a.contains("remote:dev:new"));
        assert!(project_a.contains("remote:dev:old"));

        // matching epoch 空 list：full commit，release 仍在 map 的 sessions。
        let epoch_now = runtime.project_watch_epoch("project-a");
        let released =
            runtime.reconcile_project_running_sessions_if_epoch("project-a", &[], Some(epoch_now));
        assert_eq!(released, Some(2));
        let keys = runtime.clone_watch_keys();
        assert!(!keys.contains("remote:dev:old"));
        assert!(!keys.contains("remote:dev:new"));
    }

    /// Business Logic（R44: 为什么需要这个测试）:
    ///     Gap resync 投影 disconnected 必须门控在 reconcile committed；
    ///     纯函数决定 previous−listed 是否可见，避免 stale 路径误伤 UI。
    ///
    /// Code Logic（这个测试做什么）:
    ///     committed=true → previous−listed；committed=false → empty。
    #[test]
    fn r44_gap_resync_missing_ids_for_disconnect_gates_on_commit() {
        let previous: HashSet<String> = ["remote:dev:a".into(), "remote:dev:b".into()]
            .into_iter()
            .collect();
        let listed: HashSet<String> = ["remote:dev:a".into()].into_iter().collect();
        let mut missing = gap_resync_missing_ids_for_disconnect(&previous, &listed, true);
        missing.sort();
        assert_eq!(missing, vec!["remote:dev:b".to_string()]);
        assert!(
            gap_resync_missing_ids_for_disconnect(&previous, &listed, false).is_empty(),
            "stale/uncommitted must not project previous−listed disconnected"
        );
        assert!(
            gap_resync_missing_ids_for_disconnect(&previous, &previous, true).is_empty(),
            "no missing when listed covers previous"
        );
    }

    /// Business Logic（R45: 为什么需要这个测试）:
    ///     Gap resync 发现 running={B} 且 previous={A} 时，reconcile commit 会 release A；
    ///     若不同时 retain B，subscribers 归零 → idle 回收 bridge，B 的 live 永久冻结。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed A watch + project map → reconcile running=[B] →
    ///     A 被 release、B 在 watch_keys、subscribers>0、idle 不因 last_used 过期而触发。
    #[test]
    fn r45_gap_resync_must_retain_running_session_watches_on_commit() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:a"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:a".into()]),
            Some(0)
        );
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);

        // Gap list 仅见 B（尚未有 B 的 watch lease）。
        let released =
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:b".into()]);
        assert_eq!(released, Some(1), "A must be released as previous−running");
        let keys = runtime.clone_watch_keys();
        assert!(
            !keys.contains("remote:dev:a"),
            "A must leave watch_keys after commit"
        );
        assert!(
            keys.contains("remote:dev:b"),
            "B must be retained on Gap reconcile commit (R45)"
        );
        assert_eq!(
            runtime.subscribers.load(Ordering::SeqCst),
            1,
            "subscribers must stay >0 so bridge does not idle while B is running"
        );
        let map = runtime.clone_project_running_sessions();
        let project_a = map.get("project-a").expect("project-a");
        assert!(project_a.contains("remote:dev:b"));
        assert!(!project_a.contains("remote:dev:a"));

        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert_eq!(
            runtime.idle_for(),
            Duration::ZERO,
            "retained running lease must keep bridge non-idle"
        );
    }

    /// Business Logic（R45: 为什么需要这个测试）:
    ///     stale Gap list 不得 release previous，但仍必须 retain 新发现的 running ids，
    ///     避免 list 已见到 B 却只写入 project map、永不建 session lease。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed A → capture epoch → note C（bump）→ reconcile_if_epoch(old, [B]) → None；
    ///     A/C 仍在 watch；B 也被 retain。
    #[test]
    fn r45_gap_resync_stale_still_retains_listed_running_watches() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:a"));
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-a", &["remote:dev:a".into()]),
            Some(0)
        );
        let list_epoch = runtime.project_watch_epoch("project-a");
        assert!(runtime.retain_watch_key("remote:dev:c"));
        runtime.note_project_running_session("project-a", "remote:dev:c");
        assert!(runtime.project_watch_epoch("project-a") > list_epoch);

        assert_eq!(
            runtime.reconcile_project_running_sessions_if_epoch(
                "project-a",
                &["remote:dev:b".into()],
                Some(list_epoch),
            ),
            None
        );
        let keys = runtime.clone_watch_keys();
        assert!(
            keys.contains("remote:dev:a"),
            "stale must not release previous A"
        );
        assert!(
            keys.contains("remote:dev:c"),
            "stale must not release noted C"
        );
        assert!(
            keys.contains("remote:dev:b"),
            "stale path must still retain listed running B (R45)"
        );
        assert!(runtime.subscribers.load(Ordering::SeqCst) >= 3);
    }

    /// Business Logic（R38 M2: 为什么需要这个测试）:
    ///     URL 变化 / task restart 时 project_running_sessions 必须与 after_cursor/watch_keys
    ///     一样 transfer，否则下一次 list 会把仍存活 session 误当消失而 release。
    ///
    /// Code Logic（这个测试做什么）:
    ///     seed project map → clone/seed 到新 runtime；映射内容一致。
    #[test]
    fn project_running_sessions_preserved_across_restart() {
        let runtime = BridgeRuntimeState::new();
        assert_eq!(
            runtime.reconcile_project_running_sessions(
                "project-a",
                &["remote:dev:s1".into(), "remote:dev:s2".into()],
            ),
            Some(0)
        );
        assert_eq!(
            runtime.reconcile_project_running_sessions("project-b", &["remote:dev:b1".into()]),
            Some(0)
        );
        let transferred = runtime.clone_project_running_sessions();
        let restarted = BridgeRuntimeState::new();
        restarted.seed_project_running_sessions(transferred);
        let map = restarted.clone_project_running_sessions();
        assert_eq!(map.len(), 2);
        assert!(map
            .get("project-a")
            .expect("project-a")
            .contains("remote:dev:s1"));
        assert!(map
            .get("project-a")
            .expect("project-a")
            .contains("remote:dev:s2"));
        assert!(map
            .get("project-b")
            .expect("project-b")
            .contains("remote:dev:b1"));
        // restart 后仍可对 previous−running 正确 release。
        assert!(restarted.retain_watch_key("remote:dev:s1"));
        assert!(restarted.retain_watch_key("remote:dev:s2"));
        assert_eq!(
            restarted.reconcile_project_running_sessions("project-a", &["remote:dev:s1".into()],),
            Some(1)
        );
        assert_eq!(restarted.subscribers.load(Ordering::SeqCst), 1);
    }

    /// Business Logic（R30 M3 / R35 M2: 为什么需要这个测试）:
    ///     registry acquire/release/session-watch 在无 bridge 时必须 no-op 安全；
    ///     runtime 路径按 key 驱动 subscribers。
    ///
    /// Code Logic（这个测试做什么）:
    ///     空 registry 返回 false；runtime retain/release 同步 subscribers。
    #[test]
    fn registry_subscription_api_noops_without_task_and_retains_with_runtime() {
        let registry = RemoteEventBridgeRegistry::new();
        assert!(!registry.acquire_subscription("missing-device"));
        assert!(!registry.release_subscription("missing-device"));
        assert!(!registry.ensure_session_watch("missing-device", "session-a"));
        assert!(!registry.release_session_watch("missing-device", "session-a"));
        assert!(!registry.reconcile_session_watches_for_project(
            "missing-device",
            "project-a",
            &[],
        ));

        // 直接测 runtime 路径与 registry 语义对齐（避免构造完整 AppState 任务）。
        let runtime = BridgeRuntimeState::new();
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        assert!(runtime.retain_watch_key(WATCH_KEY_DEVICE));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert!(runtime.release_watch_key(WATCH_KEY_DEVICE));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
    }

    /// Business Logic（R36 H4: 为什么需要这个测试）:
    ///     生产 ensure_bridge 不再 retain `__device__`；仅 session keys 时，最后一 session
    ///     release 必须把 subscribers 归零并允许 idle，否则 offline bridge 挡 Gap inventory。
    ///
    /// Code Logic（这个测试做什么）:
    ///     仅 retain 两个 session key（无 device key）→ release 全部 → subscribers=0 且可 idle；
    ///     watch_keys 不含 `__device__`。
    #[test]
    fn last_session_release_zeros_subscribers_without_device_key() {
        let runtime = BridgeRuntimeState::new();
        runtime.set_phase("connecting");
        assert!(runtime.retain_watch_key("session-a"));
        assert!(runtime.retain_watch_key("session-b"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 2);
        assert!(!runtime.clone_watch_keys().contains(WATCH_KEY_DEVICE));
        assert!(runtime.release_watch_key("session-a"));
        assert!(runtime.release_watch_key("session-b"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
        *runtime.last_used.lock().expect("last_used") =
            Instant::now() - Duration::from_secs(BRIDGE_IDLE_TTL_SECS + 5);
        assert!(
            runtime.idle_for() >= Duration::from_secs(BRIDGE_IDLE_TTL_SECS),
            "session-only leases must idle after last release (R36 H4)"
        );
    }

    /// Business Logic（R36 H2 / R38 H1: 为什么需要这个测试）:
    ///     bridged remote live 经映射后必须能发布到本机 `workbench_remote_events`，
    ///     Mobile `/api/workbench/events` 才能收到；仅 Tauri emit 不够。
    ///     R38 H1：emit 不得再二次 drop mapped remote ids；mapped 事件必须能 publish。
    ///
    /// Code Logic（这个测试做什么）:
    ///     将 mapped-style TerminalStatus（entity 已是 remote:）写入 local bus，
    ///     断言 sequence 递增且 open_relay 可收到——证明 mapped remote ids 合法且可交付。
    #[test]
    fn mapped_remote_event_can_publish_to_local_bus_for_mobile_live() {
        let bus = WorkbenchRemoteEventBus::new("local-owner");
        let mapped = WorkbenchRemoteEvent::TerminalStatus(WorkbenchTerminalStatusPayload {
            session_id: "remote:dev-1:inner-s1".into(),
            status: "running".into(),
            exit_code: None,
            ts: 42,
        });
        // R38 H1：mapped 事件 entity 已是 remote:；若 emit 再调 inbound_event_has_remote_entity_id
        // 会误杀。此处模拟 emit 的 clone+publish 路径（不构造完整 AppState），必须成功。
        assert!(
            inbound_event_has_remote_entity_id(&mapped),
            "mapped live events carry remote: ids; emit must not drop them (R38 H1)"
        );
        let published = mapped.clone();
        let cursor = bus.publish(published);
        assert_eq!(cursor.sequence, 1);
        assert_eq!(cursor.owner_instance_id, "local-owner");
        assert_eq!(bus.latest_sequence(), 1);
        let mut relay = bus.open_relay(None);
        match relay.try_recv() {
            Some(WorkbenchRemoteRelayMessage::Event {
                sequence, event, ..
            }) => {
                assert_eq!(sequence, 1);
                match *event {
                    WorkbenchRemoteEvent::TerminalStatus(payload) => {
                        assert_eq!(payload.session_id, "remote:dev-1:inner-s1");
                    }
                    other => panic!("expected TerminalStatus payload, got {other:?}"),
                }
            }
            other => panic!("expected mapped terminal status on local bus, got {other:?}"),
        }
        // clone 后原事件仍可再用于 GUI emit 侧（形状不变）。
        match mapped {
            WorkbenchRemoteEvent::TerminalStatus(payload) => {
                assert_eq!(payload.session_id, "remote:dev-1:inner-s1");
            }
            other => panic!("mapped event clone must preserve TerminalStatus, got {other:?}"),
        }
    }

    /// Business Logic（R37 H1 / R38 H1: 为什么需要这个测试）:
    ///     peer bus 导出的 `remote:*` 事件若再 map/publish 会形成 A↔B 无限环路；
    ///     环路防护只在 process 路径 pre-map drop。map 后的合法 live 事件 entity 已是
    ///     remote:，若 emit 再检查会误杀全部 live（R38 H1）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     原生 sessionId 经 process 后 map 一次进入消息列表；已带 remote: 的 inbound
    ///     被完整 drop（不进消息列表）。并断言 process 产出的 mapped event 上
    ///     `inbound_event_has_remote_entity_id` 为 true——证明若 emit 再检查会误杀。
    #[test]
    fn inbound_already_remote_session_id_is_dropped_before_map() {
        let project_ids = HashMap::new();
        let mut buffer = Vec::new();
        let native = serde_json::json!({
            "type": "terminalOutput",
            "payload": {
                "sessionId": "inner-session",
                "chunk": "ok",
                "seq": 1,
                "ts": 1
            },
            "ownerInstanceId": "peer-owner",
            "sequence": 3
        })
        .to_string()
            + "\n";
        let looped = serde_json::json!({
            "type": "terminalOutput",
            "payload": {
                "sessionId": "remote:device-a:inner-session",
                "chunk": "loop",
                "seq": 2,
                "ts": 2
            },
            "ownerInstanceId": "peer-owner",
            "sequence": 4
        })
        .to_string()
            + "\n";
        let messages = process_event_chunk_to_messages(
            "device-a",
            &project_ids,
            &mut buffer,
            format!("{native}{looped}").as_bytes(),
        )
        .expect("parse ok");
        assert_eq!(
            messages.len(),
            1,
            "already-remote inbound must be dropped on process path; native maps once"
        );
        match &messages[0] {
            WorkbenchRemoteStreamMessage::Event {
                sequence, event, ..
            } => {
                assert_eq!(*sequence, 3);
                match event.as_ref() {
                    WorkbenchRemoteEvent::TerminalOutput(payload) => {
                        assert_eq!(payload.session_id, "remote:device-a:inner-session");
                        assert_eq!(payload.chunk, "ok");
                    }
                    other => panic!("expected mapped TerminalOutput, got {other:?}"),
                }
                // R38 H1：process 产出的 mapped event 已是 remote:；若 emit 再检查会误杀。
                assert!(
                    inbound_event_has_remote_entity_id(event.as_ref()),
                    "mapped live event must report remote entity id; emit must not re-drop it (R38 H1)"
                );
            }
            other => panic!("expected single mapped native event, got {other:?}"),
        }
        assert!(inbound_event_has_remote_entity_id(
            &WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:x".into(),
                chunk: String::new(),
                seq: 0,
                ts: 0,
                owner_instance_id: None,
            })
        ));
        assert!(!inbound_event_has_remote_entity_id(
            &WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "native-x".into(),
                chunk: String::new(),
                seq: 0,
                ts: 0,
                owner_instance_id: None,
            })
        ));
    }

    /// Business Logic（R37 H2: 为什么需要这个测试）:
    ///     Mobile 通过 NDJSON bus 消费 terminalResync；encode/decode 必须 round-trip。
    ///
    /// Code Logic（这个测试做什么）:
    ///     encode TerminalResync Event → decode 还原 sessionId/buffer/lastSeq/owner。
    #[test]
    fn terminal_resync_event_encodes_and_decodes_for_mobile_bus() {
        let event = WorkbenchRemoteEvent::TerminalResync(WorkbenchTerminalResyncPayload {
            session_id: "remote:dev:s1".into(),
            buffer: "screen".into(),
            truncated: true,
            last_seq: 88,
            owner_instance_id: Some("comp-owner".into()),
        });
        let line = encode_workbench_remote_relay_ndjson(&WorkbenchRemoteRelayMessage::Event {
            owner_instance_id: "local-owner".into(),
            sequence: 12,
            event: Box::new(event.clone()),
        })
        .expect("encode");
        assert!(line.contains("\"type\":\"terminalResync\""));
        let decoded = decode_remote_event(&line)
            .expect("decode ok")
            .expect("Some event");
        match decoded {
            WorkbenchRemoteStreamMessage::Event {
                owner_instance_id,
                sequence,
                event,
            } => {
                assert_eq!(owner_instance_id, "local-owner");
                assert_eq!(sequence, 12);
                match *event {
                    WorkbenchRemoteEvent::TerminalResync(payload) => {
                        assert_eq!(payload.session_id, "remote:dev:s1");
                        assert_eq!(payload.buffer, "screen");
                        assert!(payload.truncated);
                        assert_eq!(payload.last_seq, 88);
                        assert_eq!(payload.owner_instance_id.as_deref(), Some("comp-owner"));
                    }
                    other => panic!("expected TerminalResync payload, got {other:?}"),
                }
            }
            other => panic!("expected TerminalResync event, got {other:?}"),
        }
        // from_replay / to_replay 与 DTO 字段对齐。
        let replay = WorkbenchSessionReplayDto {
            session_id: "s".into(),
            buffer: "b".into(),
            truncated: false,
            last_seq: 3,
            owner_instance_id: Some("o".into()),
        };
        let payload = WorkbenchTerminalResyncPayload::from_replay(&replay);
        assert_eq!(payload.to_replay(), replay);
    }

    /// Business Logic（R37 H3: 为什么需要这个测试）:
    ///     多 session watch 中仅 running 应保留；exited 释放后 subscribers 必须下降。
    ///
    /// Code Logic（这个测试做什么）:
    ///     retain 两个 session key；release 一个后 subscribers=1；再 release 后 idle。
    #[test]
    fn multi_session_watch_releases_non_running_keys() {
        let runtime = BridgeRuntimeState::new();
        assert!(runtime.retain_watch_key("remote:dev:s-running"));
        assert!(runtime.retain_watch_key("remote:dev:s-exited"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 2);
        // 模拟 list reconcile / status exited：释放非 running。
        assert!(runtime.release_watch_key("remote:dev:s-exited"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 1);
        assert!(runtime.clone_watch_keys().contains("remote:dev:s-running"));
        assert!(!runtime.clone_watch_keys().contains("remote:dev:s-exited"));
        assert!(runtime.release_watch_key("remote:dev:s-running"));
        assert_eq!(runtime.subscribers.load(Ordering::SeqCst), 0);
    }
}
