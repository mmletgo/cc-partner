# Agent Hub Gate C — Replication and Backup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 Canonical Hub 通过同一个可验证的 SnapshotEnvelope v1 完成源侧手动 LAN push、Git device-lane 自动备份和确认式导入，同时保留完整 revision ancestry、tombstone、variant、conflict 与明文 credential-bearing payload。

**Architecture:** Snapshot Builder 从 Gate A/B 的 SQLite DAG 与 CAS 生成确定性 envelope；LAN 协议按 prepare/object/commit 三阶段协商缺失内容，Git 则把同一 envelope 展开为可读 device lane。两条通道最终都调用同一个 `SnapshotImporter`，先验证/落 CAS，再在单个数据库事务中导入 lineage/revision/head，随后异步 reconcile；文件投影成功不属于传输提交事务。

**Tech Stack:** Rust 2021、serde_json、SHA-256、UUIDv7、SQLite/sqlx、axum、reqwest、tokio、Git CLI、React 19、TypeScript、Vitest、Playwright。

## Global Constraints

- Gate B 全部通过后才开始本计划；Snapshot 不得绕过 Gate A Revision DAG/CAS 或 Gate B typed payload。
- `SnapshotEnvelope.format="cc-partner-agent-hub"`、`formatVersion=1`、`canonicalization="RFC8785-JSON"` 固定。
- `snapshotHash` 对移除 `snapshotHash` 字段后的 canonical JSON 做 SHA-256；不得依赖普通 `serde_json::to_string` 的 map 顺序。
- v1 只接受 RFC 8785 兼容子集：object key 为 ASCII、禁止浮点、整数不超过 `2^53-1`、UTF-8 字符串使用 JSON 标准 escape。
- selection 最多 100,000 entries、未压缩总量 2 GiB、单 blob 512 MiB、manifest 32 MiB；LAN chunk ≤8 MiB。
- 凭据在 Hub、Snapshot、LAN、Git 与目标配置中保持原字节；日志、错误与 UI 摘要继续脱敏。
- LAN 只有源用户选择目标后的 push；不新增浏览源资产或目标主动 pull 的 Hub API/UI。
- `sourceDeviceId`、`clientRequestId`、expected device header 只用于路由绑定/幂等，不描述为身份认证。
- 每台设备只写 Git `agent-hub/devices/<deviceId>/`；fetch/rebase 不等于导入其他 lane。
- Git 远端 lane 只有 preview + 用户 confirm 后才进入 Hub；定时任务永不自动 import。
- 未映射项目可以导入 canonical backup，但不得猜测本机路径或自动 opt-in。
- N/N+1 继续保留旧 CLAUDE.md/Claude asset 路由；新 UI 不展示旧 pull，旧路由结果不能算 Hub push 成功。
- 新增/修改 Rust 与 TypeScript 代码遵守根 `AGENTS.md` 的中文 docstring、strict 类型、token 与 Hooks-before-early-return 合同。

---

## File Structure

- Create: `src-tauri/src/agent_hub/snapshot/{mod.rs,canonical_json.rs,envelope.rs,builder.rs,archive.rs,importer.rs}`。
- Create: `src-tauri/src/agent_hub/replication/{mod.rs,ledger.rs,receiver.rs,sender.rs}`。
- Create: `src-tauri/src/net/routes/agent_hub.rs`。
- Create: `src-tauri/src/agent_hub/git/{mod.rs,lane.rs,runtime.rs,preview.rs}`。
- Extend: `src-tauri/src/storage/agent_hub_repo.rs`。
- Modify: `src-tauri/src/net/{http_server.rs,protocol.rs,routes/mod.rs}`。
- Modify: `src-tauri/src/cloud_sync/{mod.rs,runtime.rs,engine.rs}` — 共享 Git singleflight，不改变旧领域自动 import 语义。
- Extend: `src-tauri/src/commands/agent_hub.rs`, `src-tauri/src/backend/control_agent_hub.rs`。
- Extend: `web/src/pages/AgentHub/`, `web/src/api/agentHub.ts`, `web/src/lib/{types,schemas}/agentHub.ts`。
- Create: `src-tauri/tests/agent_hub_replication_smoke.rs`。
- Test: `web/tests/agent-hub.spec.ts`。

## Task Dependency Graph

```text
C1 -> C2 -> C3 -> C4 -> C5 -> C6 -> C7 -> C8
```

