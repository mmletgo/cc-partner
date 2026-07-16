# Agent-first CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增独立`cc-partner` Agent控制CLI，以稳定selector、JSON/JSONL、exit code和本机/远端transport合同暴露既有Workbench、Agent、Orchestrator、Fleet与Browser能力。

**Architecture:** CLI只负责参数解析、selector解析、transport选择和机器可读呈现；领域读写继续由既有service/command helper执行。本机通过loopback control descriptor和typed agent endpoints访问共享`AppState`，远端通过显式device ID复用P2P route/capability；不直接访问SQLite，也不把GUI局部状态变成隐式selector。

**Tech Stack:** Rust 2021, `clap = 4.5.32` derive（兼容项目Rust 1.77.2）, serde/serde_json, existing backend control descriptor, reqwest/axum P2P, Cargo integration smoke tests.

## Global Constraints

- 保留`cc-partner-backend start|serve|stop|status|doctor`，`cc-partner`是独立bin且不进入Tauri `externalBin`。
- v1只接受`local`或`id:<deviceId>`、`id:<entityId>`、规范化精确path/branch；禁止`active/current/recent/name`、fuzzy picker和自动remote选择。
- Prompt、goal、terminal bytes和browser fill value只从stdin/`--input-json -`读取；不得进入argv、日志或错误envelope。
- `--json` stdout只能有一个JSON；`event follow`只能输出JSONL；诊断到stderr。
- query只可在刷新control descriptor后重试一次；non-replayable mutation只发送一次，连接丢失返回`outcomeUnknown=true`。
- remote业务API保持固定LAN无身份鉴权边界；不得发送本机control token或把peer描述为认证/可信设备。
- CLI不得绕过Agent/Orchestrator claim、provider approval、browser verification或delivery状态机。
- 不实现全局Quick Open、Command Recipe或用户Diff审查流。

---

## File Structure

- Create: `src-tauri/src/bin/cc-partner.rs`。
- Create: `src-tauri/src/agent_cli/{mod.rs,args.rs,output.rs,selectors.rs,protocol.rs,client.rs,remote.rs}`。
- Create: `src-tauri/src/backend/control_agent.rs`。
- Modify: `src-tauri/src/backend/{mod.rs,control.rs,control_api.rs,control_client.rs}` and `src-tauri/src/net/http_server.rs`。
- Modify: `src-tauri/src/lib.rs` to export the CLI module to the standalone bin.
- Modify: `src-tauri/src/commands/workbench/{projects.rs,sessions.rs,browser.rs,fleet.rs,browser_verification.rs}`、`src-tauri/src/commands/orchestrator/{tasks.rs,actions.rs,runtime.rs,experiments.rs}` and `src-tauri/src/commands/attention.rs` so CLI and Tauri commands share typed service entrypoints.
- Modify: `src-tauri/Cargo.toml`, `Cargo.lock`, `.github/workflows/release-tauri.yml`。
- Create: `src-tauri/tests/agent_cli_smoke.rs` and CLI fixture helpers.
- Modify: `README.md`, `docs/development/backend-operations.md`, `docs/development/testing.md`, `docs/development/quality-matrix.json`, `src-tauri/CLAUDE.md`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → T5 → T6 → T7
                ↘ T5 ↗
```

### Task 1: Add the Separate Binary, Parser, JSON Envelope, and Exit Codes

**Files:**
- Create: `src-tauri/src/bin/cc-partner.rs`
- Create: `src-tauri/src/agent_cli/{mod.rs,args.rs,output.rs}`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `Cli`, `DeviceSelector`, resource/action enums, `CliSuccess<T>`, `CliFailure`, `CliExitCode`, `dispatch`.

- [ ] **Step 1: Write parser, envelope, and stream-output tests**

```rust
#[test]
fn json_failure_maps_conflict_to_exit_four_without_stdout_noise() {
    let rendered = render_failure(CliError::conflict("ambiguous_selector"), true);
    assert_eq!(rendered.exit_code, CliExitCode::Conflict as i32);
    assert_eq!(rendered.stderr, "");
    let body: serde_json::Value = serde_json::from_str(&rendered.stdout).unwrap();
    assert_eq!(body["schemaVersion"], 1);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"]["outcomeUnknown"], false);
}
```

Cover all exit codes `0..=7`, missing action, invalid `--device`, `--json` stdout isolation, and `event follow` JSONL one-event-per-line behavior.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_cli::args --lib && cargo test --locked agent_cli::output --lib`

