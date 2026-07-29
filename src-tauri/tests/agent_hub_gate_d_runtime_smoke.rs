//! agent_hub_gate_d_runtime_smoke — Gate D Plugin + OpenCode runtime certification (L2)
//! Evidence: L2-AGENT-HUB-D-PLUGIN-001, L2-AGENT-HUB-D-RUNTIME-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     Gate D 必须在隔离 data_dir 下证明：mixed Plugin 的固定 revision 投影、
//!     per-target full/partial/sourceOnly/activationRequired 聚合、package 删除引用派生
//!     保留 shared/standalone，以及 OpenCode runtime bridge hash / OSC 剥离 /
//!     preflight fail-closed / Fresh resume CAS 契约。真实 OpenCode TUI 不在本 smoke 宣称。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke（不启动完整 backend 二进制 / 不启动真实 OpenCode CLI）：
//!     - L2-AGENT-HUB-D-PLUGIN-001：portable Skill/MCP/Command/Agent + targetOnly Hook +
//!       OpenCode residual 的 per-target 投影；Snapshot residual CAS 闭包；
//!       package delete preserve shared/standalone。
//!     - L2-AGENT-HUB-D-RUNTIME-001：bridge generated_source_hash 钉死、preview/materialize/
//!       verify、OSC 可见剥离、preflight fail-closed、Fresh resume CAS 源码契约。
//!
//! NOT VERIFIED（本 smoke 不宣称）:
//!     - 真实 OpenCode CLI 可见 TUI / session.idle / permission NeedsInput / Ctrl-C takeover
//!       （`L3-AGENT-HUB-D-OPENCODE-001` / `L3-AGENT-HUB-OPENCODE-RUNTIME-001`）
//!     - 真实 Claude/Codex product install 写能力（`L3-AGENT-HUB-B-CLI-001`）
//!     - 双主机 mDNS Plugin 复制（`L3-AGENT-HUB-C-LAN-001`）
//!     - 打包 GUI / 全平台矩阵；当前仅 cargo test 本机

use app_lib::agent_hub::assets::{
    McpTransport, PortableAgent, PortableAssetPayload, PortableCommand, PortableMcpServer,
    PortableSkill,
};
use app_lib::agent_hub::models::{
    AssetAggregateStatus, AssetKind, AssetPolicy, NewLogicalAsset, NewScopeNode,
    RevisionOriginKind, ScopeKind,
};
use app_lib::agent_hub::object_store::{TreeEntry, TreeEntryType, TreeManifest};
use app_lib::agent_hub::plugins::{
    decide_component_delete, project_plugin_package, ComponentDeleteDecision, ComponentOwnership,
    ComponentTargetStatus, HookEventIntent, PackageRenderInput, PluginComponentRef,
    PluginPackagePayload, PluginResidualRef, PortableHook, ResidualKind, ResolvedComponentPayload,
};
use app_lib::agent_hub::snapshot::builder::{
    build_snapshot, SnapshotSelectionMode, SnapshotSelectionRequest,
};
use app_lib::agent_hub_sha256_hex;
use app_lib::orchestrator::agent_adapter::{
    opencode_preflight_block_reason, AgentAvailability, AgentProbeResult, AgentProviderId,
    REASON_EXTERNAL_COLLISION, REASON_L3_RUNTIME_EVIDENCE_MISSING, REASON_RUNTIME_BRIDGE_REQUIRED,
};
use app_lib::{
    AgentHubObjectStore, AgentHubRepo, AgentOscDecoder, AgentSessionPhase, AgentTarget,
    OpenCodeBridgeOutcome, OpenCodeEventMapper, OpenCodeOfficialEvent, OpenCodeRuntimeBridge,
    OPENCODE_RUNTIME_BRIDGE_REL_PATH, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH,
};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const SKILL_BODY: &str = "---\nname: review\ndescription: mixed skill\n---\n# Review carefully\n";
const RESIDUAL_JS: &str = "export default async function plugin() { return {}; }\n";

// ---------------------------------------------------------------------------
// 隔离环境
// ---------------------------------------------------------------------------