- Exact edges are the linear chain shown above: canonical envelope precedes archive/import, which precedes LAN transport, Git lanes and mixed-version certification.
- Dependency-ready waves: `[C1]`, `[C2]`, `[C3]`, `[C4]`, `[C5]`, `[C6]`, `[C7]`, `[C8]`.
- Do not overlap write workers inside Gate C; use a fresh task implementer and the integrated predecessor commit as each task baseline.

### Task 1: Implement SnapshotEnvelope v1 and the Canonical JSON Subset

**Files:**
- Create: `src-tauri/src/agent_hub/snapshot/mod.rs`
- Create: `src-tauri/src/agent_hub/snapshot/canonical_json.rs`
- Create: `src-tauri/src/agent_hub/snapshot/envelope.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct SnapshotEnvelopeV1 {
    pub format: String,
    pub format_version: u32,
    pub canonicalization: String,
    pub snapshot_id: String,
    pub snapshot_hash: String,
    pub source_replica_id: String,
    pub created_at: String,
    pub selection: SnapshotSelection,
    pub asset_heads: BTreeMap<String, Vec<String>>,
    pub assets: Vec<SnapshotAsset>,
    pub lineages: Vec<SnapshotLineage>,
    pub revisions: Vec<SnapshotRevision>,
    pub variants: Vec<SnapshotVariant>,
    pub conflicts: Vec<SnapshotConflict>,
    pub aliases: Vec<SnapshotAlias>,
    pub objects: Vec<SnapshotObjectDescriptor>,
}

pub struct SnapshotLimits {
    pub max_entries: u64,
    pub max_uncompressed_bytes: u64,
    pub max_blob_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_chunk_bytes: u64,
}
```

- Produces: `canonicalize_snapshot_without_hash`, `compute_snapshot_hash`, `validate_snapshot`.

- [ ] **Step 1: Write canonicalization vectors**

Cover:

- shuffled object insertion produces identical bytes/hash;
- ASCII keys sort lexicographically, which is the RFC 8785 UTF-16 order for this accepted subset;
- Unicode string values and escape sequences match checked-in expected bytes;
- floats, non-ASCII map keys, duplicate decoded keys and integers above `9_007_199_254_740_991` are rejected;
- changing only `snapshotHash` does not change the recomputed hash input;
- changing a parent, tombstone, alias or object size changes the hash.

- [ ] **Step 2: Write limit tests**

Test each boundary at limit and limit+1. A limit failure returns a stable diagnostic containing counts/sizes only. Include a `plain-fixture-secret` object and assert no validation error/log includes its bytes.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::snapshot::canonical_json
cargo test --locked agent_hub::snapshot::envelope
```

Expected: missing modules fail.

- [ ] **Step 4: Implement the RFC 8785-compatible subset**

Recursively emit JSON without whitespace. Sort validated ASCII object keys by byte order, escape strings per JSON, and emit only boolean/null/string/safe integer/array/object. Snapshot schema uses decimal strings for any domain counter that may exceed the safe integer limit. Do not normalize Unicode string values or asset bytes.

- [ ] **Step 5: Implement hash and schema validation**

Parse with duplicate-key detection before deserializing to the typed envelope. Require exact format/version/canonicalization, sorted unique revision/object IDs, valid SHA-256 hex, UUID/RFC3339 fields, referential integrity and hard limits. Compute hash from a clone with the field omitted, then constant-time compare normalized hex.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::snapshot::canonical_json
cargo test --locked agent_hub::snapshot::envelope
```

Expected: deterministic vectors pass and malformed manifests allocate only within declared limits.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/snapshot/mod.rs src-tauri/src/agent_hub/snapshot/canonical_json.rs src-tauri/src/agent_hub/snapshot/envelope.rs src-tauri/src/agent_hub/mod.rs
git commit -m "feat: define agent hub snapshot envelope"
```

### Task 2: Build and Expand Deterministic Snapshot Archives

**Files:**
- Create: `src-tauri/src/agent_hub/snapshot/builder.rs`
- Create: `src-tauri/src/agent_hub/snapshot/archive.rs`
- Modify: `src-tauri/src/agent_hub/snapshot/mod.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub async fn build_snapshot(
    repo: &AgentHubRepo,
    objects: &ObjectStore,
    request: SnapshotSelectionRequest,
) -> Result<BuiltSnapshot, AppError>;

