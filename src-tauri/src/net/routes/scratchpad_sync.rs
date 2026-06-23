//! net/routes/scratchpad_sync.rs — /api/scratchpad/sync/{pull,push} handler
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网设备间需要同步同一个速记本文本。由于 Scratchpad 是单例，协议只传一个
//!     vector_clock 或一条 ScratchpadRow，不引入多笔记列表语义。
//!
//! Code Logic（这个模块做什么）:
//!     - pull：对端发 `{vector_clock}`，本端根据向量时钟判断是否返回本端完整 scratchpad；
//!     - push：对端发 `{scratchpad}`，本端与本地单例 merge_scratchpad 后按需 upsert。

use crate::error::AppError;
use crate::models::scratchpad::ScratchpadRow;
use crate::state::AppState;
use crate::sync::scratchpad::{merge_scratchpad, scratchpad_changed};
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// scratchpad/sync/pull 请求体：对端当前速记本向量时钟。
#[derive(Debug, Deserialize)]
pub struct ScratchpadPullReq {
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// scratchpad/sync/pull 响应体：本端认为对端需要吸收的速记本版本。
#[derive(Debug, Serialize)]
pub struct ScratchpadPullResp {
    pub scratchpad: Option<ScratchpadRow>,
}

/// scratchpad/sync/push 请求体：对端推送的速记本版本。
#[derive(Debug, Deserialize)]
pub struct ScratchpadPushReq {
    pub scratchpad: ScratchpadRow,
}

/// scratchpad/sync/push 响应体：是否实际落库。
#[derive(Debug, Serialize)]
pub struct ScratchpadPushResp {
    pub accepted: bool,
}

/// POST /api/scratchpad/sync/pull：接收对端时钟，按需返回本端速记本版本。
///
/// Business Logic: 若本端版本领先或与对端并发，对端需要拿到完整文本再合并；本端落后/相等则无需返回。
/// Code Logic: get_or_init 保证本端存在单例；compare(local, remote_clock) 判断 After/Concurrent。
pub async fn scratchpad_pull(
    State(state): State<AppState>,
    Json(req): Json<ScratchpadPullReq>,
) -> Result<Json<ScratchpadPullResp>, AppError> {
    let local = state
        .scratchpad_repo
        .get_or_init(state.device_id.as_str())
        .await?;
    let relation = compare(&local.vector_clock, &req.vector_clock);
    let scratchpad =
        if matches!(relation, ClockOrder::After) || matches!(relation, ClockOrder::Concurrent) {
            Some(local)
        } else {
            None
        };
    Ok(Json(ScratchpadPullResp { scratchpad }))
}

/// POST /api/scratchpad/sync/push：接收对端版本，合并后按需落库。
///
/// Business Logic: 对端推送可能是领先、落后或并发版本；本端必须用同一套 LWW 策略合并，保证最终一致。
/// Code Logic: get_or_init 取本端单例，merge_scratchpad 后比较同步字段，有变化才 upsert。
pub async fn scratchpad_push(
    State(state): State<AppState>,
    Json(req): Json<ScratchpadPushReq>,
) -> Result<Json<ScratchpadPushResp>, AppError> {
    let local = state
        .scratchpad_repo
        .get_or_init(state.device_id.as_str())
        .await?;
    let merged = merge_scratchpad(&local, &req.scratchpad);
    let accepted = scratchpad_changed(&merged, &local);
    if accepted {
        state.scratchpad_repo.upsert(&merged).await?;
    }
    Ok(Json(ScratchpadPushResp { accepted }))
}
