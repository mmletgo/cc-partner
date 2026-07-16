# Browser Verification Surface Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在project owning device按需运行隔离的managed Chromium，自动产生snapshot、交互、console和screenshot evidence，同时保持现有iframe sandbox与loopback边界。

**Architecture:** `BrowserVerificationService`只接受live preview ID并从registry解析target；engine使用固定Chrome for Testing headless shell和临时profile，每个run有持久到TTL的状态/artifact。local/remote/mobile/CLI共享owner helper，Orchestrator通过中立evidence adapter接入。

**Tech Stack:** Rust 2021, `chromiumoxide = 0.7.0` with Tokio, Chrome for Testing headless-shell `150.0.7871.114`, Tauri/axum, React 19, existing browser proxy/preview.

## Global Constraints

- Engine只在owning device启动，按需child、空闲60秒退出；不是第二个daemon。
- start只接受`previewId`，不得接受target URL/CDP endpoint。
- preview iframe继续没有`allow-same-origin`；不注入高权限bridge。
- redirect必须持续限制loopback HTTP(S)+显式port。
- 不导入cookie/history/profile/extension，不提供arbitrary JavaScript eval。
- snapshot 5,000 nodes/2 MiB；screenshot 8 MiB；console 1,000 entries/1 MiB。
- click/fill只接受当前generation的opaque nodeRef；non-replayable mutation不盲重放。
- P2P business API继续无调用者身份鉴权；preview/session/node opaque ID不是认证token，仍须应用既有LAN guard与资源上限。
- UI复用现有Browser Workspace、tokens和primitives；默认一键自动验证，不要求用户写脚本或选元素。

---

## File Structure

- Create: `src-tauri/src/workbench/browser_verification/{mod.rs,models.rs,engine.rs,chromium.rs,runtime.rs,artifact_store.rs}`。
- Create: `scripts/prepare-browser-runtime.mjs`, `scripts/browser-runtime-lock.json`。
- Modify: `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, release/CI workflows and sidecar preparation scripts.
- Create: `src-tauri/src/commands/workbench/browser_verification.rs`。
- Create: `src-tauri/src/net/routes/browser_verification.rs`。
- Modify: `src-tauri/src/workbench/{browser.rs,browser_proxy.rs,remote_client.rs,remote_ids.rs}`、`src-tauri/src/backend/{control_workbench.rs,control_client.rs}`、`src-tauri/src/net/routes/{mod.rs,workbench.rs}`、`src-tauri/src/net/{http_server.rs,protocol.rs,discovery.rs}`。
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/{WorkbenchBrowserVerificationPanel.tsx,WorkbenchBrowserVerificationPanel.module.css,WorkbenchBrowserVerificationPanel.test.tsx,workbenchBrowserVerification.ts,workbenchBrowserVerification.test.ts}`。
- Create: `src-tauri/tests/browser_verification_smoke.rs`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → T5 → T6 → T7
```

### Task 1: Define Bounded DTOs and Engine Seam

**Files:**
- Create: `src-tauri/src/workbench/browser_verification/{mod.rs,models.rs,engine.rs}`
- Modify: `src-tauri/src/workbench/mod.rs`

**Interfaces:**
- Produces: verification session/command/evidence DTOs and `BrowserVerificationEngine`.

- [ ] **Step 1: Write resource/redaction tests**

```rust
#[test]
fn fill_result_never_serializes_input_value() {
    let result = BrowserCommandResult::filled("node-1", 12);
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("secret value"));
    assert!(!json.contains("value"));
}
```

Add 5,001 nodes, 2 MiB snapshot, 64 KiB fill, timeout 99/30,001 and console query/header redaction cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::browser_verification::models --lib`

Expected: FAIL because module is absent.

- [ ] **Step 3: Implement exact seam**

```rust
pub trait BrowserVerificationEngine: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: EngineRunRequest,
        observer: Arc<dyn BrowserVerificationObserver>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<EngineRunResult, AppError>>;
}
```

Use command enum `Snapshot/Click/Fill/WaitFor/Screenshot/ReadConsole`; snapshot refs are opaque and generation-bound. No arbitrary selector/eval field.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench::browser_verification::models --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/browser_verification src-tauri/src/workbench/mod.rs
git commit -m "feat(workbench): define browser verification engine"
```

### Task 2: Pin and Package Managed Chrome for Testing

**Files:**
- Create: `scripts/prepare-browser-runtime.mjs`
- Create: `scripts/browser-runtime-lock.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `scripts/prepare-tauri-sidecar.mjs`
- Modify: `.github/workflows/{ci.yml,cross-platform-smoke.yml,release-tauri.yml}`

