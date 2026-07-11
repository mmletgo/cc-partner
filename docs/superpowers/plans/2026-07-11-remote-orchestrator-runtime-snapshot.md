# Remote Orchestrator Runtime Snapshot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让远端项目的 Orchestrator runtime snapshot 来自 owning device，并在桌面与移动端清晰区分 live、offline、unsupported 和 unavailable。

**Architecture:** owning device 暴露 local-only、只读、capability-gated 的 `POST /api/orchestrator/runtime-snapshot`，直接复用现有本机 snapshot 构造逻辑。调用端映射 remote shortcut ID 后请求 owner；桌面 hook 与移动 store 各自保留最后一次成功的进程内显示缓存，离线时标旧但不持久化，也不让缓存进入 scheduler/验证/交付逻辑。

**Tech Stack:** Rust 2021, Tauri 2, axum, reqwest, React 19, TypeScript, Vitest, existing Orchestrator DTO and remote ID mapping.

## Global Constraints

- 前置依赖：先完成 `2026-07-11-p2p-protocol-metadata-errors.md`；调用新 route 必须通过 authoritative health capability `orchestrator.runtime-snapshot.v1`。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md`、`src-tauri/CLAUDE.md` 与 `web/CLAUDE.md`；新增业务函数/组件使用中文 Business Logic / Code Logic 注释。
- 本机 snapshot 行为和 DTO 不变；只提取/复用构造逻辑，不复制 scheduler 状态计算。
- route 只接受 owning device 的 local project；remote shortcut 递归请求必须拒绝。
- 远端 `generatedAt`、tick、slots、running/retrying 和 recent events 原样来自 owner；禁止用调用设备 telemetry 补空。
- desktop/mobile 缓存彼此独立，只在内存中用于显示；不写 SQLite、localStorage 或磁盘，不被任何业务动作消费。
- offline、unsupported、unavailable 是不同状态；404 不能被当作 capability 判断，localized error 文本不能驱动分支。
- 不改变 Orchestrator 页面整体视觉，只补真实状态与最后更新时间；hooks 位于所有 early return 前。

---

## Task Dependency Graph

最大并行 waves：`T1 → T2 → T3 → T4 → (T5 | T6) → T7 → T8`。T1–T4 依次建立 builder、route、client 与 command；桌面缓存和移动 store 在契约稳定后可并行，T7 汇合两端回归。

## File Structure

- Modify `src-tauri/src/commands/orchestrator.rs`: DTO Deserialize、纯 local snapshot helper、remote shortcut调用与状态映射。
- Modify `src-tauri/src/orchestrator/remote_protocol.rs`: `RemoteRuntimeSnapshotReq`。
- Modify `src-tauri/src/net/protocol.rs`: 增加并声明 `orchestrator.runtime-snapshot.v1`，与 route 同一提交落地。
- Modify `src-tauri/src/orchestrator/remote_client.rs`: capability-gated client method。
- Modify `src-tauri/src/net/routes/orchestrator.rs`: local-only runtime handler。
- Modify `src-tauri/src/net/http_server.rs`: register fixed POST route。
- Modify `web/src/lib/types.ts`: 增加 `OrchestratorRuntimeDisplayState`，复用既有四态 `OrchestratorRemoteRuntimeStatus`。
- Modify `web/src/api/orchestrator.ts`: existing Tauri snapshot adapter remains typed。
- Modify `web/src/api/workbenchHttp.ts`: Mobile runtime snapshot endpoint adapter。
- Create `web/src/hooks/useOrchestratorRuntimeSnapshot.ts` and test: desktop request/cache/stale state。
- Create `web/src/mobile/mobileRuntimeSnapshotStore.ts` and test: mobile in-memory cache reducer/store。
- Modify `web/src/pages/Orchestrator/Orchestrator.tsx`: consume desktop hook and render four states。
- Modify `web/src/mobile/components/MobileAutomationPanel.tsx`: consume mobile store and render same semantic statuses。
- Modify `src-tauri/CLAUDE.md`, `web/CLAUDE.md`, `docs/prd.md`: persistent remote runtime behavior。

## Shared Contracts

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteRuntimeSnapshotReq {
    #[serde(rename = "project_id")]
    pub project_id: String,
}
```

