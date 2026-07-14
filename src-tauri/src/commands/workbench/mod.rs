//! commands/workbench 目录模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     保持 workbench_cmd:: 注册名不变。
//!
//! Code Logic（这个模块做什么）:
//!     子模块 + pub use 再导出。

mod browser;
mod common;
mod files;
mod git;
mod projects;
mod sessions;

#[cfg(test)]
mod tests;

pub use browser::*;
pub use common::*;
pub use files::*;
pub use git::*;
pub use projects::*;
pub use sessions::*;
