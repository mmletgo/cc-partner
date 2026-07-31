//! net/lan_guard.rs — 固定 LAN socket peer 范围 + 浏览器 Host/Origin/Content-Type 门禁
//!
//! Business Logic（为什么需要这个模块）:
//!     产品只支持本机与局域网访问。HTTP 业务 API 对合法 loopback/LAN peer 一律无凭据放行，
//!     但对全局可路由、unspecified、multicast、文档保留或无法判定的 peer 必须在 handler 前拒绝。
//!     浏览器侧还需 Host/Origin/Content-Type 与 WebSocket 来源检查，降低 DNS rebinding 与跨站滥用风险，
//!     同时兼容无 Origin 的 native P2P 与 opaque preview iframe 的受限 `Origin: null`。
//!     这些检查是部署边界与请求完整性保护，不是身份鉴权；不得读取代理来源 header，也不发 CORS。
//!
//! Code Logic（这个模块做什么）:
//!     - `LanPeerScope`：内部三类 peer 范围（Loopback / Lan / Denied），不是用户配置或权限模式；
//!     - `classify_peer_ip`：纯函数，规范化 IPv4-mapped IPv6 后按固定地址范围分类；
//!     - `lan_socket_gate`：axum middleware，仅信任 `ConnectInfo<SocketAddr>`，拒绝 Denied/缺失 peer；
//!     - `require_loopback_peer`：backend stop 等本机生命周期接口在 token 比较前强制 loopback；
//!     - `BrowserGuardParams` + `evaluate_browser_request`：Host/Origin/Content-Type 纯判定；
//!     - `browser_request_guard`：axum middleware，从 AppState 读取实际端口与受控 mDNS 名后执行判定；
//!     - `expected_device_id_guard`：可选 `X-Cc-Partner-Expected-Device-Id` 与本机 device_id 绑定（非鉴权）；
//!     - preview proxy 的 `Origin: null` 仅在全局标记为可延期，最终是否放行由 browser_proxy 会话查找决定。

use crate::net::error_response::{P2pError, P2pErrorCode};
use crate::net::request_context::{new_request_id, P2pRequestContext};
use crate::state::AppState;
use crate::workbench::browser_proxy::{
    DESKTOP_BROWSER_PROXY_ROUTE_PREFIX, MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
};
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderName, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// 客户端声明的期望对端 device_id header（可选绑定，非身份鉴权）。
///
/// Business Logic（为什么需要这个常量）:
///     Agent CLI / 显式 `--device` 调用方可在每个 HTTP 请求上声明期望设备；
///     服务端在 header 与本机 `device_id` 不一致时 fail-closed，防 stale peer 映射写错机。
///     缺省 header 时行为不变（LAN 仍无调用者身份校验）。
pub static EXPECTED_DEVICE_ID_HEADER: HeaderName =
    HeaderName::from_static("x-cc-partner-expected-device-id");

/// 内部 socket peer 范围分类。
///
/// Business Logic（为什么需要这个枚举）:
///     业务 API 只区分“支持的本机/LAN”与“不支持的网络来源”；backend stop 额外要求本机 loopback。
///     不引入暴露模式、只读模式或设备权限分级。
///
/// Code Logic（这个枚举做什么）:
///     `Loopback` 与 `Lan` 对业务 API 等价放行；`Denied` 在 middleware 层以 403 拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanPeerScope {
    /// IPv4 127.0.0.0/8 或 IPv6 ::1。
    Loopback,
    /// RFC1918、IPv4 link-local、IPv6 ULA、IPv6 link-local。
    Lan,
    /// 全局可路由、unspecified、multicast、文档保留或其它不支持地址。
    Denied,
}

/// 根据真实 socket peer IP 分类支持范围。
///
/// Business Logic（为什么需要这个函数）:
///     产品边界由真实 TCP peer 决定；代理 header 不能改写支持网络范围。
///
/// Code Logic（这个函数做什么）:
///     接收 `IpAddr`；若为 IPv4-mapped IPv6 先还原为 IPv4；再按固定范围返回 `LanPeerScope`。
///     允许：IPv4 loopback/private/link-local、IPv6 loopback/ULA/link-local；其余 Denied。
pub fn classify_peer_ip(ip: IpAddr) -> LanPeerScope {
    let ip = normalize_peer_ip(ip);
    match ip {
        IpAddr::V4(v4) => classify_ipv4(v4),
        IpAddr::V6(v6) => classify_ipv6(v6),
    }
}

/// 判断 IP 是否在用户显式配置的 overlay 信任集合（手动对端 IP ∪ 本机 overlay 接口 IP）。
///
/// Business Logic（为什么需要这个函数）:
///     mDNS 仅覆盖同子网 LAN；跨 VPN/不同子网（如 Tailscale CGNAT 100.64/10）对端需 opt-in。
///     用户配置 `manual_peers` 后，`AppState.overlay_trusted_ips` 收集精确 IP，本函数据此放行。
///     这是最小权限的精确 IP 白名单，**非**整段 CGNAT 放开，也**非**身份认证；默认空集合 = 不放行。
///
/// Code Logic（这个函数做什么）:
///     先 `normalize_peer_ip`（IPv4-mapped IPv6 还原），再查集合。
pub fn is_overlay_trusted(ip: IpAddr, overlay: &HashSet<IpAddr>) -> bool {
    overlay.contains(&normalize_peer_ip(ip))
}

/// 请求扩展：overlay 信任 IP 快照（由 `inject_overlay_trust` middleware 注入，供 `lan_socket_gate` 读取）。
///
/// Business Logic（为什么走扩展而非 `from_fn_with_state`）:
///     `lan_socket_gate` 在多处测试 router 里以 `from_fn` 装配；改签名会大面积改测试。
///     用扩展注入使 gate 保持 `(request, next)` 签名，未注入时（默认/测试）行为不变（无 overlay 放行）。
#[derive(Debug, Clone)]
pub struct OverlayTrustSnapshot(pub Arc<HashSet<IpAddr>>);

/// 规范化 peer IP：把 IPv4-mapped IPv6 还原为 IPv4。
///
/// Business Logic（为什么需要这个函数）:
///     dual-stack listener 上对端可能呈现为 `::ffff:192.168.1.1`，必须按 IPv4 私网规则判定。
///
/// Code Logic（这个函数做什么）:
///     对 `Ipv6Addr::to_ipv4_mapped()` 命中的地址返回对应 IPv4，否则原样返回。
fn normalize_peer_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                IpAddr::V4(v4)
            } else {
                IpAddr::V6(v6)
            }
        }
        other => other,
    }
}

/// 分类 IPv4 peer。
///
/// Business Logic（为什么需要这个函数）:
///     IPv4 支持范围固定为 loopback、RFC1918 与 link-local。
///
/// Code Logic（这个函数做什么）:
///     loopback → Loopback；private / link-local → Lan；其余 → Denied。
fn classify_ipv4(ip: Ipv4Addr) -> LanPeerScope {
    if ip.is_loopback() {
        LanPeerScope::Loopback
    } else if ip.is_private() || ip.is_link_local() {
        LanPeerScope::Lan
    } else {
        LanPeerScope::Denied
    }
}

