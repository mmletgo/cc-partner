//! workbench/git.rs — 工作台 Git/worktree 辅助逻辑
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 需要把项目下多个 Git worktree 作为比 terminal window 更高一级的工作区。
//!     用户在一个项目中切换 worktree 后，文件树、Prompt 优化目录和 terminal windows 都应跟随该工作区。
//!
//! Code Logic（这个模块做什么）:
//!     封装系统 git CLI 调用、worktree/status 输出解析和工作台专用 worktree 路径生成。

use crate::error::AppError;
use crate::workbench::models::{
    WorkbenchGitCommitDto, WorkbenchGitRefDto, WorkbenchGitRefKindDto, WorkbenchGitStatusDto,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// 已暂存改动的 commit message 输入摘要。
///
/// Business Logic（为什么需要这个结构体）:
///     Claude Code 生成 commit message 时需要看到真实会进入 commit 的改动内容。
///
/// Code Logic（这个结构体做什么）:
///     保存 staged diff 的 stat、正文和正文是否因长度上限被截断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedCommitChanges {
    pub stat: String,
    pub diff: String,
    pub truncated: bool,
}

/// Git merge 尝试的分类结果。
///
/// Business Logic（为什么需要这个枚举）:
///     Workbench 一键 merge 遇到冲突时需要进入 Claude Code 自动解决阶段，而不是把 Git 非零退出
///     直接作为终止错误返回给用户。
///
/// Code Logic（这个枚举做什么）:
///     区分 merge 已完成和 merge 进入冲突状态两类可继续处理的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBranchOutcome {
    Merged,
    Conflicted,
}

const MAX_COMMIT_DIFF_CHARS: usize = 24_000;

/// Business Logic（为什么需要这个函数）:
///     多个 Git helper 都需要把失败输出整理成用户可读错误，避免只展示退出码。
///
/// Code Logic（这个函数做什么）:
///     优先取 stderr，缺失时取 stdout，两者都为空时返回统一兜底文案。
fn git_failure_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        "未知 Git 错误".to_string()
    } else {
        detail
    }
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
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     用户合并并删除 worktree 后，本地分支引用可能仍残留；再次创建同名 worktree 前需要判断分支名是否仍被占用。
///
/// Code Logic（这个函数做什么）:
///     使用 `git show-ref --verify --quiet refs/heads/<branch>` 检查本地分支是否存在，退出码 1 视为不存在。
fn local_branch_exists(repo_path: &Path, branch: &str) -> Result<bool, AppError> {
    if branch.trim().is_empty() {
        return Ok(false);
    }
    let local_ref = format!("refs/heads/{branch}");
    let output = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &local_ref])
        .current_dir(repo_path)
        .output()?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     只有已经合入目标基线的旧分支才可以由 Workbench 自动删除，避免覆盖用户尚未合并的工作。
