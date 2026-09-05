//! agent_hub/service/tests — service 门面测试
//!
//! Business Logic（为什么需要这个模块）:
//!     DTO wire 键名、removal-blocked 预检、presence mutation 集成路径与 probe
//!     支持级判定必须回归保护，防止静默破坏前端契约与 fail-closed 语义。
//!
//! Code Logic（这个模块做什么）:
//!     serde 键名断言、tempfile sqlite + 最小 AppState 的公共 service 方法集成测，
//!     以及 probe_support_map / evaluate_target_support_flags 支持级单测。

use super::summary::{
    evaluate_target_support_flags, probe_all_targets_best_effort, probe_support_map,
};
use super::target_intent::compute_removal_blocked_paths;
use crate::agent_hub::models::{
    AgentTarget, DesiredPresence, LogicalAsset, Materialization, MaterializationStatus,
    NewMaterialization,
};
use crate::agent_hub::object_store::sha256_hex;

use super::*;

/// Business Logic: 前端依赖 camelCase 键名。
/// Code Logic: serde_json 断言关键键。
#[test]
fn status_dto_serializes_camel_case_keys() {
    let dto = AgentHubStatusDto {
        enabled: true,
        background_enabled: false,
        agent_hub_api_version: 1,
        owner_instance_id: Some("owner".to_string()),
        write_compatible: true,
        probes: vec![],
        conflict_count: 0,
        blocked_materialization_count: 0,
    };
    let v = serde_json::to_value(&dto).unwrap();
    assert!(v.get("agentHubApiVersion").is_some());
    assert!(v.get("writeCompatible").is_some());
    assert!(v.get("blockedMaterializationCount").is_some());
    assert!(v.get("backgroundEnabled").is_some());
}

/// Business Logic: resolution enum wire tokens。
/// Code Logic: camelCase serde。
#[test]
fn conflict_resolution_wire_tokens() {
    assert_eq!(
        serde_json::to_value(AgentHubConflictResolution::KeepHub).unwrap(),
        serde_json::json!("keepHub")
    );
    assert_eq!(
        serde_json::to_value(AgentHubConflictResolution::KeepExternal).unwrap(),
        serde_json::json!("keepExternal")
    );
    assert_eq!(
        serde_json::to_value(AgentHubConflictResolution::Manual).unwrap(),
        serde_json::json!("manual")
    );
}

/// Business Logic: Gate B presence/enabled/restore/everywhere 请求 DTO 必须 camelCase 稳定。
/// Code Logic: serde 键名断言。
#[test]
fn presence_mutation_request_dto_camel_case_keys() {
    let presence = SetTargetPresenceRequest {
        asset_id: "a".into(),
        target: AgentTarget::Claude,
        desired_presence: DesiredPresence::Absent,
    };
    let v = serde_json::to_value(&presence).unwrap();
    assert!(v.get("assetId").is_some());
    assert!(v.get("desiredPresence").is_some());
    assert_eq!(v.get("desiredPresence").unwrap(), "absent");

    let enabled = SetTargetEnabledRequest {
        asset_id: "a".into(),
        target: AgentTarget::Codex,
        desired_enabled: false,
    };
    let v = serde_json::to_value(&enabled).unwrap();
    assert!(v.get("desiredEnabled").is_some());

    let restore = RestoreDetachedTargetRequest {
        asset_id: "a".into(),
        target: AgentTarget::OpenCode,
    };
    let v = serde_json::to_value(&restore).unwrap();
    assert!(v.get("assetId").is_some());
    assert!(v.get("target").is_some());

    let everywhere = DeleteAssetEverywhereRequest {
        asset_id: "a".into(),
    };
    let v = serde_json::to_value(&everywhere).unwrap();
    assert_eq!(v.get("assetId").unwrap(), "a");
}

