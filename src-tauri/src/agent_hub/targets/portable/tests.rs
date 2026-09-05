//! agent_hub/targets/portable/tests — portable 扫描/解析/渲染内嵌单测
//!
//! Business Logic（为什么需要这个模块）:
//!     portable 共享扫描与渲染 helper 被所有 CLI Agent 适配器复用，frontmatter 解码、
//!     树 hash 缓存失效、store/逃逸软链语义、MCP content_hash 同域与投影渲染必须有
//!     回归测试保护。
//!
//! Code Logic（这个模块做什么）:
//!     原 portable.rs 内嵌 `mod tests` 原样迁移：经 `use super::*` 访问 portable 门面
//!     全部公共项，并按需显式引入 assets DTO、adapter 与 std 类型；覆盖 frontmatter
//!     解码、hash 缓存、七 adapter 扫描、MCP hash 同域、插件组件扫描与渲染 round-trip。

use super::frontmatter::unescape_json_string_inner;
use super::*;
use crate::agent_hub::assets::{PortableAssetPayload, PortableCommand, CODE_UNKNOWN_SOURCE_FIELD};
use crate::agent_hub::models::{AgentTarget, AssetKind, ScopeKind};
use crate::agent_hub::targets::{
    AssetAdapter, ClaudeInstructionAdapter, CodexInstructionAdapter, CursorInstructionAdapter,
    GeminiInstructionAdapter, GrokInstructionAdapter, LocalScopeMapping,
    OpenCodeInstructionAdapter, PiInstructionAdapter, TargetEnvironment,
};
use std::collections::BTreeMap;
use std::collections::BTreeMap as Map;
use std::fs;
use std::path::{Path, PathBuf};

fn write(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, text).unwrap();
}

#[test]
fn parse_simple_frontmatter_decodes_quoted_unicode_escapes() {
    let text = concat!(
            "---\n",
            "name: \"reinvent-from-scratch\"\n",
            "description: \"\\u7528\\u6770\\u68ee\\u00b7\\u5a01\\u5c14\\u514b\\u65af\\\"\\u524d\\u6570\\u5b66\\\"\"\n",
            "---\n# body\n",
        );
    let (fields, _, body) = parse_simple_frontmatter(text);
    assert_eq!(
        fields.get("description").map(String::as_str),
        Some("用杰森·威尔克斯\"前数学\"")
    );
    assert_eq!(
        fields.get("name").map(String::as_str),
        Some("reinvent-from-scratch")
    );
    assert_eq!(body, "# body\n");
}

#[test]
fn parse_simple_frontmatter_keeps_unquoted_utf8_chinese() {
    let text = "---\nname: huashu\ndescription: 花叔Design——用HTML做高保真原型\n---\nbody\n";
    let (fields, _, _) = parse_simple_frontmatter(text);
    assert_eq!(
        fields.get("description").map(String::as_str),
        Some("花叔Design——用HTML做高保真原型")
    );
}

#[test]
fn parse_simple_frontmatter_single_quotes_unescape_doubled_quote() {
    let text = "---\nname: 'it''s ok'\ndescription: 'plain'\n---\n";
    let (fields, _, _) = parse_simple_frontmatter(text);
    assert_eq!(fields.get("name").map(String::as_str), Some("it's ok"));
    assert_eq!(fields.get("description").map(String::as_str), Some("plain"));
}

#[test]
fn unescape_json_string_inner_keeps_invalid_unicode_escape() {
    assert_eq!(unescape_json_string_inner(r"\uZZZZ left"), r"\uZZZZ left");
    assert_eq!(unescape_json_string_inner(r"\u7528"), "用");
}

