//! net/relay.rs — 中转访问（跳板机）透明转发器核心
//!
//! Business Logic（为什么需要这个模块）:
//!     局域网存在发起方 A 与目标 C 互相不可达、但共享可达邻居 B 的拓扑（跨 VLAN/AP
//!     隔离、防火墙策略）。A 把对 C 的请求发给 B（`/api/relay/{C_device_id}/api/...`），
//!     B 从自己的直连表解析 C 的地址并透明转发，C 零感知、零改动（老版本 C 也能被中转）。
//!     B 不解析 body、不缓存、不改写语义，仅剥路径前缀、透传 headers/body、回传
//!     status/body——一次实现覆盖 C 的全部现有及未来路由（受白名单约束）。
//!
//! Code Logic（这个模块做什么）:
//!     - `RelayRuntime`：转发器运行时（独立 reqwest stream client + 全局/per-target
//!       semaphore + 活跃转发计数），挂在 `AppState.relay` 供路由层共享。
//!     - `filter_forwarded_headers`：剥除 hop-by-hop / 逐跳安全敏感 header 的纯函数
//!       （请求与响应两个方向复用）。
//!     - `is_relay_path_allowed`：转发路径白名单（health/workbench/orchestrator）。
//!     - `resolve_relay_target`：从 `state.devices` 直连表解析目标（必须 online、
//!       拒绝自引用）——单跳硬保证，绝不做二级查找。
//!     - `forward_relay_request`：HTTP 透明转发核心（流式 body，不缓冲）。
//!     - `connect_relay_terminal_upstream` / `bridge_relay_websocket`：终端输入 WS 桥
//!       （axum WS server ↔ tungstenite client，双向帧透传）。
//!     - 错误统一走 `error_response::P2pError::stable` 信封 + `relay_*` domain code。

use crate::models::device::Device;
use crate::net::error_response::P2pError;
use crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::request_context::{P2pRequestContext, REQUEST_ID_HEADER};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::IntoResponse;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request as TungsteniteRequest;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// 转发路径白名单中的精确 health 路径。
const RELAY_ALLOWED_EXACT_HEALTH: &str = "/api/health";
/// 转发路径白名单中的前缀（workbench / orchestrator 业务面）。
const RELAY_ALLOWED_PREFIXES: [&str; 2] = ["/api/workbench/", "/api/orchestrator/"];

/// 全局转发并发上限（同时处于转发中的 HTTP 请求 + WS 桥总数）。
pub const RELAY_GLOBAL_MAX_CONCURRENCY: usize = 8;
/// 单目标设备转发并发上限（防单 target 占满全局额度）。
pub const RELAY_PER_TARGET_MAX_CONCURRENCY: usize = 4;

/// relay 路由统一 URL 前缀（与 `http_server.rs` 注册的字面量保持一致）。
pub const RELAY_ROUTE_PREFIX: &str = "/api/relay/";

/// 中转转发器运行时：共享出站 client、并发闸与活跃计数。
///
/// Business Logic（为什么需要这个结构）:
///     B 被用作跳板时所有 `/api/relay/*` 流量共享同一份资源约束，防止 B 被当作
///     流量放大器：全局并发 8、单目标 4，超限立即返回 `relay_busy`（不排队，
///     排队会让 NDJSON 长流占满等待队列）。活跃计数供后续控制面展示"当前中转连接数"。
///
/// Code Logic（这个结构做什么）:
///     - `client`：独立 reqwest Client（connect_timeout 3s + 连接池、**无总超时**——
///       NDJSON 长流/上传需要），参照 `peer_client.rs` 的 stream_client 先例；
///     - `global_permits`：全局 Semaphore(8)；
///     - `per_target_permits`：`device_id -> Arc<Semaphore(4)>` lazy 创建表；
///     - `active_forwards`：当前活跃转发计数（HTTP + WS），RAII guard 维护。
#[derive(Debug)]
pub struct RelayRuntime {
    client: reqwest::Client,
    global_permits: Arc<Semaphore>,
    per_target_permits: RwLock<HashMap<String, Arc<Semaphore>>>,
    active_forwards: Arc<AtomicUsize>,
    /// 影子设备表（A 侧角色：经跳板可见的目标）。挂在 RelayRuntime 避免新增
    /// AppState 字段波及全部装配点；读写经由 `net::relay_shadow` 的表操作函数。
    pub shadow_devices: RwLock<crate::net::relay_shadow::RelayShadowTable>,
}

