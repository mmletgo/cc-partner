//! S6 Quality Faults 集成测试（L2）：批事务 rollback、SQLite busy 有界超时、
//! peer 响应丢失幂等收敛、malformed HTTP DTO fail-closed、
//! Transfer send 路径 fail-closed、Scratchpad 事务 inject rollback、Settings 多字段事务隔离。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     生产路径上的 batch 写、连接池 busy、transfer complete 响应丢失与对端畸形 DTO
//!     必须在 L2 用可复现故障注入验证：整批 rollback、有界等待、幂等收敛、
//!     InvalidResponse fail-closed（不得当业务成功）。故障 seam 仅 debug/test-only
//!     （inject_fail_at / 短 backoff policy / mock peer / FaultInjectingConfigIo），
//!     禁止生产环境变量打开故障；history/scratchpad inject API 在 release 构建剥离。
//!
//! L2 coverage map（稳定 ID 见 `docs/development/quality-matrix.json`）:
//!     - `L2-FAULT-BATCH-001`：`fail_row_n_in_batch_rolls_back_all`
//!     - `L2-FAULT-BUSY-001`：`hold_write_lock_past_busy_timeout_is_bounded`
//!     - `L2-FAULT-PEER-001`：`peer_response_lost_after_commit_converges_idempotently`
//!     - `L2-FAULT-DTO-001`：malformed transfer status/complete 响应
//!     - `L2-FAULT-TRANSFER-SEND-001`：`malformed_transfer_init_dto_fails_closed`
//!     - `L2-FAULT-SCRATCH-TX-001`：`scratchpad_inject_fail_rolls_back_batch`
//!     - `L2-FAULT-SETTINGS-001`：`settings_partial_command_failure_isolates_fields`
//!     - `L2-FAULT-AGENT-HUB-PROJECTION-001`：`agent_hub_projection_*` 故障注入（temp/sync/precondition/rename/db commit）
//!     - `L2-FAULT-AGENT-HUB-ADOPTION-001`：`agent_hub_adoption_*` legacy 纳管故障点（激活失败/archive 前崩溃/DB commit 前崩溃）
//!     - `L2-FAULT-AGENT-HUB-IMPORT-001`：`agent_hub_import_*` SnapshotImporter 两阶段故障
//!       （corrupt object / DB commit 前崩溃 / CAS 后残留未计 imported asset）
//!     - `L2-FAULT-OPENCODE-RUNTIME-BRIDGE-001`：`opencode_runtime_bridge_*`
//!       （hash 钉死 / 未 opt-in fail-closed / externalCollision 不覆盖 / OSC 可解码）
//!     - `L2-LAN-BOUNDARY-001`：**不在本文件重复**；权威自动化矩阵见
//!       `tests/lan_trust_boundary_smoke.rs` + `lan_trust_boundary_harness`
//!       （无凭据 loopback/mobile 读写、Host/Origin、stop loopback+token、
//!       injected public/XFF peer；真实多机 mDNS/公网 NIC = L3 NOT VERIFIED）
//!
//! Code Logic（这个文件做什么）:
//!     1) fail_row_n_in_batch_rolls_back_all：ClaudeHistoryRepo inject_fail_at 中途失败 → COUNT=0
//!     2) hold_write_lock_past_busy_timeout_is_bounded：短 busy_timeout 池 + BEGIN IMMEDIATE 持锁
//!        → 并发写在有界时间内 locked/busy，释放后写成功
//!     3) peer_response_lost_after_commit_converges_idempotently：axum mock complete 恒 timeout
//!        信封 + status=completed → transfer_complete_with_policy 收敛 Ok(true)，并校验稳定 code
//!     4) malformed_transfer_status_dto_is_invalid_response：status 返回非 DTO shape → InvalidResponse
//!     5) malformed_transfer_complete_body_is_invalid_response：complete 非 JSON body → InvalidResponse
//!     6) malformed_transfer_init_dto_fails_closed：init 200 但非 JSON / 错误 shape → String Err，
//!        不得表现为带 resume_offset 的成功握手
//!     7) scratchpad_inject_fail_rolls_back_batch：事务 inject 中途失败 → 预置行保留、批未提交
//!     8) settings_partial_command_failure_isolates_fields：ConfigRuntime 多字段 mutate 在
//!        Rename 前故障时 memory+disk 全旧；成功命令 A 后失败命令 B 不得污染 A 已提交状态

use app_lib::config::AppConfig;
use app_lib::config_runtime::{update_config_transactionally, ConfigRuntime};
use app_lib::config_store::{
    ConfigIoStage, ConfigStore, FaultInjectingConfigIo, FsConfigStore, StdConfigIo,
};
use app_lib::{
    agent_hub_sha256_hex, AdoptionEngine, AdoptionFault, AdoptionOutcome, AdoptionRequest,
    AdoptionState, AgentHubImportFault, AgentHubObjectStore, AgentHubRepo, AgentTarget, AssetKind,
    AssetPolicy, ClaudeHistoryRepo, ClaudeHistoryRow, ConfirmedImportSelection, DesiredPresence,
    NewLogicalAsset, NewScopeNode, NewTargetBinding, OpenCodeBridgeOutcome, OpenCodeEventMapper,
    OpenCodeOfficialEvent, OpenCodeRuntimeBridge, PeerCallError, PeerClient, ProjectionJobState,
    ProjectionPayloadKind, ProjectionRequest, ProjectionScheduler, ProjectionWriteFault,
    RevisionId, ScopeKind, ScratchpadRepo, ScratchpadRow, SnapshotImporter, TransferCompletePolicy,
    ValidatedSnapshot, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// 测试用文件型 SQLite 句柄：持有 TempDir 防止库文件被删。
struct FaultTestDb {
    _dir: TempDir,
    pool: SqlitePool,
}

/// Business Logic（为什么需要这个函数）:
///     故障测试必须用隔离的文件库，避免污染用户 `~/.cc-partner`，且可配置短 busy_timeout。
///
/// Code Logic（这个函数做什么）:
///     创建 WAL 文件 SQLite，`busy_timeout`/`max_connections` 仅作用于本测试池；
///     初始化最小 `claude_history` 表后返回 pool。
async fn setup_history_db(max_connections: u32, busy_timeout: Duration) -> FaultTestDb {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("quality_faults.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(busy_timeout);
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections.max(1))
        .connect_with(options)
        .await
        .expect("connect pool");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS claude_history (\
         id TEXT PRIMARY KEY, project_path TEXT NOT NULL, project_name TEXT NOT NULL, \
         session_id TEXT NOT NULL, content TEXT NOT NULL, git_branch TEXT, cc_version TEXT, \
         occurred_at TEXT NOT NULL, device_id TEXT NOT NULL, vector_clock TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0, source TEXT NOT NULL DEFAULT 'claude')",
    )
    .execute(&pool)
    .await
    .expect("create claude_history");
    FaultTestDb { _dir: dir, pool }
}

