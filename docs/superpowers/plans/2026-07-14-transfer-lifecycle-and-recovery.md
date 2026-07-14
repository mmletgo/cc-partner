# Transfer Lifecycle and Recovery Implementation Plan

> **For agentic workers:** REQUIRED EXECUTION FLOW: use `superpowers:using-git-worktrees`, then `superpowers:test-driven-development` task-by-task, and run `superpowers:verification-before-completion` before every completion claim. Native subagents may execute independent tasks; do not require unavailable `subagent-driven-development` or `executing-plans` skills. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有 send/cancel 和 durable finalize 上补齐 Transfer 的失败阶段、retry/resume、uncertain 对账、Open/Reveal 与可操作历史。

**Architecture:** transfer repository 保存 logical transfer、attempt、发送端 client operation ledger、稳定 protocol transfer id、nullable phase 和 failure；coarse status 保持兼容。全局唯一 clientOperationId + canonical payload hash 在发送端事务 claim 后才 spawn，metadata/SHA 进入 blocking worker，resume 复用 receiver checkpoint key。receiver 继续只按 protocol id 拥有 durable promotion/finalize journal；ACK 丢失由发送端查询 receiver 真值后本地提交 operation outcome，不跨设备伪造单事务，也不重写 chunk IO。Open/Reveal 只由 same-device GUI opener 执行。

**Tech Stack:** Rust 2021, Tauri 2, axum/reqwest, sqlx/SQLite, existing transfer protocol, React/TypeScript/Vitest/E2E.

## Global Constraints

- 必读 `docs/superpowers/specs/2026-07-14-transfer-lifecycle-and-recovery-design.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md` 和 `docs/prd.md` 的 Transfer 章节。
- 不重写 send/cancel、chunk algorithm 或 durable finalize。
- 同一 clientOperationId 不得创建重复 attempt；request ID 仅追踪；unknown mutation 必须先对账。
- Open/Reveal 只允许 same-device desktop GUI 打开本机收到的 completed 文件；P2P/mobile unsupported。
- 旧 peer 无 resume capability 时只提供 retry，不显示假续传。
- 1 GiB 真正 resume 只在 N8 L3 后标 PASS。

---

## File Structure

- Modify: `src-tauri/src/models/transfer.rs`。
- Modify: `src-tauri/src/storage/transfer_repo.rs`。
- Modify: `src-tauri/src/transfer/{sender,receiver,registry,mod}.rs`。
- Modify: `src-tauri/src/commands/transfer.rs`。
- Modify: `src-tauri/src/net/routes/transfer.rs` and peer client/protocol。
- Modify: `src-tauri/src/backend/runtime.rs` and transfer repo schema helper — runtime `CREATE TABLE/ALTER` compatibility。
- Modify: `src-tauri/migrations/0001_init.sql` — schema 文档同步；禁止启用 `sqlx::migrate!`。
- Modify: `web/src/lib/types/core.ts`。
- Modify: `web/src/lib/schemas/transfer.ts` and tests。
- Modify: `web/src/api/transfer.ts` and tests。
- Modify: `web/src/pages/Transfer/Transfer.tsx` and tests。
- Modify: `web/src/components/domain/TransferItem/TransferItem.tsx` and tests。
- Create: `src-tauri/tests/transfer_recovery_smoke.rs`。

## Shared Interfaces

```rust
pub enum TransferPhase { Queued, Connecting, Transferring, Finalizing, Completed, Cancelled, Failed }

pub struct TransferFailure {
    pub stage: TransferFailureStage,
    pub code: String,
    pub retryable: bool,
    pub message: String,
}

pub enum TransferOperationStatus { NotFound, Pending, Succeeded { task_id: String }, Failed { code: String }, OperationIdConflict }
```