impl RelayRuntime {
    /// 构造转发器运行时（AppState 装配时调用一次）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 与 headless 两种形态的 AppState 装配都需要同一份转发器资源；集中构造
    ///     避免各构造点漂移（如超时/池参数不一致）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     reqwest Client 设 connect_timeout=Health(3s) 与 pool；全局 semaphore 容量
    ///     `RELAY_GLOBAL_MAX_CONCURRENCY`；per-target 表初始为空（lazy）。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(PeerTimeoutClass::Health.timeout())
            .pool_max_idle_per_host(8)
            .build()
            .expect("构造 relay reqwest Client 失败（rustls-tls 初始化异常）");
        Self {
            client,
            global_permits: Arc::new(Semaphore::new(RELAY_GLOBAL_MAX_CONCURRENCY)),
            per_target_permits: RwLock::new(HashMap::new()),
            active_forwards: Arc::new(AtomicUsize::new(0)),
            shadow_devices: RwLock::new(HashMap::new()),
        }
    }

    /// 尝试获取一个目标的转发许可（全局 + per-target，非阻塞）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     跳板资源必须 fail-fast：拿到任一闸门失败都不排队等待，立即让调用方返回
    ///     `relay_busy`（503），调用方可退避重试；排队会放大长流占用。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先 `try_acquire_owned` 全局 permit，再 lazy 创建并 `try_acquire_owned`
    ///     per-target permit；任一失败即释放已持有的（drop）并返回 None。成功时
    ///     活跃计数 +1 并包进 RAII guard（drop 时归还两个 permit 并计数 -1）。
    pub fn try_acquire(&self, device_id: &str) -> Option<RelayPermitGuard> {
        let global = self.global_permits.clone().try_acquire_owned().ok()?;
        let target_semaphore = self.target_semaphore(device_id);
        let target = match target_semaphore.try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                // per-target 满：必须显式释放已拿到的全局 permit（否则泄漏全局额度直到 drop）。
                drop(global);
                return None;
            }
        };
        Some(RelayPermitGuard::acquire(
            global,
            target,
            self.active_forwards.clone(),
        ))
    }

    /// 读取当前活跃转发数（HTTP 转发 + WS 桥）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     B 端操作者需要知道"本机正被用作跳板（当前 N 个中转连接）"；控制面/UI 展示
    ///     读取本计数，避免各处自己维护重复计数器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     原子读 `active_forwards`（SeqCst）。
    pub fn active_forwards(&self) -> usize {
        self.active_forwards.load(Ordering::SeqCst)
    }

    /// lazy 获取（或创建）目标设备的 per-target semaphore。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     per-target 并发闸按需创建，避免为每个曾经在线的设备预分配 semaphore。
    ///
    /// Code Logic（这个函数做什么）:
    ///     读锁快查；未命中时写锁 double-check 后插入 `Arc<Semaphore(4)>`。
    ///     临界区内无 await，std RwLock 即可。
    fn target_semaphore(&self, device_id: &str) -> Arc<Semaphore> {
        if let Some(semaphore) = self
            .per_target_permits
            .read()
            .expect("relay per_target_permits 读锁中毒")
            .get(device_id)
        {
            return semaphore.clone();
        }
        let mut writer = self
            .per_target_permits
            .write()
            .expect("relay per_target_permits 写锁中毒");
        writer
            .entry(device_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(RELAY_PER_TARGET_MAX_CONCURRENCY)))
            .clone()
    }

    /// 暴露内部出站 client（测试与 WS 桥上游探测复用同一连接池参数）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     handler 层（`routes/relay.rs`）发起转发需要一个与运行时一致的 client；
    ///     直接暴露引用避免每个 handler 自建 Client 破坏统一池/超时约束。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回 `&reqwest::Client`（Client 内部 Arc，Clone 廉价）。
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

/// 转发许可 RAII guard：持有期间占用全局 + per-target 并发名额并维持活跃计数。
///
/// Business Logic（为什么需要这个结构）:
///     HTTP 转发的生命周期横跨整个响应流回传、WS 桥的生命周期横跨整个会话；
///     手工归还 permit 在任一出错/提前返回路径都会泄漏，必须用 RAII。
///
/// Code Logic（这个结构做什么）:
/// 持有两个 `OwnedSemaphorePermit` 与 `Arc<AtomicUsize>`；构造 +1、Drop -1 并
/// 隐式归还两个 permit。可整体 move 进 `on_upgrade` 闭包随 WS 会话存续。
#[derive(Debug)]
pub struct RelayPermitGuard {
    _global: OwnedSemaphorePermit,
    _target: OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
}

