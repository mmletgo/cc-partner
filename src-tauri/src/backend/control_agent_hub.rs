//! backend/control_agent_hub.rs — loopback control 面的 Multi-CLI Agent Hub 操作分发。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 不得自建第二份 Agent Hub 写路径；status/assets/mutation 全部经 control client
//!     代理到 sidecar owner，由 owner 独占 SQLite 与 projection。
//!
//! Code Logic（这个模块做什么）:
//!     POST `/api/backend/control/agent-hub`：loopback+token 鉴权 → require_owner →
//!     按 `op` 分发到 `AgentHubService`；mutation 额外校验 agentHubApiVersion；
//!     响应 ≤1 MiB；永不记录 instruction content。

use crate::agent_hub::replication::sender::{
    get_push_report_for_state, push_selection_for_state, PushAgentHubSelectionRequest,
};
use crate::agent_hub::service::{
    AgentHubService, DeleteAssetEverywhereRequest, ListAssetsRequest,
    PairInstructionVariantsRequest, ResolveConflictRequest, RestoreDetachedTargetRequest,
    SetTargetBindingRequest, SetTargetEnabledRequest, SetTargetPresenceRequest,
    UpdateInstructionBlockRequest, UpdateInstructionRequest,
};
use crate::backend::control::{self, BackendControlFile, AGENT_HUB_API_VERSION};
use crate::backend::control_api::CONTROL_RESPONSE_BODY_LIMIT_BYTES;
use crate::error::AppError;
use crate::net::error_response::{P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::require_loopback_peer;
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::extract::{ConnectInfo, Extension, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;

/// Agent Hub control 请求（token + op + payload）。
///
/// Business Logic（为什么需要这个结构）:
///     GUI 用统一信封携带鉴权令牌与操作名，payload 按 op 解释。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase：controlToken / op / payload；deny_unknown_fields。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlAgentHubRequest {
    pub control_token: String,
    pub op: String,
    #[serde(default)]
    pub payload: Value,
}

/// Agent Hub control 响应（带 owner 身份）。
///
/// Business Logic（为什么需要这个结构）:
///     调用方需确认当前 ownerInstanceId，并把 result 作为业务返回值。
///
/// Code Logic（这个结构做什么）:
///     序列化 camelCase：ownerInstanceId + result。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlAgentHubResponse {
    pub owner_instance_id: String,
    pub result: Value,
}

/// Agent Hub control 路由 handler。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 GUI 与 CLI 通过 loopback control 读写 Agent Hub 权威状态。
///
/// Code Logic（这个函数做什么）:
///     鉴权 → owner 分发 → 响应 ≤1 MiB。
pub async fn control_agent_hub(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    State(state): State<AppState>,
    Json(request): Json<ControlAgentHubRequest>,
) -> P2pResult<Json<ControlAgentHubResponse>> {
    authorize_control_request(peer, &context, &request.control_token)?;
    let result = dispatch_agent_hub_op(&state, &request.op, request.payload)
        .await
        .map_err(|e| P2pError::from_app_error(e, &context, "control.agent_hub"))?;
    let owner_instance_id = state.config_runtime.owner_instance_id().to_string();
    let body = ControlAgentHubResponse {
        owner_instance_id,
        result,
    };
    ensure_response_within_limit(&body, &context)?;
    Ok(Json(body))
}

