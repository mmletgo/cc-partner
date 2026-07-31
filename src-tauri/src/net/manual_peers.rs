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
//!     - `start_manual_peer_probe`：spawn 后台 task，每 15s 一个周期：拉 Tailscale peers + 读 manual_peers
//!       → 逐个 health 探测 → upsert `state.devices` → 用「静态 ∪ 在线 cc-partner peer IP」刷新 overlay 集合。
//!     - 连续 3 次失败的候选移除其 device 条目（防抖动）；Tailscale 非 cc-partner 节点不入表不计数。

use crate::config::ManualPeerConfig;
use crate::models::device::Device;
use crate::net::lan_guard::{classify_peer_ip, LanPeerScope};
use crate::net::routes::health::HealthResponse;
use crate::state::AppState;
use chrono::Utc;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 探测周期（秒）。对端上下线由周期 health 驱动（与 mDNS 事件驱动不同）。
const PROBE_INTERVAL_SECS: u64 = 15;
/// 连续失败多少次后从 `state.devices` 移除条目（防瞬时网络抖动反复增删）。
const FAILURE_THRESHOLD: u32 = 3;
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
        let mut fail_counts: HashMap<String, u32> = HashMap::new();
        loop {
            probe_cycle(&state_clone, &mut fail_counts).await;
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

/// 单轮探测：合并 Tailscale peers + manual_peers，逐个 health 探测，刷新 devices 与 overlay 集合。
async fn probe_cycle(state: &AppState, fail_counts: &mut HashMap<String, u32>) {
    let my_device_id = state.device_id.as_ref().clone();

    // 候选 = manual_peers（config）+ Tailscale peers（自动）。每条 (host 字符串, 端口)。
    let mut candidates: Vec<(String, u16)> = Vec::new();
    let manual: Vec<ManualPeerConfig> = state
        .config
        .read()
        .expect("config 读锁中毒")
        .manual_peers
        .clone();
    for p in &manual {
        candidates.push((p.host.clone(), p.port));
    }
    let ts_peers = tailscale_peers().await;
    for (ip, _hostname) in &ts_peers {
        candidates.push((ip.to_string(), TAILSCALE_PROBE_PORT));
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
        match state.peer_client.health_info(&base_url).await {
            Ok(health) if health.ok && health.device_id == *my_device_id => {
                // 对端回环是自己（如配了本机地址），不计入。
            }
            Ok(health) if health.ok => {
                upsert_device(state, &host, port, health);
                if let Ok(ip) = host.parse::<IpAddr>() {
                    trusted.insert(ip);
                }
                fail_counts.remove(&base_url);
            }
            _ => {
                let count = fail_counts.entry(base_url.clone()).or_insert(0);
                *count += 1;
                if *count >= FAILURE_THRESHOLD {
                    remove_device_by_host(state, &host);
                    fail_counts.remove(&base_url);
                }
            }
        }
    }

    let n = trusted.len();
    *state
        .overlay_trusted_ips
        .write()
        .expect("overlay_trusted_ips 写锁中毒") = trusted;
    let _ = n; // 数量已在周期日志体现，避免高频日志噪音此处省略打印
}

/// 移除 host 匹配的对端 device 条目（连续失败阈值触发）。
fn remove_device_by_host(state: &AppState, host: &str) {
    let mut removed_ids = Vec::new();
    let mut devices = state.devices.write().expect("devices 写锁中毒");
    devices.retain(|_id, d| {
        let keep = d.host != host;
        if !keep {
            removed_ids.push(d.id.clone());
        }
        keep
    });
    drop(devices);
    if !removed_ids.is_empty() {
        tracing::info!("overlay 对端连续失败移除: {host} (ids={removed_ids:?})");
    }
}

/// 把 health 响应 + host/port 构造为 Device 并 upsert 进 state.devices。
fn upsert_device(state: &AppState, host: &str, port: u16, health: HealthResponse) {
    let device = Device {
        id: health.device_id.clone(),
        name: health.device_name.clone(),
        host: host.to_string(),
        port: health.http_port,
        last_seen: Utc::now(),
        online: true,
        proto_version: health.protocol_version,
        capabilities: health.capabilities,
    };
    let id = device.id.clone();
    let name = device.name.clone();
    state
        .devices
        .write()
        .expect("devices 写锁中毒")
        .insert(id.clone(), device);
    tracing::info!("overlay 发现对端: {name} (id={id}, {host}:{port})");
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

    #[test]
    fn constants_are_sensible() {
        assert!(FAILURE_THRESHOLD >= 1);
        assert!(PROBE_INTERVAL_SECS >= 5);
        assert!(TAILSCALE_PROBE_PORT > 0);
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
}
