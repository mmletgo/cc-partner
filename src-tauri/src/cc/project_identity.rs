//! Claude 历史项目身份解析。
//!
//! Business Logic（为什么需要这个模块）:
//!     Claude Code 在主工作区、Git worktree 或仓库子目录中运行时都会记录不同 cwd。
//!     历史页需要把这些 cwd 稳定归到同一个 Git 主项目，且不能依赖目录命名约定或
//!     Workbench 自有的 worktree 元数据。
//!
//! Code Logic（这个模块做什么）:
//!     对现存目录执行 `git worktree list --porcelain -z`，读取 Git 报告的全部 worktree，
//!     以列表首项（主工作区）作为稳定项目身份；无法由 Git 证明归属时保留原路径。

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// 读取现存 Git cwd 所属项目的全部 worktree 路径。
///
/// Business Logic（为什么需要这个函数）:
///     Prompt 可能从仓库子目录或放在任意外部目录的 linked worktree 中发出；
///     Git 自己维护完整 worktree 清单，项目归属不能由目录名或应用数据库猜测。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree list --porcelain -z`，从 NUL 分隔字段中提取每个 `worktree `
///     记录并规范化路径；Git 保证主工作区列在首位。命令失败或无记录时返回 None。
fn git_worktree_paths(project_path: &str) -> Option<Vec<String>> {
    if !Path::new(project_path).is_dir() {
        return None;
    }
    let output = Command::new("git")
        .args(["-C", project_path, "worktree", "list", "--porcelain", "-z"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let paths = output
        .stdout
        .split(|byte| *byte == b'\0')
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| !path.is_empty())
        .map(canonicalize_existing_path)
        .collect::<Vec<_>>();
    (!paths.is_empty()).then_some(paths)
}

/// 解析 Claude 历史 cwd 的稳定主项目路径。
///
/// Business Logic（为什么需要这个函数）:
///     主工作区、仓库子目录与当前 linked worktree 的 Prompt 必须归入同一个项目入口，
///     同时不能把名称恰好类似 worktree 的普通目录误归到其它项目。
///
/// Code Logic（这个函数做什么）:
///     查询 Git 的完整 worktree 清单并取主工作区；Git 无法解析时保留输入路径。
pub fn canonical_project_path(project_path: &str) -> String {
    git_worktree_paths(project_path)
        .and_then(|paths| paths.into_iter().next())
        .unwrap_or_else(|| project_path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 建立真实 linked worktree，验证项目身份来自 Git 的完整 worktree 清单。
    #[test]
    fn git_worktree_list_discovers_main_and_linked_worktrees() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let main = temp.path().join("main");
        let linked = temp.path().join("linked-anywhere");
        let main_text = main.to_string_lossy().into_owned();
        let linked_text = linked.to_string_lossy().into_owned();
        std::fs::create_dir_all(&main).expect("应创建主仓库目录");
        for args in [
            vec!["init", main_text.as_str()],
            vec!["-C", main_text.as_str(), "config", "user.name", "Test"],
            vec![
                "-C",
                main_text.as_str(),
                "config",
                "user.email",
                "test@example.com",
            ],
        ] {
            let output = Command::new("git").args(args).output().expect("应执行 git");
            assert!(output.status.success());
        }
        std::fs::write(main.join("README.md"), "base\n").expect("应写入测试文件");
        for args in [
            vec!["-C", main_text.as_str(), "add", "README.md"],
            vec!["-C", main_text.as_str(), "commit", "-m", "init"],
            vec![
                "-C",
                main_text.as_str(),
                "worktree",
                "add",
                "-b",
                "feature",
                linked_text.as_str(),
            ],
        ] {
            let output = Command::new("git").args(args).output().expect("应执行 git");
            assert!(output.status.success());
        }

        let paths = git_worktree_paths(&linked_text).expect("Git 应返回 worktree 清单");
        let canonical_main = main.canonicalize().expect("主仓库应可规范化");
        let canonical_linked = linked.canonicalize().expect("linked worktree 应可规范化");

        assert_eq!(PathBuf::from(&paths[0]), canonical_main);
        assert!(paths
            .iter()
            .map(PathBuf::from)
            .any(|path| path == canonical_linked));
        assert_eq!(
            PathBuf::from(canonical_project_path(&linked_text)),
            canonical_main
        );
    }

    #[test]
    fn directory_name_that_looks_like_worktree_does_not_override_git_identity() {
        let temp = tempfile::tempdir().expect("应创建临时目录");
        let nested_repo = temp.path().join(".worktrees").join("independent");
        std::fs::create_dir_all(&nested_repo).expect("应创建嵌套仓库目录");
        let output = Command::new("git")
            .args(["init", nested_repo.to_string_lossy().as_ref()])
            .output()
            .expect("应执行 git init");
        assert!(output.status.success());

        assert_eq!(
            PathBuf::from(canonical_project_path(
                nested_repo.to_string_lossy().as_ref()
            )),
            nested_repo.canonicalize().expect("嵌套仓库应可规范化")
        );
    }

    #[test]
    fn missing_worktree_path_is_not_guessed_from_directory_convention() {
        let missing = "/projects/repo/.worktrees/deleted/apps/api";
        assert_eq!(
            canonical_project_path(missing),
            missing,
            "Git 无法证明归属时必须保留原路径"
        );
    }
}
