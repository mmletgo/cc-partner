//! net/manual_peers.rs — 跨子网/VPN 对端发现（绕过 mDNS），含手动配置 + Tailscale 自动发现
//!
//! Business Logic（为什么需要这个模块）:
//!     cc-partner 设备发现纯靠 mDNS（`net::discovery`），仅覆盖同子网 LAN。跨 VPN/不同子网
//!     （如 Tailscale CGNAT 100.64/10）的对端无法被 mDNS 看到，且 LAN 信任门闸默认拒 CGNAT。
//!     本模块提供两条互补的 overlay 发现源：
//!       1) **Tailscale 自动发现**（首选，免配置）：`tailscale status --json` 列出同 Tailnet 全部 peer，
//!          逐个探测默认端口是否有 cc-partner health，命中即入 `state.devices`。新节点加入 Tailnet
//!          自动被发现，**无需写 config**。
//!       2) **manual_peers**（显式覆盖，用于非 Tailscale 场景如 ZeroTier/跨子网 LAN）：config.json
//!          配 `manual_peers: [{host,port}]`，同样探测入表。
//!     两源发现的 cc-partner peer 的 IP 都加入 `AppState.overlay_trusted_ips`（精确 IP 白名单），
//!     让 `lan_socket_gate` / `browser_request_guard` 放行 CGNAT/overlay。这是 opt-in 最小权限路径，
//!     不改默认 CGNAT 拒绝策略，也非身份认证。
//!
//! Code Logic（这个模块做什么）:
//!     - `populate_overlay_trusted_ips`：启动时用静态集合（manual_peers IP ∪ 本机 overlay 接口 IP）播种。
//!     - `start_manual_peer_probe`：spawn 后台 task，每 15s 一个周期：候选 = manual_peers ∪ Tailscale
//!       peers ∪ devices 表已有地址行（mDNS 发现的地址也会被补测 RTT）→ 每个地址独立 health 探测
//!       并以 `Instant` 计 RTT → 成功 `report_health`（同 device_id 多地址归并为一 Device 多行 +
//!       延迟择优写回活跃 host/port）、失败行级 fail_count+1 → 用「静态 ∪ 在线 cc-partner peer IP」
//!       刷新 overlay 集合。
//!     - 连续 3 次失败的地址行被移除，全部行移除才删 device 条目（防抖动）；Tailscale 非
//!       cc-partner 节点不入表不计数。

use crate::config::ManualPeerConfig;
use crate::models::device::{report_device_failure, upsert_device_health, DeviceHealthMeta};
use crate::net::lan_guard::{classify_peer_ip, LanPeerScope};
use crate::net::routes::health::HealthResponse;
use crate::state::AppState;
use serde_json::Value;
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// 探测周期（秒）。对端上下线由周期 health 驱动（与 mDNS 事件驱动不同）。
const PROBE_INTERVAL_SECS: u64 = 15;
/// Tailscale 自动发现使用的探测端口（cc-partner 首选默认端口；非默认端口的 peer 走 manual_peers）。
const TAILSCALE_PROBE_PORT: u16 = 62116;
/// `tailscale status --json` 调用硬超时（秒），避免 daemon 异常时阻塞探测循环。
const TAILSCALE_TIMEOUT_SECS: u64 = 4;

/// 填充 `AppState.overlay_trusted_ips` 的静态播种集合（启动时调用一次）。
///
/// Business Logic: 启动后首个探测周期完成前门闸也要有最小可用集合：manual_peers 配置 IP
/// + 本机 overlay 接口 IP。后续周期由 `probe_cycle` 用「静态 ∪ 在线 cc-partner peer IP」覆盖刷新。
pub fn populate_overlay_trusted_ips(state: &AppState) {
    let count = static_overlay_ips(state).len();
    *state
        .overlay_trusted_ips
        .write()
        .expect("overlay_trusted_ips 写锁中毒") = static_overlay_ips(state);
    tracing::info!("overlay 信任 IP 集合已播种: {count} 项");
}