Expected: FAIL because the new bin/module and clap dependency do not exist.

- [ ] **Step 3: Implement the command tree and stable envelope**

Pin `clap = { version = "=4.5.32", features = ["derive"] }`. Define the approved command surface exactly; use `ValueEnum` only for closed vocabularies and custom parsers for selector prefixes. Map usage/not-found/conflict/unavailable/unsupported/partial without localized string inspection.

```rust
#[repr(i32)]
pub enum CliExitCode {
    Success = 0,
    Internal = 1,
    Usage = 2,
    NotFound = 3,
    Conflict = 4,
    Unavailable = 5,
    Unsupported = 6,
    Partial = 7,
}
```

Keep `cc-partner-backend` definitions unchanged, add a separate `[[bin]]` for `cc-partner`, and export `pub mod agent_cli` from the library for the thin bin entrypoint.

- [ ] **Step 4: Run GREEN and binary help checks**

Run: `cd src-tauri && cargo test --locked agent_cli::args --lib && cargo test --locked agent_cli::output --lib && cargo run --locked --bin cc-partner -- --help`

Expected: tests PASS; help lists the approved resources and no backend lifecycle commands.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/bin/cc-partner.rs src-tauri/src/agent_cli src-tauri/src/lib.rs src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "feat(cli): add agent-first command surface"
```

### Task 2: Implement Exact Selectors and Bounded Stdin Input

**Files:**
- Create: `src-tauri/src/agent_cli/selectors.rs`
- Modify: `src-tauri/src/agent_cli/{args.rs,mod.rs}`
- Test: inline tests in `src-tauri/src/agent_cli/{selectors.rs,args.rs}`.

**Interfaces:**
- Produces: `ProjectSelector`, `WorktreeSelector`, `EntitySelector`, `read_input_json`, `resolve_exact_*`.

- [ ] **Step 1: Write normalization, ambiguity, and privacy tests**

```rust
#[test]
fn exact_branch_with_multiple_worktrees_is_conflict() {
    let rows = vec![worktree("w1", "feature/x"), worktree("w2", "feature/x")];
    let error = resolve_exact_worktree(&WorktreeSelector::Branch("feature/x".into()), &rows)
        .unwrap_err();
    assert_eq!(error.code(), "ambiguous_selector");
}
```

Add canonical path equality, wrong prefix, empty stdin, stdin over 1MiB, terminal body over256KiB, malformed JSON, and an assertion that rendered errors never contain the secret fixture text.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_cli::selectors --lib`

Expected: FAIL because selector/input helpers are absent.

- [ ] **Step 3: Implement closed selectors and pre-transport limits**

Resolve only against typed candidate lists returned by the domain query; normalize paths using the existing Workbench canonicalization helper. Read stdin into a bounded buffer before deserialization, reject body-bearing arguments other than literal `-`, and never include the body in `Debug`, tracing fields, or error messages.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_cli::selectors --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_cli
git commit -m "feat(cli): add exact selectors and bounded stdin"
```

### Task 3: Add Typed Local Query Control Plane

**Files:**
- Create: `src-tauri/src/backend/control_agent.rs`
- Create: `src-tauri/src/agent_cli/{protocol.rs,client.rs}`
- Modify: `src-tauri/src/backend/{mod.rs,control.rs,control_api.rs,control_client.rs}`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/commands/workbench/{projects.rs,sessions.rs,browser.rs,fleet.rs,browser_verification.rs}`
- Modify: `src-tauri/src/commands/orchestrator/{tasks.rs,runtime.rs,experiments.rs}`
- Modify: `src-tauri/src/commands/attention.rs`

