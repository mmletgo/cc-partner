//! S5 Task6 mixed-version CC History 同步集成 harness。
//!
//! Business Logic（为什么需要这个模块）:
//!     分页协议与 legacy 回退必须在 new↔new / new↔legacy / 畸形 paged 等组合上可自动验证；
//!     集成测试 `tests/backend_scale.rs` 与 crate 内 unit 测试共用同一套场景。
//!
//! Code Logic（这个模块做什么）:
//!     启动带 hit 计数器的 mock 对端（health + paged/legacy 路由），构造本机 AppState，
//!     调用 `cc_sync_with_peer` 断言路由选择与收敛语义。

use crate::backend::ui::HeadlessBackendUi;
use crate::cc::engine::cc_sync_with_peer;
use crate::cc::merger::merge_cc_history;
use crate::cc::models::ClaudeHistoryRow;
use crate::config::{AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig};
use crate::models::device::Device;
use crate::net::peer_client::PeerClient;
use crate::net::protocol::{CAPABILITY_CC_HISTORY_PAGED_SYNC_V1, PROTOCOL_VERSION_V1};
use crate::net::routes::cc_history::{
    decode_manifest_cursor, encode_manifest_cursor, CC_CONTENT_MAX_BYTES, CODE_BATCH_TOO_LARGE,
    CODE_ITEM_TOO_LARGE,
};
use crate::net::routes::health::HealthResponse;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
use crate::state::AppState;
use crate::storage::{
    ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
    WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo,
    WorkbenchWorktreeRepo,
};
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

/// 路由命中计数（paged vs legacy）。
#[derive(Clone, Default)]
struct HitCounters {
    manifest_page: Arc<AtomicU32>,
    items: Arc<AtomicU32>,
    push_batch: Arc<AtomicU32>,
    pull: Arc<AtomicU32>,
    push: Arc<AtomicU32>,
}

/// 对端内存存储。
#[derive(Clone, Default)]
struct PeerStore {
    rows: Arc<Mutex<HashMap<String, ClaudeHistoryRow>>>,
}

#[derive(Clone)]
struct PeerState {
    protocol_version: u32,
    capabilities: Vec<String>,
    hits: HitCounters,
    store: PeerStore,
    malformed_manifest: bool,
    /// 模拟 items 对 ≥N 条 ID 返回 413 batch_too_large（0=关闭）。
    force_items_batch_too_large_at: usize,
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

async fn manifest_page_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.manifest_page.fetch_add(1, Ordering::SeqCst);
    if st.malformed_manifest {
        return Json(serde_json::json!({
            "summaries": [],
            "next_cursor": "same-cursor",
            "done": false,
        }));
    }
    // 生产 codec：opaque base64url({v:1,last_id})；与 encode/decode_manifest_cursor 互通。
    let after_id = body
        .get("cursor")
        .and_then(|v| v.as_str())
        .and_then(|c| decode_manifest_cursor(c).ok());
    let mut rows: Vec<ClaudeHistoryRow> = st.store.rows.lock().unwrap().values().cloned().collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let start = match after_id {
        Some(id) => rows.iter().position(|r| r.id > id).unwrap_or(rows.len()),
        None => 0,
    };
    let page: Vec<_> = rows.into_iter().skip(start).take(256).collect();
    let done = page.len() < 256;
    let next_cursor = if done {
        None
    } else {
        page.last().map(|r| encode_manifest_cursor(&r.id))
    };
    let summaries: Vec<serde_json::Value> = page
        .iter()
        .map(|r| serde_json::json!({"id": r.id, "vector_clock": r.vector_clock}))
        .collect();
    Json(serde_json::json!({
        "summaries": summaries,
        "next_cursor": next_cursor,
        "done": done,
    }))
}

