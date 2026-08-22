//! commands/agent_hub.rs — Multi-CLI Agent Hub thin Tauri 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端通过 IPC 读写 Agent Hub status/assets/instruction/project/conflict；
//!     GuiClient 必须代理到 sidecar owner，禁止第二写路径。
//!
//! Code Logic（这个模块做什么）:
//!     HeadlessOwner → AgentHubService；GuiClient → BackendControlClient agent_hub_*；
//!     AppState runtime 复用 control client，失败失效缓存；mutation 由 typed client/control 双重校验版本。

use crate::agent_hub::cross_agent::{
    enforce_cross_agent_preview_only, preview_cross_agent_instruction,
    ApplyCrossAgentInstructionRequest, CrossAgentApplyTargetResult, CrossAgentPreviewReport,
    PreviewCrossAgentInstructionRequest,
};
use crate::agent_hub::cross_agent_full::{
    ApplyCrossAgentFullRequest, CrossAgentFullApplyItemResult, CrossAgentFullPlan,
    PreviewCrossAgentFullRequest,
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
use crate::agent_hub::portable_inventory::{PortableInventoryQuery, PortableInventorySnapshotDto};
use crate::agent_hub::portable_service::PortableService;
use crate::agent_hub::project_scope::{AgentHubProjectPreview, AgentHubProjectStatus};
use crate::agent_hub::remote_client::{
    apply_portable_asset_action_for_state, apply_user_instruction_plan_for_state,
    get_portable_asset_action_for_state, inspect_portable_inventory_for_state,
    inspect_user_instruction_workspace_for_state, list_user_instruction_slot_versions_for_state,
    preview_portable_asset_action_for_state, preview_user_instruction_setup_for_state,
    preview_user_instruction_update_for_state, read_user_native_instruction_file_for_state,
    restore_user_instruction_slot_version_for_state, save_user_instruction_blocks_for_state,
    write_user_native_instruction_file_for_state,
};
use crate::agent_hub::replication::pull::{
    apply_remote_project_portable_action, enable_remote_project,
    get_remote_project_portable_action, inspect_remote_project_portable_inventory,
    preview_remote_project, preview_remote_project_portable_action, ApplyPortablePullRequest,
    ApplyRemoteProjectPortableActionRequest, GetRemoteProjectPortableActionRequest,
    InspectRemoteProjectPortableInventoryRequest, ListRemotePortableInventoryRequest,
    PortablePullPlanDto, PortablePullResultDto, PreviewPortablePullRequest,
    PreviewRemoteProjectPortableActionRequest, RemotePortableInventoryDto, RemoteProjectRefRequest,
};
use crate::agent_hub::replication::sender::{
    compute_push_preview_token, get_push_report_for_state, push_selection_for_state,
    MultiTargetPushReport, PushAgentHubSelectionRequest,
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
    AdaptInstructionToOtherAgentsRequest, AdaptInstructionToOtherAgentsResult,
    AnalyzeInstructionOriginalRequest, AnalyzeInstructionOriginalResult,
    ApplyUserInstructionPlanRequest, ApplyUserInstructionPlanResultDto,
    PreviewUserInstructionRequest, ReadUserNativeInstructionFileRequest,
    ReviseInstructionSlotRequest, ReviseInstructionSlotResult, SaveUserInstructionBlocksRequest,
    UserInstructionCanonicalDto, UserInstructionPlanDto, UserInstructionWorkspaceDto,
    UserNativeInstructionFileDto, WriteUserNativeInstructionFileRequest,
};
use crate::backend::authority::RuntimeRole;
#[cfg(test)]
use crate::backend::control::AGENT_HUB_API_VERSION;
use crate::error::AppError;
use crate::state::AppState;
use std::collections::BTreeMap;
use tauri::State;
use tokio_util::sync::CancellationToken;

/// 复用 AppState 中的 control client，并在失败后失效 descriptor，供下一次调用刷新。
macro_rules! proxy_agent_hub {
    ($state:expr, |$client:ident| $call:expr) => {{
        let runtime = &$state.backend_control_client_runtime;
        let $client = runtime.client()?;
        let result = ($call).await;
        if result.is_err() {
            runtime.invalidate_if_current(&$client);
        }
        result
    }};
}

/// Business Logic: 首屏 status。
/// Code Logic: owner service / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_get_status(
    state: State<'_, AppState>,
) -> Result<AgentHubStatusDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client.agent_hub_get_status());
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
        return proxy_agent_hub!(state, |client| client.agent_hub_list_assets(req));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_get_asset(&asset_id));
    }
    AgentHubService::get_asset(state.inner(), &asset_id).await
}

/// Business Logic: 用户级指令默认入口展示 sidecar owner 解析的真实 source chain。
/// Code Logic: deviceId 非空则 P2P 到 owning peer；GuiClient 经 control 透传 deviceId。
#[tauri::command]
pub async fn agent_hub_inspect_user_instruction_workspace(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<UserInstructionWorkspaceDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_inspect_user_instruction_workspace(device_id.clone()));
    }
    inspect_user_instruction_workspace_for_state(state.inner(), device_id.as_deref()).await
}

/// Business Logic: 读取各 CLI 配置目录里真实加载的 AGENTS.md / CLAUDE.md / GEMINI.md。
/// Code Logic: deviceId 非空则 P2P；路径必须在 owner 白名单内。
#[tauri::command]
pub async fn agent_hub_read_user_native_instruction_file(
    state: State<'_, AppState>,
    request: ReadUserNativeInstructionFileRequest,
    device_id: Option<String>,
) -> Result<UserNativeInstructionFileDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_read_user_native_instruction_file(request, device_id.clone()));
    }
    read_user_native_instruction_file_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 用户保存自己的原生提示词文件，CAS 防覆盖。
/// Code Logic: deviceId 非空则 P2P；白名单路径 + expectedHash。
#[tauri::command]
pub async fn agent_hub_write_user_native_instruction_file(
    state: State<'_, AppState>,
    request: WriteUserNativeInstructionFileRequest,
    device_id: Option<String>,
) -> Result<UserNativeInstructionFileDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_write_user_native_instruction_file(request, device_id.clone()));
    }
    write_user_native_instruction_file_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 首次设置必须先生成绑定 revision/inventory/hash 的预览计划。
