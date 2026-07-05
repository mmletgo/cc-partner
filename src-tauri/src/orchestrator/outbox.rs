//! Orchestrator remote outbox and mirror models.
//!
//! Business Logic（为什么需要这个模块）:
//!     本机打开远端 Workbench 项目时，任务的权威执行权属于远端设备；远端离线时本机需要暂存创建请求，
//!     并在设备恢复在线后自动投递，同时缓存远端任务快照供界面离线展示。
//!
//! Code Logic（这个模块做什么）:
//!     定义 remote outbox/mirror 的状态、数据库 Row 和前端 DTO；服务 helper 在实现阶段补齐，
//!     仓储读写方法由 `OrchestratorRepo` 持有。

use crate::error::AppError;
use crate::orchestrator::models::OrchestratorTaskDto;
use crate::orchestrator::remote_client::RemoteOrchestratorClient;
use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
use crate::state::AppState;
use crate::workbench::models::WorkbenchProjectRow;
use crate::workbench::remote_client::RemoteWorkbenchClient;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

const REMOTE_OUTBOX_DISPATCH_INTERVAL_SECS: u64 = 10;
const REMOTE_OUTBOX_DISPATCH_BATCH_SIZE: i64 = 20;

/// 远端任务投递 outbox 状态。
///
/// Business Logic（为什么需要这个枚举）:
///     离线创建的远端任务需要清楚区分等待发送、发送中、已镜像和不可重试失败，供后台 dispatcher 与 UI 判断。
///
/// Code Logic（这个枚举做什么）:
///     提供 SQLite 小写存储值与 Rust enum 的互转，未知值视为数据损坏并返回业务错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteOutboxStatus {
    Pending,
    Sending,
    Mirrored,
    Failed,
}

impl RemoteOutboxStatus {
    /// Business Logic（为什么需要这个函数）:
    ///     SQLite outbox 表保存稳定小写状态，便于人工排查 pending/sending 队列。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将 enum 映射为数据库字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Mirrored => "mirrored",
            Self::Failed => "failed",
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     仓储读取 outbox 行时需要恢复强类型状态，避免命令层处理裸字符串。
    ///
    /// Code Logic（这个函数做什么）:
    ///     解析数据库状态字符串；未知状态返回 AppError。
    pub fn from_str(value: &str) -> Result<Self, AppError> {
        match value {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "mirrored" => Ok(Self::Mirrored),
            "failed" => Ok(Self::Failed),
            other => Err(AppError::generic(format!(
                "未知 Orchestrator 远端 outbox 状态: {other}"
            ))),
        }
    }
}

/// 远端任务投递 outbox 数据库行。
///
/// Business Logic（为什么需要这个结构体）:
///     本机需要持久化远端离线任务创建请求，保证应用重启后仍能继续投递。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_remote_outbox` 表一一对应；request_json 保存远端 create 请求原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestratorRemoteOutboxRow {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub remote_project_path: String,
    pub remote_project_id: Option<String>,
    pub request_json: String,
    pub status: RemoteOutboxStatus,
    pub remote_task_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sent_at: Option<String>,
}

/// 远端任务投递 outbox 前端 DTO。
///
/// Business Logic（为什么需要这个结构体）:
///     Phase 6 UI 需要展示 pending remote task，并提示目标设备、路径和失败原因。
///
/// Code Logic（这个结构体做什么）:
///     以 camelCase 序列化 outbox row，status 保持强类型 enum 输出。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorRemoteOutboxDto {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub remote_project_path: String,
    pub remote_project_id: Option<String>,
    pub request_json: String,
    pub status: RemoteOutboxStatus,
    pub remote_task_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub sent_at: Option<String>,
}

/// 远端任务镜像数据库行。
///
/// Business Logic（为什么需要这个结构体）:
///     本机只缓存远端任务展示快照，不能把镜像当作本机任务执行。
///
/// Code Logic（这个结构体做什么）:
///     字段与 `orchestrator_remote_task_mirrors` 表一一对应；payload_json 保存远端任务 DTO 原文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMirrorTask {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub remote_project_id: String,
    pub remote_project_path: String,
    pub remote_task_id: String,
    pub payload_json: String,
    pub last_synced_at: String,
}

