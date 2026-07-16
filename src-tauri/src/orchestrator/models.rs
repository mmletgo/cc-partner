//! Orchestrator DTOs and row models.

#![allow(dead_code)]

use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// 开发尝试 evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     自动编排任务会积累多轮开发 attempt，前端和远端同步需要稳定 kind 字符串识别开发尝试证据。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 developmentAttempt 的存储值，避免调用点硬编码。
pub const EVIDENCE_KIND_DEVELOPMENT_ATTEMPT: &str = "developmentAttempt";

/// 验证命令输出 evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     Agent 完成后必须保存验证命令输出，供用户和 verifier 判断任务是否满足验收标准。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 verificationOutput 的存储值。
pub const EVIDENCE_KIND_VERIFICATION_OUTPUT: &str = "verificationOutput";

/// Claude verifier 审查结果 evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     Phase8 引入 headless verifier Claude，用户需要看到模型审查结论与风险说明。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 verificationReview 的存储值。
pub const EVIDENCE_KIND_VERIFICATION_REVIEW: &str = "verificationReview";

/// 修复指令 evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     verifier 判定失败后生成的 repair prompt 需要持久化，方便用户理解下一轮 Claude 的修复目标。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 repairPrompt 的存储值。
pub const EVIDENCE_KIND_REPAIR_PROMPT: &str = "repairPrompt";

/// 远端 outbox evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     远端任务离线队列与 mirror 同步需要稳定 kind 字符串描述 outbox 相关证据。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 remoteOutbox 的存储值。
pub const EVIDENCE_KIND_REMOTE_OUTBOX: &str = "remoteOutbox";

/// 自动交付 evidence kind。
///
/// Business Logic（为什么需要这个常量）:
///     full-auto delivery 的 commit/push/merge/push-main 阶段需要统一归类，便于前端展示交付轨迹。
///
/// Code Logic（这个常量做什么）:
///     集中定义 `orchestrator_task_evidence.kind` 中 delivery 的存储值。
pub const EVIDENCE_KIND_DELIVERY: &str = "delivery";

/// Orchestrator 任务生命周期状态。
///
/// Business Logic（为什么需要这个枚举）:
///     自动编排任务需要在草稿、排队、执行、验证、交付和终态之间流转，后续调度器与前端
///     都会依赖同一组状态值。
///
/// Code Logic（这个枚举做什么）:
///     用 Rust enum 表达内部状态，并通过 serde camelCase 兼容前端 DTO 序列化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorTaskStatus {
    Draft,
    Queued,
    Preparing,
    Running,
    Verifying,
    Delivering,
    Done,
    Blocked,
    Aborted,
}

impl OrchestratorTaskStatus {
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite 表用小写字符串保存状态，方便后续前端筛选和人工排查数据库内容。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把内部状态枚举转换为稳定的小写存储值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Delivering => "delivering",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Aborted => "aborted",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     仓储从 SQLite 读取任务状态时必须还原为内部枚举，避免命令层处理裸字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析小写状态字符串；未知值返回业务错误，暴露数据损坏或迁移问题。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "draft" => Ok(Self::Draft),
            "queued" => Ok(Self::Queued),
            "preparing" => Ok(Self::Preparing),
            "running" => Ok(Self::Running),
            "verifying" => Ok(Self::Verifying),
            "delivering" => Ok(Self::Delivering),
            "done" => Ok(Self::Done),
            "blocked" => Ok(Self::Blocked),
            "aborted" => Ok(Self::Aborted),
            other => Err(AppError::generic(format!(
                "未知 Orchestrator 状态: {other}"
            ))),
        }
    }
}

/// Orchestrator 创建任务动作。
///
/// Business Logic（为什么需要这个枚举）:
///     创建任务弹窗提供“放入 Backlog / 放入 Todo / 创建并启动”三种动作，后端需要稳定契约区分默认保存、
///     等待调度和立即尝试调度，避免旧默认行为把新任务直接入队。
///
/// Code Logic（这个枚举做什么）:
///     使用 camelCase/lowercase JSON 值接收 `createAction`，默认 Backlog；提供持久化初始状态和是否触发调度的判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorCreateAction {
    Backlog,
    Todo,
    Start,
}

