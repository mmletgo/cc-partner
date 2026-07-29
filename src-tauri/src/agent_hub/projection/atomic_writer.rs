//! agent_hub/projection/atomic_writer — 原子投影写盘
//!
//! Business Logic（为什么需要这个模块）:
//!     Hub 投影必须保证崩溃时目标文件要么完整旧内容要么完整新内容，绝不能留下半截文件。
//!
//! Code Logic（这个模块做什么）:
//!     单文件 sibling temp + sync + precondition recheck + rename + rehash；
//!     目录 sibling staging + backup rename，backup 仅在 materialization committed 后删除。
//!     提供 test-only 故障注入 seam，覆盖 temp write / sync / precondition / rename / rehash。

use crate::agent_hub::object_store::sha256_hex;
use crate::error::AppError;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 故障注入点（仅 debug/test）。
///
/// Business Logic（为什么需要这个枚举）:
///     L2 故障注入必须能在每个提交边界复现“旧或新完整文件”不变量。
///
/// Code Logic（这个枚举做什么）:
///     命名注入阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionWriteFault {
    /// 临时文件写入失败
    TempWrite,
    /// 临时文件 fsync 失败
    FileSync,
    /// rename 前 precondition 二次校验失败（模拟外部并发改动）
    PreconditionRecheck,
    /// rename 失败
    Rename,
    /// 目标 re-hash 失败
    TargetRehash,
    /// DB commit 阶段失败（由 scheduler 侧注入）
    DbCommit,
}

/// 原子写结果。
///
/// Business Logic（为什么需要这个枚举）:
///     scheduler 需要区分成功、漂移与可恢复失败。
///
/// Code Logic（这个枚举做什么）:
///     携带最终 hash 或漂移信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtomicWriteOutcome {
    /// 原子替换成功，目标 hash == rendered
    Replaced {
        /// 目标最终 hash
        target_hash: String,
        /// 若有备份路径（目录）
        backup_path: Option<PathBuf>,
        /// staging 路径（用于 cleanup）
        staging_path: PathBuf,
    },
    /// 目标已是 rendered hash，无需写盘
    AlreadyRendered {
        /// 目标 hash
        target_hash: String,
    },
    /// 目标相对 expected/base 已漂移，禁止盲写
    Drift {
        /// 当前目标 hash（None=不存在）
        current_hash: Option<String>,
    },
    /// 目录目标存在未知外部文件
    DirectoryUnknownFiles {
        /// 未知相对路径
        unknown_paths: Vec<String>,
    },
}

/// 单文件原子写请求。
///
/// Business Logic（为什么需要这个结构体）:
///     调用方传入渲染字节与 expected external hash。
///
/// Code Logic（这个结构体做什么）:
///     保存路径与内容引用。
#[derive(Debug, Clone)]
pub struct FileWriteRequest<'a> {
    /// 目标路径
    pub target: &'a Path,
    /// 渲染字节
    pub rendered_bytes: &'a [u8],
    /// 渲染 hash（必须等于 sha256(rendered_bytes)）
    pub rendered_hash: &'a str,
    /// 写前期望外部 hash；None 表示目标应不存在
    pub expected_external_hash: Option<&'a str>,
}

/// 目录原子写请求。
///
/// Business Logic（为什么需要这个结构体）:
///     Skill/Plugin 整目录投影，未知文件禁止递归删除。
///
/// Code Logic（这个结构体做什么）:
///     managed_paths 为受管相对路径；entries 为 (相对路径, 字节)。
#[derive(Debug, Clone)]
pub struct DirectoryWriteRequest<'a> {
    /// 目标目录
    pub target_dir: &'a Path,
    /// 受管相对路径（正斜杠）
    pub managed_paths: &'a [String],
    /// 要写入的条目
    pub entries: &'a [(String, Vec<u8>)],
    /// 渲染树 manifest hash
    pub rendered_hash: &'a str,
    /// 写前期望外部树 hash；None 表示目标应不存在
    pub expected_external_hash: Option<&'a str>,
}

/// 原子投影写入器。
///
/// Business Logic（为什么需要这个结构体）:
///     集中实现 sibling temp / staging 策略与故障注入 seam。
///
/// Code Logic（这个结构体做什么）:
///     无状态 helper；test 下可读注入点。
#[derive(Debug, Default, Clone)]
pub struct AtomicProjectionWriter {
    #[cfg(any(test, debug_assertions))]
    fault: Option<ProjectionWriteFault>,
}

