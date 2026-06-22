//! net/routes/sync.rs — /api/sync/{pull,push} handler（供对端 P2P 同步调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起同步时调用这两个端点：pull 让对端告知本端需要回传哪些 prompt；
//!     push 让对端把本端缺少/过时的 prompt 推过来。对照 Python `protocol.py` 的
//!     `handle_sync_pull` / `handle_sync_push`。字段命名与 Python 逐字一致
//!     （summaries / prompts / vector_clock / accepted），保证迁移期 Rust↔Python 互通。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/sync/pull：body `{summaries: [{id, vector_clock}]}`，比对后返回本端需要
//!       下发给对端的完整 PromptRow（本端有而对端没有 / 本端领先 / 并发的），返回 `{prompts: [...]}`。
//!     - POST /api/sync/push：body `{prompts: [PromptRow]}`，逐条用 merger 决策后 bulk_upsert，
//!       返回 `{accepted: <count>}`（accepted = 实际落库条数）。
//!     序列化用 snake_case（PromptRow 默认），与 Python `Prompt.to_dict()` 互通，非 camelCase。

use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::state::AppState;
use crate::sync::merger::merge_prompt;
use crate::sync::vector_clock::compare;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    Json(req): Json<SyncPullReq>,
) -> Result<Json<SyncPullResp>, AppError> {
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
    Ok(Json(SyncPullResp { prompts }))
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
    Json(req): Json<SyncPushReq>,
) -> Result<Json<SyncPushResp>, AppError> {
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
    Ok(Json(SyncPushResp { accepted }))
}
