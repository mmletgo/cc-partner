# Frontend Foundation, UX, and Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立可自动验证的 design token、模态、键盘、错误隔离、拆包、终端缓存和 i18n 基础，并按职责渐进拆分前端巨型文件。

**Architecture:** 先用静态 contract 和 characterization tests 锁定现状，再引入无业务语义 primitives/纯 helper，最后迁移页面和拆包。路由、编辑器和移动面板采用 lazy boundary；terminal store 保持外部订阅 API、内部改为 chunk deque + animation-frame batching；大文件按 controller/view/dialog 拆分且不重复 Workbench controller 计划。

**Tech Stack:** React 19, TypeScript 6 compiler API, React Router v6, ReactDOM portals, Vite 8/Rollup, CSS Modules, Vitest + jsdom + Testing Library, Playwright, i18next, xterm/CodeMirror/Tiptap/Recharts dynamic imports.

## Global Constraints

- 前置：核心完整性计划中的 Settings partial loader 先落地，再执行本计划 Settings 拆分；其它 tasks 可在独立 worktree 先行。
- 开始前读取根 `AGENTS.md`、`web/CLAUDE.md`；执行阶段先用 `superpowers:using-git-worktrees`；共享文件 tasks 不并行落同一 worktree。
- 不引入 Redux、Zustand、CSS framework、第三方 modal/focus-trap 或新的前端路由库。
- 所有新增组件/函数有中文 Business Logic / Code Logic docstring；hooks 在 early return 前；用户文案进入 zh/en i18n。
- Dialog/Drawer 使用 portal、focus trap、Escape、inert、scroll lock、focus restore；不得只添加 ARIA 而不实现键盘行为。
- canonical token 只使用 `tokens.css` 已有名称；不为错误名称新增 alias。
- production sourcemap 默认 false；仅 `CC_PARTNER_SOURCEMAP=1` 生成 hidden map，release bundle 不包含 map。
- desktop main initial ≤ 320 KiB gzip；mobile initial ≤ 280 KiB gzip；mobile initial 禁止 xterm/Tiptap/CodeMirror/Recharts。
- terminal buffer 上限保持 200,000 UTF-16 code units；replay/diff、session 隔离和 Provider 生命周期不变。
- 不重复七个 Workbench controllers，不改变 Orchestrator 状态机、Attention source 或 Inbox navigation-only 语义。
- 行为保持型拆分每次先 characterization、后移动一个边界；不得靠复制代码或 barrel 循环依赖达成行数目标。
- 每个 task 独立提交；提交前跑 focused tests、lint/build，并检查 `git status --short` 只含当前 task 文件。

---

## Task Dependency Graph

```text
T1 ─> T2 ─> T3 ─> T4
T2 ─> T5 ─> T6
T5 ─> T8
T6 ─> T10
T7 ─> T10
T8 ─> T10
T9 ─> T10
```

可并行 waves：`(T1 | T7 | T9) → (T2 | T5) → (T3 | T6 | T8) → T4 → T10`。T3/T4 都改 Workbench/Mobile shell，不并行；T5/T6 都改 App/Vite，顺序执行。

## File Structure

### Contracts and primitives

- Create `web/scripts/check-css-tokens.mjs`, `check-css-tokens.test.mjs`。
- Create `web/scripts/check-i18n-jsx.mjs`, `check-i18n-jsx.test.mjs`。
- Create `web/scripts/check-bundle-contract.mjs`, `check-bundle-contract.test.mjs`。
- Create `web/src/components/primitives/Dialog/{Dialog.tsx,Dialog.module.css,index.ts,useModalLayer.ts,useModalLayer.test.tsx}`。
- Create `web/src/components/primitives/Drawer/{Drawer.tsx,Drawer.module.css,index.ts}`。
- Modify primitive barrels and `AGENTS.md` component list。

### Interaction and routing

- Create `web/src/lib/rovingTablist.ts`, `.test.ts`。
- Create `web/src/components/layout/RouteErrorBoundary/{RouteErrorBoundary.tsx,RouteErrorBoundary.module.css,index.ts,RouteErrorBoundary.test.tsx}`。
- Modify all current dialog owners in `App.tsx`, Prompts, Scratchpad, CcHistory, Orchestrator, Workbench domain/mobile components。
- Modify Attention desktop/mobile, Workbench terminal/inspector tabs, Transfer keyboard contract, mobile shell/drawer。
- Modify `web/src/styles/globals.css` for reduced motion。

