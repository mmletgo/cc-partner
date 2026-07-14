//! LAN trust boundary 集成 smoke 服务器与矩阵。
//!
//! Business Logic（为什么需要这个模块）:
//!     S1 Task 6 需要在真实绑定端口上验证固定 LAN 边界：无凭据业务读写、Host/Origin、
//!     stop 的 loopback+token、资源上限，以及把 denied/forwarded 标为 **injected** 证据。
//!     该矩阵作为跨平台 smoke 与 `docs/development/testing.md` 证据表的自动化基线。
//!
//! Code Logic（这个模块做什么）:
//!     构造与生产相近的 middleware 栈（request_id → lan_socket_gate → browser_guard →
//!     envelope_fallback → DefaultBodyLimit），`into_make_service_with_connect_info` 绑定
//!     `127.0.0.1:0`，用 reqwest 发真实 HTTP；对无法经真实 socket 表达的 peer 用 oneshot
//!     注入 ConnectInfo，并在日志/断言消息中标注 `INJECTED_PEER_EVIDENCE`。

use crate::net::error_response::{envelope_fallback_middleware, P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::{
    browser_guard_params, browser_request_guard_with_params, classify_peer_ip, lan_socket_gate,
    require_loopback_peer, BrowserGuardParams, LanPeerScope,
};
use crate::net::request_context::{request_id_middleware, P2pRequestContext};
use crate::transfer::CHUNK_SIZE;
use crate::workbench::browser_proxy::{
    accept_preview_request_with_origin, WorkbenchBrowserPreviewRegistry,
};
use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension, Path, State};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower::ServiceExt;

/// 与生产 `http_server::BODY_LIMIT_BYTES` 对齐的全局 body 上限（32 MiB）。
pub const BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// oneshot / 注入 peer 证据标签：不得当作真实多机网络结果。
pub const INJECTED_PEER_EVIDENCE: &str = "INJECTED_PEER_EVIDENCE";

/// smoke stop 控制请求体（camelCase controlToken）。
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StopRequest {
    control_token: String,
}

/// smoke stop 响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StopResponse {
    ok: bool,
}

/// 绑定 smoke 服务器句柄。
///
/// Business Logic（为什么需要这个结构）:
///     集成测试需要知道实际端口、stop token、preview registry，并在结束后释放 listener。
///
/// Code Logic（这个结构做什么）:
///     持有 base URL、实际端口、token、共享 preview registry、shutdown 发送端与 join handle。
pub struct BoundSmokeServer {
    pub base_url: String,
    pub port: u16,
    pub stop_token: String,
    pub device_id: String,
    /// 与 serve 栈共享的 preview registry（live/expired 场景由 smoke 直接写入）。
    pub preview_registry: WorkbenchBrowserPreviewRegistry,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl BoundSmokeServer {
    /// 关闭绑定服务器。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 结束后必须释放端口，避免并行 case 冲突。
    ///
    /// Code Logic（这个函数做什么）:
    ///     发送 shutdown 信号并 await join handle。
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

/// smoke 共享状态：stop token + 业务写是否到达 handler + 真实 preview registry。
#[derive(Clone)]
struct SmokeState {
    stop_token: String,
    write_reached: Arc<AtomicBool>,
    stop_reached: Arc<AtomicBool>,
    /// 真实 browser preview registry：bound smoke 验证 null Origin 仅在 live session 后放行。
    preview_registry: WorkbenchBrowserPreviewRegistry,
}

/// 启动绑定到 loopback 临时端口的边界 smoke 服务器。
///
/// Business Logic（为什么需要这个函数）:
///     真实 TCP ConnectInfo 只能覆盖 loopback 客户端；Host/Origin/body/stop 仍需经完整 serve 栈验证。
///
/// Code Logic（这个函数做什么）:
///     bind `127.0.0.1:0`，按实际端口构造 BrowserGuardParams，spawn `axum::serve`，
///     返回 base_url/port/token 与 shutdown 通道。
pub async fn spawn_bound_smoke_server() -> BoundSmokeServer {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind smoke listener");
    let addr = listener.local_addr().expect("local_addr");
    let port = addr.port();
    let device_id = "smoke-device".to_string();
    let stop_token = "smoke-control-token-fixed".to_string();
    let preview_registry = WorkbenchBrowserPreviewRegistry::new();
    let state = SmokeState {
        stop_token: stop_token.clone(),
        write_reached: Arc::new(AtomicBool::new(false)),
        stop_reached: Arc::new(AtomicBool::new(false)),
        preview_registry: preview_registry.clone(),
    };
    let params = browser_guard_params(&device_id, port);
    let app = smoke_router(params, state.clone());

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();
        let server = axum::serve(listener, make_svc).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(err) = server.await {
            eprintln!("lan_trust_boundary smoke server error: {err}");
        }
    });

