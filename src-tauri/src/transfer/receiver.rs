//! transfer/receiver.rs — 文件接收端逻辑（被 axum route 调用）
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
//! 临时文件命名 `.{transfer_id}.tmp` 与 Python 一致（断点续传识别）。
//! 文件名冲突处理（file.txt → file (1).txt → file (2).txt）与 Python `_resolve_filename` 一致。

use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use crate::transfer::CHUNK_SIZE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// finalize 落盘意图 journal 目录名（位于 receive_dir 下）。
const FINALIZE_INTENT_DIR: &str = ".cc-partner-transfer-intents";

/// 落盘前写入的 durable intent（跨进程恢复 place→history 窗口）。
///
/// Business Logic（为什么需要这个结构）:
///     最终文件可能已 place 成功，但 transfer_history 尚未写入；进程崩溃后内存 active/墓碑丢失，
///     若不保留 intent，handle_init 会接受同一 transfer_id 并产生后缀重复文件。
///
/// Code Logic（这个结构做什么）:
///     序列化为 receive_dir 下 `.cc-partner-transfer-intents/<transfer_id>.json`；
///     含 transfer 元数据与候选 final_path；history+墓碑成功后删除。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FinalizeIntent {
    transfer_id: String,
    filename: String,
    size: u64,
    sha256: String,
    chunk_size: u64,
    final_filename: String,
    final_path: String,
    created_at: String,
}

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
            // 父目录可能尚未创建：必须基于 *canonical_dir* 拼相对后缀。
            // macOS 上 `/var` 与 canonicalize 后的 `/private/var` 不一致，
            // 若对不存在父路径只做 normalize_path 会误判逃逸。
            if parent == dir {
                canonical_dir.clone()
            } else if let Ok(rel) = parent.strip_prefix(dir) {
                canonical_dir.join(rel)
            } else if let Ok(rel) = parent.strip_prefix(&canonical_dir) {
                canonical_dir.join(rel)
            } else {
                // 回退：相对路径拼到 canonical_dir；绝对且不在 dir 下则交给后续 starts_with 拒绝。
                let normalized = normalize_path(parent);
                if normalized.is_absolute() {
                    normalized
                } else {
                    canonical_dir.join(normalized)
                }
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

/// 完成传输：SHA256 校验临时文件，通过则原子落盘（处理冲突）+ 写历史；失败标记 failed。
///
/// Business Logic: 文件全部接收后需校验完整性，确保无误后落地为最终文件名。
///     并发同名不同 transfer_id 不得互相覆盖：resolve_filename 到 place 必须在 receive_dir 锁内，
///     并用 exclusive create + 失败重试后缀，禁止静默 replace。
///     落盘成功与 transfer_history 持久化之间不得出现“已 completed 但无 durable 终态”窗口：
///     否则崩溃/DB 失败会导致重启后 complete=false、status=unknown、发送端假失败并后缀重试。
/// Code Logic:
///     1. 仅处理 direction=Receive 的任务；
///     2. 若 active 已是 Completed（落盘成功但 history 未写入），只重试 durable 晋升；
///     3. 计算 .tmp 的 SHA256，与任务记录的 sha256 比较；
///     4. 校验失败：标记 failed + 删除 .tmp + emit failed；
///     5. 校验通过：持 receive_dir 锁，**每个候选**严格 resolve → 写 intent → no-replace place；
///     6. 先 mark_completed 保留 active（可恢复），再 durable record；成功后才 remove + 墓碑 + emit。
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

    // 落盘已成功但 history 未 durable：在同一绑定句柄上重做 len+SHA+fsync，并确认目录项身份未替换，
    // 再晋升，禁止二次 place 或假报 completed。
    if task.status == TransferStatus::Completed {
        return promote_completed_to_durable(state, transfer_id, &task).await;
    }

    let tmp_path = PathBuf::from(&task.file_path);

    // 校验 SHA256（no-follow：禁止 symlink tmp 把哈希指到 receive_dir 外任意文件）。
    let actual = match compute_sha256_nofollow(&tmp_path).await {
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

    // 解析文件名冲突并原子落盘；落盘前再次校验 basename 与 receive_dir 边界。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    let safe_filename = match sanitize_receive_basename(&task.filename, "filename") {
        Ok(name) => name,
        Err(e) => {
            on_receive_failed(state, transfer_id, &format!("非法文件名: {e}")).await;
            return Ok(());
        }
    };

    // receive_dir 锁覆盖 resolve → write intent → exclusive place，防止并发同名 TOCTOU 覆盖。
    let dir_lock = state.transfers.receive_dir_lock();
    let _dir_guard = dir_lock.lock().await;

    // 每个候选（首选与后缀）严格：resolve → 持久化 intent → no-replace place。
    // 禁止先 place 再补写 intent，也禁止 place 前删除 intent（place 成功但 history 前崩溃会丢 journal）。
    let placed = match place_final_file_with_intent(
        &receive_dir,
        &safe_filename,
        &tmp_path,
        transfer_id,
        &task,
    )
    .await
    {
        Ok(p) => p,
        Err(PlaceFinalError::DurabilityPending { placed, message }) => {
            // 文件已落地：保留 intent + Completed active，返回可重试错误，禁止 Failed history。
            tracing::error!(
                "receive place durability pending: {transfer_id} -> {} ({message})",
                placed.final_path.display()
            );
            let completed_at = now_iso();
            state.transfers.mark_completed(
                transfer_id,
                completed_at,
                Some(placed.final_path.to_string_lossy().to_string()),
            );
            return Err(AppError::unavailable(format!(
                "最终文件已落地但 durability 未完成，请重试 complete: {transfer_id}: {message}"
            )));
        }
        Err(PlaceFinalError::Unplaced(e)) => {
            on_receive_failed(state, transfer_id, &format!("原子落盘失败: {e}")).await;
            return Ok(());
        }
    };
    let placed_path = placed.final_path;
    tracing::debug!(
        "receive exclusive place ok: {transfer_id} -> {} ({})",
        placed.final_filename,
        placed_path.display()
    );

    // 先把 active 标为 Completed 并写入最终路径，但**不** remove / 不写墓碑 / 不 emit completed。
    // history 成功前对发送端仍表现为 transferring，complete/chunk 可重试 durable 晋升。
    let completed_at = now_iso();
    state.transfers.mark_completed(
        transfer_id,
        completed_at,
        Some(placed_path.to_string_lossy().to_string()),
    );
    let completed_task = state.transfers.get(transfer_id).ok_or_else(|| {
        AppError::generic(format!(
            "落盘后丢失 active 任务，无法写入 transfer_history: {transfer_id}"
        ))
    })?;
    promote_completed_to_durable(state, transfer_id, &completed_task).await
}

/// Business Logic（为什么需要这个函数）:
///     最终文件已落地后，只有 transfer_history 成功才可向发送端/UI 宣告 completed。
///     record 失败或崩溃窗口内必须保留可恢复 active，禁止“文件在、终态丢”。
///
/// Code Logic（这个函数做什么）:
///     要求 active 已是 Receive+Completed；`transfer_repo.record` 成功后才 remove、
///     写成功墓碑并 emit `transfer:completed`；record 失败保留 active 并返回 Err。
async fn promote_completed_to_durable(
    state: &AppState,
    transfer_id: &str,
    task: &TransferTask,
) -> Result<(), AppError> {
    if task.direction != TransferDirection::Receive {
        return Err(AppError::validation(format!(
            "仅 Receive 任务可晋升 durable 终态: {transfer_id}"
        )));
    }
    if task.status != TransferStatus::Completed {
        return Err(AppError::generic(format!(
            "任务尚未 Completed，不能晋升 durable: {transfer_id}"
        )));
    }

    // 写 Completed history 前必须在同一绑定句柄上证明 len/SHA/fsync，并确认父目录项身份未替换。
    // 禁止仅按路径 reopen+fsync：普通文件原子替换后 no-follow 仍会通过，导致静默错误认证。
    let final_path = PathBuf::from(&task.file_path);
    if let Err(e) = certify_final_file_for_history(&final_path, task.size, &task.sha256).await {
        return Err(AppError::unavailable(format!(
            "最终文件已落地但 durability 未完成，请重试 complete: {transfer_id}: {e}"
        )));
    }

    state.transfer_repo.record(task).await.map_err(|e| {
        tracing::error!("transfer_history 写入失败，保留 active 等待重试: {transfer_id}, {e}");
        e
    })?;

    let final_path_str = final_path.to_string_lossy().to_string();
    let final_filename = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(task.filename.as_str())
        .to_string();
    let completed_at = task.completed_at.clone().unwrap_or_else(now_iso);

    state.transfers.remove(transfer_id);
    state.transfers.record_tombstone(
        transfer_id,
        crate::transfer::registry::TransferTombstone {
            outcome: crate::transfer::registry::TransferOutcome::Completed {
                final_filename,
                file_path: final_path_str.clone(),
            },
            received_bytes: task.size,
            size: task.size,
            filename: task.filename.clone(),
            completed_at,
            created_at: std::time::Instant::now(),
        },
    );

    // history + 墓碑已就绪：清除跨崩溃 intent journal。
    let receive_dir = PathBuf::from(&state.config.read().expect("config 读锁中毒").receive_dir);
    if let Err(e) = clear_finalize_intent(&receive_dir, transfer_id).await {
        tracing::warn!("清除 finalize intent 失败（可忽略）: {transfer_id}, {e}");
    }

    state.emit_event(
        "transfer:completed",
        serde_json::json!({
            "id": transfer_id,
            "status": "completed",
            "filePath": final_path_str,
        }),
    );

    tracing::info!("文件接收完成: {transfer_id} -> {final_path_str}");
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     place 前必须把候选 final 路径持久化到 journal，才能在进程崩溃后避免同 transfer_id 重传后缀副本。
///     intent 目录/文件若是 symlink/reparse，普通 create_dir_all/write 会跟随写出 receive_dir；
///     路径检查后再 create/rename 仍有父目录交换 TOCTOU，必须绑定已验证目录 HANDLE/fd。
///
/// Code Logic（这个函数做什么）:
///     Unix：open 已验证 receive_dir fd → openat/mkdirat 绑定 intent 目录 → openat 创建 tmp 写字节
///     + sync_all → renameat 正式 intent → fsync intent 目录；
///     Windows：CreateFile 打开 receive_dir（BACKUP_SEMANTICS）→ NtCreateFile 相对路径创建/打开
///     普通 intent 目录（OPEN_REPARSE_POINT 后拒绝 reparse）→ 相对路径 create/write/
///     NtSetInformationFile(FileRenameInformation=10, RootDirectory=intent HANDLE)/unlink，
///     检查后不再用绝对 path 做 create/rename/delete。
async fn write_finalize_intent(
    receive_dir: &Path,
    intent: &FinalizeIntent,
) -> Result<(), AppError> {
    // 词法边界校验（basename/逃逸）；真正写入走 dirfd / directory HANDLE，不再 path-ops。
    let _path = finalize_intent_path(receive_dir, &intent.transfer_id)?;
    let safe_id = sanitize_receive_basename(&intent.transfer_id, "transfer_id")?;
    let intent_name = format!("{safe_id}.json");
    let tmp_name = format!("{safe_id}.json.tmp");
    let bytes = serde_json::to_vec_pretty(intent)?;

    #[cfg(unix)]
    {
        let joined = tokio::task::spawn_blocking({
            let receive_dir = receive_dir.to_path_buf();
            let intent_name = intent_name.clone();
            let tmp_name = tmp_name.clone();
            let bytes = bytes.clone();
            move || write_finalize_intent_unix_dirfd(&receive_dir, &intent_name, &tmp_name, &bytes)
        })
        .await
        .map_err(|e| AppError::generic(format!("write_finalize_intent join 失败: {e}")))?;
        joined
    }

    #[cfg(windows)]
    {
        let joined = tokio::task::spawn_blocking({
            let receive_dir = receive_dir.to_path_buf();
            let intent_name = intent_name.clone();
            let tmp_name = tmp_name.clone();
            let bytes = bytes.clone();
            move || {
                write_finalize_intent_windows_handle(&receive_dir, &intent_name, &tmp_name, &bytes)
            }
        })
        .await
        .map_err(|e| AppError::generic(format!("write_finalize_intent join 失败: {e}")))?;
        joined
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (receive_dir, intent_name, tmp_name, bytes);
        Err(AppError::generic(
            "当前平台无法以目录句柄相对路径写 finalize intent".to_string(),
        ))
    }
}

/// Business Logic（为什么需要这个函数）:
///     finalize intent 目录若被替换为指向 receive_dir 外的 symlink，create_dir_all/write 会跟随逃逸。
///
/// Code Logic（这个函数做什么）:
///     对 receive_dir 下 FINALIZE_INTENT_DIR：不存在则 create_dir（目录项本身）；存在则 symlink_metadata
///     拒绝 symlink/非目录；返回校验后的 path。
///     生产写路径已改用 Unix dirfd / Windows directory HANDLE；本 helper 仅保留给单测与诊断。
#[allow(dead_code)]
async fn ensure_regular_intent_dir(receive_dir: &Path) -> Result<PathBuf, AppError> {
    let dir = receive_dir.join(FINALIZE_INTENT_DIR);
    ensure_path_within_dir(receive_dir, &dir)?;
    match tokio::fs::symlink_metadata(&dir).await {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                return Err(AppError::validation(format!(
                    "intent 目录是符号链接，拒绝写入: {}",
                    dir.display()
                )));
            }
            if !ft.is_dir() {
                return Err(AppError::validation(format!(
                    "intent 路径不是目录: {}",
                    dir.display()
                )));
            }
            Ok(dir)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            match tokio::fs::create_dir(&dir).await {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    return Box::pin(ensure_regular_intent_dir(receive_dir)).await;
                }
                Err(err) => return Err(AppError::from(err)),
            }
            match tokio::fs::symlink_metadata(&dir).await {
                Ok(meta) => {
                    let ft = meta.file_type();
                    if ft.is_symlink() || !ft.is_dir() {
                        return Err(AppError::validation(format!(
                            "intent 目录创建后不是普通目录: {}",
                            dir.display()
                        )));
                    }
                    Ok(dir)
                }
                Err(err) => Err(AppError::from(err)),
            }
        }
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     写 intent 临时文件时若路径已是指向外部的 symlink，tokio::fs::write 会跟随并截断外部文件。
///     create_new 后若关闭再按路径重开，攻击者可在两次 open 之间把临时文件换成 hardlink/普通文件。
///
/// Code Logic（这个函数做什么）:
///     先 remove 残留同名 tmp；create_new 打开后不丢弃句柄，write_all + flush + sync_all。
///     生产 intent 写入已改用目录句柄相对路径；本 helper 供单测验证 create_new 语义。
#[allow(dead_code)]
async fn write_bytes_create_new_nofollow(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let _ = tokio::fs::remove_file(path).await;
    let created = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            return Err(AppError::from(std::io::Error::new(
                e.kind(),
                format!("创建 intent 临时文件失败 {}: {e}", path.display()),
            )));
        }
    };
    let mut file = tokio::fs::File::from_std(created);
    if let Err(err) = file.write_all(bytes).await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    if let Err(err) = file.flush().await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    if let Err(err) = file.sync_all().await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(AppError::from(err));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     rename/hardlink/unlink 后若不同步父目录，断电可能丢失目录项，而 SQLite history 已 completed。
