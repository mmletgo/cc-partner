//! updater — 自动更新 generation 状态机
//!
//! Business Logic（为什么需要这个模块）:
//!     检查/下载/安装更新会跨多个 invoke 命令与异步回调交错执行。旧实现把
//!     status/pending/bytes/task/token 拆成五把锁，旧下载回调可能覆盖新 check 结果，
//!     install 失败后 `take()` 掉 bytes 导致无法重试。
//!
//! Code Logic（这个模块做什么）:
//!     提供单锁 `UpdateRuntime`，用 generation + phase 状态机串行所有转移；
//!     锁内不 await、不跑网络/安装；下载/安装完成回调比对 generation 后才写回。

pub mod runtime;

pub use runtime::{InstallOutcome, UpdateRuntime};
