//! Codex visible terminal adapter。
//!
//! Business Logic（为什么需要这个模块）:
//!     owner 可在 WORKFLOW 选择 codexVisible；不可用时 fail-closed 返回 provider_unavailable。
//!
//! Code Logic（这个模块做什么）:
//!     probe `codex --version`（2s/4KiB）；launch 用受控 args/stdin；resume 仅在有 native session 时。

use super::registry::{
    AgentAdapter, AgentAvailability, AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult,
    AgentUsageDelta, NativeAgentEvent,
};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::error::AppError;
use crate::workbench::agent_runtime::AgentRuntimeMutation;
use std::process::Command;
use std::time::Duration;

/// Codex visible adapter。
///
/// Business Logic（为什么需要这个结构体）:
///     把 Codex CLI 启动语义收敛到 registry，避免 Runner 拼 shell。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit；实现 AgentAdapter。
#[derive(Debug, Default, Clone, Copy)]
pub struct CodexAdapter;

impl CodexAdapter {
    /// 有界 `codex --version`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     catalog 需要可用性，不能阻塞。
    ///
    /// Code Logic（这个函数做什么）:
    ///     2s 超时、4KiB 输出上限。
    fn probe_version() -> Result<(Option<String>, Option<String>), AppError> {
        let mut child = Command::new("codex")
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| AppError::generic("codex executable unavailable"))?;
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
                    if status.success() {
                        let version = if text.is_empty() {
                            None
                        } else {
                            Some(text.chars().take(128).collect())
                        };
                        return Ok((Some("codex".into()), version));
                    }
                    return Ok((None, None));
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::timeout("codex --version 超时"));
                }
                Err(err) => return Err(AppError::generic(format!("codex probe 失败: {err}"))),
            }
        }
    }
}

impl AgentAdapter for CodexAdapter {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId::CodexVisible
    }

    fn probe(&self) -> Result<AgentProbeResult, AppError> {
        match Self::probe_version() {
            Ok((Some(exe), version)) => Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Available,
                executable: Some(exe),
                version,
                reason_code: None,
            }),
            _ => Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: None,
                version: None,
                reason_code: Some("provider_unavailable".into()),
            }),
        }
    }

    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        if request.prompt.trim().is_empty() {
            return Err(AppError::generic("Codex launch prompt 不能为空"));
        }
        // 受控 visible 启动：`codex` + 将 prompt 经 stdin 注入，不拼接 shell。
        Ok(AgentLaunchPlan {
            executable: "codex".into(),
            args: vec![],
            stdin: Some(format!("{}\n", request.prompt.trim_end_matches('\n'))),
            env: vec![],
            completion: AgentCompletionContract::HookEvent,
        })
    }

    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        let Some(native) = request
            .native_session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            return Err(AppError::generic(
                "Codex resume 需要可靠 native session id，禁止伪装 resume",
            ));
        };
        Ok(AgentLaunchPlan {
            executable: "codex".into(),
            args: vec!["resume".into(), native.to_string()],
            stdin: Some(format!("{}\n", request.prompt.trim_end_matches('\n'))),
            env: vec![],
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
        true
    }

    fn completion_contract(&self) -> AgentCompletionContract {
        AgentCompletionContract::HookEvent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     Codex launch 不得拼 shell，且 completion 为 HookEvent。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_launch_plan 断言 executable=codex、stdin 含 prompt。
    #[test]
    fn codex_launch_plan_is_controlled() {
        let plan = CodexAdapter
            .build_launch_plan(&AgentLaunchRequest {
                agent_session_id: "a".into(),
                terminal_session_id: "t".into(),
                cwd: "/tmp".into(),
                prompt: "do work".into(),
                native_session_id: None,
                max_turns: 2,
                stall_timeout_ms: 60_000,
            })
            .unwrap();
        assert_eq!(plan.executable, "codex");
        assert!(plan.args.is_empty());
        assert_eq!(plan.stdin.as_deref(), Some("do work\n"));
        assert_eq!(plan.completion, AgentCompletionContract::HookEvent);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无 native id 时不得伪装 resume。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_resume_plan 无 native → Err。
    #[test]
    fn codex_resume_requires_native_session() {
        let err = CodexAdapter
            .build_resume_plan(&AgentLaunchRequest {
                agent_session_id: "a".into(),
                terminal_session_id: "t".into(),
                cwd: "/tmp".into(),
                prompt: "continue".into(),
                native_session_id: None,
                max_turns: 2,
                stall_timeout_ms: 60_000,
            })
            .unwrap_err();
        assert!(err.to_string().contains("native"));
    }
}
