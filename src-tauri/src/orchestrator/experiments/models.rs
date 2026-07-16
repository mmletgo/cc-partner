//! Automated Candidate Experiments 领域模型。
//!
//! Business Logic（为什么需要这个模块）:
//!     同一任务的多个 candidate 需要组级状态、结果与置信度契约，才能在
//!     比较、唯一交付与 Attention 决策之间共享稳定语义。
//!
//! Code Logic（这个模块做什么）:
//!     定义 ExperimentStatus / CandidateOutcome / ComparativeConfidence 及
//!     实验组、candidate、evidence、create request 的 Row/DTO 类型。

use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// 实验组生命周期状态。
///
/// Business Logic（为什么需要这个枚举）:
///     实验组从创建到比较、交付或决策的全过程需要稳定状态机，
///     供 reducer、delivery 与 Attention 共用。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase；SQLite 使用小写 snake 持久化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentStatus {
    Draft,
    Queued,
    Running,
    Comparing,
    WinnerReady,
    Delivering,
    Completed,
    NeedsDecision,
    Failed,
    Cancelled,
}

impl ExperimentStatus {
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite 必须用稳定字符串保存状态，避免调用点硬编码漂移。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回小写持久化字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Comparing => "comparing",
            Self::WinnerReady => "winner_ready",
            Self::Delivering => "delivering",
            Self::Completed => "completed",
            Self::NeedsDecision => "needs_decision",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     从数据库读取时必须 fail-closed：未知状态视为数据损坏，禁止静默回退。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析小写状态字符串；未知值返回业务错误。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "draft" => Ok(Self::Draft),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "comparing" => Ok(Self::Comparing),
            "winner_ready" => Ok(Self::WinnerReady),
            "delivering" => Ok(Self::Delivering),
            "completed" => Ok(Self::Completed),
            "needs_decision" => Ok(Self::NeedsDecision),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(AppError::generic(format!(
                "未知 Experiment 状态: {other}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     降级/取消与 Attention 投影需要区分终态与仍可推进的组。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Completed/Failed/Cancelled 为终态。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Candidate 在实验组中的结果。
///
/// Business Logic（为什么需要这个枚举）:
///     candidate 复用普通 task 状态机，但组级需要额外 outcome 标记
///     ready/winner/loser，才能隔离交付并驱动比较。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase；SQLite 小写 snake；仅允许一个 winner（DB partial unique）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CandidateOutcome {
    Pending,
    Running,
    CandidateReady,
    Rejected,
    Winner,
    Loser,
    Failed,
    Cancelled,
}

impl CandidateOutcome {
    /// Business Logic（为什么需要这个函数）:
    ///     outcome 持久化字符串必须全局唯一且稳定。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回小写持久化字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::CandidateReady => "candidate_ready",
            Self::Rejected => "rejected",
            Self::Winner => "winner",
            Self::Loser => "loser",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     未知 outcome 必须 fail-closed，防止把损坏行当 pending 继续调度。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析小写 outcome；未知值返回业务错误。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "candidate_ready" => Ok(Self::CandidateReady),
            "rejected" => Ok(Self::Rejected),
            "winner" => Ok(Self::Winner),
            "loser" => Ok(Self::Loser),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(AppError::generic(format!(
                "未知 CandidateOutcome: {other}"
            ))),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     组级 reduce 只关心已通过硬门禁的 ready candidate。
    ///
    /// Code Logic（这个函数做什么）:
    ///     CandidateReady 为 true。
    pub fn is_ready(self) -> bool {
        matches!(self, Self::CandidateReady)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     判断 candidate 是否已结束（不可再被 claim）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Winner/Loser/Failed/Cancelled/Rejected 为终态。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Winner | Self::Loser | Self::Failed | Self::Cancelled | Self::Rejected
        )
    }
}

/// 比较判定的置信度。
///
/// Business Logic（为什么需要这个枚举）:
///     只有 high 且 full-auto 时才能自动交付 winner；medium/low 必须进入组级决策。
///
/// Code Logic（这个枚举做什么）:
///     serde camelCase；SQLite 小写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ComparativeConfidence {
    High,
    Medium,
    Low,
}

impl ComparativeConfidence {
    /// Business Logic（为什么需要这个函数）:
    ///     置信度持久化与 wire 协议需要稳定字面量。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回小写字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     未知置信度禁止猜测为 high，必须 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 small 字面量；未知值返回错误。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            other => Err(AppError::generic(format!(
                "未知 ComparativeConfidence: {other}"
            ))),
        }
    }
}

