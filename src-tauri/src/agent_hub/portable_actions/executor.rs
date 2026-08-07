//! portable_actions/executor — claim plan → target action → rescan → complete ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     Apply 必须原子 claim、按 target adapter 执行、rescan 后仅在 observed 匹配 expected
//!     时标 succeeded；spawn/transport 不确定必须 outcomeUnknown；部分成功按项返回。
//!
//! Code Logic（这个模块做什么）:
//!     `apply_portable_asset_action` / `apply_portable_asset_action_with`；依赖 B2/B3 与 targets。

use super::ledger::{
    claim_portable_asset_action, complete_portable_asset_action, parse_stored_plan,
};
use super::models::{
    ApplyPortableAssetActionRequest, PortableAssetActionItemResultDto,
    PortableAssetActionItemState, PortableAssetActionKind, PortableAssetActionResultDto,
    PortableAssetPlanOperation, StoredPortableAssetActionPlan,
};
use super::targets::{
    executor_for, expected_enabled_after, TargetActionContext, TargetActionRawOutcome,
};
use crate::agent_hub::models::PortableActionClaim;
use crate::agent_hub::packages::activator::ProcessRunner;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_with_env, PortableInventoryItemDto, PortableInventorySnapshotDto,
};
use crate::agent_hub::targets::paths::TargetEnvironment;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// 执行器依赖（可注入 runner / env / 路径，便于 FakeProcessRunner 测试）。
///
/// Business Logic（为什么需要这个结构体）:
///     生产路径用真实 CLI；单测必须记录 argv 且隔离 CLAUDE_CONFIG_DIR。
///
/// Code Logic（这个结构体做什么）:
///     持有 repo、runner、可选 TargetEnvironment 与 config/data 根。
pub struct PortableActionExecutorDeps {
    /// Agent Hub repo
    pub repo: AgentHubRepo,
    /// CLI runner
    pub runner: Arc<dyn ProcessRunner>,
    /// 可选注入扫描环境（None 时 apply 对 AppState 走真实 inspect）
    pub env: Option<TargetEnvironment>,
    /// 预置 inventory（测试/服务层可注入；None 则 rescan 时 inspect）
    pub pre_inventory: Option<PortableInventorySnapshotDto>,
    /// Claude 配置根
    pub claude_config_dir: Option<PathBuf>,
    /// 数据根（disabled/backup）
    pub data_dir: Option<PathBuf>,
    /// rescan 覆盖（测试可在 mutation 后改写 observed）
    pub rescan_override: Option<PortableInventorySnapshotDto>,
}

impl PortableActionExecutorDeps {
    /// 从 AppState 构造生产依赖。
    ///
    /// 注：B4 允许 FakeProcessRunner 缝；生产 CLI 通过 `RealProcessRunner`。
    pub fn from_state(state: &AppState) -> Self {
        Self {
            repo: AgentHubRepo::new(state.agent_hub_repo.pool().clone()),
            runner: Arc::new(RealProcessRunner),
            env: None,
            pre_inventory: None,
            claude_config_dir: None,
            data_dir: None,
            rescan_override: None,
        }
    }
}

/// 真实进程 runner（生产）。
struct RealProcessRunner;

