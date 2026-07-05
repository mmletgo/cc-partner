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

use crate::error::AppError;
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{OrchestratorEvidenceDto, OrchestratorTaskDto};
use crate::orchestrator::remote_protocol::{
    RemoteCreateOrchestratorTaskReq, RemoteListTasksReq, RemoteOrchestratorConfigResp,
    RemoteOrchestratorEvidenceResp, RemoteOrchestratorTaskListResp, RemoteTaskReq,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
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
///     持有 cloneable 的 `reqwest::Client`，对外提供 create/list/evidence/queue/retry/abort/config 方法。
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

    /// 读取远端设备 Orchestrator 全局自动化配置。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote UI 需要展示 owning device 的 scheduler/verification/delivery 策略。
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
    ///     对 reqwest GET 设置 timeout，发送后委托 parse_json_response。
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
            .get(url)
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Orchestrator 请求失败: {error}")))?;
        parse_json_response(response).await
    }

    /// 发送 POST JSON 请求并解析响应。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 远端接口统一使用 JSON 请求体，错误文案应在所有方法间保持一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 reqwest POST 设置 JSON body 和 timeout，发送后委托 parse_json_response。
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
            .post(url)
            .json(body)
            .timeout(remote_request_timeout(timeout_kind))
            .send()
            .await
            .map_err(|error| AppError::generic(format!("远端 Orchestrator 请求失败: {error}")))?;
        parse_json_response(response).await
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
///
/// Code Logic（这个函数做什么）:
///     检查 HTTP 2xx 状态；非 2xx 返回 `AppError::generic`；成功时按泛型解析 JSON。
async fn parse_json_response<T>(response: reqwest::Response) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::generic(remote_error_message(status, &body)));
    }
    response
        .json::<T>()
        .await
        .map_err(|error| AppError::generic(format!("远端 Orchestrator 响应解析失败: {error}")))
}

/// 提取远端错误文案。
///
/// Business Logic（为什么需要这个函数）:
///     远端业务错误通常由对端 AppError 序列化为 `{error}`，本机应保留原始业务文案。
///
/// Code Logic（这个函数做什么）:
///     优先从 JSON body 读取非空 error 字段；否则返回 HTTP 状态与截断后的正文摘要。
fn remote_error_message(status: reqwest::StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            let error = error.trim();
            if !error.is_empty() {
                return error.to_string();
            }
        }
    }
    if trimmed.is_empty() {
        return format!("远端 Orchestrator 请求失败: HTTP {status}");
    }
    format!(
        "远端 Orchestrator 请求失败: HTTP {status}: {}",
        truncate_error_body(trimmed)
    )
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
    ///     远端 Orchestrator 业务错误由对端序列化为 `{error}`，本机 UI 应展示对端原始中文文案。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用错误解析 helper，断言 JSON error 字段优先于 HTTP 状态包装。
    #[test]
    fn remote_error_message_prefers_json_error_field() {
        let message = remote_error_message(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"远端 Orchestrator 项目不存在"}"#,
        );

        assert_eq!(message, "远端 Orchestrator 项目不存在");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端代理或崩溃时可能返回非 JSON 正文，客户端仍要给出可读且有限长度的错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入非 JSON 正文，断言错误包含 HTTP 状态和正文摘要。
    #[test]
    fn remote_error_message_falls_back_to_status_and_body() {
        let message = remote_error_message(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");

        assert_eq!(
            message,
            "远端 Orchestrator 请求失败: HTTP 500 Internal Server Error: boom"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 HTML/堆栈正文可能很长，错误展示必须避免把超长正文整段塞进 UI。
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
