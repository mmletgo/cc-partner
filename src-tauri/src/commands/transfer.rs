//! commands/transfer.rs — 文件传输命令（本地前端 invoke）
//!
//! Business Logic（为什么需要这个模块）:
//!     前端传输面板通过 invoke 调用：列出传输任务（活跃+历史）、发起发送、取消任务、
//!     幂等 retry/resume（clientOperationId）、uncertain operation 对账查询、
//!     same-device Open/Reveal 准备（prepare_transfer_open）。
//!     对照 Python `/api/transfer/tasks`、`/api/transfer/send`、`DELETE /api/transfer/tasks/{id}`。
//!
//! Code Logic（这个模块做什么）:
//!     - `list_transfers`：合并 registry 活跃任务 + transfer_history 历史，按 created_at 倒序，
//!       转为 TransferTaskDto（camelCase）返回。
//!     - `send_transfer`：调 `transfer::sender::start_sending`（clientOperationId claim 后 spawn），
//!       立即返回 `{accepted, deviceId, filePath, id}`。
//!     - `cancel_transfer`：触发 CancellationToken，返回 `{ok, id}`。
//!     - `retry_transfer` / `resume_transfer` / `send_transfer` / `cancel_transfer` /
//!       `get_transfer_operation`：owner 本地执行；GuiClient 经 loopback control 代理（与 N1 sidecar sole owner 一致）。
//!     - `prepare_transfer_open`：owner 校验 Receive+completed+path；GuiClient 经 control 代理。

use crate::backend::authority::RuntimeRole;
use crate::backend::control_client::BackendControlClient;
use crate::error::AppError;
use crate::models::transfer::{
    LocalTransferOpenTarget, TransferDirection, TransferOpenAction, TransferOperationStatus,
    TransferStatus, TransferTaskDto,
};
use crate::state::AppState;
use crate::transfer::sender;
use std::path::{Component, Path};
use tauri::State;

/// 列出全部传输任务（活跃 + 历史），按创建时间倒序。
///
/// Business Logic: 前端传输面板展示进行中任务与已结束历史。对照 Python `/api/transfer/tasks`。
/// Code Logic: 委托 `list_transfers_for_state`（Tauri 与 mobile HTTP 共用）。
#[tauri::command]
pub async fn list_transfers(state: State<'_, AppState>) -> Result<Vec<TransferTaskDto>, AppError> {
    list_transfers_for_state(state.inner()).await
}

/// owner/本地：合并 registry 活跃任务与 transfer_history。
///
/// Business Logic（为什么需要这个函数）:
///     桌面 Tauri 与 `/api/mobile/transfer/tasks` 必须看到同一份主机任务列表；
///     抽成 helper 避免 HTTP 再抄一份合并/去重逻辑。
///
/// Code Logic（这个函数做什么）:
///     合并 `registry.list()` 与 `transfer_repo.list()`（历史去重活跃 id），
///     按 `created_at` 倒序，转为 `TransferTaskDto`（桌面 DTO 仍含 path；mobile 路由再剥离）。
pub async fn list_transfers_for_state(state: &AppState) -> Result<Vec<TransferTaskDto>, AppError> {
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

/// 发起文件发送：clientOperationId claim 后 spawn，立即返回 transfer_id。
///
/// Business Logic: 前端选择文件与目标设备后调用；稳定 clientOperationId 保证 lost ACK
///     不重复发送。后端 claim 后 spawn 异步任务并立即返回，前端通过
///     listen('transfer:progress') 等事件追踪进度。对照 Python `/api/transfer/send`。
///
/// Code Logic: GuiClient → control `transfer/send`；owner → `sender::start_sending`。
#[tauri::command]
pub async fn send_transfer(
    state: State<'_, AppState>,
    device_id: String,
    file_path: String,
    client_operation_id: String,
) -> Result<serde_json::Value, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client
            .send_transfer(&device_id, &file_path, &client_operation_id)
            .await;
    }
    let transfer_id = sender::start_sending(
        state.inner().clone(),
        device_id.clone(),
        file_path.clone(),
        client_operation_id,
    )
    .await?;
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
/// Code Logic: GuiClient → control `transfer/cancel`；owner → registry.cancel。
#[tauri::command]
pub async fn cancel_transfer(
    state: State<'_, AppState>,
    task_id: String,
) -> Result<serde_json::Value, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.cancel_transfer(&task_id).await;
    }
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
///     GuiClient → control `transfer/retry`；owner → `sender::retry_transfer`。
#[tauri::command]
pub async fn retry_transfer(
    state: State<'_, AppState>,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTaskDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.retry_transfer(&task_id, &client_operation_id).await;
    }
    let task = sender::retry_transfer(state.inner().clone(), task_id, client_operation_id).await?;
    Ok(task.to_dto(None))
}

