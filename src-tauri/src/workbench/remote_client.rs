//! workbench/remote_client.rs — Workbench 远端 HTTP 客户端
//!
//! Business Logic（为什么需要这个模块）:
//!     本机 Workbench 需要通过局域网对端的 P2P HTTP server 浏览目录并打开远端项目，
//!     让用户不必手动挂载共享目录也能保存远端项目快捷方式。
//!
//! Code Logic（这个模块做什么）:
//!     封装 reqwest::Client，调用 `/api/workbench/...` 远端路由，并把网络、状态码与 JSON
//!     解析错误统一转换为简洁中文 AppError。

use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::peer_call_error_to_app_error;
use crate::net::protocol::CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1;
use crate::net::protocol::{
    PeerProtocolInfo, CAPABILITY_DEVICE_REQUEST_BINDING_V1,
    CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1, CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1,
};
use crate::workbench::agent_ledger::models::{
    AgentLedgerSummaryBatchReq, AgentLedgerSummaryBatchResp,
};
use crate::workbench::browser_models::{WorkbenchBrowserDiscovery, WorkbenchBrowserPreview};
use crate::workbench::browser_verification::{
    BrowserVerificationArtifactDto, BrowserVerificationCommand, BrowserVerificationRun,
};
use crate::workbench::claude_sessions::{
    decode_session_search_response_body, SessionPreview, SessionSearchResult,
};
use crate::workbench::lan_fleet::models::{LanFleetOwnerBatchReq, LanFleetOwnerBatchResp};
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchGitCommitDto, WorkbenchHtmlAssetDto, WorkbenchOpenFileDto,
    WorkbenchPathInfo, WorkbenchProjectDto, WorkbenchRemoteDirectoryEntryDto,
    WorkbenchRemotePathInfoDto, WorkbenchRemoteRootDto, WorkbenchSaveTextResultDto,
    WorkbenchSessionDto, WorkbenchSqlitePreview, WorkbenchWorktreeDto,
};
use crate::workbench::operation_ledger::{
    WorkbenchMutationEnvelopeDto, WorkbenchMutationOperationDto,
};
use crate::workbench::remote_protocol::{
    RemoteClaudeSessionReq, RemoteCommitWorktreeReq, RemoteCreatePathReq, RemoteCreateSessionReq,
    RemoteCreateWorktreeReq, RemoteDeletePathReq, RemoteFocusedSessionReq,
    RemoteFocusedSessionResp, RemoteGitCommitsReq, RemoteListDirReq, RemoteListSessionsReq,
    RemoteMutationOperationReq, RemoteOpenFileReq, RemotePathInfoReq, RemotePreviewHtmlAssetReq,
    RemotePreviewSqliteReq, RemoteProjectReq, RemotePromptOptimizerReq, RemoteRemoveWorktreeReq,
    RemoteRenamePathReq, RemoteRenameSessionReq, RemoteReplaySessionReq, RemoteResizeSessionReq,
    RemoteSafeAttachReq, RemoteSaveTextReq, RemoteSearchClaudeSessionsReq, RemoteSelectPaneAtReq,
    RemoteSelectPaneAtResp, RemoteSessionReq, RemoteSplitPaneReq,
    RemoteWorkbenchBrowserDiscoverReq, RemoteWorkbenchBrowserPreviewReq,
    RemoteWorkspaceRestorePreflightReq, RemoteWorktreeReq, RemoteWriteSessionInputReq,
    ResumeClaudeSessionResult,
};
use crate::workbench::sessions::WorkbenchSessionReplayDto;
use crate::workbench::workspace_restore::{SafeAttachResult, WorkspaceRestorePlan};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::time::Duration;

const SHORT_REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 15;
const LONG_REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 120;
const VERY_LONG_REMOTE_WORKBENCH_TIMEOUT_SECS: u64 = 420;

/// 远端请求超时类别。
///
/// Business Logic（为什么需要这个枚举）:
///     Workbench 既有目录浏览这类短读操作，也有创建 worktree、保存文件、commit/merge 等耗时远端操作。
///
/// Code Logic（这个枚举做什么）:
///     区分短请求、长请求和覆盖 Claude Code 子流程的超长请求，供每个 reqwest request 单独设置 timeout。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteRequestTimeoutKind {
    Short,
    Long,
    VeryLong,
}

/// Workbench 远端 HTTP 客户端。
///
/// Business Logic（为什么需要这个结构体）:
///     多个远端 Workbench 命令需要复用同一套 HTTP 调用与错误映射规则。
///
/// Code Logic（这个结构体做什么）:
///     持有 cloneable 的 `reqwest::Client`，对外提供目录根、目录列表、路径信息和打开项目方法。
///     `forwarded_request_id`（Finding 3）若被设置，所有出站请求会复用该 ID，把多跳代理
///     （手机 → 本机 → 远端设备）串成同一调用链；否则每次出站生成新 UUID。
#[derive(Clone)]
pub struct RemoteWorkbenchClient {
    client: reqwest::Client,
    forwarded_request_id: Option<String>,
    /// 可选：出站绑定期望 device_id（与 health 预检配合，服务端 header guard 校验）。
    expected_device_id: Option<String>,
}

impl RemoteWorkbenchClient {
    /// 创建 Workbench 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     命令层每次处理远端请求时需要一个可直接使用的客户端实例。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造不带全局超时的 reqwest client；每个请求按短/长操作单独设置 timeout。
    ///     `forwarded_request_id` 默认 None（每次出站生成新 ID）。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("构造 Workbench 远端 reqwest Client 失败");
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
    ///
    /// 注：当前生产路径中 `RemoteWorkbenchClient` 仅在 Tauri 命令（无 inbound request_id）使用，
    /// 该 builder 暂无生产 caller，留给未来 P2P workbench relay 路由接入。测试已锁定其行为。
    #[allow(dead_code)]
    pub fn with_forwarded_request_id(mut self, request_id: impl Into<String>) -> Self {
        let id = request_id.into();
        self.forwarded_request_id = if id.is_empty() { None } else { Some(id) };
        self
    }

