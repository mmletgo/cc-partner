# README and Layered Documentation Calibration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在七项工程工作流与全局 Inbox 实现完成后，校准 README、PRD 和分层开发指令，使产品定位、命令、协议、CI、发布和平台范围与仓库事实一致。

**Architecture:** README 只承担用户入口与 local-first 产品叙事；PRD 记录已落地的持久产品行为；根 AGENTS 保留项目概览、顶级地图、组件清单和关键陷阱；前端与后端细节分别下沉 `web/CLAUDE.md`、`src-tauri/CLAUDE.md` 及两份短开发指南。新增静态链接/事实检查脚本，防止旧 runner、tauri-action 和动态端口说法回流。

**Tech Stack:** Markdown, Node.js 22 static validation, existing npm/cargo smoke commands, GitHub Actions/release workflow as source of truth.

## Global Constraints

- 本计划最后执行；任何未合并能力不得提前写成已支持。若某前置工作流延后，文档保留当前事实并把对应校准步骤一起延后。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md`、`web/CLAUDE.md`、`src-tauri/CLAUDE.md` 和实际 workflow/package/router/CLI 源码。
- README 不复制长 API 表或内部实现；开发指南不写任务时间线、提交摘要或历史复盘。
- PRD 只记录持久产品行为，不记录内部重构、测试迁移过程或计划状态。
- Hosted smoke 的未覆盖项必须原样明确，不能把 release build 或 unit test 写成 WSL/tmux/GUI/权限已验证。
- 文档里的命令必须来自锁定脚本/CLI help，至少完成静态或 smoke 验证；不得推荐 `npx --yes`。
- 仅移动/精简指令时必须保证约束没有丢失：先复制到正确下层并验证，再从根文件删除重复内容。
- 不生成任务总结 Markdown。

---

## Task Dependency Graph

最大并行 waves：`T1 → (T2 | T3 | T4 | T6) → T5 → T7`。事实清单是所有写作与 guard 的共同基线；README、PRD、分层指令和 guard 写集独立，开发指南待 README/分层职责稳定后编写，最终统一校验。

## File Structure

- Modify `README.md`: local-first Workbench-first positioning, current commands, architecture, release and limits。
- Modify `docs/prd.md`: Inbox、remote runtime、doctor 等已经落地的持久行为；删除过时 unsupported statements。
- Modify `AGENTS.md`: root scope calibration and links to layered instructions。
- Modify `web/CLAUDE.md`: Vitest/Playwright commands, Attention/provider/deep links, controller boundaries, remote runtime display cache。
- Modify `src-tauri/CLAUDE.md`: P2P v1/error/request ID/idempotency, owner runtime route, smoke scope, logging/doctor。
- Create `docs/development/testing.md`: concise quality gates and verified platform matrix。
- Create `docs/development/backend-operations.md`: backend CLI lifecycle, ports/firewall, logs and doctor。
- Create `scripts/check-docs.mjs`: relative-link, command/source-of-truth and banned-stale-claim checks。
- Create `.github/workflows/docs.yml`: docs-only and code-PR static documentation guard。

## Source-of-Truth Map

| Document claim | Code source to inspect |
| --- | --- |
| npm commands | `web/package.json`, `web/playwright.config.ts` |
| local IPC vs P2P boundary | `web/src/api/client.ts`, `src-tauri/src/net/http_server.rs` |
| preferred/actual port | backend config/http-server binding and `/api/health` |
| protocol/capabilities/errors | `src-tauri/src/net/protocol.rs`, health route, error_response |
| remote runtime | route/client/command and frontend hooks/stores |
| backend commands/exit codes | `src-tauri/src/backend/cli.rs`, `--help` |
| logs/doctor | `src-tauri/src/backend/logging.rs`, `doctor.rs` |
| daily/PR smoke | `.github/workflows/cross-platform-smoke.yml` |
| release mechanism | `.github/workflows/release-tauri.yml`, bump script |
| component inventory | `web/src/components/{primitives,layout,domain}` |

---

### Task 1: Build a Documentation Fact Inventory

**Files:**
- No writes until the inventory is recorded in the working notes for this task.

- [ ] **Step 1: Capture executable command sources**

Read `web/package.json`, backend CLI dispatch/help, `scripts/bump-version.mjs`, CI, cross-platform smoke and release workflows. Record exact command spelling, job names, triggers and exit semantics.

- [ ] **Step 2: Capture architecture/protocol sources**

Read the Tauri invoke client, HTTP router, health protocol metadata, capability list, runtime route and backend bind logic. Record preferred port 62116 with occupied-port increment and health's actual-port field.

- [ ] **Step 3: Capture current product behavior**

Verify desktop/mobile Inbox routes/nav, remote runtime four-state behavior, outbox Retry/Discard, backend logs/doctor, and Workbench controller ownership in code/tests. Any missing implementation remains absent from final prose.

- [ ] **Step 4: Locate stale claims**

```bash
rg -n "tauri-action|动态端口|npx --yes tsx|remote snapshot unavailable|remote snapshot unsupported|pending remote outbox.*不允许 action" README.md docs/prd.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md
```

Classify each match as stale, historical-but-useful, or still true. Historical facts belong only where they prevent a current engineering trap, not README.

---

### Task 2: Reframe README Around the Current Product

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Rewrite the opening hierarchy**

The first feature order is:

1. local-first multi-device Workbench
2. Mobile Workbench
3. Orchestrator automatic orchestration and visible execution
4. headless backend CLI
5. file transfer, screenshot, Prompt and scratchpad supporting tools

Describe cc-partner as a local-first multi-device project workbench, not primarily a transfer/screenshot utility.

- [ ] **Step 2: Add a concise start path**

Document desktop launch/development link and headless commands that actually exist:

```text
cc-partner-backend start
cc-partner-backend status
cc-partner-backend doctor
cc-partner-backend doctor --json
cc-partner-backend stop
```

State doctor exits 0/1/2 and `--json` stdout is machine-readable only after checking the implemented help/schema.

- [ ] **Step 3: Correct network and architecture wording**

State local frontend→Rust uses Tauri invoke without a frontend local API port; device/mobile P2P uses axum HTTP. TCP 62116 is the preferred port and increments when occupied; actual URL/port comes from app UI or `/api/health`. UDP 5353 remains mDNS. Preserve firewall commands as manual examples, never automatic actions.

- [ ] **Step 4: Correct platform/remote limits**

State native macOS/Linux tmux and Windows WSL dependency behavior as implemented, while making hosted CI exclusions explicit. Distinguish “supported product behavior” from “automated smoke verified”.

- [ ] **Step 5: Correct release mechanics**

Replace tauri-action claims with the real three-job flow: matrix native build with prepared backend sidecar, release publish, then independent `latest.json` assembly from signatures. Link to workflow rather than duplicating secrets/history.

- [ ] **Step 6: Verify README links and commands**

```bash
node scripts/check-docs.mjs README.md
cd web && npm run build
cd ../src-tauri && cargo check --locked --bins
```

Expected: links resolve, stale-claim rules pass, build/check exit 0.

- [ ] **Step 7: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add README.md
git commit -m "docs: reposition cc-partner around local-first workbench"
```

