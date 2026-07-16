//! receiver 单元/集成 characterization 测试
//!
//! Business Logic: 锁住 validation/chunk/resume/finalize 边界行为，防止拆分回归。
//! Code Logic: 与原 receiver.rs 内联 tests 等价，经 `mod tests` 挂到 receiver。

use super::*;
use crate::storage::TransferRepo;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Once;

/// 全局递增计数器，为每个测试生成唯一的临时子目录名，避免并发/串行测试互相干扰。
static SEQ: AtomicU64 = AtomicU64::new(0);
static INIT: Once = Once::new();

/// 创建一个唯一的临时目录（在系统 temp 下），返回其路径与清理句柄。
///
/// Business Logic: 测试需要隔离的目录来验证文件名冲突逻辑，且不依赖 tempfile crate。
fn unique_temp_dir() -> PathBuf {
    INIT.call_once(|| {
        // 确保 base temp 目录存在
        let _ = fs::create_dir_all(std::env::temp_dir().join("cp_transfer_tests"));
    });
    let n = SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir()
        .join("cp_transfer_tests")
        .join(format!("t{}", n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// 文件名冲突解析：无冲突时原样返回。
#[test]
fn test_resolve_filename_no_conflict() {
    let dir = unique_temp_dir();
    let got = resolve_filename(&dir, "file.txt");
    assert_eq!(got, "file.txt");
    let _ = fs::remove_dir_all(&dir);
}

/// 文件名冲突解析：存在同名文件时加 (1)。
#[test]
fn test_resolve_filename_conflict_1() {
    let dir = unique_temp_dir();
    fs::write(dir.join("file.txt"), b"x").unwrap();
    let got = resolve_filename(&dir, "file.txt");
    assert_eq!(got, "file (1).txt");
    let _ = fs::remove_dir_all(&dir);
}

/// 文件名冲突解析：连冲突时递增 (2)。
#[test]
fn test_resolve_filename_conflict_2() {
    let dir = unique_temp_dir();
    fs::write(dir.join("file.txt"), b"x").unwrap();
    fs::write(dir.join("file (1).txt"), b"x").unwrap();
    let got = resolve_filename(&dir, "file.txt");
    assert_eq!(got, "file (2).txt");
    let _ = fs::remove_dir_all(&dir);
}

/// 无扩展名文件的冲突解析。
#[test]
fn test_resolve_filename_no_ext() {
    let dir = unique_temp_dir();
    fs::write(dir.join("README"), b"x").unwrap();
    let got = resolve_filename(&dir, "README");
    assert_eq!(got, "README (1)");
    let _ = fs::remove_dir_all(&dir);
}

/// 构造 transfer 测试用最小 AppState（隔离 receive_dir + 内存 SQLite）。
///
/// Business Logic（为什么需要这个函数）:
///     并发 finalize 回归必须走真实 handle_chunk/finalize 路径，需要可写 receive_dir 与 transfer_repo。
///
/// Code Logic（这个函数做什么）:
///     创建唯一临时 receive_dir、内存 transfer_history 表与完整 AppState 字段；
///     workbench_dependency::new 会同步探测 tmux（最多约 3s），仅测试可接受。
async fn build_transfer_test_state(receive_dir: &Path) -> AppState {
    use crate::backend::ui::HeadlessBackendUi;
    use crate::config::{
        AppConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
        WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo,
        WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    let options = SqliteConnectOptions::from_str("sqlite::memory:")
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
    )
    .execute(&pool)
    .await
    .unwrap();
    // N5 recovery 列：幂等升级（与生产 ensure_schema 一致）。
    TransferRepo::ensure_schema(&pool).await.unwrap();

    let config = AppConfig {
        device_id: "device-test".to_string(),
        device_name: "test-device".to_string(),
        http_port: 0,
        receive_dir: receive_dir.to_string_lossy().to_string(),
        db_path: receive_dir.join("data.db").to_string_lossy().to_string(),
        screenshot_hotkey: "<cmd>+s".to_string(),
        prompt_optimizer_hotkey: "<ctrl>".to_string(),
        prompt_optimizer_fill_language: "zh".to_string(),
        cloud_sync_repo_url: None,
        cloud_sync_enabled: false,
        cloud_sync_auto: false,
        cloud_sync_interval_secs: 600,
        cloud_sync_branch: None,
        health: HealthConfig::default(),
        orchestrator: OrchestratorAutomationConfig::default(),
        github_trending: GithubTrendingConfig::default(),
    };
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
        device_id: Arc::new("device-test".to_string()),
        devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
        actual_http_port: Arc::new(AtomicU16::new(0)),
        discovery: Arc::new(Mutex::new(None)),
        peer_client: Arc::new(PeerClient::new()),
        transfers: Arc::new(TransferRegistry::new()),
        ui: Arc::new(HeadlessBackendUi::new(receive_dir.join("dist"))),
        update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
        cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
        workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
        workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
        workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
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
        cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
        cloud_sync_cancel: Arc::new(Mutex::new(None)),
        health: Arc::new(crate::health::HealthRuntime::new()),
        health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
        health_cancel: Arc::new(Mutex::new(None)),
        orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
        orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
        orchestrator_cancel: Arc::new(Mutex::new(None)),
        orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
        workbench_claude_session_indexes: Arc::new(RwLock::new(std::collections::HashMap::new())),
        workbench_claude_session_watchers: Arc::new(Mutex::new(std::collections::HashMap::new())),
        workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
            std::collections::HashMap::new(),
        )),
        workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(
            std::collections::HashMap::new(),
        )),
        runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
        runtime_role: crate::backend::authority::RuntimeRole::HeadlessOwner,
        event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
            "transfer-test-owner",
        )),
    }
}

