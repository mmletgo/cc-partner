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
use crate::workbench::operation_ledger::{
    MutationExecError, WorkbenchHookFailureDto, WorkbenchHookStage,
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

/// Workbench 隔离合并冻结快照。
///
/// Business Logic（为什么需要这个结构体）:
///     一键 merge 可能经历 Claude Code 长耗时冲突解决；期间不能再按可漂移分支名解析源提交，
///     也不能允许真实主分支在发布前悄然变化。
///
/// Code Logic（这个结构体做什么）:
///     保存 merge 开始时真实主 worktree 的分支、主 HEAD OID，以及源 worktree 的实际 HEAD OID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenWorkbenchMerge {
    pub main_branch: String,
    pub main_oid: String,
    pub source_oid: String,
}

/// 冻结的 push 目标（remote + remote ref），禁止在 merge 后再读可变分支解析。
///
/// Business Logic（为什么需要这个结构体）:
///     自动交付在 merge main 前后主分支 tip 与 checkout 可能变化；push 必须绑定 merge 前解析的
///     remote/ref，否则会把 merge OID 推到错误分支或跟随漂移 tip。
///
/// Code Logic（这个结构体做什么）:
///     保存 local branch 名、remote 名与完整 remote ref（如 refs/heads/main）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenPushTarget {
    pub branch: String,
    pub remote: String,
    pub remote_ref: String,
}

/// 已按 reviewed OID 合并 main 后的冻结结果。
///
/// Business Logic（为什么需要这个结构体）:
///     merge 成功后调用方只应 push 固定 merge OID 到固定 remote/ref，不能再读 current branch。
///
/// Code Logic（这个结构体做什么）:
///     保存 merge 后的 commit OID、merge 前 tip（pre_oid，供 abort 回滚 CAS）与冻结 push target。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenMainMergeResult {
    pub merge_oid: String,
    /// merge 前主分支 tip；若交付在 merge 后被终止，可用 CAS 回滚到此 OID。
    pub pre_oid: String,
    pub push_target: FrozenPushTarget,
}

/// reviewed OID merge 的分类结果。
///
/// Business Logic（为什么需要这个枚举）:
///     交付流水线需要区分 merge 成功（可 push 固定 OID）与冲突（应 abort 并 Blocked）。
///
/// Code Logic（这个枚举做什么）:
///     Merged 携带冻结 push 目标与 merge OID；Conflicted 表示已检测到冲突。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeReviewedOutcome {
    Merged(FrozenMainMergeResult),
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

/// `run_git_classified` 的非零退出结果（保留结构化 stdout/stderr/退出码，不拍平成文案）。
///
/// Business Logic（为什么需要这个结构体）:
///     pre-commit/pre-push 钩子失败需要把原始输出交给修复 agent，旧 `run_git` 拍平成字符串后丢失结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCommandFailure {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// `run_git_classified` 的错误分类。
#[derive(Debug)]
pub enum GitRunError {
    /// git 非零退出（可能含 hook 失败）。
    NonZero(GitCommandFailure),
    /// 启动 git 子进程失败等 IO 错误。
    Io(AppError),
}

impl From<GitRunError> for AppError {
    /// 非 hook 场景回退为与旧 `run_git` 一致的 `Git 命令失败: ...` 文案。
    fn from(err: GitRunError) -> Self {
        match err {
            GitRunError::NonZero(failure) => {
                let detail = if !failure.stderr.trim().is_empty() {
                    failure.stderr.trim().to_string()
                } else {
                    failure.stdout.trim().to_string()
                };
                AppError::generic(format!("Git 命令失败: {detail}"))
            }
            GitRunError::Io(err) => err,
        }
    }
}

/// 在指定 cwd 下执行 `git <args>`，成功返回 stdout，失败返回结构化 `GitRunError`。
///
/// Business Logic（为什么需要这个函数）:
///     commit/push 想判断是否钩子失败必须保留 stderr/stdout/exit_code；旧 run_git 把它们合并成字符串。
fn run_git_classified(cwd: &Path, args: &[&str]) -> Result<String, GitRunError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| {
            GitRunError::Io(AppError::generic(format!(
                "启动 git 失败（{}）: {e}",
                args.join(" ")
            )))
        })?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    Err(GitRunError::NonZero(GitCommandFailure {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code(),
    }))
}

/// 仓库当前是否安装了指定阶段的钩子脚本（pre-commit / pre-push）。
///
/// Business Logic（为什么需要这个函数）:
///     只有真正安装了钩子的仓库，commit/push 非零退出才有可能是钩子拒绝。
///
/// Code Logic（这个函数做什么）:
///     1) `git rev-parse --git-dir` 取 git 目录（best-effort，失败返回 false）；
///     2) `<git_dir>/hooks/<hook>` 存在且非 `.sample` 且非空 → true；
///     3) `git config --get core.hooksPath` 非空时，`<hooksPath>/<hook>` 存在且非空 → true
///        （覆盖 husky v9 `core.hooksPath=.husky/_` 与 lefthook 自管 hooksPath）。
///     磁盘 IO 全部 no-follow；任何 git/IO 错误都保守返回 false。
pub(crate) fn repo_has_hook_installed(path: &Path, stage: WorkbenchHookStage) -> bool {
    let hook = stage.hook_name();
    // git-dir 相对 cwd 解析（linked worktree 返回 .git/worktrees/<id>，主仓库返回 .git）。
    let git_dir = match run_git(path, &["rev-parse", "--git-dir"]) {
        Ok(s) => PathBuf::from(s.trim()),
        Err(_) => return false,
    };
    let resolved = if git_dir.is_absolute() {
        git_dir
    } else {
        path.join(&git_dir)
    };
    let direct = resolved.join("hooks").join(hook);
    if hook_file_is_real(&direct) {
        return true;
    }
    // core.hooksPath（husky/lefthook 等）：相对 cwd 解析。
    if let Ok(raw) = run_git(path, &["config", "--get", "core.hooksPath"]) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            let hooks_path = if Path::new(trimmed).is_absolute() {
                PathBuf::from(trimmed)
            } else {
                path.join(trimmed)
            };
            if hook_file_is_real(&hooks_path.join(hook)) {
                return true;
            }
        }
    }
    false
}

/// 钩子文件存在、非 `.sample`、且内容非空才视为真实钩子。
fn hook_file_is_real(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".sample"))
        .unwrap_or(false)
    {
        return false;
    }
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// 已知「不是钩子问题」的 git 失败关键词（小写匹配）。
///
/// Business Logic（为什么需要这个函数）:
///     用户明确要求只修复钩子脚本失败；身份未配置、合并冲突、远端拒绝等必须保持原 AppError 路径，
///     不能误判为钩子失败让 AI 反复尝试。
fn is_known_non_hook_git_failure(combined: &str) -> bool {
    // 内部小写化，调用方无需预处理；marker 一律小写匹配。
    let haystack = combined.to_lowercase();
    const MARKERS: &[&str] = &[
        // commit 身份未配置（应引导用户配置，而非 AI 改全局 git identity）
        "author identity unknown",
        "empty ident name",
        "empty ident email",
        "committer identity unknown",
        "please tell me who you are",
        // 无可提交改动（has_changes gate 应已挡住，保险起见排除）
        "nothing to commit",
        "no changes added to commit",
        // 合并冲突（不是钩子）
        "merge conflict",
        "automatic merge failed",
        // 非仓库 / 路径问题
        "not a git repository",
        "does not have a commit checked out",
        // push 远端拒绝（用户明确要求只修钩子脚本，不修 push 拒绝）
        "non-fast-forward",
        "fetch first",
        "! [rejected]",
        "[rejected]",
        "remote rejected",
        "remote: error",
        "denied",
        "could not read from remote repository",
    ];
    MARKERS.iter().any(|m| haystack.contains(m))
}

