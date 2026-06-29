//! mobile — 移动端局域网访问入口信息
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要展示手机可扫码访问的局域网 `/mobile` 地址。localhost/loopback 地址只能被本机访问，
//!     返回给手机会造成二维码不可用，因此必须只输出真实局域网候选地址。
//!
//! Code Logic（这个模块做什么）:
//!     从 AppConfig 取设备名，结合 HTTP server 实际端口和候选 IP 列表，过滤 loopback/空值后
//!     生成 camelCase DTO，供 axum HTTP API 返回给桌面前端。

use crate::config::AppConfig;
use serde::Serialize;
use std::collections::HashSet;
use std::net::IpAddr;

/// 移动端访问入口信息 DTO。
///
/// Business Logic（为什么需要这个结构）:
///     桌面端需要设备名、实际 HTTP 端口和可供手机访问的 URL 列表来渲染链接与二维码。
///
/// Code Logic（这个结构做什么）:
///     使用 camelCase 序列化给前端；测试中通过 PartialEq/Eq 直接比较期望输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessInfoDto {
    pub device_name: String,
    pub port: u16,
    pub urls: Vec<String>,
}

/// 构建移动端访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     手机访问必须使用桌面所在局域网 IP 和实际监听端口，不能展示本机 loopback 地址。
///
/// Code Logic（这个函数做什么）:
///     接收配置、实际端口和候选 IP 字符串列表；过滤空白、localhost 与 loopback IP 后，
///     按 `http://<host>:<port>/mobile` 生成 URL，并保留设备名与端口。
pub fn build_mobile_access_info(
    config: &AppConfig,
    port: u16,
    candidate_ips: Vec<String>,
) -> MobileAccessInfoDto {
    let mut seen = HashSet::new();
    let urls = candidate_ips
        .into_iter()
        .filter_map(|candidate| normalize_mobile_host(&candidate))
        .filter(|host| seen.insert(host.clone()))
        .map(|host| format!("http://{}:{port}/mobile", format_url_host(&host)))
        .collect();

    MobileAccessInfoDto {
        device_name: config.device_name.clone(),
        port,
        urls,
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
    use super::{build_mobile_access_info, MobileAccessInfoDto};
    use crate::config::AppConfig;

    /// Business Logic（为什么需要这个测试）:
    ///     桌面端需要把移动端访问地址展示为链接和二维码，返回 localhost/loopback 会导致手机无法访问。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造包含 localhost、127.0.0.1 和局域网 IP 的候选列表，断言只保留局域网 `/mobile` URL，
    ///     同时保留设备名和实际 HTTP 端口。
    #[test]
    fn access_info_filters_loopback_urls() {
        let config = AppConfig {
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
            github_trending: Default::default(),
        };

        let info = build_mobile_access_info(
            &config,
            14203,
            vec![
                "127.0.0.1".to_string(),
                "localhost".to_string(),
                "192.168.1.23".to_string(),
            ],
        );

        assert_eq!(
            info,
            MobileAccessInfoDto {
                device_name: "Hans Mac".to_string(),
                port: 14203,
                urls: vec!["http://192.168.1.23:14203/mobile".to_string()],
            }
        );
    }
}
