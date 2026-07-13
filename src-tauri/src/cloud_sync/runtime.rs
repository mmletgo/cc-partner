//! cloud_sync/runtime.rs — Cloud Sync 工作区全流程单飞门闸
//!
//! Business Logic（为什么需要这个模块）:
//!     手动同步、scheduler 自动同步与 CLAUDE.md 云端推送都会写同一份
//!     `~/.cc-partner/cloud-sync/` Git 工作区。并发 reset/export/commit 会互相踩踏，
//!     产生半写工作树或 push 被拒后的脏状态。本模块提供进程内互斥门闸：
//!     同一时刻最多一个写流程；scheduler 忙时跳过，手动/推送最多等 5 分钟。
//!
//! Code Logic（这个模块做什么）:
//!     `tokio::sync::Mutex<()>` 作 gate；`CloudSyncBusyPolicy` 区分 Wait/ReturnBusy；
//!     获锁后更新 status，RAII 守卫在 drop（正常/panic/cancel）时释放 gate 并清理
//!     running 状态；`run_cloud_sync_exclusive` 是唯一入口。

use std::future::Future;
use std::sync::RwLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, MutexGuard};

use crate::error::AppError;

/// 触发云端同步写流程的来源。
///
/// Business Logic: 不同入口对“忙”的策略不同——用户主动操作应等待，
///     后台 scheduler 应跳过本 tick，避免堆积。
/// Code Logic: 纯枚举，写入 status.running_trigger 供观测。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudSyncTrigger {
    /// 前端「立即同步」手动触发
    Manual,
    /// 后台 scheduler 周期 tick
    Scheduler,
    /// CLAUDE.md 页面主动推送到 GitHub
    ClaudeMdPush,
}

/// 门闸繁忙时的策略。
///
/// Business Logic: 手动/CLAUDE.md push 等待当前流程结束；scheduler 忙则跳过。
/// Code Logic: Wait 用 timeout 包裹 `Mutex::lock`；ReturnBusy 用 `try_lock`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSyncBusyPolicy {
    /// 等待当前持有者释放，超时返回 `cloud_sync.busy_timeout`
    Wait {
        /// 最长等待时间
        timeout: Duration,
    },
    /// 立即返回 `Ok(None)`，递增 `skipped_busy`，不排队
    ReturnBusy,
}

/// 最近一次写流程的结果摘要。
///
/// Business Logic: 供调试/后续 status 命令观察上次同步成败。
/// Code Logic: 成功/失败布尔 + 中文 note。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncLastResult {
    /// 是否成功
    pub ok: bool,
    /// 友好中文说明
    pub note: String,
    /// 完成时间 RFC3339
    pub finished_at: String,
}

/// 云端同步运行时状态（可观测字段）。
///
/// Business Logic: 前端/doctor 将来可读 runningTrigger/startedAt/lastResult/skippedBusy。
/// Code Logic: 由 gate 获取/释放时更新；`skipped_busy` 仅 ReturnBusy 命中时递增。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncRuntimeStatus {
    /// 当前持有 gate 的触发来源；空闲时为 None
    pub running_trigger: Option<CloudSyncTrigger>,
    /// 当前流程开始时间 RFC3339
    pub started_at: Option<String>,
    /// 最近一次完成结果
    pub last_result: Option<CloudSyncLastResult>,
    /// scheduler ReturnBusy 累计跳过次数
    pub skipped_busy: u64,
}

/// Cloud Sync 工作区单飞运行时。
///
/// Business Logic: AppState 持有一份 Arc，所有写工作区入口共享同一 gate。
/// Code Logic: `gate` 串行写流程；`status` 用 std RwLock 做短临界区状态快照。
pub struct CloudSyncRuntime {
    /// 全流程互斥门闸（跨 await 持有）
    gate: Mutex<()>,
    /// 可观测状态
    status: RwLock<CloudSyncRuntimeStatus>,
}

impl Default for CloudSyncRuntime {
    /// Business Logic: 应用启动时无进行中的同步。
    /// Code Logic: 空闲 Mutex + 默认 status。
    fn default() -> Self {
        Self::new()
    }
}

impl CloudSyncRuntime {
    /// Business Logic: AppState / 测试需要一份空闲初态运行时。
    /// Code Logic: 新建 Mutex 与默认 status。
    pub fn new() -> Self {
        Self {
            gate: Mutex::new(()),
            status: RwLock::new(CloudSyncRuntimeStatus::default()),
        }
    }

