# Global Inbox Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现“现在有哪些事情需要我处理，工作才能继续”的全局 Inbox，在桌面与移动端共享实时投影、数量和分类语义，并只导航到权威业务界面。

**Architecture:** Rust 新增无持久化表的 `attention` 聚合领域，通过显式 source 投影 Orchestrator task/mirror/outbox 与 Workbench tmux dependency；任一非网络 source 失败则整次快照失败，远端网络失败只回退最近 mirror。React 共享 DTO、纯规则和 Attention Provider，桌面使用独立 `/attention` 页面，移动端在 Projects 后增加第二导航项；解决动作仍留在 Orchestrator/Settings 原界面。

**Tech Stack:** Rust 2021, Tauri 2, axum, sqlx SQLite, tokio, React 19, TypeScript, React Router v6, Vitest, Playwright, CSS Modules, i18next.

## Global Constraints

- 前置依赖：先完成 Vitest、Workbench controller、P2P protocol plans；Remote runtime plan 与本计划共享 Orchestrator/Workbench 文件，不得并发执行，推荐先完成 runtime 再开始 Inbox。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md`、`web/CLAUDE.md`、`src-tauri/CLAUDE.md`；代码使用 UTF-8，新增业务函数/组件添加中文 Business Logic / Code Logic 注释。
- 不新增 Inbox 表、已读状态、忽略/稍后/关闭动作或已解决历史。
- Inbox 条目只导航，不在列表内执行 Deliver、Request Rework、Retry、Discard 或依赖安装。
- outbox source 接入前必须先在原 Automation UI 实现 failed-only Retry/Discard；Retry 保留原 payload 与 `clientRequestId`，Discarded 保留审计但不再活跃。
- 设备离线不是独立 source；只有它造成的 cached task/outbox 业务后果可以显示。
- remote mirror 刷新最多 4 并发；网络失败可回退 mirror，损坏 mirror/SQLite/项目仓储失败必须使整个 snapshot 失败。
- Provider 有 snapshot 时刷新失败只标 stale，不清列表/badge；初次失败没有数字；旧请求不能覆盖新请求。
- React hooks 全部位于 early return 之前；视觉样式只用现有 `tokens.css`，同时验证浅色/深色；固定文案进入 `attention` i18n namespace。
- remote shortcut 已被移除时，其 orphan failed outbox 不再代表当前可导航项目，保留审计但不投影为 Attention item。

---

## Task Dependency Graph

最大并行 waves：`(T1 | T3 | T4) → (T2 | T5) → T6 → T7 → (T8 | T9) → T10`。首 wave 分别稳定 outbox、tmux 时间与 Attention 核心；T5 依赖 T1/T4，T6 汇合 T3/T4/T5，T8/T9 仅在共享 Provider 完成后并行，T10 汇合桌面、移动端及原界面 invalidation。

## File Structure

### Backend

- Create `src-tauri/src/attention/mod.rs`: attention exports。
- Create `src-tauri/src/attention/models.rs`: snapshot/item/count/category/freshness/source/target DTO。
- Create `src-tauri/src/attention/source.rs`: source trait 与 source error policy。
- Create `src-tauri/src/attention/aggregator.rs`: 全成功合并、去重、排序、计数。
- Create `src-tauri/src/attention/orchestrator_source.rs`: local task、remote mirror、failed outbox 投影。
- Create `src-tauri/src/attention/workbench_dependency_source.rs`: 项目存在性与 tmux dependency 投影。
- Create `src-tauri/src/commands/attention.rs`: Tauri command/state helper。
- Create `src-tauri/src/net/routes/attention.rs`: Mobile HTTP handler。
- Modify `src-tauri/src/net/protocol.rs`: 增加并声明 `attention.v1`，与 Attention route 同一提交落地。
- Modify `src-tauri/src/orchestrator/outbox.rs`: 增加 `Discarded`。
- Modify `src-tauri/src/orchestrator/repo.rs`: failed-only Retry/Discard 和 attention 查询所需 repository helpers。
- Modify `src-tauri/src/commands/orchestrator.rs`: remote outbox Retry/Discard state/Tauri helpers。
- Modify `src-tauri/src/net/routes/orchestrator.rs`: Mobile Retry/Discard handlers。
- Modify `src-tauri/src/workbench/dependencies.rs`: 进程内 `status_changed_at`。
- Modify `src-tauri/src/commands/workbench_dependencies.rs`: 暴露稳定状态时间供 source 读取。
- Modify `src-tauri/src/commands/mod.rs`, `src-tauri/src/net/routes/mod.rs`, `src-tauri/src/net/http_server.rs`, `src-tauri/src/lib.rs`: 注册模块、routes 和 commands。

### Frontend

- Modify `web/src/lib/types.ts`: Attention DTO 和 `discarded` outbox status。
- Create `web/src/lib/attention.ts`: badge、分组、排序保护、action key、desktop target URL。
- Create `web/src/api/attention.ts`: Tauri loader。
- Create `web/src/api/attentionHttp.ts`: capability-gated Mobile loader。
- Modify `web/src/api/orchestrator.ts`, `web/src/api/workbenchHttp.ts`: outbox Retry/Discard。
- Create `web/src/hooks/attentionState.ts`: 可测试 reducer/request sequence。
- Create `web/src/hooks/attentionContext.ts` and `web/src/hooks/useAttention.tsx`: shared Provider/context。
- Create `web/src/pages/Attention/Attention.tsx`, `.module.css`, `index.ts`: desktop page。
- Modify `web/src/App.tsx`, `web/src/components/layout/AppShell/AppShell.tsx`: Provider、route、sidebar badge。
- Modify `web/src/pages/Workbench/workbenchDeepLink.ts`, `Workbench.tsx`: staged target application。
- Modify `web/src/pages/Orchestrator/Orchestrator.tsx`: task/outbox focus 与已解决回退。
- Modify `web/src/pages/Settings/Settings.tsx`: 响应 search params 变化。
- Create `web/src/mobile/components/MobileAttentionPanel.tsx`, `.module.css`: compact mobile page。
- Create `web/src/mobile/mobileAttentionTarget.ts`: semantic target→panel/project/entity mapper。
- Modify `web/src/mobile/MobileApp.tsx`, `MobileWorkbench.tsx`, `mobilePanelState.ts`, `components/MobileWorkbenchShell.tsx`, `components/MobileAutomationPanel.tsx`: Provider、导航第二项、目标跳转、原界面动作。
- Create `web/src/i18n/locales/{zh,en}/attention.json`; modify locale registration and nav translations。
- Create `web/tests/attention.spec.ts`: desktop/Mobile 关键 E2E。
- Modify `docs/prd.md`, `AGENTS.md`, `web/CLAUDE.md`, `src-tauri/CLAUDE.md`: 持久行为和边界。

## Shared Interfaces

Rust DTO 必须精确序列化为下面的 TypeScript 契约：

```ts
export type AttentionCategory = 'decision' | 'blocked' | 'environment';
export type AttentionFreshness = 'live' | 'cached';
export type AttentionSourceKind =
  | 'orchestratorHumanReview'
  | 'orchestratorBlocked'
  | 'remoteOutboxFailed'
  | 'workbenchDependency';

