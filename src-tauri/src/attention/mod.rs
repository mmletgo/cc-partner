//! attention — 全局 Inbox 聚合领域。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要“现在有哪些事情需要我处理，工作才能继续”的实时投影；聚合层不写 Inbox 表，
//!     只从 Orchestrator/Workbench 权威状态投影，失败时整次快照失败。
//!
//! Code Logic（这个模块做什么）:
//!     导出 models/source/aggregator；具体 source 与 command/route 在后续 task 接入。

// 本 task 只落地 DTO/trait/aggregator；后续 task 的 source/command/route 才会消费这些符号。
#![allow(dead_code)]

pub mod aggregator;
pub mod models;
pub(crate) mod source;