/// Business Logic（为什么需要这个测试）:
///     push/pull 打包必须能跟随「仓库真树 + Agent 软链」的逃逸根读取内容，
///     而本机写路径 hash 仍保持逃逸 fail-closed；断链软链两种模式都拒绝。
///
/// Code Logic（这个测试做什么）:
///     真树 + 软链根；deref hash 与直读真树 hash 一致；deref 拒绝断链；
///     `hash_skill_directory` 对软链根保持拒绝。
#[test]
fn hash_skill_directory_dereferenced_follows_escape_root_only() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("repo/skill-a");
    fs::create_dir_all(&real).unwrap();
    fs::write(
        real.join("SKILL.md"),
        "---\nname: skill-a\ndescription: d\n---\nbody\n",
    )
    .unwrap();
    let link = tmp.path().join("claude-skills/skill-a");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    let broken = tmp.path().join("claude-skills/broken");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real, &link).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("missing"), &broken).unwrap();
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(&real, &link).unwrap();
        std::os::windows::fs::symlink_dir(tmp.path().join("missing"), &broken).unwrap();
    }

    let deref = hash_skill_directory_dereferenced(&link).expect("dereferenced hash");
    let direct = hash_skill_directory(&real).expect("direct hash");
    assert_eq!(
        deref.1, direct.1,
        "dereferenced tree hash must match the real tree"
    );

    assert!(
        hash_skill_directory_dereferenced(&broken).is_err(),
        "dangling escape link must be rejected"
    );
    assert!(
        hash_skill_directory(&link).is_err(),
        "write-path hash must keep rejecting escape roots"
    );
}

#[test]
fn plugin_enablement_writes_viewing_agent_not_owner() {
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::Grok,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Plugin,
            true,
        ),
        AgentTarget::Grok
    );
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::Grok,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Plugin,
            false,
        ),
        AgentTarget::Claude
    );
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::Grok,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Skill,
            true,
        ),
        AgentTarget::Claude
    );
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::Codex,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Plugin,
            true,
        ),
        AgentTarget::Codex
    );
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::OpenCode,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Plugin,
            true,
        ),
        AgentTarget::OpenCode
    );
    assert_eq!(
        mutation_target_for_action(
            AgentTarget::Cursor,
            PortableAssetOwner::Claude,
            false,
            AssetKind::Plugin,
            true,
        ),
        AgentTarget::Cursor
    );
}

#[test]
fn borrowed_runtime_origin_is_owner_based_not_drift_or_legacy() {
    assert!(
        !is_borrowed_runtime_origin(
            AgentTarget::Claude,
            PortableAssetOwner::Claude,
            true,
            PortableOriginKind::Native,
        ),
        "same-agent native is installed"
    );
    assert!(
        !is_borrowed_runtime_origin(
            AgentTarget::Claude,
            PortableAssetOwner::Claude,
            false,
            PortableOriginKind::Native,
        ),
        "same-agent native stays installed even when not a native output candidate"
    );
    assert!(
        !is_borrowed_runtime_origin(
            AgentTarget::Codex,
            PortableAssetOwner::Codex,
            false,
            PortableOriginKind::LegacyStandalone,
        ),
        "Codex ~/.agents/skills is this Agent's install, not borrowed"
    );
    assert!(
        !is_borrowed_runtime_origin(
            AgentTarget::Claude,
            PortableAssetOwner::PortableStore,
            true,
            PortableOriginKind::Native,
        ),
        "store attached on native path is installed"
    );
    assert!(
        !is_borrowed_runtime_origin(
            AgentTarget::Codex,
            PortableAssetOwner::PortableStore,
            false,
            PortableOriginKind::LegacyStandalone,
        ),
        "store attached on Codex legacy root is installed"
    );
    assert!(is_borrowed_runtime_origin(
        AgentTarget::Grok,
        PortableAssetOwner::Claude,
        false,
        PortableOriginKind::Compatibility,
    ));
    assert!(is_borrowed_runtime_origin(
        AgentTarget::Grok,
        PortableAssetOwner::SharedAgents,
        true,
        PortableOriginKind::Native,
    ));
    assert!(is_borrowed_runtime_origin(
        AgentTarget::Grok,
        PortableAssetOwner::PortableStore,
        false,
        PortableOriginKind::Compatibility,
    ));
}

