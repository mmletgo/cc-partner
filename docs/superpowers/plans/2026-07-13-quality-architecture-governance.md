# Quality Gates and Architecture Governance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为关键桌面/移动旅程补齐分层 E2E 和故障注入，以运行时 DTO schema、bundle/token/module 门禁和机器可读 coverage traceability 持续约束正确性、性能与可维护性。

**Architecture:** L0 unit/contract、L1 deterministic browser mock、L2 Rust backend/Tauri integration、L3 real-device certification 各自保存清晰证据；前端在 IPC/HTTP 边界用轻量 decoder fail closed。CI 新增四个自测静态门禁，超大模块按 controller/view/repository/command 领域边界渐进拆分并由 no-growth ratchet 防止反弹。

**Tech Stack:** React 19, TypeScript 6 strict, Vitest 4, Playwright 1.60 Chromium, Vite 8 manifest, Node 22 built-ins, Tauri 2, Rust 2021, axum/sqlx integration tests, GitHub Actions.

## Global Constraints

- 开始前读取根 `AGENTS.md`、`web/CLAUDE.md`、`src-tauri/CLAUDE.md`、`docs/development/testing.md` 和对应 design spec；涉及 LAN 时先读取信任边界 spec/plan 的最终 socket peer、Origin/Host/WebSocket 与 stop 生命周期合同，不在本计划引入配对或逐设备鉴权。
- 不重复 Vitest 迁移、Workbench controller 拆分、P2P error envelope、Doctor 或现有 cross-platform smoke；只复用其入口并扩展 coverage。
- 所有 hooks 在 early return 前；所有新增/修改函数有规定格式中文 docstring；无 `any`、无 `!important`、无硬编码颜色/字体/间距。
- L1 browser mock 不能宣称验证 Tauri command 注册、WebView、系统权限、真实文件 dialog 或多主机 LAN；L3 未执行项必须写 `NOT VERIFIED`。
- 最终 bundle 预算统一为 desktop initial JS gzip ≤320 KiB、mobile ≤280 KiB、单 lazy chunk ≤700 KiB、全部 runtime JS gzip ≤1,400 KiB、dist sourcemap ≤2 MiB 或不随 dist 发布；baseline ratchet 只是过渡，不能成为第二套最终阈值。
- Runtime decoder 错误不得序列化 payload；只记录 contract/path/primitive kind/request ID。legacy default 必须逐 schema 显式声明。
- 模块治理保持路由、command 名、DTO、视觉、i18n 和产品行为；`lib/types.ts` 在迁移期保留兼容 re-export。
- 每个 task 采用 TDD：失败证据→最小实现→窄验证→影响面验证→逐 task commit；不要实际把多个 task 合为一个提交。

---

## Task Dependency Graph

```text
T1 harness ─> T2 desktop E2E ─┬─> T3 Workbench/mobile/LAN-boundary E2E ─> T9 traceability/docs
                              └─> T4 fault injection ────────────────┘
T5 runtime schemas ─────────────────────────────────────────────────> T9
T6 token/bundle gates ──────────────────────────────────────────────> T9
T7 module ratchet ─> T8 large-module decomposition ────────────────> T9
T1/T4/T5 ─> T10 L2/L3 certification and final matrix
```

可并行 wave：`T1 | T5 | T6 | T7` → `T2 | T4 | T8` → `T3` → `T9 | T10`。

## File Structure

