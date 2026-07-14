//! workbench/browser_proxy.rs — Workbench 浏览器预览代理
//!
//! Business Logic（为什么需要这个模块）:
//!     Workbench Browser tab 需要把项目设备上的本机 dev server 包装为桌面端和手机端都能访问的同源预览地址。
//!
//! Code Logic（这个模块做什么）:
//!     管理短期 preview session，按 previewId 把 HTTP/WebSocket 请求转发到安全的上游目标；
//!     在 registry 成功解析 live session 之后，才允许 opaque iframe 的 `Origin: null`。

use crate::net::routes::ApiError;
use crate::state::AppState;
use crate::workbench::browser_models::WorkbenchBrowserPreview;
use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ws::{Message as AxumWsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::FromRequestParts;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use reqwest::{
    header::{
        HeaderMap as ReqwestHeaderMap, HeaderName as ReqwestHeaderName,
        HeaderValue as ReqwestHeaderValue,
    },
    Method as ReqwestMethod,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as TungsteniteRequest;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

const PREVIEW_TTL: Duration = Duration::from_secs(30 * 60);
/// preview proxy 单次请求体上限，需与 HTTP server DefaultBodyLimit 保持一致（32MB）。
const PROXY_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const AXUM_LENGTH_LIMIT_ERROR: &str = "length limit exceeded";
pub const DESKTOP_BROWSER_PROXY_ROUTE_PREFIX: &str = "/api/workbench/browser/proxy";
pub const MOBILE_BROWSER_PROXY_ROUTE_PREFIX: &str = "/api/mobile/workbench/browser/proxy";
type UpstreamWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// 浏览器预览会话注册表。
///
/// Business Logic（为什么需要这个结构体）:
///     预览 URL 必须使用不可预测 previewId，而不是直接暴露用户选择的 dev server URL。
///
/// Code Logic（这个结构体做什么）:
///     用线程安全 HashMap 保存 previewId 到上游目标的短期映射，支持 clone 后在命令和 HTTP route 间共享。
#[derive(Clone)]
pub struct WorkbenchBrowserPreviewRegistry {
    inner: Arc<RwLock<HashMap<String, BrowserPreviewSession>>>,
}

/// 浏览器预览上游目标。
///
/// Business Logic（为什么需要这个枚举）:
///     本机项目直接代理到本设备 loopback dev server；远端项目只能代理到 owning device 已创建的 preview。
///
/// Code Logic（这个枚举做什么）:
///     区分本地目标 URL 与远端 relay 所需的 owner base URL、远端 previewId 和展示 targetUrl。
#[derive(Debug, Clone)]
pub enum BrowserPreviewTarget {
    Local {
        target_url: String,
    },
    RemoteRelay {
        base_url: String,
        remote_preview_id: String,
        target_url: String,
    },
}

/// 浏览器预览会话。
///
/// Business Logic（为什么需要这个结构体）:
///     每个预览 iframe 需要绑定项目、worktree、上游目标与过期时间，避免旧链接永久可用。
///
/// Code Logic（这个结构体做什么）:
///     保存 previewId、业务归属、上游目标、Instant 过期点以及前端可展示的毫秒时间戳。
#[derive(Debug, Clone)]
pub struct BrowserPreviewSession {
    pub preview_id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub target: BrowserPreviewTarget,
    pub expires_at: Instant,
    pub expires_at_ms: i64,
}

impl WorkbenchBrowserPreviewRegistry {
    /// 创建空的浏览器预览注册表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     应用启动时需要初始化一份共享 registry，供所有 Workbench Browser preview 会话复用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造 Arc<RwLock<HashMap>>，初始不包含任何 preview session。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 创建本机浏览器预览会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机项目的 dev server 只能通过应用同源 proxy URL 暴露给桌面端和手机端。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 UUID previewId，保存 Local target，并返回桌面绝对代理 URL 与 mobile 同源 path。
    pub fn create_local(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        target_url: String,
        actual_http_port: u16,
    ) -> WorkbenchBrowserPreview {
        self.create_session(
            project_id,
            worktree_id,
            BrowserPreviewTarget::Local { target_url },
            actual_http_port,
        )
    }

    /// 创建远端 relay 浏览器预览会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     远端项目必须先在 owning device 创建 preview，本机只登记到 owner proxy 的 relay，不能直接访问 owner loopback。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 RemoteRelay target，并返回本机桌面/手机 proxy 地址，后续 HTTP proxy 再转发到 owner proxy path。
    pub fn create_remote_relay(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        base_url: String,
        remote_preview_id: String,
        target_url: String,
        actual_http_port: u16,
    ) -> WorkbenchBrowserPreview {
        self.create_session(
            project_id,
            worktree_id,
            BrowserPreviewTarget::RemoteRelay {
                base_url,
                remote_preview_id,
                target_url,
            },
            actual_http_port,
        )
    }

    /// 查找预览会话。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     每次 iframe 请求都必须确认 previewId 仍存在且未过期，同时活跃预览应续期避免使用中失效。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写锁内先清理过期 session，命中后续期 30 分钟并返回 clone；未命中返回 None。
    pub fn lookup(&self, preview_id: &str) -> Option<BrowserPreviewSession> {
        let now = Instant::now();
        let mut guard = self
            .inner
            .write()
            .expect("browser preview registry 写锁中毒");
        guard.retain(|_, session| session.expires_at > now);
        let session = guard.get_mut(preview_id)?;
        let (expires_at, expires_at_ms) = next_expiry();
        session.expires_at = expires_at;
        session.expires_at_ms = expires_at_ms;
        Some(session.clone())
    }

    /// 创建 preview session 并写入注册表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     本机 preview 和远端 relay 共享相同的 previewId、TTL 与返回 DTO 生成逻辑。
    ///
    /// Code Logic（这个函数做什么）:
    ///     生成 session，按 previewId 写入 HashMap，再根据 session 构造 WorkbenchBrowserPreview。
    fn create_session(
        &self,
        project_id: String,
        worktree_id: Option<String>,
        target: BrowserPreviewTarget,
        actual_http_port: u16,
    ) -> WorkbenchBrowserPreview {
        let preview_id = Uuid::new_v4().simple().to_string();
        let (expires_at, expires_at_ms) = next_expiry();
        let session = BrowserPreviewSession {
            preview_id: preview_id.clone(),
            project_id,
            worktree_id,
            target,
            expires_at,
            expires_at_ms,
        };
        let preview = session_to_preview(&session, actual_http_port);
        self.inner
            .write()
            .expect("browser preview registry 写锁中毒")
            .insert(preview_id, session);
        preview
    }

    /// 创建测试/smoke 用本机 preview。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     registry 单元测试与 bound smoke 只关心 previewId 与 TTL，不应依赖真实 HTTP server 端口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     以固定端口调用 create_local，保持测试调用简洁稳定。
    pub fn create_local_for_test(
        &self,
        project_id: &str,
        worktree_id: Option<&str>,
        target_url: &str,
    ) -> WorkbenchBrowserPreview {
        self.create_local(
            project_id.to_string(),
            worktree_id.map(str::to_string),
            target_url.to_string(),
            62116,
        )
    }

    /// 强制测试/smoke 会话过期。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     过期清理不能让测试等待 30 分钟，必须能直接制造过期 session。
    ///
    /// Code Logic（这个函数做什么）:
    ///     在写锁内把目标 session 的 expires_at 调整到过去，并同步设置过期毫秒时间戳。
    pub fn force_expire_for_test(&self, preview_id: &str) {
        if let Some(session) = self
            .inner
            .write()
            .expect("browser preview registry 写锁中毒")
            .get_mut(preview_id)
        {
            session.expires_at = Instant::now() - Duration::from_secs(1);
            session.expires_at_ms = Utc::now().timestamp_millis() - 1_000;
        }
    }
}

impl Default for WorkbenchBrowserPreviewRegistry {
    /// 创建默认浏览器预览注册表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化和测试可用 Default 语义获取空 registry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `WorkbenchBrowserPreviewRegistry::new`。
    fn default() -> Self {
        Self::new()
    }
}

/// 计算下一次过期时间。
///
/// Business Logic（为什么需要这个函数）:
///     预览 session 需要同时有运行时 Instant 和前端可展示的 epoch 毫秒过期时间。
///
/// Code Logic（这个函数做什么）:
///     基于固定 30 分钟 TTL 返回 Instant 过期点与 UTC epoch 毫秒。
fn next_expiry() -> (Instant, i64) {
    (
        Instant::now() + PREVIEW_TTL,
        Utc::now().timestamp_millis() + PREVIEW_TTL.as_millis() as i64,
    )
}

/// 从会话生成前端 DTO。
///
/// Business Logic（为什么需要这个函数）:
///     创建 preview 后，前端需要拿到项目归属、目标 URL 和两个入口 URL。
///
/// Code Logic（这个函数做什么）:
///     从 session target 提取展示 targetUrl，并拼接 desktop proxy URL 与 mobile proxy path。
fn session_to_preview(
    session: &BrowserPreviewSession,
    actual_http_port: u16,
) -> WorkbenchBrowserPreview {
    WorkbenchBrowserPreview {
        preview_id: session.preview_id.clone(),
        project_id: session.project_id.clone(),
        worktree_id: session.worktree_id.clone(),
        target_url: session_target_url(session).to_string(),
        desktop_proxy_url: desktop_proxy_url(actual_http_port, &session.preview_id),
        mobile_proxy_path: mobile_proxy_path(&session.preview_id),
        expires_at_ms: session.expires_at_ms,
    }
}

/// 读取 session 展示目标 URL。
///
/// Business Logic（为什么需要这个函数）:
///     relay preview 对用户展示的是远端 dev server target，而不是 owner proxy URL。
///
/// Code Logic（这个函数做什么）:
///     Local 和 RemoteRelay 都返回其保存的 target_url 字段引用。
fn session_target_url(session: &BrowserPreviewSession) -> &str {
    match &session.target {
        BrowserPreviewTarget::Local { target_url } => target_url,
        BrowserPreviewTarget::RemoteRelay { target_url, .. } => target_url,
    }
}

/// 构造桌面端代理 URL。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端 iframe 需要可直接打开的本机 HTTP 绝对 URL。
///
/// Code Logic（这个函数做什么）:
///     使用实际 axum 监听端口拼出 `/api/workbench/browser/proxy/{previewId}/`。
fn desktop_proxy_url(actual_http_port: u16, preview_id: &str) -> String {
    format!("http://127.0.0.1:{actual_http_port}/api/workbench/browser/proxy/{preview_id}/")
}

/// 构造移动端同源代理 path。
///
/// Business Logic（为什么需要这个函数）:
///     手机浏览器只能使用本机 HTTP server 同源 path，避免 iframe 跨源访问远端设备。
///
/// Code Logic（这个函数做什么）:
///     拼出 `/api/mobile/workbench/browser/proxy/{previewId}/`，由当前设备 HTTP server 处理。
fn mobile_proxy_path(preview_id: &str) -> String {
    format!("/api/mobile/workbench/browser/proxy/{preview_id}/")
}

/// 代理 Workbench Browser preview 请求。
///
/// Business Logic（为什么需要这个函数）:
///     桌面端和移动端 iframe 都只能访问本机同源 preview path，由后端按 previewId 安全转发到已登记目标。
///
/// Code Logic（这个函数做什么）:
///     从 registry 查找并续期 session；WebSocket upgrade 走 WS 桥接，其余 HTTP 请求读取 body 后转发到上游。
pub async fn proxy_workbench_browser_request(
    state: AppState,
    preview_id: String,
    tail_path: String,
    req: Request<Body>,
    route_prefix: &'static str,
) -> Result<Response, ApiError> {
    // 会话查找 + Origin 最终裁决（null 仅 live session；非 null 必须同源）。
    let session = accept_preview_request_with_origin(
        &state.workbench_browser_previews,
        &preview_id,
        req.headers(),
    )?;
    if is_websocket_upgrade(req.headers()) {
        return proxy_workbench_browser_websocket(state, session, tail_path, req).await;
    }
    proxy_http_request_for_session(session, tail_path, req, route_prefix).await
}

/// 查找 preview session 或返回 404。
///
/// Business Logic（为什么需要这个函数）:
///     proxy route 在任何上游访问前都必须确认 previewId 有效，未知/过期 preview 不应触发网络请求。
///
/// Code Logic（这个函数做什么）:
///     调用 registry.lookup；None 映射为 ApiError::not_found，Some 返回已续期 session。
fn lookup_preview_or_not_found(
    registry: &WorkbenchBrowserPreviewRegistry,
    preview_id: &str,
) -> Result<BrowserPreviewSession, ApiError> {
    registry
        .lookup(preview_id)
        .ok_or_else(|| ApiError::not_found("预览会话不存在或已过期"))
}

/// 在 live preview session 查找成功后执行 Origin 最终判定。
///
/// Business Logic（为什么需要这个函数）:
///     opaque sandbox iframe 可能发送 `Origin: null`；该例外只能绑定有效 preview session，
///     不能在全局 guard 对未知/过期 previewId 直接放行。同时对非 null Origin 做会话层同源复检，
///     防止未来子路由漏挂全局 guard 时跨站 + 有效 previewId 直达上游。
///
/// Code Logic（这个函数做什么）:
///     读取 Origin：缺失（native）允许；字面 `null` 允许；其它非 null 必须与 Host 规范化同源
///     （复用 `lan_guard::is_same_origin_with_host`），否则 403。缺少 Host 时 fail-closed 403。
///     调用方必须先 `lookup_preview_or_not_found`。
fn enforce_preview_origin_after_session_lookup(headers: &HeaderMap) -> Result<(), ApiError> {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => Ok(()),
        Some(origin) if origin.trim().eq_ignore_ascii_case("null") => Ok(()),
        Some(origin) => {
            let host = headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .filter(|h| !h.trim().is_empty())
                .ok_or_else(|| ApiError::forbidden("预览请求缺少 Host，无法校验同源 Origin"))?;
            if crate::net::lan_guard::is_same_origin_with_host(origin, host) {
                Ok(())
            } else {
                Err(ApiError::forbidden("预览请求跨站 Origin 不被允许"))
            }
        }
    }
}

/// 在 preview session 查找前后统一评估 Origin（生产入口与测试/smoke 共用）。
///
/// Business Logic（为什么需要这个函数）:
///     未知/过期 previewId 即使带 `Origin: null` 也必须失败；有效 session 才允许 null；
///     非 null Origin 必须同源。生产 proxy 与 unit/smoke 必须走同一裁决路径。
///
/// Code Logic（这个函数做什么）:
///     先 lookup；失败直接返回 404；成功再执行 `enforce_preview_origin_after_session_lookup`。
pub fn accept_preview_request_with_origin(
    registry: &WorkbenchBrowserPreviewRegistry,
    preview_id: &str,
    headers: &HeaderMap,
) -> Result<BrowserPreviewSession, ApiError> {
    let session = lookup_preview_or_not_found(registry, preview_id)?;
    enforce_preview_origin_after_session_lookup(headers)?;
    Ok(session)
}

/// 转发普通 HTTP preview 请求。
///
/// Business Logic（为什么需要这个函数）:
///     Browser iframe 里的 HTML/CSS/JS/API 请求需要透明转发到 dev server 或 owner proxy。
///
/// Code Logic（这个函数做什么）:
///     根据 session 构造上游 URL，过滤 hop-by-hop header，读取请求 body，使用 reqwest 发起请求并转换响应。
async fn proxy_http_request_for_session(
    session: BrowserPreviewSession,
    tail_path: String,
    req: Request<Body>,
    route_prefix: &'static str,
) -> Result<Response, ApiError> {
    let upstream_url = build_upstream_proxy_url(&session, &tail_path, req.uri().query())?;
    let method = reqwest_method(req.method())?;
    let headers = filtered_proxy_headers(req.headers());
    let body = read_proxy_request_body(req.into_body()).await?;
    let response = proxy_http_client()?
        .request(method, upstream_url)
        .headers(headers)
        .body(body)
        .send()
        .await?;
    response_to_axum_response(&session, response, route_prefix).await
}

/// 读取代理请求体。
///
/// Business Logic（为什么需要这个函数）:
///     preview proxy 需要支持 POST/PUT 等开发接口请求，但必须拒绝超过 HTTP server 上限的 body，避免内存耗尽。
///
/// Code Logic（这个函数做什么）:
///     用 32MB 上限聚合 axum Body；超限错误映射为 413，其它读取错误保留 400。
async fn read_proxy_request_body(body: Body) -> Result<Bytes, ApiError> {
    to_bytes(body, PROXY_BODY_LIMIT_BYTES)
        .await
        .map_err(|error| {
            if std::error::Error::source(&error)
                .map(|source| source.to_string() == AXUM_LENGTH_LIMIT_ERROR)
                .unwrap_or(false)
            {
                ApiError::payload_too_large("预览请求体超过 32MB 限制")
            } else {
                ApiError::bad_request("读取预览请求失败")
            }
        })
}

/// 构造预览 HTTP 代理客户端。
///
/// Business Logic（为什么需要这个函数）:
///     preview proxy 只能请求已登记上游；即使上游返回外链 redirect，也不能由后端继续跟随请求。
///
/// Code Logic（这个函数做什么）:
///     创建禁用自动 redirect 的 reqwest client，让 3xx 和 Location 原样进入响应重写流程。
fn proxy_http_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ApiError::bad_gateway("构造预览 HTTP 客户端失败"))
}

