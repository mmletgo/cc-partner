# Targeted Performance and Maintainability Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改变产品语义、xterm 生命周期和 SQLite pool=1 的前提下，治理 Workbench 1 Hz 渲染、CodeMirror 语言包、Claude session 索引、peer timeout/health 重复与到期模块例外。

**Architecture:** 先记录 profiler/bundle/index 基线，再逐热点做可测优化。前端时钟下沉叶子组件，语言 extension 动态 import/cache。后端扫描进入 `spawn_blocking` 并受预算控制，peer client 使用请求类别。模块拆分仅跟随本轮触达代码与 characterization。

**Tech Stack:** React 19, TypeScript/Vite bundle analysis, CodeMirror 6, Rust/tokio notify/reqwest, module/bundle check scripts, Vitest/criterion-style deterministic benchmarks.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-targeted-performance-and-maintainability-design.md`、`web/CLAUDE.md`、`src-tauri/CLAUDE.md`、`web/scripts/bundle-budget-baseline.json` 与 `scripts/module-boundary-baseline.json`。
- 保留 xterm DOM 常驻与 AppShell terminal buffer provider；不因优化卸载隐藏终端。
- SQLite `max_connections(1)` 不变，除非独立后续 spec 用新基准推翻。
- Workbench 仍只有七个 domain controllers，页面 ≤1200 行，不创建 `useWorkbenchController`。
- bundle/module baseline 只能下降或保持，不得提高掩盖回归。
- 先测量再改，每任务包含 before/after 证据和 commit。

---

## File Structure

- Create: `web/src/pages/Workbench/SessionRuntimeText.tsx` and test。
- Modify: `web/src/pages/Workbench/Workbench.tsx`; Create: `web/src/pages/Workbench/WorkbenchRenderBudget.test.tsx`。
- Modify: `web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorLanguage.ts` and tests。
- Modify: `web/scripts/bundle-budget-baseline.json` and mirrored `scripts/bundle-budget-baseline.json` only via the existing checker after measured reduction/new editor-entry metric。
- Modify: `src-tauri/src/workbench/claude_sessions.rs`。
- Modify: `src-tauri/src/commands/workbench/sessions.rs`。
- Modify: `src-tauri/src/state.rs`, `src-tauri/src/backend/runtime.rs`, Workbench Session Search type/API/UI/tests for bounded diagnostics。
- Modify: `src-tauri/src/net/peer_client.rs`。
- Modify: `src-tauri/src/sync/engine.rs` only for backend peer timeout/health reuse not already landed in N2。
- Modify: oversized modules touched by previous plans with characterization tests。
- Modify: `scripts/module-boundary-baseline.json`。

## Shared Interfaces

```ts
export type WorkbenchLanguageLoader = () => Promise<Extension>
export function loadWorkbenchLanguage(language: string): Promise<Extension | null>
```

```rust
pub enum PeerTimeoutClass { Health, Metadata, Mutation, LongRunning }

