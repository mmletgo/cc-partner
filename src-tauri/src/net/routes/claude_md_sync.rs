//! net/routes/claude_md_sync.rs — /api/sync/claude_md/{pull,push} handler（供对端 P2P 同步 user 级 CLAUDE.md）
//!
//! Business Logic（为什么需要这个模块）:
//!     user 级 CLAUDE.md（~/.claude/CLAUDE.md）需跨设备同步成全局记忆。对端设备发起同步时
//!     调用这两个端点：pull 让对端告知本端需要回传的本端 CLAUDE.md 版本；push 让对端把本端
//!     缺少/过时的 CLAUDE.md 推过来。与 prompts 同步路由（sync.rs）结构对称，只是 CLAUDE.md
//!     是单例记录（id 恒为 "claude_md"），故 pull 请求只发本端向量时钟、响应只回 0 或 1 条。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/sync/claude_md/pull：body `{vector_clock: {...}}`，比对后若本端领先/并发
//!       则返回本端 CLAUDE.md 完整 ClaudeMdRow，否则 None。
//!     - POST /api/sync/claude_md/push：body `{claude_md: ClaudeMdRow}`，merge_claude_md 决策后
//!       仅在合并结果与本地有差异时落库 + 写文件，返回 `{accepted: bool}`。
//!     字段 snake_case（ClaudeMdRow 默认序列化），与 sync.rs 的 prompts 同步路由一致。

use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::State;
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

/// claude_md/push 响应体：是否实际接受落库（true=合并后有变化已写入）。
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
    Json(req): Json<ClaudeMdPullReq>,
) -> Result<Json<ClaudeMdPullResp>, AppError> {
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
    Ok(Json(ClaudeMdPullResp { claude_md }))
}

/// POST /api/sync/claude_md/push：接收对端推送的 CLAUDE.md，合并决策后落库 + 写文件。
///
/// Business Logic: 对端把本端缺少/过时的 CLAUDE.md 推过来，本端 merge_claude_md 决策胜出方
///     并合并向量时钟，仅当合并结果与本地有差异（内容或时钟）时才落库 + 写文件，避免无意义覆盖。
///
/// Code Logic:
///     1. 本地 None → 直接接收 remote（upsert + 写文件），accepted:true；
///     2. 本地 Some → merge_claude_md，若 merged 与本地有差异（content/vector_clock）→ 落库 + 写文件，
///        accepted:true；否则 accepted:false。
pub async fn claude_md_push(
    State(state): State<AppState>,
    Json(req): Json<ClaudeMdPushReq>,
) -> Result<Json<ClaudeMdPushResp>, AppError> {
    let local = state.claude_md_repo.get().await?;
    let accepted = match local {
        None => {
            // 本地无记录 → 直接接收对端版本
            state.claude_md_repo.upsert(&req.claude_md).await?;
            crate::sync::claude_md::write_file_if_changed(&req.claude_md.content).await?;
            true
        }
        Some(local_row) => {
            // 合并决策胜出方 + 向量时钟
            let merged = crate::sync::claude_md::merge_claude_md(&local_row, &req.claude_md);
            if merged.content != local_row.content || merged.vector_clock != local_row.vector_clock
            {
                state.claude_md_repo.upsert(&merged).await?;
                crate::sync::claude_md::write_file_if_changed(&merged.content).await?;
                true
            } else {
                false
            }
        }
    };
    Ok(Json(ClaudeMdPushResp { accepted }))
}
