//! agent_hub/git/lane — device lane 路径与原子替换
//!
//! Business Logic（为什么需要这个模块）:
//!     每台设备只允许改写 `agent-hub/devices/<deviceId>/`；远端其它 lane、旧
//!     prompts/CC/SSH 工作区文件在 export 时必须原样保留。
//!
//! Code Logic（这个模块做什么）:
//!     提供相对路径拼装、目录清单 inventory，以及「staging → 原子替换本 lane」辅助。

use crate::error::AppError;
use std::fs;
use std::path::{Path, PathBuf};

/// 云端仓库内 Agent Hub 根目录名。
pub const AGENT_HUB_GIT_ROOT: &str = "agent-hub";
/// devices 子目录名。
pub const DEVICES_DIR: &str = "devices";

/// 返回本设备 lane 相对 pathspec（POSIX 斜杠，供 git pathspec 使用）。
///
/// Business Logic（为什么需要这个函数）:
///     commit/push 与 diff 必须严格限制在本 device lane，避免误提交其它设备或旧领域文件。
///
/// Code Logic（这个函数做什么）:
///     拼接 `agent-hub/devices/<deviceId>`；校验 device_id 无路径分隔符。
pub fn device_lane_rel_path(device_id: &str) -> Result<String, AppError> {
    validate_device_id_segment(device_id)?;
    Ok(format!(
        "{AGENT_HUB_GIT_ROOT}/{DEVICES_DIR}/{}",
        device_id.trim()
    ))
}

/// 返回 workdir 下本设备 lane 绝对路径。
///
/// Business Logic: 原子替换与 expand 目标定位。
/// Code Logic: workdir + agent-hub/devices/<deviceId>。
pub fn device_lane_abs_path(workdir: &Path, device_id: &str) -> Result<PathBuf, AppError> {
    validate_device_id_segment(device_id)?;
    Ok(workdir
        .join(AGENT_HUB_GIT_ROOT)
        .join(DEVICES_DIR)
        .join(device_id.trim()))
}

/// 校验 device_id 可作为单路径段。
///
/// Business Logic: 防止 path traversal 写出 workdir 外。
/// Code Logic: 拒绝空串、`..`、`/`、`\` 与 NUL。
fn validate_device_id_segment(device_id: &str) -> Result<(), AppError> {
    let id = device_id.trim();
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err(AppError::validation(
            "agent_hub_git_invalid_device_id".to_string(),
        ));
    }
    Ok(())
}

/// 枚举 workdir 中已有的 agent-hub device lane id（不读内容、不 import）。
///
/// Business Logic（为什么需要这个函数）:
///     证明 fetch 后远端 lane 仅被 inventory，不得进入 Hub；供回归与预览（Task 7）复用。
///
/// Code Logic（这个函数做什么）:
///     扫描 `agent-hub/devices/*` 一级目录名，跳过非目录与隐藏项。
pub fn inventory_agent_hub_device_lanes(workdir: &Path) -> Result<Vec<String>, AppError> {
    let devices = workdir.join(AGENT_HUB_GIT_ROOT).join(DEVICES_DIR);
    if !devices.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in fs::read_dir(&devices).map_err(AppError::from)? {
        let entry = entry.map_err(AppError::from)?;
        let meta = entry.metadata().map_err(AppError::from)?;
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(s) = name.to_str() else {
            continue;
        };
        if s.starts_with('.') {
            continue;
        }
        ids.push(s.to_string());
    }
    ids.sort();
    Ok(ids)
}

/// 将 staging 目录原子替换为 workdir 内本设备 lane。
///
/// Business Logic（为什么需要这个函数）:
///     expand 在 sibling staging 完成后再替换，避免半写 lane；其它 device lane 与
///     prompts/CC/SSH 文件字节不变。
///
/// Code Logic（这个函数做什么）:
///     确保 `agent-hub/devices/` 存在 → 若目标存在则 rename 到 `.bak-<uuid>` →
///     rename staging → 目标 → 删除 bak。失败时尽量回滚 bak。
pub fn replace_device_lane(
    workdir: &Path,
    device_id: &str,
    staging: &Path,
) -> Result<PathBuf, AppError> {
    if !staging.is_dir() {
        return Err(AppError::validation(
            "agent_hub_git_staging_not_dir".to_string(),
        ));
    }
    let devices_root = workdir.join(AGENT_HUB_GIT_ROOT).join(DEVICES_DIR);
    fs::create_dir_all(&devices_root).map_err(AppError::from)?;
    let target = device_lane_abs_path(workdir, device_id)?;

    let bak = if target.exists() {
        let bak = devices_root.join(format!(
            ".bak-{}-{}",
            device_id.trim(),
            uuid::Uuid::new_v4()
        ));
        fs::rename(&target, &bak).map_err(AppError::from)?;
        Some(bak)
    } else {
        None
    };

    match fs::rename(staging, &target) {
        Ok(()) => {
            if let Some(bak) = bak {
                let _ = fs::remove_dir_all(bak);
            }
            Ok(target)
        }
        Err(e) => {
            // 尽力回滚
            if let Some(bak) = bak {
                let _ = fs::rename(&bak, &target);
            }
            Err(AppError::generic(format!(
                "agent_hub_git_lane_replace_failed: {e}"
            )))
        }
    }
}

