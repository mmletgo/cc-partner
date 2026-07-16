# Agent Adapter Platform Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把硬编码Claude Runner迁移为provider-neutral adapter，并接入Codex、受控generic terminal、attempt上限和stall watchdog。

**Architecture:** `AgentAdapterRegistry`只负责probe/launch/resume/runtime normalization/usage/interrupt；Runner继续拥有worktree、terminal、claim与task状态。Resolved runner policy在claim/attempt创建时持久化，后续WORKFLOW变化不能漂移已运行任务。

**Tech Stack:** Rust 2021, Tokio processes, existing Workbench terminal/Orchestrator repos, A1 Agent Runtime, axum P2P, React Settings.

## Global Constraints

- provider wire values固定`claudeCodeVisible|codexVisible|genericTerminal`。
- verifier仍使用现有独立Claude headless judge，不随Runner provider切换。
- `max_turns`表示development attempt总数，范围1–20；不是provider内部tool turn。
- `stall_timeout_ms`范围30,000–1,800,000，使用active runtime last activity。
- provider executable/path/env/credential不进入P2P DTO或日志。
- P2P business API继续无调用者身份鉴权；adapter capability/catalog不得成为授权或设备信任机制。
- generic terminal不接受WORKFLOW任意shell文本，不猜窗口标题或stdout完成。
- 旧peer unsupported时不静默回退Claude；降级前必须quiesce non-Claude Runner。
- 不实现用户Diff审查或全局Quick Open。

---

## File Structure

- Create: `src-tauri/src/orchestrator/agent_adapter/{mod.rs,types.rs,registry.rs,claude_code.rs,codex.rs,generic_terminal.rs}`。
- Create: `src-tauri/src/orchestrator/{runner_limits.rs,runner_watchdog.rs,agent_runtime_bridge.rs}`。
- Modify: `src-tauri/src/orchestrator/{mod.rs,workflow.rs,runner.rs,completion.rs,scheduler.rs,models.rs}`。
- Modify: `src-tauri/src/orchestrator/repo/{schema.rs,tasks.rs,attempts.rs,tests.rs}`。
- Create: `src-tauri/src/commands/orchestrator_adapters.rs`。
- Modify: `src-tauri/src/orchestrator/{remote_protocol.rs,remote_client.rs}`、`src-tauri/src/net/routes/orchestrator.rs`、`src-tauri/src/net/{http_server.rs,protocol.rs,discovery.rs}`、`web/src/lib/types/orchestrator.ts`、`web/src/lib/schemas/{orchestrator.ts,orchestrator.test.ts}`、`web/src/api/{orchestrator.ts,orchestrator.test.ts}`、`web/src/pages/Settings/{AutomationSettingsPanel.tsx,useSettingsController.ts,automationSettingsState.ts,Settings.test.tsx}`、`web/src/i18n/locales/{zh/orchestrator.json,en/orchestrator.json,zh/settings.json,en/settings.json}`。

## Task Dependency Graph

```text
T1 → T2 → T3 → T4 → (T5 | T6) → T7 → T8
```

### Task 1: Add Strong Provider and Runner Policy Types

**Files:**
- Create: `src-tauri/src/orchestrator/agent_adapter/{mod.rs,types.rs}`
- Modify: `src-tauri/src/orchestrator/mod.rs`
- Modify: `src-tauri/src/orchestrator/workflow.rs`

**Interfaces:**
- Produces: `AgentProviderId`, `AgentCompletionContract`, `RunnerAttemptPolicy`, `resolve_task_runner_policy`.

- [ ] **Step 1: Write parser and roundtrip tests**

```rust
#[test]
fn workflow_accepts_all_built_in_agent_providers() {
    for value in ["claudeCodeVisible", "codexVisible", "genericTerminal"] {
        let policy = resolve_workflow(&format!("---\nrunner:\n  provider: {value}\n---\nPrompt")).unwrap().runner;
        assert_eq!(policy.provider.as_str(), value);
    }
}
```

Add unknown provider, max 0/21 and stall 29,999/1,800,001 failures.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::workflow --lib`

Expected: Codex/generic cases fail under current Claude-only parser.

- [ ] **Step 3: Implement strong policy**

```rust
pub enum AgentProviderId { ClaudeCodeVisible, CodexVisible, GenericTerminal }
pub enum AgentCompletionContract { SentinelLine, HookEvent, Manual }
pub struct RunnerAttemptPolicy {
    pub provider: AgentProviderId,
    pub max_turns: i64,
    pub stall_timeout_ms: i64,
    pub completion_contract: AgentCompletionContract,
}
```

`resolve_task_runner_policy` applies task/candidate override first, then resolved workflow. Unknown value fails closed.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::workflow --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/agent_adapter src-tauri/src/orchestrator/mod.rs src-tauri/src/orchestrator/workflow.rs
git commit -m "feat(orchestrator): define runner adapter policy"
```

