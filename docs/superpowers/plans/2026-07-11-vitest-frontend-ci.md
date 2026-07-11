# Vitest Migration and Frontend CI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把现有 47 个前端单元测试完整迁移到 Vitest，并把单元测试与 Playwright E2E 作为相互独立、失败可诊断的 CI 合并门禁。

**Architecture:** 复用现有 Vite alias 与 TypeScript 配置，新增 Node 默认环境的 Vitest 配置；以受版本控制的迁移清单防止测试漏迁，分批把顶层自执行断言包进 `describe`/`test`，不改业务断言语义。CI 将现有 `quality` 拆出独立 `frontend-unit` 与 `frontend-e2e` job，E2E 失败统一上传 Playwright 产物。

**Tech Stack:** React 19, TypeScript 6, Vite 8, Vitest, Playwright 1.60, GitHub Actions, Node.js 22.

## Global Constraints

- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md` 与 `web/CLAUDE.md`；所有修改使用 UTF-8。
- 本计划只迁移 runner 结构，不重写 helper、业务断言或产品行为，不设置覆盖率百分比 KPI。
- Vitest 默认使用 Node environment；只有真实访问 DOM 的测试文件才允许在文件顶部声明 `// @vitest-environment jsdom`。
- 不使用 `globals: true`；每个测试显式从 `vitest` 导入 `describe`、`expect`、`test`。
- 已迁移文件只由 Vitest 执行；legacy runner 只执行迁移清单中仍为 `legacy` 的文件。
- CI 只使用 `npm ci` 安装锁定依赖，禁止 `npx --yes` 浮动安装测试 runner。
- 每批迁移先证明目标测试在 Vitest 下失败或无法收集，再完成转换并运行该批与全量测试。

---

## Task Dependency Graph

最大并行 waves：`T1 → (T2 | T6) → T3 → T4 → T5 → T7 → T8`。T3/T4 共享迁移清单而串行；T7 同时依赖 T5 的完整 Vitest 迁移和 T6 的 Playwright 诊断。此图是并行上界；发现新增写集或测试资源冲突时只能进一步串行。

## File Structure

- Modify `web/package.json`: 增加 Vitest/Playwright 固定脚本和 Vitest dev dependency。
- Modify `web/package-lock.json`: 锁定 Vitest 依赖图。
- Create `web/vitest.config.ts`: 复用 `vite.config.ts`，默认 Node environment，并只收集 `src/**/*.test.{ts,tsx}`。
- Create then delete `web/scripts/test-migration-manifest.mjs`: 迁移期间的 47 文件互斥清单；全部迁移后删除，最终由 Vitest 自动发现。
- Create `web/scripts/run-legacy-tests.mjs`: 迁移期间只运行 manifest 中 `legacy` 文件；清零后删除。
- Modify all 47 `web/src/**/*.test.ts`: 顶层自执行断言转换成 Vitest suite/case。
- Create `web/tests/fixtures.ts`: 收集 console/pageerror 并在失败时附加浏览器日志。
- Modify `web/playwright.config.ts`: 失败截图、trace、CI 重试和稳定输出目录。
- Modify `.github/workflows/ci.yml`: 增加独立 unit/E2E 门禁与 E2E artifact。
- Modify `web/CLAUDE.md`: 用四条稳定 npm script 替代逐文件 `tsx` 命令清单。

## Migration Manifest

清单必须逐字包含以下 47 个相对 `web/` 的路径，并以 `{ path, runner: 'legacy' | 'vitest' }` 表示状态：

