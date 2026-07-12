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
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

/// 单条登记项：任务实体 + 取消令牌。
struct Entry {
    task: TransferTask,
    cancel: CancellationToken,
}

/// finalize 终态结果（Finding 4）。
///
/// Business Logic（为什么需要这个枚举）:
///     finalize 完成后任务从 registry 移除，重放的最后一块请求需要从墓碑还原"成功落地"或
///     "失败原因"的结果。成功路径携带最终文件名（可能因冲突加了 (1)/(2)）与绝对路径，
///     失败路径携带错误原因，便于 status 查询与日志诊断。
#[derive(Clone, Debug)]
pub enum TransferOutcome {
    /// 成功：文件已重命名落地。`final_filename` 可能因冲突被加了 (1)/(2) 后缀；
    /// `file_path` 是最终绝对路径。
    Completed {
        final_filename: String,
        file_path: String,
    },
    /// 失败：含错误原因（SHA256 校验失败、重命名失败、读取临时文件失败等）。
    Failed { error: String },
}

/// 终态墓碑：finalize 完成后短期保留，用于幂等响应重放的最后一块请求与 status 查询。
///
/// Business Logic（Finding 4: 为什么需要墓碑）:
///     传输的最后一块在 finalize 后会从 registry 移除。若传输层自动重试同一最后一块，
///     registry.get 返回 None，handle_chunk 直接返回 `{success:false}`，发送端误判失败。
///     墓碑保存第一次 finalize 的成功/失败结果，重放命中后返回同一结果，保证"最后一块
///     重放安全"。TTL 由 `TOMBSTONE_TTL_SECS` 控制，访问时惰性淘汰。
#[derive(Clone, Debug)]
pub struct TransferTombstone {
    /// 终态结果（含最终文件名/路径或失败原因）。
    pub outcome: TransferOutcome,
    /// finalize 完成时已写入字节数（成功路径一般 == size）。
    pub received_bytes: u64,
    /// 任务原始 size（status 响应需要）。
    pub size: u64,
    /// 任务原始 filename（status 响应需要）。
    pub filename: String,
    /// finalize 完成时间 ISO 字符串（业务时间，与 created_at 区分）。
    pub completed_at: String,
    /// 墓碑写入时刻（用于 TTL 惰性淘汰）。
    pub created_at: Instant,
}

/// 墓碑保留时长（Finding 4）：超过此时间的墓碑在访问时被惰性清理。
///
/// Business Logic: 5 分钟覆盖传输层最坏重试窗口（reqwest 默认连接/读取超时 + 指数退避），
/// 之后清理避免内存无限增长。传输 ID 在会话生命周期内唯一，TTL 内墓碑数量有界。
const TOMBSTONE_TTL_SECS: u64 = 300;

/// 惰性淘汰过期墓碑，避免无限增长。
///
/// Code Logic: 用 `Instant::now()` 与每条墓碑的 `created_at` 比对，超过 TTL 的 retain 移除。
/// 仅在持有 `tombstones` 锁时调用。
fn prune_expired_tombstones(map: &mut HashMap<String, TransferTombstone>) {
    let now = Instant::now();
    map.retain(|_, tomb| now.duration_since(tomb.created_at).as_secs() < TOMBSTONE_TTL_SECS);
}

/// 活跃传输任务登记表，跨发送端/接收端共享。
#[derive(Clone)]
pub struct TransferRegistry {
    inner: Arc<RwLock<HashMap<String, Entry>>>,
    /// finalize 单飞锁：每个 transfer_id 一把 tokio Mutex，确保并发重复最后一块串行 finalize
    /// （Finding 4）。锁在 transfer_id 生命周期内常驻，TTL 由 tombstones 间接覆盖。
    finalize_locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    /// 终态墓碑表：finalize 完成后短期保留，重放的最后一块请求与 status 查询命中后返回同一结果。
    tombstones: Arc<Mutex<HashMap<String, TransferTombstone>>>,
}

