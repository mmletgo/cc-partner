//! agent_hub/portable_actions — 本机 portable 资产动作 preview plan 与 ledger
//!
//! Business Logic（为什么需要这个模块）:
//!     所有本机 Skill/Command/Plugin/MCP mutation 必须经 preview→claim→apply→rescan；
//!     preview 零写入；未纳管启停不创建 ownership；claim 幂等与 outcomeUnknown 对账。
//!
//! Code Logic（这个模块做什么）:
//!     models + planner + ledger + executor + target adapters。

pub mod executor;
pub mod ledger;
pub mod models;
pub mod planner;
pub mod targets;

pub use executor::{
    apply_portable_asset_action, apply_portable_asset_action_with, PortableActionExecutorDeps,
};
pub use ledger::{
    claim_portable_asset_action, complete_portable_asset_action,
    get_portable_asset_action_by_request, get_portable_asset_action_plan, outcome_unknown_result,
};
pub use models::{
    ApplyPortableAssetActionRequest, PortableAssetActionChangeDto,
    PortableAssetActionItemResultDto, PortableAssetActionItemState, PortableAssetActionKind,
    PortableAssetActionPlanDto, PortableAssetActionResultDto, PortableAssetBackupPolicy,
    PortableAssetCanonicalEffect, PortableAssetConflictPolicy, PortableAssetPlanOperation,
    PreviewPortableAssetActionRequest,
};
pub use planner::{
    preview_portable_asset_action, preview_portable_asset_action_with_inventory, PLAN_TTL_MINUTES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_hub::models::{AgentTarget, PortableActionClaim, ScopeKind};
    use crate::agent_hub::portable_inventory::{
        inventory_item_id, inventory_snapshot_hash, PortableAssetKind,
        PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
        PortableInventoryManagementState, PortableInventoryMutationCapability,
        PortableInventoryScanCapability, PortableInventorySnapshotDto,
        PortableInventorySourceOrigin, PortableInventoryTargetDto,
    };
    use crate::storage::agent_hub_repo::AgentHubRepo;
    use chrono::{Duration, Utc};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

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
        native_id: &str,
        path: &str,
        management: PortableInventoryManagementState,
        opted_in: bool,
    ) -> PortableInventoryItemDto {
        PortableInventoryItemDto {
            inventory_item_id: inventory_item_id(target, "user", path, native_id),
            target,
            kind: PortableAssetKind::Skill,
            native_id: native_id.into(),
            display_name: native_id.into(),
            description: None,
            version: None,
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: opted_in,
            source_path: Some(path.into()),
            source_origin: PortableInventorySourceOrigin::Standalone,
            parent_plugin_inventory_item_id: None,
            actual_enabled: Some(true),
            content_hash: Some("content-hash".into()),
            tree_hash: Some("tree-hash".into()),
            canonical_asset_id: if management == PortableInventoryManagementState::Unmanaged {
                None
            } else {
                Some("asset-1".into())
            },
            canonical_revision_id: if management == PortableInventoryManagementState::Unmanaged {
                None
            } else {
                Some("rev-1".into())
            },
            management_state: management,
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

    fn preview_req(
        snapshot: &PortableInventorySnapshotDto,
        action: PortableAssetActionKind,
        ids: Vec<String>,
    ) -> PreviewPortableAssetActionRequest {
        PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: snapshot.inventory_snapshot_hash.clone(),
            inventory_item_ids: ids,
            action,
            keep_data: false,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        }
    }

    /// Business Logic: preview 只持久化 plan，不得写目标文件；且返回 10 分钟 TTL。
    /// Code Logic: with_inventory preview 后 plan expires_at ≈ now+10m，且 changes 非空。
    #[tokio::test]
    async fn preview_is_zero_write_and_expires_in_ten_minutes() {
        let repo = test_repo().await;
        let item = sample_item(
            AgentTarget::Claude,
            "skill-a",
            "/skills/a",
            PortableInventoryManagementState::Unmanaged,
            true,
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let before = Utc::now();
        let plan = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Enable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner-fp",
        )
        .await
        .expect("preview");
        let expires = chrono::DateTime::parse_from_rfc3339(&plan.expires_at)
            .unwrap()
            .with_timezone(&Utc);
        let delta = expires - before;
        assert!(delta >= Duration::minutes(9) && delta <= Duration::minutes(11));
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(
            plan.changes[0].canonical_effect,
            PortableAssetCanonicalEffect::None
        );
        assert!(!plan.changes[0].creates_ownership);
        // plan 行存在
        assert!(repo
            .get_portable_asset_action_plan(&plan.plan_token)
            .await
            .unwrap()
            .is_some());
    }

    /// Business Logic: 过期 inventory hash / CLI 能力阻断必须进入 blocking_reasons。
    #[tokio::test]
    async fn stale_inventory_source_cli_and_mapping_block_preview() {
        let repo = test_repo().await;
        let mut item = sample_item(
            AgentTarget::Claude,
            "skill-a",
            "/skills/a",
            PortableInventoryManagementState::Drifted,
            true,
        );
        item.content_hash = Some("old".into());
        let mut target = sample_target(AgentTarget::Claude);
        target.mutation_capability = PortableInventoryMutationCapability::Blocked;
        let snap = snapshot_from(vec![target], vec![item.clone()]);

        // hash mismatch
        let mut bad = preview_req(
            &snap,
            PortableAssetActionKind::Disable,
            vec![item.inventory_item_id.clone()],
        );
        bad.inventory_snapshot_hash = "deadbeef".into();
        let err = preview_portable_asset_action_with_inventory(&repo, bad, &snap, "owner")
            .await
            .unwrap_err();
        assert_eq!(err.ipc_category_code(), "conflict");

        // drifted + blocked mutation → plan with blocking reasons
        let plan = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Disable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        assert!(plan
            .blocking_reasons
            .iter()
            .any(|r| r.contains("SOURCE_DRIFTED") || r.contains("MUTATION_BLOCKED")));
    }

    /// Business Logic: 项目未 opt-in 不得执行目录写入意图。
    #[tokio::test]
    async fn unopted_project_blocks_action() {
        let repo = test_repo().await;
        let mut item = sample_item(
            AgentTarget::Codex,
            "proj-skill",
            "/proj/skill",
            PortableInventoryManagementState::HubManaged,
            false,
        );
        item.scope_kind = ScopeKind::Project;
        item.scope_id = "project:p1".into();
        item.project_id = Some("p1".into());
        item.project_opted_in = false;
        // recompute id with project scope
        item.inventory_item_id = inventory_item_id(
            AgentTarget::Codex,
            "project:p1",
            "/proj/skill",
            "proj-skill",
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Codex)], vec![item.clone()]);
        let plan = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Enable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        assert!(plan
            .blocking_reasons
            .iter()
            .any(|r| r == "PORTABLE_ASSET_ACTION_PROJECT_NOT_OPTED_IN"));
    }

    /// Business Logic: capability 不允许的 mutation 必须 blocked。
    #[tokio::test]
    async fn unsupported_mutation_is_blocked() {
        let repo = test_repo().await;
        let mut item = sample_item(
            AgentTarget::OpenCode,
            "skill-x",
            "/skills/x",
            PortableInventoryManagementState::Unmanaged,
            true,
        );
        item.capabilities.can_uninstall = false;
        let snap = snapshot_from(
            vec![sample_target(AgentTarget::OpenCode)],
            vec![item.clone()],
        );
        let plan = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Uninstall,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        assert!(plan
            .blocking_reasons
            .iter()
            .any(|r| r == "PORTABLE_ASSET_ACTION_CANNOT_UNINSTALL"));
        assert_eq!(
            plan.changes[0].canonical_effect,
            PortableAssetCanonicalEffect::None
        );
        assert!(!plan.changes[0].creates_ownership);
    }

    /// Business Logic: 仅 adopt 创建 ownership；unmanaged enable 不创建。
    #[tokio::test]
    async fn only_adopt_creates_ownership() {
        let repo = test_repo().await;
        let item = sample_item(
            AgentTarget::Claude,
            "skill-a",
            "/skills/a",
            PortableInventoryManagementState::Unmanaged,
            true,
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let enable = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Enable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        assert!(!enable.changes[0].creates_ownership);
        assert_eq!(
            enable.changes[0].canonical_effect,
            PortableAssetCanonicalEffect::None
        );

        let adopt = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Adopt,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        assert!(adopt.changes[0].creates_ownership);
        assert_eq!(
            adopt.changes[0].canonical_effect,
            PortableAssetCanonicalEffect::CreateOwnership
        );
    }

    /// Business Logic: 同 request claim 后完成可 replay；未完成查 outcomeUnknown；异 plan 冲突。
    #[tokio::test]
    async fn claim_replay_and_outcome_unknown_lookup() {
        let repo = test_repo().await;
        let item = sample_item(
            AgentTarget::Claude,
            "skill-a",
            "/skills/a",
            PortableInventoryManagementState::HubManaged,
            true,
        );
        let snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![item.clone()]);
        let plan = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Disable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();

        let claimed = claim_portable_asset_action(&repo, &plan.plan_token, "req-1")
            .await
            .unwrap();
        assert!(matches!(claimed, PortableActionClaim::Claimed(_)));

        // same request pending
        let pending = claim_portable_asset_action(&repo, &plan.plan_token, "req-1")
            .await
            .unwrap();
        assert_eq!(pending, PortableActionClaim::Pending);

        // lookup outcomeUnknown
        let unknown = get_portable_asset_action_by_request(&repo, "req-1")
            .await
            .unwrap();
        assert_eq!(unknown.client_request_id, "req-1");
        assert!(unknown
            .items
            .iter()
            .all(|i| i.state == PortableAssetActionItemState::OutcomeUnknown));

        // complete + replay
        let result = PortableAssetActionResultDto {
            plan_token: plan.plan_token.clone(),
            client_request_id: "req-1".into(),
            items: vec![PortableAssetActionItemResultDto {
                inventory_item_id: item.inventory_item_id.clone(),
                state: PortableAssetActionItemState::Succeeded,
                error_code: None,
                message: None,
            }],
        };
        complete_portable_asset_action(&repo, &plan.plan_token, "req-1", &result)
            .await
            .unwrap();
        let replay = claim_portable_asset_action(&repo, &plan.plan_token, "req-1")
            .await
            .unwrap();
        match replay {
            PortableActionClaim::Replay(json) => {
                let back: PortableAssetActionResultDto = serde_json::from_str(&json).unwrap();
                assert_eq!(back, result);
            }
            other => panic!("expected Replay, got {other:?}"),
        }

        // same request id cannot claim a different plan
        let plan2 = preview_portable_asset_action_with_inventory(
            &repo,
            preview_req(
                &snap,
                PortableAssetActionKind::Enable,
                vec![item.inventory_item_id.clone()],
            ),
            &snap,
            "owner",
        )
        .await
        .unwrap();
        let conflict = claim_portable_asset_action(&repo, &plan2.plan_token, "req-1")
            .await
            .unwrap_err();
        assert_eq!(conflict.ipc_category_code(), "conflict");
    }

    /// Business Logic: MCP plan/result JSON 不得包含 secret 字段。
    #[test]
    fn plan_dto_has_no_secret_fields() {
        let change = PortableAssetActionChangeDto {
            inventory_item_id: "i1".into(),
            target: AgentTarget::Claude,
            kind: PortableAssetKind::Mcp,
            path: Some("/cfg/mcp.json".into()),
            operation: PortableAssetPlanOperation::Enable,
            expected_source_hash: Some("h".into()),
            expected_tree_hash: None,
            expected_canonical_revision_id: None,
            backup_policy: PortableAssetBackupPolicy::None,
            creates_ownership: false,
            canonical_effect: PortableAssetCanonicalEffect::None,
            blocking_reasons: vec![],
            warnings: vec![],
        };
        let json = serde_json::to_string(&change).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("token"));
        assert!(!json.contains("password"));
        assert!(!json.contains("apiKey"));
    }
}
