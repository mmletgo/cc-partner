# Backend CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an independent `cc-partner-backend start|stop|status` CLI so remote devices can expose cc-partner remote support without launching the GUI, and make the GUI manage that backend lifecycle.

**Architecture:** Extract backend state construction and service startup from `src-tauri/src/lib.rs` into reusable backend modules. Replace hard `AppHandle` dependencies in HTTP-facing paths with a small backend runtime handle that can emit Tauri events in GUI mode and no-op or broadcast-only in CLI mode. Add a sidecar-backed GUI lifecycle layer that starts the CLI backend on launch and prompts on close.

**Tech Stack:** Rust 2021, Tauri 2.11, axum 0.7, tokio, sqlx SQLite, mdns-sd, React 19, TypeScript, Tauri JS `@tauri-apps/api/window`, `@tauri-apps/plugin-dialog`, Rust `tauri-plugin-shell`.

## Global Constraints

- 对话和用户可见说明使用中文。
- 所有代码文件使用 UTF-8。
- 修改代码前按目录读取最近的 `AGENTS.md` 或 fallback `CLAUDE.md`。
- 改动超过 100 行，执行阶段应使用 subagent，并按任务性质使用 `gpt-5.5(xhigh)`。
- 新增/修改 Rust 函数和结构需要中文 doc comment，说明 Business Logic 与 Code Logic。
- React hooks 必须放在 early return 之前。
- 前端用户可见文案必须走 i18n，不硬编码。
- 除数据库迁移外不要求向后兼容。
- 验证只跑相关子目录命令：`cd src-tauri && cargo test ...`、`cargo check`，前端改动跑聚焦 tsx/`npx tsc --noEmit`。
- 实现完成后更新 `src-tauri/CLAUDE.md`、根 `AGENTS.md`、相关 PRD。

---

## File Structure

- Create `src-tauri/src/backend/mod.rs`: backend module exports.
- Create `src-tauri/src/backend/control.rs`: pid/control file models, status classification, health probing, stop request helper.
- Create `src-tauri/src/backend/ui.rs`: GUI/headless event and asset adapter.
- Create `src-tauri/src/backend/runtime.rs`: shared AppState construction, service startup, background task startup and shutdown.
- Create `src-tauri/src/backend/cli.rs`: `start|serve|stop|status` orchestration used by bin.
- Create `src-tauri/src/bin/cc-partner-backend.rs`: thin binary entrypoint.
- Modify `src-tauri/src/lib.rs`: delegate setup to runtime, register shell plugin, ensure backend sidecar, handle exit cleanup through runtime.
- Modify `src-tauri/src/state.rs`: replace mandatory `AppHandle` field with backend UI/runtime handle.
- Modify `src-tauri/src/net/http_server.rs`: make mobile asset serving work without Tauri asset resolver and add local control route.
- Modify `src-tauri/src/net/routes/transfer.rs`, `src-tauri/src/transfer/{sender,receiver}.rs`: emit via backend UI handle.
- Modify `src-tauri/src/workbench/sessions.rs`, `src-tauri/src/workbench/remote_events.rs`, `src-tauri/src/commands/workbench.rs`, `src-tauri/src/net/routes/workbench.rs`: use runtime handle for terminal events and remote event broadcast.
- Modify `src-tauri/src/orchestrator/{scheduler,runner,delivery,completion,outbox}.rs`, `src-tauri/src/commands/orchestrator.rs`, `src-tauri/src/net/routes/orchestrator.rs`: pass runtime handle/state instead of raw `AppHandle` where HTTP mode needs it.
- Modify `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`: add `cc-partner-backend` bin and `tauri-plugin-shell`.
- Modify `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`: bundle CLI sidecar and allow execution.
- Create `web/src/api/backend.ts`: backend lifecycle IPC client.
- Create `web/src/lib/backendLifecycle.test.ts`: static/logic test for close dialog contracts.
- Modify `web/src/App.tsx`: close-request listener and dialog state.
- Modify `web/src/i18n/locales/{en,zh}/common.json`: close dialog strings.
- Modify `src-tauri/CLAUDE.md`, `AGENTS.md`, `docs/prd.md`: project memory and PRD updates.

