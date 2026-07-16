//! Automated Candidate Experiments 领域模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     同一任务的 2–8 个 candidate 需要组级创建、公平调度、比较判定与唯一交付，
//!     不能依赖用户手工创建多 worktree 或逐条 task 审查。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 models 与 create/reducer/judge/delivery/outbox/remote 子模块。

pub mod create;
pub mod delivery;
pub mod judge;
pub mod models;
pub mod outbox;
pub mod reducer;
pub mod remote_client;
pub mod remote_protocol;

pub use models::*;