**Interfaces:**
- Produces: verified executable path for `linux64/mac-arm64/mac-x64/win64` and Cargo CDP dependency.

- [ ] **Step 1: Write lock/prepare script self-tests**

```js
assert.equal(resolvePlatform('darwin', 'arm64'), 'mac-arm64')
assert.equal(resolvePlatform('darwin', 'x64'), 'mac-x64')
assert.equal(resolvePlatform('win32', 'x64'), 'win64')
assert.equal(resolvePlatform('linux', 'x64'), 'linux64')
assert.throws(() => verifySha256(Buffer.from('bad'), '0'.repeat(64)))
```

- [ ] **Step 2: Run RED**

Run: `node scripts/prepare-browser-runtime.mjs --self-test`

Expected: FAIL because script/lock are absent.

- [ ] **Step 3: Implement pinned asset preparation**

Lock version `150.0.7871.114` and official `chrome-headless-shell` URLs. `--write-lock --all-platforms` downloads the four supported target assets and writes real 64-hex SHA-256 values in one deterministic lock-generation operation; normal mode refuses missing/mismatched hash, zip traversal and unexpected executable layout. Add:

```toml
chromiumoxide = { version = "=0.7.0", default-features = false, features = ["tokio-runtime"] }
```

Bundle the prepared runtime as a resource, not `externalBin`; CI cache key includes version+hash.

- [ ] **Step 4: Generate and verify lock**

Run: `node scripts/prepare-browser-runtime.mjs --write-lock --all-platforms && node scripts/prepare-browser-runtime.mjs --platform current`

Expected: both exit 0; the committed lock contains version, official URL, SHA-256 and executable relative path for `linux64`, `mac-arm64`, `mac-x64` and `win64`; current-platform extraction verifies its matching lock entry.

- [ ] **Step 5: Verify MSRV and commit**

Run: `cd src-tauri && cargo check --locked`

Expected: PASS on project Rust 1.77.2.

```bash
git add scripts/prepare-browser-runtime.mjs scripts/browser-runtime-lock.json src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json scripts/prepare-tauri-sidecar.mjs .github/workflows/ci.yml .github/workflows/cross-platform-smoke.yml .github/workflows/release-tauri.yml
git commit -m "build: package managed browser runtime"
```

### Task 3: Implement Ephemeral Chromium Engine and Runtime State

**Files:**
- Create: `src-tauri/src/workbench/browser_verification/{chromium.rs,runtime.rs,artifact_store.rs}`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`

**Interfaces:**
- Produces: `BrowserVerificationService::{start,get,cancel,artifact}` and event `workbench:browser-verification`.

- [ ] **Step 1: Write FakeEngine lifecycle tests**

```rust
#[tokio::test(start_paused = true)]
async fn runtime_reuses_same_idempotent_run_and_expires_artifacts() {
    let service = fixture_service(FakeEngine::succeeds()).await;
    let a = service.start(req("request-1")).await.unwrap();
    let b = service.start(req("request-1")).await.unwrap();
    assert_eq!(a.id, b.id);
    tokio::time::advance(Duration::from_secs(86_401)).await;
    service.cleanup().await.unwrap();
    assert!(service.artifact(&a.id, "shot").await.is_err());
}
```

Add different fingerprint conflict, concurrency cap, timeout, cancel, crash and profile cleanup cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked browser_verification::runtime --lib`

Expected: FAIL because runtime is absent.

- [ ] **Step 3: Implement managed engine**

Resolve only bundled executable; create `<dataDir>/browser-verification/runtime/<runId>` temp profile; launch via `BrowserConfig::builder().chrome_executable(...).user_data_dir(...).new_headless_mode()`. Drive handler task, enforce 30-minute session and 60-second idle exit, kill/reap child on cancel/shutdown and delete profile.

- [ ] **Step 4: Implement artifact limits and run GREEN**

Store PNG≤8MiB, max20 artifacts/run, max50MiB/run, retention24h. Run: `cd src-tauri && cargo test --locked browser_verification::runtime --lib && cargo test --locked browser_verification::artifact_store --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/browser_verification/chromium.rs src-tauri/src/workbench/browser_verification/runtime.rs src-tauri/src/workbench/browser_verification/artifact_store.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs
git commit -m "feat(workbench): run ephemeral browser verification"
```

