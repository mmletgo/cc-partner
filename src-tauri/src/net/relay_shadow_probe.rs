//! net/relay_shadow_probe.rs — A 侧影子设备周期探测（中转访问/跳板机发起方）
//!
//! Business Logic（为什么需要这个模块）:
//!     A 配置了跳板（`relay.via_device_ids`）后，需要周期拉取每个跳板的
//!     `GET /api/relay/peers` 报告，把"经跳板可见的目标"合成为影子设备写入
//!     `RelayRuntime.shadow_devices`，设备列表与 `device_base_url` 三段解析据此
//!     把对 C 的出站链路路由成 `http://{B}/api/relay/{C}`。探测节奏与 manual_peers
//!     同为 15s：配置热生效（每轮重读内存配置）、跳板掉线整批下线、跳板从配置
//!     移除则清理其名下影子。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_relay_shadow_probe`：spawn 周期循环，返回取消令牌；令牌存放在模块级
//!       槽位（仿 `wordgame` 先例，避免给 AppState 加字段波及全部装配点）。
//!     - `probe_cycle`：读配置 via 集合 → 与影子表现有 via 集合 diff（移除的清理）→
//!       逐个 via：不在直连表或 offline → `mark_via_offline`；在线 → 拉取
//!       `/api/relay/peers`（Health 3s 超时），成功整批替换影子、失败（含老版本 B
//!       无此路由的 404）整批下线。
//!     - 影子写入经 `net::relay_shadow::replace_shadows_for_via`，排除规则
//!       （非本机、不与直连重复、跨 via 先到先得）集中在该函数。

use crate::models::device::Device;
use crate::net::peer_timeout::PeerTimeoutClass;
use crate::net::relay_shadow::{mark_via_offline, remove_via, replace_shadows_for_via};
use crate::net::request_context::{new_request_id, REQUEST_ID_HEADER};
use crate::state::AppState;
use chrono::Utc;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// 影子探测周期（秒）。与 manual_peers 同节奏：配置热生效与跳板状态收敛的最长滞后。
const PROBE_INTERVAL_SECS: u64 = 15;

/// 供 shutdown 取消的运行时令牌槽位。
///
/// Business Logic（为什么放模块级而不是 AppState）:
///     与 `wordgame::WORDGAME_RUNTIME_CANCEL` 同理：新增 AppState 字段会波及全部
///     装配点（生产 + 十余处测试构造）；模块级 OnceLock 槽位配合
///     `start_cancelled_task_once` / `cancel_runtime_token` 即可对齐
///     manual_peer_cancel 的「启动一次 + 退出取消」生命周期。
pub static RELAY_SHADOW_PROBE_CANCEL: OnceLock<Mutex<Option<CancellationToken>>> = OnceLock::new();

/// 返回模块级取消令牌槽位（供 runtime 用统一的 once-start / cancel 机制管理）。
pub fn relay_shadow_probe_cancel_slot() -> &'static Mutex<Option<CancellationToken>> {
    RELAY_SHADOW_PROBE_CANCEL.get_or_init(|| Mutex::new(None))
}

/// 启动影子设备周期探测循环，返回取消令牌（shutdown 时 cancel）。
///
/// Business Logic（为什么需要这个函数）:
///     影子表必须有唯一写入方持续刷新，否则设备列表与路由解析会基于陈旧的跳板报告
///     （目标已下线/跳板已撤销信任仍显示可用）。周期循环是这份新鲜度的保证。
///
/// Code Logic（这个函数做什么）:
///     仿 `start_manual_peer_probe`：创建 CancellationToken，spawn 循环——每轮先跑
///     `probe_cycle` 再等 15s（cancel 与 sleep select，取消即退出）。
pub fn start_relay_shadow_probe(state: AppState) -> CancellationToken {
    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let state_clone = state.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            probe_cycle(&state_clone).await;
            tokio::select! {
                _ = cancel_clone.cancelled() => {
                    tracing::info!("relay 影子设备探测循环已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(PROBE_INTERVAL_SECS)) => {}
            }
        }
    });
    cancel
}

