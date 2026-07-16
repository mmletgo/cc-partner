//! 远端 experiment 客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     remote shortcut 创建/列表/批准实验前必须检查 capability，旧 peer 不得降级普通 tasks。
//!
//! Code Logic（这个模块做什么）:
//!     经 PeerClient.require_capability 后用 reqwest POST 组级路由；
//!     可选注入 `X-Cc-Partner-Expected-Device-Id` 防止端口被另一设备接管。

use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::{parse_peer_response, PeerCallError};
use crate::orchestrator::experiments::models::{
    CreateExperimentRequest, OrchestratorExperimentDto,
};
use crate::orchestrator::experiments::remote_protocol::{
    ApproveExperimentWinnerRequest, CancelExperimentRequest, CreateExperimentResponse,
    GetExperimentRequest, ListExperimentsRequest, ListExperimentsResponse,
    CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

const REMOTE_TIMEOUT_SECS: u64 = 30;

/// Business Logic（为什么需要这个函数）:
///     远端创建必须 atomic 且 capability 门控。
///
/// Code Logic（这个函数做什么）:
///     require_capability → POST /api/orchestrator/experiments/create；
///     可选 expected_device_id 注入设备头。
pub async fn create_remote_experiment(
    peer: &PeerClient,
    http: &reqwest::Client,
    base_url: &str,
    request: &CreateExperimentRequest,
    expected_device_id: Option<&str>,
) -> Result<CreateExperimentResponse, AppError> {
    peer.require_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1)
        .await
        .map_err(map_peer_err)?;
    post_json(
        http,
        base_url,
        "/api/orchestrator/experiments/create",
        request,
        expected_device_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     远端列表实验组。
///
/// Code Logic（这个函数做什么）:
///     require_capability → POST list；可选 expected_device_id 注入设备头。
pub async fn list_remote_experiments(
    peer: &PeerClient,
    http: &reqwest::Client,
    base_url: &str,
    project_id: &str,
    expected_device_id: Option<&str>,
) -> Result<Vec<OrchestratorExperimentDto>, AppError> {
    peer.require_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1)
        .await
        .map_err(map_peer_err)?;
    let resp: ListExperimentsResponse = post_json(
        http,
        base_url,
        "/api/orchestrator/experiments/list",
        &ListExperimentsRequest {
            project_id: project_id.to_string(),
        },
        expected_device_id,
    )
    .await?;
    Ok(resp.experiments)
}

/// Business Logic（为什么需要这个函数）:
///     远端详情。
///
/// Code Logic（这个函数做什么）:
///     require_capability → POST get；可选 expected_device_id 注入设备头。
pub async fn get_remote_experiment(
    peer: &PeerClient,
    http: &reqwest::Client,
    base_url: &str,
    experiment_id: &str,
    expected_device_id: Option<&str>,
) -> Result<OrchestratorExperimentDto, AppError> {
    peer.require_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1)
        .await
        .map_err(map_peer_err)?;
    post_json(
        http,
        base_url,
        "/api/orchestrator/experiments/get",
        &GetExperimentRequest {
            experiment_id: experiment_id.to_string(),
        },
        expected_device_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     远端批准 winner。
///
/// Code Logic（这个函数做什么）:
///     require_capability → POST approve-winner；可选 expected_device_id 注入设备头。
pub async fn approve_remote_experiment_winner(
    peer: &PeerClient,
    http: &reqwest::Client,
    base_url: &str,
    experiment_id: &str,
    winner_task_id: &str,
    reason: Option<String>,
    expected_device_id: Option<&str>,
) -> Result<OrchestratorExperimentDto, AppError> {
    peer.require_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1)
        .await
        .map_err(map_peer_err)?;
    post_json(
        http,
        base_url,
        "/api/orchestrator/experiments/approve-winner",
        &ApproveExperimentWinnerRequest {
            experiment_id: experiment_id.to_string(),
            winner_task_id: winner_task_id.to_string(),
            reason,
        },
        expected_device_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     远端取消实验组。
///
/// Code Logic（这个函数做什么）:
///     require_capability → POST cancel；可选 expected_device_id 注入设备头。
pub async fn cancel_remote_experiment(
    peer: &PeerClient,
    http: &reqwest::Client,
    base_url: &str,
    experiment_id: &str,
    expected_device_id: Option<&str>,
) -> Result<OrchestratorExperimentDto, AppError> {
    peer.require_capability(base_url, CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1)
        .await
        .map_err(map_peer_err)?;
    post_json(
        http,
        base_url,
        "/api/orchestrator/experiments/cancel",
        &CancelExperimentRequest {
            experiment_id: experiment_id.to_string(),
        },
        expected_device_id,
    )
    .await
}

/// Business Logic（为什么需要这个函数）:
///     统一 POST JSON 与错误解析；在 device 已知时绑定期望设备头。
///
/// Code Logic（这个函数做什么）:
///     reqwest post + 可选 EXPECTED_DEVICE_ID_HEADER + parse_peer_response。
async fn post_json<B: Serialize, R: DeserializeOwned>(
    http: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: &B,
    expected_device_id: Option<&str>,
) -> Result<R, AppError> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let mut req = http
        .post(&url)
        .timeout(Duration::from_secs(REMOTE_TIMEOUT_SECS))
        .json(body);
    if let Some(device_id) = expected_device_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        req = req.header(
            crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
            device_id,
        );
    }
    let response = req
        .send()
        .await
        .map_err(|err| AppError::generic(format!("远端 experiment 网络失败: {err}")))?;
    parse_peer_response(response, &url)
        .await
        .map_err(map_peer_err)
}

/// Business Logic（为什么需要这个函数）:
///     PeerCallError 映射为 AppError；Unsupported 保留 capability 信息。
///
/// Code Logic（这个函数做什么）:
///     match PeerCallError 变体。
fn map_peer_err(err: PeerCallError) -> AppError {
    match err {
        PeerCallError::Unsupported { capability, .. } => {
            AppError::generic(format!("对端不支持 {capability}，不能降级为普通 tasks"))
        }
        other => AppError::generic(format!("远端 experiment 调用失败: {other}")),
    }
}
