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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::async_runtime::JoinHandle;

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

/// Workbench 远端事件桥中的项目 ID 映射。
///
/// Business Logic（为什么需要这个结构体）:
///     merge progress 的 projectId 必须映射成本机 remote shortcut projectId，前端才能按当前项目过滤事件。
///
/// Code Logic（这个结构体做什么）:
///     保存远端设备内 local projectId 到本机 shortcut projectId 的一条映射，供桥接任务实时读取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEventBridgeProjectMapping {
    pub inner_project_id: String,
    pub local_project_id: String,
}

/// Workbench 远端事件桥接后台任务记录。
///
/// Business Logic（为什么需要这个结构体）:
///     同一台设备的事件连接需要随着端口变化替换，同时持续复用已发现的项目 ID 映射。
///
/// Code Logic（这个结构体做什么）:
///     保存任务 base_url、共享 project 映射、任务结束标记和 JoinHandle；registry 按这些字段判断是否重启。
struct RemoteEventBridgeTask {
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    finished: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl RemoteEventBridgeTask {
    /// Business Logic（为什么需要这个函数）:
    ///     事件桥任务可能因为 panic、runtime 取消或主动 abort 结束，registry 需要准确识别并替换。
    ///
    /// Code Logic（这个函数做什么）:
    ///     同时读取自维护 finished 标记和底层 Tokio JoinHandle 状态，任一显示结束则返回 true。
    fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst) || self.handle.inner().is_finished()
    }
}

/// Workbench 远端事件桥接任务登记表。
///
/// Business Logic（为什么需要这个结构体）:
///     list/create remote terminal 可能被频繁调用，但每台设备只应保持一个事件长连接，避免重复输出。
///
/// Code Logic（这个结构体做什么）:
///     用 Mutex<HashMap<device_id, task>> 记录后台任务；同 URL 运行中只更新 project 映射，URL 变化或任务结束时替换。
#[derive(Default)]
pub struct RemoteEventBridgeRegistry {
    tasks: Mutex<HashMap<String, RemoteEventBridgeTask>>,
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

    /// 返回当前登记的远端事件桥数量。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     control status / 诊断需要轻量 bridge 计数，不暴露设备 URL 或订阅细节。
    ///
    /// Code Logic（这个函数做什么）:
    ///     持锁读取 tasks HashMap 长度。
    pub fn active_bridge_count(&self) -> usize {
        self.tasks
            .lock()
            .expect("remote event bridge 锁中毒")
            .len()
    }

    /// Business Logic（为什么需要这个函数）:
    ///     每次进入 remote terminal 项目或创建 remote session 后，都要确保事件桥已连接，并记住项目映射。
    ///
    /// Code Logic（这个函数做什么）:
    ///     按 device_id 读取已有任务；先更新 innerProjectId 映射，再按 base_url/finished 判断是否重启后台长连接。
    pub fn ensure_bridge(
        &self,
        device_id: String,
        base_url: String,
        project_mapping: Option<RemoteEventBridgeProjectMapping>,
        state: AppState,
    ) {
        let mut tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        if let Some(existing) = tasks.get_mut(&device_id) {
            update_project_mapping(&existing.project_ids, project_mapping);
            if !bridge_should_restart(&existing.base_url, existing.is_finished(), &base_url) {
                return;
            }
            existing.handle.abort();
            let project_ids = Arc::clone(&existing.project_ids);
            *existing = spawn_bridge_task(device_id, base_url, project_ids, state);
            return;
        }

        let project_ids = Arc::new(RwLock::new(HashMap::new()));
        update_project_mapping(&project_ids, project_mapping);
        let task = spawn_bridge_task(device_id.clone(), base_url, project_ids, state);
        tasks.insert(device_id, task);
    }