/// Business Logic（为什么需要这个测试）:
///     并发末块若在 finalize 锁外 open/write，迟到请求可改写已校验落地文件；必须保证最终内容与哈希一致。
///
/// Code Logic（这个测试做什么）:
///     1) 并发发送两份相同正确末块，断言均 success 且最终文件字节正确；
///     2) 再发错误数据重放，仍 success（墓碑）且最终文件不被改写。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_final_chunks_cannot_corrupt_verified_file() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;

    let good_bytes = b"A".to_vec();
    let bad_bytes = b"B".to_vec();
    let sha = format!("{:x}", Sha256::digest(&good_bytes));
    let transfer_id = "concurrent-final-chunk".to_string();
    let tmp_path = receive_dir.join(format!(".{transfer_id}.tmp"));

    state.transfers.add(TransferTask {
        id: transfer_id.clone(),
        filename: "payload.bin".to_string(),
        file_path: tmp_path.to_string_lossy().to_string(),
        size: 1,
        sha256: sha,
        chunk_size: 1,
        direction: TransferDirection::Receive,
        peer_device_id: String::new(),
        status: TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        ..TransferTask::recovery_defaults(&transfer_id)
    });

    let state_a = state.clone();
    let state_b = state.clone();
    let id_a = transfer_id.clone();
    let id_b = transfer_id.clone();
    let good1 = good_bytes.clone();
    let good2 = good_bytes.clone();

    let (r1, r2) = tokio::join!(
        async move { handle_chunk(&state_a, &id_a, 0, good1).await },
        async move { handle_chunk(&state_b, &id_b, 0, good2).await },
    );

    let c1 = r1.expect("chunk A");
    let c2 = r2.expect("chunk B");
    assert!(c1.success && c2.success, "并发正确末块均应成功");

    let final_path = receive_dir.join("payload.bin");
    let content = fs::read(&final_path).expect("最终文件应存在");
    assert_eq!(
        content, good_bytes,
        "并发 finalize 后文件必须与校验哈希一致"
    );

    // 迟到的不同 payload 必须在 open/write 前命中墓碑，不得污染最终文件。
    let late = handle_chunk(&state, &transfer_id, 0, bad_bytes)
        .await
        .expect("late chunk");
    assert!(late.success, "迟到请求应命中成功墓碑");
    let content_after = fs::read(&final_path).expect("最终文件应仍存在");
    assert_eq!(
        content_after, good_bytes,
        "迟到错误数据不得改写已校验落地文件"
    );
    assert!(
        !tmp_path.exists(),
        "成功 finalize 后临时文件应已被 rename 移除"
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     同一 transfer_id 重复 init 必须幂等，不能覆盖活跃 entry 的元数据或进度。
///
/// Code Logic（这个测试做什么）:
///     首次 init 后写一部分进度，再以相同元数据 init，断言 resume_offset 反映现有进度且任务仍唯一。
#[tokio::test]
async fn handle_init_is_idempotent_for_same_metadata() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"hello-world";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "init-idempotent".to_string();

    let first = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "hello.txt".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 4,
        },
    )
    .await
    .expect("first init");
    assert!(first.accepted);
    assert_eq!(first.resume_offset, 0);

    // 写入部分数据模拟进行中传输。
    let partial = handle_chunk(&state, &transfer_id, 0, payload[..5].to_vec())
        .await
        .expect("partial chunk");
    assert!(partial.success);
    assert_eq!(partial.received_bytes, 5);

    let second = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "hello.txt".to_string(),
            size: payload.len() as u64,
            sha256: sha,
            chunk_size: 4,
        },
    )
    .await
    .expect("second init");
    assert!(second.accepted);
    assert!(
        second.resume_offset >= 5,
        "幂等 init 应返回至少已写入字节数，实际 {}",
        second.resume_offset
    );
    assert_eq!(state.transfers.list().len(), 1);

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     同 id 不同元数据的 init 必须 conflict，禁止覆盖活跃传输。
#[tokio::test]
async fn handle_init_rejects_metadata_conflict_on_active_task() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let sha = format!("{:x}", Sha256::digest(b"A"));
    let transfer_id = "init-conflict".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "a.bin".to_string(),
            size: 1,
            sha256: sha.clone(),
            chunk_size: 1,
        },
    )
    .await
    .expect("first init");

    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id),
            filename: "b.bin".to_string(),
            size: 1,
            sha256: sha,
            chunk_size: 1,
        },
    )
    .await
    .expect_err("different metadata must conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     finalize 完成后重放 init 不得重建 active task 重新打开写路径。
///
/// Code Logic（这个测试做什么）:
///     完整传输完成后再次 init 同 id，断言 Conflict 且 registry 无 active 任务。
#[tokio::test]
async fn handle_init_rejects_reopen_after_finalize_tombstone() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"Z";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "init-after-finalize".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "z.bin".to_string(),
            size: 1,
            sha256: sha.clone(),
            chunk_size: 1,
        },
    )
    .await
    .expect("init");
    let chunk = handle_chunk(&state, &transfer_id, 0, payload.to_vec())
        .await
        .expect("chunk");
    assert!(chunk.success);
    assert!(
        state.transfers.get(&transfer_id).is_none(),
        "finalize 后应移除 active"
    );
    assert!(
        state.transfers.tombstone(&transfer_id).is_some(),
        "应有终态墓碑"
    );

    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "z.bin".to_string(),
            size: 1,
            sha256: sha,
            chunk_size: 1,
        },
    )
    .await
    .expect_err("post-finalize init must conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
    assert!(
        state.transfers.get(&transfer_id).is_none(),
        "重放 init 不得重建 active 任务"
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     协议约定单块上限 CHUNK_SIZE（960 KiB）；超限 chunk 必须在 open/write 临时文件前拒绝，
///     否则恶意对端可用超大 body 浪费磁盘与 IO。
///
/// Code Logic（这个测试做什么）:
///     先 init 接收任务，再提交 CHUNK_SIZE+1 的 chunk，断言 Validation 错误且 tmp 未被创建/改写。
#[tokio::test]
async fn handle_chunk_rejects_oversized_payload_before_disk_mutation() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let transfer_id = "chunk-too-large".to_string();
    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "big.bin".to_string(),
            size: (CHUNK_SIZE as u64) * 2,
            sha256: "deadbeef".to_string(),
            chunk_size: CHUNK_SIZE as u64,
        },
    )
    .await
    .expect("init oversized-chunk fixture");

    let tmp_path = receive_dir.join(format!(".{transfer_id}.tmp"));
    assert!(
        !tmp_path.exists(),
        "init 后尚未写入任何 chunk，tmp 不应存在"
    );

    let oversized = vec![0u8; CHUNK_SIZE + 1];
    let err = handle_chunk(&state, &transfer_id, 0, oversized)
        .await
        .expect_err("CHUNK_SIZE+1 must be rejected before disk mutation");
    assert!(
        matches!(err, AppError::Validation(_)),
        "超限 chunk 应返回 Validation: {err:?}"
    );
    assert!(
        err.to_string().contains("上限") || err.to_string().contains(&CHUNK_SIZE.to_string()),
        "错误消息应提及上限: {err}"
    );
    assert!(
        !tmp_path.exists(),
        "超限 chunk 拒绝后不得创建或写入临时文件"
    );

    // 恰好 CHUNK_SIZE 必须仍可通过大小校验（落盘前不再因 size 被拒）。
    let exact = vec![1u8; CHUNK_SIZE];
    let resp = handle_chunk(&state, &transfer_id, 0, exact)
        .await
        .expect("exact CHUNK_SIZE must pass size gate");
    assert!(resp.success);
    assert_eq!(resp.received_bytes, CHUNK_SIZE as u64);
    assert!(tmp_path.exists(), "合法 CHUNK_SIZE chunk 应写入临时文件");
    assert_eq!(
        fs::metadata(&tmp_path).expect("tmp meta").len(),
        CHUNK_SIZE as u64
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     接收 chunk 若只按 id 取任务，攻击者可用 outbound Send 任务 id 改写/删除本机源文件。
///
/// Code Logic（这个测试做什么）:
///     注册真实 Send 任务指向源文件，用该 id 提交 chunk，断言 success=false 且源文件内容不变。
#[tokio::test]
async fn handle_chunk_rejects_outbound_send_task_without_touching_source() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let source_path = receive_dir.join("outbound-source.bin");
    let original = b"KEEP-SOURCE-BYTES";
    fs::write(&source_path, original).unwrap();

    let transfer_id = "outbound-send-id".to_string();
    state.transfers.add(TransferTask {
        id: transfer_id.clone(),
        filename: "outbound-source.bin".to_string(),
        file_path: source_path.to_string_lossy().to_string(),
        size: original.len() as u64,
        sha256: "deadbeef".to_string(),
        chunk_size: 4,
        direction: TransferDirection::Send,
        peer_device_id: "peer".to_string(),
        status: TransferStatus::Transferring,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        ..TransferTask::recovery_defaults(&transfer_id)
    });

    let resp = handle_chunk(&state, &transfer_id, 0, b"XXXX".to_vec())
        .await
        .expect("chunk call should return Ok envelope");
    assert!(
        !resp.success,
        "对 Send 任务的 chunk 必须失败，不得写入源文件"
    );
    assert_eq!(resp.received_bytes, 0);
    let after = fs::read(&source_path).expect("源文件应仍存在");
    assert_eq!(after, original, "outbound 源文件内容必须完全不变");
    // 任务应仍在 registry 中且仍是 Send
    let task = state.transfers.get(&transfer_id).expect("Send 任务应保留");
    assert_eq!(task.direction, TransferDirection::Send);

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     init 幂等路径若命中 Send entry 并返回 accepted，会把发送源路径暴露给后续 chunk 写入。
///
/// Code Logic（这个测试做什么）:
///     先放一个 Send 任务，再用相同 transfer_id 调 init，断言 Conflict 且源文件不变。
#[tokio::test]
async fn handle_init_rejects_send_entry_on_idempotent_path() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let source_path = receive_dir.join("send-source.txt");
    let original = b"send-source-content";
    fs::write(&source_path, original).unwrap();
    let transfer_id = "send-init-id".to_string();

    state.transfers.add(TransferTask {
        id: transfer_id.clone(),
        filename: "send-source.txt".to_string(),
        file_path: source_path.to_string_lossy().to_string(),
        size: original.len() as u64,
        sha256: "abc".to_string(),
        chunk_size: 4,
        direction: TransferDirection::Send,
        peer_device_id: "peer".to_string(),
        status: TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        ..TransferTask::recovery_defaults(&transfer_id)
    });

    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "send-source.txt".to_string(),
            size: original.len() as u64,
            sha256: "abc".to_string(),
            chunk_size: 4,
        },
    )
    .await
    .expect_err("init 命中 Send entry 必须 conflict");
    assert!(
        matches!(err, AppError::Conflict(_)),
        "应返回 Conflict: {err:?}"
    );
    let after = fs::read(&source_path).unwrap();
    assert_eq!(after, original);
    assert_eq!(
        state.transfers.get(&transfer_id).unwrap().direction,
        TransferDirection::Send
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     绝对路径 filename 经 PathBuf::join 会替换 receive_dir，导致任意路径写入。
///
/// Code Logic（这个测试做什么）:
///     init 提交绝对路径 filename，断言 Validation 错误，且 receive_dir 外不产生新文件。
#[tokio::test]
async fn handle_init_rejects_absolute_filename() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let outside = std::env::temp_dir()
        .join("cp_transfer_tests")
        .join(format!("escape-abs-{}", SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_file(&outside);

    let abs_name = outside.to_string_lossy().to_string();
    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some("abs-name".to_string()),
            filename: abs_name,
            size: 1,
            sha256: "x".to_string(),
            chunk_size: 1,
        },
    )
    .await
    .expect_err("绝对路径 filename 必须拒绝");
    assert!(
        matches!(err, AppError::Validation(_)),
        "应返回 Validation: {err:?}"
    );
    assert!(!outside.exists(), "不得在 receive_dir 外创建目标");
    assert!(state.transfers.list().is_empty());

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     `../` 相对路径可逃逸 receive_dir，必须在 init 边界拒绝。
#[tokio::test]
async fn handle_init_rejects_parent_dir_filename() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;

    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some("parent-escape".to_string()),
            filename: "../evil.bin".to_string(),
            size: 1,
            sha256: "x".to_string(),
            chunk_size: 1,
        },
    )
    .await
    .expect_err("../ filename 必须拒绝");
    assert!(
        matches!(err, AppError::Validation(_)),
        "应返回 Validation: {err:?}"
    );
    assert!(state.transfers.list().is_empty());

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     basename 校验本身必须拒绝绝对路径、父目录与多组件路径。
#[test]
fn sanitize_receive_basename_rejects_escape_patterns() {
    assert!(sanitize_receive_basename("ok.txt", "filename").is_ok());
    assert!(sanitize_receive_basename("/tmp/x", "filename").is_err());
    assert!(sanitize_receive_basename("../x", "filename").is_err());
    assert!(sanitize_receive_basename("a/b", "filename").is_err());
    assert!(sanitize_receive_basename("a\\b", "filename").is_err());
    assert!(sanitize_receive_basename("..", "filename").is_err());
    assert!(sanitize_receive_basename(".", "filename").is_err());
    assert!(sanitize_receive_basename("", "filename").is_err());
    assert!(sanitize_receive_basename("  ", "filename").is_err());
}