/// 判断请求是否为 WebSocket upgrade。
///
/// Business Logic（为什么需要这个函数）:
///     Vite/Next 等 dev server 的 HMR 依赖 WebSocket，preview proxy 需要识别并走双向桥接。
///
/// Code Logic（这个函数做什么）:
///     case-insensitive 检查 Connection 是否包含 upgrade，且 Upgrade 是否为 websocket。
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
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

/// 构造上游代理 URL。
///
/// Business Logic（为什么需要这个函数）:
///     本机 preview 只能访问登记过的 targetUrl；远端 relay 只能访问 owner 已登记的 proxy path。
///
/// Code Logic（这个函数做什么）:
///     Local 用 target_url 作为 base；RemoteRelay 用 `{baseUrl}/api/workbench/browser/proxy/{remotePreviewId}/` 作为 base，再拼 tail path/query。
fn build_upstream_proxy_url(
    session: &BrowserPreviewSession,
    tail_path: &str,
    query: Option<&str>,
) -> Result<Url, ApiError> {
    let base = match &session.target {
        BrowserPreviewTarget::Local { target_url } => target_url.clone(),
        BrowserPreviewTarget::RemoteRelay {
            base_url,
            remote_preview_id,
            ..
        } => format!(
            "{}/api/workbench/browser/proxy/{remote_preview_id}/",
            base_url.trim_end_matches('/')
        ),
    };
    join_proxy_url(&base, tail_path, query)
}

