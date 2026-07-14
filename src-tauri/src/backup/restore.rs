//! backup/restore.rs — 事务恢复管道与 pre-restore 备份保留策略
//!
//! Business Logic（为什么需要这个模块）:
//!     inspect → 用户选 merge/replace-domain → exclusive maintenance gate →
//!     pre-restore 备份 → 单事务导入选中领域 → 状态机可崩溃恢复；
//!     config report 永不写回；备份保留 7 天且最多 3 份。
//!
//! Code Logic（这个模块做什么）:
//!     BackupRestoreService 编排 recovery_jobs + archive 读写 + 领域 bulk_upsert；
//!     pre-restore 备份使用用户私有权限，新备份完整后才删旧。

use crate::backup::archive::{
    inspect_archive_streaming, read_entry_bytes, ArchiveLimits, DOMAIN_CC_HISTORY,
    DOMAIN_CLAUDE_MD, DOMAIN_CONFIG_REPORT, DOMAIN_DELETION_FLOORS, DOMAIN_PROMPTS,
    DOMAIN_SCRATCHPAD, DOMAIN_SSH_TARGETS,
};
use crate::error::AppError;
use crate::models::prompt::PromptRow;
use crate::models::scratchpad::ScratchpadRow;
use crate::models::ssh_target::SshTargetRow;
use crate::state::AppState;
use crate::storage::deletion_floor_repo::DeletionFloor;
use crate::storage::maintenance_gate::{
    begin_write_with_permit, DatabaseMaintenanceGate, DatabaseWritePermit,
};
use crate::storage::recovery_job_repo::{RecoveryJobRepo, RecoveryJobRow, RecoveryJobStatus};
use crate::cc::models::ClaudeHistoryRow;
use crate::models::claude_md::ClaudeMdRow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// pre-restore 备份最长保留 7 天。
pub const PRE_RESTORE_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
/// 最多保留 3 份完整备份。
pub const PRE_RESTORE_MAX_COUNT: usize = 3;

/// 恢复模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoreMode {
    /// 向量时钟 / conflict copy 规则合并。
    Merge,
    /// 仅替换用户勾选领域（先清空该域再导入）。
    ReplaceDomain,
}

/// 恢复请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    pub archive_path: String,
    pub mode: RestoreMode,
    /// 勾选领域 token（prompts/ccHistory/...）；不含 configReport。
    pub domains: Vec<String>,
}

/// 只读预览（零写入）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectPreview {
    pub format_version: u32,
    pub domain_counts: BTreeMap<String, u32>,
    pub warnings: Vec<String>,
    pub conflicts_estimate: u32,
}

/// 恢复结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult {
    pub job_id: String,
    pub status: RecoveryJobStatus,
    pub applied_domains: Vec<String>,
    pub pre_restore_backup_path: Option<String>,
    pub error_summary: Option<String>,
}

/// 导出备份结果（Settings「导出数据」）。
///
/// Business Logic（为什么需要这个结构）:
///     前端导出成功后需要展示路径与格式版本，供用户归档与后续 inspect。
///
/// Code Logic（这个结构做什么）:
///     camelCase：`path` + `formatVersion`；与 control API / Tauri 命令共用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBackupResult {
    pub path: String,
    pub format_version: u32,
}

/// pre-restore 备份列表项。
///
/// Business Logic（为什么需要这个结构）:
///     Settings 恢复页列出恢复前自动备份，便于用户识别时间。
///
/// Code Logic（这个结构做什么）:
///     camelCase：`path` + 可选 `createdAt`（优先从文件名时间戳解析）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreRestoreBackupInfo {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// 从 pre-restore 文件名解析时间戳（`pre-restore-YYYYMMDDTHHMMSSZ.zip`）。
///
/// Business Logic（为什么需要这个函数）:
///     列表展示时间不必读 mtime；文件名已含 UTC 时间戳。
///
/// Code Logic（这个函数做什么）:
///     剥离前缀/后缀后返回原始时间戳串；格式不匹配则 None。
pub fn parse_pre_restore_created_at(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_prefix("pre-restore-")?.strip_suffix(".zip")?;
    if stem.len() >= 16 && stem.contains('T') {
        Some(stem.to_string())
    } else {
        None
    }
}

