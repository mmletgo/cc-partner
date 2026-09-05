//! portable_inventory/scanner/tests — scanner 目录模块单元测试
//!
//! Business Logic（为什么需要这个模块）:
//!     origin/enabled/parentPlugin/hash/capability 规则与各 CLI Agent 的磁盘布局强相关，
//!     需要隔离 HOME fixture 回归验证扫描行为只读、根候选收敛正确且能力判定不越权。
//!
//! Code Logic（这个模块做什么）:
//!     原 scanner.rs 内联 `#[cfg(test)]` 测试区原样搬运；除 `use super::*` 外，
//!     显式导入兄弟子模块 plugin_roots / items / hashing 中升为 pub(super) 的内部项，
//!     覆盖 mod（入口/扫描）、plugin_roots（根候选）、hashing（树哈希）、items（能力判定）。

use super::items::{
    action_capability_reason, action_capability_supported, annotate_store_loaded_via_other_path,
    item_capabilities, mutation_capability_reason, mutation_gates_for_origin, should_replace_with,
    store_catalog_enabled,
};
use super::plugin_roots::{claude_user_plugin_roots, codex_user_plugin_roots, plugin_roots_for};
use crate::agent_hub::{
    models::{AgentTarget, ScopeKind},
    portable_actions::models::PortableAssetActionKind,
    portable_inventory::models::{
        inventory_snapshot_hash, PortableAssetKind, PortableInventoryItemCapabilitiesDto,
        PortableInventoryItemDto, PortableInventoryManagementState,
        PortableInventoryMutationCapability, PortableInventoryQuery,
        PortableInventoryScanCapability, PortableInventorySourceOrigin, PortableInventoryTargetDto,
        PortableStoreFactDto,
    },
    support::{CapabilitySupport, EvaluatedTargetSupport, TargetCapability},
    targets::{
        portable::{PortableAssetOwner, PortableOriginKind},
        LocalScopeMapping, TargetEnvironment, TargetPathResolver, TargetProbe,
    },
};
use std::{collections::BTreeMap, fs, path::Path};

use super::*;
use crate::agent_hub::{
    portable_inventory::{
        plugin_enablement::{
            parse_claude_plugin_enablement_from_settings, parse_codex_plugin_enablement_from_toml,
        },
        reconcile::reconcile_portable_inventory_with_facts,
    },
    targets::{
        AdapterSupportLevel, ClaudeInstructionAdapter, CodexInstructionAdapter,
        CursorInstructionAdapter, GeminiInstructionAdapter, GrokInstructionAdapter,
        OpenCodeInstructionAdapter, PiInstructionAdapter,
    },
};
use std::collections::BTreeMap as Map;

fn write(path: &Path, text: &str) {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn seed_all_targets_fixture() -> (tempfile::TempDir, TargetEnvironment) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();

    // --- Claude user ---
    write(
        &home.join(".claude/skills/review/SKILL.md"),
        "---\nname: review\ndescription: Review\n---\n# Review\n",
    );
    write(
        &home.join(".claude/disabled/skills/old-review/SKILL.md"),
        "---\nname: old-review\ndescription: Disabled\n---\n# Old\n",
    );
    write(
        &home.join(".claude/commands/ship.md"),
        "---\nname: ship\n---\nShip it\n",
    );
    write(
        &home.join(".claude/disabled/commands/legacy.md"),
        "---\nname: legacy\n---\nLegacy\n",
    );
    // standalone + plugin same-name skill
    write(
        &home.join(".claude/skills/shared-name/SKILL.md"),
        "---\nname: shared-name\n---\n# Standalone\n",
    );
    write(
        &home.join(".claude/plugins/demo-plugin/.claude-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"1.0.0","description":"Demo"}"#,
    );
    write(
        &home.join(".claude/plugins/demo-plugin/skills/shared-name/SKILL.md"),
        "---\nname: shared-name\n---\n# Plugin component\n",
    );
    write(
        &home.join(".claude/.claude.json"),
        r#"{
  "mcpServers": {
    "good-api": {
      "command": "uvx",
      "args": ["srv"],
      "env": { "API_TOKEN": "plain-fixture" },
      "enabled": true
    },
    "off-api": {
      "command": "uvx",
      "args": ["off"],
      "enabled": false
    }
  }
}"#,
    );
    // corrupt MCP sibling file for blocked diagnostic path (settings)
    write(&home.join(".claude/broken-mcp.json"), "{ not json !!");

    // --- Codex user ---
    write(
        &home.join(".codex/config.toml"),
        r#"
[mcp_servers.good-api]
command = "uvx"
args = ["srv"]
enabled = true
env = { API_TOKEN = "plain-fixture" }

[mcp_servers.off-api]
command = "uvx"
args = ["off"]
enabled = false
"#,
    );
    write(
        &home.join(".agents/skills/review/SKILL.md"),
        "---\nname: review\n---\n# Codex review\n",
    );
    write(
        &home.join(".codex/plugins/demo-plugin/.codex-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"0.2.0"}"#,
    );
    write(
        &home.join(".codex/plugins/demo-plugin/skills/shared-name/SKILL.md"),
        "---\nname: shared-name\n---\n# Codex plugin skill\n",
    );

    // --- OpenCode user ---
    write(
        &home.join(".opencode/skills/review/SKILL.md"),
        "---\nname: review\n---\n# OC\n",
    );
    write(
        &home.join(".opencode/disabled/skills/old/SKILL.md"),
        "---\nname: old\n---\n# old\n",
    );
    write(
        &home.join(".opencode/commands/ship.md"),
        "---\nname: ship\n---\nOC ship\n",
    );
    write(
        &home.join(".opencode/plugins/demo-plugin/package.json"),
        r#"{"name":"demo-plugin","version":"3.0.0"}"#,
    );
    write(
        &home.join(".opencode/plugins/demo-plugin/skills/shared-name/SKILL.md"),
        "---\nname: shared-name\n---\n# OC plugin\n",
    );
    write(
        &home.join("opencode.jsonc"),
        r#"{
  "mcpServers": {
    "good-api": {
      "command": "uvx",
      "args": ["oc"],
      "env": { "API_TOKEN": "plain-fixture" },
      "enabled": true
    }
  }
}
"#,
    );

    // Project fixtures
    let opted = home.join("proj-opted");
    let unopted = home.join("proj-unopted");
    write(
        &opted.join(".claude/skills/proj-skill/SKILL.md"),
        "---\nname: proj-skill\n---\n# P\n",
    );
    write(
        &unopted.join(".claude/skills/hidden/SKILL.md"),
        "---\nname: hidden\n---\n# H\n",
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
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    (dir, env)
}

fn user_and_projects(home: &Path) -> Vec<PortableScanScope> {
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

#[test]
fn scan_only_manifest_cannot_be_promoted_by_direct_local_allowlist() {
    let (_tmp, env) = seed_all_targets_fixture();
    let probe = TargetProbe {
        target: AgentTarget::Claude,
        executable: Some(env.home.join("bin/claude")),
        // 缺失 runtime version 会使 manifest 求值进入 scan-only；旧直管 allowlist
        // 仍然存在，但不得因此把 mutation capability 提升为 Supported。
        version: None,
        config_root: env.home.join(".claude"),
        support: AdapterSupportLevel::Supported,
        fingerprint: "fixture-fingerprint".into(),
    };

    let target = target_dto_from_probe(AgentTarget::Claude, &probe, &env).unwrap();

    assert_eq!(
        target.mutation_capability,
        PortableInventoryMutationCapability::Blocked
    );
    assert_eq!(target.reason_code.as_deref(), Some("cli_version_unknown"));
}

#[test]
fn sparse_gui_path_still_exposes_claude_plugin_and_mcp_toggles() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let local_bin = home.join(".local").join("bin");
    fs::create_dir_all(&local_bin).unwrap();
    let fake_cli = local_bin.join("claude");
    write(&fake_cli, "#!/bin/sh\necho '2.1.207 (Claude Code)'\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_cli).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_cli, perms).unwrap();
    }
    write(
        &home.join(".claude/plugins/demo-plugin/.claude-plugin/plugin.json"),
        r#"{"name":"demo-plugin","version":"1.0.0"}"#,
    );
    write(
        &home.join(".claude/.claude.json"),
        r#"{
  "mcpServers": {
    "good-api": {
      "command": "uvx",
      "args": ["srv"],
      "enabled": true
    }
  }
}"#,
    );

    let mut vars = Map::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        home.join(".claude").to_string_lossy().into(),
    );
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: crate::agent_hub::targets::paths::gui_augmented_path_entries(
            &home,
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        ),
    };
    let scopes = [PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];

    let plugin_query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (plugin_targets, plugin_items) =
        scan_portable_inventory_facts_query(&env, &scopes, plugin_query).expect("plugin scan");
    let claude_target = plugin_targets
        .iter()
        .find(|t| t.target == AgentTarget::Claude)
        .expect("claude target");
    assert_eq!(
        claude_target.mutation_capability,
        PortableInventoryMutationCapability::Supported,
        "GUI 稀疏 PATH 仍应认证 ~/.local/bin/claude；got reason={:?}",
        claude_target.reason_code
    );
    let plugin = plugin_items
        .iter()
        .find(|i| i.native_id.contains("demo-plugin"))
        .expect("demo plugin");
    assert_eq!(plugin.actual_enabled, Some(true));
    assert!(
        plugin.capabilities.can_disable,
        "Claude plugin must expose disable when CLI is only in ~/.local/bin"
    );

    let mcp_query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Mcp),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_mcp_targets, mcp_items) =
        scan_portable_inventory_facts_query(&env, &scopes, mcp_query).expect("mcp scan");
    let mcp = mcp_items
        .iter()
        .find(|i| i.native_id == "good-api")
        .expect("mcp");
    assert_eq!(mcp.actual_enabled, Some(true));
    assert!(
        mcp.capabilities.can_disable,
        "Claude MCP must expose disable when CLI is only in ~/.local/bin"
    );
}