/// 比较 judge 输出。
///
/// Business Logic（为什么需要这个结构体）:
///     reducer 需要统一结构消费唯一 winner、并列集合与风险说明。
///
/// Code Logic（这个结构体做什么）:
///     可选 winner、置信度、reason、风险列表与 tied task id。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparativeVerdict {
    pub winner_task_id: Option<String>,
    pub confidence: ComparativeConfidence,
    pub reason: String,
    pub risk_notes: Vec<String>,
    pub tied_task_ids: Vec<String>,
}

/// 实验组数据库行。
///
/// Business Logic（为什么需要这个结构体）:
///     组级进度、winner 与 version CAS 都依赖权威实验行。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_experiments` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorExperimentRow {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance: String,
    pub status: ExperimentStatus,
    pub selection_policy: String,
    pub max_parallel: i64,
    pub winner_task_id: Option<String>,
    pub selection_reason: Option<String>,
    pub confidence: Option<ComparativeConfidence>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// Candidate 链接行。
///
/// Business Logic（为什么需要这个结构体）:
///     每个 candidate 复用普通 task，但组级需要 ordinal/provider/outcome 元数据。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_experiment_candidates` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorExperimentCandidateRow {
    pub experiment_id: String,
    pub task_id: String,
    pub ordinal: i64,
    pub provider_id: String,
    pub strategy_label: String,
    pub outcome: CandidateOutcome,
    pub selection_metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 组级 evidence 行（不复制 child evidence）。
///
/// Business Logic（为什么需要这个结构体）:
///     比较判定、风险与状态转换需要组级审计轨迹，但禁止存完整 patch。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_experiment_evidence` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorExperimentEvidenceRow {
    pub id: String,
    pub experiment_id: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub created_at: String,
}

/// 创建请求幂等映射行。
///
/// Business Logic（为什么需要这个结构体）:
///     同一 clientRequestId + fingerprint 必须复用同一 experiment，
///     不同 fingerprint 必须 conflict。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_experiment_create_requests` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorExperimentCreateRequestRow {
    pub request_id: String,
    pub project_id: String,
    pub experiment_id: String,
    pub request_fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 实验组前端/API DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     桌面与 mobile 需要 camelCase 投影组级进度与推荐结果。
///
/// Code Logic（这个结构体做什么）:
///     从 Row 投影；含可选 candidates 列表供详情。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorExperimentDto {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance: String,
    pub status: ExperimentStatus,
    pub selection_policy: String,
    pub max_parallel: i64,
    pub winner_task_id: Option<String>,
    pub selection_reason: Option<String>,
    pub confidence: Option<ComparativeConfidence>,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<OrchestratorExperimentCandidateDto>>,
}

/// Candidate 前端 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     组详情需要展示每个 candidate 的 provider/strategy/outcome，不展示 diff。
///
/// Code Logic（这个结构体做什么）:
///     camelCase 投影 candidate 链接字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorExperimentCandidateDto {
    pub experiment_id: String,
    pub task_id: String,
    pub ordinal: i64,
    pub provider_id: String,
    pub strategy_label: String,
    pub outcome: CandidateOutcome,
    pub selection_metadata_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<OrchestratorExperimentRow> for OrchestratorExperimentDto {
    /// Business Logic（为什么需要这个函数）:
    ///     命令/路由层需要把权威行投影为前端 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐字段映射；candidates 默认 None。
    fn from(row: OrchestratorExperimentRow) -> Self {
        Self {
            id: row.id,
            project_id: row.project_id,
            title: row.title,
            goal: row.goal,
            acceptance: row.acceptance,
            status: row.status,
            selection_policy: row.selection_policy,
            max_parallel: row.max_parallel,
            winner_task_id: row.winner_task_id,
            selection_reason: row.selection_reason,
            confidence: row.confidence,
            version: row.version,
            created_at: row.created_at,
            updated_at: row.updated_at,
            candidates: None,
        }
    }
}

