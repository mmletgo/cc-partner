//! transfer/receiver — 文件接收端逻辑（被 axum route 调用）
//!
//! Business Logic（为什么需要这个模块）:
//!     对端向本机发送文件时，本机作为接收端：处理 init（创建任务 + 断点续传 offset）、
//!     chunk（写入临时文件）、complete/finalize（SHA256 校验 + 原子落盘 + 文件名冲突处理）。
//!     对照 Python `transfer/receiver.py`。
//!
//! Code Logic（这个模块做什么）:
//!     - `handle_init(state, meta) -> resume_offset`：在 receive_dir 建 `.{transfer_id}.tmp`，
//!       已存在则返回其大小作 resume_offset（tmp 大于 size 时拒绝续传并删除损坏 tmp）；
//!       新建 TransferTask（direction=Receive）入 registry。
//!     - `handle_chunk(state, id, offset, bytes)`：seek 到 offset 写入 .tmp，更新 transferred_bytes；
//!       收齐（>= size）时自动 finalize。
//!     - `handle_complete(state, id)`：显式终态握手；对 size=0 或 resume 已满的任务触发 finalize，
//!       并返回与 chunk 相同的 `{success, received_bytes}`（命中墓碑则幂等回放）。
//!     - finalize：SHA256 校验 .tmp，通过后在 receive_dir 锁内原子 no-replace 落盘。
//!     - finalize intent journal：每个候选 final 路径（首选与后缀）place 前先持久化 intent，
//!       覆盖 place→history 崩溃窗口；重启后 init/complete 可晋升 durable，禁止同 transfer_id
//!       后缀重复副本（含首选名已存在时的后缀落盘路径）。
//!
//! 子模块：
//!     - `validation`：路径/basename 校验
//!     - `chunk_io`：chunk/tmp 打开与 SHA256
//!     - `resume`：history 终态回放与 intent 恢复编排
//!     - `finalize`：intent journal + place/commit/certify
//!
//! 临时文件命名 `.{transfer_id}.tmp` 与 Python 一致（断点续传识别）。
//! 文件名冲突处理（file.txt → file (1).txt → file (2).txt）与 Python `_resolve_filename` 一致。

mod validation;
mod chunk_io;
mod resume;
mod finalize;

// 子模块内部 helper 提升到本模块，供 handlers 与 tests（use super::*）使用。
use chunk_io::*;
use finalize::*;
use resume::*;
use validation::*;