impl Default for OrchestratorCreateAction {
    /// Business Logic（为什么需要这个函数）:
    ///     旧请求或未显式选择动作的创建弹窗必须安全落入 Backlog，不能自动启动任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 createAction 缺省值 Backlog，供 serde(default) 和内部默认值复用。
    fn default() -> Self {
        Self::Backlog
    }
}

impl OrchestratorCreateAction {
    /// Business Logic（为什么需要这个函数）:
    ///     新建任务写入 legacy status 时仍要兼容旧状态机与旧 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Backlog 映射 Draft；Todo/Start 先映射 Queued，后续 scheduler 只读取 split state 领取。
    pub fn initial_status(self) -> OrchestratorTaskStatus {
        match self {
            Self::Backlog => OrchestratorTaskStatus::Draft,
            Self::Todo | Self::Start => OrchestratorTaskStatus::Queued,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Create and Start 只是创建后尝试调度，不应影响 Backlog/Todo 的创建结果。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅 Start 返回 true，调用方据此触发 best-effort dispatch。
    pub fn should_dispatch_after_create(self) -> bool {
        matches!(self, Self::Start)
    }
}

/// Orchestrator 任务工作流状态。
///
/// Business Logic（为什么需要这个枚举）:
///     任务的看板流转需要与运行时 runner 状态拆分，避免“正在排队/运行”与“待办/返工/合并”等用户可见阶段互相覆盖。
///
/// Code Logic（这个枚举做什么）:
///     表达用户可见的任务工作流状态，serde 使用 camelCase，SQLite 使用稳定字符串持久化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorWorkflowState {
    Backlog,
    Todo,
    InProgress,
    HumanReview,
    Rework,
    Merging,
    Done,
    Canceled,
}

impl OrchestratorWorkflowState {
    /// Business Logic（为什么需要这个函数）:
    ///     split state 会进入 SQLite schema，数据库内必须保存稳定且可读的工作流状态字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把工作流状态枚举转换为 camelCase 存储字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "inProgress",
            Self::HumanReview => "humanReview",
            Self::Rework => "rework",
            Self::Merging => "merging",
            Self::Done => "done",
            Self::Canceled => "canceled",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     仓储读取旧库或新库任务时需要把工作流状态还原为强类型，避免后续逻辑处理裸字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析稳定存储字符串；未知值转为 AppError，显式暴露数据损坏或迁移遗漏。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "backlog" => Ok(Self::Backlog),
            "todo" => Ok(Self::Todo),
            "inProgress" => Ok(Self::InProgress),
            "humanReview" => Ok(Self::HumanReview),
            "rework" => Ok(Self::Rework),
            "merging" => Ok(Self::Merging),
            "done" => Ok(Self::Done),
            "canceled" => Ok(Self::Canceled),
            other => Err(AppError::generic(format!(
                "未知 Orchestrator 工作流状态: {other}"
            ))),
        }
    }
}

/// Orchestrator 任务运行状态。
///
/// Business Logic（为什么需要这个枚举）:
///     Runner 生命周期需要独立表达排队、准备、运行、验证、阻塞和交付，供调度器和前端判断当前执行现场。
///
/// Code Logic（这个枚举做什么）:
///     表达机器可执行的运行态，serde 使用 camelCase，SQLite 使用稳定字符串持久化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorRunState {
    Idle,
    Queued,
    Preparing,
    Running,
    Verifying,
    Retrying,
    Blocked,
    Delivering,
}

