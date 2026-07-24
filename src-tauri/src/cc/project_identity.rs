//! Claude 历史项目身份解析。
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude Code 在主工作区、Git worktree 或仓库子目录中运行时都会记录不同 cwd。
//!     历史页需要把这些 cwd 稳定归到同一个 Git 主项目，且不能依赖可能已删除的
//!     Workbench worktree 元数据。
//!
//! Code Logic（这个模块做什么）:
//!     优先识别 `.worktrees/<name>` 与 `.claude/worktrees/<name>` 约定路径；
//!     对仍存在的目录再读取 Git common dir，把普通子目录和外置 worktree 解析到
//!     主工作区根目录；无法证明属于 Git 项目时保留原路径。

use std::path::{Path, PathBuf};
use std::process::Command;

const WORKTREE_MARKERS: [&str; 4] = [
    "/.claude/worktrees/",
    "/.worktrees/",
    "\\.claude\\worktrees\\",
    "\\.worktrees\\",
];

/// 从约定式 worktree 路径中提取主项目路径。
///
/// Business Logic（为什么需要这个函数）:
///     已删除 worktree 已无法再执行 Git 命令，但其历史 cwd 仍包含稳定的目录约定，
///     必须仅凭历史字符串恢复主项目身份。
///
/// Code Logic（这个函数做什么）:
///     查找 POSIX/Windows 两类 `.worktrees` 标记，返回标记之前的非空前缀；
///     未命中则返回 None。
fn conventional_worktree_main_path(project_path: &str) -> Option<String> {
    WORKTREE_MARKERS
        .iter()
        .filter_map(|marker| {
            project_path
                .find(marker)
                .map(|index| (index, &project_path[..index]))
        })
        .min_by_key(|(index, _)| *index)
        .and_then(|(_, prefix)| {
            let trimmed = prefix.trim_end_matches(['/', '\\']);
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

/// 尽力把路径规范化为磁盘上的绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     macOS `/var` 与 `/private/var` 等别名会让同一项目再次产生不同字符串键。
///
/// Code Logic（这个函数做什么）:
///     路径存在时使用 canonicalize；失败或路径不存在时原样返回。
fn canonicalize_existing_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .into_owned()
}

/// 读取现存 Git cwd 对应的主工作区根路径。
///
/// Business Logic（为什么需要这个函数）:
///     Prompt 可能从仓库子目录或放在任意外部目录的 linked worktree 中发出；
///     仅靠目录命名无法完整判断它们属于哪个主项目。
///
/// Code Logic（这个函数做什么）:
///     执行 `git rev-parse --git-common-dir`；common dir 名为 `.git` 时取其父目录
///     作为主工作区，否则回退 `--show-toplevel` 的当前工作树根。命令失败返回 None。
fn git_main_worktree_path(project_path: &str) -> Option<String> {
    if !Path::new(project_path).is_dir() {
        return None;
    }
    let common_output = Command::new("git")
        .args([
            "-C",
            project_path,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .output()
        .ok()?;
    if !common_output.status.success() {
        return None;
    }
    let common_text = String::from_utf8(common_output.stdout).ok()?;
    let common_dir = PathBuf::from(common_text.trim());
    if common_dir.file_name().and_then(|name| name.to_str()) == Some(".git") {
        let main_path = common_dir.parent()?.to_string_lossy().into_owned();
        return Some(canonicalize_existing_path(&main_path));
    }

    let top_output = Command::new("git")
        .args(["-C", project_path, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !top_output.status.success() {
        return None;
    }
    let top_text = String::from_utf8(top_output.stdout).ok()?;
    let top_level = top_text.trim();
    (!top_level.is_empty()).then(|| canonicalize_existing_path(top_level))
}

/// 解析 Claude 历史 cwd 的稳定主项目路径。
///
/// Business Logic（为什么需要这个函数）:
///     主工作区、仓库子目录、当前 worktree 与已删除 worktree 的 Prompt 都必须归入
///     同一个项目入口，避免 Claude 历史按 cwd 碎片化。
///
/// Code Logic（这个函数做什么）:
///     约定式 worktree 路径优先（支持已删除目录），其次查询现存 Git common dir；
///     两者都无法解析时保留输入路径。
pub fn canonical_project_path(project_path: &str) -> String {
    if let Some(main_path) = conventional_worktree_main_path(project_path) {
        return canonicalize_existing_path(&main_path);
    }
    git_main_worktree_path(project_path).unwrap_or_else(|| project_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_worktree_paths_support_posix_and_windows() {
        assert_eq!(
            canonical_project_path("/projects/repo/.worktrees/feature-a/apps/api"),
            "/projects/repo"
        );
        assert_eq!(
            canonical_project_path("/projects/repo/.claude/worktrees/feature-a"),
            "/projects/repo"
        );
        assert_eq!(
            canonical_project_path(r"C:\projects\repo\.worktrees\feature-a\apps\api"),
            r"C:\projects\repo"
        );
    }
}
