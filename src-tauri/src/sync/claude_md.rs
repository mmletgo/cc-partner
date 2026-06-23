//! sync/claude_md.rs — CLAUDE.md 领域逻辑层
//!
//! Business Logic（为什么需要这个模块）:
//!     user 级 CLAUDE.md 的同步与 Prompt 不同：它既是磁盘文件（用户可能用任意编辑器改），
//!     也是 DB 单例记录（同步的权威来源）。因此需要集中处理三件事：
//!     1) 定位文件路径（~/.claude/CLAUDE.md）；
//!     2) 文件↔DB 对账（启动时、编辑后）——文件被应用外编辑时以文件为准并推进 vector_clock；
//!     3) 远端合并——复用与 Prompt 相同的 LWW 判定（向量时钟序 + 并发时 updated_at/device_id tie-break），
//!        合并后写回 DB 与文件。
//!
//! Code Logic（这个模块做什么）:
//!     - `claude_md_path`：复用 dirs::home_dir，拼 ~/.claude/CLAUDE.md。
//!     - `merge_claude_md`：纯函数，复用 vector_clock::merge/compare，决策胜出方（与 merger.rs 同款语义）。
//!     - `wins_concurrent_cm`：并发时的纯判定（与 merger::wins_concurrent 同款 tie-break，入参为 ClaudeMdRow）。
//!     - `write_file_if_changed`：仅在内容变化时写文件，避免无谓 IO。
//!     - `reconcile_from_file`：文件↔DB 对账（DB 无行→初始化；内容一致→no-op；不一致→以文件为准推进时钟）。

use crate::error::AppError;
use crate::models::claude_md::{ClaudeMdRow, CLAUDE_MD_ID};
use crate::sync::vector_clock::{self, ClockOrder};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// 返回 user 级 CLAUDE.md 的绝对路径：~/.claude/CLAUDE.md。
///
/// Business Logic: 该文件是 Claude Code 等工具读取的全局用户记忆，路径固定在 home 下的 .claude 目录。
/// Code Logic: 复用 dirs::home_dir（与 config.rs 一致），拼 ".claude/CLAUDE.md"。
pub fn claude_md_path() -> PathBuf {
    dirs::home_dir()
        .expect("无法定位用户 home 目录，环境异常")
        .join(".claude")
        .join("CLAUDE.md")
}

/// 合并本地与远端的 CLAUDE.md 版本，返回最终版本（胜出方内容 + 合并后的向量时钟）。
///
/// Business Logic: 跨设备同步需决策保留哪一方内容，并合并双方因果历史。判定与 `merger::merge_prompt`
///     同款：remote 严格领先（compare(remote, local)==After）→ remote 胜；local 严格领先（Before）→
///     local 胜；完全相同（Equal）→ local 胜（无意义覆盖）；并发（Concurrent）→ LWW + device_id tie-break。
///
/// Code Logic:
/// 1. 始终合并双方向量时钟（保留完整因果历史）；
/// 2. compare(&remote.vc, &local.vc) 返回 remote 相对 local 的偏序关系：
///    - After（remote 领先）→ remote 胜；
///    - Before / Equal → local 胜；
///    - Concurrent → wins_concurrent_cm 决策。
/// 3. 胜出方克隆 + 覆盖 vector_clock 为合并结果。
pub fn merge_claude_md(local: &ClaudeMdRow, remote: &ClaudeMdRow) -> ClaudeMdRow {
    let merged_clock = vector_clock::merge(&local.vector_clock, &remote.vector_clock);
    let relation = vector_clock::compare(&remote.vector_clock, &local.vector_clock);
    let remote_wins = match relation {
        ClockOrder::Concurrent => wins_concurrent_cm(local, remote),
        ClockOrder::After => true,
        ClockOrder::Before | ClockOrder::Equal => false,
    };

    let mut winner = if remote_wins {
        remote.clone()
    } else {
        local.clone()
    };
    winner.vector_clock = merged_clock;
    winner
}

