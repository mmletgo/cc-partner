# Workspace Safe Restore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 自动保存Workbench结构状态，并在重启/切换后通过纯读preflight和仅复用既有资源的safe attach恢复现场，不创建terminal/worktree、不写terminal、不启动或resume Agent。

**Architecture:** 控制设备本地sidecar SQLite保存layout引用；`preflight_workspace_restore`只读取project/worktree/session/tmux/browser元数据并生成带revision的计划，`apply`只允许前端selection变化和后端幂等safe attach。Remote layout仍存控制设备，owning device只接收local inner IDs做preflight/attach，避免递归remote shortcut。

**Tech Stack:** Rust 2021, sqlx SQLite, existing Workbench repositories/tmux registry/remote client/browser preview, Tauri commands, React 19, Vitest fake timers, Playwright.

## Global Constraints

- auto slot固定`desktop:auto`，`schemaVersion=1`，保存500ms debounce；无project时既不写空layout也不删除旧layout。
- layout不得包含terminal bytes、Prompt/response、文件正文、env、token、命令、provider配置或preview ID。
- preflight必须side-effect-free；禁止调用可能restore/spawn的现有session list路径。
- safe attach只接受persisted tmux且目标已存在；禁止`tmux new-session/new-window`、raw PTY fallback、terminal input、Claude/Codex resume。
- dirty editor内容不被layout读取或覆盖；Mobile v1不自动应用Desktop layout。
- remote route只处理owner local project；fixed LAN API不增加身份认证、token或权限模式。
- 完全恢复静默；partial只出现一条可关闭inline notice；不新增Sidebar入口或第八个Workbench controller。
- 命名snapshot仅保存结构metadata，不包含可执行配方，不实现全局Quick Open或用户Diff审查。

---

## File Structure

- Create: `src-tauri/src/workbench/workspace_layout.rs`。
- Create: `src-tauri/src/workbench/workspace_restore.rs`。
- Create: `src-tauri/src/storage/workbench_workspace_layout_repo.rs`。
- Create: `src-tauri/src/commands/workbench/layout.rs`。
- Modify: `src-tauri/src/{workbench/mod.rs,storage/mod.rs,state.rs}`、`src-tauri/src/backend/runtime.rs` and Workbench command registration.
- Modify: `src-tauri/src/workbench/{sessions.rs,remote_client.rs}`。
- Modify: `src-tauri/src/net/routes/workbench.rs`, `src-tauri/src/net/{protocol.rs,discovery.rs,http_server.rs}`。
- Modify: `src-tauri/src/backend/{control_workbench.rs,control_client.rs}`。
- Modify: `src-tauri/migrations/0001_init.sql`; new repo `ensure_schema` is initialized from `src-tauri/src/backend/runtime.rs`.
- Create: `web/src/pages/Workbench/{workspaceLayout.ts,workspaceLayout.test.ts,workspaceRestore.ts,workspaceRestore.test.ts}`。
- Create: `web/src/pages/Workbench/views/{WorkspaceRestoreNotice.tsx,WorkspaceRestoreNotice.test.tsx,WorkspaceSnapshotDialog.tsx,WorkspaceSnapshotDialog.test.tsx}`。
- Modify: `web/src/pages/Workbench/controllers/{useWorkbenchProjectController.ts,useWorkbenchTerminalController.ts,useWorkbenchFileController.ts}`、`web/src/pages/Workbench/{Workbench.tsx,Workbench.module.css}`、`web/src/api/{workbench.ts,workbenchHttp.ts,workbenchTransport.ts}`、`web/src/lib/types/workbench.ts`、`web/src/lib/schemas/workbench.ts`。
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchTransport.ts}` and tests.
- Modify: `docs/prd.md`, `docs/p2p-protocol.md`, `docs/development/{testing.md,quality-matrix.json}`, `web/CLAUDE.md`, `src-tauri/CLAUDE.md`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → T5 → T6 → T7
          ↘ remote T4 ↗
```

### Task 1: Persist Versioned Layouts with Revision CAS