### Task 4: Bind Local Commands to Live Preview Registry

**Files:**
- Create: `src-tauri/src/commands/workbench/browser_verification.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/control_workbench.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/workbench/browser_proxy.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: start/get/cancel/artifact local helpers and control ops.

- [ ] **Step 1: Write expired/foreign preview tests**

```rust
#[tokio::test]
async fn start_rejects_target_url_and_requires_live_preview_id() {
    let state = browser_state().await;
    let err = start_browser_verification_for_state(&state, req_with_preview("expired")).await.unwrap_err();
    assert_eq!(err.code(), "browser_preview_not_found");
    assert_eq!(state.engine_start_count(), 0);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked commands::workbench::browser_verification --lib`

Expected: FAIL because command is absent.

- [ ] **Step 3: Implement preview lookup and target revalidation**

Accept only `previewId`; call existing registry lookup, derive normalized owner loopback target, and revalidate every navigation/redirect. RemoteRelay never starts engine on controller. Register Tauri commands/control ops with request size limits.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked commands::workbench::browser_verification --lib && cargo test --locked workbench::browser_proxy --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/workbench/browser_verification.rs src-tauri/src/commands/workbench/mod.rs src-tauri/src/backend/control_workbench.rs src-tauri/src/backend/control_client.rs src-tauri/src/workbench/browser_proxy.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): bind browser verification to previews"
```

### Task 5: Add Owner P2P Routes and Orchestrator Evidence Adapter

**Files:**
- Create: `src-tauri/src/net/routes/browser_verification.rs`
- Modify: `src-tauri/src/net/routes/mod.rs`, `http_server.rs`, `protocol.rs`, `discovery.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`, `browser_proxy.rs`, `remote_ids.rs`
- Create: `src-tauri/src/orchestrator/browser_verification.rs`
- Modify: `src-tauri/src/orchestrator/evidence.rs`
- Modify: `docs/p2p-protocol.md`, route inventory.

**Interfaces:**
- Produces: `workbench.browser-verification.v1`, remote run/artifact routes and `browserVerification` evidence kind.

- [ ] **Step 1: Write owner execution and lost-ACK tests**

```rust
#[tokio::test]
async fn remote_verification_runs_engine_only_on_owner() {
    let fixture = two_device_browser_fixture().await;
    fixture.controller.start_remote().await.unwrap();
    assert_eq!(fixture.controller.engine_starts(), 0);
    assert_eq!(fixture.owner.engine_starts(), 1);
}
```

Add stale node, redirect escape, artifact traversal/type/size and click hit-count=1 cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked net::routes::browser_verification --lib`

Expected: FAIL because route/module is absent.

- [ ] **Step 3: Implement routes and evidence adapter**

Add create/get/cancel/artifact routes; create requires idempotency key, cancel naturally idempotent, click/fill result queries reconcile unknown outcome. Remote IDs wrap run/artifact refs. Orchestrator writes sanitized assertion/console/screenshot ID only; non-Web task without preview records notApplicable.

- [ ] **Step 4: Verify protocol inventory**

Run: `cd src-tauri && cargo test --locked net::routes::browser_verification --lib && cargo test --locked orchestrator::browser_verification --lib && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/routes/browser_verification.rs src-tauri/src/net/routes/mod.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs src-tauri/src/net/discovery.rs src-tauri/src/workbench/remote_client.rs src-tauri/src/workbench/browser_proxy.rs src-tauri/src/workbench/remote_ids.rs src-tauri/src/orchestrator/browser_verification.rs src-tauri/src/orchestrator/evidence.rs docs/p2p-protocol.md scripts/check-p2p-route-inventory.mjs
git commit -m "feat(p2p): relay browser verification evidence"
```

### Task 6: Add One-click Desktop and Mobile Verification UI

**Files:**
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/WorkbenchBrowserVerificationPanel.tsx`
- Create: `web/src/components/domain/WorkbenchBrowserWorkspace/{WorkbenchBrowserVerificationPanel.module.css,WorkbenchBrowserVerificationPanel.test.tsx,workbenchBrowserVerification.ts,workbenchBrowserVerification.test.ts}`
- Modify: `web/src/components/domain/WorkbenchBrowserWorkspace/{WorkbenchBrowserWorkspaceView.tsx,WorkbenchBrowserWorkspace.module.css,index.ts}`
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchHttp.test.ts,workbenchTransport.ts}`
- Modify: `web/src/lib/types/workbench.ts`
- Modify: `web/src/lib/schemas/{workbench.ts,workbench.test.ts}`
- Modify: `web/src/i18n/locales/{zh/workbench.json,en/workbench.json}` and `AGENTS.md` component list.

**Interfaces:**
- Consumes: run DTO/event/artifact.
- Produces: one-click automatic smoke UI and optional advanced structured steps.

- [ ] **Step 1: Write default-flow and no-script tests**

```tsx
it('starts snapshot plus screenshot without asking for selectors', async () => {
  render(<WorkbenchBrowserVerificationPanel previewId="p1" transport={transport} />)
  await userEvent.click(screen.getByRole('button', { name: '验证当前预览' }))
  expect(transport.start).toHaveBeenCalledWith(expect.objectContaining({ previewId: 'p1' }))
  expect(screen.queryByRole('textbox', { name: /脚本|JavaScript|selector/i })).not.toBeInTheDocument()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkbenchBrowserVerificationPanel`

Expected: FAIL because panel is absent.

- [ ] **Step 3: Implement low-burden panel**

Default action runs smoke and shows screenshot, accessibility summary, console/network errors and assertion status. Advanced controls remain structured, no arbitrary script. Reuse existing Browser Workspace navigation and tokens; no extra top-level route or decorative cards.

- [ ] **Step 4: Run GREEN/design gates**

Run: `cd web && npm test -- WorkbenchBrowserVerificationPanel workbenchBrowser && npm run check:css-tokens && npm run check:i18n && npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/WorkbenchBrowserWorkspace web/src/api web/src/lib/types/workbench.ts web/src/lib/schemas/workbench.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json AGENTS.md
git commit -m "feat(workbench): show browser verification results"
```

### Task 7: Run Real-browser, Cross-platform and Security Gates

**Files:**
- Create: `src-tauri/tests/browser_verification_smoke.rs`
- Modify: `src-tauri/tests/support/mod.rs`
- Modify: `web/tests/{workbench.spec.ts,mobile-workbench.spec.ts}`
- Modify: `docs/prd.md`
- Modify: `docs/development/{quality-matrix.json,testing.md,backend-operations.md}`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

**Interfaces:** Consumes Tasks 1–6 and produces L2/L3 evidence.

- [ ] **Step 1: Add real local fixture smoke**

Start loopback fixture; run snapshot→click→fill→wait→screenshot→console/network failure→cancel; assert profile cleanup and screenshot PNG signature.

- [ ] **Step 2: Run current-platform smoke**

Run: `cd src-tauri && cargo test --locked --test browser_verification_smoke -- --nocapture --test-threads=1`

Expected: PASS only when managed runtime is present; otherwise test reports explicit environment skip and quality row remains NOT VERIFIED.

- [ ] **Step 3: Run full gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked workbench::browser_verification && cargo test --locked net::routes::browser_verification && cd ../web && npm run build && npm test -- workbenchBrowser && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts && cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs`

Expected: all non-L3 gates exit 0.

- [ ] **Step 4: Audit sandbox and privacy**

Run: `rg -n "allow-same-origin|targetUrl|cookie|user-data-dir" web/src/components/domain/WorkbenchBrowserWorkspace src-tauri/src/workbench/browser_verification src-tauri/src/net/routes/browser_verification.rs`

Expected: iframe has no allow-same-origin; API DTO has no targetUrl/cookie; user-data-dir appears only in ephemeral engine implementation/tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/browser_verification_smoke.rs src-tauri/tests/support web/tests docs/development docs/prd.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "test: certify browser verification surface"
```

## Completion Contract

- Verification can only target a live owner preview and stays loopback-bound through redirects.
- Managed browser uses an ephemeral profile and is reaped on idle/cancel/shutdown.
- Default UI produces evidence with one action and no manual selector/script burden.
- Existing Browser preview remains usable when verification capability is unavailable or rolled back.

## Plan Self-Review

- Spec coverage: DTO, managed runtime, lifecycle, preview binding, remote/evidence, UI and L3 each map to tasks.
- Placeholder scan: no unresolved implementation placeholders; all supported asset hashes are generated by one explicit all-platform lock step and reverified by each release target.
- Type consistency: session/command/evidence/run IDs remain stable across engine, routes and UI.
