# Core Product Integrity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让文件传输、速记本、Prompt、Claude History、Settings、权限与轮询状态都与权威后端一致，不再出现 no-op、静默丢数据、旧响应覆盖或整页失败。

**Architecture:** 先建立 visibility-aware single-flight polling 和 AppShell 常驻 pending-write/autosave queue，再逐业务接入窄状态机。所有乐观 mutation 都保存回滚快照；Settings 与权限采用显式 partial/error state；Transfer 只暴露后端真实支持的 send/cancel。

**Tech Stack:** React 19, TypeScript 6, React Router v6, Tauri 2 JS APIs/plugin-dialog, Vitest + jsdom + Testing Library, Playwright, i18next, Rust transfer commands.

## Global Constraints

- 开始前读取根 `AGENTS.md`、`web/CLAUDE.md`、`docs/prd.md`；执行阶段使用 `superpowers:using-git-worktrees` 创建独立 worktree。
- 所有 hooks 位于 early return 前；新增函数/组件必须有中文 Business Logic / Code Logic docstring；禁止 `any`。
- macOS、Windows、Ubuntu 文件路径作为不透明 UTF-8 字符串传给 Rust，不自行替换分隔符、decode URI 或拼接路径。
- Transfer 本轮只实现 send/cancel；pause/resume/retry/open 未有后端命令时不得渲染，不保留空回调。
- required permissions 固定为 screenCapture/accessibility/inputMonitoring；notification 展示但不阻断 onboarding。
- 后台标签页停止网络轮询，恢复 visible 时立即执行一次；同一 poll task 最多一个 in-flight 请求。
- 有成功数据时刷新失败保留数据并显示 stale/error；不得用空数组覆盖已成功数据。
- 用户可见文案写入现有 zh/en namespace；样式只使用 `tokens.css` 已定义 token。
- 每个 task 按 failing test → 最小实现 → focused tests → lint/build → commit 执行；实际实现时每个 commit 只包含该 task 文件。

---

## Task Dependency Graph

```text
T1 ─┬─> T3
    ├─> T8
    └─> T9
T2 ─> T3
T4 ─> T5
T6 ─┐
T7 ─┼─> T10
T8 ─┤
T9 ─┘
T3 ─> T10
T5 ─> T10
```

可并行 waves：`(T1 | T2 | T4 | T6 | T7) → (T3 | T5 | T8 | T9) → T10`。T4/T5 都修改 Scratchpad/App，T1/T9 都修改 polling consumers，必须遵守依赖；共享 `App.tsx` 的 T5 与 T10 不并行。

## File Structure

### Shared async infrastructure

- Create `web/src/hooks/useVisibilityPolling.ts`, `.test.tsx`: visibility + single-flight hook。
- Create `web/src/lib/pendingWrites.ts`, `.test.ts`: App 关闭前 flush registry。
- Create `web/src/hooks/scratchpadAutosave.ts`, `.test.ts`: 多页面 pending queue。
- Create `web/src/hooks/ScratchpadAutosaveProvider.tsx`, `scratchpadAutosaveContext.ts`: AppShell 常驻 provider/context。

### Product flows

- Modify `web/src/api/transfer.ts`, `web/src/lib/types.ts`: send/cancel exact DTO。
- Create `web/src/pages/Transfer/transferFileSelection.ts`, `.test.ts`: native dialog/drag path adapter。
- Modify `web/src/pages/Transfer/Transfer.tsx`, `Transfer.module.css`, `web/src/components/domain/TransferItem/TransferItem.tsx`: 真实 send/cancel 与只渲染可用动作。
- Modify `web/src/pages/Scratchpad/Scratchpad.tsx`, `web/src/App.tsx`: queue + close flush。
- Create `web/src/pages/Prompts/promptMutations.ts`, `.test.ts`; modify `Prompts.tsx`: rollback/retry。
- Create `web/src/pages/CcHistory/ccHistoryRequestState.ts`, `.test.ts`; modify `CcHistory.tsx`: stale guard。
- Create `web/src/pages/Settings/settingsResources.ts`, `.test.ts`; modify `Settings.tsx`, `Settings.module.css`: partial loading/retry。
- Modify `web/src/hooks/usePermissions.ts`, create `.test.tsx`; modify `Welcome.tsx`, `Settings.tsx`, `PermissionCard.tsx`: error/retry/逐项请求。
- Modify `Devices.tsx`, `Health.tsx`, `Transfer.tsx`: shared polling。

