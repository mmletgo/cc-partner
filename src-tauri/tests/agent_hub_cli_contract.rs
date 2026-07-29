//! agent_hub_cli_contract — 真实 CLI 合同 harness（Gate B Task 4）
//!
//! Evidence: L3-AGENT-HUB-B-CLI-001
//!
//! Business Logic（为什么需要这个测试文件）:
//!     support manifest 的写能力只能绑定真实 Claude/Codex/OpenCode CLI 的 exact 版本
//!     与发现/激活合同；单元测试不能替代 L3。本 harness 在隔离 HOME 下探测可执行
//!     realpath/`--version`，并与 manifest `currentTestedVersion` 比对。
//!
//! Code Logic（这个文件做什么）:
//!     - 非 ignored 编译烟测：内置 manifest 可解析、三 target 存在
//!     - ignored L3：`CC_PARTNER_L3_TARGET=claude|codex|opencode` 选择目标；
//!       隔离 HOME/CODEX_HOME/OPENCODE_*；打印仅 version/fingerprint，永不打印资产正文
//!
//! 运行（需本机安装对应 CLI）:
//! ```bash
//! cd src-tauri
//! CC_PARTNER_L3_TARGET=claude cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
//! ```
//!
//! NOT VERIFIED（本文件默认不宣称；evidence `L3-AGENT-HUB-B-CLI-001` 保持 NOT VERIFIED
//!     直至本机 exact version 的 ignored L3 实际跑通并更新 support manifest）:
//!     - CI 未安装 CLI 时的写能力认证
//!     - 真实 marketplace 激活副作用
//!     - 未 pin 的 Claude/Codex/OpenCode 版本写路径

use app_lib::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, format_probe_identity,
    parse_semver_core, CapabilitySupport, EvaluatedSupportMode, RuntimeProbeSnapshot,
    TargetCapability, SUPPORT_MANIFEST_JSON,
};
use app_lib::agent_hub::targets::{
    compute_probe_fingerprint, probe_cli_version, resolve_executable, ClaudeInstructionAdapter,
    CodexInstructionAdapter, OpenCodeInstructionAdapter, TargetEnvironment, TargetPathResolver,
};
use app_lib::agent_hub::AssetAdapter;
use app_lib::AgentTarget;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 编译期烟测：manifest 可解析且含三 target。
///
/// Business Logic: 非 ignored 路径保证 harness 与二进制一并编译。
/// Code Logic: builtin_support_manifest + target 集合。
#[test]
fn support_manifest_compiles_and_lists_three_targets() {
    assert!(
        SUPPORT_MANIFEST_JSON.contains("schemaVersion"),
        "embedded manifest missing schemaVersion"
    );
    let manifest = builtin_support_manifest().expect("parse builtin manifest");
    assert_eq!(manifest.schema_version, 1);
    let mut names: Vec<&str> = manifest.targets.iter().map(|t| t.target.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["claude", "codex", "opencode"]);
}

/// 编译期烟测：基线写能力必须 blocked（未跑 L3 前）。
///
/// Business Logic: Gate B 完成前不得把本地未认证版本写成 supported write。
/// Code Logic: 遍历 capabilities，写侧均为 Blocked。
#[test]
fn baseline_write_capabilities_are_blocked() {
    let manifest = builtin_support_manifest().unwrap();
    for record in &manifest.targets {
        for (cap, support) in &record.capabilities {
            if cap.is_write_side() {
                assert_eq!(
                    *support,
                    CapabilitySupport::Blocked,
                    "{} {:?} should be blocked in baseline",
                    record.target.as_str(),
                    cap
                );
            }
        }
    }
}

/// 解析 `CC_PARTNER_L3_TARGET`。
fn l3_target_from_env() -> Option<AgentTarget> {
    let raw = env::var("CC_PARTNER_L3_TARGET").ok()?;
    AgentTarget::parse(raw.trim())
}

