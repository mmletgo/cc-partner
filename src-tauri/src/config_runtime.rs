//! config_runtime.rs — 配置内存态的串行事务更新
//!
//! Business Logic（为什么需要这个模块）:
//!     多个命令可能并发修改不同配置字段；若各自 clone→改→写盘→swap，会产生 lost update。
//!     需要单一 writer gate：clone → mutate → validate → durable save → memory swap。
//!     截图快捷键 OS 注册也必须与 config 事务同锁串行，避免 OS/config 分叉。
//!
//! Code Logic（这个模块做什么）:
//!     `ConfigRuntime` 持有 `Arc<RwLock<AppConfig>>`、异步 `update_lock` 与 `ConfigStore`；
//!     `update_config_transactionally` 串行化写路径，durable IO 走 `spawn_blocking`，
//!     且不把 std `RwLockGuard` 跨 await。

use crate::config::AppConfig;
use crate::config_store::ConfigStore;
use crate::error::AppError;
use std::sync::{Arc, RwLock};

/// 配置运行时：共享内存值 + 串行 writer + 持久化后端。
///
/// Business Logic（为什么需要这个结构）:
///     读路径需要廉价 clone；写路径必须串行并在落盘成功后才 swap，避免半提交状态。
///
/// Code Logic（这个结构做什么）:
///     `value` 供读；`update_lock` 串行事务；`store` 执行 durable save。
pub struct ConfigRuntime {
    pub value: Arc<RwLock<AppConfig>>,
    update_lock: tokio::sync::Mutex<()>,
    store: Arc<dyn ConfigStore>,
}

impl ConfigRuntime {
    /// 用已加载的配置与 store 构造 runtime。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     启动时 load 一次后注入共享状态，供命令层读写。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 value 与 store，初始化空 update_lock。
    pub fn new(initial: AppConfig, store: Arc<dyn ConfigStore>) -> Self {
        Self {
            value: Arc::new(RwLock::new(initial)),
            update_lock: tokio::sync::Mutex::new(()),
            store,
        }
    }

    /// 返回共享内存配置句柄（与 `value` 相同）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     迁移期 `AppState.config` 可与 runtime 共享同一 `Arc`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone `Arc<RwLock<AppConfig>>`。
    pub fn shared_value(&self) -> Arc<RwLock<AppConfig>> {
        Arc::clone(&self.value)
    }

    /// 只读克隆当前内存配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层读配置不应长时间持锁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     短暂读锁后 clone 并释放。
    pub fn snapshot(&self) -> Result<AppConfig, AppError> {
        self.value
            .read()
            .map(|g| g.clone())
            .map_err(|_| AppError::generic("配置读锁中毒"))
    }

    /// 获取串行 writer 锁（热键 OS 切换等需与 config 事务同临界区时使用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     截图快捷键 OS 注册必须与落盘/内存 swap 串行，命令层需要显式持有同一把 gate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `await` 异步 `update_lock`，返回守卫；持有期间其它 writer 阻塞。
    pub async fn lock_for_update(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.update_lock.lock().await
    }

    /// 返回可克隆的 store 句柄（供持锁路径 `spawn_blocking(save_atomic)`）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     热键同锁路径需在命令层自行 spawn_blocking 落盘，但仍必须走同一 ConfigStore。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone `Arc<dyn ConfigStore>`。
    pub fn store_handle(&self) -> Arc<dyn ConfigStore> {
        Arc::clone(&self.store)
    }

    /// 将已落盘成功的 candidate 写入内存（仅在持有 update_lock 且 save 成功后调用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     热键同锁路径在锁外 spawn_blocking 落盘后，需在同一临界区完成 memory swap。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁覆盖 `value`。
    pub fn swap_memory(&self, candidate: AppConfig) -> Result<(), AppError> {
        let mut write = self
            .value
            .write()
            .map_err(|_| AppError::generic("配置写锁中毒"))?;
        *write = candidate;
        Ok(())
    }
}

