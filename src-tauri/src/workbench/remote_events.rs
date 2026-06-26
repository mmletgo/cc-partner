//! workbench/remote_events.rs — Workbench 远端事件桥接
//!
//! Business Logic（为什么需要这个模块）:
//!     remote shortcut 的 terminal 输出、状态和 merge 进度需要从项目所在设备实时转发到本机 UI。
//!
//! Code Logic（这个模块做什么）:
//!     定义可通过 broadcast/NDJSON 传输的事件 DTO，提供本机事件发布 helper，
//!     并维护按 device_id 去重的远端 `/api/workbench/events` 长连接桥接任务。

use crate::error::AppError;
use crate::state::AppState;
use crate::workbench::remote_ids::remote_entity_id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};

const REMOTE_EVENT_RECONNECT_DELAY_SECS: u64 = 2;

/// Workbench 远端终端输出 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 需要把远端 PTY 增量输出传回本机 xterm。
///
/// Code Logic（这个结构体做什么）:
///     对齐本机 `workbench:terminal-output` event payload，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalOutputPayload {
    pub session_id: String,
    pub chunk: String,
    pub seq: u64,
    pub ts: i64,
}

/// Workbench 远端终端状态 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote terminal 的 running/exited/disconnected 状态需要同步到本机 tab 和状态栏。
///
/// Code Logic（这个结构体做什么）:
///     对齐本机 `workbench:terminal-status` event payload，字段使用 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchTerminalStatusPayload {
    pub session_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub ts: i64,
}

/// Workbench 远端 merge 进度 payload。
///
/// Business Logic（为什么需要这个结构体）:
///     remote worktree merge 后续需要把多阶段进度桥接回本机 UI。
///
/// Code Logic（这个结构体做什么）:
///     project/worktree 使用字符串 ID，stage 保持 JSON 值以复用命令层现有阶段 DTO。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchMergeProgressPayload {
    pub project_id: String,
    pub worktree_id: String,
    pub stage: Value,
}

/// Workbench 可跨 HTTP NDJSON 传输的事件。
///
/// Business Logic（为什么需要这个枚举）:
///     远端事件流需要在一条连接中承载 terminal output、terminal status 和 merge progress 多种事件。
///
/// Code Logic（这个枚举做什么）:
///     使用 serde 内部 tag `{type,payload}`，type 按 camelCase 输出为前端和桥接层约定的稳定值。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload", rename_all = "camelCase")]
pub enum WorkbenchRemoteEvent {
    TerminalOutput(WorkbenchTerminalOutputPayload),
    TerminalStatus(WorkbenchTerminalStatusPayload),
    MergeProgress(WorkbenchMergeProgressPayload),
}