/// Code Logic: 新 V2 control 合同需要 API 版本一致；不执行目标文件 mutation。
#[tauri::command]
pub async fn agent_hub_preview_user_instruction_setup(
    state: State<'_, AppState>,
    request: PreviewUserInstructionRequest,
    device_id: Option<String>,
) -> Result<UserInstructionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_user_instruction_setup(request, device_id.clone()));
    }
    preview_user_instruction_setup_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 日常更新与首次设置共享相同的 preview 安全门闩。
/// Code Logic: owner preview / GuiClient V2 control。
#[tauri::command]
pub async fn agent_hub_preview_user_instruction_update(
    state: State<'_, AppState>,
    request: PreviewUserInstructionRequest,
    device_id: Option<String>,
) -> Result<UserInstructionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_user_instruction_update(request, device_id.clone()));
    }
    preview_user_instruction_update_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 用户确认后才可应用短期 plan；当前写能力未认证时逐 target blocked。
/// Code Logic: owner 原子 claim/apply / GuiClient V2 control mutation。
#[tauri::command]
pub async fn agent_hub_apply_user_instruction_plan(
    state: State<'_, AppState>,
    request: ApplyUserInstructionPlanRequest,
    device_id: Option<String>,
) -> Result<ApplyUserInstructionPlanResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_apply_user_instruction_plan(request, device_id.clone()));
    }
    apply_user_instruction_plan_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 保存块文档是 cc-partner 内部编辑态，独立于 CLI 写入门禁，但仍受 V2 合同约束。
/// Code Logic: owner canonical CAS / GuiClient V2 control mutation。
#[tauri::command]
pub async fn agent_hub_save_user_instruction_blocks(
    state: State<'_, AppState>,
    request: SaveUserInstructionBlocksRequest,
    device_id: Option<String>,
) -> Result<UserInstructionCanonicalDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_save_user_instruction_blocks(request, device_id.clone()));
    }
    save_user_instruction_blocks_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 列出三槽历史（公共 / 适配 / 独有）的最近 20 条 history + 30 天 conflict 副本。
///
/// Code Logic: owner 直接读 content_versions；GuiClient 经 loopback control query。
///   slot → content_versions.item_id 映射在 inventory 层完成；
///   返回 ContentVersionDto 数组供 VersionHistoryDrawer 渲染。
#[tauri::command]
pub async fn agent_hub_list_user_instruction_slot_versions(
    state: State<'_, AppState>,
    request: crate::agent_hub::user_instructions::ListUserInstructionSlotVersionsRequest,
    device_id: Option<String>,
) -> Result<Vec<crate::commands::prompts::ContentVersionDto>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_list_user_instruction_slot_versions(request, device_id.clone()));
    }
    list_user_instruction_slot_versions_for_state(state.inner(), device_id.as_deref(), request)
        .await
}

/// Business Logic: 把目标历史版本恢复到当前槽，写一条新 head；与 save_blocks 同口径 CAS。
///
/// Code Logic: owner 走 inventory::restore_user_instruction_slot_version
///   （CAS + pre-restore baseline + commit + prune_retention）；
///   GuiClient 经 loopback control mutation（要求 agent_hub v4 write compatibility）。
#[tauri::command]
pub async fn agent_hub_restore_user_instruction_slot_version(
    state: State<'_, AppState>,
    request: crate::agent_hub::user_instructions::RestoreUserInstructionSlotRequest,
    device_id: Option<String>,
) -> Result<UserInstructionCanonicalDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_restore_user_instruction_slot_version(request, device_id.clone()));
    }
    restore_user_instruction_slot_version_for_state(state.inner(), device_id.as_deref(), request)
        .await
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
        return proxy_agent_hub!(state, |client| client.agent_hub_update_instruction(req));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_update_instruction_block(req));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_pair_instruction_variants(req));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_project(&project_id));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_enable_project(&project_id, true));
    }
    AgentHubService::enable_project(state.inner(), &project_id).await
}

/// Business Logic: 预览 remote shortcut owning peer 上的项目 opt-in。
#[tauri::command]
pub async fn agent_hub_preview_remote_project(
    state: State<'_, AppState>,
    project_ref: String,
) -> Result<AgentHubProjectPreview, AppError> {
    let request = RemoteProjectRefRequest { project_ref };
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_remote_project(request));
    }
    preview_remote_project(state.inner(), request).await
}

/// Business Logic: 在 remote shortcut owning peer 显式启用项目 Agent Hub。
#[tauri::command]
pub async fn agent_hub_enable_remote_project(
    state: State<'_, AppState>,
    project_ref: String,
) -> Result<AgentHubProjectStatus, AppError> {
    let request = RemoteProjectRefRequest { project_ref };
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_enable_remote_project(request));
    }
    enable_remote_project(state.inner(), request).await
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
        return proxy_agent_hub!(state, |client| client.agent_hub_resolve_conflict(req));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_set_target_binding(req));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_set_target_presence(req));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_set_target_enabled(req));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_restore_detached_target(req));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_delete_asset_everywhere(req));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_push_selection(request));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_get_push_report(&request_id));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_preview_lan_push(request));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_start_lan_push(request));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_get_lan_push(&request_id));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_inspect_git_lanes());
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_git_import(&lane_device_id));
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
        return proxy_agent_hub!(state, |client| client.agent_hub_confirm_git_import(request));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_confirm_project_mapping(request));
    }
    confirm_project_mapping_for_state(state.inner(), request).await
}

/// Business Logic: portable inventory 是实际状态真相（只读）；deviceId 非空则对端 user scope。
/// Code Logic: owner for_state / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_inspect_portable_inventory(
    state: State<'_, AppState>,
    request: Option<PortableInventoryQuery>,
    device_id: Option<String>,
) -> Result<PortableInventorySnapshotDto, AppError> {
    let query = request.unwrap_or_default();
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_inspect_portable_inventory(query, device_id.clone()));
    }
    inspect_portable_inventory_for_state(state.inner(), device_id.as_deref(), query).await
}

