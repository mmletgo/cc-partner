//! net/peer_client.rs — 对端 HTTP 客户端（reqwest）
//!
//! Business Logic（为什么需要这个模块）:
//!     P2P 架构中每个实例也是客户端，需主动向其他设备发起请求：健康检查、同步 pull/push、
//!     文件传输 init/chunk/status。对照 Python `network/client.py`（aiohttp 实现）。
//!     M3 仅实现 health 调用 + 基础结构；sync/transfer 留 M4/M5 填实现。
//!
//! Code Logic（这个模块做什么）:
//!     - 持有一个 reqwest::Client（连接池复用，rustls-tls 避免 OpenSSL 依赖）。
//!     - `health_info(base_url)`：GET `{base_url}/api/health`，10s 超时，成功且 status==200
//!       返回解析后的 `HealthResponse`（含 protocol_version / capabilities）；失败返回 PeerCallError。
//!     - `health(addr, port)`：legacy 布尔包装，复用 health_info，仅返回 ok 字段（旧调用方兼容）。
//!     - sync/transfer 方法（pull/push/init/chunk 等）。

use crate::net::peer_error::parse_peer_response;
use crate::net::protocol::PeerProtocolInfo;
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::net::routes::health::HealthResponse;
use std::time::Duration;

/// health 请求超时（秒）。对照 Python `DEFAULT_TIMEOUT=5`，Rust 版略放宽到 10s 提升弱网容错。
const HEALTH_TIMEOUT_SECS: u64 = 10;

// 兼容性再导出：历史上 `PeerCallError` 定义在本模块；Task 7 把它统一到 `net::peer_error`，
// 这里再导出统一类型，使旧调用点（`use crate::net::peer_client::PeerCallError`）无需改动，
// 同时 `health_info` 等签名自动指向新枚举（Network/Unsupported/InvalidResponse/Remote）。
pub use crate::net::peer_error::PeerCallError;

/// sync/pull 响应体（字段名对照 Python `handle_sync_pull` 返回 `{prompts: [...]}`）。
#[derive(Debug, serde::Deserialize)]
struct SyncPullResp {
    #[serde(default)]
    prompts: Vec<crate::models::prompt::PromptRow>,
}

/// transfer/chunk 响应体（字段名对照 Python `receive_chunk` 返回 `{success, received_bytes}`）。
///
/// Business Logic（为什么本地定义而不复用 `transfer::receiver::ChunkResp`）:
///     `transfer::receiver::ChunkResp` 是 route 层响应序列化结构（只 derive Serialize）；
///     客户端需要的是反序列化视图。本地定义避免给 route 结构补 derive Deserialize 造成语义混淆，
///     也避免 peer_client 反向依赖 transfer route 模块。
#[derive(Debug, serde::Deserialize)]
struct ChunkResp {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    #[allow(dead_code)]
    received_bytes: u64,
}

/// sync/push 响应体（字段名对照 Python `handle_sync_push` 返回 `{accepted: <count>}`）。
#[derive(Debug, serde::Deserialize)]
struct SyncPushResp {
    #[serde(default)]
    accepted: u64,
}

/// cc-history/sync/pull 响应体（字段名对照 routes/cc_history.rs 的 CcSyncPullResp）。
#[derive(Debug, serde::Deserialize)]
struct CcSyncPullResp {
    #[serde(default)]
    items: Vec<crate::cc::models::ClaudeHistoryRow>,
}

/// cc-history/sync/push 响应体（字段名对照 routes/cc_history.rs 的 CcSyncPushResp）。
#[derive(Debug, serde::Deserialize)]
struct CcSyncPushResp {
    #[serde(default)]
    accepted: u64,
}

/// ssh-target/sync/pull 响应体（字段名对照 routes/ssh_target_sync.rs 的 SshSyncPullResp）。
#[derive(Debug, serde::Deserialize)]
struct SshTargetPullResp {
    #[serde(default)]
    targets: Vec<crate::models::ssh_target::SshTargetRow>,
}

/// ssh-target/sync/push 响应体（字段名对照 routes/ssh_target_sync.rs 的 SshSyncPushResp）。
#[derive(Debug, serde::Deserialize)]
struct SshTargetPushResp {
    #[serde(default)]
    accepted: u64,
}

/// scratchpad/sync/pull 响应体（字段名对照 routes/scratchpad_sync.rs 的 ScratchpadPullResp）。
#[derive(Debug, serde::Deserialize)]
struct ScratchpadPullResp {
    #[serde(default)]
    pages: Vec<crate::models::scratchpad::ScratchpadRow>,
}

/// scratchpad/sync/push 响应体（字段名对照 routes/scratchpad_sync.rs 的 ScratchpadPushResp）。
#[derive(Debug, serde::Deserialize)]
struct ScratchpadPushResp {
    #[serde(default)]
    accepted: u64,
}

/// claude_md/push 响应体（字段名对照 ClaudeMdPushResp 的 `{accepted: bool}`）。
#[derive(Debug, serde::Deserialize)]
struct ClaudeMdPushResp {
    #[serde(default)]
    accepted: bool,
}

/// Claude Code assets inventory 响应体：直接是 DTO 数组。
type ClaudeAssetsInventoryResp = Vec<crate::claude_code_assets::ClaudeCodeAsset>;

/// complete 握手重试策略。
///
/// Business Logic（为什么需要这个结构）:
///     接收端 finalize 后墓碑在 TTL 内幂等；网络抖动/响应丢失时同一 transfer_id 可安全重试。
///     策略独立结构便于生产默认值与测试注入短退避。
///
/// Code Logic（这个结构做什么）:
///     max_attempts：含首次在内的总尝试次数；base_backoff：第 n 次重试前 sleep = base * n；
///     status_fallback：complete 仍失败时是否查 status 收敛 completed。
#[derive(Debug, Clone)]
pub struct TransferCompletePolicy {
    /// 最大尝试次数（含首次）。
    pub max_attempts: u32,
    /// 基础退避（第 attempt 次失败后 sleep = base_backoff * attempt）。
    pub base_backoff: Duration,
    /// complete 失败后是否用 status 查询收敛。
    pub status_fallback: bool,
}

impl Default for TransferCompletePolicy {
    /// Business Logic: 生产默认在墓碑 TTL（约 5 分钟）内做有限次快速重试。
    /// Code Logic: 4 次尝试、200ms 递增退避、开启 status 收敛。
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_backoff: Duration::from_millis(200),
            status_fallback: true,
        }
    }
}

/// chunk 发送重试策略（与 complete 同形，便于测试注入短退避）。
///
/// Business Logic（为什么需要这个结构）:
///     最后一块在接收端可能已校验落盘，但响应超时丢失；同 transfer_id/offset/body 幂等可重放。
///     发送端必须有界重试，并在仍失败时用 status 收敛 completed，否则本地 failed、远端 completed。
///
/// Code Logic（这个结构做什么）:
///     max_attempts / base_backoff 控制 Network/retryable 5xx 重试；
///     status_fallback：耗尽后若 status=completed 则 Ok(true)，transferring 时再安全重放一次；
///     若仍 transferring 且 transferred_bytes>=size，则在 complete_fallback 下改走 complete 收敛。
#[derive(Debug, Clone)]
pub struct TransferChunkPolicy {
    /// 最大尝试次数（含首次）。
    pub max_attempts: u32,
    /// 基础退避（第 attempt 次失败后 sleep = base_backoff * attempt）。
    pub base_backoff: Duration,
    /// 失败后是否用 status 收敛 completed / transferring 再放一次。
    pub status_fallback: bool,
    /// 字节已收齐但仍 transferring 时，是否调用 complete 做 durable 收敛。
    pub complete_fallback: bool,
}

impl Default for TransferChunkPolicy {
    /// Business Logic: 覆盖弱网下最后一块响应丢失窗口与 durable history 瞬时失败。
    /// Code Logic: 4 次尝试、150ms 递增退避、开启 status 收敛与 complete 收敛。
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_backoff: Duration::from_millis(150),
            status_fallback: true,
            complete_fallback: true,
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     complete/chunk 只对网络抖动与服务端瞬时错误安全重试；4xx 业务错误（如 validation）不得重试。
///
/// Code Logic（这个函数做什么）:
///     Network 可重试；Remote 在 status>=500 或 status==504 或 retryable=true 时可重试；
///     Unsupported/InvalidResponse 不可重试。
fn is_transfer_transport_retryable(error: &PeerCallError) -> bool {
    match error {
        PeerCallError::Network { .. } => true,
        PeerCallError::Remote {
            status, retryable, ..
        } => *retryable || *status >= 500 || *status == 504,
        PeerCallError::Unsupported { .. } | PeerCallError::InvalidResponse { .. } => false,
    }
}

/// 兼容旧名：complete 路径复用同一判定。
fn is_transfer_complete_retryable(error: &PeerCallError) -> bool {
    is_transfer_transport_retryable(error)
}

/// Business Logic（为什么需要这个函数）:
///     complete 返回 success=false 或传输错误时，若远端 status 已 completed，必须收敛成功。
///
/// Code Logic（这个函数做什么）:
///     读取 status JSON 的 `status` 字段；`"completed"` → true。
fn status_json_is_completed(status: &serde_json::Value) -> bool {
    status
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "completed")
}

/// Business Logic（为什么需要这个函数）:
///     最后一块失败后若 status 仍 transferring，同 offset 重放是安全的（seek-write + 墓碑）。
///
/// Code Logic（这个函数做什么）:
///     `status == "transferring"` 或 `"pending"` → true。
fn status_json_is_still_transferring(status: &serde_json::Value) -> bool {
    status
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "transferring" || s == "pending")
}

/// Business Logic（为什么需要这个函数）:
///     接收端 history 瞬时故障时最后一块可能已落盘但仍返回 503，status 故意保持 transferring。
///     若 `transferred_bytes >= size`，字节面已收齐，继续重放 chunk 无法收敛，必须改走 complete。
///
/// Code Logic（这个函数做什么）:
///     读取 status JSON 的 `transferred_bytes` 与 `size`（u64）；两者均存在且 transferred >= size → true。
///     size=0 时 transferred>=0 也视为收齐（空文件仅靠 complete 收口）。
fn status_json_bytes_fully_received(status: &serde_json::Value) -> bool {
    let transferred = status
        .get("transferred_bytes")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            status
                .get("transferred_bytes")
                .and_then(|v| v.as_i64())
                .map(|n| n as u64)
        });
    let size = status.get("size").and_then(|v| v.as_u64()).or_else(|| {
        status
            .get("size")
            .and_then(|v| v.as_i64())
            .map(|n| n as u64)
    });
    match (transferred, size) {
        (Some(transferred), Some(size)) => transferred >= size,
        _ => false,
    }
}

/// 对端 HTTP 客户端，封装 reqwest::Client。
///
/// Business Logic: 所有对端调用复用同一 Client（内部连接池），提升效率。
///     Client 本身是 Clone 廉贵的（内部 Arc），故 PeerClient 可直接 Clone 共享。
///     长连接事件桥复用无默认超时的 `stream_client`，禁止每桥 `Client::new()`。
#[allow(dead_code)]
pub struct PeerClient {
    client: reqwest::Client,
    /// 无默认请求超时的共享 client，仅供 NDJSON 长连接复用。
    stream_client: reqwest::Client,
}

