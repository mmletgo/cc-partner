//! mobile — 移动端局域网访问入口信息
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要展示手机可扫码访问的局域网 `/mobile` 地址。localhost/loopback 地址只能被本机访问，
//!     返回给手机会造成二维码不可用，因此必须只输出真实局域网候选地址；多网段机器还需结构化
//!     entries 供前端芯片切换，并保持 urls 与 entries 同序兼容旧消费者。
//!
//! Code Logic（这个模块做什么）:
//!     从 AppConfig 取设备名，结合 HTTP server 实际端口与候选 host/role 列表，过滤 loopback/空值、
//!     去重后生成 entries（含 isDefault/role）与同序 urls 的 camelCase DTO。

use crate::config::AppConfig;
use crate::net::discovery::{
    list_mobile_access_candidates, local_lan_ip, MobileAccessCandidate as DiscoveryCandidate,
};
use serde::Serialize;
use std::collections::HashSet;
use std::net::IpAddr;

/// 移动端访问入口候选（构建 DTO 前的中间结构）。
///
/// Business Logic（为什么需要这个结构）:
///     多网卡场景下每个候选 IP 可能带 wifi/wired 角色与接口名，构建 entries 时需要这些字段。
///
/// Code Logic（这个结构做什么）:
///     持有 host、可选角色与接口名；role 使用本模块 `MobileAccessRole`，由调用方从 discovery
///     的字符串标签映射而来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MobileAccessCandidate {
    pub host: String,
    pub role: Option<MobileAccessRole>,
    pub ifa_name: String,
}

/// 移动端访问入口角色（Wi‑Fi / 有线）。
///
/// Business Logic（为什么需要这个枚举）:
///     前端芯片在可识别时显示「Wi‑Fi / 有线 · IP」，帮助用户选择手机所在网段。
///
/// Code Logic（这个枚举做什么）:
///     camelCase 序列化为 `wifi` / `wired`；无法推断时为 None，不额外输出「其他」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MobileAccessRole {
    Wifi,
    Wired,
}

/// 单条移动端访问入口 DTO。
///
/// Business Logic（为什么需要这个结构）:
///     前端需要按条目渲染 URL、复制、二维码与网段芯片，并标记默认出站项。
///
/// Code Logic（这个结构做什么）:
///     camelCase 字段：id/url/host/role/isDefault；role 为空时跳过序列化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessEntryDto {
    pub id: String,
    pub url: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<MobileAccessRole>,
    pub is_default: bool,
}

/// 移动端访问入口信息 DTO。
///
/// Business Logic（为什么需要这个结构）:
///     桌面端需要设备名、实际 HTTP 端口、结构化 entries 以及兼容字段 urls 来渲染链接与二维码。
///
/// Code Logic（这个结构做什么）:
///     使用 camelCase 序列化给前端；`urls` 必须与 `entries[].url` 同序派生。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessInfoDto {
    pub device_name: String,
    pub port: u16,
    pub urls: Vec<String>,
    pub entries: Vec<MobileAccessEntryDto>,
}

/// 从配置与实际端口组装多网段移动端访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     `get_mobile_access_info` 与 `GET /api/mobile/access-info` 必须产出同一份多网段
///     access-info，避免 command/route 两处各自枚举网卡或 fallback 漂移。
///
/// Code Logic（这个函数做什么）:
///     调用 `list_mobile_access_candidates` 枚举候选，将 discovery 的 `"wifi"|"wired"`
///     角色标签映射为 `MobileAccessRole`；候选为空时回退 `local_lan_ip()` 单 host；
///     `default_host` 始终取自 `local_lan_ip()`，再交给 `build_mobile_access_info`。
pub fn mobile_access_info_from_state(config: &AppConfig, port: u16) -> MobileAccessInfoDto {
    let default_host = local_lan_ip().map(|ip| ip.to_string());
    let mut candidates: Vec<MobileAccessCandidate> = list_mobile_access_candidates()
        .into_iter()
        .map(|c: DiscoveryCandidate| MobileAccessCandidate {
            host: c.host,
            role: match c.role {
                Some("wifi") => Some(MobileAccessRole::Wifi),
                Some("wired") => Some(MobileAccessRole::Wired),
                _ => None,
            },
            ifa_name: c.ifa_name,
        })
        .collect();

    if candidates.is_empty() {
        if let Some(ref ip) = default_host {
            candidates.push(MobileAccessCandidate {
                host: ip.clone(),
                role: None,
                ifa_name: String::new(),
            });
        }
    }

    build_mobile_access_info(config, port, candidates, default_host.as_deref())
}

