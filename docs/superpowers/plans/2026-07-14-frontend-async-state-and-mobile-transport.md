# Frontend Async State and Mobile Transport Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除保存/切页/长操作的旧响应覆盖，保留失败草稿，并让 Mobile transport 在弱网下具备 timeout、取消、错误分类和安全对账。

**Architecture:** 新增无业务依赖的 async-state 纯 helper；页面 controller 持有 edit/request/context sequence。Workbench commit/push/merge/remove 先进入 sidecar durable operation ledger，Tauri/HTTP 共用 typed outcome 与权威对账。移动 HTTP helper 统一接收 request policy 与 AbortSignal；只读请求有限重试，mutation 只以稳定 operation id 对账/幂等重送。

**Tech Stack:** React 19, TypeScript strict, Vitest 4, Testing Library, Tauri invoke wrappers, browser Fetch/AbortController.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-frontend-async-state-and-mobile-transport-design.md`、根 `AGENTS.md`、`web/CLAUDE.md`。
- 所有 Hooks 位于 early return 前；不新增页面级万能 controller。
- 不引入 Redux/Zustand/数据请求框架。
- mutation 无幂等或对账能力时禁止 transport 自动重试；浏览器 transport 不声称能区分 connect/first-byte/not-started。
- 失败必须保留 draft、回滚或明确显示 unknown；不得静默。
- 每任务先 RED、再最小实现、focused tests、commit。

---

## File Structure

- Create: `web/src/lib/asyncState/saveAttempt.ts` and test。
- Create: `web/src/lib/asyncState/operationContext.ts` and test。
- Create: `web/src/lib/asyncState/mutationOutcome.ts` and test。
- Create: `src-tauri/src/workbench/operation_ledger.rs`; modify Workbench command/control/P2P/runtime schema and tests。
- Modify: `web/src/api/workbench.ts`, `web/src/lib/types/workbench.ts`; create shared Workbench mutation reconciliation helper/tests。
- Modify: `web/src/pages/ClaudeMd/ClaudeMd.tsx`。
- Modify: `web/src/pages/Settings/useSettingsController.ts` and tests。
- Modify: `web/src/pages/Scratchpad/Scratchpad.tsx` and tests。
- Modify: `web/src/pages/Devices/Devices.tsx` and tests。
- Modify: `web/src/pages/CcHistory/CcHistory.tsx` and tests。
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.ts` and test。
- Modify: `web/src/mobile/components/MobileGitPanel.tsx` and test。
- Modify: `web/src/mobile/components/MobileWorktreePanel.tsx` and mobile panel state/tests。
- Modify: `web/src/api/workbenchHttp.ts` and test。
- Modify: `web/src/mobile/MobileWorkbench.tsx` and state/tests。
- Modify: `web/src/components/domain/TagInput/TagInput.tsx`。
- Create: `web/src/components/primitives/StatusMessage/{StatusMessage.tsx,StatusMessage.module.css,StatusMessage.test.tsx,index.ts}` and update primitive barrel/root AGENTS component list。

## Shared Interfaces

```ts
export interface SaveAttempt<T> {
  requestSeq: number
  submittedSnapshot: T
  submittedEditVersion: number
}

export interface SaveResolution<T> {
  baseline: T
  draft: T
  dirty: boolean
  applied: boolean
}

export type WorkbenchOperationKey = {
  projectId: string
  worktreeId: string | null
  sequence: number
}

export type HttpRequestPolicy = {
  kind: 'query' | 'mutation' | 'longMutation' | 'eventStream'
  timeoutMs?: number
  signal?: AbortSignal
}

export type WorkbenchMutationEnvelope<T> =
  | { kind: 'succeeded'; value: T; clientOperationId: string }
  | { kind: 'unknown'; clientOperationId: string; transportClass?: 'timeout' | 'network' }
```

### Task 1: Implement Pure Save and Operation Context Contracts

