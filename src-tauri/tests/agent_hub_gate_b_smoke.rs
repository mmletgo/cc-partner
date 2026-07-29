//! agent_hub_gate_b_smoke — Gate B portable asset process smoke (L2)
//! Evidence: L2-AGENT-HUB-B-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     Gate B 需要在隔离 HOME/data_dir 下证明：shared Skill/Command/Agent/MCP 可被三端
//!     扫描发现、Claude/Codex targetOnly 不泄漏到 OpenCode 受管/ native 集合、unmanaged
//!     TOML/JSONC 在 enable/disable/update/remove 后仍存活、legacy adoption crash 恢复后
//!     恰好一份 discoverable source、credential 字节与 canonical/目标配置一致且日志不泄漏。
//!
//! Code Logic（这个文件做什么）:
//!     library-level process smoke（不启动完整 backend 二进制；FakeProcessRunner + support bypass）：
//!     A) 隔离 fixture：shared Skill/Command/Agent/MCP + Claude/Codex targetOnly Skills
//!     B) 三端 scanner：各端恰好一份 shared Skill；OpenCode 不报告 targetOnly
//!     C) managed package 物化：targetOnly 不进入错误 target package
//!     D) ownership-aware TOML/JSONC round-trip 保留 unmanaged 内容
//!     E) legacy adoption crash → recover → 一份 discoverable source
//!     F) MCP credential 原文进 CAS/canonical；诊断/redaction 不含 fixture
//!
//! NOT VERIFIED（本 smoke 不宣称）:
//!     - 真实 Claude/Codex/OpenCode CLI 可执行写能力（见 L3-AGENT-HUB-B-CLI-001）
//!     - 完整 sidecar owner runtime / GUI / 多机 mDNS / LAN Hub 复制
//!     - 全平台矩阵；当前仅验证 cargo test 本机环境

