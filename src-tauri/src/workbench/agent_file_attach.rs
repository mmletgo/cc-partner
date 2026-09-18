//! 工作台终端把文件交给 Agent 的收集、清洗与落盘。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端 Agent 只能读 owning device 上的文件。拖入/粘贴的本机文件必须先变成可注入的
//!     路径列表：本机项目用原路径，远端项目落到 data_dir 临时目录后再注入。
//!
//! Code Logic（这个模块做什么）:
//!     清洗相对路径、遍历根路径（跳过 symlink）、强制体积/数量上限、把 payload 写到临时目录。

use crate::agent_catalog::HeadlessImagePasteKind;
use crate::error::AppError;
use crate::screenshot::clipboard::headless_image_paste_input;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// 远端拷贝解码后的总字节上限（为 32 MiB JSON+base64 留余量）。
pub const MAX_AGENT_ATTACH_BYTES: u64 = 20 * 1024 * 1024;
/// 单次交给 Agent 的文件数量上限。
pub const MAX_AGENT_ATTACH_FILES: usize = 256;

/// 一次交给 Agent 的文件树。
///
/// Business Logic（为什么需要这个结构体）:
///     远端 POST 与本机 blob 落盘共用同一份树，避免两套相对路径规则。
///
/// Code Logic（这个结构体做什么）:
///     `files` 带字节；`directories` 覆盖空目录；`inject_relative_paths` 是拖入根（文件名或目录名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachPayload {
    pub files: Vec<AttachFile>,
    #[serde(default)]
    pub directories: Vec<String>,
    pub inject_relative_paths: Vec<String>,
}

/// 树中的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachFile {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// P2P/control 线上的文件项（base64，避免 JSON 数字数组膨胀）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachFileWire {
    pub relative_path: String,
    pub content_base64: String,
}

/// 把落盘树编成线上文件列表。
///
/// Business Logic（为什么需要这个函数）:
///     远端 POST 必须走 JSON；原始字节用 base64，才能卡在 32 MiB body 内。
///
/// Code Logic（这个函数做什么）:
///     STANDARD base64 编码每个文件。
pub fn payload_files_to_wire(payload: &AttachPayload) -> Vec<AttachFileWire> {
    payload
        .files
        .iter()
        .map(|file| AttachFileWire {
            relative_path: file.relative_path.clone(),
            content_base64: B64.encode(&file.bytes),
        })
        .collect()
}

/// 从线上文件列表还原落盘树。
///
/// Business Logic（为什么需要这个函数）:
///     owning device 收到 POST 后必须再校验相对路径与体积，不能信任对端。
///
/// Code Logic（这个函数做什么）:
///     解码 base64、清洗路径、强制上限后组装 AttachPayload。
pub fn payload_from_wire(
    files: &[AttachFileWire],
    directories: Vec<String>,
    inject_relative_paths: Vec<String>,
) -> Result<AttachPayload, AppError> {
    let directories = directories
        .into_iter()
        .map(|dir| sanitize_relative_path(&dir))
        .collect::<Result<Vec<_>, _>>()?;
    let inject = inject_relative_paths
        .into_iter()
        .map(|rel| sanitize_relative_path(&rel))
        .collect::<Result<Vec<_>, _>>()?;
    let mut payload = if files.is_empty() {
        AttachPayload {
            files: Vec::new(),
            directories: Vec::new(),
            inject_relative_paths: Vec::new(),
        }
    } else {
        let mut blobs = Vec::with_capacity(files.len());
        for file in files {
            let bytes = B64
                .decode(file.content_base64.as_bytes())
                .map_err(|_| AppError::validation("附件内容不是合法 base64"))?;
            blobs.push((file.relative_path.clone(), bytes));
        }
        collect_from_blobs(&blobs, MAX_AGENT_ATTACH_BYTES, MAX_AGENT_ATTACH_FILES)?
    };
    if !directories.is_empty() {
        payload.directories = directories;
    }
    if !inject.is_empty() {
        payload.inject_relative_paths = inject;
    }
    if payload.files.is_empty() && payload.directories.is_empty() {
        return Err(AppError::validation("没有可交给 Agent 的文件"));
    }
    Ok(payload)
}

