//! sync/scratchpad.rs — 速记本多页面同步合并与 typed P2P 流程
//!
//! Business Logic（为什么需要这个模块）:
//!     Scratchpad 是多页面自动保存文本，需与 Prompt/SSH 一致的向量时钟 + LWW，
//!     并在 N2 下返回 typed domain outcome。
//!
//! Code Logic（这个模块做什么）:
//!     merge helpers + scratchpad_sync_with_peer（v2 plan / typed legacy）。

use crate::models::scratchpad::ScratchpadRow;
use crate::state::AppState;
use crate::sync::engine::{
    fetch_complete_remote_manifest, peer_error_to_domain_outcome,
};
use crate::sync::protocol::{
    compute_sync_plan, content_sha256_hex, SyncDomainOutcome, SyncSummary, PUSH_BATCH_ITEMS,
};
use crate::sync::vector_clock::{compare, merge, ClockOrder};
use std::collections::HashMap;
use uuid::Uuid;

/// 判断两条速记本行是否在同步相关字段上不同。
///
/// Business Logic: 合并后只有真正改变本地内容/时钟/删除状态时才需要落库。
/// Code Logic: 比较向量时钟、更新时间、内容、device_id、deleted。
pub fn scratchpad_changed(merged: &ScratchpadRow, local: &ScratchpadRow) -> bool {
    merged.vector_clock != local.vector_clock
        || merged.updated_at != local.updated_at
        || merged.title != local.title
        || merged.content != local.content
        || merged.device_id != local.device_id
        || merged.deleted != local.deleted
}

/// 判断 remote 是否应覆盖 local。
pub fn should_update_scratchpad(local: &ScratchpadRow, remote: &ScratchpadRow) -> bool {
    let relation = compare(&remote.vector_clock, &local.vector_clock);
    match relation {
        ClockOrder::After => true,
        ClockOrder::Before => false,
        ClockOrder::Concurrent => remote.updated_at > local.updated_at,
        ClockOrder::Equal => false,
    }
}

/// 并发冲突时决定 remote 是否胜出。
pub fn wins_concurrent_scratchpad(local: &ScratchpadRow, remote: &ScratchpadRow) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    remote.device_id > local.device_id
}

