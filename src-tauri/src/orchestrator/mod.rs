//! Orchestrator 后端基础模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器后续会管理任务状态、持久化队列、执行证据和事件流，需要先把领域模型、
//!     状态机与仓储拆成独立模块，避免混入 Workbench 或命令层。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 Orchestrator 的数据模型、状态机和 SQLite 仓储。

pub mod config;
pub mod delivery;
pub mod models;
pub mod outbox;
pub mod prompt;
pub mod remote_client;
pub mod remote_protocol;
pub mod repo;
pub mod runner;
pub mod scheduler;
pub mod state;
