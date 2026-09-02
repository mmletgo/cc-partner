//! net/routes/mobile_transfer.rs — 移动端主机中转文件传输
//!
//! Business Logic（为什么需要这个模块）:
//!     手机浏览器不是 mDNS/P2P 节点，不能走桌面 `send_transfer(filePath)`。
//!     `/mobile` 把文件分块上传到主机 staging，主机再对本机 `receive_dir` 落盘，
//!     或对局域网对端调用 `sender::start_sending`。任务列表就是主机上的传输任务。
//!     已完成 Receive 提供流式 Download，JSON **永不**返回主机 path；不走 loopback control。
//!
//! Code Logic（这个模块做什么）:
//!     - `GET /api/mobile/devices`：合成 self + 对端
//!     - `GET /api/mobile/transfer/tasks`：`list_transfers_for_state` 后剥离 path
//!     - `POST /api/mobile/transfer/upload/{init,chunk/:id,complete/:id}`：staging 续传
//!     - `POST /api/mobile/transfer/{cancel,retry,resume,get-operation}`：复用 registry/sender
//!     - `GET /api/mobile/transfer/download/:taskId`：Receive+completed 或发给手机的
//!       Send+completed 流式下载

use crate::commands::devices::{get_local_device_for_state, list_devices_for_state};
use crate::commands::transfer::list_transfers_for_state;
use crate::error::AppError;
use crate::models::device::DeviceDto;
use crate::models::transfer::{
    TransferDirection, TransferFailure, TransferPhase, TransferStatus, TransferTask,
    TransferTaskDto,
};
use crate::net::error_response::{P2pError, P2pResult};
use crate::net::request_context::P2pRequestContext;
use crate::state::AppState;
use crate::transfer::receiver::{
    ensure_path_within_dir, resolve_filename, sanitize_receive_basename,
};
use crate::transfer::sender;
use crate::transfer::CHUNK_SIZE;
use axum::body::{Body, Bytes};
use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::Utc;
use futures_util::stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

/// staging 根目录名（位于 data_dir 下，不是 receive_dir）。
const STAGING_DIR_NAME: &str = "mobile-transfer-uploads";
/// 未完成/失败 staging 超过此时长后在访问路径上 GC。
const UPLOAD_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// 已交给 `start_sending` 的 staging 最长保留（发送仍在读该路径）。
const HANDOFF_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// staging payload 文件名（固定单组件，避免用户 filename 进入路径）。
const PAYLOAD_NAME: &str = "payload";
const META_NAME: &str = "meta.json";
const MAX_FILENAME_BYTES: usize = 255;

/// 进程内 per-upload 锁，避免同 id 并发 chunk/complete 交错写。
fn upload_locks() -> &'static Mutex<HashMap<String, Weak<AsyncMutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取某 staging id 的单飞锁。
///
/// Business Logic: 同 id 的 chunk/complete 必须串行，否则 offset 续传会互相覆盖。
/// Code Logic: Weak 登记，无强引用后可回收，避免随机 id 探测撑爆 map。
fn upload_lock(id: &str) -> std::sync::Arc<AsyncMutex<()>> {
    let mut map = match upload_locks().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.retain(|_, weak| weak.strong_count() > 0);
    if let Some(existing) = map.get(id).and_then(|weak| weak.upgrade()) {
        return existing;
    }
    let lock = std::sync::Arc::new(AsyncMutex::new(()));
    map.insert(id.to_string(), std::sync::Arc::downgrade(&lock));
    lock
}

/// 移动端任务 DTO：与桌面 `TransferTaskDto` 对齐但**不含 path**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileTransferTaskDto {
    pub id: String,
    pub file_name: String,
    pub file_size: u64,
    pub direction: TransferDirection,
    pub status: TransferStatus,
    pub progress: f64,
    pub peer_device_id: Option<String>,
    pub peer_device_name: Option<String>,
    pub speed: Option<f64>,
    pub error_message: Option<String>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub transferred_bytes: u64,
    pub phase: Option<TransferPhase>,
    pub failure: Option<TransferFailure>,
    pub attempt: u32,
    pub logical_transfer_id: String,
    pub attempt_id: String,
    pub protocol_transfer_id: String,
    pub client_operation_id: Option<String>,
    pub operation_payload_hash: Option<String>,
}

impl MobileTransferTaskDto {
    /// 从桌面 DTO 剥离 `filePath`。
    ///
    /// Business Logic: mobile/LAN JSON 不得泄露主机绝对路径。
    /// Code Logic: 逐字段拷贝，故意不映射 `file_path`。
    fn from_desktop(dto: TransferTaskDto) -> Self {
        Self {
            id: dto.id,
            file_name: dto.file_name,
            file_size: dto.file_size,
            direction: dto.direction,
            status: dto.status,
            progress: dto.progress,
            peer_device_id: dto.peer_device_id,
            peer_device_name: dto.peer_device_name,
            speed: dto.speed,
            error_message: dto.error_message,
            started_at: dto.started_at,
            completed_at: dto.completed_at,
            transferred_bytes: dto.transferred_bytes,
            phase: dto.phase,
            failure: dto.failure,
            attempt: dto.attempt,
            logical_transfer_id: dto.logical_transfer_id,
            attempt_id: dto.attempt_id,
            protocol_transfer_id: dto.protocol_transfer_id,
            client_operation_id: dto.client_operation_id,
            operation_payload_hash: dto.operation_payload_hash,
        }
    }

    fn from_task(task: &TransferTask) -> Self {
        Self::from_desktop(task.to_dto(None))
    }
}

/// `POST /api/mobile/transfer/upload/init` 请求体。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadInitRequest {
    pub filename: String,
    pub size: u64,
    pub device_id: String,
    pub client_operation_id: String,
}

/// init 响应：staging id + 已收字节（续传）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadInitResponse {
    pub id: String,
    pub received_bytes: u64,
}