impl OrchestratorRunState {
    /// Business Logic（为什么需要这个函数）:
    ///     split runtime state 需要持久化到 SQLite，便于崩溃恢复和跨设备诊断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把运行状态枚举转换为稳定存储字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Retrying => "retrying",
            Self::Blocked => "blocked",
            Self::Delivering => "delivering",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     仓储读取任务行时必须恢复强类型运行状态，避免调度逻辑误用未知字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析稳定存储字符串；未知值返回 AppError。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "idle" => Ok(Self::Idle),
            "queued" => Ok(Self::Queued),
            "preparing" => Ok(Self::Preparing),
            "running" => Ok(Self::Running),
            "verifying" => Ok(Self::Verifying),
            "retrying" => Ok(Self::Retrying),
            "blocked" => Ok(Self::Blocked),
            "delivering" => Ok(Self::Delivering),
            other => Err(AppError::generic(format!(
                "未知 Orchestrator 运行状态: {other}"
            ))),
        }
    }
}

/// Orchestrator runner 当前尝试阶段。
///
/// Business Logic（为什么需要这个枚举）:
///     可见 Runner 需要向用户展示当前卡在准备 workspace、构建 prompt、启动 Claude、流式输出或收尾等具体阶段。
///
/// Code Logic（这个枚举做什么）:
///     表达单次执行 attempt 的细粒度 phase，serde 使用 camelCase，SQLite 存储同名稳定字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OrchestratorAttemptPhase {
    PreparingWorkspace,
    BuildingPrompt,
    LaunchingRunner,
    InitializingSession,
    Streaming,
    Finishing,
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CanceledByReconciliation,
}

impl OrchestratorAttemptPhase {
    /// Business Logic（为什么需要这个函数）:
    ///     attempt phase 会记录在任务行中，数据库值必须稳定以支持恢复、排查和未来前端筛选。
    ///
    /// Code Logic（这个函数做什么）:
    ///     把 attempt phase 枚举转换为 camelCase 存储字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreparingWorkspace => "preparingWorkspace",
            Self::BuildingPrompt => "buildingPrompt",
            Self::LaunchingRunner => "launchingRunner",
            Self::InitializingSession => "initializingSession",
            Self::Streaming => "streaming",
            Self::Finishing => "finishing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timedOut",
            Self::Stalled => "stalled",
            Self::CanceledByReconciliation => "canceledByReconciliation",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     仓储读取 attempt phase 时需要强类型结果，未知值应显式暴露而不是被静默忽略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 camelCase 存储字符串；未知值返回 AppError。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "preparingWorkspace" => Ok(Self::PreparingWorkspace),
            "buildingPrompt" => Ok(Self::BuildingPrompt),
            "launchingRunner" => Ok(Self::LaunchingRunner),
            "initializingSession" => Ok(Self::InitializingSession),
            "streaming" => Ok(Self::Streaming),
            "finishing" => Ok(Self::Finishing),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "timedOut" => Ok(Self::TimedOut),
            "stalled" => Ok(Self::Stalled),
            "canceledByReconciliation" => Ok(Self::CanceledByReconciliation),
            other => Err(AppError::generic(format!(
                "未知 Orchestrator 尝试阶段: {other}"
            ))),
        }
    }
}

/// Orchestrator split state 聚合结果。
///
/// Business Logic（为什么需要这个结构体）:
///     迁移旧 status 时需要一次性得到工作流状态和运行状态，确保历史任务在新模型下可解释。
///
/// Code Logic（这个结构体做什么）:
///     保存 workflow_state/run_state 二元组，并提供 legacy status 到 split state 的确定性映射。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitTaskState {
    pub workflow_state: OrchestratorWorkflowState,
    pub run_state: OrchestratorRunState,
}

