# Sync Integrity, Conflict and Recovery Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Prompt、SSH target、Scratchpad 同步准确报告真值、按有界事务收敛，并提供冲突历史、tombstone 回收和可验证导出恢复。

**Architecture:** 复用 vector clock 和 CC History 的 keyset 分页经验，server 提供无状态 manifest-page/items/push-batch，client 拉完或流式 merge 完整排序 manifest 后计算计划，不再用单页补集猜远端状态。批量写入使用单事务；版本/水位/deletion-floor/recovery 元数据独立表。恢复只在 sidecar maintenance gate 内执行 SQLite 事务，配置仅导出 report。

**Tech Stack:** Rust 2021, axum, reqwest, sqlx/SQLite, serde, zip-compatible archive library already approved by Cargo policy, React 19/TypeScript for Settings and history UI.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-sync-integrity-conflict-and-recovery-design.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md` 与 `docs/p2p-protocol.md`。
- 不修改 CC History 现有 paged protocol；新 capability 仅覆盖 Prompt/SSH/Scratchpad。
- network/HTTP/JSON/413/partial 都不得返回成功空集或增加 completed 计数。
- page ≤500 items/1 MiB；正文 batch ≤100 items/4 MiB。
- 导出排除项目源码、终端 transcript、SSH 私钥、token、凭据和 lifecycle control token。
- 恢复流式限制固定为 archive ≤2 GiB、entry ≤100,000、单 entry ≤64 MiB、总解压量 ≤4 GiB；恢复前备份保留 7 天且最多 3 份。
- runtime 幂等 schema upgrade 必须有混合版本测试与回滚说明；`migrations/0001_init.sql` 只同步文档，不启用 `sqlx::migrate!`；每任务 TDD + commit。

---

## File Structure

- Create: `src-tauri/src/sync/protocol.rs`。
- Modify: `src-tauri/src/sync/{mod,engine,ssh_target,scratchpad}.rs`。
- Modify: `src-tauri/src/net/{peer_client,protocol,http_server}.rs`。
- Modify: `src-tauri/src/net/routes/{sync,ssh_target_sync,scratchpad_sync}.rs`。
- Modify: `src-tauri/src/storage/{prompt_repo,ssh_target_repo,scratchpad_repo}.rs`。
- Create: `src-tauri/src/storage/content_version_repo.rs`。
- Create: `src-tauri/src/storage/sync_watermark_repo.rs`。
- Create: `src-tauri/src/storage/sync_delete_sequence_repo.rs`。
- Create: `src-tauri/src/storage/deletion_floor_repo.rs`。
- Create: `src-tauri/src/storage/sync_request_ledger_repo.rs`。
- Create: `src-tauri/src/storage/recovery_job_repo.rs`。
- Create: `src-tauri/src/storage/maintenance_gate.rs`。
- Modify: `src-tauri/src/storage/mod.rs` — 每个新增 repo/gate 在首次创建它的任务中同步注册，保持逐任务可编译。
- Create: `src-tauri/src/backup/{mod,archive,restore}.rs`。
- Create: `src-tauri/src/commands/backup.rs`。
- Modify: `src-tauri/src/backend/control_api.rs`, `src-tauri/src/backend/control_client.rs`, `src-tauri/src/backend/runtime.rs` — owner maintenance gate + backup control routes。
- Modify: `src-tauri/src/backend/runtime.rs` — 幂等 runtime schema。
- Modify: `src-tauri/migrations/0001_init.sql` — schema 文档同步。
- Create: `src-tauri/tests/backup_restore_smoke.rs`。
- Modify: `web/src/pages/Settings/SettingsSyncPanel.tsx`, `web/src/pages/Settings/useSettingsController.ts`, `web/src/pages/Settings/Settings.test.tsx`; Create: `web/src/api/sync.ts`, `web/src/api/sync.test.ts`。

## Shared Interfaces

```rust
pub struct SyncManifestPage<K> {
    pub items: Vec<SyncSummary<K>>,
    pub next_cursor: Option<String>,
}

