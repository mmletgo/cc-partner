//! commands/devices.rs — 设备列表命令
//!
//! Business Logic（为什么需要这个模块）:
//!     前端设备面板通过 invoke 拉取当前已发现的对端设备列表，以及本机设备信息。
//!     对照 Python `protocol.py` 的 `handle_list_devices`（前端 REST）+ Device 序列化。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_devices`：读 AppState.devices 直连表 + relay 影子表，合并转 DeviceDto
//!       列表返回（is_self=false；影子条目带 via 字段）。
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

/// owner/本地：对端设备快照（`is_self=false`，含经跳板可见的影子设备）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Tauri 与 `/api/mobile/devices` 共用同一份"直连 + 影子"对端表；
///     影子设备经跳板可见，列表需要带 via 信息与在线状态。
///
/// Code Logic（这个函数做什么）:
///     委托 `collect_device_dtos_with_shadows`（直连按 name 排序在前，
///     影子按 name 排序追加在后）。
pub fn list_devices_for_state(state: &AppState) -> Result<Vec<DeviceDto>, AppError> {
    collect_device_dtos_with_shadows(state)
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
pub fn collect_device_dtos_with_shadows(state: &AppState) -> Result<Vec<DeviceDto>, AppError> {
    // 锁序与 net::relay_shadow 写入方一致：先影子表后 devices（避免交叉锁死锁）。
    let shadows = state
        .relay
        .shadow_devices
        .read()
        .map_err(|_| AppError::generic("relay 影子表读锁中毒"))?;
    let devices = state
        .devices
        .read()
        .map_err(|_| AppError::generic("devices 读锁中毒"))?;
    let mut direct: Vec<DeviceDto> = devices.values().map(|d| d.to_dto(false)).collect();
    direct.sort_by(|a, b| a.name.cmp(&b.name));
    if shadows.is_empty() {
        return Ok(direct);
    }
    let mut shadow_dtos: Vec<DeviceDto> = Vec::with_capacity(shadows.len());
    for shadow in shadows.values() {
        let via = devices.get(&shadow.via_device_id);
        let via_online = via.map(|d| d.online).unwrap_or(false);
        let (address, port) = via
            .map(|d| (d.host.clone(), d.port))
            .unwrap_or_else(|| (String::new(), 0));
        shadow_dtos.push(DeviceDto {
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
    shadow_dtos.sort_by(|a, b| a.name.cmp(&b.name));
    direct.extend(shadow_dtos);
    Ok(direct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::device::Device;
    use crate::net::relay_shadow::RelayShadowDevice;
    use crate::net::relay_shadow_probe::test_support::build_test_state;

    /// 固定测试身份：A（本机）/ 直连对端 B / 跳板 V / 经跳板目标 C、D。
    const SELF_ID: &str = "devices-self-A";
    const DIRECT_B: &str = "devices-direct-B";
    const VIA_V: &str = "devices-via-V";
    const TARGET_C: &str = "devices-target-C";
    const TARGET_D: &str = "devices-target-D";

    /// 构造一条直连表 Device。
    fn device(id: &str, host: &str, port: u16, online: bool) -> Device {
        Device {
            id: id.to_string(),
            name: format!("device-{id}"),
            host: host.to_string(),
            port,
            last_seen: chrono::Utc::now(),
            online,
            proto_version: 1,
            capabilities: Vec::new(),
        }
    }

    /// 构造一条影子条目。
    fn shadow(target: &str, via: &str, online: bool) -> RelayShadowDevice {
        RelayShadowDevice {
            target_device_id: target.to_string(),
            via_device_id: via.to_string(),
            device_name: format!("device-{target}"),
            proto_version: 1,
            capabilities: vec!["workbench.projects.v1".to_string()],
            online,
            last_seen: chrono::Utc::now(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备列表合并是用户感知影子的唯一入口：直连在前按 name 排序，影子在后按
    ///     name 排序，影子 DTO 的 address/port 取 via 跳板直连地址并带 via 字段。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直连表注入 B（online）与跳板 V（online），影子表注入 C（经 V online）；
    ///     断言输出顺序 [B, V, C]、C 的 address/port=via、viaDeviceId/viaDeviceName 正确。
    #[test]
    fn collect_merges_direct_and_shadow_dtos_with_via_fields() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(SELF_ID, vec![VIA_V.to_string()]));
        state.devices.write().unwrap().insert(
            DIRECT_B.to_string(),
            device(DIRECT_B, "192.168.1.9", 62116, true),
        );
        state
            .devices
            .write()
            .unwrap()
            .insert(VIA_V.to_string(), device(VIA_V, "10.0.0.2", 62117, true));
        state
            .relay
            .shadow_devices
            .write()
            .unwrap()
            .insert(TARGET_C.to_string(), shadow(TARGET_C, VIA_V, true));

        let dtos = list_devices_for_state(&state).unwrap();

        let ids: Vec<&str> = dtos.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![DIRECT_B, VIA_V, TARGET_C],
            "直连按 name 排序在前，影子追加在后"
        );
        let shadow_dto = dtos.iter().find(|d| d.id == TARGET_C).unwrap();
        assert_eq!(
            shadow_dto.address, "10.0.0.2",
            "影子 address 应取 via 跳板地址"
        );
        assert_eq!(shadow_dto.port, 62117);
        assert!(shadow_dto.online);
        assert_eq!(shadow_dto.via_device_id.as_deref(), Some(VIA_V));
        // via_device_name 取 via 在直连表中的设备名（helper 命名为 `device-{id}`）。
        assert_eq!(
            shadow_dto.via_device_name.as_deref(),
            Some("device-devices-via-V")
        );
        assert!(!shadow_dto.is_self);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     影子 online 语义 = via 可达 && via 报告 online：via 从直连表消失或掉线时，
    ///     合并输出必须强制 offline（防探测周期间隙的陈旧 online 误导用户）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     影子表注入 C（经 V，online=true）与 D（经 V，online=false），跳板 V offline；
    ///     断言两条影子输出 online 均为 false。
    #[test]
    fn collect_marks_shadow_offline_when_via_not_reachable() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(SELF_ID, vec![VIA_V.to_string()]));
        state
            .devices
            .write()
            .unwrap()
            .insert(VIA_V.to_string(), device(VIA_V, "10.0.0.2", 62117, false));
        {
            let mut shadows = state.relay.shadow_devices.write().unwrap();
            shadows.insert(TARGET_C.to_string(), shadow(TARGET_C, VIA_V, true));
            shadows.insert(TARGET_D.to_string(), shadow(TARGET_D, VIA_V, false));
        }

        let dtos = list_devices_for_state(&state).unwrap();

        for target in [TARGET_C, TARGET_D] {
            let dto = dtos.iter().find(|d| d.id == target).unwrap();
            assert!(!dto.online, "{target} 在 via 掉线时必须 offline");
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     无影子时合并输出必须与旧纯直连行为完全一致（排序、字段），保证
    ///     `/api/mobile/devices` 与桌面列表在未配置跳板时零回归。
    ///
    /// Code Logic（这个测试做什么）:
    ///     只注入直连设备，断言输出不含 via 字段且按 name 排序。
    #[test]
    fn collect_without_shadows_matches_direct_only_behavior() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let state = rt.block_on(build_test_state(SELF_ID, Vec::new()));
        state.devices.write().unwrap().insert(
            DIRECT_B.to_string(),
            device(DIRECT_B, "192.168.1.9", 62116, true),
        );

        let dtos = list_devices_for_state(&state).unwrap();

        assert_eq!(dtos.len(), 1);
        assert_eq!(dtos[0].id, DIRECT_B);
        assert!(dtos[0].via_device_id.is_none());
        assert!(dtos[0].via_device_name.is_none());
    }
}
