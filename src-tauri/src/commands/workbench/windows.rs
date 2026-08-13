//! commands/workbench/windows — 工作台卫星窗 GUI 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     用户要在另一块屏打开不同项目；建窗、占用、聚焦必须只活在 GUI 进程，
//!     不得走 sidecar control，也不得 `exit_gui`。
//!
//! Code Logic（这个模块做什么）:
//!     注册 open/focus/claim/list/apply-deeplink；建窗复用 overlay 的 WebviewWindowBuilder。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::window_registry::{
    ClaimResult, WindowOpenDecision, WorkbenchWindowOccupancy, WorkbenchWindowRegistry,
};
use crate::workbench::workspace_layout::{
    parse_satellite_window_slot, window_auto_slot_key, MAIN_WINDOW_LABEL,
    WORKBENCH_WINDOW_LABEL_PREFIX,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::common::get_project;

/// 打开卫星窗结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkbenchWindowResult {
    /// focused | created
    pub action: String,
    /// 窗口 label。
    pub label: String,
    /// 占用项目。
    pub project_id: String,
}

/// claim 结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimWorkbenchProjectResult {
    /// claimed | unchanged | occupied
    pub action: String,
    /// 占用或冲突窗 label。
    pub label: String,
    /// 目标项目。
    pub project_id: String,
}

/// occupancy DTO。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchWindowOccupancyDto {
    /// 项目 id。
    pub project_id: String,
    /// 窗口 label。
    pub window_label: String,
}

/// 跨窗深链载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchApplyDeepLinkPayload {
    /// 目标项目。
    pub project_id: Option<String>,
    /// worktree。
    pub worktree_id: Option<String>,
    /// session。
    pub session_id: Option<String>,
    /// automation | files。
    pub view: Option<String>,
    /// 任务。
    pub task_id: Option<String>,
    /// outbox。
    pub outbox_id: Option<String>,
    /// 相对文件路径。
    pub path: Option<String>,
}

/// 打开或聚焦工作台卫星窗。
///
/// Business Logic（为什么需要这个命令）:
///     「在新窗口打开」必须互斥占用：已开则前置，未开则建卫星窗。
///
/// Code Logic（这个命令做什么）:
///     registry.focus_or_allocate → 已存在则 show+focus；否则 WebviewWindowBuilder 建 `workbench-N`。
#[tauri::command]
pub async fn open_workbench_window(
    app: AppHandle,
    state: State<'_, AppState>,
    registry: State<'_, WorkbenchWindowRegistry>,
    project_id: String,
) -> Result<OpenWorkbenchWindowResult, AppError> {
    let project = get_project(state.inner(), &project_id).await?;
    match registry.focus_or_allocate(&project.id)? {
        WindowOpenDecision::FocusExisting { label } => {
            focus_existing_window(&app, &label)?;
            Ok(OpenWorkbenchWindowResult {
                action: "focused".into(),
                label,
                project_id: project.id,
            })
        }
        WindowOpenDecision::Create { label } => {
            if let Err(error) = create_satellite_window(&app, &label, &project.id, &project.name) {
                registry.release_label(&label);
                return Err(error);
            }
            Ok(OpenWorkbenchWindowResult {
                action: "created".into(),
                label,
                project_id: project.id,
            })
        }
    }
}

/// 前置指定工作台窗。
///
/// Business Logic（为什么需要这个命令）:
///     Inbox / 占用冲突必须把用户带到已打开该项目的窗，而不是再开一扇。
///
/// Code Logic（这个命令做什么）:
///     校验 label 后 show + set_focus。
#[tauri::command]
pub fn focus_workbench_window(app: AppHandle, label: String) -> Result<(), AppError> {
    focus_existing_window(&app, &label)
}

/// 关闭卫星工作台窗。
///
/// Business Logic（为什么需要这个命令）:
///     删除被卫星窗占用的项目前，必须先关掉该窗释放 occupancy，避免留下幽灵占用。
///
/// Code Logic（这个命令做什么）:
///     仅允许 `workbench-1..4`；close 后 Destroyed 钩子会 release 并删 slot。
#[tauri::command]
pub fn close_workbench_window(app: AppHandle, label: String) -> Result<(), AppError> {
    if parse_satellite_window_slot(&label).is_none() {
        return Err(AppError::validation(format!(
            "workbench_window_close_satellite_only:{label}"
        )));
    }
    let Some(window) = app.get_webview_window(&label) else {
        return Ok(());
    };
    window
        .close()
        .map_err(|error| AppError::generic(format!("关闭工作台窗口失败: {error}")))
}

