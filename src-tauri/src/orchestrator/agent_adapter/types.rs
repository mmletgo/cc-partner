//! Agent adapter 强类型：provider / completion / attempt policy。
//!
//! Business Logic（为什么需要这个模块）:
//!     Orchestrator 需要把 WORKFLOW runner 配置收敛为 provider-neutral 的不可变 attempt 策略，
//!     避免 Runner 硬编码 Claude，并为 Codex/generic 提供 fail-closed 校验入口。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AgentProviderId`、`AgentCompletionContract`、`RunnerAttemptPolicy` 与解析/解析函数；
//!     wire 值固定为 `claudeCodeVisible|codexVisible|genericTerminal`。

use crate::error::AppError;
use serde::{Deserialize, Serialize};

/// stall timeout 下限（30 秒）。
pub const MIN_STALL_TIMEOUT_MS: i64 = 30_000;
/// stall timeout 上限（30 分钟）。
pub const MAX_STALL_TIMEOUT_MS: i64 = 30 * 60 * 1000;
/// development attempt 总数下限。
pub const MIN_MAX_TURNS: i64 = 1;
/// development attempt 总数上限。
pub const MAX_MAX_TURNS: i64 = 20;
/// 旧行/缺省 policy 默认 max_turns。
pub const DEFAULT_MAX_TURNS: i64 = 1;
/// 旧行/缺省 policy 默认 stall timeout（5 分钟）。
pub const DEFAULT_STALL_TIMEOUT_MS: i64 = 300_000;

/// 内置 Agent provider 标识（wire camelCase）。
///
/// Business Logic（为什么需要这个枚举）:
///     workflow / attempt 快照 / adapter registry 必须共享同一组稳定 provider token，未知值 fail-closed。
///
/// Code Logic（这个枚举做什么）:
///     三种内置 provider；`as_str`/`parse` 与 wire 字面量双向转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentProviderId {
    /// 可见 Claude Code 终端 Runner
    ClaudeCodeVisible,
    /// 可见 Codex 终端 Runner
    CodexVisible,
    /// 受控 generic terminal（owner allowlist）
    GenericTerminal,
}

impl AgentProviderId {
    /// 返回 wire 稳定字符串。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite / P2P / 前端 DTO 都依赖固定 camelCase token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射枚举到 `claudeCodeVisible|codexVisible|genericTerminal`。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCodeVisible => "claudeCodeVisible",
            Self::CodexVisible => "codexVisible",
            Self::GenericTerminal => "genericTerminal",
        }
    }

    /// 解析 wire provider；未知值 fail-closed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     WORKFLOW 与 task 覆盖不能静默回退到 Claude。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim 后精确匹配三个内置 token；否则返回带 token 的业务错误。
    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim() {
            "claudeCodeVisible" => Ok(Self::ClaudeCodeVisible),
            "codexVisible" => Ok(Self::CodexVisible),
            "genericTerminal" => Ok(Self::GenericTerminal),
            other => Err(AppError::generic(format!(
                "runner.provider 不支持: {other}（仅允许 claudeCodeVisible|codexVisible|genericTerminal）"
            ))),
        }
    }

    /// 旧 NULL / 空白映射为 Claude（兼容历史任务）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     升级前任务没有 provider 列或为 NULL，必须安全映射到 Claude 而非 fail。
    ///
    /// Code Logic（这个函数做什么）:
    ///     None/空白 → Claude；否则走 `parse`。
    pub fn parse_legacy(value: Option<&str>) -> Result<Self, AppError> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Self::ClaudeCodeVisible),
            Some(raw) => Self::parse(raw),
        }
    }

    /// provider 默认 completion 合同。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     创建 attempt 快照时若无显式覆盖，需按 provider 选择安全默认完成语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     Claude → SentinelLine；Codex → SentinelLine（无 Hook 桥接时 fail-closed，
    ///     禁止默认 HookEvent 导致永远等不到 Completed）；Generic → Manual。
    pub fn default_completion_contract(self) -> AgentCompletionContract {
        match self {
            Self::ClaudeCodeVisible => AgentCompletionContract::SentinelLine,
            // 未安装 cc-partner OSC Hook 桥接前 Codex 不得默认 HookEvent。
            Self::CodexVisible => AgentCompletionContract::SentinelLine,
            Self::GenericTerminal => AgentCompletionContract::Manual,
        }
    }

    /// 是否为 Claude provider（legacy dual-write / downgrade 守卫用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     旧 peer 降级与 Claude-only 兼容路径需要快速判断。
    ///
    /// Code Logic（这个函数做什么）:
    ///     仅 `ClaudeCodeVisible` 返回 true。
    pub fn is_claude(self) -> bool {
        matches!(self, Self::ClaudeCodeVisible)
    }
}

