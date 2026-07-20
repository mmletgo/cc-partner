# Workbench Terminal Low-Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除桌面 Workbench 终端固定 250ms 回显等待，并在不改变 PTY/tmux、sidecar sole-owner、输入不重放和 Gap/replay 语义的前提下，使本机真实回显 p95 ≤ 50ms。

**Architecture:** 复用已有 `/api/backend/control/events/stream` 作为 GUI 正常事件路径，保留 owner cursor、dedupe、Gap 和 replay；GUI 缓存 control client，前端用每 session leading-edge FIFO input pump 串行写入。输出 store 同时维护 bounded replay snapshot 与 imperative live delta，已挂载 xterm 直接按 generation/appendId 增量写入；前后端完整历史仅在 mount/resync/snapshot 时物化。

**Tech Stack:** Rust 2021、Tokio、axum、reqwest 0.12、portable-pty/tmux、Tauri 2 events、React 19、TypeScript、xterm 6、Vitest、现有 Rust smoke harness。

**Design Spec:** [`docs/superpowers/specs/2026-07-20-workbench-terminal-low-latency-design.md`](../specs/2026-07-20-workbench-terminal-low-latency-design.md)

## Global Constraints

- `HeadlessOwner` 继续是 PTY/tmux、remote bridge 和 event bus 的唯一 owner；`GuiClient` 不得本地 attach 或直接写 PTY。
- 终端显示只使用真实 PTY 输出；禁止 optimistic local echo。
- `sessions.write` 明确失败或响应不确定时都不得自动重放同一批输入。
- 不新增 WebSocket、第二 event bus、第二 terminal 模型、LAN capability、数据库 schema 或第三方状态库。
- `/api/backend/control/events/stream` 是正常路径；`events/catch-up` 只作 mixed-version unsupported fallback 和显式恢复测试。
- stream body 无 overall timeout；所有非 stream control 请求继续使用现有逐请求 timeout。
- terminal input/output、Prompt、路径、token、远端 URL 和命令正文不得进入日志、metric 或测试产物。
- 前端 Hooks 必须位于所有 early return 之前；不卸载隐藏 xterm DOM。
- 新增或修改的 TypeScript 生产函数必须按项目模板补齐中文 Business Logic / Code Logic docstring；Rust 公共接口同步维护现有中文 rustdoc。
- 后端 replay 容量保持 120,000 Unicode scalar；前端 replay 容量保持 200,000 UTF-16 unit。
- 本机 release GUI：key-to-visible p95 ≤ 50ms、p99 ≤ 100ms；owner publish→GUI listener p95 ≤ 20ms。

---

## File Structure

- `src-tauri/src/backend/control_client.rs`：control client cache、events stream connect、bounded NDJSON decoder。
- `src-tauri/src/backend/ui.rs`：stream-first GUI relay、cursor/reconnect/Gap resync、fallback 分类。
- `src-tauri/src/state.rs`：GUI control runtime 与 relay cancel token 生命周期字段。
- `src-tauri/src/backend/runtime.rs`：构造 client runtime、shutdown cancel。
- `src-tauri/src/commands/workbench/common.rs`：Workbench GUI proxy 复用 cached client，mutation 不重放。
- `src-tauri/src/lib.rs`：启动并保存 GUI relay cancel token。
- `src-tauri/src/workbench/sessions.rs`：后端 `SessionReplayBuffer` chunk deque。
- `src-tauri/tests/runtime_authority_smoke.rs`：真实 control stream、cursor reconnect、Gap/resync 集成合同。
- `web/src/pages/Workbench/terminalInputPump.ts`：每 session leading-edge FIFO 输入泵。
- `web/src/pages/Workbench/terminalInputPump.test.ts`：输入顺序、合并、失败和 dispose 合同。
- `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts`：接入 input pump。
- `web/src/hooks/workbenchTerminalBuffer.ts`：bounded replay ring + generation/appendId live delta store。
- `web/src/hooks/workbenchTerminalBuffersContext.ts`：snapshot 与 imperative live subscription context 接口。
- `web/src/hooks/useWorkbenchTerminalBuffers.tsx`：Tauri output/resync 事件写入新 store。
- `web/src/pages/Workbench/terminalLiveWriter.ts`：xterm replay/live 握手和单 in-flight write queue。
- `web/src/pages/Workbench/WorkbenchTerminalPane.tsx`：接入 live writer，移除 live full-buffer effect 热路径。
- `web/src/pages/Workbench/terminalReplay.ts`：保留首次 mount/resync gate；live 路径不再调用 KMP。
- `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.ts`：Prompt 浮层关闭时跳过布局读取。
- `docs/prd.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md`：持久行为与分层合同。

---

### Task 1: Control NDJSON stream 客户端

**Files:**
- Modify: `src-tauri/src/backend/control_client.rs`
- Test: `src-tauri/src/backend/control_client.rs`

**Interfaces:**
- Produces: `ControlEventStreamDecoder::push/finish`、`ControlEventsStream::next_message`、`BackendControlClient::open_events_stream`。
- Consumes: existing `ControlEventsBody`、`BackendRuntimeCursor`、`RuntimeRelayMessage`、`QUERY_TIMEOUT`。

- [ ] **Step 1: 写 bounded NDJSON decoder 失败测试**

在 `backend::control_client::tests` 增加：

```rust
#[test]
fn control_event_decoder_preserves_split_utf8_and_multiple_lines() {
    let mut decoder = ControlEventStreamDecoder::default();
    let first = br#"{"kind":"event","ownerInstanceId":"o1","sequence":1,"event":"workbench:terminal-output","payload":{"chunk":""#;
    assert!(decoder.push(first).unwrap().is_empty());
    assert!(decoder.push(&[0xE4, 0xBD]).unwrap().is_empty());
    let mut tail = vec![0xA0];
    tail.extend_from_slice(
        br#""}}
{"kind":"gap","ownerInstanceId":"o1","oldestAvailable":2,"latest":9}
"#,
    );
    let messages = decoder.push(&tail).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[1], RuntimeRelayMessage::Gap { latest: 9, .. }));
    assert!(decoder.finish().unwrap().is_empty());
}

#[test]
fn control_event_decoder_rejects_oversize_malformed_and_partial_eof() {
    let mut oversize = ControlEventStreamDecoder::default();
    let error = oversize
        .push(&vec![b'x'; CONTROL_EVENT_STREAM_MAX_LINE_BYTES + 1])
        .unwrap_err();
    assert!(error.to_string().contains("control_event_stream_line_too_large"));

    let mut malformed = ControlEventStreamDecoder::default();
    let error = malformed.push(b"{not-json}\n").unwrap_err();
    assert!(error.to_string().contains("control_event_stream_malformed"));

    let mut partial = ControlEventStreamDecoder::default();
    partial.push(br#"{"kind":"event"}"#).unwrap();
    let error = partial.finish().unwrap_err();
    assert!(error.to_string().contains("control_event_stream_truncated"));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked backend::control_client::tests::control_event_decoder -- --nocapture`

Expected: FAIL，`ControlEventStreamDecoder` 和容量常量不存在。

- [ ] **Step 3: 实现 decoder 与 stream reader**

在 `control_client.rs` 增加以下生产接口；错误消息使用稳定 code 前缀，禁止拼接原始 line：

