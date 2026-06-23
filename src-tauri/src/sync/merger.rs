//! sync/merger.rs — LWW 冲突合并
//!
//! Business Logic（为什么需要这个模块）:
//!     多设备同步 Prompt 时，可能出现同一条 Prompt 在不同设备上被独立修改的情况。需要一套
//!     冲突解决策略来决定保留哪个版本，保证数据最终一致。对照 Python `sync/merger.py`：
//!     - 严格领先：直接覆盖（向量时钟判定）；
//!     - 并发冲突：使用 LWW（Last-Writer-Wins），以 `updated_at` 时间戳决定胜出方；
//!     - 时间戳相等：用 `device_id` 做 tie-break（确定性，对照任务要求；Python 端在相等时
//!       默认保留 local，此处提供确定性方向避免双端抖动）；
//!     - 无论谁胜出，最终都合并双方向量时钟以保留完整因果历史。
//!
//! Code Logic（这个模块做什么）:
//!     should_update(local, remote) 返回 bool：对照 Python PromptMerger.should_update，
//!         判断是否应用 remote 覆盖 local（向量时钟 compare + 并发 LWW）。
//!     merge_prompt(local, remote) 返回 PromptRow：对照 Python merge_prompt，返回
//!         胜出方内容 + 合并后的向量时钟。
//!     `wins_concurrent(local, remote)`：并发时的纯判定（含确定性 tie-break）。

use crate::models::prompt::PromptRow;
use crate::sync::vector_clock::{compare, merge, ClockOrder};

/// 判断是否应使用 remote 覆盖 local。
///
/// Business Logic: 同步时收到对端 Prompt 版本，需判断是否该用对端覆盖本地。对照 Python
///     `PromptMerger.should_update`，逐分支等价：
///     - remote 严格领先（compare(remote, local) == After）→ true；
///     - local 严格领先（Before）→ false；
///     - 并发（Concurrent）→ remote.updated_at > local.updated_at 时 true；
///     - 完全相同（Equal）→ false。
///
/// Code Logic: 注意 compare 的方向是 `compare(remote, local)`：
///     - 返回 After 表示 remote 领先 → 应更新；
///     - 返回 Before 表示 remote 落后 → 不更新；
///     - Concurrent 时比较 updated_at 字符串（ISO8601 字典序与时间序一致）。
pub fn should_update(local: &PromptRow, remote: &PromptRow) -> bool {
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
///     为保证双端确定性（Rust↔Rust、迁移期 Rust↔Python 都不抖动），用 device_id 字典序
///     做 tie-break：device_id 较大的版本胜出。
///
/// Code Logic: 返回 true 表示 remote 胜出，false 表示 local 胜出。
///     - updated_at 严格更大者胜；
///     - 相等时 device_id 字典序更大者胜（确定性）。
pub fn wins_concurrent(local: &PromptRow, remote: &PromptRow) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    // 时间戳相等：device_id 字典序 tie-break（确定性，任一端独立计算结果一致）
    remote.device_id > local.device_id
}

/// 合并两条同 id 的 Prompt，返回最终版本（胜出方内容 + 合并后的向量时钟）。
///
/// Business Logic: 同步时需将本地与远端版本合并为一个最终版本，包含正确内容与完整因果历史。
///     对照 Python PromptMerger.merge_prompt。
///
/// Code Logic:
/// 1. 始终合并双方向量时钟（保留完整因果历史）；
/// 2. 决策胜出方：非并发时按向量时钟严格序（should_update），并发时用 wins_concurrent
///    （LWW + device_id tie-break，比 Python 的纯 LWW 更确定）；
/// 3. 返回胜出方内容（克隆）+ 合并后的 vector_clock。
///
/// 注意：deleted 状态照常参与合并传播（删除是一次写入，deleted=true 的 prompt 也参与同步）。
pub fn merge_prompt(local: &PromptRow, remote: &PromptRow) -> PromptRow {
    let merged_clock = merge(&local.vector_clock, &remote.vector_clock);

    let relation = compare(&remote.vector_clock, &local.vector_clock);
    let remote_wins = match relation {
        ClockOrder::Concurrent => wins_concurrent(local, remote),
        // After/Before/Equal 统一走 should_update（与 Python 完全一致）
        _ => should_update(local, remote),
    };

    if remote_wins {
        // remote 胜出：用 remote 内容 + 合并后的时钟
        let mut winner = remote.clone();
        winner.vector_clock = merged_clock;
        winner
    } else {
        // local 胜出：用 local 内容 + 合并后的时钟
        let mut winner = local.clone();
        winner.vector_clock = merged_clock;
        winner
    }
}

