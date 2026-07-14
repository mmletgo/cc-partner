//! net/routes/sync.rs — Prompt 同步路由（legacy pull/push + v2 manifest/items/push-batch）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起 Prompt 同步时调用这些端点。legacy pull/push 保留一代兼容；
//!     v2 无状态 manifest-page/items/push-batch 供 typed peer 流式比较完整排序 manifest，
//!     网络/JSON/413 不得折叠为空成功。`sync.manifest.v2` capability 由 Task 3 原子宣告。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/sync/pull|push：legacy 摘要比对/合并路径（保持行为）。
//!     - POST /api/sync/prompts/manifest-page：按 id 升序 keyset 分页摘要。
//!     - POST /api/sync/prompts/items：按 id 批取正文。
//!     - POST /api/sync/prompts/push-batch：有界 batch 合并后 bulk_upsert + accepted。
//!     - 预算：page ≤500/1MiB；batch ≤100/4MiB（`sync::protocol` 常量）。

use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::sync_request_ledger_repo::SyncRequestLedgerRepo;
use crate::storage::PromptRepo;
use crate::sync::apply_merge::apply_prompt_merge_batch;
use crate::sync::merger::merge_prompt;
use crate::sync::protocol::{
    content_sha256_hex, decode_keyset_cursor, encode_keyset_cursor, estimate_summary_wire_bytes,
    SyncManifestPage, SyncSummary, MANIFEST_PAGE_BYTES, MANIFEST_PAGE_ITEMS, PUSH_BATCH_BYTES,
    PUSH_BATCH_ITEMS,
};
use crate::sync::vector_clock::compare;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// 常量与错误码
// ---------------------------------------------------------------------------

/// Prompt v2 路由 body 上限（与 push-batch 4MiB 对齐，略留 JSON 开销）。
pub const PROMPT_SYNC_ROUTE_BODY_LIMIT_BYTES: usize = PUSH_BATCH_BYTES;

/// 稳定错误码：批过大（HTTP 413，retryable=false）。
pub const CODE_BATCH_TOO_LARGE: &str = "prompts.batch_too_large";
/// 稳定错误码：单条过大（HTTP 422）。
pub const CODE_ITEM_TOO_LARGE: &str = "prompts.item_too_large";
/// 稳定错误码：非法 cursor（HTTP 400）。
pub const CODE_INVALID_CURSOR: &str = "prompts.invalid_cursor";
/// 单条 id UTF-8 字节上限。
const ID_MAX_BYTES: usize = 256;
/// 单条 content UTF-8 字节上限（1 MiB）。
const CONTENT_MAX_BYTES: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Legacy DTOs
// ---------------------------------------------------------------------------

/// sync/pull 请求体：对端发来的 prompt 摘要列表（字段对照 Python handler）。
#[derive(Debug, Deserialize)]
pub struct SyncPullReq {
    #[serde(default)]
    pub summaries: Vec<Summary>,
}