    // 等待 listener 就绪（短 sleep 足够；失败由后续 HTTP 断言暴露）。
    tokio::time::sleep(Duration::from_millis(20)).await;

    BoundSmokeServer {
        base_url: format!("http://127.0.0.1:{port}"),
        port,
        stop_token,
        device_id,
        preview_registry,
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    }
}

/// 构造与生产 middleware 顺序一致的最小业务 Router。
///
/// Business Logic（为什么需要这个函数）:
///     全量 AppState 过重；用 probe handler 验证边界 middleware 与真实 ConnectInfo 即可。
///
/// Code Logic（这个函数做什么）:
///     注册 health/read/write/stop/proxy/chunk 路由；layer 顺序 last=outermost：
///     body limit → envelope → browser_guard → lan_socket_gate → request_id。
fn smoke_router(params: BrowserGuardParams, state: SmokeState) -> Router {
    Router::new()
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({"ok": true, "route": "health"})) }),
        )
        .route(
            "/api/sync/pull",
            post(|| async { Json(serde_json::json!({"prompts": [], "route": "sync_pull"})) }),
        )
        .route(
            "/api/mobile/workbench/files/save-text",
            post(smoke_save_text_handler),
        )
        .route("/api/backend/control/stop", post(smoke_stop_handler))
        .route(
            "/api/workbench/browser/proxy/:previewId/*path",
            get(smoke_preview_proxy_handler),
        )
        .route(
            "/api/transfer/chunk/:id",
            post(smoke_transfer_chunk_handler).layer(DefaultBodyLimit::max(CHUNK_SIZE)),
        )
        .route(
            "/probe",
            get(|| async { "reached" }).post(smoke_probe_post_handler),
        )
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .layer(axum::middleware::from_fn(envelope_fallback_middleware))
        .layer(axum::middleware::from_fn(move |req, next| {
            let params = params.clone();
            async move { browser_request_guard_with_params(params, req, next).await }
        }))
        .layer(axum::middleware::from_fn(lan_socket_gate))
        .layer(axum::middleware::from_fn(request_id_middleware))
        .with_state(state)
}

/// 业务写 probe：标记 write_reached。
///
/// Business Logic（为什么需要这个函数）:
///     证明合法 loopback/LAN 无凭据写路径可进入 handler。
///
/// Code Logic（这个函数做什么）:
///     置位 AtomicBool 并返回 JSON ok。
async fn smoke_save_text_handler(State(state): State<SmokeState>) -> Json<Value> {
    state.write_reached.store(true, Ordering::SeqCst);
    Json(serde_json::json!({"ok": true, "route": "save_text"}))
}

/// preview proxy probe：真实 registry 会话裁决 + Origin 最终判定。
///
/// Business Logic（为什么需要这个函数）:
///     bound smoke 必须证明：全局 guard 延期 null Origin 后，handler 用真实 registry
///     在 live session 上接受 null，并对 unknown/expired 返回 404；非 null 跨站在会话层 403。
///
/// Code Logic（这个函数做什么）:
///     直接调用生产 `accept_preview_request_with_origin`：lookup registry → null/同源 Origin 终判；
///     成功返回 JSON；失败映射 ApiError 状态码。WS upgrade 头仅验证会话层，不桥接上游。
async fn smoke_preview_proxy_handler(
    State(state): State<SmokeState>,
    Path((preview_id, path)): Path<(String, String)>,
    req: Request<Body>,
) -> Response {
    match accept_preview_request_with_origin(&state.preview_registry, &preview_id, req.headers()) {
        Ok(session) => {
            let is_ws = is_websocket_upgrade_headers(req.headers());
            Json(serde_json::json!({
                "ok": true,
                "route": "preview_proxy",
                "previewId": session.preview_id,
                "path": path,
                "sessionValidation": "registry_live_session",
                "websocketUpgrade": is_ws
            }))
            .into_response()
        }
        Err(err) => err.into_response(),
    }
}

