# Backend Scale and Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Orchestrator claim 的文件 IO 移出 SQLite 事务，以有界候选+CAS 保持调度正确性，并把 CC History 同步升级为有资源上限、兼容旧设备的分页批处理协议及本地性能指标。

**Architecture:** Orchestrator claim 拆为短 DB 候选快照、事务外 workflow preflight、短写事务 CAS 三阶段；CC History 通过 capability-gated manifest/items/push-batch 路由分页同步，legacy 路由保留一代。`RuntimeMetrics` 记录有界进程内指标，先保持单连接，只有压测达到明确门槛才允许独立提交扩到 2。

**Tech Stack:** Rust 2021, Tokio, sqlx 0.8 SQLite/WAL, axum, reqwest, serde/serde_json, tracing, Vitest-independent Rust integration tests.

## Global Constraints

- 开始前读取根 `AGENTS.md`、`src-tauri/CLAUDE.md`、`docs/p2p-protocol.md`、本 plan 对应 design spec；所有新增/修改 Rust 业务函数必须有规定格式的中文 docstring。
- 默认生产池保持 `max_connections(1)`、WAL 不变、SQLite `busy_timeout=5s`；只有 Task 8 的五项门槛全部满足才允许独立提交改为 2，绝不改为 3+。
- Orchestrator 状态机、优先级、Preparing lease、claim token、full-auto 语义不变；`WORKFLOW.md` 内容不入库。
- 固定上限：候选 256、项目 64、manifest 默认 256/最大 512、items/push batch 128、单条 content 1 MiB、单请求/响应估算 8 MiB、ID 256 UTF-8 bytes。
- 新 capability 精确命名 `cc-history.paged-sync.v1`；三个新路由与 token 原子上线；legacy `/api/cc-history/sync/pull|push` 保留。
- 指标只在本机进程内和脱敏 tracing 中存在，不增加 telemetry/上传，不记录正文、路径、项目/设备名、host、SQL 或凭据。
- 每个 task 先写失败测试、确认失败原因、最小实现、运行窄测试与影响面测试，再只提交本 task 文件；执行者不得实际复用 plan 中的示例 commit 作为跳过验证的理由。
- DB/schema 若无必要不迁移；本计划设计为零业务 schema 变更，可通过停止使用 capability/新路由回滚。

---

## Task Dependency Graph

```text
T1 metrics ─┬─> T2 bounded claim snapshot ─> T3 workflow preflight/CAS ─> T8 benchmark decision
            └─> T6 sync metrics/engine ────────────────────────────────┘
T4 CC repo paging/transactions ─> T5 paged routes/limits ─> T6 client compatibility ─> T7 protocol/docs
```

可并行 wave：`T1 | T4` → `T2 | T5` → `T3 | T6` → `T7` → `T8`。

## File Structure

- Create `src-tauri/src/backend/runtime_metrics.rs`: 有界 counter/duration/ewma snapshot 与脱敏 warning。
- Modify `src-tauri/src/backend/mod.rs`, `src-tauri/src/state.rs`, `src-tauri/src/backend/runtime.rs`: 注册 metrics、注入 AppState、统一 DB pool options。
- Create `src-tauri/src/orchestrator/claim.rs`: `ClaimCandidate`、`ClaimScanCursor`、workflow preflight 与三阶段 orchestration。
- Modify `src-tauri/src/orchestrator/mod.rs`, `repo.rs`, `scheduler.rs`: bounded JOIN snapshot、短 CAS transaction、cursor 生命周期与 scheduler 指标。
- Modify `src-tauri/src/storage/cc_history_repo.rs`: manifest keyset page、get-many、事务 ingest/upsert。
- Modify `src-tauri/src/net/routes/cc_history.rs`, `src-tauri/src/net/http_server.rs`: paged DTO/routes、route body limit、稳定错误。
- Modify `src-tauri/src/net/peer_client.rs`, `src-tauri/src/cc/engine.rs`: typed paged calls、capability gate、legacy fallback。
- Modify `src-tauri/src/net/protocol.rs`, `docs/p2p-protocol.md`, `src-tauri/CLAUDE.md`, `docs/prd.md`, `docs/development/testing.md`, `scripts/check-p2p-route-inventory.mjs`: capability/route/行为/验证事实。
- Create `src-tauri/tests/backend_scale.rs`: 慢 workflow、claim race、10k manifest、rollback、mixed-version 与 benchmark harness。

## Interfaces