impl OrchestratorRemoteOutboxRow {
    /// Business Logic（为什么需要这个函数）:
    ///     命令层返回 pending remote task 时不能暴露内部 Row 类型，需要统一 DTO 投影。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆 Row 字段并转换为 camelCase DTO。
    pub fn to_dto(&self) -> OrchestratorRemoteOutboxDto {
        OrchestratorRemoteOutboxDto {
            id: self.id.clone(),
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            remote_project_path: self.remote_project_path.clone(),
            remote_project_id: self.remote_project_id.clone(),
            request_json: self.request_json.clone(),
            status: self.status,
            remote_task_id: self.remote_task_id.clone(),
            last_error: self.last_error.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            sent_at: self.sent_at.clone(),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     mirror 缓存要保存完整远端任务 DTO，便于离线状态下直接还原展示数据。
///
/// Code Logic（这个函数做什么）:
///     使用 serde_json 把 OrchestratorTaskDto 序列化为 payload_json。
pub fn mirror_payload_from_task(task: &OrchestratorTaskDto) -> Result<String, AppError> {
    Ok(serde_json::to_string(task)?)
}

/// 远端投递错误类别。
///
/// Business Logic（为什么需要这个枚举）:
///     dispatcher 需要把可重试的网络错误和不可重试的协议/校验错误分开，决定 outbox 继续 pending 还是 failed。
///
/// Code Logic（这个枚举做什么）:
///     Network 保存可重试错误文案，Protocol 保存不可重试错误文案；仅在本模块内部使用。
#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteOutboxDispatchError {
    Network(String),
    Protocol(String),
}

/// Business Logic（为什么需要这个函数）:
///     远端项目的本机 shortcut 必须携带设备 ID、设备名和真实远端路径，否则无法投递任务。
///
/// Code Logic（这个函数做什么）:
///     校验 WorkbenchProjectRow.kind/device/path，非 remote 或字段缺失时返回业务错误。
fn ensure_remote_shortcut(project: &WorkbenchProjectRow) -> Result<(), AppError> {
    if project.kind != "remote" {
        return Err(AppError::generic("当前项目不是远端项目"));
    }
    if project.device_id.trim().is_empty() {
        return Err(AppError::generic("远端项目缺少设备 ID"));
    }
    if project.device_name.trim().is_empty() {
        return Err(AppError::generic("远端项目缺少设备名称"));
    }
    if project.path.trim().is_empty() {
        return Err(AppError::generic("远端项目缺少路径"));
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     outbox dispatcher 和 remote-aware 命令需要判断设备是否在线，离线时不能把请求视为协议失败。
///
/// Code Logic（这个函数做什么）:
///     从 AppState.devices 快照中查找 device_id 并返回 base_url；持锁期间不 await。
pub fn remote_device_base_url(state: &AppState, device_id: &str) -> Result<String, AppError> {
    let devices = state.devices.read().expect("devices 读锁中毒");
    let device = devices
        .get(device_id)
        .ok_or_else(|| AppError::generic("远端设备不在线"))?;
    Ok(device.base_url())
}

/// Business Logic（为什么需要这个函数）:
///     网络离线类错误应保持 pending，协议/校验类错误才应标 failed。
///
/// Code Logic（这个函数做什么）:
///     基于现有远端客户端错误前缀和设备离线文案做分类；非网络错误默认按协议/校验失败处理。
pub fn is_remote_network_error(error: &AppError) -> bool {
    let message = error.to_string();
    if message.contains("远端设备不在线") {
        return true;
    }
    let request_failed = message.starts_with("远端 Orchestrator 请求失败:")
        || message.starts_with("远端 Workbench 请求失败:");
    request_failed && !message.contains("HTTP ")
}

/// Business Logic（为什么需要这个函数）:
///     用户在远端项目离线时创建任务，需要先写入本机 pending outbox，等待设备恢复在线后自动投递。
///
/// Code Logic（这个函数做什么）:
///     校验 remote shortcut，序列化远端 create 请求，并调用 repo 插入 status=pending 行。
pub async fn create_pending_remote_task(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
    create_req: RemoteCreateOrchestratorTaskReq,
) -> Result<OrchestratorRemoteOutboxRow, AppError> {
    ensure_remote_shortcut(remote_shortcut)?;
    let request_json = serde_json::to_string(&create_req)?;
    state
        .orchestrator_repo
        .insert_remote_outbox_pending(
            &remote_shortcut.device_id,
            &remote_shortcut.device_name,
            &remote_shortcut.path,
            None,
            &request_json,
        )
        .await
}

/// Business Logic（为什么需要这个函数）:
///     后台 dispatcher 每次 tick 需要尝试投递一批 pending 远端任务，不依赖前端页面是否打开。
///
/// Code Logic（这个函数做什么）:
///     查询 pending items，逐条用条件 claim 变为 sending，成功投递后标 mirrored 并 upsert mirror；
///     网络错误写 last_error 并回 pending，协议/校验错误标 failed，返回成功投递数量。
pub async fn dispatch_remote_outbox_once(state: &AppState) -> Result<usize, AppError> {
    let pending = state
        .orchestrator_repo
        .list_pending_remote_outbox_items(REMOTE_OUTBOX_DISPATCH_BATCH_SIZE)
        .await?;
    let mut dispatched = 0usize;

    for item in pending {
        let Some(claimed) = state
            .orchestrator_repo
            .claim_remote_outbox_item_as_sending(&item.id)
            .await?
        else {
            continue;
        };

        match dispatch_claimed_remote_outbox_item(state, &claimed).await {
            Ok(()) => dispatched += 1,
            Err(RemoteOutboxDispatchError::Network(message)) => {
                state
                    .orchestrator_repo
                    .mark_remote_outbox_pending_after_network_failure(&claimed.id, &message)
                    .await?;
            }
            Err(RemoteOutboxDispatchError::Protocol(message)) => {
                state
                    .orchestrator_repo
                    .mark_remote_outbox_failed(&claimed.id, &message)
                    .await?;
            }
        }
    }

    Ok(dispatched)
}

/// Business Logic（为什么需要这个函数）:
///     应用启动后应自动投递 pending 远端任务，用户无需打开 Workbench 或手动刷新。
///
/// Code Logic（这个函数做什么）:
///     创建 CancellationToken 并启动 tauri async task，按固定 interval 调用 dispatch_remote_outbox_once。
pub fn start_orchestrator_remote_outbox_dispatcher(
    _app_handle: AppHandle,
    state: AppState,
) -> CancellationToken {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    tracing::info!("Orchestrator remote outbox dispatcher 已停止");
                    break;
                }
                _ = tokio::time::sleep(Duration::from_secs(REMOTE_OUTBOX_DISPATCH_INTERVAL_SECS)) => {
                    match dispatch_remote_outbox_once(&state).await {
                        Ok(dispatched) if dispatched > 0 => {
                            tracing::info!("Orchestrator remote outbox 已投递 {dispatched} 条任务");
                        }
                        Ok(_) => {}
                        Err(err) => {
                            tracing::error!("Orchestrator remote outbox dispatch 失败: {err}");
                        }
                    }
                }
            }
        }
    });
    cancel
}

