//! net/routes/cc_history.rs — /api/cc-history/sync/* handler（P2P CC 历史同步）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起 Claude Code 历史同步时调用这些端点。legacy pull/push 保留一代兼容；
//!     新分页协议（capability `cc-history.paged-sync.v1`）用 manifest-page / items /
//!     push-batch 做有资源上限的批处理，避免 10k+ 行全量 body 与内存峰值。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/cc-history/sync/pull|push：legacy 全量摘要/正文路径（保持不变）。
//!     - POST /api/cc-history/sync/manifest-page：keyset 分页摘要 `{summaries,next_cursor,done}`。
//!     - POST /api/cc-history/sync/items：按 ID 批取正文，保序 + missing_ids。
//!     - POST /api/cc-history/sync/push-batch：批量 merge 后事务 upsert。
//!     - 不透明 cursor：base64url(NO_PAD) of JSON `{v:1,last_id}`。
//!     - 稳定错误：413 `cc_history.batch_too_large`、422 `cc_history.item_too_large`、
//!       400 `cc_history.invalid_cursor`；日志不写 ID/content。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::{CcSyncSummary, ClaudeHistoryRow};
use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::ClaudeHistoryRepo;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// 分页协议常量
// ---------------------------------------------------------------------------

/// manifest 默认 page size。
pub const CC_MANIFEST_PAGE_LIMIT_DEFAULT: u32 = 256;
/// manifest 最大 page size。
pub const CC_MANIFEST_PAGE_LIMIT_MAX: u32 = 512;
/// items / push-batch 单批最大条数。
pub const CC_ITEM_BATCH_LIMIT: usize = 128;
/// 单条 content UTF-8 字节上限（1 MiB）。
pub const CC_CONTENT_MAX_BYTES: usize = 1024 * 1024;
/// 单次 items 响应 / push-batch 请求估算上限（8 MiB）。
pub const CC_BATCH_MAX_ESTIMATED_BYTES: usize = 8 * 1024 * 1024;
/// 单个 ID UTF-8 字节上限。
pub const CC_ID_MAX_BYTES: usize = 256;
/// 分页路由 body limit（8 MiB），与业务估算上限对齐。
pub const CC_ROUTE_BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// 稳定错误码：批过大（HTTP 413，retryable=false）。
pub const CODE_BATCH_TOO_LARGE: &str = "cc_history.batch_too_large";
/// 稳定错误码：单条过大（HTTP 422）。
pub const CODE_ITEM_TOO_LARGE: &str = "cc_history.item_too_large";
/// 稳定错误码：非法 cursor（HTTP 400）。
pub const CODE_INVALID_CURSOR: &str = "cc_history.invalid_cursor";

// ---------------------------------------------------------------------------
// Legacy DTOs
// ---------------------------------------------------------------------------

/// cc-history/sync/pull 请求体：对端发来的 cc 历史摘要列表。
#[derive(Debug, Deserialize)]
pub struct CcSyncPullReq {
    #[serde(default)]
    pub summaries: Vec<CcSummary>,
}

