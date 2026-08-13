//! plugin 安装路径身份解析
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude/Codex 的 plugin 常落在 `plugins/cache/<market>/<id>/<version>/...`，
//!     一级 `plugins/<name>` 又可能是 cache/data/staging 基础设施。
//!     若把 `cache` 当 plugin id、或把版本目录名当 package 名，inventory 会出现
//!     `plugin:cache`、`1.0.0`、假 package 行与跨版本 Detached。
//!
//! Code Logic（这个模块做什么）:
//!     解析路径布局 → plugin_id / package_root；识别 infrastructure 目录名。

use std::path::{Path, PathBuf};

/// `plugins/` 下一层绝不是 package 的基础设施目录名。
const PLUGIN_INFRASTRUCTURE_NAMES: &[&str] = &[
    "cache",
    "data",
    "marketplaces",
    ".marketplace-plugin-source-staging",
    ".plugin-appserver",
    ".remote-plugin-install-staging",
];

/// 组件子目录名（出现在 package root 下）。
const PLUGIN_COMPONENT_DIR_NAMES: &[&str] = &[
    "skills",
    "commands",
    "agents",
    "hooks",
    "mcp",
    "mcp-servers",
    ".claude-plugin",
    ".codex-plugin",
];

/// Business Logic: UI/账本不得把 cache/data/staging 当 package。
/// Code Logic: 对照固定名表（大小写敏感，与磁盘一致）。
pub fn is_plugin_infrastructure_name(name: &str) -> bool {
    PLUGIN_INFRASTRUCTURE_NAMES.contains(&name)
}

/// Business Logic: 从任意 plugin 内路径或 package 根得到稳定 plugin id。
/// Code Logic:
///   - `.../plugins/cache/<market>/<id>/<version>(/...)` → `<id>`
///   - `.../plugins/<id>(/...)` 且 `<id>` 非基础设施 → `<id>`
///   - 其它 → None
pub fn plugin_id_from_path(path: Option<&str>) -> Option<String> {
    let path = path?;
    let parts = path_segments_owned(path);
    plugin_id_from_owned_segments(&parts)
}

/// Business Logic: package 根用于 parent inventory id 与 native_path。
/// Code Logic:
///   - cache 布局：停在 version 目录
///   - 直装布局：停在 plugins 下第一层非基础设施目录
pub fn infer_plugin_package_root(path: &Path) -> Option<PathBuf> {
    let owned = path.to_string_lossy().into_owned();
    let parts = path_segments_owned(&owned);
    let (plugins_idx, layout) = plugin_layout_from_owned_segments(&parts)?;
    let end = match layout {
        PluginLayoutOwned::Cache { .. } => plugins_idx + 4, // plugins + cache + market + id + version
        PluginLayoutOwned::Direct { .. } => plugins_idx + 1,
    };
    truncate_path_to_plugin_root(path, end)
}