/// Workbench 远端事件桥接任务登记表。
///
/// Business Logic（为什么需要这个结构体）:
///     list/create remote terminal 可能被频繁调用，但每台设备只应保持一个事件长连接，避免重复输出。
///
/// Code Logic（这个结构体做什么）:
///     用 Mutex<HashMap<device_id, JoinHandle>> 记录后台任务；仍运行的任务不重复启动，已结束任务会被替换。
#[derive(Default)]
pub struct RemoteEventBridgeRegistry {
    tasks: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl RemoteEventBridgeRegistry {
    /// Business Logic（为什么需要这个函数）:
    ///     AppState 初始化时需要创建空的远端事件桥接登记表。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回没有任何设备连接任务的 registry。
    pub fn new() -> Self {
        Self::default()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次进入 remote terminal 项目或创建 remote session 后，都要确保事件桥已连接。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id 检查已有 JoinHandle；仍运行则直接返回，否则 spawn 一个带自动重连的后台任务。
    pub fn ensure_bridge(&self, device_id: String, base_url: String, app: AppHandle) {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        if tasks.contains_key(&device_id) {
            return;
        }

        let task_device_id = device_id.clone();
        let handle = tauri::async_runtime::spawn(async move {
            remote_event_loop(task_device_id, base_url, app).await;
        });
        tasks.insert(device_id, handle);
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机 session/merge 事件 emit 时，也要同步发布到 HTTP broadcast channel 供远端设备订阅。
///
/// Code Logic（这个函数做什么）:
///     从 AppHandle 读取 AppState，向 `workbench_remote_events` broadcast sender 发送事件；无订阅者时忽略错误。
pub fn publish_workbench_remote_event(app: &AppHandle, event: WorkbenchRemoteEvent) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let _ = state.workbench_remote_events.send(event);
}

/// Business Logic（为什么需要这个函数）:
///     远端事件连接可能因网络切换、对端重启或 HTTP server 重启而断开，需要自动恢复。
///
/// Code Logic（这个函数做什么）:
///     循环连接 `/api/workbench/events`；连接失败或流结束后等待短暂延迟再重连。
async fn remote_event_loop(device_id: String, base_url: String, app: AppHandle) {
    let client = reqwest::Client::new();
    loop {
        if let Err(error) = read_remote_event_stream(&client, &device_id, &base_url, &app).await {
            tracing::debug!("Workbench 远端事件流断开，将重连: {error}");
        }
        tokio::time::sleep(Duration::from_secs(REMOTE_EVENT_RECONNECT_DELAY_SECS)).await;
    }
}

/// Business Logic（为什么需要这个函数）:
///     一次远端事件连接负责持续读取 NDJSON 并把远端内部 ID 映射成本机 remote ID。
///
/// Code Logic（这个函数做什么）:
///     GET 远端 events endpoint，按 chunk 累积行，逐行反序列化 WorkbenchRemoteEvent 后 emit 到本机 Tauri。
async fn read_remote_event_stream(
    client: &reqwest::Client,
    device_id: &str,
    base_url: &str,
    app: &AppHandle,
) -> Result<(), AppError> {
    let mut response = client
        .get(event_stream_url(base_url))
        .send()
        .await
        .map_err(|error| AppError::generic(format!("连接远端 Workbench 事件流失败: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AppError::generic(format!(
            "连接远端 Workbench 事件流失败: HTTP {status}: {}",
            body.trim()
        )));
    }

    let mut buffer = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::generic(format!("读取远端 Workbench 事件流失败: {error}")))?
    {
        process_event_chunk(device_id, app, &mut buffer, &chunk);
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     NDJSON 事件可能被 TCP chunk 拆开，必须跨 chunk 保留未完成的一行。
///
/// Code Logic（这个函数做什么）:
///     把新 chunk 追加到 buffer，按换行取完整 JSON 行；解析失败只记录 debug 并继续读取后续事件。
fn process_event_chunk(device_id: &str, app: &AppHandle, buffer: &mut String, chunk: &[u8]) {
    buffer.push_str(&String::from_utf8_lossy(chunk));
    while let Some(index) = buffer.find('\n') {
        let line = buffer[..index].trim().to_string();
        let remaining = buffer[index + 1..].to_string();
        *buffer = remaining;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<WorkbenchRemoteEvent>(&line) {
            Ok(event) => {
                emit_mapped_remote_event(app, map_remote_event_for_device(device_id, event))
            }
            Err(error) => tracing::debug!("解析 Workbench 远端事件失败: {error}; line={line}"),
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     本机前端只监听 Tauri event，不关心事件来自本机 PTY 还是远端 HTTP stream。
///
/// Code Logic（这个函数做什么）:
///     按事件类型 emit 到现有 `workbench:*` 事件名；失败只记录 warn，不中断桥接循环。
fn emit_mapped_remote_event(app: &AppHandle, event: WorkbenchRemoteEvent) {
    let result = match event {
        WorkbenchRemoteEvent::TerminalOutput(payload) => {
            app.emit("workbench:terminal-output", payload)
        }
        WorkbenchRemoteEvent::TerminalStatus(payload) => {
            app.emit("workbench:terminal-status", payload)
        }
        WorkbenchRemoteEvent::MergeProgress(payload) => {
            app.emit("workbench:merge-progress", payload)
        }
    };
    if let Err(error) = result {
        tracing::warn!("桥接 Workbench 远端事件失败: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端设备发出的事件只包含自己的 local ID，本机 UI 需要可区分设备归属的 remote ID。
///
/// Code Logic（这个函数做什么）:
///     根据事件类型把 sessionId/projectId/worktreeId 映射为 `remote:<device_id>:<inner_id>`。
fn map_remote_event_for_device(
    device_id: &str,
    event: WorkbenchRemoteEvent,
) -> WorkbenchRemoteEvent {
    match event {
        WorkbenchRemoteEvent::TerminalOutput(mut payload) => {
            payload.session_id = remote_entity_id(device_id, &payload.session_id);
            WorkbenchRemoteEvent::TerminalOutput(payload)
        }
        WorkbenchRemoteEvent::TerminalStatus(mut payload) => {
            payload.session_id = remote_entity_id(device_id, &payload.session_id);
            WorkbenchRemoteEvent::TerminalStatus(payload)
        }
        WorkbenchRemoteEvent::MergeProgress(mut payload) => {
            payload.project_id = remote_entity_id(device_id, &payload.project_id);
            payload.worktree_id = remote_entity_id(device_id, &payload.worktree_id);
            WorkbenchRemoteEvent::MergeProgress(payload)
        }
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端设备 base URL 可能带尾斜杠，事件桥必须拼出稳定 endpoint。
///
/// Code Logic（这个函数做什么）:
///     去掉 base URL 尾部 `/` 后追加 `/api/workbench/events`。
fn event_stream_url(base_url: &str) -> String {
    format!("{}/api/workbench/events", base_url.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     远端 terminal 输出事件桥接到本机后，sessionId 必须带设备前缀才能和本机会话区分。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 terminalOutput 事件并映射 device-a，断言 payload.sessionId 使用 remote entity ID。
    #[test]
    fn map_remote_terminal_output_event_prefixes_session_id() {
        let event = WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
            session_id: "inner-session".to_string(),
            chunk: "hello".to_string(),
            seq: 7,
            ts: 1000,
        });

        let mapped = map_remote_event_for_device("device-a", event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:inner-session".to_string(),
                chunk: "hello".to_string(),
                seq: 7,
                ts: 1000,
            })
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端 merge 进度事件后续会被本机 UI 按项目和 worktree 过滤，两个 ID 都必须映射。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 mergeProgress 事件，断言 projectId/worktreeId 都带 remote entity 前缀且 stage 保持不变。
    #[test]
    fn map_remote_merge_progress_event_prefixes_project_and_worktree_ids() {
        let stage = serde_json::json!({"id":"mergeMain","status":"running"});
        let event = WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
            project_id: "inner-project".to_string(),
            worktree_id: "inner-worktree".to_string(),
            stage: stage.clone(),
        });

        let mapped = map_remote_event_for_device("device-a", event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
                project_id: "remote:device-a:inner-project".to_string(),
                worktree_id: "remote:device-a:inner-worktree".to_string(),
                stage,
            })
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     设备发现保存的 base URL 可能包含尾斜杠，事件桥不应生成双斜杠路径。
    ///
    /// Code Logic（这个测试做什么）:
    ///     传入带尾斜杠 base URL，断言 endpoint URL 规范化。
    #[test]
    fn event_stream_url_trims_trailing_slash() {
        assert_eq!(
            event_stream_url("http://127.0.0.1:1420/"),
            "http://127.0.0.1:1420/api/workbench/events"
        );
    }
}