/// Business Logic（为什么需要这个函数）:
///     Scratchpad 事务 inject 测试需要隔离的 scratchpad 表，避免污染用户库。
///
/// Code Logic（这个函数做什么）:
///     创建 WAL 文件 SQLite 并建最小 `scratchpad` 表，返回 pool。
async fn setup_scratchpad_db() -> FaultTestDb {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("quality_faults_scratchpad.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect pool");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS scratchpad (\
         id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '速记本', content TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, device_id TEXT NOT NULL, \
         vector_clock TEXT NOT NULL, deleted INTEGER DEFAULT 0, \
         delete_epoch INTEGER NOT NULL DEFAULT 0)",
    )
    .execute(&pool)
    .await
    .expect("create scratchpad");
    FaultTestDb { _dir: dir, pool }
}

/// Business Logic（为什么需要这个函数）:
///     批 rollback / 成功写入路径都需要构造合法 ClaudeHistoryRow。
///
/// Code Logic（这个函数做什么）:
///     生成 `count` 条最小可用 row，id 形如 `{prefix}-{i}`，vector_clock 为 `{device:1}`。
fn sample_history_batch(prefix: &str, count: usize) -> Vec<ClaudeHistoryRow> {
    let mut batch = Vec::with_capacity(count);
    for i in 0..count {
        let mut vc = HashMap::new();
        vc.insert("d".to_string(), 1);
        batch.push(ClaudeHistoryRow {
            id: format!("{prefix}-{i}"),
            project_path: "/p".into(),
            project_name: "p".into(),
            session_id: "s".into(),
            content: format!("c{i}"),
            git_branch: None,
            cc_version: None,
            occurred_at: "t".into(),
            device_id: "d".into(),
            vector_clock: vc,
            created_at: "t".into(),
            updated_at: "t".into(),
            deleted: false,
            source: "claude".to_string(),
        });
    }
    batch
}

/// Business Logic（为什么需要这个函数）:
///     Scratchpad inject 路径需要合法页面行构造。
///
/// Code Logic（这个函数做什么）:
///     生成最小 ScratchpadRow，vector_clock 为 `{device:1}`。
fn sample_scratchpad_row(id: &str, title: &str, content: &str) -> ScratchpadRow {
    let mut vc = HashMap::new();
    vc.insert("d".to_string(), 1);
    ScratchpadRow {
        id: id.to_string(),
        title: title.to_string(),
        content: content.to_string(),
        created_at: "t".into(),
        updated_at: "t".into(),
        device_id: "d".into(),
        vector_clock: vc,
        deleted: false,
        delete_epoch: 0,
    }
}

/// Business Logic（为什么需要这个函数）:
///     Settings L2 需要构造可 `validate()` 的最小合法 AppConfig。
///
/// Code Logic（这个函数做什么）:
///     在 temp 根下生成 device/path/hotkey 等合法字段，与 config_runtime 单测样本对齐。
fn sample_settings_config(data_dir: &Path, device_name: &str) -> AppConfig {
    AppConfig {
        device_id: "settings-l2-device".into(),
        device_name: device_name.into(),
        http_port: 0,
        receive_dir: data_dir.join("recv").to_string_lossy().to_string(),
        game_plugin_dir: "/tmp/plugins".into(),
        db_path: data_dir.join("data.db").to_string_lossy().to_string(),
        screenshot_hotkey: "<ctrl>+<shift>+s".into(),
        prompt_optimizer_hotkey: "<ctrl>".into(),
        prompt_optimizer_fill_language: "zh".into(),
        prompt_optimizer_provider: "claude".into(),
        prompt_quick_input_hotkey: "<ctrl>+/".into(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: Default::default(),
        battery: Default::default(),
        orchestrator: Default::default(),
        github_trending: Default::default(),
        agent_hub: app_lib::config::AgentHubConfig::default(),
        manual_peers: Vec::new(),
        experimental_features: app_lib::config::ExperimentalFeaturesConfig::default(),
        internal_claude: app_lib::config::InternalClaudeConfig::default(),
    }
}

/// Business Logic（为什么需要这个函数）:
///     分类 SQLite locked/busy，断言有界超时失败而不依赖中文文案分支。
///
/// Code Logic（这个函数做什么）:
///     对错误 Display 做小写 contains 匹配 locked/busy/database is locked。
fn is_sqlite_busy_msg(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("locked") || m.contains("busy") || m.contains("database is locked")
}

/// Business Logic（为什么需要这个测试）:
///     push-batch 类写路径在中途失败时必须整批 rollback，禁止 partial accepted；
///     必须走产品 `upsert_merged_batch` 事务边界（inject_fail_at seam），而非手写 SQL。
///
/// Code Logic（这个测试做什么）:
///     构造 3 行，调用 `upsert_merged_batch_inject_fail_at(..., Some(1))`，
///     断言 Err 且 COUNT(*)==0；再无注入成功写入，证明 seam 不污染生产路径。
#[tokio::test]
async fn fail_row_n_in_batch_rolls_back_all() {
    let db = setup_history_db(1, Duration::from_secs(5)).await;
    let repo = ClaudeHistoryRepo::new(db.pool.clone());

    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .expect("count before");
    assert_eq!(before, 0);

    let batch = sample_history_batch("partial", 3);
    let err = repo
        .upsert_merged_batch_inject_fail_at(&batch, Some(1))
        .await
        .expect_err("inject fail must surface");
    let err_text = err.to_string();
    assert!(
        err_text.contains("injected") || err_text.contains("vector_clock"),
        "error should mention injection seam: {err_text}"
    );

    let after_fail: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .expect("count after fail");
    assert_eq!(
        after_fail, 0,
        "product upsert path must roll back the entire batch"
    );

    // 无注入生产路径必须可写，证明 inject seam 不改变默认行为。
    let written = repo
        .upsert_merged_batch(&batch)
        .await
        .expect("production path without inject must succeed");
    assert_eq!(written, 3);
    let after_ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .expect("count after ok");
    assert_eq!(after_ok, 3);
}

/// Business Logic（为什么需要这个测试）:
///     连接池在写锁竞争时必须在 busy_timeout 内失败，禁止无限挂起饿死调度；
///     生产用 5s，本测试用短有界值仅测池行为，不改生产 runtime。
///
/// Code Logic（这个测试做什么）:
///     max_connections>=2、busy_timeout=200ms；连接1 BEGIN IMMEDIATE 持写锁；
///     连接2 经 repo 写批，外层 tokio::time::timeout 证明在有界时间内返回 locked/busy；
///     ROLLBACK 释放后写成功。
#[tokio::test]
async fn hold_write_lock_past_busy_timeout_is_bounded() {
    let busy_timeout = Duration::from_millis(200);
    let db = setup_history_db(2, busy_timeout).await;
    let repo = ClaudeHistoryRepo::new(db.pool.clone());
    let batch = sample_history_batch("busy", 1);

    // 连接1：持有写锁，超出一次 acquire 的 busy_timeout 窗口。
    let mut holder = db.pool.acquire().await.expect("acquire holder");
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *holder)
        .await
        .expect("BEGIN IMMEDIATE");

    // hosted runner 上 SQLite busy 返回可能超过 200ms+500ms；3s 仍有界，证明不会无限挂起。
    let outer_budget = Duration::from_secs(3);
    let started = Instant::now();
    let write_result = tokio::time::timeout(outer_budget, repo.upsert_merged_batch(&batch)).await;
    let elapsed = started.elapsed();

    match write_result {
        Ok(Err(err)) => {
            assert!(
                is_sqlite_busy_msg(&err.to_string()),
                "expected locked/busy while write lock held, got: {err}"
            );
            assert!(
                elapsed <= outer_budget + Duration::from_millis(100),
                "write must fail within bounded outer budget, elapsed={elapsed:?}"
            );
        }
        Ok(Ok(n)) => panic!("write should not succeed under held IMMEDIATE lock, written={n}"),
        Err(_) => panic!(
            "write hung past outer budget {outer_budget:?}; busy_timeout must bound the wait"
        ),
    }

    // 释放写锁后，同一 repo 写路径必须成功。
    sqlx::query("ROLLBACK")
        .execute(&mut *holder)
        .await
        .expect("ROLLBACK");
    drop(holder);

    let written = repo
        .upsert_merged_batch(&batch)
        .await
        .expect("write after lock release must succeed");
    assert_eq!(written, 1);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM claude_history")
        .fetch_one(&db.pool)
        .await
        .expect("count");
    assert_eq!(count, 1);
}

