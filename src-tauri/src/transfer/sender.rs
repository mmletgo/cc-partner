//! transfer/sender.rs — 文件发送端
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在传输面板选择文件与目标设备后，需将文件分块发送到对端。发送端封装：
//!     计算 SHA256 → init 握手拿 resume_offset → 从 offset 逐块读取发送 → 显式 complete 握手 →
//!     emit 进度/完成/失败/取消事件 → 写 transfer_history。对照 Python `transfer/sender.py`。
//!     N5 起增加幂等 `retry_transfer` / `resume_transfer`：全局 clientOperationId claim 后才 spawn，
//!     resume 复用稳定 protocol_transfer_id，retry 可 mint 新 id；旧 peer 无 resume 能力时拒绝 resume。
//!     T3：`get_transfer_operation` 按发送端 ledger 对账；lost final ACK 经 receiver status 权威
//!     成功后本地单事务提交 completed，uncertain 保持 pending 不提供 retry。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_sending(state, device_id, file_path)`：在调用方线程内 spawn 异步任务，
//!       立即返回 transfer_id（命令层 send_transfer 用）。
//!     - `retry_transfer` / `resume_transfer`：校验父任务状态/指纹/能力 → claim → registry.add → spawn。
//!     - `recover_pending_claimed_operations`：owner 启动时恢复 insert-before-spawn 的 Queued 行。
//!     - `get_transfer_operation`：按 clientOperationId 查发送端 ledger；Finalizing uncertain 时
//!       可按 protocol id 查对端 status 并对账提交。
//!     - spawn 内：查 devices → transfer_init 拿 resume_offset → 分块 → complete 门控。
//!
//! 协议：init/chunk/complete JSON 字段、X-Chunk-Offset header、960KB chunk_size、resume_offset 语义
//!     与既有对端互通；complete 是 capability-gated 的显式终态握手（兼容 size=0 与 full-tmp 续传）。

use crate::error::AppError;
use crate::models::transfer::{
    canonical_recovery_payload_hash, SourceFingerprint, TransferDirection, TransferFailure,
    TransferFailureStage, TransferOperationStatus, TransferPhase, TransferRecoveryKind,
    TransferStatus, TransferTask,
};
use crate::net::peer_error::PeerCallError;
use crate::net::protocol::{CAPABILITY_TRANSFER_COMPLETE_V1, CAPABILITY_TRANSFER_RESUME_V1};
use crate::state::AppState;
use crate::storage::transfer_repo::SenderClaimOutcome;
use crate::transfer::registry::TransferRegistry;
use crate::transfer::CHUNK_SIZE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::SystemTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// 当前时间 RFC3339 ISO 字符串（对照 Python datetime.now().isoformat()）。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// 以 8KB 块流式计算文件 SHA256（避免大文件一次性载入内存），对照 Python `_calculate_sha256`。
fn calculate_sha256(path: &Path) -> Result<String, AppError> {
    use std::fs::File;
    use std::io::{BufReader, Read};
    let f = File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 发送进度事件载荷（camelCase，前端 listen('transfer:progress') 解析）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload {
    id: String,
    transferred_bytes: u64,
    size: u64,
    progress: f64,
}

/// 发送终态事件载荷（completed/failed/cancelled 共用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusPayload {
    id: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
}

/// 启动一次文件发送（异步 spawn，立即返回 transfer_id）。
///
/// Business Logic: 命令层 send_transfer 调用此函数；内部 spawn 异步任务执行实际传输，
///     立即返回 transfer_id 供前端追踪。对照 Python `send_file`。
///
/// Code Logic:
///     1. 校验文件存在并取 size/filename；
///     2. 生成 transfer_id（UUID），构造 TransferTask（status=Pending）；
///     3. registry.add(task)；spawn 异步任务（持有 AppState clone 与 cancel_token clone）；
///     4. 任务内：init → 分块发送循环（检查 cancel）→ emit completed + 写历史 / emit failed。
pub fn start_sending(
    state: AppState,
    device_id: String,
    file_path: String,
) -> Result<String, AppError> {
    let path = Path::new(&file_path);
    if !path.exists() {
        return Err(AppError::NotFound(format!("文件不存在: {file_path}")));
    }
    let metadata = std::fs::metadata(path)?;
    let file_size = metadata.len();
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    // 同步计算 SHA256（发送前必须已知，用于 init 元数据与对端校验）
    let sha256 = calculate_sha256(path)?;
    let transfer_id = Uuid::new_v4().to_string();

    let task = TransferTask {
        id: transfer_id.clone(),
        filename: filename.clone(),
        file_path: file_path.clone(),
        size: file_size,
        sha256: sha256.clone(),
        chunk_size: CHUNK_SIZE as u64,
        direction: TransferDirection::Send,
        peer_device_id: device_id.clone(),
        status: TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        ..TransferTask::recovery_defaults(&transfer_id)
    };

    // 注册任务（附带 CancellationToken），spawn 前先 add 以便 cancel 命令可立即生效
    state.transfers.add(task.clone());

    // 取 cancel_token（spawn 任务循环中每块前检查）
    let cancel_token = state
        .transfers
        .cancel_token(&transfer_id)
        .unwrap_or_default();

    // spawn 异步发送任务（不阻塞命令返回）
    // TransferRegistry 内部为 Arc，Clone 廉价；这里 deref 取出内部值传给循环。
    let registry = (*state.transfers).clone();
    // 在 move 进闭包前 clone 一份 transfer_id 供函数返回值使用
    let returned_id = transfer_id.clone();
    // 首次 send：protocol id 与 local task id 相同。
    let protocol_transfer_id = transfer_id.clone();
    tokio::spawn(async move {
        run_send_loop(
            state,
            registry,
            protocol_transfer_id,
            transfer_id,
            device_id,
            file_path,
            file_size,
            sha256,
            cancel_token,
        )
        .await;
    });

    Ok(returned_id)
}

