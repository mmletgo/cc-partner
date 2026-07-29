//! OpenCode visible terminal adapter（`openCodeVisible`）。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator 选择 OpenCode 时必须走官方 TUI flag + project 内 runtime bridge + HookEvent，
//!     禁止在缺少 bridge 时回落 Sentinel/stdout 猜测完成。
//!
//! Code Logic（这个模块做什么）:
//!     probe 经 Gate B support contract 定位 `opencode` 与 exact L3 evidence；
//!     launch=`opencode --prompt <prompt>`；resume=`opencode --session <id> --prompt <prompt>`；
//!     completion 固定 HookEvent；resume 终端策略 Fresh；usage 仅在事件字段存在时透传。

use super::registry::{
    AgentAdapter, AgentAvailability, AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult,
    AgentUsageDelta, NativeAgentEvent, ResumeTerminalPolicy,
};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::agent_hub::support::{
    builtin_support_manifest, evaluate_target_support, find_target_record, parse_semver_core,
    CapabilitySupport, RuntimeProbeSnapshot, TargetCapability,
};
use crate::agent_hub::targets::{
    compute_probe_fingerprint, probe_cli_version, resolve_executable, TargetEnvironment,
    TargetPathResolver,
};
use crate::error::AppError;
use crate::workbench::agent_runtime::{
    opencode_bridge::{
        OpenCodeBridgeOutcome, OpenCodeRuntimeBridge, CODE_EXTERNAL_COLLISION,
        CODE_RUNTIME_BRIDGE_REQUIRED, OPENCODE_VISIBLE_PROVIDER_ID,
    },
    AgentRuntimeMutation,
};
use crate::AgentTarget;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// OpenCode 缺少 L3 runtime evidence 的稳定 reason。
pub const REASON_L3_RUNTIME_EVIDENCE_MISSING: &str = "l3_runtime_evidence_missing";
/// OpenCode CLI 版本不在 support manifest 认证范围。
pub const REASON_UNSUPPORTED_CLI_VERSION: &str = "unsupported_cli_version";
/// OpenCode executable 不可用。
pub const REASON_PROVIDER_UNAVAILABLE: &str = "provider_unavailable";
/// project 未 opt-in / bridge 未物化。
pub const REASON_RUNTIME_BRIDGE_REQUIRED: &str = CODE_RUNTIME_BRIDGE_REQUIRED;
/// bridge 保留路径被外部占用。
pub const REASON_EXTERNAL_COLLISION: &str = CODE_EXTERNAL_COLLISION;

