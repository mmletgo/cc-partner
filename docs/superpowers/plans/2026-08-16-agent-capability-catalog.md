# Agent 身份目录与 Grok/Gemini 全表面适配 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 `docs/superpowers/specs/2026-08-16-agent-capability-catalog-design.md`：统一身份目录，并把 Grok Build / Gemini CLI 接到全部现有 agent 适配面。

**Architecture:** Rust `agent_catalog` 为权威身份表；`AgentTarget` / `AgentProviderId` / session / history 都是可选投影。Runtime 继续用 `AgentAdapter`，Hub 继续用 `AssetAdapter`，补 `SessionHistory` / `UsageSource` / `HistoryCollector` / `HeadlessCompletion` 四个小合同。前端只读 catalog，禁止再写死三元组。

**Tech Stack:** Tauri 2 / Rust / React 19 / TypeScript / Vitest / cargo test

**Workspace:** `.worktrees/agent-capability-catalog` on `feat/agent-capability-catalog`

---

## File map

| 新建 | 职责 |
|------|------|
| `src-tauri/src/agent_catalog/mod.rs` | `AgentId`、投影、编译期表、查询 API |
| `web/src/lib/agentCatalog.ts` | 前端同源表 + 查询 helper |
| `src-tauri/src/workbench/session_history.rs` | SessionHistory trait + registry |
| `src-tauri/src/workbench/agent_runtime/usage_source.rs` | UsageSource trait（从 agent_usage 抽出） |
| `src-tauri/src/cc/sources/grok.rs` | Grok Prompt 历史采集 |
| `src-tauri/src/cc/sources/gemini.rs` | Gemini Prompt 历史采集 |
| `src-tauri/src/orchestrator/agent_adapter/grok_build.rs` | Grok Runtime |
| `src-tauri/src/orchestrator/agent_adapter/gemini_cli.rs` | Gemini Runtime |
| `src-tauri/src/agent_hub/targets/grok.rs` | Grok AssetAdapter |
| `src-tauri/src/agent_hub/targets/gemini.rs` | Gemini AssetAdapter |
| `src-tauri/src/workbench/auto_title_grok.rs` | Grok 自动标题 |
| `src-tauri/src/workbench/auto_title_gemini.rs` | Gemini 自动标题 |

| 修改（关键） | 职责 |
|--------------|------|
| `src-tauri/src/agent_hub/models.rs` | `AgentTarget` + Grok/Gemini |
| `src-tauri/src/orchestrator/agent_adapter/types.rs` | `AgentProviderId` + 两家 Visible |
| `src-tauri/src/workbench/agent_session_search.rs` | source 扩枚举，分发 SessionHistory |
| `src-tauri/src/cc/models.rs` + `collector.rs` + `sources/mod.rs` | history source + scan_once |
| `src-tauri/src/agent_hub/support/support-manifest.json` | 五 target |
| `web/src/lib/types/agentHub.ts` / `core.ts` / schemas | 五值 decoder |
| `web/src/components/domain/WorkbenchSessionSearch/*` | 搜索源来自 catalog |
| `web/src/pages/CcHistory/CcHistory.tsx` | 筛选来自 catalog |
| `web/src/pages/AgentHub/context/agentHubContext.ts` | AGENT_TARGETS 来自 catalog |
| `web/src/lib/agentAdapterPresentation.ts` | provider 标签 |
| `AGENTS.md` / `web/AGENTS.md` / `src-tauri/AGENTS.md` | 身份清单 |

---

### Task 1: Rust + 前端身份目录，扩展全部写死三家的类型

**Files:**
- Create: `src-tauri/src/agent_catalog/mod.rs`
- Modify: `src-tauri/src/lib.rs`（pub use）、`agent_hub/models.rs`、`orchestrator/agent_adapter/types.rs`、`workbench/agent_session_search.rs`（parse）、`cc/models.rs`、`web/src/lib/agentCatalog.ts`、`web/src/lib/types/agentHub.ts`、`web/src/lib/types/core.ts`、`web/src/lib/schemas/agentHub.ts`、`web/src/lib/schemas/orchestrator.ts`