```text
src/api/mobile.test.ts
src/api/orchestrator.test.ts
src/api/promptOptimizer.test.ts
src/api/workbenchHttp.test.ts
src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
src/components/domain/WorkbenchCodeEditor/workbenchCodeEditorTheme.test.ts
src/components/domain/WorkbenchHtmlPreview/htmlAssets.test.ts
src/components/domain/WorkbenchProjectRail/projectSessionStats.test.ts
src/components/domain/WorkbenchProjectRail/workbenchProjectRailStyles.test.ts
src/components/domain/WorkbenchRemoteProjectPicker/workbenchRemoteProjectPickerLayout.test.ts
src/hooks/workbenchHttpEvents.test.ts
src/hooks/workbenchTerminalBuffer.test.ts
src/lib/backendLifecycle.test.ts
src/lib/claudeCodeAssets.test.ts
src/lib/lanFirewallDependency.test.ts
src/lib/orchestrator.test.ts
src/lib/orchestratorRemote.test.ts
src/lib/permissionEntries.test.ts
src/lib/platform.test.ts
src/lib/workbenchDependency.test.ts
src/mobile/MobileAutomationPanel.test.ts
src/mobile/MobileWorktreeQuickSwitch.test.ts
src/mobile/mobileBrowserPanel.test.ts
src/mobile/mobilePanelState.test.ts
src/mobile/mobileTerminalReplay.test.ts
src/mobile/mobileTerminalTouchScroll.test.ts
src/mobile/mobileWorkbenchState.test.ts
src/pages/Health/HabitStatsCard.test.ts
src/pages/Orchestrator/orchestratorActions.test.ts
src/pages/Orchestrator/orchestratorBoard.test.ts
src/pages/Settings/HealthPanel.test.ts
src/pages/Settings/automationSettingsState.test.ts
src/pages/Settings/settingsState.test.ts
src/pages/Settings/shortcutRecorder.test.ts
src/pages/Workbench/promptOptimizerWidget.test.ts
src/pages/Workbench/terminalOptions.test.ts
src/pages/Workbench/terminalReplay.test.ts
src/pages/Workbench/terminalSessionOrder.test.ts
src/pages/Workbench/terminalSizing.test.ts
src/pages/Workbench/workbenchAutomationView.test.ts
src/pages/Workbench/workbenchBrowserPreview.test.ts
src/pages/Workbench/workbenchDeepLink.test.ts
src/pages/Workbench/workbenchFiles.test.ts
src/pages/Workbench/workbenchLayerStyles.test.ts
src/pages/Workbench/workbenchRemoteProjects.test.ts
src/pages/Workbench/workbenchWorkspaceSwitch.test.ts
src/pages/Workbench/workbenchWorktrees.test.ts
```

---

### Task 1: Lock the Test Inventory and Add the Runner

**Files:**
- Create: `web/scripts/test-migration-manifest.mjs`
- Create: `web/scripts/run-legacy-tests.mjs`
- Create: `web/vitest.config.ts`
- Modify: `web/package.json`
- Modify: `web/package-lock.json`

- [ ] **Step 1: Add a manifest integrity command that initially fails**

`test-migration-manifest.mjs` must export `TEST_MIGRATION_MANIFEST` and, when executed with `--check`, recursively walk `src/` with `node:fs/promises.readdir`, compare the sorted `*.test.ts`/`*.test.tsx` paths with the manifest, print missing/extra paths to stderr and exit `1` on mismatch. Add a temporary omitted entry, run the check, and confirm failure before restoring all 47 entries.

```bash
cd web
node scripts/test-migration-manifest.mjs --check
```

Expected after restoring the full inventory: `47 test files accounted for` and exit `0`.

- [ ] **Step 2: Install the local Vitest runner and add transitional scripts**

Add `vitest` to `devDependencies` with the package-manager-selected compatible version and update scripts to:

```json
{
  "test": "node scripts/test-migration-manifest.mjs --check && vitest run --passWithNoTests && node scripts/run-legacy-tests.mjs",
  "test:unit:watch": "vitest",
  "test:e2e": "playwright test",
  "test:all": "npm run test && npm run test:e2e"
}
```

`run-legacy-tests.mjs` must call `spawnSync(process.execPath, ['--import', 'tsx', testPath], { stdio: 'inherit' })` once per manifest entry marked `legacy`, stop on the first non-zero exit, and print `No legacy tests remain` when the set is empty. Add `tsx` as a temporary locked dev dependency while any legacy entries remain; this invocation is platform-neutral and never downloads a runner.

- [ ] **Step 3: Configure Vitest without duplicate collection**

Create `vitest.config.ts` by merging the existing Vite config and these test settings:

```ts
import { defineConfig, mergeConfig } from 'vitest/config';
import viteConfig from './vite.config';
import { legacyTestPaths } from './scripts/test-migration-manifest.mjs';

export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: 'node',
      globals: false,
      include: ['src/**/*.test.{ts,tsx}'],
      exclude: ['tests/**', 'node_modules/**', 'dist/**', ...legacyTestPaths],
      passWithNoTests: false,
    },
  }),
);
```

