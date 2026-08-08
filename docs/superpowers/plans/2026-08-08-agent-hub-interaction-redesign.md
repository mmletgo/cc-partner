# Agent Hub Interaction Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 [`2026-08-08-agent-hub-interaction-redesign-design.md`](../specs/2026-08-08-agent-hub-interaction-redesign-design.md) 交付 Agent Hub 新交互：Agent→范围→五 Tab 壳层、提示词三栏、发现即管理、用户级设备/项目级远端、LAN 拉推、同机跨 Agent 选择性与 Claude 全量适配（皆强制预览）。

**Architecture:** 前端以 **上下文三元组/四元组**（`agent × scope × device|project × tab`）替换五段 `section` 主导航；提示词与 portable 资产仍走既有 Tauri 命令，增量补「扫描后 ensure-managed」「跨设备上下文参数」「全量跨 Agent Claude 方案」后端；跨 Agent 从 Dialog 升级为独立路由页。Views 不 import `@/api/*`；controller 持有 sequence / stale / dirty draft。

**Tech Stack:** React 19、TypeScript strict、Vite、i18next、CSS Modules + `tokens.css`、Vitest、Playwright、Tauri 2 invoke、Rust agent_hub（inventory / projection / cross_agent）。

---

## Global Constraints

- 权威交互：`docs/superpowers/specs/2026-08-08-agent-hub-interaction-redesign-design.md`。
- 产品阶段边界：`docs/superpowers/specs/2026-08-08-agent-hub-three-phase-refactor-design.md`（D1–D4；无后台跨 Agent 自动写）。
- 写能力唯一门闩：`support-manifest` + `evaluate_target_support`；UI 不得在 blocked 时伪装可写。
- Hooks 全部在 early return 前；不新增页面级 `useWorkbenchController`；`AgentHub.tsx` 硬顶 1200 行（超限拆子 view）。
- 复用 primitives（Button/Card/Input/Pill/StatusMessage/Dialog/Drawer）；禁止 `window.confirm`、硬编码色、`!important`。
- 文案 zh/en key parity；LAN 文案禁止「已认证/可信/安全设备」。
- MCP **UI 不脱敏**（Spec §1）；tracing/日志仍避免无必要明文 secret。
- 发现即管理：**不提供**「停止管理并保留文件」；删除 Adopt 主路径。
- 提示词打开：**不**自动 parse 块；「从原始重新解析块」仅在原始栏。
- L3 真机未跑保持 `NOT VERIFIED`；新 E2E 用 mock/harness，不替代 L2。
- 不恢复 `/claude-code` 独立页。

---

## Prerequisites

| 依赖 | 说明 |
|------|------|
| Portable inventory 命令 | `web/src/api/portableInventory.ts` + 后端 inspect/preview/apply 已存在 |
| 用户级指令 V2 | `agent_hub_inspect_user_instruction_workspace` / preview / apply |
| Cross-agent 最小集 | `src-tauri/src/agent_hub/cross_agent.rs` + `agentHub` API preview/apply instruction |
| Devices 列表 | `web/src/api/devices.ts` mDNS peers |
| Workbench 远端项目 | 项目选择器复用既有 remote project 列表能力 |

若某写路径仍 scan-only：UI 必须 fail-closed，但**壳层与三栏草稿**仍可交付。

---

## File Structure

### Create

| Path | 职责 |
|------|------|
| `web/src/pages/AgentHub/context/agentHubContext.ts` | URL/context 纯函数：parse/write agent, scope, deviceId, projectKey, tab |
| `web/src/pages/AgentHub/context/agentHubContext.test.ts` | 上下文往返与 legacy section 映射 |
| `web/src/pages/AgentHub/shell/AgentHubShell.tsx` | 顶栏 Agent、工具栏、范围、设备/项目、五 Tab 壳 |
| `web/src/pages/AgentHub/shell/AgentHubShell.module.css` | 壳层布局 tokens |
| `web/src/pages/AgentHub/shell/AgentHubShell.test.tsx` | 壳层交互（无 api） |
| `web/src/pages/AgentHub/instructions/instructionThreePane.ts` | 块/预览/原始状态机 pure helpers |
| `web/src/pages/AgentHub/instructions/instructionThreePane.test.ts` | 初始空块、parse、双脏合流、同步基线 |
| `web/src/pages/AgentHub/instructions/InstructionThreePaneView.tsx` | 三栏 pure view |
| `web/src/pages/AgentHub/instructions/useInstructionThreePaneController.ts` | 单 agent×scope×device/project 提示词 controller |
| `web/src/pages/AgentHub/crossAgent/CrossAgentAdaptPage.tsx` | 阶段三独立页 |
| `web/src/pages/AgentHub/crossAgent/useCrossAgentAdaptController.ts` | 选择性 + 全量模式 state |
| `web/src/pages/AgentHub/crossAgent/crossAgentPresentation.ts` | 分类展示 pure |
| `src-tauri/src/agent_hub/inventory/ensure_managed.rs` | 扫描后 ensure-managed（或并入既有 reconcile） |
| `src-tauri/src/agent_hub/cross_agent_full.rs` | Claude 全量适配方案 DTO + preview 管道 |