/// OpenCode visible adapter。
///
/// Business Logic（为什么需要这个结构体）:
///     把 OpenCode TUI 启动语义与 runtime-bridge 依赖收敛到 registry。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit；实现 AgentAdapter。
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    /// 解析 owner 本机探测环境（PATH + home 派生配置根）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gate B probe 不得污染 process env，但需要 PATH 找到真实 CLI。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 `TargetEnvironment`：home + PATH entries。
    fn probe_environment() -> TargetEnvironment {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let path_entries = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        TargetEnvironment {
            home,
            vars: Default::default(),
            path_entries,
        }
    }

    /// 有界 `opencode --version` 回落路径。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     当 support 解析器未返回 version 时仍可短探测可用性。
    ///
    /// Code Logic（这个函数做什么）:
    ///     2s 超时、4KiB 输出上限；成功返回 (exe, version)。
    fn probe_version_fallback(exe: &Path) -> Result<Option<String>, AppError> {
        let mut child = Command::new(exe)
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| AppError::generic("opencode executable unavailable"))?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut buf = Vec::new();
                    if let Some(out) = child.stdout.take() {
                        use std::io::Read;
                        let _ = out.take(4096).read_to_end(&mut buf);
                    }
                    let text = String::from_utf8_lossy(&buf).trim().to_string();
                    if status.success() && !text.is_empty() {
                        return Ok(Some(text.chars().take(128).collect()));
                    }
                    return Ok(None);
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::timeout("opencode --version 超时"));
                }
                Err(err) => return Err(AppError::generic(format!("opencode probe 失败: {err}"))),
            }
        }
    }

    /// 判断 support 是否具备 OpenCode runtime（visible runner）evidence。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     缺 exact L3 evidence 时 provider 必须 Unavailable，禁止宣称可写 runtime。
    ///
    /// Code Logic（这个函数做什么）:
    ///     liveReload 能力为 Supported* 且 evaluate 非 Blocked 才通过；基线 Blocked → missing evidence。
    fn runtime_evidence_reason(version: Option<&str>, exe: Option<&Path>) -> Option<&'static str> {
        let Ok(manifest) = builtin_support_manifest() else {
            return Some(REASON_L3_RUNTIME_EVIDENCE_MISSING);
        };
        let Some(record) = find_target_record(&manifest, AgentTarget::OpenCode) else {
            return Some(REASON_L3_RUNTIME_EVIDENCE_MISSING);
        };
        let Some(version) = version.map(str::trim).filter(|v| !v.is_empty()) else {
            return Some(REASON_PROVIDER_UNAVAILABLE);
        };
        let prefix = record.executable_probe.version_prefix.as_deref();
        let suffix = record.executable_probe.version_suffix.as_deref();
        let Some(actual_core) = parse_semver_core(version, prefix, suffix) else {
            return Some(REASON_UNSUPPORTED_CLI_VERSION);
        };
        let Some(expected_raw) = record
            .current_tested_version
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Some(REASON_L3_RUNTIME_EVIDENCE_MISSING);
        };
        let Some(expected_core) = parse_semver_core(expected_raw, prefix, suffix) else {
            return Some(REASON_L3_RUNTIME_EVIDENCE_MISSING);
        };
        if actual_core != expected_core {
            return Some(REASON_UNSUPPORTED_CLI_VERSION);
        }
        let env = Self::probe_environment();
        let homes = TargetPathResolver::resolve_all(&env);
        let config_root = homes.opencode.config_root.clone();
        let fingerprint = compute_probe_fingerprint(
            AgentTarget::OpenCode.as_str(),
            exe,
            Some(version),
            &config_root,
        );
        let snap = RuntimeProbeSnapshot {
            target: AgentTarget::OpenCode,
            executable: exe.map(|p| p.to_path_buf()),
            version: Some(version.to_string()),
            config_root,
            fingerprint,
            help_fingerprint: None,
        };
        let eval = evaluate_target_support(&manifest, &snap);
        // 基线 liveReload=Blocked：诚实返回 NOT VERIFIED / evidence missing。
        let cap = eval.capability(TargetCapability::LiveReload);
        if matches!(
            cap,
            CapabilitySupport::Supported
                | CapabilitySupport::SupportedAfterRestart
                | CapabilitySupport::ActivationRequired
        ) {
            None
        } else {
            Some(REASON_L3_RUNTIME_EVIDENCE_MISSING)
        }
    }

    /// 组装 launch/resume 的 env（agent/terminal 身份）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     OpenCode bridge 依赖 shell 中的 agent/terminal ID；plan.env 由 renderer 注入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 CC_PARTNER_AGENT_SESSION_ID / CC_PARTNER_TERMINAL_SESSION_ID（非空时）。
    fn identity_env(request: &AgentLaunchRequest) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if !request.agent_session_id.trim().is_empty() {
            env.push((
                "CC_PARTNER_AGENT_SESSION_ID".into(),
                request.agent_session_id.trim().to_string(),
            ));
        }
        if !request.terminal_session_id.trim().is_empty() {
            env.push((
                "CC_PARTNER_TERMINAL_SESSION_ID".into(),
                request.terminal_session_id.trim().to_string(),
            ));
        }
        env
    }
}

