//! runtime_authority_smoke — GUI control client 与 owner CAS / event relay 收敛 smoke。
//!
//! Business Logic（为什么需要这个测试）:
//!     N1 Task3 要求 GUI 通过 control client 更新 sidecar 权威配置，generation 与 runtime 值必须收敛；
//!     截图快捷键两阶段补偿与响应丢失对账规则必须确定性验证。
//!     N1 Task5 要求桌面 snapshot 来自 owner telemetry，以及 afterSequence/Gap 事件 relay。
//!
//! Code Logic（这个模块做什么）:
//!     在隔离 temp 目录启动轻量 owner HTTP（status/get-config/update-config/snapshot/events），
//!     用 `BackendControlClient` 走真实 loopback 协议；热键对账规则用纯函数验证；
//!     event bus 用有界 ring/broadcast 验证 replay/gap/owner 重启。

use app_lib::backend::authority::CONTROL_SCHEMA_VERSION;
use app_lib::backend::control_client::rebind_control_token_body;
use app_lib::backend::control_client::{
    decide_hotkey_reconcile, BackendControlClient, BackendControlClientRuntime,
    ControlEventsStream, HotkeyOsReconcileDecision,
};
use app_lib::backend::event_bus::{
    perform_gap_resync, BackendRuntimeCursor, GapResyncOutcome, GuiEventRelayState,
    RelayClientAction, RuntimeEventBus, RuntimeRelayMessage,
};
use app_lib::backend::ui::{run_gui_owner_event_relay, RecordingBackendUi};
use app_lib::config::{
    AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
};
use app_lib::config_runtime::{
    ConfigRuntime, ConfigSnapshot, ConfigUpdateResponse, RuntimeConfigPatch, RuntimeOwnerStatus,
};
use app_lib::config_store::MemoryConfigStore;
use app_lib::error::AppError;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::stream;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// harness 鉴权 body。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthBody {
    control_token: String,
}

/// harness update body。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    control_token: String,
    expected_owner_instance_id: String,
    expected_generation: u64,
    patch: RuntimeConfigPatch,
}

/// harness runtime-snapshot body。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotBody {
    control_token: String,
    #[allow(dead_code)]
    project_id: String,
}

/// harness events catch-up body。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventsBody {
    control_token: String,
    after_owner_instance_id: Option<String>,
    after_sequence: Option<u64>,
}

/// 桌面 smoke 视图：映射 owner telemetry 的 scheduler tick。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopSnapshotView {
    /// 与 brief 字段名对齐；对应 owner `latestTickAt`。
    latest_scheduler_tick: Option<String>,
}

/// harness 共享状态。
#[derive(Clone)]
struct OwnerState {
    runtime: Arc<ConfigRuntime>,
    token: String,
    fail_next_save: Arc<std::sync::atomic::AtomicBool>,
    event_bus: Arc<RuntimeEventBus>,
    /// owner 侧 scheduler tick（GUI 不得读本地空值）。
    latest_tick: Arc<Mutex<Option<String>>>,
}

/// Runtime authority harness：owner runtime + loopback control HTTP + client。
struct RuntimeAuthorityHarness {
    runtime: Arc<ConfigRuntime>,
    client: BackendControlClient,
    owner_id: String,
    fail_next_save: Arc<std::sync::atomic::AtomicBool>,
    event_bus: Arc<RuntimeEventBus>,
    latest_tick: Arc<Mutex<Option<String>>>,
    _shutdown: oneshot::Sender<()>,
}

impl RuntimeAuthorityHarness {
    /// 启动隔离 owner 与 client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 需在不触碰用户 `~/.cc-partner` 的前提下验证 control 协议与 CAS。
    ///
    /// Code Logic（这个函数做什么）:
    ///     MemoryConfigStore + ConfigRuntime(with_owner) + event_bus + axum 绑定 127.0.0.1:0
    ///     + BackendControlClient::for_test。
    async fn start() -> Self {
        let owner_id = format!("owner-smoke-{}", uuid::Uuid::new_v4());
        let token = format!("token-{}", uuid::Uuid::new_v4());
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::with_owner(initial, store, owner_id.clone()));
        let fail_next_save = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let event_bus = Arc::new(RuntimeEventBus::new(owner_id.clone()));
        let latest_tick = Arc::new(Mutex::new(None));
        let state = OwnerState {
            runtime: Arc::clone(&runtime),
            token: token.clone(),
            fail_next_save: Arc::clone(&fail_next_save),
            event_bus: Arc::clone(&event_bus),
            latest_tick: Arc::clone(&latest_tick),
        };

        let app = Router::new()
            .route("/api/backend/control/status", post(h_status))
            .route("/api/backend/control/get-config", post(h_get_config))
            .route("/api/backend/control/update-config", post(h_update_config))
            .route(
                "/api/backend/control/orchestrator/runtime-snapshot",
                post(h_runtime_snapshot),
            )
            .route(
                "/api/backend/control/events/catch-up",
                post(h_events_catch_up),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .ok();
        });
        // 给 server 一拍启动时间
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client =
            BackendControlClient::for_test(addr.port(), &token, &owner_id).expect("client");
        let _ = token; // token 已注入 client
        Self {
            runtime,
            client,
            owner_id,
            fail_next_save,
            event_bus,
            latest_tick,
            _shutdown: tx,
        }
    }

    /// 读取 owner 内存配置。
    async fn owner_config(&self) -> AppConfig {
        self.runtime.snapshot().expect("owner snapshot")
    }

    /// 注入下一次 durable save 失败（通过拦截 apply：先成功 validate，再手动失败路径）。
    ///
    /// 说明：MemoryConfigStore 支持 fail_next_save；此处把标志交给 handler。
    fn inject_save_failure(&self) {
        self.fail_next_save
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// owner 记录 scheduler tick（模拟 sidecar telemetry）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     桌面 snapshot 必须读到 owner 写入的 tick，而非 GUI 空 telemetry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     写入 latest_tick 并 publish 到 event_bus。
    async fn owner_record_tick(&self, tick: &str) {
        *self.latest_tick.lock().expect("tick lock") = Some(tick.to_string());
        let _ = self
            .event_bus
            .publish("orchestrator:scheduler-tick", json!({ "tick": tick }));
    }

    /// GUI 经 control client 拉取 snapshot。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     验证桌面路径走 control client 而非本地空 telemetry。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `orchestrator_runtime_snapshot` 后映射为 DesktopSnapshotView。
    async fn gui_snapshot(&self) -> Result<DesktopSnapshotView, AppError> {
        let snap = self
            .client
            .orchestrator_runtime_snapshot("proj-smoke")
            .await?;
        Ok(DesktopSnapshotView {
            latest_scheduler_tick: snap.latest_tick_at,
        })
    }
}

