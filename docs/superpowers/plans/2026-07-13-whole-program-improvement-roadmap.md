# Whole-Program Improvement Roadmap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 LAN 信任边界、产品正确性、后端事务、前端基础、后端规模和质量治理六个独立计划推进 cc-partner 全面改进，同时让每个波次都保持可验证、可发布、可回滚。

**Architecture:** 本计划只负责编排、依赖门禁和跨计划验收，不复制六份领域计划的代码步骤。每个领域使用自己的 spec、implementation plan、worktree 和提交序列；合法 LAN peer 的业务请求始终无身份鉴权并允许全部读取、写入与执行，S1 只约束 peer/browser/stop/resource/risk/cross-platform 边界。

**Tech Stack:** Git worktrees, GitHub Actions, React 19, TypeScript 6, Vite 8, Vitest 4, Playwright 1.60, Tauri 2, Rust 2021, axum 0.7, sqlx/SQLite, platform smoke tests.

## Global Constraints

- 开始任何实现前读取根 `AGENTS.md`、目标目录分层指令和对应子项目 spec/plan。
- 本计划不授权把六个子项目合并成一个 PR；每个子计划独立 worktree、分支、验证、提交和回滚。
- 跨计划顺序固定为：`(S1 | S2 | S3) → (S4 | S5) → S6`；同一波内只并行边界清晰的任务。
- 新增数据库结构必须兼容旧版本读取并提供回滚说明；非数据库代码不为已废弃行为做无限期兼容。
- 任何阶段都不得破坏 desktop invoke 与 mobile/P2P HTTP 的既有边界、xterm DOM 常驻、P2P error envelope、request ID、capability gate、Doctor 隐私或 transfer durable finalize。
- 每个任务遵循 TDD；提交前运行对应子计划的 focused tests，波次结束运行集成门禁。
- P2P、Workbench 与 `/mobile` 不得新增配对、签名、Bearer、cookie session、逐设备身份或业务权限；Origin/Host 与 route gate 只用于 peer 范围、浏览器跨站、资源和生命周期边界。
- 不在计划执行中自动修改系统防火墙、上传日志、记录 Prompt/文件内容或输出本机 lifecycle control token。

---

## Task Dependency Graph

```text
T1 Baseline
  ├─ T2 S1 fixed unauthenticated LAN boundary (6 child tasks)
  ├─ T3 S2 core integrity
  └─ T4 S3 transactional runtime

(T2 + T3 + T4)
  → (T5 S4 frontend foundation | T6 S5 backend scale)
  → T7 S6 quality/governance/certification
  → T8 final program verification
```

T2、T3、T4 可并行；T5 消费 S2/S3 已稳定的前端与 runtime 行为，T6 消费 S1 的 route/browser/resource 合同，二者可并行；T7 统一收口 E2E、schema、矩阵、模块治理和真机证据。

## File Structure

- Read: `docs/superpowers/specs/2026-07-13-whole-program-improvement-roadmap-design.md`。
- Execute: 六份 `docs/superpowers/plans/2026-07-13-*.md` 子计划。
- Modify per subplan: `docs/prd.md`、`web/CLAUDE.md`、`src-tauri/CLAUDE.md`、`docs/development/testing.md`、`docs/p2p-protocol.md`。
- Do not create: 额外的任务总结 Markdown；每个子计划自己的 commit history 与权威文档已经构成实施证据。

## Interfaces

- Consumes: 六份领域 spec 的完成标准与六份 implementation plan 的任务结果。
- Produces: 波次级 go/no-go 决策、跨计划回归证据、固定 LAN 边界验收和最终文档事实校准。
- Does not produce: 业务 API、数据库表或 UI 组件；这些全部由子计划定义。

### Task 1: Freeze the Baseline and Create the Program Branch Map

**Files:**
- Read: `docs/superpowers/specs/2026-07-13-*-design.md`
- Read: `docs/superpowers/plans/2026-07-13-*.md`
- Read: `docs/development/testing.md`

- [ ] **Step 1: Verify a clean, evidence-backed baseline**

Run:

```bash
git status --short
cd web && npm run lint && npm run build && npm test && npm run test:e2e
cd ../src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked
cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-docs.mjs
```

Expected: worktree cleanliness is understood; all quality commands exit 0; Vite may still report the known pre-S4 chunk warning, which is captured as baseline rather than ignored.

- [ ] **Step 2: Record dependency ownership in the implementation issue or PR stack**

Create six implementation tracks named `S1-lan-boundary`, `S2-core-integrity`, `S3-transactional-runtime`, `S4-frontend-foundation`, `S5-backend-scale`, and `S6-quality-governance`. Record allowed file sets from each child plan; shared documentation changes land in the owning plan, not in a catch-all branch.

- [ ] **Step 3: Commit only if a persistent branch-map file was explicitly requested**

No repository file is created by default. If the project tracker requires a durable branch map, update its existing issue/board rather than creating a new Markdown summary.

