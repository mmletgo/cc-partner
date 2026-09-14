//! commands/provider_manager.rs — Provider Manager invoke 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端「Provider Manager」页与「设置 → 依赖环境」的 cc-switch 依赖卡片通过 invoke 调用
//!     这些命令：查整体状态、列各 agent 的 provider、切换当前 provider、安装 cc-switch CLI。
//!     status/switch/install_cli 支持可选 `deviceId`：选中局域网内其他 cc-partner 设备时
//!     查询/切换对端 cc-switch provider，或直接在对端安装 cc-switch CLI（对端
//!     `/api/provider-manager/*` 路由）；本机路径保持现状。
//!
//! Code Logic（这个模块做什么）:
//!     `list` 仍为 stateless 薄封装（无 AppState）；`status`/`switch`/`install_cli` 持有
//!     AppState 做设备分流：GuiClient（打包发行版）经 loopback control op
//!     `provider-manager.{status,switch,install}` 代理到 sidecar owner，owner 与 headless
//!     命令层共享 `*_for_state` helper（外机 → `device_base_url` +
//!     `RemoteProviderManagerClient`，本机 → `provider_manager` 领域模块原路径）。
//!     参数 camelCase，返回 `Result<T, AppError>`。

use crate::commands::workbench::{device_base_url, proxy_workbench_if_gui};
use crate::error::AppError;
use crate::provider_manager::remote::RemoteProviderManagerClient;
use crate::provider_manager::{
    self, AgentApp, AppProviders, InstallResult, ProviderManagerSummary,
};
use crate::state::AppState;
use tauri::State;

/// Business Logic（为什么需要这个函数）:
///     本机路径必须与历史行为完全一致；只有显式选中局域网内其他设备时才走 P2P。
///
/// Code Logic（这个函数做什么）:
///     deviceId 为空或等于本机 deviceId → false（照抄 workbench banner/dependency 同款分流），
///     委托纯函数 `is_foreign_device_id` 判定。
fn is_foreign_device(state: &AppState, device_id: Option<&str>) -> bool {
    is_foreign_device_id(state.device_id.as_str(), device_id)
}

/// Business Logic（为什么需要这个函数）:
///     分流判定必须是可脱离 AppState 单测的纯函数：空、空白或等于本机 deviceId 都判为本机，
///     仅显式的其他设备 id 才走远端，避免本机路径因可选参数引入行为漂移。
///
/// Code Logic（这个函数做什么）:
///     入参 trim 后为空或与 `local_device_id` 相等 → false；否则 true（本机 id 按 banner.rs
///     模板原样比较，不做 trim）。
fn is_foreign_device_id(local_device_id: &str, device_id: Option<&str>) -> bool {
    let device_id = device_id.unwrap_or("").trim();
    !device_id.is_empty() && device_id != local_device_id
}

/// Provider Manager 整体状态：DB 是否存在 + CLI 检测/版本 + GUI 检测 + 各 app provider 列表。
///
/// 只读；绝不启动或修改 GUI，也不写任何 agent 活配置文件。可选 `deviceId`：外机走对端
/// P2P summary，本机走既有 `provider_manager::summary()`。
#[tauri::command]
pub async fn provider_manager_status(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<ProviderManagerSummary, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "provider-manager.status",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    provider_manager_status_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 分发器与 invoke 命令必须共享同一条设备分流路径，避免 GuiClient 代理与
///     sidecar owner 语义漂移。
///
/// Code Logic（这个函数做什么）:
///     外机 → `device_base_url`（直连 + 影子三段解析）解析对端 HTTP 入口，
///     `RemoteProviderManagerClient`（绑定 expected device_id、预检 provider-manager.v1）
///     GET 对端 summary；本机 → 现有 `provider_manager::summary()` 原样。
pub async fn provider_manager_status_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<ProviderManagerSummary, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        return RemoteProviderManagerClient::new()
            .with_expected_device_id(device_id.trim())
            .summary(&base_url)
            .await;
    }
    Ok(provider_manager::summary().await)
}