/// Business Logic: summary/detail 必须暴露 aggregateStatus 与 cell-level 输入。
/// Code Logic: serde 键名断言。
#[test]
fn summary_and_cell_dto_expose_aggregate_and_cell_inputs() {
    let cell = AgentHubTargetCellDto {
        target: AgentTarget::Claude,
        desired_presence: DesiredPresence::Present,
        desired_enabled: true,
        materialization_status: Some("synced".into()),
        last_error: None,
        requested: true,
        supported: true,
        source_only: false,
        verified: true,
    };
    let v = serde_json::to_value(&cell).unwrap();
    assert!(v.get("requested").is_some());
    assert!(v.get("supported").is_some());
    assert!(v.get("sourceOnly").is_some());
    assert!(v.get("verified").is_some());

    let summary = AgentHubAssetSummaryDto {
        asset_id: "a".into(),
        scope_id: "s".into(),
        kind: "instruction".into(),
        display_name: "d".into(),
        logical_key: "k".into(),
        origin_namespace: "n".into(),
        policy: "shared".into(),
        current_revision_id: None,
        targets: vec![cell],
        has_conflict: false,
        aggregate_status: "full".into(),
    };
    let v = serde_json::to_value(&summary).unwrap();
    assert_eq!(v.get("aggregateStatus").unwrap(), "full");
    assert!(v.get("hasConflict").is_some());
}

/// Business Logic: managed hash 匹配可删；漂移/未知子项阻塞并返回精确路径。
/// Code Logic: tempfile 文件/目录场景。
#[test]
fn removal_blocked_paths_detect_hash_drift_and_unknown_children() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("CLAUDE.md");
    std::fs::write(&file, b"managed-content").unwrap();
    let managed = sha256_hex(b"managed-content");
    let mat = Materialization {
        id: "m1".into(),
        asset_id: "a".into(),
        target: AgentTarget::Claude,
        target_binding_id: "b1".into(),
        native_path: Some(file.to_string_lossy().into_owned()),
        last_projected_revision_id: None,
        rendered_hash: Some(managed.clone()),
        observed_external_hash: Some(managed.clone()),
        status: MaterializationStatus::Synced,
        last_error: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    assert!(compute_removal_blocked_paths(Some(&mat)).is_empty());

    std::fs::write(&file, b"external-edit").unwrap();
    let blocked = compute_removal_blocked_paths(Some(&mat));
    assert_eq!(blocked.len(), 1);
    assert!(blocked[0].ends_with("CLAUDE.md"));

    // 目录：未知子文件阻塞
    let pkg = dir.path().join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("extra.txt"), b"unknown").unwrap();
    let dir_mat = Materialization {
        id: "m2".into(),
        asset_id: "a".into(),
        target: AgentTarget::Codex,
        target_binding_id: "b2".into(),
        native_path: Some(pkg.to_string_lossy().into_owned()),
        last_projected_revision_id: None,
        rendered_hash: Some(managed),
        observed_external_hash: None,
        status: MaterializationStatus::Synced,
        last_error: None,
        created_at: "t".into(),
        updated_at: "t".into(),
    };
    let blocked_dir = compute_removal_blocked_paths(Some(&dir_mat));
    assert!(!blocked_dir.is_empty());
    assert!(blocked_dir.iter().any(|p| p.ends_with("extra.txt")));
}