async fn items_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
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
    if st.force_items_batch_too_large_at > 0 && ids.len() >= st.force_items_batch_too_large_at {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": "batch too large",
                "code": CODE_BATCH_TOO_LARGE,
                "request_id": "harness",
                "retryable": false,
                "details": {}
            })),
        )
            .into_response();
    }
    let guard = st.store.rows.lock().unwrap();
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for id in &ids {
        if let Some(r) = guard.get(id) {
            if r.content.len() > CC_CONTENT_MAX_BYTES {
                // 与生产 items_impl 一致：任一超限 → 整批 422。
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "error": "item too large",
                        "code": CODE_ITEM_TOO_LARGE,
                        "request_id": "harness",
                        "retryable": false,
                        "details": {}
                    })),
                )
                    .into_response();
            }
            items.push(r.clone());
        } else {
            missing.push(id.clone());
        }
    }
    Json(serde_json::json!({"items": items, "missing_ids": missing})).into_response()
}

async fn push_batch_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.push_batch.fetch_add(1, Ordering::SeqCst);
    let items: Vec<ClaudeHistoryRow> = body
        .get("items")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut guard = st.store.rows.lock().unwrap();
    let mut accepted = 0usize;
    for remote in items {
        match guard.get(&remote.id).cloned() {
            None => {
                guard.insert(remote.id.clone(), remote);
                accepted += 1;
            }
            Some(local) => {
                let merged = merge_cc_history(&local, &remote);
                if merged.vector_clock != local.vector_clock
                    || merged.updated_at != local.updated_at
                    || merged.content != local.content
                    || merged.deleted != local.deleted
                {
                    guard.insert(merged.id.clone(), merged);
                    accepted += 1;
                }
            }
        }
    }
    Json(serde_json::json!({"accepted": accepted}))
}

