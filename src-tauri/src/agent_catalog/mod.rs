//! agent_catalog — 多 CLI Agent 身份目录
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub / Runtime / 会话搜索 / Prompt 历史 / 用量 / headless 各自曾写死
//!     Claude/Codex/OpenCode 三元组。接入 Grok Build 与 Gemini CLI 时必须共用
//!     一份身份表，禁止再按功能面复制枚举。
//!
//! Code Logic（这个模块做什么）:
//!     定义 `AgentId` 与编译期登记表；把 Hub/Runtime/Session/History/Headless
//!     投影做成可缺省查询。未知 token fail-closed。

use crate::agent_hub::models::AgentTarget;
use crate::orchestrator::agent_adapter::types::AgentProviderId;

/// 产品级 Agent 身份（稳定小写 token）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentId {
    /// Claude Code
    Claude,
    /// Codex CLI
    Codex,
    /// OpenCode CLI
    OpenCode,
    /// Grok Build（可执行 `grok`）
    Grok,
    /// Gemini CLI（可执行 `gemini`）
    Gemini,
    /// Cursor CLI（可执行 `cursor-agent`，兼容 `agent`）
    Cursor,
    /// Pi Coding Agent（可执行 `pi`）
    Pi,
}

/// 一条身份登记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentIdentity {
    /// 产品身份
    pub id: AgentId,
    /// 稳定 wire
    pub wire: &'static str,
    /// UI 显示名
    pub display_name: &'static str,
    /// Hub target；None = 不进 Agent Hub
    pub hub_target: Option<AgentTarget>,
    /// Runtime provider；None = 不进编排器/可见 Runner
    pub runtime_provider: Option<AgentProviderId>,
    /// 会话搜索 / 自动标题 source
    pub session_source: Option<&'static str>,
    /// Prompt 历史 source
    pub history_source: Option<&'static str>,
    /// 是否登记 UsageSource
    pub has_usage: bool,
    /// 是否登记 HeadlessCompletion（优化器可选）
    pub has_headless: bool,
    /// probe 可执行名
    pub executable_names: &'static [&'static str],
}

const IDENTITIES: &[AgentIdentity] = &[
    AgentIdentity {
        id: AgentId::Claude,
        wire: "claude",
        display_name: "Claude Code",
        hub_target: Some(AgentTarget::Claude),
        runtime_provider: Some(AgentProviderId::ClaudeCodeVisible),
        session_source: Some("claude"),
        history_source: Some("claude"),
        has_usage: true,
        has_headless: true,
        executable_names: &["claude"],
    },
    AgentIdentity {
        id: AgentId::Codex,
        wire: "codex",
        display_name: "Codex",
        hub_target: Some(AgentTarget::Codex),
        runtime_provider: Some(AgentProviderId::CodexVisible),
        session_source: Some("codex"),
        history_source: Some("codex"),
        has_usage: true,
        has_headless: false,
        executable_names: &["codex"],
    },
    AgentIdentity {
        id: AgentId::OpenCode,
        wire: "opencode",
        display_name: "OpenCode",
        hub_target: Some(AgentTarget::OpenCode),
        runtime_provider: Some(AgentProviderId::OpenCodeVisible),
        session_source: Some("opencode"),
        history_source: Some("opencode"),
        has_usage: true,
        has_headless: false,
        executable_names: &["opencode"],
    },
    AgentIdentity {
        id: AgentId::Grok,
        wire: "grok",
        display_name: "Grok Build",
        hub_target: Some(AgentTarget::Grok),
        runtime_provider: Some(AgentProviderId::GrokBuildVisible),
        session_source: Some("grok"),
        history_source: Some("grok"),
        has_usage: true,
        has_headless: true,
        executable_names: &["grok"],
    },
    AgentIdentity {
        id: AgentId::Gemini,
        wire: "gemini",
        display_name: "Gemini CLI",
        hub_target: Some(AgentTarget::Gemini),
        runtime_provider: Some(AgentProviderId::GeminiCliVisible),
        session_source: Some("gemini"),
        history_source: Some("gemini"),
        has_usage: true,
        has_headless: true,
        executable_names: &["gemini"],
    },
    AgentIdentity {
        id: AgentId::Cursor,
        wire: "cursor",
        display_name: "Cursor CLI",
        hub_target: Some(AgentTarget::Cursor),
        runtime_provider: Some(AgentProviderId::CursorCliVisible),
        session_source: Some("cursor"),
        history_source: Some("cursor"),
        has_usage: true,
        has_headless: true,
        executable_names: &["cursor-agent", "agent"],
    },
    AgentIdentity {
        id: AgentId::Pi,
        wire: "pi",
        display_name: "Pi",
        hub_target: Some(AgentTarget::Pi),
        runtime_provider: Some(AgentProviderId::PiVisible),
        session_source: Some("pi"),
        history_source: Some("pi"),
        has_usage: true,
        has_headless: true,
        executable_names: &["pi"],
    },
];