/// Business Logic（为什么需要这个测试）:
///     不同 transfer_id 并发接收同名文件时，不得后写覆盖先落地的内容；两份数据都必须保留。
///
/// Code Logic（这个测试做什么）:
///     并发 finalize 两个同名不同内容的 Receive 任务，断言两个最终文件都存在且内容互不覆盖。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_name_different_transfer_ids_do_not_overwrite() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;

    let payload_a = b"CONTENT-A".to_vec();
    let payload_b = b"CONTENT-B-DIFFERENT".to_vec();
    let sha_a = format!("{:x}", Sha256::digest(&payload_a));
    let sha_b = format!("{:x}", Sha256::digest(&payload_b));
    let id_a = "same-name-a".to_string();
    let id_b = "same-name-b".to_string();
    let tmp_a = receive_dir.join(format!(".{id_a}.tmp"));
    let tmp_b = receive_dir.join(format!(".{id_b}.tmp"));
    fs::write(&tmp_a, &payload_a).unwrap();
    fs::write(&tmp_b, &payload_b).unwrap();

    for (id, tmp, size, sha) in [
        (id_a.clone(), tmp_a.clone(), payload_a.len() as u64, sha_a),
        (id_b.clone(), tmp_b.clone(), payload_b.len() as u64, sha_b),
    ] {
        state.transfers.add(TransferTask {
            id: id.clone(),
            filename: "report.txt".to_string(),
            file_path: tmp.to_string_lossy().to_string(),
            size,
            sha256: sha,
            chunk_size: 64,
            direction: TransferDirection::Receive,
            peer_device_id: String::new(),
            status: TransferStatus::Transferring,
            transferred_bytes: size,
            created_at: now_iso(),
            completed_at: None,
            ..TransferTask::recovery_defaults(&id)
        });
    }

    let state_a = state.clone();
    let state_b = state.clone();
    let (r1, r2) = tokio::join!(
        async move { finalize_transfer(&state_a, "same-name-a").await },
        async move { finalize_transfer(&state_b, "same-name-b").await },
    );
    r1.expect("finalize A");
    r2.expect("finalize B");

    let path_plain = receive_dir.join("report.txt");
    let path_one = receive_dir.join("report (1).txt");
    assert!(path_plain.exists(), "应保留第一份 report.txt");
    assert!(path_one.exists(), "第二份应落为 report (1).txt");

    let mut contents = vec![
        fs::read(&path_plain).expect("read plain"),
        fs::read(&path_one).expect("read (1)"),
    ];
    contents.sort();
    let mut expected = vec![payload_a, payload_b];
    expected.sort();
    assert_eq!(contents, expected, "两份内容都必须完整保留且互不覆盖");
    assert!(
        !tmp_a.exists() && !tmp_b.exists(),
        "tmp 应在 hard_link 提交后移除"
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     hard_link 提交必须真正落地最终文件并移除 tmp；不得留下零字节占位当成功。
#[tokio::test]
async fn place_final_file_hard_link_commits_content_and_removes_tmp() {
    let receive_dir = unique_temp_dir();
    let tmp = receive_dir.join(".place-hl.tmp");
    let payload = b"hard-link-payload";
    fs::write(&tmp, payload).unwrap();

    let placed = place_final_file_exclusive(&receive_dir, "hl.txt", &tmp)
        .await
        .expect("hard_link place");
    assert_eq!(placed.final_filename, "hl.txt");
    assert_eq!(fs::read(&placed.final_path).unwrap(), payload);
    assert!(!tmp.exists(), "成功 hard_link 后应删除 tmp");
    // 最终文件不得是零字节占位。
    assert_eq!(
        fs::metadata(&placed.final_path).unwrap().len(),
        payload.len() as u64
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     目标路径已存在时提交必须失败并换名，且不得删除/覆盖竞争者文件内容。
#[tokio::test]
async fn place_final_file_does_not_overwrite_or_delete_competitor() {
    let receive_dir = unique_temp_dir();
    let competitor = receive_dir.join("report.txt");
    let competitor_bytes = b"EXTERNAL-COMPETITOR";
    fs::write(&competitor, competitor_bytes).unwrap();

    let tmp = receive_dir.join(".place-comp.tmp");
    let payload = b"incoming-transfer";
    fs::write(&tmp, payload).unwrap();

    let placed = place_final_file_exclusive(&receive_dir, "report.txt", &tmp)
        .await
        .expect("should pick alternate name");
    assert_ne!(placed.final_path, competitor, "不得占用已存在的竞争者路径");
    assert_eq!(
        fs::read(&competitor).unwrap(),
        competitor_bytes,
        "竞争者文件内容必须原样保留"
    );
    assert_eq!(fs::read(&placed.final_path).unwrap(), payload);
    assert!(!tmp.exists());

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     hard_link 失败回退路径中，清理逻辑不得删除非本次创建的最终文件。
///     用已存在路径直接调 commit，断言 AlreadyExists 且竞争者仍在。
#[tokio::test]
async fn commit_no_replace_failure_preserves_existing_final() {
    let receive_dir = unique_temp_dir();
    let final_path = receive_dir.join("keep-me.bin");
    let existing = b"do-not-delete";
    fs::write(&final_path, existing).unwrap();
    let tmp = receive_dir.join(".commit-fail.tmp");
    fs::write(&tmp, b"new-bytes").unwrap();

    let err = commit_tmp_to_final_no_replace(&tmp, &final_path)
        .await
        .expect_err("existing final must fail");
    assert!(
        matches!(err, CommitFinalError::AlreadyExists),
        "expected AlreadyExists, got non-matching commit error"
    );
    assert_eq!(fs::read(&final_path).unwrap(), existing);
    // tmp 仍应存在（提交未成功，不应删除源）。
    assert!(tmp.exists(), "失败时不应删除 tmp 源文件");

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     落盘并写 history 后若进程重启（内存墓碑清空），complete/status 仍须按 Receive
///     历史收敛为 completed，否则发送端会假失败并可能产生后缀副本。
///
/// Code Logic（这个测试做什么）:
///     complete 空文件 → clear_tombstones_for_test 模拟重启 → complete 与 status 均成功。
#[tokio::test]
async fn handle_complete_and_status_survive_restart_via_history() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let empty_sha = format!("{:x}", Sha256::digest(b""));
    let transfer_id = "restart-history-complete".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "restart.txt".to_string(),
            size: 0,
            sha256: empty_sha,
            chunk_size: 1,
        },
    )
    .await
    .expect("init");

    let first = handle_complete(&state, &transfer_id)
        .await
        .expect("first complete");
    assert!(first.success);
    assert!(receive_dir.join("restart.txt").exists());

    // 模拟接收端重启：内存墓碑与 active 均消失，仅 history 残留。
    state.transfers.clear_tombstones_for_test();
    assert!(state.transfers.tombstone(&transfer_id).is_none());
    assert!(state.transfers.get(&transfer_id).is_none());

    let after_restart = handle_complete(&state, &transfer_id)
        .await
        .expect("complete after restart");
    assert!(
        after_restart.success,
        "history 中 completed Receive 应让 complete 成功"
    );

    let status = handle_status(&state, &transfer_id).await;
    assert_eq!(status.status, "completed");
    assert_eq!(status.filename, "restart.txt");

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     零字节文件不会触发 chunk 路径；complete 握手必须能校验空内容并落地最终文件。
#[tokio::test]
async fn handle_complete_finalizes_empty_file() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let empty_sha = format!("{:x}", Sha256::digest(b""));
    let transfer_id = "empty-complete".to_string();

    let init = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "empty.txt".to_string(),
            size: 0,
            sha256: empty_sha,
            chunk_size: 1,
        },
    )
    .await
    .expect("init empty");
    assert_eq!(init.resume_offset, 0);

    let resp = handle_complete(&state, &transfer_id)
        .await
        .expect("complete empty");
    assert!(resp.success, "空文件 complete 应成功");
    assert_eq!(resp.received_bytes, 0);
    let final_path = receive_dir.join("empty.txt");
    assert!(final_path.exists(), "空文件最终路径应存在");
    assert_eq!(fs::read(&final_path).unwrap(), b"");
    assert!(
        state.transfers.get(&transfer_id).is_none(),
        "complete 后应移除 active"
    );
    assert!(
        matches!(
            state.transfers.tombstone(&transfer_id).map(|t| t.outcome),
            Some(crate::transfer::registry::TransferOutcome::Completed { .. })
        ),
        "应写入成功墓碑"
    );

    // 重放 complete 必须幂等。
    let replay = handle_complete(&state, &transfer_id)
        .await
        .expect("replay complete");
    assert!(replay.success);

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     崩溃后遗留已写满的 .tmp 时，重试 init 返回 resume_offset==size；complete 必须
///     校验哈希并原子落地，而不是让发送端空转 chunk 循环后假报完成。
#[tokio::test]
async fn handle_complete_finalizes_full_tmp_after_restart() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"full-tmp-payload";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "full-tmp-restart".to_string();
    let tmp_path = receive_dir.join(format!(".{transfer_id}.tmp"));
    // 模拟崩溃后遗留的写满临时文件。
    fs::write(&tmp_path, payload).unwrap();

    let init = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "resume.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha,
            chunk_size: 4,
        },
    )
    .await
    .expect("init full tmp");
    assert_eq!(
        init.resume_offset,
        payload.len() as u64,
        "写满 tmp 应返回 size 作为 resume_offset"
    );

    let resp = handle_complete(&state, &transfer_id)
        .await
        .expect("complete full tmp");
    assert!(resp.success, "写满 tmp 的 complete 应成功落地");
    let final_path = receive_dir.join("resume.bin");
    assert_eq!(fs::read(&final_path).unwrap(), payload);
    assert!(!tmp_path.exists(), "成功后 tmp 应消失");

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     resume_offset > size 的脏临时文件必须拒绝续传，避免发送端假完成。
#[tokio::test]
async fn handle_init_rejects_tmp_larger_than_declared_size() {
    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let transfer_id = "oversized-tmp".to_string();
    let tmp_path = receive_dir.join(format!(".{transfer_id}.tmp"));
    fs::write(&tmp_path, b"too-large-for-declared-size").unwrap();

    let err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "x.bin".to_string(),
            size: 4,
            sha256: "dead".to_string(),
            chunk_size: 1,
        },
    )
    .await
    .expect_err("oversized tmp must be rejected");
    assert!(
        matches!(err, AppError::Validation(_)),
        "应返回 Validation: {err:?}"
    );
    assert!(
        !tmp_path.exists(),
        "损坏 oversized tmp 应被删除以便干净重试"
    );
    assert!(state.transfers.list().is_empty());

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     最终文件落盘后若 transfer_history 写入失败，不得向发送端报告 completed，
///     也不得 remove active/写成功墓碑；必须保留可恢复状态，并以 retryable 5xx
///     驱动 PeerClient 重试 complete（而非 HTTP 200 success=false 立即终止）。
///
/// Code Logic（这个测试做什么）:
///     init 后 DROP transfer_history 注入 record 失败 → complete 返回 Unavailable；
///     最终文件已落地、active 仍在、无成功墓碑、status≠completed、intent 仍在；
///     重建表后重试 complete 应 durable 成功。
#[tokio::test]
async fn history_record_failure_keeps_recoverable_state_without_claiming_completed() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let empty_sha = format!("{:x}", Sha256::digest(b""));
    let transfer_id = "history-record-fail".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "durable.txt".to_string(),
            size: 0,
            sha256: empty_sha,
            chunk_size: 1,
        },
    )
    .await
    .expect("init");

    // 注入 record 失败：落盘后 INSERT 无表。
    sqlx::query("DROP TABLE transfer_history")
        .execute(&state.db)
        .await
        .expect("drop history table");

    let first_err = handle_complete(&state, &transfer_id)
        .await
        .expect_err("history 失败应返回 retryable Unavailable 而非 success=false");
    assert!(
        matches!(first_err, AppError::Unavailable(_)),
        "应返回 Unavailable 驱动 5xx 重试: {first_err:?}"
    );
    assert!(receive_dir.join("durable.txt").exists(), "最终文件应已落盘");
    let intent_path = receive_dir
        .join(FINALIZE_INTENT_DIR)
        .join(format!("{transfer_id}.json"));
    assert!(intent_path.exists(), "history 失败时 intent 必须保留");
    let active = state
        .transfers
        .get(&transfer_id)
        .expect("history 失败必须保留 active");
    assert_eq!(active.status, TransferStatus::Completed);
    assert!(
        state.transfers.tombstone(&transfer_id).is_none(),
        "未 durable 前不得写成功墓碑"
    );
    let status = handle_status(&state, &transfer_id).await;
    assert_ne!(
        status.status, "completed",
        "未 durable 时 status 不得宣称 completed: {status:?}"
    );
    assert!(
        status.status == "transferring" || status.status == "pending",
        "应表现为可重试中: {status:?}"
    );

    // 恢复 schema 后重试 complete → 应晋升 durable 并宣告完成。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
    )
    .execute(&state.db)
    .await
    .expect("recreate history");
    TransferRepo::ensure_schema(&state.db)
        .await
        .expect("ensure recovery schema");

    let retry = handle_complete(&state, &transfer_id)
        .await
        .expect("retry complete");
    assert!(retry.success, "history 恢复后应 durable 成功");
    assert!(state.transfers.get(&transfer_id).is_none());
    assert!(matches!(
        state.transfers.tombstone(&transfer_id).map(|t| t.outcome),
        Some(crate::transfer::registry::TransferOutcome::Completed { .. })
    ));
    assert!(
        !intent_path.exists(),
        "durable 成功后应清除 finalize intent"
    );
    let hist = state
        .transfer_repo
        .get_by_id(&transfer_id)
        .await
        .expect("repo ok")
        .expect("history row");
    assert_eq!(hist.status, TransferStatus::Completed);
    let status_after = handle_status(&state, &transfer_id).await;
    assert_eq!(status_after.status, "completed");

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     place 成功后、history 写入前进程崩溃时，不得因无 memory/history 而接受同一
///     transfer_id 重新 init 并生成带后缀的重复副本。
///
/// Code Logic（这个测试做什么）:
///     complete 落盘后 DROP history 并 clear registry/tombstones 模拟崩溃；
///     保留 intent + 最终文件 → init 必须 conflict；complete 应恢复 durable 且无后缀文件。
#[tokio::test]
async fn place_before_history_crash_recovers_via_intent_without_suffix_duplicate() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"intent-crash-payload";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "intent-crash".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "crash.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 8,
        },
    )
    .await
    .expect("init");

    // 写入完整 tmp 后 complete（会 place）。
    let tmp = receive_dir.join(format!(".{transfer_id}.tmp"));
    fs::write(&tmp, payload).unwrap();

    // 注入 history 失败：place + intent 成功，但 durable 失败。
    sqlx::query("DROP TABLE transfer_history")
        .execute(&state.db)
        .await
        .expect("drop history");
    let err = handle_complete(&state, &transfer_id)
        .await
        .expect_err("history 失败应 Unavailable");
    assert!(matches!(err, AppError::Unavailable(_)));
    assert!(receive_dir.join("crash.bin").exists());
    let intent_path = receive_dir
        .join(FINALIZE_INTENT_DIR)
        .join(format!("{transfer_id}.json"));
    assert!(intent_path.exists(), "崩溃窗口必须保留 intent");

    // 模拟进程重启：清空内存 active + 墓碑，history 表仍缺失。
    state.transfers.remove(&transfer_id);
    state.transfers.clear_tombstones_for_test();
    assert!(state.transfers.get(&transfer_id).is_none());
    assert!(state.transfers.tombstone(&transfer_id).is_none());

    // 重建 history 表后：init 不得 reopen；complete 应恢复。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
    )
    .execute(&state.db)
    .await
    .expect("recreate history");
    TransferRepo::ensure_schema(&state.db)
        .await
        .expect("ensure recovery schema");

    let init_err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "crash.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha,
            chunk_size: 8,
        },
    )
    .await
    .expect_err("intent+final 存在时 init 必须 conflict");
    assert!(
        matches!(init_err, AppError::Conflict(_)),
        "应 conflict 禁止重传: {init_err:?}"
    );

    let recovered = handle_complete(&state, &transfer_id)
        .await
        .expect("complete 应从 intent 恢复");
    assert!(recovered.success);
    assert_eq!(fs::read(receive_dir.join("crash.bin")).unwrap(), payload);
    assert!(
        !receive_dir.join("crash (1).bin").exists(),
        "不得生成后缀重复副本"
    );
    assert!(!intent_path.exists(), "恢复后应清除 intent");
    let hist = state
        .transfer_repo
        .get_by_id(&transfer_id)
        .await
        .unwrap()
        .expect("history 应写入");
    assert_eq!(hist.status, TransferStatus::Completed);

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     首选文件名已存在时，finalize 会落盘到后缀路径；若 place 后缀后、history 前崩溃
///     且 intent 未指向该后缀路径，重启后同 transfer_id 会 reopen 并再生成第二份后缀副本。
///     每个候选必须 journal-before-place，崩溃后 init 应 conflict、complete 应恢复。
///
/// Code Logic（这个测试做什么）:
///     预置 preferred 同名文件 → complete 落盘到 `name (1).ext`；注入 history 失败后
///     清空 registry 模拟重启；assert intent 指向后缀文件；init 必须 conflict；
///     complete 恢复 durable；不得出现 `name (2).ext` 第二份后缀副本。
#[tokio::test]
async fn suffix_place_before_history_crash_recovers_via_intent_without_second_suffix() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"suffix-intent-crash-payload";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "suffix-intent-crash".to_string();

    // 首选文件名已存在 → finalize 必须走后缀候选。
    fs::write(receive_dir.join("report.bin"), b"preexisting-preferred").unwrap();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "report.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 8,
        },
    )
    .await
    .expect("init");

    let tmp = receive_dir.join(format!(".{transfer_id}.tmp"));
    fs::write(&tmp, payload).unwrap();

    // place + 后缀 intent 成功，history durable 失败。
    sqlx::query("DROP TABLE transfer_history")
        .execute(&state.db)
        .await
        .expect("drop history");
    let err = handle_complete(&state, &transfer_id)
        .await
        .expect_err("history 失败应 Unavailable");
    assert!(matches!(err, AppError::Unavailable(_)));

    let preferred = receive_dir.join("report.bin");
    let suffix1 = receive_dir.join("report (1).bin");
    let suffix2 = receive_dir.join("report (2).bin");
    assert_eq!(
        fs::read(&preferred).unwrap(),
        b"preexisting-preferred",
        "不得覆盖既有首选文件"
    );
    assert_eq!(fs::read(&suffix1).unwrap(), payload, "应落盘到第一后缀候选");
    assert!(!suffix2.exists(), "place 阶段不得提前写第二后缀");

    let intent_path = receive_dir
        .join(FINALIZE_INTENT_DIR)
        .join(format!("{transfer_id}.json"));
    assert!(
        intent_path.exists(),
        "后缀 place 后、history 前必须保留 intent"
    );
    let intent_raw = fs::read_to_string(&intent_path).expect("read intent");
    assert!(
        intent_raw.contains("report (1).bin"),
        "intent 必须指向已 place 的后缀路径，而非已清空的首选: {intent_raw}"
    );

    // 模拟进程重启：清空内存 active + 墓碑。
    state.transfers.remove(&transfer_id);
    state.transfers.clear_tombstones_for_test();
    assert!(state.transfers.get(&transfer_id).is_none());
    assert!(state.transfers.tombstone(&transfer_id).is_none());

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
    )
    .execute(&state.db)
    .await
    .expect("recreate history");
    TransferRepo::ensure_schema(&state.db)
        .await
        .expect("ensure recovery schema");

    let init_err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "report.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha,
            chunk_size: 8,
        },
    )
    .await
    .expect_err("后缀 intent+final 存在时 init 必须 conflict");
    assert!(
        matches!(init_err, AppError::Conflict(_)),
        "应 conflict 禁止重传: {init_err:?}"
    );

    let recovered = handle_complete(&state, &transfer_id)
        .await
        .expect("complete 应从后缀 intent 恢复");
    assert!(recovered.success);
    assert_eq!(fs::read(&suffix1).unwrap(), payload);
    assert_eq!(fs::read(&preferred).unwrap(), b"preexisting-preferred");
    assert!(
        !suffix2.exists(),
        "不得因重启 reopen 生成第二份后缀副本 report (2).bin"
    );
    assert!(!intent_path.exists(), "恢复后应清除 intent");
    let hist = state
        .transfer_repo
        .get_by_id(&transfer_id)
        .await
        .unwrap()
        .expect("history 应写入");
    assert_eq!(hist.status, TransferStatus::Completed);
    assert!(
        hist.file_path.ends_with("report (1).bin"),
        "history 应记录后缀最终路径: {}",
        hist.file_path
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     intent 写在 no-replace place 之前：若候选被同尺寸不同内容的竞争文件占用，
///     place 返回 AlreadyExists 后、下一轮覆盖 intent 前崩溃，intent 仍指向竞争文件。
///     恢复若只比 size 会把竞争文件晋升为 Completed 并向发送端确认成功，原始 tmp 永久丢失。
///
/// Code Logic（这个测试做什么）:
///     写入真实 payload 的 .tmp + 同尺寸不同内容的碰撞 final；手工写 intent 指向碰撞文件
///     （含正确 intent.sha256=tmp 哈希）；清空 registry 模拟 AlreadyExists 后崩溃重启；
///     complete/recover 不得晋升 history Completed，tmp 保留；随后 complete 可安全落到下一后缀。
#[tokio::test]
async fn collision_same_size_different_hash_intent_must_not_promote_on_recovery() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    // 同尺寸、不同内容：仅 size 校验无法区分。
    let payload = b"AAAA-payload-bytes!"; // 19 bytes
    let collision = b"BBBB-collision-byte"; // 19 bytes
    assert_eq!(payload.len(), collision.len());
    let sha = format!("{:x}", Sha256::digest(payload));
    let collision_sha = format!("{:x}", Sha256::digest(collision));
    assert_ne!(sha, collision_sha);
    let transfer_id = "collision-same-size".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "doc.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 8,
        },
    )
    .await
    .expect("init");

    let tmp = receive_dir.join(format!(".{transfer_id}.tmp"));
    fs::write(&tmp, payload).unwrap();

    // 模拟：intent 已写指向首选候选，place 因 AlreadyExists 失败后崩溃。
    // 竞争者同尺寸、不同哈希。
    let collision_path = receive_dir.join("doc.bin");
    fs::write(&collision_path, collision).unwrap();
    let intent = FinalizeIntent {
        transfer_id: transfer_id.clone(),
        filename: "doc.bin".to_string(),
        size: payload.len() as u64,
        sha256: sha.clone(),
        chunk_size: 8,
        final_filename: "doc.bin".to_string(),
        final_path: collision_path.to_string_lossy().to_string(),
        created_at: now_iso(),
    };
    write_finalize_intent(&receive_dir, &intent)
        .await
        .expect("write intent pointing at collision");

    // 模拟进程重启：清空 active + 墓碑（intent + tmp + 碰撞文件仍在）。
    state.transfers.remove(&transfer_id);
    state.transfers.clear_tombstones_for_test();
    assert!(state.transfers.get(&transfer_id).is_none());
    assert!(tmp.exists(), "tmp 必须保留供安全重试");

    // 恢复不得把同尺寸碰撞文件晋升为 Completed。
    let recovered = try_recover_finalize_intent(&state, &transfer_id, &receive_dir)
        .await
        .expect("recover ok");
    assert!(
        recovered.is_none(),
        "sha 不匹配时不得返回 success ChunkResp"
    );
    assert!(
        state
            .transfer_repo
            .get_by_id(&transfer_id)
            .await
            .expect("repo ok")
            .is_none(),
        "不得写入 Completed history"
    );
    assert_eq!(
        fs::read(&collision_path).unwrap(),
        collision,
        "不得改写/删除竞争文件"
    );
    assert!(tmp.exists(), "不匹配时不得清除原始 tmp");
    assert!(
        !receive_dir
            .join(FINALIZE_INTENT_DIR)
            .join(format!("{transfer_id}.json"))
            .exists(),
        "不匹配后应清除过期 intent，允许后续干净 place"
    );
    assert!(
        state.transfers.tombstone(&transfer_id).is_none(),
        "不得写成功墓碑"
    );

    // 可继续安全 finalize：重新 init 后 complete 应落到下一后缀，不覆盖碰撞文件。
    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "doc.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 8,
        },
    )
    .await
    .expect("init after safe reject must reopen");
    // resume 可能已见 tmp 全量；ensure tmp 内容仍在。
    assert_eq!(fs::read(&tmp).unwrap(), payload);

    let completed = handle_complete(&state, &transfer_id)
        .await
        .expect("complete should place next suffix");
    assert!(completed.success);
    assert_eq!(
        fs::read(&collision_path).unwrap(),
        collision,
        "不得覆盖同尺寸碰撞文件"
    );
    let suffix = receive_dir.join("doc (1).bin");
    assert_eq!(
        fs::read(&suffix).unwrap(),
        payload,
        "真实内容应落到下一后缀"
    );
    let hist = state
        .transfer_repo
        .get_by_id(&transfer_id)
        .await
        .unwrap()
        .expect("history after real place");
    assert_eq!(hist.status, TransferStatus::Completed);
    assert_eq!(hist.sha256, sha);
    assert!(
        hist.file_path.ends_with("doc (1).bin"),
        "history 应记录真实落盘路径: {}",
        hist.file_path
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     `metadata()` 跟随符号链接：若 intent 指向 symlink（目标同尺寸同哈希），
///     is_file + 跟随哈希会通过并误晋升 Completed，尽管本次 .tmp 从未 place 到 final。
///     链接被删/改指后静默丢数据，直接违反 regular-file 恢复不变量。
///
/// Code Logic（这个测试做什么）:
///     写真实 payload 的 target 与 .tmp；创建指向 target 的 symlink 作为 final；
///     intent.sha256=payload 哈希；清空 registry 模拟崩溃；recover 必须 None、
///     不写 history、保留 tmp 与 intent 清除后允许干净重试。
#[tokio::test]
#[cfg(unix)]
async fn symlink_matching_content_must_not_promote_on_recovery() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::symlink;

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"symlink-target-payload!!";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "symlink-bypass".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "via-link.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: 8,
        },
    )
    .await
    .expect("init");

    let tmp = receive_dir.join(format!(".{transfer_id}.tmp"));
    fs::write(&tmp, payload).unwrap();

    // 同尺寸同内容的真实目标 + 指向它的 symlink 作为 intent final。
    let real_target = receive_dir.join("real-target.bin");
    fs::write(&real_target, payload).unwrap();
    let link_path = receive_dir.join("via-link.bin");
    symlink(&real_target, &link_path).expect("create symlink final");
    assert!(
        fs::symlink_metadata(&link_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "fixture 必须是 symlink"
    );
    // 跟随 metadata 会把 symlink 当普通文件：这正是要堵住的绕过。
    let followed = fs::metadata(&link_path).unwrap();
    assert!(followed.is_file());
    assert_eq!(followed.len(), payload.len() as u64);

    let intent = FinalizeIntent {
        transfer_id: transfer_id.clone(),
        filename: "via-link.bin".to_string(),
        size: payload.len() as u64,
        sha256: sha.clone(),
        chunk_size: 8,
        final_filename: "via-link.bin".to_string(),
        final_path: link_path.to_string_lossy().to_string(),
        created_at: now_iso(),
    };
    write_finalize_intent(&receive_dir, &intent)
        .await
        .expect("write intent pointing at symlink");

    state.transfers.remove(&transfer_id);
    state.transfers.clear_tombstones_for_test();

    let recovered = try_recover_finalize_intent(&state, &transfer_id, &receive_dir)
        .await
        .expect("recover ok");
    assert!(
        recovered.is_none(),
        "symlink 即使指向匹配内容也不得晋升 Completed"
    );
    assert!(
        state
            .transfer_repo
            .get_by_id(&transfer_id)
            .await
            .expect("repo ok")
            .is_none(),
        "不得写入 Completed history"
    );
    assert!(tmp.exists(), "不得清除原始 tmp");
    assert!(
        link_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "不得把 symlink 替换/删除"
    );
    assert_eq!(fs::read(&real_target).unwrap(), payload);
    assert!(
        !receive_dir
            .join(FINALIZE_INTENT_DIR)
            .join(format!("{transfer_id}.json"))
            .exists(),
        "拒绝后应清除过期 intent"
    );
    assert!(state.transfers.tombstone(&transfer_id).is_none());

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     若 `.{transfer_id}.tmp` 被预置为指向 receive_dir 外文件的 symlink，普通 OpenOptions
///     会跟随写入；LAN 请求即可越权破坏任意可写文件。chunk 路径必须拒绝跟随。
///
/// Code Logic（这个测试做什么）:
///     在 receive_dir 外放 victim；在 receive_dir 内建指向 victim 的 `.{id}.tmp` symlink；
///     init 后 handle_chunk 必须失败且 victim 内容不变。
#[tokio::test]
#[cfg(unix)]
async fn chunk_refuses_to_follow_tmp_symlink_outside_receive_dir() {
    use std::os::unix::fs::symlink;

    let receive_dir = unique_temp_dir();
    let outside_dir = unique_temp_dir();
    let victim = outside_dir.join("victim.bin");
    fs::write(&victim, b"ORIGINAL-OUTSIDE").unwrap();

    let state = build_transfer_test_state(&receive_dir).await;
    let transfer_id = "tmp-symlink-escape".to_string();
    let tmp = receive_dir.join(format!(".{transfer_id}.tmp"));
    symlink(&victim, &tmp).expect("pre-plant tmp symlink");

    // init：发现 symlink tmp 时拒绝并 best-effort 删除危险路径。
    let init_err = handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "escape.bin".to_string(),
            size: 8,
            sha256: "deadbeef".to_string(),
            chunk_size: 8,
        },
    )
    .await
    .expect_err("init 必须拒绝 symlink tmp 作为 resume 路径");
    let _ = init_err;
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"ORIGINAL-OUTSIDE",
        "init 路径不得跟随改写 victim"
    );

    // 重新种植 symlink，绕过 init 直接测 chunk 写入路径。
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    symlink(&victim, &tmp).expect("re-plant tmp symlink for chunk");
    let task = crate::models::transfer::TransferTask {
        id: transfer_id.clone(),
        filename: "escape.bin".to_string(),
        file_path: tmp.to_string_lossy().to_string(),
        size: 8,
        sha256: "deadbeef".to_string(),
        chunk_size: 8,
        direction: crate::models::transfer::TransferDirection::Receive,
        peer_device_id: String::new(),
        status: crate::models::transfer::TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        ..crate::models::transfer::TransferTask::recovery_defaults(&transfer_id)
    };
    state.transfers.add(task);

    let chunk_err = handle_chunk(&state, &transfer_id, 0, b"ATTACK!!!".to_vec())
        .await
        .expect_err("chunk 不得跟随 tmp symlink 写入");
    let _ = chunk_err;
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"ORIGINAL-OUTSIDE",
        "receive_dir 外 victim 不得被改写"
    );
    // create_new 对既有 symlink 返回 AlreadyExists，随后 no-follow open 失败；
    // 不得把 symlink 替换成写出到 victim 的普通文件。
    if tmp.exists() {
        assert!(
            tmp.symlink_metadata().unwrap().file_type().is_symlink(),
            "危险 tmp 若仍存在必须保持 symlink"
        );
    }

    let _ = fs::remove_dir_all(&receive_dir);
    let _ = fs::remove_dir_all(&outside_dir);
}