/// 单条 prompt 摘要（id + 向量时钟），对照 Python `{id, vector_clock}`。
#[derive(Debug, Deserialize)]
pub struct Summary {
    pub id: String,
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// sync/pull 响应体：本端需要下发给对端的完整 prompt 列表。
#[derive(Debug, Serialize)]
pub struct SyncPullResp {
    pub prompts: Vec<PromptRow>,
}

/// sync/push 请求体：对端推送来的完整 prompt 列表。
#[derive(Debug, Deserialize)]
pub struct SyncPushReq {
    #[serde(default)]
    pub prompts: Vec<PromptRow>,
}

/// sync/push 响应体：实际落库条数。
#[derive(Debug, Serialize)]
pub struct SyncPushResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// V2 DTOs
// ---------------------------------------------------------------------------

/// manifest-page 请求：可选 cursor + limit。
#[derive(Debug, Deserialize)]
pub struct PromptManifestPageReq {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// items 请求：按 ID 批量取正文。
#[derive(Debug, Deserialize)]
pub struct PromptItemsReq {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// items 响应：保序存在行 + 缺失 ID。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptItemsResp {
    pub items: Vec<PromptRow>,
    pub missing_ids: Vec<String>,
}

/// push-batch 请求：正文 + 稳定 client_request_id + claimed_device_id（ledger 键）。
#[derive(Debug, Deserialize)]
pub struct PromptPushBatchReq {
    #[serde(default)]
    pub items: Vec<PromptRow>,
    /// 客户端批请求幂等键（与 claimed_device_id/domain 组成 UNIQUE）。
    pub client_request_id: String,
    /// 收敛标签（**非**认证身份）；缺省空串，与 client_request_id 共同命名空间 ledger。
    #[serde(default)]
    pub claimed_device_id: String,
    /// 完整 manifest + 成功 apply 后客户端回传的最高连续 ackedDeleteEpoch；缺省 None 不推进水位。
    #[serde(default)]
    pub acked_delete_epoch: Option<u64>,
}

/// push-batch 响应：实际落库条数（**无** serde default，缺字段解析失败）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPushBatchResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// RouteFail
// ---------------------------------------------------------------------------

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
    /// Business Logic: 客户端依赖稳定 code 做拆批/终止；413/422 必须 retryable=false。
    /// Code Logic: InvalidCursor→400；BatchTooLarge→413；ItemTooLarge→422；Validation→400；App→from_app_error。
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

/// 计算 Prompt 正文指纹（title/content/tags）。
///
/// Business Logic: manifest exact equality 用 hash 判断是否需要交换正文。
/// Code Logic: 固定分隔符拼接字段后 SHA-256 hex。
fn prompt_content_hash(row: &PromptRow) -> String {
    let tags_json = serde_json::to_string(&row.tags).unwrap_or_else(|_| "[]".to_string());
    content_sha256_hex(&[
        row.title.as_bytes(),
        b"\0",
        row.content.as_bytes(),
        b"\0",
        tags_json.as_bytes(),
    ])
}

/// 从 PromptRow 构造 SyncSummary。
///
/// Business Logic: 摘要仅含元数据，供 client 完整流比较。
/// Code Logic: id/vector_clock/hash/size/updated_at/deleted。
fn prompt_to_summary(row: &PromptRow) -> SyncSummary<String> {
    SyncSummary {
        id: row.id.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash: prompt_content_hash(row),
        size: row.content.len() as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
        delete_epoch: row.delete_epoch,
    }
}

/// 估算完整 Prompt 行 wire 字节（batch 预算）。
///
/// Business Logic: 在序列化前拒绝超 4MiB 批。
/// Code Logic: 各字符串字段 + tags/vector_clock JSON 长度。
fn estimate_prompt_row_bytes(row: &PromptRow) -> usize {
    let tags_len = serde_json::to_string(&row.tags)
        .map(|s| s.len())
        .unwrap_or(2);
    let vc_len = serde_json::to_string(&row.vector_clock)
        .map(|s| s.len())
        .unwrap_or(2);
    row.id.len()
        + row.title.len()
        + row.content.len()
        + tags_len
        + row.created_at.len()
        + row.updated_at.len()
        + row.device_id.len()
        + vc_len
        + 64
}

/// 校验单个同步 ID。
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

/// 校验 ID 列表条数/空白/重复。
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

/// 校验完整 Prompt 行（id/content/估算）。
fn validate_prompt_row(row: &PromptRow) -> Result<(), RouteFail> {
    if let Err(msg) = validate_sync_id(&row.id) {
        return Err(RouteFail::Validation(msg));
    }
    if row.content.len() > CONTENT_MAX_BYTES {
        return Err(RouteFail::ItemTooLarge(format!(
            "content 超过 {CONTENT_MAX_BYTES} 字节上限（收到 {} 字节）",
            row.content.len()
        )));
    }
    let est = estimate_prompt_row_bytes(row);
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

/// POST /api/sync/pull：接收对端摘要，返回本端需要下发的 prompt。
///
/// Business Logic: 对端把它的 prompt 摘要发来，本端比对后返回"本端有而对端没有 / 本端领先 /
///     并发"的完整 prompt，供对端做合并。对照 Python `handle_sync_pull`。
///
/// Code Logic:
///     1. 取本端全部 prompt（get_all_for_sync，含 deleted）；
///     2. 构建对端摘要查找表 {id: vector_clock}；
///     3. 对本端每条：对端没有 → 下发；有则 compare(local, remote)，After/Concurrent → 下发；
///     4. 返回完整 PromptRow 列表（snake_case）。
pub async fn sync_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SyncPullReq>,
) -> P2pResult<Json<SyncPullResp>> {
    let prompts = sync_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "sync.pull"))?;
    Ok(Json(SyncPullResp { prompts }))
}

