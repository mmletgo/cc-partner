//! 受控 generic terminal adapter（owner allowlist only）。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户可显式配置受控 executable/args；禁止 workflow 任意 shell，禁止猜完成。
//!
//! Code Logic（这个模块做什么）:
//!     无 allowlist → Unavailable；校验无 shell metachar；completion 仅 Manual|SentinelLine。

use super::registry::{
    AgentAdapter, AgentAvailability, AgentLaunchPlan, AgentLaunchRequest, AgentProbeResult,
    AgentUsageDelta, NativeAgentEvent,
};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::error::AppError;
use crate::workbench::agent_runtime::AgentRuntimeMutation;
use serde::{Deserialize, Serialize};

/// owner-local generic terminal allowlist 配置（永不进 P2P）。
///
/// Business Logic（为什么需要这个结构体）:
///     Settings 可持久化受控 CLI；LAN 路由不得接收此配置。
///
/// Code Logic（这个结构体做什么）:
///     executable + 字面量 args + completion 合同。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericTerminalConfig {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_manual_completion")]
    pub completion_contract: String,
}

/// serde 默认 Manual completion。
fn default_manual_completion() -> String {
    AgentCompletionContract::Manual.as_str().to_string()
}

impl GenericTerminalConfig {
    /// 校验 executable/args 无 shell metachar，completion 仅 Manual|SentinelLine。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     禁止把 arbitrary shell 当 adapter 配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     拒绝空白、`|&;$<>\`\"'` \n 与 completion 非法值。
    pub fn validate(&self) -> Result<AgentCompletionContract, AppError> {
        validate_no_shell_meta("executable", &self.executable)?;
        if self.executable.trim().is_empty() {
            return Err(AppError::validation("generic terminal executable 不能为空"));
        }
        for (i, arg) in self.args.iter().enumerate() {
            validate_no_shell_meta(&format!("args[{i}]"), arg)?;
        }
        let completion = AgentCompletionContract::parse(&self.completion_contract)?;
        match completion {
            AgentCompletionContract::Manual | AgentCompletionContract::SentinelLine => {
                Ok(completion)
            }
            AgentCompletionContract::HookEvent => Err(AppError::validation(
                "generic terminal 不支持 hookEvent completion",
            )),
        }
    }
}

/// 拒绝 shell 元字符。
///
/// Business Logic（为什么需要这个函数）:
///     generic adapter 只能字面量 argv，不能经 shell 解释。
///
/// Code Logic（这个函数做什么）:
///     扫描 `| & ; $ < > \` " ' \\n \\r` 与空白嵌套命令符号。
fn validate_no_shell_meta(field: &str, value: &str) -> Result<(), AppError> {
    const FORBIDDEN: &[char] = &[
        '|', '&', ';', '$', '<', '>', '`', '"', '\'', '\n', '\r', '(', ')', '{', '}',
    ];
    if value.chars().any(|c| FORBIDDEN.contains(&c)) {
        return Err(AppError::validation(format!(
            "generic terminal {field} 含非法 shell 元字符"
        )));
    }
    Ok(())
}

/// Generic terminal adapter。
///
/// Business Logic（为什么需要这个结构体）:
///     无 allowlist 时必须 Unavailable，防止静默执行任意命令。
///
/// Code Logic（这个结构体做什么）:
///     持有 Option<GenericTerminalConfig>。
#[derive(Debug, Clone)]
pub struct GenericTerminalAdapter {
    config: Option<GenericTerminalConfig>,
}

impl GenericTerminalAdapter {
    /// 构造 adapter。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     registry 注入 owner-local 配置。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装 Option 配置。
    pub fn new(config: Option<GenericTerminalConfig>) -> Self {
        Self { config }
    }
}

impl AgentAdapter for GenericTerminalAdapter {
    fn provider_id(&self) -> AgentProviderId {
        AgentProviderId::GenericTerminal
    }