/// Business Logic: native_path 是否明显是基础设施目录（整包误纳管）。
/// Code Logic: 末段名 ∈ infrastructure 表。
pub fn is_plugin_infrastructure_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(is_plugin_infrastructure_name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PluginLayoutOwned {
    Cache {
        market: String,
        plugin: String,
        version: String,
    },
    Direct {
        plugin: String,
    },
}

fn path_segments_owned(path: &str) -> Vec<String> {
    path.replace('\\', "/")
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .map(|s| s.to_string())
        .collect()
}

fn plugin_layout_from_owned_segments(parts: &[String]) -> Option<(usize, PluginLayoutOwned)> {
    let plugins_idx = parts.iter().position(|p| p == "plugins")?;
    let rest = &parts[plugins_idx + 1..];
    if rest.is_empty() {
        return None;
    }
    if rest[0] == "cache" {
        // plugins/cache/<market>/<id>/<version>/...
        if rest.len() < 4 {
            return None;
        }
        let market = rest[1].as_str();
        let plugin = rest[2].as_str();
        let version = rest[3].as_str();
        if is_plugin_infrastructure_name(market)
            || is_plugin_infrastructure_name(plugin)
            || plugin.is_empty()
            || version.is_empty()
        {
            return None;
        }
        // version 不应是组件目录名（避免把 skills 当 version）
        if PLUGIN_COMPONENT_DIR_NAMES.contains(&version) {
            return None;
        }
        return Some((
            plugins_idx,
            PluginLayoutOwned::Cache {
                market: market.to_string(),
                plugin: plugin.to_string(),
                version: version.to_string(),
            },
        ));
    }
    let plugin = rest[0].as_str();
    if is_plugin_infrastructure_name(plugin) {
        return None;
    }
    Some((
        plugins_idx,
        PluginLayoutOwned::Direct {
            plugin: plugin.to_string(),
        },
    ))
}

fn plugin_id_from_owned_segments(parts: &[String]) -> Option<String> {
    match plugin_layout_from_owned_segments(parts)?.1 {
        PluginLayoutOwned::Cache { plugin, .. } => Some(plugin),
        PluginLayoutOwned::Direct { plugin } => Some(plugin),
    }
}

fn truncate_path_to_plugin_root(path: &Path, end_idx_inclusive: usize) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut seg_i: isize = -1;
    for comp in path.components() {
        use std::path::Component;
        match comp {
            Component::Prefix(p) => {
                out.push(p.as_os_str());
            }
            Component::RootDir => {
                out.push(comp.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {}
            Component::Normal(s) => {
                let name = s.to_string_lossy();
                if name.is_empty() {
                    continue;
                }
                seg_i += 1;
                out.push(s);
                if seg_i as usize == end_idx_inclusive {
                    return Some(out);
                }
            }
        }
    }
    // 输入恰好是 package 根
    if seg_i as usize == end_idx_inclusive {
        return Some(out);
    }
    let owned = path.to_string_lossy().into_owned();
    let parts = path_segments_owned(&owned);
    if plugin_layout_from_owned_segments(&parts).is_some() && parts.len() == end_idx_inclusive + 1 {
        return Some(path.to_path_buf());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_from_cache_layout_not_cache() {
        assert_eq!(
            plugin_id_from_path(Some(
                "/Users/h/.claude/plugins/cache/ecc/ecc/2.0.0/skills/deep-research"
            )),
            Some("ecc".into())
        );
        assert_eq!(
            plugin_id_from_path(Some(
                "/Users/h/.codex/plugins/cache/openai-bundled/browser/26.803.61601"
            )),
            Some("browser".into())
        );
        assert_eq!(
            plugin_id_from_path(Some(
                r"C:\Users\a\.claude\plugins\cache\market\pyright-lsp\1.0.0"
            )),
            Some("pyright-lsp".into())
        );
    }

    #[test]
    fn plugin_id_from_direct_layout() {
        assert_eq!(
            plugin_id_from_path(Some("/home/.claude/plugins/demo/skills/x")),
            Some("demo".into())
        );
        assert_eq!(
            plugin_id_from_path(Some("/home/.claude/skills/review")),
            None
        );
        assert_eq!(
            plugin_id_from_path(Some("/home/.claude/plugins/cache")),
            None
        );
        assert_eq!(
            plugin_id_from_path(Some("/home/.codex/plugins/.plugin-appserver")),
            None
        );
    }

    #[test]
    fn package_root_stops_at_version_or_direct_id() {
        let component =
            PathBuf::from("/Users/h/.claude/plugins/cache/ecc/ecc/2.0.0/skills/deep-research");
        assert_eq!(
            infer_plugin_package_root(&component).as_deref(),
            Some(Path::new("/Users/h/.claude/plugins/cache/ecc/ecc/2.0.0"))
        );
        let direct = PathBuf::from("/home/.claude/plugins/demo/skills/x");
        assert_eq!(
            infer_plugin_package_root(&direct).as_deref(),
            Some(Path::new("/home/.claude/plugins/demo"))
        );
        let root =
            PathBuf::from("/Users/h/.codex/plugins/cache/openai-bundled/chrome/26.803.61601");
        assert_eq!(
            infer_plugin_package_root(&root).as_deref(),
            Some(Path::new(
                "/Users/h/.codex/plugins/cache/openai-bundled/chrome/26.803.61601"
            ))
        );
    }

    #[test]
    fn infrastructure_names_detected() {
        assert!(is_plugin_infrastructure_name("cache"));
        assert!(is_plugin_infrastructure_name("data"));
        assert!(is_plugin_infrastructure_name("marketplaces"));
        assert!(is_plugin_infrastructure_name(".plugin-appserver"));
        assert!(!is_plugin_infrastructure_name("browser"));
        assert!(is_plugin_infrastructure_path(Path::new(
            "/Users/h/.claude/plugins/cache"
        )));
    }
}