/// Business Logic: apply 前必须生成绑定 inventory hash 的短期 plan；deviceId 非空则对端。
/// Code Logic: v3 写兼容；owner preview / GuiClient control。
#[tauri::command]
pub async fn agent_hub_preview_portable_asset_action(
    state: State<'_, AppState>,
    request: PreviewPortableAssetActionRequest,
    device_id: Option<String>,
) -> Result<PortableAssetActionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_portable_asset_action(request, device_id.clone()));
    }
    preview_portable_asset_action_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 用户确认后 claim/apply；同 clientRequestId 幂等回放；deviceId 非空则对端。
/// Code Logic: v3 mutation；owner apply / GuiClient 长超时 control。
#[tauri::command]
pub async fn agent_hub_apply_portable_asset_action(
    state: State<'_, AppState>,
    request: ApplyPortableAssetActionRequest,
    device_id: Option<String>,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_apply_portable_asset_action(request, device_id.clone()));
    }
    apply_portable_asset_action_for_state(state.inner(), device_id.as_deref(), request).await
}

/// Business Logic: 对账 apply 结果（含 outcomeUnknown）；deviceId 非空则对端。
/// Code Logic: owner get / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_get_portable_asset_action(
    state: State<'_, AppState>,
    client_request_id: String,
    device_id: Option<String>,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_get_portable_asset_action(&client_request_id, device_id.clone()));
    }
    get_portable_asset_action_for_state(state.inner(), device_id.as_deref(), &client_request_id)
        .await
}

/// Business Logic: 读取 remote shortcut owning peer 的精确项目库存。
#[tauri::command]
pub async fn agent_hub_inspect_remote_project_portable_inventory(
    state: State<'_, AppState>,
    request: InspectRemoteProjectPortableInventoryRequest,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_inspect_remote_project_portable_inventory(request));
    }
    inspect_remote_project_portable_inventory(state.inner(), request).await
}

/// Business Logic: 在 remote shortcut owning peer 生成精确项目动作计划。
#[tauri::command]
pub async fn agent_hub_preview_remote_project_portable_action(
    state: State<'_, AppState>,
    request: PreviewRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionPlanDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_remote_project_portable_action(request));
    }
    preview_remote_project_portable_action(state.inner(), request).await
}

/// Business Logic: 在 remote shortcut owning peer apply 精确项目动作计划。
#[tauri::command]
pub async fn agent_hub_apply_remote_project_portable_action(
    state: State<'_, AppState>,
    request: ApplyRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_apply_remote_project_portable_action(request));
    }
    apply_remote_project_portable_action(state.inner(), request).await
}

/// Business Logic: 对账 remote shortcut owning peer 的项目动作结果。
#[tauri::command]
pub async fn agent_hub_get_remote_project_portable_action(
    state: State<'_, AppState>,
    request: GetRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_get_remote_project_portable_action(request));
    }
    get_remote_project_portable_action(state.inner(), request).await
}

/// Business Logic: 远端 portable inventory 仅 metadata（无 secret）。
/// Code Logic: owner PortableService / GuiClient control query。
#[tauri::command]
pub async fn agent_hub_list_remote_portable_inventory(
    state: State<'_, AppState>,
    request: ListRemotePortableInventoryRequest,
) -> Result<RemotePortableInventoryDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        return proxy_agent_hub!(state, |client| client
            .agent_hub_list_remote_portable_inventory(request));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_preview_portable_pull(request));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_apply_portable_pull(request));
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
        return proxy_agent_hub!(state, |client| client
            .agent_hub_get_portable_pull(&client_request_id));
    }
    PortableService::get_portable_pull(state.inner(), &client_request_id).await
}

/// Business Logic: 同机跨 Agent 指令适配预览（手动，非后台）。
/// Code Logic: 构造 TargetEnvironment → preview_cross_agent_instruction。
#[tauri::command]
pub async fn agent_hub_preview_cross_agent_instruction(
    state: State<'_, AppState>,
    request: PreviewCrossAgentInstructionRequest,
) -> Result<CrossAgentPreviewReport, AppError> {
    let mut report = if state.runtime_role == RuntimeRole::GuiClient {
        proxy_agent_hub!(state, |client| client
            .agent_hub_preview_cross_agent_instruction(request))?
    } else {
        let env = TargetEnvironment::from_process();
        preview_cross_agent_instruction(&request, &env)?
    };
    enforce_cross_agent_preview_only(&mut report);
    Ok(report)
}

/// Business Logic: 真实 CLI 写入未通过 L3 认证时，旧客户端也不得绕过预览边界。
/// Code Logic: 在 GuiClient proxy 之前固定 fail-closed，防止新 GUI 把 mutation 转发给旧 sidecar。
#[tauri::command]
pub async fn agent_hub_apply_cross_agent_instruction(
    _state: State<'_, AppState>,
    _request: ApplyCrossAgentInstructionRequest,
) -> Result<Vec<CrossAgentApplyTargetResult>, AppError> {
    Err(AppError::validation("CROSS_AGENT_APPLY_NOT_CERTIFIED"))
}

/// Business Logic: full runner 仍是 stub，不对用户暴露伪可执行 plan。
/// Code Logic: 在 GuiClient proxy 之前稳定拒绝，避免混合版本回退到旧 sidecar。
#[tauri::command]
pub async fn agent_hub_preview_cross_agent_full(
    _state: State<'_, AppState>,
    _request: PreviewCrossAgentFullRequest,
) -> Result<CrossAgentFullPlan, AppError> {
    Err(AppError::validation("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"))
}

/// Business Logic: full apply 在功能未完成且未认证时必须 fail-closed。
/// Code Logic: 在任何 proxy、扫描或 writer 之前返回与 preview 相同的稳定 unavailable code。
#[tauri::command]
pub async fn agent_hub_apply_cross_agent_full(
    _state: State<'_, AppState>,
    _request: ApplyCrossAgentFullRequest,
) -> Result<Vec<CrossAgentFullApplyItemResult>, AppError> {
    Err(AppError::validation("CROSS_AGENT_FULL_ADAPT_UNAVAILABLE"))
}

/// 已通过校验的改写任务（按 lane 携带当前正文）。
#[derive(Debug)]
enum PreparedReviseLane {
    Common { current: String },
    Exclusive { current: String },
    Adapted { variants: BTreeMap<String, String> },
}

