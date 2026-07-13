//! net — P2P 网络层
//!
//! Business Logic: 实现 P2P 局域网协作的三块能力，对照 Python `network/` 包：
//!     1) `discovery`：mdns-sd 注册/发现（`_cc-partner._tcp.local.`）；
//!     2) `http_server`：axum HTTP server（固定首选端口，冲突时递增），供对端 reqwest 调用 `/api/health` 等；
//!     3) `peer_client`：reqwest 客户端，调对端 API（health 实测，sync/transfer 留 M4/M5）。
//!
//! Code Logic: 三个子模块各自独立，通过 AppState 共享 devices 表与端口。
//!     启动顺序（backend/runtime.rs 编排）：advertise 模式先 axum 拿实际端口 → 用该端口启动 mDNS 注册；
//!     browse-only 模式验证 sidecar 控制文件与 health 后复用端口且不注册本机服务。

pub mod discovery;
pub mod error_response;
pub mod http_server;
pub mod lan_guard;
pub mod peer_client;
pub mod peer_error;
pub mod protocol;
pub mod request_context;
pub mod routes;

/// mDNS 服务类型。跟随应用名 cc-partner，供局域网内同版本实例互相发现。
pub const SERVICE_TYPE: &str = "_cc-partner._tcp.local.";
