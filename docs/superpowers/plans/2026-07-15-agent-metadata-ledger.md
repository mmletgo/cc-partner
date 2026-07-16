# Agent Metadata Ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从Agent Runtime终态自动生成metadata-only历史，提供可靠用量聚合、有限本机明细与自动保留清理，同时让LAN Fleet只读取owning device的时间窗聚合。

**Architecture:** A1 runtime终态是写入触发器，A3 adapter仅提供可验证的cumulative usage snapshot；Ledger以`agent_session_id`唯一行做幂等finalize/null-fill，不参与runtime/task真值。Owner-local repo负责分页、聚合和清理，P2P只暴露限定project/time-window summary，前端在Fleet详情和Workbench二级drawer中呈现。

**Tech Stack:** Rust 2021, sqlx SQLite, Tokio background task, A1 Agent Runtime/A3 Adapter events, axum P2P, React 19, existing Workbench/Fleet/Settings controllers, Vitest/Playwright.

## Global Constraints

- 不保存Prompt、回复、terminal bytes、diff、transcript path、cwd、env、secret、native session ID或provider credential。
- provider/model/token/cost只有adapter提供可靠结构化字段时写入；unknown保持`null`，不得通过文本或价格表估算。
- `agent_session_id`唯一；终态重放不新增row，只允许可靠null-fill或更正endedAt/duration。
- 默认保留30天且每device最多10,000条；启动和每24小时清理，单批最多500条，无用户配置负担。
- 本机详情默认50、最大200；remote只提供24h/7d/30d aggregate，不提供entry列表。
- Ledger失败不能阻断Agent或Orchestrator完成，也不能成为scheduler/runtime的真值。
- Ledger不进入Prompt/SSH/Scratchpad/GitHub sync；Fleet缓存不反向写Ledger。
- UI不新增Sidebar页面、标签/备注、排行榜、手工计费或装饰性统计卡。
- fixed LAN仍无身份鉴权；不得新增token、配对或可信设备概念。

---

## File Structure

- Create: `src-tauri/src/workbench/agent_ledger/{mod.rs,models.rs,service.rs,aggregation.rs,retention.rs}`。
- Create: `src-tauri/src/storage/agent_ledger_repo.rs`。
- Create: `src-tauri/src/commands/workbench/agent_ledger.rs`。
- Modify: `src-tauri/src/{workbench/mod.rs,storage/mod.rs,state.rs}`、`src-tauri/src/backend/runtime.rs` and Workbench command registration.
- Modify: `src-tauri/src/workbench/agent_runtime/{mod.rs,reducer.rs,snapshot.rs}` for terminal-finalization/reconciliation.
- Modify: `src-tauri/src/orchestrator/agent_adapter/{mod.rs,types.rs,registry.rs,claude_code.rs,codex.rs,generic_terminal.rs}` for usage normalization.
- Modify: `src-tauri/src/backend/{control_workbench.rs,control_client.rs}`。
- Modify: `src-tauri/src/workbench/remote_client.rs`, `src-tauri/src/net/routes/workbench.rs`, `src-tauri/src/net/{protocol.rs,discovery.rs,http_server.rs}`。
- Modify: `src-tauri/migrations/0001_init.sql`; new repo `ensure_schema` is initialized from `src-tauri/src/backend/runtime.rs`.
- Create: `web/src/lib/types/agentLedger.ts`, `web/src/lib/schemas/{agentLedger.ts,agentLedger.test.ts}`。
- Modify: `web/src/lib/types/index.ts`, `web/src/lib/schemas/index.ts`, Workbench API/transport modules.
- Create: `web/src/pages/Workbench/views/{AgentLedgerDrawer.tsx,AgentLedgerDrawer.test.tsx}`。
- Modify: `web/src/pages/Workbench/views/WorkbenchFleetView.tsx`、`web/src/hooks/useLanAgentFleet.ts`、`web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts`、`web/src/pages/Workbench/{Workbench.tsx,Workbench.module.css}`。
- Modify: `web/src/pages/Settings/{SettingsGeneralPanel.tsx,useSettingsController.ts,Settings.test.tsx}`。
- Modify: `docs/prd.md`, `docs/p2p-protocol.md`, `docs/development/{testing.md,quality-matrix.json}`, `web/CLAUDE.md`, `src-tauri/CLAUDE.md`。

## Task Dependency Graph