---

### Task 3: Calibrate PRD to Implemented Persistent Behavior

**Files:**
- Modify: `docs/prd.md`

- [ ] **Step 1: Add the global Inbox product contract**

Record real-time projection, no Inbox table/history/read/dismiss, exact v1 sources/exclusions, desktop fixed page, mobile second nav with Projects still default, live/cached semantics, navigation-only behavior and immediate post-action refresh.

- [ ] **Step 2: Update remote runtime behavior**

Replace hard unsupported wording with owner-device route, live/offline/unsupported/unavailable, process-only last-success display cache, cold-offline empty state and no local telemetry/cache consumption by execution.

- [ ] **Step 3: Update outbox and backend operations**

Record failed outbox Retry preserving `clientRequestId`, Discarded terminal/audit semantics, log rotation/privacy and doctor commands/status/exit codes. Do not describe internal controller extraction or Vitest migration as product behavior.

- [ ] **Step 4: Remove contradictions rather than append caveats**

Search the full PRD for older statements that pending remote outbox has no actions, remote runtime is always unsupported, or Inbox does not exist. Rewrite the authoritative paragraph; do not leave two conflicting sections.

- [ ] **Step 5: Validate against behavior tests**

Run focused Rust/Vitest suites named by the feature plans, then:

```bash
node scripts/check-docs.mjs docs/prd.md
rg -n "Inbox|待处理|runtime snapshot|remoteStatus|discarded|doctor" docs/prd.md
```