impl RelayPermitGuard {
    /// 获取转发许可（计数 +1），由 `RelayRuntime::try_acquire` 调用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     活跃计数必须与 permit 获取原子地绑定，避免控制面读到"有 permit 无计数"的窗口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     包装两个 permit，fetch_add(1) 后返回 guard。
    fn acquire(
        global: OwnedSemaphorePermit,
        target: OwnedSemaphorePermit,
        active: Arc<AtomicUsize>,
    ) -> Self {
        active.fetch_add(1, Ordering::SeqCst);
        Self {
            _global: global,
            _target: target,
            active,
        }
    }
}

impl Drop for RelayPermitGuard {
    /// 释放转发许可（计数 -1 + 归还两个 permit）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     转发结束（响应回传完/WS 会话结束/出错提前返回）都必须立即归还并发名额，
    ///     否则长流会永久吃掉全局 8 个额度直到进程重启。
    ///
    /// Code Logic（这个函数做什么）:
    ///     fetch_sub(1)；两个 permit 字段 drop 时自动归还 semaphore。
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 转发 hop-by-hop / 逐跳敏感 header 的剥除清单（小写比较）。
///
/// Business Logic（为什么需要这个常量）:
///     代理转发时这些 header 属于单跳连接语义（连接管理、分帧、WS 握手、代理鉴权），
///     或由出站客户端/目标自行重算（host、content-length）；透传它们会导致
///     出站握手错误或响应分帧错乱。集中一张表供请求/响应两个方向与单测共用。
///
/// Code Logic（这个常量做什么）:
///     静态字符串切片数组；`filter_forwarded_headers` 按小写化 name 过滤。
const FORWARDED_STRIP_HEADERS: [&str; 15] = [
    "host",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "sec-websocket-accept",
    "sec-websocket-extensions",
    "sec-websocket-key",
    "sec-websocket-protocol",
    // 注：sec-websocket-version 归入下方 starts-with 前缀处理。
];

/// 过滤可转发的 headers：剥除 hop-by-hop 与 WS 握手 header。
///
/// Business Logic（为什么需要这个函数）:
///     透明转发要求端到端 header（如 `X-CC-Request-Id`、`X-Cc-Partner-Expected-Device-Id`、
///     `Content-Type`、`X-Chunk-Offset`）原样到达目标设备，同时连接级 header 必须剥除；
///     响应方向同理（status/端到端 header 回传，content-length 因流式 body 重算而剥除）。
///
/// Code Logic（这个函数做什么）:
///     复制入站 HeaderMap，跳过清单内 header（小写比较）与 `sec-websocket-*` 前缀；
///     其余 name/value 原样保留。纯函数，请求/响应两方向复用。
pub fn filter_forwarded_headers(headers: &HeaderMap) -> HeaderMap {
    let mut filtered = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers.iter() {
        let lower = name.as_str().to_ascii_lowercase();
        if FORWARDED_STRIP_HEADERS.contains(&lower.as_str()) || lower.starts_with("sec-websocket-")
        {
            continue;
        }
        filtered.append(name.clone(), value.clone());
    }
    filtered
}

/// 判断转发目标路径是否在白名单内。
///
/// Business Logic（为什么需要这个函数）:
///     中转拓扑不应承载双向同步类流量（`/api/sync/*` 等，见设计 §4.4），且缩小攻击面；
///     白名单只放行 health（绑定预检）与 workbench/orchestrator（远程项目访问）业务面。
///
/// Code Logic（这个函数做什么）:
///     精确匹配 `/api/health`，或前缀匹配 `/api/workbench/`、`/api/orchestrator/`；
///     纯函数供 handler 与单测共用。
pub fn is_relay_path_allowed(path: &str) -> bool {
    path == RELAY_ALLOWED_EXACT_HEALTH
        || RELAY_ALLOWED_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

/// 构造 relay 领域稳定错误信封。
///
/// Business Logic（为什么需要这个函数）:
///     A 侧需要用 `relay_*` domain code 区分"跳板故障（busy/disabled/unreachable）"
///     与"目标故障（offline）"来渲染不同错误文案；统一入口保证 code/status/retryable
///     契约不漂移。
///
/// Code Logic（这个函数做什么）:
///     包装 `P2pError::stable`（envelope.code = domain code，request_id 取自 context）；
///     502/503 暂态错误 retryable=true，403/404 稳定错误 retryable=false。
pub(crate) fn relay_error(
    message: impl Into<String>,
    domain_code: &str,
    status: StatusCode,
    retryable: bool,
    context: &P2pRequestContext,
) -> P2pError {
    P2pError::stable(message, domain_code, status, context, retryable)
}

/// 读取 relay 开关（config 热生效：路由常注册，handler 内检查）。
///
/// Business Logic（为什么需要这个函数）:
///     `relay.enabled` 可经 control plane update-config 运行期翻转；路由不动态注册
///     （axum Router 构造后不可变），因此 handler 每次请求都读内存配置，false 时
///     返回 `relay_disabled`（503）。
///
/// Code Logic（这个函数做什么）:
///     读 `state.config` RwLock 的 `relay.enabled`；锁内无 await。
pub(crate) fn relay_enabled(state: &AppState) -> bool {
    state.config.read().expect("config 读锁中毒").relay.enabled
}

/// 从 B 的直连表解析转发目标（单跳硬保证）。
///
/// Business Logic（为什么需要这个函数）:
///     转发目标地址只从 B 自己的直连表（mDNS + manual_peers）解析——转发出的请求
///     是普通 `/api/...` 路径，结构上杜绝 `A → B → D → C` 多跳与环路；同时拒绝
///     `target == 本机 device_id`（防自引用）。目标不在线/不在表内 → fail-closed
///     404 `relay_target_offline`（与 `device_base_url_from_devices` 同语义）。
///
/// Code Logic（这个函数做什么）:
///     先比对本机 device_id；再读 `state.devices` 查 device_id，命中且 `online`
///     才返回 clone 的 Device；否则返回 404 信封错误。
pub(crate) fn resolve_relay_target(
    state: &AppState,
    device_id: &str,
    context: &P2pRequestContext,
) -> Result<Device, P2pError> {
    if device_id == state.device_id.as_str() {
        return Err(relay_error(
            "relay 拒绝自引用目标（目标即本机）",
            "relay_target_offline",
            StatusCode::NOT_FOUND,
            false,
            context,
        ));
    }
    let devices = state.devices.read().expect("devices 读锁中毒");
    match devices.get(device_id) {
        Some(device) if device.online => Ok(device.clone()),
        _ => Err(relay_error(
            format!("中转目标设备不在线: {device_id}"),
            "relay_target_offline",
            StatusCode::NOT_FOUND,
            false,
            context,
        )),
    }
}

/// 连接目标失败时把该 target 在直连表标记 offline（加速收敛）。
///
/// Business Logic（为什么需要这个函数）:
///     devices 表的 online 状态来自 mDNS/manual_peers 探测，最长滞后一个探测周期；
///     转发实测连接失败说明目标已不可达，立即置 false 可让后续请求 fail-fast，
///     不再等下一轮探测。
///
/// Code Logic（这个函数做什么）:
///     写锁 `state.devices`，命中 device_id 则 `online=false`；未命中静默（探测循环
///     会再剔除）。锁内无 await。
pub(crate) fn mark_relay_target_offline(state: &AppState, device_id: &str) {
    let mut devices = state.devices.write().expect("devices 写锁中毒");
    if let Some(device) = devices.get_mut(device_id) {
        device.online = false;
    }
}

/// 携带转发许可的响应体流：流结束/被丢弃时归还并发名额。
///
/// Business Logic（为什么需要这个结构）:
///     流式转发的响应 body 在 handler 返回后仍在向 A 传输（NDJSON 长流可持续数分钟）；
///     若许可在 handler 返回时释放，全局 8 个名额将无法约束活跃长流数。把 guard 绑定
///     到流的 Drop（客户端断开或流自然结束时触发）才能让并发上限反映真实占用。
///
/// Code Logic（这个结构做什么）:
///     内部流固定 `Pin<Box<S>>`（结构体整体 Unpin，无需 unsafe pin 投影）；
///     额外持有 `RelayPermitGuard`，结构体 drop 时随之归还。
struct RelayPermitStream<S> {
    inner: std::pin::Pin<Box<S>>,
    _permit: RelayPermitGuard,
}

impl<S: futures_util::Stream> futures_util::Stream for RelayPermitStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// 一次 HTTP 透明转发的全部输入（`forward_relay_request` 打包参数）。
///
/// Business Logic（为什么需要这个结构）:
///     转发要素（目标/路径/方法/headers/body/请求上下文/并发许可）天然属于同一次
///     转发作业；散落成 9 个裸参数既难读也让调用方容易传错顺序。
///
/// Code Logic（这个结构做什么）:
///     值类型聚合；`permit` 随作业 move 进响应流（`RelayPermitStream`），
///     body 与 headers 来自拆解后的入站 `Request`。
pub(crate) struct RelayForwardJob {
    /// 并发许可（RAII，随响应流生命周期归还）。
    pub permit: RelayPermitGuard,
    /// 已解析的转发目标设备（含 host/port）。
    pub target: Device,
    /// 目标设备上的绝对路径（如 `/api/workbench/projects/open`）。
    pub path: String,
    /// 原样透传的 query string。
    pub query: Option<String>,
    /// 原样透传的 HTTP 方法。
    pub method: reqwest::Method,
    /// 入站 headers（hop-by-hop 由转发核心剥除）。
    pub headers: HeaderMap,
    /// 入站 body（流式，不缓冲）。
    pub body: Body,
    /// 请求上下文（request_id 全链透传）。
    pub context: P2pRequestContext,
}

/// HTTP 透明转发核心：把入站请求流式搬运到目标设备并流式回传响应。
///
/// Business Logic（为什么需要这个函数）:
///     这是跳板机的核心价值——A 无法直连 C 时，B 用一次通用实现转发 C 的全部白名单
///     路由（现有 + 未来），B 不持有任何 Workbench 状态、不解析 body。流式搬运保证
///     32 MiB 级请求/NDJSON 长流不在 B 内存放大。
///
/// Code Logic（这个函数做什么）:
///     1. 过滤 headers（剥 hop-by-hop）并显式写入 `X-CC-Request-Id = context.request_id`
///        （入站缺失时 middleware 生成的 ID 也能贯穿全链）；
///     2. query 透传（拼到出站 URL）；
///     3. body 用 `reqwest::Body::wrap_stream(request.into_body().into_data_stream())`
///        流式转发（不缓冲）；方法不变；无总超时（connect_timeout 3s 已在 client 上）；
///     4. 响应 status/headers 原样回传，body 用 `Body::from_stream(bytes_stream())`；
///     5. 连接目标失败 → 502 `relay_target_unreachable` 并顺带把 target 置 offline。
///     调用方（handler）已保证 enabled/白名单/目标解析/并发许可前置检查通过。
pub(crate) async fn forward_relay_request(
    state: &AppState,
    job: RelayForwardJob,
) -> axum::response::Response {
    let RelayForwardJob {
        permit,
        target,
        path,
        query,
        method,
        headers,
        body,
        context,
    } = job;
    let mut url = format!("{}{}", target.base_url(), path);
    if let Some(query) = query.as_deref() {
        url.push('?');
        url.push_str(query);
    }

    let mut outbound_headers = filter_forwarded_headers(&headers);
    // request_id 全链透传：入站 header 有值时为同一 ID；缺失时写入 B 侧生成的 ID，
    // 保证 C 的日志/错误信封与本请求可关联（启用 remote_client 预留的转发语义）。
    if let Ok(value) = axum::http::HeaderValue::from_str(&context.request_id) {
        outbound_headers.insert(REQUEST_ID_HEADER, value);
    }

    let outbound_body = reqwest::Body::wrap_stream(body.into_data_stream());
    let request = state
        .relay
        .client()
        .request(method, &url)
        .headers(outbound_headers)
        .body(outbound_body);

    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!("relay 转发连接目标失败 ({url}): {error}");
            mark_relay_target_offline(state, &target.id);
            return relay_error(
                format!("中转目标不可达: {error}"),
                "relay_target_unreachable",
                StatusCode::BAD_GATEWAY,
                true,
                &context,
            )
            .into_response();
        }
    };

