//! workbench/projects.rs — 工作台项目辅助逻辑
//!
//! Business Logic（为什么需要这个模块）:
//!     添加项目时需要校验目录存在、生成显示名，并保证后续文件操作不能逃出项目根目录。
//!
//! Code Logic（这个模块做什么）:
//!     提供 infer_project_name、canonical_project_root、resolve_project_path 三个纯辅助。

#![allow(dead_code)]

use crate::error::AppError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Business Logic（为什么需要这个函数）:
///     用户选择目录后，左侧项目卡片需要一个可读名称。
///
/// Code Logic（这个函数做什么）:
///     取路径最后一段作为项目名，取不到时回退为完整路径字符串。
pub fn infer_project_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Business Logic（为什么需要这个函数）:
///     工作台只允许添加真实存在的本机目录，避免后续 PTY cwd 或文件树读取失败。
///
/// Code Logic（这个函数做什么）:
///     canonicalize 输入路径，要求结果是目录。
pub fn canonical_project_root(path: &str) -> Result<PathBuf, AppError> {
    let root = PathBuf::from(path)
        .canonicalize()
        .map_err(|error| AppError::generic(format!("项目路径不可访问: {error}")))?;
    if !root.is_dir() {
        return Err(AppError::generic("项目路径必须是文件夹"));
    }
    Ok(root)
}

/// Business Logic（为什么需要这个函数）:
///     文件树操作必须限制在项目根目录内，防止通过 `../` 误删或读取项目外文件。
///
/// Code Logic（这个函数做什么）:
///     把相对路径拼到 canonical root 后 canonicalize，并校验结果仍以 root 开头。
pub fn resolve_project_path(root: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| AppError::generic(format!("项目路径不可访问: {error}")))?;
    if !canonical_root.is_dir() {
        return Err(AppError::generic("项目路径必须是文件夹"));
    }

    let target = if relative.trim().is_empty() {
        canonical_root.clone()
    } else {
        canonical_root.join(relative)
    };
    let canonical = target
        .canonicalize()
        .map_err(|error| AppError::generic(format!("路径不可访问: {error}")))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    Ok(canonical)
}

/// 规范化 Git remote URL 为可移植 fingerprint。
///
/// Business Logic（为什么需要这个函数）:
///     跨设备按 remote 提议 Hub 项目映射时，需要忽略尾部 `.git`/斜杠与大小写差异。
///
/// Code Logic（这个函数做什么）:
///     trim → 去尾部 `/` → 去尾部不区分大小写的 `.git` → 再去尾部 `/` → 整体 lowercase。
pub fn normalize_git_remote_fingerprint(url: &str) -> String {
    let mut s = url.trim().to_string();
    while s.ends_with('/') {
        s.pop();
    }
    if s.len() >= 4 {
        let tail = &s[s.len() - 4..];
        if tail.eq_ignore_ascii_case(".git") {
            s.truncate(s.len() - 4);
        }
    }
    while s.ends_with('/') {
        s.pop();
    }
    s.make_ascii_lowercase();
    s
}

/// 把 Git remote 收成「host/owner/repo」，让 SSH 与 HTTPS 指向同一仓库时 fingerprint 相同。
///
/// Business Logic（为什么需要这个函数）:
///     工作台按仓库合并列表项；同一 GitHub/GitLab 仓的 git@ 与 https:// 必须合成一项。
///
/// Code Logic（这个函数做什么）:
///     先走 strip `.git`/大小写，再把 `git@host:path` 与 `scheme://[user@]host[:port]/path` 收成 `host/path`，
///     最后把 GitHub SSH-over-443 主机 `ssh.github.com` 映射为 `github.com`。
pub fn canonical_git_remote_fingerprint(url: &str) -> String {
    let s = normalize_git_remote_fingerprint(url);
    if let Some(rest) = s.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            let path = path.trim_start_matches('/');
            if !host.is_empty() && !path.is_empty() {
                return alias_canonical_git_remote_host(format!("{host}/{path}"));
            }
        }
    }
    if let Some(scheme_end) = s.find("://") {
        let rest = &s[scheme_end + 3..];
        let rest = rest
            .split_once('@')
            .map(|(_, hostpath)| hostpath)
            .unwrap_or(rest);
        let rest = rest.trim_start_matches('/');
        if let Some((hostport, path)) = rest.split_once('/') {
            let host = hostport.split(':').next().unwrap_or(hostport);
            if !host.is_empty() && !path.is_empty() {
                return alias_canonical_git_remote_host(format!("{host}/{path}"));
            }
        }
    }
    alias_canonical_git_remote_host(s)
}