/// Business Logic（为什么需要这个测试）:
///     place 成功后 fsync 失败绝不能写成 Failed history，否则 recovery 清 intent 并强制重传后缀副本。
///
/// Code Logic（这个测试做什么）:
///     直接构造 PlaceFinalError::DurabilityPending 语义路径：mark_completed + 不调用 on_receive_failed；
///     校验 PlaceFinalError→AppError 为 unavailable，且 Unplaced 保留原错误。
#[test]
fn place_final_error_maps_durability_pending_to_unavailable() {
    let err: AppError = PlaceFinalError::DurabilityPending {
        placed: PlacedFile {
            final_filename: "a.txt".into(),
            final_path: PathBuf::from("/tmp/a.txt"),
        },
        message: "fsync failed".into(),
    }
    .into();
    let msg = err.to_string();
    assert!(
        msg.contains("fsync failed")
            || msg.contains("Unavailable")
            || msg.contains("不可用")
            || msg.to_lowercase().contains("unavailable"),
        "unexpected: {msg}"
    );
    let unplaced: AppError = PlaceFinalError::Unplaced(AppError::generic("boom")).into();
    assert!(unplaced.to_string().contains("boom"));
}

/// Business Logic（为什么需要这个测试）:
///     崩溃恢复在 re-fsync 失败时必须保留 intent，不能写 Completed history。
///
/// Code Logic（这个测试做什么）:
///     写 intent + 最终普通文件后，用 ensure_final_file_durable 成功路径确认 helper 可用；
///     并断言 recovery 成功路径会清除 intent（正常 fsync 环境）。
#[tokio::test]
async fn ensure_final_file_durable_syncs_existing_regular_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("final.bin");
    tokio::fs::write(&file, b"payload").await.unwrap();
    ensure_final_file_durable(&file)
        .await
        .expect("fsync regular file + parent dir");
}

