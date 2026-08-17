//! Workbench 设备级标语命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     顶栏标语按 owning device 存在 SQLite；选中远端项目时控制端用 deviceId 路由。
//!
//! Code Logic（这个模块做什么）:
//!     `get/save` + `*_for_state`：外机 → P2P；本机 → banner repo。

use crate::error::AppError;
use crate::state::AppState;
use crate::storage::WorkbenchBannerRepo;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_protocol::RemoteBannerSaveReq;
use tauri::State;

pub use crate::workbench::remote_protocol::WorkbenchBannerDto;

use super::common::{device_base_url, proxy_workbench_if_gui};

/// Business Logic（为什么需要这个函数）:
///     Settings/本机项目读本机；只有选中外机时才走 P2P。
fn is_foreign_device(state: &AppState, device_id: Option<&str>) -> bool {
    let device_id = device_id.unwrap_or("").trim();
    !device_id.is_empty() && device_id != state.device_id.as_str()
}

/// Business Logic（为什么需要这个函数）:
///     命令层不把 banner repo 挂进 AppState，避免改 15+ 处测试 fixture。
fn banner_repo(state: &AppState) -> WorkbenchBannerRepo {
    WorkbenchBannerRepo::with_gate(state.db.clone(), state.maintenance_gate.clone())
}

/// 读取设备标语。
///
/// Business Logic（为什么需要这个命令）:
///     打开工作台顶栏时加载 owning device 标语。
#[tauri::command]
pub async fn get_workbench_banner(
    state: State<'_, AppState>,
    device_id: Option<String>,
) -> Result<WorkbenchBannerDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "banner.get",
            serde_json::json!({ "deviceId": device_id }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    get_workbench_banner_for_state(state.inner(), device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享读取；外机必须读对端。
pub async fn get_workbench_banner_for_state(
    state: &AppState,
    device_id: Option<String>,
) -> Result<WorkbenchBannerDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(device_id.trim())
            .get_banner(&base_url)
            .await;
    }
    local_get_workbench_banner(state).await
}

/// Business Logic（为什么需要这个函数）:
///     P2P owner 路由只读本机单行表。
pub(crate) async fn local_get_workbench_banner(
    state: &AppState,
) -> Result<WorkbenchBannerDto, AppError> {
    match banner_repo(state).get().await? {
        Some(row) => Ok(WorkbenchBannerDto {
            markdown: row.markdown,
            updated_at: row.updated_at,
        }),
        None => Ok(WorkbenchBannerDto {
            markdown: String::new(),
            updated_at: String::new(),
        }),
    }
}

/// 保存设备标语。
///
/// Business Logic（为什么需要这个命令）:
///     用户编辑后覆盖 owning device SQLite。
#[tauri::command]
pub async fn save_workbench_banner(
    state: State<'_, AppState>,
    markdown: String,
    device_id: Option<String>,
) -> Result<WorkbenchBannerDto, AppError> {
    if is_foreign_device(state.inner(), device_id.as_deref()) {
        if let Some(v) = proxy_workbench_if_gui(
            state.inner(),
            "banner.save",
            serde_json::json!({
                "markdown": markdown.clone(),
                "deviceId": device_id,
            }),
        )
        .await?
        {
            return Ok(v);
        }
    }
    save_workbench_banner_for_state(state.inner(), markdown, device_id).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享写入；外机必须写对端。
pub async fn save_workbench_banner_for_state(
    state: &AppState,
    markdown: String,
    device_id: Option<String>,
) -> Result<WorkbenchBannerDto, AppError> {
    if is_foreign_device(state, device_id.as_deref()) {
        let device_id = device_id.unwrap_or_default();
        let base_url = device_base_url(state, device_id.trim())?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(device_id.trim())
            .save_banner(&base_url, RemoteBannerSaveReq { markdown })
            .await;
    }
    local_save_workbench_banner(state, markdown).await
}

/// Business Logic（为什么需要这个函数）:
///     P2P owner 路由只写本机单行表。
pub(crate) async fn local_save_workbench_banner(
    state: &AppState,
    markdown: String,
) -> Result<WorkbenchBannerDto, AppError> {
    let row = banner_repo(state).upsert(&markdown).await?;
    Ok(WorkbenchBannerDto {
        markdown: row.markdown,
        updated_at: row.updated_at,
    })
}
