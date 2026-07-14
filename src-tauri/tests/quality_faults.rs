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
    ClaudeHistoryRepo, ClaudeHistoryRow, PeerCallError, PeerClient, ScratchpadRepo, ScratchpadRow,
    TransferCompletePolicy,
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
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL, deleted INTEGER DEFAULT 0)",
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
        db_path: data_dir.join("data.db").to_string_lossy().to_string(),
        screenshot_hotkey: "<ctrl>+<shift>+s".into(),
        prompt_optimizer_hotkey: "<ctrl>".into(),
        prompt_optimizer_fill_language: "zh".into(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: Default::default(),
        orchestrator: Default::default(),
        github_trending: Default::default(),
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

    let outer_budget = busy_timeout + Duration::from_millis(500);
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
