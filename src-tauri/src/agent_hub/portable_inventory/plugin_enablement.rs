//! 当前 Agent 的 plugin 启用标记（与所有者磁盘上的开关隔离）。
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude `enabledPlugins`、Codex `[plugins."id@market"]`、Grok `[plugins] enabled/disabled`
//!     是三套独立标记。借用 Claude cache 的 Grok/其它 Agent 不得继承所有者开关；
//!     Codex native 白名单也不得套到借用包。无独立开关的 Agent 目录存在即启用。
//!
//! Code Logic（这个模块做什么）:
//!     只加载 **viewing** target 的配置；`plugin_actual_enabled` 按 AgentTarget 穷尽分发。

use crate::agent_hub::models::AgentTarget;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// 当前 Agent 已加载的 plugin 开关快照。
///
/// Business Logic: 一份快照只描述一个 viewing Agent，禁止混入 owner 配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewingPluginEnablement {
    target: AgentTarget,
    claude: BTreeMap<String, bool>,
    codex: BTreeMap<String, bool>,
    grok: GrokPluginEnablement,
}

/// Grok `config.toml` `[plugins]` 启停表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GrokPluginEnablement {
    enabled: Vec<String>,
    disabled: Vec<String>,
}

impl ViewingPluginEnablement {
    /// 只读当前 Agent 的开关文件。
    ///
    /// Business Logic: Codex 项目 scope 用项目 `.codex/config.toml`；其余用 user 配置根。
    /// Code Logic: `match target` 穷尽；未实现独立开关的 Agent 得到空快照。
    pub(crate) fn load(
        target: AgentTarget,
        claude_config_root: &Path,
        codex_config_root: &Path,
        grok_config_root: &Path,
    ) -> Self {
        match target {
            AgentTarget::Claude => Self {
                target,
                claude: load_claude_plugin_enablement(claude_config_root),
                codex: BTreeMap::new(),
                grok: GrokPluginEnablement::default(),
            },
            AgentTarget::Codex => Self {
                target,
                claude: BTreeMap::new(),
                codex: load_codex_plugin_enablement(codex_config_root),
                grok: GrokPluginEnablement::default(),
            },
            AgentTarget::Grok => Self {
                target,
                claude: BTreeMap::new(),
                codex: BTreeMap::new(),
                grok: load_grok_plugin_enablement(grok_config_root),
            },
            AgentTarget::OpenCode | AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
                Self {
                    target,
                    claude: BTreeMap::new(),
                    codex: BTreeMap::new(),
                    grok: GrokPluginEnablement::default(),
                }
            }
        }
    }

    #[cfg(test)]
    fn empty(target: AgentTarget) -> Self {
        Self {
            target,
            claude: BTreeMap::new(),
            codex: BTreeMap::new(),
            grok: GrokPluginEnablement::default(),
        }
    }
}

/// 解析 package 对**当前 Agent**的启用态。
///
/// Business Logic: native 走该 Agent 自己的安装表；借用项不得套 owner 开关，
///     也不得把 native 白名单当成「没登记就是关」。
/// Code Logic: 穷尽 `AgentTarget`；返回 `(enabled, optional warning)`。
pub(crate) fn plugin_actual_enabled(
    enablement: &ViewingPluginEnablement,
    plugin_id: &str,
    registry_key: Option<&str>,
    native: bool,
) -> (bool, Option<String>) {
    match enablement.target {
        AgentTarget::Claude => {
            claude_plugin_actual_enabled(plugin_id, registry_key, &enablement.claude)
        }
        AgentTarget::Codex => codex_plugin_actual_enabled(plugin_id, native, &enablement.codex),
        AgentTarget::Grok => (
            grok_plugin_actual_enabled(plugin_id, native, &enablement.grok),
            None,
        ),
        AgentTarget::OpenCode | AgentTarget::Gemini | AgentTarget::Cursor | AgentTarget::Pi => {
            (true, None)
        }
    }
}

/// 解析 Codex `config.toml` 中 `[plugins."id@market"] enabled` 映射。
pub(crate) fn parse_codex_plugin_enablement_from_toml(text: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return out;
    };
    let Some(plugins) = doc.get("plugins").and_then(|i| i.as_table()) else {
        return out;
    };
    for (key, item) in plugins.iter() {
        let Some(table) = item.as_table() else {
            continue;
        };
        let enabled = table
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        out.insert(key.to_string(), enabled);
    }
    out
}

fn load_codex_plugin_enablement(config_root: &Path) -> BTreeMap<String, bool> {
    let path = config_root.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(text) => parse_codex_plugin_enablement_from_toml(&text),
        Err(_) => BTreeMap::new(),
    }
}