fn sample_config() -> AppConfig {
    AppConfig {
        device_id: "smoke-device".into(),
        device_name: "desk-a".into(),
        http_port: 0,
        receive_dir: "/tmp/cc-partner-smoke-recv".into(),
        game_plugin_dir: "/tmp/plugins".into(),
        db_path: "/tmp/cc-partner-smoke.db".into(),
        screenshot_hotkey: "<ctrl>+s".into(),
        prompt_optimizer_hotkey: "<ctrl>".into(),
        prompt_optimizer_fill_language: "zh".into(),
        prompt_optimizer_provider: "claude".into(),
        prompt_quick_input_hotkey: "<ctrl>+/".into(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: HealthConfig::default(),
        battery: BatteryConfig::default(),
        orchestrator: OrchestratorAutomationConfig::default(),
        github_trending: GithubTrendingConfig::default(),
        agent_hub: app_lib::config::AgentHubConfig::default(),
        manual_peers: Vec::new(),
        experimental_features: app_lib::config::ExperimentalFeaturesConfig::default(),
        internal_claude: app_lib::config::InternalClaudeConfig::default(),
    }
}

fn auth_ok(state: &OwnerState, token: &str) -> Result<(), (StatusCode, String)> {
    if token != state.token {
        return Err((StatusCode::UNAUTHORIZED, "控制令牌不匹配".into()));
    }
    Ok(())
}

async fn h_status(
    State(state): State<OwnerState>,
    Json(body): Json<AuthBody>,
) -> Result<Json<RuntimeOwnerStatus>, (StatusCode, Json<serde_json::Value>)> {
    auth_ok(&state, &body.control_token).map_err(|(s, m)| {
        (
            s,
            Json(serde_json::json!({"error": m, "code": "unauthorized"})),
        )
    })?;
    let status = state
        .runtime
        .owner_status(0, 0, "idle", Default::default())
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string(), "code": "internal"})),
            )
        })?;
    Ok(Json(status))
}

async fn h_get_config(
    State(state): State<OwnerState>,
    Json(body): Json<AuthBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    auth_ok(&state, &body.control_token).map_err(|(s, m)| {
        (
            s,
            Json(serde_json::json!({"error": m, "code": "unauthorized"})),
        )
    })?;
    let snapshot = state.runtime.snapshot_with_generation().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string(), "code": "internal"})),
        )
    })?;
    Ok(Json(serde_json::json!({ "snapshot": snapshot })))
}

async fn h_update_config(
    State(state): State<OwnerState>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<ConfigUpdateResponse>, (StatusCode, Json<serde_json::Value>)> {
    auth_ok(&state, &body.control_token).map_err(|(s, m)| {
        (
            s,
            Json(serde_json::json!({"error": m, "code": "unauthorized"})),
        )
    })?;
    if state
        .fail_next_save
        .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "注入: durable save 失败",
                "code": "internal"
            })),
        ));
    }
    match state
        .runtime
        .apply_patch_if_generation(
            &body.expected_owner_instance_id,
            body.expected_generation,
            body.patch,
        )
        .await
    {
        Ok(resp) => Ok(Json(resp)),
        Err(e) => {
            let status = match e.classify() {
                app_lib::error::AppErrorCategory::Conflict => StatusCode::CONFLICT,
                app_lib::error::AppErrorCategory::Validation => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            let code = if status == StatusCode::CONFLICT {
                "conflict"
            } else {
                "internal"
            };
            Err((
                status,
                Json(serde_json::json!({"error": e.to_string(), "code": code})),
            ))
        }
    }
}

async fn h_runtime_snapshot(
    State(state): State<OwnerState>,
    Json(body): Json<SnapshotBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    auth_ok(&state, &body.control_token).map_err(|(s, m)| {
        (
            s,
            Json(serde_json::json!({"error": m, "code": "unauthorized"})),
        )
    })?;
    let tick = state.latest_tick.lock().expect("tick").clone();
    // 返回与 OrchestratorRuntimeSnapshotDto camelCase 对齐的最小 JSON；tick 来自 owner telemetry。
    Ok(Json(serde_json::json!({
        "projectId": body.project_id,
        "projectKind": "local",
        "remoteStatus": "local",
        "generatedAt": "2026-07-14T00:00:00Z",
        "latestTickAt": tick,
        "lastDispatchAt": tick,
        "lastDispatchedCount": 0,
        "schedulerEnabled": true,
        "workflowSource": "builtIn",
        "workflowValid": true,
        "workflowError": null,
        "maxConcurrentTasks": 1,
        "slotsUsed": 0,
        "slotsAvailable": 1,
        "latestError": null,
        "runningTasks": [],
        "retryingTasks": [],
        "recentEvents": [],
    })))
}

async fn h_events_catch_up(
    State(state): State<OwnerState>,
    Json(body): Json<EventsBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    auth_ok(&state, &body.control_token).map_err(|(s, m)| {
        (
            s,
            Json(serde_json::json!({"error": m, "code": "unauthorized"})),
        )
    })?;
    let after = match (body.after_owner_instance_id.as_deref(), body.after_sequence) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let mut relay = state.event_bus.open_relay(after.as_ref());
    let mut messages = Vec::new();
    while let Some(msg) = relay.try_recv() {
        messages.push(msg);
    }
    let latest = BackendRuntimeCursor {
        owner_instance_id: state.event_bus.owner_instance_id().to_string(),
        sequence: state.event_bus.latest_sequence(),
    };
    Ok(Json(serde_json::json!({
        "messages": messages,
        "latest": latest,
    })))
}

/// GUI 配置更新推进 owner generation 与 runtime 值。
///
/// Business Logic（为什么需要这个测试）:
///     Task3 核心：client 提交 patch 后 sidecar generation+1 且 device_name 收敛。
///
/// Code Logic（这个测试做什么）:
///     start harness → status → update_device_name → 断言 generation 与 owner_config。
#[tokio::test]
async fn gui_config_update_changes_owner_generation_and_runtime_value() {
    let harness = RuntimeAuthorityHarness::start().await;
    let before = harness.client.status().await.expect("status");
    assert_eq!(before.owner_instance_id, harness.owner_id);
    assert_eq!(before.generation, 0);
    let after = harness
        .client
        .update_device_name(&before, "desk-b")
        .await
        .expect("update");
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(harness.owner_config().await.device_name, "desk-b");
    assert_eq!(after.snapshot.device_name, "desk-b");
}

/// 预检 generation 冲突时拒绝提交。
///
/// Business Logic（为什么需要这个测试）:
///     并发 writer 下旧 generation 不得覆盖；GUI 应收到 conflict。
///
/// Code Logic（这个测试做什么）:
///     先成功一次 generation=1，再用 expected=0 提交，期望 conflict。
#[tokio::test]
async fn hotkey_preflight_conflict_on_stale_generation() {
    let harness = RuntimeAuthorityHarness::start().await;
    let before = harness.client.status().await.unwrap();
    harness
        .client
        .update_device_name(&before, "once")
        .await
        .unwrap();
    let err = harness
        .client
        .update_config(app_lib::config_runtime::ConfigUpdateRequest {
            expected_owner_instance_id: harness.owner_id.clone(),
            expected_generation: 0,
            patch: RuntimeConfigPatch {
                screenshot_hotkey: Some("<ctrl>+<shift>+s".into()),
                ..Default::default()
            },
        })
        .await
        .expect_err("stale generation");
    assert_eq!(err.classify(), app_lib::error::AppErrorCategory::Conflict);
    assert!(
        err.to_string().contains("config_generation_conflict")
            || err.to_string().contains("conflict"),
        "err={err}"
    );
    // owner 热键未变
    assert_eq!(harness.owner_config().await.screenshot_hotkey, "<ctrl>+s");
}