**Interfaces:**
- Produces: `AgentControlQuery`, `AgentControlQueryResult`, `POST /api/backend/control/agent/query`, `AgentCliClient::query`.

- [ ] **Step 1: Write query purity, auth, size, and refresh tests**

```rust
#[tokio::test]
async fn session_read_query_never_spawns_or_restores_a_terminal() {
    let fixture = control_agent_fixture().persisted_session_without_backend().await;
    let response = fixture.query(AgentControlQuery::SessionRead {
        session_id: fixture.session_id(),
        after_sequence: Some(0),
    }).await.unwrap();
    assert!(response.is_session_read());
    assert_eq!(fixture.terminal_spawn_count(), 0);
    assert_eq!(fixture.terminal_write_count(), 0);
}
```

Add non-loopback rejection, missing/wrong control token, query body>256KiB, response>1MiB, first connection failure then refreshed descriptor success, second failure→unavailable, and `AgentInspect` output excluding provider-native session ID/transcript path/launch environment.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked backend::control_agent --lib && cargo test --locked agent_cli::client --lib`

Expected: FAIL because typed agent control query is absent.

- [ ] **Step 3: Implement closed query variants**

```rust
pub enum AgentControlQuery {
    ProjectList,
    ProjectInspect { selector: ProjectSelector },
    WorktreeList { project: ProjectSelector },
    SessionList { project: ProjectSelector, worktree: Option<WorktreeSelector> },
    SessionRead { session_id: String, after_sequence: Option<u64> },
    AgentList { project: ProjectSelector },
    AgentInspect { agent_session_id: String },
    AgentWait { agent_session_id: String, phase: String, timeout_ms: u64 },
    TaskList { project: ProjectSelector },
    ExperimentInspect { experiment_id: String },
    AttentionList,
    FleetSnapshot,
    BrowserDiscover { project: ProjectSelector },
    BrowserInspect { run_id: String },
}
```

Route through existing service/helper functions, never command-to-command invocation or direct repo access from CLI. Enforce envelope byte limits before writing the response. Client may re-read the descriptor and retry the query exactly once.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked backend::control_agent --lib && cargo test --locked agent_cli::client --lib`

Expected: PASS with spawn/write counters at zero.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend src-tauri/src/agent_cli src-tauri/src/workbench src-tauri/src/orchestrator src-tauri/src/attention
git commit -m "feat(cli): expose typed local queries"
```

### Task 4: Add Mutation Idempotency and Unknown-outcome Semantics

**Files:**
- Modify: `src-tauri/src/backend/control_agent.rs`
- Modify: `src-tauri/src/agent_cli/{protocol.rs,client.rs,mod.rs}`
- Modify: `src-tauri/src/workbench/sessions.rs`、`src-tauri/src/commands/workbench/{projects.rs,sessions.rs,agent_runtime.rs,browser_verification.rs}`、`src-tauri/src/commands/orchestrator/{actions.rs,experiments.rs}`。

**Interfaces:**
- Produces: `AgentControlMutation`, `AgentControlMutationResult`, `POST /api/backend/control/agent/mutate`, domain retry classification.

- [ ] **Step 1: Write hit-count and reconciliation tests**

```rust
#[tokio::test]
async fn terminal_send_connection_loss_is_never_replayed() {
    let transport = FakeTransport::drop_after_apply();
    let error = client(transport.clone()).mutate(terminal_send("s1", b"pwd\n")).await.unwrap_err();
    assert_eq!(transport.hit_count(), 1);
    assert!(error.outcome_unknown());
}
```

Cover task cancel/retry with `clientRequestId` reconciliation, experiment create with stable request ID, worktree create without server dedupe, browser click/fill non-replayability, and an idempotent cancel already applied.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked backend::control_agent --lib && cargo test --locked agent_cli::client --lib`