/// Business Logic（为什么需要这个测试）:
///     intent 目录若被替换为指向 receive_dir 外的 symlink，旧 write 路径会跟随 create/write 逃逸。
///
/// Code Logic（这个测试做什么）:
///     在 receive_dir 外建 victim 目录，在 receive_dir 内把 intent 目录名种为指向 victim 的 symlink；
///     调用 write_finalize_intent 必须失败，且 victim 目录保持空。
#[cfg(unix)]
#[tokio::test]
async fn write_finalize_intent_refuses_symlink_intent_dir() {
    use std::os::unix::fs::symlink;

    let receive_dir = unique_temp_dir();
    let outside_dir = unique_temp_dir();
    fs::create_dir_all(&receive_dir).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    let intent_link = receive_dir.join(FINALIZE_INTENT_DIR);
    symlink(&outside_dir, &intent_link).expect("plant intent dir symlink");
    assert!(intent_link
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());

    let intent = FinalizeIntent {
        transfer_id: "escape-intent-dir".to_string(),
        filename: "a.bin".to_string(),
        size: 1,
        sha256: "aa".to_string(),
        chunk_size: 1,
        final_filename: "a.bin".to_string(),
        final_path: receive_dir.join("a.bin").to_string_lossy().to_string(),
        created_at: now_iso(),
    };
    let err = write_finalize_intent(&receive_dir, &intent)
        .await
        .expect_err("intent 目录 symlink 必须拒绝");
    let msg = err.to_string();
    assert!(
        msg.contains("符号链接") || msg.contains("symlink") || msg.contains("intent"),
        "错误应说明 intent 目录不安全: {msg}"
    );
    assert!(
        fs::read_dir(&outside_dir).unwrap().next().is_none(),
        "不得在 receive_dir 外创建 intent 文件"
    );

    let _ = fs::remove_dir_all(&receive_dir);
    let _ = fs::remove_dir_all(&outside_dir);
}