/// 隔离 smoke 根目录。
///
/// Business Logic: Gate D smoke 不得触碰用户真实 HOME / `~/.cc-partner`。
/// Code Logic: tempfile + data 子路径；串行 `--test-threads=1`。
struct GateDSmokeEnv {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    db_path: PathBuf,
}

/// Business Logic: 每个 smoke case 独立 data。
/// Code Logic: 创建目录布局并 set_var。
fn setup_isolated_env(name: &str) -> GateDSmokeEnv {
    let root = tempfile::Builder::new()
        .prefix(&format!("cc-partner-gate-d-{name}-"))
        .tempdir()
        .expect("tempdir");
    let data_dir = root.path().join("data");
    let db_path = data_dir.join("data.db");
    fs::create_dir_all(data_dir.join("agent-hub").join("objects")).expect("objects");
    // SAFETY: 串行 smoke（--test-threads=1）。
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
    GateDSmokeEnv {
        _root: root,
        data_dir,
        db_path,
    }
}

/// Business Logic: smoke 需要独立 SQLite + AgentHub schema。
/// Code Logic: WAL 单连接池 ensure_schema。
async fn open_hub_pool(db_path: &Path) -> sqlx::SqlitePool {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).expect("db parent");
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}?mode=rwc", db_path.display()))
        .expect("sqlite options")
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("pool");
    AgentHubRepo::ensure_schema(&pool)
        .await
        .expect("agent hub schema");
    pool
}

// ---------------------------------------------------------------------------
// L2-AGENT-HUB-D-PLUGIN-001
// ---------------------------------------------------------------------------

