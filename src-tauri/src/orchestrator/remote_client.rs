//! orchestrator/remote_client.rs — Orchestrator 远端 HTTP 客户端
//!
//! Business Logic（为什么需要这个模块）:
//!     本机打开 Workbench remote shortcut 时，Orchestrator 任务的创建、列表、evidence 和状态操作
//!     必须代理到项目所在设备执行。
//!
//! Code Logic（这个模块做什么）:
//!     封装 reqwest::Client，调用 `/api/orchestrator/...` 远端路由，并把网络、状态码与 JSON
//!     解析错误统一转换为简洁中文 AppError。

#![allow(dead_code)]

use crate::commands::prompt_optimizer::OrchestratorTaskPromptCompletionDto;
use crate::error::AppError;
use crate::net::peer_error::{parse_peer_response, PeerCallError};
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{OrchestratorEvidenceDto, OrchestratorTaskDto};
use crate::orchestrator::remote_protocol::{
    RemoteCompleteOrchestratorTaskPromptReq, RemoteCreateOrchestratorTaskReq, RemoteListTasksReq,
    RemoteOrchestratorConfigResp, RemoteOrchestratorEvidenceResp,
    RemoteOrchestratorProjectRefreshResp, RemoteOrchestratorTaskListResp, RemoteTaskReq,
    RemoteTaskReworkReq,
};
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;

const SHORT_REMOTE_ORCHESTRATOR_TIMEOUT_SECS: u64 = 15;
const LONG_REMOTE_ORCHESTRATOR_TIMEOUT_SECS: u64 = 120;
const REMOTE_ERROR_BODY_MAX_CHARS: usize = 240;

/// 远端请求超时类别。
///
/// Business Logic（为什么需要这个枚举）:
///     Orchestrator 既有列表/config 这类短读操作，也有创建和状态写入这类需要等待 SQLite 写入的操作。
///
/// Code Logic（这个枚举做什么）:
///     区分短请求与长请求，供每个 reqwest request 单独设置 timeout。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteRequestTimeoutKind {
    Short,
    Long,
}

/// Orchestrator 远端 HTTP 客户端。
///
/// Business Logic（为什么需要这个结构体）:
///     remote shortcut 的 Orchestrator 命令需要复用同一套 HTTP 调用与错误映射规则。
///
/// Code Logic（这个结构体做什么）:
///     持有 cloneable 的 `reqwest::Client`，对外提供 create/list/evidence/start/rework/deliver/cancel/refresh/config 方法。
#[derive(Clone)]
pub struct RemoteOrchestratorClient {
    client: reqwest::Client,
}

