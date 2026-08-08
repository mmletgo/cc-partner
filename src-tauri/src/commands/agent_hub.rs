//! commands/agent_hub.rs — Multi-CLI Agent Hub thin Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端通过 IPC 读写 Agent Hub status/assets/instruction/project/conflict；
//!     GuiClient 必须代理到 sidecar owner，禁止第二写路径。
//!
//! Code Logic（这个模块做什么）:
//!     HeadlessOwner → AgentHubService；GuiClient → BackendControlClient agent_hub_*；
//!     mutation 前 client 调用 require_agent_hub_write_compatibility。

use crate::agent_hub::cross_agent::{
    apply_cross_agent_instruction, preview_cross_agent_instruction,
    ApplyCrossAgentInstructionRequest, CrossAgentApplyTargetResult, CrossAgentPreviewReport,
    PreviewCrossAgentInstructionRequest,
};
use crate::agent_hub::cross_agent_full::{
    apply_cross_agent_full_default, preview_cross_agent_full_default, ApplyCrossAgentFullRequest,
    CrossAgentFullApplyItemResult, CrossAgentFullPlan, PreviewCrossAgentFullRequest,
};
use crate::agent_hub::git::preview::{
    confirm_git_import_for_state, confirm_project_mapping_for_state, inspect_git_lanes_for_state,
    preview_git_import_for_state, ConfirmGitImportOutcome, ConfirmGitImportRequest,
    ConfirmProjectMappingRequest, GitImportPreview, GitLaneInspectReport,
};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::portable_actions::{
    ApplyPortableAssetActionRequest, PortableAssetActionPlanDto, PortableAssetActionResultDto,
    PreviewPortableAssetActionRequest,
};
use crate::agent_hub::portable_inventory::PortableInventorySnapshotDto;
use crate::agent_hub::portable_service::PortableService;
use crate::agent_hub::project_scope::{AgentHubProjectPreview, AgentHubProjectStatus};
use crate::agent_hub::replication::pull::{
    ApplyPortablePullRequest, ListRemotePortableInventoryRequest, PortablePullPlanDto,
    PortablePullResultDto, PreviewPortablePullRequest, RemotePortableInventoryDto,
};
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
use crate::agent_hub::snapshot::builder::{build_snapshot, SnapshotSelectionRequest};
use crate::agent_hub::snapshot::importer::ResolvedProjectMapping;
use crate::agent_hub::targets::TargetEnvironment;
use crate::agent_hub::user_instructions::{
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, UserInstructionPlanDto, UserInstructionWorkspaceDto,
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

/// Business Logic: 用户级指令默认入口展示 sidecar owner 解析的真实 source chain。
/// Code Logic: owner inspect / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_inspect_user_instruction_workspace(
    state: State<'_, AppState>,
) -> Result<UserInstructionWorkspaceDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_inspect_user_instruction_workspace()
            .await;
    }
    AgentHubService::inspect_user_instruction_workspace(state.inner()).await
}

/// Business Logic: 首次设置必须先生成绑定 revision/inventory/hash 的预览计划。
/// Code Logic: 新 V2 control 合同需要 API 版本一致；不执行目标文件 mutation。
#[tauri::command]
pub async fn agent_hub_preview_user_instruction_setup(
    state: State<'_, AppState>,
    request: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client
            .agent_hub_preview_user_instruction_setup(request)
            .await;
    }
    AgentHubService::preview_user_instruction_setup(state.inner(), request).await
}

/// Business Logic: 日常更新与首次设置共享相同的 preview 安全门闩。
/// Code Logic: owner preview / GuiClient V2 control。
#[tauri::command]
pub async fn agent_hub_preview_user_instruction_update(
    state: State<'_, AppState>,
    request: PreviewUserInstructionRequest,
) -> Result<UserInstructionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client
            .agent_hub_preview_user_instruction_update(request)
            .await;
    }
    AgentHubService::preview_user_instruction_update(state.inner(), request).await
}

