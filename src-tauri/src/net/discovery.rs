//! net/discovery.rs — mDNS 设备发现（mdns-sd）
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 局域网协作需要零配置自动发现同一网络中的其他 Claude Partner 实例，
//!     无需用户手动输入 IP/端口。通过 mDNS（multicast DNS）协议注册本机服务并
//!     浏览对端服务。对照 Python `network/discovery.py`（zeroconf 实现）。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_discovery`：创建 ServiceDaemon → 注册本机服务
//!       （service type `_claude-partner._tcp.local.`，TXT 含 device_id/device_name，
//!       SRV record 的 port 为 axum 实际监听端口）→ spawn 后台任务消费 browse 事件流
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

/// TXT 记录 key：设备 ID（与 Python `discovery.py` 完全一致）。
const TXT_KEY_DEVICE_ID: &str = "device_id";
/// TXT 记录 key：设备名（与 Python `discovery.py` 完全一致）。
const TXT_KEY_DEVICE_NAME: &str = "device_name";

/// 启动 mDNS 发现：注册本机服务 + 后台消费 browse 事件流。
///
/// Business Logic: 应用启动、axum 拿到实际端口后调用。注册本机服务让对端能发现自己，
///     同时浏览局域网内其他实例，把发现的设备写入 AppState.devices。
///
/// Code Logic:
///     1. 创建 ServiceDaemon（mdns-sd 自带后台线程）。
///     2. 探测本机 LAN IP（失败则注册时不带地址，让库按 hostname 解析）。
///     3. 构造 ServiceInfo：ty_domain = SERVICE_TYPE，my_name = device_id（服务实例名），
///        host_name = `cp-{device_id}.local.`（对照 Python server_name，避免用系统 hostname
///        解析到多个 IP），port = axum 实际端口，TXT = {device_id, device_name}。
///     4. register 服务，browse 同一 service type。
///     5. spawn 任务循环 recv 事件：Resolved 更新 devices（过滤本机），Removed 剔除。
///     6. 把 daemon 句柄存入 AppState.discovery 供关闭时 shutdown。
pub async fn start_discovery(state: &AppState, port: u16) -> Result<(), String> {
    // 创建 mDNS 守护进程（mdns-sd 内部起一个后台线程监听 5353）
    let daemon = ServiceDaemon::new().map_err(|e| format!("创建 mDNS daemon 失败: {e}"))?;

    // 读取本机设备信息用于注册服务
    let device_id = state.device_id.as_ref().clone();
    let device_name = state.device_name();

    // 探测本机局域网 IP（用于 mDNS A record）。探测失败则注册空地址集，
    // mdns-sd 仍会通过 SRV/TXT 宣告，对端可经 hostname 解析（部分环境仍可达）。
    let local_ip = local_lan_ip();

    // 构造 TXT 记录（与 Python discovery.py 字段命名完全一致：device_id、device_name）
    let mut properties = HashMap::new();
    properties.insert(TXT_KEY_DEVICE_ID.to_string(), device_id.clone());
    properties.insert(TXT_KEY_DEVICE_NAME.to_string(), device_name.clone());

    // host_name 对照 Python server_name：用 device_id 专用主机名，避免系统 hostname 解析到
    // 多个 IP（含 VPN/Docker 虚拟接口）。
    let host_name = format!("cp-{device_id}.local.");

    // 服务实例名用 device_id（对照 Python name=f"{device_id}.{SERVICE_TYPE}"，my_name 不含 type 后缀）
    let service_info = match &local_ip {
        Some(ip) => ServiceInfo::new(SERVICE_TYPE, &device_id, &host_name, *ip, port, properties),
        None => ServiceInfo::new(SERVICE_TYPE, &device_id, &host_name, "", port, properties)
            .map(|info| info.enable_addr_auto()), // 无显式 IP 时让库自动更新接口地址
    }
    .map_err(|e| format!("构造 ServiceInfo 失败: {e}"))?;

    // 注册本机服务
    daemon
        .register(service_info)
        .map_err(|e| format!("注册 mDNS 服务失败: {e}"))?;

    // 开始浏览同类型服务
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| format!("启动 mDNS browse 失败: {e}"))?;

    // 存入 AppState 供关闭使用
    {
        let mut guard = state.discovery.lock().expect("discovery 锁中毒");
        *guard = Some(daemon);
    }

    // spawn 后台任务消费事件流（持有 AppState 的 Clone，与 axum/命令层共享同一份 Arc）
    let state_clone = state.clone();
    let my_device_id = state.device_id.clone();
    async_runtime::spawn(async move {
        event_loop(receiver, state_clone, my_device_id).await;
    });

    tracing::info!(
        "mDNS 发现已启动：service={}, device={}, port={}",
        SERVICE_TYPE,
        device_name,
        port
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
    // fullname 形如 "{device_id}._claude-partner._tcp.local."，去掉 ".{SERVICE_TYPE}" 后缀
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
fn local_lan_ip() -> Option<IpAddr> {
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

// AppState::device_name 便捷访问定义在 state.rs（与类型定义同模块，组织更清晰）。