/// 单轮影子探测：配置 diff 清理 + 逐 via 探测。
///
/// Business Logic（为什么需要这个函数）:
///     用户撤销对某跳板的信任（`relay.via_device_ids` 移除）后，其名下影子必须被
///     清理而不是继续展示；仍在配置中的跳板则按最新报告刷新影子（热生效≤15s）。
///
/// Code Logic（这个函数做什么）:
///     1. 读 `config.relay.via_device_ids` 快照；
///     2. 影子表现有 via 集合中不在配置里的 → `remove_via` 清理；
///     3. 对每个配置 via 调 `probe_via`。锁内无 await，网络调用在锁外。
pub(crate) async fn probe_cycle(state: &AppState) {
    let configured: Vec<String> = state
        .config
        .read()
        .expect("config 读锁中毒")
        .relay
        .via_device_ids
        .clone();
    let configured_set: HashSet<String> = configured.iter().cloned().collect();

    let current_vias: Vec<String> = state
        .relay
        .shadow_devices
        .read()
        .expect("影子表读锁中毒")
        .values()
        .map(|shadow| shadow.via_device_id.clone())
        .collect();
    for via in current_vias {
        if !configured_set.contains(&via) {
            remove_via(state, &via);
            tracing::debug!("relay via 已从配置移除，清理其名下影子: {via}");
        }
    }

    for via in &configured {
        probe_via(state, via).await;
    }
}

/// 对单个跳板执行一轮影子探测。
///
/// Business Logic（为什么需要这个函数）:
///     影子 online 是复合语义（via 直连可达 && via 报告目标 online）；via 自身不
///     可达时无需发起网络调用即可整批下线，可达时以跳板最新报告为准整批替换。
///
/// Code Logic（这个函数做什么）:
///     via 不在直连表或 offline → `mark_via_offline`（跳过网络）；在线 →
///     `fetch_relay_peers` 拉 `/api/relay/peers`：成功把报告条目转成 Device
///     （host/port 填 via 的直连地址、online=true、last_seen=now）后
///     `replace_shadows_for_via` 整批替换；失败（含老版本 B 404）→ debug 日志 +
///     `mark_via_offline`。
async fn probe_via(state: &AppState, via_device_id: &str) {
    let via_device = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        devices
            .get(via_device_id)
            .filter(|device| device.online)
            .cloned()
    };
    let Some(via_device) = via_device else {
        mark_via_offline(state, via_device_id);
        return;
    };

    let url = format!("{}/api/relay/peers", via_device.base_url());
    match fetch_relay_peers(state, &url).await {
        Ok(peers) => {
            let now = Utc::now();
            let reported = peers
                .into_iter()
                .map(|peer| Device {
                    id: peer.device_id,
                    name: peer.device_name,
                    host: via_device.host.clone(),
                    port: via_device.port,
                    last_seen: now,
                    online: true,
                    proto_version: peer.proto_version,
                    capabilities: peer.capabilities,
                })
                .collect();
            replace_shadows_for_via(state, via_device_id, reported);
            tracing::debug!(
                via = via_device_id,
                "relay 影子探测成功，已按跳板报告刷新影子表"
            );
        }
        Err(error) => {
            tracing::debug!("relay 影子探测失败 (via={via_device_id}, {url}): {error}");
            mark_via_offline(state, via_device_id);
        }
    }
}

/// A 侧视角的跳板 `/api/relay/peers` 报告条目（camelCase 反序列化视图）。
///
/// Business Logic（为什么本地定义而不复用 B 端 DTO）:
///     B 端 `routes/relay.rs` 的 `RelayPeerInfoDto` 只 derive Serialize（响应侧）；
///     探测客户端需要 Deserialize（请求侧）视图，本地镜像定义避免给路由 DTO 补
///     derive 造成方向语义混淆。
///
/// Code Logic（这个结构做什么）:
///     camelCase 反序列化；B 端契约只上报 online 目标（`online` 字段恒为 true，
///     A 侧不消费，serde 对多余 JSON 字段默认忽略）；`proto_version`/`capabilities`
///     缺字段回退默认值（兼容 B 端字段演进）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayPeerReport {
    device_id: String,
    device_name: String,
    #[serde(default)]
    proto_version: u32,
    #[serde(default)]
    capabilities: Vec<String>,
}