#[cfg(test)]
mod tests {
    //! merger 单测：覆盖严格领先取新、并发 LWW、时间戳相等 device_id tie-break。

    use super::*;
    use std::collections::HashMap;

    /// 构造测试用 PromptRow（仅填同步相关字段）。
    fn row(
        id: &str,
        device_id: &str,
        updated_at: &str,
        vc: &[(&str, u64)],
        deleted: bool,
    ) -> PromptRow {
        let vector_clock: HashMap<String, u64> =
            vc.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        PromptRow {
            id: id.to_string(),
            title: format!("title-{device_id}"),
            content: format!("content-{device_id}"),
            tags: vec![],
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
            device_id: device_id.to_string(),
            vector_clock,
            deleted,
        }
    }

    #[test]
    fn should_update_when_remote_strictly_after() {
        // remote 向量时钟严格领先 → should_update true
        let local = row("p1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("p1", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        assert!(should_update(&local, &remote));
    }

    #[test]
    fn should_not_update_when_local_strictly_after() {
        // local 向量时钟严格领先 → should_update false
        let local = row("p1", "d1", "2024-01-02T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("p1", "d2", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update(&local, &remote));
    }

    #[test]
    fn should_not_update_when_equal() {
        // 向量时钟完全相同 → should_update false
        let local = row("p1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("p1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        assert!(!should_update(&local, &remote));
    }

    #[test]
    fn concurrent_lww_picks_newer_timestamp() {
        // 并发：remote updated_at 更晚 → remote 胜
        let local = row("p1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 2)], false);
        let remote = row("p1", "d2", "2024-01-03T00:00:00+00:00", &[("d2", 2)], false);
        let merged = merge_prompt(&local, &remote);
        // remote 内容胜出
        assert_eq!(merged.device_id, "d2");
        assert_eq!(merged.updated_at, "2024-01-03T00:00:00+00:00");
        // 向量时钟合并：逐 key 取 max
        assert_eq!(merged.vector_clock.get("d1"), Some(&2));
        assert_eq!(merged.vector_clock.get("d2"), Some(&2));

        // 反向传入（参数互换）也应得到一致胜出方（对称性）：d2 updated_at 更晚，始终胜出
        let merged2 = merge_prompt(&remote, &local);
        assert_eq!(merged2.device_id, "d2");
    }

    #[test]
    fn concurrent_equal_timestamp_device_id_tiebreak() {
        // 并发且时间戳相等：device_id 字典序更大者胜（确定性）
        let local = row(
            "p1",
            "aaa",
            "2024-01-01T00:00:00+00:00",
            &[("aaa", 1)],
            false,
        );
        let remote = row(
            "p1",
            "zzz",
            "2024-01-01T00:00:00+00:00",
            &[("zzz", 1)],
            false,
        );
        // remote device_id "zzz" > local "aaa" → remote 胜
        let merged = merge_prompt(&local, &remote);
        assert_eq!(merged.device_id, "zzz");

        // 反过来传入也应得到一致结果（确定性：无论方向，zzz 胜）
        let merged2 = merge_prompt(&remote, &local);
        assert_eq!(merged2.device_id, "zzz");
        // 向量时钟都应合并完整
        assert_eq!(merged.vector_clock.get("aaa"), Some(&1));
        assert_eq!(merged.vector_clock.get("zzz"), Some(&1));
    }

    #[test]
    fn merge_always_combines_vector_clock() {
        // 无论谁胜出，向量时钟都应合并双方
        let local = row(
            "p1",
            "d1",
            "2024-01-01T00:00:00+00:00",
            &[("d1", 3), ("d2", 1)],
            false,
        );
        let remote = row(
            "p1",
            "d2",
            "2024-01-01T00:00:00+00:00",
            &[("d1", 1), ("d2", 4)],
            false,
        );
        let merged = merge_prompt(&local, &remote);
        assert_eq!(merged.vector_clock.get("d1"), Some(&3)); // max(3,1)
        assert_eq!(merged.vector_clock.get("d2"), Some(&4)); // max(1,4)
    }

    #[test]
    fn deleted_prompt_participates_in_merge() {
        // deleted=true 的 prompt 也参与同步传播，照常按向量时钟/LWW 合并
        let local = row("p1", "d1", "2024-01-01T00:00:00+00:00", &[("d1", 1)], false);
        let remote = row("p1", "d2", "2024-01-02T00:00:00+00:00", &[("d1", 2)], true);
        let merged = merge_prompt(&local, &remote);
        // remote 严格领先（d1:2 > d1:1）→ remote 胜，删除事件传播
        assert!(merged.deleted);
        assert_eq!(merged.device_id, "d2");
    }
}
