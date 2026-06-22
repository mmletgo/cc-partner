//! net/routes/cc_history.rs — /api/cc-history/sync/{pull,push} handler（供对端 P2P 同步调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起 Claude Code 历史同步时调用这两个端点：pull 让对端告知本端需要回传哪些
//!     cc 历史；push 让对端把本端缺少/过时的 cc 历史推过来。与 `/api/sync/*` 同构但走独立链路，
//!     字段命名 snake_case（ClaudeHistoryRow 默认序列化），与对端 Rust 版互通。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/cc-history/sync/pull：body `{summaries: [{id, vector_clock}]}`，比对后返回本端
//!       需要下发给对端的完整 ClaudeHistoryRow（本端有而对端没有 / 本端领先 / 并发的），返回 `{items: [...]}`。
//!     - POST /api/cc-history/sync/push：body `{items: [ClaudeHistoryRow]}`，逐条用 merger 决策后
//!       bulk_upsert，返回 `{accepted: <count>}`。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::ClaudeHistoryRow;
use crate::error::AppError;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    Json(req): Json<CcSyncPullReq>,
) -> Result<Json<CcSyncPullResp>, AppError> {
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
                // 对端没有 → 下发
                items.push(p.clone());
            }
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After) || matches!(relation, ClockOrder::Concurrent)
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
    Ok(Json(CcSyncPullResp { items }))
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
    Json(req): Json<CcSyncPushReq>,
) -> Result<Json<CcSyncPushResp>, AppError> {
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
    Ok(Json(CcSyncPushResp { accepted }))
}
