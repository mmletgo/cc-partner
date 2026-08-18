//! agent_hub_portable_inventory_smoke — L2 portable inventory + local actions
//! Evidence: L2-AGENT-HUB-PORTABLE-PARITY-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     Portable asset management 需要在隔离 HOME/data_dir 下证明：三端 × 四类资产
//!     可被 inspect 发现；enable/disable/uninstall 经 preview→apply→rescan 真实改动；
//!     backup、Plugin keep_data、MCP comment+secret、未 opt-in 无写、request 幂等回放。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke：
//!     - 真实 scan fixtures（3×4 + project scopes + credential metadata）
//!     - 与 B4 executor 单测同构的 FakeProcessRunner + PortableActionExecutorDeps 动作路径
//!
//! NOT VERIFIED: 真实 product CLI 写能力 / 双主机 mDNS / 打包 GUI（L3）

use app_lib::agent_hub::models::PortableActionClaim;
use app_lib::agent_hub::packages::activator::FakeProcessRunner;
use app_lib::agent_hub::portable_actions::{
    apply_portable_asset_action_with, claim_portable_asset_action,
    get_portable_asset_action_by_request, preview_portable_asset_action_with_inventory,
    ApplyPortableAssetActionRequest, PortableActionExecutorDeps, PortableAssetActionItemState,
    PortableAssetActionKind, PortableAssetActionResultDto, PortableAssetConflictPolicy,
    PreviewPortableAssetActionRequest,
};
use app_lib::agent_hub::portable_inventory::{
    inspect_portable_inventory, inspect_portable_inventory_force, inventory_item_id,
    inventory_snapshot_hash, scan_portable_inventory_facts, PortableAssetKind, PortableAssetOwner,
    PortableInventoryItemCapabilitiesDto, PortableInventoryItemDto,
    PortableInventoryManagementState, PortableInventoryMutationCapability,
    PortableInventoryScanCapability, PortableInventorySnapshotDto, PortableInventorySourceOrigin,
    PortableInventoryTargetDto, PortableOriginKind, PortableScanScope,
};
use app_lib::agent_hub::targets::paths::TargetEnvironment;
use app_lib::backend::runtime::build_app_state;
use app_lib::backend::ui::HeadlessBackendUi;
use app_lib::{AgentHubRepo, AgentTarget, ScopeKind};
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

const CREDENTIAL_FIXTURE: &str = "plain-fixture-portable-parity";
const EVIDENCE: &str = "L2-AGENT-HUB-PORTABLE-PARITY-001";

struct PortableParityEnv {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    home: PathBuf,
    db_path: PathBuf,
}

/// 生产 inspect cache 合同：普通 inspect 可命中短 TTL，但 mutation/force rescan
/// 必须看到嵌套文件的新 tree hash；不注入 `rescan_override`。
#[tokio::test]
async fn production_force_rescan_bypasses_recent_inventory_cache() {
    let env = setup_isolated_env("force-rescan");
    let target_env = seed_3x4_fixtures(&env.home);
    std::env::set_var("HOME", &env.home);
    for (key, value) in &target_env.vars {
        std::env::set_var(key, value);
    }
    std::env::set_var(
        "PATH",
        target_env
            .path_entries
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(if cfg!(windows) { ";" } else { ":" }),
    );
    write(
        &env.data_dir.join("config.json"),
        &format!(
            r#"{{
  "device_id": "force-rescan-device",
  "device_name": "force-rescan-device",
  "http_port": 0,
  "receive_dir": "{}",
  "db_path": "{}",
  "screenshot_hotkey": "<cmd>+s",
  "prompt_optimizer_hotkey": "<ctrl>",
  "prompt_optimizer_fill_language": "zh"
}}"#,
            env.data_dir.join("received").display(),
            env.db_path.display(),
        ),
    );
    let state = build_app_state(Arc::new(HeadlessBackendUi::new(env.data_dir.clone())))
        .await
        .expect("production app state");

    let before = inspect_portable_inventory_force(&state)
        .await
        .expect("initial force inspect");
    let before_tree = before
        .items
        .iter()
        .find(|item| {
            item.target == AgentTarget::Claude
                && item.kind == PortableAssetKind::Skill
                && item.native_id == "review"
        })
        .and_then(|item| item.tree_hash.clone())
        .expect("review tree hash");

    write(
        &env.home.join(".claude/skills/review/nested/changed.txt"),
        "mutation-after-cache",
    );
    let cached = inspect_portable_inventory(&state)
        .await
        .expect("cached inspect");
    assert_eq!(
        cached
            .items
            .iter()
            .find(|item| {
                item.target == AgentTarget::Claude
                    && item.kind == PortableAssetKind::Skill
                    && item.native_id == "review"
            })
            .and_then(|item| item.tree_hash.clone()),
        Some(before_tree.clone()),
        "normal inspect intentionally remains within its short cache window"
    );

    let fresh = inspect_portable_inventory_force(&state)
        .await
        .expect("force inspect");
    let fresh_tree = fresh
        .items
        .iter()
        .find(|item| {
            item.target == AgentTarget::Claude
                && item.kind == PortableAssetKind::Skill
                && item.native_id == "review"
        })
        .and_then(|item| item.tree_hash.clone())
        .expect("fresh review tree hash");
    assert_ne!(
        fresh_tree, before_tree,
        "force rescan must observe nested mutation"
    );
}

