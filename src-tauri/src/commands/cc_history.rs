//! commands/cc_history.rs — Claude Code 历史 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 CC 历史页面通过 invoke 调用这些命令：按项目浏览历史 prompt、查看详情、
//!     手动触发采集刷新、软删除某条。采集本身由后台采集器自动进行，refresh 仅供用户主动触发。
//!
//! Code Logic（这个模块做什么）:
//!     从 State 取 device_id 与 cc_history_repo；调用 repo 方法或 collector::scan_once；
//!     返回 CcProjectDto / ClaudeHistoryDto（camelCase）。delete_cc_prompt 软删除时
//!     推进 vector_clock[device_id] += 1（CRDT 删除是一次写入，需让对端感知）。

use crate::cc::collector;
use crate::cc::models::{CcProjectDto, ClaudeHistoryDto};
use crate::error::AppError;
use crate::state::AppState;
use chrono::Utc;
use tauri::State;

/// 当前时间的 RFC3339 字符串（带 UTC 时区）。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 列出所有有 Claude Code 历史的项目（聚合 count + 最近活动时间），按最近活动降序。
#[tauri::command]
pub async fn list_cc_projects(state: State<'_, AppState>) -> Result<Vec<CcProjectDto>, AppError> {
    state.cc_history_repo.list_projects().await
}

/// 列出某项目下的历史 prompt（可选关键词搜索），按 occurred_at 降序，最多 500 条。
#[tauri::command]
pub async fn list_cc_prompts(
    state: State<'_, AppState>,
    project_path: String,
    search: Option<String>,
) -> Result<Vec<ClaudeHistoryDto>, AppError> {
    let rows = state
        .cc_history_repo
        .list_by_project(&project_path, search.as_deref())
        .await?;
    Ok(rows.iter().map(|r| r.to_dto()).collect())
}

/// 按 id 获取单条 CC 历史；不存在或已删除返回 NotFound。
#[tauri::command]
pub async fn get_cc_prompt(
    state: State<'_, AppState>,
    id: String,
) -> Result<ClaudeHistoryDto, AppError> {
    let row = state.cc_history_repo.get(&id).await?;
    match row {
        Some(r) if !r.deleted => Ok(r.to_dto()),
        _ => Err(AppError::not_found("CC 历史不存在")),
    }
}

/// 手动触发一次 CC 历史采集，返回本次新入库条数。
///
/// Business Logic: 采集器后台每 5 分钟自动扫一次，用户也可在前端点"刷新"立即触发。
/// Code Logic: 调 collector::scan_once，collected = 新入库数。
#[tauri::command]
pub async fn refresh_cc_history(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let collected = collector::scan_once(state.inner()).await?;
    Ok(serde_json::json!({ "ok": true, "collected": collected }))
}

/// 软删除一条 CC 历史。
///
/// Business Logic: CRDT 删除是一次写入，需推进 vector_clock 让对端感知删除事件。
/// Code Logic: 取本地行 → increment 本设备计数器 → soft_delete 写回。
#[tauri::command]
pub async fn delete_cc_prompt(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let mut row = state
        .cc_history_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("CC 历史不存在"))?;
    // 推进 vector_clock（CRDT 删除）
    let counter = row.vector_clock.entry(device_id).or_insert(0);
    *counter += 1;
    let now = now_iso();
    state
        .cc_history_repo
        .soft_delete(&id, &now, &row.vector_clock)
        .await?;
    Ok(serde_json::json!({ "ok": true, "id": id }))
}