/// 计算静态 overlay 信任 IP：manual_peers 解析 IP ∪ 本机非默认作用域接口 IP。
fn static_overlay_ips(state: &AppState) -> HashSet<IpAddr> {
    let mut set: HashSet<IpAddr> = HashSet::new();

    let peers: Vec<ManualPeerConfig> = state
        .config
        .read()
        .expect("config 读锁中毒")
        .manual_peers
        .clone();
    for peer in &peers {
        collect_peer_host_ip(&peer.host, &mut set);
    }

    // 本机非默认作用域接口 IP（CGNAT/overlay）。Host 头会是对端连过来的"我方 IP"，必须放行；
    // 仅收录 Denied 段避免与默认 scope 重复。
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            let ip = iface.ip();
            if classify_peer_ip(ip) == LanPeerScope::Denied {
                set.insert(ip);
            }
        }
    }
    set
}

/// 解析 manual_peer 的 host：IP 字面量直接收录；主机名走同步 DNS 取首个结果。
fn collect_peer_host_ip(host: &str, set: &mut HashSet<IpAddr>) {
    if let Ok(ip) = host.parse::<IpAddr>() {
        set.insert(ip);
        return;
    }
    use std::net::ToSocketAddrs;
    if let Ok(mut iter) = (host, 53u16).to_socket_addrs() {
        if let Some(addr) = iter.next() {
            set.insert(addr.ip());
            return;
        }
    }
    tracing::warn!("manual_peers host 无法解析为 IP（已跳过信任放行）: {host}");
}

/// 启动 overlay 对端周期探测循环，返回取消令牌（shutdown 时 cancel）。
///
/// Business Logic: mDNS 发现不到跨子网/VPN 对端；本循环合并 Tailscale 自动发现 + manual_peers
/// 两个源，主动 health 探测后写入 `state.devices`，使下游 sync/workbench/agent-cli 经
/// `Device::base_url()` 正常访问。同时刷新 overlay 信任集合。
pub fn start_manual_peer_probe(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let state_clone = state.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            probe_cycle(&state_clone).await;
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    tracing::info!("overlay 对端探测循环已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(PROBE_INTERVAL_SECS)) => {}
            }
        }
    });
    cancel
}

/// 单轮探测：合并 manual_peers + Tailscale + devices 已有地址行，逐地址测 RTT 探测并归并进表。
///
/// Business Logic: 同一设备（如 LAN IP + Tailscale IP）的多地址必须共存于一个 Device 的多行、
///     各自独立测 RTT，由 `models::device` 的择优逻辑自动选路；不再是整体覆盖互相抖动。
///
/// Code Logic: 候选 = manual_peers ∪ Tailscale peers ∪ devices 表已有地址行（去重）；
///     每个候选以 `Instant` 包住 `health_info` 计 RTT：成功 → `record_probe_success`
///     （行级 upsert + EMA），失败 → `record_probe_failure`（行级计数、达阈值移除）；
///     最后以「静态 ∪ Tailscale peer IP ∪ 在线 cc-partner peer IP」刷新 overlay 信任集合。
async fn probe_cycle(state: &AppState) {
    let my_device_id = state.device_id.as_ref().clone();

    // 候选 = manual_peers（config）+ Tailscale peers（自动）+ devices 表已有地址行
    // （mDNS 发现的地址行由此纳入每轮 RTT 测量）。按 (host, port) 去重保序。
    let mut candidates: Vec<(String, u16)> = Vec::new();
    let mut seen: HashSet<(String, u16)> = HashSet::new();
    let mut push_candidate = |host: String, port: u16, candidates: &mut Vec<(String, u16)>| {
        if seen.insert((host.clone(), port)) {
            candidates.push((host, port));
        }
    };
    let manual: Vec<ManualPeerConfig> = state
        .config
        .read()
        .expect("config 读锁中毒")
        .manual_peers
        .clone();
    for p in &manual {
        push_candidate(p.host.clone(), p.port, &mut candidates);
    }
    let ts_peers = tailscale_peers().await;
    for (ip, _hostname) in &ts_peers {
        push_candidate(ip.to_string(), TAILSCALE_PROBE_PORT, &mut candidates);
    }
    for device in state.devices.read().expect("devices 读锁中毒").values() {
        for addr in &device.addresses {
            push_candidate(addr.host.clone(), addr.port, &mut candidates);
        }
    }
    if candidates.is_empty() {
        return;
    }

    // overlay 信任 = 静态（manual_peers IP ∪ 本机 overlay IP）∪ 全部 Tailscale peer IP ∪ 在线 cc-partner peer IP。
    let mut trusted = static_overlay_ips(state);
    // Tailscale peer 预信任（Tailnet 成员即受信 overlay）：破解冷启动互锁——否则两端各自只在
    // 对端 health 成功后才把对方 IP 加进 overlay，而 health 又要求对方门闸先放行自己，互相 403
    // 死锁、谁也发现不了谁。同 Tailnet 的节点由用户自己加入，视同受信 LAN，预放行其 IP 让双方
    // probe 能落地、随即互相发现；device 条目仍只在 health 成功（确属 cc-partner 实例）时入表。
    for (ip, _hostname) in &ts_peers {
        trusted.insert(*ip);
    }

    for (host, port) in candidates {
        let base_url = format!("http://{host}:{port}");
        // Instant 计时包住 health_info：成功路径的耗时即该地址行的实测 RTT。
        let started = Instant::now();
        let result = state.peer_client.health_info(&base_url).await;
        let rtt_ms = started.elapsed().as_secs_f64() * 1000.0;
        match result {
            Ok(health) if health.ok && health.device_id == *my_device_id => {
                // 对端回环是自己（如配了本机地址），不计入。
            }
            Ok(health) if health.ok => {
                record_probe_success(state, &host, health, rtt_ms);
                if let Ok(ip) = host.parse::<IpAddr>() {
                    trusted.insert(ip);
                }
            }
            _ => {
                record_probe_failure(state, &host, port);
            }
        }
    }

    *state
        .overlay_trusted_ips
        .write()
        .expect("overlay_trusted_ips 写锁中毒") = trusted;
}