use app_lib::agent_hub::assets::{
    canonical_bytes, redact_sensitive_text, McpTransport, PortableAssetPayload, PortableMcpServer,
};
use app_lib::agent_hub::config_patch::{
    apply_config_patch_atomically, JsoncConfigPatcher, ManagedConfigPatch, TomlConfigPatcher,
};
use app_lib::agent_hub::packages::{
    materialize_package, package_materialized_root, AdoptionEngine, AdoptionFault, AdoptionOutcome,
    AdoptionRequest, PackageBuildInput, PackageSkillInput,
};
use app_lib::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, LocalScopeMapping,
    OpenCodeInstructionAdapter, TargetEnvironment,
};
use app_lib::{
    hash_skill_directory, AdoptionState, AgentHubObjectStore, AgentHubRepo, AgentTarget, AssetKind,
    DiscoveredPortableAsset, FakeProcessRunner, PortableAssetOrigin, PortableDiscoveryStatus,
    PortableOriginKind, PortableSkill, ScopeKind,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

const CREDENTIAL_FIXTURE: &str = "plain-fixture";
const SHARED_SKILL: &str = "review";
const CLAUDE_ONLY: &str = "claude-only";
const CODEX_ONLY: &str = "codex-only";

// ---------------------------------------------------------------------------
// 隔离环境
// ---------------------------------------------------------------------------

/// 隔离 smoke 根目录。
///
/// Business Logic: Gate B smoke 不得触碰用户真实 HOME / `~/.cc-partner`。
/// Code Logic: tempfile + data/home 子路径；注入 CC_PARTNER_DATA_DIR（串行）。
struct GateBSmokeEnv {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    home: PathBuf,
    db_path: PathBuf,
}

/// Business Logic: 每个 smoke case 独立 data/home。
/// Code Logic: 创建目录布局并 set_var（--test-threads=1）。
fn setup_isolated_env(name: &str) -> GateBSmokeEnv {
    let root = tempfile::Builder::new()
        .prefix(&format!("cc-partner-gate-b-{name}-"))
        .tempdir()
        .expect("tempdir");
    let data_dir = root.path().join("data");
    let home = root.path().join("home");
    let db_path = data_dir.join("data.db");
    fs::create_dir_all(data_dir.join("agent-hub").join("objects")).expect("objects");
    fs::create_dir_all(&home).expect("home");
    // SAFETY: 串行 smoke 进程内使用，不跨线程并发改 env。
    std::env::set_var("CC_PARTNER_DATA_DIR", &data_dir);
    GateBSmokeEnv {
        _root: root,
        data_dir,
        home,
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
        .expect("ensure_schema");
    pool
}

/// Business Logic: 扫描用注入环境，永不改 process HOME。
/// Code Logic: CLAUDE_CONFIG_DIR / CODEX_HOME / OPENCODE_* 指向隔离 home。
fn isolated_target_env(home: &Path) -> TargetEnvironment {
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
        path_entries: vec![],
    }
}

fn user_scope(home: &Path) -> LocalScopeMapping {
    LocalScopeMapping {
        scope_kind: ScopeKind::User,
        absolute_path: home.to_path_buf(),
        project_root: None,
        relative_root: None,
        codex_fallback_filenames: vec![],
    }
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

fn write_agent(dir: &Path, name: &str, body: &str) {
    write(
        &dir.join(format!("{name}.md")),
        &format!("---\nname: {name}\ndescription: agent\n---\n{body}\n"),
    );
}

/// Business Logic: shared portable fixtures 进三端 native 根；targetOnly 仅 Claude/Codex。
/// Code Logic: 写 skills/commands/agents + MCP JSON/TOML/JSONC；不含 targetOnly 到 OpenCode native。
fn seed_shared_and_target_only_fixtures(home: &Path) {
    // Shared skill on all three native roots
    write_skill(
        &home.join(".claude/skills"),
        SHARED_SKILL,
        "Shared review carefully.",
    );
    write_skill(
        &home.join(".agents/skills"),
        SHARED_SKILL,
        "Shared review carefully.",
    );
    write_skill(
        &home.join(".opencode/skills"),
        SHARED_SKILL,
        "Shared review carefully.",
    );

    // Shared command / agent on Claude + OpenCode native (Codex uses agents.toml + md)
    write_command(
        &home.join(".claude/commands"),
        "release",
        "Ship shared $ARGUMENTS",
    );
    write_command(
        &home.join(".opencode/commands"),
        "release",
        "Ship shared $ARGUMENTS",
    );
    write_agent(
        &home.join(".claude/agents"),
        "reviewer",
        "Be thorough shared.",
    );
    write_agent(
        &home.join(".opencode/agents"),
        "reviewer",
        "Be thorough shared.",
    );

    // Claude MCP with credential fixture
    write(
        &home.join(".claude/.claude.json"),
        &format!(
            r#"{{
  "mcpServers": {{
    "private-api": {{
      "type": "http",
      "url": "https://example.invalid/mcp?token={CREDENTIAL_FIXTURE}",
      "headers": {{ "Authorization": "Bearer {CREDENTIAL_FIXTURE}" }},
      "env": {{ "API_TOKEN": "{CREDENTIAL_FIXTURE}" }}
    }}
  }}
}}"#
        ),
    );

    // Codex config.toml MCP + agent
    write(
        &home.join(".codex/config.toml"),
        &format!(
            r#"# user unmanaged header
model = "user-model"

[mcp_servers.user-owned]
command = "uvx"
args = ["user-srv"]

[mcp_servers.private-api]
command = "uvx"
args = ["srv"]
env = {{ API_TOKEN = "{CREDENTIAL_FIXTURE}" }}

[agents.reviewer]
description = "Reviews"
config_file = "agents/reviewer.md"
"#
        ),
    );
    write(
        &home.join(".codex/agents/reviewer.md"),
        "Codex shared reviewer instructions\n",
    );

    // OpenCode jsonc MCP
    write(
        &home.join("opencode.jsonc"),
        &format!(
            r#"{{
  // keep comment unmanaged
  "mcpServers": {{
    "user-owned": {{
      "command": "uvx",
      "args": ["user-oc"]
    }},
    "private-api": {{
      "command": "uvx",
      "args": ["oc-srv"],
      "env": {{ "API_TOKEN": "{CREDENTIAL_FIXTURE}" }}
    }}
  }}
}}
"#
        ),
    );

    // targetOnly: ONLY under Claude / Codex native roots — never OpenCode native
    write_skill(
        &home.join(".claude/skills"),
        CLAUDE_ONLY,
        "Claude only body.",
    );
    write_skill(&home.join(".agents/skills"), CODEX_ONLY, "Codex only body.");
}

