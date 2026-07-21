//! backend/ui.rs — 后端运行时 UI 适配层。
//!
//! Business Logic（为什么需要这个模块）:
//!     GUI 模式和独立 headless 后端都需要复用同一套后端运行时；运行时不能直接依赖 Tauri AppHandle，
//!     因此需要一个 UI 适配边界来统一事件发送与移动端静态资源读取。
//!
//! Code Logic（这个模块做什么）:
//!     实现 `BackendUi` trait，以及 Tauri GUI 和 headless 两种适配器。

use crate::backend::control_client::{BackendControlClient, BackendControlClientRuntime};
use crate::backend::event_bus::{
    BackendRuntimeCursor, GapResyncOutcome, GuiEventRelayState, RelayClientAction,
    RuntimeRelayMessage,
};
use crate::error::AppError;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

/// GUI 收到 owner event Gap 后的 Tauri 事件名（触发前端 runtime snapshot 刷新）。
pub const BACKEND_RUNTIME_GAP_EVENT: &str = "backend:runtime-gap";
/// Gap resync 后终端 buffer 全量重置事件（sessionId + buffer）。
pub const WORKBENCH_TERMINAL_RESYNC_EVENT: &str = "workbench:terminal-resync";

/// GUI owner event relay 对单条消息的应用结果。
///
/// Business Logic（为什么需要这个枚举）:
///     stream 与 poll fallback 必须区分“无 Gap / Gap 完整 / Gap 不完整”，
///     incomplete 时保留 recovery cursor 重试，禁止以 None 当 brand-new consumer 重连。
///
/// Code Logic（这个枚举做什么）:
///     `NoGap`：Deliver/DropDuplicate，无需为 Gap 重连；
///     `GapComplete`：resync 成功并 attach latest，应关闭当前 stream 按新 cursor 重连；
///     `GapIncomplete`：resync 失败/取消，已 restore recovery cursor，应重连/退避重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiRelayApplyResult {
    /// 非 Gap 消息：无需为 Gap 关闭 stream。
    NoGap,
    /// Gap resync 完整成功：已 attach latest。
    GapComplete,
    /// Gap resync 未完成：已恢复 pre-gap recovery cursor（若有）。
    GapIncomplete,
}

impl GuiRelayApplyResult {
    /// 是否应关闭当前 stream 后按 cursor 重连。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Gap 成功与 incomplete 都不得继续消费 laggy stream 尾部。
    ///
    /// Code Logic（这个函数做什么）:
    ///     `GapComplete` / `GapIncomplete` → true；`NoGap` → false。
    pub fn should_reconnect_stream(self) -> bool {
        matches!(
            self,
            GuiRelayApplyResult::GapComplete | GuiRelayApplyResult::GapIncomplete
        )
    }
}

/// 后端 UI 静态资源载荷。
///
/// Business Logic（为什么需要这个结构）:
///     `/mobile` 静态入口既可能来自 Tauri 嵌入资源，也可能来自 headless 的 dist 目录；HTTP 层需要统一读取字节、
///     MIME 和可选 CSP，而不关心资源来源。
///
/// Code Logic（这个结构做什么）:
///     保存资源 bytes、MIME 字符串和可选 Content-Security-Policy header；Tauri adapter 会从 `tauri::Asset`
///     转换，headless adapter 会从文件系统读取并按扩展名推导 MIME。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAsset {
    /// 静态资源字节内容。
    pub bytes: Vec<u8>,
    /// 静态资源 MIME 类型。
    pub mime_type: String,
    /// 可选 CSP header；headless 文件系统资源通常为空。
    pub csp_header: Option<String>,
}

impl BackendAsset {
    /// 构造后端 UI 静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     adapter 需要用统一入口创建资源载荷，避免 HTTP 层绑定 Tauri 或文件系统资源类型。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收资源 bytes、MIME 与可选 CSP，返回 `BackendAsset`。
    pub fn new(bytes: Vec<u8>, mime_type: String, csp_header: Option<String>) -> Self {
        Self {
            bytes,
            mime_type,
            csp_header,
        }
    }

    /// 从 Tauri asset 转换为后端资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 模式仍由 Tauri asset resolver 提供生产包内资源，但运行时其它层不应直接依赖 `tauri::Asset`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     移动 `tauri::Asset` 的 bytes、mime_type 和 csp_header 字段，构造通用 `BackendAsset`。
    pub fn from_tauri_asset(asset: tauri::Asset) -> Self {
        Self::new(asset.bytes, asset.mime_type, asset.csp_header)
    }

    /// 读取资源 MIME 类型。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     HTTP 响应需要设置 content-type，调用方应通过方法读取而不是假设字段语义。
    ///
    /// Code Logic（这个函数做什么）:
    ///     返回内部 MIME 字符串切片。
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// 读取可选 CSP header。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri 嵌入 HTML 资源可能携带 CSP，HTTP 层需要原样转发给手机浏览器。
    ///
    /// Code Logic（这个函数做什么）:
    ///     将内部 `Option<String>` 转为 `Option<&str>`，避免调用方 clone。
    pub fn csp_header(&self) -> Option<&str> {
        self.csp_header.as_deref()
    }
}