/// 登记一次探测成功：行级 upsert 地址行（EMA RTT + 元数据刷新）并触发延迟择优。
///
/// Business Logic: 探测循环与 mDNS 共用 `state.devices`；成功探测按 device_id 归并——
///     同一设备的第二个地址自然成为同一 Device 的第二行，由 `select_active` 择优，
///     不再整体覆盖。
///
/// Code Logic: 行端口取 health 报告的实际监听端口（对端端口被占自动 +1 后的权威值）；
///     RTT 以 Some 传入走 EMA。新设备/新地址行打 info 日志，例行刷新降为 debug。
fn record_probe_success(state: &AppState, host: &str, health: HealthResponse, rtt_ms: f64) {
    let port = health.http_port;
    let (existed, had_row) = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        let entry = devices.get(&health.device_id);
        (
            entry.is_some(),
            entry
                .map(|d| d.addresses.iter().any(|a| a.host == host))
                .unwrap_or(false),
        )
    };
    {
        let mut devices = state.devices.write().expect("devices 写锁中毒");
        upsert_device_health(
            &mut devices,
            &health.device_id,
            host,
            port,
            Some(rtt_ms),
            DeviceHealthMeta {
                name: &health.device_name,
                proto_version: health.protocol_version,
                capabilities: &health.capabilities,
            },
        );
    }
    if !existed {
        tracing::info!(
            "overlay 发现对端: {} (id={}, {host}:{port}, rtt={rtt_ms:.1}ms)",
            health.device_name,
            health.device_id
        );
    } else if !had_row {
        tracing::info!(
            "overlay 对端新增地址: {} (id={}, {host}:{port}, rtt={rtt_ms:.1}ms)",
            health.device_name,
            health.device_id
        );
    } else {
        tracing::debug!(
            "overlay 对端地址刷新: {} (id={}, {host}:{port}, rtt={rtt_ms:.1}ms)",
            health.device_name,
            health.device_id
        );
    }
}

/// 登记一次探测失败：行级 fail_count+1，达阈值移除该行；全部行移除才删除 Device 条目。
///
/// Business Logic: 失败按（host, port）行级归因——同 host 探测未开 cc-partner 的端口
///     （如 Tailscale 扫到非默认端口）不得惩罚同 host 的健康地址行；设备只有全部地址
///     行都被移除才从表里删除（防瞬时抖动反复增删）。
///
/// Code Logic: 委托 `models::device::report_device_failure`（持有 devices 写锁）；
///     返回 Some(device_id) 表示该设备行全空、条目已被删除，打 info 日志。
fn record_probe_failure(state: &AppState, host: &str, port: u16) {
    let mut devices = state.devices.write().expect("devices 写锁中毒");
    if let Some(device_id) = report_device_failure(&mut devices, host, port) {
        drop(devices);
        tracing::info!("overlay 对端全部地址失败，移除条目: {host}:{port} (id={device_id})");
    }
}

