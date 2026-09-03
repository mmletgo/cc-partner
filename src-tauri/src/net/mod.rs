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
/// S1 Task 6 集成 smoke harness（绑定端口 + injected peer 证据矩阵）。
pub mod lan_trust_boundary_harness;
/// 手动配置 overlay 对端探测（跨子网/VPN，绕过 mDNS；opt-in 精确 IP 放行）。
pub mod manual_peers;
/// 开发态 `/mobile` → 本机 Vite 反向代理（HMR）；生产不启用业务语义。
pub mod mobile_dev_proxy;
pub mod peer_client;
pub mod peer_error;
pub mod peer_timeout;
pub mod protocol;
/// 中转访问（跳板机）透明转发器（HTTP 流式转发 + 终端 WS 桥 + 并发闸）。
pub mod relay;
/// 影子设备（经跳板可见的远端目标）状态与纯规则。
pub mod relay_shadow;
/// 影子设备周期探测（A 侧：拉取跳板 `/api/relay/peers` 合成/老化影子表）。
pub mod relay_shadow_probe;
pub mod request_context;
pub mod routes;

/// mDNS 服务类型。跟随应用名 cc-partner，供局域网内同版本实例互相发现。
pub const SERVICE_TYPE: &str = "_cc-partner._tcp.local.";
