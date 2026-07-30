//! agent_hub/replication/outbox_worker — LAN projection intent 可恢复排水器
//!
//! Business Logic（为什么需要这个模块）:
//!     commit 路由只 spawn best-effort 投影任务；进程在 commit 后、spawn 前崩溃时，
//!     发送端重试 prepare 得到 committed 后不再调用 commit，投影会永久缺失。
//!     owner 必须有启动 + 周期 durable worker claim 全部 queued intent。
//!
//! Code Logic（这个模块做什么）:
//!     `drain_lan_projection_intents`：claim queued intents → schedule_asset_projections →
//!     mark status=done；`start_lan_projection_outbox_loop` 启动 cancel-aware 周期循环。

use crate::error::AppError;
use crate::state::AppState;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 单次 claim 上限。
const CLAIM_LIMIT: i64 = 64;

/// 周期 tick 间隔。
const DRAIN_INTERVAL: Duration = Duration::from_secs(5);

/// 排水全部（有界批）queued LAN projection intent。
///
/// Business Logic（为什么需要这个函数）:
///     owner 启动与周期 worker、committed-prepare 补偿共用；投影 job 持久入队后才推进 intent。
///     部分 target 调度失败时不得 mark done（否则唯一 durable retry 记录被清）。
///
/// Code Logic（这个函数做什么）:
///     claim_queued(CAS) → schedule_asset_projections_report →
///     全部 enqueued/terminal_blocked 才 mark done；否则 requeue processing→queued。
///     禁止把 Ok(0) 当完整成功。
pub async fn drain_lan_projection_intents(state: &AppState) -> Result<u32, AppError> {
    let claimed = state
        .agent_hub_repo
        .claim_queued_lan_projection_intents(CLAIM_LIMIT)
        .await?;
    if claimed.is_empty() {
        return Ok(0);
    }
    let mut done = 0u32;
    for (transfer_id, asset_id) in claimed {
        match crate::agent_hub::projection_ops::schedule_asset_projections_report(state, &asset_id)
            .await
        {
            Ok(report) => {
                // 完整成功：所有 target 已入队或明确 terminal-blocked
                if report.complete {
                    if let Err(e) = state
                        .agent_hub_repo
                        .mark_lan_projection_intent_status(&transfer_id, &asset_id, "done")
                        .await
                    {
                        tracing::warn!(
                            transfer_id = %transfer_id,
                            asset_id = %asset_id,
                            error = %e,
                            "agent_hub lan projection intent mark done failed"
                        );
                        // mark 失败：requeue 以保留 durable retry
                        if let Err(mark_error) = state
                            .agent_hub_repo
                            .mark_lan_projection_intent_status(&transfer_id, &asset_id, "queued")
                            .await
                        {
                            tracing::warn!(transfer_id = %transfer_id, asset_id = %asset_id, error = %mark_error, "agent_hub lan projection intent requeue failed");
                        }
                    } else {
                        done = done.saturating_add(1);
                    }
                } else {
                    tracing::warn!(
                        transfer_id = %transfer_id,
                        asset_id = %asset_id,
                        enqueued = report.enqueued,
                        blocked = report.terminal_blocked,
                        failed = report.failed,
                        "agent_hub lan projection intent incomplete; requeue for retry"
                    );
                    if let Err(mark_error) = state
                        .agent_hub_repo
                        .mark_lan_projection_intent_status(&transfer_id, &asset_id, "queued")
                        .await
                    {
                        tracing::warn!(transfer_id = %transfer_id, asset_id = %asset_id, error = %mark_error, "agent_hub lan projection intent requeue failed");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    transfer_id = %transfer_id,
                    asset_id = %asset_id,
                    error = %e,
                    "agent_hub lan projection intent drain schedule failed; requeue for retry"
                );
                if let Err(mark_error) = state
                    .agent_hub_repo
                    .mark_lan_projection_intent_status(&transfer_id, &asset_id, "queued")
                    .await
                {
                    tracing::warn!(
                        transfer_id = %transfer_id,
                        asset_id = %asset_id,
                        error = %mark_error,
                        "agent_hub lan projection intent requeue failed"
                    );
                }
            }
        }
    }
    Ok(done)
}

/// 启动 owner 周期 LAN projection outbox worker。
///
/// Business Logic（为什么需要这个函数）:
///     仅依赖 commit endpoint spawn 无法覆盖崩溃窗口；Headless owner 必须周期排水。
///
/// Code Logic（这个函数做什么）:
///     立即 drain 一次；随后每 DRAIN_INTERVAL 再 drain，直到 cancel。
pub fn start_lan_projection_outbox_loop(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let child = cancel.child_token();
    tauri::async_runtime::spawn(async move {
        // 启动即排水
        if let Err(e) = drain_lan_projection_intents(&state).await {
            tracing::warn!(error = %e, "agent_hub lan projection outbox startup drain failed");
        }
        loop {
            tokio::select! {
                _ = child.cancelled() => break,
                _ = tokio::time::sleep(DRAIN_INTERVAL) => {
                    if let Err(e) = drain_lan_projection_intents(&state).await {
                        tracing::warn!(error = %e, "agent_hub lan projection outbox tick drain failed");
                    }
                }
            }
        }
    });
    cancel
}

#[cfg(test)]
mod tests {
    use crate::storage::AgentHubRepo;

    /// Business Logic: claim 空表应返回 0，不 panic。
    /// Code Logic: 内存库 ensure_schema 后 drain。
    #[tokio::test]
    async fn drain_empty_returns_zero() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        // 无 AppState 时直接验证 claim API
        let repo = AgentHubRepo::new(pool);
        let claimed = repo.claim_queued_lan_projection_intents(10).await.unwrap();
        assert!(claimed.is_empty());
    }
}