/// Business Logic（为什么需要这个测试）:
///     intent 临时文件若预置为指向外部文件的 symlink，tokio::fs::write 会跟随截断外部文件。
///
/// Code Logic（这个测试做什么）:
///     先建合法 intent 目录；在目录内预置 `<id>.json.tmp` symlink 指向外部 victim；
///     write_finalize_intent 必须失败或安全覆盖目录项本身，且 victim 内容不变。
#[cfg(unix)]
#[tokio::test]
async fn write_finalize_intent_refuses_to_follow_tmp_symlink() {
    use std::os::unix::fs::symlink;

    let receive_dir = unique_temp_dir();
    let outside_dir = unique_temp_dir();
    fs::create_dir_all(&receive_dir).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    let intent_dir = receive_dir.join(FINALIZE_INTENT_DIR);
    fs::create_dir_all(&intent_dir).unwrap();
    let victim = outside_dir.join("victim.json");
    fs::write(&victim, b"KEEP-ME").unwrap();
    let transfer_id = "escape-intent-tmp";
    let tmp = intent_dir.join(format!("{transfer_id}.json.tmp"));
    symlink(&victim, &tmp).expect("plant intent tmp symlink");

    let intent = FinalizeIntent {
        transfer_id: transfer_id.to_string(),
        filename: "b.bin".to_string(),
        size: 1,
        sha256: "bb".to_string(),
        chunk_size: 1,
        final_filename: "b.bin".to_string(),
        final_path: receive_dir.join("b.bin").to_string_lossy().to_string(),
        created_at: now_iso(),
    };
    // 实现会先 remove_file(tmp) 再 create_new：remove 只删目录项，不应触达 victim；
    // 随后 create_new 在 intent 目录内建普通文件并成功写入。无论成功还是失败，victim 必须不变。
    let _ = write_finalize_intent(&receive_dir, &intent).await;
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"KEEP-ME",
        "不得跟随 intent tmp symlink 截断外部文件"
    );

    let _ = fs::remove_dir_all(&receive_dir);
    let _ = fs::remove_dir_all(&outside_dir);
}

/// Business Logic（为什么需要这个测试）:
///     若 create_new 后关闭句柄再按路径 reopen 写字节，攻击者可在两次 open 之间把 tmp 换成
///     指向外部文件的 hardlink；O_NOFOLLOW 只拦 symlink，会写坏外部目标。
///     单句柄写入：remove 只删目录项，create_new 建新 inode，写操作不得触达 victim。
///
/// Code Logic（这个测试做什么）:
///     外部 victim + intent 目录内 hardlink 到 victim 作为 tmp；调用 write_bytes_create_new_nofollow；
///     成功或失败均断言 victim 仍为 KEEP-HARD（不得经 hardlink 写坏外部文件）。
#[cfg(unix)]
#[tokio::test]
async fn write_bytes_create_new_nofollow_does_not_overwrite_existing_hardlink_target() {
    let receive_dir = unique_temp_dir();
    let outside_dir = unique_temp_dir();
    fs::create_dir_all(&receive_dir).unwrap();
    fs::create_dir_all(&outside_dir).unwrap();
    let intent_dir = receive_dir.join(FINALIZE_INTENT_DIR);
    fs::create_dir_all(&intent_dir).unwrap();
    let victim = outside_dir.join("victim-hardlink-target.json");
    fs::write(&victim, b"KEEP-HARD").unwrap();
    let tmp = intent_dir.join("hardlink-tmp.json.tmp");
    fs::hard_link(&victim, &tmp).expect("plant hardlink tmp -> victim");

    // remove 只删 hardlink 目录项；create_new 新建独立 inode 并写入——victim 必须不变。
    write_bytes_create_new_nofollow(&tmp, b"OVERWRITE")
        .await
        .expect("应在新 inode 上写入，而非 hardlink 目标");
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"KEEP-HARD",
        "不得经 hardlink 覆盖外部 victim"
    );
    assert_eq!(
        fs::read(&tmp).unwrap(),
        b"OVERWRITE",
        "tmp 应为新普通文件内容"
    );

    let _ = fs::remove_dir_all(&receive_dir);
    let _ = fs::remove_dir_all(&outside_dir);
}