///
/// Code Logic（这个函数做什么）:
///     打开目录并 fsync（Unix）或 FlushFileBuffers（Windows）；失败上抛。
///     Windows 必须用可写目录句柄（GENERIC_WRITE / write access）：`FlushFileBuffers`
///     要求写权限，只读句柄会 AccessDenied，导致 durability 永远无法晋升。
fn fsync_dir(dir: &Path) -> Result<(), AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::File::open(dir).map_err(|e| {
            AppError::from(std::io::Error::new(
                e.kind(),
                format!("打开目录以 fsync 失败 {}: {e}", dir.display()),
            ))
        })?;
        let rc = unsafe { libc::fsync(file.as_raw_fd()) };
        if rc != 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS 才能打开目录句柄；OPEN_REPARSE_POINT 拒绝跟随 junction。
        // write(true) 申请 GENERIC_WRITE，满足 FlushFileBuffers 权限要求。
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir)
            .map_err(|e| {
                AppError::from(std::io::Error::new(
                    e.kind(),
                    format!("打开目录以 FlushFileBuffers 失败 {}: {e}", dir.display()),
                ))
            })?;
        // 目录 HANDLE 上 sync_all ≈ FlushFileBuffers（需 GENERIC_WRITE）。
        file.sync_all().map_err(AppError::from)?;
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Business Logic（为什么需要这个函数）:
///     place 前必须把接收 tmp 的数据刷到稳定存储，否则断电可能只剩 history completed 而文件内容丢失。
///
/// Code Logic（这个函数做什么）:
///     no-follow 以可写方式打开普通文件后 sync_all（含元数据）。
///     Windows 上 `sync_all` → FlushFileBuffers 需要 GENERIC_WRITE，只读句柄会 AccessDenied。
async fn sync_regular_file(path: &Path) -> Result<(), AppError> {
    let file = open_regular_file_nofollow(path, true).await?;
    // tokio File sync_all
    let std_file = file.into_std().await;
    std_file.sync_all().map_err(AppError::from)?;
    Ok(())
}

/// Unix：在已验证 receive_dir fd 上 openat/mkdirat/renameat 写 intent，关闭父目录交换 TOCTOU。
#[cfg(unix)]
fn write_finalize_intent_unix_dirfd(
    receive_dir: &Path,
    intent_name: &str,
    tmp_name: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let receive_c = CString::new(receive_dir.as_os_str().as_bytes())
        .map_err(|_| AppError::validation("receive_dir 含内部 NUL"))?;
    let intent_dir_c =
        CString::new(FINALIZE_INTENT_DIR).map_err(|_| AppError::validation("intent dir 名非法"))?;
    let intent_c = CString::new(intent_name.as_bytes())
        .map_err(|_| AppError::validation("intent 文件名含内部 NUL"))?;
    let tmp_c = CString::new(tmp_name.as_bytes())
        .map_err(|_| AppError::validation("intent tmp 名含内部 NUL"))?;

    // O_DIRECTORY|O_RDONLY；若 receive_dir 是 symlink，open 跟随其目标——配置路径可信，关键是 intent 子项不逃逸。
    let receive_fd = unsafe {
        libc::open(
            receive_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if receive_fd < 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let receive_file = unsafe { std::fs::File::from_raw_fd(receive_fd) };

    // mkdirat intent 目录；EEXIST 时 openat 校验为普通目录。
    let mkdir_rc = unsafe { libc::mkdirat(receive_file.as_raw_fd(), intent_dir_c.as_ptr(), 0o700) };
    if mkdir_rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EEXIST) {
            return Err(AppError::from(err));
        }
    }
    let intent_dir_fd = unsafe {
        libc::openat(
            receive_file.as_raw_fd(),
            intent_dir_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if intent_dir_fd < 0 {
        let err = std::io::Error::last_os_error();
        // O_NOFOLLOW 遇 symlink → ELOOP/EPERM
        return Err(AppError::validation(format!(
            "intent 目录是符号链接或不可绑定的普通目录: {err}"
        )));
    }
    let intent_dir_file = unsafe { std::fs::File::from_raw_fd(intent_dir_fd) };

    // 清残留 tmp 目录项（unlinkat 不跟随）。
    let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };

    let tmp_fd = unsafe {
        libc::openat(
            intent_dir_file.as_raw_fd(),
            tmp_c.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if tmp_fd < 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let mut tmp_file = unsafe { std::fs::File::from_raw_fd(tmp_fd) };
    if let Err(err) = tmp_file.write_all(bytes) {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    if let Err(err) = tmp_file.flush() {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    if let Err(err) = tmp_file.sync_all() {
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    drop(tmp_file);

    // 覆盖正式 intent：先 unlink 旧目录项，再 renameat。
    let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), intent_c.as_ptr(), 0) };
    let rename_rc = unsafe {
        libc::renameat(
            intent_dir_file.as_raw_fd(),
            tmp_c.as_ptr(),
            intent_dir_file.as_raw_fd(),
            intent_c.as_ptr(),
        )
    };
    if rename_rc != 0 {
        let err = std::io::Error::last_os_error();
        let _ = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), tmp_c.as_ptr(), 0) };
        return Err(AppError::from(err));
    }
    // fsync intent 目录，保证 rename 目录项断电后可见。
    let rc = unsafe { libc::fsync(intent_dir_file.as_raw_fd()) };
    if rc != 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    // 保持 receive_file 活到此处，避免 fd 过早关闭。
    drop(intent_dir_file);
    drop(receive_file);
    Ok(())
}