/// 幂等 retry：同一 logical transfer 新建 attempt，可 mint 新 protocol_transfer_id。
///
/// Business Logic（为什么需要这个函数）:
///     失败且可重试的发送任务需要“重新传输”；同一 clientOperationId 不得产生重复 attempt。
///
/// Code Logic（这个函数做什么）:
///     加载父任务 → 校验 Send/终态/retryable/非 active → 指纹 → claim → spawn。
pub async fn retry_transfer(
    state: AppState,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTask, AppError> {
    start_recovery_operation(
        state,
        task_id,
        client_operation_id,
        TransferRecoveryKind::Retry,
    )
    .await
}

/// 幂等 resume：复用稳定 protocol_transfer_id，要求 peer 具备 resume 能力。
///
/// Business Logic（为什么需要这个函数）:
///     有 checkpoint 的中断任务应续传而非重传；旧 peer 无能力时必须明确 unsupported。
///
/// Code Logic（这个函数做什么）:
///     加载父任务 → 校验 + 源指纹 → 探测 peer `transfer.resume.v1` → claim（复用 protocol id）→ spawn。
pub async fn resume_transfer(
    state: AppState,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTask, AppError> {
    start_recovery_operation(
        state,
        task_id,
        client_operation_id,
        TransferRecoveryKind::Resume,
    )
    .await
}

/// 按发送端全局 clientOperationId 查询 operation 真值。
///
/// Business Logic（为什么需要这个函数）:
///     transport timeout / lost final ACK 后调用方不得盲重试；必须先查询发送端 ledger。
///     receiver 不持有 clientOperationId；对账键仅在本机发送端。
///
/// Code Logic（这个函数做什么）:
///     1) 读 transfer_history by client_operation_id（无行 → NotFound）；
///     2) 终态直接映射 Succeeded/Failed；
///     3) 非终态（含 Finalizing uncertain）尝试 `reconcile_lost_final_ack`：
///        按 protocol id 查对端 status，receiver completed 时本地单事务提交 completed；
///     4) 仍未收敛则 Pending。
pub async fn get_transfer_operation(
    state: &AppState,
    client_operation_id: &str,
) -> Result<TransferOperationStatus, AppError> {
    let id = client_operation_id.trim();
    if id.is_empty() {
        return Ok(TransferOperationStatus::NotFound);
    }

    // registry 优先：活跃 attempt 可能尚未把中间态刷回 history（除 claim 行外）。
    if let Some(active) = find_active_by_client_operation_id(state, id) {
        if is_terminal_status(active.status) {
            return Ok(map_task_to_operation_status(&active));
        }
        if let Some(updated) = reconcile_lost_final_ack(state, &active).await? {
            return Ok(map_task_to_operation_status(&updated));
        }
        return Ok(TransferOperationStatus::Pending);
    }

    let Some(task) = state.transfer_repo.get_by_client_operation_id(id).await? else {
        return Ok(TransferOperationStatus::NotFound);
    };

    if is_terminal_status(task.status) {
        return Ok(map_task_to_operation_status(&task));
    }

    if let Some(updated) = reconcile_lost_final_ack(state, &task).await? {
        return Ok(map_task_to_operation_status(&updated));
    }

    Ok(TransferOperationStatus::Pending)
}

/// Business Logic: list 路径需要在活跃表命中同 op 的 attempt。
/// Code Logic: 线性扫描 registry.list()，匹配 client_operation_id。
fn find_active_by_client_operation_id(
    state: &AppState,
    client_operation_id: &str,
) -> Option<TransferTask> {
    state.transfers.list().into_iter().find(|t| {
        t.client_operation_id
            .as_deref()
            .is_some_and(|op| op == client_operation_id)
    })
}

/// Business Logic: completed/failed/cancelled 是 definitive outcome，不再对账。
/// Code Logic: 匹配 coarse TransferStatus 终态。
fn is_terminal_status(status: TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
    )
}

/// Business Logic: ledger 行 → 调用方可见 operation status（smoke/对账共用）。
/// Code Logic: Completed→Succeeded；Failed/Cancelled→Failed{code}；其余→Pending；None→NotFound。
pub fn operation_status_from_task(task: Option<&TransferTask>) -> TransferOperationStatus {
    match task {
        None => TransferOperationStatus::NotFound,
        Some(t) => map_task_to_operation_status(t),
    }
}

/// Business Logic: ledger 行 → 调用方可见 operation status。
/// Code Logic: Completed→Succeeded；Failed/Cancelled→Failed{code}；其余→Pending。
fn map_task_to_operation_status(task: &TransferTask) -> TransferOperationStatus {
    match task.status {
        TransferStatus::Completed => TransferOperationStatus::Succeeded {
            task_id: task.id.clone(),
        },
        TransferStatus::Failed => TransferOperationStatus::Failed {
            code: task
                .failure
                .as_ref()
                .map(|f| f.code.clone())
                .unwrap_or_else(|| "failed".to_string()),
        },
        TransferStatus::Cancelled => TransferOperationStatus::Failed {
            code: "cancelled".to_string(),
        },
        TransferStatus::Pending | TransferStatus::Transferring => TransferOperationStatus::Pending,
    }
}

/// lost final ACK 对账：receiver 权威 success 后本地提交 completed。
///
/// Business Logic（为什么需要这个函数）:
///     complete 响应丢失时 receiver 可能已 durable success；发送端不得二次破坏性 finalize，
///     只能按 protocolTransferId 查 status，确认后本地单事务写 completed outcome。
///
/// Code Logic（这个函数做什么）:
///     解析 peer base_url → transfer_status_typed(protocol_id)；
///     status=completed → commit_sender_completed_outcome（registry+repo）；
///     pending/unreachable → Ok(None) 保持 pending；不发起 retry。
pub async fn reconcile_lost_final_ack(
    state: &AppState,
    task: &TransferTask,
) -> Result<Option<TransferTask>, AppError> {
    if task.direction != TransferDirection::Send {
        return Ok(None);
    }
    if is_terminal_status(task.status) {
        return Ok(None);
    }

    let protocol_id = if task.protocol_transfer_id.is_empty() {
        task.id.as_str()
    } else {
        task.protocol_transfer_id.as_str()
    };

    let Some(base_url) = resolve_peer_base_url(state, &task.peer_device_id) else {
        return Ok(None);
    };

    let status = match state
        .peer_client
        .transfer_status_typed(&base_url, protocol_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "lost-ACK 对账 status 不可达 op={:?} protocol={protocol_id}: {e}",
                task.client_operation_id
            );
            return Ok(None);
        }
    };

    if !peer_status_is_completed(&status) {
        return Ok(None);
    }

    let updated = commit_sender_completed_outcome(state, task).await?;
    tracing::info!(
        "lost final ACK 对账成功：protocol={protocol_id} task={} → completed",
        updated.id
    );
    Ok(Some(updated))
}

/// Business Logic: devices 表解析对端 host:port。
/// Code Logic: 读锁取 Device → http://host:port。
fn resolve_peer_base_url(state: &AppState, peer_device_id: &str) -> Option<String> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    devices.get(peer_device_id).map(|d| d.base_url())
}

/// Business Logic: 与 peer_client 一致，status 字段 completed 即权威成功。
/// Code Logic: JSON `status == "completed"`。
fn peer_status_is_completed(status: &serde_json::Value) -> bool {
    status
        .get("status")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s == "completed")
}

/// 本地单事务提交发送端 completed + operation outcome。
///
/// Business Logic（为什么需要这个函数）:
///     receiver 权威 success 后，发送端必须把 task completed 与 clientOperationId outcome
///     一并落库，供后续 get_transfer_operation 回放 Succeeded。
///
/// Code Logic（这个函数做什么）:
///     mark_completed（若仍在 registry）→ 构造 completed 快照 → transfer_repo.record
///     （同一 id upsert，保留 client_operation_id）→ remove registry → emit completed。
async fn commit_sender_completed_outcome(
    state: &AppState,
    task: &TransferTask,
) -> Result<TransferTask, AppError> {
    let completed_at = now_iso();
    if state.transfers.get(&task.id).is_some() {
        state
            .transfers
            .mark_completed(&task.id, completed_at.clone(), None);
    }

    let mut snapshot = state
        .transfers
        .get(&task.id)
        .unwrap_or_else(|| task.clone());
    snapshot.status = TransferStatus::Completed;
    snapshot.phase = Some(TransferPhase::Completed);
    snapshot.failure = None;
    snapshot.completed_at = Some(completed_at);
    snapshot.transferred_bytes = snapshot.size;
    // 保留 client_operation_id / protocol ids
    if snapshot.client_operation_id.is_none() {
        snapshot.client_operation_id = task.client_operation_id.clone();
    }
    if snapshot.operation_payload_hash.is_none() {
        snapshot.operation_payload_hash = task.operation_payload_hash.clone();
    }
    if snapshot.protocol_transfer_id.is_empty() {
        snapshot.protocol_transfer_id = task.protocol_transfer_id.clone();
    }
    if snapshot.logical_transfer_id.is_empty() {
        snapshot.logical_transfer_id = task.logical_transfer_id.clone();
    }

    state.transfer_repo.record(&snapshot).await?;
    state.transfers.remove(&task.id);
    state.emit_event(
        "transfer:completed",
        StatusPayload {
            id: snapshot.id.clone(),
            status: "completed".to_string(),
            error_message: None,
        },
    );
    Ok(snapshot)
}

/// 启动时恢复 insert-before-spawn 的 Queued 发送行。
///
/// Business Logic（为什么需要这个函数）:
///     claim 落库后 spawn 前 crash 会留下 Queued 行；owner 重启必须 re-spawn 或标可恢复失败。
///
/// Code Logic（这个函数做什么）:
///     列出 recoverable queued sends；跳过已在 registry 的；源有效则 re-spawn，否则 mark failed。
pub async fn recover_pending_claimed_operations(state: &AppState) -> Result<u32, AppError> {
    let rows = state.transfer_repo.list_recoverable_queued_sends().await?;
    let mut recovered = 0u32;
    for task in rows {
        if state.transfers.get(&task.id).is_some() {
            continue;
        }
        let path = Path::new(&task.file_path);
        if !path.exists() {
            let mut failed = task.clone();
            failed.status = TransferStatus::Failed;
            failed.phase = Some(TransferPhase::Failed);
            failed.failure = Some(TransferFailure {
                stage: TransferFailureStage::Source,
                code: "source_missing".into(),
                retryable: true,
                message: "启动恢复时源文件不存在".into(),
            });
            failed.completed_at = Some(now_iso());
            let _ = state.transfer_repo.record(&failed).await;
            continue;
        }
        // 重算指纹并 re-spawn。
        match recheck_source_fingerprint(path, &task) {
            Ok((size, sha, _fp)) => {
                spawn_claimed_send(state.clone(), task, size, sha);
                recovered += 1;
            }
            Err(err) => {
                let mut failed = task.clone();
                failed.status = TransferStatus::Failed;
                failed.phase = Some(TransferPhase::Failed);
                failed.failure = Some(TransferFailure {
                    stage: TransferFailureStage::Source,
                    code: "source_changed".into(),
                    retryable: true,
                    message: err.to_string(),
                });
                failed.completed_at = Some(now_iso());
                let _ = state.transfer_repo.record(&failed).await;
            }
        }
    }
    Ok(recovered)
}