Review every match for current behavior.

- [ ] **Step 6: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add docs/prd.md
git commit -m "docs: align prd with attention and remote runtime"
```

---

### Task 4: Rebalance Root and Layered Instructions

**Files:**
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Preserve root-level responsibilities**

Keep project overview/stack, top-level directory map, component inventory, token/reuse/Hooks safety traps, main verification entrypoints and links to child instructions. Keep concise facts that every contributor needs before choosing a directory.

- [ ] **Step 2: Move frontend details before deleting duplicates**

Ensure `web/CLAUDE.md` contains:

- final `npm test`, watch, E2E and test-all commands; Node-default/jsdom-explicit policy
- Workbench controller ownership/bridges/≤1200-line contract
- Attention Provider refresh/stale/deep-link rules
- desktop/mobile runtime cache separation
- component/token/i18n/Hooks constraints

Remove the old per-file `npx --yes tsx` command list.

- [ ] **Step 3: Move backend details before deleting duplicates**

Ensure `src-tauri/CLAUDE.md` contains:

- protocol v1 authoritative health, bounded mDNS hints and one-generation compatibility
- error envelope/request ID and route idempotency checklist
- local-only remote runtime route/no recursion
- macOS/Windows smoke verified/unverified scope
- 5 MiB/3-history sanitized logs and doctor schema/exit rules
- release signing/build/latest-json mechanics

- [ ] **Step 4: Remove root duplication safely**

Move detailed Tauri command catalogs, P2P route catalogs, release implementation steps and domain-specific test lists from root after child documents contain them. Replace with short pointers. Do not remove the component list or critical port/updater/key trap summaries needed across directories.

- [ ] **Step 5: Validate instruction discoverability**

From root, web and src-tauri, verify a new agent can find the correct instruction file and command in at most one link hop. Search for contradictory duplicate commands/behavior.

- [ ] **Step 6: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md
git commit -m "docs: rebalance layered development instructions"
```

---

### Task 5: Add Focused Testing and Backend Operations Guides

**Files:**
- Create: `docs/development/testing.md`
- Create: `docs/development/backend-operations.md`
- Modify: `README.md`

- [ ] **Step 1: Write the testing guide**

Include one table for local command, CI job, trigger, verified scope and explicit exclusions. Cover frontend unit/E2E, Ubuntu full quality, macOS/Windows related-PR+daily smoke and release build separation.

- [ ] **Step 2: Write the backend operations guide**

Include lifecycle commands, preferred/actual port, firewall/mDNS checks, control/data/log locations with `<HOME>` notation, rotation policy, doctor human/JSON examples and exit status interpretation. Include no upload/telemetry behavior.

- [ ] **Step 3: Link without duplicating**

README's development/troubleshooting sections link to both guides. Child CLAUDE files link only where a human guide is useful; they retain engineering invariants locally.

- [ ] **Step 4: Validate links and code blocks**

```bash
node scripts/check-docs.mjs README.md docs/development/testing.md docs/development/backend-operations.md
```