/// Windows：绑定 receive_dir / intent 目录 HANDLE，用相对路径 create/rename/unlink，
/// 拒绝 reparse/junction，关闭检查后路径操作 TOCTOU。
///
/// Business Logic（为什么需要这个函数）:
///     Windows 上 path-check-then-path-ops 可在校验后把 intent 目录换成 junction/reparse，
///     使 create/rename/delete 逃逸 receive_dir；必须相对已验证目录 HANDLE 操作。
///
/// Code Logic（这个函数做什么）:
///     1) CreateFileW 打开 receive_dir（BACKUP_SEMANTICS，不跟随 reparse 打开目录自身后拒绝 reparse）；
///     2) NtCreateFile 相对 receive HANDLE 创建/打开 intent 子目录（OPEN_REPARSE_POINT 后拒绝 reparse）；
///     3) 相对 intent HANDLE 删除旧 tmp/intent 目录项、CREATE_NEW 写 tmp、FlushFileBuffers、
///        NtSetInformationFile(FileRenameInformation=10, RootDirectory=intent HANDLE) 原子改名；
///     4) FlushFileBuffers(intent 目录)。
#[cfg(windows)]
fn write_finalize_intent_windows_handle(
    receive_dir: &Path,
    intent_name: &str,
    tmp_name: &str,
    bytes: &[u8],
) -> Result<(), AppError> {
    use std::ffi::c_void;
    use std::io::{ErrorKind, Write};
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
    use std::ptr;

    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut c_void;
    type NTSTATUS = i32;
    type ULONG = u32;
    type USHORT = u16;
    type ACCESS_MASK = u32;

    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
    const FILE_SHARE_READ: DWORD = 0x0000_0001;
    const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
    const FILE_SHARE_DELETE: DWORD = 0x0000_0004;
    const OPEN_EXISTING: DWORD = 3;
    const GENERIC_READ: DWORD = 0x8000_0000;
    const GENERIC_WRITE: DWORD = 0x4000_0000;
    const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;
    const FILE_ATTRIBUTE_NORMAL: DWORD = 0x80;
    const ERROR_ALREADY_EXISTS: i32 = 183;
    const ERROR_FILE_EXISTS: i32 = 80;
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PATH_NOT_FOUND: i32 = 3;
    const FILE_OPEN: ULONG = 0x0000_0001;
    const FILE_CREATE: ULONG = 0x0000_0002;
    const FILE_OPEN_IF: ULONG = 0x0000_0003;
    const FILE_DIRECTORY_FILE: ULONG = 0x0000_0001;
    const FILE_NON_DIRECTORY_FILE: ULONG = 0x0000_0040;
    const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x0000_0020;
    const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
    const FILE_DELETE_ON_CLOSE: ULONG = 0x0000_1000;
    const DELETE: ACCESS_MASK = 0x0001_0000;
    const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x0001;
    const FILE_ADD_FILE: ACCESS_MASK = 0x0002;
    const FILE_ADD_SUBDIRECTORY: ACCESS_MASK = 0x0004;
    const FILE_WRITE_DATA: ACCESS_MASK = 0x0002;
    const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x0080;
    const FILE_WRITE_ATTRIBUTES: ACCESS_MASK = 0x0100;
    const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
    const FILE_GENERIC_WRITE: ACCESS_MASK =
        GENERIC_WRITE | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
    // NtSetInformationFile 的 FileRenameInformation = 10（不是 Win32 FileRenameInfo=3）。
    const FILE_RENAME_INFORMATION_CLASS: ULONG = 10;
    const OBJ_CASE_INSENSITIVE: ULONG = 0x0000_0040;
    const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035u32 as NTSTATUS;
    const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034u32 as NTSTATUS;
    const STATUS_OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003Au32 as NTSTATUS;
    const STATUS_DELETE_PENDING: NTSTATUS = 0xC000_0056u32 as NTSTATUS;

    #[repr(C)]
    struct UnicodeString {
        length: USHORT,
        maximum_length: USHORT,
        buffer: *mut u16,
    }

    #[repr(C)]
    struct ObjectAttributes {
        length: ULONG,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: ULONG,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }

    #[repr(C)]
    struct IoStatusBlock {
        status: NTSTATUS,
        information: usize,
    }

    /// NtSetInformationFile(FileRenameInformation) 缓冲布局：BOOLEAN + 对齐后的 RootDirectory。
    #[repr(C)]
    struct FileRenameInformation {
        replace_if_exists: u8, // BOOLEAN
        root_directory: HANDLE,
        file_name_length: ULONG,
        // file_name: [u16; 1] follows in buffer
        file_name: [u16; 1],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: DWORD,
            dw_share_mode: DWORD,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: DWORD,
            dw_flags_and_attributes: DWORD,
            h_template_file: HANDLE,
        ) -> HANDLE;
        fn GetFileInformationByHandle(
            h_file: HANDLE,
            lp_file_information: *mut ByHandleFileInformation,
        ) -> BOOL;
        fn FlushFileBuffers(h_file: HANDLE) -> BOOL;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: ACCESS_MASK,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: ULONG,
            share_access: ULONG,
            create_disposition: ULONG,
            create_options: ULONG,
            ea_buffer: *mut c_void,
            ea_length: ULONG,
        ) -> NTSTATUS;
        fn NtSetInformationFile(
            file_handle: HANDLE,
            io_status_block: *mut IoStatusBlock,
            file_information: *mut c_void,
            length: ULONG,
            file_information_class: ULONG,
        ) -> NTSTATUS;
        fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: DWORD,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        volume_serial_number: DWORD,
        file_size_high: DWORD,
        file_size_low: DWORD,
        number_of_links: DWORD,
        file_index_high: DWORD,
        file_index_low: DWORD,
    }

    /// Business Logic: 路径必须是合法宽字符串，供 CreateFileW / NtCreateFile 使用。
    /// Code Logic: encode_wide + 拒绝内部 NUL + 追加 NUL 终止符。
    fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("路径含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }

    /// Business Logic: 相对路径组件名（intent 目录/文件）同样需宽字符串。
    /// Code Logic: 按 UTF-8 字节转宽字符；拒绝空串与内部 NUL。
    fn name_to_wide(name: &str) -> Result<Vec<u16>, AppError> {
        if name.is_empty() || name.contains('\0') {
            return Err(AppError::validation("相对路径名非法".to_string()));
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("相对路径名含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }

    /// Business Logic: NTSTATUS 非成功时映射为 Win32 错误，便于上层处理 AlreadyExists/NotFound。
    /// Code Logic: status >= 0 成功；否则 RtlNtStatusToDosError。
    fn nt_ok(status: NTSTATUS) -> Result<(), std::io::Error> {
        if status >= 0 {
            Ok(())
        } else {
            let dos = unsafe { RtlNtStatusToDosError(status) };
            Err(std::io::Error::from_raw_os_error(dos as i32))
        }
    }

    /// Business Logic: 目录 HANDLE 必须是普通目录，不能是 reparse/junction。
    /// Code Logic: GetFileInformationByHandle 检查 DIRECTORY 且无 REPARSE_POINT。
    fn ensure_plain_directory_handle(handle: HANDLE) -> Result<(), AppError> {
        let mut info: ByHandleFileInformation = unsafe { zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(AppError::validation(
                "intent 路径不是目录（句柄绑定失败）".to_string(),
            ));
        }
        if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::validation(
                "intent 目录是 reparse/junction，拒绝写入".to_string(),
            ));
        }
        Ok(())
    }

    /// Business Logic: 相对父目录 HANDLE 打开/创建子对象，避免绝对 path 在校验后被交换。
    /// Code Logic: 组装 UNICODE_STRING + OBJECT_ATTRIBUTES，调用 NtCreateFile。
    unsafe fn open_relative(
        parent: HANDLE,
        name_wide: &mut [u16],
        desired_access: ACCESS_MASK,
        create_disposition: ULONG,
        create_options: ULONG,
        file_attributes: ULONG,
    ) -> Result<OwnedHandle, AppError> {
        // name_wide 含结尾 NUL；Length 不含 NUL。
        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        if byte_len > u16::MAX as usize {
            return Err(AppError::validation("相对路径名过长".to_string()));
        }
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = zeroed();
        let mut out: HANDLE = ptr::null_mut();
        let status = NtCreateFile(
            &mut out,
            desired_access,
            &mut attrs,
            &mut iosb,
            ptr::null_mut(),
            file_attributes,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            create_disposition,
            create_options,
            ptr::null_mut(),
            0,
        );
        if let Err(err) = nt_ok(status) {
            // 统一 AlreadyExists 语义，供调用方判断。
            if status == STATUS_OBJECT_NAME_COLLISION
                || err.raw_os_error() == Some(ERROR_ALREADY_EXISTS)
                || err.raw_os_error() == Some(ERROR_FILE_EXISTS)
            {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::AlreadyExists,
                    err,
                )));
            }
            if status == STATUS_OBJECT_NAME_NOT_FOUND
                || status == STATUS_OBJECT_PATH_NOT_FOUND
                || err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND)
                || err.raw_os_error() == Some(ERROR_PATH_NOT_FOUND)
            {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::NotFound,
                    err,
                )));
            }
            return Err(AppError::from(err));
        }
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "NtCreateFile 返回无效 HANDLE".to_string(),
            ));
        }
        Ok(OwnedHandle::from_raw_handle(out as RawHandle))
    }

    /// Business Logic: 删除 intent 目录项必须相对目录 HANDLE，不能再走绝对 path remove。
    /// Code Logic: 以 DELETE|FILE_DELETE_ON_CLOSE 打开后立即 drop（关闭时删除）。
    unsafe fn unlink_relative(parent: HANDLE, name_wide: &mut [u16]) -> Result<(), AppError> {
        match open_relative(
            parent,
            name_wide,
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_DELETE_ON_CLOSE
                | FILE_OPEN_REPARSE_POINT,
            0,
        ) {
            Ok(handle) => {
                drop(handle);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("os error 2")
                    || msg.contains("os error 3")
                    || msg.contains("NotFound")
                    || msg.contains("找不到")
                {
                    Ok(())
                } else {
                    // 也接受 std NotFound kind。
                    Err(e)
                }
            }
        }
    }

    /// Business Logic: rename 必须相对已验证 intent 目录 HANDLE，禁止 basename 相对 CWD 解析。
    /// Code Logic: NtSetInformationFile(FileRenameInformation=10) + RootDirectory=intent HANDLE
    ///     + ReplaceIfExists=TRUE；禁止 Win32 FileRenameInfo 信息类或 RootDirectory=NULL。
    unsafe fn rename_relative(
        file: HANDLE,
        intent_dir: HANDLE,
        new_name_wide: &[u16],
    ) -> Result<(), AppError> {
        if intent_dir.is_null() || intent_dir == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "intent 目录 HANDLE 无效，拒绝 rename".to_string(),
            ));
        }
        let name_units = new_name_wide.len().saturating_sub(1);
        let name_bytes = name_units * 2;
        // 结构体 + 文件名（不含内嵌的 1 个 wchar，需额外 (name_units-1)）
        let extra = name_units.saturating_sub(1) * 2;
        let total = size_of::<FileRenameInformation>() + extra;
        let mut buf = vec![0u8; total];
        let info = buf.as_mut_ptr() as *mut FileRenameInformation;
        (*info).replace_if_exists = 1;
        // 关键：绑定已验证 intent 目录 HANDLE，basename 不得相对进程 CWD。
        (*info).root_directory = intent_dir;
        (*info).file_name_length = name_bytes as ULONG;
        ptr::copy_nonoverlapping(
            new_name_wide.as_ptr(),
            (*info).file_name.as_mut_ptr(),
            name_units,
        );
        let mut iosb: IoStatusBlock = zeroed();
        let status = NtSetInformationFile(
            file,
            &mut iosb,
            buf.as_mut_ptr() as *mut c_void,
            total as ULONG,
            FILE_RENAME_INFORMATION_CLASS,
        );
        nt_ok(status).map_err(AppError::from)
    }

    let receive_wide = to_wide(receive_dir)?;
    let mut intent_dir_wide = name_to_wide(FINALIZE_INTENT_DIR)?;
    let mut intent_name_wide = name_to_wide(intent_name)?;
    let mut tmp_name_wide = name_to_wide(tmp_name)?;

    // 打开 receive_dir 自身（BACKUP_SEMANTICS + OPEN_REPARSE_POINT），再拒绝 reparse。
    let receive_raw = unsafe {
        CreateFileW(
            receive_wide.as_ptr(),
            GENERIC_READ | FILE_LIST_DIRECTORY | FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if receive_raw.is_null() || receive_raw == INVALID_HANDLE_VALUE {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }
    let receive_handle = unsafe { OwnedHandle::from_raw_handle(receive_raw as RawHandle) };
    ensure_plain_directory_handle(receive_handle.as_raw_handle() as HANDLE)?;

    // 相对 receive HANDLE 创建/打开 intent 目录；OPEN_REPARSE_POINT 后拒绝 reparse。
    let intent_dir_handle = match unsafe {
        open_relative(
            receive_handle.as_raw_handle() as HANDLE,
            &mut intent_dir_wide,
            GENERIC_READ
                | FILE_LIST_DIRECTORY
                | FILE_ADD_FILE
                | FILE_ADD_SUBDIRECTORY
                | FILE_WRITE_ATTRIBUTES
                | SYNCHRONIZE
                | DELETE,
            FILE_OPEN_IF,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            FILE_ATTRIBUTE_DIRECTORY,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            return Err(AppError::validation(format!(
                "无法绑定 intent 目录 HANDLE: {e}"
            )));
        }
    };
    ensure_plain_directory_handle(intent_dir_handle.as_raw_handle() as HANDLE)?;

    let parent = intent_dir_handle.as_raw_handle() as HANDLE;

    // 清残留 tmp 目录项（相对 unlink，不跟随 reparse 目标内容）。
    let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };

    // CREATE_NEW 相对创建 tmp 普通文件；立即 into_raw 交给 File，避免与 OwnedHandle double-close。
    let tmp_owned = unsafe {
        open_relative(
            parent,
            &mut tmp_name_wide,
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            FILE_ATTRIBUTE_NORMAL,
        )
    }
    .map_err(|e| {
        AppError::from(std::io::Error::new(
            ErrorKind::Other,
            format!("创建 intent 临时文件失败: {e}"),
        ))
    })?;
    let mut tmp_file = unsafe { std::fs::File::from_raw_handle(tmp_owned.into_raw_handle()) };
    if let Err(err) = (|| -> Result<(), AppError> {
        tmp_file.write_all(bytes).map_err(AppError::from)?;
        tmp_file.flush().map_err(AppError::from)?;
        tmp_file.sync_all().map_err(AppError::from)?;
        Ok(())
    })() {
        // 写失败：关闭句柄后相对 unlink 残留 tmp。
        drop(tmp_file);
        let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };
        return Err(err);
    }

    // 删除旧正式 intent 目录项（若存在），再把 tmp rename 为正式名。
    // rename 需要仍打开的文件 HANDLE。
    let _ = unsafe { unlink_relative(parent, &mut intent_name_wide) };
    let tmp_raw = tmp_file.into_raw_handle();
    let tmp_handle = unsafe { OwnedHandle::from_raw_handle(tmp_raw) };
    if let Err(err) = unsafe {
        rename_relative(
            tmp_handle.as_raw_handle() as HANDLE,
            parent,
            &intent_name_wide,
        )
    } {
        drop(tmp_handle);
        let _ = unsafe { unlink_relative(parent, &mut tmp_name_wide) };
        return Err(err);
    }
    drop(tmp_handle);

    // FlushFileBuffers(intent 目录)，保证 rename 目录项断电后可见。
    let flush_ok = unsafe { FlushFileBuffers(parent) };
    if flush_ok == 0 {
        return Err(AppError::from(std::io::Error::last_os_error()));
    }

    drop(intent_dir_handle);
    drop(receive_handle);
    let _ = (STATUS_DELETE_PENDING, FILE_OPEN);
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     durable history + 墓碑成功后必须删除 intent，避免后续误恢复。
///     删除同样不能在 path-check 后再用绝对 path remove（父目录可被换成 reparse）。
///
/// Code Logic（这个函数做什么）:
///     Unix：receive_dir fd + openat intent 目录 + unlinkat；Windows：目录 HANDLE 相对 unlink；
///     其它平台：词法路径 remove（无 dirfd 原语时的降级，生产目标平台为 unix/windows）。
async fn clear_finalize_intent(receive_dir: &Path, transfer_id: &str) -> Result<(), AppError> {
    // 边界校验副作用（非法 id / 逃逸）。
    let _ = finalize_intent_path(receive_dir, transfer_id)?;
    let safe_id = sanitize_receive_basename(transfer_id, "transfer_id")?;
    let intent_name = format!("{safe_id}.json");

    #[cfg(unix)]
    {
        let receive_dir = receive_dir.to_path_buf();
        let intent_name = intent_name.clone();
        tokio::task::spawn_blocking(move || {
            clear_finalize_intent_unix_dirfd(&receive_dir, &intent_name)
        })
        .await
        .map_err(|e| AppError::generic(format!("clear_finalize_intent join 失败: {e}")))?
    }

    #[cfg(windows)]
    {
        let receive_dir = receive_dir.to_path_buf();
        let intent_name = intent_name.clone();
        tokio::task::spawn_blocking(move || {
            clear_finalize_intent_windows_handle(&receive_dir, &intent_name)
        })
        .await
        .map_err(|e| AppError::generic(format!("clear_finalize_intent join 失败: {e}")))?
    }

    #[cfg(not(any(unix, windows)))]
    {
        let path = finalize_intent_path(receive_dir, transfer_id)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::from(e)),
        }
    }
}

