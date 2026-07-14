//! N2 mixed-version content sync 集成 harness（Prompt 域为主）。
//!
//! Business Logic（为什么需要这个模块）:
//!     v2 客户端对 legacy 对端不得尝试 manifest/items/push-batch；legacy 路径上的网络/远端
//!     失败必须保持 typed 非空成功，禁止折叠为 Succeeded 空成功。本 harness 自动验证路由
//!     门控与真值计数。
//!
//! Code Logic（这个模块做什么）:
//!     启动带 HitCounters 的 mock axum peer（health + Prompt v2/legacy 路由），构造本机
//!     AppState，调用 `trigger_sync` 断言 prompt 路由选择与 device/domain 终态。

use crate::backend::ui::HeadlessBackendUi;
use crate::config::{AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
use crate::models::device::Device;
use crate::models::prompt::PromptRow;
use crate::net::peer_client::PeerClient;
use crate::net::protocol::{CAPABILITY_SYNC_MANIFEST_V2, PROTOCOL_VERSION_V1};
use crate::net::routes::health::HealthResponse;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
use crate::state::AppState;
use crate::storage::sync_delete_sequence_repo::SyncDeleteSequenceRepo;
use crate::storage::{
    ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
    WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorktreeRepo,
};
use crate::sync::engine::{
    domain_outcome_is_success, trigger_sync, DeviceSyncStatus, DOMAIN_PROMPT,
};
use crate::sync::protocol::{SyncDomainOutcome, SyncManifestPage, SyncSummary};
use crate::transfer::registry::TransferRegistry;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// 路由命中计数（Prompt v2 vs legacy）。
#[derive(Clone, Default)]
struct HitCounters {
    manifest_page: Arc<AtomicU32>,
    items: Arc<AtomicU32>,
    push_batch: Arc<AtomicU32>,
    pull: Arc<AtomicU32>,
    push: Arc<AtomicU32>,
}

/// 对端内存 Prompt 存储。
#[derive(Clone, Default)]
struct PeerStore {
    rows: Arc<Mutex<HashMap<String, PromptRow>>>,
}

/// mock peer 运行时状态。
#[derive(Clone)]
struct PeerState {
    protocol_version: u32,
    capabilities: Vec<String>,
    hits: HitCounters,
    store: PeerStore,
    /// legacy pull 强制失败：None=正常；Some(status)=返回该 HTTP 状态。
    force_pull_status: Option<u16>,
}

async fn health_handler(AxumState(st): AxumState<PeerState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        device_id: "peer".into(),
        device_name: "peer".into(),
        http_port: 1,
        ts: 1,
        protocol_version: st.protocol_version,
        capabilities: st.capabilities.clone(),
    })
}

async fn prompt_manifest_page_handler(
    AxumState(st): AxumState<PeerState>,
    Json(_body): Json<serde_json::Value>,
) -> Json<SyncManifestPage<String>> {
    st.hits.manifest_page.fetch_add(1, Ordering::SeqCst);
    // 空完整页：next_cursor=None 表示流结束。
    let mut items: Vec<SyncSummary<String>> = st
        .store
        .rows
        .lock()
        .unwrap()
        .values()
        .map(|r| SyncSummary {
            id: r.id.clone(),
            vector_clock: r.vector_clock.clone(),
            content_hash: format!("hash-{}", r.id),
            size: r.content.len() as u64,
            updated_at: r.updated_at.clone(),
            deleted: r.deleted,
            delete_epoch: r.delete_epoch,
        })
        .collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Json(SyncManifestPage {
        items,
        next_cursor: None,
    })
}

async fn prompt_items_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.items.fetch_add(1, Ordering::SeqCst);
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let guard = st.store.rows.lock().unwrap();
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        if let Some(row) = guard.get(&id) {
            items.push(row.clone());
        } else {
            missing.push(id);
        }
    }
    Json(serde_json::json!({ "items": items, "missing_ids": missing }))
}

async fn prompt_push_batch_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.push_batch.fetch_add(1, Ordering::SeqCst);
    let items: Vec<PromptRow> = body
        .get("items")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let n = items.len();
    let mut guard = st.store.rows.lock().unwrap();
    for item in items {
        guard.insert(item.id.clone(), item);
    }
    Json(serde_json::json!({ "accepted": n }))
}