/// claim + spawn 的共享恢复入口。
///
/// Business Logic: retry/resume 共用校验与幂等 claim，只在 protocol id / capability 上分叉。
/// Code Logic:
///     - Retry 的 payload hash **不含** 随机 protocol id（空串占位）；Fresh 后才用 attempt_id 作 wire protocol id。
///     - Fresh → TOCTOU 重检 → spawn。
///     - Replay 终态 → typed conflict（要求新 op id）；非终态且不在 registry → re-spawn；已在 registry → 回放。
///     - Conflict → AppError::Conflict。
async fn start_recovery_operation(
    state: AppState,
    task_id: String,
    client_operation_id: String,
    kind: TransferRecoveryKind,
) -> Result<TransferTask, AppError> {
    let parent = load_parent_task(&state, &task_id).await?;
    validate_parent_for_recovery(&parent, kind)?;

    let logical_id = resolve_logical_transfer_id(&parent);
    // 旧 failed 父行仍可点 recovery；同 logical 已有**不同** clientOperationId 的活跃 child 才 conflict。
    // 同 op 重放留给 claim Replay 路径，不得在此误杀。
    ensure_no_active_logical_recovery(
        &state,
        &logical_id,
        &parent.id,
        Some(client_operation_id.as_str()),
    )
    .await?;

    let path = Path::new(&parent.file_path);
    if !path.exists() {
        return Err(AppError::not_found(format!(
            "源文件不存在: {}",
            parent.file_path
        )));
    }

    // 在 claim 前计算指纹（阻塞在 spawn_blocking），Queued 行在 claim 后可见。
    let parent_for_fp = parent.clone();
    let path_buf = path.to_path_buf();
    let (file_size, sha256, _fp) =
        tokio::task::spawn_blocking(move || recheck_source_fingerprint(&path_buf, &parent_for_fp))
            .await
            .map_err(|e| AppError::generic(format!("计算源指纹任务失败: {e}")))??;

    // Resume 复用父 protocol；Retry 的 hash 用空 protocol 占位（避免 claim 前随机 UUID 破坏幂等）。
    let resume_protocol_id = match kind {
        TransferRecoveryKind::Retry => None,
        TransferRecoveryKind::Resume => {
            ensure_peer_resume_capability(&state, &parent.peer_device_id).await?;
            Some(if parent.protocol_transfer_id.is_empty() {
                parent.id.clone()
            } else {
                parent.protocol_transfer_id.clone()
            })
        }
    };

    let hash_protocol = resume_protocol_id.as_deref().unwrap_or("");
    let payload_hash = canonical_recovery_payload_hash(
        kind,
        &logical_id,
        &parent.file_path,
        &parent.peer_device_id,
        hash_protocol,
    );

    let attempt_id = Uuid::new_v4().to_string();
    // Retry：wire protocol 与 attempt 绑定，仅 Fresh 赢家写入；Replay 忽略本 claim_task。
    let claim_protocol_id = match kind {
        TransferRecoveryKind::Retry => attempt_id.clone(),
        TransferRecoveryKind::Resume => resume_protocol_id.expect("resume protocol set"),
    };
    let next_attempt = parent.attempt.saturating_add(1).max(2);
    let claim_task = TransferTask {
        id: attempt_id.clone(),
        filename: parent.filename.clone(),
        file_path: parent.file_path.clone(),
        size: file_size,
        sha256: sha256.clone(),
        chunk_size: CHUNK_SIZE as u64,
        direction: TransferDirection::Send,
        peer_device_id: parent.peer_device_id.clone(),
        status: TransferStatus::Pending,
        transferred_bytes: 0,
        created_at: now_iso(),
        completed_at: None,
        phase: Some(TransferPhase::Queued),
        failure: None,
        attempt: next_attempt,
        logical_transfer_id: logical_id.clone(),
        attempt_id: attempt_id.clone(),
        protocol_transfer_id: claim_protocol_id,
        client_operation_id: Some(client_operation_id.clone()),
        operation_payload_hash: Some(payload_hash.clone()),
    };

    let outcome = state
        .transfer_repo
        .claim_sender_operation(&client_operation_id, &payload_hash, &claim_task)
        .await?;

    match outcome {
        SenderClaimOutcome::Fresh(task) => {
            // claim 后 TOCTOU 再检：并发不同 op 可能刚插入同 logical 活跃行。
            // 排除本 claim 行；同 op 自身不视为冲突。
            if let Err(conflict) = ensure_no_active_logical_recovery(
                &state,
                &logical_id,
                &task.id,
                Some(client_operation_id.as_str()),
            )
            .await
            {
                let mut failed = task.clone();
                failed.status = TransferStatus::Failed;
                failed.phase = Some(TransferPhase::Failed);
                failed.failure = Some(TransferFailure {
                    stage: TransferFailureStage::Transfer,
                    code: "logical_recovery_conflict".into(),
                    retryable: true,
                    message: "同 logical_transfer_id 已有活跃 attempt，本次 claim 作废".into(),
                });
                failed.completed_at = Some(now_iso());
                state.transfer_repo.record(&failed).await?;
                return Err(conflict);
            }
            // spawn 前再次重检 size/mtime（TOCTOU）。
            let path = Path::new(&task.file_path);
            let (size2, sha2, _) = recheck_source_fingerprint(path, &task)?;
            if size2 != file_size || sha2 != sha256 {
                let mut failed = task.clone();
                failed.status = TransferStatus::Failed;
                failed.phase = Some(TransferPhase::Failed);
                failed.failure = Some(TransferFailure {
                    stage: TransferFailureStage::Source,
                    code: "source_changed".into(),
                    retryable: true,
                    message: "spawn 前源文件 fingerprint 已变化".into(),
                });
                failed.completed_at = Some(now_iso());
                state.transfer_repo.record(&failed).await?;
                return Err(AppError::conflict("source_changed: 源文件已变化"));
            }
            spawn_claimed_send(state, task.clone(), size2, sha2);
            Ok(task)
        }
        SenderClaimOutcome::Replay(task) => ensure_replayed_attempt_running(state, task).await,
        SenderClaimOutcome::Conflict { .. } => Err(AppError::conflict(
            "operationIdConflict: clientOperationId 已绑定不同 payload",
        )),
    }
}

/// 解析跨 attempt 稳定的 logical transfer 身份。
///
/// Business Logic（为什么需要这个函数）:
///     旧行缺 logical_transfer_id 时必须回落 task.id，保证 recovery 互斥键稳定。
///
/// Code Logic（这个函数做什么）:
///     非空 logical_transfer_id 优先，否则 `task.id`。
fn resolve_logical_transfer_id(task: &TransferTask) -> String {
    if task.logical_transfer_id.trim().is_empty() {
        task.id.clone()
    } else {
        task.logical_transfer_id.clone()
    }
}