/// Business Logic（为什么需要这个函数）:
///     远端项目任务列表需要刷新 owning device 的真实任务，并把最新 payload 写入本机 mirror cache。
///
/// Code Logic（这个函数做什么）:
///     使用远端 Workbench open-project 恢复 remote local projectId，再调用 RemoteOrchestratorClient::list_tasks，
///     对每个远端任务 upsert mirror，最终返回该远端项目的 mirror rows。
pub async fn sync_remote_task_mirror_for_project(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
) -> Result<Vec<RemoteMirrorTask>, AppError> {
    let context = open_remote_project_for_shortcut(state, remote_shortcut).await?;
    let tasks = RemoteOrchestratorClient::new()
        .list_tasks(&context.base_url, &context.remote_project_id)
        .await?;

    for task in tasks {
        let payload = mirror_payload_from_task(&task)?;
        state
            .orchestrator_repo
            .upsert_remote_task_mirror(
                &context.device_id,
                &context.device_name,
                &context.remote_project_id,
                &context.remote_project_path,
                &task.id,
                &payload,
            )
            .await?;
    }

    state
        .orchestrator_repo
        .list_remote_task_mirrors_for_project(&context.device_id, &context.remote_project_id)
        .await
}

/// 远端项目打开上下文。
///
/// Business Logic（为什么需要这个结构体）:
///     remote shortcut 只有本机保存的设备与路径，执行 Orchestrator HTTP 操作前必须恢复远端 local projectId。
///
/// Code Logic（这个结构体做什么）:
///     保存 base_url、远端 local projectId、设备信息和远端项目路径。
#[derive(Debug, Clone)]
pub struct RemoteOrchestratorProjectContext {
    pub device_id: String,
    pub device_name: String,
    pub base_url: String,
    pub remote_project_id: String,
    pub remote_project_path: String,
}