async fn prompt_pull_handler(
    AxumState(st): AxumState<PeerState>,
    Json(_body): Json<serde_json::Value>,
) -> axum::response::Response {
    st.hits.pull.fetch_add(1, Ordering::SeqCst);
    if let Some(code) = st.force_pull_status {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        return (
            status,
            Json(serde_json::json!({
                "error": "injected pull failure",
                "code": "internal",
                "request_id": "harness",
                "retryable": false,
            })),
        )
            .into_response();
    }
    let prompts: Vec<PromptRow> = st.store.rows.lock().unwrap().values().cloned().collect();
    Json(serde_json::json!({ "prompts": prompts })).into_response()
}

async fn prompt_push_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.push.fetch_add(1, Ordering::SeqCst);
    let prompts: Vec<PromptRow> = body
        .get("prompts")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let n = prompts.len();
    let mut guard = st.store.rows.lock().unwrap();
    for item in prompts {
        guard.insert(item.id.clone(), item);
    }
    Json(serde_json::json!({ "accepted": n }))
}

/// 空成功 stub：让 ssh/scratchpad legacy 不因 404 主导失败路径（可选）。
async fn empty_legacy_pull() -> Json<serde_json::Value> {
    // 兼容 targets/pages/prompts 多种字段名，返回空列表即可。
    Json(serde_json::json!({
        "targets": [],
        "pages": [],
        "prompts": [],
        "items": []
    }))
}

async fn empty_legacy_push() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "accepted": 0 }))
}

/// 空 v2 stub：完整空 manifest 页 + 空 items/push-batch。
async fn empty_v2_manifest_page() -> Json<SyncManifestPage<String>> {
    Json(SyncManifestPage {
        items: vec![],
        next_cursor: None,
    })
}

async fn empty_v2_items() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "items": [], "missing_ids": [] }))
}

async fn empty_v2_push_batch(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let n = body
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Json(serde_json::json!({ "accepted": n }))
}

/// 空 v2 ack-delete-epoch stub：无正文可推时专用水位 ack，返回 `{ok:true}`。
async fn empty_v2_ack_delete_epoch() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn spawn_peer(st: PeerState) -> (String, HitCounters, PeerStore) {
    let hits = st.hits.clone();
    let store = st.store.clone();
    let app = Router::new()
        .route("/api/health", get(health_handler))
        // Prompt legacy
        .route("/api/sync/pull", post(prompt_pull_handler))
        .route("/api/sync/push", post(prompt_push_handler))
        // Prompt v2
        .route(
            "/api/sync/prompts/manifest-page",
            post(prompt_manifest_page_handler),
        )
        .route("/api/sync/prompts/items", post(prompt_items_handler))
        .route(
            "/api/sync/prompts/push-batch",
            post(prompt_push_batch_handler),
        )
        .route(
            "/api/sync/prompts/ack-delete-epoch",
            post(empty_v2_ack_delete_epoch),
        )
        // SSH/scratchpad legacy stubs（避免 404 掩盖 prompt 路径断言）
        .route("/api/ssh-target/sync/pull", post(empty_legacy_pull))
        .route("/api/ssh-target/sync/push", post(empty_legacy_push))
        .route("/api/scratchpad/sync/pull", post(empty_legacy_pull))
        .route("/api/scratchpad/sync/push", post(empty_legacy_push))
        // SSH/scratchpad v2 stubs
        .route(
            "/api/ssh-target/sync/manifest-page",
            post(empty_v2_manifest_page),
        )
        .route("/api/ssh-target/sync/items", post(empty_v2_items))
        .route("/api/ssh-target/sync/push-batch", post(empty_v2_push_batch))
        .route(
            "/api/ssh-target/sync/ack-delete-epoch",
            post(empty_v2_ack_delete_epoch),
        )
        .route(
            "/api/scratchpad/sync/manifest-page",
            post(empty_v2_manifest_page),
        )
        .route("/api/scratchpad/sync/items", post(empty_v2_items))
        .route("/api/scratchpad/sync/push-batch", post(empty_v2_push_batch))
        .route(
            "/api/scratchpad/sync/ack-delete-epoch",
            post(empty_v2_ack_delete_epoch),
        )
        .with_state(st);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), hits, store)
}

