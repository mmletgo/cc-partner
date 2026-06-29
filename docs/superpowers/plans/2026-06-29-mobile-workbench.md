# Mobile Workbench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a default-on LAN mobile Workbench at `/mobile` with QR/link discovery, HTTP Workbench transport, NDJSON terminal events, backend terminal replay, and mobile panels for terminal, files, Git, worktrees, and Prompt optimization.

**Architecture:** Keep the desktop Tauri Workbench unchanged on `invoke()` and Tauri events. Add a second browser SPA under `/mobile` that talks to the existing axum `/api/workbench/...` routes with fetch and reads `/api/workbench/events` directly. Preserve the existing PC tmux/session/window/pane model; mobile only changes presentation and adds backend replay for newly opened browsers.

**Tech Stack:** Tauri 2, Rust, axum 0.7, tokio, React 19, TypeScript, Vite, CSS Modules, xterm, CodeMirror, existing Workbench DTOs.

---

## Execution Notes

- Execute from a new worktree/branch because this feature is large and will touch frontend, Rust backend, docs, tests, and build config.
- Use branch prefix `codex/`, for example `codex/mobile-workbench`.
- Do not stage or commit `.superpowers/` brainstorming files.
- Follow project rule 20: all React hooks must be before early returns.
- All new functions and classes need Chinese doc comments/docstrings consistent with existing project style.
- For implementation, prefer subagents per task group:
  - Backend entry/replay tasks: code subagent.
  - Mobile frontend shell/transport/panels: code subagent.
  - Final docs/verification: inline review or code subagent.

## File Structure

### Backend

- Create `src-tauri/src/mobile/mod.rs`
  - Owns mobile access URL generation and DTOs.
- Create `src-tauri/src/net/routes/mobile.rs`
  - Exposes `/api/mobile/access-info`.
- Modify `src-tauri/src/net/routes/mod.rs`
  - Exports `mobile`.
- Modify `src-tauri/src/net/http_server.rs`
  - Adds `/api/mobile/access-info`.
  - Serves embedded mobile static assets under `/mobile`.
  - Keeps `/api/*` routes above static fallback.
- Modify `src-tauri/src/workbench/sessions.rs`
  - Adds session output replay ring buffer.
  - Adds replay query API in registry.
- Modify `src-tauri/src/workbench/models.rs`
  - Adds replay DTO if central DTO file is preferred.
- Modify `src-tauri/src/workbench/remote_protocol.rs`
  - Adds `RemoteReplaySessionReq` if HTTP Workbench routes use protocol DTOs.
- Modify `src-tauri/src/net/routes/workbench.rs`
  - Adds `/api/workbench/projects/list`.
  - Adds `/api/workbench/sessions/replay`.
- Modify `src-tauri/src/workbench/remote_client.rs`
  - No planned change for first implementation; replay is consumed by mobile browser transport.
- Modify `src-tauri/src/lib.rs`
  - Registers `mobile` module.

### Frontend Shared

- Create `web/src/api/workbenchTransport.ts`
  - Defines transport interface and desktop adapter.
- Create `web/src/api/workbenchHttp.ts`
  - Maps browser fetch calls to `/api/workbench/...`.
- Create `web/src/api/mobile.ts`
  - Fetches `/api/mobile/access-info`.
- Create `web/src/hooks/useWorkbenchHttpEvents.ts`
  - Reads NDJSON event stream and appends to terminal buffer store.
- Create `web/src/hooks/workbenchHttpEvents.test.ts`
  - Tests NDJSON parsing and stale line buffering.
- Modify `web/src/hooks/useWorkbenchTerminalBuffers.tsx`
  - Allows an optional event source mode or reuses store in mobile provider.
- Modify `web/src/lib/types.ts`
  - Adds `MobileAccessInfo`, `WorkbenchSessionReplay`, and transport-adjacent DTOs.

### Mobile SPA

- Create `web/mobile.html`
  - Vite mobile entry HTML.
- Create `web/src/mobile/main.tsx`
  - React entry for `/mobile`.
- Create `web/src/mobile/MobileApp.tsx`
  - Mobile router/provider shell.
- Create `web/src/mobile/MobileWorkbench.tsx`
  - Mobile Workbench state coordinator.
- Create `web/src/mobile/MobileWorkbench.module.css`
  - Mobile shell layout, top bar, drawer, fixed rail.
- Create `web/src/mobile/mobileWorkbenchState.ts`
  - Reducers/helpers for active panel, project/worktree/window selection.
- Create `web/src/mobile/mobileWorkbenchState.test.ts`
  - Pure state tests.
- Create `web/src/mobile/components/MobileWorkbenchShell.tsx`
  - Top bar, drawer, wide rail.
- Create `web/src/mobile/components/MobileProjectPanel.tsx`
  - Recent project selection.
- Create `web/src/mobile/components/MobileTerminalPanel.tsx`
  - xterm terminal window/pane surface.
- Create `web/src/mobile/components/MobileFilesPanel.tsx`
  - File tree and file workspace wrapper.
- Create `web/src/mobile/components/MobileGitPanel.tsx`
  - Git history/status/actions.
- Create `web/src/mobile/components/MobileWorktreePanel.tsx`
  - Worktree list/create/remove.
- Create `web/src/mobile/components/MobilePromptPanel.tsx`
  - Prompt optimizer input and write-to-terminal action.
- Reuse existing domain components where practical:
  - `WorkbenchCodeEditor`
  - `WorkbenchMarkdownEditor`
  - `WorkbenchHtmlPreview`
  - `WorkbenchImagePreview`
  - `WorkbenchCsvPreview`
  - `WorkbenchSqlitePreview`
  - `WorkbenchFileWorkspace`

### Desktop QR Entry

- Create `web/src/components/domain/MobileAccessCard/MobileAccessCard.tsx`
  - Shows LAN URLs, copy button, QR canvas/SVG, warning.
- Create `web/src/components/domain/MobileAccessCard/MobileAccessCard.module.css`
- Create `web/src/components/domain/MobileAccessCard/index.ts`
- Create `web/src/components/domain/MobileAccessCard/mobileQr.ts`
  - Thin wrapper around QR generation.
- Create `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`
  - Tests URL selection and warning rendering helpers.
- Modify `web/package.json` / `web/package-lock.json`
  - Add QR dependency with `npm install qrcode @types/qrcode`.
- Modify `web/src/pages/Settings/Settings.tsx`
  - Adds Mobile Access card in a relevant settings section.
- Modify `web/src/pages/Workbench/Workbench.tsx`
  - Adds a compact mobile access entry in the Workbench toolbar/inspector area without touching terminal viewport.
- Modify `web/src/i18n/locales/zh/settings.json`, `web/src/i18n/locales/en/settings.json`, `web/src/i18n/locales/zh/workbench.json`, `web/src/i18n/locales/en/workbench.json`
  - Adds user-facing copy.

### Build and Docs

- Modify `web/vite.config.ts`
  - Adds multi-page input for desktop `index.html` and `mobile.html`.
  - Keeps output assets in `web/dist/assets`.
- Modify `AGENTS.md`
  - Root overview and top-level map only.
- Modify `web/CLAUDE.md`
  - Mobile SPA architecture and frontend test commands.
- Modify `src-tauri/CLAUDE.md`
  - Mobile axum entry, access-info, replay buffer, tmux reuse.
- Modify `docs/prd.md`
  - Product requirement for LAN mobile Workbench.

---

## Task 0: Prepare Implementation Worktree

**Files:**
- No source files changed in this task.

- [ ] **Step 1: Create isolated branch/worktree**

Run from `/Users/hans/web_project/cc-partner`:

```bash
git status --short --branch
git worktree add ../cc-partner-mobile-workbench -b codex/mobile-workbench
cd ../cc-partner-mobile-workbench
```

Expected:

```text
Preparing worktree (new branch 'codex/mobile-workbench')
HEAD is now at <current> ...
```

- [ ] **Step 2: Confirm instructions in new worktree**

Run:

```bash
pwd
sed -n '1,260p' AGENTS.md
sed -n '1,220p' web/CLAUDE.md
sed -n '1,260p' src-tauri/CLAUDE.md
```

Expected: current directory is `/Users/hans/web_project/cc-partner-mobile-workbench`; documents describe Workbench and validation commands.

- [ ] **Step 3: Commit checkpoint**

No commit needed. This task only creates the isolated execution branch.

---

## Task 1: Backend Mobile Access Info API

**Files:**
- Create: `src-tauri/src/mobile/mod.rs`
- Create: `src-tauri/src/net/routes/mobile.rs`
- Modify: `src-tauri/src/net/routes/mod.rs`
- Modify: `src-tauri/src/net/http_server.rs`

- [ ] **Step 1: Write failing Rust tests for mobile access URL generation**

Create `src-tauri/src/mobile/mod.rs`:

```rust
//! mobile/mod.rs — 局域网移动访问辅助能力
//!
//! Business Logic（为什么需要这个模块）:
//!     手机浏览器需要一个可直接打开的局域网 URL。桌面端设置页和 Workbench 需要展示
//!     访问链接与二维码，因此后端要集中生成不含 localhost 的移动端访问地址。
//!
//! Code Logic（这个模块做什么）:
//!     定义移动访问 DTO，过滤局域网 IP，生成 `/mobile` URL，并提供纯函数测试。

use crate::config::AppConfig;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileAccessInfoDto {
    pub device_name: String,
    pub port: u16,
    pub urls: Vec<String>,
}

/// Business Logic（为什么需要这个函数）:
///     桌面端需要把当前设备可被手机访问的地址展示为链接和二维码。
///
/// Code Logic（这个函数做什么）:
///     接收设备名、实际 HTTP 端口和候选 IP，过滤 loopback/空值后生成 `/mobile` URL。
pub fn build_mobile_access_info(
    config: &AppConfig,
    port: u16,
    candidate_ips: Vec<String>,
) -> MobileAccessInfoDto {
    let urls = candidate_ips
        .into_iter()
        .filter(|ip| is_mobile_access_ip(ip))
        .map(|ip| format!("http://{ip}:{port}/mobile"))
        .collect();
    MobileAccessInfoDto {
        device_name: config.device_name.clone(),
        port,
        urls,
    }
}

/// Business Logic（为什么需要这个函数）:
///     二维码不能生成 localhost 地址，否则手机扫码会访问自己的 localhost。
///
/// Code Logic（这个函数做什么）:
///     用字符串规则过滤 loopback 和空地址；保持实现简单，真实 IP 获取在调用方完成。
fn is_mobile_access_ip(ip: &str) -> bool {
    let trimmed = ip.trim();
    !trimmed.is_empty()
        && trimmed != "localhost"
        && trimmed != "127.0.0.1"
        && trimmed != "::1"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Business Logic（为什么需要这个测试）:
    ///     手机二维码必须使用局域网地址，不能把 localhost 暴露给用户。
    ///
    /// Code Logic（这个测试做什么）:
    ///     构造包含 loopback 和 LAN IP 的候选列表，断言只输出 LAN `/mobile` URL。
    #[test]
    fn access_info_filters_loopback_urls() {
        let mut config = AppConfig::default();
        config.device_name = "Hans MacBook".to_string();
        let info = build_mobile_access_info(
            &config,
            51842,
            vec![
                "127.0.0.1".to_string(),
                "localhost".to_string(),
                "192.168.1.23".to_string(),
            ],
        );

        assert_eq!(info.device_name, "Hans MacBook");
        assert_eq!(info.port, 51842);
        assert_eq!(info.urls, vec!["http://192.168.1.23:51842/mobile"]);
    }
}
```

- [ ] **Step 2: Run the focused failing test**

Run:

```bash
cd src-tauri
cargo test mobile::tests::access_info_filters_loopback_urls --lib
```

Expected: FAIL because `mobile` module is not registered yet.

- [ ] **Step 3: Register the module and add route handler**

Modify `src-tauri/src/lib.rs` near existing module declarations:

```rust
mod mobile;
```

Modify `src-tauri/src/net/routes/mod.rs`:

```rust
pub mod mobile;
```

Create `src-tauri/src/net/routes/mobile.rs`:

```rust
//! net/routes/mobile.rs — 移动访问 HTTP 路由
//!
//! Business Logic（为什么需要这个模块）:
//!     桌面端需要通过 HTTP API 获取手机可访问的局域网链接，用于展示文本链接和二维码。
//!
//! Code Logic（这个模块做什么）:
//!     暴露 `/api/mobile/access-info`，从 AppState 读取配置和实际端口并返回 mobile DTO。

use crate::mobile::{build_mobile_access_info, MobileAccessInfoDto};
use crate::net::discovery::local_lan_ip;
use crate::state::AppState;
use axum::{extract::State, Json};
use std::sync::atomic::Ordering;

/// Business Logic（为什么需要这个函数）:
///     手机扫码前，桌面端需要知道当前设备在局域网中的访问地址。
///
/// Code Logic（这个函数做什么）:
///     读取实际 HTTP 端口和当前配置，使用 LAN IP 生成 `/mobile` 访问 URL。
pub async fn access_info(State(state): State<AppState>) -> Json<MobileAccessInfoDto> {
    let config = state.config.read().expect("config 锁中毒").clone();
    let port = state.actual_http_port.load(Ordering::SeqCst);
    let ips = local_lan_ip().map(|ip| vec![ip.to_string()]).unwrap_or_default();
    Json(build_mobile_access_info(&config, port, ips))
}
```

Change `src-tauri/src/net/discovery.rs` from `fn local_lan_ip() -> Option<IpAddr>` to `pub fn local_lan_ip() -> Option<IpAddr>`.

Modify `src-tauri/src/net/http_server.rs` imports:

```rust
use crate::net::routes::{
    cc_history, claude_code_assets, claude_md_sync, health, mobile, scratchpad_sync,
    ssh_target_sync, sync, transfer, workbench,
};
```

Add the route before Workbench routes:

```rust
.route("/api/mobile/access-info", get(mobile::access_info))
```

- [ ] **Step 4: Run tests and cargo check**

Run:

```bash
cd src-tauri
cargo test mobile::tests::access_info_filters_loopback_urls --lib
cargo check
```