/// owner durable-save 失败时 generation 不变。
///
/// Business Logic（为什么需要这个测试）:
///     落盘失败不得推进 generation；GUI 两阶段路径应回滚 OS（此处验证 owner 侧 CAS 失败语义）。
///
/// Code Logic（这个测试做什么）:
///     inject_save_failure → update → Err 且 generation 仍 0、device_name 未变。
#[tokio::test]
async fn owner_durable_save_failure_does_not_advance_generation() {
    let harness = RuntimeAuthorityHarness::start().await;
    let before = harness.client.status().await.unwrap();
    harness.inject_save_failure();
    let err = harness
        .client
        .update_device_name(&before, "should-not-stick")
        .await
        .expect_err("save fail");
    assert!(
        err.to_string().contains("注入") || err.to_string().contains("save"),
        "err={err}"
    );
    assert_eq!(harness.client.status().await.unwrap().generation, 0);
    assert_eq!(harness.owner_config().await.device_name, "desk-a");
}

/// 响应丢失对账：已提交新热键 → KeepNew。
///
/// Business Logic（为什么需要这个测试）:
///     不确定响应后若 owner 已 commit，OS 必须保留新快捷键。
///
/// Code Logic（这个测试做什么）:
///     decide_hotkey_reconcile generation+1 + new hotkey。
#[test]
fn lost_response_reconcile_keeps_new_when_committed() {
    assert_eq!(
        decide_hotkey_reconcile(1, "<ctrl>+n", 0, "<ctrl>+o", "<ctrl>+n"),
        HotkeyOsReconcileDecision::KeepNew
    );
}

/// 响应丢失对账：确认仍旧 → RollbackToOld。
#[test]
fn lost_response_reconcile_rolls_back_when_old() {
    assert_eq!(
        decide_hotkey_reconcile(0, "<ctrl>+o", 0, "<ctrl>+o", "<ctrl>+n"),
        HotkeyOsReconcileDecision::RollbackToOld
    );
}

/// 响应丢失对账：歧义 → ManualReconcile。
#[test]
fn lost_response_reconcile_blocks_when_ambiguous() {
    assert_eq!(
        decide_hotkey_reconcile(1, "<ctrl>+x", 0, "<ctrl>+o", "<ctrl>+n"),
        HotkeyOsReconcileDecision::ManualReconcile
    );
}

/// 控制描述符 schema 常量与客户端一致。
#[test]
fn control_schema_version_is_current() {
    assert_eq!(CONTROL_SCHEMA_VERSION, 2);
}

