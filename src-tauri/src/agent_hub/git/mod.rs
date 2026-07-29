//! agent_hub/git — Git device-lane 自动备份导出（仅本机 lane）
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub 需要把本机 full-Hub snapshot 备份到既有 GitHub 私有仓库的
//!     `agent-hub/devices/<deviceId>/` 车道。每台设备只写自己的 lane；fetch/rebase
//!     只为完成本 lane push，永不把远端 lane 自动导入 Hub SQLite。
//!
//! Code Logic（这个模块做什么）:
//!     Gate C Task 6：`lane` 负责车道路径与原子替换；`runtime` 提供
//!     `AgentHubGitRuntime::{mark_dirty,flush_pending,recover_pending}`，
//!     经 `CloudSyncRuntime` 单飞门闸执行 prepare/fetch → expand → 替换本 lane →
//!     pathspec commit/push，并持久化 pending 状态。

pub mod lane;
pub mod runtime;

pub use lane::{
    device_lane_rel_path, inventory_agent_hub_device_lanes, replace_device_lane, AGENT_HUB_GIT_ROOT,
};
pub use runtime::{
    next_retry_delay_secs, AgentHubGitRuntime, EXPORT_DEBOUNCE, PENDING_RETRY, RETRY_IMMEDIATE_SECS,
};