fn sample_prompt(id: &str, device: &str, content: &str, vc: u64) -> PromptRow {
    let mut vector_clock = HashMap::new();
    vector_clock.insert(device.to_string(), vc);
    PromptRow {
        id: id.to_string(),
        title: format!("t-{id}"),
        content: content.to_string(),
        tags: vec![],
        created_at: "2026-07-14T00:00:00Z".into(),
        updated_at: "2026-07-14T00:00:00Z".into(),
        device_id: device.to_string(),
        vector_clock,
        deleted: false,
        delete_epoch: 0,
    }
}

async fn build_local_state(device_id: &str) -> AppState {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();

    // Prompt 域必需表 + delete_epoch 序列
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS prompts (\
         id TEXT PRIMARY KEY, title TEXT NOT NULL, content TEXT NOT NULL, \
         tags TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
         device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
         delete_epoch INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    SyncDeleteSequenceRepo::ensure_schema(&pool).await.unwrap();

    // 空表即可让 ssh/scratchpad get_all_for_sync 返回 Ok([])，不主导失败
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ssh_targets (\
         host TEXT PRIMARY KEY, port INTEGER NOT NULL, username TEXT NOT NULL, \
         label TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, \
         device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
         delete_epoch INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scratchpad (\
         id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
         vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
         delete_epoch INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let config = AppConfig {
        device_id: device_id.to_string(),
        device_name: "local".into(),
        http_port: 0,
        receive_dir: "/tmp/cc-partner-test-recv".into(),
        db_path: ":memory:".into(),
        screenshot_hotkey: "<cmd>+s".into(),
        prompt_optimizer_hotkey: "<ctrl>".into(),
        prompt_optimizer_fill_language: "zh".into(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: HealthConfig::default(),
        orchestrator: OrchestratorAutomationConfig::default(),
        github_trending: GithubTrendingConfig::default(),
    };
    let store = Arc::new(crate::config_store::MemoryConfigStore::with_config(
        config.clone(),
    ));
    let config_runtime = Arc::new(crate::config_runtime::ConfigRuntime::new(config, store));
    let config = config_runtime.shared_value();
    let maintenance_gate = Arc::new(crate::storage::DatabaseMaintenanceGate::new());

    AppState {
        config,
        config_runtime,
        db: pool.clone(),
        maintenance_gate: maintenance_gate.clone(),
        prompt_repo: Arc::new(PromptRepo::with_gate(
            pool.clone(),
            maintenance_gate.clone(),
        )),
        transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
        claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
        scratchpad_repo: Arc::new(ScratchpadRepo::with_gate(
            pool.clone(),
            maintenance_gate.clone(),
        )),
        device_id: Arc::new(device_id.to_string()),
        devices: Arc::new(RwLock::new(HashMap::new())),
        actual_http_port: Arc::new(AtomicU16::new(0)),
        discovery: Arc::new(Mutex::new(None)),
        peer_client: Arc::new(PeerClient::new()),
        transfers: Arc::new(TransferRegistry::new()),
        ui: Arc::new(HeadlessBackendUi::new(std::path::PathBuf::from("/tmp"))),
        update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
        cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
        ssh_target_repo: Arc::new(SshTargetRepo::with_gate(
            pool.clone(),
            maintenance_gate.clone(),
        )),
        workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
        workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
        workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
        workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
        workbench_browser_previews: Arc::new(
            crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
        ),
        workbench_sessions: Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new()),
        workbench_remote_events: {
            let (tx, _) = tokio::sync::broadcast::channel(8);
            tx
        },
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
        orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
        orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
        orchestrator_cancel: Arc::new(Mutex::new(None)),
        orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
        workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
        workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
        runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
        runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
        event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(format!(
            "sync-mixed-{device_id}"
        ))),
    }
}

fn device_for(base_url: &str) -> Device {
    let trimmed = base_url.trim_start_matches("http://");
    let (host, port_s) = trimmed.split_once(':').unwrap();
    Device {
        id: "peer".into(),
        name: "peer".into(),
        host: host.to_string(),
        port: port_s.parse().unwrap(),
        last_seen: Utc::now(),
        online: true,
        proto_version: 1,
        capabilities: vec![],
    }
}

fn insert_device(state: &AppState, device: Device) {
    let mut guard = state.devices.write().expect("devices 写锁");
    guard.insert(device.id.clone(), device);
}

fn prompt_outcome_from_result(
    result: &crate::sync::engine::SyncRunResult,
) -> Option<&SyncDomainOutcome> {
    result
        .devices
        .first()
        .and_then(|d| d.domains.iter().find(|x| x.domain == DOMAIN_PROMPT))
        .map(|d| &d.outcome)
}

/// v2 客户端对 legacy 对端：只走 legacy pull/push，永不打 v2 batch 路由。
///
/// Business Logic（为什么需要这个函数）:
///     capability 门控是 mixed-version 正确性核心：对端无 `sync.manifest.v2` 时，
///     本地即使支持 v2 也只能 legacy；误打 v2 路由会整轮失败或协议损坏。
///
/// Code Logic（这个函数做什么）:
///     mock legacy health（无 capability）→ seed 本地 prompt → trigger_sync →
///     断言 manifest_page/items/push_batch 全 0，且 pull/push 至少命中一次。
pub async fn assert_v2_client_to_legacy_peer_uses_only_legacy_routes() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    store.rows.lock().unwrap().insert(
        "remote-only".into(),
        sample_prompt("remote-only", "peer", "from-peer", 1),
    );
    let (base, hits, store) = spawn_peer(PeerState {
        protocol_version: 0,
        capabilities: vec![],
        hits,
        store,
        force_pull_status: None,
    })
    .await;

    let state = build_local_state("local").await;
    state
        .prompt_repo
        .create(&sample_prompt("local-only", "local", "from-local", 1))
        .await
        .unwrap();
    insert_device(&state, device_for(&base));

    let result = trigger_sync(&state).await;

    assert_eq!(
        hits.manifest_page.load(Ordering::SeqCst),
        0,
        "legacy peer 不得调用 manifest-page"
    );
    assert_eq!(
        hits.items.load(Ordering::SeqCst),
        0,
        "legacy peer 不得调用 items"
    );
    assert_eq!(
        hits.push_batch.load(Ordering::SeqCst),
        0,
        "legacy peer 不得调用 push-batch"
    );
    assert!(
        hits.pull.load(Ordering::SeqCst) > 0,
        "应至少调用 legacy pull 一次"
    );
    assert!(
        hits.push.load(Ordering::SeqCst) > 0,
        "应至少调用 legacy push 一次"
    );

    // prompt 域应成功（其它域 stub 也可能成功）
    let prompt_out = prompt_outcome_from_result(&result).expect("应有 prompt domain");
    assert!(
        domain_outcome_is_success(prompt_out),
        "prompt legacy 路径应 Succeeded: {prompt_out:?}"
    );
    assert!(
        state
            .prompt_repo
            .get("remote-only")
            .await
            .unwrap()
            .is_some(),
        "应从 legacy peer pull 到 remote-only"
    );
    assert!(
        store.rows.lock().unwrap().contains_key("local-only"),
        "应向 legacy peer push local-only"
    );
}

/// legacy 对端 pull 网络/远端失败：必须 typed 失败，不得 Succeeded 空成功。
///
/// Business Logic（为什么需要这个函数）:
///     旧 bug 会把 pull 500/断连折叠成空远端成功；Settings 会误计设备成功并静默丢同步。
///
/// Code Logic（这个函数做什么）:
///     mock legacy health OK 但 pull 返回 500 → trigger_sync → 断言 prompt outcome 非 Succeeded，
///     device status 非 Succeeded，succeeded_devices/synced == 0。
pub async fn assert_legacy_peer_network_failure_is_typed_not_empty_success() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    let (base, hits, _) = spawn_peer(PeerState {
        protocol_version: 1,
        capabilities: vec![], // 无 sync.manifest.v2 → legacy
        hits,
        store,
        force_pull_status: Some(500),
    })
    .await;

    let state = build_local_state("local").await;
    state
        .prompt_repo
        .create(&sample_prompt("local-only", "local", "from-local", 1))
        .await
        .unwrap();
    insert_device(&state, device_for(&base));

    let result = trigger_sync(&state).await;

    assert!(
        hits.pull.load(Ordering::SeqCst) > 0,
        "应实际命中 legacy pull"
    );
    assert_eq!(
        hits.manifest_page.load(Ordering::SeqCst),
        0,
        "失败路径仍不得误走 v2"
    );
    assert_eq!(result.succeeded_devices, 0, "失败设备不得计入 succeeded");
    assert_eq!(result.synced, 0, "synced 必须与 succeeded_devices 同值");

    let device = result.devices.first().expect("应有设备报告");
    assert_ne!(
        device.status,
        DeviceSyncStatus::Succeeded,
        "设备不得标全成功: {:?}",
        device.status
    );

    let prompt_out = prompt_outcome_from_result(&result).expect("应有 prompt domain");
    assert!(
        !domain_outcome_is_success(prompt_out),
        "prompt 不得 Succeeded 空成功: {prompt_out:?}"
    );
    match prompt_out {
        SyncDomainOutcome::ProtocolError { .. }
        | SyncDomainOutcome::Unreachable { .. }
        | SyncDomainOutcome::ResourceLimit { .. }
        | SyncDomainOutcome::Partial { .. } => {}
        SyncDomainOutcome::Succeeded {
            pulled,
            pushed,
            unchanged,
        } => {
            panic!("expected typed failure, got Succeeded pulled={pulled} pushed={pushed} unchanged={unchanged}");
        }
    }
}

