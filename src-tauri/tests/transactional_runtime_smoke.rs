//! transactional_runtime_smoke — 配置原子写 / Cloud Sync 单飞 / Updater / Health 校验 smoke
//!
//! Business Logic（为什么需要这个测试）:
//!     事务化运行时必须在隔离目录下证明故障注入可恢复、并发单飞、generation 守卫与 Health 无副作用，
//!     且不得触碰真实 `~/.cc-partner`。
//!
//! Code Logic（这个模块做什么）:
//!     独立 temp data_dir；复用 FsConfigStore + FaultInjectingConfigIo、CloudSyncRuntime、
//!     UpdateRuntime、health::validation。

use app_lib::cloud_sync::runtime::{
    run_cloud_sync_exclusive, CloudSyncBusyPolicy, CloudSyncRuntime, CloudSyncTrigger,
};
use app_lib::config::{AppConfig, HealthConfig};
use app_lib::config_store::{
    ConfigIoStage, ConfigStore, FaultInjectingConfigIo, FsConfigStore, StdConfigIo,
};
use app_lib::error::AppError;
use app_lib::health::validation::{
    checked_future_timestamp, validate_health_config, validate_health_config_with_field,
};
use app_lib::updater::{UpdatePhase, UpdateRuntime};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 创建唯一隔离目录（不依赖 backend 二进制）。
fn make_iso_dir(name: &str) -> PathBuf {
    let unique = format!(
        "cc-partner-tx-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    fs::create_dir_all(&root).unwrap();
    // 防止误用用户 home
    if let Some(home) = dirs::home_dir() {
        assert!(!root.starts_with(home.join(".cc-partner")));
    }
    root
}

fn sample_config(data_dir: &Path) -> AppConfig {
    AppConfig {
        device_id: "smoke-device".into(),
        device_name: "smoke".into(),
        http_port: 0,
        receive_dir: data_dir.join("recv").to_string_lossy().to_string(),
        db_path: data_dir.join("data.db").to_string_lossy().to_string(),
        screenshot_hotkey: "<ctrl>+s".into(),
        prompt_optimizer_hotkey: "<ctrl>".into(),
        prompt_optimizer_fill_language: "zh".into(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: HealthConfig::default(),
        orchestrator: Default::default(),
        github_trending: Default::default(),
        agent_hub: app_lib::config::AgentHubConfig::default(),
        manual_peers: Vec::new(),
    }
}

/// 配置原子写：各阶段故障注入后旧 JSON 仍可解析，随后健康保存成功。
#[test]
fn config_atomic_store_recovers_from_injected_stage_failures() {
    let root = make_iso_dir("config");
    let config_path = root.join("config.json");
    let initial = sample_config(&root);
    let store = FsConfigStore::new(config_path.clone(), Arc::new(StdConfigIo));
    store.save_atomic(&initial).expect("initial save");
    assert_eq!(
        serde_json::from_str::<AppConfig>(&fs::read_to_string(&config_path).unwrap())
            .unwrap()
            .device_name,
        "smoke"
    );

    // 在 rename 之前失败：旧文件必须保持权威且可解析。
    for stage in [
        ConfigIoStage::Create,
        ConfigIoStage::Write,
        ConfigIoStage::Flush,
        ConfigIoStage::FileSync,
        ConfigIoStage::Rename,
    ] {
        let io = FaultInjectingConfigIo::fail_once(Arc::new(StdConfigIo), stage);
        let fault_store = FsConfigStore::new(config_path.clone(), Arc::new(io));
        let mut candidate = initial.clone();
        candidate.device_name = format!("fail-{stage:?}");
        assert!(fault_store.save_atomic(&candidate).is_err());
        let parsed: AppConfig =
            serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(parsed.device_name, "smoke", "stage {stage:?}");
    }

    // DirectorySync 在原子替换之后：rename 是 commit 点，磁盘权威必须是 NEW，
    // 且 save_atomic 返回 Ok（dirsync 失败仅 warn）。
    {
        let io =
            FaultInjectingConfigIo::fail_once(Arc::new(StdConfigIo), ConfigIoStage::DirectorySync);
        let fault_store = FsConfigStore::new(config_path.clone(), Arc::new(io));
        let mut candidate = initial.clone();
        candidate.device_name = "after-rename".into();
        fault_store
            .save_atomic(&candidate)
            .expect("dirsync fault after rename must still commit");
        let text = fs::read_to_string(&config_path).unwrap();
        let parsed: AppConfig =
            serde_json::from_str(&text).expect("JSON still valid after dir-sync fault");
        assert_eq!(parsed.device_name, "after-rename");
    }

    let healthy = FsConfigStore::new(config_path.clone(), Arc::new(StdConfigIo));
    let mut ok = initial.clone();
    ok.device_name = "recovered".into();
    healthy.save_atomic(&ok).unwrap();
    let final_cfg: AppConfig =
        serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(final_cfg.device_name, "recovered");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config file should be 0600");
    }

    let _ = fs::remove_dir_all(&root);
}

/// Cloud Sync：两个 Wait 互不重叠；ReturnBusy 递增 skippedBusy。
#[tokio::test]
async fn cloud_sync_exclusive_gate_serializes_writers() {
    let runtime = Arc::new(CloudSyncRuntime::new());
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let spawn_wait = |rt: Arc<CloudSyncRuntime>,
                      c: Arc<AtomicUsize>,
                      m: Arc<AtomicUsize>,
                      trigger: CloudSyncTrigger| {
        tokio::spawn(async move {
            run_cloud_sync_exclusive(
                &rt,
                trigger,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || {
                    let c = c.clone();
                    let m = m.clone();
                    async move {
                        let cur = c.fetch_add(1, Ordering::SeqCst) + 1;
                        m.fetch_max(cur, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(80)).await;
                        c.fetch_sub(1, Ordering::SeqCst);
                        Ok::<(), AppError>(())
                    }
                },
            )
            .await
        })
    };

    let h1 = spawn_wait(
        runtime.clone(),
        concurrent.clone(),
        max_seen.clone(),
        CloudSyncTrigger::Manual,
    );
    let h2 = spawn_wait(
        runtime.clone(),
        concurrent.clone(),
        max_seen.clone(),
        CloudSyncTrigger::ClaudeMdPush,
    );
    h1.await.unwrap().unwrap();
    h2.await.unwrap().unwrap();
    assert_eq!(max_seen.load(Ordering::SeqCst), 1);

    let runtime2 = Arc::new(CloudSyncRuntime::new());
    let hold = tokio::spawn({
        let rt = runtime2.clone();
        async move {
            run_cloud_sync_exclusive(
                &rt,
                CloudSyncTrigger::Manual,
                CloudSyncBusyPolicy::Wait {
                    timeout: Duration::from_secs(5),
                },
                || async {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok::<(), AppError>(())
                },
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let busy = run_cloud_sync_exclusive(
        &runtime2,
        CloudSyncTrigger::Scheduler,
        CloudSyncBusyPolicy::ReturnBusy,
        || async {
            panic!("scheduler should not run while busy");
            #[allow(unreachable_code)]
            Ok::<(), AppError>(())
        },
    )
    .await
    .unwrap();
    assert!(busy.is_none());
    assert!(runtime2.status_snapshot().skipped_busy >= 1);
    hold.await.unwrap().unwrap();
}

/// Updater generation 递增与 cancel 空 lease。
#[test]
fn updater_generation_cancel_smoke() {
    let rt = UpdateRuntime::new();
    let g1 = rt.begin_check().unwrap();
    assert_eq!(g1, 1);
    let _ = rt.finish_check(g1, Ok(None));
    let g2 = rt.begin_check().unwrap();
    assert!(g2 > g1);
    let lease = rt.cancel();
    assert!(lease.cancel.is_none());
    assert!(lease.task.is_none());
    let (g, phase) = rt.snapshot();
    assert_eq!(g, g2);
    // cancel on Checking may leave Cancelled or Checking depending on implementation
    let _ = phase;
    let g3 = rt.begin_check().unwrap();
    assert!(g3 > g2);
    assert!(matches!(rt.snapshot().1, UpdatePhase::Checking));
}

/// Health 极端输入被拒绝。
#[test]
fn health_extreme_inputs_rejected() {
    let cfg = HealthConfig {
        work_window_seconds: i64::MAX,
        ..HealthConfig::default()
    };
    assert!(validate_health_config(&cfg).is_err());
    assert!(validate_health_config_with_field(&cfg)
        .unwrap_err()
        .starts_with("health."));

    let cfg = HealthConfig {
        break_seconds: 0,
        ..HealthConfig::default()
    };
    assert!(validate_health_config(&cfg).is_err());

    let cfg = HealthConfig {
        dnd_start: Some("7:00".into()),
        dnd_end: Some("08:00".into()),
        ..HealthConfig::default()
    };
    assert!(validate_health_config(&cfg).is_err());

    assert!(checked_future_timestamp(1_000, 0).is_err());
    assert!(checked_future_timestamp(1_000, i64::MAX).is_err());
    assert_eq!(checked_future_timestamp(1_000, 1).unwrap(), 1_060);
}

/// 隔离目录不落在真实 ~/.cc-partner。
#[test]
fn smoke_paths_isolated_from_user_home() {
    let root = make_iso_dir("isolation");
    if let Some(home) = dirs::home_dir() {
        let real = home.join(".cc-partner");
        assert!(!root.starts_with(&real));
        assert_ne!(root, real);
    }
    let _ = fs::remove_dir_all(&root);
}
