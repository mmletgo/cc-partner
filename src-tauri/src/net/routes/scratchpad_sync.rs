//! net/routes/scratchpad_sync.rs — /api/scratchpad/sync/{pull,push} handler
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网设备间需要同步多个速记本页面。路径沿用旧版本，协议升级为 summaries/pages，
//!     旧对端解析失败由 peer_client 兼容为跳过，不阻断其他同步。
//!
//! Code Logic（这个模块做什么）:
//!     - pull：对端发 `{summaries:[{id, vector_clock}]}`，本端返回对端缺少/本端领先/并发的 pages；
//!     - push：对端发 `{pages:[ScratchpadRow]}`，本端逐条 merge_scratchpad 后按需 bulk_upsert。

use crate::error::AppError;
use crate::models::scratchpad::ScratchpadRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::sync::scratchpad::{merge_scratchpad, scratchpad_changed};
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
async fn scratchpad_push_impl(
    state: &AppState,
    req: ScratchpadPushReq,
) -> Result<usize, AppError> {
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
