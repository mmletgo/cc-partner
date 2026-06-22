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
/// Code Logic: 读 RwLock<HashMap> 拷贝快照，每个 Device 转 DTO（is_self=false），按 name 排序稳定输出。
#[tauri::command]
pub async fn list_devices(state: State<'_, AppState>) -> Result<Vec<DeviceDto>, AppError> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    let mut dtos: Vec<DeviceDto> = devices.values().map(|d| d.to_dto(false)).collect();
    // 按 name 排序，保证前端展示顺序稳定
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(dtos)
}

/// 返回本机设备信息。
///
/// Business Logic: 前端设备面板顶部展示"本机"卡片，需本机 id/name/端口。
/// Code Logic: device_id 从 AppState.device_id 取，device_name 从 config 读，
///             address 固定 127.0.0.1（本机回环，供前端展示；实际局域网 IP 由对端发现），
///             port 取 actual_http_port 原子读。
#[tauri::command]
pub async fn get_local_device(state: State<'_, AppState>) -> Result<DeviceDto, AppError> {
    let device_name = state.device_name();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    Ok(DeviceDto {
        id: state.device_id.as_ref().clone(),
        name: device_name,
        address: "127.0.0.1".to_string(),
        port,
        last_seen: chrono::Utc::now().to_rfc3339(),
        online: true,
        is_self: true,
    })
}
