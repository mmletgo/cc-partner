//! sync/scratchpad.rs — 速记本单例同步合并与 P2P 流程
//!
//! Business Logic（为什么需要这个模块）:
//!     Scratchpad 是单个自动保存文本，但仍可能在多设备并发编辑。需要与 Prompt/SSH 一致的
//!     向量时钟 + LWW 策略，保证局域网和 GitHub 同步最终收敛。
//!
//! Code Logic（这个模块做什么）:
//!     `merge_scratchpad` 复用 vector_clock compare/merge；`scratchpad_sync_with_peer`
//!     复用全局 trigger_sync 的设备遍历，由每个对端执行 pull + merge + push。

use crate::models::scratchpad::ScratchpadRow;
use crate::state::AppState;
use crate::sync::vector_clock::{compare, merge, ClockOrder};

/// 判断两条速记本行是否在同步相关字段上不同。
///
/// Business Logic: 合并后只有真正改变本地内容/时钟/删除状态时才需要落库，减少无意义写入。
/// Code Logic: 比较向量时钟、更新时间、内容、device_id、deleted。
pub fn scratchpad_changed(merged: &ScratchpadRow, local: &ScratchpadRow) -> bool {
    merged.vector_clock != local.vector_clock
        || merged.updated_at != local.updated_at
        || merged.content != local.content
        || merged.device_id != local.device_id
        || merged.deleted != local.deleted
}

/// 判断 remote 是否应覆盖 local。
///
/// Business Logic: 严格领先的远端版本必须被本机吸收；落后或相等版本不覆盖。
/// Code Logic: compare(remote, local)；并发分支只按 updated_at 初判，tie-break 在 wins_concurrent 中处理。
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
///
/// Business Logic: 两台设备同时编辑同一速记本文本时，用 LWW 选择更新时间更晚者；
///     时间完全相同则用 device_id 字典序保证所有设备作出同一选择。
/// Code Logic: 先比较 updated_at 字符串（RFC3339 可字典序比较），相同再比较 device_id。
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
///
/// Business Logic: 保证局域网/GitHub 同步冲突处理与 Prompt/SSH 一致：严格领先覆盖，并发 LWW，
///     无论哪边胜出都合并双方向量时钟以保留因果历史。
/// Code Logic: compare(remote, local) 后决定 winner，最后把 merged_clock 写回 winner。
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

/// 与单个对端同步速记本单例。
///
/// Business Logic: 全局 `trigger_sync` 应自动纳入速记本。此函数在 prompt/cc/ssh 同步后执行，
///     对端不可达或旧版本无 scratchpad 路由时只记录告警，不影响主同步计数。
///
/// Code Logic:
///     1. health 检查；
///     2. 本端 get_or_init 得到单例，并把 vector_clock 发给对端 pull；
///     3. 若对端返回版本，merge 后按需 upsert；
///     4. 重新读取本端单例并 push 给对端，对端自行 merge/no-op。
pub async fn scratchpad_sync_with_peer(
    state: &AppState,
    device: &crate::models::device::Device,
) -> Result<(), String> {
    let base_url = device.base_url();
    tracing::info!("开始与设备 {} 同步速记本 ({})", device.name, base_url);

    if !state.peer_client.health(&device.host, device.port).await {
        tracing::warn!("设备 {} 不可达，跳过速记本同步", device.name);
        return Ok(());
    }

    let local = state
        .scratchpad_repo
        .get_or_init(state.device_id.as_str())
        .await
        .map_err(|e| format!("读取本地速记本失败: {e}"))?;

    let remote = state
        .peer_client
        .scratchpad_pull(&base_url, &local.vector_clock)
        .await;

    if let Some(remote_row) = remote {
        let current = state
            .scratchpad_repo
            .get_or_init(state.device_id.as_str())
            .await
            .map_err(|e| format!("重新读取本地速记本失败: {e}"))?;
        let merged = merge_scratchpad(&current, &remote_row);
        if scratchpad_changed(&merged, &current) {
            state
                .scratchpad_repo
                .upsert(&merged)
                .await
                .map_err(|e| format!("速记本 upsert 失败: {e}"))?;
        }
    }

    let local_after = state
        .scratchpad_repo
        .get_or_init(state.device_id.as_str())
        .await
        .map_err(|e| format!("读取合并后速记本失败: {e}"))?;
    if !state
        .peer_client
        .scratchpad_push(&base_url, &local_after)
        .await
    {
        tracing::warn!("向 {} 推送速记本失败", device.name);
    }

    tracing::info!("与设备 {} 速记本同步完成", device.name);
    Ok(())
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
}
