//! commands/sync.rs — 同步触发命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Prompt 管理页面"同步"按钮经 `invoke('trigger_sync')` 触发全网 Prompt 同步。
//!     对照 Python `/api/sync` handler（调用 SyncEngine.sync_all）。
//!
//! Code Logic（这个模块做什么）:
//!     `trigger_sync`：调 `sync::engine::trigger_sync`，返回 SyncResult（含 synced 计数）。
//!     前端 `promptsApi.sync()` 期望返回 `{ synced: number }`，SyncResult serde 直接满足。

use crate::error::AppError;
use crate::state::AppState;
use crate::sync::engine;
use tauri::State;

/// 触发全网 Prompt 同步。
///
/// Business Logic: 用户点击"同步"按钮时调用，与所有在线对端双向同步 Prompt。
/// Code Logic: 转发到 `sync::engine::trigger_sync`；返回 SyncResult（accepted/synced/note）。
#[tauri::command]
pub async fn trigger_sync(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let result = engine::trigger_sync(state.inner()).await;
    // 序列化为 {accepted, synced, note}（serde_json::Value 透传，前端取 synced）
    let value = serde_json::to_value(&result)?;
    Ok(value)
}
