# Agent Hub Gate D — Plugin Decomposition and OpenCode Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Plugin 拆为可独立同步和引用计数的 Skill/MCP/Command/Agent/Hook/residual，并把 OpenCode 作为具备真实 session/completion 事件的可见 Workbench/Orchestrator provider。

**Architecture:** Canonical `PluginPackage` revision 只保存 package metadata、固定 component revision refs 与 residual CAS refs；跨 target 只转换 Gate B portable component 和具备真实合同 evidence 的 Hook，运行时代码默认 source-only。OpenCode runtime 使用 opted-in project 内的 cc-partner 派生 Plugin 监听官方 session/permission events，并向既有 `cc-partner-agent-v1` OSC decoder 输出有界事件；现有 Agent Runtime reducer 仍是唯一状态写入口。

**Tech Stack:** Rust 2021、SQLite/sqlx、Gate A–C Revision DAG/CAS/Snapshot、Claude/Codex Plugin CLI、OpenCode TypeScript Plugin、portable-pty/tmux、OSC base64url、React 19、TypeScript、Vitest、Playwright。

## Global Constraints

- Gate C 全部通过后才开始本计划；Plugin/Hook/residual 必须自然进入同一个 Revision DAG、CAS、Snapshot/LAN/Git 路径。
- Plugin component 使用固定 revision ref；component 后续更新不能改写旧 package revision。
- 删除 package 只 tombstone 该 package 独占且没有 standalone/其他 package ref 的 component；引用数从边表查询，不维护易漂移计数器。
- OpenCode JS/TS/npm/custom-tool runtime 默认只投影回 OpenCode；Claude/Codex runtime 默认只回来源 target。
- Hook 默认 targetOnly；只有 support manifest 中具备双端 schema、信任模型和真实 CLI evidence 的 mapping 才跨 target。
- package aggregate 状态必须准确区分 `full`、`partial`、`sourceOnly`、`activationRequired`、`externalCollision`、`blocked`。
- cc-partner OpenCode runtime bridge 是 app-version 派生物，不是用户 canonical Plugin，不进入 Snapshot；project opt-in preview 必须列出它的文件写入。
- OpenCode runtime bridge 只能在对应 Workbench project/checkout 已 opt-in、文件 hash 已验证且 CLI runtime capability 有 exact evidence 时启用。
- OpenCode completion 不解析可见 stdout 文本；仅接受官方 Plugin event 经 OSC 进入现有 reducer。
- `openCodeVisible` 不得在缺少 runtime bridge 时退化成 Sentinel/猜测完成；provider 应在创建 worktree/session 前 fail-closed。
- Orchestrator 不新增第八个 Workbench page controller；provider 状态复用 automation controller/现有 runtime hooks。
- 凭据/Plugin payload 在 Hub/LAN/Git 保持原字节；日志与 UI 摘要不打印正文、env/header 或 runtime payload。
- N/N+1 保留旧入口；只有达到 N+2 且有稳定迁移 evidence 时才允许实际删除旧表/路由。
- 新增/修改 Rust 与 TypeScript 代码遵守根 `AGENTS.md` 的中文 docstring、strict 类型、token 与 Hooks-before-early-return 合同。

---

## File Structure

- Create: `src-tauri/src/agent_hub/plugins/{mod.rs,models.rs,decompose.rs,ownership.rs,render.rs,hook_mapping.rs}`。
- Extend: `src-tauri/src/agent_hub/packages/`, `src-tauri/src/agent_hub/targets/`, `src-tauri/src/storage/agent_hub_repo.rs`。
- Create: `src-tauri/src/orchestrator/agent_adapter/opencode.rs`。
- Modify: `src-tauri/src/orchestrator/agent_adapter/{mod.rs,types.rs,registry.rs}`。
- Create: `src-tauri/src/workbench/agent_runtime/opencode_bridge.rs`。
- Modify: `src-tauri/src/workbench/{sessions.rs,agent_runtime/mod.rs}`。
- Modify: `src-tauri/src/orchestrator/{runner.rs,workflow.rs,agent_runtime_bridge.rs,completion.rs}`。
- Extend: `src-tauri/src/agent_hub/support/support-manifest.json`, `src-tauri/tests/agent_hub_cli_contract.rs`。
- Extend: `web/src/pages/AgentHub/`, `web/src/pages/Settings/AutomationSettingsPanel.tsx`, Workbench/Orchestrator provider types and views。
- Create: `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs`。
- Test: `web/tests/agent-hub.spec.ts`。

