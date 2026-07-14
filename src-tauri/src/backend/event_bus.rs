//! backend/event_bus.rs — sidecar 有界事件总线与本机 relay 游标。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 进程不得拥有第二份 terminal/merge/transfer/runtime 事件源；sidecar 作为唯一 HeadlessOwner
//!     发布事件，GUI 通过 `(ownerInstanceId, sequence)` 游标做 afterSequence 重连、去重与 gap 恢复。
//!
//! Code Logic（这个模块做什么）:
//!     提供 `RuntimeEventBus`：有界 broadcast + 有界 replay ring；`BackendRuntimeCursor` 与
//!     `RuntimeRelayMessage::{Event,Gap}`；`GuiEventRelayState` 在 owner 变化时重置、同 owner 去重，
//!     并在 Gap/Lag 时要求 terminal replay + runtime snapshot resync。

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
    ///     terminal/merge/transfer/runtime 事件必须在 sidecar 单调编号后才能被 GUI 去重。
    ///
    /// Code Logic（这个函数做什么）:
    ///     分配 next_sequence；写入 ring（满则弹出最旧）；broadcast Event；返回 cursor。
    pub fn publish(&self, event: &str, payload: Value) -> BackendRuntimeCursor {
        let sequence = {
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
            sequence
        };
        let message = RuntimeRelayMessage::Event {
            owner_instance_id: self.owner_instance_id.clone(),
            sequence,
            event: event.to_string(),
            payload,
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
    ///     GUI 重连时需要 afterSequence 回放，若游标早于 ring 必须先收到 Gap 再 attach live。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 subscribe live，再在锁内计算 gap/replay，避免丢失窗口内新事件；
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
            let after_seq = if same_owner {
                after.map(|c| c.sequence).unwrap_or(0)
            } else {
                // owner 变化：清旧游标，从 ring 头开始（若有）或直接 live。
                0
            };

            if same_owner && after_seq > 0 && oldest > 0 && after_seq < oldest.saturating_sub(0) {
                // after_seq 严格早于 ring 最旧条（即 after_seq + 1 < oldest 或 after 不在 ring 覆盖范围）
                if after_seq < oldest {
                    pending.push(RuntimeRelayMessage::Gap {
                        owner_instance_id: self.owner_instance_id.clone(),
                        oldest_available: oldest,
                        latest,
                    });
                    max_replayed = latest;
                }
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
    /// 向前端投递业务事件（原始 event 名 + payload）。
    Deliver { event: String, payload: Value },
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
///
/// Code Logic（这个结构做什么）:
///     持有可选 cursor；处理 Event/Gap 并产出动作列表。
#[derive(Debug, Clone, Default)]
pub struct GuiEventRelayState {
    cursor: Option<BackendRuntimeCursor>,
    /// 累计 resync 次数（测试/诊断）。
    pub resync_count: u64,
}

impl GuiEventRelayState {
    /// 当前游标。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     重连 afterSequence 需要读出已提交游标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 cursor 克隆。
    pub fn cursor(&self) -> Option<BackendRuntimeCursor> {
        self.cursor.clone()
    }

    /// 处理一条 relay 消息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 重启不得当重复；Gap 必须触发 resync 而非 silent drop。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Gap → RequestResync 并清空 cursor；
    ///     Event：owner 不同则重置后投递；同 owner sequence<=cursor 则 DropDuplicate；否则推进游标并 Deliver。
    pub fn on_message(&mut self, message: RuntimeRelayMessage) -> RelayClientAction {
        match message {
            RuntimeRelayMessage::Gap {
                owner_instance_id,
                oldest_available,
                latest,
            } => {
                self.cursor = None;
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
                    }
                }
                self.cursor = Some(BackendRuntimeCursor {
                    owner_instance_id,
                    sequence,
                });
                RelayClientAction::Deliver { event, payload }
            }
        }
    }

    /// resync 完成后 attach 到最新 live 游标（不投递历史）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap 后 GUI 先 snapshot/replay，再从 latest live 接上，避免重复历史。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接设置 cursor 为给定值。
    pub fn attach_at(&mut self, cursor: BackendRuntimeCursor) {
        self.cursor = Some(cursor);
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

    #[test]
    fn cursor_before_ring_emits_gap() {
        let bus = RuntimeEventBus::with_capacity("owner-a", 2, 8);
        let _ = bus.publish("e", json!(1));
        let _ = bus.publish("e", json!(2));
        let _ = bus.publish("e", json!(3)); // ring keeps 2,3
        let stale = BackendRuntimeCursor {
            owner_instance_id: "owner-a".into(),
            sequence: 1,
        };
        let mut relay = bus.open_relay(Some(&stale));
        let msg = relay.try_recv().expect("gap");
        match msg {
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
        assert!(state.cursor().is_none());
    }
}