fn isolated_fixture() -> (tempfile::TempDir, TargetEnvironment) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    // Claude
    write(
        &home.join(".claude/skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review changes\ncustomFlag: keep-me\n---\n# Review\n",
    );
    write(
        &home.join(".claude/commands/release.md"),
        "---\nname: release\ndescription: Cut release\n---\nShip $ARGUMENTS\n",
    );
    write(
        &home.join(".claude/agents/reviewer.md"),
        "---\nname: reviewer\ndescription: Reviews PRs\nmodel: sonnet\n---\nBe thorough.\n",
    );
    // CLAUDE_CONFIG_DIR 指向 ~/.claude 时，MCP 配置为 <CLAUDE_CONFIG_DIR>/.claude.json
    write(
        &home.join(".claude/.claude.json"),
        r#"{
  "mcpServers": {
    "private-api": {
      "type": "http",
      "url": "https://example.invalid/mcp?token=plain-fixture",
      "headers": { "Authorization": "Bearer plain-fixture" },
      "env": { "API_TOKEN": "plain-fixture" }
    }
  }
}"#,
    );
    // agents compat
    write(
        &home.join(".agents/skills/review/SKILL.md"),
        "---\nname: review\ndescription: Agents copy\n---\n# Agents review\n",
    );
    // Codex
    write(
        &home.join(".codex/config.toml"),
        r#"
model = "o3"

[mcp_servers.private-api]
command = "uvx"
args = ["srv"]
env = { API_TOKEN = "plain-fixture" }

[agents.reviewer]
description = "Reviews"
config_file = "agents/reviewer.md"
"#,
    );
    write(
        &home.join(".codex/agents/reviewer.md"),
        "Codex reviewer instructions\n",
    );
    // OpenCode native under XDG-style default ~/.config/opencode — use OPENCODE_CONFIG_DIR
    write(
        &home.join(".opencode/skills/review/SKILL.md"),
        "---\nname: review\ndescription: OC skill\n---\n# OC\n",
    );
    write(
        &home.join(".opencode/commands/release.md"),
        "---\nname: release\n---\nOC release\n",
    );
    write(
        &home.join(".opencode/agents/reviewer.md"),
        "---\nname: reviewer\n---\nOC agent\n",
    );
    write(
        &home.join("opencode.jsonc"),
        r#"{
  // keep comment
  "mcpServers": {
    "private-api": {
      "command": "uvx",
      "args": ["oc-srv"],
      "env": { "API_TOKEN": "plain-fixture" }
    }
  }
}
"#,
    );
    let mut vars = Map::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        home.join(".claude").to_string_lossy().into(),
    );
    vars.insert(
        "CODEX_HOME".into(),
        home.join(".codex").to_string_lossy().into(),
    );
    vars.insert(
        "OPENCODE_CONFIG_DIR".into(),
        home.join(".opencode").to_string_lossy().into(),
    );
    vars.insert(
        "OPENCODE_CONFIG".into(),
        home.join("opencode.jsonc").to_string_lossy().into(),
    );
    let env = TargetEnvironment {
        home,
        vars,
        path_entries: vec![],
    };
    (dir, env)
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

#[test]
fn incremental_skill_hash_cache_invalidates_when_tree_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("skills");
    let skill = root.join("review");
    write(&skill.join("SKILL.md"), "---\nname: review\n---\nfirst\n");
    write(&skill.join("notes.txt"), "one\n");

    let first = scan_skill_dirs(
        AgentTarget::Claude,
        ScopeKind::User,
        &root,
        PortableOriginKind::Native,
    )
    .expect("first scan");
    let second = scan_skill_dirs(
        AgentTarget::Claude,
        ScopeKind::User,
        &root,
        PortableOriginKind::Native,
    )
    .expect("cached scan");
    assert_eq!(first[0].origin.tree_hash, second[0].origin.tree_hash);

    write(&skill.join("notes.txt"), "two changed\n");
    let changed = scan_skill_dirs(
        AgentTarget::Claude,
        ScopeKind::User,
        &root,
        PortableOriginKind::Native,
    )
    .expect("changed scan");
    assert_ne!(first[0].origin.tree_hash, changed[0].origin.tree_hash);
}

#[test]
fn claude_scan_finds_skill_command_agent_mcp() {
    let (_tmp, env) = isolated_fixture();
    let scope = user_scope(&env.home);
    let found = ClaudeInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    assert!(
        found.iter().any(|d| d.kind == AssetKind::Skill
            && d.semantic_name == "review"
            && d.origin.origin_kind == PortableOriginKind::Native),
        "skills={found:?}"
    );
    assert!(found
        .iter()
        .any(|d| d.kind == AssetKind::Command && d.semantic_name == "release"));
    assert!(found
        .iter()
        .any(|d| d.kind == AssetKind::Agent && d.semantic_name == "reviewer"));
    let mcp = found
        .iter()
        .find(|d| d.kind == AssetKind::Mcp && d.semantic_name == "private-api")
        .expect("mcp");
    match &mcp.payload {
        PortableAssetPayload::Mcp(s) => {
            assert_eq!(
                s.env.get("API_TOKEN").map(String::as_str),
                Some("plain-fixture")
            );
        }
        _ => panic!("expected mcp"),
    }
    // unknown frontmatter retained
    let skill = found.iter().find(|d| d.kind == AssetKind::Skill).unwrap();
    match &skill.payload {
        PortableAssetPayload::Skill(s) => {
            let ext = s.target_extensions.get(&AgentTarget::Claude).unwrap();
            assert_eq!(ext["customFlag"], "keep-me");
        }
        _ => panic!("skill"),
    }
    assert!(skill
        .diagnostics
        .iter()
        .any(|d| d.code == CODE_UNKNOWN_SOURCE_FIELD));
}

