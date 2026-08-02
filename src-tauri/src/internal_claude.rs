//! internal_claude — 把 cc-switch 的某个 claude provider 应用到 cc-partner 内部 headless
//! Claude 调用，且**不**改写 OS 默认 `~/.claude/settings.json`。
//!
//! Business Logic（为什么需要这个模块）:
//!     commit / merge / prompt 优化 / GitHub 解说 / verifier 等内部 Claude 调用默认继承
//!     `~/.claude/settings.json`（cc-switch 维护的 OS 默认 provider）。用户希望这些内部调用
//!     使用一个**不同**的 cc-switch provider，且不与交互式 Claude 会话争用 OS 默认配置。
//!
//!     经查 Claude Code 官方文档：进程 env 会被 settings.json 的 `env` 块覆盖；`--settings`
//!     是浅层 per-key merge，存在 stale-key 泄露风险；唯一无合并/无泄露的机制是
//!     `CLAUDE_CONFIG_DIR`（整体重定位 `~/.claude`，使 claude 只读我们写的 settings.json）。
//!
//! Code Logic（这个模块做什么）:
//!     - 读取所选 provider 的 `settings_config`（provider_manager::store 只读查询）。
//!     - 写入隔离目录 `<data_dir>/claude-config-internal/settings.json`（0600/0700，原子写，
//!       内容一致则跳过），由 `claude_cli` 把该目录作为 `CLAUDE_CONFIG_DIR` 注入 spawn。
//!     - 失败/未配置/找不到 provider 时返回 `Ok(None)`，调用方回落 OS 默认（best-effort，
//!       不阻断内部功能）。

use crate::config;
use crate::error::AppError;
use crate::provider_manager;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

/// 隔离 settings 所在子目录名（落在 `data_dir()` 下）。
const ISO_SUBDIR: &str = "claude-config-internal";
/// 隔离 settings 文件名。
const ISO_SETTINGS_FILENAME: &str = "settings.json";

/// 解析 cc-partner 内部 Claude 调用应使用的隔离 `CLAUDE_CONFIG_DIR`。
///
/// Business Logic:
///     provider_id 为 None/空 → 沿用 OS 默认（返回 None）；否则实时从 cc-switch DB 读取该
///     provider 的 settings_config，写入隔离 settings.json，返回其目录。任何失败都回落 OS
///     默认（Ok(None) + warn），不阻断内部功能。
///
/// Code Logic:
///     1. provider_id trim 后空 → Ok(None)。
///     2. `provider_manager::fetch_claude_settings_config` 取片段；None → warn + Ok(None)。
///     3. 计算隔离路径，ensure_iso_dir，原子写入（内容一致跳过）。
///     4. 返回 Ok(Some(dir))。
pub(crate) async fn resolve_internal_provider_config_dir(
    provider_id: Option<&str>,
) -> Result<Option<PathBuf>, AppError> {
    let id = provider_id.map(str::trim).filter(|s| !s.is_empty());
    let Some(id) = id else {
        return Ok(None);
    };

    let settings_config = match provider_manager::fetch_claude_settings_config(id).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            tracing::warn!(
                "内部 Claude provider '{id}' 在 cc-switch 中未找到 settings_config，回落 OS 默认"
            );
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!("读取内部 Claude provider settings_config 失败，回落 OS 默认: {e}");
            return Ok(None);
        }
    };

    let dir = iso_dir()?;
    if let Err(e) = ensure_iso_dir(&dir) {
        tracing::warn!("创建隔离 Claude 配置目录失败，回落 OS 默认: {e}");
        return Ok(None);
    }
    let target = dir.join(ISO_SETTINGS_FILENAME);
    if let Err(e) = write_settings_atomic(&target, &settings_config) {
        tracing::warn!("写入隔离 Claude settings.json 失败，回落 OS 默认: {e}");
        return Ok(None);
    }
    Ok(Some(dir))
}

/// 计算隔离目录绝对路径：`<data_dir>/claude-config-internal`。
fn iso_dir() -> Result<PathBuf, AppError> {
    Ok(config::data_dir()?.join(ISO_SUBDIR))
}

/// 确保隔离目录存在，并设置 Unix 0700 权限。
fn ensure_iso_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(dir, perms)?;
    }
    Ok(())
}

/// 原子写入隔离 settings.json：内容一致则跳过；否则 sibling temp → flush/sync → rename。
///
/// Code Logic:
///     读现有文件字节与序列化后的新内容比较，一致直接返回；不一致写 temp（Unix 0600）、
///     flush+fsync、rename 覆盖目标、best-effort 目录 fsync。Windows 上 std::fs::rename 对
///     文件目标会覆盖。
fn write_settings_atomic(target: &std::path::Path, settings_config: &Value) -> std::io::Result<()> {
    let new_bytes = serde_json::to_vec_pretty(settings_config)?;
    if let Ok(existing) = std::fs::read(target) {
        if existing == new_bytes {
            return Ok(());
        }
    }
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings 目标路径无父目录",
        )
    })?;
    let tmp = parent.join(format!(
        ".{ISO_SETTINGS_FILENAME}.tmp.{}",
        std::process::id()
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&new_bytes)?;
        f.flush()?;
        #[cfg(unix)]
        {
            f.sync_all()?;
        }
    }
    std::fs::rename(&tmp, target)?;
    #[cfg(unix)]
    {
        let _ = sync_dir(parent);
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &std::path::Path) -> std::io::Result<()> {
    let f = std::fs::File::open(dir)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_provider_id_returns_none_without_io() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(rt
            .block_on(resolve_internal_provider_config_dir(None))
            .unwrap()
            .is_none());
        assert!(rt
            .block_on(resolve_internal_provider_config_dir(Some("   ")))
            .unwrap()
            .is_none());
    }

    #[test]
    fn write_settings_atomic_roundtrips_and_skips_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(ISO_SETTINGS_FILENAME);
        let v = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"https://x.test"}});
        write_settings_atomic(&target, &v).unwrap();
        let first = std::fs::read(&target).unwrap();
        assert_eq!(first, serde_json::to_vec_pretty(&v).unwrap());
        // 相同内容二次写入应跳过（不改 mtime 亦可，这里仅验证不报错且内容一致）。
        write_settings_atomic(&target, &v).unwrap();
        let second = std::fs::read(&target).unwrap();
        assert_eq!(first, second);
        // 内容变化应覆盖。
        let v2 = serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"https://y.test"}});
        write_settings_atomic(&target, &v2).unwrap();
        let third = std::fs::read(&target).unwrap();
        assert_eq!(third, serde_json::to_vec_pretty(&v2).unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "隔离 settings.json 必须为 0600");
        }
    }
}