### Modify (high-touch)

| Path | 职责 |
|------|------|
| `web/src/pages/AgentHub/AgentHub.tsx` | Composer：壳 + 当前 tab 内容；瘦身 |
| `web/src/pages/AgentHub/useAgentHubController.ts` | 上下文驱动；废弃 section 主路径或映射到 context |
| `web/src/pages/AgentHub/userInstructions/*` | 迁入三栏或由 `instructions/` 取代主路径 |
| `web/src/pages/AgentHub/portableAssets/*` | 去掉 adopt 主路径；management 状态；单 agent 过滤 |
| `web/src/api/agentHub.ts` / `portableInventory.ts` | 设备/项目上下文参数；全量跨 Agent API |
| `web/src/App.tsx` | `/agent-hub/adapt` 或 query `view=adapt` 路由 |
| `web/src/i18n/locales/{zh,en}/agentHub.json` | 新文案 |
| `src-tauri/src/commands/agent_hub.rs` | 新/扩展 command 注册 |
| `src-tauri/src/agent_hub/**` | ensure-managed、上下文、full adapt |
| `web/tests/*agent-hub*` | E2E 对齐新 IA |
| `docs/development/quality-matrix.json` | 新 E2E/L2 ID |
| `AGENTS.md` / `web/CLAUDE.md` | 组件与 controller 清单 |

### Deprecate / remove after parity

- 主路径依赖：`targetMatrix` 三列矩阵、五段 section nav 文案
- `AssetAdoptionDialog` 主入口（若仅 adopt）
- `CrossAgentSyncDialog` 作为唯一入口（保留可被页内复用或删除）

---

## Task Dependency Graph

```text
T1 context pure
  → T2 shell chrome
  → T3 instruction pure + view
  → T4 instruction controller wire
T1 → T5 ensure-managed backend
  → T6 portable UI adopt removal
T2 + T4 + T6 → T7 device + remote project context
  → T8 LAN pull/push toolbar
T4 + T7 → T9 cross-agent page selective
  → T10 cross-agent full Claude
T2..T10 → T11 deep link, i18n, E2E, docs
```

**Waves:** `[T1]` → `[T2,T3,T5]` → `[T4,T6]` → `[T7]` → `[T8,T9]` → `[T10]` → `[T11]`

T2/T3/T5 可 worktree 并行；合并前各自单测绿。

---

### Task 1: Agent Hub context URL model

**Files:**
- Create: `web/src/pages/AgentHub/context/agentHubContext.ts`
- Create: `web/src/pages/AgentHub/context/agentHubContext.test.ts`
- Modify: `web/src/pages/AgentHub/useAgentHubController.ts`（仅接入 parse/write，不改布局）

**Produces:**

```ts
export type AgentHubTab = 'instructions' | 'skill' | 'command' | 'mcp' | 'plugin';
export type AgentHubScope = 'user' | 'project';

export interface AgentHubContext {
  agent: AgentTarget;
  scope: AgentHubScope;
  /** user scope: null = local device */
  deviceId: string | null;
  /** project scope identity; null when scope=user */
  projectKey: string | null;
  tab: AgentHubTab;
  /** true when view=adapt cross-agent page */
  adaptView: boolean;
}

export function parseAgentHubContext(params: URLSearchParams): AgentHubContext;
export function writeAgentHubContext(
  params: URLSearchParams,
  ctx: AgentHubContext,
): URLSearchParams;
/** Map legacy section=userInstructions|assets|… into AgentHubContext defaults */
export function mapLegacySection(section: string | null): Partial<AgentHubContext>;
```

