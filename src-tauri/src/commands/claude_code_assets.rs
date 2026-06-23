//! commands/claude_code_assets.rs — Claude Code 资产管理 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端「Claude Code」页面需要通过 Tauri IPC 管理本机 assets，并从局域网设备选择性拉取 assets。
//!
//! Code Logic（这个模块做什么）:
//!     作为 thin command layer，把参数转换为 claude_code_assets 模块的领域函数调用，并返回 camelCase DTO。

use crate::claude_code_assets::{
    self, ClaudeCodeAsset, ClaudeCodeAssetInstallReport, ClaudeCodeAssetKind,
    ClaudeCodeAssetSelector, ClaudeCodeInstallSource,
};
use crate::error::AppError;
use crate::state::AppState;
use tauri::State;

/// 列出本机 Claude Code assets。
#[tauri::command]
pub async fn list_claude_code_assets() -> Result<Vec<ClaudeCodeAsset>, AppError> {
    claude_code_assets::list_assets().await
}

/// 启用或禁用一个 Claude Code asset。
#[tauri::command]
pub async fn set_claude_code_asset_enabled(
    kind: ClaudeCodeAssetKind,
    id: String,
    enabled: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    claude_code_assets::set_asset_enabled(kind, id, enabled).await
}

/// 从本地路径或 JSON 安装 Claude Code asset。
#[tauri::command]
pub async fn install_claude_code_asset(
    source: ClaudeCodeInstallSource,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    claude_code_assets::install_asset(source).await
}

/// 卸载本机 Claude Code asset。
#[tauri::command]
pub async fn uninstall_claude_code_asset(
    kind: ClaudeCodeAssetKind,
    id: String,
    keep_data: Option<bool>,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    claude_code_assets::uninstall_asset(kind, id, keep_data.unwrap_or(false)).await
}

/// 列出指定局域网设备可供拉取的 Claude Code assets。
#[tauri::command]
pub async fn list_remote_claude_code_assets(
    state: State<'_, AppState>,
    device_id: String,
) -> Result<Vec<ClaudeCodeAsset>, AppError> {
    claude_code_assets::list_remote_assets(state.inner(), device_id).await
}

/// 从指定局域网设备拉取用户选择的 Claude Code assets。
#[tauri::command]
pub async fn pull_claude_code_assets(
    state: State<'_, AppState>,
    device_id: String,
    items: Vec<ClaudeCodeAssetSelector>,
    overwrite: bool,
) -> Result<ClaudeCodeAssetInstallReport, AppError> {
    claude_code_assets::pull_remote_assets(state.inner(), device_id, items, overwrite).await
}