Every sender-side durable attempt stores `client_operation_id`, canonical `operation_payload_hash`, `logical_transfer_id`, `attempt_id`, and `protocol_transfer_id`; resume preserves the protocol id, while a full retry may mint a new one. `request_id` remains trace-only. Receiver durability is keyed only by `protocol_transfer_id`.

### Task 1: Persist Compatible Phase, Failure, Logical/Attempt/Protocol IDs, and Client Operation ID

**Files:**
- Modify: `src-tauri/src/models/transfer.rs`
- Modify: `src-tauri/src/storage/transfer_repo.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`
- Test: inline tests in `src-tauri/src/storage/transfer_repo.rs` and `src-tauri/src/models/transfer.rs`

**Interfaces:** Produces additive durable fields consumed by retry/reconcile/UI while preserving existing `TransferStatus` and legacy rows. Sender operation rows persist the canonical payload hash; receiver journal rows remain protocol-id-owned.

- [ ] **Step 1: Write schema-upgrade/default/idempotency tests**

```rust
#[tokio::test]
async fn legacy_transfer_defaults_to_attempt_one() {
    let task = repo.load_legacy_fixture().await.unwrap();
    assert_eq!(task.attempt, 1);
    assert_eq!(task.logical_transfer_id, task.id);
    assert_eq!(task.attempt_id, task.id);
    assert_eq!(task.protocol_transfer_id, task.id);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked transfer_repo && cargo test --locked models::transfer`

Expected: FAIL because fields are absent.

- [ ] **Step 3: Add additive DTO/repository fields and unique operation index**

Keep coarse status unchanged and add nullable phase/failure with stable enums/codes. Add `client_operation_id`, `operation_payload_hash`, `logical_transfer_id`, `attempt_id`, `protocol_transfer_id` and global `UNIQUE(client_operation_id)` through idempotent runtime schema/legacy `ALTER TABLE` helpers. The canonical hash covers operation kind, logical task/source identity, peer and expected protocol id. Legacy rows derive all ids from task id and attempt=1; their nullable payload hash is lazily backfilled before replay, and unknown/new phase falls back to coarse status without becoming Failed.

- [ ] **Step 4: Run repo/model tests**

Run: `cd src-tauri && cargo test --locked transfer_repo && cargo test --locked models::transfer`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/models/transfer.rs src-tauri/src/storage/transfer_repo.rs src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql
git commit -m "feat(transfer): persist recovery state"
```

### Task 2: Implement Idempotent Retry and Resume

**Files:**
- Modify: `src-tauri/src/transfer/sender.rs`
- Modify: `src-tauri/src/transfer/registry.rs`
- Modify: `src-tauri/src/storage/transfer_repo.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/commands/transfer.rs`
- Modify: `src-tauri/src/net/routes/transfer.rs`
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Test inline: modified transfer/repo/runtime modules
- Test black-box: `src-tauri/tests/transfer_recovery_smoke.rs`

**Interfaces:** Produces `retry_transfer(task_id,client_operation_id)` and `resume_transfer(task_id,client_operation_id)`; request id remains trace-only.

- [ ] **Step 1: Write retry/resume guard tests**

```rust
#[tokio::test]
async fn duplicate_resume_request_creates_one_attempt() {
    let h = transfer_test_harness_with_checkpoint().await;
    let (a, b) = tokio::join!(h.resume("op-1"), h.resume("op-1"));
    assert_eq!(a.unwrap().id, b.unwrap().id);
    assert_eq!(h.attempt_count().await, 2);
}
```

Keep private fault/claim/crash seams in inline module tests. The integration smoke starts a real sidecar and uses only HTTP/control APIs. Add non-retryable, active-phase, source-changed, old-peer capability, concurrent unique-claim, insert-before-spawn crash/restart and runtime-heartbeat cases. Add same-ID/same-payload replay plus same-ID/different-payload rejection for retry↔resume and two different logical tasks.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked --test transfer_recovery_smoke duplicate_resume_request_creates_one_attempt`

Expected: FAIL because commands do not exist.