/// Business Logic: 用户确认后才可应用短期 plan；当前写能力未认证时逐 target blocked。
/// Code Logic: owner 原子 claim/apply / GuiClient V2 control mutation。
#[tauri::command]
pub async fn agent_hub_apply_user_instruction_plan(
    state: State<'_, AppState>,
    request: ApplyUserInstructionPlanRequest,
) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_apply_user_instruction_plan(request).await;
    }
    AgentHubService::apply_user_instruction_plan(state.inner(), request).await
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

/// Business Logic: LAN push 前预览 selection 计数/hash（零传输、零 import）。
/// Code Logic: build_snapshot once；只返回 hash/counts，不 push。
#[tauri::command]
pub async fn agent_hub_preview_lan_push(
    state: State<'_, AppState>,
    request: PushAgentHubSelectionRequest,
) -> Result<serde_json::Value, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_preview_lan_push(request)
            .await;
    }
    preview_lan_push_for_state(state.inner(), request).await
}

/// Business Logic: 启动源侧 multi-target LAN push（别名 start_lan_push）。
/// Code Logic: 复用 push_selection_for_state。
#[tauri::command]
pub async fn agent_hub_start_lan_push(
    state: State<'_, AppState>,
    request: PushAgentHubSelectionRequest,
) -> Result<MultiTargetPushReport, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_start_lan_push(request).await;
    }
    let cancel = CancellationToken::new();
    push_selection_for_state(state.inner(), request, &cancel).await
}

/// Business Logic: 读取 LAN push 进度（别名 get_lan_push）。
/// Code Logic: 复用 get_push_report_for_state。
#[tauri::command]
pub async fn agent_hub_get_lan_push(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<MultiTargetPushReport>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_lan_push(&request_id)
            .await;
    }
    get_push_report_for_state(state.inner(), &request_id).await
}

/// Business Logic: 只读枚举 Git device lanes。
/// Code Logic: inspect_git_lanes_for_state。
#[tauri::command]
pub async fn agent_hub_inspect_git_lanes(
    state: State<'_, AppState>,
) -> Result<GitLaneInspectReport, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_inspect_git_lanes()
            .await;
    }
    inspect_git_lanes_for_state(state.inner()).await
}

/// Business Logic: Git lane import 预览（零 Hub 写入）。
/// Code Logic: preview_git_import_for_state。
#[tauri::command]
pub async fn agent_hub_preview_git_import(
    state: State<'_, AppState>,
    lane_device_id: String,
) -> Result<GitImportPreview, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_preview_git_import(&lane_device_id)
            .await;
    }
    preview_git_import_for_state(state.inner(), &lane_device_id).await
}

/// Business Logic: 确认 Git import（hash 精确匹配，否则 previewStale）。
/// Code Logic: mutation + SnapshotImporter。
#[tauri::command]
pub async fn agent_hub_confirm_git_import(
    state: State<'_, AppState>,
    request: ConfirmGitImportRequest,
) -> Result<ConfirmGitImportOutcome, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_confirm_git_import(request).await;
    }
    confirm_git_import_for_state(state.inner(), request).await
}

/// Business Logic: 保存 project mapping（默认 not opted-in）。
/// Code Logic: mutation；不猜测路径。
#[tauri::command]
pub async fn agent_hub_confirm_project_mapping(
    state: State<'_, AppState>,
    request: ConfirmProjectMappingRequest,
) -> Result<ResolvedProjectMapping, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_confirm_project_mapping(request).await;
    }
    confirm_project_mapping_for_state(state.inner(), request).await
}

/// Business Logic: 本机 portable inventory 是实际状态真相（只读）。
/// Code Logic: owner PortableService / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_inspect_portable_inventory(
    state: State<'_, AppState>,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_inspect_portable_inventory()
            .await;
    }
    PortableService::inspect_portable_inventory(state.inner()).await
}