**Files:**
- Create: `src-tauri/src/workbench/workspace_layout.rs`
- Create: `src-tauri/src/storage/workbench_workspace_layout_repo.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Produces: `WorkspaceLayout`, closed enums, `WorkspaceLayoutDraft`, `WorkspaceLayoutRepo::{get_by_slot,save_cas,list_named,delete_named}`.

- [ ] **Step 1: Write schema, enum, validation, and CAS tests**

```rust
#[tokio::test]
async fn stale_layout_revision_cannot_overwrite_newer_selection() {
    let repo = layout_repo().await;
    let first = repo.save_cas(auto_draft("p1"), None).await.unwrap();
    let second = repo.save_cas(auto_draft("p2"), Some(first.revision)).await.unwrap();
    let error = repo.save_cas(auto_draft("p3"), Some(first.revision)).await.unwrap_err();
    assert_eq!(error.code(), "workspace_layout_conflict");
    assert_eq!(repo.get_by_slot("desktop:auto").await.unwrap().unwrap().project_id, "p2");
    assert_eq!(second.revision, first.revision + 1);
}
```

Add upgrade-from-existing-database, unique slot, unknown schema/enum, invalid named slot/name, invalid browser URL, and serialization tests proving forbidden fields are absent.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::workspace_layout --lib && cargo test --locked storage::workbench_workspace_layout_repo --lib`

Expected: FAIL because table/models/repo are absent.

- [ ] **Step 3: Add the additive table and typed repository**

Create `workbench_workspace_layouts(id,slot_key UNIQUE,kind,name,schema_version,project_id,active_worktree_id,active_session_id,workspace_view,inspector_tab,browser_target_url,revision,created_at,updated_at)`. Validate `desktop:auto` versus `named:<uuid>` and normalize the browser URL through the existing loopback target helper before persistence.

CAS must execute one transaction with `WHERE revision = expectedRevision`; create requires no existing slot. Unknown schema fails closed. Named delete may only target `kind=named`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench::workspace_layout --lib && cargo test --locked storage::workbench_workspace_layout_repo --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/workspace_layout.rs src-tauri/src/workbench/mod.rs src-tauri/src/storage src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql
git commit -m "feat(workbench): persist versioned workspace layouts"
```

### Task 2: Build a Side-effect-free Restore Preflight

**Files:**
- Create: `src-tauri/src/workbench/workspace_restore.rs`
- Modify: `src-tauri/src/storage/{workbench_project_repo.rs,workbench_worktree_repo.rs,workbench_session_repo.rs}`
- Modify: `src-tauri/src/workbench/sessions.rs`

**Interfaces:**
- Produces: `WorkspaceRestorePlan`, `WorkspaceRestoreAction`, `RestoreSkipReason`, `preflight_workspace_restore`.

- [ ] **Step 1: Write stale-resource and zero-side-effect tests**

```rust
#[tokio::test]
async fn preflight_skips_missing_tmux_without_spawning_or_writing() {
    let fixture = restore_fixture().persisted_tmux("s1").tmux_target_absent().await;
    let plan = fixture.preflight().await.unwrap();
    assert!(plan.actions.iter().any(|a| a.reason() == Some(RestoreSkipReason::TmuxTargetMissing)));
    assert_eq!(fixture.tmux_new_session_count(), 0);
    assert_eq!(fixture.tmux_new_window_count(), 0);
    assert_eq!(fixture.terminal_write_count(), 0);
    assert_eq!(fixture.agent_spawn_count(), 0);
}
```

Cover missing project/worktree/session, worktree ownership mismatch, raw PTY running/exited, existing registry session, existing tmux target, invalid loopback target, layout revision changed, and unknown schema.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::workspace_restore --lib`

Expected: FAIL because restore plan/preflight are absent.

- [ ] **Step 3: Implement pure inspection adapters and deterministic plan order**

Add read-only repo lookup methods that never call terminal restoration. Produce actions only from `select|reuse|safeAttach|skip` in project→worktree→session→view→inspector→browser order. Carry `layout_id`, `layout_revision`, a random `restore_id`, resolved IDs, and bounded reason codes; do not include absolute remote paths.