```rust
pub const CLAIM_CANDIDATE_LIMIT: u32 = 256;
pub const CLAIM_PROJECT_LIMIT: usize = 64;

pub struct ClaimScanCursor { pub priority: i64, pub created_at: String, pub id: String }
pub struct ClaimCandidate { pub task: OrchestratorTaskRow, pub project_path: PathBuf }
pub struct ClaimPreflight { pub eligible: Vec<ClaimCandidate>, pub next_cursor: Option<ClaimScanCursor>, pub exhausted: bool }

pub async fn preflight_claim_candidates(
    candidates: Vec<ClaimCandidate>,
) -> Result<ClaimPreflight, AppError>;

pub async fn list_sync_manifest_page(
    &self, after_id: Option<&str>, limit: u32,
) -> Result<Vec<CcSyncSummary>, AppError>;
pub async fn get_many_for_sync(
    &self, ids: &[String],
) -> Result<HashMap<String, ClaudeHistoryRow>, AppError>;
pub async fn upsert_merged_batch(
    &self, items: &[ClaudeHistoryRow],
) -> Result<usize, AppError>;

pub struct RuntimeMetricSnapshot { pub count: u64, pub last_ms: u64, pub max_ms: u64, pub ewma_ms: f64 }
pub struct RuntimeMetricsSnapshot { pub metrics: BTreeMap<String, RuntimeMetricSnapshot> }
```

后续 task 只能消费以上稳定名称；如实现发现签名无法成立，先修订 spec/plan 并重新审查，不能在相邻 task 中各自发明别名。

---

### Task 1: Add Bounded Local Runtime Metrics

**Files:**
- Create: `src-tauri/src/backend/runtime_metrics.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Test: inline tests in `runtime_metrics.rs`

**Interfaces:**
- Consumes: `std::time::{Duration, Instant}`, existing tracing/log sanitization.
- Produces: `RuntimeMetrics::record_duration(name, duration)`, `record_count(name, value)`, `snapshot()` and `AppState.runtime_metrics: Arc<RuntimeMetrics>`.

- [ ] **Step 1: Write failing metric tests**

Test deterministic durations `10/30/20ms`: count=3, last=20, max=30 and EWMA remains finite/in range; insert more than 64 distinct metric names and assert snapshot retains at most 64; assert rejected names containing `/`, space or user text are not recorded.

- [ ] **Step 2: Run the narrow failure**

Run: `cd src-tauri && cargo test --locked backend::runtime_metrics -- --nocapture`

Expected: FAIL because `backend::runtime_metrics` and `RuntimeMetrics` do not exist.

- [ ] **Step 3: Implement fixed-name, bounded metrics**

Use `Mutex<BTreeMap<&'static str, MetricAccumulator>>`; only `&'static str` names accepted, max 64 entries, EWMA alpha `0.2`. Add warning helper with fixed fields `metric/count/last_ms/max_ms`, never arbitrary context. Inject one shared instance in every `AppState` constructor/test fixture.

- [ ] **Step 4: Verify**

Run: `cd src-tauri && cargo test --locked backend::runtime_metrics && cargo check --locked --all-targets`

Expected: all PASS; no `AppState` initializer missing-field errors.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/runtime_metrics.rs src-tauri/src/backend/mod.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs
git commit -m "feat: add bounded backend runtime metrics"
```

---

### Task 2: Bound Orchestrator Candidate Reads and Add Stable Cursor

**Files:**
- Create: `src-tauri/src/orchestrator/claim.rs`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Test: `src-tauri/src/orchestrator/repo.rs`, `src-tauri/src/orchestrator/claim.rs`

**Interfaces:**
- Consumes: Task 1 metrics types.
- Produces: `ClaimCandidate`, `ClaimScanCursor`, `list_local_queued_claim_candidates(cursor, 256)`; one JOIN returns task fields and project path.

- [ ] **Step 1: Write failing keyset/limit tests**

Insert 300 equal-priority queued tasks plus remote/Draft/Blocked rows. Assert page 1 has exactly 256, page 2 has 44, union has 300 unique IDs, ordering is `priority DESC, created_at ASC, id ASC`, and every row already carries project path without a second query.

- [ ] **Step 2: Verify failure**

Run: `cd src-tauri && cargo test --locked orchestrator::claim && cargo test --locked orchestrator::repo::tests::claim_candidate`

Expected: FAIL because bounded candidate API/cursor is absent.

- [ ] **Step 3: Implement the bounded SELECT**

Build keyset predicates for descending priority then ascending created/id; bind limit `min(requested, 256)`. JOIN `workbench_projects project` and require `project.kind='local'`, `status=queued`, `run_state=idle`. Do not begin a transaction and do not call workflow resolver.

