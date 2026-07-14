# Core Workbench Experience and LAN Onboarding Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 保持 Trending 为默认首页，把“继续工作”收敛到 Workbench 启动页，修复侧栏/Workbench 空态和移动导航，并在 GUI 第一次启动 LAN sidecar 前完成一次性无身份风险确认。

**Architecture:** 保留现有设计系统与 `/`→Home/Trending 路由。Workbench 在“有项目但未选中”时消费 sidecar 有界只读 `WorkbenchLaunchSummary`；完全无项目时只显示聚焦 CTA，已有 active project 时维持现有工作区。启动摘要状态并入现有 `useWorkbenchProjectController`，不增加第八个页面 controller。AppShell 分组但继续复用 NavItem/ProjectRail。LAN disclosure 写入 launcher-owned `gui-bootstrap.json`；App-level `LanDisclosureGate` + hook 覆盖新用户、升级用户与 Welcome skip，Rust startup coordinator 同时控制 ensure 与 GUI backend services。实现阶段用 `huashu-design` 对既有语言内的布局进行截图验证。

**Tech Stack:** React 19, React Router v6, TypeScript, CSS Modules/tokens, Tauri 2/Rust atomic launcher bootstrap file, Vitest, Playwright.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-core-workbench-experience-and-lan-onboarding-design.md`、根 `AGENTS.md`、`web/CLAUDE.md`；涉及视觉实现时使用 `huashu-design`，不得另造品牌方向。
- 所有 CSS 值来自 tokens；新增颜色/间距 token 同时定义浅深主题。
- 合法 LAN peer 仍无身份鉴权；disclosure 不是 LAN 模式、token、配对或权限矩阵。
- `gui-bootstrap.json` 只能存 disclosure version/timestamp，不得复制任何 sidecar runtime config；AppConfig 继续由 N1 owner 控制。
- `/` 必须继续显示 Trending；不得新增 `/discover` 搬迁页或把 Workbench 摘要塞入 Home。
- Workbench 启动页各资源独立 loading/error/stale；不得单个失败拖垮整页。
- Hooks 必须在 early return 前；复用 Card/Button/Dialog/Drawer/NavItem/ProjectRail。
- 每任务 TDD、focused visual/interaction verification、commit。

---

## File Structure

- Preserve/test: `web/src/pages/Home/{Home.tsx,Home.module.css}` and `/` lazy route — Trending 默认首页。
- Modify: `web/src/App.tsx` and lazy route tests; create `web/src/LanDisclosureGate.tsx`, `web/src/LanDisclosureGate.test.tsx`, `web/src/hooks/useLanDisclosureStartup.ts` and test。
- Modify: `web/src/components/layout/AppShell/{AppShell.tsx,AppShell.module.css}`。
- Modify: `web/src/components/layout/Sidebar/Sidebar.module.css`。
- Modify: `web/src/pages/Workbench/{Workbench.tsx,Workbench.module.css,controllers/useWorkbenchProjectController.ts}` and characterization/controller tests；create pure `workbenchLaunchState.ts` and tests。
- Modify: `web/src/api/workbench.ts`, `src-tauri/src/commands/workbench/projects.rs` and N1 control API/client — 有界 `WorkbenchLaunchSummary`。
- Modify: `web/src/mobile/{MobileWorkbench.tsx,MobileWorkbench.module.css,mobileWorkbenchState.ts}` and shell/tests。
- Modify: `web/src/pages/Settings/Settings.tsx` and `web/src/pages/Settings/Settings.module.css`。
- Modify: `web/src/styles/tokens.css`, `web/scripts/check-css-tokens.mjs`, `web/scripts/check-css-tokens.test.mjs`。
- Modify: `web/src/pages/Welcome/{Welcome.tsx,Welcome.module.css}`。
- Create: `src-tauri/src/gui_bootstrap.rs`, `src-tauri/src/gui_startup.rs`, `src-tauri/src/commands/gui_bootstrap.rs` and inline tests。
- Modify: `src-tauri/src/lib.rs`, `src-tauri/src/commands/mod.rs`, `web/src/api/backend.ts`; Create: `web/src/api/backend.test.ts`。

## Shared Interfaces

```rust
pub const LAN_DISCLOSURE_VERSION: u32 = 1;