/// 运行 GUI 进程对本机 sidecar 的事件 relay 循环（stream-first）。
///
/// Business Logic（为什么需要这个函数）:
///     sidecar 是唯一事件源；GUI 必须用 afterSequence 消费 owner 事件，Gap 时先 terminal/runtime
///     真实恢复再 attach latest，禁止 silent loss。正常路径使用 control NDJSON live stream，
///     消除固定 250ms catch-up 轮询；仅旧 sidecar 不支持 stream 时退回 poll fallback。
///
/// Code Logic（这个函数做什么）:
///     循环：cached `BackendControlClientRuntime` → `open_events_stream(cursor)` →
///     `apply_gui_relay_message`；EOF/网络错误按 50/100/250/500/1000ms 退避重连并失效缓存；
///     `control_event_stream_unsupported` 进入 5s poll fallback（每轮一次 catch-up + 250ms wait，
///     窗口内不重复 404 探测）；cancel token 退出。
pub async fn run_gui_owner_event_relay(
    ui: Arc<dyn BackendUi>,
    client_runtime: Arc<BackendControlClientRuntime>,
    cancel: CancellationToken,
) {
    let mut relay_state = GuiEventRelayState::default();
    let reconnect_delays = [50_u64, 100, 250, 500, 1_000];
    let mut attempt = 0usize;
    let mut poll_fallback_until: Option<tokio::time::Instant> = None;
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let client = match client_runtime.client() {
            Ok(client) => client,
            Err(_) => {
                wait_relay_retry(&cancel, 500).await;
                continue;
            }
        };

        if poll_fallback_until.is_some_and(|deadline| deadline > tokio::time::Instant::now()) {
            if let Err(error) = run_poll_fallback_once(&client, ui.as_ref(), &mut relay_state).await
            {
                tracing::debug!("GUI owner event relay poll fallback 失败: {error}");
                client_runtime.invalidate_if_current(&client);
            }
            wait_relay_retry(&cancel, 250).await;
            continue;
        }
        poll_fallback_until = None;

        match client
            .open_events_stream(relay_state.cursor().as_ref())
            .await
        {
            Ok(mut stream) => {
                let mut received_message = false;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        message = stream.next_message() => match message {
                            Ok(Some(message)) => {
                                received_message = true;
                                // Gap 成功/不完全后都 break：重连 open_events_stream(cursor)
                                // incomplete 时 cursor 为 pre-gap recovery，禁止 None brand-new 重连。
                                let apply_result = apply_gui_relay_message_with_cancel(
                                    &client,
                                    ui.as_ref(),
                                    &mut relay_state,
                                    message,
                                    Some(&cancel),
                                )
                                .await;
                                if apply_result.should_reconnect_stream() {
                                    break;
                                }
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
                if received_message {
                    attempt = 0;
                }
                client_runtime.invalidate_if_current(&client);
            }
            Err(error) if error.code() == "control_event_stream_unsupported" => {
                tracing::info!(
                    relay_mode = "pollFallback",
                    "GUI owner event stream unsupported"
                );
                poll_fallback_until = Some(tokio::time::Instant::now() + Duration::from_secs(5));
                continue;
            }
            Err(_) => client_runtime.invalidate_if_current(&client),
        }
        let delay = reconnect_delays[attempt.min(reconnect_delays.len() - 1)];
        attempt = attempt.saturating_add(1);
        wait_relay_retry(&cancel, delay).await;
    }
}

/// 统一处理单条 owner relay 消息（Deliver / DropDuplicate / Gap resync）。
///
/// Business Logic（为什么需要这个函数）:
///     stream 与 catch-up fallback 必须共享同一套 enrichment、去重与 Gap 恢复语义，
///     避免双路径漂移导致重复投递或 silent loss。
///
/// Code Logic（这个函数做什么）:
///     `GuiEventRelayState::on_message` → Deliver 走 `emit_gui_relay_event`；DropDuplicate
///     静默；RequestResync 先 `resync_after_gap`：完整则 `attach_at(latest)` 返回 `GapComplete`；
///     incomplete/cancel 则 `restore_recovery_cursor` 返回 `GapIncomplete`。
pub async fn apply_gui_relay_message(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    relay_state: &mut GuiEventRelayState,
    message: RuntimeRelayMessage,
) -> GuiRelayApplyResult {
    apply_gui_relay_message_with_cancel(client, ui, relay_state, message, None).await
}

/// 带可选 cancel 的 relay 消息处理（stream 热路径使用，避免 gap resync 阻塞 shutdown）。
///
/// Business Logic（为什么需要这个函数）:
///     Gap resync 会 list sessions 并 N 次 replay；shutdown 时不能长时间占住 stream select arm。
///     成功 resync 后若继续消费**同一** laggy stream 的 pre-gap 尾部，会把已包含在
///     snapshot 中的 terminal 输出再次转发。incomplete 时必须恢复 pre-gap cursor，
///     禁止以 None 重连成 brand-new consumer。
///
/// Code Logic（这个函数做什么）:
///     与 `apply_gui_relay_message` 相同，但 RequestResync 经 cancel-aware `resync_after_gap`；
///     cancel/incomplete → restore recovery + `GapIncomplete`；完整成功 → attach latest + `GapComplete`。
pub async fn apply_gui_relay_message_with_cancel(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    relay_state: &mut GuiEventRelayState,
    message: RuntimeRelayMessage,
    cancel: Option<&CancellationToken>,
) -> GuiRelayApplyResult {
    match relay_state.on_message(message) {
        RelayClientAction::Deliver {
            event,
            payload,
            owner_instance_id,
            sequence,
        } => {
            emit_gui_relay_event(ui, event, payload, owner_instance_id, sequence);
            GuiRelayApplyResult::NoGap
        }
        RelayClientAction::DropDuplicate => GuiRelayApplyResult::NoGap,
        RelayClientAction::RequestResync {
            owner_instance_id,
            oldest_available,
            latest,
        } => {
            let Some(outcome) = resync_after_gap(
                client,
                ui,
                &owner_instance_id,
                oldest_available,
                latest,
                cancel,
            )
            .await
            else {
                // incomplete/cancel：不 attach latest；恢复 pre-gap recovery cursor 供重连/重试。
                relay_state.restore_recovery_cursor();
                return GuiRelayApplyResult::GapIncomplete;
            };
            tracing::info!(
                terminal_replay_count = outcome.terminal_replay_count,
                runtime_snapshot_refresh_count = outcome.runtime_snapshot_refresh_count,
                "GUI owner event relay gap resync completed"
            );
            // 真实 resync 完成后再 attach latest，避免后续 DropDuplicate 掩盖丢更新。
            relay_state.attach_at(BackendRuntimeCursor {
                owner_instance_id,
                sequence: latest,
            });
            // 调用方应关闭当前 stream 并按新 cursor 重连，避免 pre-gap 尾部二次转发。
            GuiRelayApplyResult::GapComplete
        }
    }
}

/// 转发单条 Deliver 事件到 GUI（含运营通知/Agent runtime 游标 enrichment）。
///
/// Business Logic（为什么需要这个函数）:
///     部分事件需要把 owner/sequence 并入 payload 供前端 handshake 去重；其余事件原样转发。
///
/// Code Logic（这个函数做什么）:
///     operational:notification / workbench:agent-runtime / workbench:terminal-output
///     合并 ownerInstanceId（后两者还带 event-bus sequence）；其余事件原样转发。
///     terminal-output：remote session 用 payload 中 producer owner 与 bus owner 合成 composite；
///     local / 无 producer 时退化为 bus owner；agent-runtime / operational 保持纯 bus owner。
fn emit_gui_relay_event(
    ui: &dyn BackendUi,
    event: String,
    payload: Value,
    owner_instance_id: String,
    sequence: u64,
) {
    if event == crate::orchestrator::notifications::OPERATIONAL_NOTIFICATION_EVENT
        || event == "workbench:agent-runtime"
        || event == "workbench:terminal-output"
    {
        let mut enriched = match payload {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("payload".into(), other);
                map
            }
        };
        let authority = if event == "workbench:terminal-output" {
            let session_id = enriched
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let producer_owner = enriched
                .get("ownerInstanceId")
                .and_then(|v| v.as_str());
            crate::workbench::terminal_authority::terminal_stream_authority(
                session_id,
                &owner_instance_id,
                producer_owner,
            )
        } else {
            owner_instance_id
        };
        enriched.insert("ownerInstanceId".into(), Value::String(authority));
        // agent/operational 握手依赖 event-bus sequence；terminal-output 也附带以便前端诊断。
        enriched.insert("sequence".into(), Value::Number(sequence.into()));
        ui.emit(&event, Value::Object(enriched));
    } else {
        ui.emit(&event, payload);
    }
}

/// 旧 sidecar 不支持 stream 时执行单次 catch-up（不含 sleep）。
///
/// Business Logic（为什么需要这个函数）:
///     mixed-version 下仍需交付 owner 事件；fallback 必须可观测为 poll 模式，
///     且不得把 250ms 等待塞进本函数，以便外层统一 cancel-aware 调度。
///     incomplete Gap 后不得 attach batch.latest，否则永久越过缺口。
///
/// Code Logic（这个函数做什么）:
///     `events_catch_up(after)` → 逐条 `apply_gui_relay_message`；遇 `GapIncomplete` 停止本批
///     后续消息且不 attach latest；仅当仍无 cursor、无 pending recovery、且 latest.sequence>0
///     （真正的 brand-new 空批消费者）才 `attach_at(latest)`。
async fn run_poll_fallback_once(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    relay_state: &mut GuiEventRelayState,
) -> Result<(), AppError> {
    let after = relay_state.cursor();
    let batch = client.events_catch_up(after.as_ref()).await?;
    for message in batch.messages {
        let apply_result = apply_gui_relay_message(client, ui, relay_state, message).await;
        if apply_result == GuiRelayApplyResult::GapIncomplete {
            // incomplete：recovery cursor 已 restore；停止本批，禁止 attach latest。
            return Ok(());
        }
        // GapComplete：cursor 已 attach latest，可继续消费本批后续（若有）；
        // NoGap：正常推进。
    }
    // 仅 brand-new 消费者（从未 attach、无 pending recovery）才用 batch.latest 初始化游标。
    if relay_state.cursor().is_none()
        && !relay_state.recovery_pending()
        && batch.latest.sequence > 0
    {
        relay_state.attach_at(batch.latest);
    }
    Ok(())
}

/// cancel-aware 的 relay 重试等待。
///
/// Business Logic（为什么需要这个函数）:
///     重连退避与 fallback 间隔必须在应用退出时立即中断，避免幽灵 stream task。
///
/// Code Logic（这个函数做什么）:
///     `select!` cancel 与 sleep(ms)；cancel 时立即返回。
async fn wait_relay_retry(cancel: &CancellationToken, delay_ms: u64) {
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
    }
}

/// 测试/smoke 用的可录制 UI adapter。
///
/// Business Logic（为什么需要这个结构）:
///     unit/smoke 需要断言 relay 投递次数、事件名与 terminal chunk，而不能依赖真实 Tauri 窗口。
///
/// Code Logic（这个结构做什么）:
///     Mutex 记录 `(event, payload)`；asset 恒为 None；提供计数与异步等待 helper。
#[derive(Default)]
pub struct RecordingBackendUi {
    events: Mutex<Vec<(String, Value)>>,
}

impl RecordingBackendUi {
    /// 统计指定事件名的投递次数。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     合同测试需要断言 live 一次投递、Gap 后 terminal-resync 等。
    ///
    /// Code Logic（这个函数做什么）:
    ///     扫描内部事件列表按 name 计数。
    pub fn event_count(&self, event: &str) -> usize {
        self.events
            .lock()
            .expect("recording ui 锁中毒")
            .iter()
            .filter(|(name, _)| name == event)
            .count()
    }