/// Unix：相对 receive_dir / intent 目录 fd unlinkat 删除 intent。
#[cfg(unix)]
fn clear_finalize_intent_unix_dirfd(receive_dir: &Path, intent_name: &str) -> Result<(), AppError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::{AsRawFd, FromRawFd};

    let receive_c = CString::new(receive_dir.as_os_str().as_bytes())
        .map_err(|_| AppError::validation("receive_dir 含内部 NUL"))?;
    let intent_dir_c =
        CString::new(FINALIZE_INTENT_DIR).map_err(|_| AppError::validation("intent dir 名非法"))?;
    let intent_c = CString::new(intent_name.as_bytes())
        .map_err(|_| AppError::validation("intent 文件名含内部 NUL"))?;

    let receive_fd = unsafe {
        libc::open(
            receive_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if receive_fd < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let receive_file = unsafe { std::fs::File::from_raw_fd(receive_fd) };

    let intent_dir_fd = unsafe {
        libc::openat(
            receive_file.as_raw_fd(),
            intent_dir_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if intent_dir_fd < 0 {
        let err = std::io::Error::last_os_error();
        // 目录不存在 / 是 symlink → 视为无 intent 可清。
        if err.kind() == ErrorKind::NotFound
            || err.raw_os_error() == Some(libc::ELOOP)
            || err.raw_os_error() == Some(libc::EPERM)
        {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let intent_dir_file = unsafe { std::fs::File::from_raw_fd(intent_dir_fd) };
    let rc = unsafe { libc::unlinkat(intent_dir_file.as_raw_fd(), intent_c.as_ptr(), 0) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != ErrorKind::NotFound {
            return Err(AppError::from(err));
        }
    }
    drop(intent_dir_file);
    drop(receive_file);
    Ok(())
}

/// Windows：相对目录 HANDLE 删除 intent 文件，拒绝 reparse 目录上的 path-ops。
#[cfg(windows)]
fn clear_finalize_intent_windows_handle(
    receive_dir: &Path,
    intent_name: &str,
) -> Result<(), AppError> {
    // 复用写路径的打开语义：绑定 plain directory HANDLE 后相对 unlink。
    // 为避免重复大段 FFI，这里走“打开 intent 目录 HANDLE + 相对 DELETE_ON_CLOSE”。
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::ptr;

    type BOOL = i32;
    type DWORD = u32;
    type HANDLE = *mut c_void;
    type NTSTATUS = i32;
    type ULONG = u32;
    type USHORT = u16;
    type ACCESS_MASK = u32;

    const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
    const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
    const FILE_SHARE_READ: DWORD = 0x1;
    const FILE_SHARE_WRITE: DWORD = 0x2;
    const FILE_SHARE_DELETE: DWORD = 0x4;
    const OPEN_EXISTING: DWORD = 3;
    const GENERIC_READ: DWORD = 0x8000_0000;
    const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;
    const FILE_OPEN: ULONG = 0x1;
    const FILE_DIRECTORY_FILE: ULONG = 0x1;
    const FILE_NON_DIRECTORY_FILE: ULONG = 0x40;
    const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x20;
    const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
    const FILE_DELETE_ON_CLOSE: ULONG = 0x1000;
    const DELETE: ACCESS_MASK = 0x0001_0000;
    const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x1;
    const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x80;
    const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
    const OBJ_CASE_INSENSITIVE: ULONG = 0x40;

    #[repr(C)]
    struct UnicodeString {
        length: USHORT,
        maximum_length: USHORT,
        buffer: *mut u16,
    }
    #[repr(C)]
    struct ObjectAttributes {
        length: ULONG,
        root_directory: HANDLE,
        object_name: *mut UnicodeString,
        attributes: ULONG,
        security_descriptor: *mut c_void,
        security_quality_of_service: *mut c_void,
    }
    #[repr(C)]
    struct IoStatusBlock {
        status: NTSTATUS,
        information: usize,
    }
    #[repr(C)]
    struct ByHandleFileInformation {
        file_attributes: DWORD,
        creation_time: u64,
        last_access_time: u64,
        last_write_time: u64,
        volume_serial_number: DWORD,
        file_size_high: DWORD,
        file_size_low: DWORD,
        number_of_links: DWORD,
        file_index_high: DWORD,
        file_index_low: DWORD,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: DWORD,
            dw_share_mode: DWORD,
            lp_security_attributes: *mut c_void,
            dw_creation_disposition: DWORD,
            dw_flags_and_attributes: DWORD,
            h_template_file: HANDLE,
        ) -> HANDLE;
        fn GetFileInformationByHandle(
            h_file: HANDLE,
            lp_file_information: *mut ByHandleFileInformation,
        ) -> BOOL;
    }
    #[link(name = "ntdll")]
    extern "system" {
        fn NtCreateFile(
            file_handle: *mut HANDLE,
            desired_access: ACCESS_MASK,
            object_attributes: *mut ObjectAttributes,
            io_status_block: *mut IoStatusBlock,
            allocation_size: *mut i64,
            file_attributes: ULONG,
            share_access: ULONG,
            create_disposition: ULONG,
            create_options: ULONG,
            ea_buffer: *mut c_void,
            ea_length: ULONG,
        ) -> NTSTATUS;
        fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
    }

    fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("路径含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }
    fn name_to_wide(name: &str) -> Result<Vec<u16>, AppError> {
        if name.is_empty() || name.contains('\0') {
            return Err(AppError::validation("相对路径名非法".to_string()));
        }
        let mut wide: Vec<u16> = std::ffi::OsStr::new(name).encode_wide().collect();
        if wide.iter().any(|&u| u == 0) {
            return Err(AppError::validation("相对路径名含内部 NUL".to_string()));
        }
        wide.push(0);
        Ok(wide)
    }
    fn ensure_plain_directory_handle(handle: HANDLE) -> Result<(), AppError> {
        let mut info: ByHandleFileInformation = unsafe { zeroed() };
        let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(AppError::validation("intent 路径不是目录".to_string()));
        }
        if info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(AppError::validation(
                "intent 目录是 reparse/junction，拒绝删除".to_string(),
            ));
        }
        Ok(())
    }
    unsafe fn open_relative(
        parent: HANDLE,
        name_wide: &mut [u16],
        desired_access: ACCESS_MASK,
        create_disposition: ULONG,
        create_options: ULONG,
    ) -> Result<OwnedHandle, AppError> {
        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = zeroed();
        let mut out: HANDLE = ptr::null_mut();
        let status = NtCreateFile(
            &mut out,
            desired_access,
            &mut attrs,
            &mut iosb,
            ptr::null_mut(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            create_disposition,
            create_options,
            ptr::null_mut(),
            0,
        );
        if status < 0 {
            let dos = RtlNtStatusToDosError(status) as i32;
            if dos == 2 || dos == 3 {
                return Err(AppError::from(std::io::Error::new(
                    ErrorKind::NotFound,
                    std::io::Error::from_raw_os_error(dos),
                )));
            }
            return Err(AppError::from(std::io::Error::from_raw_os_error(dos)));
        }
        Ok(OwnedHandle::from_raw_handle(out as RawHandle))
    }

    let receive_wide = to_wide(receive_dir)?;
    let mut intent_dir_wide = name_to_wide(FINALIZE_INTENT_DIR)?;
    let mut intent_name_wide = name_to_wide(intent_name)?;

    let receive_raw = unsafe {
        CreateFileW(
            receive_wide.as_ptr(),
            GENERIC_READ | FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if receive_raw.is_null() || receive_raw == INVALID_HANDLE_VALUE {
        let err = std::io::Error::last_os_error();
        if err.kind() == ErrorKind::NotFound {
            return Ok(());
        }
        return Err(AppError::from(err));
    }
    let receive_handle = unsafe { OwnedHandle::from_raw_handle(receive_raw as RawHandle) };
    if let Err(e) = ensure_plain_directory_handle(receive_handle.as_raw_handle() as HANDLE) {
        // receive_dir 本身是 reparse：拒绝 path 删除，直接失败更安全。
        return Err(e);
    }

    let intent_dir_handle = match unsafe {
        open_relative(
            receive_handle.as_raw_handle() as HANDLE,
            &mut intent_dir_wide,
            GENERIC_READ | FILE_LIST_DIRECTORY | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("NotFound") || msg.contains("os error 2") || msg.contains("os error 3")
            {
                return Ok(());
            }
            return Err(e);
        }
    };
    if let Err(_e) = ensure_plain_directory_handle(intent_dir_handle.as_raw_handle() as HANDLE) {
        // intent 目录是 reparse：不跟随删除外部目标，视为已无安全 intent。
        return Ok(());
    }

    match unsafe {
        open_relative(
            intent_dir_handle.as_raw_handle() as HANDLE,
            &mut intent_name_wide,
            DELETE | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE
                | FILE_SYNCHRONOUS_IO_NONALERT
                | FILE_DELETE_ON_CLOSE
                | FILE_OPEN_REPARSE_POINT,
        )
    } {
        Ok(h) => drop(h),
        Err(e) => {
            let msg = e.to_string();
            if !(msg.contains("NotFound")
                || msg.contains("os error 2")
                || msg.contains("os error 3"))
            {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Code Logic: 拼 intent 文件路径并校验仍在 receive_dir 内。
fn finalize_intent_path(receive_dir: &Path, transfer_id: &str) -> Result<PathBuf, AppError> {
    let safe_id = sanitize_receive_basename(transfer_id, "transfer_id")?;
    let intent_dir = receive_dir.join(FINALIZE_INTENT_DIR);
    // intent 目录若是 symlink，canonicalize 会跟随逃逸；先 no-follow 拒绝。
    match std::fs::symlink_metadata(&intent_dir) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(AppError::validation(format!(
                "intent 目录是符号链接，拒绝写入: {}",
                intent_dir.display()
            )));
        }
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(AppError::validation(format!(
                "intent 路径不是目录: {}",
                intent_dir.display()
            )));
        }
        Ok(_) | Err(_) => {}
    }
    let path = intent_dir.join(format!("{safe_id}.json"));
    // 词法边界：用 normalize 而非对可能不存在/symlink 父目录 canonicalize。
    let canonical_dir = match receive_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => normalize_path(receive_dir),
    };
    let lexical = canonical_dir
        .join(FINALIZE_INTENT_DIR)
        .join(format!("{safe_id}.json"));
    if !lexical.starts_with(&canonical_dir) {
        return Err(AppError::validation(
            "目标路径逃逸 receive_dir，拒绝写入".to_string(),
        ));
    }
    // 若 intent 目录已是普通目录，再做一次真实路径校验。
    if intent_dir.is_dir() {
        ensure_path_within_dir(receive_dir, &path)?;
    }
    Ok(path)
}

/// Business Logic（为什么需要这个函数）:
///     进程在 place 后、history 前崩溃时，内存 active/墓碑丢失；必须从 intent + 最终文件恢复
///     durable 终态，阻止 handle_init 接受同一 transfer_id 生成后缀副本。
///     但 intent 写在 no-replace place 之前：若 place 因 AlreadyExists 失败且崩溃发生在下一轮
///     覆盖 intent 前，磁盘上 intent 仍指向竞争文件。仅靠 size 会把同尺寸碰撞文件误晋升为
///     Completed，导致发送端确认成功而原始 tmp 从未交付。
///     同样，`metadata()` 会跟随符号链接：若 final 是指向同尺寸同哈希目标的 symlink，
///     is_file + 跟随哈希都会通过，却从未把本次 .tmp place 到 final；链接被删/改指后静默丢数据。
///     恢复晋升只接受**普通文件**（no-follow）。
///
/// Code Logic（这个函数做什么）:
///     读 intent；若 history 已有终态则清 intent；对 final 使用 no-follow 元数据：
///     拒绝 symlink/非普通文件；长度匹配后再对同一路径 no-follow 打开并流式 SHA256，
///     与 intent.sha256 严格比对；通过后构造 Completed task → promote_completed_to_durable
///     （内部在同一绑定句柄上 re-hash + fsync + 父目录项身份确认）→ 返回 success ChunkResp；
///     durability / 身份确认失败：保留 intent、确保 active=Completed(final)，返回 Unavailable 可重试，
///     不写 Completed history；
///     symlink / 非普通文件 / size 不匹配 / sha 不匹配 / 读失败：清 intent 并返回 None
///     （不删 .tmp、不宣称 success，允许干净重传或下一后缀重试）。
async fn try_recover_finalize_intent(
    state: &AppState,
    transfer_id: &str,
    receive_dir: &Path,
) -> Result<Option<ChunkResp>, AppError> {
    let path = match finalize_intent_path(receive_dir, transfer_id) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::from(e)),
    };
    let intent: FinalizeIntent = match serde_json::from_slice(&bytes) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("损坏的 finalize intent，删除: {transfer_id}, {e}");
            let _ = clear_finalize_intent(receive_dir, transfer_id).await;
            return Ok(None);
        }
    };
    if intent.transfer_id != transfer_id {
        tracing::warn!(
            "finalize intent transfer_id 不匹配，删除: expected={transfer_id}, got={}",
            intent.transfer_id
        );
        let _ = clear_finalize_intent(receive_dir, transfer_id).await;
        return Ok(None);
    }

    // 已 durable：只清 intent。
    if let Some(hist) = state.transfer_repo.get_by_id(transfer_id).await? {
        if hist.direction == TransferDirection::Receive
            && matches!(
                hist.status,
                TransferStatus::Completed | TransferStatus::Failed | TransferStatus::Cancelled
            )
        {
            let _ = clear_finalize_intent(receive_dir, transfer_id).await;
            return history_terminal_chunk_resp(state, transfer_id).await;
        }
    }

    let final_path = PathBuf::from(&intent.final_path);
    // 边界：final_path 必须仍在 receive_dir 内。
    if ensure_path_within_dir(receive_dir, &final_path).is_err() {
        tracing::warn!("intent final_path 逃逸 receive_dir，删除: {transfer_id}");
        let _ = clear_finalize_intent(receive_dir, transfer_id).await;
        return Ok(None);
    }

    // no-follow：禁止 symlink 绕过“仅普通文件可恢复 Completed”。
    // tokio::fs::metadata 会跟随链接，必须用 symlink_metadata + 同路径 no-follow 读哈希。
    let meta = match tokio::fs::symlink_metadata(&final_path).await {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            tracing::warn!(
                "finalize intent 存在但最终文件缺失，清除 intent 允许重传: {transfer_id}"
            );
            let _ = clear_finalize_intent(receive_dir, transfer_id).await;
            return Ok(None);
        }
        Err(e) => {
            tracing::warn!(
                "finalize intent 目标 metadata 失败，清除 intent 允许重传: {transfer_id}, {e}"
            );
            let _ = clear_finalize_intent(receive_dir, transfer_id).await;
            return Ok(None);
        }
    };
    let ft = meta.file_type();
    if ft.is_symlink() || !ft.is_file() || meta.len() != intent.size {
        tracing::warn!(
            "finalize intent 目标非普通文件/尺寸不匹配（symlink={}/is_file={}/len={}），\
             清除 intent 且不宣称 success: {transfer_id}",
            ft.is_symlink(),
            ft.is_file(),
            meta.len()
        );
        let _ = clear_finalize_intent(receive_dir, transfer_id).await;
        return Ok(None);
    }

    // 内容所有权：size 匹配不够。intent 写后 place 前被同尺寸竞争文件占用时，
    // AlreadyExists 后崩溃会使 intent 指向竞争者；必须 no-follow 重算 SHA256。
    let actual = match compute_sha256_nofollow(&final_path).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                "finalize intent 目标无法校验 sha256，清除 intent 允许重传: {transfer_id}, {e}"
            );
            let _ = clear_finalize_intent(receive_dir, transfer_id).await;
            return Ok(None);
        }
    };
    if actual != intent.sha256 {
        tracing::warn!(
            "finalize intent 目标 sha256 不匹配（可能是同尺寸碰撞文件），\
             清除 intent 且不宣称 success，允许重试下一后缀: {transfer_id}"
        );
        let _ = clear_finalize_intent(receive_dir, transfer_id).await;
        // 不删除 .tmp，不写 history Completed，不返回 success。
        return Ok(None);
    }

    // 最终文件在且内容匹配：构造 Completed task 后由 promote 在同一绑定句柄上
    // 再次 len+SHA+fsync 并确认父目录项身份，再写 durable history，禁止 re-receive。
    // 失败时保留 intent，并确保 active=Completed（file_path=final）以便 complete 重试，
    // 绝不写 Completed history。
    let task = TransferTask {
        id: transfer_id.to_string(),
        filename: intent.filename.clone(),
        file_path: intent.final_path.clone(),
        size: intent.size,
        sha256: intent.sha256.clone(),
        chunk_size: intent.chunk_size,
        direction: TransferDirection::Receive,
        peer_device_id: String::new(),
        status: TransferStatus::Completed,
        transferred_bytes: intent.size,
        created_at: intent.created_at.clone(),
        completed_at: Some(now_iso()),
        ..TransferTask::recovery_defaults(transfer_id)
    };
    // 若 registry 无 entry，临时 add 以便 promote 后 remove/tombstone；durability pending 也保留。
    if state.transfers.get(transfer_id).is_none() {
        state.transfers.add(task.clone());
    } else {
        state.transfers.mark_completed(
            transfer_id,
            task.completed_at.clone().unwrap_or_else(now_iso),
            Some(intent.final_path.clone()),
        );
    }
    // promote 内部 certify；此处不再 close-then-reopen 按路径 fsync（身份切换窗口）。
    let _ = final_path;
    promote_completed_to_durable(state, transfer_id, &task).await?;
    Ok(Some(ChunkResp {
        success: true,
        received_bytes: intent.size,
    }))
}