The manifest exports `legacyTestPaths` with paths relative to `web/`, so Vitest and tsx never execute the same file. After Task 5 deletes the manifest, remove its import and spread from the final config.

- [ ] **Step 4: Prove the empty Vitest side and legacy side are both enforced**

Run `npm test`. Expected: manifest check succeeds, Vitest reports no migrated tests but is temporarily allowed by the explicit CLI flag, and all 47 legacy files execute. Intentionally alter one legacy assertion, confirm non-zero exit, then revert only that intentional alteration. `passWithNoTests` remains false in config; only this pre-canary transition script overrides it.

- [ ] **Step 5: Commit the migration harness**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/package.json web/package-lock.json web/vitest.config.ts web/scripts
git commit -m "test: add vitest migration harness"
```

---

### Task 2: Establish the Vitest Conversion Pattern

**Files:**
- Modify: `web/src/pages/Workbench/workbenchDeepLink.test.ts`
- Modify: `web/src/lib/platform.test.ts`
- Modify: `web/scripts/test-migration-manifest.mjs`

- [ ] **Step 1: Mark the two canaries as Vitest and observe collection failure**

Change their manifest runner to `vitest`, run:

```bash
cd web
npm exec -- vitest run src/pages/Workbench/workbenchDeepLink.test.ts src/lib/platform.test.ts
```

Expected: collection reports no Vitest cases because both files only self-invoke helpers.

- [ ] **Step 2: Convert named self-invoking functions without changing assertions**

Use this exact structural pattern:

```ts
import { describe, test } from 'vitest';

describe('workbench deep links', () => {
  test('parses complete ids', () => {
    const parsed = parseWorkbenchDeepLink('?projectId=p1&worktreeId=w1&sessionId=s1');
    if (parsed.projectId !== 'p1' || parsed.worktreeId !== 'w1' || parsed.sessionId !== 's1') {
      throw new Error(`expected complete deep link ids, got ${JSON.stringify(parsed)}`);
    }
  });
});
```

Delete only the bottom-level `testXxx();` calls and obsolete wrapper functions. Preserve fixture values, error strings, loops, filesystem reads and helper assertions.

- [ ] **Step 3: Verify canaries and duplicate protection**

```bash
cd web
npm exec -- vitest run src/pages/Workbench/workbenchDeepLink.test.ts src/lib/platform.test.ts
npm test
```

Expected: both canaries pass once under Vitest; legacy runner skips them; all other legacy tests still pass.

Remove the temporary `--passWithNoTests` flag from the transitional `test` script now that Vitest always has at least two cases.

- [ ] **Step 4: Commit the conversion pattern**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/src/pages/Workbench/workbenchDeepLink.test.ts web/src/lib/platform.test.ts web/scripts/test-migration-manifest.mjs
git commit -m "test: establish vitest conversion pattern"
```

---

### Task 3: Migrate API, Hook, and Library Tests

**Files:**
- Modify: the 13 remaining files under `web/src/api`, `web/src/hooks`, and `web/src/lib` listed in the manifest
- Modify: `web/scripts/test-migration-manifest.mjs`

- [ ] **Step 1: Move the API batch to Vitest and confirm no cases are silently accepted**

Mark the four `src/api/*.test.ts` files as `vitest`, convert each top-level assertion block into named `test` cases under one `describe` per module, then run:

```bash
cd web
npm exec -- vitest run src/api
```

Expected: four files collected, all cases pass, no file reports zero tests.

- [ ] **Step 2: Move the hook batch**

Convert `workbenchHttpEvents.test.ts` and `workbenchTerminalBuffer.test.ts`. Keep fake event sequences and buffer fixtures byte-for-byte identical. Run `npm exec -- vitest run src/hooks`.

- [ ] **Step 3: Move the library batch**

Convert every `src/lib/*.test.ts` except the already migrated `platform.test.ts`. Loop-based status matrices remain one named test per matrix; do not explode them into rewritten snapshots. Run `npm exec -- vitest run src/lib`.

For async/API files, replace bottom-level `.catch(() => process.exit(1))` runners with `test('...', async () => { await ... })`; never leave an un-awaited Promise or `process.exit` inside a Vitest case.

- [ ] **Step 4: Run the mixed full suite and inventory check**

