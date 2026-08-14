//! 扫描 plugin 根下的一级游戏目录。
//!
//! Business Logic（为什么需要这个模块）:
//!     大厅要列出可玩/不可玩的游戏，并给出缺产物或清单损坏的原因。
//!
//! Code Logic（这个模块做什么）:
//!     读一级子目录；校验 id/entry；不存在则创建根目录。

use super::manifest::GameManifest;
use crate::error::AppError;
use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// 大厅展示用的插件摘要。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GamePluginSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub entry: String,
    pub reward_minutes: i64,
    pub playable: bool,
    pub reason: Option<String>,
}

/// kebab-case 游戏 id。
fn is_valid_game_id(id: &str) -> bool {
    let mut chars = id.chars().peekable();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    let mut prev_hyphen = false;
    for ch in chars {
        if ch == '-' {
            if prev_hyphen {
                return false;
            }
            prev_hyphen = true;
            continue;
        }
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() {
            return false;
        }
        prev_hyphen = false;
    }
    !prev_hyphen
}

/// entry 必须是相对路径且不含 `..`。
fn is_safe_relative_entry(entry: &str) -> bool {
    let path = Path::new(entry);
    if path.is_absolute() || entry.is_empty() {
        return false;
    }
    path.components().all(|c| matches!(c, Component::Normal(_)))
}

/// 不存在则创建 plugin 根，再扫描。
pub fn list_or_create(root: &Path) -> Result<Vec<GamePluginSummary>, AppError> {
    ensure_game_plugin_dir(root)?;
    scan_game_plugins(root)
}

/// 创建 plugin 根目录。
pub fn ensure_game_plugin_dir(root: &Path) -> Result<(), AppError> {
    if root.exists() {
        if !root.is_dir() {
            return Err(AppError::validation(format!(
                "游戏插件路径不是目录: {}",
                root.display()
            )));
        }
        return Ok(());
    }
    fs::create_dir_all(root).map_err(|e| {
        AppError::validation(format!("无法创建游戏插件目录 {}: {e}", root.display()))
    })
}

/// 扫描一级子目录。无 game.json 的文件夹忽略。
pub fn scan_game_plugins(root: &Path) -> Result<Vec<GamePluginSummary>, AppError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    if !root.is_dir() {
        return Err(AppError::validation(format!(
            "游戏插件路径不是目录: {}",
            root.display()
        )));
    }
    let mut games = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(root)
        .map_err(|e| AppError::validation(format!("无法读取游戏插件目录: {e}")))?
        .filter_map(|e| e.ok().map(|d| d.path()))
        .collect();
    entries.sort();
    for dir in entries {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("game.json");
        if !manifest_path.is_file() {
            continue;
        }
        games.push(summarize_game(&dir, name, &manifest_path));
    }
    Ok(games)
}

fn summarize_game(dir: &Path, folder: &str, manifest_path: &Path) -> GamePluginSummary {
    let text = match fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(_) => {
            return invalid(folder, folder, "index.html", 0, "invalid_manifest");
        }
    };
    let parsed: Result<GameManifest, _> = serde_json::from_str(&text);
    let Ok(manifest) = parsed else {
        return invalid(folder, folder, "index.html", 0, "invalid_manifest");
    };
    let reward = manifest.reward_minutes.max(0);
    if !is_valid_game_id(&manifest.id) || manifest.id != folder {
        return invalid(
            folder,
            &manifest.name,
            &manifest.entry,
            reward,
            "invalid_id",
        );
    }
    if !is_safe_relative_entry(&manifest.entry) {
        return invalid(
            &manifest.id,
            &manifest.name,
            &manifest.entry,
            reward,
            "invalid_entry",
        );
    }
    if !dir.join(&manifest.entry).is_file() {
        return GamePluginSummary {
            id: manifest.id,
            name: manifest.name,
            description: manifest.description,
            entry: manifest.entry,
            reward_minutes: reward,
            playable: false,
            reason: Some("missing_entry".into()),
        };
    }
    GamePluginSummary {
        id: manifest.id,
        name: manifest.name,
        description: manifest.description,
        entry: manifest.entry,
        reward_minutes: reward,
        playable: true,
        reason: None,
    }
}