Raw PTY always yields `skip`; an already registered terminal yields `reuse`; only persisted tmux with an observed existing target yields `safeAttach`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench::workspace_restore --lib`

Expected: PASS with all mutation counters zero.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/workspace_restore.rs src-tauri/src/workbench/sessions.rs src-tauri/src/storage/workbench_project_repo.rs src-tauri/src/storage/workbench_worktree_repo.rs src-tauri/src/storage/workbench_session_repo.rs
git commit -m "feat(workbench): preflight safe workspace restore"
```

### Task 3: Implement Idempotent tmux Safe Attach

**Files:**
- Modify: `src-tauri/src/workbench/workspace_restore.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Test: inline tests in `src-tauri/src/workbench/{workspace_restore.rs,sessions.rs}`.

**Interfaces:**
- Produces: `safe_attach_workbench_session`, `SafeAttachResult`, reuse of existing `RestoreClaimGuard`.

- [ ] **Step 1: Write idempotency, claim, and forbidden-operation tests**

```rust
#[tokio::test]
async fn concurrent_safe_attach_creates_one_attach_client_only() {
    let fixture = restore_fixture().existing_tmux_target("s1").await;
    let (a, b) = tokio::join!(fixture.safe_attach("s1"), fixture.safe_attach("s1"));
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(fixture.attach_client_count(), 1);
    assert_eq!(fixture.tmux_new_session_count(), 0);
    assert_eq!(fixture.terminal_write_count(), 0);
}
```

Add wrong backend, missing target between preflight/apply, cancelled caller claim cleanup, existing registry reuse, raw PTY rejection, and Agent resume count=0.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workspace_restore --lib && cargo test --locked safe_attach --lib`

Expected: FAIL because safe attach entrypoint is absent.

- [ ] **Step 3: Add a narrow attach-only tmux primitive**

Recheck persisted session/backend/target under the restore claim. Reuse an existing registry handle when present; otherwise create only the attach client/registry wiring required to observe the existing tmux window. Do not call generic create/restore helpers that contain raw PTY fallback or `tmux new-*`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workspace_restore --lib && cargo test --locked safe_attach --lib`

Expected: PASS; forbidden-operation counters stay zero.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench
git commit -m "feat(workbench): attach existing tmux sessions safely"
```

### Task 4: Expose Local and Owning-device Restore APIs

**Files:**
- Create: `src-tauri/src/commands/workbench/layout.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/{control_workbench.rs,control_client.rs}`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/{protocol.rs,discovery.rs,http_server.rs}`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: desktop invoke/control layout CRUD/preflight/apply; P2P owner-local preflight/safe-attach; capability `workbench.workspace-safe-restore.v1`.

- [ ] **Step 1: Write local/remote ownership and apply-CAS tests**

```rust
#[tokio::test]
async fn remote_restore_rejects_remote_shortcut_recursion() {
    let state = restore_route_fixture().await;
    let error = post_preflight(&state, remote_project_id("d2", "p2")).await.unwrap_err();
    assert_eq!(error.code(), "local_project_required");
}
```

Cover unknown capability, offline owner, inner ID mapping, body/resource limits, wrong project/session ownership, changed layout revision before apply, duplicate apply, and P2P response fields excluding layout name/absolute path.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked commands::workbench --lib && cargo test --locked net::routes::workbench --lib && cargo test --locked workbench::remote_client --lib`

Expected: FAIL because APIs/capability are absent.

- [ ] **Step 3: Wire shared service entrypoints**

Local commands expose `get/save/list/delete/preflight/apply`. `apply` verifies plan/layout revision and only executes listed `safeAttach` plus bounded browser preview creation. Remote route accepts local inner project/worktree/session IDs only, invokes owner-local preflight/attach, and never reads or stores the controller's layout row.

For an unsupported old peer, return a structured partial plan that permits controller-local project selection and skips deeper items; do not silently invoke old generic restore routes.

- [ ] **Step 4: Run GREEN and protocol inventory**

Run:

```bash
cd src-tauri && cargo test --locked commands::workbench --lib && cargo test --locked net::routes::workbench --lib && cargo test --locked workbench::remote_client --lib && cargo test --locked net::protocol --lib
node scripts/check-p2p-route-inventory.mjs
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/workbench src-tauri/src/backend src-tauri/src/workbench/remote_client.rs src-tauri/src/net src-tauri/src/lib.rs
git commit -m "feat(workbench): expose safe restore transport"
```

