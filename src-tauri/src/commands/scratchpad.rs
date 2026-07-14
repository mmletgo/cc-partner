//! commands/scratchpad.rs — 速记本多页面 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Scratchpad 页面需要列出页面、读取页面详情、创建页面、自动保存内容、重命名和删除页面。
//!     内容权威源从 localStorage 迁移到 Rust/SQLite 后，所有页面操作都必须走这些命令。
//!     N2 还提供版本历史 list/restore，让用户恢复冲突副本或本地历史快照为新 active 版本。
//!
//! Code Logic（这个模块做什么）:
//!     每个命令只做 IPC 参数适配与 DTO 投影，具体 CRUD/向量时钟推进由 ScratchpadRepo 负责；
//!     `sync_scratchpad` 复用全局 trigger_sync，使 scratchpad 随 prompts/cc/ssh 一起同步；
//!     版本命令经 ContentVersionRepo 读写 content_versions。

use crate::commands::prompts::{content_version_to_dto, ContentVersionDto};
use crate::error::AppError;
use crate::models::scratchpad::{ScratchpadPageDto, ScratchpadPageSummaryDto, ScratchpadRow};
use crate::state::AppState;
use crate::storage::content_version_repo::{ContentVersion, ContentVersionRepo, KIND_HISTORY};
use crate::storage::sync_request_ledger_repo::DOMAIN_SCRATCHPAD;
use crate::sync::engine;
use crate::sync::scratchpad::scratchpad_text_content_hash;
use chrono::Utc;
use serde::Deserialize;
use tauri::State;

/// 删除速记本页面结果（camelCase）。
///
/// Business Logic: 前端删除当前页面后只需要确认操作成功并知道被删除的页面 id。
/// Code Logic: serde 在 IPC 边界输出 `{ok,pageId}`。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScratchpadDeleteResult {
    pub ok: bool,
    pub page_id: String,
}