impl AgentId {
    /// 稳定 wire token。
    pub fn as_str(self) -> &'static str {
        self.identity().wire
    }

    /// 解析 wire；未知返回 None。
    pub fn parse(raw: &str) -> Option<Self> {
        identity_by_wire(raw.trim()).map(|row| row.id)
    }

    /// 该身份的登记行。
    pub fn identity(self) -> &'static AgentIdentity {
        IDENTITIES
            .iter()
            .find(|row| row.id == self)
            .expect("每个 AgentId 必须登记")
    }
}

/// 全部已登记产品身份（不含 genericTerminal）。
pub fn all_identities() -> &'static [AgentIdentity] {
    IDENTITIES
}

/// 全部 Hub target。
pub fn all_hub_targets() -> impl Iterator<Item = AgentTarget> {
    IDENTITIES.iter().filter_map(|row| row.hub_target)
}

/// 按 wire 查登记。
pub fn identity_by_wire(wire: &str) -> Option<&'static AgentIdentity> {
    IDENTITIES.iter().find(|row| row.wire == wire)
}

/// 按 Hub target 反查。
pub fn identity_by_hub_target(target: AgentTarget) -> Option<&'static AgentIdentity> {
    IDENTITIES.iter().find(|row| row.hub_target == Some(target))
}

/// 按 Runtime provider 反查（genericTerminal 返回 None）。
pub fn identity_by_runtime(provider: AgentProviderId) -> Option<&'static AgentIdentity> {
    IDENTITIES
        .iter()
        .find(|row| row.runtime_provider == Some(provider))
}

/// 是否为已登记 session source。
pub fn is_session_source(raw: &str) -> bool {
    IDENTITIES.iter().any(|row| row.session_source == Some(raw))
}

/// 是否为已登记 history source。
pub fn is_history_source(raw: &str) -> bool {
    IDENTITIES.iter().any(|row| row.history_source == Some(raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_registers_seven_agent_ids() {
        assert_eq!(all_identities().len(), 7);
        for wire in [
            "claude", "codex", "opencode", "grok", "gemini", "cursor", "pi",
        ] {
            assert!(AgentId::parse(wire).is_some(), "{wire}");
        }
    }

    #[test]
    fn unknown_agent_id_fails_closed() {
        assert!(AgentId::parse("antigravity").is_none());
        assert!(AgentId::parse("genericTerminal").is_none());
        assert!(identity_by_wire("").is_none());
    }

    #[test]
    fn generic_terminal_has_no_hub_or_product_id() {
        assert!(identity_by_runtime(AgentProviderId::GenericTerminal).is_none());
        assert_eq!(all_hub_targets().count(), 7);
    }

    #[test]
    fn grok_and_gemini_project_to_all_surfaces() {
        let grok = AgentId::Grok.identity();
        assert_eq!(grok.hub_target, Some(AgentTarget::Grok));
        assert_eq!(
            grok.runtime_provider,
            Some(AgentProviderId::GrokBuildVisible)
        );
        assert_eq!(grok.session_source, Some("grok"));
        assert_eq!(grok.history_source, Some("grok"));
        assert!(grok.has_usage);
        assert!(grok.has_headless);
        assert_eq!(grok.executable_names, &["grok"]);

        let gemini = AgentId::Gemini.identity();
        assert_eq!(gemini.hub_target, Some(AgentTarget::Gemini));
        assert_eq!(
            gemini.runtime_provider,
            Some(AgentProviderId::GeminiCliVisible)
        );
        assert_eq!(gemini.session_source, Some("gemini"));

        let cursor = AgentId::Cursor.identity();
        assert_eq!(cursor.hub_target, Some(AgentTarget::Cursor));
        assert_eq!(
            cursor.runtime_provider,
            Some(AgentProviderId::CursorCliVisible)
        );
        assert_eq!(cursor.session_source, Some("cursor"));
        assert_eq!(cursor.history_source, Some("cursor"));
        assert!(cursor.has_usage);
        assert!(cursor.has_headless);
        assert_eq!(cursor.executable_names, &["cursor-agent", "agent"]);

        let pi = AgentId::Pi.identity();
        assert_eq!(pi.hub_target, Some(AgentTarget::Pi));
        assert_eq!(pi.runtime_provider, Some(AgentProviderId::PiVisible));
        assert_eq!(pi.session_source, Some("pi"));
        assert_eq!(pi.history_source, Some("pi"));
        assert!(pi.has_usage);
        assert!(pi.has_headless);
        assert_eq!(pi.executable_names, &["pi"]);
    }

    #[test]
    fn wire_roundtrip() {
        for row in all_identities() {
            assert_eq!(AgentId::parse(row.wire), Some(row.id));
            assert_eq!(row.id.as_str(), row.wire);
        }
    }
}
