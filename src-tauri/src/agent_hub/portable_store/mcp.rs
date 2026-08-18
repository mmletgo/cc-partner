//! portable_store/mcp — Hub 目录 MCP JSON（0600，凭据在文件内）
//!
//! Business Logic（为什么需要这个模块）:
//!     MCP 是配置 leaf 不是目录，不能软链；目录里保存一份含 env/headers 原文的 JSON，
//!     再经现有 config_patch 投影到各 Agent。
//!
//! Code Logic（这个模块做什么）:
//!     Unix 用 `mode(0o600)` 写入；读回 serde_json Value。

use crate::error::AppError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// 写入 store MCP JSON；Unix 权限 0600。
///
/// Business Logic: 凭据只活在这份子文件里，不得进 inventory DTO/日志。
/// Code Logic: 先写临时文件再 rename；Unix `OpenOptionsExt::mode(0o600)`。
pub fn write_mcp_store_json(path: &Path, value: &serde_json::Value) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension("json.tmp");
    write_private_file(&tmp, &bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// 读取 store MCP JSON。
pub fn read_mcp_store_json(path: &Path) -> Result<serde_json::Value, AppError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::validation(format!("PORTABLE_STORE_MCP_JSON_INVALID:{e}")))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}
