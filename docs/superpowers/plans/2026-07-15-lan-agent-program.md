# cc-partner LAN Agent Program Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 以正确依赖顺序交付 A1–A9，同时保持固定无鉴权 LAN 边界、自动收敛原则和可验证的降级路径。

**Architecture:** 本计划只负责跨子项目波次、共享门禁和旧规格处置；每个领域的函数级实现位于同名子计划。Wave 内只并行不共享写集的工作，后续波次只能消费已经合并并通过验证的接口。

**Tech Stack:** Rust 2021, Tauri 2, axum, SQLite/sqlx, React 19, TypeScript, Vitest, Playwright, managed Chromium, GitHub Actions.

## Global Constraints

- 正常路径必须自动感知、自动执行、自动收敛；只有歧义、失败或必须决策时打扰用户。
- 不实现面向用户的 Diff/批注/Rework 审查闭环。
- 不实现全局 Quick Open、Command Recipe 或命令模板。
- LAN business API 继续无调用者身份校验；不新增账号、配对、token、权限矩阵或可切换 LAN 模式。
- owning device 是 project/worktree/terminal/Agent/experiment/browser/task evidence 唯一权威。
- capability 只做协议协商，不表达权限或设备信任。
- schema 仅做 additive migration；活动 non-Claude Runner/experiment 降级前必须 quiesce。
- Prompt、回复、terminal bytes、transcript path、env、credential、cookie/profile 不进入 Fleet/Ledger/通知/P2P event。

---

## File Structure

- Specs: `docs/superpowers/specs/2026-07-15-*-design.md`。
- Plans: `docs/superpowers/plans/2026-07-15-*.md`。
- Supersession targets: `docs/superpowers/{specs,plans}/2026-07-14-orchestrator-review-workflow-and-notifications*`。
- Parent roadmap targets: `docs/superpowers/{specs,plans}/2026-07-14-post-audit-improvement-program*`。
- Persistent behavior: `docs/prd.md`。
- Protocol inventory: `docs/p2p-protocol.md`, `scripts/check-p2p-route-inventory.mjs`。
- Evidence matrix: `docs/development/quality-matrix.json`。

## Dependency Graph

```text
T1 → (T2 | T3)
T2 → T4
(T3 | T4) → T5 → T6 → T7

T2: A1
T3: A5 | A8
T4: A2 | A3
T5: A4 | A6 | A9
T6: A7
T7: full integration/certification
```

### Task 1: Freeze Scope and Supersede the Conflicting N6 Execution Unit

**Files:**
- Modify: `docs/superpowers/specs/2026-07-14-orchestrator-review-workflow-and-notifications-design.md`
- Modify: `docs/superpowers/plans/2026-07-14-orchestrator-review-workflow-and-notifications.md`
- Modify: `docs/superpowers/specs/2026-07-14-post-audit-improvement-program-design.md`
- Modify: `docs/superpowers/plans/2026-07-14-post-audit-improvement-program.md`

**Interfaces:**
- Consumes: A0 scope decisions.
- Produces: one unambiguous execution entry point with Review Diff tasks disabled and notifications re-owned by A2.

- [ ] **Step 1: Add explicit supersession metadata**

Add this block below each old document title:

```markdown
> **执行状态（2026-07-15）：已被部分取代。** Review Diff、review digest、Changes UI、mobile diff、Diff→Rework E2E 与 Deliver 人工确认门已取消；通知合同迁移到 Agent State Projection；WORKFLOW.md 向导保留为历史设计但不在当前 LAN Agent Program 实施。
```

- [ ] **Step 2: Remove N6 as an executable dependency**

Change the parent roadmap N6 row and Task 7 to `superseded` and link A0/A2; do not delete historical text or mark WORKFLOW as P1-4.

- [ ] **Step 3: Run document facts checks**

Run: `node scripts/check-docs.mjs && rg -n "Execute bounded diff|review diffs before delivery" docs/superpowers`