pub struct LanDisclosureStatus {
    pub required: bool,
    pub version: u32,
    pub local_addresses: Vec<String>,
    pub preferred_port: u16,
    pub mdns_port: u16,
    pub already_running: bool,
    pub actual_http_port: Option<u16>,
}
```

```ts
export type WorkbenchLaunchResource<T> =
  | { kind: 'loading' }
  | { kind: 'ready'; value: T; stale: boolean }
  | { kind: 'error'; message: string; cached?: T }
```

## Task Dependency Graph

```text
T1 → T2
T2 → (T3 | T4)
(T3, T4) → T5 → T6 → T7
```

`T3/T4` 可在隔离 worktree 并行；后续任务会共同修改 Workbench/layout/tests/styles，按图串行合并。`T7` 消费全部语义与 viewport 结果后执行最终视觉/可访问性门禁。

### Task 1: Gate GUI Sidecar Startup on Versioned LAN Disclosure

**Files:**
- Create: `src-tauri/src/gui_bootstrap.rs`
- Create: `src-tauri/src/gui_startup.rs`
- Create: `src-tauri/src/commands/gui_bootstrap.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/api/backend.ts`
- Create: `web/src/api/backend.test.ts`
- Modify: `web/src/App.tsx`
- Modify: `web/src/App.lazyRoutes.test.tsx`
- Create: `web/src/LanDisclosureGate.tsx`
- Create: `web/src/LanDisclosureGate.test.tsx`
- Create: `web/src/hooks/useLanDisclosureStartup.ts`
- Create: `web/src/hooks/useLanDisclosureStartup.test.tsx`
- Modify: `web/src/i18n/locales/zh/welcome.json`
- Modify: `web/src/i18n/locales/en/welcome.json`
- Modify: `web/tests/support/appBootstrap.ts`
- Modify: `web/tests/attention.spec.ts`
- Modify: `web/tests/core-integrity.spec.ts`
- Modify: `web/tests/frontend-foundation.spec.ts`
- Modify: `AGENTS.md`
- Modify: `web/src/pages/Welcome/Welcome.tsx`
- Modify: `web/src/pages/Welcome/Welcome.module.css`
- Create: `web/src/pages/Welcome/Welcome.test.tsx`
- Test: Rust config/setup tests

**Interfaces:** Produces injectable `GuiStartupCoordinator`, `get_lan_disclosure_status`, `acknowledge_lan_disclosure_and_start_backend`, and frontend state `loading|required|starting|error|pass`.

- [ ] **Step 1: Write failing startup gate tests**

```rust
#[tokio::test]
async fn first_gui_launch_does_not_start_sidecar_before_acknowledgement() {
    let harness = GuiSetupHarness::with_disclosure_version(0).await;
    harness.setup().await.unwrap();
    assert_eq!(harness.ensure_backend_calls(), 0);
}
```

Add acknowledged version starts once, concurrent double-confirm still starts once, disclosure version bump requires a new acknowledgement, existing `cp-onboarding-complete=1` still blocks, Welcome skip cannot bypass, start failure is retryable/fail-closed, and an independently running CLI is reported without being stopped.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked lan_disclosure`

Expected: FAIL because GUI always ensures backend.

- [ ] **Step 3: Add launcher bootstrap state and startup commands**

```rust
pub struct GuiBootstrapState {
    pub lan_disclosure_version: u32,
    pub acknowledged_at: Option<String>,
}
```

Extract an injectable startup coordinator around current setup. GUI first builds/manages state and window; if unacknowledged it skips both `ensure_backend_process_for_gui` and `start_gui_backend_services`. Acknowledge atomically writes only version/timestamp, then under one async mutex/once gate calls ensure followed by `start_gui_backend_services`, waits for health and returns actual access info. Repeated/concurrent calls reuse the same result. The headless CLI never reads this file and remains allowed with fixed risk logging.

- [ ] **Step 4: Build Welcome disclosure UI and tests**