/// 单条 cc 历史摘要（id + 向量时钟）。
#[derive(Debug, Deserialize)]
pub struct CcSummary {
    pub id: String,
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// cc-history/sync/pull 响应体：本端需要下发给对端的完整 cc 历史列表。
#[derive(Debug, Serialize)]
pub struct CcSyncPullResp {
    pub items: Vec<ClaudeHistoryRow>,
}

/// cc-history/sync/push 请求体：对端推送来的完整 cc 历史列表。
#[derive(Debug, Deserialize)]
pub struct CcSyncPushReq {
    #[serde(default)]
    pub items: Vec<ClaudeHistoryRow>,
}

/// cc-history/sync/push 响应体：实际落库条数。
#[derive(Debug, Serialize)]
pub struct CcSyncPushResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// Paged DTOs
// ---------------------------------------------------------------------------

/// manifest-page 请求：可选 cursor + limit。
#[derive(Debug, Deserialize)]
pub struct CcManifestPageReq {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// manifest-page 响应：摘要页 + 下一游标 + 是否结束。
#[derive(Debug, Serialize, Deserialize)]
pub struct CcManifestPageResp {
    pub summaries: Vec<CcSyncSummary>,
    pub next_cursor: Option<String>,
    pub done: bool,
}

/// items 请求：按 ID 批量取正文。
#[derive(Debug, Deserialize)]
pub struct CcItemsReq {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// items 响应：保序存在行 + 缺失 ID。
#[derive(Debug, Serialize, Deserialize)]
pub struct CcItemsResp {
    pub items: Vec<ClaudeHistoryRow>,
    pub missing_ids: Vec<String>,
}

/// push-batch 请求：已由对端筛选的完整行列表。
#[derive(Debug, Deserialize)]
pub struct CcPushBatchReq {
    #[serde(default)]
    pub items: Vec<ClaudeHistoryRow>,
}

/// push-batch 响应：实际事务写入条数。
#[derive(Debug, Serialize, Deserialize)]
pub struct CcPushBatchResp {
    pub accepted: usize,
}

/// cursor v1 载荷：不透明编码前的 JSON 结构。
#[derive(Debug, Serialize, Deserialize)]
struct CursorV1 {
    v: u32,
    last_id: String,
}

// ---------------------------------------------------------------------------
// Cursor codec
// ---------------------------------------------------------------------------

/// 编码不透明 cursor：base64url(NO_PAD) of `{v:1,last_id}`。
///
/// Business Logic（为什么需要这个函数）:
///     客户端不得解析 cursor 语义；服务端用 last_id keyset 翻页，v 字段预留版本演进。
///
/// Code Logic（这个函数做什么）:
///     序列化 CursorV1 为 JSON 字节，再用 URL_SAFE_NO_PAD base64 编码为字符串。
pub fn encode_manifest_cursor(last_id: &str) -> String {
    let payload = CursorV1 {
        v: 1,
        last_id: last_id.to_string(),
    };
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

/// 解码不透明 cursor，成功返回 last_id。
///
/// Business Logic（为什么需要这个函数）:
///     非法/损坏/错误版本 cursor 必须在访问 DB 前拒绝，返回稳定 `invalid_cursor`。
///
/// Code Logic（这个函数做什么）:
///     base64url 解码 → JSON 解析 → 要求 v==1 且 last_id 非空，否则 Err。
pub fn decode_manifest_cursor(cursor: &str) -> Result<String, ()> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor.as_bytes()).map_err(|_| ())?;
    let payload: CursorV1 = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if payload.v != 1 || payload.last_id.is_empty() {
        return Err(());
    }
    Ok(payload.last_id)
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// 校验单个同步 ID：非空白、≤256 UTF-8 字节。
///
/// Business Logic（为什么需要这个函数）:
///     空白/超长 ID 会撑爆索引与 IN 列表，必须在路由边界拒绝。
///
/// Code Logic（这个函数做什么）:
///     trim 后非空且 `len() <= CC_ID_MAX_BYTES`。
fn validate_sync_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("id 不能为空或空白".to_string());
    }
    if id.len() > CC_ID_MAX_BYTES {
        return Err(format!(
            "id 超过 {CC_ID_MAX_BYTES} UTF-8 字节上限（收到 {} 字节）",
            id.len()
        ));
    }
    Ok(())
}

/// 校验 ID 列表：条数、空白、超长、重复。
///
/// Business Logic（为什么需要这个函数）:
///     items/push 路径必须在 DB 前强制 batch 与 ID 约束，避免无界绑定与重复处理。
///
/// Code Logic（这个函数做什么）:
///     长度 >128 → batch_too_large；任一 ID 非法或集合内重复 → validation 文案。
fn validate_id_list(ids: &[String]) -> Result<(), RouteFail> {
    if ids.len() > CC_ITEM_BATCH_LIMIT {
        return Err(RouteFail::BatchTooLarge(format!(
            "单批最多 {CC_ITEM_BATCH_LIMIT} 个 id，收到 {}",
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

/// 估算单行序列化前字节占用（字段 UTF-8 + 向量时钟 JSON）。
///
/// Business Logic（为什么需要这个函数）:
///     8 MiB 上限需在序列化前确定性估算，避免真实 JSON 膨胀后才失败。
///
/// Code Logic（这个函数做什么）:
///     累加各字符串字段 UTF-8 长度 + vector_clock 紧凑 JSON 长度；Option 取 Some 长度。
pub fn estimate_row_bytes(row: &ClaudeHistoryRow) -> usize {
    let vc_len = serde_json::to_string(&row.vector_clock)
        .map(|s| s.len())
        .unwrap_or(2);
    row.id.len()
        + row.project_path.len()
        + row.project_name.len()
        + row.session_id.len()
        + row.content.len()
        + row.git_branch.as_ref().map(|s| s.len()).unwrap_or(0)
        + row.cc_version.as_ref().map(|s| s.len()).unwrap_or(0)
        + row.occurred_at.len()
        + row.device_id.len()
        + vc_len
        + row.created_at.len()
        + row.updated_at.len()
        + 1 // deleted
}

/// 校验完整行：id + content 上限 + 单条估算不超 8MiB。
///
/// Business Logic（为什么需要这个函数）:
///     单条 content 超 1MiB 或单条本身就超批上限时，客户端不得对半拆分继续，
///     必须以 item_too_large 结束本轮。
///
/// Code Logic（这个函数做什么）:
///     content.len()>1MiB 或 estimate_row_bytes>8MiB → ItemTooLarge；id 非法 → Validation。
fn validate_row(row: &ClaudeHistoryRow) -> Result<(), RouteFail> {
    if let Err(msg) = validate_sync_id(&row.id) {
        return Err(RouteFail::Validation(msg));
    }
    if row.content.len() > CC_CONTENT_MAX_BYTES {
        return Err(RouteFail::ItemTooLarge(format!(
            "content 超过 {CC_CONTENT_MAX_BYTES} 字节上限（收到 {} 字节）",
            row.content.len()
        )));
    }
    let est = estimate_row_bytes(row);
    if est > CC_BATCH_MAX_ESTIMATED_BYTES {
        return Err(RouteFail::ItemTooLarge(format!(
            "单条估算 {est} 字节超过批上限 {CC_BATCH_MAX_ESTIMATED_BYTES}"
        )));
    }
    Ok(())
}

/// 路由层失败分类，映射到稳定 code/status。
#[derive(Debug)]
enum RouteFail {
    InvalidCursor,
    BatchTooLarge(String),
    ItemTooLarge(String),
    Validation(String),
    App(AppError),
}

impl RouteFail {
    /// 转为边界 P2pError（精确 status/code/retryable）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     分页协议客户端依赖稳定 code 做拆批/终止；必须精确 status 与 retryable=false。
    ///
    /// Code Logic（这个函数做什么）:
    ///     InvalidCursor→400+invalid_cursor；BatchTooLarge→413+batch_too_large retryable=false；
    ///     ItemTooLarge→422+item_too_large；Validation→400 validation_error；App→from_app_error。
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
// Legacy handlers
// ---------------------------------------------------------------------------

/// POST /api/cc-history/sync/pull：接收对端摘要，返回本端需要下发的 cc 历史。
///
/// Business Logic: 对端把它的 cc 历史摘要发来，本端比对后返回"本端有而对端没有 / 本端领先 /
///     并发"的完整 cc 历史供对端合并。
///
/// Code Logic:
///     1. 取本端全部 cc 历史（get_all_for_sync，含 deleted）；
///     2. 构建对端摘要查找表 {id: vector_clock}；
///     3. 对本端每条：对端没有 → 下发；有则 compare(local, remote)，After/Concurrent → 下发；
///     4. 返回完整 ClaudeHistoryRow 列表（snake_case）。
pub async fn cc_sync_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<CcSyncPullReq>,
) -> P2pResult<Json<CcSyncPullResp>> {
    let items = cc_sync_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "cc_history.pull"))?;
    Ok(Json(CcSyncPullResp { items }))
}

/// cc_sync_pull 业务实现：返回需要下发的 cc 历史列表。
async fn cc_sync_pull_impl(
    state: &AppState,
    req: CcSyncPullReq,
) -> Result<Vec<ClaudeHistoryRow>, AppError> {
    let remote_map: HashMap<&str, &HashMap<String, u64>> = req
        .summaries
        .iter()
        .map(|s| (s.id.as_str(), &s.vector_clock))
        .collect();

    let local_all = state.cc_history_repo.get_all_for_sync().await?;

    let mut items: Vec<ClaudeHistoryRow> = Vec::new();
    for p in &local_all {
        match remote_map.get(p.id.as_str()) {
            None => {
                items.push(p.clone());
            }
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After)
                    || matches!(relation, ClockOrder::Concurrent)
                {
                    items.push(p.clone());
                }
            }
        }
    }

    tracing::info!(
        "cc-history/sync/pull: 对端摘要 {} 条，本端 {} 条，返回 {} 条",
        req.summaries.len(),
        local_all.len(),
        items.len()
    );
    Ok(items)
}

/// POST /api/cc-history/sync/push：接收对端推送的 cc 历史，逐条合并后落库。
///
/// Business Logic: 对端把本端缺少/过时的 cc 历史推过来，本端对每条用 merger 决策后 bulk_upsert。
///
/// Code Logic:
///     1. 对每条 remote：查本地；本地没有 → 直接接收；本地有 → merge_cc_history 合并，
///        仅当合并结果与本地有差异时才写入；
///     2. bulk_upsert 实际需要写入的条目；
///     3. 返回 accepted = 实际落库条数。
pub async fn cc_sync_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<CcSyncPushReq>,
) -> P2pResult<Json<CcSyncPushResp>> {
    let accepted = cc_sync_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "cc_history.push"))?;
    Ok(Json(CcSyncPushResp { accepted }))
}

