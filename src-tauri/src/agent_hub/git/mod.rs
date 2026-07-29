//! agent_hub/git — Git device-lane 自动备份导出 + 确认式导入
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub 需要把本机 full-Hub snapshot 备份到既有 GitHub 私有仓库的
//!     `agent-hub/devices/<deviceId>/` 车道。每台设备只写自己的 lane；fetch/rebase
//!     只为完成本 lane push，永不把远端 lane 自动导入 Hub SQLite。远端 lane 仅在
//!     用户 inspect/preview/confirm 后经 SnapshotImporter 进入 Hub。
//!
//! Code Logic（这个模块做什么）:
//!     Gate C Task 6：`lane` 负责车道路径与原子替换；`runtime` 提供
//!     `AgentHubGitRuntime::{mark_dirty,flush_pending,recover_pending}`。
//!     Gate C Task 7：`preview` 提供 inspect/preview/confirm_git_import 与
//!     confirm_project_mapping（零自动 import）。

pub mod lane;
pub mod preview;
pub mod runtime;

pub use lane::{
    device_lane_rel_path, inventory_agent_hub_device_lanes, replace_device_lane, AGENT_HUB_GIT_ROOT,
};
pub use preview::{
    confirm_git_import_for_state, confirm_git_import_in_workdir, confirm_project_mapping_for_state,
    inspect_git_lanes_for_state, inspect_git_lanes_in_workdir, preview_git_import_for_state,
    preview_git_import_in_workdir, ConfirmGitImportOutcome, ConfirmGitImportRequest,
    ConfirmProjectMappingRequest, GitImportPreview, GitLaneInspectReport, GitLaneSummary,
};
pub use runtime::{
    next_retry_delay_secs, AgentHubGitRuntime, EXPORT_DEBOUNCE, PENDING_RETRY, RETRY_IMMEDIATE_SECS,
};
