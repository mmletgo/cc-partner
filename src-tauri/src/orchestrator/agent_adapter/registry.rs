//! Agent adapter trait 与 owner-local registry。
//!
//! Business Logic（为什么需要这个模块）:
//!     Runner 只能通过 registry 解析 adapter 并执行 launch/resume plan，不能硬编码 Claude 命令。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AgentAdapter` 合同、`AgentLaunchRequest/Plan`、probe 结果与 `AgentAdapterRegistry`。

use super::claude_code::ClaudeCodeAdapter;
use super::codex::CodexAdapter;
use super::generic_terminal::{GenericTerminalAdapter, GenericTerminalConfig};
use super::types::{AgentCompletionContract, AgentProviderId};
use crate::error::AppError;
use crate::workbench::agent_runtime::{AgentRuntimeMutation, AgentSessionPhase};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// probe 结果缓存 TTL（60 秒）。
const PROBE_CACHE_TTL: Duration = Duration::from_secs(60);

/// Agent 可执行性。
///
/// Business Logic（为什么需要这个枚举）:
///     catalog / claim 前需要知道 provider 是否可在 owner 本机使用。
///
/// Code Logic（这个枚举做什么）:
///     Available / Unavailable 二态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAvailability {
    Available,
    Unavailable,
}

/// owner-local probe 结果（可含 executable，不得进 P2P DTO）。
///
/// Business Logic（为什么需要这个结构体）:
///     adapter 需要本地定位 CLI 与版本；远端只看 available/reason。
///
/// Code Logic（这个结构体做什么）:
///     聚合 provider、availability、可选 executable/version/reason_code。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProbeResult {
    pub provider_id: AgentProviderId,
    pub availability: AgentAvailability,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub reason_code: Option<String>,
}

/// 启动/恢复 Agent 的请求（Runner 组装）。
///
/// Business Logic（为什么需要这个结构体）:
///     adapter 只需 cwd/prompt/session 关联，不拥有 worktree 创建权。
///
/// Code Logic（这个结构体做什么）:
///     承载 launch/resume 所需字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunchRequest {
    pub agent_session_id: String,
    pub terminal_session_id: String,
    pub cwd: String,
    pub prompt: String,
    pub native_session_id: Option<String>,
    pub max_turns: u32,
    pub stall_timeout_ms: u64,
}

/// adapter 产出的可见终端启动计划（无 shell 拼接）。
///
/// Business Logic（为什么需要这个结构体）:
///     Runner 只执行 plan，不解释 provider 特有 flag。
///
/// Code Logic（这个结构体做什么）:
///     executable + args + 可选 stdin + env + completion 合同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLaunchPlan {
    pub executable: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub env: Vec<(String, String)>,
    pub completion: AgentCompletionContract,
}

impl AgentLaunchPlan {
    /// 渲染为可见终端写入文本（无 shell）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Workbench terminal 接收键入流，需要把 plan 转成等价于历史 Claude 注入的输入。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `executable [args...]\n` + 可选 stdin；args 以空格连接（adapter 保证无 metachar）。
    pub fn to_terminal_input(&self) -> String {
        let mut line = self.executable.clone();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line.push('\n');
        if let Some(stdin) = &self.stdin {
            line.push_str(stdin);
        }
        line
    }
}

/// provider-native 入站事件（OSC/Hook 归一前）。
///
/// Business Logic（为什么需要这个结构体）:
///     不同 CLI 的原始事件形状不同，adapter 负责归一到 A1 mutation。
///
/// Code Logic（这个结构体做什么）:
///     最小公共字段：phase 提示、native session、usage、payload JSON。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAgentEvent {
    pub agent_session_id: String,
    pub terminal_session_id: String,
    pub expected_version: u64,
    pub event_version: u64,
    pub phase: AgentSessionPhase,
    pub native_session_id: Option<String>,
    pub outcome_code: Option<String>,
    pub occurred_at: String,
    pub usage_input_tokens: Option<u64>,
    pub usage_output_tokens: Option<u64>,
    pub raw_kind: Option<String>,
}