impl SplitTaskState {
    /// Business Logic（为什么需要这个函数）:
    ///     创建任务动作需要直接得到 split state，避免各入口重复拼接 Backlog/Todo 与 Idle。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Backlog 映射 Backlog/Idle；Todo/Start 映射 scheduler 可领取的 Todo/Idle。
    pub fn from_create_action(action: OrchestratorCreateAction) -> Self {
        match action {
            OrchestratorCreateAction::Backlog => Self {
                workflow_state: OrchestratorWorkflowState::Backlog,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorCreateAction::Todo | OrchestratorCreateAction::Start => Self {
                workflow_state: OrchestratorWorkflowState::Todo,
                run_state: OrchestratorRunState::Idle,
            },
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧数据库只保存单一 status；升级到 split state 时必须保持旧任务在看板和运行态上的语义一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 legacy OrchestratorTaskStatus 返回对应 workflow_state 与 run_state，供 migration 和新建默认值复用。
    pub fn from_legacy_status(status: OrchestratorTaskStatus) -> Self {
        match status {
            OrchestratorTaskStatus::Draft => Self {
                workflow_state: OrchestratorWorkflowState::Backlog,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorTaskStatus::Queued => Self {
                workflow_state: OrchestratorWorkflowState::Todo,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorTaskStatus::Preparing => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Preparing,
            },
            OrchestratorTaskStatus::Running => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Running,
            },
            OrchestratorTaskStatus::Verifying => Self {
                workflow_state: OrchestratorWorkflowState::InProgress,
                run_state: OrchestratorRunState::Verifying,
            },
            OrchestratorTaskStatus::Delivering => Self {
                workflow_state: OrchestratorWorkflowState::Merging,
                run_state: OrchestratorRunState::Delivering,
            },
            OrchestratorTaskStatus::Done => Self {
                workflow_state: OrchestratorWorkflowState::Done,
                run_state: OrchestratorRunState::Idle,
            },
            OrchestratorTaskStatus::Blocked => Self {
                workflow_state: OrchestratorWorkflowState::Rework,
                run_state: OrchestratorRunState::Blocked,
            },
            OrchestratorTaskStatus::Aborted => Self {
                workflow_state: OrchestratorWorkflowState::Canceled,
                run_state: OrchestratorRunState::Idle,
            },
        }
    }
}

/// Orchestrator 阶段输出。
///
/// Business Logic（为什么需要这个枚举）:
///     调度器、Runner、验证器和交付器后续会用统一 outcome 推动状态机，避免每个阶段直接写状态。
///
/// Code Logic（这个枚举做什么）:
///     表达状态机输入事件，只在 Rust 内部使用，不需要序列化到前端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStageOutcome {
    Queue,
    StartPreparing,
    RunnerReady,
    AgentFinished,
    VerificationPassed,
    VerificationFailed,
    VerificationInfraFailed,
    DeliveryPassed,
    Block,
    Abort,
    Noop,
}

/// Orchestrator 任务数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     编排任务需要完整持久化目标、验收标准、执行关联信息、阻塞原因与时间戳。
///
/// Code Logic（这个结构体做什么）:
///     字段与 orchestrator_tasks 表一一对应，状态用枚举表达，时间戳按项目约定透传 String。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OrchestratorTaskRow {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub status: OrchestratorTaskStatus,
    pub workflow_state: OrchestratorWorkflowState,
    pub run_state: OrchestratorRunState,
    pub attempt_phase: Option<OrchestratorAttemptPhase>,
    pub source: String,
    pub external_id: Option<String>,
    pub external_identifier: Option<String>,
    pub external_url: Option<String>,
    pub external_state: Option<String>,
    pub external_labels: Option<Vec<String>>,
    pub runner_provider: Option<String>,
    pub claude_session_id: Option<String>,
    /// 统一 Agent session 引用（A1 dual-write，与 claude_session_id 并行一个版本）。
    pub agent_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub runtime_started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub last_runtime_event: Option<String>,
    pub last_runtime_message: Option<String>,
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    /// Preparing claim 世代 token：claim 签发；touch/phase/mark_running 必须 CAS 匹配。
    pub prepare_claim_token: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    /// 可通知状态 revision：进入 HumanReview/Blocked/Done 时 +1，供 operational 去重。
    pub state_version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// 运营通知 kind（隐私安全，不含任务正文）。
///
/// Business Logic（为什么需要这个枚举）:
///     桌面 OS 通知与 baseline snapshot 需要稳定四类运营事件，且不得泄露 title/project/goal。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase：`humanReview|blocked|remoteOutboxFailed|taskDone`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationalNotificationKind {
    HumanReview,
    Blocked,
    RemoteOutboxFailed,
    TaskDone,
}

impl OperationalNotificationKind {
    /// Business Logic（为什么需要这个函数）:
    ///     snapshot SQL UNION 与 wire payload 需要稳定字符串，禁止调用点硬编码漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 camelCase kind 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanReview => "humanReview",
            Self::Blocked => "blocked",
            Self::RemoteOutboxFailed => "remoteOutboxFailed",
            Self::TaskDone => "taskDone",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     snapshot 查询结果要把 kind 文本还原为枚举，损坏值应 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 camelCase kind；未知值返回业务错误。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "humanReview" => Ok(Self::HumanReview),
            "blocked" => Ok(Self::Blocked),
            "remoteOutboxFailed" => Ok(Self::RemoteOutboxFailed),
            "taskDone" => Ok(Self::TaskDone),
            other => Err(AppError::generic(format!("未知运营通知 kind: {other}"))),
        }
    }
}