/// Codex MCP content_hash 必须等于 TomlConfigPatcher leaf inspect（含 int 字段）。
#[test]
fn parse_codex_mcp_content_hash_matches_toml_leaf_with_integers() {
    use crate::agent_hub::config_patch::{SemanticConfigPatcher, TomlConfigPatcher};

    let text = r#"
[mcp_servers.node_repl]
command = "node"
startup_timeout_sec = 120
args = ["mcp"]
enabled = true
"#;
    let path = PathBuf::from("/tmp/codex-config.toml");
    let found =
        parse_codex_mcp_toml(AgentTarget::Codex, ScopeKind::User, text, &path).expect("parse");
    assert_eq!(found.len(), 1);
    let owned = TomlConfigPatcher
        .inspect(text.as_bytes(), &["mcp_servers".into(), "node_repl".into()])
        .expect("inspect");
    assert!(owned.present);
    assert_eq!(
        found[0].origin.content_hash,
        owned.value_hash.expect("hash"),
        "scan content_hash must share CAS domain with apply Toml leaf"
    );
    // 不完整 string-only 重建不得冒充 content_hash
    let incomplete = serde_json::json!({
        "command": "node",
        "args": ["mcp"],
        "enabled": true,
    });
    assert_ne!(
        found[0].origin.content_hash,
        crate::agent_hub::config_patch::value_content_hash(&incomplete)
    );
}

#[test]
fn mcp_content_hash_is_key_order_independent_like_value_content_hash() {
    // inventory content_hash 必须与 action CAS 的 value_content_hash 同域，
    // 否则 ensure/reconcile 会因键序把健康 MCP 标成 drift。
    use crate::agent_hub::config_patch::value_content_hash;
    use serde_json::json;

    let a = json!({
        "type": "stdio",
        "command": "npx",
        "args": ["-y", "ctx"],
    });
    let b = json!({
        "args": ["-y", "ctx"],
        "command": "npx",
        "type": "stdio",
    });
    let mut map_a = serde_json::Map::new();
    map_a.insert("ctx".into(), a);
    let mut map_b = serde_json::Map::new();
    map_b.insert("ctx".into(), b);
    let path = PathBuf::from("/tmp/mcp-hash-order.json");
    let disc_a = parse_mcp_servers_json_map(
        AgentTarget::Claude,
        ScopeKind::User,
        &map_a,
        &path,
        PortableOriginKind::Native,
        true,
    );
    let disc_b = parse_mcp_servers_json_map(
        AgentTarget::Claude,
        ScopeKind::User,
        &map_b,
        &path,
        PortableOriginKind::Native,
        true,
    );
    assert_eq!(disc_a.len(), 1);
    assert_eq!(disc_b.len(), 1);
    assert_eq!(
        disc_a[0].origin.content_hash, disc_b[0].origin.content_hash,
        "reordered mcp leaf keys must share content_hash"
    );
    let expected = value_content_hash(map_a.get("ctx").unwrap());
    assert_eq!(
        disc_a[0].origin.content_hash, expected,
        "content_hash must equal value_content_hash(leaf)"
    );
}

#[test]
fn codex_scan_mcp_and_legacy_agents_skills() {
    let (_tmp, env) = isolated_fixture();
    let scope = user_scope(&env.home);
    let found = CodexInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    assert!(found.iter().any(|d| d.kind == AssetKind::Mcp));
    let legacy = found
        .iter()
        .find(|d| {
            d.kind == AssetKind::Skill
                && d.origin.origin_kind == PortableOriginKind::LegacyStandalone
        })
        .expect("legacy .agents/skills");
    assert!(legacy.origin.path.to_string_lossy().contains(".agents"));
    assert!(!legacy.origin.native_output_candidate);
    assert_eq!(legacy.origin.owned_by, PortableAssetOwner::Codex);
    assert!(found.iter().any(|d| d.kind == AssetKind::Agent));
}

