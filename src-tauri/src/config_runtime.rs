//! config_runtime.rs — 配置内存态的串行事务更新
//!
//! Business Logic（为什么需要这个模块）:
//!     多个命令可能并发修改不同配置字段；若各自 clone→改→写盘→swap，会产生 lost update。
//!     需要单一 writer gate：clone → mutate → validate → durable save → memory swap。
//!
//! Code Logic（这个模块做什么）:
//!     `ConfigRuntime` 持有 `Arc<RwLock<AppConfig>>`、异步 `update_lock` 与 `ConfigStore`；
//!     `update_config_transactionally` 串行化写路径，且不把 std `RwLockGuard` 跨 await。

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
}

/// 串行事务更新配置：clone → mutate → validate → save_atomic → swap。
///
/// Business Logic（为什么需要这个函数）:
///     所有配置 writer 必须经此 helper，保证失败时内存与旧文件不变，成功时字段合并不丢。
///
/// Code Logic（这个函数做什么）:
///     1) 持有异步 `update_lock` 全程；2) 读锁 clone candidate 后立即释放；
///     3) mutate；4) `validate`；5) `store.save_atomic`；6) 写锁 swap 内存；
///     返回提交后的配置与 mutate 结果。错误路径不做 memory swap。
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
    runtime.store.save_atomic(&candidate)?;

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
    use crate::config_store::MemoryConfigStore;
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
}
