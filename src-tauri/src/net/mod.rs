//! net — P2P 网络层
//!
//! Business Logic: 实现 P2P 局域网协作的三块能力，对照 Python `network/` 包：
//!     1) `discovery`：mdns-sd 注册/发现（`_claude-partner._tcp.local.`），与旧 Python 版互发现；
//!     2) `http_server`：axum HTTP server（port=0 动态），供对端 reqwest 调用 `/api/health` 等；
//!     3) `peer_client`：reqwest 客户端，调对端 API（health 实测，sync/transfer 留 M4/M5）。
//!
//! Code Logic: 三个子模块各自独立，通过 AppState 共享 devices 表与端口。
//!     启动顺序（lib.rs 编排）：先 axum 拿实际端口 → 用该端口启动 mDNS 注册。

pub mod discovery;
pub mod http_server;
pub mod peer_client;
pub mod routes;

/// mDNS 服务类型，必须与 Python `discovery.py` 的 `SERVICE_TYPE` 完全一致，
/// 否则 Rust 版与旧 Python 版无法互相发现。
pub const SERVICE_TYPE: &str = "_claude-partner._tcp.local.";