/// 拼接 base URL、尾部 path 和 query。
///
/// Business Logic（为什么需要这个函数）:
///     iframe 内资源使用相对路径访问，proxy 必须把这些路径稳定落到登记的 base 下。
///
/// Code Logic（这个函数做什么）:
///     确保 base path 以 `/` 结尾后逐段追加 tail，避免 `Url::join` 把绝对 tail 当成新上游。
fn join_proxy_url(base: &str, tail_path: &str, query: Option<&str>) -> Result<Url, ApiError> {
    let mut url = Url::parse(base).map_err(|_| ApiError::bad_request("预览上游地址格式无效"))?;
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path().trim_end_matches('/'));
        url.set_path(&path);
    }
    let tail = tail_path.trim_start_matches('/');
    if !tail.is_empty() {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| ApiError::bad_request("预览上游地址不能追加路径"))?;
        segments.pop_if_empty();
        for segment in tail.split('/') {
            segments.push(segment);
        }
        drop(segments);
    }
    url.set_query(query);
    url.set_fragment(None);
    Ok(url)
}

/// 过滤转发请求头。
///
/// Business Logic（为什么需要这个函数）:
///     代理不能把连接级 header 原样传给上游，否则会破坏 reqwest 与上游之间的新连接语义。
///
/// Code Logic（这个函数做什么）:
///     跳过 hop-by-hop header 与 Host，其余 header 尽力转换为 reqwest HeaderMap。
fn filtered_proxy_headers(headers: &HeaderMap) -> ReqwestHeaderMap {
    let mut out = ReqwestHeaderMap::new();
    for (name, value) in headers {
        if should_drop_proxy_header(name.as_str()) {
            continue;
        }
        let Ok(name) = ReqwestHeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let Ok(value) = ReqwestHeaderValue::from_bytes(value.as_bytes()) else {
            continue;
        };
        out.append(name, value);
    }
    out
}