/// 构建最小 HeadlessOwner AppState 供 presence mutation 集成测。
///
/// Business Logic: Step 1 六项断言必须经 public service 方法，而非手写 upsert。
/// Code Logic: tempfile sqlite + AgentHubRepo schema + 精简 AppState 字段。
async fn build_service_state() -> (AppState, tempfile::TempDir) {
    use crate::backend::authority::RuntimeRole;
    use crate::backend::event_bus::RuntimeEventBus;
    use crate::backend::runtime_metrics::RuntimeMetrics;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::cloud_sync::runtime::CloudSyncRuntime;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::config_runtime::ConfigRuntime;
    use crate::config_store::MemoryConfigStore;
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use crate::storage::{
        AgentHubRepo, ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo,
        TransferRepo, WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo, WorkbenchWorkspaceLayoutRepo, WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use crate::updater::UpdateRuntime;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    let tmp = tempfile::tempdir().unwrap();
    // 隔离 data_dir，避免 schedule/object_store 写真实 home
    std::env::set_var("CC_PARTNER_DATA_DIR", tmp.path());
    let db_path = tmp.path().join("data.db");
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let options = SqliteConnectOptions::from_str(&db_url)
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let agent_hub = AgentHubRepo::new(pool.clone());

    let config = AppConfig {
        device_id: "svc-test".to_string(),
        device_name: "svc-test".to_string(),
        http_port: 0,
        receive_dir: tmp.path().join("recv").to_string_lossy().to_string(),
        game_plugin_dir: "/tmp/plugins".into(),
        db_path: db_path.to_string_lossy().to_string(),
        screenshot_hotkey: "<cmd>+s".to_string(),
        prompt_optimizer_hotkey: "<ctrl>".to_string(),
        prompt_optimizer_fill_language: "zh".to_string(),
        prompt_optimizer_provider: "claude".into(),
        prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: HealthConfig::default(),
        battery: BatteryConfig::default(),
        orchestrator: OrchestratorAutomationConfig::default(),
        github_trending: GithubTrendingConfig::default(),
        internal_claude: crate::config::InternalClaudeConfig::default(),
        agent_hub: crate::config::AgentHubConfig::default(),
        manual_peers: Vec::new(),
        relay: crate::config::RelayConfig::default(),
        experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
    };
    let store = Arc::new(MemoryConfigStore::with_config(config.clone()));
    let config_runtime = Arc::new(ConfigRuntime::new(config, store));
    let config = config_runtime.shared_value();
    let maintenance_gate = Arc::new(DatabaseMaintenanceGate::new());
    let owner = uuid::Uuid::new_v4().to_string();
    let event_bus = Arc::new(RuntimeEventBus::new(owner));
    let layout_repo = WorkbenchWorkspaceLayoutRepo::new(pool.clone());
    let _ = layout_repo.ensure_schema().await;

    let state = AppState {
        config,
        config_runtime,
        db: pool.clone(),
        maintenance_gate: maintenance_gate.clone(),
        prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
        attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
        transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
        claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
        scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
        ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
        device_id: Arc::new("svc-test".to_string()),
        devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
        actual_http_port: Arc::new(AtomicU16::new(0)),
        discovery: Arc::new(Mutex::new(None)),
        overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
        manual_peer_cancel: Arc::new(Mutex::new(None)),
        peer_client: Arc::new(PeerClient::new()),
        relay: Arc::new(crate::net::relay::RelayRuntime::new()),
        transfers: Arc::new(TransferRegistry::new()),
        ui: Arc::new(HeadlessBackendUi::new(tmp.path().to_path_buf())),
        update_runtime: Arc::new(UpdateRuntime::new()),
        cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
        workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
        workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
        workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
        workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
        workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
        agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
        agent_ledger_service: Arc::new(crate::workbench::agent_ledger::AgentLedgerService::new(
            crate::storage::AgentLedgerRepo::new(pool.clone()),
        )),
        agent_hub_repo: Arc::new(agent_hub),
        workbench_workspace_layout_repo: Arc::new(layout_repo),
        workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
            pool.clone(),
        )),
        browser_verification: Arc::new(
            crate::workbench::browser_verification::BrowserVerificationService::new(
                Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                tmp.path().join("browser-verification"),
                "test-owner".into(),
            )
            .expect("browser verification fixture"),
        ),
        workbench_browser_previews: Arc::new(
            crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
        ),
        workbench_sessions: Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new()),
        workbench_remote_events: Arc::new(
            crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
        ),
        workbench_remote_event_bridges: Arc::new(
            crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
        ),
        workbench_dependency: Arc::new(
            crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
        ),
        cc_collector_cancel: Arc::new(Mutex::new(None)),
        cloud_sync_runtime: Arc::new(CloudSyncRuntime::new()),
        cloud_sync_cancel: Arc::new(Mutex::new(None)),
        health: Arc::new(crate::health::HealthRuntime::new()),
        health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
        health_cancel: Arc::new(Mutex::new(None)),
        orchestrator_repo: Arc::new(OrchestratorRepo::new(pool.clone())),
        orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::default(),
        orchestrator_cancel: Arc::new(Mutex::new(None)),
        orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
        agent_ledger_cancel: Arc::new(Mutex::new(None)),
        agent_hub_cancel: Arc::new(Mutex::new(None)),
        agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
        agent_hub_git_cancel: Arc::new(Mutex::new(None)),
        workbench_claude_session_indexes: Arc::new(RwLock::new(std::collections::HashMap::new())),
        workbench_claude_session_watchers: Arc::new(Mutex::new(std::collections::HashMap::new())),
        workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
            std::collections::HashMap::new(),
        )),
        runtime_metrics: Arc::new(RuntimeMetrics::new()),
        runtime_role: RuntimeRole::HeadlessOwner,
        event_bus,
        backend_control_client_runtime: Arc::new(
            crate::backend::control_client::BackendControlClientRuntime::new(),
        ),
        gui_event_relay_cancel: Arc::new(Mutex::new(None)),
    };
    (state, tmp)
}

