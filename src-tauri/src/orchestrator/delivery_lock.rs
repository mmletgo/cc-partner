//! 交付 per-task 进程内锁（无仓储依赖，避免 repo ↔ delivery 循环）。
//!
//! Business Logic（为什么需要这个模块）:
//!     同一任务不能并发两条 delivery pipeline；Abort/Cancel 也不得与交付 merge/push 交叉，
//!     否则会出现任务 Aborted 但 main 已被推送的不一致。
//!
//! Code Logic（这个模块做什么）:
//!     进程内 HashSet 记录 in-flight task_id；提供 try_acquire 守卫与 is_in_progress 查询。

use crate::error::AppError;
use std::collections::HashSet;
use std::sync::{Mutex as StdMutex, OnceLock};

static DELIVERY_TASK_LOCKS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();

/// 单任务 delivery 执行权守卫。
///
/// Business Logic（为什么需要这个结构体）:
///     同一个 Orchestrator 任务不能同时执行两条自动交付流水线。
///
/// Code Logic（这个结构体做什么）:
///     记录已领取的 task_id；Drop 时从进程内 HashSet 移除。
pub(crate) struct DeliveryTaskGuard {
    task_id: String,
}

impl Drop for DeliveryTaskGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     delivery pipeline 结束后必须释放任务执行权。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Drop 中短暂锁定全局 HashSet 并移除 task_id；锁中毒时静默跳过。
    fn drop(&mut self) {
        if let Some(locks) = DELIVERY_TASK_LOCKS.get() {
            if let Ok(mut locked) = locks.lock() {
                locked.remove(&self.task_id);
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     Abort/Cancel 若与交付 push 并发，可在状态检查后仍把远端 main 推到已终止任务的 merge。
///
/// Code Logic（这个函数做什么）:
///     查询全局 HashSet 是否含 task_id；锁中毒视为占用（fail closed）。
pub(crate) fn is_delivery_task_in_progress(task_id: &str) -> bool {
    let Some(locks) = DELIVERY_TASK_LOCKS.get() else {
        return false;
    };
    match locks.lock() {
        Ok(locked) => locked.contains(task_id),
        Err(_) => true,
    }
}

/// Business Logic（为什么需要这个函数）:
///     自动交付需要 per-task 执行权，防止重复点击或并发命令同时执行 Git side effect。
///
/// Code Logic（这个函数做什么）:
///     短暂锁定全局 HashSet；首次插入 task_id 返回守卫，已存在返回 None。
pub(crate) fn try_acquire_delivery_task_guard(
    task_id: &str,
) -> Result<Option<DeliveryTaskGuard>, AppError> {
    let locks = DELIVERY_TASK_LOCKS.get_or_init(|| StdMutex::new(HashSet::new()));
    let mut locked = locks
        .lock()
        .map_err(|_| AppError::generic("Orchestrator delivery lock 已损坏"))?;
    if !locked.insert(task_id.to_string()) {
        return Ok(None);
    }
    Ok(Some(DeliveryTaskGuard {
        task_id: task_id.to_string(),
    }))
}
