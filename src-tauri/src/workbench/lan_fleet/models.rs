//! workbench/lan_fleet/models — LAN Agent Fleet 只读投影 DTO
//!
//! Business Logic（为什么需要这个模块）:
//!     控制设备需要按 owning device 聚合已保存 shortcut 的 Agent/Attention/Git/browser/
//!     Orchestrator 摘要；DTO 不得携带 Prompt、terminal bytes 或远端绝对 path。
//!
//! Code Logic（这个模块做什么）:
//!     定义 reachability/freshness/git/browser 枚举与 device/project/snapshot 结构；
//!     全部 camelCase serde，供 IPC/P2P/前端严格 schema 对齐。

use crate::workbench::agent_ledger::models::AgentLedgerSummary;
use serde::{Deserialize, Serialize};

/// Fleet 项目上 Agent activity（ledger 7d 聚合）的获取状态。
///
/// Business Logic（为什么需要这个类型）:
///     旧 peer / 单 field 失败时必须显示 unsupported/unavailable，不得把 unknown 当 0。
///
/// Code Logic（这个类型做什么）:
///     camelCase 字符串枚举；默认 Unavailable。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum FleetAgentActivityStatus {
    /// 本轮成功拿到 aggregate
    Live,
    /// 对端无 workbench.agent-ledger-summary.v1
    Unsupported,
    /// 超时/错误/未 join（默认，不阻断其它 Fleet 字段）
    #[default]
    Unavailable,
}

/// 单设备 owner batch 最多聚合的 project 数。
pub const FLEET_OWNER_BATCH_MAX_PROJECTS: usize = 100;

/// 全局 snapshot 最多返回的 project 总数（跨 device 截断）。
pub const FLEET_SNAPSHOT_MAX_PROJECTS: usize = 500;

/// 单 snapshot 最多引用的 active Agent 条数（投影计数上限提示）。
#[allow(dead_code)]
pub const FLEET_SNAPSHOT_MAX_AGENT_REFS: usize = 500;

/// remote fan-out 最大并发 device 数。
pub const FLEET_FANOUT_MAX_CONCURRENCY: usize = 3;

/// 单 device remote 请求超时（秒）。
pub const FLEET_DEVICE_TIMEOUT_SECS: u64 = 5;

/// 设备可达性（仅协议/网络，非认证或信任）。
///
/// Business Logic（为什么需要这个类型）:
///     UI 必须区分 live / offline / unsupported，且不得把 mDNS online 表述为可信设备。
///
/// Code Logic（这个类型做什么）:
///     camelCase 字符串枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FleetReachability {
    /// 对端可达且返回了本次 live 摘要
    Live,
    /// 对端不可达（超时/断连）
    Offline,
    /// 对端在线但不具备 workbench.lan-fleet.v1
    Unsupported,
}

/// 数据新鲜度。
///
/// Business Logic（为什么需要这个类型）:
///     offline 可展示 last cache，但必须明确 cached，不能伪装成 live 调度真值。
///
/// Code Logic（这个类型做什么）:
///     camelCase 字符串枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FleetFreshness {
    /// 本轮成功拉取
    Live,
    /// 使用内存 display cache
    Cached,
    /// 无可用数据
    Unknown,
}

/// Git 工作区摘要（field-level；失败时 Unknown）。
///
/// Business Logic（为什么需要这个类型）:
///     Fleet 行只需 clean/dirty/conflict，不暴露 path 或 diff 正文。
///
/// Code Logic（这个类型做什么）:
///     camelCase 字符串枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FleetGitState {
    Clean,
    Dirty,
    Conflict,
    Unknown,
}

/// Browser preview 摘要。
///
/// Business Logic（为什么需要这个类型）:
///     仅表达是否有 live preview，不暴露 target URL 到 Fleet 行（URL 可含敏感 host）。
///
/// Code Logic（这个类型做什么）:
///     camelCase 字符串枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FleetBrowserState {
    /// 存在未过期 preview session
    Active,
    /// 无 active preview
    Absent,
    /// 子源失败
    Unknown,
}

/// Agent phase 计数（provider-neutral）。
///
/// Business Logic（为什么需要这个类型）:
///     Rail/Fleet 需要低噪音摘要；只展示计数，不暴露 session 正文。
///
/// Code Logic（这个类型做什么）:
///     七态 u32 计数；camelCase。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPhaseCounts {
    pub launching: u32,
    pub working: u32,
    pub needs_input: u32,
    pub idle: u32,
    pub completed: u32,
    pub failed: u32,
    pub disconnected: u32,
}

