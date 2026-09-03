//! net/routes/relay.rs — 中转访问（跳板机）三路由 handler
//!
//! Business Logic（为什么需要这个模块）:
//!     发起方 A 无法直连目标 C 时，经共同可达邻居 B 的 `/api/relay/*` 路由访问 C：
//!     - `GET /api/relay/peers`：A 探测 B 直连可见、可被中转的设备清单（合成影子设备）；
//!     - `ANY /api/relay/:device_id/*path`：透明转发（剥前缀、透传方法/headers/流式 body）；
//!     - `GET /api/relay/:device_id/api/workbench/terminal-input-stream`：终端输入 WS 桥。
//!     三个 handler 共享同一组前置检查（enabled / 白名单 / 目标解析 / 并发上限），
//!     错误统一走 `relay_*` domain code 信封。
//!
//! Code Logic（这个模块做什么）:
//!     handler 只做协议编排：检查 → 解析 → 获取许可 → 委托 `net::relay` 的转发/桥接
//!     核心。路由在 `http_server.rs` 的 LAN 业务路由区注册（静态路由先于 `*path` 通配，
//!     axum matchit 静态优先，`/api/relay/peers` 与 terminal-input-stream 精确路径
//!     不会被通配吞掉——测试覆盖）。

use crate::net::error_response::P2pError;
use crate::net::relay::{
    bridge_relay_websocket, build_relay_terminal_upstream_request, forward_relay_request,
    is_relay_path_allowed, relay_enabled, relay_error, resolve_relay_target, RelayForwardJob,
    RELAY_PER_TARGET_MAX_CONCURRENCY,
};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Extension, FromRequestParts, Path as AxumPath, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use tokio_tungstenite::connect_async;

/// relay 转发的终端输入 WS 内层路径（与 B 端目标设备的既有路由一致）。
const TERMINAL_INPUT_INNER_PATH: &str = "/api/workbench/terminal-input-stream";

/// `/api/relay/peers` 返回的中转目标设备条目（camelCase DTO）。
///
/// Business Logic（为什么需要这个结构）:
///     A 侧需要知道"经 B 可见哪些设备"来合成影子设备（名称/在线状态/协议与能力提示），
///     但**不需要也不应拿到地址**——地址解析只发生在 B（单跳硬保证）。
///
/// Code Logic（这个结构做什么）:
///     五字段 camelCase 序列化；proto_version / capabilities 为 B 直连表中的 mDNS
///     非权威提示（A 侧真实调用前仍以 health 为准）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayPeerInfoDto {
    /// 目标设备 device_id。
    pub device_id: String,
    /// 目标设备显示名。
    pub device_name: String,
    /// 协议版本提示（mDNS 非权威）。
    pub proto_version: u32,
    /// 能力清单提示（mDNS 非权威，可能被 TXT 上限裁剪）。
    pub capabilities: Vec<String>,
    /// B 报告的目标可达性。
    pub online: bool,
}

/// GET /api/relay/peers：报告 B 直连可见、可被中转访问的在线设备清单。
///
/// Business Logic（为什么需要这个函数）:
///     A 侧跳板探测任务周期调用本端点，把返回内容合成本地影子设备（不污染 mDNS
///     直连表）；列表只含 online 且非 B 自身的设备，不含地址信息。
///
/// Code Logic（这个函数做什么）:
///     relay.enabled=false → 503 `relay_disabled` 信封；否则读 `state.devices`
///     过滤（online && id != 本机 device_id），按 device_id 排序保证稳定输出，
///     返回 `Json<Vec<RelayPeerInfoDto>>`。
pub async fn relay_peers(
    State(state): State<AppState>,
    Extension(context): Extension<P2pRequestContext>,
) -> Response {
    if !relay_enabled(&state) {
        return relay_disabled_error(&context).into_response();
    }
    let self_id = state.device_id.as_str();
    let mut peers: Vec<RelayPeerInfoDto> = state
        .devices
        .read()
        .expect("devices 读锁中毒")
        .values()
        .filter(|device| device.online && device.id != self_id)
        .map(|device| RelayPeerInfoDto {
            device_id: device.id.clone(),
            device_name: device.name.clone(),
            proto_version: device.proto_version,
            capabilities: device.capabilities.clone(),
            online: device.online,
        })
        .collect();
    peers.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    Json(peers).into_response()
}

