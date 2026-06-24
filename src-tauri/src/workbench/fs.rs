//! workbench/fs.rs — 工作台本机文件系统操作
//!
//! Business Logic（为什么需要这个模块）:
//!     工作台右侧文件树需要列出项目目录、查看路径信息，并提供基础创建、重命名、删除操作。
//!
//! Code Logic（这个模块做什么）:
//!     所有操作先解析到项目根目录内，再用标准库文件系统 API 完成具体读写。

#![allow(dead_code)]

use crate::error::AppError;
use crate::workbench::models::{WorkbenchFileNode, WorkbenchPathInfo};
use crate::workbench::projects::resolve_project_path;
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Business Logic（为什么需要这个函数）:
///     工作台文件操作只允许用户输入单个子文件名，避免通过名称字段表达路径跳转。
///
/// Code Logic（这个函数做什么）:
///     拒绝空名、路径分隔符、`.` 和 `..`。
fn validate_child_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err(AppError::generic("名称不能包含路径分隔符"));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     前端文件树显示相对项目根的路径，不能暴露本机绝对路径。
///
/// Code Logic（这个函数做什么）:
///     将绝对路径剥离 canonical root 前缀，并用 `/` 作为跨平台分隔符。
fn relative_path(root: &Path, path: &Path) -> Result<String, AppError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| AppError::generic("不能访问项目目录之外的路径"))?;
    Ok(rel
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Business Logic（为什么需要这个函数）:
///     多个文件系统操作都需要同一份 canonical 项目根，避免软链接或相对路径造成边界判断不一致。
///
/// Code Logic（这个函数做什么）:
///     canonicalize root 并要求结果是目录。
fn canonical_root(root: &Path) -> Result<PathBuf, AppError> {
    let canonical = root
        .canonicalize()
        .map_err(|error| AppError::generic(format!("项目路径不可访问: {error}")))?;
    if !canonical.is_dir() {
        return Err(AppError::generic("项目路径必须是文件夹"));
    }
    Ok(canonical)
}

/// Business Logic（为什么需要这个函数）:
///     文件树与详情面板需要可读的最后修改时间，用于展示和刷新判断。
///
/// Code Logic（这个函数做什么）:
///     从 metadata.modified() 读取系统时间，转换为 UTC RFC3339 字符串；平台不支持时返回 None。
fn modified_at(metadata: &fs::Metadata) -> Option<String> {
    metadata.modified().ok().map(|time| {
        let datetime: DateTime<Utc> = time.into();
        datetime.to_rfc3339()
    })
}

/// Business Logic（为什么需要这个函数）:
///     list_dir 与 path_info 都需要把本机 metadata 转成前端统一文件节点字段。
///
/// Code Logic（这个函数做什么）:
///     读取文件名、相对路径、类型、文件大小和修改时间，目录 size 返回 None。
fn node_from_path(root: &Path, path: &Path) -> Result<WorkbenchFileNode, AppError> {
    let metadata = fs::metadata(path)?;
    let is_dir = metadata.is_dir();
    Ok(WorkbenchFileNode {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        path: relative_path(root, path)?,
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        size: if is_dir { None } else { Some(metadata.len()) },
        modified_at: modified_at(&metadata),
        children: None,
    })
}

/// Business Logic（为什么需要这个函数）:
///     文件树列目录时遇到 symlink 不能直接跟随到项目外目标，否则会泄露外部文件 metadata。
///
/// Code Logic（这个函数做什么）:
///     先用 symlink_metadata 判断链接；链接目标 canonicalize 后必须仍在 root 内，否则返回 None 跳过。
fn safe_node_from_entry(root: &Path, path: &Path) -> Result<Option<WorkbenchFileNode>, AppError> {
    let link_metadata = fs::symlink_metadata(path)?;
    if link_metadata.file_type().is_symlink() {
        let canonical = path
            .canonicalize()
            .map_err(|error| AppError::generic(format!("路径不可访问: {error}")))?;
        if !canonical.starts_with(root) {
            return Ok(None);
        }
    }

    node_from_path(root, path).map(Some)
}

/// Business Logic（为什么需要这个函数）:
///     项目内 symlink 若指向项目外，后续读取 metadata 会泄露外部目标信息，必须提前阻断。
///
/// Code Logic（这个函数做什么）:
///     对 symlink leaf canonicalize 目标并校验仍在 canonical root 内；普通路径直接通过。
fn reject_external_symlink(root: &Path, path: &Path) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        let canonical = path
            .canonicalize()
            .map_err(|error| AppError::generic(format!("路径不可访问: {error}")))?;
        if !canonical.starts_with(root) {
            return Err(AppError::generic("不能操作指向项目目录之外的符号链接"));
        }
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     创建、重命名和查询后，前端需要统一 PathInfo 来刷新选中项。
///
/// Code Logic（这个函数做什么）:
///     将已解析的绝对路径 metadata 转换为 `WorkbenchPathInfo`。
fn info_from_path(root: &Path, path: &Path) -> Result<WorkbenchPathInfo, AppError> {
    let metadata = fs::metadata(path)?;
    let is_dir = metadata.is_dir();
    Ok(WorkbenchPathInfo {
        name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string(),
        path: relative_path(root, path)?,
        kind: if is_dir { "dir" } else { "file" }.to_string(),
        size: if is_dir { None } else { Some(metadata.len()) },
        modified_at: modified_at(&metadata),
    })
}

/// Business Logic（为什么需要这个函数）:
///     新建文件/文件夹前需要从父目录和单个名称得出最终路径，并保证仍在项目内。
///
/// Code Logic（这个函数做什么）:
///     先解析父目录，再 join 已验证名称，最后检查目标路径前缀不逃出 canonical root。
fn resolve_new_child(
    root: &Path,
    parent: &str,
    name: &str,
) -> Result<(PathBuf, PathBuf), AppError> {
    validate_child_name(name)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| AppError::generic(format!("项目路径不可访问: {error}")))?;
    let parent_path = resolve_project_path(&canonical_root, parent)?;
    if !parent_path.is_dir() {
        return Err(AppError::generic("父路径必须是文件夹"));
    }
    let target = parent_path.join(name);
    if !target.starts_with(&canonical_root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    Ok((canonical_root, target))
}

/// Business Logic（为什么需要这个函数）:
///     删除和重命名要操作项目内 leaf 本身，尤其 symlink leaf 不能先 canonicalize 到目标文件。
///
/// Code Logic（这个函数做什么）:
///     校验相对路径不是绝对路径，canonicalize 父目录并限制在 root 内，再把已验证 leaf 名拼回父目录。
fn resolve_existing_leaf(root: &Path, relative: &str) -> Result<(PathBuf, PathBuf), AppError> {
    if relative.trim().is_empty() {
        return Err(AppError::generic("路径不能为空"));
    }

    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::Prefix(_) | Component::RootDir))
    {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }

    let leaf = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AppError::generic("路径缺少文件名"))?;
    validate_child_name(leaf)?;

    let parent_relative = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let canonical_root = canonical_root(root)?;
    let parent = if parent_relative.as_os_str().is_empty() {
        canonical_root.clone()
    } else {
        resolve_project_path(&canonical_root, &parent_relative.to_string_lossy())?
    };
    if !parent.is_dir() {
        return Err(AppError::generic("父路径必须是文件夹"));
    }

    let leaf_path = parent.join(leaf);
    if !leaf_path.starts_with(&canonical_root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    Ok((canonical_root, leaf_path))
}