pub struct ClaudeIndexBudget {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_jsonl_line_bytes: usize,
    pub max_total_bytes: u64,
    pub max_session_chars: usize,
}
```

## Task Dependency Graph

```text
T1 → (T2 | T3 | T4 | T6 | T7)
T4 → T5
(T2, T3, T5, T6, T7) → T8
```

基线后五条热点可按依赖与写集在独立 task worktree 并行，但同一时刻最多四个 implementer；`T5` 消费 session index，`T8` 汇总全部 before/after 证据并最后执行。

### Task 1: Capture Reproducible Performance Baselines

**Files:**
- Create: `web/src/pages/Workbench/WorkbenchRenderBudget.test.tsx`
- Modify: inline tests in `src-tauri/src/workbench/claude_sessions.rs`
- Modify: `web/scripts/check-bundle-contract.mjs` and `web/scripts/check-bundle-contract.test.mjs` only if current output cannot expose the measured values

**Interfaces:** Produces evidence for root render count, CodeEditor chunks and index duration/files/bytes. Peer health call-count characterization belongs to Task 6, which owns those files.

- [ ] **Step 1: Write tests that expose current hot paths**

```ts
test('captures the current workbench root rerender baseline', async () => {
  vi.useFakeTimers()
  const renders = vi.fn()
  render(<WorkbenchRenderProbe onRender={renders} />)
  await vi.advanceTimersByTimeAsync(5_000)
  expect(renders.mock.calls.length).toBeGreaterThan(1)
})
```

This characterization passes before Task 2 and records the unwanted behavior. Add an index budget harness and record current CodeEditor chunk from `npm run check:bundle`.

- [ ] **Step 2: Run and save baseline evidence in test output/PR description**

Run: `cd web && npm test -- WorkbenchRenderBudget.test.tsx && npm run check:bundle`

Expected: characterization PASS and records root rerenders; bundle command succeeds and reports baseline.

- [ ] **Step 3: Add deterministic backend budget harness**

Use generated temp JSONL fixtures in module tests, not user files or an integration test that cannot access private `workbench`. Record heartbeat responsiveness, bytes processed and current truncation behavior.

- [ ] **Step 4: Run backend baseline harness**

Run: `cd src-tauri && cargo test --locked workbench::claude_sessions::tests::index_budget_baseline -- --nocapture`

Expected: tests characterize current behavior and all committed assertions pass.

- [ ] **Step 5: Commit stable characterization only**

```bash
git add web/src/pages/Workbench/WorkbenchRenderBudget.test.tsx src-tauri/src/workbench/claude_sessions.rs web/scripts/check-bundle-contract.mjs web/scripts/check-bundle-contract.test.mjs
git commit -m "test(perf): characterize workbench and session index"
```

### Task 2: Isolate the Workbench Runtime Ticker

**Files:**
- Create: `web/src/pages/Workbench/SessionRuntimeText.tsx`
- Create: `web/src/pages/Workbench/SessionRuntimeText.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchStatusCard.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchRenderBudget.test.tsx`

**Interfaces:** `SessionRuntimeText({startedAt,endedAt,running,visible,emptyValue})`; interval only when running + owning Workbench surface visible + document visible, stopped value freezes at endedAt.

- [ ] **Step 1: Write leaf visibility/fake timer tests**

Assert no interval when stopped, document-hidden or mounted in a non-visible inspector/workspace surface; stopped duration remains frozen at `endedAt`, one formatted update per second when both visibility conditions hold, and cleanup on unmount. After initial StrictMode settle, update `WorkbenchRenderBudget.test.tsx` to require the root render count not to increase across five ticks rather than asserting an absolute render count.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- SessionRuntimeText.test.tsx WorkbenchRenderBudget.test.tsx`

Expected: FAIL because leaf component is absent/root rerenders.

- [ ] **Step 3: Move runtimeNow state into leaf**

Remove top-level Workbench interval/state. Pass immutable `startedAt/endedAt/running/visible/emptyValue`; derive `visible` from the existing active inspector/workspace state rather than IntersectionObserver ambiguity. Keep all hooks before early returns and do not change controllers.

- [ ] **Step 4: Verify render budget and characterization**

Run: `cd web && npm test -- SessionRuntimeText.test.tsx WorkbenchRenderBudget.test.tsx WorkbenchTerminal.characterization.test.tsx`

Expected: PASS; five ticks leave root render count unchanged.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench/SessionRuntimeText.tsx web/src/pages/Workbench/SessionRuntimeText.test.tsx web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/WorkbenchStatusCard.tsx web/src/pages/Workbench/WorkbenchRenderBudget.test.tsx
git commit -m "perf(workbench): isolate session runtime ticker"
```

### Task 3: Dynamically Load and Cache CodeMirror Languages

**Files:**
- Modify: `web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorLanguage.ts`
- Create: `web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorLanguage.test.ts`
- Modify: `web/src/components/domain/WorkbenchCodeEditor/WorkbenchCodeEditor.tsx`
- Modify: `web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorTheme.test.ts` only if the shared editor contract changes

**Interfaces:** Implements Shared `loadWorkbenchLanguage`; Promise cache keyed by canonical language.

- [ ] **Step 1: Write cache/stale/failure tests**

```ts
test('loads one language once and reuses its promise', async () => {
  const first = loadWorkbenchLanguage('typescript')
  const second = loadWorkbenchLanguage('ts')
  expect(first).toBe(second)
  await expect(first).resolves.toBeTruthy()
})
```

Add unknown language→null, import failure→plain text, rapid A→B old promise ignored.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- workbenchCodeEditorLanguage.test.ts`

