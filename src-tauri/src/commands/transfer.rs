//! commands/transfer.rs — 文件传输命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端传输面板通过 invoke 调用：列出传输任务（活跃+历史）、发起发送、取消任务、
//!     幂等 retry/resume（clientOperationId）、uncertain operation 对账查询。
//!     对照 Python `/api/transfer/tasks`、`/api/transfer/send`、`DELETE /api/transfer/tasks/{id}`。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_transfers`：合并 registry 活跃任务 + transfer_history 历史，按 created_at 倒序，
//!       转为 TransferTaskDto（camelCase）返回。
//!     - `send_transfer`：调 `transfer::sender::start_sending`（内部 spawn 异步任务），
//!       立即返回 `{accepted, deviceId, filePath}`。
//!     - `cancel_transfer`：触发 CancellationToken，返回 `{ok, id}`。
//!     - `retry_transfer` / `resume_transfer`：本机 owner 路径幂等 claim 后 spawn。
//!     - `get_transfer_operation`：按发送端 clientOperationId 查询 ledger 真值。

use crate::error::AppError;
use crate::models::transfer::{TransferOperationStatus, TransferTaskDto};
use crate::state::AppState;
use crate::transfer::sender;
use tauri::State;

/// 列出全部传输任务（活跃 + 历史），按创建时间倒序。
///
/// Business Logic: 前端传输面板展示进行中任务与已结束历史。对照 Python `/api/transfer/tasks`。
/// Code Logic: 合并 registry.list()（活跃）与 transfer_repo.list()（历史，去重活跃 id），
///     按 created_at 倒序，转为 TransferTaskDto。
#[tauri::command]
pub async fn list_transfers(state: State<'_, AppState>) -> Result<Vec<TransferTaskDto>, AppError> {
    let active = state.transfers.list();
    let history = state.transfer_repo.list().await?;

    // 活跃任务 id 集合（历史中同 id 的视为活跃的旧快照，优先用活跃版本）
    let active_ids: std::collections::HashSet<String> =
        active.iter().map(|t| t.id.clone()).collect();

    let mut all: Vec<crate::models::transfer::TransferTask> = active;
    for t in history {
        if !active_ids.contains(&t.id) {
            all.push(t);
        }
    }
    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(all.iter().map(|t| t.to_dto(None)).collect())
}

/// 发起文件发送：异步 spawn，立即返回 transfer_id。
///
/// Business Logic: 前端选择文件与目标设备后调用；后端 spawn 异步发送任务并立即返回，
///     前端通过 listen('transfer:progress') 等事件追踪进度。对照 Python `/api/transfer/send`。
#[tauri::command]
pub async fn send_transfer(
    state: State<'_, AppState>,
    device_id: String,
    file_path: String,
) -> Result<serde_json::Value, AppError> {
    let transfer_id =
        sender::start_sending(state.inner().clone(), device_id.clone(), file_path.clone())?;
    tracing::info!("已发起传输任务 {transfer_id} → {device_id}");
    Ok(serde_json::json!({
        "accepted": true,
        "deviceId": device_id,
        "filePath": file_path,
        "id": transfer_id,
    }))
}

/// 取消传输任务：触发 CancellationToken。
///
/// Business Logic: 前端传输项"取消"按钮调用。对照 Python `DELETE /api/transfer/tasks/{id}`。
/// Code Logic: registry.cancel(id) 触发对应任务的取消令牌；发送循环在下一块前检查并停止。
#[tauri::command]
pub async fn cancel_transfer(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<serde_json::Value, AppError> {
    let ok = state.transfers.cancel(&task_id);
    if !ok {
        return Err(AppError::not_found(format!("传输任务不存在: {task_id}")));
    }
    Ok(serde_json::json!({ "ok": true, "id": task_id }))
}

/// 幂等重新传输（新 protocol id，同 logical transfer）。
///
/// Business Logic（为什么需要这个命令）:
///     失败且可重试的发送任务需要用户显式“重新传输”；同一 clientOperationId 不得重复 attempt。
///
/// Code Logic（这个函数做什么）:
///     委托 `sender::retry_transfer`；返回新/回放 attempt 的 DTO。
#[tauri::command]
pub async fn retry_transfer(
    state: State<'_, AppState>,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTaskDto, AppError> {
    let task = sender::retry_transfer(
        state.inner().clone(),
        task_id,
        client_operation_id,
    )
    .await?;
    Ok(task.to_dto(None))
}

/// 幂等断点续传（复用稳定 protocol transfer id）。
///
/// Business Logic（为什么需要这个命令）:
///     有 resume metadata 且对端支持时从 checkpoint 继续；旧 peer 返回 unsupported。
///
/// Code Logic（这个函数做什么）:
///     委托 `sender::resume_transfer`；返回新/回放 attempt 的 DTO。
#[tauri::command]
pub async fn resume_transfer(
    state: State<'_, AppState>,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTaskDto, AppError> {
    let task = sender::resume_transfer(
        state.inner().clone(),
        task_id,
        client_operation_id,
    )
    .await?;
    Ok(task.to_dto(None))
}

/// 查询发送端 clientOperationId 的 operation 真值。
///
/// Business Logic（为什么需要这个命令）:
///     transport timeout / lost final ACK 后 UI 必须先对账，禁止盲重试。
///
/// Code Logic（这个命令做什么）:
///     委托 `sender::get_transfer_operation`；返回 camelCase `TransferOperationStatus`。
#[tauri::command]
pub async fn get_transfer_operation(
    state: State<'_, AppState>,
    client_operation_id: String,
) -> Result<TransferOperationStatus, AppError> {
    sender::get_transfer_operation(state.inner(), &client_operation_id).await
}