Mount App-level `LanDisclosureGate` above both normal routes and permission onboarding; all hooks live in `useLanDisclosureStartup` before conditional return. Bootstrap read/write/start failure stays on the gate with retry and diagnostic action—never fail-open. Display local address candidates, preferred TCP 62116, mDNS UDP 5353, port-increment note and exact no-identity wording. Confirmation is explicit; permissions remain separate. When a CLI listener already exists, run the GUI browse/discovery services against it, show its actual address and clarify acknowledgement did not start/stop it.

Update shared E2E bootstrap to default to acknowledged status and register both disclosure commands. Update direct Tauri mocks in Attention/core-integrity/frontend-foundation. Add one dedicated unacknowledged first-launch journey; all unrelated E2E starts from acknowledged baseline.

Run: `cd src-tauri && cargo test --locked lan_disclosure && cargo test --locked gui_startup && cd ../web && npm test -- Welcome.test.tsx LanDisclosureGate.test.tsx useLanDisclosureStartup.test.tsx && npm run check:i18n && npm run test:e2e -- frontend-foundation.spec.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/gui_bootstrap.rs src-tauri/src/gui_startup.rs src-tauri/src/commands/gui_bootstrap.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs web/src/api/backend.ts web/src/api/backend.test.ts web/src/App.tsx web/src/App.lazyRoutes.test.tsx web/src/LanDisclosureGate.tsx web/src/LanDisclosureGate.test.tsx web/src/hooks/useLanDisclosureStartup.ts web/src/hooks/useLanDisclosureStartup.test.tsx web/src/pages/Welcome/Welcome.tsx web/src/pages/Welcome/Welcome.module.css web/src/pages/Welcome/Welcome.test.tsx web/src/i18n/locales/zh/welcome.json web/src/i18n/locales/en/welcome.json web/tests/support/appBootstrap.ts web/tests/attention.spec.ts web/tests/core-integrity.spec.ts web/tests/frontend-foundation.spec.ts AGENTS.md
git commit -m "feat(onboarding): require LAN risk acknowledgement"
```

### Task 2: Lock Trending as the Default Home

**Files:**
- Modify: `web/src/pages/Home/Home.test.tsx`
- Modify: `web/src/App.lazyRoutes.test.tsx`
- Modify if needed for landmark semantics only: `web/src/pages/Home/Home.tsx`

**Interfaces:** Preserves `/` → existing Home/Trending and the current Trending data/loading/error contract. Produces no `/discover` route and no dashboard API.

- [ ] **Step 1: Add characterization tests before adjacent navigation work**

Assert cold launch at `/`, sidebar Home activation and browser refresh all render the existing Trending heading/content; `/workbench` remains a separate lazy route. Also assert the production route table contains no `/discover` migration alias.

- [ ] **Step 2: Run the preservation baseline**

Run: `cd web && npm test -- Home.test.tsx App.lazyRoutes.test.tsx`

Expected: PASS on the existing behavior. If it fails, repair the characterization fixture without changing the product route.

- [ ] **Step 3: Keep the page landmark valid without redesign**

Only if the existing Home renders a nested `<main>`, replace that inner landmark with a labelled `section`/`div` so AppShell remains the single main landmark. Do not move or duplicate Trending code and do not create a second navigation destination for it.

- [ ] **Step 4: Verify and commit the guardrail**

Run: `cd web && npm test -- Home.test.tsx App.lazyRoutes.test.tsx && npm run check:i18n && npm run build`

```bash
git add web/src/pages/Home/Home.tsx web/src/pages/Home/Home.test.tsx web/src/App.lazyRoutes.test.tsx
git commit -m "test(home): preserve trending as default route"
```

### Task 3: Implement the Continue Working Workbench Launch Surface