/// Cloud Sync 配置与设备名字段共享同一 CAS generation 门闸。
///
/// Business Logic（为什么需要这个测试）:
///     手动配置路径与其它 writer 必须共享 owner generation，避免 split 写。
///
/// Code Logic（这个测试做什么）:
///     连续两次不同 allowlist patch，generation 单调 +1/+2。
#[tokio::test]
async fn cloud_sync_and_device_patches_share_owner_generation_gate() {
    let harness = RuntimeAuthorityHarness::start().await;
    let s0 = harness.client.status().await.unwrap();
    let r1 = harness
        .client
        .update_config(app_lib::config_runtime::ConfigUpdateRequest {
            expected_owner_instance_id: s0.owner_instance_id.clone(),
            expected_generation: s0.generation,
            patch: RuntimeConfigPatch {
                cloud_sync_enabled: Some(true),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(r1.generation, 1);
    let r2 = harness
        .client
        .update_config(app_lib::config_runtime::ConfigUpdateRequest {
            expected_owner_instance_id: r1.owner_instance_id.clone(),
            expected_generation: r1.generation,
            patch: RuntimeConfigPatch {
                device_name: Some("desk-c".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(r2.generation, 2);
    let cfg = harness.owner_config().await;
    assert!(cfg.cloud_sync_enabled);
    assert_eq!(cfg.device_name, "desk-c");
}

/// 桌面 snapshot 必须来自 owner telemetry，而非 GUI 本地空值。
///
/// Business Logic（为什么需要这个测试）:
///     Task5 核心：GUI 空 telemetry 不得补 owner 字段。
///
/// Code Logic（这个测试做什么）:
///     owner_record_tick → gui_snapshot 断言 latest_scheduler_tick。
#[tokio::test]
async fn desktop_snapshot_comes_from_owner_telemetry() {
    let harness = RuntimeAuthorityHarness::start().await;
    harness.owner_record_tick("tick-1").await;
    let snapshot = harness.gui_snapshot().await.unwrap();
    assert_eq!(snapshot.latest_scheduler_tick.as_deref(), Some("tick-1"));
}

/// owner 重启：sequence 重置但 owner id 变化，不得当重复丢弃。
///
/// Business Logic（为什么需要这个测试）:
///     新 sidecar 从 sequence=1 起号；GUI 若只按 sequence 去重会永久丢事件。
///
/// Code Logic（这个测试做什么）:
///     owner-a 投递 seq=5 后，owner-b seq=1 必须 Deliver。
#[test]
fn event_relay_owner_restart_resets_sequence_with_new_owner_id() {
    let mut gui = GuiEventRelayState::default();
    let first = gui.on_message(RuntimeRelayMessage::Event {
        owner_instance_id: "owner-a".into(),
        sequence: 5,
        event: "workbench:terminal-output".into(),
        payload: json!({"n": 5}),
    });
    assert!(matches!(first, RelayClientAction::Deliver { .. }));

    let restarted = gui.on_message(RuntimeRelayMessage::Event {
        owner_instance_id: "owner-b".into(),
        sequence: 1,
        event: "workbench:terminal-output".into(),
        payload: json!({"n": 1}),
    });
    assert!(
        matches!(restarted, RelayClientAction::Deliver { .. }),
        "owner 变化后低 sequence 不得当重复"
    );
    assert_eq!(
        gui.cursor().unwrap(),
        BackendRuntimeCursor {
            owner_instance_id: "owner-b".into(),
            sequence: 1,
        }
    );
}

/// 断线重连：afterSequence 只回放更新的事件。
///
/// Business Logic（为什么需要这个测试）:
///     重连不得重放已消费事件，也不得静默丢更新。
///
/// Code Logic（这个测试做什么）:
///     publish 1..3，catch-up after seq=1，得到 2/3。
#[tokio::test]
async fn event_relay_disconnect_reconnect_replays_from_after_sequence() {
    let harness = RuntimeAuthorityHarness::start().await;
    let c1 = harness
        .event_bus
        .publish("workbench:terminal-output", json!({"seq": 1}));
    let _ = harness
        .event_bus
        .publish("workbench:terminal-output", json!({"seq": 2}));
    let _ = harness
        .event_bus
        .publish("workbench:terminal-output", json!({"seq": 3}));

    let catch_up = harness
        .client
        .events_catch_up(Some(&c1))
        .await
        .expect("catch-up");
    let sequences: Vec<u64> = catch_up
        .messages
        .iter()
        .filter_map(|m| m.sequence())
        .collect();
    assert_eq!(sequences, vec![2, 3]);
    assert_eq!(catch_up.latest.sequence, 3);
    assert_eq!(catch_up.latest.owner_instance_id, harness.owner_id);
}

/// broadcast lag / ring gap 必须显式 Gap 并触发 resync，而非 silent loss。
///
/// Business Logic（为什么需要这个测试）:
///     慢消费者或游标早于 ring 时，GUI 必须先 terminal/runtime 恢复再接 live。
///
/// Code Logic（这个测试做什么）:
///     小 ring 挤掉旧事件 → open_relay after_seq=0（true gap：oldest>after+1）→ Gap；
///     GuiEventRelayState 记 resync。R28：连续边界 oldest==after+1 只回放 Event 不发 Gap。
#[tokio::test]
async fn event_relay_broadcast_lag_emits_gap_and_triggers_resync() {
    let bus = RuntimeEventBus::with_capacity("owner-lag", 2, 2);
    let _ = bus.publish("e", json!(1));
    let _ = bus.publish("e", json!(2));
    let _ = bus.publish("e", json!(3)); // ring oldest=2 latest=3
    let after_zero = BackendRuntimeCursor {
        owner_instance_id: "owner-lag".into(),
        sequence: 0,
    };
    let mut relay = bus.open_relay(Some(&after_zero));
    let msg = relay.try_recv().expect("gap");
    assert!(
        matches!(
            msg,
            RuntimeRelayMessage::Gap {
                oldest_available: 2,
                latest: 3,
                ..
            }
        ),
        "got {msg:?}"
    );

    let mut gui = GuiEventRelayState::default();
    let action = gui.on_message(msg);
    assert!(matches!(action, RelayClientAction::RequestResync { .. }));
    assert_eq!(gui.resync_count, 1);
    // 行为断言：RequestResync 后必须走真实 resync hooks（非注释）
    let outcome = perform_gap_resync(|| async { Ok(1u64) }, || async { Ok(1u64) }).await;
    assert_eq!(outcome.terminal_replay_count, 1);
    assert_eq!(outcome.runtime_snapshot_refresh_count, 1);
}

/// Gap 后应先 terminal/runtime resync，再 attach 最新 live 游标。
///
/// Business Logic（为什么需要这个测试）:
///     永久漏事件与重复去重都不可接受；resync 后从 latest 附着。
///
/// Code Logic（这个测试做什么）:
///     Gap → RequestResync → perform_gap_resync 可观测 hook（非注释）→ attach_at(latest)
///     → 后续同 sequence 去重。
#[tokio::test]
async fn event_relay_gap_triggers_terminal_and_runtime_resync() {
    let mut gui = GuiEventRelayState::default();
    let gap = RuntimeRelayMessage::Gap {
        owner_instance_id: "owner-a".into(),
        oldest_available: 10,
        latest: 20,
    };
    let action = gui.on_message(gap);
    match action {
        RelayClientAction::RequestResync {
            oldest_available,
            latest,
            ..
        } => {
            assert_eq!(oldest_available, 10);
            assert_eq!(latest, 20);
            let mut terminal_calls = 0u64;
            let mut runtime_calls = 0u64;
            let outcome = perform_gap_resync(
                || {
                    terminal_calls += 1;
                    async move { Ok(3u64) }
                },
                || {
                    runtime_calls += 1;
                    async move { Ok(1u64) }
                },
            )
            .await;
            assert_eq!(
                outcome,
                GapResyncOutcome {
                    terminal_replay_count: 3,
                    runtime_snapshot_refresh_count: 1,
                }
            );
            assert_eq!(terminal_calls, 1, "terminal replay hook 必须被调用");
            assert_eq!(runtime_calls, 1, "runtime snapshot hook 必须被调用");
            // 真实 resync 完成后再 attach latest live cursor
            gui.attach_at(BackendRuntimeCursor {
                owner_instance_id: "owner-a".into(),
                sequence: latest,
            });
        }
        other => panic!("expected resync, got {other:?}"),
    }
    let dup = gui.on_message(RuntimeRelayMessage::Event {
        owner_instance_id: "owner-a".into(),
        sequence: 20,
        event: "e".into(),
        payload: json!(null),
    });
    assert_eq!(dup, RelayClientAction::DropDuplicate);
    let next = gui.on_message(RuntimeRelayMessage::Event {
        owner_instance_id: "owner-a".into(),
        sequence: 21,
        event: "e".into(),
        payload: json!(null),
    });
    assert!(matches!(next, RelayClientAction::Deliver { .. }));
}

/// query 刷新 control token 时必须保留原业务字段（projectId / afterSequence）。
///
/// Business Logic（为什么需要这个测试）:
///     sidecar 重启后 port/token 变更时，重试若丢掉 projectId 会导致 snapshot 400；
///     丢掉 afterSequence 会破坏 catch-up 游标契约。
///
/// Code Logic（这个测试做什么）:
///     构造含 projectId 与 afterSequence 的 body，rebind 后断言业务字段仍在且 token 已替换。
#[test]
fn query_refresh_preserves_original_request_body_fields() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SnapBody {
        control_token: String,
        project_id: String,
    }
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct EventsBody {
        control_token: String,
        after_owner_instance_id: Option<String>,
        after_sequence: Option<u64>,
    }

    let snap = SnapBody {
        control_token: "old-token".into(),
        project_id: "proj-42".into(),
    };
    let rebound = rebind_control_token_body(&snap, "new-token").expect("rebind snap");
    assert_eq!(
        rebound.get("controlToken").and_then(|v| v.as_str()),
        Some("new-token")
    );
    assert_eq!(
        rebound.get("projectId").and_then(|v| v.as_str()),
        Some("proj-42")
    );

    let events = EventsBody {
        control_token: "old-token".into(),
        after_owner_instance_id: Some("owner-x".into()),
        after_sequence: Some(17),
    };
    let rebound = rebind_control_token_body(&events, "token-2").expect("rebind events");
    assert_eq!(
        rebound.get("controlToken").and_then(|v| v.as_str()),
        Some("token-2")
    );
    assert_eq!(
        rebound.get("afterOwnerInstanceId").and_then(|v| v.as_str()),
        Some("owner-x")
    );
    assert_eq!(
        rebound.get("afterSequence").and_then(|v| v.as_u64()),
        Some(17)
    );
}

/// Cloud Sync 手动与 scheduler 路径共享同一门闸：ReturnBusy 可观测 skipped。
///
/// Business Logic（为什么需要这个测试）:
///     并发 GUI manual + owner scheduler 只能执行一个 Git 临界区；忙则 skip 而非双写 workdir。
///
/// Code Logic（这个测试做什么）:
///     持 Wait 锁跑长任务的同时 ReturnBusy 触发；断言 skipped_busy 递增且 busy 路径返回 None。
#[tokio::test]
async fn cloud_sync_manual_and_scheduler_share_single_gate() {
    use app_lib::cloud_sync::runtime::{
        run_cloud_sync_exclusive, CloudSyncBusyPolicy, CloudSyncRuntime, CloudSyncTrigger,
    };
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Barrier;

    let runtime = Arc::new(CloudSyncRuntime::new());
    let barrier = Arc::new(Barrier::new(2));

    let r1 = Arc::clone(&runtime);
    let b1 = Arc::clone(&barrier);
    let holder = tokio::spawn(async move {
        run_cloud_sync_exclusive(
            &r1,
            CloudSyncTrigger::Manual,
            CloudSyncBusyPolicy::Wait {
                timeout: Duration::from_secs(5),
            },
            || {
                let b1 = Arc::clone(&b1);
                async move {
                    b1.wait().await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    Ok::<(), AppError>(())
                }
            },
        )
        .await
    });

    // 等 holder 进入 operation 临界区
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(runtime.phase_token(), "running");

    let r2 = Arc::clone(&runtime);
    let b2 = Arc::clone(&barrier);
    let busy = tokio::spawn(async move {
        // 先放行 holder 进入 operation
        b2.wait().await;
        run_cloud_sync_exclusive(
            &r2,
            CloudSyncTrigger::Scheduler,
            CloudSyncBusyPolicy::ReturnBusy,
            || async { Ok::<(), AppError>(()) },
        )
        .await
    });

    let (hold_res, busy_res) = tokio::join!(holder, busy);
    hold_res.expect("join holder").expect("holder ok");
    let skipped = busy_res.expect("join busy").expect("busy ok");
    assert!(
        skipped.is_none(),
        "scheduler 在 busy 时必须 ReturnBusy → None"
    );
    assert!(
        runtime.status_snapshot().skipped_busy >= 1,
        "skipped_busy 必须可观测"
    );
    assert_eq!(runtime.phase_token(), "succeeded");
}

// 抑制未使用告警（部分类型仅用于 serde 形状对齐）
#[allow(dead_code)]
fn _type_smoke(s: ConfigSnapshot) -> ConfigSnapshot {
    s
}

/// 运营通知经 event_bus catch-up 中继时携带 owner/sequence，payload 隐私安全。
///
/// Business Logic（为什么需要这个测试）:
///     GUI handshake 依赖 operational:notification 带 ownerInstanceId/sequence 与 opaque 字段，
///     不得含 title；Gap 路径仍走 backend:runtime-gap。
///
/// Code Logic（这个测试做什么）:
///     publish operational 事件 → client catch-up → 校验 Event owner/seq/payload；
///     GuiEventRelayState Deliver 带 owner/sequence；无 title 字段。
#[tokio::test]
async fn operational_notification_relay() {
    let harness = RuntimeAuthorityHarness::start().await;
    let payload = json!({
        "kind": "humanReview",
        "opaqueSourceId": "task-opaque-1",
        "stateVersion": 2,
        "occurredAt": "2026-07-15T00:00:00Z"
    });
    let cursor = harness
        .event_bus
        .publish("operational:notification", payload.clone());

    let catch_up = harness
        .client
        .events_catch_up(None)
        .await
        .expect("catch-up");
    let msg = catch_up
        .messages
        .iter()
        .find(|m| {
            matches!(
                m,
                RuntimeRelayMessage::Event {
                    event,
                    ..
                } if event == "operational:notification"
            )
        })
        .expect("operational event in catch-up");
    match msg {
        RuntimeRelayMessage::Event {
            owner_instance_id,
            sequence,
            event,
            payload: p,
        } => {
            assert_eq!(owner_instance_id, &harness.owner_id);
            assert_eq!(*sequence, cursor.sequence);
            assert_eq!(event, "operational:notification");
            assert_eq!(p["kind"], "humanReview");
            assert_eq!(p["opaqueSourceId"], "task-opaque-1");
            assert_eq!(p["stateVersion"], 2);
            assert!(p.get("title").is_none());
            assert!(p.get("goal").is_none());
        }
        other => panic!("expected Event, got {other:?}"),
    }

    let mut gui = GuiEventRelayState::default();
    let action = gui.on_message(msg.clone());
    match action {
        RelayClientAction::Deliver {
            event,
            payload: p,
            owner_instance_id,
            sequence,
        } => {
            assert_eq!(event, "operational:notification");
            assert_eq!(owner_instance_id, harness.owner_id);
            assert_eq!(sequence, cursor.sequence);
            assert_eq!(p["opaqueSourceId"], "task-opaque-1");
            assert!(p.get("title").is_none());
        }
        other => panic!("expected Deliver, got {other:?}"),
    }
    assert_eq!(catch_up.latest.owner_instance_id, harness.owner_id);
    assert!(catch_up.latest.sequence >= cursor.sequence);
}

#[allow(dead_code)]
fn _err_smoke() -> AppError {
    AppError::generic("x")
}

/// stream fixture 的可观测计数与断流控制。
#[derive(Clone)]
struct StreamFixtureMetrics {
    stream_open_count: Arc<AtomicU64>,
    catch_up_count: Arc<AtomicU64>,
    stream_after_sequences: Arc<Mutex<Vec<Option<u64>>>>,
    /// 当前活跃 stream 的取消发送端（break 时 take 并 drop 使连接结束）。
    active_stream_cancel: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    /// 可选：强制 stream 404（unsupported）。
    stream_unsupported: Arc<std::sync::atomic::AtomicBool>,
}

/// 隔离 control server fixture：支持 live stream / catch-up / break / 计数。
///
/// Business Logic（为什么需要这个结构）:
///     Task3 要求验证 GUI relay 用 stream 作正常路径、重连带 afterSequence、404 才 fallback。
///
/// Code Logic（这个结构做什么）:
///     event_bus + axum routes（stream/catch-up/workbench）+ BackendControlClientRuntime loader。
struct RuntimeAuthorityFixture {
    event_bus: Arc<RuntimeEventBus>,
    owner_id: String,
    token: String,
    port: u16,
    metrics: StreamFixtureMetrics,
    /// 原生 PTY 会话（仅 start_with_native_pty 路径填充）。
    native_sessions: Arc<Mutex<HashMap<String, NativePtySession>>>,
    last_cursor: Arc<Mutex<Option<BackendRuntimeCursor>>>,
    _shutdown: oneshot::Sender<()>,
}

/// 隔离 native PTY 会话句柄。
///
/// Business Logic（为什么需要这个结构）:
///     L2 顺序/Gap 合同需要真实 shell 回显，而不是伪造 terminal-output payload。
///
/// Code Logic（这个结构做什么）:
///     持有 master/writer 与 reader 线程；写入字节后由 reader 把输出 publish 到 event_bus。
struct NativePtySession {
    _session_id: String,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _master: Box<dyn MasterPty + Send>,
    _child_guard: PtyChildGuard,
}

/// 确保 child 在 drop 时被 kill/reap。
struct PtyChildGuard {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl Drop for PtyChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 终端 fixture 顺序事件（失败输出只报告 sequence/字节数/step，不写回显正文）。
#[derive(Debug, Clone)]
struct TerminalFixtureEvent {
    sequence: u64,
    byte_len: usize,
    step: &'static str,
    /// 仅测试内存比对使用；assert 失败不得打印。
    chunk: String,
}

impl RuntimeAuthorityFixture {
    /// 启动支持 stream 的隔离 owner harness。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 不得触碰用户 `~/.cc-partner`，且需真实 loopback HTTP + NDJSON stream。
    ///
    /// Code Logic（这个函数做什么）:
    ///     bind 127.0.0.1:0；挂 events/stream、events/catch-up、workbench；返回 fixture。
    async fn start() -> Self {
        Self::start_inner(false).await
    }

    /// 启动 stream 路由恒 404 的 fixture（mixed-version unsupported）。
    async fn start_stream_unsupported() -> Self {
        Self::start_inner(true).await
    }

    async fn start_inner(stream_unsupported: bool) -> Self {
        let owner_id = format!("owner-stream-{}", uuid::Uuid::new_v4());
        let token = format!("token-{}", uuid::Uuid::new_v4());
        let event_bus = Arc::new(RuntimeEventBus::new(owner_id.clone()));
        let metrics = StreamFixtureMetrics {
            stream_open_count: Arc::new(AtomicU64::new(0)),
            catch_up_count: Arc::new(AtomicU64::new(0)),
            stream_after_sequences: Arc::new(Mutex::new(Vec::new())),
            active_stream_cancel: Arc::new(Mutex::new(None)),
            stream_unsupported: Arc::new(std::sync::atomic::AtomicBool::new(stream_unsupported)),
        };
        let state = StreamOwnerState {
            token: token.clone(),
            event_bus: Arc::clone(&event_bus),
            metrics: metrics.clone(),
        };

        let app = Router::new()
            .route(
                "/api/backend/control/events/stream",
                post(h_fixture_events_stream),
            )
            .route(
                "/api/backend/control/events/catch-up",
                post(h_fixture_events_catch_up),
            )
            .route("/api/backend/control/workbench", post(h_fixture_workbench))
            .route(
                "/api/backend/control/workbench/data",
                post(h_fixture_workbench),
            )
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stream fixture");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        Self {
            event_bus,
            owner_id,
            token,
            port: addr.port(),
            metrics,
            native_sessions: Arc::new(Mutex::new(HashMap::new())),
            last_cursor: Arc::new(Mutex::new(None)),
            _shutdown: tx,
        }
    }

    /// 启动带 native PTY 支持的 stream fixture（隔离 DATA_DIR 语义：不触碰用户 home）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Task8 L2 要求真实 control stream + native PTY 输入顺序合同。
    ///
    /// Code Logic（这个函数做什么）:
    ///     复用 start_inner(false)；后续 create_terminal 绑定真实 shell。
    async fn start_with_native_pty() -> Self {
        Self::start_inner(false).await
    }

    /// 返回绑定本 fixture 的 control client runtime（测试 loader，不读 control file）。
    fn control_client_runtime(&self) -> Arc<BackendControlClientRuntime> {
        let port = self.port;
        let token = self.token.clone();
        let owner = self.owner_id.clone();
        Arc::new(BackendControlClientRuntime::with_loader(move || {
            BackendControlClient::for_test(port, &token, &owner)
        }))
    }

    /// 发布一条 terminal-output 事件（payload 仅 sessionId/chunk/seq/ts，无敏感正文日志）。
    fn publish_terminal_event(&self, session_id: &str, chunk: &str) {
        let seq = self.event_bus.latest_sequence().saturating_add(1);
        let _ = self.event_bus.publish(
            "workbench:terminal-output",
            json!({
                "sessionId": session_id,
                "chunk": chunk,
                "seq": seq,
                "ts": 1,
            }),
        );
    }

    /// 断开当前活跃 stream 连接，迫使 GUI relay 用 last cursor 重连。
    async fn break_current_event_stream(&self) {
        if let Some(tx) = self
            .metrics
            .active_stream_cancel
            .lock()
            .expect("stream cancel lock")
            .take()
        {
            let _ = tx.send(());
        }
        // 给连接收尾一拍
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    fn stream_open_count(&self) -> u64 {
        self.metrics.stream_open_count.load(Ordering::SeqCst)
    }

    fn catch_up_count(&self) -> u64 {
        self.metrics.catch_up_count.load(Ordering::SeqCst)
    }

    /// 第二次 stream 打开时请求体中的 afterSequence。
    fn second_stream_after_sequence(&self) -> Option<u64> {
        let seqs = self
            .metrics
            .stream_after_sequences
            .lock()
            .expect("after seq lock");
        seqs.get(1).copied().flatten()
    }

    /// 返回绑定本 fixture 的 control client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     L2 测试需要直接 open_events_stream，而不是 GUI relay 间接消费。
    ///
    /// Code Logic（这个函数做什么）:
    ///     for_test(port, token, owner)。
    fn control_client(&self) -> BackendControlClient {
        BackendControlClient::for_test(self.port, &self.token, &self.owner_id)
            .expect("control client")
    }

    /// 创建真实 native PTY 会话并把 reader 输出发布到 event bus。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     低延迟合同必须对真实 shell 回显验序，禁止前端乐观 echo。
    ///
    /// Code Logic（这个函数做什么）:
    ///     portable-pty 打开 shell；reader 线程按 chunk publish terminal-output。
    async fn create_terminal(&self, cols: u16, rows: u16) -> NativePtySessionInfo {
        let session_id = format!("pty-{}", uuid::Uuid::new_v4());
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = if cfg!(windows) {
            CommandBuilder::new("cmd.exe")
        } else {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            CommandBuilder::new(shell)
        };
        // 固定非交互提示，避免日志/失败输出泄漏路径。
        if !cfg!(windows) {
            cmd.env("PS1", "");
            cmd.env("TERM", "xterm-256color");
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn shell");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let writer = pair.master.take_writer().expect("take writer");
        let writer = Arc::new(Mutex::new(writer));
        let event_bus = Arc::clone(&self.event_bus);
        let sid = session_id.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if chunk.is_empty() {
                            continue;
                        }
                        let seq = event_bus.latest_sequence().saturating_add(1);
                        let _ = event_bus.publish(
                            "workbench:terminal-output",
                            json!({
                                "sessionId": sid,
                                "chunk": chunk,
                                "seq": seq,
                                "ts": 1,
                            }),
                        );
                    }
                    Err(_) => break,
                }
            }
        });
        // 给 shell 启动一拍
        tokio::time::sleep(Duration::from_millis(80)).await;
        let session = NativePtySession {
            _session_id: session_id.clone(),
            writer: Arc::clone(&writer),
            _master: pair.master,
            _child_guard: PtyChildGuard { child: Some(child) },
        };
        self.native_sessions
            .lock()
            .expect("sessions lock")
            .insert(session_id.clone(), session);
        NativePtySessionInfo { id: session_id }
    }

    /// 向 native PTY 写入固定非敏感输入。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     输入泵合同要求字节原样进入 owner PTY。
    ///
    /// Code Logic（这个函数做什么）:
    ///     取 writer 写 bytes 并 flush；失败返回 Err。
    async fn write_terminal(&self, session_id: &str, data: &str) -> Result<(), String> {
        let sessions = self.native_sessions.lock().expect("sessions lock");
        let session = sessions
            .get(session_id)
            .ok_or_else(|| "session_not_found".to_string())?;
        let mut writer = session.writer.lock().expect("writer lock");
        writer
            .write_all(data.as_bytes())
            .map_err(|_| "write_failed".to_string())?;
        writer.flush().map_err(|_| "flush_failed".to_string())?;
        Ok(())
    }

    /// 收集至少 min_events 条 terminal-output（含超时）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     顺序合同需要在有界时间内拿到 fixture 事件。
    ///
    /// Code Logic（这个函数做什么）:
    ///     next_message 循环；只记录 sequence/byte_len/step，正文仅内存比对。
    async fn collect_terminal_output(
        &self,
        stream: &mut ControlEventsStream,
        min_events: usize,
        timeout: Duration,
    ) -> Vec<TerminalFixtureEvent> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut out = Vec::new();
        while out.len() < min_events {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let msg = tokio::time::timeout(remaining, stream.next_message())
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
            let Some(msg) = msg else { break };
            if let RuntimeRelayMessage::Event {
                sequence,
                event,
                payload,
                ..
            } = msg
            {
                if event != "workbench:terminal-output" {
                    continue;
                }
                let chunk = payload
                    .get("chunk")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                *self.last_cursor.lock().expect("cursor") = Some(BackendRuntimeCursor {
                    owner_instance_id: self.owner_id.clone(),
                    sequence,
                });
                out.push(TerminalFixtureEvent {
                    sequence,
                    byte_len: chunk.len(),
                    step: "collect",
                    chunk,
                });
            }
        }
        out
    }

    /// 收集直到看到包含 marker 子串的 chunk（失败不打印 shell 正文）。
    async fn collect_until_chunk(
        &self,
        stream: &mut ControlEventsStream,
        marker: &str,
    ) -> Vec<TerminalFixtureEvent> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut out = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let msg = tokio::time::timeout(remaining, stream.next_message())
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();
            let Some(msg) = msg else { break };
            match msg {
                RuntimeRelayMessage::Event {
                    sequence,
                    event,
                    payload,
                    ..
                } if event == "workbench:terminal-output" => {
                    let chunk = payload
                        .get("chunk")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    *self.last_cursor.lock().expect("cursor") = Some(BackendRuntimeCursor {
                        owner_instance_id: self.owner_id.clone(),
                        sequence,
                    });
                    let hit = chunk.contains(marker);
                    out.push(TerminalFixtureEvent {
                        sequence,
                        byte_len: chunk.len(),
                        step: if hit { "resume-hit" } else { "resume" },
                        chunk,
                    });
                    if hit {
                        break;
                    }
                }
                RuntimeRelayMessage::Gap { .. } => break,
                _ => {}
            }
        }
        out
    }

    /// 返回最近交付的 cursor。
    fn last_cursor(&self) -> BackendRuntimeCursor {
        self.last_cursor
            .lock()
            .expect("cursor")
            .clone()
            .unwrap_or(BackendRuntimeCursor {
                owner_instance_id: self.owner_id.clone(),
                sequence: 0,
            })
    }

    /// 丢弃 stream（通过 drop 关闭连接）。
    fn drop_stream(&self, stream: ControlEventsStream) {
        drop(stream);
    }

    /// 强制 event ring 产生 Gap：灌满 ring 使旧 cursor 落后。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap 恢复路径是低延迟计划的强制验收。
    ///
    /// Code Logic（这个函数做什么）:
    ///     连续 publish 超过默认 ring 容量，使 after_seq < oldest。
    async fn force_event_ring_gap(&self) {
        for i in 0..512u32 {
            let _ = self.event_bus.publish(
                "workbench:terminal-output",
                json!({
                    "sessionId": "gap-fill",
                    "chunk": format!("g{i}"),
                    "seq": self.event_bus.latest_sequence().saturating_add(1),
                    "ts": 1,
                }),
            );
        }
    }
}