/// 判断代理时应丢弃的 header。
///
/// Business Logic（为什么需要这个函数）:
///     hop-by-hop header 只对当前连接有效，不属于端到端 HTTP 语义。
///
/// Code Logic（这个函数做什么）:
///     case-insensitive 匹配 RFC 常见连接级 header、upgrade 相关 header 和 host。
fn should_drop_proxy_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
    )
}

/// 转换 reqwest 响应为 axum 响应。
///
/// Business Logic（为什么需要这个函数）:
///     上游 dev server 的状态码、内容类型、缓存头等需要尽量透明返回给 iframe。
///
/// Code Logic（这个函数做什么）:
///     复制状态、过滤响应 header、重写 Location 到本机 proxy path，并把上游 bytes_stream 直接包装为 axum streaming Body。
async fn response_to_axum_response(
    session: &BrowserPreviewSession,
    response: reqwest::Response,
    route_prefix: &'static str,
) -> Result<Response, ApiError> {
    let status = StatusCode::from_u16(response.status().as_u16())
        .map_err(|_| ApiError::bad_gateway("预览上游返回了无效状态码"))?;
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers() {
        if should_drop_proxy_header(name.as_str()) {
            continue;
        }
        let Ok(header_name) = HeaderName::from_bytes(name.as_str().as_bytes()) else {
            continue;
        };
        let header_value = if name == reqwest::header::LOCATION {
            match value
                .to_str()
                .ok()
                .and_then(|raw| rewrite_location(session, raw, route_prefix))
            {
                Some(rewritten) => HeaderValue::from_str(&rewritten)
                    .unwrap_or_else(|_| HeaderValue::from_bytes(value.as_bytes()).unwrap()),
                None => HeaderValue::from_bytes(value.as_bytes()).unwrap(),
            }
        } else {
            HeaderValue::from_bytes(value.as_bytes()).unwrap()
        };
        if let Some(headers) = builder.headers_mut() {
            headers.append(header_name, header_value);
        }
    }
    let body = Body::from_stream(response.bytes_stream());
    builder
        .body(body)
        .map_err(|_| ApiError::bad_gateway("构造预览响应失败"))
}

/// 重写 Location header。
///
/// Business Logic（为什么需要这个函数）:
///     dev server redirect 到自己的绝对 URL 时，iframe 仍应留在本机 preview proxy 下。
///
/// Code Logic（这个函数做什么）:
///     处理 absolute path、target_url absolute URL 以及 remote owner proxy absolute URL，输出当前 route surface 的 proxy path。
fn rewrite_location(
    session: &BrowserPreviewSession,
    raw: &str,
    route_prefix: &'static str,
) -> Option<String> {
    if raw.starts_with('/') && !raw.starts_with("//") {
        if let Some(owner_tail) = owner_proxy_tail_from_path_location(session, raw) {
            return Some(proxy_path_for_tail(
                route_prefix,
                &session.preview_id,
                &owner_tail,
            ));
        }
        return Some(proxy_path_for_tail(
            route_prefix,
            &session.preview_id,
            raw.trim_start_matches('/'),
        ));
    }
    let location = Url::parse(raw).ok()?;
    let target_base = session_target_url(session);
    if let Some(rewritten) =
        rewrite_absolute_location(session, &location, target_base, route_prefix)
    {
        return Some(rewritten);
    }
    if let BrowserPreviewTarget::RemoteRelay {
        base_url,
        remote_preview_id,
        ..
    } = &session.target
    {
        for owner_route_prefix in [
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
            MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
        ] {
            let owner_proxy_base = format!(
                "{}{owner_route_prefix}/{remote_preview_id}/",
                base_url.trim_end_matches('/')
            );
            if let Some(rewritten) =
                rewrite_absolute_location(session, &location, &owner_proxy_base, route_prefix)
            {
                return Some(rewritten);
            }
        }
    }
    None
}

/// 从 remote owner proxy 的 path Location 提取真实 tail。
///
/// Business Logic（为什么需要这个函数）:
///     remote relay 上游可能返回 `/api/workbench/browser/proxy/<remote>/...`，本机必须把它映射成当前 local preview 的相同 tail。
///
/// Code Logic（这个函数做什么）:
///     仅 RemoteRelay 生效；识别 desktop/mobile owner proxy prefix，返回 remote previewId 后的 path/query。
fn owner_proxy_tail_from_path_location(
    session: &BrowserPreviewSession,
    raw: &str,
) -> Option<String> {
    let BrowserPreviewTarget::RemoteRelay {
        remote_preview_id, ..
    } = &session.target
    else {
        return None;
    };
    let (path, query) = split_path_and_query(raw);
    for route_prefix in [
        DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
    ] {
        if let Some(tail) = extract_proxy_tail_from_path(path, route_prefix, remote_preview_id) {
            return Some(append_query_to_tail(tail, query));
        }
    }
    None
}

/// 重写某个 base URL 下的绝对 Location。
///
/// Business Logic（为什么需要这个函数）:
///     Local target 和 Remote owner proxy 都可能返回绝对跳转地址，需要共用匹配与 tail 计算。
///
/// Code Logic（这个函数做什么）:
///     比较 scheme/host/port 与 path 前缀，命中后把剩余 tail 拼成本机 proxy path 并保留 query。
fn rewrite_absolute_location(
    session: &BrowserPreviewSession,
    location: &Url,
    base: &str,
    route_prefix: &'static str,
) -> Option<String> {
    let mut base_url = Url::parse(base).ok()?;
    if location.scheme() != base_url.scheme()
        || location.host_str() != base_url.host_str()
        || location.port_or_known_default() != base_url.port_or_known_default()
    {
        return None;
    }
    base_url.set_query(None);
    base_url.set_fragment(None);
    let tail = tail_for_base_path(location.path(), base_url.path())?;
    let tail = append_query_to_tail(tail, location.query());
    Some(proxy_path_for_tail(
        route_prefix,
        &session.preview_id,
        &tail,
    ))
}

/// 拆分 path 与 query。
///
/// Business Logic（为什么需要这个函数）:
///     Location 可以是只有 path 的 header，重写 remote owner proxy path 时仍要保留 query。
///
/// Code Logic（这个函数做什么）:
///     按第一个 `?` 分离 path 和 query，不做 URL 解码。
fn split_path_and_query(raw: &str) -> (&str, Option<&str>) {
    raw.split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((raw, None))
}

