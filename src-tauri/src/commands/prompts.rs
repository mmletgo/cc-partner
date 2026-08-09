//! commands/prompts.rs — Prompt CRUD 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 Prompt 管理页面通过 invoke 调用这些命令完成列表/详情/新建/编辑/删除/标签。
//!     行为对照 Python protocol.py 的 handle_list/create/get/update/delete/list_tags handler。
//!     N2 还提供版本历史 list/restore，让用户恢复冲突副本或本地历史快照为新 active 版本。
//!
//! Code Logic（这个模块做什么）:
//!     从 State 取 device_id 与 prompt_repo；构造 PromptRow 后调用 repo 方法；
//!     返回 PromptDto（camelCase）。vector_clock 维护：create 初始化 {device_id:1}，
//!     update/delete/restore 推进 vector_clock[device_id] += 1（CRDT 语义）。
//!     版本命令经 ContentVersionRepo 读写 content_versions。

use crate::error::AppError;
use crate::models::prompt::{PromptDto, PromptRow};
use crate::state::AppState;
use crate::storage::content_version_repo::{ContentVersion, ContentVersionRepo, KIND_HISTORY};
use crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS;
use crate::sync::merger::prompt_text_content_hash;
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashMap;
use tauri::State;
use uuid::Uuid;

/// 内容预览截断长度（字符）。
const CONTENT_PREVIEW_CHARS: usize = 200;

/// 当前时间的 RFC3339 字符串（带 UTC 时区，对照 Python datetime.now(timezone.utc).isoformat()）。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 内容版本 DTO（camelCase，对齐前端 ContentVersion）。
///
/// Business Logic: 版本历史抽屉需要 id/来源设备/hash/时间/kind 与可选标题/预览/全文。
/// Code Logic: 从 ContentVersion + snapshot_json 投影；不回传 domain/item_id。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentVersionDto {
    pub id: String,
    pub source_device: String,
    pub content_hash: String,
    pub created_at: String,
    pub kind: String,
    pub title: Option<String>,
    pub content_preview: Option<String>,
    pub content: Option<String>,
}