impl ProcessRunner for RealProcessRunner {
    fn run(
        &self,
        spec: &crate::agent_hub::packages::activator::ProcessSpec,
    ) -> Result<crate::agent_hub::packages::activator::ProcessOutcome, AppError> {
        let output = std::process::Command::new(&spec.program)
            .args(&spec.args)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.to_string().contains("No such file")
                {
                    AppError::unavailable(format!("spawn failed: {e}"))
                } else {
                    AppError::generic(format!("process error: {e}"))
                }
            })?;
        Ok(crate::agent_hub::packages::activator::ProcessOutcome {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Apply portable 资产动作（生产入口）。
///
/// Business Logic（为什么需要这个函数）:
///     UI/IPC 只提交 planToken + clientRequestId；执行后必须 rescan 对账。
///
/// Code Logic（这个函数做什么）:
///     委托 `apply_portable_asset_action_with(Some(state), from_state)`。
pub async fn apply_portable_asset_action(
    state: &AppState,
    request: ApplyPortableAssetActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    let deps = PortableActionExecutorDeps::from_state(state);
    apply_portable_asset_action_with(Some(state), &deps, request).await
}

/// 可注入依赖的 apply（单测主入口）。
///
/// Business Logic（为什么需要这个函数）:
///     claim → execute → rescan → complete；同 request replay；pending → outcomeUnknown。
///
/// Code Logic（这个函数做什么）:
///     解析 plan、按项执行 target adapter、对比 expected/observed、写 ledger。
///     `state` 仅在未注入 pre/rescan inventory 时用于 inspect；单测可传 None。
pub async fn apply_portable_asset_action_with(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    request: ApplyPortableAssetActionRequest,
) -> Result<PortableAssetActionResultDto, AppError> {
    if request.plan_token.trim().is_empty() || request.client_request_id.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_APPLY_IDS_REQUIRED",
        ));
    }

    let claim =
        claim_portable_asset_action(&deps.repo, &request.plan_token, &request.client_request_id)
            .await?;

    match claim {
        PortableActionClaim::Replay(json) => {
            serde_json::from_str(&json).map_err(AppError::from)
        }
        PortableActionClaim::Pending => {
            // 未完成 claim：诚实 outcomeUnknown，尽量 rescan 附加 observed 事实
            let row = deps
                .repo
                .get_portable_asset_action_plan(&request.plan_token)
                .await?
                .ok_or_else(|| AppError::not_found("PORTABLE_ASSET_ACTION_PLAN_NOT_FOUND"))?;
            let stored = parse_stored_plan(&row.plan_json)?;
            let mut result = super::ledger::outcome_unknown_result(
                &request.plan_token,
                &request.client_request_id,
                &stored.public,
            );
            if let Ok(post) = resolve_post_inventory(state, deps).await {
                let by_id: BTreeMap<_, _> = post
                    .items
                    .iter()
                    .map(|i| (i.inventory_item_id.clone(), i))
                    .collect();
                for item in &mut result.items {
                    if let Some(obs) = by_id.get(&item.inventory_item_id) {
                        item.message = Some(format!(
                            "action claimed but not completed; observed enabled={:?}",
                            obs.actual_enabled
                        ));
                    }
                }
            }
            Ok(result)
        }
        PortableActionClaim::Claimed(record) => {
            let stored = parse_stored_plan(&record.plan_json)?;
            // claim 后立即 revalidate：expiry / owner fingerprint / inventory hash / CLI fingerprints
            // record 为 Box 以压低 enum size；此处直接用 stored
            if let Err(block) = revalidate_claimed_plan(state, deps, &stored).await {
                let items = stored
                    .public
                    .changes
                    .iter()
                    .map(|c| PortableAssetActionItemResultDto {
                        inventory_item_id: c.inventory_item_id.clone(),
                        state: PortableAssetActionItemState::Failed,
                        error_code: Some(block.clone()),
                        message: Some("plan revalidation failed at apply".into()),
                    })
                    .collect();
                let result = PortableAssetActionResultDto {
                    plan_token: request.plan_token.clone(),
                    client_request_id: request.client_request_id.clone(),
                    items,
                };
                complete_portable_asset_action(
                    &deps.repo,
                    &request.plan_token,
                    &request.client_request_id,
                    &result,
                )
                .await?;
                return Ok(result);
            }
            let result = execute_claimed_plan(state, deps, &request, &stored).await?;
            complete_portable_asset_action(
                &deps.repo,
                &request.plan_token,
                &request.client_request_id,
                &result,
            )
            .await?;
            Ok(result)
        }
    }
}

