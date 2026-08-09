//! 终端会话与 Claude session
//!
//! Business Logic（为什么需要这个模块）:
//!     拆分 monofile 中的本领域命令。
//!
//! Code Logic（这个模块做什么）:
//!     命令与 pub(crate) helper。

use crate::claude_cli;
use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::claude_sessions::{
    ensure_worktree_session_index_scanned, search_sessions_result, to_session_preview,
    ClaudeSessionIndex, SessionPreview, SessionSearchResult,
};
use crate::workbench::models::{
    WorkbenchFileNode, WorkbenchPathInfo, WorkbenchSessionDto, WorkbenchSessionRow,
};
use crate::workbench::sessions::{
    kill_persisted_backend, PaneCloseOutcome, PaneSplitDirection, WorkbenchSessionReplayDto,
};
use crate::workbench::{
    fs as workbench_fs,
    remote_client::RemoteWorkbenchClient,
    remote_ids::{parse_remote_entity_id, remote_entity_id},
    remote_protocol::{
        RemoteClaudeSessionReq, RemoteCreatePathReq, RemoteCreateSessionReq, RemoteDeletePathReq,
        RemoteRenamePathReq, RemoteSearchClaudeSessionsReq, ResumeClaudeSessionResult,
    },
};
use std::path::PathBuf;
use tauri::State;

use super::common::*;
use super::files::list_workbench_sessions_for_state;

/// 列出工作台终端会话。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要按项目查看本机或远端 terminal window。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 State 后委托 for_state helper。
#[tauri::command]
pub async fn list_workbench_sessions(
    state: State<'_, AppState>,
    project_id: Option<String>,
) -> Result<Vec<WorkbenchSessionDto>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.list",
        serde_json::json!({ "projectId": project_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    list_workbench_sessions_for_state(state.inner(), project_id).await
}

/// 在项目目录中创建一个普通 PTY 终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     用户在工作台中打开终端时，应只进入当前项目根目录的 shell，不自动运行 Claude Code。
///
/// Code Logic（这个函数做什么）:
///     读取项目路径；调用 session registry 按前端初始尺寸创建 shell/tmux 会话，写入 SQLite，
///     并通过 Tauri event 推送输出与状态。
pub(crate) async fn local_create_workbench_session(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
) -> Result<WorkbenchSessionDto, AppError> {
    state.runtime_role.require_owner()?;
    // R29 H1：在 get_project/resolve_worktree 之前持有 project op lease，
    // 防止 remove 在项目读取后完成 bulk delete 并清除 barrier，create 再用陈旧快照 orphan spawn。
    let _project_lease = state
        .workbench_sessions
        .try_acquire_project_op_lease(&project_id)?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let row = state.workbench_sessions.create(
        state.clone(),
        project,
        worktree.path.clone(),
        Some(worktree.id.clone()),
        Some(worktree.name.clone()),
        initial_cols,
        initial_rows,
    )?;
    // RAII：upsert 失败时 Drop 自动 close，避免 ghost PTY/registry。
    let mut spawn_guard = crate::workbench::sessions::SessionSpawnGuard::new_with_state(
        (*state.workbench_sessions).clone(),
        row.id.clone(),
        state.clone(),
    );
    // R26 M1：upsert 前再 revalidate project barrier；失败 reclaim PTY。
    // R30 M2：SessionSpawnGuard 只杀 PTY，不 destroy tmux window；失败路径显式 kill_persisted_backend。
    if let Err(error) = state
        .workbench_sessions
        .require_project_not_closing(&row.project_id)
    {
        drop(spawn_guard);
        // create 失败 reclaim：best-effort destroy；metadata 尚未 durable 提交成功，
        // kill 失败仍返回原始业务错误（不伪装 create 成功）。
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(error);
    }
    if let Err(error) = state.workbench_session_repo.upsert(&row).await {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(error);
    }
    // upsert 后再 revalidate：project remove 可能在写库期间完成。
    if state
        .workbench_sessions
        .require_project_not_closing(&row.project_id)
        .is_err()
    {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(AppError::unavailable(
            "project_closing_barrier_active".to_string(),
        ));
    }
    // R20 M1：generation CAS 失败不得对外宣称 running/Ready。
    if !spawn_guard.commit() {
        // commit 失败：Drop 回收 PTY；仍需销毁 create 新建的 tmux window。
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(AppError::unavailable(
            "session_spawn_ready_cas_miss".to_string(),
        ));
    }
    // lease drops after Ready commit when function returns.
    Ok(row.to_dto())
}

/// 创建带预分配 terminal/agent session UUID 的本机 shell。
///
/// Business Logic（为什么需要这个函数）:
///     OpenCode runtime bridge 要求 shell 启动前 env 中已有 `CC_PARTNER_AGENT_SESSION_ID`，
///     且 Agent Runtime 行使用同一 UUID。
///
/// Code Logic（这个函数做什么）:
///     与 `local_create_workbench_session` 相同的 lease/upsert/Ready CAS 路径，
///     但调用 `create_with_preallocated_ids`。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn local_create_workbench_session_with_preallocated_ids(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
    terminal_session_id: String,
    agent_session_id: String,
) -> Result<WorkbenchSessionDto, AppError> {
    state.runtime_role.require_owner()?;
    let _project_lease = state
        .workbench_sessions
        .try_acquire_project_op_lease(&project_id)?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let row = state.workbench_sessions.create_with_preallocated_ids(
        state.clone(),
        project,
        worktree.path.clone(),
        Some(worktree.id.clone()),
        Some(worktree.name.clone()),
        initial_cols,
        initial_rows,
        terminal_session_id,
        agent_session_id,
    )?;
    let mut spawn_guard = crate::workbench::sessions::SessionSpawnGuard::new_with_state(
        (*state.workbench_sessions).clone(),
        row.id.clone(),
        state.clone(),
    );
    if let Err(error) = state
        .workbench_sessions
        .require_project_not_closing(&row.project_id)
    {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(error);
    }
    if let Err(error) = state.workbench_session_repo.upsert(&row).await {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(error);
    }
    if state
        .workbench_sessions
        .require_project_not_closing(&row.project_id)
        .is_err()
    {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(AppError::unavailable(
            "project_closing_barrier_active".to_string(),
        ));
    }
    if !spawn_guard.commit() {
        drop(spawn_guard);
        if let Err(kill_error) = kill_persisted_backend(&row) {
            tracing::debug!(error = %kill_error, "create reclaim kill_persisted_backend failed");
        }
        return Err(AppError::unavailable(
            "session_spawn_ready_cas_miss".to_string(),
        ));
    }
    Ok(row.to_dto())
}