/// 构造隔离探测环境。
///
/// Business Logic: L3 不得污染开发者真实 home / CODEX_HOME。
/// Code Logic: temp home + 注入 PATH 条目（继承当前 PATH 以便找到 CLI）。
fn isolated_env(home: &Path) -> TargetEnvironment {
    let mut vars = BTreeMap::new();
    vars.insert(
        "CODEX_HOME".into(),
        home.join(".codex").to_string_lossy().into_owned(),
    );
    vars.insert(
        "OPENCODE_CONFIG_DIR".into(),
        home.join(".config")
            .join("opencode")
            .to_string_lossy()
            .into_owned(),
    );
    vars.insert(
        "OPENCODE_CONFIG".into(),
        home.join(".config")
            .join("opencode")
            .join("opencode.json")
            .to_string_lossy()
            .into_owned(),
    );
    vars.insert(
        "CLAUDE_CONFIG_DIR".into(),
        home.join(".claude").to_string_lossy().into_owned(),
    );
    // 用当前 process PATH 解析真实 CLI，但配置根隔离
    let path_entries = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    TargetEnvironment {
        home: home.to_path_buf(),
        vars,
        path_entries,
    }
}

fn ensure_isolated_layout(home: &Path) {
    let _ = fs::create_dir_all(home.join(".claude"));
    let _ = fs::create_dir_all(home.join(".codex"));
    let _ = fs::create_dir_all(home.join(".config").join("opencode"));
    let _ = fs::create_dir_all(home.join(".agents").join("skills"));
}

fn adapter_for(target: AgentTarget) -> &'static dyn AssetAdapter {
    match target {
        AgentTarget::Claude => &ClaudeInstructionAdapter,
        AgentTarget::Codex => &CodexInstructionAdapter,
        AgentTarget::OpenCode => &OpenCodeInstructionAdapter,
    }
}

fn command_name(target: AgentTarget) -> &'static str {
    match target {
        AgentTarget::Claude => "claude",
        AgentTarget::Codex => "codex",
        AgentTarget::OpenCode => "opencode",
    }
}