impl TransferRegistry {
    /// 构造空登记表。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            finalize_locks: Arc::new(Mutex::new(HashMap::new())),
            tombstones: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 获取某 transfer_id 的 chunk/finalize 单飞锁（Finding 4 + fix6）。
    ///
    /// Business Logic（为什么需要这个方法）:
    ///     最后一块请求可能在传输层被重试。锁必须覆盖 re-read 任务/墓碑、open/write、
    ///     进度更新与 finalize：若只在 finalize 前加锁，迟到请求可在 rename 后仍持旧 fd
    ///     改写已校验文件。第一个请求持锁完成 finalize（任务移除 + 写墓碑），
    ///     第二个请求拿锁后先命中墓碑，不得再 open 文件。
    ///
    /// Code Logic: 锁 map 用 std Mutex（无 await），按 id 取/建 Arc<AsyncMutex>（持锁跨越 await）。
    pub fn finalize_lock(&self, id: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self.finalize_locks.lock().expect("finalize_locks 写锁中毒");
        map.entry(id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// 写入终态墓碑（Finding 4）。finalize 完成后调用。
    ///
    /// Business Logic: 保留第一次 finalize 的成功/失败结果 + 接收字节数，重放的最后一块
    /// 请求与 status 查询命中后返回同一结果，避免误判为 `success:false`/`unknown`。
    pub fn record_tombstone(&self, id: &str, tomb: TransferTombstone) {
        let mut map = self.tombstones.lock().expect("tombstones 写锁中毒");
        prune_expired_tombstones(&mut map);
        map.insert(id.to_string(), tomb);
    }

    /// 查询终态墓碑（克隆），不存在返回 None。重放的最后一块请求与 status 查询使用。
    pub fn tombstone(&self, id: &str) -> Option<TransferTombstone> {
        let mut map = self.tombstones.lock().expect("tombstones 读锁中毒");
        prune_expired_tombstones(&mut map);
        map.get(id).cloned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};

    fn make_task(id: &str) -> TransferTask {
        TransferTask {
            id: id.to_string(),
            filename: format!("{id}.txt"),
            file_path: format!("/tmp/{id}.tmp"),
            size: 100,
            sha256: "abc".to_string(),
            chunk_size: 1024,
            direction: TransferDirection::Receive,
            peer_device_id: String::new(),
            status: TransferStatus::Pending,
            transferred_bytes: 0,
            created_at: "2026-07-12T00:00:00Z".to_string(),
            completed_at: None,
        }
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     同一 transfer_id 多次拿 finalize 单飞锁必须返回同一个 Arc，否则 finalize 串行化失效。
    #[tokio::test]
    async fn finalize_lock_returns_same_arc_for_same_id() {
        let reg = TransferRegistry::new();
        let a = reg.finalize_lock("t1");
        let b = reg.finalize_lock("t1");
        assert!(
            Arc::ptr_eq(&a, &b),
            "同一 transfer_id 应返回同一个 Arc<AsyncMutex>"
        );
        let c = reg.finalize_lock("t2");
        assert!(
            !Arc::ptr_eq(&a, &c),
            "不同 transfer_id 应返回不同 Arc<AsyncMutex>"
        );
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     finalize 单飞锁必须真的互斥：一个 task 持锁时，另一个 task 必须等待。
    #[tokio::test]
    async fn finalize_lock_provides_mutual_exclusion() {
        let reg = TransferRegistry::new();
        let lock = reg.finalize_lock("t1");
        let g1 = lock.clone();
        let g2 = lock.clone();

        let held = Arc::new(AsyncMutex::new(()));
        let held_inner = held.clone();
        // 第一个任务持锁。
        let g1_handle = tokio::spawn(async move {
            let _g = g1.lock().await;
            // 持锁期间设置标记。
            {
                let _h = held_inner.lock().await;
            }
            // 模拟 finalize 工作时长。
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        // 让第一个任务先抢到锁。
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // 第二个任务应被阻塞直到第一个完成。
        let start = std::time::Instant::now();
        let _g2 = g2.lock().await;
        let elapsed = start.elapsed();
        g1_handle.await.unwrap();
        // 应至少等待到第一个任务释放锁（>= 40ms 表示确实等待了）。
        assert!(
            elapsed >= std::time::Duration::from_millis(30),
            "第二个任务应被单飞锁阻塞，实际仅等待 {:?}",
            elapsed
        );
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     写入 Completed 墓碑后，handle_chunk 重放与 handle_status 都应命中并返回成功结果。
    #[test]
    fn tombstone_round_trip_completed() {
        let reg = TransferRegistry::new();
        let task = make_task("t1");
        reg.add(task.clone());
        reg.remove("t1"); // 模拟 finalize 完成后移除。
        reg.record_tombstone(
            "t1",
            TransferTombstone {
                outcome: TransferOutcome::Completed {
                    final_filename: "t1.txt".to_string(),
                    file_path: "/tmp/t1.txt".to_string(),
                },
                received_bytes: task.size,
                size: task.size,
                filename: task.filename.clone(),
                completed_at: "2026-07-12T00:00:01Z".to_string(),
                created_at: Instant::now(),
            },
        );
        let tomb = reg.tombstone("t1").expect("墓碑应可读回");
        match tomb.outcome {
            TransferOutcome::Completed { final_filename, .. } => {
                assert_eq!(final_filename, "t1.txt");
            }
            other => panic!("应为 Completed，实际: {other:?}"),
        }
        assert_eq!(tomb.received_bytes, 100);
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     Failed 墓碑需保留错误原因，重放的最后一块据此返回 success:false 而非误判成功。
    #[test]
    fn tombstone_round_trip_failed() {
        let reg = TransferRegistry::new();
        reg.record_tombstone(
            "t1",
            TransferTombstone {
                outcome: TransferOutcome::Failed {
                    error: "SHA256 校验失败".to_string(),
                },
                received_bytes: 80,
                size: 100,
                filename: "t1.txt".to_string(),
                completed_at: "2026-07-12T00:00:01Z".to_string(),
                created_at: Instant::now(),
            },
        );
        let tomb = reg.tombstone("t1").expect("墓碑应可读回");
        match &tomb.outcome {
            TransferOutcome::Failed { error } => assert_eq!(error, "SHA256 校验失败"),
            other => panic!("应为 Failed，实际: {other:?}"),
        }
        assert_eq!(tomb.received_bytes, 80);
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     查询不存在的 transfer_id 应返回 None（registry 与墓碑都 miss）。
    #[test]
    fn tombstone_returns_none_when_absent() {
        let reg = TransferRegistry::new();
        assert!(reg.tombstone("never-existed").is_none());
    }

    /// Business Logic（Finding 4: 为什么需要这个测试）:
    ///     墓碑必须有 TTL 淘汰，否则内存无限增长。写入一条"过期"墓碑应被惰性清理。
    #[test]
    fn tombstone_prunes_expired_entries() {
        let reg = TransferRegistry::new();
        // 直接写入 map，构造一条已过期的墓碑（created_at 早于 TTL）。
        {
            let mut map = reg.tombstones.lock().unwrap();
            map.insert(
                "expired".to_string(),
                TransferTombstone {
                    outcome: TransferOutcome::Failed {
                        error: "old".to_string(),
                    },
                    received_bytes: 0,
                    size: 0,
                    filename: String::new(),
                    completed_at: "2026-07-12T00:00:01Z".to_string(),
                    created_at: Instant::now()
                        - std::time::Duration::from_secs(TOMBSTONE_TTL_SECS + 60),
                },
            );
        }
        // 访问应触发惰性清理并返回 None。
        assert!(reg.tombstone("expired").is_none(), "过期墓碑应被淘汰");
    }
}