/// 拉取工作台终端最近输出。
///
/// Business Logic（为什么需要这个函数）:
///     移动端首次打开本机或远端 terminal 时，需要先 replay 最近输出，再消费 live 事件。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 转发到项目所在设备并把返回 sessionId 映射回本机；local sessionId 直接读取本机 registry buffer。
pub(crate) async fn replay_workbench_session_for_state(
    state: &AppState,
    session_id: String,
) -> Result<WorkbenchSessionReplayDto, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        let mut replay = RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .replay(&base_url, &inner_session_id)
            .await?;
        replay.session_id = session_id.clone();
        // remote stream owner 来自 peer replay DTO；与 bridged live 的 producer owner 合成同一 composite。
        // 远端 backend 重启后 remote owner 变化 → authority cutover → lastSeq 重置，避免静默冻结。
        let remote_owner = replay.owner_instance_id.clone();
        let local_bus = state.config_runtime.owner_instance_id();
        replay.owner_instance_id = Some(
            crate::workbench::terminal_authority::terminal_stream_authority(
                &session_id,
                local_bus,
                remote_owner.as_deref(),
            ),
        );
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(replay);
    }
    // R15 M1：原子 Live|RestoreInProgress|Missing；restore 中 → recoverable unavailable。
    state
        .workbench_sessions
        .require_live_for_replay(&session_id)?;
    let mut replay = state.workbench_sessions.replay(&session_id);
    // 本机 PTY ring 的 authority 即 sidecar ownerInstanceId。
    replay.owner_instance_id = Some(state.config_runtime.owner_instance_id().to_string());
    Ok(replay)
}

/// 拉取工作台终端最近输出（Tauri 命令入口）。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Provider 在 listener 就绪后需要对活跃 session 做 baseline replay，
///     以补上 React 挂载前已发出的 ring 输出，并用 lastSeq 做 stream cutover。
///
/// Code Logic（这个函数做什么）:
///     GuiClient 经 control proxy；Owner 本地走 `replay_workbench_session_for_state`。
#[tauri::command]
pub async fn replay_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<WorkbenchSessionReplayDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.replay",
        serde_json::json!({ "sessionId": session_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    replay_workbench_session_for_state(state.inner(), session_id).await
}

/// 在项目目录中创建一个普通 PTY 终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 remote shortcut 上打开终端时，真实 shell 应运行在远端设备；本机项目仍走本机 registry。
///
/// Code Logic（这个函数做什么）:
///     remote：先 `try_acquire_project_op_lease(project_id)`，lease 内 re-get durable project，
///     再 open context、create-session、最终 revalidate；
///     revalidate 失败 best-effort close 远端 session；成功则
///     `ensure_session_watch_for_project` 登记 watch+project map 并映射 DTO。
///     local：委托 local_create（自身已持 project lease）。
pub(crate) async fn create_workbench_session_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
) -> Result<WorkbenchSessionDto, AppError> {
    // R42 H2：先按 project_id 取得 lease（不读 project 快照），再在 lease 内 re-get。
    // 若非 remote，立刻释放 lease 并走 local_create（其内部会重新 acquire）。
    let project_lease = state
        .workbench_sessions
        .try_acquire_project_op_lease(&project_id)?;
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        // R41 M1 / R42 H2：ProjectOpLease 全程覆盖 remote create RPC + mapping + project-scoped watch。
        let _project_lease = project_lease;
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        ensure_remote_event_bridge_for_context(state, &context);
        let item = RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .create_session(
                &context.base_url,
                RemoteCreateSessionReq {
                    project_id: context.inner_project_id.clone(),
                    worktree_id: inner_worktree_id,
                    initial_cols,
                    initial_rows,
                },
            )
            .await?;
        // R41 M1：远端已创建成功后若 project 进入 closing，best-effort 关闭远端 session，
        // 禁止本机无 shortcut 可管理的孤儿终端。
        if state
            .workbench_sessions
            .require_project_not_closing(&project_id)
            .is_err()
        {
            let inner_session_id = item.id.clone();
            if let Err(close_error) = RemoteWorkbenchClient::new()
                .with_expected_device_id(&context.device_id)
                .close_session(&context.base_url, &inner_session_id)
                .await
            {
                tracing::debug!(
                    error = %close_error,
                    session_id = %inner_session_id,
                    "remote create compensation close failed after project closing"
                );
            }
            return Err(AppError::unavailable(
                "project_closing_barrier_active".to_string(),
            ));
        }
        let session =
            map_remote_session_dtos(&context.device_id, &context.local_project_id, vec![item])
                .into_iter()
                .next()
                .ok_or_else(|| AppError::generic("远端 session 创建结果为空"))?;
        // R35 M2 / R41 M2：session-keyed watch + project_running_sessions 归属，
        // 使 create-only（未 list）会话在 project remove 时也能被 clear 释放。
        let _ = state
            .workbench_remote_event_bridges
            .ensure_session_watch_for_project(
                &context.device_id,
                &context.local_project_id,
                &session.id,
            );
        return Ok(session);
    }
    drop(project_lease);
    local_create_workbench_session(state, project_id, worktree_id, initial_cols, initial_rows).await
}

/// 在项目目录中创建一个普通 PTY 终端会话。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要在本机或远端项目中创建 terminal window。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 State 后委托 for_state helper。
#[tauri::command]
pub async fn create_workbench_session(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    initial_cols: Option<u16>,
    initial_rows: Option<u16>,
) -> Result<WorkbenchSessionDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "sessions.create", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "initialCols": initial_cols.clone(), "initialRows": initial_rows.clone() })).await? {
        return Ok(v);
    }
    create_workbench_session_for_state(
        state.inner(),
        project_id,
        worktree_id,
        initial_cols,
        initial_rows,
    )
    .await
}

