//! commands/backup.rs — 可验证导出/恢复 Tauri 命令层
//!
//! Business Logic（为什么需要这个模块）:
//!     Settings 数据导出/恢复页需要：创建导出包、预览校验、事务恢复、job 列表、
//!     pre-restore 备份列表与一键回退。GUI 不得在本进程跑 exclusive restore，
//!     必须代理到 sidecar owner；HeadlessOwner 直连本机 backup 服务。
//!
//! Code Logic（这个模块做什么）:
//!     GuiClient → BackendControlClient `backup/*`；
//!     HeadlessOwner → create_export_archive / BackupRestoreService。
//!     DTO 复用 `crate::backup` 公开类型（camelCase）。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::backup::{
    create_export_archive, list_pre_restore_backups as list_pre_restore_backup_paths,
    pre_restore_dir, pre_restore_infos_from_paths, BackupRestoreService, CreateBackupResult,
    InspectPreview, PreRestoreBackupInfo, RestoreMode, RestoreRequest, RestoreResult,
    FORMAT_VERSION,
};
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::RecoveryJobRow;
use std::path::PathBuf;
use tauri::State;

/// 创建导出备份 ZIP。
///
/// Business Logic（为什么需要这个函数）:
///     Settings「导出数据」：用户选定路径后写出可校验 ZIP，不含项目源码/凭据。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `backup/create`；Owner → `create_export_archive` 返回 path+formatVersion。
#[tauri::command]
pub async fn create_backup(
    state: State<'_, AppState>,
    dest_path: String,
) -> Result<CreateBackupResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.create_backup(&dest_path).await;
    }
    let dest = PathBuf::from(&dest_path);
    create_export_archive(state.inner(), &dest).await?;
    Ok(CreateBackupResult {
        path: dest.display().to_string(),
        format_version: FORMAT_VERSION,
    })
}

/// 只读预览备份包。
///
/// Business Logic（为什么需要这个函数）:
///     用户确认恢复前查看领域计数与警告；确认前零写入。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `backup/inspect`；Owner → `BackupRestoreService::inspect`。
#[tauri::command]
pub async fn inspect_backup(
    state: State<'_, AppState>,
    archive_path: String,
) -> Result<InspectPreview, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.inspect_backup(&archive_path).await;
    }
    let service = BackupRestoreService::new(state.inner().clone());
    service.inspect(PathBuf::from(&archive_path).as_path())
}

/// 事务恢复备份。
///
/// Business Logic（为什么需要这个函数）:
///     用户勾选领域与 merge/replace 后执行恢复；全程 exclusive maintenance_gate。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `backup/restore`；Owner → `service.restore(RestoreRequest)`。
#[tauri::command]
pub async fn restore_backup(
    state: State<'_, AppState>,
    archive_path: String,
    mode: RestoreMode,
    domains: Vec<String>,
) -> Result<RestoreResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client
            .restore_backup(&archive_path, mode, domains)
            .await;
    }
    let service = BackupRestoreService::new(state.inner().clone());
    service
        .restore(RestoreRequest {
            archive_path,
            mode,
            domains,
        })
        .await
}

/// 列出恢复任务。
///
/// Business Logic（为什么需要这个函数）:
///     Settings 展示最近恢复/回退历史与失败摘要。
///
/// Code Logic（这个函数做什么）:
///     limit 缺省 50；GuiClient 代理；Owner `list_jobs`。
#[tauri::command]
pub async fn list_recovery_jobs(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<RecoveryJobRow>, AppError> {
    let limit = limit.unwrap_or(50);
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.list_recovery_jobs(Some(limit)).await;
    }
    let service = BackupRestoreService::new(state.inner().clone());
    service.list_jobs(limit).await
}

/// 列出 pre-restore 自动备份。
///
/// Business Logic（为什么需要这个函数）:
///     用户需要看到恢复前自动生成的备份文件路径与时间。
///
/// Code Logic（这个函数做什么）:
///     GuiClient 代理；Owner 读 `recovery-backups` 目录并映射 PreRestoreBackupInfo。
#[tauri::command]
pub async fn list_pre_restore_backups(
    state: State<'_, AppState>,
) -> Result<Vec<PreRestoreBackupInfo>, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.list_pre_restore_backups().await;
    }
    let dir = pre_restore_dir()?;
    let paths = list_pre_restore_backup_paths(&dir)?;
    Ok(pre_restore_infos_from_paths(&paths))
}

/// 按 recovery job 一键回退。
///
/// Business Logic（为什么需要这个函数）:
///     恢复失败或误操作时，用 pre-restore 备份回灌选中域。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `backup/rollback`；Owner → `service.rollback_job`。
#[tauri::command]
pub async fn rollback_recovery_job(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<RestoreResult, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.rollback_recovery_job(&job_id).await;
    }
    let service = BackupRestoreService::new(state.inner().clone());
    service.rollback_job(&job_id).await
}
