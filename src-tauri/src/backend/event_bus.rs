//! backend/event_bus.rs — sidecar 有界事件总线与本机 relay 游标。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 进程不得拥有第二份 terminal/merge/transfer/runtime 事件源；sidecar 作为唯一 HeadlessOwner
//!     发布事件，GUI 通过 `(ownerInstanceId, sequence)` 游标做 afterSequence 重连、去重与 gap 恢复。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `RuntimeEventBus`：有界 broadcast + 有界 replay ring；`BackendRuntimeCursor` 与
//!     `RuntimeRelayMessage::{Event,Gap}`；`open_relay` 在 after 不同 owner、或同 owner 且
//!     `oldest > after_seq+1`（含 after_seq=0 的 truncated ring）时强制 Gap（不回放 partial ring）；
//!     连续边界 `oldest == after_seq+1` 不 Gap；`GuiEventRelayState` 在 owner 变化时重置、同 owner
//!     去重，并在 Gap/Lag 时要求 terminal replay + runtime snapshot resync；incomplete Gap 保留
//!     pre-gap recovery cursor，禁止以 `cursor=None` 重连成 brand-new consumer。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// 默认 replay ring 容量（事件条数）。
pub const DEFAULT_REPLAY_RING_CAPACITY: usize = 256;
/// 默认 live broadcast 容量。
pub const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// 后端运行时事件游标。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 必须用 owner 身份 + 单调 sequence 才能在重启/重连后正确去重与恢复，
///     避免把新 owner 的 sequence=1 误判为旧 owner 的重复。
///
/// Code Logic（这个结构做什么）:
///     camelCase 序列化：`ownerInstanceId` + `sequence`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendRuntimeCursor {
    pub owner_instance_id: String,
    pub sequence: u64,
}

/// 本机 control relay 消息（catch-up / live / gap）。
///
/// Business Logic（为什么需要这个枚举）:
///     GUI 需要区分业务事件与“游标落后 ring”的显式 Gap，以便先 snapshot/replay 再接 live。
///
/// Code Logic（这个枚举做什么）:
///     内部 tag `kind`：`event` 携带事件名与 payload；`gap` 携带 oldestAvailable/latest。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RuntimeRelayMessage {
    /// 带游标的业务事件。
    #[serde(rename = "event")]
    Event {
        owner_instance_id: String,
        sequence: u64,
        event: String,
        payload: Value,
    },
    /// 请求游标早于 ring 或 broadcast lag 后的显式缺口。
    #[serde(rename = "gap")]
    Gap {
        owner_instance_id: String,
        oldest_available: u64,
        latest: u64,
    },
}

impl RuntimeRelayMessage {
    /// 读取消息所属 owner。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 去重与 resync 决策都按 owner 维度分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 Event/Gap 上的 owner_instance_id。
    pub fn owner_instance_id(&self) -> &str {
        match self {
            Self::Event {
                owner_instance_id, ..
            }
            | Self::Gap {
                owner_instance_id, ..
            } => owner_instance_id,
        }
    }

    /// 若为 Event 则返回 sequence。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     同 owner 去重需要比较 sequence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Event → Some(sequence)；Gap → None。
    pub fn sequence(&self) -> Option<u64> {
        match self {
            Self::Event { sequence, .. } => Some(*sequence),
            Self::Gap { .. } => None,
        }
    }
}

/// ring 内单条事件。
#[derive(Debug, Clone)]
struct RingEntry {
    sequence: u64,
    event: String,
    payload: Value,
}

/// 总线内部可变状态。
#[derive(Debug)]
struct EventBusInner {
    next_sequence: u64,
    ring: VecDeque<RingEntry>,
    ring_capacity: usize,
}

/// sidecar 有界事件总线。
///
/// Business Logic（为什么需要这个结构）:
///     owner 进程需要同时支撑 live 订阅与断线后的有限 replay，并在 ring 无法覆盖时显式 Gap。
///
/// Code Logic（这个结构做什么）:
///     Mutex 保护 sequence/ring；broadcast::Sender 推送 live；catch-up 先订阅再读 ring 去重。
#[derive(Debug)]
pub struct RuntimeEventBus {
    owner_instance_id: String,
    inner: Mutex<EventBusInner>,
    tx: broadcast::Sender<RuntimeRelayMessage>,
}

impl RuntimeEventBus {
    /// 使用默认容量创建事件总线。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产 AppState 与 smoke harness 需要快速挂上标准容量总线。
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