export type AttentionTarget =
  | { kind: 'orchestratorTask'; projectId: string; taskId: string }
  | { kind: 'remoteOutbox'; projectId: string; outboxId: string }
  | { kind: 'settings'; tab: 'dependencies' };

export interface AttentionItem {
  id: string;
  category: AttentionCategory;
  sourceKind: AttentionSourceKind;
  title: string;
  summary: string;
  updatedAt: string;
  freshness: AttentionFreshness;
  cachedAt: string | null;
  project: { id: string; name: string; kind: 'local' | 'remote' } | null;
  device: { id: string; name: string } | null;
  target: AttentionTarget;
}

export interface AttentionSnapshot {
  generatedAt: string;
  counts: { total: number; decision: number; blocked: number; environment: number };
  items: AttentionItem[];
}
```

Rust source/aggregator entrypoints:

```rust
pub(crate) trait AttentionSource: Send + Sync {
    fn collect<'a>(
        &'a self,
        state: &'a AppState,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<AttentionItemDto>, AppError>>;
}

pub async fn list_attention_items_for_state(
    state: &AppState,
) -> Result<AttentionSnapshotDto, AppError>;
```

Provider contract:

```ts
export interface AttentionContextValue {
  snapshot: AttentionSnapshot | null;
  loading: boolean;
  refreshing: boolean;
  stale: boolean;
  error: Error | null;
  lastSucceededAt: string | null;
  refresh: () => Promise<void>;
}
```

Stable IDs are exact and shared by tests:

```text
orchestrator:human-review:<taskId>
orchestrator:blocked:<taskId>
orchestrator:outbox-failed:<outboxId>
workbench:dependency:tmux
```

For remote tasks, `<taskId>` is the existing wrapped `remote:<deviceId>:<innerTaskId>` so IDs remain unique and navigable across devices.

---

### Task 1: Complete the Failed Outbox State Machine

**Files:**
- Modify: `src-tauri/src/orchestrator/outbox.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`

- [ ] **Step 1: Write repository tests for Retry/Discard before changing the enum**

Cover failed→pending, failed→discarded, pending/sending/mirrored/discarded rejection, concurrent duplicate action, preserved `request_json`/`clientRequestId`, retry clearing `last_error`, discard preserving `last_error`, and discarded exclusion from active/dispatcher queries.

- [ ] **Step 2: Run the focused failing tests**

```bash
cd src-tauri
cargo test --locked orchestrator::repo::tests::retry_failed_remote_outbox
cargo test --locked orchestrator::repo::tests::discard_failed_remote_outbox
```

Expected: compile failure because the methods/status do not exist.

- [ ] **Step 3: Add `RemoteOutboxStatus::Discarded`**

Update `as_str`/`from_str` and DTO conversion. Do not add a new database column or migration; the existing text status column stores `discarded`.

- [ ] **Step 4: Implement atomic transitions**

```rust
pub async fn retry_failed_remote_outbox_item(
    &self,
    item_id: &str,
) -> Result<OrchestratorRemoteOutboxRow, AppError>;

