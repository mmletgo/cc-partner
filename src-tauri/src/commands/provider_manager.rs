//! commands/provider_manager.rs — Provider Manager invoke 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端「Provider Manager」页与「设置 → 依赖环境」的 cc-switch 依赖卡片通过 invoke 调用
//!     这些命令：查整体状态、列各 agent 的 provider、切换当前 provider、安装 cc-switch CLI。
//!
//! Code Logic（这个模块做什么）:
//!     全部为 stateless 薄封装（无 AppState）：编排留在 `provider_manager` 领域模块，命令只做
//!     IPC 边界映射。参数 camelCase，返回 `Result<T, AppError>`。

use crate::error::AppError;
use crate::provider_manager::{
    self, AgentApp, AppProviders, InstallResult, ProviderManagerSummary,
};

/// Provider Manager 整体状态：DB 是否存在 + CLI 检测/版本 + GUI 检测 + 各 app provider 列表。
///
/// 只读；绝不启动或修改 GUI，也不写任何 agent 活配置文件。
#[tauri::command]
pub async fn provider_manager_status() -> Result<ProviderManagerSummary, AppError> {
    Ok(provider_manager::summary().await)
}

/// 各受支持 agent 的 provider 列表（隐藏 0 provider 的 app，排除 `claude-desktop`）。
#[tauri::command]
pub async fn provider_manager_list() -> Result<Vec<AppProviders>, AppError> {
    provider_manager::list_apps().await
}

/// 切换某 agent 的当前 provider（委托 cc-switch CLI 执行真实写盘）。
#[tauri::command]
pub async fn provider_manager_switch(
    app: AgentApp,
    provider_id: String,
) -> Result<AppProviders, AppError> {
    provider_manager::switch(app, &provider_id).await
}

/// 安装 cc-switch CLI（显式用户动作；macOS 走 brew，其余返回人工指引）。
#[tauri::command]
pub async fn provider_manager_install_cli() -> Result<InstallResult, AppError> {
    provider_manager::install_cli().await
}
