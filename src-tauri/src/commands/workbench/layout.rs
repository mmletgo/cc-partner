//! Workbench workspace layout / safe restore 命令。
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要保存/读取 auto layout 与命名 snapshot，并在启动时 preflight + apply。
//!
//! Code Logic（这个模块做什么）:
//!     Tauri commands + control 可复用 for_state helpers；remote project 的 deep preflight/attach
//!     走 owning device，layout 行始终留在控制设备。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use crate::workbench::remote_protocol::{
    RemoteSafeAttachReq, RemoteWorkspaceRestorePreflightReq,
};
use crate::workbench::workspace_layout::{
    desktop_auto_slot_key, WorkspaceLayout, WorkspaceLayoutDraft,
};
use crate::workbench::workspace_restore::{
    ensure_plan_layout_revision, preflight_workspace_restore, safe_attach_workbench_session,
    RestoreInspectionContext, RestorePlanStatus, RestoreSkipReason, WorkspaceRestoreAction,
    WorkspaceRestoreOutcome, WorkspaceRestorePlan,
};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::common::{
    ensure_remote_project_context, get_project, proxy_workbench_if_gui, remote_inner_worktree_id,
};

/// apply 请求体。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyWorkspaceRestoreReq {
    /// preflight 产出的计划。
    pub plan: WorkspaceRestorePlan,
}

/// apply 结果摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyWorkspaceRestoreResult {
    /// restore id。
    pub restore_id: String,
    /// 最终状态。
    pub status: RestorePlanStatus,
    /// 成功项数。
    pub restored_count: u32,
    /// 跳过项数。
    pub skipped_count: u32,
    /// 执行后的动作（含 attach 结果）。
    pub actions: Vec<WorkspaceRestoreAction>,
}

/// save layout 请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveWorkspaceLayoutReq {
    /// draft。
    pub draft: WorkspaceLayoutDraft,
    /// CAS expected revision；新建 auto/named 为 null。
    pub expected_revision: Option<u64>,
}

/// preflight 请求（可按 slot 或显式 layout）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightWorkspaceRestoreReq {
    /// slot key，默认 desktop:auto。
    pub slot_key: Option<String>,
    /// 可选显式 layout id。
    pub layout_id: Option<String>,
}

/// 读取 layout（按 slot）。
///
/// Business Logic（为什么需要这个命令）:
///     autosave/preflight 需要当前 revision 与结构 selection。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理到 sidecar；owner 读 repo。
#[tauri::command]
pub async fn get_workspace_layout(
    state: State<'_, AppState>,
    slot_key: Option<String>,
) -> Result<Option<WorkspaceLayout>, AppError> {
    let slot = slot_key.unwrap_or_else(|| desktop_auto_slot_key().to_string());
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "layout.get",
        serde_json::json!({ "slotKey": slot.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_workspace_layout_for_state(state.inner(), &slot).await
}

/// Business Logic（为什么需要这个函数）:
///     control/HTTP 与 invoke 共享读取逻辑。
///
/// Code Logic（这个函数做什么）:
///     `get_by_slot`。
pub async fn get_workspace_layout_for_state(
    state: &AppState,
    slot_key: &str,
) -> Result<Option<WorkspaceLayout>, AppError> {
    state
        .workbench_workspace_layout_repo
        .get_by_slot(slot_key)
        .await
}

/// 保存 layout（CAS）。
///
/// Business Logic（为什么需要这个命令）:
///     稳定 selection 500ms 后合并写入。
///
/// Code Logic（这个命令做什么）:
///     GUI 代理；owner `save_cas`。
#[tauri::command]
pub async fn save_workspace_layout(
    state: State<'_, AppState>,
    draft: WorkspaceLayoutDraft,
    expected_revision: Option<u64>,
) -> Result<WorkspaceLayout, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "layout.save",
        serde_json::json!({
            "draft": draft.clone(),
            "expectedRevision": expected_revision,
        }),
    )
    .await?
    {
        return Ok(v);
    }
    save_workspace_layout_for_state(state.inner(), draft, expected_revision).await
}

