//! agent_hub/projection — durable projection jobs 与原子写盘
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub revision 与目标 CLI 文件之间需要可崩溃恢复的投影边界：DB 与 FS 无法单事务，
//!     必须用 job ledger + 原子替换保证“旧或新完整文件”与 materialization 真值。
//!
//! Code Logic（这个模块做什么）:
//!     Gate A Task 6：`AtomicProjectionWriter`（sibling temp/staging）+
//!     `ProjectionScheduler`（Semaphore(4)、per-asset 锁、opt-in 过滤、conflict 冻结、crash recovery）。

pub mod atomic_writer;
pub mod scheduler;

pub use atomic_writer::{
    AtomicProjectionWriter, AtomicWriteOutcome, DirectoryWriteRequest, FileWriteRequest,
    ProjectionWriteFault,
};
pub use scheduler::{
    advance_package_activation, is_managed_package_target_path, PackageActivationAdvance,
    ProjectionRequest, ProjectionRunStats, ProjectionScheduler, MAX_GLOBAL_PROJECTION_PARALLELISM,
};