/// 原子落盘结果。
struct PlacedFile {
    final_filename: String,
    final_path: PathBuf,
}

/// place 失败分类：未放置 vs 已放置但 durability 待补。
enum PlaceFinalError {
    /// hard_link/rename 未成功：可写 Failed history / 换后缀 / 重传。
    Unplaced(AppError),
    /// 最终路径已独占成功，但 fsync 最终文件/父目录失败：保留 Completed active + intent，禁止 Failed。
    DurabilityPending { placed: PlacedFile, message: String },
}

impl From<PlaceFinalError> for AppError {
    fn from(value: PlaceFinalError) -> Self {
        match value {
            PlaceFinalError::Unplaced(err) => err,
            PlaceFinalError::DurabilityPending { message, .. } => AppError::unavailable(message),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     finalize 在 place 前必须为**每个**候选 final 路径（首选与后缀）写 durable intent。
///     若仅在首次候选写 intent，冲突后清 intent 再 place 后缀，place→补写 intent 之间崩溃
///     会丢失 journal：重启后同 transfer_id 可 reopen 并生成第二份后缀副本。
///
/// Code Logic（这个函数做什么）:
///     有限循环：resolve 候选 → 校验 basename/边界 → **先** write_finalize_intent 指向该候选
///     → no-replace place；AlreadyExists 则保留/覆盖下一候选 intent 再试，绝不 place 无匹配
///     intent 的路径；其它错误上抛（intent 指向未成功路径时 recovery 会清 intent 允许干净重传）。
///     调用方必须已持有 receive_dir 锁。
async fn place_final_file_with_intent(
    receive_dir: &Path,
    safe_filename: &str,
    tmp_path: &Path,
    transfer_id: &str,
    task: &TransferTask,
) -> Result<PlacedFile, PlaceFinalError> {
    // 有限重试：极端并发下 resolve 可能反复撞车；上限防止死循环。
    for _ in 0..10_000 {
        let final_filename = resolve_filename(receive_dir, safe_filename);
        if let Err(e) = sanitize_receive_basename(&final_filename, "final_filename") {
            return Err(PlaceFinalError::Unplaced(AppError::validation(format!(
                "非法最终文件名: {e}"
            ))));
        }
        let final_path = receive_dir.join(&final_filename);
        ensure_path_within_dir(receive_dir, &final_path).map_err(PlaceFinalError::Unplaced)?;

        // 每个候选：先 journal 再 place（含首次与后缀）。
        let intent = FinalizeIntent {
            transfer_id: transfer_id.to_string(),
            filename: task.filename.clone(),
            size: task.size,
            sha256: task.sha256.clone(),
            chunk_size: task.chunk_size,
            final_filename: final_filename.clone(),
            final_path: final_path.to_string_lossy().to_string(),
            created_at: now_iso(),
        };
        write_finalize_intent(receive_dir, &intent)
            .await
            .map_err(PlaceFinalError::Unplaced)?;

        match commit_tmp_to_final_no_replace(tmp_path, &final_path).await {
            Ok(()) => {
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::AlreadyExists) => {
                // 候选被占：下一轮会写新候选 intent 再 place，禁止无 intent 的 place helper。
                continue;
            }
            Err(CommitFinalError::DurabilityPending(e)) => {
                // 最终文件已落地：保留 intent，交上层 mark Completed + 可重试，禁止 Failed。
                return Err(PlaceFinalError::DurabilityPending {
                    placed: PlacedFile {
                        final_filename,
                        final_path,
                    },
                    message: format!("最终文件已落地但 durability 同步失败: {e}"),
                });
            }
            Err(CommitFinalError::Failed(e)) => {
                // 非冲突失败：intent 仍在但 final 缺失；recovery 会清 intent 允许重传。
                return Err(PlaceFinalError::Unplaced(AppError::generic(format!(
                    "提交最终文件失败: {e}"
                ))));
            }
        }
    }
    Err(PlaceFinalError::Unplaced(AppError::generic(
        "无法分配不冲突的最终文件名（重试耗尽）".to_string(),
    )))
}

/// Business Logic（为什么需要这个函数）:
///     并发同名接收时，仅 `exists()` 检查后 rename 会在 POSIX 上静默覆盖先落地的文件。
///     零字节占位再 rename 仍有 TOCTOU：外部进程可在两步之间替换路径；失败时无条件
///     `remove_file` 还可能误删竞争者文件；崩溃会留下“看似成功”的零字节最终文件。
///     因此必须用 hard_link(tmp, final) 作为跨平台 no-replace 提交原语。
///
/// Code Logic（这个函数做什么）:
///     循环 resolve 候选名 → 校验路径边界 → `hard_link(tmp, final)` 原子抢占最终路径
///     （已存在 → AlreadyExists，换后缀继续）→ 成功后删除 tmp（失败仅 warn，最终文件已落地）。
///     hard_link 在跨卷等场景不可用时，回退到平台 no-replace rename（见
///     `commit_tmp_to_final_no_replace`），仍保证失败路径不删除非本次创建的文件。
///     **生产 finalize 必须走 `place_final_file_with_intent`**（每个候选 journal-before-place）；
///     本 helper 仅用于无 journal 的底层 no-replace 单测。
///     调用方必须已持有 receive_dir 锁（同进程额外协调）。
#[cfg(test)]
async fn place_final_file_exclusive(
    receive_dir: &Path,
    safe_filename: &str,
    tmp_path: &Path,
) -> Result<PlacedFile, AppError> {
    // 有限重试：极端并发下 resolve 可能反复撞车；上限防止死循环。
    for _ in 0..10_000 {
        let final_filename = resolve_filename(receive_dir, safe_filename);
        if let Err(e) = sanitize_receive_basename(&final_filename, "final_filename") {
            return Err(AppError::validation(format!("非法最终文件名: {e}")));
        }
        let final_path = receive_dir.join(&final_filename);
        ensure_path_within_dir(receive_dir, &final_path)?;

        match commit_tmp_to_final_no_replace(tmp_path, &final_path).await {
            Ok(()) => {
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::AlreadyExists) => {
                // 候选名被并发占走，重新 resolve 下一个后缀。
                continue;
            }
            Err(CommitFinalError::DurabilityPending(e)) => {
                // 测试 helper 将 durability pending 视为成功落盘（文件已在）。
                tracing::warn!("place exclusive durability pending: {e}");
                return Ok(PlacedFile {
                    final_filename,
                    final_path,
                });
            }
            Err(CommitFinalError::Failed(e)) => {
                return Err(AppError::generic(format!("提交最终文件失败: {e}")));
            }
        }
    }
    Err(AppError::generic(
        "无法分配不冲突的最终文件名（重试耗尽）".to_string(),
    ))
}

/// place 底层 commit 错误：区分未放置 / 已放置但 durability 未完成。
enum CommitFinalError {
    AlreadyExists,
    Failed(std::io::Error),
    DurabilityPending(std::io::Error),
}

/// Business Logic（为什么需要这个函数）:
///     将已校验的临时文件提交到最终路径，且不得覆盖/删除路径上已有的竞争文件。
///     hard_link 是首选：同 inode 链接要么成功要么 AlreadyExists，无零字节占位窗口。
///     place 成功后的 fsync 失败不能伪装成“未放置”，否则上层会写 Failed 并清 intent。
///
/// Code Logic（这个函数做什么）:
///     1) 同步 hard_link(tmp → final)；成功后 best-effort remove tmp；
///     2) hard_link 因 AlreadyExists 失败 → AlreadyExists，调用方换后缀；
///     3) hard_link 因跨卷/不支持失败 → 平台 no-replace rename 回退；
///        回退失败绝不 remove final（可能是竞争者文件）；
///     4) place 成功后 fsync 最终普通文件 + 父目录；失败返回 DurabilityPending。
async fn commit_tmp_to_final_no_replace(
    tmp_path: &Path,
    final_path: &Path,
) -> Result<(), CommitFinalError> {
    // place 前把 tmp 数据刷到稳定存储，避免断电丢内容而 history 已 completed。
    if let Err(e) = sync_regular_file(tmp_path).await {
        return Err(CommitFinalError::Failed(std::io::Error::other(format!(
            "sync tmp 失败: {e}"
        ))));
    }
    // hard_link 是同步 syscall；放在 blocking 上下文外也可接受（单次元数据操作）。
    match std::fs::hard_link(tmp_path, final_path) {
        Ok(()) => {
            if let Err(e) = tokio::fs::remove_file(tmp_path).await {
                tracing::warn!(
                    "最终文件 hard_link 成功但删除 tmp 失败 ({}): {e}",
                    tmp_path.display()
                );
            }
            // 最终目录项 + unlink 后 fsync 最终文件与 receive_dir。
            if let Err(e) = ensure_final_file_durable(final_path).await {
                return Err(CommitFinalError::DurabilityPending(std::io::Error::other(
                    format!("fsync final/receive_dir 失败: {e}"),
                )));
            }
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(CommitFinalError::AlreadyExists),
        Err(link_err) => {
            // 跨卷/不支持 hard_link：回退到平台 no-replace rename，仍禁止覆盖已有 final。
            match rename_no_replace(tmp_path, final_path).await {
                Ok(()) => {
                    if let Err(e) = ensure_final_file_durable(final_path).await {
                        return Err(CommitFinalError::DurabilityPending(std::io::Error::other(
                            format!("fsync final/receive_dir 失败: {e}"),
                        )));
                    }
                    Ok(())
                }
                Err(rename_err) if rename_err.kind() == ErrorKind::AlreadyExists => {
                    Err(CommitFinalError::AlreadyExists)
                }
                Err(rename_err) => {
                    // 保留原始 hard_link 错误上下文，便于诊断跨卷场景。
                    Err(CommitFinalError::Failed(std::io::Error::new(
                        rename_err.kind(),
                        format!(
                            "hard_link 失败 ({link_err}) 且 no-replace rename 失败 ({rename_err})"
                        ),
                    )))
                }
            }
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     place 成功后的即时 fsync：最终普通文件与父目录项进入稳定存储。
///     **不得**单独用于 Completed history 晋升：路径 reopen 无法抵御普通文件替换。
///
/// Code Logic（这个函数做什么）:
///     no-follow sync 最终普通文件，再 fsync 其父目录；任一步失败上抛。
async fn ensure_final_file_durable(final_path: &Path) -> Result<(), AppError> {
    sync_regular_file(final_path).await?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// 绑定文件身份：用于 durability 晋升前确认父目录项仍指向同一普通文件。
///
/// Business Logic（为什么需要这个结构）:
///     durability 失败与重试之间，最终路径上的普通文件可被原子替换；仅 no-follow open
///     会通过，但内容/身份已变，不能再按原始 size/SHA 写 Completed history。
///
/// Code Logic（这个结构做什么）:
///     Unix 存 (dev,ino)；Windows 存 (volume_serial, file_index)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BoundFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(not(any(unix, windows)))]
    _marker: u8,
}

/// Business Logic（为什么需要这个函数）:
///     从已打开的普通文件句柄读取稳定身份，供 fsync 后与父目录项对照。
///
/// Code Logic（这个函数做什么）:
///     Unix: fstat → (st_dev, st_ino)；Windows: GetFileInformationByHandle →
///     (volume_serial_number, file_index_high<<32|file_index_low)。
fn file_identity_from_std(file: &std::fs::File) -> Result<BoundFileIdentity, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata().map_err(AppError::from)?;
        Ok(BoundFileIdentity {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut std::ffi::c_void;

        #[repr(C)]
        struct ByHandleFileInformation {
            file_attributes: DWORD,
            creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            volume_serial_number: DWORD,
            file_size_high: DWORD,
            file_size_low: DWORD,
            number_of_links: DWORD,
            file_index_high: DWORD,
            file_index_low: DWORD,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandle(
                h_file: HANDLE,
                lp_file_information: *mut ByHandleFileInformation,
            ) -> BOOL;
        }

        let mut info = unsafe { std::mem::zeroed::<ByHandleFileInformation>() };
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let file_index = ((info.file_index_high as u64) << 32) | (info.file_index_low as u64);
        Ok(BoundFileIdentity {
            volume_serial: info.volume_serial_number,
            file_index,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法读取文件身份",
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     fsync 后必须通过父目录 HANDLE/fd 再次打开同名目录项，证明仍是同一 inode/file id。
///
/// Code Logic（这个函数做什么）:
///     Unix: open 父目录 O_DIRECTORY → openat(O_NOFOLLOW|O_RDONLY) → fstat 身份；
///     Windows: CreateFileW 打开父目录（BACKUP_SEMANTICS|OPEN_REPARSE_POINT，拒绝 reparse）
///     → NtCreateFile 相对 basename 打开普通文件 → GetFileInformationByHandle。
fn dirent_identity_via_parent(final_path: &Path) -> Result<BoundFileIdentity, AppError> {
    let parent = final_path.parent().ok_or_else(|| {
        AppError::validation(format!(
            "最终路径无父目录，无法确认目录项身份: {}",
            final_path.display()
        ))
    })?;
    let name = final_path.file_name().ok_or_else(|| {
        AppError::validation(format!(
            "最终路径无文件名，无法确认目录项身份: {}",
            final_path.display()
        ))
    })?;

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::io::{AsRawFd, FromRawFd};

        let parent_c = CString::new(parent.as_os_str().as_bytes())
            .map_err(|_| AppError::validation("父目录路径含内部 NUL"))?;
        let name_c = CString::new(name.as_bytes())
            .map_err(|_| AppError::validation("最终文件名含内部 NUL"))?;
        let parent_fd = unsafe {
            libc::open(
                parent_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if parent_fd < 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let parent_file = unsafe { std::fs::File::from_raw_fd(parent_fd) };
        let child_fd = unsafe {
            libc::openat(
                parent_file.as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if child_fd < 0 {
            let err = std::io::Error::last_os_error();
            return Err(AppError::from(std::io::Error::new(
                err.kind(),
                format!(
                    "经父目录确认最终文件身份失败 {}: {err}",
                    final_path.display()
                ),
            )));
        }
        let child = unsafe { std::fs::File::from_raw_fd(child_fd) };
        let id = file_identity_from_std(&child)?;
        drop(child);
        drop(parent_file);
        Ok(id)
    }

    #[cfg(windows)]
    {
        use std::ffi::c_void;
        use std::mem::{size_of, zeroed};
        use std::os::windows::ffi::OsStrExt;
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
        use std::ptr;

        type BOOL = i32;
        type DWORD = u32;
        type HANDLE = *mut c_void;
        type NTSTATUS = i32;
        type ULONG = u32;
        type USHORT = u16;
        type ACCESS_MASK = u32;

        const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
        const FILE_FLAG_BACKUP_SEMANTICS: DWORD = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: DWORD = 0x0020_0000;
        const FILE_SHARE_READ: DWORD = 0x1;
        const FILE_SHARE_WRITE: DWORD = 0x2;
        const FILE_SHARE_DELETE: DWORD = 0x4;
        const OPEN_EXISTING: DWORD = 3;
        const GENERIC_READ: DWORD = 0x8000_0000;
        const FILE_LIST_DIRECTORY: ACCESS_MASK = 0x0001;
        const FILE_READ_ATTRIBUTES: ACCESS_MASK = 0x0080;
        const SYNCHRONIZE: ACCESS_MASK = 0x0010_0000;
        const FILE_OPEN: ULONG = 0x0000_0001;
        const FILE_NON_DIRECTORY_FILE: ULONG = 0x0000_0040;
        const FILE_SYNCHRONOUS_IO_NONALERT: ULONG = 0x0000_0020;
        const FILE_OPEN_REPARSE_POINT: ULONG = 0x0020_0000;
        const OBJ_CASE_INSENSITIVE: ULONG = 0x0000_0040;
        const FILE_ATTRIBUTE_DIRECTORY: DWORD = 0x10;
        const FILE_ATTRIBUTE_REPARSE_POINT: DWORD = 0x400;

        #[repr(C)]
        struct UnicodeString {
            length: USHORT,
            maximum_length: USHORT,
            buffer: *mut u16,
        }
        #[repr(C)]
        struct ObjectAttributes {
            length: ULONG,
            root_directory: HANDLE,
            object_name: *mut UnicodeString,
            attributes: ULONG,
            security_descriptor: *mut c_void,
            security_quality_of_service: *mut c_void,
        }
        #[repr(C)]
        struct IoStatusBlock {
            status: NTSTATUS,
            information: usize,
        }
        #[repr(C)]
        struct ByHandleFileInformation {
            file_attributes: DWORD,
            creation_time: u64,
            last_access_time: u64,
            last_write_time: u64,
            volume_serial_number: DWORD,
            file_size_high: DWORD,
            file_size_low: DWORD,
            number_of_links: DWORD,
            file_index_high: DWORD,
            file_index_low: DWORD,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn CreateFileW(
                lp_file_name: *const u16,
                dw_desired_access: DWORD,
                dw_share_mode: DWORD,
                lp_security_attributes: *mut c_void,
                dw_creation_disposition: DWORD,
                dw_flags_and_attributes: DWORD,
                h_template_file: HANDLE,
            ) -> HANDLE;
            fn GetFileInformationByHandle(
                h_file: HANDLE,
                lp_file_information: *mut ByHandleFileInformation,
            ) -> BOOL;
        }
        #[link(name = "ntdll")]
        extern "system" {
            fn NtCreateFile(
                file_handle: *mut HANDLE,
                desired_access: ACCESS_MASK,
                object_attributes: *mut ObjectAttributes,
                io_status_block: *mut IoStatusBlock,
                allocation_size: *mut i64,
                file_attributes: ULONG,
                share_access: ULONG,
                create_disposition: ULONG,
                create_options: ULONG,
                ea_buffer: *mut c_void,
                ea_length: ULONG,
            ) -> NTSTATUS;
            fn RtlNtStatusToDosError(status: NTSTATUS) -> DWORD;
        }

        fn to_wide(path: &Path) -> Result<Vec<u16>, AppError> {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.iter().any(|&u| u == 0) {
                return Err(AppError::validation("路径含内部 NUL".to_string()));
            }
            wide.push(0);
            Ok(wide)
        }
        fn name_to_wide(name: &std::ffi::OsStr) -> Result<Vec<u16>, AppError> {
            let mut wide: Vec<u16> = name.encode_wide().collect();
            if wide.is_empty() || wide.iter().any(|&u| u == 0) {
                return Err(AppError::validation("相对路径名非法".to_string()));
            }
            wide.push(0);
            Ok(wide)
        }

        let parent_wide = to_wide(parent)?;
        let mut name_wide = name_to_wide(name)?;
        let parent_raw = unsafe {
            CreateFileW(
                parent_wide.as_ptr(),
                GENERIC_READ | FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if parent_raw.is_null() || parent_raw == INVALID_HANDLE_VALUE {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        let parent_handle = unsafe { OwnedHandle::from_raw_handle(parent_raw as RawHandle) };
        let mut pinfo: ByHandleFileInformation = unsafe { zeroed() };
        let pok = unsafe {
            GetFileInformationByHandle(parent_handle.as_raw_handle() as HANDLE, &mut pinfo)
        };
        if pok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if pinfo.file_attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || pinfo.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(AppError::validation(
                "父目录不是普通目录（reparse/非目录），拒绝确认身份".to_string(),
            ));
        }

        let name_units = name_wide.len().saturating_sub(1);
        let byte_len = name_units * 2;
        if byte_len > u16::MAX as usize {
            return Err(AppError::validation("相对路径名过长".to_string()));
        }
        let mut unicode = UnicodeString {
            length: byte_len as USHORT,
            maximum_length: (byte_len + 2) as USHORT,
            buffer: name_wide.as_mut_ptr(),
        };
        let mut attrs = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as ULONG,
            root_directory: parent_handle.as_raw_handle() as HANDLE,
            object_name: &mut unicode,
            attributes: OBJ_CASE_INSENSITIVE,
            security_descriptor: ptr::null_mut(),
            security_quality_of_service: ptr::null_mut(),
        };
        let mut iosb: IoStatusBlock = unsafe { zeroed() };
        let mut out: HANDLE = ptr::null_mut();
        let status = unsafe {
            NtCreateFile(
                &mut out,
                GENERIC_READ | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &mut attrs,
                &mut iosb,
                ptr::null_mut(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
                ptr::null_mut(),
                0,
            )
        };
        if status < 0 {
            let dos = unsafe { RtlNtStatusToDosError(status) };
            return Err(AppError::from(std::io::Error::from_raw_os_error(
                dos as i32,
            )));
        }
        if out.is_null() || out == INVALID_HANDLE_VALUE {
            return Err(AppError::generic(
                "NtCreateFile 返回无效 HANDLE（dirent 身份确认）".to_string(),
            ));
        }
        let child = unsafe { OwnedHandle::from_raw_handle(out as RawHandle) };
        let mut cinfo: ByHandleFileInformation = unsafe { zeroed() };
        let cok =
            unsafe { GetFileInformationByHandle(child.as_raw_handle() as HANDLE, &mut cinfo) };
        if cok == 0 {
            return Err(AppError::from(std::io::Error::last_os_error()));
        }
        if cinfo.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0
            || cinfo.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(AppError::validation(
                "最终目录项不是普通文件（reparse/目录）".to_string(),
            ));
        }
        let file_index = ((cinfo.file_index_high as u64) << 32) | (cinfo.file_index_low as u64);
        Ok(BoundFileIdentity {
            volume_serial: cinfo.volume_serial_number,
            file_index,
        })
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (parent, name);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法经父目录确认文件身份",
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     Completed history 晋升（含 DurabilityPending 重试与 intent 恢复）前，必须证明路径上的
///     普通文件仍是原始传输内容，且 fsync 作用在该同一句柄上。若仅按路径 reopen+fsync，
///     替换后的同尺寸普通文件会静默通过 no-follow 并被认证为成功。
///
/// Code Logic（这个函数做什么）:
///     1) no-follow **读写**打开普通文件句柄（Windows FlushFileBuffers 需 GENERIC_WRITE）；
///     2) 读 len，流式 SHA256（同一句柄）；
///     3) 与 expected size/sha 比对；
///     4) 同一句柄 sync_all；
///     5) fsync 父目录（Windows 目录句柄同样需写权限）；
///     6) 经父目录 HANDLE/fd 重新打开目录项，比对 file id/inode，不一致则拒绝晋升。
async fn certify_final_file_for_history(
    final_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), AppError> {
    let path = final_path.to_path_buf();
    let expected_sha = expected_sha256.to_string();
    tokio::task::spawn_blocking(move || {
        certify_final_file_for_history_blocking(&path, expected_size, &expected_sha)
    })
    .await
    .map_err(|e| AppError::generic(format!("certify_final_file_for_history join 失败: {e}")))?
}

/// Business Logic（为什么需要这个函数）:
///     certify 的同步实现，在 blocking 线程持有文件句柄完成 hash/fsync/身份确认。
///
/// Code Logic（这个函数做什么）:
///     见 `certify_final_file_for_history`。
///     以 writable=true no-follow 打开：Windows 上同一句柄 `sync_all`→FlushFileBuffers
///     需要 GENERIC_WRITE；只读打开会 AccessDenied，Completed history 永远无法晋升。
fn certify_final_file_for_history_blocking(
    final_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), AppError> {
    use std::io::Read;

    // 同步 no-follow 读写打开：读用于 hash，写权限用于 Windows FlushFileBuffers。
    let std_file = open_regular_file_nofollow_std(final_path, true)?;
    let meta = std_file.metadata().map_err(AppError::from)?;
    if meta.len() != expected_size {
        return Err(AppError::validation(format!(
            "最终文件长度与任务不一致: path={}, len={}, expected={expected_size}",
            final_path.display(),
            meta.len()
        )));
    }
    let bound_id = file_identity_from_std(&std_file)?;

    // 同一句柄流式 SHA256。
    let mut file = std_file;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8192];
    // 从头读：open 后默认 offset=0。
    loop {
        let n = file.read(&mut buf).map_err(AppError::from)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected_sha256 {
        return Err(AppError::validation(format!(
            "最终文件 SHA256 与任务不一致（可能被替换）: path={}, actual={actual}, expected={expected_sha256}",
            final_path.display()
        )));
    }

    // 同一句柄 fsync（Windows 需写权限句柄）。
    file.sync_all().map_err(AppError::from)?;
    if let Some(parent) = final_path.parent() {
        fsync_dir(parent)?;
    }

    // 经父目录重新打开目录项，确认仍是同一 file id/inode。
    let dirent_id = dirent_identity_via_parent(final_path)?;
    if dirent_id != bound_id {
        return Err(AppError::validation(format!(
            "最终文件目录项身份已变化（可能被原子替换），拒绝写 Completed history: {}",
            final_path.display()
        )));
    }
    // 保持 file 活到身份确认之后，缩小替换窗口。
    drop(file);
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     certify 在 blocking 上下文需要同步 no-follow 打开，避免 async runtime 跨 await 丢句柄。
///
/// Code Logic（这个函数做什么）:
///     与 `open_regular_file_nofollow` 相同平台语义，返回 `std::fs::File`。
fn open_regular_file_nofollow_std(path: &Path, writable: bool) -> Result<std::fs::File, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(libc::O_NOFOLLOW);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path).map_err(|e| {
            if e.kind() == ErrorKind::Other
                || e.raw_os_error() == Some(libc::ELOOP)
                || e.raw_os_error() == Some(libc::EPERM)
            {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("拒绝跟随符号链接打开: {}: {e}", path.display()),
                )
            } else {
                e
            }
        })?;
        let meta = std_file.metadata()?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(std_file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path)?;
        let meta = std_file.metadata()?;
        let ft = meta.file_type();
        if ft.is_symlink()
            || !ft.is_file()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(std_file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, writable);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法 no-follow 打开文件",
        )))
    }
}

/// Business Logic（为什么需要这个函数）:
///     hard_link 不可用（跨卷/网络盘/受限 FS）时仍必须保证 no-replace：已有目标则失败、
///     绝不覆盖竞争者；也不得用零字节占位 + 普通 rename（崩溃会留下假最终文件，外部
///     进程可在间隙替换路径）。
///
/// Code Logic（这个函数做什么）:
///     在 blocking 线程调用平台原生**原子排他 rename**：
///     - Linux：`renameat2(..., RENAME_NOREPLACE)`；
///     - macOS：`renamex_np(..., RENAME_EXCL)`；
///     - Windows：`MoveFileExW` **不带** `MOVEFILE_REPLACE_EXISTING`（目标存在 → 失败）；
///     平台/内核无法提供 no-replace 时 fail-closed 返回 Unsupported，**禁止**占位回退。
async fn rename_no_replace(tmp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    let from = tmp_path.to_path_buf();
    let to = final_path.to_path_buf();
    tokio::task::spawn_blocking(move || rename_no_replace_blocking(&from, &to))
        .await
        .map_err(|e| std::io::Error::other(format!("rename_no_replace join 失败: {e}")))?
}

/// Business Logic（为什么需要这个函数）:
///     真正的 no-replace rename 是同步 syscall，必须在 blocking 上下文执行。
///
/// Code Logic（这个函数做什么）:
///     见 `rename_no_replace` 的平台分支；仅同步路径。
fn rename_no_replace_blocking(tmp_path: &Path, final_path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(tmp_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "tmp 路径含内部 NUL"))?;
        let to = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "final 路径含内部 NUL"))?;
        // RENAME_NOREPLACE = 1：目标存在则失败，不覆盖。
        const RENAME_NOREPLACE: libc::c_uint = 1;
        // SAFETY: 路径已转为以 NUL 结尾的 CString；renameat2 对 AT_FDCWD 语义与 rename 一致。
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ENOSYS / EINVAL：内核或 FS 不支持 noreplace → fail-closed，禁止回退普通 rename。
        if err.raw_os_error() == Some(libc::ENOSYS) || err.raw_os_error() == Some(libc::EINVAL) {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!("文件系统不支持 RENAME_NOREPLACE: {err}"),
            ));
        }
        // EEXIST → AlreadyExists，供调用方换后缀。
        if err.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(tmp_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "tmp 路径含内部 NUL"))?;
        let to = CString::new(final_path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidInput, "final 路径含内部 NUL"))?;
        // RENAME_EXCL = 0x00000004：目标存在则失败（与 Linux RENAME_NOREPLACE 同语义）。
        const RENAME_EXCL: libc::c_uint = 0x0000_0004;
        // SAFETY: CString 保证 NUL 结尾；renamex_np 在失败时设置 errno。
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOTSUP) || err.raw_os_error() == Some(libc::ENOSYS) {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!("文件系统不支持 RENAME_EXCL: {err}"),
            ));
        }
        if err.raw_os_error() == Some(libc::EEXIST) {
            return Err(std::io::Error::new(ErrorKind::AlreadyExists, err));
        }
        Err(err)
    }