/// Business Logic（为什么需要这个函数）:
///     文件树排序需要在不同平台和文件系统上保持稳定，避免前端列表顺序抖动。
///
/// Code Logic（这个函数做什么）:
///     先目录后文件；同类型先按小写名称升序，小写相等时按原始名称升序。
fn sort_file_nodes(entries: &mut [WorkbenchFileNode]) {
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
///     工作台右侧文件树需要列出当前目录下的文件夹和文件。
///
/// Code Logic（这个函数做什么）:
///     解析目录到项目根内，读取一级子项并按目录优先、同类小写名称升序排序。
pub fn list_dir(root: &Path, relative: &str) -> Result<Vec<WorkbenchFileNode>, AppError> {
    let canonical_root = canonical_root(root)?;
    let dir = resolve_project_path(&canonical_root, relative)?;
    if !dir.is_dir() {
        return Err(AppError::generic("路径必须是文件夹"));
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(node) = safe_node_from_entry(&canonical_root, &entry.path())? {
            entries.push(node);
        }
    }
    sort_file_nodes(&mut entries);
    Ok(entries)
}

/// Business Logic（为什么需要这个函数）:
///     前端需要获取选中路径的类型、大小和更新时间，以便展示文件详情。
///
/// Code Logic（这个函数做什么）:
///     解析路径到项目根内，再把 metadata 转换为 PathInfo。
pub fn path_info(root: &Path, relative: &str) -> Result<WorkbenchPathInfo, AppError> {
    let canonical_root = canonical_root(root)?;
    let path = resolve_project_path(&canonical_root, relative)?;
    info_from_path(&canonical_root, &path)
}

/// Business Logic（为什么需要这个函数）:
///     用户可在工作台项目树中创建新文件，用于快速补充项目资料或代码文件。
///
/// Code Logic（这个函数做什么）:
///     验证名称并解析父目录，在项目根内 create_new 空文件后返回 PathInfo。
pub fn create_file(root: &Path, parent: &str, name: &str) -> Result<WorkbenchPathInfo, AppError> {
    let (canonical_root, target) = resolve_new_child(root, parent, name)?;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)?;
    info_from_path(&canonical_root, &target)
}

