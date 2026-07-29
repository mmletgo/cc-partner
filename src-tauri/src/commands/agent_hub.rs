//! commands/agent_hub.rs — Multi-CLI Agent Hub thin Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端通过 IPC 读写 Agent Hub status/assets/instruction/project/conflict；
//!     GuiClient 必须代理到 sidecar owner，禁止第二写路径。
//!
//! Code Logic（这个模块做什么）:
//!     HeadlessOwner → AgentHubService；GuiClient → BackendControlClient agent_hub_*；
//!     mutation 前 client 调用 require_agent_hub_write_compatibility。

use crate::agent_hub::project_scope::{AgentHubProjectPreview, AgentHubProjectStatus};
use crate::agent_hub::replication::sender::{
    get_push_report_for_state, push_selection_for_state, MultiTargetPushReport,
    PushAgentHubSelectionRequest,
};
use crate::agent_hub::service::{
    AgentHubAssetDetailDto, AgentHubAssetSummaryDto, AgentHubService, AgentHubStatusDto,
    DeleteAssetEverywhereRequest, InstructionBlockDto, ListAssetsRequest,
    PairInstructionVariantsRequest, ResolveConflictRequest, RestoreDetachedTargetRequest,
    SetTargetBindingRequest, SetTargetEnabledRequest, SetTargetPresenceRequest,
    UpdateInstructionBlockRequest, UpdateInstructionRequest,
};
use crate::backend::authority::RuntimeRole;
use crate::backend::control::AGENT_HUB_API_VERSION;
use crate::backend::control_client::BackendControlClient;
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;
use tauri::State;
use tokio_util::sync::CancellationToken;

/// Business Logic: 首屏 status。
/// Code Logic: owner service / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_get_status(
    state: State<'_, AppState>,
) -> Result<AgentHubStatusDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_status()
            .await;
    }
    AgentHubService::get_status(state.inner()).await
}

/// Business Logic: 资产列表。
/// Code Logic: owner list_assets / GuiClient query。
#[tauri::command]
pub async fn agent_hub_list_assets(
    state: State<'_, AppState>,
    scope_id: Option<String>,
    kind: Option<String>,
) -> Result<Vec<AgentHubAssetSummaryDto>, AppError> {
    let req = ListAssetsRequest { scope_id, kind };
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_list_assets(req)
            .await;
    }
    AgentHubService::list_assets(state.inner(), req).await
}

/// Business Logic: 资产详情。
/// Code Logic: owner get_asset / GuiClient query。
#[tauri::command]
pub async fn agent_hub_get_asset(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<AgentHubAssetDetailDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_asset(&asset_id)
            .await;
    }
    AgentHubService::get_asset(state.inner(), &asset_id).await
}

/// Business Logic: 保存整份指令。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_update_instruction(
    state: State<'_, AppState>,
    asset_id: String,
    content_markdown: String,
    expected_revision_id: Option<String>,
) -> Result<AgentHubAssetDetailDto, AppError> {
    let req = UpdateInstructionRequest {
        asset_id,
        content_markdown,
        expected_revision_id,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_update_instruction(req).await;
    }
    AgentHubService::update_instruction(state.inner(), req).await
}

/// Business Logic: 更新指令块。
/// Code Logic: mutation + write compatibility；扁平参数对齐前端。
#[tauri::command]
pub async fn agent_hub_update_instruction_block(
    state: State<'_, AppState>,
    asset_id: String,
    block_id: String,
    mode: Option<String>,
    common_markdown: Option<String>,
    variants: Option<BTreeMap<String, String>>,
    expected_revision_id: Option<String>,
) -> Result<InstructionBlockDto, AppError> {
    let req = UpdateInstructionBlockRequest {
        asset_id,
        block_id,
        mode,
        common_markdown,
        variants,
        expected_revision_id,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_update_instruction_block(req).await;
    }
    AgentHubService::update_instruction_block(state.inner(), req).await
}

/// Business Logic: 配对 adapted 变体。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_pair_instruction_variants(
    state: State<'_, AppState>,
    asset_id: String,
    block_ids: Vec<String>,
    common_markdown: Option<String>,
    expected_revision_id: Option<String>,
) -> Result<AgentHubAssetDetailDto, AppError> {
    let req = PairInstructionVariantsRequest {
        asset_id,
        block_ids,
        common_markdown,
        expected_revision_id,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_pair_instruction_variants(req).await;
    }
    AgentHubService::pair_instruction_variants(state.inner(), req).await
}