/// claim 后、mutation 前 revalidate（expiry + owner + inventory/target fingerprints）。
async fn revalidate_claimed_plan(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    stored: &StoredPortableAssetActionPlan,
) -> Result<(), String> {
    use chrono::{DateTime, Utc};
    let plan = &stored.public;
    if let Ok(expires) = DateTime::parse_from_rfc3339(&plan.expires_at) {
        if expires < Utc::now() {
            return Err("PORTABLE_ASSET_ACTION_PLAN_EXPIRED".into());
        }
    } else if plan.expires_at.as_str() < Utc::now().to_rfc3339().as_str() {
        return Err("PORTABLE_ASSET_ACTION_PLAN_EXPIRED".into());
    }

    // 重新 inspect inventory 并比对 snapshot hash + target fingerprints
    let live = match resolve_pre_inventory(state, deps).await {
        Ok(s) => s,
        Err(e) => {
            return Err(format!(
                "PORTABLE_ASSET_ACTION_INVENTORY_REVALIDATE_FAILED:{e}"
            ))
        }
    };
    if live.inventory_snapshot_hash != plan.inventory_snapshot_hash {
        return Err("PORTABLE_ASSET_ACTION_INVENTORY_HASH_MISMATCH".into());
    }
    if live.stale {
        return Err("PORTABLE_ASSET_ACTION_INVENTORY_STALE".into());
    }
    let mut live_fps: BTreeMap<String, String> = BTreeMap::new();
    for t in &live.targets {
        live_fps.insert(
            t.target.as_str().to_string(),
            format!(
                "{}|{}|{}|{}",
                t.target.as_str(),
                t.version.as_deref().unwrap_or(""),
                t.executable.as_deref().unwrap_or(""),
                t.config_root
            ),
        );
    }
    for expected in &stored.target_fingerprints {
        let target = expected.split('|').next().unwrap_or("");
        match live_fps.get(target) {
            Some(actual) if actual == expected => {}
            _ => return Err("PORTABLE_ASSET_ACTION_TARGET_FINGERPRINT_MISMATCH".into()),
        }
    }
    // owner fingerprint：若可从 state 重算则比对
    if let Some(state) = state {
        let roots = live
            .targets
            .iter()
            .map(|t| format!("{}={}", t.target.as_str(), t.config_root))
            .collect::<Vec<_>>()
            .join("|");
        let current = crate::agent_hub::object_store::sha256_hex(
            format!("{}|{}", state.device_id.as_str(), roots).as_bytes(),
        );
        if !stored.owner_fingerprint.is_empty() && current != stored.owner_fingerprint {
            return Err("PORTABLE_ASSET_ACTION_OWNER_FINGERPRINT_MISMATCH".into());
        }
    }
    Ok(())
}

async fn execute_claimed_plan(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    request: &ApplyPortableAssetActionRequest,
    stored: &StoredPortableAssetActionPlan,
) -> Result<PortableAssetActionResultDto, AppError> {
    let plan = &stored.public;
    // 项级 blocking 走 change.blocking_reasons，允许 partial results。
    // 计划级全局阻断仅覆盖 inventory/target 指纹类原因。
    let global_block = plan.blocking_reasons.iter().any(|r| {
        r.starts_with("PORTABLE_ASSET_ACTION_INVENTORY_")
            || r.starts_with("PORTABLE_ASSET_ACTION_TARGET_MUTATION_BLOCKED")
            || r.starts_with("PORTABLE_ASSET_ACTION_CLI_MISSING")
            || r.starts_with("PORTABLE_ASSET_ACTION_TARGET_MISSING")
    });
    if global_block {
        let items = plan
            .changes
            .iter()
            .map(|c| PortableAssetActionItemResultDto {
                inventory_item_id: c.inventory_item_id.clone(),
                state: PortableAssetActionItemState::Blocked,
                error_code: Some(
                    plan.blocking_reasons
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "PORTABLE_ASSET_ACTION_BLOCKED".into()),
                ),
                message: Some("plan blocked".into()),
            })
            .collect();
        return Ok(PortableAssetActionResultDto {
            plan_token: plan.plan_token.clone(),
            client_request_id: request.client_request_id.clone(),
            items,
        });
    }

    let pre_snapshot = resolve_pre_inventory(state, deps).await?;
    let pre_by_id: BTreeMap<String, PortableInventoryItemDto> = pre_snapshot
        .items
        .iter()
        .cloned()
        .map(|i| (i.inventory_item_id.clone(), i))
        .collect();

    let ctx = TargetActionContext {
        runner: deps.runner.clone(),
        claude_config_dir: deps.claude_config_dir.clone(),
        data_dir: deps.data_dir.clone(),
        keep_data: plan.keep_data,
        action: plan.action,
    };

    let mut raw_results: Vec<(
        String,
        TargetActionRawOutcome,
        Option<PortableInventoryItemDto>,
    )> = Vec::with_capacity(plan.changes.len());

    for change in &plan.changes {
        let pre = pre_by_id.get(&change.inventory_item_id).cloned();
        // 逐项 blocking
        if !change.blocking_reasons.is_empty() {
            raw_results.push((
                change.inventory_item_id.clone(),
                TargetActionRawOutcome::Blocked {
                    code: change
                        .blocking_reasons
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "PORTABLE_ASSET_ACTION_BLOCKED".into()),
                    message: "item blocked".into(),
                },
                pre,
            ));
            continue;
        }

        // leave / noop
        if change.operation == PortableAssetPlanOperation::Leave {
            raw_results.push((
                change.inventory_item_id.clone(),
                TargetActionRawOutcome::Skipped,
                pre,
            ));
            continue;
        }

        // Plugin shared preserve：canonical_effect TombstoneComponents 时不删除 shared components
        // （具体 preserve 由 claude target + ownership 决策；此处保证不静默删）

        let exec = executor_for(change.target);
        let outcome = match exec.execute_change(&ctx, plan, change, pre.as_ref()) {
            Ok(o) => o,
            Err(e) => {
                if e.ipc_category_code() == "unavailable" || e.ipc_category_code() == "timeout" {
                    TargetActionRawOutcome::OutcomeUnknown {
                        code: "PORTABLE_ASSET_ACTION_SPAWN_UNKNOWN".into(),
                        message: e.to_string(),
                    }
                } else {
                    TargetActionRawOutcome::Failed {
                        code: "PORTABLE_ASSET_ACTION_EXECUTE_ERROR".into(),
                        message: e.to_string(),
                    }
                }
            }
        };
        raw_results.push((change.inventory_item_id.clone(), outcome, pre));
    }

    // rescan
    let post = resolve_post_inventory(state, deps).await?;
    let post_by_id: BTreeMap<String, &PortableInventoryItemDto> = post
        .items
        .iter()
        .map(|i| (i.inventory_item_id.clone(), i))
        .collect();

    let mut items = Vec::with_capacity(raw_results.len());
    for (item_id, raw, pre) in raw_results {
        let change = plan
            .changes
            .iter()
            .find(|c| c.inventory_item_id == item_id)
            .expect("change exists");
        let state = reconcile_item(
            plan.action,
            plan.keep_data,
            change.kind,
            &raw,
            pre.as_ref(),
            post_by_id.get(&item_id).copied(),
        );
        items.push(PortableAssetActionItemResultDto {
            inventory_item_id: item_id,
            state: state.0,
            error_code: state.1,
            message: state.2,
        });
    }

    Ok(PortableAssetActionResultDto {
        plan_token: plan.plan_token.clone(),
        client_request_id: request.client_request_id.clone(),
        items,
    })
}