pub use finalize::finalize_transfer;

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use crate::transfer::CHUNK_SIZE;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

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
///     同一 transfer_id 的重复 init 必须与 chunk/finalize 串行，且不得覆盖活跃任务或绕过终态墓碑。
///     安全边界：filename/transfer_id 只能是单个普通路径组件；幂等路径只接受 Receive 任务，禁止把 Send 源文件当接收目标。
/// Code Logic:
///     1. 取 receive_dir，确保存在；
///     2. 校验并规范化 filename；解析 transfer_id（缺省 UUID，否则校验为单组件）；
///     3. 获取与 chunk 相同的 per-ID 锁；
///     4. 持锁后重查 active task / 墓碑：
///        - 活跃且 direction=Receive 且元数据相同 → 幂等返回当前 resume_offset；
///        - 活跃但 direction=Send 或元数据不同 → conflict；
///        - 命中墓碑 → conflict（终态后禁止 reopen 写路径）；
///     5. 否则读/建 `.{transfer_id}.tmp`，构造 TransferTask（Receive）入 registry；
///     6. 返回 `{transfer_id, accepted:true, resume_offset}`。
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

    let safe_filename = sanitize_receive_basename(&meta.filename, "filename")?;
    let transfer_id = match meta.transfer_id.as_deref() {
        Some(id) => sanitize_receive_basename(id, "transfer_id")?,
        None => uuid::Uuid::new_v4().to_string(),
    };

    // init 与 chunk/finalize 共享 per-ID 锁，防止并发 init 覆盖活跃 entry 或 finalize 后重开写路径。
    let init_lock = state.transfers.finalize_lock(&transfer_id);
    let _guard = init_lock.lock().await;

    if let Some(existing) = state.transfers.get(&transfer_id) {
        // Send 任务与 Receive 共享 registry；init 幂等路径不得把 outbound 源文件当接收目标。
        if existing.direction != TransferDirection::Receive {
            return Err(AppError::conflict(format!(
                "transfer_id `{transfer_id}` 属于发送任务，拒绝作为接收 init 目标"
            )));
        }
        if init_metadata_matches(&existing, &meta, &safe_filename) {
            let resume_offset = match tokio::fs::metadata(&existing.file_path).await {
                Ok(m) => m.len().max(existing.transferred_bytes),
                Err(_) => existing.transferred_bytes,
            };
            tracing::info!("幂等 init 命中活跃传输: {transfer_id}, resume_offset={resume_offset}");
            return Ok(InitResp {
                transfer_id,
                accepted: true,
                resume_offset,
            });
        }
        return Err(AppError::conflict(format!(
            "transfer_id `{transfer_id}` 已存在且元数据不一致，拒绝覆盖活跃传输"
        )));
    }

    if state.transfers.tombstone(&transfer_id).is_some() {
        return Err(AppError::conflict(format!(
            "transfer_id `{transfer_id}` 已终态完成，拒绝重新 init"
        )));
    }

    // 跨重启：history 中已 durable 的 Receive 终态禁止 reopen（避免后缀重复）。
    if let Some(hist) = state.transfer_repo.get_by_id(&transfer_id).await? {
        if hist.direction == TransferDirection::Receive
            && matches!(
                hist.status,
                TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
            )
        {
            return Err(AppError::conflict(format!(
                "transfer_id `{transfer_id}` 已在 transfer_history 终态，拒绝重新 init"
            )));
        }
    }

    // place→history 崩溃窗口：intent + 最终文件已在 → 晋升 durable 后拒绝 reopen。
    if try_recover_finalize_intent(state, &transfer_id, &dir)
        .await?
        .is_some()
    {
        return Err(AppError::conflict(format!(
            "transfer_id `{transfer_id}` 已从 finalize intent 恢复终态，拒绝重新 init"
        )));
    }

    let tmp_path = receive_tmp_path(&dir, &transfer_id)?;

    // 断点续传：检查临时文件已存在大小（no-follow，拒绝 symlink 把 resume 指到 receive_dir 外）。
    // tmp 大于声明 size 说明损坏/脏数据，拒绝静默截断：删除 tmp 并返回 Validation，
    // 让发送端 fail 后由用户重试（新 init 从 0 开始）。
    let resume_offset = match receive_tmp_len_nofollow(&tmp_path).await {
        Ok(Some(len)) => {
            if len > meta.size {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(AppError::validation(format!(
                    "临时文件长度 {len} 超过声明 size {}，已删除损坏断点，请重新发起传输",
                    meta.size
                )));
            }
            len
        }
        Ok(None) => 0,
        Err(e) => return Err(e),
    };

    let task = TransferTask {
        id: transfer_id.clone(),
        filename: safe_filename.clone(),
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
        ..TransferTask::recovery_defaults(&transfer_id)
    };
    state.transfers.add(task);

    tracing::info!(
        "接受传输请求: {transfer_id}, 文件={safe_filename}, 大小={}, resume_offset={resume_offset}",
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
///     Finding 4: 最后一块可能在传输层被重试，重放时 registry 已移除会误返回 success:false。
///     通过 per-transfer 单飞锁 + 终态墓碑保证重放安全：第一个请求完成 finalize 写墓碑，
///     迟到请求必须在任何文件 open/write 前命中墓碑，返回第一次的成功结果。
///     安全边界：仅接受 direction=Receive 的任务；禁止用 outbound Send 任务 id 写入源文件。
///     durable 窗口：最后一块 place 成功但 history 瞬时失败时，active 已是 Completed、
///     status 仍 transferring；必须返回 retryable 5xx（与 handle_complete 一致），
///     禁止 HTTP 200 success=false——否则 chunk 客户端只接受 completed，发送端永久失败，
///     后续 complete 重试路径永不执行。
/// Code Logic:
///     1. 先取 per-transfer 单飞锁（覆盖 re-read 任务/墓碑、写块、进度、finalize 全临界区）；
///     2. 持锁后若 `data.len() > CHUNK_SIZE` 立即 Validation 拒绝（禁止 open/write 磁盘）；
///     3. 持锁后重读 registry：不存在则查墓碑，命中则直接返回第一次结果（禁止 open 文件）；
///     4. 存在但 direction≠Receive → 拒绝写入并返回 success:false；
///     5. active 已 Completed → 只重试 durable 晋升；失败上抛 Unavailable；
///     6. 打开/创建 .tmp（写模式，允许读写以 seek），seek 到 offset 写入；
///     7. 更新 transferred_bytes；
///     8. 若 transferred >= size 则 finalize；place 成功但 history 未 durable → Unavailable；
///     9. 返回 `{success:true, received_bytes}` 或墓碑结果。
pub async fn handle_chunk(
    state: &AppState,
    transfer_id: &str,
    offset: u64,
    data: Vec<u8>,
) -> Result<ChunkResp, AppError> {
    // 单飞锁必须覆盖 re-read → open/write → progress → finalize。
    // 若只在 finalize 前加锁，并发末块可在 rename 后仍持有旧 fd 改写已校验文件。
    let chunk_lock = state.transfers.finalize_lock(transfer_id);
    let _guard = chunk_lock.lock().await;

    // 资源边界：单块不得超过 CHUNK_SIZE，必须在任何磁盘 open/write 前拒绝。
    // HTTP 路由另有 route-local body limit；此处覆盖直接调用与协议防御。
    if data.len() > CHUNK_SIZE {
        return Err(AppError::validation(format!(
            "chunk 大小 {} 超过上限 {} 字节",
            data.len(),
            CHUNK_SIZE
        )));
    }

    // 持锁后重读任务/墓碑：迟到请求必须在任何文件打开前命中墓碑。
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => {
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                let success = matches!(
                    tomb.outcome,
                    crate::transfer::registry::TransferOutcome::Completed { .. }
                );
                return Ok(ChunkResp {
                    success,
                    received_bytes: tomb.received_bytes,
                });
            }
            // 重启后墓碑丢失：history 中 Receive 终态仍可幂等响应最后一块重放。
            if let Some(resp) = history_terminal_chunk_resp(state, transfer_id).await? {
                tracing::info!(
                    "重放最后一块命中 transfer_history 终态: {transfer_id}, success={}",
                    resp.success
                );
                return Ok(resp);
            }
            tracing::error!("未找到传输任务: {transfer_id}");
            return Ok(ChunkResp {
                success: false,
                received_bytes: 0,
            });
        }
    };

    // 方向隔离：Send 任务的 file_path 指向本机源文件，绝不能被 chunk 路由改写/删除。
    if task.direction != TransferDirection::Receive {
        tracing::warn!(
            "拒绝向非 Receive 任务写入 chunk: {transfer_id}, direction={:?}",
            task.direction
        );
        return Ok(ChunkResp {
            success: false,
            received_bytes: 0,
        });
    }

    // 落盘已成功但 history 未 durable：禁止再写文件，只重试晋升；失败上抛 5xx（与 complete 一致）。
    if task.status == TransferStatus::Completed {
        if let Err(e) = finalize_transfer(state, transfer_id).await {
            tracing::error!("chunk 路径 durable 晋升失败: {transfer_id}, {e}");
            return Err(AppError::unavailable(format!(
                "transfer_history 尚未 durable，请重试 complete: {transfer_id}: {e}"
            )));
        }
        if let Some(tomb) = state.transfers.tombstone(transfer_id) {
            let success = matches!(
                tomb.outcome,
                crate::transfer::registry::TransferOutcome::Completed { .. }
            );
            return Ok(ChunkResp {
                success,
                received_bytes: tomb.received_bytes,
            });
        }
        return Err(AppError::unavailable(format!(
            "transfer durable 晋升后无墓碑，请重试 complete: {transfer_id}"
        )));
    }

    state
        .transfers
        .set_status(transfer_id, TransferStatus::Transferring);

    let tmp_path = PathBuf::from(&task.file_path);
    // 首次 create_new；续传 no-follow 只打开普通文件。禁止跟随 symlink 写出 receive_dir。
    // seek 到 offset 后写入（r+b 语义，不 truncate）。
    let mut file = open_receive_tmp_rw(&tmp_path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    file.write_all(&data).await?;
    file.flush().await?;
    // 显式 drop 文件句柄，finalize 的 rename 前不持有写 fd。
    drop(file);

    let new_transferred = offset + data.len() as u64;
    state
        .transfers
        .update_progress(transfer_id, new_transferred, TransferStatus::Transferring);

    // 收齐则 finalize（已持锁，串行化）。
    if new_transferred >= task.size {
        if let Err(e) = finalize_transfer(state, transfer_id).await {
            tracing::error!("finalize 失败: {transfer_id}, {e}");
            // 已 failed 墓碑：返回 success=false（校验失败等业务终态）。
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                let success = matches!(
                    tomb.outcome,
                    crate::transfer::registry::TransferOutcome::Completed { .. }
                );
                return Ok(ChunkResp {
                    success,
                    received_bytes: tomb.received_bytes,
                });
            }
            // durable 未就绪：active 已是 Completed（文件在、history 无）→ 5xx 驱动重试。
            // 禁止 HTTP 200 success=false：chunk 客户端只在 status=completed 时收敛，
            // 而此处 status 故意仍为 transferring，会让发送端永久失败且跳过 complete。
            if let Some(t) = state.transfers.get(transfer_id) {
                if t.status == TransferStatus::Completed {
                    return Err(AppError::unavailable(format!(
                        "transfer_history 尚未 durable，请重试 complete: {transfer_id}: {e}"
                    )));
                }
                return Ok(ChunkResp {
                    success: false,
                    received_bytes: t.transferred_bytes,
                });
            }
            return Ok(ChunkResp {
                success: false,
                received_bytes: new_transferred,
            });
        }
    }

    // finalize 后若任务已移除，优先返回墓碑结果（成功/失败），避免并发重放把失败当成功。
    if state.transfers.get(transfer_id).is_none() {
        if let Some(tomb) = state.transfers.tombstone(transfer_id) {
            let success = matches!(
                tomb.outcome,
                crate::transfer::registry::TransferOutcome::Completed { .. }
            );
            return Ok(ChunkResp {
                success,
                received_bytes: tomb.received_bytes,
            });
        }
    }

    // 收齐后若仍停留在 Completed active（finalize Ok 却无墓碑）→ durable 窗口，上抛 5xx。
    if let Some(t) = state.transfers.get(transfer_id) {
        if t.status == TransferStatus::Completed && state.transfers.tombstone(transfer_id).is_none()
        {
            return Err(AppError::unavailable(format!(
                "transfer_history 尚未 durable，请重试 complete: {transfer_id}"
            )));
        }
    }

    Ok(ChunkResp {
        success: true,
        received_bytes: new_transferred,
    })
}

