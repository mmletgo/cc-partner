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
use crate::net::protocol::server_protocol_info;
#[cfg(test)]
use crate::net::protocol::PROTOCOL_VERSION_V1;
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
/// TXT 记录 key：协议版本提示（值为十进制 u32 字符串；缺失/非法视为 v0）。
const TXT_KEY_PROTO: &str = "proto";
/// TXT 记录 key：能力清单提示（值为按字典序排序、逗号分隔的 token 字符串）。
const TXT_KEY_CAPS: &str = "caps";

/// `caps` TXT value（逗号分隔的 token 列表，不含 `caps=` key 前缀）的 UTF-8 字节上限。
///
/// Business Logic: mDNS TXT 记录整体须保持精简（单条 RR 推荐上限 ~255B，整组 RR 推荐上限 ~1300B），
///     为 device_id/device_name/proto 等其它键留出空间，且不得让单个能力 token 被截断成无意义片段。
/// Code Logic: 与计划一致固定为 220；`encode_mdns_capabilities` 在累加过程中遇到超限的完整 token 时整段丢弃。
const MAX_CAPS_TXT_BYTES: usize = 220;

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
    // 协议元数据提示：proto=<u32>，caps=<bounded-list>。仅作发现层快速预筛，权威值仍来自对端 health。
    let info = server_protocol_info();
    properties.insert(TXT_KEY_PROTO.to_string(), info.protocol_version.to_string());
    properties.insert(
        TXT_KEY_CAPS.to_string(),
        encode_mdns_capabilities(&info.capabilities, MAX_CAPS_TXT_BYTES),
    );

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

    // 解析协议元数据提示（非权威，缺失/非法回落为 v0 / 空）。
    let proto_version = parse_proto_hint(info.get_property_val_str(TXT_KEY_PROTO));
    let capabilities = parse_caps_hint(info.get_property_val_str(TXT_KEY_CAPS));

    let device = Device {
        id: device_id.clone(),
        name: device_name.clone(),
        host,
        port,
        last_seen: Utc::now(),
        online: true,
        proto_version,
        capabilities,
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

/// 把能力 token 列表编码为 mDNS TXT `caps` 值（逗号分隔，不含 `caps=` 前缀）。
///
/// Business Logic（为什么需要这个函数）:
///     mDNS TXT 记录空间受限，能力清单可能很长或包含多字节 token，直接拼接会撑爆 TXT 上限，
///     还可能把某个 token 切成无意义的半截。本函数按字典序排序、去重，再逐个累加完整 token，
///     一旦加入下一个完整 token 会让整段值超过 `max_txt_bytes` 字节就停止，
///     保证每个出现的 token 都是完整的。返回值仅是逗号分隔的 token 列表——`caps=` 前缀由
///     TXT key（`TXT_KEY_CAPS`）在 wire 侧自动提供，函数内部若再补一次前缀会导致
///     对端读到 `caps=caps=...` 双前缀（Finding 6）。
///
/// Code Logic（这个函数做什么）:
///     1. 排序 + 去重输入；
///     2. 逐个累加完整 token，每次先按"当前长度 + 分隔符 + token"计算字节数，超限就停止累加；
///     3. 即使第一个 token 也放不下也返回空串，不会截断 token；
///     4. 输出不含 `caps=` 前缀（前缀由 TXT key 提供），空输入返回空串。
pub fn encode_mdns_capabilities(capabilities: &[String], max_txt_bytes: usize) -> String {
    let mut sorted: Vec<&String> = capabilities.iter().collect();
    sorted.sort();
    sorted.dedup();

    let mut out = String::new();
    let mut current_len: usize = 0;

    for token in sorted {
        let token_bytes = token.len();
        // 第一个 token 无分隔符；后续 token 前补逗号。
        let separator_len = if current_len == 0 { 0 } else { 1 };
        let projected = current_len + separator_len + token_bytes;
        if projected > max_txt_bytes {
            // 已按字典序排序，后续 token 至少等长或更长，无法再塞下，停止累加。
            break;
        }
        if separator_len == 1 {
            out.push(',');
        }
        out.push_str(token);
        current_len = projected;
    }

    out
}

/// 解析 `proto` TXT 提示为 u32 协议版本；缺失或非法（含负数/溢出/空串）一律回落为 v0。
///
/// Business Logic: mDNS 提示不是权威来源，对端可能用旧版本不带 proto，也可能写了非数字字符串，
///     绝不能因解析失败把对端踢出 devices 表；统一安全回落 v0（`supports()` 对 v0 永远返回 false）。
fn parse_proto_hint(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0)
}