/// 本窗登记占用项目。
///
/// Business Logic（为什么需要这个命令）:
///     主窗切项目与卫星加载都要 claim；冲突时前端改为聚焦占用窗。
///
/// Code Logic（这个命令做什么）:
///     用调用方当前窗 label + projectId 走 registry.claim。
#[tauri::command]
pub fn claim_workbench_window_project(
    window: tauri::Window,
    registry: State<'_, WorkbenchWindowRegistry>,
    project_id: String,
) -> Result<ClaimWorkbenchProjectResult, AppError> {
    let label = window.label().to_string();
    match registry.claim(&label, &project_id)? {
        ClaimResult::Claimed => Ok(ClaimWorkbenchProjectResult {
            action: "claimed".into(),
            label,
            project_id,
        }),
        ClaimResult::Unchanged => Ok(ClaimWorkbenchProjectResult {
            action: "unchanged".into(),
            label,
            project_id,
        }),
        ClaimResult::OccupiedByOther { label: owner } => Ok(ClaimWorkbenchProjectResult {
            action: "occupied".into(),
            label: owner,
            project_id,
        }),
    }
}

/// 列出当前 occupancy。
///
/// Business Logic（为什么需要这个命令）:
///     Rail 与 Attention 需要知道项目在哪扇窗。
///
/// Code Logic（这个命令做什么）:
///     返回 registry 快照的 camelCase DTO。
#[tauri::command]
pub fn list_workbench_window_occupancy(
    registry: State<'_, WorkbenchWindowRegistry>,
) -> Result<Vec<WorkbenchWindowOccupancyDto>, AppError> {
    Ok(registry.snapshot().into_iter().map(occupancy_dto).collect())
}

/// 向指定窗投递深链。
///
/// Business Logic（为什么需要这个命令）:
///     Inbox 点到他窗占用的项目时，本窗不得切项目，只把 deep link 交给占用窗。
///
/// Code Logic（这个命令做什么）:
///     focus 后 `emit_to(label, workbench:apply-deeplink, payload)`。
#[tauri::command]
pub fn apply_workbench_window_deeplink(
    app: AppHandle,
    label: String,
    payload: WorkbenchApplyDeepLinkPayload,
) -> Result<(), AppError> {
    focus_existing_window(&app, &label)?;
    app.emit_to(&label, "workbench:apply-deeplink", payload)
        .map_err(|error| AppError::generic(format!("workbench_window_emit_failed:{error}")))
}

/// 卫星窗销毁后释放占用并删除 window auto slot。
///
/// Business Logic（为什么需要这个函数）:
///     关卫星不得锁死项目，也不得把旧现场留给下一个回收 slot。
///
/// Code Logic（这个函数做什么）:
///     release_label；若是卫星 label 则 best-effort 删对应 auto slot。
pub fn release_destroyed_workbench_window(app: &AppHandle, label: &str) {
    if label == MAIN_WINDOW_LABEL || !label.starts_with(WORKBENCH_WINDOW_LABEL_PREFIX) {
        return;
    }
    let registry = app.state::<WorkbenchWindowRegistry>();
    registry.release_label(label);
    if let Ok(slot_key) = window_auto_slot_key(label) {
        if let Some(state) = app.try_state::<AppState>() {
            let repo = state.workbench_workspace_layout_repo.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = repo.delete_window_auto_slot(&slot_key).await {
                    tracing::warn!(slot_key, error = %error, "删除卫星窗 auto layout 失败");
                }
            });
        }
    }
}

fn occupancy_dto(row: WorkbenchWindowOccupancy) -> WorkbenchWindowOccupancyDto {
    WorkbenchWindowOccupancyDto {
        project_id: row.project_id,
        window_label: row.window_label,
    }
}

fn focus_existing_window(app: &AppHandle, label: &str) -> Result<(), AppError> {
    let Some(window) = app.get_webview_window(label) else {
        return Err(AppError::not_found(format!(
            "workbench_window_not_found:{label}"
        )));
    };
    let _ = window.show();
    let _ = window.set_focus();
    Ok(())
}

fn create_satellite_window(
    app: &AppHandle,
    label: &str,
    project_id: &str,
    project_name: &str,
) -> Result<(), AppError> {
    if let Some(existing) = app.get_webview_window(label) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }

    let encoded = encode_query_component(project_id);
    let url = format!("/workbench?projectId={encoded}");
    let title = if project_name.trim().is_empty() {
        format!("{project_id} — cc-partner")
    } else {
        format!("{} — cc-partner", project_name.trim())
    };

    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(title)
        .resizable(true)
        .min_inner_size(900.0, 600.0)
        .inner_size(1200.0, 760.0);

    if let Some((x, y)) = cascade_position(app) {
        builder = builder.position(x, y);
    }

    builder
        .build()
        .map_err(|error| AppError::generic(format!("创建工作台窗口失败: {error}")))?;
    Ok(())
}

fn cascade_position(app: &AppHandle) -> Option<(f64, f64)> {
    let windows = app.webview_windows();
    let source = windows
        .values()
        .find(|window| window.is_focused().unwrap_or(false))
        .cloned()
        .or_else(|| app.get_webview_window(MAIN_WINDOW_LABEL))?;
    let pos = source.outer_position().ok()?;
    let scale = source.scale_factor().unwrap_or(1.0).max(0.0001);
    Some((pos.x as f64 / scale + 48.0, pos.y as f64 / scale + 48.0))
}

fn encode_query_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
