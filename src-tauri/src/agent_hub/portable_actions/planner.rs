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
use crate::agent_hub::models::{AgentTarget, PortableAssetActionPlanRecord};
use crate::agent_hub::portable_actions::targets::supports_direct_local_action;
use crate::agent_hub::portable_inventory::{
    PortableAssetKind, PortableInventoryItemDto, PortableInventoryManagementState,
    PortableInventoryMutationCapability, PortableInventorySnapshotDto,
};
use crate::agent_hub::portable_store::{classify_store_link, StoreLinkClass};
use crate::agent_hub::targets::portable::{
    is_borrowed_runtime_origin, mutation_target_for_action, mutation_target_for_origin,
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
        }
        let enablement_action = matches!(
            request.action,
            PortableAssetActionKind::Enable
                | PortableAssetActionKind::Disable
                | PortableAssetActionKind::Attach
                | PortableAssetActionKind::Detach
                | PortableAssetActionKind::MigrateToStore
                | PortableAssetActionKind::DestroyStore
        );
        if let Some(code) = portable_store_kind_block(item.kind, request.action) {
            plan_blocking.push(code.into());
        }
        let mutation_target = if request.action.is_hub_ledger_only() {
            item.target
        } else {
            mutation_target_for_action(
                item.target,
                item.owned_by,
                item.native_output_candidate,
                item.kind.to_asset_kind(),
                enablement_action,
            )
        };
        let owner_target =
            mutation_target_for_origin(item.target, item.owned_by, item.native_output_candidate);
        let owner_dto = snapshot.targets.iter().find(|t| t.target == owner_target);
        let borrowed = is_borrowed_runtime_origin(
            item.target,
            item.owned_by,
            item.native_output_candidate,
            item.origin_kind,
        );
        // Plugin 启停跟当前 Agent 的标记；卸载才看所有者 CLI。
        let gate_dto = if borrowed && !enablement_action {
            owner_dto.or(target_dto)
        } else {
            target_dto
        };

        let (change, mut reasons) =
            build_change(item, gate_dto, mutation_target, borrowed, &request).await?;
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

/// MCP/Plugin 不得走 store attach/migrate/destroy。
fn portable_store_kind_block(
    kind: PortableAssetKind,
    action: PortableAssetActionKind,
) -> Option<&'static str> {
    if !action.is_portable_store_action() || kind.supports_portable_store() {
        return None;
    }
    Some(if kind == PortableAssetKind::Mcp {
        "PORTABLE_ASSET_ACTION_MCP_STORE_UNSUPPORTED"
    } else {
        "PORTABLE_ASSET_ACTION_PLUGIN_STORE_UNSUPPORTED"
    })
}

