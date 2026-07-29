# Agent Hub Gate A — Foundation and Instructions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立可崩溃恢复的 Canonical Hub，并让 Claude Code、Codex CLI、OpenCode 的用户级、项目根和嵌套指令在 sidecar owner 中自动对账。

**Architecture:** 新增 `agent_hub` Rust 领域，SQLite 保存作用域、DAG、目标绑定、materialization/job/conflict，CAS 保存不可变正文和目录对象；Instruction Compiler 负责 shared/adapted/targetOnly 块，Projection Scheduler 负责 precondition + 原子替换。GUI 只经 owner control plane 读写，项目通过 Workbench project/checkout binding opt-in。

**Tech Stack:** Rust 2021、tokio、notify、SQLite/sqlx、SHA-256 CAS、Tauri IPC、React 19、TypeScript、Vitest、Playwright。

## Global Constraints

- `AgentHubService` 只在 `RuntimeRole::HeadlessOwner` 执行 scan/reconcile/project；GuiClient 只做代理。
- watcher 500 ms trailing debounce；变更目录 30 秒 rescan、全 scope 10 分钟 rescan；projection 全局并行上限 4、同资产串行。
- project 未 opt-in 时只 scan/preview，零文件写入。
- opt-in 覆盖 main checkout 和所有 Workbench 登记 worktree；外部 worktree 不写入。
- 子目录单来源普通正文默认 shared；用户级/项目根单来源默认 source targetOnly。
- Codex 同层扫描必须同时报告 `AGENTS.override.md`、`AGENTS.md` 和 fallback；被遮蔽非空文件不能丢失。
- OpenCode 嵌套 `AGENTS.md` 必须列出祖先规则相对路径，不复制祖先正文。
- 文件投影失败不能推进 materialization committed；共同正文 conflict 冻结该资产全部 target。
- Unix Hub 目录 `0700`、正文/object/temp `0600`；Windows 当前用户 ACL。
- Rust/TypeScript 新函数使用项目要求的中文 docstring；前端 Hooks 全部位于 early return 前。

---

## File Structure

- Create: `src-tauri/src/agent_hub/mod.rs` — 领域公开接口与 `AgentHubService` 组装。
- Create: `src-tauri/src/agent_hub/models.rs` — Scope/Asset/Revision/Binding/Materialization/Conflict DTO。
- Create: `src-tauri/src/agent_hub/object_store.rs` — 明文 SHA-256 CAS 与 TreeManifest。
- Create: `src-tauri/src/agent_hub/revision_graph.rs` — DAG ancestor、merge-base、tombstone。
- Create: `src-tauri/src/agent_hub/project_scope.rs` — Workbench project/checkout binding 与 opt-in preview。
- Create: `src-tauri/src/agent_hub/targets/{mod.rs,paths.rs,claude.rs,codex.rs,opencode.rs}` — target probe/path/instruction adapter。
- Create: `src-tauri/src/agent_hub/instructions/{mod.rs,document.rs,compiler.rs,reconcile.rs}` — 块模型、导入、渲染、三方合并。
- Create: `src-tauri/src/agent_hub/projection/{mod.rs,atomic_writer.rs,scheduler.rs}` — durable projection job。
- Create: `src-tauri/src/agent_hub/runtime.rs` — watcher、rescan、job recovery。
- Create: `src-tauri/src/agent_hub/autostart.rs` — backend supervisor/login-start 适配。
- Create: `src-tauri/src/storage/agent_hub_repo.rs` — schema 与数据库访问。
- Create: `src-tauri/src/commands/agent_hub.rs` — thin Tauri commands。
- Create: `src-tauri/src/backend/control_agent_hub.rs` — owner-local control dispatch。
- Create: `src-tauri/src/attention/agent_hub_source.rs` — conflict/projection blocked Attention source。
- Create: `web/src/pages/AgentHub/` — controller + pure views。
- Create: `web/src/components/domain/AgentAssetRow/` — target matrix row。
- Create: `web/src/api/agentHub.ts`, `web/src/lib/types/agentHub.ts`, `web/src/lib/schemas/agentHub.ts`。
- Create: `web/src/i18n/locales/{en,zh}/agentHub.json`。

## Task Dependency Graph

```text
A1 -> A2 -> A3 -> A4 --\
  \-> A5 --------------+-> A6 -> A7 --\
  \-> A8 -----------------------------+-> A9 -> A10
```

- Exact edges: `A1→A2→A3→A4`; `A1→A5`; `{A4,A5}→A6→A7`; `A1→A8`; `{A7,A8}→A9→A10`.
- Dependency-ready waves: `[A1]`, `[A2,A5,A8]`, `[A3]`, `[A4]`, `[A6]`, `[A7]`, `[A9]`, `[A10]`.
- `A2/A5/A8` may use isolated task worktrees concurrently; all other tasks wait for the listed predecessors and the wave integration baseline.

### Task 1: Add Canonical Models and Additive SQLite Schema

**Files:**
- Create: `src-tauri/src/agent_hub/mod.rs`
- Create: `src-tauri/src/agent_hub/models.rs`
- Create: `src-tauri/src/storage/agent_hub_repo.rs`
- Modify: `src-tauri/src/storage/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/Cargo.toml`
- Test: `src-tauri/src/storage/agent_hub_repo.rs`