/// 向工作台终端写入输入。
///
/// Business Logic（为什么需要这个函数）:
///     xterm 捕获到用户键盘输入后，需要把字节流转发给对应 PTY。
///
/// Code Logic（这个函数做什么）:
///     查找 session writer，写入 UTF-8 字符串并 flush，成功返回 sessionId。
pub(crate) async fn local_write_workbench_session_input(
    state: &AppState,
    session_id: String,
    data: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    state.workbench_sessions.write_input(&session_id, &data)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 向工作台终端写入输入。
///
/// Business Logic（为什么需要这个函数）:
///     本机与 remote terminal 都通过同一前端 API 接收 xterm 输入。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 解析设备和 inner id 后转发；local sessionId 调用本地 helper。
pub(crate) async fn write_workbench_session_input_for_state(
    state: &AppState,
    session_id: String,
    data: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .write_input(&base_url, &inner_session_id, &data)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    local_write_workbench_session_input(state, session_id, data).await
}

/// 向工作台终端写入输入。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 terminal 需要把键盘输入写入本机或远端 PTY/tmux。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn write_workbench_session_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.write",
        serde_json::json!({ "sessionId": session_id.clone(), "data": data.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    write_workbench_session_input_for_state(state.inner(), session_id, data).await
}

/// 将桌面交互式终端输入接纳到常驻输入流。
///
/// Business Logic（为什么需要这个命令）:
///     xterm 的按键路径只能等待 GUI 本机有界队列，不能等待 loopback HTTP、远端 RTT 或 PTY ACK。
///
/// Code Logic（这个命令做什么）:
///     GuiClient 委托 BackendControlClientRuntime 的 WS actor；owner 测试/特殊宿主直接写本地 PTY。
#[tauri::command]
pub async fn enqueue_workbench_terminal_input(
    state: State<'_, AppState>,
    session_id: String,
    data: String,
) -> Result<serde_json::Value, AppError> {
    if state.runtime_role == crate::backend::authority::RuntimeRole::GuiClient {
        state
            .backend_control_client_runtime
            .enqueue_terminal_input(state.ui.clone(), session_id.clone(), data)?;
        return Ok(serde_json::json!({ "accepted": true, "sessionId": session_id }));
    }
    local_write_workbench_session_input(state.inner(), session_id.clone(), data).await?;
    Ok(serde_json::json!({ "accepted": true, "sessionId": session_id }))
}

/// 调整工作台终端尺寸。
///
/// Business Logic（为什么需要这个函数）:
///     终端面板尺寸变化时，PTY 子进程需要收到新的 cols/rows，避免输出换行错乱。
///
/// Code Logic（这个函数做什么）:
///     更新 registry 中的 row 尺寸，调用 MasterPty::resize，并写回 SQLite。
pub(crate) async fn local_resize_workbench_session(
    state: &AppState,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let row = state.workbench_sessions.resize(&session_id, cols, rows)?;
    state.workbench_session_repo.upsert(&row).await?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 调整工作台终端尺寸。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 的尺寸变化也必须转发到远端 PTY/tmux，保证交互式程序布局正确。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 resize；local sessionId 调用本地 helper。
pub(crate) async fn resize_workbench_session_for_state(
    state: &AppState,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .resize(&base_url, &inner_session_id, cols, rows)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    local_resize_workbench_session(state, session_id, cols, rows).await
}

/// 调整工作台终端尺寸。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 terminal viewport 变化需要同步到本机或远端 PTY/tmux。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn resize_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "sessions.resize", serde_json::json!({ "sessionId": session_id.clone(), "cols": cols.clone(), "rows": rows.clone() })).await? {
        return Ok(v);
    }
    resize_workbench_session_for_state(state.inner(), session_id, cols, rows).await
}

/// 聚焦工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     顶部 app tab 与真实 tmux window 一一绑定，用户切换 tab 时终端内容也必须切到对应 window。
///
/// Code Logic（这个函数做什么）:
///     调用 registry 对 tmux-backed 会话执行 select-window；raw PTY fallback 直接视为成功。
pub(crate) async fn local_focus_workbench_session(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    state.workbench_sessions.focus_window(&session_id)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 聚焦工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal tab 切换时，远端 tmux current window 需要同步切换。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 focus；local sessionId 调用本地 helper。
pub(crate) async fn focus_workbench_session_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        // 本机 active tab 是带宽过滤权威；先切流再同步 tmux focus，focus 失败也不得继续拉旧窗口正文。
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        let _ = state
            .workbench_remote_event_bridges
            .set_active_terminal_session(
                &parsed.device_id,
                inner_session_id.clone(),
                state.clone(),
            );
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .focus(&base_url, &inner_session_id)
            .await?;
        // 切换过滤目标后立即发权威屏幕快照；即使新窗口暂时没有增量输出，UI 也能立即恢复内容。
        let replay = replay_workbench_session_for_state(state, session_id.clone()).await?;
        crate::workbench::remote_events::emit_remote_terminal_resync(state, replay);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    let _ = state
        .workbench_remote_event_bridges
        .clear_all_active_terminal_sessions(state.clone());
    local_focus_workbench_session(state, session_id).await
}

/// 释放远端终端窗口的实时正文订阅。
///
/// Business Logic（为什么需要这个函数）:
///     active tab effect cleanup、项目切换和离开 Workbench 后，旧窗口必须立即停止实时正文传输。
///
/// Code Logic（这个函数做什么）:
///     remote id 解析后执行 compare-and-clear；local session 无远程带宽，直接成功。
pub(crate) fn deactivate_workbench_terminal_stream_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        let _ = state
            .workbench_remote_event_bridges
            .clear_active_terminal_session_if(&parsed.device_id, &inner_session_id, state.clone());
    }
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 聚焦工作台终端 window。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 terminal tab 切换需要同步本机或远端 tmux current window。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn focus_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    stream_active: Option<bool>,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.focus",
        serde_json::json!({
            "sessionId": session_id.clone(),
            "streamActive": stream_active.unwrap_or(true),
        }),
    )
    .await?
    {
        return Ok(v);
    }
    if stream_active == Some(false) {
        return deactivate_workbench_terminal_stream_for_state(state.inner(), session_id);
    }
    focus_workbench_session_for_state(state.inner(), session_id).await
}

/// 获取当前 worktree 聚焦的工作台终端 window。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在 tmux 底部 status bar 或快捷键中切换 window，顶部 app tab 需要跟随真实 tmux current window。
///
/// Code Logic（这个函数做什么）:
///     校验项目存在，读取当前 worktree tmux session 当前 window id，并映射成 Workbench sessionId 返回。
pub(crate) async fn get_focused_workbench_session_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<serde_json::Value, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        let session_id = RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .focused(
                &context.base_url,
                &context.inner_project_id,
                inner_worktree_id.as_deref(),
            )
            .await?
            .map(|inner| remote_entity_id(&context.device_id, &inner));
        ensure_remote_event_bridge_for_context(state, &context);
        return Ok(serde_json::json!({ "sessionId": session_id }));
    }
    let session_id = state
        .workbench_sessions
        .focused_session_id(&project_id, worktree_id.as_deref())?;
    Ok(serde_json::json!({ "sessionId": session_id }))
}

/// 获取当前 worktree 聚焦的工作台终端 window。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要跟随本机或远端 tmux 当前 window。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn get_focused_workbench_session(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.focused",
        serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    get_focused_workbench_session_for_state(state.inner(), project_id, worktree_id).await
}