/// Business Logic: 项目启用预览（零写入）。
/// Code Logic: owner preview / GuiClient query。
#[tauri::command]
pub async fn agent_hub_preview_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<AgentHubProjectPreview, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_preview_project(&project_id)
            .await;
    }
    AgentHubService::preview_project(state.inner(), &project_id).await
}

/// Business Logic: 启用项目。
/// Code Logic: mutation + write compatibility；IPC 默认 confirm=true。
#[tauri::command]
pub async fn agent_hub_enable_project(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<AgentHubProjectStatus, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_enable_project(&project_id, true).await;
    }
    AgentHubService::enable_project(state.inner(), &project_id).await
}

/// Business Logic: 解决冲突。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_resolve_conflict(
    state: State<'_, AppState>,
    asset_id: String,
    conflict_id: String,
    resolution: String,
    content_markdown: Option<String>,
) -> Result<AgentHubAssetDetailDto, AppError> {
    let req = ResolveConflictRequest {
        asset_id,
        conflict_id,
        resolution,
        content_markdown,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_resolve_conflict(req).await;
    }
    AgentHubService::resolve_conflict(state.inner(), req).await
}

/// Business Logic: 设置 target binding。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_set_target_binding(
    state: State<'_, AppState>,
    asset_id: String,
    target: String,
    desired_presence: String,
    desired_enabled: bool,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    use crate::agent_hub::models::{AgentTarget, DesiredPresence};
    let target = AgentTarget::parse(target.trim())
        .ok_or_else(|| AppError::validation(format!("未知 target: {target}")))?;
    let desired_presence = DesiredPresence::parse(desired_presence.trim())
        .ok_or_else(|| AppError::validation(format!("未知 desiredPresence: {desired_presence}")))?;
    let req = SetTargetBindingRequest {
        asset_id,
        target,
        desired_presence,
        desired_enabled,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_set_target_binding(req).await;
    }
    AgentHubService::set_target_binding(state.inner(), req).await
}

/// Business Logic: 设置 target-local desiredPresence。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_set_target_presence(
    state: State<'_, AppState>,
    asset_id: String,
    target: String,
    desired_presence: String,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    use crate::agent_hub::models::{AgentTarget, DesiredPresence};
    let target = AgentTarget::parse(target.trim())
        .ok_or_else(|| AppError::validation(format!("未知 target: {target}")))?;
    let desired_presence = DesiredPresence::parse(desired_presence.trim())
        .ok_or_else(|| AppError::validation(format!("未知 desiredPresence: {desired_presence}")))?;
    let req = SetTargetPresenceRequest {
        asset_id,
        target,
        desired_presence,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_set_target_presence(req).await;
    }
    AgentHubService::set_target_presence(state.inner(), req).await
}

/// Business Logic: 设置 target-local desiredEnabled。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_set_target_enabled(
    state: State<'_, AppState>,
    asset_id: String,
    target: String,
    desired_enabled: bool,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    use crate::agent_hub::models::AgentTarget;
    let target = AgentTarget::parse(target.trim())
        .ok_or_else(|| AppError::validation(format!("未知 target: {target}")))?;
    let req = SetTargetEnabledRequest {
        asset_id,
        target,
        desired_enabled,
    };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_set_target_enabled(req).await;
    }
    AgentHubService::set_target_enabled(state.inner(), req).await
}

/// Business Logic: 恢复 detached target 并调度投影。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_restore_detached_target(
    state: State<'_, AppState>,
    asset_id: String,
    target: String,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    use crate::agent_hub::models::AgentTarget;
    let target = AgentTarget::parse(target.trim())
        .ok_or_else(|| AppError::validation(format!("未知 target: {target}")))?;
    let req = RestoreDetachedTargetRequest { asset_id, target };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_restore_detached_target(req).await;
    }
    AgentHubService::restore_detached_target(state.inner(), req).await
}