#[test]
fn preview_only_target_exposes_zero_mutation_affordances_and_reason() {
    let target = PortableInventoryTargetDto {
        target: AgentTarget::Claude,
        installed: true,
        version: Some("1.0.0".into()),
        executable: Some("/bin/claude".into()),
        config_root: "/cfg/claude".into(),
        scan_capability: PortableInventoryScanCapability::Supported,
        mutation_capability: PortableInventoryMutationCapability::PreviewOnly,
        reason_code: None,
        evidence_ids: vec![],
    };

    assert_ne!(
        target.mutation_capability,
        PortableInventoryMutationCapability::Supported
    );
    let capabilities = item_capabilities(
        target.target,
        target.target,
        PortableAssetKind::Skill,
        Some(true),
        false,
        false,
        false,
        true,
        mutation_capability_reason(&target),
        false,
        PortableOriginKind::Native,
        true,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(!capabilities.can_enable);
    assert!(!capabilities.can_disable);
    assert!(!capabilities.can_uninstall);
    assert_eq!(
        capabilities.reason_code.as_deref(),
        Some("portable_mutation_preview_only")
    );
}

#[test]
fn partial_manifest_plugin_deactivation_has_zero_remove_affordances() {
    let evaluated = EvaluatedTargetSupport {
        target: AgentTarget::Claude,
        mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
        capabilities: BTreeMap::from([
            (
                TargetCapability::RenderPortableAssets,
                CapabilitySupport::Supported,
            ),
            (
                TargetCapability::ActivatePackage,
                CapabilitySupport::Supported,
            ),
            (
                TargetCapability::DeactivatePackage,
                CapabilitySupport::Blocked,
            ),
        ]),
        write_allowed: true,
        reasons: vec![],
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
    let capabilities = item_capabilities(
        target.target,
        target.target,
        PortableAssetKind::Plugin,
        Some(false),
        action_capability_supported(
            &evaluated,
            target.target,
            PortableAssetKind::Plugin,
            PortableAssetActionKind::Enable,
        ),
        action_capability_supported(
            &evaluated,
            target.target,
            PortableAssetKind::Plugin,
            PortableAssetActionKind::Disable,
        ),
        action_capability_supported(
            &evaluated,
            target.target,
            PortableAssetKind::Plugin,
            PortableAssetActionKind::Uninstall,
        ),
        true,
        action_capability_reason(
            &target,
            &evaluated,
            target.target,
            PortableAssetKind::Plugin,
        ),
        false,
        PortableOriginKind::Native,
        true,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );

    assert!(capabilities.can_enable);
    assert!(!capabilities.can_disable);
    assert!(!capabilities.can_uninstall);
    assert_eq!(
        capabilities.reason_code.as_deref(),
        Some("deactivate_package_not_supported")
    );
}

#[test]
fn file_only_codex_plugin_toggle_survives_blocked_cli() {
    let evaluated = EvaluatedTargetSupport {
        target: AgentTarget::Codex,
        mode: crate::agent_hub::support::EvaluatedSupportMode::ScanOnly {
            reasons: vec!["cli_version_unknown".into()],
        },
        capabilities: BTreeMap::from([
            (
                TargetCapability::ActivatePackage,
                CapabilitySupport::Blocked,
            ),
            (
                TargetCapability::DeactivatePackage,
                CapabilitySupport::Blocked,
            ),
            (
                TargetCapability::RenderPortableAssets,
                CapabilitySupport::Blocked,
            ),
        ]),
        write_allowed: false,
        reasons: vec!["cli_version_unknown".into()],
    };
    let (can_enable, can_disable, can_uninstall, _, _) = mutation_gates_for_origin(
        AgentTarget::Codex,
        PortableAssetOwner::Codex,
        true,
        PortableOriginKind::Native,
        PortableAssetKind::Plugin,
        &evaluated,
        true,
    );
    assert!(
        can_enable && can_disable,
        "Codex plugin enable/disable is a file toggle and must not wait for CLI probe"
    );
    assert!(
        !can_uninstall,
        "Codex plugin uninstall still requires DeactivatePackage"
    );
}

#[test]
fn file_only_grok_borrowed_plugin_toggle_survives_blocked_cli() {
    let evaluated = EvaluatedTargetSupport {
        target: AgentTarget::Grok,
        mode: crate::agent_hub::support::EvaluatedSupportMode::ScanOnly {
            reasons: vec!["cli_version_unknown".into()],
        },
        capabilities: BTreeMap::from([
            (
                TargetCapability::ActivatePackage,
                CapabilitySupport::Blocked,
            ),
            (
                TargetCapability::DeactivatePackage,
                CapabilitySupport::Blocked,
            ),
            (
                TargetCapability::RenderPortableAssets,
                CapabilitySupport::Blocked,
            ),
        ]),
        write_allowed: false,
        reasons: vec!["cli_version_unknown".into()],
    };
    let (can_enable, can_disable, can_uninstall, enablement, owner) = mutation_gates_for_origin(
        AgentTarget::Grok,
        PortableAssetOwner::Claude,
        false,
        PortableOriginKind::Compatibility,
        PortableAssetKind::Plugin,
        &evaluated,
        true,
    );
    assert!(
        can_enable && can_disable,
        "Grok borrowed plugin enable/disable is a file toggle on viewing flags"
    );
    assert_eq!(enablement, AgentTarget::Grok);
    assert_eq!(owner, AgentTarget::Claude);
    assert!(
        can_uninstall,
        "borrowed plugin uninstall still goes to Claude owner allowlist"
    );
}

#[test]
fn partial_manifest_render_only_keeps_non_plugin_actions_available() {
    let evaluated = EvaluatedTargetSupport {
        target: AgentTarget::Claude,
        mode: crate::agent_hub::support::EvaluatedSupportMode::Certified,
        capabilities: BTreeMap::from([
            (
                TargetCapability::RenderPortableAssets,
                CapabilitySupport::Supported,
            ),
            (
                TargetCapability::ActivatePackage,
                CapabilitySupport::Blocked,
            ),
            (
                TargetCapability::DeactivatePackage,
                CapabilitySupport::Blocked,
            ),
        ]),
        write_allowed: true,
        reasons: vec![],
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
    let can_render = action_capability_supported(
        &evaluated,
        target.target,
        PortableAssetKind::Skill,
        PortableAssetActionKind::Disable,
    );
    let capabilities = item_capabilities(
        target.target,
        target.target,
        PortableAssetKind::Skill,
        Some(true),
        can_render,
        can_render,
        can_render,
        true,
        action_capability_reason(&target, &evaluated, target.target, PortableAssetKind::Skill),
        false,
        PortableOriginKind::Native,
        true,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );

    assert!(!capabilities.can_disable);
    assert!(!capabilities.can_uninstall);
    assert!(capabilities.can_migrate_to_store);
    assert!(capabilities.reason_code.is_none());
}

#[test]
fn uncertified_opencode_executor_remains_blocked() {
    let (_tmp, env) = seed_all_targets_fixture();
    // OpenCode 仍无 min/current pin → evaluate 写能力 fail-closed。
    let probe = TargetProbe {
        target: AgentTarget::OpenCode,
        executable: Some(env.home.join("bin/opencode")),
        version: Some("1.0.0".into()),
        config_root: env.home.join(".opencode"),
        support: AdapterSupportLevel::Supported,
        fingerprint: "fixture-fingerprint".into(),
    };

    let target_dto = target_dto_from_probe(AgentTarget::OpenCode, &probe, &env).unwrap();
    assert_eq!(
        target_dto.mutation_capability,
        PortableInventoryMutationCapability::Blocked
    );
}

#[test]
fn codex_known_version_unlocks_portable_mutation_after_phase1_certification() {
    let (_tmp, env) = seed_all_targets_fixture();
    let probe = TargetProbe {
        target: AgentTarget::Codex,
        executable: Some(env.home.join("bin/codex")),
        // phase-1 认证后 manifest 已 pin codex 0.145.0-alpha.4；匹配版本应解锁 mutation。
        version: Some("codex-cli 0.145.0-alpha.4".into()),
        config_root: env.home.join(".codex"),
        support: AdapterSupportLevel::Supported,
        fingerprint: "fixture-fingerprint".into(),
    };

    let target_dto = target_dto_from_probe(AgentTarget::Codex, &probe, &env).unwrap();
    assert_eq!(
        target_dto.mutation_capability,
        PortableInventoryMutationCapability::Supported,
        "phase-1 certified codex runtime must unlock portable write mutation"
    );
}

#[test]
fn scan_finds_four_kinds_per_target_with_enabled_and_plugin_parent() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let (targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");
    assert_eq!(targets.len(), AgentTarget::ALL.len());

    for target in [
        AgentTarget::Claude,
        AgentTarget::Codex,
        AgentTarget::OpenCode,
    ] {
        let t_items: Vec<_> = items.iter().filter(|i| i.target == target).collect();
        assert!(
            t_items.iter().any(|i| i.kind == PortableAssetKind::Skill),
            "{target:?} missing skill: {t_items:?}"
        );
        assert!(
            t_items.iter().any(|i| i.kind == PortableAssetKind::Plugin),
            "{target:?} missing plugin package"
        );
        assert!(
            t_items.iter().any(|i| i.kind == PortableAssetKind::Mcp),
            "{target:?} missing mcp"
        );
    }

    // Claude command present
    assert!(items
        .iter()
        .any(|i| { i.target == AgentTarget::Claude && i.kind == PortableAssetKind::Command }));

    // disabled skill actualEnabled=false
    let disabled = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.native_id == "old-review"
                && i.kind == PortableAssetKind::Skill
        })
        .expect("disabled skill");
    assert_eq!(disabled.actual_enabled, Some(false));

    // active skill actualEnabled=true
    let active = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.native_id == "review"
                && i.source_origin == PortableInventorySourceOrigin::Standalone
        })
        .expect("active skill");
    assert_eq!(active.actual_enabled, Some(true));
    assert!(active.content_hash.is_some());
    assert!(active.tree_hash.is_some());

    // MCP credential present/hash only + disabled MCP
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
    assert!(
        !mcp.capabilities.can_migrate_to_store
            && !mcp.capabilities.can_attach
            && !mcp.capabilities.can_detach
            && !mcp.capabilities.can_destroy_store,
        "MCP stays a native leaf, not a store attach item"
    );
    assert!(
        active.capabilities.can_migrate_to_store,
        "native Skill remains eligible for portable-store migrate"
    );
    assert!(
        !active.capabilities.can_enable && !active.capabilities.can_disable,
        "Skill/Command no longer expose enable/disable; store lifecycle replaced them"
    );
    let agents_skill = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Codex
                && i.kind == PortableAssetKind::Skill
                && i.native_id == "review"
                && i.source_path
                    .as_deref()
                    .is_some_and(|p| p.contains(".agents"))
        })
        .expect("codex ~/.agents skill");
    assert!(
        agents_skill.capabilities.can_migrate_to_store,
        "~/.agents Skill must be eligible to migrate into portable-store"
    );
    assert!(!agents_skill.capabilities.can_disable);
    assert!(!agents_skill.capabilities.can_detach);
    let wire = serde_json::to_value(cred).unwrap();
    assert!(!wire.to_string().contains("plain-fixture"));

    let off = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Mcp
                && i.native_id == "off-api"
        })
        .expect("disabled mcp");
    assert_eq!(off.actual_enabled, Some(false));

    // plugin component has parent; standalone same name remains separate
    let standalone = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Skill
                && i.native_id == "shared-name"
                && i.source_origin == PortableInventorySourceOrigin::Standalone
        })
        .expect("standalone shared-name");
    let component = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Skill
                && i.native_id == "shared-name"
                && i.source_origin == PortableInventorySourceOrigin::PluginComponent
        })
        .expect("plugin component shared-name");
    assert_ne!(standalone.inventory_item_id, component.inventory_item_id);
    assert!(component.parent_plugin_inventory_item_id.is_some());
    let plugin = items
        .iter()
        .find(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Plugin
                && i.native_id == "demo-plugin"
        })
        .expect("plugin package");
    assert_eq!(
        component.parent_plugin_inventory_item_id.as_deref(),
        Some(plugin.inventory_item_id.as_str())
    );
}

