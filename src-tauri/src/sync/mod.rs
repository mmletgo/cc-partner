//! sync — Prompt 跨设备同步引擎
//!
//! Business Logic: 实现 CRDT 风格的 Prompt 跨设备同步，对照 Python `sync/` 包：
//!     1) `vector_clock`：向量时钟 compare/merge/increment（纯算法，CRDT 正确性根基）；
//!     2) `merger`：LWW 冲突合并（并发时按 updated_at 取较新，时间戳相等按 device_id tie-break）；
//!     3) `engine`：`trigger_sync` 并发协调（buffer_unordered(4)）并返回 per-device/domain 真值；
//!     4) `protocol`：有界 manifest/page/batch 计划与 typed domain outcome（N2 纯协议层）；
//!     5) `apply_merge`：三域 push-batch/本地 pull 单事务落库（winner/conflict/epoch/ledger/ack）。
//!
//! Code Logic: vector_clock / merger / protocol 为纯函数无 IO，配单测保证正确性；
//!     engine 持有 AppState，调 prompt_repo / peer_client 完成实际同步；
//!     apply_merge 由 HTTP push-batch 与引擎本地 apply 复用。

pub mod apply_merge;
pub mod attention_read_apply;
pub mod claude_md;
pub mod engine;
pub mod merger;
#[cfg(test)]
pub mod mixed_version_harness;
pub mod protocol;
pub mod scratchpad;
pub mod ssh_target;
pub mod vector_clock;