/// 递归计算目录内容的稳定指纹（路径 + 文件字节 sha256），用于单测断言「只改本 lane」。
///
/// Business Logic: 测试需证明 device-b 与 prompts 未变。
/// Code Logic: 收集相对路径排序后拼接 `path\\0hash\\n` 再 sha256。
#[cfg(test)]
pub fn directory_content_fingerprint(root: &Path) -> Result<String, AppError> {
    use crate::agent_hub::object_store::sha256_hex;
    use sha2::{Digest, Sha256};
    let mut entries: Vec<(String, String)> = Vec::new();
    walk_files(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, hash) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0]);
        hasher.update(hash.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(sha256_hex(&hasher.finalize()))
}

#[cfg(test)]
fn walk_files(
    root: &Path,
    current: &Path,
    out: &mut Vec<(String, String)>,
) -> Result<(), AppError> {
    use crate::agent_hub::object_store::sha256_hex;
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current).map_err(AppError::from)? {
        let entry = entry.map_err(AppError::from)?;
        let path = entry.path();
        let meta = entry.metadata().map_err(AppError::from)?;
        if meta.is_dir() {
            // 跳过 .bak-* 临时目录
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(".bak-") {
                    continue;
                }
            }
            walk_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| AppError::generic("agent_hub_git_fingerprint_prefix"))?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path).map_err(AppError::from)?;
            out.push((rel, sha256_hex(&bytes)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    /// Business Logic: export 只改 device-a，device-b 与 prompts 字节指纹不变。
    /// Code Logic: 播种三处内容 → replace device-a → 比对指纹。
    #[test]
    fn replace_device_lane_only_touches_local_device() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = tmp.path();
        write_file(&workdir.join("prompts").join("p1.json"), r#"{"id":"p1"}"#);
        write_file(
            &workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join(DEVICES_DIR)
                .join("device-a")
                .join("snapshot.json"),
            r#"{"old":"a"}"#,
        );
        write_file(
            &workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join(DEVICES_DIR)
                .join("device-b")
                .join("snapshot.json"),
            r#"{"old":"b"}"#,
        );

        let prompts_fp = directory_content_fingerprint(&workdir.join("prompts")).unwrap();
        let b_fp = directory_content_fingerprint(
            &workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join(DEVICES_DIR)
                .join("device-b"),
        )
        .unwrap();

        let staging = tmp.path().join("staging-a");
        write_file(&staging.join("snapshot.json"), r#"{"new":"a"}"#);
        write_file(&staging.join("user").join("x.json"), r#"{"k":1}"#);

        replace_device_lane(workdir, "device-a", &staging).unwrap();

        assert!(workdir
            .join(AGENT_HUB_GIT_ROOT)
            .join(DEVICES_DIR)
            .join("device-a")
            .join("snapshot.json")
            .exists());
        let body = fs::read_to_string(
            workdir
                .join(AGENT_HUB_GIT_ROOT)
                .join(DEVICES_DIR)
                .join("device-a")
                .join("snapshot.json"),
        )
        .unwrap();
        assert!(body.contains("new"));
        assert_eq!(
            directory_content_fingerprint(&workdir.join("prompts")).unwrap(),
            prompts_fp
        );
        assert_eq!(
            directory_content_fingerprint(
                &workdir
                    .join(AGENT_HUB_GIT_ROOT)
                    .join(DEVICES_DIR)
                    .join("device-b")
            )
            .unwrap(),
            b_fp
        );

        let inventory = inventory_agent_hub_device_lanes(workdir).unwrap();
        assert_eq!(
            inventory,
            vec!["device-a".to_string(), "device-b".to_string()]
        );
    }

    /// Business Logic: 非法 device id 不得形成路径逃逸。
    #[test]
    fn rejects_traversal_device_id() {
        assert!(device_lane_rel_path("../evil").is_err());
        assert!(device_lane_rel_path("a/b").is_err());
        assert!(device_lane_rel_path("").is_err());
    }
}
