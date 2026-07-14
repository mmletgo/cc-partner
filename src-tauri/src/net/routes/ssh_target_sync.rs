//! net/routes/ssh_target_sync.rs — SSH 目标同步（legacy pull/push + v2 manifest/items/push-batch）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起 SSH 目标同步时调用这些端点。legacy 路径保留；v2 无状态分页与 Prompt 同构，
//!     主键为 host。`sync.manifest.v2` 由 Task 3 原子宣告。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/ssh-target/sync/pull|push：legacy。
//!     - POST /api/ssh-target/sync/manifest-page|items|push-batch：有界 typed 协议。

use crate::error::AppError;
use crate::models::ssh_target::SshTargetRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::storage::sync_request_ledger_repo::SyncRequestLedgerRepo;
use crate::storage::SshTargetRepo;
use crate::sync::apply_merge::apply_ssh_merge_batch;
use crate::sync::protocol::{
    content_sha256_hex, decode_keyset_cursor, encode_keyset_cursor, estimate_summary_wire_bytes,
    SyncManifestPage, SyncSummary, MANIFEST_PAGE_BYTES, MANIFEST_PAGE_ITEMS, PUSH_BATCH_BYTES,
    PUSH_BATCH_ITEMS,
};
use crate::sync::ssh_target::merge_ssh_target;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// SSH v2 路由 body 上限。
pub const SSH_SYNC_ROUTE_BODY_LIMIT_BYTES: usize = PUSH_BATCH_BYTES;
/// 稳定错误码。
pub const CODE_BATCH_TOO_LARGE: &str = "ssh_target.batch_too_large";
pub const CODE_ITEM_TOO_LARGE: &str = "ssh_target.item_too_large";
pub const CODE_INVALID_CURSOR: &str = "ssh_target.invalid_cursor";
const ID_MAX_BYTES: usize = 256;

// ---------------------------------------------------------------------------
// Legacy DTOs
// ---------------------------------------------------------------------------

/// ssh-target/sync/pull 请求体：对端发来的 SSH 目标摘要列表。
#[derive(Debug, Deserialize)]
pub struct SshSyncPullReq {
    #[serde(default)]
    pub summaries: Vec<SshSummary>,
}

