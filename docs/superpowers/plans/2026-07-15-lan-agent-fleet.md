# LAN Agent Fleet Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为用户已保存的local/remote project shortcut提供按owning device批量聚合的Agent、Attention、Git、browser和Orchestrator只读视图。

**Architecture:** owner-local collector从现有repos/services构建field-level摘要；控制设备按device分组并以有界并发fan-out，最后cache仅作display。Project Rail只显示异常摘要，`/workbench/fleet`提供详情且所有操作只导航到既有authority。

**Tech Stack:** Rust Workbench/Orchestrator/Attention, reqwest/axum P2P, React 19, existing visibility polling, CSS Modules/tokens, Vitest/Playwright.

## Global Constraints

- Fleet只聚合当前控制设备已保存的project shortcuts；不枚举远端全部project。
- 不自动调度、迁移、复制repo、创建task或改变max concurrency。
- mDNS/capability只表达可达性和协议，不称设备认证/可信/安全。
- owner batch最多100 projects；全局snapshot最多500 projects；remote fan-out最多3 devices并发。
- page visible时30秒safety reconcile，hidden停止；event invalidation优先。
- 单device/field失败不清空其他live结果，cached/offline/unsupported必须显式。
- Fleet不显示绝对remote path、Prompt、terminal output或Ledger逐session明细。
- P2P business API继续无调用者身份鉴权；Fleet capability和reachability不得表述为授权或设备信任。
- UI复用现有Rail、Pill/StatusDot和tokens，不增加全局Sidebar入口或装饰性统计卡。

---

## File Structure

- Create: `src-tauri/src/workbench/lan_fleet/{mod.rs,models.rs,collector.rs,cache.rs}`。
- Create: `src-tauri/src/commands/workbench/fleet.rs`。
- Modify: `src-tauri/src/orchestrator/repo/{tasks.rs,tests.rs}`、`src-tauri/src/attention/aggregator.rs`、`src-tauri/src/workbench/remote_client.rs`、`src-tauri/src/net/routes/workbench.rs`、`src-tauri/src/net/{http_server.rs,protocol.rs,discovery.rs}`、`src-tauri/src/backend/{control_workbench.rs,control_client.rs}`。
- Create: `web/src/lib/types/lanFleet.ts`, `web/src/lib/schemas/lanFleet.ts`, `web/src/hooks/useLanAgentFleet.ts`。
- Create: `web/src/pages/Workbench/views/WorkbenchFleetView.tsx` and test/style.
- Modify: `WorkbenchProjectRail`, Workbench route/controller wiring and Mobile project panel.

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → (T5 | T6) → T7
```

### Task 1: Build Local Project Summaries with True Device Slots

**Files:**
- Create: `src-tauri/src/workbench/lan_fleet/{mod.rs,models.rs,collector.rs}`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/orchestrator/repo/tasks.rs`
- Modify: `src-tauri/src/orchestrator/repo/tests.rs`

**Interfaces:**
- Produces: `LanFleetProjectSummary`, `build_local_fleet_project`, `count_active_slots_for_device`.

- [ ] **Step 1: Write phase/slot/field-failure tests**

```rust
#[tokio::test]
async fn device_slots_include_active_tasks_from_other_projects() {
    let fixture = fleet_fixture().active_task("p1").active_task("p2").selected_project("p1").await;
    let summary = fixture.collect("p1").await.unwrap();
    assert_eq!(summary.device_slots_used, 2);
    assert_eq!(summary.project_orchestrator_running, 1);
}
```

Add Agent phase counts, Attention count, Git error→unknown and browser absent cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::lan_fleet::collector --lib`

Expected: FAIL because collector/device-global slot helper are absent.

- [ ] **Step 3: Implement owner-local collector**

Query A1 active runtime, Attention aggregator, terminal repo, read-only Git status, browser registry and Orchestrator repos. Each subsource returns `Known(value)|Unknown(code)`; scheduler slots use all active local projects on the owner, never current-project `slotsUsed`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench::lan_fleet::collector --lib && cargo test --locked orchestrator::repo --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/lan_fleet src-tauri/src/workbench/mod.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/orchestrator/repo/tests.rs
git commit -m "feat(workbench): collect local fleet summaries"
```

### Task 2: Add Owning-device Batch Snapshot Route