**Files:**
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/Workbench.module.css`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchProjectController.test.tsx`
- Create: `web/src/pages/Workbench/workbenchLaunchState.ts`
- Create: `web/src/pages/Workbench/workbenchLaunchState.test.ts`
- Modify: `web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx`
- Modify: `web/src/api/workbench.ts`
- Modify: `web/src/api/workbench.test.ts`
- Modify: `src-tauri/src/commands/workbench/projects.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/workbench/claude_sessions.rs`
- Modify: `src-tauri/src/orchestrator/repo/tasks.rs`
- Modify: `src-tauri/src/storage/transfer_repo.rs`
- Modify: `src-tauri/src/commands/devices.rs`
- Modify: `src-tauri/tests/runtime_authority_smoke.rs`
- Reuse: Attention provider and device read model
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`

**Interfaces:** Requires N1 Task 2/3's `control_api.rs`/`control_client.rs`. Produces sidecar-owned read-only `WorkbenchLaunchSummary` with five independent section outcomes, each ≤5: recent projects, recent active sessions, Orchestrator tasks, active/failed transfers and online/recent-sync devices. Attention remains provider-owned. No mutation, no per-project N+1 and no eighth Workbench controller.

- [ ] **Step 1: Write state, rendering, deep-link and error-isolation tests**

```ts
test('one failed launch resource preserves the others', () => {
  const state = reduceWorkbenchLaunchResults([
    success('projects', [project]),
    failure('transfers', 'offline'),
  ])
  expect(state.projects.kind).toBe('ready')
  expect(state.transfers.kind).toBe('error')
})
```

Also assert: existing projects + no selection renders “继续工作”; zero projects renders only the three focused actions; active project renders normal Workbench chrome; `/` still renders Trending.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- workbenchLaunchState.test.ts useWorkbenchProjectController.test.tsx WorkbenchProject.characterization.test.tsx Home.test.tsx`

Expected: FAIL because the Workbench launch summary does not exist; the Home preservation assertion already passes.

- [ ] **Step 3: Implement the bounded owner read model**

Add `GET /api/backend/control/workbench-launch-summary` to the N1 `BackendControlApi` router mounted from `net/http_server.rs`. It is a lifecycle-control route: real socket peer must be loopback and the control-file token is required; it is never advertised as a LAN business route. `BackendControlClient` decodes the DTO, existing `commands/workbench/projects.rs` remains the thin Tauri adapter, and `commands/workbench/mod.rs` registers it. The sidecar handler concurrently gathers each section and serializes per-section ready/error outcomes so one repository failure does not fail the response. Recent projects/sessions use bounded indexed queries, Orchestrator is global rather than current-project-only, and every section has deterministic recency ordering and max 5. Add query-count tests proving no per-project session/task N+1 plus a black-box GUI-command→loopback route→sidecar repository smoke; wrong token/non-loopback tests must fail before repository access.

- [ ] **Step 4: Extend the existing project controller and compose the launch surface**

Keep all seven Workbench controllers before every early return. Extend `useWorkbenchProjectController` with launch-summary state rather than creating another controller. Fetch only while there are existing projects but no active project, on mount/invalidation and at most every 15 seconds while document visible; abort on unmount/context change and retain per-section stale timestamps on refresh failure. Compose Card/Button/Pill with existing deep-link helpers and Attention provider. Show real empty CTA, never fabricated metrics, and do not create a new global data store.

- [ ] **Step 5: Run focused tests and visual smoke**

Run: `cd src-tauri && cargo test --locked workbench_launch_summary && cd ../web && npm test -- workbenchLaunchState.test.ts useWorkbenchProjectController.test.tsx WorkbenchProject.characterization.test.tsx Home.test.tsx workbench.test.ts && npm run check:i18n && npm run build`

Expected: PASS; Home remains Trending and the launch summary appears only inside Workbench.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/workbench/projects.rs src-tauri/src/commands/workbench/mod.rs src-tauri/src/commands/devices.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/net/http_server.rs src-tauri/src/workbench/claude_sessions.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/storage/transfer_repo.rs src-tauri/tests/runtime_authority_smoke.rs web/src/api/workbench.ts web/src/api/workbench.test.ts web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/Workbench.module.css web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts web/src/pages/Workbench/controllers/useWorkbenchProjectController.test.tsx web/src/pages/Workbench/workbenchLaunchState.ts web/src/pages/Workbench/workbenchLaunchState.test.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat(workbench): add continue working launch surface"
```

### Task 4: Group Navigation and Fix Short-Window Sidebar

**Files:**
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`
- Modify: `web/src/components/layout/AppShell/AppShell.module.css`
- Modify: `web/src/components/layout/Sidebar/Sidebar.module.css`
- Create: `web/src/components/layout/AppShell/AppShell.test.tsx`
- Modify: `web/tests/frontend-foundation.spec.ts`
- Modify: `web/src/i18n/locales/zh/nav.json`
- Modify: `web/src/i18n/locales/en/nav.json`