    /// 返回已录制事件快照（按到达顺序）。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     smoke 需要检查 payload 字段（如 chunk）而不只是计数。
    ///
    /// Code Logic（这个函数做什么）:
    ///     clone 内部 Vec。
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        self.events.lock().expect("recording ui 锁中毒").clone()
    }

    /// 等待指定事件至少出现一次，超时则 panic。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     stream 交付应在几十毫秒内完成；测试用短超时失败更快定位回归。
    ///
    /// Code Logic（这个函数做什么）:
    ///     轮询 event_count，每 5ms 一次直到 timeout。
    pub async fn wait_for_event(&self, event: &str, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.event_count(event) > 0 {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("等待事件 {event} 超时 ({timeout:?})");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// 等待 terminal-output 事件按序包含给定 chunk 序列。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     重连测试需要确认 stream 断线前后两段输出都按顺序到达 GUI；
    ///     失败诊断不得打印 terminal body（隐私合同）。
    ///
    /// Code Logic（这个函数做什么）:
    ///     收集 `workbench:terminal-output` 的 chunk，按 expected 顺序顺序匹配；
    ///     超时 panic 只报 step/counts/byte_len，不 dump 正文。
    pub async fn wait_for_terminal_chunks(&self, expected: &[&str], timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let chunks: Vec<String> = self
                .snapshot()
                .into_iter()
                .filter(|(name, _)| name == "workbench:terminal-output")
                .filter_map(|(_, payload)| {
                    payload
                        .get("chunk")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            // 顺序敏感：expected[i] 必须在 expected[i-1] 之后出现。
            let mut from = 0usize;
            let mut matched = 0usize;
            for want in expected {
                if let Some(offset) = chunks[from..].iter().position(|got| got == *want) {
                    from += offset + 1;
                    matched += 1;
                } else {
                    break;
                }
            }
            if matched == expected.len() {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                let lens: Vec<usize> = chunks.iter().map(|c| c.len()).collect();
                panic!(
                    "等待 terminal chunks 超时：expected_steps={} matched_steps={} received_count={} received_byte_lens={:?}",
                    expected.len(),
                    matched,
                    chunks.len(),
                    lens
                );
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

impl BackendUi for RecordingBackendUi {
    /// 记录 UI 事件到内存列表。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     测试断言 relay 真实 emit，而非 no-op。
    ///
    /// Code Logic（这个函数做什么）:
    ///     push `(event, payload)`。
    fn emit(&self, event: &str, payload: Value) {
        self.events
            .lock()
            .expect("recording ui 锁中毒")
            .push((event.to_string(), payload));
    }

    /// 测试 adapter 不提供静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     relay 测试不依赖 /mobile asset。
    ///
    /// Code Logic（这个函数做什么）:
    ///     恒返回 None。
    fn asset(&self, _asset_key: &str) -> Option<BackendAsset> {
        None
    }
}

/// Gap 后执行 terminal replay + runtime snapshot 恢复（可观测、cancel-aware）。
///
/// Business Logic（为什么需要这个函数）:
///     RequestResync 不能只发空事件；必须调用既有 sessions.list/replay 与 runtime 刷新路径；
///     多 session 回放期间必须响应 shutdown cancel，避免幽灵 task 长时间占住 relay。
///     **不完整 resync 不得 attach latest**，否则会永久越过缺口。
///
/// Code Logic（这个函数做什么）:
///     先 `resync_terminals_via_control`（list 失败或任一 replay 失败 → Err）；
///     仅 complete 时 emit `backend:runtime-gap` 并返回 Some；cancel/incomplete 返回 None。
pub async fn resync_after_gap(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    owner_instance_id: &str,
    oldest_available: u64,
    latest: u64,
    cancel: Option<&CancellationToken>,
) -> Option<GapResyncOutcome> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return None;
    }
    // 顺序恢复：terminal 先于 runtime 通知；失败/不完全时不 commit cursor。
    let terminal_replay_count =
        match resync_terminals_via_control(client, ui, owner_instance_id, cancel).await {
            Ok(n) => n,
            Err(e) if e.code() == "cancelled" => return None,
            Err(e) => {
                tracing::warn!("gap resync incomplete: terminal replay 失败: {e}");
                return None;
            }
        };
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return None;
    }
    ui.emit(
        BACKEND_RUNTIME_GAP_EVENT,
        json!({
            "ownerInstanceId": owner_instance_id,
            "oldestAvailable": oldest_available,
            "latest": latest,
            "resyncTerminal": true,
            "resyncRuntime": true,
        }),
    );
    Some(GapResyncOutcome {
        terminal_replay_count,
        runtime_snapshot_refresh_count: 1,
    })
}

/// 判断 gap resync 是否应对该 list 行执行 terminal replay。
///
/// Business Logic（为什么需要这个函数）:
///     `sessions.list` 含 SQLite 持久化的 disconnected/exited 会话；对这些会话
///     调 replay 会 NotFound，若把整次 Gap 判 incomplete，会永久无法 attach 新 owner。
///
/// Code Logic（这个函数做什么）:
///     读取 camelCase/snake_case `status`；仅 `running`（或缺失 status 的兼容路径）
///     视为可 replay；`disconnected`/`exited` 及其它非 running 状态返回 false。
fn session_row_is_replayable_for_gap(session: &serde_json::Value) -> bool {
    let status = session
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("running");
    status.eq_ignore_ascii_case("running")
}

/// 收集 Gap resync 需要 inventory 的全部 session 行（本机 + 活跃远端源）。
///
/// Business Logic（为什么需要这个函数）:
///     无 projectId 的 `sessions.list` 只返回本机会话，但活跃 remote bridge 的 terminal-output
///     也发布进同一 event bus；若 Gap 只 replay 本机，会把远端缺口标 complete 并 attach latest。
///     反之，任意已保存但无关的离线 remote shortcut 也不得把本机 terminal/runtime 永久拖进 incomplete。
///     R41 M4：同设备其它失效/未映射 shortcut 的 list 失败不得阻塞仍映射的活跃项目；
///     active mapped 集合为空时跳过 projects.list，避免无关 projects 故障阻断本机恢复。
///
/// Code Logic（这个函数做什么）:
///     1) `sessions.list({})` 本机行；
///     2) `bridges.active_mapped_projects` 取 active bridge 上已映射 local projectId；
///     3) 集合为空 → 只返回本机（不调 projects.list）；
///     4) 仅对这些 projectId 调 `sessions.list({projectId})`，失败 incomplete；
///     5) 按 session id 去重合并返回。
async fn list_sessions_for_gap_resync(
    client: &BackendControlClient,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<Value>, AppError> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(AppError::generic("cancelled"));
    }
    let mut by_id: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();

    let local_sessions: Vec<Value> = client
        .workbench_op("sessions.list", json!({}))
        .await
        .map_err(|e| AppError::generic(format!("gap_resync_list_failed:{}", e.code())))?;
    for session in local_sessions {
        if let Some(id) = session
            .get("id")
            .or_else(|| session.get("sessionId"))
            .and_then(|v| v.as_str())
        {
            by_id.insert(id.to_string(), session);
        }
    }

    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(AppError::generic("cancelled"));
    }
    // R41 M4 / R42 M5：优先项目级 active inventory。
    // 旧 sidecar 未知 op 时 graceful fallback 到 active_devices + projects.list + skip-offline，
    // 禁止 unknown-op 永久 incomplete 阻塞 Gap 恢复。
    match client
        .workbench_op::<Vec<String>>("bridges.active_mapped_projects", json!({}))
        .await
    {
        Ok(active_mapped_projects) => {
            if active_mapped_projects.is_empty() {
                return Ok(by_id.into_values().collect());
            }
            for project_id in active_mapped_projects {
                if cancel.is_some_and(|c| c.is_cancelled()) {
                    return Err(AppError::generic("cancelled"));
                }
                let project_id = project_id.trim().to_string();
                if project_id.is_empty() {
                    continue;
                }
                let remote_sessions: Vec<Value> = client
                    .workbench_op(
                        "sessions.list",
                        json!({ "projectId": project_id }),
                    )
                    .await
                    .map_err(|e| {
                        AppError::generic(format!(
                            "gap_resync_remote_list_failed:{}",
                            e.code()
                        ))
                    })?;
                for session in remote_sessions {
                    if let Some(id) = session
                        .get("id")
                        .or_else(|| session.get("sessionId"))
                        .and_then(|v| v.as_str())
                    {
                        by_id.insert(id.to_string(), session);
                    }
                }
            }
            return Ok(by_id.into_values().collect());
        }
        Err(error) if is_unknown_workbench_control_op_error(&error) => {
            tracing::warn!(
                error = %error,
                "bridges.active_mapped_projects unsupported; falling back to active_devices inventory"
            );
            list_sessions_for_gap_resync_legacy_active_devices(client, cancel, by_id).await
        }
        Err(error) => Err(AppError::generic(format!(
            "gap_resync_active_mapped_projects_failed:{}",
            error.code()
        ))),
    }
}

/// Business Logic（为什么需要这个函数）:
///     新 GUI 可能连上未实现 `bridges.active_mapped_projects` 的旧 sidecar；
///     若把 unknown-op 当永久 incomplete，Gap 恢复会卡死（R42 M5）。
///
/// Code Logic（这个函数做什么）:
///     识别 validation 消息中的「未知 workbench control op」/ unknown op 形态。
fn is_unknown_workbench_control_op_error(error: &AppError) -> bool {
    let code = error.code();
    let msg = error.to_string();
    let lower = msg.to_ascii_lowercase();
    code.contains("未知 workbench control op")
        || lower.contains("未知 workbench control op")
        || lower.contains("unknown workbench control op")
        || (lower.contains("unknown") && lower.contains("op") && lower.contains("workbench"))
}

/// Business Logic（为什么需要这个函数）:
///     旧 sidecar 无 mapped-projects op 时，回退 R40 active_devices + projects.list 路径，
///     跳过无活跃桥的 offline shortcut，避免无关项目阻断本机 cutover（R42 M5）。
///
/// Code Logic（这个函数做什么）:
///     bridges.active_devices → projects.list → 仅 deviceId ∈ active 的 remote 调 sessions.list；
///     合并进 by_id 后返回。
async fn list_sessions_for_gap_resync_legacy_active_devices(
    client: &BackendControlClient,
    cancel: Option<&CancellationToken>,
    mut by_id: std::collections::BTreeMap<String, Value>,
) -> Result<Vec<Value>, AppError> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(AppError::generic("cancelled"));
    }
    let active_devices: Vec<String> = client
        .workbench_op("bridges.active_devices", json!({}))
        .await
        .map_err(|e| {
            AppError::generic(format!(
                "gap_resync_active_bridges_failed:{}",
                e.code()
            ))
        })?;
    let active_device_set: std::collections::HashSet<String> =
        active_devices.into_iter().collect();

    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(AppError::generic("cancelled"));
    }
    let projects: Vec<Value> = client
        .workbench_op("projects.list", json!({}))
        .await
        .map_err(|e| {
            AppError::generic(format!("gap_resync_projects_list_failed:{}", e.code()))
        })?;

    for project in projects {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(AppError::generic("cancelled"));
        }
        let kind = project
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("local");
        if !kind.eq_ignore_ascii_case("remote") {
            continue;
        }
        let Some(project_id) = project
            .get("id")
            .or_else(|| project.get("projectId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            continue;
        };
        let device_id = project
            .get("deviceId")
            .or_else(|| project.get("device_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if device_id.is_empty() || !active_device_set.contains(&device_id) {
            continue;
        }
        let remote_sessions: Vec<Value> = client
            .workbench_op(
                "sessions.list",
                json!({ "projectId": project_id }),
            )
            .await
            .map_err(|e| {
                AppError::generic(format!(
                    "gap_resync_remote_list_failed:{}",
                    e.code()
                ))
            })?;
        for session in remote_sessions {
            if let Some(id) = session
                .get("id")
                .or_else(|| session.get("sessionId"))
                .and_then(|v| v.as_str())
            {
                by_id.insert(id.to_string(), session);
            }
        }
    }

    Ok(by_id.into_values().collect())
}