### Verification and docs

- Create `web/tests/core-integrity.spec.ts`; modify `web/tests/fixtures.ts` only if reusable Tauri dialog/event fake is needed。
- Modify `web/src/i18n/locales/{zh,en}/{transfer,prompts,scratchpad,ccHistory,settings,welcome}.json`。
- Modify `docs/prd.md`, `web/CLAUDE.md`, `AGENTS.md`（仅新增组件/持久约定）。

## Shared Interfaces

```ts
export interface UseVisibilityPollingOptions {
  intervalMs: number;
  enabled?: boolean;
  runImmediately?: boolean;
  refreshOnVisible?: boolean;
}
export interface UseVisibilityPollingResult {
  runNow: () => Promise<void>;
  inFlight: boolean;
}
export function useVisibilityPolling(
  task: () => Promise<void>,
  options: UseVisibilityPollingOptions,
): UseVisibilityPollingResult;

export interface SendTransferResult {
  accepted: true;
  deviceId: string;
  filePath: string;
  id: string;
}

export interface PendingWriteRegistry {
  register(id: string, flush: () => Promise<void>): () => void;
  flushAll(): Promise<void>;
}

export interface ScratchpadAutosaveQueue {
  schedule(pageId: string, content: string): void;
  flushPage(pageId: string): Promise<void>;
  flushAll(): Promise<void>;
  getSnapshot(): ScratchpadAutosaveSnapshot;
  subscribe(listener: () => void): () => void;
}
```

### Task 1: Visibility-Aware Single-Flight Polling

**Files:**
- Create: `web/src/hooks/useVisibilityPolling.ts`
- Create: `web/src/hooks/useVisibilityPolling.test.tsx`

**Interfaces:** Produces `useVisibilityPolling(task, options)` exactly as declared above; later tasks consume `runNow` for mutation-triggered refresh.

- [ ] **Step 1: Write failing fake-timer tests**

Cover immediate run, no overlapping tick while a deferred Promise is pending, hidden pause, one immediate run on visible, `enabled=false`, task identity update without timer reset, and no state write after unmount.

```ts
const first = deferred<void>();
const task = vi.fn(() => first.promise);
renderHook(() => useVisibilityPolling(task, { intervalMs: 3000 }));
await act(() => vi.advanceTimersByTimeAsync(9000));
expect(task).toHaveBeenCalledTimes(1);
first.resolve();
```

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- useVisibilityPolling`
Expected: FAIL because `./useVisibilityPolling` does not exist.

- [ ] **Step 3: Implement the minimal hook**

Use `taskRef`, `inFlightPromiseRef`, mounted ref and a visibility listener. `runNow` returns the existing Promise while in flight; interval callback checks `document.visibilityState === 'visible'`. Do not swallow task errors in `runNow`; interval calls attach `.catch(() => undefined)` so there is no unhandled rejection.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- useVisibilityPolling
npm run lint
npm run build
git add src/hooks/useVisibilityPolling.ts src/hooks/useVisibilityPolling.test.tsx
git commit -m "feat: add visibility-aware polling hook"
```

Expected: focused suite PASS; lint/build exit 0.

### Task 2: Transfer Native Path and DTO Contract

**Files:**
- Modify: `web/src/api/transfer.ts`
- Modify: `web/src/lib/types.ts`
- Create: `web/src/pages/Transfer/transferFileSelection.ts`
- Create: `web/src/pages/Transfer/transferFileSelection.test.ts`
- Create: `web/src/api/transfer.test.ts`