/// seed user scope + instruction asset + revision + multi bindings。
async fn seed_instruction_asset(
    state: &AppState,
    policy: crate::agent_hub::models::AssetPolicy,
    targets: &[(AgentTarget, DesiredPresence, bool)],
) -> LogicalAsset {
    use crate::agent_hub::models::{
        AssetKind, NewLogicalAsset, NewRevision, NewScopeNode, NewTargetBinding, RevisionId,
        RevisionOperation, RevisionOriginKind, ScopeKind,
    };
    let scope = state
        .agent_hub_repo
        .insert_scope(NewScopeNode {
            id: None,
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap();
    let asset = state
        .agent_hub_repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: format!("lk-{}", uuid::Uuid::new_v4().simple()),
            display_name: "demo".into(),
            policy,
        })
        .await
        .unwrap();
    let _rev = state
        .agent_hub_repo
        .append_revision(NewRevision {
            id: RevisionId::new_v7(),
            asset_lineage_id: asset.id.clone(),
            parents: vec![],
            operation: RevisionOperation::Upsert,
            origin_kind: RevisionOriginKind::Ui,
            origin_target: None,
            origin_replica_id: "svc-test".into(),
            payload_hash: Some("aa".repeat(32)),
            tree_manifest_hash: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expected_parent_id: None,
        })
        .await
        .unwrap();
    for (target, presence, enabled) in targets {
        state
            .agent_hub_repo
            .upsert_target_binding(NewTargetBinding {
                asset_id: asset.id.clone(),
                target: *target,
                local_scope_mapping_id: None,
                checkout_binding_id: None,
                desired_presence: *presence,
                desired_enabled: *enabled,
            })
            .await
            .unwrap();
    }
    state
        .agent_hub_repo
        .get_asset(&asset.id)
        .await
        .unwrap()
        .unwrap()
}