```text
T1 → T2 → T3
 │    └──→ T4 → T5 → T6 → T7
 └────────→ T4

External: A6 T3 owner collector → T5 remote summary join
          A6 T5 Fleet view       → T6 frontend integration
```

### Task 1: Add the Metadata-only Schema and Idempotent Repository

**Files:**
- Create: `src-tauri/src/workbench/agent_ledger/{mod.rs,models.rs}`
- Create: `src-tauri/src/storage/agent_ledger_repo.rs`
- Modify: `src-tauri/src/workbench/mod.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Produces: `AgentLedgerEntry`, `AgentLedgerOutcome`, `ReliableUsageSnapshot`, `AgentLedgerRepo::{finalize,get_page,clear_all}`.

- [ ] **Step 1: Write migration, idempotency, null-fill, and validation tests**

```rust
#[tokio::test]
async fn terminal_replay_fills_reliable_usage_without_duplicate_entry() {
    let repo = ledger_repo().await;
    repo.finalize(finalize("a1", None)).await.unwrap();
    repo.finalize(finalize("a1", Some(usage(120, 40, "USD", 3)))).await.unwrap();
    let rows = repo.page(default_query()).await.unwrap().items;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, Some(120));
    assert_eq!(rows[0].cost_currency.as_deref(), Some("USD"));
}
```

Add existing-database upgrade, `agent_session_id` uniqueness, invalid outcome, lowercase/non-3-char currency rejection, negative/overflow/lossy decimal conversion rejection, counter rollback, conflicting provider/project, later endedAt duration correction, and DTO field scan for forbidden names.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_ledger_repo --lib && cargo test --locked workbench::agent_ledger::models --lib`

Expected: FAIL because table/models/repo are absent.

- [ ] **Step 3: Add the table and monotonic finalize transaction**

Create `agent_session_ledger(id,agent_session_id UNIQUE,project_id,worktree_id,provider_id,model_id,started_at,ended_at,duration_ms,outcome,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cost_minor_units,cost_currency,created_at,updated_at)` plus project/provider/ended indexes.

`finalize` inserts the first terminal snapshot. On conflict it may fill nullable reliable fields, take a later valid `ended_at`, recompute nonnegative duration, and accept monotonic cumulative counters; it must reject identity changes, currency mismatch and counter rollback. Use checked integer conversions and write cost only when provider amount converts losslessly with the currency's ISO exponent; never round.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_ledger_repo --lib && cargo test --locked workbench::agent_ledger::models --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_ledger src-tauri/src/workbench/mod.rs src-tauri/src/storage src-tauri/src/state.rs src-tauri/src/backend/runtime.rs src-tauri/migrations/0001_init.sql
git commit -m "feat(workbench): persist agent metadata ledger"
```

### Task 2: Finalize Entries from Runtime and Reliable Adapter Usage

**Files:**
- Create: `src-tauri/src/workbench/agent_ledger/service.rs`
- Modify: `src-tauri/src/workbench/agent_runtime/{mod.rs,reducer.rs,snapshot.rs}`
- Modify: `src-tauri/src/orchestrator/agent_adapter/{types.rs,registry.rs,claude_code.rs,codex.rs,generic_terminal.rs}`
- Modify: `src-tauri/src/{state.rs,lib.rs}` and `src-tauri/src/backend/runtime.rs`

**Interfaces:**
- Produces: `AgentLedgerService::{record_terminal,reconcile_terminal_sessions}`, reliable cumulative usage merge, bounded retry metric.

- [ ] **Step 1: Write terminal-transition and failure-isolation tests**

```rust
#[tokio::test]
async fn ledger_failure_never_changes_runtime_terminal_outcome() {
    let fixture = runtime_with_failing_ledger().await;
    fixture.complete_agent("a1").await.unwrap();
    assert_eq!(fixture.runtime("a1").await.phase, AgentPhase::Completed);
    assert_eq!(fixture.ledger_retry_count(), 1);
    assert_eq!(fixture.ledger_failure_metric(), 1);
}
```

Add every terminal outcome mapping, nonterminal event no-write, duplicate terminal versions, restart reconciliation, usage arriving before/after terminal, unstructured provider output→null, adapter counter rollback, and no transcript/message/path fields passed to the service.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_ledger_service --lib && cargo test --locked agent_runtime --lib && cargo test --locked agent_adapter --lib`

Expected: FAIL because runtime/adapter are not wired to the Ledger.

- [ ] **Step 3: Integrate asynchronously without changing completion truth**