impl AgentAdapter for OpenCodeAdapter {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId::OpenCodeVisible
    }

    fn probe(&self) -> Result<AgentProbeResult, AppError> {
        let env = Self::probe_environment();
        let exe = resolve_executable("opencode", &env);
        let version = exe.as_ref().and_then(|p| probe_cli_version(p)).or_else(|| {
            exe.as_ref()
                .and_then(|p| Self::probe_version_fallback(p).ok().flatten())
        });

        let Some(exe_path) = exe else {
            return Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: None,
                version: None,
                reason_code: Some(REASON_PROVIDER_UNAVAILABLE.into()),
            });
        };

        if let Some(reason) = Self::runtime_evidence_reason(version.as_deref(), Some(&exe_path)) {
            return Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: Some(exe_path.display().to_string()),
                version,
                reason_code: Some(reason.into()),
            });
        }

        Ok(AgentProbeResult {
            provider_id: self.provider_id(),
            availability: AgentAvailability::Available,
            executable: Some(exe_path.display().to_string()),
            version,
            reason_code: None,
        })
    }

    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        if request.prompt.trim().is_empty() {
            return Err(AppError::generic("OpenCode launch prompt 不能为空"));
        }
        Ok(AgentLaunchPlan {
            executable: "opencode".into(),
            args: vec![
                "--prompt".into(),
                request.prompt.trim_end_matches('\n').to_string(),
            ],
            stdin: None,
            env: Self::identity_env(request),
            completion: AgentCompletionContract::HookEvent,
        })
    }

    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        if request.prompt.trim().is_empty() {
            return Err(AppError::generic("OpenCode resume prompt 不能为空"));
        }
        let Some(native) = request
            .native_session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            return Err(AppError::generic(
                "OpenCode resume 需要可靠 native session id，禁止伪装 resume",
            ));
        };
        Ok(AgentLaunchPlan {
            executable: "opencode".into(),
            args: vec![
                "--session".into(),
                native.to_string(),
                "--prompt".into(),
                request.prompt.trim_end_matches('\n').to_string(),
            ],
            stdin: None,
            env: Self::identity_env(request),
            completion: AgentCompletionContract::HookEvent,
        })
    }

    fn normalize_runtime_event(
        &self,
        event: NativeAgentEvent,
    ) -> Result<AgentRuntimeMutation, AppError> {
        Ok(AgentRuntimeMutation {
            agent_session_id: event.agent_session_id,
            terminal_session_id: event.terminal_session_id,
            expected_version: event.expected_version,
            event_version: event.event_version,
            phase: event.phase,
            native_session_id: event.native_session_id,
            outcome_code: event.outcome_code,
            occurred_at: event.occurred_at,
        })
    }

    fn extract_usage(&self, event: &NativeAgentEvent) -> Option<AgentUsageDelta> {
        // 官方事件未提供 usage 时禁止估算。
        if event.usage_input_tokens.is_none() && event.usage_output_tokens.is_none() {
            return None;
        }
        Some(AgentUsageDelta {
            model_id: None,
            input_tokens: event.usage_input_tokens,
            output_tokens: event.usage_output_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_major: None,
            cost_currency: None,
        })
    }

    fn interrupt_input(&self) -> &'static str {
        "\u{3}"
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn supports_usage(&self) -> bool {
        // 仅在官方事件显式提供时透传；无事件字段不估算。
        true
    }

    fn completion_contract(&self) -> AgentCompletionContract {
        AgentCompletionContract::HookEvent
    }

    fn resume_terminal_policy(&self) -> ResumeTerminalPolicy {
        ResumeTerminalPolicy::Fresh
    }
}

/// 校验 OpenCode project preflight（worktree/session 创建前）。
///
/// Business Logic（为什么需要这个函数）:
///     未 opt-in / 碰撞 / 缺 bridge / CLI 不可用时必须 fail-closed，且不得创建 worktree。
///
/// Code Logic（这个函数做什么）:
///     返回稳定 reason code；None 表示可继续（仍须随后 materialize/verify）。
pub fn opencode_preflight_block_reason(
    probe: &AgentProbeResult,
    bridge: &OpenCodeBridgeOutcome,
) -> Option<&'static str> {
    if probe.availability != AgentAvailability::Available {
        let s = probe
            .reason_code
            .as_deref()
            .unwrap_or(REASON_PROVIDER_UNAVAILABLE);
        return Some(match s {
            REASON_L3_RUNTIME_EVIDENCE_MISSING => REASON_L3_RUNTIME_EVIDENCE_MISSING,
            REASON_UNSUPPORTED_CLI_VERSION => REASON_UNSUPPORTED_CLI_VERSION,
            REASON_EXTERNAL_COLLISION => REASON_EXTERNAL_COLLISION,
            REASON_RUNTIME_BRIDGE_REQUIRED => REASON_RUNTIME_BRIDGE_REQUIRED,
            _ => REASON_PROVIDER_UNAVAILABLE,
        });
    }
    match bridge {
        OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. } => Some(REASON_RUNTIME_BRIDGE_REQUIRED),
        OpenCodeBridgeOutcome::ExternalCollision { .. } => Some(REASON_EXTERNAL_COLLISION),
        OpenCodeBridgeOutcome::Verified { .. }
        | OpenCodeBridgeOutcome::Materialized { .. }
        | OpenCodeBridgeOutcome::Preview { .. } => None,
    }
}

