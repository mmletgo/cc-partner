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

/// Workbench Prompt 优化会跑最长 180s 的 Claude CLI，GUI→sidecar 不能套 15s mutation 超时。
///
/// Business Logic（为什么需要这个测试）:
///     桌面 GUI 必须把流式优化代理到 sidecar owner；默认 15s 会把仍在生成的 CLI 误报 uncertain。
///
/// Code Logic（这个测试做什么）:
///     `prompt_optimizer.stream` 使用 360s，与其它 Claude 长操作一致。
#[test]
fn workbench_control_timeout_extends_prompt_optimizer_stream() {
    assert_eq!(
        workbench_control_timeout("prompt_optimizer.stream"),
        Some(Duration::from_secs(360))
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

/// Workbench 远端 cc-switch CLI 安装在对端跑 brew（可能数分钟），GUI→sidecar 不能套 15s 超时。
///
/// Business Logic（为什么需要这个测试）:
///     GuiClient 经 control op `provider-manager.install` 代理到 sidecar owner，owner 再
///     POST 对端 `/api/provider-manager/install-cli`（远端客户端独立预算 420s）。control
///     链路用 360s 长变更预算（对齐 BACKUP_MUTATE_TIMEOUT 先例），15s 会把仍在安装的
///     brew 误报 uncertain；两层预算独立，control 墙钟先到时 GUI 侧报超时，sidecar 继续
///     完成安装，用户重新检测可见结果。
///
/// Code Logic（这个测试做什么）:
///     `provider-manager.install` 使用 360s，普通状态查询 op 仍为 MUTATE_TIMEOUT。
#[test]
fn workbench_control_timeout_extends_provider_manager_install() {
    assert_eq!(
        workbench_control_timeout("provider-manager.install"),
        Some(Duration::from_secs(360))
    );
    assert_eq!(
        workbench_control_timeout("provider-manager.status"),
        Some(MUTATE_TIMEOUT)
    );
}
