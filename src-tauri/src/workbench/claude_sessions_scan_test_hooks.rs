//! Claude session 扫描 mid-scan 测试 barrier（文件名含 test，module-boundary 排除）。
//!
//! Business Logic:
//!     回归 dispose/ensure 竞态时需要可控 barrier。
//!
//! Code Logic:
//!     提供 install / wait_before_scan；仅 cfg(test) 挂载。

use std::sync::{Mutex, OnceLock};
use tokio::sync::Notify;

/// mid-scan 测试 barrier：entered 通知测试 dispose 可介入；release 放行扫描。
#[derive(Clone)]
pub struct ScanTestBarrier {
    pub entered: std::sync::Arc<Notify>,
    pub release: std::sync::Arc<Notify>,
}

static HOOKS: OnceLock<Mutex<Option<ScanTestBarrier>>> = OnceLock::new();

fn hooks_lock() -> std::sync::MutexGuard<'static, Option<ScanTestBarrier>> {
    HOOKS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("scan test hooks 锁中毒")
}

/// Business Logic（为什么需要这个函数）:
///     回归测试要稳定复现 “ensure in-flight + dispose mid-scan”。
///
/// Code Logic（这个函数做什么）:
///     安装 barrier；None 清除。
pub fn install(barrier: Option<ScanTestBarrier>) {
    *hooks_lock() = barrier;
}

/// Business Logic（为什么需要这个函数）:
///     finish_scan_and_insert 在扫描前等待测试放行。
///
/// Code Logic（这个函数做什么）:
///     若已安装 barrier：notify entered 后 await release；无 hook 则立即返回。
pub async fn wait_before_scan() {
    let barrier = hooks_lock().clone();
    if let Some(b) = barrier {
        b.entered.notify_waiters();
        b.release.notified().await;
    }
}