/// cc_sync_push 业务实现：逐条合并后落库，返回实际落库条数。
async fn cc_sync_push_impl(state: &AppState, req: CcSyncPushReq) -> Result<usize, AppError> {
    let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();

    for remote in req.items {
        let local = state.cc_history_repo.get(&remote.id).await?;
        match local {
            None => {
                to_upsert.push(remote);
            }
            Some(local_row) => {
                let merged = merge_cc_history(&local_row, &remote);
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.content != local_row.content
                    || merged.deleted != local_row.deleted
                {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let accepted = to_upsert.len();
    if !to_upsert.is_empty() {
        state.cc_history_repo.bulk_upsert(&to_upsert).await?;
    }

    tracing::info!("cc-history/sync/push: 接收并落库 {} 条 CC 历史", accepted);
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// Paged handlers
// ---------------------------------------------------------------------------

/// POST /api/cc-history/sync/manifest-page：分页返回同步摘要。
///
/// Business Logic（为什么需要这个函数）:
///     新客户端分页交换摘要，避免一次拉全量 id/clock 撑爆 body。
///
/// Code Logic（这个函数做什么）:
///     校验/解码 cursor 与 limit（默认 256、最大 512）；内部 limit+1 判定 done/next_cursor；
///     不写 ID 到日志。
pub async fn cc_sync_manifest_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<CcManifestPageReq>,
) -> P2pResult<Json<CcManifestPageResp>> {
    let resp = manifest_page_impl(state.cc_history_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "cc_history.manifest_page"))?;
    Ok(Json(resp))
}

/// manifest-page 业务实现（可单测，不依赖 AppState）。
///
/// Business Logic（为什么需要这个函数）:
///     把分页与 cursor 编解码从 axum 边界剥离，便于用内存 SQLite 契约测试。
///
/// Code Logic（这个函数做什么）:
///     解析 limit/cursor → list_sync_manifest_page(limit+1) → 裁剪 page 并编码 next_cursor。
async fn manifest_page_impl(
    repo: &ClaudeHistoryRepo,
    req: CcManifestPageReq,
) -> Result<CcManifestPageResp, RouteFail> {
    let limit = match req.limit {
        None => CC_MANIFEST_PAGE_LIMIT_DEFAULT,
        Some(0) => {
            return Err(RouteFail::Validation(
                "manifest limit 必须在 1..=512".to_string(),
            ));
        }
        Some(n) if n > CC_MANIFEST_PAGE_LIMIT_MAX => {
            return Err(RouteFail::Validation(format!(
                "manifest limit 最大 {CC_MANIFEST_PAGE_LIMIT_MAX}，收到 {n}"
            )));
        }
        Some(n) => n,
    };

    let after_id = match req.cursor.as_deref() {
        None | Some("") => None,
        Some(c) => Some(decode_manifest_cursor(c).map_err(|_| RouteFail::InvalidCursor)?),
    };

    // 优先 limit+1 判定 done；limit 已是 512 时仓储上限不允许 513，改为同页后再 peek 1 条。
    let (rows, done) = if limit < CC_MANIFEST_PAGE_LIMIT_MAX {
        let fetch_limit = limit + 1;
        let mut rows = repo
            .list_sync_manifest_page(after_id.as_deref(), fetch_limit)
            .await?;
        let done = (rows.len() as u32) <= limit;
        if !done {
            rows.truncate(limit as usize);
        }
        (rows, done)
    } else {
        let rows = repo
            .list_sync_manifest_page(after_id.as_deref(), limit)
            .await?;
        let done = if (rows.len() as u32) < limit {
            true
        } else if let Some(last) = rows.last() {
            let peek = repo
                .list_sync_manifest_page(Some(last.id.as_str()), 1)
                .await?;
            peek.is_empty()
        } else {
            true
        };
        (rows, done)
    };
    let next_cursor = if done {
        None
    } else {
        rows.last().map(|s| encode_manifest_cursor(&s.id))
    };

    tracing::info!(
        "cc-history/sync/manifest-page: limit={} page={} done={}",
        limit,
        rows.len(),
        done
    );

    Ok(CcManifestPageResp {
        summaries: rows,
        next_cursor,
        done,
    })
}

/// POST /api/cc-history/sync/items：按请求 ID 顺序返回完整行与 missing_ids。
///
/// Business Logic（为什么需要这个函数）:
///     客户端比较 manifest 后只拉需要的正文；保序便于对端对齐请求列表。
///
/// Code Logic（这个函数做什么）:
///     校验 ids（≤128、无空白/重复/超长）→ get_many_for_sync → 按请求序组装 items/missing；
///     响应估算超 8MiB → batch_too_large；单条超限 → item_too_large。
pub async fn cc_sync_items(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<CcItemsReq>,
) -> P2pResult<Json<CcItemsResp>> {
    let resp = items_impl(state.cc_history_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "cc_history.items"))?;
    Ok(Json(resp))
}

/// items 业务实现。
///
/// Business Logic（为什么需要这个函数）:
///     与 handler 分离以便契约测试覆盖顺序、missing、批/条大小拒绝。
///
/// Code Logic（这个函数做什么）:
///     validate_id_list → get_many_for_sync → 保序收集 + 估算响应字节。
async fn items_impl(repo: &ClaudeHistoryRepo, req: CcItemsReq) -> Result<CcItemsResp, RouteFail> {
    validate_id_list(&req.ids)?;

    let map = repo.get_many_for_sync(&req.ids).await?;

    let mut items = Vec::new();
    let mut missing_ids = Vec::new();
    let mut estimated: usize = 0;

    for id in &req.ids {
        match map.get(id) {
            Some(row) => {
                // 单条 content/估算超限：客户端不得静默跳过。
                if row.content.len() > CC_CONTENT_MAX_BYTES {
                    return Err(RouteFail::ItemTooLarge(format!(
                        "本地条目 content 超过 {CC_CONTENT_MAX_BYTES} 字节"
                    )));
                }
                let est = estimate_row_bytes(row);
                if est > CC_BATCH_MAX_ESTIMATED_BYTES {
                    return Err(RouteFail::ItemTooLarge(
                        "本地单条估算超过 8MiB 批上限".to_string(),
                    ));
                }
                estimated = estimated.saturating_add(est);
                if estimated > CC_BATCH_MAX_ESTIMATED_BYTES {
                    return Err(RouteFail::BatchTooLarge(format!(
                        "items 响应估算超过 {CC_BATCH_MAX_ESTIMATED_BYTES} 字节"
                    )));
                }
                items.push(row.clone());
            }
            None => missing_ids.push(id.clone()),
        }
    }

    tracing::info!(
        "cc-history/sync/items: requested={} found={} missing={}",
        req.ids.len(),
        items.len(),
        missing_ids.len()
    );

    Ok(CcItemsResp { items, missing_ids })
}

/// POST /api/cc-history/sync/push-batch：批量 merge + 事务 upsert。
///
/// Business Logic（为什么需要这个函数）:
///     对端把本地领先/并发/远端缺失的 rows 分批推送；服务端一次读本地、merge、事务写入，
///     任一行失败整批 rollback，accepted 为实际写入数。
///
/// Code Logic（这个函数做什么）:
///     校验 batch/id/content/估算 → get_many 本地 → merge_cc_history → upsert_merged_batch。
pub async fn cc_sync_push_batch(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<CcPushBatchReq>,
) -> P2pResult<Json<CcPushBatchResp>> {
    let accepted = push_batch_impl(state.cc_history_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "cc_history.push_batch"))?;
    Ok(Json(CcPushBatchResp { accepted }))
}

/// push-batch 业务实现。
///
/// Business Logic（为什么需要这个函数）:
///     保证批量读本地 + merge + 事务写入的契约可单测，且不逐条 N+1 get。
///
/// Code Logic（这个函数做什么）:
///     校验 → 汇总 ids 批量 get → merge → 仅变化行 upsert_merged_batch → 返回写入数。
async fn push_batch_impl(
    repo: &ClaudeHistoryRepo,
    req: CcPushBatchReq,
) -> Result<usize, RouteFail> {
    if req.items.len() > CC_ITEM_BATCH_LIMIT {
        return Err(RouteFail::BatchTooLarge(format!(
            "push-batch 最多 {CC_ITEM_BATCH_LIMIT} 条，收到 {}",
            req.items.len()
        )));
    }

    let mut estimated: usize = 0;
    let mut ids = Vec::with_capacity(req.items.len());
    let mut seen = HashSet::with_capacity(req.items.len());
    for item in &req.items {
        validate_row(item)?;
        if !seen.insert(item.id.as_str()) {
            return Err(RouteFail::Validation("items 含重复 id".to_string()));
        }
        estimated = estimated.saturating_add(estimate_row_bytes(item));
        if estimated > CC_BATCH_MAX_ESTIMATED_BYTES {
            return Err(RouteFail::BatchTooLarge(format!(
                "push-batch 估算超过 {CC_BATCH_MAX_ESTIMATED_BYTES} 字节"
            )));
        }
        ids.push(item.id.clone());
    }

    let local_map = repo.get_many_for_sync(&ids).await?;
    let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();
    for remote in req.items {
        match local_map.get(&remote.id) {
            None => to_upsert.push(remote),
            Some(local_row) => {
                let merged = merge_cc_history(local_row, &remote);
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.content != local_row.content
                    || merged.deleted != local_row.deleted
                {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let accepted = if to_upsert.is_empty() {
        0
    } else {
        repo.upsert_merged_batch(&to_upsert).await?
    };

    tracing::info!(
        "cc-history/sync/push-batch: received batch, accepted={}",
        accepted
    );
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::request_context::P2pRequestContext;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// 构造内存 SQLite 与 claude_history 表，返回仓库。
    async fn setup_repo() -> ClaudeHistoryRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS claude_history (\
             id TEXT PRIMARY KEY, project_path TEXT NOT NULL, project_name TEXT NOT NULL, \
             session_id TEXT NOT NULL, content TEXT NOT NULL, git_branch TEXT, cc_version TEXT, \
             occurred_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0, source TEXT NOT NULL DEFAULT 'claude')",
        )
        .execute(&pool)
        .await
        .unwrap();
        ClaudeHistoryRepo::new(pool)
    }

    /// 构造测试用 ClaudeHistoryRow。
    fn sample_row(id: &str, content: &str, vc: u64) -> ClaudeHistoryRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("d1".to_string(), vc);
        ClaudeHistoryRow {
            id: id.to_string(),
            project_path: "/p".to_string(),
            project_name: "p".to_string(),
            session_id: "s".to_string(),
            content: content.to_string(),
            git_branch: None,
            cc_version: None,
            occurred_at: "2024-01-01T00:00:00Z".to_string(),
            device_id: "d1".to_string(),
            vector_clock,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            deleted: false,
            source: crate::cc::models::SOURCE_CLAUDE.to_string(),
        }
    }

    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-test-1".to_string(),
        }
    }

    /// 断言 RouteFail 映射到精确 status/code/retryable。
    fn assert_fail(fail: RouteFail, status: StatusCode, code: &str, retryable: bool) {
        let p2p = fail.into_p2p(&ctx(), "cc_history.test");
        assert_eq!(p2p.status(), status);
        assert_eq!(p2p.envelope().code, code);
        assert_eq!(p2p.envelope().retryable, retryable);
        assert_eq!(p2p.envelope().request_id, "req-test-1");
    }

    /// cursor 往返：编码后再解码得到同一 last_id。
    #[test]
    fn cursor_roundtrip_v1() {
        let encoded = encode_manifest_cursor("id-42");
        let decoded = decode_manifest_cursor(&encoded).expect("decode");
        assert_eq!(decoded, "id-42");
        // 非法 base64 / JSON / version
        assert!(decode_manifest_cursor("!!!not-base64!!!").is_err());
        assert!(decode_manifest_cursor(&URL_SAFE_NO_PAD.encode(b"{}")).is_err());
        let bad_v = URL_SAFE_NO_PAD.encode(br#"{"v":2,"last_id":"x"}"#);
        assert!(decode_manifest_cursor(&bad_v).is_err());
    }

    /// invalid cursor → 400 + cc_history.invalid_cursor + retryable=false。
    #[tokio::test]
    async fn manifest_rejects_invalid_cursor_with_stable_error() {
        let repo = setup_repo().await;
        let err = manifest_page_impl(
            &repo,
            CcManifestPageReq {
                cursor: Some("not-a-cursor".into()),
                limit: Some(10),
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::BAD_REQUEST, CODE_INVALID_CURSOR, false);
    }

    /// 默认 limit=256、max=512；limit+1 内部判定 done/next_cursor。
    #[tokio::test]
    async fn manifest_default_and_max_page_and_cursor_advances() {
        let repo = setup_repo().await;
        let mut batch = Vec::new();
        for i in 0..300 {
            // 固定宽度 id，保证字典序 = 数值序
            batch.push(sample_row(&format!("id-{i:04}"), "c", 1));
        }
        repo.bulk_upsert(&batch).await.unwrap();

        // 默认 limit
        let page1 = manifest_page_impl(
            &repo,
            CcManifestPageReq {
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(page1.summaries.len(), 256);
        assert!(!page1.done);
        assert!(page1.next_cursor.is_some());

        // 第二页
        let page2 = manifest_page_impl(
            &repo,
            CcManifestPageReq {
                cursor: page1.next_cursor.clone(),
                limit: Some(CC_MANIFEST_PAGE_LIMIT_DEFAULT),
            },
        )
        .await
        .unwrap();
        assert_eq!(page2.summaries.len(), 44);
        assert!(page2.done);
        assert!(page2.next_cursor.is_none());

        // max 512
        let wide = manifest_page_impl(
            &repo,
            CcManifestPageReq {
                cursor: None,
                limit: Some(CC_MANIFEST_PAGE_LIMIT_MAX),
            },
        )
        .await
        .unwrap();
        assert_eq!(wide.summaries.len(), 300);
        assert!(wide.done);

        // limit 越界
        let bad = manifest_page_impl(
            &repo,
            CcManifestPageReq {
                cursor: None,
                limit: Some(513),
            },
        )
        .await
        .unwrap_err();
        match bad {
            RouteFail::Validation(_) => {}
            other => panic!("expected Validation, got domain via {other:?} path"),
        }
    }

    /// 129 IDs → 413 batch_too_large retryable=false。
    #[tokio::test]
    async fn items_rejects_129_ids_as_batch_too_large() {
        let repo = setup_repo().await;
        let ids: Vec<String> = (0..129).map(|i| format!("id-{i}")).collect();
        let err = items_impl(&repo, CcItemsReq { ids }).await.unwrap_err();
        assert_fail(
            err,
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_BATCH_TOO_LARGE,
            false,
        );
    }

    /// 空白 / 重复 / 257-byte ID 拒绝。
    #[tokio::test]
    async fn items_rejects_blank_duplicate_and_oversized_id() {
        let repo = setup_repo().await;

        let blank = items_impl(
            &repo,
            CcItemsReq {
                ids: vec!["   ".into()],
            },
        )
        .await
        .unwrap_err();
        match blank {
            RouteFail::Validation(m) => assert!(m.contains("空")),
            _ => panic!("blank id must be Validation"),
        }

        let dup = items_impl(
            &repo,
            CcItemsReq {
                ids: vec!["a".into(), "a".into()],
            },
        )
        .await
        .unwrap_err();
        match dup {
            RouteFail::Validation(m) => assert!(m.contains("重复")),
            _ => panic!("dup id must be Validation"),
        }

        let long = "x".repeat(CC_ID_MAX_BYTES + 1);
        let oversized = items_impl(&repo, CcItemsReq { ids: vec![long] })
            .await
            .unwrap_err();
        match oversized {
            RouteFail::Validation(m) => assert!(m.contains("256")),
            _ => panic!("257-byte id must be Validation"),
        }
    }

    /// 1MiB+1 content → 422 item_too_large。
    #[tokio::test]
    async fn push_rejects_item_content_over_1mib() {
        let repo = setup_repo().await;
        let mut row = sample_row("big", "", 1);
        row.content = "a".repeat(CC_CONTENT_MAX_BYTES + 1);
        let err = push_batch_impl(&repo, CcPushBatchReq { items: vec![row] })
            .await
            .unwrap_err();
        assert_fail(
            err,
            StatusCode::UNPROCESSABLE_ENTITY,
            CODE_ITEM_TOO_LARGE,
            false,
        );
    }

    /// 估算 8MiB+1 批 → 413 batch_too_large。
    #[tokio::test]
    async fn push_rejects_estimated_batch_over_8mib() {
        let repo = setup_repo().await;
        // 构造多条接近 1MiB 的内容，使总估算 > 8MiB 且条数 ≤128。
        let mut items = Vec::new();
        // 每条 content ~ 700KiB；12 条 ≈ 8.4MiB
        let chunk = "b".repeat(700 * 1024);
        for i in 0..12 {
            items.push(sample_row(&format!("bulk-{i}"), &chunk, 1));
        }
        let total: usize = items.iter().map(estimate_row_bytes).sum();
        assert!(
            total > CC_BATCH_MAX_ESTIMATED_BYTES,
            "fixture must exceed 8MiB estimate, got {total}"
        );
        let err = push_batch_impl(&repo, CcPushBatchReq { items })
            .await
            .unwrap_err();
        assert_fail(
            err,
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_BATCH_TOO_LARGE,
            false,
        );
    }

    /// items 保序 + missing_ids。
    #[tokio::test]
    async fn items_preserves_request_order_and_missing_ids() {
        let repo = setup_repo().await;
        repo.bulk_upsert(&[
            sample_row("b", "B", 1),
            sample_row("a", "A", 1),
            sample_row("c", "C", 1),
        ])
        .await
        .unwrap();

        let resp = items_impl(
            &repo,
            CcItemsReq {
                ids: vec![
                    "c".into(),
                    "missing-1".into(),
                    "a".into(),
                    "missing-2".into(),
                    "b".into(),
                ],
            },
        )
        .await
        .unwrap();

        let got_ids: Vec<&str> = resp.items.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got_ids, vec!["c", "a", "b"]);
        assert_eq!(
            resp.missing_ids,
            vec!["missing-1".to_string(), "missing-2".to_string()]
        );
    }

    /// 合法 128 条 push-batch 事务写入；重复 id 拒绝。
    #[tokio::test]
    async fn push_batch_accepts_128_and_merges() {
        let repo = setup_repo().await;
        // 预置一半本地旧版本
        let mut existing = Vec::new();
        for i in 0..64 {
            existing.push(sample_row(&format!("row-{i:03}"), "old", 1));
        }
        repo.bulk_upsert(&existing).await.unwrap();

        let mut items = Vec::new();
        for i in 0..128 {
            items.push(sample_row(&format!("row-{i:03}"), "new", 5));
        }
        let accepted = push_batch_impl(&repo, CcPushBatchReq { items })
            .await
            .unwrap();
        assert_eq!(accepted, 128);

        let check = repo
            .get_many_for_sync(&["row-000".into(), "row-127".into()])
            .await
            .unwrap();
        assert_eq!(check.get("row-000").unwrap().content, "new");
        assert_eq!(check.get("row-127").unwrap().content, "new");
        assert_eq!(
            *check
                .get("row-000")
                .unwrap()
                .vector_clock
                .get("d1")
                .unwrap(),
            5
        );
    }

    /// 稳定错误构造器契约：413/422/400 + retryable=false。
    #[test]
    fn stable_error_codes_contract() {
        assert_fail(
            RouteFail::BatchTooLarge("too big".into()),
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_BATCH_TOO_LARGE,
            false,
        );
        assert_fail(
            RouteFail::ItemTooLarge("one item".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            CODE_ITEM_TOO_LARGE,
            false,
        );
        assert_fail(
            RouteFail::InvalidCursor,
            StatusCode::BAD_REQUEST,
            CODE_INVALID_CURSOR,
            false,
        );
    }

    /// 本机宣告 paged-sync capability（与路由原子上线）。
    #[test]
    fn server_advertises_cc_history_paged_sync_capability() {
        use crate::net::protocol::{
            server_protocol_info, CAPABILITY_CC_HISTORY_PAGED_SYNC_V1, PROTOCOL_VERSION_V1,
        };
        let info = server_protocol_info();
        assert_eq!(info.protocol_version, PROTOCOL_VERSION_V1);
        assert!(
            info.supports(CAPABILITY_CC_HISTORY_PAGED_SYNC_V1),
            "must advertise cc-history.paged-sync.v1 with paged routes"
        );
    }
}
