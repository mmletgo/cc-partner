//! portable_store/symlink — 受控软链（Unix symlink / Windows junction）
//!
//! Business Logic（为什么需要这个模块）:
//!     各 Agent native 根只应挂 store 真树；逃逸链不得跟随。
//!     Windows 无权限时必须失败并提示 Developer Mode，禁止静默 copy 成第二份。
//!
//! Code Logic（这个模块做什么）:
//!     分类 Regular / StoreLink / EscapeLink；创建与拆除软链；从不 copy。

use super::{current_portable_store_root, store_id_from_canonical, PortableStoreKind};
use crate::error::AppError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// 路径相对 portable-store 的分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreLinkClass {
    /// 普通文件/目录（非 symlink）
    Regular,
    /// 目标 canonicalize 后落在 portable-store 内
    StoreLink {
        /// `skill:foo` 等
        store_id: String,
        /// 跟随后的绝对路径
        canonical: PathBuf,
        /// store 类别
        kind: PortableStoreKind,
    },
    /// symlink 但目标在 store 外，或无法解析
    EscapeLink,
}

/// 判断 canonical 是否位于 store 根之下。
///
/// Business Logic: 白名单跟随的唯一条件。
/// Code Logic: 两边 canonicalize 后 `strip_prefix`。
pub fn is_under_portable_store(canonical: &Path, store_root: &Path) -> bool {
    let Ok(store) = fs::canonicalize(store_root) else {
        return false;
    };
    canonical.starts_with(&store)
}

/// 分类路径：仅 store 内 symlink 算 StoreLink。
///
/// Business Logic: 扫描必须 fail-closed；逃逸链不得当普通 Skill 哈希。
/// Code Logic: 非 symlink → Regular；canonicalize 失败或越界 → EscapeLink。
pub fn classify_store_link(path: &Path) -> StoreLinkClass {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return StoreLinkClass::Regular,
    };
    if !meta.file_type().is_symlink() {
        return StoreLinkClass::Regular;
    }
    let Some(store_root) = current_portable_store_root() else {
        return StoreLinkClass::EscapeLink;
    };
    if !store_root.exists() {
        return StoreLinkClass::EscapeLink;
    }
    let Ok(canonical) = fs::canonicalize(path) else {
        return StoreLinkClass::EscapeLink;
    };
    if !is_under_portable_store(&canonical, &store_root) {
        return StoreLinkClass::EscapeLink;
    }
    let Some(store_id) = store_id_from_canonical(&canonical, &store_root) else {
        return StoreLinkClass::EscapeLink;
    };
    let kind = if store_id.starts_with("skill:") {
        PortableStoreKind::Skill
    } else if store_id.starts_with("command:") {
        PortableStoreKind::Command
    } else {
        PortableStoreKind::Mcp
    };
    StoreLinkClass::StoreLink {
        store_id,
        canonical,
        kind,
    }
}

/// 在 `link_path` 创建指向 `store_target` 的软链。
///
/// Business Logic: 附加到某 Agent = 只建链，不复制真树。
/// Code Logic: Unix `symlink`；Windows 目录 junction / 文件 symlink；已存在且已是该目标则跳过。
pub fn create_store_link(store_target: &Path, link_path: &Path) -> Result<(), AppError> {
    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = fs::symlink_metadata(link_path) {
        if meta.file_type().is_symlink() {
            if let Ok(existing) = fs::canonicalize(link_path) {
                if let Ok(want) = fs::canonicalize(store_target) {
                    if existing == want {
                        return Ok(());
                    }
                }
            }
            unlink_store_link(link_path)?;
        } else {
            return Err(AppError::validation(
                "PORTABLE_STORE_LINK_CONFLICT_REAL_PATH".to_string(),
            ));
        }
    }
    create_os_link(store_target, link_path)
}

/// 附加：确保 native 路径是指向 store 真树的软链。
pub fn attach_store_link(store_target: &Path, link_path: &Path) -> Result<(), AppError> {
    create_store_link(store_target, link_path)
}

/// 拆除软链；目标必须是 symlink。
///
/// Business Logic: 卸下只拆链，禁止 `remove_dir_all` 跟随进 store。
/// Code Logic: Unix `remove_file`；Windows 目录链用 `remove_dir`。
pub fn unlink_store_link(link_path: &Path) -> Result<(), AppError> {
    let meta = fs::symlink_metadata(link_path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            AppError::not_found("PORTABLE_STORE_LINK_MISSING")
        } else {
            AppError::from(e)
        }
    })?;
    if !meta.file_type().is_symlink() {
        return Err(AppError::validation(
            "PORTABLE_STORE_REFUSE_UNLINK_REAL_TREE".to_string(),
        ));
    }
    remove_os_link(link_path, &meta)
}