**Files:**
- Create: `web/src/lib/asyncState/saveAttempt.ts`
- Create: `web/src/lib/asyncState/saveAttempt.test.ts`
- Create: `web/src/lib/asyncState/operationContext.ts`
- Create: `web/src/lib/asyncState/operationContext.test.ts`
- Create: `web/src/lib/asyncState/mutationOutcome.ts`
- Create: `web/src/lib/asyncState/mutationOutcome.test.ts`

**Interfaces:** Produces `createSaveAttempt`, `resolveSaveSuccess`, `isCurrentOperation` and transport-neutral `WorkbenchMutationEnvelope<T>` for Tasks 2–7.

- [ ] **Step 1: Write failing pure tests**

```ts
test('success updates baseline without replacing newer draft', () => {
  const result = resolveSaveSuccess({
    attempt: { requestSeq: 1, submittedSnapshot: 'A', submittedEditVersion: 1 },
    currentRequestSeq: 1,
    currentDraft: 'B',
    currentEditVersion: 2,
    serverValue: 'A',
  })
  expect(result).toEqual({ baseline: 'A', draft: 'B', dirty: true, applied: true })
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- saveAttempt.test.ts operationContext.test.ts mutationOutcome.test.ts`

Expected: FAIL because modules do not exist.

- [ ] **Step 3: Implement deterministic pure functions**

```ts
export function isCurrentOperation(current: WorkbenchOperationKey, settled: WorkbenchOperationKey): boolean {
  return current.projectId === settled.projectId
    && current.worktreeId === settled.worktreeId
    && current.sequence === settled.sequence
}
```

`resolveSaveSuccess` updates baseline on current response and hydrates draft only when edit version/snapshot are unchanged; stale response returns `applied:false`.

- [ ] **Step 4: Run tests**

Run: `cd web && npm test -- saveAttempt.test.ts operationContext.test.ts mutationOutcome.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/asyncState/saveAttempt.ts web/src/lib/asyncState/saveAttempt.test.ts web/src/lib/asyncState/operationContext.ts web/src/lib/asyncState/operationContext.test.ts web/src/lib/asyncState/mutationOutcome.ts web/src/lib/asyncState/mutationOutcome.test.ts
git commit -m "feat(frontend): add safe async state contracts"
```

### Task 2: Migrate ClaudeMd and Settings Safe Saves

**Files:**
- Modify: `web/src/pages/ClaudeMd/ClaudeMd.tsx`
- Create: `web/src/pages/ClaudeMd/ClaudeMd.test.tsx`
- Modify: `web/src/pages/Settings/useSettingsController.ts`
- Modify: `web/src/pages/Settings/Settings.test.tsx`

**Interfaces:** Consumes Task 1 save contract. Covers General, Cloud Sync, GitHub/AI, Prompt Optimizer, Health and Automation forms.

- [ ] **Step 1: Add deferred-response regression tests**

```ts
test('keeps edits typed while settings save is pending', async () => {
  const save = deferred<AppConfig>()
  mockConfigUpdate.mockReturnValue(save.promise)
  render(<Settings />)
  await user.type(screen.getByLabelText('设备名称'), 'A')
  await user.click(screen.getByRole('button', { name: '应用配置' }))
  await user.type(screen.getByLabelText('设备名称'), 'B')
  save.resolve(serverConfig('A'))
  expect(screen.getByLabelText('设备名称')).toHaveValue('AB')
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- ClaudeMd.test.tsx Settings.test.tsx`

Expected: FAIL because response rehydrates old snapshot.

- [ ] **Step 3: Integrate save attempts per form resource**

Increment edit version in existing onChange callbacks; capture attempt on submit; on success update saved baseline and conditionally hydrate. Keep errors scoped to the resource and preserve draft.

- [ ] **Step 4: Verify every save path**

Run: `cd web && npm test -- ClaudeMd.test.tsx Settings.test.tsx settingsState.test.ts`

