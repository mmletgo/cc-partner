//! cc/engine.rs — Claude Code 历史跨设备同步引擎
//!
//! Business Logic（为什么需要这个模块）:
//!     采集到的 Claude Code 历史需要在用户的各设备间同步（在 A 设备问的 prompt，B 设备也能查到）。
//!     复用现有向量时钟基础设施，但走独立同步链路（`/api/cc-history/sync/*`），与 prompts 同步
//!     解耦——cc 同步失败不影响 prompts 同步计数。由 `sync/engine.rs::sync_with_peer` 末尾调用。
//!
//! Code Logic（这个模块做什么）:
//! cc_sync_with_peer(state, device) 与 sync/engine.rs::sync_with_peer 同构：
//! 1. health 检查，不可达跳过；
//! 2. 本端全部 cc 历史（含 deleted），投影为 summaries {id, vector_clock}；
//! 3. Pull：cc_sync_pull 拿回对端需给的，逐条本地 get + merge_cc_history（仅变化才收集）→ bulk_upsert；
//! 4. Push：重新取全量算补集（本端有而对端 pull 未返回的 / 本端领先并发的）→ cc_sync_push。
//!
//! 全程失败仅 tracing::warn 不阻断。

use crate::cc::merger::merge_cc_history;
use crate::cc::models::ClaudeHistoryRow;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, ClockOrder};
use std::collections::{HashMap, HashSet};

/// 与单个对端执行 Claude Code 历史的双向同步。
///
/// Business Logic: 确保双方 cc 历史一致。失败仅 warn 不阻断（调用方 sync_with_peer
///     在 prompts 同步后追加调用本方法，cc 失败不影响 prompts 计数）。
///
/// Code Logic:
///     1. health 检查，不可达跳过；
///     2. 本端 summaries（全部 cc 历史含 deleted 的 {id, vector_clock}）；
///     3. Pull：cc_sync_pull 拿回对端需要给的；逐条查本地，本地无则直接接收，本地有则
///        merge_cc_history，仅当合并结果与本地有差异时收集；bulk_upsert；
///     4. Push：重新取本端全量，算补集（对端没有的 / 本端领先或并发的）→ cc_sync_push。
pub async fn cc_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();
    tracing::info!("开始与设备 {} 同步 CC 历史 ({})", device.name, base_url);

    // 1. 健康检查
    if !state.peer_client.health(&device.host, device.port).await {
        tracing::warn!("设备 {} 不可达，跳过 CC 历史同步", device.name);
        return Ok(());
    }

    // 2. 本端全部 cc 历史（含 deleted），投影为 summaries {id, vector_clock}
    let local_all = state
        .cc_history_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("读取本地 CC 历史失败: {e}"))?;
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "vector_clock": p.vector_clock }))
        .collect();

    // 3. Pull：发本端 summaries，拿回对端认为本端需要的 cc 历史
    let remote_items: Vec<ClaudeHistoryRow> = state
        .peer_client
        .cc_sync_pull(&base_url, summary_values)
        .await;

    let mut to_upsert: Vec<ClaudeHistoryRow> = Vec::new();
    for remote in &remote_items {
        let local_row = state
            .cc_history_repo
            .get(&remote.id)
            .await
            .map_err(|e| format!("查询本地 CC 历史 {} 失败: {e}", remote.id))?;
        match local_row {
            None => {
                // 本地没有 → 直接接收
                to_upsert.push(remote.clone());
            }
            Some(local_row) => {
                // 本地有 → 合并决策
                let merged = merge_cc_history(&local_row, remote);
                // 仅当合并结果与本地有差异时才落库
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

    if !to_upsert.is_empty() {
        let n = to_upsert.len();
        state
            .cc_history_repo
            .bulk_upsert(&to_upsert)
            .await
            .map_err(|e| format!("CC 历史 bulk_upsert 失败: {e}"))?;
        tracing::info!("从 {} 拉取并更新了 {} 条 CC 历史", device.name, n);
    }

    // 4. Push：本端有而对端 pull 未返回的（即对端可能没有 / 对端落后），推送给对端
    let remote_ids: HashSet<String> = remote_items.iter().map(|p| p.id.clone()).collect();
    let remote_clock_map: HashMap<String, &HashMap<String, u64>> = remote_items
        .iter()
        .map(|p| (p.id.clone(), &p.vector_clock))
        .collect();

    // 重新取本端最新全量（pull 阶段可能已落库更新）
    let local_all_after = state
        .cc_history_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("重新读取本地 CC 历史失败: {e}"))?;

    let mut push_items: Vec<ClaudeHistoryRow> = Vec::new();
    for p in &local_all_after {
        match remote_clock_map.get(&p.id) {
            None => {
                // 对端没有 → 推送
                push_items.push(p.clone());
            }
            Some(remote_clock) => {
                // 本端 vs 对端：本端领先或并发 → 推送（对端会做 LWW 合并）
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After)
                    || matches!(relation, ClockOrder::Concurrent)
                {
                    // 仅当不在 remote_ids（避免重复推送 pull 已带走的）时推送
                    if !remote_ids.contains(&p.id) {
                        push_items.push(p.clone());
                    }
                }
            }
        }
    }

    if !push_items.is_empty() {
        let n = push_items.len();
        let success = state.peer_client.cc_sync_push(&base_url, &push_items).await;
        if success {
            tracing::info!("向 {} 推送了 {} 条 CC 历史", device.name, n);
        } else {
            tracing::warn!("向 {} 推送 CC 历史失败", device.name);
        }
    }

    tracing::info!("与设备 {} CC 历史同步完成", device.name);
    Ok(())
}