pub fn expand_readable_archive(
    snapshot: &BuiltSnapshot,
    destination: &Path,
) -> Result<ExpandedSnapshot, AppError>;

pub fn repack_readable_archive(
    source: &Path,
    limits: &SnapshotLimits,
) -> Result<BuiltSnapshot, AppError>;
```

- [ ] **Step 1: Write selection-closure tests**

Select one asset head and assert the envelope includes:

- the asset/logical identity and every parent needed to reach retained merge bases;
- current target variants, tombstone and unresolved conflicts;
- referenced blobs/tree manifests/supporting files exactly once;
- project portable identity/external aliases but no local absolute checkout path;
- no unrelated asset.

Test full Hub, user scope, one project and explicit asset selections.

- [ ] **Step 2: Write readable archive round-trip**

Expand then repack a fixture containing instruction, binary Skill file, MCP credentials, two heads and a tombstone. Assert identical canonical manifest/hash and object hashes. Assert output permissions are `0700` directories and `0600` files on Unix.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::snapshot::builder
cargo test --locked agent_hub::snapshot::archive
```

Expected: builder/archive modules are absent.

- [ ] **Step 4: Implement a read-transaction snapshot builder**

Read the selected heads and their ancestry in one consistent SQLite read transaction. Sort every collection by stable ID before canonicalization. Re-hash each CAS object while streaming it into the archive descriptor; a missing/corrupt object blocks the whole snapshot.

Before allocating `snapshotId`/`createdAt`, compute `selectionStateHash` over the canonical selection plus selected asset/lineage/revision/head/variant/conflict/alias/object identities. Cache the last completed envelope by `{selectionHash,selectionStateHash}`; an identical Hub state reuses its prior envelope, UUIDv7, timestamp and `snapshotHash`. This makes repeated full-lane export byte-stable instead of creating metadata-only Git commits.

- [ ] **Step 5: Implement the readable layout**

Write:

```text
snapshot.json
objects/sha256/<first-two>/<hash>
history/<asset-id>/<revision-id>/revision.json
user/{instructions,skills,commands,agents,mcp,plugins}/...
projects/<hub-project-id>/{project.json,instructions,assets}/...
```

The readable files are views indexed by `snapshot.json`; importer trusts only the validated envelope/object hashes, never directory names. Reject symlinks, traversal, duplicate normalized paths and unknown required metadata.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::snapshot::builder
cargo test --locked agent_hub::snapshot::archive
```

Expected: byte-stable round-trip passes and limit failures never emit partial success.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/snapshot src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: build deterministic agent hub snapshots"
```

### Task 3: Import Snapshot Lineage, Aliases and Heads Transactionally

**Files:**
- Create: `src-tauri/src/agent_hub/snapshot/importer.rs`
- Modify: `src-tauri/src/agent_hub/snapshot/mod.rs`
- Modify: `src-tauri/src/agent_hub/revision_graph.rs`
- Modify: `src-tauri/src/agent_hub/instructions/reconcile.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files
- Test: `src-tauri/tests/quality_faults.rs`

**Interfaces:**
- Produces:

```rust
pub async fn inspect_import(
    &self,
    snapshot: &ValidatedSnapshot,
) -> Result<SnapshotImportPreview, AppError>;

pub async fn commit_import(
    &self,
    snapshot: ValidatedSnapshot,
    selection: ConfirmedImportSelection,
) -> Result<SnapshotImportOutcome, AppError>;
```

- [ ] **Step 1: Write DAG convergence tests**

Create two replicas that branch from a common revision. Assert:

- disjoint instruction blocks produce a merge revision with both heads as parents;
- same-block changes preserve two heads and create a conflict;
- identical revision IDs/external aliases deduplicate;
- delete-vs-edit conflicts;
- distinct `hubProjectId` values mapped as external aliases merge into one local project scope;
- unmapped project revisions import but schedule zero projections.

- [ ] **Step 2: Write failure-boundary tests**

Inject missing parent, corrupt object, DB failure before head update and crash after CAS insertion. No invalid revision/head becomes active. Immutable unreferenced CAS objects may remain for GC and must not be reported as an imported asset.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::snapshot::importer
cargo test --locked --test quality_faults agent_hub_import -- --nocapture --test-threads=1
```

Expected: importer API/tests are absent.

- [ ] **Step 4: Implement two-phase validation and import**

