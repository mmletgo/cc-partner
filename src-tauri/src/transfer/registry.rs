//! transfer/registry.rs — 活跃传输任务表
//!
//! Business Logic（为什么需要这个模块）:
//!     发送端 / 接收端都需要一个并发安全的活跃传输任务登记表，用于：
//!     1) 存储当前进行中（pending/transferring）的 TransferTask，供 status 查询与 list_transfers 返回；
//!     2) 为每个任务关联一个 CancellationToken，cancel_transfer 命令可触发对应任务停止；
//!     对照 Python sender.py 的 `_tasks` / `_cancelled` 与 receiver.py 的 `_tasks`。
//!
//! Code Logic（这个模块做什么）:
//!     `TransferRegistry` 内部为 `RwLock<HashMap<String, (TransferTask, CancellationToken)>>`。
//!     - add：插入任务（附带新 CancellationToken）
//!     - get：只读克隆当前快照
//!     - update_progress：写锁更新 transferred_bytes / status
//!     - cancel：取出对应 CancellationToken 并 cancel()（sender/receiver 循环中检查 cancel.is_cancelled()）
//!     - remove：任务终态后移除（completed/failed/cancelled），持久化到 transfer_history
//!     - list：返回全部活跃任务快照（按 created_at 倒序，对照 Python list_tasks）

use crate::models::transfer::{TransferStatus, TransferTask};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio_util::sync::CancellationToken;

/// 单条登记项：任务实体 + 取消令牌。
struct Entry {
    task: TransferTask,
    cancel: CancellationToken,
}

/// 活跃传输任务登记表，跨发送端/接收端共享。
#[derive(Clone)]
pub struct TransferRegistry {
    inner: Arc<RwLock<HashMap<String, Entry>>>,
}

impl TransferRegistry {
    /// 构造空登记表。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 插入一个新任务（附带独立的 CancellationToken）。
    pub fn add(&self, task: TransferTask) {
        let id = task.id.clone();
        let entry = Entry {
            task,
            cancel: CancellationToken::new(),
        };
        self.inner
            .write()
            .expect("transfer registry 写锁中毒")
            .insert(id, entry);
    }

    /// 取某任务当前快照（克隆），不存在返回 None。
    pub fn get(&self, id: &str) -> Option<TransferTask> {
        self.inner
            .read()
            .expect("transfer registry 读锁中毒")
            .get(id)
            .map(|e| e.task.clone())
    }

    /// 取某任务的 CancellationToken 克隆（供 sender 异步循环持有并检查）。
    pub fn cancel_token(&self, id: &str) -> Option<CancellationToken> {
        self.inner
            .read()
            .expect("transfer registry 读锁中毒")
            .get(id)
            .map(|e| e.cancel.clone())
    }

    /// 更新任务进度（transferred_bytes）与状态。
    pub fn update_progress(&self, id: &str, transferred_bytes: u64, status: TransferStatus) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("transfer registry 写锁中毒")
            .get_mut(id)
        {
            entry.task.transferred_bytes = transferred_bytes;
            entry.task.status = status;
        }
    }

    /// 更新任务状态（不改进度）。
    pub fn set_status(&self, id: &str, status: TransferStatus) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("transfer registry 写锁中毒")
            .get_mut(id)
        {
            entry.task.status = status;
        }
    }

    /// 标记任务完成：设置 status=Completed 并回填 completed_at。
    pub fn mark_completed(&self, id: &str, completed_at: String, final_path: Option<String>) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("transfer registry 写锁中毒")
            .get_mut(id)
        {
            entry.task.status = TransferStatus::Completed;
            entry.task.completed_at = Some(completed_at);
            if let Some(p) = final_path {
                entry.task.file_path = p;
            }
            entry.task.transferred_bytes = entry.task.size;
        }
    }

    /// 标记任务失败：status=Failed + completed_at + 错误信息可选（错误信息通过事件 emit）。
    pub fn mark_failed(&self, id: &str, completed_at: String) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("transfer registry 写锁中毒")
            .get_mut(id)
        {
            entry.task.status = TransferStatus::Failed;
            entry.task.completed_at = Some(completed_at);
        }
    }

    /// 标记任务取消：status=Cancelled + completed_at。
    pub fn mark_cancelled(&self, id: &str, completed_at: String) {
        if let Some(entry) = self
            .inner
            .write()
            .expect("transfer registry 写锁中毒")
            .get_mut(id)
        {
            entry.task.status = TransferStatus::Cancelled;
            entry.task.completed_at = Some(completed_at);
        }
    }

    /// 触发取消：取出 CancellationToken 并 cancel()。返回是否找到并触发。
    pub fn cancel(&self, id: &str) -> bool {
        let token = self
            .inner
            .read()
            .expect("transfer registry 读锁中毒")
            .get(id)
            .map(|e| e.cancel.clone());
        if let Some(t) = token {
            t.cancel();
            true
        } else {
            false
        }
    }

    /// 移除任务（任务终态后调用，持久化交由调用方在 remove 前写入 transfer_history）。
    pub fn remove(&self, id: &str) -> Option<TransferTask> {
        self.inner
            .write()
            .expect("transfer registry 写锁中毒")
            .remove(id)
            .map(|e| e.task)
    }

    /// 列出全部活跃任务（按 created_at 倒序，对照 Python list_tasks）。
    pub fn list(&self) -> Vec<TransferTask> {
        let mut tasks: Vec<TransferTask> = self
            .inner
            .read()
            .expect("transfer registry 读锁中毒")
            .values()
            .map(|e| e.task.clone())
            .collect();
        tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        tasks
    }
}

impl Default for TransferRegistry {
    fn default() -> Self {
        Self::new()
    }
}