impl AtomicProjectionWriter {
    /// 构造默认写入器。
    ///
    /// Business Logic: 生产路径无故障注入。
    /// Code Logic: fault=None。
    pub fn new() -> Self {
        Self::default()
    }

    /// 测试/debug 注入故障点。
    ///
    /// Business Logic: quality_faults 需在指定阶段失败。
    /// Code Logic: 仅 test/debug_assertions 生效。
    #[cfg(any(test, debug_assertions))]
    pub fn with_fault(fault: ProjectionWriteFault) -> Self {
        Self { fault: Some(fault) }
    }

    /// 原子写单文件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     投影文件必须 sibling temp + precondition + rename，保证无半截可见文件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) 校验 rendered_hash；2) 读当前 hash；
    ///     already rendered → AlreadyRendered；
    ///     与 expected 不符 → Drift；
    ///     写 temp → sync → recheck → rename → rehash。
    pub fn write_file(&self, req: FileWriteRequest<'_>) -> Result<AtomicWriteOutcome, AppError> {
        let computed = sha256_hex(req.rendered_bytes);
        if computed != req.rendered_hash {
            return Err(AppError::validation(format!(
                "agent_hub_rendered_hash_mismatch:expected={},actual={}",
                req.rendered_hash, computed
            )));
        }

        let current = optional_file_hash(req.target)?;
        if current.as_deref() == Some(req.rendered_hash) {
            return Ok(AtomicWriteOutcome::AlreadyRendered {
                target_hash: req.rendered_hash.to_string(),
            });
        }
        if !hash_matches_expected(current.as_deref(), req.expected_external_hash) {
            return Ok(AtomicWriteOutcome::Drift {
                current_hash: current,
            });
        }

        let parent = req
            .target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| AppError::validation("agent_hub_target_missing_parent"))?;
        fs::create_dir_all(parent)?;
        let staging = sibling_temp_path(req.target, "proj")?;

