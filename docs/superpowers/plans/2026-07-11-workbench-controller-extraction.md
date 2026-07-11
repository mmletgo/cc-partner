# Workbench Controller Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在不改变 Workbench 产品行为、UI、路由和 API 的前提下，用可独立测试的领域 controller/hook 将 `Workbench.tsx` 从 4,135 行降到格式化后不超过 1,200 行。

**Architecture:** 先用 jsdom characterization harness 锁定完整页面的可观察行为，再按 Project→Terminal→Worktree/Git→Files→Automation→Overlays 顺序把权威状态与副作用迁入窄接口 controller。页面保留布局与少量显式跨领域协调；xterm buffer/replay 继续由现有外部 Provider 持有，leaf view 只做渲染。

**Tech Stack:** React 19, TypeScript 6, Vitest, jsdom, Testing Library DOM/React/User Event, React Router v6, Tauri invoke/event fakes, xterm 6, CSS Modules.

## Global Constraints

- 前置依赖：先完成 `2026-07-11-vitest-frontend-ci.md`，本计划只使用稳定的 Vitest 入口。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md` 与 `web/CLAUDE.md`；所有 hooks 位于 early return 之前，新增函数/组件写中文 Business Logic / Code Logic 注释。
- 不改变 UI、文案、CSS、路由参数、API 参数、Tauri event 名、terminal buffer、replay gate、tmux attach 或 xterm DOM 常驻语义。
- 不引入 Redux、Zustand、全局巨型 Context 或一个包揽全部领域的 `useWorkbenchController`。
- 每个 controller 接收窄 API/回调并持有单一领域的权威状态；不得复制邻接 controller 的 state。
- 每次只抽一个领域：先 characterization，后 controller unit test，再迁移，再跑全部 Workbench tests/lint/build，再提交。
- 行数目标不能靠复制 JSX、转发全部页面 state 或无意义文件切割达成；页面文件中不得残留 terminal/files/Git/worktree/automation 的具体 API 副作用。
- 这是内部行为保持型重构，不更新 PRD；只更新 `web/CLAUDE.md` 的持久架构约束。

---

## Task Dependency Graph

最大并行 waves：`T1 → T2 → T3 → T4 → T5 → T6 → T7 → T8 → T9`。T2–T8 都修改 `Workbench.tsx`，且后一 controller extraction 依赖前一阶段稳定页面，因此不得并行；Task 内测试与实现仍由同一 implementer 完成。

## File Structure

### Test Infrastructure

- Modify `web/package.json`, `web/package-lock.json`: 增加 `jsdom`, `@testing-library/dom`, `@testing-library/react`, `@testing-library/user-event` dev dependencies。
- Create `web/src/pages/Workbench/testing/workbenchTestHarness.tsx`: router/context/API/event/deferred fakes。
- Create `web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx`。
- Create `web/src/pages/Workbench/WorkbenchTerminal.characterization.test.tsx`。
- Create `web/src/pages/Workbench/WorkbenchWorktreeGit.characterization.test.tsx`。
- Create `web/src/pages/Workbench/WorkbenchFiles.characterization.test.tsx`。
- Create `web/src/pages/Workbench/WorkbenchAutomation.characterization.test.tsx`。
- Create `web/src/pages/Workbench/WorkbenchOverlays.characterization.test.tsx`。

### Controllers and Views

- Create `web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchFileController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.ts` and test。
- Create `web/src/pages/Workbench/controllers/useWorkbenchSessionSearchController.ts` and test。
- Create `web/src/pages/Workbench/controllers/index.ts`: re-export only。
- Create `web/src/pages/Workbench/WorkbenchTerminalPane.tsx`: existing xterm leaf view。
- Create `web/src/pages/Workbench/WorkbenchInspector.tsx`: inspector tabs/layout coordinator。
- Create `web/src/pages/Workbench/WorkbenchFileInspector.tsx`: file tree/path-info leaf view。
- Create `web/src/pages/Workbench/WorkbenchGitInspector.tsx`: Git graph/actions leaf view。
- Modify `web/src/pages/Workbench/Workbench.tsx`: controller composition、layout 和 cross-domain bridges only。
- Modify `web/CLAUDE.md`: controller ownership and verification commands。

## Controller Contracts

Use named result interfaces. The exact DTO types come from existing `web/src/lib/types.ts`; no controller may return `Record<string, unknown>` or `any`.

```ts
export interface WorkbenchProjectControllerResult {
  remoteProjectOffline: boolean;
  remoteWriteDisabled: boolean;
  isCurrentProject: (projectId: string) => boolean;
  markRequestFailure: (projectId: string, error: unknown) => void;
  markRequestSuccess: (projectId: string) => void;
  selectProjectFromDeepLink: (projectId: string) => Promise<boolean>;
}