**Interfaces:**
- Produces: `AgentHubRepo::ensure_schema`, `AgentHubRepo::with_gate`, `LogicalAsset`, `Revision`, `TargetBinding`, `Materialization`, `AgentHubConflict`.
- Produces: `AppState.agent_hub_repo: Arc<AgentHubRepo>`.
- Consumes: `DatabaseMaintenanceGate`, single-connection `SqlitePool`.

- [ ] **Step 1: Write schema and round-trip tests**

Add tests that create an in-memory pool, call `AgentHubRepo::ensure_schema`, then assert:

```rust
let asset = repo
    .insert_asset(NewLogicalAsset {
        scope_id: scope.id.clone(),
        kind: AssetKind::Instruction,
        origin_namespace: "standalone".into(),
        logical_key: "src-tauri".into(),
        display_name: "src-tauri rules".into(),
        policy: AssetPolicy::Shared,
    })
    .await?;
let revision = repo
    .append_revision(NewRevision {
        id: RevisionId::new_v7(),
        asset_lineage_id: asset.id.clone(),
        parents: vec![],
        operation: RevisionOperation::Upsert,
        origin_kind: RevisionOriginKind::Migration,
        origin_target: Some(AgentTarget::Claude),
        origin_replica_id: "device-a".into(),
        payload_hash: Some("a".repeat(64)),
        tree_manifest_hash: None,
        created_at: "2026-07-29T00:00:00Z".into(),
    })
    .await?;
assert_eq!(repo.get_revision(&revision.id).await?.unwrap(), revision);
```

Also assert the unique key is `(scope_id, kind, origin_namespace, logical_key)`, revision parent rows preserve order, a delete revision permits no payload hash, and `desired_enabled=false` is target-local.

- [ ] **Step 2: Run the tests to verify RED**

Run:

```bash
cd src-tauri
cargo test --locked storage::agent_hub_repo -- --nocapture
```

Expected: compilation fails because the repo and model types do not exist.

- [ ] **Step 3: Define exact domain enums and IDs**

In `models.rs`, add serde camelCase enums and newtypes:

```rust
pub enum AgentTarget { Claude, Codex, OpenCode }
pub enum AssetKind { Instruction, Skill, Command, Agent, Mcp, Plugin, Hook }
pub enum AssetPolicy { Shared, Adapted, TargetOnly }
pub enum RevisionOperation { Upsert, Delete }
pub enum RevisionOriginKind { Filesystem, Ui, Lan, Git, Migration }
pub enum DesiredPresence { Present, Absent }
pub enum MaterializationStatus {
    Synced, Pending, Drift, Detached, Conflict, Blocked,
    Unsupported, ActivationRequired, ExternalCollision,
}

pub struct Revision {
    pub id: RevisionId,
    pub asset_lineage_id: String,
    pub parents: Vec<RevisionId>,
    pub generation: u64,
    pub operation: RevisionOperation,
    pub origin_kind: RevisionOriginKind,
    pub origin_target: Option<AgentTarget>,
    pub origin_replica_id: String,
    pub payload_hash: Option<String>,
    pub tree_manifest_hash: Option<String>,
    pub created_at: String,
}
```

Enable UUID v7 in `Cargo.toml` by changing the existing uuid feature list from `["v4"]` to `["v4", "v7"]`; `RevisionId::new_v7()` wraps `Uuid::now_v7()`.

- [ ] **Step 4: Implement additive schema and repository writes**

`AgentHubRepo::ensure_schema` creates these tables and indexes with `IF NOT EXISTS`:

```text
agent_hub_scopes
agent_hub_assets
agent_hub_asset_lineages
agent_hub_revisions
agent_hub_revision_parents
agent_hub_variants
agent_hub_target_bindings
agent_hub_materializations
agent_hub_projection_jobs
agent_hub_conflicts
agent_hub_project_mappings
agent_hub_checkout_bindings
agent_hub_replica_state
```

All mutating methods use `with_shared_write_lease`; multi-row revision/parent/head updates use one SQL transaction. `append_revision` computes `generation=max(parent.generation)+1`, rejects missing parents and rejects payload on delete.

- [ ] **Step 5: Wire the repo into runtime construction**

Call `AgentHubRepo::ensure_schema(&pool)` from `init_db`; construct with the shared maintenance gate in `build_app_state_with_role`; add the field to every `AppState { ... }` fixture found by:

```bash
rg -l 'AppState \{' src-tauri/src src-tauri/tests
```

Use `AgentHubRepo::new(pool.clone())` in isolated fixtures and `with_gate` in production.

- [ ] **Step 6: Run GREEN and regression tests**

Run:

```bash
cd src-tauri
cargo fmt --check
cargo test --locked storage::agent_hub_repo
cargo test --locked backend::runtime::tests
```