/// Business Logic（为什么需要这个测试）:
///     最后一块触发 finalize 后，若文件已落盘、active 已 Completed，但 history 瞬时失败，
///     旧实现返回 HTTP 200 success=false；chunk 客户端只接受 status=completed，
///     而 status 故意仍为 transferring → 发送端永久失败，complete 重试永不执行。
///
/// Code Logic（这个测试做什么）:
///     init + 最后一块 handle_chunk；DROP transfer_history 注入 durable 失败 →
///     必须返回 AppError::Unavailable（非 Ok success=false）；重建表后
///     再 chunk 重放（或 complete）应 durable 成功。
#[tokio::test]
async fn last_chunk_history_failure_returns_unavailable_then_recovers() {
    use sha2::{Digest, Sha256};

    let receive_dir = unique_temp_dir();
    let state = build_transfer_test_state(&receive_dir).await;
    let payload = b"last-chunk-durable!";
    let sha = format!("{:x}", Sha256::digest(payload));
    let transfer_id = "last-chunk-hist-fail".to_string();

    handle_init(
        &state,
        InitMeta {
            transfer_id: Some(transfer_id.clone()),
            filename: "last.bin".to_string(),
            size: payload.len() as u64,
            sha256: sha.clone(),
            chunk_size: payload.len() as u64,
        },
    )
    .await
    .expect("init");

    // 注入 history 失败：place 成功但 durable 失败。
    sqlx::query("DROP TABLE transfer_history")
        .execute(&state.db)
        .await
        .expect("drop history");

    let err = handle_chunk(&state, &transfer_id, 0, payload.to_vec())
        .await
        .expect_err("最后一块 history 失败必须 5xx 而非 success=false");
    assert!(
        matches!(err, AppError::Unavailable(_)),
        "应返回 Unavailable 驱动 chunk/complete 重试: {err:?}"
    );
    assert!(receive_dir.join("last.bin").exists(), "最终文件应已落盘");
    assert_eq!(fs::read(receive_dir.join("last.bin")).unwrap(), payload);
    let active = state
        .transfers
        .get(&transfer_id)
        .expect("history 失败必须保留 active Completed");
    assert_eq!(active.status, TransferStatus::Completed);
    assert!(
        state.transfers.tombstone(&transfer_id).is_none(),
        "未 durable 前不得写成功墓碑"
    );
    let status = handle_status(&state, &transfer_id).await;
    assert_eq!(
        status.status, "transferring",
        "未 durable 时 status 不得宣称 completed: {status:?}"
    );

    // 恢复 schema 后重放最后一块 → 只晋升 durable，不再写文件。
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_history (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                file_path TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                direction TEXT NOT NULL,
                peer_device_id TEXT NOT NULL,
                status TEXT NOT NULL,
                transferred_bytes INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                completed_at TEXT
            )",
    )
    .execute(&state.db)
    .await
    .expect("recreate history");
    TransferRepo::ensure_schema(&state.db)
        .await
        .expect("ensure recovery schema");

    let retry = handle_chunk(&state, &transfer_id, 0, payload.to_vec())
        .await
        .expect("history 恢复后 chunk 重放应 durable 成功");
    assert!(retry.success, "durable 后 chunk 重放应 success=true");
    assert_eq!(retry.received_bytes, payload.len() as u64);
    let hist = state
        .transfer_repo
        .get_by_id(&transfer_id)
        .await
        .unwrap()
        .expect("history row");
    assert_eq!(hist.status, TransferStatus::Completed);
    assert!(
        state.transfers.tombstone(&transfer_id).is_some(),
        "durable 成功后应写墓碑"
    );
    assert!(
        !receive_dir
            .join(FINALIZE_INTENT_DIR)
            .join(format!("{transfer_id}.json"))
            .exists(),
        "durable 成功后应清除 intent"
    );

    let _ = fs::remove_dir_all(&receive_dir);
}

/// Business Logic（为什么需要这个测试）:
///     Windows 回退路径必须调用 MoveFileExW 且 flags=0；不得使用会带
///     MOVEFILE_REPLACE_EXISTING 的 `std::fs::rename`，否则 hard_link 失败时覆盖竞争者。
///
/// Code Logic（这个测试做什么）:
///     源码契约：Windows cfg 块含 MoveFileExW(..., 0)，且不含 std::fs::rename。
#[test]
fn windows_rename_no_replace_source_contract_omits_replace_existing() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/transfer/receiver/finalize.rs"
    ));
    // 定位 rename_no_replace_blocking 内的 Windows 分支，避免命中 fsync_dir 等其它 #[cfg(windows)]。
    let rename_fn = src
        .split("fn rename_no_replace_blocking")
        .nth(1)
        .expect("应存在 rename_no_replace_blocking");
    let after_windows = rename_fn
        .split("#[cfg(windows)]")
        .nth(1)
        .expect("rename_no_replace_blocking 应存在 #[cfg(windows)] 分支");
    let windows_block = after_windows
        .split("#[cfg(not(any(target_os = \"linux\"")
        .next()
        .expect("windows 分支应在 other-os cfg 前结束");
    assert!(
        windows_block.contains("MoveFileExW"),
        "Windows 必须直接调用 MoveFileExW"
    );
    assert!(
        windows_block.contains("MoveFileExW(from.as_ptr(), to.as_ptr(), 0)"),
        "MoveFileExW flags 必须为 0（无 MOVEFILE_REPLACE_EXISTING）"
    );
    assert!(
        !windows_block.contains("std::fs::rename"),
        "禁止 Windows 回退使用 std::fs::rename（std 会 REPLACE_EXISTING）"
    );
    assert!(
        !windows_block.contains("MOVEFILE_REPLACE_EXISTING)"),
        "不得在 flags 中传入 MOVEFILE_REPLACE_EXISTING"
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows intent 写入不得在校验后再用绝对 path create/rename/delete，
///     否则 intent 目录可被换成 junction 导致写出 receive_dir；rename 也不得
///     用错 API 信息类或 RootDirectory=NULL（basename 会相对进程 CWD）。
///
/// Code Logic（这个测试做什么）:
///     源码契约：存在 write_finalize_intent_windows_handle；函数内使用 NtCreateFile /
///     FILE_OPEN_REPARSE_POINT / NtSetInformationFile(FileRenameInformation=10) 且
///     RootDirectory 绑定 intent 目录 HANDLE（非 null）；禁止 SetFileInformationByHandle
///     与 RootDirectory=null_mut 的 rename 路径；write_finalize_intent 的 Windows 分支
///     调用该 helper，且不再调用 ensure_regular_intent_dir /
///     write_bytes_create_new_nofollow / tokio::fs::rename。
#[test]
fn windows_intent_write_uses_directory_handle_relative_ops_source_contract() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/transfer/receiver/finalize.rs"
    ));
    assert!(
        src.contains("fn write_finalize_intent_windows_handle"),
        "必须存在 Windows directory HANDLE 相对路径 intent 写 helper"
    );
    assert!(
        src.contains("fn clear_finalize_intent_windows_handle"),
        "必须存在 Windows directory HANDLE 相对路径 intent 删除 helper"
    );

    let write_fn = src
        .split("fn write_finalize_intent_windows_handle")
        .nth(1)
        .expect("应存在 write_finalize_intent_windows_handle");
    // 截到下一个顶层 async fn / fn clear，避免吞掉全文件。
    let write_body = write_fn
        .split("async fn clear_finalize_intent")
        .next()
        .expect("write helper 应在 clear_finalize_intent 之前结束");
    assert!(
        write_body.contains("NtCreateFile"),
        "Windows intent 写必须 NtCreateFile 相对目录 HANDLE"
    );
    assert!(
        write_body.contains("FILE_OPEN_REPARSE_POINT"),
        "必须 OPEN_REPARSE_POINT 后拒绝 reparse/junction"
    );
    assert!(
        write_body.contains("NtSetInformationFile"),
        "rename 必须 NtSetInformationFile(FileRenameInformation=10)，非 SetFileInformationByHandle"
    );
    assert!(
        write_body.contains("FILE_RENAME_INFORMATION_CLASS: ULONG = 10")
            || write_body.contains("const FILE_RENAME_INFORMATION_CLASS: ULONG = 10"),
        "FileRenameInformation class 必须为 10（Nt 路径）"
    );
    // rename_relative 必须把 intent_dir 写入 root_directory，禁止 null_mut。
    let rename_fn = write_body
        .split("unsafe fn rename_relative")
        .nth(1)
        .expect("应存在 rename_relative");
    let rename_body = rename_fn
        .split("let receive_wide")
        .next()
        .expect("rename_relative 应在 receive_wide 前结束");
    assert!(
        rename_body.contains("root_directory = intent_dir")
            || rename_body.contains("(*info).root_directory = intent_dir"),
        "RootDirectory 必须绑定 intent 目录 HANDLE"
    );
    assert!(
        !rename_body.contains("root_directory = ptr::null_mut()")
            && !rename_body.contains("root_directory = std::ptr::null_mut()"),
        "禁止 RootDirectory=NULL（basename 会相对 CWD）"
    );
    // 生产路径不得声明/调用 Win32 SetFile* rename API（注释中的禁令除外，用 Nt 判定）。
    assert!(
        !write_body.contains("fn SetFileInformationByHandle")
            && !write_body.contains("SetFileInformationByHandle("),
        "禁止声明或调用 SetFileInformationByHandle 做 rename（信息类错误）"
    );
    assert!(
        write_body.contains("FILE_FLAG_BACKUP_SEMANTICS"),
        "打开目录 HANDLE 需要 FILE_FLAG_BACKUP_SEMANTICS"
    );
    assert!(
        !write_body.contains("tokio::fs::rename"),
        "Windows intent helper 禁止 path-based tokio::fs::rename"
    );
    assert!(
        !write_body.contains("tokio::fs::remove_file"),
        "Windows intent helper 禁止 path-based remove_file"
    );

    // write_finalize_intent 的 Windows 分支应调用 handle helper，不再 path-ops。
    let write_intent = src
        .split("async fn write_finalize_intent")
        .nth(1)
        .expect("应存在 write_finalize_intent");
    let write_intent_body = write_intent
        .split("async fn ensure_regular_intent_dir")
        .next()
        .expect("write_finalize_intent 应在 ensure_regular_intent_dir 前结束");
    assert!(
        write_intent_body.contains("write_finalize_intent_windows_handle"),
        "write_finalize_intent Windows 分支必须调用 handle helper"
    );
    assert!(
        !write_intent_body.contains("ensure_regular_intent_dir(receive_dir)"),
        "write_finalize_intent 不得再 path-check-then-ops ensure_regular_intent_dir"
    );
    assert!(
        !write_intent_body.contains("write_bytes_create_new_nofollow"),
        "write_finalize_intent 不得再 path create_new 写 intent tmp"
    );
}