/// 在已 opt-in 项目上 ensure bridge 并 verify。
///
/// Business Logic（为什么需要这个函数）:
///     新 worktree 启动 OpenCode 前必须 hash 验证派生 Plugin。
///
/// Code Logic（这个函数做什么）:
///     materialize → verify；失败返回稳定 AppError 文案（含 reason code）。
pub fn ensure_opencode_bridge_verified(
    project_root: &Path,
    opted_in: bool,
) -> Result<OpenCodeBridgeOutcome, AppError> {
    if !opted_in {
        return Err(AppError::generic(format!(
            "{REASON_RUNTIME_BRIDGE_REQUIRED}: project 未 opt-in OpenCode runtime bridge"
        )));
    }
    let mat = OpenCodeRuntimeBridge::materialize(project_root, true)?;
    if let Some(reason) = match &mat {
        OpenCodeBridgeOutcome::ExternalCollision { .. } => Some(REASON_EXTERNAL_COLLISION),
        OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. } => Some(REASON_RUNTIME_BRIDGE_REQUIRED),
        _ => None,
    } {
        return Err(AppError::generic(format!(
            "{reason}: OpenCode runtime bridge 不可用"
        )));
    }
    let verified = OpenCodeRuntimeBridge::verify(project_root, true);
    match verified {
        OpenCodeBridgeOutcome::Verified { .. } => Ok(verified),
        OpenCodeBridgeOutcome::ExternalCollision { .. } => Err(AppError::generic(format!(
            "{REASON_EXTERNAL_COLLISION}: OpenCode runtime bridge 路径冲突"
        ))),
        _ => Err(AppError::generic(format!(
            "{REASON_RUNTIME_BRIDGE_REQUIRED}: OpenCode runtime bridge 未验证"
        ))),
    }
}

