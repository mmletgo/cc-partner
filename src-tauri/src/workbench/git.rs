//! workbench/git.rs — 工作台 Git/worktree 辅助逻辑
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 需要把项目下多个 Git worktree 作为比 terminal window 更高一级的工作区。
//!     用户在一个项目中切换 worktree 后，文件树、Prompt 优化目录和 terminal windows 都应跟随该工作区。
//!
//! Code Logic（这个模块做什么）:
//!     封装系统 git CLI 调用、worktree/status 输出解析和工作台专用 worktree 路径生成。

use crate::error::AppError;
use crate::workbench::models::WorkbenchGitStatusDto;
use std::path::Path;
use std::process::Command;

/// `git worktree list --porcelain` 的单项解析结果。
///
/// Business Logic（为什么需要这个结构体）:
///     Workbench 需要把 Git worktree 映射成可展示的工作区候选。
///
/// Code Logic（这个结构体做什么）:
///     保存 worktree path、branch 与是否为项目主工作区三类字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedWorktree {
    pub path: String,
    pub branch: Option<String>,
    pub is_main: bool,
}

/// Workbench push 操作的远端选择结果。
///
/// Business Logic（为什么需要这个枚举）:
///     用户仓库可能已经设置 upstream，也可能只有非 origin 的单个 remote。
///
/// Code Logic（这个枚举做什么）:
///     区分复用现有 upstream 的普通 `git push`，以及首次推送时需要 `-u <remote> <branch>`。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PushTarget {
    Upstream,
    Remote(String),
}

/// Business Logic（为什么需要这个函数）:
///     Git worktree 管理命令都需要执行系统 git，并在失败时返回可读错误。
///
/// Code Logic（这个函数做什么）:
///     在指定 cwd 下执行 `git <args>`，成功返回 stdout，失败把 stderr/stdout 合并成 AppError。
fn run_git(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    let output = Command::new("git").args(args).current_dir(cwd).output()?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let message = if detail.is_empty() {
        "未知 Git 错误".to_string()
    } else {
        detail
    };
    Err(AppError::generic(format!("Git 命令失败: {message}")))
}

/// Business Logic（为什么需要这个函数）:
///     用户添加的项目可能是子目录，worktree 操作必须先找到 Git 仓库根目录。
///
/// Code Logic（这个函数做什么）:
///     调用 `git rev-parse --show-toplevel` 并返回修剪后的绝对路径字符串。
pub fn repo_root(path: &Path) -> Result<String, AppError> {
    let output = run_git(path, &["rev-parse", "--show-toplevel"])?;
    let root = output.trim();
    if root.is_empty() {
        return Err(AppError::generic("当前项目不是 Git 仓库"));
    }
    Ok(root.to_string())
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 需要展示当前项目下 Git 已知的全部 worktree，便于和本地记录对齐。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree list --porcelain` 后交给 parse_worktree_porcelain 解析。
#[allow(dead_code)]
pub fn list_worktrees(repo_path: &Path, main_path: &str) -> Result<Vec<ParsedWorktree>, AppError> {
    let output = run_git(repo_path, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_porcelain(&output, main_path))
}

/// Business Logic（为什么需要这个函数）:
///     顶部 worktree strip 需要显示每个工作区的分支、变更数、领先/落后与冲突数。
///
/// Code Logic（这个函数做什么）:
///     执行 `git status --porcelain --branch`，并解析为 WorkbenchGitStatusDto。
pub fn status(path: &Path) -> Result<WorkbenchGitStatusDto, AppError> {
    let output = run_git(path, &["status", "--porcelain", "--branch"])?;
    Ok(parse_status_porcelain(&output))
}

/// Business Logic（为什么需要这个函数）:
///     创建主 worktree 行或新 worktree 行时，需要知道当前分支名作为默认展示名。
///
/// Code Logic（这个函数做什么）:
///     优先从 status porcelain 读取 branch；失败时回退 None。
pub fn current_branch(path: &Path) -> Option<String> {
    status(path).ok().and_then(|status| status.branch)
}

/// Business Logic（为什么需要这个函数）:
///     用户输入分支名后，Workbench 需要在本机创建对应 Git worktree 和新分支。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree add -b <branch> <path> <base>`；base 为空时使用 HEAD。
pub fn create_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<(), AppError> {
    let target = worktree_path.to_string_lossy().to_string();
    let base_ref = base
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("HEAD");
    run_git(
        repo_path,
        &["worktree", "add", "-b", branch, &target, base_ref],
    )?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     用户需要从 Workbench 直接把当前 worktree 的全部本地改动提交成一个普通 commit。
///
/// Code Logic（这个函数做什么）:
///     执行 `git add -A` 后检查 staged/working 状态；有变更时执行 `git commit -m`，无变更返回 false。
pub fn commit_all(path: &Path, message: &str) -> Result<bool, AppError> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err(AppError::generic("Commit message 不能为空"));
    }
    run_git(path, &["add", "-A"])?;
    let pending = run_git(path, &["status", "--porcelain"])?;
    if pending.trim().is_empty() {
        return Ok(false);
    }
    run_git(path, &["commit", "-m", trimmed])?;
    Ok(true)
}