/// v2 对端：至少命中一条 v2 路由，且 legacy pull/push 为 0。
///
/// Business Logic（为什么需要这个函数）:
///     对端宣告 `sync.manifest.v2` 时客户端必须走 plan 路径，避免再打 legacy 全量交换。
///
/// Code Logic（这个函数做什么）:
///     mock v2 capability peer → trigger_sync → 断言 v2 hit>0 且 legacy pull/push=0。
pub async fn assert_v2_peer_uses_manifest_routes() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    store.rows.lock().unwrap().insert(
        "remote-v2".into(),
        sample_prompt("remote-v2", "peer", "from-v2-peer", 1),
    );
    let (base, hits, _) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_SYNC_MANIFEST_V2.to_string()],
        hits,
        store,
        force_pull_status: None,
    })
    .await;

    let state = build_local_state("local").await;
    state
        .prompt_repo
        .create(&sample_prompt("local-v2", "local", "from-local-v2", 1))
        .await
        .unwrap();
    insert_device(&state, device_for(&base));

    let result = trigger_sync(&state).await;

    assert!(
        hits.manifest_page.load(Ordering::SeqCst) > 0,
        "v2 peer 应调用 manifest-page"
    );
    // 有远端独有/本机独有 → items 与/或 push_batch 命中；无正文时走 ack-delete-epoch（非空 push）
    assert!(
        hits.items.load(Ordering::SeqCst) > 0 || hits.push_batch.load(Ordering::SeqCst) > 0,
        "v2 peer 应调用 items 或 push-batch 至少一次 (items={}, push_batch={})",
        hits.items.load(Ordering::SeqCst),
        hits.push_batch.load(Ordering::SeqCst)
    );
    assert_eq!(
        hits.pull.load(Ordering::SeqCst),
        0,
        "v2 peer 不得调用 legacy pull"
    );
    assert_eq!(
        hits.push.load(Ordering::SeqCst),
        0,
        "v2 peer 不得调用 legacy push"
    );

    let prompt_out = prompt_outcome_from_result(&result).expect("应有 prompt domain");
    assert!(
        domain_outcome_is_success(prompt_out),
        "v2 prompt 路径应 Succeeded: {prompt_out:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn content_sync_mixed_version_v2_to_legacy_only_legacy() {
        assert_v2_client_to_legacy_peer_uses_only_legacy_routes().await;
    }

    #[tokio::test]
    async fn content_sync_mixed_version_legacy_failure_typed() {
        assert_legacy_peer_network_failure_is_typed_not_empty_success().await;
    }

    #[tokio::test]
    async fn content_sync_mixed_version_v2_peer_uses_manifest() {
        assert_v2_peer_uses_manifest_routes().await;
    }
}