/// 解析 Claude `settings.json` 的 `enabledPlugins` 映射。
pub(crate) fn parse_claude_plugin_enablement_from_settings(text: &str) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return out;
    };
    let Some(plugins) = value.get("enabledPlugins").and_then(|v| v.as_object()) else {
        return out;
    };
    for (key, item) in plugins {
        if let Some(enabled) = item.as_bool() {
            out.insert(key.clone(), enabled);
        }
    }
    out
}

fn load_claude_plugin_enablement(config_root: &Path) -> BTreeMap<String, bool> {
    let path = config_root.join("settings.json");
    match fs::read_to_string(&path) {
        Ok(text) => parse_claude_plugin_enablement_from_settings(&text),
        Err(_) => BTreeMap::new(),
    }
}

fn claude_plugin_actual_enabled(
    plugin_id: &str,
    registry_key: Option<&str>,
    enablement: &BTreeMap<String, bool>,
) -> (bool, Option<String>) {
    if enablement.is_empty() {
        return (true, None);
    }
    if let Some(key) = registry_key {
        if let Some(v) = enablement.get(key) {
            return (*v, None);
        }
    }
    if let Some(v) = enablement.get(plugin_id) {
        return (*v, None);
    }
    let prefix = format!("{plugin_id}@");
    let mut matched: Option<bool> = None;
    for (key, enabled) in enablement {
        if key == plugin_id || key.starts_with(&prefix) {
            matched = Some(matched.map(|m| m && *enabled).unwrap_or(*enabled));
        }
    }
    match matched {
        Some(v) => (v, None),
        None => (true, None),
    }
}

/// 解析 package 在 Codex config 中的启用态。
///
/// Business Logic: native 表非空且未登记 → false；借用项未登记 → true（不得当白名单）。
fn codex_plugin_actual_enabled(
    plugin_id: &str,
    native: bool,
    enablement: &BTreeMap<String, bool>,
) -> (bool, Option<String>) {
    if enablement.is_empty() {
        return (true, None);
    }
    if let Some(v) = enablement.get(plugin_id) {
        return (*v, None);
    }
    if let Some(v) = lookup_plugin_bool(enablement, plugin_id) {
        return (v, None);
    }
    if native {
        (false, Some("codex_plugin_not_in_config".into()))
    } else {
        (true, None)
    }
}

/// 解析 Grok `[plugins] enabled/disabled` 字符串数组。
pub(crate) fn parse_grok_plugin_enablement_from_toml(text: &str) -> GrokPluginEnablement {
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return GrokPluginEnablement::default();
    };
    let Some(plugins) = doc.get("plugins").and_then(|item| item.as_table()) else {
        return GrokPluginEnablement::default();
    };
    GrokPluginEnablement {
        enabled: grok_plugin_string_array(plugins.get("enabled")),
        disabled: grok_plugin_string_array(plugins.get("disabled")),
    }
}