### Task 5: Add Pure Frontend Draft and Restore Coordinators

**Files:**
- Create: `web/src/pages/Workbench/{workspaceLayout.ts,workspaceLayout.test.ts,workspaceRestore.ts,workspaceRestore.test.ts}`
- Modify: `web/src/lib/types/workbench.ts`
- Modify: `web/src/lib/schemas/{workbench.ts,workbench.test.ts}`
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchHttp.test.ts,workbenchTransport.ts}`

**Interfaces:**
- Produces: `buildWorkspaceLayoutDraft`, `WorkspaceLayoutAutosaveCoordinator`, `applyWorkspaceRestorePlan`.

- [ ] **Step 1: Write debounce, CAS, ordering, and rollback tests**

```tsx
it('coalesces stable selection changes and excludes content events', async () => {
  const fixture = layoutCoordinatorFixture()
  fixture.selectProject('p1')
  fixture.selectWorktree('w1')
  fixture.emitTerminalOutput('secret')
  fixture.advance(499)
  expect(fixture.saved()).toHaveLength(0)
  fixture.advance(1)
  expect(fixture.saved()).toEqual([expect.objectContaining({ projectId: 'p1', activeWorktreeId: 'w1' })])
  expect(JSON.stringify(fixture.saved())).not.toContain('secret')
})
```

Add no-project behavior, timer/pane/Agent event exclusion, conflict reread+recompute, outcome-unknown get/revision reconciliation, preflight-before-selection, apply action order, dirty editor preservation, frontend exception restoring previous selection, and one combined partial result.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- workspaceLayout.test.ts workspaceRestore.test.ts workbench.test.ts`

Expected: FAIL because coordinator/schema/API are absent.

- [ ] **Step 3: Implement transport-neutral pure coordinators**

`buildWorkspaceLayoutDraft` accepts only stable IDs/view/inspector/normalized target. Autosave owns one 500ms timer and revision; conflict reads current layout and rebuilds from current selectors rather than replaying the old draft. Restore captures previous selection, waits for preflight, applies approved steps sequentially, and returns one summary without rendering UI.

- [ ] **Step 4: Run GREEN**

Run: `cd web && npm test -- workspaceLayout.test.ts workspaceRestore.test.ts workbench.test.ts workbenchHttp.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench/workspaceLayout.ts web/src/pages/Workbench/workspaceLayout.test.ts web/src/pages/Workbench/workspaceRestore.ts web/src/pages/Workbench/workspaceRestore.test.ts web/src/lib web/src/api
git commit -m "feat(workbench): coordinate workspace save and restore"
```

### Task 6: Integrate Low-noise Workbench UI Without a New Controller

**Files:**
- Create: `web/src/pages/Workbench/views/{WorkspaceRestoreNotice.tsx,WorkspaceRestoreNotice.test.tsx,WorkspaceSnapshotDialog.tsx,WorkspaceSnapshotDialog.test.tsx}`
- Modify: `web/src/pages/Workbench/controllers/{useWorkbenchProjectController.ts,useWorkbenchTerminalController.ts,useWorkbenchFileController.ts}` and the Browser Workspace bridge in `web/src/pages/Workbench/Workbench.tsx`.
- Modify: `web/src/pages/Workbench/{Workbench.tsx,Workbench.module.css}`
- Modify: `web/src/pages/Workbench/{WorkbenchProject.characterization.test.tsx,WorkbenchTerminal.characterization.test.tsx,WorkbenchOverlays.characterization.test.tsx}`

**Interfaces:**
- Produces: silent automatic restore, one partial notice, secondary named snapshot entry.

- [ ] **Step 1: Write behavior, accessibility, and architecture tests**

```tsx
it('keeps a complete automatic restore silent and summarizes partial restore once', async () => {
  const complete = renderWorkbenchWithRestore(completePlan())
  await complete.ready()
  expect(complete.queryByRole('status')).toBeNull()

  const partial = renderWorkbenchWithRestore(partialPlan(3, 2))
  await partial.ready()
  expect(partial.getAllByRole('status')).toHaveLength(1)
  expect(partial.getByText('已恢复 3 项，2 项已跳过')).toBeVisible()
})
```