pub async fn discard_failed_remote_outbox_item(
    &self,
    item_id: &str,
) -> Result<OrchestratorRemoteOutboxRow, AppError>;
```

Each `UPDATE` uses `WHERE id = ? AND status = 'failed'`. Retry updates only status, last_error and updated_at; it never rewrites `request_json`. Discard updates status/updated_at and retains the failure details. On zero updated rows, read current state and return not-found or invalid-transition distinctly.

- [ ] **Step 5: Run outbox/repository tests and commit**

```bash
cd src-tauri
cargo test --locked orchestrator::repo --lib
cargo test --locked orchestrator::outbox --lib
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/orchestrator/outbox.rs src-tauri/src/orchestrator/repo.rs
git commit -m "feat: close failed remote outbox lifecycle"
```

---

### Task 2: Expose Outbox Actions in the Original Automation UI

**Files:**
- Modify: `src-tauri/src/commands/orchestrator.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Modify: `web/src/mobile/components/MobileAutomationPanel.tsx`
- Modify: `web/src/api/orchestrator.test.ts`
- Modify: `web/src/api/workbenchHttp.test.ts`
- Modify: `web/src/pages/Orchestrator/orchestratorActions.test.ts`
- Modify: `web/src/mobile/MobileAutomationPanel.test.ts`

- [ ] **Step 1: Add command/route contract tests**

Test project ownership, missing outbox, wrong project shortcut, failed-only transition and camelCase DTO. The outbox row lives on the current cc-partner device and must not be proxied recursively to the remote owner.

- [ ] **Step 2: Implement Tauri helpers and commands**

```rust
#[tauri::command]
pub async fn retry_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError>;

#[tauri::command]
pub async fn discard_orchestrator_remote_outbox(
    state: State<'_, AppState>,
    project_id: String,
    outbox_id: String,
) -> Result<OrchestratorRemoteOutboxDto, AppError>;
```