/// sync_pull 业务实现：返回需要下发的 prompt 列表（命令层错误保持 AppError 形态）。
async fn sync_pull_impl(state: &AppState, req: SyncPullReq) -> Result<Vec<PromptRow>, AppError> {
    // 对端摘要查找表
    let remote_map: HashMap<&str, &HashMap<String, u64>> = req
        .summaries
        .iter()
        .map(|s| (s.id.as_str(), &s.vector_clock))
        .collect();

    // 本端全部 prompt（含 deleted，删除事件需传播）
    let local_all = state.prompt_repo.get_all_for_sync().await?;

    // 筛选需要下发的 prompt
    let mut prompts: Vec<PromptRow> = Vec::new();
    for p in &local_all {
        match remote_map.get(p.id.as_str()) {
            None => {
                // 对端没有 → 下发
                prompts.push(p.clone());
            }
            Some(remote_clock) => {
                // 本端 vs 对端：After（本端领先）或 Concurrent（并发，交对端 LWW 合并）→ 下发
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, crate::sync::vector_clock::ClockOrder::After)
                    || matches!(relation, crate::sync::vector_clock::ClockOrder::Concurrent)
                {
                    prompts.push(p.clone());
                }
            }
        }
    }

    tracing::info!(
        "sync/pull: 对端摘要 {} 条，本端 {} 条，返回 {} 条 prompt",
        req.summaries.len(),
        local_all.len(),
        prompts.len()
    );
    Ok(prompts)
}

/// POST /api/sync/push：接收对端推送的 prompt，逐条合并后落库。
///
/// Business Logic: 对端把本端缺少/过时的 prompt 推过来，本端对每条用 merger 决策后 bulk_upsert。
///     对照 Python `handle_sync_push`（Python 直接 bulk_upsert 全部，依赖 push 前已由 pull 过滤；
///     Rust 端在此额外做 merger 决策，更稳健——即便对端误推已过时版本也不会覆盖本地较新版本，
///     且保证向量时钟因果历史完整传播）。
///
/// Code Logic:
///     1. 对每条 remote prompt：查本地；
///        - 本地没有 → 直接接收 remote；
///        - 本地有 → merge_prompt 合并（胜出方内容 + 合并后的向量时钟），仅当合并结果与本地
///          有差异时才写入（避免无意义覆盖）；
///     2. bulk_upsert 实际需要写入的条目；
///     3. 返回 accepted = 实际落库条数。
pub async fn sync_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SyncPushReq>,
) -> P2pResult<Json<SyncPushResp>> {
    let accepted = sync_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "sync.push"))?;
    Ok(Json(SyncPushResp { accepted }))
}

