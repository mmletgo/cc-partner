//! net/discovery.rs — mDNS 设备发现（mdns-sd）
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 局域网协作需要零配置自动发现同一网络中的其他 cc-partner 实例，
//!     无需用户手动输入 IP/端口。通过 mDNS（multicast DNS）协议注册本机服务并
//!     浏览对端服务。对照 Python `network/discovery.py`（zeroconf 实现）。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_discovery`：创建 ServiceDaemon → 按 advertise 决定是否注册本机服务
//!       （service type `_cc-partner._tcp.local.`，TXT 含 device_id/device_name，
//!       SRV record 的 port 为 axum/sidecar 实际监听端口）→ 按 browse 决定是否消费事件流
//!       更新 AppState 的 devices 表。
//!     - `stop_discovery`：shutdown daemon，清空 devices 表。
//!     - 本机过滤：ServiceResolved 时比对 TXT 的 device_id 与本机 device_id，一致则忽略
//!       （与 Python `_on_service_state_change` 过滤逻辑一致）。
//!     - 本机 IP 探测：`local_lan_ip` 优先选真实局域网接口 IP，对照 Python `_get_local_ip`。

use crate::models::device::Device;
use crate::net::SERVICE_TYPE;
use crate::state::AppState;
use chrono::Utc;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use tauri::async_runtime;

/// TXT 记录 key：设备 ID。
const TXT_KEY_DEVICE_ID: &str = "device_id";
/// TXT 记录 key：设备名。
const TXT_KEY_DEVICE_NAME: &str = "device_name";

/// mDNS 启动计划。
///
/// Business Logic（为什么需要这个结构）:
///     GUI sidecar 模式只需要浏览设备，不应重复宣告同一个 device_id；headless 则需要同时宣告与浏览。
///
/// Code Logic（这个结构做什么）:
///     保存 register_service 和 browse_services 两个执行开关，供 `start_discovery` 按计划调用 mdns-sd。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiscoveryStartPlan {
    register_service: bool,
    browse_services: bool,
}

impl DiscoveryStartPlan {
    /// 从调用方传入的 advertise/browse 参数创建 mDNS 启动计划。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     上层 runtime 使用 advertise/browse 描述启动意图，discovery 模块需要把它们映射为具体 mdns-sd 动作。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原样保存两个布尔值，保持测试可直接验证 browse-only 不触发 register。
    fn new(advertise: bool, browse: bool) -> Self {
        Self {
            register_service: advertise,
            browse_services: browse,
        }
    }
}

/// 按 mDNS 启动计划构造可注册的本机 ServiceInfo。
///
/// Business Logic（为什么需要这个函数）:
///     GUI sidecar browse-only 模式必须在“构造服务对象”这一层就停止本机广告路径，避免未来调用方误用计划字段后仍注册重复服务。
///
/// Code Logic（这个函数做什么）:
///     `register_service=false` 时直接返回 None；否则按 device_id/device_name/port/local_ip 构造 mdns-sd ServiceInfo。
fn build_service_info_for_plan(
    plan: &DiscoveryStartPlan,
    device_id: &str,
    device_name: &str,
    port: u16,
    local_ip: Option<IpAddr>,
) -> Result<Option<ServiceInfo>, String> {
    if !plan.register_service {
        return Ok(None);
    }

    let mut properties = HashMap::new();
    properties.insert(TXT_KEY_DEVICE_ID.to_string(), device_id.to_string());
    properties.insert(TXT_KEY_DEVICE_NAME.to_string(), device_name.to_string());

    let host_name = format!("cc-{device_id}.local.");
    let service_info = match local_ip {
        Some(ip) => ServiceInfo::new(SERVICE_TYPE, device_id, &host_name, ip, port, properties),
        None => ServiceInfo::new(SERVICE_TYPE, device_id, &host_name, "", port, properties)
            .map(|info| info.enable_addr_auto()),
    }
    .map_err(|e| format!("构造 ServiceInfo 失败: {e}"))?;

    Ok(Some(service_info))
}

