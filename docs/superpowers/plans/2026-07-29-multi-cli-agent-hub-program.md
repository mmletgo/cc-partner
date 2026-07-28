# Multi-CLI Agent Hub Program Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 以四个可独立验收的交付门，为 Claude Code、Codex CLI 与 OpenCode 建立同机自动收敛、源侧 LAN push、Git 完整备份和可见 Agent Runtime 支持。

**Architecture:** cc-partner sidecar owner 持有 Canonical Hub、Revision DAG、CAS、watch/reconcile/projection 和跨设备复制；三个 CLI 只通过版本化 adapter 暴露原生可编辑投影。Program 计划只冻结 gate 顺序、共享接口与整体验收，函数级 RED/GREEN 步骤位于四份子计划。

**Tech Stack:** Rust 2021、Tauri 2、tokio、notify、SQLite/sqlx、axum、reqwest、Git CLI、React 19、TypeScript、Vitest、Playwright。

## Global Constraints

- Canonical Hub 是唯一逻辑真相源；CLI 文件是可编辑投影。
- 同机 watcher/projector 只在 `HeadlessOwner` 运行，GUI 不成为第二 writer。
- 项目首次写入前必须 exact preview + 一次性 opt-in；cc-partner 永不对项目执行 `git add`、commit 或 push。
- 项目 opt-in 覆盖主 checkout 与所有当前/未来由 Workbench 登记的 worktree；外部 worktree 不写入。
- 子目录单来源普通正文默认 shared；用户级和项目根单来源默认来源 targetOnly。
- OpenCode 子目录 `AGENTS.md` 必须列出从项目根到父目录的明确相对规则路径；不得复制祖先正文。
- Claude/Codex 受管 Skill 只能通过各自隔离 Plugin 投影；不得把受管终态写进 OpenCode 也会扫描的 `.claude/skills` 或 `.agents/skills`。
- MCP/headers/env/URL credential 按原文进入 Hub、LAN、Git 和目标配置；日志继续脱敏。
- 本版本产生的旧 Claude asset/CLAUDE.md 兼容 payload 也发送原值；只对旧 peer 输入的历史 placeholder 做有损兼容识别，绝不生成新 placeholder。
- LAN 只提供源设备选择目标后的手动 push；不新增身份鉴权或目标侧任意 pull UI。
- Git 自动 push 只写本 device lane；任何远端 lane 导入必须 preview/confirm。
- SnapshotEnvelope 固定 `formatVersion=1`；selection ≤100,000 entries、≤2 GiB、单 blob ≤512 MiB、manifest ≤32 MiB、LAN chunk ≤8 MiB。
- watcher 使用 500 ms trailing debounce；变更目录 30 秒 rescan、全 scope 10 分钟 rescan；不同资产 projection 全局并行上限 4。
- 适配器没有 exact `minTestedVersion`、`currentTestedVersion` 和真实 CLI evidence 时写能力必须 blocked。
- schema migration 只追加；N/N+1 保留旧表 dual-write 和旧 P2P 路由，最早 N+2 删除兼容写入。
- 新增/修改 Rust 与 TypeScript 代码遵守根 `AGENTS.md` 的中文 docstring、UTF-8、strict 类型和 Hooks-before-early-return 合同。

---

## File Structure

- Design authority: `docs/superpowers/specs/2026-07-29-multi-cli-agent-hub-design.md`
- Gate A: `docs/superpowers/plans/2026-07-29-agent-hub-gate-a-foundation-instructions.md`
- Gate B: `docs/superpowers/plans/2026-07-29-agent-hub-gate-b-portable-assets.md`
- Gate C: `docs/superpowers/plans/2026-07-29-agent-hub-gate-c-replication-backup.md`
- Gate D: `docs/superpowers/plans/2026-07-29-agent-hub-gate-d-plugin-runtime.md`
- Rust domain root: `src-tauri/src/agent_hub/`
- Rust persistence: `src-tauri/src/storage/agent_hub_repo.rs`
- Desktop IPC/control: `src-tauri/src/commands/agent_hub.rs`, `src-tauri/src/backend/control_agent_hub.rs`
- P2P: `src-tauri/src/net/routes/agent_hub.rs`
- Frontend domain: `web/src/pages/AgentHub/`, `web/src/api/agentHub.ts`, `web/src/lib/{types,schemas}/agentHub.ts`
- Persistent product/protocol/evidence: `docs/prd.md`, `docs/p2p-protocol.md`, `docs/development/{testing.md,quality-matrix.json}`

## Dependency Graph