/// 清洗协议里的相对路径（只用 `/`）。
///
/// Business Logic（为什么需要这个函数）:
///     远端落盘必须拒绝 `..` / 绝对路径，否则临时目录可被写穿。
///
/// Code Logic（这个函数做什么）:
///     反斜杠改 `/`，拒绝空、绝对、盘符、`.` / `..` / 空段。
pub fn sanitize_relative_path(raw: &str) -> Result<String, AppError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(AppError::validation("文件相对路径非法"));
    }
    let normalized = raw.replace('\\', "/");
    if normalized.starts_with('/') || normalized.starts_with("./") || normalized.contains(':') {
        return Err(AppError::validation("文件相对路径非法"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(AppError::validation("文件相对路径非法"));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(AppError::validation("文件相对路径非法"));
    }
    Ok(parts.join("/"))
}

/// 把多个绝对路径编成 Agent 可消费的 PTY 输入。
///
/// Business Logic（为什么需要这个函数）:
///     文件不走剪贴板 Ctrl+V；各 CLI 用身份表路径语法。
///
/// Code Logic（这个函数做什么）:
///     对每个路径调用 `headless_image_paste_input` 后拼接。
pub fn attach_paths_input(kind: HeadlessImagePasteKind, paths: &[&Path]) -> String {
    paths
        .iter()
        .map(|path| headless_image_paste_input(kind, path))
        .collect()
}

/// Agent 附件临时根目录。
///
/// Business Logic（为什么需要这个函数）:
///     回退文件不得进入用户项目树或传输接收目录。
///
/// Code Logic（这个函数做什么）:
///     `{data_dir}/tmp/agent-files`。
pub fn agent_files_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tmp").join("agent-files")
}

/// 一次 drop 的落盘子目录。
///
/// Business Logic（为什么需要这个函数）:
///     多次拖入不得互相覆盖。
///
/// Code Logic（这个函数做什么）:
///     `{agent-files}/{safe_session}/{drop_id}`。
pub fn agent_attach_drop_dir(data_dir: &Path, session_id: &str, drop_id: &str) -> PathBuf {
    agent_files_dir(data_dir)
        .join(sanitize_session_segment(session_id))
        .join(sanitize_session_segment(drop_id))
}

/// 从本机根路径收集 payload（远端拷贝 / 体积校验）。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 有原生路径，必须在读盘阶段强制上限并跳过 symlink。
///
/// Code Logic（这个函数做什么）:
///     文件用文件名作相对根；目录保留 `dirname/...`；空目录写入 `directories`。
pub fn collect_from_paths(
    paths: &[PathBuf],
    max_bytes: u64,
    max_files: usize,
) -> Result<AttachPayload, AppError> {
    if paths.is_empty() {
        return Err(AppError::validation("没有可交给 Agent 的文件"));
    }
    let mut payload = AttachPayload {
        files: Vec::new(),
        directories: Vec::new(),
        inject_relative_paths: Vec::new(),
    };
    let mut total_bytes: u64 = 0;
    let mut used_names = std::collections::HashSet::new();
    for path in paths {
        let meta = fs::symlink_metadata(path)
            .map_err(|_| AppError::validation(format!("文件不存在: {}", path.display())))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::validation("文件名非法"))?;
        let root = sanitize_relative_path(name)?;
        if !used_names.insert(root.clone()) {
            return Err(AppError::validation(format!("重复的文件名: {root}")));
        }
        payload.inject_relative_paths.push(root.clone());
        if meta.is_dir() {
            collect_dir(
                path,
                &root,
                &mut payload,
                &mut total_bytes,
                max_bytes,
                max_files,
            )?;
            if !payload.files.iter().any(|file| {
                file.relative_path == root || file.relative_path.starts_with(&format!("{root}/"))
            }) && !payload
                .directories
                .iter()
                .any(|dir| dir == &root || dir.starts_with(&format!("{root}/")))
            {
                payload.directories.push(root);
            }
        } else if meta.is_file() {
            push_file(
                path,
                root,
                &mut payload,
                &mut total_bytes,
                max_bytes,
                max_files,
            )?;
        }
    }
    if payload.files.is_empty() && payload.directories.is_empty() {
        return Err(AppError::validation("没有可交给 Agent 的文件"));
    }
    Ok(payload)
}

