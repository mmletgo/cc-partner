//! agent_hub/replication — LAN source-push 发送端 + 接收端与幂等 ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate C 仅允许源侧选择目标后的 push；源设备 multi-target 独立 outcome，
//!     目标设备原子接收 SnapshotEnvelope v1，用 (sourceDeviceId, clientRequestId) 幂等收敛。
//!
//! Code Logic（这个模块做什么）:
//!     `sender`（源侧 multi-target push + source ledger）、`ledger`/`receiver`（目标端）；
//!     路由层 `net/routes/agent_hub.rs` 薄封装接收端。

pub mod ledger;
pub mod receiver;
pub mod sender;

pub use ledger::{
    PushObjectRow, PushRequestRow, PushRequestStatus, ReplicationLedger, MAX_STAGING_AGE,
};
pub use receiver::{
    commit_push, gc_abandoned_incoming_staging, prepare_push, put_object_chunk, AgentHubChunkLimit,
    CommitPushRequest, CommitPushResponse, PreparePushRequest, PreparePushResponse,
    PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
pub use sender::{
    get_push_report_for_state, list_failed_source_push_targets, push_selection_for_state,
    AgentHubPushSender, MultiTargetPushReport, PushAgentHubSelectionRequest, SourcePushTargetRow,
    TargetPushOutcome, TargetPushStatus, MAX_TARGET_PARALLELISM,
};