```text
Gate A: Hub + Instructions
  └── Gate B: Portable Assets + Physical Isolation
        └── Gate C: LAN/Git Replication
              └── Gate D: Plugin Decomposition + OpenCode Runtime
                    └── Program-wide certification
```

Gate 内只并行不共享写集的任务。任何 gate 只有在自己的数据库 migration、crash recovery、真实 CLI support manifest 和文档 evidence 全部通过后，才能成为下一 gate 的 migration base。

## Design Coverage Matrix

| 设计章节 | 实施落点 |
|---|---|
| §1–§5 问题、边界、总体架构 | 本 Program 的 Global Constraints、Dependency Graph；Gate A Task 1/7/8 |
| §6–§7 Canonical 模型、SQLite、CAS、Snapshot | Gate A Task 1/2；Gate C Task 1–3 |
| §8 Instruction Compiler | Gate A Task 3/4/5/6 |
| §9.0–§9.4 Skill/Command/Agent/MCP 与物理隔离 | Gate B Task 1–7 |
| §9.5 Plugin/Hook/residual 与引用所有权 | Gate D Task 1–3/7 |
| §9.6–§9.7 target 状态与 adapter 支持合同 | Gate B Task 4/7；Gate D Task 3/5 |
| §10 sidecar、去环、文件/DB 提交边界 | Gate A Task 6–8 |
| §11 project opt-in、worktree、跨设备映射 | Gate A Task 5；Gate C Task 3/7 |
| §12 LAN source push | Gate C Task 4/5/7/8 |
| §13 Git device lane 与确认导入 | Gate C Task 6–8 |
| §14 Agent CLI runtime | Gate D Task 4–6/8 |
| §15 用户表面 | Gate A Task 9；Gate B Task 8；Gate C Task 7；Gate D Task 6 |
| §16 错误处理与恢复 | 各 gate 的 fault-injection、Attention、certification task |
| §17 迁移、混合版本、回滚 | Gate A Task 10；Gate B Task 6/8；Gate C Task 8；Gate D Task 7 |
| §18–§20 交付门、验收、持久文档 | 本 Program Task 1–5 与每份 gate 的最终 certification task |
| §21 官方行为依据 | Gate B Task 4；Gate D Task 4/5/8 |

### Task 1: Execute Gate A — Hub Foundation + Instructions

**Files:**
- Follow: `docs/superpowers/plans/2026-07-29-agent-hub-gate-a-foundation-instructions.md`

**Interfaces:**
- Consumes: existing `AppState`, `DatabaseMaintenanceGate`, Workbench project/worktree repos, backend owner lifecycle, Attention aggregator.
- Produces: `AgentHubService`, Revision DAG/CAS, `InstructionCompiler`, durable projection jobs, project/checkout opt-in, minimal Agent Hub UI.

- [ ] **Step 1: Execute Gate A tasks in order**

Use the gate plan’s test-first order. Do not add Skill/MCP/Plugin projection while instruction projection and crash recovery are still feature-flagged or failing.

- [ ] **Step 2: Run Gate A completion command**

```bash
cd src-tauri
cargo test --locked agent_hub
cargo test --locked backend::runtime::tests
cd ../web
npm test -- AgentHub attention typeBarrel localeParity
npm run check:i18n
npm run build
```

Expected: all commands exit 0; a project without opt-in has zero file writes; OpenCode nested fixture consumes root/intermediate/current facts.

- [ ] **Step 3: Record Gate A evidence**

```bash
node scripts/check-quality-traceability.mjs
git log -1 --oneline
```

Expected: quality matrix contains Gate A evidence and the current commit contains the verified Gate A implementation; this plan does not create or push release tags.

### Task 2: Execute Gate B — Portable Assets + Physical Isolation

**Files:**
- Follow: `docs/superpowers/plans/2026-07-29-agent-hub-gate-b-portable-assets.md`

**Interfaces:**
- Consumes: Gate A `AssetAdapter`, `TargetBinding`, CAS/tree manifests, projection scheduler and UI DTOs.
- Produces: canonical Skill/Command/Agent/MCP schemas, three target adapters, isolated managed packages, support manifests and unified target matrix.

- [ ] **Step 1: Execute Gate B tasks in order**

Keep legacy Claude assets as a compatibility façade until adoption preview, activation and collision tests pass.

- [ ] **Step 2: Run Gate B completion command**

```bash
cd src-tauri
cargo test --locked agent_hub::assets
cargo test --locked agent_hub::targets
cargo test --locked claude_code_assets
cd ../web
npm test -- AgentHub agentHub
npm run check:i18n
npm run build
```