- [ ] **Step 3: Add Mobile HTTP routes**

Register `POST /api/orchestrator/outbox/retry` and `/api/orchestrator/outbox/discard` with body `{projectId,outboxId}` and reuse the same state helpers.

- [ ] **Step 4: Add actions only to failed outbox rows in desktop/mobile Automation**

Show Retry and Discard only when status is `failed`. Discard requires a confirmation in the original UI. Pending/sending/mirrored/discarded render no actions. A successful action reloads the task-view list; Attention invalidation is added in Task 10.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri
cargo test --locked commands::orchestrator --lib
cargo test --locked net::routes::orchestrator --lib
cd ../web
npm test -- orchestratorActions MobileAutomationPanel workbenchHttp
npm run build
git -C "$(git rev-parse --show-toplevel)" add src-tauri web/src
git commit -m "feat: add remote outbox retry and discard actions"
```

---

### Task 3: Give tmux Dependency a Stable Change Time

**Files:**
- Modify: `src-tauri/src/workbench/dependencies.rs`
- Modify: `src-tauri/src/commands/workbench_dependencies.rs`
- Modify: `web/src/lib/types.ts`

- [ ] **Step 1: Add failing transition-time tests**

Assert initial status has a timestamp, same enum/status payload preserves it, actual `checking→ready` or `checking→failed` changes it, and repeated read/poll does not reset it.

- [ ] **Step 2: Implement process-local `status_changed_at`**

Store it beside the cached dependency status inside the manager. Update only when the semantic status enum changes. Serialize as `statusChangedAt`; do not persist to SQLite or trigger dependency probing from Attention collection.

- [ ] **Step 3: Verify and commit**

```bash
cd src-tauri
cargo test --locked workbench::dependencies --lib
cargo test --locked commands::workbench_dependencies --lib
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/workbench/dependencies.rs src-tauri/src/commands/workbench_dependencies.rs web/src/lib/types.ts
git commit -m "feat: track workbench dependency state changes"
```

---

### Task 4: Define Attention Models and Deterministic Aggregation

**Files:**
- Create: `src-tauri/src/attention/mod.rs`
- Create: `src-tauri/src/attention/models.rs`
- Create: `src-tauri/src/attention/source.rs`
- Create: `src-tauri/src/attention/aggregator.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Write DTO serialization tests**

Assert every enum/tag serializes to the TypeScript literals above, target fields are camelCase, `cachedAt` is nullable, and no backend URL is present.

- [ ] **Step 2: Write aggregator tests with fake sources**

Cover source concatenation; stable-ID dedupe; category order `decision→blocked→environment`; `updatedAt` descending; equal-time ID tie-break; count consistency; one source error failing the whole aggregate; generatedAt only after all sources succeed.

- [ ] **Step 3: Implement models/source trait/aggregator**

The aggregator receives source objects for tests and production. On duplicate ID, require item equality; conflicting duplicates return an integrity error instead of silently choosing one.

- [ ] **Step 4: Run tests and commit**

```bash
cd src-tauri
cargo test --locked attention::models::tests
cargo test --locked attention::aggregator::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/attention src-tauri/src/lib.rs
git commit -m "feat: add deterministic attention aggregation"
```

---

### Task 5: Project Orchestrator Attention Sources