Expected: FAIL because mutation route/classification are absent.

- [ ] **Step 3: Implement explicit mutation policy**

```rust
pub enum MutationReplayPolicy {
    ReconcileByRequestId,
    NaturallyIdempotent,
    NeverReplay,
}
```

Each `AgentControlMutation` variant returns its policy from a total match. `NeverReplay` performs one HTTP hit; transport loss after request dispatch maps to error code `outcome_unknown`, retryable false, `outcomeUnknown=true`. Reconcile variants query existing domain state before any second mutation attempt.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked backend::control_agent --lib && cargo test --locked agent_cli::client --lib`

Expected: PASS; non-replayable fixtures show one hit.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/backend/control_agent.rs src-tauri/src/agent_cli src-tauri/src/workbench src-tauri/src/orchestrator
git commit -m "feat(cli): preserve mutation outcome semantics"
```

### Task 5: Implement Explicit Remote Transport and Capability Gates

**Files:**
- Create: `src-tauri/src/agent_cli/remote.rs`
- Modify: `src-tauri/src/agent_cli/{client.rs,mod.rs,output.rs}`
- Modify: `src-tauri/src/workbench/remote_client.rs`、`src-tauri/src/orchestrator/remote_client.rs`、`src-tauri/src/net/peer_client.rs`。
- Modify: `src-tauri/src/net/{protocol.rs,remote_ids.rs}` if exports are not public to the CLI layer.

**Interfaces:**
- Produces: `AgentCliTransport::{Local,Remote}`, `resolve_remote_device`, structured P2P error mapping.

- [ ] **Step 1: Write offline, v0/v1, mapping, and non-replay tests**

```rust
#[tokio::test]
async fn remote_terminal_send_does_not_receive_control_token_or_retry() {
    let peer = mock_peer().drop_after_apply();
    let error = remote_client(peer.clone()).send_terminal("remote:d:s", b"ls\n").await.unwrap_err();
    assert_eq!(peer.hit_count(), 1);
    assert!(!peer.last_headers().contains_key("x-cc-partner-control-token"));
    assert!(error.outcome_unknown());
}
```

Add missing device, duplicate device ID conflict, offline, old protocol, missing capability→exit6, structured `requestId`, saved remote ID unwrap/wrap, and remote recursion rejection.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_cli::remote --lib`

Expected: FAIL because remote transport is absent.

- [ ] **Step 3: Reuse domain remote clients behind explicit device selection**

Resolve `id:<deviceId>` from the current owner's mDNS table, use the advertised actual HTTP port, health/capability gate each command, and call existing typed clients. Preserve per-route retry class; map error envelope fields directly and never parse localized `message`. Use `remote_ids` helpers for entity IDs.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_cli::remote --lib && cargo test --locked net::protocol --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_cli src-tauri/src/workbench src-tauri/src/orchestrator src-tauri/src/attention src-tauri/src/net
git commit -m "feat(cli): add explicit LAN remote transport"
```

### Task 6: Wire Every Approved Command and Event Follow

**Files:**
- Modify: `src-tauri/src/agent_cli/{args.rs,mod.rs,client.rs,remote.rs}`
- Modify: `src-tauri/src/backend/control_agent.rs`
- Modify: `src-tauri/src/backend/{control_api.rs,event_bus.rs}`
- Test: inline dispatch tests in `src-tauri/src/agent_cli/{mod.rs,args.rs}`.

**Interfaces:**
- Produces: full approved resource/action matrix and resumable `event follow`.

- [ ] **Step 1: Add a table-driven command coverage test**

```rust
#[test]
fn every_approved_command_has_a_dispatch_handler() {
    for argv in APPROVED_COMMAND_FIXTURES {
        let parsed = Cli::try_parse_from(argv).unwrap();
        assert!(dispatch_kind(&parsed).is_supported());
    }
}
```

