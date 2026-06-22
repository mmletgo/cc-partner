//! net/routes — axum HTTP 路由处理器
//!
//! Business Logic: 对照 Python `network/protocol.py` 中供对端调用的 P2P API handler。
//!     已实现 `/api/health`（M3）、`/api/sync/{pull,push}`（M4）；transfer handler 留待 M5。
//!
//! Code Logic: 每个 handler 通过 axum `State<AppState>` 取共享依赖，返回 `axum::Json`。
//!     字段命名与 Python handler 返回结构一致，确保 Rust 版与旧 Python 版对端可互解析。

pub mod health;
pub mod sync;
pub mod transfer;
