//! commands/ssh_target.rs — SSH 目标 invoke 命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 SSH 页通过 invoke 调用这些命令：列出已配置目标、新增/更新目标的用户名/端口、
//!     删除目标、查询本机操作系统（用于按系统渲染配置指南）。
//!
//! Code Logic（这个模块做什么）:
//!     从 State 取 device_id 与 ssh_target_repo；upsert 时读旧记录推进 vector_clock（新建 {device_id:1}，
//!     更新 increment）；delete 软删除推进 vc。get_os_info 用 std::env::consts::OS 归一化平台。

use crate::error::AppError;
use crate::models::ssh_target::{SshTargetDto, SshTargetRow};
use crate::state::AppState;
use chrono::Utc;
use std::collections::HashMap;
use tauri::State;

/// 当前时间的 RFC3339 字符串（带 UTC 时区）。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 本机操作系统信息（camelCase，对齐前端 types）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsInfo {
    /// 归一化平台：mac / windows / ubuntu
    pub platform: String,
    /// 原始 OS 字符串（macos / windows / linux 等）
    pub raw: String,
}

/// 列出所有已配置的 SSH 目标（排除已删除），返回 DTO。
#[tauri::command]
pub async fn list_ssh_targets(state: State<'_, AppState>) -> Result<Vec<SshTargetDto>, AppError> {
    let rows = state.ssh_target_repo.list().await?;
    Ok(rows.iter().map(|r| r.to_dto()).collect())
}

/// 新增/更新一个 SSH 目标（按 host 主键 upsert），返回更新后的 DTO。
///
/// Business Logic: 用户在前端为某 host 设置用户名/端口。新建初始化 {device_id:1}，更新 increment
///     本设备计数器，使对端感知变更。
/// Code Logic: port 缺省 22；读旧记录取 created_at 与 vector_clock；推进 vc；upsert 落库。
#[tauri::command]
pub async fn upsert_ssh_target(
    state: State<'_, AppState>,
    host: String,
    username: String,
    port: Option<u16>,
    label: Option<String>,
) -> Result<SshTargetDto, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let port = port.unwrap_or(22);
    let now = now_iso();

    let existing = state.ssh_target_repo.get(&host).await?;
    let mut vector_clock = match &existing {
        Some(row) => {
            let mut vc = row.vector_clock.clone();
            let counter = vc.entry(device_id.clone()).or_insert(0);
            *counter += 1;
            vc
        }
        None => {
            let mut vc = HashMap::new();
            vc.insert(device_id.clone(), 1);
            vc
        }
    };
    let created_at = existing
        .as_ref()
        .map(|r| r.created_at.clone())
        .unwrap_or_else(|| now.clone());

    let row = SshTargetRow {
        host: host.clone(),
        port,
        username,
        label,
        device_id,
        vector_clock: std::mem::take(&mut vector_clock),
        created_at,
        updated_at: now,
        deleted: false,
    };
    state.ssh_target_repo.upsert(&row).await?;
    Ok(row.to_dto())
}

/// 软删除一个 SSH 目标。
///
/// Business Logic: CRDT 删除是一次写入，需推进 vector_clock 让对端感知删除事件。
#[tauri::command]
pub async fn delete_ssh_target(
    state: State<'_, AppState>,
    host: String,
) -> Result<serde_json::Value, AppError> {
    let device_id = state.device_id.as_ref().clone();
    let mut row = state
        .ssh_target_repo
        .get(&host)
        .await?
        .ok_or_else(|| AppError::not_found("SSH 目标不存在"))?;
    let counter = row.vector_clock.entry(device_id).or_insert(0);
    *counter += 1;
    let now = now_iso();
    state
        .ssh_target_repo
        .soft_delete(&host, &now, &row.vector_clock)
        .await?;
    Ok(serde_json::json!({ "ok": true, "host": host }))
}

/// 查询本机操作系统，归一化为 mac/windows/ubuntu，供前端按系统渲染配置指南。
#[tauri::command]
pub async fn get_os_info() -> Result<OsInfo, AppError> {
    let raw = std::env::consts::OS;
    let platform = match raw {
        "macos" => "mac",
        "windows" => "windows",
        _ => "ubuntu", // linux 及其他统一映射为 ubuntu
    };
    Ok(OsInfo {
        platform: platform.to_string(),
        raw: raw.to_string(),
    })
}
