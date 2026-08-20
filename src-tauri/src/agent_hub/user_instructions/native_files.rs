//! 用户级原生提示词文件（各 CLI 配置目录里真实加载的 AGENTS.md / CLAUDE.md / GEMINI.md）。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户级提示词不再走 Hub 三槽投影；要直接编辑 Claude/Codex 等真正读取的文件。
//!     多数 Agent 在**各自配置目录**使用 `AGENTS.md`，这些是不同文件；只有路径相同时才共用。
//!     OpenCode 在自身 AGENTS.md 缺失时会回退 Claude 的 `CLAUDE.md`，那才是真正的共用。
//!     不得把 Hub 独有槽（cc-partner.exclusive、AGENTS.override.md、rules）当成编辑目标。
//!
//! Code Logic（这个模块做什么）:
//!     按 TargetHomes 白名单解析路径；有界读 UTF-8；CAS 原子写。不查 support manifest L3。

use crate::agent_hub::object_store::sha256_hex;
use crate::agent_hub::projection::{AtomicProjectionWriter, AtomicWriteOutcome, FileWriteRequest};
use crate::agent_hub::targets::{TargetEnvironment, TargetHomes, TargetPathResolver};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 单文件正文上限（与 inspect canonical 一致）。
const MAX_NATIVE_FILE_BYTES: usize = 256 * 1024;

/// 读取用户级原生提示词文件请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadUserNativeInstructionFileRequest {
    pub path: String,
}

/// 写入用户级原生提示词文件请求。
///
/// Business Logic: `expectedHash=None` 表示文件应尚不存在（创建）；Some 则 CAS 更新。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteUserNativeInstructionFileRequest {
    pub path: String,
    pub content: String,
    pub expected_hash: Option<String>,
}

/// 用户级原生提示词文件快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserNativeInstructionFileDto {
    pub path: String,
    pub exists: bool,
    pub content: String,
    pub hash: Option<String>,
    pub truncated: bool,
    pub created: bool,
}

/// 读取白名单内的用户级原生提示词文件。
///
/// Business Logic: 缺失文件返回空正文 exists=false，便于编辑后创建。
/// Code Logic: 解析允许路径 → 有界读；非 UTF-8 / 过大 fail-closed。
pub fn read_user_native_instruction_file(
    env: &TargetEnvironment,
    request: &ReadUserNativeInstructionFileRequest,
) -> Result<UserNativeInstructionFileDto, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let path = resolve_allowed_native_path(&request.path, &homes)?;
    read_resolved_native_file(&path)
}

/// 写入白名单内的用户级原生提示词文件。
///
/// Business Logic: 用户在编辑器里保存真实文件，不是 Hub 三槽投影。
/// Code Logic: 解析允许路径 → 大小/UTF-8 校验 → AtomicProjectionWriter CAS。
pub fn write_user_native_instruction_file(
    env: &TargetEnvironment,
    request: &WriteUserNativeInstructionFileRequest,
) -> Result<UserNativeInstructionFileDto, AppError> {
    let homes = TargetPathResolver::resolve_all(env);
    let path = resolve_allowed_native_path(&request.path, &homes)?;
    if request.content.len() > MAX_NATIVE_FILE_BYTES {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_CONTENT_TOO_LARGE".to_string(),
        ));
    }
    let bytes = request.content.as_bytes();
    let rendered_hash = sha256_hex(bytes);
    let writer = AtomicProjectionWriter::default();
    let outcome = writer
        .write_file(FileWriteRequest {
            target: &path,
            rendered_bytes: bytes,
            rendered_hash: &rendered_hash,
            expected_external_hash: request.expected_hash.as_deref(),
        })
        .map_err(|e| AppError::generic(format!("USER_NATIVE_INSTRUCTION_WRITE_FAILED:{e}")))?;
    match outcome {
        AtomicWriteOutcome::Replaced { target_hash, .. }
        | AtomicWriteOutcome::AlreadyRendered { target_hash } => {
            let created = request.expected_hash.is_none();
            Ok(UserNativeInstructionFileDto {
                path: path.to_string_lossy().into_owned(),
                exists: true,
                content: request.content.clone(),
                hash: Some(target_hash),
                truncated: false,
                created,
            })
        }
        AtomicWriteOutcome::Drift { .. } => Err(AppError::conflict(
            "USER_NATIVE_INSTRUCTION_STALE".to_string(),
        )),
        AtomicWriteOutcome::DirectoryUnknownFiles { .. } => Err(AppError::generic(
            "USER_NATIVE_INSTRUCTION_UNEXPECTED_DIRECTORY_OUTCOME".to_string(),
        )),
    }
}