impl AgentPhaseCounts {
    /// 返回异常 Agent 数量（needsInput + failed）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Project Rail 只对异常打 badge，正常 working 不得红标。
    ///
    /// Code Logic（这个函数做什么）:
    ///     needs_input + failed 饱和加。
    #[allow(dead_code)] // Project Rail / 前端 badge API surface（测试与 DTO 辅助）
    pub fn exception_count(self) -> u32 {
        self.needs_input.saturating_add(self.failed)
    }

    /// 返回全部 phase 计数之和。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     辅助文本与空态判断需要总 Agent 数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     七字段饱和求和。
    #[allow(dead_code)] // 空态/辅助文本总 Agent 数 API surface
    pub fn total(self) -> u32 {
        self.launching
            .saturating_add(self.working)
            .saturating_add(self.needs_input)
            .saturating_add(self.idle)
            .saturating_add(self.completed)
            .saturating_add(self.failed)
            .saturating_add(self.disconnected)
    }
}

/// 单项目 Fleet 摘要。
///
/// Business Logic（为什么需要这个类型）:
///     Fleet 视图与 Rail 按 project 展示聚合字段，且禁止绝对 path/Prompt/terminal bytes。
///
/// Code Logic（这个类型做什么）:
///     camelCase DTO；project_id 在控制侧 remote 时为 remote: 包装 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanFleetProjectSummary {
    pub project_id: String,
    pub display_name: String,
    pub project_kind: String,
    pub agent_counts: AgentPhaseCounts,
    pub attention_count: u32,
    pub terminal_count: u32,
    pub git_state: FleetGitState,
    pub browser_state: FleetBrowserState,
    pub orchestrator_running: u32,
    pub orchestrator_retrying: u32,
    pub last_activity_at: Option<String>,
    /// 7d Agent ledger 聚合状态；field 失败不得阻断其它字段。
    #[serde(default)]
    pub agent_activity_status: FleetAgentActivityStatus,
    /// 7d metadata-only 聚合；仅 status=live 时存在；不含 entry/session id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_activity: Option<AgentLedgerSummary>,
}

/// 单设备 Fleet 摘要。
///
/// Business Logic（为什么需要这个类型）:
///     按 owning device 分组；scheduler slots 必须是 device-global，不得用 project slotsUsed。
///
/// Code Logic（这个类型做什么）:
///     camelCase；error_code 为稳定 token（如 timeout / peer_error）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanFleetDeviceSummary {
    pub device_id: String,
    pub device_name: String,
    pub reachability: FleetReachability,
    pub freshness: FleetFreshness,
    pub scheduler_slots_used: Option<u32>,
    pub scheduler_slots_max: Option<u32>,
    pub projects: Vec<LanFleetProjectSummary>,
    pub error_code: Option<String>,
    /// 本设备摘要捕获时间（RFC3339）；cache 命中时为上次 live 时间。
    pub captured_at: Option<String>,
}

/// 控制设备全局 Fleet 快照。
///
/// Business Logic（为什么需要这个类型）:
///     前端 hook 一次拉取跨设备摘要；非调度权威，仅 display。
///
/// Code Logic（这个类型做什么）:
///     devices 列表 + truncated 标志 + generated_at。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanFleetSnapshot {
    pub generated_at: String,
    pub devices: Vec<LanFleetDeviceSummary>,
    pub truncated: bool,
}

/// owner-local batch 请求体（P2P snake_case）。
///
/// Business Logic（为什么需要这个类型）:
///     owning device 只接受调用方列出的本机 project id/path，不枚举全部项目。
///
/// Code Logic（这个类型做什么）:
///     snake_case 字段；project_ids 与 project_paths 至少其一；总数 ≤100。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LanFleetOwnerBatchReq {
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub project_paths: Vec<String>,
}

/// owner-local batch 响应（P2P camelCase，与控制侧 DTO 对齐）。
///
/// Business Logic（为什么需要这个类型）:
///     对端返回本机 device 视角的摘要；控制设备再包装 remote project_id 并合并 cache。
///
/// Code Logic（这个类型做什么）:
///     单设备摘要 + generated_at。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanFleetOwnerBatchResp {
    pub generated_at: String,
    pub device: LanFleetDeviceSummary,
}