- [ ] **Step 4: Run regression tests**

Run: `cd src-tauri && cargo test --locked orchestrator::repo && cargo test --locked orchestrator::scheduler`

Expected: PASS; existing queue/state tests unchanged.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/claim.rs src-tauri/src/orchestrator/mod.rs src-tauri/src/orchestrator/repo.rs
git commit -m "perf: bound orchestrator claim candidates"
```

---

### Task 3: Move Workflow IO Outside the CAS Transaction

**Files:**
- Modify: `src-tauri/src/orchestrator/claim.rs`
- Modify: `src-tauri/src/orchestrator/repo.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`
- Create: `src-tauri/tests/backend_scale.rs`

**Interfaces:**
- Consumes: bounded candidates/cursor, `resolve_project_workflow`, runtime metrics.
- Produces: `preflight_claim_candidates`, `claim_preflighted_candidates_with_global_capacity`, scheduler-owned cursor.

- [ ] **Step 1: Add failing race, slow-IO and fairness tests**

Add a resolver seam used only by preflight tests. Block workflow resolution on a channel, issue `repo.get_task()` concurrently, and assert it completes within 100ms. Run two concurrent CAS calls against the same eligible IDs and assert each task is returned once. Fill the first 256 with invalid projects and assert cursor reaches a valid row on the next call, then wraps after tail.

- [ ] **Step 2: Confirm old implementation fails for the intended reason**

Run: `cd src-tauri && cargo test --locked --test backend_scale orchestrator_claim -- --nocapture --test-threads=1`

Expected: slow-IO query times out or cursor/CAS API is missing; failure must not be a fixture path error.

- [ ] **Step 3: Implement preflight and short write transaction**

Group candidates by `project_id`, cap at 64 projects, call `spawn_blocking(resolve_project_workflow)` once per project, filter active states, then open a transaction. Recount active slots and CAS in original order with `EXISTS(SELECT 1 FROM workbench_projects ... kind='local')`; generate a fresh UUID token per hit. No `std::fs`, `Path::exists`, YAML parsing or project-path SELECT may occur after `begin()`.

- [ ] **Step 4: Record scheduler delay/scan metrics**

Measure expected tick deadline versus actual start, candidate/project/claimed counts, exhausted and CAS miss. Use fixed metric names only. Advance cursor only after successful bounded scan, not after DB error.

- [ ] **Step 5: Verify**

Run: `cd src-tauri && cargo test --locked --test backend_scale orchestrator_claim -- --nocapture --test-threads=1 && cargo test --locked orchestrator::scheduler && cargo test --locked orchestrator::repo`

Expected: PASS; slow resolver does not block DB; no duplicate claim; existing invalid/default workflow behavior passes.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/orchestrator/claim.rs src-tauri/src/orchestrator/repo.rs src-tauri/src/orchestrator/scheduler.rs src-tauri/tests/backend_scale.rs
git commit -m "perf: keep workflow io outside claim transactions"
```

---

### Task 4: Add CC History Manifest and Transactional Batch Repository APIs

**Files:**
- Modify: `src-tauri/src/cc/models.rs`
- Modify: `src-tauri/src/storage/cc_history_repo.rs`
- Test: inline repo tests

**Interfaces:**
- Consumes: existing `ClaudeHistoryRow`, `merge_cc_history` remains caller concern.
- Produces: `CcSyncSummary` and the three repo methods in the global Interfaces block; `bulk_ingest` becomes one transaction.

- [ ] **Step 1: Write failing repository tests**

Insert 10,001 rows and page by `after_id`; assert exact union/no duplicates. Query 128 mixed existing/missing IDs and assert returned map. Inject an invalid vector-clock serialization/row in a test-only transaction callback and assert no earlier row is committed. Assert ingest IGNORE still does not overwrite merged rows.

- [ ] **Step 2: Run failure**

Run: `cd src-tauri && cargo test --locked storage::cc_history_repo -- --nocapture`

Expected: FAIL on missing page/get-many/transaction interfaces.

- [ ] **Step 3: Implement keyset and batch SQL**

Use `WHERE id > ? ORDER BY id ASC LIMIT ?`; validate `1..=512`. Build `IN (?,...)` only for validated non-empty ≤128 IDs. Use explicit transaction for all batch writes and preserve `INSERT OR IGNORE` versus sync replacement semantics.

- [ ] **Step 4: Verify**

Run: `cd src-tauri && cargo test --locked storage::cc_history_repo && cargo test --locked cc::`

