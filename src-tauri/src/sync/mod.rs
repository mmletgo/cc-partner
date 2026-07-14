//! sync — Prompt 跨设备同步引擎
//!
//! Business Logic: 实现 CRDT 风格的 Prompt 跨设备同步，对照 Python `sync/` 包：
//!     1) `vector_clock`：向量时钟 compare/merge/increment（纯算法，CRDT 正确性根基）；
//!     2) `merger`：LWW 冲突合并（并发时按 updated_at 取较新，时间戳相等按 device_id tie-break）；
//!     3) `engine`：`trigger_sync` 协调流程（遍历对端双向 pull/push）；
//!     4) `protocol`：有界 manifest/page/batch 计划与 typed domain outcome（N2 纯协议层）。
//!
//! Code Logic: vector_clock / merger / protocol 为纯函数无 IO，配单测保证正确性；
//!     engine 持有 AppState，调 prompt_repo / peer_client 完成实际同步。

pub mod claude_md;
pub mod engine;
pub mod merger;
pub mod protocol;
pub mod scratchpad;
pub mod ssh_target;
pub mod vector_clock;