/// Business Logic（为什么需要这个函数）:
///     控制面与 invoke 共享 CAS 写入。
///
/// Code Logic（这个函数做什么）:
///     委托 layout repo。
pub async fn save_workspace_layout_for_state(
    state: &AppState,
    draft: WorkspaceLayoutDraft,
    expected_revision: Option<u64>,
) -> Result<WorkspaceLayout, AppError> {
    state
        .workbench_workspace_layout_repo
        .save_cas(draft, expected_revision)
        .await
}

/// 列出命名 snapshot。
///
/// Business Logic（为什么需要这个命令）:
///     Snapshot 对话框展示用户保存的结构现场。
///
/// Code Logic（这个命令做什么）:
///     `list_named`。
#[tauri::command]
pub async fn list_named_workspace_layouts(
    state: State<'_, AppState>,
) -> Result<Vec<WorkspaceLayout>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "layout.listNamed", serde_json::json!({}))
        .await?
    {
        return Ok(v);
    }
    list_named_workspace_layouts_for_state(state.inner()).await
}

/// Business Logic（为什么需要这个函数）:
///     control 与 invoke 共享 list。
///
/// Code Logic（这个函数做什么）:
///     repo.list_named。
pub async fn list_named_workspace_layouts_for_state(
    state: &AppState,
) -> Result<Vec<WorkspaceLayout>, AppError> {
    state.workbench_workspace_layout_repo.list_named().await
}