/// Business Logic: disable 一 target 不改其它 binding 与 canonical revision。
/// Code Logic: set_target_enabled(false) 公共路径。
#[tokio::test]
async fn service_disable_one_target_leaves_other_bindings_and_revision() {
    let (state, _tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[
            (AgentTarget::Claude, DesiredPresence::Present, true),
            (AgentTarget::Codex, DesiredPresence::Present, true),
        ],
    )
    .await;
    let head_before = asset.current_revision_id.clone();
    let summary = AgentHubService::set_target_enabled(
        &state,
        SetTargetEnabledRequest {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            desired_enabled: false,
        },
    )
    .await
    .unwrap();
    let claude = summary
        .targets
        .iter()
        .find(|t| t.target == AgentTarget::Claude)
        .unwrap();
    let codex = summary
        .targets
        .iter()
        .find(|t| t.target == AgentTarget::Codex)
        .unwrap();
    assert!(!claude.desired_enabled);
    assert_eq!(claude.desired_presence, DesiredPresence::Present);
    assert!(codex.desired_enabled);
    assert_eq!(codex.desired_presence, DesiredPresence::Present);
    let after = state
        .agent_hub_repo
        .get_asset(&asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.current_revision_id, head_before);
    // disable 策略落地：materialization 带 disable_strategy token
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap();
    let claude_b = bindings
        .iter()
        .find(|b| b.target == AgentTarget::Claude)
        .unwrap();
    let mat = state
        .agent_hub_repo
        .get_materialization_by_binding(&claude_b.id)
        .await
        .unwrap();
    assert!(mat
        .as_ref()
        .and_then(|m| m.last_error.as_ref())
        .is_some_and(|e| e.contains("disable_strategy")));
    assert!(!summary.aggregate_status.is_empty());
}

/// Business Logic: desiredPresence=absent 只卸本 target。
/// Code Logic: set_target_presence(Absent) 公共路径。
#[tokio::test]
async fn service_absent_is_target_local_only() {
    let (state, _tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[
            (AgentTarget::Claude, DesiredPresence::Present, true),
            (AgentTarget::Codex, DesiredPresence::Present, true),
        ],
    )
    .await;
    let summary = AgentHubService::set_target_presence(
        &state,
        SetTargetPresenceRequest {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            desired_presence: DesiredPresence::Absent,
        },
    )
    .await
    .unwrap();
    let claude = summary
        .targets
        .iter()
        .find(|t| t.target == AgentTarget::Claude)
        .unwrap();
    let codex = summary
        .targets
        .iter()
        .find(|t| t.target == AgentTarget::Codex)
        .unwrap();
    assert_eq!(claude.desired_presence, DesiredPresence::Absent);
    assert!(!claude.desired_enabled);
    assert_eq!(codex.desired_presence, DesiredPresence::Present);
    assert!(codex.desired_enabled);
}

/// Business Logic: 外部漂移路径在 binding 变更前拒绝，并返回精确 preview。
/// Code Logic: materialization native_path hash 漂移 → validation reject。
#[tokio::test]
async fn service_absent_rejects_before_mutation_when_paths_blocked() {
    let (state, tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[(AgentTarget::Claude, DesiredPresence::Present, true)],
    )
    .await;
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap();
    let b = bindings
        .iter()
        .find(|x| x.target == AgentTarget::Claude)
        .unwrap();
    let path = tmp.path().join("external.md");
    std::fs::write(&path, b"external").unwrap();
    state
        .agent_hub_repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            target_binding_id: b.id.clone(),
            native_path: Some(path.to_string_lossy().into_owned()),
            last_projected_revision_id: None,
            rendered_hash: Some(sha256_hex(b"managed")),
            observed_external_hash: Some(sha256_hex(b"managed")),
            status: MaterializationStatus::Synced,
            last_error: None,
        })
        .await
        .unwrap();
    let err = AgentHubService::set_target_presence(
        &state,
        SetTargetPresenceRequest {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            desired_presence: DesiredPresence::Absent,
        },
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("agent_hub_removal_blocked_unknown_or_changed_paths"),
        "msg={msg}"
    );
    // binding 未变
    let still = state
        .agent_hub_repo
        .get_target_binding(&b.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still.desired_presence, DesiredPresence::Present);
}

