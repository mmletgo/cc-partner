//! workbench/lan_fleet — LAN Agent Fleet 只读聚合
//!
//! Business Logic（为什么需要这个模块）:
//!     用户已保存的 local/remote shortcut 需要跨设备展示 Agent/Attention/Git/browser/
//!     Orchestrator 摘要；Fleet 只观察与导航，不调度、不复制 repo、不改 concurrency。
//!
//! Code Logic（这个模块做什么）:
//!     导出 models、collector、cache；owner-local 构建与控制设备 fan-out 共用。

pub mod cache;
pub mod collector;
pub mod models;

pub use collector::{build_owner_device_summary, collect_lan_fleet_for_state};
pub use models::{LanFleetOwnerBatchReq, LanFleetOwnerBatchResp, LanFleetSnapshot};
