//! transfer/receiver.rs — 文件接收端逻辑（被 axum route 调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端向本机发送文件时，本机作为接收端：处理 init（创建任务 + 断点续传 offset）、
//!     chunk（写入临时文件）、finalize（SHA256 校验 + 重命名 + 文件名冲突处理）。
//!     对照 Python `transfer/receiver.py`。
//!
//! Code Logic（这个模块做什么）:
//!     - `handle_init(state, meta) -> resume_offset`：在 receive_dir 建 `.{transfer_id}.tmp`，
//!       已存在则返回其大小作 resume_offset；新建 TransferTask（direction=Receive）入 registry。
//!     - `handle_chunk(state, id, offset, bytes)`：seek 到 offset 写入 .tmp，更新 transferred_bytes；
//!       收齐（>= size）时自动 finalize。
//!     - `handle_complete(state, id)`：SHA256 校验 .tmp，通过则解析文件名冲突后重命名 + 写历史。
//!
//! 临时文件命名 `.{transfer_id}.tmp` 与 Python 一致（断点续传识别）。
//! 文件名冲突处理（file.txt → file (1).txt → file (2).txt）与 Python `_resolve_filename` 一致。

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use crate::transfer::CHUNK_SIZE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// 当前时间 RFC3339 ISO 字符串。
fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

/// init 请求体（对照 Python handle_transfer_init 解析的 body）。
#[derive(Debug, serde::Deserialize)]
pub struct InitMeta {
    #[serde(default)]
    pub transfer_id: Option<String>,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default = "default_chunk_size")]
    pub chunk_size: u64,
}

fn default_chunk_size() -> u64 {
    CHUNK_SIZE as u64
}

/// init 响应体（对照 Python init_transfer 返回 `{transfer_id, accepted, resume_offset}`）。
#[derive(Debug, serde::Serialize)]
pub struct InitResp {
    pub transfer_id: String,
    pub accepted: bool,
    pub resume_offset: u64,
}

/// chunk 响应体（对照 Python receive_chunk 返回 `{success, received_bytes}`）。
#[derive(Debug, serde::Serialize)]
pub struct ChunkResp {
    pub success: bool,
    pub received_bytes: u64,
}

/// status 响应体（对照 Python get_transfer_status 返回结构）。
#[derive(Debug, serde::Serialize)]
pub struct StatusResp {
    pub transfer_id: String,
    pub status: String,
    pub progress: f64,
    pub transferred_bytes: u64,
    pub size: u64,
    pub filename: String,
}

/// 处理 init：创建接收任务并返回断点续传 offset。
///
/// Business Logic: 对端发起传输前先发元数据，本端确认接收并告知从何处续传。
/// Code Logic:
///     1. 取 receive_dir，确保存在；
///     2. 临时文件 `.{transfer_id}.tmp`，已存在则其大小为 resume_offset；
///     3. 构造 TransferTask（Receive）入 registry；
///     4. 返回 `{transfer_id, accepted:true, resume_offset}`。
pub async fn handle_init(state: &AppState, meta: InitMeta) -> Result<InitResp, AppError> {
    // 标准 RwLockReadGuard 非 Send，必须在 await 前释放：先 clone 出 receive_dir 字符串。
    let receive_dir = state
        .config
        .read()
        .expect("config 读锁中毒")
        .receive_dir
        .clone();
    let dir = PathBuf::from(&receive_dir);
    tokio::fs::create_dir_all(&dir).await?;

    let transfer_id = meta
        .transfer_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let tmp_path = dir.join(format!(".{transfer_id}.tmp"));

    // 断点续传：检查临时文件已存在大小
    let resume_offset = match tokio::fs::metadata(&tmp_path).await {
        Ok(m) => m.len(),
        Err(_) => 0,
    };

    let task = TransferTask {
        id: transfer_id.clone(),
        filename: meta.filename.clone(),
        file_path: tmp_path.to_string_lossy().to_string(),
        size: meta.size,
        sha256: meta.sha256.clone(),
        chunk_size: meta.chunk_size,
        direction: TransferDirection::Receive,
        peer_device_id: String::new(),
        status: TransferStatus::Pending,
        transferred_bytes: resume_offset,
        created_at: now_iso(),
        completed_at: None,
    };
    state.transfers.add(task);

    tracing::info!(
        "接受传输请求: {transfer_id}, 文件={}, 大小={}, resume_offset={resume_offset}",
        meta.filename,
        meta.size
    );

    Ok(InitResp {
        transfer_id,
        accepted: true,
        resume_offset,
    })
}

/// 处理 chunk：将数据写入临时文件指定 offset，收齐时自动 finalize。
///
/// Business Logic: 对端逐块发来数据，本端按 offset 写入临时文件；全部收齐后校验并保存。
/// Code Logic:
///     1. 查 registry 任务，不存在返回 success:false（对照 Python）；
///     2. 打开/创建 .tmp（写模式，允许读写以 seek），seek 到 offset 写入；
///     3. 更新 transferred_bytes；
///     4. 若 transferred >= size 则 finalize（SHA256 校验 + 重命名）；
///     5. 返回 `{success:true, received_bytes}`。
pub async fn handle_chunk(
    state: &AppState,
    app_handle: &AppHandle,
    transfer_id: &str,
    offset: u64,
    data: Vec<u8>,
) -> Result<ChunkResp, AppError> {
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => {
            tracing::error!("未找到传输任务: {transfer_id}");
            return Ok(ChunkResp {
                success: false,
                received_bytes: 0,
            });
        }
    };

    state
        .transfers
        .set_status(transfer_id, TransferStatus::Transferring);

    let tmp_path = PathBuf::from(&task.file_path);
    // 以 OpenOptions 打开（create + write + read，不 truncate）：断点续传需保留旧内容，
    // seek 到 offset 后写入。对照 Python `open(path, "r+b" if exists else "wb")` 的 r+b 语义。
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(&tmp_path)
        .await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    file.write_all(&data).await?;
    file.flush().await?;

    let new_transferred = offset + data.len() as u64;
    state
        .transfers
        .update_progress(transfer_id, new_transferred, TransferStatus::Transferring);

    // 收齐则 finalize
    if new_transferred >= task.size {
        if let Err(e) = finalize_transfer(state, app_handle, transfer_id).await {
            tracing::error!("finalize 失败: {transfer_id}, {e}");
        }
    }

    Ok(ChunkResp {
        success: true,
        received_bytes: new_transferred,
    })
}