/// snapshot_json 中恢复所需的文本字段。
///
/// Business Logic: conflict/history 快照可能是完整 PromptRow 或精简 JSON。
/// Code Logic: 仅反序列化 title/content/tags，缺省用当前 active 行补齐。
#[derive(Debug, Clone, Deserialize)]
struct PromptSnapshotFields {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

/// 通用 snapshot 文本字段（Prompt / Scratchpad 共用）。
///
/// Business Logic: list DTO 需要从 snapshot_json 抽出 title/content 供预览与复制。
/// Code Logic: 宽松反序列化 title/content，忽略未知字段。
#[derive(Debug, Clone, Deserialize)]
struct SnapshotTextFields {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// 截断内容预览。
///
/// Business Logic: 历史列表只展示短预览，避免 IPC 传超大正文。
/// Code Logic: 按 Unicode 标量字符截到 max_chars，超出加省略号。
fn truncate_preview(content: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in content.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// ContentVersion → ContentVersionDto。
///
/// Business Logic: 前端只消费 camelCase 摘要，不需要 domain/item_id/snapshot_json。
/// Code Logic: 解析 snapshot_json 取 title/content；content_preview 截断约 200 字符。
pub fn content_version_to_dto(version: &ContentVersion) -> ContentVersionDto {
    let fields: SnapshotTextFields =
        serde_json::from_str(&version.snapshot_json).unwrap_or(SnapshotTextFields {
            title: None,
            content: None,
        });
    let content = fields.content;
    let content_preview = content
        .as_ref()
        .map(|c| truncate_preview(c, CONTENT_PREVIEW_CHARS));
    ContentVersionDto {
        id: version.id.clone(),
        source_device: version.source_device.clone(),
        content_hash: version.content_hash.clone(),
        created_at: version.created_at.clone(),
        kind: version.kind.clone(),
        title: fields.title,
        content_preview,
        content,
    }
}

/// 将 PromptRow 序列化为 content_versions 快照 JSON。
///
/// Business Logic: 恢复需要完整标题/正文/标签等字段。
/// Code Logic: serde_json 序列化 PromptRow；失败时回退精简对象。
fn prompt_snapshot_json(row: &PromptRow) -> String {
    serde_json::to_string(row).unwrap_or_else(|_| {
        serde_json::json!({
            "id": row.id,
            "title": row.title,
            "content": row.content,
            "tags": row.tags,
            "device_id": row.device_id,
            "updated_at": row.updated_at,
            "deleted": row.deleted,
        })
        .to_string()
    })
}

/// 把当前 active Prompt 写入 content_versions(kind=history)。
///
/// Business Logic: 本地编辑/恢复覆盖前保留上一版，供 UI 回看与再恢复。
/// Code Logic: 确定性 id + insert_idempotent；hash 用 prompt_text_content_hash。
async fn snapshot_prompt_history(
    version_repo: &ContentVersionRepo,
    row: &PromptRow,
    now: &str,
) -> Result<(), AppError> {
    let content_hash = prompt_text_content_hash(row);
    let version = ContentVersion {
        id: ContentVersionRepo::deterministic_id(
            DOMAIN_PROMPTS,
            &row.id,
            &row.device_id,
            &content_hash,
        ),
        domain: DOMAIN_PROMPTS.to_string(),
        item_id: row.id.clone(),
        source_device: row.device_id.clone(),
        content_hash,
        created_at: now.to_string(),
        kind: KIND_HISTORY.to_string(),
        snapshot_json: prompt_snapshot_json(row),
    };
    let _ = version_repo.insert_idempotent(&version).await?;
    Ok(())
}

/// 列出 Prompt：可选关键词搜索或单标签筛选。
///
/// Business Logic: 前端列表页传 search 或 tag 查询参数；对应 GET /api/prompts?search=&tag=。
///     favorite 过滤仅由移动端路由使用，桌面 invoke 不暴露该参数（始终 None 表示不过滤）。
#[tauri::command]
pub async fn list_prompts(
    state: State<'_, AppState>,
    search: Option<String>,
    tag: Option<String>,
) -> Result<Vec<PromptDto>, AppError> {
    let rows = state
        .prompt_repo
        .list(search.as_deref(), tag.as_deref(), None)
        .await?;
    Ok(rows.iter().map(PromptRow::to_dto).collect())
}

/// 按 ID 获取单条 Prompt；不存在或已删除返回 NotFound。
#[tauri::command]
pub async fn get_prompt(state: State<'_, AppState>, id: String) -> Result<PromptDto, AppError> {
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
        delete_epoch: 0,
        favorite: false,
    };
    state.prompt_repo.create(&row).await?;
    Ok(row.to_dto())
}

/// 更新 Prompt。对照 Python update handler：应用 title/content/tags patch，推进 vector_clock。
///
/// Business Logic: 成功本地更新前把旧 active 写入 history，保证版本抽屉可回看。
/// Code Logic: 加载旧行 → 应用 patch → 幂等写 history → update → prune_retention。
#[tauri::command]
pub async fn update_prompt(
    state: State<'_, AppState>,
    id: String,
    title: Option<String>,
    content: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<PromptDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let old = state
        .prompt_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("Prompt 不存在"))?;
    let mut row = old.clone();
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
    let now = now_iso();
    row.updated_at = now.clone();
    // 推进本端计数器（CRDT：本端编辑产生新版本）
    let counter = row.vector_clock.entry(device_id.clone()).or_insert(0);
    *counter += 1;
    row.device_id = device_id;

    let version_repo = ContentVersionRepo::new(state.prompt_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    snapshot_prompt_history(&version_repo, &old, &now).await?;
    state.prompt_repo.update(&row).await?;
    let _ = version_repo
        .prune_retention(DOMAIN_PROMPTS, &id, &now)
        .await;
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

/// 切换 Prompt 收藏状态（用户星标）。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 Prompt 库点击星标收藏/取消收藏；favorite 作为 PromptRow 元数据字段，
///     必须推进本机 vector_clock 让对端感知，并随整行 LWW 跨设备同步。收藏是元数据变更，
///     不产生内容版本（不写 content_versions history），区别于 `update_prompt`。
///
/// Code Logic（这个函数做什么）:
///     加载旧行 → flip favorite → 推进 vector_clock[device_id] += 1 → 更新 updated_at →
///     repo.update 落库；返回更新后的 PromptDto。不存在或已删除返回 NotFound。
#[tauri::command]
pub async fn toggle_prompt_favorite(
    state: State<'_, AppState>,
    id: String,
) -> Result<PromptDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let mut row = state
        .prompt_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("Prompt 不存在"))?;
    // 收藏切换只翻转 favorite，不触碰 title/content/tags
    row.favorite = !row.favorite;
    // 推进本端计数器（CRDT：元数据变更产生新版本，使对端感知）
    let counter = row.vector_clock.entry(device_id.clone()).or_insert(0);
    *counter += 1;
    row.updated_at = now_iso();
    row.device_id = device_id;
    // 仅元数据变更，不写 content_versions history（区别于 update_prompt）
    state.prompt_repo.update(&row).await?;
    Ok(row.to_dto())
}

/// 列出所有去重标签。对照 Python list_tags handler。
#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> Result<Vec<String>, AppError> {
    state.prompt_repo.list_tags().await
}

