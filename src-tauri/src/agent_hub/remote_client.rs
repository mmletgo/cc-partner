//! agent_hub/remote_client.rs — 用户级三栏指令的 owning-device P2P 客户端。
//!
//! Business Logic（为什么需要这个模块）:
//!     控制端选中对端设备后，inspect/save/CAS/写入原始文件/analyze·adapt·revise/槽历史
//!     必须在 owning device 的用户目录执行，禁止静默回落本机 `~/.claude`。
//!
//! Code Logic（这个模块做什么）:
//!     `RemoteAgentHubClient` 对 `agent-hub.user-instructions.v1` 路由发 POST；
//!     缺 capability → `capability_unsupported`；`remote:` deviceId 视为递归并拒绝。

use crate::agent_hub::service::AgentHubService;
use crate::agent_hub::user_instructions::{
    AdaptInstructionToOtherAgentsRequest, AdaptInstructionToOtherAgentsResult,
    AnalyzeInstructionOriginalRequest, AnalyzeInstructionOriginalResult,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    ListUserInstructionSlotVersionsRequest, PreviewUserInstructionRequest,
    RestoreUserInstructionSlotRequest, ReviseInstructionSlotRequest, ReviseInstructionSlotResult,
    SaveUserInstructionBlocksRequest, UserInstructionCanonicalDto, UserInstructionPlanDto,
    UserInstructionWorkspaceDto,
};
use crate::commands::prompts::{content_version_to_dto, ContentVersionDto};
use crate::error::AppError;
use crate::models::device::Device;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_client::PeerClient;
use crate::net::peer_error::{peer_call_error_to_app_error, PeerCallError};
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::CAPABILITY_USER_INSTRUCTIONS_V1;
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

/// analyze/adapt/revise 在对端跑 HeadlessCompletion，预算对齐本机 180s + 余量。
const USER_INSTRUCTION_LLM_PEER_TIMEOUT: Duration = Duration::from_secs(200);

/// 用户级三栏 P2P 路径（与 `docs/p2p-protocol.md` 脚手架行一致）。
pub const USER_INSTRUCTIONS_INSPECT_PATH: &str = "/api/agent-hub/user-instructions/inspect";
pub const USER_INSTRUCTIONS_SAVE_BLOCKS_PATH: &str = "/api/agent-hub/user-instructions/save-blocks";
pub const USER_INSTRUCTIONS_PREVIEW_SETUP_PATH: &str =
    "/api/agent-hub/user-instructions/preview-setup";
pub const USER_INSTRUCTIONS_PREVIEW_UPDATE_PATH: &str =
    "/api/agent-hub/user-instructions/preview-update";
pub const USER_INSTRUCTIONS_APPLY_PLAN_PATH: &str = "/api/agent-hub/user-instructions/apply-plan";
pub const USER_INSTRUCTIONS_ANALYZE_PATH: &str = "/api/agent-hub/user-instructions/analyze";
pub const USER_INSTRUCTIONS_ADAPT_PATH: &str = "/api/agent-hub/user-instructions/adapt";
pub const USER_INSTRUCTIONS_REVISE_PATH: &str = "/api/agent-hub/user-instructions/revise";
pub const USER_INSTRUCTIONS_SLOT_VERSIONS_PATH: &str =
    "/api/agent-hub/user-instructions/slot-versions";
pub const USER_INSTRUCTIONS_RESTORE_SLOT_PATH: &str =
    "/api/agent-hub/user-instructions/restore-slot-version";

/// 用户级三栏远端客户端。
///
/// Business Logic: 控制端 sidecar 在 deviceId 非空时只发网络请求，读写落在 owning peer。
/// Code Logic: PeerClient + expected-device header；先 require_capability 再业务 POST。
#[derive(Debug)]
pub struct RemoteAgentHubClient {
    peer: PeerClient,
    expected_device_id: String,
}