/// 从 proxy path 中提取 previewId 后的 tail。
///
/// Business Logic（为什么需要这个函数）:
///     remote owner proxy prefix 是协议路径，不是 dev server 资源路径。
///
/// Code Logic（这个函数做什么）:
///     匹配 `route_prefix/previewId` 或其子路径，返回不含前导 `/` 的 tail。
fn extract_proxy_tail_from_path(
    path: &str,
    route_prefix: &str,
    preview_id: &str,
) -> Option<String> {
    let prefix = format!("{}/{preview_id}", route_prefix.trim_end_matches('/'));
    if path == prefix {
        return Some(String::new());
    }
    let prefix_with_slash = format!("{prefix}/");
    path.strip_prefix(&prefix_with_slash)
        .map(|tail| tail.to_string())
}

/// 根据 base path 提取 absolute URL 的 tail。
///
/// Business Logic（为什么需要这个函数）:
///     上游可能 redirect 到 base 本身或 base 下任意子路径，proxy 需要统一映射为当前 preview tail。
///
/// Code Logic（这个函数做什么）:
///     支持根路径 base、带尾斜杠 base 和无尾斜杠 base，返回不含前导 `/` 的 tail。
fn tail_for_base_path(location_path: &str, base_path: &str) -> Option<String> {
    if base_path == "/" {
        return Some(location_path.trim_start_matches('/').to_string());
    }
    let normalized_base = base_path.trim_end_matches('/');
    if location_path == normalized_base {
        return Some(String::new());
    }
    let base_with_slash = format!("{normalized_base}/");
    location_path
        .strip_prefix(&base_with_slash)
        .map(|tail| tail.to_string())
}

/// 给 tail 追加 query。
///
/// Business Logic（为什么需要这个函数）:
///     redirect 重写不能丢失登录回跳、Vite cache busting 等 query 参数。
///
/// Code Logic（这个函数做什么）:
///     query 存在时用 `?` 拼回 tail；tail 为空时返回 `?query` 供 proxy_path_for_tail 生成 `/preview/?query`。
fn append_query_to_tail(mut tail: String, query: Option<&str>) -> String {
    if let Some(query) = query {
        tail.push('?');
        tail.push_str(query);
    }
    tail
}

/// 构造 workbench proxy path。
///
/// Business Logic（为什么需要这个函数）:
///     redirect 重写需要把任意上游 tail 转回当前本机 previewId 的代理路径。
///
/// Code Logic（这个函数做什么）:
///     拼出 `{route_prefix}/{previewId}/{tail}`，tail 可包含 query。
fn proxy_path_for_tail(route_prefix: &str, preview_id: &str, tail: &str) -> String {
    let route_prefix = route_prefix.trim_end_matches('/');
    if tail.is_empty() {
        format!("{route_prefix}/{preview_id}/")
    } else {
        format!(
            "{route_prefix}/{preview_id}/{}",
            tail.trim_start_matches('/')
        )
    }
}

/// 转换 HTTP method。
///
/// Business Logic（为什么需要这个函数）:
///     axum 与 reqwest 都基于 HTTP method，但显式转换能避免版本差异导致类型不兼容。
///
/// Code Logic（这个函数做什么）:
///     通过 method 字符串 bytes 构造 reqwest::Method。
fn reqwest_method(method: &axum::http::Method) -> Result<ReqwestMethod, ApiError> {
    ReqwestMethod::from_bytes(method.as_str().as_bytes())
        .map_err(|_| ApiError::bad_request("预览请求方法无效"))
}

/// 代理 WebSocket preview 请求。
///
/// Business Logic（为什么需要这个函数）:
///     dev server HMR 需要 WebSocket 双向消息转发，否则 iframe 中的开发预览无法热更新。
///
/// Code Logic（这个函数做什么）:
///     从原始请求提取 WebSocketUpgrade，构造 ws/wss 上游 URL，并在 upgrade 后桥接上下游消息。
async fn proxy_workbench_browser_websocket(
    _state: AppState,
    session: BrowserPreviewSession,
    tail_path: String,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    let upstream_url = build_upstream_websocket_url(&session, &tail_path, req.uri().query())?;
    let (mut parts, _body) = req.into_parts();
    let upstream_request = build_upstream_websocket_request(&upstream_url, &parts.headers)?;
    let (upstream, upstream_response) = connect_async(upstream_request)
        .await
        .map_err(|_| ApiError::bad_gateway("连接预览 WebSocket 上游失败"))?;
    let selected_protocol = selected_websocket_protocol(upstream_response.headers());
    let upgrade = WebSocketUpgrade::from_request_parts(&mut parts, &())
        .await
        .map_err(|_| ApiError::bad_request("预览 WebSocket upgrade 请求无效"))?;
    let upgrade = if let Some(protocol) = selected_protocol {
        upgrade.protocols([protocol])
    } else {
        upgrade
    };
    Ok(upgrade
        .on_upgrade(move |socket| async move {
            if let Err(error) = bridge_websocket(socket, upstream).await {
                tracing::debug!("浏览器预览 WebSocket 代理结束: {error}");
            }
        })
        .into_response())
}

/// 构造 WebSocket 上游 URL。
///
/// Business Logic（为什么需要这个函数）:
///     HMR 请求仍需沿用 HTTP proxy 的安全目标选择，只是协议从 http/https 切换为 ws/wss。
///
/// Code Logic（这个函数做什么）:
///     复用 build_upstream_proxy_url 拼接 path/query，再把 scheme 映射为 ws/wss。
fn build_upstream_websocket_url(
    session: &BrowserPreviewSession,
    tail_path: &str,
    query: Option<&str>,
) -> Result<String, ApiError> {
    let mut url = build_upstream_proxy_url(session, tail_path, query)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" => "ws",
        "wss" => "wss",
        _ => return Err(ApiError::bad_request("预览 WebSocket 上游协议无效")),
    };
    url.set_scheme(scheme)
        .map_err(|_| ApiError::bad_request("预览 WebSocket 上游协议无法转换"))?;
    Ok(url.to_string())
}

/// 构造上游 WebSocket 握手请求。
///
/// Business Logic（为什么需要这个函数）:
///     Vite HMR、受保护 dev server 和登录态预览需要 Cookie/Origin/Authorization/subprotocol 能传到上游。
///
/// Code Logic（这个函数做什么）:
///     先让 tungstenite 生成标准 WS 握手头，再追加安全允许的端到端 header，不转发 Host/Connection/Key 等连接级头。
fn build_upstream_websocket_request(
    upstream_url: &str,
    downstream_headers: &HeaderMap,
) -> Result<TungsteniteRequest, ApiError> {
    let mut request = upstream_url
        .into_client_request()
        .map_err(|_| ApiError::bad_request("预览 WebSocket 上游地址无效"))?;
    copy_forwarded_websocket_headers(downstream_headers, request.headers_mut());
    Ok(request)
}