/// chunk 响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadChunkResponse {
    pub success: bool,
    pub received_bytes: u64,
}

/// cancel 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileTransferCancelRequest {
    pub task_id: String,
}

/// retry/resume 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileTransferRecoveryRequest {
    pub task_id: String,
    pub client_operation_id: String,
}

/// get-operation 请求。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileTransferGetOperationRequest {
    pub client_operation_id: String,
}

/// 磁盘上的 staging 元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadMeta {
    id: String,
    filename: String,
    size: u64,
    device_id: String,
    client_operation_id: String,
    received_bytes: u64,
    created_at: String,
    status: UploadMetaStatus,
    task_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum UploadMetaStatus {
    Uploading,
    Completed,
    Failed,
    HandedOff,
}

/// GET /api/mobile/devices：合成「这台电脑」置顶 + 对端。
///
/// Business Logic（为什么需要这个函数）:
///     手机可选本机或局域网对端；本机项 `isSelf=true`，用户已确认 LAN 风险披露。
///
/// Code Logic（这个函数做什么）:
///     `get_local_device_for_state` 置顶，再追加 `list_devices_for_state`（去掉本机 id）。
pub async fn list_mobile_devices(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<Vec<DeviceDto>>> {
    list_mobile_devices_for_state(&state)
        .map(Json)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.devices"))
}

/// GET /api/mobile/transfer/tasks：活跃+历史，JSON 不含 path。
///
/// Business Logic（为什么需要这个函数）:
///     手机任务列表就是主机传输任务；不得把 receive_dir/源路径交给浏览器。
///
/// Code Logic（这个函数做什么）:
///     `list_transfers_for_state` → `MobileTransferTaskDto`。
pub async fn list_mobile_transfer_tasks(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
) -> P2pResult<Json<Vec<MobileTransferTaskDto>>> {
    let desktop = list_transfers_for_state(&state)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.tasks"))?;
    Ok(Json(
        desktop
            .into_iter()
            .map(MobileTransferTaskDto::from_desktop)
            .collect(),
    ))
}

/// POST /api/mobile/transfer/upload/init：创建或回放 staging。
///
/// Business Logic（为什么需要这个函数）:
///     刷新/丢 ACK 后同一 `clientOperationId` 必须回到同一 staging，才能按 offset 续传。
///
/// Code Logic（这个函数做什么）:
///     校验 filename/size/deviceId/clientOperationId → GC → 同 key 同 payload 回放，
///     不同 payload 409 → 否则新建 `mobile-transfer-uploads/<id>`。
pub async fn upload_init(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<UploadInitRequest>,
) -> P2pResult<Json<UploadInitResponse>> {
    upload_init_for_state(&state, body)
        .await
        .map(Json)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.upload.init"))
}

/// POST /api/mobile/transfer/upload/chunk/:id：原始字节 + `X-Chunk-Offset`。
///
/// Business Logic（为什么需要这个函数）:
///     浏览器按 ~960KB 切片上传；单块超限必须在落盘前拒绝。
///
/// Code Logic（这个函数做什么）:
///     解析 offset（缺省 0）→ 拒 `CHUNK_SIZE+1` 与 gap → 写入 payload → 更新 receivedBytes。
pub async fn upload_chunk(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> P2pResult<Json<UploadChunkResponse>> {
    let offset: u64 = headers
        .get("X-Chunk-Offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    upload_chunk_for_state(&state, &id, offset, body.as_ref())
        .await
        .map(Json)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.upload.chunk"))
}

/// POST /api/mobile/transfer/upload/complete/:id：主机 SHA-256 后 self-place 或 start_sending。
///
/// Business Logic（为什么需要这个函数）:
///     收齐后由主机决定落本机还是发给对端；禁止 HTTP 调 loopback control。
///
/// Code Logic（这个函数做什么）:
///     校验 size/hash → 本机 `receive_dir`+`resolve_filename` 记 Receive completed；
///     对端 `sender::start_sending(staging_path)`；终态 meta 回放同一任务 DTO。
pub async fn upload_complete(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(id): Path<String>,
) -> P2pResult<Json<MobileTransferTaskDto>> {
    upload_complete_for_state(&state, &id)
        .await
        .map(Json)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.upload.complete"))
}

/// POST /api/mobile/transfer/cancel：复用 registry。
pub async fn cancel_mobile_transfer(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<MobileTransferCancelRequest>,
) -> P2pResult<Json<serde_json::Value>> {
    let task_id = body.task_id.trim();
    if task_id.is_empty() {
        return Err(P2pError::from_app_error(
            AppError::validation("taskId 不能为空"),
            &ctx,
            "mobile.transfer.cancel",
        ));
    }
    let ok = state.transfers.cancel(task_id);
    if !ok {
        return Err(P2pError::from_app_error(
            AppError::not_found(format!("传输任务不存在: {task_id}")),
            &ctx,
            "mobile.transfer.cancel",
        ));
    }
    Ok(Json(serde_json::json!({ "ok": true, "id": task_id })))
}

/// POST /api/mobile/transfer/retry：复用 `sender::retry_transfer`，响应剥离 path。
pub async fn retry_mobile_transfer(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<MobileTransferRecoveryRequest>,
) -> P2pResult<Json<MobileTransferTaskDto>> {
    let task = sender::retry_transfer(state, body.task_id, body.client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.retry"))?;
    Ok(Json(MobileTransferTaskDto::from_task(&task)))
}

/// POST /api/mobile/transfer/resume：复用 `sender::resume_transfer`，响应剥离 path。
pub async fn resume_mobile_transfer(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<MobileTransferRecoveryRequest>,
) -> P2pResult<Json<MobileTransferTaskDto>> {
    let task = sender::resume_transfer(state, body.task_id, body.client_operation_id)
        .await
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.resume"))?;
    Ok(Json(MobileTransferTaskDto::from_task(&task)))
}

/// POST /api/mobile/transfer/get-operation：只读对账。
pub async fn get_mobile_transfer_operation(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Json(body): Json<MobileTransferGetOperationRequest>,
) -> P2pResult<Json<crate::models::transfer::TransferOperationStatus>> {
    sender::get_transfer_operation(&state, &body.client_operation_id)
        .await
        .map(Json)
        .map_err(|e| P2pError::from_app_error(e, &ctx, "mobile.transfer.get_operation"))
}

/// GET /api/mobile/transfer/download/:taskId：completed Receive 或手机邮箱 offer。
///
/// Business Logic（为什么需要这个函数）:
///     手机不能 Open/Reveal；只能下载已接收完成的文件，或电脑发给手机且已完成的原文件，
///     JSON/错误不得带 path。
///
/// Code Logic（这个函数做什么）:
///     registry/history 查找 → 非允许资格或文件不可读 → 404 泛化文案；
///     成功则 `octet-stream` + `Content-Disposition` 分块流。
pub async fn download_mobile_transfer(
    State(state): State<AppState>,
    Extension(ctx): Extension<P2pRequestContext>,
    Path(task_id): Path<String>,
) -> Result<Response, P2pError> {
    match open_completed_download(&state, &task_id).await {
        Ok((filename, file)) => Ok(stream_download_response(&filename, file)),
        Err(e) => Err(P2pError::from_app_error(
            e,
            &ctx,
            "mobile.transfer.download",
        )),
    }
}

/// 组装 mobile 设备列表（self 置顶）。
fn list_mobile_devices_for_state(state: &AppState) -> Result<Vec<DeviceDto>, AppError> {
    let local = get_local_device_for_state(state);
    let mut peers = list_devices_for_state(state)?;
    peers.retain(|d| d.id != local.id);
    let mut out = Vec::with_capacity(peers.len() + 1);
    out.push(local);
    out.append(&mut peers);
    Ok(out)
}

/// init 实现。
async fn upload_init_for_state(
    state: &AppState,
    body: UploadInitRequest,
) -> Result<UploadInitResponse, AppError> {
    let client_operation_id = body.client_operation_id.trim();
    if client_operation_id.is_empty() {
        return Err(AppError::validation("clientOperationId 不能为空"));
    }
    let device_id = body.device_id.trim();
    if device_id.is_empty() {
        return Err(AppError::validation("deviceId 不能为空"));
    }
    resolve_upload_target(state, device_id)?;
    let filename = sanitize_upload_filename(&body.filename)?;
    let root = staging_root(state)?;
    gc_staging(&root);

    if let Some(existing) = find_meta_by_client_op(&root, client_operation_id)? {
        let same = existing.filename == filename
            && existing.size == body.size
            && existing.device_id == device_id;
        if !same {
            return Err(AppError::conflict(
                "operationIdConflict: clientOperationId 已绑定不同 payload",
            ));
        }
        return Ok(UploadInitResponse {
            id: existing.id,
            received_bytes: existing.received_bytes,
        });
    }

    let id = Uuid::new_v4().to_string();
    let dir = session_dir(&root, &id)?;
    std::fs::create_dir_all(&dir)?;
    let payload = dir.join(PAYLOAD_NAME);
    std::fs::File::create(&payload)?;
    let meta = UploadMeta {
        id: id.clone(),
        filename,
        size: body.size,
        device_id: device_id.to_string(),
        client_operation_id: client_operation_id.to_string(),
        received_bytes: 0,
        created_at: now_iso(),
        status: UploadMetaStatus::Uploading,
        task_id: None,
    };
    save_meta(&dir, &meta)?;
    Ok(UploadInitResponse {
        id,
        received_bytes: 0,
    })
}

/// chunk 实现。
async fn upload_chunk_for_state(
    state: &AppState,
    id: &str,
    offset: u64,
    data: &[u8],
) -> Result<UploadChunkResponse, AppError> {
    if data.len() > CHUNK_SIZE {
        return Err(AppError::validation(format!(
            "分块超过上限 {CHUNK_SIZE} 字节"
        )));
    }
    let id = sanitize_session_id(id)?;
    let lock = upload_lock(&id);
    let _guard = lock.lock().await;
    let root = staging_root(state)?;
    let dir = session_dir(&root, &id)?;
    let mut meta = load_meta(&dir)?;
    if meta.status != UploadMetaStatus::Uploading {
        return Err(AppError::conflict("upload 已结束，不能继续写入分块"));
    }
    if offset > meta.received_bytes {
        return Err(AppError::validation(format!(
            "X-Chunk-Offset 存在缺口：offset={offset} receivedBytes={}",
            meta.received_bytes
        )));
    }
    let end = offset
        .checked_add(data.len() as u64)
        .ok_or_else(|| AppError::validation("chunk offset 溢出"))?;
    if end > meta.size {
        return Err(AppError::validation("chunk 超出声明 size"));
    }

    let payload = dir.join(PAYLOAD_NAME);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&payload)?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    file.flush()?;

    if end > meta.received_bytes {
        meta.received_bytes = end;
    }
    save_meta(&dir, &meta)?;
    Ok(UploadChunkResponse {
        success: true,
        received_bytes: meta.received_bytes,
    })
}

/// complete 实现：self-place 或 start_sending。
async fn upload_complete_for_state(
    state: &AppState,
    id: &str,
) -> Result<MobileTransferTaskDto, AppError> {
    let id = sanitize_session_id(id)?;
    let lock = upload_lock(&id);
    let _guard = lock.lock().await;
    let root = staging_root(state)?;
    let dir = session_dir(&root, &id)?;
    let mut meta = load_meta(&dir)?;

    if let Some(task_id) = meta.task_id.as_deref() {
        if matches!(
            meta.status,
            UploadMetaStatus::Completed | UploadMetaStatus::HandedOff
        ) {
            return load_task_dto(state, task_id).await;
        }
    }
    if meta.status != UploadMetaStatus::Uploading {
        return Err(AppError::conflict("upload 已结束"));
    }
    if meta.received_bytes != meta.size {
        return Err(AppError::validation(format!(
            "尚未收齐：receivedBytes={} size={}",
            meta.received_bytes, meta.size
        )));
    }

    let payload = dir.join(PAYLOAD_NAME);
    let actual = std::fs::metadata(&payload)?.len();
    if actual != meta.size {
        mark_failed(&dir, &mut meta)?;
        return Err(AppError::validation("staging 文件大小与声明不符"));
    }
    let sha256 = sha256_file(&payload)?;
    let target = resolve_upload_target(state, &meta.device_id)?;

    match target {
        UploadTarget::SelfHost => {
            let placed = place_self_receive(state, &payload, &meta, &sha256).await?;
            meta.status = UploadMetaStatus::Completed;
            meta.task_id = Some(placed.id.clone());
            save_meta(&dir, &meta)?;
            if let Err(e) = std::fs::remove_file(&payload) {
                tracing::warn!("self-place 后删除 staging payload 失败: {e}");
            }
            Ok(MobileTransferTaskDto::from_task(&placed))
        }
        UploadTarget::Peer => {
            let transfer_id = sender::start_sending(
                state.clone(),
                meta.device_id.clone(),
                payload.to_string_lossy().to_string(),
                meta.client_operation_id.clone(),
            )
            .await
            .inspect_err(|_| {
                let _ = mark_failed(&dir, &mut meta);
            })?;
            meta.status = UploadMetaStatus::HandedOff;
            meta.task_id = Some(transfer_id.clone());
            save_meta(&dir, &meta)?;
            load_task_dto(state, &transfer_id).await
        }
    }
}

/// 本机落盘并写入 Receive completed 历史。
///
/// Business Logic: 手机选「这台电脑」时文件应出现在主机接收目录，与 P2P 接收完成一致。
/// Code Logic: 持 `receive_dir` 锁 → `resolve_filename` → no-replace place → `transfer_repo.record`。
async fn place_self_receive(
    state: &AppState,
    staging_payload: &FsPath,
    meta: &UploadMeta,
    sha256: &str,
) -> Result<TransferTask, AppError> {
    let receive_dir = receive_dir_of(state)?;
    std::fs::create_dir_all(&receive_dir)?;
    let _lock = state.transfers.receive_dir_lock();
    let _guard = _lock.lock().await;

    let dest = place_staging_exclusive(&receive_dir, &meta.filename, staging_payload)?;
    let now = now_iso();
    let task_id = meta.id.clone();
    let task = TransferTask {
        id: task_id.clone(),
        filename: meta.filename.clone(),
        file_path: dest.to_string_lossy().to_string(),
        size: meta.size,
        sha256: sha256.to_string(),
        chunk_size: CHUNK_SIZE as u64,
        direction: TransferDirection::Receive,
        peer_device_id: state.device_id.as_ref().clone(),
        status: TransferStatus::Completed,
        transferred_bytes: meta.size,
        created_at: meta.created_at.clone(),
        completed_at: Some(now),
        phase: Some(TransferPhase::Completed),
        failure: None,
        attempt: 1,
        logical_transfer_id: task_id.clone(),
        attempt_id: task_id.clone(),
        protocol_transfer_id: task_id.clone(),
        client_operation_id: Some(meta.client_operation_id.clone()),
        operation_payload_hash: None,
    };
    state.transfer_repo.record(&task).await?;
    Ok(task)
}

/// exclusive place：hard_link 优先，跨卷回退 create_new+copy。
fn place_staging_exclusive(
    receive_dir: &FsPath,
    safe_filename: &str,
    staging_payload: &FsPath,
) -> Result<PathBuf, AppError> {
    for _ in 0..10_000 {
        let final_filename = resolve_filename(receive_dir, safe_filename);
        let _ = sanitize_receive_basename(&final_filename, "final_filename")?;
        let final_path = receive_dir.join(&final_filename);
        ensure_path_within_dir(receive_dir, &final_path)?;
        match std::fs::hard_link(staging_payload, &final_path) {
            Ok(()) => return Ok(final_path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&final_path)
            {
                Ok(mut dest) => {
                    let mut src = std::fs::File::open(staging_payload)?;
                    std::io::copy(&mut src, &mut dest)?;
                    dest.flush()?;
                    return Ok(final_path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(AppError::generic(format!("self-place 落盘失败: {e}")));
                }
            },
        }
    }
    Err(AppError::generic("无法分配不冲突的最终文件名（重试耗尽）"))
}

/// 仅 completed Receive 可下载；失败一律泛化 404，不带 path。
/// 是否允许 `/mobile` 下载该任务。
///
/// Business Logic（为什么需要这个函数）:
///     已接收文件和电脑发给手机的 offer 可下；发给其它电脑的 Send 即使 path 仍在也禁止。
///
/// Code Logic（这个函数做什么）:
///     Receive+completed，或 Send+completed 且 peer 为 mobile inbox。
fn is_mobile_downloadable(task: &TransferTask) -> bool {
    if task.status != TransferStatus::Completed {
        return false;
    }
    match task.direction {
        TransferDirection::Receive => true,
        TransferDirection::Send => crate::transfer::is_mobile_inbox_device_id(&task.peer_device_id),
    }
}

async fn open_completed_download(
    state: &AppState,
    task_id: &str,
) -> Result<(String, tokio::fs::File), AppError> {
    let unavailable = || AppError::not_found("下载不可用：仅已完成的接收文件可下载");
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return Err(unavailable());
    }
    let task = if let Some(active) = state.transfers.get(task_id) {
        active
    } else {
        state
            .transfer_repo
            .get_by_id(task_id)
            .await?
            .ok_or_else(unavailable)?
    };
    if !is_mobile_downloadable(&task) {
        return Err(unavailable());
    }
    let path = FsPath::new(task.file_path.trim());
    if !path.is_absolute() {
        return Err(unavailable());
    }
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(unavailable());
        }
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(unavailable());
    }
    let inbox_offer = task.direction == TransferDirection::Send
        && crate::transfer::is_mobile_inbox_device_id(&task.peer_device_id);
    if inbox_offer && meta.len() != task.size {
        return Err(unavailable());
    }
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| unavailable())?;
    Ok((task.filename, file))
}

