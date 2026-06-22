//! transfer/sender.rs — 文件发送端
//!
//! Business Logic（为什么需要这个模块）:
//!     用户在传输面板选择文件与目标设备后，需将文件分块发送到对端。发送端封装：
//!     计算 SHA256 → init 握手拿 resume_offset → 从 offset 逐块读取发送 → 完成校验 →
//!     emit 进度/完成/失败/取消事件 → 写 transfer_history。对照 Python `transfer/sender.py`。
//!
//! Code Logic（这个模块做什么）:
//!     - `start_sending(state, app_handle, device_id, file_path)`：在调用方线程内 spawn 异步任务，
//!       立即返回 transfer_id（命令层 send_transfer 用）。
//!     - spawn 内：查 devices 拿对端 host:port → 算 SHA256 → registry.add(task) → emit pending →
//!       transfer_init 拿 resume_offset → 循环分块读 + transfer_chunk（body=bytes，header X-Chunk-Offset）→
//!       每块前检查 cancel_token → 更新 progress + 节流 emit → 全部发完 emit completed + 写历史。
//!     - 任何异常：mark_failed + emit failed。
//!     - 取消：mark_cancelled + emit cancelled。
//!
//! 协议等价：init/chunk JSON 字段、X-Chunk-Offset header、960KB chunk_size、resume_offset 语义
//!     全部与 Python 一致（迁移期 Rust↔Python 互通）。

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use crate::transfer::registry::TransferRegistry;
use crate::transfer::CHUNK_SIZE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::Path;
use tauri::{AppHandle, Emitter};
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
///     3. registry.add(task)；spawn 异步任务（持有 app_handle clone 与 cancel_token clone）；
///     4. 任务内：init → 分块发送循环（检查 cancel）→ emit completed + 写历史 / emit failed。
pub fn start_sending(
    state: AppState,
    app_handle: AppHandle,
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
    tokio::spawn(async move {
        run_send_loop(
            state,
            app_handle,
            registry,
            transfer_id.clone(),
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

/// 实际发送循环（spawn 内执行）。
///
/// Business Logic: 逐块读取文件并通过 peer_client 发送到对端；支持断点续传（resume_offset）与取消。
#[allow(clippy::too_many_arguments)]
async fn run_send_loop(
    state: AppState,
    app_handle: AppHandle,
    registry: TransferRegistry,
    transfer_id: String,
    device_id: String,
    file_path: String,
    file_size: u64,
    sha256: String,
    cancel_token: CancellationToken,
) {
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
                &app_handle,
                &transfer_id,
                format!("对端设备不存在或离线: {device_id}"),
            )
            .await;
            return;
        }
    };
    let base_url = format!("http://{host}:{port}");

    // 2) init 握手：发送元数据，拿 resume_offset（断点续传）
    let init_meta = serde_json::json!({
        "transfer_id": transfer_id,
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
                fail_transfer(&state, &registry, &app_handle, &transfer_id, err).await;
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
                &app_handle,
                &transfer_id,
                format!("连接对端失败: {e}"),
            )
            .await;
            return;
        }
    };

    // 标记 transferring，emit 首个进度（含 resume_offset）
    registry.update_progress(&transfer_id, resume_offset, TransferStatus::Transferring);
    emit_progress(&app_handle, &transfer_id, resume_offset, file_size);

    // 3) 分块发送循环
    let file_result = send_file_chunks(
        &state,
        &app_handle,
        &registry,
        &base_url,
        &transfer_id,
        &file_path,
        file_size,
        resume_offset,
        &cancel_token,
    )
    .await;

    match file_result {
        Ok(()) => {
            // 4) 全部发完：对端在收齐最后一块时会自动 finalize（校验 SHA256）。
            //    发送端标记本地任务 completed 并写历史。
            let completed_at = now_iso();
            registry.mark_completed(&transfer_id, completed_at.clone(), None);
            let task = registry.get(&transfer_id);
            if let Some(t) = task {
                let _ = state.transfer_repo.record(&t).await;
            }
            registry.remove(&transfer_id);
            let _ = app_handle.emit(
                "transfer:completed",
                StatusPayload {
                    id: transfer_id,
                    status: "completed".to_string(),
                    error_message: None,
                },
            );
        }
        Err(SendError::Cancelled) => {
            let completed_at = now_iso();
            registry.mark_cancelled(&transfer_id, completed_at.clone());
            if let Some(t) = registry.get(&transfer_id) {
                let _ = state.transfer_repo.record(&t).await;
            }
            registry.remove(&transfer_id);
            let _ = app_handle.emit(
                "transfer:cancelled",
                StatusPayload {
                    id: transfer_id,
                    status: "cancelled".to_string(),
                    error_message: None,
                },
            );
        }
        Err(SendError::Failed(msg)) => {
            fail_transfer(&state, &registry, &app_handle, &transfer_id, msg).await;
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
///     4. peer_client.transfer_chunk 发送（header X-Chunk-Offset，body=bytes）；
///     5. 更新 progress + 节流 emit（每块都 emit，与 Python 一致）。
#[allow(clippy::too_many_arguments)]
async fn send_file_chunks(
    state: &AppState,
    app_handle: &AppHandle,
    registry: &TransferRegistry,
    base_url: &str,
    transfer_id: &str,
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

        // 发送分块（X-Chunk-Offset header 由 peer_client.transfer_chunk 设置）
        match state
            .peer_client
            .transfer_chunk(base_url, transfer_id, offset, chunk_data)
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
        registry.update_progress(transfer_id, offset, TransferStatus::Transferring);
        emit_progress(app_handle, transfer_id, offset, file_size);

        // 让出调度，避免阻塞（对照 Python `await asyncio.sleep(0)`）
        tokio::task::yield_now().await;
    }

    Ok(())
}

/// emit 一次进度事件。
fn emit_progress(app_handle: &AppHandle, id: &str, transferred: u64, size: u64) {
    let progress = if size == 0 {
        0.0
    } else {
        transferred as f64 / size as f64
    };
    let _ = app_handle.emit(
        "transfer:progress",
        ProgressPayload {
            id: id.to_string(),
            transferred_bytes: transferred,
            size,
            progress,
        },
    );
}

/// 统一失败处理：mark_failed + 写历史 + remove + emit failed。
async fn fail_transfer(
    state: &AppState,
    registry: &TransferRegistry,
    app_handle: &AppHandle,
    transfer_id: &str,
    error_msg: String,
) {
    let completed_at = now_iso();
    registry.mark_failed(transfer_id, completed_at.clone());
    if let Some(t) = registry.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    registry.remove(transfer_id);
    let _ = app_handle.emit(
        "transfer:failed",
        StatusPayload {
            id: transfer_id.to_string(),
            status: "failed".to_string(),
            error_message: Some(error_msg),
        },
    );
}