/// 稳定 wire id 校验（测试/文档对齐）。
#[allow(dead_code)]
pub fn open_code_visible_wire() -> &'static str {
    debug_assert_eq!(OPENCODE_VISIBLE_PROVIDER_ID, "openCodeVisible");
    AgentProviderId::OpenCodeVisible.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::agent_adapter::registry::{
        render_terminal_command, TerminalShellDialect,
    };

    fn request(prompt: &str, native: Option<&str>) -> AgentLaunchRequest {
        AgentLaunchRequest {
            agent_session_id: "agent-1".into(),
            terminal_session_id: "term-1".into(),
            cwd: "/tmp/proj".into(),
            prompt: prompt.into(),
            native_session_id: native.map(str::to_string),
            max_turns: 1,
            stall_timeout_ms: 300_000,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     launch argv 必须是 `opencode --prompt <prompt>`，completion=HookEvent。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_launch_plan 断言 executable/args/completion/env。
    #[test]
    fn opencode_launch_plan_matches_documented_flags() {
        let plan = OpenCodeAdapter
            .build_launch_plan(&request("fix the suite", None))
            .unwrap();
        assert_eq!(plan.executable, "opencode");
        assert_eq!(
            plan.args,
            vec!["--prompt".to_string(), "fix the suite".to_string()]
        );
        assert!(plan.stdin.is_none());
        assert_eq!(plan.completion, AgentCompletionContract::HookEvent);
        assert!(plan
            .env
            .iter()
            .any(|(k, v)| k == "CC_PARTNER_AGENT_SESSION_ID" && v == "agent-1"));
        assert_eq!(
            OpenCodeAdapter.completion_contract(),
            AgentCompletionContract::HookEvent
        );
        assert_eq!(
            OpenCodeAdapter.resume_terminal_policy(),
            ResumeTerminalPolicy::Fresh
        );
        assert_eq!(OpenCodeAdapter.interrupt_input(), "\u{3}");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     resume 必须带 --session 且缺 native 失败；策略 Fresh。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_resume_plan 有/无 native。
    #[test]
    fn opencode_resume_requires_native_and_is_fresh() {
        let err = OpenCodeAdapter
            .build_resume_plan(&request("continue", None))
            .unwrap_err();
        assert!(err.to_string().contains("native"));
        let plan = OpenCodeAdapter
            .build_resume_plan(&request("continue", Some("sess-abc")))
            .unwrap();
        assert_eq!(
            plan.args,
            vec![
                "--session".to_string(),
                "sess-abc".to_string(),
                "--prompt".to_string(),
                "continue".to_string()
            ]
        );
        assert_eq!(
            OpenCodeAdapter.resume_terminal_policy(),
            ResumeTerminalPolicy::Fresh
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 prompt 不得启动 OpenCode。
    ///
    /// Code Logic（这个测试做什么）:
    ///     launch/resume 空串 Err。
    #[test]
    fn opencode_rejects_empty_prompt() {
        assert!(OpenCodeAdapter
            .build_launch_plan(&request("", None))
            .is_err());
        assert!(OpenCodeAdapter
            .build_launch_plan(&request("   ", None))
            .is_err());
        assert!(OpenCodeAdapter
            .build_resume_plan(&request("", Some("n")))
            .is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无官方 usage 字段时不得估算。
    ///
    /// Code Logic（这个测试做什么）:
    ///     extract_usage 全 None → None。
    #[test]
    fn opencode_does_not_estimate_usage() {
        let event = NativeAgentEvent {
            agent_session_id: "a".into(),
            terminal_session_id: "t".into(),
            expected_version: 1,
            event_version: 2,
            phase: crate::workbench::agent_runtime::AgentSessionPhase::Working,
            native_session_id: Some("n".into()),
            outcome_code: None,
            occurred_at: "2026-01-01T00:00:00Z".into(),
            usage_input_tokens: None,
            usage_output_tokens: None,
            raw_kind: None,
        };
        assert!(OpenCodeAdapter.extract_usage(&event).is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     prompt 含 shell metachar 时必须作为单一字面 argv，不可执行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     render_terminal_command Posix 断言引号包裹。
    #[test]
    fn opencode_prompt_rendered_as_literal_argv() {
        let dangerous = r#"hello $(rm -rf /) `id` ; echo %VAR% "quoted" 'x'"#;
        let plan = OpenCodeAdapter
            .build_launch_plan(&request(dangerous, None))
            .unwrap();
        let rendered = render_terminal_command(&plan, TerminalShellDialect::Posix).unwrap();
        // env 前缀后是被单引号包裹的 executable/args。
        assert!(rendered.contains("'opencode'"));
        assert!(rendered.contains("'--prompt'"));
        // 危险片段必须出现在引号字面量中，不能裸露在 command 位置。
        assert!(rendered.contains("hello"));
        assert!(rendered.contains("$(rm -rf /)"));
        assert!(!rendered.lines().any(|l| l.trim_start().starts_with("$(")));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preflight 在 RuntimeBridgeRequired / collision / unavailable 时稳定阻断。
    ///
    /// Code Logic（这个测试做什么）:
    ///     opencode_preflight_block_reason 各分支。
    #[test]
    fn preflight_blocks_without_bridge_or_evidence() {
        let unavailable = AgentProbeResult {
            provider_id: AgentProviderId::OpenCodeVisible,
            availability: AgentAvailability::Unavailable,
            executable: None,
            version: None,
            reason_code: Some(REASON_L3_RUNTIME_EVIDENCE_MISSING.into()),
        };
        let required = OpenCodeBridgeOutcome::RuntimeBridgeRequired {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
        };
        assert_eq!(
            opencode_preflight_block_reason(&unavailable, &required),
            Some(REASON_L3_RUNTIME_EVIDENCE_MISSING)
        );

        let available = AgentProbeResult {
            provider_id: AgentProviderId::OpenCodeVisible,
            availability: AgentAvailability::Available,
            executable: Some("opencode".into()),
            version: Some("1.0.0".into()),
            reason_code: None,
        };
        assert_eq!(
            opencode_preflight_block_reason(&available, &required),
            Some(REASON_RUNTIME_BRIDGE_REQUIRED)
        );
        let collision = OpenCodeBridgeOutcome::ExternalCollision {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
            absolute_path: "/tmp/x".into(),
            current_hash: Some("def".into()),
        };
        assert_eq!(
            opencode_preflight_block_reason(&available, &collision),
            Some(REASON_EXTERNAL_COLLISION)
        );
        let verified = OpenCodeBridgeOutcome::Verified {
            relative_path: ".opencode/plugins/cc-partner-runtime.ts".into(),
            source_hash: "abc".into(),
            absolute_path: "/tmp/x".into(),
        };
        assert_eq!(opencode_preflight_block_reason(&available, &verified), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     基线 support manifest 下 OpenCode probe 必须 fail-closed（L3 NOT VERIFIED）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     probe 返回 Unavailable 且 reason 含 l3 或 unavailable。
    #[test]
    fn baseline_probe_is_unavailable_without_l3_evidence() {
        let result = OpenCodeAdapter.probe().unwrap();
        assert_eq!(result.availability, AgentAvailability::Unavailable);
        let reason = result.reason_code.unwrap_or_default();
        assert!(
            reason.contains("l3")
                || reason.contains("unavailable")
                || reason.contains("unsupported"),
            "unexpected reason: {reason}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     wire 值必须固定 openCodeVisible。
    ///
    /// Code Logic（这个测试做什么）:
    ///     as_str / parse 往返。
    #[test]
    fn open_code_provider_wire_roundtrip() {
        assert_eq!(AgentProviderId::OpenCodeVisible.as_str(), "openCodeVisible");
        assert_eq!(
            AgentProviderId::parse("openCodeVisible").unwrap(),
            AgentProviderId::OpenCodeVisible
        );
        assert_eq!(
            AgentProviderId::OpenCodeVisible.default_completion_contract(),
            AgentCompletionContract::HookEvent
        );
    }
}