### Performance and structure

- Modify `web/src/App.tsx`, `web/src/mobile/MobileWorkbench.tsx`, `web/src/components/domain/WorkbenchFileWorkspace/WorkbenchFileWorkspace.tsx`, Health imports, `web/vite.config.ts`, `web/package.json`, `.github/workflows/ci.yml`。
- Modify `web/src/hooks/workbenchTerminalBuffer.ts`, tests and both desktop/mobile callers。
- Create Settings/Orchestrator/MobileAutomation controller/view files listed in Task 10。
- Create `web/src/lib/types/{core,settings,workbench,orchestrator,attention,index}.ts`; convert `web/src/lib/types.ts` to compatibility barrel。

## Shared Interfaces

```ts
export interface ModalLayerOptions {
  open: boolean;
  surfaceRef: RefObject<HTMLElement | null>;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnEscape: boolean;
  onClose: () => void;
}
export function useModalLayer(options: ModalLayerOptions): void;

export interface DialogProps {
  open: boolean;
  titleId: string;
  children: ReactNode;
  initialFocusRef?: RefObject<HTMLElement | null>;
  closeOnEscape?: boolean;
  closeOnBackdrop?: boolean;
  onClose: () => void;
  className?: string;
}
export interface DrawerProps extends DialogProps {
  side?: 'left' | 'right';
}

export type RovingTabKey = 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End';
export function getRovingTabIndex(
  currentIndex: number,
  key: RovingTabKey,
  count: number,
): number;

export interface TerminalFrameScheduler {
  schedule(callback: () => void): () => void;
}
export interface TerminalBufferStoreOptions {
  initialBuffers?: Record<string, string>;
  maxChars?: number;
  frameScheduler?: TerminalFrameScheduler;
}
```

### Task 1: Enforce Canonical CSS Tokens and Reduced Motion

**Files:**
- Create: `web/scripts/check-css-tokens.mjs`
- Create: `web/scripts/check-css-tokens.test.mjs`
- Modify: CSS files currently using undefined semantic tokens
- Modify: `web/src/styles/globals.css`
- Modify: `web/package.json`
- Modify: `.github/workflows/ci.yml`

**Interfaces:** Produces CLI `npm run check:css-tokens`; exit 0 with `CSS token contract passed`, exit 1 with `file:line --token-name` diagnostics.

- [ ] **Step 1: Write CLI fixture tests before implementation**

Create temporary fixture strings in the test process for defined token, nested fallback, unknown semantic token, TSX-injected allowlist variable, and missing dark theme value. Invoke exported `analyzeCssTokenContract(files, tokensSource)` and assert exact diagnostics.

- [ ] **Step 2: Verify RED**

Run: `cd web && node --test scripts/check-css-tokens.test.mjs`
Expected: FAIL because checker does not exist.

- [ ] **Step 3: Implement parser and fix current violations**

Use comment-stripped regex to collect definitions/usages with line numbers. Allow only `--prompt-panel-left`, `--prompt-panel-top`, `--git-graph-color` as runtime structural variables. Replace undefined mappings exactly per spec; choose `--muted` for readable body copy and `--meta` for tertiary metadata.

- [ ] **Step 4: Add reduced-motion contract**

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    scroll-behavior: auto;
    animation-duration: 0.01ms;
    animation-iteration-count: 1;
    transition-duration: 0.01ms;
  }
}
```

Add a static test asserting no shimmer keyframe remains active under the media query.

- [ ] **Step 5: Wire gates and commit**

```bash
cd web
node --test scripts/check-css-tokens.test.mjs
npm run check:css-tokens
npm run lint
npm run build
git add scripts src package.json ../.github/workflows/ci.yml
git commit -m "fix: enforce canonical frontend tokens"
```

Expected: all exit 0; CI quality runs token check before build.

### Task 2: Build Dialog and Drawer Primitives

**Files:**
- Create: `web/src/components/primitives/Dialog/Dialog.tsx`
- Create: `web/src/components/primitives/Dialog/Dialog.module.css`
- Create: `web/src/components/primitives/Dialog/useModalLayer.ts`
- Create: `web/src/components/primitives/Dialog/useModalLayer.test.tsx`
- Create: `web/src/components/primitives/Dialog/Dialog.test.tsx`
- Create: `web/src/components/primitives/Dialog/index.ts`
- Create: `web/src/components/primitives/Drawer/Drawer.tsx`
- Create: `web/src/components/primitives/Drawer/Drawer.module.css`
- Create: `web/src/components/primitives/Drawer/Drawer.test.tsx`
- Create: `web/src/components/primitives/Drawer/index.ts`
- Modify: `web/src/components/primitives/index.ts`

**Interfaces:** Produces `Dialog`, `Drawer`, `useModalLayer` exact shared interfaces; no business imports.

- [ ] **Step 1: Write failing modal behavior tests**

Use jsdom/user-event to assert portal parent, role/label, initial focus, Tab/Shift+Tab loop, Escape, backdrop policy, background inert/aria-hidden, body scroll lock, nested layer reference counting, trigger focus restore, and unmount cleanup.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- components/primitives/Dialog components/primitives/Drawer`
Expected: missing modules.

