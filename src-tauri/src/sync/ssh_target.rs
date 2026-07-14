//! sync/ssh_target.rs — SSH 目标 LWW 冲突合并 + typed 双向同步
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备同步 SSH 目标时，同一 host 可能在不同设备上被并发编辑。
//!     需一套冲突解决策略保证最终一致，并在 N2 下返回 typed domain outcome。
//!
//! Code Logic（这个模块做什么）:
//!     merge helpers（含 conflict draft）+ ssh_target_sync_with_peer（v2 plan / typed legacy）。
use crate::models::ssh_target::SshTargetRow;
use crate::storage::content_version_repo::KIND_CONFLICT;
use crate::storage::sync_request_ledger_repo::DOMAIN_SSH_TARGET;
use crate::sync::merger::{ContentVersionDraft, DomainMergeResult};
use crate::sync::protocol::content_sha256_hex;
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

/// 计算 SSH 目标正文指纹（host/port/username/label）。
///
/// Business Logic（为什么需要这个函数）:
///     并发 conflict 行需要稳定 content_hash，与 manifest 语义对齐，供幂等去重与 UI 对比。
///
/// Code Logic（这个函数做什么）:
///     固定 `\0` 分隔 host/port/username/label 后 SHA-256 hex。
pub fn ssh_text_content_hash(row: &SshTargetRow) -> String {
    let label = row.label.as_deref().unwrap_or("");
    let port = row.port.to_string();
    content_sha256_hex(&[
        row.host.as_bytes(),
        b"\0",
        port.as_bytes(),
        b"\0",
        row.username.as_bytes(),
        b"\0",
        label.as_bytes(),
    ])
}

/// 判断两条 SSH 目标业务正文是否不同。
///
/// Business Logic: 仅 host/port/username/label 不同才写 conflict；纯时钟差异不保留副本。
/// Code Logic: 逐字段比较。
fn ssh_payload_differs(a: &SshTargetRow, b: &SshTargetRow) -> bool {
    a.host != b.host
        || a.port != b.port
        || a.username != b.username
        || a.label != b.label
}

/// 合并两条 SSH 目标，并在并发且正文不同时产出 conflict 副本草稿。
///
/// Business Logic（为什么需要这个函数）:
///     N2 要求并发不同 payload 时保留 winner 并写 conflict copy，避免 LWW 静默覆盖。
///
/// Code Logic（这个函数做什么）:
///     `merge_ssh_target` 得 winner；Concurrent 且 payload 不同时把 loser 做成 ContentVersionDraft；
///     domain=`DOMAIN_SSH_TARGET`，item_id=host。
pub fn merge_ssh_with_conflicts(
    local: &SshTargetRow,
    remote: &SshTargetRow,
    now: &str,
) -> DomainMergeResult<SshTargetRow> {
    let winner = merge_ssh_target(local, remote);
    let relation = compare(&remote.vector_clock, &local.vector_clock);
    let mut conflict_versions = Vec::new();
    if relation == ClockOrder::Concurrent && ssh_payload_differs(local, remote) {
        let remote_wins = wins_concurrent_ssh(local, remote);
        let loser = if remote_wins { local } else { remote };
        conflict_versions.push(ContentVersionDraft {
            domain: DOMAIN_SSH_TARGET.to_string(),
            item_id: loser.host.clone(),
            source_device: loser.device_id.clone(),
            content_hash: ssh_text_content_hash(loser),
            created_at: now.to_string(),
            kind: KIND_CONFLICT.to_string(),
            snapshot_json: serde_json::to_string(loser).unwrap_or_default(),
        });
    }
    DomainMergeResult {
        winner,
        conflict_versions,
    }
}

use crate::state::AppState;
use crate::sync::apply_merge::apply_ssh_pull_items;
use crate::sync::engine::{
    fetch_complete_remote_manifest, peer_error_to_domain_outcome,
};
use crate::sync::protocol::{
    compute_sync_plan, decide_acked_delete_epoch,
    max_delete_epoch_from_summaries, SyncDomainOutcome, SyncSummary, PUSH_BATCH_ITEMS,
};
use std::collections::HashMap;
use uuid::Uuid;