/// 分类 IPv6 peer（已非 IPv4-mapped）。
///
/// Business Logic（为什么需要这个函数）:
///     IPv6 支持范围固定为 loopback、ULA（fc00::/7）与 link-local（fe80::/10）。
///
/// Code Logic（这个函数做什么）:
///     loopback → Loopback；ULA / unicast link-local → Lan；其余 → Denied。
fn classify_ipv6(ip: Ipv6Addr) -> LanPeerScope {
    if ip.is_loopback() {
        LanPeerScope::Loopback
    } else if is_ipv6_ula(ip) || is_ipv6_unicast_link_local(ip) {
        LanPeerScope::Lan
    } else {
        LanPeerScope::Denied
    }
}

/// 判断 IPv6 是否属于 ULA（fc00::/7）。
///
/// Business Logic（为什么需要这个函数）:
///     局域网常见私有 IPv6 使用 ULA，应与 IPv4 私网同样视为支持范围。
///
/// Code Logic（这个函数做什么）:
///     检查最高 7 位是否为 `0b1111110`（首字节 0xfc 或 0xfd）。
fn is_ipv6_ula(ip: Ipv6Addr) -> bool {
    (ip.octets()[0] & 0xfe) == 0xfc
}

/// 判断 IPv6 是否为 unicast link-local（fe80::/10）。
///
/// Business Logic（为什么需要这个函数）:
///     link-local 是局域网链路地址，必须与 RFC1918/ULA 同样视为支持范围；
///     且 MSRV 1.77.2 不能依赖较新的 `Ipv6Addr::is_unicast_link_local`。
///
/// Code Logic（这个函数做什么）:
///     检查最高 10 位是否为 `0b1111111010`（首 16 位与 0xffc0 掩码后等于 0xfe80）。
fn is_ipv6_unicast_link_local(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

/// 要求 peer 为 loopback（backend stop 生命周期控制）。
///
/// Business Logic（为什么需要这个函数）:
///     `/api/backend/control/stop` 是本机进程生命周期接口，即使持有 control token，
///     也不得从 LAN 对端关闭本机后端。
///
/// Code Logic（这个函数做什么）:
///     用 `classify_peer_ip` 判定；非 Loopback 返回 403 `forbidden` 信封，不读取 token。
pub fn require_loopback_peer(ip: IpAddr, context: &P2pRequestContext) -> Result<(), P2pError> {
    if classify_peer_ip(ip) == LanPeerScope::Loopback {
        Ok(())
    } else {
        Err(denied_peer_error(context))
    }
}

/// 构造 socket 范围拒绝的统一错误信封。
///
/// Business Logic（为什么需要这个函数）:
///     网络范围拒绝不是身份鉴权失败，必须用 403 `forbidden` 而非 401 `unauthorized`。
///
/// Code Logic（这个函数做什么）:
///     返回固定中文文案 + `P2pErrorCode::Forbidden`。
fn denied_peer_error(context: &P2pRequestContext) -> P2pError {
    P2pError::from_code("不支持的网络来源", P2pErrorCode::Forbidden, context)
}

/// 从 request extensions 解析 peer scope；缺失 ConnectInfo 视为 Denied。
///
/// Business Logic（为什么需要这个函数）:
///     生产路径必须带 ConnectInfo；测试或错误装配缺失 peer 时 fail-closed。
///
/// Code Logic（这个函数做什么）:
///     读取 `ConnectInfo<SocketAddr>` 后调用 `classify_peer_ip`；忽略全部 header。
fn peer_scope_from_request(request: &Request<Body>) -> LanPeerScope {
    match request.extensions().get::<ConnectInfo<SocketAddr>>() {
        Some(ConnectInfo(addr)) => classify_peer_ip(addr.ip()),
        None => LanPeerScope::Denied,
    }
}

/// 解析请求上下文；缺失时生成临时 request_id 以便错误信封仍可追踪。
///
/// Business Logic（为什么需要这个函数）:
///     正常生产栈中 request_id middleware 在 gate 外层已注入 context；
///     若装配顺序变化或测试未注入，gate 仍需返回合法信封。
///
/// Code Logic（这个函数做什么）:
///     优先 clone extensions 中的 `P2pRequestContext`，否则生成新 UUID context。
fn request_context_from_request(request: &Request<Body>) -> P2pRequestContext {
    request
        .extensions()
        .get::<P2pRequestContext>()
        .cloned()
        .unwrap_or_else(|| P2pRequestContext {
            request_id: new_request_id(),
        })
}

/// LAN socket peer 门禁中间件。
///
/// Business Logic（为什么需要这个函数）:
///     所有 HTTP/P2P/Mobile 请求必须在进入业务 handler 前完成网络范围检查，
///     且完全忽略 `Forwarded` / `X-Forwarded-For` / `X-Real-IP`。
///
/// Code Logic（这个函数做什么）:
///     仅从 `ConnectInfo<SocketAddr>` 分类 peer；`Denied` 或缺失 peer 立即返回 403 信封；
///     Loopback/Lan 继续 `next.run`。不读写任何代理来源 header。
pub async fn lan_socket_gate(request: Request<Body>, next: Next) -> Response {
    match peer_scope_from_request(&request) {
        LanPeerScope::Loopback | LanPeerScope::Lan => next.run(request).await,
        LanPeerScope::Denied => {
            // overlay opt-in：peer IP 命中注入的 OverlayTrustSnapshot 时放行（精确 IP 白名单）。
            // 未注入扩展（默认/测试）时此处不匹配，行为与原先一致（Denied → 403）。
            if let Some(ConnectInfo(addr)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
                if let Some(snapshot) = request.extensions().get::<OverlayTrustSnapshot>() {
                    if is_overlay_trusted(addr.ip(), &snapshot.0) {
                        return next.run(request).await;
                    }
                }
            }
            let context = request_context_from_request(&request);
            denied_peer_error(&context).into_response()
        }
    }
}

/// 浏览器请求门禁参数（实际端口 + 受控 mDNS hostname）。
///
/// Business Logic（为什么需要这个结构体）:
///     Host 允许列表只能来自本进程真实监听端口与既有 mDNS 命名，绝不能从请求 Host 学习域名。
///
/// Code Logic（这个结构体做什么）:
///     `actual_http_port` 对应 `AppState.actual_http_port`；`controlled_mdns_host` 为
///     `cc-{device_id}.local`（无尾点，比较时同时接受尾点形式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserGuardParams {
    /// HTTP server 实际监听端口。
    pub actual_http_port: u16,
    /// 当前进程受控 mDNS 主机名（无尾点），形如 `cc-{device_id}.local`。
    pub controlled_mdns_host: String,
    /// overlay 信任 IP 快照（手动对端 ∪ 本机 overlay 接口 IP，opt-in）。
    /// 默认空（`browser_guard_params` 构造）= Host 仅允许默认 scope；生产由 `browser_request_guard` 注入。
    pub overlay_trusted_ips: HashSet<IpAddr>,
}

/// 构造浏览器门禁参数。
///
/// Business Logic（为什么需要这个函数）:
///     生产 middleware 与单测需要同一套从 device_id/端口生成允许 Host 的规则。
///
/// Code Logic（这个函数做什么）:
///     生成 `cc-{device_id}.local` 并与实际端口一起封装为 `BrowserGuardParams`。
pub fn browser_guard_params(device_id: &str, actual_http_port: u16) -> BrowserGuardParams {
    BrowserGuardParams {
        actual_http_port,
        controlled_mdns_host: controlled_mdns_hostname(device_id),
        overlay_trusted_ips: HashSet::new(),
    }
}