/// 识别 WebSocket upgrade 请求头（smoke 不桥接上游，只验证会话层）。
///
/// Business Logic（为什么需要这个函数）:
///     smoke 需要覆盖 WS upgrade 头 + Origin:null 的会话裁决，但不必真连 Vite。
///
/// Code Logic（这个函数做什么）:
///     Connection 含 upgrade 且 Upgrade=websocket（大小写不敏感）。
fn is_websocket_upgrade_headers(headers: &HeaderMap) -> bool {
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

/// transfer chunk probe：消费 body 以触发 limit。
///
/// Business Logic（为什么需要这个函数）:
///     验证 route-local 960KiB limit 在 handler 读 body 前生效。
///
/// Code Logic（这个函数做什么）:
///     丢弃 body 返回 success JSON。
async fn smoke_transfer_chunk_handler(body: axum::body::Bytes) -> Json<Value> {
    let _ = body;
    Json(serde_json::json!({"success": true, "route": "transfer_chunk"}))
}

/// probe POST：消费 body。
///
/// Business Logic（为什么需要这个函数）:
///     全局 32MiB limit 仅在 body 被提取时触发。
///
/// Code Logic（这个函数做什么）:
///     读取 Bytes 后返回 "reached"。
async fn smoke_probe_post_handler(body: axum::body::Bytes) -> &'static str {
    let _ = body;
    "reached"
}

/// smoke stop handler：loopback + token。
///
/// Business Logic（为什么需要这个函数）:
///     与生产 stop 相同：先 loopback，再 token；token 不扩散到业务 API。
///
/// Code Logic（这个函数做什么）:
///     `require_loopback_peer` → 比较 control_token → 标记 stop_reached。
async fn smoke_stop_handler(
    State(state): State<SmokeState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    Json(request): Json<StopRequest>,
) -> P2pResult<Json<StopResponse>> {
    require_loopback_peer(peer.ip(), &context)?;
    if request.control_token.is_empty() || request.control_token != state.stop_token {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            &context,
        ));
    }
    state.stop_reached.store(true, Ordering::SeqCst);
    Ok(Json(StopResponse { ok: true }))
}

/// 构造仅含 gate 的 oneshot Router（注入 peer 证据）。
///
/// Business Logic（为什么需要这个函数）:
///     公网 peer 无法在 loopback listener 上真实出现；oneshot 注入 ConnectInfo 并标注 injected。
///
/// Code Logic（这个函数做什么）:
///     `/probe` + lan_socket_gate + request_id。
fn injected_gate_router() -> Router {
    Router::new()
        .route("/probe", get(|| async { "reached" }))
        .layer(axum::middleware::from_fn(lan_socket_gate))
        .layer(axum::middleware::from_fn(request_id_middleware))
}

/// 读取 JSON body。
///
/// Business Logic（为什么需要这个函数）:
///     断言错误信封 code/message。
///
/// Code Logic（这个函数做什么）:
///     消费 response body 并解析 JSON。
async fn response_json(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// 执行完整 LAN trust boundary smoke 矩阵（同步入口，自建 runtime）。
///
/// Business Logic（为什么需要这个函数）:
///     integration test crate 不依赖 axum；由库内 harness 自建 tokio runtime 跑矩阵。
///
/// Code Logic（这个函数做什么）:
///     `Runtime::new().block_on(run_matrix_async())`，失败直接 panic 带上下文。
pub fn run_lan_trust_boundary_smoke() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_matrix_async());
}

