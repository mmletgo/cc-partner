//! control_client Workbench 超时单测（由 `control_client.rs` 以 `#[path]` 挂载）。
//!
//! Business Logic:
//!     将新增超时断言从生产源文件拆出，避免 module-boundary no-growth 与测试膨胀互相绑死。
//!
//! Code Logic:
//!     文件本体即为 `mod timeout_tests` 的模块体；仅在 `cfg(test)` 下由父模块 `#[path]` 引入。

use super::*;
use std::time::Duration;

/// Codex session 扫描可能遍历数千 jsonl；15s mutation 超时会把仍在扫描的 sidecar 误报 uncertain。
#[test]
fn workbench_control_timeout_extends_claude_session_search() {
    assert_eq!(
        workbench_control_timeout("claude.search"),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        workbench_control_timeout("claude.preview"),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        workbench_control_timeout("sessions.list"),
        Some(MUTATE_TIMEOUT)
    );
}

/// Token 统计导出可能翻页写盘，不能用默认 15s mutation 超时。
#[test]
fn workbench_control_timeout_extends_token_stats_export() {
    assert_eq!(
        workbench_control_timeout("agent_ledger.export_token_stats"),
        Some(Duration::from_secs(360))
    );
    assert_eq!(
        workbench_control_timeout("agent_ledger.summarize"),
        Some(MUTATE_TIMEOUT)
    );
}

/// Business Logic（为什么需要这个测试）:
///     GUI→sidecar 的 merge 会包住 Claude 解冲突；墙钟 360s 会在 CLI 仍有输出时误报超时。
///
/// Code Logic（这个测试做什么）:
///     merge 无 control HTTP 超时；commit 仍保留 360s 覆盖 commit message。
#[test]
fn workbench_control_timeout_lets_merge_wait_for_peer() {
    assert_eq!(workbench_control_timeout("worktrees.merge"), None);
    assert_eq!(
        workbench_control_timeout("worktrees.commit"),
        Some(Duration::from_secs(360))
    );
}