Expected: all pass; running `ensure_schema` twice preserves rows.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/agent_hub src-tauri/src/storage/agent_hub_repo.rs src-tauri/src/storage/mod.rs src-tauri/src/backend/runtime.rs src-tauri/src/state.rs src-tauri/src/lib.rs src-tauri/src/backup/restore.rs src-tauri/src/cc/mixed_version_harness.rs src-tauri/src/commands/transfer.rs src-tauri/src/commands/workbench/common.rs src-tauri/src/sync/mixed_version_harness.rs src-tauri/src/transfer/receiver/tests.rs src-tauri/src/workbench/claude_sessions_test.rs src-tauri/src/workbench/lan_fleet/collector.rs src-tauri/src/workbench/workspace_restore.rs
git commit -m "feat: add agent hub canonical schema"
```

### Task 2: Implement Plaintext CAS and Revision DAG Merge Bases

**Files:**
- Create: `src-tauri/src/agent_hub/object_store.rs`
- Create: `src-tauri/src/agent_hub/revision_graph.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Test: `src-tauri/src/agent_hub/object_store.rs`
- Test: `src-tauri/src/agent_hub/revision_graph.rs`

**Interfaces:**
- Consumes: `AgentHubRepo`, `Revision`, `RevisionId`.
- Produces: `ObjectStore::{put_blob,get_blob,put_tree,get_tree,gc_unreferenced}`.
- Produces: `RevisionGraph::{maximal_common_ancestors,merge_base}`.

- [ ] **Step 1: Write failing CAS tests**

Cover exact bytes, executable bit, path traversal, symlink escape and permissions:

```rust
let object = store.put_blob(b"token=plain-text").await?;
assert_eq!(store.get_blob(&object.hash).await?, b"token=plain-text");
assert_eq!(object.hash, sha256_hex(b"token=plain-text"));
assert!(store.object_path(&object.hash).starts_with(store.root()));
```

On Unix, assert root mode `0700` and object/temp mode `0600`. A symlink resolving outside the input tree must return a `TreeEntryDiagnostic::OutsideRoot`, not be followed.

- [ ] **Step 2: Write failing DAG tests**

Build:

```text
      a1──a2──left
       └──b2──right
```

Assert `maximal_common_ancestors(left,right)==[a1]`; add a criss-cross graph with two maximal ancestors and assert `merge_base` recursively invokes the supplied content merger in revision-ID order. No common ancestor + unequal payload returns `MergeBaseOutcome::Conflict`.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::object_store
cargo test --locked agent_hub::revision_graph
```

Expected: compilation fails for missing modules.

- [ ] **Step 4: Implement CAS**

Use `<data_dir>/agent-hub/objects/sha256/<first-two>/<hash>`. Write to a sibling UUID temp, `sync_all`, re-read/hash, rename, then sync the parent directory on Unix. `TreeManifest` sorts normalized forward-slash relative paths and stores `{path, blobHash, entryType, executable}`.

- [ ] **Step 5: Implement DAG traversal**

Use batched repo parent reads and a visited set capped by the selected snapshot’s 100,000 revision limit. `maximal_common_ancestors` removes a candidate when it is an ancestor of another common candidate. Multiple bases are recursively merged in lexicographic revision-ID order into an in-memory virtual base; virtual-base conflict returns conflict without writing a revision.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::object_store
cargo test --locked agent_hub::revision_graph
```

Expected: all pass; invalid hash/path never escapes the CAS root.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub
git commit -m "feat: add agent hub object and revision stores"
```

### Task 3: Resolve Target Homes and Define the Instruction Adapter Contract

**Files:**
- Create: `src-tauri/src/agent_hub/targets/mod.rs`
- Create: `src-tauri/src/agent_hub/targets/paths.rs`
- Create: `src-tauri/src/agent_hub/targets/claude.rs`
- Create: `src-tauri/src/agent_hub/targets/codex.rs`
- Create: `src-tauri/src/agent_hub/targets/opencode.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub trait AssetAdapter: Send + Sync {
    fn target(&self) -> AgentTarget;
    fn probe(&self, env: &TargetEnvironment) -> Result<TargetProbe, AppError>;
    fn scan_instruction_sources(
        &self,
        scope: &LocalScopeMapping,
        env: &TargetEnvironment,
    ) -> Result<Vec<InstructionSource>, AppError>;
    fn render_instruction(
        &self,
        document: &InstructionDocument,
        context: &InstructionRenderContext,
    ) -> Result<RenderedInstruction, AppError>;
}
```

- Produces: `TargetPathResolver::resolve_all(&TargetEnvironment) -> TargetHomes`.

- [ ] **Step 1: Write environment precedence tests**

Use an injected `TargetEnvironment { home, vars, path_entries }`; never mutate the real process environment. Assert:

```rust
assert_eq!(homes.claude.config_root, PathBuf::from("/tmp/claude-home"));
assert_eq!(homes.codex.config_root, PathBuf::from("/tmp/codex-home"));
assert_eq!(homes.opencode.config_root, PathBuf::from("/tmp/oc-dir"));
assert_eq!(homes.opencode.config_file, PathBuf::from("/tmp/custom-opencode.json"));
```

where vars contain `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG`. Add XDG and default fallback cases.

- [ ] **Step 2: Write instruction source precedence tests**

For Codex, create all three sources and assert scan returns active override plus inactive non-empty `AGENTS.md`/fallback diagnostics. For OpenCode, assert the nearest local `AGENTS.md` is marked native-active and ancestors are returned as explicit prelude dependencies. For Claude, assert user and project paths stay separate.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::targets
```

