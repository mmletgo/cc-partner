//! cc/merger.rs — Claude Code 历史 LWW 冲突合并
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备同步 Claude Code 历史时，同一条记录（同 id = session_id:uuid）可能因软删除
//!     在不同设备上产生并发版本（一端删除、一端仍保留）。需要一套冲突解决策略决定保留哪个版本，
//!     保证数据最终一致。策略与 `sync/merger.rs` 对齐：
//!     - 严格领先：直接覆盖（向量时钟判定）；
//!     - 并发冲突：LWW（Last-Writer-Wins），以 `updated_at` 时间戳决定胜出方；
//!     - 时间戳相等：用 `device_id` 字典序 tie-break（确定性，避免双端抖动）；
//!     - 无论谁胜出，最终都合并双方向量时钟以保留完整因果历史。
//!
//! Code Logic（这个模块做什么）:
//!     直接复用 `crate::sync::vector_clock::{compare, merge}`（不重复实现向量时钟）。
//!     `should_update_cc(local, remote)` 返回 bool：判断是否应用 remote 覆盖 local。
//!     `wins_concurrent_cc(local, remote)`：并发时的纯判定（含确定性 tie-break）。
//!     `merge_cc_history(local, remote)`：返回胜出方内容 + 合并后的向量时钟。

use crate::cc::models::ClaudeHistoryRow;
use crate::sync::vector_clock::{compare, merge, ClockOrder};

/// 判断是否应使用 remote 覆盖 local（Claude Code 历史版本）。
///
/// Business Logic: 同步时收到对端历史版本，需判断是否该用对端覆盖本地。逐分支：
///     - remote 严格领先（compare(remote, local) == After）→ true；
///     - local 严格领先（Before）→ false；
///     - 并发（Concurrent）→ remote.updated_at > local.updated_at 时 true；
///     - 完全相同（Equal）→ false。
///
/// Code Logic: 注意 compare 的方向是 `compare(remote, local)`：
///     - 返回 After 表示 remote 领先 → 应更新；
///     - 返回 Before 表示 remote 落后 → 不更新；
///     - Concurrent 时比较 updated_at 字符串（ISO8601 字典序与时间序一致）。
pub fn should_update_cc(local: &ClaudeHistoryRow, remote: &ClaudeHistoryRow) -> bool {
    let relation = compare(&remote.vector_clock, &local.vector_clock);
    match relation {
        ClockOrder::After => true,
        ClockOrder::Before => false,
        ClockOrder::Concurrent => remote.updated_at > local.updated_at,
        ClockOrder::Equal => false,
    }
}

/// 并发冲突时的纯判定：决定 local 与 remote 谁胜出（含确定性 tie-break）。
///
/// Business Logic: 当两版本并发（向量时钟互有领先）时用 LWW。时间戳相等时无法用 LWW 区分，
///     为保证双端确定性，用 device_id 字典序 tie-break：device_id 较大的版本胜出。
///
/// Code Logic: 返回 true 表示 remote 胜出，false 表示 local 胜出。
///     - updated_at 严格更大者胜；
///     - 相等时 device_id 字典序更大者胜（确定性）。
pub fn wins_concurrent_cc(local: &ClaudeHistoryRow, remote: &ClaudeHistoryRow) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    // 时间戳相等：device_id 字典序 tie-break（确定性，任一端独立计算结果一致）
    remote.device_id > local.device_id
}