#[test]
fn filtered_scan_limits_target_kind_and_scope_before_inventory_result() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Skill),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (targets, items) = scan_portable_inventory_facts_query(&env, &scopes, query).unwrap();

    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].target, AgentTarget::Claude);
    assert!(!items.is_empty());
    assert!(items.iter().all(|item| {
        item.target == AgentTarget::Claude
            && item.kind == PortableAssetKind::Skill
            && item.scope_kind == ScopeKind::User
            && item.content_hash.is_some()
            && item.tree_hash.is_none()
    }));
    assert!(items.iter().any(|item| {
        item.native_id == "shared-name"
            && item.source_origin == PortableInventorySourceOrigin::PluginComponent
    }));
    assert!(!items
        .iter()
        .any(|item| item.kind == PortableAssetKind::Plugin));
}

#[test]
fn filtered_plugin_list_defers_recursive_tree_hash() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_targets, items) = scan_portable_inventory_facts_query(&env, &scopes, query).unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|item| {
        item.kind == PortableAssetKind::Plugin
            && item.content_hash.is_some()
            && item.tree_hash.is_none()
    }));
}

#[test]
fn unresolved_local_project_query_fails_closed_before_scan() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let query = PortableInventoryQuery {
        scope_kind: Some(ScopeKind::Project),
        local_project_id: Some("workbench-project".into()),
        ..PortableInventoryQuery::default()
    };
    let error = scan_portable_inventory_facts_query(&env, &scopes, query)
        .expect_err("pure scanner must not accept an unresolved local project id");
    assert!(error
        .to_string()
        .contains("PORTABLE_INVENTORY_LOCAL_PROJECT_ID_UNRESOLVED"));
}