    let status = upstream.status();
    let response_headers = filter_forwarded_headers(upstream.headers());
    // 许可绑定响应流：流式 body 传输期间保持占用，流结束/客户端断开时归还。
    let response_body = Body::from_stream(RelayPermitStream {
        inner: Box::pin(upstream.bytes_stream()),
        _permit: permit,
    });
    let mut response = axum::response::Response::builder().status(status);
    if let Some(map) = response.headers_mut() {
        *map = response_headers;
    }
    match response.body(response_body) {
        Ok(response) => response,
        Err(error) => relay_error(
            format!("构造中转响应失败: {error}"),
            "relay_target_unreachable",
            StatusCode::BAD_GATEWAY,
            true,
            &context,
        )
        .into_response(),
    }
}

/// 构造终端输入 WS 桥的出站握手请求（tungstenite client）。
///
/// Business Logic（为什么需要这个函数）:
///     终端输入走常驻 WS（避免逐键 HTTP 往返）；A 经 B 中转时 B 必须以 WS client
///     身份连 C 的 `/api/workbench/terminal-input-stream`。C 端
///     `expected_device_id_guard` 要求 header 值等于 C 自己，因此出站必须携带
///     `X-Cc-Partner-Expected-Device-Id = {C_device_id}`（A 侧语义原样透传）。
///
/// Code Logic（这个函数做什么）:
///     `ws://{host}:{port}/api/workbench/terminal-input-stream`（query 透传）转
///     tungstenite Request；写入入站协商的 `sec-websocket-protocol`（可能多个，
///     逐个 append）与 `X-Cc-Partner-Expected-Device-Id`；其余握手 header 由
///     tungstenite 生成（参照 `browser_proxy.rs` 的 remote_relay 桥模式）。
pub(crate) fn build_relay_terminal_upstream_request(
    target: &Device,
    device_id: &str,
    inbound_protocols: &[String],
    query: Option<&str>,
) -> Result<TungsteniteRequest, String> {
    let mut url = format!(
        "ws://{}:{}/api/workbench/terminal-input-stream",
        target.host, target.port
    );
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let mut request = url
        .into_client_request()
        .map_err(|error| format!("终端输入中转 WS URL 无效: {error}"))?;
    let headers = request.headers_mut();
    for protocol in inbound_protocols {
        let value = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(protocol)
            .map_err(|_| format!("终端输入子协议无法写入请求头: {protocol}"))?;
        headers.append(HeaderName::from_static("sec-websocket-protocol"), value);
    }
    let expected = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(device_id)
        .map_err(|_| "中转目标 device_id 无法写入请求头".to_string())?;
    headers.insert(EXPECTED_DEVICE_ID_HEADER.clone(), expected);
    Ok(request)
}