/// 合并两条同 id 的 Claude Code 历史，返回最终版本（胜出方内容 + 合并后的向量时钟）。
///
/// Business Logic: 同步时需将本地与远端版本合并为一个最终版本，包含正确内容与完整因果历史。
///
/// Code Logic:
/// 1. 始终合并双方向量时钟（保留完整因果历史）；
/// 2. 决策胜出方：非并发时按向量时钟严格序（should_update_cc），并发时用 wins_concurrent_cc
///    （LWW + device_id tie-break）；
/// 3. 返回胜出方内容（克隆）+ 合并后的 vector_clock。
///
/// 注意：deleted 状态照常参与合并传播（删除是一次写入，deleted=true 的历史也参与同步）。
pub fn merge_cc_history(local: &ClaudeHistoryRow, remote: &ClaudeHistoryRow) -> ClaudeHistoryRow {
    let merged_clock = merge(&local.vector_clock, &remote.vector_clock);

    let relation = compare(&remote.vector_clock, &local.vector_clock);
    let remote_wins = match relation {
        ClockOrder::Concurrent => wins_concurrent_cc(local, remote),
        _ => should_update_cc(local, remote),
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

#[cfg(test)]
mod tests {
    //! merger 单测：覆盖严格领先取新、并发 LWW、时间戳相等 device_id tie-break、
    //! 向量时钟始终合并、deleted 参与传播。仿 sync/merger.rs 的单测风格。

    use super::*;
    use std::collections::HashMap;

    /// 构造测试用 ClaudeHistoryRow（仅填同步相关字段）。
    fn row(
        id: &str,
        device_id: &str,
        updated_at: &str,
        vc: &[(&str, u64)],
        deleted: bool,
    ) -> ClaudeHistoryRow {
        let vector_clock: HashMap<String, u64> =
            vc.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        ClaudeHistoryRow {
            id: id.to_string(),
            project_path: "/proj".to_string(),
            project_name: "proj".to_string(),
            session_id: "s1".to_string(),
            content: format!("content-{device_id}"),
            git_branch: None,
            cc_version: None,
            occurred_at: "2024-01-01T00:00:00+00:00".to_string(),
            device_id: device_id.to_string(),
            vector_clock,
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
            deleted,
        }
    }

    #[test]
    fn should_update_when_remote_strictly_after() {
        // remote 向量时钟严格领先 → should_update true
        let local = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h1", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        assert!(should_update_cc(&local, &remote));
    }

    #[test]
    fn should_not_update_when_local_strictly_after() {
        // local 向量时钟严格领先 → should_update false
        let local = row("h1", "d1", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("h1", "d2", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update_cc(&local, &remote));
    }

    #[test]
    fn should_not_update_when_equal() {
        // 向量时钟完全相同 → should_update false
        let local = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update_cc(&local, &remote));
    }

    #[test]
    fn concurrent_lww_picks_newer_timestamp() {
        // 并发：remote updated_at 更晚 → remote 胜
        let local = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("h1", "d2", "2024-01-03T00:00:00+00:00", &[("d2", 2)], false);
        let merged = merge_cc_history(&local, &remote);
        assert_eq!(merged.device_id, "d2");
        assert_eq!(merged.updated_at, "2024-01-03T00:00:00+00:00");
        // 向量时钟合并：逐 key 取 max
        assert_eq!(merged.vector_clock.get("d1"), Some(&2));
        assert_eq!(merged.vector_clock.get("d2"), Some(&2));

        // 反向传入也应得到一致胜出方（对称性）
        let merged2 = merge_cc_history(&remote, &local);
        assert_eq!(merged2.device_id, "d2");
    }

    #[test]
    fn concurrent_equal_timestamp_device_id_tiebreak() {
        // 并发且时间戳相等：device_id 字典序更大者胜（确定性）
        let local = row("h1", "aaa", "2024-01-01T00:00:00+00:00", &[("aaa", 1)], false);
        let remote = row("h1", "zzz", "2024-01-01T00:00:00+00:00", &[("zzz", 1)], false);
        let merged = merge_cc_history(&local, &remote);
        assert_eq!(merged.device_id, "zzz");

        // 反过来传入也应得到一致结果
        let merged2 = merge_cc_history(&remote, &local);
        assert_eq!(merged2.device_id, "zzz");
        assert_eq!(merged.vector_clock.get("aaa"), Some(&1));
        assert_eq!(merged.vector_clock.get("zzz"), Some(&1));
    }

    #[test]
    fn merge_always_combines_vector_clock() {
        // 无论谁胜出，向量时钟都应合并双方
        let local = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 3), ("d2", 1)], false);
        let remote = row("h1", "d2", "2024-01-01T00:00:00+00:00", &[("d1", 1), ("d2", 4)], false);
        let merged = merge_cc_history(&local, &remote);
        assert_eq!(merged.vector_clock.get("d1"), Some(&3));
        assert_eq!(merged.vector_clock.get("d2"), Some(&4));
    }

    #[test]
    fn deleted_history_participates_in_merge() {
        // deleted=true 的历史也参与同步传播，照常按向量时钟/LWW 合并
        let local = row("h1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("h1", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], true);
        let merged = merge_cc_history(&local, &remote);
        // remote 严格领先 → remote 胜，删除事件传播
        assert!(merged.deleted);
        assert_eq!(merged.device_id, "d2");
    }
}