/// 合并两条速记本版本，返回胜出内容 + 合并后的向量时钟。
pub fn merge_scratchpad(local: &ScratchpadRow, remote: &ScratchpadRow) -> ScratchpadRow {
    let merged_clock = merge(&local.vector_clock, &remote.vector_clock);
    let relation = compare(&remote.vector_clock, &local.vector_clock);
    let remote_wins = match relation {
        ClockOrder::Concurrent => wins_concurrent_scratchpad(local, remote),
        _ => should_update_scratchpad(local, remote),
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

/// Scratchpad 行 → SyncSummary。
///
/// Business Logic: v2 planner 需要与服务端一致的 content_hash。
/// Code Logic: title/content 固定分隔后 SHA-256。
pub fn scratchpad_to_summary(row: &ScratchpadRow) -> SyncSummary<String> {
    let content_hash =
        content_sha256_hex(&[row.title.as_bytes(), b"\0", row.content.as_bytes()]);
    SyncSummary {
        id: row.id.clone(),
        vector_clock: row.vector_clock.clone(),
        content_hash,
        size: row.content.len() as u64,
        updated_at: row.updated_at.clone(),
        deleted: row.deleted,
    }
}

/// 与单个对端同步全部速记本页面，返回 typed domain outcome。
///
/// Business Logic: 失败不得伪装成功；支持 v2 时走 plan，否则 typed legacy。
/// Code Logic: supports_v2 分支；不重复 health。
pub async fn scratchpad_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
    supports_v2: bool,
) -> SyncDomainOutcome {
    if supports_v2 {
        scratchpad_sync_v2(state, device, base_url).await
    } else {
        scratchpad_sync_legacy_typed(state, device, base_url).await
    }
}

async fn scratchpad_sync_v2(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    let remote = match fetch_complete_remote_manifest(|cursor| {
        let client = state.peer_client.clone();
        let base = base_url.to_string();
        async move {
            client
                .list_scratchpad_manifest_page(&base, cursor.as_deref())
                .await
        }
    })
    .await
    {
        Ok(v) => v,
        Err(o) => return o,
    };

    let local_all = match state.scratchpad_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let mut local_manifest: Vec<SyncSummary<String>> =
        local_all.iter().map(scratchpad_to_summary).collect();
    local_manifest.sort_by(|a, b| a.id.cmp(&b.id));
    let local_by_id: HashMap<String, ScratchpadRow> =
        local_all.into_iter().map(|r| (r.id.clone(), r)).collect();

    let plan = compute_sync_plan(&local_manifest, &remote);
    let unchanged = plan.unchanged;
    let mut pulled: u32 = 0;
    let mut pushed: u32 = 0;

    for chunk in plan.fetch_from_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let ids: Vec<String> = chunk.to_vec();
        let resp = match state.peer_client.fetch_scratchpad_items(base_url, &ids).await {
            Ok(r) => r,
            Err(e) => return peer_error_to_domain_outcome(&e),
        };
        let mut to_upsert: Vec<ScratchpadRow> = Vec::new();
        for remote_row in resp.items {
            match local_by_id.get(&remote_row.id) {
                None => to_upsert.push(remote_row),
                Some(local_row) => {
                    let merged = merge_scratchpad(local_row, &remote_row);
                    if scratchpad_changed(&merged, local_row) {
                        to_upsert.push(merged);
                    }
                }
            }
        }
        if !to_upsert.is_empty() {
            let n = to_upsert.len() as u32;
            if let Err(e) = state.scratchpad_repo.bulk_upsert(&to_upsert).await {
                return SyncDomainOutcome::ProtocolError {
                    code: format!("bulk_upsert_failed:{e}"),
                };
            }
            pulled = pulled.saturating_add(n);
            tracing::info!("从 {} 拉取并更新了 {} 个速记本页面 (v2)", device.name, n);
        }
    }

    for chunk in plan.push_to_remote.chunks(PUSH_BATCH_ITEMS) {
        if chunk.is_empty() {
            continue;
        }
        let items: Vec<ScratchpadRow> = chunk
            .iter()
            .filter_map(|id| local_by_id.get(id).cloned())
            .collect();
        if items.is_empty() {
            continue;
        }
        let req_id = Uuid::new_v4().to_string();
        match state
            .peer_client
            .push_scratchpad_batch(base_url, &items, &req_id)
            .await
        {
            Ok(resp) => {
                pushed = pushed.saturating_add(resp.accepted as u32);
                tracing::info!(
                    "向 {} 推送了 {} 个速记本页面 (v2 accepted={})",
                    device.name,
                    items.len(),
                    resp.accepted
                );
            }
            Err(e) => return peer_error_to_domain_outcome(&e),
        }
    }

    SyncDomainOutcome::Succeeded {
        pulled,
        pushed,
        unchanged,
    }
}