impl RemoteAgentHubClient {
    /// 绑定期望 device_id 并构造客户端。
    ///
    /// Business Logic: 端口复用时必须让业务请求自己携带期望 device_id。
    /// Code Logic: 空 id 拒绝，避免误打本机。
    pub fn connect(expected_device_id: &str) -> Result<Self, AppError> {
        let expected_device_id = expected_device_id.trim();
        if expected_device_id.is_empty() {
            return Err(AppError::validation(
                "local_user_scope_required".to_string(),
            ));
        }
        if expected_device_id.starts_with("remote:") {
            return Err(AppError::validation(
                "local_user_scope_required".to_string(),
            ));
        }
        Ok(Self {
            peer: PeerClient::new(),
            expected_device_id: expected_device_id.to_string(),
        })
    }

    /// 解析设备并确认 `agent-hub.user-instructions.v1`。
    ///
    /// Business Logic: 缺 capability 必须 fail-closed，不得回落本机 home。
    /// Code Logic: devices 表取 base_url → require_capability。
    pub async fn open(state: &AppState, device_id: &str) -> Result<(Self, String), AppError> {
        let client = Self::connect(device_id)?;
        let base_url = resolve_device_base_url(state, &client.expected_device_id)?;
        client
            .peer
            .require_capability(&base_url, CAPABILITY_USER_INSTRUCTIONS_V1)
            .await
            .map_err(user_instruction_peer_err)?;
        Ok((client, base_url))
    }

    /// POST 对端 inspect（空 body）。
    pub async fn inspect(&self, base_url: &str) -> Result<UserInstructionWorkspaceDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_INSPECT_PATH,
            &serde_json::json!({}),
            PeerTimeoutClass::Metadata,
        )
        .await
    }

    /// POST 对端 CAS 保存块文档。
    pub async fn save_blocks(
        &self,
        base_url: &str,
        req: &SaveUserInstructionBlocksRequest,
    ) -> Result<UserInstructionCanonicalDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_SAVE_BLOCKS_PATH,
            req,
            PeerTimeoutClass::Mutation,
        )
        .await
    }

    /// POST 对端首次设置 preview。
    pub async fn preview_setup(
        &self,
        base_url: &str,
        req: &PreviewUserInstructionRequest,
    ) -> Result<UserInstructionPlanDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_PREVIEW_SETUP_PATH,
            req,
            PeerTimeoutClass::Metadata,
        )
        .await
    }

    /// POST 对端日常更新 preview。
    pub async fn preview_update(
        &self,
        base_url: &str,
        req: &PreviewUserInstructionRequest,
    ) -> Result<UserInstructionPlanDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_PREVIEW_UPDATE_PATH,
            req,
            PeerTimeoutClass::Metadata,
        )
        .await
    }

    /// POST 对端 apply plan（写原生文件）。
    pub async fn apply_plan(
        &self,
        base_url: &str,
        req: &ApplyUserInstructionPlanRequest,
    ) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_APPLY_PLAN_PATH,
            req,
            PeerTimeoutClass::Mutation,
        )
        .await
    }

    /// POST 对端 analyze（HeadlessCompletion）。
    pub async fn analyze(
        &self,
        base_url: &str,
        req: &AnalyzeInstructionOriginalRequest,
    ) -> Result<AnalyzeInstructionOriginalResult, AppError> {
        self.post_json_long(
            base_url,
            USER_INSTRUCTIONS_ANALYZE_PATH,
            req,
            USER_INSTRUCTION_LLM_PEER_TIMEOUT,
        )
        .await
    }

    /// POST 对端 adapt（HeadlessCompletion）。
    pub async fn adapt(
        &self,
        base_url: &str,
        req: &AdaptInstructionToOtherAgentsRequest,
    ) -> Result<AdaptInstructionToOtherAgentsResult, AppError> {
        self.post_json_long(
            base_url,
            USER_INSTRUCTIONS_ADAPT_PATH,
            req,
            USER_INSTRUCTION_LLM_PEER_TIMEOUT,
        )
        .await
    }

    /// POST 对端 revise（HeadlessCompletion）。
    pub async fn revise(
        &self,
        base_url: &str,
        req: &ReviseInstructionSlotRequest,
    ) -> Result<ReviseInstructionSlotResult, AppError> {
        self.post_json_long(
            base_url,
            USER_INSTRUCTIONS_REVISE_PATH,
            req,
            USER_INSTRUCTION_LLM_PEER_TIMEOUT,
        )
        .await
    }

    /// POST 对端槽历史列表。
    pub async fn list_slot_versions(
        &self,
        base_url: &str,
        req: &ListUserInstructionSlotVersionsRequest,
    ) -> Result<Vec<ContentVersionDto>, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_SLOT_VERSIONS_PATH,
            req,
            PeerTimeoutClass::Metadata,
        )
        .await
    }

    /// POST 对端 CAS 恢复槽历史。
    pub async fn restore_slot_version(
        &self,
        base_url: &str,
        req: &RestoreUserInstructionSlotRequest,
    ) -> Result<UserInstructionCanonicalDto, AppError> {
        self.post_json(
            base_url,
            USER_INSTRUCTIONS_RESTORE_SLOT_PATH,
            req,
            PeerTimeoutClass::Mutation,
        )
        .await
    }

    async fn post_json<T, B>(
        &self,
        base_url: &str,
        path: &str,
        body: &B,
        class: PeerTimeoutClass,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        post_user_instruction_json(
            &self.peer,
            base_url,
            path,
            body,
            &self.expected_device_id,
            class.timeout(),
        )
        .await
    }

    async fn post_json_long<T, B>(
        &self,
        base_url: &str,
        path: &str,
        body: &B,
        timeout: Duration,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        post_user_instruction_json(
            &self.peer,
            base_url,
            path,
            body,
            &self.expected_device_id,
            timeout,
        )
        .await
    }
}