impl std::fmt::Display for AgentProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Agent 完成判定合同。
///
/// Business Logic（为什么需要这个枚举）:
///     不同 provider 的完成信号不同：Claude 哨兵、Codex Hook、generic 人工结束，不能混用。
///
/// Code Logic（这个枚举做什么）:
///     `sentinelLine|hookEvent|manual` 三态；parse/as_str 双向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentCompletionContract {
    /// 独立一行 DEV_DONE 哨兵
    SentinelLine,
    /// provider Hook/OSC 结构化完成
    HookEvent,
    /// 用户在权威 task detail 明确结束
    Manual,
}

impl AgentCompletionContract {
    /// 返回 wire 稳定字符串。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     attempt 快照列需要稳定 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     映射到 camelCase 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SentinelLine => "sentinelLine",
            Self::HookEvent => "hookEvent",
            Self::Manual => "manual",
        }
    }

    /// 解析 completion 合同；未知 fail-closed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     损坏或未知合同不能默认成任意完成语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     trim 匹配三态；否则业务错误。
    pub fn parse(value: &str) -> Result<Self, AppError> {
        match value.trim() {
            "sentinelLine" => Ok(Self::SentinelLine),
            "hookEvent" => Ok(Self::HookEvent),
            "manual" => Ok(Self::Manual),
            other => Err(AppError::generic(format!(
                "completion_contract 不支持: {other}"
            ))),
        }
    }

    /// 旧 NULL 映射为 Claude 默认 SentinelLine。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     历史 attempt 无 completion 列时按 Claude 哨兵语义恢复。
    ///
    /// Code Logic（这个函数做什么）:
    ///     None/空白 → SentinelLine；否则 `parse`。
    pub fn parse_legacy(value: Option<&str>) -> Result<Self, AppError> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Self::SentinelLine),
            Some(raw) => Self::parse(raw),
        }
    }
}

impl std::fmt::Display for AgentCompletionContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单次 attempt 创建时冻结的 Runner 策略快照。
///
/// Business Logic（为什么需要这个结构体）:
///     claim/attempt 创建后 WORKFLOW 改动不得漂移已运行任务的 provider/限额。
///
/// Code Logic（这个结构体做什么）:
///     聚合 provider、max_turns、stall_timeout_ms、completion_contract。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerAttemptPolicy {
    pub provider: AgentProviderId,
    pub max_turns: i64,
    pub stall_timeout_ms: i64,
    pub completion_contract: AgentCompletionContract,
}

impl RunnerAttemptPolicy {
    /// 构造并校验边界后的 policy。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     所有写入 attempt 的路径必须拒绝越界 max_turns / stall_timeout。
    ///
    /// Code Logic（这个函数做什么）:
    ///     校验 1–20 与 30s–30min；completion 缺省由 provider 决定时可在外部先选好。
    pub fn new(
        provider: AgentProviderId,
        max_turns: i64,
        stall_timeout_ms: i64,
        completion_contract: AgentCompletionContract,
    ) -> Result<Self, AppError> {
        validate_max_turns(max_turns)?;
        validate_stall_timeout_ms(stall_timeout_ms)?;
        Ok(Self {
            provider,
            max_turns,
            stall_timeout_ms,
            completion_contract,
        })
    }

    /// Claude 默认 policy（旧行映射）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     NULL 历史行需要稳定默认：Claude / max_turns=1 / 300s / SentinelLine。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回已校验的默认 Claude policy。
    pub fn claude_default() -> Self {
        Self {
            provider: AgentProviderId::ClaudeCodeVisible,
            max_turns: DEFAULT_MAX_TURNS,
            stall_timeout_ms: DEFAULT_STALL_TIMEOUT_MS,
            completion_contract: AgentCompletionContract::SentinelLine,
        }
    }

    /// 从 workflow runner 字段派生 policy。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     ResolvedWorkflow.runner 是 claim 时策略真值来源之一。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析 provider 字符串，completion 取 provider 默认。
    pub fn from_runner_fields(
        provider: &str,
        max_turns: i64,
        stall_timeout_ms: i64,
    ) -> Result<Self, AppError> {
        let provider = AgentProviderId::parse(provider)?;
        Self::new(
            provider,
            max_turns,
            stall_timeout_ms,
            provider.default_completion_contract(),
        )
    }
}

/// 校验 max_turns ∈ 1..=20。
///
/// Business Logic（为什么需要这个函数）:
///     development attempt 总数是安全与成本边界，parser 与运行时共用同一范围。
///
/// Code Logic（这个函数做什么）:
///     越界返回中文业务错误。
pub fn validate_max_turns(max_turns: i64) -> Result<(), AppError> {
    if !(MIN_MAX_TURNS..=MAX_MAX_TURNS).contains(&max_turns) {
        return Err(AppError::generic(format!(
            "runner.max_turns 必须在 {MIN_MAX_TURNS}..={MAX_MAX_TURNS}，收到 {max_turns}"
        )));
    }
    Ok(())
}

