//! net/routes/mobile.rs — 移动端局域网访问入口信息 API
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要从后端获取当前设备在局域网内的 `/mobile` 可访问地址，用于生成链接和二维码。
//!
//! Code Logic（这个模块做什么）:
//!     GET /api/mobile/access-info 从 AppState 读取设备配置和实际 HTTP 端口，
//!     调用 discovery 的局域网 IP 探测结果组装 MobileAccessInfoDto。

use crate::mobile::{build_mobile_access_info, MobileAccessCandidate, MobileAccessInfoDto};
use crate::net::discovery::local_lan_ip;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use std::sync::atomic::Ordering;

/// GET /api/mobile/access-info：返回移动端局域网访问入口信息。
///
/// Business Logic（为什么需要这个函数）:
///     前端需要一个可信后端 API 获取手机可访问 URL，避免前端误用 localhost 或浏览器地址栏端口。
///
/// Code Logic（这个函数做什么）:
///     从共享状态复制配置和实际监听端口；当前仍用 local_lan_ip 单候选薄适配到新 DTO 签名，
///     多网段候选列表接线见后续 Task 3；交给 mobile 模块统一过滤并生成 DTO 后以 JSON 返回。
pub async fn access_info(State(state): State<AppState>) -> Json<MobileAccessInfoDto> {
    let config = state.config.read().expect("config 读锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let default_host = local_lan_ip().map(|ip| ip.to_string());
    let candidates = default_host
        .as_ref()
        .map(|host| {
            vec![MobileAccessCandidate {
                host: host.clone(),
                role: None,
                ifa_name: String::new(),
            }]
        })
        .unwrap_or_default();

    Json(build_mobile_access_info(
        &config,
        port,
        candidates,
        default_host.as_deref(),
    ))
}
