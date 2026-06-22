//! net/routes — axum HTTP 路由处理器
//!
//! Business Logic: 对照 Python `network/protocol.py` 中供对端调用的 P2P API handler。
//!     已实现 `/api/health`（M3）、`/api/sync/{pull,push}`（M4）、`/api/transfer/*`（M5）；
//!     `/api/cc-history/sync/{pull,push}` 走独立链路同步 Claude Code 历史。
//!
//! Code Logic: 每个 handler 通过 axum `State<AppState>` 取共享依赖，返回 `axum::Json`。
//!     字段命名与对端约定一致（sync/cc-history 用 snake_case 互通，transfer 字段对照 Python）。

pub mod cc_history;
pub mod health;
pub mod sync;
pub mod transfer;