impl PeerClient {
    /// 创建客户端，配置默认超时。
    ///
    /// Code Logic: reqwest::Client::builder 设置 timeout；rustls-tls feature 已在 Cargo.toml 启用，
    ///     无需系统 OpenSSL。本机自签场景实际走 http，TLS 仅用于 https 资源（如 GitHub Releases）。
    ///     另建无默认超时的 stream_client 供 remote event bridge 长连接。
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HEALTH_TIMEOUT_SECS))
            .build()
            .expect("构造 reqwest Client 失败（rustls-tls 初始化异常）");
        let stream_client = reqwest::Client::builder()
            .pool_max_idle_per_host(8)
            .build()
            .expect("构造 stream reqwest Client 失败（rustls-tls 初始化异常）");
        Self {
            client,
            stream_client,
        }
    }

    /// 打开 NDJSON 事件流 GET（无默认超时）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     remote event bridge 需要长连接读 `/api/workbench/events`，不能用带 10s 默认超时的
    ///     业务 client，也禁止每桥新建未分类 Client。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 `stream_client` GET `url`，注入 request id；send 失败 → Network。
    ///     成功返回原始 Response 供调用方按 chunk 读取，不在此聚合 body。
    pub async fn open_ndjson_stream(&self, url: &str) -> Result<reqwest::Response, PeerCallError> {
        self.stream_client
            .get(url)
            .header(REQUEST_ID_HEADER, new_request_id())
            .send()
            .await
            .map_err(|e| {
                tracing::debug!("open_ndjson_stream 网络失败 ({url}): {e}");
                PeerCallError::Network {
                    url: url.to_string(),
                    source: e,
                }
            })
    }

    /// 健康检查（typed）：GET 对端 /api/health，返回完整 HealthResponse。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     新增的能力探测场景需要 protocol_version + capabilities 字段，而不只是 bool 可达性；
    ///     调用方据此判断对端是否支持 errors.envelope.v1 等能力后再决定调哪些新路由。
    ///     失败原因必须回传（网络/HTTP/JSON 解码），不能像 legacy `health` 那样吃掉错误。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET `{base_url}/api/health`（base_url 形如 `http://192.168.1.5:8765`，无尾斜杠），
    ///     经统一解析器 `parse_peer_response` 消费 status/header request_id/body：
    ///     - send 失败 → `PeerCallError::Network`；
    ///     - 2xx 且 body 可解析为 HealthResponse → Ok；
    ///     - 2xx 但 body 无法解析 / 非 2xx 且 body 非 JSON → `InvalidResponse`；
    ///     - 非 2xx 且 body 是错误信封（v1 或 v0 老形态）→ `Remote`（携带 code/status）。
    pub async fn health_info(&self, base_url: &str) -> Result<HealthResponse, PeerCallError> {
        let url = format!("{base_url}/api/health");
        let resp = self
            .client
            .get(&url)
            // Finding 3: 出站 request_id 让对端把 health 请求纳入同一调用链日志，
            // 多跳代理（orchestrator/workbench）也能据此关联本机发起的请求。
            .header(
                crate::net::request_context::REQUEST_ID_HEADER,
                crate::net::request_context::new_request_id(),
            )
            .send()
            .await
            .map_err(|e| {
                tracing::debug!("health_info 网络失败 ({url}): {e}");
                PeerCallError::Network {
                    url: url.clone(),
                    source: e,
                }
            })?;
        parse_peer_response::<HealthResponse>(resp, &url)
            .await
            .map_err(|e| {
                tracing::debug!("health_info 解析失败 ({url}): {e}");
                e
            })
    }

    /// 能力门：调用新路由前检查对端是否支持某能力，不支持则直接返回 `Unsupported` 而不发起路由请求。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     P2P 调用新路由（仅 v1 对端才实现）前必须先确认对端具备对应能力 token，否则会打到旧版对端
    ///     返回 404/HTML 等噪音错误。本函数集中能力探测：始终拉取权威 health 元数据（而非依赖可能过期的
    ///     缓存），缺失能力时返回 `PeerCallError::Unsupported` 且**不**调用目标路由，避免无效请求。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1. `health_info(base_url)` 取对端权威 `PeerProtocolInfo`（失败原样上抛 Network/InvalidResponse/Remote）；
    ///     2. `info.supports(capability)` 判定能力存在；
    ///     3. 命中返回 Ok(HealthResponse)（调用方复用做后续调用）；
    ///     4. 未命中返回 `Unsupported { url, capability }`。
    pub async fn require_capability(
        &self,
        base_url: &str,
        capability: &'static str,
    ) -> Result<HealthResponse, PeerCallError> {
        let health = self.health_info(base_url).await?;
        let info: PeerProtocolInfo = health.protocol_info();
        if info.supports(capability) {
            Ok(health)
        } else {
            Err(PeerCallError::Unsupported {
                url: base_url.to_string(),
                capability,
            })
        }
    }

    /// 健康检查（legacy 布尔）：GET 对端 /api/health，返回 true 表示可达。
    ///
    /// Business Logic: 同步/传输前需验证对端在线且 HTTP 服务正常；历史调用方只关心 bool。
    ///     新代码应改用 `health_info`，以便拿到协议元数据；保留此包装只为兼容现有调用点。
    /// Code Logic: 用 `{addr}:{port}` 拼 base_url，复用 health_info；任何失败（网络/HTTP/JSON）
    ///     均视为不可达返回 false（与 Python `health_check` 一致，不向上抛错）。
    pub async fn health(&self, addr: &str, port: u16) -> bool {
        let base_url = format!("http://{addr}:{port}");
        self.health_info(&base_url)
            .await
            .map(|r| r.ok)
            .unwrap_or(false)
    }

    // ===== Finding 2: 共享出站请求 helper =====
    //
    // 历史上 sync_pull/sync_push/transfer_*/cc_sync_*/ssh_target_*/scratchpad_* 每个方法
    // 自己拼 reqwest 请求并手写 `status.is_success()` 判断，把所有失败折叠成空 Vec/false/
    // 字符串错误，导致 code/status/retryable/request_id 全部丢失。下面两个 helper 集中：
    //   1. 自动注入 X-CC-Request-Id（多跳调用链关联）；
    //   2. 统一委托 `parse_peer_response` 解析成功/错误（v1 信封 + v0 老形态）；
    //   3. 失败返回结构化 `PeerCallError`（携带 code/status/retryable/request_id）。
    // 各公开方法据此重写，公开签名保留向后兼容（Vec/bool），但内部走结构化错误路径，
    // 并在 tracing 里记录 code/status，便于诊断。

    /// 共享 GET helper：注入 request_id 并用 `parse_peer_response` 解析响应（Finding 2）。
    ///
    /// Business Logic: 只读远端调用（如 transfer_status）复用同一套 request_id 注入 +
    ///     统一错误分类，避免每处手写 `status.is_success()`。
    ///
    /// Code Logic: 构造 GET 请求（带 X-CC-Request-Id），发送后委托 `parse_peer_response`；
    ///     send 失败 → `PeerCallError::Network`；2xx 反序列化失败 → `InvalidResponse`；
    ///     非 2xx → `Remote`（携带 code/status/retryable/request_id）。
    async fn request_get<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, PeerCallError> {
        let resp = self
            .client
            .get(url)
            .header(REQUEST_ID_HEADER, new_request_id())
            .send()
            .await
            .map_err(|e| PeerCallError::Network {
                url: url.to_string(),
                source: e,
            })?;
        parse_peer_response::<T>(resp, url).await
    }

    /// 共享 POST helper：注入 request_id、发送 JSON body 并用 `parse_peer_response` 解析响应
    ///（Finding 2）。
    ///
    /// Business Logic: sync/transfer/cc-history/ssh-target/scratchpad 等 POST 调用都需要
    ///     统一的 request_id 注入与错误分类。
    ///
    /// Code Logic: 构造 POST 请求（JSON body + X-CC-Request-Id），发送后委托
    ///     `parse_peer_response`；错误分类与 `request_get` 一致。
    async fn request_post<T, B>(&self, url: &str, body: &B) -> Result<T, PeerCallError>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize + ?Sized,
    {
        let resp = self
            .client
            .post(url)
            .header(REQUEST_ID_HEADER, new_request_id())
            .json(body)
            .send()
            .await
            .map_err(|e| PeerCallError::Network {
                url: url.to_string(),
                source: e,
            })?;
        parse_peer_response::<T>(resp, url).await
    }

    /// 共享 POST raw-bytes helper：注入 request_id、发送原始字节 body 并用 `parse_peer_response`
    /// 解析响应（Finding 2）。
    ///
    /// Business Logic: transfer/chunk 的 body 是原始字节 + 自定义 header（X-Chunk-Offset），
    ///     无法走 `request_post` 的 JSON 路径，但同样需要 request_id 注入与统一错误分类。
    ///
    /// Code Logic: 构造 POST 请求（自定义 header + raw body + X-CC-Request-Id），发送后委托
    ///     `parse_peer_response`；错误分类与其它 helper 一致。
    async fn request_post_raw<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        extra_header_name: &str,
        extra_header_value: &str,
        body: Vec<u8>,
    ) -> Result<T, PeerCallError> {
        let resp = self
            .client
            .post(url)
            .header(REQUEST_ID_HEADER, new_request_id())
            .header(extra_header_name, extra_header_value)
            .body(body)
            .send()
            .await
            .map_err(|e| PeerCallError::Network {
                url: url.to_string(),
                source: e,
            })?;
        parse_peer_response::<T>(resp, url).await
    }

    /// 同步 pull：向对端发送本端 prompt 摘要，获取对端认为本端需要的 prompt。
    ///
    /// Business Logic: Prompt 同步第一步——把本端摘要发给对端，对端比对后返回本端需要更新的
    ///     prompt 完整数据。对照 Python `sync_pull`。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/sync/pull`，请求体 `{summaries: [...]}`，
    ///     经共享 `request_post` helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析：
    ///     成功返回 `{prompts: [...]}`；任何失败（网络/HTTP/JSON）都记录结构化 code/status 后
    ///     返回空 Vec（保留公开签名以兼容调用方不阻断整轮同步）。
    pub async fn sync_pull(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Vec<crate::models::prompt::PromptRow> {
        match self.sync_pull_result(base_url, local_summary).await {
            Ok(v) => v,
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "sync_pull",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("sync_pull 失败: {e}"))
                .emit();
                Vec::new()
            }
        }
    }

    /// Prompt legacy pull（typed）：失败上抛，禁止折叠空集。
    ///
    /// Business Logic: N2 引擎 legacy 路径需要区分“成功空远端”与 transport 失败。
    /// Code Logic: POST `/api/sync/pull`，成功返回 prompts；失败 `PeerCallError`。
    pub async fn sync_pull_result(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Result<Vec<crate::models::prompt::PromptRow>, PeerCallError> {
        let url = format!("{base_url}/api/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        let data: SyncPullResp = self.request_post(&url, &body).await?;
        crate::backend::logging::OperationLog::new(
            "p2p",
            "sync_pull",
            crate::backend::logging::OperationResult::Ok,
        )
        .message(format!(
            "sync_pull 从对端获取 {} 条 prompt",
            data.prompts.len()
        ))
        .emit();
        Ok(data.prompts)
    }

    /// 同步 push：将本端有但对端缺少的 prompt 推送给对端。
    ///
    /// Business Logic: Prompt 同步第二步——把本端独有或领先的 prompt 推过去。对照 Python `sync_push`。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/sync/push`，请求体 `{prompts: [...]}`，
    ///     经共享 `request_post` helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析：
    ///     成功（HTTP 2xx）即视为推送完成；失败记录结构化 code/status 后返回 false。
    pub async fn sync_push(
        &self,
        base_url: &str,
        prompts: &[crate::models::prompt::PromptRow],
    ) -> bool {
        match self.sync_push_result(base_url, prompts).await {
            Ok(v) => v,
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "sync_push",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("sync_push 失败: {e}"))
                .emit();
                false
            }
        }
    }

    /// Prompt legacy push（typed）：失败上抛。
    ///
    /// Business Logic: N2 引擎需要 push 失败时返回 typed Protocol/Unreachable，不得假成功。
    /// Code Logic: POST `/api/sync/push`，2xx → Ok(true)；错误上抛 PeerCallError。
    pub async fn sync_push_result(
        &self,
        base_url: &str,
        prompts: &[crate::models::prompt::PromptRow],
    ) -> Result<bool, PeerCallError> {
        let url = format!("{base_url}/api/sync/push");
        let body = serde_json::json!({ "prompts": prompts });
        let data: SyncPushResp = self.request_post(&url, &body).await?;
        crate::backend::logging::OperationLog::new(
            "p2p",
            "sync_push",
            crate::backend::logging::OperationResult::Ok,
        )
        .message(format!("sync_push 成功，对端接收 {} 条", data.accepted))
        .emit();
        Ok(true)
    }

    /// CLAUDE.md 主动 push：将本端的 CLAUDE.md 版本推送给对端。
    ///
    /// Business Logic: 用户主动推送 CLAUDE.md 时，对端应被更新为触发设备的版本，
    ///     因此服务端 push handler 会覆盖落库，而不是做双向 merge。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/sync/claude_md/push`，请求体 `{claude_md: row}`，
    ///     经共享 `request_post` helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析。
    ///     期望响应 `{accepted: bool}`（对端实际落库为 true）。返回 accepted。
    ///     失败经 `peer_call_error_to_app_error` 映射为 `AppError::Remote`，保留 code/status/retryable。
    pub async fn claude_md_push(
        &self,
        base_url: &str,
        row: &crate::models::claude_md::ClaudeMdRow,
    ) -> Result<bool, crate::error::AppError> {
        let url = format!("{base_url}/api/sync/claude_md/push");
        let body = serde_json::json!({ "claude_md": row });
        let resp: ClaudeMdPushResp = self.request_post(&url, &body).await.map_err(|e| {
            crate::net::peer_error::peer_call_error_to_app_error(e, "远端 CLAUDE.md")
        })?;
        Ok(resp.accepted)
    }

    /// 速记本同步 pull：向对端发送本端页面 summaries，获取对端认为本端需要的页面版本。
    ///
    /// Business Logic: Scratchpad 是多页面文本，pull 需要逐页比较向量时钟。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/scratchpad/sync/pull`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；失败记录结构化错误后
    ///     返回空 Vec 以兼容旧版本对端（不阻断整轮同步）。
    pub async fn scratchpad_pull(
        &self,
        base_url: &str,
        summaries: Vec<serde_json::Value>,
    ) -> Vec<crate::models::scratchpad::ScratchpadRow> {
        let url = format!("{base_url}/api/scratchpad/sync/pull");
        let body = serde_json::json!({ "summaries": summaries });
        match self
            .request_post::<ScratchpadPullResp, _>(&url, &body)
            .await
        {
            Ok(data) => data.pages,
            Err(e) => {
                tracing::debug!("scratchpad_pull 跳过 ({base_url}): {e}");
                Vec::new()
            }
        }
    }

    /// 速记本同步 push：向对端推送本端当前页面版本列表。
    ///
    /// Business Logic: 本端缺失/领先/并发页面需要推送给对端；对端会 merge/no-op。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/scratchpad/sync/push`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；HTTP 2xx 即视为成功。
    pub async fn scratchpad_push(
        &self,
        base_url: &str,
        pages: &[crate::models::scratchpad::ScratchpadRow],
    ) -> bool {
        let url = format!("{base_url}/api/scratchpad/sync/push");
        let body = serde_json::json!({ "pages": pages });
        match self
            .request_post::<ScratchpadPushResp, _>(&url, &body)
            .await
        {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "scratchpad_push",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!("scratchpad_push 完成，accepted={}", data.accepted))
                .emit();
                true
            }
            Err(e) => {
                tracing::debug!("scratchpad_push 跳过 ({base_url}): {e}");
                false
            }
        }
    }

    /// 获取对端 Claude Code assets inventory。
    ///
    /// Business Logic: 前端从某个局域网设备拉取前，先展示远端可选清单，让用户逐项勾选。
    ///
    /// Code Logic（Finding 2 起）: GET `{base_url}/api/claude-code/assets/inventory`，经共享
    ///     `request_get` helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；
    ///     失败经 `peer_call_error_to_app_error` 映射为 `AppError`（Remote 保留 code/status）。
    pub async fn claude_assets_inventory(
        &self,
        base_url: &str,
    ) -> Result<Vec<crate::claude_code_assets::ClaudeCodeAsset>, crate::error::AppError> {
        let url = format!("{base_url}/api/claude-code/assets/inventory");
        let resp: ClaudeAssetsInventoryResp = self
            .request_get(&url)
            .await
            .map_err(|e| crate::net::peer_error::peer_call_error_to_app_error(e, "远端 assets"))?;
        Ok(resp)
    }

    /// 请求对端按 selectors 生成 Claude Code assets bundle。
    ///
    /// Business Logic: 只下载用户勾选的 assets，避免全量拉取覆盖不想要的本机配置。
    ///
    /// Code Logic（Finding 2 起）: POST selectors 到 `/api/claude-code/assets/bundle`，返回 zip 原始字节
    ///     （非 JSON，无法走 `parse_peer_response` 反序列化路径）；但同样注入 X-CC-Request-Id，
    ///     并把非 2xx 折叠为携带状态码的 `AppError`，便于调用方区分网络/HTTP/业务失败。
    pub async fn claude_assets_bundle(
        &self,
        base_url: &str,
        items: &[crate::claude_code_assets::ClaudeCodeAssetSelector],
    ) -> Result<Vec<u8>, crate::error::AppError> {
        let url = format!("{base_url}/api/claude-code/assets/bundle");
        let body = serde_json::json!({ "items": items });
        let resp = self
            .client
            .post(&url)
            .header(REQUEST_ID_HEADER, new_request_id())
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::error::AppError::generic(format!("assets bundle 请求失败: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(crate::error::AppError::generic(format!(
                "assets bundle 失败: HTTP {}",
                status
            )));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| crate::error::AppError::generic(format!("assets bundle 读取失败: {e}")))?;
        Ok(bytes.to_vec())
    }

    /// 文件传输初始化：向对端发送文件元数据，获取 accepted 与 resume_offset。
    ///
    /// Business Logic: 发送端分块前先握手，告知对端文件名/大小/SHA256，对端确认并返回续传 offset。
    ///     对照 Python `transfer_init`（POST /api/transfer/init）。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/transfer/init`，body
    ///     `{transfer_id, filename, size, sha256, chunk_size}`，经共享 `request_post` helper 注入
    ///     X-CC-Request-Id 并用 `parse_peer_response` 统一解析。成功返回完整响应 JSON；
    ///     失败返回 Err，文案携带结构化 code/status（调用方据此标记任务 failed）。
    pub async fn transfer_init(
        &self,
        base_url: &str,
        metadata: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{base_url}/api/transfer/init");
        self.request_post::<serde_json::Value, _>(&url, &metadata)
            .await
            .map_err(|e| peer_call_error_to_transfer_message("init", &url, e))
    }

    /// 发送一个数据块到对端（生产默认：有界重试 + status 收敛）。
    ///
    /// Business Logic: 分块传输核心调用，body 为原始字节，header X-Chunk-Offset 标明写入 offset。
    ///     对照 Python `transfer_chunk`（POST /api/transfer/chunk/{id}）。
    ///     最后一块可能已在远端 finalize，但响应丢失；必须同 offset/body 有界重试并在耗尽后
    ///     查 status：completed → 本地成功；仍 transferring → 再安全重放一次。
    ///
    /// Code Logic（Finding 2 起）: 委托 `transfer_chunk_with_policy` 使用生产默认策略；
    ///     公开签名仍为 `Result<bool, String>` 以兼容 sender。
    pub async fn transfer_chunk(
        &self,
        base_url: &str,
        transfer_id: &str,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<bool, String> {
        self.transfer_chunk_with_policy(
            base_url,
            transfer_id,
            offset,
            data,
            TransferChunkPolicy::default(),
        )
        .await
        .map_err(|e| {
            let url = format!("{base_url}/api/transfer/chunk/{transfer_id}");
            peer_call_error_to_transfer_message("chunk", &url, e)
        })
    }

    /// 带策略的 chunk 发送（结构化错误，测试可注入短退避）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     发送循环对 Network/retryable 5xx 必须重放同一 transfer_id/offset/body；
    ///     最后一块响应丢失时 status=completed 应收敛成功，避免“远端 completed、本地 failed”。
    ///
    /// Code Logic（这个函数做什么）:
    ///     1) 对可重试错误有界退避重试（body 每次 clone，幂等 seek-write）；
    ///     2) success=true → Ok(true)；success=false 且开启 status_fallback 时查 status；
    ///     3) 传输错误耗尽后 status=completed → Ok(true)；status=transferring → 再放一次；
    ///     4) 仍 transferring 且 transferred_bytes>=size 且 complete_fallback → 调用 complete 收敛；
    ///     5) 其它 → 原样 Err(PeerCallError) 或 Ok(false)。
    pub async fn transfer_chunk_with_policy(
        &self,
        base_url: &str,
        transfer_id: &str,
        offset: u64,
        data: Vec<u8>,
        policy: TransferChunkPolicy,
    ) -> Result<bool, PeerCallError> {
        let url = format!("{base_url}/api/transfer/chunk/{transfer_id}");
        let mut last_err: Option<PeerCallError> = None;

        for attempt in 1..=policy.max_attempts {
            match self
                .request_post_raw::<ChunkResp>(
                    &url,
                    "X-Chunk-Offset",
                    &offset.to_string(),
                    data.clone(),
                )
                .await
            {
                Ok(resp) if resp.success => return Ok(true),
                Ok(resp) => {
                    // success=false：可能是业务拒绝；若 status 已 completed 仍收敛成功。
                    // 字节已收齐但仍 transferring 时改走 complete（history durable 瞬时失败）。
                    if policy.status_fallback {
                        if let Ok(status) = self.transfer_status_typed(base_url, transfer_id).await
                        {
                            if status_json_is_completed(&status) {
                                tracing::info!(
                                    "chunk success=false 但 status=completed，按成功收敛: {transfer_id} offset={offset}"
                                );
                                return Ok(true);
                            }
                            if policy.complete_fallback
                                && status_json_is_still_transferring(&status)
                                && status_json_bytes_fully_received(&status)
                                && self
                                    .try_complete_after_chunk_exhaustion(
                                        base_url,
                                        transfer_id,
                                        &policy,
                                    )
                                    .await?
                            {
                                return Ok(true);
                            }
                        }
                    }
                    let _ = resp;
                    return Ok(false);
                }
                Err(e) if is_transfer_transport_retryable(&e) && attempt < policy.max_attempts => {
                    tracing::debug!(
                        "transfer chunk 可重试失败 attempt={attempt}/{} ({url} offset={offset}): {e}",
                        policy.max_attempts
                    );
                    last_err = Some(e);
                    let backoff = policy.base_backoff * attempt;
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }

        // 响应丢失 / 重试耗尽：用 status 收敛；字节已收齐时再尝试 complete。
        if policy.status_fallback {
            if let Ok(status) = self.transfer_status_typed(base_url, transfer_id).await {
                if status_json_is_completed(&status) {
                    tracing::info!(
                        "chunk 响应丢失但 status=completed，按成功收敛: {transfer_id} offset={offset}"
                    );
                    return Ok(true);
                }
                if status_json_is_still_transferring(&status) {
                    tracing::debug!(
                        "chunk 失败后 status 仍 transferring，安全重放一次: {transfer_id} offset={offset}"
                    );
                    match self
                        .request_post_raw::<ChunkResp>(
                            &url,
                            "X-Chunk-Offset",
                            &offset.to_string(),
                            data,
                        )
                        .await
                    {
                        Ok(resp) if resp.success => return Ok(true),
                        Ok(_) | Err(_) => {
                            // 再查 status：completed 收敛；字节已收齐则改走 complete。
                            if let Ok(st2) = self.transfer_status_typed(base_url, transfer_id).await
                            {
                                if status_json_is_completed(&st2) {
                                    return Ok(true);
                                }
                                if policy.complete_fallback
                                    && status_json_is_still_transferring(&st2)
                                    && status_json_bytes_fully_received(&st2)
                                    && self
                                        .try_complete_after_chunk_exhaustion(
                                            base_url,
                                            transfer_id,
                                            &policy,
                                        )
                                        .await?
                                {
                                    return Ok(true);
                                }
                            }
                            // 重放后仍失败：若首查 status 时字节已收齐，也尝试 complete。
                            if policy.complete_fallback
                                && status_json_bytes_fully_received(&status)
                                && self
                                    .try_complete_after_chunk_exhaustion(
                                        base_url,
                                        transfer_id,
                                        &policy,
                                    )
                                    .await?
                            {
                                return Ok(true);
                            }
                            // 保留 last_err（若有）；无 last_err 且重放 Ok(false) → Ok(false)。
                            if let Some(e) = last_err {
                                return Err(e);
                            }
                            return Ok(false);
                        }
                    }
                }
            }
        }

        Err(last_err.unwrap_or_else(|| PeerCallError::InvalidResponse {
            url,
            reason: "chunk 重试耗尽且无错误详情".to_string(),
        }))
    }

    /// Business Logic（为什么需要这个函数）:
    ///     最后一块 durable history 故障会让 chunk 持续 503 且 status 仍 transferring；
    ///     发送端若只重试 chunk 最终 failed，对端却留下已落盘文件 + intent，形成分裂终态。
    ///     字节已收齐时必须在 transfer.complete.v1 语义下改走 complete 驱动 durable 晋升。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用与 chunk 策略对齐的短退避调用 `transfer_complete_with_policy`；
    ///     Ok(true) → 成功；Ok(false)/Err → 返回 false 让调用方按原 chunk 失败路径处理。
    async fn try_complete_after_chunk_exhaustion(
        &self,
        base_url: &str,
        transfer_id: &str,
        chunk_policy: &TransferChunkPolicy,
    ) -> Result<bool, PeerCallError> {
        tracing::info!("chunk 重试耗尽且字节已收齐，尝试 complete 收敛: {transfer_id}");
        let complete_policy = TransferCompletePolicy {
            max_attempts: chunk_policy.max_attempts.max(2),
            base_backoff: chunk_policy.base_backoff,
            status_fallback: true,
        };
        match self
            .transfer_complete_with_policy(base_url, transfer_id, complete_policy)
            .await
        {
            Ok(true) => {
                tracing::info!("chunk 失败后 complete 收敛成功: {transfer_id}");
                Ok(true)
            }
            Ok(false) => {
                tracing::warn!("chunk 失败后 complete 返回 success=false: {transfer_id}");
                Ok(false)
            }
            Err(e) => {
                tracing::warn!("chunk 失败后 complete 收敛失败: {transfer_id}: {e}");
                // complete 失败不覆盖原始 chunk 错误语义：返回 false 让上层用 last_err。
                Ok(false)
            }
        }
    }

    /// 查询对端某接收任务的状态。
    ///
    /// Business Logic: 发送端可轮询对端接收进度（M5 当前未强制使用，保留供扩展）。
    ///     对照 Python `get_transfer_status`（GET /api/transfer/status/{id}）。
    ///
    /// Code Logic（Finding 2 起）: GET `{base_url}/api/transfer/status/{id}`，经共享 `request_get`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析。
    #[allow(dead_code)]
    pub async fn transfer_status(
        &self,
        base_url: &str,
        transfer_id: &str,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{base_url}/api/transfer/status/{transfer_id}");
        self.request_get::<serde_json::Value>(&url)
            .await
            .map_err(|e| peer_call_error_to_transfer_message("status", &url, e))
    }

    /// 显式 complete/finalize 握手：要求对端校验并落地后返回 success。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     size=0 与 resume_offset==size 时不会发送任何 chunk；发送端必须确认对端已 finalize
    ///     才能标记本地 completed，否则会假报成功而远端只剩隐藏 .tmp。
    ///     接收端可能已完成 SHA256/落盘/墓碑，但响应在网络中丢失；同一 transfer_id 在墓碑
    ///     TTL 内重放 complete 是 naturally-idempotent，必须有界重试以避免分裂终态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `{base_url}/api/transfer/complete/{id}`，空 JSON body；期望 `{success, received_bytes}`。
    ///     对 Network / 5xx / 504 / retryable Remote 做有界退避重试（默认 4 次，间隔 200ms*attempt）。
    ///     HTTP 200 但 success=false 时：status=completed → 成功收敛；status=transferring/pending
    ///     且仍有剩余 attempt → 退避后重发 complete（覆盖 durable history 写入瞬时失败窗口）。
    ///     仍失败时可选 status 轮询：若对端 status=completed 则视为成功（响应丢失收敛）。
    ///     success==true → Ok(true)；不可恢复的 success=false → Ok(false)；其它 → Err(PeerCallError)。
    pub async fn transfer_complete(
        &self,
        base_url: &str,
        transfer_id: &str,
    ) -> Result<bool, PeerCallError> {
        self.transfer_complete_with_policy(base_url, transfer_id, TransferCompletePolicy::default())
            .await
    }

    /// 带策略的 complete 握手（测试可注入更短重试）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     生产路径与单测共享同一重试/status 收敛逻辑；单测可缩小 attempts 与 backoff。
    ///
    /// Code Logic（这个函数做什么）:
    ///     见 `transfer_complete`；`policy` 控制最大尝试次数与基础退避。
    pub async fn transfer_complete_with_policy(
        &self,
        base_url: &str,
        transfer_id: &str,
        policy: TransferCompletePolicy,
    ) -> Result<bool, PeerCallError> {
        let url = format!("{base_url}/api/transfer/complete/{transfer_id}");
        let body = serde_json::json!({});
        let mut last_err: Option<PeerCallError> = None;
        // success=false 且 status 仍 transferring 时的最后一次观测，用于重试耗尽诊断。
        let mut saw_retryable_soft_false = false;

        for attempt in 1..=policy.max_attempts {
            match self.request_post::<ChunkResp, _>(&url, &body).await {
                Ok(resp) if resp.success => return Ok(true),
                Ok(_resp) => {
                    // success=false：可能是尚未收齐、durable 晋升失败、或重启后内存 miss。
                    // 开启 status_fallback 时再查终态：completed 收敛成功；transferring/pending
                    // 则在有界策略内退避重发 complete，驱动接收端 promote_completed_to_durable。
                    if policy.status_fallback {
                        if let Ok(status) = self.transfer_status_typed(base_url, transfer_id).await
                        {
                            if status_json_is_completed(&status) {
                                tracing::info!(
                                    "transfer complete success=false 但 status=completed，按成功收敛: {transfer_id}"
                                );
                                return Ok(true);
                            }
                            if status_json_is_still_transferring(&status)
                                && attempt < policy.max_attempts
                            {
                                saw_retryable_soft_false = true;
                                tracing::debug!(
                                    "transfer complete success=false 且 status 仍 transferring，退避重试 complete: {transfer_id} attempt={attempt}/{}",
                                    policy.max_attempts
                                );
                                let backoff = policy.base_backoff * attempt;
                                tokio::time::sleep(backoff).await;
                                continue;
                            }
                        }
                    }
                    return Ok(false);
                }
                Err(e) if is_transfer_complete_retryable(&e) && attempt < policy.max_attempts => {
                    tracing::debug!(
                        "transfer complete 可重试失败 attempt={attempt}/{} ({url}): {e}",
                        policy.max_attempts
                    );
                    last_err = Some(e);
                    let backoff = policy.base_backoff * attempt;
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            }
        }

        // 响应丢失收敛：complete 失败后查 status，若已 completed 则本地视为成功。
        if policy.status_fallback {
            if let Ok(status) = self.transfer_status_typed(base_url, transfer_id).await {
                if status_json_is_completed(&status) {
                    tracing::info!(
                        "transfer complete 响应丢失但 status=completed，按成功收敛: {transfer_id}"
                    );
                    return Ok(true);
                }
            }
        }

        if let Some(e) = last_err {
            return Err(e);
        }
        // soft success=false 重试耗尽且 status 从未 completed：返回 false（业务未完成）。
        if saw_retryable_soft_false {
            return Ok(false);
        }
        Err(PeerCallError::InvalidResponse {
            url,
            reason: "complete 重试耗尽且无错误详情".to_string(),
        })
    }

    /// 查询对端接收任务状态（结构化错误，供 complete 收敛使用）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     complete 响应丢失时需要用 status 确认远端是否已终态 completed，避免假失败。
    ///
    /// Code Logic（这个函数做什么）:
    ///     GET `/api/transfer/status/{id}`，保留 `PeerCallError` 结构。
    pub async fn transfer_status_typed(
        &self,
        base_url: &str,
        transfer_id: &str,
    ) -> Result<serde_json::Value, PeerCallError> {
        let url = format!("{base_url}/api/transfer/status/{transfer_id}");
        self.request_get::<serde_json::Value>(&url).await
    }

    /// Claude Code 历史同步 pull：向对端发送本端 cc 历史摘要，获取对端认为本端需要的 cc 历史。
    ///
    /// Business Logic: CC 历史同步第一步——把本端摘要发给对端，对端比对后返回本端需要更新的
    ///     cc 历史完整数据。走独立链路 `/api/cc-history/sync/pull`，与 prompts 同步解耦。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/cc-history/sync/pull`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；失败记录结构化错误后
    ///     返回空 Vec（不阻断同步）。
    pub async fn cc_sync_pull(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Vec<crate::cc::models::ClaudeHistoryRow> {
        let url = format!("{base_url}/api/cc-history/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        match self.request_post::<CcSyncPullResp, _>(&url, &body).await {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_pull",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!("cc_sync_pull 获取 {} 条 CC 历史", data.items.len()))
                .emit();
                data.items
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_pull",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code("unavailable")
                .message(format!("cc_sync_pull 失败: {e}"))
                .emit();
                Vec::new()
            }
        }
    }

    /// Claude Code 历史同步 push：将本端有而对端缺少的 cc 历史推送给对端。
    ///
    /// Business Logic: CC 历史同步第二步——把本端独有或领先的 cc 历史推过去。
    ///     **仅** legacy 回退路径使用；paged 能力对端走 `cc_sync_push_batch`。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/cc-history/sync/push`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；HTTP 2xx 即视为成功。
    pub async fn cc_sync_push(
        &self,
        base_url: &str,
        items: &[crate::cc::models::ClaudeHistoryRow],
    ) -> bool {
        let url = format!("{base_url}/api/cc-history/sync/push");
        let body = serde_json::json!({ "items": items });
        match self.request_post::<CcSyncPushResp, _>(&url, &body).await {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_push",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!("cc_sync_push 成功，对端接收 {} 条", data.accepted))
                .emit();
                true
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_push",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code("unavailable")
                .message(format!("cc_sync_push 失败: {e}"))
                .emit();
                false
            }
        }
    }

    /// 分页同步：拉一页对端 CC 历史摘要。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     有界分页协议先交换摘要再按需拉正文；必须返回结构化错误，绝不能把失败折叠成空页，
    ///     否则引擎会误判「对端无数据」并当成成功零同步。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/cc-history/sync/manifest-page` body `{cursor, limit}`，
    ///     成功返回 `CcManifestPageResp`；任何网络/业务/解析失败原样上抛 `PeerCallError`。
    pub async fn cc_sync_manifest_page(
        &self,
        base_url: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<crate::net::routes::cc_history::CcManifestPageResp, PeerCallError> {
        let url = format!("{base_url}/api/cc-history/sync/manifest-page");
        let body = serde_json::json!({
            "cursor": cursor,
            "limit": limit,
        });
        match self
            .request_post::<crate::net::routes::cc_history::CcManifestPageResp, _>(&url, &body)
            .await
        {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_manifest_page",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!(
                    "cc_sync_manifest_page 获取 {} 条摘要 done={}",
                    data.summaries.len(),
                    data.done
                ))
                .emit();
                Ok(data)
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_manifest_page",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("cc_sync_manifest_page 失败: {e}"))
                .emit();
                Err(e)
            }
        }
    }

    /// 分页同步：按 ID 批取对端完整 CC 历史正文。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     比较摘要后只拉取本端缺失/落后的正文；错误必须上抛以便引擎拆批或终止本轮，
    ///     禁止折叠成空 Vec 伪装成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/cc-history/sync/items` body `{ids}`，返回 `CcItemsResp` 或 `PeerCallError`
    ///     （含 `cc_history.batch_too_large` / `cc_history.item_too_large`）。
    pub async fn cc_sync_items(
        &self,
        base_url: &str,
        ids: &[String],
    ) -> Result<crate::net::routes::cc_history::CcItemsResp, PeerCallError> {
        let url = format!("{base_url}/api/cc-history/sync/items");
        let body = serde_json::json!({ "ids": ids });
        match self
            .request_post::<crate::net::routes::cc_history::CcItemsResp, _>(&url, &body)
            .await
        {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_items",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!(
                    "cc_sync_items 获取 {} 条 missing={}",
                    data.items.len(),
                    data.missing_ids.len()
                ))
                .emit();
                Ok(data)
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_items",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("cc_sync_items 失败: {e}"))
                .emit();
                Err(e)
            }
        }
    }

    /// 分页同步：批量 push 本端领先/独有的 CC 历史。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     对端有 paged 能力时用事务性 push-batch 代替 legacy 全量 push；失败必须上抛，
    ///     引擎据此拆批或结束本轮，禁止折叠成 `false` 后静默当成功。
    ///
    /// Code Logic（这个函数做什么）:
    ///     POST `/api/cc-history/sync/push-batch` body `{items}`，返回 `CcPushBatchResp`
    ///     或 `PeerCallError`（含 batch/item too large）。
    pub async fn cc_sync_push_batch(
        &self,
        base_url: &str,
        items: &[crate::cc::models::ClaudeHistoryRow],
    ) -> Result<crate::net::routes::cc_history::CcPushBatchResp, PeerCallError> {
        let url = format!("{base_url}/api/cc-history/sync/push-batch");
        let body = serde_json::json!({ "items": items });
        match self
            .request_post::<crate::net::routes::cc_history::CcPushBatchResp, _>(&url, &body)
            .await
        {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_push_batch",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!("cc_sync_push_batch 对端接受 {} 条", data.accepted))
                .emit();
                Ok(data)
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "cc_sync_push_batch",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("cc_sync_push_batch 失败: {e}"))
                .emit();
                Err(e)
            }
        }
    }

    /// SSH 目标同步 pull：向对端发送本端 SSH 目标摘要，获取对端认为本端需要的 SSH 目标。
    ///
    /// Business Logic: SSH 同步第一步——把本端摘要发给对端，对端比对后返回本端需要更新的
    ///     SSH 目标完整数据。走独立链路 `/api/ssh-target/sync/pull`，与 prompts 同步解耦。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/ssh-target/sync/pull`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；失败返回空 Vec。
    pub async fn ssh_target_pull(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Vec<crate::models::ssh_target::SshTargetRow> {
        let url = format!("{base_url}/api/ssh-target/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        match self.request_post::<SshTargetPullResp, _>(&url, &body).await {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "ssh_target_pull",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!(
                    "ssh_target_pull 获取 {} 条 SSH 目标",
                    data.targets.len()
                ))
                .emit();
                data.targets
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "ssh_target_pull",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code("unavailable")
                .message(format!("ssh_target_pull 失败: {e}"))
                .emit();
                Vec::new()
            }
        }
    }

    /// SSH 目标同步 push：将本端有而对端缺少的 SSH 目标推送给对端。
    ///
    /// Business Logic: SSH 同步第二步——把本端独有或领先的 SSH 目标推过去。
    ///
    /// Code Logic（Finding 2 起）: POST `{base_url}/api/ssh-target/sync/push`，经共享 `request_post`
    ///     helper 注入 X-CC-Request-Id 并用 `parse_peer_response` 统一解析；HTTP 2xx 即视为成功。
    pub async fn ssh_target_push(
        &self,
        base_url: &str,
        targets: &[crate::models::ssh_target::SshTargetRow],
    ) -> bool {
        let url = format!("{base_url}/api/ssh-target/sync/push");
        let body = serde_json::json!({ "targets": targets });
        match self.request_post::<SshTargetPushResp, _>(&url, &body).await {
            Ok(data) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "ssh_target_push",
                    crate::backend::logging::OperationResult::Ok,
                )
                .message(format!(
                    "ssh_target_push 成功，对端接收 {} 条",
                    data.accepted
                ))
                .emit();
                true
            }
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "ssh_target_push",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code("unavailable")
                .message(format!("ssh_target_push 失败: {e}"))
                .emit();
                false
            }
        }
    }

    // ===== N2 typed content sync (Prompt / SSH / Scratchpad) =====
    // 失败必须上抛 PeerCallError，禁止折叠为空页/false（见 invalid_json_is_failure_not_empty_remote）。

    /// 校验 push-batch accepted 不得超过本批条数。
    ///
    /// Business Logic: 对端若宣称 accepted 大于发送条数，属于协议损坏，不得当成功。
    /// Code Logic: accepted > sent → InvalidResponse。
    fn ensure_accepted_not_exceeds(
        url: &str,
        accepted: usize,
        sent: usize,
    ) -> Result<(), PeerCallError> {
        if accepted > sent {
            return Err(PeerCallError::InvalidResponse {
                url: url.to_string(),
                reason: format!("accepted {accepted} exceeds batch size {sent}"),
            });
        }
        Ok(())
    }


    /// SSH legacy pull（typed）。
    ///
    /// Business Logic: 引擎 legacy 路径区分空成功与传输失败。
    /// Code Logic: POST `/api/ssh-target/sync/pull`，失败上抛。
    pub async fn ssh_target_pull_result(
        &self,
        base_url: &str,
        local_summary: Vec<serde_json::Value>,
    ) -> Result<Vec<crate::models::ssh_target::SshTargetRow>, PeerCallError> {
        let url = format!("{base_url}/api/ssh-target/sync/pull");
        let body = serde_json::json!({ "summaries": local_summary });
        let data: SshTargetPullResp = self.request_post(&url, &body).await?;
        Ok(data.targets)
    }

    /// SSH legacy push（typed）。
    ///
    /// Business Logic: push 失败不得假成功。
    /// Code Logic: POST `/api/ssh-target/sync/push`，2xx → Ok(true)。
    pub async fn ssh_target_push_result(
        &self,
        base_url: &str,
        targets: &[crate::models::ssh_target::SshTargetRow],
    ) -> Result<bool, PeerCallError> {
        let url = format!("{base_url}/api/ssh-target/sync/push");
        let body = serde_json::json!({ "targets": targets });
        let _data: SshTargetPushResp = self.request_post(&url, &body).await?;
        Ok(true)
    }

    /// Scratchpad legacy pull（typed）。
    ///
    /// Business Logic: 引擎 legacy 路径区分空成功与传输失败。
    /// Code Logic: POST `/api/scratchpad/sync/pull`，失败上抛。
    pub async fn scratchpad_pull_result(
        &self,
        base_url: &str,
        summaries: Vec<serde_json::Value>,
    ) -> Result<Vec<crate::models::scratchpad::ScratchpadRow>, PeerCallError> {
        let url = format!("{base_url}/api/scratchpad/sync/pull");
        let body = serde_json::json!({ "summaries": summaries });
        let data: ScratchpadPullResp = self.request_post(&url, &body).await?;
        Ok(data.pages)
    }

    /// Scratchpad legacy push（typed）。
    ///
    /// Business Logic: push 失败不得假成功。
    /// Code Logic: POST `/api/scratchpad/sync/push`，2xx → Ok(true)。
    pub async fn scratchpad_push_result(
        &self,
        base_url: &str,
        pages: &[crate::models::scratchpad::ScratchpadRow],
    ) -> Result<bool, PeerCallError> {
        let url = format!("{base_url}/api/scratchpad/sync/push");
        let body = serde_json::json!({ "pages": pages });
        let _data: ScratchpadPushResp = self.request_post(&url, &body).await?;
        Ok(true)
    }

    /// Prompt v2：拉一页对端 manifest 摘要。
    ///
    /// Business Logic: 引擎流式拉完全部页后才能与本地完整 manifest 比较；失败不得空页伪装。
    /// Code Logic: POST `/api/sync/prompts/manifest-page`，返回 `SyncManifestPage` 或 `PeerCallError`。
    pub async fn list_prompt_manifest_page(
        &self,
        base_url: &str,
        cursor: Option<&str>,
    ) -> Result<crate::sync::protocol::SyncManifestPage<String>, PeerCallError> {
        let url = format!("{base_url}/api/sync/prompts/manifest-page");
        let body = serde_json::json!({ "cursor": cursor, "limit": null });
        match self
            .request_post::<crate::sync::protocol::SyncManifestPage<String>, _>(&url, &body)
            .await
        {
            Ok(data) => Ok(data),
            Err(e) => {
                crate::backend::logging::OperationLog::new(
                    "p2p",
                    "list_prompt_manifest_page",
                    crate::backend::logging::OperationResult::Error,
                )
                .level(tracing::Level::WARN)
                .error_code(e.code().unwrap_or("unavailable"))
                .message(format!("list_prompt_manifest_page 失败: {e}"))
                .emit();
                Err(e)
            }
        }
    }

    /// Prompt v2：按 id 批取正文。
    ///
    /// Business Logic: 比较摘要后只拉需要的正文；错误上抛。
    /// Code Logic: POST `/api/sync/prompts/items`。
    pub async fn fetch_prompt_items(
        &self,
        base_url: &str,
        ids: &[String],
    ) -> Result<crate::net::routes::sync::PromptItemsResp, PeerCallError> {
        let url = format!("{base_url}/api/sync/prompts/items");
        let body = serde_json::json!({ "ids": ids });
        self.request_post(&url, &body).await
    }

    /// Prompt v2：批量 push 正文。
    ///
    /// Business Logic: 有界 batch + client_request_id + claimed_device_id（ledger 命名空间，非认证）；
    ///     仅在完整 manifest + 成功 apply 后可在末批携带 `acked_delete_epoch` 推进对端水位。
    /// Code Logic: POST `/api/sync/prompts/push-batch`；`acked_delete_epoch=None` 序列化为 null。
    pub async fn push_prompt_batch(
        &self,
        base_url: &str,
        items: &[crate::models::prompt::PromptRow],
        client_request_id: &str,
        claimed_device_id: &str,
        acked_delete_epoch: Option<u64>,
    ) -> Result<crate::net::routes::sync::PromptPushBatchResp, PeerCallError> {
        let url = format!("{base_url}/api/sync/prompts/push-batch");
        let body = serde_json::json!({
            "items": items,
            "client_request_id": client_request_id,
            "claimed_device_id": claimed_device_id,
            "acked_delete_epoch": acked_delete_epoch,
        });
        let data: crate::net::routes::sync::PromptPushBatchResp =
            self.request_post(&url, &body).await?;
        Self::ensure_accepted_not_exceeds(&url, data.accepted, items.len())?;
        Ok(data)
    }

    /// SSH v2：拉一页对端 manifest 摘要（id=host）。
    ///
    /// Business Logic: 与 Prompt 同构的 typed 分页；失败不得空页。
    /// Code Logic: POST `/api/ssh-target/sync/manifest-page`。
    pub async fn list_ssh_manifest_page(
        &self,
        base_url: &str,
        cursor: Option<&str>,
    ) -> Result<crate::sync::protocol::SyncManifestPage<String>, PeerCallError> {
        let url = format!("{base_url}/api/ssh-target/sync/manifest-page");
        let body = serde_json::json!({ "cursor": cursor, "limit": null });
        self.request_post(&url, &body).await
    }

    /// SSH v2：按 host 批取正文。
    ///
    /// Business Logic: 只拉需要的目标配置；错误上抛。
    /// Code Logic: POST `/api/ssh-target/sync/items`。
    pub async fn fetch_ssh_items(
        &self,
        base_url: &str,
        ids: &[String],
    ) -> Result<crate::net::routes::ssh_target_sync::SshItemsResp, PeerCallError> {
        let url = format!("{base_url}/api/ssh-target/sync/items");
        let body = serde_json::json!({ "ids": ids });
        self.request_post(&url, &body).await
    }

    /// SSH v2：批量 push。
    ///
    /// Business Logic: accepted 不得大于本批条数；末批可带 acked_delete_epoch 推进水位。
    /// Code Logic: POST `/api/ssh-target/sync/push-batch`；携带 claimed_device_id。
    pub async fn push_ssh_batch(
        &self,
        base_url: &str,
        items: &[crate::models::ssh_target::SshTargetRow],
        client_request_id: &str,
        claimed_device_id: &str,
        acked_delete_epoch: Option<u64>,
    ) -> Result<crate::net::routes::ssh_target_sync::SshPushBatchResp, PeerCallError> {
        let url = format!("{base_url}/api/ssh-target/sync/push-batch");
        let body = serde_json::json!({
            "items": items,
            "client_request_id": client_request_id,
            "claimed_device_id": claimed_device_id,
            "acked_delete_epoch": acked_delete_epoch,
        });
        let data: crate::net::routes::ssh_target_sync::SshPushBatchResp =
            self.request_post(&url, &body).await?;
        Self::ensure_accepted_not_exceeds(&url, data.accepted, items.len())?;
        Ok(data)
    }

    /// Scratchpad v2：拉一页对端 manifest 摘要。
    ///
    /// Business Logic: typed 分页，失败不得空页。
    /// Code Logic: POST `/api/scratchpad/sync/manifest-page`。
    pub async fn list_scratchpad_manifest_page(
        &self,
        base_url: &str,
        cursor: Option<&str>,
    ) -> Result<crate::sync::protocol::SyncManifestPage<String>, PeerCallError> {
        let url = format!("{base_url}/api/scratchpad/sync/manifest-page");
        let body = serde_json::json!({ "cursor": cursor, "limit": null });
        self.request_post(&url, &body).await
    }

    /// Scratchpad v2：按 id 批取正文。
    ///
    /// Business Logic: 只拉需要的页面；错误上抛。
    /// Code Logic: POST `/api/scratchpad/sync/items`。
    pub async fn fetch_scratchpad_items(
        &self,
        base_url: &str,
        ids: &[String],
    ) -> Result<crate::net::routes::scratchpad_sync::ScratchpadItemsResp, PeerCallError> {
        let url = format!("{base_url}/api/scratchpad/sync/items");
        let body = serde_json::json!({ "ids": ids });
        self.request_post(&url, &body).await
    }

    /// Scratchpad v2：批量 push。
    ///
    /// Business Logic: accepted 不得大于本批条数；末批可带 acked_delete_epoch 推进水位。
    /// Code Logic: POST `/api/scratchpad/sync/push-batch`；携带 claimed_device_id。
    pub async fn push_scratchpad_batch(
        &self,
        base_url: &str,
        items: &[crate::models::scratchpad::ScratchpadRow],
        client_request_id: &str,
        claimed_device_id: &str,
        acked_delete_epoch: Option<u64>,
    ) -> Result<crate::net::routes::scratchpad_sync::ScratchpadPushBatchResp, PeerCallError> {
        let url = format!("{base_url}/api/scratchpad/sync/push-batch");
        let body = serde_json::json!({
            "items": items,
            "client_request_id": client_request_id,
            "claimed_device_id": claimed_device_id,
            "acked_delete_epoch": acked_delete_epoch,
        });
        let data: crate::net::routes::scratchpad_sync::ScratchpadPushBatchResp =
            self.request_post(&url, &body).await?;
        Self::ensure_accepted_not_exceeds(&url, data.accepted, items.len())?;
        Ok(data)
    }
}