---

### Task 1: Backend Control Files and Status Model

**Files:**
- Create: `src-tauri/src/backend/mod.rs`
- Create: `src-tauri/src/backend/control.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces:
  - `backend::control::BackendControlFile`
  - `backend::control::BackendStatusKind`
  - `backend::control::BackendStatus`
  - `backend::control::control_file_path() -> PathBuf`
  - `backend::control::pid_file_path() -> PathBuf`
  - `backend::control::read_control_file() -> Result<Option<BackendControlFile>, AppError>`
  - `backend::control::write_control_file(control: &BackendControlFile) -> Result<(), AppError>`
  - `backend::control::remove_control_files() -> Result<(), AppError>`
  - `backend::control::classify_status(control: Option<BackendControlFile>, process_alive: bool, health_ok: bool, error: Option<String>) -> BackendStatus`

- Consumes:
  - `crate::config::config_dir()`
  - `crate::error::AppError`

- [ ] **Step 1: Add module shell**

```rust
// src-tauri/src/backend/mod.rs
//! backend — 独立后端进程与 GUI 共享的运行时模块。
//!
//! Business Logic（为什么需要这个模块）:
//!     远端设备应能只启动 cc-partner 后端进程就暴露 Workbench/P2P 能力，GUI 也需要管理该后端生命周期。
//!
//! Code Logic（这个模块做什么）:
//!     聚合控制文件、UI 适配、共享 runtime 和 CLI 命令入口。

pub mod control;
pub mod ui;
pub mod runtime;
pub mod cli;
```

In `src-tauri/src/lib.rs`, change the module list to expose the backend module to the bin:

```rust
pub mod backend;
```

- [ ] **Step 2: Write failing control tests**

Add to `src-tauri/src/backend/control.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_status_reports_stopped_without_control_file() {
        let status = classify_status(None, false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stopped);
        assert!(status.control.is_none());
    }

    #[test]
    fn classify_status_reports_stale_when_pid_dead() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control), false, false, None);
        assert_eq!(status.kind, BackendStatusKind::Stale);
    }

    #[test]
    fn classify_status_reports_running_only_when_pid_and_health_are_ok() {
        let control = BackendControlFile::for_test(1234, 62116, "device-a");
        let status = classify_status(Some(control.clone()), true, true, None);
        assert_eq!(status.kind, BackendStatusKind::Running);
        assert_eq!(status.control.unwrap().pid, 1234);
    }
}
```

- [ ] **Step 3: Run failing test**

Run:

```bash
cd src-tauri
cargo test backend::control::tests::classify_status_reports_stopped_without_control_file
```

Expected: fail because `backend::control` does not yet define the model.

- [ ] **Step 4: Implement control model**

Core definitions:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendControlFile {
    pub pid: u32,
    pub port: u16,
    pub device_id: String,
    pub device_name: String,
    pub started_at: String,
    pub control_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendStatusKind {
    Running,
    Stopped,
    Stale,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    pub kind: BackendStatusKind,
    pub control: Option<BackendControlFile>,
    pub error: Option<String>,
}
```

Path and file helpers must use `crate::config::config_dir()` and JSON UTF-8. `classify_status` must return:

- `Stopped` when control is `None`.
- `Error` when `error.is_some()`.
- `Running` only when process and health are both OK.
- `Stale` for all other control-file-present cases.

- [ ] **Step 5: Run control tests**

Run:

```bash
cd src-tauri
cargo test backend::control
```