/// 可靠 cumulative usage 快照（可选，不进 P2P；仅 structured adapter 字段）。
///
/// Business Logic（为什么需要这个结构体）:
///     provider 可上报 token/cost 用量供 owner-local Ledger；unknown 保持 None，禁止估算。
///
/// Code Logic（这个结构体做什么）:
///     可选 model/token/cost 字段；cost_major 为主单位十进制字符串。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentUsageDelta {
    pub model_id: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    /// provider 主单位金额字符串（如 "0.0123"）
    pub cost_major: Option<String>,
    /// ISO 4217 三字符大写
    pub cost_currency: Option<String>,
}

impl AgentUsageDelta {
    /// 是否含任何可靠字段。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     全空快照无需写入 Ledger cache。
    ///
    /// Code Logic（这个函数做什么）:
    ///     任一字段 Some 即 true。
    pub fn has_any(&self) -> bool {
        self.model_id.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_read_tokens.is_some()
            || self.cache_write_tokens.is_some()
            || self.cost_major.is_some()
            || self.cost_currency.is_some()
    }
}

/// Agent adapter 合同。
///
/// Business Logic（为什么需要这个 trait）:
///     Claude/Codex/generic 共享 probe/launch/resume/normalize/usage/interrupt 边界。
///
/// Code Logic（这个 trait 做什么）:
///     全部方法 Send+Sync；normalize 返回 A1 `AgentRuntimeMutation`。
pub trait AgentAdapter: Send + Sync {
    /// 返回内置 provider id。
    fn provider_id(&self) -> AgentProviderId;

    /// owner-local probe（可执行文件/版本）。
    fn probe(&self) -> Result<AgentProbeResult, AppError>;

    /// 首次 launch plan。
    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError>;

    /// resume plan；不支持时返回业务错误。
    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError>;

    /// 将 native 事件归一为 A1 mutation。
    fn normalize_runtime_event(
        &self,
        event: NativeAgentEvent,
    ) -> Result<AgentRuntimeMutation, AppError>;

    /// 从事件提取 usage。
    fn extract_usage(&self, event: &NativeAgentEvent) -> Option<AgentUsageDelta>;

    /// 中断输入（如 Ctrl-C 字符）。
    fn interrupt_input(&self) -> &'static str;

    /// 是否支持 resume。
    fn supports_resume(&self) -> bool {
        false
    }

    /// 是否支持 usage 提取。
    fn supports_usage(&self) -> bool {
        false
    }

    /// 默认 completion 合同。
    fn completion_contract(&self) -> AgentCompletionContract {
        self.provider_id().default_completion_contract()
    }
}

/// probe 缓存条目。
struct ProbeCacheEntry {
    result: AgentProbeResult,
    cached_at: Instant,
}

/// owner-local adapter registry。
///
/// Business Logic（为什么需要这个结构体）:
///     Runner / catalog / watchdog 需要按 provider 解析同一组内置 adapter。
///
/// Code Logic（这个结构体做什么）:
///     持有三个内置 adapter；probe 结果缓存 60s。
pub struct AgentAdapterRegistry {
    adapters: HashMap<AgentProviderId, Arc<dyn AgentAdapter>>,
    probe_cache: Mutex<HashMap<AgentProviderId, ProbeCacheEntry>>,
}

impl AgentAdapterRegistry {
    /// 构造默认内置 registry（无 generic allowlist）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     大多数路径只需 Claude+Codex；generic 在无 allowlist 时 probe 为 Unavailable。
    ///
    /// Code Logic（这个函数做什么）:
    ///     注册 Claude/Codex/Generic(None)。
    pub fn with_defaults() -> Self {
        Self::new(None)
    }