/// 流式 octet-stream 响应。
fn stream_download_response(filename: &str, file: tokio::fs::File) -> Response {
    let stream = stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 64 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    });
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Ok(value) = HeaderValue::from_str(&content_disposition_attachment(filename)) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// RFC 5987 `Content-Disposition`（ASCII fallback + UTF-8 filename*）。
fn content_disposition_attachment(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = filename
        .bytes()
        .flat_map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') {
                vec![b as char]
            } else {
                format!("%{b:02X}").chars().collect()
            }
        })
        .collect();
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UploadTarget {
    SelfHost,
    Peer,
}

/// 目标必须是本机或当前 devices 表中的对端。
fn resolve_upload_target(state: &AppState, device_id: &str) -> Result<UploadTarget, AppError> {
    if device_id == state.device_id.as_str() {
        return Ok(UploadTarget::SelfHost);
    }
    let devices = state
        .devices
        .read()
        .map_err(|_| AppError::generic("devices 读锁中毒"))?;
    if devices.contains_key(device_id) {
        return Ok(UploadTarget::Peer);
    }
    Err(AppError::validation(
        "deviceId 不是本机，也不在当前对端列表中",
    ))
}

fn sanitize_upload_filename(raw: &str) -> Result<String, AppError> {
    let name = sanitize_receive_basename(raw, "filename")?;
    if name.len() > MAX_FILENAME_BYTES {
        return Err(AppError::validation("filename 过长"));
    }
    Ok(name)
}