Expected: docs check exits 0; remaining matches only occur inside explicitly superseded historical sections.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-07-14-orchestrator-review-workflow-and-notifications-design.md docs/superpowers/plans/2026-07-14-orchestrator-review-workflow-and-notifications.md docs/superpowers/specs/2026-07-14-post-audit-improvement-program-design.md docs/superpowers/plans/2026-07-14-post-audit-improvement-program.md
git commit -m "docs: supersede human review diff program"
```

### Task 2: Execute A1 Agent Session Runtime

**Files:** Follow `docs/superpowers/plans/2026-07-15-agent-session-runtime.md`.

**Interfaces:**
- Consumes: existing sidecar event bus, Workbench terminal/session repo.
- Produces: `AgentSessionRuntime`, owner snapshot/event, `workbench.agent-runtime.v1`.

- [ ] **Step 1: Execute all A1 tasks in order**

Run each RED/GREEN command and commit from the A1 plan; do not begin A2/A3 integration until the mixed-version and Gap tests pass.

- [ ] **Step 2: Run A1 completion gate**

Run: `cd src-tauri && cargo test --locked workbench::agent_runtime && cargo test --locked orchestrator::agent_runtime_bridge && cargo test --locked net::routes::workbench`

Expected: all exit 0; OSC frames are absent from terminal replay/UI fixtures.

### Task 3: Execute Independent Browser Verification and Safe Restore Foundations

**Files:**
- Follow: `docs/superpowers/plans/2026-07-15-browser-verification-surface.md`
- Follow: `docs/superpowers/plans/2026-07-15-workspace-safe-restore.md`

**Interfaces:**
- Produces: `BrowserVerificationService`, `WorkspaceRestorePlan` and their owner-only capabilities.

- [ ] **Step 1: Execute A5 without changing iframe sandbox**

Verify `allow-same-origin` is not introduced and all engine targets originate from live preview registry.

- [ ] **Step 2: Execute A8 with zero terminal writes**

Verify restore smoke records terminal write/session create/Agent spawn counts of zero.

- [ ] **Step 3: Run shared Workbench gate**

Run: `cd src-tauri && cargo test --locked workbench::browser_verification && cargo test --locked workbench::workspace_restore && cd ../web && npm run build && npm test -- workbenchBrowser workspaceRestore`

Expected: all exit 0.

### Task 4: Execute A2 Projection and A3 Adapter Platform

**Files:**
- Follow: `docs/superpowers/plans/2026-07-15-agent-state-projection.md`
- Follow: `docs/superpowers/plans/2026-07-15-agent-adapter-platform.md`

**Interfaces:**
- Consumes: A1 runtime.
- Produces: low-noise Desktop/Mobile/Attention projection and provider-neutral Runner policy.

- [ ] **Step 1: Implement A2 against A1 snapshot/event only**

Reject any frontend implementation that reads Orchestrator legacy Claude runtime fields as Agent truth.

- [ ] **Step 2: Implement A3 Claude parity before enabling Codex/generic**

Run Claude characterization tests before provider parser expansion; snapshot attempt policy before launch.

- [ ] **Step 3: Run combined gate**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_adapter && cargo test --locked attention:: && cd ../web && npm run check:i18n && npm test -- AgentRuntime Attention && npm run build`

Expected: all exit 0; completed state does not create default OS notification.

### Task 5: Execute A4 Experiments, A6 Fleet and A9 Ledger

**Files:**
- Follow: `docs/superpowers/plans/2026-07-15-automated-candidate-experiments.md`
- Follow: `docs/superpowers/plans/2026-07-15-lan-agent-fleet.md`
- Follow: `docs/superpowers/plans/2026-07-15-agent-metadata-ledger.md`

**Interfaces:**
- Produces: unique-winner experiment delivery, owner-batched Fleet and metadata-only history.

**Cross-plan ordering:** A4可与A6并行；A9 Task 1–4可并行开发，但A9 Task 5–7必须在A6 owner collector、frontend schema/hook和Fleet view合并后执行，避免共享写集冲突。