/// 幂等断点续传（复用稳定 protocol transfer id）。
///
/// Business Logic（为什么需要这个命令）:
///     有 resume metadata 且对端支持时从 checkpoint 继续；旧 peer 返回 unsupported。
///
/// Code Logic（这个函数做什么）:
///     GuiClient → control `transfer/resume`；owner → `sender::resume_transfer`。
#[tauri::command]
pub async fn resume_transfer(
    state: State<'_, AppState>,
    task_id: String,
    client_operation_id: String,
) -> Result<TransferTaskDto, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.resume_transfer(&task_id, &client_operation_id).await;
    }
    let task = sender::resume_transfer(state.inner().clone(), task_id, client_operation_id).await?;
    Ok(task.to_dto(None))
}

/// 查询发送端 clientOperationId 的 operation 真值。
///
/// Business Logic（为什么需要这个命令）:
///     transport timeout / lost final ACK 后 UI 必须先对账，禁止盲重试。
///
/// Code Logic（这个命令做什么）:
///     GuiClient → control `transfer/get-operation`；owner → `sender::get_transfer_operation`。
#[tauri::command]
pub async fn get_transfer_operation(
    state: State<'_, AppState>,
    client_operation_id: String,
) -> Result<TransferOperationStatus, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.get_transfer_operation(&client_operation_id).await;
    }
    sender::get_transfer_operation(state.inner(), &client_operation_id).await
}

/// 为 same-device GUI Open/Reveal 准备 local target。
///
/// Business Logic（为什么需要这个命令）:
///     用户只能在本机桌面打开/显示「本机收到」的 completed 文件；sidecar 校验后返回路径，
///     GUI 再调 Tauri opener。P2P/mobile 无此面，不得暴露路径。
///
/// Code Logic（这个命令做什么）:
///     GuiClient → control `transfer/prepare-open`；否则本机 owner 路径
///     `prepare_transfer_open_for_state`。
#[tauri::command]
pub async fn prepare_transfer_open(
    state: State<'_, AppState>,
    task_id: String,
    action: TransferOpenAction,
) -> Result<LocalTransferOpenTarget, AppError> {
    if state.runtime_role == RuntimeRole::GuiClient {
        let client = BackendControlClient::from_control_file()?;
        return client.prepare_transfer_open(&task_id, action).await;
    }
    prepare_transfer_open_for_state(state.inner(), &task_id, action).await
}