#[derive(Debug)]
struct PreparedRevise {
    agent: String,
    direction: String,
    lane: PreparedReviseLane,
}

/// Business Logic: 改写命令在调 Claude 前必须拒绝空方向 / 非法 lane / 超长输入。
/// Code Logic: trim direction；lane 归一；适配补齐三端；合计字符不超过上限。
fn prepare_revise_instruction_slot(
    request: &ReviseInstructionSlotRequest,
) -> Result<PreparedRevise, AppError> {
    let direction = request.direction.trim();
    if direction.is_empty() {
        return Err(AppError::validation("INSTRUCTION_REVISE_EMPTY_DIRECTION"));
    }
    let agent = normalize_agent_target_token(&request.agent)?;
    let lane = match request.lane.trim() {
        "common" => PreparedReviseLane::Common {
            current: request.common_markdown.clone().unwrap_or_default(),
        },
        "exclusive" => PreparedReviseLane::Exclusive {
            current: request.exclusive_markdown.clone().unwrap_or_default(),
        },
        "adapted" => {
            let incoming = request.adapted_variants.clone().unwrap_or_default();
            let mut variants = BTreeMap::new();
            for key in ["claude", "codex", "opencode"] {
                variants.insert(
                    key.to_string(),
                    incoming.get(key).cloned().unwrap_or_default(),
                );
            }
            PreparedReviseLane::Adapted { variants }
        }
        _ => return Err(AppError::validation("INSTRUCTION_REVISE_LANE_INVALID")),
    };

    let mut chars = direction.chars().count();
    match &lane {
        PreparedReviseLane::Common { current } | PreparedReviseLane::Exclusive { current } => {
            chars = chars.saturating_add(current.chars().count());
        }
        PreparedReviseLane::Adapted { variants } => {
            for text in variants.values() {
                chars = chars.saturating_add(text.chars().count());
            }
        }
    }
    if chars > MAX_INSTRUCTION_LLM_CHARS {
        return Err(AppError::validation("INSTRUCTION_REVISE_INPUT_TOO_LARGE"));
    }

    Ok(PreparedRevise {
        agent,
        direction: direction.to_string(),
        lane,
    })
}

const INSTRUCTION_LLM_TIMEOUT_SECS: u64 = 180;
const MAX_INSTRUCTION_LLM_CHARS: usize = 80_000;

/// 表面层关键词：命中则整段视为混写，只能进 adapted（不得进 common）。
const INSTRUCTION_SURFACE_MARKERS: &[&str] = &[
    "claude.md",
    "agents.md",
    "agents.override.md",
    "~/.claude",
    "~/.codex",
    "~/.config/opencode",
    "claude_config_dir",
    "codex_home",
    "opencode_config",
    "claude code",
    "anthropic",
    "sonnet",
    "opus",
    "haiku",
    "gpt-5",
    "o3-",
    "o4-",
    "cc-partner",
    ".claude/",
    ".codex/",
    "opencode",
    "codex cli",
    "claude cli",
];

/// Business Logic: 产品层强制「语义+表面混写只进适配」；模型偶发把同一段同时放进
///   common/adapted 时，后处理去重，避免公共/适配几乎相同。
/// Code Logic: 按空行分段；表面词 → 整段并入 adapted；与 adapted 覆盖/高重叠 → 从 common 删除；
///   exclusive 若被 adapted 覆盖则丢弃（优先 adapted）。
pub(crate) fn normalize_instruction_analyze_parts(
    common: &str,
    adapted: &str,
    exclusive: &str,
) -> AnalyzeInstructionOriginalResult {
    let mut adapted_blocks = split_instruction_blocks(adapted);
    let exclusive_blocks = split_instruction_blocks(exclusive);
    let common_blocks = split_instruction_blocks(common);

    let mut kept_common: Vec<String> = Vec::new();
    for block in common_blocks {
        if block_has_surface_markers(&block) {
            // 混写整段进适配，不保留 hollow common stub。
            push_unique_block(&mut adapted_blocks, block);
            continue;
        }
        if block_covered_by_adapted(&block, &adapted_blocks, adapted) {
            continue;
        }
        kept_common.push(block);
    }

    let mut kept_exclusive: Vec<String> = Vec::new();
    for block in exclusive_blocks {
        if block_covered_by_adapted(&block, &adapted_blocks, adapted)
            || block_covered_by_adapted(
                &block,
                &adapted_blocks,
                &join_instruction_blocks(&adapted_blocks),
            )
        {
            // 真独占才保留；与适配高度重叠的段落归适配侧。
            continue;
        }
        // 表面可同构映射的不应进 exclusive：有表面词且能被 adapted 语义覆盖时已在上面 drop。
        kept_exclusive.push(block);
    }

    AnalyzeInstructionOriginalResult {
        common: join_instruction_blocks(&kept_common),
        adapted: join_instruction_blocks(&adapted_blocks),
        exclusive: join_instruction_blocks(&kept_exclusive),
    }
}

