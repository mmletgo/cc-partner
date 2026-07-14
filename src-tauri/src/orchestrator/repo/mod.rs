//! Orchestrator SQLite repository.
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器需要把任务队列、状态变化与证据持久化。
//!
//! Code Logic（这个模块做什么）:
//!     目录拆分 schema/tasks/attempts/evidence/remote；对外 API 保持 `OrchestratorRepo`。

#![allow(dead_code)]

mod attempts;
mod evidence;
mod helpers;
mod remote;
mod schema;
mod tasks;

#[cfg(test)]
mod tests;

pub use helpers::{
    OrchestratorRecentEventRow, ORCHESTRATOR_EVENT_SCHEMA, ORCHESTRATOR_EVIDENCE_SCHEMA,
    ORCHESTRATOR_PROJECT_CONFIG_SCHEMA, ORCHESTRATOR_REMOTE_OUTBOX_SCHEMA,
    ORCHESTRATOR_REMOTE_TASK_CREATE_REQUEST_SCHEMA, ORCHESTRATOR_REMOTE_TASK_MIRROR_SCHEMA,
    ORCHESTRATOR_TASK_ATTEMPT_SCHEMA, ORCHESTRATOR_TASK_SCHEMA,
};

#[allow(unused_imports)]
pub(crate) use helpers::*;

use sqlx::sqlite::SqlitePool;

/// 幂等 create 结果：任务行 + 是否首次插入。
///
/// Business Logic（为什么需要这个结构体）:
///     clientRequestId 重试时需区分新建与幂等命中。
///
/// Code Logic（这个结构体做什么）:
///     `task` 权威行；`newly_created` 表示本次是否 insert。
#[derive(Debug, Clone)]
pub struct IdempotentCreateTaskOutcome {
    pub task: crate::orchestrator::models::OrchestratorTaskRow,
    pub newly_created: bool,
}

/// Orchestrator 仓储。
///
/// Business Logic（为什么需要这个结构体）:
///     持久化任务队列与证据。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool 并提供 CRUD/CAS 方法。
#[derive(Clone)]
pub struct OrchestratorRepo {
    pub(crate) pool: SqlitePool,
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 需要用同一个 SQLite pool 构造 Orchestrator 仓储。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 SqlitePool clone；pool 内部是 Arc，clone 廉价。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产路径 `db.acquire_wait_ms` 埋点与测试需要访问同一 SqlitePool 采样连接等待。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 pool 引用（clone 廉价，调用方可 `acquire`）。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