- [ ] **Step 3: Implement shared layer stack**

Maintain module-level `openLayers` and per-element previous attribute maps. Only top layer handles Escape/Tab. Query focusables with disabled/hidden filtering; surface gets `tabIndex={-1}` fallback. `Dialog` and `Drawer` only compose portal/backdrop/surface around the hook.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- components/primitives/Dialog components/primitives/Drawer
npm run lint
npm run build
git add src/components/primitives
git commit -m "feat: add accessible dialog and drawer primitives"
```

### Task 3: Migrate Existing Modals and Mobile Navigation

**Files:**
- Modify: `web/src/App.tsx`, `web/src/App.module.css`
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`, `web/src/components/layout/AppShell/AppShell.module.css`
- Modify: `web/src/pages/{Prompts,Scratchpad,CcHistory,Orchestrator}/*.{tsx,module.css}`
- Modify: `web/src/pages/Welcome/Welcome.tsx`（独立 onboarding 页面改用 main/region 语义，不伪装 modal）
- Modify: `web/src/components/domain/{WorkbenchSessionSearch,WorkbenchProjectRail}/*.{tsx,module.css}`
- Modify: `web/src/mobile/components/{MobileWorkbenchShell,MobileWorktreeQuickSwitch,MobileAutomationPanel}.tsx`
- Modify: `web/src/mobile/MobileWorkbench.module.css`
- Create: `web/src/components/primitives/Dialog/dialogMigrations.test.tsx`
- Modify: relevant existing characterization tests

**Interfaces:** Consumes T2 only; business close/confirm callbacks remain unchanged.

- [ ] **Step 1: Add a migration inventory test**

Scan production TSX and fail on raw `role="dialog"`/`aria-modal="true"` outside Dialog/Drawer implementation. Welcome 是独立 route，不在 allowlist：把外层改为 `main`/labelled region；AppShell 的 Mobile Access 弹层迁移为 Dialog。添加 backend close、Prompt delete、Orchestrator create/detail、session search 和 mobile drawer focus restore 交互测试。

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- dialogMigrations WorkbenchOverlays MobileWorktreeQuickSwitch MobileAutomationPanel`
Expected: raw modal inventory and missing focus behavior fail.

- [ ] **Step 3: Migrate desktop dialogs by ownership group**

Replace masks/surfaces with `<Dialog open titleId onClose>`. Preserve Card/forms and busy-state close prevention by passing an `onClose` that returns early. Delete duplicated Escape/autofocus effects after each migration.

- [ ] **Step 4: Migrate mobile overlays and nav**

Use `<Drawer side="left">` only when narrow drawer is open; initial focus close button; menu button is restored trigger. Keep desktop-width rail outside Drawer. Quick switch/create/detail use Dialog.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- dialogMigrations WorkbenchOverlays MobileWorktreeQuickSwitch MobileAutomationPanel
npm run test:e2e -- attention.spec.ts
npm run lint
npm run build
git add src
git commit -m "refactor: migrate product modals to shared primitives"
```

### Task 4: Normalize Attention and Workbench Keyboard Semantics

**Files:**
- Create: `web/src/lib/rovingTablist.ts`
- Create: `web/src/lib/rovingTablist.test.ts`
- Modify: `web/src/pages/Attention/Attention.tsx`
- Modify: `web/src/mobile/components/MobileAttentionPanel.tsx`
- Modify: `web/src/pages/Attention/attentionView.test.tsx`
- Modify: `web/src/mobile/MobileAttentionPanel.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchInspector.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchTerminal.characterization.test.tsx`
- Modify: `web/src/components/domain/WorkbenchFileWorkspace/WorkbenchFileWorkspace.tsx`
- Modify: `web/src/pages/Transfer/Transfer.test.tsx`