export interface WorkbenchTerminalBridge {
  loadSessions: () => Promise<void>;
  focusSession: (sessionId: string) => Promise<boolean>;
  createSessionForWorktree: (worktreeId: string) => Promise<void>;
  clearBuffersForWorktree: (worktreeId: string) => void;
}

export interface WorkbenchFileBridge {
  resetForContext: (projectId: string | null, worktreeId: string | null) => void;
  guardDirtyContextChange: () => Promise<boolean>;
}

export interface WorkbenchAutomationControllerResult {
  automationOpen: boolean;
  openAutomation: () => void;
  closeAutomation: () => void;
  applyAutomationDeepLink: (target: WorkbenchDeepLink) => Promise<boolean>;
  openTaskWorkbench: (url: string) => Promise<void>;
}
```

Each controller takes only the relevant `Pick<typeof workbenchApi, ...>` or explicit method interface, plus narrow bridges. Tests inject fakes; production passes existing API functions.

---

### Task 1: Build a Characterization Harness and Baseline

**Files:**
- Modify: `web/package.json`
- Modify: `web/package-lock.json`
- Create: `web/src/pages/Workbench/testing/workbenchTestHarness.tsx`
- Create: six characterization test files listed above

- [ ] **Step 1: Install DOM test dependencies**

Add locked dev dependencies for jsdom and Testing Library. Every new characterization/controller DOM test begins with:

```ts
// @vitest-environment jsdom
```

- [ ] **Step 2: Create deterministic deferred/API/event fakes**

The harness must provide MemoryRouter, project context, `WorkbenchTerminalBuffersProvider`, fake Tauri invoke/listen, controllable deferred promises, and stubs for xterm/browser/editor/orchestrator heavy children. It exposes call logs and helpers to resolve requests out of order.

- [ ] **Step 3: Write project and terminal characterization tests against current `Workbench`**

Lock: project/worktree selection, stale project response ignored, remote read failure disables writes, later success restores; focus/resize/pane calls; terminal layer stays mounted when switching workspace views; output buffer/replay continues after route/view changes.

- [ ] **Step 4: Write worktree/Git and file characterization tests**

Lock: create worktree then create session; remove/merge refresh worktrees/sessions/Git and clear correct buffers; merge events only affect current project; dirty tab cancel/discard/save; baseHash conflict; directory/open/save/format stale responses ignored.

- [ ] **Step 5: Write Automation and overlay characterization tests**

Lock: automation toggle; staged project→worktree→session deep link; execution-site switch; Prompt shortcut/IME/position/close-reset/remote disabled; session search ⌘K lifecycle and resume→refresh/focus.

- [ ] **Step 6: Prove the baseline before extraction**

```bash
cd web
npm test -- WorkbenchProject.characterization WorkbenchTerminal.characterization WorkbenchWorktreeGit.characterization WorkbenchFiles.characterization WorkbenchAutomation.characterization WorkbenchOverlays.characterization
npm test -- workbenchTerminalBuffer terminalReplay workbenchFiles workbenchWorktrees workbenchDeepLink
```

Expected: all pass against the unextracted page. Fix only harness assumptions; do not start extraction until baseline is green.

- [ ] **Step 7: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/package.json web/package-lock.json web/src/pages/Workbench/testing web/src/pages/Workbench/*.characterization.test.tsx
git commit -m "test: characterize workbench behavior"
```

---

### Task 2: Extract Project Selection and Remote Offline State

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchProjectController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Write controller tests first**

Cover current-project request success/failure, old-project failure ignored, remote offline disabling writes, later successful read clearing offline, local project never marked remote-offline, and project deep-link not found.

- [ ] **Step 2: Run the failing controller test**

```bash
cd web
npm test -- useWorkbenchProjectController
```

Expected: module-not-found/compile failure.

- [ ] **Step 3: Implement the project controller**

Move only remote-offline state, cross-project request sequence/current-project guard and project-stage deep-link selection. Leave worktree/session/application in their current owners. Use refs for latest project ID where async closures require it.

- [ ] **Step 4: Replace page-owned project logic**