Expected: all Task 1 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/backend src-tauri/src/lib.rs
git commit -m "feat: add backend control status model"
```

---

### Task 2: Runtime UI Adapter for GUI and Headless Modes

**Files:**
- Modify: `src-tauri/src/state.rs`
- Create: `src-tauri/src/backend/ui.rs`
- Modify: `src-tauri/src/net/http_server.rs`

**Interfaces:**
- Produces:
  - `backend::ui::BackendAsset`
  - `backend::ui::BackendUi`
  - `backend::ui::TauriBackendUi`
  - `backend::ui::HeadlessBackendUi`
  - `AppState::emit_event<T: Serialize>(&self, event: &str, payload: T)`
  - `AppState::mobile_asset(&self, asset_key: &str) -> Option<BackendAsset>`

- Consumes:
  - `tauri::AppHandle` only in `TauriBackendUi`.
  - `web/dist` filesystem fallback in `HeadlessBackendUi`.

- [ ] **Step 1: Write adapter contract tests**

Add tests in `backend/ui.rs` that do not require a Tauri app:

```rust
#[test]
fn headless_asset_rejects_parent_paths() {
    let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
    assert!(ui.asset("../mobile.html").is_none());
}

#[test]
fn headless_emit_is_noop() {
    let ui = HeadlessBackendUi::new(std::path::PathBuf::from("/tmp/missing"));
    ui.emit("workbench:terminal-output", serde_json::json!({"ok": true}));
}
```

- [ ] **Step 2: Implement `BackendUi`**

Use a trait object that is `Send + Sync`:

```rust
pub trait BackendUi: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value);
    fn asset(&self, asset_key: &str) -> Option<BackendAsset>;
}
```

`TauriBackendUi` wraps `AppHandle` and uses `Emitter::emit`; its asset implementation must preserve existing exact-match resolver behavior from `mobile_tauri_asset`.

`HeadlessBackendUi` reads only normalized relative files under a dist directory. It must reject absolute paths, empty paths, `..`, Windows prefixes and backslash-containing path segments.

- [ ] **Step 3: Update `AppState`**

Replace:

```rust
pub app_handle: AppHandle,
```

with:

```rust
pub ui: Arc<dyn crate::backend::ui::BackendUi>,
```

Add methods:

```rust
pub fn emit_event<T>(&self, event: &str, payload: T)
where
    T: serde::Serialize,
{
    match serde_json::to_value(payload) {
        Ok(value) => self.ui.emit(event, value),
        Err(error) => tracing::warn!("序列化事件 {event} 失败: {error}"),
    }
}
```

and:

```rust
pub fn mobile_asset(&self, asset_key: &str) -> Option<crate::backend::ui::BackendAsset> {
    self.ui.asset(asset_key)
}
```

- [ ] **Step 4: Move mobile asset resolver**

In `net/http_server.rs`, make `mobile_asset_response` call `state.mobile_asset(&asset_key)` before filesystem fallback. Keep `mobile_asset_key`, MIME mapping, `/mobile` fallback behavior and tests unchanged.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cd src-tauri
cargo test backend::ui
cargo test net::http_server::tests
```