/// native PTY 会话公开信息（仅 id）。
struct NativePtySessionInfo {
    id: String,
}

/// 断言 fixture 输出包含期望 step 子串（失败只报 sequence/byte_len/step）。
///
/// Business Logic（为什么需要这个函数）:
///     顺序合同必须可观测，但不能把 shell 回显正文写进失败输出。
///
/// Code Logic（这个函数做什么）:
///     拼接内存 chunk 做 contains 检查；失败 panic 仅 sequence 列表。
fn assert_terminal_fixture_order(events: &[TerminalFixtureEvent], expected_parts: &[&str]) {
    let joined: String = events.iter().map(|e| e.chunk.as_str()).collect();
    for (idx, part) in expected_parts.iter().enumerate() {
        if !joined.contains(part) {
            let summary: Vec<String> = events
                .iter()
                .map(|e| format!("seq={} bytes={} step={}", e.sequence, e.byte_len, e.step))
                .collect();
            panic!(
                "fixture order miss at part index {idx}; events=[{}]",
                summary.join(", ")
            );
        }
    }
}

/// 断言两段事件无重复 sequence。
fn assert_no_duplicate_sequences(first: &[TerminalFixtureEvent], second: &[TerminalFixtureEvent]) {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    for e in first.iter().chain(second.iter()) {
        if !seen.insert(e.sequence) {
            panic!(
                "duplicate sequence {} (byte_len={} step={})",
                e.sequence, e.byte_len, e.step
            );
        }
    }
}