/// 若路径是 store 软链则拆除；普通路径原样返回 false。
///
/// Business Logic: Disable/Detach 必须先确认是 store 链，才能避免 MOVE 真树。
/// Code Logic: StoreLink → unlink true；其余 false。
pub fn unlink_if_store_link(link_path: &Path) -> Result<bool, AppError> {
    match classify_store_link(link_path) {
        StoreLinkClass::StoreLink { .. } => {
            unlink_store_link(link_path)?;
            Ok(true)
        }
        StoreLinkClass::EscapeLink => Err(AppError::validation(
            "PORTABLE_STORE_REFUSE_UNLINK_ESCAPE".to_string(),
        )),
        StoreLinkClass::Regular => Ok(false),
    }
}

fn create_os_link(store_target: &Path, link_path: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(store_target, link_path).map_err(map_symlink_error)
    }
    #[cfg(windows)]
    {
        let is_dir = store_target.is_dir();
        let result = if is_dir {
            std::os::windows::fs::symlink_dir(store_target, link_path)
        } else {
            std::os::windows::fs::symlink_file(store_target, link_path)
        };
        result.map_err(map_symlink_error)
    }
}

fn remove_os_link(link_path: &Path, meta: &fs::Metadata) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        let _ = meta;
        fs::remove_file(link_path)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        // 目录 junction/symlink 必须 remove_dir；文件 symlink 用 remove_file。
        // 绝不用 remove_dir_all，避免跟随删除 store 真树。
        if meta.file_type().is_symlink() && store_target_is_dir_link(link_path, meta) {
            fs::remove_dir(link_path)?;
        } else {
            fs::remove_file(link_path)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn store_target_is_dir_link(_link_path: &Path, meta: &fs::Metadata) -> bool {
    // Windows symlink_metadata：目录 junction 的 file_type 仍是 symlink；
    // 用 metadata() 跟随判断会把逃逸链当目录。这里仅在已确认 StoreLink 后调用。
    meta.is_dir() || !meta.is_file()
}

/// 把 native 真树/文件 move 进 store，再在原处放回软链。
///
/// Business Logic: 一键迁移必须留下一份真树；原路径变成链，禁止变成第二份副本长期共存。
/// Code Logic: 优先 `rename`；跨卷失败则 copy 到 store 后删除 native，再 create_store_link。
pub fn migrate_native_into_store(native: &Path, store_dest: &Path) -> Result<(), AppError> {
    if matches!(
        classify_store_link(native),
        StoreLinkClass::StoreLink { .. }
    ) {
        return Ok(());
    }
    if store_dest.exists() {
        return Err(AppError::validation(
            "PORTABLE_STORE_MIGRATE_DEST_EXISTS".to_string(),
        ));
    }
    if let Some(parent) = store_dest.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(native, store_dest) {
        Ok(()) => {}
        Err(err) if is_exdev(&err) => {
            copy_then_remove(native, store_dest)?;
        }
        Err(err) => return Err(AppError::from(err)),
    }
    create_store_link(store_dest, native)
}

fn is_exdev(err: &io::Error) -> bool {
    // Unix EXDEV=18；Windows ERROR_NOT_SAME_DEVICE=17。
    matches!(err.raw_os_error(), Some(17) | Some(18))
}

fn copy_then_remove(src: &Path, dest: &Path) -> Result<(), AppError> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        copy_dir_all(src, dest)?;
        fs::remove_dir_all(src)?;
    } else {
        fs::copy(src, dest)?;
        fs::remove_file(src)?;
    }
    Ok(())
}

fn copy_dir_all(src: &Path, dest: &Path) -> Result<(), AppError> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn map_symlink_error(err: io::Error) -> AppError {
    #[cfg(windows)]
    {
        let _ = err;
        return AppError::validation(
            "PORTABLE_STORE_SYMLINK_DENIED: Windows 需要 Developer Mode 或管理员权限才能创建 junction/symlink，禁止静默复制成第二份安装"
                .to_string(),
        );
    }
    #[cfg(not(windows))]
    {
        AppError::from(err)
    }
}
