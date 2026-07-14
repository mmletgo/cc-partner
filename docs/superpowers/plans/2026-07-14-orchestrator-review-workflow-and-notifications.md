# Orchestrator Review, Workflow and Notifications Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 Human Review 提供有界只读 diff 与交付前漂移保护，为 WORKFLOW.md 提供安全向导，并用去重系统通知把关键状态导航回权威界面。

**Architecture:** 把 verifier 已有 worktree diff 收集提炼为 bounded snapshot helper，展示 patch 可截断但 review digest 基于完整 canonical tree/content identity；desktop/remote/mobile 共享 command。WORKFLOW 由后端 parser/validator 单一权威并用 expected hash 保存。sidecar 发出全局版本化 operational events，GUI 使用桌面插件当前支持的标准 informational notification；Attention/deep link 继续负责导航。

**Tech Stack:** Rust 2021, Git CLI helpers, Tauri 2 notification plugin 2.3.3, React 19/TypeScript, existing Orchestrator/Attention/Workbench APIs, Vitest/E2E.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-orchestrator-review-workflow-and-notifications-design.md`、`docs/superpowers/specs/2026-07-05-orchestrator-design.md`、`docs/superpowers/specs/2026-07-05-orchestrator-remote-autonomous-verification-loop-design.md`、`docs/superpowers/specs/2026-07-06-symphony-inspired-workbench-automation-design.md`、`web/CLAUDE.md` 与 `src-tauri/CLAUDE.md`。
- diff 只读：≤200 files、总 patch ≤2 MiB、单文件 ≤256 KiB；binary 无 patch。
- base/head 从 task attempt/worktree 派生，拒绝任意 repo path/ref 输入。
- Deliver 前 diff digest 漂移必须 Conflict；不得交付未审变更。
- WORKFLOW.md 不能启用/改变 delivery；通知/Attention 只导航不执行业务动作。
- 通知可见文案不含任务标题、项目名、goal/acceptance/diff/evidence/终端正文；只显示通用状态。本轨道主动不使用/不承诺尚未完成三平台认证的 `onAction`/action type/extra deep link，系统通知只提醒。

---

## File Structure

- Create: `src-tauri/src/orchestrator/review_diff.rs`。
- Modify: `src-tauri/src/orchestrator/{mod,verifier,models,delivery,workflow}.rs`。
- Modify: `src-tauri/src/commands/orchestrator/{actions,evidence,tasks,tests}.rs`。
- Modify: `src-tauri/src/net/routes/orchestrator.rs`, `src-tauri/src/orchestrator/remote_client.rs`, `src-tauri/src/orchestrator/remote_protocol.rs`。
- Modify: `web/src/lib/types/orchestrator.ts` and runtime schemas。
- Modify: `web/src/api/orchestrator.ts` and tests。
- Modify: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`。
- Modify: `web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.tsx`, `web/src/pages/Orchestrator/Orchestrator.module.css`; create `web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.test.tsx`。
- Modify: `web/src/mobile/components/MobileAutomationTaskDetail.tsx`, `web/src/mobile/MobileAutomationPanel.test.ts`。
- Modify: `web/src/mobile/controllers/useMobileAutomationController.ts`, `web/src/api/workbenchHttp.ts`, `web/src/api/workbenchHttp.test.ts` for remote/mobile diff transport state。
- Create: `web/src/pages/Orchestrator/views/WorkflowWizardDialog.tsx`, `web/src/pages/Orchestrator/views/WorkflowWizardDialog.module.css`, `web/src/pages/Orchestrator/views/WorkflowWizardDialog.test.tsx`。
- Create: `web/src/hooks/useOperationalNotifications.ts`, `web/src/hooks/useOperationalNotifications.test.tsx`。
- Modify: `web/src/lib/notification.ts`, `web/src/App.tsx`, `web/src/pages/Settings/AutomationSettingsPanel.tsx`, `web/src/pages/Settings/automationSettingsState.ts`, `web/src/pages/Settings/useSettingsController.ts`, `web/src/pages/Settings/Settings.test.tsx`, `web/src/api/orchestratorConfig.ts`。
- Modify: `src-tauri/src/orchestrator/{models,outbox}.rs`, `src-tauri/src/orchestrator/repo/{schema,tasks}.rs`, `src-tauri/src/backend/event_bus.rs` for versioned global operational events。