Expected: PASS; 10,001-row test finishes without unbounded single response.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/cc/models.rs src-tauri/src/storage/cc_history_repo.rs
git commit -m "perf: page and batch cc history storage"
```

---

### Task 5: Add Paged CC History Routes and Resource Limits

**Files:**
- Modify: `src-tauri/src/net/routes/cc_history.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/error_response.rs`
- Test: route tests in `cc_history.rs`

**Interfaces:**
- Consumes: Task 4 repo methods.
- Produces: manifest/items/push-batch DTOs, opaque v1 cursor codec, stable errors.

- [ ] **Step 1: Write failing route contract tests**

Cover default/max page, invalid cursor, 129 IDs, duplicate/blank/257-byte ID, 1MiB+1 content, estimated 8MiB+1 batch, missing IDs order, valid 128-item transaction, and injected row-N failure rollback. Assert exact status/code/retryable.

- [ ] **Step 2: Run failure**

Run: `cd src-tauri && cargo test --locked net::routes::cc_history -- --nocapture`

Expected: FAIL because new DTO/routes/codes are absent.

- [ ] **Step 3: Implement validation and cursor codec**

Encode `{v:1,last_id}` with the existing `base64 = "0.22"` dependency using `URL_SAFE_NO_PAD`; reject decode/JSON/version errors before DB access. Estimate bytes from fixed field UTF-8 lengths plus vector-clock serialized length; route-level body max is 8 MiB.

- [ ] **Step 4: Implement handlers**

Manifest returns `limit+1` internally to compute `done/next_cursor`; items preserves requested order and returns `missing_ids`; push reads local rows in one batch, merges, then transactionally upserts. Do not log IDs/content.

- [ ] **Step 5: Verify**

Run: `cd src-tauri && cargo test --locked net::routes::cc_history && cargo test --locked storage::cc_history_repo`

Expected: PASS, including rollback and exact 413/422/400 envelopes.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/net/routes/cc_history.rs src-tauri/src/net/http_server.rs src-tauri/src/net/error_response.rs
git commit -m "feat: add bounded paged cc history routes"
```

---

### Task 6: Implement Capability-Gated Paged Client and Mixed-Version Fallback

**Files:**
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/cc/engine.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Test: `src-tauri/src/net/peer_client.rs`, `src-tauri/src/cc/engine.rs`, `src-tauri/tests/backend_scale.rs`

**Interfaces:**
- Consumes: paged DTO/routes, `PeerProtocolInfo::supports`.
- Produces: typed `cc_sync_manifest_page`, `cc_sync_items`, `cc_sync_push_batch`; new engine path and legacy fallback.

- [ ] **Step 1: Write three mixed-version failing tests**

Start test servers for new capability, legacy health without token, and malformed paged response. Assert new↔new uses only paged routes, new client↔legacy uses only legacy routes, legacy request bodies still work against new server, malformed paged response fails the round rather than becoming empty success.

- [ ] **Step 2: Run failure**

Run: `cd src-tauri && cargo test --locked --test backend_scale cc_history_mixed_version -- --nocapture`

Expected: FAIL because capability/client methods do not exist.

- [ ] **Step 3: Add typed peer calls**

Return `Result<Dto, PeerCallError>` for new methods; never collapse error to `Vec::new()`/`bool`. Keep legacy methods unchanged for legacy fallback only. Each batch records item/estimated-byte/duration metrics with fixed labels.

- [ ] **Step 4: Implement the paged engine**

Fetch manifest pages until `done`, reject repeated/non-advancing cursor, compare with batched local rows, fetch needed remote items in chunks ≤128, merge/upsert transactionally, then page local manifest and push needed rows ≤128/8MiB. A typed `cc_history.batch_too_large` response halves the items batch until one ID; a one-ID `item_too_large` ends the round without skipping data. Cancellation/error ends the round; next scheduled sync restarts safely.

- [ ] **Step 5: Verify**

Run: `cd src-tauri && cargo test --locked cc::engine -- --nocapture && cargo test --locked net::peer_client -- --nocapture && cargo test --locked --test backend_scale cc_history -- --nocapture`

Expected: all version combinations converge; no paged error is logged as successful zero-item sync.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/net/peer_client.rs src-tauri/src/cc/engine.rs src-tauri/src/net/protocol.rs src-tauri/tests/backend_scale.rs
git commit -m "feat: sync cc history with paged capability"
```

---

### Task 7: Register Protocol Inventory and Update Durable Documentation

**Files:**
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `scripts/check-p2p-route-inventory.mjs`
- Modify: `docs/p2p-protocol.md`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`