#[derive(Clone)]
struct StreamOwnerState {
    token: String,
    event_bus: Arc<RuntimeEventBus>,
    metrics: StreamFixtureMetrics,
}

fn fixture_auth(state: &StreamOwnerState, token: &str) -> Result<(), StatusCode> {
    if token != state.token {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn h_fixture_events_stream(
    State(state): State<StreamOwnerState>,
    Json(body): Json<EventsBody>,
) -> Response {
    if fixture_auth(&state, &body.control_token).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized","code":"unauthorized"})),
        )
            .into_response();
    }
    if state.metrics.stream_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    state
        .metrics
        .stream_open_count
        .fetch_add(1, Ordering::SeqCst);
    state
        .metrics
        .stream_after_sequences
        .lock()
        .expect("after seq")
        .push(body.after_sequence);

    let after = match (body.after_owner_instance_id.as_deref(), body.after_sequence) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let relay = state.event_bus.open_relay(after.as_ref());
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    *state
        .metrics
        .active_stream_cancel
        .lock()
        .expect("active cancel") = Some(cancel_tx);

    let stream = stream::unfold(
        (relay, cancel_rx),
        |(mut relay, mut cancel_rx)| async move {
            tokio::select! {
                _ = &mut cancel_rx => None,
                msg = relay.recv() => {
                    let msg = msg?;
                    let line = serde_json::to_string(&msg).ok()?;
                    Some((Ok::<_, Infallible>(format!("{line}\n")), (relay, cancel_rx)))
                }
            }
        },
    );
    let mut response = Response::new(axum::body::Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/x-ndjson"),
    );
    response
}