### Task 2: Establish the Fixed Unauthenticated LAN Boundary

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-lan-trust-boundary-hardening.md` after it is revised to six tasks.

- [ ] **Step 1: Execute the six child-task boundaries without duplicating their implementation steps**

The six results are: socket peer + backend stop isolation, Host/Origin/WebSocket guard, existing resource-limit regression, fixed risk surfaces, protocol/docs alignment, and cross-platform/real-device evidence. Every legal LAN business read/write/execute request remains credential-free and allowed.

- [ ] **Step 2: Run the S1 completion gate and merge**

Require peer spoof/public rejection, browser hostile-request rejection, loopback + control-file token stop protection, bounded request resources, truthful UI/Doctor risk text, and cross-platform evidence. Legal LAN business requests must remain credential-free and fully available.

### Task 3: Repair Core Product Integrity

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-core-product-integrity.md`.

- [ ] **Step 1: Execute the complete S2 child plan**

Deliver Transfer, Scratchpad, Prompt, stale-response, Settings, Permissions and polling contracts using the child plan's focused tests and ownership boundaries.

- [ ] **Step 2: Run the S2 integration gate and merge**

Expected: focused unit/E2E, lint/build and affected Rust command tests pass; persistent behavior docs reflect only delivered behavior.

### Task 4: Make Backend Runtime State Transactional

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-backend-transactional-runtime.md`.

- [ ] **Step 1: Execute the complete S3 child plan**

Deliver Cloud Sync single-flight, atomic config, hotkey compensation, Updater generation and Health validation. LAN exposure is not a configuration writer or migration consumer.

- [ ] **Step 2: Run the S3 integration gate and merge**

Run Rust quality, transactional smoke and affected frontend tests; inspect diagnostics for secret/content leakage.

### Task 5: Execute Frontend Foundation, UX and Performance Work

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-frontend-foundation-ux-performance.md`.

- [ ] **Step 1: Confirm S2/S3 consumption points**

Start behavior-sensitive Settings, Transfer and runtime DTO work only after the corresponding S2/S3 contracts have landed; independent token, modal, split and terminal tasks may follow the child plan's own safe order.

- [ ] **Step 2: Execute S4 and run its full gate**

Deliver token, Dialog/Drawer, keyboard, error isolation, lazy boundaries, terminal buffer, IA/i18n and bounded frontend decomposition without duplicating S6 governance work.

### Task 6: Execute Backend Scale and Observability Work

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-backend-scale-observability.md`.

- [ ] **Step 1: Confirm S1 consumption points**

Add CC History routes and protocol metadata only after S1's route/browser/resource inventory is stable; preserve the fixed rule that legal LAN business calls are fully allowed without credentials.

- [ ] **Step 2: Execute S5 and run its full gate**

Deliver bounded Orchestrator claim, paged/batched CC History, local metrics and evidence-based pool decision; verify route inventory and mixed-version convergence.

### Task 7: Complete Quality Governance and Certification

**Files:**
- Follow: `docs/superpowers/plans/2026-07-13-quality-architecture-governance.md` after all behavior plans are stable.

- [ ] **Step 1: Close coverage and schema gaps without reimplementing child-plan tests**

Reuse S1–S5 evidence in the deterministic harness, runtime schema, quality matrix and traceability records. Add only missing E2E/L2/L3 coverage; fixed LAN evidence covers unauthenticated full business access plus peer/browser/stop/resource boundaries.

- [ ] **Step 2: Complete bounded governance and real-device evidence**

Perform only the remaining non-duplicated module extraction and certification work. Keep unavailable GUI/permission/WSL/multi-host cases explicitly `NOT VERIFIED`.

### Task 8: Run the Final Program Verification

**Files:**
- Modify only if facts changed: `README.md`, `docs/prd.md`, `docs/development/testing.md`, `docs/development/backend-operations.md`, `docs/p2p-protocol.md`, `AGENTS.md`, `web/CLAUDE.md`, `src-tauri/CLAUDE.md`

- [ ] **Step 1: Run repository-wide automated gates**

```bash
cd web
npm run lint
npm run build
npm test
npm run test:e2e

cd ../src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo check --locked --bins

cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
```

Expected: all commands exit 0; bundle/token/DTO budgets run as part of their owning scripts; no known-warning suppression hides failures.

- [ ] **Step 2: Run platform and multi-device verification**

Execute the S6 matrix for packaged macOS, Windows and Linux GUI, permissions, WSL/tmux, two-host mDNS/P2P, mobile Safari/Chrome, credential-free LAN read/write/execute and peer/browser/stop/resource boundaries. Record unavailable environments as NOT VERIFIED rather than pass.

- [ ] **Step 3: Audit completion against the master coverage matrix**

For every row in the master spec, link one passing test, smoke result or manual evidence. Any row without evidence keeps its child plan open; do not mark the program complete based only on code merge.

- [ ] **Step 4: Commit final fact calibration only when needed**

```bash
git add README.md docs AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs: calibrate whole-program guarantees"
```

Do not create a separate completion-summary Markdown file.

## Plan Self-Review

- Spec coverage: all six child specs are represented by an execution task and the master audit matrix.
- Completion detail: this orchestration plan contains no unresolved placeholders or deferred implementation instructions; code details intentionally live only in the six child plans.
- Type consistency: the master plan introduces no production interfaces and therefore cannot diverge from child-plan signatures.
- Scope: each code change belongs to exactly one child plan; this document only coordinates dependencies and gates.