```bash
cd web
node scripts/test-migration-manifest.mjs --check
npm test
```

Expected: all migrated tests pass under Vitest and remaining component/mobile/page tests pass only under legacy tsx.

- [ ] **Step 5: Commit the batch**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/src/api web/src/hooks web/src/lib web/scripts/test-migration-manifest.mjs
git commit -m "test: migrate api hooks and library tests to vitest"
```

---

### Task 4: Migrate Component and Mobile Tests

**Files:**
- Modify: all 6 component test files and all 7 mobile test files in the manifest
- Modify: `web/scripts/test-migration-manifest.mjs`

- [ ] **Step 1: Convert component source/style contract tests**

Keep filesystem path resolution based on `import.meta.url`; wrap each contract in explicit cases. `workbenchAutomationView.test.ts` and `backendLifecycle.test.ts` may reference `document` as searched source text, but must remain Node tests unless they execute DOM APIs.

Convert the explicit process runner in `workbenchCodeEditorTheme.test.ts` into an awaited test; the final test corpus must contain no `process.exit`.

- [ ] **Step 2: Verify component batch in Node environment**

```bash
cd web
npm exec -- vitest run src/components
```

Expected: all six files pass without jsdom.

- [ ] **Step 3: Convert mobile state and source contract tests**

Preserve existing panel order, terminal replay, touch scroll and remote project fixtures. Group cases by exported helper; remove bottom self-invocations only after every function is represented by a Vitest `test`.

`MobileWorktreeQuickSwitch.test.ts` keeps its `renderToStaticMarkup`/Node module registration harness and remains in Node environment; it does not gain jsdom.

- [ ] **Step 4: Verify mobile batch and mixed suite**

```bash
cd web
npm exec -- vitest run src/mobile
npm test
```

Expected: all component/mobile files run under Vitest and no duplicate execution appears in legacy output.

- [ ] **Step 5: Commit the batch**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/src/components web/src/mobile web/scripts/test-migration-manifest.mjs
git commit -m "test: migrate component and mobile tests to vitest"
```

---

### Task 5: Migrate Page Tests and Remove the Legacy Runner

**Files:**
- Modify: all remaining page tests in the manifest
- Modify: `web/package.json`
- Modify: `web/package-lock.json`
- Modify: `web/vitest.config.ts`
- Delete: `web/scripts/run-legacy-tests.mjs`
- Delete: `web/scripts/test-migration-manifest.mjs`

- [ ] **Step 1: Convert Health, Orchestrator, and Settings tests**

Migrate the seven tests under those page directories and run `npm exec -- vitest run src/pages/Health src/pages/Orchestrator src/pages/Settings`.

`HabitStatsCard.test.ts` and `HealthPanel.test.ts` keep their React SSR harness and remain Node tests.

- [ ] **Step 2: Convert all Workbench page tests**

Migrate the 13 `src/pages/Workbench/*.test.ts` files. Keep terminal escape sequences, remote IDs, source-text selectors and CSS assertions unchanged. Run `npm exec -- vitest run src/pages/Workbench`.

- [ ] **Step 3: Mark all 47 manifest entries Vitest and prove legacy is empty**

Run `npm test`; expected legacy output is exactly `No legacy tests remain`. Compare `find src -name '*.test.ts' -o -name '*.test.tsx'` with the manifest and confirm 47/47.

- [ ] **Step 4: Remove transitional machinery**

Change scripts to the final contract:

```json
{
  "test": "vitest run",
  "test:unit:watch": "vitest",
  "test:e2e": "playwright test",
  "test:all": "npm run test && npm run test:e2e"
}
```

Delete both migration scripts, remove `tsx` if no other package script/import uses it, and simplify Vitest config so it no longer excludes legacy paths. Do not keep a hand-maintained inventory: final discovery belongs solely to `vitest run`.

- [ ] **Step 5: Verify a deliberate failure is observable**

Temporarily invert one assertion in `workbenchDeepLink.test.ts`, run `npm test`, confirm exit code is non-zero and the case name is printed, then revert the deliberate inversion.

- [ ] **Step 6: Commit the completed migration**

```bash
git -C "$(git rev-parse --show-toplevel)" add web
git commit -m "test: complete frontend vitest migration"
```

---

### Task 6: Harden Playwright Diagnostics

