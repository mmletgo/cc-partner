//! Claude Code visible terminal adapter。
//!
//! Business Logic（为什么需要这个模块）:
//!     首版必须把现有 `claude\n{prompt}\n` 注入语义迁入 adapter，保证 claim/attempt/sentinel 行为不变。
//!
//! Code Logic（这个模块做什么）:
//!     probe `claude --version`；launch/resume plan；normalize 事件到 A1 mutation；interrupt=\u{3}。

use super::registry::{
    AgentAdapter, AgentAvailability, AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult,
    AgentUsageDelta, NativeAgentEvent,
};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::error::AppError;
use crate::workbench::agent_runtime::AgentRuntimeMutation;
use std::process::Command;
use std::time::Duration;

/// Claude Code visible adapter。
///
/// Business Logic（为什么需要这个结构体）:
///     保持与历史 Runner 硬编码行为的 characterization 等价。
///
/// Code Logic（这个结构体做什么）:
///     无状态 unit struct，实现 AgentAdapter。
#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// 有界执行 `claude --version`。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     probe 不能无限阻塞 owner 启动路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     spawn 子进程，超时 2s，stdout 截断 4KiB。
    fn probe_version() -> Result<(Option<String>, Option<String>), AppError> {
        let mut child = Command::new("claude")
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| AppError::generic("claude executable unavailable"))?;
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
                        return Ok((Some("claude".into()), version));
                    }
                    return Ok((None, None));
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::timeout("claude --version 超时"));
                }
                Err(err) => return Err(AppError::generic(format!("claude probe 失败: {err}"))),
            }
        }
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId::ClaudeCodeVisible
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
            Ok((None, _)) => Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: None,
                version: None,
                reason_code: Some("provider_unavailable".into()),
            }),
            Err(_) => Ok(AgentProbeResult {
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
            return Err(AppError::generic("Claude launch prompt 不能为空"));
        }
        Ok(AgentLaunchPlan {
            executable: "claude".into(),
            args: vec![],
            stdin: Some(format!("{}\n", request.prompt.trim_end_matches('\n'))),
            env: vec![],
            completion: AgentCompletionContract::SentinelLine,
        })
    }

    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        // Claude resume：`--resume <native>` 若有 native id；否则退化为新 launch。
        if let Some(native) = request
            .native_session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(AgentLaunchPlan {
                executable: "claude".into(),
                args: vec!["--resume".into(), native.to_string()],
                stdin: Some(format!("{}\n", request.prompt.trim_end_matches('\n'))),
                env: vec![],
                completion: AgentCompletionContract::SentinelLine,
            });
        }
        self.build_launch_plan(request)
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
            input_tokens: event.usage_input_tokens,
            output_tokens: event.usage_output_tokens,
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
        AgentCompletionContract::SentinelLine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(prompt: &str) -> AgentLaunchRequest {
        AgentLaunchRequest {
            agent_session_id: "agent-1".into(),
            terminal_session_id: "term-1".into(),
            cwd: "/tmp/proj".into(),
            prompt: prompt.into(),
            native_session_id: None,
            max_turns: 1,
            stall_timeout_ms: 300_000,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude characterization：可见终端必须保持 `claude` + prompt stdin + SentinelLine。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_launch_plan("fix tests") 断言 executable/stdin/completion。
    #[test]
    fn claude_adapter_keeps_visible_terminal_input() {
        let plan = ClaudeCodeAdapter
            .build_launch_plan(&request("fix tests"))
            .unwrap();
        assert_eq!(plan.executable, "claude");
        assert_eq!(plan.stdin.as_deref(), Some("fix tests\n"));
        assert_eq!(plan.completion, AgentCompletionContract::SentinelLine);
        assert_eq!(plan.to_terminal_input(), "claude\nfix tests\n");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 prompt 不得启动 Claude。
    ///
    /// Code Logic（这个测试做什么）:
    ///     build_launch_plan("") 返回错误。
    #[test]
    fn claude_adapter_rejects_empty_prompt() {
        assert!(ClaudeCodeAdapter.build_launch_plan(&request("")).is_err());
        assert!(ClaudeCodeAdapter.build_launch_plan(&request("   ")).is_err());
    }
}
