//! workbench/remote_directory.rs — Workbench 远端目录浏览辅助
//!
//! Business Logic（为什么需要这个模块）:
//!     用户从局域网设备添加远端项目时，需要在对端设备上浏览目录并识别可打开的项目文件夹。
//!
//! Code Logic（这个模块做什么）:
//!     提供远端根目录、目录列表、路径信息和 Git 仓库检测的纯文件系统 helper。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::fs::validate_child_name;
use crate::workbench::models::{
    WorkbenchRemoteDirectoryEntryDto, WorkbenchRemotePathInfoDto, WorkbenchRemoteRootDto,
};
use crate::workbench::projects::infer_project_name;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 打开为空目录时 `git init` 可忽略的系统垃圾文件名（精确匹配）。
const GIT_INIT_IGNORE_NAMES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini", ".localized"];

/// Business Logic（为什么需要这个函数）:
///     远端目录浏览和路径详情都需要向前端展示可读的修改时间。
///
/// Code Logic（这个函数做什么）:
///     从 metadata.modified() 读取系统时间，并转换成 UTC RFC3339 字符串；平台不支持时返回 None。
fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    metadata.modified().ok().map(|time| {
        let datetime: DateTime<Utc> = time.into();
        datetime.to_rfc3339()
    })
}

/// Business Logic（为什么需要这个函数）:
///     目录选择器需要标记一个目录是否已经是 Git 仓库，帮助用户判断能否直接作为项目打开。
///
/// Code Logic（这个函数做什么）:
///     仅对目录检查其下 `.git` 路径是否存在，普通文件直接返回 false。
fn is_git_repo(path: &Path, is_dir: bool) -> bool {
    is_dir && path.join(".git").exists()
}

/// Business Logic（为什么需要这个函数）:
///     根目录列表可能由多个来源生成同一路径，需要去重以免前端显示重复入口。
///
/// Code Logic（这个函数做什么）:
///     检查路径是目录后按显示字符串去重，并追加为远端根目录 DTO。
fn push_root(
    roots: &mut Vec<WorkbenchRemoteRootDto>,
    seen: &mut HashSet<String>,
    label: impl Into<String>,
    path: PathBuf,
) {
    if !path.is_dir() {
        return;
    }
    let path_text = path.display().to_string();
    if seen.insert(path_text.clone()) {
        roots.push(WorkbenchRemoteRootDto {
            label: label.into(),
            path: path_text,
            kind: "dir".to_string(),
        });
    }
}

/// Business Logic（为什么需要这个函数）:
///     Windows 远端设备可能有多个可浏览盘符，根目录选择器应暴露存在的盘符入口。
///
/// Code Logic（这个函数做什么）:
///     在 Windows 上扫描 A-Z 盘符并追加存在的根路径；其他平台为空实现。
#[cfg(windows)]
fn push_platform_roots(roots: &mut Vec<WorkbenchRemoteRootDto>, seen: &mut HashSet<String>) {
    for letter in b'A'..=b'Z' {
        let drive = format!("{}:\\", letter as char);
        push_root(roots, seen, drive.clone(), PathBuf::from(drive));
    }
}

/// Business Logic（为什么需要这个函数）:
///     Unix 远端设备使用单一文件系统根，目录选择器需要提供从根目录开始浏览的入口。
///
/// Code Logic（这个函数做什么）:
///     在非 Windows 平台追加 `/` 根路径。
#[cfg(not(windows))]
fn push_platform_roots(roots: &mut Vec<WorkbenchRemoteRootDto>, seen: &mut HashSet<String>) {
    push_root(roots, seen, "文件系统", PathBuf::from("/"));
}

/// Business Logic（为什么需要这个函数）:
///     常用代码目录通常位于用户 home 下，远端项目选择器应把它们作为快捷入口。
///
/// Code Logic（这个函数做什么）:
///     为 home 目录下的 `web_project`、`projects`、`workspace` 追加存在的目录入口。
fn push_common_code_roots(
    roots: &mut Vec<WorkbenchRemoteRootDto>,
    seen: &mut HashSet<String>,
    home: &Path,
) {
    for name in ["web_project", "projects", "workspace"] {
        push_root(roots, seen, name, home.join(name));
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端目录浏览需要把标准库 DirEntry 转成稳定的前端目录条目。
///
/// Code Logic（这个函数做什么）:
///     读取 metadata、名称、路径、类型、修改时间和 Git 仓库标识，生成 camelCase DTO。
fn entry_from_path(path: &Path) -> Result<WorkbenchRemoteDirectoryEntryDto, AppError> {
    let metadata = fs::metadata(path)?;
    let is_dir = metadata.is_dir();
    Ok(WorkbenchRemoteDirectoryEntryDto {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        path: path.display().to_string(),
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        modified_at: modified_at(&metadata),
        is_git_repo: is_git_repo(path, is_dir),
    })
}

/// Business Logic（为什么需要这个函数）:
///     远端目录列表在不同平台和文件系统上应保持稳定顺序，避免前端列表抖动。
///
/// Code Logic（这个函数做什么）:
///     先目录后文件；同类型先按小写名称升序，小写相等时按原始名称升序。
fn sort_entries(entries: &mut [WorkbenchRemoteDirectoryEntryDto]) {
    entries.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
        ("dir", "file") => std::cmp::Ordering::Less,
        ("file", "dir") => std::cmp::Ordering::Greater,
        _ => a
            .name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name)),
    });
}