Expected: PASS for the test and successful cargo check.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/mobile/mod.rs src-tauri/src/net/routes/mobile.rs src-tauri/src/net/routes/mod.rs src-tauri/src/net/http_server.rs src-tauri/src/lib.rs src-tauri/src/net/discovery.rs
git commit -m "feat: expose mobile access info"
```

---

## Task 2: Backend Mobile Static SPA Routing

**Files:**
- Modify: `src-tauri/src/net/http_server.rs`
- Test: `src-tauri/src/net/http_server.rs`
- Produced by Task 5 build: `web/dist/mobile.html`, `web/dist/assets/*`

- [ ] **Step 1: Write route matching tests**

Append to `src-tauri/src/net/http_server.rs` test module:

```rust
/// Business Logic（为什么需要这个函数）:
///     `/api/*` 不能被移动端 SPA fallback 吃掉，否则 Workbench HTTP API 会失效。
///
/// Code Logic（这个函数做什么）:
///     用纯路径匹配 helper 断言 `/mobile` 归 mobile，`/api/...` 归 API。
#[test]
fn mobile_spa_path_matching_does_not_capture_api_routes() {
    assert!(is_mobile_spa_path("/mobile"));
    assert!(is_mobile_spa_path("/mobile/"));
    assert!(is_mobile_spa_path("/mobile/assets/index.js"));
    assert!(!is_mobile_spa_path("/api/workbench/events"));
    assert!(!is_mobile_spa_path("/api/mobile/access-info"));
}
```

- [ ] **Step 2: Run the focused failing test**

```bash
cd src-tauri
cargo test net::http_server::tests::mobile_spa_path_matching_does_not_capture_api_routes --lib
```

Expected: FAIL because `is_mobile_spa_path` does not exist.

- [ ] **Step 3: Add static SPA helpers and fallback**

Modify `src-tauri/src/net/http_server.rs` imports:

```rust
use axum::body::Body;
use axum::http::{header, HeaderValue, Request, Response, StatusCode, Uri};
use axum::routing::{get, get_service, post};
use tower::service_fn;
```

Add helper functions near `BODY_LIMIT_BYTES`:

```rust
/// Business Logic（为什么需要这个函数）:
///     手机浏览器刷新 `/mobile` 深层路径时，需要返回 mobile SPA，而不是 404。
///
/// Code Logic（这个函数做什么）:
///     仅匹配 `/mobile` 和 `/mobile/...`；显式排除所有 `/api/...`。
fn is_mobile_spa_path(path: &str) -> bool {
    (path == "/mobile" || path.starts_with("/mobile/")) && !path.starts_with("/api/")
}

/// Business Logic（为什么需要这个函数）:
///     打包后的移动端静态资源需要由 axum 提供给同局域网手机浏览器。
///
/// Code Logic（这个函数做什么）:
///     读取 Tauri 嵌入资源或开发期 dist 文件，按路径返回 JS/CSS/HTML 响应。
async fn serve_mobile_spa(req: Request<Body>) -> Result<Response<Body>, std::convert::Infallible> {
    let path = req.uri().path();
    let asset_path = if path == "/mobile" || path == "/mobile/" {
        "mobile.html".to_string()
    } else if let Some(rest) = path.strip_prefix("/mobile/") {
        rest.to_string()
    } else {
        "mobile.html".to_string()
    };

    let response = match mobile_asset_response(&asset_path).await {
        Some(response) => response,
        None => mobile_asset_response("mobile.html")
            .await
            .unwrap_or_else(|| {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("mobile asset not found"))
                    .expect("valid not found response")
            }),
    };
    Ok(response)
}

/// Business Logic（为什么需要这个函数）:
///     静态资源需要正确 MIME，手机浏览器才能加载 mobile SPA。
///
/// Code Logic（这个函数做什么）:
///     从 `../web/dist` 读取构建产物，返回带 content-type 的 response。
async fn mobile_asset_response(path: &str) -> Option<Response<Body>> {
    let safe_path = path.trim_start_matches('/');
    if safe_path.contains("..") {
        return None;
    }
    let full_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../web/dist")
        .join(safe_path);
    let bytes = tokio::fs::read(&full_path).await.ok()?;
    let content_type = mobile_content_type(safe_path);
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
            .body(Body::from(bytes))
            .expect("valid mobile asset response"),
    )
}

/// Business Logic（为什么需要这个函数）:
///     浏览器按 MIME 执行 JS/CSS；错误 MIME 会导致移动端白屏。
///
/// Code Logic（这个函数做什么）:
///     根据扩展名返回常见静态资源 content-type。
fn mobile_content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else {
        "application/octet-stream"
    }
}
```

Add route after `/api/*` routes and before `.layer(...)`:

```rust
.fallback_service(service_fn(|req: Request<Body>| async move {
    if is_mobile_spa_path(req.uri().path()) {
        serve_mobile_spa(req).await
    } else {
        Ok::<_, std::convert::Infallible>(
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("not found"))
                .expect("valid not found response"),
        )
    }
}))
```

Remove unused imports if `get_service`, `Uri`, or `StatusCode` are not used after implementation.

- [ ] **Step 4: Run route test and cargo check**

```bash
cd src-tauri
cargo test net::http_server::tests::mobile_spa_path_matching_does_not_capture_api_routes --lib
cargo check
```

Expected: PASS and successful cargo check.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/http_server.rs
git commit -m "feat: serve mobile workbench shell"
```

---

## Task 3: Backend Workbench Session Replay

**Files:**
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/workbench/remote_protocol.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `web/src/lib/types.ts`

- [ ] **Step 1: Add failing replay buffer unit test**

Append to `src-tauri/src/workbench/sessions.rs` tests:

```rust
/// Business Logic（为什么需要这个测试）:
///     手机浏览器首次进入终端时需要拉取历史输出，不能只看到未来事件。
///
/// Code Logic（这个测试做什么）:
///     构造 replay buffer，追加超过上限的输出，断言保留尾部内容、truncated=true、lastSeq 正确。
#[test]
fn session_replay_buffer_keeps_recent_output_with_last_seq() {
    let mut buffer = SessionReplayBuffer::new(8);
    buffer.append(1, "abc");
    buffer.append(2, "def");
    buffer.append(3, "ghij");

    let replay = buffer.snapshot("session-a");

    assert_eq!(replay.session_id, "session-a");
    assert_eq!(replay.buffer, "cdefghij");
    assert!(replay.truncated);
    assert_eq!(replay.last_seq, 3);
}
```

- [ ] **Step 2: Run the focused failing test**

```bash
cd src-tauri
cargo test workbench::sessions::tests::session_replay_buffer_keeps_recent_output_with_last_seq --lib
```

Expected: FAIL because `SessionReplayBuffer` does not exist.

- [ ] **Step 3: Implement replay DTO and buffer**

In `src-tauri/src/workbench/sessions.rs`, add near terminal event payload definitions:

```rust
const MAX_SESSION_REPLAY_CHARS: usize = 200_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSessionReplayDto {
    pub session_id: String,
    pub buffer: String,
    pub truncated: bool,
    pub last_seq: u64,
}

#[derive(Debug, Clone)]
struct SessionReplayBuffer {
    max_chars: usize,
    buffer: String,
    truncated: bool,
    last_seq: u64,
}

impl SessionReplayBuffer {
    /// Business Logic（为什么需要这个函数）:
    ///     每个终端 session 需要独立缓存最近输出，供移动端新连接时回放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     初始化固定最大字符数的内存 buffer。
    fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            buffer: String::new(),
            truncated: false,
            last_seq: 0,
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     PTY/tmux 输出到达时，除了 emit 给当前桌面端，也要保存给后续移动端回放。
    ///
    /// Code Logic（这个函数做什么）:
    ///     追加 chunk，超过 max_chars 时保留尾部，并更新 last_seq。
    fn append(&mut self, seq: u64, chunk: &str) {
        self.last_seq = seq;
        self.buffer.push_str(chunk);
        if self.buffer.len() > self.max_chars {
            let keep_from = self.buffer.len().saturating_sub(self.max_chars);
            self.buffer = self.buffer[keep_from..].to_string();
            self.truncated = true;
        }
    }

    /// Business Logic（为什么需要这个函数）:
    ///     移动端进入终端时需要一次性读取当前可回放输出和最后事件序号。
    ///
    /// Code Logic（这个函数做什么）:
    ///     克隆当前 buffer 状态为 camelCase DTO。
    fn snapshot(&self, session_id: &str) -> WorkbenchSessionReplayDto {
        WorkbenchSessionReplayDto {
            session_id: session_id.to_string(),
            buffer: self.buffer.clone(),
            truncated: self.truncated,
            last_seq: self.last_seq,
        }
    }
}
```

- [ ] **Step 4: Store replay buffers in registry**

Extend `WorkbenchSessionRegistry`:

```rust
pub struct WorkbenchSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<WorkbenchSessionHandle>>>>>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
}
```

Update `new()`:

```rust
Self {
    sessions: Arc::new(Mutex::new(HashMap::new())),
    replay_buffers: Arc::new(Mutex::new(HashMap::new())),
}
```

Before `spawn_reader_thread(...)` in `spawn_row`, ensure a replay buffer exists:

```rust
self.replay_buffers
    .lock()
    .expect("workbench replay buffer 锁中毒")
    .entry(session_id.clone())
    .or_insert_with(|| SessionReplayBuffer::new(MAX_SESSION_REPLAY_CHARS));
spawn_reader_thread(
    app.clone(),
    session_id.clone(),
    reader,
    self.replay_buffers.clone(),
);
```

Change `spawn_reader_thread` signature and `emit_terminal_output`:

```rust
fn spawn_reader_thread(
    app: AppHandle,
    session_id: String,
    mut reader: Box<dyn Read + Send>,
    replay_buffers: Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) {
    std::thread::spawn(move || {
        let mut seq: u64 = 0;
        let mut decoder = Utf8Decoder::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    emit_terminal_output(
                        &app,
                        &session_id,
                        &mut seq,
                        decoder.decode(&buf[..n]),
                        &replay_buffers,
                    );
                }
                Err(error) => {
                    tracing::debug!("读取工作台终端输出失败: {error}");
                    break;
                }
            }
        }
        if let Some(chunk) = decoder.finish() {
            emit_terminal_output(&app, &session_id, &mut seq, chunk, &replay_buffers);
        }
    });
}

fn emit_terminal_output(
    app: &AppHandle,
    session_id: &str,
    seq: &mut u64,
    chunk: String,
    replay_buffers: &Arc<Mutex<HashMap<String, SessionReplayBuffer>>>,
) {
    if chunk.is_empty() {
        return;
    }
    *seq += 1;
    if let Some(buffer) = replay_buffers
        .lock()
        .expect("workbench replay buffer 锁中毒")
        .get_mut(session_id)
    {
        buffer.append(*seq, &chunk);
    }
    let event = WorkbenchTerminalOutputPayload {
        session_id: session_id.to_string(),
        chunk,
        seq: *seq,
        ts: chrono::Utc::now().timestamp_millis(),
    };
    if let Err(error) = app.emit("workbench:terminal-output", event.clone()) {
        tracing::warn!("发送工作台终端输出事件失败: {error}");
    }
    publish_workbench_remote_event(app, WorkbenchRemoteEvent::TerminalOutput(event));
}
```

Add registry methods:

```rust
/// Business Logic（为什么需要这个函数）:
///     移动端首次打开 session 时需要读取最近输出。
///
/// Code Logic（这个函数做什么）:
///     从 replay buffer map 中读取 session 快照；不存在时返回空快照。
pub fn replay(&self, session_id: &str) -> WorkbenchSessionReplayDto {
    self.replay_buffers
        .lock()
        .expect("workbench replay buffer 锁中毒")
        .get(session_id)
        .map(|buffer| buffer.snapshot(session_id))
        .unwrap_or(WorkbenchSessionReplayDto {
            session_id: session_id.to_string(),
            buffer: String::new(),
            truncated: false,
            last_seq: 0,
        })
}
```

When removing a session, also remove `replay_buffers[session_id]`.

- [ ] **Step 5: Add HTTP replay route**

In `src-tauri/src/workbench/remote_protocol.rs`:

```rust
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteReplaySessionReq {
    pub session_id: String,
}
```

In `src-tauri/src/net/routes/workbench.rs`:

```rust
use crate::workbench::sessions::WorkbenchSessionReplayDto;
use crate::workbench::remote_protocol::RemoteReplaySessionReq;

/// Business Logic（为什么需要这个函数）:
///     手机浏览器打开终端时需要获取历史输出，避免只能看到未来事件。
///
/// Code Logic（这个函数做什么）:
///     校验 session 存在后返回 registry 中保存的 replay buffer 快照。
pub async fn replay_workbench_session(
    State(state): State<AppState>,
    Json(req): Json<RemoteReplaySessionReq>,
) -> Result<Json<WorkbenchSessionReplayDto>, AppError> {
    state.workbench_sessions.get_handle(&req.session_id)?;
    Ok(Json(state.workbench_sessions.replay(&req.session_id)))
}
```

In `src-tauri/src/net/http_server.rs` add route:

```rust
.route(
    "/api/workbench/sessions/replay",
    post(workbench::replay_workbench_session),
)
```

In `web/src/lib/types.ts` add:

```ts
export interface WorkbenchSessionReplay {
  sessionId: string;
  buffer: string;
  truncated: boolean;
  lastSeq: number;
}
```

- [ ] **Step 6: Run tests and check**

```bash
cd src-tauri
cargo test workbench::sessions::tests::session_replay_buffer_keeps_recent_output_with_last_seq --lib
cargo test workbench::sessions --lib
cargo check
```

Expected: PASS and successful cargo check.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/workbench/sessions.rs src-tauri/src/workbench/remote_protocol.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/http_server.rs web/src/lib/types.ts
git commit -m "feat: add workbench session replay"
```

---

## Task 4: HTTP Workbench Transport and NDJSON Events

**Files:**
- Create: `web/src/api/workbenchTransport.ts`
- Create: `web/src/api/workbenchHttp.ts`
- Create: `web/src/api/mobile.ts`
- Create: `web/src/hooks/useWorkbenchHttpEvents.ts`
- Create: `web/src/hooks/workbenchHttpEvents.test.ts`
- Modify: `web/src/lib/types.ts`

- [ ] **Step 1: Write NDJSON parser test**

Create `web/src/hooks/workbenchHttpEvents.test.ts`:

```ts
import assert from 'node:assert/strict';
import {
  parseWorkbenchNdjsonChunk,
  type WorkbenchNdjsonParserState,
} from './useWorkbenchHttpEvents';

const state: WorkbenchNdjsonParserState = { pending: '' };
const first = parseWorkbenchNdjsonChunk(
  state,
  '{"type":"terminalOutput","payload":{"sessionId":"s1","chunk":"你',
);
assert.deepEqual(first, []);
assert.equal(state.pending.includes('terminalOutput'), true);

const second = parseWorkbenchNdjsonChunk(
  state,
  '好","seq":1,"ts":10}}\\n{"type":"terminalStatus","payload":{"sessionId":"s1","status":"running","exitCode":null,"ts":11}}\\n',
);

assert.equal(second.length, 2);
assert.equal(second[0]?.type, 'terminalOutput');
assert.equal(second[1]?.type, 'terminalStatus');
assert.equal(state.pending, '');
```

- [ ] **Step 2: Run parser test and confirm failure**

```bash
cd web
npx --yes tsx src/hooks/workbenchHttpEvents.test.ts
```

Expected: FAIL because `useWorkbenchHttpEvents` does not exist.

- [ ] **Step 3: Implement mobile API and transport**

Create `web/src/api/mobile.ts`:

```ts
import type { MobileAccessInfo } from '@/lib/types';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端移动访问卡片需要获取局域网 URL，用于展示链接和二维码。
 *
 * Code Logic（这个函数做什么）:
 *   通过 HTTP 请求 `/api/mobile/access-info` 并把非 2xx 响应转成 Error。
 */