- [ ] **Step 3: Implement state/fingerprint/capability validation**

Build the canonical payload and insert/claim the sender attempt row transactionally by global unique clientOperationId; only `rows_affected=1` winner may spawn. On conflict, same payload returns the recorded/pending operation while a different payload returns typed `operationIdConflict` before any child/network work. Persist Queued before computing `{size,mtimeNsOrNull,sha256}` on an opened handle in `spawn_blocking`, then recheck size/mtime immediately before spawn and require receiver SHA match; mtime-unavailable paths revalidate size+SHA. Retry keeps the logical id and may create a new protocol id after validation; resume requires compatible checkpoint metadata and peer capability, reuses the stable protocol id, and refuses TOCTOU/source change. `backend/runtime.rs` recovers insert-before-spawn rows on owner startup. Add/advertise the exact mixed-version capability only after routes are ready; old peer falls back to retry. Use existing checkpoint/chunk helpers.

- [ ] **Step 4: Run transfer tests**

Run: `cd src-tauri && cargo test --locked transfer && cargo test --locked --test transfer_recovery_smoke`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/transfer/sender.rs src-tauri/src/transfer/registry.rs src-tauri/src/storage/transfer_repo.rs src-tauri/src/backend/runtime.rs src-tauri/src/commands/transfer.rs src-tauri/src/net/routes/transfer.rs src-tauri/src/net/peer_client.rs src-tauri/src/net/protocol.rs src-tauri/tests/transfer_recovery_smoke.rs
git commit -m "feat(transfer): add idempotent retry and resume"
```

### Task 3: Add Operation Reconciliation for Uncertain Results

**Files:**
- Modify: `src-tauri/src/storage/transfer_repo.rs`
- Modify: `src-tauri/src/transfer/sender.rs`
- Modify: `src-tauri/src/commands/transfer.rs`
- Modify: `src-tauri/src/net/routes/transfer.rs`
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/transfer/receiver.rs`
- Modify: `src-tauri/tests/quality_faults.rs`
- Test inline: `src-tauri/src/transfer/receiver.rs`, `src-tauri/src/storage/transfer_repo.rs`
- Test black-box: `src-tauri/tests/transfer_recovery_smoke.rs`

**Interfaces:** Produces sender-owned `get_transfer_operation(client_operation_id)` for desktop/P2P/mobile callers and extends the receiver's existing protocol-id complete/status lost-response fallback rather than replacing it.

- [ ] **Step 1: Write lost-ACK reconciliation test**

```rust
#[tokio::test]
async fn lost_final_ack_reconciles_to_completed_without_second_finalize() {
    let h = transfer_test_harness_dropping_final_response().await;
    let operation_id = h.start_and_timeout().await;
    assert!(matches!(h.operation(&operation_id).await.unwrap(), TransferOperationStatus::Succeeded { .. }));
    assert_eq!(h.finalize_count(), 1);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked --test transfer_recovery_smoke lost_final_ack`

First add passing characterization for the existing receiver promotion intent, restart recovery and complete/status lost-final-response behavior in `receiver.rs`/`peer_client.rs`/`quality_faults.rs`. Keep private crash points inline; the process smoke drops responses/kills the real sidecar through black-box orchestration. Then add a failing assertion only for the new operation-outcome boundary.

- [ ] **Step 3: Implement request lookup and typed status**

Reuse the receiver's existing finalizing/promotion journal and idempotent rename/hash recovery; do not replace it or add sender `clientOperationId` to its wire contract. Receiver completes and recovers by `protocolTransferId`. If the final response is lost, the sender first calls the existing receiver complete/status reconciliation by protocol id; when receiver success is authoritative, the sender commits its local task completed + globally keyed operation outcome in one SQLite transaction. If receiver is pending/unreachable, keep the sender operation pending/unknown and do not offer retry. Map transport timeout to unknown at callers and query the sender ledger before any later action.