Wire the controller into existing load functions through `markRequestSuccess/Failure` and `isCurrentProject`; delete the replaced state/effects from Workbench. Preserve current reset order exactly.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- useWorkbenchProjectController WorkbenchProject.characterization workbenchRemoteProjects workbenchDeepLink
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench project controller"
```

---

### Task 3: Extract Terminal Controller and Leaf View

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchTerminalController.test.tsx`
- Create: `web/src/pages/Workbench/WorkbenchTerminalPane.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Write controller tests for session and pane behavior**

Cover load/focus/create/rename/close, focus polling, resize, split/switch/zoom/close pane, terminal status event filtering, stale context response, and remote write disable. Assert the controller calls external buffer callbacks rather than storing terminal output in React state.

- [ ] **Step 2: Write leaf-view contract tests**

Assert the extracted pane creates/disposes xterm once per session identity, consumes the existing replay gate, forwards input/resize/focus, and does not unmount merely because workspace view changes.

- [ ] **Step 3: Implement terminal controller**

Move session state and terminal API/event effects from the page. Expose `WorkbenchTerminalBridge` plus explicit rendering data/actions. Preserve the existing `WorkbenchTerminalBuffersProvider`; controller state may reference buffer metadata but never own terminal byte content.

- [ ] **Step 4: Move the existing `TerminalPane` body verbatim into the leaf view**

Retain effect dependency arrays, xterm options, replay sequencing, DOM refs and cleanup. Do not revise terminal behavior or CSS in this task.

- [ ] **Step 5: Verify terminal invariants and commit**

```bash
cd web
npm test -- useWorkbenchTerminalController WorkbenchTerminal.characterization workbenchTerminalBuffer terminalReplay terminalOptions terminalSizing terminalSessionOrder
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench terminal controller"
```

---

### Task 4: Extract Worktree and Git Controller

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchWorktreeGitController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Write state/action tests**

Cover load/select/create/remove/commit/push/merge, create form/busy/error, Git commit refresh, merge progress filtering and stale context guard. Verify create invokes `terminalBridge.createSessionForWorktree`; remove/merge use explicit buffer/session bridges and never mutate terminal state directly.

- [ ] **Step 2: Implement with explicit cross-domain callbacks**

Controller inputs include project guard, `WorkbenchTerminalBridge`, and file dirty-context guard. Outputs include worktree/Git state and `onWorktreeCreated/Removed/Merged` effects expressed through the bridges. Preserve current operation order and error messages.

- [ ] **Step 3: Remove duplicate page state/effects**

Delete migrated worktree/Git request sequences, event listener and handlers from `Workbench.tsx`. The page may still coordinate a user-confirmed context change before invoking controller action.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- useWorkbenchWorktreeGitController WorkbenchWorktreeGit.characterization workbenchWorktrees
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench worktree git controller"
```

---

### Task 5: Extract File Workspace Controller

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchFileController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchFileController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Write controller tests for every file invariant**

Cover directory/tree load and children cache; selected path/info; open tab dedupe; dirty tab activate/close cancel/discard/save; baseHash conflict; save/format; image/CSV/SQLite/HTML/Markdown mode state; create file/dir, rename, delete, copy path; project/worktree stale response and `resetForContext`.

- [ ] **Step 2: Run the failing controller suite**

```bash
cd web
npm test -- useWorkbenchFileController
```

- [ ] **Step 3: Implement the file controller**

Own all file/tree/tab request sequences and side effects. Result props should map directly to `WorkbenchFileWorkspace` and inspector props. Keep existing baseHash/dirty semantics and remote write disable checks; do not move layout JSX into the hook.

- [ ] **Step 4: Replace page file logic and verify stale guards**

Remove migrated file state/handlers/effects from Workbench. Resolve deferred requests in reverse order in characterization tests and confirm neither tree nor active tab is overwritten.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- useWorkbenchFileController WorkbenchFiles.characterization workbenchFiles workbenchBrowserPreview workbenchWorkspaceSwitch
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench file controller"
```

---

### Task 6: Extract Automation Controller

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Write staged deep-link tests**

Test normal open/toggle; project-only automation link; task execution context requiring project→worktree→session; nonexistent project/worktree/session; old deep-link request canceled by a newer one; automation view does not unmount the terminal layer.

- [ ] **Step 2: Implement against stable controller bridges**

Accept project selection, worktree selection and terminal load/focus functions as explicit inputs. The controller owns automation open/target state and execution-context takeover, but it does not own task fetching or terminal/session state.

- [ ] **Step 3: Keep cross-domain choreography visible**

The page composes controllers and passes bridges; do not bury all controllers in a new context. Preserve staged ordering and current navigation/search cleanup.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- useWorkbenchAutomationController WorkbenchAutomation.characterization workbenchAutomationView workbenchDeepLink
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench automation controller"
```

---

### Task 7: Extract Prompt Optimizer and Session Search Overlays

**Files:**
- Create: `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchPromptOptimizerController.test.tsx`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchSessionSearchController.ts`
- Create: `web/src/pages/Workbench/controllers/useWorkbenchSessionSearchController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Characterize and test Prompt overlay ownership**

Test config load, open/input/position, Control shortcut, IME safety, stream-to-session, focus return, close clearing state, remote disabled and no active session. Implement hook and migrate only those effects/handlers.

- [ ] **Step 2: Verify Prompt extraction**