```rust
const CONTROL_EVENT_STREAM_MAX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct ControlEventStreamDecoder {
    pending: Vec<u8>,
}

impl ControlEventStreamDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeRelayMessage>, AppError> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        let mut line_start = 0usize;
        for newline in 0..self.pending.len() {
            if self.pending[newline] != b'\n' {
                continue;
            }
            if newline - line_start > CONTROL_EVENT_STREAM_MAX_LINE_BYTES {
                self.pending.clear();
                return Err(AppError::validation("control_event_stream_line_too_large"));
            }
            let mut line = &self.pending[line_start..newline];
            line_start = newline + 1;
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.is_empty() {
                continue;
            }
            let message = serde_json::from_slice::<RuntimeRelayMessage>(line)
                .map_err(|_| AppError::generic("control_event_stream_malformed"))?;
            messages.push(message);
        }
        if line_start > 0 {
            self.pending.drain(..line_start);
        }
        if self.pending.len() > CONTROL_EVENT_STREAM_MAX_LINE_BYTES {
            self.pending.clear();
            return Err(AppError::validation("control_event_stream_line_too_large"));
        }
        Ok(messages)
    }

    fn finish(&mut self) -> Result<Vec<RuntimeRelayMessage>, AppError> {
        if self.pending.iter().all(|byte| byte.is_ascii_whitespace()) {
            self.pending.clear();
            return Ok(Vec::new());
        }
        self.pending.clear();
        Err(AppError::generic("control_event_stream_truncated"))
    }
}

pub struct ControlEventsStream {
    response: reqwest::Response,
    decoder: ControlEventStreamDecoder,
    ready: std::collections::VecDeque<RuntimeRelayMessage>,
    ended: bool,
}

impl ControlEventsStream {
    pub async fn next_message(&mut self) -> Result<Option<RuntimeRelayMessage>, AppError> {
        loop {
            if let Some(message) = self.ready.pop_front() {
                return Ok(Some(message));
            }
            if self.ended {
                return Ok(None);
            }
            match self.response.chunk().await {
                Ok(Some(bytes)) => self.ready.extend(self.decoder.push(&bytes)?),
                Ok(None) => {
                    self.ready.extend(self.decoder.finish()?);
                    self.ended = true;
                }
                Err(_) => return Err(AppError::unavailable("control_event_stream_network")),
            }
        }
    }
}
```

- [ ] **Step 4: 写 stream connect 失败测试**

复用 `control_client.rs` 现有 axum mock server helper，新增测试：

```rust
#[tokio::test(start_paused = true)]
async fn events_stream_sends_cursor_and_reads_live_message_without_overall_timeout() {
    let (port, captured, _server) = spawn_control_event_stream_fixture().await;
    let client = BackendControlClient::for_test(port, "token", "owner-1").unwrap();
    let cursor = BackendRuntimeCursor {
        owner_instance_id: "owner-1".into(),
        sequence: 7,
    };
    let mut stream = client.open_events_stream(Some(&cursor)).await.unwrap();
    let next = tokio::spawn(async move { stream.next_message().await });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(15)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    let message = next.await.unwrap().unwrap().unwrap();
    assert!(matches!(message, RuntimeRelayMessage::Event { sequence: 8, .. }));
    let body = captured.lock().unwrap().clone().unwrap();
    assert_eq!(body["afterOwnerInstanceId"], "owner-1");
    assert_eq!(body["afterSequence"], 7);
}
```

fixture 先立即返回响应头，再用 Tokio timer 延迟 16s 发送第一条 NDJSON；paused-time 测试先推进到旧 15s timeout，再推进到第 16s，证明 client 不会因旧 builder overall timeout 终止 body，且不会增加真实测试耗时。

- [ ] **Step 5: 实现 `open_events_stream` 并移除 client 全局 timeout**

```rust
pub async fn open_events_stream(
    &self,
    after: Option<&BackendRuntimeCursor>,
) -> Result<ControlEventsStream, AppError> {
    let url = format!(
        "http://127.0.0.1:{}/api/backend/control/events/stream",
        self.port
    );
    let body = ControlEventsBody {
        control_token: self.control_token.clone(),
        after_owner_instance_id: after.map(|cursor| cursor.owner_instance_id.clone()),
        after_sequence: after.map(|cursor| cursor.sequence),
    };
    let send = self.http.post(url).json(&body).send();
    let response = tokio::time::timeout(QUERY_TIMEOUT, send)
        .await
        .map_err(|_| AppError::timeout("control_event_stream_connect_timeout"))?
        .map_err(|_| AppError::unavailable("control_event_stream_connect_failed"))?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(if status == reqwest::StatusCode::NOT_FOUND {
            AppError::validation("control_event_stream_unsupported")
        } else {
            AppError::unavailable(format!("control_event_stream_http_{status}"))
        });
    }
    Ok(ControlEventsStream {
        response,
        decoder: ControlEventStreamDecoder::default(),
        ready: std::collections::VecDeque::new(),
        ended: false,
    })
}
```

把 `BackendControlClient::from_control` 和 `for_test` 的 builder 改为 `reqwest::Client::builder().build()`；确认所有 `send_once` 调用仍通过 `RequestBuilder::timeout(timeout)` 设置逐请求 timeout。

- [ ] **Step 6: 运行格式化与定向测试**

Run: `cd src-tauri && cargo fmt --check && cargo test --locked backend::control_client::tests -- --nocapture`

Expected: PASS；stream decoder/client 测试通过，现有 control mutation/query 测试不变。

- [ ] **Step 7: 提交**

```bash
git add src-tauri/src/backend/control_client.rs
git commit -m "feat: add live control event stream client"
```

---

### Task 2: GUI control client runtime cache

**Files:**
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/commands/workbench/common.rs`
- Test: `src-tauri/src/backend/control_client.rs`

**Interfaces:**
- Consumes: Task 1 `BackendControlClient`。
- Produces: `BackendControlClientRuntime::{client,invalidate_if_current,workbench_op,workbench_mutation_op_value}`、`AppState.backend_control_client_runtime`。

- [ ] **Step 1: 写 cache 复用与不重放失败测试**

为 runtime 注入 descriptor loader，避免测试真实 `~/.cc-partner`：

```rust
#[test]
fn control_runtime_reuses_client_until_explicit_invalidation() {
    let loads = Arc::new(AtomicUsize::new(0));
    let runtime = BackendControlClientRuntime::with_loader({
        let loads = Arc::clone(&loads);
        move || {
            loads.fetch_add(1, Ordering::SeqCst);
            BackendControlClient::for_test(62116, "token", "owner-1")
        }
    });
    let first = runtime.client().unwrap();
    let second = runtime.client().unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    runtime.invalidate_if_current(&first);
    let third = runtime.client().unwrap();
    assert_eq!(loads.load(Ordering::SeqCst), 2);
    assert!(first.same_descriptor(&second));
    assert!(second.same_descriptor(&third));
}