/// 双向桥接 A↔B 的 axum WebSocket 与 B↔C 的 tungstenite 上游连接。
///
/// Business Logic（为什么需要这个函数）:
///     终端输入帧（按键/粘贴/resize）与 ACK/错误帧必须双向实时透传；任一侧关闭或
///     出错都应双侧关闭（重连由 A 侧既有机制——`peer_link_for_device` 缓存 + 上层
///     退避——负责，B 不做重连）。与 `workbench/browser_proxy.rs` 的 remote_relay
///     桥同一模式（该处面向 dev server HMR；此处面向 C 的终端输入网关，桥接逻辑
///     独立实现，提取共享 helper 会大动 browser_proxy 的私有类型，收益不成比例）。
///
/// Code Logic（这个函数做什么）:
///     split 两侧流，`tokio::select!` 两个方向的循环转发；axum ↔ tungstenite 消息
///     按同名 variant 转换（Ping/Pong 保留，Close 只保留关闭语义，raw Frame 忽略）。
pub(crate) async fn bridge_relay_websocket(
    downstream: WebSocket,
    upstream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) {
    if let Err(error) = bridge_relay_websocket_inner(downstream, upstream).await {
        tracing::debug!("终端输入中转 WebSocket 桥结束: {error}");
    }
}

/// `bridge_relay_websocket` 的内部实现（返回 Err 便于打日志）。
///
/// Business Logic（为什么需要这个函数）:
///     桥接循环内任一方向失败都要带着原因退出，外层统一 debug 日志，不静默吞错。
///
/// Code Logic（这个函数做什么）:
///     两侧 split 后 select 双向循环；每方向首次 Err 即结束（select 天然双侧退出）。
async fn bridge_relay_websocket_inner(
    downstream: WebSocket,
    upstream: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
) -> Result<(), String> {
    use futures_util::SinkExt;
    use futures_util::StreamExt;
    let (mut upstream_write, mut upstream_read) = upstream.split();
    let (mut downstream_write, mut downstream_read) = downstream.split();

    let downstream_to_upstream = async {
        while let Some(message) = downstream_read.next().await {
            let message =
                message.map_err(|error| format!("读取下游中转 WebSocket 失败: {error}"))?;
            upstream_write
                .send(relay_axum_to_tungstenite_message(message))
                .await
                .map_err(|error| format!("写入上游终端输入流失败: {error}"))?;
        }
        Ok::<(), String>(())
    };
    let upstream_to_downstream = async {
        while let Some(message) = upstream_read.next().await {
            let message = message.map_err(|error| format!("读取上游终端输入流失败: {error}"))?;
            if let Some(message) = relay_tungstenite_to_axum_message(message) {
                downstream_write
                    .send(message)
                    .await
                    .map_err(|error| format!("写入下游中转 WebSocket 失败: {error}"))?;
            }
        }
        Ok::<(), String>(())
    };

    tokio::select! {
        result = downstream_to_upstream => result,
        result = upstream_to_downstream => result,
    }
}

