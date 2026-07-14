//! commands/sync.rs — 同步触发命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Prompt / Settings 同步按钮经 `invoke('trigger_sync')` 触发全网同步，
//!     需要返回 per-device/domain 真值（N2），而不是乐观的 synced 计数。
//!
//! Code Logic（这个模块做什么）:
//!     `trigger_sync`：调 `sync::engine::trigger_sync`，返回 SyncRunResult JSON。

use crate::error::AppError;
use crate::state::AppState;
use crate::sync::engine;
use tauri::State;

/// 触发全网同步，返回 per-device/domain 收敛真值。
///
/// Business Logic: 用户点击同步时调用；UI 用 devices[].status/domains 展示，succeeded_devices 只计全成功。
/// Code Logic: 转发到 `sync::engine::trigger_sync`；序列化为 JSON（含 synced 兼容字段）。
#[tauri::command]
pub async fn trigger_sync(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let result = engine::trigger_sync(state.inner()).await;
    let value = serde_json::to_value(&result)?;
    Ok(value)
}
