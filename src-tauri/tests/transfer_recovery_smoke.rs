//! transfer_recovery_smoke — N5 retry/resume 幂等 claim、resume capability 与 operation 对账 smoke。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     同一 clientOperationId 并发 resume/retry 只能产生一个 Fresh attempt；payload 冲突必须
//!     拒绝；owner 宣告 `transfer.resume.v1` 后才允许 resume 能力探测。T3 起增加发送端
//!     clientOperationId operation 查询（notFound/pending/succeeded）与 lost-ACK 本地提交合同。
//!     真正 1 GiB dual-host 续传保持 NOT VERIFIED，本文件不宣称 L3。
//!
//! Code Logic（这个文件做什么）:
//!     1) 隔离 SQLite + TransferRepo::ensure_schema
//!     2) 并发 claim 同一 op id/hash → 恰好 1 Fresh + 1 Replay
//!     3) server_protocol_info 宣告 resume capability
//!     4) operation lookup 映射 pending/succeeded/notFound
//!     5) lost final ACK：status 权威 completed → 本地 commit Succeeded，不二次 complete finalize

use app_lib::{
    canonical_recovery_payload_hash, operation_status_from_task, server_protocol_info,
    PeerCallError, PeerClient, SenderClaimOutcome, TransferCompletePolicy, TransferDirection,
    TransferOperationStatus, TransferPhase, TransferRecoveryKind, TransferRepo, TransferStatus,
    TransferTask, CAPABILITY_TRANSFER_RESUME_V1, PROTOCOL_VERSION_V1,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// 隔离临时库。
struct SmokeDb {
    _dir: TempDir,
    pool: SqlitePool,
}

/// Business Logic: smoke 不得污染用户 `~/.cc-partner`。
/// Code Logic: 建 WAL 池 + ensure_schema。
async fn setup_db() -> SmokeDb {
    let dir = TempDir::new().expect("tempdir");
    let db_path: PathBuf = dir.path().join("transfer_recovery_smoke.db");
    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("pool");
    TransferRepo::ensure_schema(&pool)
        .await
        .expect("ensure_schema");
    SmokeDb { _dir: dir, pool }
}

/// Business Logic: claim 需要完整 task 快照。
/// Code Logic: 构造 Send + Pending 行（phase 由 claim 归一 Queued）。
fn queued_send_task(id: &str, op: &str, hash: &str) -> TransferTask {
    let mut task = TransferTask {
        id: id.to_string(),
        filename: "smoke.bin".into(),
        file_path: "/tmp/smoke.bin".into(),
        size: 1024,
        sha256: "abc".into(),
        chunk_size: 960 * 1024,
        direction: TransferDirection::Send,
        peer_device_id: "peer-1".into(),
        status: TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: "2026-07-15T00:00:00Z".into(),
        completed_at: None,
        ..TransferTask::recovery_defaults(id)
    };
    task.client_operation_id = Some(op.into());
    task.operation_payload_hash = Some(hash.into());
    task
}

/// Business Logic: 并发同一 op id 不得双 Fresh。
/// Code Logic: tokio::join 两次 claim；统计 Fresh/Replay。
#[tokio::test]
async fn duplicate_resume_request_creates_one_attempt() {
    let db = setup_db().await;
    let repo = Arc::new(TransferRepo::new(db.pool.clone()));
    let hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Resume,
        "logical-1",
        "/tmp/smoke.bin",
        "peer-1",
        "protocol-1",
    );

    let make = |attempt_id: &str| {
        let mut t = queued_send_task(attempt_id, "op-1", &hash);
        t.logical_transfer_id = "logical-1".into();
        t.protocol_transfer_id = "protocol-1".into();
        t.attempt_id = attempt_id.into();
        t
    };

    let r1 = Arc::clone(&repo);
    let r2 = Arc::clone(&repo);
    let t1 = make("attempt-a");
    let t2 = make("attempt-b");
    let hash1 = hash.clone();
    let hash2 = hash.clone();
    let (a, b) = tokio::join!(
        async move {
            r1.claim_sender_operation("op-1", &hash1, &t1)
                .await
                .unwrap()
        },
        async move {
            r2.claim_sender_operation("op-1", &hash2, &t2)
                .await
                .unwrap()
        },
    );

    let mut fresh = 0u32;
    let mut replay = 0u32;
    let mut ids = Vec::new();
    for outcome in [a, b] {
        match outcome {
            SenderClaimOutcome::Fresh(t) => {
                fresh += 1;
                ids.push(t.id);
            }
            SenderClaimOutcome::Replay(t) => {
                replay += 1;
                ids.push(t.id);
            }
            SenderClaimOutcome::Conflict { .. } => panic!("same payload must not conflict"),
        }
    }
    assert_eq!(fresh, 1, "exactly one Fresh winner");
    assert_eq!(replay, 1, "loser must Replay");
    assert_eq!(ids[0], ids[1], "both outcomes share the claimed attempt id");

    let again = repo
        .claim_sender_operation("op-1", &hash, &make("attempt-c"))
        .await
        .unwrap();
    assert!(matches!(again, SenderClaimOutcome::Replay(_)));

    let by_op = repo
        .get_by_client_operation_id("op-1")
        .await
        .unwrap()
        .expect("row");
    assert_eq!(by_op.client_operation_id.as_deref(), Some("op-1"));
    assert_eq!(by_op.operation_payload_hash.as_deref(), Some(hash.as_str()));
}