**Files:**
- Create: `src-tauri/src/attention/orchestrator_source.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Reuse: `src-tauri/src/commands/orchestrator.rs` remote ID/mirror helpers without copying their semantics

- [ ] **Step 1: Write local projection tests**

Human Review produces `decision`; Blocked produces `blocked`; resolved/running/queued/retrying tasks are excluded; legacy blocked mapping is applied before projection. Assert the exact stable IDs/target IDs above and use the task's authoritative `updated_at`.

- [ ] **Step 2: Write outbox projection tests**

Only failed rows for an active remote shortcut produce items. pending/sending/mirrored/discarded and orphan rows are excluded. Item target uses the local shortcut project ID; updatedAt and summary use authoritative outbox fields.

- [ ] **Step 3: Write remote live/cached tests**

Online successful mirror refresh produces live items; network/offline failure reads the previous mirror and uses `last_synced_at` as `cachedAt`; corrupted mirror JSON, repository failure or invalid remote DTO fails the entire source. Remote task IDs use the existing `remote:<deviceId>:<inner>` mapping.

- [ ] **Step 4: Enforce a four-request concurrency ceiling**

Use `futures_util::stream::iter(...).buffer_unordered(4)`. A test peer counts in-flight requests and asserts the maximum never exceeds four across many remote projects.

- [ ] **Step 5: Implement projection and run tests**

Do not consume the final `list_orchestrator_task_views_for_state` DTO because it loses per-item freshness/cached time; reuse its online-sync/network-fallback control flow and repository helpers.

```bash
cd src-tauri
cargo test --locked attention::orchestrator_source::tests -- --nocapture
```

- [ ] **Step 6: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/attention/orchestrator_source.rs src-tauri/src/orchestrator/repo.rs
git commit -m "feat: project orchestrator attention sources"
```

---

### Task 6: Project the Workbench Dependency Source and Expose APIs

**Files:**
- Create: `src-tauri/src/attention/workbench_dependency_source.rs`
- Create: `src-tauri/src/commands/attention.rs`
- Create: `src-tauri/src/net/routes/attention.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/net/routes/mod.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Test dependency source inclusion/exclusion**

With zero Workbench projects, no item is produced for any dependency status. With at least one project, only missing/failed/unsupported produce `workbench:dependency:tmux`; ready/checking/installing are excluded. updatedAt equals `statusChangedAt`.

- [ ] **Step 2: Implement Tauri and HTTP adapters**

`list_attention_items()` and `GET /api/mobile/attention` both call `list_attention_items_for_state`. Serialize each response to `serde_json::Value` in a contract test and assert equality.

- [ ] **Step 3: Apply `attention.v1` capability**

Add `attention.v1` to protocol health in the same change that registers the HTTP route. Mobile client support tests must prove a legacy server is classified unsupported and does not get its response guessed from older endpoints. The HTTP route aggregates only the current backend's projects and may contact each remote owning device once; it never asks another device to aggregate recursively.

- [ ] **Step 4: Register command and route**

Add `list_attention_items` to `invoke_handler!`; add the GET route under the existing mobile router. Preserve the request ID/error envelope from the P2P plan.

- [ ] **Step 5: Verify and commit**

```bash
cd src-tauri
cargo test --locked attention::workbench_dependency_source::tests
cargo test --locked net::routes::attention::tests
cargo check --locked
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/attention src-tauri/src/commands src-tauri/src/net src-tauri/src/lib.rs
git commit -m "feat: expose attention snapshot APIs"
```

---

### Task 7: Build Shared Frontend Rules and Provider

**Files:**
- Modify: `web/src/lib/types.ts`
- Create: `web/src/lib/attention.ts`
- Create: `web/src/lib/attention.test.ts`
- Create: `web/src/api/attention.ts`
- Create: `web/src/api/attentionHttp.ts`
- Create: `web/src/api/attention.test.ts`
- Create: `web/src/hooks/attentionState.ts`
- Create: `web/src/hooks/attentionState.test.ts`
- Create: `web/src/hooks/attentionContext.ts`
- Create: `web/src/hooks/useAttention.tsx`
- Create: `web/src/hooks/useAttention.test.tsx`

- [ ] **Step 1: Add pure rule tests**

Test badge `0→null`, `1..99→number string`, `100+→99+`; three groups and empty-group omission; category/order protection; sourceKind→action i18n key; semantic desktop target mapping to the three approved URLs.

- [ ] **Step 2: Add reducer and async Provider tests**

Use deferred Promises to prove request 2 beats late request 1; unmount ignores resolution; first load/error states; successful snapshot; refresh keeps the snapshot and sets stale/error; success clears stale; hidden document pauses the 10-second interval; focus/visible/manual refresh trigger loads.

- [ ] **Step 3: Implement APIs and provider**

Desktop loader invokes `list_attention_items`. Mobile loader checks health `attention.v1` then GETs `/api/mobile/attention`. Provider accepts `loadSnapshot` prop so both surfaces share the exact state machine. It refreshes on first mount, focus, visible transition and manual request, and polls every 10 seconds only while visible; it registers effects once and cleans all listeners/timers.

- [ ] **Step 4: Verify hooks-before-returns and types**

```bash
cd web
npm test -- attention
npx tsc --noEmit
```

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add web/src/lib web/src/api web/src/hooks
git commit -m "feat: add shared attention provider"
```