First validate manifest, revision closure and every object hash into private staging/CAS. Then open one SQLite write transaction, upsert aliases/lineages/assets/revisions/parents/variants/conflicts, compute local/remote maximal common ancestors and update heads or conflicts. Commit import outcome before enqueueing asynchronous reconcile.

- [ ] **Step 5: Keep project mapping explicit**

Use saved `hubProjectId -> local Workbench projectId`; otherwise offer normalized Git remote candidates in preview. A candidate is not a mapping until user confirms, and mapping confirmation does not automatically opt the project into writes.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::snapshot::importer
cargo test --locked agent_hub::revision_graph
cargo test --locked --test quality_faults agent_hub_import -- --nocapture --test-threads=1
```

Expected: divergent replicas converge or expose both heads, never last-write-wins.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/snapshot/importer.rs src-tauri/src/agent_hub/snapshot/mod.rs src-tauri/src/agent_hub/revision_graph.rs src-tauri/src/agent_hub/instructions/reconcile.rs src-tauri/src/storage/agent_hub_repo.rs src-tauri/tests/quality_faults.rs
git commit -m "feat: import agent hub revision snapshots"
```

### Task 4: Add the Three-Phase LAN Push Receiver and Idempotency Ledger

**Files:**
- Create: `src-tauri/src/agent_hub/replication/mod.rs`
- Create: `src-tauri/src/agent_hub/replication/ledger.rs`
- Create: `src-tauri/src/agent_hub/replication/receiver.rs`
- Create: `src-tauri/src/net/routes/agent_hub.rs`
- Modify: `src-tauri/src/net/routes/mod.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Modify: `scripts/check-p2p-route-inventory.mjs`
- Test: same files

**Interfaces:**
- Adds capability `agent-hub.v1` atomically with:

```text
POST /api/agent-hub/push/prepare
PUT  /api/agent-hub/push/:transferId/objects/:objectHash?offset=<u64>
POST /api/agent-hub/push/:transferId/commit
```

- `prepare` JSON body contains envelope plus `{sourceDeviceId,clientRequestId,selectionHash}`；`selectionHash=SHA-256(canonical SnapshotSelection)`。
- object body is `application/octet-stream`, ≤8 MiB, with declared length/chunk SHA-256.
- `commit` repeats source/request/selection/snapshot hashes.

- [ ] **Step 1: Write route/capability inventory tests**

Assert all three routes and `CAPABILITY_AGENT_HUB_V1` are present together. A server missing any route must not advertise the capability. Existing Host/Origin/expected-device/content-type guards remain active; tests must not call them authentication.

- [ ] **Step 2: Write prepare/idempotency tests**

For `(sourceDeviceId,clientRequestId)`:

- same selection+snapshot hash returns the prior prepared/committed outcome;
- different hash returns conflict;
- invalid manifest/limit returns validation without creating an active ledger;
- prepare returns missing revision/object hashes only;
- a verified object from an interrupted transfer is reused.

- [ ] **Step 3: Write chunk and commit tests**

Cover out-of-order offset rejection, overlapping mismatch, chunk >8 MiB, chunk-hash mismatch, final object-hash mismatch, missing parent/object and commit replay. Commit success imports canonical state but may report projection as queued/blocked separately.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked net::routes::agent_hub
cargo test --locked agent_hub::replication
```

Expected: missing routes/modules fail.

- [ ] **Step 5: Implement staging and ledger schema**

Add append-only/idempotent tables for push request, transfer object and final outcome. Staging lives under `<data_dir>/agent-hub/replication/incoming/<transferId>` with private permissions. Stream request bodies to disk, hash incrementally and never buffer a full blob.

- [ ] **Step 6: Implement commit**

Revalidate envelope/hash/selection, require all parents and objects reachable, call `SnapshotImporter::commit_import`, then atomically persist the stable outcome. Enqueue reconcile after DB commit. A maintenance task removes abandoned unverified staging after 24 hours but preserves verified CAS objects.

- [ ] **Step 7: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked net::routes::agent_hub
cargo test --locked agent_hub::replication
node ../scripts/check-p2p-route-inventory.mjs
```

Expected: routes/capability are atomic and interrupted transfers never expose half an import.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent_hub/replication src-tauri/src/net/routes/agent_hub.rs src-tauri/src/net/routes/mod.rs src-tauri/src/net/http_server.rs src-tauri/src/net/protocol.rs src-tauri/src/storage/agent_hub_repo.rs scripts/check-p2p-route-inventory.mjs
git commit -m "feat: receive idempotent agent hub lan pushes"
```