/// 判定一次 commit/push 的 git 非零退出是否属于钩子失败，并构造结构化 DTO。
///
/// Business Logic（为什么需要这个函数）:
///     满足「安装了对应钩子」+「非已知非钩子失败」即判定为钩子失败；判定为非钩子时返回 None，
///     调用方按普通 AppError 处理。误判上限由修复尝试次数（前端 3 次）兜底。
pub(crate) fn detect_hook_failure(
    stage: WorkbenchHookStage,
    path: &Path,
    failure: &GitCommandFailure,
) -> Option<WorkbenchHookFailureDto> {
    let combined = format!("{}\n{}", failure.stderr, failure.stdout);
    if is_known_non_hook_git_failure(&combined) {
        return None;
    }
    if !repo_has_hook_installed(path, stage) {
        return None;
    }
    Some(WorkbenchHookFailureDto {
        stage,
        stdout: failure.stdout.clone(),
        stderr: failure.stderr.clone(),
        exit_code: failure.exit_code,
    })
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
///     先确认目录是 Git 工作区；空仓库没有 HEAD 时返回空列表，否则从该 worktree 的 `HEAD`
///     读取可达提交并解析为 DTO，避免同仓库其他未合并分支污染当前工作区历史。
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
///     merge 后 parent/ancestry gate 与 merge confirm 需要对任意两 commit 做祖先判定，
///     不能只绑死当前 HEAD。
///
/// Code Logic（这个函数做什么）:
///     `git merge-base --is-ancestor <ancestor> <descendant>`；0=true，1=false。
pub fn is_ancestor(path: &Path, ancestor: &str, descendant: &str) -> Result<bool, AppError> {
    let ancestor = ancestor.trim();
    let descendant = descendant.trim();
    if ancestor.is_empty() || descendant.is_empty() {
        return Err(AppError::generic("is_ancestor 需要非空 commit oid"));
    }
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(path)
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
///     merge 验证必须确认 first parent 仍是 merge 前的 pre_oid，并判断 reviewed_oid 是否为 parent。
///
/// Code Logic（这个函数做什么）:
///     `git rev-list --parents -n 1 <commit_oid>`，解析输出中除 commit 自身外的 parent OID 列表。
pub fn commit_parent_oids(path: &Path, commit_oid: &str) -> Result<Vec<String>, AppError> {
    let commit_oid = commit_oid.trim();
    if commit_oid.is_empty() {
        return Err(AppError::generic("commit oid 不能为空"));
    }
    let output = run_git(path, &["rev-list", "--parents", "-n", "1", commit_oid])?;
    let mut parts = output.split_whitespace();
    // 首 token 是 commit 自身；其余为 parents。
    let _commit_self = parts
        .next()
        .ok_or_else(|| AppError::generic("git rev-list --parents 返回空"))?;
    Ok(parts.map(str::to_string).collect())
}

/// Business Logic（为什么需要这个函数）:
///     merge 成功后的 HEAD 可能被并发 tip 推进或被 crafty merge 伪造；必须校验 branch/ref/parents
///     仍精确绑定本次 pre-merge tip 与已审 reviewed OID。
///
/// Code Logic（这个函数做什么）:
///     1) current_branch == frozen_branch；2) refs/heads/<branch> == merge_oid；
///     3) merge_oid==pre_oid 时要求 reviewed 是 merge 的祖先（already-up-to-date）；
///     4) 否则要求恰好 2 个 parents 且 parents[0]==pre_oid、parents[1]==reviewed_oid（精确匹配，
///     禁止 ancestor 回退）。失败返回 AppError。
pub(crate) fn verify_merge_oid_binding(
    path: &Path,
    merge_oid: &str,
    pre_oid: &str,
    reviewed_oid: &str,
    frozen_branch: &str,
) -> Result<(), AppError> {
    let now_branch = current_branch(path).ok_or_else(|| {
        AppError::generic("merge main failed: current branch missing after merge")
    })?;
    if now_branch != frozen_branch {
        return Err(AppError::generic(format!(
            "merge main failed: branch drifted after merge (expected={frozen_branch}, got={now_branch})"
        )));
    }
    let refname = format!("refs/heads/{frozen_branch}");
    let ref_oid = run_git(path, &["rev-parse", &refname])?.trim().to_string();
    if ref_oid != merge_oid {
        return Err(AppError::generic(format!(
            "merge main failed: branch ref mismatch (ref={ref_oid}, head={merge_oid})"
        )));
    }
    let reviewed = reviewed_oid.trim();
    if merge_oid == pre_oid {
        if !is_ancestor(path, reviewed, merge_oid)? {
            return Err(AppError::generic(format!(
                "merge main failed: already-up-to-date tip does not contain reviewed oid {reviewed}"
            )));
        }
        return Ok(());
    }
    let parents = commit_parent_oids(path, merge_oid)?;
    if parents.len() != 2 {
        return Err(AppError::generic(format!(
            "merge main failed: expected exactly 2 parents for merge tip, got {}",
            parents.len()
        )));
    }
    if parents[0] != pre_oid {
        return Err(AppError::generic(format!(
            "merge main failed: first parent is not pre-merge tip (expected={pre_oid}, got={})",
            parents[0]
        )));
    }
    if parents[1] != reviewed {
        return Err(AppError::generic(format!(
            "merge main failed: second parent is not reviewed oid (expected={reviewed}, got={})",
            parents[1]
        )));
    }
    Ok(())
}

/// 冻结 Workbench 一键 merge 的主分支与两端 OID。
///
/// Business Logic（为什么需要这个函数）:
///     源分支名和主分支 tip 都可能在 Claude 运行期间漂移，隔离合并必须只消费开始时看到的提交。
///
/// Code Logic（这个函数做什么）:
///     从真实主 worktree 读取当前分支与 HEAD，并从源 worktree 直接读取 HEAD；任一为空即失败。
pub fn freeze_workbench_merge(
    main_path: &Path,
    source_path: &Path,
) -> Result<FrozenWorkbenchMerge, AppError> {
    let main_branch = current_branch(main_path)
        .ok_or_else(|| AppError::generic("主工作区没有可合并的当前分支"))?;
    let main_oid = head_hash(main_path)?
        .ok_or_else(|| AppError::generic("主工作区没有可合并的 HEAD 历史（empty/unborn）"))?;
    let source_oid = head_hash(source_path)?
        .ok_or_else(|| AppError::generic("源 worktree 没有可合并的 HEAD"))?;
    Ok(FrozenWorkbenchMerge {
        main_branch,
        main_oid,
        source_oid,
    })
}

/// 校验执行阶段重新读取的 merge 快照仍与 ledger 冻结 intent 一致。
///
/// Business Logic（为什么需要这个函数）:
///     ledger intent 会先于关闭终端与创建 integration worktree 落盘；这段窗口内 HEAD 漂移时，
///     实际 merge 不能改用新 OID，否则 owner 重启将无法按持久 intent 精确对账。
///
/// Code Logic（这个函数做什么）:
///     要求 main branch、main OID、source OID 三字段逐一相等；任一漂移返回 conflict。
pub fn ensure_frozen_merge_unchanged(
    expected: &FrozenWorkbenchMerge,
    actual: &FrozenWorkbenchMerge,
) -> Result<(), AppError> {
    if actual.main_branch != expected.main_branch {
        return Err(AppError::conflict(
            "ledger claim 后主工作区分支已变化，拒绝继续 merge".to_string(),
        ));
    }
    if actual.main_oid != expected.main_oid {
        return Err(AppError::conflict(
            "ledger claim 后主工作区 HEAD 已变化，拒绝继续 merge".to_string(),
        ));
    }
    if actual.source_oid != expected.source_oid {
        return Err(AppError::conflict(
            "ledger claim 后源 worktree HEAD 已变化，拒绝继续 merge".to_string(),
        ));
    }
    Ok(())
}

/// 创建未绑定业务分支的隔离 integration worktree。
///
/// Business Logic（为什么需要这个函数）:
///     merge 冲突文件不能出现在真实主 worktree，否则开发 watcher 会重启后端并杀死 Claude headless。
///
/// Code Logic（这个函数做什么）:
///     执行 `git worktree add --detach <path> <main_oid>`，使隔离目录从冻结主提交开始且不占用分支。
#[cfg(test)]
pub fn create_detached_integration_worktree(
    repo_path: &Path,
    integration_path: &Path,
    main_oid: &str,
) -> Result<(), AppError> {
    create_detached_integration_worktree_outside(
        repo_path,
        integration_path,
        main_oid,
        &[repo_path],
    )
}

/// 创建隔离 worktree，并拒绝落入任一被 watcher 监控的 checkout 根。
///
/// Business Logic（为什么需要这个函数）:
///     自定义 db_path 可能位于 main 或 source linked worktree 内；只排除 owning repository toplevel
///     仍会让该 checkout 的 watcher 因冲突文件重启 owner。
///
/// Code Logic（这个函数做什么）:
///     先解析 integration 的真实落点，并与 caller 提供的 main/source forbidden roots 做 canonical containment；
///     通过后才创建目录并调用 detached `git worktree add`。
pub fn create_detached_integration_worktree_outside(
    repo_path: &Path,
    integration_path: &Path,
    main_oid: &str,
    forbidden_roots: &[&Path],
) -> Result<(), AppError> {
    if main_oid.trim().is_empty() {
        return Err(AppError::generic(
            "integration worktree 的 main oid 不能为空",
        ));
    }
    if integration_path.exists() {
        return Err(AppError::conflict(
            "integration worktree 路径已存在，请先完成残留清理".to_string(),
        ));
    }
    let owning_repo = PathBuf::from(repo_root(repo_path)?)
        .canonicalize()
        .map_err(AppError::from)?;
    let resolved_integration = resolve_path_through_existing_ancestor(integration_path)?;
    let mut monitored_roots = vec![owning_repo];
    for root in forbidden_roots {
        let canonical = root.canonicalize().map_err(AppError::from)?;
        if !monitored_roots.contains(&canonical) {
            monitored_roots.push(canonical);
        }
    }
    if monitored_roots
        .iter()
        .any(|root| resolved_integration.starts_with(root))
    {
        return Err(AppError::validation(
            "integration worktree 必须位于 main/source checkout 之外，避免触发项目 watcher"
                .to_string(),
        ));
    }
    if let Some(parent) = integration_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let target = integration_path.to_string_lossy().to_string();
    run_git(
        repo_path,
        &["worktree", "add", "--detach", &target, main_oid.trim()],
    )?;
    Ok(())
}

/// 解析尚不存在路径的真实父链位置。
///
/// Business Logic（为什么需要这个函数）:
///     integration 目录尚未创建时仍需识别其是否经 symlink/`..` 落入 owning repository，
///     且必须在创建任何目录项前完成拒绝，避免 watcher 已被触发。
///
/// Code Logic（这个函数做什么）:
///     从目标向上寻找首个可 canonicalize 的既存祖先，记录被剥离的路径段，再按正序拼回；
///     无既存祖先或缺少普通路径段时返回校验错误。
fn resolve_path_through_existing_ancestor(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut cursor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut resolved) => {
                for component in suffix.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor.file_name().ok_or_else(|| {
                    AppError::validation("无法解析 integration worktree 路径".to_string())
                })?;
                suffix.push(name.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    AppError::validation("无法解析 integration worktree 父目录".to_string())
                })?;
            }
            Err(error) => return Err(AppError::from(error)),
        }
    }
}