/// 列出某 Prompt 的 content_versions 历史/冲突副本。
///
/// Business Logic: 版本历史抽屉需要按时间倒序展示 history 与 conflict。
/// Code Logic: ensure_schema 后 list_versions(DOMAIN_PROMPTS, id) 映射为 ContentVersionDto。
#[tauri::command]
pub async fn list_prompt_versions(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<ContentVersionDto>, AppError> {
    let version_repo = ContentVersionRepo::new(state.prompt_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    let versions = version_repo.list_versions(DOMAIN_PROMPTS, &id).await?;
    Ok(versions.iter().map(content_version_to_dto).collect())
}

/// 将某历史/冲突版本恢复为当前 Prompt 的新 active 版本。
///
/// Business Logic: 用户从版本抽屉恢复时，必须推进本地 vector_clock，使恢复对同步可见，
///     且覆盖前把当前 active 写入 history，避免静默丢失。
///
/// Code Logic:
///     1) get version 并校验 domain=prompts 与 item_id；
///     2) 解析 snapshot 的 title/content/tags；
///     3) 加载当前 active；
///     4) 可选写当前 active → history；
///     5) 应用快照字段，device_id=local、deleted=false、clock++、updated_at=now；
///     6) update + prune_retention；返回 PromptDto。
#[tauri::command]
pub async fn restore_prompt_version(
    state: State<'_, AppState>,
    id: String,
    version_id: String,
) -> Result<PromptDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let version_repo = ContentVersionRepo::new(state.prompt_repo.pool());
    let _ = ContentVersionRepo::ensure_schema(version_repo.pool()).await;
    let version = version_repo
        .get(&version_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("版本不存在: {version_id}")))?;
    if version.domain != DOMAIN_PROMPTS || version.item_id != id {
        return Err(AppError::validation("版本与目标 Prompt 不匹配"));
    }

    let snapshot: PromptSnapshotFields = serde_json::from_str(&version.snapshot_json)
        .map_err(|e| AppError::generic(format!("版本快照解析失败: {e}")))?;

    let current = state
        .prompt_repo
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("Prompt 不存在"))?;
    let now = now_iso();
    snapshot_prompt_history(&version_repo, &current, &now).await?;

    let mut row = current;
    if let Some(t) = snapshot.title {
        row.title = t;
    }
    if let Some(c) = snapshot.content {
        row.content = c;
    }
    if let Some(ts) = snapshot.tags {
        row.tags = ts;
    }
    row.updated_at = now.clone();
    row.device_id = device_id.clone();
    row.deleted = false;
    row.delete_epoch = 0;
    let counter = row.vector_clock.entry(device_id).or_insert(0);
    *counter += 1;

    state.prompt_repo.update(&row).await?;
    let _ = version_repo
        .prune_retention(DOMAIN_PROMPTS, &id, &now)
        .await;
    Ok(row.to_dto())
}

#[cfg(test)]
mod tests {
    //! 版本 DTO 映射单测。

    use super::*;
    use crate::storage::content_version_repo::KIND_CONFLICT;

    #[test]
    fn content_version_to_dto_maps_snapshot_fields_and_truncates_preview() {
        let long = "a".repeat(250);
        let snapshot = serde_json::json!({
            "title": "t1",
            "content": long,
            "tags": ["x"],
        })
        .to_string();
        let version = ContentVersion {
            id: "v1".to_string(),
            domain: DOMAIN_PROMPTS.to_string(),
            item_id: "p1".to_string(),
            source_device: "d1".to_string(),
            content_hash: "h1".to_string(),
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            kind: KIND_CONFLICT.to_string(),
            snapshot_json: snapshot,
        };
        let dto = content_version_to_dto(&version);
        assert_eq!(dto.id, "v1");
        assert_eq!(dto.source_device, "d1");
        assert_eq!(dto.kind, KIND_CONFLICT);
        assert_eq!(dto.title.as_deref(), Some("t1"));
        assert_eq!(dto.content.as_ref().map(|c| c.len()), Some(250));
        let preview = dto.content_preview.expect("preview");
        assert!(preview.chars().count() <= CONTENT_PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }
}