/// 把已知 Git 主机别名收成合并用的 canonical host。
///
/// Business Logic（为什么需要这个函数）:
///     GitHub 走 443 的 SSH 入口是 `ssh.github.com`，与 HTTPS `github.com` 是同一仓库。
///
/// Code Logic（这个函数做什么）:
///     fingerprint 以 `ssh.github.com/` 开头且后面非空时，改写为 `github.com/{path}`。
fn alias_canonical_git_remote_host(fingerprint: String) -> String {
    const SSH_GITHUB_PREFIX: &str = "ssh.github.com/";
    match fingerprint.strip_prefix(SSH_GITHUB_PREFIX) {
        Some(path) if !path.is_empty() => format!("github.com/{path}"),
        _ => fingerprint,
    }
}

/// 读取仓库 origin（或首个 remote）URL。
///
/// Business Logic（为什么需要这个函数）:
///     project opt-in 需要可选的 Git remote fingerprint 作为跨设备身份提示。
///
/// Code Logic（这个函数做什么）:
///     优先 `git remote get-url origin`；失败则 `git remote` 取第一个名称再 get-url；都失败返回 None。
pub fn read_git_remote_url(repo_path: &Path) -> Option<String> {
    if let Some(url) = git_remote_get_url(repo_path, "origin") {
        return Some(url);
    }
    let remotes = git_remote_list(repo_path)?;
    let first = remotes.into_iter().next()?;
    git_remote_get_url(repo_path, &first)
}

/// 读取并规范化 Git remote fingerprint。
///
/// Business Logic（为什么需要这个函数）:
///     刷新整合与添加项目需要把 origin 写成可比较的身份；git 无法执行时必须与「没有 remote」区分，以免误清分组。
///
/// Code Logic（这个函数做什么）:
///     spawn `git remote`：进程启动失败返回 Err；非 git 目录或无 remote 返回 Ok(None)；否则 normalize origin/首个 remote。
pub fn read_git_remote_fingerprint(repo_path: &Path) -> Result<Option<String>, AppError> {
    let output = Command::new("git")
        .args(["remote"])
        .current_dir(repo_path)
        .output()
        .map_err(|error| AppError::generic(format!("无法执行 git: {error}")))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(read_git_remote_url(repo_path)
        .map(|url| canonical_git_remote_fingerprint(&url))
        .filter(|fingerprint| !fingerprint.is_empty()))
}

/// path_info 线上的 fingerprint：有 remote 为规范化 URL，无 remote 为空串；git 无法执行则 None（省略字段）。
///
/// Business Logic（为什么需要这个函数）:
///     控制端刷新远端 shortcut 时，必须区分旧对端省略字段、新对端确认无 remote、以及扫描失败。
///
/// Code Logic（这个函数做什么）:
///     Ok(Some) → Some(fp)；Ok(None) → Some("")；Err → None。
pub fn git_remote_fingerprint_wire(repo_path: &Path) -> Option<String> {
    match read_git_remote_fingerprint(repo_path) {
        Ok(Some(fingerprint)) => Some(fingerprint),
        Ok(None) => Some(String::new()),
        Err(_) => None,
    }
}