/// 拉取跳板 `GET /api/relay/peers`（Health 类 3s 超时）。
///
/// Business Logic（为什么需要这个函数）:
///     探测是周期性尽力而为的遥测：失败必须快速返回（不阻塞整轮），且不能因老版本
///     跳板无此路由（404）产生噪音错误——统一折叠成 String 供 debug 日志与下线处理。
///
/// Code Logic（这个函数做什么）:
///     复用 `state.peer_client` 的共享 HTTP client（禁止每轮新建），按
///     `PeerTimeoutClass::Health` 设总超时并注入 request id；非 2xx（404/503 等）
///     与 JSON 解析失败都返回 Err。
async fn fetch_relay_peers(state: &AppState, url: &str) -> Result<Vec<RelayPeerReport>, String> {
    let response = state
        .peer_client
        .http_client()
        .get(url)
        .timeout(PeerTimeoutClass::Health.timeout())
        .header(REQUEST_ID_HEADER, new_request_id())
        .send()
        .await
        .map_err(|error| format!("网络失败: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    response
        .json::<Vec<RelayPeerReport>>()
        .await
        .map_err(|error| format!("响应解析失败: {error}"))
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 影子探测/影子表/设备列表合并单测共用的最小 AppState 构造。
    //!
    //! Business Logic（为什么集中在这里）:
    //!     `relay_shadow`、`relay_shadow_probe` 与 `commands::devices` 的单测都需要
    //!     一份与生产同构但隔离（内存 SQLite + 临时目录）的 AppState；三处各写一份
    //!     会漂移，集中一处供 cfg(test) 复用。

    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, InternalClaudeConfig,
        OrchestratorAutomationConfig, RelayConfig,
    };
    use crate::net::peer_client::PeerClient;
    use crate::state::AppState;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    /// 构造影子探测测试用最小 AppState。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存 SQLite pool + 最小 repo 集合 + 注入 `relay.via_device_ids`；
    ///     devices/影子表由调用方写入。`self_device_id` 为 A（本机）身份。
    pub(crate) async fn build_test_state(
        self_device_id: &str,
        via_device_ids: Vec<String>,
    ) -> AppState {
        let dir =
            std::env::temp_dir().join(format!("cc-partner-shadow-probe-{}", uuid::Uuid::new_v4()));
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
            device_id: self_device_id.to_string(),
            device_name: "shadow-probe-self".to_string(),
            http_port: 0,
            receive_dir: dir.join("receive").to_string_lossy().to_string(),
            game_plugin_dir: "/tmp/plugins".into(),
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
            relay: RelayConfig {
                enabled: true,
                via_device_ids,
                ignored_target_ids: Vec::new(),
            },
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
            device_id: Arc::new(self_device_id.to_string()),
            devices: Arc::new(RwLock::new(HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
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
                    std::env::temp_dir().join("cc-partner-bv-shadow-probe"),
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
                "shadow-probe-owner",
            )),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
            relay: Arc::new(crate::net::relay::RelayRuntime::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::build_test_state;
    use super::*;
    use crate::models::device::Device;
    use crate::net::relay_shadow::RelayShadowDevice;
    use axum::routing::get;
    use axum::Json;
    use axum::Router;
    use chrono::Utc;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// A（本机）/ 跳板 B / 目标 C/D 的固定测试身份。
    const SELF_ID: &str = "shadow-self-A";
    const VIA_B: &str = "relay-host-B";
    const TARGET_C: &str = "target-device-C";
    const TARGET_D: &str = "target-device-D";

    /// 构造一条可控 host/port/online 的直连表 Device。
    fn device(id: &str, host: &str, port: u16, online: bool) -> Device {
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

    /// 把 Device 写入 A 的直连表。
    fn seed_device(state: &AppState, entry: Device) {
        state
            .devices
            .write()
            .unwrap()
            .insert(entry.id.clone(), entry);
    }

    /// 直接向影子表预置一条条目（绕过探测，供下线/清理/先到先得场景播种）。
    fn seed_shadow(state: &AppState, via: &str, target: &str, online: bool) {
        state.relay.shadow_devices.write().unwrap().insert(
            target.to_string(),
            RelayShadowDevice {
                target_device_id: target.to_string(),
                via_device_id: via.to_string(),
                device_name: format!("device-{target}"),
                proto_version: 1,
                capabilities: Vec::new(),
                online,
                last_seen: Utc::now(),
            },
        );
    }

    /// 构造一条 `/api/relay/peers` 报告 JSON。
    fn report(device_id: &str) -> serde_json::Value {
        serde_json::json!({
            "deviceId": device_id,
            "deviceName": format!("device-{device_id}"),
            "protoVersion": 1,
            "capabilities": ["workbench.projects.v1"],
            "online": true,
        })
    }

    /// 启动 mock 跳板：`GET /api/relay/peers` 返回固定报告并统计命中次数。
    ///
    /// Business Logic（为什么需要这个测试函数）:
    ///     探测循环一轮的端到端验证需要真实 TCP 上的跳板响应（参照 routes/relay.rs
    ///     测试的 mock axum 方式），命中计数供"成功才发起调用"类断言。
    ///
    /// Code Logic（这个函数做什么）:
    ///     绑定 127.0.0.1:0 的 axum 实例，返回 (base_url, 命中计数, join_handle)。
    async fn spawn_mock_relay_peers(
        body: Vec<serde_json::Value>,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_route = hits.clone();
        let app = Router::new().route(
            "/api/relay/peers",
            get(move || {
                let hits = hits_for_route.clone();
                let body = body.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    Json(body)
                }
            }),
        );
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock relay peers listener");
        let port = listener.local_addr().unwrap().port();
        let join = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("mock relay peers serve");
        });
        (format!("http://127.0.0.1:{port}"), hits, join)
    }

    /// 从 mock base_url 解析端口（固定 `http://127.0.0.1:{port}` 形态）。
    fn port_of(base_url: &str) -> u16 {
        base_url.rsplit(':').next().unwrap().parse().unwrap()
    }

    /// Business Logic（为什么需要这个测试）:
    ///     探测循环核心价值：via 在线时拉取 `/api/relay/peers` 并把报告正确合成为
    ///     影子条目（via 名下整批、online=true、设备名取跳板转述）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock 跳板返回 C/D 两条报告；A 配置 via=B 且 B 在线指向 mock；跑一轮
    ///     `probe_cycle`，断言影子表两条、via/online/device_name 正确、命中计数=1。
    #[tokio::test]
    async fn probe_cycle_writes_shadows_from_relay_peers_report() {
        let (relay_base, hits, _join) = spawn_mock_relay_peers(vec![
            serde_json::json!({
                "deviceId": TARGET_C,
                "deviceName": "目标 C",
                "protoVersion": 1,
                "capabilities": ["workbench.projects.v1"],
                "online": true,
            }),
            report(TARGET_D),
        ])
        .await;
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string()]).await;
        seed_device(
            &state,
            device(VIA_B, "127.0.0.1", port_of(&relay_base), true),
        );

        probe_cycle(&state).await;

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(hits.load(Ordering::SeqCst), 1, "应恰好探测一次");
        assert_eq!(shadows.len(), 2, "报告两条应合成两条影子: {shadows:?}");
        for target in [TARGET_C, TARGET_D] {
            let shadow = shadows.get(target).expect("影子条目应存在");
            assert_eq!(shadow.via_device_id, VIA_B);
            assert!(shadow.online, "成功报告后影子应 online");
        }
        assert_eq!(shadows.get(TARGET_C).unwrap().device_name, "目标 C");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     via 在直连表但 offline 时不得发起网络调用，且必须把其名下影子整批下线
    ///     （复合 online 语义的 via 侧收敛）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     via=B 在表但 online=false 且指向 mock；预置该 via 名下 online 影子；
    ///     跑一轮 probe_cycle，断言影子 online=false 且 mock 命中计数=0。
    #[tokio::test]
    async fn probe_cycle_marks_shadows_offline_when_via_device_offline() {
        let (relay_base, hits, _join) = spawn_mock_relay_peers(vec![report(TARGET_C)]).await;
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string()]).await;
        seed_device(
            &state,
            device(VIA_B, "127.0.0.1", port_of(&relay_base), false),
        );
        seed_shadow(&state, VIA_B, TARGET_C, true);

        probe_cycle(&state).await;

        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "via offline 不应发起网络调用"
        );
        assert!(
            !state
                .relay
                .shadow_devices
                .read()
                .unwrap()
                .get(TARGET_C)
                .unwrap()
                .online,
            "via offline 时影子应整批下线"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     via 完全不在直连表（mDNS 消失后未清理配置）时同样整批下线且不发网络调用。
    ///
    /// Code Logic（这个测试做什么）:
    ///     配置含 via=B 但 devices 表为空；预置 online 影子；跑一轮断言下线、零命中。
    #[tokio::test]
    async fn probe_cycle_marks_shadows_offline_when_via_missing_from_devices() {
        let (relay_base, hits, _join) = spawn_mock_relay_peers(vec![report(TARGET_C)]).await;
        let _ = relay_base;
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string()]).await;
        seed_shadow(&state, VIA_B, TARGET_C, true);

        probe_cycle(&state).await;

        assert_eq!(hits.load(Ordering::SeqCst), 0, "via 缺失不应发起网络调用");
        assert!(
            !state
                .relay
                .shadow_devices
                .read()
                .unwrap()
                .get(TARGET_C)
                .unwrap()
                .online,
            "via 缺失时影子应整批下线"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     用户从 `via_device_ids` 移除跳板后（配置热生效），其名下影子必须被清理
    ///     （撤销信任语义），无论影子当前 online 与否。
    ///
    /// Code Logic（这个测试做什么）:
    ///     预置 via=B 的两条影子（一条 online 一条 offline）但配置为空（模拟热移除），
    ///     跑一轮断言影子被全部清空。
    #[tokio::test]
    async fn probe_cycle_removes_shadows_when_via_removed_from_config() {
        let state = build_test_state(SELF_ID, Vec::new()).await;
        seed_shadow(&state, VIA_B, TARGET_C, true);
        seed_shadow(&state, VIA_B, TARGET_D, false);

        probe_cycle(&state).await;

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert!(
            shadows.is_empty(),
            "配置移除的 via 名下影子应被清理: {shadows:?}"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     跳板报告成功但此前影子存在时是"整批替换"语义：消失的目标被移除、保留的
    ///     目标保持 online（列表随跳板最新视图收敛）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     首轮 mock 报告 C+D 写入两条影子；把 via 指向只报 C 的第二个 mock，
    ///     再跑一轮断言只剩 C 且 online=true。
    #[tokio::test]
    async fn probe_cycle_replaces_shadows_with_latest_report() {
        let (relay_base, _hits, _join) =
            spawn_mock_relay_peers(vec![report(TARGET_C), report(TARGET_D)]).await;
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string()]).await;
        seed_device(
            &state,
            device(VIA_B, "127.0.0.1", port_of(&relay_base), true),
        );
        probe_cycle(&state).await;
        assert_eq!(state.relay.shadow_devices.read().unwrap().len(), 2);

        let (relay_base2, _hits2, _join2) = spawn_mock_relay_peers(vec![report(TARGET_C)]).await;
        seed_device(
            &state,
            device(VIA_B, "127.0.0.1", port_of(&relay_base2), true),
        );
        probe_cycle(&state).await;

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 1, "消失的 D 应被整批替换移除: {shadows:?}");
        assert!(shadows.get(TARGET_C).unwrap().online);
        assert!(shadows.get(TARGET_D).is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     老版本跳板无 `/api/relay/peers`（404）时必须把影子整批下线（fail-closed），
    ///     而不是保留陈旧的 online 影子误导设备列表与路由解析。
    ///
    /// Code Logic（这个测试做什么）:
    ///     via 指向一个没有该路由的 mock（404），预置 online 影子；跑一轮断言
    ///     影子 online=false。
    #[tokio::test]
    async fn probe_cycle_marks_shadows_offline_when_peers_route_missing() {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = Router::new().route("/api/health", get(|| async { "ok" }));
        let _join = tokio::spawn(async move {
            axum::serve(listener, app.into_make_service())
                .await
                .expect("legacy mock serve");
        });
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string()]).await;
        seed_device(&state, device(VIA_B, "127.0.0.1", port, true));
        seed_shadow(&state, VIA_B, TARGET_C, true);

        probe_cycle(&state).await;

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert!(
            !shadows.get(TARGET_C).unwrap().online,
            "404（老版本 B）应把影子下线"
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     影子排除规则端到端：本机自身、直连表已有目标、跨 via 重复（先到先得）
    ///     都不得被新 via 的报告覆盖成影子。
    ///
    /// Code Logic（这个测试做什么）:
    ///     配置 via=[B1, B2]；B2 在本机直连表缺失（其影子随后被置 offline 但归属
    ///     保留）；直连表已有 D；影子表预置 C 属于 B2；mock B1 报告 [自身, C, D]；
    ///     跑一轮断言只剩 C（仍属 B2，且因 B2 不可达而 offline），自身与 D 均未入表。
    #[tokio::test]
    async fn probe_cycle_respects_shadow_eligibility_rules() {
        let (relay_base, _hits, _join) =
            spawn_mock_relay_peers(vec![report(SELF_ID), report(TARGET_C), report(TARGET_D)]).await;
        let via_b2 = "relay-host-B2";
        let state = build_test_state(SELF_ID, vec![VIA_B.to_string(), via_b2.to_string()]).await;
        seed_device(
            &state,
            device(VIA_B, "127.0.0.1", port_of(&relay_base), true),
        );
        seed_device(&state, device(TARGET_D, "10.0.0.9", 62116, true));
        // C 已被另一跳板 B2 先到先得（B2 仍在配置中，避免被配置 diff 清理干扰）。
        seed_shadow(&state, via_b2, TARGET_C, true);

        probe_cycle(&state).await;

        let shadows = state.relay.shadow_devices.read().unwrap();
        assert_eq!(shadows.len(), 1, "只保留先到先得的 C: {shadows:?}");
        assert_eq!(shadows.get(TARGET_C).unwrap().via_device_id, via_b2);
        assert!(
            !shadows.get(TARGET_C).unwrap().online,
            "B2 不可达时其名下影子应被置 offline"
        );
        assert!(shadows.get(SELF_ID).is_none(), "本机不得成为影子");
        assert!(shadows.get(TARGET_D).is_none(), "直连重复不得成为影子");
    }
}