export async function getMobileAccessInfo(): Promise<MobileAccessInfo> {
  const response = await fetch('/api/mobile/access-info');
  if (!response.ok) {
    throw new Error(`获取移动访问地址失败: HTTP ${response.status}`);
  }
  return (await response.json()) as MobileAccessInfo;
}
```

Create `web/src/api/workbenchTransport.ts`:

```ts
import { workbenchApi } from './workbench';
import type {
  WorkbenchFileNode,
  WorkbenchGitCommit,
  WorkbenchOpenFile,
  WorkbenchPathInfo,
  WorkbenchProject,
  WorkbenchSaveTextResult,
  WorkbenchSession,
  WorkbenchSessionReplay,
  WorkbenchWorktree,
} from '@/lib/types';

export interface WorkbenchTransport {
  projects: {
    list: () => Promise<WorkbenchProject[]>;
    open: (path: string) => Promise<WorkbenchProject>;
  };
  worktrees: {
    list: (projectId: string) => Promise<WorkbenchWorktree[]>;
  };
  sessions: {
    list: (projectId?: string) => Promise<WorkbenchSession[]>;
    create: (
      projectId: string,
      initialSize?: { cols: number; rows: number },
      worktreeId?: string | null,
    ) => Promise<WorkbenchSession>;
    writeInput: (sessionId: string, data: string) => Promise<{ ok: boolean; sessionId: string }>;
    resize: (sessionId: string, cols: number, rows: number) => Promise<{ ok: boolean; sessionId: string }>;
    replay: (sessionId: string) => Promise<WorkbenchSessionReplay>;
  };
  files: {
    listDir: (projectId: string, path?: string, worktreeId?: string | null) => Promise<WorkbenchFileNode[]>;
    info: (projectId: string, path: string, worktreeId?: string | null) => Promise<WorkbenchPathInfo>;
    open: (projectId: string, path: string, worktreeId?: string | null) => Promise<WorkbenchOpenFile>;
    saveText: (
      projectId: string,
      path: string,
      content: string,
      baseHash: string,
      worktreeId?: string | null,
    ) => Promise<WorkbenchSaveTextResult>;
  };
  git: {
    listCommits: (projectId: string, worktreeId?: string | null, limit?: number) => Promise<WorkbenchGitCommit[]>;
  };
}

export const tauriWorkbenchTransport: WorkbenchTransport = {
  projects: {
    list: workbenchApi.projects.list,
    open: workbenchApi.projects.add,
  },
  worktrees: workbenchApi.worktrees,
  sessions: {
    ...workbenchApi.sessions,
    replay: async () => ({ sessionId: '', buffer: '', truncated: false, lastSeq: 0 }),
  },
  files: workbenchApi.files,
  git: workbenchApi.git,
};
```

Create `web/src/api/workbenchHttp.ts`:

```ts
import type { WorkbenchTransport } from './workbenchTransport';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端运行在普通浏览器中，不能使用 Tauri invoke，需要通过同源 HTTP API 调 Workbench 后端。
 *
 * Code Logic（这个函数做什么）:
 *   POST JSON 到指定 path；非 2xx 响应读取 error/message/body 并抛 Error。
 */
async function postJson<T>(path: string, body: unknown): Promise<T> {
  const response = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text || `HTTP ${response.status}`);
  }
  return (await response.json()) as T;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端读取 access-info 等 GET API 时也需要统一错误处理。
 *
 * Code Logic（这个函数做什么）:
 *   GET 指定 path 并解析 JSON。
 */
async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(path);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text || `HTTP ${response.status}`);
  }
  return (await response.json()) as T;
}

export const httpWorkbenchTransport: WorkbenchTransport = {
  projects: {
    list: () => getJson('/api/workbench/projects/list'),
    open: (path) => postJson('/api/workbench/projects/open', { path }),
  },
  worktrees: {
    list: (projectId) => postJson('/api/workbench/worktrees/list', { projectId }),
  },
  sessions: {
    list: (projectId) => postJson('/api/workbench/sessions/list', { projectId: projectId ?? null }),
    create: (projectId, initialSize, worktreeId) =>
      postJson('/api/workbench/sessions/create', {
        projectId,
        worktreeId: worktreeId ?? null,
        initialCols: initialSize?.cols ?? null,
        initialRows: initialSize?.rows ?? null,
      }),
    writeInput: (sessionId, data) => postJson('/api/workbench/sessions/write', { sessionId, data }),
    resize: (sessionId, cols, rows) => postJson('/api/workbench/sessions/resize', { sessionId, cols, rows }),
    replay: (sessionId) => postJson('/api/workbench/sessions/replay', { sessionId }),
  },
  files: {
    listDir: (projectId, path, worktreeId) =>
      postJson('/api/workbench/files/list-dir', { projectId, worktreeId: worktreeId ?? null, path: path ?? null }),
    info: (projectId, path, worktreeId) =>
      postJson('/api/workbench/files/info', { projectId, worktreeId: worktreeId ?? null, path }),
    open: (projectId, path, worktreeId) =>
      postJson('/api/workbench/files/open', { projectId, worktreeId: worktreeId ?? null, path }),
    saveText: (projectId, path, content, baseHash, worktreeId) =>
      postJson('/api/workbench/files/save-text', {
        projectId,
        worktreeId: worktreeId ?? null,
        path,
        content,
        baseHash,
      }),
  },
  git: {
    listCommits: (projectId, worktreeId, limit = 30) =>
      postJson('/api/workbench/git/commits', { projectId, worktreeId: worktreeId ?? null, limit }),
  },
};
```

Add to `web/src/lib/types.ts`:

```ts
export interface MobileAccessInfo {
  deviceName: string;
  port: number;
  urls: string[];
}
```

- [ ] **Step 4: Implement NDJSON event parser and hook**

Create `web/src/hooks/useWorkbenchHttpEvents.ts`:

```ts
import { useEffect } from 'react';
import type {
  WorkbenchMergeProgressEvent,
  WorkbenchTerminalOutputEvent,
  WorkbenchTerminalStatusEvent,
} from '@/lib/types';
import type { WorkbenchTerminalBufferStore } from './workbenchTerminalBuffer';

export interface WorkbenchNdjsonParserState {
  pending: string;
}

type WorkbenchRemoteEvent =
  | { type: 'terminalOutput'; payload: WorkbenchTerminalOutputEvent }
  | { type: 'terminalStatus'; payload: WorkbenchTerminalStatusEvent }
  | { type: 'mergeProgress'; payload: WorkbenchMergeProgressEvent };

/**
 * Business Logic（为什么需要这个函数）:
 *   `/api/workbench/events` 以 NDJSON 流式输出，中文字符和 JSON 行可能跨网络 chunk 拆开。
 *
 * Code Logic（这个函数做什么）:
 *   累积未完成行，只解析完整换行结尾的 JSON 行，返回已解析事件。
 */
export function parseWorkbenchNdjsonChunk(
  state: WorkbenchNdjsonParserState,
  chunk: string,
): WorkbenchRemoteEvent[] {
  state.pending += chunk;
  const lines = state.pending.split('\n');
  state.pending = lines.pop() ?? '';
  return lines
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
    .map((line) => JSON.parse(line) as WorkbenchRemoteEvent);
}

export interface UseWorkbenchHttpEventsOptions {
  store: WorkbenchTerminalBufferStore;
  enabled: boolean;
}

/**
 * Business Logic（为什么需要这个 hook）:
 *   移动端普通浏览器不能监听 Tauri event，需要直接读取 HTTP NDJSON 事件流缓存终端输出。
 *
 * Code Logic（这个 hook 做什么）:
 *   fetch `/api/workbench/events`，用 TextDecoder 解析 chunk，terminalOutput 写入 store；断开后重连。
 */
export function useWorkbenchHttpEvents({ store, enabled }: UseWorkbenchHttpEventsOptions): void {
  useEffect(() => {
    if (!enabled) return undefined;
    let cancelled = false;
    let abortController: AbortController | null = null;

    const connect = async () => {
      while (!cancelled) {
        abortController = new AbortController();
        try {
          const response = await fetch('/api/workbench/events', {
            signal: abortController.signal,
          });
          if (!response.body) throw new Error('事件流不可用');
          const reader = response.body.getReader();
          const decoder = new TextDecoder();
          const parserState: WorkbenchNdjsonParserState = { pending: '' };
          while (!cancelled) {
            const { value, done } = await reader.read();
            if (done) break;
            const text = decoder.decode(value, { stream: true });
            for (const event of parseWorkbenchNdjsonChunk(parserState, text)) {
              if (event.type === 'terminalOutput') {
                store.append(event.payload.sessionId, event.payload.chunk);
              }
            }
          }
        } catch {
          if (cancelled) return;
        }
        await new Promise((resolve) => window.setTimeout(resolve, 1200));
      }
    };

    void connect();
    return () => {
      cancelled = true;
      abortController?.abort();
    };
  }, [enabled, store]);
}
```

- [ ] **Step 5: Run frontend tests**

```bash
cd web
npx --yes tsx src/hooks/workbenchHttpEvents.test.ts
npm run build
```

Expected: test PASS and build succeeds.

- [ ] **Step 6: Commit**

```bash
git add web/src/api/workbenchTransport.ts web/src/api/workbenchHttp.ts web/src/api/mobile.ts web/src/hooks/useWorkbenchHttpEvents.ts web/src/hooks/workbenchHttpEvents.test.ts web/src/lib/types.ts
git commit -m "feat: add mobile workbench http transport"
```

---

## Task 5: Mobile SPA Build Entry and Shell

**Files:**
- Create: `web/mobile.html`
- Create: `web/src/mobile/main.tsx`
- Create: `web/src/mobile/MobileApp.tsx`
- Create: `web/src/mobile/MobileWorkbench.tsx`
- Create: `web/src/mobile/MobileWorkbench.module.css`
- Create: `web/src/mobile/mobileWorkbenchState.ts`
- Create: `web/src/mobile/mobileWorkbenchState.test.ts`
- Create: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Modify: `web/vite.config.ts`

- [ ] **Step 1: Write mobile state test**

Create `web/src/mobile/mobileWorkbenchState.test.ts`:

```ts
import assert from 'node:assert/strict';
import {
  closeMobileNav,
  openMobileNav,
  selectMobilePanel,
  type MobileWorkbenchPanel,
} from './mobileWorkbenchState';

