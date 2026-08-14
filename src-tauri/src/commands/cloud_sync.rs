//! commands/cloud_sync.rs — 云端同步（GitHub 私有仓库）命令层
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设置页"云端同步"卡片需要：读取/修改云端同步配置、手动触发同步、测试连通。
//!     这是本地前端↔Rust 的 IPC 边界，参数 snake_case（Tauri 自动映射前端 camelCase），
//!     返回 DTO camelCase 对齐前端 types.ts。
//!
//! Code Logic（这个模块做什么）:
//!     - `get_cloud_sync_config`：优先读 sidecar 权威快照。
//!     - `get_default_cloud_sync_config`：返回后端云同步默认值，供设置页恢复默认。
//!     - `update_cloud_sync_config`：GUI 经 BackendControlClient 提交 cloud_sync allowlist patch。
//!     - `trigger_cloud_sync_cmd` / `test_cloud_sync`：GuiClient 经 BackendControlClient 代理到
//!       sidecar owner control 路由；HeadlessOwner 直接调用本机 CloudSyncRuntime 引擎。
//!     - `*_for_runtime` helper 保留给 owner 侧单测。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::cloud_sync::engine::{self, CloudSyncResult, TestCloudSyncResult};
use crate::config::default_cloud_sync_values;
use crate::config_runtime::RuntimeConfigPatch;
#[cfg(test)]
use crate::config_runtime::{update_config_transactionally, ConfigRuntime};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

/// 云端同步配置前端 DTO（camelCase，对齐锁定契约）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncConfigDto {
    /// 远端仓库 URL（git@... 或 https://...），null 表示未配置。
    pub repo_url: Option<String>,
    /// 总开关。
    pub enabled: bool,
    /// 是否自动同步。
    pub auto: bool,
    /// 自动同步间隔（秒）。
    pub interval_secs: u64,
    /// 指定分支，null 表示用远端默认分支。
    pub branch: Option<String>,
}

/// 从 AppConfig 构造 CloudSyncConfigDto。
fn to_dto(cfg: &crate::config::AppConfig) -> CloudSyncConfigDto {
    CloudSyncConfigDto {
        repo_url: cfg.cloud_sync_repo_url.clone(),
        enabled: cfg.cloud_sync_enabled,
        auto: cfg.cloud_sync_auto,
        interval_secs: cfg.cloud_sync_interval_secs,
        branch: cfg.cloud_sync_branch.clone(),
    }
}

/// 读取云端同步配置。
///
/// Business Logic: 前端设置页初始化时展示当前云端同步配置（优先 sidecar 权威）。
#[tauri::command]
pub async fn get_cloud_sync_config(
    state: State<'_, AppState>,
) -> Result<CloudSyncConfigDto, AppError> {
    if let Ok(client) = BackendControlClient::from_control_file() {
        if let Ok(snap) = client.get_config().await {
            if let Ok(mut cfg) = state.config.write() {
                snap.apply_to_local_config(&mut cfg);
            }
            return Ok(CloudSyncConfigDto {
                repo_url: snap.cloud_sync_repo_url,
                enabled: snap.cloud_sync_enabled,
                auto: snap.cloud_sync_auto,
                interval_secs: snap.cloud_sync_interval_secs,
                branch: snap.cloud_sync_branch,
            });
        }
    }
    let cfg = state.config.read().unwrap();
    Ok(to_dto(&cfg))
}

/// 读取云端同步默认配置。
///
/// Business Logic: 设置页同步 tab 需要一键回到应用默认配置，默认值由 Rust 配置层统一定义。
/// Code Logic: 不读取或写入当前配置，直接把 default_cloud_sync_values 转为前端 DTO。
#[tauri::command]
pub async fn get_default_cloud_sync_config() -> Result<CloudSyncConfigDto, AppError> {
    let (repo_url, enabled, auto, interval_secs, branch) = default_cloud_sync_values();
    Ok(CloudSyncConfigDto {
        repo_url,
        enabled,
        auto,
        interval_secs,
        branch,
    })
}

#[cfg(test)]
/// 在 ConfigRuntime 上应用云同步 patch。
///
/// Business Logic（为什么需要这个函数）:
///     设置页云同步保存必须事务化，失败不改内存；抽出 helper 供命令与并发/回滚单测复用。
///
/// Code Logic（这个函数做什么）:
///     经 `update_config_transactionally` 改 candidate 字段并返回提交后的 DTO。
pub async fn update_cloud_sync_config_for_runtime(
    runtime: &ConfigRuntime,
    repo_url: Option<String>,
    enabled: Option<bool>,
    auto: Option<bool>,
    interval_secs: Option<u64>,
    branch: Option<String>,
) -> Result<CloudSyncConfigDto, AppError> {
    let (_committed, dto) = update_config_transactionally(runtime, |cfg| {
        if let Some(u) = repo_url {
            // 空串视为未配置（统一为 None）
            cfg.cloud_sync_repo_url = if u.trim().is_empty() { None } else { Some(u) };
        }
        if let Some(e) = enabled {
            cfg.cloud_sync_enabled = e;
        }
        if let Some(a) = auto {
            cfg.cloud_sync_auto = a;
        }
        if let Some(i) = interval_secs {
            // 间隔最小 30 秒，避免过于频繁
            cfg.cloud_sync_interval_secs = i.max(30);
        }
        if let Some(b) = branch {
            cfg.cloud_sync_branch = if b.trim().is_empty() { None } else { Some(b) };
        }
        Ok(to_dto(cfg))
    })
    .await?;
    Ok(dto)
}