**Interfaces:** Produces `getRovingTabIndex` exact interface; file, session and inspector tablists share key semantics.

- [ ] **Step 1: Write failing semantic tests**

Assert one interactive element per Attention row, no nested button; one terminal tab stop; Arrow/Home/End activation/focus; close selected session focuses adjacent/new button; inspector `aria-controls`/tabpanel; file tab existing semantics remain; dropzone Enter/Space calls picker.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- rovingTablist attentionView MobileAttentionPanel WorkbenchTerminal WorkbenchInspector Transfer`
Expected: Attention nested control and terminal/inspector tab behavior fail.

- [ ] **Step 3: Implement pure index helper and DOM changes**

Render Attention row as one button containing an action `<span>`. Split session tab into tab button + sibling close button (matching file tabs), assign active-only tabIndex and stable ids. Inspector panels use ids and roving keys. Do not change focusSession/closeSession ordering.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- rovingTablist attentionView MobileAttentionPanel WorkbenchTerminal WorkbenchFileWorkspace
npm run lint
npm run build
git add src/lib/rovingTablist* src/pages/Attention src/mobile/components/MobileAttentionPanel* src/pages/Workbench src/components/domain/WorkbenchFileWorkspace
git commit -m "fix: normalize navigation keyboard semantics"
```

### Task 5: Add Route Error Boundaries and Lazy Routes

**Files:**
- Create: `web/src/components/layout/RouteErrorBoundary/RouteErrorBoundary.tsx`
- Create: `web/src/components/layout/RouteErrorBoundary/RouteErrorBoundary.module.css`
- Create: `web/src/components/layout/RouteErrorBoundary/RouteErrorBoundary.test.tsx`
- Create: `web/src/components/layout/RouteErrorBoundary/index.ts`
- Modify: `web/src/components/layout/index.ts`
- Modify: `web/src/App.tsx`
- Create: `web/src/App.lazyRoutes.test.tsx`
- Modify: `web/src/i18n/locales/{zh,en}/common.json`

**Interfaces:** `RouteErrorBoundary` props are `{resetKey:string; onRetry?:()=>void; children:ReactNode}`; route helper lazy imports named exports into `{default: module.Name}`.

- [ ] **Step 1: Write failing boundary tests**

Throw during route render; assert AppShell remains, localized fallback appears, retry remounts child, pathname reset clears error, stack absent in production, overlay failure isolated.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- RouteErrorBoundary App.lazyRoutes`
Expected: missing boundary and synchronous imports.

- [ ] **Step 3: Implement boundary and convert routes**

Use a class error boundary internally because React 19 still requires it for render errors; functional wrapper reads navigation callbacks. Replace page imports with `lazy(() => import(...).then(...))`, wrap elements in `Suspense` and route boundary. Keep providers/AppShell eager; DesignSystem import only inside `isDev` branch helper.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- RouteErrorBoundary App.lazyRoutes
npm run lint
npm run build
git add src/App.tsx src/App.lazyRoutes.test.tsx src/components/layout/RouteErrorBoundary src/i18n/locales
git commit -m "feat: isolate and lazy-load application routes"
```

### Task 6: Split Heavy Editors/Mobile Panels and Enforce Bundle Budgets

**Files:**
- Modify: `web/src/components/domain/WorkbenchFileWorkspace/WorkbenchFileWorkspace.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/pages/Health/Health.tsx` only if Recharts leaks through an eager helper
- Modify: `web/vite.config.ts`
- Create: `web/scripts/check-bundle-contract.mjs`
- Create: `web/scripts/check-bundle-contract.test.mjs`
- Modify: `web/package.json`
- Modify: `.github/workflows/ci.yml`

**Interfaces:** Vite writes `web/dist/.vite/cc-bundle-contract.json` with `{entries,chunks}`; checker CLI reads it and applies exact 320/280 KiB budgets and forbidden module list.

- [ ] **Step 1: Write checker fixture tests and current-graph characterization**

