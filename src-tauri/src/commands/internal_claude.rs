//! commands/internal_claude.rs — cc-partner 内部 Claude provider 覆盖的设置页命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户希望在设置页选择一个 cc-switch claude provider，专供 cc-partner 内部 headless
//!     Claude 调用（commit/merge/prompt 优化/GitHub 解说/verifier）使用，且不改写 OS 默认
//!     `~/.claude/settings.json`。本模块只持久化所选 provider **id**（不含凭据），运行时由
//!     `internal_claude::resolve_internal_provider_config_dir` 实时从 cc-switch DB 取 settings_config
//!     写入隔离 `CLAUDE_CONFIG_DIR`。
//!
//! Code Logic（这个模块做什么）:
//!     - `get_internal_claude_config`：读当前配置生成 DTO。
//!     - `get_default_internal_claude_config`：返回默认（providerId=null）供「恢复默认」。
//!     - `update_internal_claude_config`：经 `BackendControlClient.apply_patch` 提交
//!       `InternalClaudeRuntimePatch`（空串归一 None），刷新本地缓存，返回提交后 DTO。
//!     与 `update_github_trending_config` 同一 CAS/owner 写路径（GuiClient → sidecar）。

use crate::backend::control_client::BackendControlClient;
use crate::config::InternalClaudeConfig;
use crate::config_runtime::{InternalClaudeRuntimePatch, RuntimeConfigPatch};
use crate::error::AppError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

/// 内部 Claude provider 覆盖配置 DTO（camelCase，对齐前端类型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalClaudeConfigDto {
    /// 选中的 cc-switch claude provider id；null = 沿用 OS 默认 provider。
    pub provider_id: Option<String>,
}

/// 将配置结构转成前端 DTO。
fn config_to_dto(config: &InternalClaudeConfig) -> InternalClaudeConfigDto {
    InternalClaudeConfigDto {
        provider_id: config.provider_id.clone(),
    }
}

/// 读取内部 Claude provider 覆盖配置。
///
/// Business Logic: 设置页 AI tab 初始化时展示当前所选内部 provider。
#[tauri::command]
pub async fn get_internal_claude_config(
    state: State<'_, AppState>,
) -> Result<InternalClaudeConfigDto, AppError> {
    let cfg = state.config.read().unwrap();
    Ok(config_to_dto(&cfg.internal_claude))
}

/// 读取内部 Claude provider 覆盖默认配置。
///
/// Business Logic: 设置页 AI tab 需要一键回到默认（沿用 OS 默认 provider）。
#[tauri::command]
pub async fn get_default_internal_claude_config() -> Result<InternalClaudeConfigDto, AppError> {
    Ok(config_to_dto(&InternalClaudeConfig::default()))
}

/// 更新内部 Claude provider 覆盖配置。
///
/// Business Logic: 用户在设置页应用所选内部 provider 后需落到 sidecar 权威配置；
///     下次内部 Claude 调用实时生效。
///
/// Code Logic: 前端**始终**传 `Some(String)`：`Some(id)` 设置，`Some("")` 清空（= 沿用 OS 默认）。
///     `InternalClaudeRuntimePatch::apply_to` 把空串归一为 None。经 `BackendControlClient.apply_patch`
///     提交（owner/generation CAS），刷新本地缓存，返回提交后 DTO。
#[tauri::command]
pub async fn update_internal_claude_config(
    state: State<'_, AppState>,
    provider_id: Option<String>,
) -> Result<InternalClaudeConfigDto, AppError> {
    let patch_field = provider_id.map(|p| p.trim().to_string());
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            internal_claude: Some(InternalClaudeRuntimePatch {
                provider_id: patch_field,
            }),
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(config_to_dto(&resp.snapshot.internal_claude))
}