**Interfaces:** Produces `SendTransferResult`, `CancelTransferResult`, `pickTransferFile(): Promise<string | null>`, `subscribeTransferFileDrops(onPaths: (paths: string[]) => void): Promise<() => void>`.

- [ ] **Step 1: Write failing adapter/API tests**

Assert dialog `open({ multiple:false, directory:false })`, cancel→null, Windows path unchanged, native `drop.paths` forwarded unchanged, non-Tauri environment returns no-op unsubscribe, and invoke generics map send/cancel result shapes.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- transferFileSelection src/api/transfer.test.ts`
Expected: FAIL on missing adapter and wrong `transferApi.send/cancel` return types.

- [ ] **Step 3: Implement exact contracts**

```ts
export const transferApi = {
  list: () => invoke<TransferTask[]>('list_transfers'),
  send: (deviceId: string, filePath: string) =>
    invoke<SendTransferResult>('send_transfer', { deviceId, filePath }),
  cancel: (taskId: string) =>
    invoke<CancelTransferResult>('cancel_transfer', { taskId }),
};
```

Use dynamic imports of `@tauri-apps/plugin-dialog` and `@tauri-apps/api/webview`; guard native event registration with Tauri internals so Playwright browser mode remains stable.

- [ ] **Step 4: Verify and commit**

Run `cd web && npm test -- transferFileSelection src/api/transfer.test.ts && npm run build`.
Expected: PASS and no TypeScript mismatch.

```bash
git add src/api/transfer.ts src/api/transfer.test.ts src/lib/types.ts src/pages/Transfer/transferFileSelection.ts src/pages/Transfer/transferFileSelection.test.ts
git commit -m "fix: align transfer file and command contracts"
```

### Task 3: Complete Transfer Send/Cancel Journey

**Files:**
- Modify: `web/src/pages/Transfer/Transfer.tsx`
- Modify: `web/src/pages/Transfer/Transfer.module.css`
- Create: `web/src/pages/Transfer/Transfer.test.tsx`
- Modify: `web/src/components/domain/TransferItem/TransferItem.tsx`
- Create: `web/src/components/domain/TransferItem/TransferItem.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/transfer.json`

**Interfaces:** Consumes T1 `runNow`, T2 path adapter and DTO; produces no new backend API.

- [ ] **Step 1: Write failing component tests**

Test basename-only display, Enter/Space picker, native drop chooses first path, sending disables button, success clears selection and refreshes, failure retains selection, cancel busy/error, existing list survives refresh failure, and pause/retry/open controls are absent when callbacks are absent.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- src/pages/Transfer src/components/domain/TransferItem`
Expected: FAIL because current send only logs and unsupported actions render.

- [ ] **Step 3: Implement send/cancel state**

Store `{path,name}` rather than `File`; subscribe/unsubscribe native drop on mount. `handleSendClick` awaits `transferApi.send`, then `await runTasksNow()`. Track `sending`, `sendError`, `cancellingIds`, `taskActionErrors`. Pass only `onCancel` for pending/transferring tasks; change `TransferItem` to guard every action with callback presence.

- [ ] **Step 4: Replace page intervals with T1 hook**

Use 3,000ms tasks polling and 5,000ms devices polling; loaders preserve existing arrays on refresh error. Add `aria-busy`/`role=alert` and localized first-file-only message.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- src/pages/Transfer src/components/domain/TransferItem useVisibilityPolling
npm run lint
npm run build
git add src/pages/Transfer src/components/domain/TransferItem src/i18n/locales/zh/transfer.json src/i18n/locales/en/transfer.json
git commit -m "feat: complete desktop file transfer journey"
```

### Task 4: Build the Scratchpad Autosave Queue

**Files:**
- Create: `web/src/hooks/scratchpadAutosave.ts`
- Create: `web/src/hooks/scratchpadAutosave.test.ts`
- Create: `web/src/hooks/scratchpadAutosaveContext.ts`
- Create: `web/src/hooks/ScratchpadAutosaveProvider.tsx`
- Create: `web/src/lib/pendingWrites.ts`
- Create: `web/src/lib/pendingWrites.test.ts`

**Interfaces:** Produces `createScratchpadAutosaveQueue(save, {delayMs})`, provider `useScratchpadAutosave()`, and singleton `pendingWrites` implementing the interfaces above.

- [ ] **Step 1: Write queue and registry failing tests**

Cover debounce coalescing, independent pages, one in-flight save per page, edit during save causes second flush, failed save retains pending/error, retry succeeds, `flushAll` aggregates failures, and registry unregister removes a writer.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- scratchpadAutosave pendingWrites`
Expected: FAIL because modules do not exist.