```bash
cd web
npm test -- useWorkbenchPromptOptimizerController WorkbenchOverlays.characterization promptOptimizerWidget
```

- [ ] **Step 3: Characterize and test session search ownership**

Test ⌘K/Ctrl+K open/close, input focus exceptions, unsupported context, resume success invoking terminal reload/focus, and unmount cleanup. Search result data remains owned by the existing `WorkbenchSessionSearch` component.

- [ ] **Step 4: Implement and migrate the session-search hook**

Remove corresponding shortcut/open state from Workbench; pass narrow callbacks to the existing component.

- [ ] **Step 5: Run all overlay/page tests and commit**

```bash
cd web
npm test -- useWorkbenchPromptOptimizerController useWorkbenchSessionSearchController WorkbenchOverlays.characterization promptOptimizerWidget
npm test -- src/pages/Workbench
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: extract workbench overlay controllers"
```

---

### Task 8: Extract Inspector Rendering and Reduce the Page

**Files:**
- Create: `web/src/pages/Workbench/WorkbenchInspector.tsx`
- Create: `web/src/pages/Workbench/WorkbenchFileInspector.tsx`
- Create: `web/src/pages/Workbench/WorkbenchGitInspector.tsx`
- Create: `web/src/pages/Workbench/controllers/index.ts`
- Modify: `web/src/pages/Workbench/Workbench.tsx`

- [ ] **Step 1: Add source ownership tests**

Read `Workbench.tsx` and assert it no longer calls specific `workbenchApi.sessions/files/worktrees/git` methods or directly subscribes to terminal/merge events. Add a line-count assertion `<= 1200` and reject a page-level state object named `workbenchController`.

- [ ] **Step 2: Move inspector leaf JSX without moving state back**

`WorkbenchInspector` only coordinates the existing tabs. Move file tree/path info into `WorkbenchFileInspector` and Git graph/actions into `WorkbenchGitInspector`; each receives its own controller-derived props and never imports the other domain.

- [ ] **Step 3: Keep `Workbench.tsx` focused**

The page should contain: route/context reads, controller construction, explicit bridges, top-level derived layout state, and JSX composition. Remove dead imports/helpers/effects. Move the existing `FileTreeNode/FileTree` only with the inspector that renders them.

- [ ] **Step 4: Format and enforce the line target**

```bash
cd web
npm run lint
wc -l src/pages/Workbench/Workbench.tsx
```

Expected: ESLint exits `0` and style-conforming `Workbench.tsx` reports at most `1200` lines. Do not add a formatter dependency solely to manipulate the line count.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- src/pages/Workbench src/hooks/workbenchTerminalBuffer.test.ts
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench
git commit -m "refactor: reduce workbench page to controller composition"
```

---

### Task 9: Final Regression and Architecture Documentation

**Files:**
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: Run the complete Workbench regression set**

```bash
cd web
npm test -- src/pages/Workbench
npm test -- src/hooks/workbenchTerminalBuffer.test.ts src/hooks/workbenchHttpEvents.test.ts
npm test
npm run lint
npm run build
```

Expected: all commands exit `0`; no hook-order warning, unhandled Promise, leaked timer/listener, or xterm dispose error appears.

- [ ] **Step 2: Manually exercise the high-risk paths**

In local and remote projects: switch project/worktree during slow loads; keep terminal output flowing while opening browser/files/automation; split/focus/resize/close panes; dirty-file context change; create/merge/remove worktree; open automation execution site; open/close both overlays. Record failures as tests before fixes.

- [ ] **Step 3: Audit controller ownership**

```bash
rg -n "workbenchApi\.(sessions|files|worktrees|git)|listen\('workbench:(terminal-status|merge-progress)'" web/src/pages/Workbench/Workbench.tsx
wc -l web/src/pages/Workbench/Workbench.tsx
```

Expected: no domain API/event matches in the page and line count ≤1200.

- [ ] **Step 4: Update `web/CLAUDE.md`**

Document each controller's owned state/effects, bridge-only cross-domain policy, terminal buffer ownership, characterization command, hooks-before-return rule and line-count contract. Do not add a task timeline or change PRD.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/CLAUDE.md
git commit -m "docs: record workbench controller boundaries"
```

## Completion Contract

- `Workbench.tsx` is formatted and ≤1,200 total lines.
- Project, terminal, worktree/Git, files, automation and overlays each have a narrow independently tested controller.
- The page contains no concrete API/event implementation for those domains and no giant replacement Context/controller.
- xterm DOM persistence, buffer/replay, tmux focus/attach, dirty/baseHash guards, stale request guards and deep links retain current observable behavior.
- Full Workbench characterization, legacy helper tests, all frontend tests, lint and build pass.
- Only `web/CLAUDE.md` changes persistent documentation; product behavior/PRD remain unchanged.
