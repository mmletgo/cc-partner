//! cloud_sync/git_cli.rs — 系统 git CLI 封装
//!
//! Business Logic（为什么需要这个模块）:
//!     云端同步用 GitHub 私有仓库作中心化对端，但应用自身不实现 git 协议、不管理认证——
//!     直接调用本机已配置好的 git CLI（用户的 SSH key / credential helper / token），最省事且
//!     与用户既有 git 环境无缝衔接。需要一个统一封装：探测 git 是否可用、在指定 workdir 跑
//!     子命令、捕获 stdout/stderr、超时控制、非零退出转友好中文错误。
//!
//! Code Logic（这个模块做什么）:
//!     基于 tokio::process::Command（async），统一用 run() 跑子命令并 timeout 包裹；
//!     clone/fetch/push 这类可能耗时的网络操作给 180s，其余给 30s。
//!     push 失败需区分"被远端拒绝（需 fetch 后重试）"与"普通失败"，故用 PushError 枚举。
//!     全程绝不 panic，错误一律转 AppError 友好中文提示。

use crate::error::AppError;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// git clone/fetch/push 等网络子命令的超时（秒）。
const NETWORK_TIMEOUT_SECS: u64 = 180;
/// 其余本地子命令（config/status/commit/reset 等）的超时（秒）。
const LOCAL_TIMEOUT_SECS: u64 = 30;

/// push 失败的分类错误，便于上层决定是否 fetch 后重试。
///
/// Business Logic: 多设备并发 push 同一分支时，后 push 的一端会被远端以
///     "non-fast-forward"/"rejected" 拒绝。这种失败是可恢复的——fetch 最新后再同步一轮
///     通常即可收敛。其他失败（认证失败、仓库不存在等）不可恢复，直接报错。
#[derive(Debug)]
pub enum PushError {
    /// 远端拒绝（rejected / non-fast-forward / fetch first）——可 fetch 后重试。
    Rejected,
    /// 其他失败（认证、网络、仓库问题等），携带底层 AppError 供上报。
    Other(AppError),
}

/// 探测系统 git 是否可用，返回 git 可执行路径（恒为 "git"，依赖 PATH 解析）。
///
/// Business Logic: 云端同步强依赖本机 git。启动/测试连通前需确认 git 存在；
///     不存在时给出平台相关安装提示（Windows 提示 Git for Windows）。
/// Code Logic: 跑 `git --version`，成功即可用（返回 PathBuf::from("git")）；
///     失败转 AppError::generic 友好中文提示。
pub fn detect_git() -> Result<PathBuf, AppError> {
    let git = PathBuf::from("git");
    // 用同步 std::process 探测（启动期/测试连通时调用，无需 async 开销；探测本身很快）
    match std::process::Command::new(&git).arg("--version").output() {
        Ok(out) if out.status.success() => Ok(git),
        _ => {
            let hint = if cfg!(target_os = "windows") {
                "（Windows 请安装 Git for Windows: https://git-scm.com/download/win 并确保 git 在 PATH 中）"
            } else {
                "（请安装 git 并确保 git 在 PATH 中）"
            };
            Err(AppError::generic(format!("未检测到可用的 git 命令{hint}")))
        }
    }
}

/// 在指定 workdir 跑一个 git 子命令，返回 stdout（trim 后）。
///
/// Business Logic: 所有 git 操作的统一入口：设工作目录、捕获 stdout/stderr、超时保护、
///     非零退出转友好中文错误（含 stderr 便于诊断）。
///
/// Code Logic:
/// 1. Command::new(git).current_dir(workdir).args(args)；
/// 2. stdin inherit 关闭、stdout/stderr piped 捕获；
/// 3. tokio::time::timeout 包裹 .wait_with_output()，超时转 AppError；
/// 4. 非零退出转 AppError::generic（消息含 stderr 末段）；
/// 5. 成功返回 stdout 的 String（trim 掉首尾空白）。
pub async fn run(
    git: &Path,
    workdir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, AppError> {
    let mut cmd = Command::new(git);
    cmd.current_dir(workdir).args(args);
    // stdin 关闭避免子进程阻塞等待输入
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| AppError::generic(format!("启动 git 失败（{}）: {e}", args.join(" "))))?;

    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(AppError::generic(format!(
                "执行 git 失败（{}）: {e}",
                args.join(" ")
            )));
        }
        Err(_) => {
            return Err(AppError::generic(format!(
                "执行 git 超时（{}，{}秒）",
                args.join(" "),
                timeout.as_secs()
            )));
        }
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(AppError::generic(format!(
            "git 命令失败（{}）: {}",
            args.join(" "),
            detail
        )));
    }

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(stdout)
}