**Interfaces:**
- Consumes: complete capability/routes/limits.
- Produces: authoritative route inventory and mixed-version/operator contract.

- [ ] **Step 1: Add failing inventory expectations**

Register expected method/path/retry class/idempotency for all three routes and capability. Run `node scripts/check-p2p-route-inventory.mjs`; expected FAIL until router and docs match.

- [ ] **Step 2: Mount routes atomically with capability**

Ensure the same build both mounts all routes and advertises `cc-history.paged-sync.v1`; add a protocol unit test asserting token presence and an axum router test asserting no 404.

- [ ] **Step 3: Document exact contracts**

Record cursor opacity, limits/error codes, v0 fallback, no partial accepted, restart-from-zero behavior, metrics privacy, and “pool still 1 unless Task 8 evidence passes”. PRD describes persistent sync behavior, not implementation history.

- [ ] **Step 4: Verify docs/inventory**

Run: `node scripts/check-p2p-route-inventory.mjs && node scripts/check-docs.mjs && node scripts/check-docs.mjs --self-test`

Expected: all exit 0; route count increases by exactly 3.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs scripts/check-p2p-route-inventory.mjs docs/p2p-protocol.md src-tauri/CLAUDE.md docs/prd.md docs/development/testing.md
git commit -m "docs: define paged cc history protocol"
```

---

### Task 8: Run Load/Fault Gates and Make the Pool Decision

**Files:**
- Modify: `src-tauri/tests/backend_scale.rs`
- Modify only if evidence passes: `src-tauri/src/backend/runtime.rs`
- Modify: `docs/development/testing.md`

**Interfaces:**
- Consumes: all previous tasks and `RuntimeMetrics::snapshot()`.
- Produces: repeatable ignored benchmark `backend_scale_benchmark`; either explicit keep-1 evidence in test output or a separate max=2 code commit.

- [ ] **Step 1: Build the ignored benchmark fixture**

Generate 10k history rows (1KiB/64KiB/1MiB distribution), 1k queued tasks, 100 projects/10 invalid workflows. Concurrently run claim ticks, paged sync, history reads and Prompt CRUD. Print machine-readable JSON to stdout with median/p95/max wait/transaction/tick, RSS and error counts; never print IDs/content/path.

- [ ] **Step 2: Add non-ignored safety assertions**

Assert no request exceeds limits, no transaction includes resolver seam, no duplicate claims, no partial batch after injected failure, and `SQLITE_BUSY` count is zero under the bounded correctness fixture.

- [ ] **Step 3: Run full correctness gates**

Run:

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --locked --test backend_scale -- --nocapture --test-threads=1
```

Expected: all exit 0.

- [ ] **Step 4: Measure pool=1 and experimental pool=2 three times each**

Run: `cd src-tauri && cargo test --release --locked --test backend_scale backend_scale_benchmark -- --ignored --nocapture --test-threads=1`

Temporarily parameterize the test pool only (not production) for 1 and 2. Expected output has six valid JSON samples and no errors. Compare §4.2 five gates from the design spec.

- [ ] **Step 5: Apply the decision**

If any gate fails, leave production `max_connections(1)` unchanged and add a testing.md sentence that the current benchmark did not authorize expansion. If all pass, make a separate minimal change to `max_connections(2)` plus `busy_timeout(5s)`, rerun all correctness gates, and reject it if locked errors regress.

- [ ] **Step 6: Commit benchmark and, only if qualified, pool change separately**

```bash
git add src-tauri/tests/backend_scale.rs docs/development/testing.md
git commit -m "test: benchmark backend scale boundaries"
# Only when every gate passes:
git add src-tauri/src/backend/runtime.rs
git commit -m "perf: allow two sqlite connections after load gate"
```

## Completion Contract

- `WORKFLOW.md` read/parse never occurs inside a DB transaction; each tick is bounded and cursor prevents permanent window starvation.
- Claim remains capacity-aware and CAS-safe under concurrent ticks.
- CC History new↔new uses bounded paged protocol; both mixed-version directions remain functional; retry converges without partial batch commits.
- Item/count/byte/cursor violations return stable typed errors and do not allocate unbounded bodies.
- Metrics are bounded/local/sanitized and provide evidence for connection wait, transaction time and scheduler delay.
- Production pool remains 1 unless the documented five-part benchmark gate explicitly authorizes 2.
- Rust quality, scale integration, route inventory and documentation checks all pass.
