//! commands — Tauri invoke 命令层
//!
//! Business Logic: 对应前端调用的 `invoke('xxx')`，是本地前端↔Rust 的 IPC 边界。
//!     M1 实现 Prompt CRUD、配置读写、版本查询，对照 Python protocol.py 的相关 HTTP handler。
//!
//! Code Logic: 通过 `State<'_, AppState>` 注入依赖；参数与返回均 camelCase 对齐前端；
//!     所有命令返回 `Result<T, AppError>`，错误序列化为 `{"error": "..."}` 给前端。

pub mod config;
pub mod devices;
pub mod permissions;
pub mod prompts;
pub mod screenshot;
pub mod sync;
pub mod transfer;
pub mod updater;