**Files:**
- Create: `web/tests/fixtures.ts`
- Modify: `web/playwright.config.ts`
- Modify: `web/tests/screenshot-overlay.spec.ts`

- [ ] **Step 1: Add a failing browser-console guard test helper**

Create a Playwright fixture that registers `page.on('console')` and `page.on('pageerror')`, collects error-level messages, attaches `browser-logs` to `testInfo` on failure, and fails the case on unexpected browser errors. Import `test`/`expect` from `./fixtures` in the existing spec. Temporarily emit `console.error('playwright-canary')`, confirm failure and attachment creation, then remove the canary.

- [ ] **Step 2: Configure deterministic failure artifacts**

Set `outputDir: 'test-results'`, CI retries to `1`, `screenshot: 'only-on-failure'`, `trace: 'retain-on-failure'`, and `video: 'retain-on-failure'`. Keep one Chromium project and the existing local web server.

- [ ] **Step 3: Run E2E locally**

```bash
cd web
npm exec -- playwright install chromium
npm run test:e2e
```

Expected: screenshot overlay tests pass and `test-results` is empty or contains only successful-run metadata.

- [ ] **Step 4: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/playwright.config.ts web/tests/fixtures.ts web/tests/screenshot-overlay.spec.ts
git commit -m "test: retain playwright failure diagnostics"
```

---

### Task 7: Add Independent Unit and E2E CI Gates

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Preserve the Ubuntu quality job but remove frontend test ambiguity**

Keep lint/build/Rust fmt/clippy/test in `quality`. Add no `continue-on-error`. Cache keys continue using `web/package-lock.json` and `src-tauri/Cargo.lock`.

- [ ] **Step 2: Add `frontend-unit` job**

The job runs Ubuntu 22.04, checkout, Node 22, `cd web && npm ci`, then `npm test`. Give it a 15-minute timeout.

- [ ] **Step 3: Add `frontend-e2e` job**

The job runs checkout, Node 22, `npm ci`, `npm exec -- playwright install --with-deps chromium`, and `npm run test:e2e`. Give it a 20-minute timeout. On `failure()`, upload `web/test-results/**` and `web/playwright-report/**` using `actions/upload-artifact@v4` with 7-day retention.

- [ ] **Step 4: Validate workflow syntax and local equivalents**

```bash
cd web
npm ci
npm test
npm run lint
npm run build
npm run test:e2e
```

Expected: every command exits `0`. Inspect `.github/workflows/ci.yml` and confirm unit and E2E are separate jobs and neither is conditionally skipped for code PRs.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add .github/workflows/ci.yml
git commit -m "ci: gate frontend unit and e2e tests"
```

---

### Task 8: Update Frontend Instructions and Run Final Verification

**Files:**
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: Replace the legacy command inventory**

Document `npm test`, `npm run test:unit:watch`, `npm run test:e2e`, `npm run test:all`, Node-by-default environment, and the rule that DOM tests declare jsdom explicitly. Remove every `npx --yes tsx <file>` testing instruction.

- [ ] **Step 2: Run final clean-install verification**

```bash
cd web
rm -rf node_modules
npm ci
npm test
npm run lint
npm run build
npm run test:e2e
```

Expected: Vitest automatically collects all 47 files; any failure returns non-zero; lint/build/E2E pass.

- [ ] **Step 3: Inspect the final migration invariants**

```bash
rg -n "npx --yes tsx|run-legacy-tests|test-migration-manifest|runner: 'legacy'" web .github/workflows/ci.yml
```

Expected: no matches. `rg -n "from 'vitest'" web/src --glob '*.test.ts' --glob '*.test.tsx' | wc -l` must report `47`, and `rg -n "process\.exit" web/src --glob '*.test.ts' --glob '*.test.tsx'` must report no matches.

- [ ] **Step 4: Commit documentation**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/CLAUDE.md
git commit -m "docs: document frontend test entrypoints"
```

## Completion Contract

- `npm ci && npm test` discovers all 47 tests without a hand-maintained execution list.
- Vitest and Playwright are independent required CI jobs; E2E failure artifacts are retained.
- No test is duplicated, silently omitted, or left on the legacy runner.
- The migration changes runner structure only; existing business fixtures and assertions retain their meaning.
- `web/CLAUDE.md` and package scripts expose the same four stable commands.