On the first observed A1 terminal transition, enqueue a bounded finalize request containing only IDs/provider/model/timestamps/outcome and reliable usage. Adapter events expose typed cumulative snapshots, never regex-parsed stdout. On write failure increment a bounded metric and retry once in background; swallow the second error after structured metadata-only logging.

At owner startup, scan terminal runtimes missing a Ledger row and reconcile them. This scan must not reopen terminal transcripts or resume sessions.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_ledger_service --lib && cargo test --locked agent_runtime --lib && cargo test --locked agent_adapter --lib`

Expected: PASS; runtime outcome remains correct under forced Ledger failure.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_ledger src-tauri/src/workbench/agent_runtime src-tauri/src/orchestrator/agent_adapter src-tauri/src/lib.rs
git commit -m "feat(workbench): finalize ledger from agent runtime"
```

### Task 3: Enforce Automatic Retention in Bounded Batches

**Files:**
- Create: `src-tauri/src/workbench/agent_ledger/retention.rs`
- Modify: `src-tauri/src/storage/agent_ledger_repo.rs`
- Modify: `src-tauri/src/backend/runtime.rs`

**Interfaces:**
- Produces: `cleanup_agent_ledger_batch`, `AgentLedgerRetentionTask`, injectable clock.

- [ ] **Step 1: Write virtual-clock age/cap/batch tests**

```rust
#[tokio::test]
async fn cleanup_deletes_at_most_five_hundred_oldest_rows() {
    let fixture = retention_fixture().rows_older_than_days(30, 620).await;
    let result = fixture.run_one_batch().await.unwrap();
    assert_eq!(result.deleted, 500);
    assert!(result.more_remaining);
    assert_eq!(fixture.row_count().await, 120);
}
```

Cover exactly-30-day boundary, 10,000/10,001 rows, deterministic `ended_at ASC,id`, age-first then cap, startup run, next tick at24h, shutdown cancellation, and cleanup failure not blocking backend startup.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_ledger::retention --lib`

Expected: FAIL because retention service is absent.

- [ ] **Step 3: Implement one bounded transaction per tick**

Delete up to500 age-expired rows; if fewer than500 were deleted, delete oldest rows exceeding10,000 with the remaining batch budget. Return `more_remaining` but wait for the next daily tick as specified; do not loop aggressively at startup. Use the shared shutdown signal.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_ledger::retention --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_ledger/retention.rs src-tauri/src/storage/agent_ledger_repo.rs src-tauri/src/lib.rs
git commit -m "feat(workbench): retain bounded agent ledger history"
```

### Task 4: Add Local Pagination and Reliable Time-window Aggregation

**Files:**
- Create: `src-tauri/src/workbench/agent_ledger/aggregation.rs`
- Create: `src-tauri/src/commands/workbench/agent_ledger.rs`
- Modify: `src-tauri/src/commands/workbench/mod.rs`
- Modify: `src-tauri/src/backend/{control_workbench.rs,control_client.rs}`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `AgentLedgerQuery`, opaque cursor, `AgentLedgerSummary`, `LedgerUsageCoverage`, local list/summary/clear commands.

- [ ] **Step 1: Write cursor/filter/window/coverage tests**

```rust
#[tokio::test]
async fn aggregate_marks_usage_partial_instead_of_converting_unknown_to_zero() {
    let fixture = ledger_fixture()
        .entry(reliable_usage_entry(10, 4))
        .entry(entry_without_usage())
        .await;
    let summary = fixture.summary(LedgerWindow::SevenDays).await.unwrap();
    assert_eq!(summary.input_tokens, Some(10));
    assert_eq!(summary.usage_coverage, LedgerUsageCoverage::Partial);
}
```

Cover default50/max200, invalid cursor, stable `ended_at DESC,id` pagination, project/provider/outcome/time filters, exact 24h/7d/30d boundaries, complete/partial/unavailable coverage, multi-currency buckets, zero sessions, and clear leaving runtime/task/session tables unchanged.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_ledger::aggregation --lib && cargo test --locked commands::workbench::agent_ledger --lib`

Expected: FAIL because query/aggregation/commands are absent.

- [ ] **Step 3: Implement bounded local query and aggregate helpers**

Use an opaque versioned base64url cursor containing only endedAt/id ordering fields. Closed filters become bound SQL parameters. Aggregate token/cost only from non-null values and compute coverage from contributing/total sessions; group cost by validated currency rather than converting currencies.

Expose desktop invoke and local control operations through the same service. Clear requires an explicit call, is idempotent, and returns deleted row count.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_ledger::aggregation --lib && cargo test --locked commands::workbench::agent_ledger --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench/agent_ledger src-tauri/src/commands/workbench src-tauri/src/backend src-tauri/src/lib.rs
git commit -m "feat(workbench): query and aggregate agent ledger"
```