#[test]
fn opencode_marks_compat_origins_not_native_output() {
    let (_tmp, env) = isolated_fixture();
    let scope = user_scope(&env.home);
    let found = OpenCodeInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let native_skills: Vec<_> = found
        .iter()
        .filter(|d| {
            d.kind == AssetKind::Skill && d.origin.origin_kind == PortableOriginKind::Native
        })
        .collect();
    assert_eq!(native_skills.len(), 1);
    assert!(native_skills[0]
        .origin
        .path
        .to_string_lossy()
        .contains(".opencode"));
    let compat: Vec<_> = found
        .iter()
        .filter(|d| {
            d.kind == AssetKind::Skill && d.origin.origin_kind == PortableOriginKind::Compatibility
        })
        .collect();
    assert!(
        compat.len() >= 2,
        "expected .claude and .agents compat, got {compat:?}"
    );
    assert!(compat.iter().all(|d| !d.origin.native_output_candidate));
    // same semantic name, separate discoveries
    let reviews: Vec<_> = found
        .iter()
        .filter(|d| d.kind == AssetKind::Skill && d.semantic_name == "review")
        .collect();
    assert!(reviews.len() >= 3, "reviews={reviews:?}");
    let paths: std::collections::BTreeSet<_> =
        reviews.iter().map(|d| d.origin.path.clone()).collect();
    assert_eq!(paths.len(), reviews.len());
}

#[test]
fn scan_does_not_write_files() {
    let (_tmp, env) = isolated_fixture();
    let scope = user_scope(&env.home);
    let before = walk_snapshot(&env.home);
    let _ = ClaudeInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = CodexInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = OpenCodeInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = GrokInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = GeminiInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = CursorInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let _ = PiInstructionAdapter
        .scan_portable_assets(&scope, &env)
        .unwrap();
    let after = walk_snapshot(&env.home);
    assert_eq!(before, after);
}

fn walk_snapshot(root: &Path) -> Vec<(String, u64)> {
    let mut v = Vec::new();
    for e in walkdir::WalkDir::new(root).follow_links(false) {
        let e = e.unwrap();
        if e.file_type().is_file() {
            let rel = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let len = e.metadata().unwrap().len();
            v.push((rel, len));
        }
    }
    v.sort();
    v
}

#[test]
fn render_round_trip_command() {
    let cmd = PortableCommand {
        name: "release".into(),
        description: Some("d".into()),
        prompt_template: "go".into(),
        arguments: vec![],
        target_extensions: BTreeMap::new(),
    };
    let proj = render_command_projection(AgentTarget::Claude, &cmd);
    assert_eq!(proj.files[0].relative_path, "commands/release.md");
    let text = String::from_utf8(proj.files[0].bytes.clone()).unwrap();
    assert!(text.contains("name: release"));
    assert!(text.contains("go"));
}

#[test]
fn skill_scan_follows_store_symlink_and_rejects_escape() {
    let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);
    let store = crate::agent_hub::portable_store::ensure_portable_store_layout(&data).unwrap();
    let skill = crate::agent_hub::portable_store::store_skill_dir(&store, "foo");
    write(
        &skill.join("SKILL.md"),
        "---\nname: foo\ndescription: Store skill\n---\n# Foo\n",
    );
    let native_root = dir.path().join("skills");
    fs::create_dir_all(&native_root).unwrap();
    crate::agent_hub::portable_store::create_store_link(&skill, &native_root.join("foo")).unwrap();
    let found = scan_skill_dirs(
        AgentTarget::Claude,
        ScopeKind::User,
        &native_root,
        PortableOriginKind::Native,
    )
    .unwrap();
    let store_hit = found
        .iter()
        .find(|a| a.origin.native_id == "foo")
        .expect("store skill");
    assert_eq!(store_hit.origin.owned_by, PortableAssetOwner::PortableStore);
    assert_eq!(store_hit.origin.status, PortableDiscoveryStatus::Active);
    assert!(!store_hit.origin.content_hash.is_empty());

    let escape = dir.path().join("escape");
    fs::create_dir_all(&escape).unwrap();
    write(&escape.join("SKILL.md"), "---\nname: evil\n---\n# x\n");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&escape, native_root.join("evil")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&escape, native_root.join("evil")).unwrap();
    let again = scan_skill_dirs(
        AgentTarget::Claude,
        ScopeKind::User,
        &native_root,
        PortableOriginKind::Native,
    )
    .unwrap();
    let blocked = again
        .iter()
        .find(|a| a.origin.native_id == "evil")
        .expect("escape");
    assert_eq!(blocked.origin.status, PortableDiscoveryStatus::Blocked);
    assert!(blocked
        .diagnostics
        .iter()
        .any(|d| d.code == "store_symlink_escape"));
    assert!(!blocked.origin.content_hash.is_empty());
    let followed = hash_skill_directory(&escape).unwrap();
    assert_ne!(
        blocked.origin.content_hash, followed.0,
        "escape identity must not follow the target SKILL.md"
    );
    assert!(hash_skill_directory(&native_root.join("evil")).is_err());
    std::env::remove_var("CC_PARTNER_DATA_DIR");
}