/// 并发冲突时的纯判定：决定 local 与 remote 谁胜出（含确定性 tie-break）。
///
/// Business Logic: 当两版本向量时钟并发（互有领先）时用 LWW。时间戳相等时用 device_id 字典序
///     做 tie-break（与 merger::wins_concurrent 同款），保证双端确定性、不抖动。
///
/// Code Logic: 返回 true 表示 remote 胜出。
///     - updated_at 严格更大者胜；
///     - 相等时 device_id 字典序更大者胜。
fn wins_concurrent_cm(local: &ClaudeMdRow, remote: &ClaudeMdRow) -> bool {
    if remote.updated_at > local.updated_at {
        return true;
    }
    if remote.updated_at < local.updated_at {
        return false;
    }
    remote.device_id > local.device_id
}

/// 仅在内容变化时将正文写回 CLAUDE.md 文件，内容相同则 no-op。
///
/// Business Logic: 合并/对账后需把权威内容落盘；但无变化时写文件会刷新 mtime 误导对账，
///     故先比对再决定是否写。
/// Code Logic: 读现有文件（NotFound 视为空串），与 content 不同则 create_dir_all(parent) + write。
pub async fn write_file_if_changed(content: &str) -> Result<(), AppError> {
    let path = claude_md_path();
    let existing = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if existing == content {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content)?;
    Ok(())
}