Test static closure traversal, dynamic edge exclusion, gzip byte summation, over-budget diagnostic, and forbidden module diagnostic. Add static tests proving mobile panel/editor imports are dynamic.

- [ ] **Step 2: Verify RED**

Run: `cd web && node --test scripts/check-bundle-contract.test.mjs`
Expected: missing checker.

- [ ] **Step 3: Add Rollup contract plugin**

In `generateBundle`, emit chunk file names, entry facade, imports/dynamicImports, module ids and raw bytes. Set `sourcemap: process.env.CC_PARTNER_SOURCEMAP === '1' ? 'hidden' : false`. Checker reads emitted JS, calculates gzip with `node:zlib`, and rejects mobile initial forbidden ids.

- [ ] **Step 4: Create lazy dependency boundaries**

Lazy-load Code/Markdown/HTML editors from `WorkbenchFileWorkspace`, each heavy mobile panel from `MobileWorkbench`, and retain lightweight loading states using existing tokens. Health remains route-lazy and no eager module may import `StatsChart`.

- [ ] **Step 5: Wire budgets and commit**

```bash
cd web
node --test scripts/check-bundle-contract.test.mjs
npm run build
npm run check:bundle
find dist -name '*.map' -print
```

Expected: checker PASS; final command prints nothing without env flag. Then run `CC_PARTNER_SOURCEMAP=1 npm run build` and expect hidden `.map` files plus no `sourceMappingURL` in JS.

```bash
git add vite.config.ts package.json scripts src/components/domain/WorkbenchFileWorkspace src/mobile/MobileWorkbench.tsx ../.github/workflows/ci.yml
git commit -m "perf: split heavy frontend dependency graphs"
```

### Task 7: Replace Terminal Strings with a Batched Ring Buffer

**Files:**
- Modify: `web/src/hooks/workbenchTerminalBuffer.ts`
- Modify: `web/src/hooks/workbenchTerminalBuffer.test.ts`
- Modify: `web/src/hooks/useWorkbenchTerminalBuffers.tsx`
- Modify: `web/src/mobile/MobileApp.tsx`
- Modify: `web/src/pages/Workbench/terminalReplay.test.ts`
- Modify: `web/src/mobile/mobileTerminalReplay.test.ts`

**Interfaces:** `createWorkbenchTerminalBufferStore(options?: TerminalBufferStoreOptions)` returns the existing `WorkbenchTerminalBufferStore`; callers migrate from positional args.

- [ ] **Step 1: Extend tests before implementation**

Inject a scheduler collecting callbacks. Append 10,000 chunks and assert zero notifications before frame, one after frame, exact content/order, 200,000 trimming, materialized snapshot caching, session isolation, reset/remove cancel stale frame, replay diff unchanged.

- [ ] **Step 2: Verify RED**

Run: `cd web && npm test -- workbenchTerminalBuffer terminalReplay mobileTerminalReplay`
Expected: current store notifies per append and concatenates full string.

- [ ] **Step 3: Implement chunk deque**

Per session store `{chunks,startOffset,length,materialized,revision,scheduledCancel,generation}`. Append pushes without joining; trim from head, slicing only the boundary chunk. `getBuffer` joins once when cache null. Frame callback validates generation then increments revision and notifies once.

- [ ] **Step 4: Migrate callers and verify**

Browser scheduler uses `requestAnimationFrame`/`cancelAnimationFrame`; tests inject deterministic scheduler. Provider and MobileApp call `createWorkbenchTerminalBufferStore()` with no behavior change.

```bash
cd web
npm test -- workbenchTerminalBuffer terminalReplay mobileTerminalReplay WorkbenchTerminalPane
npm run lint
npm run build
git add src/hooks/workbenchTerminalBuffer* src/hooks/useWorkbenchTerminalBuffers.tsx src/mobile/MobileApp.tsx src/pages/Workbench/terminalReplay.test.ts src/mobile/mobileTerminalReplay.test.ts
git commit -m "perf: batch terminal ring buffer updates"
```

### Task 8: Improve Workbench Project Discovery and Enforce i18n