/// 经 control workbench 列出 session 并 replay，向前端发出 terminal-resync。
///
/// Business Logic（为什么需要这个函数）:
///     Gap 后 GUI 本地 terminal buffer 可能缺中间输出；必须从 owner registry 拉 replay buffer。
///     inventory 必须覆盖本机 + remote shortcut 会话（同一 event bus 的全部 terminal-output 源）。
///     list 失败或任一**可 replay（running）** session 的 replay 失败都必须视为 incomplete。
///     disconnected/exited 仅存在于 SQLite 的会话不得阻断 cutover（同步 status 由后续 list 路径负责）。
///
/// Code Logic（这个函数做什么）:
///     `list_sessions_for_gap_resync`（失败上抛）→ 仅对 `session_row_is_replayable_for_gap`
///     为真的 id 调 `sessions.replay`（失败上抛）→ **pass-through** 已 stamp 的非空
///     `ownerInstanceId`（for_state 已对 remote 合成 composite；R8 H1），仅缺/空时注入
///     local bus owner 后 emit resync；每步前检查 cancel；返回成功次数。
async fn resync_terminals_via_control(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    owner_instance_id: &str,
    cancel: Option<&CancellationToken>,
) -> Result<u64, AppError> {
    if cancel.is_some_and(|c| c.is_cancelled()) {
        return Err(AppError::generic("cancelled"));
    }
    let sessions = list_sessions_for_gap_resync(client, cancel).await?;
    let mut count = 0u64;
    for session in sessions {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(AppError::generic("cancelled"));
        }
        if !session_row_is_replayable_for_gap(&session) {
            // disconnected/exited：不同步 replay buffer，也不让整次 Gap incomplete。
            continue;
        }
        let Some(session_id) = session
            .get("id")
            .or_else(|| session.get("sessionId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
        else {
            continue;
        };
        let mut replay = client
            .workbench_op::<serde_json::Value>(
                "sessions.replay",
                json!({ "sessionId": session_id }),
            )
            .await
            .map_err(|e| {
                AppError::generic(format!("gap_resync_replay_failed:{}", e.code()))
            })?;
        // R8 H1：for_state 已对 remote 合成 composite authority；此处 pass-through 已 stamp 的
        // 非空 ownerInstanceId，禁止无条件覆盖为纯 local bus（否则远端重启后冻结复现）。
        // 仅当 DTO 缺/空 owner 时注入 local bus owner（legacy/mock 兼容）。
        if let Some(obj) = replay.as_object_mut() {
            let existing = obj
                .get("ownerInstanceId")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if existing.is_none() {
                obj.insert(
                    "ownerInstanceId".to_string(),
                    Value::String(owner_instance_id.to_string()),
                );
            }
        }
        ui.emit(WORKBENCH_TERMINAL_RESYNC_EVENT, replay);
        count = count.saturating_add(1);
    }
    Ok(count)
}

/// 后端运行时访问 UI 能力的抽象边界。
///
/// Business Logic（为什么需要这个 trait）:
///     GUI 和 headless 后端共享业务运行时，但前者可以向 Tauri 前端发事件和读嵌入资源，后者只能无界面运行并读本地 dist。
///
/// Code Logic（这个 trait 做什么）:
///     定义线程安全 trait object，暴露事件发送与静态资源读取两个能力；payload 统一用 `serde_json::Value`。
pub trait BackendUi: Send + Sync {
    /// 发送 UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     后端任务会报告终端输出、传输进度等事件；GUI 可接收，headless 模式则应安全忽略。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收事件名和 JSON payload，具体 adapter 决定 emit 或 no-op。
    fn emit(&self, event: &str, payload: Value);

    /// 读取移动端静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `/mobile` HTTP fallback 需要从当前运行模式对应的 UI 资源来源读取构建产物。
    ///
    /// Code Logic（这个函数做什么）:
    ///     接收规范化后的 asset key，命中时返回 `BackendAsset`，缺失或非法时返回 None。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset>;
}

/// Tauri GUI 模式 UI adapter。
///
/// Business Logic（为什么需要这个结构）:
///     桌面 GUI 模式需要继续使用 Tauri AppHandle 向前端窗口广播事件，并读取生产包内嵌入的 frontendDist 资源。
///
/// Code Logic（这个结构做什么）:
///     封装 `AppHandle`，把 emit 委托给 `Emitter::emit`，把 asset 查询委托给 Tauri asset resolver。
#[derive(Clone)]
pub struct TauriBackendUi {
    app_handle: AppHandle,
}

impl TauriBackendUi {
    /// 创建 Tauri UI adapter。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     Tauri setup 阶段拿到 `AppHandle` 后，需要把 GUI 能力注入共享 `AppState`。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存 `AppHandle` clone，供 trait 方法后续使用。
    pub fn new(app_handle: AppHandle) -> Self {
        Self { app_handle }
    }
}

impl BackendUi for TauriBackendUi {
    /// 发送 Tauri UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 模式需要把后台运行时事件转发给前端窗口，供终端输出、进度状态等界面实时更新。
    ///
    /// Code Logic（这个函数做什么）:
    ///     调用 `AppHandle::emit` 广播 JSON payload；失败时仅记录 warn，不打断业务流程。
    fn emit(&self, event: &str, payload: Value) {
        if let Err(error) = self.app_handle.emit(event, payload) {
            tracing::warn!("发送 UI 事件 {event} 失败: {error}");
        }
    }

    /// 读取 Tauri 嵌入静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     GUI 生产包内的移动端资源由 Tauri asset resolver 管理，同时必须避免 resolver 缺省回退到桌面 shell。
    ///
    /// Code Logic（这个函数做什么）:
    ///     遍历 resolver 判断是否存在嵌入资源和 exact key；有嵌入资源但 key 不精确匹配时返回 None，否则读取并转换为 `BackendAsset`。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset> {
        let resolver = self.app_handle.asset_resolver();
        let mut has_embedded_assets = false;
        let mut has_exact_asset = false;

        for (key, _) in resolver.iter() {
            has_embedded_assets = true;
            if key.as_ref().trim_start_matches('/') == asset_key {
                has_exact_asset = true;
                break;
            }
        }

        if has_embedded_assets && !has_exact_asset {
            return None;
        }

        resolver
            .get(asset_key.to_string())
            .map(BackendAsset::from_tauri_asset)
    }
}

/// Headless 后端 UI adapter。
///
/// Business Logic（为什么需要这个结构）:
///     独立后端进程没有 Tauri 窗口，但仍需服务 `/mobile` 页面给手机浏览器；资源来自显式传入的 dist 目录。
///
/// Code Logic（这个结构做什么）:
///     保存 dist 根目录；emit 为 no-op；asset 会先校验相对路径安全性，再读取 dist 下对应文件。
#[derive(Debug, Clone)]
pub struct HeadlessBackendUi {
    dist_dir: PathBuf,
}

