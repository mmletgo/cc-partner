//! net/mobile_dev_proxy.rs — 开发态把 `/mobile` 反向代理到本机 Vite（HMR）
//!
//! Business Logic（为什么需要这个模块）:
//!     手机扫码入口始终是 backend 端口上的 `/mobile`（首选 62116），生产走 dist/嵌入资源；
//!     开发时若仍只读 `web/dist`，改 `web/src/mobile/**` 必须重新打包。开发态将 SPA 与 Vite
//!     模块/HMR WebSocket 代理到 `127.0.0.1:5173`，即可在同一扫码 URL 下热更新。
//!
//! Code Logic（这个模块做什么）:
//!     - 环境开关 `CC_PARTNER_MOBILE_DEV_PROXY`：`1/true/on` 强制开，`0/false/off` 强制关，
//!       未设置时 Auto（尝试代理，上游不可达则返回 None 让调用方回落静态资源）。
//!     - 上游默认 `http://127.0.0.1:5173`，可被 `CC_PARTNER_VITE_DEV_URL` 覆盖。
//!     - `/mobile` → `/mobile.html`；SPA 子路由回落 shell；`/src`/`/@vite` 等模块路径原样转发；
//!     - fallback 收到的非 `/api` WebSocket upgrade（Vite HMR）桥接到上游。

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::FromRequestParts;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as TungsteniteRequest;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const DEFAULT_VITE_HTTP_ORIGIN: &str = "http://127.0.0.1:5173";
const PROXY_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// 上游连接失败后的短暂冷却，避免每个静态资源请求都同步打满拒连。
const NEGATIVE_CACHE_MS: u64 = 1_500;

type UpstreamWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 代理模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobileDevProxyMode {
    /// 未设置环境变量：尝试代理，失败回落静态。
    Auto,
    /// 强制开启（仍可能因上游挂掉失败）。
    On,
    /// 强制关闭。
    Off,
}

/// 解析 `CC_PARTNER_MOBILE_DEV_PROXY`。
///
/// Business Logic: 开发者可显式关掉代理（只用 dist）或强制开。
///     release 默认 Off（生产包不得探测 5173）；debug 默认 Auto（Vite 在则 HMR，不在回落 dist）。
/// Code Logic: 大小写不敏感识别 0/false/off 与 1/true/on；其它非空值视为 On；
///     缺省/空串：debug → Auto，release → Off。
pub fn proxy_mode_from_env() -> MobileDevProxyMode {
    match std::env::var("CC_PARTNER_MOBILE_DEV_PROXY") {
        Err(_) => default_proxy_mode(),
        Ok(raw) => {
            let value = raw.trim();
            if value.is_empty() {
                return default_proxy_mode();
            }
            match value.to_ascii_lowercase().as_str() {
                "0" | "false" | "off" | "no" => MobileDevProxyMode::Off,
                "1" | "true" | "on" | "yes" => MobileDevProxyMode::On,
                _ => MobileDevProxyMode::On,
            }
        }
    }
}

fn default_proxy_mode() -> MobileDevProxyMode {
    if cfg!(debug_assertions) {
        MobileDevProxyMode::Auto
    } else {
        MobileDevProxyMode::Off
    }
}

/// 当前是否允许发起 Vite 代理（含 Auto）。
pub fn is_proxy_enabled() -> bool {
    !matches!(proxy_mode_from_env(), MobileDevProxyMode::Off)
}