- [ ] **Step 1: Write failing tests** for defaults (`agent=claude`, `scope=user`, `deviceId=null`, `tab=instructions`), round-trip, legacy `section=assets&target=codex&kind=skill` → `agent=codex,tab=skill`, and `view=adapt` flag.

- [ ] **Step 2: Run RED**

```bash
cd web && npm test -- agentHubContext
```

Expected: FAIL (module missing).

- [ ] **Step 3: Implement pure parse/write/mapLegacy** — no React, no api.

- [ ] **Step 4: Run GREEN** — same command, PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/AgentHub/context/
git commit -m "feat(agent-hub): add URL context model for agent/scope/device/tab"
```

---

### Task 2: Shell chrome (Agent × scope × device/project × tabs)

**Files:**
- Create: `web/src/pages/AgentHub/shell/AgentHubShell.tsx`
- Create: `web/src/pages/AgentHub/shell/AgentHubShell.module.css`
- Create: `web/src/pages/AgentHub/shell/AgentHubShell.test.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.tsx`（用 Shell 包住内容 slot）
- Modify: `web/src/i18n/locales/zh/agentHub.json`, `en/agentHub.json`

**Shell props (pure):**

```tsx
export interface AgentHubShellProps {
  context: AgentHubContext;
  onContextChange: (patch: Partial<AgentHubContext>) => void;
  peers: Array<{ deviceId: string; name: string; online: boolean }>;
  projects: Array<{ key: string; label: string; remote: boolean }>;
  actions: {
    onPull: () => void;
    onPush: () => void;
    onAdapt: () => void;
    adaptDisabledReason?: string | null;
  };
  children: React.ReactNode;
}
```

规则：
- `scope===user'` 显示设备切换；`scope===project'` 显示项目选择、**隐藏**设备。
- peer 在线可点；离线 disabled。
- `deviceId !== null` 时 `onAdapt` disabled + reason（同机 only）。

- [ ] **Step 1: Failing view tests** — render shell; click Codex; assert `onContextChange({ agent: 'codex' })`; user→project hides device; peer offline cannot select.

- [ ] **Step 2: RED** `cd web && npm test -- AgentHubShell`

- [ ] **Step 3: Implement Shell + CSS tokens**（`--space-*`, `--accent`, 无硬编码色）。

- [ ] **Step 4: Wire AgentHub.tsx** 用 context 替换可见五段 section 导航（legacy URL 仍 map 进来）。内容区暂渲染旧 section 内容按 tab/scope 粗映射，避免大爆炸。

- [ ] **Step 5: GREEN + commit**

```bash
git commit -m "feat(agent-hub): agent-first shell with scope, device, and tabs"
```

---

### Task 3: Instruction three-pane pure model

**Files:**
- Create: `web/src/pages/AgentHub/instructions/instructionThreePane.ts`
- Create: `web/src/pages/AgentHub/instructions/instructionThreePane.test.ts`

**Produces:**

```ts
export interface InstructionBlockDraft {
  id: string;
  mode: 'shared' | 'targetOnly' | 'needsAdaptation';
  title: string;
  body: string;
}

export interface InstructionThreePaneState {
  originalPath: string | null;
  originalText: string;
  blocks: InstructionBlockDraft[];
  previewText: string;
  blocksDirty: boolean;
  originalDirty: boolean;
  externalDrift: boolean;
}

export function initialThreePaneFromDisk(path: string | null, text: string): InstructionThreePaneState;
/** blocks=[], preview='' — Spec 硬规则 */
export function parseBlocksFromOriginal(state: InstructionThreePaneState): InstructionThreePaneState;
export function recomputePreview(state: InstructionThreePaneState): InstructionThreePaneState;
export type SyncBaseline = 'blocks' | 'original';
export function resolveSyncContent(state: InstructionThreePaneState): 
  | { ok: true; baseline: SyncBaseline; content: string }
  | { ok: false; reason: 'dual_dirty_conflict' | 'empty' };