/// 构建移动端访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     手机访问必须使用桌面所在局域网 IP 和实际监听端口，不能展示本机 loopback 地址；
///     多网段时需结构化 entries 并标记默认出站 host，且 urls 与 entries 同序兼容旧前端。
///
/// Code Logic（这个函数做什么）:
///     过滤/归一化 candidate.host，按 host 去重；生成 url/id/role/is_default；
///     排序：is_default desc，再 host asc；urls 由 entries 映射；default_host 归一化后仅匹配一次。
pub fn build_mobile_access_info(
    config: &AppConfig,
    port: u16,
    candidates: Vec<MobileAccessCandidate>,
    default_host: Option<&str>,
) -> MobileAccessInfoDto {
    let default_normalized = default_host.and_then(normalize_mobile_host);
    let mut seen = HashSet::new();
    let mut entries: Vec<MobileAccessEntryDto> = Vec::new();

    for candidate in candidates {
        let Some(host) = normalize_mobile_host(&candidate.host) else {
            continue;
        };
        if !seen.insert(host.clone()) {
            continue;
        }
        let url = format!("http://{}:{port}/mobile", format_url_host(&host));
        let is_default = default_normalized.as_ref().is_some_and(|d| d == &host);
        entries.push(MobileAccessEntryDto {
            id: host.clone(),
            url,
            host,
            role: candidate.role,
            is_default,
        });
    }

    // 保证最多一个 is_default=true：若多个 host 误标，只保留排序后的第一个
    let mut default_claimed = false;
    for entry in &mut entries {
        if entry.is_default {
            if default_claimed {
                entry.is_default = false;
            } else {
                default_claimed = true;
            }
        }
    }

    entries.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.host.cmp(&b.host))
    });

    let urls = entries.iter().map(|e| e.url.clone()).collect();

    MobileAccessInfoDto {
        device_name: config.device_name.clone(),
        port,
        urls,
        entries,
    }
}

/// 归一化手机可访问候选主机。
///
/// Business Logic（为什么需要这个函数）:
///     localhost、127.0.0.1、::1 和空白地址无法被局域网手机访问，必须在生成二维码前剔除。
///
/// Code Logic（这个函数做什么）:
///     trim 输入；拒绝空值和 localhost；若能解析为 IP，则拒绝 is_loopback 的地址；
///     其它非空主机名原样保留，供未来可解析局域网 hostname 场景使用。
fn normalize_mobile_host(candidate: &str) -> Option<String> {
    let host = candidate.trim();
    if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
        return None;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if ip.is_loopback() {
            return None;
        }
    }

    Some(host.to_string())
}