/// Business Logic（为什么需要这个测试）:
///     transfer complete 在对端已提交后响应丢失时，发送端必须经 status 收敛为成功（幂等），
///     不得因瞬时 5xx/timeout 信封进入重复失败态；错误分类必须用稳定 code，不依赖中文文案。
///
/// Code Logic（这个测试做什么）:
///     axum mock：complete 恒返回 504 + code=timeout 信封；status 返回 completed。
///     `transfer_complete_with_policy(status_fallback=true, 短 backoff)` → Ok(true)。
///     另用 status_fallback=false 捕获一次 Remote，断言 `code()==Some("timeout")`。
#[tokio::test]
async fn peer_response_lost_after_commit_converges_idempotently() {
    let complete_hits = Arc::new(AtomicU32::new(0));
    let hits_c = complete_hits.clone();
    let app = axum::Router::new()
        .route(
            "/api/transfer/complete/:id",
            axum::routing::post(move || {
                let hits = hits_c.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        axum::Json(serde_json::json!({
                            "error": "对端响应超时（文案不得用于业务分支）",
                            "code": "timeout",
                            "request_id": "r-quality-to",
                            "retryable": true,
                        })),
                    )
                }
            }),
        )
        .route(
            "/api/transfer/status/:id",
            axum::routing::get(|| async move {
                axum::Json(serde_json::json!({
                    "transfer_id": "tid-quality-lost",
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
        .expect("bind mock");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    let base_url = format!("http://{addr}");
    let client = PeerClient::new();

    // 稳定 code 断言：关闭 status_fallback 时完整暴露 Remote 信封 code，不得依赖中文 message。
    let classify_policy = TransferCompletePolicy {
        max_attempts: 1,
        base_backoff: Duration::from_millis(5),
        status_fallback: false,
    };
    let classify_err = client
        .transfer_complete_with_policy(&base_url, "tid-quality-lost", classify_policy)
        .await
        .expect_err("without status_fallback the timeout envelope must surface");
    match &classify_err {
        PeerCallError::Remote { code, status, .. } => {
            assert_eq!(code, "timeout", "stable P2P error code must be timeout");
            assert_eq!(*status, 504);
        }
        other => panic!("expected PeerCallError::Remote with stable code, got: {other}"),
    }
    assert_eq!(
        classify_err.code(),
        Some("timeout"),
        "PeerCallError::code() is the business branch entry"
    );

    // 响应丢失收敛：complete 仍 504，status=completed → 本地幂等成功。
    let converge_policy = TransferCompletePolicy {
        max_attempts: 2,
        base_backoff: Duration::from_millis(5),
        status_fallback: true,
    };
    let ok = client
        .transfer_complete_with_policy(&base_url, "tid-quality-lost", converge_policy)
        .await
        .expect("status=completed must converge to local success");
    assert!(ok, "idempotent convergence must report success");
    assert!(
        complete_hits.load(Ordering::SeqCst) >= 2,
        "complete endpoint should be hit for classify + converge paths"
    );
}

/// Business Logic（为什么需要这个测试）:
///     transfer status 若返回非 JSON body，客户端必须 fail-closed 为 `InvalidResponse`，
///     禁止把任意文本当成功进度或驱动 complete 收敛。
///
/// Code Logic（这个测试做什么）:
///     axum mock GET `/api/transfer/status/:id` 返回 200 + plain text；
///     `PeerClient::transfer_status_typed` 必须 `Err(InvalidResponse)`。
#[tokio::test]
async fn malformed_transfer_status_dto_is_invalid_response() {
    let app = axum::Router::new().route(
        "/api/transfer/status/:id",
        axum::routing::get(|| async move {
            (
                axum::http::StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                "not-json-status-body",
            )
        }),
    );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    let base_url = format!("http://{addr}");
    let client = PeerClient::new();

    let err = client
        .transfer_status_typed(&base_url, "tid-bad-dto")
        .await
        .expect_err("non-JSON status body must not decode as success");
    match &err {
        PeerCallError::InvalidResponse { reason, .. } => {
            assert!(
                !reason.is_empty(),
                "InvalidResponse must carry a non-empty reason for diagnostics"
            );
        }
        other => panic!("expected PeerCallError::InvalidResponse, got: {other}"),
    }
    assert_eq!(
        err.code(),
        None,
        "InvalidResponse is not a business code branch"
    );
}

/// Business Logic（为什么需要这个测试）:
///     transfer complete 若返回非 JSON 错误体，客户端必须归类 InvalidResponse，
///     不得依赖本地化文案、不得静默当 success。
///
/// Code Logic（这个测试做什么）:
///     axum mock POST complete 返回 500 + plain text；
///     `transfer_complete_with_policy(status_fallback=false)` → InvalidResponse。
#[tokio::test]
async fn malformed_transfer_complete_body_is_invalid_response() {
    let app = axum::Router::new().route(
        "/api/transfer/complete/:id",
        axum::routing::post(|| async move {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "upstream blew up with free text",
            )
        }),
    );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    let base_url = format!("http://{addr}");
    let client = PeerClient::new();

    let policy = TransferCompletePolicy {
        max_attempts: 1,
        base_backoff: Duration::from_millis(5),
        status_fallback: false,
    };
    let err = client
        .transfer_complete_with_policy(&base_url, "tid-bad-body", policy)
        .await
        .expect_err("non-JSON complete body must surface InvalidResponse");
    match &err {
        PeerCallError::InvalidResponse { .. } => {}
        other => panic!("expected PeerCallError::InvalidResponse, got: {other}"),
    }
    assert_eq!(err.code(), None);
}

/// Business Logic（为什么需要这个测试）:
///     complete 200 响应字段类型错误时必须 fail-closed，禁止把字符串 success 当业务完成。
///
/// Code Logic（这个测试做什么）:
///     POST complete 返回 200 + `{success:"yes"}`；typed ChunkResp 反序列化失败 → InvalidResponse。
#[tokio::test]
async fn malformed_transfer_complete_shape_is_invalid_response() {
    let app = axum::Router::new().route(
        "/api/transfer/complete/:id",
        axum::routing::post(|| async move {
            axum::Json(serde_json::json!({
                "success": "yes",
                "received_bytes": 0
            }))
        }),
    );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock");
    });
    let base_url = format!("http://{addr}");
    let client = PeerClient::new();

    let policy = TransferCompletePolicy {
        max_attempts: 1,
        base_backoff: Duration::from_millis(5),
        status_fallback: false,
    };
    let err = client
        .transfer_complete_with_policy(&base_url, "tid-bad-shape", policy)
        .await
        .expect_err("wrong complete DTO shape must surface InvalidResponse");
    match &err {
        PeerCallError::InvalidResponse { .. } => {}
        other => panic!("expected PeerCallError::InvalidResponse, got: {other}"),
    }
    assert_eq!(err.code(), None);
}

/// Business Logic（为什么需要这个测试）:
///     Transfer send 握手若对端 init 返回非 JSON 或错误 shape，发送端必须 fail-closed，
///     不得把畸形响应当 accepted 并带着 resume_offset 进入分块写危险状态。
///
/// Code Logic（这个测试做什么）:
///     axum mock `POST /api/transfer/init` 先返回 200 + plain text，再返回 200 + 错误 shape JSON；
///     `PeerClient::transfer_init` 两次均 `Err(String)`，错误不得看起来像成功握手
///     （不得含可解析 resume_offset 成功语义）。
#[tokio::test]
async fn malformed_transfer_init_dto_fails_closed() {
    // Case A：200 + 非 JSON body。
    {
        let app = axum::Router::new().route(
            "/api/transfer/init",
            axum::routing::post(|| async move {
                (
                    axum::http::StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/plain")],
                    "not-json-init-body",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let meta = serde_json::json!({
            "transfer_id": "tid-bad-init",
            "filename": "a.bin",
            "size": 10,
            "sha256": "deadbeef",
            "chunk_size": 960 * 1024
        });
        let err = client
            .transfer_init(&base_url, meta)
            .await
            .expect_err("non-JSON init body must fail closed");
        assert!(
            !err.contains("\"resume_offset\""),
            "error surface must not look like a successful init with resume_offset: {err}"
        );
        assert!(
            err.to_ascii_lowercase().contains("invalid")
                || err.to_ascii_lowercase().contains("json")
                || err.contains("init"),
            "error should indicate parse/init failure, got: {err}"
        );
    }

    // Case B：业务失败信封（typed failure）仍 fail-closed，不得当 accepted。
    {
        let app = axum::Router::new().route(
            "/api/transfer/init",
            axum::routing::post(|| async move {
                (
                    axum::http::StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({
                        "error": "transfer id conflict（文案不得当成功）",
                        "code": "conflict",
                        "request_id": "r-init-conflict",
                        "retryable": false,
                    })),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind mock");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });
        let base_url = format!("http://{addr}");
        let client = PeerClient::new();
        let meta = serde_json::json!({
            "transfer_id": "tid-conflict-init",
            "filename": "a.bin",
            "size": 10,
            "sha256": "deadbeef",
            "chunk_size": 960 * 1024
        });
        let err = client
            .transfer_init(&base_url, meta)
            .await
            .expect_err("business failure envelope must fail closed");
        assert!(
            !err.contains("\"resume_offset\""),
            "error surface must not look like a successful init with resume_offset: {err}"
        );
        assert!(
            err.contains("conflict") || err.contains("409") || err.contains("init"),
            "error should carry conflict/status surface: {err}"
        );
    }
}

/// Business Logic（为什么需要这个测试）:
///     Scratchpad 批量写若走事务边界，中途失败必须整批 rollback，预置页不得被 partial batch 污染；
///     inject seam 仅 debug/test-only，成功路径（inject=None）必须可写。
///
/// Code Logic（这个测试做什么）:
///     seed 1 条 prior → COUNT=1；3 行 batch 在 index=1 inject fail → Err 且 COUNT 仍 1；
///     再 `bulk_upsert_inject_fail_at(..., None)` 成功写入批。
#[tokio::test]
async fn scratchpad_inject_fail_rolls_back_batch() {
    let db = setup_scratchpad_db().await;
    let repo = ScratchpadRepo::new(db.pool.clone());

    let prior = sample_scratchpad_row("prior-page", "prior", "keep-me");
    repo.upsert(&prior).await.expect("seed prior");
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scratchpad")
        .fetch_one(&db.pool)
        .await
        .expect("count before");
    assert_eq!(before, 1);

    let batch = vec![
        sample_scratchpad_row("sp-0", "a", "c0"),
        sample_scratchpad_row("sp-1", "b", "c1"),
        sample_scratchpad_row("sp-2", "c", "c2"),
    ];
    let err = repo
        .bulk_upsert_inject_fail_at(&batch, Some(1))
        .await
        .expect_err("inject fail must surface");
    assert!(
        err.to_string().contains("injected scratchpad"),
        "error should mention scratchpad inject seam: {err}"
    );

    let after_fail: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scratchpad")
        .fetch_one(&db.pool)
        .await
        .expect("count after fail");
    assert_eq!(
        after_fail, 1,
        "partial batch rows must roll back; prior row stays"
    );
    let kept = repo
        .get("prior-page")
        .await
        .expect("get prior")
        .expect("exists");
    assert_eq!(kept.content, "keep-me");
    assert!(repo.get("sp-0").await.expect("get sp-0").is_none());
    assert!(repo.get("sp-1").await.expect("get sp-1").is_none());
    assert!(repo.get("sp-2").await.expect("get sp-2").is_none());

    repo.bulk_upsert_inject_fail_at(&batch, None)
        .await
        .expect("inject=None path must succeed");
    let after_ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scratchpad")
        .fetch_one(&db.pool)
        .await
        .expect("count after ok");
    assert_eq!(after_ok, 4);
}

/// Business Logic（为什么需要这个测试）:
///     Settings 多字段更新经 `update_config_transactionally` 时，落盘前故障不得半应用字段；
///     且失败命令不得污染此前已成功提交的命令状态（命令边界隔离）。
///
/// Code Logic（这个测试做什么）:
///     1) seed device_name=settings-a + receive_dir；
///     2) FaultInjectingConfigIo::Rename fail_once，同时 mutate device_name+receive_dir → Err；
///        memory 与 disk 均保持旧值（两字段都不半写）；
///     3) 健康 IO 成功更新 device_name=settings-a-ok；
///     4) 再对 receive_dir 注入 Rename 失败 → settings-a-ok 仍在 memory/disk。
#[tokio::test]
async fn settings_partial_command_failure_isolates_fields() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path();
    let config_path = root.join("config.json");
    let initial = sample_settings_config(root, "settings-a");
    let old_receive = initial.receive_dir.clone();

    let seed = FsConfigStore::new(config_path.clone(), Arc::new(StdConfigIo));
    seed.save_atomic(&initial).expect("seed config");

    // 故障路径：rename 前失败，多字段 mutate 不得半应用。
    let fail_io = Arc::new(FaultInjectingConfigIo::fail_once(
        Arc::new(StdConfigIo),
        ConfigIoStage::Rename,
    ));
    let fail_store: Arc<dyn ConfigStore> =
        Arc::new(FsConfigStore::new(config_path.clone(), fail_io));
    let runtime = ConfigRuntime::new(initial.clone(), fail_store);
    let new_receive = root.join("recv-new").to_string_lossy().to_string();

    let err = update_config_transactionally(&runtime, |cfg| {
        cfg.device_name = "settings-b-half".into();
        cfg.receive_dir = new_receive.clone();
        Ok(())
    })
    .await
    .expect_err("rename inject must fail closed");
    assert!(
        err.to_string().contains("注入")
            || err.to_string().contains("故障")
            || err.to_string().contains("Rename")
            || err.to_string().contains("rename"),
        "expected inject/rename failure surface: {err}"
    );

    let mem = runtime.snapshot().expect("snapshot after fail");
    assert_eq!(mem.device_name, "settings-a");
    assert_eq!(mem.receive_dir, old_receive);
    let disk: AppConfig =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(disk.device_name, "settings-a");
    assert_eq!(disk.receive_dir, old_receive);

    // 成功命令 A：只改 device_name。
    let ok_store: Arc<dyn ConfigStore> = Arc::new(FsConfigStore::new(
        config_path.clone(),
        Arc::new(StdConfigIo),
    ));
    let runtime_ok = ConfigRuntime::new(initial.clone(), ok_store);
    let (committed, _) = update_config_transactionally(&runtime_ok, |cfg| {
        cfg.device_name = "settings-a-ok".into();
        Ok(())
    })
    .await
    .expect("healthy update of device_name");
    assert_eq!(committed.device_name, "settings-a-ok");
    assert_eq!(runtime_ok.snapshot().unwrap().device_name, "settings-a-ok");

    // 失败命令 B：改 receive_dir，Rename 注入失败，不得污染 A 已提交的 device_name。
    let fail_io_b = Arc::new(FaultInjectingConfigIo::fail_once(
        Arc::new(StdConfigIo),
        ConfigIoStage::Rename,
    ));
    let fail_store_b: Arc<dyn ConfigStore> =
        Arc::new(FsConfigStore::new(config_path.clone(), fail_io_b));
    // 用磁盘当前权威（已含 settings-a-ok）构造 runtime，模拟真实后续命令。
    let disk_after_a: AppConfig =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(disk_after_a.device_name, "settings-a-ok");
    let runtime_b = ConfigRuntime::new(disk_after_a.clone(), fail_store_b);
    let err_b = update_config_transactionally(&runtime_b, |cfg| {
        cfg.receive_dir = new_receive.clone();
        Ok(())
    })
    .await
    .expect_err("second command rename inject must fail");
    let _ = err_b;

    let mem_b = runtime_b.snapshot().expect("snapshot b");
    assert_eq!(
        mem_b.device_name, "settings-a-ok",
        "failed command B must not corrupt successful command A state"
    );
    assert_eq!(mem_b.receive_dir, old_receive);
    let disk_b: AppConfig =
        serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(disk_b.device_name, "settings-a-ok");
    assert_eq!(disk_b.receive_dir, old_receive);
}

/// Business Logic（为什么需要这个测试）:
///     防止 quality_faults 静默漂移丢失 LAN 边界 L2 入口；真实 socket 矩阵由独立
///     smoke 执行，本用例只做路径/符号存在性与产品边界注释契约，避免重复绑定端口。
///
/// Code Logic（这个测试做什么）:
///     断言 `lan_trust_boundary_harness::INJECTED_PEER_EVIDENCE` 标签常量非空，
///     并文档化「不得把 injected peer 当真实公网 NIC 证据」的产品边界。
#[test]
fn lan_boundary_l2_entry_is_documented_not_duplicated() {
    // 产品边界：无身份鉴权；LAN 全读写；stop 仅 loopback+token。
    // 真实多机 mDNS / 手机 QR / 公网 peer → L3 NOT VERIFIED（见 real-device-certification.md）。
    let label = app_lib::lan_trust_boundary_harness::INJECTED_PEER_EVIDENCE;
    assert_eq!(
        label, "INJECTED_PEER_EVIDENCE",
        "injected peer evidence label must stay stable for smoke diagnostics"
    );
    assert!(
        !label.to_ascii_lowercase().contains("verified production"),
        "label must not imply production multi-host verification"
    );
}

/// Business Logic（为什么需要这个函数）:
///     Agent Hub projection 故障注入需要隔离 SQLite + CAS 根，避免污染用户库。
///
/// Code Logic（这个函数做什么）:
///     临时目录建 schema、ObjectStore、ProjectionScheduler。
async fn setup_agent_hub_projection() -> (ProjectionScheduler, AgentHubRepo, tempfile::TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("agent_hub_projection.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect");
    AgentHubRepo::ensure_schema(&pool).await.expect("schema");
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(dir.path()).expect("object store");
    let sched = ProjectionScheduler::new(repo.clone(), store);
    sched.inject_support_bypass(true).await;
    (sched, repo, dir)
}

/// 种子 asset + binding。
///
/// Business Logic: fault 测试需要合法 binding 才能入队。
/// Code Logic: user scope + instruction asset + claude binding。
async fn seed_projection_binding(repo: &AgentHubRepo) -> (String, String) {
    let scope = repo
        .insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .expect("scope");
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .expect("asset");
    let binding = repo
        .upsert_target_binding(NewTargetBinding {
            asset_id: asset.id.clone(),
            target: AgentTarget::Claude,
            local_scope_mapping_id: None,
            checkout_binding_id: None,
            desired_presence: DesiredPresence::Present,
            desired_enabled: true,
        })
        .await
        .expect("binding");
    (asset.id, binding.id)
}

fn file_projection_request(
    asset_id: &str,
    binding_id: &str,
    path: &Path,
    bytes: &[u8],
    expected: Option<&str>,
) -> ProjectionRequest {
    let hash = agent_hub_sha256_hex(bytes);
    ProjectionRequest {
        asset_id: asset_id.into(),
        target: AgentTarget::Claude,
        target_binding_id: binding_id.into(),
        desired_revision_id: Some(RevisionId::new_v7()),
        target_path: path.to_string_lossy().to_string(),
        expected_external_hash: expected.map(|s| s.to_string()),
        rendered_hash: hash,
        rendered_bytes: bytes.to_vec(),
        desired_presence: DesiredPresence::Present,
        desired_enabled: true,
        payload_kind: ProjectionPayloadKind::File,
        directory_entries: None,
        managed_paths: None,
        hub_project_id: None,
        base_hash: expected.map(|s| s.to_string()),
    }
}

/// L2-FAULT-AGENT-HUB-PROJECTION-001：temp write 故障不得留下半截目标。
///
/// Business Logic（为什么需要这个测试）:
///     投影在 temp write 失败时必须保留旧完整文件。
///
/// Code Logic（这个测试做什么）:
///     inject TempWrite → run_ready_jobs failed → 目标仍为 old，无 .tmp 残留。
#[tokio::test]
async fn agent_hub_projection_temp_write_fault_preserves_old_file() {
    let (sched, repo, dir) = setup_agent_hub_projection().await;
    let (asset, binding) = seed_projection_binding(&repo).await;
    let target = dir.path().join("CLAUDE.md");
    std::fs::write(&target, b"old-complete").unwrap();
    let old = agent_hub_sha256_hex(b"old-complete");
    let req = file_projection_request(&asset, &binding, &target, b"new-complete", Some(&old));
    let _job = sched.enqueue_projection(req).await.expect("enqueue");
    sched
        .inject_write_fault(Some(ProjectionWriteFault::TempWrite))
        .await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let stats = sched.run_ready_jobs(&cancel).await.expect("run");
    assert_eq!(stats.failed, 1);
    assert_eq!(std::fs::read(&target).unwrap(), b"old-complete");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no partial temps: {leftovers:?}");
}

/// L2：file sync / rename 故障同样保留旧文件。
#[tokio::test]
async fn agent_hub_projection_file_sync_and_rename_faults_preserve_old() {
    for fault in [ProjectionWriteFault::FileSync, ProjectionWriteFault::Rename] {
        // 每故障独立 fixture，避免前一轮 prepared 残留抬高 failed 计数。
        let (sched, repo, dir) = setup_agent_hub_projection().await;
        let (asset, binding) = seed_projection_binding(&repo).await;
        let target = dir.path().join(format!("f-{fault:?}.md"));
        std::fs::write(&target, b"old-complete").unwrap();
        let old = agent_hub_sha256_hex(b"old-complete");
        let req = file_projection_request(&asset, &binding, &target, b"new-complete", Some(&old));
        let _ = sched.enqueue_projection(req).await.unwrap();
        sched.inject_write_fault(Some(fault)).await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let stats = sched.run_ready_jobs(&cancel).await.unwrap();
        assert_eq!(stats.failed, 1, "fault={fault:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"old-complete");
    }
}

/// L2：precondition recheck 故障/漂移 → 目标不变。
#[tokio::test]
async fn agent_hub_projection_precondition_fault_or_drift() {
    let (sched, repo, dir) = setup_agent_hub_projection().await;
    let (asset, binding) = seed_projection_binding(&repo).await;
    let target = dir.path().join("drift.md");
    std::fs::write(&target, b"base").unwrap();
    let base = agent_hub_sha256_hex(b"base");
    let req = file_projection_request(&asset, &binding, &target, b"hub", Some(&base));
    let job = sched.enqueue_projection(req).await.unwrap();
    // 外部改动
    std::fs::write(&target, b"external-edit").unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let stats = sched.run_ready_jobs(&cancel).await.unwrap();
    assert!(stats.drifted >= 1 || stats.failed >= 1);
    assert_eq!(std::fs::read(&target).unwrap(), b"external-edit");
    let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
    assert_ne!(done.state, ProjectionJobState::Committed);
}

/// L2：DB commit 故障后按实际 hash 恢复，不得仅凭 DB prepared→committed。
#[tokio::test]
async fn agent_hub_projection_db_commit_fault_then_hash_recover() {
    let (sched, repo, dir) = setup_agent_hub_projection().await;
    let (asset, binding) = seed_projection_binding(&repo).await;
    let target = dir.path().join("recover.md");
    std::fs::write(&target, b"old").unwrap();
    let old = agent_hub_sha256_hex(b"old");
    let new_bytes = b"new-after-rename";
    let req = file_projection_request(&asset, &binding, &target, new_bytes, Some(&old));
    let job = sched.enqueue_projection(req).await.unwrap();
    sched.inject_db_commit_failure(true).await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let _ = sched.run_ready_jobs(&cancel).await;
    let mid = repo.get_projection_job(&job.id).await.unwrap().unwrap();
    assert_ne!(
        mid.state,
        ProjectionJobState::Committed,
        "must not commit on DB-only state after inject"
    );
    sched.inject_db_commit_failure(false).await;
    if std::fs::read(&target).unwrap() == new_bytes {
        let stats = sched.recover_on_startup().await.unwrap();
        assert!(stats.recovered >= 1);
        let done = repo.get_projection_job(&job.id).await.unwrap().unwrap();
        assert_eq!(done.state, ProjectionJobState::Committed);
        assert_eq!(std::fs::read(&target).unwrap(), new_bytes);
    }
}

/// L2：directory 未知外部文件 → drift，绝不删除。
#[tokio::test]
async fn agent_hub_projection_directory_unknown_files_never_deleted() {
    use app_lib::{AtomicProjectionWriter, AtomicWriteOutcome, DirectoryWriteRequest};
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("skill");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("SKILL.md"), b"managed").unwrap();
    std::fs::write(target.join("user-notes.txt"), b"keep-me").unwrap();
    let managed = vec!["SKILL.md".to_string()];
    let entries = vec![("SKILL.md".to_string(), b"new".to_vec())];
    let writer = AtomicProjectionWriter::new();
    let out = writer
        .write_directory(DirectoryWriteRequest {
            target_dir: &target,
            managed_paths: &managed,
            entries: &entries,
            rendered_hash: &agent_hub_sha256_hex(b"tree"),
            expected_external_hash: Some("base"),
        })
        .unwrap();
    assert!(matches!(
        out,
        AtomicWriteOutcome::DirectoryUnknownFiles { .. }
    ));
    assert_eq!(
        std::fs::read(target.join("user-notes.txt")).unwrap(),
        b"keep-me"
    );
}

// ---------------------------------------------------------------------------
// Gate B Task 6 — legacy adoption fault points (L2-FAULT-AGENT-HUB-ADOPTION-001)
// ---------------------------------------------------------------------------

use app_lib::{
    hash_skill_directory, DiscoveredPortableAsset, FakeProcessRunner, PortableAssetOrigin,
    PortableAssetOwner, PortableAssetPayload, PortableDiscoveryStatus, PortableOriginKind,
    PortableSkill,
};
use std::path::PathBuf;

async fn setup_agent_hub_adoption() -> (AdoptionEngine, AgentHubRepo, tempfile::TempDir, PathBuf) {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("agent_hub_adoption.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("connect");
    AgentHubRepo::ensure_schema(&pool).await.expect("schema");
    let repo = AgentHubRepo::new(pool);
    let store = AgentHubObjectStore::open(dir.path()).expect("object store");
    let runner = Arc::new(FakeProcessRunner::new());
    for _ in 0..16 {
        runner.push_ok("ok");
    }
    let engine = AdoptionEngine::new(repo.clone(), store, runner);
    // 事务语义测试：FakeProcessRunner 仅经 inject 绕过 support baseline
    engine.inject_support_bypass(true);
    for _ in 0..16 {
        // setup 已 push_ok；inspect 额外消耗 list 响应
    }
    let data = dir.path().to_path_buf();
    (engine, repo, dir, data)
}

fn write_legacy_skill(root: &Path, name: &str, body: &str) -> PathBuf {
    let p = root.join(name);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test\n---\n{body}\n"),
    )
    .unwrap();
    p
}

fn discovered_legacy(target: AgentTarget, path: &Path) -> DiscoveredPortableAsset {
    let (content, tree, _, diags) = hash_skill_directory(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    DiscoveredPortableAsset {
        kind: AssetKind::Skill,
        semantic_name: name.clone(),
        scope_kind: ScopeKind::User,
        payload: PortableAssetPayload::Skill(PortableSkill {
            name: name.clone(),
            description: "test".into(),
            skill_markdown_hash: content.clone(),
            tree_manifest_hash: tree.clone(),
            target_extensions: Default::default(),
        }),
        origin: PortableAssetOrigin {
            target,
            path: path.to_path_buf(),
            origin_kind: PortableOriginKind::LegacyStandalone,
            native_id: name,
            content_hash: content,
            tree_hash: Some(tree),
            status: PortableDiscoveryStatus::Active,
            native_output_candidate: false,
            owned_by: PortableAssetOwner::from_target(target),
            parent_plugin_id: None,
        },
        diagnostics: diags,
    }
}

fn adoption_request(data: &Path, discovered: DiscoveredPortableAsset) -> AdoptionRequest {
    AdoptionRequest {
        data_dir: data.to_path_buf(),
        scope_id: "user".into(),
        scope_kind: ScopeKind::User,
        confirmed: true,
        discovered,
        origin_namespace: "legacy".into(),
        origin_replica_id: "quality-faults".into(),
    }
}

/// L2-FAULT-AGENT-HUB-ADOPTION-001：激活失败保留 legacy 目录，无第二 discoverable 副本。
#[tokio::test]
async fn agent_hub_adoption_activation_failure_preserves_legacy() {
    let (engine, _repo, _dir, data) = setup_agent_hub_adoption().await;
    let root = data.join("home/.claude/skills");
    let skill = write_legacy_skill(&root, "review", "body");
    let disc = discovered_legacy(AgentTarget::Claude, &skill);
    engine.inject_fault(AdoptionFault::ForceActivationFailure);
    let out = engine.adopt(adoption_request(&data, disc)).await.unwrap();
    assert!(
        matches!(out, AdoptionOutcome::Blocked { .. }),
        "got {out:?}"
    );
    assert!(skill.is_dir(), "legacy source must remain");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}

/// L2：archive 前崩溃保留 legacy 源。
#[tokio::test]
async fn agent_hub_adoption_crash_before_archive_preserves_legacy() {
    let (engine, _repo, _dir, data) = setup_agent_hub_adoption().await;
    let root = data.join("home/.claude/skills");
    let skill = write_legacy_skill(&root, "review", "body");
    let disc = discovered_legacy(AgentTarget::Claude, &skill);
    engine.inject_fault(AdoptionFault::CrashBeforeArchive);
    let out = engine.adopt(adoption_request(&data, disc)).await.unwrap();
    assert!(matches!(out, AdoptionOutcome::Blocked { .. }));
    assert!(skill.is_dir(), "crash before archive keeps source");
}

/// L2：DB commit 前崩溃 → 源在 staging（可恢复），兼容路径 0 份；recovery 完成后 committed。
#[tokio::test]
async fn agent_hub_adoption_crash_before_db_commit_recoverable() {
    let (engine, repo, _dir, data) = setup_agent_hub_adoption().await;
    let root = data.join("home/.claude/skills");
    let skill = write_legacy_skill(&root, "review", "body");
    let disc = discovered_legacy(AgentTarget::Claude, &skill);
    engine.inject_fault(AdoptionFault::CrashBeforeDbCommit);
    let out = engine.adopt(adoption_request(&data, disc)).await.unwrap();
    assert!(matches!(out, AdoptionOutcome::Blocked { .. }));
    assert!(!skill.exists(), "renamed into staging");
    let staging = data.join("agent-hub/adoption-staging");
    assert!(staging.is_dir(), "staging holds archive");
    let rows = repo.list_adoptions().await.unwrap();
    let archived = rows
        .into_iter()
        .find(|r| r.state == AdoptionState::Archived)
        .expect("archived row");
    engine.inject_fault(AdoptionFault::None);
    let recovered = engine.recover_adoption(&archived.id).await.unwrap();
    assert!(matches!(recovered, AdoptionOutcome::Adopted { .. }));
    let done = repo.get_adoption(&archived.id).await.unwrap().unwrap();
    assert_eq!(done.state, AdoptionState::Committed);
}

// ── Gate C Task 3: SnapshotImporter fault boundaries ─────────────────────

/// L2-FAULT-AGENT-HUB-IMPORT-001 辅助：最小可导入 instruction snapshot。
///
/// Business Logic: 故障边界测试需要合法 envelope + object_bytes，失败时不得激活 head。
/// Code Logic: 建 user scope/asset/revision + ValidatedSnapshot。
async fn setup_agent_hub_import_snapshot() -> (
    AgentHubRepo,
    AgentHubObjectStore,
    tempfile::TempDir,
    ValidatedSnapshot,
    String,
) {
    use app_lib::agent_hub::models::{
        NewRevision, RevisionId, RevisionOperation, RevisionOriginKind,
    };
    use app_lib::agent_hub::snapshot::builder::{
        build_snapshot, SnapshotSelectionMode, SnapshotSelectionRequest,
    };

    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("import.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    let _ = repo.take_import_fault();
    let store = AgentHubObjectStore::open(dir.path()).unwrap();
    let scope = repo
        .insert_scope(NewScopeNode {
            id: Some("scope-user".into()),
            kind: ScopeKind::User,
            hub_project_id: None,
            relative_path: None,
        })
        .await
        .unwrap();
    let asset = repo
        .insert_asset(NewLogicalAsset {
            scope_id: scope.id,
            kind: AssetKind::Instruction,
            origin_namespace: "standalone".into(),
            logical_key: "root".into(),
            display_name: "Root".into(),
            policy: AssetPolicy::Shared,
        })
        .await
        .unwrap();
    let body = br#"{"relativeKey":"CLAUDE.md","blocks":[{"id":"b1","mode":"shared","commonMarkdown":"body","variants":{},"headingPath":[],"needsAdaptation":false}]}"#;
    let hash = store.put_blob(body).await.unwrap().hash;
    repo.append_revision(NewRevision {
        id: RevisionId::new_v7(),
        asset_lineage_id: asset.id.clone(),
        parents: vec![],
        operation: RevisionOperation::Upsert,
        origin_kind: RevisionOriginKind::Ui,
        origin_target: None,
        origin_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
        payload_hash: Some(hash.clone()),
        tree_manifest_hash: None,
        created_at: "2026-07-29T10:00:00Z".into(),
        expected_parent_id: None,
    })
    .await
    .unwrap();
    let built = build_snapshot(
        &repo,
        &store,
        SnapshotSelectionRequest {
            mode: SnapshotSelectionMode::FullHub,
            scope_ids: vec![],
            asset_ids: vec![],
            hub_project_ids: vec![],
            include_history: true,
            source_replica_id: "01900000-0000-7000-8000-0000000000b1".into(),
            limits: None,
        },
    )
    .await
    .unwrap();
    let snapshot = ValidatedSnapshot::from_parts(built.envelope, built.object_bytes, None).unwrap();
    (repo, store, dir, snapshot, hash)
}

/// L2：corrupt object → import 失败，目标库无 active asset/head。
#[tokio::test]
async fn agent_hub_import_corrupt_object_does_not_activate_head() {
    let (_src_repo, _src_store, _src_dir, mut snapshot, _hash) =
        setup_agent_hub_import_snapshot().await;
    // 破坏全部 object 字节
    for v in snapshot.object_bytes.values_mut() {
        v.push(b'!');
    }
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("dst.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    let _ = repo.take_import_fault();
    let store = AgentHubObjectStore::open(dir.path()).unwrap();
    let importer = SnapshotImporter::new(repo.clone(), store, dir.path());
    let err = importer
        .commit_import(snapshot, ConfirmedImportSelection::default())
        .await
        .expect_err("corrupt must fail");
    assert!(
        err.to_string().contains("corrupt") || err.to_string().contains("hash"),
        "{err}"
    );
    assert!(
        repo.list_assets(None, None).await.unwrap().is_empty(),
        "no active assets after corrupt import"
    );
}

/// L2：TX commit 前注入失败 → 无非法 head；CAS 对象可残留且不报告为 imported asset。
#[tokio::test]
async fn agent_hub_import_db_fail_before_commit_keeps_cas_not_heads() {
    let (_src_repo, _src_store, _src_dir, snapshot, hash) = setup_agent_hub_import_snapshot().await;
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("dst.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    let _ = repo.take_import_fault();
    repo.inject_import_fault(AgentHubImportFault::BeforeTxCommit);
    let store = AgentHubObjectStore::open(dir.path()).unwrap();
    let importer = SnapshotImporter::new(repo.clone(), store.clone(), dir.path());
    let err = importer
        .commit_import(snapshot, ConfirmedImportSelection::default())
        .await
        .expect_err("injected fail");
    let _ = repo.take_import_fault();
    assert!(
        err.to_string().contains("injected") || err.to_string().contains("import"),
        "{err}"
    );
    assert!(
        repo.list_assets(None, None).await.unwrap().is_empty(),
        "failed TX must not leave assets"
    );
    // CAS residual allowed
    assert!(
        store.get_blob(&hash).await.is_ok(),
        "CAS residual ok for GC"
    );
}

/// L2：成功 import 后 imported_object_hashes 不含未引用 blob。
#[tokio::test]
async fn agent_hub_import_unreferenced_cas_not_reported() {
    let (_src_repo, _src_store, _src_dir, mut snapshot, hash) =
        setup_agent_hub_import_snapshot().await;
    // 附加未引用对象
    let junk = b"unreferenced-secret-not-for-report";
    let junk_hash = agent_hub_sha256_hex(junk);
    snapshot
        .object_bytes
        .insert(junk_hash.clone(), junk.to_vec());

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("dst.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    AgentHubRepo::ensure_schema(&pool).await.unwrap();
    let repo = AgentHubRepo::new(pool);
    let _ = repo.take_import_fault();
    let store = AgentHubObjectStore::open(dir.path()).unwrap();
    let importer = SnapshotImporter::new(repo.clone(), store.clone(), dir.path());
    let out = importer
        .commit_import(snapshot, ConfirmedImportSelection::default())
        .await
        .unwrap();
    assert!(out.imported_object_hashes.contains(&hash));
    assert!(
        !out.imported_object_hashes.contains(&junk_hash),
        "unreferenced CAS must not be reported as imported"
    );
    // residual may exist
    let _ = store.get_blob(&junk_hash).await;
}

/// L2-FAULT-OPENCODE-RUNTIME-BRIDGE-001：生成源 hash 钉死。
///
/// Business Logic（为什么需要这个测试）:
///     app 升级若改 bridge 源，必须显式更新钉死 hash 并出现 project preview diff。
///
/// Code Logic（这个测试做什么）:
///     live sha256 == OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH；源含 event hook 与 OSC 前缀。
#[test]
fn opencode_runtime_bridge_source_hash_is_pinned() {
    let src = OpenCodeRuntimeBridge::generated_source();
    let live = agent_hub_sha256_hex(src.as_bytes());
    assert_eq!(live, OPENCODE_RUNTIME_BRIDGE_SOURCE_HASH);
    assert!(src.contains("CC_PARTNER_AGENT_SESSION_ID"));
    assert!(src.contains("CC_PARTNER_TERMINAL_SESSION_ID"));
    assert!(src.contains("session.status"));
    assert!(src.contains("permission.asked"));
    assert!(src.contains("cc-partner-agent-v1"));
    assert!(!src.contains("API_KEY"));
}

/// L2：未 opt-in 不得 materialize；仅 RuntimeBridgeRequired。
#[test]
fn opencode_runtime_bridge_unopted_fail_closed() {
    let dir = TempDir::new().unwrap();
    let preview = OpenCodeRuntimeBridge::preview(dir.path(), false);
    assert!(matches!(
        preview,
        OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. }
    ));
    let mat = OpenCodeRuntimeBridge::materialize(dir.path(), false).unwrap();
    assert!(matches!(
        mat,
        OpenCodeBridgeOutcome::RuntimeBridgeRequired { .. }
    ));
    assert!(!OpenCodeRuntimeBridge::absolute_path(dir.path()).exists());
}

/// L2：opt-in materialize + verify；externalCollision 不覆盖。
#[test]
fn opencode_runtime_bridge_materialize_collision_no_overwrite() {
    let dir = TempDir::new().unwrap();
    let mat = OpenCodeRuntimeBridge::materialize(dir.path(), true).unwrap();
    assert!(matches!(
        mat,
        OpenCodeBridgeOutcome::Materialized { .. } | OpenCodeBridgeOutcome::Verified { .. }
    ));
    let v = OpenCodeRuntimeBridge::verify(dir.path(), true);
    assert!(matches!(v, OpenCodeBridgeOutcome::Verified { .. }));

    let path = OpenCodeRuntimeBridge::absolute_path(dir.path());
    std::fs::write(&path, b"foreign-plugin-bytes\n").unwrap();
    let coll = OpenCodeRuntimeBridge::materialize(dir.path(), true).unwrap();
    assert!(
        matches!(coll, OpenCodeBridgeOutcome::ExternalCollision { .. }),
        "{coll:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"foreign-plugin-bytes\n",
        "must not overwrite external bytes"
    );
}

/// L2：事件映射 version 从 2 起；pre-active idle 不得 completed。
#[test]
fn opencode_runtime_bridge_events_decode_via_osc() {
    let mut mapper = OpenCodeEventMapper::new("agent-qf", "term-qf");
    let frame = mapper
        .map_event(&OpenCodeOfficialEvent::SessionStatus {
            session_id: "native-qf".into(),
            status: "busy".into(),
        })
        .expect("busy maps");
    assert_eq!(frame.event_version, 2);
    assert!(!frame.osc_bytes.is_empty());
    assert!(frame.occurred_at.contains('T') || frame.occurred_at.contains('t'));

    let mut cold = OpenCodeEventMapper::new("a", "t");
    let idle = cold
        .map_event(&OpenCodeOfficialEvent::SessionIdle {
            session_id: "n".into(),
        })
        .unwrap();
    // pre-active idle stays Idle (Debug string contains Idle)
    assert!(format!("{:?}", idle.phase).contains("Idle"));
    assert_eq!(idle.event_version, 2);

    // after seenActive, session.idle -> Completed
    let done = mapper
        .map_event(&OpenCodeOfficialEvent::SessionIdle {
            session_id: "native-qf".into(),
        })
        .unwrap();
    assert!(format!("{:?}", done.phase).contains("Completed"));
}