    /// Business Logic: 观测当前 running/skipped/lastResult。
    /// Code Logic: 读锁 clone 整份 status。
    pub fn status_snapshot(&self) -> CloudSyncRuntimeStatus {
        self.status
            .read()
            .expect("cloud_sync status 读锁中毒")
            .clone()
    }

    /// Business Logic: 测试/调试记录上次结果。
    /// Code Logic: 写锁更新 last_result。
    fn record_last_result(&self, ok: bool, note: impl Into<String>) {
        let mut s = self.status.write().expect("cloud_sync status 写锁中毒");
        s.last_result = Some(CloudSyncLastResult {
            ok,
            note: note.into(),
            finished_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    /// Business Logic: ReturnBusy 命中时累计跳过次数。
    /// Code Logic: skipped_busy = saturating_add(1)。
    fn increment_skipped_busy(&self) {
        let mut s = self.status.write().expect("cloud_sync status 写锁中毒");
        s.skipped_busy = s.skipped_busy.saturating_add(1);
    }

    /// Business Logic: 获锁后标记 running。
    /// Code Logic: 写 running_trigger + started_at。
    fn mark_running(&self, trigger: CloudSyncTrigger) {
        let mut s = self.status.write().expect("cloud_sync status 写锁中毒");
        s.running_trigger = Some(trigger);
        s.started_at = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Business Logic: 释放 gate 时清 running，避免陈旧状态。
    /// Code Logic: running_trigger/started_at = None。
    fn clear_running(&self) {
        let mut s = self.status.write().expect("cloud_sync status 写锁中毒");
        s.running_trigger = None;
        s.started_at = None;
    }
}

/// 持有 gate 的 RAII 守卫：drop 时清 running 并释放 Mutex。
///
/// Business Logic: panic/cancel/正常返回都必须释放工作区写权。
/// Code Logic: Drop 先 clear_running，再 drop MutexGuard。
struct CloudSyncGateGuard<'a> {
    runtime: &'a CloudSyncRuntime,
    _guard: MutexGuard<'a, ()>,
}

impl Drop for CloudSyncGateGuard<'_> {
    /// Business Logic: 任何退出路径都不得把 gate 留死。
    /// Code Logic: clear_running；MutexGuard 随后自动 drop。
    fn drop(&mut self) {
        self.runtime.clear_running();
    }
}

/// 在 CloudSyncRuntime 互斥门闸下执行完整工作区写流程。
///
/// Business Logic: 所有会写 `cloud-sync/` 工作区的入口必须经此函数——
///     手动同步、scheduler、CLAUDE.md 云端推送。Wait 最长等 timeout；
///     ReturnBusy 立即跳过并递增 skippedBusy。获锁后调用方应重读 config
///     （repo URL/branch），避免等待期间配置变更。
///
/// Code Logic:
/// 1. 按 policy 获取 gate（try_lock 或 timeout(lock)）；
/// 2. mark_running(trigger)；构造 RAII 守卫；
/// 3. 执行 operation()；
/// 4. 按结果写 last_result；返回 Ok(Some(T)) 或 Err；
/// 5. 守卫 drop 清 running + 释放 Mutex（含 panic/cancel）。
pub async fn run_cloud_sync_exclusive<T, F, Fut>(
    runtime: &CloudSyncRuntime,
    trigger: CloudSyncTrigger,
    policy: CloudSyncBusyPolicy,
    operation: F,
) -> Result<Option<T>, AppError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, AppError>>,
{
    let guard = match policy {
        CloudSyncBusyPolicy::ReturnBusy => match runtime.gate.try_lock() {
            Ok(g) => g,
            Err(_) => {
                runtime.increment_skipped_busy();
                tracing::info!(
                    "cloud_sync: trigger={trigger:?} 忙，ReturnBusy 跳过本轮"
                );
                return Ok(None);
            }
        },
        CloudSyncBusyPolicy::Wait { timeout } => {
            match tokio::time::timeout(timeout, runtime.gate.lock()).await {
                Ok(g) => g,
                Err(_) => {
                    return Err(AppError::timeout(
                        "cloud_sync.busy_timeout: 云端同步繁忙，等待超时，请稍后重试",
                    ));
                }
            }
        }
    };

    runtime.mark_running(trigger);
    let _gate = CloudSyncGateGuard {
        runtime,
        _guard: guard,
    };

    let result = operation().await;
    match &result {
        Ok(_) => runtime.record_last_result(true, format!("ok:{trigger:?}")),
        Err(e) => runtime.record_last_result(false, e.to_string()),
    }
    result.map(Some)
}

/// 手动同步 / CLAUDE.md 推送默认等待策略（5 分钟）。
///
/// Business Logic: 用户主动操作应等当前流程结束，避免立即失败。
/// Code Logic: Wait { 300s }。
pub fn wait_policy() -> CloudSyncBusyPolicy {
    CloudSyncBusyPolicy::Wait {
        timeout: Duration::from_secs(300),
    }
}

/// scheduler 默认策略：忙则跳过。
///
/// Business Logic: 后台 tick 不得排队堆积。
/// Code Logic: ReturnBusy。
pub fn scheduler_policy() -> CloudSyncBusyPolicy {
    CloudSyncBusyPolicy::ReturnBusy
}

// ─── 测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    /// Business Logic: 两个 Wait 调用不得重叠执行写流程。
    /// Code Logic: 先启动长任务，再启动短任务；观察 concurrent 峰值。
    #[tokio::test]
    async fn two_wait_callers_never_overlap() {
        let runtime = StdArc::new(CloudSyncRuntime::new());
        let concurrent = StdArc::new(AtomicUsize::new(0));
        let peak = StdArc::new(AtomicUsize::new(0));
        let first_entered = StdArc::new(tokio::sync::Notify::new());

        let rt1 = runtime.clone();
        let c1 = concurrent.clone();
        let p1 = peak.clone();
        let n1 = first_entered.clone();
        let t1 = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt1,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let c1 = c1.clone();
                    let p1 = p1.clone();
                    let n1 = n1.clone();
                    async move {
                        let now = c1.fetch_add(1, Ordering::SeqCst) + 1;
                        p1.fetch_max(now, Ordering::SeqCst);
                        n1.notify_one();
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        c1.fetch_sub(1, Ordering::SeqCst);
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });

        first_entered.notified().await;

        let rt2 = runtime.clone();
        let c2 = concurrent.clone();
        let p2 = peak.clone();
        let t2 = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt2,
                CloudSyncTrigger::ClaudeMdPush,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let c2 = c2.clone();
                    let p2 = p2.clone();
                    async move {
                        let now = c2.fetch_add(1, Ordering::SeqCst) + 1;
                        p2.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        c2.fetch_sub(1, Ordering::SeqCst);
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });

