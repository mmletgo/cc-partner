# Post-Audit Improvement Program Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按八个独立、可验收子计划完成运行时权威、数据正确性、核心体验、新功能闭环、性能治理和真实设备发布认证。

**Architecture:** 本计划只负责编排、依赖门禁、覆盖追踪和最终事实校准，不复制领域实现步骤。每个 N1–N8 使用独立 worktree/branch、TDD、focused gate 和提交序列；N8 只在当前 Apple Silicon Mac 上认证 N1–N7 合并后的 `macos-aarch64-beta` 候选版本。

**Tech Stack:** Git worktrees, Rust/Tauri/axum/sqlx, React/TypeScript/Vite/Vitest/Playwright, quality/bundle/module/docs checkers, physical-device certification.

## Global Constraints

- 产品审计基线提交固定为 `bb980fd`；执行基线必须是包含本轮九份 spec 与九份 plan 的后继 planning commit。2026-07-13 S1–S6 已落地，不因旧 checkbox 未勾选而重新实现。
- 必读 `docs/superpowers/specs/2026-07-14-post-audit-improvement-program-design.md` 和当前子计划。
- 顺序固定为 `N1 → (N2 | N3) → (N4 | N5) → N6 → N7 → N8`；N4 先拥有 AppShell/Settings/deep-link 集成面，N6 只在 N4 合并后增量修改这些文件。
- 每个子计划独立 worktree/branch/验证/commit；共享文件由最早 owning plan 修改，后续计划 rebase 后增量更新。
- 固定 LAN 无身份模型不变；禁止新增配对、token、可信设备、权限矩阵或可切换 LAN 模式。
- 数据库 schema 继续使用 runtime 幂等 init/repo helper，`migrations/0001_init.sql` 同步为文档；不启用 `sqlx::migrate!`。
- 所有新增/修改业务函数与类遵守根 AGENTS 的中文 docstring、UTF-8、Rust/TypeScript 严格类型和 React Hooks 顺序要求。
- 任何计划失败都不得用后续计划掩盖；N8 不修 bug，只回退 owning plan。
- N8 当前固定 `claimMode=platform-beta`、`claimProfile=macos-aarch64-beta`；Windows、Ubuntu 与其他缺失硬件表面保持 `NOT VERIFIED`，不进入当前构建/执行/发布门禁。

---

## Dependency Graph

```text
T1 baseline/branch map
  → T2 N1 runtime authority
    → (T3 N2 sync/recovery | T4 N3 frontend/mobile)
      → (T5 N4 core UX | T6 N5 transfer)
        → T7 N6 orchestrator
          → T8 N7 performance/maintainability
            → T9 N8 real-device certification
              → T10 final calibration
```

## File Structure

- Read: nine specs under `docs/superpowers/specs/2026-07-14-*`。
- Execute: eight domain plans under `docs/superpowers/plans/2026-07-14-*` plus this umbrella。
- Modify only through owning subplan: source, tests, PRD/protocol/CLAUDE/quality matrix。
- Preserve untouched: user-owned untracked `2026-07-13-whole-program-improvement-roadmap*` documents。

## Interfaces

- Consumes: each domain plan Completion Contract.
- Produces: wave go/no-go, cross-plan regression evidence, candidate freeze and release claim decision.
- Does not produce: production DTO/API/UI; those belong to N1–N8.

### Task 1: Freeze Baseline and Create the Branch/Worktree Map

**Files:**
- Read: all 2026-07-14 specs/plans
- Read: `docs/development/testing.md`, quality matrix and current Git state

- [ ] **Step 1: Verify baseline commit and existing dirty files**

Run: `git status --short --branch && git rev-parse HEAD`

Expected: HEAD is the planning-doc commit descended from `bb980fd`; user-owned untracked 2026-07-13 roadmap files are not staged by this program.

- [ ] **Step 2: Run baseline gates**