/// Business Logic（为什么需要这个函数）:
///     用户可在工作台项目树中创建文件夹来组织本机项目文件。
///
/// Code Logic（这个函数做什么）:
///     验证名称并解析父目录，在项目根内创建目录后返回 PathInfo。
pub fn create_dir(root: &Path, parent: &str, name: &str) -> Result<WorkbenchPathInfo, AppError> {
    let (canonical_root, target) = resolve_new_child(root, parent, name)?;
    fs::create_dir(&target)?;
    info_from_path(&canonical_root, &target)
}

/// Business Logic（为什么需要这个函数）:
///     用户可重命名项目树中的文件或文件夹，但不能借此把路径移出项目根。
///
/// Code Logic（这个函数做什么）:
///     解析源路径、验证新名称，把目标限制在同级目录且位于 canonical root 内。
pub fn rename_path(
    root: &Path,
    relative: &str,
    new_name: &str,
) -> Result<WorkbenchPathInfo, AppError> {
    validate_child_name(new_name)?;
    if relative.trim().is_empty() {
        return Err(AppError::generic("不能重命名项目根目录"));
    }

    let (canonical_root, source) = resolve_existing_leaf(root, relative)?;
    reject_external_symlink(&canonical_root, &source)?;
    let parent = source
        .parent()
        .ok_or_else(|| AppError::generic("路径缺少父目录"))?;
    let target = parent.join(new_name);
    if !target.starts_with(&canonical_root) {
        return Err(AppError::generic("不能访问项目目录之外的路径"));
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err(AppError::generic("目标路径已存在"));
    }
    fs::rename(&source, &target)?;
    info_from_path(&canonical_root, &target)
}