### Task 5: Expose Aggregate-only Owning-device P2P Summary

**Files:**
- Modify: `src-tauri/src/workbench/remote_client.rs`
- Modify: `src-tauri/src/net/routes/workbench.rs`
- Modify: `src-tauri/src/net/{protocol.rs,discovery.rs,http_server.rs}`
- Modify: `src-tauri/src/workbench/lan_fleet/{models.rs,collector.rs}`
- Modify: `docs/p2p-protocol.md`

**Interfaces:**
- Produces: `POST /api/workbench/agent-ledger/summary`, `workbench.agent-ledger-summary.v1`, `RemoteWorkbenchClient::agent_ledger_summary`.

- [ ] **Step 1: Write local-project-only, aggregate-shape, and capability tests**

```rust
#[tokio::test]
async fn remote_summary_never_serializes_ledger_entries() {
    let response = post_summary(&route_fixture().entry(secret_free_entry()).await, local_project_ids(2)).await.unwrap();
    let json = serde_json::to_value(response).unwrap();
    assert!(json.get("entries").is_none());
    assert!(json.to_string().contains("sessions"));
    assert!(!json.to_string().contains("agentSessionId"));
}
```

Cover remote-wrapped project rejection, unknown project, max100 project IDs, invalid window, old peer unsupported, offline owner, request ID envelope, fixed LAN headers, and one field failure preserving other Fleet values.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked net::routes::workbench --lib && cargo test --locked workbench::remote_client --lib && cargo test --locked lan_fleet --lib`

Expected: FAIL because summary route/capability are absent.

- [ ] **Step 3: Add owner-local aggregate route and Fleet join**

Accept only local inner project IDs and the three enum windows; call aggregation directly and return per-project summaries with no entry/session IDs. The controller groups saved shortcuts by owner and maps summaries back through existing remote ID helpers. Unsupported displays unavailable/unsupported, never numeric zero.

- [ ] **Step 4: Run GREEN and protocol inventory**

Run:

```bash
cd src-tauri && cargo test --locked net::routes::workbench --lib && cargo test --locked workbench::remote_client --lib && cargo test --locked lan_fleet --lib && cargo test --locked net::protocol --lib
node scripts/check-p2p-route-inventory.mjs
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/workbench src-tauri/src/net docs/p2p-protocol.md
git commit -m "feat(p2p): expose aggregate agent ledger summary"
```

### Task 6: Add Fleet Activity, Local History Drawer, and One-click Clear

**Files:**
- Create: `web/src/lib/types/agentLedger.ts`
- Create: `web/src/lib/schemas/{agentLedger.ts,agentLedger.test.ts}`
- Modify: `web/src/lib/types/index.ts`
- Modify: `web/src/lib/schemas/index.ts`
- Modify: `web/src/api/{workbench.ts,workbenchHttp.ts,workbenchHttp.test.ts,workbenchTransport.ts}`
- Create: `web/src/pages/Workbench/views/{AgentLedgerDrawer.tsx,AgentLedgerDrawer.test.tsx}`
- Modify: `web/src/pages/Workbench/views/WorkbenchFleetView.tsx`
- Modify: `web/src/hooks/useLanAgentFleet.ts`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchProjectController.ts`
- Modify: `web/src/pages/Workbench/{Workbench.tsx,Workbench.module.css}`
- Modify: `web/src/pages/Settings/{SettingsGeneralPanel.tsx,useSettingsController.ts,Settings.test.tsx}`

**Interfaces:**
- Produces: strict Ledger DTO decoder, Fleet `Agent activity`, paginated local drawer, clear confirmation.

- [ ] **Step 1: Write strict schema and low-burden UI tests**

```tsx
it('renders unavailable usage as 未提供 rather than zero', () => {
  render(<AgentLedgerDrawer {...fixtureProps({ inputTokens: null, outputTokens: null })} />)
  expect(screen.getByText('未提供')).toBeVisible()
  expect(screen.queryByText('0 tokens')).toBeNull()
})
```