/// Business Logic（为什么需要这个函数）:
///     远端目录选择器需要展示对端设备的常用入口，减少用户手动输入路径的成本。
///
/// Code Logic（这个函数做什么）:
///     返回当前平台存在的根目录和常用代码目录。
pub fn remote_roots() -> Vec<WorkbenchRemoteRootDto> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    if let Some(home) = dirs::home_dir() {
        push_root(&mut roots, &mut seen, "Home", home.clone());
        if let Some(desktop) = dirs::desktop_dir() {
            push_root(&mut roots, &mut seen, "Desktop", desktop);
        }
        if let Some(documents) = dirs::document_dir() {
            push_root(&mut roots, &mut seen, "Documents", documents);
        }
        if let Some(downloads) = dirs::download_dir() {
            push_root(&mut roots, &mut seen, "Downloads", downloads);
        }
        push_common_code_roots(&mut roots, &mut seen, &home);
    }
    push_platform_roots(&mut roots, &mut seen);

    roots
}

/// Business Logic（为什么需要这个函数）:
///     用户浏览远端设备目录时，需要看到当前目录下的一级文件夹和文件。
///
/// Code Logic（这个函数做什么）:
///     读取指定路径的一级子项并返回远端目录条目 DTO。
pub fn list_remote_directory(
    path: &Path,
) -> Result<Vec<WorkbenchRemoteDirectoryEntryDto>, AppError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(AppError::generic("路径必须是文件夹"));
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        entries.push(entry_from_path(&entry.path())?);
    }
    sort_entries(&mut entries);
    Ok(entries)
}

/// Business Logic（为什么需要这个函数）:
///     用户选中远端路径后，需要确认该路径是否可读、是否是 Git 仓库以及建议项目名称。
///
/// Code Logic（这个函数做什么）:
///     读取指定路径 metadata 并返回远端路径信息 DTO。
pub fn remote_path_info(path: &Path) -> Result<WorkbenchRemotePathInfoDto, AppError> {
    let metadata = fs::metadata(path)?;
    let is_dir = metadata.is_dir();
    let suggested_project_name = infer_project_name(path);
    let readable = if is_dir {
        fs::read_dir(path).is_ok()
    } else {
        fs::File::open(path).is_ok()
    };

    Ok(WorkbenchRemotePathInfoDto {
        name: suggested_project_name.clone(),
        path: path.display().to_string(),
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        readable,
        is_git_repo: is_git_repo(path, is_dir),
        suggested_project_name,
    })
}

/// 在浏览层父目录下新建一层文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     添加项目前用户要在本机或对端指定目录里先建空文件夹；不能走项目内 `files/create-dir`。
///
/// Code Logic（这个函数做什么）:
///     校验单段名称，canonicalize 父目录且必须是文件夹，目标已存在则拒绝，`create_dir` 一层后返回 path info。
pub fn create_browse_dir(
    parent: &Path,
    name: &str,
) -> Result<WorkbenchRemotePathInfoDto, AppError> {
    validate_child_name(name)?;
    let parent = parent
        .canonicalize()
        .map_err(|error| AppError::generic(format!("父路径不可访问: {error}")))?;
    if !parent.is_dir() {
        return Err(AppError::generic("父路径必须是文件夹"));
    }
    let target = parent.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(AppError::generic("目标路径已存在"));
    }
    fs::create_dir(&target)
        .map_err(|error| AppError::generic(format!("创建文件夹失败: {error}")))?;
    remote_path_info(&target)
}

/// 判断目录在打开为项目时是否应 `git init`。
///
/// Business Logic（为什么需要这个函数）:
///     空目录（可忽略系统垃圾文件）打开时要变成 Git 仓库；已有内容或已有 `.git` 不能覆盖。
///
/// Code Logic（这个函数做什么）:
///     `.git` 存在则 false；否则一级子项名称必须都属于垃圾集合。
pub fn dir_is_empty_for_git_init(path: &Path) -> bool {
    if path.join(".git").exists() {
        return false;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        if !GIT_INIT_IGNORE_NAMES.contains(&name) {
            return false;
        }
    }
    true
}