fn split_instruction_blocks(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn join_instruction_blocks(blocks: &[String]) -> String {
    blocks
        .iter()
        .map(|b| b.trim_end())
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_instruction_ws(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn block_has_surface_markers(block: &str) -> bool {
    let lower = block.to_ascii_lowercase();
    INSTRUCTION_SURFACE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn instruction_word_set(text: &str) -> std::collections::HashSet<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn jaccard_similarity(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn block_covered_by_adapted(
    common_block: &str,
    adapted_blocks: &[String],
    adapted_full: &str,
) -> bool {
    let cn = normalize_instruction_ws(common_block);
    if cn.is_empty() {
        return true;
    }
    let an = normalize_instruction_ws(adapted_full);
    if !an.is_empty() && an.contains(&cn) {
        return true;
    }
    let cset = instruction_word_set(common_block);
    for ab in adapted_blocks {
        let abn = normalize_instruction_ws(ab);
        if abn == cn {
            return true;
        }
        let aset = instruction_word_set(ab);
        let j = jaccard_similarity(&cset, &aset);
        // 清洗版 common 与带表面词的 adapted 高重叠：只保留 adapted。
        if j >= 0.82 {
            let cl = cn.chars().count();
            let al = abn.chars().count();
            if cl <= al.saturating_mul(12) / 10 + 48 {
                return true;
            }
        }
        if !cset.is_empty() {
            let covered = cset.intersection(&aset).count() as f64 / cset.len() as f64;
            if covered >= 0.9 && cset.len() >= 4 {
                return true;
            }
        }
    }
    let aset_all = instruction_word_set(adapted_full);
    if !cset.is_empty() && !aset_all.is_empty() {
        let covered = cset.intersection(&aset_all).count() as f64 / cset.len() as f64;
        if covered >= 0.92 && cset.len() >= 6 {
            return true;
        }
    }
    false
}

fn push_unique_block(blocks: &mut Vec<String>, block: String) {
    let bn = normalize_instruction_ws(&block);
    if bn.is_empty() {
        return;
    }
    if blocks
        .iter()
        .any(|existing| normalize_instruction_ws(existing) == bn)
    {
        return;
    }
    blocks.push(block);
}

/// Business Logic: 独有页把原始文件拆成公共/适配/独有三部分（仅草稿，不写盘）。
/// Code Logic: deviceId 非空则对端 HeadlessCompletion；本机 GuiClient 仍可直跑 CLI。
#[tauri::command]
pub async fn agent_hub_analyze_instruction_original(
    state: State<'_, AppState>,
    request: AnalyzeInstructionOriginalRequest,
    device_id: Option<String>,
) -> Result<AnalyzeInstructionOriginalResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        if device_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|id| !id.is_empty())
        {
            return proxy_agent_hub!(state, |client| client
                .agent_hub_analyze_instruction_original(request, device_id.clone()));
        }
        return analyze_instruction_original_for_state(state.inner(), request).await;
    }
    crate::agent_hub::remote_client::analyze_instruction_original_for_device(
        state.inner(),
        device_id.as_deref(),
        request.clone(),
        analyze_instruction_original_for_state(state.inner(), request),
    )
    .await
}

/// Business Logic: owner / P2P 路由共用本机拆解。
/// Code Logic: 校验长度后跑 Claude structured JSON。
pub(crate) async fn analyze_instruction_original_for_state(
    state: &AppState,
    request: AnalyzeInstructionOriginalRequest,
) -> Result<AnalyzeInstructionOriginalResult, AppError> {
    let original = request.original_markdown.trim();
    if original.is_empty() {
        return Err(AppError::validation("INSTRUCTION_ANALYZE_EMPTY_ORIGINAL"));
    }
    if original.chars().count() > MAX_INSTRUCTION_LLM_CHARS {
        return Err(AppError::validation(
            "INSTRUCTION_ANALYZE_ORIGINAL_TOO_LARGE",
        ));
    }
    let agent = normalize_agent_target_token(&request.agent)?;
    let (cli_path, model, provider_id) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
            cfg.internal_claude.provider_id.clone(),
        )
    };
    let provider_dir =
        crate::internal_claude::resolve_internal_provider_config_dir(provider_id.as_deref())
            .await?;
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["common", "adapted", "exclusive"],
        "properties": {
            "common": { "type": "string" },
            "adapted": { "type": "string" },
            "exclusive": { "type": "string" }
        }
    });
    // Business Logic: 拆解优先把「可同构改写」的 agent 表面差放进 adapted；
    //   语义与表面混写时整段落 adapted（不硬拆 common）；真独占才 exclusive。
    // Code Logic: 固定三槽分类规则 + 模型/subagent 语义提示注入 Claude structured JSON。
    let prompt = format!(
        "You split an agent instruction document into three markdown parts for Agent Hub.\n\
         Target agent: {agent}\n\
         Agents in this product: claude, codex, opencode (same subagent roles/tiers; different surfaces).\n\
         Return ONLY JSON with keys common, adapted, exclusive.\n\
         \n\
         Slot definitions:\n\
         - common: pure cross-agent semantic rules only — process, style, review gates, acceptance criteria,\n\
           and subagent role boundaries (explore / implement / review / verify) when written WITHOUT any\n\
           agent-specific model ids, CLI names, config paths, tool names, or instruction-file names.\n\
         - adapted: content that is isomorphic across agents but currently written in the target agent's\n\
           surface (CLI names, config roots, instruction files, tool terms, concrete model ids, routing tables).\n\
           Later rewrite will map surface for other agents while keeping the same intent.\n\
         - exclusive: content that has no safe isomorphic mapping for other agents (true capability-only\n\
           details, unique hooks/plugins/permissions with no counterpart). Prefer common/adapted over exclusive.\n\
         \n\
         Critical mixed-content rule (must follow):\n\
         - Real documents usually interleave semantic intent with surface wording in the SAME paragraph/list/table.\n\
         - When a passage mixes semantic layer AND surface layer, put the ENTIRE passage into adapted ONLY.\n\
           Do NOT split one mixed passage into common+adapted. Do NOT strip model names and leave a hollow common stub.\n\
         - NEVER put the same passage (or a cleaned paraphrase of the same passage) into both common and adapted.\n\
           Overlap is a hard error: choose adapted for mixed/surface-bearing text; common only for pure semantics.\n\
         - Only put text in common when it is fully free of agent-specific surface terms.\n\
         \n\
         Subagent / model routing rules:\n\
         - Subagent duties and tier ranks are the SAME across agents (e.g. explore vs implement vs review;\n\
           high/mid/low effort). Different concrete model ids (opus/sonnet/haiku, Codex models, OpenCode models)\n\
           are surface differences, NOT exclusive ownership and NOT different duties.\n\
         - A mixed model-routing table (duty + concrete model name) → whole table/list to adapted.\n\
         - Pure duty text with no model/CLI surface → common.\n\
         \n\
         Surface examples that belong in adapted (not exclusive) when isomorphic:\n\
         - Instruction files: CLAUDE.md ↔ AGENTS.md / AGENTS.override.md\n\
         - Config roots: ~/.claude, ~/.codex, ~/.config/opencode, CLAUDE_CONFIG_DIR, CODEX_HOME, OPENCODE_CONFIG_DIR\n\
         - Tool/CLI product names and invocation wording for the same role\n\
         - Concrete model ids that encode a tier/duty mapping\n\
         \n\
         Exclusive only when there is no counterpart (examples): Claude-only permission/TCC flows with no peer,\n\
         product features unique to one CLI with no rewrite path.\n\
         \n\
         Keep the original language. Do not invent facts. Preserve headings/lists where possible.\n\
         Empty string is allowed for a part. Prefer moving borderline content to adapted over exclusive.\n\
         Original document:\n---\n{original}\n---"
    );
    let result =
        crate::claude_cli::run_structured_json_with_cwd::<AnalyzeInstructionOriginalResult>(
            &cli_path,
            &model,
            provider_dir.as_deref(),
            &schema.to_string(),
            &prompt,
            None,
            INSTRUCTION_LLM_TIMEOUT_SECS,
            "分析拆解提示词",
        )
        .await?;
    // 产品层后处理：混写/重叠段落强制只留在 adapted，避免公共与适配双写。
    Ok(normalize_instruction_analyze_parts(
        &result.common,
        &result.adapted,
        &result.exclusive,
    ))
}

