//! agent_hub/replication — LAN source-push 接收端与幂等 ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     Gate C 仅允许源侧选择目标后的 push；目标设备必须原子接收 SnapshotEnvelope v1，
//!     用 (sourceDeviceId, clientRequestId) 幂等收敛，禁止半截 import 或把 projection 成功混入协议提交。
//!
//! Code Logic（这个模块做什么）:
//!     导出 `ledger`（SQLite 请求/对象 outcome）与 `receiver`（prepare/chunk/commit + staging/GC）；
//!     路由层 `net/routes/agent_hub.rs` 薄封装；object 导入复用 `SnapshotImporter::commit_import`。

pub mod ledger;
pub mod receiver;

pub use ledger::{
    PushObjectRow, PushRequestRow, PushRequestStatus, ReplicationLedger, MAX_STAGING_AGE,
};
pub use receiver::{
    commit_push, gc_abandoned_incoming_staging, prepare_push, put_object_chunk, AgentHubChunkLimit,
    CommitPushRequest, CommitPushResponse, PreparePushRequest, PreparePushResponse,
    PutObjectResponse, AGENT_HUB_MAX_CHUNK_BYTES,
};