Expected: FAIL because imports are static/API absent.

- [ ] **Step 3: Replace static imports with explicit dynamic import map**

Do not construct arbitrary import strings. The loader itself is non-`async` and returns the cached Promise identity; clear a rejected cache entry so retry works. Use request sequence in editor, render editable plain text while loading, and preserve current theme/extensions.

- [ ] **Step 4: Verify tests and bundle delta**

Run: `cd web && npm test -- WorkbenchCodeEditor workbenchCodeEditorLanguage.test.ts && npm run build && npm run check:bundle`

Expected: PASS; editor-entry loaded gzip falls by at least 20%, main/mobile ceilings do not regress and total runtime JS stays under the unchanged hard ceiling. If it does not, keep the task open and inspect remaining static imports without raising budgets.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorLanguage.ts web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorLanguage.test.ts web/src/components/domain/WorkbenchCodeEditor/WorkbenchCodeEditor.tsx web/src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorTheme.test.ts web/scripts/check-bundle-contract.mjs web/scripts/check-bundle-contract.test.mjs web/scripts/bundle-budget-baseline.json scripts/bundle-budget-baseline.json
git commit -m "perf(editor): lazy load language extensions"
```

### Task 4: Move Claude Session Indexing to a Bounded Blocking Worker

**Files:**
- Modify: `src-tauri/src/workbench/claude_sessions.rs`
- Modify: `src-tauri/src/commands/workbench/sessions.rs`
- Modify: `src-tauri/src/workbench/remote_protocol.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `web/src/lib/types/workbench.ts`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.tsx`
- Modify: Workbench Session Search tests/i18n resources
- Test: inline tests in `src-tauri/src/workbench/claude_sessions.rs`

**Interfaces:** Produces async `ensure_worktree_session_index_scanned` with owner-state per-key inflight map/singleflight, `ClaudeIndexBudget`, and local/remote DTO `{items,truncated,diagnostics}`. Capability-gated old peers retain the legacy `Vec<SessionSearchHit>` decode and receive synthesized `truncated=false/diagnostics=unavailable`.

- [ ] **Step 1: Add heartbeat/singleflight/budget tests**

```rust
#[tokio::test]
async fn initial_scan_does_not_block_tokio_heartbeat() {
    let (scan, heartbeats) = run_scan_with_heartbeat(large_fixture()).await;
    assert!(scan.truncated);
    assert!(heartbeats >= 3);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::claude_sessions::tests::initial_scan_does_not_block_tokio_heartbeat`

Expected: FAIL under synchronous scan.

- [ ] **Step 3: Implement `spawn_blocking`, lock-outside parse and budgets**

Defaults: 10,000 files, 64 MiB/file, 1 MiB/JSONL line, 512 MiB total and `MAX_SESSION_INDEX_CHARS=1_000_000` Unicode scalar values per session, truncated only at a UTF-8 char boundary; recent messages stay capped at 20. Use bounded `read_until/take`, sort candidates by mtime desc + canonical path before applying budgets, parse outside write lock, then atomically swap. Store the per-key inflight map in owner state so concurrent first searches share one future and failures clear the entry. Propagate the object DTO through remote protocol/client/route under a dedicated `workbench.session-search-result.v2` capability, add it to `server_protocol_info()` only after every route/decoder is wired, and preserve the legacy Vec fallback when the token is absent. Mixed-version tests cover new client↔old server and old client↔new server.

- [ ] **Step 4: Verify UTF-8 and old ordering**

Run: `cd src-tauri && cargo test --locked workbench::claude_sessions && cd ../web && npm test -- WorkbenchSessionSearch && npm run build`

Expected: PASS including Chinese/emoji and >1 MiB single-line truncation, deterministic limit=50 ordering, heartbeat responsiveness and visible truncated diagnostics.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/claude_sessions.rs src-tauri/src/commands/workbench/sessions.rs src-tauri/src/workbench/remote_protocol.rs src-tauri/src/workbench/remote_client.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/protocol.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs web/src/lib/types/workbench.ts web/src/api/workbench.ts web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.tsx web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.module.css web/src/components/domain/WorkbenchSessionSearch/index.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "perf(workbench): bound Claude session indexing"
```

### Task 5: Handle Watcher Delete/Rename and Lifecycle Cleanup

**Files:**
- Modify: `src-tauri/src/workbench/claude_sessions.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs` shutdown callsites
- Modify: `src-tauri/src/commands/workbench/projects.rs` project-removal callsites
- Test: inline watcher tests in `src-tauri/src/workbench/claude_sessions.rs`

**Interfaces:** watcher maps remove/rename events to index deletion/reindex; owner-state map value becomes a watcher runtime holding watcher, cancellation token and debounce/scan handles; uncertain events request one bounded rescan.

- [ ] **Step 1: Write delete/rename/shutdown tests**

Assert old session disappears after delete, rename removes old+adds new, watcher cancellation leaves no background task.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked claude_sessions::tests::watcher_delete`

Expected: FAIL because only Create/Modify is handled.

- [ ] **Step 3: Implement complete event mapping and cancellation token**

Normalize paths inside the owning root, ignore outside paths, debounce uncertain rescan, and store watcher + cancellation token + trailing/scan handles together in owner state. Project removal and shutdown remove this runtime, cancel it and await/abort all pending handles before dropping the watcher/index.

- [ ] **Step 4: Run watcher tests**

Run: `cd src-tauri && cargo test --locked workbench::claude_sessions`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/claude_sessions.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/src/commands/workbench/projects.rs
git commit -m "fix(workbench): keep session index lifecycle accurate"
```

### Task 6: Classify Peer Timeouts and Reuse Health

**Files:**
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/sync/engine.rs`
- Modify: `src-tauri/src/cc/engine.rs`
- Modify: `src-tauri/src/sync/{ssh_target,scratchpad}.rs`
- Test: inline tests in `src-tauri/src/net/peer_client.rs`, `src-tauri/src/sync/engine.rs`, `src-tauri/src/cc/engine.rs`

**Interfaces:** Implements `PeerTimeoutClass`; health=3s, metadata=10s, mutation=30s, long explicit. Event streams stay in N1 owner bridge/N3 heartbeat and are not constructed by this PeerClient path.

- [ ] **Step 1: Write timeout-selection and one-health tests**

```rust
#[tokio::test]
async fn one_device_sync_uses_one_health_probe() {
    let peer = CountingPeer::new();
    sync_all_domains(&peer).await;
    assert_eq!(peer.health_calls(), 1);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked peer_timeout && cargo test --locked one_device_sync_uses_one_health_probe`

Expected: FAIL because clients use uniform timeout/repeated health.

- [ ] **Step 3: Add request class to peer helpers and pass protocol info**

Centralize timeout mapping. Fetch typed health once per device and pass it into domain engines. If N2 already implemented reuse, only add timeout classes and retain its tests.

- [ ] **Step 4: Run peer/sync tests**

Run: `cd src-tauri && cargo test --locked net::peer_client && cargo test --locked sync && cargo test --locked cc::engine`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/peer_client.rs src-tauri/src/sync/engine.rs src-tauri/src/sync/ssh_target.rs src-tauri/src/sync/scratchpad.rs src-tauri/src/cc/engine.rs
git commit -m "perf(network): classify timeouts and reuse health"
```

### Task 7: Close Oversized Module Exceptions with Characterization

**Files:**
- Modify: `src-tauri/src/transfer/receiver.rs`
- Create: `src-tauri/src/transfer/receiver/validation.rs`, `src-tauri/src/transfer/receiver/chunk_io.rs`, `src-tauri/src/transfer/receiver/resume.rs`, `src-tauri/src/transfer/receiver/finalize.rs`
- Modify: `web/src/pages/Settings/useSettingsController.ts`
- Create: `web/src/pages/Settings/controllers/useSettingsResources.ts`, `web/src/pages/Settings/controllers/useSettingsFormSaves.ts`, `web/src/pages/Settings/controllers/useSettingsUpdatePermissions.ts` and focused tests
- Modify: `scripts/module-boundary-baseline.json`
- Test: `web/src/pages/Settings/Settings.test.tsx` and `src-tauri/tests/quality_faults.rs`

**Interfaces:** No public behavior change; moved functions keep signatures or receive narrow dependency structs.

- [ ] **Step 1: Pin characterization for each moved boundary**

Receiver: validation/chunk/resume/finalize. Settings: resource loading/form save/update-permission. Add tests before moving code; do not move unrelated sections.

- [ ] **Step 2: Run characterization**

Run: `cd web && npm test -- Settings && cd ../src-tauri && cargo test --locked transfer::receiver && cargo test --locked --test quality_faults`

Expected: PASS before extraction.

- [ ] **Step 3: Extract one responsibility at a time**

After each extraction run focused tests. Preserve Chinese docstrings, strict types, visibility and no domain→domain imports. Do not raise any baseline.

- [ ] **Step 4: Lower module baseline and run checker**

Run: `cd web && npm run check:modules`

Expected: PASS with lower line caps and no exception beyond its approved expiry.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/transfer/receiver.rs src-tauri/src/transfer/receiver/validation.rs src-tauri/src/transfer/receiver/chunk_io.rs src-tauri/src/transfer/receiver/resume.rs src-tauri/src/transfer/receiver/finalize.rs web/src/pages/Settings/useSettingsController.ts web/src/pages/Settings/controllers/useSettingsResources.ts web/src/pages/Settings/controllers/useSettingsResources.test.tsx web/src/pages/Settings/controllers/useSettingsFormSaves.ts web/src/pages/Settings/controllers/useSettingsFormSaves.test.tsx web/src/pages/Settings/controllers/useSettingsUpdatePermissions.ts web/src/pages/Settings/controllers/useSettingsUpdatePermissions.test.tsx scripts/module-boundary-baseline.json
git commit -m "refactor: close oversized module exceptions"
```

### Task 8: Full Performance and Documentation Gate

**Files:**
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/development/quality-matrix.json` only when a stable evidence mapping changes
- Modify: `web/scripts/bundle-budget-baseline.json`, mirrored `scripts/bundle-budget-baseline.json`, and `scripts/module-boundary-baseline.json` only with measured reductions

- [ ] **Step 1: Re-run and compare all baselines**

Record root render count, CodeEditor chunk, index budget/heartbeat, health calls and module caps. No budget is raised.

- [ ] **Step 2: Run complete gates**

```bash
cd web
npm run check:modules
npm run check:bundle
npm run lint
npm run build
npm test
npm run test:e2e
cd ../src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cd ..
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 3: Inspect runtime semantics**

Confirm hidden xterm stays mounted, pool remains 1, Workbench hooks stay before early return and seven-controller ownership remains.

- [ ] **Step 4: Update only durable performance/module facts**

Document budgets, loaders, index limits, timeout classes and closed exceptions; do not create task-summary docs.

- [ ] **Step 5: Commit**

```bash
git add web/CLAUDE.md src-tauri/CLAUDE.md docs/development/quality-matrix.json web/scripts/bundle-budget-baseline.json scripts/bundle-budget-baseline.json scripts/module-boundary-baseline.json
git commit -m "docs: calibrate targeted performance budgets"
```

## Rollback and Failure Containment

- 每项优化独立提交；before/after 未达到阈值或行为 characterization 失败时回退该提交，不调整预算掩盖结果。
- 动态语言加载失败回退为该文件的 plaintext 模式并显示可恢复错误，不静默加载全语言包。
- watcher/index/network 优化回退必须保留取消与资源清理，SQLite pool=1、xterm 常驻和七 controller 合同不参与试验。

## Completion Contract

- Workbench root no longer rerenders each second; language packs load on demand.
- session indexing is bounded/non-blocking and watcher lifecycle accurate.
- peer timeouts/health calls are classified/reused.
- module baselines decrease, pool=1/xterm/controller contracts remain, and all gates pass.

## Plan Self-Review

- Spec coverage: render, editor, index, watcher, timeout, health and module governance each map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: loader/timeout/budget interfaces are defined once and reused.