/// 更新云端同步配置（所有字段可选 patch），并持久化。
///
/// Business Logic: 用户在设置页保存配置后需落到 sidecar 权威配置，scheduler 下个 tick 自动生效。
/// Code Logic: BackendControlClient 提交 RuntimeConfigPatch 的 cloud_sync 字段；刷新本地缓存。
#[tauri::command]
pub async fn update_cloud_sync_config(
    state: State<'_, AppState>,
    repo_url: Option<String>,
    enabled: Option<bool>,
    auto: Option<bool>,
    interval_secs: Option<u64>,
    branch: Option<String>,
) -> Result<CloudSyncConfigDto, AppError> {
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            cloud_sync_repo_url: repo_url,
            cloud_sync_enabled: enabled,
            cloud_sync_auto: auto,
            cloud_sync_interval_secs: interval_secs,
            cloud_sync_branch: branch,
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(CloudSyncConfigDto {
        repo_url: resp.snapshot.cloud_sync_repo_url,
        enabled: resp.snapshot.cloud_sync_enabled,
        auto: resp.snapshot.cloud_sync_auto,
        interval_secs: resp.snapshot.cloud_sync_interval_secs,
        branch: resp.snapshot.cloud_sync_branch,
    })
}

/// 手动触发一次云端同步。
///
/// Business Logic: 前端"立即同步"按钮调用，不受 enabled/auto 开关限制（用户主动触发）。
///     必须与 sidecar scheduler 共享唯一 owner CloudSyncRuntime 门闸，禁止 GuiClient 本地写 git。
/// Code Logic: GuiClient → control `cloud-sync/trigger`；HeadlessOwner → engine::trigger_cloud_sync。
#[tauri::command]
pub async fn trigger_cloud_sync_cmd(
    state: State<'_, AppState>,
) -> Result<CloudSyncResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.cloud_sync_trigger().await;
    }
    Ok(engine::trigger_cloud_sync(state.inner()).await)
}

/// 测试云端同步连通性。
///
/// Business Logic: 前端"测试连接"按钮调用，验证 git 可用 + 远端可达 + 返回默认分支。
///     探测可能触达正式 workdir，GuiClient 必须代理到 owner。
/// Code Logic: GuiClient → control `cloud-sync/test`；HeadlessOwner → engine::test_connection。
#[tauri::command]
pub async fn test_cloud_sync(state: State<'_, AppState>) -> Result<TestCloudSyncResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.cloud_sync_test().await;
    }
    Ok(engine::test_connection(state.inner()).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_store::MemoryConfigStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    fn sample_config() -> AppConfig {
        AppConfig {
            device_id: "dev-cs-1".into(),
            device_name: "cs-device".into(),
            http_port: 0,
            receive_dir: "/tmp/recv".into(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: "/tmp/db.db".into(),
            screenshot_hotkey: "<ctrl>+s".into(),
            prompt_optimizer_hotkey: "<ctrl>".into(),
            prompt_optimizer_fill_language: "zh".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".into(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
        }
    }

    /// 验证并发非冲突字段更新不会丢失。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     用户可能同时改云同步与其它配置路径；事务串行后两边字段都必须保留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Barrier 同步后并发改 enabled 与 branch，断言最终两者都生效。
    #[tokio::test]
    async fn concurrent_config_updates_do_not_lose_fields() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
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
            update_cloud_sync_config_for_runtime(&r1, None, Some(true), None, None, None).await
        });

        let r2 = Arc::clone(&runtime);
        let b2 = Arc::clone(&barrier);
        let s2 = Arc::clone(&started);
        let t2 = tokio::spawn(async move {
            s2.fetch_add(1, Ordering::SeqCst);
            b2.wait().await;
            update_cloud_sync_config_for_runtime(&r2, None, None, None, None, Some("main".into()))
                .await
        });

        let (a, b) = tokio::join!(t1, t2);
        a.expect("join a").expect("update a");
        b.expect("join b").expect("update b");

        let final_cfg = runtime.snapshot().expect("final");
        assert!(final_cfg.cloud_sync_enabled);
        assert_eq!(final_cfg.cloud_sync_branch.as_deref(), Some("main"));
    }

    /// 验证 save 失败时云同步配置不半提交。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     网络/磁盘故障时设置页保存失败必须保持旧配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     fail_next_save 后改 enabled，断言 Err 且 snapshot 仍 false。
    #[tokio::test]
    async fn save_failure_rolls_back() {
        let _data_dir_guard = crate::config::install_data_dir_env(None);
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        store.fail_next_save();
        let runtime = ConfigRuntime::new(initial, store.clone());

        let err =
            update_cloud_sync_config_for_runtime(&runtime, None, Some(true), None, None, None)
                .await
                .expect_err("should fail");
        assert!(err.to_string().contains("注入故障"));
        assert!(!runtime.snapshot().unwrap().cloud_sync_enabled);
        assert!(!store.snapshot().unwrap().cloud_sync_enabled);
    }
}