/// SSH 目标 LWW 冲突合并 helpers 保留在上方。
///
/// SSH 行 → SyncSummary（id=host）。
///
/// Business Logic: v2 planner 需要与服务端一致的 content_hash。
/// Code Logic: username/port/label 固定分隔后 SHA-256。
pub fn ssh_to_summary(row: &SshTargetRow) -> SyncSummary<String> {
    let label = row.label.as_deref().unwrap_or("");
    let port = row.port.to_string();
    let content_hash = content_sha256_hex(&[
        row.username.as_bytes(),
        b"\0",
        port.as_bytes(),
        b"\0",
        label.as_bytes(),
    ]);
    SyncSummary {
        id: row.host.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash,
        size: (row.username.len() + row.label.as_ref().map(|s| s.len()).unwrap_or(0) + 8) as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
        delete_epoch: row.delete_epoch,
    }
}

/// 与单个对端执行 SSH 目标双向同步，返回 typed domain outcome。
///
/// Business Logic: 失败不得伪装成功；支持 v2 时走 plan，否则 typed legacy。
/// Code Logic: supports_v2 分支；不重复 health（由 engine 注入 base_url/capability）。
pub async fn ssh_target_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
    supports_v2: bool,
) -> SyncDomainOutcome {
    if supports_v2 {
        ssh_sync_v2(state, device, base_url).await
    } else {
        ssh_sync_legacy_typed(state, device, base_url).await
    }
}

/// SSH v2 plan 路径。
async fn ssh_sync_v2(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    let remote = match fetch_complete_remote_manifest(|cursor| {
        let client = state.peer_client.clone();
        let base = base_url.to_string();
        async move {
            client
                .list_ssh_manifest_page(&base, cursor.as_deref())
                .await
        }
    })
    .await
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    let max_remote_epoch = max_delete_epoch_from_summaries(&remote);

    let local_all = match state.ssh_target_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let mut local_manifest: Vec<SyncSummary<String>> =
        local_all.iter().map(ssh_to_summary).collect();
    local_manifest.sort_by(|a, b| a.id.cmp(&b.id));
    let local_by_id: HashMap<String, SshTargetRow> =
        local_all.into_iter().map(|r| (r.host.clone(), r)).collect();

    let plan = compute_sync_plan(&local_manifest, &remote);
    let unchanged = plan.unchanged;
    let mut pulled: u32 = 0;
    let mut pushed: u32 = 0;

    for chunk in plan.fetch_from_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let ids: Vec<String> = chunk.to_vec();
        let resp = match state.peer_client.fetch_ssh_items(base_url, &ids).await {
            Ok(r) => r,
            Err(e) => return peer_error_to_domain_outcome(&e),
        };
        if !resp.items.is_empty() {
            match apply_ssh_pull_items(
                &state.ssh_target_repo.pool(),
                state.maintenance_gate.as_ref(),
                state.ssh_target_repo.as_ref(),
                &resp.items,
            )
            .await
            {
                Ok(n) => {
                    if n > 0 {
                        pulled = pulled.saturating_add(n as u32);
                        tracing::info!(
                            "从 {} 拉取并更新了 {} 条 SSH 目标 (v2 apply_merge)",
                            device.name,
                            n
                        );
                    }
                }
                Err(e) => {
                    return SyncDomainOutcome::ProtocolError {
                        code: format!("apply_merge_failed:{e}"),
                    };
                }
            }
        }
    }

    // 完整 manifest + apply 成功后，仅在末批/空 push 携带 acked_delete_epoch
    let ack_epoch = decide_acked_delete_epoch(true, true, max_remote_epoch);
    let claimed = state.device_id.as_str();

    let mut batches: Vec<Vec<SshTargetRow>> = Vec::new();
    for chunk in plan.push_to_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let items: Vec<SshTargetRow> = chunk
            .iter()
            .filter_map(|id| local_by_id.get(id).cloned())
            .collect();
        if !items.is_empty() {
            batches.push(items);
        }
    }

    if batches.is_empty() {
        let req_id = Uuid::new_v4().to_string();
        if let Err(e) = state
            .peer_client
            .push_ssh_batch(base_url, &[], &req_id, claimed, ack_epoch)
            .await
        {
            return peer_error_to_domain_outcome(&e);
        }
    } else {
        let last = batches.len() - 1;
        for (i, items) in batches.into_iter().enumerate() {
            let epoch = if i == last { ack_epoch } else { None };
            let req_id = Uuid::new_v4().to_string();
            match state
                .peer_client
                .push_ssh_batch(base_url, &items, &req_id, claimed, epoch)
                .await
            {
                Ok(resp) => {
                    pushed = pushed.saturating_add(resp.accepted as u32);
                    tracing::info!(
                        "向 {} 推送了 {} 条 SSH 目标 (v2 accepted={})",
                        device.name,
                        items.len(),
                        resp.accepted
                    );
                }
                Err(e) => return peer_error_to_domain_outcome(&e),
            }
        }
    }

    SyncDomainOutcome::Succeeded {
        pulled,
        pushed,
        unchanged,
    }
}