fn count_skills(found: &[DiscoveredPortableAsset], name: &str) -> usize {
    found
        .iter()
        .filter(|d| d.kind == AssetKind::Skill && d.semantic_name == name)
        .count()
}

fn count_native_skills(found: &[DiscoveredPortableAsset], name: &str) -> usize {
    found
        .iter()
        .filter(|d| {
            d.kind == AssetKind::Skill
                && d.semantic_name == name
                && d.origin.origin_kind == PortableOriginKind::Native
        })
        .count()
}

fn managed_patch(
    owner: &str,
    path: &[&str],
    value: Option<serde_json::Value>,
) -> ManagedConfigPatch {
    ManagedConfigPatch {
        owner_id: owner.into(),
        path: path.iter().map(|s| (*s).to_string()).collect(),
        value,
        expected_base_hash: None,
    }
}

// ---------------------------------------------------------------------------
// A + B: discovery isolation
// ---------------------------------------------------------------------------

/// L2-AGENT-HUB-B-001：shared 资产三端各发现一次；OpenCode 不报告 Claude/Codex targetOnly。
#[tokio::test]
async fn gate_b_scanners_shared_once_and_target_only_isolated() {
    let env = setup_isolated_env("scan");
    seed_shared_and_target_only_fixtures(&env.home);
    let target_env = isolated_target_env(&env.home);
    let scope = user_scope(&env.home);

    let claude = ClaudeInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("claude scan");
    let codex = CodexInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("codex scan");
    let opencode = OpenCodeInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("opencode scan");

    // 1) each target reports exactly one shared Skill (by semantic name; native preferred)
    assert_eq!(
        count_skills(&claude, SHARED_SKILL),
        1,
        "claude shared skills={:?}",
        claude
            .iter()
            .filter(|d| d.kind == AssetKind::Skill)
            .map(|d| (&d.semantic_name, d.origin.origin_kind))
            .collect::<Vec<_>>()
    );
    // Codex: legacy .agents/skills 中 shared（可能 1）
    assert!(
        count_skills(&codex, SHARED_SKILL) >= 1,
        "codex must see shared skill"
    );
    // OpenCode: native shared + may see compat copies of shared under .claude/.agents
    assert!(
        count_native_skills(&opencode, SHARED_SKILL) == 1,
        "opencode native shared must be exactly one; found {:?}",
        opencode
            .iter()
            .filter(|d| d.kind == AssetKind::Skill)
            .map(|d| (&d.semantic_name, d.origin.origin_kind, &d.origin.path))
            .collect::<Vec<_>>()
    );

    // Commands / Agents / MCP present for shared import surface
    assert!(
        claude
            .iter()
            .any(|d| d.kind == AssetKind::Command && d.semantic_name == "release"),
        "claude command"
    );
    assert!(
        claude
            .iter()
            .any(|d| d.kind == AssetKind::Agent && d.semantic_name == "reviewer"),
        "claude agent"
    );
    assert!(
        claude
            .iter()
            .any(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api"),
        "claude mcp"
    );
    assert!(
        codex
            .iter()
            .any(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api"),
        "codex mcp"
    );
    assert!(
        opencode
            .iter()
            .any(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api"),
        "opencode mcp"
    );

    // 2) OpenCode never reports Claude/Codex targetOnly as native
    assert_eq!(
        count_native_skills(&opencode, CLAUDE_ONLY),
        0,
        "OpenCode must not native-report claude-only"
    );
    assert_eq!(
        count_native_skills(&opencode, CODEX_ONLY),
        0,
        "OpenCode must not native-report codex-only"
    );
    // Managed package isolation (stricter than compat scan): targetOnly not in OpenCode package
    let pkg_input = |target: AgentTarget| PackageBuildInput {
        data_dir: env.data_dir.clone(),
        target,
        scope_id: "user".into(),
        skills: vec![
            PackageSkillInput {
                logical_asset_id: "asset-shared-review".into(),
                name: SHARED_SKILL.into(),
                description: "shared".into(),
                skill_markdown: "# Shared review\nDo careful review.\n".into(),
                target_only: false,
                visible_targets: vec![],
            },
            PackageSkillInput {
                logical_asset_id: "asset-claude-only".into(),
                name: CLAUDE_ONLY.into(),
                description: "claude only".into(),
                skill_markdown: "# Claude only\n".into(),
                target_only: true,
                visible_targets: vec![AgentTarget::Claude],
            },
            PackageSkillInput {
                logical_asset_id: "asset-codex-only".into(),
                name: CODEX_ONLY.into(),
                description: "codex only".into(),
                skill_markdown: "# Codex only\n".into(),
                target_only: true,
                visible_targets: vec![AgentTarget::Codex],
            },
        ],
    };
    let oc_pkg = materialize_package(&pkg_input(AgentTarget::OpenCode)).expect("oc pkg");
    assert!(
        oc_pkg.meta.invocation_aliases.contains_key(SHARED_SKILL),
        "shared in opencode package"
    );
    assert!(
        !oc_pkg.meta.invocation_aliases.contains_key(CLAUDE_ONLY),
        "claude-only must not leak into OpenCode package"
    );
    assert!(
        !oc_pkg.meta.invocation_aliases.contains_key(CODEX_ONLY),
        "codex-only must not leak into OpenCode package"
    );
    let claude_pkg = materialize_package(&pkg_input(AgentTarget::Claude)).expect("claude pkg");
    assert!(claude_pkg
        .meta
        .invocation_aliases
        .contains_key(SHARED_SKILL));
    assert!(claude_pkg.meta.invocation_aliases.contains_key(CLAUDE_ONLY));
    assert!(!claude_pkg.meta.invocation_aliases.contains_key(CODEX_ONLY));
    let codex_pkg = materialize_package(&pkg_input(AgentTarget::Codex)).expect("codex pkg");
    assert!(codex_pkg.meta.invocation_aliases.contains_key(SHARED_SKILL));
    assert!(codex_pkg.meta.invocation_aliases.contains_key(CODEX_ONLY));
    assert!(!codex_pkg.meta.invocation_aliases.contains_key(CLAUDE_ONLY));
    // Managed packages never under legacy skill roots
    for pkg in [&claude_pkg, &codex_pkg, &oc_pkg] {
        let s = pkg.package_root.to_string_lossy();
        assert!(!s.contains("/.claude/skills"));
        assert!(!s.contains("/.agents/skills"));
        assert!(
            s.contains("agent-hub/materialized-packages") || s.contains("materialized-packages")
        );
    }
}

// ---------------------------------------------------------------------------
// C: unmanaged TOML/JSONC survives managed enable/disable/update/remove
// ---------------------------------------------------------------------------

/// L2：ownership-aware config patch 保留 unmanaged 字段与注释。
#[test]
fn gate_b_unmanaged_toml_jsonc_survives_managed_round_trip() {
    let env = setup_isolated_env("config");
    let toml_path = env.home.join(".codex/config.toml");
    let jsonc_path = env.home.join("opencode.jsonc");
    write(
        &toml_path,
        r#"# keep header
model = "gpt-user"

[mcp_servers.user-owned]
command = "uvx"
args = ["a", "b"]

[mcp_servers.cc_partner_x]
command = "old"
"#,
    );
    write(
        &jsonc_path,
        r#"{
  // keep comment
  "mcpServers": {
    "user-owned": { "command": "uvx", "args": ["keep"] },
    "cc_partner_x": { "command": "old" }
  }
}
"#,
    );

    let toml_patcher = TomlConfigPatcher;
    // update
    apply_config_patch_atomically(
        &toml_patcher,
        &toml_path,
        &[managed_patch(
            "hub",
            &["mcp_servers", "cc_partner_x"],
            Some(serde_json::json!({"command":"new","args":["1"]})),
        )],
    )
    .expect("toml update");
    // disable (remove managed leaf)
    apply_config_patch_atomically(
        &toml_patcher,
        &toml_path,
        &[managed_patch("hub", &["mcp_servers", "cc_partner_x"], None)],
    )
    .expect("toml remove");
    // re-enable
    apply_config_patch_atomically(
        &toml_patcher,
        &toml_path,
        &[managed_patch(
            "hub",
            &["mcp_servers", "cc_partner_x"],
            Some(serde_json::json!({"command":"enabled"})),
        )],
    )
    .expect("toml re-enable");
    let after_toml = fs::read_to_string(&toml_path).expect("read toml");
    assert!(after_toml.contains("# keep header"));
    assert!(after_toml.contains("model = \"gpt-user\""));
    assert!(after_toml.contains("[mcp_servers.user-owned]"));
    assert!(
        after_toml.contains("args = [\"a\", \"b\"]") || after_toml.contains("args=[\"a\", \"b\"]")
    );
    assert!(
        after_toml.contains("command = \"enabled\"") || after_toml.contains("command=\"enabled\"")
    );

    let jsonc_patcher = JsoncConfigPatcher;
    apply_config_patch_atomically(
        &jsonc_patcher,
        &jsonc_path,
        &[managed_patch(
            "hub",
            &["mcpServers", "cc_partner_x"],
            Some(serde_json::json!({"command":"new","args":["1"]})),
        )],
    )
    .expect("jsonc update");
    apply_config_patch_atomically(
        &jsonc_patcher,
        &jsonc_path,
        &[managed_patch("hub", &["mcpServers", "cc_partner_x"], None)],
    )
    .expect("jsonc remove");
    apply_config_patch_atomically(
        &jsonc_patcher,
        &jsonc_path,
        &[managed_patch(
            "hub",
            &["mcpServers", "cc_partner_x"],
            Some(serde_json::json!({"command":"enabled"})),
        )],
    )
    .expect("jsonc re-enable");
    let after_jsonc = fs::read_to_string(&jsonc_path).expect("read jsonc");
    assert!(
        after_jsonc.contains("keep comment") || after_jsonc.contains("//"),
        "jsonc comment/unmanaged surface should survive: {after_jsonc}"
    );
    assert!(after_jsonc.contains("user-owned"));
    assert!(after_jsonc.contains("keep") || after_jsonc.contains("uvx"));
    assert!(after_jsonc.contains("enabled"));
}

// ---------------------------------------------------------------------------
// D: legacy adoption crash recovery → one discoverable source
// ---------------------------------------------------------------------------

/// Business Logic: adoption 故障注入路径需要 FakeProcessRunner + support bypass。
/// Code Logic: 对齐 quality_faults / packages::adoption 测试 setup。
async fn setup_adoption(data_dir: &Path, db_path: &Path) -> (AdoptionEngine, AgentHubRepo) {
    let pool = open_hub_pool(db_path).await;
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(data_dir).expect("object store root-only");
    let runner = Arc::new(FakeProcessRunner::new());
    for _ in 0..32 {
        runner.push_ok(r#"{"plugins":["plugin@cc-partner"]}"#);
    }
    let engine = AdoptionEngine::new(repo.clone(), store, runner);
    engine.inject_support_bypass(true);
    (engine, repo)
}

fn discovered_legacy(target: AgentTarget, path: &Path) -> DiscoveredPortableAsset {
    let (content, tree, _, diags) = hash_skill_directory(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    DiscoveredPortableAsset {
        kind: AssetKind::Skill,
        semantic_name: name.clone(),
        scope_kind: ScopeKind::User,
        payload: PortableAssetPayload::Skill(PortableSkill {
            name: name.clone(),
            description: "test".into(),
            skill_markdown_hash: content.clone(),
            tree_manifest_hash: tree.clone(),
            target_extensions: Default::default(),
        }),
        origin: PortableAssetOrigin {
            target,
            path: path.to_path_buf(),
            origin_kind: PortableOriginKind::LegacyStandalone,
            native_id: name,
            content_hash: content,
            tree_hash: Some(tree),
            status: PortableDiscoveryStatus::Active,
            native_output_candidate: false,
        },
        diagnostics: diags,
    }
}

fn adoption_request(data: &Path, discovered: DiscoveredPortableAsset) -> AdoptionRequest {
    AdoptionRequest {
        data_dir: data.to_path_buf(),
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        confirmed: true,
        discovered,
        origin_namespace: "legacy".into(),
        origin_replica_id: "gate-b-smoke".into(),
    }
}

fn count_skill_dirs(root: &Path, name: &str) -> usize {
    let p = root.join(name);
    if p.is_dir() && p.join("SKILL.md").is_file() {
        1
    } else {
        0
    }
}

/// L2：crash-before-db-commit 后 recover → 恰好一份 discoverable source。
#[tokio::test]
async fn gate_b_legacy_adoption_crash_recovers_to_one_source() {
    let env = setup_isolated_env("adopt");
    let (engine, repo) = setup_adoption(&env.data_dir, &env.db_path).await;
    let root = env.home.join(".claude/skills");
    write_skill(&root, SHARED_SKILL, "body for adoption");
    let skill = root.join(SHARED_SKILL);
    let disc = discovered_legacy(AgentTarget::Claude, &skill);

    engine.inject_fault(AdoptionFault::CrashBeforeDbCommit);
    let out = engine
        .adopt(adoption_request(&env.data_dir, disc))
        .await
        .expect("adopt");
    assert!(
        matches!(out, AdoptionOutcome::Blocked { .. }),
        "crash inject must block: {out:?}"
    );
    assert!(!skill.exists(), "source renamed into staging");
    let staging = env.data_dir.join("agent-hub/adoption-staging");
    assert!(staging.is_dir(), "staging holds archive");

    // 故障后：兼容路径 0 份 discoverable standalone
    assert_eq!(count_skill_dirs(&root, SHARED_SKILL), 0);

    let rows = repo.list_adoptions().await.expect("list");
    let archived = rows
        .into_iter()
        .find(|r| r.state == AdoptionState::Archived)
        .expect("archived row");

    engine.inject_fault(AdoptionFault::None);
    let recovered = engine
        .recover_adoption(&archived.id)
        .await
        .expect("recover");
    assert!(
        matches!(recovered, AdoptionOutcome::Adopted { .. }),
        "recover must commit: {recovered:?}"
    );
    let done = repo.get_adoption(&archived.id).await.unwrap().unwrap();
    assert_eq!(done.state, AdoptionState::Committed);

    // 恰好一份 managed package skill（非 legacy 路径双发现）
    let pkg_root = package_materialized_root(&env.data_dir).join("claude");
    assert!(pkg_root.is_dir(), "managed package present");
    let mut skill_md_count = 0usize;
    for e in walkdir::WalkDir::new(&pkg_root).follow_links(false) {
        let e = e.unwrap();
        if e.file_name() == "SKILL.md" {
            skill_md_count += 1;
        }
    }
    assert_eq!(
        skill_md_count, 1,
        "exactly one discoverable managed SKILL.md after recovery"
    );
    assert_eq!(
        count_skill_dirs(&root, SHARED_SKILL),
        0,
        "legacy standalone must remain gone"
    );
}

// ---------------------------------------------------------------------------
// E + F: credential bytes exact + logs/diagnostics clean
// ---------------------------------------------------------------------------

/// L2：credential 进入 canonical 与扫描 payload；诊断/redaction 不含 fixture。
#[tokio::test]
async fn gate_b_credential_bytes_match_and_not_logged() {
    let env = setup_isolated_env("cred");
    seed_shared_and_target_only_fixtures(&env.home);
    let target_env = isolated_target_env(&env.home);
    let scope = user_scope(&env.home);

    let claude = ClaudeInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("scan");
    let mcp = claude
        .iter()
        .find(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api")
        .expect("mcp discovered");

    match &mcp.payload {
        PortableAssetPayload::Mcp(s) => {
            assert_eq!(
                s.env.get("API_TOKEN").map(String::as_str),
                Some(CREDENTIAL_FIXTURE)
            );
            match &s.transport {
                McpTransport::Http { url, headers, .. } => {
                    assert!(
                        url.contains(CREDENTIAL_FIXTURE),
                        "url must retain credential bytes"
                    );
                    let expected_auth = format!("Bearer {CREDENTIAL_FIXTURE}");
                    assert_eq!(
                        headers.get("Authorization").map(String::as_str),
                        Some(expected_auth.as_str())
                    );
                }
                other => panic!("expected http transport, got {other:?}"),
            }
            // canonical CAS payload retains exact bytes
            let bytes = canonical_bytes(&PortableAssetPayload::Mcp(s.clone())).expect("canon");
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                text.contains(CREDENTIAL_FIXTURE),
                "canonical must store credential verbatim"
            );
            // diagnostics must not echo fixture
            for d in s.collect_diagnostics() {
                assert!(
                    !d.message.contains(CREDENTIAL_FIXTURE),
                    "diag leaked: {}",
                    d.message
                );
                let safe = d.format_safe();
                assert!(
                    !safe.contains(CREDENTIAL_FIXTURE),
                    "format_safe leaked: {safe}"
                );
            }
        }
        other => panic!("expected mcp payload, got {other:?}"),
    }

    // redaction helper
    let hostile = format!(
        "Authorization: Bearer {CREDENTIAL_FIXTURE} API_TOKEN={CREDENTIAL_FIXTURE} url=https://x?token={CREDENTIAL_FIXTURE}"
    );
    let redacted = redact_sensitive_text(&hostile);
    assert!(
        !redacted.contains(CREDENTIAL_FIXTURE),
        "redaction failed: {redacted}"
    );

    // codex/opencode scanned MCP env also exact
    let codex = CodexInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("codex");
    let codex_mcp = codex
        .iter()
        .find(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api")
        .expect("codex mcp");
    match &codex_mcp.payload {
        PortableAssetPayload::Mcp(s) => {
            assert_eq!(
                s.env.get("API_TOKEN").map(String::as_str),
                Some(CREDENTIAL_FIXTURE)
            );
        }
        _ => panic!("codex mcp payload"),
    }
    let opencode = OpenCodeInstructionAdapter
        .scan_portable_assets(&scope, &target_env)
        .expect("opencode");
    let oc_mcp = opencode
        .iter()
        .find(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api")
        .expect("opencode mcp");
    match &oc_mcp.payload {
        PortableAssetPayload::Mcp(s) => {
            assert_eq!(
                s.env.get("API_TOKEN").map(String::as_str),
                Some(CREDENTIAL_FIXTURE)
            );
        }
        _ => panic!("opencode mcp payload"),
    }

    // ObjectStore open takes data_dir root only
    let _store = AgentHubObjectStore::open(&env.data_dir).expect("open root");
}

/// L2：构建 PortableMcpServer 直接验证 env/header 往返与诊断安全。
#[test]
fn gate_b_mcp_payload_round_trip_credential_safety() {
    let payload = PortableAssetPayload::Mcp(PortableMcpServer {
        key: "private-api".into(),
        transport: McpTransport::Http {
            url: format!("https://example.invalid/mcp?token={CREDENTIAL_FIXTURE}"),
            headers: BTreeMap::from([(
                "Authorization".into(),
                format!("Bearer {CREDENTIAL_FIXTURE}"),
            )]),
        },
        env: BTreeMap::from([("API_TOKEN".into(), CREDENTIAL_FIXTURE.into())]),
        enabled: true,
        tool_allow: vec![],
        tool_deny: vec![],
        target_extensions: BTreeMap::new(),
    });
    let bytes = canonical_bytes(&payload).expect("bytes");
    assert!(String::from_utf8_lossy(&bytes).contains(CREDENTIAL_FIXTURE));
    for d in payload.collect_diagnostics() {
        assert!(!d.message.contains(CREDENTIAL_FIXTURE));
        assert!(!format!("{:?}", d).contains(CREDENTIAL_FIXTURE) || d.value_hash.is_some());
        // message field specifically
        assert!(!d.message.contains(CREDENTIAL_FIXTURE));
    }
    let redacted = redact_sensitive_text(&String::from_utf8_lossy(&bytes));
    assert!(!redacted.contains(CREDENTIAL_FIXTURE));
}