/// 从 control/invoke payload 取出可选 deviceId（会从 Value 中移除）。
///
/// Business Logic: Preview 等请求 `deny_unknown_fields`，deviceId 不能留在领域 body。
/// Code Logic: 删除 `deviceId` 后 trim；空串视为本机。
pub fn take_device_id(payload: &mut Value) -> Option<String> {
    let Some(obj) = payload.as_object_mut() else {
        return None;
    };
    let raw = obj.remove("deviceId")?;
    let id = raw.as_str().unwrap_or("").trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// 判定是否应对 peer 代理（空/本机 → None）。
///
/// Business Logic: 选中本机设备 id 仍走本机 home，避免无意义自调用。
/// Code Logic: trim 后与 `state.device_id` 比较。
pub fn remote_user_instruction_device_id(
    state: &AppState,
    device_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    let Some(id) = device_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if id.starts_with("remote:") {
        return Err(AppError::validation(
            "local_user_scope_required".to_string(),
        ));
    }
    if id == state.device_id.as_str() {
        return Ok(None);
    }
    Ok(Some(id.to_string()))
}

/// inspect：deviceId 非空则 P2P，否则本机。
pub async fn inspect_user_instruction_workspace_for_state(
    state: &AppState,
    device_id: Option<&str>,
) -> Result<UserInstructionWorkspaceDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.inspect(&base_url).await;
    }
    AgentHubService::inspect_user_instruction_workspace(state).await
}

/// save-blocks：deviceId 非空则 P2P，否则本机 CAS。
pub async fn save_user_instruction_blocks_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: SaveUserInstructionBlocksRequest,
) -> Result<UserInstructionCanonicalDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.save_blocks(&base_url, &req).await;
    }
    AgentHubService::save_user_instruction_blocks(state, req).await
}

/// preview-setup：deviceId 非空则 P2P。
pub async fn preview_user_instruction_setup_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.preview_setup(&base_url, &req).await;
    }
    AgentHubService::preview_user_instruction_setup(state, req).await
}

/// preview-update：deviceId 非空则 P2P。
pub async fn preview_user_instruction_update_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.preview_update(&base_url, &req).await;
    }
    AgentHubService::preview_user_instruction_update(state, req).await
}