- Create `web/tests/support/backendHarness.ts`, `backendHarness.test.ts`: deterministic Tauri/fetch/event/fault registry.
- Create `web/tests/{transfer,scratchpad,prompts,workbench,mobile-workbench,permissions,settings,lan-boundary}.spec.ts`: 八类 L1 旅程。
- Modify narrowly scoped pages/API only where tests expose missing stable selector/error/recovery behavior; no visual redesign in this plan.
- Create `web/src/lib/runtimeSchema.ts`, `web/src/lib/runtimeSchema.test.ts`, `web/src/lib/schemas/*.ts`: decoder primitives/domain contracts.
- Modify `web/src/api/client.ts`, `workbenchHttp.ts`, `attentionHttp.ts`, transfer/config/permission/orchestrator APIs: decoded IPC/HTTP boundaries.
- Create `scripts/check-css-tokens.mjs`, `check-bundle-budget.mjs`, `check-module-boundaries.mjs`, `check-quality-traceability.mjs` plus `--self-test` fixtures internal to each script.
- Create `scripts/bundle-budget-baseline.json`, `scripts/module-boundary-baseline.json`: temporary ratchet facts.
- Modify `web/vite.config.ts`, `web/package.json`, `.github/workflows/ci.yml`: manifest/budget scripts and CI.
- Refactor large files under `web/src/pages/{Orchestrator,Settings}`, `web/src/mobile`, `web/src/lib/types*`, `src-tauri/src/orchestrator/repo*`, `src-tauri/src/commands/{workbench,orchestrator}*`.
- Create `docs/development/quality-matrix.json`, `docs/development/real-device-certification.md`; modify `docs/development/testing.md`, relevant layered instruction files and PRD only for persistent behavior.
- Create/extend Rust integration tests under `src-tauri/tests/quality_faults.rs` and existing cross-platform smoke without claiming unsupported surfaces.

## Interfaces

```ts
export interface Decoder<T> {
  readonly name: string;
  decode(value: unknown, path?: string): T;
}

export class ContractDecodeError extends Error {
  readonly contract: string;
  readonly path: string;
  readonly actualKind: string;
}

export async function invokeDecoded<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  decoder: Decoder<T>,
): Promise<T>;

export interface BackendHarness {
  command(name: string, behavior: HarnessBehavior): void;
  route(method: 'GET' | 'POST', path: string, behavior: HarnessBehavior): void;
  emit(event: string, payload: unknown): void;
  calls(): readonly HarnessCall[];
  assertSettled(): void;
}

export type HarnessBehavior =
  | { kind: 'resolve'; value: unknown }
  | { kind: 'reject'; error: unknown }
  | { kind: 'defer'; key: string }
  | { kind: 'fault'; profile: FaultProfile };
```

Rust refactors keep `OrchestratorRepo` method signatures and `#[tauri::command]` exported function names unchanged. Test IDs in quality matrix are stable external identifiers and不得重命名 without updating docs and the traceability checker atomically.

---

### Task 1: Build the Deterministic Browser Backend Harness

**Files:**
- Create: `web/tests/support/backendHarness.ts`
- Create: `web/tests/support/backendHarness.test.ts`
- Modify: `web/tests/fixtures.ts`
- Modify: `web/playwright.config.ts`

**Interfaces:**
- Consumes: existing browser diagnostics fixture.
- Produces: `BackendHarness`, call/event/fetch injection, deferred resolution, `assertSettled` auto fixture.

- [ ] **Step 1: Write failing harness contract tests**

Cover registered invoke/fetch success, unregistered call failure with exact name/path, per-call sequence, deferred stale response, AbortSignal timeout, Tauri event subscribe/unsubscribe, and teardown detecting pending requests/unconsumed expectations.

- [ ] **Step 2: Run failure**

Run: `cd web && npm test -- tests/support/backendHarness.test.ts`

Expected: FAIL because harness module is missing.

- [ ] **Step 3: Implement the harness without production globals**

Install mocks through `page.addInitScript`; expose only test-side controller handles. Support exact path and path-regexp-free parameter templates such as `/api/transfer/status/:id` via a small segment matcher. Use fake timer control only inside test page; no production code checks a test flag.

- [ ] **Step 4: Integrate automatic diagnostics/settlement**

Extend fixtures with opt-in `backendHarness`; after use call `assertSettled`, attach call log on failure, and retain existing console/pageerror fail behavior. Configure mobile project only through per-test viewport, not a second browser binary.