### Task 5: Add Source-Selected Multi-Target LAN Push

**Files:**
- Create: `src-tauri/src/agent_hub/replication/sender.rs`
- Modify: `src-tauri/src/agent_hub/replication/mod.rs`
- Modify: `src-tauri/src/commands/agent_hub.rs`
- Modify: `src-tauri/src/backend/control_agent_hub.rs`
- Modify: `src-tauri/src/attention/agent_hub_source.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub async fn push_selection(
    &self,
    request: PushAgentHubSelectionRequest,
    cancel: &CancellationToken,
) -> Result<MultiTargetPushReport, AppError>;
```

- Request includes explicit peer IDs and exactly one selection mode: full Hub, user scope, project scope or asset IDs.
- Report contains one independent prepared/transferred/committed/failed outcome per target.

- [ ] **Step 1: Write sender negotiation tests**

Fake peers with complete, partial and no `agent-hub.v1` support. Assert the sender:

- never calls push routes before capability check;
- builds the snapshot once per selection and negotiates missing objects per peer;
- streams ≤8 MiB chunks and resumes from peer offset;
- uses one stable clientRequestId per target retry;
- does not roll back successful peers when another fails.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::replication::sender
cargo test --locked commands::agent_hub
```

Expected: sender/commands are absent.

- [ ] **Step 3: Implement the source-side sender**

Use existing peer resolution/client timeouts and bounded parallelism of three target peers. Hash/size metadata may be used for inventory, but every selected object is sent in full when missing. Never expose a target-side “browse source” endpoint.

- [ ] **Step 4: Add durable per-target retry/reporting**

Persist source request/target outcomes so GUI reconnect can read progress. Transport failure is retryable; manifest conflict and unsupported capability are terminal for that target. Attention contains peer label, counts and error code, never payload.

- [ ] **Step 5: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::replication::sender
cargo test --locked commands::agent_hub
cargo test --locked attention::agent_hub_source
```

Expected: per-peer outcomes are stable and no pull-style method exists in owner/control APIs.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent_hub/replication/sender.rs src-tauri/src/agent_hub/replication/mod.rs src-tauri/src/commands/agent_hub.rs src-tauri/src/backend/control_agent_hub.rs src-tauri/src/attention/agent_hub_source.rs
git commit -m "feat: push agent hub selections across lan"
```

### Task 6: Export Only the Local Device Lane Through the Existing Cloud Singleflight

**Files:**
- Create: `src-tauri/src/agent_hub/git/mod.rs`
- Create: `src-tauri/src/agent_hub/git/lane.rs`
- Create: `src-tauri/src/agent_hub/git/runtime.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Modify: `src-tauri/src/cloud_sync/mod.rs`
- Modify: `src-tauri/src/cloud_sync/runtime.rs`
- Modify: `src-tauri/src/cloud_sync/engine.rs`
- Modify: `src-tauri/src/backend/runtime.rs`
- Modify: `src-tauri/src/state.rs`
- Test: same files

**Interfaces:**
- Produces: `AgentHubGitRuntime::{mark_dirty,flush_pending,recover_pending}`.
- Consumes: existing private repo URL/branch/Git credentials and `CloudSyncRuntime` singleflight.
- Writes only `agent-hub/devices/<deviceId>/`.

- [ ] **Step 1: Write lane ownership tests**

Seed a cloud worktree with `devices/device-a`, `devices/device-b` and old prompts/CC/SSH files. Export as device-a and assert byte changes are confined to device-a. Fetching a changed device-b lane must not alter Hub DB or projections.

- [ ] **Step 2: Write scheduling/retry tests with fake time**

