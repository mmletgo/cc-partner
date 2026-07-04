//! Orchestrator DTOs and row models.

#![allow(dead_code)]

use crate::error::AppError;
use serde::{Deserialize, Serialize};

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
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
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
    pub priority: i64,
    pub branch_name: Option<String>,
    pub worktree_id: Option<String>,
    pub session_id: Option<String>,
    pub blocked_reason: Option<String>,
    pub attempt: i64,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// Orchestrator 项目策略前端 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     每个 Workbench 项目都需要独立的自动编排策略，用户必须能看到当前项目是否启用自动执行、
///     并发数、分支前缀、验证命令和自动提交/推送/合并等关键开关。
///
/// Code Logic（这个结构体做什么）:
///     字段与 orchestrator_project_config 表一一对应，使用 camelCase 序列化给前端展示。
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

impl OrchestratorTaskRow {
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
            priority: row.priority,
            branch_name: row.branch_name,
            worktree_id: row.worktree_id,
            session_id: row.session_id,
            blocked_reason: row.blocked_reason,
            attempt: row.attempt,
            created_at: row.created_at,
            updated_at: row.updated_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
        }
    }
}