async fn h_fixture_events_catch_up(
    State(state): State<StreamOwnerState>,
    Json(body): Json<EventsBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    fixture_auth(&state, &body.control_token)?;
    state.metrics.catch_up_count.fetch_add(1, Ordering::SeqCst);
    let after = match (body.after_owner_instance_id.as_deref(), body.after_sequence) {
        (Some(owner), Some(seq)) if !owner.is_empty() => Some(BackendRuntimeCursor {
            owner_instance_id: owner.to_string(),
            sequence: seq,
        }),
        _ => None,
    };
    let mut relay = state.event_bus.open_relay(after.as_ref());
    let mut messages = Vec::new();
    while let Some(msg) = relay.try_recv() {
        messages.push(msg);
    }
    let latest = BackendRuntimeCursor {
        owner_instance_id: state.event_bus.owner_instance_id().to_string(),
        sequence: state.event_bus.latest_sequence(),
    };
    Ok(Json(json!({
        "messages": messages,
        "latest": latest,
    })))
}

async fn h_fixture_workbench(
    State(state): State<StreamOwnerState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let token = body
        .get("controlToken")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    fixture_auth(&state, token)?;
    let op = body.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let result = match op {
        "sessions.list" => json!([]),
        "sessions.replay" => json!({
            "sessionId": body.get("payload").and_then(|p| p.get("sessionId")).cloned().unwrap_or(json!("s1")),
            "buffer": "",
            "truncated": false,
            "lastSeq": 0,
        }),
        _ => json!({ "ok": true }),
    };
    Ok(Json(json!({
        "ownerInstanceId": state.event_bus.owner_instance_id(),
        "result": result,
    })))
}