Assert 20 canonical changes in 2 seconds create one export. Push failure delays are 1/2/4 seconds for three immediate retries, then pending retry every 5 minutes while backend is online. Unchanged `snapshotHash` creates no commit.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::git
cargo test --locked cloud_sync::runtime
```

Expected: Agent Hub Git lane runtime is absent.

- [ ] **Step 4: Implement isolated lane export**

Within the existing cloud singleflight: prepare/fetch the cloud worktree, expand a fresh full-Hub envelope into a sibling staging directory, atomically replace only the local lane, inspect Git diff limited to that lane, commit and push. Allow rebase/retry of the cloud repository but never import remote Agent Hub lanes.

- [ ] **Step 5: Implement durable pending state**

Persist last exported/pushed snapshot hash, pending hash, attempt count and next attempt time in Agent Hub tables. On owner startup recover pending export. Git failure never blocks same-machine projection.

- [ ] **Step 6: Prove separation from existing automatic cloud import**

Add a regression test showing prompts/CC/SSH keep their current engine behavior while Agent Hub remote lanes are only inventoried. Do not call existing `cloud_sync::snapshot` import code with Agent Hub paths.

- [ ] **Step 7: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::git
cargo test --locked cloud_sync::runtime
cargo test --locked cloud_sync::engine
```

Expected: only the local lane is committed and no remote Agent Hub revision enters SQLite.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent_hub/git src-tauri/src/agent_hub/mod.rs src-tauri/src/cloud_sync src-tauri/src/backend/runtime.rs src-tauri/src/state.rs src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: back up agent hub device snapshots to git"
```

### Task 7: Add Git Inspection, Preview, Project Mapping and Confirmed Import

**Files:**
- Create: `src-tauri/src/agent_hub/git/preview.rs`
- Modify: `src-tauri/src/agent_hub/git/mod.rs`
- Modify: `src-tauri/src/commands/agent_hub.rs`
- Modify: `src-tauri/src/backend/control_agent_hub.rs`
- Modify: `src-tauri/src/agent_hub/project_scope.rs`
- Modify: `web/src/api/agentHub.ts`
- Modify: `web/src/lib/types/agentHub.ts`
- Modify: `web/src/lib/schemas/agentHub.ts`
- Modify: `web/src/pages/AgentHub/useAgentHubController.ts`
- Create: `web/src/pages/AgentHub/LanPushDialog.tsx`
- Create: `web/src/pages/AgentHub/GitImportDrawer.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.module.css`
- Modify: `web/src/i18n/locales/{en,zh}/agentHub.json`
- Test: corresponding `.test.rs`, `.test.ts`, `.test.tsx`

**Interfaces:**
- Produces:

```text
agent_hub.preview_lan_push
agent_hub.start_lan_push
agent_hub.get_lan_push
agent_hub.inspect_git_lanes
agent_hub.preview_git_import
agent_hub.confirm_git_import
agent_hub.confirm_project_mapping
```

- [ ] **Step 1: Write Git preview tests**

Preview one remote lane with added/modified/deleted/conflicting assets, credential-bearing MCP and mapped/unmapped projects. Assert counts and hashes are shown, secrets are represented by a boolean/label only, and preview causes zero Hub revisions/projections.

- [ ] **Step 2: Write confirmation and stale-preview tests**

`confirm_git_import` requires `{laneDeviceId,snapshotHash,selectedAssetIds,projectMappings}` from the preview. If fetch changed the lane hash, return `previewStale`; do not import a newer snapshot under old confirmation.

- [ ] **Step 3: Write frontend tests**

LAN Dialog permits explicit peers + full/user/project/assets selection and reports each peer independently. Git Drawer separates inspect, preview and confirm. Unmapped project requires explicit local mapping and then the existing project opt-in preview before any files are written.

- [ ] **Step 4: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::git::preview
cargo test --locked commands::agent_hub
cd ../web
npm test -- AgentHub LanPushDialog GitImportDrawer
```

Expected: preview/control/UI operations are absent.

- [ ] **Step 5: Implement inspect without import**

Use the fetched cloud worktree read-only, validate each remote `snapshot.json`, compare revision IDs/heads/aliases with the local repo and return DTOs. Corrupt lanes are blocked independently and do not prevent inspecting valid lanes.

- [ ] **Step 6: Implement confirmed import and mapping**

Revalidate exact hash, then call the common `SnapshotImporter`. Save confirmed external project aliases/mappings separately. Canonical import may complete while projection stays unmapped/not-opted-in; report both states.

- [ ] **Step 7: Implement frontend surfaces**

Use existing primitives and controller ownership. LAN/Git operations show plaintext-backup disclosure without printing secret values. No target-side pull action is rendered. Hooks remain before early returns.