Expected: compilation fails for missing target modules.

- [ ] **Step 4: Implement path resolution and executable probe**

Resolve executable realpaths without shell. `TargetProbe` contains `{target, executable, version, configRoot, support, fingerprint}`; a changed executable/version/config root invalidates prior materialization probe. Unknown/parse-failed versions are scan-only.

- [ ] **Step 5: Implement instruction-only adapters**

Gate A adapters scan and render only instruction documents:

```text
Claude user: <claude-config>/CLAUDE.md
Codex user:  <codex-config>/AGENTS.override.md
OpenCode user: <opencode-config>/AGENTS.md
Project directory: CLAUDE.md | AGENTS.override.md | AGENTS.md
```

Codex uses override as Hub’s managed projection so OpenCode’s `AGENTS.md` remains target-specific.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::targets
```

Expected: all path and precedence fixtures pass with no writes.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/targets src-tauri/src/agent_hub/mod.rs
git commit -m "feat: add agent instruction target probes"
```

### Task 4: Build the Instruction Compiler and Three-Way Reconciler

**Files:**
- Create: `src-tauri/src/agent_hub/instructions/mod.rs`
- Create: `src-tauri/src/agent_hub/instructions/document.rs`
- Create: `src-tauri/src/agent_hub/instructions/compiler.rs`
- Create: `src-tauri/src/agent_hub/instructions/reconcile.rs`
- Modify: `src-tauri/src/agent_hub/targets/{claude,codex,opencode}.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct InstructionBlock {
    pub id: String,
    pub mode: InstructionBlockMode,
    pub common_markdown: Option<String>,
    pub structured_intent: Option<StructuredInstructionIntent>,
    pub variants: BTreeMap<AgentTarget, String>,
    pub heading_path: Vec<String>,
}

pub enum InstructionReconcileOutcome {
    NoChange,
    Revision(NewInstructionRevision),
    Conflict(NewAgentHubConflict),
}
```

- Consumes: `InstructionSource`, `RenderedInstruction`, materialization base block map.

- [ ] **Step 1: Write import classification tests**

Add fixtures for:

1. project non-root only `CLAUDE.md` → ordinary blocks shared;
2. project root only `CLAUDE.md` → Claude targetOnly;
3. identical three files → shared;
4. different files → exact identical blocks shared, unique blocks targetOnly;
5. a block containing `CLAUDE.md`, `Read`, hook event or target config path → source targetOnly + `needsAdaptation`.

- [ ] **Step 2: Write rendering tests for the user’s nested-directory case**

Given shared body `"本目录负责 Rust 网络层"` at `src-tauri/src/net`, assert byte-identical user body in:

```text
src-tauri/src/net/CLAUDE.md
src-tauri/src/net/AGENTS.override.md
src-tauri/src/net/AGENTS.md
```

Assert the OpenCode file alone has a managed prelude listing `../../../AGENTS.md`, `../../AGENTS.md`, `../AGENTS.md` for existing managed ancestors, in root-to-parent order; parent body text must not be copied.

- [ ] **Step 3: Write reconcile tests**

Test base/current/external outcomes:

- disjoint shared block edits auto-merge;
- same block edit creates conflict and does not advance current head;
- adapted target edit changes only that target variant;
- whole-file external delete returns detached;
- invalid UTF-8 returns blocked while preserving original bytes.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::instructions
```

Expected: compilation fails for missing compiler/reconciler.

- [ ] **Step 5: Implement deterministic block parsing**

Split Markdown by heading/paragraph/fenced-code boundaries while preserving exact byte slices and order. Stable block IDs come from persisted base-map IDs; new blocks receive UUIDv7. Do not insert markers into user files.

Implement `StructuredInstructionIntent::DiscoveryBeforeEdit` with three versioned renderers. Free text is never sent to a model.

- [ ] **Step 6: Implement OpenCode prelude separation**

`RenderedInstruction` contains:

```rust
pub struct RenderedInstruction {
    pub bytes: Vec<u8>,
    pub block_map: Vec<RenderedBlockRange>,
    pub managed_prefix_len: usize,
    pub diagnostics: Vec<PortabilityDiagnostic>,
}
```

The OpenCode prelude occupies `managed_prefix_len`; reverse reconciliation treats edits in that range as OpenCode target-only.

- [ ] **Step 7: Implement three-way reconcile**

Compare base block hashes to Hub current and external current. Common payload conflicts create `AgentHubConflictScope::CanonicalAsset`; target-only conflicts create `AgentHubConflictScope::Target`. Delete-vs-edit is always conflict.

- [ ] **Step 8: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::instructions
```