Expected: adapter and existing HTTP server tests pass.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/backend/ui.rs src-tauri/src/state.rs src-tauri/src/net/http_server.rs
git commit -m "refactor: decouple backend ui handle"
```

---

### Task 3: Replace HTTP-Facing AppHandle Event Paths

**Files:**
- Modify: `src-tauri/src/transfer/{sender,receiver}.rs`
- Modify: `src-tauri/src/net/routes/transfer.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/workbench/remote_events.rs`
- Modify: `src-tauri/src/commands/workbench.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/orchestrator/completion.rs`

**Interfaces:**
- Produces:
  - `workbench::remote_events::publish_workbench_remote_event_from_state(state: &AppState, event: WorkbenchRemoteEvent)`
  - `orchestrator::completion::spawn_maybe_handle_session_output_for_state(state: AppState, session_id: String, chunk: String)`
  - Workbench session registry methods accept `AppState` instead of raw `AppHandle` for HTTP-compatible paths.

- Consumes:
  - `AppState::emit_event`
  - Existing broadcast channel `state.workbench_remote_events`

- [ ] **Step 1: Add remote event publishing helper**

In `workbench/remote_events.rs`, add:

```rust
pub fn publish_workbench_remote_event_from_state(state: &AppState, event: WorkbenchRemoteEvent) {
    if let Err(error) = state.workbench_remote_events.send(event) {
        tracing::debug!("无 Workbench 远端事件订阅者: {error}");
    }
}
```

Keep the existing `publish_workbench_remote_event(app, event)` as a GUI compatibility wrapper if needed, but new HTTP and session code must call the state-based helper.

- [ ] **Step 2: Refactor transfer emits**

Replace transfer receiver route usage:

```rust
let app_handle = state.app_handle.clone();
receiver::handle_chunk(&state, &app_handle, ...)
```

with:

```rust
receiver::handle_chunk(&state, ...)
```

Inside sender/receiver, emit with:

```rust
state.emit_event("transfer:completed", payload);
```

Do not fail transfers when GUI event emission is unavailable.

- [ ] **Step 3: Refactor Workbench session events**

Change session registry creation/restore/spawn signatures from `AppHandle` to `AppState` clone where terminal output or status events are emitted.

`emit_terminal_output` must:

1. Append replay buffer.
2. Call `publish_workbench_remote_event_from_state(state, WorkbenchRemoteEvent::TerminalOutput(...))`.
3. Call `state.emit_event("workbench:terminal-output", payload)`.
4. Call `spawn_maybe_handle_session_output_for_state(state.clone(), session_id.to_string(), chunk)`.

`emit_status` follows the same broadcast + `state.emit_event` pattern.

- [ ] **Step 4: Add orchestrator completion state helper**

In `orchestrator/completion.rs`, add a state-based helper that does not need `AppHandle::state()`:

```rust
pub fn spawn_maybe_handle_session_output_for_state(
    state: AppState,
    session_id: String,
    chunk: String,
) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = maybe_handle_session_output_for_state(&state, &session_id, &chunk).await {
            tracing::debug!("处理 Orchestrator session 输出失败: {error}");
        }
    });
}
```

Existing AppHandle wrapper can delegate by reading state and calling this function.

- [ ] **Step 5: Update commands and routes**

For command functions that still receive an `AppHandle`, use it only for GUI-only actions. For Workbench sessions, merge, and HTTP routes, pass `state.inner().clone()` or `state.clone()` into state-based helpers.

Search must show no `state.app_handle` occurrences:

```bash
rg "state\\.app_handle" src-tauri/src
```

Expected: no matches.

- [ ] **Step 6: Run focused tests**

Run:

```bash
cd src-tauri
cargo test workbench::sessions
cargo test commands::workbench::tests::device_base_url_from_devices_returns_url_and_offline_error
cargo test transfer::receiver
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/transfer src-tauri/src/net/routes src-tauri/src/workbench src-tauri/src/commands/workbench.rs src-tauri/src/orchestrator/completion.rs
git commit -m "refactor: route backend events through app state"
```

---

### Task 4: Shared Backend Runtime

**Files:**
- Create: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/net/discovery.rs`

**Interfaces:**
- Produces:
  - `BackendRuntimeMode::{Gui, Headless}`
  - `BackendRuntimeOptions`
  - `BackendRuntime`
  - `build_app_state(ui: Arc<dyn BackendUi>) -> Result<AppState, AppError>`
  - `start_backend_services(state: &AppState, advertise: bool, browse: bool) -> Result<u16, AppError>`
  - `start_background_tasks(state: &AppState, mode: BackendRuntimeMode)`
  - `shutdown_backend_runtime(state: &AppState)`

- Consumes:
  - Existing schema constants moved from `lib.rs` or exposed inside runtime.

- [ ] **Step 1: Move DB initialization**

Move `init_db` and schema constants from `lib.rs` to `backend/runtime.rs`. Keep schema SQL identical. Make the function:

```rust
pub(crate) async fn init_db(db_path: &str) -> Result<sqlx::SqlitePool, AppError>
```

Run:

```bash
cd src-tauri
cargo test storage
```

Expected: existing storage tests still compile.

- [ ] **Step 2: Build `AppState` in runtime**

Extract the state-construction block from `lib.rs` into:

```rust
pub async fn build_app_state(
    ui: Arc<dyn BackendUi>,
) -> Result<AppState, AppError>
```

