//! workbench/agent_runtime/models — Agent session 运行时领域模型
//!
//! Business Logic（为什么需要这个模块）:
//!     owning device 需要在最小 metadata 上表达 Agent lifecycle（phase/version/关联 ID），
//!     且不得携带 Prompt、回复、terminal bytes、transcript path 或 credential。
//!
//! Code Logic（这个模块做什么）:
//!     定义 phase 枚举、内部 runtime 行、create/mutation 输入结构；serde camelCase 供
//!     内部 JSON（OSC）与后续 DTO 映射复用。`native_session_id` 仅 owner-local。

use serde::{Deserialize, Serialize};

/// Agent session 生命周期阶段（provider-neutral）。
///
/// Business Logic（为什么需要这个类型）:
///     UI / Orchestrator / remote 投影都需要同一套稳定 phase token，不能依赖厂商文案。
///
/// Code Logic（这个类型做什么）:
///     七态枚举；终态为 Completed / Failed / Disconnected；serde camelCase 与 OSC JSON 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSessionPhase {
    /// 已创建、adapter 启动中
    Launching,
    /// 正在处理任务
    Working,
    /// 等待人工输入
    NeedsInput,
    /// 空闲但仍挂在 terminal
    Idle,
    /// 正常完成（终态）
    Completed,
    /// 失败结束（终态）
    Failed,
    /// terminal 丢失或 owner 对账断开（终态）
    Disconnected,
}

impl AgentSessionPhase {
    /// 判断 phase 是否为 terminal 级终态。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     active 唯一索引与 list_active 只关心未终态 session；创建替换前需先终结旧 active。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Completed / Failed / Disconnected 返回 true。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Disconnected
        )
    }

    /// 将 phase 编码为稳定存储 token（snake 风格短码，与历史 SQLite 约定一致）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite TEXT 列需要稳定、可 diff 的字面量，避免 serde 变体名漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回固定 ASCII 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launching => "launching",
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Disconnected => "disconnected",
        }
    }

    /// 从存储 token 解析 phase。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     读库与 OSC 入站需把字符串还原为类型安全枚举；未知值不得 silent 默认 Working。
    ///
    /// Code Logic（这个函数做什么）:
    ///     匹配 as_str 与 camelCase 别名；未知返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "launching" | "Launching" => Some(Self::Launching),
            "working" | "Working" => Some(Self::Working),
            "needs_input" | "needsInput" | "NeedsInput" => Some(Self::NeedsInput),
            "idle" | "Idle" => Some(Self::Idle),
            "completed" | "Completed" => Some(Self::Completed),
            "failed" | "Failed" => Some(Self::Failed),
            "disconnected" | "Disconnected" => Some(Self::Disconnected),
            _ => None,
        }
    }
}

/// owning-device 权威的 Agent session 运行时行（含 owner-local native_session_id）。
///
/// Business Logic（为什么需要这个类型）:
///     repo / reducer 需要完整行；投影层再映射到剔除 native_session_id 的 DTO。
///
/// Code Logic（这个类型做什么）:
///     镜像 `workbench_agent_sessions` 列；`is_active` 与终态 phase 同步维护。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionRuntime {
    /// owner 生成的 UUID
    pub id: String,
    /// 所属项目
    pub project_id: String,
    /// 可选 worktree
    pub worktree_id: Option<String>,
    /// 绑定的 terminal session id（active 时全局唯一）
    pub terminal_session_id: String,
    /// 可选 Orchestrator task id
    pub orchestrator_task_id: Option<String>,
    /// 可选 attempt 序号
    pub orchestrator_attempt: Option<u32>,
    /// provider 稳定 id（如 claudeCodeVisible）
    pub provider_id: String,
    /// provider-native session id（仅 owner-local，禁止进入 projection DTO）
    pub native_session_id: Option<String>,
    /// 当前 phase
    pub phase: AgentSessionPhase,
    /// owner 内单调版本
    pub version: u64,
    /// 创建时间 RFC3339
    pub started_at: String,
    /// 最近活动时间 RFC3339
    pub last_activity_at: String,
    /// 结束时间 RFC3339
    pub ended_at: Option<String>,
    /// 可选 outcome 稳定 code
    pub outcome_code: Option<String>,
    /// resume 时指向被恢复的历史 session
    pub resumed_from_agent_session_id: Option<String>,
    /// 是否为该 terminal 当前 active（与 partial unique index 对齐）
    pub is_active: bool,
}

/// 创建 active Agent session 的输入。
///
/// Business Logic（为什么需要这个类型）:
///     Runner / terminal attach 只需提供关联 ID 与 provider，id/version 由 repo 生成。
///
/// Code Logic（这个类型做什么）:
///     承载 create_active 所需字段；缺省 phase=Launching、version=1。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateActiveAgentSession {
    /// 可选显式 id（测试注入）；None 时 repo 生成 UUID
    pub id: Option<String>,
    /// 所属项目
    pub project_id: String,
    /// 可选 worktree
    pub worktree_id: Option<String>,
    /// 绑定 terminal
    pub terminal_session_id: String,
    /// 可选 Orchestrator task
    pub orchestrator_task_id: Option<String>,
    /// 可选 attempt
    pub orchestrator_attempt: Option<u32>,
    /// provider id
    pub provider_id: String,
    /// 可选 native session
    pub native_session_id: Option<String>,
    /// 初始 phase（默认 Launching）
    pub phase: AgentSessionPhase,
    /// 创建/活动时间 RFC3339
    pub started_at: String,
    /// resume 来源
    pub resumed_from_agent_session_id: Option<String>,
}

/// 对已有 Agent session 的 CAS mutation（OSC / Hook / bridge 入站）。
///
/// Business Logic（为什么需要这个类型）:
///     迟到事件与并发写必须用 agentSessionId + terminalSessionId + expectedVersion 保护。
///
/// Code Logic（这个类型做什么）:
///     expected_version 必须等于当前 version；成功后 version 变为 event_version
///     （须严格大于 expected_version）。`native_session_id` 仅在 Some 时覆盖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeMutation {
    /// 目标 agent session id
    pub agent_session_id: String,
    /// 必须与行上 terminal 一致
    pub terminal_session_id: String,
    /// CAS：必须等于当前 version
    pub expected_version: u64,
    /// 事件携带的新 version（必须 > expected_version）
    pub event_version: u64,
    /// 目标 phase
    pub phase: AgentSessionPhase,
    /// 可选更新 native session
    pub native_session_id: Option<String>,
    /// 可选 outcome（终态时）
    pub outcome_code: Option<String>,
    /// 事件发生时间 RFC3339
    pub occurred_at: String,
}