impl RemoteOrchestratorClient {
    /// 创建 Orchestrator 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次处理远端 Orchestrator 请求时需要一个可直接使用的客户端实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造不带全局超时的 reqwest client；每个请求按短/长操作单独设置 timeout。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("构造 Orchestrator 远端 reqwest Client 失败");
        Self { client }
    }

    /// 在远端项目中创建 Orchestrator 任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 创建任务时，权威任务行必须写入项目所在设备的 SQLite。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/create`，解析创建后的任务 DTO。
    pub async fn create_task(
        &self,
        base_url: &str,
        req: RemoteCreateOrchestratorTaskReq,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/tasks/create"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在远端设备上完善 Orchestrator 创建任务 Prompt。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     手机端选中 remote shortcut 时，AI 完善必须在项目所在设备执行，才能使用远端项目上下文。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/complete-prompt`，解析 title/goal/acceptanceCriteria DTO。
    pub async fn complete_prompt(
        &self,
        base_url: &str,
        req: RemoteCompleteOrchestratorTaskPromptReq,
    ) -> Result<OrchestratorTaskPromptCompletionDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/tasks/complete-prompt"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端项目 Orchestrator 任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 需要按远端 local projectId 展示任务队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/list`，解析 `{tasks}` 后返回内部 Vec。
    pub async fn list_tasks(
        &self,
        base_url: &str,
        project_id: &str,
    ) -> Result<Vec<OrchestratorTaskDto>, AppError> {
        let resp: RemoteOrchestratorTaskListResp = self
            .post_json(
                endpoint_url(base_url, "/api/orchestrator/tasks/list"),
                &RemoteListTasksReq {
                    project_id: project_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(resp.tasks)
    }

    /// 读取远端任务 evidence。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端任务详情要展示 owning device 上真实归档的验证与交付 evidence。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/evidence`，解析 `{evidence}` 后返回内部 Vec。
    pub async fn get_evidence(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<Vec<OrchestratorEvidenceDto>, AppError> {
        let resp: RemoteOrchestratorEvidenceResp = self
            .post_task_req(
                endpoint_url(base_url, "/api/orchestrator/tasks/evidence"),
                task_id,
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(resp.evidence)
    }

    /// 将远端草稿任务入队。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在 remote shortcut 上点击入队时，状态转换必须发生在项目所在设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/queue`，解析更新后的任务 DTO。
    pub async fn queue_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/queue"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 启动远端任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在 remote shortcut 上点击 Start 时，任务必须在 owning device 上进入 scheduler 可领取路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/start`，解析更新后的任务 DTO。
    pub async fn start_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/start"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 重试远端阻塞任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户处理 blocked 原因后，需要把 owning device 上的任务重新放回队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/retry`，解析更新后的任务 DTO。
    pub async fn retry_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/retry"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 请求远端任务返工。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     人工复核未通过时，返工原因必须写到项目所在设备的任务 evidence，而不是本机 mirror。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/request-rework`，携带 taskId/reason 并解析更新后的任务 DTO。
    pub async fn request_rework_task(
        &self,
        base_url: &str,
        task_id: &str,
        reason: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/tasks/request-rework"),
            &RemoteTaskReworkReq {
                task_id: task_id.to_string(),
                reason: reason.to_string(),
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 交付远端人工复核任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 上的显式交付必须由 owning device 检查 Settings 并运行 Git delivery pipeline。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/deliver-reviewed`，解析 delivery pipeline 返回的任务 DTO。
    pub async fn deliver_reviewed_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/deliver-reviewed"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 终止远端任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 abort 按钮必须终止项目所在设备上的权威任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/abort`，解析更新后的任务 DTO。
    pub async fn abort_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/abort"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 取消远端任务。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     显式 cancelTask 需要把 owning device 上的权威任务移到 Canceled/Idle，并保留执行现场。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/tasks/cancel`，解析更新后的任务 DTO。
    pub async fn cancel_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.post_task_req(
            endpoint_url(base_url, "/api/orchestrator/tasks/cancel"),
            task_id,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 刷新远端 Orchestrator 项目。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 刷新项目时，调度/reconcile 必须在项目所在设备执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/orchestrator/projects/refresh`，解析 `{projectId, dispatched}`。
    pub async fn refresh_project(
        &self,
        base_url: &str,
        project_id: &str,
    ) -> Result<RemoteOrchestratorProjectRefreshResp, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/projects/refresh"),
            &RemoteListTasksReq {
                project_id: project_id.to_string(),
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 读取远端设备 Orchestrator 全局自动化配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     诊断/兼容路径需要读取 owning device 的 scheduler/verification/delivery 全局配置。
    ///     OrchestratorPanel 不展示该配置，用户查看和编辑固定在对应设备的 Settings 自动化 tab。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET `{base_url}/api/orchestrator/config`，解析 `{config}` 后返回内部 DTO。
    pub async fn get_config(
        &self,
        base_url: &str,
    ) -> Result<OrchestratorAutomationConfigDto, AppError> {
        let resp: RemoteOrchestratorConfigResp = self
            .get_json(
                endpoint_url(base_url, "/api/orchestrator/config"),
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(resp.config)
    }

    /// 发送 taskId 请求。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     queue/retry/abort/evidence 都只携带 taskId，应复用同一请求体构造方式。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 RemoteTaskReq 后转发到 post_json。
    async fn post_task_req<T>(
        &self,
        url: String,
        task_id: &str,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        self.post_json(
            url,
            &RemoteTaskReq {
                task_id: task_id.to_string(),
            },
            timeout_kind,
        )
        .await
    }

    /// 发送 GET JSON 请求并解析响应。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多个远端只读接口需要统一超时、网络错误和响应解析文案。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 reqwest GET 设置 timeout 与出站 request_id header（Finding 3：多跳调用链关联），
    ///     发送后委托 parse_json_response。
    async fn get_json<T>(
        &self,
        url: String,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let response = self
            .client
            .get(&url)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                crate::net::request_context::new_request_id(),
            )
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Orchestrator 请求失败: {error}")))?;
        parse_json_response(response, &url).await
    }

    /// 发送 POST JSON 请求并解析响应。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 远端接口统一使用 JSON 请求体，错误文案应在所有方法间保持一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 reqwest POST 设置 JSON body、timeout 与出站 request_id header（Finding 3：多跳调用链关联），
    ///     发送后委托 parse_json_response。
    async fn post_json<T, B>(
        &self,
        url: String,
        body: &B,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .client
            .post(&url)
            .json(body)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                crate::net::request_context::new_request_id(),
            )
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Orchestrator 请求失败: {error}")))?;
        parse_json_response(response, &url).await
    }
}

impl Default for RemoteOrchestratorClient {
    /// 创建默认 Orchestrator 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方需要便捷构造客户端，同时保持与 new() 一致的超时策略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     直接转发到 RemoteOrchestratorClient::new。
    fn default() -> Self {
        Self::new()
    }
}

/// 返回远端请求 timeout。
///
/// Business Logic（为什么需要这个函数）:
///     短读接口应快速失败，写操作允许对端完成 SQLite 状态转移和任务创建。
///
/// Code Logic（这个函数做什么）:
///     根据 timeout kind 返回固定 Duration。
fn remote_request_timeout(kind: RemoteRequestTimeoutKind) -> Duration {
    match kind {
        RemoteRequestTimeoutKind::Short => {
            Duration::from_secs(SHORT_REMOTE_ORCHESTRATOR_TIMEOUT_SECS)
        }
        RemoteRequestTimeoutKind::Long => {
            Duration::from_secs(LONG_REMOTE_ORCHESTRATOR_TIMEOUT_SECS)
        }
    }
}

/// 拼接远端 API URL。
///
/// Business Logic（为什么需要这个函数）:
///     调用方可能传入带尾斜杠的 base URL，远端客户端应始终拼出唯一规范路径。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/`，再追加以 `/` 开头的 API path。
fn endpoint_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// 解析远端 JSON 响应。
///
/// Business Logic（为什么需要这个函数）:
///     所有远端 Orchestrator 响应都需要统一错误语义，避免各方法返回不同格式的错误文案。
///     Task 7 起改用共享的 `net::peer_error::parse_peer_response` 统一解析 v1 信封与 v0 老形态，
///     保证与 peer_client 行为一致；最终映射回命令层 `AppError`（保留原有 UI 文案）。
///
/// Code Logic（这个函数做什么）:
///     委托 `parse_peer_response` 一次性消费 status/header request_id/body bytes，
///     成功时按泛型解析 JSON；失败时按 `PeerCallError` 变体映射为 `AppError`。
async fn parse_json_response<T>(response: reqwest::Response, url: &str) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    parse_peer_response::<T>(response, url)
        .await
        .map_err(remote_error_to_app_error)
}

/// 把统一的 `PeerCallError` 映射为命令层 `AppError`。
///
/// Business Logic（为什么需要这个函数 / Finding 3）:
///     Orchestrator 命令层沿用 `AppError`；远端调用失败需转成可读的中文错误文案供 UI 展示，
///     同时**保留**对端信封的 code/status/retryable/request_id（旧实现把它们全部丢弃，
///     `classify()` 一律 Internal，重试只能靠文案匹配）。现委托共享
///     `peer_error::peer_call_error_to_app_error`，把 `Remote` 转为 `AppError::remote`，
///     `classify()` 据稳定 code 映射分类，`remote_meta()` 暴露 request_id/retryable。
///
/// Code Logic（这个函数做什么）:
///     委托 `peer_call_error_to_app_error(error, "远端 Orchestrator")`，文案前缀与原实现一致，
///     保证 UI 文案不回归。
fn remote_error_to_app_error(error: PeerCallError) -> AppError {
    crate::net::peer_error::peer_call_error_to_app_error(error, "远端 Orchestrator")
}

/// 截断远端错误正文。
///
/// Business Logic（为什么需要这个函数）:
///     远端非 JSON 错误可能包含代理 HTML 或长堆栈，完整回传会降低前端错误可读性。
///
/// Code Logic（这个函数做什么）:
///     按 Unicode char 截断错误正文，超长时追加省略号。
fn truncate_error_body(body: &str) -> String {
    let mut chars = body.chars();
    let truncated: String = chars.by_ref().take(REMOTE_ERROR_BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppErrorCategory;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Orchestrator client 需要复用统一 URL 拼接规则，避免 base_url 尾部斜杠造成双斜杠请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用带尾部斜杠的 base URL 拼接任务创建路径，断言输出 URL 正确。
    #[test]
    fn endpoint_url_trims_base_url_trailing_slash() {
        assert_eq!(
            endpoint_url("http://127.0.0.1:62116/", "/api/orchestrator/tasks/create"),
            "http://127.0.0.1:62116/api/orchestrator/tasks/create"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Orchestrator 业务错误由对端序列化为 `{error}`（v0 老形态）；Task 7 起改用共享
    ///     `parse_peer_response`，本机 UI 仍应展示对端原始中文文案，而不是 HTTP 状态包装。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回 400 + 老形态 `{error}`，调用 list_tasks，断言错误文案 == 对端 message。
    #[tokio::test]
    async fn remote_error_uses_peer_message_from_legacy_body() {
        let app = Router::new().route(
            "/api/orchestrator/tasks/list",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "远端 Orchestrator 项目不存在" })),
                )
            }),
        );
        let base_url = spawn_orchestrator_server(app).await;
        let err = RemoteOrchestratorClient::new()
            .list_tasks(&base_url, "project-1")
            .await
            .expect_err("400 老形态应失败");
        assert_eq!(err.to_string(), "远端 Orchestrator 项目不存在");
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     v1 对端返回完整错误信封时，客户端应展示对端 message（与 v0 行为一致），
    ///     且 code/status/retryable/request_id 必须被**保留**（不再被丢弃折叠成 Internal）。
    ///     旧实现把 503 unavailable 错误归类为 Internal，导致重试只能靠文案匹配——本测试
    ///     锁定修复后行为：classify() 据 code 映射为 Unavailable，remote_meta() 暴露全部字段。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回 503 + v1 信封（code=unavailable, retryable=true, request_id=req-1），
    ///     调用 list_tasks，断言：
    ///     - 文案 == message（UI 不回归）；
    ///     - classify() == Unavailable（据 code，不再 Internal）；
    ///     - remote_meta() 携带 code/status/retryable/request_id 全部字段。
    #[tokio::test]
    async fn remote_error_uses_peer_message_from_v1_envelope() {
        let app = Router::new().route(
            "/api/orchestrator/tasks/list",
            post(|| async {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": "对端调度忙",
                        "code": "unavailable",
                        "request_id": "req-1",
                        "retryable": true,
                        "details": {}
                    })),
                )
            }),
        );
        let base_url = spawn_orchestrator_server(app).await;
        let err = RemoteOrchestratorClient::new()
            .list_tasks(&base_url, "project-1")
            .await
            .expect_err("503 v1 信封应失败");
        assert_eq!(err.to_string(), "对端调度忙");
        // Finding 3: code=unavailable 必须映射为 Unavailable（不再 Internal）。
        assert_eq!(err.classify(), AppErrorCategory::Unavailable);
        // Finding 3: 结构化元数据必须被保留，供重试/调用链关联。
        let meta = err.remote_meta().expect("Remote 错误应携带 meta");
        assert_eq!(meta.code, "unavailable");
        assert_eq!(meta.status, 503);
        assert!(meta.retryable);
        assert_eq!(meta.request_id, "req-1");
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     不同 code token（conflict/validation_error/not_found/timeout/legacy.remote_error）
    ///     必须各自映射到对应分类，让上层用 `classify()` 而非文案做重试/合并决策。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对每类 code 构造 `AppError::remote`，断言 `classify()` 与 `remote_meta()` 一致。
    #[test]
    fn remote_variant_classifies_by_code_not_message() {
        use crate::error::{classify_remote_code, RemoteErrorMeta};
        let cases: Vec<(&str, AppErrorCategory)> = vec![
            ("validation_error", AppErrorCategory::Validation),
            ("unauthorized", AppErrorCategory::Validation),
            ("forbidden", AppErrorCategory::Validation),
            ("payload_too_large", AppErrorCategory::Validation),
            ("method_not_allowed", AppErrorCategory::Validation),
            ("not_found", AppErrorCategory::NotFound),
            ("conflict", AppErrorCategory::Conflict),
            ("unavailable", AppErrorCategory::Unavailable),
            ("timeout", AppErrorCategory::Timeout),
            // 兜底：legacy / internal / unknown / 未来 token → Internal。
            ("legacy.remote_error", AppErrorCategory::Internal),
            ("internal_error", AppErrorCategory::Internal),
            ("unknown", AppErrorCategory::Internal),
            ("future.token", AppErrorCategory::Internal),
        ];
        for (code, expected) in cases {
            // 纯函数入口直接校验。
            assert_eq!(classify_remote_code(code), expected, "code={code}");
            // 经 AppError::remote → classify 端到端校验。
            let app = AppError::remote(
                format!("msg-{code}"),
                RemoteErrorMeta {
                    code: code.to_string(),
                    status: 500,
                    retryable: false,
                    request_id: "r".to_string(),
                },
            );
            assert_eq!(app.classify(), expected, "AppError classify code={code}");
            assert_eq!(app.remote_meta().unwrap().code, code);
            // Display 只展示 message，不泄漏元数据。
            assert_eq!(app.to_string(), format!("msg-{code}"));
        }
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     v0 老形态（`{error}` 无 code）合成 code=`legacy.remote_error`，必须归类为 Internal
    ///     （不误升 4xx/5xx），且 message 取自对端原始文案，request_id 取自响应 header。
    ///     本测试锁定修复后行为，防止 legacy 被误映射为业务分类。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用纯函数 `peer_call_error_to_app_error` 直接喂一个合成的 legacy Remote 错误，
    ///     断言 classify()==Internal 且 meta.code==legacy.remote_error。
    #[test]
    fn legacy_remote_error_classifies_as_internal() {
        use crate::net::peer_error::peer_call_error_to_app_error;
        use crate::net::peer_error::PeerCallError;
        let err = PeerCallError::Remote {
            url: "http://1.2.3.4:8765/x".to_string(),
            status: 409,
            code: "legacy.remote_error".to_string(),
            message: "旧错误".to_string(),
            request_id: "hdr-1".to_string(),
            retryable: false,
            legacy: true,
        };
        let app = peer_call_error_to_app_error(err, "远端 Orchestrator");
        assert_eq!(app.classify(), AppErrorCategory::Internal);
        assert_eq!(app.to_string(), "旧错误");
        let meta = app.remote_meta().unwrap();
        assert_eq!(meta.code, "legacy.remote_error");
        assert_eq!(meta.status, 409);
        assert_eq!(meta.request_id, "hdr-1");
        assert!(!meta.retryable);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端代理或崩溃返回非 JSON 正文时，客户端应归为 InvalidResponse 并给出含 url 的可读错误，
    ///     不能把代理 500 误当业务失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回 502 + 纯文本 body，调用 list_tasks，断言错误为 generic 且含 url 上下文。
    #[tokio::test]
    async fn remote_non_json_body_becomes_invalid_response_error() {
        let app = Router::new().route(
            "/api/orchestrator/tasks/list",
            post(|| async { (StatusCode::BAD_GATEWAY, "upstream proxy down") }),
        );
        let base_url = spawn_orchestrator_server(app).await;
        let err = RemoteOrchestratorClient::new()
            .list_tasks(&base_url, "project-1")
            .await
            .expect_err("非 JSON 应失败");
        let msg = err.to_string();
        assert!(msg.contains("无法解析"), "应归为 InvalidResponse: {msg}");
        assert!(msg.contains("127.0.0.1"), "应含 url 上下文: {msg}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     capability gate 必须在缺失能力时**不**调用目标路由。用一个共享 hit 计数器挂在新路由上，
    ///     缺失能力时计数器应保持 0，证明 gate 提前返回（与 peer_client 行为对齐）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0 health 服务 + 一个带计数器的 orchestrator 新路由，
    ///     先用 peer_client.require_capability 拦截（返回 Unsupported），再断言新路由计数器为 0。
    #[tokio::test]
    async fn capability_gate_stops_before_orchestrator_route_when_unsupported() {
        use crate::net::peer_client::PeerClient;
        use crate::net::routes::health::HealthResponse;

        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "test".to_string(),
                        device_name: "test".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 0, // v0：不支持任何 v1 能力
                        capabilities: vec![],
                    })
                }),
            )
            .route(
                "/api/orchestrator/future-route",
                post(move || {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"ok": true}))
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;

        // 能力门拦截：v0 对端不支持 errors.envelope.v1。
        let gate_err = PeerClient::new()
            .require_capability(&base_url, "errors.envelope.v1")
            .await
            .expect_err("v0 应被能力门拦截");
        use crate::net::peer_client::PeerCallError;
        assert!(matches!(gate_err, PeerCallError::Unsupported { .. }));

        // 关键断言：缺失能力时新路由不应被调用（计数器为 0）。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "能力门未通过时不应调用新路由"
        );
    }

    /// Business Logic（为什么需要这个测试 / Finding 5）:
    ///     既有能力门测试用的是非生产 fixture 路由（`/api/orchestrator/future-route`），
    ///     无法证明门真的保护生产 caller。本测试把真实生产 caller
    ///     `RemoteOrchestratorClient::list_tasks`（经生产 `parse_json_response`→
    ///     `parse_peer_response` 链）挂在 `require_capability` 之后，验证：
    ///     1. v0 对端（无 errors.envelope.v1）被门拦截 → list_tasks 不被调用（hit 计数 0）；
    ///     2. v1 对端（有 errors.envelope.v1）通过门 → list_tasks 被调用且走完生产解析链。
    ///     这锁定了"能力门 + 生产 caller"的端到端契约。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0/v1 两套 health + 生产 tasks/list 路由（带 hit 计数器），
    ///     对每套先 `require_capability`，通过后再 `list_tasks`；断言 v0 拦截且 hit=0，
    ///     v1 通过且 hit=1 且响应正确解析。
    #[tokio::test]
    async fn capability_gate_protects_production_orchestrator_list_tasks_caller() {
        use crate::net::peer_client::{PeerCallError, PeerClient};
        use crate::net::routes::health::HealthResponse;
        use std::sync::atomic::{AtomicU32, Ordering};

        async fn spawn_gated_server(
            protocol_version: u32,
            capabilities: Vec<String>,
        ) -> (String, Arc<AtomicU32>) {
            let hits = Arc::new(AtomicU32::new(0));
            let hits_clone = hits.clone();
            let app = Router::new()
                .route(
                    "/api/health",
                    axum::routing::get(move || {
                        let caps = capabilities.clone();
                        async move {
                            Json(HealthResponse {
                                ok: true,
                                device_id: "test".to_string(),
                                device_name: "test".to_string(),
                                http_port: 8765,
                                ts: 1_700_000_000,
                                protocol_version,
                                capabilities: caps,
                            })
                        }
                    }),
                )
                .route(
                    "/api/orchestrator/tasks/list",
                    post(move || {
                        let hits = hits_clone.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            // 生产 caller 期望 `{tasks: [...]}` camelCase 包裹（RemoteOrchestratorTaskListResp）。
                            Json(serde_json::json!({ "tasks": [] }))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            (format!("http://{addr}"), hits)
        }

        // v0 对端：能力门拦截，生产 caller 不应被调用。
        let (v0_url, v0_hits) = spawn_gated_server(0, vec![]).await;
        let v0_gate = PeerClient::new()
            .require_capability(&v0_url, "errors.envelope.v1")
            .await;
        assert!(
            matches!(v0_gate, Err(PeerCallError::Unsupported { .. })),
            "v0 应被能力门拦截，实际: {v0_gate:?}"
        );
        // 门未通过，调用方不应继续打 list_tasks。
        assert_eq!(
            v0_hits.load(Ordering::SeqCst),
            0,
            "v0 能力门未通过时不应调用生产 list_tasks"
        );

        // v1 对端：能力门通过，生产 caller 应被调用且响应走完生产解析链。
        let (v1_url, v1_hits) = spawn_gated_server(1, vec!["errors.envelope.v1".to_string()]).await;
        PeerClient::new()
            .require_capability(&v1_url, "errors.envelope.v1")
            .await
            .expect("v1 应通过能力门");
        let tasks = RemoteOrchestratorClient::new()
            .list_tasks(&v1_url, "project-1")
            .await
            .expect("v1 生产 list_tasks 应成功");
        assert!(tasks.is_empty(), "fixture 返回空列表");
        assert_eq!(
            v1_hits.load(Ordering::SeqCst),
            1,
            "v1 通过能力门后应恰好调用一次生产 list_tasks"
        );
    }

    /// 启动临时 Orchestrator server（返回 base_url）。
    ///
    /// Code Logic: 绑定 127.0.0.1:0 由 OS 分配端口，后台 tokio::spawn 运行 axum::serve。
    async fn spawn_orchestrator_server(app: Router) -> String {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 HTML/堆栈正文可能很长，错误展示必须避免把超长正文整段塞进 UI。
    ///     truncate_error_body 现仅用于潜在的非 JSON 兜底（当前统一解析已覆盖），保留回归测试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入超过限制的正文，断言截断结果追加省略号且长度可控。
    #[test]
    fn truncate_error_body_adds_ellipsis_when_body_is_too_long() {
        let body = "x".repeat(REMOTE_ERROR_BODY_MAX_CHARS + 1);
        let truncated = truncate_error_body(&body);

        assert!(truncated.ends_with("..."));
        assert_eq!(truncated.chars().count(), REMOTE_ERROR_BODY_MAX_CHARS + 3);
    }
}