/// 按 op 字符串分发到 AgentHubService。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 作为 HeadlessOwner 独占执行 list/get/mutation。
///
/// Code Logic（这个函数做什么）:
///     require_owner；mutation 校验 agent_hub_api_version；match 10 个 op。
async fn dispatch_agent_hub_op(
    state: &AppState,
    op: &str,
    payload: Value,
) -> Result<Value, AppError> {
    state.runtime_role.require_owner()?;
    if is_mutation_op(op) {
        ensure_agent_hub_write_compatible()?;
    }

    match op {
        "agent_hub.get_status" => {
            let dto = AgentHubService::get_status(state).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.list_assets" => {
            let req: ListAssetsRequest = serde_json::from_value(payload).unwrap_or_default();
            let items = AgentHubService::list_assets(state, req).await?;
            Ok(serde_json::to_value(items)?)
        }
        "agent_hub.get_asset" => {
            let asset_id = required_string(&payload, "assetId")?;
            let dto = AgentHubService::get_asset(state, &asset_id).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.update_instruction" => {
            // 永不日志 content。
            let req: UpdateInstructionRequest =
                serde_json::from_value(normalize_content_fields(payload)).map_err(|e| {
                    AppError::validation(format!("update_instruction payload: {e}"))
                })?;
            let dto = AgentHubService::update_instruction(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.update_instruction_block" => {
            let req: UpdateInstructionBlockRequest =
                serde_json::from_value(payload).map_err(|e| {
                    AppError::validation(format!("update_instruction_block payload: {e}"))
                })?;
            let dto = AgentHubService::update_instruction_block(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.pair_instruction_variants" => {
            let req: PairInstructionVariantsRequest =
                serde_json::from_value(payload).map_err(|e| {
                    AppError::validation(format!("pair_instruction_variants payload: {e}"))
                })?;
            let dto = AgentHubService::pair_instruction_variants(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.preview_project" => {
            let project_id = required_string(&payload, "projectId")?;
            let dto = AgentHubService::preview_project(state, &project_id).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.enable_project" => {
            let project_id = required_string(&payload, "projectId")?;
            let confirm = payload
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !confirm {
                return Err(AppError::validation(
                    "启用 Agent Hub 项目作用域需要 confirm=true",
                ));
            }
            let dto = AgentHubService::enable_project(state, &project_id).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.resolve_conflict" => {
            let payload = normalize_content_fields(payload);
            let req: ResolveConflictRequest = serde_json::from_value(payload)
                .map_err(|e| AppError::validation(format!("resolve_conflict payload: {e}")))?;
            let dto = AgentHubService::resolve_conflict(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.set_target_binding" => {
            let req: SetTargetBindingRequest = serde_json::from_value(payload)
                .map_err(|e| AppError::validation(format!("set_target_binding payload: {e}")))?;
            let dto = AgentHubService::set_target_binding(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.set_target_presence" => {
            let req: SetTargetPresenceRequest = serde_json::from_value(payload)
                .map_err(|e| AppError::validation(format!("set_target_presence payload: {e}")))?;
            let dto = AgentHubService::set_target_presence(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.set_target_enabled" => {
            let req: SetTargetEnabledRequest = serde_json::from_value(payload)
                .map_err(|e| AppError::validation(format!("set_target_enabled payload: {e}")))?;
            let dto = AgentHubService::set_target_enabled(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.restore_detached_target" => {
            let req: RestoreDetachedTargetRequest =
                serde_json::from_value(payload).map_err(|e| {
                    AppError::validation(format!("restore_detached_target payload: {e}"))
                })?;
            let dto = AgentHubService::restore_detached_target(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.delete_asset_everywhere" => {
            let req: DeleteAssetEverywhereRequest =
                serde_json::from_value(payload).map_err(|e| {
                    AppError::validation(format!("delete_asset_everywhere payload: {e}"))
                })?;
            let dto = AgentHubService::delete_asset_everywhere(state, req).await?;
            Ok(serde_json::to_value(dto)?)
        }
        "agent_hub.push_selection" => {
            let req: PushAgentHubSelectionRequest = serde_json::from_value(payload)
                .map_err(|e| AppError::validation(format!("push_selection payload: {e}")))?;
            // 仅源侧 push，不提供目标 pull。
            let cancel = CancellationToken::new();
            let report = push_selection_for_state(state, req, &cancel).await?;
            Ok(serde_json::to_value(report)?)
        }
        "agent_hub.get_push_report" => {
            let request_id = required_string(&payload, "requestId")?;
            let report = get_push_report_for_state(state, &request_id).await?;
            Ok(serde_json::to_value(report)?)
        }
        other => Err(AppError::validation(format!(
            "未知 agent hub control op: {other}"
        ))),
    }
}

/// 是否为写路径 op。
///
/// Business Logic（为什么需要这个函数）:
///     mutation 必须额外校验 agentHubApiVersion，只读 op 可在旧 backend 上运行。
///
/// Code Logic（这个函数做什么）:
///     匹配 6 个 mutation op 字符串。
fn is_mutation_op(op: &str) -> bool {
    matches!(
        op,
        "agent_hub.update_instruction"
            | "agent_hub.update_instruction_block"
            | "agent_hub.pair_instruction_variants"
            | "agent_hub.enable_project"
            | "agent_hub.resolve_conflict"
            | "agent_hub.set_target_binding"
            | "agent_hub.set_target_presence"
            | "agent_hub.set_target_enabled"
            | "agent_hub.restore_detached_target"
            | "agent_hub.delete_asset_everywhere"
            | "agent_hub.push_selection"
    )
}

/// 校验 control 文件 agentHubApiVersion 与本进程一致。
///
/// Business Logic（为什么需要这个函数）:
///     旧/缺失版本允许 status/preview，但 mutation 必须 upgradeRequired。
///
/// Code Logic（这个函数做什么）:
///     读 control file；version != AGENT_HUB_API_VERSION → conflict upgradeRequired。
fn ensure_agent_hub_write_compatible() -> Result<(), AppError> {
    let control = control::read_control_file()
        .map_err(|e| AppError::generic(format!("读取控制文件失败: {e}")))?;
    let version = control
        .as_ref()
        .map(|c| c.agent_hub_api_version)
        .unwrap_or(0);
    if version == AGENT_HUB_API_VERSION {
        return Ok(());
    }
    Err(AppError::conflict("upgradeRequired"))
}

/// 兼容 content / contentMarkdown 字段名。
///
/// Business Logic（为什么需要这个函数）:
///     调用方可能用 content 或 contentMarkdown。
///
/// Code Logic（这个函数做什么）:
///     若缺 contentMarkdown 且有 content，复制为 contentMarkdown。
fn normalize_content_fields(mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        if !obj.contains_key("contentMarkdown") {
            if let Some(content) = obj.get("content").cloned() {
                obj.insert("contentMarkdown".to_string(), content);
            }
        }
    }
    payload
}

/// 读取必填字符串字段。
///
/// Business Logic（为什么需要这个函数）:
///     control payload 字段缺失应返回 validation。
///
/// Code Logic（这个函数做什么）:
///     取 string 并 trim；空串视为缺失。
fn required_string(payload: &Value, key: &str) -> Result<String, AppError> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::validation(format!("缺少 {key}")))
}

/// loopback + control token 双重鉴权。
///
/// Business Logic（为什么需要这个函数）:
///     control API 不是 LAN 业务面：非本机 peer 即使持有 token 也必须 403。
///
/// Code Logic（这个函数做什么）:
///     先 require_loopback_peer，再比较 control token。
fn authorize_control_request(
    peer: SocketAddr,
    context: &P2pRequestContext,
    request_token: &str,
) -> Result<(), P2pError> {
    require_loopback_peer(peer.ip(), context)?;
    let control = control::read_control_file()
        .map_err(|_| P2pError::from_code("控制文件不可读", P2pErrorCode::Unauthorized, context))?;
    if !control_token_matches(request_token, control.as_ref()) {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            context,
        ));
    }
    Ok(())
}

/// 校验请求 token 是否匹配控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     与 stop/workbench route 一致：空 token 或缺失控制文件一律失败。
///
/// Code Logic（这个函数做什么）:
///     控制文件存在且请求 token 非空并与 control_token 完全一致。
fn control_token_matches(request_token: &str, control: Option<&BackendControlFile>) -> bool {
    let Some(control) = control else {
        return false;
    };
    !request_token.is_empty() && request_token == control.control_token
}

/// 序列化后检查响应不超过 1 MiB。
///
/// Business Logic（为什么需要这个函数）:
///     metadata control 响应有独立 1 MiB 上限。
///
/// Code Logic（这个函数做什么）:
///     serde_json 序列化后比长度；超限返回 413。
fn ensure_response_within_limit<T: Serialize>(
    value: &T,
    context: &P2pRequestContext,
) -> Result<(), P2pError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| P2pError::from_code("控制响应序列化失败", P2pErrorCode::Internal, context))?;
    if encoded.len() > CONTROL_RESPONSE_BODY_LIMIT_BYTES {
        return Err(P2pError::from_code(
            "控制响应超过 1 MiB 限制",
            P2pErrorCode::PayloadTooLarge,
            context,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic: 未知字段必须 fail-closed。
    /// Code Logic: 带 extra 字段的 JSON 反序列化失败。
    #[test]
    fn deny_unknown_fields_rejects_extra_field() {
        let raw = r#"{
            "controlToken": "tok",
            "op": "agent_hub.get_status",
            "payload": {},
            "extra": 1
        }"#;
        let err = serde_json::from_str::<ControlAgentHubRequest>(raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field") || msg.contains("extra"),
            "unexpected error: {msg}"
        );
    }

    /// Business Logic: Gate A + presence ops 是前端/control client 的稳定合同。
    /// Code Logic: 源文件包含全部 op 字符串。
    #[test]
    fn source_contains_all_ten_op_strings() {
        let src = include_str!("control_agent_hub.rs");
        for op in [
            "agent_hub.get_status",
            "agent_hub.list_assets",
            "agent_hub.get_asset",
            "agent_hub.update_instruction",
            "agent_hub.update_instruction_block",
            "agent_hub.pair_instruction_variants",
            "agent_hub.preview_project",
            "agent_hub.enable_project",
            "agent_hub.resolve_conflict",
            "agent_hub.set_target_binding",
            "agent_hub.set_target_presence",
            "agent_hub.set_target_enabled",
            "agent_hub.restore_detached_target",
            "agent_hub.delete_asset_everywhere",
            "agent_hub.push_selection",
            "agent_hub.get_push_report",
        ] {
            assert!(src.contains(op), "missing op {op}");
        }
        let forbidden_op = format!("{}{}", "agent_hub.", "pull");
        assert!(!src.contains(&forbidden_op));
    }

    /// Business Logic: mutation 必须在版本不匹配时 upgradeRequired。
    /// Code Logic: is_mutation_op 覆盖写路径。
    #[test]
    fn mutation_ops_cover_write_paths() {
        assert!(is_mutation_op("agent_hub.update_instruction"));
        assert!(is_mutation_op("agent_hub.enable_project"));
        assert!(is_mutation_op("agent_hub.set_target_presence"));
        assert!(is_mutation_op("agent_hub.set_target_enabled"));
        assert!(is_mutation_op("agent_hub.restore_detached_target"));
        assert!(is_mutation_op("agent_hub.delete_asset_everywhere"));
        assert!(is_mutation_op("agent_hub.push_selection"));
        assert!(!is_mutation_op("agent_hub.get_status"));
        assert!(!is_mutation_op("agent_hub.preview_project"));
        assert!(!is_mutation_op("agent_hub.get_push_report"));
    }
}