/// Business Logic: 适配页把当前 agent 适配正文改写为其他 agent 变体（仅草稿）。
/// Code Logic: deviceId 非空则对端 HeadlessCompletion；本机仍直跑 CLI。
#[tauri::command]
pub async fn agent_hub_adapt_instruction_to_other_agents(
    state: State<'_, AppState>,
    request: AdaptInstructionToOtherAgentsRequest,
    device_id: Option<String>,
) -> Result<AdaptInstructionToOtherAgentsResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        if device_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|id| !id.is_empty())
        {
            return proxy_agent_hub!(state, |client| client
                .agent_hub_adapt_instruction_to_other_agents(request, device_id.clone()));
        }
        return adapt_instruction_to_other_agents_for_state(state.inner(), request).await;
    }
    crate::agent_hub::remote_client::adapt_instruction_to_other_agents_for_device(
        state.inner(),
        device_id.as_deref(),
        request.clone(),
        adapt_instruction_to_other_agents_for_state(state.inner(), request),
    )
    .await
}

/// Business Logic: owner / P2P 路由共用本机适配改写。
/// Code Logic: 校验后跑 Claude structured JSON → variants。
pub(crate) async fn adapt_instruction_to_other_agents_for_state(
    state: &AppState,
    request: AdaptInstructionToOtherAgentsRequest,
) -> Result<AdaptInstructionToOtherAgentsResult, AppError> {
    let source = normalize_agent_target_token(&request.source_agent)?;
    let body = request.adapted_markdown.trim();
    if body.is_empty() {
        return Err(AppError::validation("INSTRUCTION_ADAPT_EMPTY_SOURCE"));
    }
    if body.chars().count() > MAX_INSTRUCTION_LLM_CHARS {
        return Err(AppError::validation("INSTRUCTION_ADAPT_SOURCE_TOO_LARGE"));
    }
    let destinations: Vec<&'static str> = ["claude", "codex", "opencode"]
        .into_iter()
        .filter(|target| *target != source.as_str())
        .collect();
    let (cli_path, model, provider_id) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
            cfg.internal_claude.provider_id.clone(),
        )
    };
    let provider_dir =
        crate::internal_claude::resolve_internal_provider_config_dir(provider_id.as_deref())
            .await?;
    let mut properties = serde_json::Map::new();
    for dest in &destinations {
        properties.insert((*dest).to_string(), serde_json::json!({ "type": "string" }));
    }
    let schema = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": destinations,
        "properties": properties
    });
    let dest_list = destinations.join(", ");
    // Business Logic: 适配正文通常是「语义+表面混写」整段落；改写只换表面，职责/分级不变。
    // Code Logic: 按 destination 输出 markdown 变体；无法安全映射的表面用语保留语义或留空，不编造模型 id。
    let prompt = format!(
        "You rewrite one agent's adapted instruction body for other coding agents.\n\
         Source agent: {source}\n\
         Destination agents: {dest_list}\n\
         Return ONLY JSON whose keys are exactly the destination agents.\n\
         \n\
         Context:\n\
         - Source text is usually MIXED: semantic intent (roles, tiers, process) woven with surface terms\n\
           (CLI names, paths, tool names, concrete model ids). Keep each mixed passage as ONE unit.\n\
         - Do NOT drop the semantic half when rewriting surface. Do NOT invent a separate pure-semantic rewrite.\n\
         \n\
         Rewrite rules:\n\
         1) Preserve intent, structure, language, headings, and list/table shape.\n\
         2) Subagent duties and tier ranks stay the SAME across agents; only rewrite surface wording.\n\
            Example: \"complex planning uses highest tier / routine uses mid / explore uses low\" stays;\n\
            replace concrete model ids with the destination agent's equivalent if known, otherwise keep the\n\
            tier/duty wording and avoid inventing fake model product names.\n\
         3) Map isomorphic surfaces when clear:\n\
            - Instruction files: CLAUDE.md ↔ AGENTS.md / AGENTS.override.md\n\
            - Config roots: ~/.claude ↔ ~/.codex ↔ ~/.config/opencode (and related env vars)\n\
            - Product/CLI/tool names for the same role\n\
         4) If a term has no safe mapping for a destination, keep the surrounding semantic rule and omit or\n\
            neutralize only the unmappable surface phrase — do not fabricate capabilities or model ids.\n\
         5) Empty string is allowed for a destination when nothing applies after rewrite.\n\
         \n\
         Source adapted markdown:\n---\n{body}\n---"
    );
    let raw = crate::claude_cli::run_structured_json_with_cwd::<BTreeMap<String, String>>(
        &cli_path,
        &model,
        provider_dir.as_deref(),
        &schema.to_string(),
        &prompt,
        None,
        INSTRUCTION_LLM_TIMEOUT_SECS,
        "适配提示词到其他 Agent",
    )
    .await?;
    let mut variants = BTreeMap::new();
    for dest in destinations {
        if let Some(text) = raw.get(dest) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                variants.insert(dest.to_string(), trimmed.to_string());
            }
        }
    }
    Ok(AdaptInstructionToOtherAgentsResult { variants })
}