///
/// Code Logic（这个函数做什么）:
///     执行 `git merge-base --is-ancestor <branch> <base>`；退出码 0 表示 branch 已被 base 包含，1 表示未合并。
fn local_branch_merged_into(
    repo_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<bool, AppError> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", branch, base_ref])
        .current_dir(repo_path)
        .output()?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 自动清理已完成 worktree 时，也应释放对应本地分支名，避免用户下次创建同名工作区失败。
///
/// Code Logic（这个函数做什么）:
///     若本地分支存在且已合入 base_ref，则用 `git branch -D <branch>` 删除；未合并时返回业务错误。
pub fn delete_local_branch_if_merged(
    repo_path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<bool, AppError> {
    let branch = branch.trim();
    if branch.is_empty() || !local_branch_exists(repo_path, branch)? {
        return Ok(false);
    }
    if !local_branch_merged_into(repo_path, branch, base_ref)? {
        return Err(AppError::generic(format!(
            "本地分支 {branch} 已存在且尚未合并到 {base_ref}，请换一个分支名，或先手动处理该分支"
        )));
    }
    run_git(repo_path, &["branch", "-D", branch])?;
    Ok(true)
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
///     linked worktree 的 `--show-toplevel` 返回 worktree 自身路径，不能用来判断是否属于同一仓库；
///     归属校验必须比较共享的 git 对象库目录。
///
/// Code Logic（这个函数做什么）:
///     调用 `git rev-parse --git-common-dir`，规范化为绝对 canonical 路径后返回。
pub fn git_common_dir(path: &Path) -> Result<PathBuf, AppError> {
    let output = run_git(path, &["rev-parse", "--git-common-dir"])?;
    let common = output.trim();
    if common.is_empty() {
        return Err(AppError::generic("无法解析 git common dir"));
    }
    let raw = PathBuf::from(common);
    let absolute = if raw.is_absolute() {
        raw
    } else {
        path.join(raw)
    };
    Ok(absolute.canonicalize().unwrap_or(absolute))
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 需要展示当前项目下 Git 已知的全部 worktree，便于和本地记录对齐。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree list --porcelain` 后交给 parse_worktree_porcelain 解析。
pub fn list_worktrees(repo_path: &Path, main_path: &str) -> Result<Vec<ParsedWorktree>, AppError> {
    let output = run_git(repo_path, &["worktree", "list", "--porcelain"])?;
    Ok(parse_worktree_porcelain(&output, main_path))
}

/// Business Logic（为什么需要这个函数）:
///     Retry/崩溃恢复复用确定性 worktree 路径时，不能仅靠 is_dir 或 DB 记录：slug 碰撞、
///     失败的 `git worktree add` 残留目录、symlink 都可能让 Runner 在错误甚至非 Git 目录启动。
///
/// Code Logic（这个函数做什么）:
///     1) 拒绝 symlink/reparse 与非目录；
///     2) 在 owning repo 上执行 `git worktree list --porcelain`；
///     3) 要求存在 canonical path 匹配项，且 branch 与请求分支一致；
///     4) 比较 owning repo 与 worktree 的 canonical `git rev-parse --git-common-dir`
///        （linked worktree 的 toplevel 是自身，不能用 show-toplevel 做归属判断）。
///     未注册残留目录 / 分支不匹配 / 跨仓路径 → AppError（conflict 语义用 Bad）。
pub fn verify_registered_worktree(
    owning_repo: &Path,
    worktree_path: &Path,
    expected_branch: &str,
) -> Result<PathBuf, AppError> {
    let expected_branch = expected_branch.trim();
    if expected_branch.is_empty() {
        return Err(AppError::generic("分支名不能为空"));
    }
    let meta = std::fs::symlink_metadata(worktree_path).map_err(|err| {
        AppError::generic(format!(
            "读取 worktree 路径失败 {}: {err}",
            worktree_path.display()
        ))
    })?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Err(AppError::conflict(format!(
            "目标 worktree 路径是符号链接，拒绝复用: {}",
            worktree_path.display()
        )));
    }
    if !ft.is_dir() {
        return Err(AppError::conflict(format!(
            "目标 worktree 路径不是目录: {}",
            worktree_path.display()
        )));
    }

    let expected_canon = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let owning_canon = owning_repo
        .canonicalize()
        .unwrap_or_else(|_| owning_repo.to_path_buf());
    let owning_str = owning_canon.to_string_lossy().to_string();
    let listed = list_worktrees(owning_repo, &owning_str)?;
    let matched = listed.into_iter().find(|item| {
        let item_path = Path::new(&item.path);
        let item_canon = item_path
            .canonicalize()
            .unwrap_or_else(|_| item_path.to_path_buf());
        item_canon == expected_canon
            || item.path.trim_end_matches('/')
                == expected_canon.to_string_lossy().trim_end_matches('/')
            || item.path == worktree_path.to_string_lossy()
    });
    let Some(item) = matched else {
        return Err(AppError::conflict(format!(
            "目标路径不是 owning 仓库已注册的 Git worktree: {}",
            worktree_path.display()
        )));
    };
    let actual_branch = item
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("");
    if actual_branch != expected_branch {
        return Err(AppError::conflict(format!(
            "目标 worktree 分支不匹配: 期望 {expected_branch}，实际 {}",
            if actual_branch.is_empty() {
                "<detached/unknown>".to_string()
            } else {
                actual_branch.to_string()
            }
        )));
    }

    // 二次确认：owning repo 与 worktree 必须共享同一 git-common-dir（支持 linked worktree）。
    let owning_common = git_common_dir(&owning_canon)?;
    let wt_common = git_common_dir(&expected_canon)?;
    if owning_common != wt_common {
        return Err(AppError::conflict(format!(
            "目标 worktree 不属于当前项目仓库: {} (owning_common={}, wt_common={})",
            worktree_path.display(),
            owning_common.display(),
            wt_common.display()
        )));
    }
    Ok(expected_canon)
}

/// Business Logic（为什么需要这个函数）:
///     顶部 worktree strip 需要显示每个工作区的分支、变更数、领先/落后与冲突数。
///
/// Code Logic（这个函数做什么）:
///     执行 `git status --porcelain --branch`，并解析为 WorkbenchGitStatusDto。
pub fn status(path: &Path) -> Result<WorkbenchGitStatusDto, AppError> {
    let output = run_git(path, &["status", "--porcelain", "--branch"])?;
    let mut status = parse_status_porcelain(&output);
    status.can_push = can_push_from_status(path, &status);
    Ok(status)
}

/// Business Logic（为什么需要这个函数）:
///     Workbench 右侧 Git 历史 tab 需要读取当前 active worktree 的最近提交。
///
/// Code Logic（这个函数做什么）:
///     先确认目录是 Git 工作区；空仓库没有 HEAD 时返回空列表，否则执行 `git log` 并解析为 DTO。
pub fn list_commits(path: &Path, limit: usize) -> Result<Vec<WorkbenchGitCommitDto>, AppError> {
    run_git(path, &["rev-parse", "--is-inside-work-tree"])?;
    let head = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(path)
        .output()?;
    if !head.status.success() {
        return Ok(Vec::new());
    }
    let safe_limit = limit.clamp(1, 100).to_string();
    let output = run_git(
        path,
        &[
            "log",
            "--all",
            "--topo-order",
            "--decorate=full",
            "--date=iso-strict",
            "-n",
            &safe_limit,
            "--pretty=format:%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%D",
        ],
    )?;
    Ok(parse_git_log_output(&output))
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
///     Orchestrator delivery evidence 需要记录 commit 阶段前后的 HEAD，以便用户确认自动提交是否产生了新提交。
///
/// Code Logic（这个函数做什么）:
///     执行 `git rev-parse --verify HEAD`；有 HEAD 时返回完整 hash，空仓库或 unborn HEAD 返回 None，其它 Git 错误转 AppError。
pub fn head_hash(path: &Path) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(path)
        .output()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!hash.is_empty()).then_some(hash));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     mutation ledger 的 commit intent 需要 staged tree hash（不是 message），用于 unknown 后精确对账。
///
/// Code Logic（这个函数做什么）:
///     执行 `git write-tree`，返回当前 index 的 tree object hash。
pub fn write_tree_hash(path: &Path) -> Result<String, AppError> {
    let output = run_git(path, &["write-tree"])?;
    let hash = output.trim().to_string();
    if hash.is_empty() {
        return Err(AppError::generic("git write-tree 返回空 hash".to_string()));
    }
    Ok(hash)
}

/// Business Logic（为什么需要这个函数）:
///     commit confirm 需要 newHead.parent 与 beforeHead 比较。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse HEAD^`；无父/空仓库返回 None。
#[allow(dead_code)] // N3 MutationAuthoritySnapshot 采集 helper；当前前端对账，后端 pure confirm 单测/后续复用
pub fn head_parent_hash(path: &Path) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD^"])
        .current_dir(path)
        .output()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!hash.is_empty()).then_some(hash));
    }
    // unborn / root commit / missing parent
    if output.status.code() == Some(128) || output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     commit confirm 需要 newHead.tree == expectedTree。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse HEAD^{tree}`；无 HEAD 返回 None。
pub fn head_tree_hash(path: &Path) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "HEAD^{tree}"])
        .current_dir(path)
        .output()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!hash.is_empty()).then_some(hash));
    }
    if output.status.code() == Some(128) || output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     push intent 需要捕获 local/remote ref 与 local HEAD。
///
/// Code Logic（这个函数做什么）:
///     返回 (local_ref, remote_ref_name, local_head)；remote_ref 优先 upstream，否则 origin/<branch>。
pub fn push_ref_identity(path: &Path, branch: &str) -> Result<(String, String, String), AppError> {
    let local_head = head_hash(path)?
        .ok_or_else(|| AppError::generic("当前 worktree 没有可推送的 HEAD".to_string()))?;
    let local_ref = format!("refs/heads/{branch}");
    // 优先 @{upstream} 的 remote tracking ref；失败回退 origin/<branch>
    let upstream = Command::new("git")
        .args([
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ])
        .current_dir(path)
        .output()?;
    let remote_ref = if upstream.status.success() {
        let name = String::from_utf8_lossy(&upstream.stdout).trim().to_string();
        if name.is_empty() {
            format!("refs/remotes/origin/{branch}")
        } else if name.starts_with("refs/") {
            name
        } else {
            // 形如 origin/feature/x
            format!("refs/remotes/{name}")
        }
    } else {
        format!("refs/remotes/origin/{branch}")
    };
    Ok((local_ref, remote_ref, local_head))
}

/// Business Logic（为什么需要这个函数）:
///     push confirm 需要 remote ref 是否已到达 local HEAD。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse --verify <remote_ref>`；不存在返回 None。
#[allow(dead_code)] // N3 MutationAuthoritySnapshot 采集 helper；当前前端对账，后端 pure confirm 单测/后续复用
pub fn rev_parse_ref(path: &Path, git_ref: &str) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", git_ref])
        .current_dir(path)
        .output()?;
    if output.status.success() {
        let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!hash.is_empty()).then_some(hash));
    }
    if output.status.code() == Some(128) || output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     merge confirm 需要 main 是否包含 source HEAD。
