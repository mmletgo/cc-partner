//! portable_actions/planner — 本机 portable 资产动作 preview
//!
//! Business Logic（为什么需要这个模块）:
//!     所有 mutation 前必须 preview 零写入；绑定 inventory hash / CLI 指纹 /
//!     source hash / mapping / opt-in；未纳管启停不创建 ownership。
//!
//! Code Logic（这个模块做什么）:
//!     校验前置条件、构建 changes、持久化短期 plan（TTL 10 分钟）。

use super::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
    PortableAssetBackupPolicy, PortableAssetCanonicalEffect, PortableAssetPlanOperation,
    PreviewPortableAssetActionRequest, StoredPortableAssetActionPlan,
};
use crate::agent_hub::models::PortableAssetActionPlanRecord;
use crate::agent_hub::portable_inventory::{
    PortableAssetKind, PortableInventoryItemDto, PortableInventoryManagementState,
    PortableInventoryMutationCapability, PortableInventorySnapshotDto,
};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::agent_hub_repo::AgentHubRepo;
use chrono::{Duration, Utc};
use std::collections::BTreeSet;

/// Plan TTL（分钟）。
pub const PLAN_TTL_MINUTES: i64 = 10;

/// 生成并持久化 portable 资产动作 preview plan。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认前只读计算变更；失败必须 fail-closed，禁止写盘或 silent adoption。
///
/// Code Logic（这个函数做什么）:
///     当前 inspect 尚未接线（B2/B5）；服务层应改用 `preview_portable_asset_action_with_inventory`。
pub async fn preview_portable_asset_action(
    _state: &AppState,
    _request: PreviewPortableAssetActionRequest,
) -> Result<PortableAssetActionPlanDto, AppError> {
    Err(AppError::unavailable(
        "PORTABLE_INVENTORY_INSPECT_NOT_WIRED",
    ))
}

/// 基于已提供 inventory 快照生成 preview（零写入目标文件）。
///
/// Business Logic（为什么需要这个函数）:
///     B3 单测与后续 service 共用同一 planner；snapshot 必须由调用方提供权威 inventory。
///
/// Code Logic（这个函数做什么）:
///     校验 hash/项/能力/opt-in/CLI；构建 changes；insert plan row。
pub async fn preview_portable_asset_action_with_inventory(
    repo: &AgentHubRepo,
    request: PreviewPortableAssetActionRequest,
    snapshot: &PortableInventorySnapshotDto,
    owner_fingerprint: &str,
) -> Result<PortableAssetActionPlanDto, AppError> {
    if request.inventory_snapshot_hash.trim().is_empty() {
        return Err(AppError::validation(
            "PORTABLE_ASSET_ACTION_INVENTORY_HASH_REQUIRED",
        ));
    }
    if request.inventory_item_ids.is_empty() {
        return Err(AppError::validation("PORTABLE_ASSET_ACTION_ITEMS_REQUIRED"));
    }
    if snapshot.stale {
        return Err(AppError::conflict("PORTABLE_ASSET_ACTION_INVENTORY_STALE"));
    }
    if snapshot.inventory_snapshot_hash != request.inventory_snapshot_hash {
        return Err(AppError::conflict(
            "PORTABLE_ASSET_ACTION_INVENTORY_HASH_MISMATCH",
        ));
    }

    let mut seen = BTreeSet::new();
    for id in &request.inventory_item_ids {
        if id.trim().is_empty() {
            return Err(AppError::validation("PORTABLE_ASSET_ACTION_ITEM_ID_EMPTY"));
        }
        if !seen.insert(id.clone()) {
            return Err(AppError::validation(
                "PORTABLE_ASSET_ACTION_ITEM_ID_DUPLICATE",
            ));
        }
    }

    let mut changes = Vec::with_capacity(request.inventory_item_ids.len());
    let mut plan_blocking = Vec::new();
    let mut fingerprints = BTreeSet::new();

    for item_id in &request.inventory_item_ids {
        let Some(item) = snapshot
            .items
            .iter()
            .find(|i| i.inventory_item_id == *item_id)
        else {
            plan_blocking.push(format!("PORTABLE_ASSET_ACTION_ITEM_NOT_FOUND:{item_id}"));
            changes.push(blocked_missing_change(item_id, &request));
            continue;
        };
        let target_dto = snapshot.targets.iter().find(|t| t.target == item.target);
        if let Some(t) = target_dto {
            fingerprints.insert(target_fingerprint(t));
            if t.mutation_capability == PortableInventoryMutationCapability::Blocked {
                plan_blocking.push(format!(
                    "PORTABLE_ASSET_ACTION_TARGET_MUTATION_BLOCKED:{}",
                    item.target.as_str()
                ));
            }
            if !t.installed {
                plan_blocking.push(format!(
                    "PORTABLE_ASSET_ACTION_CLI_MISSING:{}",
                    item.target.as_str()
                ));
            }
        } else {
            plan_blocking.push(format!(
                "PORTABLE_ASSET_ACTION_TARGET_MISSING:{}",
                item.target.as_str()
            ));
        }

        let (change, mut reasons) = build_change(item, target_dto, &request).await?;
        plan_blocking.append(&mut reasons);
        changes.push(change);
    }

    plan_blocking.sort();
    plan_blocking.dedup();

    let expires_at = (Utc::now() + Duration::minutes(PLAN_TTL_MINUTES)).to_rfc3339();
    let plan_token = uuid::Uuid::new_v4().to_string();
    let public = PortableAssetActionPlanDto {
        plan_token: plan_token.clone(),
        expires_at: expires_at.clone(),
        inventory_snapshot_hash: request.inventory_snapshot_hash.clone(),
        action: request.action,
        keep_data: request.keep_data,
        conflict_policy: request.conflict_policy,
        changes,
        blocking_reasons: plan_blocking,
    };

    let stored = StoredPortableAssetActionPlan {
        public: public.clone(),
        request: request.clone(),
        owner_fingerprint: owner_fingerprint.to_string(),
        target_fingerprints: fingerprints.into_iter().collect(),
    };
    let plan_json = serde_json::to_string(&stored)?;
    repo.insert_portable_asset_action_plan(PortableAssetActionPlanRecord {
        plan_token,
        owner_fingerprint: owner_fingerprint.to_string(),
        expires_at,
        inventory_snapshot_hash: public.inventory_snapshot_hash.clone(),
        plan_json,
        client_request_id: None,
        claimed_at: None,
        consumed_at: None,
        result_json: None,
        created_at: Utc::now().to_rfc3339(),
    })
    .await?;
    Ok(public)
}

