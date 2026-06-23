//! commands/scratchpad.rs — 速记本 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Scratchpad 页面需要读取单例文本、自动保存文本，并手动触发局域网同步。
//!     内容权威源从 localStorage 迁移到 Rust/SQLite 后，所有页面操作都必须走这些命令。
//!
//! Code Logic（这个模块做什么）:
//!     `get_scratchpad` 调 repo.get_or_init；`update_scratchpad` 调 repo.update_content；
//!     `sync_scratchpad` 复用全局 trigger_sync，使 scratchpad 随 prompts/cc/ssh 一起同步。

use crate::error::AppError;
use crate::models::scratchpad::ScratchpadDto;
use crate::state::AppState;
use crate::sync::engine;
use tauri::State;

/// 获取速记本单例；首次调用自动初始化空内容。
///
/// Business Logic: 页面加载时应从 SQLite 恢复内容，不再读取 localStorage。
/// Code Logic: 使用本机 device_id 调 get_or_init，返回 camelCase DTO。
#[tauri::command]
pub async fn get_scratchpad(state: State<'_, AppState>) -> Result<ScratchpadDto, AppError> {
    let row = state
        .scratchpad_repo
        .get_or_init(state.device_id.as_str())
        .await?;
    Ok(row.to_dto())
}

/// 更新速记本文本；用于自动保存和清空。
///
/// Business Logic: 用户编辑应自动持久化到 SQLite，并推进 vector_clock 供局域网/GitHub 同步感知。
/// Code Logic: repo.update_content 负责保留 created_at、更新 updated_at、递增当前设备时钟。
#[tauri::command]
pub async fn update_scratchpad(
    state: State<'_, AppState>,
    content: String,
) -> Result<ScratchpadDto, AppError> {
    let row = state
        .scratchpad_repo
        .update_content(&content, state.device_id.as_str())
        .await?;
    Ok(row.to_dto())
}

/// 手动触发速记本局域网同步。
///
/// Business Logic: Scratchpad 页面新增“局域网同步”按钮；全局 trigger_sync 已纳入 scratchpad，
///     因此这里复用同一同步入口，避免维护两套设备遍历逻辑。
/// Code Logic: 调 sync::engine::trigger_sync 并序列化为前端已有的 `{accepted,synced,note}` 结构。
#[tauri::command]
pub async fn sync_scratchpad(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let result = engine::trigger_sync(state.inner()).await;
    Ok(serde_json::to_value(&result)?)
}
