//! commands/devices.rs — 设备列表命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设备面板通过 invoke 拉取当前已发现的对端设备列表，以及本机设备信息。
//!     对照 Python `protocol.py` 的 `handle_list_devices`（前端 REST）+ Device 序列化。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_devices`：读 AppState.devices 表，转 DeviceDto 列表返回（is_self=false）。
//!     - `get_local_device`：返回本机设备（id 取 device_id，address=127.0.0.1，port=actual_http_port，is_self=true）。

use crate::error::AppError;
use crate::models::device::DeviceDto;
use crate::state::AppState;
use std::sync::atomic::Ordering;
use tauri::State;

/// 列出当前已发现的对端设备。
///
/// Business Logic: 前端设备面板初始化时展示局域网内在线对端。
/// Code Logic: 委托 `list_devices_for_state`。
#[tauri::command]
pub async fn list_devices(state: State<'_, AppState>) -> Result<Vec<DeviceDto>, AppError> {
    list_devices_for_state(state.inner())
}

/// owner/本地：对端设备快照（`is_self=false`）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Tauri 与 `/api/mobile/devices` 共用同一份 mDNS 对端表。
///
/// Code Logic（这个函数做什么）:
///     读 `devices` 表，转 DTO（`is_self=false`），按 name 排序。
pub fn list_devices_for_state(state: &AppState) -> Result<Vec<DeviceDto>, AppError> {
    let devices = state
        .devices
        .read()
        .map_err(|_| AppError::generic("devices 读锁中毒"))?;
    let mut dtos: Vec<DeviceDto> = devices.values().map(|d| d.to_dto(false)).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(dtos)
}

/// 返回本机设备信息。
///
/// Business Logic: 前端设备面板顶部展示"本机"卡片，需本机 id/name/端口。
/// Code Logic: 委托 `get_local_device_for_state`。
#[tauri::command]
pub async fn get_local_device(state: State<'_, AppState>) -> Result<DeviceDto, AppError> {
    Ok(get_local_device_for_state(state.inner()))
}

/// owner/本地：合成「这台电脑」设备（`is_self=true`）。
///
/// Business Logic（为什么需要这个函数）:
///     移动端目标列表要把本机放在对端之前；与桌面 `get_local_device` 字段一致。
///
/// Code Logic（这个函数做什么）:
///     `device_id` + config 名 + `127.0.0.1` + `actual_http_port` + 本机协议能力。
pub fn get_local_device_for_state(state: &AppState) -> DeviceDto {
    let device_name = state.device_name();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let info = crate::net::protocol::server_protocol_info();
    DeviceDto {
        id: state.device_id.as_ref().clone(),
        name: device_name,
        address: "127.0.0.1".to_string(),
        port,
        last_seen: chrono::Utc::now().to_rfc3339(),
        online: true,
        is_self: true,
        proto_version: info.protocol_version,
        capabilities: info.capabilities,
    }
}