/// Business Logic: 从所有 target 删除（canonical tombstone + fan-out）。
/// Code Logic: mutation + write compatibility。
#[tauri::command]
pub async fn agent_hub_delete_asset_everywhere(
    state: State<'_, AppState>,
    asset_id: String,
) -> Result<AgentHubAssetSummaryDto, AppError> {
    let req = DeleteAssetEverywhereRequest { asset_id };
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_delete_asset_everywhere(req).await;
    }
    AgentHubService::delete_asset_everywhere(state.inner(), req).await
}

/// Business Logic: 源侧 multi-target LAN push（仅 push，无目标 pull）。
/// Code Logic: mutation + write compatibility；GuiClient 经 control 代理。
#[tauri::command]
pub async fn agent_hub_push_selection(
    state: State<'_, AppState>,
    request: PushAgentHubSelectionRequest,
) -> Result<MultiTargetPushReport, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_push_selection(request).await;
    }
    let cancel = CancellationToken::new();
    push_selection_for_state(state.inner(), request, &cancel).await
}

/// Business Logic: GUI reconnect 读取源侧 push 进度。
/// Code Logic: 只读 query；无 pull 语义。
#[tauri::command]
pub async fn agent_hub_get_push_report(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<MultiTargetPushReport>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_push_report(&request_id)
            .await;
    }
    get_push_report_for_state(state.inner(), &request_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::control_client::BackendControlClient;

    /// Business Logic: Gate A + Gate B presence 命令是前端 AGENT_HUB_COMMANDS 合同。
    /// Code Logic: 源文件含全部 snake_case 命令与 GuiClient 代理符号。
    #[test]
    fn source_contains_all_ten_commands_and_gui_proxy_symbols() {
        let src = include_str!("agent_hub.rs");
        for name in [
            "agent_hub_get_status",
            "agent_hub_list_assets",
            "agent_hub_get_asset",
            "agent_hub_update_instruction",
            "agent_hub_update_instruction_block",
            "agent_hub_pair_instruction_variants",
            "agent_hub_preview_project",
            "agent_hub_enable_project",
            "agent_hub_resolve_conflict",
            "agent_hub_set_target_binding",
            "agent_hub_set_target_presence",
            "agent_hub_set_target_enabled",
            "agent_hub_restore_detached_target",
            "agent_hub_delete_asset_everywhere",
            "agent_hub_push_selection",
            "agent_hub_get_push_report",
        ] {
            assert!(src.contains(name), "missing command {name}");
        }
        assert!(src.contains("RuntimeRole::GuiClient"));
        assert!(src.contains("BackendControlClient"));
        assert!(src.contains("require_agent_hub_write_compatibility"));
        // 禁止目标侧 pull API（拼接避免自命中）
        let forbidden = format!("{}{}", "agent_hub_", "pull");
        assert!(!src.contains(&forbidden));
    }

    /// Business Logic: 旧 backend agentHubApiVersion 必须阻断 mutation。
    /// Code Logic: for_test_with_agent_hub_version(0) → upgradeRequired。
    #[test]
    fn mutation_with_incompatible_control_version_returns_upgrade_required() {
        let client =
            BackendControlClient::for_test_with_agent_hub_version(1, "tok", "owner", 0).unwrap();
        let err = client
            .require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)
            .unwrap_err();
        assert_eq!(err.code(), "upgradeRequired");
    }

    /// Business Logic: status DTO 字段必须 camelCase 对齐前端 decoder。
    /// Code Logic: 序列化后断言关键 key。
    #[test]
    fn agent_hub_status_dto_serializes_expected_camel_case_keys() {
        let dto = AgentHubStatusDto {
            enabled: false,
            background_enabled: true,
            agent_hub_api_version: 1,
            owner_instance_id: None,
            write_compatible: true,
            probes: vec![],
            conflict_count: 0,
            blocked_materialization_count: 0,
        };
        let value = serde_json::to_value(&dto).expect("serialize");
        for key in [
            "enabled",
            "backgroundEnabled",
            "agentHubApiVersion",
            "writeCompatible",
            "probes",
            "conflictCount",
            "blockedMaterializationCount",
        ] {
            assert!(value.get(key).is_some(), "missing key {key}");
        }
    }

    /// Business Logic: control op 字符串必须与 command 层 1:1。
    /// Code Logic: 读取 control_agent_hub 源，断言 Gate A + presence ops。
    #[test]
    fn control_agent_hub_source_contains_all_ops() {
        let src = include_str!("../backend/control_agent_hub.rs");
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
}
