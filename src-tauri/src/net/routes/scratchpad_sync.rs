//! net/routes/scratchpad_sync.rs — 速记本同步（legacy pull/push + v2 manifest/items/push-batch）
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网设备间同步多个速记本页面。legacy 路径保留；v2 无状态分页与 Prompt 同构。
//!     `sync.manifest.v2` 由 Task 3 原子宣告。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/scratchpad/sync/pull|push：legacy。
//!     - POST /api/scratchpad/sync/manifest-page|items|push-batch：有界 typed 协议。

use crate::error::AppError;
use crate::models::scratchpad::ScratchpadRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::sync_request_ledger_repo::SyncRequestLedgerRepo;
use crate::storage::ScratchpadRepo;
use crate::sync::apply_merge::apply_scratchpad_merge_batch;
use crate::sync::protocol::{
    content_sha256_hex, decode_keyset_cursor, encode_keyset_cursor, estimate_summary_wire_bytes,
    SyncManifestPage, SyncSummary, MANIFEST_PAGE_BYTES, MANIFEST_PAGE_ITEMS, PUSH_BATCH_BYTES,
    PUSH_BATCH_ITEMS,
};
use crate::sync::scratchpad::{merge_scratchpad, scratchpad_changed};
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Scratchpad v2 路由 body 上限。
pub const SCRATCHPAD_SYNC_ROUTE_BODY_LIMIT_BYTES: usize = PUSH_BATCH_BYTES;
pub const CODE_BATCH_TOO_LARGE: &str = "scratchpad.batch_too_large";
pub const CODE_ITEM_TOO_LARGE: &str = "scratchpad.item_too_large";
pub const CODE_INVALID_CURSOR: &str = "scratchpad.invalid_cursor";
const ID_MAX_BYTES: usize = 256;
const CONTENT_MAX_BYTES: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Legacy DTOs
// ---------------------------------------------------------------------------

/// scratchpad/sync/pull 请求体：对端发来的页面摘要列表。
#[derive(Debug, Deserialize)]
pub struct ScratchpadPullReq {
    #[serde(default)]
    pub summaries: Vec<ScratchpadSummary>,
}