/// Business Logic: 按用户方向改写当前 lane 槽（公共/独有单槽，适配覆盖三端）。
/// Code Logic: deviceId 非空则对端 HeadlessCompletion；本机仍直跑 CLI。
#[tauri::command]
pub async fn agent_hub_revise_instruction_slot(
    state: State<'_, AppState>,
    request: ReviseInstructionSlotRequest,
    device_id: Option<String>,
) -> Result<ReviseInstructionSlotResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        if device_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|id| !id.is_empty())
        {
            return proxy_agent_hub!(state, |client| client
                .agent_hub_revise_instruction_slot(request, device_id.clone()));
        }
        return revise_instruction_slot_for_state(state.inner(), request).await;
    }
    crate::agent_hub::remote_client::revise_instruction_slot_for_device(
        state.inner(),
        device_id.as_deref(),
        request.clone(),
        revise_instruction_slot_for_state(state.inner(), request),
    )
    .await
}

/// Business Logic: owner / P2P 路由共用本机槽改写。
/// Code Logic: prepare → Claude structured JSON。
pub(crate) async fn revise_instruction_slot_for_state(
    state: &AppState,
    request: ReviseInstructionSlotRequest,
) -> Result<ReviseInstructionSlotResult, AppError> {
    let prepared = prepare_revise_instruction_slot(&request)?;
    let (cli_path, model, provider_id) = {
        let cfg = state.config.read().unwrap();
        (
            cfg.github_trending.claude_cli_path.clone(),
            cfg.github_trending.claude_model.clone(),
            cfg.internal_claude.provider_id.clone(),
        )
    };
    let provider_dir =
        crate::internal_claude::resolve_internal_provider_config_dir(provider_id.as_deref())
            .await?;
    match prepared.lane {
        PreparedReviseLane::Common { current } => {
            let schema = serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            });
            let prompt = format!(
                "You revise the SHARED (cross-agent) instruction slot for Agent Hub.\n\
                 Current viewing agent: {agent}\n\
                 Do NOT add agent-specific CLI names, config paths, instruction-file names, or model ids.\n\
                 Follow the user's revision direction. Keep the original language. Do not invent facts.\n\
                 Return ONLY JSON with key text.\n\
                 Empty string is allowed if the direction asks to remove everything.\n\
                 \n\
                 Current shared markdown:\n---\n{current}\n---\n\
                 User revision direction:\n---\n{direction}\n---",
                agent = prepared.agent,
                current = current,
                direction = prepared.direction,
            );
            let raw = crate::claude_cli::run_structured_json_with_cwd::<BTreeMap<String, String>>(
                &cli_path,
                &model,
                provider_dir.as_deref(),
                &schema.to_string(),
                &prompt,
                None,
                INSTRUCTION_LLM_TIMEOUT_SECS,
                "AI 辅助修改公共提示词",
            )
            .await?;
            Ok(ReviseInstructionSlotResult {
                common: Some(raw.get("text").cloned().unwrap_or_default()),
                exclusive: None,
                variants: None,
            })
        }
        PreparedReviseLane::Exclusive { current } => {
            let schema = serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["text"],
                "properties": { "text": { "type": "string" } }
            });
            let prompt = format!(
                "You revise the EXCLUSIVE (target-only) instruction slot for one coding agent.\n\
                 Target agent: {agent}\n\
                 Only include content unique to this agent (no isomorphic rewrite for others).\n\
                 Follow the user's revision direction. Keep the original language. Do not invent facts.\n\
                 Return ONLY JSON with key text.\n\
                 Empty string is allowed if the direction asks to remove everything.\n\
                 \n\
                 Current exclusive markdown:\n---\n{current}\n---\n\
                 User revision direction:\n---\n{direction}\n---",
                agent = prepared.agent,
                current = current,
                direction = prepared.direction,
            );
            let raw = crate::claude_cli::run_structured_json_with_cwd::<BTreeMap<String, String>>(
                &cli_path,
                &model,
                provider_dir.as_deref(),
                &schema.to_string(),
                &prompt,
                None,
                INSTRUCTION_LLM_TIMEOUT_SECS,
                "AI 辅助修改独有提示词",
            )
            .await?;
            Ok(ReviseInstructionSlotResult {
                common: None,
                exclusive: Some(raw.get("text").cloned().unwrap_or_default()),
                variants: None,
            })
        }
        PreparedReviseLane::Adapted { variants } => {
            let schema = serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["claude", "codex", "opencode"],
                "properties": {
                    "claude": { "type": "string" },
                    "codex": { "type": "string" },
                    "opencode": { "type": "string" }
                }
            });
            let claude = variants.get("claude").cloned().unwrap_or_default();
            let codex = variants.get("codex").cloned().unwrap_or_default();
            let opencode = variants.get("opencode").cloned().unwrap_or_default();
            let prompt = format!(
                "You revise ALL agents' ADAPTED instruction variants in one pass.\n\
                 Agents: claude, codex, opencode\n\
                 Current viewing agent: {agent}\n\
                 Follow the user's revision direction for every agent.\n\
                 Keep isomorphic intent across agents; only swap surface wording\n\
                 (CLI names, config roots, instruction files, concrete model ids).\n\
                 Generate a variant even if that agent's current text is empty.\n\
                 Do not move exclusive-only capabilities into adapted.\n\
                 Keep the original language. Do not invent facts or fake model product names.\n\
                 Return ONLY JSON whose keys are exactly claude, codex, opencode.\n\
                 Empty string is allowed for an agent when nothing remains after revision.\n\
                 \n\
                 Current adapted variants:\n\
                 claude:\n---\n{claude}\n---\n\
                 codex:\n---\n{codex}\n---\n\
                 opencode:\n---\n{opencode}\n---\n\
                 User revision direction:\n---\n{direction}\n---",
                agent = prepared.agent,
                claude = claude,
                codex = codex,
                opencode = opencode,
                direction = prepared.direction,
            );
            let raw = crate::claude_cli::run_structured_json_with_cwd::<BTreeMap<String, String>>(
                &cli_path,
                &model,
                provider_dir.as_deref(),
                &schema.to_string(),
                &prompt,
                None,
                INSTRUCTION_LLM_TIMEOUT_SECS,
                "AI 辅助修改适配提示词",
            )
            .await?;
            let mut out = BTreeMap::new();
            for key in ["claude", "codex", "opencode"] {
                out.insert(key.to_string(), raw.get(key).cloned().unwrap_or_default());
            }
            Ok(ReviseInstructionSlotResult {
                common: None,
                exclusive: None,
                variants: Some(out),
            })
        }
    }
}

