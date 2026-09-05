//! agent_hub/replication/pull/remote_project — 远端项目代理与账本对账
//!
//! Business Logic（为什么需要这个模块）:
//!     本机 GUI 需要操作「另一台设备上的项目级 portable 资产」：项目 opt-in 预览/启用、
//!     inventory 浏览、action preview/apply/get，以及未完成 Pull 的 OutcomeUnknown 对账
//!     （Pending 不得假死，也要 best-effort 附加本机可观察事实）。
//!
//! Code Logic（这个模块做什么）:
//!     resolve_remote_portable_project 解析 remote shortcut 并要求对端携带
//!     portable-project v1 capability；各代理函数经 post_json_bound 转发到 owning peer；
//!     parse_stored_pull_plan / outcome_unknown_pull_result / fold_pending_pull_observations
//!     处理持久化 plan 与 Pending 观察；build_source_selection_for_items 为源端供数构建 selection。

use super::dto::{
    ApplyRemoteProjectPortableActionRequest, GetRemoteProjectPortableActionRequest,
    InspectRemoteProjectPortableInventoryRequest, PortablePullChangeDto, PortablePullItemResultDto,
    PortablePullItemState, PortablePullPlanDto, PortablePullResultDto,
    PreviewRemoteProjectPortableActionRequest, RemoteProjectPortableInventoryQuery,
    RemoteProjectRefRequest,
};
use super::install_target::{inventory_has_scoped_item, portable_pull_inventory_query};
use super::staging::StoredPortablePullPlan;
use super::{peer_err, post_json_bound};
use crate::agent_hub::models::{AgentTarget, ScopeKind};
use crate::agent_hub::object_store::ObjectStore;
use crate::agent_hub::portable_actions::{
    PortableAssetActionPlanDto, PortableAssetActionResultDto,
};
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_query, PortableInventorySnapshotDto,
};
use crate::agent_hub::project_scope::{AgentHubProjectPreview, AgentHubProjectStatus};
use crate::agent_hub::snapshot::portable_builder::{
    build_portable_selection_envelope, BuiltPortableSelection,
};
use crate::error::AppError;
use crate::net::peer_client::PeerClient;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::protocol::CAPABILITY_PORTABLE_PROJECT_V1;
use crate::orchestrator::outbox::open_remote_project_for_shortcut;
use crate::state::AppState;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn parse_stored_pull_plan(plan_json: &str) -> Result<StoredPortablePullPlan, AppError> {
    serde_json::from_str(plan_json).map_err(AppError::from)
}

pub(super) fn outcome_unknown_pull_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &PortablePullPlanDto,
) -> PortablePullResultDto {
    PortablePullResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        source_device_id: plan.source_device_id.clone(),
        source_target: plan.source_target,
        destination_target: plan.destination_target,
        partial: true,
        items: plan
            .changes
            .iter()
            .map(|c| PortablePullItemResultDto {
                inventory_item_id: c.inventory_item_id.clone(),
                state: PortablePullItemState::OutcomeUnknown,
                install_mode: Some(c.install_mode),
                error_code: Some("PORTABLE_PULL_OUTCOME_UNKNOWN".into()),
                message: Some("pull claimed but not completed".into()),
            })
            .collect(),
    }
}

/// Best-effort fold of local inventory observations into an OutcomeUnknown pull result.
///
/// Business Logic: Pending/incomplete apply must still surface destination facts when
/// rescan can observe planned native ids (mirror portable_actions Pending rescan).
/// Code Logic: mutates item.message only; never upgrades OutcomeUnknown → Succeeded.
pub(super) fn fold_pending_pull_observations(
    result: &mut PortablePullResultDto,
    plan: &PortablePullPlanDto,
    post: &PortableInventorySnapshotDto,
) {
    let by_id: BTreeMap<&str, &PortablePullChangeDto> = plan
        .changes
        .iter()
        .map(|c| (c.inventory_item_id.as_str(), c))
        .collect();
    for item in &mut result.items {
        let Some(change) = by_id.get(item.inventory_item_id.as_str()) else {
            continue;
        };
        let observed = inventory_has_scoped_item(
            post,
            plan.destination_target,
            change.kind,
            &change.native_id,
            &change.scope_id,
        );
        item.message = Some(format!(
            "pull claimed but not completed; observed presence={observed}"
        ));
    }
}