/// 单个速记本页面摘要。
#[derive(Debug, Deserialize)]
pub struct ScratchpadSummary {
    pub id: String,
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// scratchpad/sync/pull 响应体：本端需下发给对端的完整页面列表。
#[derive(Debug, Serialize)]
pub struct ScratchpadPullResp {
    pub pages: Vec<ScratchpadRow>,
}

/// scratchpad/sync/push 请求体：对端推送的完整页面列表。
#[derive(Debug, Deserialize)]
pub struct ScratchpadPushReq {
    #[serde(default)]
    pub pages: Vec<ScratchpadRow>,
}

/// scratchpad/sync/push 响应体：实际落库条数。
#[derive(Debug, Serialize)]
pub struct ScratchpadPushResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// V2 DTOs
// ---------------------------------------------------------------------------

/// manifest-page 请求。
#[derive(Debug, Deserialize)]
pub struct ScratchpadManifestPageReq {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// items 请求。
#[derive(Debug, Deserialize)]
pub struct ScratchpadItemsReq {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// items 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadItemsResp {
    pub items: Vec<ScratchpadRow>,
    pub missing_ids: Vec<String>,
}

/// push-batch 请求。
#[derive(Debug, Deserialize)]
pub struct ScratchpadPushBatchReq {
    #[serde(default)]
    pub items: Vec<ScratchpadRow>,
    pub client_request_id: String,
    /// 收敛标签（非认证）；缺省空串。
    #[serde(default)]
    pub claimed_device_id: String,
    /// 完整 manifest + 成功 apply 后客户端回传的最高连续 ackedDeleteEpoch；缺省 None 不推进水位。
    #[serde(default)]
    pub acked_delete_epoch: Option<u64>,
}

/// push-batch 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadPushBatchResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// RouteFail
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum RouteFail {
    InvalidCursor,
    BatchTooLarge(String),
    ItemTooLarge(String),
    Validation(String),
    App(AppError),
}

impl RouteFail {
    /// 转为边界 P2pError。
    ///
    /// Business Logic: 客户端依赖稳定 code 做拆批/终止。
    /// Code Logic: 映射 invalid_cursor/batch_too_large/item_too_large/validation/app。
    fn into_p2p(self, ctx: &P2pRequestContext, domain: &str) -> P2pError {
        match self {
            RouteFail::InvalidCursor => P2pError::stable(
                "非法或损坏的 manifest cursor",
                CODE_INVALID_CURSOR,
                StatusCode::BAD_REQUEST,
                ctx,
                false,
            ),
            RouteFail::BatchTooLarge(msg) => P2pError::stable(
                msg,
                CODE_BATCH_TOO_LARGE,
                StatusCode::PAYLOAD_TOO_LARGE,
                ctx,
                false,
            ),
            RouteFail::ItemTooLarge(msg) => P2pError::stable(
                msg,
                CODE_ITEM_TOO_LARGE,
                StatusCode::UNPROCESSABLE_ENTITY,
                ctx,
                false,
            ),
            RouteFail::Validation(msg) => P2pError::validation(msg, ctx),
            RouteFail::App(e) => P2pError::from_app_error(e, ctx, domain),
        }
    }
}

impl From<AppError> for RouteFail {
    fn from(value: AppError) -> Self {
        RouteFail::App(value)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Scratchpad 正文指纹（title/content）。
fn scratchpad_content_hash(row: &ScratchpadRow) -> String {
    content_sha256_hex(&[row.title.as_bytes(), b"\0", row.content.as_bytes()])
}

/// 行 → SyncSummary。
fn scratchpad_to_summary(row: &ScratchpadRow) -> SyncSummary<String> {
    SyncSummary {
        id: row.id.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash: scratchpad_content_hash(row),
        size: row.content.len() as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
        delete_epoch: row.delete_epoch,
    }
}

/// 估算完整行 wire 字节。
fn estimate_scratchpad_row_bytes(row: &ScratchpadRow) -> usize {
    let vc_len = serde_json::to_string(&row.vector_clock)
        .map(|s| s.len())
        .unwrap_or(2);
    row.id.len()
        + row.title.len()
        + row.content.len()
        + row.created_at.len()
        + row.updated_at.len()
        + row.device_id.len()
        + vc_len
        + 64
}

fn validate_sync_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("id 不能为空或空白".to_string());
    }
    if id.len() > ID_MAX_BYTES {
        return Err(format!(
            "id 超过 {ID_MAX_BYTES} UTF-8 字节上限（收到 {} 字节）",
            id.len()
        ));
    }
    Ok(())
}

fn validate_id_list(ids: &[String]) -> Result<(), RouteFail> {
    if ids.len() > PUSH_BATCH_ITEMS {
        return Err(RouteFail::BatchTooLarge(format!(
            "单批最多 {PUSH_BATCH_ITEMS} 个 id，收到 {}",
            ids.len()
        )));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    for id in ids {
        if let Err(msg) = validate_sync_id(id) {
            return Err(RouteFail::Validation(msg));
        }
        if !seen.insert(id.as_str()) {
            return Err(RouteFail::Validation("ids 含重复 id".to_string()));
        }
    }
    Ok(())
}

fn validate_scratchpad_row(row: &ScratchpadRow) -> Result<(), RouteFail> {
    if let Err(msg) = validate_sync_id(&row.id) {
        return Err(RouteFail::Validation(msg));
    }
    if row.content.len() > CONTENT_MAX_BYTES {
        return Err(RouteFail::ItemTooLarge(format!(
            "content 超过 {CONTENT_MAX_BYTES} 字节上限（收到 {} 字节）",
            row.content.len()
        )));
    }
    let est = estimate_scratchpad_row_bytes(row);
    if est > PUSH_BATCH_BYTES {
        return Err(RouteFail::ItemTooLarge(format!(
            "单条估算 {est} 字节超过批上限 {PUSH_BATCH_BYTES}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Legacy handlers
// ---------------------------------------------------------------------------

/// POST /api/scratchpad/sync/pull：接收对端摘要，返回本端需下发的页面。
///
/// Business Logic: 若本端某页对端没有、本端版本领先或双方并发，对端需要拿到完整页面再合并。
/// Code Logic: get_all_for_sync 含 deleted；compare(local, remote_clock) 判断 After/Concurrent。
pub async fn scratchpad_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ScratchpadPullReq>,
) -> P2pResult<Json<ScratchpadPullResp>> {
    let pages = scratchpad_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "scratchpad.pull"))?;
    Ok(Json(ScratchpadPullResp { pages }))
}

/// scratchpad_pull 业务实现：返回需要下发的页面列表。
async fn scratchpad_pull_impl(
    state: &AppState,
    req: ScratchpadPullReq,
) -> Result<Vec<ScratchpadRow>, AppError> {
    let remote_map: HashMap<&str, &HashMap<String, u64>> = req
        .summaries
        .iter()
        .map(|s| (s.id.as_str(), &s.vector_clock))
        .collect();
    let local_all = state.scratchpad_repo.get_all_for_sync().await?;

    let mut pages: Vec<ScratchpadRow> = Vec::new();
    for page in &local_all {
        match remote_map.get(page.id.as_str()) {
            None => pages.push(page.clone()),
            Some(remote_clock) => {
                let relation = compare(&page.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After)
                    || matches!(relation, ClockOrder::Concurrent)
                {
                    pages.push(page.clone());
                }
            }
        }
    }

    tracing::info!(
        "scratchpad/sync/pull: 对端摘要 {} 条，本端 {} 条，返回 {} 条",
        req.summaries.len(),
        local_all.len(),
        pages.len()
    );
    Ok(pages)
}

/// POST /api/scratchpad/sync/push：接收对端页面，逐条合并后按需落库。
///
/// Business Logic: 对端推送可能是领先、落后或并发版本；本端必须用同一套 LWW 策略合并，保证最终一致。
/// Code Logic: 本地没有则直接接收；本地已有则 merge_scratchpad，再用 scratchpad_changed 判断是否写库。
pub async fn scratchpad_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ScratchpadPushReq>,
) -> P2pResult<Json<ScratchpadPushResp>> {
    let accepted = scratchpad_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "scratchpad.push"))?;
    Ok(Json(ScratchpadPushResp { accepted }))
}

/// scratchpad_push 业务实现：逐条合并后按需落库，返回实际落库条数。
async fn scratchpad_push_impl(state: &AppState, req: ScratchpadPushReq) -> Result<usize, AppError> {
    let mut to_upsert: Vec<ScratchpadRow> = Vec::new();

    for remote in req.pages {
        match state.scratchpad_repo.get(&remote.id).await? {
            None => to_upsert.push(remote),
            Some(local) => {
                let merged = merge_scratchpad(&local, &remote);
                if scratchpad_changed(&merged, &local) {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let accepted = to_upsert.len();
    if !to_upsert.is_empty() {
        state.scratchpad_repo.bulk_upsert(&to_upsert).await?;
    }

    tracing::info!("scratchpad/sync/push: 接收并落库 {} 个页面", accepted);
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// V2 handlers
// ---------------------------------------------------------------------------

/// POST /api/scratchpad/sync/manifest-page：无状态分页速记本摘要。
///
/// Business Logic: client 拉完排序页后再与本地完整 manifest 比较。
/// Code Logic: keyset by id + page 预算。
pub async fn scratchpad_manifest_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ScratchpadManifestPageReq>,
) -> P2pResult<Json<SyncManifestPage<String>>> {
    let page = scratchpad_manifest_page_impl(state.scratchpad_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "scratchpad.manifest_page"))?;
    Ok(Json(page))
}

/// manifest-page 业务实现。
async fn scratchpad_manifest_page_impl(
    repo: &ScratchpadRepo,
    req: ScratchpadManifestPageReq,
) -> Result<SyncManifestPage<String>, RouteFail> {
    let limit = match req.limit {
        None => MANIFEST_PAGE_ITEMS as u32,
        Some(0) => {
            return Err(RouteFail::Validation(format!(
                "manifest limit 必须在 1..={MANIFEST_PAGE_ITEMS}"
            )));
        }
        Some(n) if n as usize > MANIFEST_PAGE_ITEMS => {
            return Err(RouteFail::Validation(format!(
                "manifest limit 最大 {MANIFEST_PAGE_ITEMS}，收到 {n}"
            )));
        }
        Some(n) => n,
    };

    let after_id = match req.cursor.as_deref() {
        None | Some("") => None,
        Some(c) => Some(decode_keyset_cursor(c).map_err(|_| RouteFail::InvalidCursor)?),
    };

    let mut local_all = repo.get_all_for_sync().await?;
    local_all.sort_by(|a, b| a.id.cmp(&b.id));

    let start = match after_id.as_deref() {
        None => 0,
        Some(after) => local_all
            .iter()
            .position(|r| r.id.as_str() > after)
            .unwrap_or(local_all.len()),
    };
    let end = (start + limit as usize).min(local_all.len());
    let page_rows = &local_all[start..end];
    let has_more = end < local_all.len();

    let mut items: Vec<SyncSummary<String>> = Vec::with_capacity(page_rows.len());
    let mut estimated = 0usize;
    for row in page_rows {
        let summary = scratchpad_to_summary(row);
        estimated = estimated.saturating_add(estimate_summary_wire_bytes(&summary));
        if items.len() + 1 > MANIFEST_PAGE_ITEMS || estimated > MANIFEST_PAGE_BYTES {
            return Err(RouteFail::BatchTooLarge(format!(
                "manifest page 超过预算 items≤{MANIFEST_PAGE_ITEMS} bytes≤{MANIFEST_PAGE_BYTES}"
            )));
        }
        items.push(summary);
    }

    let next_cursor = if has_more {
        items.last().map(|s| encode_keyset_cursor(&s.id))
    } else {
        None
    };

    Ok(SyncManifestPage {
        items,
        next_cursor,
    })
}

/// POST /api/scratchpad/sync/items：按 id 批取正文。
pub async fn scratchpad_items(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ScratchpadItemsReq>,
) -> P2pResult<Json<ScratchpadItemsResp>> {
    let resp = scratchpad_items_impl(state.scratchpad_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "scratchpad.items"))?;
    Ok(Json(resp))
}

/// items 业务实现。
async fn scratchpad_items_impl(
    repo: &ScratchpadRepo,
    req: ScratchpadItemsReq,
) -> Result<ScratchpadItemsResp, RouteFail> {
    validate_id_list(&req.ids)?;
    let mut items = Vec::new();
    let mut missing_ids = Vec::new();
    let mut estimated = 0usize;
    for id in &req.ids {
        match repo.get(id).await? {
            Some(row) => {
                if row.content.len() > CONTENT_MAX_BYTES {
                    return Err(RouteFail::ItemTooLarge(format!(
                        "本地条目 content 超过 {CONTENT_MAX_BYTES} 字节"
                    )));
                }
                let est = estimate_scratchpad_row_bytes(&row);
                if est > PUSH_BATCH_BYTES {
                    return Err(RouteFail::ItemTooLarge(
                        "本地单条估算超过 4MiB 批上限".to_string(),
                    ));
                }
                estimated = estimated.saturating_add(est);
                if estimated > PUSH_BATCH_BYTES {
                    return Err(RouteFail::BatchTooLarge(format!(
                        "items 响应估算超过 {PUSH_BATCH_BYTES} 字节"
                    )));
                }
                items.push(row);
            }
            None => missing_ids.push(id.clone()),
        }
    }
    Ok(ScratchpadItemsResp { items, missing_ids })
}

/// 计算 Scratchpad push-batch 载荷指纹。
///
/// Business Logic: ledger 用稳定 payload hash 区分同 request_id 的不同正文。
/// Code Logic: 按 id 排序后拼接关键字段并 SHA-256。
fn scratchpad_batch_payload_hash(items: &[ScratchpadRow]) -> String {
    let mut sorted: Vec<&ScratchpadRow> = items.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for p in sorted {
        let vc = serde_json::to_string(&p.vector_clock).unwrap_or_else(|_| "{}".to_string());
        parts.push(p.id.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(p.title.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(p.content.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(vc.into_bytes());
        parts.push(b"\0".to_vec());
        parts.push(p.updated_at.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(if p.deleted { b"1" } else { b"0" }.to_vec());
        parts.push(b"\n".to_vec());
    }
    let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
    content_sha256_hex(&refs)
}

/// POST /api/scratchpad/sync/push-batch：批量 merge + 事务 bulk + ledger 幂等。
///
/// Business Logic: 同 Prompt push-batch——单事务 claim ledger、生产 bulk、记录 accepted。
/// Code Logic: 校验 → merge → apply_batch_idempotent(bulk_upsert_on_tx)。
pub async fn scratchpad_push_batch(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ScratchpadPushBatchReq>,
) -> P2pResult<Json<ScratchpadPushBatchResp>> {
    let accepted = scratchpad_push_batch_impl(state.scratchpad_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "scratchpad.push_batch"))?;
    Ok(Json(ScratchpadPushBatchResp { accepted }))
}

/// push-batch 业务实现。
async fn scratchpad_push_batch_impl(
    repo: &ScratchpadRepo,
    req: ScratchpadPushBatchReq,
) -> Result<usize, RouteFail> {
    let client_request_id = req.client_request_id.trim().to_string();
    if client_request_id.is_empty() {
        return Err(RouteFail::Validation(
            "client_request_id 不能为空".to_string(),
        ));
    }
    if req.items.len() > PUSH_BATCH_ITEMS {
        return Err(RouteFail::BatchTooLarge(format!(
            "push-batch 最多 {PUSH_BATCH_ITEMS} 条，收到 {}",
            req.items.len()
        )));
    }
    let mut estimated = 0usize;
    let mut seen = HashSet::with_capacity(req.items.len());
    for item in &req.items {
        validate_scratchpad_row(item)?;
        if !seen.insert(item.id.as_str()) {
            return Err(RouteFail::Validation("items 含重复 id".to_string()));
        }
        estimated = estimated.saturating_add(estimate_scratchpad_row_bytes(item));
        if estimated > PUSH_BATCH_BYTES {
            return Err(RouteFail::BatchTooLarge(format!(
                "push-batch 估算超过 {PUSH_BATCH_BYTES} 字节"
            )));
        }
    }

    let payload_hash = scratchpad_batch_payload_hash(&req.items);
    let claimed_device_id = req.claimed_device_id.trim().to_string();
    let acked_delete_epoch = req.acked_delete_epoch;
    let items = req.items;

    let outcome = apply_scratchpad_merge_batch(
        &repo.pool(),
        repo,
        &claimed_device_id,
        &client_request_id,
        &payload_hash,
        &items,
        acked_delete_epoch,
    )
    .await
    .map_err(RouteFail::from)?;
    Ok(outcome.accepted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn setup_repo() -> ScratchpadRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scratchpad (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
             vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        SyncRequestLedgerRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::content_version_repo::ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::deletion_floor_repo::DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::sync_watermark_repo::SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        let _ = sqlx::query(
            "ALTER TABLE scratchpad ADD COLUMN delete_epoch INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await;
        ScratchpadRepo::new(pool)
    }

    fn sample_row(id: &str, content: &str, vc: u64) -> ScratchpadRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("d1".to_string(), vc);
        ScratchpadRow {
            id: id.to_string(),
            title: format!("t-{id}"),
            content: content.to_string(),
            created_at: "2026-07-14T00:00:00Z".to_string(),
            updated_at: "2026-07-14T00:00:00Z".to_string(),
            device_id: "d1".to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: 0,
        }
    }

    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-sp-test".to_string(),
        }
    }

    fn assert_fail(fail: RouteFail, status: StatusCode, code: &str) {
        let p2p = fail.into_p2p(&ctx(), "scratchpad.test");
        assert_eq!(p2p.status(), status);
        assert_eq!(p2p.envelope().code, code);
    }

    #[tokio::test]
    async fn manifest_rejects_invalid_cursor() {
        let repo = setup_repo().await;
        let err = scratchpad_manifest_page_impl(
            &repo,
            ScratchpadManifestPageReq {
                cursor: Some("bad".into()),
                limit: Some(10),
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::BAD_REQUEST, CODE_INVALID_CURSOR);
    }

    #[tokio::test]
    async fn push_items_manifest_happy_path() {
        let repo = setup_repo().await;
        let accepted = scratchpad_push_batch_impl(
            &repo,
            ScratchpadPushBatchReq {
                items: vec![sample_row("p1", "hello", 1), sample_row("p2", "world", 1)],
                client_request_id: "sp-1".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(accepted, 2);

        let page = scratchpad_manifest_page_impl(
            &repo,
            ScratchpadManifestPageReq {
                cursor: None,
                limit: Some(1),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 1);
        assert!(page.next_cursor.is_some());

        let items = scratchpad_items_impl(
            &repo,
            ScratchpadItemsReq {
                ids: vec!["p1".into(), "missing".into()],
            },
        )
        .await
        .unwrap();
        assert_eq!(items.items.len(), 1);
        assert_eq!(items.missing_ids, vec!["missing".to_string()]);
    }

    #[tokio::test]
    async fn push_batch_rejects_over_limit() {
        let repo = setup_repo().await;
        let items: Vec<ScratchpadRow> = (0..=PUSH_BATCH_ITEMS)
            .map(|i| sample_row(&format!("id-{i}"), "c", 1))
            .collect();
        let err = scratchpad_push_batch_impl(
            &repo,
            ScratchpadPushBatchReq {
                items,
                client_request_id: "sp-2".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::PAYLOAD_TOO_LARGE, CODE_BATCH_TOO_LARGE);
    }
}