    /// Business Logic（为什么需要这个函数）:
    ///     id-only remote worktree 命令需要在收到远端 DTO 后，把 inner projectId 找回本机 shortcut projectId。
    ///
    /// Code Logic（这个函数做什么）:
    ///     从指定 device 的共享映射表读取 local projectId；未记录时返回 None 供调用方尝试其他恢复方式。
    pub fn local_project_id_for(&self, device_id: &str, inner_project_id: &str) -> Option<String> {
        let tasks = self.tasks.lock().expect("remote event bridge 锁中毒");
        let task = tasks.get(device_id)?;
        let local_project_id = task
            .project_ids
            .read()
            .expect("remote event bridge project 映射读锁中毒")
            .get(inner_project_id)
            .cloned();
        local_project_id
    }
}

/// Business Logic（为什么需要这个函数）:
///     事件桥 registry 需要把任务创建细节集中处理，保证 replacement 和首次创建使用同一套状态字段。
///
/// Code Logic（这个函数做什么）:
///     创建 finished 标记并 spawn 远端事件循环，循环返回时把标记置为 true，随后返回可存入 registry 的 task。
fn spawn_bridge_task(
    device_id: String,
    base_url: String,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
    state: AppState,
) -> RemoteEventBridgeTask {
    let finished = Arc::new(AtomicBool::new(false));
    let task_finished = Arc::clone(&finished);
    let task_device_id = device_id;
    let task_base_url = base_url.clone();
    let task_project_ids = Arc::clone(&project_ids);
    let handle = tauri::async_runtime::spawn(async move {
        remote_event_loop(task_device_id, task_base_url, state, task_project_ids).await;
        task_finished.store(true, Ordering::SeqCst);
    });
    RemoteEventBridgeTask {
        base_url,
        project_ids,
        finished,
        handle,
    }
}

/// Business Logic（为什么需要这个函数）:
///     同设备事件桥可能先从 session list 建立，后续再从 worktree/merge 操作补充更多项目映射。
///
/// Code Logic（这个函数做什么）:
///     若传入映射则写入共享 HashMap；None 表示 session-id-only 命令仅确保连接，不改变映射。
fn update_project_mapping(
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    project_mapping: Option<RemoteEventBridgeProjectMapping>,
) {
    let Some(mapping) = project_mapping else {
        return;
    };
    project_ids
        .write()
        .expect("remote event bridge project 映射写锁中毒")
        .insert(mapping.inner_project_id, mapping.local_project_id);
}

/// Business Logic（为什么需要这个函数）:
///     P2P 设备端口会动态变化，旧事件任务也可能异常结束，registry 必须知道何时替换连接。
///
/// Code Logic（这个函数做什么）:
///     base_url 不一致或旧任务已结束时返回 true；同 URL 且仍运行时返回 false。
fn bridge_should_restart(existing_base_url: &str, finished: bool, next_base_url: &str) -> bool {
    finished || existing_base_url != next_base_url
}