### Task 1: Add Immutable PluginPackage, Hook and Residual Schemas

**Files:**
- Create: `src-tauri/src/agent_hub/plugins/mod.rs`
- Create: `src-tauri/src/agent_hub/plugins/models.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Modify: `src-tauri/src/agent_hub/models.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct PluginPackagePayload {
    pub plugin_id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub source_target: AgentTarget,
    pub component_refs: Vec<PluginComponentRef>,
    pub residual_refs: Vec<PluginResidualRef>,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

pub struct PluginComponentRef {
    pub kind: AssetKind,
    pub asset_id: String,
    pub revision_id: RevisionId,
    pub ownership: ComponentOwnership,
}

pub struct PortableHook {
    pub event_intent: HookEventIntent,
    pub input_contract: serde_json::Value,
    pub output_contract: serde_json::Value,
    pub command_tree_hash: Option<String>,
    pub source_target: AgentTarget,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

pub struct PluginResidualRef {
    pub target: AgentTarget,
    pub residual_kind: ResidualKind,
    pub tree_manifest_hash: String,
}
```

- Adds tables:

```text
agent_hub_plugin_components
agent_hub_plugin_residuals
agent_hub_component_standalone_refs
```

- [ ] **Step 1: Write immutable-reference tests**

Create package revision P1 referencing Skill S1, then append S2. Assert P1 still references S1. Create P2 referencing S2 and assert snapshot closure includes both historical refs when both package revisions are retained.

- [ ] **Step 2: Write schema validation tests**

Reject duplicate component refs, kind/revision mismatch, missing component revision, residual target without tree, empty plugin ID and a Hook whose declared contract exceeds Snapshot limits. Assert source runtime files round-trip exact bytes through CAS.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::plugins::models
cargo test --locked storage::agent_hub_repo
```

Expected: plugin schemas/tables are absent.

- [ ] **Step 4: Implement typed payload and additive schema**

Sort component refs by `{kind,assetId,revisionId}` and residuals by `{target,residualKind,treeHash}` before canonical serialization. Insert package revision and all refs in one SQL transaction; foreign-key validation is performed explicitly for compatibility with existing SQLite settings.

- [ ] **Step 5: Extend Snapshot closure**

Gate C builder must traverse package component revision refs and residual tree refs even when those assets are not active heads. Import validates every ref before making the package head active.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::plugins::models
cargo test --locked storage::agent_hub_repo
cargo test --locked agent_hub::snapshot
```

Expected: immutable refs and full snapshot restore pass.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/plugins/mod.rs src-tauri/src/agent_hub/plugins/models.rs src-tauri/src/agent_hub/mod.rs src-tauri/src/agent_hub/models.rs src-tauri/src/storage/agent_hub_repo.rs src-tauri/src/agent_hub/snapshot
git commit -m "feat: add canonical plugin package schemas"
```

### Task 2: Decompose Target Plugins and Track Component Ownership

**Files:**
- Create: `src-tauri/src/agent_hub/plugins/decompose.rs`
- Create: `src-tauri/src/agent_hub/plugins/ownership.rs`
- Modify: `src-tauri/src/agent_hub/plugins/mod.rs`
- Modify: `src-tauri/src/agent_hub/targets/claude.rs`
- Modify: `src-tauri/src/agent_hub/targets/codex.rs`
- Modify: `src-tauri/src/agent_hub/targets/opencode.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub trait PluginDecomposer {
    fn inspect(
        &self,
        source: &DiscoveredPluginSource,
        objects: &ObjectStore,
    ) -> Result<PluginDecompositionPreview, AppError>;

    async fn import(
        &self,
        preview: ConfirmedPluginDecomposition,
    ) -> Result<PluginPackageRevision, AppError>;
}

pub enum ComponentDeleteDecision {
    TombstoneOwned,
    PreserveShared,
    PreserveStandalone,
}
```

- Child assets use `originNamespace=plugin:<pluginId>` unless linked to an already confirmed standalone logical asset.

- [ ] **Step 1: Write target decomposition fixtures**

Cover:

- Claude Plugin containing Skill, Command, Agent, MCP, Hook and unknown runtime files;
- Codex Plugin containing Skill, config/agent component and residual assets;
- OpenCode local Plugin containing JS/TS, npm package declaration, custom tool plus adjacent portable Skill/Command/Agent.

Assert portable components become typed child previews, Hook remains targetOnly without a mapping, and runtime files become source-target residuals.

- [ ] **Step 2: Write ownership/delete tests**

Build:

```text
package A -> skill S (owned)
package B -> skill S (shared)
standalone -> skill S
```

Deleting A preserves S because B/standalone refs exist. After deleting B, standalone still preserves S. Only after explicit standalone deletion may S receive a tombstone. Package deletion never mutates old package revisions.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::plugins::decompose
cargo test --locked agent_hub::plugins::ownership
```

Expected: decomposer/ownership modules are absent.

- [ ] **Step 4: Implement target manifest inspection**

Reuse Gate B adapter scanners for recognized components. Preserve unknown manifest fields as source target extensions and unknown files as residual CAS trees. Preview shows exact child logical keys, revision hashes, ownership and portability status before import.

- [ ] **Step 5: Implement reference-derived deletion**

Query live package-head refs and standalone refs inside the deletion transaction. Append package tombstone first; append child tombstones only for `TombstoneOwned`. Shared refs remain active. Concurrent ref creation retries from a fresh read rather than using a stale count.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::plugins::decompose
cargo test --locked agent_hub::plugins::ownership
```

Expected: mixed packages preserve every source byte and deletion never removes a referenced component.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/plugins/decompose.rs src-tauri/src/agent_hub/plugins/ownership.rs src-tauri/src/agent_hub/plugins/mod.rs src-tauri/src/agent_hub/targets src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: decompose plugins with component ownership"
```

### Task 3: Render Package Components, Residuals and Evidence-Backed Hook Mappings

**Files:**
- Create: `src-tauri/src/agent_hub/plugins/hook_mapping.rs`
- Create: `src-tauri/src/agent_hub/plugins/render.rs`
- Modify: `src-tauri/src/agent_hub/plugins/mod.rs`
- Modify: `src-tauri/src/agent_hub/packages/builder.rs`
- Modify: `src-tauri/src/agent_hub/packages/activator.rs`
- Modify: `src-tauri/src/agent_hub/targets/claude.rs`
- Modify: `src-tauri/src/agent_hub/targets/codex.rs`
- Modify: `src-tauri/src/agent_hub/targets/opencode.rs`
- Modify: `src-tauri/src/agent_hub/support/manifest.rs`
- Modify: `src-tauri/src/agent_hub/support/support-manifest.json`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct HookMappingRecord {
    pub intent: HookEventIntent,
    pub source_target: AgentTarget,
    pub destination_target: AgentTarget,
    pub schema_version: u32,
    pub trust_model: HookTrustModel,
    pub evidence_id: String,
}

pub struct PackageProjectionReport {
    pub components: Vec<ComponentProjectionReport>,
    pub residuals: Vec<ResidualProjectionReport>,
    pub aggregate_status: PackageAggregateStatus,
}
```

- [ ] **Step 1: Write fail-closed Hook mapping tests**

Assert a Hook remains source-only when:

- mapping record is absent;
- source/destination schema version mismatches;
- input/output contract loses a required field;
- trust model differs;
- evidence ID is absent from quality matrix.

Only a checked-in mapping fixture with an exact support-manifest record may render on another target.

- [ ] **Step 2: Write mixed package status tests**

For a package with portable Skill, partial Command, targetOnly Hook and OpenCode JS residual, assert:

```text
OpenCode -> full only when every requested native component/residual is verified
Claude   -> partial/sourceOnly breakdown, never full
Codex    -> partial/activationRequired as dictated by component activator
```

Aggregate status is derived from requested target bindings, not source package success.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::plugins::hook_mapping
cargo test --locked agent_hub::plugins::render
```

Expected: mapping/render modules are absent.

- [ ] **Step 4: Implement explicit mapping registry**

Load mapping records from the compiled support manifest. The initial registry may intentionally have zero cross-target mappings; this is a valid source-only result, not a placeholder. Add a mapping only in the same commit as both target render/round-trip fixtures and a real CLI evidence row.

- [ ] **Step 5: Implement package rendering**

Resolve each fixed component revision, call the Gate B target renderer, include same-target residuals unchanged and omit other runtime residuals with a diagnostic. Build/activate with Gate B’s deterministic package and durable activation state machine.

- [ ] **Step 6: Implement accurate aggregate status**

Expose per-component canonical revision, materialized alias, target status, residual reason and activation state. `full` requires every requested component/residual to be semantically represented and verified; a source-only runtime always prevents cross-target full.

- [ ] **Step 7: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::plugins::hook_mapping
cargo test --locked agent_hub::plugins::render
cargo test --locked agent_hub::packages
node ../scripts/check-agent-hub-support-manifest.mjs --gate-d
```

Expected: unsupported mappings stay source-only and status never overstates portability.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent_hub/plugins/hook_mapping.rs src-tauri/src/agent_hub/plugins/render.rs src-tauri/src/agent_hub/plugins/mod.rs src-tauri/src/agent_hub/packages src-tauri/src/agent_hub/targets src-tauri/src/agent_hub/support docs/development/quality-matrix.json
git commit -m "feat: project plugin components and residuals"
```

### Task 4: Generate and Verify the OpenCode OSC Runtime Bridge

**Files:**
- Create: `src-tauri/src/workbench/agent_runtime/opencode_bridge.rs`
- Modify: `src-tauri/src/workbench/agent_runtime/mod.rs`
- Modify: `src-tauri/src/workbench/agent_runtime/osc.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/agent_hub/project_scope.rs`
- Modify: `src-tauri/src/agent_hub/projection/scheduler.rs`
- Test: same files
- Test: `src-tauri/tests/quality_faults.rs`

**Interfaces:**
- Produces project-derived file `.opencode/plugins/cc-partner-runtime.ts`.
- Adds non-secret terminal environment `CC_PARTNER_AGENT_SESSION_ID` alongside existing project/worktree/terminal/owner IDs.
- Produces `OpenCodeRuntimeBridge::{preview,materialize,verify}`.

- [ ] **Step 1: Write generated-source snapshot tests**

The generated TypeScript Plugin must:

- read non-empty `CC_PARTNER_AGENT_SESSION_ID` and `CC_PARTNER_TERMINAL_SESSION_ID`;
- subscribe through the official `event` hook;
- handle `session.status`, `session.idle`, `session.error`, `permission.asked`;
- bind to the first native `sessionID` and ignore other sessions;
- emit base64url JSON in `\x1b]777;cc-partner-agent-v1;<payload>\x1b\\`;
- never include prompt, message, permission content or environment values other than the two IDs/provider.

Pin the exact generated source hash in a snapshot test so app upgrades produce an explicit project preview diff.

- [ ] **Step 2: Write event mapping tests**

Feed typed official event fixtures and assert:

```text
session.status busy/retry -> working
permission.asked          -> needsInput
session.status idle       -> idle
session.idle              -> completed
session.error             -> failed
```

Every frame includes native session ID, RFC3339 timestamp and a strictly increasing event version beginning at 2. Existing decoder/reducer tests must accept the frames and discard stale/wrong-terminal events.

`session.idle` may appear before the initial prompt has actually started on some versions. The bridge tracks `seenActive` per bound native session and emits Completed only after a preceding busy/retry/permission event; a pre-active idle remains Idle and cannot finish an Orchestrator task.

- [ ] **Step 3: Write project/worktree and collision tests**

An unopted project produces preview only and `runtimeBridgeRequired`. An opted-in main checkout plus future Workbench worktree receives the same generated bridge before launch. Existing different bytes at the reserved path produce `externalCollision`; no overwrite and provider blocked.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked workbench::agent_runtime::opencode_bridge
cargo test --locked workbench::agent_runtime::osc
cargo test --locked --test quality_faults opencode_runtime_bridge -- --nocapture --test-threads=1
```

Expected: bridge module/tests are absent.

- [ ] **Step 5: Preallocate and inject runtime IDs**

Add an Orchestrator-specific session creation path that preallocates terminal and agent session UUIDs before spawning the shell/tmux window. Extend `TerminalAgentContextIds` and tmux `-e` args with `CC_PARTNER_AGENT_SESSION_ID`; create the Agent Runtime row with that exact ID. Failure rolls back/inactivates the preallocated runtime and terminal.

Ordinary user-created terminals keep the field absent.

- [ ] **Step 6: Implement deterministic bridge generation**

Generate dependency-free TypeScript using the OpenCode Plugin `event` hook and `Buffer`/`process.stdout.write`. It is a derived system materialization excluded from canonical assets/Snapshot. Use Gate A preconditions/atomic writer and project opt-in; recovery verifies the exact source hash.

OpenCode scanning reserves only the exact path `.opencode/plugins/cc-partner-runtime.ts`: matching generated bytes are ignored as derived state; different bytes become `externalCollision` and are never silently imported as a user Plugin or overwritten.

- [ ] **Step 7: Verify actual PTY visibility**

The real CLI contract must prove `process.stdout.write` from the local Plugin reaches OpenCode’s PTY and is stripped by `AgentOscDecoder`. If the current exact OpenCode version captures or suppresses it, runtime support remains blocked; do not fall back to parsing human stdout.

- [ ] **Step 8: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked workbench::agent_runtime::opencode_bridge
cargo test --locked workbench::agent_runtime::osc
cargo test --locked workbench::sessions
cargo test --locked --test quality_faults opencode_runtime_bridge -- --nocapture --test-threads=1
```

Expected: bridge frames drive the existing reducer and project repositories remain uncommitted/unpushed.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/workbench/agent_runtime/opencode_bridge.rs src-tauri/src/workbench/agent_runtime/mod.rs src-tauri/src/workbench/agent_runtime/osc.rs src-tauri/src/workbench/sessions.rs src-tauri/src/agent_hub/project_scope.rs src-tauri/src/agent_hub/projection/scheduler.rs src-tauri/tests/quality_faults.rs
git commit -m "feat: bridge opencode runtime events to workbench"
```

### Task 5: Add `openCodeVisible` to the Agent Adapter Registry and Runner

**Files:**
- Create: `src-tauri/src/orchestrator/agent_adapter/opencode.rs`
- Modify: `src-tauri/src/orchestrator/agent_adapter/mod.rs`
- Modify: `src-tauri/src/orchestrator/agent_adapter/types.rs`
- Modify: `src-tauri/src/orchestrator/agent_adapter/registry.rs`
- Modify: `src-tauri/src/orchestrator/runner.rs`
- Modify: `src-tauri/src/orchestrator/workflow.rs`
- Modify: `src-tauri/src/orchestrator/agent_runtime_bridge.rs`
- Modify: `src-tauri/src/orchestrator/completion.rs`
- Modify: `src-tauri/src/orchestrator/experiments/{create.rs,judge.rs,remote_protocol.rs}`
- Modify: `src-tauri/src/orchestrator/models.rs`
- Modify: `src-tauri/src/workbench/sessions.rs`
- Modify: `src-tauri/src/agent_hub/support/support-manifest.json`
- Modify: `src-tauri/tests/agent_hub_cli_contract.rs`
- Test: same files

**Interfaces:**
- Adds `AgentProviderId::OpenCodeVisible` with wire value `openCodeVisible`.
- Launch plan:

```text
opencode --prompt <prompt>
```

- Resume plan:

```text
opencode --session <nativeSessionId> --prompt <prompt>
```

- Completion contract: `hookEvent`, only after project bridge verification.

- [ ] **Step 1: Expand provider parse/registry tests**

Update all fixed provider lists, error messages, workflow fixtures, experiment provider validation and catalog order to include four providers. Legacy null still maps to Claude. Unknown values still fail closed.

- [ ] **Step 2: Write safe terminal command-rendering tests**

`--prompt` carries arbitrary user text, so replace raw space joining with a shell-dialect renderer using the session’s actual shell:

```rust
pub enum TerminalShellDialect { Posix, PowerShell, Cmd }

pub fn render_terminal_command(
    plan: &AgentLaunchPlan,
    dialect: TerminalShellDialect,
) -> Result<String, AppError>;
```

Test spaces, quotes, newlines, `$()`, backticks, `%VAR%`, semicolons and Unicode. The rendered input must pass the prompt as one literal argv value and never execute prompt contents. Apply `AgentLaunchPlan.env` with dialect-safe literal values so initial launch and resume receive the current agent/terminal IDs. Preserve existing simple Claude/Codex terminal-input characterization.

- [ ] **Step 3: Write OpenCode adapter tests**

Assert:

- exact support-manifest/runtime-bridge probe is required;
- empty prompt is rejected;
- launch/resume argv match the documented TUI flags;
- resume without native ID fails;
- completion is HookEvent;
- interrupt is Ctrl-C;
- no usage fields are estimated when official events do not provide them.

Add `ResumeTerminalPolicy::{Reuse,Fresh}` to the adapter contract and assert OpenCode returns `Fresh`: an idle TUI still owns its PTY, so injecting a second shell command into that terminal is not a valid resume.

- [ ] **Step 4: Write Runner preflight tests**

Before worktree/session creation, an unopted project, bridge collision, unsupported CLI version or missing L3 runtime evidence blocks with a stable reason and creates no worktree/terminal. After preflight, new Workbench worktree waits for bridge projection verification before OpenCode input.

- [ ] **Step 5: Run RED**

```bash
cd src-tauri
cargo test --locked orchestrator::agent_adapter::opencode
cargo test --locked orchestrator::agent_adapter::types
cargo test --locked orchestrator::workflow
cargo test --locked orchestrator::runner
```

Expected: OpenCode provider is unknown.

- [ ] **Step 6: Implement probe, launch and resume**

Resolve executable realpath/version through the Gate B support contract. Build argv without shell strings. Runner obtains the session shell dialect and calls the safe renderer. Project runtime preflight uses `OpenCodeRuntimeBridge::verify`; no bridge means unavailable, not SentinelLine fallback.

For OpenCode resume, create a fresh Workbench terminal in the same worktree with a new preallocated Agent Runtime ID, link `resumedFromAgentSessionId`, atomically update the active attempt’s terminal/agent IDs, then render/write `opencode --session ... --prompt ...`. Do not reuse the old idle TUI. For adapters with `Reuse`, fix the existing resume path so it actually renders/writes the built plan instead of discarding it; a write failure ends the new runtime as Failed and leaves the attempt repairable.

- [ ] **Step 7: Route official events through the existing reducer**

OSC decoder produces `AgentRuntimeMutation`; existing terminal reader/reducer/`handle_normalized_agent_event` remains the only path. `session.idle -> Completed` must update Agent Runtime before `maybe_complete_from_agent_runtime_completed` moves the task to Verifying.

- [ ] **Step 8: Extend the real OpenCode contract**

With an exact pinned OpenCode version and configured test provider, launch a visible TUI in a real PTY, capture native session ID, observe Working, NeedsInput fixture or simulated typed Plugin event, Completed and Failed cases, then resume by `--session`. Record the exact runtime evidence ID in support manifest. Missing provider credentials keep the L3 row `NOT VERIFIED` and runtime capability blocked.

- [ ] **Step 9: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked orchestrator::agent_adapter
cargo test --locked orchestrator::workflow
cargo test --locked orchestrator::runner
cargo test --locked orchestrator::agent_runtime_bridge
CC_PARTNER_L3_TARGET=opencode cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
```

Expected: exact-version L3 passes before support becomes writable/available.

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/orchestrator/agent_adapter src-tauri/src/orchestrator/runner.rs src-tauri/src/orchestrator/workflow.rs src-tauri/src/orchestrator/agent_runtime_bridge.rs src-tauri/src/orchestrator/completion.rs src-tauri/src/orchestrator/experiments src-tauri/src/orchestrator/models.rs src-tauri/src/workbench/sessions.rs src-tauri/src/agent_hub/support/support-manifest.json src-tauri/tests/agent_hub_cli_contract.rs docs/development/quality-matrix.json
git commit -m "feat: add visible opencode runner"
```

### Task 6: Expose Plugin Components and OpenCode Provider in the Existing UI

**Files:**
- Modify: `web/src/api/agentHub.ts`
- Modify: `web/src/lib/types/agentHub.ts`
- Modify: `web/src/lib/schemas/agentHub.ts`
- Modify: `web/src/pages/AgentHub/useAgentHubController.ts`
- Create: `web/src/pages/AgentHub/PluginComponentsDrawer.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.module.css`
- Modify: `web/src/components/domain/AgentAssetRow/AgentAssetRow.tsx`
- Modify: `web/src/pages/Settings/AutomationSettingsPanel.tsx`
- Modify: `web/src/pages/Workbench/controllers/useWorkbenchAutomationController.ts`
- Modify: `web/src/pages/Workbench/WorkbenchAutomation.characterization.test.tsx`
- Modify: `web/src/pages/Orchestrator/controllers/useOrchestratorController.ts`
- Modify: `web/src/pages/Orchestrator/views/OrchestratorExperimentPanel.tsx`
- Modify: `web/src/lib/types/orchestrator.ts`
- Modify: `web/src/lib/schemas/orchestrator.ts`
- Modify: `web/src/pages/Workbench/agentPhasePresentation.ts`
- Modify: `web/src/i18n/locales/{en,zh}/{agentHub,orchestrator,workbench,settings}.json`
- Test: corresponding `.test.ts` and `.test.tsx`

**Interfaces:**
- Consumes per-component Plugin report and four-provider adapter catalog.
- Produces Plugin drawer, runtime bridge status/preview and `openCodeVisible` selection.

- [ ] **Step 1: Write Plugin decoder/view tests**

Decode component revision/status/ownership/residual fields. Render a mixed package and assert each component has its own target matrix; aggregate partial status names the exact blockers. Delete preview lists components that will tombstone versus remain referenced.

- [ ] **Step 2: Write provider catalog tests**

Settings/Workbench/Orchestrator display OpenCode with:

- executable/version/support evidence;
- project bridge `ready | previewRequired | conflict | unsupported`;
- HookEvent completion;
- clear blocked reason.

Unknown provider wire values remain decoder errors rather than silently mapping to Claude.

- [ ] **Step 3: Write Workbench controller ownership test**

Extend `useWorkbenchAutomationController` and existing views; do not add a page-level `useWorkbenchController` or eighth Workbench controller. All new Hooks stay before early returns. Pure views do not import `@/api/*`.

- [ ] **Step 4: Run RED**

```bash
cd web
npm test -- AgentHub PluginComponentsDrawer AutomationSettings Workbench Orchestrator
```

Expected: Plugin component/provider cases fail.

- [ ] **Step 5: Implement Plugin surfaces**

Reuse `Drawer`, `Dialog`, `Card`, `Pill`, `Button`, `StatusMessage`. Show source target, canonical component, fixed revision, ownership, target materialization and residual reason. Never compress mixed status to a green “synced”.

- [ ] **Step 6: Implement provider surfaces**

Add `openCodeVisible` to strict types/schemas/options. When preview is required, open the existing Agent Hub project preview for `.opencode/plugins/cc-partner-runtime.ts`; do not enable or overwrite without the project confirmation.

- [ ] **Step 7: Run GREEN**

```bash
cd web
npm test -- AgentHub PluginComponentsDrawer AutomationSettings Workbench Orchestrator localeParity
npm run check:css-tokens
npm run check:i18n
npm run check:modules
npm run build
```

Expected: all pass, module boundaries stay intact and both languages cover provider/status errors.

- [ ] **Step 8: Commit**

```bash
git add web/src/api/agentHub.ts web/src/lib/types/agentHub.ts web/src/lib/schemas/agentHub.ts web/src/pages/AgentHub web/src/components/domain/AgentAssetRow web/src/pages/Settings/AutomationSettingsPanel.tsx web/src/pages/Workbench web/src/pages/Orchestrator web/src/lib/types/orchestrator.ts web/src/lib/schemas/orchestrator.ts web/src/i18n
git commit -m "feat: expose plugin and opencode runtime status"
```

### Task 7: Complete Plugin Migration and Prepare N+2 Legacy Removal

**Files:**
- Modify: `src-tauri/src/agent_hub/migration.rs`
- Modify: `src-tauri/src/claude_code_assets.rs`
- Modify: `src-tauri/src/commands/claude_code_assets.rs`
- Modify: `src-tauri/src/net/routes/claude_code_assets.rs`
- Modify: `src-tauri/src/net/routes/claude_md_sync.rs`
- Modify: `src-tauri/src/sync/mixed_version_harness.rs`
- Modify: `src-tauri/src/cc/mixed_version_harness.rs`
- Modify: `web/src/App.tsx`
- Test: same files

**Interfaces:**
- Produces idempotent decomposition/adoption preview for existing Claude/Codex/OpenCode Plugins.
- Produces `LegacyAgentAssetCompatibilityStatus` with `gaVersion`, `stableMigrationEvidence`, `earliestRemovalVersion`.

- [ ] **Step 1: Write migration preview/idempotency tests**

Seed old Claude Plugin/MCP/Skill state plus Codex/OpenCode Plugins. First run creates previews only; confirmation imports one package/child graph and adopts eligible sources. Second run produces no new revisions. Collision/unverified activation remains source-only/externalCollision.

- [ ] **Step 2: Write downgrade tests**

After Gate D data exists, disable Hub and run the compatibility façade. Last successful target files remain usable; old tables/routes ignore unknown Hub tables and never clean CAS. Re-enable Hub and recover pending projections.

- [ ] **Step 3: Write N+2 guard tests**

Actual removal is allowed only when running version is at least `earliestRemovalVersion` and a checked-in stable migration evidence ID exists. Before that, old routes remain registered but hidden from new UI. This task adds the guard/status and removal checklist, not an early deletion.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::migration
cargo test --locked sync::mixed_version_harness
cargo test --locked cc::mixed_version_harness
```

Expected: Plugin graph/compatibility assertions fail.

- [ ] **Step 5: Implement migration and compatibility status**

Reuse the decomposer/adoption transaction. Old command/route DTOs translate from Hub during N/N+1; they never perform a second direct mutation when Hub is enabled. New UI routes remain `/agent-hub`; old frontend routes redirect.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::migration
cargo test --locked sync::mixed_version_harness
cargo test --locked cc::mixed_version_harness
node ../scripts/check-p2p-route-inventory.mjs
```

Expected: migration is idempotent and route inventory reflects the guarded compatibility window.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/migration.rs src-tauri/src/claude_code_assets.rs src-tauri/src/commands/claude_code_assets.rs src-tauri/src/net/routes/claude_code_assets.rs src-tauri/src/net/routes/claude_md_sync.rs src-tauri/src/sync/mixed_version_harness.rs src-tauri/src/cc/mixed_version_harness.rs web/src/App.tsx
git commit -m "feat: finalize agent plugin migration compatibility"
```

### Task 8: Certify Plugin Semantics and the OpenCode Visible Runtime

**Files:**
- Create: `src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs`
- Modify: `src-tauri/tests/agent_hub_cli_contract.rs`
- Modify: `src-tauri/tests/agent_hub_replication_smoke.rs`
- Modify: `web/tests/agent-hub.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `AGENTS.md` only if the shipped component/directory inventory changed

**Interfaces:**
- Produces evidence IDs `L2-AGENT-HUB-D-PLUGIN-001`, `L2-AGENT-HUB-D-RUNTIME-001`, `L3-AGENT-HUB-D-OPENCODE-001`, `E2E-AGENT-HUB-D-001`.

- [ ] **Step 1: Write the mixed Plugin smoke**

Import one Plugin containing portable Skill/MCP/Command/Agent, targetOnly Hook and OpenCode runtime residual. Verify per target full/partial/sourceOnly/activationRequired, Snapshot/LAN/Git exact restore, and package deletion preserving shared/standalone refs.

- [ ] **Step 2: Write the real runtime smoke**

On an opted-in Workbench project:

1. preflight exact OpenCode version/bridge;
2. create worktree + visible terminal with preallocated IDs;
3. launch `opencode --prompt`;
4. observe native session/Working;
5. receive permission/NeedsInput when the fixture requests it;
6. receive `session.idle`/Completed before task enters Verifying;
7. exercise `session.error`/Failed;
8. resume using `--session`;
9. interrupt with Ctrl-C and exercise manual takeover;
10. confirm OSC bytes never enter terminal replay/UI.

- [ ] **Step 3: Extend E2E**

Cover Plugin component Drawer, ownership-aware delete preview, residual statuses, OpenCode provider catalog, runtime bridge project preview/collision, Runner selection, phase changes and Attention navigation.

- [ ] **Step 4: Run Gate D full verification**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub::plugins
cargo test --locked orchestrator::agent_adapter
cargo test --locked orchestrator::runner
cargo test --locked orchestrator::agent_runtime_bridge
cargo test --locked --test agent_hub_gate_d_runtime_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1
cd ../web
npm run lint
npm run check:css-tokens
npm run check:i18n
npm run check:modules
npm test -- AgentHub PluginComponentsDrawer AutomationSettings Workbench Orchestrator localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts
cd ..
node scripts/check-agent-hub-support-manifest.mjs --gate-d
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all pass; the L3 OpenCode row is verified only for the exact version/provider fixture actually exercised.

- [ ] **Step 5: Update persistent truth**

Document Plugin component ownership/residual behavior, evidence-backed Hook mapping policy, OpenCode runtime bridge/opt-in requirement, four-provider registry, rollback and N+2 removal prerequisites. Include official behavior references:

- `https://opencode.ai/docs/cli/`
- `https://opencode.ai/docs/plugins/`
- `https://developers.openai.com/codex/cli/reference/`
- `https://code.claude.com/docs/en/cli-reference`

- [ ] **Step 6: Commit**

```bash
git add src-tauri/tests/agent_hub_gate_d_runtime_smoke.rs src-tauri/tests/agent_hub_cli_contract.rs src-tauri/tests/agent_hub_replication_smoke.rs web/tests/agent-hub.spec.ts docs/prd.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md AGENTS.md
git commit -m "feat: complete multi-cli agent hub"
```

Expected: omit `AGENTS.md` from `git add` when no top-level/component inventory entry changed.
