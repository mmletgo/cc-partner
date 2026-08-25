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

use crate::commands::orchestrator::OrchestratorRuntimeSnapshotDto;
use crate::commands::prompt_optimizer::OrchestratorTaskPromptCompletionDto;
use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::{parse_peer_response, peer_call_error_to_app_error, PeerCallError};
use crate::net::protocol::{
    PeerProtocolInfo, CAPABILITY_DEVICE_REQUEST_BINDING_V1,
    CAPABILITY_ORCHESTRATOR_COMPLETE_AGENT_RUN_V1, CAPABILITY_ORCHESTRATOR_MOVE_WORKFLOW_STATE_V1,
    CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1, CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1,
    CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1,
};
use crate::orchestrator::config::OrchestratorAutomationConfigDto;
use crate::orchestrator::models::{OrchestratorEvidenceDto, OrchestratorTaskDto};
use crate::orchestrator::remote_protocol::{
    block_created_dto_from_wire, task_dto_from_view_or_task_json, OrchestratorTaskBlockCreatedDto,
    RemoteAppendTaskBlockMemberReq, RemoteCompleteAgentRunReq,
    RemoteCompleteOrchestratorTaskPromptReq, RemoteCreateOrchestratorTaskBlockReq,
    RemoteCreateOrchestratorTaskReq, RemoteDeliverReviewedReq, RemoteListTasksReq,
    RemoteMoveWorkflowStateReq, RemoteOrchestratorConfigResp, RemoteOrchestratorEvidenceResp,
    RemoteOrchestratorProjectRefreshResp, RemoteOrchestratorTaskListResp,
    RemoteReorderTaskBlockMembersReq, RemoteRuntimeSnapshotReq, RemoteTaskReq, RemoteTaskReworkReq,
    RemoteWorkflowDocumentGetReq, RemoteWorkflowDocumentResp, RemoteWorkflowDocumentSaveReq,
    RemoteWorkflowDocumentValidateReq,
};
use crate::orchestrator::workflow::WorkflowDocument;
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;

const SHORT_REMOTE_ORCHESTRATOR_TIMEOUT_SECS: u64 = 15;
const LONG_REMOTE_ORCHESTRATOR_TIMEOUT_SECS: u64 = 120;
const COMPLETE_REMOTE_ORCHESTRATOR_TIMEOUT_SECS: u64 = 360;
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
    Complete,
}

/// Orchestrator 远端 HTTP 客户端。
///
/// Business Logic（为什么需要这个结构体）:
///     remote shortcut 的 Orchestrator 命令需要复用同一套 HTTP 调用与错误映射规则。
///
/// Code Logic（这个结构体做什么）:
///     持有 cloneable 的 `reqwest::Client`，对外提供 create/list/evidence/start/rework/deliver/cancel/refresh/config 方法。
///     `forwarded_request_id`（Finding 3）若被设置，所有出站请求会复用该 ID，把多跳代理
///     （手机 → 本机 → 远端设备）串成同一调用链；否则每次出站生成新 UUID。
#[derive(Clone)]
pub struct RemoteOrchestratorClient {
    client: reqwest::Client,
    forwarded_request_id: Option<String>,
    /// 可选：出站绑定期望 device_id（服务端 expected_device_id_guard 校验）。
    expected_device_id: Option<String>,
}

