//! commands/transfer.rs — 文件传输命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端传输面板通过 invoke 调用：列出传输任务（活跃+历史）、发起发送、取消任务。
//!     对照 Python `/api/transfer/tasks`、`/api/transfer/send`、`DELETE /api/transfer/tasks/{id}`。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_transfers`：合并 registry 活跃任务 + transfer_history 历史，按 created_at 倒序，
//!       转为 TransferTaskDto（camelCase）返回。
//!     - `send_transfer`：调 `transfer::sender::start_sending`（内部 spawn 异步任务），
//!       立即返回 `{accepted, deviceId, filePath}`。
//!     - `cancel_transfer`：触发 CancellationToken，返回 `{ok, id}`。

use crate::error::AppError;
use crate::models::transfer::TransferTaskDto;
use crate::state::AppState;
use crate::transfer::sender;
use tauri::{AppHandle, State};

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
    app_handle: AppHandle,
    device_id: String,
    file_path: String,
) -> Result<serde_json::Value, AppError> {
    let transfer_id = sender::start_sending(
        state.inner().clone(),
        app_handle,
        device_id.clone(),
        file_path.clone(),
    )?;
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
