//! transfer_recovery_smoke — N5 retry/resume 幂等 claim 与 resume capability 黑盒 smoke。
//!
//! Business Logic（为什么需要这个测试文件）:
//!     同一 clientOperationId 并发 resume/retry 只能产生一个 Fresh attempt；payload 冲突必须
//!     拒绝；owner 宣告 `transfer.resume.v1` 后才允许 resume 能力探测。真正 1 GiB dual-host
//!     续传保持 NOT VERIFIED，本文件不宣称 L3。
//!
//! Code Logic（这个文件做什么）:
//!     1) 隔离 SQLite + TransferRepo::ensure_schema
//!     2) 并发 claim 同一 op id/hash → 恰好 1 Fresh + 1 Replay
//!     3) server_protocol_info 宣告 resume capability

use app_lib::{
    canonical_recovery_payload_hash, server_protocol_info, SenderClaimOutcome, TransferDirection,
    TransferRecoveryKind, TransferRepo, TransferStatus, TransferTask, CAPABILITY_TRANSFER_RESUME_V1,
    PROTOCOL_VERSION_V1,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;
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
        async move { r1.claim_sender_operation("op-1", &hash1, &t1).await.unwrap() },
        async move { r2.claim_sender_operation("op-1", &hash2, &t2).await.unwrap() },
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
    let retry_hash = canonical_recovery_payload_hash(
        TransferRecoveryKind::Retry,
        "logical-1",
        "/tmp/smoke.bin",
        "peer-1",
        "protocol-2",
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