/// 解析 `tailscale status --json` 得到同 Tailnet 的 peer 列表 `(IP, hostname)`。
///
/// Business Logic（为什么查 Tailscale）:
///     mDNS 跨不过 VPN；但 Tailscale 自己知道全部 peer（`tailscale status`）。直接问它即可
///     免配置发现同 Tailnet 的节点，逐个探测 cc-partner health 确认是否为本应用实例。
///
/// Code Logic: 解析失败/无 tailscale 二进制/超时 → 返回空 Vec（graceful，不阻断探测）。
/// 二进制路径经 `resolve_tailscale_binary` 缓存解析。
async fn tailscale_peers() -> Vec<(IpAddr, String)> {
    let bin = match resolve_tailscale_binary() {
        Some(p) => p,
        None => {
            tracing::debug!("tailscale 二进制未找到，跳过自动发现");
            return Vec::new();
        }
    };
    // macOS：`/Applications/Tailscale.app/Contents/MacOS/Tailscale` 是 GUI/CLI 双用二进制。
    // launchd 拉起的 cc-partner 子进程环境里没有 TERM，该二进制会判定为非 CLI 上下文，
    // 走 GUI 启动路径并失败（stderr: "The Tailscale GUI failed to start. (CLIError error 3.)"），
    // 导致 `tailscale status --json` 拿不到 peer。显式注入 TERM=dumb 让它进入 CLI 模式、
    // 连接后台 daemon。Linux `/usr/bin/tailscale` 是纯 CLI，不受影响。
    let output = match tokio::time::timeout(
        Duration::from_secs(TAILSCALE_TIMEOUT_SECS),
        tokio::process::Command::new(&bin)
            .arg("status")
            .arg("--json")
            .env("TERM", "dumb")
            .output(),
    )
    .await
    {
        Ok(Ok(o)) if o.status.success() => {
            TAILSCALE_FAIL_WARNED.store(false, Ordering::SeqCst);
            o.stdout
        }
        Ok(Ok(o)) => {
            warn_tailscale_failure_once(&bin, || {
                let stderr = String::from_utf8_lossy(&o.stderr);
                format!(
                    "exit={:?} stderr={}",
                    o.status.code(),
                    stderr.trim().chars().take(200).collect::<String>()
                )
            });
            return Vec::new();
        }
        Ok(Err(e)) => {
            warn_tailscale_failure_once(&bin, || format!("spawn 失败: {e}"));
            return Vec::new();
        }
        Err(_) => {
            warn_tailscale_failure_once(&bin, || format!("超时 {TAILSCALE_TIMEOUT_SECS}s"));
            return Vec::new();
        }
    };
    parse_tailscale_status(&output)
}

/// 首次 tailscale 调用失败时 WARN 一次（含原因），之后同类失败降级到 DEBUG，避免每 15s 刷屏。
/// 调用成功会重置标志（`tailscale_peers` 成功路径），故 失败-恢复-再失败 会再次 WARN。
static TAILSCALE_FAIL_WARNED: AtomicBool = AtomicBool::new(false);

fn warn_tailscale_failure_once<F: FnOnce() -> String>(bin: &std::path::Path, detail: F) {
    let detail = detail();
    if !TAILSCALE_FAIL_WARNED.swap(true, Ordering::SeqCst) {
        tracing::warn!(
            binary = %bin.display(),
            detail = %detail,
            "tailscale status 失败，overlay 自动发现将仅依赖 manual_peers（此后同类失败降为 debug）"
        );
    } else {
        tracing::debug!(binary = %bin.display(), detail = %detail, "tailscale status 再次失败");
    }
}

/// 解析 `tailscale status --json` 字节流为 peer 列表（纯函数，便于单测）。
fn parse_tailscale_status(bytes: &[u8]) -> Vec<(IpAddr, String)> {
    let val: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(peers) = val.get("Peer").and_then(|p| p.as_object()) {
        for (_key, peer) in peers {
            let ip_str = peer
                .get("TailscaleIPs")
                .and_then(|i| i.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str());
            let host = peer
                .get("HostName")
                .and_then(|h| h.as_str())
                .unwrap_or("tailscale-peer");
            if let Some(ip_str) = ip_str {
                if let Ok(ip) = ip_str.parse::<IpAddr>() {
                    out.push((ip, host.to_string()));
                }
            }
        }
    }
    out
}