impl HeadlessBackendUi {
    /// 创建 headless UI adapter。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     CLI/headless runtime 启动时需要指定 web dist 目录作为手机端静态资源来源。
    ///
    /// Code Logic（这个函数做什么）:
    ///     保存调用方传入的 dist 目录路径，读取时再与规范化 asset key 拼接。
    pub fn new(dist_dir: PathBuf) -> Self {
        Self { dist_dir }
    }

    /// 校验并规范化资源 key。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     headless 文件系统读取不能接受绝对路径、父目录、Windows prefix 或反斜杠，否则可能越界读取。
    ///
    /// Code Logic（这个函数做什么）:
    ///     用 `Path::components` 只接受 Normal 组件，并额外拒绝空路径、反斜杠、Windows 盘符前缀和非 UTF-8 组件。
    fn normalized_asset_path(asset_key: &str) -> Option<PathBuf> {
        if asset_key.is_empty() || asset_key.contains('\\') {
            return None;
        }

        let requested_path = Path::new(asset_key);
        if requested_path.is_absolute() {
            return None;
        }

        let mut safe_path = PathBuf::new();
        for (index, component) in requested_path.components().enumerate() {
            match component {
                Component::Normal(segment) => {
                    let segment = segment.to_str()?;
                    if segment.is_empty()
                        || segment.contains('\\')
                        || (index == 0 && is_windows_prefix_segment(segment))
                    {
                        return None;
                    }
                    safe_path.push(segment);
                }
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => {
                    return None;
                }
            }
        }

        if safe_path.as_os_str().is_empty() {
            return None;
        }

        Some(safe_path)
    }
}

impl BackendUi for HeadlessBackendUi {
    /// 忽略 headless UI 事件。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     headless 后端没有本地前端窗口，但业务运行时仍可统一调用事件接口。
    ///
    /// Code Logic（这个函数做什么）:
    ///     不读取参数、不产生副作用，直接返回。
    fn emit(&self, _event: &str, _payload: Value) {}

    /// 读取 headless dist 静态资源。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     独立后端进程仍要通过局域网 HTTP 为手机浏览器提供 `/mobile` 页面。
    ///
    /// Code Logic（这个函数做什么）:
    ///     先校验 asset key 为安全相对路径，再读取 dist 目录下对应文件并按扩展名填充 MIME。
    fn asset(&self, asset_key: &str) -> Option<BackendAsset> {
        let relative_path = Self::normalized_asset_path(asset_key)?;
        let bytes = std::fs::read(self.dist_dir.join(relative_path)).ok()?;
        Some(BackendAsset::new(
            bytes,
            asset_mime_type(asset_key).to_string(),
            None,
        ))
    }
}

/// 判断路径首段是否是 Windows 盘符前缀。
///
/// Business Logic（为什么需要这个函数）:
///     headless 后端可能在 Unix 上解析来自 HTTP 的资源 key；`C:/...` 在 Unix 会被视作普通相对路径，但语义上是 Windows prefix。
///
/// Code Logic（这个函数做什么）:
///     检查首段是否形如 ASCII 字母 + 冒号，例如 `C:` 或 `z:`。
fn is_windows_prefix_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// 根据 asset key 推导 MIME 类型。
///
/// Business Logic（为什么需要这个函数）:
///     headless 文件系统资源没有 Tauri resolver 提供的 MIME，仍需让手机浏览器按正确类型加载 HTML/JS/CSS 等产物。
///
/// Code Logic（这个函数做什么）:
///     按扩展名返回常见移动端静态资源 MIME；未知扩展名返回 `application/octet-stream`。
fn asset_mime_type(asset_key: &str) -> &'static str {
    match Path::new(asset_key)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// 将任意可序列化 payload 转为 UI 事件 JSON 值。