/// 单条 SSH 目标摘要（host + 向量时钟）。
#[derive(Debug, Deserialize)]
pub struct SshSummary {
    pub host: String,
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// ssh-target/sync/pull 响应体：本端需下发给对端的完整 SSH 目标列表。
#[derive(Debug, Serialize)]
pub struct SshSyncPullResp {
    pub targets: Vec<SshTargetRow>,
}

/// ssh-target/sync/push 请求体：对端推送来的完整 SSH 目标列表。
#[derive(Debug, Deserialize)]
pub struct SshSyncPushReq {
    #[serde(default)]
    pub targets: Vec<SshTargetRow>,
}

/// ssh-target/sync/push 响应体：实际落库条数。
#[derive(Debug, Serialize)]
pub struct SshSyncPushResp {
    pub accepted: usize,
}

// ---------------------------------------------------------------------------
// V2 DTOs
// ---------------------------------------------------------------------------

/// manifest-page 请求。
#[derive(Debug, Deserialize)]
pub struct SshManifestPageReq {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// items 请求：ids 为 host 列表。
#[derive(Debug, Deserialize)]
pub struct SshItemsReq {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// items 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshItemsResp {
    pub items: Vec<SshTargetRow>,
    pub missing_ids: Vec<String>,
}

/// push-batch 请求。
#[derive(Debug, Deserialize)]
pub struct SshPushBatchReq {
    #[serde(default)]
    pub items: Vec<SshTargetRow>,
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
pub struct SshPushBatchResp {
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

/// SSH 目标正文指纹（username/port/label）。
fn ssh_content_hash(row: &SshTargetRow) -> String {
    let label = row.label.as_deref().unwrap_or("");
    let port = row.port.to_string();
    content_sha256_hex(&[
        row.username.as_bytes(),
        b"\0",
        port.as_bytes(),
        b"\0",
        label.as_bytes(),
    ])
}

/// 行 → SyncSummary（id = host）。
fn ssh_to_summary(row: &SshTargetRow) -> SyncSummary<String> {
    SyncSummary {
        id: row.host.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash: ssh_content_hash(row),
        size: (row.username.len() + row.label.as_ref().map(|s| s.len()).unwrap_or(0) + 8) as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
        delete_epoch: row.delete_epoch,
    }
}

/// 估算完整 SSH 行 wire 字节。
fn estimate_ssh_row_bytes(row: &SshTargetRow) -> usize {
    let vc_len = serde_json::to_string(&row.vector_clock)
        .map(|s| s.len())
        .unwrap_or(2);
    row.host.len()
        + row.username.len()
        + row.label.as_ref().map(|s| s.len()).unwrap_or(0)
        + row.device_id.len()
        + row.created_at.len()
        + row.updated_at.len()
        + vc_len
        + 64
}

fn validate_host_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("host 不能为空或空白".to_string());
    }
    if id.len() > ID_MAX_BYTES {
        return Err(format!(
            "host 超过 {ID_MAX_BYTES} UTF-8 字节上限（收到 {} 字节）",
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
        if let Err(msg) = validate_host_id(id) {
            return Err(RouteFail::Validation(msg));
        }
        if !seen.insert(id.as_str()) {
            return Err(RouteFail::Validation("ids 含重复 host".to_string()));
        }
    }
    Ok(())
}

fn validate_ssh_row(row: &SshTargetRow) -> Result<(), RouteFail> {
    if let Err(msg) = validate_host_id(&row.host) {
        return Err(RouteFail::Validation(msg));
    }
    let est = estimate_ssh_row_bytes(row);
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

/// POST /api/ssh-target/sync/pull：接收对端摘要，返回本端需下发的 SSH 目标。
pub async fn ssh_target_sync_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SshSyncPullReq>,
) -> P2pResult<Json<SshSyncPullResp>> {
    let targets = ssh_target_sync_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "ssh_target.pull"))?;
    Ok(Json(SshSyncPullResp { targets }))
}

/// ssh_target_sync_pull 业务实现：返回需要下发的 SSH 目标列表。
async fn ssh_target_sync_pull_impl(
    state: &AppState,
    req: SshSyncPullReq,
) -> Result<Vec<SshTargetRow>, AppError> {
    let remote_map: HashMap<&str, &HashMap<String, u64>> = req
        .summaries
        .iter()
        .map(|s| (s.host.as_str(), &s.vector_clock))
        .collect();

    let local_all = state.ssh_target_repo.get_all_for_sync().await?;

    let mut targets: Vec<SshTargetRow> = Vec::new();
    for p in &local_all {
        match remote_map.get(p.host.as_str()) {
            None => {
                targets.push(p.clone());
            }
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After)
                    || matches!(relation, ClockOrder::Concurrent)
                {
                    targets.push(p.clone());
                }
            }
        }
    }

    tracing::info!(
        "ssh-target/sync/pull: 对端摘要 {} 条，本端 {} 条，返回 {} 条",
        req.summaries.len(),
        local_all.len(),
        targets.len()
    );
    Ok(targets)
}

/// POST /api/ssh-target/sync/push：接收对端推送的 SSH 目标，逐条合并后落库。
pub async fn ssh_target_sync_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SshSyncPushReq>,
) -> P2pResult<Json<SshSyncPushResp>> {
    let accepted = ssh_target_sync_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "ssh_target.push"))?;
    Ok(Json(SshSyncPushResp { accepted }))
}

