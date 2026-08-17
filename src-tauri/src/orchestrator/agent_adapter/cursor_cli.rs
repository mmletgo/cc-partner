//! Cursor CLI visible terminal adapter。

use super::registry::{
    AgentAdapter, AgentAvailability, AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult,
    AgentUsageDelta, NativeAgentEvent, ResumeTerminalPolicy,
};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::error::AppError;
use crate::workbench::agent_runtime::AgentRuntimeMutation;
use std::process::Command;
use std::time::Duration;

/// Cursor CLI 探测/启动命令名（与 support-manifest `commandNames` 同序）。
const CURSOR_COMMAND_NAMES: &[&str] = &["cursor-agent", "agent"];

/// Cursor CLI 可见终端 adapter。
#[derive(Debug, Default, Clone, Copy)]
pub struct CursorCliAdapter;

impl CursorCliAdapter {
    /// 按序探测 Cursor CLI，优先 `cursor-agent`，避免命中 Grok 的 `agent` symlink。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧实现只调 `agent`，会把 Grok Build 当成 Cursor。
    ///
    /// Code Logic（这个函数做什么）:
    ///     对 `CURSOR_COMMAND_NAMES` 依次 `--version`；首个成功的命令名作为 executable。
    fn probe_version() -> Result<(Option<String>, Option<String>), AppError> {
        let mut last_err: Option<AppError> = None;
        for name in CURSOR_COMMAND_NAMES {
            match Self::probe_named(name) {
                Ok((Some(exe), version)) => return Ok((Some(exe), version)),
                Ok((None, _)) => {}
                Err(err) => last_err = Some(err),
            }
        }
        if let Some(err) = last_err {
            return Err(err);
        }
        Ok((None, None))
    }

    /// 探测单个命令名的 `--version`。
    ///
    /// Business Logic: 与 Grok/Gemini adapter 相同的短超时，避免 probe 挂死。
    /// Code Logic: spawn `name --version`，成功则返回该命令名与版本行。
    fn probe_named(name: &str) -> Result<(Option<String>, Option<String>), AppError> {
        let mut child = match Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(_) => return Ok((None, None)),
        };
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
                        return Ok((Some(name.into()), version));
                    }
                    return Ok((None, None));
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::timeout(format!("{name} --version 超时")));
                }
                Err(err) => return Err(AppError::generic(format!("{name} probe 失败: {err}"))),
            }
        }
    }

    /// 启动/恢复用的 Cursor 命令名。
    ///
    /// Business Logic: 可见终端必须启动真正的 Cursor CLI，而不是 Grok 的 `agent`。
    /// Code Logic: 复用 probe 顺序；都不可用时仍返回首选名 `cursor-agent`。
    fn preferred_command() -> String {
        match Self::probe_version() {
            Ok((Some(exe), _)) => exe,
            _ => CURSOR_COMMAND_NAMES[0].to_string(),
        }
    }
}

impl AgentAdapter for CursorCliAdapter {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId::CursorCliVisible
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
        let stdin = if request.prompt.trim().is_empty() {
            None
        } else {
            Some(format!("{}\n", request.prompt.trim_end_matches('\n')))
        };
        Ok(AgentLaunchPlan {
            executable: Self::preferred_command(),
            args: vec![],
            stdin,
            env: vec![],
            completion: AgentCompletionContract::Manual,
        })
    }

    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        if let Some(native) = request
            .native_session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            return Ok(AgentLaunchPlan {
                executable: Self::preferred_command(),
                args: vec!["--resume".into(), native.to_string()],
                stdin: None,
                env: vec![],
                completion: AgentCompletionContract::Manual,
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

    fn extract_usage(&self, _event: &NativeAgentEvent) -> Option<AgentUsageDelta> {
        None
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
        AgentCompletionContract::Manual
    }

    fn resume_terminal_policy(&self) -> ResumeTerminalPolicy {
        ResumeTerminalPolicy::Fresh
    }
}