- [ ] **Step 5: Verify and commit**

Run: `cd web && npm test -- tests/support/backendHarness.test.ts && npm run test:e2e -- attention.spec.ts`

Expected: harness tests and existing Attention E2E PASS.

```bash
git add web/tests/support web/tests/fixtures.ts web/playwright.config.ts
git commit -m "test: add deterministic browser backend harness"
```

---

### Task 2: Add Transfer, Scratchpad, Prompts, Permissions and Settings E2E

**Files:**
- Create: `web/tests/transfer.spec.ts`
- Create: `web/tests/scratchpad.spec.ts`
- Create: `web/tests/prompts.spec.ts`
- Create: `web/tests/permissions.spec.ts`
- Create: `web/tests/settings.spec.ts`
- Modify only when failing evidence requires: corresponding page/API/i18n files

**Interfaces:**
- Consumes: Task 1 harness.
- Produces: `E2E-TRANSFER-001`, `E2E-SCRATCH-001`, `E2E-PROMPTS-001`, `E2E-PERM-001`, `E2E-SETTINGS-001`.

- [ ] **Step 1: Write Transfer failing journey**

Mock a Tauri-selected absolute path, online device, `send_transfer`, progress/completed/cancelled events and send reject. Assert one send call carries absolute path/device ID; progress reaches completed; cancel is wired; unsupported pause/retry/open is hidden/disabled rather than no-op; dropzone Enter/Space activates selection.

- [ ] **Step 2: Write Scratchpad and Prompt failure/recovery journeys**

Scratchpad: type then navigate/unmount before 500ms, resolve flush, reload and assert content; reject save and assert unsaved/retry. Prompts: for create/update/delete separately reject once, assert original ordering/data restored plus visible retry; next success replaces local temp row with server DTO.

- [ ] **Step 3: Write Permission and Settings partial-failure journeys**

Permission check reject must leave “检查中”, show retry, and recover; notification reject does not block continuation; screenshot permission event navigates Welcome. Settings returns 10 successes + one non-core loader failure and must keep other tabs usable; save reject restores dirty state; deep links `dependencies/automation` land correctly.

- [ ] **Step 4: Run tests to verify real failures**

Run: `cd web && npm run test:e2e -- transfer.spec.ts scratchpad.spec.ts prompts.spec.ts permissions.spec.ts settings.spec.ts`

Expected before product fixes: one or more FAIL at no-op Transfer, unmount flush, optimistic rollback, permanent permission loading or whole-page Settings failure; no fixture/unregistered-command false positives.

- [ ] **Step 5: Make minimal behavior fixes and rerun**

Follow existing API/component patterns. Do not add new visual primitives here. Every fixed reject path keeps previous snapshot and exposes localized error/retry; hooks remain before returns.

Run: `cd web && npm run test:e2e -- transfer.spec.ts scratchpad.spec.ts prompts.spec.ts permissions.spec.ts settings.spec.ts && npm test -- src/pages/Transfer src/pages/Scratchpad src/pages/Prompts src/pages/Settings src/hooks/usePermissions`

Expected: all targeted E2E/unit PASS.

- [ ] **Step 6: Commit**

```bash
git add web/tests/transfer.spec.ts web/tests/scratchpad.spec.ts web/tests/prompts.spec.ts web/tests/permissions.spec.ts web/tests/settings.spec.ts web/src
git commit -m "test: cover critical desktop data journeys"
```

---

### Task 3: Add Workbench, Mobile and LAN Boundary E2E

**Files:**
- Create: `web/tests/workbench.spec.ts`
- Create: `web/tests/mobile-workbench.spec.ts`
- Create: `web/tests/lan-boundary.spec.ts`
- Modify narrowly: Workbench/mobile/LAN boundary API files required by failing tests