#[test]
fn skill_scan_expands_package_without_root_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skills");
    write(
        &root.join("superpowers/using-superpowers/SKILL.md"),
        "---\nname: using-superpowers\ndescription: Start here\n---\n# Use\n",
    );
    write(
        &root.join("superpowers/brainstorming/SKILL.md"),
        "---\nname: brainstorming\n---\n# Brainstorm\n",
    );
    write(&root.join("superpowers/README.md"), "# pack\n");
    write(
        &root.join("flat/SKILL.md"),
        "---\nname: flat\n---\n# Flat\n",
    );
    write(
        &root.join("nested-root/SKILL.md"),
        "---\nname: nested-root\n---\n# Root\n",
    );
    write(
        &root.join("nested-root/child/SKILL.md"),
        "---\nname: hidden-child\n---\n# Child\n",
    );

    let found = scan_skill_dirs(
        AgentTarget::Grok,
        ScopeKind::User,
        &root,
        PortableOriginKind::Compatibility,
    )
    .unwrap();
    let names: Vec<&str> = found.iter().map(|a| a.origin.native_id.as_str()).collect();
    assert!(names.contains(&"using-superpowers"));
    assert!(names.contains(&"brainstorming"));
    assert!(names.contains(&"flat"));
    assert!(names.contains(&"nested-root"));
    assert!(
        !names.contains(&"superpowers"),
        "package without SKILL.md must not appear as itself"
    );
    assert!(
        !names.contains(&"hidden-child"),
        "package with root SKILL.md must not expand children"
    );
    let nested = found
        .iter()
        .find(|a| a.origin.native_id == "using-superpowers")
        .expect("nested skill");
    assert_eq!(nested.semantic_name, "using-superpowers");
    assert!(nested
        .diagnostics
        .iter()
        .any(|d| d.code == "nested_skill_package"));
    assert!(nested
        .origin
        .path
        .ends_with("superpowers/using-superpowers"));
}

#[test]
fn skill_scan_expands_skills_subdirectory_inside_package() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skills");
    write(
        &root.join("pack/skills/review/SKILL.md"),
        "---\nname: review\n---\n# Review\n",
    );
    write(&root.join("pack/docs/notes.md"), "not a skill\n");
    let found = scan_skill_dirs(
        AgentTarget::Grok,
        ScopeKind::User,
        &root,
        PortableOriginKind::LegacyStandalone,
    )
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].origin.native_id, "review");
    assert!(found[0].origin.path.ends_with("pack/skills/review"));
}

#[test]
fn skill_scan_follows_store_symlink_package_without_root_manifest() {
    let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);
    let store = crate::agent_hub::portable_store::ensure_portable_store_layout(&data).unwrap();
    let package = crate::agent_hub::portable_store::store_skill_dir(&store, "superpowers");
    write(
        &package.join("using-superpowers/SKILL.md"),
        "---\nname: using-superpowers\n---\n# Use\n",
    );
    let native_root = dir.path().join("skills");
    fs::create_dir_all(&native_root).unwrap();
    crate::agent_hub::portable_store::create_store_link(&package, &native_root.join("superpowers"))
        .unwrap();
    let found = scan_skill_dirs(
        AgentTarget::Grok,
        ScopeKind::User,
        &native_root,
        PortableOriginKind::Compatibility,
    )
    .unwrap();
    let nested = found
        .iter()
        .find(|a| a.origin.native_id == "using-superpowers")
        .expect("nested store skill");
    assert_eq!(nested.origin.owned_by, PortableAssetOwner::PortableStore);
    assert!(nested
        .diagnostics
        .iter()
        .any(|d| d.code == "nested_skill_package"));
    std::env::remove_var("CC_PARTNER_DATA_DIR");
}