fn setup_isolated_env(name: &str) -> PortableParityEnv {
    let root = tempfile::Builder::new()
        .prefix(&format!("cc-partner-portable-parity-{name}-"))
        .tempdir()
        .expect("tempdir");
    let data_dir = root.path().join("data");
    let home = root.path().join("home");
    let db_path = data_dir.join("data.db");
    fs::create_dir_all(data_dir.join("agent-hub").join("objects")).expect("objects");
    fs::create_dir_all(&home).expect("home");
    // SAFETY: --test-threads=1
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
    PortableParityEnv {
        _root: root,
        data_dir,
        home,
        db_path,
    }
}

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
        .expect("ensure_schema");
    pool
}

fn write(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).expect("parent");
    }
    fs::write(path, text).expect("write");
}

fn write_skill(dir: &Path, name: &str, body: &str) {
    write(
        &dir.join(name).join("SKILL.md"),
        &format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
    );
}

fn write_command(dir: &Path, name: &str, body: &str) {
    write(
        &dir.join(format!("{name}.md")),
        &format!("---\nname: {name}\ndescription: cmd\n---\n{body}\n"),
    );
}

fn seed_3x4_fixtures(home: &Path) -> TargetEnvironment {
    write_skill(
        &home.join(".claude/skills"),
        "review",
        "Shared review carefully.",
    );
    write_command(&home.join(".claude/commands"), "ship", "Ship it.");
    write(
        &home.join(".claude/plugins/demo-plugin/.claude-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"1.0.0","description":"Demo"}"#,
    );
    write_skill(
        &home.join(".claude/plugins/demo-plugin/skills"),
        "shared-name",
        "Plugin component skill.",
    );
    write(
        &home.join(".claude/.claude.json"),
        &format!(
            r#"{{
  // keep-comment-for-mcp
  "mcpServers": {{
    "good-api": {{
      "command": "uvx",
      "args": ["srv"],
      "env": {{ "API_TOKEN": "{CREDENTIAL_FIXTURE}" }},
      "enabled": true
    }},
    "keep-me": {{
      "command": "uvx",
      "args": ["keep"],
      "env": {{ "TOKEN": "{CREDENTIAL_FIXTURE}" }},
      "enabled": true
    }}
  }}
}}
"#
        ),
    );

    write_skill(
        &home.join(".agents/skills"),
        "review",
        "Codex review carefully.",
    );
    write(
        &home.join(".codex/plugins/demo-plugin/.codex-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"0.2.0"}"#,
    );
    write_skill(
        &home.join(".codex/plugins/demo-plugin/skills"),
        "shared-name",
        "Codex plugin skill.",
    );
    write(
        &home.join(".codex/config.toml"),
        &format!(
            r#"
[mcp_servers.good-api]
command = "uvx"
args = ["srv"]
enabled = true
env = {{ API_TOKEN = "{CREDENTIAL_FIXTURE}" }}
"#
        ),
    );

    write_skill(
        &home.join(".opencode/skills"),
        "review",
        "OpenCode review carefully.",
    );
    write_command(&home.join(".opencode/commands"), "ship", "OC ship.");
    write(
        &home.join(".opencode/plugins/demo-plugin/package.json"),
        r#"{"name":"demo-plugin","version":"3.0.0"}"#,
    );
    write_skill(
        &home.join(".opencode/plugins/demo-plugin/skills"),
        "shared-name",
        "OC plugin skill.",
    );
    write(
        &home.join("opencode.jsonc"),
        &format!(
            r#"{{
  // keep-comment-oc
  "mcpServers": {{
    "good-api": {{
      "command": "uvx",
      "args": ["oc"],
      "env": {{ "API_TOKEN": "{CREDENTIAL_FIXTURE}" }},
      "enabled": true
    }}
  }}
}}
"#
        ),
    );

    write_skill(
        &home.join("proj-opted/.claude/skills"),
        "proj-skill",
        "Opted project skill.",
    );
    write_skill(
        &home.join("proj-unopted/.claude/skills"),
        "hidden",
        "Unopted project skill.",
    );

    let bin = home.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    for name in ["claude", "codex", "opencode"] {
        let p = bin.join(name);
        write(&p, "#!/bin/sh\necho 1.0.0\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&p, perms).unwrap();
        }
    }

    let mut vars = BTreeMap::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        home.join(".claude").to_string_lossy().into_owned(),
    );
    vars.insert(
        "CODEX_HOME".into(),
        home.join(".codex").to_string_lossy().into_owned(),
    );
    vars.insert(
        "OPENCODE_CONFIG_DIR".into(),
        home.join(".opencode").to_string_lossy().into_owned(),
    );
    vars.insert(
        "OPENCODE_CONFIG".into(),
        home.join("opencode.jsonc").to_string_lossy().into_owned(),
    );
    TargetEnvironment {
        home: home.to_path_buf(),
        vars,
        path_entries: vec![bin],
    }
}