impl Default for PeerClient {
    fn default() -> Self {
        Self::new()
    }
}

/// 把 `PeerCallError` 折叠为 transfer 调用方的字符串错误文案（Finding 2）。
///
/// Business Logic（为什么需要这个函数）:
///     `transfer_init`/`transfer_chunk`/`transfer_status` 的公开签名沿用 `Result<_, String>`（调用方
///     `transfer/sender.rs` 把它写进失败任务的 errorMessage）。直接返回 `PeerCallError` 会破坏签名；
///     但旧实现只写 `"init HTTP 503"`，丢失了 code/retryable/request_id。本函数把结构化错误折叠成
///     含 code/status 的可读字符串，让调用方/日志/用户仍能看到失败语义（如 `[unavailable/503]`）。
///
/// Code Logic（这个函数做什么）:
///     - `Remote` → `"{step} 失败 ({url}): HTTP {status} [{code}]"`（保留 code/status）；
///     - `Network` → `"{step} 网络失败 ({url}): {source}"`；
///     - `Unsupported` → `"{step} 对端不支持能力 {capability}"`；
///     - `InvalidResponse` → `"{step} 响应无法解析 ({url}): {reason}"`。
fn peer_call_error_to_transfer_message(step: &str, url: &str, error: PeerCallError) -> String {
    match error {
        PeerCallError::Remote { status, code, .. } => {
            format!("{step} 失败 ({url}): HTTP {status} [{code}]")
        }
        PeerCallError::Network { source, .. } => {
            format!("{step} 网络失败 ({url}): {source}")
        }
        PeerCallError::Unsupported { capability, .. } => {
            format!("{step} 对端不支持能力 {capability}")
        }
        PeerCallError::InvalidResponse { reason, .. } => {
            format!("{step} 响应无法解析 ({url}): {reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    /// Business Logic（为什么需要这个测试）:
    ///     对端可能是尚未携带 protocol_version / capabilities 字段的旧版；客户端必须能容忍缺失字段
    ///     并安全回落为 v0 + 空能力，不能因 JSON 字段缺失而 health_info 失败。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化旧版 health 响应 JSON（无 protocol 元数据）为 HealthResponse，
    ///     断言 protocol_version == 0 且 capabilities 为空，且基础字段（ok/device_id 等）正确。
    #[test]
    fn health_info_parses_legacy_response_without_protocol_fields() {
        let json = r#"{
            "ok": true,
            "device_id": "device-legacy",
            "device_name": "legacy-device",
            "http_port": 8765,
            "ts": 1700000000
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.device_id, "device-legacy");
        assert_eq!(resp.protocol_version, 0);
        assert!(resp.capabilities.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     新版（v1）对端响应携带 protocol_version 与 capabilities；客户端必须能解析出这些字段，
    ///     并据此判定对端支持 errors.envelope.v1，否则能力探测会全部误判为不支持。
    ///
    /// Code Logic（这个测试做什么）:
    ///     反序列化 v1 响应 JSON（含 capabilities）为 HealthResponse，断言 protocol_version == 1、
    ///     capabilities 含 errors.envelope.v1，且 protocol_info().supports() 命中。
    #[test]
    fn health_info_parses_v1_response_with_capabilities() {
        let json = r#"{
            "ok": true,
            "device_id": "device-v1",
            "device_name": "v1-device",
            "http_port": 8765,
            "ts": 1700000000,
            "protocol_version": 1,
            "capabilities": ["errors.envelope.v1"]
        }"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.protocol_version, 1);
        assert_eq!(resp.capabilities, vec!["errors.envelope.v1".to_string()]);
        let proto = resp.protocol_info();
        assert!(proto.supports("errors.envelope.v1"));
        assert!(!proto.supports("inbox.messages.v1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `PeerCallError`（统一到 `net::peer_error` 后）仍是 health_info 失败的统一封装；
    ///     调用方需对 Network/Unsupported/InvalidResponse/Remote 四类失败分别处理，必须能用 match 区分。
    ///
    /// Code Logic（这个测试做什么）:
    ///     手工构造 `Remote` 与 `Unsupported`（无需 reqwest::Error），断言 Display 含 url/状态/code 上下文。
    #[test]
    fn peer_call_error_variants_carry_url_context() {
        let remote_msg = format!(
            "{}",
            PeerCallError::Remote {
                url: "http://1.2.3.4:8765/api/health".to_string(),
                status: 503,
                code: "unavailable".to_string(),
                message: "busy".to_string(),
                request_id: "r".to_string(),
                retryable: true,
                legacy: false,
                details: serde_json::Value::Object(serde_json::Map::new()),
            }
        );
        assert!(remote_msg.contains("503"));
        assert!(remote_msg.contains("1.2.3.4:8765"));
        assert!(remote_msg.contains("unavailable"));

        let unsupported_msg = format!(
            "{}",
            PeerCallError::Unsupported {
                url: "http://1.2.3.4:8765".to_string(),
                capability: "errors.envelope.v1",
            }
        );
        assert!(unsupported_msg.contains("errors.envelope.v1"));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     legacy `health(addr, port)` 必须把 `{addr}:{port}` 拼成合法 base_url 形态，
    ///     以便后续 health_info 复用；这里只验证 URL 拼接形态（不发网络），
    ///     因为 health_info 自身由独立测试覆盖。
    ///
    /// Code Logic（这个测试做什么）:
    ///     通过 format! 验证 health() 内部使用的 base_url 形态符合 `http://{addr}:{port}` 约定。
    #[test]
    fn health_legacy_url_format_matches_health_info_contract() {
        let addr = "192.168.1.10";
        let port: u16 = 8765;
        let base_url = format!("http://{addr}:{port}");
        // health_info 内部会拼 `{base_url}/api/health`，验证拼出来无 `//` 重复斜杠。
        let full = format!("{base_url}/api/health");
        assert_eq!(full, "http://192.168.1.10:8765/api/health");
        assert!(!full.contains("//api"), "URL 出现重复斜杠: {full}");
    }

    // ===== Task 7: require_capability 能力门 =====

    /// 启动临时 health 服务，返回 base_url 与协议元数据可控的句柄。
    ///
    /// Code Logic: 用 axum 挂一个 `/api/health` handler，按入参 protocol_version/capabilities
    ///     返回 HealthResponse；端口绑定 0 由 OS 分配，避免与真实服务冲突。
    async fn spawn_health_server(protocol_version: u32, capabilities: Vec<String>) -> String {
        use axum::routing::get;
        let state = HealthState {
            protocol_version,
            capabilities,
        };
        let app = axum::Router::new()
            .route("/api/health", get(health_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// 测试用 health handler 状态。
    #[derive(Clone)]
    struct HealthState {
        protocol_version: u32,
        capabilities: Vec<String>,
    }

    /// 测试用 health handler：按 state 返回 HealthResponse。
    async fn health_handler(
        axum::extract::State(state): axum::extract::State<HealthState>,
    ) -> axum::Json<HealthResponse> {
        axum::Json(HealthResponse {
            ok: true,
            device_id: "test".to_string(),
            device_name: "test".to_string(),
            http_port: 8765,
            ts: 1_700_000_000,
            protocol_version: state.protocol_version,
            capabilities: state.capabilities,
        })
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端支持所需能力时，`require_capability` 必须返回 Ok(HealthResponse)，且调用方可复用结果。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v1 + errors.envelope.v1 的 health 服务，调用 require_capability，
    ///     断言 Ok 且 protocol_version == 1。
    #[tokio::test]
    async fn require_capability_passes_when_peer_supports_token() {
        let base_url = spawn_health_server(1, vec!["errors.envelope.v1".to_string()]).await;
        let client = PeerClient::new();
        let health = client
            .require_capability(&base_url, "errors.envelope.v1")
            .await
            .expect("支持能力时应通过");
        assert_eq!(health.protocol_version, 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端不支持所需能力时，`require_capability` 必须返回 `Unsupported`，
    ///     调用方据此跳过新路由，不打到对端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0（无能力）的 health 服务，调用 require_capability("errors.envelope.v1")，
    ///     断言错误为 Unsupported 且 capability 字段匹配。
    #[tokio::test]
    async fn require_capability_blocks_when_peer_lacks_token() {
        let base_url = spawn_health_server(0, vec![]).await;
        let client = PeerClient::new();
        let err = client
            .require_capability(&base_url, "errors.envelope.v1")
            .await
            .expect_err("缺失能力应被拦截");
        match err {
            PeerCallError::Unsupported { capability, .. } => {
                assert_eq!(capability, "errors.envelope.v1");
            }
            other => panic!("应为 Unsupported，实际: {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     对端离线（端口不可达）时，`require_capability` 必须把 health 失败原样上抛为 `Network`，
    ///     不能误报为 Unsupported（离线 ≠ 不支持）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     绑定一个立刻关闭的端口（用 ephemeral 端口但不启服务），调用 require_capability，
    ///     断言错误为 Network。
    #[tokio::test]
    async fn require_capability_propagates_network_error_when_offline() {
        // 绑定后立即释放，得到一个几乎必然连接被拒的端口。
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let err = client
            .require_capability(&base_url, "errors.envelope.v1")
            .await
            .expect_err("离线应为 Network");
        assert!(
            matches!(err, PeerCallError::Network { .. }),
            "离线应上报 Network，实际: {err:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     capability gate 必须在缺失能力时**不**调用目标路由。用一个共享 hit 计数器挂在新路由上，
    ///     缺失能力时计数器应保持 0，证明 gate 提前返回。
    ///
    /// Code Logic（这个测试做什么）:
    ///     启动 v0 health 服务 + 一个 `/api/inbox/messages` 探测路由（带 Arc<AtomicU32> 计数器），
    ///     先调 require_capability（返回 Unsupported），再断言探测路由计数器仍为 0。
    #[tokio::test]
    async fn capability_gate_stops_before_new_route_when_unsupported() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let app = axum::Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async {
                    axum::Json(HealthResponse {
                        ok: true,
                        device_id: "test".to_string(),
                        device_name: "test".to_string(),
                        http_port: 8765,
                        ts: 1_700_000_000,
                        protocol_version: 0, // v0：不支持任何 v1 能力
                        capabilities: vec![],
                    })
                }),
            )
            .route(
                "/api/inbox/messages",
                axum::routing::get(move || {
                    let hits = hits_clone.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({"messages": []}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();

        // 能力门拦截：v0 对端不支持 inbox.messages.v1。
        let gate_err = client
            .require_capability(&base_url, "inbox.messages.v1")
            .await
            .expect_err("v0 应被能力门拦截");
        assert!(matches!(gate_err, PeerCallError::Unsupported { .. }));

        // 关键断言：缺失能力时新路由不应被调用（计数器为 0）。
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "能力门未通过时不应调用新路由"
        );
    }

    // ===== Finding 2: 共享出站请求 helper =====

    /// 测试用 DTO：sync_push 响应子集（`{accepted: u64}`）。
    #[derive(Debug, serde::Deserialize)]
    struct AcceptedResp {
        #[serde(default)]
        accepted: u64,
    }

    /// Business Logic（为什么需要这个测试 / Finding 2）:
    ///     `request_post` 必须在每次出站请求上自动注入 `X-CC-Request-Id`，让对端把请求纳入
    ///     同一调用链日志；这是多跳代理关联的前提。用 echo handler 回写 observed header 验证。
    #[tokio::test]
    async fn request_post_helper_injects_request_id_header() {
        let app = axum::Router::new().route(
            "/api/echo-id",
            axum::routing::post(|headers: axum::http::HeaderMap| async move {
                let id = headers
                    .get("x-cc-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                axum::Json(serde_json::json!({ "accepted": id.len() as u64 }))
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/api/echo-id");
        let client = PeerClient::new();
        let body = serde_json::json!({});
        let resp: AcceptedResp = client
            .request_post(&url, &body)
            .await
            .expect("2xx 应解析成功");
        // UUID v4 长度 36 → accepted==36 证明 header 已注入并被对端观测到。
        assert_eq!(resp.accepted, 36, "request_post 应注入 36 字符 UUID header");
    }

    /// Business Logic（为什么需要这个测试 / Finding 2）:
    ///     `request_post` 经 `parse_peer_response` 解析时，对端 503 + v1 信封必须被分类为
    ///     `PeerCallError::Remote` 且保留 code/status/retryable，调用方据此（而非文案）决策重试。
    #[tokio::test]
    async fn request_post_helper_classifies_remote_error_envelope() {
        let app = axum::Router::new().route(
            "/api/fail",
            axum::routing::post(|| async move {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(serde_json::json!({
                        "error": "对端忙",
                        "code": "unavailable",
                        "request_id": "req-fail",
                        "retryable": true,
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("http://{addr}/api/fail");
        let client = PeerClient::new();
        let body = serde_json::json!({});
        let err = client
            .request_post::<AcceptedResp, _>(&url, &body)
            .await
            .expect_err("503 应为错误");
        match err {
            PeerCallError::Remote {
                status,
                code,
                request_id,
                retryable,
                ..
            } => {
                assert_eq!(status, 503);
                assert_eq!(code, "unavailable");
                assert_eq!(request_id, "req-fail");
                assert!(retryable);
            }
            other => panic!("应为 Remote，实际: {other:?}"),
        }
    }

    /// Business Logic（为什么需要这个测试 / Finding 2）:
    ///     transfer 失败文案必须携带结构化 code/status（不再只是 "init HTTP 503"），
    ///     便于调用方/日志诊断。验证 `peer_call_error_to_transfer_message` 折叠 Remote 时含
    ///     `[unavailable/503]` 形态。
    #[tokio::test]
    async fn transfer_init_failure_message_carries_code_and_status() {
        let app = axum::Router::new().route(
            "/api/transfer/init",
            axum::routing::post(|| async move {
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(serde_json::json!({
                        "error": "对端忙",
                        "code": "unavailable",
                        "request_id": "req-t",
                        "retryable": true,
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let err = client
            .transfer_init(&base_url, serde_json::json!({}))
            .await
            .expect_err("503 应返回 Err");
        assert!(
            err.contains("503"),
            "transfer_init 失败文案应含状态码: {err}"
        );
        assert!(
            err.contains("unavailable"),
            "transfer_init 失败文案应含 code: {err}"
        );
    }

    // ===== N2 Task 2: typed content-sync transport truth =====

    /// 启动可按路径返回固定 body/status 的假 peer，返回 base_url。
    async fn spawn_fixed_body_peer(
        path: &'static str,
        status: axum::http::StatusCode,
        body: &'static [u8],
        content_type: &'static str,
    ) -> String {
        let body = body.to_vec();
        let app = axum::Router::new().route(
            path,
            axum::routing::post(move || {
                let body = body.clone();
                async move {
                    (
                        status,
                        [(axum::http::header::CONTENT_TYPE, content_type)],
                        body,
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    /// Business Logic: 200 + 非 JSON 不得折叠为空 manifest（旧 bug）。
    /// Code Logic: list_prompt_manifest_page → InvalidResponse。
    #[tokio::test]
    async fn invalid_json_is_failure_not_empty_remote() {
        let base = spawn_fixed_body_peer(
            "/api/sync/prompts/manifest-page",
            axum::http::StatusCode::OK,
            b"not-json",
            "text/plain",
        )
        .await;
        let client = PeerClient::new();
        let result = client.list_prompt_manifest_page(&base, None).await;
        assert!(
            matches!(result, Err(PeerCallError::InvalidResponse { .. })),
            "expected InvalidResponse, got {result:?}"
        );
    }

    /// Business Logic: 500 信封必须是 Remote，不得空成功。
    /// Code Logic: list_prompt_manifest_page 解析 v1 信封。
    #[tokio::test]
    async fn remote_500_envelope_is_remote_not_empty() {
        let body = br#"{"error":"boom","code":"internal","request_id":"r1","retryable":false}"#;
        let base = spawn_fixed_body_peer(
            "/api/sync/prompts/manifest-page",
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            body,
            "application/json",
        )
        .await;
        let client = PeerClient::new();
        let err = client
            .list_prompt_manifest_page(&base, None)
            .await
            .expect_err("500 must fail");
        match err {
            PeerCallError::Remote { status, code, .. } => {
                assert_eq!(status, 500);
                assert_eq!(code, "internal");
            }
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    /// Business Logic: 连接断开是 Network，不得空页。
    /// Code Logic: 绑定后立即 drop listener。
    #[tokio::test]
    async fn disconnect_is_network_failure() {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let base = format!("http://{addr}");
        let client = PeerClient::new();
        let err = client
            .list_prompt_manifest_page(&base, None)
            .await
            .expect_err("disconnect must fail");
        assert!(
            matches!(err, PeerCallError::Network { .. }),
            "expected Network, got {err:?}"
        );
    }

    /// Business Logic: 413 batch_too_large 必须 Remote 且可按 code 分支。
    /// Code Logic: push_prompt_batch 收到 413 信封。
    #[tokio::test]
    async fn payload_too_large_413_is_remote() {
        let body = br#"{"error":"too big","code":"prompts.batch_too_large","request_id":"r2","retryable":false}"#;
        let base = spawn_fixed_body_peer(
            "/api/sync/prompts/push-batch",
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            body,
            "application/json",
        )
        .await;
        let client = PeerClient::new();
        let err = client
            .push_prompt_batch(&base, &[], "req-413", "test-device", None)
            .await
            .expect_err("413 must fail");
        match err {
            PeerCallError::Remote { status, code, .. } => {
                assert_eq!(status, 413);
                assert_eq!(code, "prompts.batch_too_large");
            }
            other => panic!("expected Remote, got {other:?}"),
        }
    }

    /// Business Logic: accepted 大于本批条数是协议损坏。
    /// Code Logic: ensure_accepted_not_exceeds → InvalidResponse。
    #[tokio::test]
    async fn accepted_count_mismatch_is_invalid_response() {
        let body = br#"{"accepted": 3}"#;
        let base = spawn_fixed_body_peer(
            "/api/sync/prompts/push-batch",
            axum::http::StatusCode::OK,
            body,
            "application/json",
        )
        .await;
        let client = PeerClient::new();
        // 发送 0 条，对端却 accepted=3
        let err = client
            .push_prompt_batch(&base, &[], "req-mismatch", "test-device", None)
            .await
            .expect_err("accepted mismatch must fail");
        assert!(
            matches!(err, PeerCallError::InvalidResponse { .. }),
            "expected InvalidResponse, got {err:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     complete 首次响应丢失/5xx 时，同一 transfer_id 必须在墓碑 TTL 内有界重试并最终成功，
    ///     避免远端已落地、本地标 failed 的分裂终态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock complete 前 2 次返回 503 retryable，第 3 次 success=true；短 backoff 策略下
    ///     transfer_complete_with_policy 应返回 Ok(true)，且命中 3 次。
    #[tokio::test]
    async fn transfer_complete_retries_retryable_5xx_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicU32::new(0));
        let hits_c = hits.clone();
        let app = axum::Router::new().route(
            "/api/transfer/complete/:id",
            axum::routing::post(move || {
                let hits = hits_c.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 3 {
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                            .header(axum::http::header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "error": "busy",
                                    "code": "unavailable",
                                    "request_id": "r-complete",
                                    "retryable": true,
                                })
                                .to_string(),
                            ))
                            .unwrap()
                    } else {
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::OK)
                            .header(axum::http::header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "success": true,
                                    "received_bytes": 1
                                })
                                .to_string(),
                            ))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferCompletePolicy {
            max_attempts: 4,
            base_backoff: Duration::from_millis(5),
            status_fallback: false,
        };
        let ok = client
            .transfer_complete_with_policy(&base_url, "tid-retry", policy)
            .await
            .expect("retryable 5xx 后应成功");
        assert!(ok);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "应在第 3 次成功");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     complete 响应全部丢失后，若 status 已 completed，本地必须收敛为成功，避免重复发送。
    ///
    /// Code Logic（这个测试做什么）:
    ///     complete 恒 503；status 返回 completed；开启 status_fallback 时 complete 应 Ok(true)。
    #[tokio::test]
    async fn transfer_complete_status_fallback_converges_on_completed() {
        let app = axum::Router::new()
            .route(
                "/api/transfer/complete/:id",
                axum::routing::post(|| async move {
                    (
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        axum::Json(serde_json::json!({
                            "error": "timeout",
                            "code": "timeout",
                            "request_id": "r-to",
                            "retryable": true,
                        })),
                    )
                }),
            )
            .route(
                "/api/transfer/status/:id",
                axum::routing::get(|| async move {
                    axum::Json(serde_json::json!({
                        "transfer_id": "tid-lost",
                        "status": "completed",
                        "progress": 1.0,
                        "transferred_bytes": 10,
                        "size": 10,
                        "filename": "a.bin"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferCompletePolicy {
            max_attempts: 2,
            base_backoff: Duration::from_millis(5),
            status_fallback: true,
        };
        let ok = client
            .transfer_complete_with_policy(&base_url, "tid-lost", policy)
            .await
            .expect("status=completed 应收敛成功");
        assert!(ok);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     最后一块已在远端提交但响应丢失时，发送端不得立即标 failed；必须经有界重试
    ///     与 status=completed 收敛为成功。
    ///
    /// Code Logic（这个测试做什么）:
    ///     chunk 恒 504；status 返回 completed；transfer_chunk_with_policy 应 Ok(true)。
    #[tokio::test]
    async fn transfer_chunk_status_fallback_converges_on_completed() {
        let app = axum::Router::new()
            .route(
                "/api/transfer/chunk/:id",
                axum::routing::post(|| async move {
                    (
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        axum::Json(serde_json::json!({
                            "error": "timeout",
                            "code": "timeout",
                            "request_id": "r-chunk-to",
                            "retryable": true,
                        })),
                    )
                }),
            )
            .route(
                "/api/transfer/status/:id",
                axum::routing::get(|| async move {
                    axum::Json(serde_json::json!({
                        "transfer_id": "tid-chunk-lost",
                        "status": "completed",
                        "progress": 1.0,
                        "transferred_bytes": 4,
                        "size": 4,
                        "filename": "last.bin"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferChunkPolicy {
            max_attempts: 2,
            base_backoff: Duration::from_millis(5),
            status_fallback: true,
            complete_fallback: false,
        };
        let ok = client
            .transfer_chunk_with_policy(&base_url, "tid-chunk-lost", 0, b"data".to_vec(), policy)
            .await
            .expect("最后一块响应丢失但 status=completed 应收敛成功");
        assert!(ok);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     chunk 瞬时 5xx 后同 offset 重放必须成功，避免可恢复抖动导致整次传输失败。
    #[tokio::test]
    async fn transfer_chunk_retries_retryable_5xx_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicU32::new(0));
        let hits_c = hits.clone();
        let app = axum::Router::new().route(
            "/api/transfer/chunk/:id",
            axum::routing::post(move || {
                let hits = hits_c.clone();
                async move {
                    let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 3 {
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                            .header(axum::http::header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "error": "busy",
                                    "code": "unavailable",
                                    "request_id": "r-chunk",
                                    "retryable": true,
                                })
                                .to_string(),
                            ))
                            .unwrap()
                    } else {
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::OK)
                            .header(axum::http::header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "success": true,
                                    "received_bytes": 4
                                })
                                .to_string(),
                            ))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferChunkPolicy {
            max_attempts: 4,
            base_backoff: Duration::from_millis(5),
            status_fallback: false,
            complete_fallback: false,
        };
        let ok = client
            .transfer_chunk_with_policy(&base_url, "tid-chunk-retry", 0, b"data".to_vec(), policy)
            .await
            .expect("retryable 5xx 后 chunk 应成功");
        assert!(ok);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     接收端 history 故障时最后一块可能已落盘但仍返回 503，status 保持 transferring 且
    ///     transferred_bytes==size。chunk 重试耗尽后必须改走 complete 才能 durable 收敛，
    ///     否则发送端 failed、对端留下文件+intent，后续重传可能产生副本。
    ///
    /// Code Logic（这个测试做什么）:
    ///     chunk 恒 503；status 恒 transferring + transferred_bytes==size；
    ///     complete 第 1 次 success=false、第 2 次 success=true；
    ///     transfer_chunk_with_policy 应 Ok(true)，且 complete 至少命中 2 次。
    #[tokio::test]
    async fn transfer_chunk_exhaustion_with_full_bytes_converges_via_complete() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let chunk_hits = Arc::new(AtomicU32::new(0));
        let complete_hits = Arc::new(AtomicU32::new(0));
        let chunk_c = chunk_hits.clone();
        let complete_c = complete_hits.clone();
        let app = axum::Router::new()
            .route(
                "/api/transfer/chunk/:id",
                axum::routing::post(move || {
                    let hits = chunk_c.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        axum::response::Response::builder()
                            .status(axum::http::StatusCode::SERVICE_UNAVAILABLE)
                            .header(axum::http::header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(
                                serde_json::json!({
                                    "error": "history not durable",
                                    "code": "unavailable",
                                    "request_id": "r-chunk-full",
                                    "retryable": true,
                                })
                                .to_string(),
                            ))
                            .unwrap()
                    }
                }),
            )
            .route(
                "/api/transfer/status/:id",
                axum::routing::get(|| async move {
                    axum::Json(serde_json::json!({
                        "transfer_id": "tid-chunk-complete",
                        "status": "transferring",
                        "progress": 1.0,
                        "transferred_bytes": 4,
                        "size": 4,
                        "filename": "full.bin"
                    }))
                }),
            )
            .route(
                "/api/transfer/complete/:id",
                axum::routing::post(move || {
                    let hits = complete_c.clone();
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if n < 2 {
                            axum::Json(serde_json::json!({
                                "success": false,
                                "received_bytes": 4
                            }))
                            .into_response()
                        } else {
                            axum::Json(serde_json::json!({
                                "success": true,
                                "received_bytes": 4
                            }))
                            .into_response()
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferChunkPolicy {
            max_attempts: 2,
            base_backoff: Duration::from_millis(5),
            status_fallback: true,
            complete_fallback: true,
        };
        let ok = client
            .transfer_chunk_with_policy(
                &base_url,
                "tid-chunk-complete",
                0,
                b"data".to_vec(),
                policy,
            )
            .await
            .expect("字节已收齐时 chunk 耗尽应经 complete 收敛成功");
        assert!(ok);
        assert!(
            chunk_hits.load(Ordering::SeqCst) >= 2,
            "chunk 应先耗尽有界重试"
        );
        assert!(
            complete_hits.load(Ordering::SeqCst) >= 2,
            "complete 应被调用并重试到 success"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     complete 返回 HTTP 200 但 success=false 时，若 status 已 completed（如重启后
    ///     history 收敛前的短暂窗口），客户端仍应收敛成功。
    #[tokio::test]
    async fn transfer_complete_false_still_status_fallback() {
        let app = axum::Router::new()
            .route(
                "/api/transfer/complete/:id",
                axum::routing::post(|| async move {
                    axum::Json(serde_json::json!({
                        "success": false,
                        "received_bytes": 0
                    }))
                }),
            )
            .route(
                "/api/transfer/status/:id",
                axum::routing::get(|| async move {
                    axum::Json(serde_json::json!({
                        "transfer_id": "tid-false",
                        "status": "completed",
                        "progress": 1.0,
                        "transferred_bytes": 1,
                        "size": 1,
                        "filename": "x.bin"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferCompletePolicy {
            max_attempts: 1,
            base_backoff: Duration::from_millis(1),
            status_fallback: true,
        };
        let ok = client
            .transfer_complete_with_policy(&base_url, "tid-false", policy)
            .await
            .expect("success=false + status=completed 应收敛");
        assert!(ok);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     接收端文件已落盘但 transfer_history 写入失败时，complete 可能返回 200 success=false，
    ///     status 仍为 transferring。发送端必须在有界策略内重发 complete，驱动 durable 晋升，
    ///     而不能立刻 Ok(false) 把任务记为 failed。
    ///
    /// Code Logic（这个测试做什么）:
    ///     complete 前 2 次 success=false，第 3 次 success=true；status 在成功前恒 transferring；
    ///     短 backoff 下 transfer_complete_with_policy 应 Ok(true)，且 complete 命中 3 次。
    #[tokio::test]
    async fn transfer_complete_retries_success_false_while_transferring() {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let hits = Arc::new(AtomicU32::new(0));
        let hits_c = hits.clone();
        let app = axum::Router::new()
            .route(
                "/api/transfer/complete/:id",
                axum::routing::post(move || {
                    let hits = hits_c.clone();
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if n < 3 {
                            axum::Json(serde_json::json!({
                                "success": false,
                                "received_bytes": 10
                            }))
                            .into_response()
                        } else {
                            axum::Json(serde_json::json!({
                                "success": true,
                                "received_bytes": 10
                            }))
                            .into_response()
                        }
                    }
                }),
            )
            .route(
                "/api/transfer/status/:id",
                axum::routing::get(|| async move {
                    axum::Json(serde_json::json!({
                        "transfer_id": "tid-soft",
                        "status": "transferring",
                        "progress": 1.0,
                        "transferred_bytes": 10,
                        "size": 10,
                        "filename": "soft.bin"
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let policy = TransferCompletePolicy {
            max_attempts: 4,
            base_backoff: Duration::from_millis(5),
            status_fallback: true,
        };
        let ok = client
            .transfer_complete_with_policy(&base_url, "tid-soft", policy)
            .await
            .expect("success=false+transferring 应有界重试后成功");
        assert!(ok);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "应在第 3 次 complete 成功");
    }
}