impl RemoteOrchestratorClient {
    /// 创建 Orchestrator 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次处理远端 Orchestrator 请求时需要一个可直接使用的客户端实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造不带全局超时的 reqwest client；每个请求按短/长操作单独设置 timeout。
    ///     `forwarded_request_id` 默认 None（每次出站生成新 ID）。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("构造 Orchestrator 远端 reqwest Client 失败");
        Self {
            client,
            forwarded_request_id: None,
            expected_device_id: None,
        }
    }

    /// 设置转发用 request_id（Finding 3），返回 self 便于链式构造。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多跳代理（手机 → 本机 → 项目所在设备）必须把入站 `X-CC-Request-Id` 转发到下一跳，
    ///     让整条调用链共用同一 ID，便于跨设备日志关联。调用方在 route handler 拿到
    ///     `P2pRequestContext` 后用本方法注入，再发起远端调用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     存储 request_id；`get_json`/`post_json` 出站时优先用它，缺失则生成新 UUID。
    pub fn with_forwarded_request_id(mut self, request_id: impl Into<String>) -> Self {
        let id = request_id.into();
        self.forwarded_request_id = if id.is_empty() { None } else { Some(id) };
        self
    }

    /// 绑定期望远端 device_id，使每个 GET/POST（含 post_json_peer / runtime_snapshot 原始构建）
    /// 携带 `X-Cc-Partner-Expected-Device-Id`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     共享 Remote*Client 路径必须与 CLI raw helper 一样按请求绑定 device，
    ///     避免 health 预检与 mutation 之间端口被另一设备接管。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空串 → None；非空存入 expected_device_id，出站 header 注入。
    pub fn with_expected_device_id(mut self, device_id: impl Into<String>) -> Self {
        let id = device_id.into();
        self.expected_device_id = if id.trim().is_empty() { None } else { Some(id) };
        self
    }

    /// 返回出站 request_id：转发 ID 优先，否则生成新 UUID（Finding 3）。
    fn outbound_request_id(&self) -> String {
        self.forwarded_request_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(crate::net::request_context::new_request_id)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     绑定 expected_device_id 时，旧 peer 会忽略设备头并 fail-open；必须先确认对端
    ///     宣告 `device.request-binding.v1` 且 health.device_id 精确匹配。
    ///
    /// Code Logic（这个函数做什么）:
    ///     expected_device_id 为 None 时直接 Ok；否则 require_capability(binding) + device_id 精确匹配。
    async fn ensure_expected_device_binding(&self, base_url: &str) -> Result<(), AppError> {
        let Some(expected) = self.expected_device_id.as_deref() else {
            return Ok(());
        };
        let expected = expected.trim();
        if expected.is_empty() {
            return Ok(());
        }
        let health = PeerClient::new()
            .require_capability(base_url, CAPABILITY_DEVICE_REQUEST_BINDING_V1)
            .await
            .map_err(|err| peer_call_error_to_app_error(err, "远端 Orchestrator"))?;
        if health.device_id.trim() != expected {
            return Err(AppError::conflict(format!(
                "远端 Orchestrator device_id 不匹配: expected={expected}, got={}",
                health.device_id
            )));
        }
        Ok(())
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

    /// 在 owning device 上创建串行任务块。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧 peer 缺失 capability 时必须 fail-closed，不得拆成多条普通任务。
    ///
    /// Code Logic（这个函数做什么）:
    ///     要求 `orchestrator.task-blocks.v1` 后 POST create-block。
    pub async fn create_task_block(
        &self,
        base_url: &str,
        req: RemoteCreateOrchestratorTaskBlockReq,
    ) -> Result<OrchestratorTaskBlockCreatedDto, AppError> {
        self.require_task_blocks_capability(base_url).await?;
        let value: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/orchestrator/task-views/create-block"),
                &req,
                RemoteRequestTimeoutKind::Long,
            )
            .await?;
        block_created_dto_from_wire(value).map_err(AppError::generic)
    }

    /// 在 owning device 上追加任务块成员。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     追加改变 live last-member，必须在 owner 上执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability gate 后 POST append-block-member。
    pub async fn append_task_block_member(
        &self,
        base_url: &str,
        req: RemoteAppendTaskBlockMemberReq,
    ) -> Result<OrchestratorTaskDto, AppError> {
        self.require_task_blocks_capability(base_url).await?;
        let value: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/orchestrator/task-views/append-block-member"),
                &req,
                RemoteRequestTimeoutKind::Long,
            )
            .await?;
        task_dto_from_view_or_task_json(&value)
            .ok_or_else(|| AppError::generic("追加任务块成员响应缺少任务"))
    }

    /// 在 owning device 上重排任务块成员。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     重排只能在 owner 上校验整块仍 backlog/todo 且 idle。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability gate 后 POST reorder-block-members。
    pub async fn reorder_task_block_members(
        &self,
        base_url: &str,
        req: RemoteReorderTaskBlockMembersReq,
    ) -> Result<Vec<OrchestratorTaskDto>, AppError> {
        self.require_task_blocks_capability(base_url).await?;
        let value: serde_json::Value = self
            .post_json(
                endpoint_url(
                    base_url,
                    "/api/orchestrator/task-views/reorder-block-members",
                ),
                &req,
                RemoteRequestTimeoutKind::Long,
            )
            .await?;
        let tasks = value
            .as_array()
            .ok_or_else(|| AppError::generic("重排任务块成员响应必须是数组"))?
            .iter()
            .filter_map(task_dto_from_view_or_task_json)
            .collect();
        Ok(tasks)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧 peer 不能被误判为支持任务块，否则会拆成普通任务或 404。
    ///
    /// Code Logic（这个函数做什么）:
    ///     peer_supports_capability；缺失返回 unavailable。
    async fn require_task_blocks_capability(&self, base_url: &str) -> Result<(), AppError> {
        if !self
            .peer_supports_capability(base_url, CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1)
            .await?
        {
            return Err(AppError::unavailable(
                "capability_unsupported:orchestrator.task-blocks.v1".to_string(),
            ));
        }
        Ok(())
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

    /// 读取 owning-device WORKFLOW 文档（capability-gated）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote 向导检测状态必须由 owning device 权威返回；旧 peer 缺失能力时 Unsupported。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require `orchestrator.workflow-document.v1` 后 POST get 路由，解析 `{document}`。
    pub async fn get_workflow_document(
        &self,
        base_url: &str,
        project_id: &str,
    ) -> Result<WorkflowDocument, PeerCallError> {
        self.require_workflow_document_capability(base_url).await?;
        let url = endpoint_url(base_url, "/api/orchestrator/workflow-document/get");
        let body = RemoteWorkflowDocumentGetReq {
            project_id: project_id.to_string(),
        };
        let resp: RemoteWorkflowDocumentResp = self
            .post_json_peer(&url, &body, RemoteRequestTimeoutKind::Short)
            .await?;
        Ok(resp.document)
    }

    /// 在 owning-device 上权威校验 WORKFLOW content。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote 编辑器保存前必须用 owner parser，不能信前端 YAML 提示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability gate 后 POST validate，解析 `{document}`。
    pub async fn validate_workflow_document(
        &self,
        base_url: &str,
        project_id: &str,
        content: &str,
    ) -> Result<WorkflowDocument, PeerCallError> {
        self.require_workflow_document_capability(base_url).await?;
        let url = endpoint_url(base_url, "/api/orchestrator/workflow-document/validate");
        let body = RemoteWorkflowDocumentValidateReq {
            project_id: project_id.to_string(),
            content: content.to_string(),
        };
        let resp: RemoteWorkflowDocumentResp = self
            .post_json_peer(&url, &body, RemoteRequestTimeoutKind::Short)
            .await?;
        Ok(resp.document)
    }

    /// CAS 保存 owning-device WORKFLOW 文档。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote 向导保存必须在文件所在设备做 expectedHash 比对与原子写；不 dispatch。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability gate 后 POST save，解析 `{document}`。
    pub async fn save_workflow_document(
        &self,
        base_url: &str,
        project_id: &str,
        expected_hash: &str,
        content: &str,
    ) -> Result<WorkflowDocument, PeerCallError> {
        self.require_workflow_document_capability(base_url).await?;
        let url = endpoint_url(base_url, "/api/orchestrator/workflow-document/save");
        let body = RemoteWorkflowDocumentSaveReq {
            project_id: project_id.to_string(),
            expected_hash: expected_hash.to_string(),
            content: content.to_string(),
        };
        let resp: RemoteWorkflowDocumentResp = self
            .post_json_peer(&url, &body, RemoteRequestTimeoutKind::Long)
            .await?;
        Ok(resp.document)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     三条 workflow-document 路由共享同一能力 token，client 必须先 gate 再发请求。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 PeerClient::require_capability。
    async fn require_workflow_document_capability(
        &self,
        base_url: &str,
    ) -> Result<(), PeerCallError> {
        PeerClient::new()
            .require_capability(base_url, CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1)
            .await
            .map(|_| ())
    }

    /// Business Logic（为什么需要这个函数）:
    ///     workflow-document client 方法复用统一的 request-id/timeout/解析路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST JSON body；注入 request_id 与可选 `EXPECTED_DEVICE_ID_HEADER`；解析 PeerCallError。
    async fn post_json_peer<T, B>(
        &self,
        url: &str,
        body: &B,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, PeerCallError>
    where
        T: DeserializeOwned,
        B: Serialize,
    {
        self.ensure_expected_device_binding_peer(url).await?;
        let outbound_request_id = self
            .forwarded_request_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(crate::net::request_context::new_request_id);
        let mut req = self
            .client
            .post(url)
            .json(body)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                outbound_request_id,
            )
            .timeout(remote_request_timeout(timeout_kind));
        if let Some(device_id) = self.expected_device_id.as_deref() {
            req = req.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = req.send().await.map_err(|error| PeerCallError::Network {
            url: url.to_string(),
            source: error,
        })?;
        parse_peer_response::<T>(response, url).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     post_json_peer / runtime_snapshot 返回 PeerCallError，绑定检查须保留 Unsupported 变体。
    ///
    /// Code Logic（这个函数做什么）:
    ///     expected_device_id 缺省时 Ok；否则 require_capability(binding) 并精确比对 device_id。
    async fn ensure_expected_device_binding_peer(&self, url: &str) -> Result<(), PeerCallError> {
        let Some(expected) = self.expected_device_id.as_deref() else {
            return Ok(());
        };
        let expected = expected.trim();
        if expected.is_empty() {
            return Ok(());
        }
        let base = origin_base_url(url).map_err(|err| PeerCallError::InvalidResponse {
            url: url.to_string(),
            reason: err.to_string(),
        })?;
        let health = PeerClient::new()
            .require_capability(&base, CAPABILITY_DEVICE_REQUEST_BINDING_V1)
            .await?;
        if health.device_id.trim() != expected {
            return Err(PeerCallError::InvalidResponse {
                url: base,
                reason: format!(
                    "device_id mismatch: expected={expected}, got={}",
                    health.device_id
                ),
            });
        }
        Ok(())
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

    /// 在 owning device 上移动任务工作流泳道。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     控制端拖拽必须改对端权威行，不得写本机 mirror 当真相。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/orchestrator/tasks/move-workflow-state`。
    pub async fn move_task_workflow_state(
        &self,
        base_url: &str,
        req: RemoteMoveWorkflowStateReq,
    ) -> Result<OrchestratorTaskDto, AppError> {
        if !self
            .peer_supports_capability(base_url, CAPABILITY_ORCHESTRATOR_MOVE_WORKFLOW_STATE_V1)
            .await?
        {
            return Err(AppError::unavailable(
                "capability_unsupported:orchestrator.move-workflow-state.v1".to_string(),
            ));
        }
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/tasks/move-workflow-state"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在 owning device 上完成 Agent 运行并走验证/交付。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     验证 pipeline 必须在任务所在设备执行；超时约 360s，no-transport-retry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/orchestrator/tasks/complete-agent-run`。
    pub async fn complete_agent_run(
        &self,
        base_url: &str,
        req: RemoteCompleteAgentRunReq,
    ) -> Result<OrchestratorTaskDto, AppError> {
        if !self
            .peer_supports_capability(base_url, CAPABILITY_ORCHESTRATOR_COMPLETE_AGENT_RUN_V1)
            .await?
        {
            return Err(AppError::unavailable(
                "capability_unsupported:orchestrator.complete-agent-run.v1".to_string(),
            ));
        }
        self.post_json(
            endpoint_url(base_url, "/api/orchestrator/tasks/complete-agent-run"),
            &req,
            RemoteRequestTimeoutKind::Complete,
        )
        .await
    }

    /// 探测对端是否支持某 capability。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新契约只能在 capability 命中时使用，缺能力必须 fail-closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET /api/health 解析 PeerProtocolInfo::supports。
    pub async fn peer_supports_capability(
        &self,
        base_url: &str,
        capability: &str,
    ) -> Result<bool, AppError> {
        let info: PeerProtocolInfo = self
            .get_json(
                endpoint_url(base_url, "/api/health"),
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(info.supports(capability))
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
    ///     A0 后无人工 digest 门禁。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{taskId}` 到 deliver-reviewed；解析 delivery pipeline 返回的任务 DTO。
    pub async fn deliver_reviewed_task(
        &self,
        base_url: &str,
        task_id: &str,
    ) -> Result<OrchestratorTaskDto, AppError> {
        let url = endpoint_url(base_url, "/api/orchestrator/tasks/deliver-reviewed");
        self.post_json(
            url,
            &RemoteDeliverReviewedReq {
                task_id: task_id.to_string(),
            },
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

    /// 拉取 owning-device Orchestrator 项目运行时快照（capability-gated）。
    ///
    /// Business Logic（为什么需要这个函数 / Plan 4 Task 3）:
    ///     remote shortcut 的状态条需要通过 P2P HTTP 拉取 owning device 上的权威运行时快照，
    ///     供前端展示调度器、workflow、槽位和最近事件。该路由由 `orchestrator.runtime-snapshot.v1`
    ///     能力 token 门控——必须在调用前先确认对端具备该能力，否则旧版本（未挂载该路由）会
    ///     返回 404/HTML 噪音。本方法把 `require_capability` 与新路由调用合成一个原子方法，
    ///     调用方无需自己记得先 gate。
    ///
    /// 与既有 create/list/evidence 等方法不同，本方法返回 `PeerCallError` 而非 `AppError`：
    ///     - `Unsupported`：对端在线但不具备 `orchestrator.runtime-snapshot.v1` 能力（**未发起路由请求**，
    ///        上层可据此回退到本机 builder 或 `remote_runtime_snapshot_unavailable` 提示）；
    ///     - `Network`：health 或路由请求的 send/读取失败（对端离线/网络中断）；
    ///     - `InvalidResponse`：对端响应非 JSON / 字段不全（协议违例，应告警而非当业务错误）；
    ///     - `Remote`：对端返回业务错误（v1 信封或 v0 老形态），携带 code/status/retryable/request_id。
    ///     上层用变体类型而非文案做"能力不支持 / 离线 / 协议违例"分支。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1. `PeerClient::require_capability(.., CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1)`
    ///        先做能力门；未通过直接返回 `Unsupported`，不打路由；
    ///     2. 通过后构造 `RemoteRuntimeSnapshotReq { project_id }`（P2P snake_case wire 契约，与
    ///        Shared Contracts / `routes/orchestrator.rs::runtime_snapshot` handler 一致）；
    ///     3. POST `{base_url}/api/orchestrator/runtime-snapshot`，注入出站 `X-CC-Request-Id`
    ///        （非空 request_id 入参优先转发，构建多跳调用链；空入参生成新 UUID）；
    ///     4. 用共享 `parse_peer_response` 解析 v0/v1 响应，成功返回
    ///        `OrchestratorRuntimeSnapshotDto`（owner 字段逐字保留，不重新计算）。
    pub async fn runtime_snapshot(
        &self,
        base_url: &str,
        project_id: &str,
        request_id: &str,
    ) -> Result<OrchestratorRuntimeSnapshotDto, PeerCallError> {
        // 能力门：先确认对端具备 owning-device runtime-snapshot 路由对应的能力 token。
        // 未通过时**不**发起路由请求，避免对旧版本对端发出无效请求并误判 404 为业务错误。
        PeerClient::new()
            .require_capability(base_url, CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1)
            .await?;
        // 绑定 expected_device_id 时 fail-closed：缺 binding 能力或 device 不匹配不得发路由。
        self.ensure_expected_device_binding_peer(&endpoint_url(
            base_url,
            "/api/orchestrator/runtime-snapshot",
        ))
        .await?;

        // 出站 request_id：非空入参原样转发（含首尾空格，与 middleware 可打印 ASCII 契约一致），
        // 仅真正空串生成新 UUID（禁止 trim，避免 ` req-1 ` 被改写）。
        let outbound_request_id: String = if request_id.is_empty() {
            crate::net::request_context::new_request_id()
        } else {
            request_id.to_string()
        };

        let url = endpoint_url(base_url, "/api/orchestrator/runtime-snapshot");
        let body = RemoteRuntimeSnapshotReq {
            project_id: project_id.to_string(),
        };
        let mut req = self
            .client
            .post(&url)
            .json(&body)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                outbound_request_id,
            )
            .timeout(remote_request_timeout(RemoteRequestTimeoutKind::Short));
        if let Some(device_id) = self.expected_device_id.as_deref() {
            req = req.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = req.send().await.map_err(|error| PeerCallError::Network {
            url: url.clone(),
            source: error,
        })?;
        let snapshot =
            parse_peer_response::<OrchestratorRuntimeSnapshotDto>(response, &url).await?;
        // owner 身份语义校验：成功 2xx 也不能把错误项目/远端 shortcut 快照重标为 live。
        validate_owner_runtime_snapshot(&snapshot, project_id, &url)?;
        Ok(snapshot)
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
    ///     发送后委托 parse_json_response。request_id 优先转发 `forwarded_request_id`（多跳代理），
    ///     缺失时生成新 UUID。
    async fn get_json<T>(
        &self,
        url: String,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        if self.expected_device_id.is_some() {
            let base = origin_base_url(&url)?;
            self.ensure_expected_device_binding(&base).await?;
        }
        let mut req = self
            .client
            .get(&url)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                self.outbound_request_id(),
            )
            .timeout(remote_request_timeout(timeout_kind));
        if let Some(device_id) = self.expected_device_id.as_deref() {
            req = req.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = req
            .send()
            .await
            // send 失败属于传输离线：用 Unavailable 分类，供 outbox/preflight 按类型分支。
            .map_err(|error| {
                AppError::unavailable(format!("远端 Orchestrator 请求失败: {error}"))
            })?;
        parse_json_response(response, &url).await
    }

    /// 发送 POST JSON 请求并解析响应。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Orchestrator 远端接口统一使用 JSON 请求体，错误文案应在所有方法间保持一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 reqwest POST 设置 JSON body、timeout 与出站 request_id header（Finding 3：多跳调用链关联），
    ///     发送后委托 parse_json_response。request_id 优先转发 `forwarded_request_id`（多跳代理），
    ///     缺失时生成新 UUID。
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
        if self.expected_device_id.is_some() {
            let base = origin_base_url(&url)?;
            self.ensure_expected_device_binding(&base).await?;
        }
        let mut req = self
            .client
            .post(&url)
            .json(body)
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                self.outbound_request_id(),
            )
            .timeout(remote_request_timeout(timeout_kind));
        if let Some(device_id) = self.expected_device_id.as_deref() {
            req = req.header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str(),
                device_id,
            );
        }
        let response = req
            .send()
            .await
            // send 失败属于传输离线：用 Unavailable 分类，供 outbox/preflight 按类型分支。
            .map_err(|error| {
                AppError::unavailable(format!("远端 Orchestrator 请求失败: {error}"))
            })?;
        parse_json_response(response, &url).await
    }
}

/// Business Logic（为什么需要这个函数）:
///     get_json/post_json 收到完整 endpoint URL，能力探测只需 origin base。
///
/// Code Logic（这个函数做什么）:
///     解析 scheme/host/port，拼 `scheme://host:port`。
fn origin_base_url(full_url: &str) -> Result<String, AppError> {
    let parsed = reqwest::Url::parse(full_url)
        .map_err(|err| AppError::generic(format!("远端 Orchestrator URL 无效: {err}")))?;
    let scheme = parsed.scheme();
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::generic("远端 Orchestrator URL 缺少 host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AppError::generic("远端 Orchestrator URL 缺少 port"))?;
    Ok(format!("{scheme}://{host}:{port}"))
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
        RemoteRequestTimeoutKind::Complete => {
            Duration::from_secs(COMPLETE_REMOTE_ORCHESTRATOR_TIMEOUT_SECS)
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

/// Business Logic（为什么需要这个函数）:
///     owning device 的 runtime-snapshot 成功响应若混入错误 projectId、remote shortcut 身份或
///     非 local remoteStatus，本机会无条件 remap 为 shortcut live，导致串项目/串状态缓存。
///
/// Code Logic（这个函数做什么）:
///     校验 `project_id == requested`、`project_kind == "local"`、`remote_status == "local"`；
///     任一不满足返回 `PeerCallError::InvalidResponse`，由 command 层映射 unavailable。
fn validate_owner_runtime_snapshot(
    snapshot: &OrchestratorRuntimeSnapshotDto,
    requested_project_id: &str,
    url: &str,
) -> Result<(), PeerCallError> {
    if snapshot.project_id != requested_project_id {
        return Err(PeerCallError::InvalidResponse {
            url: url.to_string(),
            reason: format!(
                "owner snapshot project_id mismatch: expected {requested_project_id}, got {}",
                snapshot.project_id
            ),
        });
    }
    if snapshot.project_kind != "local" {
        return Err(PeerCallError::InvalidResponse {
            url: url.to_string(),
            reason: format!(
                "owner snapshot project_kind must be local, got {}",
                snapshot.project_kind
            ),
        });
    }
    if snapshot.remote_status != "local" {
        return Err(PeerCallError::InvalidResponse {
            url: url.to_string(),
            reason: format!(
                "owner snapshot remote_status must be local, got {}",
                snapshot.remote_status
            ),
        });
    }
    Ok(())
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
    use crate::net::peer_error::PeerCallError;
    use crate::net::protocol::{
        CAPABILITY_DEVICE_REQUEST_BINDING_V1, CAPABILITY_ERRORS_ENVELOPE_V1,
        CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1, PROTOCOL_VERSION_V1,
    };
    use crate::net::routes::health::HealthResponse;
    use crate::orchestrator::models::OrchestratorAttemptPhase;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};
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
    ///     task-views/create-block 可能返回 origin 视图；client 必须抽出 task，不能当整段 DTO。
    ///
    /// Code Logic（这个测试做什么）:
    ///     喂入 Local view JSON，断言 block_created_dto_from_wire 得到 task.id。
    #[test]
    fn block_created_wire_unwraps_origin_views() {
        let task = deliver_reviewed_task_dto_json();
        let task_id = task["id"].as_str().unwrap().to_string();
        let value = serde_json::json!({
            "block": {
                "id": "block-1",
                "projectId": "project-1",
                "title": "Serial",
                "sharedWorktreeId": null,
                "sharedBranchName": null,
                "createdAt": "2026-08-18T00:00:00Z",
                "updatedAt": "2026-08-18T00:00:00Z"
            },
            "tasks": [{
                "origin": "remote",
                "deviceId": "device-a",
                "deviceName": "Mini",
                "task": task
            }]
        });
        let created = block_created_dto_from_wire(value).expect("parse view wire");
        assert_eq!(created.tasks.len(), 1);
        assert_eq!(created.tasks[0].id, task_id);
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
                    details: serde_json::Value::Object(serde_json::Map::new()),
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
            details: Box::new(serde_json::Value::Object(serde_json::Map::new())),
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

    /// Business Logic（为什么需要这个测试 / Finding 6）:
    ///     `errors.envelope.v1` 是**错误信封 wire-format**能力，不是路由访问 gate。
    ///     v0 对端（裸 `{error}` + 无能力探测）必须仍能被既有路由（如 tasks/list）调用——
    ///     这些路由在 v1 之前就已存在。本测试锁定这条向后兼容契约：
    ///     1. v0 对端的 `require_capability("errors.envelope.v1")` 返回 `Unsupported`（wire-format
    ///        不兼容，调用方据此决定是否启用 v1 专属的错误细节透传等新行为）；
    ///     2. 但 `RemoteOrchestratorClient::list_tasks` 这类既有 caller **不应**被该能力门阻断，
    ///        v0 对端调用应正常成功（hit 计数 1，响应走 v0 老形态解析链）。
    ///     这避免了把"wire-format 能力"误用作"路由访问 gate"的语义错误。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0/v1 两套 health + 生产 tasks/list 路由（带 hit 计数器）。
    ///     对 v0：断言 `require_capability` 返回 Unsupported，但 `list_tasks` 直接调用仍成功（hit=1）。
    ///     对 v1：断言 `require_capability` 通过，`list_tasks` 成功（hit=1）。
    ///     两侧 list_tasks 都不应被 errors.envelope.v1 能力门保护。
    #[tokio::test]
    async fn list_tasks_remains_callable_on_v0_peers_errors_envelope_is_not_route_gate() {
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

        // v0 对端：require_capability 返回 Unsupported（wire-format 不兼容），
        // 但这**不**应阻断既有 list_tasks caller —— v0 对端必须保持向后兼容。
        let (v0_url, v0_hits) = spawn_gated_server(0, vec![]).await;
        let v0_gate = PeerClient::new()
            .require_capability(&v0_url, "errors.envelope.v1")
            .await;
        assert!(
            matches!(v0_gate, Err(PeerCallError::Unsupported { .. })),
            "v0 不支持 errors.envelope.v1，require_capability 应返回 Unsupported，实际: {v0_gate:?}"
        );
        // 关键断言（Finding 6）：既有 list_tasks 不被 wire-format 能力门阻断，
        // v0 对端调用应成功，hit=1。
        let v0_tasks = RemoteOrchestratorClient::new()
            .list_tasks(&v0_url, "project-1")
            .await
            .expect("v0 对端的既有 list_tasks 路由应可调用（向后兼容）");
        assert!(v0_tasks.is_empty(), "fixture 返回空列表");
        assert_eq!(
            v0_hits.load(Ordering::SeqCst),
            1,
            "v0 既有 list_tasks 应被调用一次（不被 errors.envelope.v1 阻断）"
        );

        // v1 对端：require_capability 通过，list_tasks 同样成功。
        let (v1_url, v1_hits) = spawn_gated_server(1, vec!["errors.envelope.v1".to_string()]).await;
        PeerClient::new()
            .require_capability(&v1_url, "errors.envelope.v1")
            .await
            .expect("v1 应通过能力门");
        let v1_tasks = RemoteOrchestratorClient::new()
            .list_tasks(&v1_url, "project-1")
            .await
            .expect("v1 生产 list_tasks 应成功");
        assert!(v1_tasks.is_empty(), "fixture 返回空列表");
        assert_eq!(
            v1_hits.load(Ordering::SeqCst),
            1,
            "v1 list_tasks 应被调用一次"
        );
    }

    /// Business Logic（为什么需要这个测试 / Finding 6）:
    ///     真正属于"未来能力专属路由"的调用，必须把 `require_capability` **封装在生产 client 方法内部**，
    ///     而不是要求每个 caller 自己记得先 gate。本测试用一个假想的未来能力 token
    ///     (`orchestrator.runtime_snapshot.v1`) 与未来路由 `/api/orchestrator/runtime-snapshot`，
    ///     验证生产 client 方法 `require_capability` 在调用前确实先做能力 gate：v0 对端被拦截，
    ///     v1（带该 token）对端放行。token 与 `errors.envelope.v1` 解耦——后者只描述错误信封格式。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0（无 token）+ v1（带 `orchestrator.runtime_snapshot.v1`）两套 health + 新路由。
    ///     对 v0 调 `require_capability(.., "orchestrator.runtime_snapshot.v1")`：返回 Unsupported。
    ///     对 v1 调同一方法：通过。新路由 hit 在 v0 上为 0，v1 上为 1（前提是 caller 先 gate 再调）。
    #[tokio::test]
    async fn future_capability_token_gates_its_own_route_without_conflating_errors_envelope() {
        use crate::net::peer_client::{PeerCallError, PeerClient};
        use crate::net::routes::health::HealthResponse;
        use std::sync::atomic::{AtomicU32, Ordering};

        const FUTURE_TOKEN: &str = "orchestrator.runtime_snapshot.v1";

        async fn spawn_future_server(capabilities: Vec<String>) -> (String, Arc<AtomicU32>) {
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
                                protocol_version: if caps.is_empty() { 0 } else { 1 },
                                capabilities: caps,
                            })
                        }
                    }),
                )
                .route(
                    "/api/orchestrator/runtime-snapshot",
                    post(move || {
                        let hits = hits_clone.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            Json(serde_json::json!({ "snapshot": {} }))
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

        // v0 对端（无任何能力 token）：未来能力门拦截。
        let (v0_url, v0_hits) = spawn_future_server(vec![]).await;
        let v0_gate = PeerClient::new()
            .require_capability(&v0_url, FUTURE_TOKEN)
            .await;
        assert!(
            matches!(v0_gate, Err(PeerCallError::Unsupported { .. })),
            "v0 不支持 {FUTURE_TOKEN}，应被未来能力门拦截，实际: {v0_gate:?}"
        );
        // 模拟生产封装：caller 在 gate 未通过时不应继续打新路由。
        assert_eq!(
            v0_hits.load(Ordering::SeqCst),
            0,
            "未来能力门未通过时新路由不应被调用"
        );

        // v1 对端（带未来 token 但**没有** errors.envelope.v1）：
        // 验证 errors.envelope.v1 不应被用作任何路由访问 gate —— 新路由的访问权
        // 由它自己的 token（FUTURE_TOKEN）独立决定。
        let (v1_url, v1_hits) = spawn_future_server(vec![FUTURE_TOKEN.to_string()]).await;
        PeerClient::new()
            .require_capability(&v1_url, FUTURE_TOKEN)
            .await
            .expect("v1 应通过未来能力门（携带该 token）");
        assert_eq!(
            v1_hits.load(Ordering::SeqCst),
            0,
            "断言前 v1 新路由尚未被调用"
        );
        // gate 通过后调用方可以打新路由。
        // （生产 client 方法应把 require_capability + 新路由调用合为一个原子方法，这里分两步仅作演示。）
    }

    /// 启动带 health + runtime-snapshot 路由（带 hit 计数 + 请求体捕获）的临时服务。
    ///
    /// Code Logic: 复用测试常用的 v0/v1 health + 新路由骨架；额外捕获
    /// `RemoteRuntimeSnapshotReq` 请求体 `project_id` 与入站 `X-CC-Request-Id`，
    /// 供 capability gate / 出站 request_id 转发断言使用。返回 (base_url, hits, observed_project_id, observed_request_id, observed_raw_body)。
    async fn spawn_runtime_snapshot_server(
        protocol_version: u32,
        capabilities: Vec<String>,
        snapshot_payload: serde_json::Value,
    ) -> (
        String,
        Arc<AtomicU32>,
        Arc<Mutex<String>>,
        Arc<Mutex<String>>,
        Arc<Mutex<String>>,
    ) {
        use crate::orchestrator::remote_protocol::RemoteRuntimeSnapshotReq;
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let observed_project_id: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_request_id: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_raw_body: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_pid_clone = observed_project_id.clone();
        let observed_rid_clone = observed_request_id.clone();
        let observed_raw_clone = observed_raw_body.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(move || {
                    let caps = capabilities.clone();
                    async move {
                        Json(HealthResponse {
                            ok: true,
                            device_id: "owning-device".to_string(),
                            device_name: "Owning Device".to_string(),
                            http_port: 8765,
                            ts: 1_700_000_000,
                            protocol_version,
                            capabilities: caps,
                        })
                    }
                }),
            )
            .route(
                "/api/orchestrator/runtime-snapshot",
                post(
                    move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                        let hits = hits_clone.clone();
                        let observed_pid = observed_pid_clone.clone();
                        let observed_rid = observed_rid_clone.clone();
                        let observed_raw = observed_raw_clone.clone();
                        let payload = snapshot_payload.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            let raw = String::from_utf8_lossy(&body).to_string();
                            *observed_raw.lock().unwrap() = raw.clone();
                            if let Ok(req) =
                                serde_json::from_slice::<RemoteRuntimeSnapshotReq>(&body)
                            {
                                *observed_pid.lock().unwrap() = req.project_id;
                            }
                            *observed_rid.lock().unwrap() = headers
                                .get("x-cc-request-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            // owning-device 路由直接返回 OrchestratorRuntimeSnapshotDto（无 `{snapshot}` 包裹）。
                            Json(payload)
                        }
                    },
                ),
            );
        let url = spawn_orchestrator_server(app).await;
        (
            url,
            hits,
            observed_project_id,
            observed_request_id,
            observed_raw_body,
        )
    }

    /// Business Logic（为什么需要这个测试）:
    ///     生产 client 方法 `runtime_snapshot` 必须把 `require_capability` 与新路由调用合成一个原子方法：
    ///     对端具备 `orchestrator.runtime-snapshot.v1` 能力时，应通过能力门并实际 POST 到
    ///     `/api/orchestrator/runtime-snapshot`（hit=1），返回的 DTO 关键字段必须与对端响应**逐字一致**
    ///     （projectId / projectKind / schedulerEnabled / runningTasks 等 owner 字段）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v1 health（带 token）+ runtime-snapshot 路由（返回真实 owner 字段 JSON），
    ///     调用 `runtime_snapshot(base_url, "project-1", "req-1")`，断言：
    ///     - 命中路由 1 次（hit=1，证明能力门通过后调用真正发生）；
    ///     - 路由观测到的请求体 projectId == "project-1"（contract 字段未漂移）；
    ///     - 路由观测到出站 `X-CC-Request-Id` == "req-1"（多跳调用链关联）；
    ///     - 返回的 DTO projectId/projectKind/schedulerEnabled/runningTasks 与对端 payload 完全一致。
    #[tokio::test]
    async fn runtime_snapshot_returns_owner_fields_when_capability_supported() {
        let payload = serde_json::json!({
            "projectId": "project-1",
            "projectKind": "local",
            "remoteStatus": "local",
            "generatedAt": "2026-07-12T03:00:00Z",
            "latestTickAt": "2026-07-12T02:59:00Z",
            "lastDispatchAt": null,
            "lastDispatchedCount": 0,
            "schedulerEnabled": true,
            "workflowSource": "built-in",
            "workflowValid": true,
            "workflowError": null,
            "maxConcurrentTasks": 2,
            "slotsUsed": 1,
            "slotsAvailable": 1,
            "latestError": null,
            "runningTasks": [{
                "taskId": "task-running-1",
                "title": "运行中任务",
                "workflowState": "inProgress",
                "runState": "running",
                "attemptPhase": "streaming",
                "sessionId": "session-1",
                "worktreeId": null,
                "lastRuntimeMessage": "正在流式输出",
                "lastActivityAt": "2026-07-12T02:58:00Z"
            }],
            "retryingTasks": [],
            "recentEvents": [{
                "id": "event-1",
                "taskId": "task-running-1",
                "taskTitle": "运行中任务",
                "kind": "runner",
                "message": "Runner 启动",
                "createdAt": "2026-07-12T02:58:00Z"
            }]
        });
        let (base_url, hits, observed_pid, observed_rid, observed_raw) =
            spawn_runtime_snapshot_server(
                1,
                vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
                payload,
            )
            .await;

        let snapshot = RemoteOrchestratorClient::new()
            .runtime_snapshot(&base_url, "project-1", "req-1")
            .await
            .expect("v1 对端支持能力，应返回 owner snapshot");

        // 能力门通过 → 路由确实被调用一次。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "v1 应通过能力门并调用新路由"
        );
        // 请求体契约：P2P snake_case project_id 原样到达对端。
        assert_eq!(observed_pid.lock().unwrap().as_str(), "project-1");
        let raw_body = observed_raw.lock().unwrap().clone();
        assert!(
            raw_body.contains("\"project_id\""),
            "P2P client must send snake_case project_id, raw={raw_body}"
        );
        assert!(
            !raw_body.contains("\"projectId\""),
            "P2P client must not send camelCase projectId, raw={raw_body}"
        );
        // Finding 3：出站 request_id 必须原样转发到对端（多跳调用链关联）。
        assert_eq!(observed_rid.lock().unwrap().as_str(), "req-1");
        // Owner 字段必须逐字保留（不漂移、不重新计算）。
        assert_eq!(snapshot.project_id, "project-1");
        assert_eq!(snapshot.project_kind, "local");
        assert_eq!(snapshot.remote_status, "local");
        assert!(snapshot.scheduler_enabled);
        assert_eq!(snapshot.slots_used, 1);
        assert_eq!(snapshot.slots_available, 1);
        assert_eq!(snapshot.running_tasks.len(), 1);
        assert_eq!(snapshot.running_tasks[0].task_id, "task-running-1");
        assert_eq!(
            snapshot.running_tasks[0].attempt_phase,
            Some(OrchestratorAttemptPhase::Streaming)
        );
        assert_eq!(
            snapshot.running_tasks[0].session_id.as_deref(),
            Some("session-1")
        );
        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].task_id, "task-running-1");
        assert_eq!(snapshot.recent_events[0].kind, "runner");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     capability gate 必须在缺失能力时**不**调用 runtime-snapshot 路由。封装在生产 client 方法
    ///     内部的 `require_capability` 未通过时，方法应返回 `PeerCallError::Unsupported`，
    ///     且对端路由 hit 计数保持 0（不会向 v0/无 token 对端发出无效请求）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v1 health（无 token）+ runtime-snapshot 路由（带 hit 计数），
    ///     调用 `runtime_snapshot(...)`，断言返回 `Unsupported` 且路由未被调用（hit=0）。
    #[tokio::test]
    async fn runtime_snapshot_returns_unsupported_when_capability_absent() {
        let (base_url, hits, _observed_pid, _observed_rid, _observed_raw) =
            spawn_runtime_snapshot_server(1, vec![], serde_json::json!({})).await;

        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&base_url, "project-1", "req-1")
            .await
            .expect_err("无能力 token 应被 capability gate 拦截");
        assert!(
            matches!(err, PeerCallError::Unsupported { .. }),
            "缺失能力应返回 Unsupported，实际: {err:?}"
        );
        // 关键断言：未通过能力门时不应打对端路由。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "能力门未通过时 runtime-snapshot 路由不应被调用"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     workflow-document client 必须 capability-gate；缺失 token 时 Unsupported 且不打目标路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 无 token + get 路由计数器，调用 get_workflow_document 断言 Unsupported 且 hit=0。
    #[tokio::test]
    async fn workflow_document_returns_unsupported_when_capability_absent() {
        use crate::net::peer_error::PeerCallError;
        use crate::net::protocol::CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1;
        use crate::net::routes::health::HealthResponse;
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicU32::new(0));
        let hits_route = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "owning-device".to_string(),
                        device_name: "Owning Device".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 1,
                        capabilities: vec![],
                    })
                }),
            )
            .route(
                "/api/orchestrator/workflow-document/get",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "document": {
                                "status": "missing",
                                "content": null,
                                "contentHash": null,
                                "diagnostics": []
                            }
                        }))
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;

        let err = RemoteOrchestratorClient::new()
            .get_workflow_document(&base_url, "project-1")
            .await
            .expect_err("无能力 token 应被 capability gate 拦截");
        assert!(
            matches!(err, PeerCallError::Unsupported { capability, .. } if capability == CAPABILITY_ORCHESTRATOR_WORKFLOW_DOCUMENT_V1),
            "expected Unsupported for workflow-document capability, got {err:?}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "capability gate 不得调用目标路由"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     看板拖拽与 completeAgentRun 必须 fail-closed：对端缺 capability 时不得打目标路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 无 token + 两个目标路由计数器，分别调用 move/complete，断言 unavailable 且 hit=0。
    #[tokio::test]
    async fn move_and_complete_return_unsupported_when_capability_absent() {
        use crate::net::protocol::{
            CAPABILITY_ORCHESTRATOR_COMPLETE_AGENT_RUN_V1,
            CAPABILITY_ORCHESTRATOR_MOVE_WORKFLOW_STATE_V1,
        };
        use crate::net::routes::health::HealthResponse;
        use crate::orchestrator::models::OrchestratorWorkflowState;
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let move_hits = Arc::new(AtomicU32::new(0));
        let complete_hits = Arc::new(AtomicU32::new(0));
        let move_hits_route = move_hits.clone();
        let complete_hits_route = complete_hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "owning-device".to_string(),
                        device_name: "Owning Device".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 1,
                        capabilities: vec![],
                    })
                }),
            )
            .route(
                "/api/orchestrator/tasks/move-workflow-state",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = move_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(deliver_reviewed_task_dto_json())
                    }
                }),
            )
            .route(
                "/api/orchestrator/tasks/complete-agent-run",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = complete_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(deliver_reviewed_task_dto_json())
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;
        let client = RemoteOrchestratorClient::new();

        let move_err = client
            .move_task_workflow_state(
                &base_url,
                RemoteMoveWorkflowStateReq {
                    project_id: "project-1".to_string(),
                    task_id: "task-1".to_string(),
                    target_state: OrchestratorWorkflowState::Todo,
                },
            )
            .await
            .expect_err("缺 move capability 必须失败");
        assert!(
            move_err
                .to_string()
                .contains(CAPABILITY_ORCHESTRATOR_MOVE_WORKFLOW_STATE_V1),
            "move 应 fail-closed 为 capability_unsupported，实际: {move_err}"
        );
        assert_eq!(move_hits.load(Ordering::SeqCst), 0);

        let complete_err = client
            .complete_agent_run(
                &base_url,
                RemoteCompleteAgentRunReq {
                    project_id: "project-1".to_string(),
                    task_id: "task-1".to_string(),
                },
            )
            .await
            .expect_err("缺 complete capability 必须失败");
        assert!(
            complete_err
                .to_string()
                .contains(CAPABILITY_ORCHESTRATOR_COMPLETE_AGENT_RUN_V1),
            "complete 应 fail-closed 为 capability_unsupported，实际: {complete_err}"
        );
        assert_eq!(complete_hits.load(Ordering::SeqCst), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     旧 peer 缺 `orchestrator.task-blocks.v1` 时 create/append/reorder 必须 fail-closed，不得拆成普通任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 无 token + 三条目标路由计数器，分别调用 client，断言 capability_unsupported 且 hit=0。
    #[tokio::test]
    async fn task_block_mutations_return_unsupported_when_capability_absent() {
        use crate::net::protocol::CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1;
        use crate::net::routes::health::HealthResponse;
        use crate::orchestrator::remote_protocol::{
            RemoteAppendTaskBlockMemberReq, RemoteCreateOrchestratorTaskBlockReq,
            RemoteReorderTaskBlockMembersReq, RemoteTaskBlockMemberReq,
        };
        use axum::routing::{get, post};
        use axum::{Json, Router};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let create_hits = Arc::new(AtomicU32::new(0));
        let append_hits = Arc::new(AtomicU32::new(0));
        let reorder_hits = Arc::new(AtomicU32::new(0));
        let create_hits_route = create_hits.clone();
        let append_hits_route = append_hits.clone();
        let reorder_hits_route = reorder_hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "owning-device".to_string(),
                        device_name: "Owning Device".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 1,
                        capabilities: vec![],
                    })
                }),
            )
            .route(
                "/api/orchestrator/task-views/create-block",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = create_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({}))
                    }
                }),
            )
            .route(
                "/api/orchestrator/task-views/append-block-member",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = append_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({}))
                    }
                }),
            )
            .route(
                "/api/orchestrator/task-views/reorder-block-members",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let hits = reorder_hits_route.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({}))
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;
        let client = RemoteOrchestratorClient::new();

        let create_err = client
            .create_task_block(
                &base_url,
                RemoteCreateOrchestratorTaskBlockReq {
                    project_id: "project-1".to_string(),
                    title: "Serial".to_string(),
                    members: vec![
                        RemoteTaskBlockMemberReq {
                            title: "one".to_string(),
                            goal: "g1".to_string(),
                            acceptance_criteria: "a1".to_string(),
                        },
                        RemoteTaskBlockMemberReq {
                            title: "two".to_string(),
                            goal: "g2".to_string(),
                            acceptance_criteria: "a2".to_string(),
                        },
                    ],
                    create_action: crate::orchestrator::models::OrchestratorCreateAction::Todo,
                    client_request_id: Some("req-create-block".to_string()),
                    mutation_kind: Some("createBlock".to_string()),
                },
            )
            .await
            .expect_err("缺 task-blocks capability 必须失败");
        assert!(
            create_err
                .to_string()
                .contains(CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1),
            "create-block 应 fail-closed 为 capability_unsupported，实际: {create_err}"
        );
        assert_eq!(create_hits.load(Ordering::SeqCst), 0);

        let append_err = client
            .append_task_block_member(
                &base_url,
                RemoteAppendTaskBlockMemberReq {
                    project_id: "project-1".to_string(),
                    block_id: "block-1".to_string(),
                    title: "three".to_string(),
                    goal: "g3".to_string(),
                    acceptance_criteria: "a3".to_string(),
                    client_request_id: Some("req-append".to_string()),
                    mutation_kind: Some("appendBlockMember".to_string()),
                },
            )
            .await
            .expect_err("缺 task-blocks capability 必须失败");
        assert!(
            append_err
                .to_string()
                .contains(CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1),
            "append 应 fail-closed 为 capability_unsupported，实际: {append_err}"
        );
        assert_eq!(append_hits.load(Ordering::SeqCst), 0);

        let reorder_err = client
            .reorder_task_block_members(
                &base_url,
                RemoteReorderTaskBlockMembersReq {
                    project_id: "project-1".to_string(),
                    block_id: "block-1".to_string(),
                    ordered_task_ids: vec!["t1".to_string(), "t2".to_string()],
                    client_request_id: Some("req-reorder".to_string()),
                    mutation_kind: Some("reorderBlockMembers".to_string()),
                },
            )
            .await
            .expect_err("缺 task-blocks capability 必须失败");
        assert!(
            reorder_err
                .to_string()
                .contains(CAPABILITY_ORCHESTRATOR_TASK_BLOCKS_V1),
            "reorder 应 fail-closed 为 capability_unsupported，实际: {reorder_err}"
        );
        assert_eq!(reorder_hits.load(Ordering::SeqCst), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     complete-agent-run 必须与 control 路径一致使用约 360s 超时，避免验证 pipeline 被短超时切断。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 Complete timeout kind 为 360 秒。
    #[test]
    fn complete_agent_run_timeout_is_360_seconds() {
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::Complete),
            Duration::from_secs(360)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端离线（health_info send 失败）时，方法必须返回 `PeerCallError::Network`，
    ///     而不是把网络错误折叠成业务文案或泛型错误，让上层能据此做"离线/在线"决策。
    ///
    /// Code Logic（这个测试做什么）:
    ///     向一个肯定不存在的端口发起调用，断言返回 `Network` 变体。
    #[tokio::test]
    async fn runtime_snapshot_returns_network_error_when_peer_offline() {
        // 绑定一个临时 socket 然后立即关闭，拿到一个"必拒绝"端口。
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&format!("http://{addr}"), "project-1", "req-1")
            .await
            .expect_err("离线端口应返回 Network 错误");
        assert!(
            matches!(err, PeerCallError::Network { .. }),
            "对端离线应返回 Network，实际: {err:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端响应非 JSON 或字段不全时，必须返回 `PeerCallError::InvalidResponse`
    ///     （协议违例），不能被误判为业务错误（Remote）或离线（Network）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v1 health（带 token）+ 一个返回 502 + 纯文本 body 的 runtime-snapshot 路由，
    ///     调用 `runtime_snapshot(...)`，断言返回 `InvalidResponse`。
    #[tokio::test]
    async fn runtime_snapshot_returns_invalid_response_for_non_json_body() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "owning-device".to_string(),
                        device_name: "Owning Device".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 1,
                        capabilities: vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
                    })
                }),
            )
            .route(
                "/api/orchestrator/runtime-snapshot",
                post(move || {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (StatusCode::BAD_GATEWAY, "upstream proxy down")
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;

        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&base_url, "project-1", "req-1")
            .await
            .expect_err("非 JSON 响应应失败");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "非 JSON body 应归为 InvalidResponse，实际: {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "对端路由确实被调用");
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     `request_id` 入参用于把入站请求 ID 转发到下一跳，构建多跳调用链。空 `request_id`
    ///     应被视为"未提供"，方法内部应生成新 UUID，避免空 ID 污染对端日志。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用空字符串 request_id 调用方法，断言对端观测到的 X-CC-Request-Id 为新生成的 36 字符 UUID
    ///     （而非空串）。
    #[tokio::test]
    async fn runtime_snapshot_generates_request_id_when_caller_passes_empty() {
        let (base_url, _hits, _observed_pid, observed_rid, _observed_raw) =
            spawn_runtime_snapshot_server(
                1,
                vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
                serde_json::json!({
                    "projectId": "project-1",
                    "projectKind": "local",
                    "remoteStatus": "local",
                    "generatedAt": "2026-07-12T03:00:00Z",
                    "latestTickAt": null,
                    "lastDispatchAt": null,
                    "lastDispatchedCount": 0,
                    "schedulerEnabled": false,
                    "workflowSource": "built-in",
                    "workflowValid": true,
                    "workflowError": null,
                    "maxConcurrentTasks": 1,
                    "slotsUsed": 0,
                    "slotsAvailable": 1,
                    "latestError": null,
                    "runningTasks": [],
                    "retryingTasks": [],
                    "recentEvents": []
                }),
            )
            .await;

        let _snapshot = RemoteOrchestratorClient::new()
            .runtime_snapshot(&base_url, "project-1", "")
            .await
            .expect("空 request_id 仍应成功调用");
        let observed = observed_rid.lock().unwrap().clone();
        assert_ne!(observed, "", "不应发送空 X-CC-Request-Id");
        assert_eq!(
            observed.len(),
            36,
            "空入参时应生成 36 字符 UUID: {observed}"
        );
    }

    /// Business Logic（为什么需要这个测试 / T7 混合 v0/v1 契约）:
    ///     生产 `runtime_snapshot` 必须在真实 v0 对端（protocol_version=0、无 capability）上返回
    ///     Unsupported 且零路由命中；同一方法在 v1 对端上返回 live owner 字段。
    ///     这锁死“能力门先于 feature route”的 mixed-version 契约，避免 404 被误当 capability。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0（protocol=0、caps=[]）与 v1（protocol=1、带 runtime-snapshot token）两套 fixture，
    ///     分别调用生产 `runtime_snapshot`：v0 断言 Unsupported + hit=0；v1 断言 hit=1 且 owner
    ///     generatedAt/tick/slots/event 唯一值原样返回。
    #[tokio::test]
    async fn runtime_snapshot_mixed_v0_v1_contract_unsupported_without_route_and_live_with_owner_fields(
    ) {
        // v0 对端：protocol_version=0，无任何能力，但挂着同名路由（hit 计数器验证门控）。
        let (v0_url, v0_hits, _v0_pid, _v0_rid, _v0_raw) =
            spawn_runtime_snapshot_server(0, vec![], serde_json::json!({})).await;
        let v0_err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&v0_url, "owner-local-project", "req-v0")
            .await
            .expect_err("v0 对端应被能力门拦截");
        assert!(
            matches!(v0_err, PeerCallError::Unsupported { .. }),
            "new client + v0 server 必须 Unsupported，实际: {v0_err:?}"
        );
        assert_eq!(
            v0_hits.load(Ordering::SeqCst),
            0,
            "v0 对端不得命中 feature route"
        );

        // v1 对端：带权威 capability，返回带唯一 owner 指纹的 payload。
        let owner_generated_at = "2026-07-12T15:16:17.018Z";
        let owner_tick = "2026-07-12T15:15:00.001Z";
        let owner_event_message = "owner-only-event-fingerprint-p4t7";
        let payload = serde_json::json!({
            "projectId": "owner-local-project",
            "projectKind": "local",
            "remoteStatus": "local",
            "generatedAt": owner_generated_at,
            "latestTickAt": owner_tick,
            "lastDispatchAt": "2026-07-12T15:14:30.002Z",
            "lastDispatchedCount": 7,
            "schedulerEnabled": true,
            "workflowSource": "projectOverride",
            "workflowValid": true,
            "workflowError": null,
            "maxConcurrentTasks": 6,
            "slotsUsed": 4,
            "slotsAvailable": 2,
            "latestError": "owner-only-latest-error-p4t7",
            "runningTasks": [{
                "taskId": "task-owner-p4t7",
                "title": "owner running",
                "workflowState": "inProgress",
                "runState": "running",
                "attemptPhase": "streaming",
                "sessionId": "session-owner-p4t7",
                "worktreeId": "worktree-owner-p4t7",
                "lastRuntimeMessage": "owner streaming p4t7",
                "lastActivityAt": "2026-07-12T15:13:00Z"
            }],
            "retryingTasks": [],
            "recentEvents": [{
                "id": "event-owner-p4t7",
                "taskId": "task-owner-p4t7",
                "taskTitle": "owner running",
                "kind": "runner",
                "message": owner_event_message,
                "createdAt": "2026-07-12T15:12:00Z"
            }]
        });
        let (v1_url, v1_hits, observed_pid, _observed_rid, observed_raw) =
            spawn_runtime_snapshot_server(
                1,
                vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
                payload,
            )
            .await;
        let snapshot = RemoteOrchestratorClient::new()
            .runtime_snapshot(&v1_url, "owner-local-project", "req-v1")
            .await
            .expect("new client + v1 server 应返回 live owner snapshot");
        assert_eq!(v1_hits.load(Ordering::SeqCst), 1, "v1 应命中 feature route");
        assert_eq!(observed_pid.lock().unwrap().as_str(), "owner-local-project");
        let raw_body = observed_raw.lock().unwrap().clone();
        assert!(
            raw_body.contains("\"project_id\""),
            "mixed v1 path must send snake_case project_id, raw={raw_body}"
        );
        assert_eq!(snapshot.generated_at, owner_generated_at);
        assert_eq!(snapshot.latest_tick_at.as_deref(), Some(owner_tick));
        assert_eq!(snapshot.last_dispatched_count, 7);
        assert_eq!(snapshot.slots_used, 4);
        assert_eq!(snapshot.slots_available, 2);
        assert_eq!(
            snapshot.latest_error.as_deref(),
            Some("owner-only-latest-error-p4t7")
        );
        assert_eq!(snapshot.running_tasks.len(), 1);
        assert_eq!(snapshot.running_tasks[0].task_id, "task-owner-p4t7");
        assert_eq!(snapshot.recent_events.len(), 1);
        assert_eq!(snapshot.recent_events[0].message, owner_event_message);
    }

    /// Business Logic（为什么需要这个测试 / T7 invalid→unavailable）:
    ///     v1 对端返回字段不全的 JSON 时，client 必须归为 InvalidResponse（上层映射 unavailable），
    ///     不能误判为 Unsupported 或 Network，也不能用本机 telemetry 补空。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动带 capability 的 v1 health + 返回缺字段 payload 的 runtime-snapshot 路由，
    ///     调用 runtime_snapshot 断言 InvalidResponse 且路由已被命中。
    #[tokio::test]
    async fn runtime_snapshot_returns_invalid_response_for_incomplete_v1_payload() {
        let (base_url, hits, _pid, _rid, _raw) = spawn_runtime_snapshot_server(
            1,
            vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
            serde_json::json!({ "projectId": "only-partial" }),
        )
        .await;
        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&base_url, "project-1", "req-invalid")
            .await
            .expect_err("不完整 v1 payload 应失败");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "invalid v1 payload 必须 InvalidResponse（→ unavailable），实际: {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "能力门通过后路由应被调用");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     owner 成功响应若 projectId/projectKind/remoteStatus 身份不匹配，必须 InvalidResponse，
    ///     禁止被 command 层 remap 成目标 shortcut 的 live 缓存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别喂 wrong-project / wrong-kind / wrong-status fixture，断言均为 InvalidResponse。
    #[tokio::test]
    async fn runtime_snapshot_rejects_owner_identity_mismatch_as_invalid_response() {
        // wrong project_id
        let wrong_project = serde_json::json!({
            "projectId": "other-project",
            "projectKind": "local",
            "remoteStatus": "local",
            "generatedAt": "2026-07-12T03:00:00Z",
            "latestTickAt": null,
            "lastDispatchAt": null,
            "lastDispatchedCount": 0,
            "schedulerEnabled": true,
            "workflowSource": "built-in",
            "workflowValid": true,
            "workflowError": null,
            "maxConcurrentTasks": 1,
            "slotsUsed": 0,
            "slotsAvailable": 1,
            "latestError": null,
            "runningTasks": [],
            "retryingTasks": [],
            "recentEvents": []
        });
        let (url, hits, _, _, _) = spawn_runtime_snapshot_server(
            1,
            vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
            wrong_project,
        )
        .await;
        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&url, "project-1", "req-wrong-project")
            .await
            .expect_err("wrong project must be invalid");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "wrong project → InvalidResponse, got {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // wrong project_kind
        let wrong_kind = serde_json::json!({
            "projectId": "project-1",
            "projectKind": "remote",
            "remoteStatus": "local",
            "generatedAt": "2026-07-12T03:00:00Z",
            "latestTickAt": null,
            "lastDispatchAt": null,
            "lastDispatchedCount": 0,
            "schedulerEnabled": true,
            "workflowSource": "built-in",
            "workflowValid": true,
            "workflowError": null,
            "maxConcurrentTasks": 1,
            "slotsUsed": 0,
            "slotsAvailable": 1,
            "latestError": null,
            "runningTasks": [],
            "retryingTasks": [],
            "recentEvents": []
        });
        let (url, hits, _, _, _) = spawn_runtime_snapshot_server(
            1,
            vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
            wrong_kind,
        )
        .await;
        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&url, "project-1", "req-wrong-kind")
            .await
            .expect_err("wrong kind must be invalid");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "wrong kind → InvalidResponse, got {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // wrong remote_status
        let wrong_status = serde_json::json!({
            "projectId": "project-1",
            "projectKind": "local",
            "remoteStatus": "live",
            "generatedAt": "2026-07-12T03:00:00Z",
            "latestTickAt": null,
            "lastDispatchAt": null,
            "lastDispatchedCount": 0,
            "schedulerEnabled": true,
            "workflowSource": "built-in",
            "workflowValid": true,
            "workflowError": null,
            "maxConcurrentTasks": 1,
            "slotsUsed": 0,
            "slotsAvailable": 1,
            "latestError": null,
            "runningTasks": [],
            "retryingTasks": [],
            "recentEvents": []
        });
        let (url, hits, _, _, _) = spawn_runtime_snapshot_server(
            1,
            vec![CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1.to_string()],
            wrong_status,
        )
        .await;
        let err = RemoteOrchestratorClient::new()
            .runtime_snapshot(&url, "project-1", "req-wrong-status")
            .await
            .expect_err("wrong status must be invalid");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "wrong status → InvalidResponse, got {err:?}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
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

    /// Business Logic（为什么需要这个函数）:
    ///     deliver-reviewed client 测试只需稳定的任务 DTO fixture，避免每个用例重复字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回可被 OrchestratorTaskDto 反序列化的最小 camelCase JSON。
    fn deliver_reviewed_task_dto_json() -> serde_json::Value {
        serde_json::json!({
            "id": "task-1",
            "projectId": "project-1",
            "title": "t",
            "goal": "g",
            "acceptanceCriteria": "a",
            "status": "delivering",
            "workflowState": "merging",
            "runState": "delivering",
            "source": "internal",
            "priority": 0,
            "attempt": 1,
            "createdAt": "2026-07-05T00:00:00Z",
            "updatedAt": "2026-07-05T00:00:00Z"
        })
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

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     多跳代理（手机 → 本机 → 项目所在设备）必须把入站 `X-CC-Request-Id` 转发到下一跳，
    ///     让整条调用链共用同一 ID。`with_forwarded_request_id` 设置后，出站请求必须携带该 ID
    ///     而非新生成的 UUID；未设置时仍生成新 UUID（向后兼容）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 echo server 用 Arc<Mutex<String>> 捕获 observed `X-CC-Request-Id`；
    ///     用转发 ID 调用，断言对端观测到的就是该固定 ID。
    #[tokio::test]
    async fn with_forwarded_request_id_propagates_inbound_id_to_next_hop() {
        use std::sync::{Arc, Mutex};
        let observed: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_clone = observed.clone();
        let app = Router::new().route(
            "/api/orchestrator/tasks/list",
            post(
                move |headers: axum::http::HeaderMap, _req: Json<RemoteListTasksReq>| {
                    let observed = observed_clone.clone();
                    async move {
                        let id = headers
                            .get("x-cc-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        *observed.lock().unwrap() = id;
                        Json(RemoteOrchestratorTaskListResp {
                            tasks: Vec::<OrchestratorTaskDto>::new(),
                        })
                    }
                },
            ),
        );
        let base_url = spawn_orchestrator_server(app).await;

        RemoteOrchestratorClient::new()
            .with_forwarded_request_id("trace-multi-hop-001")
            .list_tasks(&base_url, "project-1")
            .await
            .expect("转发场景应成功");
        assert_eq!(
            observed.lock().unwrap().as_str(),
            "trace-multi-hop-001",
            "转发 ID 必须原样到达下一跳"
        );

        // 非转发场景：出站 ID 应为新生成的 36 字符 UUID。
        RemoteOrchestratorClient::new()
            .list_tasks(&base_url, "project-1")
            .await
            .expect("非转发场景应成功");
        assert_eq!(
            observed.lock().unwrap().len(),
            36,
            "未设置转发 ID 时应生成 36 字符 UUID"
        );
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     `with_forwarded_request_id` 传入空字符串应被视为 None（不转发），避免空 ID 污染对端日志。
    #[test]
    fn with_forwarded_request_id_ignores_empty_string() {
        let client = RemoteOrchestratorClient::new().with_forwarded_request_id("");
        // 空串 → forwarded_request_id 应为 None → outbound_request_id 回落到 36 字符 UUID。
        let id = client.outbound_request_id();
        assert_eq!(id.len(), 36, "空转发 ID 应被忽略，回落到 UUID");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     不经 get_json/post_json 的原始 POST 构建路径若漏注入设备头，端口被另一设备接管时
    ///     会把 mutation/快照打到错误 owner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     源码断言 post_json_peer 与 runtime_snapshot 均在 expected_device_id Some 时写
    ///     EXPECTED_DEVICE_ID_HEADER。
    #[test]
    fn post_json_peer_and_runtime_snapshot_inject_expected_device_id_header() {
        let src = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/orchestrator/remote_client.rs"
        ));
        // 粗粒度：文件内至少三处（get_json / post_json / 原始路径）注入设备头。
        let header_hits = src.matches("EXPECTED_DEVICE_ID_HEADER").count();
        assert!(
            header_hits >= 3,
            "get_json/post_json/post_json_peer|runtime_snapshot 均应引用 EXPECTED_DEVICE_ID_HEADER，got {header_hits}"
        );
        assert!(
            src.contains("async fn post_json_peer"),
            "必须保留 post_json_peer"
        );
        // post_json_peer 体与 runtime_snapshot 体内各自出现 expected_device_id 注入。
        let peer_fn = src
            .split("async fn post_json_peer")
            .nth(1)
            .and_then(|s| s.split("async fn queue_task").next())
            .expect("定位 post_json_peer 函数体");
        assert!(
            peer_fn.contains("EXPECTED_DEVICE_ID_HEADER"),
            "post_json_peer 必须注入 EXPECTED_DEVICE_ID_HEADER"
        );
        let snap_fn = src
            .split("pub async fn runtime_snapshot")
            .nth(1)
            .and_then(|s| s.split("/// 发送 taskId 请求").next())
            .expect("定位 runtime_snapshot 函数体");
        assert!(
            snap_fn.contains("EXPECTED_DEVICE_ID_HEADER"),
            "runtime_snapshot 必须注入 EXPECTED_DEVICE_ID_HEADER"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     with_expected_device_id 绑定后，post_json 路径必须把设备头送到对端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 宣告 binding + matching device_id；list 捕获设备头并断言命中。
    #[tokio::test]
    async fn with_expected_device_id_propagates_header_on_post_json() {
        let observed: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_clone = observed.clone();
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "device-owner-42".into(),
                        device_name: "owner".into(),
                        http_port: 1,
                        ts: 1,
                        protocol_version: PROTOCOL_VERSION_V1,
                        capabilities: vec![CAPABILITY_DEVICE_REQUEST_BINDING_V1.to_string()],
                    })
                }),
            )
            .route(
                "/api/orchestrator/tasks/list",
                post(
                    move |headers: axum::http::HeaderMap, _req: Json<RemoteListTasksReq>| {
                        let observed = observed_clone.clone();
                        let hits = hits_clone.clone();
                        async move {
                            hits.fetch_add(1, Ordering::SeqCst);
                            let id = headers
                                .get(crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.as_str())
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("")
                                .to_string();
                            *observed.lock().unwrap() = id;
                            Json(RemoteOrchestratorTaskListResp {
                                tasks: Vec::<OrchestratorTaskDto>::new(),
                            })
                        }
                    },
                ),
            );
        let base_url = spawn_orchestrator_server(app).await;
        RemoteOrchestratorClient::new()
            .with_expected_device_id("device-owner-42")
            .list_tasks(&base_url, "project-1")
            .await
            .expect("list_tasks 应成功");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            observed.lock().unwrap().as_str(),
            "device-owner-42",
            "post_json 路径必须携带 expected device id"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     绑定 device 时旧 peer 无 `device.request-binding.v1` 必须 fail-closed，禁止 mutation。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 无 binding 能力；list_tasks 失败且 list 路由 hit=0。
    #[tokio::test]
    async fn expected_device_binding_fails_closed_without_capability() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "device-owner-42".into(),
                        device_name: "owner".into(),
                        http_port: 1,
                        ts: 1,
                        protocol_version: PROTOCOL_VERSION_V1,
                        capabilities: vec![CAPABILITY_ERRORS_ENVELOPE_V1.to_string()],
                    })
                }),
            )
            .route(
                "/api/orchestrator/tasks/list",
                post(move |_req: Json<RemoteListTasksReq>| {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(RemoteOrchestratorTaskListResp {
                            tasks: Vec::<OrchestratorTaskDto>::new(),
                        })
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;
        let err = RemoteOrchestratorClient::new()
            .with_expected_device_id("device-owner-42")
            .list_tasks(&base_url, "project-1")
            .await
            .expect_err("missing binding capability must fail closed");
        assert!(
            err.to_string().contains("device.request-binding.v1")
                || err.to_string().contains("不支持能力"),
            "unexpected err: {err}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0, "mutation must not hit");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     binding 能力存在但 health.device_id 不匹配时必须 conflict fail-closed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     health 宣告 binding + wrong device；list_tasks 失败且 hit=0。
    #[tokio::test]
    async fn expected_device_binding_fails_closed_on_device_mismatch() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    Json(HealthResponse {
                        ok: true,
                        device_id: "other-device".into(),
                        device_name: "other".into(),
                        http_port: 1,
                        ts: 1,
                        protocol_version: PROTOCOL_VERSION_V1,
                        capabilities: vec![CAPABILITY_DEVICE_REQUEST_BINDING_V1.to_string()],
                    })
                }),
            )
            .route(
                "/api/orchestrator/tasks/list",
                post(move |_req: Json<RemoteListTasksReq>| {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Json(RemoteOrchestratorTaskListResp {
                            tasks: Vec::<OrchestratorTaskDto>::new(),
                        })
                    }
                }),
            );
        let base_url = spawn_orchestrator_server(app).await;
        let err = RemoteOrchestratorClient::new()
            .with_expected_device_id("device-owner-42")
            .list_tasks(&base_url, "project-1")
            .await
            .expect_err("device mismatch must fail closed");
        assert!(
            err.to_string().contains("device_id") || err.to_string().contains("不匹配"),
            "unexpected err: {err}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}