Expected: PASS for all six Settings save groups.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/ClaudeMd/ClaudeMd.tsx web/src/pages/ClaudeMd/ClaudeMd.test.tsx web/src/pages/Settings/useSettingsController.ts web/src/pages/Settings/Settings.test.tsx
git commit -m "fix(frontend): preserve edits across save responses"
```

### Task 3: Guard Scratchpad Navigation and Sync Reload

**Files:**
- Modify: `web/src/pages/Scratchpad/Scratchpad.tsx`
- Modify: `web/src/pages/Scratchpad/Scratchpad.test.tsx`

**Interfaces:** Uses `{pageId,navigationSeq,draftVersion}`; existing autosave queue remains the write owner.

- [ ] **Step 1: Add B→C inverse-resolution tests**

```ts
test('latest page selection wins when responses resolve out of order', async () => {
  const b = deferred<ScratchpadPage>();
  const c = deferred<ScratchpadPage>();
  mockGetPage.mockReturnValueOnce(b.promise).mockReturnValueOnce(c.promise)
  await selectPage('B');
  await selectPage('C');
  c.resolve(page('C')); b.resolve(page('B'))
  expect(screen.getByRole('textbox')).toHaveValue('C')
})
```

Add sync reload while user edits current page.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- Scratchpad.test.tsx`

Expected: FAIL with B overwriting C or sync overwriting draft.

- [ ] **Step 3: Add navigation/request/draft guards**

Only latest seq + page target may update active page. Sync reload updates baseline but preserves draft when draftVersion changed. Keep all hooks before early returns.

- [ ] **Step 4: Run Scratchpad tests**

Run: `cd web && npm test -- Scratchpad.test.tsx scratchpadAutosave.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Scratchpad/Scratchpad.tsx web/src/pages/Scratchpad/Scratchpad.test.tsx
git commit -m "fix(scratchpad): discard stale page responses"
```

### Task 4: Preserve Devices Drafts and Roll Back CcHistory Actions

**Files:**
- Modify: `web/src/pages/Devices/Devices.tsx`
- Modify: `web/src/pages/Devices/Devices.test.tsx`
- Modify: `web/src/pages/CcHistory/CcHistory.tsx`
- Modify: `web/src/pages/CcHistory/CcHistory.test.tsx`

**Interfaces:** `saveTarget` returns `Promise<boolean>`; CcHistory stores deletion snapshot until acknowledgement and exposes retry after rollback, without a success Undo contract.

- [ ] **Step 1: Write failure recovery tests**

```ts
test('failed target save keeps the editable draft', async () => {
  mockSaveTarget.mockRejectedValue(new Error('offline'))
  render(<Devices />)
  await editAndSaveTarget('desk', '10.0.0.7')
  expect(screen.getByDisplayValue('10.0.0.7')).toBeVisible()
  expect(screen.getByRole('button', { name: '重试' })).toBeVisible()
})
```

Add CcHistory “保存到 Prompt” rejection with visible retry and delete rejection rollback/retry. Assert successful delete does not expose a fake Undo without a backend restore contract.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- Devices.test.tsx CcHistory.test.tsx`

Expected: FAIL because drafts clear/deletion stays removed.

- [ ] **Step 3: Implement explicit results and failed-delete rollback**

Only clear Devices fields after `true`. Keep CcHistory item snapshot until backend confirms; rollback on reject and show retry. “保存到 Prompt” failure uses the existing accessible alert surface and retains a retry action; Task 8 later migrates it to `StatusMessage`. Do not show a success Undo until a separate backend restore/vector-clock contract exists.

- [ ] **Step 4: Verify**

Run: `cd web && npm test -- Devices.test.tsx CcHistory.test.tsx`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Devices/Devices.tsx web/src/pages/Devices/Devices.test.tsx web/src/pages/CcHistory/CcHistory.tsx web/src/pages/CcHistory/CcHistory.test.tsx
git commit -m "fix(frontend): retain drafts and recover failed deletes"
```

### Task 5: Scope Desktop and Mobile Git Mutations

