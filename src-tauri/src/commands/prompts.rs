//! commands/prompts.rs — Prompt CRUD 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Prompt 管理页面通过 invoke 调用这些命令完成列表/详情/新建/编辑/删除/标签。
//!     行为对照 Python protocol.py 的 handle_list/create/get/update/delete/list_tags handler。
//!
//! Code Logic（这个模块做什么）:
//!     从 State 取 device_id 与 prompt_repo；构造 PromptRow 后调用 repo 方法；
//!     返回 PromptDto（camelCase）。vector_clock 维护：create 初始化 {device_id:1}，
//!     update/delete 推进 vector_clock[device_id] += 1（CRDT 语义）。

use crate::error::AppError;
use crate::models::prompt::{PromptDto, PromptRow};
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashMap;
use tauri::State;
use uuid::Uuid;

/// 当前时间的 RFC3339 字符串（带 UTC 时区，对照 Python datetime.now(timezone.utc).isoformat()）。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 列出 Prompt：可选关键词搜索或单标签筛选。
///
/// Business Logic: 前端列表页传 search 或 tag 查询参数；对应 GET /api/prompts?search=&tag=。
#[tauri::command]
pub async fn list_prompts(
    state: State<'_, AppState>,
    search: Option<String>,
    tag: Option<String>,
) -> Result<Vec<PromptDto>, AppError> {
    let rows = state
        .prompt_repo
        .list(search.as_deref(), tag.as_deref())
        .await?;
    Ok(rows.iter().map(PromptRow::to_dto).collect())
}

/// 按 ID 获取单条 Prompt；不存在或已删除返回 NotFound。
#[tauri::command]
pub async fn get_prompt(
    state: State<'_, AppState>,
    id: String,
) -> Result<PromptDto, AppError> {
    let row = state.prompt_repo.get(&id).await?;
    match row {
        Some(p) if !p.deleted => Ok(p.to_dto()),
        _ => Err(AppError::not_found("Prompt 不存在")),
    }
}

/// 新建 Prompt。对照 Python create handler：生成 uuid、vector_clock 初始 {device_id:1}。
#[tauri::command]
pub async fn create_prompt(
    state: State<'_, AppState>,
    title: String,
    content: String,
    tags: Option<Vec<String>>,
) -> Result<PromptDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let now = now_iso();
    // 标签清洗：去空白、去空串（对照 Python [t.strip() for t in tags if t.strip()]）
    let clean_tags: Vec<String> = tags
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    // vector_clock 初始化：本端计数器置 1
    let mut vc = HashMap::new();
    vc.insert(device_id.clone(), 1u64);
    let row = PromptRow {
        id: Uuid::new_v4().to_string(),
        title: title.trim().to_string(),
        content,
        tags: clean_tags,
        created_at: now.clone(),
        updated_at: now,
        device_id: device_id.clone(),
        vector_clock: vc,
        deleted: false,
    };
    state.prompt_repo.create(&row).await?;
    Ok(row.to_dto())
}

/// 更新 Prompt。对照 Python update handler：应用 title/content/tags patch，推进 vector_clock。
#[tauri::command]
pub async fn update_prompt(
    state: State<'_, AppState>,
    id: String,
    title: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<PromptDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let mut row = state
        .prompt_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("Prompt 不存在"))?;
    // 应用 patch（仅当字段提供时）
    if let Some(t) = title {
        row.title = t.trim().to_string();
    }
    if let Some(c) = content {
        row.content = c;
    }
    if let Some(ts) = tags {
        row.tags = ts
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
    }
    row.updated_at = now_iso();
    // 推进本端计数器（CRDT：本端编辑产生新版本）
    let counter = row.vector_clock.entry(device_id.clone()).or_insert(0);
    *counter += 1;
    state.prompt_repo.update(&row).await?;
    Ok(row.to_dto())
}

/// 软删除 Prompt。对照 Python delete handler：先推进 vector_clock 再标记 deleted=1。
///
/// Business Logic: CRDT 删除是一次写入，需推进 clock 让对端感知删除事件。
///     返回 {ok: true, id}（对照 Python 返回结构）。
#[tauri::command]
pub async fn delete_prompt(
    state: State<'_, AppState>,
    id: String,
) -> Result<serde_json::Value, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let mut row = state
        .prompt_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("Prompt 不存在"))?;
    // 推进 vector_clock（CRDT 删除）
    let counter = row.vector_clock.entry(device_id).or_insert(0);
    *counter += 1;
    let now = now_iso();
    // 软删除：写回推进后的 vector_clock + updated_at + deleted=1
    state
        .prompt_repo
        .soft_delete(&id, &now, &row.vector_clock)
        .await?;
    Ok(serde_json::json!({ "ok": true, "id": id }))
}

/// 列出所有去重标签。对照 Python list_tags handler。
#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> Result<Vec<String>, AppError> {
    state.prompt_repo.list_tags().await
}