/// Business Logic: 同 clientOperationId 的两次 retry（空 protocol 占位 hash）必须 Replay，不 Conflict。
/// Code Logic: 顺序 claim 同一 retry hash → 1 Fresh + 1 Replay，attempt id 相同。
#[tokio::test]
async fn sequential_retry_same_op_id_replays_without_conflict() {
    let db = setup_db().await;
    let repo = TransferRepo::new(db.pool.clone());
    let hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Retry,
        "logical-retry",
        "/tmp/smoke.bin",
        "peer-1",
        "",
    );
    let make = |attempt_id: &str| {
        let mut t = queued_send_task(attempt_id, "op-retry-1", &hash);
        t.logical_transfer_id = "logical-retry".into();
        t.protocol_transfer_id = attempt_id.into();
        t.attempt_id = attempt_id.into();
        t
    };
    let first = repo
        .claim_sender_operation("op-retry-1", &hash, &make("attempt-r1"))
        .await
        .unwrap();
    let second = repo
        .claim_sender_operation("op-retry-1", &hash, &make("attempt-r2"))
        .await
        .unwrap();
    let id1 = match first {
        SenderClaimOutcome::Fresh(t) => t.id,
        other => panic!("first must be Fresh, got {other:?}"),
    };
    let id2 = match second {
        SenderClaimOutcome::Replay(t) => t.id,
        other => panic!("second must be Replay, got {other:?}"),
    };
    assert_eq!(id1, id2);
    assert_eq!(id1, "attempt-r1");
}

/// Business Logic: same id + different payload 必须 conflict。
/// Code Logic: 先 Fresh resume hash，再 claim retry hash → Conflict。
#[tokio::test]
async fn same_id_different_payload_is_operation_conflict() {
    let db = setup_db().await;
    let repo = TransferRepo::new(db.pool.clone());
    let resume_hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Resume,
        "logical-1",
        "/tmp/smoke.bin",
        "peer-1",
        "protocol-1",
    );
    // Retry 生产路径用空 protocol 占位；与 resume hash 仍不同 → Conflict。
    let retry_hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Retry,
        "logical-1",
        "/tmp/smoke.bin",
        "peer-1",
        "",
    );
    let task = queued_send_task("attempt-1", "op-mix", &resume_hash);
    let fresh = repo
        .claim_sender_operation("op-mix", &resume_hash, &task)
        .await
        .unwrap();
    assert!(matches!(fresh, SenderClaimOutcome::Fresh(_)));
    let conflict = repo
        .claim_sender_operation("op-mix", &retry_hash, &task)
        .await
        .unwrap();
    assert!(matches!(conflict, SenderClaimOutcome::Conflict { .. }));
}