- [ ] **Step 3: Implement queue state machine**

Represent each page as `{pendingVersion,savedVersion,content,inFlight,error,timer}`. `flushPage` loops while `savedVersion < pendingVersion`; only clear error after a successful latest-version save. Provider creates one queue for AppShell lifetime and registers `queue.flushAll` in `pendingWrites`.

- [ ] **Step 4: Verify and commit**

Run `cd web && npm test -- scratchpadAutosave pendingWrites && npm run lint && npm run build`.
Expected: PASS.

```bash
git add src/hooks/scratchpadAutosave* src/hooks/ScratchpadAutosaveProvider.tsx src/lib/pendingWrites*
git commit -m "feat: add durable scratchpad autosave queue"
```

### Task 5: Integrate Scratchpad Unmount and GUI-Close Flush

**Files:**
- Modify: `web/src/pages/Scratchpad/Scratchpad.tsx`
- Create: `web/src/pages/Scratchpad/Scratchpad.test.tsx`
- Modify: `web/src/App.tsx`
- Create: `web/src/App.closeFlush.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/scratchpad.json`
- Modify: `web/src/i18n/locales/{zh,en}/common.json`

**Interfaces:** Consumes T4 provider/registry; `BackendCloseChoiceListener` must await `pendingWrites.flushAll()` before either `backendApi.exitGui()` path.

- [ ] **Step 1: Write failing lifecycle tests**

Type content then unmount before 500ms and assert save starts; switch page awaits flush before get; failed queue item remains retryable; GUI close calls `flushAll` before stop/exit; flush rejection prevents exit and shows close-dialog error.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- Scratchpad App.closeFlush`
Expected: current cleanup only clears timer, and close exits without pending write flush.

- [ ] **Step 3: Integrate provider and page**

Mount `ScratchpadAutosaveProvider` inside the AppShell provider tree. Replace page-local timer/pending refs with `queue.schedule/flushPage`; subscribe to snapshot for saving/error UI. Cleanup calls `void queue.flushAll()`; `pagehide` does the same best-effort.

- [ ] **Step 4: Gate GUI exit and verify**

Both close handlers execute `await pendingWrites.flushAll()` before backend stop/exit. On rejection, keep modal open and reset busy state.

```bash
cd web
npm test -- Scratchpad App.closeFlush scratchpadAutosave
npm run lint
npm run build
git add src/pages/Scratchpad src/App.tsx src/App.closeFlush.test.tsx src/i18n/locales
git commit -m "fix: flush scratchpad writes before navigation and exit"
```

### Task 6: Make Prompt Mutations Transactional in the UI

**Files:**
- Create: `web/src/pages/Prompts/promptMutations.ts`
- Create: `web/src/pages/Prompts/promptMutations.test.ts`
- Modify: `web/src/pages/Prompts/Prompts.tsx`
- Create: `web/src/pages/Prompts/Prompts.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/prompts.json`

**Interfaces:** Produces `applyOptimisticPromptMutation`, `rollbackPromptMutation`, `commitPromptMutation`; mutation union matches the design spec.

- [ ] **Step 1: Write failing pure reducer tests**

Assert create replace/rollback, update server canonical DTO/rollback, delete original-index restoration, and unrelated rows retain identity/order.

- [ ] **Step 2: Write failing UI reject/retry tests**

Mock create/update/delete/sync rejection. Assert row rollback, original draft reopened, error visible, retry uses stored payload, entity conflict actions disabled, and sync failure is not silent.

- [ ] **Step 3: Verify RED**

Run: `cd web && npm test -- src/pages/Prompts`
Expected: FAIL because current catches preserve optimistic state silently.

- [ ] **Step 4: Implement minimal mutation controller in page**

Keep `failedMutation` and `pendingEntityIds`; wrap each API call in apply/commit/rollback. Derive tags from `prompts` after mutation rather than mutating a second optimistic source. Use returned DTO for create and update.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- src/pages/Prompts
npm run lint
npm run build
git add src/pages/Prompts src/i18n/locales/zh/prompts.json src/i18n/locales/en/prompts.json
git commit -m "fix: rollback failed prompt mutations"
```