/// 拒绝同一 logical 上**不同** clientOperationId 的并发 recovery（registry + history）。
///
/// Business Logic（为什么需要这个函数）:
///     父 failed 行终态仍允许点击；若 child 已 Queued/Transferring 且属于另一 op，
///     必须 conflict，避免第二份 clientOperationId 再 spawn 并发发送。
///     同 op 重放必须放行到 claim Replay，不能在此误杀。
///
/// Code Logic（这个函数做什么）:
///     扫 registry 非终态同 logical（排除 exclude_task_id / 同 allow_client_operation_id）；
///     再查 history pending/transferring；命中返回 conflict `logical_transfer_active`。
async fn ensure_no_active_logical_recovery(
    state: &AppState,
    logical_transfer_id: &str,
    exclude_task_id: &str,
    allow_client_operation_id: Option<&str>,
) -> Result<(), AppError> {
    for active in state.transfers.list() {
        if active.id == exclude_task_id {
            continue;
        }
        if resolve_logical_transfer_id(&active) != logical_transfer_id {
            continue;
        }
        if is_terminal_status(active.status) {
            continue;
        }
        if is_same_client_operation(active.client_operation_id.as_deref(), allow_client_operation_id)
        {
            continue;
        }
        return Err(AppError::conflict(format!(
            "logical_transfer_active: logical_transfer_id={logical_transfer_id} 已有活跃 attempt {}",
            active.id
        )));
    }

    if let Some(history_active) = state
        .transfer_repo
        .find_active_send_for_logical(logical_transfer_id)
        .await?
    {
        if history_active.id != exclude_task_id
            && !is_same_client_operation(
                history_active.client_operation_id.as_deref(),
                allow_client_operation_id,
            )
        {
            return Err(AppError::conflict(format!(
                "logical_transfer_active: logical_transfer_id={logical_transfer_id} 已有活跃 attempt {}",
                history_active.id
            )));
        }
    }
    Ok(())
}

/// 比较两个可选 clientOperationId 是否同指同一用户意图。
///
/// Business Logic（为什么需要这个函数）:
///     recovery 互斥只拦不同 op；同 op Replay 必须识别。
///
/// Code Logic（这个函数做什么）:
///     两侧都是 Some 且字符串相等 → true。
fn is_same_client_operation(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Replay 路径：终态诚实返回；非终态确保 send loop 在跑（必要时 re-spawn）。
///
/// Business Logic（为什么需要这个函数）:
///     同 hash Replay 不得把 Failed 伪装成“已接受的新 attempt”，也不得在 orphan Queued
///     行上只返回 Ok 而不 spawn（进程内超时重放 / insert-before-spawn 未重启场景）。
///
/// Code Logic（这个函数做什么）:
///     terminal → conflict `operation_terminal`；registry 命中 → Ok；否则源指纹通过则 re-spawn。
async fn ensure_replayed_attempt_running(
    state: AppState,
    task: TransferTask,
) -> Result<TransferTask, AppError> {
    if is_terminal_status(task.status) {
        let code = match task.status {
            TransferStatus::Completed => "succeeded",
            TransferStatus::Failed => task
                .failure
                .as_ref()
                .map(|f| f.code.as_str())
                .unwrap_or("failed"),
            TransferStatus::Cancelled => "cancelled",
            TransferStatus::Pending | TransferStatus::Transferring => "unknown",
        };
        return Err(AppError::conflict(format!(
            "operation_terminal: clientOperationId 已有终态 {} ({code})，请使用新的 clientOperationId",
            task.status.as_str()
        )));
    }

    if state.transfers.get(&task.id).is_some() {
        return Ok(task);
    }

    // Orphan Queued/非终态：单飞 re-spawn（与 recover_pending_claimed_operations 同语义）。
    let path = Path::new(&task.file_path);
    if !path.exists() {
        let mut failed = task.clone();
        failed.status = TransferStatus::Failed;
        failed.phase = Some(TransferPhase::Failed);
        failed.failure = Some(TransferFailure {
            stage: TransferFailureStage::Source,
            code: "source_missing".into(),
            retryable: true,
            message: "Replay re-spawn 时源文件不存在".into(),
        });
        failed.completed_at = Some(now_iso());
        state.transfer_repo.record(&failed).await?;
        return Err(AppError::not_found(format!(
            "源文件不存在: {}",
            task.file_path
        )));
    }

    let (size, sha, _) = recheck_source_fingerprint(path, &task)?;
    spawn_claimed_send(state, task.clone(), size, sha);
    Ok(task)
}

/// 加载父任务：registry 优先，否则 history。
///
/// Business Logic: 活跃任务可能尚未落历史；终态任务只在 history。
/// Code Logic: registry.get → transfer_repo.get_by_id。
async fn load_parent_task(state: &AppState, task_id: &str) -> Result<TransferTask, AppError> {
    if let Some(t) = state.transfers.get(task_id) {
        return Ok(t);
    }
    state
        .transfer_repo
        .get_by_id(task_id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("传输任务不存在: {task_id}")))
}

/// 校验父任务是否允许 retry/resume。
///
/// Business Logic: active 只允许 cancel；非 retryable 拒绝；非 Send 拒绝；completed 拒绝。
/// Code Logic: 检查 direction/status/phase/failure.retryable。
fn validate_parent_for_recovery(
    parent: &TransferTask,
    kind: TransferRecoveryKind,
) -> Result<(), AppError> {
    if parent.direction != TransferDirection::Send {
        return Err(AppError::validation(
            "仅发送方向任务支持 retry/resume".to_string(),
        ));
    }
    let phase = parent.effective_phase();
    match phase {
        TransferPhase::Queued
        | TransferPhase::Connecting
        | TransferPhase::Transferring
        | TransferPhase::Finalizing => {
            return Err(AppError::conflict(
                "transfer_active: 活跃阶段不允许 retry/resume，请先取消",
            ));
        }
        TransferPhase::Completed => {
            return Err(AppError::validation(
                "已完成任务不能 retry/resume".to_string(),
            ));
        }
        TransferPhase::Failed | TransferPhase::Cancelled => {}
    }
    // coarse status 兜底（phase 缺失时）
    match parent.status {
        TransferStatus::Pending | TransferStatus::Transferring => {
            return Err(AppError::conflict(
                "transfer_active: 活跃阶段不允许 retry/resume，请先取消",
            ));
        }
        TransferStatus::Completed => {
            return Err(AppError::validation(
                "已完成任务不能 retry/resume".to_string(),
            ));
        }
        TransferStatus::Failed | TransferStatus::Cancelled => {}
    }
    if let Some(failure) = &parent.failure {
        if !failure.retryable {
            return Err(AppError::validation(format!(
                "non_retryable: 失败码 {} 不可重试",
                failure.code
            )));
        }
    }
    if kind == TransferRecoveryKind::Resume && parent.status == TransferStatus::Cancelled {
        // cancelled 默认走显式 retry，不自动 resume。
        return Err(AppError::validation(
            "cancelled 任务请使用 retry 重新传输".to_string(),
        ));
    }
    Ok(())
}

/// 探测对端是否宣告 transfer.resume.v1。
///
/// Business Logic: 旧 peer 不得显示假续传。
/// Code Logic: devices 取 host:port → health_info → supports(CAPABILITY_TRANSFER_RESUME_V1)。
async fn ensure_peer_resume_capability(
    state: &AppState,
    peer_device_id: &str,
) -> Result<(), AppError> {
    let peer_addr: Option<(String, u16)> = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        devices
            .get(peer_device_id)
            .map(|d| (d.host.clone(), d.port))
    };
    let (host, port) = peer_addr
        .ok_or_else(|| AppError::not_found(format!("对端设备不存在或离线: {peer_device_id}")))?;
    let base_url = format!("http://{host}:{port}");
    let health = state
        .peer_client
        .health_info(&base_url)
        .await
        .map_err(|e| AppError::unavailable(format!("无法探测对端 resume 能力: {e}")))?;
    if !health
        .protocol_info()
        .supports(CAPABILITY_TRANSFER_RESUME_V1)
    {
        return Err(AppError::validation(
            "unsupported: 对端不支持 transfer.resume.v1，请使用 retry".to_string(),
        ));
    }
    Ok(())
}

/// 重取 size/mtime 并校验 SHA 与任务记录一致。
///
/// Business Logic: resume 在源文件变化时必须拒绝，避免错误 finalize。
/// Code Logic: metadata size/mtime_ns + calculate_sha256；与 task.size/sha256 比较。
fn recheck_source_fingerprint(
    path: &Path,
    task: &TransferTask,
) -> Result<(u64, String, SourceFingerprint), AppError> {
    let meta = std::fs::metadata(path)?;
    let size = meta.len();
    let mtime_ns = meta.modified().ok().and_then(|t| {
        t.duration_since(SystemTime::UNIX_EPOCH).ok().and_then(|d| {
            let secs = d.as_secs();
            let nanos = d.subsec_nanos() as u64;
            secs.checked_mul(1_000_000_000)?.checked_add(nanos)
        })
    });
    let sha256 = calculate_sha256(path)?;
    if !task.sha256.is_empty() && task.sha256 != sha256 {
        return Err(AppError::conflict(format!(
            "source_changed: SHA 不匹配（期望 {}）",
            task.sha256
        )));
    }
    if task.size > 0 && task.size != size {
        return Err(AppError::conflict(format!(
            "source_changed: 大小不匹配（期望 {} 实际 {size}）",
            task.size
        )));
    }
    let fp = SourceFingerprint {
        size,
        mtime_ns,
        sha256: sha256.clone(),
    };
    Ok((size, sha256, fp))
}