/// 启动 mDNS 发现：按需注册本机服务 + 按需后台消费 browse 事件流。
///
/// Business Logic（为什么需要这个函数）:
///     Headless 后端需要宣告自己并发现对端；GUI 连接 sidecar 时只浏览局域网实例，避免同 device_id 重复广告。
///
/// Code Logic（这个函数做什么）:
///     创建 ServiceDaemon；advertise=true 时构造 ServiceInfo 并 register；browse=true 时 browse 同一 service type
///     并 spawn 事件循环；最后把 daemon 句柄存入 AppState.discovery 供关闭时 shutdown。
pub async fn start_discovery(
    state: &AppState,
    port: u16,
    advertise: bool,
    browse: bool,
) -> Result<(), String> {
    let plan = DiscoveryStartPlan::new(advertise, browse);
    if !plan.register_service && !plan.browse_services {
        tracing::info!("mDNS 发现未启动：advertise=false, browse=false");
        return Ok(());
    }

    // 创建 mDNS 守护进程（mdns-sd 内部起一个后台线程监听 5353）
    let daemon = ServiceDaemon::new().map_err(|e| format!("创建 mDNS daemon 失败: {e}"))?;

    // 读取本机设备信息用于注册服务
    let device_id = state.device_id.as_ref().clone();
    let device_name = state.device_name();

    let local_ip = if plan.register_service {
        local_lan_ip()
    } else {
        None
    };
    if let Some(service_info) =
        build_service_info_for_plan(&plan, &device_id, &device_name, port, local_ip)?
    {
        // 注册本机服务
        daemon
            .register(service_info)
            .map_err(|e| format!("注册 mDNS 服务失败: {e}"))?;
    }

    let receiver = if plan.browse_services {
        Some(
            daemon
                .browse(SERVICE_TYPE)
                .map_err(|e| format!("启动 mDNS browse 失败: {e}"))?,
        )
    } else {
        None
    };

    // 存入 AppState 供关闭使用
    {
        let mut guard = state.discovery.lock().expect("discovery 锁中毒");
        *guard = Some(daemon);
    }

    if let Some(receiver) = receiver {
        // spawn 后台任务消费事件流（持有 AppState 的 Clone，与 axum/命令层共享同一份 Arc）
        let state_clone = state.clone();
        let my_device_id = state.device_id.clone();
        async_runtime::spawn(async move {
            event_loop(receiver, state_clone, my_device_id).await;
        });
    }

    tracing::info!(
        "mDNS 发现已启动：service={}, device={}, port={}, advertise={}, browse={}",
        SERVICE_TYPE,
        device_name,
        port,
        advertise,
        browse
    );
    Ok(())
}

/// 停止 mDNS 发现：shutdown daemon 并清空 devices 表。
///
/// Business Logic: 应用关闭时注销本机服务、释放 mDNS 资源、清空对端列表。
pub fn stop_discovery(state: &AppState) {
    let daemon = {
        let mut guard = state.discovery.lock().expect("discovery 锁中毒");
        guard.take()
    };
    if let Some(daemon) = daemon {
        // shutdown 优雅停止守护线程（内部会注销服务）
        if let Err(e) = daemon.shutdown() {
            tracing::warn!("mDNS shutdown 失败: {e}");
        }
    }
    // 清空对端设备表
    state.devices.write().expect("devices 写锁中毒").clear();
    tracing::info!("mDNS 发现已停止");
}

/// 后台事件循环：消费 browse 事件流，更新 AppState.devices。
///
/// Business Logic: mDNS 事件在 mdns-sd 后台线程产生，经 channel 传到这里；
///     Resolved → 新增/更新对端设备；Removed → 剔除对端。本机设备（device_id 相同）一律忽略。
///
/// Code Logic: 用 `recv()` 阻塞等待事件；daemon shutdown 后 channel 断开，recv 返回 Err 即退出循环。
///     不用 recv_timeout——对端上下线完全由 mDNS 事件驱动，无需周期轮询。
async fn event_loop(receiver: Receiver<ServiceEvent>, state: AppState, my_device_id: Arc<String>) {
    loop {
        let event = match receiver.recv() {
            Ok(ev) => ev,
            Err(_) => {
                // channel 断开（daemon 已 shutdown），退出循环
                tracing::info!("mDNS 事件流已关闭，退出发现循环");
                break;
            }
        };

        match event {
            ServiceEvent::ServiceResolved(info) => {
                handle_resolved(&state, info, &my_device_id);
            }
            ServiceEvent::ServiceRemoved(_service_type, fullname) => {
                handle_removed(&state, &fullname, &my_device_id);
            }
            // ServiceFound / SearchStarted / SearchStopped 无需处理（Resolved 才有完整信息）
            _ => {}
        }
    }
}