Assert no `quick-open`, `recipe`, implicit `current`, or body-bearing value appears in help. Event tests cover `afterOwner/afterSequence`, reconnect without duplicate sequence, bounded line size, Ctrl-C exit0, and remote unsupported exit6.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_cli --lib`

Expected: FAIL for command variants not yet wired.

- [ ] **Step 3: Complete dispatch using typed requests**

Wire project/worktree/session/agent/task/experiment/attention/fleet/browser commands. Adapt `/api/backend/control/events/stream` to JSONL with the existing owner/sequence cursor; do not invent a second event bus. `agent wait` observes A1 runtime projections and times out as exit5 without changing runtime state.

- [ ] **Step 4: Run GREEN and help snapshot**

Run: `cd src-tauri && cargo test --locked agent_cli --lib && cargo run --locked --bin cc-partner -- --help`

Expected: PASS; help exactly matches the spec command surface.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_cli src-tauri/src/backend/control_agent.rs
git commit -m "feat(cli): wire agent control resources"
```

### Task 7: Package, Smoke-test, and Document the CLI Contract

**Files:**
- Create: `src-tauri/tests/agent_cli_smoke.rs`
- Modify: `.github/workflows/release-tauri.yml`
- Modify: `README.md`
- Modify: `docs/development/{backend-operations.md,testing.md,quality-matrix.json}`
- Modify: `src-tauri/CLAUDE.md`

**Interfaces:**
- Produces: isolated CLI smoke evidence and standalone release artifact on macOS/Windows/Ubuntu.

- [ ] **Step 1: Write isolated backend/CLI smoke tests**

Start an isolated backend with temporary config/data dirs and dynamic preferred-port fallback; run JSON queries for project/session/task/agent/browser, one request-ID-backed task create, and a non-replayable failure fixture. Assert `cc-partner-backend` still accepts its old commands and Tauri `externalBin` remains backend-only.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked --test agent_cli_smoke -- --nocapture --test-threads=1`

Expected: FAIL until packaging/bootstrap and all command routes are wired.

- [ ] **Step 3: Add release artifacts without changing the sidecar**

Build `cc-partner` for each existing release target and upload it as a separate named artifact; do not copy it into `src-tauri/binaries` or add it to `tauri.conf.json`. Document stdin, envelope, exit code, mutation uncertainty, fixed LAN boundary, and exact selectors.

- [ ] **Step 4: Run focused and repository quality gates**

Run:

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets --locked -- -D warnings
cd src-tauri && cargo test --locked --test agent_cli_smoke -- --nocapture --test-threads=1
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
```

Expected: all PASS; cross-platform rows not run locally remain `NOT VERIFIED` until CI evidence exists.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/agent_cli_smoke.rs .github/workflows/release-tauri.yml README.md docs/development src-tauri/CLAUDE.md
git commit -m "docs(cli): package and verify agent control CLI"
```

## Completion Contract

- `cc-partner-backend` lifecycle behavior and Tauri sidecar packaging are unchanged.
- Every approved CLI command has typed local transport; supported commands have explicit remote capability gates.
- JSON/envelope/exit code/stdout-stderr contracts are fixture-tested.
- No sensitive body is accepted in argv or emitted in error/log fixtures.
- Non-replayable operations have observed hit count1 under connection loss.
- No selector depends on GUI-local active/recent state and no remote device is auto-selected.
- Rust focused tests, CLI smoke, protocol inventory, traceability and docs checks pass.

## Plan Self-review

- Every mutation variant has an explicit replay policy and uncertainty result.
- CLI is a transport/presentation layer over existing domain helpers, not a second business implementation.
- Release integration adds a standalone artifact without altering GUI startup burden.
- The plan contains no Quick Open, Command Recipe, Diff review, authentication mode or capability token.
