# Agent Session Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立 owning-device 权威的 provider-neutral Agent session runtime，并让普通 Workbench 与 Orchestrator 共享稳定身份、phase、snapshot 和 Gap 恢复。

**Architecture:** 新增独立 `workbench_agent_sessions` repo 与 runtime reducer；terminal backend剥离 app-private OSC 后把结构化 mutation交给owner。复用现有`RuntimeEventBus`和remote ID mapping，Orchestrator一个版本dual-write旧Claude字段。

**Tech Stack:** Rust 2021, sqlx/SQLite, Tokio, Tauri event relay, axum P2P, React/TypeScript schema consumers.

## Global Constraints

- owning device 是 Agent runtime 唯一权威；remote 只映射 DTO。
- 不持久化或传输 Prompt、回复、terminal bytes、transcript path、env 或 credential；`native_session_id`可owner-local持久化但不得出现在任何projection DTO。
- OSC 单帧最大 16 KiB；每 terminal 每秒最多接受 20 个 Agent event。
- 每个 terminal 任一时刻最多一个 active Agent session。
- mutation 受 `agentSessionId + terminalSessionId + expectedVersion`保护。
- capability 固定为`workbench.agent-runtime.v1`，只做版本协商。
- P2P business API继续无调用者身份鉴权；不得把capability或opaque session ID实现为权限token。
- schema additive；Claude legacy字段dual-write一个版本。

---

## File Structure

- Create: `src-tauri/src/workbench/agent_runtime/{mod.rs,models.rs,osc.rs,reducer.rs,snapshot.rs}`。
- Create: `src-tauri/src/storage/workbench_agent_session_repo.rs`。
- Modify: `src-tauri/src/workbench/{mod.rs,sessions.rs,remote_events.rs,remote_ids.rs}`。
- Modify: `src-tauri/src/storage/mod.rs`, `src-tauri/src/state.rs`, `src-tauri/src/backend/runtime.rs`。
- Create: `src-tauri/src/commands/workbench/agent_runtime.rs`。
- Modify: `src-tauri/src/commands/workbench/mod.rs`, `src-tauri/src/net/routes/workbench.rs`, `src-tauri/src/net/http_server.rs`, `src-tauri/src/net/protocol.rs`, `src-tauri/src/lib.rs`。
- Modify: `src-tauri/src/orchestrator/{runner.rs,completion.rs,models.rs}`, `src-tauri/src/orchestrator/repo/attempts.rs`。
- Modify: `src-tauri/migrations/0001_init.sql`, `docs/p2p-protocol.md`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → T5 → T6 → T7
```

### Task 1: Add Agent Runtime Types and Additive Repository

**Files:**
- Create: `src-tauri/src/workbench/agent_runtime/models.rs`
- Create: `src-tauri/src/storage/workbench_agent_session_repo.rs`
- Modify: `src-tauri/src/workbench/agent_runtime/mod.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Produces: `AgentSessionPhase`, `AgentSessionRuntime`, `AgentRuntimeMutation`, `WorkbenchAgentSessionRepo`.

- [ ] **Step 1: Write failing repository tests**

```rust
#[tokio::test]
async fn active_agent_is_unique_per_terminal_and_version_is_cas_guarded() {
    let repo = fixture_repo().await;
    let first = repo.create_active(fixture_create("terminal-1", "claudeCodeVisible")).await.unwrap();
    let err = repo.create_active(fixture_create("terminal-1", "codexVisible")).await.unwrap_err();
    assert_eq!(err.code(), "agent_session_conflict");
    assert!(!repo.apply_mutation(&first.id, "terminal-1", first.version + 1, first.version).await.unwrap());
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench_agent_session_repo --lib`

Expected: FAIL because repo/types do not exist.

- [ ] **Step 3: Implement model and schema**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSessionPhase { Launching, Working, NeedsInput, Idle, Completed, Failed, Disconnected }

pub struct AgentRuntimeMutation {
    pub agent_session_id: String,
    pub terminal_session_id: String,
    pub expected_version: u64,
    pub phase: AgentSessionPhase,
    pub native_session_id: Option<String>,
    pub occurred_at: String,
}
```

Create `workbench_agent_sessions` with active-terminal partial unique index, resume/project/worktree/activity indexes and no content/path columns. Implement `create_active`, `apply_mutation`, `end_active_for_terminal`, `list_active`, `mark_disconnected`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked workbench_agent_session_repo --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_runtime src-tauri/src/storage/workbench_agent_session_repo.rs src-tauri/src/workbench/mod.rs src-tauri/src/storage/mod.rs src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql
git commit -m "feat(workbench): add agent session runtime storage"
```

### Task 2: Parse and Strip Agent OSC Frames