Expected: all fixtures pass; no target-specific block appears in another target output.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/agent_hub/instructions src-tauri/src/agent_hub/targets
git commit -m "feat: compile multi-cli instruction documents"
```

### Task 5: Bind Workbench Projects and Worktrees with Exact Opt-In Preview

**Files:**
- Create: `src-tauri/src/agent_hub/project_scope.rs`
- Modify: `src-tauri/src/workbench/projects.rs`
- Modify: `src-tauri/src/commands/workbench/projects.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: `src-tauri/src/agent_hub/project_scope.rs`
- Test: `src-tauri/src/commands/workbench/tests.rs`

**Interfaces:**
- Produces:

```rust
pub async fn build_project_enable_preview(
    state: &AppState,
    project_id: &str,
) -> Result<AgentHubProjectPreview, AppError>;

pub async fn enable_project_scope(
    state: &AppState,
    request: EnableAgentHubProjectRequest,
) -> Result<AgentHubProjectStatus, AppError>;

pub async fn refresh_checkout_bindings(
    state: &AppState,
    project_id: &str,
) -> Result<Vec<ProjectCheckoutBinding>, AppError>;
```

- Consumes: `WorkbenchProjectRepo`, `WorkbenchWorktreeRepo`, `workbench::git::{git_common_dir,list_worktrees}`.

- [ ] **Step 1: Write project/worktree tests**

Create a real temporary Git repository with main, one Workbench-recorded worktree and one unregistered worktree. Assert preview lists main + registered only, reports dirty state/diffs, and performs zero writes.

- [ ] **Step 2: Write opt-in inheritance test**

Enable the project, create a second Workbench worktree through the existing local helper, then assert a checkout binding is created before the helper returns. A pre-existing conflicting `AGENTS.md` produces a blocked binding and warning, not overwrite.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::project_scope
cargo test --locked commands::workbench::tests
```

Expected: new assertions fail because no Hub binding exists.

- [ ] **Step 4: Implement portable project identity**

Store `hubProjectId`, optional normalized Git remote fingerprint and local Workbench ID mapping. Absolute checkout paths stay only in `agent_hub_checkout_bindings`; they never enter portable asset payloads.

- [ ] **Step 5: Hook Workbench project/worktree lifecycle**

After add/open/create/list reconciliation, call `refresh_checkout_bindings`. The hook is idempotent and only schedules projection when project opt-in is true. Removing a Workbench worktree marks its binding detached; it does not tombstone canonical assets.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::project_scope
cargo test --locked commands::workbench::tests
```

Expected: registered worktrees inherit opt-in; Git index/HEAD remain unchanged.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/project_scope.rs src-tauri/src/workbench/projects.rs src-tauri/src/commands/workbench/projects.rs src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: bind agent hub project checkouts"
```

### Task 6: Add Durable Projection Jobs and Atomic File Replacement

**Files:**
- Create: `src-tauri/src/agent_hub/projection/mod.rs`
- Create: `src-tauri/src/agent_hub/projection/atomic_writer.rs`
- Create: `src-tauri/src/agent_hub/projection/scheduler.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files
- Test: `src-tauri/tests/quality_faults.rs`

**Interfaces:**
- Produces:

```rust
pub async fn enqueue_projection(
    &self,
    request: ProjectionRequest,
) -> Result<ProjectionJob, AppError>;

pub async fn run_ready_jobs(
    &self,
    cancel: &CancellationToken,
) -> Result<ProjectionRunStats, AppError>;
```

- Consumes: rendered bytes, expected external hash, target path, `desiredPresence`, `desiredEnabled`.

- [ ] **Step 1: Write failure-injection tests**

Inject failure at temp write, file sync, precondition recheck, rename and DB commit. Assert:

```text
prepared + unchanged target -> recoverable
target hash == rendered hash -> mark committed
target hash == old base hash -> retry atomic replace
target hash differs from both -> drift/reconcile
```

Unknown files in a directory projection must produce drift/preview, never recursive deletion.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::projection
cargo test --locked --test quality_faults agent_hub_projection -- --nocapture --test-threads=1
```

Expected: compilation fails or named test is absent.

- [ ] **Step 3: Implement `AtomicProjectionWriter`**

Use sibling temp + file sync + precondition hash + atomic replace + target re-hash. For directories use sibling staging and backup rename; backup is deleted only after committed materialization.

- [ ] **Step 4: Implement scheduler ownership**

Use a global `Semaphore::new(4)` and per-asset async mutex map. A canonical conflict prevents all target jobs for the asset; target conflict prevents only that target/checkout. A project without opt-in is filtered before job insertion.

- [ ] **Step 5: Implement crash recovery**

On owner startup, list prepared/writing jobs and reconcile actual hashes before processing new watcher events. Never convert prepared to committed based only on DB state.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::projection
cargo test --locked --test quality_faults agent_hub_projection -- --nocapture --test-threads=1
```

