//! S6 Quality Faults 集成测试（L2）：批事务 rollback、SQLite busy 有界超时、
//! peer 响应丢失幂等收敛、malformed HTTP DTO fail-closed。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     生产路径上的 batch 写、连接池 busy、transfer complete 响应丢失与对端畸形 DTO
//!     必须在 L2 用可复现故障注入验证：整批 rollback、有界等待、幂等收敛、
//!     InvalidResponse fail-closed（不得当业务成功）。故障 seam 仅 test-only 参数
//!     （inject_fail_at / 短 backoff policy / mock peer），禁止生产环境变量打开故障。
//!
//! L2 coverage map（稳定 ID 见 `docs/development/quality-matrix.json`）:
//!     - `L2-FAULT-BATCH-001` / `L2-FAULT-BUSY-001` / `L2-FAULT-PEER-001`：本文件 T4 用例
//!     - `L2-FAULT-DTO-001`：本文件 malformed transfer status/complete 响应
//!     - `L2-LAN-BOUNDARY-001`：**不在本文件重复**；权威自动化矩阵见
//!       `tests/lan_trust_boundary_smoke.rs` + `lan_trust_boundary_harness`
//!       （无凭据 loopback/mobile 读写、Host/Origin、stop loopback+token、
//!       injected public/XFF peer；真实多机 mDNS/公网 NIC = L3 NOT VERIFIED）
//!     - Transfer send Tauri command / Settings 部分失败：前端 L1 harness；
//!       本文件不造第二套 GUI/AppState 假件。
//!
//! Code Logic（这个文件做什么）:
//!     1) fail_row_n_in_batch_rolls_back_all：ClaudeHistoryRepo inject_fail_at 中途失败 → COUNT=0
//!     2) hold_write_lock_past_busy_timeout_is_bounded：短 busy_timeout 池 + BEGIN IMMEDIATE 持锁
//!        → 并发写在有界时间内 locked/busy，释放后写成功
//!     3) peer_response_lost_after_commit_converges_idempotently：axum mock complete 恒 timeout
//!        信封 + status=completed → transfer_complete_with_policy 收敛 Ok(true)，并校验稳定 code
//!     4) malformed_transfer_status_dto_is_invalid_response：status 返回非 DTO shape → InvalidResponse
//!     5) malformed_transfer_complete_body_is_invalid_response：complete 非 JSON body → InvalidResponse

use app_lib::{
    ClaudeHistoryRepo, ClaudeHistoryRow, PeerCallError, PeerClient, TransferCompletePolicy,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::net::SocketAddr;
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
