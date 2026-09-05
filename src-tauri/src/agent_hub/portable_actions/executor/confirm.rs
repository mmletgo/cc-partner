//! portable_actions/executor/confirm — 确认当前版本族（只写 Hub 账本）
//!
//! Business Logic（为什么需要这个模块）:
//!     Skill/Command/Plugin/MCP 可能被 CLI 自行更新；用户确认当前版本后 Hub 只把账本
//!     materialization 对齐磁盘观测并标 Synced，同一 canonical asset 下其它 Agent 的
//!     相同内容观测必须一并确认；mutation 前的确定性 tree hash 写前复验也在此 fail-closed。
//!
//! Code Logic（这个模块做什么）:
//!     `confirm_current_version_on_ledger`（校验 preview 绑定 hash → 账本写入 → 跨 Agent
//!     聚合）、`aggregate_confirm_same_asset_other_targets`、`confirm_current_version_write`、
//!     `verify_expected_tree_hash`；由 executor 核心流程在 hub-ledger-only 动作与写前复验时调用。

use super::{resolve_force_inventory, PortableActionExecutorDeps};
use crate::agent_hub::models::{MaterializationStatus, NewMaterialization};
use crate::agent_hub::portable_actions::models::PortableAssetActionChangeDto;
use crate::agent_hub::portable_actions::targets::TargetActionRawOutcome;
use crate::agent_hub::portable_inventory::{
    hash_directory_tree, hash_plugin_root, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryQuery,
};
use crate::agent_hub::targets::portable::hash_skill_directory;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;

/// 把当前磁盘 hash 写回 materialization，不改 Agent 文件；并跨 Agent 聚合确认。
///
/// Business Logic（为什么需要这个函数）:
///     Skill/Command/Plugin/MCP 可能被 CLI 自己更新；用户确认后 Hub 只把当前文件记为一致基准。
///     同一仓库真树常被多个 Agent 软链观测（如 `~/.claude/skills/foo` 与 `~/.codex/skills/foo`
///     同时链到 `~/.agents/skills/foo`），确认一次必须让同一 canonical asset 下其它 target 上
///     观测到相同内容的 Drifted 项一并生效，而不是逼用户逐个 Agent 重复点确认。
///
/// Code Logic（这个函数做什么）:
///     校验 preview 绑定的 observed hash，按 viewing target 找到 binding/materialization，
///     把 `rendered_hash`/`observed_external_hash` 对齐并标 Synced；自身写入成功（Applied 或
///     Skipped）后用 target=None 的同 scope query 强制重扫，把同一 canonical asset 下其它
///     target 观测到相同内容 hash 的 Drifted 项一并写 Synced（单项失败只 warn 不中断）。
///     返回 raw outcome 与聚合观测数（含自身）；聚合重扫失败时观测数为 None（不聚合、不报错）。
pub(super) async fn confirm_current_version_on_ledger(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    query: PortableInventoryQuery,
    change: &PortableAssetActionChangeDto,
    pre: Option<&PortableInventoryItemDto>,
) -> (TargetActionRawOutcome, Option<usize>) {
    let Some(item) = pre else {
        return (
            TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_ITEM_NOT_FOUND".into(),
                message: "inventory item missing before confirm current version".into(),
            },
            None,
        );
    };
    let Some(asset_id) = item.canonical_asset_id.as_deref() else {
        return (
            TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_CANONICAL_MISSING".into(),
                message: "canonical asset missing for confirm current version".into(),
            },
            None,
        );
    };
    let observed = item.content_hash.clone().or_else(|| item.tree_hash.clone());
    let Some(observed) = observed else {
        return (
            TargetActionRawOutcome::Blocked {
                code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_MISSING".into(),
                message: "observed hash missing for confirm current version".into(),
            },
            None,
        );
    };
    if let Some(expected) = change.expected_source_hash.as_deref() {
        if expected != observed {
            return (
                TargetActionRawOutcome::Failed {
                    code: "PORTABLE_ASSET_ACTION_SOURCE_HASH_CHANGED".into(),
                    message: "disk hash changed since preview".into(),
                },
                None,
            );
        }
    }

    match confirm_current_version_write(&deps.repo, item, asset_id, &observed).await {
        Ok(outcome) => {
            // Applied / Skipped 都执行聚合：自身已一致时其它 Agent 观测可能仍 Drifted。
            let aggregate_count = if matches!(
                outcome,
                TargetActionRawOutcome::Applied | TargetActionRawOutcome::Skipped
            ) {
                aggregate_confirm_same_asset_other_targets(
                    state, deps, query, item, asset_id, &observed,
                )
                .await
            } else {
                None
            };
            (outcome, aggregate_count)
        }
        Err(error) => (
            TargetActionRawOutcome::Failed {
                code: "PORTABLE_ASSET_ACTION_CONFIRM_CURRENT_VERSION_FAILED".into(),
                message: error.to_string(),
            },
            None,
        ),
    }
}

