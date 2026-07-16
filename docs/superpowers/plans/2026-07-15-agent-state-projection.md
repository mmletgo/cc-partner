# Agent State Projection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将A1运行态自动投影到Desktop、Mobile、Attention和低噪音系统通知，同时保持projection-only和只导航合同。

**Architecture:** 前端使用外部runtime store完成snapshot→listen handshake与Gap重建；terminal/Mobile只消费selector。Attention v2在后端实时派生Agent/experiment异常，通知协调器消费owner revision event但不成为状态真值。

**Tech Stack:** React 19, TypeScript, Tauri events, Rust Attention sources, axum, Vitest, Playwright, existing primitives/tokens/i18n.

## Global Constraints

- 所有Hooks位于early return之前；不得新增`useWorkbenchController`或第八个Workbench controller。
- 复用Pill/StatusDot/Drawer/Dialog和现有tokens；不新增第三方状态库或modal。
- working/idle/completed默认不产生Attention；completed系统通知默认关闭。
- Attention和系统通知只导航/提醒，不执行输入、审批、retry或delivery。
- 通知文案不含项目名、任务标题、Prompt、diff、evidence、terminal正文或路径。
- `attention.v1`继续返回旧枚举；新枚举只出现在`attention.v2`。
- P2P business API继续无调用者身份鉴权；Attention capability只做v1/v2协议协商。
- UI实施前按`huashu-design`专家评审检查信息密度、低噪音和反装饰性icon；既有cc-partner设计系统是唯一视觉上下文。

---

## File Structure

- Create: `web/src/lib/types/agentRuntime.ts`, `web/src/lib/schemas/agentRuntime.ts`, `web/src/lib/agentRuntimeState.ts`。
- Create: `web/src/hooks/useAgentRuntime.ts`, `web/src/hooks/useOperationalNotifications.ts`。
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchTransport.ts}`。
- Modify: `web/src/hooks/useWorkbenchHttpEvents.ts`。
- Modify: `web/src/pages/Workbench/{WorkbenchSessionTabs.tsx,WorkbenchStatusCard.tsx,WorkbenchTerminalArea.tsx}`、`web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts`、`web/src/mobile/{mobileRuntimeSnapshotStore.ts,mobileRuntimeSnapshotStore.test.ts}`、`web/src/mobile/components/{MobileTerminalPanel.tsx,MobileAttentionPanel.tsx}`。
- Create: `src-tauri/src/attention/agent_runtime_source.rs`。
- Modify: `src-tauri/src/attention/{mod.rs,models.rs,aggregator.rs}` and attention routes/commands.
- Modify: `web/src/lib/{attention.ts,types/attention.ts,schemas/attention.ts}`、`web/src/i18n/locales/{zh/attention.json,en/attention.json}`。
- Create: `src-tauri/src/operational_notifications/{mod.rs,models.rs,source.rs,snapshot.rs}` and `src-tauri/src/commands/operational_notifications.rs`。
- Modify: `src-tauri/src/backend/{event_bus.rs,control_api.rs}`、`src-tauri/src/net/http_server.rs`、`src-tauri/src/{config_runtime.rs,state.rs,lib.rs}`、`src-tauri/src/orchestrator/{models.rs,outbox.rs}`、`src-tauri/src/orchestrator/repo/{schema.rs,tasks.rs}`。
- Modify: `web/src/lib/notification.ts`, `web/src/App.tsx`, `web/src/pages/Settings/{AutomationSettingsPanel.tsx,automationSettingsState.ts,useSettingsController.ts,Settings.test.tsx}` and `web/src/i18n/locales/{zh/settings.json,en/settings.json}`.

## Task Dependency Graph

```text
T1 → T2 → (T3 | T4) → T5 → T6 → T7
```

### Task 1: Add Strict Runtime Types and Pure Reducer

**Files:**
- Create: `web/src/lib/types/agentRuntime.ts`
- Create: `web/src/lib/schemas/agentRuntime.ts`
- Create: `web/src/lib/schemas/agentRuntime.test.ts`
- Create: `web/src/lib/agentRuntimeState.ts`
- Create: `web/src/lib/agentRuntimeState.test.ts`

**Interfaces:**
- Produces: `AgentSessionProjection`, `AgentRuntimeSnapshot`, `applyAgentRuntimeSnapshot`, `applyAgentRuntimeEvent`, `latestAgentForTerminal`.

- [ ] **Step 1: Write failing decoder/reducer tests**

```ts
it('does not let an older version replace a newer terminal agent', () => {
  const current = applyAgentRuntimeEvent(emptyAgentRuntimeState(), event({ id: 'a', terminalSessionId: 't', version: 3 }))
  const next = applyAgentRuntimeEvent(current, event({ id: 'a', terminalSessionId: 't', version: 2, phase: 'failed' }))
  expect(latestAgentForTerminal(next, 't')?.version).toBe(3)
})
```

Add strict enum/RFC3339/truncated/snapshot replacement and corrupt DTO cases.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- agentRuntime.test.ts agentRuntimeState.test.ts`

