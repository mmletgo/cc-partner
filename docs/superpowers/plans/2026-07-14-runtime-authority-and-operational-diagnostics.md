# Runtime Authority and Operational Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 sidecar 成为配置、Cloud Sync、全部本机/远端 Workbench runtime、Orchestrator telemetry 和后台 bridge 的唯一运行时权威，并向 GUI 提供可验证的本机控制面与脱敏诊断。

**Architecture:** 在现有 loopback + control-file-token 生命周期边界上增加 versioned control API、owner identity 和配置 CAS。GUI 仅保留 UI/OS 集成并代理运行时命令；sidecar event bus relay 为 Tauri event。PTY/restore/bridge 使用 RAII 与 cancellation，避免 ghost resource。

**Tech Stack:** Rust 2021, Tauri 2, axum 0.7, tokio, reqwest, sqlx/SQLite, serde, Vitest/React for Settings diagnostics.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-runtime-authority-and-operational-diagnostics-design.md`、根 `AGENTS.md` 与 `src-tauri/CLAUDE.md`。
- 合法 LAN 业务 API 继续无身份鉴权；control API 仅 loopback + control token，不创建配对或业务 capability。
- sidecar 是唯一 `HeadlessOwner`；GUI 不得 fallback 到第二份本地 runtime 执行 mutation。
- 配置成功顺序固定为 validate allowlist patch → existing transactional writer/spawn_blocking atomic save → sync memory swap → generation increment；禁止提交完整 stale AppConfig。
- 不记录 Prompt、文件、终端文本、远端 URL 凭据或 control token。
- 每个任务 TDD、focused verification、独立 commit。

---

## File Structure

- Create: `src-tauri/src/backend/authority.rs` — runtime role、owner descriptor、generation types。
- Create: `src-tauri/src/backend/control_api.rs` — loopback control handlers。
- Create: `src-tauri/src/backend/control_client.rs` — GUI control-file client。
- Create: `src-tauri/src/backend/event_bus.rs` — sidecar event relay。
- Modify: `src-tauri/src/backend/{mod,control,cli,runtime}.rs`。
- Modify: `src-tauri/src/{config_runtime,state,lib}.rs`。
- Modify: `src-tauri/src/commands/{config,cloud_sync,claude_md,orchestrator_config,health,github_trending}.rs`。
- Modify: all `src-tauri/src/commands/workbench/{common,projects,files,git,browser,sessions,tests}.rs` adapters。
- Modify: `src-tauri/src/workbench/{sessions,remote_client,remote_protocol,remote_events}.rs`。
- Modify: `src-tauri/src/commands/orchestrator/{tasks,common}.rs`。
- Create: `src-tauri/tests/runtime_authority_smoke.rs`。
- Modify: `web/src/pages/Settings/SettingsDependenciesPanel.tsx` and tests。

## Shared Interfaces

```rust
pub enum RuntimeRole { HeadlessOwner, GuiClient }

pub struct RuntimeOwnerDescriptor {
    pub schema_version: u32,
    pub owner_instance_id: String,
    pub generation: u64,
}

pub struct ConfigUpdateRequest {
    pub expected_owner_instance_id: String,
    pub expected_generation: u64,
    pub patch: RuntimeConfigPatch,
}
```

`RuntimeConfigPatch` uses `deny_unknown_fields` and an explicit allowlist; GUI theme/window and N4 `gui-bootstrap.json` never enter this DTO. Small control JSON body ≤256 KiB and ordinary metadata response ≤1 MiB; event lines retain the separate 1 MiB stream limit. Workbench data-plane routes are separate: text files ≤5 MiB, image/HTML preview ≤10 MiB and browser bodies ≤32 MiB, using streamed/binary bodies rather than JSON/base64 amplification.

### Task 1: Establish Runtime Role and Versioned Control Descriptor

**Files:**
- Create: `src-tauri/src/backend/authority.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/backend/control.rs`
- Modify: `src-tauri/src/backend/cli.rs`
- Test: `src-tauri/src/backend/control.rs`

**Interfaces:** Produces `RuntimeRole`, `RuntimeOwnerDescriptor`, and `BackendControlFile.owner_instance_id/schema_version` for all later tasks.

- [ ] **Step 1: Write failing serialization and stale-file tests**

```rust
#[test]
fn control_file_round_trips_owner_descriptor() {
    let mut file = BackendControlFile::for_test(1, 62116, "device-a");
    file.control_schema_version = 2;
    file.owner_instance_id = Some("owner-a".to_string());
    let value = serde_json::to_value(&file).unwrap();
    assert_eq!(value["controlSchemaVersion"], 2);
    assert_eq!(value["ownerInstanceId"], "owner-a");
}