Route:

```text
POST /api/orchestrator/runtime-snapshot
{"project_id":"<owning-device-local-project-id>"}
```

The request body intentionally uses `project_id` exactly as approved; success body is the existing `OrchestratorRuntimeSnapshotDto` camelCase serialization.

Remote display state:

```ts
export type OrchestratorRemoteRuntimeStatus =
  | 'live'
  | 'unsupported'
  | 'offline'
  | 'unavailable';

export interface OrchestratorRuntimeDisplayState {
  snapshot: OrchestratorRuntimeSnapshot | null;
  remoteStatus: OrchestratorRemoteRuntimeStatus | null;
  cachedAt: string | null;
  loading: boolean;
  error: Error | null;
}
```

---

### Task 1: Make the Existing Local Snapshot Builder Reusable

**Files:**
- Modify: `src-tauri/src/commands/orchestrator.rs`

- [ ] **Step 1: Extend current local tests before refactoring**

Lock the full DTO for a local project: project ID/kind, generatedAt shape, tick, slot totals, running/retrying attempts and recent events. Add a repository/scheduler failure test.

- [ ] **Step 2: Extract one local-only helper**

Keep or refactor `get_orchestrator_runtime_snapshot_for_project` so commands and HTTP route both call it. Its input must resolve a verified local `WorkbenchProject`; it must never branch on remote shortcuts internally.

- [ ] **Step 3: Derive `Deserialize` on the existing DTO graph**

Add only the derives required by remote client success parsing. Do not rename or add response fields in this task.

- [ ] **Step 4: Prove behavior is unchanged**

```bash
cd src-tauri
cargo test --locked commands::orchestrator::tests::runtime_snapshot
```