The function must initialize every repo/registry currently initialized in `lib.rs`, including Workbench browser previews, remote event bridge registry, health runtime and orchestrator telemetry.

- [ ] **Step 3: Split discovery advertise/browse**

Change `discovery::start_discovery(&state, port)` to:

```rust
pub async fn start_discovery(
    state: &AppState,
    port: u16,
    advertise: bool,
    browse: bool,
) -> Result<(), String>
```

Rules:

- Headless CLI uses `advertise=true, browse=true`.
- GUI uses `advertise=false, browse=true` when the sidecar backend is running, to avoid duplicate mDNS advertisement for the same device ID.
- Tests must verify browse-only mode does not register a service when `advertise=false`.

- [ ] **Step 4: Start service groups by mode**

Implement:

```rust
pub async fn start_backend_services(
    state: &AppState,
    advertise: bool,
    browse: bool,
) -> Result<u16, AppError>
```

It starts axum HTTP server only when `advertise == true`; GUI browse-only state should set `actual_http_port` from the running sidecar status instead.

Implement:

```rust
pub fn start_background_tasks(state: &AppState, mode: BackendRuntimeMode)
```

Mode behavior:

- `Headless`: start CC collector, cloud sync scheduler, Orchestrator scheduler and remote outbox.
- `Gui`: keep GUI-only health daemon, tray and hotkey in `lib.rs`; do not duplicate headless CC/cloud/orchestrator schedulers when sidecar is running.

- [ ] **Step 5: Centralize shutdown**

Move the existing `RunEvent::Exit` cleanup into:

```rust
pub fn shutdown_backend_runtime(state: &AppState)
```

It must stop discovery, cancel CC/cloud/orchestrator/outbox/health tokens if present, and call `workbench_sessions.shutdown_all()`.

- [ ] **Step 6: Run compile check**

Run:

```bash
cd src-tauri
cargo check
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/backend/runtime.rs src-tauri/src/lib.rs src-tauri/src/net/discovery.rs
git commit -m "refactor: extract shared backend runtime"
```

---

### Task 5: CLI Binary and Local Control API

**Files:**
- Create: `src-tauri/src/backend/cli.rs`
- Create: `src-tauri/src/bin/cc-partner-backend.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`

**Interfaces:**
- Produces:
  - `backend::cli::run_from_env() -> i32`
  - CLI commands: `start`, `serve`, `stop`, `status`
  - HTTP route: `POST /api/backend/control/stop`

- Consumes:
  - `BackendControlFile`
  - `build_app_state`
  - `start_backend_services`
  - `shutdown_backend_runtime`

- [ ] **Step 1: Add bin target**

In `src-tauri/Cargo.toml`:

```toml
[[bin]]
name = "cc-partner-backend"
path = "src/bin/cc-partner-backend.rs"
```

Create bin:

```rust
fn main() {
    std::process::exit(app_lib::backend::cli::run_from_env());
}
```

- [ ] **Step 2: Implement `serve`**

`serve` must:

1. Initialize tracing.
2. Create `HeadlessBackendUi`.
3. Build `AppState`.
4. Start HTTP/mDNS with advertise and browse enabled.
5. Write `backend-control.json` and `backend.pid`.
6. Wait for shutdown signal (`ctrl_c` or control route).
7. Call `shutdown_backend_runtime`.
8. Remove control files.

Use `tokio::sync::watch` or `CancellationToken` for the control route to signal shutdown.

- [ ] **Step 3: Implement control stop route**

Add route inside `http_server`:

```rust
.route("/api/backend/control/stop", post(backend_control::stop))
```

The handler must require JSON:

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopBackendRequest {
    pub control_token: String,
}
```

It must compare the token to the current control file or in-memory token and only then trigger shutdown. Invalid token returns 403.

- [ ] **Step 4: Implement `status` and `stop`**

`status` prints JSON to stdout:

```json
{"kind":"running","control":{"pid":123,"port":62116},"error":null}
```

`stop` reads the control file, calls `POST http://127.0.0.1:<port>/api/backend/control/stop`, waits briefly until health fails or pid exits, then removes stale control files.

- [ ] **Step 5: Implement `start`**

