//! net/lan_guard.rs — 固定 LAN socket peer 范围门禁
//!
//! Business Logic（为什么需要这个模块）:
//!     产品只支持本机与局域网访问。HTTP 业务 API 对合法 loopback/LAN peer 一律无凭据放行，
//!     但对全局可路由、unspecified、multicast、文档保留或无法判定的 peer 必须在 handler 前拒绝。
//!     该门禁约束支持网络范围，不是身份鉴权；不得读取代理来源 header。
//!
//! Code Logic（这个模块做什么）:
//!     - `LanPeerScope`：内部三类 peer 范围（Loopback / Lan / Denied），不是用户配置或权限模式；
//!     - `classify_peer_ip`：纯函数，规范化 IPv4-mapped IPv6 后按固定地址范围分类；
//!     - `lan_socket_gate`：axum middleware，仅信任 `ConnectInfo<SocketAddr>`，拒绝 Denied/缺失 peer；
//!     - `require_loopback_peer`：backend stop 等本机生命周期接口在 token 比较前强制 loopback。

use crate::net::error_response::{P2pError, P2pErrorCode};
use crate::net::request_context::{new_request_id, P2pRequestContext};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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
    } else if is_ipv6_ula(ip) || ip.is_unicast_link_local() {
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
            let context = request_context_from_request(&request);
            denied_peer_error(&context).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use serde_json::Value;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    use tower::ServiceExt;

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
}