- [ ] 写 `agent_catalog` 单测：五 AgentId、generic 无 Hub、未知 token 失败
- [ ] 实现 catalog 与枚举扩展；修编译期 exhaustive match
- [ ] 前端 catalog + decoder 扩五值
- [ ] `cargo test --locked --lib agent_catalog`；`cd web && npm test -- --run agentCatalog`
- [ ] Commit: `feat(catalog): 引入 Agent 身份目录并扩展五家 CLI 身份`

### Task 2: 抽出 SessionHistory / UsageSource / HistoryCollector，迁现有三家

**Files:** session_history.rs、agent_usage.rs、cc/sources、collector.rs、claude_sessions.rs、agent_session_search.rs

- [ ] 现有 Claude/Codex/OpenCode 搜索、用量、历史 characterization 继续绿
- [ ] Commit: `refactor(agent): 按能力抽出 SessionHistory/Usage/History 合同`

### Task 3: Grok Runtime + SessionHistory + Usage + 标题 + Prompt 历史

**Files:** grok_build.rs、session_history grok 实现、usage grok、auto_title_grok.rs、cc/sources/grok.rs

- [ ] Fixture：`summary.json` + `updates.jsonl` + `signals.json`
- [ ] 空 prompt 可 launch；resume Fresh + `grok --resume <uuid>`；completion Manual
- [ ] Commit: `feat(grok): 接入 Runtime/会话/用量/Prompt 历史`

### Task 4: Grok AssetAdapter + Hub + support-manifest

**Files:** `agent_hub/targets/grok.rs`、`targets/mod.rs`、scanner/scheduler match、support-manifest.json、cross_agent destinations

- [ ] common 不写 AGENTS.md；adapted/exclusive 只写 `.grok/rules/cc-partner.*.md`；不写 `~/.claude/**`
- [ ] Commit: `feat(grok): Hub AssetAdapter 与 rules 投影`

### Task 5: Gemini Runtime + SessionHistory + Usage + 标题 + Prompt 历史

**Files:** 对称于 Task 3

- [ ] hash 命中 + tmp 枚举回退
- [ ] Commit: `feat(gemini): 接入 Runtime/会话/用量/Prompt 历史`

### Task 6: Gemini AssetAdapter + Hub

- [ ] common 写 GEMINI.md；adapted/exclusive 单一落点；不写 AGENTS.md
- [ ] Commit: `feat(gemini): Hub AssetAdapter 与 GEMINI.md 投影`

### Task 7: 跨 Agent、Headless、前端 catalog 化、文档

- [ ] 目的地含 grok/gemini；Plugin residual；Grok common skip
- [ ] Prompt 优化可选 grok；gemini 不稳则不可选
- [ ] 搜索 tab / 历史筛选 / Hub 切换 / Token provider / Settings catalog 无字面量三元组
- [ ] 更新分层 AGENTS.md
- [ ] Commit: `feat(catalog): 前端与跨 Agent/优化器改读身份目录`

### Task 8: 回归

- [ ] `cd src-tauri && cargo test --locked --lib agent_hub -- --test-threads=1` 关键子集
- [ ] `cd web && npm run build`（含 tsc）
- [ ] 现有 Claude characterization 仍绿

---

## Spec coverage

| Spec 节 | Task |
|---------|------|
| §4 身份目录 / 硬编码清单 | 1, 7 |
| §5.1 Runtime | 3, 5 |
| §5.2 SessionHistory | 2, 3, 5 |
| §5.3 Usage / Token | 2, 3, 5, 7 |
| §5.4 Prompt 历史 | 2, 3, 5 |
| §5.5 Hub | 4, 6, 7 |
| §5.6 Headless | 7 |
| §5.7 编排器/Settings | 1, 7 |
| §6 支持矩阵 | 3–7 |
| §8 测试 | 各 task + 8 |