pub(super) async fn build_source_selection_for_items(
    state: &AppState,
    source_target: AgentTarget,
    source_local_project_id: Option<String>,
    item_ids: &[String],
) -> Result<BuiltPortableSelection, AppError> {
    let snap = inspect_portable_inventory_query(
        state,
        portable_pull_inventory_query(source_target, source_local_project_id),
    )
    .await?;
    let wanted: BTreeSet<&str> = item_ids.iter().map(String::as_str).collect();
    let selected: Vec<_> = snap
        .items
        .into_iter()
        .filter(|i| i.target == source_target && wanted.contains(i.inventory_item_id.as_str()))
        .collect();
    if selected.is_empty() {
        return Err(AppError::validation(
            "PORTABLE_PULL_SELECTION_EMPTY".to_string(),
        ));
    }
    let data_dir = crate::config::data_dir()?;
    let store = ObjectStore::open(&data_dir)?;
    build_portable_selection_envelope(&store, state.device_id.as_str(), source_target, &selected)
        .await
}

// ───────────────────────── 本机 list / preview / apply / get ─────────────────────────

/// 解析 remote shortcut 并确认对端支持精确项目协议。
pub(super) async fn resolve_remote_portable_project(
    state: &AppState,
    project_ref: &str,
) -> Result<crate::orchestrator::outbox::RemoteOrchestratorProjectContext, AppError> {
    let shortcut = state
        .workbench_project_repo
        .get(project_ref)
        .await?
        .ok_or_else(|| AppError::not_found("AGENT_HUB_REMOTE_PROJECT_NOT_FOUND"))?;
    let context = open_remote_project_for_shortcut(state, &shortcut, None).await?;
    PeerClient::new()
        .require_capability(&context.base_url, CAPABILITY_PORTABLE_PROJECT_V1)
        .await
        .map_err(peer_err)?;
    Ok(context)
}

/// 预览 owning peer 上的项目 opt-in。
pub async fn preview_remote_project(
    state: &AppState,
    request: RemoteProjectRefRequest,
) -> Result<AgentHubProjectPreview, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/preview",
        &serde_json::json!({ "localProjectId": context.remote_project_id }),
        &context.device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

/// 在 owning peer 显式启用项目 Agent Hub scope。
pub async fn enable_remote_project(
    state: &AppState,
    request: RemoteProjectRefRequest,
) -> Result<AgentHubProjectStatus, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/enable",
        &serde_json::json!({ "localProjectId": context.remote_project_id }),
        &context.device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(peer_err)
}

/// 读取远端项目的完整 portable inventory。
pub async fn inspect_remote_project_portable_inventory(
    state: &AppState,
    request: InspectRemoteProjectPortableInventoryRequest,
) -> Result<PortableInventorySnapshotDto, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/inventory",
        &RemoteProjectPortableInventoryQuery {
            local_project_id: context.remote_project_id,
            target: request.target,
            kind: request.kind,
        },
        &context.device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}

/// 在 owning peer 生成项目资产 action plan。
pub async fn preview_remote_project_portable_action(
    state: &AppState,
    mut request: PreviewRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionPlanDto, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    request.request.inventory_query.local_project_id = Some(context.remote_project_id);
    request.request.inventory_query.scope_kind = Some(ScopeKind::Project);
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/action/preview",
        &request.request,
        &context.device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(peer_err)
}

/// 在 owning peer claim/apply 项目资产 action plan。
pub async fn apply_remote_project_portable_action(
    state: &AppState,
    request: ApplyRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/action/apply",
        &request.request,
        &context.device_id,
        PeerTimeoutClass::Mutation,
    )
    .await
    .map_err(peer_err)
}

/// 在 owning peer 查询远端项目 action 结果。
pub async fn get_remote_project_portable_action(
    state: &AppState,
    request: GetRemoteProjectPortableActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    let context = resolve_remote_portable_project(state, &request.project_ref).await?;
    post_json_bound(
        &PeerClient::new(),
        &context.base_url,
        "/api/agent-hub/portable/project/action/get",
        &serde_json::json!({ "clientRequestId": request.client_request_id }),
        &context.device_id,
        PeerTimeoutClass::Metadata,
    )
    .await
    .map_err(peer_err)
}
