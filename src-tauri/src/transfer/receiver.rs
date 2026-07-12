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
use std::path::{Component, Path, PathBuf};
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

    let tmp_path = receive_tmp_path(&dir, &transfer_id)?;

    // 断点续传：检查临时文件已存在大小
    let resume_offset = match tokio::fs::metadata(&tmp_path).await {
        Ok(m) => m.len(),
        Err(_) => 0,
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

/// Business Logic（为什么需要这个函数）:
///     同一 transfer_id 重放 init 时，只有元数据完全一致才允许幂等返回，否则必须 conflict。
///
/// Code Logic（这个函数做什么）:
///     比较已规范化的 filename/size/sha256/chunk_size 是否与活跃 Receive 任务一致。
fn init_metadata_matches(task: &TransferTask, meta: &InitMeta, safe_filename: &str) -> bool {
    task.filename == safe_filename
        && task.size == meta.size
        && task.sha256 == meta.sha256
        && task.chunk_size == meta.chunk_size
}

/// Business Logic（为什么需要这个函数）:
///     远端 filename/transfer_id 进入路径拼接前必须限制为单个普通组件，否则绝对路径或 `..`
///     可逃逸 receive_dir，把校验通过的内容写到任意可写路径。
///
/// Code Logic（这个函数做什么）:
///     trim 后拒绝空串；Path components 必须恰好一个 Normal，且不含 `/` `\` 或 `.`/`..`。
fn sanitize_receive_basename(raw: &str, field: &str) -> Result<String, AppError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AppError::validation(format!("{field} 不能为空")));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(AppError::validation(format!(
            "{field} 只能是单个文件名组件，禁止路径分隔符"
        )));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(AppError::validation(format!("{field} 不能是绝对路径")));
    }
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(part)), None) => {
            let s = part.to_string_lossy();
            if s.is_empty() || s == "." || s == ".." {
                return Err(AppError::validation(format!(
                    "{field} 非法：禁止 `.`/`..` 或空组件"
                )));
            }
            // 防御：Windows 下某些前缀/盘符可能被解析为 Prefix 而非 Normal。
            Ok(s.into_owned())
        }
        _ => Err(AppError::validation(format!(
            "{field} 只能是单个普通文件名组件，禁止绝对路径、父目录或前缀"
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     临时文件 `.{transfer_id}.tmp` 也必须落在 receive_dir 内，避免 transfer_id 逃逸。
///
/// Code Logic（这个函数做什么）:
///     用已校验的 transfer_id 拼临时名，再验证最终路径仍位于 receive_dir 之下。
fn receive_tmp_path(receive_dir: &Path, transfer_id: &str) -> Result<PathBuf, AppError> {
    let tmp_name = format!(".{transfer_id}.tmp");
    // transfer_id 已是单组件，前缀 `.` + 后缀 `.tmp` 仍应是单组件。
    let _ = sanitize_receive_basename(&tmp_name, "transfer_id_tmp")?;
    let tmp_path = receive_dir.join(&tmp_name);
    ensure_path_within_dir(receive_dir, &tmp_path)?;
    Ok(tmp_path)
}

/// Business Logic（为什么需要这个函数）:
///     join 后的最终/临时路径必须仍在 receive_dir 内，防止绝对路径替换或 `..` 逃逸。
///
/// Code Logic（这个函数做什么）:
///     对父目录做 canonicalize（不存在时回退规范化），断言目标仍以 receive_dir 为前缀。
fn ensure_path_within_dir(dir: &Path, candidate: &Path) -> Result<(), AppError> {
    let canonical_dir = match dir.canonicalize() {
        Ok(p) => p,
        Err(_) => normalize_path(dir),
    };
    let parent = candidate.parent().unwrap_or(dir);
    let canonical_parent = match parent.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // 父目录可能尚未创建；用 dir 的 canonical + 相对剩余部分做逻辑归一化。
            if parent == dir {
                canonical_dir.clone()
            } else {
                normalize_path(parent)
            }
        }
    };
    if !canonical_parent.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    let file_name = candidate
        .file_name()
        .ok_or_else(|| AppError::validation("目标路径缺少文件名".to_string()))?;
    let final_path = canonical_parent.join(file_name);
    if !final_path.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    Ok(())
}

/// Code Logic: 去掉 `.`、解析 `..` 的逻辑路径规范化（不访问磁盘）。
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// 处理 chunk：将数据写入临时文件指定 offset，收齐时自动 finalize。
///
/// Business Logic: 对端逐块发来数据，本端按 offset 写入临时文件；全部收齐后校验并保存。
///     Finding 4: 最后一块可能在传输层被重试，重放时 registry 已移除会误返回 success:false。
///     通过 per-transfer 单飞锁 + 终态墓碑保证重放安全：第一个请求完成 finalize 写墓碑，
///     迟到请求必须在任何文件 open/write 前命中墓碑，返回第一次的成功结果。
///     安全边界：仅接受 direction=Receive 的任务；禁止用 outbound Send 任务 id 写入源文件。
/// Code Logic:
///     1. 先取 per-transfer 单飞锁（覆盖 re-read 任务/墓碑、写块、进度、finalize 全临界区）；
///     2. 持锁后重读 registry：不存在则查墓碑，命中则直接返回第一次结果（禁止 open 文件）；
///     3. 存在但 direction≠Receive → 拒绝写入并返回 success:false；
///     4. 打开/创建 .tmp（写模式，允许读写以 seek），seek 到 offset 写入；
///     5. 更新 transferred_bytes；
///     6. 若 transferred >= size 则 finalize（SHA256 校验 + 重命名）；
///     7. 返回 `{success:true, received_bytes}`。
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

    // 持锁后重读任务/墓碑：迟到请求必须在任何文件打开前命中墓碑。
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => {
            if let Some(tomb) = state.transfers.tombstone(transfer_id) {
                tracing::info!(
                    "重放最后一块命中终态墓碑: {transfer_id}, outcome={:?}",
                    tomb.outcome
                );
                let success = matches!(
                    tomb.outcome,
                    crate::transfer::registry::TransferOutcome::Completed { .. }
                );
                return Ok(ChunkResp {
                    success,
                    received_bytes: tomb.received_bytes,
                });
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

    Ok(ChunkResp {
        success: true,
        received_bytes: new_transferred,
    })
}

/// 完成传输：SHA256 校验临时文件，通过则重命名（处理冲突）+ 写历史；失败标记 failed。
///
/// Business Logic: 文件全部接收后需校验完整性，确保无误后落地为最终文件名。
/// Code Logic: 对照 Python `finalize_transfer`：
///     1. 仅处理 direction=Receive 的任务；
///     2. 计算 .tmp 的 SHA256，与任务记录的 sha256 比较；
///     3. 校验失败：标记 failed + 删除 .tmp + emit failed；
///     4. 校验通过：再校验 filename 为单组件，resolve_filename 解析冲突，
///        ensure 最终路径仍在 receive_dir 内，再重命名 .tmp → 最终路径；
///        标记 completed + 写历史 + emit completed。
pub async fn finalize_transfer(state: &AppState, transfer_id: &str) -> Result<(), AppError> {
    let task = match state.transfers.get(transfer_id) {
        Some(t) => t,
        None => return Ok(()),
    };

    // 防御：即便 chunk 漏检，finalize 也不得操作 Send 源文件（哈希失败会删、成功会 move）。
    if task.direction != TransferDirection::Receive {
        tracing::warn!(
            "finalize 拒绝非 Receive 任务: {transfer_id}, direction={:?}",
            task.direction
        );
        return Ok(());
    }

    let tmp_path = PathBuf::from(&task.file_path);

    // 校验 SHA256
    let actual = match compute_sha256(&tmp_path).await {
        Ok(h) => h,
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("读取临时文件失败: {e}")).await;
            return Ok(());
        }
    };

    if actual != task.sha256 {
        // 校验失败：删除损坏的临时文件
        let _ = tokio::fs::remove_file(&tmp_path).await;
        on_receive_failed(
            state,
            transfer_id,
            &format!("SHA256 校验失败: 期望={}, 实际={actual}", task.sha256),
        )
        .await;
        return Ok(());
    }

    // 解析文件名冲突并重命名；落盘前再次校验 basename 与 receive_dir 边界。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    let safe_filename = match sanitize_receive_basename(&task.filename, "filename") {
        Ok(name) => name,
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("非法文件名: {e}")).await;
            return Ok(());
        }
    };
    let final_filename = resolve_filename(&receive_dir, &safe_filename);
    // resolve_filename 可能产出 "stem (n).ext"；仍须是单组件。
    if let Err(e) = sanitize_receive_basename(&final_filename, "final_filename") {
        on_receive_failed(state, transfer_id, &format!("非法最终文件名: {e}")).await;
        return Ok(());
    }
    let final_path = receive_dir.join(&final_filename);
    if let Err(e) = ensure_path_within_dir(&receive_dir, &final_path) {
        on_receive_failed(state, transfer_id, &format!("最终路径非法: {e}")).await;
        return Ok(());
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
        on_receive_failed(state, transfer_id, &format!("重命名失败: {e}")).await;
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

    // Finding 4: 写入成功墓碑，重放的最后一块与 status 查询可还原成功结果。
    state.transfers.record_tombstone(
        transfer_id,
        crate::transfer::registry::TransferTombstone {
            outcome: crate::transfer::registry::TransferOutcome::Completed {
                final_filename: final_filename.clone(),
                file_path: final_path.to_string_lossy().to_string(),
            },
            received_bytes: task.size,
            size: task.size,
            filename: task.filename.clone(),
            completed_at: completed_at.clone(),
            created_at: std::time::Instant::now(),
        },
    );

    state.emit_event(
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
///
/// Business Logic（为什么需要这个函数）:
///     接收端运行在 HTTP 路由中，独立后端没有 GUI 句柄；失败事件仍需统一通知 GUI 或 headless adapter。
///
/// Code Logic（这个函数做什么）:
///     更新 registry/history 后通过 `AppState::emit_event` 发布 `transfer:failed`。
async fn on_receive_failed(state: &AppState, transfer_id: &str, error: &str) {
    let completed_at = now_iso();
    // 在移除前抓任务快照，用于写 Failed 墓碑（Finding 4）。
    let snapshot = state.transfers.get(transfer_id);
    state
        .transfers
        .mark_failed(transfer_id, completed_at.clone());
    if let Some(t) = state.transfers.get(transfer_id) {
        let _ = state.transfer_repo.record(&t).await;
    }
    state.transfers.remove(transfer_id);

    // Finding 4: 写失败墓碑，重放的最后一块返回 success:false，但与第一次 finalize 失败的结果一致，
    // 避免"第一次失败、重放变成功"的诡异步态。
    if let Some(t) = &snapshot {
        state.transfers.record_tombstone(
            transfer_id,
            crate::transfer::registry::TransferTombstone {
                outcome: crate::transfer::registry::TransferOutcome::Failed {
                    error: error.to_string(),
                },
                received_bytes: t.transferred_bytes,
                size: t.size,
                filename: t.filename.clone(),
                completed_at: completed_at.clone(),
                created_at: std::time::Instant::now(),
            },
        );
    }

    state.emit_event(
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
///     Finding 4: 任务终态后从 registry 移除，status 查询命中终态墓碑还原 completed/failed
///     与最终字节数，避免返回 "unknown" 让对端误判为丢失。
/// Code Logic: 对照 Python `get_transfer_status`，先查 registry，miss 时查墓碑，仍 miss 返回 unknown。
pub async fn handle_status(state: &AppState, transfer_id: &str) -> StatusResp {
    if let Some(t) = state.transfers.get(transfer_id) {
        return StatusResp {
            transfer_id: transfer_id.to_string(),
            status: status_str(t.status),
            progress: t.progress(),
            transferred_bytes: t.transferred_bytes,
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
    StatusResp {
        transfer_id: transfer_id.to_string(),
        status: "unknown".to_string(),
        progress: 0.0,
        transferred_bytes: 0,
        size: 0,
        filename: String::new(),
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
            ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo,
            TransferRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo, WorkbenchSessionRepo,
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

        AppState {
            config: Arc::new(RwLock::new(config)),
            db: pool.clone(),
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
            update_status: Arc::new(RwLock::new(
                crate::commands::updater::UpdateDownloadStatus::default(),
            )),
            update_pending: Arc::new(Mutex::new(None)),
            update_bytes: Arc::new(Mutex::new(None)),
            update_download_task: Arc::new(Mutex::new(None)),
            update_cancel_token: Arc::new(Mutex::new(None)),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
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
            cloud_sync_cancel: Arc::new(Mutex::new(None)),
            health: Arc::new(crate::health::HealthRuntime::new()),
            health_repo: Arc::new(crate::storage::health_repo::HealthRepo::new(pool.clone())),
            health_cancel: Arc::new(Mutex::new(None)),
            orchestrator_repo: Arc::new(OrchestratorRepo::new(pool)),
            orchestrator_scheduler_telemetry: OrchestratorSchedulerTelemetry::new(),
            orchestrator_cancel: Arc::new(Mutex::new(None)),
            orchestrator_outbox_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
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
}
