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

#[cfg(test)]
use std::cell::RefCell;

use crate::error::AppError;
use sha2::{Digest, Sha256};

#[cfg(test)]
thread_local! {
    static BEFORE_FINAL_HASH_CHECK_HOOK: RefCell<Option<Box<dyn Fn() + 'static>>> = RefCell::new(None);
}

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
///     检查内容大小与当前文件 hash；hash 一致时写入同目录唯一临时文件，继承目标文件的 std Permissions
///     后再次检查目标 hash，最后用 rename 替换目标并返回新 hash。该流程是缩小窗口的乐观锁，不是文件系统级强 CAS；
///     权限继承仅覆盖 Unix mode bits / Windows readonly 等 `std::fs::Permissions` 能表达的基础语义，不承诺保留 ACL、
///     xattrs 或 macOS 扩展属性。
pub fn save_text_file_atomic(
    path: &Path,
    content: &str,
    base_hash: &str,
) -> Result<String, AppError> {
    ensure_editable_size(content.len() as u64)?;

    ensure_base_hash_matches(path, base_hash)?;

    let temporary_path = temporary_save_path(path)?;
    let write_result = write_temporary_file(&temporary_path, content);
    if let Err(err) = write_result {
        cleanup_temporary_file(&temporary_path);
        return Err(err);
    }

    if let Err(err) = inherit_target_permissions(path, &temporary_path) {
        cleanup_temporary_file(&temporary_path);
        return Err(err);
    }

    run_before_final_hash_check_hook_for_test();

    if let Err(err) = ensure_base_hash_matches(path, base_hash) {
        cleanup_temporary_file(&temporary_path);
        return Err(err);
    }

    if let Err(err) = fs::rename(&temporary_path, path) {
        cleanup_temporary_file(&temporary_path);
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

/// Business Logic（为什么需要这个函数）:
///     保存流程需要在打开时和 rename 前各校验一次 baseHash，拒绝覆盖外部编辑器改动。
///
/// Code Logic（这个函数做什么）:
///     计算当前目标文件 SHA256 并与 base_hash 比较，不一致时返回统一冲突错误。
fn ensure_base_hash_matches(path: &Path, base_hash: &str) -> Result<(), AppError> {
    let current_hash = sha256_file_hex(path)?;
    if current_hash != base_hash {
        return Err(AppError::generic("文件已被修改，请重新打开文件后再保存"));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     原子保存会用新临时文件替换目标文件，必须尽量保留 std Permissions 能表达的基础权限语义。
///
/// Code Logic（这个函数做什么）:
///     读取目标文件 metadata.permissions() 并设置到临时文件；这会保留 Unix mode bits / Windows readonly 等基础语义，
///     但不承诺保留 ACL、xattrs 或 macOS 扩展属性。失败时返回错误，让调用方删除临时文件并停止保存。
fn inherit_target_permissions(target_path: &Path, temporary_path: &Path) -> Result<(), AppError> {
    let permissions = fs::metadata(target_path)?.permissions();
    fs::set_permissions(temporary_path, permissions)?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     保存失败时应尽最大努力删除同目录临时文件；Windows readonly 临时文件直接 remove 可能失败。
///
/// Code Logic（这个函数做什么）:
///     先读取临时文件权限，若 readonly 则尝试解除 readonly，再执行 remove_file；所有错误都被忽略。
fn cleanup_temporary_file(path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            let _ = fs::set_permissions(path, permissions);
        }
    }
    let _ = fs::remove_file(path);
}

/// Business Logic（为什么需要这个函数）:
///     单测需要稳定模拟“临时文件写好后、rename 前目标文件被外部修改”的并发窗口。
///
/// Code Logic（这个函数做什么）:
///     在测试构建中运行一次注册的 hook；生产构建中没有该函数调用效果。
#[cfg(test)]
fn run_before_final_hash_check_hook_for_test() {
    BEFORE_FINAL_HASH_CHECK_HOOK.with(|hook| {
        let hook = hook.borrow_mut().take();
        if let Some(hook) = hook {
            hook();
        }
    });
}

/// Business Logic（为什么需要这个函数）:
///     生产代码不应携带测试 hook 行为，但同一保存流程需要在非测试构建中可编译。
///
/// Code Logic（这个函数做什么）:
///     非测试构建下提供空实现，让保存路径不受测试辅助逻辑影响。
#[cfg(not(test))]
fn run_before_final_hash_check_hook_for_test() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Business Logic（为什么需要这个函数）:
    ///     文件内容测试需要互不影响的真实目录，验证 rename/hash/UTF-8 行为。
    ///
    /// Code Logic（这个函数做什么）:
    ///     使用 tempfile 创建 RAII 临时目录，测试 panic 时也由 Drop 尽量清理。
    fn temp_dir() -> TempDir {
        tempfile::tempdir().expect("create temp dir")
    }

    /// Business Logic（为什么需要这个结构体）:
    ///     TOCTOU 单测会注册一次全局 hook，测试结束时必须清理以免影响其他测试。
    ///
    /// Code Logic（这个结构体做什么）:
    ///     Drop 时清空 BEFORE_FINAL_HASH_CHECK_HOOK。
    struct TestHookGuard;

    impl Drop for TestHookGuard {
        fn drop(&mut self) {
            BEFORE_FINAL_HASH_CHECK_HOOK.with(|hook| {
                *hook.borrow_mut() = None;
            });
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     保存流程是同步函数，测试需要在固定位置插入外部修改来验证二次 hash 检查。
    ///
    /// Code Logic（这个函数做什么）:
    ///     注册一个只执行一次的 hook，并返回 RAII guard 在测试结束时清理。
    fn set_before_final_hash_check_hook_for_test(hook: impl Fn() + 'static) -> TestHookGuard {
        BEFORE_FINAL_HASH_CHECK_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        TestHookGuard
    }

    /// Business Logic（为什么需要这个函数）:
    ///     冲突保存失败时应清理同目录临时文件，避免项目目录里积累隐藏残留。
    ///
    /// Code Logic（这个函数做什么）:
    ///     统计目录下符合 `.note.txt.*.tmp` 命名模式的临时保存文件。
    fn count_note_temp_files(dir: &TempDir) -> usize {
        fs::read_dir(dir.path())
            .expect("read temp dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(".note.txt.") && name.ends_with(".tmp")
            })
            .count()
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
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "first").expect("write first");
        let first = sha256_file_hex(&path).expect("hash first");
        fs::write(&path, "second").expect("write second");
        let second = sha256_file_hex(&path).expect("hash second");
        assert_ne!(first, second);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 UTF-8 文件不能进入文本编辑器，否则会显示乱码并可能破坏原文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入非法字节，断言 read_text_file 返回 UTF-8 相关错误。
    #[test]
    fn read_text_file_rejects_non_utf8() {
        let dir = temp_dir();
        let path = dir.path().join("bad.txt");
        fs::write(&path, [0xff, 0xfe, 0xfd]).expect("write invalid utf8");
        let err = read_text_file(&path).expect_err("non UTF-8 rejected");
        assert!(err.to_string().contains("UTF-8"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     超过 5MB 的文件不应被读入编辑器，避免 Workbench 阻塞或占用过多内存。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入比 MAX_EDITABLE_TEXT_BYTES 大 1 字节的 UTF-8 文件，断言 read_text_file 拒绝。
    #[test]
    fn read_text_file_rejects_oversized_file() {
        let dir = temp_dir();
        let path = dir.path().join("large.txt");
        fs::write(&path, vec![b'a'; (MAX_EDITABLE_TEXT_BYTES + 1) as usize])
            .expect("write large file");
        let err = read_text_file(&path).expect_err("oversized read rejected");
        assert!(err.to_string().contains("上限"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     外部修改发生后保存必须拒绝，不能覆盖用户在其他编辑器里的改动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用错误 baseHash 保存，断言返回冲突错误且原文件内容保持不变。
    #[test]
    fn save_text_file_atomic_rejects_base_hash_mismatch_without_overwrite() {
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "current").expect("write current");
        let err =
            save_text_file_atomic(&path, "new", "stale-hash").expect_err("stale hash rejected");
        assert!(err.to_string().contains("已被修改") || err.to_string().contains("hash"));
        assert_eq!(fs::read_to_string(&path).expect("read current"), "current");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     初次 hash 校验通过后，rename 前仍可能发生外部修改，Workbench 不能覆盖这个窗口里的改动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 test hook 在临时文件写好后改写目标文件，断言保存拒绝且保留外部修改内容。
    #[test]
    fn save_text_file_atomic_rechecks_hash_before_rename() {
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "old").expect("write old");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let hook_path = path.clone();
        let _guard = set_before_final_hash_check_hook_for_test(move || {
            fs::write(&hook_path, "external").expect("external write");
        });

        let err = save_text_file_atomic(&path, "new", &base_hash)
            .expect_err("external write before rename rejected");
        assert!(err.to_string().contains("已被修改") || err.to_string().contains("hash"));
        assert_eq!(
            fs::read_to_string(&path).expect("read external"),
            "external"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     rename 前发现二次 hash 冲突时，临时文件必须被清理，不能污染用户项目目录。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 hook 制造二次 hash 冲突，断言保存失败后不存在 `.note.txt.*.tmp` 残留。
    #[test]
    fn save_text_file_atomic_cleans_temp_file_after_final_hash_conflict() {
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "old").expect("write old");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let hook_path = path.clone();
        let _guard = set_before_final_hash_check_hook_for_test(move || {
            fs::write(&hook_path, "external").expect("external write");
        });

        let err = save_text_file_atomic(&path, "new", &base_hash)
            .expect_err("external write before rename rejected");
        assert!(err.to_string().contains("已被修改") || err.to_string().contains("hash"));
        assert_eq!(count_note_temp_files(&dir), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Windows 上 readonly 临时文件可能无法直接删除，清理 helper 需要先尽力解除 readonly。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 readonly 临时文件后调用 cleanup_temporary_file，断言文件最终不存在。
    #[test]
    fn cleanup_temporary_file_removes_readonly_temp_file() {
        let dir = temp_dir();
        let path = dir.path().join(".note.txt.manual.tmp");
        fs::write(&path, "temp").expect("write temp");
        let mut permissions = fs::metadata(&path).expect("temp metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("set readonly");

        cleanup_temporary_file(&path);

        assert!(!path.exists());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     保存超限内容同样会阻塞 UI 并可能导致大文件误写，必须在落盘前拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用正确 baseHash 保存超过上限 1 字节的内容，断言拒绝且原文件不变。
    #[test]
    fn save_text_file_atomic_rejects_oversized_content_without_overwrite() {
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "old").expect("write old");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let content = "a".repeat((MAX_EDITABLE_TEXT_BYTES + 1) as usize);
        let err = save_text_file_atomic(&path, &content, &base_hash)
            .expect_err("oversized save rejected");
        assert!(err.to_string().contains("上限"));
        assert_eq!(fs::read_to_string(&path).expect("read old"), "old");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     保存脚本文件时不能改变完整 Unix mode，否则用户原本可执行/私有语义会失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Unix 下把文件权限设为 755，保存后断言低 9 位完整 mode 仍为 755。
    #[cfg(unix)]
    #[test]
    fn save_text_file_atomic_preserves_unix_executable_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let path = dir.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\necho old\n").expect("write script");
        let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("set executable");
        let base_hash = sha256_file_hex(&path).expect("base hash");

        save_text_file_atomic(&path, "#!/bin/sh\necho new\n", &base_hash).expect("save script");

        let mode = fs::metadata(&path)
            .expect("script metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     私密配置文件保存后不能被 umask 或临时文件默认权限放宽，否则可能泄露敏感配置。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Unix 下把文件权限设为 600，保存后断言低 9 位完整 mode 仍为 600。
    #[cfg(unix)]
    #[test]
    fn save_text_file_atomic_preserves_unix_private_config_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir();
        let path = dir.path().join("secret.env");
        fs::write(&path, "TOKEN=old\n").expect("write secret");
        let mut permissions = fs::metadata(&path).expect("secret metadata").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&path, permissions).expect("set private mode");
        let base_hash = sha256_file_hex(&path).expect("base hash");

        save_text_file_atomic(&path, "TOKEN=new\n", &base_hash).expect("save secret");

        let mode = fs::metadata(&path)
            .expect("secret metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     非 Unix 平台也应尽量保留 readonly 这类基础权限语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     非 Unix 下把文件设为 readonly，保存后断言 readonly 标记仍存在。
    #[cfg(not(unix))]
    #[test]
    fn save_text_file_atomic_preserves_readonly_flag() {
        let dir = temp_dir();
        let path = dir.path().join("config.txt");
        fs::write(&path, "old").expect("write config");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let mut permissions = fs::metadata(&path).expect("config metadata").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("set readonly");

        save_text_file_atomic(&path, "new", &base_hash).expect("save config");

        assert!(fs::metadata(&path)
            .expect("config metadata")
            .permissions()
            .readonly());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     正常保存后前端需要新的 baseHash，并且磁盘内容必须完成更新。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用正确 baseHash 保存新内容，断言返回新 hash、文件内容和磁盘 hash 一致。
    #[test]
    fn save_text_file_atomic_updates_file_and_returns_new_hash() {
        let dir = temp_dir();
        let path = dir.path().join("note.txt");
        fs::write(&path, "old").expect("write old");
        let base_hash = sha256_file_hex(&path).expect("base hash");
        let new_hash = save_text_file_atomic(&path, "new", &base_hash).expect("save text");
        assert_ne!(base_hash, new_hash);
        assert_eq!(fs::read_to_string(&path).expect("read new"), "new");
        assert_eq!(sha256_file_hex(&path).expect("hash new"), new_hash);
    }
}