/// Business Logic（为什么需要这个函数）:
///     所有远端 Orchestrator 操作都必须先通过 Workbench open-project 确保对端有本机 local 项目记录。
///
/// Code Logic（这个函数做什么）:
///     校验 remote shortcut，解析 base_url，调用 RemoteWorkbenchClient::open_project，并返回远端 local projectId。
pub async fn open_remote_project_for_shortcut(
    state: &AppState,
    remote_shortcut: &WorkbenchProjectRow,
) -> Result<RemoteOrchestratorProjectContext, AppError> {
    ensure_remote_shortcut(remote_shortcut)?;
    let base_url = remote_device_base_url(state, &remote_shortcut.device_id)?;
    let remote = RemoteWorkbenchClient::new()
        .open_project(&base_url, &remote_shortcut.path)
        .await?;
    Ok(RemoteOrchestratorProjectContext {
        device_id: remote_shortcut.device_id.clone(),
        device_name: remote_shortcut.device_name.clone(),
        base_url,
        remote_project_id: remote.id,
        remote_project_path: remote.path,
    })
}

/// Business Logic（为什么需要这个函数）:
///     dispatcher 领取 outbox item 后需要完成一次完整远端投递，并在成功后缓存远端任务镜像。
///
/// Code Logic（这个函数做什么）:
///     解析 request_json、打开远端项目、替换 request.project_id、创建远端任务、标 mirrored 并 upsert mirror。
async fn dispatch_claimed_remote_outbox_item(
    state: &AppState,
    item: &OrchestratorRemoteOutboxRow,
) -> Result<(), RemoteOutboxDispatchError> {
    let mut request: RemoteCreateOrchestratorTaskReq = serde_json::from_str(&item.request_json)
        .map_err(|err| {
            RemoteOutboxDispatchError::Protocol(format!("远端 outbox 请求解析失败: {err}"))
        })?;

    let shortcut = WorkbenchProjectRow {
        id: String::new(),
        name: item.remote_project_path.clone(),
        kind: "remote".to_string(),
        device_id: item.device_id.clone(),
        device_name: item.device_name.clone(),
        path: item.remote_project_path.clone(),
        last_opened_at: item.updated_at.clone(),
        created_at: item.created_at.clone(),
        updated_at: item.updated_at.clone(),
    };
    let context = open_remote_project_for_shortcut(state, &shortcut)
        .await
        .map_err(classify_remote_error)?;
    state
        .orchestrator_repo
        .update_remote_outbox_remote_project_id(item.id.as_str(), &context.remote_project_id)
        .await
        .map_err(|err| RemoteOutboxDispatchError::Protocol(err.to_string()))?;

    request.project_id = context.remote_project_id.clone();
    let task = RemoteOrchestratorClient::new()
        .create_task(&context.base_url, request)
        .await
        .map_err(classify_remote_error)?;
    let payload = mirror_payload_from_task(&task)
        .map_err(|err| RemoteOutboxDispatchError::Protocol(err.to_string()))?;
    state
        .orchestrator_repo
        .mark_remote_outbox_mirrored(&item.id, &task.id)
        .await
        .map_err(|err| RemoteOutboxDispatchError::Protocol(err.to_string()))?;
    state
        .orchestrator_repo
        .upsert_remote_task_mirror(
            &context.device_id,
            &context.device_name,
            &context.remote_project_id,
            &context.remote_project_path,
            &task.id,
            &payload,
        )
        .await
        .map_err(|err| RemoteOutboxDispatchError::Protocol(err.to_string()))?;
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     dispatcher 对远端错误的分类必须集中，避免某些路径把离线错误误标为 failed。
///
/// Code Logic（这个函数做什么）:
///     复用 is_remote_network_error，返回 RemoteOutboxDispatchError 的 Network 或 Protocol 变体。
fn classify_remote_error(error: AppError) -> RemoteOutboxDispatchError {
    let message = error.to_string();
    if is_remote_network_error(&error) {
        RemoteOutboxDispatchError::Network(message)
    } else {
        RemoteOutboxDispatchError::Protocol(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{OrchestratorTaskDto, OrchestratorTaskStatus};
    use crate::orchestrator::remote_protocol::RemoteCreateOrchestratorTaskReq;
    use crate::orchestrator::repo::OrchestratorRepo;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// Business Logic（为什么需要这个函数）:
    ///     outbox 仓储测试必须使用隔离内存数据库，避免污染真实远端任务投递记录。
    ///
    /// Code Logic（这个函数做什么）:
    ///     创建单连接 SQLite 内存库，初始化 Orchestrator schema，并返回 repo。
    async fn setup_repo() -> OrchestratorRepo {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("sqlite options")
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("sqlite pool");
        OrchestratorRepo::init_schema(&pool)
            .await
            .expect("orchestrator schema");
        OrchestratorRepo::new(pool)
    }

    /// Business Logic（为什么需要这个函数）:
    ///     多个 outbox 测试都需要同一份远端创建请求，统一 helper 能让断言聚焦状态变化。
    ///
    /// Code Logic（这个函数做什么）:
    ///     构造可序列化的 RemoteCreateOrchestratorTaskReq。
    fn create_req() -> RemoteCreateOrchestratorTaskReq {
        RemoteCreateOrchestratorTaskReq {
            project_id: "remote-project-1".to_string(),
            title: "远端任务".to_string(),
            goal: "在远端执行".to_string(),
            acceptance_criteria: "远端测试通过".to_string(),
            priority: 3,
            queue: false,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     mirror 测试需要一个完整远端任务 DTO，模拟远端 Orchestrator 创建或列表返回。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 task_id/title 构造稳定 OrchestratorTaskDto。
    fn task_dto(task_id: &str, title: &str) -> OrchestratorTaskDto {
        OrchestratorTaskDto {
            id: task_id.to_string(),
            project_id: "remote-project-1".to_string(),
            title: title.to_string(),
            goal: "goal".to_string(),
            acceptance_criteria: "criteria".to_string(),
            status: OrchestratorTaskStatus::Draft,
            priority: 0,
            branch_name: None,
            worktree_id: None,
            session_id: None,
            blocked_reason: None,
            attempt: 0,
            created_at: "2026-07-05T00:00:00Z".to_string(),
            updated_at: "2026-07-05T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
        }
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端离线创建任务必须落到 pending outbox，避免用户提交的工作在重启后丢失。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 pending item 后读取该 item，断言目标设备、路径、请求 JSON 和状态正确。
    #[tokio::test]
    async fn insert_pending_item() {
        let repo = setup_repo().await;
        let request_json = serde_json::to_string(&create_req()).expect("request json");

        let item = repo
            .insert_remote_outbox_pending(
                "device-1",
                "Mac mini",
                "/Users/hans/project",
                None,
                &request_json,
            )
            .await
            .expect("insert pending");
        let persisted = repo
            .get_remote_outbox_item(&item.id)
            .await
            .expect("get pending")
            .expect("pending exists");

        assert_eq!(persisted.device_id, "device-1");
        assert_eq!(persisted.device_name, "Mac mini");
        assert_eq!(persisted.remote_project_path, "/Users/hans/project");
        assert_eq!(persisted.status, RemoteOutboxStatus::Pending);
        assert_eq!(persisted.request_json, request_json);
        assert!(persisted.remote_task_id.is_none());
        assert!(persisted.last_error.is_none());
        assert!(persisted.sent_at.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     dispatcher 多实例或重复 tick 时只能有一个执行者领取同一条 pending item，避免重复创建远端任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     插入 pending 后第一次 claim 成功并变为 sending；第二次 claim 返回 None。
    #[tokio::test]
    async fn claim_pending_item_as_sending() {
        let repo = setup_repo().await;
        let item = repo
            .insert_remote_outbox_pending("device-1", "Mac mini", "/Users/hans/project", None, "{}")
            .await
            .expect("insert pending");

        let claimed = repo
            .claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim pending")
            .expect("claimed");
        let second = repo
            .claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("second claim");

        assert_eq!(claimed.status, RemoteOutboxStatus::Sending);
        assert!(second.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     网络离线或设备不在线只是暂时失败，outbox 必须回到 pending 并保留 last_error 供 UI 提示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim 后调用 network failure 标记方法，断言状态回 pending 且错误被写入。
    #[tokio::test]
    async fn network_failure_returns_item_to_pending_and_stores_last_error() {
        let repo = setup_repo().await;
        let item = repo
            .insert_remote_outbox_pending("device-1", "Mac mini", "/project", None, "{}")
            .await
            .expect("insert pending");
        repo.claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim")
            .expect("claimed");

        let updated = repo
            .mark_remote_outbox_pending_after_network_failure(&item.id, "远端设备不在线")
            .await
            .expect("mark pending");

        assert_eq!(updated.status, RemoteOutboxStatus::Pending);
        assert_eq!(updated.last_error.as_deref(), Some("远端设备不在线"));
        assert!(updated.sent_at.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端协议或校验拒绝说明请求本身不可重试，不能让 dispatcher 无限重试同一条无效任务。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim 后调用 failed 标记方法，断言状态为 failed 且保留错误。
    #[tokio::test]
    async fn remote_validation_failure_marks_failed() {
        let repo = setup_repo().await;
        let item = repo
            .insert_remote_outbox_pending("device-1", "Mac mini", "/project", None, "{}")
            .await
            .expect("insert pending");
        repo.claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim")
            .expect("claimed");

        let failed = repo
            .mark_remote_outbox_failed(&item.id, "项目不能为空")
            .await
            .expect("mark failed");

        assert_eq!(failed.status, RemoteOutboxStatus::Failed);
        assert_eq!(failed.last_error.as_deref(), Some("项目不能为空"));
        assert!(failed.sent_at.is_none());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     outbox 成功投递后必须保存远端 task id 并生成 mirror，后续 UI 才能从 pending 切换为远端任务展示。
    ///
    /// Code Logic（这个测试做什么）:
    ///     claim 后用远端任务 DTO 标记 mirrored，并断言 outbox 与 mirror 表同时更新。
    #[tokio::test]
    async fn successful_send_marks_mirrored_with_remote_task_id() {
        let repo = setup_repo().await;
        let item = repo
            .insert_remote_outbox_pending("device-1", "Mac mini", "/project", None, "{}")
            .await
            .expect("insert pending");
        repo.claim_remote_outbox_item_as_sending(&item.id)
            .await
            .expect("claim")
            .expect("claimed");
        let task = task_dto("remote-task-1", "远端任务");

        let mirrored = repo
            .mark_remote_outbox_mirrored(&item.id, &task.id)
            .await
            .expect("mark mirrored");
        repo.upsert_remote_task_mirror(
            "device-1",
            "Mac mini",
            "remote-project-1",
            "/project",
            &task.id,
            &mirror_payload_from_task(&task).expect("payload"),
        )
        .await
        .expect("upsert mirror");
        let mirrors = repo
            .list_remote_task_mirrors_for_project("device-1", "remote-project-1")
            .await
            .expect("list mirrors");

        assert_eq!(mirrored.status, RemoteOutboxStatus::Mirrored);
        assert_eq!(mirrored.remote_task_id.as_deref(), Some("remote-task-1"));
        assert!(mirrored.sent_at.is_some());
        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].remote_task_id, "remote-task-1");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     同一远端任务多次同步时，本机 mirror 应反映最新远端 payload，不能产生重复卡片。
    ///
    /// Code Logic（这个测试做什么）:
    ///     对同一 `(device_id, remote_task_id)` 连续 upsert 两个 payload，断言只保留一条且标题被替换。
    #[tokio::test]
    async fn mirror_upsert_replaces_payload_for_same_device_and_remote_task() {
        let repo = setup_repo().await;
        let first = task_dto("remote-task-1", "旧标题");
        let second = task_dto("remote-task-1", "新标题");

        repo.upsert_remote_task_mirror(
            "device-1",
            "Mac mini",
            "remote-project-1",
            "/project",
            "remote-task-1",
            &mirror_payload_from_task(&first).expect("first payload"),
        )
        .await
        .expect("first upsert");
        repo.upsert_remote_task_mirror(
            "device-1",
            "Mac mini",
            "remote-project-1",
            "/project",
            "remote-task-1",
            &mirror_payload_from_task(&second).expect("second payload"),
        )
        .await
        .expect("second upsert");

        let mirrors = repo
            .list_remote_task_mirrors_for_project("device-1", "remote-project-1")
            .await
            .expect("list mirrors");
        let payload: OrchestratorTaskDto =
            serde_json::from_str(&mirrors[0].payload_json).expect("payload dto");

        assert_eq!(mirrors.len(), 1);
        assert_eq!(payload.title, "新标题");
    }

    /// Business Logic（为什么需要这个测试）:
    ///     outbox 只能把真实网络/离线错误保持 pending；HTTP 协议响应异常应标 failed 以避免无限重试。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造设备离线、reqwest 发送失败和 HTTP 500 fallback 三类错误文案，断言仅前两类属于网络错误。
    #[test]
    fn network_error_classifier_excludes_http_protocol_failures() {
        assert!(is_remote_network_error(&AppError::generic(
            "远端设备不在线"
        )));
        assert!(is_remote_network_error(&AppError::generic(
            "远端 Orchestrator 请求失败: error sending request"
        )));
        assert!(!is_remote_network_error(&AppError::generic(
            "远端 Orchestrator 请求失败: HTTP 500"
        )));
    }
}
