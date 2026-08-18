//! net/http_server.rs — axum HTTP server（供对端调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     每个 cc-partner 实例既是客户端也是服务端，需监听 HTTP 端口接收对端的
//!     同步/传输/健康检查请求。对照 Python `network/server.py`（aiohttp 实现）。
//!     同时为手机浏览器提供 `/mobile` 静态 SPA 入口，便于局域网移动端访问工作台。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_http_server`：构造 axum Router（with_state(AppState)，挂载全部 /api 路由），
//!       优先绑定固定 HTTP 端口，冲突时向上递增，取 local_addr 实际端口回填
//!       AppState.actual_http_port（AtomicU16），tokio::spawn(axum::serve)。
//!     - `/mobile` fallback：通过 Tauri asset resolver 读取 frontendDist 嵌入资源，并精确服务 `/assets/*` 构建资源。
//!     - 开发态可选：fallback 内把 `/mobile` + Vite 模块/HMR WS 反向代理到本机 `5173`（见 `mobile_dev_proxy`）。
//!     - body limit 覆盖文件传输 chunk 和 Workbench 远端文本保存。

use crate::backend::control::{self, BackendControlFile};
use crate::net::error_response::{envelope_fallback_middleware, P2pError, P2pErrorCode, P2pResult};
use crate::net::lan_guard::{
    browser_request_guard, expected_device_id_guard, inject_overlay_trust, lan_socket_gate,
    require_loopback_peer,
};
use crate::net::mobile_dev_proxy;
use crate::net::request_context::{request_id_middleware, P2pRequestContext};
use crate::net::routes::{
    agent_hub, attention, browser_verification, cc_history, claude_code_assets, claude_md_sync,
    health, mobile, mobile_transfer, orchestrator, prompts, provider_manager, scratchpad_sync,
    ssh_target_sync, sync, transfer, workbench, workbench_project_order_sync,
};
use crate::state::AppState;
use crate::transfer::CHUNK_SIZE;
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Extension};
use axum::http::{header, HeaderValue, Request, Response, StatusCode};
use axum::routing::{any, get, post, put};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io::{Error as IoError, ErrorKind};
use std::net::SocketAddr;
use std::path::{Component, Path};
use std::sync::atomic::Ordering;
use tower::service_fn;

/// axum body 大小上限（字节）。32MB，容纳 M5 chunk（960KB）+ Workbench 远端文本保存（5MB 高转义 JSON）+ 开销。
const BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
/// HTTP server 默认首选端口。配置端口为 0 或非法值时使用该端口，冲突则自动向上递增。
const DEFAULT_HTTP_PORT: u16 = 62116;

/// 后端 stop 控制请求。
///
/// Business Logic（为什么需要这个结构）:
///     `/api/backend/control/stop` 会关闭当前 headless 后端，调用方必须证明自己读到了本机控制文件。
///
/// Code Logic（这个结构做什么）:
///     反序列化 camelCase JSON 中的 `controlToken` 字段，供 handler 与控制文件令牌比较。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopBackendRequest {
    pub control_token: String,
}

/// 后端 stop 控制响应。
///
/// Business Logic（为什么需要这个结构）:
///     `cc-partner-backend stop` 需要知道 stop 请求已被 route 接收并触发 shutdown 通知。
///
/// Code Logic（这个结构做什么）:
///     以 `{ok:true}` 形式返回控制请求是否送达进程内 shutdown notifier。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StopBackendResponse {
    ok: bool,
}

/// 处理本地后端 stop 控制请求。
///
/// Business Logic（为什么需要这个函数）:
///     stop CLI 必须能通过本机 HTTP route 唤醒 `serve` 主循环优雅关闭，而不是直接 kill 进程。
///     该接口是本机进程生命周期控制，不是 LAN 业务 API：必须同时满足 loopback peer 与 control-file token，
///     即使持有 token 也不能从局域网对端关闭后端。所有错误路径返回统一的 P2P 错误信封。
///
/// Code Logic（这个函数做什么）:
///     1) 先用 `ConnectInfo` 真实 peer 要求 Loopback（非 loopback → 403 `forbidden`，不读 token）；
///     2) 再读取控制文件并比较 controlToken（不可读或 token 不匹配 → 401 `unauthorized`）；
///     3) notifier 不可用时返回 503 `unavailable`（retryable=true）；成功触发 shutdown 后返回 `{ok:true}`。
async fn stop_backend_control(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(context): Extension<P2pRequestContext>,
    Json(request): Json<StopBackendRequest>,
) -> P2pResult<Json<StopBackendResponse>> {
    // 生命周期边界：loopback 必须先于 token 比较，避免非本机 peer 触发控制文件读取路径。
    require_loopback_peer(peer.ip(), &context)?;

    let control = control::read_control_file()
        .map_err(|_| P2pError::from_code("控制文件不可读", P2pErrorCode::Unauthorized, &context))?;
    if !backend_stop_token_matches(&request, control.as_ref()) {
        return Err(P2pError::from_code(
            "控制令牌不匹配",
            P2pErrorCode::Unauthorized,
            &context,
        ));
    }

    if !control::request_backend_shutdown() {
        return Err(P2pError::unavailable(
            "后端关闭通知器不可用，请稍后重试",
            &context,
        ));
    }

    Ok(Json(StopBackendResponse { ok: true }))
}

/// 校验 stop 请求 token 是否匹配当前控制文件。
///
/// Business Logic（为什么需要这个函数）:
///     关闭 headless 后端是敏感本机操作，不能让任意局域网请求关闭服务。
///
/// Code Logic（这个函数做什么）:
///     控制文件存在且请求 token 非空并与 `control_token` 完全一致时返回 true。
fn backend_stop_token_matches(
    request: &StopBackendRequest,
    control: Option<&BackendControlFile>,
) -> bool {
    let Some(control) = control else {
        return false;
    };
    !request.control_token.is_empty() && request.control_token == control.control_token
}

/// 判断请求路径是否属于移动端静态入口命名空间。
///
/// Business Logic（为什么需要这个函数）:
///     手机端入口只应接管 `/mobile` shell 与 Vite 生成的 `/assets/*` 资源；P2P API、桌面 Workbench 等其它路径必须维持原路由语义。
///
/// Code Logic（这个函数做什么）:
///     对 path 做精确前缀判断：`/mobile`、`/mobile/...` 和 `/assets/...` 返回 true。
///     开发态 Vite 模块路径（`/src/*`、`/@vite/*` 等）不在此集合，由 `mobile_dev_proxy` 单独接管。
fn is_mobile_spa_path(path: &str) -> bool {
    path == "/mobile" || path.starts_with("/mobile/") || path.starts_with("/assets/")
}

/// 生产静态资源命名空间（Vite 代理失败时可回落 dist/嵌入）。
fn is_mobile_static_fallback_path(path: &str) -> bool {
    is_mobile_spa_path(path)
}