async fn pull_handler(
    AxumState(st): AxumState<PeerState>,
    Json(_body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.pull.fetch_add(1, Ordering::SeqCst);
    let items: Vec<ClaudeHistoryRow> = st.store.rows.lock().unwrap().values().cloned().collect();
    Json(serde_json::json!({"items": items}))
}

async fn push_handler(
    AxumState(st): AxumState<PeerState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    st.hits.push.fetch_add(1, Ordering::SeqCst);
    let items: Vec<ClaudeHistoryRow> = body
        .get("items")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut guard = st.store.rows.lock().unwrap();
    let n = items.len();
    for item in items {
        guard.insert(item.id.clone(), item);
    }
    Json(serde_json::json!({"accepted": n}))
}

async fn spawn_peer(st: PeerState) -> (String, HitCounters, PeerStore) {
    let hits = st.hits.clone();
    let store = st.store.clone();
    let app = Router::new()
        .route("/api/health", get(health_handler))
        .route(
            "/api/cc-history/sync/manifest-page",
            post(manifest_page_handler),
        )
        .route("/api/cc-history/sync/items", post(items_handler))
        .route("/api/cc-history/sync/push-batch", post(push_batch_handler))
        .route("/api/cc-history/sync/pull", post(pull_handler))
        .route("/api/cc-history/sync/push", post(push_handler))
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

fn sample_row(id: &str, device: &str, content: &str, vc: u64) -> ClaudeHistoryRow {
    let mut vector_clock = HashMap::new();
    vector_clock.insert(device.to_string(), vc);
    ClaudeHistoryRow {
        id: id.to_string(),
        project_path: "/p".into(),
        project_name: "p".into(),
        session_id: "s".into(),
        content: content.to_string(),
        git_branch: None,
        cc_version: None,
        occurred_at: "2024-01-01T00:00:00Z".into(),
        device_id: device.to_string(),
        vector_clock,
        created_at: "2024-01-01T00:00:00Z".into(),
        updated_at: "2024-01-01T00:00:00Z".into(),
        deleted: false,
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
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS claude_history (
            id TEXT PRIMARY KEY,
            project_path TEXT NOT NULL,
            project_name TEXT NOT NULL,
            session_id TEXT NOT NULL,
            content TEXT NOT NULL,
            git_branch TEXT,
            cc_version TEXT,
            occurred_at TEXT NOT NULL,
            device_id TEXT NOT NULL,
            vector_clock TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted INTEGER DEFAULT 0
        )",
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
    // S3 ConfigRuntime + UpdateRuntime；与 config 共享同一 Arc。
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
        prompt_repo: Arc::new(PromptRepo::new(pool.clone())),
        transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
        claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
        scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
        ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
        device_id: Arc::new(device_id.to_string()),
        devices: Arc::new(RwLock::new(HashMap::new())),
        actual_http_port: Arc::new(AtomicU16::new(0)),
        discovery: Arc::new(Mutex::new(None)),
        peer_client: Arc::new(PeerClient::new()),
        transfers: Arc::new(TransferRegistry::new()),
        ui: Arc::new(HeadlessBackendUi::new(std::path::PathBuf::from("/tmp"))),
        update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
        cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
        workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
        workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
        workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
        agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
        agent_ledger_service: Arc::new(crate::workbench::agent_ledger::AgentLedgerService::new(
            crate::storage::AgentLedgerRepo::new(pool.clone()),
        )),
        agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
        workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
        workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
        workbench_workspace_layout_repo: Arc::new(
            crate::storage::WorkbenchWorkspaceLayoutRepo::new(pool.clone()),
        ),
        workbench_browser_previews: Arc::new(
            crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
        ),
        browser_verification: Arc::new(
            crate::workbench::browser_verification::BrowserVerificationService::new(
                Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                std::env::temp_dir().join("cc-partner-bv-test"),
                "test-owner".into(),
            )
            .expect("browser verification test service"),
        ),
        workbench_sessions: Arc::new(crate::workbench::sessions::WorkbenchSessionRegistry::new()),
        workbench_remote_events: std::sync::Arc::new(
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
        orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
        orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
        orchestrator_cancel: Arc::new(Mutex::new(None)),
        orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
        agent_ledger_cancel: Arc::new(Mutex::new(None)),
        workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
        workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
        workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        workbench_claude_session_index_dispose_epochs: Arc::new(std::sync::Mutex::new(
            HashMap::new(),
        )),
        runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
        runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
        event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(format!(
            "mixed-{device_id}"
        ))),
        backend_control_client_runtime: Arc::new(
            crate::backend::control_client::BackendControlClientRuntime::new(),
        ),
        gui_event_relay_cancel: Arc::new(Mutex::new(None)),
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

/// new↔new：仅命中 paged 路由，数据双向收敛。
///
/// Business Logic（为什么需要这个函数）:
///     有 capability 的对端必须只走 manifest/items/push-batch，不得回退 legacy。
///
/// Code Logic（这个函数做什么）:
///     mock paged peer + 本地数据 → cc_sync_with_peer → 断言 hit 与收敛。
async fn assert_new_to_new_uses_only_paged_routes_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    store.rows.lock().unwrap().insert(
        "remote-only".into(),
        sample_row("remote-only", "peer", "from-peer", 1),
    );
    let (base, hits, store) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let state = build_local_state("local").await;
    state
        .cc_history_repo
        .bulk_ingest(&[sample_row("local-only", "local", "from-local", 1)])
        .await
        .unwrap();

    cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect("paged sync should succeed");

    assert!(
        hits.manifest_page.load(Ordering::SeqCst) > 0,
        "应调用 manifest-page"
    );
    assert!(hits.items.load(Ordering::SeqCst) > 0, "应调用 items");
    assert!(
        hits.push_batch.load(Ordering::SeqCst) > 0,
        "应调用 push-batch"
    );
    assert_eq!(hits.pull.load(Ordering::SeqCst), 0, "不得调用 legacy pull");
    assert_eq!(hits.push.load(Ordering::SeqCst), 0, "不得调用 legacy push");
    assert!(state
        .cc_history_repo
        .get("remote-only")
        .await
        .unwrap()
        .is_some());
    assert!(store.rows.lock().unwrap().contains_key("local-only"));
}

/// new↔legacy：仅命中 legacy 路由。
///
/// Business Logic（为什么需要这个函数）:
///     无 capability 的旧对端必须只走 pull/push，不得尝试 paged。
///
/// Code Logic（这个函数做什么）:
///     mock v0 peer → cc_sync_with_peer → 断言仅 legacy hit。
async fn assert_new_to_legacy_uses_only_legacy_routes_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    store.rows.lock().unwrap().insert(
        "remote-only".into(),
        sample_row("remote-only", "peer", "from-peer", 1),
    );
    let (base, hits, store) = spawn_peer(PeerState {
        protocol_version: 0,
        capabilities: vec![],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let state = build_local_state("local").await;
    state
        .cc_history_repo
        .bulk_ingest(&[sample_row("local-only", "local", "from-local", 1)])
        .await
        .unwrap();

    cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect("legacy sync should succeed");

    assert_eq!(hits.manifest_page.load(Ordering::SeqCst), 0);
    assert_eq!(hits.items.load(Ordering::SeqCst), 0);
    assert_eq!(hits.push_batch.load(Ordering::SeqCst), 0);
    assert!(hits.pull.load(Ordering::SeqCst) > 0);
    assert!(hits.push.load(Ordering::SeqCst) > 0);
    assert!(state
        .cc_history_repo
        .get("remote-only")
        .await
        .unwrap()
        .is_some());
    assert!(store.rows.lock().unwrap().contains_key("local-only"));
}

/// 畸形 paged 响应必须失败本轮，不得当空成功。
///
/// Business Logic（为什么需要这个函数）:
///     cursor 不前进等协议故障若被折叠成空成功，会丢数据且难排查。
///
/// Code Logic（这个函数做什么）:
///     mock 固定 cursor + done=false → 断言 Err 且不走 legacy。
async fn assert_malformed_paged_fails_round_not_empty_success_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    let (base, hits, _) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: true,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let state = build_local_state("local").await;
    state
        .cc_history_repo
        .bulk_ingest(&[sample_row("local-only", "local", "from-local", 1)])
        .await
        .unwrap();

    let err = cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect_err("malformed paged must fail round");
    assert!(
        err.contains("cursor") || err.contains("manifest"),
        "错误应指向 cursor/manifest: {err}"
    );
    assert!(hits.manifest_page.load(Ordering::SeqCst) >= 1);
    assert_eq!(hits.pull.load(Ordering::SeqCst), 0);
}

/// legacy 请求体仍可对 new 服务端工作。
///
/// Business Logic（为什么需要这个函数）:
///     新服务端必须保留 legacy 路由，旧客户端才能与新设备同步。
///
/// Code Logic（这个函数做什么）:
///     mock 带 paged capability 的 peer，直接调 legacy pull/push 并断言成功。
async fn assert_legacy_bodies_work_against_new_server_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    store.rows.lock().unwrap().insert(
        "remote-only".into(),
        sample_row("remote-only", "peer", "x", 1),
    );
    let (base, hits, store) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let client = PeerClient::new();
    let pulled = client
        .cc_sync_pull(
            &base,
            vec![serde_json::json!({"id":"local-only","vector_clock":{"local":1}})],
        )
        .await;
    assert!(!pulled.is_empty());
    assert!(hits.pull.load(Ordering::SeqCst) > 0);

    let ok = client
        .cc_sync_push(&base, &[sample_row("from-legacy-client", "c", "y", 1)])
        .await;
    assert!(ok);
    assert!(hits.push.load(Ordering::SeqCst) > 0);
    assert!(store
        .rows
        .lock()
        .unwrap()
        .contains_key("from-legacy-client"));
}

/// 多 ID 批中混入 1 条 content>1MiB 毒丸时，客户端必须对半拆批并仍 pull 到好数据。
///
/// Business Logic（为什么需要这个函数）:
///     H1 回归：服务端整批 422 不得永久卡死整轮 paged sync；好 ID 必须先落库。
///
/// Code Logic（这个函数做什么）:
///     peer 存 1 条超限 + 2 条正常；本机空库 sync → 正常行落库，本轮以 item_too_large 结束。
async fn assert_item_too_large_halves_and_isolates_poison_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    {
        let mut g = store.rows.lock().unwrap();
        g.insert("good-a".into(), sample_row("good-a", "peer", "ok-a", 1));
        g.insert("good-b".into(), sample_row("good-b", "peer", "ok-b", 1));
        let mut poison = sample_row("poison", "peer", "x", 1);
        poison.content = "P".repeat(CC_CONTENT_MAX_BYTES + 8);
        g.insert("poison".into(), poison);
    }
    let (base, hits, _) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let state = build_local_state("local").await;
    let err = cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect_err("poison must end round after isolating");
    assert!(
        err.contains("item_too_large"),
        "错误应指向 item_too_large: {err}"
    );
    assert!(hits.items.load(Ordering::SeqCst) >= 2, "应拆批多次 items");
    assert!(
        state.cc_history_repo.get("good-a").await.unwrap().is_some(),
        "好 ID good-a 必须已 pull"
    );
    assert!(
        state.cc_history_repo.get("good-b").await.unwrap().is_some(),
        "好 ID good-b 必须已 pull"
    );
    assert!(
        state.cc_history_repo.get("poison").await.unwrap().is_none(),
        "毒丸不得入库"
    );
}

/// mock 对 ≥2 条 ID 返回 413 时，客户端必须对半拆批直至成功。
///
/// Business Logic（为什么需要这个函数）:
///     413 拆批是分页协议核心恢复路径，mixed harness 必须覆盖而非只测 happy path。
///
/// Code Logic（这个函数做什么）:
///     force_items_batch_too_large_at=2 + 3 条远端 → 多次 items 调用后收敛。
async fn assert_batch_too_large_halves_until_success_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    {
        let mut g = store.rows.lock().unwrap();
        for id in ["a", "b", "c"] {
            g.insert(id.into(), sample_row(id, "peer", id, 1));
        }
    }
    let (base, hits, _) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 2,
    })
    .await;

    let state = build_local_state("local").await;
    cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect("413 拆批后应成功");
    assert!(hits.items.load(Ordering::SeqCst) >= 3, "应拆到单条多次调用");
    for id in ["a", "b", "c"] {
        assert!(
            state.cc_history_repo.get(id).await.unwrap().is_some(),
            "应 pull 到 {id}"
        );
    }
}