    #[cfg(windows)]
    {
        // 关键：Rust std 的 `fs::rename` 在 Windows 调用 MoveFileExW **带**
        // MOVEFILE_REPLACE_EXISTING，会覆盖竞争者。必须直接调原生 API 且 flags=0。
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(
                lp_existing_file_name: *const u16,
                lp_new_file_name: *const u16,
                dw_flags: u32,
            ) -> i32;
        }

        /// Business Logic: 把 OsStr 编成 Windows 宽字符串（NUL 结尾），供 MoveFileExW 使用。
        /// Code Logic: encode_wide 追加 0；路径含内部 NUL 时返回 InvalidInput。
        fn to_wide(path: &Path) -> std::io::Result<Vec<u16>> {
            let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
            if wide.iter().any(|&u| u == 0) {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidInput,
                    "路径含内部 NUL",
                ));
            }
            wide.push(0);
            Ok(wide)
        }

        let from = to_wide(tmp_path)?;
        let to = to_wide(final_path)?;
        // flags=0：不带 MOVEFILE_REPLACE_EXISTING(0x1)；目标存在则失败，绝不覆盖。
        // SAFETY: 宽字符串以 NUL 结尾；flags 明确为 0。
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
        if ok != 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        // ERROR_ALREADY_EXISTS=183 / ERROR_FILE_EXISTS=80 → AlreadyExists（调用方换后缀）。
        match err.raw_os_error() {
            Some(183) | Some(80) => Err(std::io::Error::new(ErrorKind::AlreadyExists, err)),
            _ if err.kind() == ErrorKind::AlreadyExists => Err(err),
            _ => Err(err),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (tmp_path, final_path);
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法提供原子 no-replace rename；禁止占位回退",
        ))
    }
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