        let write_result = (|| -> Result<AtomicWriteOutcome, AppError> {
            self.maybe_fault(ProjectionWriteFault::TempWrite)?;
            {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&staging)?;
                file.write_all(req.rendered_bytes)?;
                file.flush()?;
                self.maybe_fault(ProjectionWriteFault::FileSync)?;
                file.sync_all()?;
            }

            // rename 前再次校验目标未漂移
            let recheck = optional_file_hash(req.target)?;
            if !hash_matches_expected(recheck.as_deref(), req.expected_external_hash) {
                self.maybe_fault(ProjectionWriteFault::PreconditionRecheck)?;
                return Ok(AtomicWriteOutcome::Drift {
                    current_hash: recheck,
                });
            }
            self.maybe_fault(ProjectionWriteFault::PreconditionRecheck)?;

            self.maybe_fault(ProjectionWriteFault::Rename)?;
            fs::rename(&staging, req.target)?;
            sync_dir(parent);

            self.maybe_fault(ProjectionWriteFault::TargetRehash)?;
            let final_hash = file_hash(req.target)?;
            if final_hash != req.rendered_hash {
                return Err(AppError::generic(format!(
                    "agent_hub_target_hash_mismatch:expected={},actual={final_hash}",
                    req.rendered_hash
                )));
            }
            Ok(AtomicWriteOutcome::Replaced {
                target_hash: final_hash,
                backup_path: None,
                staging_path: staging.clone(),
            })
        })();

        if write_result.is_err()
            || matches!(
                write_result,
                Ok(AtomicWriteOutcome::Drift { .. })
                    | Ok(AtomicWriteOutcome::AlreadyRendered { .. })
            )
        {
            let _ = fs::remove_file(&staging);
        }
        // 成功 rename 后 staging 已不存在；若仍在则清理
        if matches!(write_result, Ok(AtomicWriteOutcome::Replaced { .. })) {
            let _ = fs::remove_file(&staging);
        }
        write_result
    }

    /// 原子写目录（sibling staging + backup rename）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     目录投影在存在未知外部文件时必须 drift/preview，绝不递归删除。
    ///
    /// Code Logic（这个函数做什么）:
    ///     扫描未知路径 → staging 写全量受管文件 → backup 旧目录 → rename staging→target。
    pub fn write_directory(
        &self,
        req: DirectoryWriteRequest<'_>,
    ) -> Result<AtomicWriteOutcome, AppError> {
        if req.target_dir.exists() {
            let unknown = collect_unknown_paths(req.target_dir, req.managed_paths)?;
            if !unknown.is_empty() {
                return Ok(AtomicWriteOutcome::DirectoryUnknownFiles {
                    unknown_paths: unknown,
                });
            }
            // 若目标已完全匹配 rendered（简化：全部 managed 文件 hash 一致且无未知）
            // 调用方以 expected_external_hash 判定；这里仅用 expected。
            let current = optional_tree_fingerprint(req.target_dir, req.managed_paths)?;
            if current.as_deref() == Some(req.rendered_hash) {
                return Ok(AtomicWriteOutcome::AlreadyRendered {
                    target_hash: req.rendered_hash.to_string(),
                });
            }
            if !hash_matches_expected(current.as_deref(), req.expected_external_hash) {
                return Ok(AtomicWriteOutcome::Drift {
                    current_hash: current,
                });
            }
        } else if req.expected_external_hash.is_some() {
            return Ok(AtomicWriteOutcome::Drift { current_hash: None });
        }

        let parent = req
            .target_dir
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| AppError::validation("agent_hub_target_dir_missing_parent"))?;
        fs::create_dir_all(parent)?;
        let staging = sibling_temp_path(req.target_dir, "projdir")?;
        let backup = sibling_temp_path(req.target_dir, "projbak")?;

        let write_result = (|| -> Result<AtomicWriteOutcome, AppError> {
            self.maybe_fault(ProjectionWriteFault::TempWrite)?;
            fs::create_dir_all(&staging)?;
            for (rel, bytes) in req.entries {
                let dest = staging.join(rel);
                if let Some(p) = dest.parent() {
                    fs::create_dir_all(p)?;
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dest)?;
                file.write_all(bytes)?;
                file.flush()?;
                self.maybe_fault(ProjectionWriteFault::FileSync)?;
                file.sync_all()?;
            }

            // precondition recheck：目标仍匹配 expected
            if req.target_dir.exists() {
                let recheck = optional_tree_fingerprint(req.target_dir, req.managed_paths)?;
                if !hash_matches_expected(recheck.as_deref(), req.expected_external_hash) {
                    self.maybe_fault(ProjectionWriteFault::PreconditionRecheck)?;
                    return Ok(AtomicWriteOutcome::Drift {
                        current_hash: recheck,
                    });
                }
                let unknown = collect_unknown_paths(req.target_dir, req.managed_paths)?;
                if !unknown.is_empty() {
                    return Ok(AtomicWriteOutcome::DirectoryUnknownFiles {
                        unknown_paths: unknown,
                    });
                }
            }
            self.maybe_fault(ProjectionWriteFault::PreconditionRecheck)?;

            let mut backup_path = None;
            if req.target_dir.exists() {
                self.maybe_fault(ProjectionWriteFault::Rename)?;
                fs::rename(req.target_dir, &backup)?;
                backup_path = Some(backup.clone());
            }
            self.maybe_fault(ProjectionWriteFault::Rename)?;
            fs::rename(&staging, req.target_dir)?;
            sync_dir(parent);

            self.maybe_fault(ProjectionWriteFault::TargetRehash)?;
            let final_hash = optional_tree_fingerprint(req.target_dir, req.managed_paths)?
                .ok_or_else(|| AppError::generic("agent_hub_directory_missing_after_rename"))?;
            if final_hash != req.rendered_hash {
                // 尝试回滚：把 backup 移回
                if let Some(ref bak) = backup_path {
                    let _ = fs::rename(req.target_dir, &staging);
                    let _ = fs::rename(bak, req.target_dir);
                }
                return Err(AppError::generic(format!(
                    "agent_hub_directory_hash_mismatch:expected={},actual={final_hash}",
                    req.rendered_hash
                )));
            }

            Ok(AtomicWriteOutcome::Replaced {
                target_hash: final_hash,
                backup_path,
                staging_path: staging.clone(),
            })
        })();

        // 失败清理 staging；backup 保留给上层决定
        if !matches!(write_result, Ok(AtomicWriteOutcome::Replaced { .. })) {
            let _ = remove_path_all(&staging);
            // 若 backup 已创建但 rename 失败导致目标丢失，尝试恢复
            if !req.target_dir.exists() && backup.exists() {
                let _ = fs::rename(&backup, req.target_dir);
            } else if write_result.is_err() {
                let _ = remove_path_all(&backup);
            }
        }
        write_result
    }

    /// 在 materialization committed 后删除目录 backup。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     backup 只能在 DB 提交后删除，防止 crash 丢失旧目录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     remove_dir_all best-effort。
    pub fn delete_backup_after_commit(backup: &Path) -> Result<(), AppError> {
        if backup.exists() {
            remove_path_all(backup)?;
        }
        Ok(())
    }

    /// 故障注入检查。
    ///
    /// Business Logic: 测试 seam。
    /// Code Logic: 匹配则返回错误。
    fn maybe_fault(&self, point: ProjectionWriteFault) -> Result<(), AppError> {
        #[cfg(any(test, debug_assertions))]
        {
            if self.fault == Some(point) {
                return Err(AppError::generic(format!(
                    "agent_hub_projection_injected_fault:{}",
                    fault_name(point)
                )));
            }
        }
        let _ = point;
        Ok(())
    }
}