```bash
cd web && npm run lint && npm run build && npm test && npm run test:e2e
cd ../src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 3: Create active-wave `codex/` worktrees**

Use `superpowers:using-git-worktrees` at execution time and branch from the planning-doc commit so every worker sees the same spec/plan contracts. Do not create all write worktrees simultaneously unless their wave is active.

- [ ] **Step 4: Record allowed file ownership in the PR descriptions**

N1 owns control/runtime first; N2 owns sync schema/recovery; N3 owns async helpers; later tracks consume them after rebase.

- [ ] **Step 5: Do not create a repository summary file**

Branch/worktree state belongs in the issue/PR stack and Git history.

### Task 2: Execute N1 Runtime Authority

**Files:** Follow `docs/superpowers/plans/2026-07-14-runtime-authority-and-operational-diagnostics.md`.

- [ ] **Step 1: Execute all N1 tasks with TDD and per-task commits**

Require owner descriptor, generation CAS, control client, sole terminal owner, event relay, authority snapshot, bridge limits and diagnostics.

- [ ] **Step 2: Run N1 completion gate**

Expected: one owner under GUI/Mobile concurrency; config/Cloud Sync/terminal/snapshot converge; full Rust/frontend/protocol/docs gates pass.

- [ ] **Step 3: Merge/rebase N1 before opening dependent waves**

N2/N3 may start from the merged N1 commit; no local duplicate runtime fallback remains.

### Task 3: Execute N2 Sync Integrity and Recovery

**Files:** Follow `docs/superpowers/plans/2026-07-14-sync-integrity-conflict-and-recovery.md`.

- [ ] **Step 1: Execute typed protocol, transactions, truth aggregation, history/GC and backup/restore tasks**

- [ ] **Step 2: Run N2 completion gate**

Expected: equal second sync has zero payload; failures never count success; batch rollback, conflict/GC and unsafe archive tests pass.

- [ ] **Step 3: Merge N2 after N1 rebase**

Resolve shared runtime/protocol docs in favor of N1 owner model plus N2 domain result types.

### Task 4: Execute N3 Frontend Async and Mobile Transport

**Files:** Follow `docs/superpowers/plans/2026-07-14-frontend-async-state-and-mobile-transport.md`.

- [ ] **Step 1: Execute safe-save, stale/context, recovery, transport and accessibility tasks**

- [ ] **Step 2: Run N3 completion gate**

Expected: inverse-order tests pass; mobile mutation never blind-retries; full frontend gates pass.

- [ ] **Step 3: Merge N3 after N1 rebase**

N2 and N3 may merge in either order if shared docs are reconciled and both completion gates rerun.

### Task 5: Execute N4 Core Experience and LAN Onboarding

**Files:** Follow `docs/superpowers/plans/2026-07-14-core-workbench-experience-and-lan-onboarding.md`.

- [ ] **Step 1: Confirm N1/N3 consumption points**

Listener startup uses N1 control/lifecycle; Workbench launch/Welcome forms use N3 async feedback.

- [ ] **Step 2: Execute disclosure, Trending default-route guardrail, Workbench launch/empty states, navigation/sidebar, mobile groups and contrast tasks**

- [ ] **Step 3: Run N4 completion gate and visual review**

Expected: fixed viewports have no overlap/crop, no-project flow is focused, LAN wording preserves fixed boundary, contrast passes.

### Task 6: Execute N5 Transfer Lifecycle

**Files:** Follow `docs/superpowers/plans/2026-07-14-transfer-lifecycle-and-recovery.md`.

- [ ] **Step 1: Confirm N1/N3 dependencies**

Owner-scoped commands route through N1; uncertain UI uses N3 mutation/reconciliation policy.

- [ ] **Step 2: Execute phase/retry/resume/reconcile/Open/Reveal/UI tasks**

- [ ] **Step 3: Run N5 completion gate**

Expected: lost ACK does not duplicate finalize; old peer fallback and action matrix pass; 1 GiB dual-host certification remains deferred `NOT VERIFIED` outside the current Mac-only N8 scope.

### Task 7: Execute N6 Orchestrator Review, Workflow and Notifications

**Files:** Follow `docs/superpowers/plans/2026-07-14-orchestrator-review-workflow-and-notifications.md`.

- [ ] **Step 1: Confirm N1/N3 dependencies**

Runtime/deep links use owner snapshot; WORKFLOW draft/notification async behavior uses N3 contracts.

- [ ] **Step 2: Execute bounded diff, digest, WORKFLOW and notification tasks**

- [ ] **Step 3: Run N6 completion gate**

Expected: delivery drift conflicts, WORKFLOW cannot alter delivery, notifications are privacy-safe navigation-only.

### Task 8: Execute N7 Targeted Performance and Maintainability

**Files:** Follow `docs/superpowers/plans/2026-07-14-targeted-performance-and-maintainability.md`.

- [ ] **Step 1: Rebase onto merged N1–N6 behavior**

- [ ] **Step 2: Execute measured render/editor/index/watcher/network/module tasks**

- [ ] **Step 3: Run N7 completion gate**

Expected: root render and editor chunk improve; index bounded; module caps decrease; pool=1/xterm/seven-controller contracts remain.

### Task 9: Land Certification Infrastructure and Freeze the Integrated Candidate

**Files:**
- Follow Task 1 of `docs/superpowers/plans/2026-07-14-real-device-release-certification.md`.
- Modify: `README.md`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/backend-operations.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Land N8 checker, updater harness, RC and evidence-aware release infrastructure**

Merge the architecture-level evidence schema, Apple Silicon GUI/VoiceOver execution IDs, fixed `macos-aarch64-beta` profile, single-matrix non-public RC workflow, non-releasable updater certification harness and beta-only evidence-aware release gate before any candidate freeze. This is checker/workflow code and therefore must be part of `subjectCommit`; Windows/Linux/Intel build jobs remain deferred.

- [ ] **Step 2: Prepare a unique release version**

Run the N8 version/tag/release preflight and `scripts/bump-version.mjs`; the audited 0.6.7 baseline already has `v0.6.7`, so default to unused next minor `0.7.0` unless execution-time history requires a higher unused semver. Commit the synchronized version files before gates. Never reuse or move an existing tag/release.

- [ ] **Step 3: Run repository-wide gates**

```bash
cd web
npm run check:css-tokens
npm run check:i18n
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
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 4: Audit master coverage matrix row by row**