/// 分割当前 tmux window 的 pane。
///
/// Business Logic（为什么需要这个函数）:
///     工作台采用真实 tmux 映射后，用户需要在当前 window 内创建左右或上下 pane。
///
/// Code Logic（这个函数做什么）:
///     校验 direction 字符串，读取会话 row，调用 registry 按 row.cwd 执行带 cwd 的 tmux split-window。
pub(crate) async fn local_split_workbench_pane(
    state: &AppState,
    session_id: String,
    direction: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let split_direction = PaneSplitDirection::from_api(&direction)?;
    let _row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    state
        .workbench_sessions
        .split_pane(&session_id, split_direction)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "direction": direction }))
}

/// 分割当前 tmux window 的 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 也需要支持 pane 分屏，真实 split-window 在远端设备执行。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 split-pane；local sessionId 调用本地 helper。
pub(crate) async fn split_workbench_pane_for_state(
    state: &AppState,
    session_id: String,
    direction: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .split_pane(&base_url, &inner_session_id, &direction)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(
            serde_json::json!({ "ok": true, "sessionId": session_id, "direction": direction }),
        );
    }
    local_split_workbench_pane(state, session_id, direction).await
}

/// 分割当前 tmux window 的 pane。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要在本机或远端 terminal window 内创建 pane。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn split_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
    direction: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.split",
        serde_json::json!({ "sessionId": session_id.clone(), "direction": direction.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    split_workbench_pane_for_state(state.inner(), session_id, direction).await
}

/// 切换当前 tmux window 到下一个 pane。
///
/// Business Logic（为什么需要这个函数）:
///     用户在同一个 terminal window 分出多个 pane 后，需要快速循环切换 active pane。
///
/// Code Logic（这个函数做什么）:
///     确认 session row 存在后调用 registry 在 running tmux-backed session 上执行 select-pane，并返回 `{ok, sessionId}`。
pub(crate) async fn local_switch_workbench_pane(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let _row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    state.workbench_sessions.switch_to_next_pane(&session_id)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 切换当前 tmux window 到下一个 pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 也需要支持 pane 间切换，真实 select-pane 必须在项目所在设备执行。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 switch-pane；local sessionId 调用本地 helper。
pub(crate) async fn switch_workbench_pane_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .switch_pane(&base_url, &inner_session_id)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    local_switch_workbench_pane(state, session_id).await
}

/// 切换当前 tmux window 到下一个 pane。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要在本机或远端 terminal window 内循环切换 active pane。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn switch_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.switch",
        serde_json::json!({ "sessionId": session_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    switch_workbench_pane_for_state(state.inner(), session_id).await
}

/// 按终端字符格坐标选中 tmux pane。
///
/// Business Logic（为什么需要这个函数）:
///     用户在多 pane window 内应能直接点击目标 pane 切换 active pane，而不是反复按循环切换按钮猜位置。
///
/// Code Logic（这个函数做什么）:
///     确认 session row 存在后调用 registry 坐标命中；命中并切换时返回 `{ok, sessionId, paneId, changed:true}`，
///     zoom / 单 pane / 边框 / 已 active 一律返回 `changed:false`。
pub(crate) async fn local_select_workbench_pane_at(
    state: &AppState,
    session_id: String,
    col: u32,
    row: u32,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let _row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    let pane_id = state
        .workbench_sessions
        .select_pane_at(&session_id, col, row)?;
    Ok(serde_json::json!({
        "ok": true,
        "sessionId": session_id,
        "paneId": pane_id,
        "changed": pane_id.is_some(),
    }))
}

/// 按终端字符格坐标选中 tmux pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 也需要点击切换 pane，真实 select-pane 必须在项目所在设备执行。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 select-pane-at；local sessionId 调用本地 helper。
pub(crate) async fn select_workbench_pane_at_for_state(
    state: &AppState,
    session_id: String,
    col: u32,
    row: u32,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        let remote = RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .select_pane_at(&base_url, &inner_session_id, col, row)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(serde_json::json!({
            "ok": true,
            "sessionId": session_id,
            "paneId": remote.pane_id,
            "changed": remote.changed,
        }));
    }
    local_select_workbench_pane_at(state, session_id, col, row).await
}

/// 按终端字符格坐标选中 tmux pane。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要在本机或远端 terminal window 内用鼠标点击切换 active pane。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn select_workbench_pane_at(
    state: State<'_, AppState>,
    session_id: String,
    col: u32,
    row: u32,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.select_pane_at",
        serde_json::json!({ "sessionId": session_id.clone(), "col": col, "row": row }),
    )
    .await?
    {
        return Ok(v);
    }
    select_workbench_pane_at_for_state(state.inner(), session_id, col, row).await
}

/// 确保当前 tmux active pane 以单 pane 视图显示。
///
/// Business Logic（为什么需要这个函数）:
///     移动端屏幕空间有限，新增/切换/关闭 pane 后应只显示当前 active pane，而不是保留桌面分屏布局。
///
/// Code Logic（这个函数做什么）:
///     确认 session row 存在；raw/disconnected session 返回 no-op，tmux-backed running session 调用 registry 幂等 ensure-zoom。
pub(crate) async fn local_zoom_workbench_pane(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let row = state
        .workbench_session_repo
        .get(&session_id)
        .await?
        .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
    if !should_attempt_session_zoom(&row) {
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    state
        .workbench_sessions
        .ensure_active_pane_zoomed(&session_id)?;
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 判断 session 是否需要尝试 tmux zoom。
///
/// Business Logic（为什么需要这个函数）:
///     移动端单 pane 视图只适用于 running tmux window；raw PTY 或 disconnected window 不应因为 zoom-pane 报错。
///
/// Code Logic（这个函数做什么）:
///     读取持久化 row 的 status/backend/window id，只有 running + tmux + window id 同时满足时返回 true。
pub(crate) fn should_attempt_session_zoom(row: &WorkbenchSessionRow) -> bool {
    row.status == "running" && row.backend == "tmux" && row.backend_window_id.is_some()
}

/// 确保当前 tmux active pane 以单 pane 视图显示。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 的移动端单 pane 视图也必须由项目所在设备的 tmux window 负责。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 zoom-pane；local sessionId 调用本地 helper。
pub(crate) async fn zoom_workbench_pane_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .zoom_pane(&base_url, &inner_session_id)
            .await?;
        ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    local_zoom_workbench_pane(state, session_id).await
}

/// 确保当前 tmux active pane 以单 pane 视图显示。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端和移动端都需要把本机或远端 active pane 切到单 pane 视图。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn zoom_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.zoom",
        serde_json::json!({ "sessionId": session_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    zoom_workbench_pane_for_state(state.inner(), session_id).await
}

/// 关闭当前 tmux pane。
///
/// Business Logic（为什么需要这个函数）:
///     用户点击分屏工具栏 X 时，需要关闭当前 active pane；最后一个 pane 会关闭整个 window。
///
/// Code Logic（这个函数做什么）:
///     调用 registry 关闭当前 active pane；若关闭了 window，则销毁持久后端并删除 SQLite row。
pub(crate) async fn local_close_workbench_pane(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    match state.workbench_sessions.close_active_pane(&session_id)? {
        PaneCloseOutcome::PaneClosed { renamed } => {
            // title-owner 交接后若重贴了 auto 标题，立即持久化并推送 session-updated。
            if let Some(row) = renamed {
                if let Err(err) = state.workbench_session_repo.upsert(&row).await {
                    tracing::debug!("pane 交接后自动标题持久化失败: {err}");
                }
                crate::workbench::sessions::emit_session_updated(state, &row);
            }
            Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "closedWindow": false }))
        }
        PaneCloseOutcome::WindowClosed(cleanup) => {
            // R24 H1 / R35 M3：kill 成功才 delete + finish_cleanup；
            // kill 失败保留 barrier/row（SessionCloseCleanup Drop 不清 barrier）。
            kill_persisted_backend(cleanup.row())?;
            state.workbench_session_repo.delete(&session_id).await?;
            cleanup.finish_cleanup();
            Ok(serde_json::json!({ "ok": true, "sessionId": session_id, "closedWindow": true }))
        }
    }
}