```

- [ ] **Step 1: Tests**
  - `initialThreePaneFromDisk` → blocks empty, preview empty, original filled
  - `parseBlocksFromOriginal` only when called
  - dual dirty → `dual_dirty_conflict`
  - blocks-only dirty → baseline blocks
  - original-only dirty → baseline original

- [ ] **Step 2–4: RED → implement → GREEN**

```bash
cd web && npm test -- instructionThreePane
```

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(agent-hub): instruction three-pane pure state machine"
```

---

### Task 4: Instruction three-pane UI + controller

**Files:**
- Create: `web/src/pages/AgentHub/instructions/InstructionThreePaneView.tsx`
- Create: `web/src/pages/AgentHub/instructions/InstructionThreePaneView.module.css`
- Create: `web/src/pages/AgentHub/instructions/useInstructionThreePaneController.ts`
- Create: `web/src/pages/AgentHub/instructions/useInstructionThreePaneController.test.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.tsx` — tab=instructions 渲染三栏
- Modify: `web/src/api/agentHub.ts` — 若需按 target 过滤 inspect（单 agent）
- Modify: i18n agentHub.json

**Controller 规则：**
- 依赖 `context.agent` + `scope` + device/project；`inspect` 加载原始进 ③。
- 按钮「从原始重新解析块」**仅 View 原始栏**绑定 `parseBlocksFromOriginal`。
- 同步：`resolveSyncContent` → 既有 `preview_user_instruction_*` / `apply_user_instruction_plan`（单 destination = context.agent）；preview Dialog 复用/精简 `UserInstructionPreviewDialog`。
- 同步成功后：rescan；若 baseline=original，**自动 re-parse 一次**对齐 ①②；打开时仍不自动 parse。
- write blocked：同步 disabled + reason。

- [ ] **Step 1: View test** — 三栏存在；原始栏有 reparse 按钮；块栏无 reparse；同步触发 callback。

- [ ] **Step 2: Controller test** — mock inspect 返回 markdown；initial blocks empty；reparse fills blocks。

- [ ] **Step 3: Implement view + controller；AgentHub 接入。**

- [ ] **Step 4:**

```bash
cd web && npm test -- InstructionThreePane useInstructionThreePane
```

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(agent-hub): wire instruction three-pane editor and sync"
```

---

### Task 5: Backend discover-as-managed

**Files:**
- Create or modify: `src-tauri/src/agent_hub/inventory/`（ensure-managed 路径，贴合现有 reconcile）
- Modify: portable inventory inspect/refresh command path
- Create/modify: `src-tauri/tests/` L2 smoke 或 unit test under `agent_hub`
- Modify: `docs/development/quality-matrix.json` 增加/更新 L2 ID notes

**行为合同：**
- `inspect` / refresh 在返回 items 前：对每个可管理 discovered item **ensure** Hub binding/canonical（幂等）。
- 不静默改磁盘内容；仅账本/管理记录。
- 失败：该项 `unsupported` 或 error 字段，不拖垮整表。
- **删除**产品路径对「unmanaged 需 adopt」的依赖；DTO 可保留 enum 值作迁移，但新扫描不得以 unmanaged 为稳态。

- [ ] **Step 1: Failing Rust test** — 临时目录放入 skill fixture → inspect → assert managed/hub 记录存在且无二次 adopt。

- [ ] **Step 2: RED** `cd src-tauri && cargo test --locked ensure_managed -- --nocapture`

- [ ] **Step 3: Implement ensure on refresh**

- [ ] **Step 4: GREEN**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(agent-hub): ensure portable assets managed on inventory discover"
```

---

### Task 6: Portable UI — remove adopt primary path

**Files:**
- Modify: `web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.ts`
- Modify: `web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts`
- Modify: `PortableInventoryView.tsx`, `PortableAssetDetailsDrawer.tsx`, `PortableInventoryRow.tsx`
- Modify: i18n — 去掉 adopt 主 CTA；状态文案对齐 一致/漂移/冲突/不支持
- Delete or gut: `AssetAdoptionDialog` 主入口

**Presentation 规则：**
- `resolvePortablePrimaryAction`：**不得**返回 `adopt`；对历史 unmanaged 显示「刷新以纳入」或直接 enable/disable 若后端已 managed。
- 过滤默认不再强调 unmanaged bucket。
- 列表默认 `filters.target = context.agent`（单 agent 工作台）。