**Files:**
- Create: `src-tauri/src/workbench/operation_ledger.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/commands/workbench/git.rs`
- Modify: `src-tauri/src/commands/workbench/projects.rs`
- Modify: `src-tauri/src/commands/workbench/tests.rs`
- Modify: `src-tauri/src/workbench/remote_protocol.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/migrations/0001_init.sql`
- Modify: `web/src/lib/types/workbench.ts`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/api/workbench.test.ts`
- Modify: `docs/p2p-protocol.md`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.test.tsx`
- Create: `web/src/lib/workbenchMutationReconciliation.ts`
- Create: `web/src/lib/workbenchMutationReconciliation.test.ts`

**Interfaces:** Consumes Task 1 `WorkbenchOperationKey`/`WorkbenchMutationEnvelope<T>`; success/catch/finally all require current key. Produces sidecar `WorkbenchMutationOperation` ledger/status for commit/push/merge/remove and shared pure `reconcileWorkbenchMutation(intent,ledger,authorityAfter) -> confirmedSucceeded | unknown`. Definitive `AppError` remains backward-compatible; control timeout/network becomes `Ok({kind:'unknown',...})`, so desktop never parses error text.

- [ ] **Step 1: Add context-switch tests for success, error, finally**

```ts
test.each(['resolve', 'reject'])('old commit %s cannot mutate new context', async outcome => {
  const pending = deferred<void>()
  const view = renderGitController(project('A'), worktree('a'), pending.promise)
  const operation = view.result.current.commit('message')
  view.rerender(project('B'), worktree('b'))
  settle(pending, outcome)
  await operation.catch(() => undefined)
  expect(view.result.current.busy).toBe(false)
  expect(view.result.current.error).toBeNull()
})
```

Add Rust/TypeScript table tests for commit/push/merge/remove and control-response loss. Unknown envelopes contain only caller-known operation id/transport class; intent must come from the later ledger status DTO. Ledger claim precedes execution and stores canonical payload hash plus intent: commit `beforeHead + expectedTree`, push local/remote refs, merge source/main HEAD, remove exact worktree identity. Same ID/same payload replays status; same ID/different payload rejects. Commit only confirms when refreshed `newHead.parent=beforeHead && newHead.tree=expectedTree`; add same-parent/same-message/different-tree concurrent fixture. Push only confirms when remote ref reaches the captured local ref; merge only when main contains captured source HEAD and source worktree meets the postcondition; remove only when exact identity is absent. Ledger query failure/notFound/pending/unrelated movement stays `unknown` and never creates a second operation. Add mixed-version remote tests: new peer propagates id/envelope/status; old peer success maps to succeeded, while timeout/network maps to non-reconcilable unknown and is never replayed.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::operation_ledger && cargo test --locked commands::workbench::tests::mutation_outcome && cd ../web && npm test -- workbench.test.ts useWorkbenchWorktreeGitController.test.tsx workbenchMutationReconciliation.test.ts`

Expected: FAIL because catch/finally write global state.

- [ ] **Step 3: Guard every settlement path**

Create a global-domain `UNIQUE(client_operation_id)` ledger with canonical payload hash, intent, state and outcome. Sidecar claims before Git/worktree execution; commit intent computes the exact staged tree hash, not message identity. Add `get_workbench_mutation_operation` to control/P2P/Tauri adapters. Propagate request/envelope/status through `remote_protocol.rs`/`remote_client.rs` and advertise `workbench.mutation-outcome.v1` only after route, remote client/decoder and docs are all ready. Capability absent: perform exactly one legacy call; success maps to succeeded, uncertain transport becomes unknown without intent/status/replay. Tauri commands catch only uncertain control transport classes and return typed `unknown` through the success channel; validation/conflict remain existing `AppError`. Create operation context key/ref in the desktop controller and guard success/catch/finally. On unknown, query the owning ledger for intent, then refresh authority without replay; ledger unavailable/notFound/pending stays unknown. Confirm only exact intent postconditions, otherwise keep “结果未知，请刷新后人工核对”, disable the action and retain diagnostics.

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo test --locked workbench::operation_ledger && cargo test --locked commands::workbench && cargo test --locked backend::control_api && cargo test --locked workbench::remote_client && cd .. && node scripts/check-p2p-route-inventory.mjs && cd web && npm test -- workbench.test.ts workbenchMutationReconciliation.test.ts useWorkbenchWorktreeGitController.test.tsx WorkbenchWorktreeGit.characterization.test.tsx && npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/operation_ledger.rs src-tauri/src/workbench/mod.rs src-tauri/src/workbench/remote_protocol.rs src-tauri/src/workbench/remote_client.rs src-tauri/src/backend/runtime.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/commands/workbench/git.rs src-tauri/src/commands/workbench/projects.rs src-tauri/src/commands/workbench/tests.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/protocol.rs src-tauri/migrations/0001_init.sql docs/p2p-protocol.md web/src/lib/types/workbench.ts web/src/api/workbench.ts web/src/api/workbench.test.ts web/src/lib/workbenchMutationReconciliation.ts web/src/lib/workbenchMutationReconciliation.test.ts web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.ts web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.test.tsx
git commit -m "fix(workbench): scope git mutations to context"
```