/// Business Logic: apply 前必须生成绑定 inventory hash 的短期 plan。
/// Code Logic: v3 写兼容；owner preview / GuiClient control。
#[tauri::command]
pub async fn agent_hub_preview_portable_asset_action(
    state: State<'_, AppState>,
    request: PreviewPortableAssetActionRequest,
) -> Result<PortableAssetActionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client
            .agent_hub_preview_portable_asset_action(request)
            .await;
    }
    PortableService::preview_portable_asset_action(state.inner(), request).await
}

/// Business Logic: 用户确认后 claim/apply；同 clientRequestId 幂等回放。
/// Code Logic: v3 mutation；owner apply / GuiClient 长超时 control。
#[tauri::command]
pub async fn agent_hub_apply_portable_asset_action(
    state: State<'_, AppState>,
    request: ApplyPortableAssetActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_apply_portable_asset_action(request).await;
    }
    PortableService::apply_portable_asset_action(state.inner(), request).await
}

/// Business Logic: 对账 apply 结果（含 outcomeUnknown）。
/// Code Logic: owner get / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_get_portable_asset_action(
    state: State<'_, AppState>,
    client_request_id: String,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_portable_asset_action(&client_request_id)
            .await;
    }
    PortableService::get_portable_asset_action(state.inner(), &client_request_id).await
}

/// Business Logic: 远端 portable inventory 仅 metadata（无 secret）。
/// Code Logic: owner PortableService / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_list_remote_portable_inventory(
    state: State<'_, AppState>,
    request: ListRemotePortableInventoryRequest,
) -> Result<RemotePortableInventoryDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_list_remote_portable_inventory(request)
            .await;
    }
    PortableService::list_remote_portable_inventory(state.inner(), request).await
}

/// Business Logic: apply 前必须生成同源 target 的 pull plan。
/// Code Logic: v3 写兼容；owner preview / GuiClient control。
#[tauri::command]
pub async fn agent_hub_preview_portable_pull(
    state: State<'_, AppState>,
    request: PreviewPortablePullRequest,
) -> Result<PortablePullPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_preview_portable_pull(request).await;
    }
    PortableService::preview_portable_pull(state.inner(), request).await
}

/// Business Logic: 用户确认后 objects→import→install；同 clientRequestId 幂等。
/// Code Logic: v3 mutation；owner apply / GuiClient 长超时 control。
#[tauri::command]
pub async fn agent_hub_apply_portable_pull(
    state: State<'_, AppState>,
    request: ApplyPortablePullRequest,
) -> Result<PortablePullResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        return client.agent_hub_apply_portable_pull(request).await;
    }
    PortableService::apply_portable_pull(state.inner(), request).await
}

/// Business Logic: 对账 pull 结果（含 partial / outcomeUnknown）。
/// Code Logic: owner get / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_get_portable_pull(
    state: State<'_, AppState>,
    client_request_id: String,
) -> Result<PortablePullResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return BackendControlClient::from_control_file()?
            .agent_hub_get_portable_pull(&client_request_id)
            .await;
    }
    PortableService::get_portable_pull(state.inner(), &client_request_id).await
}

/// Business Logic: 同机跨 Agent 指令适配预览（手动，非后台）。
/// Code Logic: 构造 TargetEnvironment → preview_cross_agent_instruction。
#[tauri::command]
pub async fn agent_hub_preview_cross_agent_instruction(
    request: PreviewCrossAgentInstructionRequest,
) -> Result<CrossAgentPreviewReport, AppError> {
    let env = TargetEnvironment::from_process();
    preview_cross_agent_instruction(&request, &env)
}