async fn scratchpad_sync_legacy_typed(
    state: &AppState,
    device: &crate::models::device::Device,
    base_url: &str,
) -> SyncDomainOutcome {
    let local_all = match state.scratchpad_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_read_failed:{e}"),
            };
        }
    };
    let summaries: Vec<serde_json::Value> = local_all
        .iter()
        .map(|p| serde_json::json!({ "id": p.id, "vector_clock": p.vector_clock }))
        .collect();

    let remote_pages = match state
        .peer_client
        .scratchpad_pull_result(base_url, summaries)
        .await
    {
        Ok(v) => v,
        Err(e) => return peer_error_to_domain_outcome(&e),
    };

    let mut to_upsert: Vec<ScratchpadRow> = Vec::new();
    for remote in &remote_pages {
        let local = match state.scratchpad_repo.get(&remote.id).await {
            Ok(v) => v,
            Err(e) => {
                return SyncDomainOutcome::ProtocolError {
                    code: format!("local_get_failed:{e}"),
                };
            }
        };
        match local {
            None => to_upsert.push(remote.clone()),
            Some(local_row) => {
                let merged = merge_scratchpad(&local_row, remote);
                if scratchpad_changed(&merged, &local_row) {
                    to_upsert.push(merged);
                }
            }
        }
    }

    let mut pulled: u32 = 0;
    if !to_upsert.is_empty() {
        let n = to_upsert.len() as u32;
        if let Err(e) = state.scratchpad_repo.bulk_upsert(&to_upsert).await {
            return SyncDomainOutcome::ProtocolError {
                code: format!("bulk_upsert_failed:{e}"),
            };
        }
        pulled = n;
        tracing::info!("从 {} 拉取并更新了 {} 个速记本页面 (legacy)", device.name, n);
    }

    let remote_clock_map: HashMap<String, &HashMap<String, u64>> = remote_pages
        .iter()
        .map(|p| (p.id.clone(), &p.vector_clock))
        .collect();
    let local_after = match state.scratchpad_repo.get_all_for_sync().await {
        Ok(v) => v,
        Err(e) => {
            return SyncDomainOutcome::ProtocolError {
                code: format!("local_reread_failed:{e}"),
            };
        }
    };

    let mut push_pages: Vec<ScratchpadRow> = Vec::new();
    for page in &local_after {
        match remote_clock_map.get(&page.id).copied() {
            None => push_pages.push(page.clone()),
            Some(clock) => {
                let relation = compare(&page.vector_clock, clock);
                if matches!(relation, ClockOrder::After | ClockOrder::Concurrent) {
                    push_pages.push(page.clone());
                }
            }
        }
    }

    let mut pushed: u32 = 0;
    if !push_pages.is_empty() {
        let n = push_pages.len() as u32;
        match state
            .peer_client
            .scratchpad_push_result(base_url, &push_pages)
            .await
        {
            Ok(true) => {
                pushed = n;
                tracing::info!("向 {} 推送了 {} 个速记本页面 (legacy)", device.name, n);
            }
            Ok(false) => {
                return SyncDomainOutcome::ProtocolError {
                    code: "push_rejected".to_string(),
                };
            }
            Err(e) => return peer_error_to_domain_outcome(&e),
        }
    }

    let unchanged = local_after
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
    use super::*;
    use crate::models::scratchpad::ScratchpadRow;
    use std::collections::HashMap;

    /// 构造测试用 ScratchpadRow（仅填同步相关字段）。
    fn row(device_id: &str, updated_at: &str, vc: &[(&str, u64)], content: &str) -> ScratchpadRow {
        let vector_clock: HashMap<String, u64> =
            vc.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        ScratchpadRow {
            id: "scratchpad".to_string(),
            title: format!("title-{device_id}"),
            content: content.to_string(),
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
            device_id: device_id.to_string(),
            vector_clock,
            deleted: false,
        }
    }

    /// 远端向量时钟严格领先时，合并结果采用远端内容。
    #[test]
    fn merge_scratchpad_uses_remote_when_remote_clock_is_after() {
        let local = row("a", "2024-01-01T00:00:00+00:00", &[("a", 1)], "local");
        let remote = row(
            "b",
            "2024-01-02T00:00:00+00:00",
            &[("a", 1), ("b", 1)],
            "remote",
        );

        let merged = merge_scratchpad(&local, &remote);

        assert_eq!(merged.content, "remote");
        assert_eq!(merged.vector_clock.get("a"), Some(&1));
        assert_eq!(merged.vector_clock.get("b"), Some(&1));
    }

    /// 并发修改时，更新时间更晚的一端胜出。
    #[test]
    fn merge_scratchpad_resolves_concurrent_by_updated_at() {
        let local = row("a", "2024-01-01T00:00:00+00:00", &[("a", 2)], "local");
        let remote = row("b", "2024-01-03T00:00:00+00:00", &[("b", 2)], "remote");

        let merged = merge_scratchpad(&local, &remote);

        assert_eq!(merged.content, "remote");
        assert_eq!(merged.vector_clock.get("a"), Some(&2));
        assert_eq!(merged.vector_clock.get("b"), Some(&2));
    }

    /// 并发且时间戳相等时，用 device_id 字典序保证确定性。
    #[test]
    fn merge_scratchpad_uses_device_id_tiebreak_for_equal_timestamps() {
        let local = row("a", "2024-01-01T00:00:00+00:00", &[("a", 2)], "local");
        let remote = row("z", "2024-01-01T00:00:00+00:00", &[("z", 2)], "remote");

        let merged = merge_scratchpad(&local, &remote);

        assert_eq!(merged.content, "remote");
    }

    /// 标题变化也属于同步字段变化，避免远端重命名无法落库。
    #[test]
    fn scratchpad_changed_detects_title_changes() {
        let local = row("a", "2024-01-01T00:00:00+00:00", &[("a", 1)], "same");
        let mut remote = local.clone();
        remote.title = "renamed".to_string();

        assert!(scratchpad_changed(&remote, &local));
    }
}