/// owner/本地：解析任务并校验 Receive + completed + path exists。
///
/// Business Logic（为什么需要这个函数）:
///     control API 与 HeadlessOwner Tauri 命令共享同一校验；只映射 repository 状态、
///     路径缺失与路径校验错误；不执行 opener。
///
/// Code Logic（这个函数做什么）:
///     registry 优先 → history；direction≠Receive → unsupported；status≠Completed →
///     transfer_not_completed；路径空/非法组件 → transfer_path_invalid；不存在 →
///     transfer_path_missing；成功返回 LocalTransferOpenTarget。
pub async fn prepare_transfer_open_for_state(
    state: &AppState,
    task_id: &str,
    action: TransferOpenAction,
) -> Result<LocalTransferOpenTarget, AppError> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return Err(AppError::validation(
            "transfer_path_invalid: task_id 不能为空".to_string(),
        ));
    }

    let task = if let Some(active) = state.transfers.get(task_id) {
        active
    } else {
        state
            .transfer_repo
            .get_by_id(task_id)
            .await?
            .ok_or_else(|| AppError::not_found(format!("not_found: 传输任务不存在: {task_id}")))?
    };

    if task.direction != TransferDirection::Receive {
        return Err(AppError::validation(
            "unsupported: Open/Reveal 仅支持本机接收完成的文件".to_string(),
        ));
    }

    if task.status != TransferStatus::Completed {
        return Err(AppError::conflict(format!(
            "transfer_not_completed: 任务尚未完成（status={}），无法 {}",
            task.status.as_str(),
            action.as_str()
        )));
    }

    let path_raw = task.file_path.trim();
    if path_raw.is_empty() {
        return Err(AppError::validation(
            "transfer_path_invalid: 任务未记录目标路径".to_string(),
        ));
    }

    let path = Path::new(path_raw);
    validate_open_path(path)?;

    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::not_found(format!("transfer_path_missing: 目标文件不存在: {path_raw}"))
        } else {
            AppError::validation(format!("transfer_path_invalid: 无法读取目标路径: {e}"))
        }
    })?;

    if meta.file_type().is_symlink() {
        return Err(AppError::validation(
            "transfer_path_invalid: 拒绝跟随符号链接目标".to_string(),
        ));
    }
    if !meta.is_file() {
        return Err(AppError::validation(
            "transfer_path_invalid: 目标不是普通文件".to_string(),
        ));
    }

    Ok(LocalTransferOpenTarget {
        task_id: task.id,
        action,
        path: path_raw.to_string(),
    })
}