/// 关闭当前 tmux pane。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal 关闭 pane 时，远端设备需要返回是否关闭了整个 window。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 close-pane 并保留本机 remote sessionId；local sessionId 调用本地 helper。
pub(crate) async fn close_workbench_pane_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        let closed_window = RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .close_pane(&base_url, &inner_session_id)
            .await?;
        if closed_window {
            // R35 M2：关闭整个 window 等价于关闭 tab：仅释放该 session 的 watch lease。
            let _ = state
                .workbench_remote_event_bridges
                .release_session_watch(&parsed.device_id, &session_id);
        } else {
            // 仍有 pane：保持 bridge + 本 session watch。
            ensure_remote_event_bridge_for_device(state, &parsed.device_id, &base_url);
            let _ = state
                .workbench_remote_event_bridges
                .ensure_session_watch(&parsed.device_id, &session_id);
        }
        return Ok(
            serde_json::json!({ "ok": true, "sessionId": session_id, "closedWindow": closed_window }),
        );
    }
    local_close_workbench_pane(state, session_id).await
}

/// 关闭当前 tmux pane。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要关闭本机或远端 terminal 当前 active pane。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn close_workbench_pane(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.close_pane",
        serde_json::json!({ "sessionId": session_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    close_workbench_pane_for_state(state.inner(), session_id).await
}

/// 关闭工作台终端 tab。
///
/// Business Logic（为什么需要这个函数）:
///     用户关闭 tab 后，该会话应从运行期 registry 和 SQLite 中移除，并释放 PTY/tmux 资源。
///
/// Code Logic（这个函数做什么）:
///     优先关闭 registry 中的运行期句柄；若 registry 已无该会话但 SQLite 仍有记录，
///     R25 H1：仍 install close intent/tombstone（取消 restoring claim），再 kill/delete/finish，
///     禁止 claim-held 无 live handle 在 delete 后 re-upsert 复活。
pub(crate) async fn local_close_workbench_session(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    // R24 H1 / R25 H1：registry close 或 missing-handle close intent；kill/SQLite 成功后再 finish。
    match state.workbench_sessions.close(&session_id) {
        Ok(cleanup) => {
            // R35 M3：仅 kill Ok 才 delete + finish；Err 时 Drop 保留 barrier。
            kill_persisted_backend(cleanup.row())?;
            state.workbench_session_repo.delete(&session_id).await?;
            cleanup.finish_cleanup();
        }
        Err(AppError::NotFound(_)) => {
            let row = state
                .workbench_session_repo
                .get(&session_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
            // R25 H1：无 live handle 也要 close intent，阻断 stale restore re-upsert。
            let cleanup = match state
                .workbench_sessions
                .begin_close_intent_for_missing_handle(&session_id, row.clone())
            {
                Ok(cleanup) => cleanup,
                Err(AppError::Conflict(_)) => {
                    // 竞态：claim 窗口内已 insert live；改走 registry close。
                    match state.workbench_sessions.close(&session_id) {
                        Ok(cleanup) => cleanup,
                        Err(AppError::NotFound(_)) => {
                            // live 又被并发 close：仍用 durable row 装 intent。
                            state
                                .workbench_sessions
                                .begin_close_intent_for_missing_handle(&session_id, row)?
                        }
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            };
            kill_persisted_backend(cleanup.row())?;
            state.workbench_session_repo.delete(&session_id).await?;
            cleanup.finish_cleanup();
        }
        Err(error) => return Err(error),
    }
    Ok(serde_json::json!({ "ok": true, "sessionId": session_id }))
}

/// 关闭工作台终端 tab。
///
/// Business Logic（为什么需要这个函数）:
///     用户关闭 remote terminal tab 时，应清理远端设备上的真实 terminal window。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 close；local sessionId 调用本地 helper。
pub(crate) async fn close_workbench_session_for_state(
    state: &AppState,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .close_session(&base_url, &inner_session_id)
            .await?;
        // R35 M2：关闭远端 terminal window 后仅释放该 session 的 watch lease，
        // 其它仍打开的 remote terminal 保持 subscribers>0。
        let _ = state
            .workbench_remote_event_bridges
            .release_session_watch(&parsed.device_id, &session_id);
        return Ok(serde_json::json!({ "ok": true, "sessionId": session_id }));
    }
    local_close_workbench_session(state, session_id).await
}

/// 关闭工作台终端 tab。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端需要关闭本机或远端 terminal window。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn close_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.close",
        serde_json::json!({ "sessionId": session_id.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    close_workbench_session_for_state(state.inner(), session_id).await
}

/// 重命名工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     同一项目可打开多个终端，用户需要给 tab 起名区分不同任务。
///
/// Code Logic（这个函数做什么）:
///     更新运行期 row 或持久化 row 的 name 字段并返回最新会话。
pub(crate) async fn local_rename_workbench_session(
    state: &AppState,
    session_id: String,
    name: String,
) -> Result<WorkbenchSessionDto, AppError> {
    state.runtime_role.require_owner()?;
    match state.workbench_sessions.rename(&session_id, &name) {
        Ok(row) => {
            state.workbench_session_repo.upsert(&row).await?;
            crate::workbench::sessions::emit_session_updated(state, &row);
            Ok(row.to_dto_with_pane_count(
                crate::workbench::sessions::pane_count_for_row(&row),
            ))
        }
        Err(AppError::NotFound(_)) => {
            let mut row = state
                .workbench_session_repo
                .get(&session_id)
                .await?
                .ok_or_else(|| AppError::not_found("工作台会话不存在"))?;
            row.name = name.trim().to_string();
            row.name_source = "manual".to_string();
            row.updated_at = now_iso();
            state.workbench_session_repo.upsert(&row).await?;
            crate::workbench::sessions::mark_session_name_manual(&session_id);
            crate::workbench::sessions::emit_session_updated(state, &row);
            Ok(row.to_dto())
        }
        Err(error) => Err(error),
    }
}

/// 重命名工作台终端会话。
///
/// Business Logic（为什么需要这个函数）:
///     remote terminal tab 重命名应写入远端 registry/SQLite/tmux window，本机只接收映射后的 DTO。
///
/// Code Logic（这个函数做什么）:
///     remote sessionId 走远端 rename，按返回 inner projectId 恢复本机 shortcut projectId 后映射 DTO；local sessionId 调用本地 helper。
#[tauri::command]
pub async fn rename_workbench_session(
    state: State<'_, AppState>,
    session_id: String,
    name: String,
) -> Result<WorkbenchSessionDto, AppError> {
    if let Some(v) = proxy_workbench_if_gui(
        state.inner(),
        "sessions.rename",
        serde_json::json!({ "sessionId": session_id.clone(), "name": name.clone() }),
    )
    .await?
    {
        return Ok(v);
    }
    if let Some(parsed) = parse_remote_entity_id(&session_id) {
        let base_url = device_base_url(&state, &parsed.device_id)?;
        let inner_session_id = remote_inner_session_id(&parsed.device_id, &session_id)?;
        let item = RemoteWorkbenchClient::new()
            .with_expected_device_id(&parsed.device_id)
            .rename_session(&base_url, &inner_session_id, &name)
            .await?;
        let local_project_id = local_project_id_for_remote_inner_project(
            &state,
            &parsed.device_id,
            &base_url,
            &item.project_id,
        )
        .await?;
        ensure_remote_event_bridge_for_project_mapping(
            &state,
            &parsed.device_id,
            &base_url,
            &item.project_id,
            &local_project_id,
        );
        return map_remote_session_dtos_with_project(
            &parsed.device_id,
            Some(&local_project_id),
            vec![item],
        )
        .into_iter()
        .next()
        .ok_or_else(|| AppError::generic("远端 session 重命名结果为空"));
    }
    local_rename_workbench_session(&state, session_id, name).await
}

/// 列出项目目录下的一级文件节点。
///
/// Business Logic（为什么需要这个函数）:
///     右侧检查器需要交互式展开项目文件夹，本期先提供文件树，后续再做文件预览。
///
/// Code Logic（这个函数做什么）:
///     读取项目根路径，把阻塞 list_dir 放入 spawn_blocking 执行；path 为空表示项目根。
pub(crate) async fn local_list_workbench_dir(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: Option<String>,
) -> Result<Vec<WorkbenchFileNode>, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let relative = path.unwrap_or_default();
    run_blocking_fs(move || workbench_fs::list_dir(&root, &relative)).await
}

/// 列出项目目录下的一级文件节点。
///
/// Business Logic（为什么需要这个函数）:
///     右侧检查器需要交互式展开项目文件夹，本期先提供文件树，后续再做文件预览。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把相对路径转发到远端设备；local 项目走原有本机文件树 helper。
pub(crate) async fn list_workbench_dir_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: Option<String>,
) -> Result<Vec<WorkbenchFileNode>, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .list_workbench_dir(
                &context.base_url,
                &context.inner_project_id,
                inner_worktree_id.as_deref(),
                path.as_deref(),
            )
            .await;
    }
    local_list_workbench_dir(state, project_id, worktree_id, path).await
}