`start` must:

1. Call `status`.
2. If running, print status and exit 0.
3. If stale, remove control files.
4. Spawn current executable with `serve` using `std::process::Command`.
5. Detach child by not waiting.
6. Poll status for up to 10 seconds.
7. Print running status or return non-zero with error.

- [ ] **Step 6: Run CLI tests**

Run:

```bash
cd src-tauri
cargo test backend::cli backend::control
cargo check --bin cc-partner-backend
```

Expected: tests and bin check pass.

- [ ] **Step 7: Manual CLI smoke**

Run:

```bash
cd src-tauri
cargo run --bin cc-partner-backend -- start
cargo run --bin cc-partner-backend -- status
PORT=$(cargo run --quiet --bin cc-partner-backend -- status | node -e 'let s="";process.stdin.on("data",d=>s+=d);process.stdin.on("end",()=>console.log(JSON.parse(s).control.port))')
curl "http://127.0.0.1:${PORT}/api/health"
cargo run --bin cc-partner-backend -- stop
cargo run --bin cc-partner-backend -- status
```

Expected: health returns current device while running; final status is stopped or stale.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/backend/cli.rs src-tauri/src/bin/cc-partner-backend.rs src-tauri/src/net/http_server.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat: add cc partner backend cli"
```

---

### Task 6: GUI Sidecar Startup and Close Choice

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/capabilities/default.json`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/tray.rs`
- Create: `web/src/api/backend.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/i18n/locales/en/common.json`
- Modify: `web/src/i18n/locales/zh/common.json`
- Create: `web/src/lib/backendLifecycle.test.ts`

**Interfaces:**
- Produces:
  - Rust commands:
    - `get_backend_status() -> BackendStatus`
    - `start_backend_process() -> BackendStatus`
    - `stop_backend_process() -> BackendStatus`
    - `exit_gui() -> Result<(), AppError>`
  - Frontend API:
    - `backendApi.status()`
    - `backendApi.start()`
    - `backendApi.stop()`
    - `backendApi.exitGui()`

- Consumes:
  - `tauri_plugin_shell::ShellExt`
  - Tauri JS `getCurrentWindow().onCloseRequested`

- [ ] **Step 1: Add shell plugin and sidecar config**

Add dependency:

```toml
tauri-plugin-shell = "2"
```

Register plugin:

```rust
.plugin(tauri_plugin_shell::init())
```

In `tauri.conf.json`:

```json
"bundle": {
  "externalBin": ["binaries/cc-partner-backend"]
}
```

In `capabilities/default.json`, add `shell:allow-execute` for the sidecar.

- [ ] **Step 2: Add backend lifecycle commands**

Commands should call `backend::control` for status and `app.shell().sidecar("cc-partner-backend")` for packaged start/stop. In dev fallback, locate `target/debug/cc-partner-backend` relative to current executable or use `cargo run --bin cc-partner-backend`.

Register commands in `invoke_handler!`.

- [ ] **Step 3: Ensure backend on GUI setup**

In `lib.rs` setup after config/state is ready:

1. Check backend status.
2. If not running, start sidecar.
3. Set GUI `actual_http_port` from sidecar control port.
4. Start discovery in browse-only mode so GUI can populate `state.devices` without advertising duplicate self.

Do not start GUI axum server or mDNS advertise when sidecar is running.

- [ ] **Step 4: Update tray quit**

Replace direct `app.exit(0)` in `tray.rs` with showing the main window and emitting `backend:close-requested` or calling the same Rust close-choice command path. It must not bypass the user's backend stop choice.

- [ ] **Step 5: Add frontend API**

Create `web/src/api/backend.ts`:

```ts
import { invoke } from './client';
import type { BackendStatus } from '@/lib/types';