/// 取 git 版本字符串（`git --version` 的 stdout，如 "git version 2.43.0"）。
///
/// Business Logic: 测试连通时回显 git 版本，供前端展示/诊断。
/// Code Logic: 不需 workdir，直接在当前目录跑 `--version`，30s 超时。
pub async fn git_version(git: &Path) -> Result<String, AppError> {
    // --version 不依赖 workdir，用临时当前目录即可
    run(
        git,
        Path::new("."),
        &["--version"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await
}

/// clone 一个仓库到 workdir（workdir 必须不存在或为空）。
///
/// Business Logic: 首次同步前需把远端仓库 clone 到本地工作目录作为同步工作区。
/// Code Logic: `git clone <url> <workdir>`，180s 超时（网络操作）。
pub async fn clone(git: &Path, url: &str, workdir: &Path) -> Result<(), AppError> {
    let workdir_str = workdir
        .to_str()
        .ok_or_else(|| AppError::generic(format!("工作目录路径含非 UTF-8 字符: {workdir:?}")))?;
    run(
        git,
        Path::new("."),
        &["clone", url, workdir_str],
        Duration::from_secs(NETWORK_TIMEOUT_SECS),
    )
    .await?;
    Ok(())
}

/// 设置本地仓库的提交身份（仅 local，不污染用户全局 git 配置）。
///
/// Business Logic: 同步产生的 commit 需有 author。复用一个固定的应用身份，避免依赖
///     用户全局 git user.name/user.email 是否已配置（CI/全新机器可能未配）。
/// Code Logic: `git config --local user.name "Claude Partner"` + `user.email`，30s 超时。
pub async fn set_local_identity(git: &Path, workdir: &Path) -> Result<(), AppError> {
    run(
        git,
        workdir,
        &["config", "user.name", "Claude Partner"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;
    run(
        git,
        workdir,
        &["config", "user.email", "claude-partner@local"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;
    Ok(())
}

/// 返回当前 HEAD 指向的分支名；空仓库（unborn HEAD）返回 None。
///
/// Business Logic: clone 一个全新空仓库后 HEAD 尚未指向任何 commit（unborn），
///     此时取分支名会失败，需识别为 None 而非错误。
/// Code Logic: `git symbolic-ref --short HEAD`，退出非零且 stderr 含 "HEAD" 视为 unborn→None，
///     其他错误正常抛出。
#[allow(dead_code)]
pub async fn current_branch(git: &Path, workdir: &Path) -> Result<Option<String>, AppError> {
    let mut cmd = Command::new(git);
    cmd.current_dir(workdir)
        .args(["symbolic-ref", "--short", "HEAD"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| AppError::generic(format!("启动 git 失败（symbolic-ref）: {e}")))?;
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| AppError::generic(format!("执行 git 失败（symbolic-ref）: {e}")))?;

    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            return Ok(None);
        }
        return Ok(Some(s));
    }
    // unborn HEAD：symbolic-ref 非零退出，stderr 通常含 "HEAD"（ref 名未解析）
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("HEAD") || stderr.to_lowercase().contains("no such") {
        return Ok(None);
    }
    Err(AppError::generic(format!(
        "git symbolic-ref 失败: {}",
        stderr.trim()
    )))
}

/// 解析远端默认分支名（origin/HEAD 指向的分支）。
///
/// Business Logic: 用户未显式配置 cloud_sync_branch 时，使用远端默认分支同步。
/// Code Logic: 优先 `git rev-parse --abbrev-ref origin/HEAD`；失败回退 `git remote show origin`
///     解析 "HEAD branch:" 行。两者都失败则转 AppError（调用方可回退到 current_branch）。
pub async fn default_remote_branch(git: &Path, workdir: &Path) -> Result<String, AppError> {
    // 优先 rev-parse origin/HEAD（快、本地解析）
    let parsed = run(
        git,
        workdir,
        &["rev-parse", "--abbrev-ref", "origin/HEAD"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await;
    if let Ok(s) = parsed {
        let trimmed = s.trim();
        // origin/HEAD 解析结果形如 "origin/main"，去 "origin/" 前缀
        if let Some(branch) = trimmed.strip_prefix("origin/") {
            if !branch.is_empty() {
                return Ok(branch.to_string());
            }
        }
        if !trimmed.is_empty() && trimmed != "origin/HEAD" {
            return Ok(trimmed.to_string());
        }
    }

    // 回退：remote show origin（需网络，解析 "  HEAD branch: main" 行）
    let remote_info = run(
        git,
        workdir,
        &["remote", "show", "origin"],
        Duration::from_secs(NETWORK_TIMEOUT_SECS),
    )
    .await?;
    for line in remote_info.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("HEAD branch:") {
            let branch = rest.trim();
            if !branch.is_empty() {
                return Ok(branch.to_string());
            }
        }
    }
    Err(AppError::generic(
        "无法解析远端默认分支（origin/HEAD 未设置且 remote show origin 失败）",
    ))
}

/// fetch origin（拉取远端引用到本地，不合并）。
///
/// Business Logic: push 被拒后或同步前需先 fetch 远端最新引用，供 reset --hard 对齐。
/// Code Logic: `git fetch origin`，180s 超时。
pub async fn fetch_origin(git: &Path, workdir: &Path) -> Result<(), AppError> {
    run(
        git,
        workdir,
        &["fetch", "origin"],
        Duration::from_secs(NETWORK_TIMEOUT_SECS),
    )
    .await?;
    Ok(())
}

/// 把当前分支硬重置到 origin/<branch>，丢弃本地工作区与索引差异。
///
/// Business Logic: 同步前需以远端权威状态作为 import 基线，丢弃本地未提交的脏改动
///     （本地权威在 SQLite，工作区只是临时载体，脏改动无意义）。
/// Code Logic: `git reset --hard origin/<branch>`，30s 超时。
pub async fn reset_hard(git: &Path, workdir: &Path, branch: &str) -> Result<(), AppError> {
    let target = format!("origin/{branch}");
    run(
        git,
        workdir,
        &["reset", "--hard", &target],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;
    Ok(())
}

/// 把全部改动（含新增/删除）加入索引并提交。
///
/// Business Logic: export 把本地权威写回工作区后，需 commit 成一个新版本供 push。
/// Code Logic: `git add -A`；`git status --porcelain` 判空——无变化返回 false（无需 push）；
///     有变化则 `git commit -m <msg>` 返回 true，30s 超时。
pub async fn commit_all(git: &Path, workdir: &Path, msg: &str) -> Result<bool, AppError> {
    run(
        git,
        workdir,
        &["add", "-A"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;

    let status = run(
        git,
        workdir,
        &["status", "--porcelain"],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;
    if status.trim().is_empty() {
        // 无变化，无需 commit
        return Ok(false);
    }

    run(
        git,
        workdir,
        &["commit", "-m", msg],
        Duration::from_secs(LOCAL_TIMEOUT_SECS),
    )
    .await?;
    Ok(true)
}

/// push 当前分支到 origin/<branch>，区分被拒（可重试）与其他失败。
///
/// Business Logic: 多设备并发时后 push 会被远端拒绝，需识别此场景让上层 fetch+重试收敛。
///     其他失败（认证、网络等）直接上报。
/// Code Logic: `git push origin <branch>`，180s 超时；非零退出时检查 stderr/stdout 是否含
///     "rejected"/"non-fast-forward"/"fetch first" 任一关键词 → PushError::Rejected，
///     否则 PushError::Other(AppError)。
pub async fn push(git: &Path, workdir: &Path, branch: &str) -> Result<(), PushError> {
    let mut cmd = Command::new(git);
    cmd.current_dir(workdir)
        .args(["push", "origin", branch])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(PushError::Other(AppError::generic(format!(
                "启动 git push 失败: {e}"
            ))))
        }
    };

    let out = match tokio::time::timeout(
        Duration::from_secs(NETWORK_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Err(PushError::Other(AppError::generic(format!(
                "执行 git push 失败: {e}"
            ))))
        }
        Err(_) => {
            return Err(PushError::Other(AppError::generic(format!(
                "执行 git push 超时（{}秒）",
                NETWORK_TIMEOUT_SECS
            ))))
        }
    };

    if out.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{}\n{}", stderr, stdout).to_lowercase();
    let detail = if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        stdout.trim().to_string()
    };

    // 远端拒绝关键词（non-fast-forward / rejected / fetch first）
    if combined.contains("rejected")
        || combined.contains("non-fast-forward")
        || combined.contains("fetch first")
    {
        return Err(PushError::Rejected);
    }
    Err(PushError::Other(AppError::generic(format!(
        "git push 失败: {detail}"
    ))))
}

#[cfg(test)]
mod tests {
    //! git_cli 单测：覆盖可纯函数化测试的部分（run 参数拼装行为通过 detect_git 的环境相关性
    //! 跳过），重点验证错误分类逻辑与 detect_git 在有/无 git 环境下的行为不 panic。

    use super::*;

    /// detect_git 在有 git 的环境返回 "git"，无 git 环境返回 Err（不 panic）。
    /// 这是环境相关测试：CI/开发机通常有 git，断言 Ok；极少数无 git 环境断言 Err。
    #[test]
    fn detect_git_does_not_panic() {
        let result = detect_git();
        match result {
            Ok(p) => assert_eq!(p, PathBuf::from("git")),
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("git"), "错误消息应提及 git: {msg}");
            }
        }
    }

    /// 超时常量合理：网络操作 180s，本地操作 30s。
    #[test]
    fn timeouts_are_sane() {
        assert_eq!(NETWORK_TIMEOUT_SECS, 180);
        assert_eq!(LOCAL_TIMEOUT_SECS, 30);
    }

    /// PushError 变体可构造（保证枚举形态稳定，上层匹配不漏分支）。
    #[test]
    fn push_error_variants_constructible() {
        let _rejected = PushError::Rejected;
        let _other = PushError::Other(AppError::generic("test"));
        // 匹配两个变体确保未来扩展时编译器提醒
        fn classify(e: PushError) -> &'static str {
            match e {
                PushError::Rejected => "rejected",
                PushError::Other(_) => "other",
            }
        }
        assert_eq!(classify(PushError::Rejected), "rejected");
        assert_eq!(classify(PushError::Other(AppError::generic("x"))), "other");
    }
}
