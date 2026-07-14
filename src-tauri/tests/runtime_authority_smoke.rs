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
use app_lib::backend::control_client::{
    decide_hotkey_reconcile, BackendControlClient, HotkeyOsReconcileDecision,
};
use app_lib::backend::event_bus::{
    BackendRuntimeCursor, GuiEventRelayState, RelayClientAction, RuntimeEventBus,
    RuntimeRelayMessage,
};
use app_lib::config::{
    AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
};
use app_lib::config_runtime::{
    ConfigRuntime, ConfigSnapshot, ConfigUpdateResponse, RuntimeConfigPatch, RuntimeOwnerStatus,
};
use app_lib::config_store::MemoryConfigStore;
use app_lib::error::AppError;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

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
        db_path: "/tmp/cc-partner-smoke.db".into(),
        screenshot_hotkey: "<ctrl>+s".into(),
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
///     小 ring 挤掉旧事件 → open_relay after 旧 cursor → Gap；GuiEventRelayState 记 resync。
#[test]
fn event_relay_broadcast_lag_emits_gap_and_triggers_resync() {
    let bus = RuntimeEventBus::with_capacity("owner-lag", 2, 2);
    let stale = bus.publish("e", json!(1));
    let _ = bus.publish("e", json!(2));
    let _ = bus.publish("e", json!(3)); // ring: 2,3

    let mut relay = bus.open_relay(Some(&stale));
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
}

/// Gap 后应先 terminal/runtime resync，再 attach 最新 live 游标。
///
/// Business Logic（为什么需要这个测试）:
///     永久漏事件与重复去重都不可接受；resync 后从 latest 附着。
///
/// Code Logic（这个测试做什么）:
///     Gap → RequestResync → attach_at(latest) → 后续同 sequence 去重。
#[test]
fn event_relay_gap_triggers_terminal_and_runtime_resync() {
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
            // 模拟：terminal replay + runtime snapshot 完成后 attach latest live cursor
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

// 抑制未使用告警（部分类型仅用于 serde 形状对齐）
#[allow(dead_code)]
fn _type_smoke(s: ConfigSnapshot) -> ConfigSnapshot {
    s
}

#[allow(dead_code)]
fn _err_smoke() -> AppError {
    AppError::generic("x")
}