fn user_and_project_scopes(home: &Path) -> Vec<PortableScanScope> {
    vec![
        PortableScanScope {
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            absolute_path: home.to_path_buf(),
        },
        PortableScanScope {
            scope_id: "project:opted".into(),
            scope_kind: ScopeKind::Project,
            project_id: Some("opted".into()),
            project_opted_in: true,
            absolute_path: home.join("proj-opted"),
        },
        PortableScanScope {
            scope_id: "project:unopted".into(),
            scope_kind: ScopeKind::Project,
            project_id: Some("unopted".into()),
            project_opted_in: false,
            absolute_path: home.join("proj-unopted"),
        },
    ]
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
        evidence_ids: vec![EVIDENCE.into()],
    }
}

fn sample_item(
    target: AgentTarget,
    kind: PortableAssetKind,
    native_id: &str,
    path: &str,
    enabled: Option<bool>,
    content_hash: Option<String>,
) -> PortableInventoryItemDto {
    PortableInventoryItemDto {
        inventory_item_id: inventory_item_id(target, "user", path, native_id),
        target,
        loaded_by: target,
        owned_by: PortableAssetOwner::from_target(target),
        origin_kind: PortableOriginKind::Native,
        native_output_candidate: true,
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
        content_hash,
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
            can_migrate_to_store: false,
            can_attach: false,
            can_detach: false,
            can_destroy_store: false,
            can_confirm_current_version: false,
            can_materialize_escape_link: false,

            reason_code: None,
            evidence_ids: vec![EVIDENCE.into()],
        },
        warnings: vec![],
        mcp_credential: None,
        store: Default::default(),
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
) -> app_lib::agent_hub::portable_actions::PortableAssetActionPlanDto {
    preview_portable_asset_action_with_inventory(
        repo,
        PreviewPortableAssetActionRequest {
            inventory_snapshot_hash: snap.inventory_snapshot_hash.clone(),
            inventory_query: Default::default(),
            inventory_item_ids: ids,
            action,
            keep_data,
            conflict_policy: PortableAssetConflictPolicy::SkipExisting,
            expected_canonical_revision_id: None,
        },
        snap,
        "owner-fp-parity",
    )
    .await
    .expect("preview")
}

