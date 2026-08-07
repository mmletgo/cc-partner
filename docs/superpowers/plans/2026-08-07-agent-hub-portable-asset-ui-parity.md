# Agent Hub Four-Kind Asset Management UI Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** 在 Agent Hub 中交付三 Agent、四类资产的真实库存管理、本机 preview/apply、同类远端选择性 Pull、可复现 deep link，并在功能等价证据通过后删除旧 Claude Code 前端。

**Architecture:** 新建 portableAssets 子域 controller 和 pure views，消费后端计划冻结的严格 DTO/命令，不再把 inventory 状态塞入现有巨型 controller。列表以 observed inventory 为真源，四类详情与动作 Dialog 分离；远端 Pull 使用独立 controller/Drawer，最后由 AgentHub composer 接线并统一 URL、i18n、E2E 和旧代码清理。

**Tech Stack:** React 19、TypeScript strict、Vite、i18next、CSS Modules/design tokens、Vitest、Playwright backendHarness、Tauri invokeDecoded。

## Global Constraints

- 必须先完整集成并通过 docs/superpowers/plans/2026-08-07-agent-hub-portable-asset-backend-parity.md；本计划不臆造或改变后端命令/DTO fixtures。
- 权威设计为 docs/superpowers/specs/2026-08-07-agent-hub-portable-asset-management-parity-design.md。
- 四类固定为 Skill/Command/Plugin/MCP，等权展示；三 target 固定为 Claude/Codex/OpenCode。
- UI 只支持同类 Agent Pull，不渲染 cross-target destination picker。
- 本机 inventory 是 actual 状态真源；canonical desired/materialization 下沉到详情，不冒充 installed/enabled。
- Views 不 import @/api/*；controller 持有 sequence、stale、selection、planToken、clientRequestId 和 action state。
- 所有 Hooks 位于 early return 前；不新增页面级 useWorkbenchController。
- 复用 Button/Card/Input/Pill/StatusMessage/Dialog/Drawer/ProgressBar；危险动作不用 window.confirm。
- 所有颜色、字体、间距、圆角、阴影使用 tokens.css 已有 var；交互 transition 遵守项目规范；无 !important。
- 用户文案全部走 zh/en i18n，保持 key parity；TS Props 严格类型，无 any。
- MCP 只显示 credential present/hash/diagnostic，不渲染、日志或错误回显 secret。
- Refresh 失败保留 stale snapshot；stale 禁止 mutation；partial/outcomeUnknown 不得显示全成功。
- 旧前端只有在新 E2E parity 通过后删除；旧 Rust/P2P N/N+1 本计划不删除。
- E2E mock 仅为 L1 UI evidence，不替代后端计划 L2；L3 未执行继续 NOT VERIFIED。

---

## Prerequisites and Consumed Contract

**Prerequisite plan:** docs/superpowers/plans/2026-08-07-agent-hub-portable-asset-backend-parity.md（8 Tasks）。

**Consumes:** 该计划 Produced Contract 中 8 个 Tauri 命令、全部 camelCase response fixtures、两个 L2 evidence ID、P2P capability/mapping/error codes。若集成后的名字与设计阶段建议不同，以后端计划 committed fixture 为唯一真源，并在本计划 Task 1 原样编码。

## File Structure

- Create: web/src/lib/types/portableInventory.ts
- Create: web/src/lib/schemas/portableInventory.ts and tests
- Create: web/src/api/portableInventory.ts and tests
- Create: web/src/pages/AgentHub/portableAssets/{usePortableInventoryController.ts,portableInventoryPresentation.ts,PortableInventoryView.tsx}
- Create: web/src/pages/AgentHub/portableAssets/{PortableAssetDetailsDrawer.tsx,SkillDetails.tsx,CommandDetails.tsx,McpDetails.tsx,PortableAssetActionDialog.tsx}
- Create: web/src/pages/AgentHub/portableAssets/{usePortablePullController.ts,portablePullPresentation.ts,PortablePullDrawer.tsx}
- Modify: web/src/pages/AgentHub/{AgentHub.tsx,AgentHub.module.css,useAgentHubController.ts,PluginComponentsDrawer.tsx,pluginPackagePresentation.ts}
- Modify: web/src/App.tsx, web/src/i18n/locales/{zh,en}/agentHub.json
- Modify: web/tests/agent-hub.spec.ts and route/unit tests
- Delete after parity: ClaudeCodeAssets page/API, ClaudeAssetRow, RemoteAssetPicker, old helper/types/i18n/tests
- Modify: AGENTS.md, web/CLAUDE.md, docs/prd.md, docs/development/quality-matrix.json

## Shared Write Sets

- F1 exclusively owns types/schema/API/barrels.
- After F1, F2 inventory list, F3 details/actions/Plugin files and F4 Pull files use isolated worktrees and non-overlapping files.
- AgentHub.tsx、useAgentHubController.ts、AgentHub.module.css、App.tsx and both agentHub.json are exclusively F5.
- E2E、legacy deletion、i18n index/barrels、AGENTS/CLAUDE/PRD/quality matrix are exclusively F6.
- F2 owns its page-local PortableInventoryRow and does not modify canonical AgentAssetRow; F3 owns PluginComponentsDrawer/plugin presentation; F4 never modifies LanPushDialog.

## Task Dependency Graph

~~~
F1 -> F2 --\
   \-> F3 ---+-> F5 -> F6
    \-> F4 --/
~~~

- Exact edges: F1→F2、F1→F3、F1→F4、{F2,F3,F4}→F5、F5→F6。
- Dependency-ready waves: [F1]、[F2,F3,F4]、[F5]、[F6]。
- F2/F3/F4 可在独立 worktree 并行；按编号集成并通过完整 portableAssets unit suite 后启动 F5。

### Task 1: Add Strict Portable Inventory, Action and Pull Wire Contracts

**Files:**
- Create: web/src/lib/types/portableInventory.ts
- Create: web/src/lib/schemas/portableInventory.ts
- Create: web/src/lib/schemas/portableInventory.test.ts
- Create: web/src/api/portableInventory.ts
- Create: web/src/api/portableInventory.test.ts
- Modify: web/src/lib/types/index.ts
- Modify: web/src/lib/schemas/index.ts
- Modify: web/src/lib/types/typeBarrel.test.ts
- Test: new tests and typeBarrel test

**Interfaces:**
- Consumes: backend Plan committed camelCase fixtures and exact command manifest.
- Produces:

~~~ts
export type PortableAssetKind = 'skill' | 'command' | 'plugin' | 'mcp';
export type PortableInventoryManagementState =
  | 'unmanaged'
  | 'hubManaged'
  | 'drifted'
  | 'externalCollision'
  | 'unsupported';

export interface PortableInventorySnapshotDto {
  inventorySnapshotHash: string;
  refreshedAt: string;
  stale: boolean;
  targets: PortableInventoryTargetDto[];
  items: PortableInventoryItemDto[];
}

export interface PortableAssetApi {
  inspect(): Promise<PortableInventorySnapshotDto>;
  previewAction(request: PreviewPortableAssetActionRequest): Promise<PortableAssetActionPlanDto>;
  applyAction(request: ApplyPortableAssetActionRequest): Promise<PortableAssetActionResultDto>;
  getAction(clientRequestId: string): Promise<PortableAssetActionResultDto>;
}
~~~

Pull request/result types are copied exactly from the backend fixture; no optional/default field is invented.

- [ ] **Step 1: Write failing strict decoder tests**

Use one valid 3×4 fixture. Reject missing hash/target/kind/capability, invalid enum, non-finite size and malformed item result. Allow unknown extra fields for forward compatibility. Assert a secret-shaped extra payload cannot be represented by the typed MCP DTO.

- [ ] **Step 2: Run RED**

~~~bash
cd web
npm test -- src/lib/schemas/portableInventory.test.ts src/api/portableInventory.test.ts src/lib/types/typeBarrel.test.ts
~~~

Expected: modules/exports do not exist.

- [ ] **Step 3: Implement types, decoders and invokeDecoded API**

Copy exact command constants and request shapes from backend Plan. Use runtimeSchema primitives and fail-closed success-body decode. Normalize only transport errors; preserve stable backend codes for stale/outcomeUnknown/partial handling.

- [ ] **Step 4: Run GREEN and commit**

~~~bash
cd web
npm test -- src/lib/schemas/portableInventory.test.ts src/api/portableInventory.test.ts src/lib/types/typeBarrel.test.ts
npm run build
git add src/lib/types/portableInventory.ts src/lib/schemas/portableInventory.ts src/lib/schemas/portableInventory.test.ts src/api/portableInventory.ts src/api/portableInventory.test.ts src/lib/types/index.ts src/lib/schemas/index.ts src/lib/types/typeBarrel.test.ts
git commit -m "feat: add portable asset frontend contracts"
~~~

### Task 2: Build Inventory Controller, Four-Kind Filters and List

**Files:**
- Create: web/src/pages/AgentHub/portableAssets/usePortableInventoryController.ts
- Create: web/src/pages/AgentHub/portableAssets/usePortableInventoryController.test.tsx
- Create: web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.ts
- Create: web/src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts
- Create: web/src/pages/AgentHub/portableAssets/PortableInventoryView.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortableInventoryView.test.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortableInventoryRow.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortableInventoryRow.module.css
- Create: web/src/pages/AgentHub/portableAssets/PortableInventoryRow.test.tsx
- Test: new files

**Interfaces:**
- Consumes: F1 portableAssetApi/types.
- Produces:

~~~ts
export type PortableInventoryFilters = {
  kind: PortableAssetKind;
  target: 'all' | AgentTarget;
  scope: 'all' | 'user' | 'project';
  actualState: 'all' | 'enabled' | 'disabled' | 'problem';
  management: 'all' | PortableInventoryManagementState;
  search: string;
};

export interface UsePortableInventoryControllerResult {
  snapshot: PortableInventorySnapshotDto | null;
  visibleItems: PortableInventoryItemDto[];
  filters: PortableInventoryFilters;
  stale: boolean;
  mutationBlocked: boolean;
  refresh(): Promise<void>;
  selectItem(id: string | null): void;
  openAction(itemId: string, action: PortableAssetActionKind): void;
}
~~~

- [ ] **Step 1: Write failing pure filter/status tests**

Cover all four kind tabs, target/scope/actual/management/search combinations, plugin component exclusion from standalone count, problem classification, projectOptedIn read-only and actualEnabled=null.

- [ ] **Step 2: Write failing controller race tests**

Defer two refreshes and resolve old last; assert new snapshot wins. Refresh rejection keeps old snapshot stale and mutationBlocked. Applying one item locks only that item. Unopted/unsupported/stale items never expose mutation action.

- [ ] **Step 3: Run RED**

~~~bash
cd web
npm test -- src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts src/pages/AgentHub/portableAssets/usePortableInventoryController.test.tsx
~~~

- [ ] **Step 4: Implement controller and pure view**

All hooks precede guards. Use refresh generation and mounted ref; no canonical listAssets call. Render fixed Skill/Command/Plugin/MCP tabs, tokenized filters and actual-state rows with one primary action.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd web
npm test -- src/pages/AgentHub/portableAssets
npm run check:i18n
git add src/pages/AgentHub/portableAssets/usePortableInventoryController.ts src/pages/AgentHub/portableAssets/usePortableInventoryController.test.tsx src/pages/AgentHub/portableAssets/portableInventoryPresentation.ts src/pages/AgentHub/portableAssets/portableInventoryPresentation.test.ts src/pages/AgentHub/portableAssets/PortableInventoryView.tsx src/pages/AgentHub/portableAssets/PortableInventoryView.test.tsx src/pages/AgentHub/portableAssets/PortableInventoryRow.tsx src/pages/AgentHub/portableAssets/PortableInventoryRow.module.css src/pages/AgentHub/portableAssets/PortableInventoryRow.test.tsx
git commit -m "feat: add portable inventory workspace"
~~~

### Task 3: Add Four-Kind Details and Local Preview/Apply Flow

**Files:**
- Create: web/src/pages/AgentHub/portableAssets/PortableAssetDetailsDrawer.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortableAssetDetailsDrawer.test.tsx
- Create: web/src/pages/AgentHub/portableAssets/{SkillDetails.tsx,CommandDetails.tsx,McpDetails.tsx}
- Create: web/src/pages/AgentHub/portableAssets/PortableAssetActionDialog.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortableAssetActionDialog.test.tsx
- Modify: web/src/pages/AgentHub/{PluginComponentsDrawer.tsx,PluginComponentsDrawer.test.tsx,pluginPackagePresentation.ts,pluginPackagePresentation.test.ts}
- Test: listed files

**Interfaces:**
- Consumes: F1 action types/API and inventory item selected by F2.
- Produces pure details/action props:

~~~ts
export interface PortableAssetActionDialogProps {
  open: boolean;
  item: PortableInventoryItemDto | null;
  plan: PortableAssetActionPlanDto | null;
  result: PortableAssetActionResultDto | null;
  busy: boolean;
  error: string | null;
  onPreview(request: PreviewPortableAssetActionRequest): void;
  onConfirm(planToken: string, clientRequestId: string): void;
  onReconcile(clientRequestId: string): void;
  onClose(): void;
}
~~~

- [ ] **Step 1: Write failing four-kind rendering tests**

Assert Skill tree/origin/invocation; Command native file/invocation/compatibility; Plugin package/components/residual/activation/ownership delete groups; MCP transport/source/credential-present without secret. Unknown/unsupported fields use honest diagnostic, not fabricated UI.

- [ ] **Step 2: Write failing action state-machine tests**

Inspect→preview→confirm→apply→rescan; planToken/clientRequestId preserved. Cover keepData, blocked/stale, partial item rows, outcomeUnknown reconcile, close prevention while applying and focus restoration.

- [ ] **Step 3: Run RED**

~~~bash
cd web
npm test -- PortableAssetDetailsDrawer PortableAssetActionDialog PluginComponentsDrawer pluginPackagePresentation
~~~

- [ ] **Step 4: Implement pure details and shared Dialog**

Views receive callbacks only. Replace Plugin Drawer dead-end delete preview with generic action preview/confirm. Delete-everywhere stays only in details danger zone. No window.confirm and no credential value rendering.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd web
npm test -- PortableAssetDetailsDrawer PortableAssetActionDialog PluginComponentsDrawer pluginPackagePresentation
git add src/pages/AgentHub/portableAssets/PortableAssetDetailsDrawer.tsx src/pages/AgentHub/portableAssets/PortableAssetDetailsDrawer.test.tsx src/pages/AgentHub/portableAssets/SkillDetails.tsx src/pages/AgentHub/portableAssets/CommandDetails.tsx src/pages/AgentHub/portableAssets/McpDetails.tsx src/pages/AgentHub/portableAssets/PortableAssetActionDialog.tsx src/pages/AgentHub/portableAssets/PortableAssetActionDialog.test.tsx src/pages/AgentHub/PluginComponentsDrawer.tsx src/pages/AgentHub/PluginComponentsDrawer.test.tsx src/pages/AgentHub/pluginPackagePresentation.ts src/pages/AgentHub/pluginPackagePresentation.test.ts
git commit -m "feat: add portable asset detail actions"
~~~

### Task 4: Add Same-Agent Remote Pull Controller and Drawer

**Files:**
- Create: web/src/pages/AgentHub/portableAssets/usePortablePullController.ts
- Create: web/src/pages/AgentHub/portableAssets/usePortablePullController.test.tsx
- Create: web/src/pages/AgentHub/portableAssets/portablePullPresentation.ts
- Create: web/src/pages/AgentHub/portableAssets/portablePullPresentation.test.ts
- Create: web/src/pages/AgentHub/portableAssets/PortablePullDrawer.tsx
- Create: web/src/pages/AgentHub/portableAssets/PortablePullDrawer.test.tsx
- Test: new files

**Interfaces:**
- Consumes: F1 Pull API/types and existing devicesApi.
- Produces:

~~~ts
export interface UsePortablePullControllerResult {
  devices: Device[];
  selectedDeviceId: string;
  sourceTarget: AgentTarget;
  remoteInventory: RemotePortableInventoryDto | null;
  selectedItemIds: Set<string>;
  conflictPolicy: 'skipExisting' | 'replaceAfterPreview';
  plan: PortablePullPlanDto | null;
  result: PortablePullResultDto | null;
  loadInventory(): Promise<void>;
  preview(): Promise<void>;
  apply(): Promise<void>;
  reconcile(): Promise<void>;
}
~~~

Destination target is sourceTarget and is not separately editable.

- [ ] **Step 1: Write failing selection/presentation tests**

Cover device→source target→inventory, all four filters, select visible, same-target label, mapping canonical-only, skip/replace diff, credential disclosure boolean, per-item result/progress.

- [ ] **Step 2: Write failing async/recovery tests**

Device/target change cancels stale inventory and clears invalid selection. Preview failure retains selection/policy. Partial and outcomeUnknown expose reconcile; repeated apply reuses clientRequestId. Stale remote inventory disables confirm.

- [ ] **Step 3: Run RED**

~~~bash
cd web
npm test -- usePortablePullController PortablePullDrawer portablePullPresentation
~~~

- [ ] **Step 4: Implement independent controller/Drawer**

Reuse devicesApi and primitives. Do not reuse Claude-only RemoteAssetPicker or source-push LanPushDialog. No cross-Agent destination control. Show no-auth LAN risk copy and canonical-only mapping result explicitly.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd web
npm test -- usePortablePullController PortablePullDrawer portablePullPresentation
git add src/pages/AgentHub/portableAssets/usePortablePullController.ts src/pages/AgentHub/portableAssets/usePortablePullController.test.tsx src/pages/AgentHub/portableAssets/portablePullPresentation.ts src/pages/AgentHub/portableAssets/portablePullPresentation.test.ts src/pages/AgentHub/portableAssets/PortablePullDrawer.tsx src/pages/AgentHub/portableAssets/PortablePullDrawer.test.tsx
git commit -m "feat: add portable asset pull workspace"
~~~

### Task 5: Integrate Agent Hub Sections, URL State and i18n

**Files:**
- Modify: web/src/pages/AgentHub/useAgentHubController.ts
- Modify: web/src/pages/AgentHub/useAgentHubController.test.ts
- Modify: web/src/pages/AgentHub/AgentHub.tsx
- Modify: web/src/pages/AgentHub/AgentHub.test.tsx
- Modify: web/src/pages/AgentHub/AgentHub.module.css
- Create: web/src/pages/AgentHub/portableAssets/index.ts
- Modify: web/src/App.tsx
- Modify: web/src/App.lazyRoutes.test.tsx
- Modify: web/src/i18n/locales/{en,zh}/agentHub.json
- Test: listed tests and locale parity

**Interfaces:**
- Consumes: F2/F3/F4 controllers/views.
- Produces URL query contract:

~~~text
section=assets
target=claude|codex|opencode
kind=skill|command|plugin|mcp
scope=user|project
state=all|enabled|disabled|problem
management=all|unmanaged|hubManaged|drifted|externalCollision|unsupported
inventoryItemId=<stable-id>
~~~

- [ ] **Step 1: Write failing URL/composer tests**

Direct URL restores section/filters/selected detail; internal changes update search params without losing unrelated deep links. Legacy portableAssets query alias maps to assets. /claude-code redirects exactly to /agent-hub?section=assets&target=claude.

- [ ] **Step 2: Write failing integrated view tests**

Navigation names are user instructions/Agent assets/project/sync/diagnostics. Agent assets render F2 plus F3. Sync retains source-push/Git and opens F4 Pull. Existing conflict/project deep links still work.

- [ ] **Step 3: Run RED**

~~~bash
cd web
npm test -- src/pages/AgentHub src/App.lazyRoutes.test.tsx
~~~

- [ ] **Step 4: Compose subcontrollers and centralize styles/copy**

Existing useAgentHubController becomes composer only; no duplicate inventory/action/pull state. F5 exclusively adds typed i18n keys and CSS tokens. Hooks remain before early returns.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd web
npm test -- src/pages/AgentHub src/App.lazyRoutes.test.tsx
npm run check:i18n
npm test -- localeParity
npm run build
git add src/pages/AgentHub src/App.tsx src/App.lazyRoutes.test.tsx src/i18n/locales/en/agentHub.json src/i18n/locales/zh/agentHub.json
git commit -m "feat: integrate Agent Hub asset workspace"
~~~

### Task 6: Lock Parity E2E, Remove Old Frontend and Update Product Contracts

**Files:**
- Modify: web/tests/agent-hub.spec.ts
- Modify: web/src/i18n/index.ts
- Modify: web/src/components/domain/index.ts
- Modify: web/src/lib/types/core.ts
- Modify: web/src/lib/types/typeBarrel.test.ts
- Delete: web/src/pages/ClaudeCodeAssets/
- Delete: web/src/api/claudeCodeAssets.ts
- Delete: web/src/components/domain/ClaudeAssetRow/
- Delete: web/src/components/domain/RemoteAssetPicker/
- Delete: web/src/lib/claudeCodeAssets.ts
- Delete: web/src/lib/claudeCodeAssets.test.ts
- Delete: web/src/i18n/locales/{en,zh}/claudeCodeAssets.json
- Modify: AGENTS.md
- Modify: web/CLAUDE.md
- Modify: docs/prd.md
- Modify: docs/development/quality-matrix.json
- Test: E2E, unit, i18n, build, docs/traceability

**Interfaces:**
- Consumes: complete F1–F5 UI and backend Plan L2 evidence.
- Produces stable E2E parity evidence and removes all unreachable legacy frontend only after it passes.

- [ ] **Step 1: Write failing backendHarness E2E journeys**

Extend deterministic Agent Hub fixtures with 3×4 inventory/action/Pull responses. Cover four filters, stale mutation gate, absent-target install, enable/disable/uninstall preview+apply+rescan, four details, Plugin confirmed deletion, exact legacy route deep link, same-target Pull, canonical-only mapping, replace diff, progress, partial/outcomeUnknown and no credential plaintext.

- [ ] **Step 2: Run RED**

~~~bash
cd web
npm run test:e2e -- agent-hub.spec.ts
~~~

Expected: new journeys fail before integration/fixtures are complete.

- [ ] **Step 3: Make E2E pass without production bypasses**

Use tests/fixtures.ts backendHarness and appBootstrap; production code never reads test globals. Keep mock evidence labeled L1 and reference backend Plan L2 IDs in quality matrix without upgrading L3.

- [ ] **Step 4: Delete legacy frontend and update imports/types/i18n**

Remove only files listed above. Remove legacy ClaudeCodeAsset DTOs and namespace after rg proves no live consumer. Preserve Rust legacy IPC/P2P. Update root component inventory and web controller/view contract.

- [ ] **Step 5: Update PRD/evidence and run full completion gates**

Replace old Claude-only PRD behavior with three-target/four-kind parity and same-agent Pull. Record exact E2E and backend L2 IDs; unrun multi-host/platform L3 remains NOT VERIFIED.

~~~bash
cd web
npm run test:e2e -- agent-hub.spec.ts
npm test -- src/pages/AgentHub src/lib/schemas/portableInventory.test.ts src/api/portableInventory.test.ts src/lib/types/typeBarrel.test.ts
npm run check:i18n
npm test -- localeParity
npm run lint
npm run build
npm run check:bundle
cd ..
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
~~~

- [ ] **Step 6: Commit**

~~~bash
git add -A web/src/pages/ClaudeCodeAssets web/src/api/claudeCodeAssets.ts web/src/components/domain/ClaudeAssetRow web/src/components/domain/RemoteAssetPicker web/src/lib/claudeCodeAssets.ts web/src/lib/claudeCodeAssets.test.ts web/src/i18n/locales/en/claudeCodeAssets.json web/src/i18n/locales/zh/claudeCodeAssets.json web/tests/agent-hub.spec.ts web/src/i18n/index.ts web/src/components/domain/index.ts web/src/lib/types/core.ts web/src/lib/types/typeBarrel.test.ts AGENTS.md web/CLAUDE.md docs/prd.md docs/development/quality-matrix.json
git commit -m "feat: complete Agent Hub asset management parity"
~~~

## Completion Contract

- Six tasks are committed and integrated by the task graph.
- Agent Hub displays observed 3×4 inventory with fixed four-kind division and typed filters.
- Four kind details/actions use preview/apply/rescan; stale, blocked, partial and outcomeUnknown remain honest.
- Plugin deletion confirms ownership effects; MCP never renders credential values.
- Same-agent Pull supports remote inventory, selection, conflict preview, progress, canonical-only mapping and per-item report without cross-target picker.
- /claude-code deep-links to assets+Claude; other existing Agent Hub deep links remain valid.
- Legacy ClaudeCodeAssets frontend/API/components/helper/types/i18n are deleted only after parity E2E passes.
- Fresh unit/E2E/i18n/lint/build/bundle/quality/docs commands pass with results recorded.
- Backend L2 evidence is referenced; real multi-host/full-platform L3 remains NOT VERIFIED.

## Adjacent-Race Checklist

- refresh/apply/rescan stale response ordering；
- filter/deep-link synchronization and browser history；
- same-item action locking and Dialog close/focus restoration；
- preview expiry/source drift and retry；
- outcomeUnknown reconcile without duplicate clientRequestId；
- Pull device/target switch cancellation and selection reset；
- partial progress/report preservation；
- nested Drawer/Dialog Escape/inert/scroll lock；
- route redirect/query aliases and existing conflict/project deep links；
- credential-shaped response never reaching DOM/error snapshot。