/// Business Logic（为什么需要这个函数）:
///     本机 session/merge 事件 emit 时，也要同步发布到 HTTP broadcast channel 供远端设备订阅。
///
/// Code Logic（这个函数做什么）:
///     从 AppState 向 `workbench_remote_events` broadcast sender 发送事件；无订阅者时记录 debug 后忽略。
pub fn publish_workbench_remote_event_from_state(state: &AppState, event: WorkbenchRemoteEvent) {
    if let Err(error) = state.workbench_remote_events.send(event) {
        tracing::debug!("无 Workbench 远端事件订阅者: {error}");
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端事件连接可能因网络切换、对端重启或 HTTP server 重启而断开，需要自动恢复。
///
/// Code Logic（这个函数做什么）:
///     循环连接 `/api/workbench/events`；连接失败或流结束后等待短暂延迟再重连。
async fn remote_event_loop(
    device_id: String,
    base_url: String,
    state: AppState,
    project_ids: Arc<RwLock<HashMap<String, String>>>,
) {
    let client = reqwest::Client::new();
    loop {
        if let Err(error) =
            read_remote_event_stream(&client, &device_id, &base_url, &state, &project_ids).await
        {
            tracing::debug!("Workbench 远端事件流断开，将重连: {error}");
        }
        tokio::time::sleep(Duration::from_secs(REMOTE_EVENT_RECONNECT_DELAY_SECS)).await;
    }
}

/// Business Logic（为什么需要这个函数）:
///     一次远端事件连接负责持续读取 NDJSON 并把远端内部 ID 映射成本机 remote ID。
///
/// Code Logic（这个函数做什么）:
///     GET 远端 events endpoint，按 chunk 累积行，逐行反序列化 WorkbenchRemoteEvent 后 emit 到本机后端 UI adapter。
async fn read_remote_event_stream(
    client: &reqwest::Client,
    device_id: &str,
    base_url: &str,
    state: &AppState,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
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

    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| AppError::generic(format!("读取远端 Workbench 事件流失败: {error}")))?
    {
        process_event_chunk(device_id, state, project_ids, &mut buffer, &chunk);
    }
    Ok(())
}

/// Business Logic（为什么需要这个函数）:
///     NDJSON 事件可能被 TCP chunk 拆开，必须跨 chunk 保留未完成的一行，并避免破坏 UTF-8 字符。
///
/// Code Logic（这个函数做什么）:
///     复用纯解析 helper 得到已映射事件，再逐个 emit 到本机后端 UI adapter。
fn process_event_chunk(
    device_id: &str,
    state: &AppState,
    project_ids: &Arc<RwLock<HashMap<String, String>>>,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) {
    let project_ids = project_ids
        .read()
        .expect("remote event bridge project 映射读锁中毒")
        .clone();
    for event in process_event_chunk_to_events(device_id, &project_ids, buffer, chunk) {
        emit_mapped_remote_event(state, event);
    }
}

/// Business Logic（为什么需要这个函数）:
///     远端事件流中的用户输出可能包含中文或 emoji，跨 chunk 解析必须以完整 NDJSON 行为边界。
///
/// Code Logic（这个函数做什么）:
///     以 byte buffer 追加 chunk，按 `b'\n'` 拆完整行；仅对完整行做 UTF-8 解码和 serde 解析，返回已映射事件。
fn process_event_chunk_to_events(
    device_id: &str,
    project_ids: &HashMap<String, String>,
    buffer: &mut Vec<u8>,
    chunk: &[u8],
) -> Vec<WorkbenchRemoteEvent> {
    buffer.extend_from_slice(chunk);
    let mut events = Vec::new();
    while let Some(index) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=index).collect::<Vec<_>>();
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let line = trim_ascii_whitespace_bytes(&line);
        if line.is_empty() {
            continue;
        }
        match std::str::from_utf8(line) {
            Ok(text) => match serde_json::from_str::<WorkbenchRemoteEvent>(text) {
                Ok(event) => {
                    events.push(map_remote_event_for_device(device_id, project_ids, event))
                }
                Err(error) => tracing::debug!("解析 Workbench 远端事件失败: {error}; line={text}"),
            },
            Err(error) => tracing::debug!("远端 Workbench 事件不是合法 UTF-8: {error}"),
        }
    }
    events
}