/// apply-plan：deviceId 非空则 P2P。
pub async fn apply_user_instruction_plan_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: ApplyUserInstructionPlanRequest,
) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.apply_plan(&base_url, &req).await;
    }
    AgentHubService::apply_user_instruction_plan(state, req).await
}

/// slot-versions：deviceId 非空则 P2P。
pub async fn list_user_instruction_slot_versions_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: ListUserInstructionSlotVersionsRequest,
) -> Result<Vec<ContentVersionDto>, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.list_slot_versions(&base_url, &req).await;
    }
    let versions =
        AgentHubService::list_user_instruction_slot_versions(state, req.asset_id, req.slot).await?;
    Ok(versions.iter().map(content_version_to_dto).collect())
}

/// restore-slot-version：deviceId 非空则 P2P。
pub async fn restore_user_instruction_slot_version_for_state(
    state: &AppState,
    device_id: Option<&str>,
    req: RestoreUserInstructionSlotRequest,
) -> Result<UserInstructionCanonicalDto, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.restore_slot_version(&base_url, &req).await;
    }
    AgentHubService::restore_user_instruction_slot_version(state, req).await
}

/// analyze：deviceId 非空则 P2P，否则本机 HeadlessCompletion。
pub async fn analyze_instruction_original_for_device(
    state: &AppState,
    device_id: Option<&str>,
    req: AnalyzeInstructionOriginalRequest,
    local: impl std::future::Future<Output = Result<AnalyzeInstructionOriginalResult, AppError>>,
) -> Result<AnalyzeInstructionOriginalResult, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.analyze(&base_url, &req).await;
    }
    local.await
}

/// adapt：deviceId 非空则 P2P。
pub async fn adapt_instruction_to_other_agents_for_device(
    state: &AppState,
    device_id: Option<&str>,
    req: AdaptInstructionToOtherAgentsRequest,
    local: impl std::future::Future<Output = Result<AdaptInstructionToOtherAgentsResult, AppError>>,
) -> Result<AdaptInstructionToOtherAgentsResult, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.adapt(&base_url, &req).await;
    }
    local.await
}

/// revise：deviceId 非空则 P2P。
pub async fn revise_instruction_slot_for_device(
    state: &AppState,
    device_id: Option<&str>,
    req: ReviseInstructionSlotRequest,
    local: impl std::future::Future<Output = Result<ReviseInstructionSlotResult, AppError>>,
) -> Result<ReviseInstructionSlotResult, AppError> {
    if let Some(peer_id) = remote_user_instruction_device_id(state, device_id)? {
        let (client, base_url) = RemoteAgentHubClient::open(state, &peer_id).await?;
        return client.revise(&base_url, &req).await;
    }
    local.await
}

fn resolve_device_base_url(state: &AppState, device_id: &str) -> Result<String, AppError> {
    let devices = state.devices.read().expect("devices lock");
    devices
        .get(device_id)
        .cloned()
        .ok_or_else(|| AppError::not_found("设备不存在或已离线".to_string()))
        .map(|device: Device| device.base_url())
}

fn user_instruction_peer_err(error: PeerCallError) -> AppError {
    match error {
        PeerCallError::Unsupported { capability, .. } => {
            AppError::unavailable(format!("capability_unsupported:{capability}"))
        }
        other => peer_call_error_to_app_error(other, "远端 Agent Hub"),
    }
}

