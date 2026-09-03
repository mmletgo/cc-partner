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
        via_device_id: None,
        via_device_name: None,
    }
}

/// 含影子设备的对端快照（直连 + 经跳板可见，device_id 直连优先）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 `list_devices`、`/api/mobile/devices` 与 control plane `devices` 端点
///     需要同一份"直连 + 影子"合并视图；单点实现避免三处口径漂移
///     （影子 online 语义 = via 可达 && via 报告 online；address 展示跳板地址）。
///
/// Code Logic（这个函数做什么）:
///     先收集直连 DTO（排除本机），再读影子表把影子条目转为 DeviceDto
///     （address/port 取 via 跳板的直连地址；via 已不在直连表时 online 强制 false），
///     影子按 name 排序追加在直连之后。
// 脚手架：影子合并由 A 侧任务接线（当前仅直连），接线时移除本 allow。
#[allow(dead_code)]
pub fn collect_device_dtos_with_shadows(state: &AppState) -> Result<Vec<DeviceDto>, AppError> {
    let mut direct = list_devices_for_state(state)?;
    let shadows = state
        .relay
        .shadow_devices
        .read()
        .map_err(|_| AppError::generic("relay 影子表读锁中毒"))?;
    if shadows.is_empty() {
        return Ok(direct);
    }
    let devices = state
        .devices
        .read()
        .map_err(|_| AppError::generic("devices 读锁中毒"))?;
    for shadow in shadows.values() {
        let via = devices.get(&shadow.via_device_id);
        let via_online = via.map(|d| d.online).unwrap_or(false);
        let (address, port) = via
            .map(|d| (d.host.clone(), d.port))
            .unwrap_or_else(|| (String::new(), 0));
        direct.push(DeviceDto {
            id: shadow.target_device_id.clone(),
            name: shadow.device_name.clone(),
            address,
            port,
            last_seen: shadow.last_seen.to_rfc3339(),
            online: shadow.online && via_online,
            is_self: false,
            proto_version: shadow.proto_version,
            capabilities: shadow.capabilities.clone(),
            via_device_id: Some(shadow.via_device_id.clone()),
            via_device_name: via.map(|d| d.name.clone()),
        });
    }
    Ok(direct)
}