/// 解析 Vite 上游 HTTP origin（无尾斜杠）。
///
/// Business Logic: 默认 loopback 5173，与 `web/vite.config.ts` / Tauri `devUrl` 对齐；可 env 覆盖。
/// Code Logic: 读 `CC_PARTNER_VITE_DEV_URL`，去尾 `/`；非法空串回退默认。
pub fn vite_http_origin() -> String {
    std::env::var("CC_PARTNER_VITE_DEV_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_VITE_HTTP_ORIGIN.to_string())
}

/// 判断 path 是否应尝试 HTTP 代理到 Vite。
///
/// Business Logic: 仅移动端 SPA 与 Vite dev 模块命名空间；`/api/*` 永不到这里。
/// Code Logic: `/mobile`、`/assets`、Vite 内部路径、源码与 node_modules 前缀。
pub fn is_proxyable_http_path(path: &str) -> bool {
    path == "/mobile"
        || path == "/mobile/"
        || path == "/mobile.html"
        || path.starts_with("/mobile/")
        || path.starts_with("/assets/")
        || is_vite_dev_module_path(path)
}

/// Vite dev server 模块/工具路径（不含 `/mobile` shell）。
pub fn is_vite_dev_module_path(path: &str) -> bool {
    path == "/@react-refresh"
        || path.starts_with("/@vite/")
        || path.starts_with("/@id/")
        || path.starts_with("/@fs/")
        || path.starts_with("/src/")
        || path.starts_with("/node_modules/")
        || path.starts_with("/@")
}

/// fallback 上的 WebSocket 是否应按 Vite HMR 代理。
///
/// Business Logic: 显式 `/api/*` WS 已由路由处理；落到 fallback 的 upgrade 在开发态是 HMR。
/// Code Logic: 非 `/api` 前缀即 true（调用方仍需检查 upgrade 头与开关）。
pub fn is_proxyable_websocket_path(path: &str) -> bool {
    !path.starts_with("/api/") && path != "/api"
}

/// 将 backend 对外 path 映射为 Vite 上游 path（保留 leading `/`）。
///
/// Business Logic:
///     扫码 URL 是 `/mobile`，Vite MPA 入口文件是 `mobile.html`；客户端路由需回落 shell；
///     模块路径保持 Vite 约定的根路径（`/src/...`）。
///
/// Code Logic:
///     `/mobile`/`/mobile/`/`无扩展名的 /mobile/<spa>` → `/mobile.html`；
///     `/mobile/<file.ext>` → `/<file.ext>`；其余 proxyable path 原样返回。
pub fn map_backend_path_to_vite(path: &str) -> Option<String> {
    if path == "/mobile" || path == "/mobile/" || path == "/mobile.html" {
        return Some("/mobile.html".to_string());
    }
    if let Some(rest) = path.strip_prefix("/mobile/") {
        if rest.is_empty() {
            return Some("/mobile.html".to_string());
        }
        if looks_like_static_file(rest) {
            return Some(format!("/{rest}"));
        }
        // SPA client route under /mobile/*
        return Some("/mobile.html".to_string());
    }
    if is_proxyable_http_path(path) {
        return Some(path.to_string());
    }
    None
}

/// 路径最后一段是否像静态文件（含扩展名）。
fn looks_like_static_file(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or(path);
    if last.is_empty() || last == "." || last == ".." {
        return false;
    }
    match last.rsplit_once('.') {
        Some((name, ext)) => !name.is_empty() && !ext.is_empty() && !ext.contains('.'),
        None => false,
    }
}

/// 若上游近期失败则跳过代理（Auto/On 共用，避免拒连风暴）。
fn negative_cache_blocks() -> bool {
    let until = NEGATIVE_UNTIL_MS.load(Ordering::Relaxed);
    if until == 0 {
        return false;
    }
    now_ms() < until
}

fn mark_upstream_failure() {
    let until = now_ms().saturating_add(NEGATIVE_CACHE_MS);
    NEGATIVE_UNTIL_MS.store(until, Ordering::Relaxed);
}

fn mark_upstream_success() {
    NEGATIVE_UNTIL_MS.store(0, Ordering::Relaxed);
}

static NEGATIVE_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 尝试把 HTTP 请求代理到 Vite；上游不可用时返回 None（供静态回落）。
///
/// Business Logic: 开发态优先 Vite；Vite 未启动时手机仍可吃 dist。
/// Code Logic: 映射 path → 组 upstream URL → reqwest 转发；连接/5xx 类失败 mark negative cache。
pub async fn try_proxy_http(req: Request<Body>) -> Option<Response<Body>> {
    if !is_proxy_enabled() || negative_cache_blocks() {
        return None;
    }
    let path = req.uri().path();
    let upstream_path = map_backend_path_to_vite(path)?;
    let query = req.uri().query().map(str::to_string);
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD && method != Method::OPTIONS {
        // mobile SPA 开发几乎只有 GET；其它方法仍转发但不做静态回落语义
    }
    let headers = req.headers().clone();
    let body = to_bytes(req.into_body(), PROXY_BODY_LIMIT_BYTES)
        .await
        .ok()?;
    match proxy_http_to_vite(&method, &upstream_path, query.as_deref(), &headers, body).await {
        Ok(response) => {
            mark_upstream_success();
            Some(response)
        }
        Err(error) => {
            tracing::debug!(target: "mobile_dev_proxy", "Vite HTTP 代理失败: {error}");
            mark_upstream_failure();
            None
        }
    }
}

async fn proxy_http_to_vite(
    method: &Method,
    upstream_path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, String> {
    let origin = vite_http_origin();
    let mut url = format!(
        "{origin}{}",
        if upstream_path.starts_with('/') {
            upstream_path.to_string()
        } else {
            format!("/{upstream_path}")
        }
    );
    if let Some(query) = query {
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(400))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("构造 Vite 客户端失败: {error}"))?;

    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|error| format!("非法方法: {error}"))?;

    let mut request = client.request(reqwest_method, &url);
    for (name, value) in filtered_forward_headers(headers) {
        request = request.header(name, value);
    }
    // 上游是 loopback Vite；Host 必须是 Vite 的 host，不能透传手机看到的 LAN Host。
    if let Ok(uri) = url.parse::<Uri>() {
        if let Some(authority) = uri.authority() {
            if let Ok(host) = HeaderValue::from_str(authority.as_str()) {
                request = request.header(header::HOST, host);
            }
        }
    }
    if !body.is_empty() {
        request = request.body(body);
    }

    let upstream = request
        .send()
        .await
        .map_err(|error| format!("连接 Vite 失败: {error}"))?;

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response_builder = Response::builder().status(status);
    if let Some(headers_mut) = response_builder.headers_mut() {
        for (name, value) in upstream.headers().iter() {
            if is_hop_by_hop_header(name.as_str()) {
                continue;
            }
            if let (Ok(axum_name), Ok(axum_value)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                headers_mut.append(axum_name, axum_value);
            }
        }
    }
    let bytes = upstream
        .bytes()
        .await
        .map_err(|error| format!("读取 Vite 响应失败: {error}"))?;
    response_builder
        .body(Body::from(bytes))
        .map_err(|error| format!("构造响应失败: {error}"))
}