/// L3：对选定 target 跑真实 CLI 合同。
///
/// Business Logic: 比对 normalized version 与 currentTestedVersion；不匹配只打印指纹/版本。
/// Code Logic: ignored + 环境变量选择 target；隔离 home；probe + evaluate。
#[test]
#[ignore = "L3 real CLI contract; set CC_PARTNER_L3_TARGET=claude|codex|opencode"]
fn l3_cli_contract_for_selected_target() {
    let target = l3_target_from_env().unwrap_or_else(|| {
        panic!(
            "set CC_PARTNER_L3_TARGET to claude|codex|opencode (got {:?})",
            env::var("CC_PARTNER_L3_TARGET")
        )
    });

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis();
    let home = env::temp_dir().join(format!("cc-partner-l3-{}-{}", target.as_str(), stamp));
    fs::create_dir_all(&home).expect("temp home");
    ensure_isolated_layout(&home);

    // 隔离 HOME 语义（部分 CLI 仍读 HOME）
    // 注意：不修改持久 process env 以外的副作用；仅本测试线程内 set_var。
    // SAFETY: 单线程 --test-threads=1 的 ignored L3 约定。
    env::set_var("HOME", &home);
    env::set_var("CODEX_HOME", home.join(".codex"));
    env::set_var("OPENCODE_CONFIG_DIR", home.join(".config").join("opencode"));
    env::set_var(
        "OPENCODE_CONFIG",
        home.join(".config").join("opencode").join("opencode.json"),
    );
    env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));

    let env_probe = isolated_env(&home);
    let adapter = adapter_for(target);
    let probe = adapter.probe(&env_probe).expect("probe");

    let exe = probe
        .executable
        .clone()
        .or_else(|| resolve_executable(command_name(target), &env_probe));
    let version = probe
        .version
        .clone()
        .or_else(|| exe.as_ref().and_then(|p| probe_cli_version(p)));

    let config_root = probe.config_root.clone();
    let fingerprint = compute_probe_fingerprint(
        target.as_str(),
        exe.as_deref(),
        version.as_deref(),
        &config_root,
    );

    let snapshot = RuntimeProbeSnapshot {
        target,
        executable: exe.clone(),
        version: version.clone(),
        config_root: config_root.clone(),
        fingerprint: fingerprint.clone(),
        help_fingerprint: None,
    };

    // 只打印版本/指纹，永不打印资产正文
    eprintln!("L3 probe identity: {}", format_probe_identity(&snapshot));

    let exe_path = exe.unwrap_or_else(|| {
        panic!(
            "executable not found for {} (version/fingerprint only: {})",
            target.as_str(),
            format_probe_identity(&snapshot)
        )
    });
    assert!(
        exe_path.exists(),
        "executable path missing: {}",
        exe_path.display()
    );

    let version_str = version.unwrap_or_else(|| {
        panic!(
            "version probe failed for {} ({})",
            target.as_str(),
            format_probe_identity(&snapshot)
        )
    });

    let manifest = builtin_support_manifest().expect("manifest");
    let record = find_target_record(&manifest, target).expect("manifest record");
    let expected_current = record
        .current_tested_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "manifest currentTestedVersion missing for {} — cannot certify",
                target.as_str()
            )
        });

    let prefix = record.executable_probe.version_prefix.as_deref();
    let suffix = record.executable_probe.version_suffix.as_deref();
    let actual_core = parse_semver_core(&version_str, prefix, suffix).unwrap_or_else(|| {
        panic!(
            "cannot parse runtime version for {} raw={:?} identity={}",
            target.as_str(),
            version_str,
            format_probe_identity(&snapshot)
        )
    });
    let expected_core = parse_semver_core(expected_current, prefix, suffix).unwrap_or_else(|| {
        panic!(
            "cannot parse manifest currentTestedVersion for {} raw={:?}",
            target.as_str(),
            expected_current
        )
    });

    if actual_core != expected_core {
        panic!(
            "version mismatch for {}: actual={:?} (raw={}) expected_current={:?} (raw={}) identity={}",
            target.as_str(),
            actual_core,
            version_str,
            expected_core,
            expected_current,
            format_probe_identity(&snapshot)
        );
    }

    // 合同检查：隔离配置根下 scan 不需要网络凭据
    let homes = TargetPathResolver::resolve_all(&env_probe);
    match target {
        AgentTarget::Claude => {
            assert!(homes.claude.config_root.starts_with(&home));
        }
        AgentTarget::Codex => {
            assert!(homes.codex.config_root.starts_with(&home));
        }
        AgentTarget::OpenCode => {
            assert!(homes.opencode.config_root.starts_with(&home));
        }
    }

    let eval = evaluate_target_support(&manifest, &snapshot);
    eprintln!(
        "L3 evaluate mode={:?} write_allowed={} reasons={:?}",
        match &eval.mode {
            EvaluatedSupportMode::Certified => "certified",
            EvaluatedSupportMode::ScanOnly { .. } => "scanOnly",
            EvaluatedSupportMode::Blocked { .. } => "blocked",
        },
        eval.write_allowed,
        eval.reasons
    );

    // 版本匹配后若 write 仍 blocked，属于 manifest 尚未写入 Supported* 证据——允许并提示
    if !eval.write_allowed {
        eprintln!(
            "NOTE: version matched currentTestedVersion but writes remain blocked (baseline/evidence). target={}",
            target.as_str()
        );
    }

    // scan 侧至少不应 Blocked 成不可 inventory（scanOnly 时 ReadOnly）
    let scan = eval.capability(TargetCapability::ScanPortableAssets);
    assert!(
        matches!(
            scan,
            CapabilitySupport::ReadOnly
                | CapabilitySupport::Supported
                | CapabilitySupport::SupportedAfterRestart
                | CapabilitySupport::ActivationRequired
        ),
        "scan capability unexpected: {:?}",
        scan
    );

    // cleanup best-effort
    let _ = fs::remove_dir_all(&home);
}