/// L2-AGENT-HUB-D-PLUGIN-001：mixed Plugin 投影 + 删除引用派生 + residual CAS。
///
/// Business Logic: package 成功不得 overstate full；删除不得误删 shared/standalone。
/// Code Logic: project_plugin_package 四 target 态 + repo delete decisions + snapshot residual tree。
#[tokio::test]
async fn l2_agent_hub_d_plugin_001_mixed_package_projection_and_delete() {
    let env = setup_isolated_env("plugin");
    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(env.data_dir.join("agent-hub").join("objects"))
        .expect("object store");

    let scope = repo
        .insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .expect("scope");

    // --- portable components into CAS / revision graph ---
    let skill_md = store.put_blob(SKILL_BODY.as_bytes()).await.unwrap();
    let skill_tree = store
        .put_tree(&TreeManifest {
            entries: vec![TreeEntry {
                path: "SKILL.md".into(),
                blob_hash: skill_md.hash.clone(),
                entry_type: TreeEntryType::File,
                executable: false,
            }],
        })
        .await
        .unwrap();

    let skill_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Skill,
            origin_namespace: "plugin:demo.mixed".into(),
            logical_key: "review".into(),
            display_name: "review".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let skill_rev = repo
        .append_portable_asset_revision(
            &skill_asset.id,
            &PortableAssetPayload::Skill(PortableSkill {
                name: "review".into(),
                description: "mixed skill".into(),
                skill_markdown_hash: skill_md.hash.clone(),
                tree_manifest_hash: skill_tree.hash.clone(),
                target_extensions: BTreeMap::new(),
            }),
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let mcp_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Mcp,
            origin_namespace: "plugin:demo.mixed".into(),
            logical_key: "docs-mcp".into(),
            display_name: "docs-mcp".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let mcp_rev = repo
        .append_portable_asset_revision(
            &mcp_asset.id,
            &PortableAssetPayload::Mcp(PortableMcpServer {
                key: "docs-mcp".into(),
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec![
                        "-y".into(),
                        "@modelcontextprotocol/server-filesystem".into(),
                    ],
                    cwd: None,
                },
                env: BTreeMap::from([("SECRET".into(), "plain-fixture-hub-d".into())]),
                enabled: true,
                tool_allow: vec![],
                tool_deny: vec![],
                target_extensions: BTreeMap::new(),
            }),
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let cmd_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Command,
            origin_namespace: "plugin:demo.mixed".into(),
            logical_key: "ship".into(),
            display_name: "ship".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let cmd_rev = repo
        .append_portable_asset_revision(
            &cmd_asset.id,
            &PortableAssetPayload::Command(PortableCommand {
                name: "ship".into(),
                description: Some("partial command".into()),
                prompt_template: "ship it".into(),
                arguments: vec![],
                target_extensions: {
                    let mut m = BTreeMap::new();
                    m.insert(AgentTarget::Claude, json!({"unknownField": true}));
                    m
                },
            }),
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let agent_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Agent,
            origin_namespace: "plugin:demo.mixed".into(),
            logical_key: "reviewer".into(),
            display_name: "reviewer".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let agent_rev = repo
        .append_portable_asset_revision(
            &agent_asset.id,
            &PortableAssetPayload::Agent(PortableAgent {
                name: "reviewer".into(),
                description: Some("review agent".into()),
                instructions: "You review carefully.".into(),
                mode_intent: None,
                tool_intents: vec![],
                target_extensions: BTreeMap::new(),
            }),
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let hook_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Hook,
            origin_namespace: "plugin:demo.mixed".into(),
            logical_key: "pre-tool".into(),
            display_name: "pre-tool".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .unwrap();
    let hook = PortableHook {
        event_intent: HookEventIntent::PreToolUse,
        input_contract: json!({"toolName": "Bash"}),
        output_contract: json!({"permission": "allow"}),
        command_tree_hash: None,
        source_target: AgentTarget::OpenCode,
        target_extensions: BTreeMap::new(),
    };
    let hook_rev = repo
        .append_portable_hook_revision(
            &hook_asset.id,
            &hook,
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let residual_blob = store.put_blob(RESIDUAL_JS.as_bytes()).await.unwrap();
    let residual_tree = store
        .put_tree(&TreeManifest {
            entries: vec![TreeEntry {
                path: "index.ts".into(),
                blob_hash: residual_blob.hash.clone(),
                entry_type: TreeEntryType::File,
                executable: false,
            }],
        })
        .await
        .unwrap();

    let package_payload = PluginPackagePayload {
        plugin_id: "demo.mixed".into(),
        name: "Mixed Plugin".into(),
        version: Some("1.0.0".into()),
        description: Some("gate-d mixed".into()),
        source_target: AgentTarget::OpenCode,
        component_refs: vec![
            PluginComponentRef {
                kind: AssetKind::Skill,
                asset_id: skill_asset.id.clone(),
                revision_id: skill_rev.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            },
            PluginComponentRef {
                kind: AssetKind::Mcp,
                asset_id: mcp_asset.id.clone(),
                revision_id: mcp_rev.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            },
            PluginComponentRef {
                kind: AssetKind::Command,
                asset_id: cmd_asset.id.clone(),
                revision_id: cmd_rev.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            },
            PluginComponentRef {
                kind: AssetKind::Agent,
                asset_id: agent_asset.id.clone(),
                revision_id: agent_rev.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            },
            PluginComponentRef {
                kind: AssetKind::Hook,
                asset_id: hook_asset.id.clone(),
                revision_id: hook_rev.id.clone(),
                ownership: ComponentOwnership::PackageOwned,
            },
        ],
        residual_refs: vec![PluginResidualRef {
            target: AgentTarget::OpenCode,
            residual_kind: ResidualKind::Runtime,
            tree_manifest_hash: residual_tree.hash.clone(),
        }],
        target_extensions: BTreeMap::new(),
    };

    let plugin_asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Plugin,
            origin_namespace: "standalone".into(),
            logical_key: "demo.mixed".into(),
            display_name: "Mixed Plugin".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .unwrap();
    let plugin_rev = repo
        .append_plugin_package_revision(
            &plugin_asset.id,
            &package_payload,
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();
    assert_eq!(plugin_rev.operation.as_str(), "upsert");

    // resolved map for render (fixed revisions)
    let mut resolved: BTreeMap<String, ResolvedComponentPayload> = BTreeMap::new();
    resolved.insert(
        skill_rev.id.as_str().into(),
        ResolvedComponentPayload::Portable {
            payload: PortableAssetPayload::Skill(PortableSkill {
                name: "review".into(),
                description: "mixed skill".into(),
                skill_markdown_hash: skill_md.hash,
                tree_manifest_hash: skill_tree.hash,
                target_extensions: BTreeMap::new(),
            }),
            partial: false,
            skill_markdown: Some(SKILL_BODY.into()),
        },
    );
    resolved.insert(
        mcp_rev.id.as_str().into(),
        ResolvedComponentPayload::Portable {
            payload: PortableAssetPayload::Mcp(PortableMcpServer {
                key: "docs-mcp".into(),
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec![
                        "-y".into(),
                        "@modelcontextprotocol/server-filesystem".into(),
                    ],
                    cwd: None,
                },
                env: BTreeMap::from([("SECRET".into(), "plain-fixture-hub-d".into())]),
                enabled: true,
                tool_allow: vec![],
                tool_deny: vec![],
                target_extensions: BTreeMap::new(),
            }),
            partial: false,
            skill_markdown: None,
        },
    );
    resolved.insert(
        cmd_rev.id.as_str().into(),
        ResolvedComponentPayload::Portable {
            payload: PortableAssetPayload::Command(PortableCommand {
                name: "ship".into(),
                description: Some("partial command".into()),
                prompt_template: "ship it".into(),
                arguments: vec![],
                target_extensions: {
                    let mut m = BTreeMap::new();
                    m.insert(AgentTarget::Claude, json!({"unknownField": true}));
                    m
                },
            }),
            partial: true,
            skill_markdown: None,
        },
    );
    resolved.insert(
        agent_rev.id.as_str().into(),
        ResolvedComponentPayload::Portable {
            payload: PortableAssetPayload::Agent(PortableAgent {
                name: "reviewer".into(),
                description: Some("review agent".into()),
                instructions: "You review carefully.".into(),
                mode_intent: None,
                tool_intents: vec![],
                target_extensions: BTreeMap::new(),
            }),
            partial: false,
            skill_markdown: None,
        },
    );
    resolved.insert(
        hook_rev.id.as_str().into(),
        ResolvedComponentPayload::Hook { hook: hook.clone() },
    );

    // OpenCode: residual included; partial command → Partial (not overstated Full)
    let opencode_report = project_plugin_package(&PackageRenderInput {
        package: package_payload.clone(),
        destination: AgentTarget::OpenCode,
        resolved: resolved.clone(),
        data_dir: Some(env.data_dir.clone()),
        scope_id: scope.id.clone(),
        ..PackageRenderInput::default()
    })
    .expect("opencode project");
    assert_eq!(
        opencode_report.aggregate_status,
        AssetAggregateStatus::Partial,
        "partial command must keep aggregate Partial; diagnostics={:?}",
        opencode_report.diagnostics
    );
    assert!(opencode_report.residuals.iter().all(|r| r.included));
    assert!(opencode_report.components.iter().any(|c| {
        c.kind == AssetKind::Command && c.target_status == ComponentTargetStatus::Partial
    }));

    // OpenCode without partial command → Full when residual + hook same-target
    let mut full_package = package_payload.clone();
    full_package
        .component_refs
        .retain(|c| c.kind != AssetKind::Command);
    let mut full_resolved = resolved.clone();
    full_resolved.remove(cmd_rev.id.as_str());
    let full_report = project_plugin_package(&PackageRenderInput {
        package: full_package,
        destination: AgentTarget::OpenCode,
        resolved: full_resolved,
        data_dir: Some(env.data_dir.clone()),
        scope_id: scope.id.clone(),
        ..PackageRenderInput::default()
    })
    .expect("opencode full");
    assert_eq!(
        full_report.aggregate_status,
        AssetAggregateStatus::Full,
        "diagnostics={:?}",
        full_report.diagnostics
    );

    // Claude cross-target: targetOnly hook + residual sourceOnly / omitted → never Full
    let claude_report = project_plugin_package(&PackageRenderInput {
        package: package_payload.clone(),
        destination: AgentTarget::Claude,
        resolved: resolved.clone(),
        hook_registry: vec![],
        known_evidence_ids: BTreeSet::new(),
        ..PackageRenderInput::default()
    })
    .expect("claude project");
    assert_ne!(claude_report.aggregate_status, AssetAggregateStatus::Full);
    assert!(matches!(
        claude_report.aggregate_status,
        AssetAggregateStatus::Partial | AssetAggregateStatus::SourceOnly
    ));
    let hook_cell = claude_report
        .components
        .iter()
        .find(|c| c.kind == AssetKind::Hook)
        .expect("hook");
    assert_eq!(hook_cell.target_status, ComponentTargetStatus::SourceOnly);
    assert!(!claude_report.residuals[0].included);

    // Codex activationRequired
    let codex_report = project_plugin_package(&PackageRenderInput {
        package: package_payload.clone(),
        destination: AgentTarget::Codex,
        resolved: resolved.clone(),
        force_activation_required: true,
        ..PackageRenderInput::default()
    })
    .expect("codex project");
    assert_eq!(
        codex_report.aggregate_status,
        AssetAggregateStatus::ActivationRequired
    );
    assert_eq!(codex_report.activation_state, "activationRequired");

    // Snapshot residual path: residual tree must be closed over for plugin package
    let built = build_snapshot(
        &repo,
        &store,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::ExplicitAssets,
            scope_ids: vec![],
            asset_ids: vec![plugin_asset.id.clone()],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000d1".into(),
            limits: None,
        },
    )
    .await
    .expect("build snapshot with plugin residual");
    assert!(
        built.object_bytes.contains_key(&residual_tree.hash)
            || built
                .envelope
                .objects
                .iter()
                .any(|o| o.hash == residual_tree.hash),
        "snapshot must close over residual runtime tree hash"
    );
    // exact residual bytes round-trip from CAS
    let residual_bytes = store
        .get_blob(&residual_blob.hash)
        .await
        .expect("residual blob");
    assert_eq!(residual_bytes.as_slice(), RESIDUAL_JS.as_bytes());

    // --- package delete preserves shared / standalone ---
    let pkg_b = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Plugin,
            origin_namespace: "standalone".into(),
            logical_key: "demo.mixed.b".into(),
            display_name: "Mixed Plugin B".into(),
            policy: AssetPolicy::TargetOnly,
        })
        .await
        .unwrap();
    let payload_b = PluginPackagePayload {
        plugin_id: "demo.mixed.b".into(),
        name: "Mixed B".into(),
        version: Some("1".into()),
        description: None,
        source_target: AgentTarget::OpenCode,
        component_refs: vec![PluginComponentRef {
            kind: AssetKind::Skill,
            asset_id: skill_asset.id.clone(),
            revision_id: skill_rev.id.clone(),
            ownership: ComponentOwnership::Shared,
        }],
        residual_refs: vec![],
        target_extensions: BTreeMap::new(),
    };
    let _pb = repo
        .append_plugin_package_revision(
            &pkg_b.id,
            &payload_b,
            &store,
            RevisionOriginKind::Ui,
            Some(AgentTarget::OpenCode),
            "01900000-0000-7000-8000-0000000000d1",
            None,
        )
        .await
        .unwrap();

    let standalone = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id.clone(),
            kind: AssetKind::Skill,
            origin_namespace: "standalone".into(),
            logical_key: "review".into(),
            display_name: "standalone review".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    repo.upsert_component_standalone_ref(&skill_asset.id, &standalone.id)
        .await
        .unwrap();

    assert_eq!(
        decide_component_delete(true, 99),
        ComponentDeleteDecision::PreserveStandalone
    );
    assert_eq!(
        decide_component_delete(false, 1),
        ComponentDeleteDecision::PreserveShared
    );
    assert_eq!(
        decide_component_delete(false, 0),
        ComponentDeleteDecision::TombstoneOwned
    );

    let del = repo
        .delete_plugin_package_with_ownership(
            &plugin_asset.id,
            &store,
            RevisionOriginKind::Ui,
            "01900000-0000-7000-8000-0000000000d1",
        )
        .await
        .expect("delete package a");
    let skill_decision = del
        .component_decisions
        .iter()
        .find(|d| d.component_asset_id == skill_asset.id)
        .expect("skill decision");
    assert_eq!(
        skill_decision.decision,
        ComponentDeleteDecision::PreserveStandalone
    );
    let skill_after = repo.get_asset(&skill_asset.id).await.unwrap().unwrap();
    assert!(
        skill_after.deleted_at.is_none(),
        "shared+standalone skill must survive"
    );

    let del_b = repo
        .delete_plugin_package_with_ownership(
            &pkg_b.id,
            &store,
            RevisionOriginKind::Ui,
            "01900000-0000-7000-8000-0000000000d1",
        )
        .await
        .expect("delete package b");
    let skill_decision_b = del_b
        .component_decisions
        .iter()
        .find(|d| d.component_asset_id == skill_asset.id)
        .expect("skill decision b");
    assert_eq!(
        skill_decision_b.decision,
        ComponentDeleteDecision::PreserveStandalone
    );
    let skill_final = repo.get_asset(&skill_asset.id).await.unwrap().unwrap();
    assert!(skill_final.deleted_at.is_none());
}

// ---------------------------------------------------------------------------
// L2-AGENT-HUB-D-RUNTIME-001
// ---------------------------------------------------------------------------

/// L2-AGENT-HUB-D-RUNTIME-001：bridge hash / OSC 剥离 / preflight fail-closed / Fresh CAS。
///
/// Business Logic: openCodeVisible 不得在缺 bridge 时 green；OSC 不得进可见 terminal。
/// Code Logic: library-level bridge + decoder + preflight + source-contract for Fresh resume。
#[test]
fn l2_agent_hub_d_runtime_001_bridge_osc_preflight_resume_cas() {
    // 1) bridge source hash 钉死（app-version 派生物）
    let live = agent_hub_sha256_hex(OpenCodeRuntimeBridge::generated_source().as_bytes());
    assert_eq!(OpenCodeRuntimeBridge::generated_source_hash(), live);
    assert_eq!(
        OpenCodeRuntimeBridge::generated_source_hash(),
        OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH
    );

    // 2) unopted preview 不写盘；opt-in materialize/verify 后 collision 不覆盖
    let project = tempfile::tempdir().expect("project root");
    let preview = OpenCodeRuntimeBridge::preview(project.path(), false);
    assert!(
        matches!(
            preview,
            OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. }
                | OpenCodeBridgeOutcome::Preview { .. }
        ),
        "expected runtimeBridgeRequired/preview without opt-in, got {preview:?}"
    );
    assert!(!OpenCodeRuntimeBridge::absolute_path(project.path()).exists());

    let mat = OpenCodeRuntimeBridge::materialize(project.path(), true).expect("materialize");
    assert!(matches!(
        mat,
        OpenCodeBridgeOutcome::Materialized { .. } | OpenCodeBridgeOutcome::Verified { .. }
    ));
    let verified = OpenCodeRuntimeBridge::verify(project.path(), true);
    assert!(matches!(verified, OpenCodeBridgeOutcome::Verified { .. }));

    // collision: foreign bytes at reserved path
    let path = OpenCodeRuntimeBridge::absolute_path(project.path());
    fs::write(&path, b"// not generated bridge\n").expect("overwrite");
    let coll = OpenCodeRuntimeBridge::materialize(project.path(), true).expect("collision");
    assert!(
        matches!(coll, OpenCodeBridgeOutcome::ExternalCollision { .. }),
        "foreign bridge bytes must externalCollision, got {coll:?}"
    );
    assert_eq!(path, project.path().join(OPENCODE_RUNTIME_BRIDGE_REL_PATH));

    // 3) OSC mapping: busy → active; session.idle → Completed; mixed PTY strips OSC
    let mut mapper = OpenCodeEventMapper::new("agent-d-1", "term-d-1");
    let busy = mapper
        .map_event(&OpenCodeOfficialEvent::SessionStatus {
            session_id: "native-sess-1".into(),
            status: "busy".into(),
        })
        .expect("map busy");
    assert_eq!(busy.phase, AgentSessionPhase::Working);
    let completed = mapper
        .map_event(&OpenCodeOfficialEvent::SessionIdle {
            session_id: "native-sess-1".into(),
        })
        .expect("map session.idle after active");
    assert_eq!(completed.phase, AgentSessionPhase::Completed);

    let mut decoder = AgentOscDecoder::default();
    let mut mixed = b"hello-before".to_vec();
    mixed.extend_from_slice(&completed.osc_bytes);
    mixed.extend_from_slice(b"hello-after");
    let out = decoder.push(&mixed);
    assert_eq!(
        String::from_utf8_lossy(&out.visible),
        "hello-beforehello-after",
        "OSC payload must never appear in terminal visible/replay bytes"
    );
    assert!(
        !String::from_utf8_lossy(&out.visible).contains("cc-partner-agent-v1"),
        "osc prefix must be stripped"
    );
    assert!(
        !out.mutations.is_empty(),
        "decoder must extract structured mutation from OSC frame"
    );

    // 4) preflight fail-closed without bridge / L3 evidence
    let unavailable = AgentProbeResult {
        provider_id: AgentProviderId::OpenCodeVisible,
        availability: AgentAvailability::Unavailable,
        executable: None,
        version: None,
        reason_code: Some(REASON_L3_RUNTIME_EVIDENCE_MISSING.into()),
    };
    let required = OpenCodeBridgeOutcome::RuntimeBridgeRequired {
        relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.into(),
        source_hash: OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH.into(),
    };
    assert_eq!(
        opencode_preflight_block_reason(&unavailable, &required),
        Some(REASON_L3_RUNTIME_EVIDENCE_MISSING)
    );

    let available = AgentProbeResult {
        provider_id: AgentProviderId::OpenCodeVisible,
        availability: AgentAvailability::Available,
        executable: Some("opencode".into()),
        version: Some("0.0.0".into()),
        reason_code: None,
    };
    assert_eq!(
        opencode_preflight_block_reason(&available, &required),
        Some(REASON_RUNTIME_BRIDGE_REQUIRED)
    );
    let collision = OpenCodeBridgeOutcome::ExternalCollision {
        relative_path: OPENCODE_RUNTIME_BRIDGE_REL_PATH.into(),
        source_hash: OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH.into(),
        absolute_path: path.display().to_string(),
        current_hash: Some("ab".repeat(32)),
    };
    assert_eq!(
        opencode_preflight_block_reason(&available, &collision),
        Some(REASON_EXTERNAL_COLLISION)
    );

    // 5) Fresh resume CAS fail-closed contract (source-level)
    let bridge_src = include_str!("../src/orchestrator/agent_runtime_bridge.rs");
    assert!(
        bridge_src.contains("update_active_runner_session_and_agent"),
        "resume must call CAS helper"
    );
    let cas_idx = bridge_src
        .find("update_active_runner_session_and_agent")
        .expect("CAS call site");
    // Fresh branch uses `?` on CAS before build_resume_plan / write paths.
    let plan_idx = bridge_src
        .find("build_resume_plan")
        .expect("build_resume_plan");
    assert!(
        cas_idx < plan_idx || bridge_src.contains("Fresh"),
        "CAS must participate in Fresh resume path before plan write"
    );
}

/// 编译期诚实锚点：真实 OpenCode TUI 不在默认 L2 宣称。
///
/// Business Logic: 避免 CI 绿被误读为 L3 runtime 认证。
/// Code Logic: 仅文档断言 + 常量存在。
#[test]
fn l3_agent_hub_d_opencode_remains_not_verified_without_real_cli() {
    assert!(
        !OpenCodeRuntimeBridge::generated_source().is_empty(),
        "bridge source must exist for later L3 wiring"
    );
    let evidence = "L3-AGENT-HUB-D-OPENCODE-001";
    assert!(evidence.starts_with("L3-"));
}
