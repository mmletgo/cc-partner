//! portable_actions/targets/opencode — OpenCode / Grok / Gemini / Cursor / Pi
//!
//! Business Logic（为什么需要这个模块）:
//!     这些 target 的 CLI 写能力尚未认证，启停/卸载必须 fail-closed 且零 spawn。
//!     Skill/Command 仓库软链只改本机文件，不依赖 CLI，必须能附加/从此 Agent 卸下。
//!
//! Code Logic（这个模块做什么）:
//!     store 动作走 `execute_skill_or_command_store`；其余仍返回 write-not-certified。

use super::{TargetActionContext, TargetActionExecutor, TargetActionRawOutcome};
use crate::agent_hub::models::AgentTarget;
use crate::agent_hub::portable_actions::models::{
    PortableAssetActionChangeDto, PortableAssetActionKind, PortableAssetActionPlanDto,
};
use crate::agent_hub::portable_inventory::{PortableAssetKind, PortableInventoryItemDto};
use crate::agent_hub::portable_store::{
    current_portable_store_root, execute_skill_or_command_store, is_under_portable_store,
};
use crate::agent_hub::targets::paths::{TargetEnvironment, TargetHomes, TargetPathResolver};
use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// OpenCode 及共用 fail-closed executor（Grok/Gemini/Cursor/Pi 也走这里）。
pub struct OpenCodeTargetExecutor;

impl TargetActionExecutor for OpenCodeTargetExecutor {
    fn execute_change(
        &self,
        ctx: &TargetActionContext,
        _plan: &PortableAssetActionPlanDto,
        change: &PortableAssetActionChangeDto,
        pre_item: Option<&PortableInventoryItemDto>,
    ) -> Result<TargetActionRawOutcome, AppError> {
        if !change.blocking_reasons.is_empty() {
            return Ok(TargetActionRawOutcome::Blocked {
                code: change
                    .blocking_reasons
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "PORTABLE_ASSET_ACTION_BLOCKED".into()),
                message: "plan change blocked".into(),
            });
        }
        if ctx.action.is_portable_store_action()
            && matches!(
                change.kind,
                PortableAssetKind::Skill | PortableAssetKind::Command
            )
        {
            let id = native_id(change, pre_item);
            let native_path = native_store_mount(change.target, change.kind, &id, change);
            return execute_skill_or_command_store(
                change.target,
                ctx.action,
                change.kind,
                &id,
                &native_path,
                pre_item,
            );
        }
        Ok(TargetActionRawOutcome::Blocked {
            code: "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED".into(),
            message: "opencode portable mutation blocked until manifest evidence allows".into(),
        })
    }
}

/// 解析 viewing Agent 上应挂/拆的 native 路径。
///
/// Business Logic: 已附加项用库存观测路径；未附加的仓库真树不得当成挂载点。
/// Code Logic: change.path 不在 portable-store 内则用之，否则拼 config_root/skills|commands。
fn native_store_mount(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    change: &PortableAssetActionChangeDto,
) -> PathBuf {
    if let Some(path) = change.path.as_deref().map(Path::new) {
        // 已附加软链 canonicalize 会走进 store 真树；软链本身才是要拆的挂载点。
        if fs::symlink_metadata(path)
            .ok()
            .is_some_and(|m| m.file_type().is_symlink())
        {
            return path.to_path_buf();
        }
        let under_store =
            current_portable_store_root().is_some_and(|root| is_under_portable_store(path, &root));
        if !under_store {
            return path.to_path_buf();
        }
    }
    native_mount_from_homes(target, kind, native_id)
}

/// 按 target 配置根拼 native skills/commands 挂载点。
fn native_mount_from_homes(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
) -> PathBuf {
    let homes = TargetPathResolver::resolve_all(&TargetEnvironment::from_process());
    let root = config_root_for(target, &homes);
    match kind {
        PortableAssetKind::Command => root.join("commands").join(format!("{native_id}.md")),
        _ => root.join("skills").join(native_id),
    }
}

fn config_root_for(target: AgentTarget, homes: &TargetHomes) -> PathBuf {
    match target {
        AgentTarget::Claude => homes.claude.config_root.clone(),
        AgentTarget::Codex => homes.codex.config_root.clone(),
        AgentTarget::OpenCode => homes.opencode.config_root.clone(),
        AgentTarget::Grok => homes.grok.config_root.clone(),
        AgentTarget::Gemini => homes.gemini.config_root.clone(),
        AgentTarget::Cursor => homes.cursor.config_root.clone(),
        AgentTarget::Pi => homes.pi.config_root.clone(),
    }
}

