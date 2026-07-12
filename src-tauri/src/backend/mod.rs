//! backend — 独立后端进程与 GUI 共享的运行时模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端设备应能只启动 cc-partner 后端进程就暴露 Workbench/P2P 能力，GUI 也需要管理该后端生命周期。
//!
//! Code Logic（这个模块做什么）:
//!     聚合控制文件、UI 适配、共享 runtime 和 CLI 命令入口。

pub mod cli;
pub mod control;
pub mod logging;
pub mod runtime;
pub mod ui;