pub enum SyncDomainOutcome {
    Succeeded { pulled: u32, pushed: u32, unchanged: u32 },
    Partial { applied: u32, failed: Vec<SyncItemFailure> },
    Unreachable { class: TransportClass },
    ProtocolError { code: String },
    ResourceLimit { limit: String },
}
```

### Task 1: Define the Pure Sync Plan Protocol and Limits

**Files:**
- Create: `src-tauri/src/sync/protocol.rs`
- Modify: `src-tauri/src/sync/mod.rs`
- Test: `src-tauri/src/sync/protocol.rs`

**Interfaces:** Produces generic `SyncSummary`, `SyncManifestPage`, `SyncPlan { push_to_remote, fetch_from_remote, unchanged }`, `compute_sync_plan` over two complete sorted manifests, and page/batch constants.

- [ ] **Step 1: Write table-driven vector-clock tests**

```rust
#[test]
fn equal_items_require_no_payload_exchange() {
    let plan = compute_sync_plan(&[summary("a", clock(1), "h")], &[summary("a", clock(1), "h")]);
    assert!(plan.push_to_remote.is_empty());
    assert!(plan.fetch_from_remote.is_empty());
    assert_eq!(plan.unchanged, 1);
}
```

Add local-only, remote-only, local-newer, remote-newer, concurrent, opaque-cursor loop and “stopped before `nextCursor=None`” cases.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked sync::protocol::tests`

Expected: FAIL because protocol module is absent.

- [ ] **Step 3: Implement pure planner and bounded page validators**

```rust
pub const MANIFEST_PAGE_ITEMS: usize = 500;
pub const MANIFEST_PAGE_BYTES: usize = 1_048_576;
pub const PUSH_BATCH_ITEMS: usize = 100;
pub const PUSH_BATCH_BYTES: usize = 4 * 1_048_576;
```

Return both directions explicitly only after the remote manifest stream ended at `nextCursor=None`. Merge manifests in stable id order; a truncated page, repeated cursor or resource-limit abort returns typed failure and never reaches the planner.

- [ ] **Step 4: Run tests**

Run: `cd src-tauri && cargo test --locked sync::protocol::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/sync/protocol.rs src-tauri/src/sync/mod.rs
git commit -m "feat(sync): define truthful bounded sync plan"
```

### Task 2: Add Typed Peer Calls and Three-Domain Routes

**Files:**
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/net/routes/sync.rs`
- Modify: `src-tauri/src/net/routes/ssh_target_sync.rs`
- Modify: `src-tauri/src/net/routes/scratchpad_sync.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Test inline: `src-tauri/src/net/peer_client.rs`, `src-tauri/src/net/routes/{sync,ssh_target_sync,scratchpad_sync}.rs`

**Interfaces:** Consumes Task 1 types. Produces typed `list_*_manifest_page(cursor)`, `fetch_*_items(ids)` and `push_*_batch(items,client_request_id)` calls plus accepted-count responses. Route code exists for testing, but `sync.manifest.v2` is not advertised until Task 3 lands its atomic idempotency ledger.

- [ ] **Step 1: Write failing transport truth tests**

```rust
#[tokio::test]
async fn invalid_json_is_failure_not_empty_remote() {
    let peer = fake_peer().reply(200, b"not-json");
    let result = peer.list_prompt_manifest_page(None).await;
    assert!(matches!(result, Err(PeerCallError::InvalidResponse { .. })));
}
```

