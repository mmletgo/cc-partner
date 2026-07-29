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
    inspect_archive_streaming, read_entry_bytes, read_entry_bytes_verified, ArchiveLimits,
    DOMAIN_CC_HISTORY, DOMAIN_CLAUDE_MD, DOMAIN_CONFIG_REPORT, DOMAIN_CONTENT_VERSIONS,
    DOMAIN_DELETION_FLOORS, DOMAIN_PROMPTS, DOMAIN_SCRATCHPAD, DOMAIN_SSH_TARGETS,
};
use crate::cc::models::ClaudeHistoryRow;
use crate::error::AppError;
use crate::models::claude_md::ClaudeMdRow;
use crate::models::prompt::PromptRow;
use crate::models::scratchpad::ScratchpadRow;
use crate::models::ssh_target::SshTargetRow;
use crate::state::AppState;
use crate::storage::content_version_repo::ContentVersion;
use crate::storage::deletion_floor_repo::{
    DeletionFloor, DeletionFloorDecision, DeletionFloorRepo,
};
use crate::storage::maintenance_gate::{
    begin_write_with_permit, DatabaseMaintenanceGate, DatabaseWritePermit,
};
use crate::storage::recovery_job_repo::{RecoveryJobRepo, RecoveryJobRow, RecoveryJobStatus};
use crate::storage::sync_request_ledger_repo::{
    DOMAIN_PROMPTS as FLOOR_DOMAIN_PROMPTS, DOMAIN_SCRATCHPAD as FLOOR_DOMAIN_SCRATCHPAD,
    DOMAIN_SSH_TARGET as FLOOR_DOMAIN_SSH_TARGET,
};
use crate::sync::apply_merge::{
    build_prompt_merge_plan_on_tx, build_scratchpad_merge_plan_on_tx, build_ssh_merge_plan_on_tx,
    write_prompt_merge_on_tx, write_scratchpad_merge_on_tx, write_ssh_merge_on_tx,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
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
        let gate = state.maintenance_gate.clone();
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
        for domain in inspected.domain_counts.keys() {
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
    /// Code Logic: reclaim stuck → job preparing → exclusive → backup → applying → 单事务 → succeeded/failed。
    pub async fn restore(&self, request: RestoreRequest) -> Result<RestoreResult, AppError> {
        // 先诚实回收上次崩溃留下的 Applying 任务，禁止伪装成功。
        let _ = self.reclaim_stuck_applying_jobs().await;

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

    /// 在 exclusive 事务内导入选中领域。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Merge 必须走与 live sync 相同的向量时钟/conflict-copy；Replace 清空领域后 bulk 导入；
    ///     floors 在 Merge 下与本地单调合并（禁止旧备份降级新本地下限），Replace 可整域替换；
    ///     生效 floors 恢复后要对 live 行再应用 floor 决策；content_versions 必须可往返。
    ///
    /// Code Logic（这个函数做什么）:
    ///     inspect 取 manifest 哈希 → 再验 SHA 读 entry → 预构建 merge plan → 单事务写入；
    ///     floors：Merge 走 `upsert_merge_monotonic_on_tx`，Replace 走 `upsert_on_tx`。
    async fn apply_domains_in_transaction(
        &self,
        permit: &DatabaseWritePermit,
        archive_path: &Path,
        domains: &[String],
        mode: RestoreMode,
    ) -> Result<(), AppError> {
        // apply 前再 inspect，并在每次读 entry 时对照 manifest 哈希（防 TOCTOU 篡改）。
        let inspected = inspect_archive_streaming(archive_path, self.limits)?;
        let hashes = &inspected.manifest.files;

        crate::storage::ContentVersionRepo::ensure_schema(&self.state.db).await?;
        crate::storage::DeletionFloorRepo::ensure_schema(&self.state.db).await?;
        crate::storage::SyncDeleteSequenceRepo::ensure_schema(&self.state.db).await?;
        crate::storage::ensure_domain_delete_epoch_columns(&self.state.db).await?;

        let read_verified = |entry: &str| -> Result<Vec<u8>, AppError> {
            let expected = hashes.get(entry).ok_or_else(|| {
                AppError::generic(format!("manifest 未声明 entry，拒绝读取: {entry}"))
            })?;
            read_entry_bytes_verified(archive_path, entry, expected, self.limits)
        };

        // 先把各领域数据读入内存（已 re-hash 校验）
        let mut prompts: Option<Vec<PromptRow>> = None;
        let mut cc_history: Option<Vec<ClaudeHistoryRow>> = None;
        let mut scratchpad: Option<Vec<ScratchpadRow>> = None;
        let mut ssh: Option<Vec<SshTargetRow>> = None;
        let mut claude_md: Option<Option<ClaudeMdRow>> = None;
        let mut floors: Option<Vec<DeletionFloor>> = None;
        let mut content_versions: Option<Vec<ContentVersion>> = None;

        for d in domains {
            match d.as_str() {
                DOMAIN_PROMPTS => {
                    let bytes = read_verified("prompts/items.json")?;
                    prompts = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CC_HISTORY => {
                    let bytes = read_verified("ccHistory/items.json")?;
                    cc_history = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_SCRATCHPAD => {
                    let bytes = read_verified("scratchpad/items.json")?;
                    scratchpad = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_SSH_TARGETS => {
                    let bytes = read_verified("sshTargets/items.json")?;
                    ssh = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CLAUDE_MD => {
                    let bytes = read_verified("claudeMd/item.json")?;
                    claude_md = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_DELETION_FLOORS => {
                    let bytes = read_verified("deletionFloors/items.json")?;
                    floors = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CONTENT_VERSIONS => {
                    let bytes = read_verified("contentVersions/items.json")?;
                    content_versions = Some(serde_json::from_slice(&bytes)?);
                }
                DOMAIN_CONFIG_REPORT => {
                    // 明确忽略
                }
                other => {
                    return Err(AppError::generic(format!("未知领域: {other}")));
                }
            }
        }

        // 包内若含 content_versions 且未显式勾选，仍导入（避免 silent loss）。
        if content_versions.is_none() && hashes.contains_key("contentVersions/items.json") {
            let bytes = read_verified("contentVersions/items.json")?;
            content_versions = Some(serde_json::from_slice(&bytes)?);
        }

        // Prompt/SSH/Scratchpad plan 与 write 共用同一事务快照（*_on_tx）。
        let now = chrono::Utc::now().to_rfc3339();

        let mut tx = begin_write_with_permit(&self.state.db, permit).await?;

        if matches!(mode, RestoreMode::ReplaceDomain) {
            if prompts.is_some() {
                sqlx::query("DELETE FROM prompts").execute(&mut *tx).await?;
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
            // 显式替换 contentVersions 领域时清空；仅随包自动导入则幂等 INSERT。
            if domains.iter().any(|d| d == DOMAIN_CONTENT_VERSIONS) {
                sqlx::query("DELETE FROM content_versions")
                    .execute(&mut *tx)
                    .await?;
            }
        }

        // prompts：Merge 在同一事务内 plan+write；Replace 清空后 bulk_upsert
        if matches!(mode, RestoreMode::Merge) {
            if let Some(ref items) = prompts {
                let plan = build_prompt_merge_plan_on_tx(&mut tx, items, &now).await?;
                write_prompt_merge_on_tx(&mut tx, &plan).await?;
            }
        } else if let Some(items) = prompts {
            crate::storage::PromptRepo::bulk_upsert_on_tx(&mut tx, &items, None).await?;
        }
        if let Some(items) = cc_history {
            // cc_history 无 conflict-copy 体系，始终 REPLACE 风格
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
        // scratchpad：Merge 在同一事务内 plan+write；Replace 清空后 bulk_upsert
        if matches!(mode, RestoreMode::Merge) {
            if let Some(ref items) = scratchpad {
                let plan = build_scratchpad_merge_plan_on_tx(&mut tx, items, &now).await?;
                write_scratchpad_merge_on_tx(&mut tx, &plan).await?;
            }
        } else if let Some(items) = scratchpad {
            crate::storage::ScratchpadRepo::bulk_upsert_on_tx(&mut tx, &items, None).await?;
        }
        // ssh：Merge 在同一事务内 plan+write；Replace 清空后 bulk_upsert
        if matches!(mode, RestoreMode::Merge) {
            if let Some(ref items) = ssh {
                let plan = build_ssh_merge_plan_on_tx(&mut tx, items, &now).await?;
                write_ssh_merge_on_tx(&mut tx, &plan).await?;
            }
        } else if let Some(items) = ssh {
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
            // Merge：与本地 floor 单调合并，禁止旧备份 floor 降级新本地下限；
            // ReplaceDomain：领域已清空，直接 REPLACE 写回备份 floor。
            let mut effective_floors: Vec<DeletionFloor> = Vec::with_capacity(items.len());
            for floor in &items {
                let effective = if matches!(mode, RestoreMode::Merge) {
                    DeletionFloorRepo::upsert_merge_monotonic_on_tx(&mut tx, floor).await?
                } else {
                    DeletionFloorRepo::upsert_on_tx(&mut tx, floor).await?;
                    floor.clone()
                };
                effective_floors.push(effective);
            }
            // M7: 以最终生效 floor 再应用 DeleteWins / KeepHistoryButDeleted 到 live。
            reapply_floors_to_live_on_tx(&mut tx, &effective_floors).await?;
        }
        if let Some(versions) = content_versions {
            for version in &versions {
                crate::storage::ContentVersionRepo::insert_idempotent_on_tx(&mut tx, version)
                    .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    /// 列出 recovery jobs（不在列表路径回收）。
    ///
    /// Business Logic: 列表查询与正在 Applying 的恢复并发时不得把活任务误判为崩溃残留。
    /// Code Logic: 仅 `list_recent`；残留回收只在 `reclaim_on_startup`。
    pub async fn list_jobs(&self, limit: i64) -> Result<Vec<RecoveryJobRow>, AppError> {
        self.job_repo.list_recent(limit).await
    }

    /// 将崩溃遗留的 `Preparing`/`Applying` 任务诚实标记为 Failed。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     进程在 insert_preparing 之后、Applying 之前，或 apply 中途崩溃后，job 可能永远停在
    ///     Preparing/Applying；不得伪装成功，应提示可回退/可重试。
    ///
    /// Code Logic（这个函数做什么）:
    ///     扫描 recent jobs；Preparing 或 Applying → Failed，错误摘要依据状态与 pre-restore 路径。
    pub async fn reclaim_stuck_applying_jobs(&self) -> Result<usize, AppError> {
        let jobs = self.job_repo.list_recent(200).await?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut reclaimed = 0usize;
        for job in jobs {
            let msg = match job.status {
                RecoveryJobStatus::Applying => {
                    if job.pre_restore_backup_path.is_some() {
                        "进程中断：恢复应用阶段崩溃；可从 pre-restore 备份回退"
                    } else {
                        "进程中断：恢复应用阶段崩溃（无 pre-restore 备份路径）"
                    }
                }
                RecoveryJobStatus::Preparing => {
                    "进程中断：恢复准备阶段崩溃（insert_preparing 后未进入 Applying）"
                }
                _ => continue,
            };
            self.job_repo
                .update_status(&job.id, RecoveryJobStatus::Failed, None, Some(msg), &now)
                .await?;
            reclaimed += 1;
        }
        Ok(reclaimed)
    }

    /// 启动时回收卡住任务（Headless 后台入口）。
    ///
    /// Business Logic: 后端启动后不应遗留永久 Preparing/Applying。
    /// Code Logic: 委托 `reclaim_stuck_applying_jobs`。
    pub async fn reclaim_on_startup(&self) -> Result<usize, AppError> {
        self.reclaim_stuck_applying_jobs().await
    }

    /// 一键回退：用 pre-restore 备份文件回灌（exclusive）。
    ///
    /// Business Logic: 仅 succeeded/failed 且有 pre_restore 路径时可回退。
    /// Code Logic: 对 pre-restore zip 走 restore(replace all domains)；状态更新错误上抛。
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
                    DOMAIN_CONTENT_VERSIONS.into(),
                ],
            })
            .await?;
        let t = chrono::Utc::now().to_rfc3339();
        self.job_repo
            .update_status(job_id, RecoveryJobStatus::RolledBack, None, None, &t)
            .await?;
        Ok(result)
    }
}

/// 在同一恢复事务内把 floors 决策再应用到 live 领域行。
///
/// Business Logic（为什么需要这个函数）:
///     仅导入 floor 表而不改 live 行，会让已压缩删除的条目继续以 live 展示/同步复活。
///
/// Code Logic（这个函数做什么）:
///     对每条 floor 读 prompts/ssh_targets/scratchpad live 行；
///     `apply_deletion_floor` 为 DeleteWins / KeepHistoryButDeleted 时强制 `deleted=1`。
async fn reapply_floors_to_live_on_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    floors: &[DeletionFloor],
) -> Result<(), AppError> {
    for floor in floors {
        match floor.domain.as_str() {
            FLOOR_DOMAIN_PROMPTS => {
                let row = sqlx::query("SELECT id, vector_clock, deleted FROM prompts WHERE id = ?")
                    .bind(&floor.item_id)
                    .fetch_optional(&mut **tx)
                    .await?;
                if let Some(r) = row {
                    use sqlx::Row;
                    let deleted: i64 = r.try_get("deleted")?;
                    if deleted != 0 {
                        continue;
                    }
                    let vc_text: String = r.try_get("vector_clock")?;
                    let vc: std::collections::HashMap<String, u64> =
                        serde_json::from_str(&vc_text).unwrap_or_default();
                    match DeletionFloorRepo::apply_deletion_floor(floor, &vc) {
                        DeletionFloorDecision::DeleteWins
                        | DeletionFloorDecision::KeepHistoryButDeleted => {
                            sqlx::query("UPDATE prompts SET deleted = 1 WHERE id = ?")
                                .bind(&floor.item_id)
                                .execute(&mut **tx)
                                .await?;
                        }
                        DeletionFloorDecision::AcceptLive => {}
                    }
                }
            }
            FLOOR_DOMAIN_SSH_TARGET => {
                let row = sqlx::query(
                    "SELECT host, vector_clock, deleted FROM ssh_targets WHERE host = ?",
                )
                .bind(&floor.item_id)
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(r) = row {
                    use sqlx::Row;
                    let deleted: i64 = r.try_get("deleted")?;
                    if deleted != 0 {
                        continue;
                    }
                    let vc_text: String = r.try_get("vector_clock")?;
                    let vc: std::collections::HashMap<String, u64> =
                        serde_json::from_str(&vc_text).unwrap_or_default();
                    match DeletionFloorRepo::apply_deletion_floor(floor, &vc) {
                        DeletionFloorDecision::DeleteWins
                        | DeletionFloorDecision::KeepHistoryButDeleted => {
                            sqlx::query("UPDATE ssh_targets SET deleted = 1 WHERE host = ?")
                                .bind(&floor.item_id)
                                .execute(&mut **tx)
                                .await?;
                        }
                        DeletionFloorDecision::AcceptLive => {}
                    }
                }
            }
            FLOOR_DOMAIN_SCRATCHPAD => {
                let row =
                    sqlx::query("SELECT id, vector_clock, deleted FROM scratchpad WHERE id = ?")
                        .bind(&floor.item_id)
                        .fetch_optional(&mut **tx)
                        .await?;
                if let Some(r) = row {
                    use sqlx::Row;
                    let deleted: i64 = r.try_get("deleted")?;
                    if deleted != 0 {
                        continue;
                    }
                    let vc_text: String = r.try_get("vector_clock")?;
                    let vc: std::collections::HashMap<String, u64> =
                        serde_json::from_str(&vc_text).unwrap_or_default();
                    match DeletionFloorRepo::apply_deletion_floor(floor, &vc) {
                        DeletionFloorDecision::DeleteWins
                        | DeletionFloorDecision::KeepHistoryButDeleted => {
                            sqlx::query("UPDATE scratchpad SET deleted = 1 WHERE id = ?")
                                .bind(&floor.item_id)
                                .execute(&mut **tx)
                                .await?;
                        }
                        DeletionFloorDecision::AcceptLive => {}
                    }
                }
            }
            _ => {
                // 未知 floor domain 跳过，不中断恢复
            }
        }
    }
    Ok(())
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
        DOMAIN_CONTENT_VERSIONS => "contentVersions/items.json",
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
pub async fn create_pre_restore_backup(state: &AppState, dir: &Path) -> Result<PathBuf, AppError> {
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
                DOMAIN_CONTENT_VERSIONS.into(),
            ],
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::archive::{sha256_hex, write_test_archive, ArchiveManifest, FORMAT_VERSION};
    use crate::storage::content_version_repo::{ContentVersion, ContentVersionRepo, KIND_CONFLICT};
    use crate::storage::deletion_floor_repo::DeletionFloorRepo;
    use crate::storage::maintenance_gate::DatabaseMaintenanceGate;
    use crate::storage::PromptRepo;
    use crate::sync::apply_merge::{
        apply_fail_test_lock, arm_apply_merge_fail_point, clear_apply_merge_fail_point,
        ApplyMergeFailPoint,
    };
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::{BTreeMap, HashMap};
    use std::fs::File;
    use std::io::Write;
    use std::str::FromStr;
    use std::sync::Arc;
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
        m.files.insert(
            "prompts/items.json".into(),
            crate::backup::archive::sha256_hex(b"[]"),
        );
        {
            let f = File::create(&path).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let options = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
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

    /// 构建最小 restore 测试用 AppState（prompts + content_versions + floors + recovery_jobs）。
    async fn setup_restore_state() -> (AppState, tempfile::TempDir) {
        use crate::backend::ui::HeadlessBackendUi;
        use crate::config::{
            AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
        };
        use crate::net::peer_client::PeerClient;
        use crate::orchestrator::repo::OrchestratorRepo;
        use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
        use crate::storage::{
            ClaudeHistoryRepo, ClaudeMdRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
            WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
            WorkbenchSessionRepo, WorkbenchWorktreeRepo,
        };
        use crate::transfer::registry::TransferRegistry;
        use std::sync::atomic::AtomicU16;
        use std::sync::{Mutex, RwLock};

        let tmp = tempdir().unwrap();
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS prompts (
                id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL,
                tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0,
                delete_epoch INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS ssh_targets (
                host TEXT PRIMARY KEY, port INTEGER NOT NULL, username TEXT NOT NULL,
                label TEXT, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0,
                delete_epoch INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS scratchpad (
                id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL,
                vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0,
                delete_epoch INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        ContentVersionRepo::ensure_schema(&pool).await.unwrap();
        DeletionFloorRepo::ensure_schema(&pool).await.unwrap();
        crate::storage::SyncDeleteSequenceRepo::ensure_schema(&pool)
            .await
            .unwrap();
        RecoveryJobRepo::ensure_schema(&pool).await.unwrap();

        let gate = Arc::new(DatabaseMaintenanceGate::new());
        let config = AppConfig {
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 0,
            receive_dir: tmp.path().join("recv").to_string_lossy().to_string(),
            db_path: tmp.path().join("data.db").to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
        };
        let store = Arc::new(crate::config_store::MemoryConfigStore::with_config(
            config.clone(),
        ));
        let config_runtime = Arc::new(crate::config_runtime::ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();
        let state = AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: gate.clone(),
            prompt_repo: Arc::new(PromptRepo::with_gate(pool.clone(), gate.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::with_gate(pool.clone(), gate.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::with_gate(pool.clone(), gate.clone())),
            device_id: Arc::new("device-test".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(tmp.path().join("dist"))),
            update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_workspace_layout_repo: Arc::new(
                crate::storage::WorkbenchWorkspaceLayoutRepo::new(pool.clone()),
            ),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::env::temp_dir().join("cc-partner-bv-test"),
                    "test-owner".into(),
                )
                .expect("browser verification test service"),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: std::sync::Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
            workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(std::sync::Mutex::new(
                HashMap::new(),
            )),
            runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
            runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
            event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
                "test-owner",
            )),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        };
        (state, tmp)
    }

    fn sample_prompt(
        id: &str,
        device: &str,
        content: &str,
        vc: u64,
        updated_at: &str,
    ) -> PromptRow {
        let mut vector_clock = HashMap::new();
        vector_clock.insert(device.to_string(), vc);
        PromptRow {
            id: id.to_string(),
            title: format!("t-{device}"),
            content: content.to_string(),
            tags: vec![],
            created_at: "2024-01-01T00:00:00+00:00".to_string(),
            updated_at: updated_at.to_string(),
            device_id: device.to_string(),
            vector_clock,
            deleted: false,
            delete_epoch: 0,
        }
    }

    fn write_prompt_archive(path: &Path, prompts: &[PromptRow]) {
        let mut files = BTreeMap::new();
        files.insert(
            "prompts/items.json".to_string(),
            serde_json::to_vec_pretty(prompts).unwrap(),
        );
        let manifest = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "dev".into(),
            domains: vec![DOMAIN_PROMPTS.into()],
            files: BTreeMap::new(),
        };
        write_test_archive(path, &manifest, &files).unwrap();
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 与 fail-point 测试串行，避免全局注入污染
    async fn merge_restore_concurrent_keeps_conflict_copy() {
        // 全局 apply_merge fail point 与 mid_tx 测试共享；持锁确保无注入残留。
        let _lock = apply_fail_test_lock();
        clear_apply_merge_fail_point();
        let (state, tmp) = setup_restore_state().await;
        // local 较新（updated_at 更大）但与 archive 并发
        let local = sample_prompt("p1", "left", "local-body", 1, "2024-01-03T00:00:00+00:00");
        state
            .prompt_repo
            .bulk_upsert(std::slice::from_ref(&local))
            .await
            .unwrap();
        // archive remote：另一设备同 counter 不同正文，updated_at 更早 → local 胜，但应写 conflict
        let remote = sample_prompt("p1", "right", "remote-body", 1, "2024-01-02T00:00:00+00:00");
        let archive = tmp.path().join("merge.zip");
        write_prompt_archive(&archive, &[remote]);

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_PROMPTS.into()],
                RestoreMode::Merge,
            )
            .await
            .unwrap();
        drop(permit);
        drop(exclusive);

        let got = state.prompt_repo.get("p1").await.unwrap().unwrap();
        // local 时间更晚应保留 local body（LWW），且不得 silent REPLACE 丢 remote
        assert_eq!(got.content, "local-body");
        let versions = ContentVersionRepo::new(state.db.clone())
            .list_versions(FLOOR_DOMAIN_PROMPTS, "p1")
            .await
            .unwrap();
        assert!(
            !versions.is_empty(),
            "merge concurrent 必须保留 conflict copy"
        );
        assert_eq!(versions[0].kind, KIND_CONFLICT);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // intentional serial inject lock across await
    async fn merge_restore_mid_tx_fail_rolls_back() {
        let _lock = apply_fail_test_lock();
        let _fail = arm_apply_merge_fail_point(ApplyMergeFailPoint::AfterActiveRows);
        let (state, tmp) = setup_restore_state().await;
        let remote = sample_prompt("p1", "right", "remote-body", 1, "2024-01-02T00:00:00+00:00");
        let archive = tmp.path().join("fail.zip");
        write_prompt_archive(&archive, &[remote]);

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        let err = service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_PROMPTS.into()],
                RestoreMode::Merge,
            )
            .await
            .unwrap_err();
        drop(permit);
        drop(exclusive);
        clear_apply_merge_fail_point();
        assert!(format!("{err}").contains("injected") || format!("{err}").contains("fail"));
        assert!(state.prompt_repo.get("p1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn export_restore_preserves_content_version() {
        let (state, tmp) = setup_restore_state().await;
        let version = ContentVersion {
            id: "cv-1".into(),
            domain: FLOOR_DOMAIN_PROMPTS.into(),
            item_id: "p1".into(),
            source_device: "peer".into(),
            content_hash: "hash-abc".into(),
            created_at: "2024-01-01T00:00:00+00:00".into(),
            kind: KIND_CONFLICT.into(),
            snapshot_json: r#"{"id":"p1","content":"old"}"#.into(),
        };
        // 最小 archive：仅 contentVersions（不依赖全库 export 表）
        let mut files = BTreeMap::new();
        files.insert(
            "contentVersions/items.json".to_string(),
            serde_json::to_vec_pretty(std::slice::from_ref(&version)).unwrap(),
        );
        let archive = tmp.path().join("export.zip");
        let manifest = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "dev".into(),
            domains: vec![DOMAIN_CONTENT_VERSIONS.into()],
            files: BTreeMap::new(),
        };
        write_test_archive(&archive, &manifest, &files).unwrap();

        // 本地无 version → restore 后应出现
        assert!(ContentVersionRepo::new(state.db.clone())
            .list_all()
            .await
            .unwrap()
            .is_empty());

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_CONTENT_VERSIONS.into()],
                RestoreMode::ReplaceDomain,
            )
            .await
            .unwrap();
        drop(permit);
        drop(exclusive);

        let all = ContentVersionRepo::new(state.db.clone())
            .list_all()
            .await
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "cv-1");
        assert_eq!(all[0].content_hash, "hash-abc");
    }

    #[tokio::test]
    async fn rehash_mismatch_rejects_apply() {
        let (state, tmp) = setup_restore_state().await;
        let archive = tmp.path().join("bad-hash.zip");
        // 构造 manifest 哈希与内容不一致的包：inspect 失败；apply 不得写库
        let mut m = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "d".into(),
            domains: vec![DOMAIN_PROMPTS.into()],
            files: BTreeMap::new(),
        };
        m.files
            .insert("prompts/items.json".into(), sha256_hex(b"[]"));
        {
            let f = File::create(&archive).unwrap();
            let mut zip = zip::ZipWriter::new(f);
            let options = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            let mbytes = serde_json::to_vec_pretty(&m).unwrap();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(&mbytes).unwrap();
            zip.start_file("prompts/items.json", options).unwrap();
            zip.write_all(b"[{\"id\":\"x\"}]").unwrap();
            zip.finish().unwrap();
        }

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        let err = service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_PROMPTS.into()],
                RestoreMode::ReplaceDomain,
            )
            .await
            .unwrap_err();
        drop(permit);
        drop(exclusive);
        let msg = format!("{err}");
        assert!(
            msg.contains("校验和") || msg.contains("不匹配") || msg.contains("manifest"),
            "{msg}"
        );
        assert!(state.prompt_repo.get("x").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn reclaim_stuck_applying_marks_failed() {
        let (state, _tmp) = setup_restore_state().await;
        let service = BackupRestoreService::new(state.clone());
        service
            .job_repo
            .insert_preparing("stuck-1", Some("/a.zip"), "[\"prompts\"]", "merge", "t0")
            .await
            .unwrap();
        service
            .job_repo
            .update_status(
                "stuck-1",
                RecoveryJobStatus::Applying,
                Some("/pre.zip"),
                None,
                "t1",
            )
            .await
            .unwrap();
        let n = service.reclaim_stuck_applying_jobs().await.unwrap();
        assert_eq!(n, 1);
        let job = service.job_repo.get("stuck-1").await.unwrap().unwrap();
        assert_eq!(job.status, RecoveryJobStatus::Failed);
        assert!(
            job.error_summary
                .as_deref()
                .unwrap_or("")
                .contains("进程中断"),
            "{:?}",
            job.error_summary
        );
        assert_eq!(job.pre_restore_backup_path.as_deref(), Some("/pre.zip"));
    }

    #[tokio::test]
    async fn reclaim_stuck_preparing_marks_failed() {
        let (state, _tmp) = setup_restore_state().await;
        let service = BackupRestoreService::new(state.clone());
        service
            .job_repo
            .insert_preparing("stuck-prep", Some("/a.zip"), "[\"prompts\"]", "merge", "t0")
            .await
            .unwrap();
        let n = service.reclaim_stuck_applying_jobs().await.unwrap();
        assert_eq!(n, 1);
        let job = service.job_repo.get("stuck-prep").await.unwrap().unwrap();
        assert_eq!(job.status, RecoveryJobStatus::Failed);
        assert!(
            job.error_summary
                .as_deref()
                .unwrap_or("")
                .contains("准备阶段"),
            "{:?}",
            job.error_summary
        );
    }

    #[tokio::test]
    async fn floor_reapply_marks_live_deleted() {
        let (state, tmp) = setup_restore_state().await;
        // live prompt 仍未删除
        let live = sample_prompt("p-floor", "devA", "alive", 1, "2024-01-01T00:00:00+00:00");
        state
            .prompt_repo
            .bulk_upsert(std::slice::from_ref(&live))
            .await
            .unwrap();

        // floor 支配 live（Equal/Before）→ DeleteWins
        let mut delete_vc = HashMap::new();
        delete_vc.insert("devA".to_string(), 1u64);
        let floor = DeletionFloor {
            domain: FLOOR_DOMAIN_PROMPTS.into(),
            item_id: "p-floor".into(),
            delete_vector_clock: delete_vc,
            delete_epoch: 3,
            content_hash: "h".into(),
            created_at: "2024-06-01T00:00:00+00:00".into(),
        };
        let mut files = BTreeMap::new();
        files.insert(
            "deletionFloors/items.json".to_string(),
            serde_json::to_vec_pretty(&[floor]).unwrap(),
        );
        let archive = tmp.path().join("floors.zip");
        let manifest = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "dev".into(),
            domains: vec![DOMAIN_DELETION_FLOORS.into()],
            files: BTreeMap::new(),
        };
        write_test_archive(&archive, &manifest, &files).unwrap();

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_DELETION_FLOORS.into()],
                RestoreMode::Merge,
            )
            .await
            .unwrap();
        drop(permit);
        drop(exclusive);

        let got = state.prompt_repo.get("p-floor").await.unwrap().unwrap();
        assert!(got.deleted, "floor DeleteWins 后 live 必须标记 deleted");
        let floor_row = DeletionFloorRepo::new(state.db.clone())
            .get(FLOOR_DOMAIN_PROMPTS, "p-floor")
            .await
            .unwrap();
        assert!(floor_row.is_some());
    }

    /// Merge restore 不得用较旧备份 floor 覆盖较新本地 floor（防删除复活）。
    ///
    /// Business Logic: local {A:5} + backup {A:3} → 本地仍支配；live {A:4} 仍 DeleteWins。
    #[tokio::test]
    async fn merge_restore_preserves_monotonic_deletion_floors() {
        let (state, tmp) = setup_restore_state().await;
        let floors = DeletionFloorRepo::new(state.db.clone());

        // 本地较新 floor {A:5}
        let mut local_vc = HashMap::new();
        local_vc.insert("A".to_string(), 5u64);
        floors
            .upsert(&DeletionFloor {
                domain: FLOOR_DOMAIN_PROMPTS.into(),
                item_id: "p-mono".into(),
                delete_vector_clock: local_vc.clone(),
                delete_epoch: 10,
                content_hash: "local-hash".into(),
                created_at: "2024-06-02T00:00:00+00:00".into(),
            })
            .await
            .unwrap();

        // live 中间版本 {A:4}（应被 floor {A:5} 支配为 DeleteWins）
        let live = sample_prompt(
            "p-mono",
            "A",
            "should-stay-deleted",
            4,
            "2024-05-01T00:00:00+00:00",
        );
        let live_vc = live.vector_clock.clone();
        state
            .prompt_repo
            .bulk_upsert(std::slice::from_ref(&live))
            .await
            .unwrap();

        // 备份较旧 floor {A:3}
        let mut backup_vc = HashMap::new();
        backup_vc.insert("A".to_string(), 3u64);
        let backup_floor = DeletionFloor {
            domain: FLOOR_DOMAIN_PROMPTS.into(),
            item_id: "p-mono".into(),
            delete_vector_clock: backup_vc,
            delete_epoch: 3,
            content_hash: "backup-hash".into(),
            created_at: "2024-05-01T00:00:00+00:00".into(),
        };
        let mut files = BTreeMap::new();
        files.insert(
            "deletionFloors/items.json".to_string(),
            serde_json::to_vec_pretty(&[backup_floor]).unwrap(),
        );
        let archive = tmp.path().join("floors-older.zip");
        let manifest = ArchiveManifest {
            format_version: FORMAT_VERSION,
            created_at: "t".into(),
            device_id: "dev".into(),
            domains: vec![DOMAIN_DELETION_FLOORS.into()],
            files: BTreeMap::new(),
        };
        write_test_archive(&archive, &manifest, &files).unwrap();

        let service = BackupRestoreService::new(state.clone());
        let exclusive = state.maintenance_gate.acquire_exclusive().await;
        let permit = DatabaseMaintenanceGate::exclusive_permit(&exclusive);
        service
            .apply_domains_in_transaction(
                &permit,
                &archive,
                &[DOMAIN_DELETION_FLOORS.into()],
                RestoreMode::Merge,
            )
            .await
            .unwrap();
        drop(permit);
        drop(exclusive);

        let kept = floors
            .get(FLOOR_DOMAIN_PROMPTS, "p-mono")
            .await
            .unwrap()
            .expect("floor must remain");
        assert_eq!(
            kept.delete_vector_clock.get("A").copied(),
            Some(5),
            "merge restore 不得把本地 floor A=5 降级为备份 A=3"
        );
        assert_eq!(kept.delete_epoch, 10);
        assert_eq!(kept.content_hash, "local-hash");

        // 中间 live A=4 仍被支配 → DeleteWins，且 reapply 后必须 deleted
        assert_eq!(
            DeletionFloorRepo::apply_deletion_floor(&kept, &live_vc),
            DeletionFloorDecision::DeleteWins
        );
        let got = state.prompt_repo.get("p-mono").await.unwrap().unwrap();
        assert!(
            got.deleted,
            "effective floor A=5 对 live A=4 必须 DeleteWins 并标记 deleted"
        );
    }
}
