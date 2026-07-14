//! commands/orchestrator 目录模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     保持 orchestrator_cmd:: 注册名不变。
//!
//! Code Logic（这个模块做什么）:
//!     子模块 + pub use 再导出。

mod actions;
mod common;
mod evidence;
mod remote;
mod runtime;
mod tasks;

#[cfg(test)]
mod tests;

pub use actions::*;
pub use common::*;
pub use evidence::*;
pub use remote::*;
pub use runtime::*;
pub use tasks::*;