### Task 7: Guard Claude History Against Stale Responses

**Files:**
- Create: `web/src/pages/CcHistory/ccHistoryRequestState.ts`
- Create: `web/src/pages/CcHistory/ccHistoryRequestState.test.ts`
- Modify: `web/src/pages/CcHistory/CcHistory.tsx`
- Create: `web/src/pages/CcHistory/CcHistory.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/ccHistory.json`

**Interfaces:** Produces `createLatestRequestGuard<TContext>()` with `begin(context): token`, `isCurrent(token, context): boolean`, `invalidate(): void`.

- [ ] **Step 1: Write inverse-resolution tests**

Use deferred requests for project A→B and search `a`→`ab`; resolve old last and assert only B/`ab` writes prompts/error/loading. Also test refresh/sync failures display feedback.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- src/pages/CcHistory`
Expected: old response currently overwrites new context.

- [ ] **Step 3: Implement guards at write boundaries**

Create separate project and prompt guards; context key is `${projectPath}\0${search ?? ''}`. Check current before every success, catch and finally state write; invalidate prompt guard when selected project becomes null.

- [ ] **Step 4: Verify and commit**

Run `cd web && npm test -- src/pages/CcHistory && npm run lint && npm run build`.
Expected: PASS.

```bash
git add src/pages/CcHistory src/i18n/locales/zh/ccHistory.json src/i18n/locales/en/ccHistory.json
git commit -m "fix: ignore stale Claude history responses"
```

### Task 8: Load Settings Resources Independently

**Files:**
- Create: `web/src/pages/Settings/settingsResources.ts`
- Create: `web/src/pages/Settings/settingsResources.test.ts`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/pages/Settings/Settings.module.css`
- Modify: `web/src/i18n/locales/{zh,en}/settings.json`

**Interfaces:** Produces `loadSettingsResources(api): Promise<SettingsResourceResults>` and `retrySettingsResource(group)`; groups are `core/defaults/version/cloudSync/githubTrending/health/automation`.

- [ ] **Step 1: Write allSettled mapping tests**

Test all 11 success values; core current failure; each optional group current/default failure; defaults fallback disables reset; retry one group invokes only its two endpoints and preserves other state.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- settingsResources Settings`
Expected: missing loader; current `Promise.all` rejects whole page.

- [ ] **Step 3: Implement discriminated results**

```ts
export type ResourceResult<T> =
  | { status: 'ready'; value: T }
  | { status: 'error'; error: Error };
```

Use one `Promise.allSettled` call, map each result by stable index, and expose group retry factories. Do not silently replace current config with defaults.

- [ ] **Step 4: Render local failures and migrate update polling**

Keep shell available when `configApi.get` succeeds. Add panel-local retry/error; disable only affected reset/save actions. Replace updater 800ms interval with T1 hook enabled only while downloading.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- settingsResources Settings useVisibilityPolling
npm run lint
npm run build
git add src/pages/Settings src/i18n/locales/zh/settings.json src/i18n/locales/en/settings.json
git commit -m "fix: isolate settings resource failures"
```

### Task 9: Permissions and Remaining Polling Consumers