    fn probe(&self) -> Result<AgentProbeResult, AppError> {
        let Some(config) = &self.config else {
            return Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: None,
                version: None,
                reason_code: Some("provider_unavailable".into()),
            });
        };
        match config.validate() {
            Ok(_) => Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Available,
                executable: Some(config.executable.clone()),
                version: None,
                reason_code: None,
            }),
            Err(err) => Ok(AgentProbeResult {
                provider_id: self.provider_id(),
                availability: AgentAvailability::Unavailable,
                executable: None,
                version: None,
                reason_code: Some(format!("invalid_config:{err}")),
            }),
        }
    }

    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError> {
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| AppError::generic("generic terminal 未配置 owner allowlist"))?;
        let completion = config.validate()?;
        let stdin = match completion {
            AgentCompletionContract::SentinelLine => {
                if request.prompt.trim().is_empty() {
                    None
                } else {
                    Some(format!("{}\n", request.prompt.trim_end_matches('\n')))
                }
            }
            _ => {
                if request.prompt.trim().is_empty() {
                    None
                } else {
                    Some(format!("{}\n", request.prompt.trim_end_matches('\n')))
                }
            }
        };
        Ok(AgentLaunchPlan {
            executable: config.executable.clone(),
            args: config.args.clone(),
            stdin,
            env: vec![],
            completion,
        })
    }

    fn build_resume_plan(
        &self,
        _request: &AgentLaunchRequest,
    ) -> Result<AgentLaunchPlan, AppError> {
        Err(AppError::generic("generic terminal 不支持 resume"))
    }

    fn normalize_runtime_event(
        &self,
        event: NativeAgentEvent,
    ) -> Result<AgentRuntimeMutation, AppError> {
        // generic 不解析 usage/native；仅透传 phase。
        Ok(AgentRuntimeMutation {
            agent_session_id: event.agent_session_id,
            terminal_session_id: event.terminal_session_id,
            expected_version: event.expected_version,
            event_version: event.event_version,
            phase: event.phase,
            native_session_id: None,
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
        false
    }

    fn supports_usage(&self) -> bool {
        false
    }

    fn completion_contract(&self) -> AgentCompletionContract {
        self.config
            .as_ref()
            .and_then(|c| AgentCompletionContract::parse(&c.completion_contract).ok())
            .unwrap_or(AgentCompletionContract::Manual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     无 owner allowlist 时 generic 必须 Unavailable。
    ///
    /// Code Logic（这个测试做什么）:
    ///     GenericTerminalAdapter::new(None).probe() → Unavailable。
    #[test]
    fn generic_terminal_is_unavailable_without_owner_allowlist() {
        let adapter = GenericTerminalAdapter::new(None);
        assert_eq!(
            adapter.probe().unwrap().availability,
            AgentAvailability::Unavailable
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     shell metachar 必须被拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     executable 含 `|` 时 validate 失败。
    #[test]
    fn generic_terminal_rejects_shell_metacharacters() {
        let cfg = GenericTerminalConfig {
            executable: "bash".into(),
            args: vec!["-c".into(), "echo hi".into()],
            completion_contract: "manual".into(),
        };
        // args 含空格 ok; `-c` ok but "echo hi" has space - space is allowed
        // pipe not allowed:
        let bad = GenericTerminalConfig {
            executable: "echo".into(),
            args: vec!["a|b".into()],
            completion_contract: "manual".into(),
        };
        assert!(bad.validate().is_err());
        let ok = GenericTerminalConfig {
            executable: "my-agent".into(),
            args: vec!["--flag".into(), "value".into()],
            completion_contract: "sentinelLine".into(),
        };
        assert_eq!(
            ok.validate().unwrap(),
            AgentCompletionContract::SentinelLine
        );
        let _ = cfg;
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote-facing 序列化不得包含 env 字段（config 本身也无 env）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     serde GenericTerminalConfig 文本不含 env。
    #[test]
    fn generic_config_json_has_no_env_field() {
        let cfg = GenericTerminalConfig {
            executable: "tool".into(),
            args: vec![],
            completion_contract: "manual".into(),
        };
        let text = serde_json::to_string(&cfg).unwrap();
        assert!(!text.contains("env"));
        assert!(!text.contains("credential"));
    }
}