/// 用户级允许直接编辑的原生文件（各配置根下的约定文件名）。
///
/// Business Logic: Codex/OpenCode/Grok/Pi 各自目录的 AGENTS.md 是不同文件；
///     Claude 是 CLAUDE.md；Gemini 是 GEMINI.md。不含 Hub 独有槽。
fn declared_native_paths(homes: &TargetHomes) -> Vec<PathBuf> {
    vec![
        homes.claude.config_root.join("CLAUDE.md"),
        homes.codex.config_root.join("AGENTS.md"),
        homes.opencode.config_root.join("AGENTS.md"),
        homes.gemini.config_root.join("GEMINI.md"),
        homes.grok.config_root.join("AGENTS.md"),
        homes.grok.config_root.join("CLAUDE.md"),
        homes.pi.config_root.join("AGENTS.md"),
        homes.pi.config_root.join("CLAUDE.md"),
    ]
}

/// 把请求路径解析到白名单候选（允许大小写不敏感文件名）。
///
/// Business Logic: 禁止任意 home 路径；只接受各 Agent 对应目录的约定文件。
/// Code Logic: 必须绝对路径；文件名大小写不敏感、父目录与候选 config 根相等。
fn resolve_allowed_native_path(raw: &str, homes: &TargetHomes) -> Result<PathBuf, AppError> {
    let requested = PathBuf::from(raw.trim());
    if raw.trim().is_empty()
        || !requested.is_absolute()
        || raw.contains('\0')
        || requested
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED".to_string(),
        ));
    }
    let Some(requested_name) = requested.file_name().and_then(|name| name.to_str()) else {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED".to_string(),
        ));
    };
    let Some(requested_parent) = requested.parent() else {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED".to_string(),
        ));
    };
    for candidate in declared_native_paths(homes) {
        let Some(candidate_name) = candidate.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(candidate_parent) = candidate.parent() else {
            continue;
        };
        if !path_eq_ignore_ascii_case(requested_parent, candidate_parent) {
            continue;
        }
        if !requested_name.eq_ignore_ascii_case(candidate_name) {
            continue;
        }
        if requested.is_file() {
            return Ok(requested);
        }
        return Ok(candidate);
    }
    Err(AppError::validation(
        "USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED".to_string(),
    ))
}

/// 比较两条路径（ASCII 大小写不敏感，用于 macOS 默认磁盘）。
fn path_eq_ignore_ascii_case(left: &Path, right: &Path) -> bool {
    let left_s = left.to_string_lossy();
    let right_s = right.to_string_lossy();
    left_s.eq_ignore_ascii_case(&right_s)
}

/// 有界读取已解析路径。
fn read_resolved_native_file(path: &Path) -> Result<UserNativeInstructionFileDto, AppError> {
    if !path.exists() {
        return Ok(UserNativeInstructionFileDto {
            path: path.to_string_lossy().into_owned(),
            exists: false,
            content: String::new(),
            hash: None,
            truncated: false,
            created: false,
        });
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_PATH_NOT_ALLOWED".to_string(),
        ));
    }
    if metadata.len() > MAX_NATIVE_FILE_BYTES as u64 * 4 {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_CONTENT_TOO_LARGE".to_string(),
        ));
    }
    let bytes = std::fs::read(path)?;
    let hash = sha256_hex(&bytes);
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(AppError::validation(
            "USER_NATIVE_INSTRUCTION_NOT_UTF8".to_string(),
        ));
    };
    let truncated = text.len() > MAX_NATIVE_FILE_BYTES;
    let content = if truncated {
        truncate_utf8(&text, MAX_NATIVE_FILE_BYTES)
    } else {
        text
    };
    Ok(UserNativeInstructionFileDto {
        path: path.to_string_lossy().into_owned(),
        exists: true,
        content,
        hash: Some(hash),
        truncated,
        created: false,
    })
}