Each L0–L2 row links one passing test/evidence; L3 rows may still be honest `NOT VERIFIED` before execution.

- [ ] **Step 5: Inspect global invariants**

One runtime owner; no false success/data overwrite; fixed no-auth LAN; no sensitive diagnostics/notification/export; no raised budgets.

- [ ] **Step 6: Update durable product/methodology facts before freeze**

Update broad docs/AGENTS/CLAUDE now, while they can still be part of subject. Do not write future PASS claims or create a completion-summary Markdown. Preserve user-owned untracked old roadmap files.

- [ ] **Step 7: Commit calibration and rerun all gates**

```bash
git add README.md docs/prd.md docs/development/testing.md docs/development/backend-operations.md docs/development/quality-matrix.json docs/development/real-device-certification.md docs/p2p-protocol.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs: calibrate post-audit guarantees"
```

Repeat Step 3 after the commit. When all gates pass, record this exact HEAD/version as `subjectCommit`; after this point only the evidence-ref allowlist may change. Any broad doc, product, checker or workflow edit creates a new subject and invalidates all candidate evidence.

### Task 10: Execute Apple Silicon N8 Evidence and Optionally Publish the Beta

**Files:** Follow Tasks 2–6 of `docs/superpowers/plans/2026-07-14-real-device-release-certification.md`; after freeze only `README.md`, `docs/development/{quality-matrix.json,real-device-certification.md,release-claim.json}` and regular files under `docs/development/evidence/**` may differ from subject.