#[test]
fn unopted_project_is_read_only_and_opted_project_scanned() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let (_targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");

    let opted = items
        .iter()
        .find(|i| i.project_id.as_deref() == Some("opted"))
        .expect("opted project item");
    assert!(opted.project_opted_in);
    // 无真实 CLI 时 mutation 可仍 blocked；但不得因 unopted 规则关闭
    assert_ne!(
        opted.capabilities.reason_code.as_deref(),
        Some("project_not_opted_in")
    );

    let unopted = items
        .iter()
        .find(|i| i.project_id.as_deref() == Some("unopted"))
        .expect("unopted project item");
    assert!(!unopted.project_opted_in);
    assert!(!unopted.capabilities.can_enable);
    assert!(!unopted.capabilities.can_disable);
    assert!(!unopted.capabilities.can_uninstall);
    assert!(!unopted.capabilities.can_migrate_to_store);
    assert!(!unopted.capabilities.can_adopt);
    assert_eq!(
        unopted.capabilities.reason_code.as_deref(),
        Some("project_not_opted_in")
    );

    // P1-1: even writable unmanaged user assets must not advertise canAdopt until ownership write exists
    for item in &items {
        assert!(
            !item.capabilities.can_adopt,
            "canAdopt must stay false until adopt is wired: {}",
            item.inventory_item_id
        );
    }
}

#[test]
fn can_adopt_is_always_false_even_when_mutable() {
    let caps = item_capabilities(
        AgentTarget::Claude,
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        false,
        PortableOriginKind::Native,
        true,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(!caps.can_enable);
    assert!(!caps.can_disable);
    assert!(!caps.can_uninstall);
    assert!(caps.can_migrate_to_store);
    assert!(!caps.can_adopt);
}

#[test]
fn compatibility_discovery_does_not_offer_migrate_for_borrowed_runtime_skills() {
    let caps = item_capabilities(
        AgentTarget::Claude,
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        true,
        PortableOriginKind::Compatibility,
        false,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(!caps.can_enable);
    assert!(!caps.can_disable);
    assert!(!caps.can_uninstall);
    assert!(
        !caps.can_migrate_to_store,
        "Grok/Pi runtime-loaded skills must not expose 迁入便携仓库"
    );
    assert!(!caps.can_detach);
    assert!(!caps.can_install_to_source_target);
    assert_eq!(caps.reason_code.as_deref(), Some("borrowed_runtime_origin"));
}

#[test]
fn compatibility_on_uncertified_owner_still_has_zero_direct_actions() {
    let caps = item_capabilities(
        AgentTarget::OpenCode,
        AgentTarget::OpenCode,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        Some("cli_version_unknown".into()),
        true,
        PortableOriginKind::Compatibility,
        false,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(!caps.can_enable);
    assert!(!caps.can_disable);
    assert!(!caps.can_uninstall);
    assert!(!caps.can_migrate_to_store);
    assert!(!caps.can_detach);
    assert_eq!(caps.reason_code.as_deref(), Some("borrowed_runtime_origin"));
}

#[test]
fn uncertified_native_store_skills_can_detach() {
    let store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: true,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    for target in [
        AgentTarget::OpenCode,
        AgentTarget::Grok,
        AgentTarget::Gemini,
        AgentTarget::Cursor,
        AgentTarget::Pi,
    ] {
        let caps = item_capabilities(
            target,
            target,
            PortableAssetKind::Skill,
            Some(true),
            true,
            true,
            true,
            true,
            Some("cli_version_unknown".into()),
            false,
            PortableOriginKind::Native,
            true,
            &store,
            PortableAssetKind::Skill,
        );
        assert!(!caps.can_enable, "{target:?}");
        assert!(!caps.can_disable, "{target:?}");
        assert!(!caps.can_uninstall, "{target:?}");
        assert!(!caps.can_migrate_to_store, "{target:?}");
        assert!(!caps.can_attach, "{target:?}");
        assert!(
            caps.can_detach,
            "attached native store Skill on {target:?} must expose 从此 Agent 卸下"
        );
        assert!(caps.can_destroy_store, "{target:?}");
    }
}

#[test]
fn borrowed_mcp_exposes_no_owner_toggles() {
    let caps = item_capabilities(
        AgentTarget::Claude,
        AgentTarget::Claude,
        PortableAssetKind::Mcp,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        true,
        PortableOriginKind::Compatibility,
        false,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Mcp,
    );
    assert!(!caps.can_enable);
    assert!(!caps.can_disable);
    assert!(!caps.can_uninstall);
    assert_eq!(caps.reason_code.as_deref(), Some("borrowed_runtime_origin"));
}

#[test]
fn borrowed_store_skill_via_other_path_cannot_detach() {
    let store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    let caps = item_capabilities(
        AgentTarget::Grok,
        AgentTarget::Grok,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        true,
        PortableOriginKind::Compatibility,
        false,
        &store,
        PortableAssetKind::Skill,
    );
    assert!(
        !caps.can_detach,
        "借用经其他 Agent 软链加载的 Skill 不得拆源链"
    );
    assert!(!caps.can_attach);
}

#[test]
fn grok_borrowed_store_skill_cannot_detach_source_or_attach_or_migrate() {
    let store = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    let caps = item_capabilities(
        AgentTarget::Grok,
        AgentTarget::Grok,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        Some("cli_version_unknown".into()),
        true,
        PortableOriginKind::Compatibility,
        false,
        &store,
        PortableAssetKind::Skill,
    );
    assert!(!caps.can_migrate_to_store);
    assert!(
        !caps.can_attach,
        "borrowed runtime view must not attach a second native symlink"
    );
    assert!(
        !caps.can_detach,
        "借用经其他 Agent 软链加载的 Skill 不得拆源链"
    );
    assert!(
        !caps.can_destroy_store,
        "borrowed runtime view must not delete the shared store tree"
    );
    assert_eq!(caps.reason_code.as_deref(), Some("borrowed_runtime_origin"));
}

#[test]
fn unattached_store_catalog_is_not_enabled() {
    let attached = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: true,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    let catalog = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: false,
        loaded_via_other_path: false,
        loaded_via_target: None,
    };
    let via_other = PortableStoreFactDto {
        store_id: Some("skill:media-use".into()),
        store_attached: false,
        loaded_via_other_path: true,
        loaded_via_target: Some(AgentTarget::Claude),
    };
    assert_eq!(store_catalog_enabled(&attached, Some(true)), Some(true));
    assert_eq!(store_catalog_enabled(&catalog, Some(true)), Some(false));
    assert_eq!(store_catalog_enabled(&via_other, Some(true)), Some(true));
    assert_eq!(
        store_catalog_enabled(&PortableStoreFactDto::default(), Some(true)),
        Some(true)
    );
}

#[test]
fn legacy_and_shared_skills_migrate_instead_of_toggle() {
    let agents = item_capabilities(
        AgentTarget::Codex,
        AgentTarget::Codex,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        false,
        PortableOriginKind::LegacyStandalone,
        false,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(agents.can_migrate_to_store);
    assert!(!agents.can_enable);
    assert!(!agents.can_disable);
    assert!(!agents.can_uninstall);
    assert!(!agents.can_detach);

    let plugin_component = item_capabilities(
        AgentTarget::Claude,
        AgentTarget::Claude,
        PortableAssetKind::Skill,
        Some(true),
        true,
        true,
        true,
        true,
        None,
        false,
        PortableOriginKind::Plugin,
        true,
        &PortableStoreFactDto::default(),
        PortableAssetKind::Skill,
    );
    assert!(!plugin_component.can_migrate_to_store);
    assert!(!plugin_component.can_enable);
    assert!(!plugin_component.can_disable);
}

#[test]
fn recursive_plugin_tree_hash_detects_nested_content_and_empty_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("plugin");
    fs::create_dir_all(root.join("nested/empty")).unwrap();
    fs::write(root.join("plugin.json"), "{\"name\":\"demo\"}").unwrap();
    fs::write(root.join("nested/body.txt"), "v1").unwrap();
    let first = hash_plugin_root(&root).unwrap().1;

    fs::write(root.join("nested/body.txt"), "v2").unwrap();
    let changed = hash_plugin_root(&root).unwrap().1;
    assert_ne!(first, changed, "nested file content must be tree-bound");

    fs::remove_dir(root.join("nested/empty")).unwrap();
    let without_empty_dir = hash_plugin_root(&root).unwrap().1;
    assert_ne!(
        changed, without_empty_dir,
        "empty directory type/path is tree-bound"
    );
}

#[test]
fn plugin_root_discovery_uses_installs_and_rejects_infrastructure_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let claude_root = dir.path().join(".claude");
    let installed = claude_root.join("plugins/cache/market/demo/1.0.0");
    fs::create_dir_all(&installed).unwrap();
    fs::create_dir_all(claude_root.join("plugins/cache/not-a-plugin/nested")).unwrap();
    fs::create_dir_all(claude_root.join("plugins/marketplaces/huge-tree")).unwrap();
    fs::create_dir_all(claude_root.join("plugins/data/session-state")).unwrap();
    write(
        &claude_root.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "demo@market": [{
                    "scope": "user",
                    "installPath": installed.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );

    let roots = claude_user_plugin_roots(&claude_root);
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert_eq!(roots[0].path, installed);
    assert_eq!(roots[0].registry_plugin_id.as_deref(), Some("demo"));
    assert!(!roots.iter().any(|c| {
        ["cache", "data", "marketplaces"]
            .iter()
            .any(|name| c.path.ends_with(name))
    }));

    let codex_root = dir.path().join(".codex");
    let codex_plugin = codex_root.join("plugins/cache/market/demo/2.0.0");
    write(
        &codex_plugin.join(".codex-plugin/plugin.json"),
        r#"{"name":"demo"}"#,
    );
    fs::create_dir_all(codex_root.join("plugins/.plugin-appserver")).unwrap();
    fs::create_dir_all(codex_root.join("plugins/data")).unwrap();

    let roots = codex_user_plugin_roots(&codex_root);
    assert_eq!(roots.len(), 1, "{roots:?}");
    assert_eq!(roots[0].path, codex_plugin);
}

/// Grok user 根必须同时包含 native installed-plugins 与 Claude registry/marketplace。
#[test]
fn grok_user_plugin_roots_include_installed_plugins_and_claude_registry() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let grok = home.join(".grok");
    let claude = home.join(".claude");
    let installed = claude.join("plugins/cache/market/compat-plugin/1.0.0");
    write(
        &installed.join(".claude-plugin/plugin.json"),
        r#"{"name":"compat-plugin"}"#,
    );
    write(
        &claude.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "compat-plugin@market": [{
                    "scope": "user",
                    "installPath": installed.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );
    let native_plugin = grok.join("installed-plugins/native-plugin");
    write(
        &native_plugin.join("plugin.json"),
        r#"{"name":"native-plugin"}"#,
    );
    let market_root = home.join("cache/marketplaces/demo-market");
    let market_plugin = market_root.join("listed-plugin");
    write(
        &market_plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"listed-plugin"}"#,
    );
    write(
        &claude.join("plugins/known_marketplaces.json"),
        &serde_json::json!({
            "marketplaces": {
                "demo-market": {
                    "installLocation": market_root.to_string_lossy()
                }
            }
        })
        .to_string(),
    );

    let mut vars = Map::new();
    vars.insert("GROK_HOME".into(), grok.to_string_lossy().into_owned());
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        claude.to_string_lossy().into_owned(),
    );
    let env = TargetEnvironment {
        home: home.to_path_buf(),
        vars,
        path_entries: vec![],
    };
    let scope = PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.to_path_buf(),
    };
    let homes = TargetPathResolver::resolve_all(&env);
    let roots = plugin_roots_for(AgentTarget::Grok, &scope, &env, &homes);

    let native = roots
        .iter()
        .find(|c| c.path == native_plugin)
        .expect("native installed-plugins");
    assert_eq!(native.origin_kind, PortableOriginKind::Native);
    assert_eq!(native.owned_by, PortableAssetOwner::Grok);

    let borrowed = roots
        .iter()
        .find(|c| c.path == installed)
        .expect("Claude registry plugin");
    assert_eq!(borrowed.origin_kind, PortableOriginKind::Compatibility);
    assert_eq!(borrowed.owned_by, PortableAssetOwner::Claude);
    assert_eq!(
        borrowed.registry_plugin_id.as_deref(),
        Some("compat-plugin")
    );

    let market = roots
        .iter()
        .find(|c| c.path == market_plugin)
        .expect("Claude marketplace plugin");
    assert_eq!(market.origin_kind, PortableOriginKind::Compatibility);
    assert_eq!(market.owned_by, PortableAssetOwner::Claude);
}

/// Codex config.toml `[plugins."id@market"] enabled` 是启用权威。
#[test]
fn parse_codex_plugin_enablement_reads_enabled_flags() {
    let text = r#"
[plugins."browser@openai-bundled"]
enabled = true

[plugins."legacy@openai-bundled"]
enabled = false

[plugins."no-flag@openai-curated"]
"#;
    let map = parse_codex_plugin_enablement_from_toml(text);
    assert_eq!(map.get("browser@openai-bundled"), Some(&true));
    assert_eq!(map.get("legacy@openai-bundled"), Some(&false));
    // 缺 enabled 字段时默认 true（与 Codex 表存在即安装一致）
    assert_eq!(map.get("no-flag@openai-curated"), Some(&true));
    assert!(!map.contains_key("missing@x"));
}

#[test]
fn codex_plugin_package_actual_enabled_follows_config_not_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let codex = home.join(".codex");
    let browser = codex.join("plugins/cache/openai-bundled/browser/26.803.61601");
    write(
        &browser.join(".codex-plugin/plugin.json"),
        r#"{"name":"browser","version":"1"}"#,
    );
    let latex = codex.join("plugins/cache/openai-bundled/latex/0.2.2");
    write(
        &latex.join(".codex-plugin/plugin.json"),
        r#"{"name":"latex","version":"1"}"#,
    );
    write(
        &codex.join("config.toml"),
        r#"
[plugins."browser@openai-bundled"]
enabled = true

[plugins."computer-use@openai-bundled"]
enabled = false
"#,
    );
    let mut vars = BTreeMap::new();
    vars.insert("CODEX_HOME".into(), codex.to_string_lossy().into_owned());
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    let scopes = [PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Codex),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let browser_item = items
        .iter()
        .find(|i| i.native_id == "browser" || i.native_id.starts_with("browser@"))
        .expect("browser package");
    assert_eq!(
        browser_item.actual_enabled,
        Some(true),
        "config enabled=true"
    );
    let latex_item = items
        .iter()
        .find(|i| i.native_id == "latex" || i.native_id.starts_with("latex@"))
        .expect("latex residual package");
    assert_eq!(
        latex_item.actual_enabled,
        Some(false),
        "cache-only package not listed enabled in config must not report always-true"
    );
    assert!(
        latex_item
            .warnings
            .iter()
            .any(|w| w.contains("codex_plugin_not_in_config")),
        "residual should warn: {:?}",
        latex_item.warnings
    );
}