/// 按字节上限截 UTF-8，不切到码点中间。
fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    fn isolated_env(home: &Path) -> TargetEnvironment {
        TargetEnvironment {
            home: home.to_path_buf(),
            vars: BTreeMap::new(),
            path_entries: Vec::new(),
        }
    }

    #[test]
    fn allowlist_accepts_each_home_agents_md_as_distinct_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(tmp.path());
        let homes = TargetPathResolver::resolve_all(&env);
        let codex = homes.codex.config_root.join("AGENTS.md");
        let opencode = homes.opencode.config_root.join("AGENTS.md");
        assert_ne!(codex, opencode);
        assert!(resolve_allowed_native_path(codex.to_str().unwrap(), &homes).is_ok());
        assert!(resolve_allowed_native_path(opencode.to_str().unwrap(), &homes).is_ok());
        let grok_agents = homes.grok.config_root.join("AGENTS.md");
        assert!(resolve_allowed_native_path(grok_agents.to_str().unwrap(), &homes).is_ok());
    }

    #[test]
    fn allowlist_rejects_hub_exclusive_and_override_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(tmp.path());
        let homes = TargetPathResolver::resolve_all(&env);
        let exclusive = homes
            .grok
            .config_root
            .join("rules")
            .join("cc-partner.exclusive.md");
        let override_path = homes.codex.config_root.join("AGENTS.override.md");
        let cursor_rule = homes
            .cursor
            .config_root
            .join("rules")
            .join("cc-partner.exclusive.mdc");
        assert!(resolve_allowed_native_path(exclusive.to_str().unwrap(), &homes).is_err());
        assert!(resolve_allowed_native_path(override_path.to_str().unwrap(), &homes).is_err());
        assert!(resolve_allowed_native_path(cursor_rule.to_str().unwrap(), &homes).is_err());
        assert!(resolve_allowed_native_path("/etc/passwd", &homes).is_err());
        assert!(resolve_allowed_native_path("AGENTS.md", &homes).is_err());
    }

    #[test]
    fn write_then_read_codex_agents_md_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(tmp.path());
        let homes = TargetPathResolver::resolve_all(&env);
        let path = homes.codex.config_root.join("AGENTS.md");
        let written = write_user_native_instruction_file(
            &env,
            &WriteUserNativeInstructionFileRequest {
                path: path.to_string_lossy().into_owned(),
                content: "# shared user agents\n".into(),
                expected_hash: None,
            },
        )
        .expect("write");
        assert!(written.exists);
        assert!(written.created);
        let on_disk = fs::read_to_string(&path).expect("read disk");
        assert_eq!(on_disk, "# shared user agents\n");
        let read = read_user_native_instruction_file(
            &env,
            &ReadUserNativeInstructionFileRequest {
                path: path.to_string_lossy().into_owned(),
            },
        )
        .expect("read");
        assert_eq!(read.content, "# shared user agents\n");
        assert_eq!(read.hash, written.hash);
    }

    #[test]
    fn stale_cas_hash_is_conflict() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = isolated_env(tmp.path());
        let homes = TargetPathResolver::resolve_all(&env);
        let path = homes.claude.config_root.join("CLAUDE.md");
        write_user_native_instruction_file(
            &env,
            &WriteUserNativeInstructionFileRequest {
                path: path.to_string_lossy().into_owned(),
                content: "first\n".into(),
                expected_hash: None,
            },
        )
        .expect("create");
        let err = write_user_native_instruction_file(
            &env,
            &WriteUserNativeInstructionFileRequest {
                path: path.to_string_lossy().into_owned(),
                content: "second\n".into(),
                expected_hash: Some("deadbeef".into()),
            },
        )
        .expect_err("stale");
        assert!(err.to_string().contains("USER_NATIVE_INSTRUCTION_STALE"));
    }
}
