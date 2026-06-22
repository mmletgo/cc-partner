//! sync/ssh_target.rs — SSH 目标 LWW 冲突合并
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备同步 SSH 目标时，同一 host 可能在不同设备上被并发编辑（如两端同时改了用户名）。
//!     需一套冲突解决策略保证最终一致。策略与 sync/merger.rs / cc/merger.rs 对齐：
//!     - 严格领先：直接覆盖（向量时钟判定）；
//!     - 并发：LWW，以 updated_at 时间戳决定胜出方；
//!     - 时间戳相等：device_id 字典序 tie-break（确定性）；
//!     - 无论谁胜出，最终都合并双方向量时钟以保留完整因果历史。
//!
//! Code Logic（这个模块做什么）:
//!     直接复用 crate::sync::vector_clock::{compare, merge}（不重复实现向量时钟）。

use crate::models::ssh_target::SshTargetRow;
use crate::sync::vector_clock::{compare, merge, ClockOrder};

/// 判断是否应使用 remote 覆盖 local（SSH 目标版本）。
pub fn should_update_ssh_target(local: &SshTargetRow, remote: &SshTargetRow) -> bool {
    let relation = compare(&remote.vector_clock, &local.vector_clock);
    match relation {
        ClockOrder::After => true,
        ClockOrder::Before => false,
        ClockOrder::Concurrent => remote.updated_at > local.updated_at,
        ClockOrder::Equal => false,
    }
}

/// 并发冲突时的纯判定：决定 local 与 remote 谁胜出（含确定性 tie-break）。返回 true 表示 remote 胜。
pub fn wins_concurrent_ssh(local: &SshTargetRow, remote: &SshTargetRow) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    remote.device_id > local.device_id
}

/// 合并两条同 host 的 SSH 目标，返回最终版本（胜出方内容 + 合并后的向量时钟）。
pub fn merge_ssh_target(local: &SshTargetRow, remote: &SshTargetRow) -> SshTargetRow {
    let merged_clock = merge(&local.vector_clock, &remote.vector_clock);

    let relation = compare(&remote.vector_clock, &local.vector_clock);
    let remote_wins = match relation {
        ClockOrder::Concurrent => wins_concurrent_ssh(local, remote),
        _ => should_update_ssh_target(local, remote),
    };

    if remote_wins {
        let mut winner = remote.clone();
        winner.vector_clock = merged_clock;
        winner
    } else {
        let mut winner = local.clone();
        winner.vector_clock = merged_clock;
        winner
    }
}

use crate::state::AppState;

