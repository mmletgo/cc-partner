# Agent Hub Portable Asset Backend Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** 建立三 Agent、四类资产的真实库存、canonical 对账、受控本机动作与同类 Agent 远端选择性 Pull 后端合同，并用真实文件/CLI shim L2 证据证明功能等价。

**Architecture:** 新增独立 Portable Inventory read model，扫描结果只描述目标事实，并与现有 Hub canonical/binding/materialization 对账。所有写入使用短期 preview plan、clientRequestId action ledger 和 post-action rescan；远端 Pull 复用 SnapshotEnvelope/CAS/importer，只允许相同 AgentTarget 从远端导入并安装到本机。

**Tech Stack:** Rust 2021、serde、SQLite/sqlx、tokio、axum、reqwest、Agent Hub Revision DAG/CAS/Snapshot、Tauri IPC、CLI shim L2 tests。

## Global Constraints

- 权威设计为 docs/superpowers/specs/2026-08-07-agent-hub-portable-asset-management-parity-design.md；与旧 Gate B/C/D 计划冲突时以该设计为准。
- 资产种类固定为 skill/command/plugin/mcp；目标固定为 claude/codex/opencode。
- 只支持同类 Agent Pull；request、preview、commit 三层都校验 sourceTarget == destinationTarget。
- Inventory refresh 只读，不创建 canonical、revision、binding、ownership 或目标文件。
- 用户级和已 opt-in 项目允许 mutation；未映射或未 opt-in 项目只能导入 canonical，不猜测路径或自动 opt-in。
- 所有 mutation 由 sidecar owner 执行；GUI、legacy invoke 和 legacy P2P 不得形成第二 writer。
- Preview 零写入；Apply 校验 plan、inventory hash、source hash、CLI fingerprint、scope mapping 和 expected revision。
- 未纳管资产可受控启停/卸载但不自动 adoption；只有显式 adopt 创建长期 ownership。
- MCP 凭据在 Hub/Snapshot/CAS/LAN 中保持原字节，日志、错误和 inventory DTO 不含 secret 原文。
- 固定 LAN 无身份鉴权；expected-device/request ID/device ID 只用于路由和幂等，不得称为认证。
- 新增/修改 Rust 业务函数与类型具有严格类型和中文 Business Logic/Code Logic docstring；UTF-8。
- Support manifest 无 exact version/evidence 时 mutation blocked；L1/L2 不升格为 L3。

---

## Prerequisites and Produced Contract

**Prerequisites:** 当前 master 已包含 Agent Hub Gate A–D、SnapshotEnvelope v1、ReplicationLedger、Plugin decomposition、user-instruction preview/apply pattern 和 sidecar owner control 面。

**Produces for the UI plan:**

~~~
agent_hub_inspect_portable_inventory
agent_hub_preview_portable_asset_action
agent_hub_apply_portable_asset_action
agent_hub_get_portable_asset_action
agent_hub_list_remote_portable_inventory
agent_hub_preview_portable_pull
agent_hub_apply_portable_pull
agent_hub_get_portable_pull
~~~

并冻结 PortableInventorySnapshotDto、PortableAssetActionPlanDto、PortableAssetActionResultDto、RemotePortableInventoryDto、PortablePullPlanDto、PortablePullResultDto 的 camelCase wire fixtures。

## File Structure