Expected: all previous and new local snapshot tests pass with identical values except generated timestamps asserted structurally.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/commands/orchestrator.rs
git commit -m "refactor: share local orchestrator runtime snapshot builder"
```

---

### Task 2: Add the Owning-Device Route

**Files:**
- Modify: `src-tauri/src/orchestrator/remote_protocol.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/net/http_server.rs`

- [ ] **Step 1: Write route contract tests**

Cover valid local project, unknown project, blank project ID, and a `remote:<device>:` shortcut passed recursively. Assert success matches the local helper DTO and errors use stable P2P envelope/request ID.

- [ ] **Step 2: Add request model and handler**

Deserialize exactly `{"project_id":"..."}`. Resolve the project on the serving device, require `project.kind == local`, then call the shared builder. Never call `remote_client` from this handler.

- [ ] **Step 3: Register the fixed route**

Add `POST /api/orchestrator/runtime-snapshot` under the P2P router. Add the capability constant to `net/protocol.rs` and advertise it in `server_protocol_info()` in the same change; a contract test asserts the route and capability ship together.

- [ ] **Step 4: Verify and commit**

```bash
cd src-tauri
cargo test --locked net::routes::orchestrator::tests::runtime_snapshot
cargo test --locked net::routes::health::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/orchestrator/remote_protocol.rs src-tauri/src/net/protocol.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/http_server.rs
git commit -m "feat: expose owning-device runtime snapshot"
```

---

### Task 3: Add the Capability-Gated Remote Client

**Files:**
- Modify: `src-tauri/src/orchestrator/remote_client.rs`

- [ ] **Step 1: Write client fixture tests**

Use hit counters to prove: capability present calls the route; capability absent returns `Unsupported` without a route hit; network failure returns `Offline`; invalid/error response returns `Unavailable`/typed remote error; success preserves owner fields exactly.

- [ ] **Step 2: Implement client method**

```rust
pub async fn runtime_snapshot(
    &self,
    base_url: &str,
    project_id: &str,
    request_id: &str,
) -> Result<OrchestratorRuntimeSnapshotDto, PeerCallError>;
```

Call `require_capability(..., CAPABILITY_ORCHESTRATOR_RUNTIME_SNAPSHOT_V1)` first, send `X-CC-Request-Id`, serialize the approved snake_case request body, and use the shared v0/v1 error parser.

- [ ] **Step 3: Verify and commit**

```bash
cd src-tauri
cargo test --locked orchestrator::remote_client::tests::runtime_snapshot
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/orchestrator/remote_client.rs
git commit -m "feat: fetch remote orchestrator runtime snapshots"
```

---

### Task 4: Resolve Remote Shortcuts in the Tauri Command

**Files:**
- Modify: `src-tauri/src/commands/orchestrator.rs`

- [ ] **Step 1: Replace the old unsupported-only tests with four-state tests**

Cover live success, capability missing, peer offline and invalid/unavailable response. Add ID-mapping assertions for project/task/worktree/session IDs and a regression that no local telemetry values appear in a remote response.

- [ ] **Step 2: Implement remote project dispatch**

For local project, use unchanged local helper. For remote shortcut, resolve the owning device/base URL and inner local project ID via existing Workbench remote project helpers, call `runtime_snapshot`, then map the returned DTO for the shortcut surface.

- [ ] **Step 3: Map only identity/surface metadata**

Set outer `projectId` to the local shortcut ID, `projectKind='remote'`, `remoteStatus='live'`; wrap entity IDs with existing `remote:<device>:` helpers. Preserve owner generatedAt, tick, slots, attempt values and events. For unsupported/offline/unavailable return the existing DTO empty-state shape with the exact status and no local runtime data.

- [ ] **Step 4: Run command tests and commit**

```bash
cd src-tauri
cargo test --locked commands::orchestrator::tests::remote_runtime_snapshot
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/commands/orchestrator.rs
git commit -m "feat: resolve remote runtime snapshots through owner"
```

---

### Task 5: Add Desktop In-Memory Cache and Stale Guard

**Files:**
- Create: `web/src/hooks/useOrchestratorRuntimeSnapshot.ts`
- Create: `web/src/hooks/useOrchestratorRuntimeSnapshot.test.tsx`
- Modify: `web/src/pages/Orchestrator/Orchestrator.tsx`

- [ ] **Step 1: Write hook tests first**

Cover local live load; remote live load; project switch old-response discard; remote live→offline preserves last success with `cachedAt`; unsupported and unavailable do not reuse another project cache; cold-start offline has null snapshot; unmount cleanup; refresh success replaces cache.

- [ ] **Step 2: Implement module-local Map cache**

Key by exact project shortcut ID. Store only last successful live snapshot and client receipt time. Do not persist or export the Map. Request sequence/mounted refs prevent stale writes.

- [ ] **Step 3: Consume the hook in Orchestrator**

Remove the current local-only load gate and hardcoded remote unavailable rendering. Show cached content only for offline after a prior success, labeled with last update; unsupported/offline cold/unavailable use distinct existing-panel empty states. Actions/scheduler never receive cached snapshot.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- useOrchestratorRuntimeSnapshot orchestratorRemote
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/hooks web/src/pages/Orchestrator/Orchestrator.tsx
git commit -m "feat: show remote runtime snapshots on desktop"
```

---

### Task 6: Add the Mobile Transport and In-Memory Store

**Files:**
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Create: `web/src/mobile/mobileRuntimeSnapshotStore.ts`
- Create: `web/src/mobile/mobileRuntimeSnapshotStore.test.ts`
- Modify: `web/src/mobile/components/MobileAutomationPanel.tsx`
- Modify: `web/src/api/workbenchHttp.test.ts`
- Modify: `web/src/mobile/MobileAutomationPanel.test.ts`

- [ ] **Step 1: Lock the Mobile HTTP contract**

Register `POST /api/mobile/orchestrator/runtime-snapshot` with body `{projectId}`. The mobile browser calls this remote-aware route on its connected backend; the handler reuses the same state helper as Tauri. Do not expose the owning device's P2P base URL to the browser.

- [ ] **Step 2: Write store tests**

Mirror desktop semantics but use a separate module-local Map: per-project success cache, stale request guard, live→offline cache, cold offline empty, unsupported/unavailable distinction, and no localStorage calls.

