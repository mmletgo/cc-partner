//! 交付 per-task 进程内锁（无仓储依赖，避免 repo ↔ delivery 循环）。
//!
//! Business Logic（为什么需要这个模块）:
//!     同一任务不能并发两条 delivery pipeline；Abort/Cancel 也不得与交付 merge/push 交叉，
//!     否则会出现任务 Aborted 但 main 已被推送的不一致。
//!
//! Code Logic（这个模块做什么）:
//!     进程内 HashSet 记录 in-flight task_id；提供 try_acquire 守卫与 is_in_progress 查询。
//!     owner 侧 SQLite 租约见 `OrchestratorRepo::try_acquire_delivery_lease`（跨 GuiClient 可见）。

use crate::error::AppError;
use crate::orchestrator::repo::OrchestratorRepo;
use std::collections::HashSet;
use std::sync::{Mutex as StdMutex, OnceLock};

static DELIVERY_TASK_LOCKS: OnceLock<StdMutex<HashSet<String>>> = OnceLock::new();

/// delivery 默认租约 TTL（秒）：覆盖 commit/push/merge 慢路径，过期后 abort 可恢复。
pub(crate) const DELIVERY_LEASE_TTL_SECS: i64 = 900;

/// 单任务 delivery 执行权守卫（进程内 + 可选 owner DB 租约）。
///
/// Business Logic（为什么需要这个结构体）:
///     同一个 Orchestrator 任务不能同时执行两条自动交付流水线；DB 租约让跨进程 abort 可见。
///
/// Code Logic（这个结构体做什么）:
///     记录已领取的 task_id 与可选 (repo, holder)；Drop 时释放 HashSet，并 best-effort 异步释放 DB 租约。
pub(crate) struct DeliveryTaskGuard {
    task_id: String,
    db_lease: Option<(OrchestratorRepo, String)>,
}

impl Drop for DeliveryTaskGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     delivery pipeline 结束后必须释放任务执行权与 owner 租约。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在 Drop 中短暂锁定全局 HashSet 并移除 task_id；若持有 DB 租约则 spawn 异步 release。
    fn drop(&mut self) {
        if let Some(locks) = DELIVERY_TASK_LOCKS.get() {
            if let Ok(mut locked) = locks.lock() {
                locked.remove(&self.task_id);
            }
        }
        if let Some((repo, holder)) = self.db_lease.take() {
            let task_id = self.task_id.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(err) = repo.release_delivery_lease(&task_id, &holder).await {
                        tracing::warn!(
                            task_id = %task_id,
                            "release delivery lease on drop failed: {err}"
                        );
                    }
                });
            }
        }
    }
}

impl DeliveryTaskGuard {
    /// Business Logic（为什么需要这个函数）:
    ///     进程内锁成功后还要在 owner SQLite 占位，防止 GuiClient abort 与交付交叉。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 holder UUID，调用 `try_acquire_delivery_lease`；失败返回 false（调用方应 drop 守卫）。
    pub(crate) async fn attach_db_lease(
        &mut self,
        repo: &OrchestratorRepo,
        ttl_secs: i64,
    ) -> Result<bool, AppError> {
        let holder = uuid::Uuid::new_v4().to_string();
        let acquired = repo
            .try_acquire_delivery_lease(&self.task_id, &holder, ttl_secs)
            .await?;
        if acquired {
            self.db_lease = Some((repo.clone(), holder));
        }
        Ok(acquired)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     pipeline 正常结束路径必须 **await** 释放 DB 租约，不能只依赖 Drop+spawn
    ///     （runtime 关闭或 spawn 延迟时 abort 会被挡住直至 TTL）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     若持有 db_lease 则 `take` 后 `await release_delivery_lease`；之后 Drop 不再二次释放。
    pub(crate) async fn release_db_lease_now(&mut self) -> Result<(), AppError> {
        if let Some((repo, holder)) = self.db_lease.take() {
            repo.release_delivery_lease(&self.task_id, &holder).await?;
        }
        Ok(())
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
        db_lease: None,
    }))
}