export const backendApi = {
  status: () => invoke<BackendStatus>('get_backend_status'),
  start: () => invoke<BackendStatus>('start_backend_process'),
  stop: () => invoke<BackendStatus>('stop_backend_process'),
  exitGui: () => invoke<{ ok: boolean }>('exit_gui'),
};
```

Add `BackendStatus` types to `web/src/lib/types.ts`.

- [ ] **Step 6: Add close requested listener**

In `App.tsx`, before routes, add a component that:

1. Calls `getCurrentWindow().onCloseRequested`.
2. Calls `event.preventDefault()`.
3. Opens a React modal with three buttons.
4. “仅关闭 GUI” calls `backendApi.exitGui()`.
5. “前后端都关闭” calls `backendApi.stop()` then `backendApi.exitGui()`.
6. “取消” closes the modal.

All strings use `common:backendClose.*`; hooks must be before early returns.

- [ ] **Step 7: Add frontend static test**

`web/src/lib/backendLifecycle.test.ts` must assert:

- `App.tsx` imports `onCloseRequested` or `getCurrentWindow`.
- `App.tsx` calls `preventDefault`.
- `App.tsx` calls `backendApi.stop` only for the full-close path.
- `common.json` has zh/en keys for title, guiOnly, stopBackend, cancel.

- [ ] **Step 8: Run GUI checks**

Run:

```bash
cd web
npx --yes tsx src/lib/backendLifecycle.test.ts
npx tsc --noEmit
```

Run Rust side:

```bash
cd src-tauri
cargo check
```

Expected: all pass.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json src-tauri/capabilities/default.json src-tauri/src/lib.rs src-tauri/src/tray.rs web/src
git commit -m "feat: manage backend lifecycle from gui"
```

---

### Task 7: Documentation, PRD, and Final Verification

**Files:**
- Modify: `src-tauri/CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `docs/prd.md`

**Interfaces:**
- Consumes final command names and behavior from Tasks 1-6.
- Produces updated project memory, not a changelog.

- [ ] **Step 1: Update `src-tauri/CLAUDE.md`**

Add a concise section covering:

- `cc-partner-backend start|stop|status`.
- Runtime split: CLI advertises HTTP/mDNS, GUI starts sidecar and browses mDNS.
- Focused validation commands:

```bash
cd src-tauri
cargo test backend::control backend::cli backend::ui
cargo check --bin cc-partner-backend
cargo check
```

- [ ] **Step 2: Update root `AGENTS.md`**

Only update project overview / top-level map:

- Add that `src-tauri/src/backend/` owns shared backend runtime and CLI lifecycle.
- Add that remote devices can run `cc-partner-backend start` without GUI.

Do not add implementation history.

- [ ] **Step 3: Update `docs/prd.md`**

Find existing Workbench/P2P remote support text and update requirements:

- Remote support must be available from `cc-partner-backend`.
- GUI launch auto-starts backend if missing.
- GUI close asks whether to stop backend.

- [ ] **Step 4: Full focused verification**

Run:

```bash
cd src-tauri
cargo test backend::control backend::cli backend::ui
cargo test workbench::sessions
cargo check --bin cc-partner-backend
cargo check
```

Run:

```bash
cd web
npx --yes tsx src/lib/backendLifecycle.test.ts
npx tsc --noEmit
```

Manual smoke:

```bash
cd src-tauri
cargo run --bin cc-partner-backend -- start
cargo run --bin cc-partner-backend -- status
cargo run --bin cc-partner-backend -- stop
```

Expected:

- Rust tests pass.
- TypeScript passes.
- CLI start/status/stop works.
- No `state.app_handle` remains.
- `rg "app\\.exit\\(0\\)" src-tauri/src/tray.rs` returns no direct tray quit bypass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/CLAUDE.md AGENTS.md docs/prd.md
git commit -m "docs: document backend cli lifecycle"
```

---

## Self-Review Checklist

- Spec coverage: CLI commands, GUI startup, GUI close choice, remote behavior, control files, docs and tests are covered.
- No placeholders: every task names exact files, interfaces and verification commands.
- Type consistency: `BackendControlFile`, `BackendStatusKind`, `BackendStatus`, `BackendUi`, `BackendRuntimeMode` names are consistent across tasks.
- Risk called out: raw `AppHandle` dependencies are explicitly removed from HTTP-facing paths before headless CLI runtime is enabled.