/// 把 blob 列表收成 payload（粘贴无原生路径）。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器 File 没有绝对路径，只能按文件名落临时目录。
///
/// Code Logic（这个函数做什么）:
///     清洗每个 `relative_path`，累加字节，inject 为第一段。
pub fn collect_from_blobs(
    blobs: &[(String, Vec<u8>)],
    max_bytes: u64,
    max_files: usize,
) -> Result<AttachPayload, AppError> {
    if blobs.is_empty() {
        return Err(AppError::validation("没有可交给 Agent 的文件"));
    }
    let mut payload = AttachPayload {
        files: Vec::new(),
        directories: Vec::new(),
        inject_relative_paths: Vec::new(),
    };
    let mut total_bytes: u64 = 0;
    let mut used_names = std::collections::HashSet::new();
    for (raw_path, bytes) in blobs {
        let relative = sanitize_relative_path(raw_path)?;
        let root = relative
            .split('/')
            .next()
            .ok_or_else(|| AppError::validation("文件相对路径非法"))?
            .to_string();
        if payload.files.len() >= max_files {
            return Err(AppError::validation(format!(
                "交给 Agent 的文件不能超过 {max_files} 个"
            )));
        }
        let next = total_bytes.saturating_add(bytes.len() as u64);
        if next > max_bytes {
            return Err(AppError::validation(format!(
                "交给 Agent 的文件总大小不能超过 {} MiB",
                max_bytes / (1024 * 1024)
            )));
        }
        total_bytes = next;
        if used_names.insert(root.clone()) {
            payload.inject_relative_paths.push(root);
        }
        payload.files.push(AttachFile {
            relative_path: relative,
            bytes: bytes.clone(),
        });
    }
    Ok(payload)
}

/// 把 payload 写到 drop 目录，返回应注入的绝对路径。
///
/// Business Logic（为什么需要这个函数）:
///     owning device 上 Agent 只能读真实路径。
///
/// Code Logic（这个函数做什么）:
///     mkdir 目录项与文件父目录，写文件，按 inject 列表拼绝对路径。
pub fn persist_attach_payload(
    base: &Path,
    payload: &AttachPayload,
) -> Result<Vec<PathBuf>, AppError> {
    fs::create_dir_all(base)?;
    for dir in &payload.directories {
        let rel = sanitize_relative_path(dir)?;
        fs::create_dir_all(join_under_base(base, &rel)?)?;
    }
    for file in &payload.files {
        let rel = sanitize_relative_path(&file.relative_path)?;
        let dest = join_under_base(base, &rel)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, &file.bytes)?;
    }
    let mut inject = Vec::with_capacity(payload.inject_relative_paths.len());
    for rel in &payload.inject_relative_paths {
        let cleaned = sanitize_relative_path(rel)?;
        inject.push(join_under_base(base, &cleaned)?);
    }
    Ok(inject)
}

