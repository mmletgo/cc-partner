//! net/peer_timeout.rs — Peer 出站请求超时分类。
//!
//! Business Logic（为什么需要这个模块）:
//!     health/metadata/mutation 不能共用同一超时；长操作必须显式预算。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `PeerTimeoutClass` 与固定秒数映射，以及 `long_running` 预算入口。

use std::time::Duration;

/// Peer 出站请求超时分类。
///
/// Business Logic（为什么需要这个类型）:
///     所有 peer 请求共用 10s 会让 health 过慢、mutation 过早失败；按请求类别配置超时可降低
///     多设备同步尾延迟，并给 push/transfer 足够预算。事件流不在本 enum 内。
///
/// Code Logic（这个类型做什么）:
///     `timeout()` 映射固定秒数；长操作必须用 `long_running(budget)` 显式预算，禁止隐式默认。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PeerTimeoutClass {
    /// 健康检查 / capability probe
    Health,
    /// 只读元数据与列表（manifest、pull、items、status、inventory）
    Metadata,
    /// 普通写路径（push、push-batch、ack、transfer init/chunk/complete）
    Mutation,
    /// 长操作占位；实际时长必须经 `PeerTimeoutClass::long_running(budget)` 显式给出。
    /// 调用方通过 `long_running(budget)` 取 Duration，不必构造本 variant；保留以完成分类枚举。
    #[allow(dead_code)]
    LongRunning,
}

impl PeerTimeoutClass {
    /// Health 超时秒数。
    pub const HEALTH_SECS: u64 = 3;
    /// Metadata 超时秒数。
    pub const METADATA_SECS: u64 = 10;
    /// Mutation 超时秒数。
    pub const MUTATION_SECS: u64 = 30;

    /// 返回该分类的默认超时。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方按请求语义选类后需要稳定 Duration，避免散落 magic number。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Health→3s，Metadata→10s，Mutation→30s；LongRunning 无固定默认，调用方应使用
    ///     `long_running`，此处返回 Mutation 作为 fail-safe 上界提示而非生产路径依赖。
    pub const fn timeout(self) -> Duration {
        match self {
            Self::Health => Duration::from_secs(Self::HEALTH_SECS),
            Self::Metadata => Duration::from_secs(Self::METADATA_SECS),
            Self::Mutation => Duration::from_secs(Self::MUTATION_SECS),
            // LongRunning 必须显式预算；保留 Mutation 上界避免误用时无超时。
            Self::LongRunning => Duration::from_secs(Self::MUTATION_SECS),
        }
    }

    /// 构造长操作的显式超时预算。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     大包 bundle / 长 finalize 等不能套 3/10/30 固定类，调用方必须声明预算。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原样返回传入的 `budget`（Duration），供 request helper 的 `.timeout(...)` 使用。
    pub const fn long_running(budget: Duration) -> Duration {
        budget
    }
}

// 兼容性再导出：历史上 `PeerCallError` 定义在本模块；Task 7 把它统一到 `net::peer_error`，
// 这里再导出统一类型，使旧调用点（`use crate::net::peer_client::PeerCallError`）无需改动，
// 同时 `health_info` 等签名自动指向新枚举（Network/Unsupported/InvalidResponse/Remote）。
