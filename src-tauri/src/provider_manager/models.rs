//! provider_manager/models.rs — Provider Manager DTO 与 AgentApp 枚举。
//!
//! Business Logic（为什么需要这个模块）:
//!     Provider Manager 只做"读 cc-switch 已配置的 provider + 切换当前 provider"，
//!     不编辑 provider 详情。本模块定义 IPC 边界的 camelCase DTO 与受支持的 agent 枚举，
//!     对齐 cc-switch-cli 的 `--app` 目标集合。
//!
//! Code Logic（这个模块做什么）:
//!     `AgentApp` 是受支持 agent 的唯一来源（`--app` flag / DB app_type / settings.json
//!     `currentProvider<Pascal>` 键三者的映射）。其余结构体仅供 IPC 返回，绝不泄露
//!     `settings_config`（含 API key）等敏感字段。

/// 受 cc-switch-cli 支持的 agent（`--app` 目标集合）。
///
/// Business Logic: `claude-desktop` 不是 CLI `--app` 目标，故排除。
/// serde 小写形式同时用于 IPC 入参（前端 `provider_manager_switch({app})`）与
/// DB `app_type` 字符串，确保一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentApp {
    Claude,
    Codex,
    Gemini,
    Opencode,
    Hermes,
    Openclaw,
}

impl AgentApp {
    /// `--app` flag 值 / DB `app_type` 字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            AgentApp::Claude => "claude",
            AgentApp::Codex => "codex",
            AgentApp::Gemini => "gemini",
            AgentApp::Opencode => "opencode",
            AgentApp::Hermes => "hermes",
            AgentApp::Openclaw => "openclaw",
        }
    }

    /// 由 DB `app_type` 字符串解析为 `AgentApp`，不支持的（如 `claude-desktop`）返回 `None`。
    pub fn from_app_type(s: &str) -> Option<AgentApp> {
        match s {
            "claude" => Some(AgentApp::Claude),
            "codex" => Some(AgentApp::Codex),
            "gemini" => Some(AgentApp::Gemini),
            "opencode" => Some(AgentApp::Opencode),
            "hermes" => Some(AgentApp::Hermes),
            "openclaw" => Some(AgentApp::Openclaw),
            _ => None,
        }
    }

    /// cc-switch `~/.cc-switch/settings.json` 中的当前 provider 键
    /// `currentProvider<Pascal>`（优先级高于 DB `is_current`）。
    pub fn settings_current_key(self) -> String {
        let pascal = match self {
            AgentApp::Claude => "Claude",
            AgentApp::Codex => "Codex",
            AgentApp::Gemini => "Gemini",
            AgentApp::Opencode => "Opencode",
            AgentApp::Hermes => "Hermes",
            AgentApp::Openclaw => "Openclaw",
        };
        format!("currentProvider{pascal}")
    }

    /// 受支持 agent 的展示顺序。
    pub fn all() -> &'static [AgentApp] {
        &[
            AgentApp::Claude,
            AgentApp::Codex,
            AgentApp::Gemini,
            AgentApp::Opencode,
            AgentApp::Hermes,
            AgentApp::Openclaw,
        ]
    }
}

/// cc-switch CLI 检测结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    /// 是否检测到一个"行为像 CLI"的 cc-switch 可执行文件。
    pub available: bool,
    /// 解析到的绝对路径（按绝对路径调用，避免 PATH 歧义）。
    pub path: Option<String>,
    /// `cc-switch --version` 输出。
    pub version: Option<String>,
}

/// cc-switch GUI 检测结果（best-effort，只读，从不启动或修改 GUI）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchGuiStatus {
    pub installed: bool,
    pub version: Option<String>,
    /// v1 不检测运行态（避免每次轮询都跑 `ps`）；`None` 表示未知。
    pub running: Option<bool>,
    /// CLI 与 GUI 主版本不一致时为 `Some(true)`，用于提示"对齐版本以免触发 GUI 看不懂的迁移"。
    pub version_mismatch: Option<bool>,
}

/// 单个 provider 摘要（不含 `settings_config`/API key 等敏感字段）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub is_current: bool,
}

/// 某 agent 下全部 provider 及其当前 provider id。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProviders {
    pub app: AgentApp,
    pub providers: Vec<ProviderEntry>,
    pub current_provider_id: Option<String>,
}

/// Provider Manager 页面整体状态快照。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderManagerSummary {
    pub cc_switch_db_present: bool,
    pub cli: CliStatus,
    /// 非 macOS 平台 v1 不检测 GUI（返回 `None`，前端按未知处理）。
    pub gui: Option<CcSwitchGuiStatus>,
    pub apps: Vec<AppProviders>,
}

/// 安装 cc-switch CLI 的结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    /// `"brew"`（已执行安装）或 `"manual"`（仅返回人工指引，不自行 curl|bash）。
    pub method: String,
    pub ok: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub message: Option<String>,
    pub url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_app_string_round_trip() {
        for &app in AgentApp::all() {
            assert_eq!(AgentApp::from_app_type(app.as_str()), Some(app));
            assert!(app.settings_current_key().starts_with("currentProvider"));
        }
    }

    #[test]
    fn claude_desktop_is_excluded() {
        // cc-switch-desktop 不是 CLI --app 目标，必须被过滤掉。
        assert_eq!(AgentApp::from_app_type("claude-desktop"), None);
        assert_eq!(AgentApp::from_app_type("nonsense"), None);
    }

    #[test]
    fn settings_current_key_uses_pascal_case() {
        assert_eq!(
            AgentApp::Claude.settings_current_key(),
            "currentProviderClaude"
        );
        assert_eq!(
            AgentApp::Codex.settings_current_key(),
            "currentProviderCodex"
        );
        assert_eq!(
            AgentApp::Openclaw.settings_current_key(),
            "currentProviderOpenclaw"
        );
    }

    #[test]
    fn supported_app_count_is_six() {
        assert_eq!(AgentApp::all().len(), 6);
    }
}