let panel: MobileWorkbenchPanel = 'terminal';
panel = selectMobilePanel(panel, 'files');
assert.equal(panel, 'files');

assert.equal(openMobileNav(false), true);
assert.equal(closeMobileNav(true), false);
```

- [ ] **Step 2: Run failing test**

```bash
cd web
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
```

Expected: FAIL because the module does not exist.

- [ ] **Step 3: Add mobile state helpers**

Create `web/src/mobile/mobileWorkbenchState.ts`:

```ts
export type MobileWorkbenchPanel =
  | 'projects'
  | 'terminal'
  | 'files'
  | 'git'
  | 'worktrees'
  | 'prompt'
  | 'settings';

/**
 * Business Logic（为什么需要这个函数）:
 *   移动端抽屉点击功能项后需要切换主面板。
 *
 * Code Logic（这个函数做什么）:
 *   返回用户选择的目标面板；保留 current 参数便于测试同签名 reducer。
 */
export function selectMobilePanel(
  _current: MobileWorkbenchPanel,
  next: MobileWorkbenchPanel,
): MobileWorkbenchPanel {
  return next;
}

/** 打开移动端导航抽屉。 */
export function openMobileNav(_current: boolean): boolean {
  return true;
}

/** 关闭移动端导航抽屉。 */
export function closeMobileNav(_current: boolean): boolean {
  return false;
}
```

- [ ] **Step 4: Add Vite multipage entry**

Modify `web/vite.config.ts`:

```ts
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    sourcemap: true,
    rollupOptions: {
      input: {
        app: path.resolve(__dirname, 'index.html'),
        mobile: path.resolve(__dirname, 'mobile.html'),
      },
    },
  },
  css: {
    modules: {
      localsConvention: 'camelCase',
      generateScopedName: '[name]__[local]__[hash:base64:5]',
    },
  },
});
```

- [ ] **Step 5: Create mobile HTML and React entry**

Create `web/mobile.html`:

```html
<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover" />
    <title>cc-partner Mobile Workbench</title>
  </head>
  <body>
    <div id="mobile-root"></div>
    <script type="module" src="/src/mobile/main.tsx"></script>
  </body>
</html>
```

Create `web/src/mobile/main.tsx`:

```tsx
import React from 'react';
import ReactDOM from 'react-dom/client';
import '@/styles/tokens.css';
import '@/styles/reset.css';
import '@/styles/globals.css';
import '@/i18n';
import { MobileApp } from './MobileApp';

/**
 * Business Logic（为什么需要这个入口）:
 *   手机浏览器访问 `/mobile` 时需要加载独立于桌面 AppShell 的移动 Workbench SPA。
 *
 * Code Logic（这个入口做什么）:
 *   挂载 React root，加载全局 token/reset/i18n，并渲染 MobileApp。
 */
ReactDOM.createRoot(document.getElementById('mobile-root')!).render(
  <React.StrictMode>
    <MobileApp />
  </React.StrictMode>,
);
```

Create `web/src/mobile/MobileApp.tsx`:

```tsx
import { useState } from 'react';
import { createWorkbenchTerminalBufferStore } from '@/hooks/workbenchTerminalBuffer';
import { WorkbenchTerminalBuffersContext } from '@/hooks/workbenchTerminalBuffersContext';
import { useWorkbenchHttpEvents } from '@/hooks/useWorkbenchHttpEvents';
import { MobileWorkbench } from './MobileWorkbench';

/**
 * Business Logic（为什么需要这个组件）:
 *   移动端需要独立 Provider 树，不能依赖桌面 AppShell 和 Tauri event。
 *
 * Code Logic（这个组件做什么）:
 *   创建终端 buffer store，启动 HTTP NDJSON 事件订阅，并渲染 MobileWorkbench。
 */
export function MobileApp(): JSX.Element {
  const [store] = useState(() => createWorkbenchTerminalBufferStore());
  useWorkbenchHttpEvents({ store, enabled: true });

  return (
    <WorkbenchTerminalBuffersContext.Provider
      value={{
        store,
        resetBuffer: store.reset,
        removeBuffer: store.remove,
      }}
    >
      <MobileWorkbench />
    </WorkbenchTerminalBuffersContext.Provider>
  );
}
```

Create `web/src/mobile/MobileWorkbench.tsx`:

```tsx
import { useState } from 'react';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { MobileWorkbenchShell } from './components/MobileWorkbenchShell';
import styles from './MobileWorkbench.module.css';
import type { MobileWorkbenchPanel } from './mobileWorkbenchState';

/**
 * Business Logic（为什么需要这个组件）:
 *   移动端 Workbench 需要集中保存当前项目、worktree、window 和面板状态。
 *
 * Code Logic（这个组件做什么）:
 *   初始化移动端状态并按 active panel 渲染当前移动端容器，后续任务会接入具体业务面板。
 */
export function MobileWorkbench(): JSX.Element {
  const [panel, setPanel] = useState<MobileWorkbenchPanel>('projects');
  const [activeProject, setActiveProject] = useState<WorkbenchProject | null>(null);
  const [activeWorktree, setActiveWorktree] = useState<WorkbenchWorktree | null>(null);
  const [activeSession, setActiveSession] = useState<WorkbenchSession | null>(null);

  return (
    <MobileWorkbenchShell
      panel={panel}
      project={activeProject}
      worktree={activeWorktree}
      session={activeSession}
      onPanelChange={setPanel}
    >
      <section className={styles.panel}>
        <h1>Mobile Workbench</h1>
        <p>当前面板：{panel}</p>
      </section>
    </MobileWorkbenchShell>
  );
}
```

Create `web/src/mobile/components/MobileWorkbenchShell.tsx`:

```tsx
import type { ReactNode } from 'react';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';
import type { MobileWorkbenchPanel } from '../mobileWorkbenchState';

export interface MobileWorkbenchShellProps {
  panel: MobileWorkbenchPanel;
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  session: WorkbenchSession | null;
  onPanelChange: (panel: MobileWorkbenchPanel) => void;
  children: ReactNode;
}

const PANELS: Array<{ id: MobileWorkbenchPanel; label: string }> = [
  { id: 'projects', label: '项目' },
  { id: 'terminal', label: '终端' },
  { id: 'files', label: '文件' },
  { id: 'git', label: 'Git' },
  { id: 'worktrees', label: 'Worktree' },
  { id: 'prompt', label: 'Prompt' },
  { id: 'settings', label: '设置' },
];

/**
 * Business Logic（为什么需要这个组件）:
 *   手机竖屏需要顶部展开按钮和覆盖式抽屉，宽屏需要固定 Rail。
 *
 * Code Logic（这个组件做什么）:
 *   渲染统一 top bar、nav rail 和内容容器；具体业务面板由 children 提供。
 */
export function MobileWorkbenchShell({
  panel,
  project,
  worktree,
  session,
  onPanelChange,
  children,
}: MobileWorkbenchShellProps): JSX.Element {
  return (
    <div className={styles.shell}>
      <header className={styles.topbar}>
        <button className={styles.menuButton} type="button" aria-label="展开导航">
          ☰
        </button>
        <div className={styles.context}>
          <strong>{project?.name ?? '选择项目'}</strong>
          <span>{worktree?.name ?? '未选择 worktree'} · {session?.name ?? '无终端'}</span>
        </div>
      </header>
      <aside className={styles.rail}>
        {PANELS.map((item) => (
          <button
            key={item.id}
            className={item.id === panel ? styles.navActive : styles.navItem}
            type="button"
            onClick={() => onPanelChange(item.id)}
          >
            {item.label}
          </button>
        ))}
      </aside>
      <main className={styles.content}>{children}</main>
    </div>
  );
}
```

Create `web/src/mobile/MobileWorkbench.module.css`:

```css
.shell {
  min-height: 100vh;
  background: var(--bg);
  color: var(--fg);
}

.topbar {
  position: sticky;
  top: 0;
  z-index: var(--z-sticky);
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-3);
  border-bottom: 1px solid var(--border);
  background: var(--surface);
}

.menuButton {
  width: 36px;
  height: 36px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface-elevated);
  color: var(--fg);
  transition: all var(--motion-fast) var(--ease-standard);
}

.context {
  min-width: 0;
  display: grid;
  gap: var(--space-1);
}