Expected: all injection points preserve either old or new complete file; no partial file is visible.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/projection src-tauri/src/storage/agent_hub_repo.rs src-tauri/tests/quality_faults.rs
git commit -m "feat: add durable agent hub projections"
```

### Task 7: Run Watch/Reconcile in the Sidecar Owner

**Files:**
- Create: `src-tauri/src/agent_hub/runtime.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/state.rs`
- Test: `src-tauri/src/agent_hub/runtime.rs`
- Test: `src-tauri/tests/backend_cli_smoke.rs`

**Interfaces:**
- Produces: `AgentHubRuntime::start(state: AppState) -> CancellationToken`.
- Produces: `AppState.agent_hub_cancel: Arc<Mutex<Option<CancellationToken>>>`.
- Consumes: target adapters, project bindings, reconciler, scheduler.

- [ ] **Step 1: Write watcher and de-loop tests**

Use temp HOME/config roots and fake clock. Assert an external edit produces one revision and one projection wave; the watcher event generated by Hub’s own rendered hash is a no-op. Burst 20 events in 500 ms produces one scan.

- [ ] **Step 2: Write GUI-closed owner smoke**

Start `cc-partner-backend serve` under isolated `CC_PARTNER_DATA_DIR`, enable a user instruction, terminate only the GUI fixture, edit a target file, and poll the second target until it converges. Stop by the existing control route.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::runtime
cargo test --locked --test backend_cli_smoke agent_hub_owner -- --nocapture --test-threads=1
```

Expected: named tests fail because no runtime task exists.

- [ ] **Step 4: Implement watcher scheduling**

Use `notify` only as an event hint. Maintain dirty directory set, 500 ms trailing debounce, 30-second changed-directory ticker and 10-minute full-scope ticker with `MissedTickBehavior::Skip`. Each scan compares stored external hashes before loading bytes.

- [ ] **Step 5: Wire owner start and shutdown**

In `start_background_tasks(Headless)`, use `start_cancelled_task_once(&state.agent_hub_cancel, ...)`. In `shutdown_backend_runtime`, cancel the token before session shutdown. GUI mode logs skip and never watches.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::runtime
cargo test --locked --test backend_cli_smoke agent_hub_owner -- --nocapture --test-threads=1
```

Expected: all pass; duplicate backend start still has one owner/watcher.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/runtime.rs src-tauri/src/agent_hub/mod.rs src-tauri/src/backend/runtime.rs src-tauri/src/state.rs src-tauri/tests/backend_cli_smoke.rs
git commit -m "feat: run agent hub in backend owner"
```

### Task 8: Add Login Start, Crash Supervision and GUI/Backend Version Handshake

**Files:**
- Create: `src-tauri/src/agent_hub/autostart.rs`
- Create: `src-tauri/src/backend/supervisor.rs`
- Modify: `src-tauri/src/backend/mod.rs`
- Modify: `src-tauri/src/backend/cli.rs`
- Modify: `src-tauri/src/backend/control.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/config.rs`
- Test: same files
- Test: `src-tauri/tests/backend_cli_smoke.rs`

**Interfaces:**
- Produces: CLI subcommand `cc-partner-backend supervise`.
- Produces: `AgentHubAutostart::{install,inspect,remove}` using injected command/file adapters.
- Produces: `BackendControlFile.agent_hub_api_version: u32` and `AGENT_HUB_API_VERSION=1`.
- Produces: `AppConfig.agent_hub: AgentHubConfig`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHubConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub background_enabled: bool,
}
```

`Default` 两字段均为 `false`；首次确认启用 Hub 后才设置 `enabled=true`，只有成功安装登录启动后才设置 `background_enabled=true`。

- [ ] **Step 1: Write supervisor backoff tests**

With a fake child runner returning exit codes `[1,1,1,0]`, assert delays `[1s,2s,4s]` and stop after exit 0. A child alive for 10 minutes resets the next failure delay to 1 second. No test sleeps real time; inject a `Sleeper`.

- [ ] **Step 2: Write platform artifact snapshot tests**

Assert generated artifacts invoke the current backend executable with `supervise` and no shell:

```text
macOS: ~/Library/LaunchAgents/com.cc-partner.agent-hub.plist
Windows: current-user Task Scheduler XML, LogonTrigger
Linux: ~/.config/systemd/user/cc-partner-agent-hub.service
```

Artifacts use RunAtLoad/logon start but no second restart policy because `supervise` owns exponential backoff.

- [ ] **Step 3: Write version mismatch tests**

Old/missing `agentHubApiVersion` permits status/preview reads but rejects mutations with `upgradeRequired`. New GUI to v1 backend works; a backend advertising a higher incompatible major is read-only.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked backend::supervisor
cargo test --locked agent_hub::autostart
cargo test --locked backend::control
```

Expected: missing modules/fields fail.

- [ ] **Step 5: Implement `supervise`**

Spawn the current executable’s `serve` subcommand directly. Exit 0 ends supervision for the login session; non-zero exit restarts at 1/2/4/8/16/32/60 seconds. Forward `CC_PARTNER_DATA_DIR`; never print control token.

- [ ] **Step 6: Implement user-level autostart**

Write artifacts atomically and invoke `launchctl bootstrap`, `schtasks /Create /XML`, or `systemctl --user daemon-reload && enable --now` as argv arrays. Permission/capability failure returns `backgroundStartUnavailable` and leaves same-process runtime working.

Add `AgentHubConfig` to `AppConfig` with `#[serde(default)]`, include it in `Default`, validation and config round-trip tests. Persist `background_enabled=true` only after `inspect` confirms the installed login item references the current executable; uninstall or failed inspection resets that field without disabling the current owner process.

