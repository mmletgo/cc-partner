//! provider_manager — cc-switch 联动：读 cc-switch 已配置 provider + 委托 CLI 切换。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户希望 cc-partner 能列出每个 agent（Claude Code / Codex / Gemini …）在 cc-switch
//!     中已配置好的 provider 并切换当前 provider，但不要编辑 provider 详情。切换会改写
//!     `~/.claude/settings.json` 等活配置，逻辑复杂且高风险，因此"读"直接查 cc-switch 的
//!     SQLite（只读），"写"委托给 cc-switch CLI（与 GUI 同源服务层）。
//!
//! Code Logic（这个模块做什么）:
//!     - `summary()`：DB 是否存在 + CLI 检测/版本 + GUI 检测 + 各 app provider 列表。
//!     - `list_apps()`：组装受支持 agent 的 provider 列表。
//!     - `switch(app, id)`：校验 → CLI 切换 → 重读该 app 返回更新态。
//!     - `install_cli()`：安装 cc-switch CLI（macOS brew / 其余人工指引）。
//!
//!     无 AppState 字段、无 axum 路由、不写 `~/.cc-switch` 与各 agent 活配置文件。
//!     切换是本机 GUI 进程的文件/CLI IO（与 `claude_code_assets` 同一先例）。

mod cc_switch_cli;
pub mod models;
mod store;

pub use models::{
    AgentApp, AppProviders, CcSwitchGuiStatus, CliStatus, InstallResult, ProviderEntry,
    ProviderManagerSummary,
};

use crate::error::AppError;

/// 整体状态快照（供 `provider_manager_status` 命令）。
pub(crate) async fn summary() -> ProviderManagerSummary {
    let db_present = store::db_present();
    let cli_path = cc_switch_cli::detect().await;
    let cli_version = match &cli_path {
        Some(p) => cc_switch_cli::version(p).await,
        None => None,
    };
    let cli = CliStatus {
        available: cli_path.is_some(),
        path: cli_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
        version: cli_version.clone(),
    };
    let gui = cc_switch_cli::detect_gui(cli_version.as_deref());
    let apps = match store::list_apps().await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!("provider_manager list_apps 失败: {e}");
            Vec::new()
        }
    };
    ProviderManagerSummary {
        cc_switch_db_present: db_present,
        cli,
        gui,
        apps,
    }
}

/// 各受支持 agent 的 provider 列表（供 `provider_manager_list` 命令）。
pub(crate) async fn list_apps() -> Result<Vec<AppProviders>, AppError> {
    store::list_apps().await
}

/// 切换某 agent 的当前 provider（供 `provider_manager_switch` 命令）。
///
/// Business Logic: 校验非空 provider id → 要求 CLI 存在 → 委托 CLI 切换 → 重读该 app。
///     CLI 不存在时返回 `unavailable`（503），前端据此引导去依赖页安装。
pub(crate) async fn switch(app: AgentApp, provider_id: &str) -> Result<AppProviders, AppError> {
    let trimmed = provider_id.trim();
    if trimmed.is_empty() {
        return Err(AppError::validation("provider id 不能为空"));
    }
    let path = cc_switch_cli::detect().await.ok_or_else(|| {
        AppError::unavailable("未找到 cc-switch CLI，请在「设置 → 依赖环境」安装后再切换")
    })?;
    cc_switch_cli::run_switch(&path, app, trimmed).await?;
    store::refresh_app(app).await
}

/// 安装 cc-switch CLI（供 `provider_manager_install_cli` 命令；显式用户动作）。
pub(crate) async fn install_cli() -> Result<InstallResult, AppError> {
    cc_switch_cli::invalidate();
    cc_switch_cli::install().await
}