---

### Task 8: Implement the Desktop Inbox and Deep Links

**Files:**
- Create: `web/src/pages/Attention/Attention.tsx`
- Create: `web/src/pages/Attention/Attention.module.css`
- Create: `web/src/pages/Attention/index.ts`
- Create: `web/src/pages/Attention/attentionView.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`
- Modify: `web/src/pages/Workbench/workbenchDeepLink.ts`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`
- Modify: `web/src/pages/Settings/Settings.tsx`
- Modify: `web/src/i18n/index.ts` and `web/src/i18n/locales/{zh,en}/{nav,attention}.json`

- [ ] **Step 1: Add view contract tests first**

Cover skeleton/no badge, first error/reload, empty state without celebration/metrics, hidden empty groups, cached label with cachedAt, stale banner preserving list, row and explicit 44×44 action control, semantic headings/list structure, no assertive whole-list live region, no direct side-effect action, and automatic removal without toast/focus/scroll changes.

- [ ] **Step 2: Add `/attention` and sidebar entry**

Mount desktop Provider above AppShell. Add the NavItem immediately after Home. Reuse its badge prop; zero/null hides it and 100+ renders `99+`.

- [ ] **Step 3: Implement the approved independent page**

Title “待处理”, subtitle “只保留会阻塞工作继续的事项”, groups “需要你的决定/运行受阻/环境受阻”. Action labels are fixed by source: “前往复核”“查看阻塞原因”“查看失败项”“打开设置”. Fixed UI labels come from `attention` namespace; title/summary fields are treated as authoritative content. Use existing tokens and Button/Nav primitives; add no Inbox-specific color system.

- [ ] **Step 4: Extend deep links and staged consumption**

Parse/build:

```text
/workbench?projectId=<id>&view=automation&taskId=<id>
/workbench?projectId=<id>&view=automation&outboxId=<id>
/settings?tab=dependencies
```

Workbench applies project first, then automation view, then task/outbox after data loads. Task opens details/Evidence, not terminal. Settings watches `location.search`, including changes while already mounted.

- [ ] **Step 5: Handle a resolved/missing target**

Orchestrator reports a typed target-not-found result to the page coordinator. Show “事项已解决或状态已变化”, call `refresh()`, and navigate back to `/attention`; never render an empty detail or open a terminal.

- [ ] **Step 6: Verify and commit**

```bash
cd web
npm test -- attentionView workbenchDeepLink settingsState orchestratorActions
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src
git commit -m "feat: add desktop global inbox"
```

---

### Task 9: Implement the Mobile Inbox and Target Mapper

**Files:**
- Create: `web/src/mobile/components/MobileAttentionPanel.tsx`
- Create: `web/src/mobile/components/MobileAttentionPanel.module.css`
- Create: `web/src/mobile/MobileAttentionPanel.test.tsx`
- Create: `web/src/mobile/mobileAttentionTarget.ts`
- Create: `web/src/mobile/mobileAttentionTarget.test.ts`
- Modify: `web/src/mobile/MobileApp.tsx`
- Modify: `web/src/mobile/mobilePanelState.ts`
- Modify: `web/src/mobile/components/MobileWorkbenchShell.tsx`
- Modify: `web/src/mobile/MobileWorkbench.tsx`
- Modify: `web/src/mobile/components/MobileAutomationPanel.tsx`

- [ ] **Step 1: Lock navigation behavior with tests**

Default remains `projects`; order begins `projects→attention→automation→terminal`; Attention badge uses identical helper; selecting Attention closes the drawer; nonzero items never auto-select the panel or add a permanent top-bar button.

- [ ] **Step 2: Implement compact grouped list**

Reuse shared grouping/action/freshness helpers, not desktop JSX. Keep headings, reason, project/device, time and cached text. Tap target is at least 44×44 and category/freshness have visible text.

- [ ] **Step 3: Implement semantic target mapping**

`orchestratorTask` selects project→automation→task detail/Evidence; `remoteOutbox` selects project→automation→outbox row; settings selects Settings dependencies. Reuse existing Automation/Settings components; do not create mobile duplicate details.

- [ ] **Step 4: Handle missing target and capability states**

Missing/solved target refreshes and returns to Attention. Legacy backend without `attention.v1` displays unsupported, not empty/error guess. Offline/refresh failures with an existing snapshot retain items and stale status.

- [ ] **Step 5: Verify and commit**

```bash
cd web
npm test -- mobilePanelState mobileAttention MobileAutomationPanel
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/mobile web/src/api/attentionHttp.ts
git commit -m "feat: add mobile global inbox"
```

---

### Task 10: Wire Immediate Invalidation, E2E, and Project Memory

**Files:**
- Modify: desktop/mobile Orchestrator action success paths
- Modify: Workbench dependency refresh/install success paths
- Create: `web/tests/attention.spec.ts`
- Modify: `docs/prd.md`
- Modify: `AGENTS.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Add a single invalidation bridge**