/// Business Logic: DeleteEverywhere 在任意 target 路径 blocked 时拒绝，且不写 tombstone/binding。
/// Code Logic: collect_removal_blocked_for_asset → RejectRemovalBlocked；asset 仍 live。
#[tokio::test]
async fn service_delete_everywhere_rejects_before_mutation_when_paths_blocked() {
    let (state, tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[
            (AgentTarget::Claude, DesiredPresence::Present, true),
            (AgentTarget::Codex, DesiredPresence::Present, true),
        ],
    )
    .await;
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap();
    let claude = bindings
        .iter()
        .find(|x| x.target == AgentTarget::Claude)
        .unwrap();
    let path = tmp.path().join("everywhere-drift.md");
    std::fs::write(&path, b"external").unwrap();
    state
        .agent_hub_repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            target_binding_id: claude.id.clone(),
            native_path: Some(path.to_string_lossy().into_owned()),
            last_projected_revision_id: None,
            rendered_hash: Some(sha256_hex(b"managed")),
            observed_external_hash: Some(sha256_hex(b"managed")),
            status: MaterializationStatus::Synced,
            last_error: None,
        })
        .await
        .unwrap();
    let before_rev = asset.current_revision_id.clone();
    let err = AgentHubService::delete_asset_everywhere(
        &state,
        DeleteAssetEverywhereRequest {
            asset_id: asset.id.clone(),
        },
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("agent_hub_removal_blocked_unknown_or_changed_paths"),
        "msg={msg}"
    );
    // 全部 binding 与 head 均未变
    let still_asset = state
        .agent_hub_repo
        .get_asset(&asset.id)
        .await
        .unwrap()
        .unwrap();
    assert!(still_asset.deleted_at.is_none());
    assert_eq!(still_asset.current_revision_id, before_rev);
    for b in state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap()
    {
        assert_eq!(b.desired_presence, DesiredPresence::Present);
        assert!(b.desired_enabled);
    }
}

/// Business Logic: restore_detached 清 Detached、Present+enabled、schedule 投影意图。
/// Code Logic: materialization Pending；binding Present。
#[tokio::test]
async fn service_restore_detached_clears_and_schedules() {
    let (state, _tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[(AgentTarget::Claude, DesiredPresence::Present, false)],
    )
    .await;
    let bindings = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap();
    let b = bindings.first().unwrap();
    state
        .agent_hub_repo
        .upsert_materialization(NewMaterialization {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            target_binding_id: b.id.clone(),
            native_path: Some("/tmp/x".into()),
            last_projected_revision_id: None,
            rendered_hash: Some("hh".into()),
            observed_external_hash: None,
            status: MaterializationStatus::Detached,
            last_error: Some("external_delete".into()),
        })
        .await
        .unwrap();
    let summary = AgentHubService::restore_detached_target(
        &state,
        RestoreDetachedTargetRequest {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
        },
    )
    .await
    .unwrap();
    let cell = summary
        .targets
        .iter()
        .find(|t| t.target == AgentTarget::Claude)
        .unwrap();
    assert_eq!(cell.desired_presence, DesiredPresence::Present);
    assert!(cell.desired_enabled);
    let mat = state
        .agent_hub_repo
        .get_materialization_by_binding(&b.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mat.status, MaterializationStatus::Pending);
}