Add partial coverage label, multi-currency display without conversion, load-more cursor, filter bounds, local-only drawer availability, remote Fleet aggregate, unsupported peer, clear Dialog focus/confirmation/cancel, successful clear refresh, no top-level nav, and no forbidden metadata text.

- [ ] **Step 2: Run RED**

Run: `cd web && npm test -- agentLedger AgentLedgerDrawer WorkbenchFleetView Settings.test.tsx`

Expected: FAIL because schema/UI/actions are absent.

- [ ] **Step 3: Integrate into existing ownership boundaries**

Extend A6 Fleet DTO/hook with summaries. Workbench project controller owns local history loading and passes props to a view-only Drawer; views import no `@/api/*`. Reuse existing `Drawer`, `Dialog`, `Button`, `Pill`, tokens and typography. Settings adds one destructive confirmation under existing data controls; no retention setting, export, tags or notes.

- [ ] **Step 4: Run GREEN, lint, build, and architecture guards**

Run:

```bash
cd web && npm test -- agentLedger AgentLedgerDrawer WorkbenchFleetView Settings.test.tsx settingsOwnership.test.ts
cd web && npm run lint
cd web && npm run build
```

Expected: PASS; Hooks precede early returns, views do not import APIs, Workbench has no new controller, and unknown is never rendered as0.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib web/src/api web/src/hooks/useLanAgentFleet.ts web/src/pages/Workbench web/src/pages/Settings
git commit -m "feat(workbench): present agent metadata history"
```

### Task 7: Verify Privacy, Retention, and Cross-device Evidence

**Files:**
- Create: `web/tests/agent-metadata-ledger.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/development/{testing.md,quality-matrix.json}`
- Modify: `web/CLAUDE.md`
- Modify: `src-tauri/CLAUDE.md`
- Create: `src-tauri/tests/agent_ledger_privacy.rs`

**Interfaces:**
- Produces: end-to-end evidence that Ledger is automatic, bounded, metadata-only, and aggregate-only over LAN.

- [ ] **Step 1: Add E2E and payload/log/sync privacy scans**

Cover terminal Agent completion→one entry, later reliable usage null-fill, owner restart reconciliation, local drawer pagination, remote Fleet summary, old peer unsupported, clear history, and runtime/task unaffected. Scan serialized DTOs, error envelopes, tracing fixtures and all sync payloads for `prompt|response|terminalBytes|transcriptPath|cwd|env|nativeSessionId|credential` fields.

- [ ] **Step 2: Run RED**

Run: `cd web && npm run test:e2e -- agent-metadata-ledger.spec.ts`

Expected: FAIL until full backend/frontend flow and fixtures are wired.

- [ ] **Step 3: Update persistent behavior and quality evidence**

Document automatic creation, reliable-only usage, 30-day/10,000/500 limits, local detail, remote aggregate-only capability, clear semantics and non-sync guarantee. Add stable quality IDs; unrun cross-platform scenarios stay `NOT VERIFIED`.

- [ ] **Step 4: Run final gates**

Run:

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets --locked -- -D warnings
cd src-tauri && cargo test --locked agent_ledger --lib
cd web && npm run lint && npm run build
cd web && npm test -- agentLedger AgentLedgerDrawer WorkbenchFleetView Settings.test.tsx
cd web && npm run test:e2e -- agent-metadata-ledger.spec.ts
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add docs web src-tauri
git commit -m "docs(workbench): verify agent metadata ledger"
```

## Completion Contract

- A terminal Agent session produces at most one Ledger row; replay only fills reliable missing usage.
- Unknown model/token/cost remains null and UI renders“未提供”，not zero.
- Ledger failure cannot change runtime/task completion and performs at most one background retry.
- Cleanup enforces age/cap with ≤500 rows per daily/startup batch.
- Local pagination/filter bounds and all three summary windows are tested.
- Remote route accepts owner-local projects and returns aggregate only, never entry/session IDs.
- Clear deletes only Ledger rows; sync, runtime, task, evidence and terminal state remain unchanged.
- No new top-level navigation or recurring user maintenance is introduced.
- Rust/frontend/E2E/protocol/docs/privacy checks pass.

## Plan Self-review

- Runtime remains the source of truth; Ledger is an optional derived record.
- Reliable usage and coverage semantics prevent false precision.
- Automatic bounded retention protects storage without adding a setting the user must understand.
- P2P exposure is narrower than local detail and preserves the fixed LAN philosophy without inventing authentication.