/// 显式 complete/finalize 握手（round9）。
///
/// Business Logic（为什么需要这个函数）:
///     size=0 与 resume_offset==size 时发送端不会发任何 chunk，接收端若只在 handle_chunk 里
///     finalize，最终文件不会落地，发送端却可能误报 completed。发送端必须在标记本地 completed
///     前调用本接口确认远端终态。
///
/// Code Logic（这个函数做什么）:
///     1. 取 per-transfer 单飞锁；
///     2. 无 active 任务时查墓碑；仍 miss 时查 transfer_history（Receive 方向）跨重启收敛；
///        再 miss 时尝试 finalize intent 恢复（place 后 crash 窗口）；
///     3. 拒绝 non-Receive；
///     4. 读取 tmp 实际长度：不足 size → success=false（尚未收齐）；
///        >= size（含 size=0 空文件）→ 调用 finalize_transfer；
///     5. durable history 写入失败上抛 Unavailable（retryable 5xx），驱动发送端重试 complete；
///     6. 返回墓碑结果或当前进度。
pub async fn handle_complete(state: &AppState, transfer_id: &str) -> Result<ChunkResp, AppError> {
    let complete_lock = state.transfers.finalize_lock(transfer_id);
    let _guard = complete_lock.lock().await;

    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => {
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                let success = matches!(
                    tomb.outcome,
                    crate::transfer::registry::TransferOutcome::Completed { .. }
                );
                return Ok(ChunkResp {
                    success,
                    received_bytes: tomb.received_bytes,
                });
            }
            // 进程重启后内存墓碑丢失：用持久化 history 收敛已完成的 Receive。
            if let Some(resp) = history_terminal_chunk_resp(state, transfer_id).await? {
                return Ok(resp);
            }
            // place→history 崩溃窗口：intent + 最终文件 → 晋升 durable。
            let receive_dir =
                PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
            if let Some(resp) =
                try_recover_finalize_intent(state, transfer_id, &receive_dir).await?
            {
                return Ok(resp);
            }
            tracing::error!("complete 未找到传输任务: {transfer_id}");
            return Ok(ChunkResp {
                success: false,
                received_bytes: 0,
            });
        }
    };

    if task.direction != TransferDirection::Receive {
        tracing::warn!(
            "complete 拒绝非 Receive 任务: {transfer_id}, direction={:?}",
            task.direction
        );
        return Ok(ChunkResp {
            success: false,
            received_bytes: 0,
        });
    }

    // 已落盘待 durable：file_path 已是最终路径，禁止按 tmp 再写；只重试 history 晋升。
    if task.status == TransferStatus::Completed {
        if let Err(e) = finalize_transfer(state, transfer_id).await {
            // durable 失败：上抛 retryable Unavailable，让 PeerClient 走 5xx 重试路径。
            tracing::error!("complete durable 晋升失败: {transfer_id}, {e}");
            return Err(AppError::unavailable(format!(
                "transfer_history 尚未 durable，请重试 complete: {transfer_id}: {e}"
            )));
        }
        if let Some(tomb) = state.transfers.tombstone(transfer_id) {
            let success = matches!(
                tomb.outcome,
                crate::transfer::registry::TransferOutcome::Completed { .. }
            );
            return Ok(ChunkResp {
                success,
                received_bytes: tomb.received_bytes,
            });
        }
        return Err(AppError::unavailable(format!(
            "transfer durable 晋升后无墓碑，请重试 complete: {transfer_id}"
        )));
    }

    let tmp_path = PathBuf::from(&task.file_path);
    // size=0：没有 tmp 也视为已收齐；否则以 no-follow 普通文件长度为准（拒绝 symlink）。
    let actual_len = match receive_tmp_len_nofollow(&tmp_path).await {
        Ok(Some(len)) => len,
        Ok(None) => {
            if task.size == 0 {
                0
            } else {
                task.transferred_bytes
            }
        }
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("读取临时文件失败: {e}")).await;
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                return Ok(ChunkResp {
                    success: false,
                    received_bytes: tomb.received_bytes,
                });
            }
            return Ok(ChunkResp {
                success: false,
                received_bytes: 0,
            });
        }
    };

    if actual_len < task.size {
        return Ok(ChunkResp {
            success: false,
            received_bytes: actual_len,
        });
    }

    // size=0 且 tmp 不存在时 create_new 空普通文件（拒绝覆盖/跟随 symlink）。
    if task.size == 0 && actual_len == 0 {
        if let Err(e) = open_receive_tmp_rw(&tmp_path).await {
            on_receive_failed(state, transfer_id, &format!("创建空临时文件失败: {e}")).await;
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                return Ok(ChunkResp {
                    success: false,
                    received_bytes: tomb.received_bytes,
                });
            }
            return Ok(ChunkResp {
                success: false,
                received_bytes: 0,
            });
        }
    }

    if let Err(e) = finalize_transfer(state, transfer_id).await {
        tracing::error!("complete finalize 失败: {transfer_id}, {e}");
        // 若文件已落盘但 history 未 durable（active Completed），上抛 5xx 驱动重试。
        if let Some(t) = state.transfers.get(transfer_id) {
            if t.status == TransferStatus::Completed {
                return Err(AppError::unavailable(format!(
                    "transfer_history 尚未 durable，请重试 complete: {transfer_id}: {e}"
                )));
            }
        }
        // 其它 finalize 错误（校验失败等）已写 failed 墓碑，下面按墓碑/active 返回。
    }

    if let Some(tomb) = state.transfers.tombstone(transfer_id) {
        let success = matches!(
            tomb.outcome,
            crate::transfer::registry::TransferOutcome::Completed { .. }
        );
        return Ok(ChunkResp {
            success,
            received_bytes: tomb.received_bytes,
        });
    }

    // finalize 未写墓碑：若仍是 Completed active → durable 窗口，上抛 5xx。
    if let Some(t) = state.transfers.get(transfer_id) {
        if t.status == TransferStatus::Completed {
            return Err(AppError::unavailable(format!(
                "transfer_history 尚未 durable，请重试 complete: {transfer_id}"
            )));
        }
        return Ok(ChunkResp {
            success: false,
            received_bytes: t.transferred_bytes,
        });
    }
    Ok(ChunkResp {
        success: false,
        received_bytes: actual_len,
    })
}

