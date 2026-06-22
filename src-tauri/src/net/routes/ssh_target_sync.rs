//! net/routes/ssh_target_sync.rs — /api/ssh-target/sync/{pull,push} handler（供对端 P2P 同步调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端设备发起 SSH 目标同步时调用这两个端点：pull 让对端告知本端需回传哪些 SSH 目标；
//!     push 让对端把本端缺少/过时的 SSH 目标推过来。与 cc-history/sync 同构但走独立链路，
//!     字段命名 snake_case（SshTargetRow 默认序列化）互通。
//!
//! Code Logic（这个模块做什么）:
//!     - POST /api/ssh-target/sync/pull：body `{summaries: [{host, vector_clock}]}`，返回本端需下发
//!       的完整 SshTargetRow（本端有而对端没有 / 本端领先 / 并发的），`{targets: [...]}`。
//!     - POST /api/ssh-target/sync/push：body `{targets: [SshTargetRow]}`，逐条 merge 后 bulk_upsert，`{accepted}`。

use crate::error::AppError;
use crate::models::ssh_target::SshTargetRow;
use crate::state::AppState;
use crate::sync::ssh_target::merge_ssh_target;
use crate::sync::vector_clock::{compare, ClockOrder};
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// POST /api/ssh-target/sync/pull：接收对端摘要，返回本端需下发的 SSH 目标。
pub async fn ssh_target_sync_pull(
    State(state): State<AppState>,
    Json(req): Json<SshSyncPullReq>,
) -> Result<Json<SshSyncPullResp>, AppError> {
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
                if matches!(relation, ClockOrder::After) || matches!(relation, ClockOrder::Concurrent)
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
    Ok(Json(SshSyncPullResp { targets }))
}

/// POST /api/ssh-target/sync/push：接收对端推送的 SSH 目标，逐条合并后落库。
pub async fn ssh_target_sync_push(
    State(state): State<AppState>,
    Json(req): Json<SshSyncPushReq>,
) -> Result<Json<SshSyncPushResp>, AppError> {
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
    Ok(Json(SshSyncPushResp { accepted }))
}