**Interfaces:** Reuses NavItem and WorkbenchProjectRail; each non-focusable group label owns an id and its nav group is a `section aria-labelledby`.

- [ ] **Step 1: Add nav order/tab-stop and 720px layout tests**

Assert Work/Knowledge/Connect/System labels, route order, one focus stop per link, and sidebar content/footer rectangles do not overlap at 1280×720.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- AppShell && npm run check:i18n && npm run test:e2e -- frontend-foundation.spec.ts`

Expected: FAIL for grouping/overlap.

- [ ] **Step 3: Implement groups and scroll contract**

Set content `min-height:0; overflow-y:auto`; keep footer in flex flow. Render semantic labelled sections, keep the existing Home/Trending NavItem as the default Explore entry, move ProjectRail into Work group, and preserve all routes/badges. Do not add a duplicate Discover NavItem.

- [ ] **Step 4: Verify keyboard and viewport**

Run: `cd web && npm test -- AppShell && npm run test:e2e -- frontend-foundation.spec.ts`

Expected: PASS at default and 1280×720.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/layout/AppShell/AppShell.tsx web/src/components/layout/AppShell/AppShell.module.css web/src/components/layout/AppShell/AppShell.test.tsx web/src/components/layout/Sidebar/Sidebar.module.css web/tests/frontend-foundation.spec.ts web/src/i18n/locales/zh/nav.json web/src/i18n/locales/en/nav.json
git commit -m "feat(navigation): group routes and fix sidebar overflow"
```

### Task 5: Replace Workbench Chrome with a Focused No-Project State