fn invalid(
    id: &str,
    name: &str,
    entry: &str,
    reward_minutes: i64,
    reason: &str,
) -> GamePluginSummary {
    GamePluginSummary {
        id: id.to_string(),
        name: name.to_string(),
        description: String::new(),
        entry: entry.to_string(),
        reward_minutes,
        playable: false,
        reason: Some(reason.into()),
    }
}

/// 把游戏 id + 相对路径解析到 plugin 根内的真实文件。
pub fn resolve_game_asset(root: &Path, id: &str, rel: &str) -> Result<PathBuf, AppError> {
    if !is_valid_game_id(id) {
        return Err(AppError::validation("非法游戏 id"));
    }
    if !is_safe_relative_entry(rel) {
        return Err(AppError::validation("非法游戏资源路径"));
    }
    let game_root = root.join(id);
    let candidate = game_root.join(rel);
    let root_canon = game_root.canonicalize().map_err(|_| {
        AppError::validation(format!("游戏目录不存在: {}", game_root.display()))
    })?;
    let file_canon = candidate
        .canonicalize()
        .map_err(|_| AppError::validation("游戏资源不存在"))?;
    if !file_canon.starts_with(&root_canon) {
        return Err(AppError::validation("游戏资源路径逃逸"));
    }
    if !file_canon.is_file() {
        return Err(AppError::validation("游戏资源不是文件"));
    }
    Ok(file_canon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_valid_static_game_and_skips_hidden() {
        let root = tempfile::tempdir().unwrap();
        let snake = root.path().join("snake");
        fs::create_dir_all(&snake).unwrap();
        fs::write(
            snake.join("game.json"),
            r#"{
                "id":"snake","name":"Snake","description":"s","entry":"index.html","rewardMinutes":5
            }"#,
        )
        .unwrap();
        fs::write(snake.join("index.html"), "<html></html>").unwrap();
        fs::create_dir_all(root.path().join(".cache")).unwrap();
        let games = scan_game_plugins(root.path()).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "snake");
        assert!(games[0].playable);
        assert_eq!(games[0].reward_minutes, 5);
    }

    #[test]
    fn marks_missing_entry_not_playable() {
        let root = tempfile::tempdir().unwrap();
        let g = root.path().join("tower");
        fs::create_dir_all(&g).unwrap();
        fs::write(
            g.join("game.json"),
            r#"{"id":"tower","name":"Tower","description":"t","entry":"dist/index.html"}"#,
        )
        .unwrap();
        let games = scan_game_plugins(root.path()).unwrap();
        assert_eq!(games.len(), 1);
        assert!(!games[0].playable);
        assert_eq!(games[0].reason.as_deref(), Some("missing_entry"));
    }

    #[test]
    fn rejects_id_mismatch_and_path_escape_entry() {
        let root = tempfile::tempdir().unwrap();
        let g = root.path().join("bad");
        fs::create_dir_all(&g).unwrap();
        fs::write(
            g.join("game.json"),
            r#"{"id":"other","name":"Bad","entry":"../secret.html"}"#,
        )
        .unwrap();
        let games = scan_game_plugins(root.path()).unwrap();
        assert_eq!(games.len(), 1);
        assert!(!games[0].playable);
        assert!(games[0].reason.as_deref() == Some("invalid_id") || games[0].reason.as_deref() == Some("invalid_entry"));
    }

    #[test]
    fn resolve_asset_rejects_escape() {
        let root = tempfile::tempdir().unwrap();
        let g = root.path().join("snake");
        fs::create_dir_all(&g).unwrap();
        fs::write(g.join("index.html"), "ok").unwrap();
        assert!(resolve_game_asset(root.path(), "snake", "index.html").is_ok());
        assert!(resolve_game_asset(root.path(), "snake", "../index.html").is_err());
        assert!(resolve_game_asset(root.path(), "other", "index.html").is_err());
    }

    #[test]
    fn list_creates_missing_dir_and_returns_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("plugins");
        let games = list_or_create(&dir).unwrap();
        assert!(dir.is_dir());
        assert!(games.is_empty());
    }
}