/// 列出项目目录下的一级文件节点。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端文件树需要展开本机或远端项目目录。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn list_workbench_dir(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: Option<String>,
) -> Result<Vec<WorkbenchFileNode>, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.list_dir", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone() })).await? {
        return Ok(v);
    }
    list_workbench_dir_for_state(state.inner(), project_id, worktree_id, path).await
}

/// 查询项目内某个路径的信息。
///
/// Business Logic（为什么需要这个函数）:
///     前端选中文件或文件夹后，需要在检查器里显示类型、大小和更新时间。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用 path_info，并保留项目根路径边界检查。
pub(crate) async fn local_get_workbench_path_info(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchPathInfo, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || workbench_fs::path_info(&root, &path)).await
}

/// 查询项目内某个路径的信息。
///
/// Business Logic（为什么需要这个函数）:
///     前端选中文件或文件夹后，需要在检查器里显示类型、大小和更新时间。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把请求转发到远端设备；local 项目走原有本机 path_info helper。
pub(crate) async fn get_workbench_path_info_for_state(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchPathInfo, AppError> {
    let project = get_project(state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .workbench_path_info(
                &context.base_url,
                &context.inner_project_id,
                inner_worktree_id.as_deref(),
                &path,
            )
            .await;
    }
    local_get_workbench_path_info(state, project_id, worktree_id, path).await
}

/// 查询项目内某个路径的信息。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端文件详情面板需要读取本机或远端项目内路径 metadata。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包参数后委托 for_state helper。
#[tauri::command]
pub async fn get_workbench_path_info(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<WorkbenchPathInfo, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.info", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone() })).await? {
        return Ok(v);
    }
    get_workbench_path_info_for_state(state.inner(), project_id, worktree_id, path).await
}

/// 在项目内创建文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台快速创建项目文件，为后续代码或文档编辑打基础。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中验证父路径与单个文件名，create_new 空文件后返回 PathInfo。
pub(crate) async fn local_create_workbench_file(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || workbench_fs::create_file(&root, &parent_path, &name)).await
}

/// 在项目内创建文件。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台快速创建项目文件，为后续代码或文档编辑打基础。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把创建请求转发到远端设备；local 项目走原有本机 create_file helper。
#[tauri::command]
pub async fn create_workbench_file(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.create_file", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "parentPath": parent_path.clone(), "name": name.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .create_file(
                &context.base_url,
                RemoteCreatePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    parent_path,
                    name,
                },
            )
            .await;
    }
    local_create_workbench_file(&state, project_id, worktree_id, parent_path, name).await
}

/// 在项目内创建文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台整理项目结构，新建文件夹承载代码、素材或文档。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中验证父路径与单个目录名，创建目录后返回 PathInfo。
pub(crate) async fn local_create_workbench_dir(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || workbench_fs::create_dir(&root, &parent_path, &name)).await
}