/// 与单个对端执行 SSH 目标的双向同步。
///
/// Business Logic: 确保双方 SSH 目标配置一致。失败仅 warn 不阻断（由 sync/engine.rs::sync_with_peer
///     末尾追加调用，SSH 同步失败不影响 prompts 计数）。
///
/// Code Logic: 与 cc/engine.rs::cc_sync_with_peer 同构（host 取代 id）：
///     1. health 检查，不可达跳过；
///     2. 本端 summaries（全部含 deleted 的 {host, vector_clock}）；
///     3. Pull：ssh_target_pull 拿回对端需给的；逐条查本地，本地无则直接接收，本地有则
///        merge_ssh_target，仅当合并结果与本地有差异时收集；bulk_upsert；
///     4. Push：重新取本端全量，算补集（对端没有的 / 本端领先或并发的）→ ssh_target_push。
pub async fn ssh_target_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();
    tracing::info!("开始与设备 {} 同步 SSH 目标 ({})", device.name, base_url);

    // 1. 健康检查
    if !state.peer_client.health(&device.host, device.port).await {
        tracing::warn!("设备 {} 不可达，跳过 SSH 目标同步", device.name);
        return Ok(());
    }

    // 2. 本端全部 SSH 目标（含 deleted），投影为 summaries {host, vector_clock}
    let local_all = state
        .ssh_target_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("读取本地 SSH 目标失败: {e}"))?;
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "host": p.host, "vector_clock": p.vector_clock }))
        .collect();

    // 3. Pull：发本端 summaries，拿回对端认为本端需要的 SSH 目标
    let remote_items: Vec<SshTargetRow> = state
        .peer_client
        .ssh_target_pull(&base_url, summary_values)
        .await;

    let mut to_upsert: Vec<SshTargetRow> = Vec::new();
    for remote in &remote_items {
        let local_row = state
            .ssh_target_repo
            .get(&remote.host)
            .await
            .map_err(|e| format!("查询本地 SSH 目标 {} 失败: {e}", remote.host))?;
        match local_row {
            None => {
                to_upsert.push(remote.clone());
            }
            Some(local_row) => {
                let merged = merge_ssh_target(&local_row, remote);
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

    if !to_upsert.is_empty() {
        let n = to_upsert.len();
        state
            .ssh_target_repo
            .bulk_upsert(&to_upsert)
            .await
            .map_err(|e| format!("SSH 目标 bulk_upsert 失败: {e}"))?;
        tracing::info!("从 {} 拉取并更新了 {} 条 SSH 目标", device.name, n);
    }

    // 4. Push：本端有而对端 pull 未返回的，推送给对端
    let remote_hosts: std::collections::HashSet<String> =
        remote_items.iter().map(|p| p.host.clone()).collect();
    let remote_clock_map: std::collections::HashMap<String, &std::collections::HashMap<String, u64>> =
        remote_items
            .iter()
            .map(|p| (p.host.clone(), &p.vector_clock))
            .collect();

    let local_all_after = state
        .ssh_target_repo
        .get_all_for_sync()
        .await
        .map_err(|e| format!("重新读取本地 SSH 目标失败: {e}"))?;

    let mut push_items: Vec<SshTargetRow> = Vec::new();
    for p in &local_all_after {
        match remote_clock_map.get(&p.host) {
            None => {
                push_items.push(p.clone());
            }
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                // 本端领先或并发，且 pull 未返回（避免重复推送）→ 推送（对端会做 LWW 合并）
                if (matches!(relation, ClockOrder::After)
                    || matches!(relation, ClockOrder::Concurrent))
                    && !remote_hosts.contains(&p.host)
                {
                    push_items.push(p.clone());
                }
            }
        }
    }

    if !push_items.is_empty() {
        let n = push_items.len();
        let success = state.peer_client.ssh_target_push(&base_url, &push_items).await;
        if success {
            tracing::info!("向 {} 推送了 {} 条 SSH 目标", device.name, n);
        } else {
            tracing::warn!("向 {} 推送 SSH 目标失败", device.name);
        }
    }

    tracing::info!("与设备 {} SSH 目标同步完成", device.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! merger 单测：覆盖严格领先、并发 LWW、时间戳相等 device_id tie-break、
    //! 向量时钟始终合并、deleted 参与传播。仿 cc/merger.rs 的单测风格。

    use super::*;
    use std::collections::HashMap;

    /// 构造测试用 SshTargetRow（仅填同步相关字段）。
    fn row(
        host: &str,
        device_id: &str,
        updated_at: &str,
        vc: &[(&str, u64)],
        deleted: bool,
    ) -> SshTargetRow {
        let vector_clock: HashMap<String, u64> =
            vc.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        SshTargetRow {
            host: host.to_string(),
            port: 22,
            username: format!("user-{device_id}"),
            label: None,
            device_id: device_id.to_string(),
            vector_clock,
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
            deleted,
        }
    }

    #[test]
    fn should_update_when_remote_strictly_after() {
        let local = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        assert!(should_update_ssh_target(&local, &remote));
    }

    #[test]
    fn should_not_update_when_local_strictly_after() {
        let local = row("h", "d1", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("h", "d2", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update_ssh_target(&local, &remote));
    }

    #[test]
    fn should_not_update_when_equal() {
        let local = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update_ssh_target(&local, &remote));
    }

    #[test]
    fn concurrent_lww_picks_newer_timestamp() {
        let local = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("h", "d2", "2024-01-03T00:00:00+00:00", &[("d2", 2)], false);
        let merged = merge_ssh_target(&local, &remote);
        assert_eq!(merged.device_id, "d2");
        assert_eq!(merged.updated_at, "2024-01-03T00:00:00+00:00");
        assert_eq!(merged.vector_clock.get("d1"), Some(&2));
        assert_eq!(merged.vector_clock.get("d2"), Some(&2));
        // 对称性
        let merged2 = merge_ssh_target(&remote, &local);
        assert_eq!(merged2.device_id, "d2");
    }

    #[test]
    fn concurrent_equal_timestamp_device_id_tiebreak() {
        let local = row("h", "aaa", "2024-01-01T00:00:00+00:00", &[("aaa", 1)], false);
        let remote = row("h", "zzz", "2024-01-01T00:00:00+00:00", &[("zzz", 1)], false);
        let merged = merge_ssh_target(&local, &remote);
        assert_eq!(merged.device_id, "zzz");
        let merged2 = merge_ssh_target(&remote, &local);
        assert_eq!(merged2.device_id, "zzz");
    }

    #[test]
    fn merge_always_combines_vector_clock() {
        let local = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 3), ("d2", 1)], false);
        let remote = row("h", "d2", "2024-01-01T00:00:00+00:00", &[("d1", 1), ("d2", 4)], false);
        let merged = merge_ssh_target(&local, &remote);
        assert_eq!(merged.vector_clock.get("d1"), Some(&3));
        assert_eq!(merged.vector_clock.get("d2"), Some(&4));
    }

    #[test]
    fn deleted_target_participates_in_merge() {
        let local = row("h", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], true);
        let merged = merge_ssh_target(&local, &remote);
        assert!(merged.deleted);
        assert_eq!(merged.device_id, "d2");
    }
}