/// Business Logic（为什么需要这个函数）:
///     NDJSON 行尾可能带 CRLF 或空白，解析前应清理协议空白但不能修改 JSON 字符串内容。
///
/// Code Logic（这个函数做什么）:
///     仅裁剪字节切片两端的 ASCII whitespace，返回原 buffer 内的有效 JSON 行切片。
fn trim_ascii_whitespace_bytes(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// Business Logic（为什么需要这个函数）:
///     本机前端只监听 Tauri event，不关心事件来自本机 PTY 还是远端 HTTP stream。
///
/// Code Logic（这个函数做什么）:
///     按事件类型 emit 到现有 `workbench:*` 事件名；headless adapter 可安全 no-op。
fn emit_mapped_remote_event(state: &AppState, event: WorkbenchRemoteEvent) {
    match event {
        WorkbenchRemoteEvent::TerminalOutput(payload) => {
            state.emit_event("workbench:terminal-output", payload);
        }
        WorkbenchRemoteEvent::TerminalStatus(payload) => {
            state.emit_event("workbench:terminal-status", payload);
        }
        WorkbenchRemoteEvent::MergeProgress(payload) => {
            state.emit_event("workbench:merge-progress", payload);
        }
    };
}

/// Business Logic（为什么需要这个函数）:
///     远端设备发出的事件只包含自己的 local ID，本机 UI 需要可区分设备归属的 remote ID。
///
/// Code Logic（这个函数做什么）:
///     根据事件类型把 sessionId/projectId/worktreeId 映射为 `remote:<device_id>:<inner_id>`。
fn map_remote_event_for_device(
    device_id: &str,
    project_ids: &HashMap<String, String>,
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
            payload.project_id = project_ids
                .get(&payload.project_id)
                .cloned()
                .unwrap_or_else(|| remote_entity_id(device_id, &payload.project_id));
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

        let mapped = map_remote_event_for_device("device-a", &HashMap::new(), event);

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
    ///     远端 merge 进度事件后续会被本机 UI 按本机 remote shortcut projectId 过滤。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造 innerProjectId -> local shortcut 映射，断言 projectId 使用 shortcut、worktreeId 仍使用 remote entity。
    #[test]
    fn map_remote_merge_progress_event_uses_local_shortcut_project_id() {
        let stage = serde_json::json!({"id":"mergeMain","status":"running"});
        let event = WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
            project_id: "inner-project".to_string(),
            worktree_id: "inner-worktree".to_string(),
            stage: stage.clone(),
        });
        let project_ids = HashMap::from([(
            "inner-project".to_string(),
            "remote:device-a:shortcut-project".to_string(),
        )]);

        let mapped = map_remote_event_for_device("device-a", &project_ids, event);

        assert_eq!(
            mapped,
            WorkbenchRemoteEvent::MergeProgress(WorkbenchMergeProgressPayload {
                project_id: "remote:device-a:shortcut-project".to_string(),
                worktree_id: "remote:device-a:inner-worktree".to_string(),
                stage,
            })
        );
    }

    /// Business Logic（为什么需要这个测试）:
    ///     远端事件流承载用户终端输出，中文和 emoji 不能因为 TCP chunk 切分被替换成乱码。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造包含多字节字符的 NDJSON 行，并故意在 emoji UTF-8 字节中间切分，断言完整行解析后内容保持不变。
    #[test]
    fn process_event_chunk_preserves_multibyte_characters_across_chunks() {
        let line = serde_json::json!({
            "type": "terminalOutput",
            "payload": {
                "sessionId": "inner-session",
                "chunk": "中文🚀输出",
                "seq": 1,
                "ts": 1000
            }
        })
        .to_string()
            + "\n";
        let bytes = line.as_bytes();
        let rocket_offset = line.find('🚀').expect("fixture should contain rocket");
        let split_at = rocket_offset + 1;
        let mut buffer = Vec::new();
        let project_ids = HashMap::new();

        let first = process_event_chunk_to_events(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[..split_at],
        );
        let second = process_event_chunk_to_events(
            "device-a",
            &project_ids,
            &mut buffer,
            &bytes[split_at..],
        );

        assert!(first.is_empty());
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0],
            WorkbenchRemoteEvent::TerminalOutput(WorkbenchTerminalOutputPayload {
                session_id: "remote:device-a:inner-session".to_string(),
                chunk: "中文🚀输出".to_string(),
                seq: 1,
                ts: 1000,
            })
        );
        assert!(buffer.is_empty());
    }

    /// Business Logic（为什么需要这个测试）:
    ///     P2P HTTP 端口可能随对端重启而变化，事件桥必须替换旧连接而不是继续复用 stale URL。
    ///
    /// Code Logic（这个测试做什么）:
    ///     直接覆盖 registry 的重启判定 helper，断言 URL 变化或旧任务结束会触发 replacement，同 URL 运行中不会。
    #[test]
    fn bridge_restart_decision_replaces_finished_or_changed_base_url() {
        assert!(bridge_should_restart(
            "http://127.0.0.1:1000",
            false,
            "http://127.0.0.1:2000"
        ));
        assert!(bridge_should_restart(
            "http://127.0.0.1:1000",
            true,
            "http://127.0.0.1:1000"
        ));
        assert!(!bridge_should_restart(
            "http://127.0.0.1:1000",
            false,
            "http://127.0.0.1:1000"
        ));
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