/// 在项目内创建文件夹。
///
/// Business Logic（为什么需要这个函数）:
///     用户可从工作台整理项目结构，新建文件夹承载代码、素材或文档。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把创建请求转发到远端设备；local 项目走原有本机 create_dir helper。
#[tauri::command]
pub async fn create_workbench_dir(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    parent_path: String,
    name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.create_dir", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "parentPath": parent_path.clone(), "name": name.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .create_dir(
                &context.base_url,
                RemoteCreatePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    parent_path,
                    name,
                },
            )
            .await;
    }
    local_create_workbench_dir(&state, project_id, worktree_id, parent_path, name).await
}

/// 重命名项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中重命名文件或文件夹，但不能覆盖已有路径或逃出项目根目录。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用安全 rename_path，保留 Phase B 的 symlink/path 边界检查。
pub(crate) async fn local_rename_workbench_path(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    new_name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    run_blocking_fs(move || workbench_fs::rename_path(&root, &path, &new_name)).await
}

/// 重命名项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中重命名文件或文件夹，但不能覆盖已有路径或逃出项目根目录。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把重命名请求转发到远端设备；local 项目走原有本机 rename helper。
#[tauri::command]
pub async fn rename_workbench_path(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
    new_name: String,
) -> Result<WorkbenchPathInfo, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.rename", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone(), "newName": new_name.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .rename_path(
                &context.base_url,
                RemoteRenamePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                    new_name,
                },
            )
            .await;
    }
    local_rename_workbench_path(&state, project_id, worktree_id, path, new_name).await
}

/// 删除项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中删除项目内文件或文件夹；删除项目根目录被明确拒绝。
///
/// Code Logic（这个函数做什么）:
///     在 blocking pool 中调用 delete_path；symlink 删除只删除链接本身，不删除目标文件。
pub(crate) async fn local_delete_workbench_path(
    state: &AppState,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<serde_json::Value, AppError> {
    state.runtime_role.require_owner()?;
    let project = get_project(state, &project_id).await?;
    let worktree = resolve_worktree(state, &project, worktree_id.as_deref()).await?;
    let root = PathBuf::from(worktree.path);
    let deleted_path = path.clone();
    run_blocking_fs(move || workbench_fs::delete_path(&root, &path)).await?;
    Ok(serde_json::json!({ "ok": true, "path": deleted_path }))
}

/// 删除项目内路径。
///
/// Business Logic（为什么需要这个函数）:
///     用户可在文件树中删除项目内文件或文件夹；删除项目根目录被明确拒绝。
///
/// Code Logic（这个函数做什么）:
///     remote 项目把删除请求转发到远端设备；local 项目走原有本机 delete helper。
#[tauri::command]
pub async fn delete_workbench_path(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    path: String,
) -> Result<serde_json::Value, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "files.delete", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "path": path.clone() })).await? {
        return Ok(v);
    }
    let project = get_project(&state, &project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(&state, &project).await?;
        let inner_worktree_id = remote_inner_worktree_id(&context.device_id, worktree_id)?;
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .delete_path(
                &context.base_url,
                RemoteDeletePathReq {
                    project_id: context.inner_project_id,
                    worktree_id: inner_worktree_id,
                    path,
                },
            )
            .await;
    }
    local_delete_workbench_path(&state, project_id, worktree_id, path).await
}

// ---------------------------------------------------------------------------
// Claude session 搜索 / preview / resume（Phase 2）
// ---------------------------------------------------------------------------

/// 搜索 worktree 范围内的 Claude Code 历史 session。
///
/// Business Logic（为什么需要这个函数）:
///     用户在 Workbench 终端想找回之前的 Claude Code 会话继续对话；本机项目在本机扫描 jsonl 索引，
///     remote shortcut 则把搜索请求转发到项目所在设备解析 transcript，避免本机读取不到远端文件。
///
/// Code Logic（这个函数做什么）:
///     读 project row；local 分支 await ensure（singleflight+spawn_blocking）后读锁 search_sessions_result；
///     remote 分支委托 remote_client（双形态解码为 SessionSearchResult）。
pub(crate) async fn search_claude_sessions_for_state(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    query: &str,
) -> Result<SessionSearchResult, AppError> {
    let project = get_project(state, project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            remote_inner_worktree_id(&context.device_id, worktree_id.map(str::to_string))?;
        ensure_remote_event_bridge_for_context(state, &context);
        let req = RemoteSearchClaudeSessionsReq {
            project_id: context.inner_project_id.clone(),
            worktree_id: inner_worktree_id,
            query: query.to_string(),
        };
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .search_claude_sessions(&context.base_url, req)
            .await;
    }
    // local 分支
    let worktree = resolve_worktree(state, &project, worktree_id).await?;
    let shared =
        ensure_worktree_session_index_scanned(state, std::path::Path::new(&worktree.path)).await;
    let index = shared.read().expect("session index 读锁中毒");
    Ok(search_sessions_result(&index, query, 50))
}

/// 搜索 worktree 范围内的 Claude Code 历史 session。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端搜索面板需要按关键词返回本机或远端历史 Claude 会话（含 truncated/diagnostics）。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 State 后委托 for_state helper。
#[tauri::command]
pub async fn search_claude_sessions(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    query: String,
) -> Result<SessionSearchResult, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "claude.search", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "query": query.clone() })).await? {
        return Ok(v);
    }
    search_claude_sessions_for_state(state.inner(), &project_id, worktree_id.as_deref(), &query)
        .await
}

/// 读取单个 Claude session 的 preview 详情。
///
/// Business Logic（为什么需要这个函数）:
///     用户在搜索结果选中一条 session 后，preview 面板需要展示最近对话、cwd、git 分支等详情；
///     本机从内存索引取，remote shortcut 转发到项目所在设备解析 transcript。
///
/// Code Logic（这个函数做什么）:
///     读 project row；local 分支取索引读锁后从 sessions.get(session_id) 拿 ClaudeSessionIndex 并 to_session_preview，
///     不存在返回 not_found；remote 分支解析 inner worktreeId 后委托 remote_client。
pub(crate) async fn get_claude_session_preview_for_state(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    session_id: &str,
) -> Result<SessionPreview, AppError> {
    let project = get_project(state, project_id).await?;
    if project.kind == "remote" {
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            remote_inner_worktree_id(&context.device_id, worktree_id.map(str::to_string))?;
        ensure_remote_event_bridge_for_context(state, &context);
        let req = RemoteClaudeSessionReq {
            project_id: context.inner_project_id.clone(),
            worktree_id: inner_worktree_id,
            session_id: session_id.to_string(),
        };
        return RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .get_claude_session_preview(&context.base_url, req)
            .await;
    }
    // local 分支
    let worktree = resolve_worktree(state, &project, worktree_id).await?;
    let shared =
        ensure_worktree_session_index_scanned(state, std::path::Path::new(&worktree.path)).await;
    let index = shared.read().expect("session index 读锁中毒");
    let claude_index: &ClaudeSessionIndex = index
        .sessions
        .get(session_id)
        .ok_or_else(|| AppError::not_found("Claude session 不存在"))?;
    Ok(to_session_preview(claude_index))
}