fn reconcile_item(
    action: PortableAssetActionKind,
    keep_data: bool,
    kind: crate::agent_hub::portable_inventory::PortableAssetKind,
    raw: &TargetActionRawOutcome,
    pre: Option<&PortableInventoryItemDto>,
    post: Option<&PortableInventoryItemDto>,
) -> (PortableAssetActionItemState, Option<String>, Option<String>) {
    match raw {
        TargetActionRawOutcome::Blocked { code, message } => (
            PortableAssetActionItemState::Blocked,
            Some(code.clone()),
            Some(message.clone()),
        ),
        TargetActionRawOutcome::Failed { code, message } => (
            PortableAssetActionItemState::Failed,
            Some(code.clone()),
            Some(message.clone()),
        ),
        TargetActionRawOutcome::OutcomeUnknown { code, message } => (
            PortableAssetActionItemState::OutcomeUnknown,
            Some(code.clone()),
            Some(message.clone()),
        ),
        TargetActionRawOutcome::Skipped => (
            PortableAssetActionItemState::Skipped,
            None,
            Some("already satisfied".into()),
        ),
        TargetActionRawOutcome::Applied => {
            // rescan 对账
            match action {
                PortableAssetActionKind::Uninstall => {
                    if post.is_none() {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("uninstalled verified by rescan".into()),
                        )
                    } else if keep_data && post.and_then(|p| p.actual_enabled) == Some(false) {
                        // keep_data：允许 residual present+disabled
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("uninstalled/disabled verified (keep_data)".into()),
                        )
                    } else if !keep_data {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("item still present after full uninstall".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("item still present after uninstall".into()),
                        )
                    }
                }
                PortableAssetActionKind::Enable | PortableAssetActionKind::Disable => {
                    let expected =
                        expected_enabled_after(action, kind, pre.and_then(|p| p.actual_enabled));
                    let observed = post.and_then(|p| p.actual_enabled);
                    if expected.is_some() && observed == expected {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("rescan matches expected".into()),
                        )
                    } else if expected.is_none() {
                        // 无 enable 语义
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("applied without enable semantics".into()),
                        )
                    } else if post.is_none() {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING".into()),
                            Some("item missing after enable/disable".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some(format!(
                                "expected enabled={expected:?} observed={observed:?}"
                            )),
                        )
                    }
                }
                PortableAssetActionKind::Adopt => (
                    // 永不假成功：Adopt Applied 若到达这里说明 adapter 未 fail-closed
                    PortableAssetActionItemState::Failed,
                    Some("PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED".into()),
                    Some("adopt must not succeed without ownership write".into()),
                ),
                PortableAssetActionKind::InstallToSourceTarget => {
                    if post.is_some() {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("install verified".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING".into()),
                            Some("install not observed".into()),
                        )
                    }
                }
            }
        }
    }
}