fn collect_dir(
    dir: &Path,
    prefix: &str,
    payload: &mut AttachPayload,
    total_bytes: &mut u64,
    max_bytes: u64,
    max_files: usize,
) -> Result<(), AppError> {
    let mut saw_child = false;
    let entries = fs::read_dir(dir).map_err(|e| AppError::generic(format!("读取目录失败: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| AppError::generic(format!("读取目录失败: {e}")))?;
        let child = entry.path();
        let meta = match fs::symlink_metadata(&child) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = child
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::validation("文件名非法"))?;
        let relative = sanitize_relative_path(&format!("{prefix}/{name}"))?;
        saw_child = true;
        if meta.is_dir() {
            collect_dir(
                &child,
                &relative,
                payload,
                total_bytes,
                max_bytes,
                max_files,
            )?;
        } else if meta.is_file() {
            push_file(&child, relative, payload, total_bytes, max_bytes, max_files)?;
        }
    }
    if !saw_child {
        payload.directories.push(prefix.to_string());
    }
    Ok(())
}

fn push_file(
    path: &Path,
    relative: String,
    payload: &mut AttachPayload,
    total_bytes: &mut u64,
    max_bytes: u64,
    max_files: usize,
) -> Result<(), AppError> {
    if payload.files.len() >= max_files {
        return Err(AppError::validation(format!(
            "交给 Agent 的文件不能超过 {max_files} 个"
        )));
    }
    let bytes = fs::read(path).map_err(|e| AppError::generic(format!("读取文件失败: {e}")))?;
    let next = total_bytes.saturating_add(bytes.len() as u64);
    if next > max_bytes {
        return Err(AppError::validation(format!(
            "交给 Agent 的文件总大小不能超过 {} MiB",
            max_bytes / (1024 * 1024)
        )));
    }
    *total_bytes = next;
    payload.files.push(AttachFile {
        relative_path: relative,
        bytes,
    });
    Ok(())
}

fn join_under_base(base: &Path, relative: &str) -> Result<PathBuf, AppError> {
    let mut dest = base.to_path_buf();
    for part in relative.split('/') {
        dest.push(part);
    }
    Ok(dest)
}

fn sanitize_session_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(80));
    for ch in raw.chars() {
        if out.len() >= 80 {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn sanitize_relative_path_rejects_traversal_and_absolute() {
        assert!(sanitize_relative_path("a/b.txt").is_ok());
        assert!(sanitize_relative_path("../secret").is_err());
        assert!(sanitize_relative_path("/etc/passwd").is_err());
        assert!(sanitize_relative_path("foo/../bar").is_err());
        assert!(sanitize_relative_path("").is_err());
        assert!(sanitize_relative_path("C:\\\\Windows").is_err());
    }

    #[test]
    fn collect_from_paths_keeps_file_and_folder_roots() {
        let tmp = TempDir::new().expect("tmp");
        let pdf = write_file(tmp.path(), "note.pdf", b"pdf");
        let folder = tmp.path().join("logs");
        fs::create_dir(&folder).expect("dir");
        fs::write(folder.join("a.log"), b"aaa").expect("log");
        let payload = collect_from_paths(
            &[pdf, folder],
            MAX_AGENT_ATTACH_BYTES,
            MAX_AGENT_ATTACH_FILES,
        )
        .expect("collect");
        assert_eq!(payload.inject_relative_paths, vec!["note.pdf", "logs"]);
        assert!(payload
            .files
            .iter()
            .any(|file| file.relative_path == "note.pdf" && file.bytes == b"pdf"));
        assert!(payload
            .files
            .iter()
            .any(|file| file.relative_path == "logs/a.log" && file.bytes == b"aaa"));
    }

    #[cfg(unix)]
    #[test]
    fn collect_from_paths_skips_symlinks_and_keeps_empty_dir() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().expect("tmp");
        let target = write_file(tmp.path(), "real.txt", b"ok");
        let link = tmp.path().join("alias.txt");
        symlink(&target, &link).expect("symlink");
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).expect("empty");
        let payload = collect_from_paths(
            &[link, empty],
            MAX_AGENT_ATTACH_BYTES,
            MAX_AGENT_ATTACH_FILES,
        )
        .expect("collect");
        assert!(payload.files.is_empty());
        assert_eq!(payload.inject_relative_paths, vec!["empty"]);
        assert_eq!(payload.directories, vec!["empty"]);
    }

    #[test]
    fn collect_from_paths_rejects_oversize() {
        let tmp = TempDir::new().expect("tmp");
        let big = write_file(tmp.path(), "big.bin", &[0u8; 32]);
        let err = collect_from_paths(&[big], 16, MAX_AGENT_ATTACH_FILES).expect_err("oversize");
        assert!(err.to_string().contains("总大小"));
    }

    #[test]
    fn persist_attach_payload_writes_tree_and_inject_paths() {
        let tmp = TempDir::new().expect("tmp");
        let payload = AttachPayload {
            files: vec![AttachFile {
                relative_path: "logs/a.log".into(),
                bytes: b"aaa".to_vec(),
            }],
            directories: vec!["empty".into()],
            inject_relative_paths: vec!["logs".into(), "empty".into()],
        };
        let base = tmp.path().join("drop");
        let inject = persist_attach_payload(&base, &payload).expect("persist");
        assert_eq!(fs::read(base.join("logs/a.log")).expect("read"), b"aaa");
        assert!(base.join("empty").is_dir());
        assert_eq!(inject[0], base.join("logs"));
        assert_eq!(inject[1], base.join("empty"));
    }

    #[test]
    fn attach_paths_input_uses_agent_syntax() {
        let path = Path::new("/tmp/note.pdf");
        assert_eq!(
            attach_paths_input(HeadlessImagePasteKind::AtFileMention, &[path]),
            "@/tmp/note.pdf "
        );
        assert_eq!(
            attach_paths_input(HeadlessImagePasteKind::TypedAbsolutePath, &[path]),
            " /tmp/note.pdf "
        );
    }

    #[test]
    fn collect_from_blobs_uses_first_segment_as_inject_root() {
        let payload = collect_from_blobs(
            &[
                ("note.pdf".into(), b"pdf".to_vec()),
                ("logs/a.log".into(), b"aaa".to_vec()),
            ],
            MAX_AGENT_ATTACH_BYTES,
            MAX_AGENT_ATTACH_FILES,
        )
        .expect("blobs");
        assert_eq!(payload.inject_relative_paths, vec!["note.pdf", "logs"]);
        assert_eq!(payload.files.len(), 2);
    }
}