/// 读取单个 Claude session 的 preview 详情。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端 preview 面板需要按 sessionId 展示本机或远端会话详情。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 State 后委托 for_state helper。
#[tauri::command]
pub async fn get_claude_session_preview(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    session_id: String,
) -> Result<SessionPreview, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "claude.preview", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "sessionId": session_id.clone() })).await? {
        return Ok(v);
    }
    get_claude_session_preview_for_state(
        state.inner(),
        &project_id,
        worktree_id.as_deref(),
        &session_id,
    )
    .await
}

/// resume 一个历史 Claude Code 会话。
///
/// Business Logic（为什么需要这个函数）:
///     用户选中某条历史 session 后，应在当前 worktree 新建一个 workbench terminal 并注入
///     `claude --dangerously-skip-permissions --resume <sessionId>` 命令，让对话继续。
///     本机在本机完成 CLI 检测+建会话+写命令；remote shortcut 把 resume 请求转发到项目所在设备，
///     并把远端新建的 inner sessionId 包装成本机统一 remote sessionId。
///
/// Code Logic（这个函数做什么）:
///     local 分支：读 config.github_trending.claude_cli_path → check_claude_cli_available（失败转中文业务错误）
///     → resolve_worktree → local_create_workbench_session（默认 120x32）→ local_write_workbench_session_input 注入 resume 命令
///     → 返回 ResumeClaudeSessionResult（sessionId 为本机新建 terminal id）。
///     remote 分支（R42 H3）：先 project lease → re-get project → resume RPC → revalidate；
///     失败补偿 close 远端 session；成功 ensure_session_watch_for_project 并返回 wrapped id。
pub(crate) async fn resume_claude_session_for_state(
    state: &AppState,
    project_id: &str,
    worktree_id: Option<&str>,
    session_id: &str,
) -> Result<ResumeClaudeSessionResult, AppError> {
    // R42 H3：remote resume 实际会创建 terminal；与 create 一样先 lease 再读项目。
    let project_lease = state
        .workbench_sessions
        .try_acquire_project_op_lease(project_id)?;
    let project = get_project(state, project_id).await?;
    if project.kind == "remote" {
        let _project_lease = project_lease;
        let context = ensure_remote_project_context(state, &project).await?;
        let inner_worktree_id =
            remote_inner_worktree_id(&context.device_id, worktree_id.map(str::to_string))?;
        ensure_remote_event_bridge_for_context(state, &context);
        let req = RemoteClaudeSessionReq {
            project_id: context.inner_project_id.clone(),
            worktree_id: inner_worktree_id,
            session_id: session_id.to_string(),
        };
        let remote_result = RemoteWorkbenchClient::new()
            .with_expected_device_id(&context.device_id)
            .resume_claude_session(&context.base_url, req)
            .await?;
        // R42 H3：远端已创建 terminal 后若 project 进入 closing，补偿 close 并 fail-closed。
        if state
            .workbench_sessions
            .require_project_not_closing(project_id)
            .is_err()
        {
            let inner_session_id = remote_result.session_id.clone();
            if let Err(close_error) = RemoteWorkbenchClient::new()
                .with_expected_device_id(&context.device_id)
                .close_session(&context.base_url, &inner_session_id)
                .await
            {
                tracing::debug!(
                    error = %close_error,
                    session_id = %inner_session_id,
                    "remote resume compensation close failed after project closing"
                );
            }
            return Err(AppError::unavailable(
                "project_closing_barrier_active".to_string(),
            ));
        }
        // 把远端新建的 inner terminal sessionId 包装成本机统一 remote sessionId
        let wrapped = remote_entity_id(&context.device_id, &remote_result.session_id);
        // R42 H3：project-scoped watch，避免依赖前端 loadSessions 补建。
        let _ = state
            .workbench_remote_event_bridges
            .ensure_session_watch_for_project(
                &context.device_id,
                &context.local_project_id,
                &wrapped,
            );
        return Ok(ResumeClaudeSessionResult {
            ok: remote_result.ok,
            session_id: wrapped,
        });
    }
    drop(project_lease);
    // local 分支：先检测 Claude CLI 可用，失败给清晰中文错误
    let cli_path = state
        .config
        .read()
        .expect("config 读锁中毒")
        .github_trending
        .claude_cli_path
        .clone();
    claude_cli::check_claude_cli_available(&cli_path)
        .await
        .map_err(|err| AppError::generic(format!("Claude CLI 不可用：{err}")))?;
    // 解析 worktree（同时校验项目存在性），仅用于确保 worktree_path 有效
    let _worktree = resolve_worktree(state, &project, worktree_id).await?;
    // 在该 worktree 新建一个 workbench terminal
    let new_session = local_create_workbench_session(
        state,
        project_id.to_string(),
        worktree_id.map(str::to_string),
        Some(120),
        Some(32),
    )
    .await?;
    // 注入 resume 命令并回车
    let command = format!(
        "{} --dangerously-skip-permissions --resume {}\n",
        claude_cli::normalize_cli_path(&cli_path),
        session_id
    );
    local_write_workbench_session_input(state, new_session.id.clone(), command).await?;
    Ok(ResumeClaudeSessionResult {
        ok: true,
        session_id: new_session.id,
    })
}

/// resume 一个历史 Claude Code 会话。
///
/// Business Logic（为什么需要这个命令）:
///     桌面端选中搜索结果后需要一键 resume 本机或远端历史会话。
///
/// Code Logic（这个命令做什么）:
///     Tauri command 解包 State 后委托 for_state helper。
#[tauri::command]
pub async fn resume_claude_session(
    state: State<'_, AppState>,
    project_id: String,
    worktree_id: Option<String>,
    session_id: String,
) -> Result<ResumeClaudeSessionResult, AppError> {
    if let Some(v) = proxy_workbench_if_gui(state.inner(), "claude.resume", serde_json::json!({ "projectId": project_id.clone(), "worktreeId": worktree_id.clone(), "sessionId": session_id.clone() })).await? {
        return Ok(v);
    }
    resume_claude_session_for_state(
        state.inner(),
        &project_id,
        worktree_id.as_deref(),
        &session_id,
    )
    .await
}