async fn resolve_pre_inventory(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if let Some(snap) = &deps.pre_inventory {
        return Ok(snap.clone());
    }
    let state = state
        .ok_or_else(|| AppError::validation("PORTABLE_ASSET_ACTION_STATE_REQUIRED_FOR_INSPECT"))?;
    if let Some(env) = &deps.env {
        return inspect_portable_inventory_with_env(state, env).await;
    }
    crate::agent_hub::portable_inventory::inspect_portable_inventory(state).await
}

async fn resolve_post_inventory(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if let Some(snap) = &deps.rescan_override {
        return Ok(snap.clone());
    }
    let state = state
        .ok_or_else(|| AppError::validation("PORTABLE_ASSET_ACTION_STATE_REQUIRED_FOR_INSPECT"))?;
    if let Some(env) = &deps.env {
        return inspect_portable_inventory_with_env(state, env).await;
    }
    crate::agent_hub::portable_inventory::inspect_portable_inventory(state).await
}

/// 测试辅助：空 Fake runner 依赖。
#[cfg(test)]
pub fn test_deps_with_runner(
    repo: AgentHubRepo,
    runner: Arc<crate::agent_hub::packages::activator::FakeProcessRunner>,
) -> PortableActionExecutorDeps {
    PortableActionExecutorDeps {
        repo,
        runner,
        env: None,
        pre_inventory: None,
        claude_config_dir: None,
        data_dir: None,
        rescan_override: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, ScopeKind};
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    use crate::agent_hub::portable_actions::models::{
        PortableAssetActionKind, PortableAssetActionPlanDto, PortableAssetConflictPolicy,
        PreviewPortableAssetActionRequest,
    };
    use crate::agent_hub::portable_actions::planner::preview_portable_asset_action_with_inventory;
    use crate::agent_hub::portable_inventory::{
        hash_plugin_root, inventory_item_id, inventory_snapshot_hash, PortableAssetKind,
        PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
        PortableInventoryManagementState, PortableInventoryMutationCapability,
        PortableInventoryScanCapability, PortableInventorySourceOrigin, PortableInventoryTargetDto,
    };
    use crate::agent_hub::targets::portable::hash_skill_directory;
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::Arc;

    async fn test_repo() -> AgentHubRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        AgentHubRepo::ensure_schema(&pool).await.unwrap();
        AgentHubRepo::new(pool)
    }

    fn sample_target(target: AgentTarget) -> PortableInventoryTargetDto {
        PortableInventoryTargetDto {
            target,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some(format!("/bin/{}", target.as_str())),
            config_root: format!("/cfg/{}", target.as_str()),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        }
    }

    fn sample_item(
        target: AgentTarget,
        kind: PortableAssetKind,
        native_id: &str,
        path: &str,
        enabled: Option<bool>,
    ) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(target, "user", path, native_id),
            target,
            kind,
            native_id: native_id.into(),
            display_name: native_id.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(path.into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: enabled,
            content_hash: Some("content-hash".into()),
            tree_hash: Some("tree-hash".into()),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: true,
                can_disable: true,
                can_uninstall: true,
                can_adopt: true,
                can_install_to_source_target: true,
                reason_code: None,
                evidence_ids: vec![],
            },
            warnings: vec![],
            mcp_credential: None,
        }
    }

    fn snapshot_from(
        targets: Vec<PortableInventoryTargetDto>,
        items: Vec<PortableInventoryItemDto>,
    ) -> PortableInventorySnapshotDto {
        let hash = inventory_snapshot_hash(&targets, &items).expect("hash");
        PortableInventorySnapshotDto {
            inventory_snapshot_hash: hash,
            refreshed_at: Utc::now().to_rfc3339(),
            stale: false,
            targets,
            items,
        }
    }

    async fn preview_action(
        repo: &AgentHubRepo,
        snap: &PortableInventorySnapshotDto,
        ids: Vec<String>,
        action: PortableAssetActionKind,
        keep_data: bool,
    ) -> PortableAssetActionPlanDto {
        preview_portable_asset_action_with_inventory(
            repo,
            PreviewPortableAssetActionRequest {
                inventory_snapshot_hash: snap.inventory_snapshot_hash.clone(),
                inventory_item_ids: ids,
                action,
                keep_data,
                conflict_policy: PortableAssetConflictPolicy::SkipExisting,
                expected_canonical_revision_id: None,
            },
            snap,
            "owner-fp",
        )
        .await
        .expect("preview")
    }

    /// Business Logic: Claude Plugin enable 必须带 --scope user argv。
    #[tokio::test]
    async fn claude_plugin_enable_locks_scope_argv() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        let item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "review@local",
            "/plugins/review",
            Some(false),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Enable,
            false,
        )
        .await;

        let mut post_item = item.clone();
        post_item.actual_enabled = Some(true);
        let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

        let deps = PortableActionExecutorDeps {
            repo: repo.clone(),
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token.clone(),
                client_request_id: "req-plugin-1".into(),
            },
        )
        .await
        .expect("apply");

        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::Succeeded
        );
        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], "plugin");
        assert_eq!(calls[0].args[1], "enable");
        let scope_idx = calls[0].args.iter().position(|a| a == "--scope").unwrap();
        assert_eq!(calls[0].args[scope_idx + 1], "user");
    }

    /// Business Logic: 真实 plugin 根 + inventory hash 域 recheck 不得误报 SOURCE_HASH_CHANGED。
    /// Code Logic: temp plugin root + hash_plugin_root → preview → apply；CLI 必须执行。
    #[tokio::test]
    async fn plugin_real_root_hash_domain_passes_recheck() {
        let dir = tempfile::tempdir().unwrap();
        let plugin_root = dir.path().join("plugins").join("demo-plugin");
        std::fs::create_dir_all(plugin_root.join(".claude-plugin")).unwrap();
        std::fs::write(
            plugin_root.join(".claude-plugin/plugin.json"),
            r#"{"name":"demo-plugin","version":"1.0.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(plugin_root.join("skills")).unwrap();
        let (content_hash, tree_hash) = hash_plugin_root(&plugin_root).unwrap();
        // 生产 inventory 对有 manifest 的 plugin 用 material hash，不等于 path-string sha
        let path_string_hash =
            crate::agent_hub::object_store::sha256_hex(plugin_root.display().to_string().as_bytes());
        assert_ne!(content_hash, path_string_hash);

        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("enabled");
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "demo-plugin@local",
            plugin_root.to_str().unwrap(),
            Some(false),
        );
        item.content_hash = Some(content_hash);
        item.tree_hash = Some(tree_hash);
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Enable,
            false,
        )
        .await;
        assert_eq!(
            plan.changes[0].expected_source_hash.as_deref(),
            item.content_hash.as_deref()
        );

        let mut post_item = item.clone();
        post_item.actual_enabled = Some(true);
        let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);
        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-plugin-hash-domain".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::Succeeded,
            "unchanged real plugin root must not fail source-hash recheck: {:?}",
            result.items[0].error_code
        );
        assert_ne!(
            result.items[0].error_code.as_deref(),
            Some("PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED")
        );
        assert_eq!(runner.calls().len(), 1);
    }

    /// Business Logic: Skill disable 必须 move 到 disabled 且零 spawn。
    #[tokio::test]
    async fn skill_disable_moves_to_disabled_with_backup_root() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        let data = dir.path().join("data");
        std::fs::create_dir_all(claude.join("skills/my-skill")).unwrap();
        std::fs::write(claude.join("skills/my-skill/SKILL.md"), "# skill\n").unwrap();
        let skill_path = claude.join("skills/my-skill");
        let (hash, _, _, _) = hash_skill_directory(&skill_path).unwrap();

        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Skill,
            "my-skill",
            skill_path.to_str().unwrap(),
            Some(true),
        );
        item.content_hash = Some(hash);
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;

        let mut post_item = item.clone();
        post_item.actual_enabled = Some(false);
        post_item.source_path = Some(
            data.join("claude-assets/disabled/skills/my-skill")
                .to_string_lossy()
                .into(),
        );
        let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: Some(claude.clone()),
            data_dir: Some(data.clone()),
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-skill-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::Succeeded
        );
        assert!(!skill_path.exists());
        assert!(data.join("claude-assets/disabled/skills/my-skill").exists());
        assert!(runner.calls().is_empty());
    }

    /// Business Logic: MCP disable 使用 semantic patch，保留 sibling keys，DTO 无 secret。
    #[tokio::test]
    async fn mcp_disable_semantic_patch_preserves_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&claude).unwrap();
        let cfg = claude.join(".claude.json");
        std::fs::write(
            &cfg,
            r#"{
  // keep comment
  "mcpServers": {
    "keep-me": { "command": "uvx", "env": { "TOKEN": "secret-value" } },
    "drop-me": { "command": "npx", "env": { "KEY": "secret-key" } }
  }
}
"#,
        )
        .unwrap();

        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Mcp,
            "drop-me",
            cfg.to_str().unwrap(),
            Some(true),
        );
        // 避免整文件 hash 误伤（MCP 语义 path CAS 独立）
        item.content_hash = None;
        item.tree_hash = Some("t".into());
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;

        let mut post_item = item.clone();
        post_item.actual_enabled = Some(false);
        let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_item]);

        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: Some(claude),
            data_dir: Some(data),
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-mcp-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::Succeeded
        );
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("keep-me"));
        assert!(!text.contains("\"drop-me\""));
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("secret-value"));
        assert!(!json.contains("secret-key"));
        assert!(runner.calls().is_empty());
    }

    /// Business Logic: unsupported target 零 spawn 且 blocked。
    #[tokio::test]
    async fn unsupported_target_zero_spawn() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        let item = sample_item(
            AgentTarget::Codex,
            PortableAssetKind::Skill,
            "x",
            "/skills/x",
            Some(true),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Codex)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;
        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap.clone()),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(snap),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-codex-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(result.items[0].state, PortableAssetActionItemState::Blocked);
        assert!(runner.calls().is_empty());
    }

    /// Business Logic: source hash 变化 fail-closed，不执行 CLI。
    #[tokio::test]
    async fn changed_source_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        std::fs::create_dir_all(claude.join("skills/s")).unwrap();
        std::fs::write(claude.join("skills/s/SKILL.md"), "v1").unwrap();
        let path = claude.join("skills/s");

        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        let mut item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Skill,
            "s",
            path.to_str().unwrap(),
            Some(true),
        );
        item.content_hash = Some("stale-hash".into());
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;
        std::fs::write(claude.join("skills/s/SKILL.md"), "v2-changed").unwrap();

        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap.clone()),
            claude_config_dir: Some(claude),
            data_dir: Some(dir.path().join("data")),
            rescan_override: Some(snap),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-drift-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(result.items[0].state, PortableAssetActionItemState::Failed);
        assert_eq!(
            result.items[0].error_code.as_deref(),
            Some("PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED")
        );
        assert!(runner.calls().is_empty());
    }

    /// Business Logic: spawn 不确定 → outcomeUnknown 并 complete ledger 可 replay。
    #[tokio::test]
    async fn spawn_ambiguity_marks_outcome_unknown_and_completes_ledger() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_io_err(AppError::unavailable("spawn transport lost"));

        let item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "p@x",
            "/p",
            Some(true),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;
        let deps = PortableActionExecutorDeps {
            repo: repo.clone(),
            runner,
            env: None,
            pre_inventory: Some(snap.clone()),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(snap),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token.clone(),
                client_request_id: "req-unknown-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::OutcomeUnknown
        );
        let replay = claim_portable_asset_action(&repo, &plan.plan_token, "req-unknown-1")
            .await
            .unwrap();
        match replay {
            PortableActionClaim::Replay(json) => {
                let back: PortableAssetActionResultDto = serde_json::from_str(&json).unwrap();
                assert_eq!(back, result);
            }
            other => panic!("expected replay, got {other:?}"),
        }
    }

    /// Business Logic: 部分项 blocked/部分成功 → 逐项 partial results。
    #[tokio::test]
    async fn partial_per_item_results() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        let ok_item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "ok@p",
            "/ok",
            Some(false),
        );
        let mut bad = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "bad@p",
            "/bad",
            Some(false),
        );
        bad.capabilities.can_enable = false;
        let snap = snapshot_from(
            vec![sample_target(AgentTarget::Claude)],
            vec![ok_item.clone(), bad.clone()],
        );
        let plan = preview_action(
            &repo,
            &snap,
            vec![
                ok_item.inventory_item_id.clone(),
                bad.inventory_item_id.clone(),
            ],
            PortableAssetActionKind::Enable,
            false,
        )
        .await;

        let mut ok_post = ok_item.clone();
        ok_post.actual_enabled = Some(true);
        let post = snapshot_from(
            vec![sample_target(AgentTarget::Claude)],
            vec![ok_post, bad.clone()],
        );
        let deps = PortableActionExecutorDeps {
            repo,
            runner,
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-partial-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(result.items.len(), 2);
        let by_id: BTreeMap<_, _> = result
            .items
            .iter()
            .map(|i| (i.inventory_item_id.clone(), i.state))
            .collect();
        assert_eq!(
            by_id.get(&ok_item.inventory_item_id),
            Some(&PortableAssetActionItemState::Succeeded)
        );
        assert_eq!(
            by_id.get(&bad.inventory_item_id),
            Some(&PortableAssetActionItemState::Blocked)
        );
    }

    /// Business Logic: Adopt 不得假成功，必须 PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED。
    #[tokio::test]
    async fn adopt_fail_closed_without_ownership_write() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        let item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Skill,
            "adopt-me",
            "/skills/adopt-me",
            Some(true),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Adopt,
            false,
        )
        .await;
        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap.clone()),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(snap),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-adopt-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(result.items[0].state, PortableAssetActionItemState::Failed);
        assert_eq!(
            result.items[0].error_code.as_deref(),
            Some("PORTABLE_ASSET_ACTION_ADOPT_NOT_WIRED")
        );
        assert!(runner.calls().is_empty());
    }

    /// Business Logic: 过期 plan 在 claim 时 fail-closed。
    #[tokio::test]
    async fn expired_plan_rejected_at_claim() {
        let repo = test_repo().await;
        let item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "p@x",
            "/p",
            Some(true),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let mut plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Disable,
            false,
        )
        .await;
        // 手工把 expires_at 改到过去并更新 DB
        plan.expires_at = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        // 直接 update row
        sqlx::query(
            "UPDATE agent_hub_portable_asset_action_plans SET expires_at = ? WHERE plan_token = ?",
        )
        .bind(&plan.expires_at)
        .bind(&plan.plan_token)
        .execute(&repo.pool())
        .await
        .unwrap();
        let err = claim_portable_asset_action(&repo, &plan.plan_token, "req-exp-1")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("PORTABLE_ASSET_ACTION_PLAN_EXPIRED")
                || format!("{err:?}").contains("PORTABLE_ASSET_ACTION_PLAN_EXPIRED")
        );
    }

    /// Business Logic: Plugin uninstall 固定 --scope，keep_data 传入 argv。
    #[tokio::test]
    async fn plugin_uninstall_preserves_scope_and_keep_data_argv() {
        let repo = test_repo().await;
        let runner = Arc::new(FakeProcessRunner::new());
        runner.push_ok("ok");
        let item = sample_item(
            AgentTarget::Claude,
            PortableAssetKind::Plugin,
            "shared@cc",
            "/plugins/shared",
            Some(true),
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_action(
            &repo,
            &snap,
            vec![item.inventory_item_id.clone()],
            PortableAssetActionKind::Uninstall,
            true,
        )
        .await;

        let post = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![]);
        let deps = PortableActionExecutorDeps {
            repo,
            runner: runner.clone(),
            env: None,
            pre_inventory: Some(snap),
            claude_config_dir: None,
            data_dir: None,
            rescan_override: Some(post),
        };
        let result = apply_portable_asset_action_with(
            None,
            &deps,
            ApplyPortableAssetActionRequest {
                plan_token: plan.plan_token,
                client_request_id: "req-uninst-1".into(),
            },
        )
        .await
        .expect("apply");
        assert_eq!(
            result.items[0].state,
            PortableAssetActionItemState::Succeeded
        );
        let args = &runner.calls()[0].args;
        assert!(args.iter().any(|a| a == "uninstall"));
        assert!(args.iter().any(|a| a == "--scope"));
        assert!(args.iter().any(|a| a == "--keep-data"));
    }
}