/// SSH typed legacy 路径。
async fn ssh_sync_legacy_typed(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    let local_all = match state.ssh_target_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let summary_values: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "host": p.host, "vector_clock": p.vector_clock }))
        .collect();

    let remote_items = match state
        .peer_client
        .ssh_target_pull_result(base_url, summary_values)
        .await
    {
        Ok(v) => v,
        Err(e) => return peer_error_to_domain_outcome(&e),
    };

    let mut to_upsert: Vec<SshTargetRow> = Vec::new();
    for remote in &remote_items {
        let local_row = match state.ssh_target_repo.get(&remote.host).await {
            Ok(v) => v,
            Err(e) => {
                return SyncDomainOutcome::ProtocolError {
                    code: format!("local_get_failed:{e}"),
                };
            }
        };
        match local_row {
            None => to_upsert.push(remote.clone()),
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

    let mut pulled: u32 = 0;
    if !to_upsert.is_empty() {
        let n = to_upsert.len() as u32;
        if let Err(e) = state.ssh_target_repo.bulk_upsert(&to_upsert).await {
            return SyncDomainOutcome::ProtocolError {
                code: format!("bulk_upsert_failed:{e}"),
            };
        }
        pulled = n;
        tracing::info!("从 {} 拉取并更新了 {} 条 SSH 目标 (legacy)", device.name, n);
    }

    let remote_hosts: std::collections::HashSet<String> =
        remote_items.iter().map(|p| p.host.clone()).collect();
    let remote_clock_map: HashMap<String, &HashMap<String, u64>> = remote_items
        .iter()
        .map(|p| (p.host.clone(), &p.vector_clock))
        .collect();

    let local_all_after = match state.ssh_target_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_reread_failed:{e}"),
            };
        }
    };

    let mut push_items: Vec<SshTargetRow> = Vec::new();
    for p in &local_all_after {
        match remote_clock_map.get(&p.host) {
            None => push_items.push(p.clone()),
            Some(remote_clock) => {
                let relation = compare(&p.vector_clock, remote_clock);
                if matches!(relation, ClockOrder::After | ClockOrder::Concurrent)
                    && !remote_hosts.contains(&p.host)
                {
                    push_items.push(p.clone());
                }
            }
        }
    }

    let mut pushed: u32 = 0;
    if !push_items.is_empty() {
        let n = push_items.len() as u32;
        match state
            .peer_client
            .ssh_target_push_result(base_url, &push_items)
            .await
        {
            Ok(true) => {
                pushed = n;
                tracing::info!("向 {} 推送了 {} 条 SSH 目标 (legacy)", device.name, n);
            }
            Ok(false) => {
                return SyncDomainOutcome::ProtocolError {
                    code: "push_rejected".to_string(),
                };
            }
            Err(e) => return peer_error_to_domain_outcome(&e),
        }
    }

    let unchanged = local_all_after
        .len()
        .saturating_sub(pulled as usize)
        .saturating_sub(pushed as usize) as u32;
    SyncDomainOutcome::Succeeded {
        pulled,
        pushed,
        unchanged,
    }
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
            delete_epoch: 0,
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
        let local = row(
            "h",
            "aaa",
            "2024-01-01T00:00:00+00:00",
            &[("aaa", 1)],
            false,
        );
        let remote = row(
            "h",
            "zzz",
            "2024-01-01T00:00:00+00:00",
            &[("zzz", 1)],
            false,
        );
        let merged = merge_ssh_target(&local, &remote);
        assert_eq!(merged.device_id, "zzz");
        let merged2 = merge_ssh_target(&remote, &local);
        assert_eq!(merged2.device_id, "zzz");
    }

    #[test]
    fn merge_always_combines_vector_clock() {
        let local = row(
            "h",
            "d1",
            "2024-01-01T00:00:00+00:00",
            &[("d1", 3), ("d2", 1)],
            false,
        );
        let remote = row(
            "h",
            "d2",
            "2024-01-01T00:00:00+00:00",
            &[("d1", 1), ("d2", 4)],
            false,
        );
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