/// Business Logic（为什么需要这个测试）:
///     DurabilityPending 重试若只按路径 reopen+fsync，普通文件被原子替换后仍可能
///     用原始 size/SHA 写 Completed history，造成静默数据丢失。
///
/// Code Logic（这个测试做什么）:
///     写入原始内容并 certify 成功；再原子 rename 替换为另一普通文件（不同 SHA），
///     再 certify 必须失败；源码契约要求 promote 走 certify_final_file_for_history。
#[test]
fn certify_final_file_rejects_ordinary_file_replacement() {
    let dir = unique_temp_dir();
    let final_path = dir.join("payload.bin");
    let original = b"original-transfer-bytes-v1";
    fs::write(&final_path, original).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(original);
    let sha = format!("{:x}", hasher.finalize());
    let size = original.len() as u64;

    certify_final_file_for_history_blocking(&final_path, size, &sha)
        .expect("原始文件应通过 handle-bound certify");

    // 原子替换：另一普通文件覆盖同名目录项（同尺寸不同内容）。
    let mut replacement = b"REPLACED-BY-RACE-CONTENT".to_vec();
    while replacement.len() < original.len() {
        replacement.push(b'X');
    }
    replacement.truncate(original.len());
    assert_ne!(&replacement[..], &original[..]);
    let swap = dir.join("payload.bin.swap");
    fs::write(&swap, &replacement).unwrap();
    fs::rename(&swap, &final_path).unwrap();

    let err = certify_final_file_for_history_blocking(&final_path, size, &sha)
        .expect_err("替换后的普通文件不得用原 SHA 认证成功");
    let msg = err.to_string();
    assert!(
        msg.contains("SHA256")
            || msg.contains("身份")
            || msg.contains("替换")
            || msg.contains("不一致"),
        "错误应说明内容/身份不匹配: {msg}"
    );

    // 源码契约：promote 必须调用 certify，而非仅 ensure_final_file_durable。
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/transfer/receiver/finalize.rs"
    ));
    let promote = src
        .split("async fn promote_completed_to_durable")
        .nth(1)
        .expect("应存在 promote_completed_to_durable");
    let promote_body = promote
        .split("/// Business Logic（为什么需要这个函数）:")
        .next()
        .expect("promote 函数体应在下一 Business Logic 前结束");
    assert!(
        promote_body.contains("certify_final_file_for_history"),
        "promote 写 history 前必须 certify_final_file_for_history"
    );
    // Completed 重试分支不得只调 ensure_final_file_durable 后直接 promote。
    let finalize = src
        .split("pub async fn finalize_transfer")
        .nth(1)
        .expect("应存在 finalize_transfer");
    let completed_retry = finalize
        .split("if task.status == TransferStatus::Completed")
        .nth(1)
        .expect("应存在 Completed 重试分支");
    let completed_retry_body = completed_retry
        .split("let tmp_path")
        .next()
        .expect("Completed 分支应在 tmp_path 前结束");
    assert!(
        !completed_retry_body.contains("ensure_final_file_durable"),
        "Completed 重试不得仅 ensure_final_file_durable（缺身份确认）"
    );
    assert!(
        completed_retry_body.contains("promote_completed_to_durable"),
        "Completed 重试应直接 promote（内部 certify）"
    );
}

/// Business Logic（为什么需要这个测试）:
///     Windows 上 FlushFileBuffers 要求句柄具备 GENERIC_WRITE；若 certify/fsync_dir
///     仍用只读句柄，AccessDenied 会使 Completed history 永远无法晋升，任务永久
///     停在 DurabilityPending。
///
/// Code Logic（这个测试做什么）:
///     源码契约：certify_final_file_for_history_blocking 以 writable=true 打开；
///     fsync_dir 的 Windows 分支含 write(true)；sync_regular_file 以 writable=true
///     打开。Unix 路径不改语义（writable 额外 write 标志在 fsync 前仍可读 hash）。
#[test]
fn windows_certify_and_fsync_dir_request_write_access_source_contract() {
    let src = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/transfer/receiver/finalize.rs"
    ));

    // certify：必须 writable=true 打开（hash+FlushFileBuffers 同一句柄）。
    let certify_fn = src
        .split("fn certify_final_file_for_history_blocking")
        .nth(1)
        .expect("应存在 certify_final_file_for_history_blocking");
    let certify_body = certify_fn
        .split("/// Business Logic（为什么需要这个函数）:")
        .next()
        .expect("certify 函数体应在下一 Business Logic 前结束");
    assert!(
        certify_body.contains("open_regular_file_nofollow_std(final_path, true)"),
        "certify 必须以 writable=true no-follow 打开（Windows FlushFileBuffers 需 GENERIC_WRITE）"
    );
    assert!(
        !certify_body.contains("open_regular_file_nofollow_std(final_path, false)"),
        "certify 禁止只读打开后 sync_all（Windows 会 AccessDenied）"
    );
    assert!(
        certify_body.contains("file.sync_all()"),
        "certify 仍须同一句柄 sync_all"
    );
    assert!(
        certify_body.contains("fsync_dir(parent)"),
        "certify 仍须 fsync 父目录"
    );

    // fsync_dir Windows：目录句柄必须 write(true)。
    let fsync_fn = src
        .split("fn fsync_dir(dir: &Path)")
        .nth(1)
        .expect("应存在 fsync_dir");
    let fsync_body = fsync_fn
        .split("/// Business Logic（为什么需要这个函数）:")
        .next()
        .expect("fsync_dir 函数体应在下一 Business Logic 前结束");
    let fsync_windows = fsync_body
        .split("#[cfg(windows)]")
        .nth(1)
        .expect("fsync_dir 应存在 #[cfg(windows)] 分支");
    let fsync_windows_block = fsync_windows
        .split("#[cfg(not(any(unix, windows)))]")
        .next()
        .expect("windows 分支应在 not-any 前结束");
    assert!(
        fsync_windows_block.contains(".write(true)"),
        "Windows fsync_dir 必须以 write(true) 打开目录（FlushFileBuffers 需 GENERIC_WRITE）"
    );
    assert!(
        fsync_windows_block.contains("FILE_FLAG_BACKUP_SEMANTICS"),
        "Windows fsync_dir 仍需 BACKUP_SEMANTICS 打开目录"
    );
    assert!(
        fsync_windows_block.contains("FILE_FLAG_OPEN_REPARSE_POINT"),
        "Windows fsync_dir 仍需 OPEN_REPARSE_POINT（no-follow）"
    );
    assert!(
        fsync_windows_block.contains("sync_all()"),
        "Windows fsync_dir 仍须 sync_all≈FlushFileBuffers"
    );

    // place 前/后的普通文件 sync 同样需要写权限句柄。
    let sync_fn = src
        .split("async fn sync_regular_file")
        .nth(1)
        .expect("应存在 sync_regular_file");
    let sync_body = sync_fn
        .split("/// Business Logic（为什么需要这个函数）:")
        .next()
        .expect("sync_regular_file 函数体应在下一 Business Logic 前结束");
    assert!(
        sync_body.contains("open_regular_file_nofollow(path, true)"),
        "sync_regular_file 必须以 writable=true 打开（Windows FlushFileBuffers）"
    );
    assert!(
        !sync_body.contains("open_regular_file_nofollow(path, false)"),
        "sync_regular_file 禁止只读打开后 sync_all"
    );
}

/// Business Logic（为什么需要这个测试）:
///     hard_link 不可用时 rename_no_replace 仍须 no-replace；在可 hard_link 的宿主上
///     直接测 rename_no_replace_blocking，确保目标存在 → AlreadyExists 且不覆盖。
#[test]
fn rename_no_replace_blocking_preserves_existing_target() {
    let dir = unique_temp_dir();
    let final_path = dir.join("existing.dat");
    let existing = b"competitor-bytes";
    fs::write(&final_path, existing).unwrap();
    let tmp = dir.join("incoming.tmp");
    fs::write(&tmp, b"incoming-bytes").unwrap();

    let err = rename_no_replace_blocking(&tmp, &final_path)
        .expect_err("existing target must fail no-replace");
    assert_eq!(err.kind(), ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&final_path).unwrap(), existing);
    assert!(tmp.exists(), "失败不得删除/移动 tmp 源");
    assert_eq!(fs::read(&tmp).unwrap(), b"incoming-bytes");

    let _ = fs::remove_dir_all(&dir);
}
