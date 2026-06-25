//! workbench/file_content.rs — 工作台文件内容读写核心
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench 文件查看器需要安全读取、格式化并保存项目中的文本文件，同时避免覆盖用户在外部编辑器中的并发修改。
//!
//! Code Logic（这个模块做什么）:
//!     提供文本大小限制、SHA256 基准哈希、UTF-8 文本读取、带 base_hash 校验的原子保存以及结构化内容格式化。

#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::AppError;
use sha2::{Digest, Sha256};

/// 单个可编辑文本文件的最大字节数。
///
/// Business Logic（为什么需要这个常量）:
///     文件工作区第一版面向轻量编辑，必须拒绝过大的文本文件，避免阻塞 UI 或占用过多内存。
///
/// Code Logic（这个常量做什么）:
///     以字节为单位定义 5MB 上限，读写入口都会用它做硬限制。
pub const MAX_EDITABLE_TEXT_BYTES: u64 = 5 * 1024 * 1024;

/// Business Logic（为什么需要这个函数）:
///     打开和保存文本文件时需要稳定基线，防止 Workbench 覆盖外部编辑器产生的并发修改。
///
/// Code Logic（这个函数做什么）:
///     用 8KB 缓冲流式读取文件并计算 SHA256，返回小写十六进制字符串，不一次性载入大文件。
pub fn sha256_file_hex(path: &Path) -> Result<String, AppError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Business Logic（为什么需要这个函数）:
///     文件工作区需要读取可编辑文本并把打开时 hash 返回给前端，作为后续保存的乐观锁基线。
///
/// Code Logic（这个函数做什么）:
///     先拒绝超过 5MB 的文件，再读取字节、校验 UTF-8，并返回文本内容和对应 SHA256 hash。
pub fn read_text_file(path: &Path) -> Result<(String, String), AppError> {
    let metadata = fs::metadata(path)?;
    ensure_editable_size(metadata.len())?;

    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = format!("{:x}", hasher.finalize());
    let content = String::from_utf8(bytes)
        .map_err(|_| AppError::generic("文件不是有效 UTF-8 文本，无法在 Workbench 中编辑"))?;

    Ok((content, hash))
}

/// Business Logic（为什么需要这个函数）:
///     用户保存文件时，Workbench 必须拒绝覆盖外部已修改内容，并尽量保证写入过程不会留下半截文件。
///
/// Code Logic（这个函数做什么）:
///     检查内容大小与当前文件 hash；hash 一致时写入同目录唯一临时文件，flush/sync 后 rename 替换目标，
///     成功返回新文件 SHA256，失败时尽量删除临时文件。
pub fn save_text_file_atomic(
    path: &Path,
    content: &str,
    base_hash: &str,
) -> Result<String, AppError> {
    ensure_editable_size(content.len() as u64)?;

    let current_hash = sha256_file_hex(path)?;
    if current_hash != base_hash {
        return Err(AppError::generic("文件已被修改，请重新打开文件后再保存"));
    }

    let temporary_path = temporary_save_path(path)?;
    let write_result = write_temporary_file(&temporary_path, content);
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(AppError::from(err));
    }

    sha256_file_hex(path)
}