### Task 6: Add Request Policies, Abort, and Safe Retry to Mobile HTTP

**Files:**
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `web/src/api/workbenchHttp.test.ts`
- Modify: `web/src/hooks/useWorkbenchHttpEvents.ts`
- Modify: `web/src/hooks/workbenchHttpEvents.test.ts`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `docs/p2p-protocol.md`

**Interfaces:** Implements transport-only `HttpRequestPolicy`; query overall=15s including decode, mutation=30s. Base transport keeps classified errors, while Workbench mutation wrappers consume the Task 5 operation id/envelope and map timeout/network to typed success-channel `unknown` for domain-controller reconciliation. Event stream uses typed heartbeat=15s, client watchdog=35s and separate lifecycle/per-connection abort controllers.

- [ ] **Step 1: Write fake-timer timeout/retry tests**

```ts
test('mutation wrapper maps timeout to typed unknown and does not replay', async () => {
  vi.useFakeTimers()
  mockFetch.mockImplementation(() => new Promise(() => undefined))
  const request = workbenchHttp.git.commit({ ...body, clientOperationId: 'op-1' })
  await vi.advanceTimersByTimeAsync(30_000)
  await expect(request).resolves.toMatchObject({ kind: 'unknown', clientOperationId: 'op-1' })
  expect(mockFetch).toHaveBeenCalledTimes(1)
})
```

Add query body-decode timeout, caller abort with no retry, and a half-open NDJSON stream whose per-connection controller aborts after 35 seconds then reconnects with a fresh controller while the lifecycle signal remains active. Verify business frames and typed heartbeat frames both reset the watchdog.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- workbenchHttp.test.ts workbenchHttpEvents.test.ts`

Expected: FAIL because postJson lacks policies/signals.

- [ ] **Step 3: Implement composed AbortSignal and classified errors**

Compose external signal with one overall timeout and clear it only after response decoding finishes. Retry only query/read-only POST twice with bounded backoff and only while visible. Base transport exposes only `timeout/callerAbort/network/protocol/decode`; each commit/push/merge/remove wrapper requires a stable clientOperationId, catches only timeout/network, and returns Task 5's typed `unknown` envelope without replay. Protocol/decode/definitive server errors remain errors; the domain controller owns status reconciliation. Keep one lifecycle AbortController for hook lifetime and create a fresh child controller for each connection. Server sends NDJSON `{"type":"heartbeat","sentAt":"<RFC3339>"}` every 15 seconds; parser handles it before business-event decoding. The 35-second watchdog aborts only the child and schedules reconnect unless lifecycle/context ended.

- [ ] **Step 4: Verify all mobile API callsites are classified**

Run: `cd web && npm test -- workbenchHttp.test.ts workbenchHttpEvents.test.ts && npm run build && cd ../src-tauri && cargo test --locked workbench_event_heartbeat`

Expected: PASS; TypeScript forces every mutation caller to choose a policy.

- [ ] **Step 5: Commit**

```bash
git add web/src/api/workbenchHttp.ts web/src/api/workbenchHttp.test.ts web/src/hooks/useWorkbenchHttpEvents.ts web/src/hooks/workbenchHttpEvents.test.ts src-tauri/src/net/routes/workbench.rs docs/p2p-protocol.md
git commit -m "feat(mobile): add bounded cancellable transport"
```

### Task 7: Repair Mobile Project Retry and Connection State

**Files:**
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/mobileWorkbenchState.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.test.ts`
- Create: `web/src/mobile/MobileWorkbench.test.tsx`
- Modify: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Modify: `web/src/mobile/components/MobileGitPanel.tsx`
- Create: `web/src/mobile/components/MobileGitPanel.test.tsx`
- Modify: `web/src/mobile/components/MobileWorktreePanel.tsx`
- Create: `web/src/mobile/components/MobileWorktreePanel.test.tsx`
- Modify: `web/src/mobile/mobilePanelState.ts`
- Modify: `web/src/mobile/mobilePanelState.test.ts`