/// 各受支持 agent 的 provider 列表（隐藏 0 provider 的 app，排除 `claude-desktop`）。
///
/// 保持纯本机：列表服务的是"目标设备上有什么 provider"以外的展示需求，v1 不做远端。
#[tauri::command]
pub async fn provider_manager_list() -> Result<Vec<AppProviders>, AppError> {
    provider_manager::list_apps().await
}

/// 切换某 agent 的当前 provider（委托 cc-switch CLI 执行真实写盘）。
///
/// 可选 `deviceId`：外机在对端执行切换（写对端活配置），本机走既有
/// `provider_manager::switch()`。
#[tauri::command]
pub async fn provider_manager_switch(
    state: State<'_, AppState>,
    app: AgentApp,
    provider_id: String,
    device_id: Option<String>,
) -> Result<AppProviders, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "provider-manager.switch",
            serde_json::json!({
                "app": app,
                "providerId": provider_id.clone(),
                "deviceId": device_id,
            }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    provider_manager_switch_for_state(state.inner(), app, provider_id, device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 分发器与 invoke 命令必须共享切换路径；且对端 switch 是 CLI 写盘（非幂等），
///     必须由 `RemoteProviderManagerClient` 单次发送，本层不引入重试。
///
/// Code Logic（这个函数做什么）:
///     外机 → `device_base_url` 解析对端入口后 POST 对端 `/api/provider-manager/switch`
///     （camelCase `{app, providerId}`），返回对端重读的 `AppProviders`；
///     本机 → 现有 `provider_manager::switch()` 原样（含 CLI 缺失 → unavailable 语义）。
pub async fn provider_manager_switch_for_state(
    state: &AppState,
    app: AgentApp,
    provider_id: String,
    device_id: Option<String>,
) -> Result<AppProviders, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        return RemoteProviderManagerClient::new()
            .with_expected_device_id(device_id.trim())
            .switch(&base_url, app, &provider_id)
            .await;
    }
    provider_manager::switch(app, &provider_id).await
}

/// 安装 cc-switch CLI（显式用户动作；macOS 走 brew，其余返回人工指引）。
///
/// 可选 `deviceId`：外机在对端执行安装（对端 brew/检测），本机走既有
/// `provider_manager::install_cli()`。
#[tauri::command]
pub async fn provider_manager_install_cli(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<InstallResult, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "provider-manager.install",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    provider_manager_install_cli_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 分发器与 invoke 命令必须共享安装路径；且对端 install 是 brew 写盘
///     （非幂等、可能数分钟），必须由 `RemoteProviderManagerClient` 单次发送（420s 超时），
///     本层不引入重试。
///
/// Code Logic（这个函数做什么）:
///     外机 → `device_base_url` 解析对端入口后 POST 对端
///     `/api/provider-manager/install-cli`（预检 `provider-manager.install.v1` 能力），
///     返回对端 `InstallResult`（macOS brew 成功/失败，其余平台 manual 人工指引）；
///     本机 → 现有 `provider_manager::install_cli()` 原样。
pub async fn provider_manager_install_cli_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<InstallResult, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        return RemoteProviderManagerClient::new()
            .with_expected_device_id(device_id.trim())
            .install_cli(&base_url)
            .await;
    }
    provider_manager::install_cli().await
}

#[cfg(test)]
mod tests {
    use super::is_foreign_device_id;

    /// Business Logic（为什么需要这个测试）:
    ///     本机路径不得因可选 deviceId 引入行为漂移：空、空白或等于本机 deviceId 都必须
    ///     判为本机；仅显式的其他设备 id 才走远端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言纯函数 `is_foreign_device_id` 对 None/空串/空白/本机 id（含带空白）→ false，
    ///     其他设备 id → true。
    #[test]
    fn is_foreign_device_only_matches_explicit_other_device() {
        assert!(!is_foreign_device_id("dev-local", None));
        assert!(!is_foreign_device_id("dev-local", Some("")));
        assert!(!is_foreign_device_id("dev-local", Some("   ")));
        assert!(!is_foreign_device_id("dev-local", Some("dev-local")));
        assert!(!is_foreign_device_id("dev-local", Some(" dev-local ")));
        assert!(is_foreign_device_id("dev-local", Some("dev-remote")));
    }
}