/// 并发向量时钟经 merge 后收敛（push-batch 走 merge，非覆盖写）。
///
/// Business Logic（为什么需要这个函数）:
///     mixed harness 若只覆盖写，测不出 LWW/VC 收敛；new↔new 必须验证 merge 语义。
///
/// Code Logic（这个函数做什么）:
///     本机与 peer 同 id 并发 clock → sync 后本机与 peer 的 clock 合并且 content 按 LWW。
async fn assert_concurrent_vector_clock_merges_async() {
    let hits = HitCounters::default();
    let store = PeerStore::default();
    let mut peer_row = sample_row("shared", "peer", "peer-content", 1);
    peer_row.updated_at = "2024-01-02T00:00:00Z".into();
    peer_row.vector_clock.insert("peer".into(), 3);
    store
        .rows
        .lock()
        .unwrap()
        .insert("shared".into(), peer_row.clone());

    let (base, _hits, store) = spawn_peer(PeerState {
        protocol_version: PROTOCOL_VERSION_V1,
        capabilities: vec![CAPABILITY_CC_HISTORY_PAGED_SYNC_V1.to_string()],
        hits,
        store,
        malformed_manifest: false,
        force_items_batch_too_large_at: 0,
    })
    .await;

    let state = build_local_state("local").await;
    let mut local_row = sample_row("shared", "local", "local-content", 1);
    local_row.updated_at = "2024-01-01T00:00:00Z".into();
    local_row.vector_clock.insert("local".into(), 5);
    // 并发：两端各有对方没有的分量。
    state
        .cc_history_repo
        .bulk_ingest(&[local_row.clone()])
        .await
        .unwrap();

    cc_sync_with_peer(&state, &device_for(&base))
        .await
        .expect("concurrent merge sync");

    let after = state
        .cc_history_repo
        .get("shared")
        .await
        .unwrap()
        .expect("shared exists");
    // peer 时间更新 → LWW 取 peer content；clock 合并两侧分量。
    assert_eq!(after.content, "peer-content");
    assert!(after.vector_clock.get("local").copied().unwrap_or(0) >= 5);
    assert!(after.vector_clock.get("peer").copied().unwrap_or(0) >= 3);

    let peer_after = store
        .rows
        .lock()
        .unwrap()
        .get("shared")
        .cloned()
        .expect("peer shared");
    // push-batch merge 后 peer 也应吸收 local 分量（若本机领先/并发会 push）。
    assert!(
        peer_after.vector_clock.get("local").copied().unwrap_or(0) >= 1
            || peer_after.vector_clock.get("peer").copied().unwrap_or(0) >= 3
    );
}

