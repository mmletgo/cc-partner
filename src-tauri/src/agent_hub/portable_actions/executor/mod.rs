//! portable_actions/executor — claim plan → target action → rescan → complete ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     Apply 必须原子 claim、按 target adapter 执行、rescan 后仅在 observed 匹配 expected
//!     时标 succeeded；spawn/transport 不确定必须 outcomeUnknown；部分成功按项返回。
//!
//! Code Logic（这个模块做什么）:
//!     目录模块：本文件承载核心执行流程（PortableActionExecutorDeps 依赖注入、
//!     `apply_portable_asset_action` / `apply_portable_asset_action_with` 生产入口、
//!     claim 后复验、逐项执行与 rescan 对账、逃逸软链修复）；`confirm` 子模块承载
//!     确认当前版本族（Hub 账本写入 + 跨 Agent 聚合 + tree hash 写前复验）；
//!     `tests` 子模块承载单测。依赖 B2/B3 与 targets。

mod confirm;

#[cfg(test)]
mod tests;

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
use crate::agent_hub::models::{AgentTarget, PortableActionClaim};
use crate::agent_hub::packages::activator::ProcessRunner;
use crate::agent_hub::portable_inventory::{
    inspect_portable_inventory_force_query, inspect_portable_inventory_force_with_env_query,
    inspect_portable_inventory_query, inspect_portable_inventory_with_env_query,
    PortableInventoryItemDto, PortableInventoryManagementState,
    PortableInventoryMutationCapability, PortableInventoryQuery, PortableInventorySnapshotDto,
};
use crate::agent_hub::targets::paths::TargetEnvironment;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use self::confirm::{confirm_current_version_on_ledger, verify_expected_tree_hash};