#[tokio::test]
async fn workbench_mutation_failure_invalidates_but_never_replays() {
    let (port, calls, _server) = spawn_failing_control_workbench_fixture().await;
    let runtime = BackendControlClientRuntime::with_loader(move || {
        BackendControlClient::for_test(port, "token", "owner-1")
    });
    let result = runtime
        .workbench_mutation_op_value(
            "sessions.write",
            serde_json::json!({"sessionId":"s1","data":"x"}),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked backend::control_client::tests::control_runtime -- --nocapture`

Expected: FAIL，runtime 类型不存在。

- [ ] **Step 3: 实现 runtime cache**

```rust
type ControlClientLoader = dyn Fn() -> Result<BackendControlClient, AppError> + Send + Sync;

pub struct BackendControlClientRuntime {
    cached: std::sync::Mutex<Option<BackendControlClient>>,
    loader: Arc<ControlClientLoader>,
}

impl BackendControlClientRuntime {
    pub fn new() -> Self {
        Self::with_loader(BackendControlClient::from_control_file)
    }

    fn with_loader(
        loader: impl Fn() -> Result<BackendControlClient, AppError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            cached: std::sync::Mutex::new(None),
            loader: Arc::new(loader),
        }
    }

    pub fn client(&self) -> Result<BackendControlClient, AppError> {
        let mut cached = self.cached.lock().expect("control client cache 锁中毒");
        if let Some(client) = cached.as_ref() {
            return Ok(client.clone());
        }
        let client = (self.loader)()?;
        *cached = Some(client.clone());
        Ok(client)
    }

    pub fn invalidate_if_current(&self, observed: &BackendControlClient) {
        let mut cached = self.cached.lock().expect("control client cache 锁中毒");
        if cached
            .as_ref()
            .is_some_and(|current| current.same_descriptor(observed))
        {
            *cached = None;
        }
    }

    pub async fn workbench_op<T: DeserializeOwned>(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<T, AppError> {
        let client = self.client()?;
        let result = client.workbench_op(op, payload).await;
        if result.is_err() {
            self.invalidate_if_current(&client);
        }
        result
    }

    pub async fn workbench_mutation_op_value(
        &self,
        op: &str,
        payload: impl Serialize,
    ) -> Result<serde_json::Value, MutationControlError> {
        let client = self.client().map_err(MutationControlError::Failed)?;
        let result = client.workbench_mutation_op_value(op, payload).await;
        if result.is_err() {
            self.invalidate_if_current(&client);
        }
        result
    }
}
```

`BackendControlClient::same_descriptor` 比较 port、token、owner id 和 schema，但不得打印这些字段。

- [ ] **Step 4: 接入 AppState 和 runtime builder**

在 `AppState` 增加：

```rust
pub backend_control_client_runtime: Arc<BackendControlClientRuntime>,
```

在 `build_app_state_with_role` 统一构造：

```rust
backend_control_client_runtime: Arc::new(BackendControlClientRuntime::new()),
```

HeadlessOwner 可持有但不使用该字段，避免双构造路径。

- [ ] **Step 5: 把 Workbench GUI proxy 切到 cache**

修改 `proxy_workbench_if_gui`：

```rust
if state.runtime_role != RuntimeRole::GuiClient {
    return Ok(None);
}
Ok(Some(
    state
        .backend_control_client_runtime
        .workbench_op(op, payload)
        .await?,
))
```

`proxy_workbench_mutation_if_gui` 改调 `state.backend_control_client_runtime.workbench_mutation_op_value(...)`，继续把 `Uncertain` 映射成 existing unknown envelope、把 `Failed` 返回错误；runtime 只让下一个业务调用重新加载 descriptor，不得在 cache miss/refetch 后重发当前 mutation。

- [ ] **Step 6: 运行定向回归**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked backend::control_client::tests -- --nocapture
cargo test --locked backend::control_workbench::tests -- --nocapture
```

Expected: PASS；失败 fixture 的 `sessions.write` 调用数恰好 1。

- [ ] **Step 7: 提交**

```bash
git add src-tauri/src/backend/control_client.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/src/commands/workbench/common.rs
git commit -m "perf: reuse desktop control client"
```

---

### Task 3: Stream-first GUI owner relay

**Files:**
- Modify: `src-tauri/src/backend/ui.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: `src-tauri/src/backend/ui.rs`
- Test: `src-tauri/tests/runtime_authority_smoke.rs`

**Interfaces:**
- Consumes: Task 1 `ControlEventsStream`、Task 2 `BackendControlClientRuntime`。
- Produces: `run_gui_owner_event_relay(ui, client_runtime, cancel)`、stream reconnect/fallback phases、`AppState.gui_event_relay_cancel`。

- [ ] **Step 1: 提取单消息应用函数并写失败测试**

```rust
#[tokio::test]
async fn live_relay_delivers_once_and_gap_resync_attaches_latest() {
    let ui = RecordingBackendUi::default();
    let client = fake_resync_client_with_terminal_replay("s1", "prompt$ ");
    let mut state = GuiEventRelayState::default();
    apply_gui_relay_message(
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
    assert_eq!(ui.event_count("workbench:terminal-output"), 1);

    apply_gui_relay_message(
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
    assert_eq!(state.cursor().unwrap().sequence, 9);
    assert_eq!(ui.event_count("workbench:terminal-resync"), 1);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked backend::ui::tests::live_relay -- --nocapture`

Expected: FAIL，helper/fake 尚不存在。

- [ ] **Step 3: 实现统一 Deliver/Gap helper**

把现有 enrichment 和 `resync_after_gap` 分支移入：

```rust
async fn apply_gui_relay_message(
    client: &BackendControlClient,
    ui: &dyn BackendUi,
    relay_state: &mut GuiEventRelayState,
    message: RuntimeRelayMessage,
) {
    match relay_state.on_message(message) {
        RelayClientAction::Deliver {
            event,
            payload,
            owner_instance_id,
            sequence,
        } => emit_gui_relay_event(ui, event, payload, owner_instance_id, sequence),
        RelayClientAction::DropDuplicate => {}
        RelayClientAction::RequestResync {
            owner_instance_id,
            oldest_available,
            latest,
        } => {
            let _ = resync_after_gap(
                client,
                ui,
                &owner_instance_id,
                oldest_available,
                latest,
            )
            .await;
            relay_state.attach_at(BackendRuntimeCursor {
                owner_instance_id,
                sequence: latest,
            });
        }
    }
}
```

- [ ] **Step 4: 写 stream reconnect 与 unsupported fallback 测试**

在 `runtime_authority_smoke.rs` 的隔离 control server fixture 中加入：

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gui_relay_uses_live_stream_and_reconnects_from_last_cursor() {
    let fixture = RuntimeAuthorityFixture::start().await;
    let ui = Arc::new(RecordingBackendUi::default());
    let cancel = CancellationToken::new();
    let relay = tokio::spawn(run_gui_owner_event_relay(
        ui.clone(),
        fixture.control_client_runtime(),
        cancel.clone(),
    ));

    fixture.publish_terminal_event("s1", "a");
    ui.wait_for_event("workbench:terminal-output", Duration::from_millis(100))
        .await;
    fixture.break_current_event_stream().await;
    fixture.publish_terminal_event("s1", "b");
    ui.wait_for_terminal_chunks(&["a", "b"], Duration::from_secs(1))
        .await;

    assert_eq!(fixture.stream_open_count(), 2);
    assert_eq!(fixture.second_stream_after_sequence(), Some(1));
    cancel.cancel();
    relay.await.unwrap();
}
```

另加 unsupported fixture，断言 404 后进入 catch-up fallback；同版本成功 stream fixture断言 `events/catch-up` 调用数为 0。

- [ ] **Step 5: 实现 stream-first relay**

```rust
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
            run_poll_fallback_once(&client, ui.as_ref(), &mut relay_state).await;
            wait_relay_retry(&cancel, 250).await;
            continue;
        }
        poll_fallback_until = None;

        match client.open_events_stream(relay_state.cursor().as_ref()).await {
            Ok(mut stream) => {
                let mut received_message = false;
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        message = stream.next_message() => match message {
                            Ok(Some(message)) => {
                                received_message = true;
                                apply_gui_relay_message(&client, ui.as_ref(), &mut relay_state, message).await;
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
                tracing::info!(relay_mode = "pollFallback", "GUI owner event stream unsupported");
                poll_fallback_until = Some(
                    tokio::time::Instant::now() + Duration::from_secs(5),
                );
                continue;
            }
            Err(_) => client_runtime.invalidate_if_current(&client),
        }
        let delay = reconnect_delays[attempt.min(reconnect_delays.len() - 1)];
        attempt = attempt.saturating_add(1);
        wait_relay_retry(&cancel, delay).await;
    }
}
```

`run_poll_fallback_once` 只执行一次 catch-up，不包含 sleep；外层以 250ms cancel-aware wait 维持旧 sidecar 功能。404 后 5s 内不重复探测不支持的 endpoint，窗口到期再试 stream；任何 catch-up transport 失败仍使 client cache 失效。不得永久锁死在 fallback，也不得每 250ms 制造一次 404。

- [ ] **Step 6: 保存并关闭 relay cancel token**

在 `AppState` 增加：

```rust
pub gui_event_relay_cancel: Arc<Mutex<Option<CancellationToken>>>,
```

runtime builder 初始化 `None`。`lib.rs` setup：

```rust
let cancel = CancellationToken::new();
*state.gui_event_relay_cancel.lock().unwrap() = Some(cancel.clone());
let ui = Arc::clone(&state.ui);
let clients = Arc::clone(&state.backend_control_client_runtime);
tauri::async_runtime::spawn(run_gui_owner_event_relay(ui, clients, cancel));
```

`shutdown_backend_runtime` 调用现有 `cancel_runtime_token(&state.gui_event_relay_cancel, "GUI owner event relay")`。

- [ ] **Step 7: 运行 Rust relay 合同**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked backend::ui::tests -- --nocapture
cargo test --locked backend::event_bus::tests -- --nocapture
cargo test --locked --test runtime_authority_smoke -- --nocapture --test-threads=1
```

Expected: PASS；成功 stream 场景在 100ms 内交付，stream 正常期间 catch-up 调用数为 0。

- [ ] **Step 8: 提交**

```bash
git add src-tauri/src/backend/ui.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/src/lib.rs src-tauri/tests/runtime_authority_smoke.rs
git commit -m "fix: stream owner events to desktop"
```

---

### Task 4: 每 session 有序输入泵

**Files:**
- Create: `web/src/pages/Workbench/terminalInputPump.ts`
- Create: `web/src/pages/Workbench/terminalInputPump.test.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx`

**Interfaces:**
- Produces: `createTerminalInputPump(options): TerminalInputPump`。
- Consumes: existing `workbenchApi.sessions.writeInput(sessionId, data)`。

- [ ] **Step 1: 写 FIFO、leading-edge 与不重放失败测试**

```ts
import { describe, expect, test, vi } from 'vitest';
import { createTerminalInputPump } from './terminalInputPump';

function deferred(): { promise: Promise<void>; resolve: () => void; reject: (error: Error) => void } {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

describe('terminalInputPump', () => {
  test('sends the leading batch immediately and coalesces only while in flight', async () => {
    const first = deferred();
    const writes: Array<[string, string]> = [];
    const write = vi.fn((sessionId: string, data: string) => {
      writes.push([sessionId, data]);
      return writes.length === 1 ? first.promise : Promise.resolve();
    });
    const pump = createTerminalInputPump({ write });

    pump.enqueue('s1', 'a');
    expect(writes).toEqual([['s1', 'a']]);
    pump.enqueue('s1', 'b');
    pump.enqueue('s1', '\u007f');
    expect(writes).toHaveLength(1);
    first.resolve();
    await pump.whenIdle('s1');
    expect(writes).toEqual([['s1', 'a'], ['s1', 'b\u007f']]);
  });

  test('isolates sessions and never replays a failed batch', async () => {
    const calls: Array<[string, string]> = [];
    const write = vi.fn(async (sessionId: string, data: string) => {
      calls.push([sessionId, data]);
      if (data === 'a') throw new Error('uncertain');
    });
    const pump = createTerminalInputPump({ write });
    pump.enqueue('s1', 'a');
    pump.enqueue('s1', 'b');
    pump.enqueue('s2', 'x');
    await Promise.all([pump.whenIdle('s1'), pump.whenIdle('s2')]);
    expect(calls.filter(([id]) => id === 's1')).toEqual([['s1', 'a'], ['s1', 'b']]);
    expect(calls.filter(([, data]) => data === 'a')).toHaveLength(1);
  });

  test('dispose drops pending bytes without cancelling or replaying the in-flight write', async () => {
    const first = deferred();
    const write = vi.fn(() => first.promise);
    const pump = createTerminalInputPump({ write });
    pump.enqueue('s1', 'a');
    pump.enqueue('s1', 'b');
    pump.disposeSession('s1');
    first.resolve();
    await Promise.resolve();
    expect(write).toHaveBeenCalledTimes(1);
  });
});
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd web && npm test -- src/pages/Workbench/terminalInputPump.test.ts`

Expected: FAIL，模块不存在。

- [ ] **Step 3: 实现 pump**

```ts
export interface TerminalInputPumpOptions {
  write: (sessionId: string, data: string) => Promise<unknown>;
}

export interface TerminalInputPump {
  enqueue: (sessionId: string, data: string) => void;
  disposeSession: (sessionId: string) => void;
  dispose: () => void;
  whenIdle: (sessionId: string) => Promise<void>;
}

interface InputLane {
  generation: number;
  pending: string;
  running: boolean;
  idleWaiters: Set<() => void>;
}

export function createTerminalInputPump(options: TerminalInputPumpOptions): TerminalInputPump {
  const lanes = new Map<string, InputLane>();
  let disposed = false;

  const settleIdle = (lane: InputLane): void => {
    if (lane.running || lane.pending.length > 0) return;
    lane.idleWaiters.forEach((resolve) => resolve());
    lane.idleWaiters.clear();
  };

  const drain = async (sessionId: string, lane: InputLane, generation: number): Promise<void> => {
    lane.running = true;
    while (!disposed && lane.generation === generation && lane.pending.length > 0) {
      const batch = lane.pending;
      lane.pending = '';
      try {
        await options.write(sessionId, batch);
      } catch {
        // Mutation 不重放；后续已排队批次仍按原顺序继续。
      }
    }
    if (lane.generation === generation) {
      lane.running = false;
      settleIdle(lane);
      if (lane.pending.length === 0 && lane.idleWaiters.size === 0) lanes.delete(sessionId);
    }
  };

  const disposeSession = (sessionId: string): void => {
    const lane = lanes.get(sessionId);
    if (!lane) return;
    lane.generation += 1;
    lane.pending = '';
    lane.running = false;
    settleIdle(lane);
    lanes.delete(sessionId);
  };

  return {
    enqueue(sessionId, data) {
      if (disposed || data.length === 0) return;
      const lane = lanes.get(sessionId) ?? {
        generation: 0,
        pending: '',
        running: false,
        idleWaiters: new Set<() => void>(),
      };
      lanes.set(sessionId, lane);
      lane.pending += data;
      if (!lane.running) void drain(sessionId, lane, lane.generation);
    },
    disposeSession,
    dispose() {
      disposed = true;
      for (const sessionId of [...lanes.keys()]) disposeSession(sessionId);
    },
    whenIdle(sessionId) {
      const lane = lanes.get(sessionId);
      if (!lane || (!lane.running && lane.pending.length === 0)) return Promise.resolve();
      return new Promise<void>((resolve) => lane.idleWaiters.add(resolve));
    },
  };
}
```

- [ ] **Step 4: 接入 controller**

在 controller 所有 early return 前创建稳定 pump：

```ts
const terminalInputPumpRef = useRef<TerminalInputPump | null>(null);
if (terminalInputPumpRef.current === null) {
  terminalInputPumpRef.current = createTerminalInputPump({
    write: (sessionId, data) => workbenchApi.sessions.writeInput(sessionId, data),
  });
}

useEffect(() => {
  const pump = terminalInputPumpRef.current;
  return () => pump?.dispose();
}, []);

const handleInput = useCallback(
  (sessionId: string, data: string): void => {
    if (remoteWriteDisabled) return;
    terminalInputPumpRef.current?.enqueue(sessionId, data);
  },
  [remoteWriteDisabled],
);
```

关闭 session 成功、remote offline transition 或项目切换清理旧 session 时调用 `disposeSession(sessionId)`；不得清理其它仍活动 session。

- [ ] **Step 5: 扩展 controller 测试**

在现有 harness 中触发同一 xterm callback 连续输入 `a`、`b`、Backspace，第一 invoke defer；断言第二个 invoke 只在第一 settle 后出现且 `data === 'b\u007f'`。另断言 remoteWriteDisabled 不入队，close 后 pending 不发送。

- [ ] **Step 6: 运行前端定向测试和 lint**

```bash
cd web
npm test -- src/pages/Workbench/terminalInputPump.test.ts src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx
npm run lint
```

Expected: PASS；无 Hook 顺序错误，逐键测试证明每 session 最大 in-flight=1。

- [ ] **Step 7: 提交**

```bash
git add web/src/pages/Workbench/terminalInputPump.ts web/src/pages/Workbench/terminalInputPump.test.ts web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts web/src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx
git commit -m "perf: serialize workbench terminal input"
```

---

### Task 5: 后端 replay chunk ring

**Files:**
- Modify: `src-tauri/src/workbench/sessions.rs`
- Test: `src-tauri/src/workbench/sessions.rs`

**Interfaces:**
- Produces: internal `ReplayChunk`、amortized incremental `SessionReplayBuffer::append`、unchanged `WorkbenchSessionReplayDto`。
- Consumes: existing `SESSION_REPLAY_MAX_CHARS`、`last_seq`、`truncated` semantics。

- [ ] **Step 1: 写 Unicode、零容量与满容量增量失败测试**

```rust
#[test]
fn replay_chunk_ring_preserves_unicode_and_tail_contract() {
    let mut buffer = SessionReplayBuffer::new(4);
    buffer.append("你🙂", 1);
    buffer.append("ab", 2);
    buffer.append("c", 3);
    let snapshot = buffer.snapshot("s1");
    assert_eq!(snapshot.buffer, "🙂abc");
    assert!(snapshot.truncated);
    assert_eq!(snapshot.last_seq, 3);
    assert_eq!(buffer.char_count, 4);
}

#[test]
fn replay_chunk_ring_handles_zero_and_large_single_chunk() {
    let mut zero = SessionReplayBuffer::new(0);
    zero.append("secret", 1);
    assert_eq!(zero.snapshot("s0").buffer, "");
    assert!(zero.snapshot("s0").truncated);

    let mut small = SessionReplayBuffer::new(3);
    small.append("abcdef", 4);
    assert_eq!(small.snapshot("s1").buffer, "def");
    assert_eq!(small.chunks.len(), 1);
}

#[test]
fn full_replay_ring_drops_one_small_head_chunk_per_small_append() {
    let mut buffer = SessionReplayBuffer::new(8);
    for (seq, value) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        buffer.append(value, seq as u64 + 1);
    }
    buffer.append("i", 9);
    assert_eq!(buffer.snapshot("s").buffer, "bcdefghi");
    assert_eq!(buffer.char_count, 8);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked workbench::sessions::tests::replay_chunk_ring -- --nocapture`

Expected: FAIL，现有 String buffer 没有 chunks/char_count。

- [ ] **Step 3: 实现 `VecDeque<ReplayChunk>`**

```rust
#[derive(Debug, Clone)]
struct ReplayChunk {
    text: String,
    char_count: usize,
}

#[derive(Debug, Clone)]
struct SessionReplayBuffer {
    max_chars: usize,
    chunks: VecDeque<ReplayChunk>,
    char_count: usize,
    byte_count: usize,
    truncated: bool,
    last_seq: u64,
}

impl SessionReplayBuffer {
    fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            chunks: VecDeque::new(),
            char_count: 0,
            byte_count: 0,
            truncated: false,
            last_seq: 0,
        }
    }

    fn append(&mut self, chunk: &str, seq: u64) {
        let char_count = chunk.chars().count();
        if !chunk.is_empty() {
            self.byte_count += chunk.len();
            self.char_count += char_count;
            self.chunks.push_back(ReplayChunk {
                text: chunk.to_string(),
                char_count,
            });
        }
        self.last_seq = seq;
        self.trim_to_limit();
    }

    fn trim_to_limit(&mut self) {
        if self.char_count <= self.max_chars {
            return;
        }
        self.truncated = true;
        let mut overflow = self.char_count - self.max_chars;
        while overflow > 0 {
            let Some(front) = self.chunks.pop_front() else { break };
            self.byte_count -= front.text.len();
            self.char_count -= front.char_count;
            if overflow >= front.char_count {
                overflow -= front.char_count;
                continue;
            }
            let byte_offset = front
                .text
                .char_indices()
                .nth(overflow)
                .map(|(index, _)| index)
                .unwrap_or(front.text.len());
            let text = front.text[byte_offset..].to_string();
            let kept_chars = front.char_count - overflow;
            self.byte_count += text.len();
            self.char_count += kept_chars;
            self.chunks.push_front(ReplayChunk {
                text,
                char_count: kept_chars,
            });
            overflow = 0;
        }
    }

    fn snapshot(&self, session_id: &str) -> WorkbenchSessionReplayDto {
        let mut buffer = String::with_capacity(self.byte_count);
        for chunk in &self.chunks {
            buffer.push_str(&chunk.text);
        }
        WorkbenchSessionReplayDto {
            session_id: session_id.to_string(),
            buffer,
            truncated: self.truncated,
            last_seq: self.last_seq,
        }
    }
}
```

导入 `std::collections::VecDeque`；删除旧 `buffer.chars().count()` 和 `Vec<char>` rebuild。

- [ ] **Step 4: 增加大容量非严格 wall-clock 回归**

使用结构断言而不是脆弱的毫秒阈值：填满 120,000 个单字符 chunk 后继续 append 10,000 次，断言 `char_count` 恒定、`chunks.len()` 不增长、snapshot 尾部正确。性能门槛留给 Task 8 release/L2 证据。

- [ ] **Step 5: 运行 sessions 与 PTY 测试**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked workbench::sessions::tests -- --nocapture
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
```

Expected: PASS；replay DTO shape、UTF-8 和 native PTY echo 不变。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/workbench/sessions.rs
git commit -m "perf: make terminal replay append incremental"
```

---

### Task 6: 前端 live delta store 与 xterm writer

**Files:**
- Modify: `web/src/hooks/workbenchTerminalBuffer.ts`
- Modify: `web/src/hooks/workbenchTerminalBuffer.test.ts`
- Modify: `web/src/hooks/workbenchTerminalBuffersContext.ts`
- Modify: `web/src/hooks/useWorkbenchTerminalBuffers.tsx`
- Create: `web/src/pages/Workbench/terminalLiveWriter.ts`
- Create: `web/src/pages/Workbench/terminalLiveWriter.test.ts`
- Modify: `web/src/pages/Workbench/WorkbenchTerminalPane.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchTerminalPane.test.tsx`
- Modify: `web/src/pages/Workbench/terminalReplay.ts`
- Modify: `web/src/pages/Workbench/terminalReplay.test.ts`

**Interfaces:**
- Produces: `TerminalBufferCursor`、`TerminalBufferSnapshot`、`TerminalBufferDelta`、`store.getSnapshot`、`store.subscribeLive`、`createTerminalLiveWriter`。
- Consumes: existing 200,000-unit bounded buffer、Task 3 Tauri `terminal-output/resync` events、existing replay gate。

- [ ] **Step 1: 写 store handshake 和 no-materialize 失败测试**

```ts
test('subscribe-before-snapshot dedupes queued deltas by generation and appendId', () => {
  const scheduler = createManualFrameScheduler();
  const store = createWorkbenchTerminalBufferStore({ frameScheduler: scheduler.scheduler });
  const deltas: TerminalBufferDelta[] = [];
  const unsubscribe = store.subscribeLive('s1', (delta) => deltas.push(delta));
  store.append('s1', 'a');
  const snapshot = store.getSnapshot('s1');
  store.append('s1', 'b');

  expect(snapshot.buffer).toBe('a');
  expect(snapshot.cursor).toEqual({ generation: 0, appendId: 1 });
  expect(deltas.map((delta) => delta.chunk)).toEqual(['a', 'b']);
  expect(
    deltas.filter(
      (delta) =>
        delta.generation > snapshot.cursor.generation ||
        delta.appendId > snapshot.cursor.appendId,
    ),
  ).toHaveLength(1);
  unsubscribe();
});

test('live append never materializes the replay snapshot', () => {
  let materializeCalls = 0;
  const store = createWorkbenchTerminalBufferStore({
    onMaterializeForTest: () => {
      materializeCalls += 1;
    },
  });
  store.subscribeLive('s1', () => undefined);
  for (let index = 0; index < 1_000; index += 1) store.append('s1', 'x');
  expect(materializeCalls).toBe(0);
  expect(store.getSnapshot('s1').buffer.length).toBe(1_000);
  expect(materializeCalls).toBe(1);
});

test('full replay ring advances a head index instead of shifting every append', () => {
  let compactions = 0;
  const store = createWorkbenchTerminalBufferStore({
    maxChars: 8,
    onCompactForTest: () => {
      compactions += 1;
    },
  });
  for (let index = 0; index < 10_000; index += 1) store.append('s1', 'x');
  expect(store.getSnapshot('s1').buffer).toBe('xxxxxxxx');
  expect(compactions).toBeLessThan(12);
});

test('reset starts a new generation and remove invalidates old deltas', () => {
  const store = createWorkbenchTerminalBufferStore();
  const deltas: TerminalBufferDelta[] = [];
  store.subscribeLive('s1', (delta) => deltas.push(delta));
  store.append('s1', 'old');
  store.reset('s1', 'new');
  store.append('s1', '!');
  expect(store.getSnapshot('s1').buffer).toBe('new!');
  expect(deltas.at(-1)?.generation).toBe(1);
});
```

- [ ] **Step 2: 运行 store 测试确认失败**

Run: `cd web && npm test -- src/hooks/workbenchTerminalBuffer.test.ts`

Expected: FAIL，新接口不存在。

- [ ] **Step 3: 扩展 store 数据模型**

```ts
export interface TerminalBufferCursor {
  generation: number;
  appendId: number;
}

export interface TerminalBufferSnapshot {
  buffer: string;
  cursor: TerminalBufferCursor;
  revision: number;
}

export interface TerminalBufferDelta extends TerminalBufferCursor {
  sessionId: string;
  chunk: string;
}

export interface WorkbenchTerminalBufferStore {
  getSnapshot: (sessionId: string | null) => TerminalBufferSnapshot;
  getRevision: (sessionId: string | null) => number;
  subscribe: (sessionId: string | null, listener: () => void) => () => void;
  subscribeLive: (
    sessionId: string | null,
    listener: (delta: TerminalBufferDelta) => void,
  ) => () => void;
  subscribeReset: (sessionId: string, listener: () => void) => () => void;
  append: (sessionId: string, chunk: string) => void;
  reset: (sessionId: string, buffer?: string) => void;
  remove: (sessionId: string) => void;
}
```

`TerminalBufferStoreOptions` 增加仅测试使用的可选 `onMaterializeForTest?: () => void` 与 `onCompactForTest?: () => void`；前者由真正 join/slice 触发，后者由废弃 chunk 前缀物理 compact 时触发，生产调用不传测试 seam。每个 `SessionRingBuffer` 增加 `headIndex`、`generation`、`appendId`；trim 完整头 chunk 时只递增 `headIndex`，禁止调用 `Array.shift()`。当 `headIndex >= 1_024` 且废弃前缀至少占数组一半时才执行一次 `chunks = chunks.slice(headIndex)` 并归零 headIndex，使满容量小 chunk append 保持摊销 O(1)；materialize 从 `headIndex` 开始读取，并仅在活动首 chunk 应用 `startOffset`。另建 `liveListenersBySession` 与 `resetListenersBySession`。`append` 顺序固定为：push/trim → `appendId += 1` → 同步发布 delta → schedule React revision。`reset(sessionId, buffer='')`：cancel frame → generation+1 → appendId=0 → 用单 chunk seed buffer → 同步 notify reset listeners → immediate revision notify；reset 本身不伪造 live delta。`remove` 同样先递增 generation 并同步 notify reset listeners，再删除 session，保证旧 writer queue 失效。

保留兼容 `getBuffer` 时只让它委托 `getSnapshot().buffer`，并在 Task 6 结束后确认生产 live 路径无 caller。

- [ ] **Step 4: 写 xterm live writer 失败测试**

```ts
test('replays snapshot once then drains only newer deltas in exact order', async () => {
  const terminal = new FakeTerminalWriter();
  const source = new FakeTerminalLiveSource('history', { generation: 0, appendId: 2 });
  const writer = createTerminalLiveWriter({ terminal, source, sessionId: 's1' });
  source.emit({ sessionId: 's1', generation: 0, appendId: 2, chunk: 'duplicate' });
  source.emit({ sessionId: 's1', generation: 0, appendId: 3, chunk: 'a' });
  source.emit({ sessionId: 's1', generation: 0, appendId: 4, chunk: 'b' });
  terminal.completeWrite(0);
  terminal.completeWrite(1);
  expect(terminal.writes).toEqual(['history', 'ab']);
  writer.dispose();
});

test('generation change clears and replays the new snapshot before later deltas', () => {
  const terminal = new FakeTerminalWriter();
  const source = new FakeTerminalLiveSource('old', { generation: 0, appendId: 1 });
  createTerminalLiveWriter({ terminal, source, sessionId: 's1' });
  terminal.completeWrite(0);
  source.replace('new', { generation: 1, appendId: 0 });
  source.emit({ sessionId: 's1', generation: 1, appendId: 1, chunk: '!' });
  terminal.completeWrite(1);
  terminal.completeWrite(2);
  expect(terminal.clearCalls).toBe(1);
  expect(terminal.writes).toEqual(['old', 'new', '!']);
});
```

- [ ] **Step 5: 实现 `terminalLiveWriter.ts`**

定义窄接口，避免模块依赖 React：

```ts
export interface TerminalLiveWriterTarget {
  clear: () => void;
  write: (data: string, callback?: () => void) => void;
}

export interface TerminalLiveSource {
  getSnapshot: (sessionId: string) => TerminalBufferSnapshot;
  subscribeLive: (
    sessionId: string,
    listener: (delta: TerminalBufferDelta) => void,
  ) => () => void;
  subscribeReset: (sessionId: string, listener: () => void) => () => void;
}

export interface TerminalLiveWriter {
  dispose: () => void;
}
```

实现状态机：先 subscribe live/reset，再 snapshot；replay 时 queue delta；write callback 后过滤 `<= snapshot.cursor` 并合并下一批；generation 变化时 invalidate 当前 queue、`terminal.clear()`、读取/写入最新 snapshot。所有回调检查 disposed token。

- [ ] **Step 6: Provider 和 Context 接入**

`useWorkbenchTerminalBuffers.tsx`：

```ts
store.append(event.payload.sessionId, event.payload.chunk);
```

`workbench:terminal-resync` 改为：

```ts
store.reset(event.payload.sessionId, event.payload.buffer);
```

Context value 继续只暴露稳定 store 引用；新增 `useWorkbenchTerminalBufferStore()` 返回 store，TerminalPane 用它建立 imperative subscription。React `useWorkbenchTerminalBuffer` 继续按 revision 提供 snapshot 给非 xterm caller。`TerminalLiveSource` 直接适配 store 的 `getSnapshot/subscribeLive/subscribeReset`，不得另建第二份事件状态。

- [ ] **Step 7: TerminalPane 接入 live writer**

在 xterm lifecycle effect 中，`terminal.open` 后创建 writer；移除 `[buffer, revision, sessionId]` live effect。首次 mount/replay 和后续 live 均由 writer 管理；现有 `replayGateRef` 交给 writer，确保 replay 产生的 xterm device response 不写回 PTY。

cleanup 顺序固定：dispose writer → data/cursor listener dispose → observer disconnect → terminal.dispose。session identity 不变时不得重建 Terminal。

- [ ] **Step 8: 删除 live KMP 依赖但保留 replay gate**

`planTerminalBufferWrite` 若移动端仍有 caller则暂时保留；桌面 `WorkbenchTerminalPane` 不再调用。增加 ownership test：

```ts
expect(workbenchTerminalPaneSource).not.toContain('planTerminalBufferWrite');
expect(workbenchTerminalPaneSource).toContain('createTerminalLiveWriter');
```

不得删除 `writeTerminalReplay`/`shouldForwardTerminalInput`。

- [ ] **Step 9: 运行前端终端回归**

```bash
cd web
npm test -- src/hooks/workbenchTerminalBuffer.test.ts src/pages/Workbench/terminalLiveWriter.test.ts src/pages/Workbench/WorkbenchTerminalPane.test.tsx src/pages/Workbench/terminalReplay.test.ts
npm run lint
npm run build
```

Expected: PASS；live writer 测试不调用 rAF，200k cap append test 不触发 KMP/materialize。

- [ ] **Step 10: 提交**

```bash
git add web/src/hooks/workbenchTerminalBuffer.ts web/src/hooks/workbenchTerminalBuffer.test.ts web/src/hooks/workbenchTerminalBuffersContext.ts web/src/hooks/useWorkbenchTerminalBuffers.tsx web/src/pages/Workbench/terminalLiveWriter.ts web/src/pages/Workbench/terminalLiveWriter.test.ts web/src/pages/Workbench/WorkbenchTerminalPane.tsx web/src/pages/Workbench/WorkbenchTerminalPane.test.tsx web/src/pages/Workbench/terminalReplay.ts web/src/pages/Workbench/terminalReplay.test.ts
git commit -m "perf: stream terminal deltas directly to xterm"
```

---

### Task 7: 光标布局热路径

**Files:**
- Modify: `web/src/pages/Workbench/WorkbenchTerminalPane.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchTerminalPane.test.tsx`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.test.tsx`

**Interfaces:**
- Consumes: existing `TerminalCursorAnchor`、ResizeObserver、Prompt panel state。
- Produces: callback/浮层关闭时零同步布局读取的合同。

- [ ] **Step 1: 写零布局读取失败测试**

```ts
test('cursor move does not measure viewport when no anchor callback is registered', () => {
  renderPane({ onCursorAnchorChange: undefined });
  const viewport = screen.getByTestId('terminal-pane').firstElementChild as HTMLDivElement;
  const rect = vi.spyOn(viewport, 'getBoundingClientRect');
  latestTerminal().emitCursorMove();
  expect(rect).not.toHaveBeenCalled();
});

test('closed prompt panel does not measure terminal area on cursor movement', () => {
  const { result } = renderPromptOptimizerController({ promptPanelOpen: false });
  const area = result.current.terminalAreaRef.current!;
  const rect = vi.spyOn(area, 'getBoundingClientRect');
  act(() => result.current.handleCursorAnchorChange({ left: 1, top: 2, bottom: 3 }));
  expect(rect).not.toHaveBeenCalled();
});
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd web
npm test -- src/pages/Workbench/WorkbenchTerminalPane.test.tsx src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.test.tsx
```

Expected: FAIL，当前两条路径都会先 `getBoundingClientRect()`。

- [ ] **Step 3: 实现 early gate 与缓存度量**

TerminalPane：

```ts
interface TerminalCursorMetrics {
  left: number;
  top: number;
  cellWidth: number;
  cellHeight: number;
}

const cursorMetricsRef = useRef<TerminalCursorMetrics | null>(null);

const measureTerminalCursorMetrics = (
  viewport: HTMLDivElement,
  terminal: Terminal,
): TerminalCursorMetrics => {
  const rect = viewport.getBoundingClientRect();
  return {
    left: rect.left,
    top: rect.top,
    cellWidth: rect.width / Math.max(terminal.cols, 1),
    cellHeight: rect.height / Math.max(terminal.rows, 1),
  };
};

const cursorAnchorFromMetrics = (
  metrics: TerminalCursorMetrics,
  cursorX: number,
  cursorY: number,
): TerminalCursorAnchor => {
  const left = metrics.left + cursorX * metrics.cellWidth;
  const top = metrics.top + cursorY * metrics.cellHeight;
  return { left, top, bottom: top + metrics.cellHeight };
};

const emitCursorAnchor = (): void => {
  const callback = cursorAnchorCallbackRef.current;
  if (!callback) return;
  try {
    const metrics = cursorMetricsRef.current ?? measureTerminalCursorMetrics(viewport, terminal);
    cursorMetricsRef.current = metrics;
    callback(cursorAnchorFromMetrics(metrics, terminal.buffer.active.cursorX, terminal.buffer.active.cursorY));
  } catch {
    // 定位失败不影响终端输入输出。
  }
};
```

上述三个新增函数/类型放在现有组件边界内最小可复用位置，并补齐项目要求的中文 docstring。在 `resize()` 的 `fit.fit()` 之后、theme/font 变化后先把 `cursorMetricsRef.current = null`，只有 callback 存在时才由 `emitCursorAnchor` 重算。Prompt controller：

```ts
cursorAnchorRef.current = anchor;
if (!promptPanelOpenRef.current || !anchor) return;
const area = terminalAreaRef.current;
if (!area) return;
const nextPosition = promptOptimizerPanelPosition(area.getBoundingClientRect(), anchor);
```

- [ ] **Step 4: 运行定向测试**

```bash
cd web
npm test -- src/pages/Workbench/WorkbenchTerminalPane.test.tsx src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.test.tsx
npm run lint
```

Expected: PASS；callback 存在且 Prompt 打开时定位测试仍通过。

- [ ] **Step 5: 提交**

```bash
git add web/src/pages/Workbench/WorkbenchTerminalPane.tsx web/src/pages/Workbench/WorkbenchTerminalPane.test.tsx web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.ts web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.test.tsx
git commit -m "perf: avoid terminal cursor layout reads"
```

---

### Task 8: 端到端性能合同、文档与最终验证

**Files:**
- Modify: `src-tauri/tests/runtime_authority_smoke.rs`
- Modify: `web/src/pages/Workbench/WorkbenchTerminal.characterization.test.tsx`
- Modify: `web/tests/workbench.spec.ts`
- Modify: `docs/prd.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `docs/development/backend-operations.md`

**Interfaces:**
- Consumes: Tasks 1–7 全部接口。
- Produces: 自动顺序/恢复证据、release GUI 手动证据步骤、持久产品和开发合同。

- [ ] **Step 1: 增加真实 control stream 顺序与 Gap L2**

在 `runtime_authority_smoke.rs` 使用隔离 `CC_PARTNER_DATA_DIR` 和固定非敏感输入：

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_stream_preserves_order_across_reconnect_and_gap() {
    let fixture = RuntimeAuthorityFixture::start_with_native_pty().await;
    let session = fixture.create_terminal(120, 32).await;
    let mut stream = fixture.control_client().open_events_stream(None).await.unwrap();
    for data in ["a", "b", "\u{7f}", "left\u{1b}[D", "paste-0123456789"] {
        fixture.write_terminal(&session.id, data).await.unwrap();
    }
    let first = fixture.collect_terminal_output(&mut stream, 5, Duration::from_secs(2)).await;
    assert_terminal_fixture_order(&first, &["a", "b", "paste-0123456789"]);

    let cursor = fixture.last_cursor();
    fixture.drop_stream(stream);
    fixture.write_terminal(&session.id, "after-reconnect").await.unwrap();
    let mut resumed = fixture.control_client().open_events_stream(Some(&cursor)).await.unwrap();
    let resumed_events = fixture.collect_until_chunk(&mut resumed, "after-reconnect").await;
    assert_no_duplicate_sequences(&first, &resumed_events);

    fixture.force_event_ring_gap().await;
    assert!(matches!(resumed.next_message().await.unwrap().unwrap(), RuntimeRelayMessage::Gap { .. }));
}
```

不要把 shell 回显的完整正文写入失败输出；assert helper 失败只报告 sequence、字节数和 fixture step。

- [ ] **Step 2: 增加前端 1000 输入与 live fast-path 合同**

characterization test 通过 fake input writer defer 第一批，连续发 1000 个确定字符/控制序列，断言拼接结果精确相等、每 session 最大并发 1。Terminal fake 记录 `terminal-output listener timestamp → terminal.write invocation`，断言调用发生在手动 rAF scheduler flush 前；这不是 wall-clock 测试。

- [ ] **Step 3: 增加 Playwright release-like journey**

`workbench.spec.ts` 使用 backendHarness 的 event/invoke 控制：

1. 打开本机项目与 running session。
2. xterm 输入 `abc` 和 Backspace。
3. settle 对应 write invoke 后 emit terminal-output。
4. 下一次浏览器绘制前断言 xterm fake/可观察文本已接收增量。
5. 切到 browser/files 再切回，断言 Terminal 未重建且输出无重复。
6. 注入 `workbench:terminal-resync`，断言旧 generation queue 不追加。

E2E 证明浏览器接线，不声称真实 PTY 或 GUI p95。

- [ ] **Step 4: 更新 PRD 和分层指令**

在 `docs/prd.md` Workbench terminal 段加入：

```markdown
- 桌面终端始终展示 sidecar PTY 的真实回显，不做前端本地 echo；GUI 正常态通过 owner-sequenced control live stream 接收输出，断线按 cursor catch-up，Gap 时先 replay 再恢复 live。输入按 terminal session 串行提交，响应不确定时不得自动重放；已挂载 xterm 消费 live 增量，完整 bounded buffer 仅用于挂载与 resync。
```

`src-tauri/CLAUDE.md` 记录：stream normal path、unsupported poll fallback、3s header timeout/no body timeout、client runtime cache、mutation no replay、replay deque。`web/CLAUDE.md` 记录：input pump、generation/appendId handshake、live writer、Hooks 顺序和隐藏 xterm 保持挂载。`backend-operations.md` 把 `/events/stream` 标为 GUI normal path、`catch-up` 标为 recovery/fallback。

- [ ] **Step 5: 运行前端完整相关门禁**

```bash
cd web
npm run check:tokens
npm run check:i18n
npm run lint
npm run build
npm test -- src/hooks/workbenchTerminalBuffer.test.ts src/pages/Workbench/terminalInputPump.test.ts src/pages/Workbench/terminalLiveWriter.test.ts src/pages/Workbench/WorkbenchTerminalPane.test.tsx src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx src/pages/Workbench/WorkbenchTerminal.characterization.test.tsx
npm run test:e2e -- workbench.spec.ts frontend-foundation.spec.ts
```

Expected: all PASS；bundle contract 如 build 后为独立门禁则再运行 `npm run check:bundle`。

- [ ] **Step 6: 运行 Rust 相关门禁**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked backend::control_client::tests -- --nocapture
cargo test --locked backend::ui::tests -- --nocapture
cargo test --locked backend::event_bus::tests -- --nocapture
cargo test --locked workbench::sessions::tests -- --nocapture
cargo test --locked --test pty_smoke -- --nocapture --test-threads=1
cargo test --locked --test runtime_authority_smoke -- --nocapture --test-threads=1
```

Expected: all PASS；无 control token、terminal input/output fixture 正文出现在日志。

- [ ] **Step 7: 运行跨目录合同**

```bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all exit 0；没有新 capability 或 route inventory 漂移。

- [ ] **Step 8: 执行 L3 release GUI 性能验收**

按 spec 第 13.4 节执行空历史、120k/200k 历史、4 pane、本机/远端、IME/password/tmux/Claude TUI 矩阵。只记录各阶段时间、字节数、event count 和 opaque session hash：

```text
onData -> control ACK -> PTY emit -> GUI relay -> Tauri listener -> xterm write callback -> next painted frame
```

xterm callback 只标记 JS 写入完成；`key-to-visible` 必须用其后的首个绘制帧作为终点，并以 release GUI 帧时间线或高速视频抽样交叉验证。完成门槛：本机 key-to-visible p95 ≤ 50ms、p99 ≤ 100ms；publish→listener p95 ≤ 20ms；1000 输入零丢失/重复/重排。未执行则在交付说明中明确 `L3 GUI latency: NOT VERIFIED`。

- [ ] **Step 9: 提交最终合同与文档**

```bash
git add src-tauri/tests/runtime_authority_smoke.rs web/src/pages/Workbench/WorkbenchTerminal.characterization.test.tsx web/tests/workbench.spec.ts docs/prd.md src-tauri/CLAUDE.md web/CLAUDE.md docs/development/backend-operations.md
git commit -m "test: lock terminal latency and recovery contracts"
```

---

## Execution Order and Review Gates

1. Task 1 → review stream framing、timeout 和资源上限。
2. Task 2 → review cache invalidation 与 mutation 零重放。
3. Task 3 → review cursor、Gap、fallback 和 shutdown；此时应已消除固定 250ms 主延迟。
4. Task 4 与 Task 5 可在 Task 3 后并行；分别 review 输入顺序和 replay DTO 等价。
5. Task 6 依赖现有 provider/replay 合同，但可与 Task 5 并行；重点 review subscribe-before-snapshot 竞态。
6. Task 7 依赖 Task 6 的 TerminalPane 最终结构。
7. Task 8 最后执行，不得用 mock E2E 代替 L2 PTY 或 L3 GUI 性能证据。

每个任务必须先看失败测试，再看最小实现，再看定向绿测和 diff；不得把 Task 1–7 合成一个不可定位回归的大提交。