impl From<OrchestratorExperimentCandidateRow> for OrchestratorExperimentCandidateDto {
    /// Business Logic（为什么需要这个函数）:
    ///     详情接口需要 candidate 列表 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     逐字段映射。
    fn from(row: OrchestratorExperimentCandidateRow) -> Self {
        Self {
            experiment_id: row.experiment_id,
            task_id: row.task_id,
            ordinal: row.ordinal,
            provider_id: row.provider_id,
            strategy_label: row.strategy_label,
            outcome: row.outcome,
            selection_metadata_json: row.selection_metadata_json,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 创建 experiment 请求中的单个 candidate 规格。
///
/// Business Logic（为什么需要这个结构体）:
///     用户只选 provider/strategy，不指定 device 或 baseUrl。
///
/// Code Logic（这个结构体做什么）:
///     camelCase：providerId + strategyLabel。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentCandidateSpec {
    pub provider_id: String,
    pub strategy_label: String,
}

/// 创建 experiment 的幂等请求体。
///
/// Business Logic（为什么需要这个结构体）:
///     本地与远端共用同一创建合同；clientRequestId + fingerprint 保证原子幂等。
///
/// Code Logic（这个结构体做什么）:
///     2–8 candidates；maxParallel 由服务端钳制。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateExperimentRequest {
    pub client_request_id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance: String,
    pub max_parallel: u32,
    pub candidates: Vec<ExperimentCandidateSpec>,
}

/// 幂等创建结果。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方需区分首次插入与重放命中（是否触发 dispatch）。
///
/// Code Logic（这个结构体做什么）:
///     experiment DTO + newly_created 标志。
#[derive(Debug, Clone)]
pub struct IdempotentCreateExperimentOutcome {
    pub experiment: OrchestratorExperimentDto,
    pub newly_created: bool,
}

/// 组级 evidence kind：比较选择审查。
pub const EXPERIMENT_EVIDENCE_KIND_SELECTION_REVIEW: &str = "selectionReview";
/// 组级 evidence kind：状态转换。
pub const EXPERIMENT_EVIDENCE_KIND_STATUS_TRANSITION: &str = "statusTransition";
/// 默认 selection policy。
pub const EXPERIMENT_SELECTION_POLICY_COMPARATIVE: &str = "comparative";
/// candidate task 的 source 标记。
pub const EXPERIMENT_TASK_SOURCE: &str = "experiment";

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     枚举序列化契约漂移会破坏前端与 DB 兼容。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 as_str/from_str 往返与未知值 fail-closed。
    #[test]
    fn experiment_status_roundtrip_and_fail_closed() {
        for status in [
            ExperimentStatus::Draft,
            ExperimentStatus::Queued,
            ExperimentStatus::Running,
            ExperimentStatus::Comparing,
            ExperimentStatus::WinnerReady,
            ExperimentStatus::Delivering,
            ExperimentStatus::Completed,
            ExperimentStatus::NeedsDecision,
            ExperimentStatus::Failed,
            ExperimentStatus::Cancelled,
        ] {
            assert_eq!(
                ExperimentStatus::from_str(status.as_str()).unwrap(),
                status
            );
        }
        assert!(ExperimentStatus::from_str("unknown").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     winner 字符串必须与 partial unique index 字面量一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 CandidateOutcome::Winner.as_str() == "winner"。
    #[test]
    fn candidate_outcome_winner_literal_stable() {
        assert_eq!(CandidateOutcome::Winner.as_str(), "winner");
        assert_eq!(
            CandidateOutcome::from_str("candidate_ready").unwrap(),
            CandidateOutcome::CandidateReady
        );
        assert!(CandidateOutcome::from_str("WON").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     置信度 wire 契约必须稳定。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 high/medium/low 与 JSON camelCase。
    #[test]
    fn comparative_confidence_serde_stable() {
        let json = serde_json::to_string(&ComparativeConfidence::High).unwrap();
        assert_eq!(json, "\"high\"");
        assert_eq!(
            serde_json::from_str::<ComparativeConfidence>("\"medium\"").unwrap(),
            ComparativeConfidence::Medium
        );
    }
}
