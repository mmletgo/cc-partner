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

#[cfg(test)]
mod tests {
    use super::resolve_project_path;
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
}