/// 复制允许透传的 WebSocket header。
///
/// Business Logic（为什么需要这个函数）:
///     WebSocket preview 需要保留应用认证与 HMR subprotocol，但不能转发 hop-by-hop 握手内部 header。
///
/// Code Logic（这个函数做什么）:
///     仅复制 Cookie、Origin、Authorization、Sec-WebSocket-Protocol，保持其它握手 header 由 tungstenite 生成。
fn copy_forwarded_websocket_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for name in [
        header::COOKIE,
        header::ORIGIN,
        header::AUTHORIZATION,
        HeaderName::from_static("sec-websocket-protocol"),
    ] {
        for value in from.get_all(&name) {
            to.append(name.clone(), value.clone());
        }
    }
}

/// 读取上游选择的 WebSocket subprotocol。
///
/// Business Logic（为什么需要这个函数）:
///     如果上游 HMR server 接受了某个 subprotocol，下游浏览器也必须看到同一个协商结果。
///
/// Code Logic（这个函数做什么）:
///     从上游握手响应读取单个 Sec-WebSocket-Protocol 值，并转换为 axum protocols 接口需要的 String。
fn selected_websocket_protocol(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HeaderName::from_static("sec-websocket-protocol"))
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 桥接上下游 WebSocket。
///
/// Business Logic（为什么需要这个函数）:
///     iframe 中的 HMR client 和 dev server 需要持续双向通信，任一侧关闭都应结束桥接。
///
/// Code Logic（这个函数做什么）:
///     使用已经完成握手的上游 WebSocket，split 上下游流，用 tokio::select 在两个方向之间转发消息。
async fn bridge_websocket(socket: WebSocket, upstream: UpstreamWebSocket) -> Result<(), String> {
    let (mut upstream_write, mut upstream_read) = upstream.split();
    let (mut downstream_write, mut downstream_read) = socket.split();

    let client_to_upstream = async {
        while let Some(message) = downstream_read.next().await {
            let message = message.map_err(|error| format!("读取下游 WebSocket 失败: {error}"))?;
            upstream_write
                .send(axum_to_tungstenite_message(message))
                .await
                .map_err(|error| format!("写入上游 WebSocket 失败: {error}"))?;
        }
        Ok::<(), String>(())
    };

    let upstream_to_client = async {
        while let Some(message) = upstream_read.next().await {
            let message = message.map_err(|error| format!("读取上游 WebSocket 失败: {error}"))?;
            if let Some(message) = tungstenite_to_axum_message(message) {
                downstream_write
                    .send(message)
                    .await
                    .map_err(|error| format!("写入下游 WebSocket 失败: {error}"))?;
            }
        }
        Ok::<(), String>(())
    };

    tokio::select! {
        result = client_to_upstream => result,
        result = upstream_to_client => result,
    }
}

/// 转换 axum WebSocket 消息到 tungstenite 消息。
///
/// Business Logic（为什么需要这个函数）:
///     WebSocket 桥接两端使用不同消息类型，但业务上需要保留文本、二进制和心跳消息。
///
/// Code Logic（这个函数做什么）:
///     按同名 variant 转换；Close frame 只保留关闭语义，不保留 code/reason 细节。
fn axum_to_tungstenite_message(message: AxumWsMessage) -> TungsteniteMessage {
    match message {
        AxumWsMessage::Text(text) => TungsteniteMessage::Text(text),
        AxumWsMessage::Binary(binary) => TungsteniteMessage::Binary(binary),
        AxumWsMessage::Ping(ping) => TungsteniteMessage::Ping(ping),
        AxumWsMessage::Pong(pong) => TungsteniteMessage::Pong(pong),
        AxumWsMessage::Close(_) => TungsteniteMessage::Close(None),
    }
}