- [ ] **Step 1: Build one immutable RC from the frozen subject**

Create/push the ruleset-protected immutable `subjectTag`, verify it peels to `subjectCommit`, then dispatch RC workflow with `ref=<subjectTag>` plus the full SHA input. Verify `github.sha/head_sha`, exact `macos-aarch64` production/harness inventory, live retention and production certification-marker scan. Do not build Windows、Linux 或 Intel Mac assets. Candidate assets and non-releasable updater harness artifacts remain private.

- [ ] **Step 2: Execute the two required Apple Silicon L3 executions**

On the current Mac, run `L3-MACOS-GUI-PERMISSIONS-001@macos-aarch64` and `L3-MACOS-VOICEOVER-001@macos-aarch64` against the same subject/run and exact package SHA tuples. Write execution PASS/FAIL and aggregate PASS/FAIL/PARTIAL only after real execution. Intel Mac、Windows/WSL、Ubuntu、dual-host、iOS/Android and NVDA remain canonical `NOT VERIFIED` without placeholder manifests.

- [ ] **Step 3: Send every discovered defect back to its owning plan**

After any product/checker/workflow fix, return to Task 9, create a new subject/RC and rerun both current required executions. Never edit commit/run fields or FAIL into PASS without execution.

- [ ] **Step 4: Run go/no-go and commit only allowlisted claim/evidence files**

Run N8 checker against fixed `macos-aarch64-beta`. Update release claim, README evidence wording, certification document/matrix and the two evidence directories only. Commit them, resolve the evidence ref once to `expectedEvidenceCommit`, rerun the checker with that SHA, and freeze the exact subject tag/commit, RC, evidence ref/commit, profile and Apple Silicon artifact publish bundle; do not publish yet.

- [ ] **Step 5: Run the final non-mutating gates**

Run the evidence checker self-test/live command, docs self-test/live command and repository status/path-allowlist verification. Also verify the L0–L2 gate outputs recorded in Task 9 still correspond to `subjectCommit`; do not rerun a build that changes bytes. Any required edit returns to Step 3/new candidate.

- [ ] **Step 6: Stop after certification or dispatch the authorized beta as the final irreversible action**

If the requested outcome is certification only, return the frozen GO/NO-GO bundle and do not publish. If beta publication is explicitly in scope, dispatch the separate `.github/workflows/release-tauri-beta.yml` through Actions API with `ref=<subjectTag>` plus the exact frozen subject SHA, RC run, evidence ref, `expectedEvidenceCommit`, `platform-beta` and `macos-aarch64-beta`. The first job must assert `github.sha == inputs.subjectCommit`, tag peel equality and `resolve(evidenceRef)==expectedEvidenceCommit`; only then may it publish Apple Silicon `releasable=true` RC bytes and provenance to a new prerelease. Any stable metadata, extra-platform asset, existing target tag/release, force-move or asset overwrite is fatal；现有 stable `release-tauri.yml` 不在本里程碑调用，发布后不得再修改代码、文档或证据。

## Rollback and Failure Containment

- 任一 N1–N7 track 失败时停在当前 wave，回退该 track 的独立 commit/branch，不用后续 track 补偿或掩盖。
- 已创建的 additive schema/历史表保留数据；回退代码只停止读写新能力，不自动删表或覆盖用户内容。
- N8 失败只降低发布宣称并回到 owning track；不得修改证据结果来通过门禁。

## Completion Contract

- N1–N7 completion contracts pass on the integrated branch.
- N8 evidence honestly supports or blocks only `macos-aarch64-beta`; deferred platforms remain explicit `NOT VERIFIED`.
- every master coverage row has test/evidence ownership.
- user-owned old roadmap docs remain untouched and no global invariant regresses.

## Plan Self-Review

- Spec coverage: all eight child specs have ordered execution and integration gates.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: umbrella introduces no production interfaces.
