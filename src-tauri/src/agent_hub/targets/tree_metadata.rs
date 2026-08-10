//! targets/tree_metadata — 扫描缓存使用的目录元数据指纹。
//!
//! Business Logic（为什么需要这个模块）:
//!     Agent Hub 切换资产页签时会重复观察同一目录；内容未变时不应再次读取全部文件。
//!
//! Code Logic（这个模块做什么）:
//!     不跟随 symlink，稳定记录相对路径、类型、长度、时间与平台 inode/mode，生成元数据 hash。

use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// 为目录生成低成本递归元数据指纹；只用于只读 inventory 缓存键。
pub(crate) fn tree_metadata_fingerprint(root: &Path) -> Result<String, AppError> {
    if !root.is_dir() {
        return Err(AppError::not_found("PORTABLE_INVENTORY_TREE_MISSING"));
    }
    let mut facts = Vec::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|e| AppError::generic(format!("portable tree metadata: {e}")))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path)?;
        let relative = path
            .strip_prefix(root)
            .map(|value| value.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"));
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            "symlink"
        } else if file_type.is_dir() {
            "directory"
        } else if file_type.is_file() {
            "file"
        } else {
            "other"
        };
        let modified_ns = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let created_ns = metadata
            .created()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let symlink_target = if file_type.is_symlink() {
            fs::read_link(path)
                .ok()
                .map(|value| value.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        } else {
            String::new()
        };
        #[cfg(unix)]
        let platform = {
            use std::os::unix::fs::MetadataExt;
            format!(
                "{}:{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.mode(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        };
        #[cfg(not(unix))]
        let platform = String::new();
        facts.push(format!(
            "{relative}\0{kind}\0{}\0{modified_ns}\0{created_ns}\0{platform}\0{symlink_target}",
            metadata.len()
        ));
    }
    facts.sort();
    Ok(sha256_hex(facts.join("\n").as_bytes()))
}
