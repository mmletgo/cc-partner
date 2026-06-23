//! commands/cloud_sync.rs — 云端同步（GitHub 私有仓库）命令层
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设置页"云端同步"卡片需要：读取/修改云端同步配置、手动触发同步、测试连通。
//!     这是本地前端↔Rust 的 IPC 边界，参数 snake_case（Tauri 自动映射前端 camelCase），
//!     返回 DTO camelCase 对齐前端 types.ts。
//!
//! Code Logic（这个模块做什么）:
//!     - `get_cloud_sync_config`：读 config 转 CloudSyncConfigDto。
//!     - `update_cloud_sync_config`：写锁应用 patch → save() → 返回最新 DTO。
//!       scheduler 无需重启（setup 无条件启动，内部每 tick 按 config 决定）。
//!     - `trigger_cloud_sync_cmd`：调 engine::trigger_cloud_sync。
//!     - `test_cloud_sync`：调 engine::test_connection。

use crate::cloud_sync::engine::{self, CloudSyncResult, TestCloudSyncResult};
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
/// Business Logic: 前端设置页初始化时展示当前云端同步配置。
#[tauri::command]
pub async fn get_cloud_sync_config(
    state: State<'_, AppState>,
) -> Result<CloudSyncConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    Ok(to_dto(&cfg))
}

/// 更新云端同步配置（所有字段可选 patch），并持久化。
///
/// Business Logic: 用户在设置页保存配置后需落盘，scheduler 下个 tick 自动生效。
/// Code Logic: 取写锁应用 patch → save() → 返回最新 DTO。
#[tauri::command]
pub async fn update_cloud_sync_config(
    state: State<'_, AppState>,
    repo_url: Option<String>,
    enabled: Option<bool>,
    auto: Option<bool>,
    interval_secs: Option<u64>,
    branch: Option<String>,
) -> Result<CloudSyncConfigDto, AppError> {
    {
        let mut cfg = state.config.write().unwrap();
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
        cfg.save()?;
    }
    let cfg = state.config.read().unwrap();
    Ok(to_dto(&cfg))
}

/// 手动触发一次云端同步。
///
/// Business Logic: 前端"立即同步"按钮调用，不受 enabled/auto 开关限制（用户主动触发）。
#[tauri::command]
pub async fn trigger_cloud_sync_cmd(
    state: State<'_, AppState>,
) -> Result<CloudSyncResult, AppError> {
    Ok(engine::trigger_cloud_sync(state.inner()).await)
}

/// 测试云端同步连通性。
///
/// Business Logic: 前端"测试连接"按钮调用，验证 git 可用 + 远端可达 + 返回默认分支。
#[tauri::command]
pub async fn test_cloud_sync(state: State<'_, AppState>) -> Result<TestCloudSyncResult, AppError> {
    Ok(engine::test_connection(state.inner()).await)
}