///
/// Code Logic（这个函数做什么）:
///     `git merge-base --is-ancestor <source_head> HEAD` 在 main 路径执行；0=true，1=false。
#[allow(dead_code)] // N3 MutationAuthoritySnapshot 采集 helper；当前前端对账，后端 pure confirm 单测/后续复用
pub fn is_ancestor(main_path: &Path, source_head: &str) -> Result<bool, AppError> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", source_head, "HEAD"])
        .current_dir(main_path)
        .output()?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     用户输入分支名后，Workbench 需要在本机创建对应 Git worktree 和新分支。
///
/// Code Logic（这个函数做什么）:
///     先清理已合并的同名旧本地分支，再执行 `git worktree add -b <branch> <path> <base>`；base 为空时使用 HEAD。
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
    delete_local_branch_if_merged(repo_path, branch, base_ref)?;
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
///
/// 生产 delivery 走 ledger `local_commit_workbench_worktree`；本 helper 供 delivery 单测 harness 与
/// 简单 stage+commit 场景复用。
#[cfg(test)]
pub fn commit_all(path: &Path, message: &str) -> Result<bool, AppError> {
    if !stage_all_for_commit(path)? {
        return Ok(false);
    }
    commit_staged(path, message)?;
    Ok(true)
}

/// Business Logic（为什么需要这个函数）:
///     Commit 按钮需要把所有本地改动纳入本次提交，包括删除、修改和未跟踪文件。
///
/// Code Logic（这个函数做什么）:
///     执行 `git add -A` 后读取 `git status --porcelain`，返回是否存在待提交改动。
pub fn stage_all_for_commit(path: &Path) -> Result<bool, AppError> {
    run_git(path, &["add", "-A"])?;
    let pending = run_git(path, &["status", "--porcelain"])?;
    Ok(!pending.trim().is_empty())
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 生成 commit message 时应基于 staged diff，而不是基于可能变化的工作区状态。
///
/// Code Logic（这个函数做什么）:
///     读取 `git diff --cached --stat` 和 `git diff --cached`；diff 正文超过上限时按字符截断并标记。
pub fn staged_changes_for_commit_message(path: &Path) -> Result<StagedCommitChanges, AppError> {
    let stat = run_git(
        path,
        &["diff", "--cached", "--stat", "--no-ext-diff", "--no-color"],
    )?;
    let diff = run_git(path, &["diff", "--cached", "--no-ext-diff", "--no-color"])?;
    let (diff, truncated) = truncate_for_commit_message(&diff);
    Ok(StagedCommitChanges {
        stat: stat.trim().to_string(),
        diff,
        truncated,
    })
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 输出可能包含代码围栏、首尾空白或空文本，Git commit 前必须归一化。
///
/// Code Logic（这个函数做什么）:
///     去掉 markdown 代码围栏和首尾空白；清洗后为空则返回业务错误。
pub fn sanitize_commit_message(message: &str) -> Result<String, AppError> {
    let mut lines = message.trim().lines().collect::<Vec<_>>();
    if lines
        .first()
        .map(|line| line.trim_start().starts_with("```"))
        .unwrap_or(false)
    {
        lines.remove(0);
        if lines
            .last()
            .map(|line| line.trim() == "```")
            .unwrap_or(false)
        {
            lines.pop();
        }
    }
    let cleaned = lines.join("\n").trim().replace("\r\n", "\n");
    if cleaned.trim().is_empty() {
        return Err(AppError::generic("Commit message 不能为空"));
    }
    Ok(cleaned)
}

/// Business Logic（为什么需要这个函数）:
///     AI 或手写 message 准备好后，Workbench 需要提交当前 staged 改动。
///
/// Code Logic（这个函数做什么）:
///     清洗 message 后执行 `git commit -m <message>`；不再重新 stage，避免 message 与 diff 不一致。
pub fn commit_staged(path: &Path, message: &str) -> Result<(), AppError> {
    let cleaned = sanitize_commit_message(message)?;
    run_git(path, &["commit", "-m", &cleaned])?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Orchestrator 交付必须把“已审 tree”冻结成不可变 commit OID，不能再从可变 HEAD 二次推导意图。
///
/// Code Logic（这个函数做什么）:
///     `git commit-tree <tree> [-p parent] -m msg` 生成 OID，再以
///     `git update-ref refs/heads/<branch> <new> <old>` CAS 更新分支；并发提交会 CAS 失败。
///     `expected_parent` 由调用方在 stage/digest 前捕获，禁止函数内再读可变 HEAD 作 parent。
///     返回新 commit OID（完整 hash）。
pub fn commit_frozen_tree(
    path: &Path,
    tree_oid: &str,
    message: &str,
    expected_parent: Option<&str>,
) -> Result<String, AppError> {
    if tree_oid.trim().is_empty() {
        return Err(AppError::generic("tree oid 不能为空"));
    }
    let cleaned = sanitize_commit_message(message)?;
    let mut args: Vec<String> = vec![
        "commit-tree".into(),
        tree_oid.trim().into(),
        "-m".into(),
        cleaned,
    ];
    if let Some(p) = expected_parent.map(str::trim).filter(|p| !p.is_empty()) {
        args.push("-p".into());
        args.push(p.to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let new_oid = run_git(path, &arg_refs)?.trim().to_string();
    if new_oid.is_empty() {
        return Err(AppError::generic("git commit-tree 返回空 commit oid"));
    }
    let committed_tree = commit_tree_hash(path, &new_oid)?;
    if committed_tree != tree_oid.trim() {
        return Err(AppError::generic(format!(
            "commit-tree tree 绑定失败: expected={tree_oid}, got={committed_tree}"
        )));
    }
    let branch =
        current_branch(path).ok_or_else(|| AppError::generic("当前 worktree 没有可更新的分支"))?;
    let refname = format!("refs/heads/{branch}");
    match expected_parent.map(str::trim).filter(|p| !p.is_empty()) {
        Some(old) => {
            run_git(path, &["update-ref", &refname, &new_oid, old])?;
        }
        None => {
            run_git(
                path,
                &[
                    "update-ref",
                    &refname,
                    &new_oid,
                    "0000000000000000000000000000000000000000",
                ],
            )
            .or_else(|_| run_git(path, &["update-ref", &refname, &new_oid]))?;
        }
    }
    Ok(new_oid)
}

/// Business Logic（为什么需要这个函数）:
///     交付意图必须绑定具体 commit 对象的 tree，而不是再次读取可变 HEAD。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse <commit>^{tree}`，返回完整 tree hash。
pub fn commit_tree_hash(path: &Path, commit_oid: &str) -> Result<String, AppError> {
    if commit_oid.trim().is_empty() {
        return Err(AppError::generic("commit oid 不能为空"));
    }
    let rev = format!("{}^{{tree}}", commit_oid.trim());
    let hash = run_git(path, &["rev-parse", "--verify", &rev])?
        .trim()
        .to_string();
    if hash.is_empty() {
        return Err(AppError::generic(format!(
            "无法解析 commit tree: {commit_oid}"
        )));
    }
    Ok(hash)
}

/// Business Logic（为什么需要这个函数）:
///     自动交付必须推送“已审 commit OID”，禁止按分支 tip 再解析导致未审提交被推送。
///
/// Code Logic（这个函数做什么）:
///     解析 push target；执行 `git push <remote> <commit_oid>:refs/heads/<branch>`
///     （有 upstream 时同样用显式 refspec，避免跟随可变 HEAD）。
pub fn push_commit_oid(path: &Path, branch: &str, commit_oid: &str) -> Result<(), AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("当前 worktree 没有可推送的分支"));
    }
    if commit_oid.trim().is_empty() {
        return Err(AppError::generic("commit oid 不能为空"));
    }
    let (remote, remote_ref) = resolve_push_remote_and_ref(path, branch)?;
    let refspec = format!("{}:{}", commit_oid.trim(), remote_ref);
    run_git(path, &["push", &remote, &refspec])?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     OID push 必须推到真实 upstream 目标 ref，而不是假设远程也叫同名 local branch。
///
/// Code Logic（这个函数做什么）:
///     有 upstream 时解析 `remote` + `branch.<name>.merge`；否则 origin + refs/heads/<branch>。
fn resolve_push_remote_and_ref(path: &Path, branch: &str) -> Result<(String, String), AppError> {
    match resolve_push_target(path)? {
        PushTarget::Upstream => {
            let remote = upstream_remote_name(path)
                .ok_or_else(|| AppError::generic("无法解析 upstream remote"))?;
            let merge_key = format!("branch.{}.merge", branch.trim());
            let merge_ref = run_git(path, &["config", "--get", &merge_key])
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| format!("refs/heads/{}", branch.trim()));
            let remote_ref = if merge_ref.starts_with("refs/") {
                merge_ref
            } else {
                format!("refs/heads/{merge_ref}")
            };
            Ok((remote, remote_ref))
        }
        PushTarget::Remote(remote) => Ok((remote, format!("refs/heads/{}", branch.trim()))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     merge 成功后主分支 tip 也必须按固定 OID 推送，禁止再读可变 current branch tip。
///
/// Code Logic（这个函数做什么）:
///     读取当前分支名，调用 `push_commit_oid` 推送给定 main commit OID。
pub fn push_main_commit_oid(path: &Path, commit_oid: &str) -> Result<String, AppError> {
    let branch =
        current_branch(path).ok_or_else(|| AppError::generic("主工作区没有可推送的当前分支"))?;
    push_commit_oid(path, &branch, commit_oid)?;
    Ok(branch)
}

/// 解析当前分支 upstream 的 remote 名（如 origin）。
///
/// Business Logic（为什么需要这个函数）:
///     显式 OID push 需要 remote 名，而不能依赖 `git push` 无参数时的隐式 HEAD 跟随。
///
/// Code Logic（这个函数做什么）:
///     `git rev-parse --abbrev-ref @{u}` → 取 `remote/branch` 前缀；失败返回 None。
fn upstream_remote_name(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "@{u}"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let upstream = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let remote = upstream.split('/').next()?.to_string();
    (!remote.is_empty()).then_some(remote)
}

/// Business Logic（为什么需要这个函数）:
///     自动交付 merge 必须合并已审 commit OID，禁止 `git merge <branch>` 解析到漂移 tip。
///
/// Code Logic（这个函数做什么）:
///     执行 `git merge --no-ff <commit_oid>`；成功 Merged，冲突 Conflicted，其它错误 AppError。
pub fn merge_commit_oid(
    main_path: &Path,
    commit_oid: &str,
) -> Result<MergeBranchOutcome, AppError> {
    if commit_oid.trim().is_empty() {
        return Err(AppError::generic("commit oid 不能为空"));
    }
    let output = Command::new("git")
        .args(["merge", "--no-ff", commit_oid.trim()])
        .current_dir(main_path)
        .output()?;
    if output.status.success() {
        return Ok(MergeBranchOutcome::Merged);
    }
    if unresolved_conflict_files(main_path)
        .map(|files| !files.is_empty())
        .unwrap_or(false)
    {
        return Ok(MergeBranchOutcome::Conflicted);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     大型 diff 不能完整塞给 Claude CLI，否则容易超时或超出上下文。
///
/// Code Logic（这个函数做什么）:
///     按 Unicode scalar 截断 diff，返回截断文本与是否截断。
fn truncate_for_commit_message(diff: &str) -> (String, bool) {
    let mut chars = diff.chars();
    let truncated = chars
        .by_ref()
        .take(MAX_COMMIT_DIFF_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        (truncated, true)
    } else {
        (diff.to_string(), false)
    }
}

/// Business Logic（为什么需要这个函数）:
///     用户完成 worktree commit 后，需要把对应分支推送到远端以便协作或备份。
///
/// Code Logic（这个函数做什么）:
///     已有 upstream 时执行普通 `git push`；否则只选择 origin 执行 `git push -u origin <branch>`。
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
///     Orchestrator 自动 merge 成功后，源任务 worktree 会被清理，主分支推送必须从主工作区 cwd 读取当前分支并执行。
///
/// Code Logic（这个函数做什么）:
///     在传入的主工作区路径读取当前分支名，再复用 push_branch 的 upstream/origin 安全规则推送，成功返回分支名供 evidence 记录。
pub fn push_main_worktree_current_branch(path: &Path) -> Result<String, AppError> {
    let branch =
        current_branch(path).ok_or_else(|| AppError::generic("主工作区没有可推送的当前分支"))?;
    push_branch(path, &branch)?;
    Ok(branch)
}

/// Business Logic（为什么需要这个函数）:
///     Push 按钮不能把 fork 的 upstream remote 当作用户自己的发布 remote。
///
/// Code Logic（这个函数做什么）:
///     若当前分支已有 upstream，返回 Upstream；否则只在存在 origin 时返回 Remote("origin")。
fn resolve_push_target(path: &Path) -> Result<PushTarget, AppError> {
    if has_upstream(path) {
        return Ok(PushTarget::Upstream);
    }

    let remotes = list_remotes(path)?;
    if remotes.is_empty() {
        return Err(AppError::generic(
            "当前分支没有 upstream，且 Git 仓库没有配置 origin remote，无法推送。请先在项目目录执行 `git remote add origin <url>`，或设置当前分支 upstream 后重试。",
        ));
    }
    if remotes.iter().any(|remote| remote == "origin") {
        return Ok(PushTarget::Remote("origin".to_string()));
    }

    Err(AppError::generic(format!(
        "当前分支没有 upstream，且 Git 仓库没有 origin remote（现有 remote：{}），无法判断安全的发布目标。请先设置当前分支 upstream，或添加 origin 后重试。",
        remotes.join(", ")
    )))
}

/// Business Logic（为什么需要这个函数）:
///     本地未发布仓库没有 remote 时，Workbench 的 Push 按钮应直接禁用，而不是等用户点击后报错。
///
/// Code Logic（这个函数做什么）:
///     当前 status 有分支且 resolve_push_target 能找到 upstream/origin 时返回 true。
fn can_push_from_status(path: &Path, status: &WorkbenchGitStatusDto) -> bool {
    status
        .branch
        .as_deref()
        .map(|branch| !branch.trim().is_empty() && resolve_push_target(path).is_ok())
        .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     已经跟踪远端分支的 worktree 应复用用户现有 upstream 配置。
///
/// Code Logic（这个函数做什么）:
///     执行 `git rev-parse --abbrev-ref --symbolic-full-name @{u}`，成功且输出非空即视为存在 upstream。
fn has_upstream(path: &Path) -> bool {
    run_git(
        path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
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
///     用户希望在 Workbench 中把功能 worktree 合并回主工作区所在分支；如果发生冲突，
///     后端还需要保留 merge 状态供 Claude Code 继续处理。
///
/// Code Logic（这个函数做什么）:
///     在主工作区路径执行 `git merge --no-ff <branch>`；成功返回 Merged，非零退出后若检测到
///     unmerged path 则返回 Conflicted，否则返回普通 Git 错误。
pub fn merge_branch(main_path: &Path, branch: &str) -> Result<MergeBranchOutcome, AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("当前 worktree 没有可合并的分支"));
    }
    let output = Command::new("git")
        .args(["merge", "--no-ff", branch])
        .current_dir(main_path)
        .output()?;
    if output.status.success() {
        return Ok(MergeBranchOutcome::Merged);
    }
    if unresolved_conflict_files(main_path)
        .map(|files| !files.is_empty())
        .unwrap_or(false)
    {
        return Ok(MergeBranchOutcome::Conflicted);
    }
    Err(AppError::generic(format!(
        "Git 命令失败: {}",
        git_failure_message(&output)
    )))
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 冲突解决阶段需要知道当前有哪些 Git unmerged 文件，才能构造有限、明确的输入。
///
/// Code Logic（这个函数做什么）:
///     执行 `git diff --name-only --diff-filter=U -z`，按 NUL 分隔解析未解决冲突文件相对路径。
pub fn unresolved_conflict_files(path: &Path) -> Result<Vec<String>, AppError> {
    let output = run_git(path, &["diff", "--name-only", "--diff-filter=U", "-z"])?;
    Ok(output
        .split('\0')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect())
}

/// Business Logic（为什么需要这个函数）:
///     merge 冲突自动解决失败时，后端需要判断是否还能安全执行 `git merge --abort`。
///
/// Code Logic（这个函数做什么）:
///     用 `git rev-parse -q --verify MERGE_HEAD` 判断当前仓库是否处于 merge 进行中。
pub fn merge_in_progress(path: &Path) -> bool {
    run_git(path, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
        .map(|output| !output.trim().is_empty())
        .unwrap_or(false)
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 仍无法解决冲突时，后端应尽量回滚主工作区 merge 状态，避免用户项目卡在半合并状态。
///
/// Code Logic（这个函数做什么）:
///     当前存在 MERGE_HEAD 时执行 `git merge --abort`；没有 merge 状态时直接 no-op。
pub fn abort_merge(path: &Path) -> Result<(), AppError> {
    if merge_in_progress(path) {
        run_git(path, &["merge", "--abort"])?;
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     Claude Code 改写冲突文件后，后端需要把解决结果加入 index，并在仍有冲突时给出明确错误。
///
/// Code Logic（这个函数做什么）:
///     执行 `git add -A` 后重新读取 unmerged 文件；若仍有冲突则返回包含文件列表的业务错误。
pub fn stage_all_merge_resolution(path: &Path) -> Result<(), AppError> {
    run_git(path, &["add", "-A"])?;
    let remaining = unresolved_conflict_files(path)?;
    if !remaining.is_empty() {
        return Err(AppError::generic(format!(
            "仍有未解决的 merge 冲突: {}",
            remaining.join(", ")
        )));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     冲突被自动解决后，用户不应再手动执行 git commit；后端应完成 Git 已准备好的 merge commit。
///
/// Code Logic（这个函数做什么）:
///     当前存在 MERGE_HEAD 时执行 `git commit --no-edit` 使用 Git 生成的 merge message；
///     没有 merge 状态时视为无需提交并 no-op。
pub fn commit_merge_no_edit(path: &Path) -> Result<(), AppError> {
    if merge_in_progress(path) {
        run_git(path, &["commit", "--no-edit"])?;
    }
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
///     Git log 原始文本不适合直接给 UI；右侧历史 tab 需要结构化提交项和 refs。
///
/// Code Logic（这个函数做什么）:
///     按行读取，每行用 ASCII unit separator 拆成 8 个字段；字段不足的异常行跳过。
pub fn parse_git_log_output(output: &str) -> Vec<WorkbenchGitCommitDto> {
    output
        .lines()
        .filter_map(|line| {
            let fields = line.split('\x1f').collect::<Vec<_>>();
            if fields.len() < 8 {
                return None;
            }
            Some(WorkbenchGitCommitDto {
                hash: fields[0].to_string(),
                short_hash: fields[1].to_string(),
                parent_hashes: fields[2]
                    .split_whitespace()
                    .map(ToString::to_string)
                    .collect(),
                author_name: fields[3].to_string(),
                author_email: fields[4].to_string(),
                authored_at: fields[5].to_string(),
                summary: fields[6].to_string(),
                refs: parse_git_refs(fields[7]),
            })
        })
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     Git 历史树需要把 Git decoration 转成可标识本地/云端的稳定标签。
///
/// Code Logic（这个函数做什么）:
///     解析 `%D` 输出，识别 HEAD 指向、本地分支、远端分支、tag 和其他 ref。
fn parse_git_refs(raw: &str) -> Vec<WorkbenchGitRefDto> {
    raw.split(',')
        .filter_map(|item| parse_git_ref(item.trim()))
        .collect()
}

/// Business Logic（为什么需要这个函数）:
///     单个 Git decoration 可能是 `HEAD -> refs/heads/main` 或普通 ref，需要统一归一化。
///
/// Code Logic（这个函数做什么）:
///     去掉 symbolic ref 左侧，把完整 ref 转为展示名、类型和远端名。
fn parse_git_ref(raw: &str) -> Option<WorkbenchGitRefDto> {
    if raw.is_empty() {
        return None;
    }
    if raw == "HEAD" {
        return Some(WorkbenchGitRefDto {
            name: "HEAD".to_string(),
            full_name: "HEAD".to_string(),
            kind: WorkbenchGitRefKindDto::Head,
            remote: None,
            is_head: true,
        });
    }

    let (target, is_head) = if let Some(rest) = raw.strip_prefix("HEAD -> ") {
        (rest.trim(), true)
    } else if let Some((_, target)) = raw.split_once(" -> ") {
        (target.trim(), false)
    } else {
        (raw, false)
    };
    let target = target.strip_prefix("tag: ").unwrap_or(target).trim();

    if let Some(name) = target.strip_prefix("refs/heads/") {
        return Some(WorkbenchGitRefDto {
            name: name.to_string(),
            full_name: target.to_string(),
            kind: WorkbenchGitRefKindDto::Local,
            remote: None,
            is_head,
        });
    }
    if let Some(name) = target.strip_prefix("refs/remotes/") {
        let remote = name.split('/').next().filter(|value| !value.is_empty());
        return Some(WorkbenchGitRefDto {
            name: name.to_string(),
            full_name: target.to_string(),
            kind: WorkbenchGitRefKindDto::Remote,
            remote: remote.map(ToString::to_string),
            is_head,
        });
    }
    if let Some(name) = target.strip_prefix("refs/tags/") {
        return Some(WorkbenchGitRefDto {
            name: name.to_string(),
            full_name: target.to_string(),
            kind: WorkbenchGitRefKindDto::Tag,
            remote: None,
            is_head,
        });
    }

    Some(WorkbenchGitRefDto {
        name: target.to_string(),
        full_name: target.to_string(),
        kind: WorkbenchGitRefKindDto::Other,
        remote: None,
        is_head,
    })
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
    ///     用户通过 Workbench 合并并清理旧 worktree 后，Git 本地分支可能残留；再次创建同名 worktree 不应报 branch already exists。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建已合入 HEAD 的同名旧分支，调用 create_worktree，断言后端会清理旧分支并基于当前 HEAD 重建 worktree。
    #[test]
    fn create_worktree_reuses_name_after_merged_branch_cleanup() {
        let root = temp_git_dir("workbench-create-stale-branch");
        let repo = root.join("repo");
        let worktree = root.join("feature-test-worktree");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(&repo, &["checkout", "-b", "feature/test"]);
        fs::write(repo.join("feature.txt"), "feature\n").expect("write feature");
        git_test_command(&repo, &["add", "feature.txt"]);
        git_test_command(&repo, &["commit", "-m", "feature"]);
        git_test_command(&repo, &["checkout", "main"]);
        git_test_command(
            &repo,
            &["merge", "--no-ff", "feature/test", "-m", "merge feature"],
        );
        let merged_head = git_test_command(&repo, &["rev-parse", "HEAD"])
            .trim()
            .to_string();

        create_worktree(&repo, &worktree, "feature/test", Some("HEAD"))
            .expect("create worktree from reusable branch name");

        let worktree_head = git_test_command(&worktree, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        assert_eq!(worktree_head, merged_head);
        assert_eq!(
            git_test_command(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "feature/test"
        );

        let _ = remove_worktree(&repo, &worktree, true);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本地未发布仓库没有 remote 时，Workbench Push 按钮应该禁用，避免用户点击后才看到 fatal。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建无 remote 的真实 Git 仓库，断言 status 派生 can_push=false。
    #[test]
    fn status_marks_can_push_false_without_remote() {
        let root = temp_git_dir("workbench-status-no-remote");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "feature/local-only"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);

        let status = status(&repo).expect("read status");

        assert_eq!(status.branch.as_deref(), Some("feature/local-only"));
        assert!(!status.can_push);

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     已配置 remote 的本地分支允许首次 push，让后端用 `git push -u` 建立 upstream。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库和 bare origin remote，断言 status 派生 can_push=true。
    #[test]
    fn status_marks_can_push_true_with_origin_remote() {
        let root = temp_git_dir("workbench-status-origin-remote");
        let repo = root.join("repo");
        let remote = root.join("origin.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "feature/publishable"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(
            &root,
            &["init", "--bare", remote.to_string_lossy().as_ref()],
        );
        git_test_command(
            &repo,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );

        let status = status(&repo).expect("read status");

        assert_eq!(status.branch.as_deref(), Some("feature/publishable"));
        assert!(status.can_push);

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     fork/upstream-only 仓库没有发布到用户自己的 remote 时，Push 按钮不能误判为可推送。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建只有 `excalidraw-upstream` remote 且分支无 upstream 的真实仓库，断言 can_push=false。
    #[test]
    fn status_marks_can_push_false_with_upstream_only_remote() {
        let root = temp_git_dir("workbench-status-upstream-only");
        let repo = root.join("repo");
        let remote = root.join("excalidraw-upstream.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(
            &root,
            &["init", "--bare", remote.to_string_lossy().as_ref()],
        );
        git_test_command(
            &repo,
            &[
                "remote",
                "add",
                "excalidraw-upstream",
                remote.to_string_lossy().as_ref(),
            ],
        );

        let status = status(&repo).expect("read status");

        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(!status.can_push);

        let _ = fs::remove_dir_all(root);
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
    ///     Retry/崩溃恢复必须能复用合法 linked worktree，不能因 toplevel!=main 误判跨仓。
    ///
    /// Code Logic（这个测试做什么）:
    ///     真实临时仓库创建 linked worktree 后调用 verify_registered_worktree，断言 Ok。
    #[test]
    fn verify_registered_worktree_accepts_linked_worktree() {
        let root = temp_git_dir("workbench-verify-linked-wt");
        let repo = root.join("repo");
        let worktree = root.join("linked-wt");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        create_worktree(&repo, &worktree, "feature/reuse", Some("HEAD"))
            .expect("create linked worktree");

        let verified = verify_registered_worktree(&repo, &worktree, "feature/reuse")
            .expect("linked worktree must verify");
        assert_eq!(
            verified,
            worktree.canonicalize().unwrap_or(worktree.clone())
        );

        let _ = remove_worktree(&repo, &worktree, true);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     跨仓路径绝不能被当成当前项目 worktree 复用，否则会在错误仓库启动 Runner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两个独立仓库；对 A 校验 B 的路径，断言 conflict。
    #[test]
    fn verify_registered_worktree_rejects_cross_repo_path() {
        let root = temp_git_dir("workbench-verify-cross-repo");
        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        for repo in [&repo_a, &repo_b] {
            fs::create_dir_all(repo).expect("create repo dir");
            git_test_command(repo, &["init"]);
            git_test_command(repo, &["checkout", "-b", "main"]);
            git_test_command(repo, &["config", "user.email", "test@example.com"]);
            git_test_command(repo, &["config", "user.name", "Workbench Test"]);
            fs::write(repo.join("README.md"), "base\n").expect("write base");
            git_test_command(repo, &["add", "README.md"]);
            git_test_command(repo, &["commit", "-m", "initial"]);
        }
        let worktree_b = root.join("b-wt");
        create_worktree(&repo_b, &worktree_b, "feature/b", Some("HEAD")).expect("create b wt");

        let err = verify_registered_worktree(&repo_a, &worktree_b, "feature/b")
            .expect_err("cross-repo must reject");
        let message = err.to_string();
        assert!(
            message.contains("不属于当前项目仓库")
                || message.contains("不是 owning 仓库已注册")
                || message.contains("conflict")
                || message.contains("冲突"),
            "unexpected error: {message}"
        );

        let _ = remove_worktree(&repo_b, &worktree_b, true);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户项目配置了 origin 但当前分支尚未设置 upstream 时，Workbench push 应能完成首次发布。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库和 bare origin remote，断言 push_branch 可以推送当前分支并设置 upstream。
    #[test]
    fn push_branch_uses_origin_remote_when_upstream_missing() {
        let root = temp_git_dir("workbench-push-origin-remote");
        let repo = root.join("repo");
        let remote = root.join("origin.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "feature/worktree-push"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(
            &root,
            &["init", "--bare", remote.to_string_lossy().as_ref()],
        );
        git_test_command(
            &repo,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );

        push_branch(&repo, "feature/worktree-push").expect("push with origin remote");
        git_test_command(
            &remote,
            &["rev-parse", "--verify", "refs/heads/feature/worktree-push"],
        );

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Orchestrator 自动交付在 task worktree 被清理后只能从主工作区推送当前主分支，不能依赖源 worktree。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库与 bare origin，在 main 上产生提交，调用 main push helper 并断言 origin/main 收到该提交。
    #[test]
    fn push_main_worktree_current_branch_pushes_current_branch() {
        let root = temp_git_dir("workbench-push-main-current-branch");
        let repo = root.join("repo");
        let remote = root.join("origin.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        git_test_command(
            &root,
            &["init", "--bare", remote.to_string_lossy().as_ref()],
        );
        git_test_command(
            &repo,
            &["remote", "add", "origin", remote.to_string_lossy().as_ref()],
        );
        fs::write(repo.join("README.md"), "main push\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "main change"]);

        let branch = push_main_worktree_current_branch(&repo).expect("push current main branch");

        assert_eq!(branch, "main");
        let remote_content = git_test_command(&remote, &["show", "refs/heads/main:README.md"]);
        assert_eq!(remote_content.trim(), "main push");

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
        assert!(message.contains("没有配置 origin remote"));
        assert!(message.contains("upstream"));
        assert!(message.contains("git remote add origin <url>"));

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     上游源码 remote 不等于用户自己的发布 remote，Workbench 不应默认把分支 push 到 upstream。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建只有 `excalidraw-upstream` remote 的真实仓库，断言 push_branch 返回配置 origin/upstream 的提示。
    #[test]
    fn push_branch_rejects_upstream_only_remote() {
        let root = temp_git_dir("workbench-push-upstream-only");
        let repo = root.join("repo");
        let remote = root.join("excalidraw-upstream.git");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(
            &root,
            &["init", "--bare", remote.to_string_lossy().as_ref()],
        );
        git_test_command(
            &repo,
            &[
                "remote",
                "add",
                "excalidraw-upstream",
                remote.to_string_lossy().as_ref(),
            ],
        );

        let err = push_branch(&repo, "main").expect_err("upstream-only remote should fail");
        let message = err.to_string();
        assert!(message.contains("origin"));
        assert!(message.contains("upstream"));

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     交付必须把 digest 门后的 frozen tree 冻结成固定 commit OID，且并发 tip 推进不能改变该 OID。
    ///
    /// Code Logic（这个测试做什么）:
    ///     stage + write-tree + commit_frozen_tree；再在工作区制造额外 commit 后断言
    ///     commit_tree_hash(reviewed) 仍等于 frozen tree，且 merge_commit_oid 合并的是 reviewed OID。
    #[test]
    fn commit_frozen_tree_and_merge_commit_oid_bind_immutable_review() {
        let root = temp_git_dir("workbench-frozen-oid");
        let main = root.join("main");
        let feature = root.join("feature");
        fs::create_dir_all(&main).expect("main dir");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/oid",
                feature.to_str().unwrap(),
            ],
        );
        fs::write(feature.join("feat.txt"), "v1\n").expect("feat");
        assert!(stage_all_for_commit(&feature).expect("stage"));
        let frozen = write_tree_hash(&feature).expect("write-tree");
        let parent = head_hash(&feature).expect("parent").expect("some parent");
        let reviewed = commit_frozen_tree(&feature, &frozen, "reviewed change", Some(&parent))
            .expect("commit-tree CAS");
        assert_eq!(
            commit_tree_hash(&feature, &reviewed).expect("tree of reviewed"),
            frozen
        );
        // 模拟并发 tip 推进：额外 commit 后 HEAD ≠ reviewed，但 reviewed tree 不变。
        fs::write(feature.join("extra.txt"), "sneak\n").expect("extra");
        git_test_command(&feature, &["add", "extra.txt"]);
        git_test_command(&feature, &["commit", "-m", "sneak"]);
        let head_now = head_hash(&feature).expect("head").expect("some");
        assert_ne!(head_now, reviewed);
        assert_eq!(
            commit_tree_hash(&feature, &reviewed).expect("reviewed still frozen"),
            frozen
        );
        // merge 按 OID：main 只应拿到 feat.txt，不应含 sneak。
        let outcome = merge_commit_oid(&main, &reviewed).expect("merge oid");
        assert_eq!(outcome, MergeBranchOutcome::Merged);
        assert!(main.join("feat.txt").exists());
        assert!(!main.join("extra.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Workbench 一键 merge 遇到冲突时不能把冲突当作普通 Git fatal 直接丢给用户；
    ///     后续阶段需要识别冲突文件并交给 Claude Code 尝试解决。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 冲突，断言 merge_branch 返回 Conflicted、保留 MERGE_HEAD，并能列出未解决文件。
    #[test]
    fn merge_branch_reports_conflict_for_claude_resolution() {
        let root = temp_git_dir("workbench-merge-conflict");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(&repo, &["checkout", "-b", "feature/conflict"]);
        fs::write(repo.join("README.md"), "feature\n").expect("write feature");
        git_test_command(&repo, &["commit", "-am", "feature change"]);
        git_test_command(&repo, &["checkout", "main"]);
        fs::write(repo.join("README.md"), "main\n").expect("write main");
        git_test_command(&repo, &["commit", "-am", "main change"]);

        let outcome = merge_branch(&repo, "feature/conflict").expect("merge should be classed");
        let conflicts = unresolved_conflict_files(&repo).expect("read conflicts");

        assert_eq!(outcome, MergeBranchOutcome::Conflicted);
        assert_eq!(conflicts, vec!["README.md"]);
        assert!(merge_in_progress(&repo));

        let _ = abort_merge(&repo);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude Code 成功改写冲突文件后，后端应自动 stage 并完成 merge commit，
    ///     用户不应再手动回到主 worktree 执行 git add/commit。
    ///
    /// Code Logic（这个测试做什么）:
    ///     手动模拟 Claude 已写入解决后的文件，执行 stage_all_merge_resolution + commit_merge_no_edit，
    ///     断言 merge 状态结束且 HEAD 是双父 merge commit。
    #[test]
    fn commit_merge_no_edit_completes_resolved_conflict_merge() {
        let root = temp_git_dir("workbench-merge-resolution");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "-b", "main"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "initial"]);
        git_test_command(&repo, &["checkout", "-b", "feature/conflict"]);
        fs::write(repo.join("README.md"), "feature\n").expect("write feature");
        git_test_command(&repo, &["commit", "-am", "feature change"]);
        git_test_command(&repo, &["checkout", "main"]);
        fs::write(repo.join("README.md"), "main\n").expect("write main");
        git_test_command(&repo, &["commit", "-am", "main change"]);

        assert_eq!(
            merge_branch(&repo, "feature/conflict").expect("merge outcome"),
            MergeBranchOutcome::Conflicted
        );
        fs::write(repo.join("README.md"), "main\nfeature\n").expect("write resolved");
        stage_all_merge_resolution(&repo).expect("stage resolution");
        assert!(unresolved_conflict_files(&repo)
            .expect("read conflicts")
            .is_empty());

        commit_merge_no_edit(&repo).expect("commit merge");
        let parents = git_test_command(&repo, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        let parent_count = parents.split_whitespace().count() - 1;

        assert!(!merge_in_progress(&repo));
        assert_eq!(parent_count, 2);

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     AI commit message 生成必须基于 commit 将实际包含的 staged diff，且要覆盖未跟踪文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库，新增未跟踪文件后 stage_all_for_commit，再断言 staged diff 摘要包含该文件。
    #[test]
    fn stage_all_for_commit_includes_untracked_files_in_staged_diff() {
        let root = temp_git_dir("workbench-commit-diff");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "hello\n").expect("write readme");

        assert!(stage_all_for_commit(&repo).expect("stage changes"));
        let diff = staged_changes_for_commit_message(&repo).expect("read staged diff");

        assert!(diff.stat.contains("README.md"));
        assert!(diff.diff.contains("hello"));

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude Code 可能返回带代码围栏或多余空白的文本，Git commit 前必须清洗成稳定 message。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入代码围栏包裹的 commit message，断言输出只保留真实 message 内容。
    #[test]
    fn sanitize_generated_commit_message_strips_code_fences() {
        let message = sanitize_commit_message(
            "```text\nfeat: add worktree commits\n\n- generate message\n```",
        )
        .expect("sanitize message");

        assert_eq!(message, "feat: add worktree commits\n\n- generate message");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空 AI 输出不能进入 git commit，否则用户会看到底层 Git 编辑器或失败信息。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入空白和空代码围栏，断言返回业务错误。
    #[test]
    fn sanitize_generated_commit_message_rejects_empty_text() {
        let err = sanitize_commit_message("```text\n   \n```").expect_err("empty message");

        assert!(err.to_string().contains("Commit message 不能为空"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     右侧 Git 历史 tab 需要稳定解析 git log 输出，避免把原始文本直接交给前端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造带字段分隔符的 git log 输出，断言解析出完整 hash、短 hash、作者、时间和标题。
    #[test]
    fn parse_git_log_output_extracts_commit_history_items() {
        let output = "abcdef123456\x1fabcdef1\x1f111111111111 222222222222\x1fAlice\x1fa@example.com\x1f2026-06-25T10:00:00+08:00\x1ffeat: add history\x1fHEAD -> refs/heads/main, refs/remotes/origin/main, tag: refs/tags/v1.0\n";

        let commits = parse_git_log_output(output);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].hash, "abcdef123456");
        assert_eq!(commits[0].short_hash, "abcdef1");
        assert_eq!(
            commits[0].parent_hashes,
            vec!["111111111111", "222222222222"]
        );
        assert_eq!(commits[0].author_name, "Alice");
        assert_eq!(commits[0].author_email, "a@example.com");
        assert_eq!(commits[0].authored_at, "2026-06-25T10:00:00+08:00");
        assert_eq!(commits[0].summary, "feat: add history");
        assert_eq!(commits[0].refs.len(), 3);
        assert_eq!(commits[0].refs[0].name, "main");
        assert_eq!(
            commits[0].refs[0].kind,
            crate::workbench::models::WorkbenchGitRefKindDto::Local
        );
        assert!(commits[0].refs[0].is_head);
        assert_eq!(commits[0].refs[1].name, "origin/main");
        assert_eq!(
            commits[0].refs[1].kind,
            crate::workbench::models::WorkbenchGitRefKindDto::Remote
        );
        assert_eq!(commits[0].refs[1].remote.as_deref(), Some("origin"));
        assert_eq!(commits[0].refs[2].name, "v1.0");
        assert_eq!(
            commits[0].refs[2].kind,
            crate::workbench::models::WorkbenchGitRefKindDto::Tag
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Git 历史 tab 必须读取 active worktree 的真实提交历史，且按 limit 控制数量。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建真实 Git 仓库和两个提交，断言 list_commits 只返回最近一条。
    #[test]
    fn list_commits_reads_recent_commits_with_limit() {
        let root = temp_git_dir("workbench-git-history");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "one\n").expect("write first");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "feat: first"]);
        fs::write(repo.join("README.md"), "two\n").expect("write second");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "fix: second"]);

        let commits = list_commits(&repo, 1).expect("list commits");

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].summary, "fix: second");

        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     新建 Git 仓库尚无提交时，Git 历史 tab 应显示空态而不是错误提示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     初始化空仓库但不创建提交，断言 list_commits 返回空列表。
    #[test]
    fn list_commits_returns_empty_for_unborn_branch() {
        let root = temp_git_dir("workbench-git-empty-history");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);

        let commits = list_commits(&repo, 30).expect("list empty commits");

        assert!(commits.is_empty());

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