/// 运营通知事件（event_bus / Tauri 中继 payload 主体）。
///
/// Business Logic（为什么需要这个结构）:
///     sidecar 在 HR/Blocked/Done/outboxFailed 真实转换后广播隐私安全事件，GUI 用
///     `{kind,opaqueSourceId,stateVersion}` 去重，不得包含任务标题/项目/goal/diff。
///
/// Code Logic（这个结构做什么）:
///     camelCase DTO：kind + opaqueSourceId + stateVersion + occurredAt。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalNotificationEvent {
    pub kind: OperationalNotificationKind,
    pub opaque_source_id: String,
    pub state_version: i64,
    pub occurred_at: String,
}

/// 运营通知 baseline snapshot（owner control）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 冷启动/Gap 需要用 owner 当前 opaque 状态建立 no-notify baseline，并绑定稳定 event cursor。
///
/// Code Logic（这个结构做什么）:
///     `asOfCursor` 为 capture 稳定时的 BackendRuntimeCursor；items ≤1000；truncated 表示截断。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalNotificationSnapshot {
    pub as_of_cursor: crate::backend::event_bus::BackendRuntimeCursor,
    pub items: Vec<OperationalNotificationEvent>,
    pub truncated: bool,
}

/// Orchestrator 任务尝试数据库行模型。
///
/// Business Logic（为什么需要这个结构体）:
///     自动验证/修复循环会为同一个任务产生多轮 Claude Code 开发尝试；系统需要保留每一轮可见 terminal、
///     prompt 和完成时间，供后续 sentinel、evidence 和任务详情追溯。
///
/// Code Logic（这个结构体做什么）:
///     字段与 orchestrator_task_attempts 表一一对应；status 目前保存为稳定字符串，后续 runner 接入时再收敛为枚举。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct OrchestratorTaskAttemptRow {
    pub id: String,
    pub task_id: String,
    pub attempt: i64,
    pub worktree_id: String,
    pub session_id: String,
    pub prompt: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Orchestrator 任务前端 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     前端页面后续需要 camelCase 字段和统一的枚举状态展示任务列表。