/// 完成传输：SHA256 校验临时文件，通过则重命名（处理冲突）+ 写历史；失败标记 failed。
///
/// Business Logic: 文件全部接收后需校验完整性，确保无误后落地为最终文件名。
/// Code Logic: 对照 Python `finalize_transfer`：
///     1. 计算 .tmp 的 SHA256，与任务记录的 sha256 比较；
///     2. 校验失败：标记 failed + 删除 .tmp + emit failed；
///     3. 校验通过：resolve_filename 解析冲突，重命名 .tmp → 最终路径；
///        标记 completed + 写历史 + emit completed。
pub async fn finalize_transfer(
    state: &AppState,
    app_handle: &AppHandle,
    transfer_id: &str,
) -> Result<(), AppError> {
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => return Ok(()),
    };

    let tmp_path = PathBuf::from(&task.file_path);

    // 校验 SHA256
    let actual = match compute_sha256(&tmp_path).await {
        Ok(h) => h,
        Err(e) => {
            on_receive_failed(
                state,
                app_handle,
                transfer_id,
                &format!("读取临时文件失败: {e}"),
            )
            .await;
            return Ok(());
        }
    };

    if actual != task.sha256 {
        // 校验失败：删除损坏的临时文件
        let _ = tokio::fs::remove_file(&tmp_path).await;
        on_receive_failed(
            state,
            app_handle,
            transfer_id,
            &format!("SHA256 校验失败: 期望={}, 实际={actual}", task.sha256),
        )
        .await;
        return Ok(());
    }

    // 解析文件名冲突并重命名
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    let final_filename = resolve_filename(&receive_dir, &task.filename);
    let final_path = receive_dir.join(&final_filename);
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        on_receive_failed(state, app_handle, transfer_id, &format!("重命名失败: {e}")).await;
        return Ok(());
    }

    // 标记 completed + 写历史
    let completed_at = now_iso();
    state.transfers.mark_completed(
        transfer_id,
        completed_at.clone(),
        Some(final_path.to_string_lossy().to_string()),
    );
    if let Some(t) = state.transfers.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    state.transfers.remove(transfer_id);

    let _ = app_handle.emit(
        "transfer:completed",
        serde_json::json!({
            "id": transfer_id,
            "status": "completed",
            "filePath": final_path.to_string_lossy().to_string(),
        }),
    );

    tracing::info!("文件接收完成: {transfer_id} -> {}", final_path.display());
    Ok(())
}

/// 接收失败统一处理：标记 failed + 写历史 + remove + emit failed。
async fn on_receive_failed(
    state: &AppState,
    app_handle: &AppHandle,
    transfer_id: &str,
    error: &str,
) {
    let completed_at = now_iso();
    state.transfers.mark_failed(transfer_id, completed_at);
    if let Some(t) = state.transfers.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    state.transfers.remove(transfer_id);
    let _ = app_handle.emit(
        "transfer:failed",
        serde_json::json!({
            "id": transfer_id,
            "status": "failed",
            "errorMessage": error,
        }),
    );
}

/// 处理 status 查询（对端 GET /api/transfer/status/:id 调用）。
///
/// Business Logic: 对端或本端可查询接收任务进度。
/// Code Logic: 对照 Python `get_transfer_status`，任务不存在返回 error 字段。
pub async fn handle_status(state: &AppState, transfer_id: &str) -> StatusResp {
    match state.transfers.get(transfer_id) {
        Some(t) => StatusResp {
            transfer_id: transfer_id.to_string(),
            status: status_str(t.status),
            progress: t.progress(),
            transferred_bytes: t.transferred_bytes,
            size: t.size,
            filename: t.filename,
        },
        None => StatusResp {
            transfer_id: transfer_id.to_string(),
            status: "unknown".to_string(),
            progress: 0.0,
            transferred_bytes: 0,
            size: 0,
            filename: String::new(),
        },
    }
}

/// 将状态枚举转为字符串（对照 Python status.value）。
fn status_str(s: TransferStatus) -> String {
    match s {
        TransferStatus::Pending => "pending",
        TransferStatus::Transferring => "transferring",
        TransferStatus::Completed => "completed",
        TransferStatus::Failed => "failed",
        TransferStatus::Cancelled => "cancelled",
    }
    .to_string()
}

/// 异步流式计算文件 SHA256（8KB 块，对照 Python）。
async fn compute_sha256(path: &Path) -> Result<String, AppError> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 解析文件名冲突：receive_dir 下同名则加 (1)/(2) 后缀。
///
/// Business Logic: 避免覆盖已存在文件。对照 Python `_resolve_filename`。
/// Code Logic: stem + " ({n})" + suffix，逐次递增直到不冲突。
pub fn resolve_filename(dir: &Path, filename: &str) -> String {
    let target = dir.join(filename);
    if !target.exists() {
        return filename.to_string();
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| filename.to_string());
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut counter = 1;
    loop {
        let new_name = format!("{stem} ({counter}){suffix}");
        if !dir.join(&new_name).exists() {
            return new_name;
        }
        counter += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
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
}