**Interfaces:**
- Consumes: Task 1 harness; LAN socket peer、browser boundary 与 local stop contracts from the trust-boundary spec.
- Produces: `E2E-WORKBENCH-001`, `E2E-MOBILE-001`, `E2E-LAN-001`.

- [ ] **Step 1: Write Workbench stale/offline journey**

Open project A, defer its worktree/files response, switch to B, resolve B then A and assert A never appears. Exercise terminal replay/focus, open/save file, remote offline disables writes, successful refresh restores writes, and listener count returns to baseline after navigation.

- [ ] **Step 2: Write mobile phone-viewport journey**

At 390×844 open `/mobile`, move Projects→Attention→Terminal→Files→Automation, assert drawer focus enters/restores and Escape closes, terminal disconnect/replay does not duplicate input, mobile write uses HTTP not Tauri invoke, and offline cache is marked stale.

- [ ] **Step 3: Write the unauthenticated LAN boundary journey**

Verify a legal LAN peer can use native P2P and same-origin `/mobile` to complete representative Workbench reads, file writes, terminal input and Orchestrator actions without credentials. Assert a public peer and a request that only forges `Forwarded`/`X-Forwarded-For` are rejected before the business handler; hostile Host/Origin and invalid WebSocket Origin are rejected; a remote peer cannot trigger backend stop, while the local lifecycle client remains covered by its dedicated integration test.

- [ ] **Step 4: Run failing tests then minimal fixes**

Run: `cd web && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts lan-boundary.spec.ts`

Expected: FAIL only on uncovered product/interaction contract. If the LAN boundary plan has not landed, mark this task dependency-blocked; do not invent endpoint names or add an authentication mock.

- [ ] **Step 5: Verify affected suites**

Run: `cd web && npm run test:e2e -- workbench.spec.ts mobile-workbench.spec.ts lan-boundary.spec.ts && npm test -- src/pages/Workbench src/mobile`

Expected: PASS, no leaked timers/listeners/page errors.

- [ ] **Step 6: Commit**

```bash
git add web/tests/workbench.spec.ts web/tests/mobile-workbench.spec.ts web/tests/lan-boundary.spec.ts web/src/pages/Workbench web/src/mobile web/src/api
git commit -m "test: cover workbench mobile and lan boundary journeys"
```

---

### Task 4: Establish Reusable Fault Injection at Frontend and Repository Boundaries

**Files:**
- Modify: `web/tests/support/backendHarness.ts`
- Create: `web/src/lib/faultRecovery.test.ts`
- Create: `src-tauri/tests/quality_faults.rs`
- Modify only for test seams: relevant repo/transport constructors

**Interfaces:**
- Consumes: `FaultProfile` from Task 1, existing trait/transport injection patterns.
- Produces: deterministic fault matrix; no production env toggle.

- [ ] **Step 1: Add failing frontend fault cases**

For each profile `networkOffline|timeout|malformedJson|permissionDenied|conflict|lanBoundaryRejected|crossSiteRejected`, assert typed classification, stale/cache policy, retry visibility and no optimistic state divergence. Add terminal event disconnect/reconnect with exact listener/input counts.

- [ ] **Step 2: Add failing Rust transaction/network cases**

Use repository test seam to fail row N in a batch, hold a SQLite write lock past one acquire attempt, and simulate peer response lost after commit. Assert full rollback or idempotent convergence, bounded timeout, and stable P2P error code.

- [ ] **Step 3: Verify failure**

Run: `cd web && npm test -- src/lib/faultRecovery.test.ts && cd ../src-tauri && cargo test --locked --test quality_faults -- --nocapture --test-threads=1`

Expected: FAIL on missing profiles/seams, not real network timing.

- [ ] **Step 4: Implement minimal injectable seams**

Frontend faults stay in harness. Rust accepts a test-only trait/callback at transaction/peer boundary; production constructor always uses real implementation and no environment variable can activate failures.

- [ ] **Step 5: Verify and commit**

Run both commands again; expected PASS with no sleep longer than bounded timeout.