///
/// Code Logic（这个结构体做什么）:
///     从数据库 Row 投影为可序列化 DTO，字段名通过 serde 统一转 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct OrchestratorTaskDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance_criteria: String,
    pub status: OrchestratorTaskStatus,
    pub workflow_state: OrchestratorWorkflowState,
    pub run_state: OrchestratorRunState,
    pub attempt_phase: Option<OrchestratorAttemptPhase>,
    pub source: String,
    pub external_id: Option<String>,
    pub external_identifier: Option<String>,
    pub external_url: Option<String>,
    pub external_state: Option<String>,
    pub external_labels: Option<Vec<String>>,
    pub runner_provider: Option<String>,
    pub claude_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub runtime_started_at: Option<String>,
    pub last_activity_at: Option<String>,
    pub last_runtime_event: Option<String>,
    pub last_runtime_message: Option<String>,
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_claim_token: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// Orchestrator legacy 项目配置 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     历史版本曾把自动化配置写入项目级表；后端需要保留 DTO 以便兼容旧数据和调试读取。
///     用户可见配置入口已经迁移到 Settings 自动化 tab，运行时不再消费该 DTO。
///
/// Code Logic（这个结构体做什么）:
///     字段与 orchestrator_project_config 表一一对应，使用 camelCase 序列化给兼容/诊断接口。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct OrchestratorProjectConfigDto {
    pub project_id: String,
    pub enabled: bool,
    pub max_concurrent_tasks: i64,
    pub branch_prefix: String,
    pub verification_commands: Vec<String>,
    pub auto_commit: bool,
    pub auto_push_task_branch: bool,
    pub auto_merge_to_main: bool,
    pub auto_push_main: bool,
    pub retry_limit: i64,
    pub retain_worktree_on_done: bool,
    pub retain_worktree_on_blocked: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Orchestrator 任务证据 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Agent 完成后，验证命令输出需要作为可审计证据展示给用户，帮助用户判断任务是否可交付。
///
/// Code Logic（这个结构体做什么）:
///     字段与 orchestrator_task_evidence 表一一对应，使用 camelCase 序列化给前端。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct OrchestratorEvidenceDto {
    pub id: String,
    pub task_id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub created_at: String,
}

/// Human Review 有界 diff 快照。
///
/// Business Logic（为什么需要这个结构体）:
///     Human Review / Deliver 前需要展示任务 worktree 的只读改动，并用 digest 检测审阅后漂移。
///
/// Code Logic（这个结构体做什么）:
///     保存 task/base/head 身份、有界文件列表、总文件数、截断标记，以及与展示截断无关的 review_digest。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorReviewDiff {
    pub task_id: String,
    pub base_ref: String,
    pub head_ref: String,
    pub files: Vec<ReviewDiffFile>,
    pub total_files: u32,
    pub truncated: bool,
    pub review_digest: String,
}

/// Review diff 中的单文件条目。
///
/// Business Logic（为什么需要这个结构体）:
///     前端 Changes tab 需要路径、状态、增删统计和可选 patch；二进制与超限文件只能展示元数据。
///
/// Code Logic（这个结构体做什么）:
///     保存 repo-relative path、status、additions/deletions、可选 patch 与 binary/truncated 标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDiffFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub patch: Option<String>,
    pub binary: bool,
    pub truncated: bool,
}