**Files:**
- Create: `src-tauri/src/workbench/agent_runtime/osc.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Test: inline tests in `src-tauri/src/workbench/agent_runtime/osc.rs` and `src-tauri/src/workbench/sessions.rs`.

**Interfaces:**
- Consumes: `AgentRuntimeMutation`.
- Produces: `AgentOscDecoder::push(&[u8]) -> AgentOscDecodeResult { visible, mutations, diagnostics }`.

- [ ] **Step 1: Write split-frame and redaction tests**

```rust
#[test]
fn split_osc_is_removed_from_visible_output() {
    let mut decoder = AgentOscDecoder::default();
    let a = decoder.push(b"before\x1b]777;cc-partner-agent-v1;eyJhZ2VudFNlc3Npb25JZCI6");
    let b = decoder.push(b"ImEifQ\x1b\\after");
    assert_eq!([a.visible, b.visible].concat(), b"beforeafter");
}
```

Add cases for invalid base64, fragmented ST, 16 KiB overflow, two frames, ordinary OSC passthrough and 20 events/second coalescing.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_runtime::osc --lib`

Expected: FAIL because decoder is absent.

- [ ] **Step 3: Implement the bounded decoder**

Implement a streaming state machine for exact prefix `\x1b]777;cc-partner-agent-v1;`, base64url JSON decode and terminal-scoped rate bucket. Feed only `visible` bytes into replay/event emission; enqueue mutations through a bounded channel rather than SQL per output chunk.

- [ ] **Step 4: Run GREEN and terminal regressions**

Run: `cd src-tauri && cargo test --locked agent_runtime::osc --lib && cargo test --locked workbench::sessions --lib`

Expected: PASS; terminal replay fixtures contain no Agent OSC payload.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_runtime/osc.rs src-tauri/src/workbench/sessions.rs
git commit -m "feat(workbench): ingest agent runtime OSC events"
```

### Task 3: Propagate Stable Terminal Context and Reduce Mutations

**Files:**
- Create: `src-tauri/src/workbench/agent_runtime/reducer.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`

**Interfaces:**
- Produces: `AgentRuntimeReducer::apply(AgentRuntimeMutation)` and `reconcile_active_sessions()`.

- [ ] **Step 1: Write context and stale-event tests**

```rust
#[tokio::test]
async fn stale_event_cannot_replace_new_active_agent() {
    let fixture = RuntimeFixture::started().await;
    let old = fixture.start_agent("terminal-1", 1).await;
    let new = fixture.replace_agent("terminal-1", 1).await;
    fixture.reduce(event(&old, 2, AgentSessionPhase::Working)).await;
    assert_eq!(fixture.active("terminal-1").await.id, new.id);
}
```

Assert tmux/raw PTY env contains only the four `CC_PARTNER_*_ID` variables and no control token.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_runtime::reducer --lib`

Expected: FAIL because reducer/context injection is absent.

- [ ] **Step 3: Implement reducer and startup reconciliation**

Use a single owner worker to serialize repo mutations. Reject mismatched terminal IDs and non-increasing versions; terminate old active row before creating replacement. On owner startup compare active rows with persisted terminal/tmux state and mark missing/exited terminals `Disconnected`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_runtime::reducer --lib && cargo test --locked workbench::sessions --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_runtime/reducer.rs src-tauri/src/workbench/sessions.rs src-tauri/src/state.rs src-tauri/src/backend/runtime.rs
git commit -m "feat(workbench): reconcile agent session runtime"
```

### Task 4: Expose Bounded Snapshot and Runtime Events

**Files:**
- Create: `src-tauri/src/workbench/agent_runtime/snapshot.rs`
- Create: `src-tauri/src/commands/workbench/agent_runtime.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/control_workbench.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `get_agent_runtime_snapshot_for_state(state, project_id) -> AgentRuntimeSnapshot` and event `workbench:agent-runtime`.

- [ ] **Step 1: Write snapshot/GAP baseline tests**

```rust
#[tokio::test]
async fn snapshot_is_stably_sorted_and_bounded() {
    let state = state_with_active_agents(1_002).await;
    let snapshot = get_agent_runtime_snapshot_for_state(&state, None).await.unwrap();
    assert_eq!(snapshot.sessions.len(), 1_000);
    assert!(snapshot.truncated);
    assert!(snapshot.sessions.windows(2).all(|w| w[0].last_activity_at >= w[1].last_activity_at));
}
```

Add serialization tests proving `nativeSessionId` is absent from Tauri/control/P2P snapshots and events.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_runtime_snapshot --lib`

Expected: FAIL because command/helper is absent.

- [ ] **Step 3: Implement snapshot and emit after durable mutation**

Capture event-bus cursor around the repo read, retry once if cursor changes, and return `ownerInstanceId/asOfSequence`. Emit only after repo commit; map the repo row to a dedicated sanitized `AgentSessionRuntimeDto` that omits `native_session_id`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_runtime_snapshot --lib && cargo test --locked backend::event_bus --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_runtime/snapshot.rs src-tauri/src/commands/workbench/agent_runtime.rs src-tauri/src/commands/workbench/mod.rs src-tauri/src/backend/control_workbench.rs src-tauri/src/backend/control_client.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): expose agent runtime snapshots"
```

### Task 5: Extend Workbench P2P Events Safely