/// Business Logic: resume 能力必须与代码同提交宣告。
/// Code Logic: server_protocol_info 含 transfer.resume.v1。
#[test]
fn resume_capability_is_advertised_with_v1() {
    let info = server_protocol_info();
    assert_eq!(info.protocol_version, PROTOCOL_VERSION_V1);
    assert!(
        info.supports(CAPABILITY_TRANSFER_RESUME_V1),
        "owner must advertise {}",
        CAPABILITY_TRANSFER_RESUME_V1
    );
    assert_eq!(CAPABILITY_TRANSFER_RESUME_V1, "transfer.resume.v1");
}

/// Business Logic: get_transfer_operation 必须覆盖 notFound/pending/succeeded 三态。
/// Code Logic: 空 ledger → NotFound；claim Queued → Pending；record Completed → Succeeded{taskId}。
#[tokio::test]
async fn operation_lookup_pending_succeeded_not_found() {
    let db = setup_db().await;
    let repo = TransferRepo::new(db.pool.clone());

    assert!(matches!(
        operation_status_from_task(None),
        TransferOperationStatus::NotFound
    ));

    let hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Retry,
        "logical-op",
        "/tmp/smoke.bin",
        "peer-1",
        "protocol-op",
    );
    let mut task = queued_send_task("attempt-op", "op-lookup-1", &hash);
    task.logical_transfer_id = "logical-op".into();
    task.protocol_transfer_id = "protocol-op".into();
    let fresh = repo
        .claim_sender_operation("op-lookup-1", &hash, &task)
        .await
        .unwrap();
    let claimed = match fresh {
        SenderClaimOutcome::Fresh(t) => t,
        other => panic!("expected Fresh, got {other:?}"),
    };
    let pending = repo
        .get_by_client_operation_id("op-lookup-1")
        .await
        .unwrap();
    assert!(matches!(
        operation_status_from_task(pending.as_ref()),
        TransferOperationStatus::Pending
    ));

    let mut done = claimed;
    done.status = TransferStatus::Completed;
    done.phase = Some(TransferPhase::Completed);
    done.completed_at = Some("2026-07-15T01:00:00Z".into());
    done.transferred_bytes = done.size;
    repo.record(&done).await.unwrap();

    let loaded = repo
        .get_by_client_operation_id("op-lookup-1")
        .await
        .unwrap();
    match operation_status_from_task(loaded.as_ref()) {
        TransferOperationStatus::Succeeded { task_id } => {
            assert_eq!(task_id, "attempt-op");
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }

    assert!(matches!(
        operation_status_from_task(
            repo.get_by_client_operation_id("op-missing")
                .await
                .unwrap()
                .as_ref()
        ),
        TransferOperationStatus::NotFound
    ));
}