#[test]
fn parse_claude_plugin_enablement_reads_enabled_plugins() {
    let text = r#"{
  "enabledPlugins": {
    "superpowers@claude-plugins-official": false,
    "pyright-lsp@claude-plugins-official": true
  }
}"#;
    let map = parse_claude_plugin_enablement_from_settings(text);
    assert_eq!(map.get("superpowers@claude-plugins-official"), Some(&false));
    assert_eq!(map.get("pyright-lsp@claude-plugins-official"), Some(&true));
    assert!(!map.contains_key("missing@x"));
    assert!(parse_claude_plugin_enablement_from_settings("{}").is_empty());
}

#[test]
fn claude_plugin_package_actual_enabled_follows_settings_not_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let claude = home.join(".claude");
    let official = claude.join("plugins/cache/claude-plugins-official/superpowers/6.3.0");
    write(
        &official.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers","version":"6.3.0"}"#,
    );
    let pyright = claude.join("plugins/cache/claude-plugins-official/pyright-lsp/1.0.0");
    write(
        &pyright.join(".claude-plugin/plugin.json"),
        r#"{"name":"pyright-lsp","version":"1.0.0"}"#,
    );
    write(
        &claude.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "superpowers@claude-plugins-official": [{
                    "scope": "user",
                    "installPath": official.to_string_lossy()
                }],
                "pyright-lsp@claude-plugins-official": [{
                    "scope": "user",
                    "installPath": pyright.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );
    write(
        &claude.join("settings.json"),
        r#"{
  "enabledPlugins": {
    "superpowers@claude-plugins-official": false,
    "pyright-lsp@claude-plugins-official": true
  }
}"#,
    );
    let mut vars = BTreeMap::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        claude.to_string_lossy().into_owned(),
    );
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    let scopes = [PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let superpowers = items
        .iter()
        .find(|i| {
            i.native_id == "superpowers"
                || i.native_id == "superpowers@claude-plugins-official"
                || i.source_path.as_deref() == official.to_str()
        })
        .expect("superpowers package");
    assert_eq!(
        superpowers.native_id, "superpowers@claude-plugins-official",
        "cache installs must keep marketplace-qualified native id"
    );
    assert_eq!(
        superpowers.actual_enabled,
        Some(false),
        "enabledPlugins false must not report directory-exists as enabled"
    );
    let pyright_item = items
        .iter()
        .find(|i| {
            i.native_id == "pyright-lsp" || i.native_id == "pyright-lsp@claude-plugins-official"
        })
        .expect("pyright package");
    assert_eq!(pyright_item.actual_enabled, Some(true));
}