/// 格式化 URL host 片段。
///
/// Business Logic（为什么需要这个函数）:
///     IPv6 地址直接拼进 URL 会与端口分隔符冲突，未来若候选地址包含 IPv6，二维码 URL 仍需合法。
///
/// Code Logic（这个函数做什么）:
///     解析为 IPv6 时加方括号；IPv4 或普通 hostname 原样返回。
fn format_url_host(host: &str) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]"),
        _ => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_mobile_access_info, MobileAccessCandidate, MobileAccessEntryDto, MobileAccessInfoDto,
        MobileAccessRole,
    };
    use crate::config::AppConfig;

    /// Business Logic（为什么需要这个函数）:
    ///     mobile access info 测试只关心设备名、端口与 URL/entries 过滤结果，需要稳定的最小配置样本。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造带默认字段的 AppConfig，避免每个测试重复初始化与当前断言无关的配置项。
    fn test_config() -> AppConfig {
        AppConfig {
            device_id: "device-1".to_string(),
            device_name: "Hans Mac".to_string(),
            http_port: 0,
            receive_dir: "/tmp".to_string(),
            db_path: "/tmp/cc-partner.db".to_string(),
            screenshot_hotkey: "<cmd>+<shift>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: Default::default(),
            orchestrator: Default::default(),
            github_trending: Default::default(),
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     旧测试与适配器场景只需 host 字符串，需快速构造无角色候选。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用给定 host 填入 MobileAccessCandidate，role/ifa_name 为空默认。
    fn candidate(host: &str) -> MobileAccessCandidate {
        MobileAccessCandidate {
            host: host.to_string(),
            role: None,
            ifa_name: String::new(),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     桌面端需要把移动端访问地址展示为链接和二维码，返回 localhost/loopback 会导致手机无法访问。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造包含 localhost、127.0.0.1 和局域网 IP 的候选列表，断言只保留局域网 entries/urls，
    ///     同时保留设备名和实际 HTTP 端口。
    #[test]
    fn access_info_filters_loopback_urls() {
        let config = test_config();

        let info = build_mobile_access_info(
            &config,
            14203,
            vec![
                candidate("127.0.0.1"),
                candidate("localhost"),
                candidate("192.168.1.23"),
            ],
            Some("192.168.1.23"),
        );

        assert_eq!(
            info,
            MobileAccessInfoDto {
                device_name: "Hans Mac".to_string(),
                port: 14203,
                urls: vec!["http://192.168.1.23:14203/mobile".to_string()],
                entries: vec![MobileAccessEntryDto {
                    id: "192.168.1.23".to_string(),
                    url: "http://192.168.1.23:14203/mobile".to_string(),
                    host: "192.168.1.23".to_string(),
                    role: None,
                    is_default: true,
                }],
            }
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户机器枚举网卡地址时可能拿到空字符串、空白字符串或带空白的有效 LAN IP，二维码不能因此失效。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造空值、纯空白和带空白 IPv4 候选地址，断言空白被过滤且有效地址 trim 后生成 URL。
    #[test]
    fn access_info_trims_candidates_and_filters_blank_hosts() {
        let config = test_config();

        let info = build_mobile_access_info(
            &config,
            14203,
            vec![
                candidate(""),
                candidate("   "),
                candidate("  192.168.1.23  "),
            ],
            None,
        );

        assert_eq!(
            info.urls,
            vec!["http://192.168.1.23:14203/mobile".to_string()]
        );
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].host, "192.168.1.23");
        assert!(!info.entries[0].is_default);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     127.0.0.0/8 的任意地址和 IPv6 loopback 都只能本机访问，展示给手机会得到不可用二维码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造非 127.0.0.1 的 IPv4 loopback、::1 与一个 LAN IP，断言仅 LAN IP 被保留。
    #[test]
    fn access_info_filters_extended_loopback_hosts() {
        let config = test_config();

        let info = build_mobile_access_info(
            &config,
            14203,
            vec![
                candidate("127.12.3.4"),
                candidate("::1"),
                candidate("192.168.1.23"),
            ],
            None,
        );

        assert_eq!(
            info.urls,
            vec!["http://192.168.1.23:14203/mobile".to_string()]
        );
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].host, "192.168.1.23");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一个局域网地址可能从多个枚举来源重复出现，移动端入口列表不应展示重复二维码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造重复 IPv4 候选地址，并混入带空白的同一地址，断言归一化后只输出一次。
    #[test]
    fn access_info_deduplicates_hosts_after_normalization() {
        let config = test_config();

        let info = build_mobile_access_info(
            &config,
            14203,
            vec![
                candidate("192.168.1.23"),
                candidate("192.168.1.23"),
                candidate(" 192.168.1.23 "),
            ],
            Some("192.168.1.23"),
        );

        assert_eq!(
            info.urls,
            vec!["http://192.168.1.23:14203/mobile".to_string()]
        );
        assert_eq!(info.entries.len(), 1);
        assert!(info.entries[0].is_default);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     局域网设备可能只有 IPv6 地址，手机扫码链接需要使用合法 URL host 方括号格式。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造非 loopback IPv6 候选地址，断言输出 URL 使用 `http://[ipv6]:port/mobile` 格式。
    #[test]
    fn access_info_formats_non_loopback_ipv6_hosts() {
        let config = test_config();

        let info = build_mobile_access_info(&config, 14203, vec![candidate("2001:db8::5")], None);

        assert_eq!(
            info.urls,
            vec!["http://[2001:db8::5]:14203/mobile".to_string()]
        );
        assert_eq!(info.entries.len(), 1);
        assert_eq!(info.entries[0].host, "2001:db8::5");
        assert_eq!(info.entries[0].url, "http://[2001:db8::5]:14203/mobile");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     多网段机器应产出结构化 entries：默认出站优先排序，role/host/url 一致，urls 与 entries 同序。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 wired/wifi/loopback 三类候选，指定 wifi 为 default，断言 loopback 过滤、
    ///     default 排前、urls[0] 对齐 entries[0].url。
    #[test]
    fn access_info_builds_entries_marks_default_and_sorts() {
        let config = test_config();
        let candidates = vec![
            MobileAccessCandidate {
                host: "10.0.0.5".into(),
                role: Some(MobileAccessRole::Wired),
                ifa_name: "eth0".into(),
            },
            MobileAccessCandidate {
                host: "192.168.1.23".into(),
                role: Some(MobileAccessRole::Wifi),
                ifa_name: "wlan0".into(),
            },
            MobileAccessCandidate {
                host: "127.0.0.1".into(),
                role: None,
                ifa_name: "lo".into(),
            },
        ];
        let info = build_mobile_access_info(&config, 14203, candidates, Some("192.168.1.23"));
        assert_eq!(info.port, 14203);
        assert_eq!(info.urls.len(), 2);
        assert_eq!(info.entries.len(), 2);
        // default first
        assert_eq!(info.entries[0].host, "192.168.1.23");
        assert!(info.entries[0].is_default);
        assert_eq!(info.entries[0].role, Some(MobileAccessRole::Wifi));
        assert_eq!(info.entries[0].url, "http://192.168.1.23:14203/mobile");
        assert_eq!(info.urls[0], info.entries[0].url);
        assert_eq!(info.entries[1].host, "10.0.0.5");
        assert!(!info.entries[1].is_default);
        assert_eq!(info.entries[1].role, Some(MobileAccessRole::Wired));
        assert_eq!(info.urls[1], info.entries[1].url);
    }
}