/// sync_push 业务实现：逐条合并后落库，返回实际落库条数（命令层错误保持 AppError 形态）。
async fn sync_push_impl(state: &AppState, req: SyncPushReq) -> Result<usize, AppError> {
    let mut to_upsert: Vec<PromptRow> = Vec::new();

    for remote in req.prompts {
        let local = state.prompt_repo.get(&remote.id).await?;
        match local {
            None => {
                // 本地没有 → 直接接收 remote
                to_upsert.push(remote);
            }
            Some(local_row) => {
                // 本地有 → 合并决策（merger 内部按向量时钟/LWW 判定胜出方并合并时钟）
                let merged = merge_prompt(&local_row, &remote);
                // 仅当合并结果与本地有差异时才落库（内容/时钟/deleted 任一变化）
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.content != local_row.content
                    || merged.title != local_row.title
                    || merged.deleted != local_row.deleted
                {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let accepted = to_upsert.len();
    if !to_upsert.is_empty() {
        state.prompt_repo.bulk_upsert(&to_upsert).await?;
    }

    tracing::info!("sync/push: 接收并落库 {} 条 prompt", accepted);
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// V2 handlers
// ---------------------------------------------------------------------------

/// POST /api/sync/prompts/manifest-page：无状态分页返回 Prompt 摘要。
///
/// Business Logic（为什么需要这个函数）:
///     client 流式拉完全部排序页后与本地完整 manifest 比较；server 不根据 caller 单页推断缺失。
///
/// Code Logic（这个函数做什么）:
///     解码 cursor → get_all_for_sync → 按 id 升序 keyset 切片 → 校验 page 预算 → SyncManifestPage。
pub async fn prompt_manifest_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<PromptManifestPageReq>,
) -> P2pResult<Json<SyncManifestPage<String>>> {
    let page = prompt_manifest_page_impl(state.prompt_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "prompts.manifest_page"))?;
    Ok(Json(page))
}

/// manifest-page 业务实现（可单测，不依赖 AppState）。
///
/// Business Logic: 把分页与 cursor 编解码从 axum 边界剥离。
/// Code Logic: limit 默认 MANIFEST_PAGE_ITEMS；按 id 过滤 after_id 后取 limit 条。
async fn prompt_manifest_page_impl(
    repo: &PromptRepo,
    req: PromptManifestPageReq,
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
        let summary = prompt_to_summary(row);
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

    tracing::info!(
        "sync/prompts/manifest-page: limit={} page={} has_more={}",
        limit,
        items.len(),
        has_more
    );

    Ok(SyncManifestPage {
        items,
        next_cursor,
    })
}

/// POST /api/sync/prompts/items：按请求 ID 顺序返回完整行与 missing_ids。
///
/// Business Logic: 客户端比较 manifest 后只拉需要的正文。
/// Code Logic: 校验 ids → 逐个 get → 保序组装 + 估算响应字节。
pub async fn prompt_items(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<PromptItemsReq>,
) -> P2pResult<Json<PromptItemsResp>> {
    let resp = prompt_items_impl(state.prompt_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "prompts.items"))?;
    Ok(Json(resp))
}

/// items 业务实现。
async fn prompt_items_impl(
    repo: &PromptRepo,
    req: PromptItemsReq,
) -> Result<PromptItemsResp, RouteFail> {
    validate_id_list(&req.ids)?;

    let mut items = Vec::new();
    let mut missing_ids = Vec::new();
    let mut estimated: usize = 0;

    for id in &req.ids {
        match repo.get(id).await? {
            Some(row) => {
                if row.content.len() > CONTENT_MAX_BYTES {
                    return Err(RouteFail::ItemTooLarge(format!(
                        "本地条目 content 超过 {CONTENT_MAX_BYTES} 字节"
                    )));
                }
                let est = estimate_prompt_row_bytes(&row);
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

    tracing::info!(
        "sync/prompts/items: requested={} found={} missing={}",
        req.ids.len(),
        items.len(),
        missing_ids.len()
    );

    Ok(PromptItemsResp { items, missing_ids })
}

/// POST /api/sync/prompts/push-batch：批量 merge + 事务 bulk + ledger 幂等。
///
/// Business Logic: 对端把本地领先/并发/远端缺失的 rows 分批推送；服务端在同一事务中
///     claim ledger、执行生产 bulk 循环、记录 exact accepted outcome。同 key/同 hash 重放
///     返回原 outcome 且不重复写入；同 key/不同 hash 返回 conflict。
///
/// Code Logic: 校验 → merge（读路径）→ payload hash → ledger.apply_batch_idempotent → accepted。
pub async fn prompt_push_batch(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<PromptPushBatchReq>,
) -> P2pResult<Json<PromptPushBatchResp>> {
    let accepted = prompt_push_batch_impl(state.prompt_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "prompts.push_batch"))?;
    Ok(Json(PromptPushBatchResp { accepted }))
}

/// 计算 Prompt push-batch 载荷指纹（稳定、与顺序无关）。
///
/// Business Logic: ledger 用 payload hash 区分同 request_id 的不同正文，防止错误重放覆盖。
/// Code Logic: 按 id 排序后逐条 hash id/title/content/tags/vc/deleted/updated_at。
fn prompt_batch_payload_hash(items: &[PromptRow]) -> String {
    let mut sorted: Vec<&PromptRow> = items.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for p in sorted {
        let tags = serde_json::to_string(&p.tags).unwrap_or_else(|_| "[]".to_string());
        let vc = serde_json::to_string(&p.vector_clock).unwrap_or_else(|_| "{}".to_string());
        parts.push(p.id.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(p.title.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(p.content.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(tags.into_bytes());
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

/// push-batch 业务实现。
async fn prompt_push_batch_impl(
    repo: &PromptRepo,
    req: PromptPushBatchReq,
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

    let mut estimated: usize = 0;
    let mut seen = HashSet::with_capacity(req.items.len());
    for item in &req.items {
        validate_prompt_row(item)?;
        if !seen.insert(item.id.as_str()) {
            return Err(RouteFail::Validation("items 含重复 id".to_string()));
        }
        estimated = estimated.saturating_add(estimate_prompt_row_bytes(item));
        if estimated > PUSH_BATCH_BYTES {
            return Err(RouteFail::BatchTooLarge(format!(
                "push-batch 估算超过 {PUSH_BATCH_BYTES} 字节"
            )));
        }
    }

    let payload_hash = prompt_batch_payload_hash(&req.items);
    let claimed_device_id = req.claimed_device_id.trim().to_string();
    let acked_delete_epoch = req.acked_delete_epoch;
    let items = req.items;

    // N2: 单事务 apply_merge_batch（winner + conflict + delete_epoch + ledger + 可选 watermark ack）
    let outcome = apply_prompt_merge_batch(
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

    tracing::info!(
        "sync/prompts/push-batch: client_request_id={} accepted={}",
        client_request_id,
        outcome.accepted
    );
    Ok(outcome.accepted)
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

    /// 构造内存 SQLite prompts 表 + ledger 表与仓库。
    async fn setup_repo() -> PromptRepo {
        use crate::storage::content_version_repo::ContentVersionRepo;
        use crate::storage::deletion_floor_repo::DeletionFloorRepo;
        use crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo;
        use crate::storage::sync_watermark_repo::SyncWatermarkRepo;
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompts (\
             id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, \
             tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
             device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        SyncRequestLedgerRepo::ensure_schema(&pool).await.unwrap();
        ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        PromptRepo::new(pool)
    }

    /// 构造测试 PromptRow。
    fn sample_row(id: &str, content: &str, vc: u64) -> PromptRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("d1".to_string(), vc);
        PromptRow {
            id: id.to_string(),
            title: format!("t-{id}"),
            content: content.to_string(),
            tags: vec![],
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
            request_id: "req-prompt-test".to_string(),
        }
    }

    fn assert_fail(fail: RouteFail, status: StatusCode, code: &str, retryable: bool) {
        let p2p = fail.into_p2p(&ctx(), "prompts.test");
        assert_eq!(p2p.status(), status);
        assert_eq!(p2p.envelope().code, code);
        assert_eq!(p2p.envelope().retryable, retryable);
    }

    #[test]
    fn cursor_roundtrip() {
        let encoded = encode_keyset_cursor("prompt-1");
        assert_eq!(decode_keyset_cursor(&encoded).unwrap(), "prompt-1");
        assert!(decode_keyset_cursor("!!!").is_err());
    }

    #[tokio::test]
    async fn manifest_rejects_invalid_cursor() {
        let repo = setup_repo().await;
        let err = prompt_manifest_page_impl(
            &repo,
            PromptManifestPageReq {
                cursor: Some("not-a-cursor".into()),
                limit: Some(10),
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::BAD_REQUEST, CODE_INVALID_CURSOR, false);
    }

    #[tokio::test]
    async fn manifest_pages_by_id_order_and_budget() {
        let repo = setup_repo().await;
        let mut batch = Vec::new();
        for i in 0..3 {
            batch.push(sample_row(&format!("id-{i:02}"), "c", 1));
        }
        repo.bulk_upsert(&batch).await.unwrap();

        let page1 = prompt_manifest_page_impl(
            &repo,
            PromptManifestPageReq {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
        assert_eq!(page1.items.len(), 2);
        assert_eq!(page1.items[0].id, "id-00");
        assert_eq!(page1.items[1].id, "id-01");
        assert!(page1.next_cursor.is_some());

        let page2 = prompt_manifest_page_impl(
            &repo,
            PromptManifestPageReq {
                cursor: page1.next_cursor,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
        assert_eq!(page2.items.len(), 1);
        assert_eq!(page2.items[0].id, "id-02");
        assert!(page2.next_cursor.is_none());
    }

    #[tokio::test]
    async fn items_rejects_over_batch_limit() {
        let repo = setup_repo().await;
        let ids: Vec<String> = (0..=PUSH_BATCH_ITEMS)
            .map(|i| format!("id-{i}"))
            .collect();
        let err = prompt_items_impl(&repo, PromptItemsReq { ids })
            .await
            .unwrap_err();
        assert_fail(
            err,
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_BATCH_TOO_LARGE,
            false,
        );
    }

    #[tokio::test]
    async fn items_returns_missing_and_found() {
        let repo = setup_repo().await;
        repo.bulk_upsert(&[sample_row("a", "body", 1)])
            .await
            .unwrap();
        let resp = prompt_items_impl(
            &repo,
            PromptItemsReq {
                ids: vec!["a".into(), "missing".into()],
            },
        )
        .await
        .unwrap();
        assert_eq!(resp.items.len(), 1);
        assert_eq!(resp.items[0].id, "a");
        assert_eq!(resp.missing_ids, vec!["missing".to_string()]);
    }

    #[tokio::test]
    async fn push_batch_rejects_empty_client_request_id() {
        let repo = setup_repo().await;
        let err = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![sample_row("a", "x", 1)],
                client_request_id: "  ".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        match err {
            RouteFail::Validation(m) => assert!(m.contains("client_request_id")),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_batch_accepts_new_items() {
        use crate::sync::apply_merge::{apply_fail_test_lock, clear_apply_merge_fail_point};
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let repo = setup_repo().await;
        let accepted = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![sample_row("a", "x", 1), sample_row("b", "y", 1)],
                client_request_id: "req-1".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(accepted, 2);
        assert!(repo.get("a").await.unwrap().is_some());
        assert!(repo.get("b").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn push_batch_rejects_too_many_items() {
        use crate::sync::apply_merge::{apply_fail_test_lock, clear_apply_merge_fail_point};
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let repo = setup_repo().await;
        let items: Vec<PromptRow> = (0..=PUSH_BATCH_ITEMS)
            .map(|i| sample_row(&format!("id-{i}"), "c", 1))
            .collect();
        let err = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items,
                client_request_id: "req-2".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        assert_fail(
            err,
            StatusCode::PAYLOAD_TOO_LARGE,
            CODE_BATCH_TOO_LARGE,
            false,
        );
    }

    /// 同 key/同 hash 重放返回原 outcome，且不重新 apply（删库后仍不回写）。
    #[tokio::test]
    async fn replayed_batch_returns_recorded_outcome() {
        use crate::sync::apply_merge::{apply_fail_test_lock, clear_apply_merge_fail_point};
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let repo = setup_repo().await;
        let req = PromptPushBatchReq {
            items: vec![sample_row("a", "x", 1), sample_row("b", "y", 1)],
            client_request_id: "req-replay".into(),
            claimed_device_id: "peer-1".into(),
            acked_delete_epoch: None,
        };
        let accepted1 = prompt_push_batch_impl(&repo, {
            PromptPushBatchReq {
                items: req.items.clone(),
                client_request_id: req.client_request_id.clone(),
                claimed_device_id: req.claimed_device_id.clone(),
                acked_delete_epoch: None,
            }
        })
        .await
        .unwrap();
        assert_eq!(accepted1, 2);

        // 人为删除已写入行，模拟“若重放会 re-apply 则会再出现”
        sqlx::query("DELETE FROM prompts")
            .execute(&repo.pool())
            .await
            .unwrap();
        assert!(repo.get_all_for_sync().await.unwrap().is_empty());

        let accepted2 = prompt_push_batch_impl(&repo, req).await.unwrap();
        assert_eq!(accepted2, accepted1);
        // 重放不得重新 apply：表仍为空
        assert!(
            repo.get_all_for_sync().await.unwrap().is_empty(),
            "replay must not re-apply bulk upsert"
        );
    }

    /// 同 key 不同 payload hash → conflict（409）。
    #[tokio::test]
    async fn same_key_different_hash_is_conflict() {
        use crate::sync::apply_merge::{apply_fail_test_lock, clear_apply_merge_fail_point};
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let repo = setup_repo().await;
        prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![sample_row("a", "x", 1)],
                client_request_id: "req-conflict".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap();

        let err = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![sample_row("a", "DIFFERENT", 2)],
                client_request_id: "req-conflict".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        let p2p = err.into_p2p(&ctx(), "prompts.push_batch");
        assert_eq!(p2p.status(), StatusCode::CONFLICT);
    }

    /// active 写后注入失败 → active/conflict/ledger 全回滚。
    #[tokio::test]
    async fn push_batch_fail_after_active_rolls_back() {
        use crate::storage::content_version_repo::ContentVersionRepo;
        use crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS;
        use crate::sync::apply_merge::{
            apply_fail_test_lock, arm_apply_merge_fail_point, ApplyMergeFailPoint,
        };
        let _lock = apply_fail_test_lock();
        let _fail = arm_apply_merge_fail_point(ApplyMergeFailPoint::AfterActiveRows);
        let repo = setup_repo().await;
        let err = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![sample_row("a", "x", 1)],
                client_request_id: "req-fail-active".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RouteFail::App(_)));
        assert!(repo.get("a").await.unwrap().is_none());
        let versions = ContentVersionRepo::new(repo.pool())
            .list_versions(DOMAIN_PROMPTS, "a")
            .await
            .unwrap();
        assert!(versions.is_empty());
        let gate = std::sync::Arc::new(crate::storage::DatabaseMaintenanceGate::new());
        let (_permit, mut tx) =
            crate::storage::begin_shared_write(repo.pool(), &gate)
                .await
                .unwrap();
        let row = SyncRequestLedgerRepo::get_on_tx(
            &mut tx,
            "peer-1",
            DOMAIN_PROMPTS,
            "req-fail-active",
        )
        .await
        .unwrap();
        assert!(row.is_none());
    }

    /// conflict 写后注入失败 → active 与 conflict 全回滚。
    #[tokio::test]
    async fn push_batch_fail_after_conflict_rolls_back() {
        use crate::storage::content_version_repo::ContentVersionRepo;
        use crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS;
        use crate::sync::apply_merge::{
            apply_fail_test_lock, arm_apply_merge_fail_point, ApplyMergeFailPoint,
        };
        let _lock = apply_fail_test_lock();
        let _fail = arm_apply_merge_fail_point(ApplyMergeFailPoint::AfterConflictOrMeta);
        let repo = setup_repo().await;
        let mut left = sample_row("a", "left-body", 1);
        left.device_id = "left".into();
        left.vector_clock = {
            let mut m = HashMap::new();
            m.insert("left".into(), 1);
            m
        };
        repo.bulk_upsert(&[left]).await.unwrap();
        let mut right = sample_row("a", "right-body", 1);
        right.device_id = "right".into();
        right.updated_at = "2026-07-15T00:00:00Z".into();
        right.vector_clock = {
            let mut m = HashMap::new();
            m.insert("right".into(), 1);
            m
        };
        let err = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![right],
                client_request_id: "req-fail-conflict".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, RouteFail::App(_)));
        let got = repo.get("a").await.unwrap().unwrap();
        assert_eq!(got.content, "left-body");
        let versions = ContentVersionRepo::new(repo.pool())
            .list_versions(DOMAIN_PROMPTS, "a")
            .await
            .unwrap();
        assert!(versions.is_empty());
    }

    /// 并发 push 产出 content_versions conflict 行。
    #[tokio::test]
    async fn push_batch_concurrent_writes_conflict_row() {
        use crate::storage::content_version_repo::ContentVersionRepo;
        use crate::storage::sync_request_ledger_repo::DOMAIN_PROMPTS;
        use crate::sync::apply_merge::{apply_fail_test_lock, clear_apply_merge_fail_point};
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let repo = setup_repo().await;
        let mut left = sample_row("a", "left-body", 1);
        left.device_id = "left".into();
        left.vector_clock = {
            let mut m = HashMap::new();
            m.insert("left".into(), 1);
            m
        };
        repo.bulk_upsert(&[left]).await.unwrap();
        let mut right = sample_row("a", "right-body", 1);
        right.device_id = "right".into();
        right.updated_at = "2026-07-15T00:00:00Z".into();
        right.vector_clock = {
            let mut m = HashMap::new();
            m.insert("right".into(), 1);
            m
        };
        let accepted = prompt_push_batch_impl(
            &repo,
            PromptPushBatchReq {
                items: vec![right],
                client_request_id: "req-conflict-ok".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(accepted, 1);
        let versions = ContentVersionRepo::new(repo.pool())
            .list_versions(DOMAIN_PROMPTS, "a")
            .await
            .unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].kind, "conflict");
    }
}
