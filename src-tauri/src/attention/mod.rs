//! attention — 全局 Inbox 聚合领域。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户需要“现在有哪些事情需要我处理，工作才能继续”的实时投影；聚合层不写 Inbox 表，
//!     只从 Orchestrator/Workbench 权威状态投影，失败时整次快照失败。
//!
//! Code Logic（这个模块做什么）:
//!     导出 models/source/aggregator 与 orchestrator/workbench dependency source；
//!     Tauri command 与 Mobile HTTP 经 commands/attention 与 net/routes/attention 接入。

// source 纯投影 helper 可能仅被测试/后续扩展引用；保留 dead_code 以免阻断编译。
#![allow(dead_code)]

pub(crate) mod agent_runtime_source;
pub mod aggregator;
pub(crate) mod experiment_source;
pub mod models;
pub(crate) mod orchestrator_source;
pub(crate) mod source;
pub(crate) mod workbench_dependency_source;