**Files:**
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.tsx`
- Modify: `web/src/components/domain/WorkbenchProjectRail/WorkbenchProjectRail.module.css`
- Modify: `web/src/components/domain/WorkbenchProjectRail/workbenchProjectRailStyles.test.ts`
- Modify: `web/src/i18n/locales/{zh,en}/{workbench,nav,common}.json`
- Create: `web/scripts/check-i18n-jsx.mjs`
- Create: `web/scripts/check-i18n-jsx.test.mjs`
- Modify: `web/src/i18n/index.ts` tests or create `web/src/i18n/localeParity.test.ts`
- Modify: `web/package.json`, `.github/workflows/ci.yml`

**Interfaces:** Produces CLI `npm run check:i18n`; AST checks production TSX JSXText and `title/aria-label/placeholder/alt` literal attributes, with finite allowlist exported as `ALLOWED_LITERAL_COPY`.

- [ ] **Step 1: Write rail behavior and AST checker tests**

Assert rail section heading, empty explanation, local/remote CTAs, keyboard names/status. Checker fixtures cover translated expression, Chinese JSXText failure, English aria-label failure, symbol-only pass, brand allowlist pass, test/DesignSystem exclusion, zh/en key parity.

- [ ] **Step 2: Verify RED**

Run: `cd web && node --test scripts/check-i18n-jsx.test.mjs && npm test -- WorkbenchProjectRail localeParity`
Expected: missing checker/parity and rail copy.

- [ ] **Step 3: Implement AST scan and migrate violations**

Load `typescript` compiler API, recursively scan `src/**/*.tsx`, exclude `.test/.stories`, `DesignSystem`, generated declarations. Report `file:line:column`. Migrate all current reported literals to domain namespace; allow only `cc-partner`, `Claude Code`, `Git`, `GitHub`, `HTML`, `JSON`, `SQLite`, `tmux`, `WSL` and pure punctuation.

- [ ] **Step 4: Implement rail IA and wire CI**

Reuse existing local/remote picker callbacks; do not add new project API. Add section label and empty CTA; preserve selected project/deep link. CI runs locale parity and `check:i18n` before build.

- [ ] **Step 5: Verify and commit**

```bash
cd web
node --test scripts/check-i18n-jsx.test.mjs
npm run check:i18n
npm test -- WorkbenchProjectRail localeParity
npm run lint
npm run build
git add scripts src/components/domain/WorkbenchProjectRail src/i18n package.json ../.github/workflows/ci.yml
git commit -m "feat: clarify project navigation and enforce localized copy"
```

### Task 9: Split the Shared Type Monolith by Domain

**Files:**
- Create: `web/src/lib/types/core.ts`
- Create: `web/src/lib/types/settings.ts`
- Create: `web/src/lib/types/workbench.ts`
- Create: `web/src/lib/types/orchestrator.ts`
- Create: `web/src/lib/types/attention.ts`
- Create: `web/src/lib/types/index.ts`
- Modify: `web/src/lib/types.ts`
- Create: `web/src/lib/types/typeBarrel.test.ts`

**Interfaces:** Existing `import type {...} from '@/lib/types'` remains valid; domain files import only upstream `core` types, never the compatibility barrel.

- [ ] **Step 1: Add compile-only barrel contract**

Import representative public types from old path and new domain paths; use `satisfies`/`expectTypeOf` to assert identical shapes. Add a script assertion that domain files do not import `../types` or `@/lib/types`.

- [ ] **Step 2: Verify baseline and move one domain at a time**

Run `cd web && npm test -- typeBarrel && npm run build` after each move in order: core→settings→workbench→orchestrator→attention. Expected: first test initially fails because domain modules do not exist; every intermediate build after creation passes.

- [ ] **Step 3: Make the legacy file a pure barrel**

```ts
export * from './types/index';
```

Resolve cross-domain dependency through direct relative imports and `export type`; no runtime values or duplicate interfaces in the barrel.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- typeBarrel
npm run lint
npm run build
git add src/lib/types.ts src/lib/types
git commit -m "refactor: split frontend types by domain"
```

### Task 10: Split Giant Views, Run Full Gates, and Update Contracts