fn target_fingerprint(
    t: &crate::agent_hub::portable_inventory::PortableInventoryTargetDto,
) -> String {
    format!(
        "{}|{}|{}|{}",
        t.target.as_str(),
        t.version.as_deref().unwrap_or(""),
        t.executable.as_deref().unwrap_or(""),
        t.config_root
    )
}

fn blocked_missing_change(
    item_id: &str,
    request: &PreviewPortableAssetActionRequest,
) -> PortableAssetActionChangeDto {
    PortableAssetActionChangeDto {
        inventory_item_id: item_id.to_string(),
        target: crate::agent_hub::models::AgentTarget::Claude,
        kind: PortableAssetKind::Skill,
        path: None,
        operation: PortableAssetPlanOperation::Leave,
        expected_source_hash: None,
        expected_tree_hash: None,
        expected_canonical_revision_id: request.expected_canonical_revision_id.clone(),
        backup_policy: PortableAssetBackupPolicy::None,
        creates_ownership: false,
        canonical_effect: PortableAssetCanonicalEffect::None,
        blocking_reasons: vec![format!("PORTABLE_ASSET_ACTION_ITEM_NOT_FOUND:{item_id}")],
        warnings: vec![],
    }
}

async fn build_change(
    item: &PortableInventoryItemDto,
    target_dto: Option<&crate::agent_hub::portable_inventory::PortableInventoryTargetDto>,
    request: &PreviewPortableAssetActionRequest,
) -> Result<(PortableAssetActionChangeDto, Vec<String>), AppError> {
    let mut blocking = Vec::new();

    // project scope opt-in
    if item.scope_kind == crate::agent_hub::models::ScopeKind::Project && !item.project_opted_in {
        blocking.push("PORTABLE_ASSET_ACTION_PROJECT_NOT_OPTED_IN".into());
    }

    // management / mapping / source / capability gates
    match item.management_state {
        PortableInventoryManagementState::ExternalCollision => {
            blocking.push("PORTABLE_ASSET_ACTION_EXTERNAL_COLLISION".into());
        }
        PortableInventoryManagementState::Unsupported => {
            blocking.push("PORTABLE_ASSET_ACTION_UNSUPPORTED_MANAGEMENT".into());
        }
        PortableInventoryManagementState::Drifted
            if request.action != PortableAssetActionKind::Adopt =>
        {
            // 漂移下非 adopt 的 mutation fail-closed（需 re-inspect）
            blocking.push("PORTABLE_ASSET_ACTION_SOURCE_DRIFTED".into());
        }
        _ => {}
    }

    if item.content_hash.is_none()
        && item.tree_hash.is_none()
        && matches!(
            request.action,
            PortableAssetActionKind::Enable
                | PortableAssetActionKind::Disable
                | PortableAssetActionKind::Uninstall
                | PortableAssetActionKind::Adopt
        )
    {
        blocking.push("PORTABLE_ASSET_ACTION_SOURCE_HASH_MISSING".into());
    }

    if let Some(expected) = request.expected_canonical_revision_id.as_deref() {
        match item.canonical_revision_id.as_deref() {
            Some(actual) if actual == expected => {}
            Some(_) => blocking.push("PORTABLE_ASSET_ACTION_CANONICAL_REVISION_MISMATCH".into()),
            None if request.action != PortableAssetActionKind::Adopt => {
                blocking.push("PORTABLE_ASSET_ACTION_CANONICAL_REVISION_MISSING".into());
            }
            None => {}
        }
    }

    if let Some(t) = target_dto {
        if t.mutation_capability == PortableInventoryMutationCapability::Blocked {
            blocking.push("PORTABLE_ASSET_ACTION_MUTATION_BLOCKED".into());
        }
        if t.mutation_capability == PortableInventoryMutationCapability::PreviewOnly
            && matches!(
                request.action,
                PortableAssetActionKind::Enable
                    | PortableAssetActionKind::Disable
                    | PortableAssetActionKind::Uninstall
                    | PortableAssetActionKind::InstallToSourceTarget
                    | PortableAssetActionKind::Adopt
            )
        {
            // preview 仍可生成计划，但标记 blocked 供 apply fail-closed
            blocking.push("PORTABLE_ASSET_ACTION_MUTATION_PREVIEW_ONLY".into());
        }
        if !t.installed {
            blocking.push("PORTABLE_ASSET_ACTION_CLI_NOT_INSTALLED".into());
        }
    } else {
        blocking.push("PORTABLE_ASSET_ACTION_CLI_FINGERPRINT_MISSING".into());
    }

    // capability gates
    match request.action {
        PortableAssetActionKind::Enable if !item.capabilities.can_enable => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_ENABLE".into());
        }
        PortableAssetActionKind::Disable if !item.capabilities.can_disable => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_DISABLE".into());
        }
        PortableAssetActionKind::Uninstall if !item.capabilities.can_uninstall => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_UNINSTALL".into());
        }
        PortableAssetActionKind::Adopt if !item.capabilities.can_adopt => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_ADOPT".into());
        }
        PortableAssetActionKind::InstallToSourceTarget
            if !item.capabilities.can_install_to_source_target =>
        {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_INSTALL_TO_SOURCE".into());
        }
        _ => {}
    }

    let unmanaged = item.management_state == PortableInventoryManagementState::Unmanaged;
    let (operation, creates_ownership, canonical_effect, backup_policy) = match request.action {
        PortableAssetActionKind::Enable => (
            PortableAssetPlanOperation::Enable,
            false,
            if unmanaged {
                PortableAssetCanonicalEffect::None
            } else {
                PortableAssetCanonicalEffect::UpdateDesired
            },
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::Disable => (
            PortableAssetPlanOperation::Disable,
            false,
            if unmanaged {
                PortableAssetCanonicalEffect::None
            } else {
                PortableAssetCanonicalEffect::UpdateDesired
            },
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::Uninstall => {
            let backup = match item.kind {
                PortableAssetKind::Skill | PortableAssetKind::Command if !request.keep_data => {
                    PortableAssetBackupPolicy::RecoverableBeforeDelete
                }
                _ => PortableAssetBackupPolicy::None,
            };
            let effect = if unmanaged {
                PortableAssetCanonicalEffect::None
            } else if item.kind == PortableAssetKind::Plugin {
                PortableAssetCanonicalEffect::TombstoneComponents
            } else {
                // 单目标 uninstall 不删 canonical
                PortableAssetCanonicalEffect::UpdateDesired
            };
            (PortableAssetPlanOperation::Uninstall, false, effect, backup)
        }
        PortableAssetActionKind::Adopt => (
            PortableAssetPlanOperation::Adopt,
            true,
            PortableAssetCanonicalEffect::CreateOwnership,
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::InstallToSourceTarget => (
            PortableAssetPlanOperation::Install,
            false,
            PortableAssetCanonicalEffect::UpdateDesired,
            PortableAssetBackupPolicy::None,
        ),
    };

    // 未纳管动作不得创建 ownership（仅 adopt 例外，上面已设 true）
    debug_assert!(creates_ownership == (request.action == PortableAssetActionKind::Adopt));
    if unmanaged
        && request.action != PortableAssetActionKind::Adopt
        && canonical_effect == PortableAssetCanonicalEffect::CreateOwnership
    {
        blocking.push("PORTABLE_ASSET_ACTION_UNMANAGED_OWNERSHIP_FORBIDDEN".into());
    }

    // MCP expected_source_hash 必须与 config_patch leaf value_content_hash 同域。
    // Skill/Command content_hash 已是 inventory 语义（Skill=SKILL.md-only）。
    let (expected_source_hash, expected_tree_hash) = match item.kind {
        PortableAssetKind::Mcp => (
            mcp_expected_leaf_hash(item).or_else(|| item.content_hash.clone()),
            item.tree_hash.clone(),
        ),
        PortableAssetKind::Plugin if item.tree_hash.is_none() => {
            let root: std::path::PathBuf = item
                .source_path
                .as_deref()
                .ok_or_else(|| AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"))?
                .into();
            let (content_hash, tree_hash) = tokio::task::spawn_blocking(move || {
                crate::agent_hub::portable_inventory::hash_plugin_root(&root)
            })
            .await
            .map_err(|error| {
                AppError::generic(format!("portable plugin preview hash: {error}"))
            })??;
            (Some(content_hash), Some(tree_hash))
        }
        PortableAssetKind::Skill if item.tree_hash.is_none() => {
            let root: std::path::PathBuf = item
                .source_path
                .as_deref()
                .ok_or_else(|| AppError::not_found("PORTABLE_ASSET_ACTION_SOURCE_MISSING"))?
                .into();
            let (content_hash, tree_hash, _, _) = tokio::task::spawn_blocking(move || {
                crate::agent_hub::targets::portable::hash_skill_directory(&root)
            })
            .await
            .map_err(|error| {
                AppError::generic(format!("portable skill preview hash: {error}"))
            })??;
            (Some(content_hash), Some(tree_hash))
        }
        _ => (item.content_hash.clone(), item.tree_hash.clone()),
    };

    let change = PortableAssetActionChangeDto {
        inventory_item_id: item.inventory_item_id.clone(),
        target: item.target,
        kind: item.kind,
        path: item.source_path.clone(),
        operation,
        expected_source_hash,
        expected_tree_hash,
        expected_canonical_revision_id: request
            .expected_canonical_revision_id
            .clone()
            .or_else(|| item.canonical_revision_id.clone()),
        backup_policy,
        creates_ownership,
        canonical_effect,
        blocking_reasons: blocking.clone(),
        warnings: item.warnings.clone(),
    };
    Ok((change, blocking))
}

/// 读取 MCP leaf 的 value_content_hash（与 apply CAS 对齐）。
///
/// Business Logic: planner 必须绑定语义 leaf，而非整文件 raw sha。
/// Code Logic: 读 source_path JSONC/JSON 的 mcpServers[id] → value_content_hash。
fn mcp_expected_leaf_hash(item: &PortableInventoryItemDto) -> Option<String> {
    use crate::agent_hub::config_patch::{
        value_content_hash, JsoncConfigPatcher, SemanticConfigPatcher,
    };
    let path = item.source_path.as_deref()?;
    let bytes = std::fs::read(path).ok()?;
    let patcher = JsoncConfigPatcher;
    let owned = patcher
        .inspect(&bytes, &["mcpServers".into(), item.native_id.clone()])
        .ok()?;
    if owned.present {
        Some(value_content_hash(&owned.value))
    } else {
        None
    }
}