Expected: all relative links/anchors/files resolve and fenced blocks balance.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add README.md docs/development
git commit -m "docs: add testing and backend operations guides"
```

---

### Task 6: Add an Automated Documentation Guard

**Files:**
- Create: `scripts/check-docs.mjs`
- Create: `.github/workflows/docs.yml`

- [ ] **Step 1: Write checker fixtures before implementation**

Create in-memory/temporary Markdown fixtures covering valid relative file link, valid same/cross-file heading anchor, missing file, missing heading anchor, ignored http/mailto link, unbalanced fence and banned stale phrases. Assert non-zero on each invalid fixture and readable file/line diagnostics.

- [ ] **Step 2: Implement the root Node checker**

With no new package dependency, recursively inspect passed Markdown files or the repository's tracked Markdown set. Check relative file links, same/cross-file anchors against GitHub-style heading slugs, balanced triple-backtick fences and these scoped stale claims:

- README may not contain `tauri-action` or call P2P “动态端口”.
- `web/CLAUDE.md` may not contain `npx --yes tsx`.
- docs may not call hosted smoke coverage of WSL+tmux, GUI/WebView, permissions or multi-host mDNS.
- README command names must occur in package scripts/CLI dispatch sources through explicit allowlist checks.

Do not reject historical spec/plan text under `docs/superpowers/**`; those are design records, not current user docs.

- [ ] **Step 3: Run the checker across current docs**

```bash
node scripts/check-docs.mjs README.md docs/prd.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md docs/development/testing.md docs/development/backend-operations.md
```

Expected: exit 0. Intentionally break one link and phrase, confirm exit 1, then revert the intentional changes.

- [ ] **Step 4: Add a lightweight CI docs job**

Create `docs.yml` with pull-request and master-push paths for `**/*.md`, `scripts/check-docs.mjs` and the workflow itself. Its single `docs` job checks out, sets up Node 22 and runs the command above. It must not install frontend/Rust dependencies and must not use `continue-on-error`.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add scripts/check-docs.mjs .github/workflows/docs.yml
git commit -m "ci: validate current documentation facts"
```

---

### Task 7: Final Cross-Document Verification

- [ ] **Step 1: Run all static checks**

```bash
node scripts/check-docs.mjs README.md docs/prd.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md docs/development/testing.md docs/development/backend-operations.md
rg -n "tauri-action|动态端口|npx --yes tsx" README.md web/CLAUDE.md
```

Expected: checker exits 0 and the stale-phrase search has no matches.

- [ ] **Step 2: Verify documented frontend commands**

```bash
cd web
npm ci
npm test
npm run lint
npm run build
npm run test:e2e
```

- [ ] **Step 3: Verify documented backend commands**

```bash
cd src-tauri
cargo check --locked --bins
cargo test --locked
cargo run --locked --bin cc-partner-backend -- status
set +e
cargo run --locked --bin cc-partner-backend -- doctor --json > /tmp/cc-partner-doctor-doc-check.json
doctor_exit=$?
set -e
jq -e . /tmp/cc-partner-doctor-doc-check.json
test "$doctor_exit" -ge 0 -a "$doctor_exit" -le 2
```

- [ ] **Step 4: Compare workflow prose with YAML**

Check exact job names, PR paths, daily cron, artifact behavior, release jobs and `latest.json` assembly. Search docs for every workflow name and correct any drift.

- [ ] **Step 5: Review product claims against implementation**

For Inbox, remote runtime, outbox actions and doctor, pair every PRD/README statement with at least one current source/test. Remove claims that cannot be demonstrated.

- [ ] **Step 6: Commit final corrections**

```bash
git -C "$(git rev-parse --show-toplevel)" add README.md docs AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md scripts/check-docs.mjs .github/workflows/docs.yml
git commit -m "docs: complete repository fact calibration"
```

## Completion Contract

- README leads with local-first Workbench/Mobile/Orchestrator/headless backend and accurately describes IPC/P2P/ports/release/platform limits.
- PRD has one non-contradictory account of implemented Inbox, remote runtime, outbox and doctor behavior.
- Root and child instructions are concise at the correct layer; no essential rule is lost during relocation.
- Stable npm/backend commands and CI scopes in docs execute as written.
- Static checks reject broken relative links, unbalanced fences and scoped stale claims, including docs-only PRs.
- No current documentation claims hosted WSL/tmux, GUI/WebView, permissions or multi-host mDNS coverage.