fn normalize_agent_target_token(value: &str) -> Result<String, AppError> {
    match value.trim() {
        "claude" | "codex" | "opencode" => Ok(value.trim().to_string()),
        _ => Err(AppError::validation("CROSS_AGENT_TARGET_INVALID")),
    }
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
    let preview_token = compute_push_preview_token(
        &request,
        &built.selection_hash,
        &built.envelope.snapshot_hash,
    )?;
    Ok(serde_json::json!({
        "snapshotHash": built.envelope.snapshot_hash,
        "snapshotId": built.envelope.snapshot_id,
        "selectionHash": built.selection_hash,
        "previewToken": preview_token,
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

    /// Business Logic: 混写段落只能进适配；清洗版 common 不得与 adapted 双写。
    /// Code Logic: normalize_instruction_analyze_parts 删除被 adapted 覆盖的 common。
    #[test]
    fn normalize_drops_common_overlap_with_adapted() {
        let common = "Always use TypeScript for new modules.";
        let adapted = "Always use TypeScript for new modules.\nPrefer Claude Code with Sonnet.";
        let out = normalize_instruction_analyze_parts(common, adapted, "");
        assert!(out.common.is_empty(), "common={}", out.common);
        assert!(out.adapted.contains("TypeScript"));
        assert!(out.adapted.contains("Sonnet"));
    }

    /// Business Logic: 含表面词的段落整段进适配，不留 hollow common。
    #[test]
    fn normalize_moves_surface_bearing_common_into_adapted() {
        let common = "Put project rules in CLAUDE.md and load ~/.claude settings.";
        let adapted = "Use Sonnet for implementation.";
        let out = normalize_instruction_analyze_parts(common, adapted, "");
        assert!(out.common.is_empty(), "common={}", out.common);
        assert!(
            out.adapted.contains("CLAUDE.md")
                || out.adapted.contains("claude.md")
                || out.adapted.to_ascii_lowercase().contains("claude.md")
                || out.adapted.contains("CLAUDE.md")
        );
        // 原 common 并入 adapted
        assert!(
            out.adapted.to_ascii_lowercase().contains("claude.md")
                || out.adapted.contains("CLAUDE.md")
        );
        assert!(
            out.adapted.contains("Sonnet")
                || out.adapted.contains("sonnet")
                || out.adapted.contains("implementation")
        );
    }

    fn revise_request(lane: &str, direction: &str, agent: &str) -> ReviseInstructionSlotRequest {
        ReviseInstructionSlotRequest {
            lane: lane.to_string(),
            agent: agent.to_string(),
            direction: direction.to_string(),
            common_markdown: None,
            exclusive_markdown: None,
            adapted_variants: None,
        }
    }

    #[test]
    fn prepare_revise_rejects_empty_direction() {
        let err = prepare_revise_instruction_slot(&revise_request("common", "   ", "claude"))
            .unwrap_err();
        assert_eq!(err.code(), "INSTRUCTION_REVISE_EMPTY_DIRECTION");
    }

    #[test]
    fn prepare_revise_rejects_invalid_lane() {
        let err = prepare_revise_instruction_slot(&revise_request("preview", "shorter", "claude"))
            .unwrap_err();
        assert_eq!(err.code(), "INSTRUCTION_REVISE_LANE_INVALID");
    }

    #[test]
    fn prepare_revise_rejects_invalid_agent() {
        let err = prepare_revise_instruction_slot(&revise_request("common", "shorter", "gemini"))
            .unwrap_err();
        assert_eq!(err.code(), "CROSS_AGENT_TARGET_INVALID");
    }

    #[test]
    fn prepare_revise_rejects_oversized_input() {
        let huge = "汉".repeat(MAX_INSTRUCTION_LLM_CHARS);
        let mut request = revise_request("common", "再改短一点", "claude");
        request.common_markdown = Some(huge);
        let err = prepare_revise_instruction_slot(&request).unwrap_err();
        assert_eq!(err.code(), "INSTRUCTION_REVISE_INPUT_TOO_LARGE");
    }

    #[test]
    fn prepare_revise_accepts_empty_common_slot() {
        let prepared = prepare_revise_instruction_slot(&revise_request(
            "common",
            "写成更短的验收标准",
            "codex",
        ))
        .unwrap();
        assert_eq!(prepared.agent, "codex");
        assert!(matches!(prepared.lane, PreparedReviseLane::Common { .. }));
    }

    /// Business Logic: 纯语义公共段落在适配无重叠时应保留。
    #[test]
    fn normalize_keeps_pure_common_when_not_overlapping() {
        let common = "All pull requests need two reviewers.";
        let adapted = "Map Claude model Sonnet to implementation work.";
        let out = normalize_instruction_analyze_parts(common, adapted, "");
        assert!(out.common.contains("two reviewers"));
        assert!(out.adapted.contains("Sonnet") || out.adapted.contains("implementation"));
    }

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
        assert!(src.contains("proxy_agent_hub!"));
        let direct_reload = concat!("BackendControlClient", "::from_control_file()");
        assert!(!src.contains(direct_reload));
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
        // HeadlessOwner → for_state（本机或 P2P）；GuiClient → control client
        assert!(src.contains("inspect_portable_inventory_for_state"));
        assert!(src.contains("preview_portable_asset_action_for_state"));
        assert!(src.contains("apply_portable_asset_action_for_state"));
        assert!(src.contains("get_portable_asset_action_for_state"));
        assert!(src.contains("PortableService::list_remote_portable_inventory"));
        assert!(src.contains("PortableService::preview_portable_pull"));
        assert!(src.contains("PortableService::apply_portable_pull"));
        assert!(src.contains("PortableService::get_portable_pull"));
        assert!(src.contains(".agent_hub_inspect_portable_inventory(query, device_id.clone())"));
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
            "agent_hub.read_user_native_instruction_file",
            "agent_hub.write_user_native_instruction_file",
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
    fn agent_hub_api_version_is_v4_for_scan_only_write_policy() {
        assert_eq!(AGENT_HUB_API_VERSION, 4);
    }
}