/// 校验 stall_timeout_ms ∈ 30000..=1800000。
///
/// Business Logic（为什么需要这个函数）:
///     stall watchdog 依赖合理超时区间，禁止极小/极大悬挂。
///
/// Code Logic（这个函数做什么）:
///     越界返回含实际值的业务错误。
pub fn validate_stall_timeout_ms(stall_timeout_ms: i64) -> Result<(), AppError> {
    if !(MIN_STALL_TIMEOUT_MS..=MAX_STALL_TIMEOUT_MS).contains(&stall_timeout_ms) {
        return Err(AppError::generic(format!(
            "runner.stall_timeout_ms 必须在 {MIN_STALL_TIMEOUT_MS}..={MAX_STALL_TIMEOUT_MS}，收到 {stall_timeout_ms}"
        )));
    }
    Ok(())
}

/// 解析任务级 Runner policy：task/candidate 覆盖优先，否则用已解析 workflow 字段。
///
/// Business Logic（为什么需要这个函数）:
///     claim 与 attempt 创建前需要冻结策略；覆盖项只能改允许字段且 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     以 base_* 为底，可选覆盖 provider/max_turns/stall_timeout_ms；completion 跟 provider 默认。
pub fn resolve_task_runner_policy(
    base_provider: &str,
    base_max_turns: i64,
    base_stall_timeout_ms: i64,
    override_provider: Option<&str>,
    override_max_turns: Option<i64>,
    override_stall_timeout_ms: Option<i64>,
) -> Result<RunnerAttemptPolicy, AppError> {
    let provider = match override_provider {
        Some(raw) => AgentProviderId::parse(raw)?,
        None => AgentProviderId::parse(base_provider)?,
    };
    let max_turns = override_max_turns.unwrap_or(base_max_turns);
    let stall_timeout_ms = override_stall_timeout_ms.unwrap_or(base_stall_timeout_ms);
    RunnerAttemptPolicy::new(
        provider,
        max_turns,
        stall_timeout_ms,
        provider.default_completion_contract(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     三个内置 provider 必须 round-trip，否则 WORKFLOW 与 adapter 会分叉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对每个 wire 值 parse → as_str 再比对。
    #[test]
    fn provider_wire_roundtrip() {
        for value in ["claudeCodeVisible", "codexVisible", "genericTerminal"] {
            let id = AgentProviderId::parse(value).unwrap();
            assert_eq!(id.as_str(), value);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未知 provider 必须 fail-closed，禁止静默 Claude。
    ///
    /// Code Logic（这个测试做什么）:
    ///     parse("gemini") 断言错误。
    #[test]
    fn unknown_provider_fails_closed() {
        let err = AgentProviderId::parse("gemini").unwrap_err();
        assert!(err.to_string().contains("gemini"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     max_turns 边界是安全合同，0/21 必须拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     validate_max_turns(0) 与 (21) 失败；(1)(20) 成功。
    #[test]
    fn max_turns_bounds() {
        assert!(validate_max_turns(0).is_err());
        assert!(validate_max_turns(21).is_err());
        assert!(validate_max_turns(1).is_ok());
        assert!(validate_max_turns(20).is_ok());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     stall timeout 边界 29999/1800001 必须拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     边界外失败、边界内成功。
    #[test]
    fn stall_timeout_bounds() {
        assert!(validate_stall_timeout_ms(29_999).is_err());
        assert!(validate_stall_timeout_ms(1_800_001).is_err());
        assert!(validate_stall_timeout_ms(30_000).is_ok());
        assert!(validate_stall_timeout_ms(1_800_000).is_ok());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     task 覆盖 provider 必须优先于 workflow，且 completion 跟随新 provider。
    ///
    /// Code Logic（这个测试做什么）:
    ///     workflow=Claude，override=codex → Codex + SentinelLine（无 Hook 桥接）。
    #[test]
    fn resolve_task_runner_policy_prefers_override() {
        let policy = resolve_task_runner_policy(
            "claudeCodeVisible",
            3,
            120_000,
            Some("codexVisible"),
            Some(4),
            None,
        )
        .unwrap();
        assert_eq!(policy.provider, AgentProviderId::CodexVisible);
        assert_eq!(policy.max_turns, 4);
        assert_eq!(policy.stall_timeout_ms, 120_000);
        assert_eq!(
            policy.completion_contract,
            AgentCompletionContract::SentinelLine
        );
    }
}