/// 将路径列表映射为 PreRestoreBackupInfo。
///
/// Business Logic（为什么需要这个函数）:
///     control/Tauri 出口统一序列化形态，避免重复解析逻辑。
///
/// Code Logic（这个函数做什么）:
///     path 字符串化 + 可选 createdAt。
pub fn pre_restore_infos_from_paths(paths: &[PathBuf]) -> Vec<PreRestoreBackupInfo> {
    paths
        .iter()
        .map(|p| PreRestoreBackupInfo {
            path: p.display().to_string(),
            created_at: parse_pre_restore_created_at(p),
        })
        .collect()
}

/// 备份恢复服务（sidecar owner）。
pub struct BackupRestoreService {
    state: AppState,
    job_repo: RecoveryJobRepo,
    limits: ArchiveLimits,
}

impl BackupRestoreService {
    /// 构造。
    pub fn new(state: AppState) -> Self {
        let gate = state
            .maintenance_gate
            .clone();
        let job_repo = RecoveryJobRepo::new(state.db.clone(), gate);
        Self {
            state,
            job_repo,
            limits: ArchiveLimits::default(),
        }
    }

    /// 只读 inspect（不改 DB）。
    ///
    /// Business Logic: 预览领域计数/警告；确认前零写入。
    /// Code Logic: inspect_archive_streaming + 粗算条数。
    pub fn inspect(&self, archive_path: &Path) -> Result<InspectPreview, AppError> {
        let inspected = inspect_archive_streaming(archive_path, self.limits)?;
        let mut domain_counts = BTreeMap::new();
        for (domain, _) in &inspected.domain_counts {
            if domain == DOMAIN_CONFIG_REPORT {
                continue;
            }
            let count = count_items_in_archive(archive_path, domain, self.limits).unwrap_or(0);
            domain_counts.insert(domain.clone(), count);
        }
        Ok(InspectPreview {
            format_version: inspected.manifest.format_version,
            domain_counts,
            warnings: inspected.warnings,
            conflicts_estimate: 0,
        })
    }

    /// 执行恢复（exclusive lease 全程）。
    ///
    /// Business Logic: 从 pre-restore 到 commit 独占，失败回滚事务；config 永不写回。
    /// Code Logic: job preparing → exclusive → backup → applying → 单事务 → succeeded/failed。
    pub async fn restore(&self, request: RestoreRequest) -> Result<RestoreResult, AppError> {
        let archive_path = PathBuf::from(&request.archive_path);
        // 预览校验（零写入）
        let _preview = self.inspect(&archive_path)?;

        let domains: Vec<String> = request
            .domains
            .into_iter()
            .filter(|d| d != DOMAIN_CONFIG_REPORT)
            .collect();
        if domains.is_empty() {
            return Err(AppError::generic("请至少选择一个可恢复领域"));
        }

        let now = chrono::Utc::now().to_rfc3339();
        let job_id = uuid::Uuid::new_v4().to_string();
        let domains_json = serde_json::to_string(&domains)?;
        let mode_str = match request.mode {
            RestoreMode::Merge => "merge",
            RestoreMode::ReplaceDomain => "replaceDomain",
        };
        self.job_repo
            .insert_preparing(
                &job_id,
                Some(archive_path.to_str().unwrap_or("")),
                &domains_json,
                mode_str,
                &now,
            )
            .await?;

        // exclusive from pre-backup through commit
        let exclusive = self.state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);

        let backup_dir = pre_restore_dir()?;
        let backup_result = create_pre_restore_backup(&self.state, &backup_dir).await;
        let pre_path = match backup_result {
            Ok(p) => p,
            Err(e) => {
                let t = chrono::Utc::now().to_rfc3339();
                let _ = self
                    .job_repo
                    .update_status_with_permit(
                        &permit,
                        &job_id,
                        RecoveryJobStatus::Failed,
                        None,
                        Some(&format!("pre-restore 备份失败: {e}")),
                        &t,
                    )
                    .await;
                drop(permit);
                drop(exclusive);
                return Err(e);
            }
        };

        let t = chrono::Utc::now().to_rfc3339();
        self.job_repo
            .update_status_with_permit(
                &permit,
                &job_id,
                RecoveryJobStatus::Applying,
                Some(pre_path.to_str().unwrap_or("")),
                None,
                &t,
            )
            .await?;

        let apply = self
            .apply_domains_in_transaction(&permit, &archive_path, &domains, request.mode)
            .await;