fn sanitize_session_id(raw: &str) -> Result<String, AppError> {
    sanitize_receive_basename(raw, "id")
}

/// staging 根：`{db_path 父目录}/mobile-transfer-uploads`（生产即 data_dir）。
fn staging_root(state: &AppState) -> Result<PathBuf, AppError> {
    let db_path = state
        .config
        .read()
        .map_err(|_| AppError::generic("config 读锁中毒"))?
        .db_path
        .clone();
    let parent = FsPath::new(&db_path)
        .parent()
        .ok_or_else(|| AppError::generic("无法从 db_path 解析 data_dir（缺少父目录）"))?;
    Ok(parent.join(STAGING_DIR_NAME))
}

fn receive_dir_of(state: &AppState) -> Result<PathBuf, AppError> {
    let dir = state
        .config
        .read()
        .map_err(|_| AppError::generic("config 读锁中毒"))?
        .receive_dir
        .clone();
    Ok(PathBuf::from(dir))
}

fn session_dir(root: &FsPath, id: &str) -> Result<PathBuf, AppError> {
    let dir = root.join(id);
    ensure_path_within_dir(root, &dir.join(PAYLOAD_NAME))?;
    Ok(dir)
}

fn meta_path(dir: &FsPath) -> PathBuf {
    dir.join(META_NAME)
}