Expected: each real CLI discovers exactly one shared Skill; a Claude/Codex targetOnly Skill is absent from OpenCode discovery; unmanaged config survives MCP patch round-trips.

- [ ] **Step 3: Record Gate B evidence**

```bash
node scripts/check-quality-traceability.mjs
git log -1 --oneline
```

Expected: support manifest contains exact tested CLI versions and evidence IDs.

### Task 3: Execute Gate C — Replication + Backup

**Files:**
- Follow: `docs/superpowers/plans/2026-07-29-agent-hub-gate-c-replication-backup.md`

**Interfaces:**
- Consumes: Gate A Revision DAG/CAS and Gate B target variants.
- Produces: SnapshotEnvelope v1, `agent-hub.v1`, source-side multi-target push, Git device lanes and confirmed import.

- [ ] **Step 1: Execute Gate C tasks in order**

Do not reuse the legacy inventory/bundle route as a success path. The new protocol must transfer revision parents, tombstones and object hashes.

- [ ] **Step 2: Run Gate C completion command**

```bash
cd src-tauri
cargo test --locked agent_hub::snapshot
cargo test --locked net::routes::agent_hub
cargo test --locked agent_hub::git
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1
node ../scripts/check-p2p-route-inventory.mjs
```

Expected: interrupted push resumes without half-commit; divergent heads merge or form a visible conflict; Git remote lane is never imported without confirmation.

- [ ] **Step 3: Record Gate C evidence**

```bash
node scripts/check-quality-traceability.mjs
git log -1 --oneline
```

Expected: `agent-hub.v1` capability and every registered route ship atomically.

### Task 4: Execute Gate D — Plugin Decomposition + Runtime

**Files:**
- Follow: `docs/superpowers/plans/2026-07-29-agent-hub-gate-d-plugin-runtime.md`

**Interfaces:**
- Consumes: Gate B package/adapter surface and Gate C snapshot lineage.
- Produces: Plugin/Hook decomposition, residual payload ownership, OpenCode runtime adapter and final unified migration surface.

- [ ] **Step 1: Execute Gate D tasks in order**

Keep OpenCode JS/TS runtime source-only outside OpenCode; only explicit Hook mappings may cross targets.

- [ ] **Step 2: Run Gate D completion command**

```bash
cd src-tauri
cargo test --locked agent_hub::plugins
cargo test --locked orchestrator::agent_adapter
cargo test --locked orchestrator::runner
cd ../web
npm test -- AgentHub AutomationSettings Workbench
npm run check:i18n
npm run build
```

Expected: component status is accurate; package deletion preserves shared refs; `openCodeVisible` launch/resume/completion/failed/manual-takeover fixtures pass.

- [ ] **Step 3: Record Gate D evidence**

```bash
node scripts/check-quality-traceability.mjs
git log -1 --oneline
```

Expected: no old Claude-only UI owns a watcher or synchronization state.

### Task 5: Run Program-Wide Certification

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `AGENTS.md` only when top-level directory/component inventory changed
- Test: `src-tauri/tests/agent_hub_replication_smoke.rs`
- Test: `web/tests/agent-hub.spec.ts`

**Interfaces:**
- Consumes: verified Gates A–D.
- Produces: release evidence and persistent product/protocol truth.

- [ ] **Step 1: Run focused Rust quality gates**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub
cargo test --locked orchestrator::agent_adapter
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1
```

Expected: all exit 0 with no credential fixture in stdout/stderr.

- [ ] **Step 2: Run focused frontend quality gates**

```bash
cd web
npm run lint
npm run check:tokens
npm run check:i18n
npm test -- AgentHub attention typeBarrel localeParity
npm run build
npm run check:bundle
npm run test:e2e -- agent-hub.spec.ts
```

Expected: all exit 0; E2E verifies project preview, target matrix, conflict navigation, source push selection and confirmed Git import.

- [ ] **Step 3: Run protocol and documentation checks**

```bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
git diff --check
```

Expected: all exit 0; L3 rows not actually executed remain `NOT VERIFIED`.

- [ ] **Step 4: Verify the project repository was never mutated by Agent Hub automation**

Run the Agent Hub worktree fixture and then:

```bash
git status --short
git diff --cached --exit-code
```

Expected: only intentional implementation/documentation changes are present; fixture project index and HEAD remain unchanged.

- [ ] **Step 5: Commit certification truth**

```bash
git add docs/prd.md docs/p2p-protocol.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md AGENTS.md
git commit -m "docs: certify multi-cli agent hub"
```

Expected: omit `AGENTS.md` from `git add` when its component/directory inventory did not change.