- [ ] **Step 1: Update presentation tests** — unmanaged fixture primary action ≠ adopt。

- [ ] **Step 2: RED then fix presentation + views.**

- [ ] **Step 3:**

```bash
cd web && npm test -- portableInventoryPresentation PortableInventory
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(agent-hub): drop adopt primary path for discover-as-managed assets"
```

---

### Task 7: Device + remote project context plumbing

**Files:**
- Modify: `useAgentHubController.ts` — load devices; project list local+remote
- Modify: `useInstructionThreePaneController.ts`, `usePortableInventoryController.ts` — pass device/project into API
- Modify: `web/src/api/portableInventory.ts`, `agentHub.ts` — optional `deviceId` / `projectRef` on inspect & mutations
- Modify: Rust commands + inventory scanners to accept remote target via peer proxy（复用既有 P2P workbench/agent_hub 远端模式；**若远端写尚未存在**：peer 上下文 UI 可写但 apply 返回明确 error code，不得静默本地写）

**最小可交付：**
1. 本机路径全通。
2. peer 上下文：inspect 走 peer HTTP（若已有 agent hub 远端 API）；否则 Task 7 交付 UI + typed error `AGENT_HUB_PEER_CONTEXT_UNAVAILABLE`，T8 再补全。

- [ ] **Step 1: API/decoder tests** for optional context fields.

- [ ] **Step 2: Controller tests** — changing deviceId retriggers inspect with new id.