/// 异步矩阵主体。
///
/// Business Logic（为什么需要这个函数）:
///     串联 pure classifier、injected peer、bound HTTP 与 stop/body 场景。
///
/// Code Logic（这个函数做什么）:
///     分场景断言；打印覆盖摘要。
async fn run_matrix_async() {
    println!("=== LAN trust boundary smoke matrix ===");

    // 1) pure classifier（与 unit 表一致的代表样例）
    assert_eq!(
        classify_peer_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
        LanPeerScope::Loopback
    );
    assert_eq!(
        classify_peer_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
        LanPeerScope::Lan
    );
    assert_eq!(
        classify_peer_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
        LanPeerScope::Denied
    );
    println!("[ok] classifier sample ranges");

    // 2) injected denied + forwarded spoof（不得当作真实公网 peer）
    {
        let public_peer = SocketAddr::from((Ipv4Addr::new(8, 8, 8, 8), 54321));
        let mut request = Request::builder()
            .uri("/probe")
            .header("x-cc-request-id", "smoke-injected-denied")
            .header("x-forwarded-for", "127.0.0.1")
            .header("forwarded", "for=127.0.0.1")
            .header("x-real-ip", "192.168.1.1")
            .body(Body::empty())
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(public_peer));
        let response = injected_gate_router()
            .oneshot(request)
            .await
            .expect("router");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{INJECTED_PEER_EVIDENCE}: public peer must be denied even with spoofed XFF"
        );
        let body = response_json(response).await;
        assert_eq!(body["code"], "forbidden");
        println!("[ok] {INJECTED_PEER_EVIDENCE} denied public peer + forwarded spoof ignored");
    }

    // 3) injected LAN peer 业务放行（非真实多机）
    {
        let lan_peer = SocketAddr::from((Ipv4Addr::new(192, 168, 1, 50), 40000));
        let mut request = Request::builder()
            .uri("/probe")
            .header("x-cc-request-id", "smoke-injected-lan")
            .body(Body::empty())
            .expect("request");
        request.extensions_mut().insert(ConnectInfo(lan_peer));
        let response = injected_gate_router()
            .oneshot(request)
            .await
            .expect("router");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{INJECTED_PEER_EVIDENCE}: LAN peer must reach handler without credentials"
        );
        println!("[ok] {INJECTED_PEER_EVIDENCE} LAN peer business allow (no credentials)");
    }

    // 4) bound server：真实 loopback ConnectInfo
    let server = spawn_bound_smoke_server().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client");
    let host = format!("127.0.0.1:{}", server.port);
    let origin_ok = format!("http://{}", host);

    // 4a credential-free loopback business read
    {
        let resp = client
            .get(format!("{}/api/health", server.base_url))
            .header("host", &host)
            .send()
            .await
            .expect("health");
        assert_eq!(resp.status(), StatusCode::OK, "loopback health read");
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["ok"], true);
        println!("[ok] bound loopback credential-free business READ");
    }

    // 4b native no-Origin write (P2P 互操作)
    {
        let resp = client
            .post(format!("{}/api/sync/pull", server.base_url))
            .header("host", &host)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("sync pull");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "native no-Origin business write/read path"
        );
        println!("[ok] bound native no-Origin interoperability");
    }

    // 4c same-origin mobile write
    {
        let resp = client
            .post(format!(
                "{}/api/mobile/workbench/files/save-text",
                server.base_url
            ))
            .header("host", &host)
            .header("origin", &origin_ok)
            .header("content-type", "application/json")
            .body(r#"{"path":"a.md","content":"x"}"#)
            .send()
            .await
            .expect("mobile write");
        assert_eq!(resp.status(), StatusCode::OK, "same-origin mobile write");
        println!("[ok] bound same-origin mobile write (credential-free)");
    }

    // 4d hostile Host
    {
        let resp = client
            .get(format!("{}/api/health", server.base_url))
            .header("host", format!("evil.example:{}", server.port))
            .send()
            .await
            .expect("hostile host");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "hostile Host");
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["code"], "forbidden");
        println!("[ok] bound hostile Host rejected");
    }

    // 4e valid Host with wrong port
    {
        let resp = client
            .get(format!("{}/api/health", server.base_url))
            .header("host", "127.0.0.1:9")
            .send()
            .await
            .expect("wrong port host");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "wrong Host port");
        println!("[ok] bound wrong Host port rejected");
    }

    // 4f cross-origin ordinary API
    {
        let resp = client
            .post(format!(
                "{}/api/mobile/workbench/files/save-text",
                server.base_url
            ))
            .header("host", &host)
            .header("origin", "http://evil.test")
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .expect("cross origin");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "cross-origin");
        println!("[ok] bound cross-origin ordinary API rejected");
    }

    // 4g ordinary API Origin: null
    {
        let resp = client
            .get(format!("{}/api/health", server.base_url))
            .header("host", &host)
            .header("origin", "null")
            .send()
            .await
            .expect("null origin");
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "ordinary Origin:null");
        println!("[ok] bound ordinary API Origin:null rejected");
    }

    // 4h form/multipart simple write content-type
    {
        let resp = client
            .post(format!("{}/api/sync/pull", server.base_url))
            .header("host", &host)
            .header("content-type", "text/plain")
            .body("x")
            .send()
            .await
            .expect("text/plain write");
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "simple content-type write"
        );
        println!("[ok] bound simple Content-Type ordinary write rejected");
    }

    // 4i preview proxy：真实 registry + Origin:null / expired / WS upgrade 头
    {
        let live = server.preview_registry.create_local_for_test(
            "smoke-project",
            None,
            "http://127.0.0.1:5173/",
        );
        // live + Origin:null → 200
        let resp = client
            .get(format!(
                "{}/api/workbench/browser/proxy/{}/index.html",
                server.base_url, live.preview_id
            ))
            .header("host", &host)
            .header("origin", "null")
            .send()
            .await
            .expect("preview live null");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "live preview + Origin:null must succeed"
        );
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["route"], "preview_proxy");
        assert_eq!(body["sessionValidation"], "registry_live_session");
        assert_eq!(body["previewId"], live.preview_id);
        assert_eq!(body["websocketUpgrade"], false);
        println!("[ok] bound live preview Origin:null HTTP accepted via real registry");

        // unknown + Origin:null → 404（不得访问其它 /api/* 已在 4g 覆盖）
        let resp = client
            .get(format!(
                "{}/api/workbench/browser/proxy/missing-preview/index.html",
                server.base_url
            ))
            .header("host", &host)
            .header("origin", "null")
            .send()
            .await
            .expect("preview missing null");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unknown preview + Origin:null must 404"
        );
        println!("[ok] bound unknown preview Origin:null rejected 404");

        // expired + Origin:null → 404
        let expired = server.preview_registry.create_local_for_test(
            "smoke-project",
            None,
            "http://127.0.0.1:5173/",
        );
        server
            .preview_registry
            .force_expire_for_test(&expired.preview_id);
        let resp = client
            .get(format!(
                "{}/api/workbench/browser/proxy/{}/index.html",
                server.base_url, expired.preview_id
            ))
            .header("host", &host)
            .header("origin", "null")
            .send()
            .await
            .expect("preview expired null");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "expired preview + Origin:null must 404"
        );
        println!("[ok] bound expired preview Origin:null rejected 404");

        // live + WS upgrade 头 + Origin:null → 会话层 200（不桥接上游）
        let ws_live = server.preview_registry.create_local_for_test(
            "smoke-project",
            None,
            "http://127.0.0.1:5173/",
        );
        let resp = client
            .get(format!(
                "{}/api/workbench/browser/proxy/{}/@vite/client",
                server.base_url, ws_live.preview_id
            ))
            .header("host", &host)
            .header("origin", "null")
            .header("connection", "Upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .expect("preview ws null");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "live preview WS upgrade headers + Origin:null must pass session layer"
        );
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["sessionValidation"], "registry_live_session");
        assert_eq!(body["websocketUpgrade"], true);
        println!(
            "[ok] bound live preview WS upgrade headers + Origin:null accepted (no upstream bridge)"
        );

        // live + 跨站 Origin → 403（会话层防御纵深；全局 guard 也会挡，此处走 bound stack）
        let resp = client
            .get(format!(
                "{}/api/workbench/browser/proxy/{}/index.html",
                server.base_url, live.preview_id
            ))
            .header("host", &host)
            .header("origin", "http://evil.example")
            .send()
            .await
            .expect("preview cross origin");
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "live preview + cross-origin must 403"
        );
        println!("[ok] bound live preview cross-origin rejected 403");
    }

    // 4j stop: loopback + valid token
    {
        let resp = client
            .post(format!("{}/api/backend/control/stop", server.base_url))
            .header("host", &host)
            .header("content-type", "application/json")
            .json(&serde_json::json!({"controlToken": server.stop_token}))
            .send()
            .await
            .expect("stop ok");
        assert_eq!(resp.status(), StatusCode::OK, "stop loopback+token");
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["ok"], true);
        println!("[ok] bound stop accepts loopback + valid token");
    }

    // 4k stop: loopback + bad token
    {
        let resp = client
            .post(format!("{}/api/backend/control/stop", server.base_url))
            .header("host", &host)
            .header("content-type", "application/json")
            .json(&serde_json::json!({"controlToken": "wrong-token"}))
            .send()
            .await
            .expect("stop bad token");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "stop bad token");
        let body: Value = resp.json().await.expect("json");
        assert_eq!(body["code"], "unauthorized");
        println!("[ok] bound stop rejects invalid token on loopback");
    }

    // 4l stop: injected non-loopback + valid token（INJECTED）
    {
        let params = browser_guard_params(&server.device_id, server.port);
        let state = SmokeState {
            stop_token: server.stop_token.clone(),
            write_reached: Arc::new(AtomicBool::new(false)),
            stop_reached: Arc::new(AtomicBool::new(false)),
            preview_registry: server.preview_registry.clone(),
        };
        let router = smoke_router(params, state);
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/api/backend/control/stop")
            .header("host", &host)
            .header("content-type", "application/json")
            .header("x-cc-request-id", "smoke-stop-lan")
            .body(Body::from(
                serde_json::json!({"controlToken": server.stop_token}).to_string(),
            ))
            .expect("request");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((
                Ipv4Addr::new(192, 168, 1, 9),
                50000,
            ))));
        let response = router.oneshot(request).await.expect("router");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{INJECTED_PEER_EVIDENCE}: stop must reject non-loopback even with valid token"
        );
        let body = response_json(response).await;
        assert_eq!(body["code"], "forbidden");
        println!("[ok] {INJECTED_PEER_EVIDENCE} stop rejects LAN peer with valid token");
    }

    // 4m global body limit via bound server（小一些的超限：用 oneshot 大 body 更稳）
    {
        let params = browser_guard_params(&server.device_id, server.port);
        let state = SmokeState {
            stop_token: server.stop_token.clone(),
            write_reached: Arc::new(AtomicBool::new(false)),
            stop_reached: Arc::new(AtomicBool::new(false)),
            preview_registry: server.preview_registry.clone(),
        };
        let router = smoke_router(params, state);
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/probe")
            .header("host", &host)
            .header("content-type", "application/octet-stream")
            .body(Body::from(vec![0u8; BODY_LIMIT_BYTES + 1]))
            .expect("request");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1))));
        let response = router.oneshot(request).await.expect("router");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = response_json(response).await;
        assert_eq!(body["code"], "payload_too_large");
        println!("[ok] global 32MiB body limit envelope (oneshot through production-like stack)");
    }

    // 4n transfer chunk route-local limit
    {
        let params = browser_guard_params(&server.device_id, server.port);
        let state = SmokeState {
            stop_token: server.stop_token.clone(),
            write_reached: Arc::new(AtomicBool::new(false)),
            stop_reached: Arc::new(AtomicBool::new(false)),
            preview_registry: server.preview_registry.clone(),
        };
        let router = smoke_router(params, state);
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/api/transfer/chunk/smoke-id")
            .header("host", &host)
            .header("content-type", "application/octet-stream")
            .body(Body::from(vec![0u8; CHUNK_SIZE + 1]))
            .expect("request");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1))));
        let response = router.oneshot(request).await.expect("router");
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "chunk route limit CHUNK_SIZE+1"
        );
        println!("[ok] transfer chunk route-local 960KiB limit");
    }

    server.shutdown().await;

    println!("=== LAN trust boundary smoke matrix PASS ===");
    println!("NOTE: multi-host mDNS / phone QR / real public peer path = NOT VERIFIED here");
    println!(
        "NOTE: preview registry live/expired + Origin:null HTTP/WS-headers verified on bound stack"
    );
    println!(
        "NOTE: real WebSocket bridge to Vite upstream = NOT VERIFIED here (session layer only)"
    );
    println!("NOTE: browser L1 Playwright = S6 ownership (not duplicated)");
}