/// 校验 open/reveal 路径组件（拒绝 `..` 与空组件；允许绝对路径）。
///
/// Business Logic（为什么需要这个函数）:
///     repository 路径理论上由本机 finalize 写入，仍需拒绝畸形/穿越组件，防止误开错误位置。
///
/// Code Logic（这个函数做什么）:
///     遍历 Path components；ParentDir / 空 Normal → transfer_path_invalid。
fn validate_open_path(path: &Path) -> Result<(), AppError> {
    if !path.is_absolute() {
        return Err(AppError::validation(
            "transfer_path_invalid: 目标路径必须是绝对路径".to_string(),
        ));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(AppError::validation(
                    "transfer_path_invalid: 路径包含上级目录组件".to_string(),
                ));
            }
            Component::Normal(name) if name.is_empty() => {
                return Err(AppError::validation(
                    "transfer_path_invalid: 路径含空组件".to_string(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// 从 AppError 文案提取稳定业务码（`code: message` 前缀）。
///
/// Business Logic（为什么需要这个函数）:
///     Open/Reveal 失败需要稳定 code 供测试与前端分支；IPC 仍只序列化 message。
///
/// Code Logic（这个函数做什么）:
///     取 Display 首段 `:` 前 token；无则 `internal`。
#[cfg_attr(not(test), allow(dead_code))]
pub fn prepare_error_code(err: &AppError) -> &str {
    // 注意：不能返回指向临时 String 的引用；从静态表匹配。
    let msg = err.to_string();
    extract_stable_code(&msg)
}

/// 匹配已知稳定码；未知前缀回落 internal。
///
/// Business Logic（为什么需要这个函数）:
///     测试与日志只关心固定 token 集合。
///
/// Code Logic（这个函数做什么）:
///     若 message 以已知 code + `:` 开头则返回该 code，否则 internal。
#[cfg_attr(not(test), allow(dead_code))]
fn extract_stable_code(message: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "transfer_not_completed",
        "transfer_path_missing",
        "transfer_path_invalid",
        "unsupported",
        "not_found",
    ];
    for code in KNOWN {
        let prefix = format!("{code}:");
        if message.starts_with(&prefix) || message.starts_with(code) {
            return code;
        }
    }
    "internal"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ui::HeadlessBackendUi;
    use crate::config::{
        AppConfig, BatteryConfig, GithubTrendingConfig, HealthConfig, OrchestratorAutomationConfig,
    };
    use crate::models::transfer::{TransferDirection, TransferPhase, TransferStatus, TransferTask};
    use crate::net::peer_client::PeerClient;
    use crate::orchestrator::repo::OrchestratorRepo;
    use crate::orchestrator::scheduler::OrchestratorSchedulerTelemetry;
    use crate::storage::{
        ClaudeHistoryRepo, ClaudeMdRepo, PromptRepo, ScratchpadRepo, SshTargetRepo, TransferRepo,
        WorkbenchAgentSessionRepo, WorkbenchBrowserRepo, WorkbenchProjectRepo,
        WorkbenchSessionRepo, WorkbenchWorktreeRepo,
    };
    use crate::transfer::registry::TransferRegistry;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::atomic::AtomicU16;
    use std::sync::{Arc, Mutex, RwLock};

    /// 构造带 transfer_history 的最小 owner AppState。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Open/Reveal 校验只依赖 registry/repo/config，无需完整 HTTP。
    ///
    /// Code Logic（这个函数做什么）:
    ///     内存 SQLite + TransferRepo::ensure_schema + HeadlessOwner。
    async fn build_open_test_state(receive_dir: &std::path::Path) -> AppState {
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
        TransferRepo::ensure_schema(&pool).await.unwrap();

        let config = AppConfig {
            device_id: "device-test".to_string(),
            device_name: "test-device".to_string(),
            http_port: 0,
            receive_dir: receive_dir.to_string_lossy().to_string(),
            game_plugin_dir: "/tmp/plugins".into(),
            db_path: receive_dir.join("data.db").to_string_lossy().to_string(),
            screenshot_hotkey: "<cmd>+s".to_string(),
            prompt_optimizer_hotkey: "<ctrl>".to_string(),
            prompt_optimizer_fill_language: "zh".to_string(),
            prompt_optimizer_provider: "claude".into(),
            prompt_quick_input_hotkey: "<ctrl>+/".to_string(),
            cloud_sync_repo_url: None,
            cloud_sync_enabled: false,
            cloud_sync_auto: false,
            cloud_sync_interval_secs: 600,
            cloud_sync_branch: None,
            health: HealthConfig::default(),
            battery: BatteryConfig::default(),
            orchestrator: OrchestratorAutomationConfig::default(),
            github_trending: GithubTrendingConfig::default(),
            internal_claude: crate::config::InternalClaudeConfig::default(),
            agent_hub: crate::config::AgentHubConfig::default(),
            manual_peers: Vec::new(),
            experimental_features: crate::config::ExperimentalFeaturesConfig::default(),
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
            attention_read_repo: Arc::new(crate::storage::AttentionReadRepo::new(pool.clone())),
            transfer_repo: Arc::new(TransferRepo::new(pool.clone())),
            claude_md_repo: Arc::new(ClaudeMdRepo::new(pool.clone())),
            scratchpad_repo: Arc::new(ScratchpadRepo::new(pool.clone())),
            ssh_target_repo: Arc::new(SshTargetRepo::new(pool.clone())),
            device_id: Arc::new("device-test".to_string()),
            devices: Arc::new(RwLock::new(std::collections::HashMap::new())),
            actual_http_port: Arc::new(AtomicU16::new(0)),
            discovery: Arc::new(Mutex::new(None)),
            overlay_trusted_ips: Arc::new(RwLock::new(std::collections::HashSet::new())),
            manual_peer_cancel: Arc::new(Mutex::new(None)),
            peer_client: Arc::new(PeerClient::new()),
            transfers: Arc::new(TransferRegistry::new()),
            ui: Arc::new(HeadlessBackendUi::new(receive_dir.join("dist"))),
            update_runtime: Arc::new(crate::updater::UpdateRuntime::new()),
            cc_history_repo: Arc::new(ClaudeHistoryRepo::new(pool.clone())),
            workbench_project_repo: Arc::new(WorkbenchProjectRepo::new(pool.clone())),
            workbench_session_repo: Arc::new(WorkbenchSessionRepo::new(pool.clone())),
            workbench_agent_session_repo: Arc::new(WorkbenchAgentSessionRepo::new(pool.clone())),
            agent_ledger_repo: Arc::new(crate::storage::AgentLedgerRepo::new(pool.clone())),
            agent_ledger_service: Arc::new(
                crate::workbench::agent_ledger::AgentLedgerService::new(
                    crate::storage::AgentLedgerRepo::new(pool.clone()),
                ),
            ),
            agent_hub_repo: Arc::new(crate::storage::AgentHubRepo::new(pool.clone())),
            workbench_worktree_repo: Arc::new(WorkbenchWorktreeRepo::new(pool.clone())),
            workbench_browser_repo: Arc::new(WorkbenchBrowserRepo::new(pool.clone())),
            workbench_workspace_layout_repo: Arc::new(
                crate::storage::WorkbenchWorkspaceLayoutRepo::new(pool.clone()),
            ),
            workbench_project_note_repo: Arc::new(crate::storage::WorkbenchProjectNoteRepo::new(
                pool.clone(),
            )),
            workbench_browser_previews: Arc::new(
                crate::workbench::browser_proxy::WorkbenchBrowserPreviewRegistry::new(),
            ),
            browser_verification: Arc::new(
                crate::workbench::browser_verification::BrowserVerificationService::new(
                    Arc::new(crate::workbench::browser_verification::FakeEngine::succeeds()),
                    std::env::temp_dir().join("cc-partner-bv-test"),
                    "test-owner".into(),
                )
                .expect("browser verification test service"),
            ),
            workbench_sessions: Arc::new(
                crate::workbench::sessions::WorkbenchSessionRegistry::new(),
            ),
            workbench_remote_events: std::sync::Arc::new(
                crate::workbench::remote_events::WorkbenchRemoteEventBus::new("test-owner"),
            ),
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
            agent_ledger_cancel: Arc::new(Mutex::new(None)),
            agent_hub_cancel: Arc::new(Mutex::new(None)),
            agent_hub_git_runtime: Arc::new(crate::agent_hub::git::AgentHubGitRuntime::new()),
            agent_hub_git_cancel: Arc::new(Mutex::new(None)),
            workbench_claude_session_indexes: Arc::new(RwLock::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_watchers: Arc::new(Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_inflight: Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            workbench_claude_session_index_dispose_epochs: Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            runtime_metrics: Arc::new(crate::backend::runtime_metrics::RuntimeMetrics::new()),
            runtime_role: RuntimeRole::HeadlessOwner,
            event_bus: Arc::new(crate::backend::event_bus::RuntimeEventBus::new(
                "transfer-open-test-owner",
            )),
            backend_control_client_runtime: Arc::new(
                crate::backend::control_client::BackendControlClientRuntime::new(),
            ),
            gui_event_relay_cancel: Arc::new(Mutex::new(None)),
        }
    }

    /// 构造一条历史任务并落库。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     各用例需要不同 direction/status/path 组合。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 recovery_defaults + 字段覆盖后 record。
    async fn record_task(
        state: &AppState,
        id: &str,
        direction: TransferDirection,
        status: TransferStatus,
        file_path: &str,
    ) {
        let phase = TransferPhase::from_status(status);
        let task = TransferTask {
            filename: "payload.bin".into(),
            file_path: file_path.into(),
            size: 4,
            sha256: "abcd".into(),
            direction,
            peer_device_id: "peer".into(),
            status,
            transferred_bytes: 4,
            created_at: "2026-07-14T10:00:00Z".into(),
            completed_at: if status == TransferStatus::Completed {
                Some("2026-07-14T10:01:00Z".into())
            } else {
                None
            },
            phase: Some(phase),
            ..TransferTask::recovery_defaults(id)
        };
        state.transfer_repo.record(&task).await.unwrap();
    }

    fn unique_temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cc-partner-transfer-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 非 completed 任务拒绝 reveal。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Open/Reveal 只能针对 completed；failed 不得打开。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写入 failed Receive 任务 → prepare Reveal → transfer_not_completed。
    #[tokio::test]
    async fn reveal_rejects_non_completed_task() {
        let dir = unique_temp_dir();
        let state = build_open_test_state(&dir).await;
        let file = dir.join("missing-or-failed.bin");
        record_task(
            &state,
            "failed-task",
            TransferDirection::Receive,
            TransferStatus::Failed,
            &file.to_string_lossy(),
        )
        .await;

        let err =
            prepare_transfer_open_for_state(&state, "failed-task", TransferOpenAction::Reveal)
                .await
                .expect_err("non-completed must fail");
        assert_eq!(prepare_error_code(&err), "transfer_not_completed");
    }

    /// completed 但路径缺失。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     文件被用户删除后，Open/Reveal 必须稳定报 path missing。
    ///
    /// Code Logic（这个测试做什么）:
    ///     completed Receive + 不存在路径 → transfer_path_missing。
    #[tokio::test]
    async fn open_rejects_missing_path() {
        let dir = unique_temp_dir();
        let state = build_open_test_state(&dir).await;
        let missing = dir.join("gone.bin");
        record_task(
            &state,
            "completed-missing",
            TransferDirection::Receive,
            TransferStatus::Completed,
            &missing.to_string_lossy(),
        )
        .await;

        let err =
            prepare_transfer_open_for_state(&state, "completed-missing", TransferOpenAction::Open)
                .await
                .expect_err("missing path must fail");
        assert_eq!(prepare_error_code(&err), "transfer_path_missing");
    }

    /// 仅 Receive 方向允许 open/reveal。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     发送端 completed 不得在本机伪装 open 对端路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     completed Send → unsupported。
    #[tokio::test]
    async fn open_rejects_send_direction() {
        let dir = unique_temp_dir();
        let state = build_open_test_state(&dir).await;
        let file = dir.join("local-source.bin");
        std::fs::write(&file, b"data").unwrap();
        record_task(
            &state,
            "send-completed",
            TransferDirection::Send,
            TransferStatus::Completed,
            &file.to_string_lossy(),
        )
        .await;

        let err =
            prepare_transfer_open_for_state(&state, "send-completed", TransferOpenAction::Open)
                .await
                .expect_err("send direction unsupported");
        assert_eq!(prepare_error_code(&err), "unsupported");
    }

    /// 路径含 `..` 被拒绝。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     畸形 path 不得用于 opener。
    ///
    /// Code Logic（这个测试做什么）:
    ///     completed Receive + 含 ParentDir → transfer_path_invalid。
    #[tokio::test]
    async fn open_rejects_path_with_parent_component() {
        let dir = unique_temp_dir();
        let state = build_open_test_state(&dir).await;
        // 构造绝对路径但含 `..` 组件（不依赖真实存在）
        let bad = dir.join("sub").join("..").join("x.bin");
        record_task(
            &state,
            "bad-path",
            TransferDirection::Receive,
            TransferStatus::Completed,
            &bad.to_string_lossy(),
        )
        .await;

        let err = prepare_transfer_open_for_state(&state, "bad-path", TransferOpenAction::Reveal)
            .await
            .expect_err("parent component must fail");
        assert_eq!(prepare_error_code(&err), "transfer_path_invalid");
    }

    /// completed Receive + 真实文件成功返回 target。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     happy path 必须返回 taskId/action/path 供 GUI opener。
    ///
    /// Code Logic（这个测试做什么）:
    ///     写文件 + completed Receive → LocalTransferOpenTarget。
    #[tokio::test]
    async fn prepare_open_returns_local_target_for_completed_receive() {
        let dir = unique_temp_dir();
        let state = build_open_test_state(&dir).await;
        let file = dir.join("ok.bin");
        std::fs::write(&file, b"ok").unwrap();
        record_task(
            &state,
            "recv-ok",
            TransferDirection::Receive,
            TransferStatus::Completed,
            &file.to_string_lossy(),
        )
        .await;

        let target = prepare_transfer_open_for_state(&state, "recv-ok", TransferOpenAction::Open)
            .await
            .expect("completed receive must succeed");
        assert_eq!(target.task_id, "recv-ok");
        assert_eq!(target.action, TransferOpenAction::Open);
        assert_eq!(Path::new(&target.path), file.as_path());
    }
}