**Interfaces:** Consumes Task 5 mutation ledger/envelope and Task 6 policies. Produces `MobileConnectionState`, project detail `idle/loading/ready/error`, and mobile commit/push/merge/remove `reconciling | confirmedSucceeded | unknown` state.

- [ ] **Step 1: Add failed-same-project retry and context-abort tests**

```ts
test('clicking the failed active project retries details', async () => {
mockLoadDetails.mockRejectedValueOnce(new Error('offline')).mockResolvedValueOnce(details())
  render(<MobileWorkbench />)
  await openProject('p1')
  await user.click(screen.getByRole('button', { name: '重新加载项目' }))
  expect(mockLoadDetails).toHaveBeenCalledTimes(2)
})
```

Add mobile mutation tests where the HTTP wrapper returns typed unknown: controller queries `get_workbench_mutation_operation`, refreshes Git/worktree authority, uses the shared expected-tree/ref/identity matrix, never issues a second mutation, and retains/locks ambiguous state. Include the same-message/different-tree commit case.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- MobileWorkbench.test.tsx mobileWorkbenchState.test.ts`

Expected: FAIL because same-project early return prevents reload.

- [ ] **Step 3: Implement detail/connection state machine**

Early return only for ready. Abort old project requests on selection change/unmount. Show cached/offline time and refresh current visible panel once connection returns. Generate/reuse stable operation IDs in mobile Git/worktree panels. On typed unknown, enter reconciling, query the sidecar ledger, refresh the authoritative panel and apply Task 5's pure matrix; confirmed success may advance UI, while pending/notFound/ambiguous remains unknown and never creates a new ID or blind replay.

- [ ] **Step 4: Verify mobile flows**

Run: `cd web && npm test -- MobileWorkbench.test.tsx mobileWorkbenchState.test.ts MobileGitPanel.test.tsx MobileWorktreePanel.test.tsx mobilePanelState.test.ts MobileWorktreeQuickSwitch.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/mobile/MobileWorkbench.tsx web/src/mobile/MobileWorkbench.test.tsx web/src/mobile/mobileWorkbenchState.ts web/src/mobile/mobileWorkbenchState.test.ts web/src/mobile/mobilePanelState.ts web/src/mobile/mobilePanelState.test.ts web/src/mobile/components/MobileWorkbenchShell.tsx web/src/mobile/components/MobileGitPanel.tsx web/src/mobile/components/MobileGitPanel.test.tsx web/src/mobile/components/MobileWorktreePanel.tsx web/src/mobile/components/MobileWorktreePanel.test.tsx
git commit -m "fix(mobile): retry failed projects and surface connection state"
```

### Task 8: Complete Accessible Async Feedback and Full Gates

**Files:**
- Modify: `web/src/components/domain/TagInput/TagInput.tsx`
- Create: `web/src/components/domain/TagInput/TagInput.test.tsx`
- Modify: `web/src/pages/Prompts/Prompts.tsx`
- Modify: `web/src/i18n/locales/zh/prompts.json`
- Modify: `web/src/i18n/locales/en/prompts.json`
- Modify: `web/src/pages/ClaudeMd/ClaudeMd.tsx`
- Modify: `web/src/pages/CcHistory/CcHistory.tsx`
- Modify: `web/src/pages/CcHistory/CcHistory.test.tsx`
- Modify: `web/src/mobile/components/MobileGitPanel.tsx`
- Modify: `web/src/mobile/components/MobileGitPanel.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchWorktreeGit.characterization.test.tsx`
- Modify: `web/src/i18n/locales/zh/common.json`
- Modify: `web/src/i18n/locales/en/common.json`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`
- Create: `web/src/components/primitives/StatusMessage/StatusMessage.tsx`
- Create: `web/src/components/primitives/StatusMessage/StatusMessage.module.css`
- Create: `web/src/components/primitives/StatusMessage/StatusMessage.test.tsx`
- Create: `web/src/components/primitives/StatusMessage/index.ts`
- Modify: `web/src/components/primitives/index.ts`
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `docs/prd.md`

