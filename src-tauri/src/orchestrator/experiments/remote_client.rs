//! 远端 experiment 客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     remote shortcut 创建/列表/批准实验前必须检查 capability，旧 peer 不得降级普通 tasks。
//!
//! Code Logic（这个模块做什么）:
//!     经 PeerClient.require_capability 后用 reqwest POST 组级路由；
//!     绑定 expected_device_id 时 fail-closed：要求 device.request-binding.v1 且 health.device_id 精确匹配。

use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::{parse_peer_response, PeerCallError};
use crate::net::protocol::CAPABILITY_DEVICE_REQUEST_BINDING_V1;
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
///     绑定 expected_device_id 时，旧 peer 会忽略设备头并 fail-open；必须先确认对端宣告
///     device.request-binding.v1 且 health.device_id 精确匹配，否则不得发 mutation。
///
/// Code Logic（这个函数做什么）:
///     expected 为 None/空串时直接 Ok；否则 require_capability(binding) + device_id 精确匹配。
async fn ensure_device_binding(
    peer: &PeerClient,
    base: &str,
    expected: Option<&str>,
) -> Result<(), AppError> {
    let Some(expected) = expected.map(str::trim).filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let health = peer
        .require_capability(base, CAPABILITY_DEVICE_REQUEST_BINDING_V1)
        .await
        .map_err(map_peer_err)?;
    if health.device_id.trim() != expected {
        return Err(AppError::conflict(format!(
            "远端 experiment device_id 不匹配: expected={expected}, got={}",
            health.device_id
        )));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     远端创建必须 atomic 且 capability 门控。
///
/// Code Logic（这个函数做什么）:
///     require_capability(experiments) → ensure_device_binding → POST create。
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
    ensure_device_binding(peer, base_url, expected_device_id).await?;
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
///     require_capability(experiments) → ensure_device_binding → POST list。
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
    ensure_device_binding(peer, base_url, expected_device_id).await?;
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
///     require_capability(experiments) → ensure_device_binding → POST get。
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
    ensure_device_binding(peer, base_url, expected_device_id).await?;
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
///     require_capability(experiments) → ensure_device_binding → POST approve-winner。
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
    ensure_device_binding(peer, base_url, expected_device_id).await?;
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
///     require_capability(experiments) → ensure_device_binding → POST cancel。
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
    ensure_device_binding(peer, base_url, expected_device_id).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::routes::health::HealthResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// Business Logic（为什么需要这个函数）:
    ///     单测需要可绑定 ephemeral 端口的 mock peer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启动 axum serve 并返回 base_url。
    async fn spawn_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        format!("http://{addr}")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     绑定 expected_device_id 时，仅有 experiments.v1 而无 device.request-binding.v1 的旧 peer
    ///     必须 fail-closed，且不得触达 mutation 路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock health 只宣告 experiments.v1；create 路由带 hit 计数；
    ///     带 expected_device_id 调 create_remote_experiment → Err 且 hits==0。
    #[tokio::test]
    async fn expected_device_id_requires_device_request_binding_capability() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "device-owner".to_string(),
                        device_name: "owner".to_string(),
                        http_port: 1,
                        ts: 1,
                        protocol_version: 1,
                        capabilities: vec![CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1.to_string()],
                    })
                }),
            )
            .route(
                "/api/orchestrator/experiments/create",
                post(move || {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "experiment": {
                                "id": "exp-1",
                                "projectId": "p1",
                                "title": "t",
                                "goal": "g",
                                "status": "running",
                                "candidateCount": 2,
                                "candidates": [],
                                "createdAt": "2020-01-01T00:00:00Z",
                                "updatedAt": "2020-01-01T00:00:00Z"
                            }
                        }))
                    }
                }),
            );
        let base = spawn_server(app).await;
        let peer = PeerClient::new();
        let http = reqwest::Client::new();
        let req = CreateExperimentRequest {
            client_request_id: "req-1".to_string(),
            project_id: "p1".to_string(),
            title: "t".to_string(),
            goal: "g".to_string(),
            acceptance: "a".to_string(),
            max_parallel: 2,
            candidates: vec![],
        };
        let err = create_remote_experiment(&peer, &http, &base, &req, Some("device-owner"))
            .await
            .expect_err("missing binding capability must fail");
        let msg = err.to_string();
        assert!(
            msg.contains(CAPABILITY_DEVICE_REQUEST_BINDING_V1)
                || msg.contains("device.request-binding")
                || msg.contains("不支持"),
            "unexpected err: {msg}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "mutation must not be hit");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同时宣告 experiments + binding 且 device_id 匹配时，绑定路径应放行并触达路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock health 含两项能力；list 计数 +1；expected 匹配。
    #[tokio::test]
    async fn expected_device_id_passes_when_binding_capability_and_device_match() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "device-owner".to_string(),
                        device_name: "owner".to_string(),
                        http_port: 1,
                        ts: 1,
                        protocol_version: 1,
                        capabilities: vec![
                            CAPABILITY_ORCHESTRATOR_EXPERIMENTS_V1.to_string(),
                            CAPABILITY_DEVICE_REQUEST_BINDING_V1.to_string(),
                        ],
                    })
                }),
            )
            .route(
                "/api/orchestrator/experiments/list",
                post(move || {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({ "experiments": [] }))
                    }
                }),
            );
        let base = spawn_server(app).await;
        let peer = PeerClient::new();
        let http = reqwest::Client::new();
        let list = list_remote_experiments(&peer, &http, &base, "p1", Some("device-owner"))
            .await
            .expect("binding + match should pass");
        assert!(list.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