fn save_meta(dir: &FsPath, meta: &UploadMeta) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(meta)?;
    std::fs::write(meta_path(dir), bytes)?;
    Ok(())
}

fn load_meta(dir: &FsPath) -> Result<UploadMeta, AppError> {
    let bytes = std::fs::read(meta_path(dir)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::not_found("upload 不存在或已清理")
        } else {
            AppError::from(e)
        }
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn mark_failed(dir: &FsPath, meta: &mut UploadMeta) -> Result<(), AppError> {
    meta.status = UploadMetaStatus::Failed;
    save_meta(dir, meta)?;
    let payload = dir.join(PAYLOAD_NAME);
    if payload.exists() {
        let _ = std::fs::remove_file(payload);
    }
    Ok(())
}

fn find_meta_by_client_op(
    root: &FsPath,
    client_operation_id: &str,
) -> Result<Option<UploadMeta>, AppError> {
    if !root.exists() {
        return Ok(None);
    }
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_file = entry.path().join(META_NAME);
        if !meta_file.exists() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&meta_file) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<UploadMeta>(&bytes) else {
            continue;
        };
        if meta.client_operation_id == client_operation_id {
            return Ok(Some(meta));
        }
    }
    Ok(None)
}

/// 访问路径上清理过期 staging。
fn gc_staging(root: &FsPath) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let meta_file = path.join(META_NAME);
        let Ok(bytes) = std::fs::read(&meta_file) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<UploadMeta>(&bytes) else {
            continue;
        };
        let Ok(created) = chrono::DateTime::parse_from_rfc3339(&meta.created_at) else {
            continue;
        };
        let created =
            SystemTime::UNIX_EPOCH + Duration::from_secs(created.timestamp().max(0) as u64);
        let Ok(age) = now.duration_since(created) else {
            continue;
        };
        let expired = match meta.status {
            UploadMetaStatus::Uploading | UploadMetaStatus::Failed => age > UPLOAD_TTL,
            UploadMetaStatus::Completed => age > UPLOAD_TTL,
            UploadMetaStatus::HandedOff => age > HANDOFF_TTL,
        };
        if expired {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

async fn load_task_dto(state: &AppState, task_id: &str) -> Result<MobileTransferTaskDto, AppError> {
    if let Some(active) = state.transfers.get(task_id) {
        return Ok(MobileTransferTaskDto::from_task(&active));
    }
    let task = state
        .transfer_repo
        .get_by_id(task_id)
        .await?
        .ok_or_else(|| AppError::not_found("传输任务不存在"))?;
    Ok(MobileTransferTaskDto::from_task(&task))
}

fn sha256_file(path: &FsPath) -> Result<String, AppError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::authority::RuntimeRole;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::models::device::Device;
    use crate::models::transfer::{
        TransferDirection, TransferFailure, TransferFailureStage, TransferPhase, TransferStatus,
        TransferTask,
    };
    use crate::net::peer_client::PeerClient;
    use crate::net::request_context::P2pRequestContext;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
        WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo, WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use axum::routing::post;
    use axum::Router;
    use chrono::Utc;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};
    use tower::ServiceExt;

    fn ctx() -> P2pRequestContext {
        P2pRequestContext {
            request_id: "req-mobile-transfer-test".to_string(),
        }
    }

    fn unique_temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cc-partner-mobile-transfer-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 构造带 transfer_history 的最小 owner AppState（隔离 data/receive 目录）。
    async fn build_test_state(root: &FsPath) -> AppState {
        let receive_dir = root.join("receive");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&receive_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        TransferRepo::ensure_schema(&pool).await.unwrap();

        let config = AppConfig {
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 0,
            receive_dir: receive_dir.to_string_lossy().to_string(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: data_dir.join("data.db").to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_optimizer_provider: "claude".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
            experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
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
            attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("device-test".to_string()),
            devices: Arc::new(RwLock::new(HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(root.join("dist"))),
            update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_workspace_layout_repo: Arc::new(
                crate::storage::WorkbenchWorkspaceLayoutRepo::new(pool.clone()),
            ),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::env::temp_dir().join("cc-partner-bv-mobile-tf"),
                    "test-owner".into(),
                )
                .expect("browser verification test service"),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
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
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(HashMap::new())),
            workbench_claude_session_watchers: Arc::new(Mutex::new(HashMap::new())),
            workbench_claude_session_index_inflight: Arc::new(AsyncMutex::new(HashMap::new())),
            workbench_claude_session_index_dispose_epochs: Arc::new(Mutex::new(HashMap::new())),
            runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
                "mobile-transfer-test-owner",
            )),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    fn json_has_path_key(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.keys().any(|k| {
                    let lower = k.to_ascii_lowercase();
                    lower == "path" || lower == "filepath"
                }) || map.values().any(json_has_path_key)
            }
            serde_json::Value::Array(items) => items.iter().any(json_has_path_key),
            _ => false,
        }
    }

    async fn record_history(
        state: &AppState,
        id: &str,
        direction: TransferDirection,
        status: TransferStatus,
        file_path: &str,
    ) {
        record_history_with_peer(state, id, direction, status, file_path, "peer", 4).await;
    }

    async fn record_history_with_peer(
        state: &AppState,
        id: &str,
        direction: TransferDirection,
        status: TransferStatus,
        file_path: &str,
        peer_device_id: &str,
        size: u64,
    ) {
        let task = TransferTask {
            filename: "payload.bin".into(),
            file_path: file_path.into(),
            size,
            sha256: "abcd".into(),
            direction,
            peer_device_id: peer_device_id.into(),
            status,
            transferred_bytes: size,
            created_at: "2026-07-14T10:00:00Z".into(),
            completed_at: if status == TransferStatus::Completed {
                Some("2026-07-14T10:01:00Z".into())
            } else {
                None
            },
            phase: Some(TransferPhase::from_status(status)),
            ..TransferTask::recovery_defaults(id)
        };
        state.transfer_repo.record(&task).await.unwrap();
    }

    /// init/chunk/complete 对本机落盘，并写入 Receive completed。
    #[tokio::test]
    async fn init_chunk_complete_self_places_into_receive_dir() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let op = format!("op-self-{}", Uuid::new_v4());
        let init = upload_init(
            State(state.clone()),
            Extension(ctx()),
            Json(UploadInitRequest {
                filename: "hello.txt".into(),
                size: 5,
                device_id: "device-test".into(),
                client_operation_id: op,
            }),
        )
        .await
        .expect("init")
        .0;

        let mut headers = HeaderMap::new();
        headers.insert("X-Chunk-Offset", "0".parse().unwrap());
        let _ = upload_chunk(
            State(state.clone()),
            Extension(ctx()),
            Path(init.id.clone()),
            headers,
            Bytes::from_static(b"hello"),
        )
        .await
        .expect("chunk");

        let dto = upload_complete(
            State(state.clone()),
            Extension(ctx()),
            Path(init.id.clone()),
        )
        .await
        .expect("complete")
        .0;

        assert_eq!(dto.direction, TransferDirection::Receive);
        assert_eq!(dto.status, TransferStatus::Completed);
        assert_eq!(dto.file_name, "hello.txt");
        let json = serde_json::to_value(&dto).unwrap();
        assert!(
            !json_has_path_key(&json),
            "complete DTO 不得含 path: {json}"
        );

        let placed = root.join("receive").join("hello.txt");
        assert_eq!(std::fs::read(&placed).unwrap(), b"hello");
        let hist = state.transfer_repo.get_by_id(&dto.id).await.unwrap();
        assert!(hist.is_some());
        assert_eq!(hist.unwrap().direction, TransferDirection::Receive);
    }

    /// peer complete 必须调用 start_sending（registry 出现 Send 任务）。
    #[tokio::test]
    async fn complete_peer_invokes_start_sending() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        {
            let mut devices = state.devices.write().unwrap();
            devices.insert(
                "peer-1".into(),
                Device {
                    id: "peer-1".into(),
                    name: "peer".into(),
                    host: "192.0.2.10".into(),
                    port: 62116,
                    last_seen: Utc::now(),
                    online: true,
                    proto_version: 1,
                    capabilities: vec![],
                },
            );
        }
        let op = format!("op-peer-{}", Uuid::new_v4());
        let init = upload_init(
            State(state.clone()),
            Extension(ctx()),
            Json(UploadInitRequest {
                filename: "send.bin".into(),
                size: 3,
                device_id: "peer-1".into(),
                client_operation_id: op,
            }),
        )
        .await
        .expect("init")
        .0;
        let mut headers = HeaderMap::new();
        headers.insert("X-Chunk-Offset", "0".parse().unwrap());
        let _ = upload_chunk(
            State(state.clone()),
            Extension(ctx()),
            Path(init.id.clone()),
            headers,
            Bytes::from_static(b"abc"),
        )
        .await
        .expect("chunk");

        let dto = upload_complete(State(state.clone()), Extension(ctx()), Path(init.id))
            .await
            .expect("complete")
            .0;
        assert_eq!(dto.direction, TransferDirection::Send);
        assert_eq!(dto.peer_device_id.as_deref(), Some("peer-1"));
        assert!(
            state.transfers.get(&dto.id).is_some(),
            "start_sending 应把 Send 任务写入 registry"
        );
        let json = serde_json::to_value(&dto).unwrap();
        assert!(!json_has_path_key(&json), "peer complete DTO 不得含 path");
    }

    /// download 只允许 completed Receive；其它状态 404 且错误不含 path。
    #[tokio::test]
    async fn download_only_allows_completed_receive() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let file = root.join("receive").join("ok.bin");
        std::fs::write(&file, b"DATA").unwrap();
        record_history(
            &state,
            "recv-ok",
            TransferDirection::Receive,
            TransferStatus::Completed,
            &file.to_string_lossy(),
        )
        .await;
        record_history(
            &state,
            "recv-fail",
            TransferDirection::Receive,
            TransferStatus::Failed,
            &file.to_string_lossy(),
        )
        .await;
        record_history(
            &state,
            "send-ok",
            TransferDirection::Send,
            TransferStatus::Completed,
            &file.to_string_lossy(),
        )
        .await;

        let ok = download_mobile_transfer(
            State(state.clone()),
            Extension(ctx()),
            Path("recv-ok".into()),
        )
        .await
        .expect("completed receive 应可下载");
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(
            ok.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        let disposition = ok
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(disposition.contains("attachment"));
        let body = to_bytes(ok.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"DATA");

        for bad_id in ["recv-fail", "send-ok", "missing"] {
            let err = download_mobile_transfer(
                State(state.clone()),
                Extension(ctx()),
                Path(bad_id.into()),
            )
            .await
            .expect_err("非 completed Receive 必须 404");
            assert_eq!(err.status(), StatusCode::NOT_FOUND);
            let envelope = serde_json::to_value(err.envelope()).unwrap();
            assert!(
                !json_has_path_key(&envelope),
                "404 信封不得含 path: {envelope}"
            );
            assert!(
                !err.envelope()
                    .error
                    .contains(file.to_string_lossy().as_ref()),
                "错误文案不得泄露 path"
            );
        }
    }

    /// 电脑发给手机的 completed Send 可下载；size 变化或普通 Send 404 且无 path。
    #[tokio::test]
    async fn download_allows_mobile_inbox_offer_but_not_peer_send() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let file = root.join("source.bin");
        std::fs::write(&file, b"INBOX").unwrap();
        record_history_with_peer(
            &state,
            "inbox-ok",
            TransferDirection::Send,
            TransferStatus::Completed,
            &file.to_string_lossy(),
            crate::transfer::MOBILE_INBOX_DEVICE_ID,
            5,
        )
        .await;
        record_history_with_peer(
            &state,
            "peer-send",
            TransferDirection::Send,
            TransferStatus::Completed,
            &file.to_string_lossy(),
            "other-pc",
            5,
        )
        .await;

        let ok = download_mobile_transfer(
            State(state.clone()),
            Extension(ctx()),
            Path("inbox-ok".into()),
        )
        .await
        .expect("inbox offer 应可下载");
        assert_eq!(ok.status(), StatusCode::OK);
        let body = to_bytes(ok.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"INBOX");

        let peer_err = download_mobile_transfer(
            State(state.clone()),
            Extension(ctx()),
            Path("peer-send".into()),
        )
        .await
        .expect_err("发给其它电脑的 Send 不得下载");
        assert_eq!(peer_err.status(), StatusCode::NOT_FOUND);
        assert!(!peer_err
            .envelope()
            .error
            .contains(file.to_string_lossy().as_ref()));

        std::fs::write(&file, b"CHANGED").unwrap();
        let size_err = download_mobile_transfer(
            State(state.clone()),
            Extension(ctx()),
            Path("inbox-ok".into()),
        )
        .await
        .expect_err("size 变化应 404");
        assert_eq!(size_err.status(), StatusCode::NOT_FOUND);
        assert!(!size_err
            .envelope()
            .error
            .contains(file.to_string_lossy().as_ref()));
    }

    /// start_sending 命中 inbox：立即 completed，不进 devices 表，幂等回放。
    #[tokio::test]
    async fn start_sending_mobile_inbox_completes_without_peer() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let file = root.join("offer.txt");
        std::fs::write(&file, b"hello").unwrap();
        let op = format!("op-inbox-{}", Uuid::new_v4());
        let id = sender::start_sending(
            state.clone(),
            crate::transfer::MOBILE_INBOX_DEVICE_ID.to_string(),
            file.to_string_lossy().into_owned(),
            op.clone(),
        )
        .await
        .expect("inbox send");
        let task = state.transfer_repo.get_by_id(&id).await.unwrap().unwrap();
        assert_eq!(task.status, TransferStatus::Completed);
        assert_eq!(task.direction, TransferDirection::Send);
        assert_eq!(task.peer_device_id, crate::transfer::MOBILE_INBOX_DEVICE_ID);
        assert_eq!(task.transferred_bytes, 5);
        assert!(task.sha256.is_empty());
        assert!(state.transfers.get(&id).is_none());
        assert!(state
            .devices
            .read()
            .unwrap()
            .get(crate::transfer::MOBILE_INBOX_DEVICE_ID)
            .is_none());

        let replay = sender::start_sending(
            state.clone(),
            crate::transfer::MOBILE_INBOX_DEVICE_ID.to_string(),
            file.to_string_lossy().into_owned(),
            op.clone(),
        )
        .await
        .expect("replay");
        assert_eq!(replay, id);

        let other = root.join("other.txt");
        std::fs::write(&other, b"hello").unwrap();
        let conflict = sender::start_sending(
            state.clone(),
            crate::transfer::MOBILE_INBOX_DEVICE_ID.to_string(),
            other.to_string_lossy().into_owned(),
            op,
        )
        .await
        .expect_err("different path same op");
        assert!(conflict.to_string().contains("operationIdConflict"));
    }

    /// inbox failed 可 retry 重新挂出；resume 明确 unsupported。
    #[tokio::test]
    async fn mobile_inbox_retry_reoffers_and_resume_is_unsupported() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let file = root.join("retry.txt");
        std::fs::write(&file, b"abc").unwrap();
        let failed = TransferTask {
            filename: "retry.txt".into(),
            file_path: file.to_string_lossy().into_owned(),
            size: 3,
            sha256: String::new(),
            direction: TransferDirection::Send,
            peer_device_id: crate::transfer::MOBILE_INBOX_DEVICE_ID.into(),
            status: TransferStatus::Failed,
            transferred_bytes: 0,
            created_at: "2026-09-02T00:00:00Z".into(),
            completed_at: Some("2026-09-02T00:01:00Z".into()),
            phase: Some(TransferPhase::Failed),
            failure: Some(TransferFailure {
                stage: TransferFailureStage::Source,
                code: "source_missing".into(),
                retryable: true,
                message: "gone".into(),
            }),
            attempt: 1,
            logical_transfer_id: "inbox-fail".into(),
            attempt_id: "inbox-fail".into(),
            protocol_transfer_id: "inbox-fail".into(),
            ..TransferTask::recovery_defaults("inbox-fail")
        };
        state.transfer_repo.record(&failed).await.unwrap();

        let resume_err =
            sender::resume_transfer(state.clone(), "inbox-fail".into(), "op-inbox-resume".into())
                .await
                .expect_err("inbox resume");
        assert!(resume_err.to_string().contains("unsupported"));

        let retried =
            sender::retry_transfer(state.clone(), "inbox-fail".into(), "op-inbox-retry".into())
                .await
                .expect("inbox retry");
        assert_eq!(retried.status, TransferStatus::Completed);
        assert_eq!(
            retried.peer_device_id,
            crate::transfer::MOBILE_INBOX_DEVICE_ID
        );
        assert_ne!(retried.id, "inbox-fail");
    }

    /// list_mobile_devices 不得注入虚拟手机目标。
    #[tokio::test]
    async fn list_mobile_devices_omits_inbox_id() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let list = list_mobile_devices_for_state(&state).unwrap();
        assert!(list
            .iter()
            .all(|d| d.id != crate::transfer::MOBILE_INBOX_DEVICE_ID));
    }

    /// 任务列表 JSON 不得出现 path/filePath。
    #[tokio::test]
    async fn tasks_json_omits_path() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let file = root.join("receive").join("secret.bin");
        std::fs::write(&file, b"x").unwrap();
        record_history(
            &state,
            "recv-secret",
            TransferDirection::Receive,
            TransferStatus::Completed,
            &file.to_string_lossy(),
        )
        .await;

        let json = list_mobile_transfer_tasks(State(state), Extension(ctx()))
            .await
            .expect("list")
            .0;
        let value = serde_json::to_value(&json).unwrap();
        assert!(
            !json_has_path_key(&value),
            "tasks JSON 不得含 path: {value}"
        );
        assert!(value[0]["fileName"].as_str().is_some());
        assert!(value[0].get("filePath").is_none());
    }

    /// handler 层拒绝 CHUNK_SIZE+1；路由层 DefaultBodyLimit 同样拒绝。
    #[tokio::test]
    async fn oversized_chunk_is_rejected() {
        let root = unique_temp_dir();
        let state = build_test_state(&root).await;
        let op = format!("op-big-{}", Uuid::new_v4());
        let init = upload_init(
            State(state.clone()),
            Extension(ctx()),
            Json(UploadInitRequest {
                filename: "big.bin".into(),
                size: (CHUNK_SIZE as u64) + 8,
                device_id: "device-test".into(),
                client_operation_id: op,
            }),
        )
        .await
        .expect("init")
        .0;

        let oversized = vec![0u8; CHUNK_SIZE + 1];
        let mut headers = HeaderMap::new();
        headers.insert("X-Chunk-Offset", "0".parse().unwrap());
        let err = upload_chunk(
            State(state.clone()),
            Extension(ctx()),
            Path(init.id.clone()),
            headers,
            Bytes::from(oversized.clone()),
        )
        .await
        .expect_err("handler 必须拒绝 CHUNK_SIZE+1");
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);

        let app = Router::new()
            .route(
                "/api/mobile/transfer/upload/chunk/:id",
                post(upload_chunk).layer(axum::extract::DefaultBodyLimit::max(CHUNK_SIZE)),
            )
            .with_state(state)
            .layer(axum::middleware::from_fn(
                crate::net::request_context::request_id_middleware,
            ));
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/mobile/transfer/upload/chunk/{}", init.id))
            .header("content-type", "application/octet-stream")
            .header("X-Chunk-Offset", "0")
            .body(Body::from(oversized))
            .unwrap();
        let response = app.oneshot(request).await.expect("router");
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "路由层 CHUNK_SIZE+1 应 413"
        );
    }
}