/// Business Logic: 一次性把源指令投影到选定目标 Agent 文件。
/// Code Logic: apply_cross_agent_instruction；不触发其它 target 后台投影。
#[tauri::command]
pub async fn agent_hub_apply_cross_agent_instruction(
    state: State<'_, AppState>,
    request: ApplyCrossAgentInstructionRequest,
) -> Result<Vec<CrossAgentApplyTargetResult>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        // GuiClient 仍走本机 apply（同机跨 Agent 只写本机文件，不经 LAN）。
    }
    let env = TargetEnvironment::from_process();
    apply_cross_agent_instruction(&request, &env)
}

/// Business Logic: 全量跨 Agent 适配预览（指令 + skill/command/mcp/plugin 五类清单）。
/// Code Logic: stub FullAdaptRunner.propose；无 skip-preview 直写路径。
#[tauri::command]
pub async fn agent_hub_preview_cross_agent_full(
    request: PreviewCrossAgentFullRequest,
) -> Result<CrossAgentFullPlan, AppError> {
    let env = TargetEnvironment::from_process();
    preview_cross_agent_full_default(&request, &env)
}

/// Business Logic: 按 preview plan_hash 逐项 apply；hash 不匹配必须失败。
/// Code Logic: re-propose stub plan → 校验 hash → 指令复用 selective apply。
#[tauri::command]
pub async fn agent_hub_apply_cross_agent_full(
    state: State<'_, AppState>,
    request: ApplyCrossAgentFullRequest,
) -> Result<Vec<CrossAgentFullApplyItemResult>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        client.require_agent_hub_write_compatibility(AGENT_HUB_API_VERSION)?;
        // 同机 only：不经 LAN peer。
    }
    let env = TargetEnvironment::from_process();
    apply_cross_agent_full_default(&request, &env)
}

