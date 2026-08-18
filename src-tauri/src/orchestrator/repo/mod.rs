//! Orchestrator SQLite repository.
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器需要把任务队列、状态变化与证据持久化。
//!
//! Code Logic（这个模块做什么）:
//!     目录拆分 schema/tasks/attempts/evidence/remote；对外 API 保持 `OrchestratorRepo`。

#![allow(dead_code)]

mod attempts;
mod blocks;
mod evidence;
mod experiments;
mod helpers;
mod remote;
mod schema;
mod tasks;

#[cfg(test)]
mod tests;

pub(crate) use blocks::{is_intermediate_block_member, BlockMemberDraft};
pub use helpers::{
    OrchestratorRecentEventRow, ORCHESTRATOR_EVENT_SCHEMA, ORCHESTRATOR_EVIDENCE_SCHEMA,
    ORCHESTRATOR_PROJECT_CONFIG_SCHEMA, ORCHESTRATOR_REMOTE_OUTBOX_SCHEMA,
    ORCHESTRATOR_REMOTE_TASK_CREATE_REQUEST_SCHEMA, ORCHESTRATOR_REMOTE_TASK_MIRROR_SCHEMA,
    ORCHESTRATOR_TASK_ATTEMPT_SCHEMA, ORCHESTRATOR_TASK_SCHEMA,
};

#[allow(unused_imports)]
pub(crate) use helpers::*;

use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
use sqlx::sqlite::SqlitePool;
use std::sync::Arc;

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

/// 幂等 create-block 结果：块行 + 成员任务 + 是否首次插入。
///
/// Business Logic（为什么需要这个结构体）:
///     clientRequestId 重试时需区分新建与幂等命中，避免 Start 重放再次调度。
///
/// Code Logic（这个结构体做什么）:
///     `block` 与 `tasks` 为权威行；`newly_created` 表示本次是否 insert。
#[derive(Debug, Clone)]
pub struct IdempotentCreateBlockOutcome {
    pub block: crate::orchestrator::models::OrchestratorTaskBlockRow,
    pub tasks: Vec<crate::orchestrator::models::OrchestratorTaskRow>,
    pub newly_created: bool,
}

/// Delivering→Done CAS 结果。
///
/// Business Logic（为什么需要这个枚举）:
///     交付收尾写 Done 可能与用户 Abort 并发；仅 Transitioned 时可安全清理 worktree。
///
/// Code Logic（这个枚举做什么）:
///     `Transitioned` 表示 rows_affected==1 已写入 Done；`CasMiss` 表示未改写，携带当前行。
#[derive(Debug, Clone)]
pub enum FinishTaskDoneOutcome {
    Transitioned(crate::orchestrator::models::OrchestratorTaskRow),
    CasMiss(crate::orchestrator::models::OrchestratorTaskRow),
}

/// Orchestrator 仓储。
///
/// Business Logic（为什么需要这个结构体）:
///     持久化任务队列与证据；生产写路径必须经全局 maintenance gate，
///     以便 restore exclusive 期间阻塞 ordinary writer。
///
/// Code Logic（这个结构体做什么）:
///     持有 SqlitePool + DatabaseMaintenanceGate，提供 CRUD/CAS 方法；
///     写事务走 begin_shared_write，单语句写走 with_shared_write_lease。
#[derive(Clone)]
pub struct OrchestratorRepo {
    pub(crate) pool: SqlitePool,
    pub(crate) gate: Arc<DatabaseMaintenanceGate>,
}

impl OrchestratorRepo {
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup / 测试可用默认独立 gate 构造仓储（fixture 不共享 restore 屏障）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 `with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))`。
    pub fn new(pool: SqlitePool) -> Self {
        Self::with_gate(pool, Arc::new(DatabaseMaintenanceGate::new()))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产路径必须把 orchestrator 写路径接到 AppState 共享 maintenance_gate，
    ///     才能在 restore exclusive 期间真正阻塞 claim/outbox 等写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 pool 与 gate 的 Arc 引用（二者 clone 廉价）。
    pub fn with_gate(pool: SqlitePool, gate: Arc<DatabaseMaintenanceGate>) -> Self {
        Self { pool, gate }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     生产路径 `db.acquire_wait_ms` 埋点与测试需要访问同一 SqlitePool 采样连接等待。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 pool 引用（clone 廉价，调用方可 `acquire`）。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Business Logic（为什么需要这个函数）:
    ///     调用方在已持有 shared lease 时可能需要复用同一 gate（较少见）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 gate 的 Arc 引用。
    #[allow(dead_code)]
    pub(crate) fn gate(&self) -> &Arc<DatabaseMaintenanceGate> {
        &self.gate
    }
}