fn native_id(
    change: &PortableAssetActionChangeDto,
    pre_item: Option<&PortableInventoryItemDto>,
) -> String {
    pre_item
        .map(|i| i.native_id.clone())
        .or_else(|| {
            change.path.as_ref().and_then(|p| {
                Path::new(p)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
        })
        .unwrap_or_else(|| change.inventory_item_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::ScopeKind;
    use crate::agent_hub::packages::activator::FakeProcessRunner;
    use crate::agent_hub::portable_actions::models::{
        PortableAssetBackupPolicy, PortableAssetCanonicalEffect, PortableAssetPlanOperation,
    };
    use crate::agent_hub::portable_inventory::{
        PortableAssetOwner, PortableInventoryItemCapabilitiesDto, PortableInventoryManagementState,
        PortableInventorySourceOrigin, PortableOriginKind, PortableStoreFactDto,
    };
    use crate::agent_hub::portable_store::{
        attach_store_link, ensure_portable_store_layout, portable_store_root, store_skill_dir,
    };
    use std::sync::Arc;

    fn dummy_plan() -> PortableAssetActionPlanDto {
        PortableAssetActionPlanDto {
            plan_token: "tok".into(),
            expires_at: "now".into(),
            inventory_snapshot_hash: "hash".into(),
            action: PortableAssetActionKind::Detach,
            keep_data: false,
            conflict_policy:
                crate::agent_hub::portable_actions::models::PortableAssetConflictPolicy::SkipExisting,
            changes: vec![],
            blocking_reasons: vec![],
        }
    }

    fn sample_item(path: &str, attached: bool) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: "opencode-skill-media-use".into(),
            target: AgentTarget::OpenCode,
            loaded_by: AgentTarget::OpenCode,
            owned_by: PortableAssetOwner::PortableStore,
            origin_kind: PortableOriginKind::Native,
            native_output_candidate: true,
            kind: PortableAssetKind::Skill,
            native_id: "media-use".into(),
            display_name: "media-use".into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            source_path: Some(path.into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("h".into()),
            tree_hash: None,
            canonical_asset_id: None,
            canonical_revision_id: None,
            management_state: PortableInventoryManagementState::HubManaged,
            desired_presence: None,
            desired_enabled: None,
            materialization_status: None,
            capabilities: PortableInventoryItemCapabilitiesDto {
                can_detach: true,
                ..PortableInventoryItemCapabilitiesDto::default()
            },
            warnings: vec![],
            mcp_credential: None,
            store: PortableStoreFactDto {
                store_id: Some("skill:media-use".into()),
                store_attached: attached,
                loaded_via_other_path: false,
                loaded_via_target: None,
            },
        }
    }

    /// Business Logic: OpenCode 仓库项必须能从此 Agent 卸下，不得被 CLI 未认证挡住。
    /// Code Logic: 在 opencode skills 根建 store 软链，Detach 后只剩仓库真树。
    #[test]
    fn detach_store_skill_unlinks_opencode_native_without_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        let oc = tmp.path().join("opencode");
        std::env::set_var("CC_PARTNER_DATA_DIR", &data);
        std::env::set_var("OPENCODE_CONFIG_DIR", &oc);
        let store_root = ensure_portable_store_layout(&data).unwrap();
        let store_tree = store_skill_dir(&store_root, "media-use");
        std::fs::create_dir_all(&store_tree).unwrap();
        std::fs::write(store_tree.join("SKILL.md"), "---\nname: media-use\n---\n").unwrap();
        let native = oc.join("skills/media-use");
        attach_store_link(&store_tree, &native).unwrap();
        assert!(std::fs::symlink_metadata(&native)
            .unwrap()
            .file_type()
            .is_symlink());

        let item = sample_item(native.to_str().unwrap(), true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: AgentTarget::OpenCode,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Detach,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Detach,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: Some(data.clone()),
        };
        let out = OpenCodeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(
            matches!(out, TargetActionRawOutcome::Applied),
            "expected Applied, got {out:?}"
        );
        assert!(!native.exists(), "OpenCode native symlink must be removed");
        assert!(
            store_tree.join("SKILL.md").is_file(),
            "store tree must remain"
        );
        std::env::remove_var("CC_PARTNER_DATA_DIR");
        std::env::remove_var("OPENCODE_CONFIG_DIR");
        let _ = portable_store_root(&data);
    }

    /// Business Logic: 启停仍要求 CLI 写认证，不得假装成功。
    #[test]
    fn disable_stays_blocked_without_cli_certification() {
        let item = sample_item("/tmp/opencode/skills/media-use", true);
        let change = PortableAssetActionChangeDto {
            inventory_item_id: item.inventory_item_id.clone(),
            target: AgentTarget::OpenCode,
            kind: PortableAssetKind::Skill,
            path: item.source_path.clone(),
            operation: PortableAssetPlanOperation::Disable,
            expected_source_hash: None,
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let ctx = TargetActionContext {
            action: PortableAssetActionKind::Disable,
            keep_data: false,
            runner: Arc::new(FakeProcessRunner::new()),
            claude_config_dir: None,
            data_dir: None,
        };
        let out = OpenCodeTargetExecutor
            .execute_change(&ctx, &dummy_plan(), &change, Some(&item))
            .unwrap();
        assert!(matches!(
            out,
            TargetActionRawOutcome::Blocked { ref code, .. }
                if code == "PORTABLE_ASSET_ACTION_TARGET_WRITE_NOT_CERTIFIED"
        ));
    }
}