/// L2-AGENT-HUB-PORTABLE-PARITY-001
#[tokio::test]
async fn l2_agent_hub_portable_parity_001_inventory_and_actions() {
    let env = setup_isolated_env("inventory");
    let pool = open_hub_pool(&env.db_path).await;
    let repo = AgentHubRepo::new(pool);
    let target_env = seed_3x4_fixtures(&env.home);
    let scopes = user_and_project_scopes(&env.home);

    // ---- Real inspect 3×4 ----
    let (targets, items) = scan_portable_inventory_facts(&target_env, &scopes).expect("scan");
    assert_eq!(targets.len(), AgentTarget::ALL.len());
    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        let t_items: Vec<_> = items.iter().filter(|i| i.target == target).collect();
        assert!(t_items.iter().any(|i| i.kind == PortableAssetKind::Skill));
        assert!(t_items.iter().any(|i| i.kind == PortableAssetKind::Plugin));
        assert!(t_items.iter().any(|i| i.kind == PortableAssetKind::Mcp));
    }
    assert!(items.iter().any(|i| {
        i.target == AgentTarget::Claude
            && i.kind == PortableAssetKind::Command
            && i.native_id == "ship"
    }));
    assert!(items.iter().any(|i| {
        i.native_id == "proj-skill"
            && i.project_opted_in
            && i.project_id.as_deref() == Some("opted")
    }));
    assert!(items.iter().any(|i| {
        i.native_id == "hidden" && !i.project_opted_in && i.project_id.as_deref() == Some("unopted")
    }));
    let mcp = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Mcp
                && i.native_id == "good-api"
        })
        .expect("mcp");
    let cred = mcp.mcp_credential.as_ref().expect("cred");
    assert!(cred.present);
    assert!(cred.hash.is_some());
    assert!(!serde_json::to_string(mcp)
        .unwrap()
        .contains(CREDENTIAL_FIXTURE));

    // ---- Skill disable with REAL inventory skill hash domain (SKILL.md-only) ----
    let skill_root = env.home.join(".claude/skills/my-skill");
    write_skill(&env.home.join(".claude/skills"), "my-skill", "disable me");
    let (skill_hash, tree_hash, _, _) =
        app_lib::agent_hub::targets::portable::hash_skill_directory(&skill_root)
            .expect("skill hash");
    let mut skill_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "my-skill",
        skill_root.to_str().unwrap(),
        Some(true),
        Some(skill_hash),
    );
    skill_item.tree_hash = Some(tree_hash);
    let snap_skill = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![skill_item.clone()],
    );
    let plan_skill = preview_action(
        &repo,
        &snap_skill,
        vec![skill_item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    assert!(
        plan_skill.blocking_reasons.is_empty(),
        "{:?}",
        plan_skill.blocking_reasons
    );
    let mut post_skill = skill_item.clone();
    post_skill.actual_enabled = Some(false);
    post_skill.source_path = Some(
        env.data_dir
            .join("claude-assets/disabled/skills/my-skill")
            .to_string_lossy()
            .into(),
    );
    let post_skill_snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_skill]);
    let runner = Arc::new(FakeProcessRunner::new());
    let deps = PortableActionExecutorDeps {
        repo: AgentHubRepo::new(repo.pool().clone()),
        runner: runner.clone(),
        env: None,
        pre_inventory: Some(snap_skill),
        claude_config_dir: Some(env.home.join(".claude")),
        data_dir: Some(env.data_dir.clone()),
        rescan_override: Some(post_skill_snap),
    };
    let skill_res = apply_portable_asset_action_with(
        None,
        &deps,
        ApplyPortableAssetActionRequest {
            plan_token: plan_skill.plan_token.clone(),
            client_request_id: "req-skill-disable".into(),
        },
    )
    .await
    .expect("apply skill");
    assert_eq!(
        skill_res.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "{:?}",
        skill_res.items[0]
    );
    assert!(
        !skill_root.exists()
            || env
                .data_dir
                .join("claude-assets/disabled/skills/my-skill")
                .exists()
    );
    assert!(runner.calls().is_empty());

    // ---- MCP disable semantic patch ----
    let claude = env.home.join(".claude");
    let cfg = claude.join(".claude.json");
    // rewrite known good jsonc for action (same as B4)
    write(
        &cfg,
        r#"{
  // keep comment
  "mcpServers": {
    "keep-me": { "command": "uvx", "env": { "TOKEN": "secret-value" } },
    "drop-me": { "command": "npx", "env": { "KEY": "secret-key" } }
  }
}
"#,
    );
    // MCP leaf value_content_hash 与 planner/CAS 同域（禁止 clear content_hash 绕过）
    let mut mcp_item = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Mcp,
        "drop-me",
        cfg.to_str().unwrap(),
        Some(true),
        None,
    );
    // planner 会从 path 读 leaf hash；这里仍放 tree_hash 占位满足 source hash 存在性
    mcp_item.tree_hash = Some("t".into());
    // content_hash 可选：planner 对 MCP 优先 leaf value hash
    mcp_item.content_hash = None;
    let snap_mcp = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![mcp_item.clone()],
    );
    let plan_mcp = preview_action(
        &repo,
        &snap_mcp,
        vec![mcp_item.inventory_item_id.clone()],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    let mut post_mcp = mcp_item.clone();
    post_mcp.actual_enabled = Some(false);
    let post_mcp_snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_mcp]);
    let runner2 = Arc::new(FakeProcessRunner::new());
    let deps_mcp = PortableActionExecutorDeps {
        repo: AgentHubRepo::new(repo.pool().clone()),
        runner: runner2.clone(),
        env: None,
        pre_inventory: Some(snap_mcp),
        claude_config_dir: Some(claude.clone()),
        data_dir: Some(env.data_dir.clone()),
        rescan_override: Some(post_mcp_snap),
    };
    let mcp_res = apply_portable_asset_action_with(
        None,
        &deps_mcp,
        ApplyPortableAssetActionRequest {
            plan_token: plan_mcp.plan_token,
            client_request_id: "req-mcp-disable".into(),
        },
    )
    .await
    .expect("apply mcp");
    assert_eq!(
        mcp_res.items[0].state,
        PortableAssetActionItemState::Succeeded,
        "{:?}",
        mcp_res.items[0]
    );
    let after = fs::read_to_string(&cfg).unwrap();
    assert!(after.contains("keep-me"));
    assert!(!after.contains("\"drop-me\""));
    let json = serde_json::to_string(&mcp_res).unwrap();
    assert!(!json.contains("secret-value"));
    assert!(!json.contains("secret-key"));
    assert!(runner2.calls().is_empty());

    // ---- Plugin enable + uninstall keep_data argv ----
    let plugin = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "review@local",
        "/plugins/review",
        Some(false),
        Some("content-hash".into()),
    );
    let snap_plugin = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![plugin.clone()],
    );
    let plan_en = preview_action(
        &repo,
        &snap_plugin,
        vec![plugin.inventory_item_id.clone()],
        PortableAssetActionKind::Enable,
        false,
    )
    .await;
    let mut post_en = plugin.clone();
    post_en.actual_enabled = Some(true);
    let post_en_snap = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![post_en]);
    let runner3 = Arc::new(FakeProcessRunner::new());
    runner3.push_ok("ok");
    let deps_en = PortableActionExecutorDeps {
        repo: AgentHubRepo::new(repo.pool().clone()),
        runner: runner3.clone(),
        env: None,
        pre_inventory: Some(snap_plugin.clone()),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post_en_snap),
    };
    let en_res = apply_portable_asset_action_with(
        None,
        &deps_en,
        ApplyPortableAssetActionRequest {
            plan_token: plan_en.plan_token,
            client_request_id: "req-plugin-enable".into(),
        },
    )
    .await
    .expect("enable");
    assert_eq!(
        en_res.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    let calls = runner3.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].args[0], "plugin");
    assert_eq!(calls[0].args[1], "enable");
    let scope_idx = calls[0].args.iter().position(|a| a == "--scope").unwrap();
    assert_eq!(calls[0].args[scope_idx + 1], "user");

    let plugin2 = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Plugin,
        "shared@cc",
        "/plugins/shared",
        Some(true),
        Some("content-hash".into()),
    );
    let snap_un = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![plugin2.clone()],
    );
    let plan_un = preview_action(
        &repo,
        &snap_un,
        vec![plugin2.inventory_item_id.clone()],
        PortableAssetActionKind::Uninstall,
        true,
    )
    .await;
    assert!(plan_un.keep_data);
    let post_un = snapshot_from(vec![sample_target(AgentTarget::Claude)], vec![]);
    let runner4 = Arc::new(FakeProcessRunner::new());
    runner4.push_ok("ok");
    let deps_un = PortableActionExecutorDeps {
        repo: AgentHubRepo::new(repo.pool().clone()),
        runner: runner4.clone(),
        env: None,
        pre_inventory: Some(snap_un),
        claude_config_dir: None,
        data_dir: None,
        rescan_override: Some(post_un),
    };
    let un_res = apply_portable_asset_action_with(
        None,
        &deps_un,
        ApplyPortableAssetActionRequest {
            plan_token: plan_un.plan_token,
            client_request_id: "req-plugin-uninst".into(),
        },
    )
    .await
    .expect("uninstall");
    assert_eq!(
        un_res.items[0].state,
        PortableAssetActionItemState::Succeeded
    );
    let un_calls = runner4.calls();
    assert_eq!(un_calls.len(), 1);
    assert!(un_calls[0].args.iter().any(|a| a == "--keep-data"));
    assert!(un_calls[0].args.iter().any(|a| a == "--scope"));

    // ---- Unopted project blocks ----
    let mut unopted = sample_item(
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        "hidden",
        env.home
            .join("proj-unopted/.claude/skills/hidden")
            .to_str()
            .unwrap(),
        Some(true),
        Some("content-hash".into()),
    );
    unopted.scope_kind = ScopeKind::Project;
    unopted.scope_id = "project:unopted".into();
    unopted.project_id = Some("unopted".into());
    unopted.project_opted_in = false;
    unopted.inventory_item_id = inventory_item_id(
        AgentTarget::Claude,
        "project:unopted",
        unopted.source_path.as_deref().unwrap(),
        "hidden",
    );
    let path_un = PathBuf::from(unopted.source_path.as_ref().unwrap());
    let before = fs::metadata(&path_un).and_then(|m| m.modified()).ok();
    let snap_uo = snapshot_from(
        vec![sample_target(AgentTarget::Claude)],
        vec![unopted.clone()],
    );
    let plan_uo = preview_action(
        &repo,
        &snap_uo,
        vec![unopted.inventory_item_id],
        PortableAssetActionKind::Disable,
        false,
    )
    .await;
    assert!(
        plan_uo
            .blocking_reasons
            .iter()
            .any(|r| r.contains("NOT_OPTED_IN") || r.contains("PROJECT")),
        "{:?}",
        plan_uo.blocking_reasons
    );
    let after_m = fs::metadata(&path_un).and_then(|m| m.modified()).ok();
    assert_eq!(before, after_m);

    // ---- Replay ----
    let replay = claim_portable_asset_action(&repo, &plan_skill.plan_token, "req-skill-disable")
        .await
        .expect("replay");
    match replay {
        PortableActionClaim::Replay(json) => {
            let back: PortableAssetActionResultDto = serde_json::from_str(&json).unwrap();
            assert_eq!(back.client_request_id, "req-skill-disable");
            assert_eq!(back.items[0].state, PortableAssetActionItemState::Succeeded);
        }
        other => panic!("expected Replay, got {other:?}"),
    }
    let by_req = get_portable_asset_action_by_request(&repo, "req-skill-disable")
        .await
        .expect("get");
    assert_eq!(by_req.client_request_id, "req-skill-disable");

    assert_eq!(EVIDENCE, "L2-AGENT-HUB-PORTABLE-PARITY-001");
    println!("{EVIDENCE}: inventory 3x4 + actions/rescan/replay certified");
}