- Create: src-tauri/src/agent_hub/portable_inventory/{mod.rs,models.rs,reconcile.rs,scanner.rs}
- Create: src-tauri/src/agent_hub/portable_actions/{mod.rs,models.rs,planner.rs,ledger.rs,executor.rs}
- Create: src-tauri/src/agent_hub/portable_service.rs
- Create: src-tauri/src/agent_hub/replication/pull.rs
- Create: src-tauri/src/agent_hub/snapshot/portable_builder.rs
- Modify: src-tauri/src/agent_hub/{mod.rs,models.rs}, targets/*, plugins/decompose.rs, packages/activator.rs, config_patch/*
- Modify: src-tauri/src/claude_code_assets.rs, src-tauri/src/storage/agent_hub_repo.rs
- Modify: src-tauri/src/commands/agent_hub.rs, src-tauri/src/backend/{control.rs,control_agent_hub.rs,control_client.rs}, src-tauri/src/lib.rs
- Modify: src-tauri/src/net/{http_server.rs,protocol.rs,peer_client.rs,routes/agent_hub.rs}
- Create: src-tauri/tests/agent_hub_portable_inventory_smoke.rs
- Create: src-tauri/tests/agent_hub_portable_pull_smoke.rs
- Modify: docs/p2p-protocol.md, docs/development/{testing.md,quality-matrix.json}, src-tauri/CLAUDE.md

## Shared Write Sets

- agent_hub/mod.rs 由 B1、B3、B5、B6 修改；集成者按依赖顺序集中解冲突。
- targets/* 和 plugins/decompose.rs 仅 B2 拥有。
- portable_actions/* 与 storage/agent_hub_repo.rs 仅 B3/B4 串行拥有。
- claude_code_assets.rs、activator/config patch 仅 B4 拥有。
- commands/backend/lib.rs 仅 B5 拥有。
- net/*、snapshot/portable_builder.rs、replication/pull.rs 仅 B6 拥有。
- 两个 smoke 仅 B7；协议、quality matrix、testing、CLAUDE 仅 B8。

## Task Dependency Graph

~~~
B1 -> B2 --\
             +-> B4 -> B5 -> B6 -> B7 -> B8
B1 -> B3 --/
~~~

- Exact edges: B1→B2、B1→B3、{B2,B3}→B4、B4→B5→B6→B7→B8。
- Dependency-ready waves: [B1]、[B2,B3]、[B4]、[B5]、[B6]、[B7]、[B8]。
- B2/B3 可在独立 task worktree 并行；每波按任务编号集成并完成聚焦验证后才能开始后继波。

### Task 1: Define Portable Inventory Models, Identity, Hash and Reconciliation

**Files:**
- Create: src-tauri/src/agent_hub/portable_inventory/{mod.rs,models.rs,reconcile.rs}
- Modify: src-tauri/src/agent_hub/mod.rs
- Test: same files

**Interfaces:**
- Consumes: AgentTarget、AssetKind、ScopeKind、LogicalAsset、TargetBinding、Materialization、AgentHubRepo read APIs、sha256_hex。
- Produces:

~~~rust
pub struct PortableInventorySnapshotDto {
    pub inventory_snapshot_hash: String,
    pub refreshed_at: String,
    pub stale: bool,
    pub targets: Vec<PortableInventoryTargetDto>,
    pub items: Vec<PortableInventoryItemDto>,
}

pub enum PortableInventoryManagementState {
    Unmanaged,
    HubManaged,
    Drifted,
    ExternalCollision,
    Unsupported,
}

pub async fn reconcile_portable_inventory(
    repo: &AgentHubRepo,
    targets: Vec<PortableInventoryTargetDto>,
    discovered: Vec<PortableInventoryItemDto>,
) -> Result<PortableInventorySnapshotDto, AppError>;
~~~

- [ ] **Step 1: Write failing identity/hash tests**

~~~rust
#[test]
fn inventory_identity_separates_target_scope_origin_and_native_id() {
    let a = inventory_item_id(AgentTarget::Claude, "user", "/x/a", "tool");
    assert_ne!(a, inventory_item_id(AgentTarget::Codex, "user", "/x/a", "tool"));
    assert_ne!(a, inventory_item_id(AgentTarget::Claude, "project:p1", "/x/a", "tool"));
    assert_ne!(a, inventory_item_id(AgentTarget::Claude, "user", "/x/b", "tool"));
}
~~~

Shuffle target/item insertion order and expect identical snapshot hash. Changing actualEnabled, source hash, CLI fingerprint, project opt-in or ownership must change it.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked agent_hub::portable_inventory --lib -- --test-threads=1
~~~

Expected: compilation fails because portable_inventory is absent.

- [ ] **Step 3: Implement strict DTO and deterministic hash**

Use serde camelCase, sorted vectors/BTreeMap and the existing Agent Hub canonical JSON subset. Reject Instruction/Agent/Hook kinds at this boundary. Inventory exposes MCP credential-present/hash only.

- [ ] **Step 4: Implement five reconciliation states**

Match by target/scope/origin/logical key/source identity/hash. Absence from a scan never tombstones canonical; desired presence never implies an observed file.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::portable_inventory --lib -- --test-threads=1
git add src-tauri/src/agent_hub/portable_inventory src-tauri/src/agent_hub/mod.rs
git commit -m "feat: define portable asset inventory"
~~~

### Task 2: Wire Three Target Scanners for Four Kinds and Project Scopes

**Files:**
- Create: src-tauri/src/agent_hub/portable_inventory/scanner.rs
- Modify: src-tauri/src/agent_hub/targets/{mod.rs,portable.rs,paths.rs,claude.rs,codex.rs,opencode.rs}
- Modify: src-tauri/src/agent_hub/plugins/decompose.rs
- Test: inline tests in those files

**Interfaces:**
- Consumes: B1 DTOs、AssetAdapter::scan_portable_assets、TargetEnvironment、TargetPathResolver、LocalScopeMapping、DefaultPluginDecomposer。
- Produces:

~~~rust
pub async fn inspect_portable_inventory(
    state: &AppState,
) -> Result<PortableInventorySnapshotDto, AppError>;
~~~

and target facts for Skill/Command/Plugin/MCP with actualEnabled、origin、parentPlugin、hash、capability/evidence。

- [ ] **Step 1: Write failing target×kind/scope fixtures**

For every target seed user Skill/Command/Plugin/MCP, active/disabled state, a Plugin same-name Skill, corrupt MCP, opted-in project and unopted project. Assert package/component relations and no silent merges.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked agent_hub::targets --lib -- --test-threads=1
~~~

Expected: current scan lacks Plugin inventory and unified actual-state DTO.

- [ ] **Step 3: Convert portable discoveries and inspect Plugin packages**

Reuse scan_skill_dirs/markdown/MCP/hash helpers. Discover Plugin through supported official CLI facts plus install path/manifest, then DefaultPluginDecomposer::inspect. Scanning must not write CAS or target files.

- [ ] **Step 4: Enforce project/capability facts**

Paths come only from registered mapping. Unopted projects return projectOptedIn=false and read-only capability. Missing/unknown CLI version may scan but mutation remains blocked. Invalid config preserves source and emits a blocked diagnostic.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::targets --lib -- --test-threads=1
cargo test --locked agent_hub::portable_inventory --lib -- --test-threads=1
git add src-tauri/src/agent_hub/portable_inventory/scanner.rs src-tauri/src/agent_hub/targets src-tauri/src/agent_hub/plugins/decompose.rs
git commit -m "feat: scan portable assets across agent targets"
~~~

### Task 3: Add Action Preview Plans and Durable Ledger

**Files:**
- Create: src-tauri/src/agent_hub/portable_actions/{mod.rs,models.rs,planner.rs,ledger.rs}
- Modify: src-tauri/src/agent_hub/{mod.rs,models.rs}
- Modify: src-tauri/src/storage/agent_hub_repo.rs
- Test: same files

**Interfaces:**
- Consumes: B1 inventory hash/DTO and existing user-instruction plan claim pattern.
- Produces:

~~~rust
pub enum PortableAssetActionKind { Adopt, Enable, Disable, Uninstall, InstallToSourceTarget }
pub enum PortableAssetActionItemState { Succeeded, Skipped, Failed, Blocked, OutcomeUnknown }

pub async fn preview_portable_asset_action(
    state: &AppState,
    request: PreviewPortableAssetActionRequest,
) -> Result<PortableAssetActionPlanDto, AppError>;

pub async fn claim_portable_asset_action(
    repo: &AgentHubRepo,
    plan_token: &str,
    client_request_id: &str,
) -> Result<PortableActionClaim, AppError>;
~~~

- [ ] **Step 1: Write failing plan/ledger tests**

Cover preview zero-write, 10-minute expiry, stale inventory/source/CLI/canonical/mapping, unopted project, unsupported mutation, same request replay, same request/different plan conflict and outcomeUnknown lookup.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked agent_hub::portable_actions --lib -- --test-threads=1
~~~

- [ ] **Step 3: Add additive plan/request tables and atomic claim**

Use AgentHubRepo pool/maintenance gate. Store public preview plus private preconditions/owner fingerprint/expiry. A committed row replays exact outcome; claimed unresolved returns outcomeUnknown.

- [ ] **Step 4: Build exact changes**

Each change records target/kind/path, CLI/file operation, backup policy, hashes, canonical/ownership effect and blocking reasons. Unmanaged enable/disable/uninstall sets canonicalEffect=none; only adopt creates ownership.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::portable_actions --lib -- --test-threads=1
cargo test --locked storage::agent_hub_repo --lib -- --test-threads=1
git add src-tauri/src/agent_hub/portable_actions src-tauri/src/agent_hub/mod.rs src-tauri/src/agent_hub/models.rs src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: add portable asset action plans"
~~~

### Task 4: Execute Target Actions and Verify by Rescan

**Files:**
- Create: src-tauri/src/agent_hub/portable_actions/executor.rs
- Create: src-tauri/src/agent_hub/portable_actions/targets/{mod.rs,claude.rs,codex.rs,opencode.rs}
- Modify: src-tauri/src/agent_hub/portable_actions/mod.rs
- Modify: src-tauri/src/claude_code_assets.rs
- Modify: src-tauri/src/agent_hub/packages/activator.rs
- Modify: src-tauri/src/agent_hub/config_patch/{mod.rs,jsonc.rs,toml.rs}
- Modify: src-tauri/src/agent_hub/projection/atomic_writer.rs
- Test: same files

**Interfaces:**
- Consumes: B2 scanners、B3 plan/ledger、FakeProcessRunner、atomic writer/config patch/Plugin ownership.
- Produces:

~~~rust
pub async fn apply_portable_asset_action(
    state: &AppState,
    request: ApplyPortableAssetActionRequest,
) -> Result<PortableAssetActionResultDto, AppError>;
~~~

- [ ] **Step 1: Write failing CLI/file executor tests**

Shims record argv and mutate fixture inventory. Lock Claude Plugin scope argv, Skill/Command safe moves+backup, MCP semantic patch, Plugin shared preserve, unsupported target zero spawn and changed-source fail closed.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked agent_hub::portable_actions --lib -- --test-threads=1
~~~

- [ ] **Step 3: Refactor mature Claude helpers behind target executor**

Make low-level set/move/backup/remove helpers crate-visible; do not remove the legacy guard on old commands. Implement supported Codex/OpenCode actions only when manifest evidence allows.

- [ ] **Step 4: Apply, rescan and close ledger**

Rescan affected scope after every action. Mark succeeded only when observed state matches expected. Spawn/transport ambiguity becomes outcomeUnknown and is reconciled from ledger+inventory before retry. Return per-item partial results.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::portable_actions --lib -- --test-threads=1
cargo test --locked claude_code_assets --lib -- --test-threads=1
git add src-tauri/src/agent_hub/portable_actions src-tauri/src/agent_hub/packages/activator.rs src-tauri/src/agent_hub/config_patch src-tauri/src/agent_hub/projection/atomic_writer.rs src-tauri/src/claude_code_assets.rs
git commit -m "feat: execute portable asset actions"
~~~

### Task 5: Expose Owner Service, Control and Tauri IPC

**Files:**
- Create: src-tauri/src/agent_hub/portable_service.rs
- Modify: src-tauri/src/agent_hub/mod.rs
- Modify: src-tauri/src/commands/agent_hub.rs
- Modify: src-tauri/src/backend/{control.rs,control_agent_hub.rs,control_client.rs}
- Modify: src-tauri/src/lib.rs
- Test: same files

**Interfaces:**
- Consumes: B1–B4 production contracts.
- Produces the first four commands in Produced Contract and increments AGENT_HUB_API_VERSION from 2 to 3.

- [ ] **Step 1: Write failing dispatch/authority tests**

Assert HeadlessOwner direct service, GuiClient control proxy, inspect read-only fallback, mutation version fail-closed, all op inventories and mutation classification.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked commands::agent_hub backend::control_agent_hub backend::control_client --lib -- --test-threads=1
~~~

- [ ] **Step 3: Wire strict owner/control/IPC**

Register exact op/command names. Preview/apply/get typed DTOs only; mutation paths require API v3 and owner. Apply uses long-mutation timeout; inspect/get use query timeout.

- [ ] **Step 4: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked commands::agent_hub backend::control_agent_hub backend::control_client --lib -- --test-threads=1
git add src-tauri/src/agent_hub/portable_service.rs src-tauri/src/agent_hub/mod.rs src-tauri/src/commands/agent_hub.rs src-tauri/src/backend src-tauri/src/lib.rs
git commit -m "feat: expose portable asset management IPC"
~~~

### Task 6: Implement Same-Agent Remote Inventory and Pull

**Files:**
- Create: src-tauri/src/agent_hub/replication/pull.rs
- Create: src-tauri/src/agent_hub/snapshot/portable_builder.rs
- Modify: src-tauri/src/agent_hub/{mod.rs,replication/mod.rs,snapshot/mod.rs,portable_service.rs}
- Modify: src-tauri/src/net/{http_server.rs,protocol.rs,peer_client.rs,routes/agent_hub.rs}
- Modify: src-tauri/src/commands/agent_hub.rs
- Modify: src-tauri/src/backend/{control_agent_hub.rs,control_client.rs}
- Test: same files

**Interfaces:**
- Consumes: SnapshotEnvelope builder/importer、ObjectStore、ReplicationLedger、B1–B5 contracts/devices.
- Produces the last four commands in Produced Contract, capability agent-hub.portable-pull.v1 and typed remote inventory/preview/progress/result fixtures.

- [ ] **Step 1: Write failing protocol tests**

Cover capability missing→zero request, metadata inventory without secrets, target mismatch, skipExisting, replaceAfterPreview, unmapped/unopted canonical-only, replay conflict, chunk resume, partial and legacyLossy credential.

- [ ] **Step 2: Run RED**

~~~bash
cd src-tauri
cargo test --locked agent_hub::replication::pull net::routes::agent_hub net::protocol --lib -- --test-threads=1
~~~

- [ ] **Step 3: Freeze remote selection and same-target preview**

Build standard SnapshotEnvelope/CAS objects from frozen inventory selection without source adoption. Bind source/destination target, device, mapping, conflict policy and inventory hash. Validate target equality before transfer.

- [ ] **Step 4: Reuse object transfer/import and B4 install**

Negotiate missing hashes, stream ≤8 MiB chunks with offset resume, import canonical transactionally, then install only mapped+opted-in same-target items. Persist per-item outcome for exact replay.

- [ ] **Step 5: Wire capability/control/IPC atomically**

Register routes/capability in one build; extend peer client and owner control. Keep expected-device/Host/Origin/Content-Type/resource guards and no-auth disclosure semantics.

- [ ] **Step 6: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::replication::pull net::routes::agent_hub net::protocol --lib -- --test-threads=1
cargo test --locked backend::control_agent_hub --lib -- --test-threads=1
git add src-tauri/src/agent_hub/replication/pull.rs src-tauri/src/agent_hub/snapshot/portable_builder.rs src-tauri/src/agent_hub/mod.rs src-tauri/src/agent_hub/replication/mod.rs src-tauri/src/agent_hub/snapshot/mod.rs src-tauri/src/agent_hub/portable_service.rs src-tauri/src/net src-tauri/src/commands/agent_hub.rs src-tauri/src/backend/control_agent_hub.rs src-tauri/src/backend/control_client.rs
git commit -m "feat: add same-agent portable asset pull"
~~~

### Task 7: Certify Inventory, Actions and Pull with L2 Smokes

**Files:**
- Create: src-tauri/tests/agent_hub_portable_inventory_smoke.rs
- Create: src-tauri/tests/agent_hub_portable_pull_smoke.rs
- Modify: src-tauri/src/agent_hub/support/support-manifest.json
- Test: both new files

**Interfaces:**
- Consumes: B1–B6 production services; fixtures from agent_hub_gate_b_smoke.rs, agent_hub_gate_d_runtime_smoke.rs and agent_hub_replication_smoke.rs.
- Produces evidence IDs L2-AGENT-HUB-PORTABLE-PARITY-001 and L2-AGENT-HUB-PORTABLE-PULL-001.

- [ ] **Step 1: Write isolated-home inventory/action smoke**

Seed 3×4 user/project facts and CLI shims. Inspect, preview/apply enable/disable/uninstall, inspect actual change; cover backup, Plugin shared preserve, MCP comment+secret, unopted no-write and replay.

- [ ] **Step 2: Write two-owner Pull smoke**

Run two isolated local backends. Exercise remote inventory→selection→objects→canonical import→same-target install→rescan for every target; assert mismatch, mapping canonical-only, replace preview, duplicate request, interrupt/resume and partial report.

- [ ] **Step 3: Run tests; add only injectable seams**

~~~bash
cd src-tauri
cargo test --locked --test agent_hub_portable_inventory_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_portable_pull_smoke -- --nocapture --test-threads=1
~~~

No production bypass flags. Only capabilities exercised by these shims may cite L2; real product/platform versions remain blocked/NOT VERIFIED.

- [ ] **Step 4: Run GREEN and commit**

~~~bash
cd src-tauri
cargo fmt --check
cargo test --locked --test agent_hub_portable_inventory_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_portable_pull_smoke -- --nocapture --test-threads=1
cargo test --locked --lib agent_hub -- --test-threads=1
git add src-tauri/tests/agent_hub_portable_inventory_smoke.rs src-tauri/tests/agent_hub_portable_pull_smoke.rs src-tauri/src/agent_hub/support/support-manifest.json
git commit -m "test: certify portable asset management parity"
~~~

### Task 8: Register Protocol, Evidence and Backend Contracts

**Files:**
- Modify: docs/p2p-protocol.md
- Modify: docs/development/{testing.md,quality-matrix.json}
- Modify: src-tauri/CLAUDE.md
- Test: repository document/traceability checks

**Interfaces:**
- Consumes: B6 routes/capability/DTOs and B7 evidence.
- Produces documented backend contract consumed by the UI plan.

- [ ] **Step 1: Run failing route/evidence checks before docs**

~~~bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
~~~

Expected: new routes/evidence are missing until documentation is updated.

- [ ] **Step 2: Document exact semantics**

Record literal routes/methods/capability/retry/idempotency, metadata-only inventory, same-target rule, mapping/opt-in, CAS limits, credential/no-auth handling and N/N+1 legacy retention. Register L2 commands and keep L3 NOT VERIFIED.

- [ ] **Step 3: Run completion verification**

~~~bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked --lib agent_hub -- --test-threads=1
cargo test --locked --test agent_hub_portable_inventory_smoke -- --nocapture --test-threads=1
cargo test --locked --test agent_hub_portable_pull_smoke -- --nocapture --test-threads=1
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-quality-traceability.mjs --self-test
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
~~~

- [ ] **Step 4: Commit**

~~~bash
git add docs/p2p-protocol.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md
git commit -m "docs: register portable asset parity evidence"
~~~

## Completion Contract

- Eight tasks are committed and integrated in dependency order.
- Produced command/DTO fixtures are frozen and camelCase round-trip tested.
- Inventory sees 3×4 assets without automatic adoption or target writes.
- User/opted-in project mutation succeeds only through owner preview/apply/rescan; unopted stays no-write.
- Unmanaged actions do not acquire ownership; explicit adopt does.
- Same-agent Pull supports inventory, selection, conflict preview, resumable CAS, canonical import, mapped/opted-in install, replay and partial results.
- Cross-target Pull fails before transfer/import/target mutation.
- Plugin ownership and MCP credential/privacy contracts pass.
- Fresh unit/clippy/two L2/route/quality/docs checks pass with exact output recorded.
- Real multi-host/full-platform/product-version L3 remains NOT VERIFIED unless separately executed.

## Adjacent-Race Checklist

- refresh/apply/rescan generation ordering；
- same-item concurrent enable/disable/uninstall；
- plan expiry and source/canonical/mapping/CLI drift；
- replay/retry/outcomeUnknown reconciliation；
- Pull cancellation/offset resume/commit replay/partial；
- watcher vs manual target-file mutation；
- Plugin shared-reference deletion；
- MCP semantic patch vs external edit；
- owner restart and GUI/backend version mismatch；
- expected-device/resource limit/legacyLossy credential。