/// ssh_target_sync_push 业务实现：逐条合并后落库，返回实际落库条数。
async fn ssh_target_sync_push_impl(
    state: &AppState,
    req: SshSyncPushReq,
) -> Result<usize, AppError> {
    let mut to_upsert: Vec<SshTargetRow> = Vec::new();

    for remote in req.targets {
        let local = state.ssh_target_repo.get(&remote.host).await?;
        match local {
            None => {
                to_upsert.push(remote);
            }
            Some(local_row) => {
                let merged = merge_ssh_target(&local_row, &remote);
                if merged.vector_clock != local_row.vector_clock
                    || merged.updated_at != local_row.updated_at
                    || merged.username != local_row.username
                    || merged.port != local_row.port
                    || merged.label != local_row.label
                    || merged.deleted != local_row.deleted
                {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let accepted = to_upsert.len();
    if !to_upsert.is_empty() {
        state.ssh_target_repo.bulk_upsert(&to_upsert).await?;
    }

    tracing::info!("ssh-target/sync/push: 接收并落库 {} 条 SSH 目标", accepted);
    Ok(accepted)
}

// ---------------------------------------------------------------------------
// V2 handlers
// ---------------------------------------------------------------------------

/// POST /api/ssh-target/sync/manifest-page：无状态分页 SSH 摘要（id=host）。
///
/// Business Logic: client 拉完排序页后再与本地完整 manifest 比较。
/// Code Logic: keyset by host + page 预算。
pub async fn ssh_manifest_page(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SshManifestPageReq>,
) -> P2pResult<Json<SyncManifestPage<String>>> {
    let page = ssh_manifest_page_impl(state.ssh_target_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "ssh_target.manifest_page"))?;
    Ok(Json(page))
}

/// manifest-page 业务实现。
async fn ssh_manifest_page_impl(
    repo: &SshTargetRepo,
    req: SshManifestPageReq,
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
    local_all.sort_by(|a, b| a.host.cmp(&b.host));

    let start = match after_id.as_deref() {
        None => 0,
        Some(after) => local_all
            .iter()
            .position(|r| r.host.as_str() > after)
            .unwrap_or(local_all.len()),
    };
    let end = (start + limit as usize).min(local_all.len());
    let page_rows = &local_all[start..end];
    let has_more = end < local_all.len();

    let mut items: Vec<SyncSummary<String>> = Vec::with_capacity(page_rows.len());
    let mut estimated = 0usize;
    for row in page_rows {
        let summary = ssh_to_summary(row);
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
        "ssh-target/sync/manifest-page: limit={} page={} has_more={}",
        limit,
        items.len(),
        has_more
    );

    Ok(SyncManifestPage {
        items,
        next_cursor,
    })
}

/// POST /api/ssh-target/sync/items：按 host 批取正文。
pub async fn ssh_items(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SshItemsReq>,
) -> P2pResult<Json<SshItemsResp>> {
    let resp = ssh_items_impl(state.ssh_target_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "ssh_target.items"))?;
    Ok(Json(resp))
}

/// items 业务实现。
async fn ssh_items_impl(repo: &SshTargetRepo, req: SshItemsReq) -> Result<SshItemsResp, RouteFail> {
    validate_id_list(&req.ids)?;
    let mut items = Vec::new();
    let mut missing_ids = Vec::new();
    let mut estimated = 0usize;
    for id in &req.ids {
        match repo.get(id).await? {
            Some(row) => {
                let est = estimate_ssh_row_bytes(&row);
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
    Ok(SshItemsResp { items, missing_ids })
}

/// 计算 SSH push-batch 载荷指纹。
///
/// Business Logic: ledger 用稳定 payload hash 区分同 request_id 的不同正文。
/// Code Logic: 按 host 排序后拼接关键字段并 SHA-256。
fn ssh_batch_payload_hash(items: &[SshTargetRow]) -> String {
    let mut sorted: Vec<&SshTargetRow> = items.iter().collect();
    sorted.sort_by(|a, b| a.host.cmp(&b.host));
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for p in sorted {
        let vc = serde_json::to_string(&p.vector_clock).unwrap_or_else(|_| "{}".to_string());
        let label = p.label.as_deref().unwrap_or("");
        parts.push(p.host.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(p.port.to_string().into_bytes());
        parts.push(b"\0".to_vec());
        parts.push(p.username.as_bytes().to_vec());
        parts.push(b"\0".to_vec());
        parts.push(label.as_bytes().to_vec());
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

/// POST /api/ssh-target/sync/push-batch：批量 merge + 事务 bulk + ledger 幂等。
///
/// Business Logic: 同 Prompt push-batch——单事务 claim ledger、生产 bulk、记录 accepted。
/// Code Logic: 校验 → merge → apply_batch_idempotent(bulk_upsert_on_tx)。
pub async fn ssh_push_batch(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<SshPushBatchReq>,
) -> P2pResult<Json<SshPushBatchResp>> {
    let accepted = ssh_push_batch_impl(state.ssh_target_repo.as_ref(), req)
        .await
        .map_err(|e| e.into_p2p(&ctx, "ssh_target.push_batch"))?;
    Ok(Json(SshPushBatchResp { accepted }))
}

/// push-batch 业务实现。
async fn ssh_push_batch_impl(
    repo: &SshTargetRepo,
    req: SshPushBatchReq,
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
        validate_ssh_row(item)?;
        if !seen.insert(item.host.as_str()) {
            return Err(RouteFail::Validation("items 含重复 host".to_string()));
        }
        estimated = estimated.saturating_add(estimate_ssh_row_bytes(item));
        if estimated > PUSH_BATCH_BYTES {
            return Err(RouteFail::BatchTooLarge(format!(
                "push-batch 估算超过 {PUSH_BATCH_BYTES} 字节"
            )));
        }
    }

    let payload_hash = ssh_batch_payload_hash(&req.items);
    let claimed_device_id = req.claimed_device_id.trim().to_string();
    let acked_delete_epoch = req.acked_delete_epoch;
    let items = req.items;

    // N2: 单事务 apply_merge_batch（winner + conflict + delete_epoch + ledger + 可选 watermark ack）
    let outcome = apply_ssh_merge_batch(
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

    async fn setup_repo() -> SshTargetRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_targets (\
             host TEXT PRIMARY KEY, port INTEGER NOT NULL, username TEXT NOT NULL, \
             label TEXT, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0, delete_epoch INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        SyncRequestLedgerRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::content_version_repo::ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::deletion_floor_repo::DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::sync_watermark_repo::SyncWatermarkRepo::ensure_schema(&pool).await.unwrap();
        // delete_epoch 列
        let _ = sqlx::query(
            "ALTER TABLE ssh_targets ADD COLUMN delete_epoch INTEGER NOT NULL DEFAULT 0",
        )
        .execute(&pool)
        .await;
        SshTargetRepo::new(pool)
    }

    fn sample_row(host: &str, user: &str, vc: u64) -> SshTargetRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert("d1".to_string(), vc);
        SshTargetRow {
            host: host.to_string(),
            port: 22,
            username: user.to_string(),
            label: None,
            device_id: "d1".to_string(),
            vector_clock,
            created_at: "2026-07-14T00:00:00Z".to_string(),
            updated_at: "2026-07-14T00:00:00Z".to_string(),
            deleted: false,
            delete_epoch: 0,
        }
    }

    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-ssh-test".to_string(),
        }
    }

    fn assert_fail(fail: RouteFail, status: StatusCode, code: &str) {
        let p2p = fail.into_p2p(&ctx(), "ssh_target.test");
        assert_eq!(p2p.status(), status);
        assert_eq!(p2p.envelope().code, code);
    }

    #[tokio::test]
    async fn manifest_rejects_invalid_cursor() {
        let repo = setup_repo().await;
        let err = ssh_manifest_page_impl(
            &repo,
            SshManifestPageReq {
                cursor: Some("bad".into()),
                limit: Some(10),
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::BAD_REQUEST, CODE_INVALID_CURSOR);
    }

    #[tokio::test]
    async fn manifest_and_items_and_push_roundtrip() {
        let repo = setup_repo().await;
        let accepted = ssh_push_batch_impl(
            &repo,
            SshPushBatchReq {
                items: vec![
                    sample_row("10.0.0.1", "alice", 1),
                    sample_row("10.0.0.2", "bob", 1),
                ],
                client_request_id: "r1".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(accepted, 2);

        let page = ssh_manifest_page_impl(
            &repo,
            SshManifestPageReq {
                cursor: None,
                limit: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(page.next_cursor.is_none());
        assert_eq!(page.items[0].id, "10.0.0.1");

        let items = ssh_items_impl(
            &repo,
            SshItemsReq {
                ids: vec!["10.0.0.1".into(), "missing".into()],
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
        let items: Vec<SshTargetRow> = (0..=PUSH_BATCH_ITEMS)
            .map(|i| sample_row(&format!("h{i}"), "u", 1))
            .collect();
        let err = ssh_push_batch_impl(
            &repo,
            SshPushBatchReq {
                items,
                client_request_id: "r2".into(),
                claimed_device_id: "peer-1".into(),
                acked_delete_epoch: None,
            },
        )
        .await
        .unwrap_err();
        assert_fail(err, StatusCode::PAYLOAD_TOO_LARGE, CODE_BATCH_TOO_LARGE);
    }
}