**Files:**
- Create: `web/src/pages/Settings/useSettingsController.ts`, `SettingsGeneralPanel.tsx`, `SettingsSyncPanel.tsx`, `SettingsDependenciesPanel.tsx`
- Modify: `web/src/pages/Settings/Settings.tsx` and tests
- Create: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`, `views/OrchestratorBoard.tsx`, `views/OrchestratorTaskDrawer.tsx`, `views/OrchestratorCreateDialog.tsx`, `views/OrchestratorOutbox.tsx`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx` and characterization tests
- Create: `web/src/mobile/controllers/useMobileAutomationController.ts`, `components/MobileAutomationTaskList.tsx`, `MobileAutomationTaskDetail.tsx`, `MobileAutomationCreateDialog.tsx`, `MobileAutomationOutbox.tsx`
- Modify: `web/src/mobile/components/MobileAutomationPanel.tsx` and tests
- Modify: `web/src/pages/Workbench/Workbench.tsx` only for terminal-tab leaf extraction from T4
- Modify: `AGENTS.md`, `web/CLAUDE.md`, `docs/prd.md`
- Create: `web/tests/frontend-foundation.spec.ts`

**Interfaces:** Settings controller consumes the already-landed `SettingsResourceResults`; Orchestrator controllers call existing APIs/actions and expose typed view props; views contain no API imports. Workbench remains composed from the existing seven controllers.

- [ ] **Step 1: Add characterization and ownership tests**

Lock Settings dirty/save/reset/partial-error, Orchestrator filter/create/action/detail/outbox/deep-link, Mobile Automation equivalent flows. Add static assertions: view files contain no `Api.`/transport calls; controller files contain no modal/board JSX; Workbench has no eighth aggregate controller.

- [ ] **Step 2: Split Settings only and verify**

Move resource/save orchestration into `useSettingsController`; panels consume explicit props. Run `npm test -- src/pages/Settings && npm run build`. Expected: behavior tests PASS and `Settings.tsx` owns only tab/layout composition. Commit:

```bash
git add src/pages/Settings
git commit -m "refactor: split settings controller and panels"
```

- [ ] **Step 3: Split desktop Orchestrator only and verify**

Move existing code without changing action order/status copy. Run `npm test -- src/pages/Orchestrator && npm run build`. Expected: all board/action/focus tests PASS. Commit:

```bash
git add src/pages/Orchestrator
git commit -m "refactor: split orchestrator controller and views"
```

- [ ] **Step 4: Split Mobile Automation and finish Workbench leaf**

Move transport/actions to controller and rendering to four views; preserve active project/task and dirty guards. Extract terminal tabs from Workbench into a leaf view using T4 contracts; do not move controller state. Assert formatted `Workbench.tsx` ≤ 1,200 lines.

```bash
cd web
npm test -- MobileAutomationPanel WorkbenchTerminal src/pages/Workbench
npm run build
git add src/mobile src/pages/Workbench
git commit -m "refactor: split mobile automation and workbench tab views"
```

- [ ] **Step 5: Add E2E accessibility/performance smoke**

Cover dialog focus loop/restore, mobile drawer Escape, Attention one tab stop, terminal arrow navigation, route crash fixture recovery, and reduced-motion media emulation. Bundle contract remains a build-time test, not timing-flaky E2E.

- [ ] **Step 6: Capture the UX state matrix and run expert review**

Use Playwright screenshots for light/dark, desktop/mobile, normal/empty/loading/partial-error/offline, Dialog/Drawer and dense Workbench states. Review functionality, craft quality, visual hierarchy, philosophy alignment and originality in that priority order. Reject decorative gradients/icons/filler data, one-off colors and extra card layers that are not supported by real product context. Convert actionable interaction/a11y findings into tests; record spacing/alignment/responsive findings in the implementation PR checklist and visual smoke evidence, without creating a one-off report file.

Expected: no critical or important UX finding remains open; personal-style preferences are explicitly excluded from implementation scope; the screenshots show the same product identity before and after the structural split.

- [ ] **Step 7: Update docs and run all gates**

Document new primitives, token/i18n/bundle contracts, type/controller ownership, project rail behavior and manual VoiceOver/NVDA smoke. Then run:

```bash
cd web
npm run check:css-tokens
npm run check:i18n
npm run lint
npm run build
npm run check:bundle
npm test
npm run test:e2e
test "$(wc -l < src/pages/Workbench/Workbench.tsx)" -le 1200
test -z "$(find dist -name '*.map' -print -quit)"
git -C .. status --short
```

Expected: all gates exit 0; Workbench line test passes; no map path is printed; status contains only this plan’s implementation/docs.

- [ ] **Step 8: Commit final contracts**

```bash
git -C .. add web/tests/frontend-foundation.spec.ts AGENTS.md web/CLAUDE.md docs/prd.md
git -C .. commit -m "test: enforce frontend foundation contracts"
```
