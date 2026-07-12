//! net/routes/claude_md_sync.rs — /api/sync/claude_md/{pull,push} handler（供对端 P2P 推送 user 级 CLAUDE.md）
//!
//! Business Logic（为什么需要这个模块）:
//!     user 级 CLAUDE.md（~/.claude/CLAUDE.md）只在用户主动点击推送时传播。push 让触发设备
//!     把自己的 CLAUDE.md 推过来，本端必须覆盖为发送方版本；pull 仅保留兼容旧同步协议。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/sync/claude_md/pull：body `{vector_clock: {...}}`，比对后若本端领先/并发
//!       则返回本端 CLAUDE.md 完整 ClaudeMdRow，否则 None。
//!     - POST /api/sync/claude_md/push：body `{claude_md: ClaudeMdRow}`，覆盖落库 + 写文件，
//!       返回 `{accepted: bool}`。
//!     字段 snake_case（ClaudeMdRow 默认序列化），与 sync.rs 的 prompts 同步路由一致。

use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::{Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// claude_md/pull 请求体：对端发来的本端向量时钟（本端据此判断是否需要回传）。
#[derive(Debug, Deserialize)]
pub struct ClaudeMdPullReq {
    /// 调用方（对端）当前的 CLAUDE.md 向量时钟；缺省视作空时钟。
    #[serde(default)]
    pub vector_clock: HashMap<String, u64>,
}

/// claude_md/pull 响应体：本端需要下发给对端的 CLAUDE.md（None 表示本端无或无更新）。
#[derive(Debug, Serialize)]
pub struct ClaudeMdPullResp {
    /// 本端领先/并发时为 Some(local_row)，否则 None。
    pub claude_md: Option<ClaudeMdRow>,
}

/// claude_md/push 请求体：对端推送来的 CLAUDE.md 完整行。
#[derive(Debug, Deserialize)]
pub struct ClaudeMdPushReq {
    pub claude_md: ClaudeMdRow,
}

/// claude_md/push 响应体：是否实际接受落库（true=发送方版本与本地有差异并已写入）。
#[derive(Debug, Serialize)]
pub struct ClaudeMdPushResp {
    pub accepted: bool,
}

/// POST /api/sync/claude_md/pull：接收对端向量时钟，若本端领先/并发则回传本端 CLAUDE.md。
///
/// Business Logic: 对端把它的向量时钟发来，本端比对后决定是否下发本端版本。本端 None
///     （无记录）或本端不领先（Before/Equal）时不下发；本端 After/Concurrent 时下发。
///     与 sync::sync_pull 的语义一致，只是 CLAUDE.md 单例退化为 0/1 条。
///
/// Code Logic:
///     1. 读本端 claude_md 单例，None → 响应 claude_md:None；
///     2. compare(local.vc, remote.vc) 返回 local 相对 remote 关系，
///        After/Concurrent（本端领先/并发）→ 回 Some(local)，否则 None。
pub async fn claude_md_pull(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ClaudeMdPullReq>,
) -> P2pResult<Json<ClaudeMdPullResp>> {
    let claude_md = claude_md_pull_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "claude_md.pull"))?;
    Ok(Json(ClaudeMdPullResp { claude_md }))
}

/// claude_md_pull 业务实现：本端领先/并发时返回本端 CLAUDE.md。
async fn claude_md_pull_impl(
    state: &AppState,
    req: ClaudeMdPullReq,
) -> Result<Option<ClaudeMdRow>, AppError> {
    let local = state.claude_md_repo.get().await?;
    let claude_md = match local {
        None => None,
        Some(local_row) => {
            // compare(local, remote)：After=本端领先，Concurrent=并发 → 需下发
            let relation = compare(&local_row.vector_clock, &req.vector_clock);
            if matches!(relation, ClockOrder::After | ClockOrder::Concurrent) {
                Some(local_row)
            } else {
                None
            }
        }
    };
    Ok(claude_md)
}

/// POST /api/sync/claude_md/push：接收对端推送的 CLAUDE.md，覆盖落库 + 写文件。
///
/// Business Logic: CLAUDE.md 的用户主动推送语义是"接收端变成触发设备这份配置"，
///     因此不能按双向同步 merge，也不能让接收端本地版本因时间戳/向量时钟获胜。
///
/// Code Logic:
///     1. 读取本地行用于判断是否有差异；
///     2. 无论本地是否存在，都 upsert 发送方 row + write_file_if_changed；
///     3. accepted 表示本地同步字段是否发生变化。
pub async fn claude_md_push(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(req): Json<ClaudeMdPushReq>,
) -> P2pResult<Json<ClaudeMdPushResp>> {
    let accepted = claude_md_push_impl(&state, req)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "claude_md.push"))?;
    Ok(Json(ClaudeMdPushResp { accepted }))
}

/// claude_md_push 业务实现：覆盖落库 + 写文件，返回是否实际发生变化。
async fn claude_md_push_impl(state: &AppState, req: ClaudeMdPushReq) -> Result<bool, AppError> {
    let local = state.claude_md_repo.get().await?;
    // 用 `Option::map_or` 而非 `Option::is_none_or`（后者 1.82 才 stable），
    // 项目 MSRV 是 1.77.2，clippy 的 `-D warnings` 会阻断。
    let accepted = local.as_ref().map_or(true, |local_row| {
        local_row.content != req.claude_md.content
            || local_row.vector_clock != req.claude_md.vector_clock
            || local_row.updated_at != req.claude_md.updated_at
            || local_row.device_id != req.claude_md.device_id
    });
    state.claude_md_repo.upsert(&req.claude_md).await?;
    crate::sync::claude_md::write_file_if_changed(&req.claude_md.content).await?;
    Ok(accepted)
}