#[test]
fn legacy_control_file_is_stale_not_authoritative() {
    let legacy = serde_json::json!({
        "pid": 1,
        "port": 62116,
        "controlToken": "x",
        "deviceId": "device-a",
        "deviceName": "Desk A",
        "startedAt": "2026-07-14T00:00:00Z"
    });
    let parsed: BackendControlFile = serde_json::from_value(legacy).unwrap();
    assert!(classify_control_descriptor(&parsed).needs_restart());
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked backend::control::tests::control_file_round_trips_owner_descriptor`

Expected: FAIL because descriptor fields/helpers do not exist.

- [ ] **Step 3: Implement role and descriptor**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRole { HeadlessOwner, GuiClient }

impl RuntimeRole {
    pub fn require_owner(self) -> Result<(), AppError> {
        match self {
            Self::HeadlessOwner => Ok(()),
            Self::GuiClient => Err(AppError::conflict("runtime_owner_required")),
        }
    }
}
```

Generate one UUID owner id in sidecar startup and write it to the control file. Add serde defaults for `control_schema_version` and optional owner so legacy JSON deserializes before classification; mark missing/old fields stale without logging the token. Keep this version out of `server_protocol_info()`.

- [ ] **Step 4: Run focused tests**

Run: `cd src-tauri && cargo test --locked backend::control::tests && cargo test --locked backend::authority::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/authority.rs src-tauri/src/backend/mod.rs src-tauri/src/backend/control.rs src-tauri/src/backend/cli.rs
git commit -m "feat(backend): define runtime owner authority"
```

### Task 2: Add Owner Generation and Config CAS

**Files:**
- Modify: `src-tauri/src/config_runtime.rs`
- Modify: `src-tauri/src/state.rs`
- Create: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Test: `src-tauri/src/config_runtime.rs`

**Interfaces:** Consumes Task 1 descriptor. Produces `ConfigSnapshot`, allowlisted `RuntimeConfigPatch`, `ConfigUpdateRequest`, `ConfigUpdateResponse`, `RuntimeOwnerStatus`.

- [ ] **Step 1: Write failing CAS tests**

```rust
#[tokio::test]
async fn concurrent_expected_generation_allows_one_writer() {
    let runtime = test_config_runtime("owner-a", 0).await;
    let first = runtime.apply_patch_if_generation("owner-a", 0, patch_name("first"));
    let second = runtime.apply_patch_if_generation("owner-a", 0, patch_name("second"));
    let (a, b) = tokio::join!(first, second);
    assert_eq!([a.is_ok(), b.is_ok()].into_iter().filter(|v| *v).count(), 1);
    assert_eq!(runtime.snapshot_with_generation().generation, 1);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked config_runtime::tests::concurrent_expected_generation_allows_one_writer`

Expected: FAIL because generation/CAS is absent.

- [ ] **Step 3: Implement generation inside the existing transactional writer**

Extend `update_config_transactionally`/`update_lock` rather than creating a second writer. Under the async update lock: verify owner/generation, apply the allowlist patch to the current value, validate, call the existing blocking-safe atomic store path, then replace the `std::sync::RwLock` value and increment generation. Expose loopback handlers for status/get-config/update-config with Task 1 control token + socket `ConnectInfo` loopback, 256 KiB request and 1 MiB response limits.

- [ ] **Step 4: Verify CAS and boundary**

Run: `cd src-tauri && cargo test --locked config_runtime::tests && cargo test --locked backend::control_api::tests`

Expected: PASS including wrong-token and non-loopback rejection.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/config_runtime.rs src-tauri/src/state.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/mod.rs src-tauri/src/backend/runtime.rs
git commit -m "feat(backend): add owner config generation CAS"
```

### Task 3: Build the GUI Control Client and Proxy Config/Cloud Sync

**Files:**
- Create: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands/config.rs`
- Modify: `src-tauri/src/commands/cloud_sync.rs`
- Modify: `src-tauri/src/commands/claude_md.rs`
- Modify: `src-tauri/src/commands/orchestrator_config.rs`
- Modify: `src-tauri/src/commands/health.rs`
- Modify: `src-tauri/src/commands/github_trending.rs`
- Test: `src-tauri/tests/runtime_authority_smoke.rs`

**Interfaces:** Consumes control routes. Produces typed `BackendControlClient` with safe-query one-time control-file refresh; mutations never auto-replay after uncertain response. Config adapters emit field-scoped patches; screenshot hotkey retains GUI OS-side compensation.

- [ ] **Step 1: Write failing smoke for config convergence and single Cloud Sync gate**

```rust
#[tokio::test]
async fn gui_config_update_changes_owner_generation_and_runtime_value() {
    let harness = RuntimeAuthorityHarness::start().await;
    let before = harness.client.status().await.unwrap();
    let after = harness.client.update_device_name(before, "desk-b").await.unwrap();
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(harness.owner_config().await.device_name, "desk-b");
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked --test runtime_authority_smoke gui_config_update`

Add tests for hotkey preflight conflict, OS replace failure, owner durable-save failure with OS rollback, and lost control response reconciled by owner/generation/config. Assert committed=new keeps the new OS shortcut, confirmed old rolls back, and ambiguous state blocks for manual reconcile. Expected: FAIL because the client/proxy does not exist.

- [ ] **Step 3: Implement typed client and migrate writers**

```rust
pub async fn mutate<T: DeserializeOwned>(
    &self,
    path: &str,
    body: &impl Serialize,
) -> Result<T, AppError> {
    self.send_once(path, body).await // no automatic mutation retry
}
```

Every GUI config writer submits only its typed business patch with expected owner/generation. Cloud Sync manual/test/CLAUDE.md push calls owner endpoints and returns existing DTOs. Screenshot hotkey follows a tested two-phase compensation path: owner CAS preflight, GUI OS shortcut replace through existing AppHandle logic, owner durable patch commit. On response loss, query owner/generation/config first: observed commit preserves the new shortcut, confirmed no-commit restores the old shortcut, and an indeterminate owner blocks further edits with manual reconcile instead of creating config/OS split-brain.

- [ ] **Step 4: Verify command adapters**

Run: `cd src-tauri && cargo test --locked --test runtime_authority_smoke && cargo test --locked commands::cloud_sync && cargo test --locked commands::config`

Expected: PASS; manual/scheduler paths share one gate.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/control_client.rs src-tauri/src/backend/mod.rs src-tauri/src/lib.rs src-tauri/src/commands/config.rs src-tauri/src/commands/cloud_sync.rs src-tauri/src/commands/claude_md.rs src-tauri/src/commands/orchestrator_config.rs src-tauri/src/commands/health.rs src-tauri/src/commands/github_trending.rs src-tauri/tests/runtime_authority_smoke.rs
git commit -m "refactor(backend): proxy runtime mutations to sidecar owner"
```

### Task 4: Make Sidecar the Sole Workbench Runtime Owner with RAII Compensation

**Files:**
- Modify: `src-tauri/src/commands/workbench/sessions.rs`
- Modify: `src-tauri/src/commands/workbench/common.rs`
- Modify: `src-tauri/src/commands/workbench/files.rs`
- Modify: `src-tauri/src/commands/workbench/projects.rs`
- Modify: `src-tauri/src/commands/workbench/git.rs`
- Modify: `src-tauri/src/commands/workbench/browser.rs`
- Modify: `src-tauri/src/commands/workbench/tests.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/workbench/remote_protocol.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Test: `src-tauri/src/commands/workbench/sessions.rs`
- Test: `src-tauri/tests/pty_smoke.rs`

**Interfaces:** Produces `SessionSpawnGuard`, `RestoreClaimGuard`, complete typed Workbench control dispatch/client routes and separate bounded data-plane streaming; every GUI adapter proxies projects/files/Git/browser/session operations to the sidecar owner, which alone consumes existing remote-aware helpers and creates remote clients/bridges.

- [ ] **Step 1: Write failure-injection tests**

```rust
#[tokio::test]
async fn repo_failure_closes_spawned_session() {
    let harness = SessionHarness::with_repo_upsert_failure().await;
    assert!(harness.create().await.is_err());
    assert_eq!(harness.registry_len().await, 0);
    assert_eq!(harness.live_child_count(), 0);
}
```

Add an inventory test that exercises local and remote project/list/file/Git/browser/session commands from a `GuiClient` state and proves they hit `BackendControlClient`; assert GUI bridge/task counts remain zero while sidecar owns the operation.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked repo_failure_closes_spawned_session`

Expected: FAIL with a retained registry/child.

- [ ] **Step 3: Add guards and enforce role**

```rust
let mut guard = SessionSpawnGuard::new(registry.clone(), session_id.clone());
repo.upsert(&record).await?;
guard.commit();
```

Wrap restore claims in a Drop guard. Add `state.runtime_role.require_owner()?` to all owner helpers; GUI adapters proxy the complete Workbench command inventory to sidecar, including project open/remove, files, Git, browser, session create/rename/focus/write/resize/close/restore. Register the exact loopback control routes and typed client here. Keep metadata under small-control limits; stream file/browser data with existing 5 MiB/10 MiB/32 MiB route budgets and add exact-boundary/over-limit tests. Remove GUI startup/callsites that instantiate `RemoteWorkbenchClient` or remote event bridges.

- [ ] **Step 4: Verify PTY and concurrent GUI/Mobile access**

Run: `cd src-tauri && cargo test --locked workbench::sessions && cargo test --locked commands::workbench && cargo test --locked --test pty_smoke && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS; concurrent list/restore yields one attach, all GUI local/remote operations report the sidecar owner id, and GUI runtime/bridge counts stay zero.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/workbench src-tauri/src/workbench/sessions.rs src-tauri/src/workbench/remote_client.rs src-tauri/src/workbench/remote_protocol.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/net/http_server.rs src-tauri/tests/pty_smoke.rs
git commit -m "fix(workbench): make sidecar sole runtime owner"
```

### Task 5: Relay Sidecar Events and Proxy Orchestrator Runtime Snapshot

**Files:**
- Create: `src-tauri/src/backend/event_bus.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/backend/ui.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/commands/orchestrator/tasks.rs`
- Modify: `src-tauri/src/commands/orchestrator/common.rs`
- Test: `src-tauri/tests/runtime_authority_smoke.rs`

**Interfaces:** Produces `BackendRuntimeCursor { owner_instance_id, sequence }`, bounded replay ring, `afterSequence` relay and explicit `Gap { oldestAvailable, latest }`; desktop snapshot consumes sidecar remote-aware route and preserves current cache semantics.

- [ ] **Step 1: Add failing relay and snapshot tests**

```rust
#[tokio::test]
async fn desktop_snapshot_comes_from_owner_telemetry() {
    let harness = RuntimeAuthorityHarness::start().await;
    harness.owner_record_tick("tick-1").await;
    let snapshot = harness.gui_snapshot().await.unwrap();
    assert_eq!(snapshot.latest_scheduler_tick.as_deref(), Some("tick-1"));
}
```

Add owner restart (`sequence` resets but owner id changes), disconnect/reconnect replay, broadcast lag/gap and terminal/runtime resync tests.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked --test runtime_authority_smoke desktop_snapshot_comes_from_owner_telemetry`

Expected: FAIL because GUI reads its local empty telemetry.

- [ ] **Step 3: Implement event bus and snapshot proxy**

Use a bounded broadcast channel plus bounded replay ring. GUI stores `(ownerInstanceId,sequence)`, resets on owner change, requests `afterSequence` on reconnect and drops duplicates only within the same owner. If the requested cursor predates the ring, relay emits `Gap`; GUI first invokes existing terminal replay and runtime snapshot refresh, then attaches at the latest live cursor. Replace local desktop snapshot builder with control client call; do not fill owner fields from GUI telemetry.

- [ ] **Step 4: Verify relay cancellation and no duplicates**

Run: `cd src-tauri && cargo test --locked --test runtime_authority_smoke event_relay && cargo test --locked --test runtime_authority_smoke desktop_snapshot`

Expected: PASS; owner restart is not mistaken for duplicate sequence and lag produces snapshot/replay recovery rather than silent loss.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/event_bus.rs src-tauri/src/backend/mod.rs src-tauri/src/backend/runtime.rs src-tauri/src/backend/ui.rs src-tauri/src/lib.rs src-tauri/src/commands/orchestrator/tasks.rs src-tauri/src/commands/orchestrator/common.rs src-tauri/tests/runtime_authority_smoke.rs
git commit -m "feat(backend): relay owner events to desktop"
```

### Task 6: Bound Remote Event Bridges and Expose Diagnostics

**Files:**
- Modify: `src-tauri/src/workbench/remote_events.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/commands/workbench/common.rs`
- Modify: `web/src/pages/Settings/SettingsDependenciesPanel.tsx`
- Modify: `web/src/pages/Settings/Settings.test.tsx`
- Test: `src-tauri/src/workbench/remote_events.rs`

**Interfaces:** Produces sidecar-owned `RemoteEventBridgeRegistry`, `RemoteEventBridgeSnapshot` and `SanitizedRuntimeDiagnostics` returned by owner status; GUI only consumes the Task 5 local relay.

- [ ] **Step 1: Write limits/lifecycle tests**

```rust
#[tokio::test]
async fn oversized_line_stops_bridge_without_retaining_buffer() {
    let result = parse_ndjson_chunks(vec![vec![b'x'; 1_048_577]]).await;
    assert!(matches!(result, Err(EventStreamError::ResourceLimit)));
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::remote_events::tests::oversized_line`

Expected: FAIL because buffer/error body are unbounded.

- [ ] **Step 3: Implement cancellation, TTL and budgets**

Move every bridge creation/callsite into the sidecar runtime registry and assert `GuiClient` cannot start one. Add `CancellationToken`, last-used time, exponential backoff capped at 60s, 60s idle TTL, 1 MiB line/pending limits and 8 KiB error prefix. Reuse the owner-managed peer/event client rather than constructing an unclassified `reqwest::Client` per bridge. Shutdown calls `shutdown_all()` and awaits bridge exit. Map only counts/phases/error codes into diagnostics.

- [ ] **Step 4: Add Settings diagnostics surface and verify**

Run: `cd src-tauri && cargo test --locked workbench::remote_events && cd ../web && npm test -- Settings.test.tsx`

Expected: PASS; copied diagnostics contain no token/content fields.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/remote_events.rs src-tauri/src/backend/runtime.rs src-tauri/src/commands/workbench/common.rs web/src/pages/Settings/SettingsDependenciesPanel.tsx web/src/pages/Settings/Settings.test.tsx
git commit -m "feat(backend): bound runtime bridges and diagnostics"
```

### Task 7: Integrate Protocol, Docs, and Full Gates

**Files:**
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control.rs`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/backend-operations.md`
- Modify: `docs/prd.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/development/quality-matrix.json`

**Interfaces:** Audits the already-registered versioned lifecycle/data routes and finalizes evidence IDs; control version remains in the control file/status and never enters LAN business protocol capabilities.

- [ ] **Step 1: Audit routes and update inventory/docs**

Verify every Task 2–6 route already has an explicit inventory entry with loopback socket + control-token + its metadata/data-plane method/body limits. Document them separately from LAN business routes. Do not defer missing registrations to this docs task, add a token to `server_protocol_info()`, or describe control version as LAN authorization capability.

- [ ] **Step 2: Run backend and inventory gates**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all exit 0.

- [ ] **Step 3: Run frontend gates**

```bash
cd web
npm run lint
npm run build
npm test
npm run test:e2e
```

Expected: all exit 0.

- [ ] **Step 4: Review secrets and split-brain evidence**

Inspect runtime authority smoke output and copied diagnostics; confirm no token/content and exactly one owner id per sidecar process.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/http_server.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control.rs docs/p2p-protocol.md docs/development/backend-operations.md docs/development/quality-matrix.json docs/prd.md src-tauri/CLAUDE.md
git commit -m "docs: define sidecar runtime authority"
```

## Rollback and Failure Containment

- control descriptor/version 字段保持 additive；旧 sidecar 明确显示需要重启，不恢复 GUI 第二套 runtime owner。
- 若 N1 必须回退，整体回退 GUI proxy 与 sidecar control routes，保留旧控制文件兼容读取；不得只回退一侧制造 split-brain。
- RAII guard 和资源清理属于正确性修复，不允许以 feature flag 绕过。

## Completion Contract

- GUI runtime mutations and all local/remote Workbench operations always round-trip through sidecar and return owner/generation where applicable.
- Cloud Sync, terminal and Orchestrator runtime have one owner under concurrent GUI/Mobile use.
- config/PTY/restore failures leave no split state or ghost resource.
- event relay/bridge lifecycle and NDJSON budgets are tested.
- full Rust/frontend/protocol/docs gates pass.

## Plan Self-Review

- Spec coverage: owner, CAS, Cloud Sync, terminal, events, snapshot, bridge, limits and diagnostics each map to a task.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: owner/generation/request types are defined once in Shared Interfaces and consumed unchanged.