After Deliver, Request Rework, task Retry/Refresh, outbox Retry/Discard and dependency install/recheck succeed, await or trigger `AttentionContext.refresh()`. Failed actions do not invalidate. The 10-second poll remains only a remote/external fallback.

- [ ] **Step 2: Test all required E2E scenarios**

Use deterministic test fixtures/API fakes for: Human Review appears then Rework removes it; Blocked opens detail/Evidence; only failed outbox appears and Retry/Discard removes it; online remote live→offline cached→online revalidated; tmux hidden with zero projects then shown with a project; stale refresh preserves count/list; missing target returns to Inbox. Run the page assertions under both light and dark themes and verify keyboard focus-visible navigation.

- [ ] **Step 3: Verify desktop/mobile count parity**

For one fixture snapshot, assert desktop sidebar and mobile nav render the same total and the same group ordering. Do not duplicate the count calculation in surface code.

- [ ] **Step 4: Update persistent documentation**

`docs/prd.md` records real-time projection, v1 sources, no history, desktop/mobile entry and cached behavior. Root `AGENTS.md` adds the Attention page to the directory map and adds any new reusable domain component to the component list. `web/CLAUDE.md` records Provider/deep-link/invalidation rules and Vitest/E2E commands. `src-tauri/CLAUDE.md` records no Inbox table, source error policy, four-request cap and capability.

- [ ] **Step 5: Run final verification**

```bash
cd src-tauri
cargo test --locked attention::
cargo test --locked orchestrator::repo
cargo test --locked net::routes::attention
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cd ../web
npm test
npm run lint
npm run build
npm run test:e2e -- --project=chromium
```

Expected: all commands exit `0`; desktop/mobile fixtures show equal totals; solved items disappear after the successful mutation without waiting for polling.

- [ ] **Step 6: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add docs/prd.md AGENTS.md web/CLAUDE.md src-tauri/CLAUDE.md web/tests web/src src-tauri/src
git commit -m "test: verify global inbox workflows"
```

## Completion Contract

- 不存在 Inbox table、read/dismiss/snooze/history；投影只反映当前权威阻塞。
- 四类 v1 source 的进入/退出和排除条件全部有 Rust 测试。
- failed outbox 在原界面可安全 Retry/Discard，且保留幂等键与审计语义。
- 桌面和移动端对相同 snapshot 显示相同 badge、分类和排序。
- 本机成功动作立即刷新；可见时 10 秒轮询反映外部变化；旧请求不能覆盖新请求。
- cached 远端条目保留真实 `last_synced_at`，不会伪装 live；意外 source 错误不会返回误导性部分快照。
- 所有条目只导航到现有任务详情/outbox/Settings；已解决目标有明确回退。
- Rust、Vitest、Playwright、lint、build 与分层文档全部通过。