- [ ] **Step 8: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::git::preview
cargo test --locked commands::agent_hub
cd ../web
npm test -- AgentHub LanPushDialog GitImportDrawer localeParity
npm run check:css-tokens
npm run check:i18n
npm run build
```

Expected: inspect is side-effect free, stale preview fails closed, confirmed revisions reconcile normally.

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/agent_hub/git/preview.rs src-tauri/src/agent_hub/git/mod.rs src-tauri/src/commands/agent_hub.rs src-tauri/src/backend/control_agent_hub.rs src-tauri/src/agent_hub/project_scope.rs web/src/api/agentHub.ts web/src/lib/types/agentHub.ts web/src/lib/schemas/agentHub.ts web/src/pages/AgentHub web/src/i18n
git commit -m "feat: add confirmed agent hub replication ui"
```

### Task 8: Preserve Mixed-Version Routes and Certify Gate C

**Files:**
- Create: `src-tauri/tests/agent_hub_replication_smoke.rs`
- Modify: `src-tauri/src/sync/mixed_version_harness.rs`
- Modify: `src-tauri/src/cc/mixed_version_harness.rs`
- Modify: `web/tests/agent-hub.spec.ts`
- Modify: `docs/p2p-protocol.md`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

**Interfaces:**
- Produces evidence IDs `L2-AGENT-HUB-C-001`, `L2-AGENT-HUB-C-GIT-001`, `E2E-AGENT-HUB-C-001`.

- [ ] **Step 1: Write the two-owner replication smoke**

Start two isolated backend owners and verify:

1. a common ancestor branches on both devices;
2. source-selected push negotiates/streams/commits;
3. disjoint branches merge and same-block branches create conflict;
4. interruption after several chunks resumes;
5. retrying the same request returns the original outcome;
6. credential bytes are identical in both CAS/envelopes/targets and absent from captured logs;
7. projection failure after commit does not roll back imported canonical state.

- [ ] **Step 2: Write the Git clone/restore smoke**

Export a full device lane, clone it into a third isolated environment, inspect then confirm import, map one project and leave another unmapped. Assert active assets, retained history, variants, tombstones, conflicts and residual-ready fields restore exactly.

- [ ] **Step 3: Preserve N/N+1 behavior**

Mixed-version tests assert:

- v1 Hub peers use only `agent-hub.v1`;
- old peers continue legacy CLAUDE.md/Claude asset routes;
- compatibility payloads produced by the current build contain original values; a placeholder received from an actually old peer is labeled `legacyLossy` and never overwrites a canonical credential;
- legacy result is labeled compatibility and never updates Hub push status;
- new UI contains no old remote inventory/pull control;
- route inventory contains both generations until the N+2 removal gate.

- [ ] **Step 4: Extend E2E**

Cover LAN selection/per-target progress, unsupported peer, Git lane inspection, credential disclosure label, stale preview, confirmed import, project mapping and Attention deep links.

- [ ] **Step 5: Run Gate C full verification**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub::snapshot
cargo test --locked agent_hub::replication
cargo test --locked agent_hub::git
cargo test --locked --test agent_hub_replication_smoke -- --nocapture --test-threads=1
cd ..
node scripts/check-p2p-route-inventory.mjs
cd web
npm run lint
npm run check:css-tokens
npm run check:i18n
npm test -- AgentHub LanPushDialog GitImportDrawer localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts
cd ..
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all pass; actual unexecuted L3 platform rows remain `NOT VERIFIED`.

- [ ] **Step 6: Update persistent truth**

Document `agent-hub.v1` routes, limits, idempotency, source-push semantics, Git device lanes, confirmed import, plaintext credential behavior and N/N+1 fallback. Do not claim LAN identity authentication or automatic Git import.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/tests/agent_hub_replication_smoke.rs src-tauri/src/sync/mixed_version_harness.rs src-tauri/src/cc/mixed_version_harness.rs web/tests/agent-hub.spec.ts docs/p2p-protocol.md docs/prd.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "feat: complete agent hub replication and backup"
```

## Completion Contract

Gate C is complete only when Task 8's full command set passes from a clean committed Gate C branch, the same verified `SnapshotEnvelope v1` preserves lineage/tombstones/variants/conflicts and plaintext payload bytes across manual LAN push and Git device lanes, receiver idempotency and limits survive fault tests, Git never auto-imports a remote lane, unmapped projects never gain guessed paths or opt-in, and N/N+1 mixed-version routes still work. Only exercised L3 combinations may be verified; every other platform remains `NOT VERIFIED`.