/// 转换 tungstenite WebSocket 消息到 axum 消息。
///
/// Business Logic（为什么需要这个函数）:
///     上游 HMR server 的消息需要转成 axum 类型后写回浏览器。
///
/// Code Logic（这个函数做什么）:
///     按同名 variant 转换；忽略 tungstenite 的 raw Frame，Close frame 只保留关闭语义。
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::extract::State;
    use axum::http::{Request, StatusCode};
    use axum::routing::any;
    use axum::Router;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    /// Business Logic（为什么需要这个测试）:
    ///     未登记或已过期的 previewId 不能访问任何上游目标。
    ///
    /// Code Logic（这个测试做什么）:
    ///     新建空 registry 后查找不存在的 previewId，断言返回 None。
    #[test]
    fn registry_rejects_unknown_preview_id() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        assert!(registry.lookup("missing").is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     预览链接会暴露给 iframe 和手机浏览器，previewId 必须不可预测且每次创建不同。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续创建两个相同 target 的 preview，断言 UUID simple 字符串不同且长度不小于 32。
    #[test]
    fn registry_creates_unpredictable_preview_ids() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let first = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
        let second = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");

        assert_ne!(first.preview_id, second.preview_id);
        assert!(first.preview_id.len() >= 32);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview session 过期后应立即失效，避免旧手机链接长期保留访问能力。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 preview 后强制过期，再通过 lookup 触发清理并断言无法找到。
    #[test]
    fn registry_expires_old_sessions() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
        registry.force_expire_for_test(&preview.preview_id);

        assert!(registry.lookup(&preview.preview_id).is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     未知 previewId 不能触发任何上游请求，应在本机 proxy 层直接返回 404 语义。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对空 registry 执行 lookup helper，断言错误状态是 NOT_FOUND。
    #[test]
    fn proxy_unknown_preview_id_returns_not_found() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let error = lookup_preview_or_not_found(&registry, "missing")
            .expect_err("missing preview should return not found");

        assert_eq!(error.status(), StatusCode::NOT_FOUND);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview proxy 不能因为请求 tail 看起来像绝对 URL 就变成开放代理。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 `http://evil.test` tail，断言上游 host 仍是登记 target，tail 只作为 path segment 追加。
    #[test]
    fn proxy_url_keeps_absolute_tail_under_registered_target() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview =
            registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/app/");
        let session = registry.lookup(&preview.preview_id).unwrap();

        let url =
            build_upstream_proxy_url(&session, "http://evil.test/pwn", Some("version=1")).unwrap();

        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(5173));
        assert_eq!(url.path(), "/app/http://evil.test/pwn");
        assert_eq!(url.query(), Some("version=1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     本机 preview proxy 必须把 iframe 的 path/query 转发到用户选择的 loopback dev server。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动测试 upstream，创建 local session，发起 GET 代理请求并断言 upstream 收到完整 path/query。
    #[tokio::test]
    async fn local_preview_proxy_forwards_get_path_and_query() {
        let seen_path = std::sync::Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/assets/app.js",
                any(
                    |State(seen_path): State<std::sync::Arc<Mutex<Option<String>>>>,
                     req: Request<Body>| async move {
                        *seen_path.lock().unwrap() =
                            Some(req.uri().path_and_query().unwrap().to_string());
                        "local ok"
                    },
                ),
            )
            .with_state(seen_path.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local(
            "project-a".to_string(),
            None,
            format!("http://{addr}/"),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("GET")
            .uri("http://127.0.0.1/proxy?version=1")
            .body(Body::empty())
            .unwrap();

        let response = proxy_http_request_for_session(
            session,
            "assets/app.js".to_string(),
            req,
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"local ok");
        assert_eq!(
            seen_path.lock().unwrap().as_deref(),
            Some("/assets/app.js?version=1")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     上游 dev server 可能返回很大或持续分块的响应，preview proxy 不能等完整 body 聚合到内存后才返回 iframe。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造延迟分块 upstream，断言 proxy 在后续 chunk 到达前先返回 response，并保留 status/header/content。
    #[tokio::test]
    async fn http_proxy_streams_upstream_response_without_waiting_for_full_body() {
        let app = Router::new().route(
            "/stream",
            any(|| async move {
                let stream = futures_util::stream::unfold(0u8, |state| async move {
                    match state {
                        0 => Some((Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk-1|")), 1)),
                        1 => {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            Some((Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk-2|")), 2))
                        }
                        2 => {
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            Some((Ok::<Bytes, Infallible>(Bytes::from_static(b"chunk-3")), 3))
                        }
                        _ => None,
                    }
                });
                Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .header("x-preview-stream", "chunked")
                    .body(Body::from_stream(stream))
                    .unwrap()
            }),
        );
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local(
            "project-a".to_string(),
            None,
            format!("http://{addr}/"),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("GET")
            .uri("http://127.0.0.1/proxy")
            .body(Body::empty())
            .unwrap();

        let response = tokio::time::timeout(
            Duration::from_millis(150),
            proxy_http_request_for_session(
                session,
                "stream".to_string(),
                req,
                DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
            ),
        )
        .await
        .expect("proxy should return headers before the upstream body finishes")
        .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            response.headers().get("x-preview-stream").unwrap(),
            "chunked"
        );
        let body = tokio::time::timeout(
            Duration::from_secs(2),
            to_bytes(response.into_body(), usize::MAX),
        )
        .await
        .expect("streamed body should finish")
        .unwrap();
        assert_eq!(&body[..], b"chunk-1|chunk-2|chunk-3");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端项目 relay preview 只能转发到 owning device 的 preview proxy，不能直接访问远端设备的 loopback targetUrl。
    ///
    /// Code Logic（这个测试做什么）:
    ///     创建 remote relay session，owner 测试服务只监听 owner proxy path，断言请求命中该 path。
    #[tokio::test]
    async fn remote_relay_proxy_forwards_to_owner_proxy_path() {
        let seen_path = std::sync::Arc::new(Mutex::new(None));
        let app = Router::new()
            .route(
                "/api/workbench/browser/proxy/remote-preview/assets/app.js",
                any(
                    |State(seen_path): State<std::sync::Arc<Mutex<Option<String>>>>,
                     req: Request<Body>| async move {
                        *seen_path.lock().unwrap() =
                            Some(req.uri().path_and_query().unwrap().to_string());
                        "remote ok"
                    },
                ),
            )
            .with_state(seen_path.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_remote_relay(
            "project-a".to_string(),
            Some("remote:device-a:inner-worktree".to_string()),
            format!("http://{addr}"),
            "remote-preview".to_string(),
            "http://127.0.0.1:9/".to_string(),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("GET")
            .uri("http://127.0.0.1/proxy?version=2")
            .body(Body::empty())
            .unwrap();

        let response = proxy_http_request_for_session(
            session,
            "assets/app.js".to_string(),
            req,
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"remote ok");
        assert_eq!(
            seen_path.lock().unwrap().as_deref(),
            Some("/api/workbench/browser/proxy/remote-preview/assets/app.js?version=2")
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview proxy 必须拒绝超过 HTTP server body limit 的大请求，避免一次性读取无限请求体导致内存耗尽。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 32MB+1 的 POST body，断言 proxy 返回 413，并确认测试 upstream 没有收到任何请求。
    #[tokio::test]
    async fn http_proxy_rejects_oversized_request_body_without_forwarding() {
        let upstream_hits = std::sync::Arc::new(Mutex::new(0usize));
        let app = Router::new()
            .route(
                "/upload",
                any(
                    |State(hits): State<std::sync::Arc<Mutex<usize>>>| async move {
                        *hits.lock().unwrap() += 1;
                        "unexpected"
                    },
                ),
            )
            .with_state(upstream_hits.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local(
            "project-a".to_string(),
            None,
            format!("http://{addr}/"),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/proxy")
            .body(Body::from(vec![b'x'; PROXY_BODY_LIMIT_BYTES + 1]))
            .unwrap();

        let error = proxy_http_request_for_session(
            session,
            "upload".to_string(),
            req,
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .await
        .expect_err("oversized proxy body should be rejected before upstream forwarding");

        assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(*upstream_hits.lock().unwrap(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     边界回归：恰好等于 PROXY_BODY_LIMIT_BYTES 的 body 必须仍被接受并转发，
    ///     防止 limit 实现写成 `<` 导致合法 32 MiB 请求被误杀。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造恰好 32 MiB 的 POST body，断言 proxy 成功且 upstream 收到 1 次请求。
    #[tokio::test]
    async fn http_proxy_accepts_exact_body_limit_and_forwards() {
        let upstream_hits = std::sync::Arc::new(Mutex::new(0usize));
        let app = Router::new()
            .route(
                "/upload",
                any(
                    |State(hits): State<std::sync::Arc<Mutex<usize>>>| async move {
                        *hits.lock().unwrap() += 1;
                        "ok"
                    },
                ),
            )
            .with_state(upstream_hits.clone());
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local(
            "project-a".to_string(),
            None,
            format!("http://{addr}/"),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/proxy")
            .body(Body::from(vec![b'x'; PROXY_BODY_LIMIT_BYTES]))
            .unwrap();

        let response = proxy_http_request_for_session(
            session,
            "upload".to_string(),
            req,
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .await
        .expect("exact PROXY_BODY_LIMIT_BYTES body must be accepted");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(*upstream_hits.lock().unwrap(), 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     preview proxy 只允许访问已登记的 loopback dev server，不能跟随该 server 的外链 redirect 继续由后端请求。
    ///
    /// Code Logic（这个测试做什么）:
    ///     上游返回指向另一个测试服务的 302，断言 proxy 返回 302 且另一个服务没有收到请求。
    #[tokio::test]
    async fn http_proxy_does_not_follow_external_redirect_location() {
        let external_hits = std::sync::Arc::new(Mutex::new(0usize));
        let external_app = Router::new()
            .route(
                "/steal",
                any(
                    |State(hits): State<std::sync::Arc<Mutex<usize>>>| async move {
                        *hits.lock().unwrap() += 1;
                        "external"
                    },
                ),
            )
            .with_state(external_hits.clone());
        let external_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let external_addr = external_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(external_listener, external_app).await.unwrap();
        });

        let redirect_target = format!("http://{external_addr}/steal");
        let upstream_app = Router::new().route(
            "/jump",
            any(move || {
                let redirect_target = redirect_target.clone();
                async move {
                    Response::builder()
                        .status(StatusCode::FOUND)
                        .header(header::LOCATION, redirect_target)
                        .body(Body::empty())
                        .unwrap()
                }
            }),
        );
        let upstream_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_app).await.unwrap();
        });

        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local(
            "project-a".to_string(),
            None,
            format!("http://{upstream_addr}/"),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();
        let req = Request::builder()
            .method("GET")
            .uri("http://127.0.0.1/proxy")
            .body(Body::empty())
            .unwrap();

        let response = proxy_http_request_for_session(
            session,
            "jump".to_string(),
            req,
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(*external_hits.lock().unwrap(), 0);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     remote relay 收到 owner proxy 的绝对路径 redirect 时，应保持当前本机 previewId，而不是把 owner proxy 前缀当成普通资源路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 RemoteRelay session 和 owner proxy Location，断言只保留 owner preview 后面的 tail。
    #[test]
    fn remote_relay_rewrites_owner_proxy_absolute_path_location_to_local_tail() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_remote_relay(
            "project-a".to_string(),
            None,
            "http://owner.local:62116".to_string(),
            "remote-preview".to_string(),
            "http://127.0.0.1:5173/".to_string(),
            62116,
        );
        let session = registry.lookup(&preview.preview_id).unwrap();

        let rewritten = rewrite_location(
            &session,
            "/api/workbench/browser/proxy/remote-preview/login?next=%2F",
            DESKTOP_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            format!(
                "/api/workbench/browser/proxy/{}/login?next=%2F",
                preview.preview_id
            )
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     mobile iframe 的 redirect 必须留在 mobile 同源 route，不能被改写到桌面 Workbench route。
    ///
    /// Code Logic（这个测试做什么）:
    ///     用 local session 重写绝对 path Location，断言输出使用 mobile proxy prefix。
    #[test]
    fn mobile_location_rewrite_keeps_mobile_proxy_prefix() {
        let registry = WorkbenchBrowserPreviewRegistry::new();
        let preview = registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
        let session = registry.lookup(&preview.preview_id).unwrap();

        let rewritten = rewrite_location(
            &session,
            "/login?next=%2Fdashboard",
            MOBILE_BROWSER_PROXY_ROUTE_PREFIX,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            format!(
                "/api/mobile/workbench/browser/proxy/{}/login?next=%2Fdashboard",
                preview.preview_id
            )
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     WebSocket preview 需要把认证、Origin 和 HMR subprotocol 传给上游，同时避免成为原始握手 header 的盲转发器。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造下游 header，断言上游请求包含允许 header，且不透传 Proxy-Authorization。
    #[test]
    fn websocket_upstream_request_forwards_only_safe_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("sid=abc"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:62116"),
        );
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer token"),
        );
        headers.insert(
            HeaderName::from_static("sec-websocket-protocol"),
            HeaderValue::from_static("vite-hmr"),
        );
        headers.insert(
            HeaderName::from_static("proxy-authorization"),
            HeaderValue::from_static("Basic secret"),
        );

        let request = build_upstream_websocket_request("ws://127.0.0.1:5173/ws", &headers)
            .expect("websocket request should build");

        assert_eq!(request.headers().get(header::COOKIE).unwrap(), "sid=abc");
        assert_eq!(
            request.headers().get(header::ORIGIN).unwrap(),
            "http://127.0.0.1:62116"
        );
        assert_eq!(
            request.headers().get(header::AUTHORIZATION).unwrap(),
            "Bearer token"
        );
        assert_eq!(
            request
                .headers()
                .get(HeaderName::from_static("sec-websocket-protocol"))
                .unwrap(),
            "vite-hmr"
        );
        assert!(request
            .headers()
            .get(HeaderName::from_static("proxy-authorization"))
            .is_none());
    }

    /// Business Logic（为什么需要这个测试模块）:
    ///     opaque iframe 的 `Origin: null` 只能绑定 live preview session，未知/过期 id 必须失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     覆盖有效 previewId 接受 HTTP/WS 语义的 null Origin；未知与过期 previewId 拒绝 null Origin。
    mod opaque_origin_matrix {
        use super::*;

        fn null_origin_headers() -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(header::ORIGIN, HeaderValue::from_static("null"));
            headers
        }

        fn same_origin_headers() -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:62116"));
            headers.insert(
                header::ORIGIN,
                HeaderValue::from_static("http://127.0.0.1:62116"),
            );
            headers
        }

        #[test]
        fn valid_preview_id_accepts_opaque_origin_null_for_http_and_websocket() {
            let registry = WorkbenchBrowserPreviewRegistry::new();
            let preview =
                registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
            let headers = null_origin_headers();

            let session =
                accept_preview_request_with_origin(&registry, &preview.preview_id, &headers)
                    .expect("live preview should accept Origin:null");
            assert_eq!(session.preview_id, preview.preview_id);

            // WebSocket 与 HTTP 共用同一会话后 Origin 判定。
            enforce_preview_origin_after_session_lookup(&headers)
                .expect("websocket path should also accept Origin:null after session lookup");
        }

        #[test]
        fn valid_preview_id_accepts_same_origin_and_rejects_cross_origin() {
            let registry = WorkbenchBrowserPreviewRegistry::new();
            let preview =
                registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");

            let session = accept_preview_request_with_origin(
                &registry,
                &preview.preview_id,
                &same_origin_headers(),
            )
            .expect("live preview should accept same-origin non-null Origin");
            assert_eq!(session.preview_id, preview.preview_id);

            let mut cross = HeaderMap::new();
            cross.insert(header::HOST, HeaderValue::from_static("127.0.0.1:62116"));
            cross.insert(
                header::ORIGIN,
                HeaderValue::from_static("http://evil.example"),
            );
            let err = accept_preview_request_with_origin(&registry, &preview.preview_id, &cross)
                .expect_err("cross-origin must fail at session layer even with live preview");
            assert_eq!(err.status(), StatusCode::FORBIDDEN);
        }

        #[test]
        fn unknown_or_expired_preview_id_with_origin_null_fails() {
            let registry = WorkbenchBrowserPreviewRegistry::new();
            let headers = null_origin_headers();

            let err = accept_preview_request_with_origin(&registry, "missing-preview", &headers)
                .expect_err("unknown preview + null Origin must fail");
            assert_eq!(err.status(), StatusCode::NOT_FOUND);

            let preview =
                registry.create_local_for_test("project-a", None, "http://127.0.0.1:5173/");
            registry.force_expire_for_test(&preview.preview_id);
            let err = accept_preview_request_with_origin(&registry, &preview.preview_id, &headers)
                .expect_err("expired preview + null Origin must fail");
            assert_eq!(err.status(), StatusCode::NOT_FOUND);
        }
    }
}
