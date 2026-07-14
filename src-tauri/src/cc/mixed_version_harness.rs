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
use crate::cc::models::ClaudeHistoryRow;
use crate::config::{
    AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
};
use crate::models::device::Device;
use crate::net::peer_client::PeerClient;
use crate::net::protocol::{CAPABILITY_CC_HISTORY_PAGED_SYNC_V1, PROTOCOL_VERSION_V1};
use crate::net::routes::health::HealthResponse;
use crate::orchestrator::repo::OrchestratorRepo;
use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
use crate::state::AppState;
use crate::storage::{
    ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
    WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo, WorkbenchWorktreeRepo,
};
use crate::transfer::registry::TransferRegistry;
use axum::extract::State as AxumState;
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
    let cursor = body
        .get("cursor")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let mut rows: Vec<ClaudeHistoryRow> = st.store.rows.lock().unwrap().values().cloned().collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    let start = match cursor {
        Some(c) => rows.iter().position(|r| r.id > c).unwrap_or(rows.len()),
        None => 0,
    };
    let page: Vec<_> = rows.into_iter().skip(start).take(256).collect();
    let done = page.len() < 256;
    let next_cursor = if done {
        None
    } else {
        page.last().map(|r| r.id.clone())
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
        if let Some(r) = guard.get(&id) {
            items.push(r.clone());
        } else {
            missing.push(id);
        }
    }
    Json(serde_json::json!({"items": items, "missing_ids": missing}))
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
    let n = items.len();
    for item in items {
        guard.insert(item.id.clone(), item);
    }
    Json(serde_json::json!({"accepted": n}))
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

    AppState {
        config: Arc::new(RwLock::new(config)),
        db: pool.clone(),
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
        update_status: Arc::new(RwLock::new(
            crate::commands::updater::UpdateDownloadStatus::default(),
        )),
        update_pending: Arc::new(Mutex::new(None)),
        update_bytes: Arc::new(Mutex::new(None)),
        update_download_task: Arc::new(Mutex::new(None)),
        update_cancel_token: Arc::new(Mutex::new(None)),
        cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
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
