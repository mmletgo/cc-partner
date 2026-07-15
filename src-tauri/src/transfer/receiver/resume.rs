//! receiver/resume — 断点续传 offset 与终态 history/intent 恢复
//!
//! Business Logic: 进程重启后内存 active/墓碑丢失时，靠 history 与 finalize intent
//!     收敛 complete/status/最后一块重放，禁止同 transfer_id 后缀重复。
//! Code Logic: 只做 resume/terminal 回放与 recover 编排；place/commit 委托 finalize。

use super::{
    clear_finalize_intent, compute_sha256_nofollow, ensure_path_within_dir, finalize_intent_path,
    now_iso, promote_completed_to_durable, ChunkResp, FinalizeIntent, StatusResp,
};
use crate::error::AppError;
use crate::models::transfer::{TransferDirection, TransferStatus, TransferTask};
use crate::state::AppState;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

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
pub(super) async fn try_recover_finalize_intent(
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

/// Business Logic（为什么需要这个函数）:
///     complete/chunk 在内存 miss 时需要把已落盘的 Receive 历史还原为 success 响应。
///
/// Code Logic（这个函数做什么）:
///     查 transfer_history by id；仅 direction=Receive 且 status 为 completed/failed 时返回
///     ChunkResp；其它方向/状态返回 None（避免把本机 Send 历史当接收终态）。
pub(super) async fn history_terminal_chunk_resp(
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
pub(super) async fn history_terminal_status_resp(
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
pub(super) fn status_str(s: TransferStatus) -> String {
    match s {
        TransferStatus::Pending => "pending",
        TransferStatus::Transferring => "transferring",
        TransferStatus::Completed => "completed",
        TransferStatus::Failed => "failed",
        TransferStatus::Cancelled => "cancelled",
    }
    .to_string()
}