/// 当前进程受控 mDNS hostname（无尾点）。
///
/// Business Logic（为什么需要这个函数）:
///     discovery 使用 `cc-{device_id}.local.` 发布；浏览器 Host 校验必须与之对齐。
///
/// Code Logic（这个函数做什么）:
///     返回 `cc-{device_id}.local`（比较时再兼容尾点）。
pub fn controlled_mdns_hostname(device_id: &str) -> String {
    format!("cc-{device_id}.local")
}

/// 请求路径类别（仅用于 Origin/Content-Type 规则分支，不是权限模式）。
///
/// Business Logic（为什么需要这个枚举）:
///     preview proxy、普通 API 与 mobile/静态资源对 `Origin: null` 与 Content-Type 的规则不同。
///
/// Code Logic（这个枚举做什么）:
///     按 path 前缀分类：PreviewProxy / OrdinaryApi / MobileOrStatic / Other。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestPathKind {
    /// desktop/mobile browser preview proxy 命名空间。
    PreviewProxy,
    /// 其它 `/api/*` 业务路由。
    OrdinaryApi,
    /// `/mobile` SPA 与 `/assets/*` 静态资源。
    MobileOrStatic,
    /// 其它路径（仍强制 Host，Origin 按保守策略）。
    Other,
}

/// Origin 分类结果。
///
/// Business Logic（为什么需要这个枚举）:
///     native P2P、同源浏览器、opaque iframe 与跨站请求需要不同处理。
///
/// Code Logic（这个枚举做什么）:
///     Missing / SameOrigin / OpaqueNull / CrossOrigin。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OriginClass {
    Missing,
    SameOrigin,
    OpaqueNull,
    CrossOrigin,
}

/// 解析 Host 头为 (hostname, optional_port)。
///
/// Business Logic（为什么需要这个函数）:
///     Host 必须可被严格比对到受控 IP/localhost/mDNS 名与实际端口，防止 DNS rebinding。
///
/// Code Logic（这个函数做什么）:
///     支持 `host`、`host:port`、`[ipv6]`、`[ipv6]:port`；非法形式返回 None。
///     返回的 hostname：IPv6 为无括号字面量，其它为原始 host 标签。
pub fn parse_http_host_header(host: &str) -> Option<(String, Option<u16>)> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if host.starts_with('[') {
        let end = host.find(']')?;
        let ip = &host[1..end];
        if ip.is_empty() || ip.parse::<Ipv6Addr>().is_err() {
            return None;
        }
        let rest = &host[end + 1..];
        if rest.is_empty() {
            return Some((ip.to_string(), None));
        }
        let port_str = rest.strip_prefix(':')?;
        if port_str.is_empty() {
            return None;
        }
        let port: u16 = port_str.parse().ok()?;
        return Some((ip.to_string(), Some(port)));
    }
    // 无括号形式不允许内嵌未转义 IPv6。
    if let Some((name, port_str)) = host.rsplit_once(':') {
        if !name.is_empty() && !name.contains(':') && port_str.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(port) = port_str.parse::<u16>() {
                return Some((name.to_string(), Some(port)));
            }
        }
    }
    if host.contains(':') {
        return None;
    }
    Some((host.to_string(), None))
}

/// 判断 hostname 是否属于受控允许列表。
///
/// Business Logic（为什么需要这个函数）:
///     仅允许支持范围内的字面 IP、localhost 与当前进程 mDNS 名；任意域名一律拒绝。
///
/// Code Logic（这个函数做什么）:
///     去尾点后大小写不敏感比较 localhost/受控名；否则解析 IP 并用 `classify_peer_ip` 要求 Loopback/Lan。
pub fn is_allowed_browser_hostname(hostname: &str, controlled_mdns_host: &str) -> bool {
    let host = normalize_hostname_label(hostname);
    if host.is_empty() {
        return false;
    }
    if host == "localhost" {
        return true;
    }
    let controlled = normalize_hostname_label(controlled_mdns_host);
    if host == controlled {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => matches!(
            classify_peer_ip(ip),
            LanPeerScope::Loopback | LanPeerScope::Lan
        ),
        Err(_) => false,
    }
}

/// 规范化 hostname：去尾点 + ASCII 小写。
///
/// Business Logic（为什么需要这个函数）:
///     Host 允许列表与 Origin 同源比较必须使用同一套主机名规范化，避免浏览器大小写/尾点形态误杀合法同源请求。
///
/// Code Logic（这个函数做什么）:
///     trim → 去掉尾部 `.` → `to_ascii_lowercase`。
fn normalize_hostname_label(hostname: &str) -> String {
    hostname.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// 把 Host 头解析为可与 Origin 比较的规范 authority（host[:port]）。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器 Origin 通常是小写 host 且无尾点；Host 头可能是 `LocalHost:port` / `cc-x.local.:port` / `[::1]:port`。
///     同源判定必须对两者规范化后再比，否则 fail-closed 误杀合法 mobile/浏览器写。
///
/// Code Logic（这个函数做什么）:
///     解析 Host → 主机名小写去尾点；IPv6 用方括号形式；有端口时拼 `host:port`。
///     解析失败返回 None。
pub fn canonical_http_authority(host_header: &str) -> Option<String> {
    let (hostname, port) = parse_http_host_header(host_header)?;
    let host = normalize_hostname_label(&hostname);
    if host.is_empty() {
        return None;
    }
    // IPv6 字面量在 Origin/Host 中必须以 [addr] 形式出现。
    let host_for_url = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]")
    } else {
        host
    };
    match port {
        Some(p) => Some(format!("{host_for_url}:{p}")),
        None => Some(host_for_url),
    }
}

/// 把 Origin 值规范化为可比较的 `scheme://authority`（仅 http/https）。
///
/// Business Logic（为什么需要这个函数）:
///     同源比较不能依赖原始字符串字面量相等；host 大小写与尾点差异必须归一，且不得放宽跨站。
///
/// Code Logic（这个函数做什么）:
///     手工解析 `scheme://authority`（不依赖 url crate，保持 MSRV/依赖面最小）；
///     scheme 仅 http/https（小写）；authority 走 `canonical_http_authority`（host 小写去尾点、IPv6 方括号、端口保留）。
///     含 path/query/userinfo 的 Origin 视为非法。失败返回 None。
fn canonical_origin_value(origin: &str) -> Option<String> {
    let value = origin.trim();
    let (scheme_raw, rest) = value.split_once("://")?;
    let scheme = scheme_raw.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // Origin 规范只有 scheme://host[:port]，不得含 path/query/userinfo。
    if rest.contains('/') || rest.contains('?') || rest.contains('#') || rest.contains('@') {
        return None;
    }
    let authority = canonical_http_authority(rest)?;
    Some(format!("{scheme}://{authority}"))
}

/// 分类 Origin 头。
///
/// Business Logic（为什么需要这个函数）:
///     同源浏览器、native 无 Origin、opaque null 与跨站必须分支处理。
///
/// Code Logic（这个函数做什么）:
///     缺失 → Missing；字面 `null` → OpaqueNull；规范化后 scheme+authority 与
///     `http://{canonical Host}` 相等 → SameOrigin；否则 CrossOrigin。
///     仅接受 http Origin 与本服务 Host 对齐（本服务固定明文 HTTP）。
pub fn classify_request_origin(origin: Option<&str>, host_header: &str) -> OriginClass {
    match origin {
        None => OriginClass::Missing,
        Some(raw) => {
            let value = raw.trim();
            if value.eq_ignore_ascii_case("null") {
                OriginClass::OpaqueNull
            } else if let (Some(origin_canon), Some(host_authority)) = (
                canonical_origin_value(value),
                canonical_http_authority(host_header),
            ) {
                let expected = format!("http://{host_authority}");
                if origin_canon == expected {
                    OriginClass::SameOrigin
                } else {
                    OriginClass::CrossOrigin
                }
            } else {
                OriginClass::CrossOrigin
            }
        }
    }
}