- [ ] **Step 7: Implement control handshake**

Serialize `agentHubApiVersion` in the control file/status. `BackendControlClient` exposes:

```rust
pub fn require_agent_hub_write_compatibility(
    &self,
    required_version: u32,
) -> Result<(), AppError>;
```

Call it before every Agent Hub mutation.

- [ ] **Step 8: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked backend::supervisor
cargo test --locked agent_hub::autostart
cargo test --locked backend::control
cargo test --locked --test backend_cli_smoke -- --nocapture --test-threads=1
```

Expected: all pass; tests do not alter the developer’s real login items.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/agent_hub/autostart.rs src-tauri/src/backend src-tauri/src/config.rs src-tauri/tests/backend_cli_smoke.rs
git commit -m "feat: supervise agent hub background owner"
```

### Task 9: Expose Owner APIs, Attention and the Minimal Unified UI

**Files:**
- Create: `src-tauri/src/commands/agent_hub.rs`
- Create: `src-tauri/src/backend/control_agent_hub.rs`
- Create: `src-tauri/src/attention/agent_hub_source.rs`
- Modify: `src-tauri/src/backend/control_api.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/attention/{mod.rs,models.rs,aggregator.rs}`
- Create: `web/src/api/agentHub.ts`
- Create: `web/src/lib/types/agentHub.ts`
- Create: `web/src/lib/schemas/agentHub.ts`
- Modify: `web/src/lib/types/index.ts`
- Modify: `web/src/lib/schemas/index.ts`
- Create: `web/src/pages/AgentHub/AgentHub.tsx`
- Create: `web/src/pages/AgentHub/useAgentHubController.ts`
- Create: `web/src/pages/AgentHub/InstructionBlocksDrawer.tsx`
- Create: `web/src/pages/AgentHub/AgentHub.module.css`
- Create: `web/src/pages/AgentHub/index.ts`
- Create: `web/src/components/domain/AgentAssetRow/{AgentAssetRow.tsx,AgentAssetRow.module.css,index.ts}`
- Create: `web/src/i18n/locales/{en,zh}/agentHub.json`
- Modify: `web/src/App.tsx`
- Modify: `web/src/components/layout/AppShell/AppShell.tsx`
- Modify: `web/src/lib/{types,schemas}/attention.ts`
- Modify: `web/src/i18n/index.ts`
- Modify: `AGENTS.md`
- Test: corresponding `.test.rs`, `.test.ts`, `.test.tsx`

**Interfaces:**
- Produces IPC/control operations:

```text
agent_hub.get_status
agent_hub.list_assets
agent_hub.get_asset
agent_hub.update_instruction
agent_hub.update_instruction_block
agent_hub.pair_instruction_variants
agent_hub.preview_project
agent_hub.enable_project
agent_hub.resolve_conflict
agent_hub.set_target_binding
```

- Produces Attention source kinds `agentHubConflict`, `agentHubProjectionBlocked`.

- [ ] **Step 1: Write Rust command/control parity tests**

Run each operation once as HeadlessOwner and once through a fake GuiClient control client; assert identical camelCase DTOs. Mutation with incompatible control version returns stable `upgradeRequired`.

- [ ] **Step 2: Write Attention aggregation tests**

Insert one canonical conflict and one blocked checkout materialization. Assert dedupe IDs are stable and targets are:

```rust
AttentionTarget::AgentHubAsset {
    asset_id,
    conflict_id: Some(conflict_id),
}
```

Attention remains navigation-only.

- [ ] **Step 3: Write frontend schema/controller tests**

`agentHubSnapshotDecoder` must reject an unknown required status enum without serializing payload. Controller tests cover first-load error, stale refresh, preview, enable, conflict resolve and request sequence preventing project-switch stale writes.

- [ ] **Step 4: Write the page characterization test**

Render the page with:

- CLI probe summary;
- scope and kind filters;
- instruction row with Claude/Codex/OpenCode target cells;
- exact project preview Dialog;
- conflict Drawer;
- instruction block Drawer showing `shared | adapted | targetOnly`, common body/target variants and exact promotion/pairing diff;
- blocked/unsupported states.

Assert all actions use the controller; pure views do not import `@/api/*`.

- [ ] **Step 5: Run RED**

```bash
cd src-tauri
cargo test --locked commands::agent_hub
cargo test --locked attention::agent_hub_source
cd ../web
npm test -- agentHub AgentHub attention typeBarrel
```

Expected: missing modules/routes/types fail.

- [ ] **Step 6: Implement thin owner command/control paths**

Tauri command functions inspect `state.runtime_role`: owner calls `AgentHubService`; GuiClient calls `BackendControlClient`. Control request bodies use `deny_unknown_fields`, ≤256 KiB metadata limit, and never log instruction content.

- [ ] **Step 7: Implement frontend domain**

Use `invokeDecoded`; keep all hooks in `useAgentHubController` before any early return. `AgentAssetRow` composes existing `Card`, `Pill`, `Button`, `StatusMessage`; CSS uses tokens only. Add `agentHub` i18n namespace in both languages.