/// 执行 `git remote get-url <name>`。
///
/// Business Logic（为什么需要这个函数）:
///     fingerprint 读取需要具体 remote 的 URL。
///
/// Code Logic（这个函数做什么）:
///     成功且 stdout 非空时返回 trim 后的 URL。
fn git_remote_get_url(repo_path: &Path, name: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", name])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

/// 列出 remote 名称。
///
/// Business Logic（为什么需要这个函数）:
///     origin 缺失时回退到仓库中第一个 remote。
///
/// Code Logic（这个函数做什么）:
///     `git remote` 按行 trim 非空名称。
fn git_remote_list(repo_path: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args(["remote"])
        .current_dir(repo_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let list = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if list.is_empty() {
        None
    } else {
        Some(list)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_git_remote_fingerprint, normalize_git_remote_fingerprint,
        read_git_remote_fingerprint, resolve_project_path,
    };
    use std::fs;
    use std::path::PathBuf;

    /// Business Logic（为什么需要这个函数）:
    ///     文件系统测试需要互不影响的临时项目根目录，避免污染用户仓库。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在系统临时目录下创建带 UUID 的目录并返回路径。
    fn temp_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ccp-workbench-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台文件操作不能通过父级路径访问项目外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 `../outside.txt` 逃逸路径，并断言解析被拒绝。
    #[tokio::test]
    async fn resolve_rejects_parent_escape() {
        let root = temp_root();
        let outside = root.parent().expect("temp root parent").join("outside.txt");
        fs::write(&outside, "outside").expect("write outside");

        let result = resolve_project_path(&root, "../outside.txt");

        assert!(result.is_err());
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Hub 跨设备 fingerprint 必须稳定忽略 `.git` 后缀与大小写。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 https URL 规范化结果。
    #[test]
    fn fingerprint_normalizes_https_origin() {
        assert_eq!(
            normalize_git_remote_fingerprint("https://GitHub.com/Org/Repo.git/"),
            "https://github.com/org/repo"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     工作台合并必须把 SSH 与 HTTPS 的同一仓库当成一项。
    ///
    /// Code Logic（这个测试做什么）:
    ///     三种 origin 写法得到同一 canonical fingerprint。
    #[test]
    fn canonical_fingerprint_unifies_ssh_and_https() {
        assert_eq!(
            canonical_git_remote_fingerprint("https://GitHub.com/Org/Repo.git/"),
            "github.com/org/repo"
        );
        assert_eq!(
            canonical_git_remote_fingerprint("git@GitHub.com:Org/Repo.git"),
            "github.com/org/repo"
        );
        assert_eq!(
            canonical_git_remote_fingerprint("ssh://git@github.com/Org/Repo.git"),
            "github.com/org/repo"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     GitHub 走 443 的 SSH 入口主机是 `ssh.github.com`，与 HTTPS 的 `github.com` 是同一仓库。
    ///
    /// Code Logic（这个测试做什么）:
    ///     ssh://、git@ 与已收成 host/path 的三种写法都必须得到 `github.com/owner/repo`。
    #[test]
    fn canonical_fingerprint_maps_ssh_github_host() {
        assert_eq!(
            canonical_git_remote_fingerprint("ssh://git@ssh.github.com:443/mmletgo/cc-partner.git"),
            "github.com/mmletgo/cc-partner"
        );
        assert_eq!(
            canonical_git_remote_fingerprint("git@ssh.github.com:mmletgo/cc-partner.git"),
            "github.com/mmletgo/cc-partner"
        );
        assert_eq!(
            canonical_git_remote_fingerprint("ssh.github.com/mmletgo/cc-partner"),
            "github.com/mmletgo/cc-partner"
        );
        assert_eq!(
            canonical_git_remote_fingerprint("https://github.com/mmletgo/cc-partner.git"),
            "github.com/mmletgo/cc-partner"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     刷新整合依赖 origin 扫描；SSH/HTTPS 必须得到同一 fingerprint。
    ///
    /// Code Logic（这个测试做什么）:
    ///     临时 git 仓设 origin 为 SSH URL，断言规范化结果。
    #[test]
    fn read_git_remote_fingerprint_from_origin() {
        let root = temp_root();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init");
        std::process::Command::new("git")
            .args(["remote", "add", "origin", "git@GitHub.com:Org/Repo.git"])
            .current_dir(&root)
            .output()
            .expect("git remote add");
        let fingerprint = read_git_remote_fingerprint(&root).expect("scan");
        assert_eq!(fingerprint.as_deref(), Some("github.com/org/repo"));
        let _ = fs::remove_dir_all(root);
    }
}