/// 判断非 null Origin 是否与 Host 精确同源（规范化后）。
///
/// Business Logic（为什么需要这个函数）:
///     preview 会话层需要对非 null Origin 做防御纵深：全局 guard 漏挂时仍拒绝跨站。
///
/// Code Logic（这个函数做什么）:
///     复用 `classify_request_origin`；仅 SameOrigin 为 true。
pub fn is_same_origin_with_host(origin: &str, host_header: &str) -> bool {
    matches!(
        classify_request_origin(Some(origin), host_header),
        OriginClass::SameOrigin
    )
}

/// 分类请求路径。
///
/// Business Logic（为什么需要这个函数）:
///     preview proxy 的 null-origin 例外不能扩散到其它 `/api/*`。
///
/// Code Logic（这个函数做什么）:
///     按 desktop/mobile proxy 前缀、`/api/`、`/mobile`、`/assets/` 分类。
fn classify_request_path(path: &str) -> RequestPathKind {
    let path = path.split('?').next().unwrap_or(path);
    if is_preview_proxy_path(path) {
        RequestPathKind::PreviewProxy
    } else if path.starts_with("/api/") || path == "/api" {
        RequestPathKind::OrdinaryApi
    } else if path == "/mobile"
        || path.starts_with("/mobile/")
        || path.starts_with("/assets/")
        || path == "/assets"
    {
        RequestPathKind::MobileOrStatic
    } else {
        RequestPathKind::Other
    }
}

/// 判断 path 是否为 browser preview proxy 命名空间。
///
/// Business Logic（为什么需要这个函数）:
///     仅该命名空间可在会话校验后接受 opaque `Origin: null`。
///
/// Code Logic（这个函数做什么）:
///     匹配 desktop/mobile proxy 前缀（含后续 previewId 与 tail）。
pub fn is_preview_proxy_path(path: &str) -> bool {
    let path = path.split('?').next().unwrap_or(path);
    path.starts_with(&format!("{DESKTOP_BROWSER_PROXY_ROUTE_PREFIX}/"))
        || path.starts_with(&format!("{MOBILE_BROWSER_PROXY_ROUTE_PREFIX}/"))
        || path == DESKTOP_BROWSER_PROXY_ROUTE_PREFIX
        || path == MOBILE_BROWSER_PROXY_ROUTE_PREFIX
}

/// 是否为禁止的 simple-request Content-Type。
///
/// Business Logic（为什么需要这个函数）:
///     恶意网页可用 form-urlencoded/multipart/text/plain 发起“简单请求”绕过 CORS 预检触发写操作。
///
/// Code Logic（这个函数做什么）:
///     取 media type（分号前）小写比较三种禁止类型；缺失 Content-Type 返回 false。
pub fn is_forbidden_simple_write_content_type(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return false;
    };
    let media = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        media.as_str(),
        "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
    )
}

/// 纯函数：校验浏览器/HTTP 请求的 Host、Origin 与普通 API Content-Type。
///
/// Business Logic（为什么需要这个函数）:
///     在 handler 前拒绝恶意 Host、跨站 Origin、普通 API 的 opaque null 与 simple-request 写类型，
///     同时放行 native 无 Origin、同源 mobile 与 preview proxy 的延期 null-origin。
///
/// Code Logic（这个函数做什么）:
///     1) 强制合法 Host；2) 按 path 类别应用 Origin 矩阵；3) 普通 API 非 GET/HEAD 拒绝三种 simple Content-Type。
///     不发送任何 CORS 头。返回 Ok 表示可进入后续中间件/handler（preview null 仍待会话确认）。
pub fn evaluate_browser_request(
    method: &Method,
    path: &str,
    host_header: Option<&str>,
    origin_header: Option<&str>,
    content_type: Option<&str>,
    params: &BrowserGuardParams,
    context: &P2pRequestContext,
) -> Result<(), P2pError> {
    let host = host_header
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| browser_guard_error("缺少 Host", context))?;
    // 复用端口/允许列表逻辑，但错误需带真实 request_id。
    let Some((hostname, port)) = parse_http_host_header(host) else {
        return Err(browser_guard_error("非法 Host", context));
    };
    if !is_allowed_browser_hostname(&hostname, &params.controlled_mdns_host)
        && !hostname
            .parse::<IpAddr>()
            .map(|ip| is_overlay_trusted(ip, &params.overlay_trusted_ips))
            .unwrap_or(false)
    {
        return Err(browser_guard_error("Host 不在支持范围", context));
    }
    let port_ok = match port {
        Some(p) => p == params.actual_http_port,
        None => params.actual_http_port == 80 || params.actual_http_port == 443,
    };
    if !port_ok {
        return Err(browser_guard_error(
            "Host 端口与实际监听端口不一致",
            context,
        ));
    }

    let path_kind = classify_request_path(path);
    let origin_class = classify_request_origin(origin_header, host);
    match (path_kind, origin_class) {
        (_, OriginClass::CrossOrigin) => {
            return Err(browser_guard_error("跨站 Origin 不被允许", context));
        }
        (RequestPathKind::OrdinaryApi, OriginClass::OpaqueNull)
        | (RequestPathKind::Other, OriginClass::OpaqueNull) => {
            return Err(browser_guard_error(
                "普通 API 不接受 opaque Origin",
                context,
            ));
        }
        // preview：null 延期到会话查找；mobile/static：资源加载允许 null；Missing/SameOrigin 全放行。
        (RequestPathKind::PreviewProxy, OriginClass::OpaqueNull)
        | (RequestPathKind::MobileOrStatic, OriginClass::OpaqueNull)
        | (_, OriginClass::Missing)
        | (_, OriginClass::SameOrigin) => {}
    }

    let is_write = method != Method::GET && method != Method::HEAD;
    if is_write
        && path_kind == RequestPathKind::OrdinaryApi
        && is_forbidden_simple_write_content_type(content_type)
    {
        return Err(browser_guard_error(
            "普通 API 写请求不接受该 Content-Type",
            context,
        ));
    }
    Ok(())
}

/// 从 Request 执行浏览器门禁判定。
///
/// Business Logic（为什么需要这个函数）:
///     middleware 与单测需要从 HTTP 头提取字段并复用同一 evaluate 逻辑。
///
/// Code Logic（这个函数做什么）:
///     读取 Host/Origin/Content-Type 与 method/path，调用 `evaluate_browser_request`。
pub fn evaluate_browser_request_from_http(
    request: &Request<Body>,
    params: &BrowserGuardParams,
) -> Result<(), P2pError> {
    let context = request_context_from_request(request);
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    evaluate_browser_request(
        request.method(),
        request.uri().path(),
        host,
        origin,
        content_type,
        params,
        &context,
    )
}