/// Business Logic（为什么需要这个函数）:
///     用户完成 worktree commit 后，需要把对应分支推送到远端以便协作或备份。
///
/// Code Logic（这个函数做什么）:
///     已有 upstream 时执行普通 `git push`；否则选择 origin 或唯一 remote 执行 `git push -u <remote> <branch>`。
pub fn push_branch(path: &Path, branch: &str) -> Result<(), AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("当前 worktree 没有可推送的分支"));
    }
    match resolve_push_target(path)? {
        PushTarget::Upstream => {
            run_git(path, &["push"])?;
        }
        PushTarget::Remote(remote) => {
            run_git(path, &["push", "-u", &remote, branch])?;
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Push 按钮应尊重用户仓库已有远端配置，不能假设所有项目都有 origin。
///
/// Code Logic（这个函数做什么）:
///     若当前分支已有 upstream，返回 Upstream；否则从 `git remote` 中选择 origin 或唯一 remote。
fn resolve_push_target(path: &Path) -> Result<PushTarget, AppError> {
    if has_upstream(path) {
        return Ok(PushTarget::Upstream);
    }

    let remotes = list_remotes(path)?;
    if remotes.is_empty() {
        return Err(AppError::generic(
            "当前 Git 仓库没有配置 Git remote，无法推送。请先在项目目录执行 `git remote add origin <url>` 后重试。",
        ));
    }
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok(PushTarget::Remote("origin".to_string()));
    }
    if remotes.len() == 1 {
        return Ok(PushTarget::Remote(remotes[0].clone()));
    }

    Err(AppError::generic(format!(
        "当前 Git 仓库有多个 Git remote（{}），但当前分支没有 upstream。请先在终端设置 upstream，或添加/使用 origin 后重试。",
        remotes.join(", ")
    )))
}

/// Business Logic（为什么需要这个函数）:
///     已经跟踪远端分支的 worktree 应复用用户现有 upstream 配置。
///
/// Code Logic（这个函数做什么）:
///     执行 `git rev-parse --abbrev-ref --symbolic-full-name @{u}`，成功且输出非空即视为存在 upstream。
fn has_upstream(path: &Path) -> bool {
    run_git(
        path,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{u}",
        ],
    )
    .map(|output| !output.trim().is_empty())
    .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     首次 push 时需要知道仓库配置了哪些 remote，以选择安全默认值或给出可操作错误。
///
/// Code Logic（这个函数做什么）:
///     执行 `git remote` 并返回去空白后的 remote 名称列表。
fn list_remotes(path: &Path) -> Result<Vec<String>, AppError> {
    let output = run_git(path, &["remote"])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|remote| !remote.is_empty())
        .map(ToString::to_string)
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     用户希望在 Workbench 中把功能 worktree 合并回主工作区所在分支。
///
/// Code Logic（这个函数做什么）:
///     在主工作区路径执行 `git merge --no-ff <branch>`，保留功能分支合并记录。
pub fn merge_branch(main_path: &Path, branch: &str) -> Result<(), AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("当前 worktree 没有可合并的分支"));
    }
    run_git(main_path, &["merge", "--no-ff", branch])?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     用户删除废弃 worktree 时，磁盘上的 Git worktree 也应同步移除。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree remove <path>`；force 为 true 时添加 `--force`。
pub fn remove_worktree(
    repo_path: &Path,
    worktree_path: &Path,
    force: bool,
) -> Result<(), AppError> {
    let target = worktree_path.to_string_lossy().to_string();
    if force {
        run_git(repo_path, &["worktree", "remove", "--force", &target])?;
    } else {
        run_git(repo_path, &["worktree", "remove", &target])?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Git porcelain worktree 输出是多行文本，UI 需要结构化 path/branch/main 字段。
///
/// Code Logic（这个函数做什么）:
///     按空行切分 block，读取 `worktree` 与 `branch refs/heads/*` 行，主路径与 main_path 相等则标记 is_main。
pub fn parse_worktree_porcelain(output: &str, main_path: &str) -> Vec<ParsedWorktree> {
    let normalized_main = main_path.trim_end_matches('/');
    let mut items = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines().chain(std::iter::once("")) {
        let line = line.trim();
        if line.is_empty() {
            if let Some(path) = current_path.take() {
                let is_main = path.trim_end_matches('/') == normalized_main;
                items.push(ParsedWorktree {
                    path,
                    branch: current_branch.take(),
                    is_main,
                });
            }
            current_branch = None;
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path.to_string());
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        }
    }

    items
}

/// Business Logic（为什么需要这个函数）:
///     Git status 原始文本不适合直接给 UI；Workbench 只需要摘要数字和当前分支。
///
/// Code Logic（这个函数做什么）:
///     解析 branch header 的 ahead/behind，并统计非 header 行的 changed/conflicts。
pub fn parse_status_porcelain(output: &str) -> WorkbenchGitStatusDto {
    let mut status = WorkbenchGitStatusDto {
        clean: true,
        ..WorkbenchGitStatusDto::default()
    };

    for line in output.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            parse_branch_header(header, &mut status);
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        status.changed += 1;
        if status_code_has_conflict(line) {
            status.conflicts += 1;
        }
    }

    status.clean = status.changed == 0 && status.conflicts == 0;
    status
}

/// Business Logic（为什么需要这个函数）:
///     用户输入的 Git 分支名会被用于本机目录名，需要转成稳定且可读的安全 slug。
///
/// Code Logic（这个函数做什么）:
///     保留 ASCII 字母数字，其他字符折叠成单个 `-`，去掉首尾 `-`；空结果回退 worktree。
pub fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in branch.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "worktree".to_string()
    } else {
        slug
    }
}

/// Business Logic（为什么需要这个函数）:
///     分支 header 同时承载 branch 名和远端 ahead/behind 信息，需要集中解析。
///
/// Code Logic（这个函数做什么）:
///     从 `branch...upstream [ahead N, behind M]` 中提取 branch/ahead/behind。
fn parse_branch_header(header: &str, status: &mut WorkbenchGitStatusDto) {
    let branch_part = header
        .split([' ', '['])
        .next()
        .unwrap_or_default()
        .split("...")
        .next()
        .unwrap_or_default()
        .trim();
    if !branch_part.is_empty() {
        status.branch = Some(branch_part.to_string());
    }

    let Some(start) = header.find('[') else {
        return;
    };
    let Some(end) = header[start + 1..].find(']') else {
        return;
    };
    let summary = &header[start + 1..start + 1 + end];
    for part in summary.split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("ahead ") {
            status.ahead = value.parse::<u32>().unwrap_or(0);
        } else if let Some(value) = part.strip_prefix("behind ") {
            status.behind = value.parse::<u32>().unwrap_or(0);
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     冲突状态需要在 worktree strip 上突出显示，用户才能先处理冲突再 merge/push。
///
/// Code Logic（这个函数做什么）:
///     读取 porcelain 状态码前两列，任一列为 U 或组合为 AA/DD 即视为冲突。
fn status_code_has_conflict(line: &str) -> bool {
    let code = line.get(0..2).unwrap_or_default();
    matches!(
        code,
        "UU" | "AA" | "DD" | "AU" | "UA" | "DU" | "UD" | "U " | " U"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    /// Business Logic（为什么需要这个测试）:
    ///     Git worktree 管理层需要识别主工作区和链接 worktree，供前端渲染 worktree strip。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入 `git worktree list --porcelain` 输出，断言解析出 path、branch 和 main 标识。
    #[test]
    fn parse_worktree_porcelain_marks_main_and_branch() {
        let output = "\
worktree /repo/main
HEAD abcdef
branch refs/heads/main

worktree /repo/.worktrees/feature-a
HEAD 123456
branch refs/heads/feature-a
";

        let parsed = parse_worktree_porcelain(output, "/repo/main");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].path, "/repo/main");
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert!(parsed[0].is_main);
        assert_eq!(parsed[1].branch.as_deref(), Some("feature-a"));
        assert!(!parsed[1].is_main);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench Git 状态卡需要显示 dirty/ahead/behind/conflict 等摘要，而不能把原始
    ///     porcelain 文本直接泄露给 UI。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入 branch/status porcelain v1 输出，断言统计 ahead/behind、变更数和冲突数。
    #[test]
    fn parse_status_porcelain_counts_dirty_ahead_behind_and_conflicts() {
        let output = "\
## feature-a...origin/feature-a [ahead 2, behind 1]
 M src/lib.rs
?? docs/new.md
UU web/src/App.tsx
";

        let status = parse_status_porcelain(output);

        assert_eq!(status.branch.as_deref(), Some("feature-a"));
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert_eq!(status.changed, 3);
        assert_eq!(status.conflicts, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户输入分支名可能包含斜杠和符号，生成本地 worktree 目录时必须稳定、安全、可读。
    ///
    /// Code Logic（这个测试做什么）:
    ///     校验 branch slug 会保留字母数字并把连续非法字符折叠成单个 `-`。
    #[test]
    fn branch_slug_is_filesystem_safe() {
        assert_eq!(branch_slug("feat/worktree ui!!"), "feat-worktree-ui");
        assert_eq!(branch_slug("  hotfix  "), "hotfix");
        assert_eq!(branch_slug("///"), "worktree");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户项目可能只配置了非 origin 的单个远端，Workbench push 不应硬编码 origin。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库和 bare remote，只添加 backup remote，断言 push_branch 可以推送当前分支。
    #[test]
    fn push_branch_uses_single_configured_remote_when_origin_missing() {
        let root = temp_git_dir("workbench-push-single-remote");
        let repo = root.join("repo");
        let remote = root.join("backup.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "feature/worktree-push"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(&root, &["init", "--bare", remote.to_string_lossy().as_ref()]);
        git_test_command(
            &repo,
            &["remote", "add", "backup", remote.to_string_lossy().as_ref()],
        );

        push_branch(&repo, "feature/worktree-push").expect("push with single non-origin remote");
        git_test_command(
            &remote,
            &["rev-parse", "--verify", "refs/heads/feature/worktree-push"],
        );

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户在没有配置任何远端的本地项目里点 Push 时，需要看到可操作提示，而不是 Git fatal。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建无 remote 的真实 Git 仓库，断言 push_branch 返回包含配置 remote 引导的业务错误。
    #[test]
    fn push_branch_reports_missing_remote_before_git_fatal() {
        let root = temp_git_dir("workbench-push-no-remote");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "feature/local-only"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);

        let err = push_branch(&repo, "feature/local-only").expect_err("missing remote should fail");
        let message = err.to_string();
        assert!(message.contains("没有配置 Git remote"));
        assert!(message.contains("git remote add origin <url>"));

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     Git 集成测试需要隔离目录，避免污染用户项目或复用历史状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在系统临时目录下生成带 UUID 的测试目录路径。
    fn temp_git_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", Uuid::new_v4()))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     测试需要反复执行 Git CLI，并在失败时输出完整上下文便于定位。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在指定 cwd 下执行 git 命令，非零退出时 panic 并打印 stdout/stderr。
    fn git_test_command(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        if output.status.success() {
            return String::from_utf8_lossy(&output.stdout).to_string();
        }
        panic!(
            "git {:?} failed in {}:\nstdout:\n{}\nstderr:\n{}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