/// 确认当前版本与逃逸软链不得 live 跟随目录树。
///
/// Business Logic: inventory 延迟 tree hash 只给会写盘的动作展开；确认版本只记库存观测值。
/// Code Logic: hub-ledger-only / `source_blocked` / `store_symlink_escape` / EscapeLink 跳过。
fn skip_live_source_tree_hash(
    action: PortableAssetActionKind,
    item: &PortableInventoryItemDto,
) -> bool {
    if action.is_hub_ledger_only() {
        return true;
    }
    if item
        .warnings
        .iter()
        .any(|warning| warning == "store_symlink_escape" || warning == "source_blocked")
    {
        return true;
    }
    item.source_path.as_deref().is_some_and(|path| {
        matches!(
            classify_store_link(std::path::Path::new(path)),
            StoreLinkClass::EscapeLink
        )
    })
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
    mutation_target: AgentTarget,
    borrowed: bool,
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
            if request.action != PortableAssetActionKind::Adopt
                && !request.action.is_hub_ledger_only() =>
        {
            // 漂移下非 adopt / 确认当前版本 的 mutation fail-closed
            blocking.push("PORTABLE_ASSET_ACTION_SOURCE_DRIFTED".into());
        }
        _ => {}
    }

    if request.action.is_hub_ledger_only()
        && item.management_state != PortableInventoryManagementState::Drifted
    {
        blocking.push("PORTABLE_ASSET_ACTION_NOT_DRIFTED".into());
    }
    if request.action.is_hub_ledger_only() && item.canonical_asset_id.is_none() {
        blocking.push("PORTABLE_ASSET_ACTION_CANONICAL_MISSING".into());
    }

    if item.content_hash.is_none()
        && item.tree_hash.is_none()
        && matches!(
            request.action,
            PortableAssetActionKind::Enable
                | PortableAssetActionKind::Disable
                | PortableAssetActionKind::Uninstall
                | PortableAssetActionKind::Adopt
                | PortableAssetActionKind::Attach
                | PortableAssetActionKind::Detach
                | PortableAssetActionKind::DestroyStore
                | PortableAssetActionKind::MigrateToStore
                | PortableAssetActionKind::ConfirmCurrentVersion
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

    let apply_target_cli_gates = !(request.action.is_hub_ledger_only()
        || (borrowed && target_dto.map(|t| t.target) != Some(mutation_target)));
    if apply_target_cli_gates {
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
                        | PortableAssetActionKind::Attach
                        | PortableAssetActionKind::Detach
                        | PortableAssetActionKind::DestroyStore
                        | PortableAssetActionKind::MigrateToStore
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
    } else if !request.action.is_hub_ledger_only()
        && !supports_direct_local_action(mutation_target, item.kind, request.action)
        && matches!(
            request.action,
            PortableAssetActionKind::Enable
                | PortableAssetActionKind::Disable
                | PortableAssetActionKind::Uninstall
                | PortableAssetActionKind::Attach
                | PortableAssetActionKind::Detach
                | PortableAssetActionKind::DestroyStore
                | PortableAssetActionKind::MigrateToStore
        )
    {
        blocking.push("PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into());
    }

    // capability gates
    match request.action {
        PortableAssetActionKind::Enable if !item.capabilities.can_enable => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_ENABLE".into());
        }
        PortableAssetActionKind::Disable if !item.capabilities.can_disable => {
            if item.kind == PortableAssetKind::Plugin
                && item.capabilities.reason_code.as_deref()
                    == Some("deactivate_package_not_supported")
            {
                blocking.push("PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED".into());
            }
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_DISABLE".into());
        }
        PortableAssetActionKind::Uninstall if !item.capabilities.can_uninstall => {
            if item.kind == PortableAssetKind::Plugin
                && item.capabilities.reason_code.as_deref()
                    == Some("deactivate_package_not_supported")
            {
                blocking.push("PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED".into());
            }
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
        PortableAssetActionKind::Attach if !item.capabilities.can_attach => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_ATTACH".into());
        }
        PortableAssetActionKind::Detach if !item.capabilities.can_detach => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_DETACH".into());
        }
        PortableAssetActionKind::DestroyStore if !item.capabilities.can_destroy_store => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_DESTROY_STORE".into());
        }
        PortableAssetActionKind::MigrateToStore if !item.capabilities.can_migrate_to_store => {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_MIGRATE_TO_STORE".into());
        }
        PortableAssetActionKind::ConfirmCurrentVersion
            if !item.capabilities.can_confirm_current_version =>
        {
            blocking.push("PORTABLE_ASSET_ACTION_CANNOT_CONFIRM_CURRENT_VERSION".into());
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
        PortableAssetActionKind::Attach => (
            PortableAssetPlanOperation::Attach,
            false,
            PortableAssetCanonicalEffect::None,
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::Detach => (
            PortableAssetPlanOperation::Detach,
            false,
            PortableAssetCanonicalEffect::None,
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::DestroyStore => (
            PortableAssetPlanOperation::DestroyStore,
            false,
            PortableAssetCanonicalEffect::None,
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::MigrateToStore => (
            PortableAssetPlanOperation::MigrateToStore,
            false,
            PortableAssetCanonicalEffect::None,
            PortableAssetBackupPolicy::None,
        ),
        PortableAssetActionKind::ConfirmCurrentVersion => (
            PortableAssetPlanOperation::ConfirmCurrentVersion,
            false,
            PortableAssetCanonicalEffect::None,
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
    // 确认当前版本 / 逃逸软链不得 live 跟随目录，否则 preview 哈希与库存观测值分域。
    let skip_live_tree = skip_live_source_tree_hash(request.action, item);
    let (expected_source_hash, expected_tree_hash) = match item.kind {
        PortableAssetKind::Mcp => (
            mcp_expected_leaf_hash(item).or_else(|| item.content_hash.clone()),
            item.tree_hash.clone(),
        ),
        PortableAssetKind::Plugin if item.tree_hash.is_none() && !skip_live_tree => {
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
        PortableAssetKind::Skill if item.tree_hash.is_none() && !skip_live_tree => {
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

    let mut warnings = item.warnings.clone();
    if borrowed && !warnings.iter().any(|w| w == "borrowed_runtime_origin") {
        warnings.push("borrowed_runtime_origin".into());
    }

    let change = PortableAssetActionChangeDto {
        inventory_item_id: item.inventory_item_id.clone(),
        target: mutation_target,
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
        warnings,
    };
    Ok((change, blocking))
}

/// 读取 MCP leaf 的 value_content_hash（与 apply CAS 对齐）。
///
/// Business Logic: planner 必须绑定语义 leaf，而非整文件 raw sha。
/// Code Logic: Claude/OpenCode → JSONC `mcpServers`；Codex → TOML `mcp_servers`
/// （与 target executor 的 patcher/path 同域，含 int 等完整字段）。
fn mcp_expected_leaf_hash(item: &PortableInventoryItemDto) -> Option<String> {
    use crate::agent_hub::config_patch::{
        value_content_hash, JsoncConfigPatcher, SemanticConfigPatcher, TomlConfigPatcher,
    };
    let path = item.source_path.as_deref()?;
    let bytes = std::fs::read(path).ok()?;
    let hash_target =
        mutation_target_for_origin(item.target, item.owned_by, item.native_output_candidate);
    let owned = match hash_target {
        AgentTarget::Codex | AgentTarget::Grok => TomlConfigPatcher
            .inspect(&bytes, &["mcp_servers".into(), item.native_id.clone()])
            .ok()?,
        AgentTarget::Claude | AgentTarget::OpenCode | AgentTarget::Gemini | AgentTarget::Cursor => {
            JsoncConfigPatcher
                .inspect(&bytes, &["mcpServers".into(), item.native_id.clone()])
                .ok()?
        }
        AgentTarget::Pi => return None,
    };
    if owned.present {
        Some(value_content_hash(&owned.value))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::config_patch::{
        value_content_hash, SemanticConfigPatcher, TomlConfigPatcher,
    };
    use crate::agent_hub::models::{AgentTarget, ScopeKind};
    use crate::agent_hub::portable_actions::models::PortableAssetConflictPolicy;
    use crate::agent_hub::portable_inventory::{
        inventory_item_id, PortableAssetOwner, PortableInventoryItemCapabilitiesDto,
        PortableInventoryMutationCapability, PortableInventoryScanCapability,
        PortableInventorySourceOrigin, PortableInventoryTargetDto, PortableOriginKind,
    };
    use std::fs;

    /// Codex MCP expected hash 必须与 apply 侧 Toml leaf inspect 同域（含 int 字段）。
    #[test]
    fn codex_mcp_expected_leaf_hash_matches_toml_inspect_with_int_fields() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(
            &config,
            r#"
[mcp_servers.node_repl]
command = "node"
startup_timeout_sec = 120
args = ["mcp"]
"#,
        )
        .unwrap();
        let item = PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(
                AgentTarget::Codex,
                "user",
                &config.display().to_string(),
                "node_repl",
            ),
            target: AgentTarget::Codex,
            loaded_by: AgentTarget::Codex,
            owned_by: PortableAssetOwner::Codex,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Mcp,
            native_id: "node_repl".into(),
            display_name: "node_repl".into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(config.display().to_string()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            // 故意放一个与 TOML 真值不同的扫描 hash，验证 planner 不得回落到它
            content_hash: Some("stale-scan-hash-without-int".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::HubManaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto::default(),
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        };
        let got = mcp_expected_leaf_hash(&item).expect("codex mcp leaf hash");
        let bytes = fs::read(&config).unwrap();
        let owned = TomlConfigPatcher
            .inspect(&bytes, &["mcp_servers".into(), "node_repl".into()])
            .expect("toml inspect");
        assert!(owned.present);
        assert_eq!(got, owned.value_hash.expect("value hash"));
        assert_eq!(got, value_content_hash(&owned.value));
        assert_ne!(got, "stale-scan-hash-without-int");
    }

    /// Claude MCP 仍走 JSONC `mcpServers` 路径。
    #[test]
    fn claude_mcp_expected_leaf_hash_uses_jsonc_mcp_servers() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = tmp.path().join("settings.json");
        fs::write(
            &config,
            r#"{"mcpServers":{"ctx":{"type":"stdio","command":"npx","args":["-y","x"]}}}"#,
        )
        .unwrap();
        let item = PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(
                AgentTarget::Claude,
                "user",
                &config.display().to_string(),
                "ctx",
            ),
            target: AgentTarget::Claude,
            loaded_by: AgentTarget::Claude,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Mcp,
            native_id: "ctx".into(),
            display_name: "ctx".into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(config.display().to_string()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: None,
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::HubManaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto::default(),
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        };
        let got = mcp_expected_leaf_hash(&item).expect("claude mcp leaf hash");
        let bytes = fs::read(&config).unwrap();
        let owned = crate::agent_hub::config_patch::JsoncConfigPatcher
            .inspect(&bytes, &["mcpServers".into(), "ctx".into()])
            .expect("jsonc inspect");
        assert_eq!(got, owned.value_hash.expect("value hash"));
    }

    #[tokio::test]
    async fn partial_deactivate_capability_blocks_plugin_uninstall_plan() {
        let item = PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(
                AgentTarget::Claude,
                "user",
                "/plugins/demo",
                "demo@local",
            ),
            target: AgentTarget::Claude,
            loaded_by: AgentTarget::Claude,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Plugin,
            native_id: "demo@local".into(),
            display_name: "demo".into(),
            description: None,
            version: Some("1.0.0".into()),
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some("/plugins/demo".into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("content".into()),
            tree_hash: Some("tree".into()),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: true,
                can_disable: false,
                can_uninstall: false,
                can_adopt: false,
                can_install_to_source_target: false,
                can_migrate_to_store: false,
                can_attach: false,
                can_detach: false,
                can_destroy_store: false,
                can_confirm_current_version: false,
                reason_code: Some("deactivate_package_not_supported".into()),
                evidence_ids: vec![],
            },
            warnings: vec![],
            mcp_credential: None,
            store: Default::default(),
        };
        let target = PortableInventoryTargetDto {
            target: AgentTarget::Claude,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/claude".into()),
            config_root: "/cfg/claude".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        };
        let request = PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: "hash".into(),
            inventory_query: Default::default(),
            inventory_item_ids: vec![item.inventory_item_id.clone()],
            action: PortableAssetActionKind::Uninstall,
            keep_data: false,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        };

        let (change, reasons) =
            build_change(&item, Some(&target), AgentTarget::Claude, false, &request)
                .await
                .expect("build change");
        assert_eq!(change.operation, PortableAssetPlanOperation::Uninstall);
        assert!(reasons
            .iter()
            .any(|reason| reason == "PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED"));
        assert!(change
            .blocking_reasons
            .iter()
            .any(|reason| reason == "PORTABLE_ASSET_ACTION_CANNOT_UNINSTALL"));

        let mut unavailable = item;
        unavailable.capabilities.reason_code = Some("portable_direct_action_unavailable".into());
        let (_, reasons) = build_change(
            &unavailable,
            Some(&target),
            AgentTarget::Claude,
            false,
            &request,
        )
        .await
        .expect("build unavailable change");
        assert!(!reasons
            .iter()
            .any(|reason| reason == "PORTABLE_ASSET_ACTION_DEACTIVATE_PACKAGE_BLOCKED"));
    }

    #[tokio::test]
    async fn borrowed_plugin_disable_from_grok_does_not_target_claude() {
        let item = PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(
                AgentTarget::Grok,
                "user",
                "/claude/plugins/superpowers",
                "superpowers",
            ),
            target: AgentTarget::Grok,
            loaded_by: AgentTarget::Grok,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Compatibility,
            native_output_candidate: false,
            kind: PortableAssetKind::Plugin,
            native_id: "superpowers".into(),
            display_name: "superpowers".into(),
            description: None,
            version: Some("6.3.0".into()),
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some("/claude/plugins/superpowers".into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("content".into()),
            tree_hash: Some("tree".into()),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: false,
                can_disable: false,
                can_uninstall: true,
                can_adopt: false,
                can_install_to_source_target: false,
                can_migrate_to_store: false,
                can_attach: false,
                can_detach: false,
                can_destroy_store: false,
                can_confirm_current_version: false,
                reason_code: Some("borrowed_runtime_origin".into()),
                evidence_ids: vec![],
            },
            warnings: vec!["borrowed_runtime_origin".into()],
            mcp_credential: None,
            store: Default::default(),
        };
        let grok_target = PortableInventoryTargetDto {
            target: AgentTarget::Grok,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/grok".into()),
            config_root: "/cfg/grok".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Blocked,
            reason_code: Some("cli_version_unknown".into()),
            evidence_ids: vec![],
        };
        let request = PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: "hash".into(),
            inventory_query: Default::default(),
            inventory_item_ids: vec![item.inventory_item_id.clone()],
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        };
        let mutation_target = mutation_target_for_action(
            item.target,
            item.owned_by,
            item.native_output_candidate,
            item.kind.to_asset_kind(),
            true,
        );
        assert_eq!(mutation_target, AgentTarget::Grok);
        let (change, _) = build_change(&item, Some(&grok_target), mutation_target, true, &request)
            .await
            .expect("build grok plugin disable");
        assert_eq!(change.target, AgentTarget::Grok);
        assert_ne!(change.target, AgentTarget::Claude);
        assert_eq!(change.operation, PortableAssetPlanOperation::Disable);
    }

    #[tokio::test]
    async fn borrowed_plugin_disable_from_codex_does_not_target_claude() {
        let item = PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(
                AgentTarget::Codex,
                "user",
                "/claude/plugins/superpowers",
                "superpowers",
            ),
            target: AgentTarget::Codex,
            loaded_by: AgentTarget::Codex,
            owned_by: PortableAssetOwner::Claude,
            origin_kind: PortableOriginKind::Compatibility,
            native_output_candidate: false,
            kind: PortableAssetKind::Plugin,
            native_id: "superpowers".into(),
            display_name: "superpowers".into(),
            description: None,
            version: Some("6.3.0".into()),
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some("/claude/plugins/superpowers".into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("content".into()),
            tree_hash: Some("tree".into()),
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::Unmanaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_enable: false,
                can_disable: true,
                can_uninstall: true,
                can_adopt: false,
                can_install_to_source_target: false,
                can_migrate_to_store: false,
                can_attach: false,
                can_detach: false,
                can_destroy_store: false,
                can_confirm_current_version: false,
                reason_code: Some("borrowed_runtime_origin".into()),
                evidence_ids: vec![],
            },
            warnings: vec!["borrowed_runtime_origin".into()],
            mcp_credential: None,
            store: Default::default(),
        };
        let codex_target = PortableInventoryTargetDto {
            target: AgentTarget::Codex,
            installed: true,
            version: Some("1.0.0".into()),
            executable: Some("/bin/codex".into()),
            config_root: "/cfg/codex".into(),
            scan_capability: PortableInventoryScanCapability::Supported,
            mutation_capability: PortableInventoryMutationCapability::Supported,
            reason_code: None,
            evidence_ids: vec![],
        };
        let request = PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: "hash".into(),
            inventory_query: Default::default(),
            inventory_item_ids: vec![item.inventory_item_id.clone()],
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        };
        let mutation_target = mutation_target_for_action(
            item.target,
            item.owned_by,
            item.native_output_candidate,
            item.kind.to_asset_kind(),
            true,
        );
        assert_eq!(mutation_target, AgentTarget::Codex);
        let (change, _) = build_change(&item, Some(&codex_target), mutation_target, true, &request)
            .await
            .expect("build codex plugin disable");
        assert_eq!(change.target, AgentTarget::Codex);
        assert_ne!(change.target, AgentTarget::Claude);
    }

    #[test]
    fn mcp_store_actions_are_blocked_without_plugin_switch_semantics() {
        for action in [
            PortableAssetActionKind::Attach,
            PortableAssetActionKind::Detach,
            PortableAssetActionKind::DestroyStore,
            PortableAssetActionKind::MigrateToStore,
        ] {
            assert_eq!(
                portable_store_kind_block(PortableAssetKind::Mcp, action),
                Some("PORTABLE_ASSET_ACTION_MCP_STORE_UNSUPPORTED")
            );
            assert_eq!(
                portable_store_kind_block(PortableAssetKind::Plugin, action),
                Some("PORTABLE_ASSET_ACTION_PLUGIN_STORE_UNSUPPORTED")
            );
            assert_eq!(
                portable_store_kind_block(PortableAssetKind::Skill, action),
                None
            );
        }
        assert_eq!(
            portable_store_kind_block(PortableAssetKind::Mcp, PortableAssetActionKind::Enable),
            None
        );
    }
}