///
/// Business Logic（为什么需要这个函数）:
///     `AppState::emit_event` 需要在进入 trait object 前把业务 DTO 统一转为 JSON，并在失败时记录日志而不打断业务流程。
///
/// Code Logic（这个函数做什么）:
///     调用 `serde_json::to_value`，成功返回 Value，失败返回 serde_json error。
pub fn serialize_event_payload<T>(payload: T) -> Result<Value, serde_json::Error>
where
    T: Serialize,
{
    serde_json::to_value(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State as AxumState;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicU16, Ordering};

    /// 验证 headless 静态资源拒绝父目录路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     headless 后端会从本地 dist 目录读取手机端静态资源，必须拒绝 `..` 越界，避免暴露项目外文件。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造不存在的 dist 目录适配器，请求 `../mobile.html`，断言不会返回资源。
    #[test]
    fn headless_asset_rejects_parent_paths() {
        let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
        assert!(ui.asset("../mobile.html").is_none());
    }

    /// 验证 headless 静态资源拒绝 Windows prefix 路径。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     Unix 文件系统可创建 `C:mobile.html` 这类文件名，但该输入在跨平台语义上是 Windows drive-relative prefix，
    ///     不应被 headless 资源读取接受。
    ///
    /// Code Logic（这个测试做什么）:
    ///     在临时 dist 目录写入 `C:mobile.html`，断言 headless adapter 仍拒绝该 asset key。
    #[test]
    fn headless_asset_rejects_windows_prefix_paths() {
        let dist = tempfile::tempdir().expect("创建临时 dist 目录");
        std::fs::write(dist.path().join("C:mobile.html"), b"secret").expect("写入测试资源");
        let ui = HeadlessBackendUi::new(dist.path().to_path_buf());

        assert!(ui.asset("C:mobile.html").is_none());
    }

    /// 验证 headless 事件发送是 no-op。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     CLI/headless 模式没有前端窗口可接收事件，但后台任务仍会调用事件接口；这里不能 panic。
    ///
    /// Code Logic（这个测试做什么）:
    ///     调用 headless adapter 的 emit 方法并确认函数正常返回。
    #[test]
    fn headless_emit_is_noop() {
        let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
        ui.emit("workbench:terminal-output", serde_json::json!({"ok": true}));
    }

    /// 启动仅服务 sessions.list/replay 的 mock workbench control。
    ///
    /// Business Logic（为什么需要这个 helper）:
    ///     Gap resync 测试需要真实 HTTP workbench 响应，但不能依赖完整 sidecar。
    ///
    /// Code Logic（这个函数做什么）:
    ///     bind 127.0.0.1:0；list 返回给定 session 行（含 status）；仅 running id 可 replay。
    async fn spawn_terminal_replay_workbench_with_sessions(
        list_rows: Vec<Value>,
        running_session_id: &str,
        buffer: &str,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        #[derive(Clone)]
        struct ReplayState {
            list_rows: Vec<Value>,
            running_session_id: String,
            buffer: String,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            #[allow(dead_code)]
            control_token: String,
            op: String,
            #[serde(default)]
            payload: Value,
        }

        let state = ReplayState {
            list_rows,
            running_session_id: running_session_id.to_string(),
            buffer: buffer.to_string(),
        };
        // sessions.replay 走 workbench/data；sessions.list 走 workbench 元数据路径。
        async fn handle_wb(
            AxumState(s): AxumState<ReplayState>,
            Json(body): Json<WbReq>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let result = match body.op.as_str() {
                // Gap resync 先取 active mapped projects；默认无活跃远端源（仅本机）。
                "bridges.active_mapped_projects" => Value::Array(vec![]),
                "sessions.list" => Value::Array(s.list_rows.clone()),
                "sessions.replay" => {
                    let sid = body
                        .payload
                        .get("sessionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if sid != s.running_session_id {
                        // 模拟 registry 对 disconnected 持久会话返回 NotFound。
                        return Err((
                            StatusCode::NOT_FOUND,
                            Json(json!({
                                "error": "session not found",
                                "code": "not_found",
                            })),
                        ));
                    }
                    json!({
                        "sessionId": sid,
                        "buffer": s.buffer,
                        "truncated": false,
                        "lastSeq": 1,
                        "ownerInstanceId": "owner-1",
                    })
                }
                _ => json!({ "ok": true }),
            };
            Ok(Json(json!({
                "ownerInstanceId": "owner-1",
                "result": result,
            })))
        }
        let app = Router::new()
            .route("/api/backend/control/workbench", post(handle_wb))
            .route("/api/backend/control/workbench/data", post(handle_wb))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind replay fixture");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        (port, server)
    }

    /// 启动单 running session 的 mock workbench control（兼容既有 Gap 测试）。
    ///
    /// Business Logic（为什么需要这个 helper）:
    ///     多数 Gap 测试只需一条可 replay 的 running 会话。
    ///
    /// Code Logic（这个函数做什么）:
    ///     委托 `spawn_terminal_replay_workbench_with_sessions`，list 仅含该 running 行。
    async fn spawn_terminal_replay_workbench(
        session_id: &str,
        buffer: &str,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        spawn_terminal_replay_workbench_with_sessions(
            vec![json!({
                "id": session_id,
                "sessionId": session_id,
                "status": "running",
            })],
            session_id,
            buffer,
        )
        .await
    }

    /// 构造带 terminal replay 的测试 control client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     `apply_gui_relay_message` Gap 分支会调用 sessions.list/replay。
    ///
    /// Code Logic（这个函数做什么）:
    ///     启动 mock workbench 并用 `for_test` 绑定 port/token/owner。
    async fn fake_resync_client_with_terminal_replay(
        session_id: &str,
        buffer: &str,
    ) -> BackendControlClient {
        let (port, _server) = spawn_terminal_replay_workbench(session_id, buffer).await;
        // 泄漏 server join handle：测试进程结束即回收；端口保持到测试完成。
        std::mem::forget(_server);
        BackendControlClient::for_test(port, "token", "owner-1").expect("test client")
    }

    /// 构造含 running + disconnected 持久会话的 resync client。
    ///
    /// Business Logic（为什么需要这个函数）:
    ///     H2 回归：list 混有 SQLite-only disconnected 行时 Gap 仍须 complete 并 attach。
    ///
    /// Code Logic（这个函数做什么）:
    ///     list 返回 running + disconnected；仅 running 可 replay，disconnected 会 404。
    async fn fake_resync_client_with_running_and_disconnected(
        running_id: &str,
        disconnected_id: &str,
        buffer: &str,
    ) -> BackendControlClient {
        let (port, _server) = spawn_terminal_replay_workbench_with_sessions(
            vec![
                json!({
                    "id": running_id,
                    "sessionId": running_id,
                    "status": "running",
                }),
                json!({
                    "id": disconnected_id,
                    "sessionId": disconnected_id,
                    "status": "disconnected",
                }),
            ],
            running_id,
            buffer,
        )
        .await;
        std::mem::forget(_server);
        BackendControlClient::for_test(port, "token", "owner-b").expect("test client")
    }

    /// 验证 live Deliver 一次投递，Gap 触发 resync 并 attach latest。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     stream-first relay 的核心合同：同消息不重复；Gap 后 terminal-resync + cursor=latest。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply Event seq=1 → count=1；apply Gap latest=9 → cursor.sequence=9 且 GapComplete。
    #[tokio::test]
    async fn live_relay_delivers_once_and_gap_resync_attaches_latest() {
        let ui = RecordingBackendUi::default();
        let client = fake_resync_client_with_terminal_replay("s1", "prompt$ ").await;
        let mut state = GuiEventRelayState::default();
        let deliver = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Event {
                owner_instance_id: "owner-1".into(),
                sequence: 1,
                event: "workbench:terminal-output".into(),
                payload: json!({"sessionId":"s1","chunk":"x","seq":1,"ts":1}),
            },
        )
        .await;
        assert_eq!(deliver, GuiRelayApplyResult::NoGap);
        assert_eq!(ui.event_count("workbench:terminal-output"), 1);

        let complete = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Gap {
                owner_instance_id: "owner-1".into(),
                oldest_available: 5,
                latest: 9,
            },
        )
        .await;
        assert_eq!(complete, GuiRelayApplyResult::GapComplete);
        assert!(complete.should_reconnect_stream());
        assert_eq!(state.cursor().unwrap().sequence, 9);
        assert!(!state.recovery_pending());
        assert_eq!(ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT), 1);
        assert_eq!(ui.event_count(BACKEND_RUNTIME_GAP_EVENT), 1);
        // Gap 完整成功后应请求 stream 重连，避免 pre-gap 尾部双写。
        let reconnect = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Gap {
                owner_instance_id: "owner-1".into(),
                oldest_available: 10,
                latest: 12,
            },
        )
        .await;
        assert_eq!(reconnect, GuiRelayApplyResult::GapComplete);
        assert!(reconnect.should_reconnect_stream());
    }

    /// 验证 list 含 disconnected 持久会话时 Gap 仍 complete 并 attach 新 owner。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     owner 切换后 list 常混有 SQLite-only disconnected 行；若对其 replay 导致
    ///     整次 incomplete，GUI 永远无法 attach 新 owner、relay 停止交付。
    ///
    /// Code Logic（这个测试做什么）:
    ///     pre-gap Event under owner-a；Gap under owner-b 且 list=running+disconnected；
    ///     disconnected 若被 replay 会 404。期望 GapComplete、cursor=owner-b latest、
    ///     terminal-resync 恰好 1 次（仅 running）。
    #[tokio::test]
    async fn gap_resync_skips_disconnected_persisted_sessions_and_attaches() {
        let ui = RecordingBackendUi::default();
        let client =
            fake_resync_client_with_running_and_disconnected("running-1", "dead-sqlite-1", "p")
                .await;
        let mut state = GuiEventRelayState::default();
        let deliver = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Event {
                owner_instance_id: "owner-a".into(),
                sequence: 3,
                event: "workbench:terminal-output".into(),
                payload: json!({"sessionId":"running-1","chunk":"x","seq":3,"ts":1}),
            },
        )
        .await;
        assert_eq!(deliver, GuiRelayApplyResult::NoGap);
        assert_eq!(state.cursor().unwrap().owner_instance_id, "owner-a");

        let complete = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Gap {
                owner_instance_id: "owner-b".into(),
                oldest_available: 1,
                latest: 11,
            },
        )
        .await;
        assert_eq!(complete, GuiRelayApplyResult::GapComplete);
        assert!(complete.should_reconnect_stream());
        let cursor = state.cursor().expect("attached");
        assert_eq!(cursor.owner_instance_id, "owner-b");
        assert_eq!(cursor.sequence, 11);
        assert!(!state.recovery_pending());
        assert_eq!(
            ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT),
            1,
            "only running session should be replayed"
        );
        assert_eq!(ui.event_count(BACKEND_RUNTIME_GAP_EVENT), 1);
    }

    /// 验证 list 失败的 incomplete Gap 不得 attach latest。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     瞬时 control 错误若仍 attach latest，会永久越过缺口且无法自愈。
    ///     首帧 incomplete 也不得 after=None：必须 seed gap.owner+0 重连（R33）。
    ///
    /// Code Logic（这个测试做什么）:
    ///     无 pre-gap cursor 时 incomplete Gap → cursor = owner-1/0 + GapIncomplete + recovery_pending。
    #[tokio::test]
    async fn incomplete_gap_resync_does_not_attach_latest() {
        let ui = RecordingBackendUi::default();
        // 不提供 workbench mock：sessions.list 失败 → incomplete
        let client = BackendControlClient::for_test(1, "token", "owner-1").expect("test client");
        let mut state = GuiEventRelayState::default();
        let result = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Gap {
                owner_instance_id: "owner-1".into(),
                oldest_available: 2,
                latest: 9,
            },
        )
        .await;
        assert_eq!(result, GuiRelayApplyResult::GapIncomplete);
        assert!(result.should_reconnect_stream());
        let cursor = state.cursor().expect("first incomplete gap seeds owner+0");
        assert_eq!(cursor.owner_instance_id, "owner-1");
        assert_eq!(
            cursor.sequence, 0,
            "first incomplete gap must reconnect with afterSequence=0, not invent latest"
        );
        assert!(
            state.recovery_pending(),
            "incomplete gap keeps recovery pending"
        );
        assert_eq!(ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT), 0);
        assert_eq!(ui.event_count(BACKEND_RUNTIME_GAP_EVENT), 0);
    }

    /// 验证 incomplete Gap 恢复 pre-gap recovery cursor（H1 回归）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     有已提交游标时 incomplete resync 若 cursor=None 重连，owner 当 brand-new 只回放 ring，
    ///     不再报告原 Gap，造成永久 silent loss。
    ///
    /// Code Logic（这个测试做什么）:
    ///     Event seq=7 → incomplete Gap → cursor.sequence 仍为 7 + GapIncomplete + recovery_pending。
    #[tokio::test]
    async fn incomplete_gap_restores_pre_gap_recovery_cursor() {
        let ui = RecordingBackendUi::default();
        let client = BackendControlClient::for_test(1, "token", "owner-1").expect("test client");
        let mut state = GuiEventRelayState::default();
        let deliver = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Event {
                owner_instance_id: "owner-1".into(),
                sequence: 7,
                event: "workbench:terminal-output".into(),
                payload: json!({"sessionId":"s1","chunk":"x","seq":7,"ts":1}),
            },
        )
        .await;
        assert_eq!(deliver, GuiRelayApplyResult::NoGap);
        assert_eq!(state.cursor().unwrap().sequence, 7);

        let incomplete = apply_gui_relay_message(
            &client,
            &ui,
            &mut state,
            RuntimeRelayMessage::Gap {
                owner_instance_id: "owner-1".into(),
                oldest_available: 10,
                latest: 20,
            },
        )
        .await;
        assert_eq!(incomplete, GuiRelayApplyResult::GapIncomplete);
        assert!(incomplete.should_reconnect_stream());
        assert_eq!(
            state.cursor().unwrap().sequence,
            7,
            "reconnect must use pre-gap recovery cursor, not None"
        );
        assert_eq!(state.cursor().unwrap().owner_instance_id, "owner-1");
        assert!(state.recovery_pending());
        assert_eq!(ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT), 0);
    }

    /// 活跃远端源 inventory 失败时 Gap 必须 incomplete，不得 attach latest。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     R7 M1 / R6 H3：sessions.list 无 projectId 仅本机，但活跃 remote bridge 同样写 event bus；
    ///     若活跃远端 sessions.list 失败仍 complete，会永久越过远端缺口。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock control：bridges.active_mapped_projects 含 remote-proj；
    ///     sessions.list(projectId) 返回 503 → resync incomplete → 不 attach latest。
    #[tokio::test]
    async fn gap_resync_remote_inventory_failure_is_incomplete() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            #[allow(dead_code)]
            control_token: String,
            op: String,
            #[serde(default)]
            payload: Value,
        }

        async fn handle_wb(
            Json(body): Json<WbReq>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let result = match body.op.as_str() {
                "bridges.active_mapped_projects" => json!(["remote-proj"]),
                "sessions.list" => {
                    let project_id = body
                        .payload
                        .get("projectId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if project_id.is_empty() {
                        // 本机 running session
                        json!([{
                            "id": "local-s1",
                            "sessionId": "local-s1",
                            "status": "running",
                        }])
                    } else {
                        return Err((
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "error": "remote offline",
                                "code": "unavailable",
                            })),
                        ));
                    }
                }
                "sessions.replay" => json!({
                    "sessionId": "local-s1",
                    "buffer": "x",
                    "truncated": false,
                    "lastSeq": 1,
                    "ownerInstanceId": "owner-1",
                }),
                _ => json!({ "ok": true }),
            };
            Ok(Json(json!({
                "ownerInstanceId": "owner-1",
                "result": result,
            })))
        }

        let app = Router::new()
            .route("/api/backend/control/workbench", post(handle_wb))
            .route("/api/backend/control/workbench/data", post(handle_wb));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote-fail fixture");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        std::mem::forget(server);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, "token", "owner-1").expect("client");
        let ui = RecordingBackendUi::default();
        let outcome = resync_after_gap(
            &client,
            &ui,
            "owner-1",
            1,
            9,
            None,
        )
        .await;
        assert!(
            outcome.is_none(),
            "active remote inventory failure must make Gap incomplete"
        );
    }

    /// 无关离线 remote shortcut 不得阻断本机 Gap complete。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     R7 M1：inventory 若遍历全部已保存 remote 并 fail-closed，离线无关 shortcut
    ///     会永久停住本机 terminal/runtime 交付。
    ///
    /// Code Logic（这个测试做什么）:
    ///     bridges.active_mapped_projects 为空；projects.list 即使含 offline remote 也不被调用；
    ///     local replay 成功 → Some outcome；不得因无关 remote 失败 incomplete。
    #[tokio::test]
    async fn gap_resync_unrelated_offline_shortcut_does_not_block_local() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            #[allow(dead_code)]
            control_token: String,
            op: String,
            #[serde(default)]
            payload: Value,
        }

        async fn handle_wb(
            Json(body): Json<WbReq>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let result = match body.op.as_str() {
                "bridges.active_mapped_projects" => json!([]),
                "projects.list" => {
                    // R41 M4：空 active mapped 时不得调用 projects.list。
                    return Err((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({
                            "error": "projects.list must not be called when mapped set empty",
                            "code": "internal",
                        })),
                    ));
                }
                "sessions.list" => {
                    let project_id = body
                        .payload
                        .get("projectId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if project_id.is_empty() {
                        json!([{
                            "id": "local-s1",
                            "sessionId": "local-s1",
                            "status": "running",
                        }])
                    } else {
                        return Err((
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({
                                "error": "remote offline",
                                "code": "unavailable",
                            })),
                        ));
                    }
                }
                "sessions.replay" => json!({
                    "sessionId": "local-s1",
                    "buffer": "local-buf",
                    "truncated": false,
                    "lastSeq": 2,
                    "ownerInstanceId": "owner-1",
                }),
                _ => json!({ "ok": true }),
            };
            Ok(Json(json!({
                "ownerInstanceId": "owner-1",
                "result": result,
            })))
        }

        let app = Router::new()
            .route("/api/backend/control/workbench", post(handle_wb))
            .route("/api/backend/control/workbench/data", post(handle_wb));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind offline-shortcut fixture");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        std::mem::forget(server);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, "token", "owner-1").expect("client");
        let ui = RecordingBackendUi::default();
        let outcome = resync_after_gap(&client, &ui, "owner-1", 1, 9, None)
            .await
            .expect("offline unrelated shortcut must not block local Gap complete");
        assert!(
            outcome.terminal_replay_count >= 1,
            "local running session must still resync"
        );
        assert_eq!(ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT), 1);
    }

    /// remote + local running 会话均可 inventory 并 replay 时 Gap complete。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     远端会话成功恢复时必须一并 resync，不能只完成本机。
    ///
    /// Code Logic（这个测试做什么）:
    ///     bridges.active_mapped_projects 含 remote-proj；
    ///     sessions.list 按 projectId 返回 remote session；两次 replay 成功 → count>=2；
    ///     远端 DTO 即使带 remote owner，resync 也覆盖为本机 bus owner。
    #[tokio::test]
    async fn gap_resync_includes_remote_running_sessions() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct WbReq {
            #[allow(dead_code)]
            control_token: String,
            op: String,
            #[serde(default)]
            payload: Value,
        }

        async fn handle_wb(
            Json(body): Json<WbReq>,
        ) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
            let result = match body.op.as_str() {
                "bridges.active_mapped_projects" => json!(["remote-proj"]),
                "sessions.list" => {
                    let project_id = body
                        .payload
                        .get("projectId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if project_id.is_empty() {
                        json!([{
                            "id": "local-s1",
                            "sessionId": "local-s1",
                            "status": "running",
                        }])
                    } else {
                        json!([{
                            "id": "remote:device-a:inner-s1",
                            "sessionId": "remote:device-a:inner-s1",
                            "status": "running",
                        }])
                    }
                }
                "sessions.replay" => {
                    let sid = body
                        .payload
                        .get("sessionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    // mock 绕过 for_state：直接返回 live 路径会使用的 authority
                    // （local → bus owner；remote → composite(local, remote)）。
                    // resync pass-through 后断言与 live 一致。
                    let authority = if sid.starts_with("remote:") {
                        crate::workbench::terminal_authority::compose_remote_terminal_authority(
                            "owner-1",
                            "owner-remote",
                        )
                    } else {
                        "owner-1".to_string()
                    };
                    json!({
                        "sessionId": sid,
                        "buffer": "buf",
                        "truncated": false,
                        "lastSeq": 3,
                        "ownerInstanceId": authority,
                    })
                }
                _ => json!({ "ok": true }),
            };
            Ok(Json(json!({
                "ownerInstanceId": "owner-1",
                "result": result,
            })))
        }

        let app = Router::new()
            .route("/api/backend/control/workbench", post(handle_wb))
            .route("/api/backend/control/workbench/data", post(handle_wb));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote-ok fixture");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        std::mem::forget(server);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, "token", "owner-1").expect("client");
        let ui = RecordingBackendUi::default();
        let outcome = resync_after_gap(
            &client,
            &ui,
            "owner-1",
            1,
            9,
            None,
        )
        .await
        .expect("remote+local inventory must complete");
        assert!(
            outcome.terminal_replay_count >= 2,
            "must replay local and remote running sessions, got {}",
            outcome.terminal_replay_count
        );
        let events = ui.snapshot();
        let resyncs: Vec<_> = events
            .iter()
            .filter(|(name, _)| name == WORKBENCH_TERMINAL_RESYNC_EVENT)
            .collect();
        assert_eq!(resyncs.len(), 2);
        let mut local_seen = false;
        let mut remote_seen = false;
        let expected_remote = crate::workbench::terminal_authority::compose_remote_terminal_authority(
            "owner-1",
            "owner-remote",
        );
        for (_, payload) in &resyncs {
            let sid = payload
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let owner = payload.get("ownerInstanceId").and_then(|v| v.as_str());
            if sid.starts_with("remote:") {
                remote_seen = true;
                assert_eq!(
                    owner,
                    Some(expected_remote.as_str()),
                    "remote resync must pass through composite authority, got {payload}"
                );
            } else {
                local_seen = true;
                assert_eq!(
                    owner,
                    Some("owner-1"),
                    "local resync must use local bus owner, got {payload}"
                );
            }
        }
        assert!(local_seen && remote_seen, "must cover local and remote resync");
    }

    /// 验证 terminal-output live enrichment 对 remote session 合成 composite authority。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     远端 backend 重启后 remote producer owner 变化，live 必须 stamp composite 才能触发
    ///     前端 authority cutover；local session 仍只用 bus owner。
    ///
    /// Code Logic（这个测试做什么）:
    ///     apply remote/local terminal-output Event，断言 emit 的 ownerInstanceId。
    #[tokio::test]
    async fn live_relay_composes_remote_terminal_authority() {
        let ui = RecordingBackendUi::default();
        let client = fake_resync_client_with_terminal_replay("s1", "p").await;
        let mut state = GuiEventRelayState::default();

        let remote_msg = RuntimeRelayMessage::Event {
            owner_instance_id: "owner-1".into(),
            sequence: 1,
            event: "workbench:terminal-output".into(),
            payload: json!({
                "sessionId": "remote:device-a:s1",
                "chunk": "x",
                "seq": 1,
                "ts": 1,
                "ownerInstanceId": "remote-owner-b",
            }),
        };
        assert_eq!(
            apply_gui_relay_message(&client, &ui, &mut state, remote_msg).await,
            GuiRelayApplyResult::NoGap
        );
        let events = ui.snapshot();
        let remote_out = events
            .iter()
            .find(|(name, _)| name == "workbench:terminal-output")
            .expect("remote terminal-output emitted");
        let expected = crate::workbench::terminal_authority::compose_remote_terminal_authority(
            "owner-1",
            "remote-owner-b",
        );
        assert_eq!(
            remote_out.1.get("ownerInstanceId").and_then(|v| v.as_str()),
            Some(expected.as_str()),
            "remote live must stamp composite, got {}",
            remote_out.1
        );

        let ui_local = RecordingBackendUi::default();
        let mut state_local = GuiEventRelayState::default();
        let local_msg = RuntimeRelayMessage::Event {
            owner_instance_id: "owner-1".into(),
            sequence: 2,
            event: "workbench:terminal-output".into(),
            payload: json!({
                "sessionId": "s1",
                "chunk": "y",
                "seq": 1,
                "ts": 1,
                "ownerInstanceId": "payload-producer-ignored",
            }),
        };
        assert_eq!(
            apply_gui_relay_message(&client, &ui_local, &mut state_local, local_msg).await,
            GuiRelayApplyResult::NoGap
        );
        let local_events = ui_local.snapshot();
        let local_out = local_events
            .iter()
            .find(|(name, _)| name == "workbench:terminal-output")
            .expect("local terminal-output emitted");
        assert_eq!(
            local_out.1.get("ownerInstanceId").and_then(|v| v.as_str()),
            Some("owner-1"),
            "local live must keep pure bus owner, got {}",
            local_out.1
        );
    }

    /// 验证同 sequence 重复消息被 DropDuplicate。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     stream 重连可能重放 ring 内已交付事件，GUI 不得双重写终端。
    ///
    /// Code Logic（这个测试做什么）:
    ///     连续 apply 同一 Event 两次，output 计数仍为 1。
    #[tokio::test]
    async fn live_relay_drops_duplicate_sequence_within_owner() {
        let ui = RecordingBackendUi::default();
        let client = fake_resync_client_with_terminal_replay("s1", "p").await;
        let mut state = GuiEventRelayState::default();
        let msg = RuntimeRelayMessage::Event {
            owner_instance_id: "owner-1".into(),
            sequence: 3,
            event: "workbench:terminal-output".into(),
            payload: json!({"sessionId":"s1","chunk":"a","seq":3,"ts":1}),
        };
        assert_eq!(
            apply_gui_relay_message(&client, &ui, &mut state, msg.clone()).await,
            GuiRelayApplyResult::NoGap
        );
        assert_eq!(
            apply_gui_relay_message(&client, &ui, &mut state, msg).await,
            GuiRelayApplyResult::NoGap
        );
        assert_eq!(ui.event_count("workbench:terminal-output"), 1);
        assert_eq!(state.cursor().unwrap().sequence, 3);
    }

    /// 验证 poll fallback 单次 catch-up 不 sleep（仅状态推进）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     fallback 外层负责 250ms wait；内层只做一次 catch-up 以免双倍延迟。
    ///
    /// Code Logic（这个测试做什么）:
    ///     mock catch-up 返回一条 event；run_poll_fallback_once 后 cursor 推进且 emit 1 次。
    #[tokio::test]
    async fn poll_fallback_once_applies_catch_up_batch() {
        #[derive(Clone)]
        struct CatchState {
            owner: String,
            hits: Arc<AtomicU16>,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EventsBody {
            #[allow(dead_code)]
            control_token: String,
            #[allow(dead_code)]
            after_owner_instance_id: Option<String>,
            #[allow(dead_code)]
            after_sequence: Option<u64>,
        }

        let hits = Arc::new(AtomicU16::new(0));
        let state = CatchState {
            owner: "owner-1".into(),
            hits: Arc::clone(&hits),
        };
        let app = Router::new()
            .route(
                "/api/backend/control/events/catch-up",
                post(
                    |AxumState(s): AxumState<CatchState>, Json(_body): Json<EventsBody>| async move {
                        s.hits.fetch_add(1, Ordering::SeqCst);
                        // 与生产 control_api 一致：经 RuntimeRelayMessage serde 输出 wire 格式。
                        let message = RuntimeRelayMessage::Event {
                            owner_instance_id: s.owner.clone(),
                            sequence: 1,
                            event: "workbench:terminal-output".into(),
                            payload: json!({"sessionId":"s1","chunk":"z","seq":1,"ts":1}),
                        };
                        let latest = BackendRuntimeCursor {
                            owner_instance_id: s.owner.clone(),
                            sequence: 1,
                        };
                        Ok::<_, (StatusCode, Json<Value>)>(Json(json!({
                            "messages": [message],
                            "latest": latest,
                        })))
                    },
                ),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let client = BackendControlClient::for_test(port, "token", "owner-1").unwrap();
        let ui = RecordingBackendUi::default();
        let mut relay_state = GuiEventRelayState::default();
        run_poll_fallback_once(&client, &ui, &mut relay_state)
            .await
            .expect("poll once");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(ui.event_count("workbench:terminal-output"), 1);
        assert_eq!(relay_state.cursor().unwrap().sequence, 1);
    }

    /// 验证 poll fallback 在 incomplete Gap 后不 attach batch.latest（H2 回归）。
    ///
    /// Business Logic（为什么需要这个测试）:
    ///     旧 sidecar fallback 若 incomplete 仍 attach latest，会永久越过缺口；
    ///     且后续消息不得继续推进 recovery 状态。
    ///
    /// Code Logic（这个测试做什么）:
    ///     pre-gap cursor=4；catch-up 返回 Gap + 后续 Event；list/replay 失败 →
    ///     cursor 仍为 4、recovery_pending、不 emit 后续 event、不 attach latest=99。
    #[tokio::test]
    async fn poll_fallback_incomplete_gap_keeps_recovery_cursor_not_batch_latest() {
        #[derive(Clone)]
        struct CatchState {
            owner: String,
        }

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EventsBody {
            #[allow(dead_code)]
            control_token: String,
            #[allow(dead_code)]
            after_owner_instance_id: Option<String>,
            #[allow(dead_code)]
            after_sequence: Option<u64>,
        }

        let state = CatchState {
            owner: "owner-1".into(),
        };
        let app = Router::new()
            .route(
                "/api/backend/control/events/catch-up",
                post(
                    |AxumState(s): AxumState<CatchState>, Json(_body): Json<EventsBody>| async move {
                        let gap = RuntimeRelayMessage::Gap {
                            owner_instance_id: s.owner.clone(),
                            oldest_available: 10,
                            latest: 99,
                        };
                        // incomplete 后应停止本批：后续 Event 不得被 apply。
                        let trailing = RuntimeRelayMessage::Event {
                            owner_instance_id: s.owner.clone(),
                            sequence: 50,
                            event: "workbench:terminal-output".into(),
                            payload: json!({"sessionId":"s1","chunk":"should-not-apply","seq":50,"ts":1}),
                        };
                        let latest = BackendRuntimeCursor {
                            owner_instance_id: s.owner.clone(),
                            sequence: 99,
                        };
                        Ok::<_, (StatusCode, Json<Value>)>(Json(json!({
                            "messages": [gap, trailing],
                            "latest": latest,
                        })))
                    },
                ),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        // port 可达 catch-up，但 workbench list/replay 指向同 client port 且无 mock → incomplete。
        let client = BackendControlClient::for_test(port, "token", "owner-1").unwrap();
        let ui = RecordingBackendUi::default();
        let mut relay_state = GuiEventRelayState::default();
        // 先建立 pre-gap 已提交游标。
        assert_eq!(
            apply_gui_relay_message(
                &client,
                &ui,
                &mut relay_state,
                RuntimeRelayMessage::Event {
                    owner_instance_id: "owner-1".into(),
                    sequence: 4,
                    event: "workbench:terminal-output".into(),
                    payload: json!({"sessionId":"s1","chunk":"pre","seq":4,"ts":1}),
                },
            )
            .await,
            GuiRelayApplyResult::NoGap
        );
        assert_eq!(relay_state.cursor().unwrap().sequence, 4);

        run_poll_fallback_once(&client, &ui, &mut relay_state)
            .await
            .expect("poll once with incomplete gap");
        assert_eq!(
            relay_state.cursor().unwrap().sequence,
            4,
            "incomplete gap must keep pre-gap recovery cursor"
        );
        assert!(
            relay_state.recovery_pending(),
            "recovery remains pending until complete attach"
        );
        assert_eq!(
            ui.event_count("workbench:terminal-output"),
            1,
            "trailing catch-up events after incomplete gap must not apply"
        );
        assert_eq!(ui.event_count(WORKBENCH_TERMINAL_RESYNC_EVENT), 0);
    }
}