**Files:**
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/Workbench.module.css`
- Modify: `web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`
- Reuse: local/remote project dialogs and dependency card

**Interfaces:** Existing controllers are still called unconditionally before early return; empty-state view receives callbacks only.

- [ ] **Step 1: Write no-project characterization**

```ts
test('no project shows focused actions without terminal chrome', () => {
  renderWorkbench({ projects: [], activeProject: null })
  expect(screen.getByRole('button', { name: '添加本机项目' })).toBeVisible()
  expect(screen.getByRole('button', { name: '连接远端项目' })).toBeVisible()
  expect(screen.queryByTestId('terminal-pane')).toBeNull()
  expect(screen.queryByTestId('workbench-inspector')).toBeNull()
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkbenchProject.characterization.test.tsx`

Expected: FAIL because disabled chrome remains.

- [ ] **Step 3: Extract a pure empty-state view at page layer**

Keep seven controller hooks before the conditional render. Reuse existing add-local/add-remote and dependency callbacks; do not create a new domain component unless reused elsewhere.

- [ ] **Step 4: Run Workbench characterization**

Run: `cd web && npm test -- WorkbenchProject.characterization.test.tsx WorkbenchTerminal.characterization.test.tsx && npm run check:i18n`

Expected: PASS; project-selected flow unchanged.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/Workbench.module.css web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat(workbench): focus the no-project empty state"
```

### Task 6: Group Mobile Navigation and Meet Responsive Contracts

**Files:**
- Modify: `web/src/mobile/mobileWorkbenchState.ts`
- Modify: `web/src/mobile/mobileWorkbenchState.test.ts`
- Modify: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Modify: `web/src/mobile/MobileWorkbench.module.css`
- Modify: `web/tests/mobile-workbench.spec.ts`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/pages/Settings/Settings.module.css`
- Modify: `web/src/pages/Settings/Settings.test.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchInspector.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.module.css`
- Modify: `web/tests/workbench.spec.ts`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`
- Modify: `web/src/i18n/locales/zh/settings.json`
- Modify: `web/src/i18n/locales/en/settings.json`

**Interfaces:** Existing `MobileWorkbenchPanel` remains the authority; groups map to current panels and do not create a second router.

- [ ] **Step 1: Write grouping and viewport tests**

Assert exact mapping Projects=`projects/worktrees`, Attention=`attention`, Work=`terminal/browser/files/git/prompt`, Automation=`automation`, More=`settings`; selected panel visibility, safe-area padding, `visualViewport` keyboard changes and landscape terminal height. Add Settings ≤680px deep-link coverage for `tablist/tab/tabpanel`, roving tabindex, arrow keys and selected-tab `scrollTo` on the tablist only, plus 1024×768 Workbench inspector visibility coverage.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- mobileWorkbenchState.test.ts MobileWorktreeQuickSwitch.test.ts Settings.test.tsx WorkbenchInspector.test.tsx`

Expected: FAIL because panels are flat.

- [ ] **Step 3: Implement grouped drawer and responsive tokens**

Map every existing panel exactly once. Keep the existing top menu + Drawer, use `visualViewport` and safe-area tokens when the soft keyboard reduces height, and preserve a visible menu action; do not introduce or reference a bottom nav.

- [ ] **Step 4: Verify 390×844 and 844×390 browser states**

Run: `cd web && npm test -- mobileWorkbenchState.test.ts MobileWorktreeQuickSwitch.test.ts Settings.test.tsx WorkbenchInspector.test.tsx && npm run check:i18n && npm run test:e2e -- mobile-workbench.spec.ts workbench.spec.ts`

Expected: PASS；物理 iOS/Android 软键盘仍属于 N8 延期认证 backlog，不是当前 Apple Silicon Mac beta 的完成条件。

- [ ] **Step 5: Commit**

```bash
git add web/src/mobile/mobileWorkbenchState.ts web/src/mobile/mobileWorkbenchState.test.ts web/src/mobile/components/MobileWorkbenchShell.tsx web/src/mobile/MobileWorkbench.module.css web/src/pages/Settings/Settings.tsx web/src/pages/Settings/Settings.module.css web/src/pages/Settings/Settings.test.tsx web/src/pages/Workbench/Workbench.module.css web/src/pages/Workbench/WorkbenchInspector.test.tsx web/tests/mobile-workbench.spec.ts web/tests/workbench.spec.ts web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json
git commit -m "feat(mobile): group workbench navigation"
```

### Task 7: Raise Semantic Text Contrast and Run Visual/Accessibility Gates

**Files:**
- Modify: `web/src/styles/tokens.css`
- Modify: `web/scripts/check-css-tokens.mjs`
- Modify: `web/scripts/check-css-tokens.test.mjs`
- Modify after semantic audit: `web/src/components/domain/CcHistoryCard/CcHistoryCard.module.css`
- Modify after semantic audit: `web/src/components/domain/DeviceCard/DeviceCard.module.css`
- Modify after semantic audit: `web/src/components/domain/GithubRepoCard/GithubRepoCard.module.css`
- Modify after semantic audit: `web/src/components/domain/LanFirewallDependencyCard/LanFirewallDependencyCard.module.css`
- Modify after semantic audit: `web/src/components/domain/PromptCard/PromptCard.module.css`
- Modify after semantic audit: `web/src/components/domain/TransferItem/TransferItem.module.css`
- Modify after semantic audit: `web/src/components/domain/WorkbenchDependencyCard/WorkbenchDependencyCard.module.css`
- Modify after semantic audit: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.module.css`
- Modify after semantic audit: `web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.module.css`
- Modify after semantic audit: `web/src/mobile/components/MobileAttentionPanel.module.css`
- Modify after semantic audit: `web/src/pages/Attention/Attention.module.css`, `web/src/pages/CcHistory/CcHistory.module.css`, `web/src/pages/Home/Home.module.css`, `web/src/pages/Orchestrator/Orchestrator.module.css`, `web/src/pages/Prompts/Prompts.module.css`, `web/src/pages/Scratchpad/Scratchpad.module.css`, `web/src/pages/Settings/Settings.module.css`, `web/src/pages/Transfer/Transfer.module.css`, `web/src/pages/Welcome/Welcome.module.css`, `web/src/pages/Workbench/Workbench.module.css`
- Modify: `docs/prd.md`
- Modify: `README.md`
- Modify: `web/CLAUDE.md`
- Modify: `web/tests/frontend-foundation.spec.ts`
- Modify: `web/tests/mobile-workbench.spec.ts`

- [ ] **Step 1: Add automated contrast-pair test**

Define required foreground/background semantic pairs and assert normal text ratio ≥4.5 in light/dark themes. Disabled/decorative text is explicitly excluded and cannot carry required information.

- [ ] **Step 2: Run RED**

Run: `cd web && npm run check:css-tokens`

Expected: FAIL for current meta/surface pairs.

- [ ] **Step 3: Add and migrate to `--fg-muted-readable`**

Define `--fg-muted-readable` in both themes, migrate user-relevant 11–12px text in the explicit candidate list, and leave `--meta` only for disabled/decorative content that carries no required information. Extend the checker with an explicit reviewed selector allowlist so any new semantic `--meta` use fails rather than relying on visual judgment alone.

- [ ] **Step 4: Run full frontend layout and screenshot-review gates**

At 1024×768, 1280×720, 390×844 and 844×390, assert full-page bounding boxes do not overlap, `scrollWidth <= clientWidth`, all primary actions are keyboard reachable, Settings tablist scroll is local, and Workbench inspector/menu remains discoverable. Save named screenshots as test artifacts and review them with `huashu-design`; do not call this pixel-regression coverage unless committed baselines and an update policy are added.

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

Expected: all exit 0; screenshots at 1024×768, 1280×720, 390×844 and 844×390 show no overlap/crop.

- [ ] **Step 5: Update behavior docs and commit**

```bash
git add web/src/styles/tokens.css web/scripts/check-css-tokens.mjs web/scripts/check-css-tokens.test.mjs web/src/components/domain/CcHistoryCard/CcHistoryCard.module.css web/src/components/domain/DeviceCard/DeviceCard.module.css web/src/components/domain/GithubRepoCard/GithubRepoCard.module.css web/src/components/domain/LanFirewallDependencyCard/LanFirewallDependencyCard.module.css web/src/components/domain/PromptCard/PromptCard.module.css web/src/components/domain/TransferItem/TransferItem.module.css web/src/components/domain/WorkbenchDependencyCard/WorkbenchDependencyCard.module.css web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.module.css web/src/components/domain/WorkbenchSessionSearch/WorkbenchSessionSearch.module.css web/src/mobile/components/MobileAttentionPanel.module.css web/src/pages/Attention/Attention.module.css web/src/pages/CcHistory/CcHistory.module.css web/src/pages/Home/Home.module.css web/src/pages/Orchestrator/Orchestrator.module.css web/src/pages/Prompts/Prompts.module.css web/src/pages/Scratchpad/Scratchpad.module.css web/src/pages/Settings/Settings.module.css web/src/pages/Transfer/Transfer.module.css web/src/pages/Welcome/Welcome.module.css web/src/pages/Workbench/Workbench.module.css web/tests/frontend-foundation.spec.ts web/tests/mobile-workbench.spec.ts docs/prd.md README.md web/CLAUDE.md
git commit -m "feat(ux): complete core workbench onboarding"
```

## Rollback and Failure Containment

- `gui-bootstrap.json` 为 additive launcher 状态；回退 UI 可忽略更高版本，但不得把它解释成可开关 LAN 模式、复制入 AppConfig 或自动撤销已确认记录。
- Home/Trending 路由只增加防回归测试；Workbench 启动页与导航按 task commit 可独立回退，不删除用户状态。
- listener 启动失败保留确认记录并进入诊断路径，不通过自动重复启动绕过失败。

## Completion Contract

- GUI first-run cannot start LAN sidecar before disclosure acknowledgement.
- Home remains the default Trending page; Workbench answers “continue working”.
- sidebar has grouped navigation and no 720px overlap.
- no-project Workbench and mobile group flows are focused and keyboard accessible.
- required small text meets 4.5:1 and full frontend gates pass.

## Plan Self-Review

- Spec coverage: disclosure, Trending default route, Workbench launch/empty states, navigation, sidebar, mobile and contrast each map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: disclosure version and Workbench launch resource types are defined once and reused.