/// 故障点名称。
#[cfg(any(test, debug_assertions))]
fn fault_name(point: ProjectionWriteFault) -> &'static str {
    match point {
        ProjectionWriteFault::TempWrite => "temp_write",
        ProjectionWriteFault::FileSync => "file_sync",
        ProjectionWriteFault::PreconditionRecheck => "precondition_recheck",
        ProjectionWriteFault::Rename => "rename",
        ProjectionWriteFault::TargetRehash => "target_rehash",
        ProjectionWriteFault::DbCommit => "db_commit",
    }
}

/// expected 与 current 是否匹配。
///
/// Business Logic: None expected 表示目标应不存在。
/// Code Logic: 两边 Option 相等。
fn hash_matches_expected(current: Option<&str>, expected: Option<&str>) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(c), Some(e)) => c == e,
        _ => false,
    }
}

/// 生成 sibling 临时路径。
///
/// Business Logic: rename 必须同文件系统。
/// Code Logic: `.{name}.{tag}.{uuid}.tmp`。
fn sibling_temp_path(target: &Path, tag: &str) -> Result<PathBuf, AppError> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| AppError::validation("agent_hub_target_missing_parent"))?;
    let name = target
        .file_name()
        .map(|s| s.to_string_lossy())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::validation("agent_hub_target_empty_name"))?;
    Ok(parent.join(format!(".{name}.{tag}.{}.tmp", Uuid::new_v4())))
}

/// 计算文件 hash。
fn file_hash(path: &Path) -> Result<String, AppError> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(sha256_hex(&buf))
}

/// 可选文件 hash；不存在返回 None。
fn optional_file_hash(path: &Path) -> Result<Option<String>, AppError> {
    if !path.exists() {
        return Ok(None);
    }
    if path.is_dir() {
        return Err(AppError::validation(format!(
            "agent_hub_expected_file_got_dir:{}",
            path.display()
        )));
    }
    Ok(Some(file_hash(path)?))
}

/// 目录受管文件指纹（sorted path+hash 再 sha256）。
fn optional_tree_fingerprint(
    dir: &Path,
    managed_paths: &[String],
) -> Result<Option<String>, AppError> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut parts: Vec<String> = Vec::new();
    let mut sorted: Vec<&String> = managed_paths.iter().collect();
    sorted.sort();
    for rel in sorted {
        let p = dir.join(rel);
        if p.is_file() {
            parts.push(format!("{rel}:{}", file_hash(&p)?));
        } else {
            parts.push(format!("{rel}:missing"));
        }
    }
    Ok(Some(sha256_hex(parts.join("\n").as_bytes())))
}

/// 收集目录中不在 managed 集合的未知相对路径。
///
/// Business Logic: 未知文件触发 drift，禁止删除。
/// Code Logic: walk 一层+子目录，跳过 `.*.tmp` 投影临时。
fn collect_unknown_paths(dir: &Path, managed_paths: &[String]) -> Result<Vec<String>, AppError> {
    let managed: std::collections::HashSet<&str> =
        managed_paths.iter().map(|s| s.as_str()).collect();
    let mut unknown = Vec::new();
    if !dir.is_dir() {
        return Ok(unknown);
    }
    walk_collect(dir, dir, &managed, &mut unknown)?;
    unknown.sort();
    Ok(unknown)
}