        let t2 = chrono::Utc::now().to_rfc3339();
        match apply {
            Ok(()) => {
                self.job_repo
                    .update_status_with_permit(
                        &permit,
                        &job_id,
                        RecoveryJobStatus::Succeeded,
                        Some(pre_path.to_str().unwrap_or("")),
                        None,
                        &t2,
                    )
                    .await?;
                // 新备份完整后清理旧备份
                let _ = prune_pre_restore_backups(&backup_dir);
                drop(permit);
                drop(exclusive);
                Ok(RestoreResult {
                    job_id,
                    status: RecoveryJobStatus::Succeeded,
                    applied_domains: domains,
                    pre_restore_backup_path: Some(pre_path.display().to_string()),
                    error_summary: None,
                })
            }
            Err(e) => {
                let msg = format!("{e}");
                let _ = self
                    .job_repo
                    .update_status_with_permit(
                        &permit,
                        &job_id,
                        RecoveryJobStatus::Failed,
                        Some(pre_path.to_str().unwrap_or("")),
                        Some(&msg),
                        &t2,
                    )
                    .await;
                drop(permit);
                drop(exclusive);
                Err(e)
            }
        }
    }

    async fn apply_domains_in_transaction(
        &self,
        permit: &DatabaseWritePermit,
        archive_path: &Path,
        domains: &[String],
        mode: RestoreMode,
    ) -> Result<(), AppError> {
        // 先把各领域数据读入内存（已 inspect 过）
        let mut prompts: Option<Vec<PromptRow>> = None;
        let mut cc_history: Option<Vec<ClaudeHistoryRow>> = None;
        let mut scratchpad: Option<Vec<ScratchpadRow>> = None;
        let mut ssh: Option<Vec<SshTargetRow>> = None;
        let mut claude_md: Option<Option<ClaudeMdRow>> = None;
        let mut floors: Option<Vec<DeletionFloor>> = None;

        for d in domains {
            match d.as_str() {
                DOMAIN_PROMPTS => {
                    let bytes =
                        read_entry_bytes(archive_path, "prompts/items.json", self.limits)?;
                    prompts = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CC_HISTORY => {
                    let bytes =
                        read_entry_bytes(archive_path, "ccHistory/items.json", self.limits)?;
                    cc_history = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_SCRATCHPAD => {
                    let bytes =
                        read_entry_bytes(archive_path, "scratchpad/items.json", self.limits)?;
                    scratchpad = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_SSH_TARGETS => {
                    let bytes =
                        read_entry_bytes(archive_path, "sshTargets/items.json", self.limits)?;
                    ssh = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CLAUDE_MD => {
                    let bytes =
                        read_entry_bytes(archive_path, "claudeMd/item.json", self.limits)?;
                    claude_md = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_DELETION_FLOORS => {
                    let bytes =
                        read_entry_bytes(archive_path, "deletionFloors/items.json", self.limits)?;
                    floors = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CONFIG_REPORT => {
                    // 明确忽略
                }
                other => {
                    return Err(AppError::generic(format!("未知领域: {other}")));
                }
            }
        }

        let mut tx = begin_write_with_permit(&self.state.db, permit).await?;

        if matches!(mode, RestoreMode::ReplaceDomain) {
            if prompts.is_some() {
                sqlx::query("DELETE FROM prompts")
                    .execute(&mut *tx)
                    .await?;
            }
            if cc_history.is_some() {
                sqlx::query("DELETE FROM claude_history")
                    .execute(&mut *tx)
                    .await?;
            }
            if scratchpad.is_some() {
                sqlx::query("DELETE FROM scratchpad")
                    .execute(&mut *tx)
                    .await?;
            }
            if ssh.is_some() {
                sqlx::query("DELETE FROM ssh_targets")
                    .execute(&mut *tx)
                    .await?;
            }
            if claude_md.is_some() {
                sqlx::query("DELETE FROM claude_md")
                    .execute(&mut *tx)
                    .await?;
            }
            if floors.is_some() {
                sqlx::query("DELETE FROM sync_deletion_floors")
                    .execute(&mut *tx)
                    .await?;
            }
        }

        // merge / replace 均用 bulk_upsert_on_tx 语义（replace 已清空）
        if let Some(items) = prompts {
            crate::storage::PromptRepo::bulk_upsert_on_tx(&mut tx, &items, None).await?;
        }
        if let Some(items) = cc_history {
            // cc_history 用事务内 REPLACE 循环
            for item in &items {
                let vc = serde_json::to_string(&item.vector_clock)?;
                sqlx::query(
                    "INSERT OR REPLACE INTO claude_history
                     (id, project_path, project_name, session_id, content, git_branch, cc_version,
                      occurred_at, device_id, vector_clock, created_at, updated_at, deleted)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&item.id)
                .bind(&item.project_path)
                .bind(&item.project_name)
                .bind(&item.session_id)
                .bind(&item.content)
                .bind(&item.git_branch)
                .bind(&item.cc_version)
                .bind(&item.occurred_at)
                .bind(&item.device_id)
                .bind(vc)
                .bind(&item.created_at)
                .bind(&item.updated_at)
                .bind(item.deleted as i64)
                .execute(&mut *tx)
                .await?;
            }
        }
        if let Some(items) = scratchpad {
            crate::storage::ScratchpadRepo::bulk_upsert_on_tx(&mut tx, &items, None).await?;
        }
        if let Some(items) = ssh {
            crate::storage::SshTargetRepo::bulk_upsert_on_tx(&mut tx, &items, None).await?;
        }
        if let Some(Some(row)) = claude_md {
            let vc = serde_json::to_string(&row.vector_clock)?;
            sqlx::query(
                "INSERT OR REPLACE INTO claude_md (id, content, updated_at, device_id, vector_clock)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&row.id)
            .bind(&row.content)
            .bind(&row.updated_at)
            .bind(&row.device_id)
            .bind(vc)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(items) = floors {
            for floor in &items {
                let vc = serde_json::to_string(&floor.delete_vector_clock)?;
                sqlx::query(
                    "INSERT OR REPLACE INTO sync_deletion_floors
                     (domain, item_id, delete_vector_clock, delete_epoch, content_hash, created_at)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&floor.domain)
                .bind(&floor.item_id)
                .bind(vc)
                .bind(floor.delete_epoch as i64)
                .bind(&floor.content_hash)
                .bind(&floor.created_at)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    /// 列出 recovery jobs。
    pub async fn list_jobs(&self, limit: i64) -> Result<Vec<RecoveryJobRow>, AppError> {
        self.job_repo.list_recent(limit).await
    }

    /// 一键回退：用 pre-restore 备份文件回灌（exclusive）。
    ///
    /// Business Logic: 仅 succeeded/failed 且有 pre_restore 路径时可回退。
    /// Code Logic: 对 pre-restore zip 走 restore(replace all domains)。
    pub async fn rollback_job(&self, job_id: &str) -> Result<RestoreResult, AppError> {
        let job = self
            .job_repo
            .get(job_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("recovery job 不存在: {job_id}")))?;
        let path = job
            .pre_restore_backup_path
            .ok_or_else(|| AppError::generic("该任务无可回退备份"))?;
        let result = self
            .restore(RestoreRequest {
                archive_path: path,
                mode: RestoreMode::ReplaceDomain,
                domains: vec![
                    DOMAIN_PROMPTS.into(),
                    DOMAIN_CC_HISTORY.into(),
                    DOMAIN_SCRATCHPAD.into(),
                    DOMAIN_SSH_TARGETS.into(),
                    DOMAIN_CLAUDE_MD.into(),
                    DOMAIN_DELETION_FLOORS.into(),
                ],
            })
            .await?;
        let t = chrono::Utc::now().to_rfc3339();
        let _ = self
            .job_repo
            .update_status(job_id, RecoveryJobStatus::RolledBack, None, None, &t)
            .await;
        Ok(result)
    }
}

fn count_items_in_archive(
    path: &Path,
    domain: &str,
    limits: ArchiveLimits,
) -> Result<u32, AppError> {
    let entry = match domain {
        DOMAIN_PROMPTS => "prompts/items.json",
        DOMAIN_CC_HISTORY => "ccHistory/items.json",
        DOMAIN_SCRATCHPAD => "scratchpad/items.json",
        DOMAIN_SSH_TARGETS => "sshTargets/items.json",
        DOMAIN_CLAUDE_MD => "claudeMd/item.json",
        DOMAIN_DELETION_FLOORS => "deletionFloors/items.json",
        _ => return Ok(0),
    };
    let bytes = read_entry_bytes(path, entry, limits)?;
    if domain == DOMAIN_CLAUDE_MD {
        let v: serde_json::Value = serde_json::from_slice(&bytes)?;
        return Ok(if v.is_null() { 0 } else { 1 });
    }
    let arr: Vec<serde_json::Value> = serde_json::from_slice(&bytes)?;
    Ok(arr.len() as u32)
}

/// pre-restore 备份目录：`<data_dir>/recovery-backups`。
pub fn pre_restore_dir() -> Result<PathBuf, AppError> {
    let root = crate::config::data_dir()?;
    let dir = root.join("recovery-backups");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// 创建用户私有 pre-restore 备份（完整导出 ZIP）。
///
/// Business Logic: 恢复前自动备份，完整落盘后才进入可回退列表。
/// Code Logic: create_export_archive 到带时间戳文件名。
pub async fn create_pre_restore_backup(
    state: &AppState,
    dir: &Path,
) -> Result<PathBuf, AppError> {
    let name = format!(
        "pre-restore-{}.zip",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    let dest = dir.join(&name);
    let tmp = dir.join(format!("{name}.partial"));
    crate::backup::archive::create_export_archive(state, &tmp).await?;
    fs::rename(&tmp, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o600));
    }
    Ok(dest)
}

/// 列出 pre-restore 备份（新→旧）。
pub fn list_pre_restore_backups(dir: &Path) -> Result<Vec<PathBuf>, AppError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("zip")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| n.starts_with("pre-restore-"))
                    .unwrap_or(false)
        })
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    Ok(files)
}

/// 保留 7 天且最多 3 份；只删已完整的旧文件。
///
/// Business Logic: 新备份原子完成后才清理。
/// Code Logic: 按 mtime/名排序，超龄或超数量 unlink。
pub fn prune_pre_restore_backups(dir: &Path) -> Result<(), AppError> {
    let files = list_pre_restore_backups(dir)?;
    let now = SystemTime::now();
    let mut kept = 0usize;
    for path in files {
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let aged_out = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map(|d| d > PRE_RESTORE_RETENTION)
            .unwrap_or(false);
        if aged_out || kept >= PRE_RESTORE_MAX_COUNT {
            let _ = fs::remove_file(&path);
        } else {
            kept += 1;
        }
    }
    Ok(())
}

/// 从 pre-restore zip 回灌（测试/服务辅助）。
pub async fn rollback_from_pre_restore_backup(
    state: &AppState,
    backup_path: &Path,
) -> Result<RestoreResult, AppError> {
    BackupRestoreService::new(state.clone())
        .restore(RestoreRequest {
            archive_path: backup_path.display().to_string(),
            mode: RestoreMode::ReplaceDomain,
            domains: vec![
                DOMAIN_PROMPTS.into(),
                DOMAIN_CC_HISTORY.into(),
                DOMAIN_SCRATCHPAD.into(),
                DOMAIN_SSH_TARGETS.into(),
                DOMAIN_CLAUDE_MD.into(),
                DOMAIN_DELETION_FLOORS.into(),
            ],
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::archive::{write_test_archive, ArchiveManifest, FORMAT_VERSION};
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    #[test]
    fn prune_keeps_at_most_three() {
        let dir = tempdir().unwrap();
        for i in 0..5 {
            let p = dir
                .path()
                .join(format!("pre-restore-2026010{i}T000000Z.zip"));
            File::create(&p).unwrap();
        }
        prune_pre_restore_backups(dir.path()).unwrap();
        let left = list_pre_restore_backups(dir.path()).unwrap();
        assert!(left.len() <= PRE_RESTORE_MAX_COUNT);
    }

    #[test]
    fn checksum_mismatch_changes_nothing_harness_shape() {
        // 与 brief 对齐：篡改包 inspect 失败，调用方不得进入 apply
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.zip");
        let mut m = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "d".into(),
            domains: vec![DOMAIN_PROMPTS.into()],
            files: BTreeMap::new(),
        };
        m.files
            .insert("prompts/items.json".into(), crate::backup::archive::sha256_hex(b"[]"));
        {
            let f = File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let options =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            let mbytes = serde_json::to_vec_pretty(&m).unwrap();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(&mbytes).unwrap();
            zip.start_file("prompts/items.json", options).unwrap();
            zip.write_all(b"[1]").unwrap();
            zip.finish().unwrap();
        }
        assert!(inspect_archive_streaming(&path, ArchiveLimits::default()).is_err());
    }

    #[tokio::test]
    async fn exclusive_blocks_shared_during_restore_window() {
        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let exclusive = gate.acquire_exclusive().await;
        assert!(gate.try_acquire_shared().is_none());
        drop(exclusive);
    }
}