/// 解析 `caps` TXT 提示为能力 Vec<String>；空 token / 缺失 一律丢弃。
///
/// Business Logic: 与 `proto` 一样属于非权威提示，仅用于发现层预筛。空 token（连续逗号、首尾逗号）
///     没有任何语义，必须丢弃避免与精确 token 匹配冲突。这里不去重/排序——`PeerProtocolInfo`
///     反序列化路径会负责规范化；直接构造 `Device` 时也已是排序去重后的来源。
fn parse_caps_hint(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(s) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        None => Vec::new(),
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

    /// 验证 `encode_mdns_capabilities` 不带 `caps=` 前缀（前缀由 TXT key 提供）。
    ///
    /// Business Logic: 对端解析器按 `caps` key 直接读取 value，若 value 自带 `caps=` 前缀会形成
    ///     `caps=caps=...` 双前缀。空输入应返回空串（无前缀、无 token），由对端解释为“无能力提示”。
    /// Code Logic: 空输入断言返回空串，避免与 wire 侧 key 拼出 `caps=`。
    #[test]
    fn encode_capabilities_empty_yields_no_prefix() {
        assert_eq!(encode_mdns_capabilities(&[], MAX_CAPS_TXT_BYTES), "");
    }

    /// 验证能力 token 按字典序排序输出（不含 `caps=` 前缀）。
    ///
    /// Business Logic: 确定性的排序输出便于 diff、日志、与对端断言比对。
    /// Code Logic: 输入乱序，断言输出按字典序排列且不带前缀。
    #[test]
    fn encode_capabilities_sorts_tokens_lexicographically() {
        let caps = vec![
            "zzz.last".to_string(),
            "errors.envelope.v1".to_string(),
            "inbox.messages.v1".to_string(),
            "aaa.first".to_string(),
        ];
        let encoded = encode_mdns_capabilities(&caps, MAX_CAPS_TXT_BYTES);
        assert_eq!(
            encoded,
            "aaa.first,errors.envelope.v1,inbox.messages.v1,zzz.last"
        );
    }

    /// 验证重复 token 被去重。
    ///
    /// Business Logic: 重复 token 会让对端解析集合不稳定，也可能无谓消耗 TXT 空间。
    /// Code Logic: 输入含重复，断言输出每个 token 仅出现一次。
    #[test]
    fn encode_capabilities_deduplicates_tokens() {
        let caps = vec![
            "errors.envelope.v1".to_string(),
            "errors.envelope.v1".to_string(),
            "inbox.messages.v1".to_string(),
        ];
        let encoded = encode_mdns_capabilities(&caps, MAX_CAPS_TXT_BYTES);
        assert_eq!(encoded, "errors.envelope.v1,inbox.messages.v1");
    }

    /// 验证超过 `max_txt_bytes` 字节上限的完整 token 会被整段丢弃，不会出现截断。
    ///
    /// Business Logic: 被截断的半截 token 没有语义且可能误命中其它能力，必须避免；
    ///     一旦某个完整 token 放不下就停止累加（后续 token 在排序后只会更长或等长）。
    ///
    /// Code Logic:
    ///     1. 构造两个短 token（能放下）+ 一个长 token（超 30 字节上限）。
    ///     2. 断言输出只含两个短 token，长 token 既不出现也不以截断形式出现。
    ///     3. 断言输出总字节数 <= 上限。
    #[test]
    fn encode_capabilities_drops_oversize_token_without_truncation() {
        let caps = vec![
            "aa".to_string(),
            "bb".to_string(),
            "this-is-a-very-long-token-that-cannot-fit".to_string(),
        ];
        let encoded = encode_mdns_capabilities(&caps, 30);
        assert_eq!(encoded, "aa,bb");
        assert!(encoded.len() <= 30);
        // 显式断言：超长 token 的任何前缀都不应出现在输出中（无截断）。
        assert!(!encoded.contains("this-is"));
    }

    /// 验证当连第一个 token 都放不下时返回空串。
    ///
    /// Business Logic: 极端紧凑的 TXT 上限下，宁可宣告“无能力提示”也不能塞半截 token。
    /// Code Logic: 单个长 token + 极小上限，断言输出为空串。
    #[test]
    fn encode_capabilities_first_token_too_large_yields_empty() {
        let caps = vec!["toolongtoken".to_string()];
        let encoded = encode_mdns_capabilities(&caps, 10);
        assert_eq!(encoded, "");
    }

    /// 验证多字节（UTF-8）token 按字节而非字符计数。
    ///
    /// Business Logic: 能力 token 可能含中文/emoji 等多字节字符，UTF-8 字节数才是 TXT 真实占用；
    ///     若按字符数计数会低估实际占用、撑爆 TXT 上限还自以为合规。
    ///
    /// Code Logic:
    ///     "🛰️" 由 U+1F6F0(4B) + U+FE0F(3B) 组成，共 7 字节 / 2 个码点 / 1 个字素。
    ///     - max=7：token 7B = 7B ≤ 7 → 完整放入（无前缀占用字节）。
    ///     - max=6：token 7B > 6 → 整段丢弃（空串），绝不截断多字节字符。
    ///     若实现误用 chars().count()(=2) 或 grapheme 计数(=1)，max=6 时也会被放入，从而失败。
    #[test]
    fn encode_capabilities_counts_multibyte_tokens_by_utf8_bytes() {
        let emoji_token = "🛰️";
        assert_eq!(emoji_token.chars().count(), 2, "precondition: 2 codepoints");
        assert_eq!(emoji_token.len(), 7, "precondition: 7 UTF-8 bytes");

        // 边界右侧：按字节刚好放下。
        let fits = encode_mdns_capabilities(std::slice::from_ref(&emoji_token.to_string()), 7);
        assert_eq!(fits, "🛰️");
        assert_eq!("🛰️".len(), 7);

        // 边界左侧：按字节差 1，整段丢弃（空串），绝不截断多字节字符。
        let dropped = encode_mdns_capabilities(std::slice::from_ref(&emoji_token.to_string()), 6);
        assert_eq!(dropped, "");
    }

    /// 验证 service 注册时构造的 TXT properties 含 `proto=1` 与 `caps=<bounded-list>`，
    /// 且 caps value 不含 `caps=` 前缀（前缀由 TXT key 提供，避免双前缀 bug，Finding 6）。
    ///
    /// Business Logic: 本机对 mDNS 局域网广播的协议元数据提示必须与 `server_protocol_info()` 一致，
    ///     让对端在 health 实测前就能预筛能力。`proto` 必须等于当前 PROTOCOL_VERSION_V1。
    ///     `caps` 的 value 必须是裸 token 列表，对端 wire 侧按 key `caps` 读取。
    ///
    /// Code Logic: 用 advertise 计划构造 ServiceInfo，从 ServiceInfo 读回 TXT 属性，
    ///             断言 proto=1、caps value 恰为 `errors.envelope.v1`（无 `caps=` 前缀）。
    #[test]
    fn service_info_advertises_proto_and_caps_hints() {
        let plan = DiscoveryStartPlan::new(true, false);
        let service_info = build_service_info_for_plan(
            &plan,
            "device-a",
            "测试设备",
            62116,
            Some("127.0.0.1".parse().unwrap()),
        )
        .expect("advertise plan should build ServiceInfo")
        .expect("register_service=true should yield Some(ServiceInfo)");

        assert_eq!(service_info.get_property_val_str(TXT_KEY_PROTO), Some("1"));
        assert_eq!("1", PROTOCOL_VERSION_V1.to_string());
        let caps = service_info
            .get_property_val_str(TXT_KEY_CAPS)
            .expect("caps TXT must be present");
        // value 必须是裸 token 列表，不能自带 `caps=` 前缀（否则对端读到 `caps=caps=...`）。
        assert_eq!(caps, "errors.envelope.v1");
        assert!(
            !caps.starts_with("caps="),
            "caps value must NOT carry the `caps=` prefix (it is provided by the TXT key); got: {caps}"
        );
    }

    /// 验证 `proto` TXT 提示解析：合法数字 → 对应版本；缺失/非法/空串 → 0。
    ///
    /// Business Logic: mDNS 提示非权威，对端可能是旧版（无 proto）或写了非法值，必须安全回落 v0。
    /// Code Logic: 直接调用 parse_proto_hint，覆盖 4 个分支。
    #[test]
    fn parse_proto_hint_handles_valid_missing_and_malformed() {
        assert_eq!(parse_proto_hint(Some("1")), 1);
        assert_eq!(parse_proto_hint(Some("  2 ")), 2);
        assert_eq!(parse_proto_hint(Some("0")), 0);
        assert_eq!(parse_proto_hint(None), 0);
        assert_eq!(parse_proto_hint(Some("")), 0);
        assert_eq!(parse_proto_hint(Some("not-a-number")), 0);
        assert_eq!(parse_proto_hint(Some("-1")), 0);
        assert_eq!(parse_proto_hint(Some("99999999999999999999")), 0);
    }

    /// 验证 `caps` TXT 提示解析：按逗号切分、去空白、丢空 token。
    ///
    /// Business Logic: 连续逗号、首尾逗号、空白 token 没有语义，必须丢弃，
    ///     避免空串与精确 token 匹配冲突。
    /// Code Logic: 直接调用 parse_caps_hint，覆盖正常/异常输入。
    #[test]
    fn parse_caps_hint_drops_empty_tokens() {
        assert_eq!(
            parse_caps_hint(Some("errors.envelope.v1,inbox.messages.v1")),
            vec!["errors.envelope.v1", "inbox.messages.v1"]
        );
        // 首尾/连续逗号 → 空 token 被丢弃
        assert_eq!(
            parse_caps_hint(Some(",errors.envelope.v1,,,")),
            vec!["errors.envelope.v1"]
        );
        // 缺失 → 空
        assert!(parse_caps_hint(None).is_empty());
        // 空串 → 空
        assert!(parse_caps_hint(Some("")).is_empty());
    }

    /// 验证 mDNS 注册 → 解析 round-trip：本机 advertise 的 proto/caps 提示能被对端解析路径还原。
    ///
    /// Business Logic: 保证 publish 端和 parse 端用同一套约定（key/格式），避免“自己宣告的对端读不懂”。
    ///     caps value 不带 `caps=` 前缀（前缀由 TXT key 提供），parse 端直接拿 value 走切分即可。
    /// Code Logic: 构造 ServiceInfo（advertise），手动从其 TXT 取 proto/caps 走解析函数，
    ///             断言还原出 proto_version=1 且 capabilities 包含 errors.envelope.v1。
    #[test]
    fn mdns_proto_caps_round_trip_through_publish_and_parse() {
        let plan = DiscoveryStartPlan::new(true, false);
        let service_info = build_service_info_for_plan(
            &plan,
            "device-b",
            "Round-Trip 设备",
            62117,
            Some("127.0.0.1".parse().unwrap()),
        )
        .expect("advertise plan should build ServiceInfo")
        .expect("register_service=true should yield Some(ServiceInfo)");

        let proto_version = parse_proto_hint(service_info.get_property_val_str(TXT_KEY_PROTO));
        let caps_raw = service_info
            .get_property_val_str(TXT_KEY_CAPS)
            .unwrap_or("");
        // publish 端写入的是裸 token 列表（无 `caps=` 前缀），parse 端直接按逗号切分即可。
        let capabilities = parse_caps_hint(Some(caps_raw));

        assert_eq!(proto_version, PROTOCOL_VERSION_V1);
        assert!(capabilities.contains(&"errors.envelope.v1".to_string()));
    }
}

// AppState::device_name 便捷访问定义在 state.rs（与类型定义同模块，组织更清晰）。
