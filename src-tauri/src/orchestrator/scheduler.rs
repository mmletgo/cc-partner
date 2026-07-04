//! Orchestrator project-scoped scheduler.
//!
//! Business Logic（为什么需要这个模块）:
//!     自动编排器需要按项目配置定期领取 Queued 任务，并把任务交给可见 Workbench Runner。
//!
//! Code Logic（这个模块做什么）:
//!     提供后台 scheduler runtime、单次 dispatch 入口和并发容量相关纯 helper。

use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskStatus;
use crate::orchestrator::runner::prepare_visible_runner;
use crate::state::AppState;
use std::time::Duration;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

const SCHEDULER_TICK_SECS: u64 = 10;

/// Orchestrator scheduler 运行时句柄。
///
/// Business Logic（为什么需要这个结构体）:
///     应用启动后需要保存后台调度循环的取消令牌，退出时能优雅停止自动任务领取。
///
/// Code Logic（这个结构体做什么）:
///     包装 tokio CancellationToken，并提供轻量 clone 与读取 token 的方法。
#[derive(Clone)]
pub struct OrchestratorRuntime {
    cancel: CancellationToken,
}

impl OrchestratorRuntime {
    /// Business Logic（为什么需要这个函数）:
    ///     scheduler 启动时需要创建一份新的取消令牌供后台循环和 AppState 共享。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造包含 CancellationToken::new() 的 OrchestratorRuntime。
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     lib.rs 需要拿到取消令牌并存入 AppState，后台 task 也需要持有同一令牌。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone 内部 CancellationToken；clone 后 cancel 会广播到所有持有者。
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}

/// Business Logic（为什么需要这个函数）:
///     应用启动后应自动轮询 Orchestrator 队列，无需用户保持页面打开。
///
/// Code Logic（这个函数做什么）:
///     使用 tauri async runtime 启动后台循环，每 10 秒调用 dispatch_once，收到 cancel 后退出。
pub fn start_orchestrator_scheduler(app_handle: AppHandle, state: AppState) -> CancellationToken {
    let runtime = OrchestratorRuntime::new();
    let cancel = runtime.cancel_token();
    let task_cancel = runtime.cancel_token();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    tracing::info!("Orchestrator scheduler 已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(SCHEDULER_TICK_SECS)) => {
                    if let Err(err) = dispatch_once(&state, app_handle.clone()).await {
                        tracing::error!("Orchestrator scheduler dispatch 失败: {err}");
                    }
                }
            }
        }
    });
    cancel
}

/// Business Logic（为什么需要这个函数）:
///     scheduler tick 和手动命令都需要执行同一套领取逻辑，避免 UI 手动触发与后台行为不一致。
///
/// Code Logic（这个函数做什么）:
///     遍历 enabled 项目配置，在 repo 事务内按并发容量原子 claim 一个 Queued 任务并交给 runner；
///     runner 失败时把任务置为 Blocked 并追加事件，成功时计入 dispatched。
pub async fn dispatch_once(state: &AppState, app_handle: AppHandle) -> Result<usize, AppError> {
    let configs = state
        .orchestrator_repo
        .list_enabled_project_configs()
        .await?;
    let mut dispatched = 0usize;
    for config in configs {
        let Some(task) = state
            .orchestrator_repo
            .claim_next_queued_task_with_capacity(&config.project_id, config.max_concurrent_tasks)
            .await?
        else {
            continue;
        };

        match prepare_visible_runner(state, app_handle.clone(), &task).await {
            Ok(_) => {
                dispatched += 1;
            }
            Err(err) => {
                let reason = err.to_string();
                state
                    .orchestrator_repo
                    .set_task_status(&task.id, OrchestratorTaskStatus::Blocked, Some(&reason))
                    .await?;
                state
                    .orchestrator_repo
                    .add_event(
                        &task.id,
                        "blocked",
                        &format!("Runner 准备失败: {reason}"),
                        None,
                    )
                    .await?;
            }
        }
    }
    Ok(dispatched)
}

/// Business Logic（为什么需要这个函数）:
///     项目配置的并发上限可能被设为 0 或负数，调度器需要安全地把它解释为不调度。
///
/// Code Logic（这个函数做什么）:
///     返回 max_concurrent_tasks - active_count 的非负剩余容量；非正上限直接返回 0。
#[cfg(test)]
pub(crate) fn dispatch_capacity(max_concurrent_tasks: i64, active_count: i64) -> i64 {
    if max_concurrent_tasks <= 0 {
        return 0;
    }
    (max_concurrent_tasks - active_count).max(0)
}

#[cfg(test)]
mod tests {
    /// Business Logic（为什么需要这个函数）:
    ///     项目并发上限为 0 或负数时必须视为不调度，避免错误配置触发自动执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用容量 helper，断言非正上限和已满项目都返回 0，未满项目返回剩余容量。
    #[test]
    fn dispatch_capacity_clamps_non_positive_limits() {
        assert_eq!(super::dispatch_capacity(0, 0), 0);
        assert_eq!(super::dispatch_capacity(-2, 0), 0);
        assert_eq!(super::dispatch_capacity(2, 2), 0);
        assert_eq!(super::dispatch_capacity(3, 1), 2);
    }
}
