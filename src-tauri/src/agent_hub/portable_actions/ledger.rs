//! portable_actions/ledger — 本机动作 plan claim / 幂等查询
//!
//! Business Logic（为什么需要这个模块）:
//!     Apply 必须原子 claim plan；同 clientRequestId 完成则回放精确结果，
//!     claim 后未完成返回 outcomeUnknown，禁止盲目重放。
//!
//! Code Logic（这个模块做什么）:
//!     封装 AgentHubRepo claim/get/complete；构造 outcomeUnknown 结果。

use super::models::{
    PortableAssetActionItemResultDto, PortableAssetActionItemState, PortableAssetActionResultDto,
    StoredPortableAssetActionPlan,
};
use crate::agent_hub::models::{PortableActionClaim, PortableAssetActionPlanRecord};
use crate::error::AppError;
use crate::storage::agent_hub_repo::AgentHubRepo;

/// 原子 claim 一个 portable action plan。
///
/// Business Logic（为什么需要这个函数）:
///     同一 planToken 同时只能有一个执行者；同 id 重试返回 pending/replay。
///
/// Code Logic（这个函数做什么）:
///     委托 `AgentHubRepo::claim_portable_asset_action_plan`。
pub async fn claim_portable_asset_action(
    repo: &AgentHubRepo,
    plan_token: &str,
    client_request_id: &str,
) -> Result<PortableActionClaim, AppError> {
    if plan_token.trim().is_empty() || client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_CLAIM_ID_REQUIRED",
        ));
    }
    repo.claim_portable_asset_action_plan(plan_token, client_request_id)
        .await
}

/// 按 clientRequestId 查询动作结果（优先于 token）。
///
/// Business Logic（为什么需要这个函数）:
///     outcomeUnknown 对账必须能用 clientRequestId 找回 ledger。
///
/// Code Logic（这个函数做什么）:
///     查 request ledger：完成则反序列化 result；claimed 未完成 → outcomeUnknown；
///     缺失 → not_found。
pub async fn get_portable_asset_action_by_request(
    repo: &AgentHubRepo,
    client_request_id: &str,
) -> Result<PortableAssetActionResultDto, AppError> {
    if client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_REQUEST_ID_REQUIRED",
        ));
    }
    let Some(row) = repo
        .get_portable_asset_action_by_request_id(client_request_id)
        .await?
    else {
        return Err(AppError::not_found(
            "PORTABLE_ASSET_ACTION_REQUEST_NOT_FOUND",
        ));
    };
    if let Some(result_json) = row.result_json.as_deref() {
        return serde_json::from_str(result_json).map_err(AppError::from);
    }
    // claimed but not completed
    let plan_token = row.plan_token;
    let stored = parse_stored_plan(&row.plan_json)?;
    Ok(outcome_unknown_result(
        &plan_token,
        client_request_id,
        &stored.public,
    ))
}

/// 从 claim Pending 构造 outcomeUnknown 结果。
///
/// Business Logic（为什么需要这个函数）:
///     未完成的 claim 不得伪装 succeeded，也不得自动重放 mutation。
///
/// Code Logic（这个函数做什么）:
///     从 plan changes 生成逐项 outcomeUnknown。
pub fn outcome_unknown_result(
    plan_token: &str,
    client_request_id: &str,
    plan: &super::models::PortableAssetActionPlanDto,
) -> PortableAssetActionResultDto {
    PortableAssetActionResultDto {
        plan_token: plan_token.to_string(),
        client_request_id: client_request_id.to_string(),
        items: plan
            .changes
            .iter()
            .map(|change| PortableAssetActionItemResultDto {
                inventory_item_id: change.inventory_item_id.clone(),
                state: PortableAssetActionItemState::OutcomeUnknown,
                error_code: Some("PORTABLE_ASSET_ACTION_OUTCOME_UNKNOWN".into()),
                message: Some("action claimed but not completed".into()),
            })
            .collect(),
    }
}

/// 解析持久化 plan JSON。
pub(crate) fn parse_stored_plan(
    plan_json: &str,
) -> Result<StoredPortableAssetActionPlan, AppError> {
    serde_json::from_str(plan_json).map_err(AppError::from)
}

/// 完成 claim 并写入结果（B4 使用）。
pub async fn complete_portable_asset_action(
    repo: &AgentHubRepo,
    plan_token: &str,
    client_request_id: &str,
    result: &PortableAssetActionResultDto,
) -> Result<(), AppError> {
    let result_json = serde_json::to_string(result)?;
    repo.complete_portable_asset_action_plan(plan_token, client_request_id, &result_json)
        .await
}

/// 读取 plan 记录。
pub async fn get_portable_asset_action_plan(
    repo: &AgentHubRepo,
    plan_token: &str,
) -> Result<Option<PortableAssetActionPlanRecord>, AppError> {
    repo.get_portable_asset_action_plan(plan_token).await
}
