//! workbench/browser_verification — 浏览器自动验证领域
//!
//! Business Logic（为什么需要这个模块）:
//!     现有 iframe preview 无法在无 `allow-same-origin` 的情况下读取 DOM；自动验证必须在
//!     owning device 侧用 ephemeral managed Chromium 产生 snapshot/交互/console/screenshot evidence，
//!     且不得扩大成任意 URL 代理或破坏既有 sandbox。
//!
//! Code Logic（这个模块做什么）:
//!     导出有界 DTO 与 `BrowserVerificationEngine` 缝合点；runtime/chromium/artifact 在后续任务接入。

pub mod engine;
pub mod models;

pub use engine::{
    BrowserVerificationEngine, BrowserVerificationObserver, EngineRunRequest, EngineRunResult,
    NoopObserver,
};
pub use models::*;