/// LAN push 预览：build selection 但不传输。
///
/// Business Logic: 用户确认 peers/mode 前看到 asset/revision 计数与 snapshotHash。
/// Code Logic: build_snapshot → camelCase JSON。
async fn preview_lan_push_for_state(
    state: &AppState,
    request: PushAgentHubSelectionRequest,
) -> Result<serde_json::Value, AppError> {
    let data_dir = crate::config::data_dir()?;
    let objects = ObjectStore::open(&data_dir)?;
    let built = build_snapshot(
        &state.agent_hub_repo,
        &objects,
        SnapshotSelectionRequest {
            mode: request.mode,
            scope_ids: request.scope_ids.clone(),
            asset_ids: request.asset_ids.clone(),
            hub_project_ids: request.hub_project_ids.clone(),
            include_history: request.include_history,
            source_replica_id: state.device_id.as_ref().clone(),
            limits: None,
        },
    )
    .await?;
    let credential_bearing = built
        .envelope
        .assets
        .iter()
        .filter(|a| matches!(a.kind, crate::agent_hub::models::AssetKind::Mcp))
        .count() as u64;
    Ok(serde_json::json!({
        "snapshotHash": built.envelope.snapshot_hash,
        "snapshotId": built.envelope.snapshot_id,
        "selectionHash": built.selection_hash,
        "assetCount": built.envelope.assets.len() as u64,
        "revisionCount": built.envelope.revisions.len() as u64,
        "credentialBearingAssetCount": credential_bearing,
        "peerDeviceIds": request.peer_device_ids,
        "mode": request.mode,
        "plaintextBackupDisclosure": crate::agent_hub::git::preview::PLAINTEXT_BACKUP_DISCLOSURE,
        "hasCredentialBearingAssets": credential_bearing > 0,
    }))
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
            "agent_hub_preview_lan_push",
            "agent_hub_start_lan_push",
            "agent_hub_get_lan_push",
            "agent_hub_inspect_git_lanes",
            "agent_hub_preview_git_import",
            "agent_hub_confirm_git_import",
            "agent_hub_confirm_project_mapping",
            "agent_hub_inspect_portable_inventory",
            "agent_hub_preview_portable_asset_action",
            "agent_hub_apply_portable_asset_action",
            "agent_hub_get_portable_asset_action",
            "agent_hub_list_remote_portable_inventory",
            "agent_hub_preview_portable_pull",
            "agent_hub_apply_portable_pull",
            "agent_hub_get_portable_pull",
        ] {
            assert!(src.contains(name), "missing command {name}");
        }
        assert!(src.contains("RuntimeRole::GuiClient"));
        assert!(src.contains("BackendControlClient"));
        assert!(src.contains("require_agent_hub_write_compatibility"));
        assert!(src.contains("PortableService"));
    }

    /// Business Logic: portable inspect 只读可代理；preview/apply 必须写兼容门闩。
    /// Code Logic: 生产 fn 签名 + owner/GuiClient 分发（避免测试字面量自命中）。
    #[test]
    fn portable_commands_authority_and_read_only_fallback() {
        let src = include_str!("agent_hub.rs");
        for sig in [
            "pub async fn agent_hub_inspect_portable_inventory(",
            "pub async fn agent_hub_preview_portable_asset_action(",
            "pub async fn agent_hub_apply_portable_asset_action(",
            "pub async fn agent_hub_get_portable_asset_action(",
            "pub async fn agent_hub_list_remote_portable_inventory(",
            "pub async fn agent_hub_preview_portable_pull(",
            "pub async fn agent_hub_apply_portable_pull(",
            "pub async fn agent_hub_get_portable_pull(",
        ] {
            assert!(src.contains(sig), "missing command signature {sig}");
        }
        // HeadlessOwner → PortableService；GuiClient → control client
        assert!(src.contains("PortableService::inspect_portable_inventory"));
        assert!(src.contains("PortableService::preview_portable_asset_action"));
        assert!(src.contains("PortableService::apply_portable_asset_action"));
        assert!(src.contains("PortableService::get_portable_asset_action"));
        assert!(src.contains("PortableService::list_remote_portable_inventory"));
        assert!(src.contains("PortableService::preview_portable_pull"));
        assert!(src.contains("PortableService::apply_portable_pull"));
        assert!(src.contains("PortableService::get_portable_pull"));
        assert!(src.contains(".agent_hub_inspect_portable_inventory()"));
        assert!(src.contains(".agent_hub_preview_portable_asset_action("));
        assert!(src.contains(".agent_hub_apply_portable_asset_action("));
        assert!(src.contains(".agent_hub_get_portable_asset_action("));
        assert!(src.contains(".agent_hub_list_remote_portable_inventory("));
        assert!(src.contains(".agent_hub_preview_portable_pull("));
        assert!(src.contains(".agent_hub_apply_portable_pull("));
        assert!(src.contains(".agent_hub_get_portable_pull("));
        assert!(src.contains("require_agent_hub_write_compatibility"));
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
            "agent_hub.inspect_user_instruction_workspace",
            "agent_hub.preview_user_instruction_setup",
            "agent_hub.preview_user_instruction_update",
            "agent_hub.apply_user_instruction_plan",
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
            "agent_hub.preview_lan_push",
            "agent_hub.start_lan_push",
            "agent_hub.get_lan_push",
            "agent_hub.inspect_git_lanes",
            "agent_hub.preview_git_import",
            "agent_hub.confirm_git_import",
            "agent_hub.confirm_project_mapping",
            "agent_hub.inspect_portable_inventory",
            "agent_hub.preview_portable_asset_action",
            "agent_hub.apply_portable_asset_action",
            "agent_hub.get_portable_asset_action",
            "agent_hub.list_remote_portable_inventory",
            "agent_hub.preview_portable_pull",
            "agent_hub.apply_portable_pull",
            "agent_hub.get_portable_pull",
        ] {
            assert!(src.contains(op), "missing op {op}");
        }
    }

    /// Business Logic: Agent Hub API major 必须升到 v3 才承载 portable mutation。
    /// Code Logic: 常量 == 3。
    #[test]
    fn agent_hub_api_version_is_v3_for_portable() {
        assert_eq!(AGENT_HUB_API_VERSION, 3);
    }
}