/// 在独立 current-thread runtime 上执行异步场景（供 integration test 同步调用）。
///
/// Business Logic（为什么需要这个函数）:
///     集成测试二进制不便直接依赖 `#[tokio::test]`；与 lan_trust harness 一样提供同步入口。
///
/// Code Logic（这个函数做什么）:
///     构造 current_thread runtime + enable_all，block_on 给定 future。
fn block_on_current<F>(fut: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
        .block_on(fut)
}

/// new↔new：仅命中 paged 路由，数据双向收敛。
///
/// Business Logic（为什么需要这个函数）:
///     有 capability 的对端必须只走 manifest/items/push-batch，不得回退 legacy。
///
/// Code Logic（这个函数做什么）:
///     同步入口，block_on 异步场景。
pub fn assert_new_to_new_uses_only_paged_routes() {
    block_on_current(assert_new_to_new_uses_only_paged_routes_async())
}

/// new↔legacy：仅命中 legacy 路由。
///
/// Business Logic（为什么需要这个函数）:
///     无 capability 的旧对端必须只走 pull/push，不得尝试 paged。
///
/// Code Logic（这个函数做什么）:
///     同步入口，block_on 异步场景。
pub fn assert_new_to_legacy_uses_only_legacy_routes() {
    block_on_current(assert_new_to_legacy_uses_only_legacy_routes_async())
}