/// 严格校验隔离产物是本次冻结输入形成的双父 merge commit。
///
/// Business Logic（为什么需要这个函数）:
///     发布到主分支前必须阻止 Claude、钩子或并发 Git 操作把额外提交伪装成本次 merge 产物。
///
/// Code Logic（这个函数做什么）:
///     读取 commit parents，要求数量恰为 2 且顺序精确等于 `[main_oid, source_oid]`。
pub fn verify_strict_merge_commit(
    repo_path: &Path,
    merge_oid: &str,
    main_oid: &str,
    source_oid: &str,
) -> Result<(), AppError> {
    let parents = commit_parent_oids(repo_path, merge_oid)?;
    if parents.len() != 2 {
        return Err(AppError::generic(format!(
            "隔离 merge 产物必须恰有两个 parent，实际为 {} 个",
            parents.len()
        )));
    }
    if parents[0] != main_oid.trim() {
        return Err(AppError::generic(
            "隔离 merge 产物的 first parent 与冻结主 HEAD 不一致".to_string(),
        ));
    }
    if parents[1] != source_oid.trim() {
        return Err(AppError::generic(
            "隔离 merge 产物的 second parent 与冻结源 HEAD 不一致".to_string(),
        ));
    }
    Ok(())
}

/// 把已验证的隔离 merge commit 安全发布到真实主 worktree。
///
/// Business Logic（为什么需要这个函数）:
///     Claude 处理期间真实主分支可能被用户或其它工具推进；发布不得覆盖并发提交或切错分支。
///
/// Code Logic（这个函数做什么）:
///     先校验隔离 commit 双父，再确认主 worktree 分支、HEAD、clean 状态仍与冻结快照一致，
///     最后执行 `git merge --ff-only <merge_oid>`，并复查 HEAD、分支与 clean 状态。
pub fn publish_integration_merge(
    main_path: &Path,
    frozen: &FrozenWorkbenchMerge,
    merge_oid: &str,
) -> Result<(), AppError> {
    verify_strict_merge_commit(main_path, merge_oid, &frozen.main_oid, &frozen.source_oid)?;
    let branch = current_branch(main_path).ok_or_else(|| {
        AppError::conflict("主工作区已不在任何分支，拒绝发布隔离 merge".to_string())
    })?;
    if branch != frozen.main_branch {
        return Err(AppError::conflict(format!(
            "主工作区分支已变化（冻结={}，当前={}），拒绝发布隔离 merge",
            frozen.main_branch, branch
        )));
    }
    let head = head_hash(main_path)?.ok_or_else(|| {
        AppError::conflict("主工作区 HEAD 已不存在，拒绝发布隔离 merge".to_string())
    })?;
    if head != frozen.main_oid {
        return Err(AppError::conflict(
            "主工作区 HEAD 已变化，拒绝覆盖并发提交".to_string(),
        ));
    }
    if !status(main_path)?.clean {
        return Err(AppError::conflict(
            "主工作区在隔离 merge 期间产生了未提交改动，拒绝发布".to_string(),
        ));
    }
    run_git(main_path, &["merge", "--ff-only", merge_oid.trim()])?;
    let published_head = head_hash(main_path)?
        .ok_or_else(|| AppError::generic("发布隔离 merge 后主工作区 HEAD 为空"))?;
    if published_head != merge_oid.trim()
        || current_branch(main_path).as_deref() != Some(frozen.main_branch.as_str())
        || !status(main_path)?.clean
    {
        return Err(AppError::generic(
            "隔离 merge 发布后的主工作区校验失败，请检查 Git 状态".to_string(),
        ));
    }
    Ok(())
}

/// 发布前确认源 worktree 仍精确停在冻结输入。
///
/// Business Logic（为什么需要这个函数）:
///     Claude 长任务期间源 worktree 可能被外部工具推进或改脏；若仍发布并 cleanup，会删除用户新提交或改动。
///
/// Code Logic（这个函数做什么）:
///     重新读取 source HEAD、branch 与 status，要求 HEAD==frozen.source_oid、branch==expected_branch 且 clean；
///     任一不符返回 conflict，调用方只清 integration 并保留源 worktree。
pub fn verify_source_unchanged_for_publish(
    source_path: &Path,
    frozen: &FrozenWorkbenchMerge,
    expected_branch: &str,
) -> Result<(), AppError> {
    let source_head = head_hash(source_path)?
        .ok_or_else(|| AppError::conflict("源 worktree HEAD 已不存在，拒绝发布".to_string()))?;
    if source_head != frozen.source_oid {
        return Err(AppError::conflict(
            "源 worktree HEAD 在隔离 merge 期间已变化，拒绝发布并保留源工作区".to_string(),
        ));
    }
    let source_branch = current_branch(source_path)
        .ok_or_else(|| AppError::conflict("源 worktree 已不在任何分支，拒绝发布".to_string()))?;
    if source_branch != expected_branch.trim() {
        return Err(AppError::conflict(
            "源 worktree 分支在隔离 merge 期间已变化，拒绝发布并保留源工作区".to_string(),
        ));
    }
    if !status(source_path)?.clean {
        return Err(AppError::conflict(
            "源 worktree 在隔离 merge 期间产生未提交改动，拒绝发布并保留源工作区".to_string(),
        ));
    }
    Ok(())
}

/// 清理隔离 integration worktree。
///
/// Business Logic（为什么需要这个函数）:
///     成功、冲突解决失败或主分支漂移都不能留下被 Git 注册的内部临时 worktree。
///
/// Code Logic（这个函数做什么）:
///     对存在或仍被登记的路径执行 `git worktree remove --force`，随后 prune 残留管理项；
///     路径和登记均已不存在时保持幂等成功。
pub fn remove_integration_worktree(
    repo_path: &Path,
    integration_path: &Path,
) -> Result<(), AppError> {
    let target = integration_path.to_string_lossy().to_string();
    let listed = list_worktrees(repo_path, &repo_root(repo_path)?)?
        .into_iter()
        .any(|item| Path::new(&item.path) == integration_path);
    if integration_path.exists() || listed {
        run_git(repo_path, &["worktree", "remove", "--force", &target])?;
    }
    run_git(repo_path, &["worktree", "prune"])?;
    Ok(())
}

/// 查找从冻结主 HEAD 到当前 tip 之间精确绑定两父的 merge commit。
///
/// Business Logic（为什么需要这个函数）:
///     后端可能在短发布窗口被 watcher 重启；ledger 仍为 running 时需要确认 merge 是否其实已经发布，
///     但不能仅凭 source 是祖先就误认其它合并为本次操作。
///
/// Code Logic（这个函数做什么）:
///     若当前 tip 包含冻结主/源提交，则遍历 `main_oid..tip` 可达提交，返回 parents 精确为
///     `[main_oid, source_oid]` 的 commit OID；没有精确匹配返回 None。
pub fn find_published_merge_commit(
    repo_path: &Path,
    main_oid: &str,
    source_oid: &str,
) -> Result<Option<String>, AppError> {
    let Some(tip) = head_hash(repo_path)? else {
        return Ok(None);
    };
    if !is_ancestor(repo_path, main_oid, &tip)? || !is_ancestor(repo_path, source_oid, &tip)? {
        return Ok(None);
    }
    let range = format!("{}..{}", main_oid.trim(), tip);
    let commits = run_git(repo_path, &["rev-list", "--topo-order", &range])?;
    for oid in commits.lines().map(str::trim).filter(|oid| !oid.is_empty()) {
        let parents = commit_parent_oids(repo_path, oid)?;
        if parents == [main_oid.trim(), source_oid.trim()] {
            return Ok(Some(oid.to_string()));
        }
    }
    Ok(None)
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
/// 简单 stage+commit 场景复用（delivery 生产/harness 已改 freeze path；保留给其它 git 单测）。
#[cfg(test)]
#[allow(dead_code)]
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
/// 提交 staged 改动（返回 AppError，不区分钩子失败）。生产 commit 按钮改走 `commit_staged_checked`；
/// 本函数保留给 `commit_all`（test-only）与未来不需要钩子分类的调用方。
#[allow(dead_code)]
pub fn commit_staged(path: &Path, message: &str) -> Result<(), AppError> {
    let cleaned = sanitize_commit_message(message)?;
    run_git(path, &["commit", "-m", &cleaned])?;
    Ok(())
}

/// 与 `commit_staged` 相同的提交语义，但失败时分类：pre-commit 钩子拒绝 → `MutationExecError::Hook`，
/// 其它失败 → `MutationExecError::Other(AppError)`。供工作台 commit 按钮走 failedHook 修复路径。
///
/// Business Logic（为什么需要这个函数）:
///     工作台 commit 失败要让前端展示「让 AI 修复并重试」，必须区分钩子失败；delivery 等仍用 commit_staged。
pub fn commit_staged_checked(path: &Path, message: &str) -> Result<(), MutationExecError> {
    let cleaned = sanitize_commit_message(message)?;
    match run_git_classified(path, &["commit", "-m", &cleaned]) {
        Ok(_) => Ok(()),
        Err(GitRunError::Io(e)) => Err(MutationExecError::Other(e)),
        Err(GitRunError::NonZero(failure)) => {
            if let Some(hook) = detect_hook_failure(WorkbenchHookStage::PreCommit, path, &failure) {
                Err(MutationExecError::Hook(hook))
            } else {
                Err(MutationExecError::Other(AppError::from(
                    GitRunError::NonZero(failure),
                )))
            }
        }
    }
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
            // unborn：仅允许 zero-OID CAS；失败不得无条件 update-ref 覆盖已有 tip。
            run_git(
                path,
                &[
                    "update-ref",
                    &refname,
                    &new_oid,
                    "0000000000000000000000000000000000000000",
                ],
            )?;
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
///     先 `freeze_push_target` 再 `push_commit_oid_to`；有 upstream 时同样用显式 refspec。
pub fn push_commit_oid(path: &Path, branch: &str, commit_oid: &str) -> Result<(), AppError> {
    let target = freeze_push_target(path, branch)?;
    push_commit_oid_to(path, &target.remote, &target.remote_ref, commit_oid)
}

/// Business Logic（为什么需要这个函数）:
///     merge main 前必须冻结 remote/ref，merge 后再读 current branch 会跟到错误 tip。
///
/// Code Logic（这个函数做什么）:
///     解析 branch 的 push remote 与 remote_ref，返回 FrozenPushTarget。
pub fn freeze_push_target(path: &Path, branch: &str) -> Result<FrozenPushTarget, AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("当前 worktree 没有可推送的分支"));
    }
    let (remote, remote_ref) = resolve_push_remote_and_ref(path, branch)?;
    Ok(FrozenPushTarget {
        branch: branch.trim().to_string(),
        remote,
        remote_ref,
    })
}