## Shared Interfaces

```rust
pub struct OrchestratorReviewDiff {
    pub task_id: String,
    pub base_ref: String,
    pub head_ref: String,
    pub files: Vec<ReviewDiffFile>,
    pub total_files: u32,
    pub truncated: bool,
    pub review_digest: String,
}

pub struct WorkflowDocument {
    pub status: WorkflowDocumentStatus,
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub diagnostics: Vec<WorkflowDiagnostic>,
}
```

### Task 1: Extract a Bounded Review Diff Snapshot from the Verifier

**Files:**
- Create: `src-tauri/src/orchestrator/review_diff.rs`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/verifier.rs`
- Modify: `src-tauri/src/orchestrator/models.rs`
- Test: `src-tauri/src/orchestrator/review_diff.rs`

**Interfaces:** Produces `collect_review_diff(task,attempt,project) -> OrchestratorReviewDiff`; verifier text rendering consumes the same snapshot.

- [ ] **Step 1: Write staged/unstaged/untracked/binary/truncation tests**

```rust
#[tokio::test]
async fn review_diff_truncates_single_patch_and_keeps_metadata() {
    let repo = DiffFixture::with_text_file("large.txt", 300 * 1024).await;
    let diff = repo.collect().await.unwrap();
    assert!(diff.files[0].truncated);
    assert!(diff.files[0].patch.as_ref().unwrap().len() <= 256 * 1024);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::review_diff::tests`

Expected: FAIL because module does not exist.

- [ ] **Step 3: Extract and bound existing diff collection**

Use existing Git command runner/path validation and preserve its staged/unstaged/untracked semantics, including unborn repositories. Canonically sort repo-relative entries. The display DTO remains bounded, but `review_digest` is computed independently from base/tree identity plus path/status/mode/old blob oid/new streaming content hash; never hash only truncated/emitted patch bytes. Add a test where only the hidden tail changes and digest must change.

- [ ] **Step 4: Verify verifier output unchanged**

Run: `cd src-tauri && cargo test --locked orchestrator::review_diff && cargo test --locked orchestrator::verifier`

Expected: PASS; existing verifier evidence remains semantically equivalent.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/review_diff.rs src-tauri/src/orchestrator/mod.rs src-tauri/src/orchestrator/verifier.rs src-tauri/src/orchestrator/models.rs
git commit -m "feat(orchestrator): collect bounded review diffs"
```

### Task 2: Expose Local/Remote/Mobile Review Diff APIs

**Files:**
- Modify: `src-tauri/src/commands/orchestrator/evidence.rs`
- Modify: `src-tauri/src/commands/orchestrator/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/orchestrator/remote_client.rs`
- Modify: `src-tauri/src/orchestrator/remote_protocol.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Test: `src-tauri/src/commands/orchestrator/tests.rs` and inline route tests in `src-tauri/src/net/routes/orchestrator.rs`

**Interfaces:** Produces `get_orchestrator_review_diff(project_id,task_id)`; remote owner derives local project/worktree.

- [ ] **Step 1: Write authorization-by-task-context and resource-limit tests**

```rust
#[tokio::test]
async fn review_diff_rejects_task_outside_human_review_or_rework() {
    let err = get_review_diff(state(), todo_task_id()).await.unwrap_err();
    assert_eq!(err.code(), "review_diff_unavailable");
}
```

Add path escape, remote shortcut owner mapping, unsupported capability and an invoke-registration contract proving the Tauri command appears in `generate_handler!`.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked review_diff_unavailable`

Expected: FAIL because command/route is absent.

- [ ] **Step 3: Implement shared command helper and routes**

Desktop local calls helper; remote shortcut forwards to owning device; mobile calls remote-aware wrapper. Register exact capability `orchestrator.review-diff.v1` and bounded DTO schema without accepting arbitrary repo path/ref.

- [ ] **Step 4: Verify routes and inventory**

Run: `cd src-tauri && cargo test --locked commands::orchestrator && cargo test --locked net::routes::orchestrator && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/orchestrator/evidence.rs src-tauri/src/commands/orchestrator/mod.rs src-tauri/src/commands/orchestrator/tests.rs src-tauri/src/lib.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/protocol.rs src-tauri/src/orchestrator/remote_client.rs src-tauri/src/orchestrator/remote_protocol.rs
git commit -m "feat(orchestrator): expose review diff snapshots"
```

### Task 3: Render Review Changes and Enforce Deliver Digest

**Files:**
- Modify: `web/src/lib/types/orchestrator.ts`
- Modify: `web/src/lib/schemas/orchestrator.ts`
- Modify: `web/src/lib/schemas/orchestrator.test.ts`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/api/orchestrator.test.ts`
- Modify: `web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.tsx`
- Modify: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`
- Modify: `web/src/mobile/components/MobileAutomationTaskDetail.tsx`
- Modify: `web/src/mobile/controllers/useMobileAutomationController.ts`
- Modify: `web/src/api/workbenchHttp.ts`
- Modify: `web/src/api/workbenchHttp.test.ts`
- Create: `web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.test.tsx`
- Modify: `web/src/mobile/MobileAutomationPanel.test.ts`
- Modify: `src-tauri/src/orchestrator/delivery.rs`
- Modify: `src-tauri/src/commands/orchestrator/tasks.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/orchestrator/remote_client.rs`
- Modify: `src-tauri/src/orchestrator/remote_protocol.rs`
- Modify: `web/src/i18n/locales/zh/orchestrator.json`
- Modify: `web/src/i18n/locales/en/orchestrator.json`

**Interfaces:** Controller stores review key `{projectId,taskId,attemptId,requestSeq,digest}`; local and remote Deliver request carries `expectedReviewDigest`. Mobile is explicitly inspection-only in this track and shows an accessible desktop-completion notice.

- [ ] **Step 1: Write drawer and drift tests**

```ts
test('diff error leaves evidence and review actions available', async () => {
  mockReviewDiff.mockRejectedValue(new Error('unavailable'))
  render(<OrchestratorTaskDrawer {...makeTaskDrawerProps({ selectedTask: humanReviewTask, reviewDiffState: 'error' })} />)
  expect(await screen.findByText('unavailable')).toBeVisible()
  expect(screen.getByRole('button', { name: '交付' })).toBeDisabled()
  expect(screen.getByRole('button', { name: '要求返工' })).toBeEnabled()
  expect(screen.getByText('Evidence')).toBeVisible()
})
```

Add A→B inverse diff resolution, attempt change, task reload and Conflict→re-review tests. Rust test: modify worktree after review and expect `review_diff_changed` conflict on both local and owning-device remote Deliver.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- Orchestrator && cd ../src-tauri && cargo test --locked review_diff_changed`

Expected: FAIL.

- [ ] **Step 3: Add Summary/Changes/Evidence and rework dialog**

Changes loads lazily, renders file metadata/patch with truncation and binary markers, and mounts only the selected patch. Summary/Changes/Evidence reuse roving-tab helpers with `tablist/tab/tabpanel`, one tab stop, Arrow/Home/End and selected-panel focus/scroll. Controller clears/aborts review state on project/task/attempt change and accepts a response only when the full review key/requestSeq is current. On a diff-capable owner, Deliver stays disabled until the user confirms the current digest; capability-unsupported legacy peer alone retains old Deliver. Rework reason is required at 1–2000 chars and remains on failure. Mobile controller owns `idle/loading/ready/error/unsupported`; its API-free inspection view uses file-title buttons with `aria-expanded/aria-controls`, mounts only selected patch, shows local errors with `role=alert`, and states “请在桌面端完成审核” instead of exposing Deliver/Rework.

- [ ] **Step 4: Recollect digest before Deliver**

Backend propagates `expectedReviewDigest` through Tauri command, remote protocol/client and owning-device route, then recollects immediately before `start_delivery_from_human_review`. Conflict leaves task in Human Review and writes no delivery side effect.

- [ ] **Step 5: Verify and commit**

Run: `cd web && npm test -- Orchestrator MobileAutomationPanel && npm run check:i18n && cd ../src-tauri && cargo test --locked review_diff && cargo test --locked delivery && cargo test --locked net::routes::orchestrator`

```bash
git add web/src/lib/types/orchestrator.ts web/src/lib/schemas/orchestrator.ts web/src/lib/schemas/orchestrator.test.ts web/src/api/orchestrator.ts web/src/api/orchestrator.test.ts web/src/api/workbenchHttp.ts web/src/api/workbenchHttp.test.ts web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.tsx web/src/pages/Orchestrator/views/OrchestratorTaskDrawer.test.tsx web/src/pages/Orchestrator/controllers/useOrchestratorController.ts web/src/pages/Orchestrator/Orchestrator.module.css web/src/mobile/components/MobileAutomationTaskDetail.tsx web/src/mobile/controllers/useMobileAutomationController.ts web/src/mobile/MobileAutomationPanel.test.ts web/src/i18n/locales/zh/orchestrator.json web/src/i18n/locales/en/orchestrator.json src-tauri/src/orchestrator/delivery.rs src-tauri/src/commands/orchestrator/tasks.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/orchestrator/remote_client.rs src-tauri/src/orchestrator/remote_protocol.rs
git commit -m "feat(orchestrator): review diffs before delivery"
```

### Task 4: Add Authoritative WORKFLOW Document APIs

**Files:**
- Modify: `src-tauri/src/orchestrator/workflow.rs`
- Modify: `src-tauri/src/commands/orchestrator/tasks.rs`
- Modify: `src-tauri/src/commands/orchestrator/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/orchestrator/remote_protocol.rs`
- Modify: `src-tauri/src/orchestrator/remote_client.rs`
- Test: `src-tauri/src/orchestrator/workflow.rs`, `src-tauri/src/commands/orchestrator/tests.rs`, `src-tauri/src/net/routes/orchestrator.rs`

**Interfaces:** Produces capability `orchestrator.workflow-document.v1` with `get_workflow_document`, `validate_workflow_document`, `save_workflow_document(expected_hash,content)`.

- [ ] **Step 1: Write missing/valid/invalid/hash-conflict golden tests**

```rust
#[tokio::test]
async fn generated_template_round_trips_through_authoritative_parser() {
    let template = default_workflow_template();
    let parsed = parse_project_workflow(&template).unwrap();
    assert_eq!(parsed.active_states, vec![WorkflowState::Todo, WorkflowState::Rework]);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::workflow::tests::generated_template_round_trips`

Expected: FAIL because template/document APIs are absent.

- [ ] **Step 3: Implement document status, diagnostics and CAS save**

Diagnostics include line/column/code/message; validate uses existing parser. Save resolves project-owned root, rejects symlink/path escape, compares SHA-256 expected hash and writes atomically. Save does not dispatch.

- [ ] **Step 4: Verify local/remote routes**

Run: `cd src-tauri && cargo test --locked orchestrator::workflow && cargo test --locked commands::orchestrator && cargo test --locked net::routes::orchestrator && cargo test --locked command_registration`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/workflow.rs src-tauri/src/commands/orchestrator/tasks.rs src-tauri/src/commands/orchestrator/mod.rs src-tauri/src/commands/orchestrator/tests.rs src-tauri/src/lib.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/orchestrator/remote_protocol.rs src-tauri/src/orchestrator/remote_client.rs
git commit -m "feat(orchestrator): add workflow document APIs"
```

### Task 5: Build the WORKFLOW Wizard with Safe Save

**Files:**
- Create: `web/src/pages/Orchestrator/views/WorkflowWizardDialog.tsx`
- Create: `web/src/pages/Orchestrator/views/WorkflowWizardDialog.module.css`
- Create: `web/src/pages/Orchestrator/views/WorkflowWizardDialog.test.tsx`
- Modify: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`
- Modify: `web/src/api/orchestrator.ts`
- Modify: `web/src/pages/Workbench/workbenchDeepLink.ts`
- Modify: `web/src/pages/Workbench/workbenchDeepLink.test.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.test.tsx`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchFileController.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchFileController.test.tsx`
- Modify: `web/src/pages/Workbench/Workbench.tsx`
- Modify: `web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx`
- Modify: `web/src/i18n/locales/zh/orchestrator.json`
- Modify: `web/src/i18n/locales/en/orchestrator.json`
- Modify: `web/src/i18n/locales/zh/workbench.json`
- Modify: `web/src/i18n/locales/en/workbench.json`

**Interfaces:** Consumes Task 4 document APIs and N3 safe-save contract; extends typed Workbench deep link with `{ view:'files', path:'WORKFLOW.md' }` and hands it from automation controller to the existing file controller without adding an eighth Workbench controller.

- [ ] **Step 1: Write missing/invalid/conflict UI tests**

Assert missing shows template preview/create, valid shows parsed summary/open file, invalid focuses diagnostic line, hash conflict preserves draft and offers reload.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- WorkflowWizardDialog.test.tsx`

Expected: FAIL because dialog is absent.

- [ ] **Step 3: Implement Dialog flow using existing editor/file workspace**

Do not round-trip rewrite comments in an existing valid YAML. Add `openFileByPath(path)` to the existing file controller. Inject that bridge into the existing automation controller, which stages `view=files&path=WORKFLOW.md`, selects the authoritative project/worktree, then calls the file controller only after context is ready. Workbench only wires controllers; it does not add an eighth controller or inline open logic. Wizard validation/save uses expected hash and N3 safe-save behavior. Reject absolute/traversal paths and preserve Hooks-before-return/seven-controller contracts.

- [ ] **Step 4: Verify Orchestrator ownership and Workbench deep link**

Run: `cd web && npm test -- WorkflowWizardDialog.test.tsx orchestratorOwnership.test.ts workbenchDeepLink.test.ts useWorkbenchAutomationController.test.tsx useWorkbenchFileController.test.tsx WorkbenchProject.characterization.test.tsx && npm run check:i18n`

Expected: PASS; views do not import API directly.

- [ ] **Step 5: Commit**

```bash
git add web/src/pages/Orchestrator/views/WorkflowWizardDialog.tsx web/src/pages/Orchestrator/views/WorkflowWizardDialog.module.css web/src/pages/Orchestrator/views/WorkflowWizardDialog.test.tsx web/src/pages/Orchestrator/controllers/useOrchestratorController.ts web/src/api/orchestrator.ts web/src/pages/Workbench/workbenchDeepLink.ts web/src/pages/Workbench/workbenchDeepLink.test.ts web/src/pages/Workbench/controllers/useWorkbenchAutomationController.ts web/src/pages/Workbench/controllers/useWorkbenchAutomationController.test.tsx web/src/pages/Workbench/controllers/useWorkbenchFileController.ts web/src/pages/Workbench/controllers/useWorkbenchFileController.test.tsx web/src/pages/Workbench/Workbench.tsx web/src/pages/Workbench/WorkbenchProject.characterization.test.tsx web/src/i18n/locales/zh/orchestrator.json web/src/i18n/locales/en/orchestrator.json web/src/i18n/locales/zh/workbench.json web/src/i18n/locales/en/workbench.json
git commit -m "feat(orchestrator): add workflow readiness wizard"
```

### Task 6: Implement the Operational Notification Coordinator

**Files:**
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/orchestrator/outbox.rs`
- Modify: `src-tauri/src/orchestrator/repo/schema.rs`
- Modify: `src-tauri/src/orchestrator/repo/tasks.rs`
- Modify: `src-tauri/src/orchestrator/delivery.rs`
- Modify: `src-tauri/src/backend/event_bus.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/backend/ui.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/commands/orchestrator/tasks.rs`
- Modify: `src-tauri/src/commands/orchestrator/mod.rs`
- Modify: `src-tauri/src/commands/orchestrator_config.rs`
- Modify: `src-tauri/src/orchestrator/config.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/lib/notification.ts`
- Create: `web/src/api/operationalNotifications.ts`
- Create: `web/src/api/operationalNotifications.test.ts`
- Create: `web/src/hooks/useOperationalNotifications.ts`
- Create: `web/src/hooks/useOperationalNotifications.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/orchestratorConfig.ts`
- Modify: `web/src/pages/Settings/automationSettingsState.ts`
- Modify: `web/src/pages/Settings/useSettingsController.ts`
- Modify: `web/src/pages/Settings/AutomationSettingsPanel.tsx`
- Modify: `web/src/pages/Settings/Settings.test.tsx`
- Modify: `web/src/i18n/locales/zh/orchestrator.json`, `web/src/i18n/locales/en/orchestrator.json`, `web/src/i18n/locales/zh/settings.json`, `web/src/i18n/locales/en/settings.json`

**Interfaces:** Requires N1's created event bus/control API/client. Produces global owner event `OperationalNotificationEvent { kind, opaque_source_id, state_version, occurred_at }` and bounded privacy-safe `OperationalNotificationSnapshot { cursor, items, truncated }` for `humanReview/blocked/remoteOutboxFailed/taskDone`; GUI dedupe key `{kind,opaqueSourceId,stateVersion}`. This track intentionally uses informational notifications only, independent of plugin action exports.

- [ ] **Step 1: Write baseline/dedupe/frontmost/deep-link tests**

```ts
test('first snapshot establishes baseline without notification spam', async () => {
  renderHook(() => useOperationalNotifications(snapshotWithThreeItems()))
  expect(sendNotification).not.toHaveBeenCalled()
})
```

Add all four owner event kinds (including Done while another/no project is open), persistent state-revision dedupe, reconnect replay, first snapshot baseline, gap snapshot baseline, owner restart, same state no repeat, future revision after gap still notifies, foreground authority suppression, permission denied and generic privacy-safe title/body. Add a deferred snapshot fixture where events arrive after listener registration but before snapshot resolution; events `<= asOfCursor` become baseline and later buffered events notify exactly once. Add Gap/owner-change while snapshot is pending and listener registration failure tests. Assert task title/goal sentinels never appear and no actionType/extra/onAction registration occurs.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- useOperationalNotifications.test.tsx`

Expected: FAIL because coordinator is absent.

- [ ] **Step 3: Emit authoritative events and extend the notification wrapper**

Increment/persist a task state revision on relevant Orchestrator transitions; outbox failed uses its durable attempt/revision. Emit all four kinds through N1 sidecar event bus independent of Attention/current project. Add owner-only loopback+control-token snapshot and stream routes to `control_api.rs`, mount them through `net/http_server.rs`, and return at most 1,000 current opaque states with an `asOf` event cursor; snapshot capture retries until the event cursor is stable around the DB read. `BackendControlClient` owns reconnect/after-sequence calls; `backend/ui.rs` relays owner id/sequence/event/Gap into the GUI process as a Tauri event. Register `get_operational_notification_snapshot` through `commands/orchestrator/mod.rs` and `lib.rs`; `web/src/api/operationalNotifications.ts` is the only snapshot command wrapper. The GUI coordinator mounts inside providers and performs an explicit handshake: await Tauri listener registration first, buffer events by owner/sequence, fetch snapshot, establish it as no-notify baseline, drop buffered cursors at/before `asOfCursor`, then drain later cursors before switching to live mode. Gap/owner change pauses drain and repeats the handshake without unregistering the buffer listener. Use only `sendNotification({ title, body })`; title/body are fixed translated state labels, not source content. Do not call action APIs; Attention/in-app badges remain navigation.

- [ ] **Step 4: Add typed preferences and Settings controls**

Defaults: Human Review/Blocked/outbox failed on, Done off. Add these as controlled fields in the existing Orchestrator config form/state/controller; save through `orchestratorConfigApi` → `commands/orchestrator_config.rs` → N1 owner patch. `AutomationSettingsPanel` remains API-free. Permission is requested only from an explicit user action. Foreground authority updates Attention/badge without OS notification; App placement guarantees the coordinator is below providers.

- [ ] **Step 5: Verify and commit**

Run: `cd src-tauri && cargo test --locked operational_notification_event && cargo test --locked operational_notification_snapshot && cargo test --locked --test runtime_authority_smoke operational_notification_relay && cd ../web && npm test -- useOperationalNotifications.test.tsx operationalNotifications.test.ts automationSettingsState.test.ts Settings.test.tsx useAttention.test.tsx && npm run check:i18n && npm run build`

```bash
git add src-tauri/src/orchestrator/models.rs src-tauri/src/orchestrator/outbox.rs src-tauri/src/orchestrator/repo/schema.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/orchestrator/delivery.rs src-tauri/src/orchestrator/config.rs src-tauri/src/backend/event_bus.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/backend/runtime.rs src-tauri/src/backend/ui.rs src-tauri/src/net/http_server.rs src-tauri/src/commands/orchestrator/tasks.rs src-tauri/src/commands/orchestrator/mod.rs src-tauri/src/commands/orchestrator_config.rs src-tauri/src/lib.rs src-tauri/tests/runtime_authority_smoke.rs web/src/lib/notification.ts web/src/api/operationalNotifications.ts web/src/api/operationalNotifications.test.ts web/src/hooks/useOperationalNotifications.ts web/src/hooks/useOperationalNotifications.test.tsx web/src/App.tsx web/src/api/orchestratorConfig.ts web/src/pages/Settings/automationSettingsState.ts web/src/pages/Settings/useSettingsController.ts web/src/pages/Settings/AutomationSettingsPanel.tsx web/src/pages/Settings/Settings.test.tsx web/src/i18n/locales/zh/orchestrator.json web/src/i18n/locales/en/orchestrator.json web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json
git commit -m "feat(notifications): surface operational state changes"
```

### Task 7: Protocol, Docs, E2E, and Completion Gates

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/development/quality-matrix.json`
- Create: `web/tests/orchestrator-review-workflow.spec.ts`

- [ ] **Step 1: Add E2E journeys**

Cover Human Review diff→Rework, hidden-tail digest drift→Conflict, WORKFLOW invalid→fix→valid, informational notification send/dedupe, Attention deep link→authority, and ensure neither notification nor Attention row executes a business action.

- [ ] **Step 2: Run complete gates**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cd ../web
npm run lint
npm run build
npm test
npm run test:e2e
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 3: Inspect privacy and authority boundaries**

Confirm notification payload/logs omit task/project/diff/evidence/goal and register no desktop action callback; remote routes derive owner context; WORKFLOW cannot enable delivery.

- [ ] **Step 4: Update persistent behavior docs**

Document diff limits/digest, workflow CAS/diagnostics, notification defaults/deep links and non-goals.

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md docs/p2p-protocol.md docs/development/quality-matrix.json web/CLAUDE.md src-tauri/CLAUDE.md web/tests/orchestrator-review-workflow.spec.ts
git commit -m "docs: define review workflow and notifications"
```

## Rollback and Failure Containment

- diff/workflow routes 通过 capability 检测降级；旧 peer 沿用旧动作合同，新 peer 不得在缺少 reviewed digest 时绕过交付检查。
- 通知协调器可通过偏好全部关闭并安全 dispose listener；Attention 与 Orchestrator 权威状态不依赖系统通知。
- WORKFLOW 向导回退不改写或删除现有 `WORKFLOW.md`；CAS/原子写失败始终保留 draft。

## Completion Contract

- Human Review has bounded diff and delivery refuses unreviewed drift.
- WORKFLOW wizard uses authoritative parser/hash and never changes delivery control.
- system notifications are preference-controlled, owner-event-driven, deduped, privacy-safe and informational; Attention/deep links remain navigation-only.
- desktop/remote/mobile routes reuse one helper and all gates pass.

## Plan Self-Review

- Spec coverage: diff, digest, Rework, WORKFLOW, notifications, deep links and privacy each map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: diff/document/digest interfaces are shared across Rust and TS tasks.