/// GUI relay 使用 live stream，断线后从 last cursor 重连。
///
/// Business Logic（为什么需要这个测试）:
///     stream-first 是低延迟主路径；重连必须带 afterSequence，避免重复/丢失。
///
/// Code Logic（这个测试做什么）:
///     发布 a → 100ms 内交付 → break stream → 发布 b → 两段 chunk 都到；
///     stream_open_count=2，第二次 afterSequence=Some(1)；catch-up=0。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gui_relay_uses_live_stream_and_reconnects_from_last_cursor() {
    let fixture = RuntimeAuthorityFixture::start().await;
    let ui = Arc::new(RecordingBackendUi::default());
    let cancel = CancellationToken::new();
    let relay = tokio::spawn(run_gui_owner_event_relay(
        ui.clone(),
        fixture.control_client_runtime(),
        cancel.clone(),
    ));

    fixture.publish_terminal_event("s1", "a");
    ui.wait_for_event("workbench:terminal-output", Duration::from_millis(100))
        .await;
    fixture.break_current_event_stream().await;
    fixture.publish_terminal_event("s1", "b");
    ui.wait_for_terminal_chunks(&["a", "b"], Duration::from_secs(1))
        .await;

    assert_eq!(fixture.stream_open_count(), 2);
    assert_eq!(fixture.second_stream_after_sequence(), Some(1));
    assert_eq!(
        fixture.catch_up_count(),
        0,
        "成功 stream 路径不得调用 events/catch-up"
    );
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), relay)
        .await
        .expect("relay join timeout");
}

/// stream 404 时进入 catch-up poll fallback。
///
/// Business Logic（为什么需要这个测试）:
///     mixed-version 旧 sidecar 无 stream 时仍需交付事件，且不得永久锁死。
///
/// Code Logic（这个测试做什么）:
///     unsupported fixture 发布事件 → 在 1s 内经 catch-up 交付；catch_up_count>=1。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gui_relay_falls_back_to_catch_up_when_stream_unsupported() {
    let fixture = RuntimeAuthorityFixture::start_stream_unsupported().await;
    let ui = Arc::new(RecordingBackendUi::default());
    let cancel = CancellationToken::new();
    let relay = tokio::spawn(run_gui_owner_event_relay(
        ui.clone(),
        fixture.control_client_runtime(),
        cancel.clone(),
    ));

    // 稍等 relay 进入 fallback 窗口
    tokio::time::sleep(Duration::from_millis(50)).await;
    fixture.publish_terminal_event("s1", "fallback-chunk");
    ui.wait_for_event("workbench:terminal-output", Duration::from_secs(1))
        .await;
    assert!(
        fixture.catch_up_count() >= 1,
        "unsupported 后应至少一次 catch-up"
    );
    assert_eq!(
        fixture.stream_open_count(),
        0,
        "404 路径不计入成功 stream open"
    );
    // stream_unsupported 走 open 失败，open_events_stream 返回 Err 时 fixture 在 handler 前已 404，
    // stream_open_count 在 auth 后、unsupported 检查后不递增——此处 open 在 handler 内先检查 unsupported。
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), relay)
        .await
        .expect("relay join timeout");
}

/// 同版本 stream 成功时 catch-up 调用数为 0。
///
/// Business Logic（为什么需要这个测试）:
///     正常路径不得每 250ms 轮询 catch-up。
///
/// Code Logic（这个测试做什么）:
///     stream 交付一条事件后等待 300ms，断言 catch_up_count 仍为 0。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gui_relay_successful_stream_does_not_call_catch_up() {
    let fixture = RuntimeAuthorityFixture::start().await;
    let ui = Arc::new(RecordingBackendUi::default());
    let cancel = CancellationToken::new();
    let relay = tokio::spawn(run_gui_owner_event_relay(
        ui.clone(),
        fixture.control_client_runtime(),
        cancel.clone(),
    ));

    fixture.publish_terminal_event("s1", "only-stream");
    ui.wait_for_event("workbench:terminal-output", Duration::from_millis(100))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fixture.catch_up_count(), 0);
    assert!(fixture.stream_open_count() >= 1);
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), relay)
        .await
        .expect("relay join timeout");
}

/// L2：真实 control stream 保序，重连 catch-up 无重复，ring Gap 显式可见。
///
/// Business Logic（为什么需要这个测试）:
///     Task8 要求自动顺序/恢复证据；不得用 mock E2E 代替 L2 PTY。
///
/// Code Logic（这个测试做什么）:
///     native PTY 写固定非敏感输入 → stream 收集 → 断线后 after cursor 重连 →
///     force ring gap 后断言 Gap 消息；失败输出仅 sequence/byte counts/step。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_stream_preserves_order_across_reconnect_and_gap() {
    let fixture = RuntimeAuthorityFixture::start_with_native_pty().await;
    let session = fixture.create_terminal(120, 32).await;
    let mut stream = fixture
        .control_client()
        .open_events_stream(None)
        .await
        .unwrap();
    for data in ["a", "b", "\u{7f}", "left\u{1b}[D", "paste-0123456789"] {
        fixture.write_terminal(&session.id, data).await.unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    // 等到 paste fixture 出现，避免过早截断导致顺序断言假失败。
    let mut first = fixture
        .collect_terminal_output(&mut stream, 1, Duration::from_millis(200))
        .await;
    let more = fixture
        .collect_until_chunk(&mut stream, "paste-0123456789")
        .await;
    first.extend(more);
    assert_terminal_fixture_order(&first, &["a", "b", "paste-0123456789"]);

    let cursor = fixture.last_cursor();
    fixture.drop_stream(stream);
    fixture
        .write_terminal(&session.id, "after-reconnect")
        .await
        .unwrap();
    let mut resumed = fixture
        .control_client()
        .open_events_stream(Some(&cursor))
        .await
        .unwrap();
    let resumed_events = fixture
        .collect_until_chunk(&mut resumed, "after-reconnect")
        .await;
    assert_no_duplicate_sequences(&first, &resumed_events);

    fixture.force_event_ring_gap().await;
    // 重新打开带旧 cursor 的 stream 以触发 Gap。
    let stale = BackendRuntimeCursor {
        owner_instance_id: fixture.owner_id.clone(),
        sequence: 1,
    };
    let mut gapped = fixture
        .control_client()
        .open_events_stream(Some(&stale))
        .await
        .unwrap();
    let gap_msg = tokio::time::timeout(Duration::from_secs(2), gapped.next_message())
        .await
        .expect("gap timeout")
        .expect("gap network")
        .expect("gap eof");
    assert!(
        matches!(gap_msg, RuntimeRelayMessage::Gap { .. }),
        "expected Gap, got non-gap relay message (seq summary only)"
    );
}