Add 500 envelope, disconnect, 413 and accepted-count mismatch.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked net::peer_client::tests::invalid_json_is_failure_not_empty_remote`

Expected: FAIL because errors collapse to empty/false.

- [ ] **Step 3: Implement stateless manifest-page/items/push-batch routes**

```rust
pub async fn list_prompt_manifest_page(
    &self,
    cursor: Option<String>,
) -> Result<SyncManifestPage<String>, PeerCallError> {
    self.get_enveloped("/api/sync/prompts/manifest-page", cursor).await
}
```

Add typed `items` and `push-batch` transport calls, repeat with strong SSH/Scratchpad types, enforce body/page/batch budgets, and preserve legacy route behavior without swallowing errors. Do not emit the new capability yet. The client streams every sorted manifest page to completion before it classifies remote-only/local-only data; the server never infers caller absence from one page.

- [ ] **Step 4: Verify peer/routes and inventory**

Run: `cd src-tauri && cargo test --locked net::peer_client::tests && cargo test --locked net::routes::sync::tests && cargo test --locked net::routes::ssh_target_sync::tests && cargo test --locked net::routes::scratchpad_sync::tests && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/peer_client.rs src-tauri/src/net/routes/sync.rs src-tauri/src/net/routes/ssh_target_sync.rs src-tauri/src/net/routes/scratchpad_sync.rs src-tauri/src/net/protocol.rs src-tauri/src/net/http_server.rs docs/p2p-protocol.md
git commit -m "feat(sync): add typed content sync routes"
```

### Task 3: Make Production Bulk Upserts Transactional

**Files:**
- Modify: `src-tauri/src/storage/prompt_repo.rs`
- Modify: `src-tauri/src/storage/ssh_target_repo.rs`
- Modify: `src-tauri/src/storage/scratchpad_repo.rs`
- Create: `src-tauri/src/storage/sync_request_ledger_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`
- Modify: `src-tauri/src/net/routes/sync.rs`
- Modify: `src-tauri/src/net/routes/ssh_target_sync.rs`
- Modify: `src-tauri/src/net/routes/scratchpad_sync.rs`
- Modify: `src-tauri/src/net/protocol.rs`

**Interfaces:** Produces shared internal `bulk_upsert_in_transaction` plus same-transaction request ledger. Ledger key is `UNIQUE(claimed_device_id,domain,client_request_id)` with payload hash/outcome; the device id is a convergence label, never authentication.

- [ ] **Step 1: Write rollback tests for all three repositories**

```rust
#[tokio::test]
async fn prompt_bulk_failure_rolls_back_entire_batch() {
    let repo = test_repo().await;
    repo.bulk_upsert_inject_fail_at(vec![prompt("a"), prompt("b")], 1).await.unwrap_err();
    assert!(repo.list_all().await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked storage::prompt_repo::tests::bulk_failure_rolls_back && cargo test --locked storage::ssh_target_repo::tests::bulk_failure_rolls_back && cargo test --locked storage::scratchpad_repo::tests::bulk_failure_rolls_back`

Expected: FAIL for Prompt/SSH or production seam mismatch.

- [ ] **Step 3: Implement one transaction per batch and idempotent outcome ledger**

```rust
let mut tx = self.pool.begin().await?;
for item in items { Self::upsert_with_executor(&mut *tx, item).await?; }
tx.commit().await?;
```

Keep deterministic fault seams inside each repo module test so no private repo API is exported to integration tests. Route apply begins one transaction, claims the ledger key, rejects same-key/different-hash, executes the production bulk loop, records the exact accepted/failure outcome and commits once. Same-key/same-hash returns the recorded outcome without inserting duplicate conflict/history rows; add a deterministic unique key for future conflict rows. Only after all three domains use this path, add and advertise `sync.manifest.v2` in `server_protocol_info()` atomically; protocol tests must prove a legacy server omits it and a fully wired server advertises it.

- [ ] **Step 4: Run repository and fault tests**

Run: `cd src-tauri && cargo test --locked storage::prompt_repo && cargo test --locked storage::ssh_target_repo && cargo test --locked storage::scratchpad_repo && cargo test --locked sync_request_ledger && cargo test --locked net::routes::sync::tests::replayed_batch_returns_recorded_outcome`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/prompt_repo.rs src-tauri/src/storage/ssh_target_repo.rs src-tauri/src/storage/scratchpad_repo.rs src-tauri/src/storage/sync_request_ledger_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql src-tauri/src/net/routes/sync.rs src-tauri/src/net/routes/ssh_target_sync.rs src-tauri/src/net/routes/scratchpad_sync.rs src-tauri/src/net/protocol.rs
git commit -m "fix(sync): make content batches transactional"
```

### Task 4: Return Per-Device/Domain Truth and Bound Concurrency

**Files:**
- Modify: `src-tauri/src/sync/engine.rs`
- Modify: `src-tauri/src/sync/ssh_target.rs`
- Modify: `src-tauri/src/sync/scratchpad.rs`
- Modify: `src-tauri/src/commands/sync.rs`
- Create: `web/src/api/sync.ts`
- Create: `web/src/api/sync.test.ts`
- Modify: `web/src/pages/Settings/SettingsSyncPanel.tsx`
- Test inline: `src-tauri/src/sync/engine.rs`
- Test: `web/src/pages/Settings/Settings.test.tsx`

**Interfaces:** Produces `SyncRunResult.devices[].domains`; completed count includes only fully completed devices.

- [ ] **Step 1: Write failing partial/count/health-reuse tests**

```rust
#[tokio::test]
async fn one_domain_failure_marks_device_partial() {
    let result = run_with_domain_failure("scratchpad").await;
    assert_eq!(result.succeeded_devices, 0);
    assert_eq!(result.devices[0].status, DeviceSyncStatus::Partial);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked sync::engine::tests::one_domain_failure_marks_device_partial`

Expected: FAIL because current accepted/synced count is optimistic.

- [ ] **Step 3: Implement truth aggregation and concurrency=4**

Perform one typed health/capability fetch per device, reuse it across domains, and execute devices with `buffer_unordered(4)`. Preserve deterministic result ordering by sorting final results by device id.

- [ ] **Step 4: Render domain statuses and verify**

Run: `cd src-tauri && cargo test --locked sync::engine::tests && cd ../web && npm test -- Settings.test.tsx`

Expected: PASS; partial/unreachable never display as success.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/sync/engine.rs src-tauri/src/sync/ssh_target.rs src-tauri/src/sync/scratchpad.rs src-tauri/src/commands/sync.rs web/src/api/sync.ts web/src/api/sync.test.ts web/src/pages/Settings/SettingsSyncPanel.tsx web/src/pages/Settings/Settings.test.tsx
git commit -m "feat(sync): report per-domain convergence truth"
```

### Task 5: Add Conflict Versions and Tombstone Watermarks

**Files:**
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`
- Create: `src-tauri/src/storage/content_version_repo.rs`
- Create: `src-tauri/src/storage/sync_watermark_repo.rs`
- Create: `src-tauri/src/storage/sync_delete_sequence_repo.rs`
- Create: `src-tauri/src/storage/deletion_floor_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/sync/merger.rs`
- Modify: `src-tauri/src/sync/engine.rs`
- Modify: `src-tauri/src/sync/ssh_target.rs`
- Modify: `src-tauri/src/sync/scratchpad.rs`
- Modify: `src-tauri/src/net/routes/sync.rs`
- Modify: `src-tauri/src/net/routes/ssh_target_sync.rs`
- Modify: `src-tauri/src/net/routes/scratchpad_sync.rs`
- Modify: `src-tauri/src/storage/prompt_repo.rs`
- Modify: `src-tauri/src/storage/ssh_target_repo.rs`
- Modify: `src-tauri/src/storage/scratchpad_repo.rs`
- Modify: `web/src/api/prompts.ts`
- Modify: `web/src/api/scratchpad.ts`
- Modify: `web/src/pages/Prompts/Prompts.tsx`
- Modify: `web/src/pages/Scratchpad/Scratchpad.tsx`
- Modify: `web/src/pages/Prompts/Prompts.test.tsx`
- Modify: `web/src/pages/Prompts/promptMutations.test.ts`
- Modify: `web/src/pages/Scratchpad/Scratchpad.test.tsx`
- Test: inline tests in `src-tauri/src/storage/content_version_repo.rs`, `src-tauri/src/storage/sync_watermark_repo.rs`, `src-tauri/src/storage/deletion_floor_repo.rs`, `src-tauri/src/sync/merger.rs`

**Interfaces:** Produces `ContentVersion`, `SyncPeerWatermark { acked_delete_epoch }`, `DeletionFloor`, per-domain delete sequence, transactional `apply_merge_batch`, `list_versions`, `restore_version`, `compact_tombstones_to_floors(now)` and `apply_deletion_floor(incoming)`.

- [ ] **Step 1: Write legacy-schema/merge/GC tests**

```rust
#[tokio::test]
async fn concurrent_text_keeps_conflict_copy() {
    let result = merge_concurrent_text(local("left"), remote("right")).await;
    assert_eq!(result.conflict_versions.len(), 1);
}

#[tokio::test]
async fn active_peer_without_watermark_blocks_tombstone_gc() {
    assert_eq!(gc_fixture().without_ack().run().await.deleted, 0);
}

#[tokio::test]
async fn peer_offline_for_180_days_cannot_resurrect_compacted_delete() {
    let incoming = old_live_row_from_offline_peer();
    assert_eq!(apply_deletion_floor(incoming).await, DeleteWins);
}
```

Repeat merge/apply coverage for Prompt, SSH and Scratchpad. Inject failure after active-row write and after conflict/watermark write; each domain must roll back active row, conflict version, floor/watermark and request outcome together. Add delete-epoch mint/manifest/ack ordering tests, including an incomplete manifest that cannot advance the ack.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked content_version && cargo test --locked sync_watermark && cargo test --locked deletion_floor && cargo test --locked concurrent_text_keeps_conflict_copy`

Expected: FAIL because tables/repos are absent.

- [ ] **Step 3: Implement idempotent runtime schema, retention and restore-as-new-version**

Add `CREATE TABLE/INDEX IF NOT EXISTS` to runtime/repo init and mirror it in `migrations/0001_init.sql`; do not use `sqlx::migrate!`. Mint a monotonic local `deleteEpoch` in the same transaction as every local/adopted delete. Manifest/floor rows carry it; a peer advances `ackedDeleteEpoch` only after complete manifest consumption and successful apply. Retain 20 versions or 30 days; conflicts at least 30 days. For Prompt/SSH/Scratchpad, route every engine and HTTP apply call through one per-domain `apply_merge_batch` transaction that commits active winner, deterministic conflict row, watermark/floor and request outcome together or rolls all back. GC requires tombstone age ≥30 days and all active-peer acks ≥ its epoch, then atomically replaces the full tombstone with a durable deletion floor. Incoming live ancestors are rejected and receive the delete; concurrent versions become history while delete remains active. Restoring a version advances the local vector clock.

- [ ] **Step 4: Add focused UI tests**

Run: `cd web && npm test -- Prompts.test.tsx promptMutations.test.ts Scratchpad.test.tsx && cd ../src-tauri && cargo test --locked content_version && cargo test --locked sync_watermark && cargo test --locked deletion_floor`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql src-tauri/src/storage/content_version_repo.rs src-tauri/src/storage/sync_watermark_repo.rs src-tauri/src/storage/sync_delete_sequence_repo.rs src-tauri/src/storage/deletion_floor_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/storage/prompt_repo.rs src-tauri/src/storage/ssh_target_repo.rs src-tauri/src/storage/scratchpad_repo.rs src-tauri/src/sync/merger.rs src-tauri/src/sync/engine.rs src-tauri/src/sync/ssh_target.rs src-tauri/src/sync/scratchpad.rs src-tauri/src/net/routes/sync.rs src-tauri/src/net/routes/ssh_target_sync.rs src-tauri/src/net/routes/scratchpad_sync.rs web/src/api/prompts.ts web/src/api/scratchpad.ts web/src/pages/Prompts/Prompts.tsx web/src/pages/Prompts/Prompts.test.tsx web/src/pages/Prompts/promptMutations.test.ts web/src/pages/Scratchpad/Scratchpad.tsx web/src/pages/Scratchpad/Scratchpad.test.tsx
git commit -m "feat(sync): preserve conflicts and collect tombstones safely"
```

### Task 6: Build Verified Export and Transactional Restore

**Files:**
- Create: `src-tauri/src/backup/mod.rs`
- Create: `src-tauri/src/backup/archive.rs`
- Create: `src-tauri/src/backup/restore.rs`
- Create: `src-tauri/src/commands/backup.rs`
- Create: `src-tauri/src/storage/recovery_job_repo.rs`
- Create: `src-tauri/src/storage/maintenance_gate.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/pages/Settings/SettingsSyncPanel.tsx`
- Modify: `web/src/pages/Settings/useSettingsController.ts`
- Modify: `web/src/pages/Settings/Settings.test.tsx`
- Modify: `web/src/api/sync.ts`
- Modify: `web/src/api/sync.test.ts`
- Modify: `src-tauri/src/commands/{prompts,scratchpad,ssh_target,cc_history,claude_md,cloud_sync,sync}.rs`
- Modify: `src-tauri/src/net/routes/{sync,ssh_target_sync,scratchpad_sync,cc_history,claude_md_sync}.rs`
- Modify: `src-tauri/src/storage/{cc_history_repo,claude_md_repo,health_repo,prompt_repo,scratchpad_repo,ssh_target_repo,transfer_repo,workbench_browser_repo,workbench_project_repo,workbench_session_repo,workbench_worktree_repo}.rs`
- Modify: `src-tauri/src/orchestrator/{delivery,outbox,scheduler}.rs`
- Modify: `src-tauri/src/orchestrator/repo/{attempts,evidence,remote,tasks}.rs`
- Modify: `src-tauri/src/orchestrator/repo/mod.rs`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`
- Modify: `src-tauri/src/transfer/receiver.rs`
- Modify: `src-tauri/src/cloud_sync/snapshot.rs`
- Test inline: `src-tauri/src/backup/{archive,restore}.rs`, `src-tauri/src/storage/maintenance_gate.rs`
- Test black-box: `src-tauri/tests/backup_restore_smoke.rs`

**Interfaces:** Produces owner-only `create_backup`, `inspect_backup`, `restore_backup`, `list_recovery_jobs`, `rollback_recovery_job`; preview returns domain counts/conflicts/warnings and never mutates. GUI Tauri commands proxy through N1 control client after file selection.

- [ ] **Step 1: Write archive safety and rollback tests**

```rust
#[tokio::test]
async fn checksum_mismatch_changes_nothing() {
    let harness = restore_test_harness_with_tampered_archive().await;
    let before = harness.database_digest().await;
    assert!(harness.restore().await.is_err());
    assert_eq!(harness.database_digest().await, before);
}
```

Keep archive/tamper/fault/crash tests inside private backup/storage module tests. Add zip-slip, symlink, archive >2 GiB, entry count >100,000, single entry >64 MiB, total uncompressed >4 GiB, unknown-version, injected transaction failure, crash at each recovery-job phase, maintenance-gate writer exclusion, seven-day/three-backup cleanup and one-click rollback. Add a timeout-bounded test where restore holds exclusive, writes recovery-job state and restored rows through a maintenance permit without self-deadlock while ordinary command/scheduler writers remain blocked until release. Add a production-writer inventory test that fails when a SQLite mutation/transaction bypasses the gated writer constructor; its fixtures cover content repos, Transfer receiver/repo, Workbench repos, Orchestrator repo/outbox/delivery/scheduler, health sampling, Cloud Sync snapshot and LAN routes. The integration smoke starts the real sidecar process and uses only HTTP/control endpoints plus process kill/restart; it does not import private repos. Verify validation is streaming and does not allocate the declared total size.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked backup::archive::tests && cargo test --locked backup::restore::tests && cargo test --locked storage::maintenance_gate::tests && cargo test --locked --test backup_restore_smoke`

Expected: FAIL because backup module is absent.

- [ ] **Step 3: Implement inspect-before-restore pipeline**

Add one `DatabaseMaintenanceGate` to `AppState`. `maintenance_gate.rs` exposes `DatabaseWritePermit::{Shared,MaintenanceExclusive}` plus the only production `begin_write_with_permit`: ordinary callers obtain Shared before `SqlitePool::begin`; restore converts its already-held exclusive guard into MaintenanceExclusive and never requests a nested shared lease. Both permit variants live through commit/rollback. Migrate every current SQLite writer—not just restored content domains—including Prompt/Scratchpad/SSH/CC History/CLAUDE.md/sync/cloud, Transfer, Workbench, Orchestrator, health/background jobs and LAN routes; no command/repo/background task may call a raw write begin/execute path. `OrchestratorRepo` in `repo/mod.rs` receives the same `Arc<DatabaseMaintenanceGate>` from `backend/runtime.rs` and supplies permits to all child repos/scheduler paths; startup-only schema bootstrap is explicitly separated and inventory-tested. Restore holds the exclusive lease from pre-restore backup through selected-domain transaction and index rebuild, so no local, LAN or background writer can be silently overwritten between backup and replace. Validate all entries before extraction, produce preview, persist a `recovery_jobs` state machine, create a user-private pre-restore backup, then apply selected SQLite domains in one transaction. Config is report-only and never restored. Retain backups for 7 days with a hard cap of 3, deleting old files only after the new backup has atomically completed; crash recovery classifies preparing/ready/applying/succeeded/failed/rolledBack without guessing success.

- [ ] **Step 4: Verify Settings flow and round-trip**

Run: `cd src-tauri && cargo test --locked --test backup_restore_smoke && cd ../web && npm test -- Settings.test.tsx`

Expected: PASS; export→restore→export is semantically equivalent, Settings lists recovery jobs/backups and can invoke verified rollback, and no GUI-local database/config writer is used.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backup/mod.rs src-tauri/src/backup/archive.rs src-tauri/src/backup/restore.rs src-tauri/src/commands/backup.rs src-tauri/src/commands/mod.rs src-tauri/src/storage/recovery_job_repo.rs src-tauri/src/storage/maintenance_gate.rs src-tauri/src/storage/mod.rs src-tauri/src/storage/cc_history_repo.rs src-tauri/src/storage/claude_md_repo.rs src-tauri/src/storage/health_repo.rs src-tauri/src/storage/prompt_repo.rs src-tauri/src/storage/scratchpad_repo.rs src-tauri/src/storage/ssh_target_repo.rs src-tauri/src/storage/transfer_repo.rs src-tauri/src/storage/workbench_browser_repo.rs src-tauri/src/storage/workbench_project_repo.rs src-tauri/src/storage/workbench_session_repo.rs src-tauri/src/storage/workbench_worktree_repo.rs src-tauri/src/state.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/backend/runtime.rs src-tauri/src/lib.rs src-tauri/src/commands/prompts.rs src-tauri/src/commands/scratchpad.rs src-tauri/src/commands/ssh_target.rs src-tauri/src/commands/cc_history.rs src-tauri/src/commands/claude_md.rs src-tauri/src/commands/cloud_sync.rs src-tauri/src/commands/sync.rs src-tauri/src/net/routes/sync.rs src-tauri/src/net/routes/ssh_target_sync.rs src-tauri/src/net/routes/scratchpad_sync.rs src-tauri/src/net/routes/cc_history.rs src-tauri/src/net/routes/claude_md_sync.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/orchestrator/delivery.rs src-tauri/src/orchestrator/outbox.rs src-tauri/src/orchestrator/scheduler.rs src-tauri/src/orchestrator/repo/mod.rs src-tauri/src/orchestrator/repo/attempts.rs src-tauri/src/orchestrator/repo/evidence.rs src-tauri/src/orchestrator/repo/remote.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/transfer/receiver.rs src-tauri/src/cloud_sync/snapshot.rs src-tauri/tests/backup_restore_smoke.rs web/src/api/sync.ts web/src/api/sync.test.ts web/src/pages/Settings/SettingsSyncPanel.tsx web/src/pages/Settings/useSettingsController.ts web/src/pages/Settings/Settings.test.tsx
git commit -m "feat(backup): add verified export and restore"
```

### Task 7: Complete Protocol, Docs, and Full Verification

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/development/backend-operations.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

- [ ] **Step 1: Update capabilities, route inventory and mixed-version facts**

Document `sync.manifest.v2`, legacy typed failures, batch budgets, conflict retention, tombstone GC and backup exclusions.

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

- [ ] **Step 3: Inspect export archive and logs**

Confirm no project source, terminal text, private key, token, credential URL or control token.

- [ ] **Step 4: Verify old peer fallback**

Run mixed-version integration: v2 client→legacy server and legacy client→v2 server; failures are typed and no successful empty result is fabricated.

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md docs/p2p-protocol.md docs/development/backend-operations.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: define truthful sync and recovery"
```

## Rollback and Failure Containment

- 可撤下 `sync.manifest.v2` capability 并回到 legacy 协议，但 legacy transport/HTTP/JSON 失败仍必须保持 typed failure，不能恢复“错误即空集”。
- additive 历史/水位/recovery 表回退时保留；代码停止读写新表，不执行自动 DROP。
- restore 在确认前零写入；事务失败回滚，恢复前备份只有完整落盘后才进入可回退列表。

## Completion Contract

- Equal data produces zero payload exchange; all failures remain failures.
- Three repository batches are atomic and partial devices are not counted complete.
- Conflict versions and tombstone GC obey fixed retention/watermark rules.
- Export/restore rejects unsafe archives and rolls back atomically.
- full frontend/Rust/protocol/docs gates pass.

## Plan Self-Review

- Spec coverage: protocol, typed errors, transactions, results, conflicts, GC, export and restore each have tasks.
- Placeholder scan: no unresolved implementation placeholders; schema follows the repository's inspected inline-init + `0001_init.sql` documentation convention.
- Type consistency: plan/result/status types match Shared Interfaces throughout.