/// ANY /api/relay/:device_id/*path：把请求透明转发到目标设备。
///
/// Business Logic（为什么需要这个函数）:
///     A 对 C 的全部白名单业务调用（项目/文件/Git/事件流等）都走这一条通用转发；
///     B 不解析 body，方法/query/端到端 headers 原样透传，C 零感知。
///
/// Code Logic（这个函数做什么）:
///     依次检查 enabled → 白名单 → 目标解析 → 并发许可（任一失败返回对应 `relay_*`
///     信封）；通过后拆出 method/query/headers/body 委托 `forward_relay_request`
///     流式转发。许可 guard 绑定响应流生命周期（NDJSON 长流期间保持占用）。
pub async fn relay_forward(
    State(state): State<AppState>,
    AxumPath((device_id, path)): AxumPath<(String, String)>,
    Extension(context): Extension<P2pRequestContext>,
    request: Request<Body>,
) -> Response {
    if !relay_enabled(&state) {
        return relay_disabled_error(&context).into_response();
    }
    // axum `*path` 通配提取的 tail 不含前导 `/`（如 `api/health`）；转发目标路径
    // 必须以 `/` 开头（`/api/health`），此处统一补齐（已是绝对形态则原样保留）。
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    if !is_relay_path_allowed(&path) {
        return relay_error(
            format!("中转路径不在白名单内: {path}"),
            "relay_path_not_allowed",
            StatusCode::FORBIDDEN,
            false,
            &context,
        )
        .into_response();
    }
    let target = match resolve_relay_target(&state, &device_id, &context) {
        Ok(target) => target,
        Err(error) => return error.into_response(),
    };
    let Some(permit) = state.relay.try_acquire(&device_id) else {
        return relay_busy_error(&context).into_response();
    };

    let method = request.method().clone();
    let query = request
        .uri()
        .path_and_query()
        .and_then(|path_and_query| path_and_query.query())
        .map(str::to_string);
    let headers = request.headers().clone();
    let body = request.into_body();
    forward_relay_request(
        &state,
        RelayForwardJob {
            permit,
            target,
            path,
            query,
            method,
            headers,
            body,
            context,
        },
    )
    .await
}