/// 文件↔DB 对账：把磁盘上 CLAUDE.md 的现状同步进 DB 单例记录。
///
/// Business Logic: 用户可能在应用外（编辑器、Claude Code 自身）修改 ~/.claude/CLAUDE.md，
///     启动或编辑刷新时需把这份"外部真相"纳入 DB，否则同步会用过期 DB 覆盖用户最新改动。
///     - DB 无行：用文件内容初始化首条记录（空文件 → 空 vector_clock；非空 → 本设备计数器置 1）；
///     - DB 行内容与文件一致：no-op；
///     - DB 行内容与文件不一致：以文件为准，推进本设备 vector_clock（表示本端发生了一次写入事件），
///       使对端能感知本次变化。
///
/// Code Logic: 读文件（NotFound→空串）→ 读 DB 行 → 三分支决策 → upsert。
pub async fn reconcile_from_file(state: &crate::state::AppState) -> Result<(), AppError> {
    let path = claude_md_path();
    let file_content = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let db_row = state.claude_md_repo.get().await?;
    let device_id = state.device_id.as_str().to_string();

    match db_row {
        None => {
            // DB 无行：用文件内容初始化首条记录
            let vector_clock = if file_content.is_empty() {
                HashMap::new()
            } else {
                let mut m = HashMap::new();
                m.insert(device_id.clone(), 1);
                m
            };
            let row = ClaudeMdRow {
                id: CLAUDE_MD_ID.to_string(),
                content: file_content,
                updated_at: Utc::now().to_rfc3339(),
                device_id: device_id.clone(),
                vector_clock,
            };
            state.claude_md_repo.upsert(&row).await?;
        }
        Some(db) if file_content == db.content => {
            // 内容一致：无需落库
        }
        Some(db) => {
            // 文件被应用外编辑：以文件为准，推进本设备 vector_clock
            let row = ClaudeMdRow {
                id: CLAUDE_MD_ID.to_string(),
                content: file_content,
                updated_at: Utc::now().to_rfc3339(),
                device_id: device_id.clone(),
                vector_clock: vector_clock::increment(&db.vector_clock, &device_id),
            };
            state.claude_md_repo.upsert(&row).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! merge_claude_md 单测：覆盖严格领先/落后/相等/并发 LWW/并发 tie-break/向量时钟合并，
    //! 对照 merger.rs 的测试风格。

    use super::*;

    /// 构造测试用 ClaudeMdRow（仅填同步相关字段）。
    fn row(device_id: &str, updated_at: &str, content: &str, vc: &[(&str, u64)]) -> ClaudeMdRow {
        let vector_clock: HashMap<String, u64> =
            vc.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        ClaudeMdRow {
            id: CLAUDE_MD_ID.to_string(),
            content: content.to_string(),
            updated_at: updated_at.to_string(),
            device_id: device_id.to_string(),
            vector_clock,
        }
    }

    #[test]
    fn remote_strictly_after_remote_wins() {
        // remote 向量时钟严格领先 → remote 胜
        let local = row("d1", "2024-01-01T00:00:00+00:00", "local", &[("d1", 1)]);
        let remote = row("d2", "2024-01-02T00:00:00+00:00", "remote", &[("d1", 2)]);
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.content, "remote");
        assert_eq!(merged.device_id, "d2");
        assert_eq!(merged.vector_clock.get("d1"), Some(&2));
    }

    #[test]
    fn remote_strictly_before_local_wins() {
        // local 向量时钟严格领先 → local 胜
        let local = row("d1", "2024-01-02T00:00:00+00:00", "local", &[("d1", 2)]);
        let remote = row("d2", "2024-01-01T00:00:00+00:00", "remote", &[("d1", 1)]);
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.content, "local");
        assert_eq!(merged.device_id, "d1");
    }

    #[test]
    fn equal_local_wins_and_clock_merged() {
        // 向量时钟完全相同（Equal）→ local 胜，且向量时钟合并（此处即自身）
        let local = row("d1", "2024-01-01T00:00:00+00:00", "local", &[("d1", 1)]);
        let remote = row("d1", "2024-01-01T00:00:00+00:00", "remote", &[("d1", 1)]);
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.content, "local");
        assert_eq!(merged.vector_clock.get("d1"), Some(&1));
    }

    #[test]
    fn concurrent_remote_newer_wins() {
        // 并发：remote updated_at 更晚 → remote 胜
        let local = row("d1", "2024-01-01T00:00:00+00:00", "local", &[("d1", 2)]);
        let remote = row("d2", "2024-01-03T00:00:00+00:00", "remote", &[("d2", 2)]);
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.content, "remote");
        assert_eq!(merged.device_id, "d2");
        // 向量时钟合并：逐 key 取 max
        assert_eq!(merged.vector_clock.get("d1"), Some(&2));
        assert_eq!(merged.vector_clock.get("d2"), Some(&2));

        // 反向传入也一致（对称性）
        let merged2 = merge_claude_md(&remote, &local);
        assert_eq!(merged2.content, "remote");
    }

    #[test]
    fn concurrent_equal_timestamp_device_id_tiebreak() {
        // 并发且时间戳相等：device_id 字典序更大者胜（确定性）
        let local = row("aaa", "2024-01-01T00:00:00+00:00", "local", &[("aaa", 1)]);
        let remote = row("zzz", "2024-01-01T00:00:00+00:00", "remote", &[("zzz", 1)]);
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.content, "remote");
        assert_eq!(merged.device_id, "zzz");

        // 反向传入也一致
        let merged2 = merge_claude_md(&remote, &local);
        assert_eq!(merged2.device_id, "zzz");
        assert_eq!(merged.vector_clock.get("aaa"), Some(&1));
        assert_eq!(merged.vector_clock.get("zzz"), Some(&1));
    }

    #[test]
    fn merge_combines_vector_clock_per_key_max() {
        // 无论谁胜出，向量时钟都是双方逐 key max
        let local = row(
            "d1",
            "2024-01-01T00:00:00+00:00",
            "local",
            &[("d1", 3), ("d2", 1)],
        );
        let remote = row(
            "d2",
            "2024-01-01T00:00:00+00:00",
            "remote",
            &[("d1", 1), ("d2", 4)],
        );
        let merged = merge_claude_md(&local, &remote);
        assert_eq!(merged.vector_clock.get("d1"), Some(&3)); // max(3,1)
        assert_eq!(merged.vector_clock.get("d2"), Some(&4)); // max(1,4)
    }
}