/// 处理 ServiceResolved：解析 TXT/IP/port，写入 devices 表（过滤本机）。
///
/// Business Logic: 一个对端服务被完整解析后，更新本地设备列表。
/// Code Logic: 从 TXT 取 device_id/device_name；device_id 与本机一致则忽略；
///             从 addresses 取首个 IPv4 作为 host（与 Python `inet_ntoa(addresses[0])` 一致）。
fn handle_resolved(state: &AppState, info: ServiceInfo, my_device_id: &str) {
    // 解析 TXT
    let device_id = match info.get_property_val_str(TXT_KEY_DEVICE_ID) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => {
            tracing::warn!("mDNS 服务缺少 device_id TXT：{}", info.get_fullname());
            return;
        }
    };

    // 过滤本机（device_id 一致）
    if device_id == my_device_id {
        return;
    }

    let device_name = info
        .get_property_val_str(TXT_KEY_DEVICE_NAME)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string());

    // 取首个 IPv4 地址（与 Python 取 addresses[0] 一致；IPv6 场景回退到任一地址）
    let host = first_ipv4(&info).unwrap_or_else(|| "0.0.0.0".to_string());
    if host == "0.0.0.0" {
        tracing::warn!("mDNS 服务无法解析 IPv4 地址：{}", info.get_fullname());
        return;
    }

    let port = info.get_port();
    let host_for_log = host.clone();
    let device = Device {
        id: device_id.clone(),
        name: device_name.clone(),
        host,
        port,
        last_seen: Utc::now(),
        online: true,
    };

    let mut devices = state.devices.write().expect("devices 写锁中毒");
    devices.insert(device_id.clone(), device);
    tracing::info!("发现设备: {device_name} (id={device_id}, {host_for_log}:{port})");
}

/// 处理 ServiceRemoved：从 devices 表剔除对应设备（过滤本机）。
///
/// Business Logic: 对端下线（注销服务或超时）时移除其条目。
/// Code Logic: fullname 格式为 `{device_id}.{SERVICE_TYPE}`，去掉 type 后缀得到 device_id。
fn handle_removed(state: &AppState, fullname: &str, my_device_id: &str) {
    // fullname 形如 "{device_id}._cc-partner._tcp.local."，去掉 ".{SERVICE_TYPE}" 后缀
    let suffix = format!(".{SERVICE_TYPE}");
    let device_id = fullname.strip_suffix(&suffix).unwrap_or(fullname);

    if device_id == my_device_id {
        return;
    }

    let mut devices = state.devices.write().expect("devices 写锁中毒");
    if devices.remove(device_id).is_some() {
        tracing::info!("设备离线: {device_id}");
    }
}

/// 从 ServiceInfo 取首个 IPv4 地址（点分十进制）。
///
/// Business Logic: Python 取 `inet_ntoa(addresses[0])`（仅 IPv4）。这里同样优先 IPv4。
fn first_ipv4(info: &ServiceInfo) -> Option<String> {
    use std::net::Ipv4Addr;
    for ip in info.get_addresses() {
        if let IpAddr::V4(v4) = ip {
            return Some(Ipv4Addr::to_string(v4));
        }
    }
    // 全 IPv6 场景回退到任一地址的字符串形式
    info.get_addresses().iter().next().map(|ip| ip.to_string())
}

/// 探测本机局域网 IPv4 地址，对照 Python `_get_local_ip`。
///
/// Business Logic: mDNS A record 需要本机真实局域网 IP；系统可能有多接口
///     （WiFi、VPN、Docker），需优先选私有局域网段地址。
///
/// Code Logic:
///     1. 用 UDP socket "连接" 8.8.8.8（不实际发包），取本地绑定的 IP；
///        这是最可靠的跨平台方式获取出站接口 IP。
///     2. 过滤 loopback；若得到非回环地址即返回。
///     3. 失败返回 None（调用方回退到 addr_auto）。
pub fn local_lan_ip() -> Option<IpAddr> {
    use std::net::UdpSocket;
    // 用 UDP "连接" 公网地址探测出站接口 IP（对照 Python socket.connect(("8.8.8.8",80))）
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let local = socket.local_addr().ok()?;
    match local.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() => Some(IpAddr::V4(v4)),
        IpAddr::V6(v6) if !v6.is_loopback() => Some(IpAddr::V6(v6)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 browse-only 模式不会注册本机 mDNS 服务。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 连接独立 sidecar 后端时只能浏览局域网设备，不能用相同 device_id 重复宣告本机服务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 advertise=false、browse=true 构造启动计划，断言 register_service 为 false 且 browse_services 为 true。
    #[test]
    fn browse_only_mode_does_not_register_service() {
        let plan = DiscoveryStartPlan::new(false, true);

        assert!(!plan.register_service);
        assert!(plan.browse_services);
    }

    /// 验证 browse-only 模式不会构造本机 ServiceInfo。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     GUI 连接独立 sidecar 时不能产生可注册的本机服务对象，否则后续代码仍可能误注册重复 mDNS 广播。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 browse-only 计划调用 service 构造 helper，断言结果为 None，而不仅是检查计划字段。
    #[test]
    fn browse_only_mode_does_not_build_service_info() {
        let plan = DiscoveryStartPlan::new(false, true);
        let service_info = build_service_info_for_plan(
            &plan,
            "device-a",
            "测试设备",
            62116,
            Some("127.0.0.1".parse().unwrap()),
        )
        .expect("browse-only service info planning should not fail");

        assert!(service_info.is_none());
    }
}

// AppState::device_name 便捷访问定义在 state.rs（与类型定义同模块，组织更清晰）。