/// GET /api/relay/:device_id/api/workbench/terminal-input-stream：终端输入 WS 桥。
///
/// Business Logic（为什么需要这个函数）:
///     终端按键输入不能逐键走 HTTP；A 经 B 中转时需要 B 提供 WS server 入口并以
///     WS client 身份连 C 的终端输入流，双向透传帧（子协议 `cc-partner.terminal-input.v1`
///     原样协商），任一侧断开双侧关闭（重连由 A 侧既有机制负责）。
///
/// Code Logic（这个函数做什么）:
///     前置检查同 `relay_forward`（enabled / 目标解析 / 并发；内层路径固定在白名单内）；
///     收集入站 `sec-websocket-protocol`（逗号分隔多协议展开）→ 先连出站上游
///     （成功才继续，失败 → 置 offline + 502 `relay_target_unreachable`）→
///     校验上游选择的子协议 → 入站 `WebSocketUpgrade`（透传 C 选择的协议）→
///     `on_upgrade` 内桥接，permit 随 WS 会话存续（move 进闭包）。
pub async fn relay_terminal_ws(
    State(state): State<AppState>,
    AxumPath(device_id): AxumPath<String>,
    Extension(context): Extension<P2pRequestContext>,
    request: Request<Body>,
) -> Response {
    if !relay_enabled(&state) {
        return relay_disabled_error(&context).into_response();
    }
    if !is_relay_path_allowed(TERMINAL_INPUT_INNER_PATH) {
        return relay_error(
            "中转路径不在白名单内",
            "relay_path_not_allowed",
            StatusCode::FORBIDDEN,
            false,
            &context,
        )
        .into_response();
    }
    let target = match resolve_relay_target(&state, &device_id, &context) {
        Ok(target) => target,
        Err(error) => return error.into_response(),
    };
    let Some(permit) = state.relay.try_acquire(&device_id) else {
        return relay_busy_error(&context).into_response();
    };

    let (mut parts, _body) = request.into_parts();
    let inbound_protocols = collect_websocket_protocols(&parts.headers);
    let query = parts
        .uri
        .path_and_query()
        .and_then(|path_and_query| path_and_query.query())
        .map(str::to_string);

    let upstream_request = match build_relay_terminal_upstream_request(
        &target,
        &device_id,
        &inbound_protocols,
        query.as_deref(),
    ) {
        Ok(request) => request,
        Err(error) => {
            return relay_error(
                error,
                "relay_target_unreachable",
                StatusCode::BAD_GATEWAY,
                true,
                &context,
            )
            .into_response();
        }
    };
    let (upstream, upstream_response) = match connect_async(upstream_request).await {
        Ok(pair) => pair,
        Err(error) => {
            tracing::warn!("relay 终端 WS 桥连接上游失败 (device={device_id}): {error}");
            crate::net::relay::mark_relay_target_offline(&state, &device_id);
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
    // 上游选择的子协议：透传给入站 upgrade，保证 A 与 C 协商结果一致。
    let selected_protocol = upstream_response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => {
            return relay_error(
                format!("终端输入中转 WS upgrade 请求无效: {rejection}"),
                "relay_target_unreachable",
                StatusCode::BAD_REQUEST,
                false,
                &context,
            )
            .into_response();
        }
    };
    let upgrade = match selected_protocol {
        Some(protocol) => upgrade.protocols([protocol]),
        None => upgrade,
    };
    upgrade
        .on_upgrade(move |socket| async move {
            // permit move 进会话闭包：WS 存续期间保持占用 per-target/全局名额。
            let _permit = permit;
            bridge_relay_websocket(socket, upstream).await;
        })
        .into_response()
}

/// 解析入站 WS 子协议列表（逗号分隔 + 多 header 展开）。
///
/// Business Logic（为什么需要这个函数）:
///     A 侧客户端可能以单 header 逗号分隔或多 header 形式请求多个子协议；
///     逐个透传给上游才能保真协商（C 端 axum `protocols()` 从中挑选）。
///
/// Code Logic（这个函数做什么）:
///     收集 `sec-websocket-protocol` 全部值，按逗号切分并 trim，过滤空串。
fn collect_websocket_protocols(headers: &HeaderMap) -> Vec<String> {
    let mut protocols = Vec::new();
    for value in headers.get_all("sec-websocket-protocol") {
        if let Ok(text) = value.to_str() {
            for protocol in text.split(',') {
                let protocol = protocol.trim();
                if !protocol.is_empty() {
                    protocols.push(protocol.to_string());
                }
            }
        }
    }
    protocols
}

/// relay 关闭时的统一 503 信封。
///
/// Business Logic（为什么需要这个函数）:
///     `enabled=false` 是运维显式关闭（不是故障），三个 handler 共用同一文案/code，
///     避免漂移；A 侧据此提示"跳板已关闭中转"而不是误判为网络故障。
///
/// Code Logic（这个函数做什么）:
///     `P2pError::stable` 构造 503 `relay_disabled`（retryable=true，等待重新开启）。
fn relay_disabled_error(context: &P2pRequestContext) -> P2pError {
    relay_error(
        "本机已关闭中转访问（relay.enabled=false）",
        "relay_disabled",
        StatusCode::SERVICE_UNAVAILABLE,
        true,
        context,
    )
}

/// 并发超限时的统一 503 信封。
///
/// Business Logic（为什么需要这个函数）:
///     B 被并发转发打满（全局 8 / 单目标 4）时 fail-fast 拒绝，调用方退避重试；
///     与 disabled 区分开 code，A 侧文案区分"跳板忙"与"跳板关闭"。
///
/// Code Logic（这个函数做什么）:
///     `P2pError::stable` 构造 503 `relay_busy`（retryable=true）。
fn relay_busy_error(context: &P2pRequestContext) -> P2pError {
    relay_error(
        format!(
            "中转并发已达上限（全局或单目标 {}），请稍后重试",
            RELAY_PER_TARGET_MAX_CONCURRENCY
        ),
        "relay_busy",
        StatusCode::SERVICE_UNAVAILABLE,
        true,
        context,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, InternalClaudeConfig,
        OrchestratorAutomationConfig, RelayConfig,
    };
    use crate::models::device::Device;
    use crate::net::error_response::envelope_fallback_middleware;
    use crate::net::lan_guard::{
        browser_guard_params, browser_request_guard_with_params, expected_device_id_guard,
        lan_socket_gate,
    };
    use crate::net::relay::RelayRuntime;
    use crate::net::request_context::request_id_middleware;
    use crate::workbench::terminal_input::TERMINAL_INPUT_SUBPROTOCOL;
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::routing::{any, get};
    use axum::Router;
    use chrono::Utc;
    use futures_util::{SinkExt, StreamExt};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;

    /// 集成测试常量：relay 测试 B 机 device_id 与目标 C 的 device_id。
    const TEST_RELAY_DEVICE_ID: &str = "relay-host-B";
    const TEST_TARGET_DEVICE_ID: &str = "target-device-C";

    /// 构造 relay 测试用最小 AppState（参照 mobile_transfer.rs 的 build_test_state 模式）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     relay handler 依赖完整 AppState（config/devices/relay runtime），集成测试
    ///     需要一份与生产同构但隔离（内存 SQLite + 临时目录）的状态。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存 SQLite pool + 最小 repo 集合 + 可注入的 relay 配置；devices 表由调用方写入。
    async fn build_relay_test_state(relay: RelayConfig) -> AppState {
        let dir =
            std::env::temp_dir().join(format!("cc-partner-relay-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let config = AppConfig {
            device_id: TEST_RELAY_DEVICE_ID.to_string(),
            device_name: "relay-test-host".to_string(),
            http_port: 0,
            receive_dir: dir.join("receive").to_string_lossy().to_string(),
            game_plugin_dir: "/tmp/plugins".to_string(),
            db_path: dir.join("data.db").to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_optimizer_provider: "claude".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
            relay,
            experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
        };
        let store = Arc::new(crate::config_store::MemoryConfigStore::with_config(
            config.clone(),
        ));
        let config_runtime = Arc::new(crate::config_runtime::ConfigRuntime::new(config, store));
        let config = config_runtime.shared_value();

        AppState {
            config,
            config_runtime,
            db: pool.clone(),
            maintenance_gate: Arc::new(crate::storage::DatabaseMaintenanceGate::new()),
            prompt_repo: Arc::new(crate::storage::PromptRepo::new(pool.clone())),
            attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
            transfer_repo: Arc::new(crate::storage::TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(crate::storage::ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(crate::storage::ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(crate::storage::SshTargetRepo::new(pool.clone())),
            device_id: Arc::new(TEST_RELAY_DEVICE_ID.to_string()),
            devices: Arc::new(RwLock::new(HashMap::new())),
            actual_http_port: Arc::new(std::sync::atomic::AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(crate::net::peer_client::PeerClient::new()),
            transfers: Arc::new(crate::transfer::registry::TransferRegistry::new()),
            ui: Arc::new(crate::backend::ui::HeadlessBackendUi::new(dir.join("dist"))),
            update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
            cc_history_repo: Arc::new(crate::storage::ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(crate::storage::WorkbenchProjectRepo::new(
                pool.clone(),
            )),
            workbench_session_repo: Arc::new(crate::storage::WorkbenchSessionRepo::new(
                pool.clone(),
            )),
            workbench_agent_session_repo: Arc::new(crate::storage::WorkbenchAgentSessionRepo::new(
                pool.clone(),
            )),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(crate::storage::WorkbenchWorktreeRepo::new(
                pool.clone(),
            )),
            workbench_browser_repo: Arc::new(crate::storage::WorkbenchBrowserRepo::new(
                pool.clone(),
            )),
            workbench_workspace_layout_repo: Arc::new(
                crate::storage::WorkbenchWorkspaceLayoutRepo::new(pool.clone()),
            ),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::env::temp_dir().join("cc-partner-bv-relay-test"),
                    "test-owner".into(),
                )
                .expect("browser verification test service"),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
            workbench_remote_event_bridges: Arc::new(
                crate::workbench::remote_events::RemoteEventBridgeRegistry::new(),
            ),
            workbench_dependency: Arc::new(
                crate::workbench::dependencies::WorkbenchDependencyInstallRuntime::new(),
            ),
            cc_collector_cancel: Arc::new(Mutex::new(None)),
            cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(crate::orchestrator::repo::OrchestratorRepo::new(pool)),
            orchestrator_scheduler_telemetry:
                crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry::new(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
            workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(HashMap::new())),
            runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
            runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
            event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
                "relay-test-owner",
            )),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
            relay: Arc::new(RelayRuntime::new()),
        }
    }

    /// 构造一条直连表 Device 记录。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     relay 转发目标解析读 devices 表；测试需要可控的 host/port/online 组合。
    fn test_device(id: &str, host: &str, port: u16, online: bool) -> Device {
        Device {
            id: id.to_string(),
            name: format!("device-{id}"),
            host: host.to_string(),
            port,
            last_seen: Utc::now(),
            online,
            proto_version: 1,
            capabilities: vec!["workbench.projects.v1".to_string()],
        }
    }

    /// 把目标设备写入（或移出）B 的直连表。
    fn seed_device(state: &AppState, device: Device) {
        state
            .devices
            .write()
            .unwrap()
            .insert(device.id.clone(), device);
    }

    /// 启动 mock 目标设备 C：echo 收到的 method/path/query/header。
    ///
    /// Business Logic（为什么需要这个测试函数）:
    ///     真实转发集成测试需要断言"透传语义"——目标实际收到了什么方法/路径/端到端
    ///     header；mock 把证据回显到响应 JSON 里。
    ///
    /// Code Logic（这个函数做什么）:
    ///     绑定 127.0.0.1:0 的 axum 实例；`ANY /api/workbench/*path` 与
    ///     `GET /api/health` 返回 `{method, path, query, requestId, expectedDeviceId,
    ///     contentType, body}`；返回 (base_url, join_handle)。
    async fn spawn_mock_target() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock target listener");
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route(
            "/api/health",
            get(|| async {
                axum::Json(serde_json::json!({"ok": true, "route": "health"}))
            }),
        )
        .route(
            "/api/workbench/*path",
            any(
                |method: axum::http::Method,
                 AxumPath(path): AxumPath<String>,
                 axum::extract::RawQuery(query): axum::extract::RawQuery,
                 headers: axum::http::HeaderMap,
                 body: axum::body::Bytes| async move {
                    axum::Json(serde_json::json!({
                        "method": method.as_str(),
                        "path": path,
                        "query": query,
                        "requestId": headers.get("x-cc-request-id").and_then(|v| v.to_str().ok()),
                        "expectedDeviceId": headers.get("x-cc-partner-expected-device-id").and_then(|v| v.to_str().ok()),
                        "contentType": headers.get("content-type").and_then(|v| v.to_str().ok()),
                        "host": headers.get("host").and_then(|v| v.to_str().ok()),
                        "contentLength": headers.get("content-length").and_then(|v| v.to_str().ok()),
                        "body": String::from_utf8_lossy(&body),
                    }))
                },
            ),
        );
        let join = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("mock target serve");
        });
        (format!("http://127.0.0.1:{port}"), join)
    }

    /// 构造与生产 middleware 顺序一致的 relay 测试 Router。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     relay 路由的边界行为（guard 的 relay 分支、错误信封、body limit）必须在
    ///     真实中间件栈下验证，避免"裸 handler 通过、装配后失败"。
    ///
    /// Code Logic（这个函数做什么）:
    ///     注册三条 relay 路由（与 http_server.rs 字面量一致）+ 全套中间件
    ///     （request_id → lan_socket_gate → browser_guard → expected_device_id →
    ///     envelope → body limit），`with_state(AppState)`。
    fn relay_test_router(state: AppState, port: u16) -> Router {
        let params = browser_guard_params(TEST_RELAY_DEVICE_ID, port);
        Router::new()
            .route("/api/relay/peers", get(relay_peers))
            .route(
                "/api/relay/:device_id/api/workbench/terminal-input-stream",
                get(relay_terminal_ws),
            )
            .route("/api/relay/:device_id/*path", any(relay_forward))
            .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024))
            .layer(axum::middleware::from_fn(envelope_fallback_middleware))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                expected_device_id_guard,
            ))
            .layer(axum::middleware::from_fn(move |req, next| {
                let params = params.clone();
                async move { browser_request_guard_with_params(params, req, next).await }
            }))
            .layer(axum::middleware::from_fn(lan_socket_gate))
            .layer(axum::middleware::from_fn(request_id_middleware))
            .with_state(state)
    }

    /// 启动挂 relay 路由的 B 实例（真实 TCP），返回 base_url 与 AppState 引用。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     集成测试用真实 socket 驱动完整中间件栈（ConnectInfo 才有真实 peer）。
    async fn spawn_relay_server(
        relay: RelayConfig,
    ) -> (String, AppState, tokio::task::JoinHandle<()>) {
        let state = build_relay_test_state(relay).await;
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind relay listener");
        let port = listener.local_addr().unwrap().port();
        state
            .actual_http_port
            .store(port, std::sync::atomic::Ordering::SeqCst);
        let app = relay_test_router(state.clone(), port);
        let join = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("relay test serve");
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        (format!("http://127.0.0.1:{port}"), state, join)
    }

    /// 解析错误信封 JSON 并断言 code。
    fn assert_envelope(expected_code: &str, body_text: &str) {
        let value: serde_json::Value = serde_json::from_str(body_text)
            .unwrap_or_else(|error| panic!("错误 body 应为 JSON 信封: {error}, body={body_text}"));
        assert_eq!(value["code"], expected_code, "body={body_text}");
        assert!(
            value["request_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "信封应携带 request_id, body={body_text}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     端到端核心价值验证：A 经 B 转发后 C 收到的是同方法/同路径/同 query 的普通
    ///     请求，端到端 header（request_id / expected-device / content-type）透传、
    ///     hop-by-hop（host / content-length）被剥除、body 原样到达。
    ///
    /// Code Logic（这个测试做什么）:
    ///     起 mock C + relay B（devices 注入 C）；POST
    ///     `/api/relay/{C}/api/workbench/projects/open?a=1`（JSON body + 两个端到端
    ///     header），断言 200 + 回显 JSON 的每个字段。
    #[tokio::test]
    async fn relay_forward_transparently_forwards_to_target() {
        let (target_base, _target_join) = spawn_mock_target().await;
        let target_port: u16 = target_base.rsplit(':').next().unwrap().parse().unwrap();
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", target_port, true),
        );

        let client = reqwest::Client::new();
        let response = client
            .post(format!(
                "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/workbench/projects/open?a=1"
            ))
            .header("X-CC-Request-Id", "relay-e2e-req-1")
            .header(
                crate::net::lan_guard::EXPECTED_DEVICE_ID_HEADER.clone(),
                TEST_TARGET_DEVICE_ID,
            )
            .header("Content-Type", "application/json")
            .body(r#"{"path":"/tmp/demo"}"#)
            .send()
            .await
            .expect("转发请求应成功");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-cc-request-id").unwrap(),
            "relay-e2e-req-1",
            "响应 request_id 应与入站一致（全链同一 ID）"
        );
        let echo: serde_json::Value = response.json().await.unwrap();
        assert_eq!(echo["method"], "POST");
        assert_eq!(echo["path"], "projects/open");
        assert_eq!(echo["query"], "a=1");
        assert_eq!(echo["requestId"], "relay-e2e-req-1");
        assert_eq!(echo["expectedDeviceId"], TEST_TARGET_DEVICE_ID);
        assert_eq!(echo["contentType"], "application/json");
        // Host 由出站客户端按目标 URL 重新生成：若错误透传入站 Host，mock 会看到 B 的地址而非自己的。
        assert_eq!(echo["host"], format!("127.0.0.1:{target_port}"));
        // content-length（若存在）是出站传输层按 body 重新计算的 fresh 值。
        assert_eq!(echo["contentLength"], serde_json::Value::Null);
        assert_eq!(echo["body"], r#"{"path":"/tmp/demo"}"#);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     白名单外的路径（sync/transfer/mobile/backend control）必须 403 信封拒绝，
    ///     防止双向同步类流量进入中转拓扑。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对在线 target 请求 `/api/relay/{C}/api/sync/pull`，断言 403 + code
    ///     `relay_path_not_allowed`。
    #[tokio::test]
    async fn relay_forward_rejects_path_outside_whitelist() {
        let (target_base, _target_join) = spawn_mock_target().await;
        let target_port: u16 = target_base.rsplit(':').next().unwrap().parse().unwrap();
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", target_port, true),
        );

        for path in [
            "/api/sync/pull",
            "/api/transfer/init",
            "/api/mobile/attention",
        ] {
            let response = reqwest::get(format!(
                "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}{path}"
            ))
            .await
            .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "path={path}");
            let body = response.text().await.unwrap();
            assert_envelope("relay_path_not_allowed", &body);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     目标不在直连表 / offline / 等于本机（自引用）都必须 fail-closed 404
    ///     `relay_target_offline`；自引用防护是多跳环路的结构保证之一。
    ///
    /// Code Logic（这个测试做什么）:
    ///     分别请求未知 device_id、offline 设备、B 自身 device_id，断言 404 信封。
    #[tokio::test]
    async fn relay_forward_rejects_unknown_offline_and_self_targets() {
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", 1, false),
        );

        let client = reqwest::Client::new();
        for device_id in [
            "no-such-device",
            TEST_TARGET_DEVICE_ID,
            TEST_RELAY_DEVICE_ID,
        ] {
            let response = client
                .get(format!("{relay_base}/api/relay/{device_id}/api/health"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "device={device_id}"
            );
            let body = response.text().await.unwrap();
            assert_envelope("relay_target_offline", &body);
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `relay.enabled=false` 时路由仍注册但必须拒绝（热生效语义），错误 code
    ///     `relay_disabled` 让 A 侧区分"跳板关闭"与网络故障。
    ///
    /// Code Logic（这个测试做什么）:
    ///     enabled=false 构造 B，请求转发与 peers 两个端点，断言 503 信封。
    #[tokio::test]
    async fn relay_rejects_all_routes_when_disabled() {
        let (relay_base, _state, _relay_join) = spawn_relay_server(RelayConfig {
            enabled: false,
            ..RelayConfig::default()
        })
        .await;

        let client = reqwest::Client::new();
        let forward = client
            .get(format!(
                "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/health"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(forward.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = forward.text().await.unwrap();
        assert_envelope("relay_disabled", &body);

        let peers = client
            .get(format!("{relay_base}/api/relay/peers"))
            .send()
            .await
            .unwrap();
        assert_eq!(peers.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = peers.text().await.unwrap();
        assert_envelope("relay_disabled", &body);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     并发上限打满时必须立即 503 `relay_busy`（fail-fast，不排队），防止 B 被
    ///     当作流量放大器；释放后应恢复。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接占满 relay runtime 的全局 8 个许可，请求转发断言 503 信封；drop 后恢复 200。
    #[tokio::test]
    async fn relay_returns_busy_when_concurrency_saturated() {
        let (target_base, _target_join) = spawn_mock_target().await;
        let target_port: u16 = target_base.rsplit(':').next().unwrap().parse().unwrap();
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", target_port, true),
        );

        // 打满全局 8 个许可（不同 device_id 也占全局额度）。
        let guards: Vec<_> = (0..8)
            .map(|i| {
                state
                    .relay
                    .try_acquire(&format!("saturate-{i}"))
                    .expect("饱和前应可获取")
            })
            .collect();
        let response = reqwest::get(format!(
            "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/health"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.text().await.unwrap();
        assert_envelope("relay_busy", &body);
        drop(guards);

        let response = reqwest::get(format!(
            "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/health"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     连接目标失败（目标端口无人监听）必须 502 `relay_target_unreachable`，
    ///     且顺带把直连表该 target 置 offline（加速收敛）——下一个请求直接 404
    ///     `relay_target_offline`。
    ///
    /// Code Logic（这个测试做什么）:
    ///     devices 注入指向死端口的 target；第一次转发断言 502 信封；再断言 devices
    ///     表 online=false 与第二次请求 404。
    #[tokio::test]
    async fn relay_marks_target_offline_after_unreachable() {
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        // 绑一个 listener 拿空闲端口后立刻 drop，制造"端口无人监听"。
        let dead_port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", dead_port, true),
        );

        let response = reqwest::get(format!(
            "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/health"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response.text().await.unwrap();
        assert_envelope("relay_target_unreachable", &body);

        assert!(
            !state
                .devices
                .read()
                .unwrap()
                .get(TEST_TARGET_DEVICE_ID)
                .unwrap()
                .online,
            "连接失败后应把 target 置 offline"
        );
        let second = reqwest::get(format!(
            "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/health"
        ))
        .await
        .unwrap();
        assert_eq!(second.status(), StatusCode::NOT_FOUND);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     `/api/relay/peers` 是静态路由，必须优先于 `:device_id/*path` 通配命中
    ///     （否则 A 的探测请求会被当作"转发到设备 peers"）；返回内容 = online 且
    ///     非本机的直连设备（camelCase DTO）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     devices 注入 online C / offline D / 本机自身三条记录；GET peers 断言
    ///     200 + 仅 C 一条 + 字段名/值正确。
    #[tokio::test]
    async fn relay_peers_lists_online_non_self_devices() {
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", 62116, true),
        );
        seed_device(
            &state,
            test_device("device-offline", "127.0.0.1", 62116, false),
        );
        seed_device(
            &state,
            test_device(TEST_RELAY_DEVICE_ID, "127.0.0.1", 62116, true),
        );

        let response = reqwest::get(format!("{relay_base}/api/relay/peers"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        let peers = body.as_array().expect("peers 应返回数组");
        assert_eq!(peers.len(), 1, "仅 online 非本机设备, body={peers:?}");
        assert_eq!(peers[0]["deviceId"], TEST_TARGET_DEVICE_ID);
        assert_eq!(
            peers[0]["deviceName"],
            format!("device-{TEST_TARGET_DEVICE_ID}")
        );
        assert_eq!(peers[0]["protoVersion"], 1);
        assert_eq!(peers[0]["online"], true);
        assert!(peers[0]["capabilities"].as_array().unwrap().len() == 1);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     terminal-input-stream 的精确路由必须优先于 `*path` 通配命中 WS handler
    ///     （设计 §12 明确要求覆盖）：普通 GET（非 WS upgrade）打到该路径时走
    ///     `relay_terminal_ws`（先连上游 WS），上游是普通 HTTP mock 时握手失败返回
    ///     502 `relay_target_unreachable`；若被通配吞掉则会得到 mock 的 200 JSON。
    ///
    /// Code Logic（这个测试做什么）:
    ///     起 HTTP mock target，普通 GET 请求
    ///     `/api/relay/{C}/api/workbench/terminal-input-stream`，断言 502（而非 200）。
    #[tokio::test]
    async fn terminal_input_stream_exact_route_beats_wildcard_forward() {
        let (target_base, _target_join) = spawn_mock_target().await;
        let target_port: u16 = target_base.rsplit(':').next().unwrap().parse().unwrap();
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", target_port, true),
        );

        let response = reqwest::get(format!(
            "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/workbench/terminal-input-stream"
        ))
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "精确 WS 路由应先尝试上游 WS 握手（mock 为 HTTP → 502），而不是通配转发得到 200"
        );
        let body = response.text().await.unwrap();
        assert_envelope("relay_target_unreachable", &body);
    }

    /// Business Logic（为什么需要这个测试）:
    ///     终端输入 WS 桥是中转链路里唯一的常驻双向通道：需要证明 A（tungstenite
    ///     client）经 B 的 WS 路由与 C（axum WS echo + 子协议协商）之间帧双向透传、
    ///     子协议 `cc-partner.terminal-input.v1` 端到端协商一致、会话结束后 B 的
    ///     并发名额归还。
    ///
    /// Code Logic（这个测试做什么）:
    ///     起 C = axum WS echo（协商 terminal 子协议，回显文本帧）；B = relay 路由 +
    ///     devices 注入 C；A 用 tungstenite 连 B 的
    ///     `/api/relay/{C}/api/workbench/terminal-input-stream`（带子协议 + upgrade 头），
    ///     发两条文本帧断言回显一致；断言响应协商出同一子协议；关闭后轮询 B 的
    ///     active_forwards 归零。
    #[tokio::test]
    async fn relay_terminal_ws_bridges_both_directions() {
        // C：WS echo 目标（子协议协商 + 文本回显）。
        let target_listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind ws target");
        let target_port = target_listener.local_addr().unwrap().port();
        let target_app = Router::new().route(
            "/api/workbench/terminal-input-stream",
            get(|ws: WebSocketUpgrade| async move {
                ws.protocols([TERMINAL_INPUT_SUBPROTOCOL])
                    .on_upgrade(|socket| async move {
                        let (mut sender, mut receiver) = socket.split();
                        while let Some(Ok(message)) = receiver.next().await {
                            if let Message::Text(text) = message {
                                if sender.send(Message::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    })
            }),
        );
        let _target_join = tokio::spawn(async move {
            axum::serve(target_listener, target_app.into_make_service())
                .await
                .expect("ws target serve");
        });

        // B：relay 实例。
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", target_port, true),
        );

        // A：tungstenite 客户端（带子协议 + WS upgrade 头）。
        let ws_url = format!(
            "ws://127.0.0.1:{port}/api/relay/{TEST_TARGET_DEVICE_ID}/api/workbench/terminal-input-stream",
            port = relay_base.rsplit(':').next().unwrap()
        );
        let mut request = ws_url.into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            TERMINAL_INPUT_SUBPROTOCOL.parse().unwrap(),
        );
        let (mut socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("经 relay 的终端 WS 应连接成功");
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            TERMINAL_INPUT_SUBPROTOCOL,
            "子协议应端到端协商一致"
        );

        for payload in [
            "{\"type\":\"input\",\"data\":\"ls\"}",
            "{\"type\":\"input\",\"data\":\"pwd\"}",
        ] {
            socket
                .send(TungsteniteMessage::Text(payload.to_string()))
                .await
                .unwrap();
            let echoed = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("回显不应超时")
                .unwrap()
                .unwrap();
            match echoed {
                TungsteniteMessage::Text(text) => assert_eq!(text, payload),
                other => panic!("应回显文本帧, 实际: {other:?}"),
            }
        }
        socket.close(None).await.unwrap();
        // 会话结束后并发名额应归还（WS 关闭传播可能略滞后，轮询等待）。
        for _ in 0..50 {
            if state.relay.active_forwards() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            state.relay.active_forwards(),
            0,
            "WS 会话结束后应释放并发名额"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     WS 桥同样受并发上限约束：打满全局许可后 WS 路由应 503 `relay_busy`
    ///     而不是绕过闸门。
    ///
    /// Code Logic（这个测试做什么）:
    ///     占满 8 个全局许可后用 HTTP GET（带 upgrade 头）打 WS 精确路径，断言
    ///     503 信封。
    #[tokio::test]
    async fn relay_terminal_ws_returns_busy_when_saturated() {
        let (relay_base, state, _relay_join) = spawn_relay_server(RelayConfig::default()).await;
        seed_device(
            &state,
            test_device(TEST_TARGET_DEVICE_ID, "127.0.0.1", 62116, true),
        );
        let _guards: Vec<_> = (0..8)
            .map(|i| {
                state
                    .relay
                    .try_acquire(&format!("saturate-ws-{i}"))
                    .expect("饱和前应可获取")
            })
            .collect();

        let response = reqwest::Client::new()
            .get(format!(
                "{relay_base}/api/relay/{TEST_TARGET_DEVICE_ID}/api/workbench/terminal-input-stream"
            ))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.text().await.unwrap();
        assert_envelope("relay_busy", &body);
    }
}