#[test]
fn claude_same_plugin_from_two_marketplaces_stays_two_inventory_rows() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let claude = home.join(".claude");
    let official = claude.join("plugins/cache/claude-plugins-official/superpowers/6.3.0");
    write(
        &official.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers","version":"6.3.0"}"#,
    );
    let marketplace = claude.join("plugins/cache/superpowers-marketplace/superpowers/6.1.1");
    write(
        &marketplace.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers","version":"6.1.1"}"#,
    );
    write(
        &claude.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "superpowers@claude-plugins-official": [{
                    "scope": "user",
                    "installPath": official.to_string_lossy()
                }],
                "superpowers@superpowers-marketplace": [{
                    "scope": "user",
                    "installPath": marketplace.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );
    write(
        &claude.join("settings.json"),
        r#"{
  "enabledPlugins": {
    "superpowers@claude-plugins-official": false,
    "superpowers@superpowers-marketplace": true
  }
}"#,
    );
    let mut vars = BTreeMap::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        claude.to_string_lossy().into_owned(),
    );
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    let scopes = [PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let official_item = items
        .iter()
        .find(|i| i.native_id == "superpowers@claude-plugins-official")
        .expect("official superpowers row");
    let market_item = items
        .iter()
        .find(|i| i.native_id == "superpowers@superpowers-marketplace")
        .expect("marketplace superpowers row");
    assert_ne!(
        official_item.inventory_item_id, market_item.inventory_item_id,
        "marketplace copies must not collapse to one inventory id"
    );
    assert_eq!(official_item.actual_enabled, Some(false));
    assert_eq!(market_item.actual_enabled, Some(true));
    assert_eq!(official_item.display_name, "superpowers");
    assert_eq!(market_item.display_name, "superpowers");
}

#[test]
fn grok_plugin_package_actual_enabled_ignores_claude_settings() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let claude = home.join(".claude");
    let grok = home.join(".grok");
    let official = claude.join("plugins/cache/claude-plugins-official/superpowers/6.3.0");
    write(
        &official.join(".claude-plugin/plugin.json"),
        r#"{"name":"superpowers","version":"6.3.0"}"#,
    );
    write(
        &claude.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "superpowers@claude-plugins-official": [{
                    "scope": "user",
                    "installPath": official.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );
    write(
        &claude.join("settings.json"),
        r#"{
  "enabledPlugins": {
    "superpowers@claude-plugins-official": false
  }
}"#,
    );
    write(
        &grok.join("config.toml"),
        r#"
[plugins]
enabled = ["native-only"]
"#,
    );
    write(
        &grok.join("installed-plugins/native-only/plugin.json"),
        r#"{"name":"native-only","version":"0.1.0"}"#,
    );
    let mut vars = BTreeMap::new();
    vars.insert("GROK_HOME".into(), grok.to_string_lossy().into_owned());
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        claude.to_string_lossy().into_owned(),
    );
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    let scopes = [PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Grok),
        kind: Some(PortableAssetKind::Plugin),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let superpowers = items
        .iter()
        .find(|i| {
            i.native_id == "superpowers"
                || i.native_id == "superpowers@claude-plugins-official"
                || i.source_path.as_deref() == official.to_str()
        })
        .expect("borrowed superpowers on Grok");
    assert_eq!(superpowers.target, AgentTarget::Grok);
    assert_eq!(superpowers.owned_by, PortableAssetOwner::Claude);
    assert_eq!(
        superpowers.actual_enabled,
        Some(true),
        "Claude enabledPlugins=false must not mark Grok inventory disabled"
    );
    assert!(
        superpowers.capabilities.can_disable,
        "Grok plugin disable is a file-only viewing toggle, not a Claude CLI remap"
    );
    let native = items
        .iter()
        .find(|i| i.native_id == "native-only")
        .expect("native grok plugin");
    assert_eq!(native.actual_enabled, Some(true));
    assert_eq!(native.owned_by, PortableAssetOwner::Grok);
}

#[test]
fn agents_without_plugin_flags_ignore_claude_enabled_plugins() {
    struct Case {
        target: AgentTarget,
        env_key: Option<&'static str>,
        config_rel: &'static str,
        manifest: &'static str,
        manifest_body: &'static str,
    }
    let cases = [
        Case {
            target: AgentTarget::OpenCode,
            env_key: Some("OPENCODE_CONFIG_DIR"),
            config_rel: ".opencode",
            manifest: "package.json",
            manifest_body: r#"{"name":"demo"}"#,
        },
        Case {
            target: AgentTarget::Gemini,
            env_key: Some("GEMINI_HOME"),
            config_rel: ".gemini",
            manifest: "plugin.json",
            manifest_body: r#"{"name":"demo"}"#,
        },
        Case {
            target: AgentTarget::Cursor,
            env_key: Some("CURSOR_HOME"),
            config_rel: ".cursor",
            manifest: "plugin.json",
            manifest_body: r#"{"name":"demo"}"#,
        },
        Case {
            target: AgentTarget::Pi,
            env_key: None,
            config_rel: ".pi/agent",
            manifest: "plugin.json",
            manifest_body: r#"{"name":"demo"}"#,
        },
    ];
    for case in cases {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let config = home.join(case.config_rel);
        write(
            &config.join("plugins/demo").join(case.manifest),
            case.manifest_body,
        );
        write(
            &home.join(".claude/settings.json"),
            r#"{
  "enabledPlugins": {
    "demo": false,
    "demo@claude-plugins-official": false
  }
}"#,
        );
        let mut vars = BTreeMap::new();
        vars.insert(
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into_owned(),
        );
        if let Some(key) = case.env_key {
            vars.insert(key.into(), config.to_string_lossy().into_owned());
        }
        let env = TargetEnvironment {
            home: home.clone(),
            vars,
            path_entries: vec![],
        };
        let scopes = [PortableScanScope {
            scope_id: "user".into(),
            scope_kind: ScopeKind::User,
            project_id: None,
            project_opted_in: true,
            absolute_path: home.clone(),
        }];
        let query = PortableInventoryQuery {
            target: Some(case.target),
            kind: Some(PortableAssetKind::Plugin),
            scope_kind: Some(ScopeKind::User),
            local_project_id: None,
        };
        let (_targets, items) =
            scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
        let demo = items
            .iter()
            .find(|i| i.native_id == "demo")
            .unwrap_or_else(|| panic!("{:?} missing demo plugin: {items:?}", case.target));
        assert_eq!(
            demo.actual_enabled,
            Some(true),
            "{:?} must not inherit Claude enabledPlugins=false",
            case.target
        );
    }
}