fn filtered_forward_headers(headers: &HeaderMap) -> Vec<(HeaderName, HeaderValue)> {
    let mut out = Vec::new();
    for (name, value) in headers.iter() {
        let key = name.as_str();
        if is_hop_by_hop_header(key) || key.eq_ignore_ascii_case("host") {
            continue;
        }
        out.push((name.clone(), value.clone()));
    }
    out
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "host"
    )
}

/// 判断请求是否为 WebSocket upgrade。
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let has_connection_upgrade = headers.get_all(header::CONNECTION).iter().any(|value| {
        value
            .to_str()
            .map(|raw| {
                raw.split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
            .unwrap_or(false)
    });
    let has_websocket_upgrade = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    has_connection_upgrade && has_websocket_upgrade
}

/// 将 fallback 上的 WebSocket 桥接到 Vite HMR。
///
/// Business Logic: 手机通过 62116 打开页面时，HMR client 会连同一 host:port 的 WS。
/// Code Logic: connect_async 上游 → axum upgrade → 双向 forward；失败返回 502 文本。
pub async fn proxy_websocket(req: Request<Body>) -> Response<Body> {
    if !is_proxy_enabled() {
        return plain_response(StatusCode::NOT_FOUND, "mobile vite proxy disabled");
    }
    if negative_cache_blocks() {
        return plain_response(StatusCode::BAD_GATEWAY, "vite dev server unavailable");
    }
    match proxy_websocket_inner(req).await {
        Ok(response) => {
            mark_upstream_success();
            response
        }
        Err(error) => {
            tracing::debug!(target: "mobile_dev_proxy", "Vite WS 代理失败: {error}");
            mark_upstream_failure();
            plain_response(StatusCode::BAD_GATEWAY, "vite hmr proxy failed")
        }
    }
}

async fn proxy_websocket_inner(req: Request<Body>) -> Result<Response<Body>, String> {
    let path = req.uri().path();
    let query = req.uri().query();
    let origin = vite_http_origin();
    let ws_origin = http_origin_to_ws_origin(&origin)?;
    let mut upstream_url = format!(
        "{ws_origin}{}",
        if path.is_empty() { "/" } else { path }
    );
    if let Some(query) = query {
        if !query.is_empty() {
            upstream_url.push('?');
            upstream_url.push_str(query);
        }
    }

    let (mut parts, _body) = req.into_parts();
    let upstream_request = build_upstream_websocket_request(&upstream_url, &parts.headers)?;
    let (upstream, upstream_response) = connect_async(upstream_request)
        .await
        .map_err(|error| format!("连接 Vite WS 失败: {error}"))?;
    let selected_protocol = upstream_response
        .headers()
        .get(HeaderName::from_static("sec-websocket-protocol"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .map_err(|error| format!("WebSocket upgrade 无效: {error}"))?;
    let upgrade = if let Some(protocol) = selected_protocol {
        upgrade.protocols([protocol])
    } else {
        upgrade
    };

    Ok(upgrade
        .on_upgrade(move |socket| async move {
            if let Err(error) = bridge_websocket(socket, upstream).await {
                tracing::debug!(target: "mobile_dev_proxy", "Vite HMR WS 桥接结束: {error}");
            }
        })
        .into_response())
}

fn http_origin_to_ws_origin(origin: &str) -> Result<String, String> {
    if let Some(rest) = origin.strip_prefix("https://") {
        return Ok(format!("wss://{rest}"));
    }
    if let Some(rest) = origin.strip_prefix("http://") {
        return Ok(format!("ws://{rest}"));
    }
    if origin.starts_with("ws://") || origin.starts_with("wss://") {
        return Ok(origin.to_string());
    }
    Err(format!("非法 Vite origin: {origin}"))
}

fn build_upstream_websocket_request(
    upstream_url: &str,
    downstream_headers: &HeaderMap,
) -> Result<TungsteniteRequest, String> {
    let mut request = upstream_url
        .into_client_request()
        .map_err(|error| format!("非法 WS URL: {error}"))?;
    for name in [
        header::COOKIE,
        header::ORIGIN,
        header::AUTHORIZATION,
        HeaderName::from_static("sec-websocket-protocol"),
    ] {
        for value in downstream_headers.get_all(&name) {
            request.headers_mut().append(name.clone(), value.clone());
        }
    }
    Ok(request)
}

async fn bridge_websocket(socket: WebSocket, upstream: UpstreamWebSocket) -> Result<(), String> {
    let (mut upstream_write, mut upstream_read) = upstream.split();
    let (mut downstream_write, mut downstream_read) = socket.split();

    let client_to_upstream = async {
        while let Some(message) = downstream_read.next().await {
            let message = message.map_err(|error| format!("读取下游 WS 失败: {error}"))?;
            upstream_write
                .send(axum_to_tungstenite_message(message))
                .await
                .map_err(|error| format!("写入上游 WS 失败: {error}"))?;
        }
        Ok::<(), String>(())
    };

    let upstream_to_client = async {
        while let Some(message) = upstream_read.next().await {
            let message = message.map_err(|error| format!("读取上游 WS 失败: {error}"))?;
            if let Some(message) = tungstenite_to_axum_message(message) {
                downstream_write
                    .send(message)
                    .await
                    .map_err(|error| format!("写入下游 WS 失败: {error}"))?;
            }
        }
        Ok::<(), String>(())
    };

    tokio::select! {
        result = client_to_upstream => result,
        result = upstream_to_client => result,
    }
}

fn axum_to_tungstenite_message(message: AxumWsMessage) -> TungsteniteMessage {
    match message {
        AxumWsMessage::Text(text) => TungsteniteMessage::Text(text),
        AxumWsMessage::Binary(binary) => TungsteniteMessage::Binary(binary),
        AxumWsMessage::Ping(ping) => TungsteniteMessage::Ping(ping),
        AxumWsMessage::Pong(pong) => TungsteniteMessage::Pong(pong),
        AxumWsMessage::Close(_) => TungsteniteMessage::Close(None),
    }
}

fn tungstenite_to_axum_message(message: TungsteniteMessage) -> Option<AxumWsMessage> {
    match message {
        TungsteniteMessage::Text(text) => Some(AxumWsMessage::Text(text)),
        TungsteniteMessage::Binary(binary) => Some(AxumWsMessage::Binary(binary)),
        TungsteniteMessage::Ping(ping) => Some(AxumWsMessage::Ping(ping)),
        TungsteniteMessage::Pong(pong) => Some(AxumWsMessage::Pong(pong)),
        TungsteniteMessage::Close(_) => Some(AxumWsMessage::Close(None)),
        TungsteniteMessage::Frame(_) => None,
    }
}

fn plain_response(status: StatusCode, body: &'static str) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// 测试辅助：清 negative cache（仅测试）。
#[cfg(test)]
pub fn reset_negative_cache_for_test() {
    NEGATIVE_UNTIL_MS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mobile_shell_and_spa_routes_to_mobile_html() {
        assert_eq!(
            map_backend_path_to_vite("/mobile").as_deref(),
            Some("/mobile.html")
        );
        assert_eq!(
            map_backend_path_to_vite("/mobile/").as_deref(),
            Some("/mobile.html")
        );
        assert_eq!(
            map_backend_path_to_vite("/mobile/projects").as_deref(),
            Some("/mobile.html")
        );
        assert_eq!(
            map_backend_path_to_vite("/mobile/work/terminal").as_deref(),
            Some("/mobile.html")
        );
    }

    #[test]
    fn maps_mobile_static_and_root_assets() {
        assert_eq!(
            map_backend_path_to_vite("/mobile/assets/index.js").as_deref(),
            Some("/assets/index.js")
        );
        assert_eq!(
            map_backend_path_to_vite("/assets/foo.css").as_deref(),
            Some("/assets/foo.css")
        );
    }

    #[test]
    fn maps_vite_module_paths_verbatim() {
        assert_eq!(
            map_backend_path_to_vite("/src/mobile/main.tsx").as_deref(),
            Some("/src/mobile/main.tsx")
        );
        assert_eq!(
            map_backend_path_to_vite("/@vite/client").as_deref(),
            Some("/@vite/client")
        );
        assert_eq!(
            map_backend_path_to_vite("/@react-refresh").as_deref(),
            Some("/@react-refresh")
        );
        assert_eq!(
            map_backend_path_to_vite("/node_modules/.vite/deps/react.js").as_deref(),
            Some("/node_modules/.vite/deps/react.js")
        );
    }

    #[test]
    fn rejects_unrelated_paths() {
        assert_eq!(map_backend_path_to_vite("/api/health"), None);
        assert_eq!(map_backend_path_to_vite("/"), None);
        assert_eq!(map_backend_path_to_vite("/workbench"), None);
    }

    #[test]
    fn websocket_proxyable_excludes_api() {
        assert!(is_proxyable_websocket_path("/"));
        assert!(is_proxyable_websocket_path("/vite-hmr"));
        assert!(!is_proxyable_websocket_path("/api/mobile/workbench/terminal-input-stream"));
        assert!(!is_proxyable_websocket_path("/api"));
    }

    #[test]
    fn proxy_mode_parses_env_values() {
        // 直接测解析分支：复制逻辑避免污染全局 env
        fn parse(raw: Option<&str>) -> MobileDevProxyMode {
            match raw {
                None => default_proxy_mode(),
                Some(value) => {
                    let value = value.trim();
                    if value.is_empty() {
                        return default_proxy_mode();
                    }
                    match value.to_ascii_lowercase().as_str() {
                        "0" | "false" | "off" | "no" => MobileDevProxyMode::Off,
                        "1" | "true" | "on" | "yes" => MobileDevProxyMode::On,
                        _ => MobileDevProxyMode::On,
                    }
                }
            }
        }
        // 本 crate 测试在 debug_assertions 下编译，缺省为 Auto。
        assert_eq!(parse(None), MobileDevProxyMode::Auto);
        assert_eq!(parse(Some("")), MobileDevProxyMode::Auto);
        assert_eq!(parse(Some("0")), MobileDevProxyMode::Off);
        assert_eq!(parse(Some("OFF")), MobileDevProxyMode::Off);
        assert_eq!(parse(Some("1")), MobileDevProxyMode::On);
        assert_eq!(parse(Some("true")), MobileDevProxyMode::On);
    }

    #[test]
    fn default_proxy_mode_is_auto_in_debug_tests() {
        assert_eq!(default_proxy_mode(), MobileDevProxyMode::Auto);
    }

    #[test]
    fn websocket_upgrade_detection() {
        let mut headers = HeaderMap::new();
        assert!(!is_websocket_upgrade(&headers));
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        assert!(is_websocket_upgrade(&headers));
    }

    #[test]
    fn http_to_ws_origin() {
        assert_eq!(
            http_origin_to_ws_origin("http://127.0.0.1:5173").unwrap(),
            "ws://127.0.0.1:5173"
        );
        assert_eq!(
            http_origin_to_ws_origin("https://example.test").unwrap(),
            "wss://example.test"
        );
    }

    #[test]
    fn looks_like_static_file_rules() {
        assert!(looks_like_static_file("assets/index.js"));
        assert!(looks_like_static_file("main.tsx"));
        assert!(!looks_like_static_file("projects"));
        assert!(!looks_like_static_file("work/terminal"));
        assert!(!looks_like_static_file(".."));
    }

    #[test]
    fn negative_cache_roundtrip() {
        reset_negative_cache_for_test();
        assert!(!negative_cache_blocks());
        mark_upstream_failure();
        assert!(negative_cache_blocks());
        reset_negative_cache_for_test();
        assert!(!negative_cache_blocks());
    }
}