Expected: FAIL because files are absent.

- [ ] **Step 3: Implement exact types and reducer**

```ts
export type AgentPhase = 'launching' | 'working' | 'needsInput' | 'idle' | 'completed' | 'failed' | 'disconnected'
export type AgentFreshness = 'live' | 'cached' | 'offline' | 'unsupported'

export interface AgentRuntimeState {
  ownerInstanceId: string | null
  asOfSequence: number
  byAgentId: ReadonlyMap<string, AgentSessionProjection>
  latestAgentIdByTerminal: ReadonlyMap<string, string>
}
```

Use immutable Map replacement and `(ownerInstanceId,version)` guards; do not read Orchestrator legacy runtime fields.

- [ ] **Step 4: Run GREEN**

Run: `cd web && npm test -- agentRuntime.test.ts agentRuntimeState.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/types/agentRuntime.ts web/src/lib/schemas/agentRuntime.ts web/src/lib/schemas/agentRuntime.test.ts web/src/lib/agentRuntimeState.ts web/src/lib/agentRuntimeState.test.ts
git commit -m "feat(web): model agent runtime projections"
```

### Task 2: Implement Snapshot/Event Handshake for Desktop and Mobile

**Files:**
- Create: `web/src/hooks/useAgentRuntime.ts`
- Create: `web/src/hooks/useAgentRuntime.test.tsx`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `web/src/api/workbenchTransport.ts`
- Modify: `web/src/hooks/useWorkbenchHttpEvents.ts`
- Modify: `web/src/hooks/workbenchHttpEvents.test.ts`

**Interfaces:**
- Consumes: A1 command/event and Task 1 reducer.
- Produces: `useAgentRuntime(projectId)` and Mobile callbacks `onTerminalStatus/onAgentRuntime`.

- [ ] **Step 1: Write handshake/Gap tests**

```tsx
it('buffers events until snapshot baseline and refetches on gap', async () => {
  const fixture = renderAgentRuntimeHook()
  fixture.emit(agentEvent({ sequence: 12, phase: 'needsInput' }))
  fixture.resolveSnapshot(snapshot({ asOfSequence: 10 }))
  expect(fixture.current('a')?.phase).toBe('needsInput')
  fixture.emitGap()
  expect(fixture.snapshotCalls()).toBe(2)
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- useAgentRuntime.test.tsx workbenchHttpEvents.test.ts`

Expected: FAIL because hook/callbacks are absent.

- [ ] **Step 3: Implement listener-first handshake**

Register Tauri listener before requesting snapshot, buffer by owner/sequence, establish baseline, discard `<=asOfSequence`, drain later events, then enter live mode. Gap/owner change pauses application and repeats handshake. Mobile unknown event returns `null` and continues stream; known malformed event remains protocol error.

- [ ] **Step 4: Run GREEN**

Run: `cd web && npm test -- useAgentRuntime.test.tsx workbenchHttpEvents.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/hooks/useAgentRuntime.ts web/src/hooks/useAgentRuntime.test.tsx web/src/api/workbench.ts web/src/api/workbenchHttp.ts web/src/api/workbenchTransport.ts web/src/hooks/useWorkbenchHttpEvents.ts web/src/hooks/workbenchHttpEvents.test.ts
git commit -m "feat(web): reconcile agent runtime events"
```

### Task 3: Project Agent Phase into Desktop Terminal Surfaces