#[test]
fn claude_registry_identity_used_when_install_lacks_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let claude_root = dir.path().join(".claude");
    let installed = claude_root.join("plugins/cache/claude-plugins-official/pyright-lsp/1.0.0");
    fs::create_dir_all(&installed).unwrap();
    // no .claude-plugin/plugin.json — only LICENSE-like content
    write(&installed.join("README.md"), "x");
    write(
        &claude_root.join("plugins/installed_plugins.json"),
        &serde_json::json!({
            "version": 2,
            "plugins": {
                "pyright-lsp@claude-plugins-official": [{
                    "scope": "user",
                    "installPath": installed.to_string_lossy()
                }]
            }
        })
        .to_string(),
    );
    let roots = claude_user_plugin_roots(&claude_root);
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].registry_plugin_id.as_deref(), Some("pyright-lsp"));
    assert_eq!(
        roots[0].registry_key.as_deref(),
        Some("pyright-lsp@claude-plugins-official")
    );
}

#[test]
fn scan_is_read_only_no_file_mutations() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let before = walk_snapshot(&env.home);
    let _ = scan_portable_inventory_facts(&env, &scopes).unwrap();
    // also exercise adapters directly
    let scope = LocalScopeMapping {
        scope_kind: ScopeKind::User,
        absolute_path: env.home.clone(),
        project_root: None,
        relative_root: None,
        codex_fallback_filenames: vec![],
    };
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
    assert_eq!(before, after, "scan must not write target files");
}

#[test]
fn reconcile_snapshot_from_scan_keeps_standalone_and_component_separate() {
    let (_tmp, env) = seed_all_targets_fixture();
    let scopes = user_and_projects(&env.home);
    let (targets, items) = scan_portable_inventory_facts(&env, &scopes).unwrap();
    let snap = reconcile_portable_inventory_with_facts(targets, items, &[]).unwrap();
    let shared: Vec<_> = snap
        .items
        .iter()
        .filter(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Skill
                && i.native_id == "shared-name"
        })
        .collect();
    assert_eq!(shared.len(), 2);
    assert!(shared.iter().any(|i| {
        i.source_origin == PortableInventorySourceOrigin::Standalone
            && i.parent_plugin_inventory_item_id.is_none()
    }));
    assert!(shared.iter().any(|i| {
        i.source_origin == PortableInventorySourceOrigin::PluginComponent
            && i.parent_plugin_inventory_item_id.is_some()
    }));
    assert!(!snap.inventory_snapshot_hash.is_empty());
    assert!(!snap.refreshed_at.is_empty());
}

/// Business Logic: inventory_item_id 路径无关后，同一逻辑资产在 active 与 disabled 路径下
/// 产出相同 id；claude.rs adapter 先扫 active 后扫 disabled，"先到先得"会让 disabled 版本
/// 被丢弃、UI 永远显示 enabled。scanner 必须用"disabled 赢"合并策略：active+disabled 共存时
/// （这是异常态，正常 disable 流程会清空 active），保留 disabled 反映用户最近的禁用意图。
/// Code Logic: 构造同名 skill 同时存在于 active 路径（.claude/skills/dup-name）与 disabled
/// 路径（.claude/disabled/skills/dup-name），跑 scan，断言只剩一条记录且 actual_enabled==Some(false)。
#[test]
fn scan_merges_active_and_disabled_with_same_logical_identity_keeps_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    // 同名 skill 同时在 active 与 disabled 目录
    write(
        &home.join(".claude/skills/dup-name/SKILL.md"),
        "---\nname: dup-name\ndescription: Active copy\n---\n# Active\n",
    );
    write(
        &home.join(".claude/disabled/skills/dup-name/SKILL.md"),
        "---\nname: dup-name\ndescription: Disabled copy\n---\n# Disabled\n",
    );
    let mut vars = Map::new();
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        home.join(".claude").to_string_lossy().into_owned(),
    );
    let env = TargetEnvironment {
        home: home.clone(),
        vars,
        path_entries: vec![],
    };
    let scopes = vec![PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let (_targets, items) = scan_portable_inventory_facts(&env, &scopes).expect("scan");

    let dup: Vec<_> = items
        .iter()
        .filter(|i| {
            i.target == AgentTarget::Claude
                && i.kind == PortableAssetKind::Skill
                && i.native_id == "dup-name"
                && i.source_origin == PortableInventorySourceOrigin::Standalone
        })
        .collect();
    // 必须合并成一条（同逻辑身份），不是两条
    assert_eq!(
        dup.len(),
        1,
        "active+disabled same logical identity must merge to one item, got: {dup:?}"
    );
    // disabled 赢：actual_enabled == Some(false)
    assert_eq!(
        dup[0].actual_enabled,
        Some(false),
        "merged item must reflect disabled (disabled wins)"
    );
    // source_path 应指向 disabled 路径（替换生效）
    assert!(
        dup[0]
            .source_path
            .as_deref()
            .unwrap_or_default()
            .contains("disabled/skills/dup-name"),
        "merged item source_path must point to disabled copy: {:?}",
        dup[0].source_path
    );
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

fn store_item(
    target: AgentTarget,
    origin: PortableOriginKind,
    attached: bool,
    via_other: bool,
) -> PortableInventoryItemDto {
    PortableInventoryItemDto {
        inventory_item_id: format!("{}-skill-foo", target.as_str()),
        target,
        loaded_by: target,
        owned_by: PortableAssetOwner::PortableStore,
        origin_kind: origin,
        native_output_candidate: origin == PortableOriginKind::Native && attached,
        kind: PortableAssetKind::Skill,
        native_id: "foo".into(),
        display_name: "foo".into(),
        description: None,
        version: None,
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        source_path: Some(format!("/{}/skills/foo", target.as_str())),
        source_origin: PortableInventorySourceOrigin::Standalone,
        parent_plugin_inventory_item_id: None,
        actual_enabled: Some(attached),
        content_hash: Some("hash-foo".into()),
        tree_hash: None,
        canonical_asset_id: None,
        canonical_revision_id: None,
        management_state: PortableInventoryManagementState::HubManaged,
        desired_presence: None,
        desired_enabled: None,
        materialization_status: None,
        capabilities: PortableInventoryItemCapabilitiesDto {
            can_enable: false,
            can_disable: false,
            can_uninstall: false,
            can_adopt: false,
            can_install_to_source_target: false,
            can_migrate_to_store: false,
            can_attach: !attached,
            can_detach: attached,
            can_destroy_store: true,
            can_confirm_current_version: false,
            can_materialize_escape_link: false,
            reason_code: None,
            evidence_ids: vec![],
        },
        warnings: vec![],
        mcp_credential: None,
        store: PortableStoreFactDto {
            store_id: Some("skill:foo".into()),
            store_attached: attached,
            loaded_via_other_path: via_other,
            loaded_via_target: None,
        },
    }
}

#[test]
fn grok_unattached_store_does_not_default_loaded_via_to_claude() {
    let claude = store_item(AgentTarget::Claude, PortableOriginKind::Native, true, false);
    let mut grok = store_item(
        AgentTarget::Grok,
        PortableOriginKind::Compatibility,
        false,
        false,
    );
    let mut items = vec![claude, grok];
    annotate_store_loaded_via_other_path(&mut items);
    assert!(items[0].store.store_attached);
    assert!(!items[0].store.loaded_via_other_path);
    grok = items.remove(1);
    assert!(!grok.store.store_attached);
    assert!(grok.store.loaded_via_other_path);
    assert_eq!(grok.store.loaded_via_target, None);
    assert!(grok
        .warnings
        .iter()
        .any(|w| w == "store_loaded_via_other_path"));
}

#[test]
fn grok_claude_compat_path_still_hints_claude() {
    let mut grok = store_item(
        AgentTarget::Grok,
        PortableOriginKind::Compatibility,
        false,
        false,
    );
    grok.source_path = Some("/home/.claude/skills/foo".into());
    let mut items = vec![grok];
    annotate_store_loaded_via_other_path(&mut items);
    assert_eq!(items[0].store.loaded_via_target, Some(AgentTarget::Claude));
}

#[test]
fn grok_agents_path_is_shared_not_claude() {
    let mut grok = store_item(
        AgentTarget::Grok,
        PortableOriginKind::Compatibility,
        false,
        false,
    );
    grok.source_path = Some("/home/.agents/skills/superpowers/using-superpowers".into());
    grok.store.loaded_via_target = Some(AgentTarget::Claude);
    let mut items = vec![grok];
    annotate_store_loaded_via_other_path(&mut items);
    assert_eq!(
        items[0].store.loaded_via_target, None,
        "~/.agents is shared; Claude does not load it"
    );
    assert!(items[0].store.loaded_via_other_path);
}

#[test]
fn unattached_store_catalog_does_not_replace_borrowed_compat() {
    let existing = store_item(
        AgentTarget::Grok,
        PortableOriginKind::Compatibility,
        false,
        true,
    );
    let mut catalog = store_item(AgentTarget::Grok, PortableOriginKind::Native, false, false);
    catalog.actual_enabled = Some(false);
    assert!(
        !should_replace_with(&catalog, &existing),
        "injecting the store tree must not hide Grok/Pi runtime-loaded skills"
    );
    let attached = store_item(AgentTarget::Grok, PortableOriginKind::Native, true, false);
    assert!(should_replace_with(&attached, &existing));
}

/// Business Logic: Codex Skill 列表 preview/apply 绑定整页 inventory hash；
///     未附加仓库树或 leftover 附加文件变化不得误报 HASH_MISMATCH。
/// Code Logic: kind=skill 延迟 tree hash；两次扫描中间改 sibling/leftover 日志后 hash 不变。
#[test]
fn skill_query_hash_ignores_unattached_store_tree_churn() {
    use crate::agent_hub::{
        portable_store::{create_store_link, ensure_portable_store_layout, store_skill_dir},
        targets::portable::DATA_DIR_ENV_LOCK,
    };

    let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);

    let store = ensure_portable_store_layout(&data).expect("layout");
    let cli = store_skill_dir(&store, "hyperframes-cli");
    write(
        &cli.join("SKILL.md"),
        "---\nname: hyperframes-cli\n---\n# CLI\n",
    );
    write(&cli.join("extra.bin"), "stable");
    create_store_link(&cli, &home.join(".codex/skills/hyperframes-cli")).expect("attach");
    write(
        &home.join(".agents/skills/hyperframes-cli/SKILL.md"),
        "---\nname: hyperframes-cli\n---\n# CLI\n",
    );
    write(
        &home.join(".agents/skills/hyperframes-cli/leftover.log"),
        "v1",
    );

    let sibling = store_skill_dir(&store, "hyperframes-core");
    write(
        &sibling.join("SKILL.md"),
        "---\nname: hyperframes-core\n---\n# Core\n",
    );
    write(&sibling.join("volatile.log"), "v1");

    let env = TargetEnvironment {
        home: home.clone(),
        vars: {
            let mut vars = Map::new();
            vars.insert(
                "CODEX_HOME".into(),
                home.join(".codex").to_string_lossy().into(),
            );
            vars
        },
        path_entries: vec![],
    };
    let scopes = vec![PortableScanScope {
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        project_id: None,
        project_opted_in: true,
        absolute_path: home.clone(),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Codex),
        kind: Some(PortableAssetKind::Skill),
        scope_kind: Some(ScopeKind::User),
        local_project_id: None,
    };

    let (targets1, items1) =
        scan_portable_inventory_facts_query(&env, &scopes, query.clone()).expect("scan1");
    let attached = items1
        .iter()
        .find(|item| item.native_id == "hyperframes-cli" && item.target == AgentTarget::Codex)
        .expect("attached cli");
    assert!(
        attached.store.store_attached,
        "Codex native store link must count as attached"
    );
    assert!(
        attached.capabilities.can_detach,
        "attached store skill must offer detach"
    );
    assert!(
        attached.tree_hash.is_none(),
        "skill list must defer tree hash"
    );
    let sibling_item = items1
        .iter()
        .find(|item| item.native_id == "hyperframes-core" && item.target == AgentTarget::Codex)
        .expect("unattached sibling");
    assert!(!sibling_item.store.store_attached);
    assert!(sibling_item.tree_hash.is_none());
    let hash1 = inventory_snapshot_hash(&targets1, &items1).expect("hash1");

    write(&sibling.join("volatile.log"), "v2-changed");
    write(
        &home.join(".agents/skills/hyperframes-cli/leftover.log"),
        "v2-changed",
    );

    let (targets2, items2) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan2");
    let hash2 = inventory_snapshot_hash(&targets2, &items2).expect("hash2");
    assert_eq!(
        hash1, hash2,
        "unattached store / leftover extra files must not change skill-page CAS hash"
    );

    std::env::remove_var("CC_PARTNER_DATA_DIR");
}