/// 删除命名 snapshot。
///
/// Business Logic（为什么需要这个命令）:
///     用户删除命名现场；禁止删 auto。
///
/// Code Logic（这个命令做什么）:
///     `delete_named`。
#[tauri::command]
pub async fn delete_named_workspace_layout(
    state: State<'_, AppState>,
    id: String,
) -> Result<(), AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "layout.deleteNamed",
        serde_json::json!({ "id": id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    delete_named_workspace_layout_for_state(state.inner(), &id).await
}

/// Business Logic（为什么需要这个函数）:
///     control/invoke 共享删除。
///
/// Code Logic（这个函数做什么）:
///     repo.delete_named。
pub async fn delete_named_workspace_layout_for_state(
    state: &AppState,
    id: &str,
) -> Result<(), AppError> {
    state.workbench_workspace_layout_repo.delete_named(id).await
}

/// preflight 恢复计划。
///
/// Business Logic（为什么需要这个命令）:
///     打开 Workbench 时先纯读生成计划，再决定 selection/attach。
///
/// Code Logic（这个命令做什么）:
///     读 layout → 若 project 为 remote shortcut 则转发 owner 的 inner preflight，否则本机 preflight。
#[tauri::command]
pub async fn preflight_workspace_restore_cmd(
    state: State<'_, AppState>,
    slot_key: Option<String>,
    layout_id: Option<String>,
) -> Result<WorkspaceRestorePlan, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "layout.preflight",
        serde_json::json!({
            "slotKey": slot_key.clone(),
            "layoutId": layout_id.clone(),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    preflight_workspace_restore_for_state(state.inner(), slot_key, layout_id).await
}

/// Business Logic（为什么需要这个函数）:
///     控制设备持有 layout；remote 只把 inner id 发给 owner 做资源探测。
///
/// Code Logic（这个函数做什么）:
///     加载 layout → 分支 local/remote → 合成计划。
pub async fn preflight_workspace_restore_for_state(
    state: &AppState,
    slot_key: Option<String>,
    layout_id: Option<String>,
) -> Result<WorkspaceRestorePlan, AppError> {
    let layout = load_layout(state, slot_key.as_deref(), layout_id.as_deref()).await?;
    let project = match state.workbench_project_repo.get(&layout.project_id).await? {
        Some(p) => p,
        None => {
            let ctx = RestoreInspectionContext::from_state(state.clone());
            return preflight_workspace_restore(&ctx, &layout).await;
        }
    };

    if project.kind == "remote" {
        return preflight_remote_layout(state, &layout, &project).await;
    }

    let ctx = RestoreInspectionContext::from_state(state.clone());
    preflight_workspace_restore(&ctx, &layout).await
}

/// apply 计划：校验 revision，执行 safeAttach 列表项。
///
/// Business Logic（为什么需要这个命令）:
///     前端 selection 之外，唯一允许的服务端副作用是幂等 tmux attach。
///
/// Code Logic（这个命令做什么）:
///     ensure revision → 对 safeAttach 动作执行 attach（remote 转发 owner）。
#[tauri::command]
pub async fn apply_workspace_restore_cmd(
    state: State<'_, AppState>,
    plan: WorkspaceRestorePlan,
) -> Result<ApplyWorkspaceRestoreResult, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "layout.apply",
        serde_json::json!({ "plan": plan.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    apply_workspace_restore_for_state(state.inner(), plan).await
}

/// Business Logic（为什么需要这个函数）:
///     invoke/control 共享 apply。
///
/// Code Logic（这个函数做什么）:
///     CAS revision → 执行 safeAttach（本机或 owner）。
pub async fn apply_workspace_restore_for_state(
    state: &AppState,
    plan: WorkspaceRestorePlan,
) -> Result<ApplyWorkspaceRestoreResult, AppError> {
    ensure_plan_layout_revision(state, &plan).await?;

    let layout = state
        .workbench_workspace_layout_repo
        .get_by_id(&plan.layout_id)
        .await?
        .ok_or_else(|| AppError::not_found("workspace_layout_not_found".to_string()))?;
    let project = state
        .workbench_project_repo
        .get(&layout.project_id)
        .await?;

    let mut actions = plan.actions.clone();
    let mut restored = 0u32;
    let mut skipped = 0u32;

    for action in &mut actions {
        match action.outcome {
            WorkspaceRestoreOutcome::Select | WorkspaceRestoreOutcome::Reuse => {
                restored += 1;
            }
            WorkspaceRestoreOutcome::Skip => {
                skipped += 1;
            }
            WorkspaceRestoreOutcome::SafeAttach => {
                let Some(session_id) = action.resource_id.clone() else {
                    action.outcome = WorkspaceRestoreOutcome::Skip;
                    action.reason = Some(RestoreSkipReason::SessionMissing);
                    skipped += 1;
                    continue;
                };
                let attach_result = if project.as_ref().is_some_and(|p| p.kind == "remote") {
                    let project = project.as_ref().expect("remote project");
                    safe_attach_remote_session(state, project, &session_id).await
                } else {
                    let ctx = RestoreInspectionContext::from_state(state.clone());
                    safe_attach_workbench_session(&ctx, &session_id)
                        .await
                        .map(|_| ())
                };
                match attach_result {
                    Ok(()) => restored += 1,
                    Err(_) => {
                        action.outcome = WorkspaceRestoreOutcome::Skip;
                        action.reason = Some(RestoreSkipReason::TmuxTargetMissing);
                        skipped += 1;
                    }
                }
            }
        }
    }

    let status = if skipped == 0 {
        RestorePlanStatus::Complete
    } else if restored == 0 {
        RestorePlanStatus::Empty
    } else {
        RestorePlanStatus::Partial
    };

    Ok(ApplyWorkspaceRestoreResult {
        restore_id: plan.restore_id,
        status,
        restored_count: restored,
        skipped_count: skipped,
        actions,
    })
}

/// owner-local preflight（P2P / 本机 inner IDs only）。
///
/// Business Logic（为什么需要这个函数）:
///     owning device 只根据 inner project/worktree/session 做纯读探测，不读写 controller layout。
///
/// Code Logic（这个函数做什么）:
///     拒绝 remote shortcut；合成临时 layout 跑 preflight。
pub async fn owner_local_preflight_for_state(
    state: &AppState,
    project_id: String,
    active_worktree_id: Option<String>,
    active_session_id: Option<String>,
    workspace_view: crate::workbench::workspace_layout::WorkspaceView,
    inspector_tab: crate::workbench::workspace_layout::InspectorTab,
    browser_target_url: Option<String>,
) -> Result<WorkspaceRestorePlan, AppError> {
    let project = state
        .workbench_project_repo
        .get(&project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project_not_found".to_string()))?;
    if project.kind != "local" {
        return Err(AppError::validation("local_project_required".to_string()));
    }

    let synthetic = WorkspaceLayout {
        schema_version: crate::workbench::workspace_layout::WORKSPACE_LAYOUT_SCHEMA_VERSION,
        id: format!("owner-local:{}", uuid::Uuid::new_v4()),
        slot_key: desktop_auto_slot_key().to_string(),
        kind: crate::workbench::workspace_layout::WorkspaceLayoutKind::Auto,
        name: None,
        project_id,
        active_worktree_id,
        active_session_id,
        workspace_view,
        inspector_tab,
        browser_target_url,
        revision: 0,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let ctx = RestoreInspectionContext::from_state(state.clone());
    preflight_workspace_restore(&ctx, &synthetic).await
}

/// owner-local safe attach。
///
/// Business Logic（为什么需要这个函数）:
///     controller 在 apply 时把 inner sessionId 交给 owner 做幂等 attach。
///
/// Code Logic（这个函数做什么）:
///     校验 session 所属 project 为 local；执行 safe_attach。
pub async fn owner_local_safe_attach_for_state(
    state: &AppState,
    session_id: String,
) -> Result<crate::workbench::workspace_restore::SafeAttachResult, AppError> {
    let row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("session_not_found".to_string()))?;
    let project = state
        .workbench_project_repo
        .get(&row.project_id)
        .await?
        .ok_or_else(|| AppError::not_found("project_not_found".to_string()))?;
    if project.kind != "local" {
        return Err(AppError::validation("local_project_required".to_string()));
    }
    let ctx = RestoreInspectionContext::from_state(state.clone());
    safe_attach_workbench_session(&ctx, &session_id).await
}

async fn load_layout(
    state: &AppState,
    slot_key: Option<&str>,
    layout_id: Option<&str>,
) -> Result<WorkspaceLayout, AppError> {
    if let Some(id) = layout_id {
        return state
            .workbench_workspace_layout_repo
            .get_by_id(id)
            .await?
            .ok_or_else(|| AppError::not_found("workspace_layout_not_found".to_string()));
    }
    let slot = slot_key.unwrap_or_else(|| desktop_auto_slot_key());
    state
        .workbench_workspace_layout_repo
        .get_by_slot(slot)
        .await?
        .ok_or_else(|| AppError::not_found("workspace_layout_not_found".to_string()))
}

async fn preflight_remote_layout(
    state: &AppState,
    layout: &WorkspaceLayout,
    project: &crate::workbench::models::WorkbenchProjectRow,
) -> Result<WorkspaceRestorePlan, AppError> {
    let context = match ensure_remote_project_context(state, project).await {
        Ok(ctx) => ctx,
        Err(err) => {
            // offline / unsupported → partial：仅允许 controller 侧 select project
            let reason = if err.code().contains("unsupported")
                || err.to_string().contains("unsupported")
            {
                RestoreSkipReason::CapabilityUnsupported
            } else {
                RestoreSkipReason::RemoteOffline
            };
            return Ok(partial_remote_controller_only(layout, reason));
        }
    };

    let inner_worktree =
        remote_inner_worktree_id(&context.device_id, layout.active_worktree_id.clone())?;
    // session id on remote shortcut is owner-local id already when stored as inner; if mapped use as-is
    let inner_session = layout.active_session_id.clone();

    let client = RemoteWorkbenchClient::new();
    match client
        .preflight_workspace_restore(
            &context.base_url,
            &RemoteWorkspaceRestorePreflightReq {
                project_id: context.inner_project_id.clone(),
                active_worktree_id: inner_worktree,
                active_session_id: inner_session,
                workspace_view: layout.workspace_view,
                inspector_tab: layout.inspector_tab,
                browser_target_url: layout.browser_target_url.clone(),
            },
        )
        .await
    {
        Ok(mut plan) => {
            // 绑定 controller layout 身份
            plan.layout_id = layout.id.clone();
            plan.layout_revision = layout.revision;
            // 保证 project select 使用 controller shortcut id
            if let Some(action) = plan.actions.iter_mut().find(|a| a.target == "project") {
                action.resource_id = Some(layout.project_id.clone());
                action.outcome = WorkspaceRestoreOutcome::Select;
                action.reason = None;
            }
            plan.resolved_project_id = Some(layout.project_id.clone());
            Ok(plan)
        }
        Err(err) => {
            let reason = if err.code().contains("unsupported") {
                RestoreSkipReason::CapabilityUnsupported
            } else {
                RestoreSkipReason::RemoteOffline
            };
            Ok(partial_remote_controller_only(layout, reason))
        }
    }
}

fn partial_remote_controller_only(
    layout: &WorkspaceLayout,
    reason: RestoreSkipReason,
) -> WorkspaceRestorePlan {
    WorkspaceRestorePlan {
        restore_id: uuid::Uuid::new_v4().to_string(),
        layout_id: layout.id.clone(),
        layout_revision: layout.revision,
        status: RestorePlanStatus::Partial,
        resolved_project_id: Some(layout.project_id.clone()),
        resolved_worktree_id: None,
        resolved_session_id: None,
        workspace_view: layout.workspace_view,
        inspector_tab: layout.inspector_tab,
        browser_target_url: None,
        actions: vec![
            WorkspaceRestoreAction {
                target: "project".to_string(),
                resource_id: Some(layout.project_id.clone()),
                outcome: WorkspaceRestoreOutcome::Select,
                reason: None,
            },
            WorkspaceRestoreAction {
                target: "worktree".to_string(),
                resource_id: layout.active_worktree_id.clone(),
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(reason),
            },
            WorkspaceRestoreAction {
                target: "session".to_string(),
                resource_id: layout.active_session_id.clone(),
                outcome: WorkspaceRestoreOutcome::Skip,
                reason: Some(reason),
            },
        ],
    }
}

async fn safe_attach_remote_session(
    state: &AppState,
    project: &crate::workbench::models::WorkbenchProjectRow,
    session_id: &str,
) -> Result<(), AppError> {
    let context = ensure_remote_project_context(state, project).await?;
    let client = RemoteWorkbenchClient::new();
    client
        .safe_attach_session(
            &context.base_url,
            &RemoteSafeAttachReq {
                session_id: session_id.to_string(),
            },
        )
        .await
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::models::WorkbenchProjectRow;
    use crate::workbench::workspace_layout::{
        InspectorTab, WorkspaceLayoutKind, WorkspaceView, WORKSPACE_LAYOUT_SCHEMA_VERSION,
    };

    #[test]
    fn local_project_required_code_is_stable() {
        let err = AppError::validation("local_project_required".to_string());
        assert_eq!(err.code(), "local_project_required");
    }

    #[test]
    fn partial_remote_keeps_controller_project_only() {
        let layout = WorkspaceLayout {
            schema_version: WORKSPACE_LAYOUT_SCHEMA_VERSION,
            id: "L1".to_string(),
            slot_key: desktop_auto_slot_key().to_string(),
            kind: WorkspaceLayoutKind::Auto,
            name: None,
            project_id: "remote-shortcut".to_string(),
            active_worktree_id: Some("w".to_string()),
            active_session_id: Some("s".to_string()),
            workspace_view: WorkspaceView::Terminal,
            inspector_tab: InspectorTab::Files,
            browser_target_url: None,
            revision: 3,
            created_at: "t".to_string(),
            updated_at: "t".to_string(),
        };
        let plan = partial_remote_controller_only(&layout, RestoreSkipReason::RemoteOffline);
        assert_eq!(plan.resolved_project_id.as_deref(), Some("remote-shortcut"));
        assert!(plan.resolved_session_id.is_none());
        assert_eq!(plan.layout_revision, 3);
        // 不得出现绝对路径
        let json = serde_json::to_string(&plan).unwrap();
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("C:\\\\"));
        let _ = WorkbenchProjectRow {
            id: "x".into(),
            name: "n".into(),
            kind: "remote".into(),
            device_id: "d".into(),
            device_name: "n".into(),
            path: "/secret".into(),
            last_opened_at: "t".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
        };
    }
}