/// 解析 tailscale 二进制路径（OnceLock 缓存，进程内只查一次）。
///
/// Code Logic: 先查已知绝对路径（macOS GUI/homebrew、Linux），再扫 PATH；全失败返回 None。
fn resolve_tailscale_binary() -> Option<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            const KNOWN: [&str; 4] = [
                "/usr/local/bin/tailscale",
                "/opt/homebrew/bin/tailscale",
                "/usr/bin/tailscale",
                "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
            ];
            for c in KNOWN {
                if std::fs::metadata(c).is_ok() {
                    return Some(PathBuf::from(c));
                }
            }
            if let Some(path) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path) {
                    let f = dir.join("tailscale");
                    if f.is_file() {
                        return Some(f);
                    }
                }
            }
            None
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::device::{DeviceAddress, FAILURE_THRESHOLD};
    use crate::net::relay_shadow_probe::test_support::build_test_state;

    /// 构造指定设备身份的 health 响应（默认探测端口）。
    fn health_of(device_id: &str, http_port: u16) -> HealthResponse {
        HealthResponse {
            ok: true,
            device_id: device_id.to_string(),
            device_name: format!("device-{device_id}"),
            http_port,
            ts: 0,
            protocol_version: 1,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn constants_are_sensible() {
        const _: () = assert!(FAILURE_THRESHOLD >= 1);
        const _: () = assert!(PROBE_INTERVAL_SECS >= 5);
        const _: () = assert!(TAILSCALE_PROBE_PORT > 0);
    }

    #[test]
    fn parse_tailscale_status_extracts_peer_ips() {
        let json = br#"{
            "Self": {"TailscaleIPs": ["100.110.254.81"], "HostName": "me"},
            "Peer": {
                "100.72.52.63": {"TailscaleIPs": ["100.72.52.63", "fd7a::1"], "HostName": "power-vpn", "Online": true},
                "100.64.0.5": {"TailscaleIPs": ["100.64.0.5"], "HostName": "other", "Online": false}
            }
        }"#;
        let mut peers = parse_tailscale_status(json);
        peers.sort();
        assert_eq!(peers.len(), 2);
        assert!(peers.contains(&(
            "100.72.52.63".parse::<IpAddr>().unwrap(),
            "power-vpn".into()
        )));
        assert!(peers.contains(&("100.64.0.5".parse::<IpAddr>().unwrap(), "other".into())));
    }

    #[test]
    fn parse_tailscale_status_tolerates_garbage() {
        assert!(parse_tailscale_status(b"not json").is_empty());
        assert!(parse_tailscale_status(b"{}").is_empty());
        // 无 TailscaleIPs 的 peer 被跳过，不 panic。
        let with_bad = br#"{"Peer":{"x":{"HostName":"h"}}}"#;
        assert!(parse_tailscale_status(with_bad).is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多地址探测归并是本特性核心：同一 device_id 的两个地址（LAN + Tailscale）
    ///     探测成功必须归并为一个 Device 的两行，活跃地址自动择优到低 RTT 行，
    ///     而不是互相整体覆盖抖动。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造测试 AppState，先后以 0.5ms/5ms RTT 上报同 device_id 的两个 host，
    ///     断言表内只有一个 Device、两行地址、活跃 host/port 为 LAN 行且 online。
    #[tokio::test]
    async fn probe_success_merges_two_addresses_into_one_device() {
        let state = build_test_state("self-a", Vec::new()).await;
        record_probe_success(&state, "100.113.214.69", health_of("peer-1", 62116), 5.0);
        record_probe_success(&state, "192.168.6.17", health_of("peer-1", 62116), 0.5);

        let devices = state.devices.read().unwrap();
        assert_eq!(devices.len(), 1, "同 device_id 多地址必须归并为一个 Device");
        let device = devices.get("peer-1").unwrap();
        assert_eq!(device.addresses.len(), 2);
        assert_eq!(
            device.host, "192.168.6.17",
            "活跃地址应择优到低 RTT 的 LAN 行"
        );
        assert_eq!(device.port, 62116);
        assert!(device.online);
        let hosts: Vec<&str> = device.addresses.iter().map(|a| a.host.as_str()).collect();
        assert!(hosts.contains(&"192.168.6.17") && hosts.contains(&"100.113.214.69"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     失败必须行级隔离：同 device_id 的一个地址连续失败达阈值只移除该行，
    ///     其余健康行保留并接管活跃地址；全部行移除才删条目。
    ///
    /// Code Logic（这个测试做什么）:
    ///     双行设备对 LAN 行连续失败 3 次 → 条目保留、活跃切 Tailscale 行；
    ///     再对 Tailscale 行连续失败 3 次 → 条目被删除。
    #[tokio::test]
    async fn probe_failure_removes_rows_level_by_level_then_entry() {
        let state = build_test_state("self-a", Vec::new()).await;
        record_probe_success(&state, "192.168.6.17", health_of("peer-1", 62116), 0.5);
        record_probe_success(&state, "100.113.214.69", health_of("peer-1", 62116), 5.0);

        for _ in 0..FAILURE_THRESHOLD {
            record_probe_failure(&state, "192.168.6.17", 62116);
        }
        {
            let devices = state.devices.read().unwrap();
            let device = devices.get("peer-1").expect("仍有健康行，条目应保留");
            assert_eq!(device.addresses.len(), 1);
            assert_eq!(device.host, "100.113.214.69", "活跃应切到剩余健康行");
            assert!(device.online);
        }

        for _ in 0..FAILURE_THRESHOLD {
            record_probe_failure(&state, "100.113.214.69", 62116);
        }
        let devices = state.devices.read().unwrap();
        assert!(devices.get("peer-1").is_none(), "全部行移除后条目应删除");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     探测候选端口与已有地址行同 host 不同 port（如 manual_peers 配了 ghost 端口、
    ///     或 Tailscale 扫到非 cc-partner 端口）时，失败不得惩罚同 host 的健康地址行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     健康行 (LAN, 62116) 在位，对 (LAN, 9999) 连续失败 3 次：
    ///     断言行数不变、活跃不变、online 保持。
    #[tokio::test]
    async fn probe_failure_on_other_port_does_not_punish_healthy_row() {
        let state = build_test_state("self-a", Vec::new()).await;
        record_probe_success(&state, "192.168.6.17", health_of("peer-1", 62116), 0.5);

        for _ in 0..FAILURE_THRESHOLD {
            record_probe_failure(&state, "192.168.6.17", 9999);
        }
        let devices = state.devices.read().unwrap();
        let device = devices.get("peer-1").expect("ghost 端口失败不应影响条目");
        assert_eq!(device.addresses.len(), 1);
        assert_eq!(device.addresses[0].port, 62116);
        assert_eq!(device.host, "192.168.6.17");
        assert!(device.online);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     mDNS 只登记未测量行，RTT 由探测循环补测：候选集必须包含 devices 表已有地址行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     预置一个 rtt=None 的地址行（模拟 mDNS 登记），探测成功后断言该行 rtt 被实测值填充。
    #[tokio::test]
    async fn probe_cycle_measures_existing_unmeasured_address_rows() {
        let state = build_test_state("self-a", Vec::new()).await;
        {
            let mut devices = state.devices.write().unwrap();
            devices.insert(
                "peer-1".to_string(),
                crate::models::device::Device::new(
                    "peer-1".to_string(),
                    "device-peer-1".to_string(),
                    "192.168.6.17".to_string(),
                    62116,
                    1,
                    Vec::new(),
                ),
            );
            let device = devices.get_mut("peer-1").unwrap();
            device.addresses[0].rtt_ms = None;
        }
        record_probe_success(&state, "192.168.6.17", health_of("peer-1", 62116), 1.5);

        let devices = state.devices.read().unwrap();
        let device = devices.get("peer-1").unwrap();
        let rtt = device.addresses[0]
            .rtt_ms
            .expect("探测成功后应写入实测 RTT");
        assert!((rtt - 1.5).abs() < 1e-9, "新行 rtt 应取测量值，实际 {rtt}");
        assert!(matches!(
            device.addresses[0],
            DeviceAddress { fail_count: 0, .. }
        ));
    }
}