/// 处理 status 查询（对端 GET /api/transfer/status/:id 调用）。
///
/// Business Logic: 对端或本端可查询接收任务进度。
///     Finding 4: 任务终态后从 registry 移除，status 查询命中终态墓碑还原 completed/failed
///     与最终字节数，避免返回 "unknown" 让对端误判为丢失。
///     进程重启后内存墓碑消失时，再查 transfer_history（Receive）收敛。
/// Code Logic: 先查 registry → 墓碑 → transfer_history Receive 终态；仍 miss 返回 unknown。
pub async fn handle_status(state: &AppState, transfer_id: &str) -> StatusResp {
    if let Some(t) = state.transfers.get(transfer_id) {
        // active 上的 Completed 表示“文件已落盘、history 尚未 durable”，
        // 对发送端仍应表现为 transferring，避免 status 收敛成假成功后不再重试 record。
        let (status, transferred_bytes) =
            if t.direction == TransferDirection::Receive && t.status == TransferStatus::Completed {
                ("transferring".to_string(), t.size)
            } else {
                (status_str(t.status), t.transferred_bytes)
            };
        return StatusResp {
            transfer_id: transfer_id.to_string(),
            status,
            progress: t.progress(),
            transferred_bytes,
            size: t.size,
            filename: t.filename,
        };
    }
    // 终态墓碑兜底（Finding 4）。
    if let Some(tomb) = state.transfers.tombstone(transfer_id) {
        let (status_str_val, received) = match &tomb.outcome {
            crate::transfer::registry::TransferOutcome::Completed { .. } => {
                ("completed".to_string(), tomb.size)
            }
            crate::transfer::registry::TransferOutcome::Failed { .. } => {
                ("failed".to_string(), tomb.received_bytes)
            }
        };
        let size = tomb.size;
        return StatusResp {
            transfer_id: transfer_id.to_string(),
            status: status_str_val,
            progress: if size == 0 {
                0.0
            } else {
                (received as f64) / (size as f64)
            },
            transferred_bytes: received,
            size,
            filename: tomb.filename,
        };
    }
    // 跨重启：history 中 Receive 终态可还原 status，避免 unknown 导致发送端假失败。
    if let Ok(Some(resp)) = history_terminal_status_resp(state, transfer_id).await {
        return resp;
    }
    // place→history 崩溃窗口：尝试从 intent 晋升后再读 history/墓碑。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    if let Ok(Some(_)) = try_recover_finalize_intent(state, transfer_id, &receive_dir).await {
        if let Some(tomb) = state.transfers.tombstone(transfer_id) {
            let (status_str_val, received) = match &tomb.outcome {
                crate::transfer::registry::TransferOutcome::Completed { .. } => {
                    ("completed".to_string(), tomb.size)
                }
                crate::transfer::registry::TransferOutcome::Failed { .. } => {
                    ("failed".to_string(), tomb.received_bytes)
                }
            };
            let size = tomb.size;
            return StatusResp {
                transfer_id: transfer_id.to_string(),
                status: status_str_val,
                progress: if size == 0 {
                    0.0
                } else {
                    (received as f64) / (size as f64)
                },
                transferred_bytes: received,
                size,
                filename: tomb.filename,
            };
        }
        if let Ok(Some(resp)) = history_terminal_status_resp(state, transfer_id).await {
            return resp;
        }
    }
    StatusResp {
        transfer_id: transfer_id.to_string(),
        status: "unknown".to_string(),
        progress: 0.0,
        transferred_bytes: 0,
        size: 0,
        filename: String::new(),
    }
}

/// 解析文件名冲突：receive_dir 下同名则加 (1)/(2) 后缀。
///
/// Business Logic: 避免覆盖已存在文件。对照 Python `_resolve_filename`。
/// Code Logic: stem + " ({n})" + suffix，逐次递增直到不冲突。
///     调用方必须先 `sanitize_receive_basename`；本函数假定 filename 已是单组件 basename。
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
mod tests;