- [ ] **Step 1: Execute A4 with database-enforced unique winner**

Require partial unique index, winner CAS, delivery defense check and existing per-task delivery lock before enabling full-auto.

- [ ] **Step 2: Execute A6 without scheduler mutations**

Static review must find no task placement, repo copy or inline terminal/Git mutation in Fleet components.

- [ ] **Step 3: Execute A9 privacy scan**

Run: `rg -n "prompt|response|transcript_path|terminal_bytes|environment" src-tauri/src/workbench/agent_ledger* src-tauri/src/storage/agent_ledger*`

Expected: matches appear only in explicit redaction tests/comments, not DTO fields or SQL columns.

- [ ] **Step 4: Run combined gate**

Run: `cd src-tauri && cargo test --locked orchestrator::experiments && cargo test --locked workbench::lan_fleet && cargo test --locked workbench::agent_ledger && cd ../web && npm test -- Experiment Fleet AgentLedger && npm run build`

Expected: all exit 0.

### Task 6: Execute A7 Agent-first CLI

**Files:** Follow `docs/superpowers/plans/2026-07-15-agent-first-cli.md`.

**Interfaces:**
- Consumes: stable A1/A4/A5/A6 domain APIs.
- Produces: `cc-partner` binary and JSON/JSONL contract.

- [ ] **Step 1: Implement typed local control commands**

Do not read SQLite from CLI; use shared command/domain helpers behind control routes.

- [ ] **Step 2: Add explicit remote transport**

Require `--device id:<deviceId>`; never auto-select a peer.

- [ ] **Step 3: Run CLI gates**

Run: `cd src-tauri && cargo check --locked --bins && cargo test --locked agent_cli && cargo test --locked --test agent_cli_smoke -- --nocapture --test-threads=1`

Expected: all exit 0; non-replayable mutation fixture receives one request.

### Task 7: Run Program-wide Verification and Update Persistent Truth

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/backend-operations.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `AGENTS.md` only when component/directory contracts changed.

**Interfaces:**
- Consumes: merged A1–A9 implementation and evidence.
- Produces: fact-aligned PRD/protocol/quality claims.

- [ ] **Step 1: Run full Rust gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`

Expected: exit 0 with zero clippy warnings and zero failed tests.

- [ ] **Step 2: Run full frontend gates**

Run: `cd web && npm run lint && npm run build && npm test && npm run test:e2e`

Expected: exit 0 with zero failed tests.

- [ ] **Step 3: Run governance gates**

Run: `node scripts/check-p2p-route-inventory.mjs && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs`

Expected: all exit 0.

- [ ] **Step 4: Audit forbidden product regressions**

Run: `rg -n "orchestrator\.review-diff|expectedReviewDigest|Command Recipe|trusted device|authenticated device" src-tauri web docs/prd.md docs/p2p-protocol.md`

Expected: no active implementation/product contract match; historical superseded documents are outside this scan.

- [ ] **Step 5: Record L3 facts honestly**

Only mark managed Chromium, CLI packaging, tmux/PTY and dual-host LAN rows passed when their platform execution evidence exists; all others remain `NOT VERIFIED`.

- [ ] **Step 6: Commit persistent documentation**

```bash
git add docs/prd.md docs/p2p-protocol.md docs/development/quality-matrix.json docs/development/testing.md docs/development/backend-operations.md web/CLAUDE.md src-tauri/CLAUDE.md AGENTS.md
git commit -m "docs: record LAN agent program behavior"
```

## Completion Contract

- A1–A9 focused and program-wide gates pass with fresh evidence.
- Normal Agent/experiment/browser paths do not require user diff review.
- P2P mixed-version and unsupported behavior is explicit.
- Real-device claims do not exceed executed evidence.

## Plan Self-Review

- Spec coverage: every A0 requirement maps to one child plan or Task 1/7.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: child plan outputs match the dependency table.