/// 可选期望 device_id 绑定中间件（非身份鉴权）。
///
/// Business Logic（为什么需要这个函数）:
///     调用方若声称目标设备（`X-Cc-Partner-Expected-Device-Id`），本机必须在进入业务 handler
///     前核对是否为本机 `device_id`；错机 fail-closed。缺 header 或空值 → 行为不变（LAN 无鉴权）。
///
/// Code Logic（这个函数做什么）:
///     读 header；非空且与 `state.device_id` 不等 → 409 + code `device_id_mismatch` 信封；
///     否则 `next.run`。
pub async fn expected_device_id_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Err(err) = evaluate_expected_device_id_header(&request, state.device_id.as_str()) {
        return err.into_response();
    }
    next.run(request).await
}

/// 纯函数：校验可选期望 device_id header。
///
/// Business Logic（为什么需要这个函数）:
///     中间件与单测共用同一 fail-closed 规则，避免 mock 与生产漂移。
///
/// Code Logic（这个函数做什么）:
///     header 缺/空 → Ok；trim 后与 actual 不等 → Err(stable device_id_mismatch @ 409)。
pub fn evaluate_expected_device_id_header(
    request: &Request<Body>,
    actual_device_id: &str,
) -> Result<(), P2pError> {
    let Some(raw) = request.headers().get(&EXPECTED_DEVICE_ID_HEADER) else {
        return Ok(());
    };
    let Ok(expected) = raw.to_str() else {
        let context = request_context_from_request(request);
        return Err(P2pError::stable(
            "X-Cc-Partner-Expected-Device-Id header is not valid UTF-8",
            "device_id_mismatch",
            StatusCode::CONFLICT,
            &context,
            false,
        ));
    };
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(());
    }
    if expected != actual_device_id {
        let context = request_context_from_request(request);
        return Err(P2pError::stable(
            format!("device_id mismatch: expected {expected}, this host is {actual_device_id}"),
            "device_id_mismatch",
            StatusCode::CONFLICT,
            &context,
            false,
        ));
    }
    Ok(())
}

/// 浏览器 Host/Origin/Content-Type 门禁中间件（生产路径，读取 AppState）。
///
/// Business Logic（为什么需要这个函数）:
///     所有 HTTP/P2P/Mobile 请求在 socket gate 之后、业务 handler 之前完成浏览器请求完整性检查。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 读取 `actual_http_port` 与 `device_id` 构造参数；失败返回 403 信封；成功 `next.run`。
///     不添加 CORS 响应头。preview 的 `Origin: null` 仅延期放行到 handler，由 browser_proxy 会话确认。
pub async fn browser_request_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let mut params = browser_guard_params(state.device_id.as_str(), port);
    // 注入 overlay 信任 IP 快照，让 Host 头为 CGNAT/overlay IP（手动对端连过来时）也能通过。
    params.overlay_trusted_ips = state
        .overlay_trusted_ips
        .read()
        .expect("overlay_trusted_ips 读锁中毒")
        .clone();
    match evaluate_browser_request_from_http(&request, &params) {
        Ok(()) => next.run(request).await,
        Err(err) => err.into_response(),
    }
}

/// overlay 信任快照注入中间件：把 `state.overlay_trusted_ips` 克隆进请求扩展，
/// 供内层 `lan_socket_gate`（非 state-aware）在 Denied 分支查阅。
///
/// Business Logic（为什么需要单独的注入层）:
///     `lan_socket_gate` 保持 `(request, next)` 签名以兼容多处测试 router 装配；
///     生产路径在 gate 外层叠一层本函数即可让 gate 感知 overlay，无需改 gate 签名。
pub async fn inject_overlay_trust(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let snapshot = state
        .overlay_trusted_ips
        .read()
        .expect("overlay_trusted_ips 读锁中毒")
        .clone();
    request
        .extensions_mut()
        .insert(OverlayTrustSnapshot(Arc::new(snapshot)));
    next.run(request).await
}

/// 使用显式参数的浏览器门禁中间件（单测/无 AppState 装配）。
///
/// Business Logic（为什么需要这个函数）:
///     单元测试需要在不构造完整 AppState 的情况下覆盖 Host/Origin 矩阵。
///
/// Code Logic（这个函数做什么）:
///     与 `browser_request_guard` 相同判定，参数由调用方注入。
pub async fn browser_request_guard_with_params(
    params: BrowserGuardParams,
    request: Request<Body>,
    next: Next,
) -> Response {
    match evaluate_browser_request_from_http(&request, &params) {
        Ok(()) => next.run(request).await,
        Err(err) => err.into_response(),
    }
}