/// 已 claim 的 attempt：registry.add + spawn run_send_loop。
///
/// Business Logic: Fresh winner 与 Replay orphan re-spawn 共用；调用方保证不在 registry 中。
/// Code Logic: add → cancel_token → tokio::spawn run_send_loop（protocol id = task.protocol_transfer_id）。
fn spawn_claimed_send(state: AppState, task: TransferTask, file_size: u64, sha256: String) {
    let transfer_id = task.id.clone();
    let protocol_transfer_id = if task.protocol_transfer_id.is_empty() {
        task.id.clone()
    } else {
        task.protocol_transfer_id.clone()
    };
    let device_id = task.peer_device_id.clone();
    let file_path = task.file_path.clone();
    state.transfers.add(task);
    let cancel_token = state
        .transfers
        .cancel_token(&transfer_id)
        .unwrap_or_default();
    let registry = (*state.transfers).clone();
    tokio::spawn(async move {
        run_send_loop(
            state,
            registry,
            protocol_transfer_id,
            transfer_id,
            device_id,
            file_path,
            file_size,
            sha256,
            cancel_token,
        )
        .await;
    });
}

/// 实际发送循环（spawn 内执行）。
///
/// Business Logic: 逐块读取文件并通过 peer_client 发送到对端；支持断点续传（resume_offset）与取消。
///
/// Code Logic:
///     `protocol_transfer_id` 用于 wire init/chunk/complete；`local_task_id` 用于 registry/history 本地主键。
#[allow(clippy::too_many_arguments)]
async fn run_send_loop(
    state: AppState,
    registry: TransferRegistry,
    protocol_transfer_id: String,
    local_task_id: String,
    device_id: String,
    file_path: String,
    file_size: u64,
    sha256: String,
    cancel_token: CancellationToken,
) {
    // 兼容旧 start_sending：protocol id 与 local id 相同。
    let transfer_id = local_task_id;
    let wire_id = protocol_transfer_id;

    registry.set_phase(&transfer_id, TransferPhase::Connecting);

    // 1) 查 devices 拿对端 host:port（不存在/不在线则失败）
    //    注意：标准 RwLockReadGuard 非 Send，必须在 await 前释放，故先 clone 出地址再处理 None。
    let peer_addr: Option<(String, u16)> = {
        let devices = state.devices.read().expect("devices 读锁中毒");
        devices.get(&device_id).map(|d| (d.host.clone(), d.port))
    };
    let (host, port) = match peer_addr {
        Some(addr) => addr,
        None => {
            fail_transfer(
                &state,
                &registry,
                &transfer_id,
                format!("对端设备不存在或离线: {device_id}"),
                TransferFailureStage::Connect,
                "peer_offline",
                true,
            )
            .await;
            return;
        }
    };
    let base_url = format!("http://{host}:{port}");

    // 2) init 握手：发送元数据，拿 resume_offset（断点续传）
    let init_meta = serde_json::json!({
        "transfer_id": wire_id,
        "filename": Path::new(&file_path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "size": file_size,
        "sha256": sha256,
        "chunk_size": CHUNK_SIZE,
    });

    let resume_offset = match state.peer_client.transfer_init(&base_url, init_meta).await {
        Ok(resp) => {
            // 对端拒绝
            let accepted = resp
                .get("accepted")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !accepted {
                let err = resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("对端拒绝接收文件")
                    .to_string();
                fail_transfer(
                    &state,
                    &registry,
                    &transfer_id,
                    err,
                    TransferFailureStage::Connect,
                    "peer_rejected",
                    true,
                )
                .await;
                return;
            }
            resp.get("resume_offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        }
        Err(e) => {
            fail_transfer(
                &state,
                &registry,
                &transfer_id,
                format!("连接对端失败: {e}"),
                TransferFailureStage::Connect,
                "peer_unreachable",
                true,
            )
            .await;
            return;
        }
    };

    // resume_offset 大于文件 size 属于协议/对端异常，不得空转后假报完成。
    if resume_offset > file_size {
        fail_transfer(
            &state,
            &registry,
            &transfer_id,
            format!("对端 resume_offset={resume_offset} 超过本机文件大小 {file_size}"),
            TransferFailureStage::Protocol,
            "invalid_resume_offset",
            true,
        )
        .await;
        return;
    }

    // 标记 transferring，emit 首个进度（含 resume_offset）
    registry.update_progress(&transfer_id, resume_offset, TransferStatus::Transferring);
    registry.set_phase(&transfer_id, TransferPhase::Transferring);
    emit_progress(&state, &transfer_id, resume_offset, file_size);

    // 3) 分块发送：wire 用 protocol_transfer_id，本地进度用 local task id。
    let file_result = send_file_chunks(
        &state,
        &registry,
        &base_url,
        &wire_id,
        &transfer_id,
        &file_path,
        file_size,
        resume_offset,
        &cancel_token,
    )
    .await;

    match file_result {
        Ok(()) => {
            if cancel_token.is_cancelled() {
                let completed_at = now_iso();
                registry.mark_cancelled(&transfer_id, completed_at.clone());
                if let Some(t) = registry.get(&transfer_id) {
                    let _ = state.transfer_repo.record(&t).await;
                }
                registry.remove(&transfer_id);
                state.emit_event(
                    "transfer:cancelled",
                    StatusPayload {
                        id: transfer_id,
                        status: "cancelled".to_string(),
                        error_message: None,
                    },
                );
                return;
            }

            registry.set_phase(&transfer_id, TransferPhase::Finalizing);
            // Finalizing 中间态刷入 ledger，便于 timeout 后 get_transfer_operation 命中 Pending。
            if let Some(t) = registry.get(&transfer_id) {
                let _ = state.transfer_repo.record(&t).await;
            }
            match finalize_send_with_peer(&state, &base_url, &wire_id, file_size, resume_offset)
                .await
            {
                FinalizeOutcome::Succeeded => {
                    if let Some(t) = registry.get(&transfer_id) {
                        let _ = commit_sender_completed_outcome(&state, &t).await;
                    } else {
                        // registry 已空时仍尽量按历史行提交（防御）。
                        tracing::warn!("finalize 成功但 registry 已无任务: {transfer_id}");
                    }
                }
                FinalizeOutcome::DefinitiveFailure { message, code } => {
                    fail_transfer(
                        &state,
                        &registry,
                        &transfer_id,
                        message,
                        TransferFailureStage::Finalize,
                        &code,
                        true,
                    )
                    .await;
                }
                FinalizeOutcome::Uncertain { message } => {
                    // lost-ACK / peer pending：保持 Finalizing pending，不提供 retry。
                    park_uncertain_finalize(&state, &registry, &transfer_id, &message).await;
                }
            }
        }
        Err(SendError::Cancelled) => {
            let completed_at = now_iso();
            registry.mark_cancelled(&transfer_id, completed_at.clone());
            if let Some(t) = registry.get(&transfer_id) {
                let _ = state.transfer_repo.record(&t).await;
            }
            registry.remove(&transfer_id);
            state.emit_event(
                "transfer:cancelled",
                StatusPayload {
                    id: transfer_id,
                    status: "cancelled".to_string(),
                    error_message: None,
                },
            );
        }
        Err(SendError::Failed(msg)) => {
            fail_transfer(
                &state,
                &registry,
                &transfer_id,
                msg,
                TransferFailureStage::Transfer,
                "chunk_failed",
                true,
            )
            .await;
        }
    }
}

/// complete/finalize 握手结果（区分 definitive 失败与 uncertain）。
///
/// Business Logic（为什么需要这个枚举）:
///     lost ACK / transport timeout 后不得把 operation 标 Failed 并提供 retry；
///     必须保持 pending 供 get_transfer_operation 对账。
///
/// Code Logic（这个枚举做什么）:
///     Succeeded / DefinitiveFailure / Uncertain 三态驱动发送循环终态写入。
#[derive(Debug)]
enum FinalizeOutcome {
    /// 对端已确认成功（含 status 收敛）
    Succeeded,
    /// 明确业务失败，可写 Failed + retryable
    DefinitiveFailure { message: String, code: String },
    /// 网络/timeout/对端仍 pending：保持 Finalizing pending
    Uncertain { message: String },
}

/// Business Logic（为什么需要这个函数）:
///     发送端在分块结束后必须确认对端已落地，但 complete 路由是新能力；对旧对端无条件
///     调用会产生 404 假失败与重试重复副本。需要按 health 权威 capability 门控。
///     lost final ACK 时 transfer_complete 内 status_fallback 收敛为 Succeeded；
///     仍不可达则 Uncertain。
///
/// Code Logic（这个函数做什么）:
///     1. health_info 探测对端是否 supports(`transfer.complete.v1`)；
///     2. 有能力 → transfer_complete（有界重试 + status 收敛）→ Succeeded / Uncertain / Definitive；
///     3. 无能力且 file_size>0 且 resume_offset < file_size（确实发过 chunk）→ Succeeded（legacy）；
///     4. 无能力且 size=0 或 full-tmp → DefinitiveFailure(unsupported)；
///     5. health 在需要 complete 时失败 → Uncertain（不得假成功/假失败）。
async fn finalize_send_with_peer(
    state: &AppState,
    base_url: &str,
    transfer_id: &str,
    file_size: u64,
    resume_offset: u64,
) -> FinalizeOutcome {
    let supports_complete = match state.peer_client.health_info(base_url).await {
        Ok(health) => health
            .protocol_info()
            .supports(CAPABILITY_TRANSFER_COMPLETE_V1),
        Err(e) => {
            // health 失败时：若本应走 complete（size=0 / full-tmp），无法确认 → uncertain；
            // 普通非空且发过块时，仍可按 legacy chunk 路径成功。
            if file_size == 0 || resume_offset >= file_size {
                return FinalizeOutcome::Uncertain {
                    message: format!(
                        "无法探测对端 transfer.complete.v1 能力且本次需要 complete 握手: {e}"
                    ),
                };
            }
            tracing::warn!("探测 transfer.complete.v1 失败，按 legacy 最后一块路径收敛: {e}");
            false
        }
    };

    if supports_complete {
        match state
            .peer_client
            .transfer_complete(base_url, transfer_id)
            .await
        {
            Ok(true) => FinalizeOutcome::Succeeded,
            Ok(false) => FinalizeOutcome::DefinitiveFailure {
                message: "对端 finalize 未成功（complete 握手返回 success=false）".to_string(),
                code: "finalize_rejected".to_string(),
            },
            Err(e) => classify_complete_error(e),
        }
    } else {
        // legacy：只有“确实发送过至少一块”的非空传输才可依赖 chunk finalize。
        if file_size > 0 && resume_offset < file_size {
            FinalizeOutcome::Succeeded
        } else {
            FinalizeOutcome::DefinitiveFailure {
                message: "对端不支持 transfer.complete.v1，无法完成空文件或 full-tmp 续传 finalize"
                    .to_string(),
                code: "complete_unsupported".to_string(),
            }
        }
    }
}

/// Business Logic: complete 错误分 uncertain（可对账）与 definitive。
/// Code Logic: Network/Timeout/retryable Remote → Uncertain；其余 DefinitiveFailure。
fn classify_complete_error(error: PeerCallError) -> FinalizeOutcome {
    let message = format_complete_error_ref(&error);
    match error {
        PeerCallError::Network { .. } => FinalizeOutcome::Uncertain { message },
        PeerCallError::Remote {
            code, retryable, ..
        } if retryable || code == "timeout" || code == "unavailable" => {
            FinalizeOutcome::Uncertain { message }
        }
        PeerCallError::Unsupported { .. } => FinalizeOutcome::DefinitiveFailure {
            message,
            code: "complete_unsupported".to_string(),
        },
        PeerCallError::InvalidResponse { .. } => FinalizeOutcome::Uncertain {
            // 响应损坏也可能已在对端提交；保守 uncertain，交给 status 对账。
            message,
        },
        PeerCallError::Remote { code, .. } => FinalizeOutcome::DefinitiveFailure { message, code },
    }
}

/// Business Logic（为什么需要这个函数）:
///     complete 失败需把结构化 PeerCallError 折叠为任务 errorMessage，保留 code/status。
///
/// Code Logic（这个函数做什么）:
///     按 PeerCallError 变体生成可读中文错误串（只读借用，便于分类后 move error）。
fn format_complete_error_ref(error: &PeerCallError) -> String {
    match error {
        PeerCallError::Remote { status, code, .. } => {
            format!("对端 complete 握手失败: HTTP {status} [{code}]")
        }
        PeerCallError::Network { source, .. } => {
            format!("对端 complete 握手网络失败: {source}")
        }
        PeerCallError::Unsupported { capability, .. } => {
            format!("对端不支持能力 {capability}")
        }
        PeerCallError::InvalidResponse { reason, .. } => {
            format!("对端 complete 响应无法解析: {reason}")
        }
    }
}

/// 将发送任务停在 Finalizing uncertain，供后续 get_transfer_operation 对账。
///
/// Business Logic（为什么需要这个函数）:
///     lost ACK / peer pending 时不得 Failed+retry；必须保留 pending 真值。
///
/// Code Logic（这个函数做什么）:
///     set_phase(Finalizing) + status 保持 Transferring → record history → 不 remove、不 emit failed。
async fn park_uncertain_finalize(
    state: &AppState,
    registry: &TransferRegistry,
    transfer_id: &str,
    message: &str,
) {
    tracing::warn!("transfer finalize uncertain，保持 pending 等待对账: {transfer_id}: {message}");
    registry.set_phase(transfer_id, TransferPhase::Finalizing);
    // 确保 coarse status 不是 Failed
    if let Some(mut t) = registry.get(transfer_id) {
        t.status = TransferStatus::Transferring;
        t.phase = Some(TransferPhase::Finalizing);
        t.failure = None;
        t.completed_at = None;
        // registry 无全量 replace；用 update_progress 保 status + 再 set_phase
        registry.update_progress(
            transfer_id,
            t.transferred_bytes,
            TransferStatus::Transferring,
        );
        registry.set_phase(transfer_id, TransferPhase::Finalizing);
        if let Some(snap) = registry.get(transfer_id) {
            let _ = state.transfer_repo.record(&snap).await;
        }
    }
}

/// 发送过程中的错误分类：取消 / 失败。
enum SendError {
    Cancelled,
    Failed(String),
}

/// 分块读取并发送。对照 Python `send_file` 的分块循环。
///
/// Code Logic:
///     1. 以 resume_offset seek 文件；
///     2. 循环读 min(CHUNK_SIZE, remaining) 字节；
///     3. 每块前检查 cancel_token，已取消返回 SendError::Cancelled；
///     4. peer_client.transfer_chunk 发送（内置 Network/retryable 5xx 有界重试 +
///        最后一块响应丢失时 status=completed 收敛）；
///     5. 更新 progress + 节流 emit（每块都 emit，与 Python 一致）。
#[allow(clippy::too_many_arguments)]
async fn send_file_chunks(
    state: &AppState,
    registry: &TransferRegistry,
    base_url: &str,
    wire_transfer_id: &str,
    local_task_id: &str,
    file_path: &str,
    file_size: u64,
    resume_offset: u64,
    cancel_token: &CancellationToken,
) -> Result<(), SendError> {
    let mut file = match tokio::fs::File::open(file_path).await {
        Ok(f) => f,
        Err(e) => return Err(SendError::Failed(format!("打开文件失败: {e}"))),
    };

    // seek 到断点续传 offset
    if resume_offset > 0 {
        if let Err(e) = file.seek(std::io::SeekFrom::Start(resume_offset)).await {
            return Err(SendError::Failed(format!("文件 seek 失败: {e}")));
        }
    }

    let mut offset = resume_offset;
    let mut buf = vec![0u8; CHUNK_SIZE];

    while offset < file_size {
        // 取消检查（每块前）
        if cancel_token.is_cancelled() {
            return Err(SendError::Cancelled);
        }

        let remaining = file_size - offset;
        let read_size = std::cmp::min(CHUNK_SIZE as u64, remaining) as usize;
        let n = match file.read(&mut buf[..read_size]).await {
            Ok(n) => n,
            Err(e) => return Err(SendError::Failed(format!("读取文件失败: {e}"))),
        };
        if n == 0 {
            break;
        }
        let chunk_data = buf[..n].to_vec();

        // 发送分块：wire 层用 protocol_transfer_id；本地进度用 local task id。
        match state
            .peer_client
            .transfer_chunk(base_url, wire_transfer_id, offset, chunk_data)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return Err(SendError::Failed("对端写入数据块失败".to_string()));
            }
            Err(e) => {
                return Err(SendError::Failed(format!("发送数据块失败: {e}")));
            }
        }

        offset += n as u64;
        registry.update_progress(local_task_id, offset, TransferStatus::Transferring);
        emit_progress(state, local_task_id, offset, file_size);

        // 让出调度，避免阻塞（对照 Python `await asyncio.sleep(0)`）
        tokio::task::yield_now().await;
    }

    Ok(())
}

/// emit 一次进度事件。
///
/// Business Logic（为什么需要这个函数）:
///     GUI 和独立后端都需要共享同一套传输进度事件出口，避免 HTTP 接收路径依赖 GUI 句柄。
///
/// Code Logic（这个函数做什么）:
///     计算进度比例并通过 AppState 后端 UI adapter 发布 `transfer:progress`。
fn emit_progress(state: &AppState, id: &str, transferred: u64, size: u64) {
    let progress = if size == 0 {
        0.0
    } else {
        transferred as f64 / size as f64
    };
    state.emit_event(
        "transfer:progress",
        ProgressPayload {
            id: id.to_string(),
            transferred_bytes: transferred,
            size,
            progress,
        },
    );
}

/// 统一失败处理：结构化 failure + 写历史 + remove + emit failed。
///
/// Business Logic: retry 依赖 failure.retryable；默认网络/传输失败可重试。
/// Code Logic: mark_failed_with → record → remove → emit。
async fn fail_transfer(
    state: &AppState,
    registry: &TransferRegistry,
    transfer_id: &str,
    error_msg: String,
    stage: TransferFailureStage,
    code: &str,
    retryable: bool,
) {
    let completed_at = now_iso();
    registry.mark_failed_with(
        transfer_id,
        completed_at.clone(),
        TransferFailure {
            stage,
            code: code.to_string(),
            retryable,
            message: error_msg.clone(),
        },
    );
    if let Some(t) = registry.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    registry.remove(transfer_id);
    state.emit_event(
        "transfer:failed",
        StatusPayload {
            id: transfer_id.to_string(),
            status: "failed".to_string(),
            error_message: Some(error_msg),
        },
    );
}

#[cfg(test)]
mod tests {
    use crate::net::protocol::{
        PeerProtocolInfo, CAPABILITY_TRANSFER_COMPLETE_V1, PROTOCOL_VERSION_V1,
    };

    /// Business Logic（为什么需要这个测试）:
    ///     能力探测结果必须正确区分“可 complete”与“legacy 仅 chunk”。
    #[test]
    fn transfer_complete_capability_token_is_stable() {
        assert_eq!(CAPABILITY_TRANSFER_COMPLETE_V1, "transfer.complete.v1");
        let with_cap = PeerProtocolInfo {
            protocol_version: PROTOCOL_VERSION_V1,
            capabilities: vec![CAPABILITY_TRANSFER_COMPLETE_V1.to_string()],
        };
        assert!(with_cap.supports(CAPABILITY_TRANSFER_COMPLETE_V1));
        let legacy = PeerProtocolInfo {
            protocol_version: 0,
            capabilities: vec![],
        };
        assert!(!legacy.supports(CAPABILITY_TRANSFER_COMPLETE_V1));
        let v1_without = PeerProtocolInfo {
            protocol_version: PROTOCOL_VERSION_V1,
            capabilities: vec!["errors.envelope.v1".to_string()],
        };
        assert!(!v1_without.supports(CAPABILITY_TRANSFER_COMPLETE_V1));
    }

    /// Business Logic（为什么需要这个测试）:
    ///     legacy 判定：只有非空且确实发送过块才允许不走 complete。
    fn legacy_chunks_sent(file_size: u64, resume_offset: u64) -> bool {
        file_size > 0 && resume_offset < file_size
    }

    #[test]
    fn legacy_finalize_allowed_only_when_chunks_were_sent() {
        assert!(legacy_chunks_sent(10, 0));
        assert!(!legacy_chunks_sent(0, 0));
        assert!(!legacy_chunks_sent(100, 100));
        assert!(legacy_chunks_sent(100, 50));
    }

    use super::*;
    use crate::models::transfer::{
        canonical_recovery_payload_hash, TransferDirection, TransferFailure, TransferFailureStage,
        TransferPhase, TransferRecoveryKind, TransferStatus, TransferTask,
    };
    use crate::net::protocol::CAPABILITY_TRANSFER_RESUME_V1;
    use crate::storage::transfer_repo::{SenderClaimOutcome, TransferRepo};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn memory_repo() -> TransferRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        TransferRepo::ensure_schema(&pool).await.unwrap();
        TransferRepo::new(pool)
    }

    fn failed_parent(id: &str, retryable: bool) -> TransferTask {
        TransferTask {
            filename: "f.bin".into(),
            file_path: "/tmp/f.bin".into(),
            size: 10,
            sha256: "deadbeef".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer-1".into(),
            status: TransferStatus::Failed,
            transferred_bytes: 4,
            created_at: "2026-07-14T00:00:00Z".into(),
            completed_at: Some("2026-07-14T00:01:00Z".into()),
            phase: Some(TransferPhase::Failed),
            failure: Some(TransferFailure {
                stage: TransferFailureStage::Transfer,
                code: if retryable {
                    "chunk_failed".into()
                } else {
                    "fatal".into()
                },
                retryable,
                message: "x".into(),
            }),
            attempt: 1,
            logical_transfer_id: id.into(),
            attempt_id: id.into(),
            protocol_transfer_id: id.into(),
            ..TransferTask::recovery_defaults(id)
        }
    }

    /// 非 retryable 失败拒绝 recovery。
    #[test]
    fn non_retryable_rejects() {
        let p = failed_parent("t1", false);
        let err = validate_parent_for_recovery(&p, TransferRecoveryKind::Retry).unwrap_err();
        assert!(
            err.to_string().contains("non_retryable")
                || format!("{err:?}").contains("non_retryable")
                || err.to_string().contains("不可重试")
        );
    }

    /// active phase 拒绝。
    #[test]
    fn active_phase_rejects() {
        let mut p = failed_parent("t2", true);
        p.status = TransferStatus::Transferring;
        p.phase = Some(TransferPhase::Transferring);
        let err = validate_parent_for_recovery(&p, TransferRecoveryKind::Resume).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("transfer_active") || s.contains("活跃"));
    }

    /// logical 身份回落与 clientOperation 同指判定。
    #[test]
    fn logical_identity_and_same_op_helpers() {
        let mut p = failed_parent("parent-id", true);
        p.logical_transfer_id = String::new();
        assert_eq!(resolve_logical_transfer_id(&p), "parent-id");
        p.logical_transfer_id = "logical-z".into();
        assert_eq!(resolve_logical_transfer_id(&p), "logical-z");
        assert!(is_same_client_operation(Some("op-1"), Some("op-1")));
        assert!(!is_same_client_operation(Some("op-1"), Some("op-2")));
        assert!(!is_same_client_operation(Some("op-1"), None));
    }

    /// history 中同 logical 活跃 child 可被 repo 查到（recovery 互斥依赖）。
    #[tokio::test]
    async fn history_active_logical_blocks_new_recovery_lookup() {
        let repo = memory_repo().await;
        let parent = failed_parent("parent-logical", true);
        repo.record(&parent).await.unwrap();
        let child = TransferTask {
            filename: "f.bin".into(),
            file_path: "/tmp/f.bin".into(),
            size: 10,
            sha256: "deadbeef".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer-1".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T00:02:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "parent-logical".into(),
            attempt_id: "child-1".into(),
            protocol_transfer_id: "proto-child".into(),
            client_operation_id: Some("op-child".into()),
            operation_payload_hash: Some("hash-child".into()),
            ..TransferTask::recovery_defaults("child-1")
        };
        repo.record(&child).await.unwrap();
        let active = repo
            .find_active_send_for_logical("parent-logical")
            .await
            .unwrap()
            .expect("child active");
        assert_eq!(active.id, "child-1");
        // 同 op 视为可 Replay；不同 op 应被视为并发冲突源
        assert!(is_same_client_operation(
            active.client_operation_id.as_deref(),
            Some("op-child")
        ));
        assert!(!is_same_client_operation(
            active.client_operation_id.as_deref(),
            Some("op-other")
        ));
    }

    /// cancelled 不允许 resume（应走 retry）。
    #[test]
    fn cancelled_rejects_resume() {
        let mut p = failed_parent("t3", true);
        p.status = TransferStatus::Cancelled;
        p.phase = Some(TransferPhase::Cancelled);
        p.failure = None;
        let err = validate_parent_for_recovery(&p, TransferRecoveryKind::Resume).unwrap_err();
        assert!(err.to_string().contains("retry") || err.to_string().contains("cancelled"));
    }

    /// source fingerprint mismatch 拒绝。
    #[test]
    fn source_changed_rejects_fingerprint() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("src.bin");
        std::fs::write(&path, b"hello-world").unwrap();
        let mut task = failed_parent("t4", true);
        task.file_path = path.to_string_lossy().to_string();
        task.size = 11;
        task.sha256 = "00".into(); // wrong
        let err = recheck_source_fingerprint(&path, &task).unwrap_err();
        assert!(
            err.to_string().contains("source_changed")
                || format!("{err:?}").contains("source_changed")
        );
    }

    /// 旧 peer 能力 token 稳定且 supports 语义正确。
    #[test]
    fn resume_capability_token_and_old_peer() {
        assert_eq!(CAPABILITY_TRANSFER_RESUME_V1, "transfer.resume.v1");
        let legacy = PeerProtocolInfo {
            protocol_version: PROTOCOL_VERSION_V1,
            capabilities: vec![CAPABILITY_TRANSFER_COMPLETE_V1.to_string()],
        };
        assert!(!legacy.supports(CAPABILITY_TRANSFER_RESUME_V1));
        let modern = PeerProtocolInfo {
            protocol_version: PROTOCOL_VERSION_V1,
            capabilities: vec![
                CAPABILITY_TRANSFER_COMPLETE_V1.to_string(),
                CAPABILITY_TRANSFER_RESUME_V1.to_string(),
            ],
        };
        assert!(modern.supports(CAPABILITY_TRANSFER_RESUME_V1));
    }

    /// Retry payload hash 对随机 protocol 占位必须稳定（空串），同 op 不会 Conflict。
    #[test]
    fn retry_payload_hash_ignores_random_protocol_placeholder() {
        let a = canonical_recovery_payload_hash(
            TransferRecoveryKind::Retry,
            "logical-1",
            "/tmp/a",
            "peer",
            "",
        );
        let b = canonical_recovery_payload_hash(
            TransferRecoveryKind::Retry,
            "logical-1",
            "/tmp/a",
            "peer",
            "",
        );
        assert_eq!(a, b);
        // 若误把随机 protocol 折进 hash 会破坏幂等——显式断言与非空不同。
        let polluted = canonical_recovery_payload_hash(
            TransferRecoveryKind::Retry,
            "logical-1",
            "/tmp/a",
            "peer",
            "uuid-random-1",
        );
        assert_ne!(a, polluted);
    }

    /// 同 id 不同 kind（retry vs resume）payload hash 不同 → Conflict 路径。
    #[tokio::test]
    async fn same_id_different_kind_payload_conflicts() {
        let repo = memory_repo().await;
        // Retry hash 使用空 protocol 占位（生产路径一致）
        let retry_hash = canonical_recovery_payload_hash(
            TransferRecoveryKind::Retry,
            "logical-1",
            "/tmp/a",
            "peer",
            "",
        );
        let resume_hash = canonical_recovery_payload_hash(
            TransferRecoveryKind::Resume,
            "logical-1",
            "/tmp/a",
            "peer",
            "proto-stable",
        );
        assert_ne!(retry_hash, resume_hash);
        let task = TransferTask {
            filename: "a".into(),
            file_path: "/tmp/a".into(),
            size: 1,
            sha256: "x".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T00:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "logical-1".into(),
            attempt_id: "a1".into(),
            protocol_transfer_id: "proto-new".into(),
            ..TransferTask::recovery_defaults("a1")
        };
        repo.claim_sender_operation("op-mixed", &retry_hash, &task)
            .await
            .unwrap();
        let conflict = repo
            .claim_sender_operation("op-mixed", &resume_hash, &task)
            .await
            .unwrap();
        assert!(matches!(conflict, SenderClaimOutcome::Conflict { .. }));
    }

    /// 并发同 op claim 只创建一个 attempt 行（attempt 计数语义）。
    #[tokio::test]
    async fn duplicate_resume_request_creates_one_attempt() {
        let repo = Arc::new(memory_repo().await);
        // 父任务 attempt=1 已存在
        let parent = failed_parent("parent-1", true);
        repo.record(&parent).await.unwrap();

        let hash = canonical_recovery_payload_hash(
            TransferRecoveryKind::Resume,
            "parent-1",
            "/tmp/f.bin",
            "peer-1",
            "parent-1",
        );
        let make_task = |id: &str, hash: &str| TransferTask {
            filename: "f.bin".into(),
            file_path: "/tmp/f.bin".into(),
            size: 10,
            sha256: "deadbeef".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer-1".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T05:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "parent-1".into(),
            attempt_id: id.into(),
            protocol_transfer_id: "parent-1".into(),
            client_operation_id: Some("op-1".into()),
            operation_payload_hash: Some(hash.to_string()),
            ..TransferTask::recovery_defaults(id)
        };
        let r1 = repo.clone();
        let r2 = repo.clone();
        let hash1 = hash.clone();
        let hash2 = hash.clone();
        let (a, b) = tokio::join!(
            async move {
                r1.claim_sender_operation("op-1", &hash1, &make_task("attempt-a", &hash1))
                    .await
                    .unwrap()
            },
            async move {
                r2.claim_sender_operation("op-1", &hash2, &make_task("attempt-b", &hash2))
                    .await
                    .unwrap()
            },
        );
        let id_a = match a {
            SenderClaimOutcome::Fresh(t) | SenderClaimOutcome::Replay(t) => t.id,
            SenderClaimOutcome::Conflict { existing } => existing.id,
        };
        let id_b = match b {
            SenderClaimOutcome::Fresh(t) | SenderClaimOutcome::Replay(t) => t.id,
            SenderClaimOutcome::Conflict { existing } => existing.id,
        };
        assert_eq!(id_a, id_b);
        // parent + one claim attempt
        assert_eq!(
            repo.count_attempts_for_logical("parent-1").await.unwrap(),
            2
        );
    }

    /// insert-before-spawn：Queued 行可被 list_recoverable 发现。
    #[tokio::test]
    async fn insert_before_spawn_rows_are_recoverable() {
        let repo = memory_repo().await;
        let task = TransferTask {
            filename: "r.bin".into(),
            file_path: "/tmp/r.bin".into(),
            size: 1,
            sha256: "aa".into(),
            direction: TransferDirection::Send,
            peer_device_id: "peer".into(),
            status: TransferStatus::Pending,
            created_at: "2026-07-14T06:00:00Z".into(),
            phase: Some(TransferPhase::Queued),
            attempt: 2,
            logical_transfer_id: "L".into(),
            attempt_id: "A".into(),
            protocol_transfer_id: "P".into(),
            client_operation_id: Some("op-rec".into()),
            operation_payload_hash: Some("h".into()),
            ..TransferTask::recovery_defaults("A")
        };
        repo.claim_sender_operation("op-rec", "h", &task)
            .await
            .unwrap();
        let list = repo.list_recoverable_queued_sends().await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "A");
    }
}