/// axum WS 消息转 tungstenite 消息（下游 → 上游方向）。
///
/// Business Logic（为什么需要这个函数）:
///     桥接两端使用不同 WS 库的消息类型，业务上需保留文本（JSON 帧）、二进制与
///     心跳；Close 只保留关闭语义。
///
/// Code Logic（这个函数做什么）:
///     按同名 variant 转换（参照 `browser_proxy.rs` 同名私有 helper 的形态）。
fn relay_axum_to_tungstenite_message(message: AxumWsMessage) -> TungsteniteMessage {
    match message {
        AxumWsMessage::Text(text) => TungsteniteMessage::Text(text),
        AxumWsMessage::Binary(binary) => TungsteniteMessage::Binary(binary),
        AxumWsMessage::Ping(ping) => TungsteniteMessage::Ping(ping),
        AxumWsMessage::Pong(pong) => TungsteniteMessage::Pong(pong),
        AxumWsMessage::Close(_) => TungsteniteMessage::Close(None),
    }
}

/// tungstenite 消息转 axum WS 消息（上游 → 下游方向）。
///
/// Business Logic（为什么需要这个函数）:
///     C 端网关的 ACK/Error/ready 帧是 JSON 文本，必须原样回到 A；tungstenite 的
///     raw Frame 没有对应语义，忽略。
///
/// Code Logic（这个函数做什么）:
///     按同名 variant 转换；Frame 返回 None（跳过），Close 只保留关闭语义。
fn relay_tungstenite_to_axum_message(message: TungsteniteMessage) -> Option<AxumWsMessage> {
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

    /// Business Logic（为什么需要这个测试）:
    ///     白名单是中转拓扑的攻击面边界；`/api/sync|prompts|transfer|mobile|backend/control`
    ///     明确排除，health 精确、workbench/orchestrator 前缀放行，任何回归都会把
    ///     双向同步流量引入跳板链路。
    ///
    /// Code Logic（这个测试做什么）:
    ///     断言允许/拒绝路径样本的布尔结果。
    #[test]
    fn relay_path_whitelist_accepts_health_workbench_orchestrator_only() {
        // 允许：health 精确 + workbench/orchestrator 任意深度的前缀。
        assert!(is_relay_path_allowed("/api/health"));
        assert!(is_relay_path_allowed("/api/workbench/"));
        assert!(is_relay_path_allowed("/api/workbench/projects/open"));
        assert!(is_relay_path_allowed(
            "/api/workbench/terminal-input-stream"
        ));
        assert!(is_relay_path_allowed("/api/orchestrator/"));
        assert!(is_relay_path_allowed("/api/orchestrator/tasks/create"));

        // 拒绝：其余全部命名空间。
        assert!(!is_relay_path_allowed("/api/sync/pull"));
        assert!(!is_relay_path_allowed("/api/prompts"));
        assert!(!is_relay_path_allowed("/api/transfer/init"));
        assert!(!is_relay_path_allowed("/api/mobile/attention"));
        assert!(!is_relay_path_allowed("/api/backend/control/stop"));
        // 拒绝：近似但不等价的前缀（healthx / 前缀无尾斜杠）。
        assert!(!is_relay_path_allowed("/api/healthx"));
        assert!(!is_relay_path_allowed("/api/health/status"));
        assert!(!is_relay_path_allowed("/api/workbench"));
        assert!(!is_relay_path_allowed("/api/orchestrator"));
        assert!(!is_relay_path_allowed("/other"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     hop-by-hop header 透传会破坏出站连接（错误分帧/握手失败）；端到端业务
    ///     header（request_id / expected-device / content-type / chunk offset）必须保留。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造含清单内外的 HeaderMap，断言 filter_forwarded_headers 的输出集合。
    #[test]
    fn filter_forwarded_headers_strips_hop_by_hop_and_keeps_end_to_end() {
        let mut headers = HeaderMap::new();
        for name in [
            "host",
            "connection",
            "transfer-encoding",
            "content-length",
            "upgrade",
            "sec-websocket-key",
            "sec-websocket-version",
            "sec-websocket-protocol",
            "sec-websocket-accept",
            "sec-websocket-extensions",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "proxy-connection",
            "te",
            "trailer",
        ] {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                "strip-me".parse().unwrap(),
            );
        }
        headers.append(REQUEST_ID_HEADER, "req-1".parse().unwrap());
        headers.append(
            EXPECTED_DEVICE_ID_HEADER.clone(),
            "device-C".parse().unwrap(),
        );
        headers.append(
            HeaderName::from_static("content-type"),
            "application/json".parse().unwrap(),
        );
        headers.append(
            HeaderName::from_static("x-chunk-offset"),
            "960".parse().unwrap(),
        );

        let filtered = filter_forwarded_headers(&headers);
        assert_eq!(filtered.len(), 4, "应仅保留 4 个端到端 header");
        assert_eq!(filtered.get(REQUEST_ID_HEADER).unwrap(), "req-1");
        assert_eq!(
            filtered.get(&EXPECTED_DEVICE_ID_HEADER).unwrap(),
            "device-C"
        );
        assert_eq!(filtered.get("content-type").unwrap(), "application/json");
        assert_eq!(filtered.get("x-chunk-offset").unwrap(), "960");
        for name in [
            "host",
            "connection",
            "transfer-encoding",
            "content-length",
            "upgrade",
            "keep-alive",
            "te",
            "trailer",
        ] {
            assert!(
                filtered.get(name).is_none(),
                "hop-by-hop header {name} 应被剥除"
            );
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     全局 8 / per-target 4 的并发契约是防流量放大的硬上限；per-target 必须
    ///     lazy 创建且同 target 复用同一 semaphore，全局打满与单 target 打满都必须
    ///     立即拒绝（relay_busy 语义）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     同一 target 连续 try_acquire 4 次成功、第 5 次失败；guard 全部 drop 后
    ///     可再次获取；不同 target 互不影响。
    #[tokio::test]
    async fn relay_runtime_per_target_semaphore_enforces_limit() {
        let runtime = RelayRuntime::new();
        let mut guards = Vec::new();
        for _ in 0..RELAY_PER_TARGET_MAX_CONCURRENCY {
            guards.push(
                runtime
                    .try_acquire("device-C")
                    .expect("前 4 个 per-target 许可应成功"),
            );
        }
        assert_eq!(runtime.active_forwards(), 4);
        assert!(
            runtime.try_acquire("device-C").is_none(),
            "第 5 个 per-target 许可应失败（relay_busy）"
        );
        // 其它 target 不受该 target 占用影响（guard 需持有，避免临时值立即 drop）。
        let device_d_guard = runtime
            .try_acquire("device-D")
            .expect("不同 target 应不受 per-target 上限影响");
        drop(guards);
        assert_eq!(
            runtime.active_forwards(),
            1,
            "drop 后计数应只剩 device-D 的 1"
        );
        drop(device_d_guard);
        assert_eq!(runtime.active_forwards(), 0);
        assert!(
            runtime.try_acquire("device-C").is_some(),
            "归还后应可重新获取"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     全局 8 是跳板机总资源上限，不能被多 target 组合绕过（8 个不同 target 各占 1
    ///     也应打满全局）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     8 个不同 device_id 各 try_acquire 一次全部成功，第 9 个（新 target）失败。
    #[tokio::test]
    async fn relay_runtime_global_semaphore_enforces_limit() {
        let runtime = RelayRuntime::new();
        let guards: Vec<_> = (0..RELAY_GLOBAL_MAX_CONCURRENCY)
            .map(|i| {
                runtime
                    .try_acquire(&format!("device-{i}"))
                    .expect("全局额度内应全部成功")
            })
            .collect();
        assert_eq!(runtime.active_forwards(), RELAY_GLOBAL_MAX_CONCURRENCY);
        assert!(
            runtime.try_acquire("device-overflow").is_none(),
            "第 9 个全局许可应失败（relay_busy）"
        );
        drop(guards);
        assert_eq!(runtime.active_forwards(), 0);
    }
}