/// 构造浏览器门禁 403 错误。
///
/// Business Logic（为什么需要这个函数）:
///     Host/Origin 失败不是身份鉴权失败，使用 403 `forbidden` 稳定信封。
///
/// Code Logic（这个函数做什么）:
///     `P2pError::from_code` + Forbidden。
fn browser_guard_error(message: &str, context: &P2pRequestContext) -> P2pError {
    P2pError::from_code(message, P2pErrorCode::Forbidden, context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
    use axum::routing::{get, post};
    use axum::Router;
    use serde_json::Value;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use tower::ServiceExt;

    /// Business Logic（为什么需要这个测试）:
    ///     可选期望 device_id header 错机必须 409 device_id_mismatch；匹配/缺省放行。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 Request 带 wrong/match/absent header，调用 evaluate_expected_device_id_header。
    #[test]
    fn expected_device_id_header_rejects_mismatch_accepts_match_or_absent() {
        let mismatch = Request::builder()
            .uri("/api/health")
            .header(&EXPECTED_DEVICE_ID_HEADER, "wrong-device")
            .body(Body::empty())
            .expect("request");
        let err = evaluate_expected_device_id_header(&mismatch, "host-device")
            .expect_err("wrong header must fail closed");
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let matching = Request::builder()
            .uri("/api/health")
            .header(&EXPECTED_DEVICE_ID_HEADER, "host-device")
            .body(Body::empty())
            .expect("request");
        assert!(evaluate_expected_device_id_header(&matching, "host-device").is_ok());

        let absent = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .expect("request");
        assert!(evaluate_expected_device_id_header(&absent, "host-device").is_ok());

        let empty = Request::builder()
            .uri("/api/health")
            .header(&EXPECTED_DEVICE_ID_HEADER, "   ")
            .body(Body::empty())
            .expect("request");
        assert!(evaluate_expected_device_id_header(&empty, "host-device").is_ok());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     支持/拒绝地址表是产品边界契约，边界地址与代表 denied 地址必须覆盖完整。
    ///
    /// Code Logic（这个测试做什么）:
    ///     表驱动断言 loopback、RFC1918、link-local、ULA、IPv4-mapped 与全局/unspecified/multicast 分类。
    #[test]
    fn classify_peer_ip_covers_supported_and_denied_ranges() {
        let cases: &[(&str, LanPeerScope)] = &[
            // IPv4 loopback 127.0.0.0/8
            ("127.0.0.1", LanPeerScope::Loopback),
            ("127.255.255.254", LanPeerScope::Loopback),
            // IPv4 private
            ("10.0.0.1", LanPeerScope::Lan),
            ("10.255.255.255", LanPeerScope::Lan),
            ("172.16.0.1", LanPeerScope::Lan),
            ("172.31.255.255", LanPeerScope::Lan),
            ("192.168.0.1", LanPeerScope::Lan),
            ("192.168.255.255", LanPeerScope::Lan),
            // IPv4 link-local
            ("169.254.1.1", LanPeerScope::Lan),
            ("169.254.255.255", LanPeerScope::Lan),
            // IPv6 loopback
            ("::1", LanPeerScope::Loopback),
            // IPv6 ULA fc00::/7
            ("fc00::1", LanPeerScope::Lan),
            ("fd12:3456:789a::1", LanPeerScope::Lan),
            // IPv6 link-local fe80::/10
            ("fe80::1", LanPeerScope::Lan),
            ("fe80:0:0:0:0:0:0:1", LanPeerScope::Lan),
            // IPv4-mapped IPv6 → 规范化后按 IPv4 规则
            ("::ffff:127.0.0.1", LanPeerScope::Loopback),
            ("::ffff:192.168.1.10", LanPeerScope::Lan),
            ("::ffff:8.8.8.8", LanPeerScope::Denied),
            // Denied: global / unspecified / multicast / docs
            ("0.0.0.0", LanPeerScope::Denied),
            ("8.8.8.8", LanPeerScope::Denied),
            ("1.1.1.1", LanPeerScope::Denied),
            ("224.0.0.1", LanPeerScope::Denied),
            ("255.255.255.255", LanPeerScope::Denied),
            ("172.15.255.255", LanPeerScope::Denied), // 紧邻 172.16/12 下界外
            ("172.32.0.1", LanPeerScope::Denied),     // 紧邻 172.16/12 上界外
            ("100.64.0.1", LanPeerScope::Denied),     // CGNAT 非产品支持范围
            ("::", LanPeerScope::Denied),
            ("2001:db8::1", LanPeerScope::Denied), // documentation
            ("2001:4860:4860::8888", LanPeerScope::Denied),
            ("ff02::1", LanPeerScope::Denied), // multicast
        ];

        for (raw, expected) in cases {
            let ip: IpAddr = raw.parse().unwrap_or_else(|_| panic!("非法测试 IP: {raw}"));
            assert_eq!(
                classify_peer_ip(ip),
                *expected,
                "classify_peer_ip({raw}) 期望 {expected:?}"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     overlay 信任是 opt-in 精确 IP 白名单；默认空集合不得放行 CGNAT/overlay，
    ///     命中集合才放行，且 IPv4-mapped IPv6 必须规范化后匹配。
    #[test]
    fn is_overlay_trusted_only_matches_explicit_set() {
        let empty: HashSet<IpAddr> = HashSet::new();
        let cgnat: IpAddr = "100.72.52.63".parse().unwrap();
        // 默认空集合：任何 IP 都不放行（含 CGNAT）。
        assert!(!is_overlay_trusted(cgnat, &empty));
        assert!(!is_overlay_trusted("192.168.1.5".parse().unwrap(), &empty));

        let mut set: HashSet<IpAddr> = HashSet::new();
        set.insert(cgnat);
        // 命中集合放行精确 IP。
        assert!(is_overlay_trusted(cgnat, &set));
        // 其它 CGNAT 仍不放行（最小权限，非整段）。
        assert!(!is_overlay_trusted("100.72.52.64".parse().unwrap(), &set));
        // IPv4-mapped IPv6 规范化后匹配同一 IPv4。
        assert!(is_overlay_trusted(
            "::ffff:100.72.52.63".parse::<IpAddr>().unwrap(),
            &set
        ));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     browser Host 门闸默认拒 CGNAT Host；当 overlay 信任集合含该 Host IP 时必须放行，
    ///     让手动对端连过来时（Host=本机 CGNAT IP）通过。端口仍须匹配。
    #[test]
    fn evaluate_browser_request_allows_overlay_host_ip() {
        let mut params = browser_guard_params("device-a", 62116);
        let cgnat: IpAddr = "100.72.52.63".parse().unwrap();
        let ctx = P2pRequestContext {
            request_id: "req-overlay".into(),
        };
        // 默认（空 overlay）：CGNAT Host 被拒。
        let err = evaluate_browser_request(
            &Method::GET,
            "/api/health",
            Some("100.72.52.63:62116"),
            None,
            None,
            &params,
            &ctx,
        )
        .unwrap_err();
        assert_eq!(err.envelope().code, "forbidden");

        // 注入 overlay 集合后：CGNAT Host 通过（端口一致）。
        params.overlay_trusted_ips = {
            let mut s = HashSet::new();
            s.insert(cgnat);
            s
        };
        evaluate_browser_request(
            &Method::GET,
            "/api/health",
            Some("100.72.52.63:62116"),
            None,
            None,
            &params,
            &ctx,
        )
        .expect("overlay Host IP + 正确端口应通过");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     攻击者不得通过 Forwarded / X-Forwarded-For / X-Real-IP 把公网 peer 伪装成 loopback/LAN。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造公网 ConnectInfo 并附带伪造 loopback 代理 header，断言 scope 仍为 Denied；
    ///     分类函数只接收 IpAddr，证明 header 不参与决策。
    #[tokio::test]
    async fn forwarded_headers_never_change_socket_scope() {
        let public_peer = SocketAddr::from((Ipv4Addr::new(8, 8, 8, 8), 54321));
        assert_eq!(
            classify_peer_ip(public_peer.ip()),
            LanPeerScope::Denied,
            "纯 IP 分类必须忽略任何 header 概念"
        );

        let mut headers = HeaderMap::new();
        headers.insert("forwarded", HeaderValue::from_static("for=127.0.0.1"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1, 10.0.0.1"),
        );
        headers.insert("x-real-ip", HeaderValue::from_static("192.168.1.1"));

        let mut request = Request::builder()
            .uri("/probe")
            .header("x-cc-request-id", "req-forwarded-test")
            .body(Body::empty())
            .expect("构造请求");
        // 合并伪造代理 header（保留 request-id）。
        for (name, value) in headers.iter() {
            request.headers_mut().insert(name.clone(), value.clone());
        }
        request.extensions_mut().insert(ConnectInfo(public_peer));

        assert_eq!(
            peer_scope_from_request(&request),
            LanPeerScope::Denied,
            "伪造代理 header 不得改变 socket scope"
        );

        // 经完整 middleware 仍必须 403，且不得进入 handler。
        let router = gate_test_router();
        let response = router.oneshot(request).await.expect("router 不可失败");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response_json(response).await;
        assert_eq!(body["code"], "forbidden");
        assert_eq!(body["error"], "不支持的网络来源");
        assert_eq!(body["request_id"], "req-forwarded-test");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     Denied peer 必须在 handler 前被拦截，不能触达任何业务逻辑。
    ///
    /// Code Logic（这个测试做什么）:
    ///     oneshot 公网 ConnectInfo 到 probe 路由，断言 403 信封且 handler 标记未设置。
    #[tokio::test]
    async fn lan_socket_gate_rejects_denied_peer_before_handler() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let reached = Arc::new(AtomicBool::new(false));
        let flag = reached.clone();
        let router = Router::new()
            .route(
                "/probe",
                get(move || {
                    let flag = flag.clone();
                    async move {
                        flag.store(true, Ordering::SeqCst);
                        "reached"
                    }
                }),
            )
            .layer(axum::middleware::from_fn(lan_socket_gate))
            .layer(axum::middleware::from_fn(
                crate::net::request_context::request_id_middleware,
            ));

        let mut request = Request::builder()
            .uri("/probe")
            .header("x-cc-request-id", "req-denied-peer")
            .body(Body::empty())
            .expect("构造请求");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((
                Ipv4Addr::new(1, 1, 1, 1),
                40000,
            ))));

        let response = router.oneshot(request).await.expect("router 不可失败");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            !reached.load(Ordering::SeqCst),
            "denied peer 不得进入 handler"
        );
        let body = response_json(response).await;
        assert_eq!(body["code"], "forbidden");
        assert_eq!(body["error"], "不支持的网络来源");
        assert_eq!(body["request_id"], "req-denied-peer");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     合法 loopback 与 LAN peer 必须无凭据直达业务 handler。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别以 loopback / RFC1918 / IPv6 loopback / link-local 注入 ConnectInfo，断言 200。
    #[tokio::test]
    async fn lan_socket_gate_allows_loopback_and_lan_business_requests() {
        let peers = [
            SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 10001)),
            SocketAddr::from((Ipv4Addr::new(192, 168, 1, 20), 10002)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, 10003)),
            SocketAddr::new("fe80::2".parse().expect("fe80"), 10004),
            SocketAddr::from((Ipv4Addr::new(10, 1, 2, 3), 10005)),
        ];

        for peer in peers {
            let router = gate_test_router();
            let mut request = Request::builder()
                .uri("/probe")
                .header("x-cc-request-id", "req-allow-peer")
                .body(Body::empty())
                .expect("构造请求");
            request.extensions_mut().insert(ConnectInfo(peer));

            let response = router.oneshot(request).await.expect("router 不可失败");
            assert_eq!(response.status(), StatusCode::OK, "peer {peer} 应被放行");
            let bytes = to_bytes(response.into_body(), 1024)
                .await
                .expect("读取 body");
            assert_eq!(
                std::str::from_utf8(&bytes).unwrap(),
                "reached",
                "peer {peer} 必须进入 handler"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     缺失 ConnectInfo 时必须 fail-closed，避免 oneshot/错误装配绕过门禁。
    ///
    /// Code Logic（这个测试做什么）:
    ///     不注入 ConnectInfo 直接 oneshot，断言 403。
    #[tokio::test]
    async fn lan_socket_gate_rejects_missing_connect_info() {
        let router = gate_test_router();
        let request = Request::builder()
            .uri("/probe")
            .header("x-cc-request-id", "req-missing-peer")
            .body(Body::empty())
            .expect("构造请求");
        let response = router.oneshot(request).await.expect("router 不可失败");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = response_json(response).await;
        assert_eq!(body["code"], "forbidden");
        assert_eq!(body["request_id"], "req-missing-peer");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     stop 与 gate 共用 loopback 判定，单元测试需覆盖非 loopback 拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     LAN IP 调用 require_loopback_peer 必须 403；127.0.0.1 通过。
    #[test]
    fn require_loopback_peer_only_accepts_loopback() {
        let ctx = P2pRequestContext {
            request_id: "req-loopback-helper".to_string(),
        };
        require_loopback_peer(IpAddr::V4(Ipv4Addr::LOCALHOST), &ctx).expect("loopback 应通过");
        let err = require_loopback_peer(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 2)), &ctx)
            .expect_err("LAN 不得通过 loopback 校验");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.envelope().code, "forbidden");
    }

    /// 构造带 request_id → lan_socket_gate 的最小 probe Router。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     gate 测试需要与生产相近的 middleware 顺序，但不依赖完整 AppState 路由表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `/probe` 返回 "reached"；layer 顺序 last=outermost：request_id 外层、gate 内层。
    fn gate_test_router() -> Router {
        Router::new()
            .route("/probe", get(|| async { "reached" }))
            .layer(axum::middleware::from_fn(lan_socket_gate))
            .layer(axum::middleware::from_fn(
                crate::net::request_context::request_id_middleware,
            ))
    }

    /// 读取 JSON 错误信封 body。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     断言稳定 code/message/request_id 需要解析 JSON。
    ///
    /// Code Logic（这个函数做什么）:
    ///     消费 response body 并 `serde_json::from_slice`。
    async fn response_json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("读取 body");
        serde_json::from_slice(&bytes).expect("响应应为 JSON 信封")
    }

    /// 默认浏览器门禁测试参数。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     矩阵测试需要稳定的实际端口与受控 mDNS 名。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 port=62116、host=`cc-device-a.local`。
    fn sample_browser_params() -> BrowserGuardParams {
        browser_guard_params("device-a", 62116)
    }

    /// 构造带 Host/Origin 的请求。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     浏览器矩阵需要快速构造不同 Host/Origin/Content-Type 组合。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构建 Request 并注入 loopback ConnectInfo，可选写入 Origin/Content-Type。
    fn browser_request(
        method: Method,
        path: &str,
        host: &str,
        origin: Option<&str>,
        content_type: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .header("x-cc-request-id", "req-browser-matrix");
        if let Some(origin) = origin {
            builder = builder.header("origin", origin);
        }
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        let mut request = builder.body(Body::empty()).expect("构造请求");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 20000))));
        request
    }

    /// 构造浏览器门禁测试 Router。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     需要验证 middleware 顺序下 Host/Origin 失败不会进入 handler。
    ///
    /// Code Logic（这个函数做什么）:
    ///     注册 probe 与若干 API/proxy 路由；layer：body 无；envelope 无；
    ///     browser_request_guard_with_params → lan_socket_gate → request_id（外层）。
    fn browser_guard_test_router(params: BrowserGuardParams) -> Router {
        Router::new()
            .route("/probe", get(|| async { "reached" }))
            .route(
                "/api/mobile/workbench/files/save-text",
                post(|| async { "saved" }),
            )
            .route("/api/health", get(|| async { "ok" }))
            .route(
                "/api/workbench/browser/proxy/:previewId/*path",
                get(|| async { "proxy" }),
            )
            .route(
                "/api/mobile/workbench/browser/proxy/:previewId/*path",
                get(|| async { "mobile-proxy" }),
            )
            .layer(axum::middleware::from_fn(move |req, next| {
                let params = params.clone();
                async move { browser_request_guard_with_params(params, req, next).await }
            }))
            .layer(axum::middleware::from_fn(lan_socket_gate))
            .layer(axum::middleware::from_fn(
                crate::net::request_context::request_id_middleware,
            ))
    }

    /// Business Logic（为什么需要这个测试模块）:
    ///     Host/Origin/Content-Type/WebSocket 敌意矩阵是 Task 2 产品契约，必须表驱动覆盖。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖合法 Host、非法 Host/端口、同源写、跨站/null Origin、native 无 Origin、
    ///     跨站 WS、simple Content-Type 写、preview null 不能访问其它 /api/*。
    mod browser_request_matrix {
        use super::*;

        #[test]
        fn valid_lan_loopback_and_controlled_local_hosts_succeed() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-host-ok".into(),
            };
            let hosts = [
                "127.0.0.1:62116",
                "192.168.1.20:62116",
                "10.0.0.8:62116",
                "localhost:62116",
                "cc-device-a.local:62116",
                "cc-device-a.local.:62116",
                "[::1]:62116",
                "[fe80::1]:62116",
                "[fd12:3456:789a::1]:62116",
            ];
            for host in hosts {
                evaluate_browser_request(
                    &Method::GET,
                    "/api/health",
                    Some(host),
                    None,
                    None,
                    &params,
                    &ctx,
                )
                .unwrap_or_else(|e| panic!("host {host} 应通过: {e:?}"));
            }
        }

        #[test]
        fn arbitrary_host_and_wrong_port_fail() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-host-bad".into(),
            };
            let cases = [
                "evil.example:62116",
                "192.168.1.20:9",
                "127.0.0.1",
                "8.8.8.8:62116",
                "localhost:1",
                "cc-other.local:62116",
            ];
            for host in cases {
                let err = evaluate_browser_request(
                    &Method::GET,
                    "/api/health",
                    Some(host),
                    None,
                    None,
                    &params,
                    &ctx,
                )
                .expect_err("应拒绝");
                assert_eq!(err.status(), StatusCode::FORBIDDEN, "host={host}");
            }
        }

        #[test]
        fn same_origin_mobile_write_succeeds() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-same-origin-write".into(),
            };
            evaluate_browser_request(
                &Method::POST,
                "/api/mobile/workbench/files/save-text",
                Some("192.168.1.20:62116"),
                Some("http://192.168.1.20:62116"),
                Some("application/json"),
                &params,
                &ctx,
            )
            .expect("同源 mobile 写应通过");
        }

        #[test]
        fn same_origin_comparison_normalizes_host_case_and_trailing_dot() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-same-origin-normalize".into(),
            };
            let cases = [
                ("LocalHost:62116", "http://localhost:62116"),
                ("CC-Device-A.local.:62116", "http://cc-device-a.local:62116"),
                ("cc-device-a.local:62116", "http://CC-Device-A.local.:62116"),
                ("[::1]:62116", "http://[::1]:62116"),
            ];
            for (host, origin) in cases {
                evaluate_browser_request(
                    &Method::POST,
                    "/api/mobile/workbench/files/save-text",
                    Some(host),
                    Some(origin),
                    Some("application/json"),
                    &params,
                    &ctx,
                )
                .unwrap_or_else(|e| {
                    panic!("规范化后应同源: host={host} origin={origin} err={e:?}")
                });
            }
            // 跨站在规范化后仍必须拒绝。
            let err = evaluate_browser_request(
                &Method::POST,
                "/api/mobile/workbench/files/save-text",
                Some("LocalHost:62116"),
                Some("http://evil.example:62116"),
                Some("application/json"),
                &params,
                &ctx,
            )
            .expect_err("规范化不得放宽跨站");
            assert_eq!(err.status(), StatusCode::FORBIDDEN);
        }

        #[test]
        fn cross_origin_and_ordinary_null_origin_fail() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-origin-bad".into(),
            };
            let err = evaluate_browser_request(
                &Method::POST,
                "/api/mobile/workbench/files/save-text",
                Some("192.168.1.20:62116"),
                Some("http://evil.test"),
                Some("application/json"),
                &params,
                &ctx,
            )
            .expect_err("跨站应失败");
            assert_eq!(err.status(), StatusCode::FORBIDDEN);

            let err = evaluate_browser_request(
                &Method::GET,
                "/api/health",
                Some("127.0.0.1:62116"),
                Some("null"),
                None,
                &params,
                &ctx,
            )
            .expect_err("普通 API Origin:null 应失败");
            assert_eq!(err.status(), StatusCode::FORBIDDEN);
        }

        #[test]
        fn native_p2p_without_origin_succeeds() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-native".into(),
            };
            evaluate_browser_request(
                &Method::POST,
                "/api/sync/pull",
                Some("192.168.1.20:62116"),
                None,
                Some("application/json"),
                &params,
                &ctx,
            )
            .expect("native 无 Origin 应通过");
        }

        #[test]
        fn cross_origin_websocket_fails() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-ws".into(),
            };
            // WebSocket upgrade 使用 GET + Origin；跨站必须拒绝。
            let err = evaluate_browser_request(
                &Method::GET,
                "/api/workbench/sessions/events",
                Some("127.0.0.1:62116"),
                Some("http://attacker.example"),
                None,
                &params,
                &ctx,
            )
            .expect_err("跨站 WS Origin 应失败");
            assert_eq!(err.status(), StatusCode::FORBIDDEN);
        }

        #[test]
        fn form_multipart_text_plain_ordinary_writes_fail() {
            let params = sample_browser_params();
            let ctx = P2pRequestContext {
                request_id: "req-ct".into(),
            };
            for ct in [
                "application/x-www-form-urlencoded",
                "multipart/form-data; boundary=abc",
                "text/plain",
                "text/plain;charset=UTF-8",
            ] {
                let err = evaluate_browser_request(
                    &Method::POST,
                    "/api/sync/push",
                    Some("127.0.0.1:62116"),
                    None,
                    Some(ct),
                    &params,
                    &ctx,
                )
                .expect_err("simple content-type 写应失败");
                assert_eq!(err.status(), StatusCode::FORBIDDEN, "ct={ct}");
            }
            // preview proxy 不受该限制
            evaluate_browser_request(
                &Method::POST,
                "/api/workbench/browser/proxy/abc/save",
                Some("127.0.0.1:62116"),
                Some("null"),
                Some("application/x-www-form-urlencoded"),
                &params,
                &ctx,
            )
            .expect("preview proxy 可携带任意业务 Content-Type");
        }

        #[tokio::test]
        async fn preview_null_origin_exception_cannot_access_other_api() {
            let params = sample_browser_params();
            let router = browser_guard_test_router(params);

            // 普通 /api/health + Origin:null → 403，不得进入 handler。
            let reached = Arc::new(AtomicBool::new(false));
            let flag = reached.clone();
            let router_health = Router::new()
                .route(
                    "/api/health",
                    get(move || {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, AtomicOrdering::SeqCst);
                            "ok"
                        }
                    }),
                )
                .layer(axum::middleware::from_fn(move |req, next| {
                    let params = sample_browser_params();
                    async move { browser_request_guard_with_params(params, req, next).await }
                }))
                .layer(axum::middleware::from_fn(lan_socket_gate))
                .layer(axum::middleware::from_fn(
                    crate::net::request_context::request_id_middleware,
                ));

            let request = browser_request(
                Method::GET,
                "/api/health",
                "127.0.0.1:62116",
                Some("null"),
                None,
            );
            let response = router_health.oneshot(request).await.expect("router");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(
                !reached.load(AtomicOrdering::SeqCst),
                "Origin:null 不得访问普通 /api/*"
            );

            // preview proxy + Origin:null 在全局 guard 可延期通过（会话校验在 handler）。
            let request = browser_request(
                Method::GET,
                "/api/workbench/browser/proxy/live-id/index.html",
                "127.0.0.1:62116",
                Some("null"),
                None,
            );
            let response = router.oneshot(request).await.expect("router");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "preview proxy null Origin 应延期进入 handler"
            );
        }

        #[tokio::test]
        async fn middleware_rejects_before_handler_on_bad_host() {
            let reached = Arc::new(AtomicBool::new(false));
            let flag = reached.clone();
            let router = Router::new()
                .route(
                    "/api/health",
                    get(move || {
                        let flag = flag.clone();
                        async move {
                            flag.store(true, AtomicOrdering::SeqCst);
                            "ok"
                        }
                    }),
                )
                .layer(axum::middleware::from_fn(move |req, next| {
                    let params = sample_browser_params();
                    async move { browser_request_guard_with_params(params, req, next).await }
                }))
                .layer(axum::middleware::from_fn(lan_socket_gate))
                .layer(axum::middleware::from_fn(
                    crate::net::request_context::request_id_middleware,
                ));
            let request =
                browser_request(Method::GET, "/api/health", "evil.example:62116", None, None);
            let response = router.oneshot(request).await.expect("router");
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert!(!reached.load(AtomicOrdering::SeqCst));
            let body = response_json(response).await;
            assert_eq!(body["code"], "forbidden");
        }
    }
}