fn grok_plugin_string_array(item: Option<&toml_edit::Item>) -> Vec<String> {
    item.and_then(|value| value.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn load_grok_plugin_enablement(config_root: &Path) -> GrokPluginEnablement {
    let path = config_root.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(text) => parse_grok_plugin_enablement_from_toml(&text),
        Err(_) => GrokPluginEnablement::default(),
    }
}

fn grok_plugin_list_contains(list: &[String], plugin_id: &str) -> bool {
    let prefix = format!("{plugin_id}@");
    list.iter()
        .any(|key| key == plugin_id || key.starts_with(&prefix))
}

fn grok_plugin_actual_enabled(
    plugin_id: &str,
    native: bool,
    enablement: &GrokPluginEnablement,
) -> bool {
    if grok_plugin_list_contains(&enablement.disabled, plugin_id) {
        return false;
    }
    if native && !enablement.enabled.is_empty() {
        return grok_plugin_list_contains(&enablement.enabled, plugin_id);
    }
    true
}

fn lookup_plugin_bool(enablement: &BTreeMap<String, bool>, plugin_id: &str) -> Option<bool> {
    let prefix = format!("{plugin_id}@");
    let mut matched: Option<bool> = None;
    for (key, enabled) in enablement {
        if key == plugin_id || key.starts_with(&prefix) {
            matched = Some(matched.map(|m| m || *enabled).unwrap_or(*enabled));
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_off() -> ViewingPluginEnablement {
        let mut claude = BTreeMap::new();
        claude.insert("superpowers@claude-plugins-official".into(), false);
        ViewingPluginEnablement {
            target: AgentTarget::Claude,
            claude,
            codex: BTreeMap::new(),
            grok: GrokPluginEnablement::default(),
        }
    }

    #[test]
    fn every_hub_target_has_an_enablement_arm() {
        for target in AgentTarget::ALL {
            let loaded = ViewingPluginEnablement::empty(target);
            let (enabled, _) = plugin_actual_enabled(&loaded, "anything", None, false);
            assert!(
                enabled,
                "{target:?} empty viewing store must not inherit another agent"
            );
        }
    }

    #[test]
    fn borrowed_plugin_ignores_claude_off_on_every_non_claude_target() {
        for target in AgentTarget::ALL {
            if target == AgentTarget::Claude {
                continue;
            }
            // 即使快照里误塞了 Claude 表，也不得用它：load() 对非 Claude 为空。
            let viewing = ViewingPluginEnablement::empty(target);
            let (enabled, _) = plugin_actual_enabled(
                &viewing,
                "superpowers",
                Some("superpowers@claude-plugins-official"),
                false,
            );
            assert!(
                enabled,
                "{target:?} borrowed plugin must stay enabled when Claude closed it"
            );
        }
        let (claude_off, _) = plugin_actual_enabled(
            &claude_off(),
            "superpowers",
            Some("superpowers@claude-plugins-official"),
            true,
        );
        assert!(!claude_off);
    }

    #[test]
    fn codex_native_whitelist_does_not_disable_borrowed_plugins() {
        let mut codex = BTreeMap::new();
        codex.insert("browser@openai-bundled".into(), true);
        let viewing = ViewingPluginEnablement {
            target: AgentTarget::Codex,
            claude: BTreeMap::new(),
            codex,
            grok: GrokPluginEnablement::default(),
        };
        let (native_missing, warn) = plugin_actual_enabled(&viewing, "latex", None, true);
        assert!(!native_missing);
        assert_eq!(warn.as_deref(), Some("codex_plugin_not_in_config"));
        let (borrowed, warn) = plugin_actual_enabled(&viewing, "superpowers", None, false);
        assert!(
            borrowed,
            "Codex whitelist must not close borrowed Claude plugins"
        );
        assert!(warn.is_none());
        let (listed, _) = plugin_actual_enabled(&viewing, "browser", None, true);
        assert!(listed);
    }

    #[test]
    fn grok_disabled_list_closes_borrowed_but_enabled_whitelist_does_not() {
        let viewing = ViewingPluginEnablement {
            target: AgentTarget::Grok,
            claude: BTreeMap::new(),
            codex: BTreeMap::new(),
            grok: GrokPluginEnablement {
                enabled: vec!["native-only".into()],
                disabled: vec!["ecc".into()],
            },
        };
        assert!(
            plugin_actual_enabled(&viewing, "superpowers", None, false).0,
            "Grok enabled whitelist is native-only"
        );
        assert!(!plugin_actual_enabled(&viewing, "ecc", None, false).0);
        assert!(plugin_actual_enabled(&viewing, "native-only", None, true).0);
        assert!(!plugin_actual_enabled(&viewing, "other-native", None, true).0);
    }

    #[test]
    fn claude_prefers_full_registry_key() {
        let mut claude = BTreeMap::new();
        claude.insert("superpowers@claude-plugins-official".into(), false);
        claude.insert("superpowers@superpowers-marketplace".into(), true);
        let viewing = ViewingPluginEnablement {
            target: AgentTarget::Claude,
            claude,
            codex: BTreeMap::new(),
            grok: GrokPluginEnablement::default(),
        };
        let (official, _) = plugin_actual_enabled(
            &viewing,
            "superpowers",
            Some("superpowers@claude-plugins-official"),
            true,
        );
        assert!(!official, "exact marketplace key must win");
        let empty = ViewingPluginEnablement::empty(AgentTarget::Claude);
        let (empty_map, _) = plugin_actual_enabled(&empty, "superpowers", None, true);
        assert!(empty_map, "missing settings keeps installed=true");
        let (unlisted, _) = plugin_actual_enabled(&viewing, "other", None, true);
        assert!(unlisted, "installed but unlisted defaults enabled");
    }

    #[test]
    fn parse_grok_plugin_enablement_reads_enabled_and_disabled_arrays() {
        let parsed = parse_grok_plugin_enablement_from_toml(
            r#"
[plugins]
enabled = ["superpowers", "native-plugin"]
disabled = ["ecc"]
"#,
        );
        assert_eq!(parsed.enabled, vec!["superpowers", "native-plugin"]);
        assert_eq!(parsed.disabled, vec!["ecc"]);
        assert_eq!(
            parse_grok_plugin_enablement_from_toml(""),
            GrokPluginEnablement::default()
        );
    }
}