/// Business Logic（为什么需要这个函数）:
///     用户可从工作台文件树删除项目内文件或文件夹。
///
/// Code Logic（这个函数做什么）:
///     解析目标路径到项目根内，文件用 remove_file，目录用 remove_dir_all，拒绝删除项目根。
pub fn delete_path(root: &Path, relative: &str) -> Result<(), AppError> {
    if relative.trim().is_empty() {
        return Err(AppError::generic("不能删除项目根目录"));
    }

    let (_canonical_root, path) = resolve_existing_leaf(root, relative)?;
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        return Err(AppError::generic("不支持删除该路径类型"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{create_file, delete_path, list_dir, path_info, rename_path, sort_file_nodes};
    use crate::workbench::models::WorkbenchFileNode;
    use std::fs;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;

    /// Business Logic（为什么需要这个函数）:
    ///     文件系统测试需要独立项目根，避免测试之间共享状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在系统临时目录创建 UUID 子目录并返回路径。
    fn temp_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ccp-workbench-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    /// Business Logic（为什么需要这个测试）:
    ///     文件树要优先展示文件夹，再展示文件，便于用户浏览项目结构。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建大小写混合的文件夹与文件，断言排序为目录优先且同类按小写名称升序。
    #[test]
    fn list_dir_sorts_dirs_before_files() {
        let root = temp_root();
        fs::create_dir(root.join("zeta")).expect("create zeta dir");
        fs::create_dir(root.join("Alpha")).expect("create Alpha dir");
        fs::write(root.join("beta.txt"), "beta").expect("write beta");
        fs::write(root.join("Aardvark.txt"), "aardvark").expect("write aardvark");

        let nodes = list_dir(&root, "").expect("list dir");

        let names: Vec<String> = nodes.into_iter().map(|node| node.name).collect();
        assert_eq!(names, vec!["Alpha", "zeta", "Aardvark.txt", "beta.txt"]);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     文件树不能通过项目内 symlink 展示项目外文件，避免泄露外部 metadata。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建指向项目外文件的 symlink，断言 list_dir 跳过该节点。
    #[test]
    fn list_dir_skips_symlink_pointing_outside_root() {
        let root = temp_root();
        let outside = root
            .parent()
            .expect("temp root parent")
            .join(format!("ccp-outside-{}.txt", uuid::Uuid::new_v4()));
        fs::write(&outside, "outside").expect("write outside");
        symlink(&outside, root.join("outside-link.txt")).expect("create symlink");

        let nodes = list_dir(&root, "").expect("list dir");

        assert!(nodes.iter().all(|node| node.name != "outside-link.txt"));
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     排序在大小写折叠后相等时仍要稳定确定，避免文件树顺序随平台或文件系统变化。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接排序大小写折叠后相等的两个节点，断言用原始名称作为 tie-break。
    #[test]
    fn list_dir_uses_original_name_tiebreak() {
        let mut nodes = vec![
            WorkbenchFileNode {
                name: "alpha.txt".to_string(),
                path: "alpha.txt".to_string(),
                kind: "file".to_string(),
                size: Some(5),
                modified_at: None,
                children: None,
            },
            WorkbenchFileNode {
                name: "Alpha.txt".to_string(),
                path: "Alpha.txt".to_string(),
                kind: "file".to_string(),
                size: Some(5),
                modified_at: None,
                children: None,
            },
        ];

        sort_file_nodes(&mut nodes);
        let names: Vec<String> = nodes.into_iter().map(|node| node.name).collect();
        assert_eq!(names, vec!["Alpha.txt", "alpha.txt"]);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     创建文件只允许单层文件名，防止用户通过名称参数绕过父目录边界。
    ///
    /// Code Logic（这个测试做什么）:
    ///     使用包含路径分隔符的名称创建文件，并断言操作被拒绝。
    #[test]
    fn create_file_rejects_path_separator_in_name() {
        let root = temp_root();

        let result = create_file(&root, "", "nested/file.txt");

        assert!(result.is_err());
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     重命名不能把项目内路径移动到项目根目录之外。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对项目内文件执行 `..` 目标名重命名，并断言操作被拒绝且原文件仍存在。
    #[test]
    fn rename_path_keeps_target_inside_root() {
        let root = temp_root();
        fs::write(root.join("inside.txt"), "inside").expect("write inside");

        let result = rename_path(&root, "inside.txt", "..");

        assert!(result.is_err());
        assert!(root.join("inside.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     重命名不能覆盖已有文件或目录，否则用户可能误丢数据。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建源文件和同级目标文件，断言重命名被拒绝且两者内容保留。
    #[test]
    fn rename_path_rejects_existing_target() {
        let root = temp_root();
        fs::write(root.join("source.txt"), "source").expect("write source");
        fs::write(root.join("target.txt"), "target").expect("write target");

        let result = rename_path(&root, "source.txt", "target.txt");

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(root.join("source.txt")).expect("read source"),
            "source"
        );
        assert_eq!(
            fs::read_to_string(root.join("target.txt")).expect("read target"),
            "target"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     项目内 symlink 若指向项目外，重命名后不能返回外部目标 metadata。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建指向项目外文件的 symlink，断言 rename_path 直接拒绝且不移动链接。
    #[test]
    fn rename_path_rejects_symlink_pointing_outside_root() {
        let root = temp_root();
        let outside = root
            .parent()
            .expect("temp root parent")
            .join(format!("ccp-outside-{}.txt", uuid::Uuid::new_v4()));
        fs::write(&outside, "external metadata").expect("write outside");
        symlink(&outside, root.join("outside-link.txt")).expect("create symlink");

        let result = rename_path(&root, "outside-link.txt", "renamed-link.txt");

        let original_link = root.join("outside-link.txt");
        let renamed_link = root.join("renamed-link.txt");
        let original_exists = original_link.exists();
        let renamed_exists = renamed_link.exists();
        let _ = fs::remove_file(original_link);
        let _ = fs::remove_file(renamed_link);
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(root);
        assert!(original_exists);
        assert!(!renamed_exists);
        assert!(result.is_err());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     删除项目内 symlink 时只应删除链接本身，不能删除其指向的真实文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建项目内文件和指向它的 symlink，删除 symlink 后断言目标文件仍存在。
    #[test]
    fn delete_path_removes_symlink_not_target() {
        let root = temp_root();
        let target = root.join("target.txt");
        let link = root.join("target-link.txt");
        fs::write(&target, "target").expect("write target");
        symlink(&target, &link).expect("create symlink");

        delete_path(&root, "target-link.txt").expect("delete symlink");

        assert!(!link.exists());
        assert_eq!(
            fs::read_to_string(target).expect("read target after delete"),
            "target"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     前端选中文件后需要展示文件类型和大小，保证文件树详情准确。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入固定长度文件，读取 path_info 后断言 kind 与 size。
    #[test]
    fn path_info_reports_file_size_and_kind() {
        let root = temp_root();
        fs::write(root.join("note.txt"), "hello").expect("write note");

        let info = path_info(&root, "note.txt").expect("path info");

        assert_eq!(info.kind, "file");
        assert_eq!(info.size, Some(5));
        let _ = fs::remove_dir_all(root);
    }
}