Add expandable bounded reason codes, dismiss, reduced motion, keyboard/focus behavior, named save/apply/delete confirmation, no command editor, no Mobile auto-apply, no new controller export, and `Workbench.tsx` line-count≤1200.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkspaceRestoreNotice WorkspaceSnapshotDialog WorkbenchProject.characterization`

Expected: FAIL because UI integration is absent.

- [ ] **Step 3: Integrate through existing controller bridges**

Project controller initiates load/preflight and autosave; terminal/file/browser controllers expose only existing selection/focus/preview callbacks. Views receive props only and import no `@/api/*`. Reuse `Dialog`, `Button`, `Pill` and current design tokens; keep the successful path invisible and place snapshots in a secondary Workbench action, not Sidebar or Project Rail decoration.

- [ ] **Step 4: Run GREEN, lint, and build**

Run:

```bash
cd web && npm test -- WorkspaceRestoreNotice WorkspaceSnapshotDialog WorkbenchProject.characterization workspaceRestore.test.ts workspaceLayout.test.ts
cd web && npm run lint
cd web && npm run build
```

Expected: PASS; no hook after early return and `Workbench.tsx` remains≤1200 lines.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Workbench web/src/api web/src/lib
git commit -m "feat(workbench): restore workspace with low-noise UI"
```

### Task 7: Verify Restart, Remote Partial, and Privacy Boundaries

**Files:**
- Create: `web/tests/workspace-safe-restore.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/{testing.md,quality-matrix.json}`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`

**Interfaces:**
- Produces: evidence for restart restore, zero forbidden side effects, and remote ownership.

- [ ] **Step 1: Add E2E scenarios and privacy fixture scan**

Cover local restart with existing tmux, stale session partial, remote owner offline, old peer unsupported, full success silence, dirty editor unchanged, and named snapshot CRUD. Instrument terminal write, tmux create, worktree create and Agent spawn counters and require all to remain zero on restore.

- [ ] **Step 2: Run RED**

Run: `cd web && npm run test:e2e -- workspace-safe-restore.spec.ts`

Expected: FAIL until app restart fixture and UI flow are wired.

- [ ] **Step 3: Update persistent behavior and protocol docs**

Document metadata-only layout, 500ms autosave, preflight/safe-attach, remote owner boundary, capability, partial notice and explicit forbidden side effects. Add stable quality IDs; unexecuted OS-specific evidence stays `NOT VERIFIED`.

- [ ] **Step 4: Run final focused gates**

Run:

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets --locked -- -D warnings
cd src-tauri && cargo test --locked workspace_restore --lib
cd web && npm run lint && npm run build
cd web && npm test -- workspaceLayout.test.ts workspaceRestore.test.ts WorkspaceRestoreNotice WorkspaceSnapshotDialog
cd web && npm run test:e2e -- workspace-safe-restore.spec.ts
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add docs web src-tauri
git commit -m "docs(workbench): verify safe workspace restore"
```

## Completion Contract

- Layout schema/revision CAS and metadata exclusion are tested.
- Preflight has observable zero terminal write, Agent spawn, worktree create and tmux create calls.
- Safe attach only reuses an existing tmux target and is concurrent-idempotent.
- Remote layout remains on the controller; owner route rejects remote shortcut recursion.
- Successful automatic restore is silent; partial restoration produces one notice.
- Dirty editor state survives restore and Mobile does not auto-apply Desktop layout.
- Workbench has no eighth controller, no new Sidebar entry, and stays within line/hook architecture guards.
- Rust/frontend/E2E/protocol/docs gates pass.

## Plan Self-review

- Persistence, preflight, safe attach, transport and UI each have separate evidence surfaces.
- Restore cannot become an execution recipe because the model has no command/content/provider fields.
- CAS and unknown-outcome reconciliation prevent blind overwrite/replay.
- The normal path removes manual re-selection without adding setup, prompts or recurring maintenance.