/// Business Logic（为什么需要这个函数）:
///     已冻结 remote/ref 后，push 不得再解析可变分支配置。
///
/// Code Logic（这个函数做什么）:
///     执行 `git push <remote> <commit_oid>:<remote_ref>`，remote/ref 仅使用入参。
pub fn push_commit_oid_to(
    path: &Path,
    remote: &str,
    remote_ref: &str,
    commit_oid: &str,
) -> Result<(), AppError> {
    if remote.trim().is_empty() {
        return Err(AppError::generic("push remote 不能为空"));
    }
    if remote_ref.trim().is_empty() {
        return Err(AppError::generic("push remote ref 不能为空"));
    }
    if commit_oid.trim().is_empty() {
        return Err(AppError::generic("commit oid 不能为空"));
    }
    let refspec = format!("{}:{}", commit_oid.trim(), remote_ref.trim());
    run_git(path, &["push", remote.trim(), &refspec])?;
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
///     读取当前分支名并 freeze push target，再 push 给定 main commit OID。
///     交付流水线优先用 `push_main_commit_oid_to`（merge 前已冻结 target）；本函数保留给
///     单步「当前 HEAD + 当前 branch」推送场景。
#[allow(dead_code)]
pub fn push_main_commit_oid(path: &Path, commit_oid: &str) -> Result<String, AppError> {
    let branch =
        current_branch(path).ok_or_else(|| AppError::generic("主工作区没有可推送的当前分支"))?;
    let target = freeze_push_target(path, &branch)?;
    push_main_commit_oid_to(path, &target, commit_oid)?;
    Ok(branch)
}

/// Business Logic（为什么需要这个函数）:
///     交付 merge 前已冻结的 main push target 必须原样用于 push，禁止再解析 current branch。
///
/// Code Logic（这个函数做什么）:
///     仅使用 `target.remote` / `target.remote_ref` 调用 `push_commit_oid_to`。
pub fn push_main_commit_oid_to(
    path: &Path,
    target: &FrozenPushTarget,
    commit_oid: &str,
) -> Result<(), AppError> {
    push_commit_oid_to(path, &target.remote, &target.remote_ref, commit_oid)
}

/// Business Logic（为什么需要这个函数）:
///     merge reviewed OID 时必须先冻结 main 的 push remote/ref，再 merge，再读 merge OID；
///     调用方须持有 project main 进程锁，保证 freeze/merge/head 原子序。
///     merge 后必须验证 first-parent/ancestry，防止并发 tip 推进返回错误 OID。
///
/// Code Logic（这个函数做什么）:
///     1) pre_oid = head_hash（要求 Some）；2) 读 branch → freeze_push_target；
///     3) merge_commit_oid；4) Conflicted 原样返回；Merged 后 head_hash +
///     verify_merge_oid_binding（branch/ref/exact parents [pre, reviewed]）。
pub fn merge_reviewed_oid_with_frozen_main(
    main_path: &Path,
    reviewed_oid: &str,
) -> Result<MergeReviewedOutcome, AppError> {
    let pre_oid = head_hash(main_path)?
        .ok_or_else(|| AppError::generic("主工作区没有可合并的 HEAD 历史（empty/unborn）"))?;
    let branch = current_branch(main_path)
        .ok_or_else(|| AppError::generic("主工作区没有可合并的当前分支"))?;
    let push_target = freeze_push_target(main_path, &branch)?;
    match merge_commit_oid(main_path, reviewed_oid)? {
        MergeBranchOutcome::Conflicted => Ok(MergeReviewedOutcome::Conflicted),
        MergeBranchOutcome::Merged => {
            let merge_oid = head_hash(main_path)?.ok_or_else(|| {
                AppError::generic("merge main failed: main HEAD empty after merge")
            })?;
            if let Err(bind_err) =
                verify_merge_oid_binding(main_path, &merge_oid, &pre_oid, reviewed_oid, &branch)
            {
                // binding 失败说明 merge 已改写 tip/index；必须完整回滚再向外抛错。
                if let Err(rb_err) =
                    rollback_main_merge_full(main_path, &branch, &pre_oid, &merge_oid)
                {
                    return Err(AppError::generic(format!(
                        "merge main binding failed ({bind_err}); full rollback also failed: {rb_err}"
                    )));
                }
                return Err(bind_err);
            }
            Ok(MergeReviewedOutcome::Merged(FrozenMainMergeResult {
                merge_oid,
                pre_oid,
                push_target,
            }))
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     交付在 merge 成功后若任务已被 Abort/Cancel，必须把本地 main 从污染 tip CAS 回 pre_oid，
///     否则后续交付会把已终止任务的 merge 当作祖先间接推送。
///
/// Code Logic（这个函数做什么）:
///     `git update-ref refs/heads/<branch> <pre_oid> <merge_oid>`；期望 old=merge_oid，
///     成功则 tip 回滚；CAS 失败（tip 已漂移）返回错误由调用方记录。
///     注意：本函数只回滚 ref，不恢复 index/worktree；生产 abort 路径应优先
///     `rollback_main_merge_full`。
pub fn rollback_main_merge_cas(
    main_path: &Path,
    branch: &str,
    pre_oid: &str,
    merge_oid: &str,
) -> Result<(), AppError> {
    if branch.trim().is_empty() {
        return Err(AppError::generic("rollback main: branch 不能为空"));
    }
    if pre_oid.trim().is_empty() || merge_oid.trim().is_empty() {
        return Err(AppError::generic("rollback main: pre/merge oid 不能为空"));
    }
    let refname = format!("refs/heads/{}", branch.trim());
    run_git(
        main_path,
        &["update-ref", &refname, pre_oid.trim(), merge_oid.trim()],
    )?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     merge 验证失败或交付 abort-after-merge 时，仅 update-ref 会留下脏 index/worktree
///     （仍停在 merge tree），污染后续 dirty check 与 push；必须完整恢复 ref+index+worktree。
///
/// Code Logic（这个函数做什么）:
///     1) `update-ref` CAS：branch tip `merge_oid` → `pre_oid`（tip 已漂移则 Err）；
///     2) 确保当前分支为 `branch` 后 `git reset --hard <pre_oid>`；
///     3) 校验 `head_hash == pre_oid` 且 `status.clean`，否则 Err。
pub fn rollback_main_merge_full(
    main_path: &Path,
    branch: &str,
    pre_oid: &str,
    merge_oid: &str,
) -> Result<(), AppError> {
    let branch = branch.trim();
    let pre_oid = pre_oid.trim();
    let merge_oid = merge_oid.trim();
    if branch.is_empty() {
        return Err(AppError::generic("rollback main full: branch 不能为空"));
    }
    if pre_oid.is_empty() || merge_oid.is_empty() {
        return Err(AppError::generic(
            "rollback main full: pre/merge oid 不能为空",
        ));
    }
    // tip 已不是 merge_oid 时 CAS 失败，避免把别人的推进误回滚。
    rollback_main_merge_cas(main_path, branch, pre_oid, merge_oid)?;

    if let Some(now_branch) = current_branch(main_path) {
        if now_branch != branch {
            // 尝试切回冻结分支再 hard reset
            run_git(main_path, &["checkout", branch])?;
        }
    } else {
        run_git(main_path, &["checkout", branch])?;
    }
    run_git(main_path, &["reset", "--hard", pre_oid])?;

    let head = head_hash(main_path)?
        .ok_or_else(|| AppError::generic("rollback main full: HEAD empty after reset --hard"))?;
    if head != pre_oid {
        return Err(AppError::generic(format!(
            "rollback main full: HEAD 未回到 pre_oid (expected={pre_oid}, got={head})"
        )));
    }
    let st = status(main_path)?;
    if !st.clean {
        return Err(AppError::generic(
            "rollback main full: index/worktree 在 reset 后仍不干净",
        ));
    }
    Ok(())
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

/// 与 `push_branch` 相同的推送语义（upstream/origin 安全规则一致），但失败时分类：
/// pre-push 钩子拒绝 → `MutationExecError::Hook`；远端拒绝（non-fast-forward 等）/其它失败 → `Other(AppError)`。
/// 供工作台 push 按钮走 failedHook 修复路径。
pub fn push_branch_checked(path: &Path, branch: &str) -> Result<(), MutationExecError> {
    if branch.trim().is_empty() {
        return Err(MutationExecError::Other(AppError::generic(
            "当前 worktree 没有可推送的分支",
        )));
    }
    let target = resolve_push_target(path).map_err(MutationExecError::Other)?;
    let args: Vec<&str> = match target {
        PushTarget::Upstream => vec!["push"],
        PushTarget::Remote(remote) => {
            // remote 是 String，需要借用存活到 run_git_classified 调用结束。
            // 用单独作用域避免临时值被释放。
            return push_branch_checked_remote(path, branch, &remote);
        }
    };
    match run_git_classified(path, &args) {
        Ok(_) => Ok(()),
        Err(GitRunError::Io(e)) => Err(MutationExecError::Other(e)),
        Err(GitRunError::NonZero(failure)) => {
            if let Some(hook) = detect_hook_failure(WorkbenchHookStage::PrePush, path, &failure) {
                Err(MutationExecError::Hook(hook))
            } else {
                Err(MutationExecError::Other(AppError::from(
                    GitRunError::NonZero(failure),
                )))
            }
        }
    }
}

/// `push_branch_checked` 的 `-u <remote> <branch>` 分支（独立函数避免 String 临时值生命周期问题）。
fn push_branch_checked_remote(
    path: &Path,
    branch: &str,
    remote: &str,
) -> Result<(), MutationExecError> {
    let args: [&str; 4] = ["push", "-u", remote, branch];
    match run_git_classified(path, &args) {
        Ok(_) => Ok(()),
        Err(GitRunError::Io(e)) => Err(MutationExecError::Other(e)),
        Err(GitRunError::NonZero(failure)) => {
            if let Some(hook) = detect_hook_failure(WorkbenchHookStage::PrePush, path, &failure) {
                Err(MutationExecError::Hook(hook))
            } else {
                Err(MutationExecError::Other(AppError::from(
                    GitRunError::NonZero(failure),
                )))
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     非 OID 绑定场景（手动/旧路径）在主工作区 cwd 推送当前分支；Orchestrator 交付已改走
///     `push_main_commit_oid_to`，本 helper 仍保留给测试与未来非交付推送复用。
///
/// Code Logic（这个函数做什么）:
///     在传入的主工作区路径读取当前分支名，再复用 push_branch 的 upstream/origin 安全规则推送，成功返回分支名。
#[allow(dead_code)]
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
#[cfg(test)]
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

/// 判断指定路径是否仍登记为 owning repository 的 Git worktree。
///
/// Business Logic（为什么需要这个函数）:
///     owner 可能在 `git worktree remove` 成功、SQLite row 删除前崩溃；恢复时必须区分
///     “物理/Git 登记已清理”与“源 worktree 仍存在但发生漂移”，避免遗留幽灵 row。
///
/// Code Logic（这个函数做什么）:
///     读取 `git worktree list --porcelain`，对目标与每个登记路径做 canonical-or-absolute 比较；
///     目标不存在时仍可按规范化绝对路径识别 prunable 登记。
pub fn is_worktree_registered(repo_path: &Path, worktree_path: &Path) -> Result<bool, AppError> {
    let target = comparable_worktree_path(worktree_path)?;
    let main = repo_root(repo_path)?;
    let items = list_worktrees(repo_path, &main)?;
    for item in items {
        if comparable_worktree_path(Path::new(&item.path))? == target {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 生成 worktree 路径的稳定比较形式。
///
/// Business Logic（为什么需要这个函数）:
///     crash recovery 要比较可能已经不存在的路径，不能强依赖 canonicalize 成功。
///
/// Code Logic（这个函数做什么）:
///     路径存在时 canonicalize；不存在时转为绝对路径，并机械消解 `.`/`..` 组件。
fn comparable_worktree_path(path: &Path) -> Result<PathBuf, AppError> {
    if let Ok(canonical) = path.canonicalize() {
        return Ok(canonical);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// 清理已不存在 worktree 的残留 Git 登记。
///
/// Business Logic（为什么需要这个函数）:
///     对不存在路径再次执行 `git worktree remove` 会失败并阻断 SQLite cleanup；应只 prune 残留登记。
///
/// Code Logic（这个函数做什么）:
///     要求磁盘路径不存在，执行 `git worktree prune`，再确认路径已不在 porcelain 列表；
///     若路径重新出现或登记仍残留则返回 conflict。
pub fn prune_missing_worktree_registration(
    repo_path: &Path,
    worktree_path: &Path,
) -> Result<(), AppError> {
    if worktree_path.exists() {
        return Err(AppError::conflict(
            "源 worktree 路径仍存在，不能按缺失路径恢复清理".to_string(),
        ));
    }
    run_git(repo_path, &["worktree", "prune"])?;
    if is_worktree_registered(repo_path, worktree_path)? {
        return Err(AppError::conflict(
            "源 worktree 路径已不存在，但 Git 登记仍未清理".to_string(),
        ));
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
    // unborn: `No commits yet on <branch>`（不能按空格取首 token，否则会得到 "No"）。
    if let Some(rest) = header.strip_prefix("No commits yet on ") {
        let branch = rest.split([' ', '[']).next().unwrap_or_default().trim();
        if !branch.is_empty() {
            status.branch = Some(branch.to_string());
        }
        return;
    }
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
    ///     unborn zero-OID CAS 失败时不得无条件 update-ref 覆盖并发初始 commit。
    ///
    /// Code Logic（这个测试做什么）:
    ///     orphan 分支上先 commit-tree 成功落首 tip，再以 expected_parent=None 并发第二次
    ///     commit_frozen_tree，断言失败且 tip 仍为第一次 OID。
    #[test]
    fn commit_frozen_tree_unborn_cas_does_not_overwrite_existing_tip() {
        let root = temp_git_dir("workbench-unborn-cas");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).expect("repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["checkout", "--orphan", "unborn-feature"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        // orphan 分支尚无 commit：write-tree 空 index 后 CAS；第二次必须失败。
        let empty_tree = git_test_command(&repo, &["write-tree"]).trim().to_string();
        let first =
            commit_frozen_tree(&repo, &empty_tree, "first unborn", None).expect("first unborn CAS");
        let tip_after_first = head_hash(&repo).expect("head").expect("some tip");
        assert_eq!(tip_after_first, first);
        // 再造不同 tree 内容后第二次 unborn CAS 应失败。
        fs::write(repo.join("a.txt"), "race\n").expect("write");
        git_test_command(&repo, &["add", "a.txt"]);
        let second_tree = write_tree_hash(&repo).expect("second tree");
        let err = commit_frozen_tree(&repo, &second_tree, "second unborn", None)
            .expect_err("second unborn CAS must fail");
        assert!(
            err.to_string().contains("update-ref")
                || err.to_string().contains("Git 命令失败")
                || err.to_string().to_ascii_lowercase().contains("cannot lock")
                || err.to_string().contains("but expected"),
            "unexpected err: {err}"
        );
        let tip_now = head_hash(&repo).expect("head").expect("some tip");
        assert_eq!(tip_now, first, "first tip must remain after failed CAS");
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     freeze_push_target 后 push 不得因 checkout 切换而改写 remote/ref。
    ///
    /// Code Logic（这个测试做什么）:
    ///     bare origin + clone；freeze main 目标后 checkout 另一分支，仍用冻结 target push OID。
    #[test]
    fn push_commit_oid_to_uses_frozen_remote_ref_after_checkout_change() {
        let root = temp_git_dir("workbench-frozen-push");
        let origin = root.join("origin.git");
        let main = root.join("main");
        fs::create_dir_all(&origin).expect("origin");
        git_test_command(&origin, &["init", "--bare"]);
        fs::create_dir_all(&main).expect("main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        git_test_command(
            &main,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_test_command(&main, &["push", "-u", "origin", "main"]);
        let target = freeze_push_target(&main, "main").expect("freeze main");
        assert_eq!(target.branch, "main");
        assert_eq!(target.remote, "origin");
        assert_eq!(target.remote_ref, "refs/heads/main");
        fs::write(main.join("feat.txt"), "v1\n").expect("feat");
        git_test_command(&main, &["add", "feat.txt"]);
        git_test_command(&main, &["commit", "-m", "feat"]);
        let oid = head_hash(&main).expect("head").expect("oid");
        // 切换到其它本地分支，证明 push 不读 current branch。
        git_test_command(&main, &["checkout", "-b", "other"]);
        push_commit_oid_to(&main, &target.remote, &target.remote_ref, &oid).expect("push frozen");
        let origin_main = git_test_command(&origin, &["rev-parse", "refs/heads/main"])
            .trim()
            .to_string();
        assert_eq!(origin_main, oid);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     merge_reviewed_oid_with_frozen_main 必须在 merge 前冻结 branch/remote/ref。
    ///
    /// Code Logic（这个测试做什么）:
    ///     feature OID merge 进 main；断言 FrozenMainMergeResult.push_target.branch=main，
    ///     merge_oid 等于 merge 后 HEAD。
    #[test]
    fn merge_reviewed_oid_with_frozen_main_freezes_branch_before_merge() {
        let root = temp_git_dir("workbench-frozen-merge");
        let origin = root.join("origin.git");
        let main = root.join("main");
        let feature = root.join("feature");
        fs::create_dir_all(&origin).expect("origin");
        git_test_command(&origin, &["init", "--bare"]);
        fs::create_dir_all(&main).expect("main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        git_test_command(
            &main,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_test_command(&main, &["push", "-u", "origin", "main"]);
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/freeze",
                feature.to_str().unwrap(),
            ],
        );
        fs::write(feature.join("feat.txt"), "v1\n").expect("feat");
        git_test_command(&feature, &["add", "feat.txt"]);
        git_test_command(&feature, &["commit", "-m", "feat"]);
        let reviewed = head_hash(&feature).expect("head").expect("oid");
        let outcome = merge_reviewed_oid_with_frozen_main(&main, &reviewed).expect("merge frozen");
        match outcome {
            MergeReviewedOutcome::Merged(result) => {
                assert_eq!(result.push_target.branch, "main");
                assert_eq!(result.push_target.remote, "origin");
                assert_eq!(result.push_target.remote_ref, "refs/heads/main");
                let head = head_hash(&main).expect("head").expect("oid");
                assert_eq!(result.merge_oid, head);
                assert!(main.join("feat.txt").exists());
            }
            MergeReviewedOutcome::Conflicted => panic!("expected merged"),
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     abort-after-merge 必须完整恢复 ref+index+worktree，否则 main 脏态会挡住后续交付。
    ///
    /// Code Logic（这个测试做什么）:
    ///     merge feature 进 main 后 `rollback_main_merge_full`；断言 HEAD=pre_oid、clean、
    ///     merge 内容（feat.txt）消失。
    #[test]
    fn rollback_main_merge_full_restores_ref_index_and_worktree() {
        let root = temp_git_dir("workbench-full-rollback");
        let origin = root.join("origin.git");
        let main = root.join("main");
        let feature = root.join("feature");
        fs::create_dir_all(&origin).expect("origin");
        git_test_command(&origin, &["init", "--bare"]);
        fs::create_dir_all(&main).expect("main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        git_test_command(
            &main,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_test_command(&main, &["push", "-u", "origin", "main"]);
        let pre_oid = head_hash(&main).expect("pre").expect("some");
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/rb",
                feature.to_str().unwrap(),
            ],
        );
        fs::write(feature.join("feat.txt"), "merged-content\n").expect("feat");
        git_test_command(&feature, &["add", "feat.txt"]);
        git_test_command(&feature, &["commit", "-m", "feat"]);
        let reviewed = head_hash(&feature).expect("head").expect("oid");
        let outcome = merge_reviewed_oid_with_frozen_main(&main, &reviewed).expect("merge");
        let merge_oid = match outcome {
            MergeReviewedOutcome::Merged(r) => {
                assert!(main.join("feat.txt").exists());
                r.merge_oid
            }
            MergeReviewedOutcome::Conflicted => panic!("expected merged"),
        };
        rollback_main_merge_full(&main, "main", &pre_oid, &merge_oid).expect("full rollback");
        let head = head_hash(&main).expect("head").expect("oid");
        assert_eq!(head, pre_oid, "HEAD must return to pre_oid");
        let st = status(&main).expect("status");
        assert!(st.clean, "worktree must be clean after full rollback");
        assert!(
            !main.join("feat.txt").exists(),
            "merge content must be gone from worktree"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     merge 后若 HEAD 被并发推进到无关 tip，parent gate 必须拒绝把错误 tip 当作 merge_oid。
    ///
    /// Code Logic（这个测试做什么）:
    ///     完成合法 merge 后在 main 上再造额外 commit；用 pre_oid + reviewed 校验推进后的 tip，
    ///     断言 first-parent 门禁失败。
    #[test]
    fn merge_reviewed_oid_parent_gate_rejects_concurrent_tip_advance() {
        let root = temp_git_dir("workbench-merge-parent-gate");
        let origin = root.join("origin.git");
        let main = root.join("main");
        let feature = root.join("feature");
        fs::create_dir_all(&origin).expect("origin");
        git_test_command(&origin, &["init", "--bare"]);
        fs::create_dir_all(&main).expect("main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        git_test_command(
            &main,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        git_test_command(&main, &["push", "-u", "origin", "main"]);
        let pre_oid = head_hash(&main).expect("pre").expect("some");
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/gate",
                feature.to_str().unwrap(),
            ],
        );
        fs::write(feature.join("feat.txt"), "v1\n").expect("feat");
        git_test_command(&feature, &["add", "feat.txt"]);
        git_test_command(&feature, &["commit", "-m", "feat"]);
        let reviewed = head_hash(&feature).expect("head").expect("oid");
        let outcome = merge_reviewed_oid_with_frozen_main(&main, &reviewed).expect("merge");
        let merge_oid = match outcome {
            MergeReviewedOutcome::Merged(r) => r.merge_oid,
            MergeReviewedOutcome::Conflicted => panic!("expected merged"),
        };
        // 合法 merge tip 应通过 parent gate。
        verify_merge_oid_binding(&main, &merge_oid, &pre_oid, &reviewed, "main")
            .expect("legitimate merge tip must pass");
        // 模拟并发 tip 推进：额外 commit 后 first parent ≠ pre_oid。
        fs::write(main.join("sneak.txt"), "race\n").expect("sneak");
        git_test_command(&main, &["add", "sneak.txt"]);
        git_test_command(&main, &["commit", "-m", "concurrent tip"]);
        let advanced = head_hash(&main).expect("advanced").expect("some");
        assert_ne!(advanced, merge_oid);
        let err = verify_merge_oid_binding(&main, &advanced, &pre_oid, &reviewed, "main")
            .expect_err("advanced tip must fail exact parent gate");
        let msg = err.to_string();
        assert!(
            msg.contains("first parent")
                || msg.contains("pre-merge")
                || msg.contains("exactly 2 parents"),
            "unexpected err: {msg}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     crafty merge tip 若 second-parent 是 reviewed 的后代而非 reviewed 本身，
    ///     会把未审改动带进 main；exact parent gate 必须拒绝 ancestor 回退。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 parents=[pre, descendant-of-reviewed] 的 commit-tree tip，
    ///     断言 verify_merge_oid_binding 因 second parent ≠ reviewed 失败。
    #[test]
    fn verify_merge_oid_binding_rejects_second_parent_descendant_of_reviewed() {
        let root = temp_git_dir("workbench-merge-exact-parents");
        let main = root.join("main");
        let feature = root.join("feature");
        fs::create_dir_all(&main).expect("main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        fs::write(main.join("README.md"), "base\n").expect("write");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "initial"]);
        let pre_oid = head_hash(&main).expect("pre").expect("some");
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/crafty",
                feature.to_str().unwrap(),
            ],
        );
        fs::write(feature.join("feat.txt"), "v1\n").expect("feat");
        git_test_command(&feature, &["add", "feat.txt"]);
        git_test_command(&feature, &["commit", "-m", "feat reviewed"]);
        let reviewed = head_hash(&feature).expect("reviewed").expect("oid");
        // 在 reviewed 之上再造一笔未审改动，作为 crafty second-parent 候选。
        fs::write(feature.join("sneak.txt"), "unreviewed\n").expect("sneak");
        git_test_command(&feature, &["add", "sneak.txt"]);
        git_test_command(&feature, &["commit", "-m", "sneak after review"]);
        let descendant = head_hash(&feature).expect("descendant").expect("oid");
        assert_ne!(descendant, reviewed);
        assert!(is_ancestor(&main, &reviewed, &descendant).expect("anc"));
        // 手工构造 first-parent=pre、second-parent=descendant 的 crafty merge tip。
        let crafty_tree =
            git_test_command(&feature, &["rev-parse", &format!("{descendant}^{{tree}}")])
                .trim()
                .to_string();
        let crafty_oid = git_test_command(
            &main,
            &[
                "commit-tree",
                &crafty_tree,
                "-p",
                &pre_oid,
                "-p",
                &descendant,
                "-m",
                "crafty merge tip",
            ],
        )
        .trim()
        .to_string();
        git_test_command(&main, &["update-ref", "refs/heads/main", &crafty_oid]);
        git_test_command(&main, &["reset", "--hard", "HEAD"]);
        let err = verify_merge_oid_binding(&main, &crafty_oid, &pre_oid, &reviewed, "main")
            .expect_err("descendant second-parent must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("second parent") || msg.contains("reviewed oid"),
            "unexpected err: {msg}"
        );
        // 对照：合法 parents=[pre, reviewed] 必须通过。
        let reviewed_tree =
            git_test_command(&feature, &["rev-parse", &format!("{reviewed}^{{tree}}")])
                .trim()
                .to_string();
        let legit_oid = git_test_command(
            &main,
            &[
                "commit-tree",
                &reviewed_tree,
                "-p",
                &pre_oid,
                "-p",
                &reviewed,
                "-m",
                "legitimate merge tip",
            ],
        )
        .trim()
        .to_string();
        git_test_command(&main, &["update-ref", "refs/heads/main", &legit_oid]);
        git_test_command(&main, &["reset", "--hard", "HEAD"]);
        verify_merge_oid_binding(&main, &legit_oid, &pre_oid, &reviewed, "main")
            .expect("exact [pre, reviewed] parents must pass");
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
    ///     冲突解决的长耗时阶段必须完全隔离，真实 main 的文件与 HEAD 在发布前都不能变化。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 main/source 同文件冲突；冻结两端 OID 后在 detached integration worktree merge，
    ///     断言冲突期间真实 main 内容/HEAD 不变；模拟解决并提交，再安全发布并校验精确双父及临时目录清理。
    #[test]
    fn isolated_conflict_keeps_real_main_unchanged_until_strict_publish() {
        let root = temp_git_dir("workbench-isolated-conflict");
        let main = root.join("main");
        let source = root.join("source");
        let integration = root.join("internal").join("merge");
        fs::create_dir_all(&main).expect("create main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("README.md"), "base\n").expect("base");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "base"]);
        git_test_command(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/conflict",
                source.to_str().unwrap(),
            ],
        );
        fs::write(source.join("README.md"), "source\n").expect("source");
        git_test_command(&source, &["commit", "-am", "source"]);
        fs::write(main.join("README.md"), "main\n").expect("main");
        git_test_command(&main, &["commit", "-am", "main"]);
        let frozen = freeze_workbench_merge(&main, &source).expect("freeze");

        create_detached_integration_worktree(&main, &integration, &frozen.main_oid)
            .expect("integration");
        assert_eq!(
            merge_commit_oid(&integration, &frozen.source_oid).expect("merge"),
            MergeBranchOutcome::Conflicted
        );
        assert_eq!(head_hash(&main).unwrap().unwrap(), frozen.main_oid);
        assert_eq!(
            fs::read_to_string(main.join("README.md")).unwrap(),
            "main\n"
        );

        fs::write(integration.join("README.md"), "main\nsource\n").expect("resolved");
        stage_all_merge_resolution(&integration).expect("stage");
        commit_merge_no_edit(&integration).expect("commit merge");
        let merge_oid = head_hash(&integration).unwrap().unwrap();
        verify_strict_merge_commit(
            &integration,
            &merge_oid,
            &frozen.main_oid,
            &frozen.source_oid,
        )
        .expect("strict parents");
        publish_integration_merge(&main, &frozen, &merge_oid).expect("publish");
        assert_eq!(head_hash(&main).unwrap().unwrap(), merge_oid);
        assert_eq!(
            commit_parent_oids(&main, &merge_oid).unwrap(),
            vec![frozen.main_oid, frozen.source_oid]
        );
        remove_integration_worktree(&main, &integration).expect("cleanup");
        assert!(!integration.exists());
        assert!(!list_worktrees(&main, &repo_root(&main).unwrap())
            .unwrap()
            .iter()
            .any(|item| Path::new(&item.path) == integration));
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     隔离合并期间若真实 main 被并发推进，发布必须拒绝且不能覆盖并发提交。
    ///
    /// Code Logic（这个测试做什么）:
    ///     生成合法隔离 merge commit 后推进真实 main，再调用 publish；断言 conflict、main 保留并发 tip，
    ///     最后确认 integration worktree 可完整清理。
    #[test]
    fn publish_rejects_main_drift_without_overwriting_concurrent_commit() {
        let root = temp_git_dir("workbench-publish-drift");
        let main = root.join("main");
        let source = root.join("source");
        let integration = root.join("internal").join("merge");
        fs::create_dir_all(&main).expect("create main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("README.md"), "base\n").expect("base");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "base"]);
        git_test_command(
            &main,
            &["worktree", "add", "-b", "feature", source.to_str().unwrap()],
        );
        fs::write(source.join("source.txt"), "source\n").expect("source");
        git_test_command(&source, &["add", "source.txt"]);
        git_test_command(&source, &["commit", "-m", "source"]);
        let frozen = freeze_workbench_merge(&main, &source).expect("freeze");
        create_detached_integration_worktree(&main, &integration, &frozen.main_oid)
            .expect("integration");
        assert_eq!(
            merge_commit_oid(&integration, &frozen.source_oid).expect("merge"),
            MergeBranchOutcome::Merged
        );
        let merge_oid = head_hash(&integration).unwrap().unwrap();

        fs::write(main.join("concurrent.txt"), "concurrent\n").expect("concurrent");
        git_test_command(&main, &["add", "concurrent.txt"]);
        git_test_command(&main, &["commit", "-m", "concurrent"]);
        let concurrent_oid = head_hash(&main).unwrap().unwrap();
        let error = publish_integration_merge(&main, &frozen, &merge_oid)
            .expect_err("drift must reject publish");
        assert!(error.to_string().contains("HEAD 已变化"));
        assert_eq!(head_hash(&main).unwrap().unwrap(), concurrent_oid);
        assert!(main.join("concurrent.txt").exists());
        assert!(!main.join("source.txt").exists());
        remove_integration_worktree(&main, &integration).expect("cleanup");
        assert!(!integration.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Claude 运行期间源 worktree 的新提交属于用户并发工作，不能被发布旧快照后的 cleanup 删除。
    ///
    /// Code Logic（这个测试做什么）:
    ///     冻结并生成合法 integration merge 后推进 source；发布前 source gate 必须拒绝，main 保持原 OID，
    ///     source 新提交与文件继续存在，integration 可独立清理。
    #[test]
    fn source_advance_rejects_publish_and_preserves_new_source_commit() {
        let root = temp_git_dir("workbench-source-drift");
        let main = root.join("main");
        let source = root.join("source");
        let integration = root.join("internal").join("merge");
        fs::create_dir_all(&main).expect("create main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("README.md"), "base\n").expect("base");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "base"]);
        git_test_command(
            &main,
            &["worktree", "add", "-b", "feature", source.to_str().unwrap()],
        );
        fs::write(source.join("source.txt"), "source\n").expect("source");
        git_test_command(&source, &["add", "source.txt"]);
        git_test_command(&source, &["commit", "-m", "source frozen"]);
        let frozen = freeze_workbench_merge(&main, &source).expect("freeze");
        create_detached_integration_worktree(&main, &integration, &frozen.main_oid)
            .expect("integration");
        assert_eq!(
            merge_commit_oid(&integration, &frozen.source_oid).expect("merge"),
            MergeBranchOutcome::Merged
        );

        fs::write(source.join("after-freeze.txt"), "preserve me\n").expect("new source");
        git_test_command(&source, &["add", "after-freeze.txt"]);
        git_test_command(&source, &["commit", "-m", "source after freeze"]);
        let source_new_oid = head_hash(&source).unwrap().unwrap();
        let error = verify_source_unchanged_for_publish(&source, &frozen, "feature")
            .expect_err("source drift must reject");
        assert!(error.to_string().contains("源 worktree HEAD"));
        assert_eq!(head_hash(&main).unwrap().unwrap(), frozen.main_oid);
        assert_eq!(head_hash(&source).unwrap().unwrap(), source_new_oid);
        assert!(source.join("after-freeze.txt").exists());
        remove_integration_worktree(&main, &integration).expect("cleanup integration");
        assert!(!integration.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     ledger claim 后任一冻结 OID 漂移都必须 fail-closed，禁止执行阶段悄然改用新 tip。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 expected 快照并分别修改 main/source OID，断言 helper 返回对应 HEAD 漂移冲突。
    #[test]
    fn frozen_merge_gate_rejects_main_or_source_head_drift() {
        let expected = FrozenWorkbenchMerge {
            main_branch: "main".to_string(),
            main_oid: "main-old".to_string(),
            source_oid: "source-old".to_string(),
        };
        let mut actual = expected.clone();
        actual.main_oid = "main-new".to_string();
        assert!(ensure_frozen_merge_unchanged(&expected, &actual)
            .expect_err("main drift")
            .to_string()
            .contains("主工作区 HEAD 已变化"));
        actual = expected.clone();
        actual.source_oid = "source-new".to_string();
        assert!(ensure_frozen_merge_unchanged(&expected, &actual)
            .expect_err("source drift")
            .to_string()
            .contains("源 worktree HEAD 已变化"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     integration worktree 不得放在 main 或 source checkout 内，否则任一 watcher 都可能重启 owner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     建 main/source linked worktree，分别选择其子目录作为 integration 目标；断言创建前即拒绝，
    ///     且目标目录和 Git worktree 登记均未出现。
    #[test]
    fn integration_path_inside_main_or_source_checkout_is_rejected() {
        let root = temp_git_dir("workbench-integration-watcher-root");
        let main = root.join("main");
        let source = root.join("source");
        fs::create_dir_all(&main).expect("create main");
        git_test_command(&main, &["init"]);
        git_test_command(&main, &["checkout", "-b", "main"]);
        git_test_command(&main, &["config", "user.name", "Workbench Test"]);
        git_test_command(&main, &["config", "user.email", "test@example.com"]);
        fs::write(main.join("README.md"), "base\n").expect("base");
        git_test_command(&main, &["add", "README.md"]);
        git_test_command(&main, &["commit", "-m", "base"]);
        git_test_command(
            &main,
            &["worktree", "add", "-b", "feature", source.to_str().unwrap()],
        );
        let head = head_hash(&main).unwrap().unwrap();
        let inside_main = main.join("app-data/merge-integrations/op-main");
        let inside_source = source.join("app-data/merge-integrations/op-source");
        for target in [&inside_main, &inside_source] {
            let error = create_detached_integration_worktree_outside(
                &main,
                target,
                &head,
                &[&main, &source],
            )
            .expect_err("watcher root must reject");
            assert!(error.to_string().contains("main/source checkout 之外"));
            assert!(!target.exists());
        }
        let listed = list_worktrees(&main, &repo_root(&main).unwrap()).unwrap();
        assert_eq!(listed.len(), 2, "拒绝路径不得新增 Git worktree 登记");
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
    ///     用户切换 Workbench worktree 时，只应看到该 worktree 当前 HEAD 可达的提交历史，
    ///     不能把同仓库其他未合并分支的提交混入并显示成全项目唯一历史树。
    ///
    /// Code Logic（这个测试做什么）:
    ///     从共同基线创建 linked worktree，让主分支与功能分支分别产生独占提交；分别读取两处历史，
    ///     断言各自包含自己的独占提交，并排除另一个 worktree 尚不可达的提交。
    #[test]
    fn list_commits_scopes_history_to_current_worktree() {
        let root = temp_git_dir("workbench-git-worktree-history");
        let repo = root.join("repo");
        let feature_worktree = root.join("feature-worktree");
        fs::create_dir_all(&repo).expect("create repo dir");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["config", "user.email", "test@example.com"]);
        git_test_command(&repo, &["config", "user.name", "Workbench Test"]);
        fs::write(repo.join("README.md"), "base\n").expect("write base");
        git_test_command(&repo, &["add", "README.md"]);
        git_test_command(&repo, &["commit", "-m", "chore: shared base"]);

        let feature_worktree_path = feature_worktree.to_string_lossy().to_string();
        git_test_command(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature/worktree-history",
                &feature_worktree_path,
                "HEAD",
            ],
        );

        fs::write(repo.join("main-only.txt"), "main\n").expect("write main-only file");
        git_test_command(&repo, &["add", "main-only.txt"]);
        git_test_command(&repo, &["commit", "-m", "feat: main worktree only"]);

        fs::write(feature_worktree.join("feature-only.txt"), "feature\n")
            .expect("write feature-only file");
        git_test_command(&feature_worktree, &["add", "feature-only.txt"]);
        git_test_command(
            &feature_worktree,
            &["commit", "-m", "feat: feature worktree only"],
        );

        let main_commits = list_commits(&repo, 30).expect("list main worktree commits");
        let feature_commits =
            list_commits(&feature_worktree, 30).expect("list feature worktree commits");
        let main_summaries = main_commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect::<Vec<_>>();
        let feature_summaries = feature_commits
            .iter()
            .map(|commit| commit.summary.as_str())
            .collect::<Vec<_>>();

        assert!(main_summaries.contains(&"feat: main worktree only"));
        assert!(!main_summaries.contains(&"feat: feature worktree only"));
        assert!(feature_summaries.contains(&"feat: feature worktree only"));
        assert!(!feature_summaries.contains(&"feat: main worktree only"));

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

    // ---- hook-failure 检测 ----

    #[test]
    fn known_non_hook_markers_match_case_insensitively() {
        // push 远端拒绝必须排除（用户明确要求只修钩子脚本，不修 push 拒绝）。
        assert!(is_known_non_hook_git_failure("! [rejected] main -> main"));
        assert!(is_known_non_hook_git_failure(
            "Updates were rejected: non-fast-forward"
        ));
        assert!(is_known_non_hook_git_failure("Please fetch first"));
        // 身份未配置
        assert!(is_known_non_hook_git_failure("Author identity unknown"));
        // 合并冲突
        assert!(is_known_non_hook_git_failure(
            "Automatic merge failed; fix conflicts"
        ));
        // 普通钩子输出不应被误判为非钩子失败
        assert!(!is_known_non_hook_git_failure(
            "Error: eslint found 3 problems"
        ));
        assert!(!is_known_non_hook_git_failure("✖ ruff check failed"));
    }

    #[test]
    fn hook_failure_dto_summary_and_combined_output() {
        let with_stderr = WorkbenchHookFailureDto {
            stage: WorkbenchHookStage::PreCommit,
            stdout: "stdout noise".to_string(),
            stderr: "  eslint: 2 errors  ".to_string(),
            exit_code: Some(1),
        };
        assert_eq!(with_stderr.summary(), "pre-commit 钩子失败");
        // stderr 非空时只用 stderr（trim）。
        assert_eq!(with_stderr.combined_output(), "eslint: 2 errors");
        let stderr_empty = WorkbenchHookFailureDto {
            stage: WorkbenchHookStage::PrePush,
            stdout: "  build failed  ".to_string(),
            stderr: String::new(),
            exit_code: Some(2),
        };
        assert_eq!(stderr_empty.summary(), "pre-push 钩子失败");
        // stderr 空时回退 stdout。
        assert_eq!(stderr_empty.combined_output(), "build failed");
    }

    #[test]
    fn detect_hook_failure_requires_installed_hook_and_ignores_known_markers() {
        let repo = temp_git_dir("hook-detect");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        git_test_command(&repo, &["init"]);
        // 没有安装钩子时，任何失败都不判为钩子失败。
        let failure = GitCommandFailure {
            stdout: String::new(),
            stderr: "some error".to_string(),
            exit_code: Some(1),
        };
        assert!(detect_hook_failure(WorkbenchHookStage::PreCommit, &repo, &failure).is_none());
        // 已知非钩子标记即使安装了钩子也不判为钩子失败。
        std::fs::write(
            repo.join(".git").join("hooks").join("pre-commit"),
            "#!/bin/sh\nexit 1\n",
        )
        .expect("write hook");
        let rejected = GitCommandFailure {
            stdout: String::new(),
            stderr: "! [rejected] main -> main (non-fast-forward)".to_string(),
            exit_code: Some(1),
        };
        assert!(detect_hook_failure(WorkbenchHookStage::PreCommit, &repo, &rejected).is_none());
        // 安装了钩子且非已知标记 → 判为钩子失败。
        let hook_err = GitCommandFailure {
            stdout: String::new(),
            stderr: "lint failed: 2 errors".to_string(),
            exit_code: Some(1),
        };
        let detected =
            detect_hook_failure(WorkbenchHookStage::PreCommit, &repo, &hook_err).expect("hook");
        assert_eq!(detected.stage, WorkbenchHookStage::PreCommit);
        assert_eq!(detected.exit_code, Some(1));
        assert!(detected.stderr.contains("lint failed"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// 真实 git + 可执行 pre-commit 钩子：commit_staged_checked 必须返回 Hook 分类。
    /// 仅 Unix（Windows 钩子可执行性语义不同，跨平台 smoke 不覆盖）。
    #[cfg(unix)]
    #[test]
    fn commit_staged_checked_classifies_failing_pre_commit_hook_as_hook() {
        use std::os::unix::fs::PermissionsExt;
        let repo = temp_git_dir("hook-commit-checked");
        std::fs::create_dir_all(&repo).expect("mkdir repo");
        git_test_command(&repo, &["init"]);
        git_test_command(&repo, &["config", "user.name", "Test"]);
        git_test_command(&repo, &["config", "user.email", "t@e.com"]);
        std::fs::write(repo.join("a.txt"), "v1\n").expect("write a");
        git_test_command(&repo, &["add", "a.txt"]);
        git_test_command(&repo, &["commit", "-m", "init"]);
        // 安装一个会失败的 pre-commit 钩子。
        let hook = repo.join(".git").join("hooks").join("pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\necho 'format failed: 1 issue' >&2\nexit 1\n",
        )
        .expect("write hook");
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
        // 制造新改动并 stage。
        std::fs::write(repo.join("a.txt"), "v2\n").expect("modify a");
        git_test_command(&repo, &["add", "a.txt"]);
        match commit_staged_checked(&repo, "second") {
            Err(MutationExecError::Hook(h)) => {
                assert_eq!(h.stage, WorkbenchHookStage::PreCommit);
                assert!(h.stderr.contains("format failed"));
            }
            other => panic!("expected Hook, got {:?}", other),
        }
        // 移除钩子后同样改动应正常提交（返回 Ok）。
        let _ = std::fs::remove_file(&hook);
        std::fs::write(repo.join("a.txt"), "v3\n").expect("modify a again");
        git_test_command(&repo, &["add", "a.txt"]);
        commit_staged_checked(&repo, "third").expect("commit without hook");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