**Files:**
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/net/discovery.rs`

**Interfaces:**
- Produces: `POST /api/workbench/lan-fleet/snapshot`, `workbench.lan-fleet.v1`, `RemoteWorkbenchClient::lan_fleet_snapshot`.

- [ ] **Step 1: Write saved-scope/resource/recursion tests**

```rust
#[tokio::test]
async fn owner_batch_rejects_remote_project_ids_and_caps_projects() {
    let state = fleet_route_state().await;
    assert_eq!(post_snapshot(&state, vec!["remote:d:p".into()]).await.unwrap_err().code(), "local_project_required");
    assert_eq!(post_snapshot(&state, ids(101)).await.unwrap_err().code(), "resource_limit");
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked net::routes::workbench --lib`

Expected: FAIL because route/capability are absent.

- [ ] **Step 3: Implement local-only batch route**

Accept only owner local project IDs resolved from requested saved paths/IDs; never call another Fleet route. Return max100 project summaries and max500 active Agent refs with no remote absolute paths.

- [ ] **Step 4: Run GREEN/protocol tests**

Run: `cd src-tauri && cargo test --locked net::routes::workbench --lib && cargo test --locked workbench::remote_client --lib && cargo test --locked net::protocol --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/remote_client.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs src-tauri/src/net/discovery.rs
git commit -m "feat(p2p): expose owner fleet summaries"
```

### Task 3: Aggregate Saved Shortcuts with Bounded Fan-out and Cache

**Files:**
- Create: `src-tauri/src/workbench/lan_fleet/cache.rs`
- Modify: `src-tauri/src/workbench/lan_fleet/collector.rs`
- Create: `src-tauri/src/commands/workbench/fleet.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/control_workbench.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `collect_lan_fleet_for_state`, `get_workbench_lan_fleet`.

- [ ] **Step 1: Write device dedupe/partial cache tests**

```rust
#[tokio::test]
async fn two_shortcuts_on_one_device_make_one_remote_call() {
    let fixture = global_fleet_fixture().remote_shortcut("d1", "p1").remote_shortcut("d1", "p2");
    let snapshot = fixture.collect().await.unwrap();
    assert_eq!(fixture.calls_to("d1"), 1);
    assert_eq!(snapshot.devices[0].projects.len(), 2);
}
```

Add 3-device concurrency, 5-second timeout, one-device failure and cached/offline cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::lan_fleet --lib`

Expected: FAIL because global collector/cache are absent.

- [ ] **Step 3: Implement by-device fan-out**

Read only persisted project shortcuts, group by device, health/capability gate, fan-out with semaphore=3 and per-device timeout=5s. Cache last successful sanitized device DTO in memory; offline uses cache with capturedAt, never scheduler truth.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench::lan_fleet --lib && cargo test --locked commands::workbench --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/lan_fleet src-tauri/src/commands/workbench/fleet.rs src-tauri/src/commands/workbench/mod.rs src-tauri/src/backend/control_workbench.rs src-tauri/src/backend/control_client.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): aggregate LAN agent fleet"
```

### Task 4: Add Strict Frontend Schema and Visibility-aware Hook

**Files:**
- Create: `web/src/lib/types/lanFleet.ts`
- Create: `web/src/lib/schemas/lanFleet.ts`
- Create: `web/src/lib/schemas/lanFleet.test.ts`
- Create: `web/src/hooks/useLanAgentFleet.ts`
- Create: `web/src/hooks/useLanAgentFleet.test.tsx`
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchTransport.ts}`

**Interfaces:**
- Produces: `useLanAgentFleet` with event invalidation, requestSeq and 30-second visible reconcile.

- [ ] **Step 1: Write stale/visibility/partial tests**

```tsx
it('stops safety polling while hidden and drops stale responses', async () => {
  const fixture = renderFleetHook()
  fixture.setHidden(true)
  fixture.advance(60_000)
  expect(fixture.calls()).toBe(1)
  fixture.setHidden(false)
  expect(fixture.calls()).toBe(2)
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- lanFleet.test.ts useLanAgentFleet.test.tsx`

Expected: FAIL because schema/hook are absent.

- [ ] **Step 3: Implement strict decoder and hook**

Reject negative counts/invalid freshness but retain field-level unknown. Use existing visibility polling helper; event invalidation coalesces within500ms and requestSeq prevents old response overwrite.

- [ ] **Step 4: Run GREEN**

Run: `cd web && npm test -- lanFleet.test.ts useLanAgentFleet.test.tsx`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/types/lanFleet.ts web/src/lib/schemas/lanFleet.ts web/src/lib/schemas/lanFleet.test.ts web/src/hooks/useLanAgentFleet.ts web/src/hooks/useLanAgentFleet.test.tsx web/src/api/workbench.ts web/src/api/workbenchHttp.ts web/src/api/workbenchTransport.ts
git commit -m "feat(web): load LAN fleet snapshots"
```

### Task 5: Add Low-noise Project Rail Summaries

**Files:**
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx`
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.module.css`
- Modify: `web/src/components/domain/WorkbenchProjectRail/{WorkbenchProjectRail.test.tsx,projectSessionStats.test.ts,workbenchProjectRailStyles.test.ts}` and `web/src/lib/workbenchProjectStats.ts`.
- Modify: `web/src/i18n/locales/{zh/workbench.json,en/workbench.json}`

**Interfaces:**
- Consumes: project-indexed fleet summaries.
- Produces: Agent status point, exception badge, offline text and Fleet navigation.

- [ ] **Step 1: Write exception-only badge tests**

```tsx
it('does not badge normal working agents but badges needs-input', () => {
  const { rerender } = renderRail(summary({ working: 4, needsInput: 0, failed: 0 }))
  expect(screen.queryByLabelText(/需要处理/)).not.toBeInTheDocument()
  rerender(rail(summary({ working: 4, needsInput: 1, failed: 0 })))
  expect(screen.getByLabelText('1 个 Agent 需要处理')).toBeVisible()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkbenchProjectRail`

Expected: FAIL because Fleet props are absent.

- [ ] **Step 3: Implement compact projection**

Show one status point and exception count; working count stays hover/accessible description. Offline/cached/unsupported use text+icon. Add one Rail-header Fleet link; no new Sidebar item or inline actions.

- [ ] **Step 4: Run GREEN/design gates**

Run: `cd web && npm test -- WorkbenchProjectRail && npm run check:css-tokens && npm run check:i18n`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/WorkbenchProjectRail web/src/lib/workbenchProjectStats.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat(workbench): summarize fleet status in project rail"
```

### Task 6: Add Workbench Fleet View and Mobile Summary

**Files:**
- Create: `web/src/pages/Workbench/views/WorkbenchFleetView.tsx`
- Create: `web/src/pages/Workbench/views/{WorkbenchFleetView.module.css,WorkbenchFleetView.test.tsx}`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/mobile/components/MobileProjectPanel.tsx`
- Create: `web/src/mobile/components/MobileProjectPanel.test.tsx`
- Modify: `web/src/mobile/MobileWorkbench.module.css`

**Interfaces:**
- Produces: `/workbench/fleet` read-only device/project grouping and authority navigation.

- [ ] **Step 1: Write no-mutation/navigation tests**

```tsx
it('offers only navigation actions', () => {
  render(<WorkbenchFleetView snapshot={fleetFixture()} onNavigate={navigate} />)
  expect(screen.queryByRole('button', { name: /运行|迁移|复制|发送/ })).not.toBeInTheDocument()
  expect(screen.getByRole('link', { name: /打开项目/ })).toBeVisible()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkbenchFleetView MobileProjectPanel`

Expected: FAIL because view is absent.

- [ ] **Step 3: Implement grouped high-density read-only view**

Device header shows reachability/global slots/capturedAt; project row shows Agent counts, Attention, Git, browser and Orchestrator fields. Use content density, not decorative icon cards. Links navigate existing Workbench/Automation/Attention. Mobile embeds a concise section in project panel, not a new main nav.

- [ ] **Step 4: Run GREEN and Workbench size guard**

Run: `cd web && npm test -- WorkbenchFleetView MobileProjectPanel && npm run build && test $(wc -l < src/pages/Workbench/Workbench.tsx) -le 1200`

Expected: PASS and Workbench.tsx≤1200.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench/views/WorkbenchFleetView.tsx web/src/pages/Workbench/views/WorkbenchFleetView.module.css web/src/pages/Workbench/views/WorkbenchFleetView.test.tsx web/src/pages/Workbench/Workbench.tsx web/src/App.tsx web/src/mobile/components/MobileProjectPanel.tsx web/src/mobile
git commit -m "feat(workbench): add LAN agent fleet view"
```

### Task 7: Protocol, E2E, Accessibility and Documentation

**Files:**
- Modify: `docs/p2p-protocol.md`, `scripts/check-p2p-route-inventory.mjs`, `docs/prd.md`, `docs/development/quality-matrix.json`.
- Modify: `web/tests/{workbench.spec.ts,mobile-workbench.spec.ts}`
- Modify: `web/CLAUDE.md`, `src-tauri/CLAUDE.md`

**Interfaces:** Consumes Tasks 1–6.

- [ ] **Step 1: Add local/live/offline/unsupported E2E journeys**

Verify keyboard navigation, text alternatives, cached timestamps and Attention/project authority links; assert no mutation requests originate from Fleet view.

- [ ] **Step 2: Run complete gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked workbench::lan_fleet && cargo test --locked net::routes::workbench && cd ../web && npm run lint && npm run build && npm test -- Fleet WorkbenchProjectRail MobileProjectPanel && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts && cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs`

Expected: all exit 0.

- [ ] **Step 3: Audit forbidden mutations/wording**

Run: `rg -n "create.*task|copy.*repo|migrate|trusted|authenticated|认证|可信" web/src/pages/Workbench/views/WorkbenchFleetView.tsx src-tauri/src/workbench/lan_fleet docs/prd.md`

Expected: no product mutation/trust claim; any test fixture matches are explicitly negative assertions.

- [ ] **Step 4: Commit**

```bash
git add docs/p2p-protocol.md scripts/check-p2p-route-inventory.mjs docs/prd.md docs/development/quality-matrix.json web/tests/workbench.spec.ts web/tests/mobile-workbench.spec.ts web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs: define LAN agent fleet behavior"
```

## Completion Contract

- One device receives one bounded batch request for its saved shortcuts.
- Device-global slots are not inferred from a project-local count.
- Project Rail is quiet for normal work and highlights only exceptions/offline state.
- Fleet offers navigation only and never becomes scheduler truth.

## Plan Self-Review

- Spec coverage: local collector, owner route, global fan-out/cache, frontend state, Rail, Fleet/Mobile and docs each map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: reachability/freshness/project summary names stay stable across Rust and TS.