- [ ] **Step 1: Add accessible-name/live-region tests**

Define TagInput accessible-name props as an XOR union: exactly one of `ariaLabel` or `ariaLabelledBy`. Assert the Prompts caller passes the translated tag-input name, success uses `role=status`, and blocking desktop/mobile Git/CcHistory failures use `role=alert` exactly once while busy buttons keep a stable name.

- [ ] **Step 2: Implement the reusable StatusMessage primitive**

Create `StatusMessage` with typed tone/live behavior and update AGENTS component inventory; migrate ClaudeMd, CcHistory save-to-Prompt/delete failures, desktop Workbench Git and MobileGitPanel failure surfaces to it. Views receive state/callbacks and do not import APIs.

- [ ] **Step 3: Run focused and full frontend gates**

```bash
cd web
npm run check:css-tokens
npm run check:i18n
npm run lint
npm run build
npm run check:bundle
npm test
npm run test:e2e
```

Expected: all exit 0.

- [ ] **Step 4: Update persistent contracts**

Document safe-save, operation context, mobile retry/timeout and accessible feedback in PRD/web CLAUDE; no task-summary Markdown.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/TagInput/TagInput.tsx web/src/components/domain/TagInput/TagInput.test.tsx web/src/components/primitives/StatusMessage/StatusMessage.tsx web/src/components/primitives/StatusMessage/StatusMessage.module.css web/src/components/primitives/StatusMessage/StatusMessage.test.tsx web/src/components/primitives/StatusMessage/index.ts web/src/components/primitives/index.ts web/src/pages/Prompts/Prompts.tsx web/src/pages/ClaudeMd/ClaudeMd.tsx web/src/pages/CcHistory/CcHistory.tsx web/src/pages/CcHistory/CcHistory.test.tsx web/src/mobile/components/MobileGitPanel.tsx web/src/mobile/components/MobileGitPanel.test.tsx web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/WorkbenchWorktreeGit.characterization.test.tsx web/src/i18n/locales/zh/prompts.json web/src/i18n/locales/en/prompts.json web/src/i18n/locales/zh/common.json web/src/i18n/locales/en/common.json web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json docs/prd.md web/CLAUDE.md AGENTS.md
git commit -m "feat(frontend): standardize async feedback contracts"
```

## Rollback and Failure Containment

- async helper 与全部 caller 作为一个兼容单元回退，禁止留下半数 caller 使用旧响应回填、半数使用新 sequence 的混合状态。
- 页面卸载/上下文切换先 abort 在途请求；回退 UI 时仍保留用户 draft 与 mutation `unknown`，不得因降级显示成功。
- 本轨道不新增持久 schema；失败可按 task commit 独立回退。

## Completion Contract

- Save/navigation/operation inverse-order tests pass and no newer draft/context is overwritten.
- Mobile query cancellation/retry and mutation unknown/reconciliation are explicit.
- Devices/CcHistory failures preserve or restore user-visible data.
- async feedback is accessible and full frontend gates pass.

## Plan Self-Review

- Spec coverage: safe-save, Scratchpad, Devices/CcHistory, Git, mobile transport/project retry and accessibility all map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: shared interfaces are consumed unchanged by all tasks.
