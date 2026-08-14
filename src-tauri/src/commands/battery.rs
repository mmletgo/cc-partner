//! commands/battery.rs — 充电模式读写命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     前端 footer / 设置页 / 扣时心跳必须走 invoke，不能把余额放在 localStorage。
//!
//! Code Logic（这个模块做什么）:
//!     从 AppState 现取 BatteryRepo；快照 / 切模式 / 上报焦点 / 列流水。

use crate::backend::control_client::BackendControlClient;
use crate::battery::{self, BatterySnapshotDto};
use crate::config::BatteryConfig;
use crate::config_runtime::RuntimeConfigPatch;
use crate::error::AppError;
use crate::state::AppState;
use crate::storage::BatteryRepo;
use serde::Deserialize;
use tauri::State;

fn repo(state: &AppState) -> BatteryRepo {
    BatteryRepo::with_gate(state.db.clone(), state.maintenance_gate.clone())
}

fn battery_config(state: &AppState) -> crate::config::BatteryConfig {
    state.config.read().unwrap().battery.clone()
}

fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 读取充电快照。
#[tauri::command]
pub async fn get_battery_snapshot(
    state: State<'_, AppState>,
) -> Result<BatterySnapshotDto, AppError> {
    battery::get_snapshot(&repo(&state), &battery_config(&state), now_s()).await
}

/// 切换充电 / 无限。
#[tauri::command]
pub async fn set_battery_mode(
    state: State<'_, AppState>,
    mode: String,
) -> Result<BatterySnapshotDto, AppError> {
    let snap = battery::set_mode(&repo(&state), &battery_config(&state), &mode, now_s()).await?;
    state.emit_event("battery:changed", snap.clone());
    Ok(snap)
}

/// 上报一扇窗是否处于消耗路由且前台。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportBatteryFocusReq {
    pub window_label: String,
    pub consuming: bool,
}

/// 上报焦点并结算扣时。
#[tauri::command]
pub async fn report_battery_focus(
    state: State<'_, AppState>,
    req: ReportBatteryFocusReq,
) -> Result<BatterySnapshotDto, AppError> {
    let snap = battery::report_focus(
        &repo(&state),
        &battery_config(&state),
        &req.window_label,
        req.consuming,
        now_ms(),
    )
    .await?;
    state.emit_event("battery:changed", snap.clone());
    Ok(snap)
}

/// 流水 DTO。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatteryLedgerItemDto {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub source_id: Option<String>,
    pub delta_ms: i64,
    pub balance_after_ms: i64,
    pub note: Option<String>,
}

/// 最近流水。
#[tauri::command]
pub async fn list_battery_ledger(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<BatteryLedgerItemDto>, AppError> {
    let rows = repo(&state).list_ledger(limit.unwrap_or(50)).await?;
    Ok(rows
        .into_iter()
        .map(|r| BatteryLedgerItemDto {
            id: r.id,
            ts: r.ts,
            kind: r.kind,
            source_id: r.source_id,
            delta_ms: r.delta_ms,
            balance_after_ms: r.balance_after_ms,
            note: r.note,
        })
        .collect())
}

/// 读取充电额度数字配置。
#[tauri::command]
pub async fn get_battery_config(state: State<'_, AppState>) -> Result<BatteryConfig, AppError> {
    if let Ok(client) = BackendControlClient::from_control_file() {
        if let Ok(snap) = client.get_config().await {
            if let Ok(mut cfg) = state.config.write() {
                snap.apply_to_local_config(&mut cfg);
            }
            return Ok(snap.battery);
        }
    }
    Ok(state.config.read().unwrap().battery.clone())
}

/// 读取默认充电额度。
#[tauri::command]
pub fn get_default_battery_config() -> BatteryConfig {
    BatteryConfig::default()
}

/// 整表覆盖充电额度数字（不改模式 / 余额）。
#[tauri::command]
pub async fn update_battery_config(
    state: State<'_, AppState>,
    config: BatteryConfig,
) -> Result<BatteryConfig, AppError> {
    let client = BackendControlClient::from_control_file()?;
    let resp = client
        .apply_patch(RuntimeConfigPatch {
            battery: Some(config),
            ..Default::default()
        })
        .await?;
    if let Ok(mut cfg) = state.config.write() {
        resp.snapshot.apply_to_local_config(&mut cfg);
    }
    Ok(resp.snapshot.battery)
}
