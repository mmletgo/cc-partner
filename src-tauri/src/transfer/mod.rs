//! transfer — 文件传输模块
//!
//! Business Logic: 实现局域网分块文件传输（对照 Python `transfer/` 包）：
//!     - `registry`：活跃传输任务表（含每任务 CancellationToken）
//!     - `sender`：发送端（init → 分块 → 完成校验）
//!     - `receiver`：接收端（init → chunk 写入 → finalize SHA256 校验 + 文件名冲突处理）
//!
//! Code Logic: 与 Python 逐方法对照，协议字段（init/chunk/status、X-Chunk-Offset header、
//!     960KB chunk_size、resume_offset 语义）保持一致，确保迁移期 Rust↔Python 互通。

pub mod receiver;
pub mod registry;
pub mod sender;

/// 分块大小：960KB（与 Python CHUNK_SIZE 完全一致）。
///
/// Business Logic: 低于 aiohttp 默认 client_max_size(1MB)，兼容未自定义 body 限制的旧版对端。
pub const CHUNK_SIZE: usize = 960 * 1024;

/// 电脑发给本机 `/mobile` 的虚拟目标 id。不是 mDNS 设备，不进 `devices` 表。
pub const MOBILE_INBOX_DEVICE_ID: &str = "cc-partner-mobile-inbox";

/// 是否发给手机邮箱的虚拟目标。
///
/// Business Logic（为什么需要这个函数）:
///     send/retry/download 必须在 peer lookup 之前认出这个 id，避免当 P2P 对端。
///
/// Code Logic（这个函数做什么）:
///     与 `MOBILE_INBOX_DEVICE_ID` 精确相等。
pub fn is_mobile_inbox_device_id(device_id: &str) -> bool {
    device_id == MOBILE_INBOX_DEVICE_ID
}