#[cfg(test)]
pub use tests::test_deps_with_runner;

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
        let env = crate::agent_hub::targets::paths::TargetEnvironment::from_process();
        let program = spec
            .program
            .to_str()
            .and_then(|name| crate::agent_hub::targets::paths::resolve_executable(name, &env))
            .unwrap_or_else(|| spec.program.clone());
        let mut cmd = std::process::Command::new(&program);
        cmd.args(&spec.args);
        cmd.env("PATH", crate::claude_cli::cli_command_path_env());
        if let Some(cwd) = spec.cwd.as_ref() {
            cmd.current_dir(cwd);
        }
        let output = cmd.output().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound || e.to_string().contains("No such file") {
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
        PortableActionClaim::Replay(json) => serde_json::from_str(&json).map_err(AppError::from),
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
            if let Ok(post) =
                resolve_post_inventory(state, deps, stored.request.inventory_query.clone()).await
            {
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
    let live =
        match resolve_force_inventory(state, deps, stored.request.inventory_query.clone()).await {
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
    let global_block = plan
        .blocking_reasons
        .iter()
        .any(|r| r.starts_with("PORTABLE_ASSET_ACTION_INVENTORY_"));
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

    let pre_snapshot =
        resolve_pre_inventory(state, deps, stored.request.inventory_query.clone()).await?;
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
    // confirm current version 的跨 Agent 聚合观测数（含自身），按 item id 记录；
    // 仅在自身确认成功且聚合重扫完成时存在，用于把聚合数反映到 item message。
    let mut confirm_aggregate_counts: BTreeMap<String, usize> = BTreeMap::new();

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

        // 旧 plan 可能来自 capability 尚未按动作拆分的版本；即使 target 汇总能力
        // 仍为 Supported，也必须先按当前 item 的 enable/disable/uninstall affordance 阻断。
        if let Some(outcome) = item_action_capability_block(change, plan.action, pre.as_ref()) {
            raw_results.push((change.inventory_item_id.clone(), outcome, pre));
            continue;
        }

        // 生产 apply 必须在任何 target mutation 前重算确定性递归 tree hash。
        // 注入式旧单测使用虚构路径/快照，仍由 target adapter 的既有 hash seam 覆盖。
        // 确认当前版本只写 Hub 账本，禁止跟随任意 symlink 重算树。
        if !plan.action.bypasses_target_cli_gates() && (state.is_some() || deps.env.is_some()) {
            if let Some(outcome) = verify_expected_tree_hash(change) {
                raw_results.push((change.inventory_item_id.clone(), outcome, pre));
                continue;
            }
        }

        if plan.action.is_hub_ledger_only() {
            let (outcome, aggregate_count) = confirm_current_version_on_ledger(
                state,
                deps,
                stored.request.inventory_query.clone(),
                change,
                pre.as_ref(),
            )
            .await;
            if let Some(count) = aggregate_count {
                confirm_aggregate_counts.insert(change.inventory_item_id.clone(), count);
            }
            raw_results.push((change.inventory_item_id.clone(), outcome, pre));
            continue;
        }
        if plan.action.is_escape_link_repair() {
            let outcome = materialize_escape_link_change(change, pre.as_ref());
            raw_results.push((change.inventory_item_id.clone(), outcome, pre));
            continue;
        }

        // 写入 target 前最后一次读取当前 manifest/CLI capability。
        // revalidate_claimed_plan 早先的 force inspect 只绑定 inventory hash；在
        // preview/claim 后 manifest 变为 scan-only 时，旧 plan 仍可能带着 direct-local
        // allowlist 生成的空 blocking_reasons，因此必须在真正调用 adapter 前再 gate 一次。
        if let Some(outcome) = revalidate_target_mutation_before_write(
            state,
            deps,
            stored.request.inventory_query.clone(),
            plan.action,
            change,
        )
        .await
        {
            raw_results.push((change.inventory_item_id.clone(), outcome, pre));
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
    let post = resolve_post_inventory(state, deps, stored.request.inventory_query.clone()).await?;
    let post_by_id: BTreeMap<String, &PortableInventoryItemDto> = post
        .items
        .iter()
        .map(|i| (i.inventory_item_id.clone(), i))
        .collect();

    // Fallback 索引：按逻辑身份 (target, scope_id, native_id) 索引 post inventory。
    // 防御性 fallback：inventory_item_id 现在已经路径无关（source_identity 用 origin_namespace，
    // 即 "standalone" / "plugin:{id}"），enable/disable 移动文件不再让 id 漂移；
    // 这段 fallback 主要兜底 scope_id 因 hub_project_id 重映射而变化、或未来其他让 id
    // 漂移的边界场景。保留比删除安全。
    // 同一逻辑键若出现多个 post item（理论不应发生），优先取 actual_enabled 与 action
    // 期望一致的；都一致则取最后一个（保持与既有覆盖式 collect 语义一致）。
    let mut post_by_logical_key: BTreeMap<
        (AgentTarget, String, String),
        &PortableInventoryItemDto,
    > = BTreeMap::new();
    for item in post.items.iter() {
        let key = (item.target, item.scope_id.clone(), item.native_id.clone());
        match post_by_logical_key.get(&key) {
            Some(existing) => {
                // 优先保留与 action 期望 actual_enabled 一致的项；否则保持稳定，跳过覆盖。
                let desired = match plan.action {
                    PortableAssetActionKind::Enable
                    | PortableAssetActionKind::Attach
                    | PortableAssetActionKind::MigrateToStore => Some(true),
                    PortableAssetActionKind::Disable | PortableAssetActionKind::Detach => {
                        Some(false)
                    }
                    _ => existing.actual_enabled,
                };
                if desired.is_some() && item.actual_enabled == desired {
                    post_by_logical_key.insert(key, item);
                }
            }
            None => {
                post_by_logical_key.insert(key, item);
            }
        }
    }

    let mut items = Vec::with_capacity(raw_results.len());
    for (item_id, raw, pre) in raw_results {
        let change = plan
            .changes
            .iter()
            .find(|c| c.inventory_item_id == item_id)
            .expect("change exists");
        let (state, error_code, mut message) = reconcile_item(
            plan.action,
            plan.keep_data,
            change.kind,
            &raw,
            pre.as_ref(),
            resolve_post_item(&item_id, pre.as_ref(), &post_by_id, &post_by_logical_key),
        );
        // 聚合了其它 Agent 观测时把数量带进成功 message（N 含自身；N==1 保持原文）。
        if plan.action == PortableAssetActionKind::ConfirmCurrentVersion {
            if let Some(count) = confirm_aggregate_counts.get(&item_id) {
                if *count > 1 && message.as_deref() == Some("current version recorded") {
                    message = Some(format!(
                        "current version recorded ({count} agent observations)"
                    ));
                }
            }
        }
        items.push(PortableAssetActionItemResultDto {
            inventory_item_id: item_id,
            state,
            error_code,
            message,
        });
    }

    Ok(PortableAssetActionResultDto {
        plan_token: plan.plan_token.clone(),
        client_request_id: request.client_request_id.clone(),
        items,
    })
}

/// 在 rescan 后按需为 enable/disable 找回 post inventory item 的兜底投影。
///
/// Business Logic（为什么需要这个函数）:
///     inventory_item_id 现在已经路径无关（source_identity 用 origin_namespace，
///     即 "standalone" / "plugin:{id}"），enable/disable 移动文件不再让 id 漂移，
///     精确匹配就能命中 post 投影。本函数作为防御性兜底，主要覆盖 scope_id 因
///     hub_project_id 重映射而变化、或未来其他让 id 漂移的边界场景。
///
/// Code Logic（这个函数做什么）:
///     1. 先按 inventory_item_id 精确匹配——覆盖 uninstall、enable/disable 与未触动物品的正常路径。
///     2. 命中失败时用 pre item 的逻辑身份 (target, scope_id, native_id) 在
///        post_by_logical_key 中查找同一物品移动后的新投影。
///     仅决定"是否找到 post 投影"；成功/失败的最终判定仍由 reconcile_item 按 action 与
///     actual_enabled 自行计算，因此 uninstall 期望 None 的语义不受影响。
fn resolve_post_item<'a>(
    item_id: &str,
    pre: Option<&PortableInventoryItemDto>,
    post_by_id: &BTreeMap<String, &'a PortableInventoryItemDto>,
    post_by_logical_key: &BTreeMap<(AgentTarget, String, String), &'a PortableInventoryItemDto>,
) -> Option<&'a PortableInventoryItemDto> {
    // 1. 精确匹配（uninstall/enable/disable/未触动物品的正常路径——id 已路径无关）
    if let Some(item) = post_by_id.get(item_id) {
        return Some(*item);
    }
    // 2. fallback：兜底 scope_id 因 hub_project_id 重映射等场景导致的 id 漂移；
    //    用 pre 的逻辑身份 (target, scope_id, native_id) 在 post 里匹配同一物品。
    let pre = pre?;
    post_by_logical_key
        .get(&(pre.target, pre.scope_id.clone(), pre.native_id.clone()))
        .copied()
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
                PortableAssetActionKind::Attach | PortableAssetActionKind::MigrateToStore => {
                    if post.is_some_and(|p| p.store.store_attached) {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("store attachment verified by rescan".into()),
                        )
                    } else if post.is_none() {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING".into()),
                            Some("item missing after store attach".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("expected storeAttached=true after attach".into()),
                        )
                    }
                }
                PortableAssetActionKind::Detach => {
                    let pre_via_other = pre.is_some_and(|p| p.store.loaded_via_other_path);
                    let detached = if pre_via_other {
                        post.is_none()
                            || post.is_some_and(|p| {
                                !p.store.loaded_via_other_path && !p.store.store_attached
                            })
                    } else {
                        post.is_none() || post.is_some_and(|p| !p.store.store_attached)
                    };
                    if detached {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("native store link removed".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some(if pre_via_other {
                                "expected source store link gone after borrowed detach".into()
                            } else {
                                "expected storeAttached=false after detach".into()
                            }),
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
                PortableAssetActionKind::DestroyStore => {
                    if post.is_none()
                        || post
                            .is_some_and(|p| p.store.store_id.is_none() && !p.store.store_attached)
                    {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("store destroyed".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("store item still present after destroy".into()),
                        )
                    }
                }
                PortableAssetActionKind::ConfirmCurrentVersion => {
                    if post.is_some_and(|p| {
                        p.management_state == PortableInventoryManagementState::HubManaged
                    }) {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("current version recorded".into()),
                        )
                    } else if post.is_none() {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING".into()),
                            Some("item missing after confirm current version".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("item still drifted after confirm current version".into()),
                        )
                    }
                }
                PortableAssetActionKind::MaterializeEscapeLink => {
                    if post.is_some_and(|p| {
                        p.store.store_attached
                            && !p.warnings.iter().any(|warning| {
                                warning == "store_symlink_escape" || warning == "source_blocked"
                            })
                    }) {
                        (
                            PortableAssetActionItemState::Succeeded,
                            None,
                            Some("escape link restored into store".into()),
                        )
                    } else if post.is_none() {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISSING".into()),
                            Some("item missing after restore escape link".into()),
                        )
                    } else {
                        (
                            PortableAssetActionItemState::Failed,
                            Some("PORTABLE_ASSET_ACTION_RESCAN_MISMATCH".into()),
                            Some("expected storeAttached=true after escape restore".into()),
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
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if let Some(snap) = &deps.pre_inventory {
        return Ok(snap.clone());
    }
    let state = state
        .ok_or_else(|| AppError::validation("PORTABLE_ASSET_ACTION_STATE_REQUIRED_FOR_INSPECT"))?;
    if let Some(env) = &deps.env {
        return inspect_portable_inventory_with_env_query(state, env, query).await;
    }
    inspect_portable_inventory_query(state, query).await
}

async fn resolve_force_inventory(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    if let Some(snap) = &deps.pre_inventory {
        return Ok(snap.clone());
    }
    let state = state
        .ok_or_else(|| AppError::validation("PORTABLE_ASSET_ACTION_STATE_REQUIRED_FOR_INSPECT"))?;
    if let Some(env) = &deps.env {
        return inspect_portable_inventory_force_with_env_query(state, env, query).await;
    }
    inspect_portable_inventory_force_query(state, query).await
}

/// 在 portable action adapter 写入前忽略旧快照，直接重新读取本机 mutation capability。
///
/// Business Logic（为什么需要这个函数）:
///     support manifest/CLI 版本可能在 preview 后变化；任何 direct-local executor 都必须
///     服从当前 scan-only 结果，不能由旧 plan 或 allowlist 恢复写能力。
///
/// Code Logic（这个函数做什么）:
///     生产状态下绕过 pre_inventory，执行 force inspect；目标不存在、扫描失败或能力非
///     Supported 都返回 Blocked raw outcome。注入式无 state 测试保持既有 fake snapshot seam。
async fn revalidate_target_mutation_before_write(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    query: PortableInventoryQuery,
    action: PortableAssetActionKind,
    change: &super::models::PortableAssetActionChangeDto,
) -> Option<TargetActionRawOutcome> {
    if action.bypasses_target_cli_gates() {
        return None;
    }
    let state = state?;
    let live = if let Some(env) = &deps.env {
        inspect_portable_inventory_force_with_env_query(state, env, query).await
    } else {
        inspect_portable_inventory_force_query(state, query).await
    };
    let live = match live {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return Some(TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_TARGET_MUTATION_REVALIDATE_FAILED".into(),
                message: error.to_string(),
            })
        }
    };
    let live_item = live
        .items
        .iter()
        .find(|candidate| candidate.inventory_item_id == change.inventory_item_id);
    let capability = live
        .targets
        .iter()
        .find(|candidate| candidate.target == change.target)
        .map(|candidate| candidate.mutation_capability);
    match capability {
        Some(PortableInventoryMutationCapability::Supported) => {}
        Some(other) => {
            return Some(TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_TARGET_MUTATION_NOT_SUPPORTED".into(),
                message: format!(
                    "target {} mutation capability is {:?}",
                    change.target.as_str(),
                    other
                ),
            });
        }
        None => {
            // 借用项把 change.target 重映射到所有者；过滤后的 inspect 可能不含所有者 CLI 指纹。
            let remapped = live_item.is_some_and(|item| item.target != change.target);
            if !remapped {
                return Some(TargetActionRawOutcome::Blocked {
                    code: "PORTABLE_ASSET_ACTION_TARGET_MUTATION_NOT_SUPPORTED".into(),
                    message: format!(
                        "target {} mutation capability is {:?}",
                        change.target.as_str(),
                        PortableInventoryMutationCapability::Blocked
                    ),
                });
            }
        }
    }
    item_action_capability_block(change, action, live_item)
}

/// 根据 inventory item 的逐动作 affordance 阻断旧 plan 的 capability 旁路。
fn item_action_capability_block(
    change: &super::models::PortableAssetActionChangeDto,
    action: PortableAssetActionKind,
    item: Option<&PortableInventoryItemDto>,
) -> Option<TargetActionRawOutcome> {
    let Some(item) = item else {
        return Some(TargetActionRawOutcome::Blocked {
            code: "PORTABLE_ASSET_ACTION_ITEM_NOT_FOUND".into(),
            message: "live inventory item missing before mutation".into(),
        });
    };
    let allowed = match action {
        PortableAssetActionKind::Enable => item.capabilities.can_enable,
        PortableAssetActionKind::Disable => item.capabilities.can_disable,
        PortableAssetActionKind::Uninstall => item.capabilities.can_uninstall,
        PortableAssetActionKind::Adopt => item.capabilities.can_adopt,
        PortableAssetActionKind::InstallToSourceTarget => {
            item.capabilities.can_install_to_source_target
        }
        PortableAssetActionKind::Attach => item.capabilities.can_attach,
        PortableAssetActionKind::Detach => item.capabilities.can_detach,
        PortableAssetActionKind::DestroyStore => item.capabilities.can_destroy_store,
        PortableAssetActionKind::MigrateToStore => item.capabilities.can_migrate_to_store,
        PortableAssetActionKind::ConfirmCurrentVersion => {
            item.capabilities.can_confirm_current_version
        }
        PortableAssetActionKind::MaterializeEscapeLink => {
            item.capabilities.can_materialize_escape_link
        }
    };
    if allowed {
        return None;
    }
    let code = if change.kind == crate::agent_hub::portable_inventory::PortableAssetKind::Plugin
        && matches!(
            action,
            PortableAssetActionKind::Disable | PortableAssetActionKind::Uninstall
        )
        && item.capabilities.reason_code.as_deref() == Some("deactivate_package_not_supported")
    {
        "PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED"
    } else {
        "PORTABLE_ASSET_ACTION_ITEM_CAPABILITY_BLOCKED"
    };
    Some(TargetActionRawOutcome::Blocked {
        code: code.into(),
        message: item
            .capabilities
            .reason_code
            .clone()
            .unwrap_or_else(|| "item action capability is not supported".into()),
    })
}

async fn resolve_post_inventory(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    query: PortableInventoryQuery,
) -> Result<PortableInventorySnapshotDto, AppError> {
    // 无论是否存在测试 override，post-mutation 入口都先失效旧缓存。
    crate::agent_hub::portable_inventory::invalidate_portable_inventory_cache();
    if let Some(snap) = &deps.rescan_override {
        return Ok(snap.clone());
    }
    let state = state
        .ok_or_else(|| AppError::validation("PORTABLE_ASSET_ACTION_STATE_REQUIRED_FOR_INSPECT"))?;
    if let Some(env) = &deps.env {
        return inspect_portable_inventory_force_with_env_query(state, env, query).await;
    }
    inspect_portable_inventory_force_query(state, query).await
}

/// 把 native 路径上的逃逸软链恢复为仓库真树 + 正规 store 软链。
///
/// Business Logic: 不删源树、不 spawn CLI；Grok 等无 L3 身份也必须能修。
/// Code Logic: 读 change.path / pre.native_id，走 execute_skill_or_command_store。
fn materialize_escape_link_change(
    change: &super::models::PortableAssetActionChangeDto,
    pre: Option<&PortableInventoryItemDto>,
) -> TargetActionRawOutcome {
    let Some(item) = pre else {
        return TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_ITEM_NOT_FOUND".into(),
            message: "live inventory item missing before escape restore".into(),
        };
    };
    let Some(path) = change.path.as_deref().or(item.source_path.as_deref()) else {
        return TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_SOURCE_MISSING".into(),
            message: "native path missing for restore escape link".into(),
        };
    };
    match crate::agent_hub::portable_store::execute_skill_or_command_store(
        change.target,
        PortableAssetActionKind::MaterializeEscapeLink,
        change.kind,
        &item.native_id,
        std::path::Path::new(path),
        Some(item),
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            let code = error.to_string();
            let stable = if code.contains("PORTABLE_STORE_ESCAPE_TARGET_MISSING") {
                "PORTABLE_STORE_ESCAPE_TARGET_MISSING"
            } else if code.contains("PORTABLE_STORE_REFUSE_MATERIALIZE_STORE_LINK") {
                "PORTABLE_STORE_REFUSE_MATERIALIZE_STORE_LINK"
            } else if code.contains("PORTABLE_STORE_LINK_MISSING") {
                "PORTABLE_STORE_LINK_MISSING"
            } else if code.contains("PORTABLE_STORE_MATERIALIZE_SOURCE_IS_NATIVE") {
                "PORTABLE_STORE_MATERIALIZE_SOURCE_IS_NATIVE"
            } else {
                "PORTABLE_ASSET_ACTION_MATERIALIZE_FAILED"
            };
            TargetActionRawOutcome::Failed {
                code: stable.into(),
                message: code,
            }
        }
    }
}