```bash
git add web/tests/support/backendHarness.ts web/src/lib/faultRecovery.test.ts src-tauri/tests/quality_faults.rs src-tauri/src
git commit -m "test: inject deterministic transport and database faults"
```

---

### Task 5: Add Runtime Decoders to Critical IPC and HTTP DTOs

**Files:**
- Create: `web/src/lib/runtimeSchema.ts`
- Create: `web/src/lib/runtimeSchema.test.ts`
- Create: `web/src/lib/schemas/{protocol,attention,orchestrator,transfer,workbench,config}.ts`
- Create matching `.test.ts` files
- Modify: `web/src/api/client.ts`, `workbenchHttp.ts`, `attentionHttp.ts`, critical domain APIs

**Interfaces:**
- Consumes: global `Decoder<T>` contract.
- Produces: `invokeDecoded`, decoder-aware `getJson/postJson`, named domain decoders.

- [ ] **Step 1: Write primitive decoder failures**

Test object/array/string/boolean/finite number/literal/nullable/optional/union, exact path such as `$.items[2].status`, max array depth/length, and sanitized `ContractDecodeError` that does not contain fixture secret/body.

- [ ] **Step 2: Implement the 200-line-or-less decoder core**

Use composable closures, no external dependency and no `any`. Unknown extra fields are allowed for forward compatibility; required fields and enums are strict. Add `actualKind` based only on null/array/primitive/object.

- [ ] **Step 3: Write normal/legacy/malformed fixtures per domain**

At minimum cover health/capabilities/error, Attention, runtime/task/outbox, Transfer result/task/event, Workbench project/worktree/session/path/save and config/permissions. Each malformed fixture changes exactly one field.

- [ ] **Step 4: Wire critical boundaries**

`invokeDecoded` wraps existing normalized errors; decoder-aware HTTP overload decodes only successful bodies. Replace casts at listed critical calls incrementally; leave non-critical generic calls unchanged rather than pretending they are validated.

- [ ] **Step 5: Verify**

Run: `cd web && npm test -- src/lib/runtimeSchema.test.ts src/lib/schemas && npm run build && npm run lint`

Expected: all PASS; malformed values fail before page state mutation; bundle delta from decoder core <10 KiB gzip in Task 6 report.

- [ ] **Step 6: Commit**

```bash
git add web/src/lib/runtimeSchema.ts web/src/lib/runtimeSchema.test.ts web/src/lib/schemas web/src/api
git commit -m "feat: validate critical frontend contracts at runtime"
```

---

### Task 6: Enforce CSS Token and Bundle Budgets

**Files:**
- Create: `scripts/check-css-tokens.mjs`
- Create: `scripts/check-bundle-budget.mjs`
- Create: `scripts/bundle-budget-baseline.json`
- Modify: `web/src/styles/tokens.css` and CSS files with undefined vars
- Modify: `web/vite.config.ts`, `web/package.json`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: Vite manifest and built dist.
- Produces: `npm run check:tokens`, `npm run check:bundle`, script `--self-test` modes.

- [ ] **Step 1: Write self-tests before scanning the repo**

CSS fixtures cover unknown no-fallback fail, fallback pass, comment ignored, root/dark semantic mismatch fail. Bundle fixtures cover entry import traversal, shared chunk counted once, gzip thresholds, `.map` separate and missing manifest failure.

- [ ] **Step 2: Run expected self-test success and real-scan failure**

Run: `node scripts/check-css-tokens.mjs --self-test && node scripts/check-css-tokens.mjs`

Expected: self-test PASS; repo scan FAIL listing current undefined variables with file:line.

- [ ] **Step 3: Fix token drift at the source**

Map existing aliases to canonical `--bg/--surface/--meta/--border-soft/--border/--warn` or define a genuinely new token in both themes. Do not add aliases solely to silence the checker. Rerun until PASS.

