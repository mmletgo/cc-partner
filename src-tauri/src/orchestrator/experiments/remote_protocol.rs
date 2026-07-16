//! 实验组 P2P 请求/响应 DTO。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端创建/列表/详情/批准 winner/取消必须走组级原子合同，不降级为 N 条 task。
//!
//! Code Logic（这个模块做什么）:
//!     定义 camelCase wire DTO 与 capability token。

use crate::orchestrator::experiments::models::{
    ComparativeConfidence, CreateExperimentRequest, OrchestratorExperimentDto,
};
use serde::{Deserialize, Serialize};

/// 能力 token：`orchestrator.experiments.v1`。
pub const CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1: &str = "orchestrator.experiments.v1";

/// 创建实验响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateExperimentResponse {
    pub experiment: OrchestratorExperimentDto,
    pub newly_created: bool,
}

/// 列表请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListExperimentsRequest {
    pub project_id: String,
}

/// 列表响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListExperimentsResponse {
    pub experiments: Vec<OrchestratorExperimentDto>,
}

/// 详情请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GetExperimentRequest {
    pub experiment_id: String,
}

/// 批准 winner 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApproveExperimentWinnerRequest {
    pub experiment_id: String,
    pub winner_task_id: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// 取消实验请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelExperimentRequest {
    pub experiment_id: String,
}

/// 创建请求 re-export 供 route 使用。
pub type RemoteCreateExperimentRequest = CreateExperimentRequest;

/// Business Logic（为什么需要这个函数）:
///     健康检查与 client 协商需要稳定 capability 字符串。
///
/// Code Logic（这个函数做什么）:
///     返回 CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1。
pub fn experiments_capability() -> &'static str {
    CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_token_stable() {
        assert_eq!(
            CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1,
            "orchestrator.experiments.v1"
        );
    }

    #[test]
    fn create_request_roundtrip() {
        let req = CreateExperimentRequest {
            client_request_id: "r1".to_string(),
            project_id: "p".to_string(),
            title: "t".to_string(),
            goal: "g".to_string(),
            acceptance: "a".to_string(),
            max_parallel: 2,
            candidates: vec![
                crate::orchestrator::experiments::models::ExperimentCandidateSpec {
                    provider_id: "claudeCodeVisible".to_string(),
                    strategy_label: "min".to_string(),
                },
                crate::orchestrator::experiments::models::ExperimentCandidateSpec {
                    provider_id: "codexVisible".to_string(),
                    strategy_label: "ref".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("clientRequestId"));
        let back: CreateExperimentRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.candidates.len(), 2);
        let _ = ComparativeConfidence::High;
    }
}