impl OrchestratorTaskRow {
    /// Business Logic（为什么需要这个函数）:
    ///     split state 扩展后，既有命令和测试构造任务时需要统一的内部默认值，避免每个调用点重复填充运行元数据。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 legacy status 生成对应 workflow/run state，并填充 internal 来源、默认 runner provider 和空运行时字段。
    pub fn default_for_status(status: OrchestratorTaskStatus) -> Self {
        let split_state = SplitTaskState::from_legacy_status(status);
        Self {
            id: String::new(),
            project_id: String::new(),
            title: String::new(),
            goal: String::new(),
            acceptance_criteria: String::new(),
            status,
            workflow_state: split_state.workflow_state,
            run_state: split_state.run_state,
            attempt_phase: None,
            source: "internal".to_string(),
            external_id: None,
            external_identifier: None,
            external_url: None,
            external_state: None,
            external_labels: None,
            runner_provider: Some("claudeCodeVisible".to_string()),
            claude_session_id: None,
            agent_session_id: None,
            transcript_path: None,
            runtime_started_at: None,
            last_activity_at: None,
            last_runtime_event: None,
            last_runtime_message: None,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            prepare_claim_token: None,
            blocked_reason: None,
            attempt: 0,
            state_version: 0,
            created_at: String::new(),
            updated_at: String::new(),
            started_at: None,
            finished_at: None,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     命令层返回任务时不应暴露内部枚举和 snake_case 语义，需要统一 DTO 转换入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 Row 后复用 From<OrchestratorTaskRow>，保持 DTO 投影入口兼容旧调用点。
    #[allow(dead_code)]
    pub fn to_dto(&self) -> OrchestratorTaskDto {
        OrchestratorTaskDto::from(self.clone())
    }
}

impl OrchestratorTaskDto {
    /// Business Logic（为什么需要这个函数）:
    ///     协议和 outbox 单测需要构造 DTO 样本，split state 扩展后应与 Row 默认语义保持一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     根据 legacy status 生成默认 DTO 字段，调用点可用 struct update 覆盖业务字段。
    pub fn default_for_status(status: OrchestratorTaskStatus) -> Self {
        OrchestratorTaskDto::from(OrchestratorTaskRow::default_for_status(status))
    }
}

impl From<OrchestratorTaskRow> for OrchestratorTaskDto {
    /// Business Logic（为什么需要这个函数）:
    ///     命令层后续会在创建或列表查询后把任务 Row 直接转换为前端 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     消费 OrchestratorTaskRow 并逐字段投影，status 保持枚举类型交给 serde 输出。
    fn from(row: OrchestratorTaskRow) -> Self {
        OrchestratorTaskDto {
            id: row.id,
            project_id: row.project_id,
            title: row.title,
            goal: row.goal,
            acceptance_criteria: row.acceptance_criteria,
            status: row.status,
            workflow_state: row.workflow_state,
            run_state: row.run_state,
            attempt_phase: row.attempt_phase,
            source: row.source,
            external_id: row.external_id,
            external_identifier: row.external_identifier,
            external_url: row.external_url,
            external_state: row.external_state,
            external_labels: row.external_labels,
            runner_provider: row.runner_provider,
            claude_session_id: row.claude_session_id,
            agent_session_id: row.agent_session_id,
            transcript_path: row.transcript_path,
            runtime_started_at: row.runtime_started_at,
            last_activity_at: row.last_activity_at,
            last_runtime_event: row.last_runtime_event,
            last_runtime_message: row.last_runtime_message,
            priority: row.priority,
            branch_name: row.branch_name,
            worktree_id: row.worktree_id,
            session_id: row.session_id,
            prepare_claim_token: row.prepare_claim_token,
            blocked_reason: row.blocked_reason,
            attempt: row.attempt,
            created_at: row.created_at,
            updated_at: row.updated_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }
    }
}

#[cfg(test)]
mod split_state_tests {
    use super::*;

    #[test]
    fn legacy_status_maps_to_split_states() {
        let cases = [
            (
                OrchestratorTaskStatus::Draft,
                OrchestratorWorkflowState::Backlog,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Queued,
                OrchestratorWorkflowState::Todo,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Preparing,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Preparing,
            ),
            (
                OrchestratorTaskStatus::Running,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Running,
            ),
            (
                OrchestratorTaskStatus::Verifying,
                OrchestratorWorkflowState::InProgress,
                OrchestratorRunState::Verifying,
            ),
            (
                OrchestratorTaskStatus::Delivering,
                OrchestratorWorkflowState::Merging,
                OrchestratorRunState::Delivering,
            ),
            (
                OrchestratorTaskStatus::Done,
                OrchestratorWorkflowState::Done,
                OrchestratorRunState::Idle,
            ),
            (
                OrchestratorTaskStatus::Blocked,
                OrchestratorWorkflowState::Rework,
                OrchestratorRunState::Blocked,
            ),
            (
                OrchestratorTaskStatus::Aborted,
                OrchestratorWorkflowState::Canceled,
                OrchestratorRunState::Idle,
            ),
        ];

        for (legacy, workflow, run) in cases {
            let mapped = SplitTaskState::from_legacy_status(legacy);
            assert_eq!(mapped.workflow_state, workflow);
            assert_eq!(mapped.run_state, run);
        }
    }
}