#[test]
fn project_scope_store_catalog_does_not_inject_user_store() {
    use crate::agent_hub::{
        portable_store::{
            ensure_store_layout, portable_project_store_root, portable_store_root, store_skill_dir,
        },
        targets::portable::DATA_DIR_ENV_LOCK,
    };

    let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);

    let user_store = ensure_store_layout(&portable_store_root(&data)).expect("user layout");
    write(
        &store_skill_dir(&user_store, "user-only").join("SKILL.md"),
        "---\nname: user-only\n---\n# User\n",
    );
    let project_store =
        ensure_store_layout(&portable_project_store_root(&data, "hub-proj-1")).expect("proj");
    write(
        &store_skill_dir(&project_store, "proj-only").join("SKILL.md"),
        "---\nname: proj-only\n---\n# Project\n",
    );

    let env = TargetEnvironment {
        home: home.clone(),
        vars: Default::default(),
        path_entries: vec![],
    };
    let scopes = vec![PortableScanScope {
        scope_id: "project:hub-proj-1".into(),
        scope_kind: ScopeKind::Project,
        project_id: Some("hub-proj-1".into()),
        project_opted_in: true,
        absolute_path: home.join("proj"),
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Skill),
        scope_kind: Some(ScopeKind::Project),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let native_ids: Vec<_> = items.iter().map(|item| item.native_id.as_str()).collect();
    assert!(
        native_ids.contains(&"proj-only"),
        "project store catalog missing: {native_ids:?}"
    );
    assert!(
        !native_ids.contains(&"user-only"),
        "user store leaked into project catalog: {native_ids:?}"
    );
    assert!(items
        .iter()
        .all(|item| item.scope_kind == ScopeKind::Project));

    std::env::remove_var("CC_PARTNER_DATA_DIR");
}

#[test]
fn project_scope_leftover_does_not_claim_user_store() {
    use crate::agent_hub::{
        portable_store::{ensure_store_layout, portable_store_root, store_skill_dir},
        targets::portable::DATA_DIR_ENV_LOCK,
    };

    let _guard = DATA_DIR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&data).unwrap();
    std::env::set_var("CC_PARTNER_DATA_DIR", &data);

    let body = "---\nname: twin\n---\n# Twin\n";
    let user_store = ensure_store_layout(&portable_store_root(&data)).expect("user layout");
    write(&store_skill_dir(&user_store, "twin").join("SKILL.md"), body);
    let proj = home.join("proj");
    write(&proj.join(".claude/skills/twin/SKILL.md"), body);

    let env = TargetEnvironment {
        home: home.clone(),
        vars: Default::default(),
        path_entries: vec![],
    };
    let scopes = vec![PortableScanScope {
        scope_id: "project:hub-proj-1".into(),
        scope_kind: ScopeKind::Project,
        project_id: Some("hub-proj-1".into()),
        project_opted_in: true,
        absolute_path: proj,
    }];
    let query = PortableInventoryQuery {
        target: Some(AgentTarget::Claude),
        kind: Some(PortableAssetKind::Skill),
        scope_kind: Some(ScopeKind::Project),
        local_project_id: None,
    };
    let (_targets, items) =
        scan_portable_inventory_facts_query(&env, &scopes, query).expect("scan");
    let twin = items.iter().find(|item| item.native_id == "twin");
    assert!(
        twin.is_some(),
        "project skill missing: {:?}",
        items
            .iter()
            .map(|item| item.native_id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        twin.unwrap().store.store_id.is_none(),
        "user store leftover leaked: {:?}",
        twin.unwrap().store
    );

    std::env::remove_var("CC_PARTNER_DATA_DIR");
}