- [ ] **Step 4: Enable Vite manifest and capture one baseline**

Build once, compute current desktop/mobile/chunk/total/map sizes, write exact values and commit SHA to baseline. Script enforces no growth until frontend performance plan reaches final 320/280 targets; final mode always enforces 320/280/700/1400/2MiB and cannot be raised through baseline.

- [ ] **Step 5: Add scripts/CI**

Add package scripts and run token check before build, bundle check after build in quality job. CI error prints actual, baseline, final target and top five chunks.

- [ ] **Step 6: Verify and commit**

Run: `cd web && npm run check:tokens && npm run build && npm run check:bundle && npm run lint`

Expected: PASS; final target remains visible even while baseline ratchet is active.

```bash
git add scripts/check-css-tokens.mjs scripts/check-bundle-budget.mjs scripts/bundle-budget-baseline.json web/src/styles web/src web/vite.config.ts web/package.json web/package-lock.json .github/workflows/ci.yml
git commit -m "ci: enforce css token and bundle budgets"
```

---

### Task 7: Add Module Size and Boundary Ratchet

**Files:**
- Create: `scripts/check-module-boundaries.mjs`
- Create: `scripts/module-boundary-baseline.json`
- Modify: `web/package.json`, `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: tracked `.ts/.tsx/.rs` files.
- Produces: no-growth baseline, TS soft/hard 1000/1500, Rust 2500/5000, expiring exception schema.

- [ ] **Step 1: Write self-test fixtures**

Assert new 1001-line TS warning/fail policy, new 1501 hard fail, existing baseline growth by one line fail, shrink pass/update guidance, expired exception fail, missing owner/reason fail, and generated/dist/vendor exclusions.

- [ ] **Step 2: Generate reviewed baseline**

Run script `--write-baseline`, inspect that it includes Orchestrator/Settings/MobileAutomation/types/repo/commands plus receiver/dependencies, and excludes tests/build artifacts. Every over-hard entry gets owner/reason/expiresAt ≤90 days.

- [ ] **Step 3: Add CI command**

`npm run check:modules` runs in quality before build. Script never rewrites baseline unless explicit `--write-baseline`; normal CI is read-only.

- [ ] **Step 4: Verify and commit**

Run: `node scripts/check-module-boundaries.mjs --self-test && node scripts/check-module-boundaries.mjs`

Expected: both exit 0; adding one temporary line to a baseline fixture fails, then revert it.

```bash
git add scripts/check-module-boundaries.mjs scripts/module-boundary-baseline.json web/package.json web/package-lock.json .github/workflows/ci.yml
git commit -m "ci: ratchet oversized source modules"
```

---

### Task 8: Decompose the Six Governed Large-Module Families

**Files:**
- Modify/Create: `web/src/pages/Orchestrator/{Orchestrator.tsx,useOrchestratorBoardController.ts,*Panel.tsx,*Dialog.tsx}`
- Modify/Create: `web/src/pages/Settings/{Settings.tsx,useSettingsController.ts,panels/*.tsx}`
- Modify/Create: `web/src/mobile/components/{MobileAutomationPanel.tsx,useMobileAutomationController.ts,MobileAutomation*.tsx}`
- Modify/Create: `web/src/lib/types.ts`, `web/src/lib/types/*.ts`
- Move/Create: `src-tauri/src/orchestrator/repo/{mod,schema,tasks,attempts,evidence,remote}.rs`
- Move/Create: `src-tauri/src/commands/workbench/*.rs`, `src-tauri/src/commands/orchestrator/*.rs`

**Interfaces:**
- Consumes: existing public props/API/command signatures and Task 7 ratchet.
- Produces: focused controllers/views/modules; compatibility barrels/modules.

- [ ] **Step 1: Add characterization/compile baselines before each family**

For frontend, render critical loading/error/success/dialog/action paths and snapshot semantics (not pixel snapshots). For Rust, add tests/import probes proving repo methods and `invoke_handler!` command names remain callable. Run the narrow suite before moving code.

- [ ] **Step 2: Split frontend one family per commit**

Order: types domain files → Settings controller/panels → Orchestrator controller/panels → Mobile automation controller/views. Controllers own data/effects; views receive narrow props; page files own composition. After each family run its tests, lint/build, module checker and commit separately.

- [ ] **Step 3: Split Rust repo by responsibility**

Move schema/migration, task state/CAS, attempts/evidence and remote/outbox helpers behind `repo/mod.rs`; keep `pub struct OrchestratorRepo` and all public method names. Do not alter SQL in the move commit. Run `cargo test --locked orchestrator::repo` before/after and commit.

- [ ] **Step 4: Split command modules without renaming commands**

Turn each large command file into directory `mod.rs`; group Workbench projects/sessions/files/git/browser and Orchestrator tasks/actions/runtime/remote. Re-export command functions so `lib.rs` `invoke_handler!` names and frontend command strings do not change. Run command/route tests and commit each command family separately.

- [ ] **Step 5: Enforce resulting bounds**

Update baseline only downward. Targets: the four frontend roots <1,000 lines; new files <1,000; Rust repo/command leaf files <2,500; compatibility `types.ts` only re-exports. Receiver/dependencies remain no-growth exceptions.

- [ ] **Step 6: Full verification**

Run:

```bash
cd web && npm run check:modules && npm test && npm run lint && npm run build
cd ../src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
```

Expected: all exit 0; no command/route/DTO/visual behavior change.

- [ ] **Step 7: Final ratchet commit**

```bash
git add scripts/module-boundary-baseline.json web/src src-tauri/src
git commit -m "refactor: enforce domain module boundaries"
```

---

### Task 9: Add Machine-Readable Coverage and Documentation Fact Traceability

**Files:**
- Create: `docs/development/quality-matrix.json`
- Create: `scripts/check-quality-traceability.mjs`
- Modify: `docs/development/testing.md`
- Modify: `scripts/check-docs.mjs`
- Modify: `.github/workflows/ci.yml`, `.github/workflows/docs.yml`
- Modify where behavior changed: `docs/prd.md`, `web/CLAUDE.md`, `src-tauri/CLAUDE.md`, root `AGENTS.md` component map only if components were added

**Interfaces:**
- Consumes: stable E2E IDs, CI jobs, scripts and L2/L3 records.
- Produces: validated quality matrix and docs guard.

- [ ] **Step 1: Write checker self-tests**

Fixtures cover duplicate ID, missing test file, unknown level/job, command not backed by package/workflow, expired L3 certification, nonexistent doc reference, and valid exclusions. All failures print JSON path/file:line where possible.

- [ ] **Step 2: Create the authoritative matrix**

Add eight L1 IDs, critical decoder L0 IDs, backend fault L2 IDs, existing smoke IDs and four L3 surfaces. Each entry includes level/tests/command/ciJob/platforms/exclusions; L3 adds commit/version/date/expiresAt/status.

- [ ] **Step 3: Update human docs without overclaiming**

`testing.md` summarizes layers and links IDs. Keep hosted exclusions explicit. PRD only records persistent flush/rollback/LAN boundary/fail-closed behavior that actually landed. Layered instructions record exact commands and module boundaries, not task changelog.

- [ ] **Step 4: Wire CI/docs workflow**

Run traceability in CI for code matrix changes and Docs workflow for docs/script changes. Extend docs checker only to verify referenced `E2E-/L2-/L3-` IDs exist; do not duplicate JSON validation logic.

- [ ] **Step 5: Verify and commit**

Run:

```bash
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
```

Expected: all exit 0.

```bash
git add docs/development/quality-matrix.json docs/development/testing.md docs/prd.md scripts/check-quality-traceability.mjs scripts/check-docs.mjs .github/workflows web/CLAUDE.md src-tauri/CLAUDE.md AGENTS.md
git commit -m "docs: trace quality evidence to product surfaces"
```

---

### Task 10: Execute L2/L3 Platform Certification and Final Quality Gate

**Files:**
- Create: `src-tauri/tests/quality_faults.rs` additions for command/route integration
- Create: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `.github/workflows/cross-platform-smoke.yml` only for automatable L2 checks

**Interfaces:**
- Consumes: Tasks 2–5, existing smoke workflow and LAN trust-boundary implementation.
- Produces: honest current evidence for GUI/permissions/WSL/mDNS and the credential-free LAN boundary; no lifecycle control-token artifacts.

- [ ] **Step 1: Add automatable L2 command/route cases**

Verify Transfer send DTO→command, scratch/prompt transaction failure, Settings partial command failure isolation at adapter boundary and malformed HTTP DTO. For LAN, verify legal LAN native/mobile requests complete representative reads and writes without credentials, while public peers, forwarded-header spoofing, hostile Host/Origin, invalid WebSocket Origin and remote backend stop are rejected. Run in isolated data dir and serialize port/process tests.

- [ ] **Step 2: Extend hosted smoke only where environment supports it**

Add macOS/Windows command/route integration that does not require UI prompts. Do not add `continue-on-error`; job summary retains WSL/tmux, GUI/WebView, macOS permission dialogs and multi-host mDNS as NOT VERIFIED.

- [ ] **Step 3: Execute the exact L3 matrix on real devices**

Record:

- macOS current supported version: packaged GUI launch, screen/accessibility/input/notification grant-deny-retry, screenshot clipboard.
- Windows current supported version: packaged GUI, file transfer path/dialog, native terminal; WSL+tmux separately.
- Ubuntu current supported version: AppImage/deb GUI and terminal/file flows.
- Two physical hosts on same LAN: mDNS discovery, native P2P and mobile relay credential-free reads/writes/actions, public-peer/forwarded-header/Host-Origin/WebSocket boundary rejection, remote backend stop rejection, and 1GiB transfer/resume.

Each row records app version, commit SHA, OS build, PASS/FAIL/NOT VERIFIED, sanitized evidence path, date and 90-day expiry. A missing device remains NOT VERIFIED; do not fabricate or substitute browser mock.

- [ ] **Step 4: Run all automated gates**

```bash
cd web
npm run check:tokens
npm run check:modules
npm run build
npm run check:bundle
npm run lint
npm test
npm run test:e2e
cd ../src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --locked --test quality_faults -- --nocapture --test-threads=1
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
```

Expected: all automated commands exit 0; matrix accurately reflects any L3 failures/unverified rows.

- [ ] **Step 5: Commit evidence metadata, not sensitive artifacts**

```bash
git add src-tauri/tests/quality_faults.rs .github/workflows/cross-platform-smoke.yml docs/development/real-device-certification.md docs/development/quality-matrix.json
git commit -m "test: certify cross surface quality matrix"
```

## Completion Contract

- 八个稳定 L1 E2E ID 在 CI 执行，具有 deterministic backend、console/pageerror、pending request/listener 泄漏诊断。
- L1/L2/L3 证据边界明确；真实 macOS 权限、Windows/WSL、三平台 GUI、多主机 mDNS 与无凭据 LAN 边界未执行时保持 NOT VERIFIED。
- 关键 HTTP/IPC/event DTO 在状态更新前 runtime decode；malformed/legacy/fault fixtures行为明确且不泄露 payload。
- CSS token、bundle 320/280 最终预算、module no-growth/threshold、quality traceability 均为带 self-test 的 CI 门禁。
- Orchestrator/Settings/MobileAutomation/types/Rust repo/commands 完成领域拆分，公共契约不变，其余巨型文件不再增长。
- 全部 frontend、Rust、route inventory、docs 和 traceability 命令通过，coverage matrix 可从 requirement 追到测试、命令、CI 和 exclusions。