    /// 构造带 optional generic 配置的 registry。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     owner 可在本地 config 启用受控 generic terminal。
    ///
    /// Code Logic（这个函数做什么）:
    ///     注册三个 adapter。
    pub fn new(generic: Option<GenericTerminalConfig>) -> Self {
        let mut adapters: HashMap<AgentProviderId, Arc<dyn AgentAdapter>> = HashMap::new();
        adapters.insert(
            AgentProviderId::ClaudeCodeVisible,
            Arc::new(ClaudeCodeAdapter),
        );
        adapters.insert(AgentProviderId::CodexVisible, Arc::new(CodexAdapter));
        adapters.insert(
            AgentProviderId::GenericTerminal,
            Arc::new(GenericTerminalAdapter::new(generic)),
        );
        Self {
            adapters,
            probe_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 按 provider 解析 adapter；未知 fail-closed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Runner 只能启动内置 provider。
    ///
    /// Code Logic（这个函数做什么）:
    ///     HashMap get；缺失返回业务错误。
    pub fn get(&self, provider: AgentProviderId) -> Result<Arc<dyn AgentAdapter>, AppError> {
        self.adapters.get(&provider).cloned().ok_or_else(|| {
            AppError::generic(format!("未注册 Agent adapter: {}", provider.as_str()))
        })
    }

    /// 带 60s 缓存的 probe。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     catalog/UI 频繁查询不能每次 spawn --version。
    ///
    /// Code Logic（这个函数做什么）:
    ///     缓存命中且未过期则返回克隆；否则调用 adapter.probe 并写入缓存。
    pub fn probe_cached(&self, provider: AgentProviderId) -> Result<AgentProbeResult, AppError> {
        {
            let cache = self
                .probe_cache
                .lock()
                .map_err(|_| AppError::generic("adapter probe 缓存锁损坏"))?;
            if let Some(entry) = cache.get(&provider) {
                if entry.cached_at.elapsed() < PROBE_CACHE_TTL {
                    return Ok(entry.result.clone());
                }
            }
        }
        let adapter = self.get(provider)?;
        let result = adapter.probe()?;
        let mut cache = self
            .probe_cache
            .lock()
            .map_err(|_| AppError::generic("adapter probe 缓存锁损坏"))?;
        cache.insert(
            provider,
            ProbeCacheEntry {
                result: result.clone(),
                cached_at: Instant::now(),
            },
        );
        Ok(result)
    }

    /// 列出全部内置 adapter 的 probe 摘要（含本地 path，调用方负责 redact）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Settings catalog 需要三 provider 状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按固定顺序 probe_cached。
    pub fn list_probes(&self) -> Result<Vec<AgentProbeResult>, AppError> {
        let order = [
            AgentProviderId::ClaudeCodeVisible,
            AgentProviderId::CodexVisible,
            AgentProviderId::GenericTerminal,
        ];
        order.into_iter().map(|id| self.probe_cached(id)).collect()
    }

    /// 从 owner AppConfig 构造带 generic allowlist 的 registry。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Runner / watchdog / bridge 必须与 catalog 使用同一 generic 配置，禁止 with_defaults 空 allowlist。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读 `config.orchestrator.generic_terminal`（锁失败则 None）后 `new`。
    pub fn from_app_config(config: &crate::config::AppConfig) -> Self {
        Self::new(config.orchestrator.generic_terminal.clone())
    }

    /// 使 probe 缓存失效。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     executable mtime 变化或用户改 generic allowlist 后需立即刷新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     清空缓存 map。
    pub fn invalidate_probe_cache(&self) {
        if let Ok(mut cache) = self.probe_cache.lock() {
            cache.clear();
        }
    }
}

impl Default for AgentAdapterRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     registry 必须注册三个内置 provider。
    ///
    /// Code Logic（这个测试做什么）:
    ///     get 三个 id 成功。
    #[test]
    fn registry_registers_built_in_providers() {
        let reg = AgentAdapterRegistry::with_defaults();
        for id in [
            AgentProviderId::ClaudeCodeVisible,
            AgentProviderId::CodexVisible,
            AgentProviderId::GenericTerminal,
        ] {
            assert_eq!(reg.get(id).unwrap().provider_id(), id);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     terminal input 渲染必须复现 Claude `claude\nprompt\n` 形态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     plan.executable=claude stdin=prompt\n → to_terminal_input。
    #[test]
    fn launch_plan_terminal_input_matches_claude_visible_shape() {
        let plan = AgentLaunchPlan {
            executable: "claude".into(),
            args: vec![],
            stdin: Some("fix tests\n".into()),
            env: vec![],
            completion: AgentCompletionContract::SentinelLine,
        };
        assert_eq!(plan.to_terminal_input(), "claude\nfix tests\n");
    }
}