`InstructionBlocksDrawer` is the explicit control for the “部分共用、部分 CLI 独有” case: it shows persisted block provenance/mode, lets the user promote a root/user targetOnly block to shared, pair target-specific blocks as adapted variants, or revert to targetOnly, and always previews all affected target-file diffs before mutation. It edits Hub block metadata/content only; no private marker is inserted into Markdown.

- [ ] **Step 8: Replace route ownership**

Add `/agent-hub`; redirect `/claude-md` and `/claude-code` to it. Remove old pages from navigation but retain their source/commands for N/N+1 compatibility. Update the root component list with `AgentAssetRow`.

- [ ] **Step 9: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked commands::agent_hub
cargo test --locked attention::agent_hub_source
cd ../web
npm test -- agentHub AgentHub attention typeBarrel localeParity
npm run check:tokens
npm run check:i18n
npm run build
```

Expected: all pass; no old page owns a watcher.

- [ ] **Step 10: Commit**

```bash
git add src-tauri/src/commands/agent_hub.rs src-tauri/src/backend/control_agent_hub.rs src-tauri/src/backend/control_api.rs src-tauri/src/backend/control_client.rs src-tauri/src/attention src-tauri/src/lib.rs web/src/api/agentHub.ts web/src/lib web/src/pages/AgentHub web/src/components/domain/AgentAssetRow web/src/i18n web/src/App.tsx web/src/components/layout/AppShell/AppShell.tsx AGENTS.md
git commit -m "feat: add agent hub instruction workspace"
```

### Task 10: Migrate Existing CLAUDE.md State and Certify Gate A

**Files:**
- Create: `src-tauri/src/agent_hub/migration.rs`
- Modify: `src-tauri/src/commands/claude_md.rs`
- Modify: `src-tauri/src/sync/claude_md.rs`
- Modify: `src-tauri/src/storage/claude_md_repo.rs`
- Create: `src-tauri/tests/agent_hub_gate_a_smoke.rs`
- Create: `web/tests/agent-hub.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

**Interfaces:**
- Consumes: old `claude_md` row/file, Gate A service/UI.
- Produces: idempotent preview migration and N/N+1 user-Claude summary dual-write.

- [ ] **Step 1: Write idempotent migration tests**

Seed old DB row + external `~/.claude/CLAUDE.md`. Assert migration creates one user instruction asset/revision, marks it Claude targetOnly, shows exact generated Codex/OpenCode diffs, and second run creates no revision.

- [ ] **Step 2: Write Gate A process smoke**

The smoke uses isolated HOME/data dir and fake CLI executables to verify:

1. owner starts and recovers prepared jobs;
2. project scan before opt-in performs zero writes;
3. after opt-in, nested Claude edit reaches same-directory Codex/OpenCode files;
4. OpenCode prelude references ancestors;
5. concurrent same-block edit creates Attention;
6. `git diff --cached` and fixture HEAD are unchanged.

- [ ] **Step 3: Write E2E**

Use `backendHarness` to verify `/agent-hub` status, project preview, enable confirmation, target matrix, conflict deep link and old-route redirect. Register stable evidence ID `E2E-AGENT-HUB-A-001`.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::migration
cargo test --locked --test agent_hub_gate_a_smoke -- --nocapture --test-threads=1
cd ../web
npm run test:e2e -- agent-hub.spec.ts
```

Expected: migration/smoke/E2E fail before wiring.

- [ ] **Step 5: Implement preview-first migration**

Migration writes canonical state but sets all new target bindings `desiredPresence=absent` until user-level confirmation. During N/N+1, accepted user Claude projection dual-writes only the legacy row summary; legacy vector clock never decides Hub conflicts.

- [ ] **Step 6: Update persistent truth**

Document only shipped Gate A behavior. Mark Skill/MCP/Plugin/LAN/Git/OpenCode runtime as later gates, not as complete. Add quality rows with unexecuted platforms `NOT VERIFIED`.

- [ ] **Step 7: Run Gate A full verification**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub
cargo test --locked --test agent_hub_gate_a_smoke -- --nocapture --test-threads=1
cd ../web
npm run lint
npm run check:tokens
npm run check:i18n
npm test -- AgentHub attention typeBarrel localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts
cd ..
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all exit 0; no plaintext fixture appears in logs.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent_hub/migration.rs src-tauri/src/commands/claude_md.rs src-tauri/src/sync/claude_md.rs src-tauri/src/storage/claude_md_repo.rs src-tauri/tests/agent_hub_gate_a_smoke.rs web/tests/agent-hub.spec.ts docs/prd.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "feat: complete agent hub instruction foundation"
```

## Completion Contract

Gate A is complete only when Task 10's full command set passes from a clean committed Gate A branch, the owner continues reconciliation with the GUI closed, nested instruction changes converge to the corresponding target document without overwriting target-only blocks, unopted projects remain write-free, Workbench worktrees obey the binding rules, and conflict/crash recovery evidence is recorded. Quality rows may mark only actually exercised versions/platforms verified; every unexecuted platform remains `NOT VERIFIED`.