/// 串行事务更新配置：clone → mutate → validate → save_atomic → swap。
///
/// Business Logic（为什么需要这个函数）:
///     所有配置 writer 必须经此 helper，保证失败时内存与旧文件不变，成功时字段合并不丢。
///
/// Code Logic（这个函数做什么）:
///     1) 持有异步 `update_lock` 全程；2) 读锁 clone candidate 后立即释放；
///     3) mutate；4) `validate`；5) `spawn_blocking(store.save_atomic)`（不阻塞 runtime worker）；
///     6) 写锁 swap 内存；返回提交后的配置与 mutate 结果。
///     错误路径不做 memory swap（rename 已提交时 store 返回 Ok，故仍会 swap）。
///     热键 OS 副作用请走 `lock_for_update` + 命令层同临界区路径，不经过本 helper 的闭包钩子。
pub async fn update_config_transactionally<T, F>(
    runtime: &ConfigRuntime,
    mutate: F,
) -> Result<(AppConfig, T), AppError>
where
    F: FnOnce(&mut AppConfig) -> Result<T, AppError>,
{
    let _guard = runtime.update_lock.lock().await;

    let mut candidate = {
        let read = runtime
            .value
            .read()
            .map_err(|_| AppError::generic("配置读锁中毒"))?;
        read.clone()
    }; // 读锁在此释放，绝不跨 await

    let result = mutate(&mut candidate)?;
    candidate.validate()?;

    let store = Arc::clone(&runtime.store);
    let to_save = candidate.clone();
    // durable IO 放到 blocking 池：持有 tokio Mutex 保持单 writer，但不把 fsync 钉在 async worker 上。
    let save_result = tokio::task::spawn_blocking(move || store.save_atomic(&to_save))
        .await
        .map_err(|e| AppError::generic(format!("配置落盘任务失败: {e}")))?;
    save_result?;

    {
        let mut write = runtime
            .value
            .write()
            .map_err(|_| AppError::generic("配置写锁中毒"))?;
        *write = candidate.clone();
    }

    Ok((candidate, result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
    use crate::config_store::{
        ConfigIoStage, FaultInjectingConfigIo, FsConfigStore, MemoryConfigStore, StdConfigIo,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-rt-1".into(),
            device_name: "runtime-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+<shift>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
        }
    }

    #[tokio::test]
    async fn save_failure_leaves_memory_unchanged() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial.clone(), store.clone());

        let err = update_config_transactionally(&runtime, |cfg| {
            cfg.device_name = "mutated".into();
            Ok(())
        })
        .await
        .expect_err("save 失败应返回 Err");
        assert!(
            err.to_string().contains("注入故障") || err.to_string().contains("MemoryConfigStore"),
            "应是 store 注入错误: {err}"
        );

        let snap = runtime.snapshot().expect("snapshot");
        assert_eq!(snap.device_name, "runtime-device");
        assert_eq!(
            store.snapshot().unwrap().device_name,
            "runtime-device",
            "磁盘/store 侧也应保持旧值"
        );
    }

    #[tokio::test]
    async fn concurrent_writers_preserve_non_conflicting_patches() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::new(initial, store));

        let barrier = Arc::new(Barrier::new(2));
        let started = Arc::new(AtomicUsize::new(0));

        let r1 = Arc::clone(&runtime);
        let b1 = Arc::clone(&barrier);
        let s1 = Arc::clone(&started);
        let t1 = tokio::spawn(async move {
            s1.fetch_add(1, Ordering::SeqCst);
            b1.wait().await;
            update_config_transactionally(&r1, |cfg| {
                cfg.device_name = "name-from-a".into();
                Ok(())
            })
            .await
        });

        let r2 = Arc::clone(&runtime);
        let b2 = Arc::clone(&barrier);
        let s2 = Arc::clone(&started);
        let t2 = tokio::spawn(async move {
            s2.fetch_add(1, Ordering::SeqCst);
            b2.wait().await;
            update_config_transactionally(&r2, |cfg| {
                cfg.receive_dir = "/tmp/from-b".into();
                Ok(())
            })
            .await
        });

        let (r_a, r_b) = tokio::join!(t1, t2);
        r_a.expect("join a").expect("update a");
        r_b.expect("join b").expect("update b");

        let final_cfg = runtime.snapshot().expect("final");
        assert_eq!(final_cfg.device_name, "name-from-a");
        assert_eq!(final_cfg.receive_dir, "/tmp/from-b");
    }

    #[tokio::test]
    async fn validate_failure_does_not_save_or_swap() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = ConfigRuntime::new(initial.clone(), store.clone());

        let err = update_config_transactionally(&runtime, |cfg| {
            cfg.device_id = "".into();
            Ok(())
        })
        .await
        .expect_err("非法配置应失败");
        assert!(
            err.to_string().contains("device_id") || err.to_string().contains("设备"),
            "应是 validation 错误: {err}"
        );
        assert_eq!(runtime.snapshot().unwrap().device_id, initial.device_id);
        assert_eq!(store.snapshot().unwrap().device_id, initial.device_id);
    }

    /// H1：DirectorySync 故障发生在 rename 之后，内存必须跟随磁盘 NEW，避免后续 lost update。
    #[tokio::test]
    async fn directory_sync_fault_after_rename_still_swaps_memory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.json");
        let mut initial = sample_config();
        initial.device_name = "old-mem".into();
        initial.receive_dir = temp.path().join("recv").to_string_lossy().to_string();
        initial.db_path = temp.path().join("db.db").to_string_lossy().to_string();

        let seed = FsConfigStore::new(path.clone(), Arc::new(StdConfigIo));
        seed.save_atomic(&initial).expect("seed");

        let io = Arc::new(FaultInjectingConfigIo::fail_once(
            Arc::new(StdConfigIo),
            ConfigIoStage::DirectorySync,
        ));
        let store: Arc<dyn ConfigStore> = Arc::new(FsConfigStore::new(path.clone(), io));
        let runtime = ConfigRuntime::new(initial.clone(), store);

        let (committed, _) = update_config_transactionally(&runtime, |cfg| {
            cfg.device_name = "new-after-rename".into();
            Ok(())
        })
        .await
        .expect("rename 后 DirectorySync 失败仍应提交");

        assert_eq!(committed.device_name, "new-after-rename");
        let mem = runtime.snapshot().expect("snapshot");
        assert_eq!(
            mem.device_name, "new-after-rename",
            "内存必须 swap 到 NEW，禁止 disk=NEW/memory=OLD"
        );
        let disk: AppConfig =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk.device_name, "new-after-rename");

        // 后续 writer 基于 NEW 内存，不会丢失已提交字段。
        let (next, _) = update_config_transactionally(&runtime, |cfg| {
            cfg.receive_dir = "/tmp/only-recv".into();
            Ok(())
        })
        .await
        .expect("后续更新");
        assert_eq!(next.device_name, "new-after-rename");
        assert_eq!(next.receive_dir, "/tmp/only-recv");
    }

    /// H2：`lock_for_update` 与 config 事务同锁；并发 side-effect 不得交错。
    #[tokio::test]
    async fn lock_for_update_serializes_side_effects_with_writers() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::new(initial, store));
        let concurrent = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let spawn_one = |rt: Arc<ConfigRuntime>,
                         c: Arc<AtomicUsize>,
                         p: Arc<AtomicUsize>,
                         b: Arc<Barrier>,
                         name: &'static str| {
            tokio::spawn(async move {
                b.wait().await;
                let _guard = rt.lock_for_update().await;
                let now = c.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                // 模拟 OS 热键替换耗时
                std::thread::sleep(std::time::Duration::from_millis(30));
                let mut candidate = rt.snapshot().unwrap();
                candidate.device_name = name.into();
                let store = rt.store_handle();
                let to_save = candidate.clone();
                store.save_atomic(&to_save).unwrap();
                rt.swap_memory(candidate).unwrap();
                c.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), AppError>(())
            })
        };

        let t1 = spawn_one(
            runtime.clone(),
            concurrent.clone(),
            peak.clone(),
            barrier.clone(),
            "a",
        );
        let t2 = spawn_one(
            runtime.clone(),
            concurrent.clone(),
            peak.clone(),
            barrier.clone(),
            "b",
        );
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "side-effect 不得与其它 writer 重叠"
        );
    }

    /// 持锁路径 save 失败时不得 swap 内存（补偿由命令层负责）。
    #[tokio::test]
    async fn locked_path_save_failure_does_not_swap() {
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store);

        let _guard = runtime.lock_for_update().await;
        let mut candidate = runtime.snapshot().unwrap();
        candidate.device_name = "x".into();
        let err = runtime
            .store_handle()
            .save_atomic(&candidate)
            .expect_err("save 应失败");
        assert!(err.to_string().contains("注入故障"));
        // 故意不 swap
        drop(_guard);
        assert_eq!(runtime.snapshot().unwrap().device_name, "runtime-device");
    }
}