/// 跨 Agent 聚合确认：同一 canonical asset 下其它 target 的相同内容 Drifted 观测一并写 Synced。
///
/// Business Logic（为什么需要这个函数）:
///     软链同一仓库真树的多个 Agent 会在库存里各自观测出 Drifted；用户确认一次当前版本后，
///     相同内容 hash 的其它 Agent 观测应同步确认为一致基准，避免重复操作。
///
/// Code Logic（这个函数做什么）:
///     把传入 query 的 target 过滤清空后强制重扫 inventory（沿用 deps 注入 seam，测试可控制），
///     对每个满足「target 不同于自身 + 同一 canonical asset + management_state 为 Drifted +
///     content/tree hash 与本次确认的 observed 一致」的项调用 `confirm_current_version_write`；
///     单项失败 `tracing::warn!` 后继续。返回聚合后的观测总数（含自身）；重扫失败返回 None。
async fn aggregate_confirm_same_asset_other_targets(
    state: Option<&AppState>,
    deps: &PortableActionExecutorDeps,
    query: PortableInventoryQuery,
    item: &PortableInventoryItemDto,
    asset_id: &str,
    observed: &str,
) -> Option<usize> {
    let mut aggregation_query = query;
    aggregation_query.target = None;
    let snapshot = match resolve_force_inventory(state, deps, aggregation_query).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "portable confirm current version aggregation rescan failed"
            );
            return None;
        }
    };
    let mut confirmed_others = 0usize;
    for other in &snapshot.items {
        if other.target == item.target {
            continue;
        }
        if other.canonical_asset_id.as_deref() != Some(asset_id)
            || other.management_state != PortableInventoryManagementState::Drifted
        {
            continue;
        }
        let other_observed = other
            .content_hash
            .clone()
            .or_else(|| other.tree_hash.clone());
        if other_observed.as_deref() != Some(observed) {
            // 磁盘内容分叉：其它 target 观测的不是本次确认的版本，保持 Drifted 不动。
            continue;
        }
        match confirm_current_version_write(&deps.repo, other, asset_id, observed).await {
            Ok(_) => confirmed_others += 1,
            Err(error) => tracing::warn!(
                error = %error,
                target = other.target.as_str(),
                "portable confirm current version aggregation write failed"
            ),
        }
    }
    Some(1 + confirmed_others)
}

/// 把单个 inventory 观测的 hash 写回其 target binding 的 materialization。
///
/// Business Logic（为什么需要这个函数）:
///     确认当前版本只改 Hub 账本：把 binding 对应 materialization 的
///     `rendered_hash`/`observed_external_hash` 对齐磁盘观测并标 Synced，不写 Agent 磁盘。
///
/// Code Logic（这个函数做什么）:
///     按 asset_id 列出 target bindings，取 `binding.target == item.target` 的一条，读其
///     materialization；若已是 Synced 且双 hash 等于 observed 则 Skipped，否则 upsert 为
///     Synced 并返回 Applied。
async fn confirm_current_version_write(
    repo: &AgentHubRepo,
    item: &PortableInventoryItemDto,
    asset_id: &str,
    observed: &str,
) -> Result<TargetActionRawOutcome, AppError> {
    let bindings = repo.list_target_bindings_for_asset(asset_id).await?;
    let Some(binding) = bindings
        .into_iter()
        .find(|binding| binding.target == item.target)
    else {
        return Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_MATERIALIZATION_MISSING".into(),
            message: "target binding missing for confirm current version".into(),
        });
    };
    let Some(existing) = repo.get_materialization_by_binding(&binding.id).await? else {
        return Ok(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_MATERIALIZATION_MISSING".into(),
            message: "materialization missing for confirm current version".into(),
        });
    };
    let already_current = existing.rendered_hash.as_deref() == Some(observed)
        && existing.observed_external_hash.as_deref() == Some(observed)
        && existing.status == MaterializationStatus::Synced;
    if already_current {
        return Ok(TargetActionRawOutcome::Skipped);
    }
    repo.upsert_materialization(NewMaterialization {
        asset_id: existing.asset_id,
        target: existing.target,
        target_binding_id: existing.target_binding_id,
        native_path: item.source_path.clone().or(existing.native_path),
        last_projected_revision_id: existing.last_projected_revision_id,
        rendered_hash: Some(observed.to_string()),
        observed_external_hash: Some(observed.to_string()),
        status: MaterializationStatus::Synced,
        last_error: None,
    })
    .await?;
    Ok(TargetActionRawOutcome::Applied)
}

/// 按 inventory 行的 tree hash 域重算源树，并在 mutation 前 fail-closed。
pub(super) fn verify_expected_tree_hash(
    change: &PortableAssetActionChangeDto,
) -> Option<TargetActionRawOutcome> {
    let expected = change.expected_tree_hash.as_deref()?;
    let Some(path) = change.path.as_deref() else {
        return Some(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_TREE_HASH_UNAVAILABLE".into(),
            message: "source path unavailable for tree hash recheck".into(),
        });
    };
    let path = std::path::Path::new(path);
    let actual = match change.kind {
        crate::agent_hub::portable_inventory::PortableAssetKind::Skill => {
            let dir = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };
            hash_skill_directory(&dir).map(|(_, tree, _, _)| tree)
        }
        crate::agent_hub::portable_inventory::PortableAssetKind::Plugin => {
            let dir = if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|| path.to_path_buf())
            };
            hash_plugin_root(&dir).map(|(_, tree)| tree)
        }
        _ if path.is_dir() => hash_directory_tree(path),
        _ => Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_TREE_HASH_UNSUPPORTED",
        )),
    };
    match actual {
        Ok(actual) if actual == expected => None,
        Ok(_) => Some(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_TREE_HASH_CHANGED".into(),
            message: "source tree changed since preview".into(),
        }),
        Err(_) => Some(TargetActionRawOutcome::Failed {
            code: "PORTABLE_ASSET_ACTION_TREE_HASH_UNAVAILABLE".into(),
            message: "source tree hash unavailable for recheck".into(),
        }),
    }
}