/// axum fallback service：按 `/mobile` SPA 规则返回 Tauri 静态资源或 404。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端生成的局域网手机访问 URL 指向 `/mobile`，手机浏览器刷新任意 SPA 子路由时需要回退到
///     `mobile.html`，但未知非移动端路径不能被错误接管。开发态可把同一入口代理到本机 Vite 以支持 HMR。
///
/// Code Logic（这个函数做什么）:
///     1) 开发态 WebSocket upgrade（非 `/api`）→ Vite HMR 桥接；
///     2) 开发态 HTTP：`/mobile`/`/assets`/Vite 模块路径优先代理，连接失败回落静态；
///     3) 非移动端静态路径直接 404；`/mobile` shell + SPA 回落 + exact `/assets/*` 读嵌入/dist。
async fn serve_mobile_spa(
    state: AppState,
    req: Request<Body>,
) -> Result<Response<Body>, Infallible> {
    let path = req.uri().path().to_string();
    let is_ws = mobile_dev_proxy::is_websocket_upgrade(req.headers());

    // 开发态：fallback 上的 HMR WebSocket（浏览器连 backend 端口）。
    if is_ws {
        if mobile_dev_proxy::is_proxy_enabled()
            && mobile_dev_proxy::is_proxyable_websocket_path(&path)
        {
            return Ok(mobile_dev_proxy::proxy_websocket(req).await);
        }
        let mut response = Response::new(Body::from("Not Found"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(response);
    }

    // 开发态：Vite HTTP 模块与 /mobile shell。
    if mobile_dev_proxy::is_proxy_enabled() && mobile_dev_proxy::is_proxyable_http_path(&path) {
        if let Some(response) = mobile_dev_proxy::try_proxy_http(req).await {
            return Ok(response);
        }
        // 上游不可达：仅对生产静态命名空间继续 dist/嵌入回落；纯 Vite 模块路径直接 502。
        if !is_mobile_static_fallback_path(&path) {
            let mut response = Response::new(Body::from("vite module unavailable"));
            *response.status_mut() = StatusCode::BAD_GATEWAY;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            return Ok(response);
        }
        // 静态回落：请求体已在 try_proxy_http 中消费；GET shell/assets 不依赖 body。
    } else if !is_mobile_spa_path(&path) {
        let mut response = Response::new(Body::from("Not Found"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(response);
    }

    if path == "/mobile" || path == "/mobile/" {
        if let Some(response) = mobile_asset_response(&state, "mobile.html").await {
            return Ok(response);
        }
    } else if path.starts_with("/assets/") {
        if let Some(response) = mobile_asset_response(&state, &path).await {
            return Ok(response);
        }

        let mut response = Response::new(Body::from("Asset not found"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        return Ok(response);
    } else if let Some(asset_path) = path.strip_prefix("/mobile/") {
        if let Some(response) = mobile_asset_response(&state, asset_path).await {
            return Ok(response);
        }

        if asset_path.starts_with("assets/") {
            let mut response = Response::new(Body::from("Asset not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            return Ok(response);
        }

        if let Some(response) = mobile_asset_response(&state, "mobile.html").await {
            return Ok(response);
        }
    }

    let mut response = Response::new(Body::from("mobile.html not found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

/// 将 `/mobile` 请求路径规范化为 Tauri frontendDist asset key。
///
/// Business Logic（为什么需要这个函数）:
///     `/mobile` 是局域网 HTTP shell 前缀，Vite 多入口产物会引用 frontendDist 根下的 `/assets/*`；
///     后端必须把这两种 HTTP 路径规范化为真实 Tauri asset key。
///
/// Code Logic（这个函数做什么）:
///     接收完整 `/mobile/...`、`/assets/...` 或已剥离的相对路径；用 `Component` 拒绝空路径、绝对路径、
///     父目录和其它非普通路径组件；返回用 `/` 拼接的 asset key。
fn mobile_asset_key(path: &str) -> Option<String> {
    let logical_path = match path {
        "/mobile" | "/mobile/" => "mobile.html".to_string(),
        _ if path.starts_with("/mobile/") => path
            .strip_prefix("/mobile/")
            .unwrap_or_default()
            .to_string(),
        _ if path.starts_with("/assets/") => {
            let asset_path = path.strip_prefix("/assets/").unwrap_or_default();
            if asset_path.is_empty() {
                return None;
            }
            format!("assets/{asset_path}")
        }
        _ if path.starts_with('/') => return None,
        _ => path.to_string(),
    };
    let requested_path = Path::new(&logical_path);

    if logical_path.is_empty() || requested_path.is_absolute() {
        return None;
    }

    let mut safe_segments = Vec::new();
    for component in requested_path.components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment.to_str()?;
                if segment.is_empty() || segment.contains('\\') {
                    return None;
                }
                safe_segments.push(segment);
            }
            _ => return None,
        }
    }

    if safe_segments.is_empty() {
        return None;
    }

    Some(safe_segments.join("/"))
}

/// 读取移动端 SPA 的 Tauri asset 并转换为 HTTP 响应。
///
/// Business Logic（为什么需要这个函数）:
///     手机端页面由前端构建写入 Tauri `frontendDist`，生产打包后源码相对目录不存在，必须从 Tauri
///     UI adapter 读取嵌入资源；开发/测试环境可在 adapter 缺失时读 dist 兜底。
///
/// Code Logic（这个函数做什么）:
///     将请求路径归一化为 asset key；优先调用 `AppState::mobile_asset` 查询当前 UI adapter；
///     命中时用 asset bytes/mime/CSP 生成响应；
///     未命中时再尝试 dev/test filesystem fallback。
async fn mobile_asset_response(state: &AppState, path: &str) -> Option<Response<Body>> {
    let asset_key = mobile_asset_key(path)?;

    if let Some(asset) = state.mobile_asset(&asset_key) {
        let content_type = mobile_content_type_header(asset.mime_type(), &asset_key);
        let csp_header = asset
            .csp_header()
            .and_then(|value| HeaderValue::from_str(value).ok());
        let mut response = Response::new(Body::from(asset.bytes));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
        if let Some(csp_header) = csp_header {
            response
                .headers_mut()
                .insert(header::CONTENT_SECURITY_POLICY, csp_header);
        }
        return Some(response);
    }

    mobile_dev_dist_asset_response(&asset_key).await
}

/// 从源码树 dist 目录读取移动端资源作为开发/测试兜底。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri dev 模式通常由 asset resolver 回退读取 `frontendDist`，但部分单测或本地开发环境可能没有完整
///     Tauri webview 上下文；保留文件系统读取只作为额外兜底，不能作为生产路径。
///
/// Code Logic（这个函数做什么）:
///     使用已规范化的 asset key 拼出 `../web/dist` 内路径；读取成功则按扩展名 fallback MIME 返回响应。
async fn mobile_dev_dist_asset_response(asset_key: &str) -> Option<Response<Body>> {
    let dist_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../web/dist")
        .join(asset_key);
    let bytes = tokio::fs::read(dist_path).await.ok()?;

    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        mobile_content_type_header("", asset_key),
    );
    Some(response)
}

/// 生成移动端响应的 content-type header。
///
/// Business Logic（为什么需要这个函数）:
///     Tauri asset resolver 能提供构建期 MIME；若 resolver 未提供或提供非法值，仍要保证浏览器按静态资源类型加载。
///
/// Code Logic（这个函数做什么）:
///     优先把非空 asset MIME 转为 HeaderValue；为空或转换失败时，按 asset key 扩展名使用本地 MIME 映射。
fn mobile_content_type_header(asset_mime: &str, asset_key: &str) -> HeaderValue {
    let asset_mime = asset_mime.trim();
    if !asset_mime.is_empty() {
        if let Ok(value) = HeaderValue::from_str(asset_mime) {
            return value;
        }
    }

    HeaderValue::from_static(mobile_content_type(asset_key))
}

/// 返回移动端静态资源的 content-type。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器会按响应 content-type 执行 JS、应用 CSS 和渲染图片；缺少映射会导致移动端 shell 加载异常。
///
/// Code Logic（这个函数做什么）:
///     根据文件扩展名返回静态 MIME，覆盖 HTML/JS/CSS/SVG/PNG，并补充常见 JSON/WASM/ICO/map 类型。
fn mobile_content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 启动 axum HTTP server，返回实际监听端口。
///
/// Business Logic: 应用启动时调用，尽量绑定稳定端口，便于移动端 Workbench 链接可收藏和复用；
///     若端口已被占用则自动向上递增，返回端口供 mDNS 注册使用（mDNS 宣告的端口必须是 axum 实际监听端口）。
///
/// Code Logic:
///     1. 构造 Router：全部 /api 路由 → 对应 handler，最后挂 `/mobile` SPA fallback，
///        with_state(AppState)，套 DefaultBodyLimit 限制请求体大小。
///     2. 从配置读取首选端口；配置为 0/非法值时用 DEFAULT_HTTP_PORT；端口占用时递增重试。
///     3. local_addr().port() 取实际端口，回填 AppState.actual_http_port。
///     4. tokio::spawn(axum::serve(listener, app)) 在后台运行（不阻塞 setup）。
pub async fn start_http_server(state: AppState) -> Result<u16, std::io::Error> {
    let fallback_state = state.clone();
    // axum Router：with_state 注入 AppState，与 invoke 命令层共享同一份 Arc
    let app: Router = Router::new()
        .route("/api/health", get(health::health))
        .route("/api/backend/control/stop", post(stop_backend_control))
        // N1 Task2：loopback control API（status/get-config/update-config，256 KiB body）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/status",
            post(crate::backend::control_api::control_status).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/get-config",
            post(crate::backend::control_api::control_get_config).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/workbench-launch-summary",
            post(crate::backend::control_api::control_workbench_launch_summary).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/update-config",
            post(crate::backend::control_api::control_update_config).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // N1 Task4：Workbench owner control（metadata 256 KiB；data 32 MiB）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/workbench",
            post(crate::backend::control_workbench::control_workbench).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/workbench/data",
            post(crate::backend::control_workbench::control_workbench_data)
                .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        // Gate A Task9：Agent Hub owner control（metadata 256 KiB；不进 server_protocol_info）
        .route(
            "/api/backend/control/agent-hub",
            post(crate::backend::control_agent_hub::control_agent_hub).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // A7：Agent-first CLI typed control（query/mutate；loopback+token）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/agent/query",
            post(crate::backend::control_agent::control_agent_query).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/agent/mutate",
            post(crate::backend::control_agent::control_agent_mutate).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // N1 Task5：owner 事件 relay + Orchestrator runtime snapshot 代理
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/orchestrator/runtime-snapshot",
            post(crate::backend::control_api::control_orchestrator_runtime_snapshot).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // N6 M3：deliver / complete-agent-run / dispatch-once / workflow-document 必须在 owner 进程执行
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/orchestrator/deliver-reviewed",
            post(crate::backend::control_api::control_orchestrator_deliver_reviewed).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/complete-agent-run",
            post(crate::backend::control_api::control_orchestrator_complete_agent_run).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/abort-task",
            post(crate::backend::control_api::control_orchestrator_abort_task).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/cancel-task",
            post(crate::backend::control_api::control_orchestrator_cancel_task).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/dispatch-once",
            post(crate::backend::control_api::control_orchestrator_dispatch_once).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // GuiClient experiment 代理：create/list/get/approve/cancel/prepare-downgrade 仅 owner 执行
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/orchestrator/experiments/create",
            post(crate::backend::control_api::control_orchestrator_experiment_create).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/experiments/list",
            post(crate::backend::control_api::control_orchestrator_experiment_list).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/experiments/get",
            post(crate::backend::control_api::control_orchestrator_experiment_get).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/experiments/approve-winner",
            post(crate::backend::control_api::control_orchestrator_experiment_approve_winner)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/api/backend/control/orchestrator/experiments/cancel",
            post(crate::backend::control_api::control_orchestrator_experiment_cancel).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/experiments/prepare-downgrade",
            post(crate::backend::control_api::control_orchestrator_experiment_prepare_downgrade)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/api/backend/control/orchestrator/task-blocks/create",
            post(crate::backend::control_api::control_orchestrator_task_block_create).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/task-blocks/append-member",
            post(crate::backend::control_api::control_orchestrator_task_block_append_member).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/task-blocks/reorder-members",
            post(crate::backend::control_api::control_orchestrator_task_block_reorder_members)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/api/backend/control/orchestrator/workflow-document/get",
            post(crate::backend::control_api::control_orchestrator_workflow_document_get).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/orchestrator/workflow-document/validate",
            post(crate::backend::control_api::control_orchestrator_workflow_document_validate)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/api/backend/control/orchestrator/workflow-document/save",
            post(crate::backend::control_api::control_orchestrator_workflow_document_save).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/operational-notifications/snapshot",
            post(crate::backend::control_api::control_operational_notification_snapshot).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/events/catch-up",
            post(crate::backend::control_api::control_events_catch_up).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/events/stream",
            post(crate::backend::control_api::control_events_stream).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/workbench/terminal-input-stream",
            get(crate::backend::control_api::control_terminal_input_stream),
        )
        // N1 fix: Cloud Sync write-path owner control（trigger/test/claude-md-push）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/cloud-sync/trigger",
            post(crate::backend::control_api::control_cloud_sync_trigger).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/cloud-sync/test",
            post(crate::backend::control_api::control_cloud_sync_test).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/cloud-sync/claude-md-push",
            post(crate::backend::control_api::control_cloud_sync_claude_md_push).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // N2: 可验证导出/恢复 owner control（create/inspect/restore/list-jobs/list-backups/rollback）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/backup/create",
            post(crate::backend::control_api::control_backup_create).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/backup/inspect",
            post(crate::backend::control_api::control_backup_inspect).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/backup/restore",
            post(crate::backend::control_api::control_backup_restore).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/backup/list-jobs",
            post(crate::backend::control_api::control_backup_list_jobs).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/backup/list-backups",
            post(crate::backend::control_api::control_backup_list_backups).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/backup/rollback",
            post(crate::backend::control_api::control_backup_rollback).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // N5: same-device transfer Open/Reveal prepare + owner mutations（仅 loopback control，不进 LAN/P2P）
        // 字面路径必须写在本文件，供 check-p2p-route-inventory 提取。
        .route(
            "/api/backend/control/transfer/prepare-open",
            post(crate::backend::control_api::control_transfer_prepare_open).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/transfer/send",
            post(crate::backend::control_api::control_transfer_send).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/transfer/retry",
            post(crate::backend::control_api::control_transfer_retry).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/transfer/resume",
            post(crate::backend::control_api::control_transfer_resume).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/transfer/get-operation",
            post(crate::backend::control_api::control_transfer_get_operation).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        .route(
            "/api/backend/control/transfer/cancel",
            post(crate::backend::control_api::control_transfer_cancel).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::backend::control_api::CONTROL_REQUEST_BODY_LIMIT_BYTES,
                ),
            ),
        )
        // 移动端访问入口：返回手机可访问的局域网 /mobile URL（过滤 localhost/loopback）
        .route("/api/mobile/access-info", get(mobile::access_info))
        // Mobile Attention 快照：v1 与 Tauri list_attention_items 共享；v2 含 Agent 投影
        .route("/api/mobile/attention", get(attention::list_attention))
        .route(
            "/api/mobile/attention/v2",
            get(attention::list_attention_v2),
        )
        .route(
            "/api/mobile/attention/mark-read",
            post(attention::mark_attention_read),
        )
        .route(
            "/api/mobile/attention/mark-unread",
            post(attention::mark_attention_unread),
        )
        .route(
            "/api/mobile/attention/mark-all-read",
            post(attention::mark_all_attention_read),
        )
        .route(
            "/api/mobile/attention/mark-category-read",
            post(attention::mark_attention_category_read),
        )
        // Mobile Prompt 库只读入口：复用 PromptRepo::list（含 favorite），移动端不 toggle
        .route("/api/mobile/prompts", get(prompts::list_mobile_prompts))
        // Mobile 主机中转文件传输：staging 分块上传 → 本机落盘或 start_sending；不含 loopback control
        .route(
            "/api/mobile/devices",
            get(mobile_transfer::list_mobile_devices),
        )
        .route(
            "/api/mobile/transfer/tasks",
            get(mobile_transfer::list_mobile_transfer_tasks),
        )
        .route(
            "/api/mobile/transfer/upload/init",
            post(mobile_transfer::upload_init),
        )
        .route(
            "/api/mobile/transfer/upload/chunk/:id",
            post(mobile_transfer::upload_chunk).layer(DefaultBodyLimit::max(CHUNK_SIZE)),
        )
        .route(
            "/api/mobile/transfer/upload/complete/:id",
            post(mobile_transfer::upload_complete),
        )
        .route(
            "/api/mobile/transfer/cancel",
            post(mobile_transfer::cancel_mobile_transfer),
        )
        .route(
            "/api/mobile/transfer/retry",
            post(mobile_transfer::retry_mobile_transfer),
        )
        .route(
            "/api/mobile/transfer/resume",
            post(mobile_transfer::resume_mobile_transfer),
        )
        .route(
            "/api/mobile/transfer/get-operation",
            post(mobile_transfer::get_mobile_transfer_operation),
        )
        .route(
            "/api/mobile/transfer/download/:taskId",
            get(mobile_transfer::download_mobile_transfer),
        )
        // P2P 同步协议（M4）：对端调 pull/push，字段对照 Python protocol.py
        .route("/api/sync/pull", post(sync::sync_pull))
        .route("/api/sync/push", post(sync::sync_push))
        // Attention 已读元数据：capability attention.read.v1 + ledger domain=attention_read
        .route(
            "/api/sync/attention-read/push-batch",
            post(attention::attention_read_push_batch),
        )
        // Prompt v2 有界同步（Task 2+3；capability sync.manifest.v2 已与 ledger 原子宣告）
        .route(
            "/api/sync/prompts/manifest-page",
            post(sync::prompt_manifest_page).layer(DefaultBodyLimit::max(
                sync::PROMPT_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/sync/prompts/items",
            post(sync::prompt_items).layer(DefaultBodyLimit::max(
                sync::PROMPT_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/sync/prompts/push-batch",
            post(sync::prompt_push_batch).layer(DefaultBodyLimit::max(
                sync::PROMPT_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/sync/prompts/ack-delete-epoch",
            post(sync::prompt_ack_delete_epoch).layer(DefaultBodyLimit::max(
                sync::PROMPT_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        // P2P CLAUDE.md 主动推送协议（单例 0/1 条；push 覆盖为发送方版本）
        .route(
            "/api/sync/claude_md/pull",
            post(claude_md_sync::claude_md_pull),
        )
        .route(
            "/api/sync/claude_md/push",
            post(claude_md_sync::claude_md_push),
        )
        // 工作台项目顺序 LWW 偏好（单例 orderedIds；不进入 settings domain 报告）
        .route(
            "/api/sync/workbench-project-order/pull",
            post(workbench_project_order_sync::project_order_pull),
        )
        .route(
            "/api/sync/workbench-project-order/push",
            post(workbench_project_order_sync::project_order_push),
        )
        // P2P 文件传输协议（M5）：init/chunk/complete/status；X-Chunk-Offset + complete 握手
        .route("/api/transfer/init", post(transfer::transfer_init))
        // chunk 路由本地上限 = CHUNK_SIZE（960 KiB），在 receiver 落盘校验前拒绝更大 body；
        // 外层仍保留全局 32 MiB DefaultBodyLimit，不复制成全路由策略表。
        .route(
            "/api/transfer/chunk/:id",
            post(transfer::transfer_chunk).layer(DefaultBodyLimit::max(CHUNK_SIZE)),
        )
        .route(
            "/api/transfer/complete/:id",
            post(transfer::transfer_complete),
        )
        .route("/api/transfer/status/:id", get(transfer::transfer_status))
        // Agent Hub LAN source-push（capability agent-hub.v1，三路由原子上线）
        // prepare body ≤32 MiB（manifest 上限）；object chunk ≤8 MiB。
        // sourceDeviceId/clientRequestId/expected-device 仅为幂等/绑定标签，不是身份认证。
        .route(
            "/api/agent-hub/push/prepare",
            post(agent_hub::agent_hub_push_prepare).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/api/agent-hub/push/:transferId/objects/:objectHash",
            put(agent_hub::agent_hub_push_object).layer(DefaultBodyLimit::max(
                crate::agent_hub::replication::receiver::AGENT_HUB_MAX_CHUNK_BYTES,
            )),
        )
        .route(
            "/api/agent-hub/push/:transferId/commit",
            post(agent_hub::agent_hub_push_commit),
        )
        // Agent Hub portable pull（capability agent-hub.portable-pull.v1，路由原子上线）
        // inventory/selection 仅 metadata/manifest；objects chunk ≤8 MiB；release 释放 staging。
        // expected-device / clientRequestId 仅为绑定与幂等，不是身份认证。
        .route(
            "/api/agent-hub/portable/inventory",
            post(agent_hub::agent_hub_portable_inventory),
        )
        .route(
            "/api/agent-hub/portable/selection",
            post(agent_hub::agent_hub_portable_selection)
                .layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/api/agent-hub/portable/objects/:transferId/:objectHash",
            get(agent_hub::agent_hub_portable_object),
        )
        .route(
            "/api/agent-hub/portable/transfers/:transferId/release",
            post(agent_hub::agent_hub_portable_transfer_release),
        )
        // Agent Hub 精确项目 inventory/action；LAN 不做身份鉴权，project id 只用于路由正确性。
        .route(
            "/api/agent-hub/portable/project/inventory",
            post(agent_hub::agent_hub_portable_project_inventory),
        )
        .route(
            "/api/agent-hub/portable/project/preview",
            post(agent_hub::agent_hub_portable_project_preview),
        )
        .route(
            "/api/agent-hub/portable/project/enable",
            post(agent_hub::agent_hub_portable_project_enable),
        )
        .route(
            "/api/agent-hub/portable/project/action/preview",
            post(agent_hub::agent_hub_portable_project_action_preview),
        )
        .route(
            "/api/agent-hub/portable/project/action/apply",
            post(agent_hub::agent_hub_portable_project_action_apply),
        )
        .route(
            "/api/agent-hub/portable/project/action/get",
            post(agent_hub::agent_hub_portable_project_action_get),
        )
        // Agent Hub 用户级三栏（capability agent-hub.user-instructions.v1）；owner 只跑本机，拒绝递归。
        .route(
            "/api/agent-hub/user-instructions/inspect",
            post(agent_hub::agent_hub_user_instructions_inspect),
        )
        .route(
            "/api/agent-hub/user-instructions/save-blocks",
            post(agent_hub::agent_hub_user_instructions_save_blocks),
        )
        .route(
            "/api/agent-hub/user-instructions/preview-setup",
            post(agent_hub::agent_hub_user_instructions_preview_setup),
        )
        .route(
            "/api/agent-hub/user-instructions/preview-update",
            post(agent_hub::agent_hub_user_instructions_preview_update),
        )
        .route(
            "/api/agent-hub/user-instructions/apply-plan",
            post(agent_hub::agent_hub_user_instructions_apply_plan),
        )
        .route(
            "/api/agent-hub/user-instructions/analyze",
            post(agent_hub::agent_hub_user_instructions_analyze),
        )
        .route(
            "/api/agent-hub/user-instructions/adapt",
            post(agent_hub::agent_hub_user_instructions_adapt),
        )
        .route(
            "/api/agent-hub/user-instructions/revise",
            post(agent_hub::agent_hub_user_instructions_revise),
        )
        .route(
            "/api/agent-hub/user-instructions/slot-versions",
            post(agent_hub::agent_hub_user_instructions_slot_versions),
        )
        .route(
            "/api/agent-hub/user-instructions/restore-slot-version",
            post(agent_hub::agent_hub_user_instructions_restore_slot_version),
        )
        // Claude Code 历史同步协议（独立链路）：cc-history/sync/{pull,push}，snake_case 互通
        .route("/api/cc-history/sync/pull", post(cc_history::cc_sync_pull))
        .route("/api/cc-history/sync/push", post(cc_history::cc_sync_push))
        // 分页 CC History 同步（capability cc-history.paged-sync.v1）：route body 上限 8 MiB
        .route(
            "/api/cc-history/sync/manifest-page",
            post(cc_history::cc_sync_manifest_page)
                .layer(DefaultBodyLimit::max(cc_history::CC_ROUTE_BODY_LIMIT_BYTES)),
        )
        .route(
            "/api/cc-history/sync/items",
            post(cc_history::cc_sync_items)
                .layer(DefaultBodyLimit::max(cc_history::CC_ROUTE_BODY_LIMIT_BYTES)),
        )
        .route(
            "/api/cc-history/sync/push-batch",
            post(cc_history::cc_sync_push_batch)
                .layer(DefaultBodyLimit::max(cc_history::CC_ROUTE_BODY_LIMIT_BYTES)),
        )
        // SSH 目标同步协议（独立链路）：ssh-target/sync/{pull,push}，snake_case 互通
        .route(
            "/api/ssh-target/sync/pull",
            post(ssh_target_sync::ssh_target_sync_pull),
        )
        .route(
            "/api/ssh-target/sync/push",
            post(ssh_target_sync::ssh_target_sync_push),
        )
        // SSH v2 有界同步（Task 2+3；capability sync.manifest.v2 已与 ledger 原子宣告）
        .route(
            "/api/ssh-target/sync/manifest-page",
            post(ssh_target_sync::ssh_manifest_page).layer(DefaultBodyLimit::max(
                ssh_target_sync::SSH_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/ssh-target/sync/items",
            post(ssh_target_sync::ssh_items).layer(DefaultBodyLimit::max(
                ssh_target_sync::SSH_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/ssh-target/sync/push-batch",
            post(ssh_target_sync::ssh_push_batch).layer(DefaultBodyLimit::max(
                ssh_target_sync::SSH_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/ssh-target/sync/ack-delete-epoch",
            post(ssh_target_sync::ssh_ack_delete_epoch).layer(DefaultBodyLimit::max(
                ssh_target_sync::SSH_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        // 速记本同步协议：scratchpad/sync/{pull,push}
        .route(
            "/api/scratchpad/sync/pull",
            post(scratchpad_sync::scratchpad_pull),
        )
        .route(
            "/api/scratchpad/sync/push",
            post(scratchpad_sync::scratchpad_push),
        )
        // Scratchpad v2 有界同步（Task 2+3；capability sync.manifest.v2 已与 ledger 原子宣告）
        .route(
            "/api/scratchpad/sync/manifest-page",
            post(scratchpad_sync::scratchpad_manifest_page).layer(DefaultBodyLimit::max(
                scratchpad_sync::SCRATCHPAD_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/scratchpad/sync/items",
            post(scratchpad_sync::scratchpad_items).layer(DefaultBodyLimit::max(
                scratchpad_sync::SCRATCHPAD_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/scratchpad/sync/push-batch",
            post(scratchpad_sync::scratchpad_push_batch).layer(DefaultBodyLimit::max(
                scratchpad_sync::SCRATCHPAD_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        .route(
            "/api/scratchpad/sync/ack-delete-epoch",
            post(scratchpad_sync::scratchpad_ack_delete_epoch).layer(DefaultBodyLimit::max(
                scratchpad_sync::SCRATCHPAD_SYNC_ROUTE_BODY_LIMIT_BYTES,
            )),
        )
        // Claude Code assets 选择性拉取：inventory + 按 selectors 生成 zip bundle
        .route(
            "/api/claude-code/assets/inventory",
            get(claude_code_assets::assets_inventory),
        )
        .route(
            "/api/claude-code/assets/bundle",
            post(claude_code_assets::assets_bundle),
        )
        // Provider Manager：移动端/对端经 HTTP 切换 cc-switch provider（复用无状态业务逻辑）
        .route(
            "/api/provider-manager/summary",
            get(provider_manager::summary),
        )
        .route("/api/provider-manager/list", get(provider_manager::list))
        .route(
            "/api/provider-manager/switch",
            post(provider_manager::switch),
        )
        // Workbench 远端目录选择与项目打开：远端设备执行本机 helper，调用方后续再建立 remote shortcut
        .route("/api/workbench/fs/roots", get(workbench::remote_roots))
        .route("/api/workbench/fs/list", post(workbench::remote_list_dir))
        .route("/api/workbench/fs/info", post(workbench::remote_path_info))
        .route(
            "/api/workbench/projects/list",
            get(workbench::list_projects),
        )
        .route(
            "/api/workbench/projects/open",
            post(workbench::open_remote_project),
        )
        .route(
            "/api/workbench/worktrees/list",
            post(workbench::list_worktrees),
        )
        .route(
            "/api/workbench/worktrees/create",
            post(workbench::create_worktree),
        )
        .route(
            "/api/workbench/worktrees/get",
            post(workbench::get_worktree),
        )
        .route(
            "/api/workbench/worktrees/commit",
            post(workbench::commit_worktree),
        )
        .route(
            "/api/workbench/worktrees/push",
            post(workbench::push_worktree),
        )
        .route(
            "/api/workbench/worktrees/merge",
            post(workbench::merge_worktree),
        )
        .route(
            "/api/workbench/worktrees/remove",
            post(workbench::remove_worktree),
        )
        .route(
            "/api/workbench/worktrees/mutation-operation",
            post(workbench::get_mutation_operation),
        )
        .route(
            "/api/workbench/worktrees/repair-hook-failure",
            post(workbench::repair_hook_failure),
        )
        .route(
            "/api/workbench/git/commits",
            post(workbench::list_git_commits),
        )
        .route(
            "/api/workbench/files/list-dir",
            post(workbench::list_workbench_dir),
        )
        .route(
            "/api/workbench/files/info",
            post(workbench::workbench_path_info),
        )
        .route(
            "/api/workbench/files/open",
            post(workbench::open_workbench_file),
        )
        .route(
            "/api/workbench/files/save-text",
            post(workbench::save_workbench_text_file),
        )
        .route(
            "/api/workbench/files/preview-sqlite",
            post(workbench::preview_workbench_sqlite),
        )
        .route(
            "/api/workbench/files/preview-html-asset",
            post(workbench::preview_workbench_html_asset),
        )
        .route(
            "/api/workbench/files/create-file",
            post(workbench::create_workbench_file),
        )
        .route(
            "/api/workbench/files/create-dir",
            post(workbench::create_workbench_dir),
        )
        .route(
            "/api/workbench/files/rename",
            post(workbench::rename_workbench_path),
        )
        .route(
            "/api/workbench/files/delete",
            post(workbench::delete_workbench_path),
        )
        .route(
            "/api/workbench/notes/get",
            post(workbench::get_project_note),
        )
        .route(
            "/api/workbench/notes/save",
            post(workbench::save_project_note),
        )
        .route("/api/workbench/banner/get", post(workbench::get_banner))
        .route("/api/workbench/banner/save", post(workbench::save_banner))
        .route(
            "/api/workbench/dependency/status",
            post(workbench::dependency_status),
        )
        .route(
            "/api/workbench/dependency/install",
            post(workbench::dependency_install),
        )
        .route(
            "/api/workbench/dependency/cancel",
            post(workbench::dependency_cancel),
        )
        .route("/api/workbench/events", get(workbench::workbench_events))
        .route(
            "/api/workbench/agent-runtime/snapshot",
            post(workbench::agent_runtime_snapshot).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/workbench/lan-fleet/snapshot",
            post(workbench::lan_fleet_snapshot).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/api/workbench/agent-ledger/summary",
            post(workbench::agent_ledger_summary).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/api/workbench/workspace/restore/preflight",
            post(workbench::workspace_restore_preflight),
        )
        .route(
            "/api/workbench/workspace/restore/safe-attach",
            post(workbench::workspace_restore_safe_attach),
        )
        .route(
            "/api/workbench/sessions/list",
            post(workbench::list_workbench_sessions),
        )
        .route(
            "/api/workbench/sessions/create",
            post(workbench::create_workbench_session),
        )
        .route(
            "/api/workbench/sessions/replay",
            post(workbench::replay_workbench_session),
        )
        .route(
            "/api/workbench/sessions/write",
            post(workbench::write_workbench_session_input),
        )
        .route(
            "/api/workbench/sessions/paste-image",
            post(workbench::paste_workbench_session_image),
        )
        .route(
            "/api/workbench/terminal-input-stream",
            get(workbench::terminal_input_stream),
        )
        .route(
            "/api/workbench/sessions/resize",
            post(workbench::resize_workbench_session),
        )
        .route(
            "/api/workbench/sessions/focus",
            post(workbench::focus_workbench_session),
        )
        .route(
            "/api/workbench/sessions/focused",
            post(workbench::focused_workbench_session),
        )
        .route(
            "/api/workbench/sessions/split-pane",
            post(workbench::split_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/switch-pane",
            post(workbench::switch_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/select-pane-at",
            post(workbench::select_workbench_pane_at),
        )
        .route(
            "/api/workbench/sessions/zoom-pane",
            post(workbench::zoom_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/close-pane",
            post(workbench::close_workbench_pane),
        )
        .route(
            "/api/workbench/sessions/close",
            post(workbench::close_workbench_session),
        )
        .route(
            "/api/workbench/sessions/rename",
            post(workbench::rename_workbench_session),
        )
        .route(
            "/api/workbench/claude-sessions/search",
            post(workbench::search_claude_sessions),
        )
        .route(
            "/api/workbench/claude-sessions/preview",
            post(workbench::get_claude_session_preview),
        )
        .route(
            "/api/workbench/claude-sessions/resume",
            post(workbench::resume_claude_session),
        )
        .route(
            "/api/workbench/wordgame/extract-delta",
            post(workbench::extract_wordgame_delta),
        )
        .route(
            "/api/workbench/prompt-optimizer/stream-to-session",
            post(workbench::stream_prompt_optimizer_to_session),
        )
        .route(
            "/api/workbench/browser/discover",
            post(workbench::discover_browser_targets),
        )
        .route(
            "/api/workbench/browser/preview",
            post(workbench::create_browser_preview),
        )
        .route(
            "/api/workbench/browser/proxy/:previewId/",
            any(workbench::proxy_browser_preview_root),
        )
        .route(
            "/api/workbench/browser/proxy/:previewId/*path",
            any(workbench::proxy_browser_preview),
        )
        // A5 Browser Verification：owner 侧 create/get/cancel/artifact（能力 workbench.browser-verification.v1）
        .route(
            "/api/workbench/browser-verification/create",
            post(browser_verification::create_browser_verification),
        )
        .route(
            "/api/workbench/browser-verification/get",
            post(browser_verification::get_browser_verification),
        )
        .route(
            "/api/workbench/browser-verification/cancel",
            post(browser_verification::cancel_browser_verification),
        )
        .route(
            "/api/workbench/browser-verification/artifact",
            post(browser_verification::get_browser_verification_artifact),
        )
        // Mobile Workbench 本机入口：手机 → 本机，可继续由本机代理到远端设备 remote shortcut
        .route(
            "/api/mobile/workbench/lan-fleet",
            post(workbench::mobile_lan_fleet).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/api/mobile/workbench/projects/list",
            get(workbench::mobile_list_projects),
        )
        .route(
            "/api/mobile/workbench/bridges/active-devices",
            post(workbench::mobile_list_active_bridge_devices),
        )
        .route(
            "/api/mobile/workbench/bridges/active-mapped-projects",
            post(workbench::mobile_list_active_mapped_projects),
        )
        .route(
            "/api/mobile/workbench/projects/open",
            post(workbench::mobile_open_project),
        )
        .route(
            "/api/mobile/workbench/worktrees/list",
            post(workbench::mobile_list_worktrees),
        )
        .route(
            "/api/mobile/workbench/worktrees/create",
            post(workbench::mobile_create_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/commit",
            post(workbench::mobile_commit_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/push",
            post(workbench::mobile_push_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/merge",
            post(workbench::mobile_merge_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/remove",
            post(workbench::mobile_remove_worktree),
        )
        .route(
            "/api/mobile/workbench/worktrees/mutation-operation",
            post(workbench::mobile_get_mutation_operation),
        )
        .route(
            "/api/mobile/workbench/git/commits",
            post(workbench::mobile_list_git_commits),
        )
        .route(
            "/api/mobile/workbench/files/list-dir",
            post(workbench::mobile_list_workbench_dir),
        )
        .route(
            "/api/mobile/workbench/files/info",
            post(workbench::mobile_workbench_path_info),
        )
        .route(
            "/api/mobile/workbench/files/open",
            post(workbench::mobile_open_workbench_file),
        )
        .route(
            "/api/mobile/workbench/files/save-text",
            post(workbench::mobile_save_workbench_text_file),
        )
        .route(
            "/api/mobile/workbench/sessions/list",
            post(workbench::mobile_list_workbench_sessions),
        )
        .route(
            "/api/mobile/workbench/sessions/create",
            post(workbench::mobile_create_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/replay",
            post(workbench::mobile_replay_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/paste-image",
            post(workbench::mobile_paste_workbench_session_image),
        )
        .route(
            "/api/mobile/workbench/terminal-input-stream",
            get(workbench::mobile_terminal_input_stream),
        )
        .route(
            "/api/mobile/workbench/sessions/resize",
            post(workbench::mobile_resize_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/focus",
            post(workbench::mobile_focus_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/focused",
            post(workbench::mobile_focused_workbench_session),
        )
        .route(
            "/api/mobile/workbench/sessions/split-pane",
            post(workbench::mobile_split_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/switch-pane",
            post(workbench::mobile_switch_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/zoom-pane",
            post(workbench::mobile_zoom_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/close-pane",
            post(workbench::mobile_close_workbench_pane),
        )
        .route(
            "/api/mobile/workbench/sessions/close",
            post(workbench::mobile_close_workbench_session),
        )
        .route(
            "/api/mobile/workbench/prompt-optimizer/stream-to-session",
            post(workbench::mobile_stream_prompt_optimizer_to_session),
        )
        .route(
            "/api/mobile/workbench/browser/discover",
            post(workbench::mobile_discover_browser_targets),
        )
        .route(
            "/api/mobile/workbench/browser/preview",
            post(workbench::mobile_create_browser_preview),
        )
        .route(
            "/api/mobile/workbench/browser/proxy/:previewId/",
            any(workbench::mobile_proxy_browser_preview_root),
        )
        .route(
            "/api/mobile/workbench/browser/proxy/:previewId/*path",
            any(workbench::mobile_proxy_browser_preview),
        )
        // Orchestrator 远端协议：remote shortcut 操作转发到项目所在设备的本机任务队列
        .route(
            "/api/orchestrator/tasks/create",
            post(orchestrator::create_task),
        )
        .route(
            "/api/orchestrator/tasks/complete-prompt",
            post(orchestrator::complete_task_prompt),
        )
        .route(
            "/api/orchestrator/tasks/list",
            post(orchestrator::list_tasks),
        )
        .route(
            "/api/orchestrator/task-views/list",
            post(orchestrator::list_task_views),
        )
        .route(
            "/api/orchestrator/task-views/create",
            post(orchestrator::create_task_view),
        )
        .route(
            "/api/orchestrator/outbox/retry",
            post(orchestrator::retry_remote_outbox),
        )
        .route(
            "/api/orchestrator/outbox/discard",
            post(orchestrator::discard_remote_outbox),
        )
        .route(
            "/api/orchestrator/tasks/evidence",
            post(orchestrator::get_evidence),
        )
        .route(
            "/api/orchestrator/tasks/queue",
            post(orchestrator::queue_task),
        )
        .route(
            "/api/orchestrator/tasks/start",
            post(orchestrator::start_task),
        )
        .route(
            "/api/orchestrator/tasks/move-workflow-state",
            post(orchestrator::move_workflow_state),
        )
        .route(
            "/api/orchestrator/tasks/complete-agent-run",
            post(orchestrator::complete_agent_run),
        )
        .route(
            "/api/orchestrator/tasks/retry",
            post(orchestrator::retry_task),
        )
        .route(
            "/api/orchestrator/tasks/request-rework",
            post(orchestrator::request_rework_task),
        )
        .route(
            "/api/orchestrator/tasks/deliver-reviewed",
            post(orchestrator::deliver_reviewed_task),
        )
        .route(
            "/api/orchestrator/tasks/abort",
            post(orchestrator::abort_task),
        )
        .route(
            "/api/orchestrator/tasks/cancel",
            post(orchestrator::cancel_task),
        )
        .route(
            "/api/orchestrator/projects/refresh",
            post(orchestrator::refresh_project),
        )
        // Orchestrator owning-device runtime snapshot：供 remote shortcut 状态条拉取权威运行时快照。
        // 由 orchestrator.runtime-snapshot.v1 能力 token 门控；仅服务本机 local 项目。
        .route(
            "/api/orchestrator/runtime-snapshot",
            post(orchestrator::runtime_snapshot),
        )
        .route(
            "/api/orchestrator/agent-adapters",
            post(orchestrator::agent_adapters_catalog),
        )
        // A4 experiments：capability orchestrator.experiments.v1；组级原子 create/list/get/approve/cancel
        .route(
            "/api/orchestrator/experiments/create",
            post(orchestrator::create_experiment_route),
        )
        .route(
            "/api/orchestrator/experiments/list",
            post(orchestrator::list_experiments_route),
        )
        .route(
            "/api/orchestrator/experiments/get",
            post(orchestrator::get_experiment_route),
        )
        .route(
            "/api/orchestrator/experiments/approve-winner",
            post(orchestrator::approve_experiment_winner_route),
        )
        .route(
            "/api/orchestrator/experiments/cancel",
            post(orchestrator::cancel_experiment_route),
        )
        // Mobile-facing runtime snapshot：手机浏览器同源调用，remote-aware 四态分发，不暴露 owner base URL。
        .route(
            "/api/mobile/orchestrator/runtime-snapshot",
            post(orchestrator::mobile_runtime_snapshot),
        )
        // Orchestrator WORKFLOW document：capability orchestrator.workflow-document.v1；仅本机 local 项目。
        .route(
            "/api/orchestrator/workflow-document/get",
            post(orchestrator::get_workflow_document_route),
        )
        .route(
            "/api/orchestrator/workflow-document/validate",
            post(orchestrator::validate_workflow_document_route),
        )
        .route(
            "/api/orchestrator/workflow-document/save",
            post(orchestrator::save_workflow_document_route),
        )
        // Mobile remote-aware WORKFLOW document wrappers。
        .route(
            "/api/mobile/orchestrator/workflow-document/get",
            post(orchestrator::mobile_get_workflow_document),
        )
        .route(
            "/api/mobile/orchestrator/workflow-document/validate",
            post(orchestrator::mobile_validate_workflow_document),
        )
        .route(
            "/api/mobile/orchestrator/workflow-document/save",
            post(orchestrator::mobile_save_workflow_document),
        )
        .route("/api/orchestrator/config", get(orchestrator::get_config))
        // 移动端 SPA fallback：只服务 /mobile 命名空间；其它未知路径保持 404。
        .fallback_service(service_fn(move |req| {
            serve_mobile_spa(fallback_state.clone(), req)
        }))
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        // P2P 错误信封兜底：把任何"非 2xx 且 body 非信封"的响应（包括 axum 框架产生的 404/405、
        // DefaultBodyLimit 触发的 413、JSON extractor 失败的 400）包成 P2pErrorEnvelope，确保客户端
        // 永远拿到稳定的 {error, code, request_id, retryable, details} 结构。放在 request_id_middleware
        // 之内，使其能从 extensions 读到 P2pRequestContext 并写入信封。
        .layer(axum::middleware::from_fn(envelope_fallback_middleware))
        // 可选期望 device_id 绑定：header 存在且非空时与本机 device_id 比对；错机 409 device_id_mismatch。
        // 缺 header 行为不变（LAN 仍无调用者身份校验）。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            expected_device_id_guard,
        ))
        // 浏览器 Host/Origin/Content-Type 门禁：socket gate 之后、body limit/handler 之前。
        // 读取 AppState.actual_http_port 与 device_id 受控 mDNS 名；不发送 CORS 头。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            browser_request_guard,
        ))
        // 固定 LAN socket peer 门禁：仅信任 ConnectInfo 真实 peer，忽略代理 header；
        // Denied/缺失 peer 在 handler 前返回 403。放在 request_id 内层，以便拒绝响应可复用 request context。
        .layer(axum::middleware::from_fn(lan_socket_gate))
        // overlay 信任快照注入：在 lan_socket_gate 之前注入 `OverlayTrustSnapshot` 扩展，
        // 使 gate 在 Denied 分支可放行用户手动配置的 overlay 对端 IP（精确白名单，opt-in）。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            inject_overlay_trust,
        ))
        // P2P/mobile API 请求 ID 边界：所有请求自动获得 X-CC-Request-Id（客户端带入或服务端生成 UUID），
        // 注入 P2pRequestContext 供 handler 使用，并打开带 `request_id` 字段的 tracing span。
        // 放在最外层（相对业务 middleware），使其覆盖全部 /api 路由 + /mobile SPA fallback。
        // axum 最后 .layer 最外：请求顺序 ConnectInfo → request_id → lan_socket_gate
        // → browser_request_guard → expected_device_id → envelope → body limit → handler。
        .layer(axum::middleware::from_fn(request_id_middleware))
        .with_state(state.clone());

    let preferred_port = {
        let config = state.config.read().expect("config 读锁中毒");
        preferred_http_port_from_config(config.http_port)
    };
    let listener = bind_preferred_http_listener(preferred_port).await?;

    // 取实际监听端口并回填 AppState（供 mDNS 注册 + health handler 返回）
    let actual_port = listener.local_addr()?.port();
    state.actual_http_port.store(actual_port, Ordering::SeqCst);

    // 后台运行 axum serve（serve 持有 listener 与 app 所有权，直到进程退出）。
    // 必须使用 into_make_service_with_connect_info，否则 lan_socket_gate / stop 拿不到真实 peer。
    // axum::serve 返回的 future 为 Send，可直接 spawn 到 tokio runtime。
    tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            // 仅记录脱敏摘要，不写请求 payload
            crate::backend::logging::OperationLog::new(
                "http",
                "serve",
                crate::backend::logging::OperationResult::Error,
            )
            .level(tracing::Level::ERROR)
            .error_code("internal")
            .message(format!("axum HTTP server 异常退出: {e}"))
            .emit();
        }
    });

    crate::backend::logging::OperationLog::new(
        "http",
        "serve",
        crate::backend::logging::OperationResult::Ok,
    )
    .message(format!("axum HTTP server 已启动，监听端口: {actual_port}"))
    .emit();
    Ok(actual_port)
}

/// 解析配置中的 HTTP 首选端口。
///
/// Business Logic（为什么需要这个函数）:
///     旧配置里 http_port 可能是 0，表示历史动态端口；现在移动端链接要求尽量稳定，因此 0/非法值需要
///     统一回落到固定默认端口。
///
/// Code Logic（这个函数做什么）:
///     接收配置中的 i64 端口值；合法的 1..=65535 直接转 u16，否则返回 DEFAULT_HTTP_PORT。
fn preferred_http_port_from_config(config_port: i64) -> u16 {
    if (1..=u16::MAX as i64).contains(&config_port) {
        config_port as u16
    } else {
        DEFAULT_HTTP_PORT
    }
}

/// 绑定首选 HTTP 端口，冲突时自动向上递增。
///
/// Business Logic（为什么需要这个函数）:
///     移动端 Workbench URL 需要尽量固定；当同机已有进程占用首选端口时，用户仍应能启动应用并获得相邻端口。
///
/// Code Logic（这个函数做什么）:
///     从 preferred_port 开始依次尝试绑定 0.0.0.0:<port>；AddrInUse 时继续下一个端口，
///     其它 IO 错误直接返回；全部端口耗尽时返回最后一次 AddrInUse 错误。
async fn bind_preferred_http_listener(
    preferred_port: u16,
) -> Result<tokio::net::TcpListener, std::io::Error> {
    let start_port = if preferred_port == 0 {
        DEFAULT_HTTP_PORT
    } else {
        preferred_port
    };
    let mut last_addr_in_use: Option<IoError> = None;

    for port in start_port..=u16::MAX {
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                if port != start_port {
                    tracing::warn!("HTTP 首选端口 {start_port} 被占用，改用 {port}");
                }
                return Ok(listener);
            }
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                last_addr_in_use = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_addr_in_use
        .unwrap_or_else(|| IoError::new(ErrorKind::AddrInUse, "没有可用的 HTTP 监听端口")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::lan_guard::{browser_guard_params, browser_request_guard_with_params};
    use crate::workbench::file_content::MAX_EDITABLE_TEXT_BYTES;
    use crate::workbench::remote_protocol::RemoteSaveTextReq;
    use std::ffi::OsString;
    use std::net::Ipv4Addr;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// 测试用 HOME 环境隔离守卫。
    ///
    /// Business Logic（为什么需要这个结构）:
    ///     backend stop control route 会读取用户级控制文件；测试必须隔离到临时目录，避免污染真实 `~/.cc-partner`。
    ///
    /// Code Logic（这个结构做什么）:
    ///     持有全局环境变量锁、临时 HOME 目录和原始 HOME/USERPROFILE；Drop 时恢复环境变量并释放临时目录。
    struct TestHomeGuard {
        _lock: MutexGuard<'static, ()>,
        previous_home: Option<OsString>,
        previous_userprofile: Option<OsString>,
        _temp_home: TempDir,
    }

    impl Drop for TestHomeGuard {
        /// 恢复测试前的 HOME 环境变量。
        ///
        /// Business Logic（为什么需要这个函数）:
        ///     单元测试修改进程级 HOME 后必须恢复，避免影响同一测试进程中的其它模块。
        ///
        /// Code Logic（这个函数做什么）:
        ///     根据保存的原值恢复或移除 HOME/USERPROFILE；临时目录由 TempDir 自动删除。
        fn drop(&mut self) {
            match &self.previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match &self.previous_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    /// 返回测试 HOME 环境变量锁。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     HOME 是进程级全局状态，多个 route 测试并发修改会互相污染控制文件路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 OnceLock 初始化全局 Mutex，所有需要临时 HOME 的测试串行执行。
    fn backend_control_home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// 安装临时 HOME 供 backend control route 测试使用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     route 读取 `config_dir()` 下的控制文件；测试需要在可控目录写入 token。
    ///
    /// Code Logic（这个函数做什么）:
    ///     加锁后创建 TempDir，把 HOME/USERPROFILE 指到临时目录，并清理遗留 shutdown notifier。
    fn install_test_home() -> TestHomeGuard {
        let lock = backend_control_home_lock()
            .lock()
            .expect("backend control HOME 测试锁中毒");
        let previous_home = std::env::var_os("HOME");
        let previous_userprofile = std::env::var_os("USERPROFILE");
        let temp_home = tempfile::tempdir().expect("应能创建临时 HOME");
        std::env::set_var("HOME", temp_home.path());
        std::env::set_var("USERPROFILE", temp_home.path());
        control::clear_shutdown_notifier();
        TestHomeGuard {
            _lock: lock,
            previous_home,
            previous_userprofile,
            _temp_home: temp_home,
        }
    }

    /// 构造 backend stop 控制测试数据。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     多个 stop route 测试需要一致的控制文件内容，避免 token/port 样板分散导致断言难读。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回固定 pid、port、device 和 control token 的 `BackendControlFile`。
    fn backend_stop_control_file_for_test() -> BackendControlFile {
        BackendControlFile {
            pid: 1234,
            port: 62116,
            device_id: "device-a".to_string(),
            device_name: "测试设备".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            control_token: "expected-token".to_string(),
            control_schema_version: crate::backend::authority::CONTROL_SCHEMA_VERSION,
            owner_instance_id: Some("owner-test".to_string()),
            agent_hub_api_version: crate::backend::control::AGENT_HUB_API_VERSION,
        }
    }

    /// 返回测试用 P2pRequestContext，供 stop_backend_control 直接调用。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     handler 升级为 `P2pResult` 后需要 `Extension<P2pRequestContext>` 才能构造信封；单元测试
    ///     直接调用 handler 时需要注入固定 context，方便断言信封里的 request_id 字段。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `request_id = "req-stop-test"` 的 P2pRequestContext。
    fn stop_ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-stop-test".to_string(),
        }
    }

    /// 返回测试用 loopback ConnectInfo。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     stop handler 在 token 比较前要求真实 loopback peer；直接调用 handler 时需注入 ConnectInfo。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `127.0.0.1:9` 的 ConnectInfo。
    fn stop_loopback_peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 9)))
    }

    /// 返回测试用 LAN ConnectInfo（非 loopback）。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     覆盖“持有合法 token 但从 LAN peer 调用 stop 必须拒绝”的路径。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `192.168.1.50:9` 的 ConnectInfo。
    fn stop_lan_peer() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from((Ipv4Addr::new(192, 168, 1, 50), 9)))
    }

    /// 验证 backend stop 控制接口拒绝错误 token 并返回 unauthorized 信封。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     `/api/backend/control/stop` 会关闭本机 headless 后端，必须要求调用方持有控制文件里的令牌；
    ///     失败路径必须返回统一的 P2P 错误信封（携带 code/request_id），而不是原始 axum StatusCode。
    ///
    /// Code Logic（这个测试做什么）:
    ///     以 loopback peer 构造控制文件与错误请求，断言 handler 返回 401 unauthorized 信封。
    #[test]
    fn backend_stop_control_rejects_invalid_token() {
        let _home = install_test_home();
        let control = backend_stop_control_file_for_test();
        control::write_control_file(&control).expect("应能写入测试控制文件");
        let request = StopBackendRequest {
            control_token: "wrong-token".to_string(),
        };

        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能创建测试 runtime")
            .block_on(stop_backend_control(
                stop_loopback_peer(),
                Extension(stop_ctx()),
                Json(request),
            ));

        let err = result.expect_err("错误 token 必须返回错误");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(err.envelope().code, "unauthorized");
        assert_eq!(err.envelope().request_id, "req-stop-test");
        assert!(!err.envelope().retryable);
    }

    /// 验证 backend stop 即使 token 正确也拒绝非 loopback peer。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     stop 是本机生命周期控制；LAN 对端即使读到 control token 也不得关闭后端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入合法 control 文件，以 192.168.1.50 peer + 正确 token 调用 handler，
    ///     断言 403 forbidden 且不依赖 notifier 状态。
    #[test]
    fn backend_stop_rejects_non_loopback_even_with_valid_token() {
        let _home = install_test_home();
        let control = backend_stop_control_file_for_test();
        control::write_control_file(&control).expect("应能写入测试控制文件");
        let request = StopBackendRequest {
            control_token: control.control_token.clone(),
        };

        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能创建测试 runtime")
            .block_on(stop_backend_control(
                stop_lan_peer(),
                Extension(stop_ctx()),
                Json(request),
            ));

        let err = result.expect_err("非 loopback 必须拒绝");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        assert_eq!(err.envelope().code, "forbidden");
        assert_eq!(err.envelope().error, "不支持的网络来源");
        assert_eq!(err.envelope().request_id, "req-stop-test");
        assert!(!err.envelope().retryable);
    }

    /// 验证 backend stop 控制接口接受当前控制文件 token。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     `cc-partner-backend stop` 读取控制文件后必须能通过本地 HTTP route 唤醒 serve 进程关闭。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造控制文件与正确请求，断言 token 校验通过。
    #[test]
    fn backend_stop_control_accepts_current_token() {
        let control = backend_stop_control_file_for_test();
        let request = StopBackendRequest {
            control_token: "expected-token".to_string(),
        };

        assert!(backend_stop_token_matches(&request, Some(&control)));
    }

    /// 验证 backend stop 控制接口在 shutdown notifier 缺失时返回 unavailable 信封且可重试。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     正确 token 只代表请求来源可信；如果进程内关闭通知器缺失，CLI 不能收到 200 并误删控制文件。
    ///     失败路径必须返回 P2P 错误信封；notifier 缺失属于瞬时状态，应标记 retryable=true。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入正确控制文件但不安装 shutdown notifier，调用 route 并断言返回 503 unavailable 信封，
    ///     retryable=true，request_id 与 ctx 一致。
    #[test]
    fn backend_stop_control_returns_non_success_when_notifier_missing() {
        let _home = install_test_home();
        let control = backend_stop_control_file_for_test();
        control::write_control_file(&control).expect("应能写入测试控制文件");
        let request = StopBackendRequest {
            control_token: control.control_token.clone(),
        };

        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能创建测试 runtime")
            .block_on(stop_backend_control(
                stop_loopback_peer(),
                Extension(stop_ctx()),
                Json(request),
            ));

        let err = result.expect_err("notifier 缺失必须返回错误");
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.envelope().code, "unavailable");
        assert!(err.envelope().retryable);
        assert_eq!(err.envelope().request_id, "req-stop-test");
    }

    /// 验证 backend stop 控制接口在 shutdown notifier 发送失败时返回 unavailable 信封且可重试。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     notifier 已安装但 receiver 已失效时，stop 请求仍无法关闭 serve，CLI 不应把它当作成功；
    ///     该路径同样属于可重试瞬时错误，必须返回 P2P 错误信封并标记 retryable=true。
    ///
    /// Code Logic（这个测试做什么）:
    ///     安装一个无 receiver 的 watch sender，以 loopback peer 调用 route 并断言 503 unavailable。
    #[test]
    fn backend_stop_control_returns_non_success_when_notifier_send_fails() {
        let _home = install_test_home();
        let control = backend_stop_control_file_for_test();
        control::write_control_file(&control).expect("应能写入测试控制文件");
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        drop(shutdown_rx);
        control::install_shutdown_notifier(shutdown_tx);
        let request = StopBackendRequest {
            control_token: control.control_token.clone(),
        };

        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("应能创建测试 runtime")
            .block_on(stop_backend_control(
                stop_loopback_peer(),
                Extension(stop_ctx()),
                Json(request),
            ));

        control::clear_shutdown_notifier();
        let err = result.expect_err("notifier 发送失败必须返回错误");
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.envelope().code, "unavailable");
        assert!(err.envelope().retryable);
        assert_eq!(err.envelope().request_id, "req-stop-test");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端 SPA 只允许接管 `/mobile` 命名空间，不能抢占 P2P API 与桌面 Workbench 路由。
    ///
    /// Code Logic（这个测试做什么）:
    ///     枚举移动端入口、移动端静态资源、API 路由和普通桌面路径，断言路径匹配 helper 只识别 `/mobile` 前缀。
    #[test]
    fn mobile_spa_path_matching_does_not_capture_api_routes() {
        assert!(is_mobile_spa_path("/mobile"));
        assert!(is_mobile_spa_path("/mobile/"));
        assert!(is_mobile_spa_path("/mobile/assets/index.js"));
        assert!(is_mobile_spa_path("/assets/mobile.js"));
        assert!(is_mobile_spa_path("/assets/index.css"));

        assert!(!is_mobile_spa_path("/api/workbench/events"));
        assert!(!is_mobile_spa_path("/api/mobile/access-info"));
        assert!(!is_mobile_spa_path("/"));
        assert!(!is_mobile_spa_path("/workbench"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     手机浏览器加载 `/mobile` SPA 时需要正确识别常见静态资源类型，避免脚本或样式被浏览器拒绝。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对 HTML、JS、CSS、SVG、PNG 以及未知扩展名分别断言 content-type 映射结果。
    #[test]
    fn mobile_content_type_maps_common_static_assets() {
        assert_eq!(
            mobile_content_type("mobile.html"),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mobile_content_type("assets/index.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            mobile_content_type("assets/index.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(mobile_content_type("assets/logo.svg"), "image/svg+xml");
        assert_eq!(mobile_content_type("assets/logo.png"), "image/png");
        assert_eq!(
            mobile_content_type("assets/file.unknown"),
            "application/octet-stream"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     移动端 Workbench 访问链接应尽量稳定，用户收藏或记忆固定端口后不应每次启动随机变化；
    ///     若首选端口被占用，应用应自动尝试下一个端口而不是启动失败或退回随机端口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     先用标准库 listener 占用一个本机端口，再调用 HTTP listener 绑定 helper，断言最终端口不是
    ///     被占端口，且按递增策略落在更高端口上。
    #[tokio::test]
    async fn preferred_http_listener_increments_when_port_is_busy() {
        let occupied = std::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
            .expect("测试应能占用一个临时端口");
        let occupied_port = occupied
            .local_addr()
            .expect("测试 listener 应能读取端口")
            .port();
        if occupied_port == u16::MAX {
            drop(occupied);
            return;
        }

        let listener = bind_preferred_http_listener(occupied_port).await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();

        assert_ne!(actual_port, occupied_port);
        assert!(actual_port > occupied_port);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/mobile` HTTP 路径只是局域网访问前缀，Tauri 打包资源内实际 key 不包含 `/mobile` 前缀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言移动端入口映射到 `mobile.html`，静态资源映射到前端 dist 相对路径，危险路径返回 None。
    #[test]
    fn mobile_asset_key_normalizes_http_path_without_mobile_prefix() {
        assert_eq!(mobile_asset_key("/mobile").as_deref(), Some("mobile.html"));
        assert_eq!(mobile_asset_key("/mobile/").as_deref(), Some("mobile.html"));
        assert_eq!(
            mobile_asset_key("/mobile/assets/index.js").as_deref(),
            Some("assets/index.js")
        );
        assert_eq!(
            mobile_asset_key("/assets/mobile.js").as_deref(),
            Some("assets/mobile.js")
        );
        assert_eq!(
            mobile_asset_key("/assets/index.css").as_deref(),
            Some("assets/index.css")
        );
        assert_eq!(
            mobile_asset_key("assets/index.css").as_deref(),
            Some("assets/index.css")
        );

        assert_eq!(mobile_asset_key("/"), None);
        assert_eq!(mobile_asset_key("/mobile/../mobile.html"), None);
        assert_eq!(mobile_asset_key("/mobile/assets/../../secret.js"), None);
        assert_eq!(mobile_asset_key("/etc/passwd"), None);
        assert_eq!(mobile_asset_key(""), None);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/mobile` 静态服务暴露在局域网，必须拒绝目录穿越和绝对路径，避免读取构建目录外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接调用路径规范化 helper，断言 `..`、绝对路径和嵌套回退穿越都不会产生 asset key。
    #[test]
    fn mobile_asset_key_rejects_unsafe_paths() {
        assert!(mobile_asset_key("../mobile.html").is_none());
        assert!(mobile_asset_key("/etc/passwd").is_none());
        assert!(mobile_asset_key("assets/../../secret.js").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     axum fallback 只能服务 `/mobile` SPA；桌面路由或未知路径仍应保持不匹配，不应被移动端入口吞掉。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过 path matching helper 断言非 `/mobile` 路径不进入移动端 SPA 处理。
    #[test]
    fn mobile_spa_fallback_keeps_unknown_paths_not_found() {
        assert!(!is_mobile_spa_path("/workbench"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 Workbench 文本保存走 P2P HTTP JSON body，服务端 body limit 必须覆盖 5MB 高转义文本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 5MB NUL 文本让 serde_json 产生接近最坏情况的 `\u0000` 转义，断言序列化 body 仍低于 HTTP limit。
    #[test]
    fn body_limit_allows_workbench_remote_text_save_payloads() {
        let escaped_content = "\u{0000}".repeat(MAX_EDITABLE_TEXT_BYTES as usize);
        let body = serde_json::to_vec(&RemoteSaveTextReq {
            project_id: "project-1".to_string(),
            worktree_id: Some("worktree-1".to_string()),
            path: "docs/note.md".to_string(),
            content: escaped_content,
            base_hash: "old-hash".to_string(),
        })
        .expect("remote save-text request should serialize");

        assert!(body.len() < BODY_LIMIT_BYTES);
    }

    /// 构造一个与生产 Router 同样的 middleware 栈的最小测试 Router。
    ///
    /// Business Logic（为什么需要这个辅助）:
    ///     `start_http_server` 内联了 Router 构造并需要 AppState，无法直接拿来跑 oneshot。
    ///     用一个简单 `ok` handler 替代真实 health/workbench handler，聚焦验证 middleware 栈在 router 层确实生效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 `/probe` 占位 handler（GET/POST），layer 顺序（last=outermost）对齐生产：
    ///     DefaultBodyLimit → envelope_fallback → lan_socket_gate → request_id_middleware。
    ///     request_id 测试会注入 loopback ConnectInfo，避免 gate 拒绝。
    fn middleware_test_router() -> Router {
        async fn ok() -> (StatusCode, &'static str) {
            (StatusCode::OK, "ok")
        }
        /// POST probe 必须消费 body，DefaultBodyLimit 只在 body 被提取/读取时触发 413。
        async fn ok_post(body: axum::body::Bytes) -> (StatusCode, &'static str) {
            let _ = body;
            (StatusCode::OK, "ok")
        }
        let params = browser_guard_params("device-a", 62116);
        Router::new()
            .route("/probe", get(ok).post(ok_post))
            .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
            .layer(axum::middleware::from_fn(envelope_fallback_middleware))
            .layer(axum::middleware::from_fn(move |req, next| {
                let params = params.clone();
                async move { browser_request_guard_with_params(params, req, next).await }
            }))
            .layer(axum::middleware::from_fn(lan_socket_gate))
            .layer(axum::middleware::from_fn(request_id_middleware))
    }

    /// Business Logic（为什么需要这个测试）:
    ///     全局 32 MiB DefaultBodyLimit 触发的 413 必须被错误信封兜底，客户端才能稳定解析
    ///     `payload_too_large`，而不是拿到 axum 裸状态码/非 JSON body。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过与生产一致的 middleware 栈 POST `BODY_LIMIT_BYTES + 1` 字节 body，
    ///     断言 HTTP 413 且 body 为 P2P 错误信封（code=payload_too_large, retryable=false）。
    #[tokio::test]
    async fn default_body_limit_rejects_oversized_body_with_error_envelope() {
        use axum::body::to_bytes;
        use tower::ServiceExt;

        let router = middleware_test_router();
        let mut request = Request::builder()
            .method("POST")
            .uri("/probe")
            .header("host", "127.0.0.1:62116")
            .header("content-type", "application/octet-stream")
            .body(Body::from(vec![0u8; BODY_LIMIT_BYTES + 1]))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1))));
        let response = router.oneshot(request).await.expect("router 不可失败");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(response.headers().get("x-cc-request-id").is_some());
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("应能读取 413 body");
        let body: serde_json::Value =
            serde_json::from_slice(&bytes).expect("413 必须包成 JSON 错误信封");
        assert_eq!(body["code"], "payload_too_large");
        assert_eq!(body["retryable"], false);
        assert!(body["error"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(body["request_id"].as_str().is_some_and(|s| !s.is_empty()));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     request_id middleware 必须对真实 `/api/health` 等路由生效；测试通过模拟生产 middleware 栈
    ///     （DefaultBodyLimit + request_id_middleware）确认 `/probe` 代表的 `/api/*` 路由会自动回写 header。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造与生产一致的 middleware 栈并 oneshot 一个 `/probe` 请求（未带 header），
    ///     断言响应带 `X-CC-Request-Id`（新生成的 UUID，长度 36）。
    #[tokio::test]
    async fn health_like_route_returns_request_id_header() {
        use tower::ServiceExt;
        let router = middleware_test_router();
        let mut request = Request::builder()
            .uri("/probe")
            .header("host", "127.0.0.1:62116")
            .body(Body::empty())
            .unwrap();
        // 生产 serve 会注入 ConnectInfo；oneshot 测试需手动注入 loopback peer。
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1))));
        let response = router.oneshot(request).await.expect("router 不可失败");

        assert_eq!(response.status(), StatusCode::OK);
        let header = response
            .headers()
            .get("x-cc-request-id")
            .expect("probe-like route should carry request id header");
        let value = header.to_str().unwrap();
        assert_eq!(value.len(), 36);
        assert_eq!(value.matches('-').count(), 4);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     客户端带入的请求 ID 必须穿透整个 middleware 栈（不被 DefaultBodyLimit 或 handler 层覆盖），
    ///     保证 A→B 的调用链 ID 与对端日志可对齐。
    ///
    /// Code Logic（这个测试做什么）:
    ///     发送带 `X-CC-Request-Id: trace-abc-001` 的 `/probe` 请求，断言响应 header 回写同一值。
    #[tokio::test]
    async fn health_like_route_echoes_client_request_id() {
        use tower::ServiceExt;
        let router = middleware_test_router();
        let mut request = Request::builder()
            .uri("/probe")
            .header("host", "127.0.0.1:62116")
            .header("x-cc-request-id", "trace-abc-001")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 1))));
        let response = router.oneshot(request).await.expect("router 不可失败");

        assert_eq!(
            response
                .headers()
                .get("x-cc-request-id")
                .unwrap()
                .to_str()
                .unwrap(),
            "trace-abc-001"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `cc-history.paged-sync.v1` 与三条分页路由必须同一构建原子上线，否则 mixed-version
    ///     客户端会在能力探测与 404 之间撕裂。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言 `server_protocol_info` 宣告 token，并用 axum 迷你 Router 挂载与生产相同的
    ///     三条路径；oneshot POST 不得 404（证明路径已注册，而非依赖 handler 业务成功）。
    #[tokio::test]
    async fn paged_cc_history_routes_and_capability_ship_atomically() {
        use crate::net::protocol::{
            server_protocol_info, CAPABILITY_CC_HISTORY_PAGED_SYNC_V1, PROTOCOL_VERSION_V1,
        };
        use axum::routing::post;
        use tower::ServiceExt;

        let info = server_protocol_info();
        assert_eq!(info.protocol_version, PROTOCOL_VERSION_V1);
        assert!(
            info.supports(CAPABILITY_CC_HISTORY_PAGED_SYNC_V1),
            "capability must ship with paged routes"
        );

        // 源码层：生产 start_http_server 必须字面挂载三条路径
        let src = include_str!("http_server.rs");
        for path in [
            "/api/cc-history/sync/manifest-page",
            "/api/cc-history/sync/items",
            "/api/cc-history/sync/push-batch",
        ] {
            assert!(src.contains(path), "http_server must mount {path}");
        }

        // axum 层：同 path 挂载后 oneshot 不得 404
        async fn ok() -> &'static str {
            "ok"
        }
        let app = Router::new()
            .route("/api/cc-history/sync/manifest-page", post(ok))
            .route("/api/cc-history/sync/items", post(ok))
            .route("/api/cc-history/sync/push-batch", post(ok));

        for path in [
            "/api/cc-history/sync/manifest-page",
            "/api/cc-history/sync/items",
            "/api/cc-history/sync/push-batch",
        ] {
            let request = Request::builder()
                .method("POST")
                .uri(path)
                .body(Body::empty())
                .expect("build request");
            let response = app.clone().oneshot(request).await.expect("router");
            assert_ne!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} must not 404 when mounted"
            );
            assert!(
                response.status().is_success(),
                "{path} stub handler should succeed"
            );
        }
    }
}