    /// 绑定期望远端 device_id，使每个 GET/POST 携带 `X-Cc-Partner-Expected-Device-Id`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     health 预检与真实 mutation 是独立 HTTP 请求；端口复用/设备切换时必须让业务请求
    ///     自己携带期望 device_id，由 owning 服务端 fail closed。
    ///
    /// Code Logic（这个函数做什么）:
    ///     空串清为 None；非空写入 `expected_device_id`，`get_json`/`post_json` 出站时注入 header。
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
    ///     expected_device_id 为 None 时直接 Ok；否则 require_capability(binding) + device_id 精确匹配，
    ///     不匹配 → conflict；缺能力 → validation（Unsupported）。
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
            .map_err(|err| peer_call_error_to_app_error(err, "远端 Workbench"))?;
        if health.device_id.trim() != expected {
            return Err(AppError::conflict(format!(
                "远端 Workbench device_id 不匹配: expected={expected}, got={}",
                health.device_id
            )));
        }
        Ok(())
    }

    /// 获取远端设备可浏览的根目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户添加远端项目时，需要先看到对端的 Home、下载、常用代码目录等入口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET `{base_url}/api/workbench/fs/roots`，解析为 `WorkbenchRemoteRootDto` 列表。
    pub async fn roots(&self, base_url: &str) -> Result<Vec<WorkbenchRemoteRootDto>, AppError> {
        self.get_json(
            endpoint_url(base_url, "/api/workbench/fs/roots"),
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 列出远端目录下的一级条目。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端项目选择器需要逐层浏览对端文件系统，直到用户选中目标项目目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/fs/list`，请求体 `{path}`，解析目录条目 DTO 列表。
    pub async fn list_dir(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/fs/list"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 获取远端路径信息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户选中远端路径时，前端需要判断路径是否可读、是否为 Git 仓库以及建议项目名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/fs/info`，请求体 `{path}`，解析单个路径信息 DTO。
    pub async fn path_info(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<WorkbenchRemotePathInfoDto, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/fs/info"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在远端设备打开项目。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机保存远端快捷方式前，需要让远端设备先创建或复用它自己的本机 Workbench 项目记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/projects/open`，请求体 `{path}`，解析远端返回的项目 DTO。
    pub async fn open_project(
        &self,
        base_url: &str,
        path: &str,
    ) -> Result<WorkbenchProjectDto, AppError> {
        self.post_path_json(
            endpoint_url(base_url, "/api/workbench/projects/open"),
            path,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 发现远端项目的浏览器预览候选。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 browser tab 必须让 owning device 扫描自己的终端输出、项目配置和 loopback 端口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/browser/discover`，请求体为远端 local project/worktree，解析 discovery DTO。
    pub async fn discover_browser_targets(
        &self,
        base_url: &str,
        req: &RemoteWorkbenchBrowserDiscoverReq,
    ) -> Result<WorkbenchBrowserDiscovery, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser/discover"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 创建远端项目的浏览器预览。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 创建 preview 时，真实 previewId 必须先在 owning device 登记，本机随后只创建 relay。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/browser/preview`，发送 targetUrl 并解析远端 preview DTO。
    pub async fn create_browser_preview(
        &self,
        base_url: &str,
        req: &RemoteWorkbenchBrowserPreviewReq,
    ) -> Result<WorkbenchBrowserPreview, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser/preview"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在对端 capability 缺失时返回 structured unsupported。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧 peer 未宣告 `workbench.browser-verification.v1` 时，UI 应看到 unsupported 而非裸 HTTP 失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `peer_supports_capability`；false → `AppError::unavailable("browser_verification_unsupported")`。
    async fn require_browser_verification_capability(
        &self,
        base_url: &str,
    ) -> Result<(), AppError> {
        let ok = self
            .peer_supports_capability(base_url, CAPABILITY_WORKBENCH_BROWSER_VERIFICATION_V1)
            .await?;
        if !ok {
            return Err(AppError::unavailable("browser_verification_unsupported"));
        }
        Ok(())
    }

    /// 在 owning device 上创建/启动浏览器验证（幂等 requestId）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     controller RemoteRelay 不得本地启 engine，必须把 create 代理到 owner。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require capability → POST `/api/workbench/browser-verification/create`。
    pub async fn create_browser_verification(
        &self,
        base_url: &str,
        preview_id: &str,
        request_id: &str,
        commands: &[BrowserVerificationCommand],
    ) -> Result<BrowserVerificationRun, AppError> {
        self.require_browser_verification_capability(base_url)
            .await?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            preview_id: &'a str,
            request_id: &'a str,
            commands: &'a [BrowserVerificationCommand],
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser-verification/create"),
            &Body {
                preview_id,
                request_id,
                commands,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 查询 owner 上的验证 run。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     controller 轮询 remote 启动的 run 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require capability → POST get。
    pub async fn get_browser_verification(
        &self,
        base_url: &str,
        run_id: &str,
    ) -> Result<BrowserVerificationRun, AppError> {
        self.require_browser_verification_capability(base_url)
            .await?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            run_id: &'a str,
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser-verification/get"),
            &Body { run_id },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 取消 owner 上的验证 run。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     controller 停止 remote 验证并让 owner 收割 child/profile。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require capability → POST cancel。
    pub async fn cancel_browser_verification(
        &self,
        base_url: &str,
        run_id: &str,
    ) -> Result<BrowserVerificationRun, AppError> {
        self.require_browser_verification_capability(base_url)
            .await?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            run_id: &'a str,
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser-verification/cancel"),
            &Body { run_id },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 拉取 owner 上的验证 artifact（base64 DTO）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     controller UI 展示 remote smoke 截图。
    ///
    /// Code Logic（这个函数做什么）:
    ///     require capability → POST artifact。
    pub async fn get_browser_verification_artifact(
        &self,
        base_url: &str,
        run_id: &str,
        artifact_id: &str,
    ) -> Result<BrowserVerificationArtifactDto, AppError> {
        self.require_browser_verification_capability(base_url)
            .await?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            run_id: &'a str,
            artifact_id: &'a str,
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/browser-verification/artifact"),
            &Body {
                run_id,
                artifact_id,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端项目下的 Git worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 remote shortcut 打开后，需要展示对端项目的主工作区和功能 worktree。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/list`，请求体 `{projectId}`，解析 worktree DTO 列表。
    pub async fn list_worktrees(
        &self,
        base_url: &str,
        project_id: &str,
    ) -> Result<Vec<WorkbenchWorktreeDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/list"),
            &RemoteProjectReq {
                project_id: project_id.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在远端项目中创建 Git worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户对 remote shortcut 点击新建 worktree 时，分支和目录应创建在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/create`，解析远端新建后的 worktree DTO。
    pub async fn create_worktree(
        &self,
        base_url: &str,
        req: RemoteCreateWorktreeReq,
    ) -> Result<WorkbenchWorktreeDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/create"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 获取远端本机 worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     id-only remote worktree 命令需要先知道该 worktree 所属远端 projectId，才能映射回本机 shortcut。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/get`，请求体 `{worktreeId}`，解析单个 worktree DTO。
    pub async fn get_worktree(
        &self,
        base_url: &str,
        worktree_id: &str,
    ) -> Result<WorkbenchWorktreeDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/get"),
            &RemoteWorktreeReq {
                worktree_id: worktree_id.to_string(),
                client_operation_id: None,
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 提交远端本机 worktree 的改动。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 Commit 按钮应在项目所在设备执行真实 git commit 和可选 message 生成。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/commit`，用 very-long timeout 解析提交后的 worktree DTO。
    pub async fn commit_worktree(
        &self,
        base_url: &str,
        req: RemoteCommitWorktreeReq,
    ) -> Result<WorkbenchWorktreeDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/commit"),
            &req,
            commit_worktree_timeout_kind(),
        )
        .await
    }

    /// 推送远端本机 worktree 分支。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 Push 按钮应在远端仓库所在设备执行 git push。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/push`，解析推送后的 worktree DTO。
    pub async fn push_worktree(
        &self,
        base_url: &str,
        worktree_id: &str,
    ) -> Result<WorkbenchWorktreeDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/push"),
            &RemoteWorktreeReq {
                worktree_id: worktree_id.to_string(),
                client_operation_id: None,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 推送远端 worktree，解析 mutation envelope。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新 peer 返回 succeeded|unknown envelope。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST push 并反序列化 WorkbenchMutationEnvelopeDto。
    pub async fn push_worktree_envelope(
        &self,
        base_url: &str,
        worktree_id: &str,
        client_operation_id: Option<String>,
    ) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/push"),
            &RemoteWorktreeReq {
                worktree_id: worktree_id.to_string(),
                client_operation_id,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 合并远端本机 worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 Merge 按钮需要在项目所在设备关闭会话、merge 主工作区并清理 worktree。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/merge`，用 very-long timeout 返回对端 merge result JSON 供命令层映射 ID。
    pub async fn merge_worktree(
        &self,
        base_url: &str,
        worktree_id: &str,
    ) -> Result<Value, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/merge"),
            &RemoteWorktreeReq {
                worktree_id: worktree_id.to_string(),
                client_operation_id: None,
            },
            merge_worktree_timeout_kind(),
        )
        .await
    }

    /// 合并远端 worktree，解析 mutation envelope。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新 peer merge 返回 envelope。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST merge 并解析 envelope。
    pub async fn merge_worktree_envelope(
        &self,
        base_url: &str,
        worktree_id: &str,
        client_operation_id: Option<String>,
    ) -> Result<WorkbenchMutationEnvelopeDto<Value>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/merge"),
            &RemoteWorktreeReq {
                worktree_id: worktree_id.to_string(),
                client_operation_id,
            },
            merge_worktree_timeout_kind(),
        )
        .await
    }

    /// 删除远端本机 worktree。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 删除 worktree 时，真实 git worktree remove 和 metadata 清理必须发生在远端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/worktrees/remove`，返回对端轻量 JSON 供命令层映射 worktreeId。
    pub async fn remove_worktree(
        &self,
        base_url: &str,
        worktree_id: &str,
        force: Option<bool>,
    ) -> Result<Value, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/remove"),
            &RemoteRemoveWorktreeReq {
                worktree_id: worktree_id.to_string(),
                force,
                client_operation_id: None,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 删除远端 worktree，解析 mutation envelope。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新 peer remove 返回 envelope。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST remove 并解析 envelope。
    pub async fn remove_worktree_envelope(
        &self,
        base_url: &str,
        worktree_id: &str,
        force: Option<bool>,
        client_operation_id: Option<String>,
    ) -> Result<WorkbenchMutationEnvelopeDto<Value>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/remove"),
            &RemoteRemoveWorktreeReq {
                worktree_id: worktree_id.to_string(),
                force,
                client_operation_id,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 提交远端 worktree，解析 mutation envelope。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新 peer 返回 succeeded|unknown。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST commit 并解析 envelope。
    pub async fn commit_worktree_envelope(
        &self,
        base_url: &str,
        req: RemoteCommitWorktreeReq,
    ) -> Result<WorkbenchMutationEnvelopeDto<WorkbenchWorktreeDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/commit"),
            &req,
            commit_worktree_timeout_kind(),
        )
        .await
    }

    /// 查询远端 mutation ledger。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     unknown 后向 owning device 取 intent/state。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST mutation-operation。
    ///
    /// 注：桌面 get 当前经 control 代理到 owning sidecar 本地 ledger；本方法供
    /// 未来 remote-aware 查询或对端诊断复用，生产路径暂未挂接。
    #[allow(dead_code)]
    pub async fn get_mutation_operation(
        &self,
        base_url: &str,
        client_operation_id: &str,
    ) -> Result<Option<WorkbenchMutationOperationDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/worktrees/mutation-operation"),
            &RemoteMutationOperationReq {
                client_operation_id: client_operation_id.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 探测对端是否支持某 capability。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mutation-outcome 等新契约只能在 capability 命中时使用。
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

    /// 拉取 owning device 的 LAN Fleet owner batch 摘要。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     控制设备按 device 一次请求已保存 shortcut 对应的 local project 摘要，
    ///     禁止递归调用对端再 fan-out。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability 预检由调用方完成（便于结构化 unsupported）；
    ///     POST `/api/workbench/lan-fleet/snapshot`，body 为 LanFleetOwnerBatchReq。
    pub async fn lan_fleet_snapshot(
        &self,
        base_url: &str,
        req: &LanFleetOwnerBatchReq,
    ) -> Result<LanFleetOwnerBatchResp, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/lan-fleet/snapshot"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 拉取 owning device 的 Agent ledger 时间窗聚合（无 entry 列表）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Fleet join 需要 remote 7d/24h/30d aggregate；旧 peer 由调用方 capability 门控。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability 预检由调用方完成；POST `/api/workbench/agent-ledger/summary`。
    pub async fn agent_ledger_summary(
        &self,
        base_url: &str,
        req: &AgentLedgerSummaryBatchReq,
    ) -> Result<AgentLedgerSummaryBatchResp, AppError> {
        let _ = CAPABILITY_WORKBENCH_AGENT_LEDGER_SUMMARY_V1; // 文档锚点：capability 与路由同名
        self.post_json(
            endpoint_url(base_url, "/api/workbench/agent-ledger/summary"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// owner-local workspace restore preflight。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     控制设备 layout 不上传；把 inner IDs 交给 owning device 纯读探测。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 capability 门控；再 POST `/api/workbench/workspace/restore/preflight`。
    pub async fn preflight_workspace_restore(
        &self,
        base_url: &str,
        req: &RemoteWorkspaceRestorePreflightReq,
    ) -> Result<WorkspaceRestorePlan, AppError> {
        if !self
            .peer_supports_capability(base_url, CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1)
            .await?
        {
            return Err(AppError::unavailable(
                "capability_unsupported:workbench.workspace-safe-restore.v1".to_string(),
            ));
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/workspace/restore/preflight"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// owner-local safe attach。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     apply 时由 owning device 幂等 attach 已有 tmux target。
    ///
    /// Code Logic（这个函数做什么）:
    ///     capability 门控后 POST `/api/workbench/workspace/restore/safe-attach`。
    pub async fn safe_attach_session(
        &self,
        base_url: &str,
        req: &RemoteSafeAttachReq,
    ) -> Result<SafeAttachResult, AppError> {
        if !self
            .peer_supports_capability(base_url, CAPABILITY_WORKBENCH_WORKSPACE_SAFE_RESTORE_V1)
            .await?
        {
            return Err(AppError::unavailable(
                "capability_unsupported:workbench.workspace-safe-restore.v1".to_string(),
            ));
        }
        self.post_json(
            endpoint_url(base_url, "/api/workbench/workspace/restore/safe-attach"),
            req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 列出远端 worktree 的 Git 提交。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Git 历史树必须读取远端 worktree 的真实仓库状态，而不是本机 shortcut 路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/git/commits`，解析提交摘要 DTO 列表。
    pub async fn list_git_commits(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/git/commits"),
            &RemoteGitCommitsReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                limit,
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端项目目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树展开 remote shortcut 时，需要让远端设备按本地文件安全规则读取目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/list-dir`，解析文件节点列表。
    pub async fn list_workbench_dir(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: Option<&str>,
    ) -> Result<Vec<WorkbenchFileNode>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/list-dir"),
            &RemoteListDirReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.map(str::to_string),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 获取远端项目内路径信息。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树选中远端路径后，需要读取远端 metadata 供详情面板展示。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/info`，解析 `WorkbenchPathInfo`。
    pub async fn workbench_path_info(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: &str,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/info"),
            &RemotePathInfoReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 打开远端项目内文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端文件的检测、预览和文本读取必须由远端设备执行并返回统一 DTO。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/open`，解析完整文件打开响应。
    pub async fn open_file(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
        path: &str,
    ) -> Result<WorkbenchOpenFileDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/open"),
            &RemoteOpenFileReq {
                project_id: project_id.to_string(),
                worktree_id: worktree_id.map(str::to_string),
                path: path.to_string(),
            },
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 保存远端项目内文本文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端文件编辑保存需要把 content 和 baseHash 发送到远端设备执行原子写入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/save-text`，解析保存后的 metadata/hash。
    pub async fn save_text_file(
        &self,
        base_url: &str,
        req: RemoteSaveTextReq,
    ) -> Result<WorkbenchSaveTextResultDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/save-text"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 预览远端项目内 SQLite 文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端 SQLite 换表预览必须在远端设备读取数据库，避免误读本机同路径文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/preview-sqlite`，解析只读 SQLite 预览 DTO。
    pub async fn preview_sqlite_file(
        &self,
        base_url: &str,
        req: RemotePreviewSqliteReq,
    ) -> Result<WorkbenchSqlitePreview, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/preview-sqlite"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 读取远端项目内 HTML/Markdown 预览资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端 HTML/Markdown 预览中的相对 CSS/图片必须从远端 worktree 根内安全读取。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/preview-html-asset`，解析可内联的 data URL 资源 DTO。
    pub async fn preview_html_asset(
        &self,
        base_url: &str,
        req: RemotePreviewHtmlAssetReq,
    ) -> Result<WorkbenchHtmlAssetDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/preview-html-asset"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在远端项目内创建文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树的新建文件动作需要在远端磁盘上创建空文件并返回 metadata。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/create-file`，解析 `WorkbenchPathInfo`。
    pub async fn create_file(
        &self,
        base_url: &str,
        req: RemoteCreatePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/create-file"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 在远端项目内创建目录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机文件树的新建目录动作需要在远端磁盘上执行并返回 metadata。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/create-dir`，解析 `WorkbenchPathInfo`。
    pub async fn create_dir(
        &self,
        base_url: &str,
        req: RemoteCreatePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/create-dir"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 重命名远端项目内路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树重命名 remote 文件/目录时，真实操作必须发生在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/rename`，解析重命名后的 `WorkbenchPathInfo`。
    pub async fn rename_path(
        &self,
        base_url: &str,
        req: RemoteRenamePathReq,
    ) -> Result<WorkbenchPathInfo, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/rename"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 删除远端项目内路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     文件树删除 remote 文件/目录时，远端设备必须复用本地删除安全规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/files/delete`，解析轻量 `{ok,path}` 响应。
    pub async fn delete_path(
        &self,
        base_url: &str,
        req: RemoteDeletePathReq,
    ) -> Result<serde_json::Value, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/files/delete"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 列出远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 remote shortcut 进入项目后，需要展示该远端项目下已有的 terminal window。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/list`，请求体 `{projectId?}`，解析 session DTO 列表。
    pub async fn list_sessions(
        &self,
        base_url: &str,
        project_id: Option<&str>,
    ) -> Result<Vec<WorkbenchSessionDto>, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/list"),
            &RemoteListSessionsReq {
                project_id: project_id.map(str::to_string),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 创建远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在 remote shortcut 上新建 terminal window 时，真实 PTY/tmux 必须创建在远端设备。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/create`，解析远端创建出的 session DTO。
    pub async fn create_session(
        &self,
        base_url: &str,
        req: RemoteCreateSessionReq,
    ) -> Result<WorkbenchSessionDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/create"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// 拉取远端终端最近输出。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     手机端首次连接 remote terminal 时，需要先拿到远端 session 的 replay buffer，再接 live 事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/replay`，解析对端 WorkbenchSessionReplay DTO。
    pub async fn replay(
        &self,
        base_url: &str,
        session_id: &str,
    ) -> Result<WorkbenchSessionReplayDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/replay"),
            &RemoteReplaySessionReq {
                session_id: session_id.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 向远端终端写入输入。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 xterm 捕获键盘输入后，需要转发到远端设备的对应 PTY writer。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/write`，成功后忽略对端 `{ok}` 响应。
    pub async fn write_input(
        &self,
        base_url: &str,
        session_id: &str,
        data: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/write"),
                &RemoteWriteSessionInputReq {
                    session_id: session_id.to_string(),
                    data: data.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 调整远端终端尺寸。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 terminal viewport 变化时，远端 PTY/tmux 也需要收到新的 cols/rows。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/resize`，成功后忽略对端 `{ok}` 响应。
    pub async fn resize(
        &self,
        base_url: &str,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/resize"),
                &RemoteResizeSessionReq {
                    session_id: session_id.to_string(),
                    cols,
                    rows,
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 聚焦远端终端 window。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机顶部 tab 切换到 remote terminal 时，远端 tmux current window 需要同步切换。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/focus`，成功后忽略对端 `{ok}` 响应。
    pub async fn focus(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/focus"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 查询远端当前聚焦终端 window。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户在远端 tmux status bar 内切换 window 后，本机 UI 需要知道当前 active session。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/focused`，解析远端 local sessionId 或 None。
    pub async fn focused(
        &self,
        base_url: &str,
        project_id: &str,
        worktree_id: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let response: RemoteFocusedSessionResp = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/focused"),
                &RemoteFocusedSessionReq {
                    project_id: project_id.to_string(),
                    worktree_id: worktree_id.map(str::to_string),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(response.session_id)
    }

    /// 创建远端 tmux pane 分屏。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote terminal 需要复用远端 tmux 的真实 pane 布局能力。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/split-pane`，成功后忽略对端 `{ok}` 响应。
    pub async fn split_pane(
        &self,
        base_url: &str,
        session_id: &str,
        direction: &str,
    ) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/split-pane"),
                &RemoteSplitPaneReq {
                    session_id: session_id.to_string(),
                    direction: direction.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 切换远端当前 window 到下一个 pane。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote terminal 用户在多个 pane 间切换时，active pane 状态必须由远端 tmux 更新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/switch-pane`，成功后忽略对端 `{ok}` 响应。
    pub async fn switch_pane(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/switch-pane"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 按坐标选中远端 window 内的 pane。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote terminal 用户点击某个 pane 时，坐标命中与 select-pane 都必须由 owning device 的 tmux 完成。
    ///     该操作以绝对坐标定位，重复执行结果一致，与相对 `.+` 循环不同。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/select-pane-at`，解析对端 `{paneId, changed}` 响应。
    pub async fn select_pane_at(
        &self,
        base_url: &str,
        session_id: &str,
        col: u32,
        row: u32,
    ) -> Result<RemoteSelectPaneAtResp, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/select-pane-at"),
            &RemoteSelectPaneAtReq {
                session_id: session_id.to_string(),
                col,
                row,
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 确保远端当前 active pane 以单 pane 视图显示。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     mobile terminal 连接远端项目时，也要隐藏 tmux 分屏布局，只显示远端 active pane。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/zoom-pane`，成功后忽略对端 `{ok}` 响应。
    pub async fn zoom_pane(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/zoom-pane"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 关闭远端当前 pane。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭 remote terminal pane 时，真实 kill-pane/close-window 应在远端设备执行。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/close-pane`，解析 `closedWindow` 布尔值。
    pub async fn close_pane(&self, base_url: &str, session_id: &str) -> Result<bool, AppError> {
        let response: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/close-pane"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(response
            .get("closedWindow")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// 关闭远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户关闭 remote terminal tab 时，远端 registry、SQLite 和 PTY/tmux 后端都应清理。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/close`，成功后忽略对端 `{ok}` 响应。
    pub async fn close_session(&self, base_url: &str, session_id: &str) -> Result<(), AppError> {
        let _: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/sessions/close"),
                &RemoteSessionReq {
                    session_id: session_id.to_string(),
                },
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        Ok(())
    }

    /// 重命名远端终端会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     用户给 remote terminal tab 改名时，远端 tmux window 与持久化 row 需要同步更新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/sessions/rename`，解析更新后的 session DTO。
    pub async fn rename_session(
        &self,
        base_url: &str,
        session_id: &str,
        name: &str,
    ) -> Result<WorkbenchSessionDto, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/sessions/rename"),
            &RemoteRenameSessionReq {
                session_id: session_id.to_string(),
                name: name.to_string(),
            },
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 流式优化 Prompt 并写入远端终端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 remote shortcut 的 Prompt 优化浮层应在项目所在设备读取 CLAUDE.md 并写入该设备 terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/prompt-optimizer/stream-to-session`，用 very-long timeout 等待对端 CLI 流式完成。
    pub async fn stream_prompt_optimizer_to_session(
        &self,
        base_url: &str,
        req: RemotePromptOptimizerReq,
    ) -> Result<Value, AppError> {
        self.post_json(
            endpoint_url(
                base_url,
                "/api/workbench/prompt-optimizer/stream-to-session",
            ),
            &req,
            prompt_optimizer_timeout_kind(),
        )
        .await
    }

    /// 搜索远端设备本机 worktree 内的 Claude Code 历史 session。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机在 remote shortcut 上搜索历史 Claude 会话时，transcript 解析必须在项目所在设备完成。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/claude-sessions/search`，short timeout；
    ///     先按 JSON Value 取成功 body，再 `decode_session_search_response_body` 兼容
    ///     v2 对象 DTO 与 legacy `Vec<SessionSearchHit>` 数组。
    pub async fn search_claude_sessions(
        &self,
        base_url: &str,
        req: RemoteSearchClaudeSessionsReq,
    ) -> Result<SessionSearchResult, AppError> {
        let value: serde_json::Value = self
            .post_json(
                endpoint_url(base_url, "/api/workbench/claude-sessions/search"),
                &req,
                RemoteRequestTimeoutKind::Short,
            )
            .await?;
        let bytes = serde_json::to_vec(&value).map_err(|e| {
            AppError::generic(format!("远端 Claude session 搜索响应再序列化失败: {e}"))
        })?;
        decode_session_search_response_body(&bytes)
            .map_err(|e| AppError::generic(format!("远端 Claude session 搜索响应解码失败: {e}")))
    }

    /// 读取远端单个 Claude session 的 preview 详情。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 的 preview 面板需要展示远端会话最近消息、cwd 等，只能由项目所在设备解析 transcript。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/claude-sessions/preview`，用 short timeout 解析 SessionPreview。
    pub async fn get_claude_session_preview(
        &self,
        base_url: &str,
        req: RemoteClaudeSessionReq,
    ) -> Result<SessionPreview, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/claude-sessions/preview"),
            &req,
            RemoteRequestTimeoutKind::Short,
        )
        .await
    }

    /// 在远端设备 resume 一个历史 Claude session。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote shortcut 选中历史会话后，真实 terminal + `claude --resume` 必须在项目所在设备启动；
    ///     返回的 sessionId 是远端新建 terminal 的 inner id，由发起方命令层包装成统一 remote sessionId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/workbench/claude-sessions/resume`，用 long timeout（120s）覆盖远端 CLI 检测 + 建会话。
    pub async fn resume_claude_session(
        &self,
        base_url: &str,
        req: RemoteClaudeSessionReq,
    ) -> Result<ResumeClaudeSessionResult, AppError> {
        self.post_json(
            endpoint_url(base_url, "/api/workbench/claude-sessions/resume"),
            &req,
            RemoteRequestTimeoutKind::Long,
        )
        .await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench GET 调用都需要统一处理网络错误、HTTP 状态码和 JSON 解析错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 GET 请求（附出站 request_id header，Finding 3：多跳调用链关联），
    ///     非成功状态转中文业务错误，成功后解析 JSON 为目标类型。request_id 优先转发
    ///     `forwarded_request_id`（多跳代理），缺失时生成新 UUID。
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
        let response = req.send().await.map_err(map_remote_send_error)?;
        parse_json_response(response).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端路径类 POST 调用都使用相同的 `{path}` 请求体和响应解析规则。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 JSON body `{path}`，非成功状态转中文业务错误，成功后解析 JSON 为目标类型。
    async fn post_path_json<T>(
        &self,
        url: String,
        path: &str,
        timeout_kind: RemoteRequestTimeoutKind,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let body = serde_json::json!({ "path": path });
        self.post_json(url, &body, timeout_kind).await
    }

    /// Business Logic（为什么需要这个函数）:
    ///     远端 Workbench POST 调用大多使用不同 DTO 请求体，但错误处理与 JSON 解析规则一致。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 JSON body（附出站 request_id header，Finding 3：多跳调用链关联），
    ///     非成功状态转中文业务错误，成功后按泛型解析 JSON。request_id 优先转发
    ///     `forwarded_request_id`（多跳代理），缺失时生成新 UUID。
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
        let response = req.send().await.map_err(map_remote_send_error)?;
        parse_json_response(response).await
    }
}

/// Business Logic（为什么需要这个函数）:
///     get_json/post_json 收到完整 endpoint URL，能力探测只需 origin base。
///
/// Code Logic（这个函数做什么）:
///     用 `url::Url`/`reqwest::Url` 解析 scheme/host/port，拼 `scheme://host:port`。
fn origin_base_url(full_url: &str) -> Result<String, AppError> {
    let parsed = reqwest::Url::parse(full_url)
        .map_err(|err| AppError::generic(format!("远端 Workbench URL 无效: {err}")))?;
    let scheme = parsed.scheme();
    let host = parsed
        .host_str()
        .ok_or_else(|| AppError::generic("远端 Workbench URL 缺少 host"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| AppError::generic("远端 Workbench URL 缺少 port"))?;
    Ok(format!("{scheme}://{host}:{port}"))
}

/// Business Logic: legacy mutation 路径需要区分 timeout vs network，才能映射 unknown envelope。
/// Code Logic: reqwest error.is_timeout() → AppError::timeout，其它 send 失败 → unavailable。
fn map_remote_send_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        AppError::timeout(format!("远端 Workbench 请求超时: {error}"))
    } else {
        // 非超时 send 失败属于传输离线，供 preflight/outbox 按 Unavailable 分支。
        AppError::unavailable(format!("远端 Workbench 请求失败: {error}"))
    }
}

impl Default for RemoteWorkbenchClient {
    /// 创建默认 Workbench 远端客户端。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     调用方在需要默认客户端时可以复用标准构造逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `RemoteWorkbenchClient::new` 返回带默认超时的客户端。
    fn default() -> Self {
        Self::new()
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端 Workbench 需要同时支持快速浏览和耗时写入，不能用单一 client-level timeout 限制所有接口。
///
/// Code Logic（这个函数做什么）:
///     将请求类别映射为具体 Duration，供每个 request builder 单独设置超时。
fn remote_request_timeout(kind: RemoteRequestTimeoutKind) -> Duration {
    match kind {
        RemoteRequestTimeoutKind::Short => Duration::from_secs(SHORT_REMOTE_WORKBENCH_TIMEOUT_SECS),
        RemoteRequestTimeoutKind::Long => Duration::from_secs(LONG_REMOTE_WORKBENCH_TIMEOUT_SECS),
        RemoteRequestTimeoutKind::VeryLong => {
            Duration::from_secs(VERY_LONG_REMOTE_WORKBENCH_TIMEOUT_SECS)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端 commit 可能在对端运行 180s commit message 生成，本机 HTTP 客户端不能提前超时。
///
/// Code Logic（这个函数做什么）:
///     返回 commit-worktree 请求专用的超长 timeout 类别，供方法和测试复用。
fn commit_worktree_timeout_kind() -> RemoteRequestTimeoutKind {
    RemoteRequestTimeoutKind::VeryLong
}

/// Business Logic（为什么需要这个函数）:
///     远端 merge 冲突处理可能在对端运行 300s Claude Code 流程，本机 HTTP 客户端不能提前超时。
///
/// Code Logic（这个函数做什么）:
///     返回 merge-worktree 请求专用的超长 timeout 类别，供方法和测试复用。
fn merge_worktree_timeout_kind() -> RemoteRequestTimeoutKind {
    RemoteRequestTimeoutKind::VeryLong
}

/// Business Logic（为什么需要这个函数）:
///     远端 Prompt 优化会包住对端 180s Claude CLI 流式任务，本机 HTTP 客户端不能提前超时。
///
/// Code Logic（这个函数做什么）:
///     返回 Prompt 优化代理请求专用的超长 timeout 类别，供方法和测试复用。
fn prompt_optimizer_timeout_kind() -> RemoteRequestTimeoutKind {
    RemoteRequestTimeoutKind::VeryLong
}

/// Business Logic（为什么需要这个函数）:
///     调用方可能传入带尾斜杠的 base URL，远端客户端应始终拼出唯一规范路径。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/`，再追加以 `/` 开头的 API path。
fn endpoint_url(base_url: &str, path: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), path)
}

/// Business Logic（为什么需要这个函数 / Finding 3）:
///     所有远端 Workbench 响应都需要统一错误语义，避免各方法返回不同格式的错误文案。
///     旧实现用 `response.text()` + 字符串解析 `{error}`，丢弃了对端信封的
///     code/status/retryable/request_id，导致上层重试只能靠文案匹配。现改用共享
///     `net::peer_error::parse_peer_response` 统一解析 v1 信封/v0 老形态，并经
///     `peer_call_error_to_app_error` 把 `Remote` 转为 `AppError::remote`，保留结构化元数据。
///
/// Code Logic（这个函数做什么）:
///     委托 `parse_peer_response` 一次性消费 status/header request_id/body bytes，
///     成功时按泛型解析 JSON；失败时按 `PeerCallError` 变体经共享 helper 映射为 `AppError`。
async fn parse_json_response<T>(response: reqwest::Response) -> Result<T, AppError>
where
    T: DeserializeOwned,
{
    let url = response.url().as_str().to_string();
    crate::net::peer_error::parse_peer_response::<T>(response, &url)
        .await
        .map_err(|err| crate::net::peer_error::peer_call_error_to_app_error(err, "远端 Workbench"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::{
        WorkbenchHtmlAssetDto, WorkbenchPathInfo, WorkbenchRemoteDirectoryEntryDto,
        WorkbenchSaveTextResultDto, WorkbenchSessionDto, WorkbenchSqlitePreview,
        WorkbenchWorktreeDto,
    };
    use crate::workbench::remote_protocol::{
        RemoteCreateSessionReq, RemotePreviewHtmlAssetReq, RemotePreviewSqliteReq,
        RemotePromptOptimizerReq,
    };
    use axum::extract::State;
    use axum::routing::post;
    use axum::{http::StatusCode, Json, Router};
    use serde_json::Value;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    /// Business Logic（为什么需要这个函数）:
    ///     远端客户端测试需要一个本地 HTTP 服务来验证请求路径、请求体和响应解析。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启动临时 axum server，记录收到的 JSON body，并返回本地 base URL 与共享记录。
    async fn spawn_list_dir_server() -> (String, Arc<Mutex<Option<Value>>>) {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/fs/list",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(vec![WorkbenchRemoteDirectoryEntryDto {
                            name: "src".to_string(),
                            path: "/tmp/app/src".to_string(),
                            kind: "dir".to_string(),
                            modified_at: None,
                            is_git_repo: false,
                        }])
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), seen_body)
    }

    /// Business Logic（为什么需要这个测试）:
    ///     浏览目录等短操作不应被长时间阻塞，但保存文件、创建 worktree 等远端重操作需要更宽松的等待窗口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接校验远端请求超时策略 helper，确保短/长两类请求不会共用单一 client-level timeout。
    #[test]
    fn remote_request_timeout_separates_short_and_long_operations() {
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::Short),
            Duration::from_secs(15)
        );
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::Long),
            Duration::from_secs(120)
        );
        assert_eq!(
            remote_request_timeout(RemoteRequestTimeoutKind::VeryLong),
            Duration::from_secs(420)
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 commit/merge 会包住本机 180s commit message 与 300s merge 冲突处理流程，HTTP 超时不能先断开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     校验 commit/merge 的 timeout kind 均使用 very-long，且具体秒数覆盖本机长操作上限。
    #[test]
    fn commit_and_merge_use_very_long_timeout() {
        assert_eq!(
            commit_worktree_timeout_kind(),
            RemoteRequestTimeoutKind::VeryLong
        );
        assert_eq!(
            merge_worktree_timeout_kind(),
            RemoteRequestTimeoutKind::VeryLong
        );
        assert!(remote_request_timeout(commit_worktree_timeout_kind()) >= Duration::from_secs(180));
        assert!(remote_request_timeout(merge_worktree_timeout_kind()) >= Duration::from_secs(300));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Prompt 优化会包住对端 Claude CLI 流式任务，本机客户端必须使用覆盖 180 秒的超长超时。
    ///
    /// Code Logic（这个测试做什么）:
    ///     校验 Prompt 优化代理请求 timeout kind 使用 very-long，避免未来误改成长短请求超时。
    #[test]
    fn prompt_optimizer_uses_very_long_timeout() {
        assert_eq!(
            prompt_optimizer_timeout_kind(),
            RemoteRequestTimeoutKind::VeryLong
        );
        assert!(
            remote_request_timeout(prompt_optimizer_timeout_kind()) >= Duration::from_secs(180)
        );
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     远端路由会返回本地业务错误（v0 老形态 `{error}` 或 v1 信封）；客户端必须保留这些
    ///     错误文案给前端展示，且**保留** code/status/retryable/request_id 供上层决策。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回 400 + 老形态 `{error}`，断言远端客户端提取 error 字段（而非只报 HTTP 状态），
    ///     且 `remote_meta()` 暴露 legacy 合成 code 与 HTTP 状态。
    #[tokio::test]
    async fn parse_json_response_uses_remote_error_field() {
        let app = Router::new().route(
            "/api/workbench/worktrees/list",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "远端项目必须是本机项目" })),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let error = RemoteWorkbenchClient::new()
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect_err("non-success JSON error should fail");

        assert_eq!(error.to_string(), "远端项目必须是本机项目");
        // Finding 3: legacy 老形态被合成 code=legacy.remote_error，状态码保留。
        let meta = error.remote_meta().expect("远端业务错误应携带结构化 meta");
        assert_eq!(meta.status, 400);
        assert_eq!(meta.code, "legacy.remote_error");
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     非 JSON 错误响应（代理 HTML/纯文本）必须归为协议违例（InvalidResponse），
    ///     不能误判为对端业务错误。旧实现把它当 `generic` 并塞进正文摘要；现经统一解析
    ///     归为 InvalidResponse → generic，文案含 url 上下文与"无法解析"说明。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时服务返回 502 + 纯文本 body，断言错误为 generic（非 Remote）、含 url 上下文、
    ///     不携带 remote_meta（防止把代理错误当业务失败重试）。
    #[tokio::test]
    async fn parse_json_response_uses_plain_body_fallback() {
        let app = Router::new().route(
            "/api/workbench/worktrees/list",
            post(|| async { (StatusCode::BAD_GATEWAY, "plain upstream failure") }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let error = RemoteWorkbenchClient::new()
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect_err("non-success plain body should fail");
        let message = error.to_string();

        // 文案含 url 上下文与"无法解析"说明（InvalidResponse 语义）。
        assert!(
            message.contains("无法解析"),
            "非 JSON body 应归为 InvalidResponse: {message}"
        );
        assert!(message.contains("127.0.0.1"), "应含 url 上下文: {message}");
        // 关键：非 JSON 不应被当 Remote（不能携带 meta，避免上层误重试）。
        assert!(
            error.remote_meta().is_none(),
            "InvalidResponse 不应携带 remote meta"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机通过远端目录选择器浏览对端目录时，必须调用约定的 HTTP 路由并发送 `{path}` 请求体。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务，调用 `list_dir`，断言请求体 path 正确且响应 DTO 被解析。
    #[tokio::test]
    async fn list_dir_posts_path_and_parses_entries() {
        let (base_url, seen_body) = spawn_list_dir_server().await;
        let client = RemoteWorkbenchClient::new();

        let entries = client.list_dir(&base_url, "/tmp/app").await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "src");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["path"], "/tmp/app");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备发现拿到的 base URL 未来可能携带尾斜杠，客户端不能因此产生双斜杠路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入带尾斜杠的 base URL，断言拼出的 API URL 只保留一个路径分隔。
    #[test]
    fn endpoint_url_trims_trailing_slash() {
        let url = endpoint_url("http://127.0.0.1:1420/", "/api/workbench/fs/roots");

        assert_eq!(url, "http://127.0.0.1:1420/api/workbench/fs/roots");
    }

    mod browser {
        use super::*;
        use crate::workbench::browser_models::{
            WorkbenchBrowserDiscovery, WorkbenchBrowserPreview, WorkbenchBrowserTarget,
            WorkbenchBrowserTargetSource,
        };
        use crate::workbench::remote_protocol::{
            RemoteWorkbenchBrowserDiscoverReq, RemoteWorkbenchBrowserPreviewReq,
        };

        /// Business Logic（为什么需要这个测试）:
        ///     本机 remote shortcut 发现浏览器候选时，必须把远端 local projectId/worktreeId 原样发到 owning device。
        ///
        /// Code Logic（这个测试做什么）:
        ///     启动一条 discover route，记录 JSON body，调用 remote client 后断言 projectId/worktreeId 和响应解析。
        #[tokio::test]
        async fn browser_discover_posts_project_and_worktree() {
            let seen_body = Arc::new(Mutex::new(None));
            let app = Router::new()
                .route(
                    "/api/workbench/browser/discover",
                    post(
                        |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                         Json(body): Json<Value>| async move {
                            *seen_body.lock().unwrap() = Some(body);
                            Json(WorkbenchBrowserDiscovery {
                                project_id: "inner-project".to_string(),
                                worktree_id: Some("inner-worktree".to_string()),
                                targets: vec![WorkbenchBrowserTarget {
                                    id: "target-1".to_string(),
                                    url: "http://127.0.0.1:5173/".to_string(),
                                    display_url: "http://127.0.0.1:5173/".to_string(),
                                    source: WorkbenchBrowserTargetSource::Remembered,
                                    label: "remembered".to_string(),
                                    reachable: true,
                                }],
                                selected_target_id: Some("target-1".to_string()),
                            })
                        },
                    ),
                )
                .with_state(seen_body.clone());
            let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = RemoteWorkbenchClient::new();

            let discovery = client
                .discover_browser_targets(
                    &format!("http://{addr}"),
                    &RemoteWorkbenchBrowserDiscoverReq {
                        project_id: "inner-project".to_string(),
                        worktree_id: Some("inner-worktree".to_string()),
                    },
                )
                .await
                .unwrap();

            assert_eq!(discovery.project_id, "inner-project");
            assert_eq!(discovery.targets.len(), 1);
            let body = seen_body.lock().unwrap().clone().unwrap();
            assert_eq!(body["projectId"], "inner-project");
            assert_eq!(body["worktreeId"], "inner-worktree");
        }

        /// Business Logic（为什么需要这个测试）:
        ///     远端 preview 必须先在 owning device 创建，remote client 需要把用户选中的 targetUrl 发给 owner。
        ///
        /// Code Logic（这个测试做什么）:
        ///     启动一条 preview route，记录 JSON body，调用 remote client 后断言 targetUrl 和 preview DTO 解析。
        #[tokio::test]
        async fn browser_preview_posts_target_url() {
            let seen_body = Arc::new(Mutex::new(None));
            let app = Router::new()
                .route(
                    "/api/workbench/browser/preview",
                    post(
                        |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                         Json(body): Json<Value>| async move {
                            *seen_body.lock().unwrap() = Some(body);
                            Json(WorkbenchBrowserPreview {
                                preview_id: "remote-preview".to_string(),
                                project_id: "inner-project".to_string(),
                                worktree_id: Some("inner-worktree".to_string()),
                                target_url: "http://127.0.0.1:5173/".to_string(),
                                desktop_proxy_url:
                                    "http://127.0.0.1:62116/api/workbench/browser/proxy/remote-preview/"
                                        .to_string(),
                                mobile_proxy_path:
                                    "/api/mobile/workbench/browser/proxy/remote-preview/"
                                        .to_string(),
                                expires_at_ms: 1_800_000,
                            })
                        },
                    ),
                )
                .with_state(seen_body.clone());
            let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = RemoteWorkbenchClient::new();

            let preview = client
                .create_browser_preview(
                    &format!("http://{addr}"),
                    &RemoteWorkbenchBrowserPreviewReq {
                        project_id: "inner-project".to_string(),
                        worktree_id: Some("inner-worktree".to_string()),
                        target_url: "http://127.0.0.1:5173/".to_string(),
                    },
                )
                .await
                .unwrap();

            assert_eq!(preview.preview_id, "remote-preview");
            let body = seen_body.lock().unwrap().clone().unwrap();
            assert_eq!(body["projectId"], "inner-project");
            assert_eq!(body["worktreeId"], "inner-worktree");
            assert_eq!(body["targetUrl"], "http://127.0.0.1:5173/");
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Workbench 文件保存必须沿用本地保存的乐观锁语义，调用方需要确认请求体字段名与前端/Rust DTO 一致。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 save-text 请求，断言 client 发送 camelCase body 并解析保存结果。
    #[tokio::test]
    async fn save_text_file_posts_camel_case_body_and_parses_result() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/files/save-text",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSaveTextResultDto {
                            metadata: WorkbenchPathInfo {
                                name: "note.md".to_string(),
                                path: "docs/note.md".to_string(),
                                kind: "file".to_string(),
                                size: Some(7),
                                modified_at: None,
                            },
                            base_hash: "new-hash".to_string(),
                            base_modified_at: None,
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let result = client
            .save_text_file(
                &format!("http://{addr}"),
                RemoteSaveTextReq {
                    project_id: "project-1".to_string(),
                    worktree_id: Some("worktree-1".to_string()),
                    path: "docs/note.md".to_string(),
                    content: "# Note\n".to_string(),
                    base_hash: "old-hash".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.base_hash, "new-hash");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "project-1");
        assert_eq!(body["worktreeId"], "worktree-1");
        assert_eq!(body["baseHash"], "old-hash");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 SQLite 换表预览必须调用远端设备，不能退回本机路径读取。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 preview-sqlite 请求，断言 camelCase body 并解析预览 DTO。
    #[tokio::test]
    async fn preview_sqlite_file_posts_camel_case_body_and_parses_result() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/files/preview-sqlite",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSqlitePreview {
                            tables: vec!["notes".to_string()],
                            selected_table: Some("notes".to_string()),
                            columns: vec!["title".to_string()],
                            rows: vec![vec!["hello".to_string()]],
                            truncated: false,
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let result = client
            .preview_sqlite_file(
                &format!("http://{addr}"),
                RemotePreviewSqliteReq {
                    project_id: "project-1".to_string(),
                    worktree_id: Some("worktree-1".to_string()),
                    path: "data/app.sqlite".to_string(),
                    table: Some("notes".to_string()),
                    limit_rows: Some(50),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.selected_table.as_deref(), Some("notes"));
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "project-1");
        assert_eq!(body["worktreeId"], "worktree-1");
        assert_eq!(body["path"], "data/app.sqlite");
        assert_eq!(body["table"], "notes");
        assert_eq!(body["limitRows"], 50);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 HTML/Markdown 预览资源必须从远端项目根内读取，避免本机同路径资源污染预览。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 preview-html-asset 请求，断言 documentPath/assetPath 和响应解析。
    #[tokio::test]
    async fn preview_html_asset_posts_camel_case_body_and_parses_result() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/files/preview-html-asset",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchHtmlAssetDto {
                            path: "docs/style.css".to_string(),
                            mime: "text/css".to_string(),
                            size: 12,
                            data_url: "data:text/css;base64,LmEge30=".to_string(),
                            text: Some(".a {}".to_string()),
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let result = client
            .preview_html_asset(
                &format!("http://{addr}"),
                RemotePreviewHtmlAssetReq {
                    project_id: "project-1".to_string(),
                    worktree_id: Some("worktree-1".to_string()),
                    document_path: "docs/index.html".to_string(),
                    asset_path: "./style.css".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.mime, "text/css");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "project-1");
        assert_eq!(body["worktreeId"], "worktree-1");
        assert_eq!(body["documentPath"], "docs/index.html");
        assert_eq!(body["assetPath"], "./style.css");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机打开远端项目后需要通过远端 local project id 读取对端 worktree 列表。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时 HTTP 服务返回一个 worktree DTO，断言 client 发送 projectId 并解析响应。
    #[tokio::test]
    async fn list_worktrees_posts_project_id_and_parses_items() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/worktrees/list",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(vec![WorkbenchWorktreeDto {
                            id: "inner-main".to_string(),
                            project_id: "inner-project".to_string(),
                            name: "main".to_string(),
                            branch: Some("main".to_string()),
                            base_branch: None,
                            path: "/repo".to_string(),
                            is_main: true,
                            status: Default::default(),
                            created_at: "2026-06-26T00:00:00Z".to_string(),
                            updated_at: "2026-06-26T00:00:00Z".to_string(),
                        }])
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let items = client
            .list_worktrees(&format!("http://{addr}"), "inner-project")
            .await
            .unwrap();

        assert_eq!(items[0].id, "inner-main");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "inner-project");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机 remote shortcut 创建 terminal window 时，真实会话必须创建在远端设备的 local project 下。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 sessions/create 请求，断言 client 发送 camelCase body 并解析 session DTO。
    #[tokio::test]
    async fn create_session_posts_camel_case_body_and_parses_session() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/sessions/create",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSessionDto {
                            id: "inner-session".to_string(),
                            project_id: "inner-project".to_string(),
                            worktree_id: Some("inner-worktree".to_string()),
                            name: "Remote App".to_string(),
                            command: "/bin/zsh".to_string(),
                            cwd: "/repo".to_string(),
                            status: "running".to_string(),
                            cols: 120,
                            rows: 36,
                            started_at: "2026-06-26T00:00:00Z".to_string(),
                            exited_at: None,
                            exit_code: None,
                            supports_panes: true,
                            pane_count: 1,
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let session = client
            .create_session(
                &format!("http://{addr}"),
                RemoteCreateSessionReq {
                    project_id: "inner-project".to_string(),
                    worktree_id: Some("inner-worktree".to_string()),
                    initial_cols: Some(120),
                    initial_rows: Some(36),
                },
            )
            .await
            .unwrap();

        assert_eq!(session.id, "inner-session");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["projectId"], "inner-project");
        assert_eq!(body["worktreeId"], "inner-worktree");
        assert_eq!(body["initialCols"], 120);
        assert_eq!(body["initialRows"], 36);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端首次打开 remote terminal 时，需要通过远端 replay route 拉取最近输出。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 sessions/replay 请求，断言 client 发送 sessionId 并解析 replay DTO。
    #[tokio::test]
    async fn replay_posts_session_id_to_replay_route() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/sessions/replay",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(WorkbenchSessionReplayDto {
                            session_id: "inner-session".to_string(),
                            buffer: "hello\n".to_string(),
                            truncated: false,
                            last_seq: 42,
                            owner_instance_id: Some("owner-remote".to_string()),
                        })
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let replay = client
            .replay(&format!("http://{addr}"), "inner-session")
            .await
            .unwrap();

        assert_eq!(replay.session_id, "inner-session");
        assert_eq!(replay.buffer, "hello\n");
        assert_eq!(replay.last_seq, 42);
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["sessionId"], "inner-session");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机 remote shortcut 切换 pane 时，真实 active pane 必须在远端 tmux window 内更新。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 switch-pane 请求，断言 client 调用约定路径并发送 camelCase sessionId。
    #[tokio::test]
    async fn switch_pane_posts_session_id_to_switch_pane_route() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/sessions/switch-pane",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(serde_json::json!({
                            "ok": true,
                            "sessionId": "inner-session"
                        }))
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        client
            .switch_pane(&format!("http://{addr}"), "inner-session")
            .await
            .unwrap();

        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["sessionId"], "inner-session");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端远端 terminal 的单 pane 视图需要通过远端 tmux zoom 实现。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 zoom-pane 请求，断言 client 调用约定路径并发送 camelCase sessionId。
    #[tokio::test]
    async fn zoom_pane_posts_session_id_to_zoom_pane_route() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/sessions/zoom-pane",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(serde_json::json!({
                            "ok": true,
                            "sessionId": "inner-session"
                        }))
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        client
            .zoom_pane(&format!("http://{addr}"), "inner-session")
            .await
            .unwrap();

        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["sessionId"], "inner-session");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机 remote shortcut 触发 Prompt 优化时，请求体必须保留远端工作目录和远端 local sessionId。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动临时 HTTP 服务接收 prompt-optimizer 请求，断言 camelCase body 与响应解析正确。
    #[tokio::test]
    async fn stream_prompt_optimizer_posts_remote_context_and_parses_json() {
        let seen_body = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/prompt-optimizer/stream-to-session",
                post(
                    |State(seen_body): State<Arc<Mutex<Option<Value>>>>,
                     Json(body): Json<Value>| async move {
                        *seen_body.lock().unwrap() = Some(body);
                        Json(serde_json::json!({
                            "ok": true,
                            "sessionId": "inner-session"
                        }))
                    },
                ),
            )
            .with_state(seen_body.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = RemoteWorkbenchClient::new();

        let result = client
            .stream_prompt_optimizer_to_session(
                &format!("http://{addr}"),
                RemotePromptOptimizerReq {
                    prompt: "优化这个任务".to_string(),
                    working_directory: Some("/remote/repo".to_string()),
                    target_language: "zh".to_string(),
                    session_id: "inner-session".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(result["sessionId"], "inner-session");
        let body = seen_body.lock().unwrap().clone().unwrap();
        assert_eq!(body["prompt"], "优化这个任务");
        assert_eq!(body["workingDirectory"], "/remote/repo");
        assert_eq!(body["targetLanguage"], "zh");
        assert_eq!(body["sessionId"], "inner-session");
    }

    /// Business Logic（为什么需要这个测试 / Finding 3）:
    ///     Workbench 远端客户端同样要支持 `with_forwarded_request_id`，把多跳代理的入站
    ///     `X-CC-Request-Id` 转发到下一跳，让整条调用链共用同一 ID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 echo server 捕获 observed `X-CC-Request-Id`；用转发 ID 调用 list_worktrees，
    ///     断言对端观测到的就是该固定 ID；再用 new()（不转发）调用，断言对端观测到 36 字符 UUID。
    #[tokio::test]
    async fn with_forwarded_request_id_propagates_inbound_id_for_workbench() {
        use std::sync::{Arc, Mutex};
        let observed: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let observed_clone = observed.clone();
        let app = Router::new().route(
            "/api/workbench/worktrees/list",
            post(
                move |headers: axum::http::HeaderMap, _req: Json<RemoteProjectReq>| {
                    let observed = observed_clone.clone();
                    async move {
                        let id = headers
                            .get("x-cc-request-id")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        *observed.lock().unwrap() = id;
                        Json(Vec::<WorkbenchWorktreeDto>::new())
                    }
                },
            ),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        RemoteWorkbenchClient::new()
            .with_forwarded_request_id("wb-trace-001")
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect("转发场景应成功");
        assert_eq!(
            observed.lock().unwrap().as_str(),
            "wb-trace-001",
            "转发 ID 必须原样到达下一跳"
        );

        RemoteWorkbenchClient::new()
            .list_worktrees(&format!("http://{addr}"), "project-1")
            .await
            .expect("非转发场景应成功");
        assert_eq!(
            observed.lock().unwrap().len(),
            36,
            "未设置转发 ID 时应生成 36 字符 UUID"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     生产路径 RemoteWorkbenchClient mutation 若漏 bind expected_device_id，端口复用会
    ///     fail-open 打到错误设备；必须用源码盘点锁死允许清单。
    ///
    /// Code Logic（这个测试做什么）:
    ///     扫描 src/ 下非测试模块的 `RemoteWorkbenchClient::new()`，要求同窗口 5 行内出现
    ///     `with_expected_device_id`；仅允许本文件测试段与明确 allowlist 路径。
    #[test]
    fn production_remote_workbench_client_must_bind_expected_device_id() {
        use std::path::PathBuf;
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut unbound = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<(PathBuf, usize)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let lines: Vec<&str> = text.lines().collect();
                let mut in_cfg_test = false;
                let mut brace_depth_at_test: Option<i32> = None;
                let mut depth = 0i32;
                for (i, line) in lines.iter().enumerate() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("#[cfg(test)]") {
                        in_cfg_test = true;
                        brace_depth_at_test = None;
                    }
                    // 跟踪 cfg(test) 模块的花括号深度，退出后恢复 production 模式
                    if in_cfg_test {
                        for ch in line.chars() {
                            if ch == '{' {
                                depth += 1;
                                if brace_depth_at_test.is_none() {
                                    brace_depth_at_test = Some(depth);
                                }
                            } else if ch == '}' {
                                depth -= 1;
                                if brace_depth_at_test == Some(depth + 1)
                                    || (brace_depth_at_test.is_some() && depth < 0)
                                {
                                    // 粗粒度：遇到空行后的下一个非 test item 时由后续逻辑处理
                                }
                            }
                        }
                    }
                    if line.contains("RemoteWorkbenchClient::new()") {
                        // 文件名 / 路径 allowlist：单元测试文件
                        let path_str = path.to_string_lossy();
                        if path_str.contains("/tests/")
                            || path_str.ends_with("tests.rs")
                            || path_str.ends_with("remote_client.rs")
                                && text[..text.find(line).unwrap_or(0).min(text.len())]
                                    .contains("#[cfg(test)]")
                        {
                            // remote_client 自身 tests 允许无 bind
                            if path_str.ends_with("remote_client.rs") {
                                // 仅当位于 cfg(test) 之后
                                let before: String = lines[..i].join("\n");
                                if before.contains("#[cfg(test)]") {
                                    continue;
                                }
                            } else {
                                continue;
                            }
                        }
                        let window = lines[i..lines.len().min(i + 5)].join("\n");
                        if !window.contains("with_expected_device_id") {
                            out.push((path.clone(), i + 1));
                        }
                    }
                }
            }
        }
        walk(&manifest, &mut unbound);
        assert!(
            unbound.is_empty(),
            "production RemoteWorkbenchClient::new() 必须链式 with_expected_device_id，未绑定: {:?}",
            unbound
        );
    }
}