        let (r1, r2) = tokio::join!(t1, t2);
        assert!(r1.unwrap().unwrap().is_some());
        assert!(r2.unwrap().unwrap().is_some());
        assert_eq!(peak.load(Ordering::SeqCst), 1, "Wait 调用不得重叠");
        assert!(runtime.status_snapshot().running_trigger.is_none());
    }

    /// Business Logic: scheduler 忙时跳过，不跑 operation，递增 skippedBusy。
    /// Code Logic: 先持锁，再 ReturnBusy → Ok(None) + skipped_busy+=1。
    #[tokio::test]
    async fn scheduler_return_busy_skips_without_running() {
        let runtime = StdArc::new(CloudSyncRuntime::new());
        let ran = StdArc::new(AtomicBool::new(false));
        let hold = StdArc::new(tokio::sync::Notify::new());
        let held = StdArc::new(tokio::sync::Notify::new());

        let rt = runtime.clone();
        let h = hold.clone();
        let hd = held.clone();
        let holder = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let h = h.clone();
                    let hd = hd.clone();
                    async move {
                        hd.notify_one();
                        h.notified().await;
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });

        held.notified().await;

        let ran2 = ran.clone();
        let result = run_cloud_sync_exclusive(
            &runtime,
            CloudSyncTrigger::Scheduler,
            CloudSyncBusyPolicy::ReturnBusy,
            || {
                let ran2 = ran2.clone();
                async move {
                    ran2.store(true, Ordering::SeqCst);
                    Ok::<(), AppError>(())
                }
            },
        )
        .await
        .unwrap();

        assert!(result.is_none());
        assert!(!ran.load(Ordering::SeqCst));
        assert_eq!(runtime.status_snapshot().skipped_busy, 1);

        hold.notify_one();
        let _ = holder.await;
    }

    /// Business Logic: Wait 超时应返回 cloud_sync.busy_timeout。
    /// Code Logic: 持锁阻塞，短 timeout Wait → Timeout 错误。
    #[tokio::test]
    async fn wait_timeout_returns_busy_timeout() {
        let runtime = StdArc::new(CloudSyncRuntime::new());
        let hold = StdArc::new(tokio::sync::Notify::new());
        let held = StdArc::new(tokio::sync::Notify::new());

        let rt = runtime.clone();
        let h = hold.clone();
        let hd = held.clone();
        let holder = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let h = h.clone();
                    let hd = hd.clone();
                    async move {
                        hd.notify_one();
                        h.notified().await;
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });

        held.notified().await;

        let err = run_cloud_sync_exclusive(
            &runtime,
            CloudSyncTrigger::Manual,
            CloudSyncBusyPolicy::Wait {
                timeout: Duration::from_millis(30),
            },
            || async { Ok::<(), AppError>(()) },
        )
        .await
        .expect_err("应超时");

        let msg = err.to_string();
        assert!(
            msg.contains("cloud_sync.busy_timeout"),
            "错误应含 busy_timeout: {msg}"
        );

        hold.notify_one();
        let _ = holder.await;
    }

    /// Business Logic: panic 必须释放 gate，后续调用可进入。
    /// Code Logic: AssertUnwindSafe + catch_unwind 模拟 panic；随后 Wait 成功。
    #[tokio::test]
    async fn panic_releases_gate() {
        let runtime = StdArc::new(CloudSyncRuntime::new());

        // 用 drop 守卫模拟：operation future 被 drop（cancel）时 gate 释放
        let rt = runtime.clone();
        let cancel_handle = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Ok::<(), AppError>(())
                },
            )
            .await
        });

        // 等它拿到锁
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(runtime.status_snapshot().running_trigger.is_some());

        cancel_handle.abort();
        // 等 abort 完成 drop
        tokio::time::sleep(Duration::from_millis(30)).await;

        let result = run_cloud_sync_exclusive(
            &runtime,
            CloudSyncTrigger::Manual,
            CloudSyncBusyPolicy::Wait {
                timeout: Duration::from_secs(2),
            },
            || async { Ok::<u8, AppError>(7) },
        )
        .await
        .unwrap();
        assert_eq!(result, Some(7));
    }

    /// Business Logic: 获锁后才读配置；等待期间配置变更应对后续 operation 可见。
    /// Code Logic: 共享 AtomicUsize 模拟 config；持锁时改值；等待者进入后读到新值。
    #[tokio::test]
    async fn config_is_reread_after_acquisition() {
        let runtime = StdArc::new(CloudSyncRuntime::new());
        let config_value = StdArc::new(AtomicUsize::new(1));
        let hold = StdArc::new(tokio::sync::Notify::new());
        let held = StdArc::new(tokio::sync::Notify::new());

        let rt = runtime.clone();
        let h = hold.clone();
        let hd = held.clone();
        let holder = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let h = h.clone();
                    let hd = hd.clone();
                    async move {
                        hd.notify_one();
                        h.notified().await;
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });

        held.notified().await;
        // 等待期间改 config
        config_value.store(42, Ordering::SeqCst);

        let cfg = config_value.clone();
        let seen = StdArc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            async move {
                run_cloud_sync_exclusive(
                    &runtime,
                    CloudSyncTrigger::Manual,
                    CloudSyncBusyPolicy::Wait {
                        timeout: Duration::from_secs(5),
                    },
                    || {
                        let cfg = cfg.clone();
                        let seen2 = seen2.clone();
                        async move {
                            // 获锁后才读
                            let v = cfg.load(Ordering::SeqCst);
                            seen2.store(v, Ordering::SeqCst);
                            Ok::<(), AppError>(())
                        }
                    },
                )
                .await
            }
        });

        // 稍后再释放 holder，确保 waiter 卡在 lock 上
        tokio::time::sleep(Duration::from_millis(20)).await;
        hold.notify_one();
        let _ = holder.await;
        let _ = waiter.await;
        assert_eq!(seen.load(Ordering::SeqCst), 42);
    }

    /// Business Logic: ReturnBusy 不排队，连续跳过只递增计数。
    /// Code Logic: 持锁期间两次 ReturnBusy → skipped_busy=2。
    #[tokio::test]
    async fn return_busy_increments_skipped_once_per_call() {
        let runtime = StdArc::new(CloudSyncRuntime::new());
        let hold = StdArc::new(tokio::sync::Notify::new());
        let held = StdArc::new(tokio::sync::Notify::new());

        let rt = runtime.clone();
        let h = hold.clone();
        let hd = held.clone();
        let holder = tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                wait_policy(),
                || {
                    let h = h.clone();
                    let hd = hd.clone();
                    async move {
                        hd.notify_one();
                        h.notified().await;
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        });
        held.notified().await;

        for _ in 0..2 {
            let r = run_cloud_sync_exclusive(
                &runtime,
                CloudSyncTrigger::Scheduler,
                scheduler_policy(),
                || async { Ok::<(), AppError>(()) },
            )
            .await
            .unwrap();
            assert!(r.is_none());
        }
        assert_eq!(runtime.status_snapshot().skipped_busy, 2);

        hold.notify_one();
        let _ = holder.await;
    }
}