- [ ] **Step 3: Implement transport/store and Mobile panel rendering**

Add typed load method, wire it when project/Automation panel changes, and render the same semantic statuses and last update using mobile layout. Cached values are display-only and cannot enable task actions.

- [ ] **Step 4: Verify and commit**

```bash
cd web
npm test -- mobileRuntimeSnapshot MobileAutomationPanel workbenchHttp
npm run lint
npm run build
git -C "$(git rev-parse --show-toplevel)" add web/src/api/workbenchHttp.ts web/src/mobile src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/http_server.rs
git commit -m "feat: show remote runtime snapshots on mobile"
```

---

### Task 7: Contract, Cache, and No-Substitution Regression

**Files:**
- Modify: tests inside `src-tauri/src/net/routes/orchestrator.rs`
- Modify: tests inside `src-tauri/src/orchestrator/remote_client.rs`
- Modify: tests inside `src-tauri/src/commands/orchestrator.rs`
- Modify: `web/src/hooks/useOrchestratorRuntimeSnapshot.test.tsx`
- Modify: `web/src/mobile/mobileRuntimeSnapshotStore.test.ts`
- Modify: `web/src/mobile/MobileAutomationPanel.test.ts`

- [ ] **Step 1: Run mixed v0/v1 contract tests**

Prove new client+v0 server returns unsupported without feature-route call; new client+v1 server returns live; invalid v1 payload is unavailable; network failure is offline.

- [ ] **Step 2: Prove owner data is preserved end-to-end**

Seed unique owner generatedAt/tick/slot/event values not present locally, pass through route→client→command→TypeScript fixture, and assert exact equality after only ID/surface mapping.

- [ ] **Step 3: Prove caches are non-persistent and display-only**

Search for runtime cache writes to storage/DB/localStorage and test cold initialization. Assert action availability derives from task DTO/state, not cached runtime snapshot.

- [ ] **Step 4: Run focused full verification**

```bash
cd src-tauri
cargo test --locked commands::orchestrator::tests
cargo test --locked net::routes::orchestrator::tests
cargo test --locked orchestrator::remote_client::tests
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cd ../web
npm test -- orchestratorRuntimeSnapshot mobileRuntimeSnapshot orchestratorRemote MobileAutomationPanel
npm run lint
npm run build
```

- [ ] **Step 5: Commit regression coverage**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src web/src
git commit -m "test: verify remote runtime ownership and caching"
```

---

### Task 8: Update Persistent Product and Engineering Documentation

**Files:**
- Modify: `docs/prd.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: Update PRD only after implementation passes**

Replace the old “remote runtime unsupported” statement with owning-device live data, the four statuses, process-only display cache and cold-offline behavior. State that cached runtime is never used for execution decisions.

- [ ] **Step 2: Update backend/frontend boundaries**

Backend doc records route body, local-only guard, capability, ID mapping and no-recursion rule. Frontend doc records separate desktop/mobile caches, stale guard, no persistence and hooks-before-returns.

- [ ] **Step 3: Verify documentation matches code**

```bash
rg -n "runtime-snapshot|orchestrator.runtime-snapshot.v1|unsupported|offline|unavailable" docs/prd.md src-tauri/CLAUDE.md web/CLAUDE.md src-tauri/src web/src
```

Review each match; no document may claim hosted/remote behavior not covered by the tests above.

- [ ] **Step 4: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add docs/prd.md src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: record remote runtime snapshot behavior"
```

## Completion Contract

- Local runtime snapshot is unchanged and uses one authoritative builder.
- The fixed P2P route accepts only owning-device local projects and rejects remote recursion.
- Capability absence, offline transport and invalid/unavailable runtime are separately typed and rendered.
- Online remote data comes from owner with exact runtime values; only IDs/surface metadata are mapped.
- Desktop and Mobile each keep a separate, in-memory, display-only last-success cache; cold offline has no fabricated data.
- No local telemetry can substitute for a remote snapshot, and cached data cannot affect scheduler/actions.
- Rust/Vitest/lint/build and persistent documentation verification pass.