/// 空目录则在该路径执行 `git init`（不提交、不写 README）。
///
/// Business Logic（为什么需要这个函数）:
///     打开为项目时，看起来为空的目录应成为可立即 commit 的空仓库。
///
/// Code Logic（这个函数做什么）:
///     非空直接 Ok；否则 `git init` 于 canonical cwd，失败返回「无法初始化 Git 仓库」。
pub fn git_init_if_empty(path: &Path) -> Result<(), AppError> {
    if !dir_is_empty_for_git_init(path) {
        return Ok(());
    }
    let output = Command::new("git")
        .arg("init")
        .current_dir(path)
        .output()
        .map_err(|error| AppError::generic(format!("无法初始化 Git 仓库: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            return Err(AppError::generic("无法初始化 Git 仓库"));
        }
        return Err(AppError::generic(format!("无法初始化 Git 仓库: {stderr}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Business Logic（为什么需要这个测试）:
    ///     远端项目选择器需要识别目录是否已经是 Git 仓库，以便前端提示可直接打开为项目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在临时目录创建 `.git` 子目录，断言路径信息返回目录类型并标记为 Git 仓库。
    #[test]
    fn path_info_marks_git_repo() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let info = remote_path_info(temp.path()).unwrap();

        assert_eq!(info.kind, "dir");
        assert!(info.is_git_repo);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端目录浏览应优先展示文件夹，帮助用户逐层进入项目路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建一个目录和一个文件，断言列表排序为目录在前、文件在后。
    #[test]
    fn list_directory_sorts_dirs_before_files() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("README.md"), "# Readme").unwrap();

        let entries = list_remote_directory(temp.path()).unwrap();

        assert_eq!(entries[0].name, "src");
        assert_eq!(entries[0].kind, "dir");
        assert_eq!(entries[1].name, "README.md");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     浏览层只能建一层合法名称，不能覆盖已有路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     成功创建、拒绝分隔符/`.`/`..`/已存在目标，以及父路径不是目录。
    #[test]
    fn create_browse_dir_accepts_one_level_and_rejects_bad_names() {
        let temp = TempDir::new().unwrap();
        let created = create_browse_dir(temp.path(), "new-studio").unwrap();
        assert_eq!(created.kind, "dir");
        assert!(!created.is_git_repo);
        assert!(temp.path().join("new-studio").is_dir());

        assert!(create_browse_dir(temp.path(), "nested/dir").is_err());
        assert!(create_browse_dir(temp.path(), "..").is_err());
        assert!(create_browse_dir(temp.path(), ".").is_err());
        assert!(create_browse_dir(temp.path(), "new-studio").is_err());

        let file = temp.path().join("notes.txt");
        fs::write(&file, "x").unwrap();
        assert!(create_browse_dir(&file, "child").is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     打开项目时的空目录判定必须忽略系统垃圾文件，且不得把已有仓库或真实内容当成空。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖真空、仅垃圾文件、`.git`、普通文件和子目录。
    #[test]
    fn dir_is_empty_for_git_init_ignores_junk_only() {
        let temp = TempDir::new().unwrap();
        assert!(dir_is_empty_for_git_init(temp.path()));

        fs::write(temp.path().join(".DS_Store"), []).unwrap();
        assert!(dir_is_empty_for_git_init(temp.path()));

        fs::write(temp.path().join("README.md"), "hi").unwrap();
        assert!(!dir_is_empty_for_git_init(temp.path()));

        let gitty = TempDir::new().unwrap();
        fs::create_dir(gitty.path().join(".git")).unwrap();
        assert!(!dir_is_empty_for_git_init(gitty.path()));

        let nested = TempDir::new().unwrap();
        fs::create_dir(nested.path().join("src")).unwrap();
        assert!(!dir_is_empty_for_git_init(nested.path()));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     空目录打开时应 git init 且不提交；非空目录不得误建 `.git`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     真空目录 init 后存在 `.git` 且 `git status` 无提交；有文件的目录 init 是 no-op。
    #[test]
    fn git_init_if_empty_inits_vacant_dir_only() {
        let empty = TempDir::new().unwrap();
        git_init_if_empty(empty.path()).unwrap();
        assert!(empty.path().join(".git").exists());
        let log = Command::new("git")
            .args([
                "-C",
                empty.path().to_str().unwrap(),
                "rev-parse",
                "--verify",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert!(!log.status.success(), "empty init must not create a commit");

        let filled = TempDir::new().unwrap();
        fs::write(filled.path().join("a.txt"), "a").unwrap();
        git_init_if_empty(filled.path()).unwrap();
        assert!(!filled.path().join(".git").exists());
    }
}
