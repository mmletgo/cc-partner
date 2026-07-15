//! receiver/chunk_io — chunk/tmp 打开、写入与哈希 IO
//!
//! Business Logic: 接收 tmp 与恢复校验必须 no-follow，禁止 symlink 写出 receive_dir。
//! Code Logic: 平台原生 O_NOFOLLOW / OPEN_REPARSE_POINT 打开 + 流式 SHA256。

use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::path::Path;
use tokio::io::AsyncReadExt;

/// Business Logic（为什么需要这个函数）:
///     certify 在 blocking 上下文需要同步 no-follow 打开，避免 async runtime 跨 await 丢句柄。
///
/// Code Logic（这个函数做什么）:
///     与 `open_regular_file_nofollow` 相同平台语义，返回 `std::fs::File`。
pub(super) fn open_regular_file_nofollow_std(path: &Path, writable: bool) -> Result<std::fs::File, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(libc::O_NOFOLLOW);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path).map_err(|e| {
            if e.kind() == ErrorKind::Other
                || e.raw_os_error() == Some(libc::ELOOP)
                || e.raw_os_error() == Some(libc::EPERM)
            {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("拒绝跟随符号链接打开: {}: {e}", path.display()),
                )
            } else {
                e
            }
        })?;
        let meta = std_file.metadata()?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(std_file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path)?;
        let meta = std_file.metadata()?;
        let ft = meta.file_type();
        if ft.is_symlink()
            || !ft.is_file()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(std_file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, writable);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法 no-follow 打开文件",
        )))
    }
}

/// 异步流式计算文件 SHA256（8KB 块，对照 Python）。
///
/// Business Logic: 测试/非路径安全场景可用跟随 open；生产 finalize 请用 nofollow。
/// Code Logic: 跟随 open 后 8KB 分块读入 Sha256。
#[allow(dead_code)]
pub(super) async fn compute_sha256(path: &Path) -> Result<String, AppError> {
    let mut file = tokio::fs::File::open(path).await?;
    hash_reader(&mut file).await
}

/// Business Logic（为什么需要这个函数）:
///     intent 恢复与 finalize 校验 .tmp 时必须证明路径 **本身**是匹配内容的普通文件。
///     普通 `File::open` / `metadata` 会跟随 symlink，链接到同尺寸同哈希目标时会误晋升或
///     把 chunk 写穿到 receive_dir 外。
///
/// Code Logic（这个函数做什么）:
///     no-follow 只读打开后流式 SHA256。
pub(super) async fn compute_sha256_nofollow(path: &Path) -> Result<String, AppError> {
    let mut file = open_regular_file_nofollow(path, false).await?;
    hash_reader(&mut file).await
}

/// Business Logic（为什么需要这个函数）:
///     init resume / complete 进度必须以 .tmp **自身**长度为准；跟随 symlink 会把
///     transferred_bytes 指到外部目标长度，掩盖写穿攻击。
///
/// Code Logic（这个函数做什么）:
///     `symlink_metadata`：不存在 → None；symlink/非普通文件 → Validation 并 best-effort 删除 symlink；
///     普通文件 → Some(len)。
pub(super) async fn receive_tmp_len_nofollow(path: &Path) -> Result<Option<u64>, AppError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                let _ = tokio::fs::remove_file(path).await;
                return Err(AppError::validation(format!(
                    "临时文件是符号链接，已删除危险路径: {}",
                    path.display()
                )));
            }
            if !ft.is_file() {
                return Err(AppError::validation(format!(
                    "临时路径不是普通文件: {}",
                    path.display()
                )));
            }
            Ok(Some(meta.len()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     chunk 写入路径若用普通 OpenOptions，会跟随预置/竞态替换的 `.{id}.tmp` symlink，
///     以本机权限 seek/write 到 receive_dir 外任意可写文件。
///
/// Code Logic（这个函数做什么）:
///     1) `create_new` 首次创建普通文件（路径已是 symlink 时失败，不会跟随）；
///     2) 已存在则 no-follow 读写打开，并校验句柄对应普通文件。
pub(super) async fn open_receive_tmp_rw(path: &Path) -> Result<tokio::fs::File, AppError> {
    // 先 create_new：存在（含 symlink 目录项）时不跟随、不覆盖。
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(std_file) => Ok(tokio::fs::File::from_std(std_file)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // 续传：no-follow 读写打开既有普通文件。
            open_regular_file_nofollow(path, true).await
        }
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     recovery、chunk 续传与 no-follow 哈希共用“拒绝 symlink、只开普通文件”语义，
///     避免 metadata 跟随与 open 跟随两套路径不一致。
///
/// Code Logic（这个函数做什么）:
///     Unix: `OpenOptionsExt::custom_flags(O_NOFOLLOW)`；Windows: `FILE_FLAG_OPEN_REPARSE_POINT`
///     打开 reparse 自身后拒绝 directory/reparse。`writable=true` 时加 write。
pub(super) async fn open_regular_file_nofollow(
    path: &Path,
    writable: bool,
) -> Result<tokio::fs::File, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(libc::O_NOFOLLOW);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path).map_err(|e| {
            if e.kind() == ErrorKind::Other
                || e.raw_os_error() == Some(libc::ELOOP)
                || e.raw_os_error() == Some(libc::EPERM)
            {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("拒绝跟随符号链接打开: {}: {e}", path.display()),
                )
            } else {
                e
            }
        })?;
        let meta = std_file.metadata()?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000：打开 reparse point 自身而不跟随。
        pub(super) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        pub(super) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path)?;
        let meta = std_file.metadata()?;
        let ft = meta.file_type();
        // Windows 上 is_symlink 覆盖 symlink/junction；再拒绝目录与 reparse point。
        if ft.is_symlink()
            || !ft.is_file()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(not(any(unix, windows)))]
    {
        // 无 no-follow 原语的平台：fail-closed。
        let _ = (path, writable);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法 no-follow 打开文件，拒绝打开接收临时文件",
        )))
    }
}

/// 从已打开的异步文件句柄流式计算 SHA256。
pub(super) async fn hash_reader(file: &mut tokio::fs::File) -> Result<String, AppError> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