- [ ] **Step 4: Run smoke**

Run: `cd src-tauri && cargo test --locked --test transfer_recovery_smoke`

Expected: PASS with finalize count=1.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/storage/transfer_repo.rs src-tauri/src/transfer/sender.rs src-tauri/src/transfer/receiver.rs src-tauri/src/commands/transfer.rs src-tauri/src/net/routes/transfer.rs src-tauri/src/net/peer_client.rs src-tauri/tests/quality_faults.rs src-tauri/tests/transfer_recovery_smoke.rs
git commit -m "feat(transfer): reconcile uncertain operations"
```

### Task 4: Add Same-Device GUI Open and Reveal Preparation

**Files:**
- Modify: `src-tauri/src/commands/transfer.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/api/transfer.ts`
- Modify: `web/src/api/transfer.test.ts`
- Test: inline tests in `src-tauri/src/commands/transfer.rs` and control API tests

**Interfaces:** Produces lifecycle-control-only `prepare_transfer_open(task_id, Open|Reveal) -> LocalTransferOpenTarget`; frontend calls installed Tauri opener. No P2P route or mobile DTO exposes the path.

- [ ] **Step 1: Write completed-only and path-missing tests**

```rust
#[tokio::test]
async fn reveal_rejects_non_completed_task() {
    let err = prepare_transfer_open(test_state(), "failed-task", Reveal).await.unwrap_err();
    assert_eq!(err.code(), "transfer_not_completed");
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked transfer_not_completed`

Expected: FAIL because command is absent.

- [ ] **Step 3: Resolve path from repository and call platform adapter**

Sidecar enforces `direction=Receive`, completed status and same-device lifecycle control, then resolves/checks the stored destination; it maps only repository status, missing target and path-validation errors. The GUI calls Tauri opener with the returned local target and its API layer maps opener permission/platform failures to stable local-only errors. P2P/mobile callers receive stable `unsupported`; no path or opener error leaks remotely.

- [ ] **Step 4: Run command/route tests**

Run: `cd src-tauri && cargo test --locked commands::transfer && cargo test --locked backend::control_api && cd ../web && npm test -- transfer.test.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/transfer.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/lib.rs web/src/api/transfer.ts web/src/api/transfer.test.ts
git commit -m "feat(transfer): open and reveal completed files"
```

### Task 5: Wire Runtime Schemas and Frontend API

**Files:**
- Modify: `web/src/lib/types/core.ts`
- Modify: `web/src/lib/schemas/transfer.ts`
- Modify: `web/src/lib/schemas/transfer.test.ts`
- Modify: `web/src/api/transfer.ts`
- Modify: `web/src/api/transfer.test.ts`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:** Produces decoded `retry/resume/operation/open/reveal` methods and typed phase/failure fields.

- [ ] **Step 1: Write decoder/API invocation tests**

```ts
test('resume sends taskId and stable clientOperationId', async () => {
  await transferApi.resume('t1', 'op1')
  expect(mockInvoke).toHaveBeenCalledWith('resume_transfer', { taskId: 't1', clientOperationId: 'op1' })
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- api/transfer.test.ts`

Expected: FAIL because methods/schema are absent.

- [ ] **Step 3: Add invokeDecoded methods and exhaustive schemas**

Do not use generic assertion; phase/failure/operation unions must reject invalid backend values.

- [ ] **Step 4: Run API/schema tests and build**

Run: `cd web && npm test -- api/transfer.test.ts && npm run build`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/api/transfer.ts web/src/api/transfer.test.ts web/src/lib/types/core.ts web/src/lib/schemas/transfer.ts web/src/lib/schemas/transfer.test.ts src-tauri/src/lib.rs
git commit -m "feat(transfer): expose recovery APIs"
```

### Task 6: Render Actionable Transfer History

**Files:**
- Modify: `web/src/components/domain/TransferItem/TransferItem.tsx`
- Modify: `web/src/components/domain/TransferItem/TransferItem.test.tsx`
- Modify: `web/src/pages/Transfer/Transfer.tsx`
- Modify: `web/src/pages/Transfer/Transfer.test.tsx`
- Modify: `web/src/pages/Transfer/Transfer.module.css`

**Interfaces:** Callbacks remain optional; page supplies only legal phase/capability actions.

- [ ] **Step 1: Write phase/action matrix tests**

```ts
test.each([
  ['transferring', ['取消']],
  ['failed-resumable', ['继续传输']],
  ['failed-retryable', ['重新传输']],
  ['completed-received', ['打开', '在文件夹中显示']],
])('%s renders only legal actions', (fixture, actions) => {
  renderTransferFixture(fixture)
  expectVisibleActions(actions)
})
```

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- TransferItem.test.tsx Transfer.test.tsx`

Expected: FAIL because actions/status do not exist.

- [ ] **Step 3: Implement sections, phases and reconciliation state**

Group active/needs-attention/recent-completed. Generate stable client operation IDs per user intent and retain them while pending/unknown; trace request IDs may change per call. During unknown show “正在确认结果” and suppress duplicate action. Open/Reveal callbacks exist only for same-device received completed rows.

- [ ] **Step 4: Run frontend tests and E2E**

Run: `cd web && npm test -- TransferItem.test.tsx Transfer.test.tsx && npm run test:e2e -- transfer.spec.ts`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/TransferItem/TransferItem.tsx web/src/components/domain/TransferItem/TransferItem.test.tsx web/src/pages/Transfer/Transfer.tsx web/src/pages/Transfer/Transfer.test.tsx web/src/pages/Transfer/Transfer.module.css
git commit -m "feat(transfer): complete recovery actions"
```

### Task 7: Update Protocol/Docs and Run Completion Gates

**Files:**
- Modify: `docs/prd.md`
- Modify: `docs/p2p-protocol.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `docs/development/quality-matrix.json`

- [ ] **Step 1: Document capabilities, retry matrix and L3 handoff**

Record old-peer fallback, client-operation idempotency versus trace request ID, stable resume protocol id, source fingerprint checks, same-device GUI Open/Reveal rule and that 1 GiB remains N8 until executed.

- [ ] **Step 2: Run full gates**

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

- [ ] **Step 3: Inspect durable finalize invariant**

Review tests/logs proving retry/reconcile does not invoke final rename twice or accept mismatched hash.

- [ ] **Step 4: Record N8 prerequisites**

Add the exact 1 GiB disconnect/restart/resume/SHA scenario to real-device certification without marking PASS.

- [ ] **Step 5: Commit**

```bash
git add docs/prd.md docs/p2p-protocol.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: define transfer recovery lifecycle"
```

## Rollback and Failure Containment

- transfer 新列/additive runtime schema 回退时保留；旧代码按默认 `attempt=1` 读取，不能删除 attempt/checkpoint/failure 记录。
- 可隐藏 retry/resume/Open/Reveal UI 并撤下 capability，但 durable finalize、client-operation idempotency 与 lost-ACK 对账修复不得降级为重复执行。
- 未完成 attempt 的回退只标记可恢复失败，不删除已接收 checkpoint 或目标文件。

## Completion Contract

- retry/resume are idempotent and source/capability validated.
- uncertain outcomes reconcile before retry; lost ACK does not duplicate finalize.
- Open/Reveal execute only in same-device desktop GUI for received completed tasks; P2P/mobile are unsupported.
- UI action matrix matches actual callbacks and all gates pass.

## Plan Self-Review

- Spec coverage: phase/failure, retry, resume, reconcile, Open/Reveal, UI and L3 handoff all map to tasks.
- Placeholder scan: no unresolved implementation placeholders; schema changes use the inspected inline runtime migration convention.
- Type consistency: Shared Interfaces match Rust/TS schema tasks.