.context strong,
.context span {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.context span {
  color: var(--fg-muted);
  font-size: var(--text-xs);
}

.rail {
  display: none;
}

.content {
  padding: var(--space-3);
}

.panel {
  display: grid;
  gap: var(--space-3);
}

.navItem,
.navActive {
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
  color: var(--fg);
  padding: var(--space-2);
  transition: all var(--motion-fast) var(--ease-standard);
}

.navActive {
  background: var(--accent);
  color: var(--accent-on);
}

@media (min-width: 820px) {
  .shell {
    display: grid;
    grid-template-columns: 112px minmax(0, 1fr);
    grid-template-rows: auto 1fr;
  }

  .topbar {
    grid-column: 1 / 3;
  }

  .menuButton {
    display: none;
  }

  .rail {
    display: grid;
    align-content: start;
    gap: var(--space-2);
    padding: var(--space-3);
    border-right: 1px solid var(--border);
    background: var(--surface);
  }
}
```

- [ ] **Step 6: Run tests and build**

```bash
cd web
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
npm run build
```

Expected: state test PASS; `dist/mobile.html` exists after build.

- [ ] **Step 7: Commit**

```bash
git add web/mobile.html web/src/mobile web/vite.config.ts
git commit -m "feat: add mobile workbench shell"
```

---

## Task 6: Mobile Project, Worktree, and Session Loading

**Files:**
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Create: `web/src/mobile/components/MobileProjectPanel.tsx`
- Create: `web/src/mobile/components/MobileWorktreePanel.tsx`

- [ ] **Step 1: Add HTTP projects list route**

In `src-tauri/src/net/routes/workbench.rs`, add:

```rust
/// Business Logic（为什么需要这个函数）:
///     移动端普通浏览器不能使用 Tauri invoke，需要通过 HTTP 列出最近 Workbench 项目。
///
/// Code Logic（这个函数做什么）:
///     调用本机项目仓库，按现有 list_workbench_projects 语义返回项目 DTO。
pub async fn list_projects(State(state): State<AppState>) -> Result<Json<Vec<WorkbenchProjectDto>>, AppError> {
    Ok(Json(state.workbench_project_repo.list().await?))
}
```

Use `state.workbench_project_repo.list().await?` and map rows with `WorkbenchProjectRow::to_dto`, matching `commands/workbench.rs::list_workbench_projects`.

In `src-tauri/src/net/http_server.rs` add:

```rust
.route("/api/workbench/projects/list", get(workbench::list_projects))
```

- [ ] **Step 2: Add mobile project panel**

Create `web/src/mobile/components/MobileProjectPanel.tsx`:

```tsx
import type { WorkbenchProject } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileProjectPanelProps {
  projects: WorkbenchProject[];
  activeProjectId: string | null;
  loading: boolean;
  error: string | null;
  onSelect: (project: WorkbenchProject) => void;
  onRefresh: () => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   手机进入 `/mobile` 后需要选择最近项目作为 Workbench 上下文。
 *
 * Code Logic（这个组件做什么）:
 *   渲染项目列表、刷新按钮、加载态和错误态；点击项目交给父组件加载 worktree/session。
 */
export function MobileProjectPanel({
  projects,
  activeProjectId,
  loading,
  error,
  onSelect,
  onRefresh,
}: MobileProjectPanelProps): JSX.Element {
  return (
    <section className={styles.panel}>
      <div className={styles.panelHeader}>
        <h1>项目</h1>
        <button type="button" onClick={onRefresh}>刷新</button>
      </div>
      {loading && <p>加载中...</p>}
      {error && <p>{error}</p>}
      {projects.map((project) => (
        <button
          key={project.id}
          type="button"
          className={project.id === activeProjectId ? styles.navActive : styles.navItem}
          onClick={() => onSelect(project)}
        >
          <strong>{project.name}</strong>
          <span>{project.path}</span>
        </button>
      ))}
    </section>
  );
}
```

Create `web/src/mobile/components/MobileWorktreePanel.tsx`:

```tsx
import type { WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileWorktreePanelProps {
  worktrees: WorkbenchWorktree[];
  activeWorktreeId: string | null;
  onSelect: (worktree: WorkbenchWorktree) => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   移动端需要切换 active worktree，驱动终端、文件和 Git 上下文同步变化。
 *
 * Code Logic（这个组件做什么）:
 *   渲染 worktree 列表，并把选择事件交给父组件。
 */
export function MobileWorktreePanel({
  worktrees,
  activeWorktreeId,
  onSelect,
}: MobileWorktreePanelProps): JSX.Element {
  return (
    <section className={styles.panel}>
      <h1>Worktree</h1>
      {worktrees.map((worktree) => (
        <button
          key={worktree.id}
          type="button"
          className={worktree.id === activeWorktreeId ? styles.navActive : styles.navItem}
          onClick={() => onSelect(worktree)}
        >
          <strong>{worktree.name}</strong>
          <span>{worktree.branch}</span>
        </button>
      ))}
    </section>
  );
}
```

- [ ] **Step 3: Wire mobile loading in `MobileWorkbench.tsx`**

Replace the component body in `web/src/mobile/MobileWorkbench.tsx` with:

```tsx
const [panel, setPanel] = useState<MobileWorkbenchPanel>('projects');
const [projects, setProjects] = useState<WorkbenchProject[]>([]);
const [activeProject, setActiveProject] = useState<WorkbenchProject | null>(null);
const [worktrees, setWorktrees] = useState<WorkbenchWorktree[]>([]);
const [activeWorktree, setActiveWorktree] = useState<WorkbenchWorktree | null>(null);
const [sessions, setSessions] = useState<WorkbenchSession[]>([]);
const [activeSession, setActiveSession] = useState<WorkbenchSession | null>(null);
const [loading, setLoading] = useState(false);
const [error, setError] = useState<string | null>(null);

const loadProjects = useCallback(async () => {
  try {
    setLoading(true);
    setError(null);
    setProjects(await httpWorkbenchTransport.projects.list());
  } catch (reason) {
    setError(reason instanceof Error ? reason.message : String(reason));
  } finally {
    setLoading(false);
  }
}, []);

const selectProject = useCallback(async (project: WorkbenchProject) => {
  try {
    setLoading(true);
    setError(null);
    setActiveProject(project);
    const nextWorktrees = await httpWorkbenchTransport.worktrees.list(project.id);
    setWorktrees(nextWorktrees);
    setActiveWorktree(nextWorktrees[0] ?? null);
    const nextSessions = await httpWorkbenchTransport.sessions.list(project.id);
    setSessions(nextSessions);
    setActiveSession(nextSessions[0] ?? null);
    setPanel('terminal');
  } catch (reason) {
    setError(reason instanceof Error ? reason.message : String(reason));
  } finally {
    setLoading(false);
  }
}, []);

useEffect(() => {
  void loadProjects();
}, [loadProjects]);
```

Add imports for `useCallback`, `useEffect`, `MobileProjectPanel`, and `MobileWorktreePanel`.

Render by panel:

```tsx
const content =
  panel === 'projects' ? (
    <MobileProjectPanel
      projects={projects}
      activeProjectId={activeProject?.id ?? null}
      loading={loading}
      error={error}
      onSelect={selectProject}
      onRefresh={loadProjects}
    />
  ) : panel === 'worktrees' ? (
    <MobileWorktreePanel
      worktrees={worktrees}
      activeWorktreeId={activeWorktree?.id ?? null}
      onSelect={setActiveWorktree}
    />
  ) : (
    <section className={styles.panel}>
      <h1>{panel}</h1>
      {error && <p>{error}</p>}
    </section>
  );
```

- [ ] **Step 4: Run focused checks**

```bash
cd src-tauri
cargo test net::routes::workbench --lib
cargo check
cd ../web
npm run build
```

Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/routes/workbench.rs src-tauri/src/net/http_server.rs web/src/mobile
git commit -m "feat: load mobile workbench projects"
```

---

## Task 7: Mobile Terminal Panel with tmux Window/Pane Controls

**Files:**
- Create: `web/src/mobile/components/MobileTerminalPanel.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Reuse: `web/src/pages/Workbench/terminalReplay.ts`
- Reuse: `web/src/hooks/workbenchTerminalBuffersContext.ts`

- [ ] **Step 1: Create terminal panel component**

Create `web/src/mobile/components/MobileTerminalPanel.tsx`:

```tsx
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import type { WorkbenchProject, WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import { useWorkbenchTerminalBuffer } from '@/hooks/workbenchTerminalBuffersContext';
import {
  planTerminalBufferWrite,
  shouldForwardTerminalInput,
  writeTerminalReplay,
} from '@/pages/Workbench/terminalReplay';
import styles from '../MobileWorkbench.module.css';

export interface MobileTerminalPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
  sessions: WorkbenchSession[];
  activeSession: WorkbenchSession | null;
  onSessionsChange: (sessions: WorkbenchSession[]) => void;
  onActiveSessionChange: (session: WorkbenchSession | null) => void;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   移动端需要操作与 PC 端同一套 tmux-backed terminal window/pane。
 *
 * Code Logic（这个组件做什么）:
 *   挂载 xterm，先请求后端 replay，再消费 terminal buffer 增量；输入、resize、split/close pane 走 HTTP API。
 */
export function MobileTerminalPanel({
  project,
  worktree,
  sessions,
  activeSession,
  onSessionsChange,
  onActiveSessionChange,
}: MobileTerminalPanelProps): JSX.Element {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const replayGateRef = useRef(false);
  const writtenBufferRef = useRef('');
  const { buffer, revision } = useWorkbenchTerminalBuffer(activeSession?.id ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const createSession = useCallback(async () => {
    if (!project) return;
    setBusy(true);
    try {
      const session = await httpWorkbenchTransport.sessions.create(
        project.id,
        { cols: 80, rows: 24 },
        worktree?.id ?? null,
      );
      const next = [...sessions, session];
      onSessionsChange(next);
      onActiveSessionChange(session);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  }, [project, sessions, worktree, onActiveSessionChange, onSessionsChange]);

  useEffect(() => {
    if (!activeSession || !hostRef.current) return undefined;
    const terminal = new Terminal({ cursorBlink: true, convertEol: false });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(hostRef.current);
    fit.fit();
    terminalRef.current = terminal;
    fitRef.current = fit;

    let disposed = false;
    void httpWorkbenchTransport.sessions.replay(activeSession.id).then((replay) => {
      if (disposed) return;
      writeTerminalReplay(terminal, replay.buffer, replayGateRef);
      writtenBufferRef.current = replay.buffer;
    });
    const disposable = terminal.onData((data) => {
      if (!shouldForwardTerminalInput(replayGateRef, true)) return;
      void httpWorkbenchTransport.sessions.writeInput(activeSession.id, data);
    });
    return () => {
      disposed = true;
      disposable.dispose();
      terminal.dispose();
      terminalRef.current = null;
      fitRef.current = null;
    };
  }, [activeSession?.id]);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) return;
    const plan = planTerminalBufferWrite(writtenBufferRef.current, buffer);
    if (plan.mode === 'append') {
      terminal.write(plan.data);
      writtenBufferRef.current = buffer;
    } else if (plan.mode === 'replay') {
      terminal.clear();
      writeTerminalReplay(terminal, plan.data, replayGateRef);
      writtenBufferRef.current = buffer;
    }
  }, [buffer, revision]);

  return (
    <section className={styles.panel}>
      <div className={styles.panelHeader}>
        <h1>终端</h1>
        <button type="button" disabled={!project || busy} onClick={createSession}>新窗口</button>
      </div>
      {error && <p>{error}</p>}
      <div className={styles.sessionTabs}>
        {sessions.map((session) => (
          <button
            key={session.id}
            type="button"
            className={session.id === activeSession?.id ? styles.navActive : styles.navItem}
            onClick={() => onActiveSessionChange(session)}
          >
            {session.name}
          </button>
        ))}
      </div>
      <div className={styles.terminalViewport} ref={hostRef} />
      <div className={styles.panelActions}>
        <button type="button" disabled={!activeSession} onClick={() => activeSession && httpWorkbenchTransport.sessions.resize(activeSession.id, 80, 24)}>适配</button>
        <button type="button" disabled={!activeSession} onClick={() => activeSession && httpWorkbenchTransport.sessions.splitPane(activeSession.id, 'right')}>左右分屏</button>
        <button type="button" disabled={!activeSession} onClick={() => activeSession && httpWorkbenchTransport.sessions.splitPane(activeSession.id, 'down')}>上下分屏</button>
      </div>
    </section>
  );
}
```

Add missing `splitPane` to `WorkbenchTransport.sessions` and `httpWorkbenchTransport.sessions`:

```ts
splitPane: (sessionId: string, direction: 'right' | 'down') => Promise<{ ok: boolean; sessionId: string; direction: 'right' | 'down' }>;
```

HTTP implementation:

```ts
splitPane: (sessionId, direction) => postJson('/api/workbench/sessions/split-pane', { sessionId, direction }),
```

- [ ] **Step 2: Wire terminal panel into MobileWorkbench**

In `MobileWorkbench.tsx`, render `MobileTerminalPanel` for `panel === 'terminal'` and pass `sessions`, `activeSession`, setters, `activeProject`, `activeWorktree`.

- [ ] **Step 3: Add CSS for terminal**

Append to `web/src/mobile/MobileWorkbench.module.css`:

```css
.panelHeader,
.panelActions,
.sessionTabs {
  display: flex;
  gap: var(--space-2);
  align-items: center;
  flex-wrap: wrap;
}

.terminalViewport {
  min-height: 58vh;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  overflow: hidden;
  background: var(--terminal-bg);
}
```

- [ ] **Step 4: Run build**

```bash
cd web
npm run build
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/mobile web/src/api/workbenchTransport.ts web/src/api/workbenchHttp.ts
git commit -m "feat: add mobile terminal panel"
```

---

## Task 8: Mobile Files, Git, Worktree, and Prompt Panels

**Files:**
- Create: `web/src/mobile/components/MobileFilesPanel.tsx`
- Create: `web/src/mobile/components/MobileGitPanel.tsx`
- Create: `web/src/mobile/components/MobilePromptPanel.tsx`
- Modify: `web/src/mobile/components/MobileWorktreePanel.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/api/workbenchTransport.ts`
- Modify: `web/src/api/workbenchHttp.ts`

- [ ] **Step 1: Extend transport for required operations**

Add to `WorkbenchTransport`:

```ts
worktrees: {
  list: (projectId: string) => Promise<WorkbenchWorktree[]>;
  create: (projectId: string, branchName: string, baseBranch?: string | null) => Promise<WorkbenchWorktree>;
  commit: (worktreeId: string, message?: string | null) => Promise<WorkbenchWorktree>;
  push: (worktreeId: string) => Promise<WorkbenchWorktree>;
  merge: (worktreeId: string) => Promise<WorkbenchMergeResult>;
  remove: (worktreeId: string, force?: boolean) => Promise<{ ok: boolean; worktreeId: string }>;
};
prompt: {
  streamToTerminal: (prompt: string, options: { workingDirectory?: string; targetLanguage: 'zh' | 'en'; sessionId: string }) => Promise<{ ok: boolean; sessionId: string }>;
};
```

Map these to existing HTTP routes in `workbenchHttp.ts`.

- [ ] **Step 2: Create files panel**

Create `MobileFilesPanel.tsx` with tree loading and open/save:

```tsx
import { useCallback, useState } from 'react';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchFileNode, WorkbenchOpenFile, WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileFilesPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   手机端需要浏览 active worktree 文件树，并快速打开/保存文本文件。
 *
 * Code Logic（这个组件做什么）:
 *   加载当前目录子节点，点击文件后调用 open，文本内容用 textarea 第一版编辑并通过 baseHash 保存。
 */
export function MobileFilesPanel({ project, worktree }: MobileFilesPanelProps): JSX.Element {
  const [nodes, setNodes] = useState<WorkbenchFileNode[]>([]);
  const [opened, setOpened] = useState<WorkbenchOpenFile | null>(null);
  const [content, setContent] = useState('');
  const [error, setError] = useState<string | null>(null);

  const loadRoot = useCallback(async () => {
    if (!project) return;
    try {
      setNodes(await httpWorkbenchTransport.files.listDir(project.id, '', worktree?.id ?? null));
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [project, worktree]);

  const openFile = useCallback(async (path: string) => {
    if (!project) return;
    const file = await httpWorkbenchTransport.files.open(project.id, path, worktree?.id ?? null);
    setOpened(file);
    setContent(file.text?.content ?? '');
  }, [project, worktree]);

  const saveFile = useCallback(async () => {
    if (!project || !opened?.text) return;
    await httpWorkbenchTransport.files.saveText(
      project.id,
      opened.metadata.path,
      content,
      opened.text.baseHash,
      worktree?.id ?? null,
    );
  }, [content, opened, project, worktree]);

  return (
    <section className={styles.panel}>
      <div className={styles.panelHeader}>
        <h1>文件</h1>
        <button type="button" onClick={loadRoot}>刷新</button>
        <button type="button" disabled={!opened?.text} onClick={saveFile}>保存</button>
      </div>
      {error && <p>{error}</p>}
      <div className={styles.fileLayout}>
        <div>
          {nodes.map((node) => (
            <button key={node.path} type="button" className={styles.navItem} onClick={() => !node.isDir && openFile(node.path)}>
              {node.name}
            </button>
          ))}
        </div>
        {opened?.text && (
          <textarea className={styles.mobileEditor} value={content} onChange={(event) => setContent(event.target.value)} />
        )}
      </div>
    </section>
  );
}
```

- [ ] **Step 3: Create Git panel**

Create `MobileGitPanel.tsx`:

```tsx
import { useCallback, useState } from 'react';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchGitCommit, WorkbenchProject, WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobileGitPanelProps {
  project: WorkbenchProject | null;
  worktree: WorkbenchWorktree | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   手机端需要查看 active worktree Git 历史并执行 commit/push/merge。
 *
 * Code Logic（这个组件做什么）:
 *   读取提交列表，调用 worktree commit/push/merge HTTP API，并显示错误。
 */
export function MobileGitPanel({ project, worktree }: MobileGitPanelProps): JSX.Element {
  const [commits, setCommits] = useState<WorkbenchGitCommit[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!project) return;
    setCommits(await httpWorkbenchTransport.git.listCommits(project.id, worktree?.id ?? null, 30));
  }, [project, worktree]);

  const commit = useCallback(async () => {
    if (!worktree) return;
    await httpWorkbenchTransport.worktrees.commit(worktree.id, null);
    await load();
  }, [load, worktree]);

  return (
    <section className={styles.panel}>
      <div className={styles.panelHeader}>
        <h1>Git</h1>
        <button type="button" onClick={load}>刷新</button>
        <button type="button" disabled={!worktree} onClick={commit}>Commit</button>
      </div>
      {error && <p>{error}</p>}
      {commits.map((commit) => (
        <article key={commit.hash} className={styles.navItem}>
          <strong>{commit.subject}</strong>
          <span>{commit.shortHash}</span>
        </article>
      ))}
    </section>
  );
}
```

- [ ] **Step 4: Create Prompt panel**

Create `MobilePromptPanel.tsx`:

```tsx
import { useState } from 'react';
import { httpWorkbenchTransport } from '@/api/workbenchHttp';
import type { WorkbenchSession, WorkbenchWorktree } from '@/lib/types';
import styles from '../MobileWorkbench.module.css';

export interface MobilePromptPanelProps {
  worktree: WorkbenchWorktree | null;
  session: WorkbenchSession | null;
}

/**
 * Business Logic（为什么需要这个组件）:
 *   手机端没有桌面快捷键，需要显式输入 Prompt 并流式写入当前终端。
 *
 * Code Logic（这个组件做什么）:
 *   收集原始 Prompt 和目标语言，调用 stream-to-session HTTP API。
 */
export function MobilePromptPanel({ worktree, session }: MobilePromptPanelProps): JSX.Element {
  const [prompt, setPrompt] = useState('');
  const [targetLanguage, setTargetLanguage] = useState<'zh' | 'en'>('zh');
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    if (!session || !prompt.trim()) return;
    try {
      await httpWorkbenchTransport.prompt.streamToTerminal(prompt, {
        workingDirectory: worktree?.path,
        targetLanguage,
        sessionId: session.id,
      });
      setPrompt('');
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  return (
    <section className={styles.panel}>
      <h1>Prompt 优化</h1>
      {error && <p>{error}</p>}
      <select value={targetLanguage} onChange={(event) => setTargetLanguage(event.target.value as 'zh' | 'en')}>
        <option value="zh">中文</option>
        <option value="en">English</option>
      </select>
      <textarea className={styles.mobileEditor} value={prompt} onChange={(event) => setPrompt(event.target.value)} />
      <button type="button" disabled={!session || !prompt.trim()} onClick={submit}>写入当前终端</button>
    </section>
  );
}
```

- [ ] **Step 5: Wire panels in `MobileWorkbench.tsx` and CSS**

Render `MobileFilesPanel`, `MobileGitPanel`, `MobileWorktreePanel`, `MobilePromptPanel` for their panel IDs.

Add CSS:

```css
.fileLayout {
  display: grid;
  gap: var(--space-3);
}

.mobileEditor {
  min-height: 50vh;
  width: 100%;
  resize: vertical;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  padding: var(--space-3);
  background: var(--surface);
  color: var(--fg);
  font: inherit;
}
```

- [ ] **Step 6: Run checks**

```bash
cd web
npm run build
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add web/src/mobile web/src/api/workbenchTransport.ts web/src/api/workbenchHttp.ts
git commit -m "feat: add mobile workbench panels"
```

---

## Task 9: Desktop Mobile Access Card and QR

**Files:**
- Modify: `web/package.json`
- Modify: `web/package-lock.json`
- Create: `web/src/components/domain/MobileAccessCard/MobileAccessCard.tsx`
- Create: `web/src/components/domain/MobileAccessCard/MobileAccessCard.module.css`
- Create: `web/src/components/domain/MobileAccessCard/index.ts`
- Create: `web/src/components/domain/MobileAccessCard/mobileQr.ts`
- Create: `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/i18n/locales/zh/settings.json`
- Modify: `web/src/i18n/locales/en/settings.json`

- [ ] **Step 1: Install QR dependency**

```bash
cd web
npm install qrcode @types/qrcode
```

Expected: `package.json` and `package-lock.json` updated.

- [ ] **Step 2: Create URL selection test**

Create `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`:

```ts
import assert from 'node:assert/strict';
import { selectPrimaryMobileUrl } from './mobileQr';

assert.equal(
  selectPrimaryMobileUrl(['http://192.168.1.23:51842/mobile']),
  'http://192.168.1.23:51842/mobile',
);
assert.equal(selectPrimaryMobileUrl([]), null);
```

- [ ] **Step 3: Implement QR helpers**

Create `mobileQr.ts`:

```ts
import QRCode from 'qrcode';

/**
 * Business Logic（为什么需要这个函数）:
 *   桌面端可能拿到多个局域网 URL，二维码区域需要选择一个主 URL。
 *
 * Code Logic（这个函数做什么）:
 *   返回第一个 URL；没有 URL 时返回 null。
 */
export function selectPrimaryMobileUrl(urls: string[]): string | null {
  return urls[0] ?? null;
}

/**
 * Business Logic（为什么需要这个函数）:
 *   用户希望手机扫码打开移动 Workbench，前端需要把 URL 渲染成二维码。
 *
 * Code Logic（这个函数做什么）:
 *   调用 qrcode 库生成 SVG 字符串。
 */
export async function renderMobileQrSvg(url: string): Promise<string> {
  return QRCode.toString(url, {
    type: 'svg',
    margin: 1,
    width: 180,
  });
}
```

- [ ] **Step 4: Build MobileAccessCard**

Create `MobileAccessCard.tsx`:

```tsx
import { useEffect, useMemo, useState } from 'react';
import { getMobileAccessInfo } from '@/api/mobile';
import type { MobileAccessInfo } from '@/lib/types';
import { renderMobileQrSvg, selectPrimaryMobileUrl } from './mobileQr';
import styles from './MobileAccessCard.module.css';

/**
 * Business Logic（为什么需要这个组件）:
 *   用户需要在桌面端看到手机访问链接和二维码，才能从局域网移动设备打开 Workbench。
 *
 * Code Logic（这个组件做什么）:
 *   请求 access-info，选择主 URL，生成二维码 SVG，并提供复制按钮和无鉴权风险说明。
 */
export function MobileAccessCard(): JSX.Element {
  const [info, setInfo] = useState<MobileAccessInfo | null>(null);
  const [qrSvg, setQrSvg] = useState<string>('');
  const [error, setError] = useState<string | null>(null);
  const primaryUrl = useMemo(() => selectPrimaryMobileUrl(info?.urls ?? []), [info]);

  useEffect(() => {
    let cancelled = false;
    void getMobileAccessInfo()
      .then((next) => {
        if (!cancelled) setInfo(next);
      })
      .catch((reason) => {
        if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!primaryUrl) return;
    let cancelled = false;
    void renderMobileQrSvg(primaryUrl).then((svg) => {
      if (!cancelled) setQrSvg(svg);
    });
    return () => {
      cancelled = true;
    };
  }, [primaryUrl]);

  const copy = async () => {
    if (!primaryUrl) return;
    await navigator.clipboard.writeText(primaryUrl);
  };

  return (
    <section className={styles.card}>
      <div>
        <h3>移动访问</h3>
        <p>同一局域网设备可直接访问；无鉴权，可执行终端输入、文件修改和 Git 操作。</p>
      </div>
      {error && <p>{error}</p>}
      {primaryUrl ? (
        <>
          <code className={styles.url}>{primaryUrl}</code>
          <button type="button" onClick={copy}>复制链接</button>
          <div className={styles.qr} dangerouslySetInnerHTML={{ __html: qrSvg }} />
        </>
      ) : (
        <p>未检测到可供手机访问的局域网地址。</p>
      )}
    </section>
  );
}
```

Create CSS using tokens only:

```css
.card {
  display: grid;
  gap: var(--space-3);
  padding: var(--space-4);
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface);
}

.url {
  display: block;
  overflow-wrap: anywhere;
  padding: var(--space-2);
  border-radius: var(--radius-sm);
  background: var(--surface-muted);
  color: var(--fg);
}

.qr {
  width: 180px;
  min-height: 180px;
  border: 1px solid var(--border);
  border-radius: var(--radius-md);
  background: var(--surface-elevated);
  padding: var(--space-2);
}
```

Create `index.ts`:

```ts
export { MobileAccessCard } from './MobileAccessCard';
```

- [ ] **Step 5: Add to Settings and Workbench**

Import and render `MobileAccessCard` in `Settings.tsx` in the most relevant settings section.

In `Workbench.tsx`, render a compact `MobileAccessCard` in a non-disruptive area such as an inspector/settings panel. Do not place it inside terminal viewport or file editor.

- [ ] **Step 6: Run checks**

```bash
cd web
npx --yes tsx src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
npm run build
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add web/package.json web/package-lock.json web/src/components/domain/MobileAccessCard web/src/pages/Settings/Settings.tsx web/src/pages/Workbench/Workbench.tsx web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json
git commit -m "feat: show mobile access qr"
```

---

## Task 10: Documentation, PRD, and Final Verification

**Files:**
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/prd.md`

- [ ] **Step 1: Update root AGENTS.md**

Add only concise project-level memory:

```md
- **移动端远程 Workbench** — axum 默认提供 `/mobile` 局域网移动端 SPA；手机端通过 HTTP Workbench API 和 NDJSON 事件流访问同一套 tmux-backed Workbench。
```

Keep root file concise; do not paste implementation history.

- [ ] **Step 2: Update web/CLAUDE.md**

Add a focused section:

```md
- **Mobile Workbench SPA**: `web/mobile.html` + `src/mobile/` 是手机浏览器入口，由 axum `/mobile` 服务。它不使用 Tauri `invoke()`，而是通过 `workbenchHttp.ts` 调 `/api/workbench/...`，通过 `useWorkbenchHttpEvents` 读取 `/api/workbench/events`。终端仍复用后端 tmux session/window/pane；pane 由 tmux 在 xterm 内渲染。
- **Mobile 验证命令**: `npx --yes tsx src/mobile/mobileWorkbenchState.test.ts && npx --yes tsx src/hooks/workbenchHttpEvents.test.ts && npm run build`。
```

- [ ] **Step 3: Update src-tauri/CLAUDE.md**

Add concise backend memory:

```md
- **移动端 Workbench HTTP 入口**: axum 默认服务 `/mobile` SPA 和 `/api/mobile/access-info`。移动端无 token，面向个人可信局域网；二维码 URL 必须使用 LAN IP 而不是 localhost。Workbench HTTP route 继续复用本机/远端项目 helper，终端仍是现有 tmux-backed session/window/pane 模型。
- **Session replay**: `WorkbenchSessionRegistry` 维护每个 session 的有限输出 replay buffer；`/api/workbench/sessions/replay` 供移动端首次进入终端时拉取历史输出，再接 `/api/workbench/events` 的 NDJSON 增量。
```

- [ ] **Step 4: Update docs/prd.md**

Add product requirement under Workbench section:

```md
### 局域网移动端 Workbench

cc-partner 默认在本机 axum HTTP server 上提供 `/mobile` 移动端 Workbench。用户可在桌面端查看局域网访问链接和二维码，用手机浏览器访问同一套项目、worktree、terminal window/pane、文件、Git 和 Prompt 优化能力。移动端不使用 token，面向个人可信局域网；终端层继续复用 PC 端 tmux 机制，window 内多个 pane 由 tmux 在 xterm 中渲染。
```

- [ ] **Step 5: Run full targeted verification**

```bash
cd src-tauri
cargo test mobile --lib
cargo test net::routes::workbench --lib
cargo test workbench::sessions --lib
cargo check
cd ../web
npx --yes tsx src/hooks/workbenchHttpEvents.test.ts
npx --yes tsx src/mobile/mobileWorkbenchState.test.ts
npx --yes tsx src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
npm run build
```

Expected: all pass.

- [ ] **Step 6: Manual smoke test**

Run:

```bash
cd /Users/hans/web_project/cc-partner-mobile-workbench
./web/node_modules/.bin/tauri dev
```

Manual checks:

1. Desktop settings shows mobile URL and QR.
2. Browser opens `http://<LAN IP>:<port>/mobile`.
3. Mobile project list loads.
4. Select project and worktree.
5. Create terminal window.
6. Terminal replay shows initial output after reload.
7. Type `pwd` and see output.
8. Split pane right/down and confirm tmux renders panes inside xterm.
9. Open and save a small text file.
10. Open Git panel and load commits.
11. Prompt panel writes optimized prompt into current terminal.

- [ ] **Step 7: Commit docs and verification fixes**

```bash
git add AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md docs/prd.md
git commit -m "docs: document mobile workbench"
```

---

## Self-Review Checklist

- Spec coverage:
  - `/mobile` SPA: Tasks 2 and 5.
  - Default-on no token: Tasks 1, 2, 9, 10.
  - Access link and QR: Task 9.
  - HTTP transport: Task 4.
  - NDJSON events: Task 4.
  - tmux/session/window/pane reuse: Tasks 3, 7, 10.
  - Backend replay: Task 3.
  - Worktree/window/pane display model: Tasks 6 and 7.
  - Files/Git/Worktree/Prompt panels: Task 8.
  - Docs and PRD: Task 10.
- Completion scan:
  - No unresolved markers or incomplete steps remain.
- Type consistency:
  - `MobileAccessInfo`, `WorkbenchSessionReplay`, `WorkbenchTransport`, and panel IDs are introduced before use.