/// snapshot_json 中恢复所需的文本字段。
///
/// Business Logic: conflict/history 快照可能是完整 ScratchpadRow 或精简 JSON。
/// Code Logic: 仅反序列化 title/content，缺省用当前 active 行补齐。
#[derive(Debug, Clone, Deserialize)]
struct ScratchpadSnapshotFields {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// 当前时间 RFC3339。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 将 ScratchpadRow 序列化为 content_versions 快照 JSON。
///
/// Business Logic: 恢复需要完整标题/正文。
/// Code Logic: serde_json 序列化 ScratchpadRow；失败时回退精简对象。
fn scratchpad_snapshot_json(row: &ScratchpadRow) -> String {
    serde_json::to_string(row).unwrap_or_else(|_| {
        serde_json::json!({
            "id": row.id,
            "title": row.title,
            "content": row.content,
            "device_id": row.device_id,
            "updated_at": row.updated_at,
            "deleted": row.deleted,
        })
        .to_string()
    })
}

/// 把当前 active 速记本页写入 content_versions(kind=history)。
///
/// Business Logic: 本地自动保存/恢复覆盖前保留上一版，供 UI 回看与再恢复。
/// Code Logic: 确定性 id + insert_idempotent。
async fn snapshot_scratchpad_history(
    version_repo: &ContentVersionRepo,
    row: &ScratchpadRow,
    now: &str,
) -> Result<(), AppError> {
    let content_hash = scratchpad_text_content_hash(row);
    let version = ContentVersion {
        id: ContentVersionRepo::deterministic_id(
            DOMAIN_SCRATCHPAD,
            &row.id,
            &row.device_id,
            &content_hash,
        ),
        domain: DOMAIN_SCRATCHPAD.to_string(),
        item_id: row.id.clone(),
        source_device: row.device_id.clone(),
        content_hash,
        created_at: now.to_string(),
        kind: KIND_HISTORY.to_string(),
        snapshot_json: scratchpad_snapshot_json(row),
    };
    let _ = version_repo.insert_idempotent(&version).await?;
    Ok(())
}

/// 列出所有未删除速记本页面摘要。
///
/// Business Logic: 侧栏需要展示所有可用页面，并按最近更新时间排序。
/// Code Logic: repo.list_pages 返回完整 Row；命令层投影为 summary DTO，避免传输大 content。
#[tauri::command]
pub async fn list_scratchpad_pages(
    state: State<'_, AppState>,
) -> Result<Vec<ScratchpadPageSummaryDto>, AppError> {
    let pages = state.scratchpad_repo.list_pages().await?;
    Ok(pages.iter().map(|p| p.to_summary_dto()).collect())
}

/// 获取单个速记本页面详情。
///
/// Business Logic: 页面打开时按 pageId 加载标题、内容和保存状态；默认页不存在时自动创建。
/// Code Logic: pageId="scratchpad" 走 get_or_create_default_page，其余 id 不存在则返回 not-found。
#[tauri::command]
pub async fn get_scratchpad_page(
    state: State<'_, AppState>,
    page_id: String,
) -> Result<ScratchpadPageDto, AppError> {
    let row = if page_id == crate::models::scratchpad::SCRATCHPAD_ID {
        state
            .scratchpad_repo
            .get_or_create_default_page(state.device_id.as_str())
            .await?
    } else {
        state
            .scratchpad_repo
            .get(&page_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?
    };
    Ok(row.to_dto())
}

/// 创建新的速记本页面。
///
/// Business Logic: 用户新增页面时可只传标题；空标题归一为“未命名”，内容初始为空。
/// Code Logic: repo.create_page 负责 UUID、created_at/updated_at 和 vector_clock 初始化。
#[tauri::command]
pub async fn create_scratchpad_page(
    state: State<'_, AppState>,
    title: Option<String>,
) -> Result<ScratchpadPageDto, AppError> {
    let row = state
        .scratchpad_repo
        .create_page(
            title.as_deref().unwrap_or("未命名"),
            "",
            state.device_id.as_str(),
            None,
        )
        .await?;
    Ok(row.to_dto())
}

/// 更新速记本页面内容；用于自动保存和清空。
///
/// Business Logic: 用户编辑应自动持久化到 SQLite，并推进 vector_clock 供局域网/GitHub 同步感知。
///     成功更新前把旧 active 写入 history，保证版本抽屉可回看。
/// Code Logic: 读取旧行 → 写 history → update_page_content → prune_retention。
#[tauri::command]
pub async fn update_scratchpad_page_content(
    state: State<'_, AppState>,
    page_id: String,
    content: String,
) -> Result<ScratchpadPageDto, AppError> {
    let old = state
        .scratchpad_repo
        .get(&page_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?;
    let now = now_iso();
    let version_repo = ContentVersionRepo::new(state.scratchpad_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    snapshot_scratchpad_history(&version_repo, &old, &now).await?;
    let row = state
        .scratchpad_repo
        .update_page_content(&page_id, &content, state.device_id.as_str())
        .await?;
    let _ = version_repo
        .prune_retention(DOMAIN_SCRATCHPAD, &page_id, &now)
        .await;
    Ok(row.to_dto())
}

/// 重命名速记本页面。
///
/// Business Logic: 标题是页面核心元数据，需要持久化并参与同步。
/// Code Logic: repo.rename_page 负责空标题归一化、更新时间和向量时钟推进。
#[tauri::command]
pub async fn rename_scratchpad_page(
    state: State<'_, AppState>,
    page_id: String,
    title: String,
) -> Result<ScratchpadPageDto, AppError> {
    let row = state
        .scratchpad_repo
        .rename_page(&page_id, &title, state.device_id.as_str())
        .await?;
    Ok(row.to_dto())
}

/// 删除速记本页面（软删除）。
///
/// Business Logic: 删除必须传播到其他设备和云端，因此只标记 deleted，不物理删除。
/// Code Logic: repo.soft_delete_page 推进本设备向量时钟，返回被删除页面详情供前端更新状态。
#[tauri::command]
pub async fn delete_scratchpad_page(
    state: State<'_, AppState>,
    page_id: String,
) -> Result<ScratchpadDeleteResult, AppError> {
    let row = state
        .scratchpad_repo
        .soft_delete_page(&page_id, state.device_id.as_str())
        .await?;
    Ok(ScratchpadDeleteResult {
        ok: true,
        page_id: row.id,
    })
}

/// 手动触发速记本局域网同步。
///
/// Business Logic: Scratchpad 页面提供“局域网同步”按钮；全局 trigger_sync 已纳入 scratchpad，
///     因此这里复用同一同步入口，避免维护两套设备遍历逻辑。
/// Code Logic: 调 sync::engine::trigger_sync 并序列化为前端已有的 `{accepted,synced,note}` 结构。
#[tauri::command]
pub async fn sync_scratchpad(state: State<'_, AppState>) -> Result<serde_json::Value, AppError> {
    let result = engine::trigger_sync(state.inner()).await;
    Ok(serde_json::to_value(&result)?)
}

/// 列出某速记本页的 content_versions 历史/冲突副本。
///
/// Business Logic: 版本历史抽屉需要按时间倒序展示 history 与 conflict。
/// Code Logic: ensure_schema 后 list_versions(DOMAIN_SCRATCHPAD, pageId) 映射为 ContentVersionDto。
#[tauri::command]
pub async fn list_scratchpad_versions(
    state: State<'_, AppState>,
    page_id: String,
) -> Result<Vec<ContentVersionDto>, AppError> {
    let version_repo = ContentVersionRepo::new(state.scratchpad_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    let versions = version_repo
        .list_versions(DOMAIN_SCRATCHPAD, &page_id)
        .await?;
    Ok(versions.iter().map(content_version_to_dto).collect())
}

/// 将某历史/冲突版本恢复为当前速记本页的新 active 版本。
///
/// Business Logic: 用户从版本抽屉恢复时，必须推进本地 vector_clock，使恢复对同步可见，
///     且覆盖前把当前 active 写入 history，避免静默丢失。
///
/// Code Logic:
///     1) get version 并校验 domain=scratchpad 与 item_id；
///     2) 解析 snapshot 的 title/content；
///     3) 加载当前 active；
///     4) 写当前 active → history；
///     5) 应用快照字段，device_id=local、deleted=false、clock++、updated_at=now；
///     6) upsert + prune_retention；返回 ScratchpadPageDto。
#[tauri::command]
pub async fn restore_scratchpad_version(
    state: State<'_, AppState>,
    page_id: String,
    version_id: String,
) -> Result<ScratchpadPageDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let version_repo = ContentVersionRepo::new(state.scratchpad_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    let version = version_repo
        .get(&version_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("版本不存在: {version_id}")))?;
    if version.domain != DOMAIN_SCRATCHPAD || version.item_id != page_id {
        return Err(AppError::validation("版本与目标速记本页面不匹配"));
    }

    let snapshot: ScratchpadSnapshotFields = serde_json::from_str(&version.snapshot_json)
        .map_err(|e| AppError::generic(format!("版本快照解析失败: {e}")))?;

    let current = state
        .scratchpad_repo
        .get(&page_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("速记本页面不存在: {page_id}")))?;
    let now = now_iso();
    snapshot_scratchpad_history(&version_repo, &current, &now).await?;

    let mut row = current;
    if let Some(t) = snapshot.title {
        row.title = t;
    }
    if let Some(c) = snapshot.content {
        row.content = c;
    }
    row.updated_at = now.clone();
    row.device_id = device_id.clone();
    row.deleted = false;
    row.delete_epoch = 0;
    let counter = row.vector_clock.entry(device_id).or_insert(0);
    *counter += 1;

    state.scratchpad_repo.upsert(&row).await?;
    let _ = version_repo
        .prune_retention(DOMAIN_SCRATCHPAD, &page_id, &now)
        .await;
    Ok(row.to_dto())
}
