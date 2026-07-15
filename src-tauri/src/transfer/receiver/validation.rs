//! receiver/validation — 接收路径与 basename 校验
//!
//! Business Logic: 远端 filename/transfer_id 进入路径拼接前必须限制为单组件；
//!     最终/临时路径落盘前必须断言仍在 receive_dir 内。
//! Code Logic: 纯校验与路径规范化，无 IO 副作用（除路径组件解析）。

use super::InitMeta;
use crate::error::AppError;
use crate::models::transfer::TransferTask;
use std::path::{Component, Path, PathBuf};

/// Business Logic（为什么需要这个函数）:
///     同一 transfer_id 重放 init 时，只有元数据完全一致才允许幂等返回，否则必须 conflict。
///
/// Code Logic（这个函数做什么）:
///     比较已规范化的 filename/size/sha256/chunk_size 是否与活跃 Receive 任务一致。
pub(super) fn init_metadata_matches(
    task: &TransferTask,
    meta: &InitMeta,
    safe_filename: &str,
) -> bool {
    task.filename == safe_filename
        && task.size == meta.size
        && task.sha256 == meta.sha256
        && task.chunk_size == meta.chunk_size
}

/// Business Logic（为什么需要这个函数）:
///     远端 filename/transfer_id 进入路径拼接前必须限制为单个普通组件，否则绝对路径或 `..`
///     可逃逸 receive_dir，把校验通过的内容写到任意可写路径。
///
/// Code Logic（这个函数做什么）:
///     trim 后拒绝空串；Path components 必须恰好一个 Normal，且不含 `/` `\` 或 `.`/`..`。
pub(super) fn sanitize_receive_basename(raw: &str, field: &str) -> Result<String, AppError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AppError::validation(format!("{field} 不能为空")));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(AppError::validation(format!(
            "{field} 只能是单个文件名组件，禁止路径分隔符"
        )));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(AppError::validation(format!("{field} 不能是绝对路径")));
    }
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) => {
            let s = part.to_string_lossy();
            if s.is_empty() || s == "." || s == ".." {
                return Err(AppError::validation(format!(
                    "{field} 非法：禁止 `.`/`..` 或空组件"
                )));
            }
            // 防御：Windows 下某些前缀/盘符可能被解析为 Prefix 而非 Normal。
            Ok(s.into_owned())
        }
        _ => Err(AppError::validation(format!(
            "{field} 只能是单个普通文件名组件，禁止绝对路径、父目录或前缀"
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     临时文件 `.{transfer_id}.tmp` 也必须落在 receive_dir 内，避免 transfer_id 逃逸。
///
/// Code Logic（这个函数做什么）:
///     用已校验的 transfer_id 拼临时名，再验证最终路径仍位于 receive_dir 之下。
pub(super) fn receive_tmp_path(receive_dir: &Path, transfer_id: &str) -> Result<PathBuf, AppError> {
    let tmp_name = format!(".{transfer_id}.tmp");
    // transfer_id 已是单组件，前缀 `.` + 后缀 `.tmp` 仍应是单组件。
    let _ = sanitize_receive_basename(&tmp_name, "transfer_id_tmp")?;
    let tmp_path = receive_dir.join(&tmp_name);
    ensure_path_within_dir(receive_dir, &tmp_path)?;
    Ok(tmp_path)
}

/// Business Logic（为什么需要这个函数）:
///     join 后的最终/临时路径必须仍在 receive_dir 内，防止绝对路径替换或 `..` 逃逸。
///
/// Code Logic（这个函数做什么）:
///     对父目录做 canonicalize（不存在时回退规范化），断言目标仍以 receive_dir 为前缀。
pub(super) fn ensure_path_within_dir(dir: &Path, candidate: &Path) -> Result<(), AppError> {
    let canonical_dir = match dir.canonicalize() {
        Ok(p) => p,
        Err(_) => normalize_path(dir),
    };
    let parent = candidate.parent().unwrap_or(dir);
    let canonical_parent = match parent.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // 父目录可能尚未创建：必须基于 *canonical_dir* 拼相对后缀。
            // macOS 上 `/var` 与 canonicalize 后的 `/private/var` 不一致，
            // 若对不存在父路径只做 normalize_path 会误判逃逸。
            if parent == dir {
                canonical_dir.clone()
            } else if let Ok(rel) = parent.strip_prefix(dir) {
                canonical_dir.join(rel)
            } else if let Ok(rel) = parent.strip_prefix(&canonical_dir) {
                canonical_dir.join(rel)
            } else {
                // 回退：相对路径拼到 canonical_dir；绝对且不在 dir 下则交给后续 starts_with 拒绝。
                let normalized = normalize_path(parent);
                if normalized.is_absolute() {
                    normalized
                } else {
                    canonical_dir.join(normalized)
                }
            }
        }
    };
    if !canonical_parent.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    let file_name = candidate
        .file_name()
        .ok_or_else(|| AppError::validation("目标路径缺少文件名".to_string()))?;
    let final_path = canonical_parent.join(file_name);
    if !final_path.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    Ok(())
}

/// Code Logic: 去掉 `.`、解析 `..` 的逻辑路径规范化（不访问磁盘）。
pub(super) fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}