/// Business Logic: final ACK 丢失后，receiver status=completed 权威时发送端本地提交
///     Succeeded，且不得再次发起破坏性 complete/finalize。
///
/// Code Logic:
///     1) claim Finalizing pending 行（模拟 complete 超时后的 uncertain ledger）；
///     2) mock peer：complete 恒 504 且计数；status=completed；
///     3) PeerClient status_fallback 收敛 Ok(true)（既有 lost-response 合同）；
///     4) 本地 record Completed（模拟 commit_sender_completed_outcome）；
///     5) operation → Succeeded；complete 计数在“对账查询阶段”不再增加（第二次 status-only 路径）。
#[tokio::test]
async fn lost_final_ack_reconciles_to_completed_without_second_finalize() {
    let db = setup_db().await;
    let repo = TransferRepo::new(db.pool.clone());
    let hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Resume,
        "logical-lost",
        "/tmp/smoke.bin",
        "peer-lost",
        "protocol-lost",
    );
    let mut task = queued_send_task("attempt-lost", "op-lost-ack", &hash);
    task.logical_transfer_id = "logical-lost".into();
    task.protocol_transfer_id = "protocol-lost".into();
    task.peer_device_id = "peer-lost".into();
    repo.claim_sender_operation("op-lost-ack", &hash, &task)
        .await
        .unwrap();

    // 模拟 complete 超时后的 uncertain：Finalizing + Transferring，无 failure。
    let mut uncertain = repo
        .get_by_client_operation_id("op-lost-ack")
        .await
        .unwrap()
        .expect("claimed");
    uncertain.status = TransferStatus::Transferring;
    uncertain.phase = Some(TransferPhase::Finalizing);
    uncertain.transferred_bytes = uncertain.size;
    repo.record(&uncertain).await.unwrap();
    assert!(matches!(
        operation_status_from_task(Some(&uncertain)),
        TransferOperationStatus::Pending
    ));

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
                            "error": "final ack dropped",
                            "code": "timeout",
                            "request_id": "r-lost-ack",
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
                    "transfer_id": "protocol-lost",
                    "status": "completed",
                    "progress": 1.0,
                    "transferred_bytes": 1024,
                    "size": 1024,
                    "filename": "smoke.bin"
                }))
            }),
        );

    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let base_url = format!("http://{addr}");
    let client = PeerClient::new();

    // 发送路径：complete 响应丢失，status=completed → 本地收敛成功（finalize 不二次执行）。
    let policy = TransferCompletePolicy {
        max_attempts: 2,
        base_backoff: Duration::from_millis(5),
        status_fallback: true,
    };
    let ok = client
        .transfer_complete_with_policy(&base_url, "protocol-lost", policy)
        .await
        .expect("status=completed must converge");
    assert!(ok);
    let hits_after_send = complete_hits.load(Ordering::SeqCst);
    assert!(
        hits_after_send >= 1,
        "complete must be attempted at least once before status fallback"
    );

    // 本地单事务提交 completed + operation outcome（发送端 ledger）。
    let mut completed = uncertain.clone();
    completed.status = TransferStatus::Completed;
    completed.phase = Some(TransferPhase::Completed);
    completed.completed_at = Some("2026-07-15T02:00:00Z".into());
    completed.failure = None;
    completed.transferred_bytes = completed.size;
    repo.record(&completed).await.unwrap();

    let loaded = repo
        .get_by_client_operation_id("op-lost-ack")
        .await
        .unwrap()
        .expect("row");
    assert!(matches!(
        operation_status_from_task(Some(&loaded)),
        TransferOperationStatus::Succeeded { .. }
    ));

    // 对账查询阶段只读 status，不得再次 complete（无第二 finalize）。
    let hits_before_query = complete_hits.load(Ordering::SeqCst);
    let status = client
        .transfer_status_typed(&base_url, "protocol-lost")
        .await
        .expect("status");
    assert_eq!(
        status.get("status").and_then(|v| v.as_str()),
        Some("completed")
    );
    assert_eq!(
        complete_hits.load(Ordering::SeqCst),
        hits_before_query,
        "operation query must not re-invoke complete/finalize"
    );
    // finalize_count == 1 合同：complete 尝试有界，且对账后不递增（单次 receiver finalize 语义）。
    assert_eq!(
        complete_hits.load(Ordering::SeqCst),
        hits_after_send,
        "finalize/complete count must stay at the single convergence window"
    );

    // 明确失败路径：无 status_fallback 时 timeout 仍是 Remote{code=timeout}，供 uncertain 分类。
    let classify = TransferCompletePolicy {
        max_attempts: 1,
        base_backoff: Duration::from_millis(1),
        status_fallback: false,
    };
    let err = client
        .transfer_complete_with_policy(&base_url, "protocol-lost", classify)
        .await
        .expect_err("timeout without fallback");
    match err {
        PeerCallError::Remote { code, .. } => assert_eq!(code, "timeout"),
        other => panic!("expected Remote timeout, got {other}"),
    }
}