- [ ] **Step 3: Implement plumbing; document any peer gap in quality-matrix notes.**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(agent-hub): plumb device and remote project context into inventory"
```

---

### Task 8: LAN pull/push toolbar integration

**Files:**
- Modify: `AgentHubShell` actions → open existing `PortablePullDrawer` / `LanPushDialog` with prefilled agent + device
- Modify: `usePortablePullController.ts` — default source peer = context.deviceId when set
- Modify: pull/push copy for same-agent only
- Tests: pull controller defaults

- [ ] **Step 1: Failing test** — context device=peer-1 → open pull → sourceDeviceId=peer-1.

- [ ] **Step 2: Wire shell buttons; remove dependency on syncImport section as only entry.**

- [ ] **Step 3: GREEN + commit**

```bash
git commit -m "feat(agent-hub): expose LAN pull/push from agent shell toolbar"
```

---

### Task 9: Cross-agent adapt page (selective)

**Files:**
- Create: `web/src/pages/AgentHub/crossAgent/*`
- Modify: `App.tsx` — route or `view=adapt`
- Modify: `web/src/api/agentHub.ts` — existing preview/apply cross-agent instruction (+ skill if present)
- Modify: shell `onAdapt` → navigate with source agent + scope
- Deprecate primary use of `CrossAgentSyncDialog`（可内嵌逻辑到 page）

**Page flow (single scroll sections):**
1. Targets multi-select  
2. Scope confirm（project opt-in gate UI）  
3. Content pick（instructions blocks / assets）  
4. Rule classification display shared/diff/residual  
5. Block-level 「用 Claude 适配此块」（可先 stub 调 internalClaude / 既有 optimizer 管道；若无则手改 only + TODO 接 T10 共享 runner）  
6. Preview → Apply  

- [ ] **Step 1: Controller tests** — cannot select source as dest; peer context blocked; preview required before apply.

- [ ] **Step 2: Implement page + wire navigation.**

- [ ] **Step 3:**

```bash
cd web && npm test -- crossAgent useCrossAgentAdapt
cd src-tauri && cargo test --locked cross_agent -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(agent-hub): dedicated cross-agent selective adapt page"
```

---

### Task 10: Claude full-volume adapt (preview required)

**Files:**
- Create: `src-tauri/src/agent_hub/cross_agent_full.rs`
- Modify: `src-tauri/src/commands/agent_hub.rs` — `agent_hub_preview_cross_agent_full`, `agent_hub_apply_cross_agent_full`
- Modify: `web/src/api/agentHub.ts` + tests
- Modify: `CrossAgentAdaptPage` — mode toggle 选择性 | 全量
- L2: temp homes + mock/stub Claude runner interface

**DTO sketch:**

```rust
pub struct CrossAgentFullPlan {
    pub source: AgentTarget,
    pub destination: AgentTarget,
    pub scope: String, // "user" | project id
    pub items: Vec<CrossAgentFullPlanItem>,
    pub plan_hash: String,
}
pub struct CrossAgentFullPlanItem {
    pub kind: CrossAgentKind,
    pub logical_key: String,
    pub action: String, // create|update|skip
    pub path: String,
    pub content: Option<String>,
    pub residual_reason: Option<String>,
    pub included: bool, // user may toggle off in preview
}
```

**规则：**
- 全量锁定五类清单进 preview；允许用户 `included=false` 单项。
- **无** skip-preview apply 命令。
- Claude runner trait：`FullAdaptRunner::propose(snapshot) -> plan`；测试用 deterministic stub；生产调本机 Claude Code headless（失败 → 按钮 disabled / error 文案）。
- Apply 逐项结果；partial failure 可重试失败项。

- [ ] **Step 1: Rust unit** — stub runner returns 2 items; preview hash stable; apply without preview token fails.

- [ ] **Step 2: Implement runner stub + commands.**

- [ ] **Step 3: Frontend full mode UI + forced preview.**

- [ ] **Step 4:**

```bash
cd src-tauri && cargo test --locked cross_agent_full -- --nocapture
cd web && npm test -- agentHub.crossAgent useCrossAgentAdapt
```

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(agent-hub): Claude full-volume cross-agent adapt with forced preview"
```

---

### Task 11: Deep links, i18n parity, E2E, docs

**Files:**
- Modify: `web/src/App.lazyRoutes.test.tsx` / agent-hub e2e specs
- Create/update: `web/tests/agent-hub-interaction.spec.ts`（或扩展现有）
  - E2E IDs: `E2E-AGENT-HUB-SHELL-001`, `E2E-AGENT-HUB-INSTR-3PANE-001`, `E2E-AGENT-HUB-DISCOVER-MANAGED-001`, `E2E-AGENT-HUB-ADAPT-FULL-001`
- Modify: `docs/development/quality-matrix.json`
- Modify: `AGENTS.md`, `web/CLAUDE.md`, Spec status → 实施中/已计划
- zh/en key parity check: `npm run check:i18n`

- [x] **Step 1: E2E mock flows** for shell context, three-pane initial empty blocks, no adopt button, adapt page preview gate.

- [x] **Step 2:** i18n parity + focused unit suites + `agent-hub-interaction.spec.ts` L1 mock（full `agent-hub.spec.ts` legacy Gate journeys may need dual-path follow-up）.

```bash
cd web && npm run check:i18n && npm test -- agentHub AgentHubShell instructionThreePane crossAgent portableInventoryPresentation useInstructionThreePane useCrossAgentAdapt usePortablePull useAgentHubController localeParity && npm run test:e2e -- agent-hub-interaction.spec.ts
```

- [x] **Step 3: Update matrix + CLAUDE notes; commit**

```bash
git commit -m "test(docs): lock agent hub interaction redesign E2E and matrix IDs"
```

---

## Spec Coverage Checklist

| Spec 要求 | Task |
|-----------|------|
| Agent 同级切换 | T2 |
| 用户级设备切换 | T2, T7 |
| 项目级远端项目、无设备切换 | T2, T7 |
| 五 Tab | T2, T4, T6 |
| 提示词三栏 + 初始空块 + 原始栏 reparse | T3, T4 |
| 显式同步 preview | T4 |
| 发现即管理 | T5, T6 |
| 无停止管理保留文件 | T6 |
| MCP UI 不脱敏 | T6（确认 McpDetails） |
| LAN 拉/推工具栏 | T8 |
| peer 完整读写（动作限制） | T7, T8, T9 adapt disabled on peer |
| 跨 Agent 独立页选择性 | T9 |
| Claude 全量 + 强制预览 | T10 |
| 深链 / 迁移旧 section | T1, T11 |
| 无后台跨 Agent 自动写 | T9/T10 仅用户确认 apply |

---

## Out of Scope for This Plan

- Marketplace  
- LAN 鉴权  
- 后台跨 Agent 收敛  
- 删除全部旧 userInstructions 文件（可在 T4 后残留兼容层，T11 再删）  
- 真实多机 L3 认证  

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-08-agent-hub-interaction-redesign.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session with executing-plans checkpoints  

Which approach?