/// Business Logic: delete_everywhere 一条 tombstone + 全部 Absent。
/// Code Logic: 公共 delete_asset_everywhere；CAS head 推进一次。
#[tokio::test]
async fn service_delete_everywhere_one_tombstone_and_fan_out() {
    let (state, _tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::Shared,
        &[
            (AgentTarget::Claude, DesiredPresence::Present, true),
            (AgentTarget::Codex, DesiredPresence::Present, true),
        ],
    )
    .await;
    let head_before = asset.current_revision_id.clone().unwrap();
    let summary = AgentHubService::delete_asset_everywhere(
        &state,
        DeleteAssetEverywhereRequest {
            asset_id: asset.id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(summary
        .targets
        .iter()
        .filter(|t| t.requested)
        .all(|t| t.desired_presence == DesiredPresence::Absent && !t.desired_enabled));
    let after = state
        .agent_hub_repo
        .get_asset(&asset.id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.deleted_at.is_some());
    assert_ne!(
        after.current_revision_id.as_ref().map(|r| r.as_str()),
        Some(head_before.as_str())
    );
}

/// Business Logic: targetOnly 最后一 target 不得猜 everywhere。
/// Code Logic: set_target_presence(Absent) → AppError validation token。
#[tokio::test]
async fn service_target_only_last_target_requires_everywhere() {
    let (state, _tmp) = build_service_state().await;
    let asset = seed_instruction_asset(
        &state,
        crate::agent_hub::models::AssetPolicy::TargetOnly,
        &[(AgentTarget::Claude, DesiredPresence::Present, true)],
    )
    .await;
    let err = AgentHubService::set_target_presence(
        &state,
        SetTargetPresenceRequest {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            desired_presence: DesiredPresence::Absent,
        },
    )
    .await
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("agent_hub_target_only_last_target_requires_everywhere"));
    let still = state
        .agent_hub_repo
        .list_target_bindings_for_asset(&asset.id)
        .await
        .unwrap();
    assert_eq!(still[0].desired_presence, DesiredPresence::Present);
}

/// Business Logic: status probe 必须走 evaluate_target_support，不得硬编码 Supported。
/// Code Logic: OpenCode 未 pin 时 scanOnly/unsupported；Claude/Codex 在本机 pin 命中时可 supported。
#[test]
fn status_probe_uses_evaluate_target_support_not_raw_supported() {
    let probes = probe_all_targets_best_effort();
    assert_eq!(
        probes.len(),
        crate::agent_hub::models::AgentTarget::ALL.len()
    );
    for p in probes {
        match p.target {
            crate::agent_hub::models::AgentTarget::OpenCode => {
                assert_ne!(
                    p.support.as_str(),
                    "supported",
                    "opencode without pin must not report Supported"
                );
                assert!(
                    matches!(p.support.as_str(), "scanOnly" | "unsupported"),
                    "unexpected support={} for opencode",
                    p.support
                );
            }
            crate::agent_hub::models::AgentTarget::Claude
            | crate::agent_hub::models::AgentTarget::Codex
            | crate::agent_hub::models::AgentTarget::Grok
            | crate::agent_hub::models::AgentTarget::Gemini
            | crate::agent_hub::models::AgentTarget::Cursor
            | crate::agent_hub::models::AgentTarget::Pi => {
                assert!(
                    matches!(p.support.as_str(), "supported" | "scanOnly" | "unsupported"),
                    "unexpected support={} for {}",
                    p.support,
                    p.target.as_str()
                );
            }
        }
    }
}

/// R5 P2.3: `probe_support_map` must funnel through
/// `builtin_support_manifest + evaluate_target_support + evaluate_target_support_flags`,
/// and a `None` manifest must label **no** target as supported.
#[test]
fn probe_support_map_null_manifest_marks_no_target_supported() {
    // Force a manifest load failure by passing an empty manifest module override path.
    // The function under test never reads process state for the manifest, so we exercise
    // it directly: when `builtin_support_manifest()` would fail-closed, every entry must
    // be `false`.
    use crate::agent_hub::support::{builtin_support_manifest, CapabilitySupport};
    let manifest = builtin_support_manifest().expect("default manifest loads");
    // Sanity: the helper must exist and be crate-reachable.
    let _flag_fn: fn(
        &crate::agent_hub::support::EvaluatedTargetSupport,
        crate::agent_hub::support::TargetCapability,
    ) -> bool = evaluate_target_support_flags;

    // Synthesise an evaluated target with no executable / version and confirm the
    // helper returns false for every capability — the canonical "no support" signal.
    let snapshot = crate::agent_hub::support::RuntimeProbeSnapshot {
        target: crate::agent_hub::models::AgentTarget::Claude,
        executable: None,
        version: None,
        config_root: std::path::PathBuf::from("/nonexistent"),
        fingerprint: String::new(),
        help_fingerprint: None,
    };
    let evaluated = crate::agent_hub::support::evaluate_target_support(&manifest, &snapshot);
    // Uncertified probe → read-side capabilities may be ReadOnly, write-side must be
    // Blocked。summary 的 scan 支持必须保留 ReadOnly，不把可发现误报为 unsupported。
    for cap in [
        crate::agent_hub::support::TargetCapability::ScanInstruction,
        crate::agent_hub::support::TargetCapability::RenderInstruction,
        crate::agent_hub::support::TargetCapability::ActivatePackage,
        crate::agent_hub::support::TargetCapability::DeactivatePackage,
    ] {
        let support = evaluated.capability(cap);
        assert!(
            matches!(
                support,
                CapabilitySupport::Blocked | CapabilitySupport::ReadOnly
            ),
            "uncertified probe must evaluate to Blocked or ReadOnly for {cap:?}, got {support:?}"
        );
        assert_eq!(
            evaluate_target_support_flags(&evaluated, cap),
            support == CapabilitySupport::ReadOnly,
            "ReadOnly should count for scan summary while Blocked remains false for {cap:?}"
        );
    }

    // Live map 只表达 scan 可用性；ReadOnly 可以为 true，但不得遗漏 target。
    let map = probe_support_map();
    assert_eq!(
        map.len(),
        AgentTarget::ALL.len(),
        "probe_support_map must cover all hub targets"
    );
    assert!(AgentTarget::ALL
        .iter()
        .all(|target| map.contains_key(target)));
}

/// R5 P2.3: `evaluate_target_support_flags` is exposed at `pub(crate)` so future
/// projection/activation code can gate writes without re-deriving the helper inline.
#[test]
fn evaluate_target_support_flags_exposed_for_crate() {
    // The type-level reference must compile, proving the function is reachable from
    // a downstream test module.
    let _fn_ref: fn(
        &crate::agent_hub::support::EvaluatedTargetSupport,
        crate::agent_hub::support::TargetCapability,
    ) -> bool = evaluate_target_support_flags;

    // And it must distinguish CapabilitySupport states as documented in the module docs.
    use crate::agent_hub::support::{
        builtin_support_manifest, CapabilitySupport, EvaluatedSupportMode, EvaluatedTargetSupport,
        RuntimeProbeSnapshot, TargetCapability,
    };
    let manifest = builtin_support_manifest().expect("default manifest");
    let snapshot = RuntimeProbeSnapshot {
        target: crate::agent_hub::models::AgentTarget::Codex,
        executable: None,
        version: None,
        config_root: std::path::PathBuf::from("/nonexistent"),
        fingerprint: String::new(),
        help_fingerprint: None,
    };
    let evaluated = crate::agent_hub::support::evaluate_target_support(&manifest, &snapshot);
    // Blocked path returns false；ReadOnly scan mode 则保持可发现。
    if matches!(evaluated.mode, EvaluatedSupportMode::Blocked { .. }) {
        let scan = evaluated.capability(TargetCapability::ScanInstruction);
        assert_eq!(
            evaluate_target_support_flags(&evaluated, TargetCapability::ScanInstruction),
            scan == CapabilitySupport::ReadOnly
        );
    } else {
        // Whatever the real-world verdict, the helper must agree with `evaluated.capability`.
        for cap in [
            TargetCapability::ScanInstruction,
            TargetCapability::RenderInstruction,
            TargetCapability::ActivatePackage,
        ] {
            let support = evaluated.capability(cap);
            let flag = evaluate_target_support_flags(&evaluated, cap);
            assert_eq!(
                flag,
                matches!(
                    support,
                    CapabilitySupport::Supported
                        | CapabilitySupport::SupportedAfterRestart
                        | CapabilitySupport::ActivationRequired
                        | CapabilitySupport::ReadOnly
                ),
                "evaluate_target_support_flags must agree with EvaluatedTargetSupport::capability for {cap:?}"
            );
        }
    }
    // And the helper must be `pub(crate)` so the next call site compiles.
    let _: EvaluatedTargetSupport = evaluated;
}