**Files:**
- Modify: `web/src/pages/Workbench/WorkbenchSessionTabs.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchStatusCard.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchTerminalArea.tsx`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts`
- Modify: `web/src/pages/Workbench/{WorkbenchTerminal.characterization.test.tsx,WorkbenchTerminalPane.test.tsx}` and `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx`.

**Interfaces:**
- Consumes: `latestAgentForTerminal`.
- Produces: accessible status label and navigation only.

- [ ] **Step 1: Write phase rendering tests**

```tsx
it.each([
  ['working', 'Agent 工作中'],
  ['needsInput', 'Agent 等待输入'],
  ['failed', 'Agent 运行失败'],
])('renders %s with text and aria label', (phase, label) => {
  renderSessionTab({ agent: makeAgent({ phase }) })
  expect(screen.getByLabelText(label)).toBeVisible()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkbenchSessionTabs WorkbenchTerminal`

Expected: FAIL because Agent props/rendering are absent.

- [ ] **Step 3: Implement low-noise projection**

Use existing `Pill`/`StatusDot`; show provider short label plus phase. Do not animate working/idle, do not add decorative icons, and keep all hooks above early returns. Clicking focuses the existing terminal only.

- [ ] **Step 4: Run GREEN and design-token checks**

Run: `cd web && npm test -- WorkbenchSessionTabs WorkbenchTerminal && npm run check:css-tokens && npm run check:i18n`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench/WorkbenchSessionTabs.tsx web/src/pages/Workbench/WorkbenchStatusCard.tsx web/src/pages/Workbench/WorkbenchTerminalArea.tsx web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat(workbench): show agent session status"
```

### Task 4: Consume Terminal and Agent Status on Mobile

**Files:**
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/components/MobileTerminalPanel.tsx`
- Modify: `web/src/mobile/mobileRuntimeSnapshotStore.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.test.ts`
- Modify: `web/src/mobile/mobileTerminalReplay.test.ts`

**Interfaces:**
- Consumes: `onTerminalStatus`, `onAgentRuntime`.
- Produces: current-session status and terminal navigation.

- [ ] **Step 1: Write failing realtime state tests**

```ts
it('applies terminalStatus and agentRuntime to the selected mobile session', () => {
  const state = reduceMobileRuntime(initialState(), terminalStatusEvent('s1', 'disconnected'))
  const next = reduceMobileRuntime(state, agentRuntimeEvent('s1', 'needsInput'))
  expect(next.sessions.s1.status).toBe('disconnected')
  expect(next.sessions.s1.agent?.phase).toBe('needsInput')
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- mobileWorkbenchState.test.ts mobileTerminalReplay.test.ts`

Expected: FAIL because terminalStatus/Agent event are not consumed.

- [ ] **Step 3: Implement Mobile reducer and view**

Apply only events for known current project/session; cached/offline text must be explicit. Agent status click selects the existing terminal panel and never sends input.

- [ ] **Step 4: Run GREEN**

Run: `cd web && npm test -- mobileWorkbenchState.test.ts mobileTerminalReplay.test.ts MobileTerminalPanel`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/mobile/MobileWorkbench.tsx web/src/mobile/components/MobileTerminalPanel.tsx web/src/mobile/mobileRuntimeSnapshotStore.ts web/src/mobile/mobileWorkbenchState.test.ts web/src/mobile/mobileTerminalReplay.test.ts
git commit -m "feat(mobile): show live agent status"
```

### Task 5: Add Attention v2 Agent and Experiment Projections

**Files:**
- Create: `src-tauri/src/attention/agent_runtime_source.rs`
- Modify: `src-tauri/src/attention/{mod.rs,models.rs,aggregator.rs}`
- Modify: `src-tauri/src/commands/attention.rs`
- Modify: `src-tauri/src/net/routes/attention.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `web/src/lib/{attention.ts,types/attention.ts,schemas/attention.ts}`
- Modify: `web/src/pages/Attention/{Attention.tsx,attentionView.test.tsx}`、`web/src/mobile/{mobileAttentionTarget.ts,mobileAttentionTarget.test.ts,MobileAttentionPanel.test.tsx}`、`web/src/mobile/components/MobileAttentionPanel.tsx`。

**Interfaces:**
- Produces: `attention.v2`, active `AgentNeedsInput`/`AgentFailed` sources, and forward-compatible `ExperimentNeedsDecision`/`Experiment` contracts for A4 registration.

- [ ] **Step 1: Write v1/v2 isolation tests**

```rust
#[tokio::test]
async fn attention_v1_never_serializes_agent_variants() {
    let state = state_with_agent_needs_input().await;
    assert!(list_attention_items_v1(&state).await.unwrap().iter().all(|item| !item.id.starts_with("agent:")));
    assert_eq!(list_attention_items_v2(&state).await.unwrap().iter().filter(|item| item.id.starts_with("agent:")).count(), 1);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked attention --lib`

Expected: FAIL because v2/source/targets are absent.

- [ ] **Step 3: Implement realtime sources and navigation DTOs**

Stable Agent IDs are `agent:needs-input:<agentSessionId>` and `agent:failed:<agentSessionId>`. Derive them from Agent runtime without a new Attention table. Define and decode the future `experiment:decision:<experimentId>` contract, but do not query an experiment repo in A2; A4 registers that source after its reducer exists. Add v2 route/capability while leaving v1 unchanged; frontend prefers v2 and falls back to v1.

- [ ] **Step 4: Run backend/frontend GREEN**

Run: `cd src-tauri && cargo test --locked attention --lib && cargo test --locked commands::attention --lib && cargo test --locked net::routes::attention --lib && cd ../web && npm test -- attention mobileAttentionTarget MobileAttentionPanel`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/attention src-tauri/src/commands/attention.rs src-tauri/src/net/routes/attention.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs web/src/lib/attention.ts web/src/lib/types/attention.ts web/src/lib/schemas/attention.ts web/src/mobile/mobileAttentionTarget.ts web/src/mobile/components/MobileAttentionPanel.tsx web/src/i18n/locales/zh/attention.json web/src/i18n/locales/en/attention.json
git commit -m "feat(attention): project agent exceptions"
```

### Task 6: Implement Privacy-safe Operational Notifications

**Files:**
- Create: `web/src/api/operationalNotifications.ts`
- Create: `web/src/hooks/useOperationalNotifications.ts`
- Create: `web/src/hooks/useOperationalNotifications.test.tsx`
- Modify: `web/src/lib/notification.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/pages/Settings/{AutomationSettingsPanel.tsx,automationSettingsState.ts,automationSettingsState.test.ts,useSettingsController.ts,Settings.test.tsx}`
- Modify: `web/src/i18n/locales/{zh/settings.json,en/settings.json}`
- Create: `src-tauri/src/operational_notifications/{mod.rs,models.rs,source.rs,snapshot.rs}`
- Create: `src-tauri/src/commands/operational_notifications.rs`
- Modify: `src-tauri/src/backend/{event_bus.rs,control_api.rs}`、`src-tauri/src/net/http_server.rs`、`src-tauri/src/{config_runtime.rs,state.rs,lib.rs}`、`src-tauri/src/orchestrator/{models.rs,outbox.rs}`、`src-tauri/src/orchestrator/repo/{schema.rs,tasks.rs}`

**Interfaces:**
- Produces: `OperationalNotificationEvent`, `OperationalNotificationSnapshot`, dedupe key `{kind,opaqueSourceId,stateVersion}`.

- [ ] **Step 1: Write baseline/dedupe/privacy tests**

```tsx
it('establishes first snapshot without notifying historical states', async () => {
  const fixture = renderOperationalNotifications({ snapshot: [state('agentNeedsInput', 3)] })
  await fixture.ready()
  expect(fixture.sendNotification).not.toHaveBeenCalled()
  fixture.emit(state('agentNeedsInput', 4))
  expect(fixture.sendNotification).toHaveBeenCalledTimes(1)
})
```

Assert title/body are fixed translations and action/onAction/extra payload are absent.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- useOperationalNotifications.test.tsx`

Expected: FAIL because coordinator is absent.

- [ ] **Step 3: Implement owner revision event and GUI coordinator**

Use listener-first snapshot handshake identical to runtime. Defaults: needsInput/failed/experimentDecision/blocked/outboxFailed on; completed off. `experimentDecision` is a contract/settings entry only until A4 emits it. Request permission only from explicit Settings action; foreground authority suppresses OS notification.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked operational_notification && cd ../web && npm test -- useOperationalNotifications operationalNotifications Settings useAttention && npm run check:i18n`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/operational_notifications src-tauri/src/commands/operational_notifications.rs src-tauri/src/orchestrator src-tauri/src/backend src-tauri/src/net/http_server.rs src-tauri/src/config_runtime.rs src-tauri/src/state.rs src-tauri/src/lib.rs web/src/api/operationalNotifications.ts web/src/hooks/useOperationalNotifications.ts web/src/hooks/useOperationalNotifications.test.tsx web/src/lib/notification.ts web/src/App.tsx web/src/pages/Settings web/src/i18n/locales
git commit -m "feat(notifications): surface agent exceptions"
```

### Task 7: E2E, Accessibility, Mixed-version and Documentation

**Files:**
- Modify: `web/tests/workbench.spec.ts`
- Modify: `web/tests/mobile-workbench.spec.ts`
- Modify: `web/tests/attention.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

**Interfaces:** Consumes Tasks 1–6 and records persistent truth.

- [ ] **Step 1: Add complete journeys**

Cover working→needsInput→Attention→terminal, failed remote cached/offline, Gap refetch, old attention.v1, completed notification default-off and one-tab-stop Attention navigation.

- [ ] **Step 2: Run frontend gates**

Run: `cd web && npm run lint && npm run build && npm test && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts attention.spec.ts`

Expected: all exit 0.

- [ ] **Step 3: Run backend/protocol gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked attention && cargo test --locked operational_notification && cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-docs.mjs`

Expected: all exit 0.

- [ ] **Step 4: Commit**

```bash
git add web/tests/workbench.spec.ts web/tests/mobile-workbench.spec.ts web/tests/attention.spec.ts docs/prd.md docs/p2p-protocol.md docs/development/quality-matrix.json web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs: define agent state projection behavior"
```

## Completion Contract

- Desktop/Mobile use A1 runtime, not legacy Claude task fields.
- Attention v1 remains strict-compatible; v2 adds only navigation projections.
- Notifications are deduped, privacy-safe, foreground-aware and informational.
- Normal working/completed paths create no default Attention or OS noise.

## Plan Self-Review

- Spec coverage: reducer, transport, Desktop, Mobile, Attention, notifications and compatibility each have a task.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: phase/source/target names match spec and v2 wire values.