**Files:**
- Modify: `src-tauri/src/workbench/remote_events.rs`
- Modify: `src-tauri/src/workbench/remote_ids.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `docs/p2p-protocol.md`
- Modify: `scripts/check-p2p-route-inventory.mjs`

**Interfaces:**
- Produces: capability `workbench.agent-runtime.v1`, snapshot route and `agentRuntime` event variant.

- [ ] **Step 1: Write unknown-event decoder and ID mapping tests**

```rust
#[test]
fn unknown_remote_event_is_ignored_without_reconnect() {
    let line = r#"{"event":"futureEvent","payload":{}}"#;
    assert_eq!(decode_remote_event(line).unwrap(), None);
}
```

Add tests for `remote:<deviceId>:<agentId>` mapping and old-peer unsupported response.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked workbench::remote_events --lib`

Expected: existing unknown event behavior fails the safe-ignore assertion.

- [ ] **Step 3: Harden decoder before adding variant**

Change decode result to `Result<Option<RemoteWorkbenchEvent>, AppError>`, ignore unknown event names, then add `AgentRuntime`. Add read-only snapshot route with explicit capability; do not add a LAN Hook ingestion route.

- [ ] **Step 4: Verify protocol inventory**

Run: `cd src-tauri && cargo test --locked workbench::remote_events --lib && cargo test --locked net::routes::workbench --lib && cd .. && node scripts/check-p2p-route-inventory.mjs`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/remote_events.rs src-tauri/src/workbench/remote_ids.rs src-tauri/src/net/routes/workbench.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs docs/p2p-protocol.md scripts/check-p2p-route-inventory.mjs
git commit -m "feat(p2p): relay agent runtime state"
```

### Task 6: Bridge Orchestrator Attempts to Unified Runtime

**Files:**
- Create: `src-tauri/src/orchestrator/agent_runtime_bridge.rs`
- Modify: `src-tauri/src/orchestrator/{runner.rs,completion.rs,models.rs}`
- Modify: `src-tauri/src/orchestrator/repo/{schema.rs,attempts.rs,tests.rs}`

**Interfaces:**
- Produces: attempt `agent_session_id`, `record_runner_activity`, `handle_normalized_agent_event`.

- [ ] **Step 1: Write legacy parity and completion ordering tests**

```rust
#[tokio::test]
async fn completion_updates_agent_before_task_enters_verifying() {
    let fixture = OrchestratorRuntimeFixture::running().await;
    fixture.complete_agent().await.unwrap();
    assert_eq!(fixture.agent_phase().await, AgentSessionPhase::Completed);
    assert_eq!(fixture.task_attempt_phase().await, OrchestratorAttemptPhase::Verifying);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_runtime_bridge --lib`

Expected: FAIL because bridge/attempt field is absent.

- [ ] **Step 3: Implement dual-write bridge**

Create unified Agent row when Runner terminal is created; persist its ID on attempt/task. Update runtime before task transition. Continue writing legacy Claude fields for one version, but never expose transcript path in unified DTO.

- [ ] **Step 4: Run GREEN and Claude regressions**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_runtime_bridge --lib && cargo test --locked orchestrator::runner --lib && cargo test --locked orchestrator::completion --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/agent_runtime_bridge.rs src-tauri/src/orchestrator/runner.rs src-tauri/src/orchestrator/completion.rs src-tauri/src/orchestrator/models.rs src-tauri/src/orchestrator/repo/schema.rs src-tauri/src/orchestrator/repo/attempts.rs src-tauri/src/orchestrator/repo/tests.rs
git commit -m "feat(orchestrator): use unified agent runtime"
```

### Task 7: Mixed-version, Downgrade and Documentation Gates

**Files:**
- Modify: `src-tauri/src/net/discovery.rs`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `docs/prd.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `docs/development/backend-operations.md`

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces: capability declaration, downgrade procedure and evidence rows.

- [ ] **Step 1: Add mixed-version tests**

Cover old DB null fields→Claude fallback, old peer unsupported, owner restart with active rows, and downgrade refusal while non-Claude sessions are active.

- [ ] **Step 2: Run focused and full Rust gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked workbench::agent_runtime && cargo test --locked orchestrator::agent_runtime_bridge && cargo test --locked net::routes::workbench`

Expected: all exit 0.

- [ ] **Step 3: Run docs/protocol gates**

Run: `node scripts/check-p2p-route-inventory.mjs && node scripts/check-quality-traceability.mjs && node scripts/check-docs.mjs`

Expected: all exit 0.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/net/discovery.rs src-tauri/CLAUDE.md docs/prd.md docs/development/quality-matrix.json docs/development/backend-operations.md
git commit -m "docs: define agent runtime operations"
```

## Completion Contract

- Ordinary Workbench and Orchestrator sessions share one Agent runtime truth.
- OSC is bounded, stripped from visible output and protected by version/terminal identity.
- Snapshot/event/Gap and remote unsupported behaviors are tested.
- No content/path/credential field enters runtime storage or P2P DTO; provider-native session ID remains owner-local.

## Plan Self-Review

- Spec coverage: storage, OSC, reconciliation, snapshot, P2P, Orchestrator and rollback each map to a task.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: internal `AgentSessionRuntime`, mutation input and sanitized `AgentSessionRuntimeDto` are explicitly separated; snapshots/events use only the DTO.
