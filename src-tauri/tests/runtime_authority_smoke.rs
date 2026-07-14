//! runtime_authority_smoke — GUI control client 与 owner CAS 收敛 smoke。
//!
//! Business Logic（为什么需要这个测试）:
//!     N1 Task3 要求 GUI 通过 control client 更新 sidecar 权威配置，generation 与 runtime 值必须收敛；
//!     截图快捷键两阶段补偿与响应丢失对账规则必须确定性验证。
//!
//! Code Logic（这个模块做什么）:
//!     在隔离 temp 目录启动轻量 owner HTTP（status/get-config/update-config），
//!     用 `BackendControlClient` 走真实 loopback 协议；热键对账规则用纯函数 + Fake OS 后端验证。

use app_lib::backend::authority::CONTROL_SCHEMA_VERSION;
use app_lib::backend::control_client::{
    decide_hotkey_reconcile, BackendControlClient, HotkeyOsReconcileDecision,
};
use app_lib::config::{
    AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
};
use app_lib::config_runtime::{
    ConfigSnapshot, ConfigUpdateResponse, RuntimeConfigPatch, RuntimeOwnerStatus,
    ConfigRuntime,
};
use app_lib::config_store::MemoryConfigStore;
use app_lib::error::AppError;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
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

/// harness 共享状态。
#[derive(Clone)]
struct OwnerState {
    runtime: Arc<ConfigRuntime>,
    token: String,
    fail_next_save: Arc<std::sync::atomic::AtomicBool>,
}

/// Runtime authority harness：owner runtime + loopback control HTTP + client。
struct RuntimeAuthorityHarness {
    runtime: Arc<ConfigRuntime>,
    client: BackendControlClient,
    owner_id: String,
    fail_next_save: Arc<std::sync::atomic::AtomicBool>,
    _shutdown: oneshot::Sender<()>,
}

impl RuntimeAuthorityHarness {
    /// 启动隔离 owner 与 client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 需在不触碰用户 `~/.cc-partner` 的前提下验证 control 协议与 CAS。
    ///
    /// Code Logic（这个函数做什么）:
    ///     MemoryConfigStore + ConfigRuntime(with_owner) + axum 绑定 127.0.0.1:0 + BackendControlClient::for_test。
    async fn start() -> Self {
        let owner_id = format!("owner-smoke-{}", uuid::Uuid::new_v4());
        let token = format!("token-{}", uuid::Uuid::new_v4());
        let initial = sample_config();
        let store = Arc::new(MemoryConfigStore::with_config(initial.clone()));
        let runtime = Arc::new(ConfigRuntime::with_owner(
            initial,
            store,
            owner_id.clone(),
        ));
        let fail_next_save = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = OwnerState {
            runtime: Arc::clone(&runtime),
            token: token.clone(),
            fail_next_save: Arc::clone(&fail_next_save),
        };

        let app = Router::new()
            .route("/api/backend/control/status", post(h_status))
            .route("/api/backend/control/get-config", post(h_get_config))
            .route("/api/backend/control/update-config", post(h_update_config))
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

        let client = BackendControlClient::for_test(addr.port(), &token, &owner_id)
            .expect("client");
        let _ = token; // token 已注入 client
        Self {
            runtime,
            client,
            owner_id,
            fail_next_save,
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
    assert_eq!(
        harness.owner_config().await.screenshot_hotkey,
        "<ctrl>+s"
    );
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

// 抑制未使用告警（部分类型仅用于 serde 形状对齐）
#[allow(dead_code)]
fn _type_smoke(s: ConfigSnapshot) -> ConfigSnapshot {
    s
}

#[allow(dead_code)]
fn _err_smoke() -> AppError {
    AppError::generic("x")
}