**Files:**
- Modify: `web/src/hooks/usePermissions.ts`
- Create: `web/src/hooks/usePermissions.test.tsx`
- Modify: `web/src/pages/Welcome/Welcome.tsx`
- Modify: `web/src/components/domain/PermissionCard/PermissionCard.tsx`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/pages/Devices/Devices.tsx`
- Modify: `web/src/pages/Health/Health.tsx`
- Create: `web/src/pages/Devices/Devices.test.tsx`
- Create: `web/src/pages/Health/HealthPolling.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/{welcome,settings,devices,health}.json`

**Interfaces:** `UsePermissionsResult` gains `refreshing`, `error`, `requesting: ReadonlySet<PermissionType>`, `request(type, openSettings?): Promise<void>`, `allRequiredGranted`; keep `requestMissing` temporarily only if another caller still requires it, implemented sequentially.

- [ ] **Step 1: Write permission state tests**

Test first failure ends loading with error, retry success, stale status preserved on later error, per-type request, request error, duplicate same-type suppression, required excludes notification, hidden pause and visible refresh.

- [ ] **Step 2: Write Devices/Health polling tests**

Assert no requests while hidden, one on visible, no overlap with deferred refresh, stale data preserved after error. Health local overlay timer is explicitly unchanged.

- [ ] **Step 3: Verify RED**

Run: `cd web && npm test -- usePermissions Devices HealthPolling`
Expected: current permissions swallow errors and pages use naked intervals.

- [ ] **Step 4: Implement hook and UI migration**

Build permissions on T1 hook at 2,000ms. Welcome/Settings render each PermissionCard with `onRequest={() => request(entry.type)}`, explicit retry, `aria-busy`; no permanent checking state. Migrate Devices 5,000ms and Health 5,000ms refresh to T1.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- usePermissions Devices HealthPolling permissionEntries
npm run lint
npm run build
git add src/hooks/usePermissions* src/pages/Welcome src/pages/Devices src/pages/Health src/pages/Settings src/components/domain/PermissionCard src/i18n/locales
git commit -m "fix: make permissions and page polling resilient"
```

### Task 10: End-to-End Contracts and Product Documentation

**Files:**
- Create: `web/tests/core-integrity.spec.ts`
- Modify: `web/tests/fixtures.ts` only for shared fake helpers
- Modify: `docs/prd.md`
- Modify: `web/CLAUDE.md`
- Modify: `AGENTS.md` if provider/component inventory changed

**Interfaces:** E2E fakes must implement exact invoke names `list_devices`, `list_transfers`, `send_transfer`, `cancel_transfer`, Prompt commands and permission commands; no production-only bypass.

- [ ] **Step 1: Add failing E2E journeys**

Add browser-mode native adapter injection for a selected `/tmp/report.txt`; test send→task visible→cancel, Prompt create/update/delete reject rollback, and permission initial failure→retry. Assert no `console.error` through existing fixture.

- [ ] **Step 2: Run focused E2E**

Run: `cd web && npm run test:e2e -- core-integrity.spec.ts`
Expected: FAIL until all preceding UI contracts are wired.

- [ ] **Step 3: Update persistent behavior docs**

Document real Transfer send/cancel, unsupported actions hidden, Scratchpad close flush, Settings partial failure, permission retry, and visibility polling. Do not add a task-summary Markdown file.

- [ ] **Step 4: Run complete gates**

```bash
cd web
npm run lint
npm run build
npm test
npm run test:e2e
cd ../src-tauri
cargo test --locked commands::transfer --lib
cargo test --locked transfer::sender --lib
git -C .. status --short
```

Expected: all commands exit 0; `git status --short` contains only plan-scoped implementation/docs files.

- [ ] **Step 5: Commit**

```bash
git -C .. add web/tests/core-integrity.spec.ts web/tests/fixtures.ts docs/prd.md web/CLAUDE.md AGENTS.md
git -C .. commit -m "test: cover core product integrity journeys"
```