### Task 2: Persist Immutable Attempt Policy Snapshots

**Files:**
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/orchestrator/repo/{schema.rs,helpers.rs,tasks.rs,attempts.rs,tests.rs}`
- Modify: `src-tauri/migrations/0001_init.sql`

**Interfaces:**
- Produces: policy columns and typed `OrchestratorAttemptStatus`.

- [ ] **Step 1: Write additive migration/snapshot tests**

```rust
#[tokio::test]
async fn attempt_policy_does_not_change_when_workflow_changes() {
    let repo = upgraded_fixture().await;
    let policy = policy(AgentProviderId::CodexVisible, 4, 300_000);
    repo.add_attempt("task", 1, &policy, None, OrchestratorAttemptStatus::Running).await.unwrap();
    assert_eq!(repo.attempt("task", 1).await.unwrap().runner_provider, "codexVisible");
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::repo --lib`

Expected: FAIL because columns/typed status are absent.

- [ ] **Step 3: Add columns and remove hardcoded provider writes**

Add task `agent_session_id/runner_max_turns/runner_stall_timeout_ms`; attempt `runner_provider/agent_session_id/max_turns/stall_timeout_ms/completion_contract`. Old null rows map to Claude/1/300000. `mark_task_running_attempt` receives `&RunnerAttemptPolicy`.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::repo --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/models.rs src-tauri/src/orchestrator/repo src-tauri/migrations/0001_init.sql
git commit -m "feat(orchestrator): snapshot runner policy"
```

### Task 3: Extract Adapter Registry with Claude Behavior Parity

**Files:**
- Create: `src-tauri/src/orchestrator/agent_adapter/{registry.rs,claude_code.rs}`
- Modify: `src-tauri/src/orchestrator/runner.rs`
- Test: inline tests in `src-tauri/src/orchestrator/agent_adapter/{claude_code.rs,registry.rs}` and `src-tauri/src/orchestrator/runner.rs`.

**Interfaces:**
- Produces: `AgentAdapter`, `AgentAdapterRegistry`, `AgentLaunchRequest`, `AgentLaunchPlan`.

- [ ] **Step 1: Capture current Claude launch behavior**

```rust
#[test]
fn claude_adapter_keeps_visible_terminal_input() {
    let plan = ClaudeCodeAdapter.build_launch_plan(&request("fix tests")).unwrap();
    assert_eq!(plan.executable, "claude");
    assert_eq!(plan.stdin.as_deref(), Some("fix tests\n"));
    assert_eq!(plan.completion, AgentCompletionContract::SentinelLine);
}
```

Also assert claim-token cancellation prevents any terminal input.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_adapter --lib && cargo test --locked orchestrator::runner --lib`

Expected: FAIL because registry/adapter are absent.

- [ ] **Step 3: Implement trait and registry**

```rust
pub trait AgentAdapter: Send + Sync {
    fn provider_id(&self) -> AgentProviderId;
    fn probe(&self) -> Result<AgentProbeResult, AppError>;
    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError>;
    fn build_resume_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError>;
    fn normalize_runtime_event(&self, event: NativeAgentEvent) -> Result<AgentRuntimeMutation, AppError>;
    fn extract_usage(&self, event: &NativeAgentEvent) -> Option<AgentUsageDelta>;
    fn interrupt_input(&self) -> &'static str;
}
```

Move direct command/scanner calls behind Claude adapter; Runner only resolves policy/adapter and executes plan.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_adapter --lib && cargo test --locked orchestrator::runner --lib`

Expected: PASS with existing Claude behavior unchanged.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/agent_adapter src-tauri/src/orchestrator/runner.rs
git commit -m "refactor(orchestrator): route Claude through adapter"
```

### Task 4: Add Codex and Controlled Generic Terminal Adapters

**Files:**
- Create: `src-tauri/src/orchestrator/agent_adapter/{codex.rs,generic_terminal.rs}`
- Modify: `src-tauri/src/orchestrator/agent_adapter/registry.rs`
- Modify: `src-tauri/src/config.rs`

**Interfaces:**
- Produces: owner-local probe and generic allowlist config; no remote executable DTO.

- [ ] **Step 1: Write availability and redaction tests**

```rust
#[test]
fn generic_terminal_is_unavailable_without_owner_allowlist() {
    let adapter = GenericTerminalAdapter::new(None);
    assert_eq!(adapter.probe().unwrap().availability, AgentAvailability::Unavailable);
}
```

Assert remote-facing serialization has no executable/args/env fields.

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_adapter --lib && cargo test --locked config:: --lib`

Expected: FAIL because adapters/config are absent.

- [ ] **Step 3: Implement bounded probes and launch plans**

Probe `codex --version` with a 2-second timeout and 4 KiB output cap. Generic config stores owner-local executable plus literal args and `Manual|SentinelLine`; reject shell metacharacter execution and do not accept this config through LAN routes.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked orchestrator::agent_adapter --lib && cargo test --locked config:: --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/agent_adapter/codex.rs src-tauri/src/orchestrator/agent_adapter/generic_terminal.rs src-tauri/src/orchestrator/agent_adapter/registry.rs src-tauri/src/config.rs
git commit -m "feat(orchestrator): add Codex and generic adapters"
```

### Task 5: Enforce max_turns Before Creating the Next Attempt

**Files:**
- Create: `src-tauri/src/orchestrator/runner_limits.rs`
- Modify: `src-tauri/src/orchestrator/{runner.rs,completion.rs}`
- Modify: `src-tauri/src/commands/orchestrator/common.rs`
- Modify: `src-tauri/src/orchestrator/repo/attempts.rs`

**Interfaces:**
- Produces: `check_next_attempt(policy,next_attempt)` and `runner_max_turns_exceeded` evidence.

- [ ] **Step 1: Write no-attempt-2 test**

```rust
#[tokio::test]
async fn max_one_blocks_before_second_session_is_created() {
    let fixture = runner_fixture(policy(AgentProviderId::ClaudeCodeVisible, 1, 300_000)).await;
    fixture.request_repair().await.unwrap();
    assert_eq!(fixture.created_session_count(), 1);
    assert_eq!(fixture.task_state().await, OrchestratorTaskState::Blocked);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked runner_limits --lib`

Expected: FAIL because limit is not consumed.

- [ ] **Step 3: Implement pre-session CAS**

Call `check_next_attempt` in initial/repair preparation before worktree/session/attempt creation. On overflow CAS task/attempt to Blocked/TurnLimitReached and emit one event/evidence; concurrent repair calls share the CAS guard.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked runner_limits --lib && cargo test --locked commands::orchestrator --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/runner_limits.rs src-tauri/src/orchestrator/runner.rs src-tauri/src/orchestrator/completion.rs src-tauri/src/commands/orchestrator/common.rs src-tauri/src/orchestrator/repo/attempts.rs
git commit -m "feat(orchestrator): enforce runner attempt limits"
```

### Task 6: Add stall_timeout Watchdog

**Files:**
- Create: `src-tauri/src/orchestrator/runner_watchdog.rs`
- Modify: `src-tauri/src/orchestrator/scheduler.rs`
- Modify: `src-tauri/src/orchestrator/repo/{tasks.rs,attempts.rs,tests.rs}`

**Interfaces:**
- Produces: `list_stalled_active_runners`, `reconcile_stalled_runner`.

- [ ] **Step 1: Write virtual-time boundary tests**

```rust
#[tokio::test(start_paused = true)]
async fn runner_stalls_at_configured_deadline_only() {
    let fixture = watchdog_fixture(300_000).await;
    tokio::time::advance(Duration::from_millis(299_999)).await;
    assert!(fixture.scan().await.is_empty());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(fixture.scan().await.len(), 1);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked runner_watchdog --lib`

Expected: FAIL because watchdog is absent.

- [ ] **Step 3: Implement liveness reconcile and guarded interrupt**

Scheduler invokes bounded scan every 10 seconds before claim. CAS on task+attempt+session+Running; only CAS winner writes `runner_stalled` evidence and sends adapter Ctrl-C best effort. CAS miss never interrupts a replacement session.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked runner_watchdog --lib && cargo test --locked orchestrator::scheduler --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/runner_watchdog.rs src-tauri/src/orchestrator/scheduler.rs src-tauri/src/orchestrator/repo/tasks.rs src-tauri/src/orchestrator/repo/attempts.rs src-tauri/src/orchestrator/repo/tests.rs
git commit -m "feat(orchestrator): stop stalled agent runners"
```

### Task 7: Bridge Adapter Events, Resume and Usage to A1 Runtime

**Files:**
- Create: `src-tauri/src/orchestrator/agent_runtime_bridge.rs`
- Modify: `src-tauri/src/orchestrator/{runner.rs,completion.rs}`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/orchestrator/repo/attempts.rs`

**Interfaces:**
- Consumes: A1 `AgentRuntimeMutation`/store.
- Produces: `record_runner_activity`, `handle_normalized_agent_event`, `resume_runner_attempt`.

- [ ] **Step 1: Write active-session guards**

```rust
#[tokio::test]
async fn resume_uses_original_provider_and_old_session_event_is_ignored() {
    let fixture = bridge_fixture(AgentProviderId::CodexVisible).await;
    let resumed = fixture.resume().await.unwrap();
    assert_eq!(resumed.provider, AgentProviderId::CodexVisible);
    fixture.emit_from_old_session().await;
    assert_eq!(fixture.active_agent_id().await, resumed.agent_session_id);
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked agent_runtime_bridge --lib`

Expected: FAIL because bridge is absent.

- [ ] **Step 3: Implement normalized events and compatibility dual-write**

Throttle terminal activity updates, normalize Hook/OSC through the selected adapter, persist native ID/usage, and resume only if the same adapter reports support. Continue Claude legacy session/transcript dual-write one version; never copy transcript path to A1 DTO.

- [ ] **Step 4: Run GREEN**

Run: `cd src-tauri && cargo test --locked agent_runtime_bridge --lib && cargo test --locked workbench::sessions --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/orchestrator/agent_runtime_bridge.rs src-tauri/src/orchestrator/runner.rs src-tauri/src/orchestrator/completion.rs src-tauri/src/workbench/sessions.rs src-tauri/src/orchestrator/repo/attempts.rs
git commit -m "feat(orchestrator): connect adapters to agent runtime"
```

### Task 8: Expose Owner Adapter Catalog and Downgrade Guard

**Files:**
- Create: `src-tauri/src/commands/orchestrator_adapters.rs`
- Modify: `src-tauri/src/orchestrator/{remote_protocol.rs,remote_client.rs}`
- Modify: `src-tauri/src/net/routes/orchestrator.rs`, `http_server.rs`, `protocol.rs`, `discovery.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `web/src/lib/types/orchestrator.ts`
- Modify: `web/src/lib/schemas/{orchestrator.ts,orchestrator.test.ts}`
- Modify: `web/src/api/{orchestrator.ts,orchestrator.test.ts}`
- Modify: `web/src/pages/Settings/{AutomationSettingsPanel.tsx,useSettingsController.ts,automationSettingsState.ts,Settings.test.tsx}`
- Modify: `web/src/i18n/locales/{zh/orchestrator.json,en/orchestrator.json,zh/settings.json,en/settings.json}`
- Modify: `docs/p2p-protocol.md`, `docs/prd.md`, `docs/development/quality-matrix.json`, `src-tauri/CLAUDE.md`

**Interfaces:**
- Produces: `orchestrator.agent-adapters.v1`, `list_orchestrator_agent_adapters(project_id)` and local-only `prepare-agent-downgrade`.

- [ ] **Step 1: Write catalog redaction and unsupported tests**

```rust
#[tokio::test]
async fn remote_adapter_catalog_never_contains_executable_or_environment() {
    let value = serde_json::to_value(adapter_catalog_fixture()).unwrap();
    let text = value.to_string();
    assert!(!text.contains("executable"));
    assert!(!text.contains("env"));
}
```

- [ ] **Step 2: Run RED**

Run: `cd src-tauri && cargo test --locked orchestrator_adapters --lib && cargo test --locked net::routes::orchestrator --lib`

Expected: FAIL because route/catalog are absent.

- [ ] **Step 3: Implement owner catalog and quiesce helper**

Route returns only `{provider,available,completionContract,supportsResume,supportsUsage,reasonCode}`. `prepare-agent-downgrade` is loopback/local-only: refuse Delivering, cancel/abort active non-Claude tasks, preserve worktree/session/evidence, and never expose as LAN route.

- [ ] **Step 4: Run complete gates**

Run: `cd src-tauri && cargo fmt --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked orchestrator::agent_adapter && cargo test --locked runner_limits && cargo test --locked runner_watchdog && cargo test --locked net::routes::orchestrator && cargo check --locked --bins && cd .. && node scripts/check-p2p-route-inventory.mjs && node scripts/check-docs.mjs && cd web && npm run check:i18n && npm test -- orchestrator && npm run build`

Expected: all exit 0.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/orchestrator_adapters.rs src-tauri/src/orchestrator/remote_protocol.rs src-tauri/src/orchestrator/remote_client.rs src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs src-tauri/src/net/discovery.rs src-tauri/src/lib.rs web/src/lib/types/orchestrator.ts web/src/lib/schemas/orchestrator.ts web/src/api/orchestrator.ts web/src/pages/Settings docs/p2p-protocol.md docs/prd.md docs/development/quality-matrix.json src-tauri/CLAUDE.md
git commit -m "feat(orchestrator): expose agent adapter catalog"
```

## Completion Contract

- Existing Claude task behavior has characterization parity.
- Codex/generic availability is owner-local and fail-closed.
- attempt/stall policies are persisted and actually enforced.
- old peers never silently convert non-Claude work to Claude.

## Plan Self-Review

- Spec coverage: policy, persistence, three adapters, limits, watchdog, runtime bridge, remote and downgrade each map to tasks.
- Placeholder scan: no unresolved implementation placeholders.
- Type consistency: provider/completion/policy names match the design spec and are reused through repository and wire tasks.