/// Business Logic（为什么需要这个函数）:
///     complete/chunk 在内存 miss 时需要把已落盘的 Receive 历史还原为 success 响应。
///
/// Code Logic（这个函数做什么）:
///     查 transfer_history by id；仅 direction=Receive 且 status 为 completed/failed 时返回
///     ChunkResp；其它方向/状态返回 None（避免把本机 Send 历史当接收终态）。
async fn history_terminal_chunk_resp(
    state: &AppState,
    transfer_id: &str,
) -> Result<Option<ChunkResp>, AppError> {
    let Some(task) = state.transfer_repo.get_by_id(transfer_id).await? else {
        return Ok(None);
    };
    if task.direction != TransferDirection::Receive {
        return Ok(None);
    }
    match task.status {
        TransferStatus::Completed => Ok(Some(ChunkResp {
            success: true,
            received_bytes: task.size,
        })),
        TransferStatus::Failed | TransferStatus::Cancelled => Ok(Some(ChunkResp {
            success: false,
            received_bytes: task.transferred_bytes,
        })),
        TransferStatus::Pending | TransferStatus::Transferring => Ok(None),
    }
}

/// Business Logic（为什么需要这个函数）:
///     status 在内存 miss 时需要把持久化 Receive 终态还原，供发送端 complete 响应丢失后收敛。
///
/// Code Logic（这个函数做什么）:
///     查 history by id；Receive + completed/failed/cancelled → StatusResp；否则 None。
async fn history_terminal_status_resp(
    state: &AppState,
    transfer_id: &str,
) -> Result<Option<StatusResp>, AppError> {
    let Some(task) = state.transfer_repo.get_by_id(transfer_id).await? else {
        return Ok(None);
    };
    if task.direction != TransferDirection::Receive {
        return Ok(None);
    }
    let status = match task.status {
        TransferStatus::Completed => "completed",
        TransferStatus::Failed => "failed",
        TransferStatus::Cancelled => "cancelled",
        TransferStatus::Pending | TransferStatus::Transferring => return Ok(None),
    };
    let transferred = if task.status == TransferStatus::Completed {
        task.size
    } else {
        task.transferred_bytes
    };
    let size = task.size;
    Ok(Some(StatusResp {
        transfer_id: transfer_id.to_string(),
        status: status.to_string(),
        progress: if size == 0 {
            0.0
        } else {
            (transferred as f64) / (size as f64)
        },
        transferred_bytes: transferred,
        size,
        filename: task.filename,
    }))
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
///
/// Business Logic: 测试/非路径安全场景可用跟随 open；生产 finalize 请用 nofollow。
/// Code Logic: 跟随 open 后 8KB 分块读入 Sha256。
#[allow(dead_code)]
async fn compute_sha256(path: &Path) -> Result<String, AppError> {
    let mut file = tokio::fs::File::open(path).await?;
    hash_reader(&mut file).await
}

/// Business Logic（为什么需要这个函数）:
///     intent 恢复与 finalize 校验 .tmp 时必须证明路径 **本身**是匹配内容的普通文件。
///     普通 `File::open` / `metadata` 会跟随 symlink，链接到同尺寸同哈希目标时会误晋升或
///     把 chunk 写穿到 receive_dir 外。
///
/// Code Logic（这个函数做什么）:
///     no-follow 只读打开后流式 SHA256。
async fn compute_sha256_nofollow(path: &Path) -> Result<String, AppError> {
    let mut file = open_regular_file_nofollow(path, false).await?;
    hash_reader(&mut file).await
}

/// Business Logic（为什么需要这个函数）:
///     init resume / complete 进度必须以 .tmp **自身**长度为准；跟随 symlink 会把
///     transferred_bytes 指到外部目标长度，掩盖写穿攻击。
///
/// Code Logic（这个函数做什么）:
///     `symlink_metadata`：不存在 → None；symlink/非普通文件 → Validation 并 best-effort 删除 symlink；
///     普通文件 → Some(len)。
async fn receive_tmp_len_nofollow(path: &Path) -> Result<Option<u64>, AppError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => {
            let ft = meta.file_type();
            if ft.is_symlink() {
                let _ = tokio::fs::remove_file(path).await;
                return Err(AppError::validation(format!(
                    "临时文件是符号链接，已删除危险路径: {}",
                    path.display()
                )));
            }
            if !ft.is_file() {
                return Err(AppError::validation(format!(
                    "临时路径不是普通文件: {}",
                    path.display()
                )));
            }
            Ok(Some(meta.len()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     chunk 写入路径若用普通 OpenOptions，会跟随预置/竞态替换的 `.{id}.tmp` symlink，
///     以本机权限 seek/write 到 receive_dir 外任意可写文件。
///
/// Code Logic（这个函数做什么）:
///     1) `create_new` 首次创建普通文件（路径已是 symlink 时失败，不会跟随）；
///     2) 已存在则 no-follow 读写打开，并校验句柄对应普通文件。
async fn open_receive_tmp_rw(path: &Path) -> Result<tokio::fs::File, AppError> {
    // 先 create_new：存在（含 symlink 目录项）时不跟随、不覆盖。
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(std_file) => Ok(tokio::fs::File::from_std(std_file)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            // 续传：no-follow 读写打开既有普通文件。
            open_regular_file_nofollow(path, true).await
        }
        Err(e) => Err(AppError::from(e)),
    }
}

/// Business Logic（为什么需要这个函数）:
///     recovery、chunk 续传与 no-follow 哈希共用“拒绝 symlink、只开普通文件”语义，
///     避免 metadata 跟随与 open 跟随两套路径不一致。
///
/// Code Logic（这个函数做什么）:
///     Unix: `OpenOptionsExt::custom_flags(O_NOFOLLOW)`；Windows: `FILE_FLAG_OPEN_REPARSE_POINT`
///     打开 reparse 自身后拒绝 directory/reparse。`writable=true` 时加 write。
async fn open_regular_file_nofollow(
    path: &Path,
    writable: bool,
) -> Result<tokio::fs::File, AppError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(libc::O_NOFOLLOW);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path).map_err(|e| {
            if e.kind() == ErrorKind::Other
                || e.raw_os_error() == Some(libc::ELOOP)
                || e.raw_os_error() == Some(libc::EPERM)
            {
                std::io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("拒绝跟随符号链接打开: {}: {e}", path.display()),
                )
            } else {
                e
            }
        })?;
        let meta = std_file.metadata()?;
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000：打开 reparse point 自身而不跟随。
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            opts.write(true);
        }
        let std_file = opts.open(path)?;
        let meta = std_file.metadata()?;
        let ft = meta.file_type();
        // Windows 上 is_symlink 覆盖 symlink/junction；再拒绝目录与 reparse point。
        if ft.is_symlink()
            || !ft.is_file()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        {
            return Err(AppError::from(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("目标不是普通文件: {}", path.display()),
            )));
        }
        Ok(tokio::fs::File::from_std(std_file))
    }
    #[cfg(not(any(unix, windows)))]
    {
        // 无 no-follow 原语的平台：fail-closed。
        let _ = (path, writable);
        Err(AppError::from(std::io::Error::new(
            ErrorKind::Unsupported,
            "当前平台无法 no-follow 打开文件，拒绝打开接收临时文件",
        )))
    }
}

/// 从已打开的异步文件句柄流式计算 SHA256。
async fn hash_reader(file: &mut tokio::fs::File) -> Result<String, AppError> {
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
    use crate::storage::TransferRepo;
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
            cloud_sync_runtime: Arc::new(crate::cloud_sync::CloudSyncRuntime::new()),
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
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
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
            "/src/transfer/receiver.rs"
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
            "/src/transfer/receiver.rs"
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
            "/src/transfer/receiver.rs"
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
            "/src/transfer/receiver.rs"
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
}