/// Business Logic（为什么需要这个函数）:
///     JSON/TOML 文件保存前和编辑中需要可靠格式化，格式错误时必须拒绝，避免写入无效配置。
///
/// Code Logic（这个函数做什么）:
///     根据 kind 分发到 serde_json 或 toml_edit 解析器；解析成功返回格式化文本，未知类型返回业务错误。
pub fn format_structured_content(kind: &str, content: &str) -> Result<String, AppError> {
    match kind {
        "json" => {
            let value = serde_json::from_str::<serde_json::Value>(content)
                .map_err(|err| AppError::generic(format!("JSON 格式无效: {err}")))?;
            let mut formatted = serde_json::to_string_pretty(&value)?;
            formatted.push('\n');
            Ok(formatted)
        }
        "toml" => {
            let document = content
                .parse::<toml_edit::DocumentMut>()
                .map_err(|err| AppError::generic(format!("TOML 格式无效: {err}")))?;
            Ok(document.to_string())
        }
        other => Err(AppError::generic(format!(
            "暂不支持格式化 {other} 类型文件"
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     读写入口共享同一套大小限制，确保超限文件不会被加载或保存。
///
/// Code Logic（这个函数做什么）:
///     比较字节数与 MAX_EDITABLE_TEXT_BYTES，超限时返回业务错误。
fn ensure_editable_size(size: u64) -> Result<(), AppError> {
    if size > MAX_EDITABLE_TEXT_BYTES {
        return Err(AppError::generic(format!(
            "文件超过 {} 字节上限，无法在 Workbench 中编辑",
            MAX_EDITABLE_TEXT_BYTES
        )));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     原子保存必须在目标文件同目录写临时文件，保证 rename 时位于同一文件系统。
///
/// Code Logic（这个函数做什么）:
///     用目标文件名和 UUID 生成隐藏临时文件路径，不实际创建文件。
fn temporary_save_path(path: &Path) -> Result<PathBuf, AppError> {
    let parent = path
        .parent()
        .filter(|candidate| !candidate.as_os_str().is_empty())
        .ok_or_else(|| AppError::generic("文件路径缺少父目录，无法保存"))?;
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::generic("文件名为空，无法保存"))?;

    Ok(parent.join(format!(".{}.{}.tmp", file_name, uuid::Uuid::new_v4())))
}

/// Business Logic（为什么需要这个函数）:
///     保存流程要把临时文件写入细节集中处理，失败时调用方才能统一清理残留。
///
/// Code Logic（这个函数做什么）:
///     使用 create_new 防碰撞写入 UTF-8 字节，flush 后 sync_all，确保 rename 前内容已经落到临时文件。
fn write_temporary_file(path: &Path, content: &str) -> Result<(), AppError> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Business Logic（为什么需要这个函数）:
    ///     文件内容测试需要互不影响的真实目录，验证 rename/hash/UTF-8 行为。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在系统临时目录下创建带 UUID 的目录，并返回路径给单测清理。
    fn temp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户格式化错误 JSON 时不能产生看似成功的编辑结果。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入非法 JSON，断言格式化返回包含 JSON 提示的错误。
    #[test]
    fn rejects_invalid_json_formatting() {
        let err =
            format_structured_content("json", "{bad json").expect_err("invalid JSON rejected");
        assert!(err.to_string().contains("JSON"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户格式化错误 TOML 时必须得到拒绝，避免配置文件被错误保存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入非法 TOML，断言格式化返回包含 TOML 提示的错误。
    #[test]
    fn rejects_invalid_toml_formatting() {
        let err = format_structured_content("toml", "name = ").expect_err("invalid TOML rejected");
        assert!(err.to_string().contains("TOML"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     JSON 文件格式化应产出可读缩进，方便用户在文件工作区检查配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     输入紧凑 JSON，断言输出包含缩进行和末尾换行。
    #[test]
    fn formats_valid_json_pretty() {
        let formatted = format_structured_content("json", r#"{"name":"cc","items":[1,2]}"#)
            .expect("valid JSON formatted");
        assert!(formatted.contains("\n  \"name\": \"cc\""));
        assert!(formatted.ends_with('\n'));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     TOML 文件格式化应在合法内容上成功，供设置类文件编辑使用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     输入合法 TOML，断言输出仍包含关键字段和表头。
    #[test]
    fn formats_valid_toml() {
        let formatted = format_structured_content("toml", "name='cc'\n[tool]\nenabled=true")
            .expect("valid TOML formatted");
        assert!(formatted.contains("name"));
        assert!(formatted.contains("cc"));
        assert!(formatted.contains("[tool]"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     baseHash 是并发保存基线，文件内容变化时 hash 必须变化。
    ///
    /// Code Logic（这个测试做什么）:
    ///     两次写入不同内容并分别计算 SHA256，断言 hash 不相同。
    #[test]
    fn hash_changes_after_file_content_changes() {
        let dir = temp_dir("ccp-file-content-hash");
        let path = dir.join("note.txt");
        fs::write(&path, "first").expect("write first");
        let first = sha256_file_hex(&path).expect("hash first");
        fs::write(&path, "second").expect("write second");
        let second = sha256_file_hex(&path).expect("hash second");
        assert_ne!(first, second);
        fs::remove_dir_all(dir).expect("cleanup");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 UTF-8 文件不能进入文本编辑器，否则会显示乱码并可能破坏原文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入非法字节，断言 read_text_file 返回 UTF-8 相关错误。
    #[test]
    fn read_text_file_rejects_non_utf8() {
        let dir = temp_dir("ccp-file-content-utf8");
        let path = dir.join("bad.txt");
        fs::write(&path, [0xff, 0xfe, 0xfd]).expect("write invalid utf8");
        let err = read_text_file(&path).expect_err("non UTF-8 rejected");
        assert!(err.to_string().contains("UTF-8"));
        fs::remove_dir_all(dir).expect("cleanup");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     外部修改发生后保存必须拒绝，不能覆盖用户在其他编辑器里的改动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用错误 baseHash 保存，断言返回冲突错误且原文件内容保持不变。
    #[test]
    fn save_text_file_atomic_rejects_base_hash_mismatch_without_overwrite() {
        let dir = temp_dir("ccp-file-content-conflict");
        let path = dir.join("note.txt");
        fs::write(&path, "current").expect("write current");
        let err =
            save_text_file_atomic(&path, "new", "stale-hash").expect_err("stale hash rejected");
        assert!(err.to_string().contains("已被修改") || err.to_string().contains("hash"));
        assert_eq!(fs::read_to_string(&path).expect("read current"), "current");
        fs::remove_dir_all(dir).expect("cleanup");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     正常保存后前端需要新的 baseHash，并且磁盘内容必须完成更新。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用正确 baseHash 保存新内容，断言返回新 hash、文件内容和磁盘 hash 一致。
    #[test]
    fn save_text_file_atomic_updates_file_and_returns_new_hash() {
        let dir = temp_dir("ccp-file-content-save");
        let path = dir.join("note.txt");
        fs::write(&path, "old").expect("write old");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let new_hash = save_text_file_atomic(&path, "new", &base_hash).expect("save text");
        assert_ne!(base_hash, new_hash);
        assert_eq!(fs::read_to_string(&path).expect("read new"), "new");
        assert_eq!(sha256_file_hex(&path).expect("hash new"), new_hash);
        fs::remove_dir_all(dir).expect("cleanup");
    }
}