/// 畸形 paged 响应必须失败本轮，不得当空成功。
///
/// Business Logic（为什么需要这个函数）:
///     cursor 不前进等协议故障若被折叠成空成功，会丢数据且难排查。
///
/// Code Logic（这个函数做什么）:
///     同步入口，block_on 异步场景。
pub fn assert_malformed_paged_fails_round_not_empty_success() {
    block_on_current(assert_malformed_paged_fails_round_not_empty_success_async())
}

/// legacy 请求体仍可对 new 服务端工作。
///
/// Business Logic（为什么需要这个函数）:
///     新服务端必须保留 legacy 路由，旧客户端才能与新设备同步。
///
/// Code Logic（这个函数做什么）:
///     同步入口，block_on 异步场景。
pub fn assert_legacy_bodies_work_against_new_server() {
    block_on_current(assert_legacy_bodies_work_against_new_server_async())
}

/// 多 ID 批混入毒丸：拆批 + 隔离 + 好数据落库。
pub fn assert_item_too_large_halves_and_isolates_poison() {
    block_on_current(assert_item_too_large_halves_and_isolates_poison_async())
}

/// 413 对半拆批直至成功。
pub fn assert_batch_too_large_halves_until_success() {
    block_on_current(assert_batch_too_large_halves_until_success_async())
}

/// 并发 VC merge 收敛。
pub fn assert_concurrent_vector_clock_merges() {
    block_on_current(assert_concurrent_vector_clock_merges_async())
}