    /// 使用指定容量创建事件总线。
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
            inner: Mutex::new(EventBusInner {
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
    ///     control 响应与 GUI 游标需要绑定同一 owner 身份。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 owner_instance_id。
    pub fn owner_instance_id(&self) -> &str {
        &self.owner_instance_id
    }

    /// 发布一条事件并返回新游标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     terminal/merge/transfer/runtime 事件必须在 sidecar 单调编号后才能被 GUI 去重；
    ///     并发 publisher 不得让 seq=N+1 先于 seq=N 交付，否则 live consumer 会推进游标
    ///     并把较晚到达的较早 sequence 当重复丢弃，且不产生 Gap。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在同一把 inner 锁内完成：分配 next_sequence、写入 ring（满则弹出最旧）、
    ///     `broadcast::Sender::send`；锁外仅构造返回的 cursor。发送与编号同临界区保证
    ///     交付顺序与 sequence 单调一致。
    pub fn publish(&self, event: &str, payload: Value) -> BackendRuntimeCursor {
        let mut inner = self.inner.lock().expect("event bus 锁中毒");
        let sequence = inner.next_sequence;
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        if inner.ring.len() >= inner.ring_capacity {
            inner.ring.pop_front();
        }
        inner.ring.push_back(RingEntry {
            sequence,
            event: event.to_string(),
            payload: payload.clone(),
        });
        let message = RuntimeRelayMessage::Event {
            owner_instance_id: self.owner_instance_id.clone(),
            sequence,
            event: event.to_string(),
            payload,
        };
        // 必须持锁发送：并发 publish 若解锁后再 send，seq=2 可先于 seq=1 交付。
        let _ = self.tx.send(message);
        BackendRuntimeCursor {
            owner_instance_id: self.owner_instance_id.clone(),
            sequence,
        }
    }

    /// 当前最新 sequence（0 表示尚未发布）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap/attach 需要知道 live 游标上沿。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `next_sequence - 1`（未发布时 0）。
    pub fn latest_sequence(&self) -> u64 {
        let inner = self.inner.lock().expect("event bus 锁中毒");
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
        let inner = self.inner.lock().expect("event bus 锁中毒");
        inner.ring.front().map(|e| e.sequence).unwrap_or(0)
    }

    /// 打开一个 catch-up + live relay 会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 重连时需要 afterSequence 回放；同 owner 游标与 ring 之间存在空洞必须 Gap；
    ///     **after_seq=0 且 ring 已截断（oldest>1）同样必须 Gap**，禁止把 truncated ring 当
    ///     brand-new 全量回放而静默丢失 pre-capacity 事件；
    ///     **owner 变化（after 携带不同 owner_instance_id）必须强制 Gap**，禁止只回放
    ///     新 owner 当前 ring 的尾部——新 owner 若在 GUI 重连前已发布超过 ring 容量的
    ///     事件，pre-capacity 事件会静默丢失，GUI 会误 attach 低 sequence 而不 resync。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 subscribe live，再在锁内计算 gap/replay，避免丢失窗口内新事件；
    ///     - after=None（brand-new consumer）：same_owner=false、after_seq=0，回放整个 ring 后接 live；
    ///     - after=Some(同 owner, seq)（含 seq=0）：当 `oldest > after_seq.saturating_add(1)` 时仅 pending Gap；
    ///       连续边界（after_seq+1 == oldest，如 after=0/oldest=1、after=2/oldest=3）不 Gap；
    ///       空 ring（oldest=0）与 after_seq=0 时 `0 > 1` 为假，不 Gap；
    ///     - after 不同 owner：无论 ring 是否为空/是否截断，仅 pending Gap，强制 GUI resync；
    ///     - 其它同 owner：从 after_seq 之后回放 ring 内 Event；
    ///     live 路径跳过已回放 sequence。
    pub fn open_relay(&self, after: Option<&BackendRuntimeCursor>) -> RuntimeEventRelay {
        let live_rx = self.tx.subscribe();
        let (pending, max_replayed) = {
            let inner = self.inner.lock().expect("event bus 锁中毒");
            let latest = inner.next_sequence.saturating_sub(1);
            let oldest = inner.ring.front().map(|e| e.sequence).unwrap_or(0);
            let mut pending = Vec::new();
            let mut max_replayed = 0_u64;

            let same_owner = after
                .map(|c| c.owner_instance_id == self.owner_instance_id)
                .unwrap_or(false);
            // owner 变化：强制 Gap（不走 after_seq=0 的 partial ring replay）。
            // brand-new（after=None）保持 after_seq=0 全量 ring 回放。
            let owner_changed = after.is_some() && !same_owner;
            let after_seq = if same_owner {
                after.map(|c| c.sequence).unwrap_or(0)
            } else {
                0
            };

            if owner_changed {
                // 不同 owner 的 after 游标：始终 Gap，强制 GUI terminal/runtime resync。
                // 空 ring 时 oldest_available=0，latest=0；有 ring 时与 truncated 同形态。
                pending.push(RuntimeRelayMessage::Gap {
                    owner_instance_id: self.owner_instance_id.clone(),
                    oldest_available: oldest,
                    latest,
                });
                max_replayed = latest;
            } else if same_owner && oldest > after_seq.saturating_add(1) {
                // R28 H2：同 owner 且 ring 最旧条严格晚于 after_seq+1 → Gap。
                // 覆盖 after_seq=0 且 oldest>1 的截断场景；连续边界 oldest==after_seq+1 不 Gap。
                pending.push(RuntimeRelayMessage::Gap {
                    owner_instance_id: self.owner_instance_id.clone(),
                    oldest_available: oldest,
                    latest,
                });
                max_replayed = latest;
            }

            if pending.is_empty() {
                for entry in inner.ring.iter() {
                    if entry.sequence > after_seq {
                        pending.push(RuntimeRelayMessage::Event {
                            owner_instance_id: self.owner_instance_id.clone(),
                            sequence: entry.sequence,
                            event: entry.event.clone(),
                            payload: entry.payload.clone(),
                        });
                        max_replayed = entry.sequence;
                    }
                }
            }

            (pending, max_replayed)
        };

        RuntimeEventRelay {
            owner_instance_id: self.owner_instance_id.clone(),
            pending: pending.into(),
            live_rx,
            skip_through_sequence: max_replayed,
        }
    }
}

/// 单次 relay 会话（先耗尽 catch-up，再收 live）。
///
/// Business Logic（为什么需要这个结构）:
///     control stream 与 smoke 都需要同一套 afterSequence 语义。
///
/// Code Logic（这个结构做什么）:
///     pending 队列 + broadcast receiver；lag 转为 Gap。
pub struct RuntimeEventRelay {
    owner_instance_id: String,
    pending: VecDeque<RuntimeRelayMessage>,
    live_rx: broadcast::Receiver<RuntimeRelayMessage>,
    skip_through_sequence: u64,
}

impl RuntimeEventRelay {
    /// 拉取下一条消息（catch-up 或 live）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方用异步循环消费 relay，直到取消。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先弹 pending；再 await live；Lagged → Gap；同 sequence 已回放则跳过。
    pub async fn recv(&mut self) -> Option<RuntimeRelayMessage> {
        loop {
            if let Some(msg) = self.pending.pop_front() {
                return Some(msg);
            }
            match self.live_rx.recv().await {
                Ok(msg) => {
                    if let RuntimeRelayMessage::Event { sequence, .. } = &msg {
                        if *sequence <= self.skip_through_sequence {
                            continue;
                        }
                        self.skip_through_sequence = *sequence;
                    }
                    return Some(msg);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let latest = self.skip_through_sequence;
                    // lag 后无法知道 ring 外丢失了什么，发出 Gap 让 GUI resync。
                    return Some(RuntimeRelayMessage::Gap {
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
    ///     smoke 需要在不挂死的情况下排空 catch-up。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 pending；再 `try_recv` live；Lagged → Gap。
    pub fn try_recv(&mut self) -> Option<RuntimeRelayMessage> {
        if let Some(msg) = self.pending.pop_front() {
            return Some(msg);
        }
        loop {
            match self.live_rx.try_recv() {
                Ok(msg) => {
                    if let RuntimeRelayMessage::Event { sequence, .. } = &msg {
                        if *sequence <= self.skip_through_sequence {
                            continue;
                        }
                        self.skip_through_sequence = *sequence;
                    }
                    return Some(msg);
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => {
                    let latest = self.skip_through_sequence;
                    return Some(RuntimeRelayMessage::Gap {
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

/// GUI 侧游标状态机对单条消息的动作。
///
/// Business Logic（为什么需要这个枚举）:
///     GUI 必须把“投递业务事件”和“先 terminal/runtime resync 再 attach”分开，避免 silent loss。
///
/// Code Logic（这个枚举做什么）:
///     Deliver / DropDuplicate / RequestResync。
#[derive(Debug, Clone, PartialEq)]
pub enum RelayClientAction {
    /// 向前端投递业务事件（原始 event 名 + payload + owner/sequence 游标）。
    Deliver {
        event: String,
        payload: Value,
        owner_instance_id: String,
        sequence: u64,
    },
    /// 同 owner 重复 sequence，丢弃。
    DropDuplicate,
    /// 需要 terminal replay + runtime snapshot 后以最新游标重新 attach。
    RequestResync {
        owner_instance_id: String,
        oldest_available: u64,
        latest: u64,
    },
}

/// GUI 进程内事件 relay 游标状态。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 保存 `(ownerInstanceId, sequence)`，owner 变化时重置，仅在同 owner 下去重。
///     incomplete Gap 不得以 `cursor=None` 重连，否则 owner 只回放 ring 且不再报告原 Gap，
///     造成永久 silent loss。
///
/// Code Logic（这个结构做什么）:
///     持有 live cursor、pre-gap recovery cursor 与 recovery_pending；
///     Gap 时保留 last committed 为 recovery 并清空 live；**首帧 Gap 无 pre-gap 时 seed
///     recovery = gap.owner + sequence 0**；incomplete 时 restore recovery；
///     complete attach 清 recovery；处理 Event/Gap 并产出动作列表。
#[derive(Debug, Clone, Default)]
pub struct GuiEventRelayState {
    cursor: Option<BackendRuntimeCursor>,
    /// Gap 前最后一次已提交游标；complete resync 前保留，供 incomplete 恢复重连。
    /// 首帧 Gap 无 live cursor 时为 gap.owner + sequence 0（禁止 after=None brand-new）。
    recovery_cursor: Option<BackendRuntimeCursor>,
    /// 已见 Gap 且尚未 complete attach；阻止 poll fallback 以 brand-new 方式 attach latest。
    recovery_pending: bool,
    /// 累计 resync 次数（测试/诊断）。
    pub resync_count: u64,
}

impl GuiEventRelayState {
    /// 当前 live 游标（重连 / catch-up afterSequence）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     重连 afterSequence 需要读出已提交游标；incomplete 后应是 pre-gap recovery，而非 None。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 live cursor 克隆。
    pub fn cursor(&self) -> Option<BackendRuntimeCursor> {
        self.cursor.clone()
    }

    /// 是否仍处于未完成的 Gap recovery。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     poll fallback 在无 live cursor 时若仍 pending recovery，不得 attach batch.latest
    ///     假装 brand-new consumer 已追平。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `recovery_pending` 标志。
    pub fn recovery_pending(&self) -> bool {
        self.recovery_pending
    }

    /// 处理一条 relay 消息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 重启不得当重复；Gap 必须触发 resync 而非 silent drop。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Gap → 若有 live cursor 则写入 recovery_cursor 并清空 live；**无 live 且尚无 recovery 时
    ///     seed gap.owner+0**（重复 Gap 不覆盖既有 recovery）；置 recovery_pending，RequestResync；
    ///     Event：owner 不同则重置后投递；同 owner sequence<=cursor 则 DropDuplicate；否则推进游标并 Deliver。
    pub fn on_message(&mut self, message: RuntimeRelayMessage) -> RelayClientAction {
        match message {
            RuntimeRelayMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            } => {
                // 有 live 已提交游标时覆盖 recovery；首帧无 live 且尚无 recovery 时 seed owner+0，
                // 禁止 incomplete 后以 after=None 重连成 brand-new（与 mobile R32 M1 对齐）。
                // 重复 Gap / incomplete 重试：保留原 pre-gap recovery，不覆盖。
                if let Some(committed) = self.cursor.take() {
                    self.recovery_cursor = Some(committed);
                } else if self.recovery_cursor.is_none() && !owner_instance_id.is_empty() {
                    self.recovery_cursor = Some(BackendRuntimeCursor {
                        owner_instance_id: owner_instance_id.clone(),
                        sequence: 0,
                    });
                }
                self.recovery_pending = true;
                self.resync_count = self.resync_count.saturating_add(1);
                RelayClientAction::RequestResync {
                    owner_instance_id,
                    oldest_available,
                    latest,
                }
            }
            RuntimeRelayMessage::Event {
                owner_instance_id,
                sequence,
                event,
                payload,
            } => {
                if let Some(cur) = &self.cursor {
                    if cur.owner_instance_id == owner_instance_id && sequence <= cur.sequence {
                        return RelayClientAction::DropDuplicate;
                    }
                    if cur.owner_instance_id != owner_instance_id {
                        // owner 变化：重置旧游标后接受新事件（sequence 可从 1 再起）。
                        self.cursor = None;
                        self.recovery_cursor = None;
                        self.recovery_pending = false;
                    }
                }
                self.cursor = Some(BackendRuntimeCursor {
                    owner_instance_id: owner_instance_id.clone(),
                    sequence,
                });
                RelayClientAction::Deliver {
                    event,
                    payload,
                    owner_instance_id,
                    sequence,
                }
            }
        }
    }

    /// incomplete Gap resync 后恢复 pre-gap 游标，供重连/catch-up 使用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     resync 失败/取消时若把 cursor 留在 None，外层重连会被 owner 当 brand-new consumer，
    ///     只回放 ring 且不再报原 Gap，导致永久 silent loss。首帧 Gap 同样必须 restore 到
    ///     gap.owner+0，使 afterSequence=0 仍能在 truncated ring 上再次触发 Gap。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 `recovery_cursor` 写回 live `cursor`（若存在）；`recovery_pending` 保持 true。
    pub fn restore_recovery_cursor(&mut self) {
        if let Some(recovery) = self.recovery_cursor.clone() {
            self.cursor = Some(recovery);
        }
    }

    /// resync 完成后 attach 到最新 live 游标（不投递历史）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap 后 GUI 先 snapshot/replay，再从 latest live 接上，避免重复历史。
    ///
    /// Code Logic（这个函数做什么）:
    ///     设置 cursor 为给定值，并清空 recovery_cursor / recovery_pending。
    pub fn attach_at(&mut self, cursor: BackendRuntimeCursor) {
        self.cursor = Some(cursor);
        self.recovery_cursor = None;
        self.recovery_pending = false;
    }
}

/// Gap 后 terminal + runtime 恢复的可观测结果。
///
/// Business Logic（为什么需要这个结构）:
///     smoke/诊断必须证明 resync 发生了真实副作用，而非仅状态机注释。
///
/// Code Logic（这个结构做什么）:
///     记录 terminal replay 与 runtime snapshot refresh 调用次数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GapResyncOutcome {
    /// 成功触发的 terminal session replay 次数。
    pub terminal_replay_count: u64,
    /// 成功触发的 runtime snapshot refresh 次数（含 emit 通知）。
    pub runtime_snapshot_refresh_count: u64,
}

/// 执行 Gap 后的 terminal replay + runtime snapshot 恢复（先恢复再 attach live）。
///
/// Business Logic（为什么需要这个函数）:
///     ring gap/lag 后不能 silent loss：必须先恢复终端 buffer 与 Orchestrator 快照，
///     再 attach latest 游标，避免后续 DropDuplicate 掩盖丢更新。
///
/// Code Logic（这个函数做什么）:
///     顺序调用两个 hook 闭包；单边失败记 warn 并计 0，不阻断另一侧；返回可观测计数。
pub async fn perform_gap_resync<FT, FR, FutT, FutR>(
    terminal_replay: FT,
    runtime_refresh: FR,
) -> GapResyncOutcome
where
    FT: FnOnce() -> FutT,
    FR: FnOnce() -> FutR,
    FutT: std::future::Future<Output = Result<u64, crate::error::AppError>>,
    FutR: std::future::Future<Output = Result<u64, crate::error::AppError>>,
{
    let terminal_replay_count = match terminal_replay().await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("gap resync: terminal replay 失败: {e}");
            0
        }
    };
    let runtime_snapshot_refresh_count = match runtime_refresh().await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("gap resync: runtime snapshot refresh 失败: {e}");
            0
        }
    };
    GapResyncOutcome {
        terminal_replay_count,
        runtime_snapshot_refresh_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn publish_assigns_monotonic_sequence_per_owner() {
        let bus = RuntimeEventBus::new("owner-a");
        let c1 = bus.publish("workbench:terminal-output", json!({"n": 1}));
        let c2 = bus.publish("workbench:terminal-output", json!({"n": 2}));
        assert_eq!(c1.sequence, 1);
        assert_eq!(c2.sequence, 2);
        assert_eq!(c1.owner_instance_id, "owner-a");
    }

    /// 并发 publisher 交付顺序必须与 sequence 单调一致。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     若 seq 分配在锁内、broadcast send 在锁外，seq=2 可先于 seq=1 交付；
    ///     live consumer 推进 skip 后会把 1 当重复丢弃且不产生 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先 open_relay 订阅 live，再 spawn 多个线程并发 publish；收集到的 Event
    ///     sequence 必须严格递增、无空洞（或仅在 lag 时见 Gap，本容量下应无 Gap）。
    #[test]
    fn concurrent_publish_delivers_monotonic_sequences() {
        use std::sync::Arc;
        use std::thread;

        let bus = Arc::new(RuntimeEventBus::with_capacity("owner-a", 256, 256));
        let mut relay = bus.open_relay(None);
        // 吃掉订阅时可能已有的 ring 快照（当前为空）。
        while relay.try_recv().is_some() {}

        let threads = 8usize;
        let per_thread = 32usize;
        let mut handles = Vec::with_capacity(threads);
        for t in 0..threads {
            let bus = Arc::clone(&bus);
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    let _ = bus.publish("e", json!({ "t": t, "i": i }));
                }
            }));
        }
        for h in handles {
            h.join().expect("publisher join");
        }

        let expected = (threads * per_thread) as u64;
        let mut sequences = Vec::with_capacity(expected as usize);
        // 全部 publish 完成后，live + catch-up 路径应能收齐全部 Event。
        // open_relay(None) 会先 ring catch-up 再 live；此处在 publish 后另开 relay 读 ring。
        let mut catch_up = bus.open_relay(None);
        while let Some(msg) = catch_up.try_recv() {
            match msg {
                RuntimeRelayMessage::Event { sequence, .. } => sequences.push(sequence),
                RuntimeRelayMessage::Gap { .. } => {
                    panic!("concurrent publish under capacity must not Gap")
                }
            }
        }
        assert_eq!(sequences.len() as u64, expected);
        for (idx, seq) in sequences.iter().enumerate() {
            assert_eq!(*seq, (idx as u64) + 1, "delivery order must match sequence");
        }
        // 实时订阅侧：任意已收到的前缀也必须单调（可能仍在 live 缓冲）。
        let mut last = 0u64;
        while let Some(msg) = relay.try_recv() {
            match msg {
                RuntimeRelayMessage::Event { sequence, .. } => {
                    assert!(
                        sequence > last,
                        "live delivery reordered: {sequence} after {last}"
                    );
                    last = sequence;
                }
                RuntimeRelayMessage::Gap { .. } => {
                    panic!("live concurrent path under capacity must not Gap")
                }
            }
        }
    }

    #[test]
    fn catch_up_after_sequence_replays_only_newer() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 16, 16);
        let c1 = bus.publish("e", json!(1));
        let _c2 = bus.publish("e", json!(2));
        let _c3 = bus.publish("e", json!(3));
        let mut relay = bus.open_relay(Some(&c1));
        let m1 = relay.try_recv().expect("seq2");
        let m2 = relay.try_recv().expect("seq3");
        assert!(matches!(m1, RuntimeRelayMessage::Event { sequence: 2, .. }));
        assert!(matches!(m2, RuntimeRelayMessage::Event { sequence: 3, .. }));
        assert!(relay.try_recv().is_none());
    }

    /// after_seq 严格早于 ring 且存在空洞（oldest > after_seq+1）必须 Gap。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     同 owner 游标落后超过 1 个 sequence 时 ring 无法补齐空洞，必须显式 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity=2 发布 1..4（oldest=3）；after=1 → 3 > 2 → Gap{oldest=3,latest=4}，无 Event。
    ///     注意 after=1/oldest=2 是连续边界，不再走本用例。
    #[test]
    fn cursor_before_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        let _ = bus.publish("e", json!(1));
        let _ = bus.publish("e", json!(2));
        let _ = bus.publish("e", json!(3));
        let _ = bus.publish("e", json!(4)); // ring keeps 3,4; oldest=3
        let stale = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 1, // oldest 3 > 1+1 → gap
        };
        let mut relay = bus.open_relay(Some(&stale));
        let msg = relay.try_recv().expect("gap");
        match msg {
            RuntimeRelayMessage::Gap {
                oldest_available,
                latest,
                ..
            } => {
                assert_eq!(oldest_available, 3);
                assert_eq!(latest, 4);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        // Gap 后不得再交付 partial ring Event。
        assert!(relay.try_recv().is_none());
    }

    /// after_seq=0 但 ring 已截断（oldest>1）必须 Gap，禁止 partial ring 静默丢失。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     同 owner 的 after=0 不是 brand-new；ring 已截断时仍需 Gap 触发 resync。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity=2 发布 1..3（oldest=2）；after_seq=0 首条 Gap{oldest=2,latest=3}，无 Event。
    #[test]
    fn open_relay_after_zero_with_truncated_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        let _ = bus.publish("e", json!(1));
        let _ = bus.publish("e", json!(2));
        let _ = bus.publish("e", json!(3)); // ring keeps 2,3; oldest=2
        let after_zero = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 0,
        };
        let mut relay = bus.open_relay(Some(&after_zero));
        match relay.try_recv().expect("gap") {
            RuntimeRelayMessage::Gap {
                oldest_available,
                latest,
                ..
            } => {
                assert_eq!(oldest_available, 2);
                assert_eq!(latest, 3);
            }
            other => panic!("expected gap, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// after_seq+1 == oldest 为连续边界，不得 Gap。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     after=0 且 oldest=1 是连续边界，应回放 event 1 而非假阳性 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity 足够，publish 1 后 after_seq=0 → Event{sequence:1}，无 Gap。
    #[test]
    fn open_relay_continuous_boundary_after_zero_oldest_one_no_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 8, 8);
        let _ = bus.publish("e", json!(1));
        let after_zero = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 0,
        };
        let mut relay = bus.open_relay(Some(&after_zero));
        match relay.try_recv().expect("event 1") {
            RuntimeRelayMessage::Event { sequence: 1, .. } => {}
            other => panic!("expected event 1, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// after_seq+1 == oldest 连续边界（非 0）不得 Gap，应回放 ring 中更新事件。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     after=1 且 oldest=2 是连续边界，禁止误报 Gap。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity=2 发布 1..3（oldest=2,latest=3）；after=1 → Event 2 与 3，无 Gap。
    #[test]
    fn open_relay_continuous_boundary_after_seq_plus_one_equals_oldest_no_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        let _ = bus.publish("e", json!(1));
        let _ = bus.publish("e", json!(2));
        let _ = bus.publish("e", json!(3)); // oldest=2, latest=3
        let after = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 1, // continuous: oldest 2 == after+1
        };
        let mut relay = bus.open_relay(Some(&after));
        match relay.try_recv().expect("event 2") {
            RuntimeRelayMessage::Event { sequence: 2, .. } => {}
            other => panic!("expected event 2, got {other:?}"),
        }
        match relay.try_recv().expect("event 3") {
            RuntimeRelayMessage::Event { sequence: 3, .. } => {}
            other => panic!("expected event 3, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    /// owner 重启后已发布超过 ring 容量，GUI 持旧 owner 游标重连必须收到 Gap。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     owner 变化时若只回放新 owner ring 尾部，pre-capacity 事件会 silent loss，
    ///     GUI 误 attach 低 sequence 而不走 terminal/runtime resync。
    ///
    /// Code Logic（这个测试做什么）:
    ///     capacity=2 的 owner-b 发布 3 条（ring 保留 2,3）；after 为 owner-a 任意 sequence；
    ///     首条必须是 Gap{oldest=2, latest=3}，且不得交付 Event-only partial ring。
    #[test]
    fn open_relay_owner_change_with_truncated_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-b", 2, 8);
        let _ = bus.publish("e", json!(1));
        let _ = bus.publish("e", json!(2));
        let _ = bus.publish("e", json!(3)); // ring keeps 2,3；oldest > 1
        let old_owner_cursor = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 5,
        };
        let mut relay = bus.open_relay(Some(&old_owner_cursor));
        let msg = relay.try_recv().expect("gap");
        match msg {
            RuntimeRelayMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            } => {
                assert_eq!(owner_instance_id, "owner-b");
                assert_eq!(oldest_available, 2);
                assert_eq!(latest, 3);
            }
            other => panic!("expected Gap on owner change, got {other:?}"),
        }
        // 不得在 Gap 之外再交付 partial ring Event。
        assert!(
            relay.try_recv().is_none(),
            "owner-change Gap must not also push partial ring events"
        );
    }

    /// owner 变化且 ring 为空时仍应 Gap，强制 GUI resync。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     新 owner 尚未发布任何事件时，旧游标重连也不得 silent attach 成 brand-new
    ///     live-only 消费者，否则 GUI 跳过 terminal/runtime snapshot resync。
    ///
    /// Code Logic（这个测试做什么）:
    ///     owner-b 空 ring；after=owner-a → 首条 Gap{oldest=0, latest=0}，无 Event。
    #[test]
    fn open_relay_owner_change_with_empty_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-b", 2, 8);
        let old_owner_cursor = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 5,
        };
        let mut relay = bus.open_relay(Some(&old_owner_cursor));
        let msg = relay.try_recv().expect("gap");
        match msg {
            RuntimeRelayMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            } => {
                assert_eq!(owner_instance_id, "owner-b");
                assert_eq!(oldest_available, 0);
                assert_eq!(latest, 0);
            }
            other => panic!("expected Gap on owner change with empty ring, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }

    #[test]
    fn gui_state_resets_on_owner_change_and_accepts_low_sequence() {
        let mut state = GuiEventRelayState::default();
        let a1 = state.on_message(RuntimeRelayMessage::Event {
            owner_instance_id: "owner-a".into(),
            sequence: 5,
            event: "e".into(),
            payload: json!(null),
        });
        assert!(matches!(a1, RelayClientAction::Deliver { .. }));
        let b1 = state.on_message(RuntimeRelayMessage::Event {
            owner_instance_id: "owner-b".into(),
            sequence: 1,
            event: "e".into(),
            payload: json!(null),
        });
        assert!(matches!(b1, RelayClientAction::Deliver { .. }));
        assert_eq!(
            state.cursor().unwrap(),
            BackendRuntimeCursor {
                owner_instance_id: "owner-b".into(),
                sequence: 1,
            }
        );
    }

    #[test]
    fn gui_state_drops_duplicates_within_same_owner() {
        let mut state = GuiEventRelayState::default();
        let _ = state.on_message(RuntimeRelayMessage::Event {
            owner_instance_id: "owner-a".into(),
            sequence: 2,
            event: "e".into(),
            payload: json!(1),
        });
        let dup = state.on_message(RuntimeRelayMessage::Event {
            owner_instance_id: "owner-a".into(),
            sequence: 2,
            event: "e".into(),
            payload: json!(1),
        });
        assert_eq!(dup, RelayClientAction::DropDuplicate);
    }

    #[test]
    fn gui_state_gap_requests_resync() {
        let mut state = GuiEventRelayState::default();
        let action = state.on_message(RuntimeRelayMessage::Gap {
            owner_instance_id: "owner-a".into(),
            oldest_available: 10,
            latest: 20,
        });
        assert!(matches!(action, RelayClientAction::RequestResync { .. }));
        assert_eq!(state.resync_count, 1);
        // 首帧 Gap 不立即推进 live cursor；recovery seed 由 restore_recovery_cursor 写回。
        assert!(state.cursor().is_none());
        assert!(state.recovery_pending());
    }

    /// Business Logic（R33: 为什么需要这个测试）:
    ///     桌面 GUI 首帧 Gap 无 pre-gap 时若 incomplete 仍 after=None 重连，owner 当 brand-new
    ///     只回放 truncated ring 且不再报原 Gap，造成永久 silent loss（与 mobile R32 M1 对齐）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     首帧 Gap → restore → cursor = owner-a/0 且 recovery_pending；重复 Gap 不覆盖 seed；
    ///     attach_at 后清 recovery。
    #[test]
    fn gui_state_first_gap_seeds_owner_zero_recovery_cursor() {
        let mut state = GuiEventRelayState::default();
        let action = state.on_message(RuntimeRelayMessage::Gap {
            owner_instance_id: "owner-a".into(),
            oldest_available: 10,
            latest: 20,
        });
        assert!(matches!(action, RelayClientAction::RequestResync { .. }));
        assert!(state.cursor().is_none());
        assert!(state.recovery_pending());
        state.restore_recovery_cursor();
        assert_eq!(
            state.cursor().unwrap(),
            BackendRuntimeCursor {
                owner_instance_id: "owner-a".into(),
                sequence: 0,
            }
        );
        assert!(state.recovery_pending());

        // 重复 Gap 不得覆盖既有 recovery seed。
        let _ = state.on_message(RuntimeRelayMessage::Gap {
            owner_instance_id: "owner-b".into(),
            oldest_available: 30,
            latest: 40,
        });
        state.restore_recovery_cursor();
        assert_eq!(
            state.cursor().unwrap(),
            BackendRuntimeCursor {
                owner_instance_id: "owner-a".into(),
                sequence: 0,
            }
        );

        state.attach_at(BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 20,
        });
        assert!(!state.recovery_pending());
        assert_eq!(state.cursor().unwrap().sequence, 20);
    }

    /// 验证 Gap 会保存 pre-gap 游标，incomplete 后可恢复，complete attach 清 recovery。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     incomplete Gap 不得以 None 重连成 brand-new consumer。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Event seq=3 → Gap → cursor None + recovery_pending → restore → cursor.sequence=3；
    ///     再 attach_at(20) → recovery_pending false。
    #[test]
    fn gui_state_gap_preserves_recovery_cursor_until_complete_attach() {
        let mut state = GuiEventRelayState::default();
        let _ = state.on_message(RuntimeRelayMessage::Event {
            owner_instance_id: "owner-a".into(),
            sequence: 3,
            event: "e".into(),
            payload: json!(null),
        });
        let action = state.on_message(RuntimeRelayMessage::Gap {
            owner_instance_id: "owner-a".into(),
            oldest_available: 10,
            latest: 20,
        });
        assert!(matches!(action, RelayClientAction::RequestResync { .. }));
        assert!(state.cursor().is_none());
        assert!(state.recovery_pending());
        state.restore_recovery_cursor();
        assert_eq!(
            state.cursor().unwrap(),
            BackendRuntimeCursor {
                owner_instance_id: "owner-a".into(),
                sequence: 3,
            }
        );
        assert!(state.recovery_pending());
        state.attach_at(BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 20,
        });
        assert!(!state.recovery_pending());
        assert_eq!(state.cursor().unwrap().sequence, 20);
    }
    /// Business Logic（R28 H2: 为什么需要这个测试）:
    ///     after sequence=0 在 ring 截断后必须 Gap，禁止 silent partial replay。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ring=2 publish 1..4 → after=(owner,0) → Gap oldest=3 latest=4。
    #[test]
    fn open_relay_after_sequence_zero_with_truncated_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        bus.publish("e", json!(1));
        bus.publish("e", json!(2));
        bus.publish("e", json!(3));
        bus.publish("e", json!(4));
        let after_zero = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 0,
        };
        let mut relay = bus.open_relay(Some(&after_zero));
        match relay.try_recv() {
            Some(RuntimeRelayMessage::Gap {
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
    ///     after=2、ring 含 3,4 → Event 3/4，无 Gap。
    #[test]
    fn open_relay_continuous_boundary_does_not_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        bus.publish("e", json!(1));
        bus.publish("e", json!(2));
        bus.publish("e", json!(3));
        bus.publish("e", json!(4));
        let after = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 2,
        };
        let mut relay = bus.open_relay(Some(&after));
        match relay.try_recv() {
            Some(RuntimeRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 3),
            other => panic!("expected event 3, got {other:?}"),
        }
        match relay.try_recv() {
            Some(RuntimeRelayMessage::Event { sequence, .. }) => assert_eq!(sequence, 4),
            other => panic!("expected event 4, got {other:?}"),
        }
        assert!(relay.try_recv().is_none());
    }
}