fn walk_collect(
    root: &Path,
    current: &Path,
    managed: &std::collections::HashSet<&str>,
    unknown: &mut Vec<String>,
) -> Result<(), AppError> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') && name.ends_with(".tmp") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| AppError::generic("agent_hub_path_strip_failed"))?
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            // 目录本身若不在 managed 且无 managed 子前缀，记 unknown
            let has_child = managed.iter().any(|m| m.starts_with(&format!("{rel}/")));
            if !has_child && !managed.contains(rel.as_str()) {
                unknown.push(rel);
            } else {
                walk_collect(root, &path, managed, unknown)?;
            }
        } else if !managed.contains(rel.as_str()) {
            unknown.push(rel);
        }
    }
    Ok(())
}

fn remove_path_all(path: &Path) -> Result<(), AppError> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(file) = File::open(dir) {
            let _ = file.sync_all();
        }
    }
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_file_replaces_when_expected_matches() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("CLAUDE.md");
        fs::write(&target, b"old").unwrap();
        let old_hash = sha256_hex(b"old");
        let new_bytes = b"new content";
        let new_hash = sha256_hex(new_bytes);
        let writer = AtomicProjectionWriter::new();
        let out = writer
            .write_file(FileWriteRequest {
                target: &target,
                rendered_bytes: new_bytes,
                rendered_hash: &new_hash,
                expected_external_hash: Some(&old_hash),
            })
            .unwrap();
        assert!(matches!(out, AtomicWriteOutcome::Replaced { .. }));
        assert_eq!(fs::read(&target).unwrap(), new_bytes);
        assert_eq!(file_hash(&target).unwrap(), new_hash);
    }

    #[test]
    fn write_file_already_rendered_is_noop() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("f.txt");
        let bytes = b"same";
        let hash = sha256_hex(bytes);
        fs::write(&target, bytes).unwrap();
        let writer = AtomicProjectionWriter::new();
        let out = writer
            .write_file(FileWriteRequest {
                target: &target,
                rendered_bytes: bytes,
                rendered_hash: &hash,
                expected_external_hash: Some("other"),
            })
            .unwrap();
        assert!(matches!(out, AtomicWriteOutcome::AlreadyRendered { .. }));
    }

    #[test]
    fn write_file_drift_when_target_differs() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, b"external").unwrap();
        let bytes = b"hub";
        let hash = sha256_hex(bytes);
        let writer = AtomicProjectionWriter::new();
        let out = writer
            .write_file(FileWriteRequest {
                target: &target,
                rendered_bytes: bytes,
                rendered_hash: &hash,
                expected_external_hash: Some(&sha256_hex(b"base")),
            })
            .unwrap();
        assert!(matches!(out, AtomicWriteOutcome::Drift { .. }));
        assert_eq!(fs::read(&target).unwrap(), b"external");
    }

    #[test]
    fn temp_write_fault_leaves_old_file() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("f.txt");
        fs::write(&target, b"old").unwrap();
        let old_hash = sha256_hex(b"old");
        let bytes = b"new";
        let hash = sha256_hex(bytes);
        let writer = AtomicProjectionWriter::with_fault(ProjectionWriteFault::TempWrite);
        let err = writer
            .write_file(FileWriteRequest {
                target: &target,
                rendered_bytes: bytes,
                rendered_hash: &hash,
                expected_external_hash: Some(&old_hash),
            })
            .unwrap_err();
        assert!(err.to_string().contains("injected_fault"));
        assert_eq!(fs::read(&target).unwrap(), b"old");
        // 无残留 temp
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn directory_unknown_files_never_deleted() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("skill");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("SKILL.md"), b"managed").unwrap();
        fs::write(target.join("user-notes.txt"), b"keep me").unwrap();
        let managed = vec!["SKILL.md".to_string()];
        let entries = vec![("SKILL.md".to_string(), b"new".to_vec())];
        let rendered = sha256_hex(b"tree");
        let writer = AtomicProjectionWriter::new();
        let out = writer
            .write_directory(DirectoryWriteRequest {
                target_dir: &target,
                managed_paths: &managed,
                entries: &entries,
                rendered_hash: &rendered,
                expected_external_hash: Some("whatever"),
            })
            .unwrap();
        match out {
            AtomicWriteOutcome::DirectoryUnknownFiles { unknown_paths } => {
                assert!(unknown_paths.iter().any(|p| p == "user-notes.txt"));
            }
            other => panic!("expected unknown files, got {other:?}"),
        }
        assert_eq!(fs::read(target.join("user-notes.txt")).unwrap(), b"keep me");
    }
}