async fn post_user_instruction_json<T, B>(
    peer: &PeerClient,
    base_url: &str,
    path: &str,
    body: &B,
    expected_device_id: &str,
    timeout: Duration,
) -> Result<T, AppError>
where
    T: DeserializeOwned,
    B: Serialize + ?Sized,
{
    let url = format!("{base_url}{path}");
    let resp = peer
        .http_client()
        .post(&url)
        .timeout(timeout)
        .header(REQUEST_ID_HEADER, new_request_id())
        .header(EXPECTED_DEVICE_ID_HEADER.as_str(), expected_device_id)
        .json(body)
        .send()
        .await
        .map_err(|e| PeerCallError::Network {
            url: url.clone(),
            source: e,
        })
        .map_err(user_instruction_peer_err)?;
    crate::net::peer_error::parse_peer_response::<T>(resp, &url)
        .await
        .map_err(user_instruction_peer_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: 脚手架路径必须与协议表 10 行一致。
    #[test]
    fn user_instruction_paths_match_scaffold_rows() {
        assert_eq!(
            USER_INSTRUCTIONS_INSPECT_PATH,
            "/api/agent-hub/user-instructions/inspect"
        );
        assert_eq!(
            USER_INSTRUCTIONS_SAVE_BLOCKS_PATH,
            "/api/agent-hub/user-instructions/save-blocks"
        );
        assert_eq!(
            USER_INSTRUCTIONS_PREVIEW_SETUP_PATH,
            "/api/agent-hub/user-instructions/preview-setup"
        );
        assert_eq!(
            USER_INSTRUCTIONS_PREVIEW_UPDATE_PATH,
            "/api/agent-hub/user-instructions/preview-update"
        );
        assert_eq!(
            USER_INSTRUCTIONS_APPLY_PLAN_PATH,
            "/api/agent-hub/user-instructions/apply-plan"
        );
        assert_eq!(
            USER_INSTRUCTIONS_ANALYZE_PATH,
            "/api/agent-hub/user-instructions/analyze"
        );
        assert_eq!(
            USER_INSTRUCTIONS_ADAPT_PATH,
            "/api/agent-hub/user-instructions/adapt"
        );
        assert_eq!(
            USER_INSTRUCTIONS_REVISE_PATH,
            "/api/agent-hub/user-instructions/revise"
        );
        assert_eq!(
            USER_INSTRUCTIONS_SLOT_VERSIONS_PATH,
            "/api/agent-hub/user-instructions/slot-versions"
        );
        assert_eq!(
            USER_INSTRUCTIONS_RESTORE_SLOT_PATH,
            "/api/agent-hub/user-instructions/restore-slot-version"
        );
        assert_eq!(
            CAPABILITY_USER_INSTRUCTIONS_V1,
            "agent-hub.user-instructions.v1"
        );
    }

    /// Business Logic: `remote:` 是项目 shortcut，用户级路由不得把它当 deviceId 再代理。
    #[test]
    fn connect_rejects_remote_project_prefix_as_recursion() {
        let Err(err) = RemoteAgentHubClient::connect("remote:dev:inner") else {
            panic!("remote: deviceId 必须拒绝");
        };
        assert_eq!(err.code(), "local_user_scope_required");
        let Err(err) = RemoteAgentHubClient::connect("") else {
            panic!("空 deviceId 必须拒绝");
        };
        assert_eq!(err.code(), "local_user_scope_required");
    }

    /// Business Logic: control payload 的 deviceId 不得进入 deny_unknown_fields 领域请求。
    #[test]
    fn take_device_id_strips_and_trims() {
        let mut payload = serde_json::json!({
            "deviceId": "  peer-1  ",
            "planToken": "p1",
            "clientRequestId": "c1"
        });
        assert_eq!(take_device_id(&mut payload).as_deref(), Some("peer-1"));
        assert!(payload.get("deviceId").is_none());
        assert_eq!(payload["planToken"], "p1");

        let mut empty = serde_json::json!({ "deviceId": "   " });
        assert_eq!(take_device_id(&mut empty), None);
    }

    /// Business Logic: 缺能力错误必须带稳定 capability_unsupported 前缀。
    #[test]
    fn unsupported_maps_to_capability_unavailable() {
        let err = user_instruction_peer_err(PeerCallError::Unsupported {
            url: "http://127.0.0.1:1".into(),
            capability: CAPABILITY_USER_INSTRUCTIONS_V1,
        });
        assert_eq!(err.code(), "unavailable");
        assert!(err
            .to_string()
            .contains("capability_unsupported:agent-hub.user-instructions.v1"));
    }
}
