# Agent Hub Gate B — Portable Assets and Physical Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 Skill、Command、Agent、MCP 纳入 Canonical Hub，并通过三套版本化 target adapter 生成不会被 OpenCode 兼容扫描重复发现的唯一物化结果。

**Architecture:** Gate A 的 Revision DAG/CAS 继续保存逻辑资产与原字节；Gate B 在其上增加 typed portable payload、语义配置 patcher、target support manifest 和 managed package activator。Claude/Codex 的受管 Skill 与 Codex Command 进入 cc-partner 生成的隔离 Plugin，OpenCode 使用 `.opencode` 原生路径；旧 `.claude/skills`/`.agents/skills` 只作为待纳管来源。

**Tech Stack:** Rust 2021、serde、serde_json、toml_edit、SQLite/sqlx、tokio process、SHA-256 CAS、React 19、TypeScript、Vitest、Playwright、真实 Claude/Codex/OpenCode CLI 合同测试。

## Global Constraints

- Gate A 全部通过后才开始本计划；不得为资产同步另建第二套 watcher、DAG、CAS 或 projection scheduler。
- 受管 Claude/Codex Skill 终态只存在于各自生成 Plugin；不得投影到 `.claude/skills` 或 `.agents/skills`。
- OpenCode 只向已解析 config root 的 `skills/commands/agents` 或项目 `.opencode/*` 写入；兼容路径仅扫描和迁移。
- MCP 配置按语义 key patch，只改 Hub 拥有的 server；用户未纳管字段、顺序和可保留注释必须继续存在。
- credential-bearing env/header/URL 原字节保存和物化；测试日志、错误、Attention 摘要不得包含 fixture secret。
- `desiredPresence`、`desiredEnabled` 是 target-local 状态；单 target 删除不能隐式生成 canonical tombstone。
- adapter 的 executable realpath、version 或 config root 改变后，旧 support probe 立即失效。
- support manifest 缺少 exact min/current tested version 或 evidence ID 时，写能力必须 `blocked`，不能降级成猜测命令。
- legacy adoption 只有在新投影激活并重新 probe 成功后才原子移走原 source；失败时保留原 source 且不生成第二份可发现副本。
- 文本元数据与配置为 UTF-8；supporting files 按原字节进入 CAS。
- 新增/修改 Rust 与 TypeScript 代码遵守根 `AGENTS.md` 的中文 docstring、strict 类型、token 与 Hooks-before-early-return 合同。

---

## File Structure

- Create: `src-tauri/src/agent_hub/assets/{mod.rs,skill.rs,command.rs,agent.rs,mcp.rs,diagnostics.rs}` — portable canonical payload。
- Create: `src-tauri/src/agent_hub/config_patch/{mod.rs,jsonc.rs,toml.rs}` — ownership-aware JSON/JSONC/TOML patch。
- Create: `src-tauri/src/agent_hub/support/{mod.rs,manifest.rs,support-manifest.json}` — adapter 支持合同。
- Create: `src-tauri/src/agent_hub/packages/{mod.rs,builder.rs,activator.rs,adoption.rs}` — 隔离 package 与 legacy adoption。
- Extend: `src-tauri/src/agent_hub/targets/{mod.rs,claude.rs,codex.rs,opencode.rs}` — scan/render/activate。
- Refactor: `src-tauri/src/claude_code_assets.rs` — N/N+1 兼容 façade。
- Modify: `src-tauri/src/commands/claude_code_assets.rs` — 委托 Hub，保留旧 DTO。
- Create: `src-tauri/tests/agent_hub_cli_contract.rs` — 真实 CLI 合同。
- Create: `scripts/check-agent-hub-support-manifest.mjs` — manifest/evidence 一致性。
- Modify: `.github/workflows/ci.yml` — pinned CLI contract job。
- Extend: `web/src/pages/AgentHub/`, `web/src/api/agentHub.ts`, `web/src/lib/{types,schemas}/agentHub.ts`。
- Test: `web/tests/agent-hub.spec.ts`。

## Task Dependency Graph

```text
B1 -> B2 -> B3 -> B4 -> B5 -> B6 -> B7 -> B8 -> B9
```

- Exact edges are the linear chain shown above because the tasks successively extend shared canonical models, target adapters, package activation, adoption, state transitions and UI DTOs.
- Dependency-ready waves: `[B1]`, `[B2]`, `[B3]`, `[B4]`, `[B5]`, `[B6]`, `[B7]`, `[B8]`, `[B9]`.
- Do not overlap write workers inside Gate B; use a fresh task implementer and the integrated predecessor commit as each task baseline.

### Task 1: Add Typed Canonical Payloads for Skill, Command, Agent and MCP

**Files:**
- Create: `src-tauri/src/agent_hub/assets/mod.rs`
- Create: `src-tauri/src/agent_hub/assets/skill.rs`
- Create: `src-tauri/src/agent_hub/assets/command.rs`
- Create: `src-tauri/src/agent_hub/assets/agent.rs`
- Create: `src-tauri/src/agent_hub/assets/mcp.rs`
- Create: `src-tauri/src/agent_hub/assets/diagnostics.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Modify: `src-tauri/src/agent_hub/models.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct PortableSkill {
    pub name: String,
    pub description: String,
    pub skill_markdown_hash: String,
    pub tree_manifest_hash: String,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

pub struct PortableCommand {
    pub name: String,
    pub description: Option<String>,
    pub prompt_template: String,
    pub arguments: Vec<CommandArgument>,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

pub struct PortableAgent {
    pub name: String,
    pub description: Option<String>,
    pub instructions: String,
    pub mode_intent: Option<String>,
    pub tool_intents: Vec<String>,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}

pub enum McpTransport {
    Stdio { command: String, args: Vec<String>, cwd: Option<String> },
    Http { url: String, headers: BTreeMap<String, String> },
}

pub struct PortableMcpServer {
    pub key: String,
    pub transport: McpTransport,
    pub env: BTreeMap<String, String>,
    pub enabled: bool,
    pub tool_allow: Vec<String>,
    pub tool_deny: Vec<String>,
    pub target_extensions: BTreeMap<AgentTarget, serde_json::Value>,
}
```

- Produces: `PortableAssetPayload` tagged enum and deterministic canonical serialization.
- Consumes: Gate A `LogicalAsset`, `TargetBinding`, CAS blob/tree hashes.

- [ ] **Step 1: Write serialization and validation tests**

Round-trip every payload and assert deterministic `BTreeMap` key order. Add validation cases for empty names, duplicate argument names, invalid MCP transport combinations, absolute-path diagnostics and a Skill tree missing `SKILL.md`.

Use a literal secret fixture:

```rust
let mcp = PortableMcpServer {
    key: "private-api".into(),
    transport: McpTransport::Http {
        url: "https://example.invalid/mcp?token=plain-fixture".into(),
        headers: BTreeMap::from([(
            "Authorization".into(),
            "Bearer plain-fixture".into(),
        )]),
    },
    env: BTreeMap::from([("API_TOKEN".into(), "plain-fixture".into())]),
    enabled: true,
    tool_allow: vec![],
    tool_deny: vec![],
    target_extensions: BTreeMap::new(),
};
```

Assert canonical bytes contain the exact values; route the same error through diagnostic formatting and assert the error text contains neither token nor Authorization value.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::assets
```

Expected: compilation fails because portable payload modules do not exist.

- [ ] **Step 3: Implement typed payloads and diagnostics**

Keep common semantics in typed fields and unrecognized source fields under the source target extension. `PortableSkill` validates the CAS tree without rewriting scripts. `PortabilityDiagnostic` uses stable codes:

```text
absolutePath
targetExecutable
unsupportedInterpolation
modelNotPortable
permissionNotPortable
unknownSourceField
materializedAlias
```

Diagnostics store a JSON pointer/path and hash/length metadata, never a credential value.

- [ ] **Step 4: Persist payload revisions**

Add `AgentHubRepo::{append_portable_asset_revision,load_portable_asset}`. The revision payload blob contains only the typed canonical JSON; Skill supporting files remain in `tree_manifest_hash`. Reject an `AssetKind`/payload-tag mismatch before starting the SQL transaction.

- [ ] **Step 5: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::assets
cargo test --locked storage::agent_hub_repo
```

Expected: all pass; binary supporting files round-trip unchanged and diagnostic strings stay redacted.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent_hub/assets src-tauri/src/agent_hub/mod.rs src-tauri/src/agent_hub/models.rs src-tauri/src/storage/agent_hub_repo.rs
git commit -m "feat: add portable agent hub asset schemas"
```

### Task 2: Implement Ownership-Aware TOML and JSON/JSONC Patchers

**Files:**
- Create: `src-tauri/src/agent_hub/config_patch/mod.rs`
- Create: `src-tauri/src/agent_hub/config_patch/jsonc.rs`
- Create: `src-tauri/src/agent_hub/config_patch/toml.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct ManagedConfigPatch {
    pub owner_id: String,
    pub path: Vec<String>,
    pub value: Option<serde_json::Value>,
    pub expected_base_hash: Option<String>,
}

pub trait SemanticConfigPatcher {
    fn inspect(&self, bytes: &[u8], path: &[String]) -> Result<OwnedConfigValue, AppError>;
    fn apply(
        &self,
        bytes: &[u8],
        patches: &[ManagedConfigPatch],
    ) -> Result<PatchedConfig, AppError>;
}
```

- Consumes: Gate A atomic writer and materialization precondition.
- Produces: new bytes, owned-path hashes and exact preview diff.

- [ ] **Step 1: Write preservation fixtures**

For TOML, seed comments, custom `model`, unrelated `mcp_servers.user-owned`, array order and whitespace. Patch only `mcp_servers.cc_partner_x` and `agents.cc_partner_y`. Assert `toml_edit` preserves every unrelated span.

For JSONC, seed line/block comments, trailing commas, CRLF and unrelated plugin config. Patch only the owned MCP member and assert:

```rust
assert_eq!(strip_owned_span(&after), strip_owned_span(&before));
assert!(String::from_utf8(after)?.contains("// keep this comment"));
```

Add conflict tests where the owned value changed since `expected_base_hash`; result must be `ConfigPatchOutcome::Conflict`, not overwrite.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::config_patch
```

Expected: missing patcher modules fail.

- [ ] **Step 3: Implement TOML patching with `toml_edit`**

Navigate semantic keys without reserializing the full document. Reject an owned path whose parent changed type. Removal deletes only the owned key and preserves the enclosing user table unless Hub created it and it is empty.

- [ ] **Step 4: Implement the local JSONC span patcher**

Build a small tokenizer for strings, escapes, punctuation, whitespace and comments; parse object member spans and replace only the requested leaf span. Do not add an unpinned JSONC formatter dependency. New members inherit indentation/newline style from the nearest sibling; invalid JSONC is `blocked` and original bytes remain untouched.

- [ ] **Step 5: Integrate with projection jobs**

Store `{ownerId,path,baseValueHash}` in materialization metadata. Projection renders from the current external config at execution time, applies semantic patches, then uses Gate A’s file hash precondition and atomic writer. A config file is never treated as a whole-file Hub-owned artifact.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::config_patch
cargo test --locked agent_hub::projection
```

Expected: comments/unmanaged keys survive round-trip and same-key concurrent edits form a visible conflict.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/config_patch src-tauri/src/agent_hub/mod.rs src-tauri/src/agent_hub/projection
git commit -m "feat: patch managed cli configuration semantically"
```

### Task 3: Add Three Portable-Asset Scanners and Preserve the Claude Compatibility Façade

**Files:**
- Modify: `src-tauri/src/agent_hub/targets/mod.rs`
- Modify: `src-tauri/src/agent_hub/targets/claude.rs`
- Modify: `src-tauri/src/agent_hub/targets/codex.rs`
- Modify: `src-tauri/src/agent_hub/targets/opencode.rs`
- Refactor: `src-tauri/src/claude_code_assets.rs`
- Modify: `src-tauri/src/commands/claude_code_assets.rs`
- Test: same files

**Interfaces:**
- Extends `AssetAdapter`:

```rust
fn scan_portable_assets(
    &self,
    scope: &LocalScopeMapping,
    env: &TargetEnvironment,
) -> Result<Vec<DiscoveredPortableAsset>, AppError>;

fn render_portable_asset(
    &self,
    asset: &PortableAssetPayload,
    context: &AssetRenderContext,
) -> Result<TargetAssetProjection, AppError>;
```

- Produces origin records with exact source path, target, native ID, content/tree hash, active/discovery status and unknown-file diagnostics.

- [ ] **Step 1: Write scanner fixtures**

Use isolated homes containing:

```text
.claude/skills/review/SKILL.md
.claude/commands/release.md
.claude/agents/reviewer.md
.agents/skills/review/SKILL.md
.codex/config.toml
.opencode/skills/review/SKILL.md
.opencode/commands/release.md
.opencode/agents/reviewer.md
opencode.jsonc
```

Assert OpenCode reports `.claude/skills` and `.agents/skills` as compatibility origins, not native output candidates. Two origins with the same semantic name remain separate discoveries until adoption resolves hashes/ownership.

- [ ] **Step 2: Characterize existing Claude behavior and pin the no-redaction change**

Before refactoring, pin the old `list_assets`, enable/disable and DTO sorting behavior in tests. Add a failing regression asserting the legacy P2P bundle produced by this version contains exact MCP env/header/URL values and never emits `__REDACTED_BY_CLAUDE_PARTNER__`. Keep a separate decode fixture for an old peer that already sent the placeholder; it must be reported as `legacyLossy` and cannot overwrite a real canonical credential.

- [ ] **Step 3: Run RED for new adapters**

```bash
cd src-tauri
cargo test --locked agent_hub::targets
cargo test --locked claude_code_assets
```

Expected: new portable scanner assertions fail while old characterization stays green.

- [ ] **Step 4: Implement source-specific parsing**

- Claude: parse native Skill/Command/Agent Markdown and user/project MCP JSON through the Claude adapter.
- Codex: parse Plugin-provided Skills, agent config references and managed MCP TOML; scan `.agents/skills` only as legacy standalone origins.
- OpenCode: parse native `.opencode`/config-root Skills, Commands, Agents and MCP JSON/JSONC; mark `.claude/skills`/`.agents/skills` as compatibility origins.

Unknown frontmatter/config fields go to `target_extensions[source]`; they are never silently discarded.

- [ ] **Step 5: Turn `claude_code_assets.rs` into an N/N+1 façade**

Move generic scanning/normalization into the Claude adapter. Existing functions translate Hub discoveries/materializations back to `ClaudeCodeAsset` DTOs. Existing mutation commands delegate to `AgentHubService` when Gate B is enabled and use the legacy path only while compatibility mode is active.

Delete the legacy export redaction transform: compatibility routes produced by the new build preserve original MCP values. Retain placeholder detection only on import from old peers; it yields `legacyLossy`/blocked rather than writing the placeholder as a credential. Diagnostic logs remain value-redacted.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::targets
cargo test --locked claude_code_assets
cargo test --locked commands::claude_code_assets
```

Expected: all sources are inventoried once, legacy DTOs remain stable and no scanner writes files.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/targets src-tauri/src/claude_code_assets.rs src-tauri/src/commands/claude_code_assets.rs
git commit -m "refactor: route cli assets through target adapters"
```

### Task 4: Enforce the Versioned Adapter Support Manifest

**Files:**
- Create: `src-tauri/src/agent_hub/support/mod.rs`
- Create: `src-tauri/src/agent_hub/support/manifest.rs`
- Create: `src-tauri/src/agent_hub/support/support-manifest.json`
- Create: `scripts/check-agent-hub-support-manifest.mjs`
- Create: `src-tauri/tests/agent_hub_cli_contract.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `src-tauri/src/agent_hub/mod.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub struct TargetSupportRecord {
    pub target: AgentTarget,
    pub adapter_schema_version: u32,
    pub min_tested_version: Option<String>,
    pub current_tested_version: Option<String>,
    pub executable_probe: ExecutableProbeSpec,
    pub capabilities: BTreeMap<TargetCapability, CapabilitySupport>,
    pub evidence_ids: Vec<String>,
}

pub enum CapabilitySupport {
    Blocked,
    ReadOnly,
    Supported,
    SupportedAfterRestart,
    ActivationRequired,
}
```

The JSON is compiled with `include_str!("support-manifest.json")`; runtime writes cannot alter it.

- [ ] **Step 1: Write manifest fail-closed tests**

Assert missing/null min/current version, empty evidence, unknown capability, malformed semver and executable/config-root fingerprint mismatch all return scan-only support. A manifest may deliberately contain null versions during development, but `scripts/check-agent-hub-support-manifest.mjs --gate-b` must reject that state.

- [ ] **Step 2: Write the real CLI contract harness**

The ignored L3 test uses isolated `HOME`, `CODEX_HOME`, `OPENCODE_CONFIG_DIR` and `OPENCODE_CONFIG`, resolves each executable realpath, records `--version`, then validates:

- instruction and Skill discovery;
- Command/Agent/MCP scan and render;
- activation/list/remove or the exact declared `activationRequired`;
- whether changes are live, new-session or restart-only;
- no network credential is required for local fixtures.

Run each target independently:

```bash
cd src-tauri
CC_PARTNER_L3_TARGET=claude cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
CC_PARTNER_L3_TARGET=codex cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
CC_PARTNER_L3_TARGET=opencode cargo test --locked --test agent_hub_cli_contract -- --ignored --nocapture --test-threads=1
```

The test compares the actual normalized version with `currentTestedVersion`; mismatch fails and prints only the version/fingerprint, never asset content.

- [ ] **Step 3: Implement manifest parsing and support evaluation**

Use an exact major/minor/patch parser that tolerates a target’s documented prefix/suffix. Below minimum, unknown breaking major, unexpected help fingerprint or version above a declared guarded major is read-only blocked. Every target capability is evaluated separately.

- [ ] **Step 4: Add the static manifest checker**

`scripts/check-agent-hub-support-manifest.mjs --gate-b` verifies:

1. all three targets exist exactly once;
2. min/current versions are exact and ordered;
3. every `Supported*` capability has a stable evidence ID present in `quality-matrix.json`;
4. generated activation command fingerprints match checked-in L3 snapshots;
5. no credential value or absolute developer home path appears in the manifest.

- [ ] **Step 5: Add pinned minimum/current CI jobs**

The CI job reads exact versions from the manifest, installs those versions in an isolated runner, and runs the ignored contract test once for minimum and once for current. A target without a reproducible pinned installer remains `Blocked` and Gate B cannot be certified as supporting writes for that target.

- [ ] **Step 6: Populate only evidence-backed records**

After the L3 commands pass, write the exact observed versions and evidence IDs into the manifest. The repository state at Gate B completion must contain no target whose supported write capability relies on a local version not exercised by CI.

- [ ] **Step 7: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::support
cargo test --locked --test agent_hub_cli_contract
cd ..
node scripts/check-agent-hub-support-manifest.mjs --gate-b
```

Expected: unit tests pass, non-ignored contract harness compiles, and the checker accepts exact support evidence.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/agent_hub/support src-tauri/src/agent_hub/mod.rs src-tauri/tests/agent_hub_cli_contract.rs scripts/check-agent-hub-support-manifest.mjs .github/workflows/ci.yml docs/development/quality-matrix.json
git commit -m "feat: enforce agent adapter support contracts"
```

### Task 5: Build Isolated Managed Packages and Target Activators

**Files:**
- Create: `src-tauri/src/agent_hub/packages/mod.rs`
- Create: `src-tauri/src/agent_hub/packages/builder.rs`
- Create: `src-tauri/src/agent_hub/packages/activator.rs`
- Modify: `src-tauri/src/agent_hub/targets/claude.rs`
- Modify: `src-tauri/src/agent_hub/targets/codex.rs`
- Modify: `src-tauri/src/agent_hub/targets/opencode.rs`
- Modify: `src-tauri/src/agent_hub/projection/scheduler.rs`
- Test: same files

**Interfaces:**
- Produces:

```rust
pub trait ManagedPackageActivator: Send + Sync {
    fn build_plan(
        &self,
        package: &GeneratedTargetPackage,
        binding: &TargetBinding,
        probe: &TargetProbe,
    ) -> Result<ActivationPlan, AppError>;

    async fn apply(
        &self,
        plan: &ActivationPlan,
        cancel: &CancellationToken,
    ) -> Result<ActivationResult, AppError>;

    async fn inspect(
        &self,
        plan: &ActivationPlan,
    ) -> Result<ActivationInspection, AppError>;
}
```

- Uses materialized root:

```text
<data_dir>/agent-hub/materialized-packages/
  claude/<scope-id>/<package-id>/
  codex/<scope-id>/<package-id>/
  opencode/<scope-id>/<package-id>/
```

- [ ] **Step 1: Write package layout tests**

Build one shared Skill and one targetOnly Skill. Assert:

- Claude generated Plugin has a valid manifest and only Claude-visible content;
- Codex generated Plugin has `.codex-plugin/plugin.json` and only Codex-visible content;
- OpenCode output uses native `skills/commands/agents`;
- generated aliases are stable across rebuilds;
- no managed output path is under `.claude/skills` or `.agents/skills`.

- [ ] **Step 2: Write activator argv tests**

Use fake process runners. Claude must add the generated marketplace, install/enable the exact `plugin@cc-partner` selector at the binding scope, then inspect `plugin list`. Codex must use the probed stable `plugin marketplace add`, `plugin add` and `plugin remove` argv surface and inspect JSON output. No command is built when support manifest capability is blocked.

For Codex, `desiredEnabled=false` is implemented as remove-with-binding-retained and re-enable as add; the canonical asset and `desiredPresence=present` remain unchanged.

- [ ] **Step 3: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::packages
```

Expected: package/activation modules are absent.

- [ ] **Step 4: Implement deterministic package building**

Package IDs derive from `{target,scopeId,logical asset IDs}` and do not contain secrets. Build into a sibling staging directory, validate target manifests, hash the tree, then atomically replace the inactive materialized package. Invocation alias and namespace are recorded in materialization metadata and shown in previews.

- [ ] **Step 5: Implement activation as a durable projection phase**

Projection state order is:

```text
prepared -> packageWritten -> activationRequested -> activationVerified -> committed
```

On recovery, inspect actual CLI state before repeating a command. `ActivationRequired` and `Unsupported` never become committed/full. OpenCode activation is an atomic native-path projection followed by scanner verification.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::packages
cargo test --locked agent_hub::projection
```

Expected: duplicate recovery is idempotent, failed activation leaves the previous discoverable package intact, and all targetOnly fixtures stay isolated.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/packages src-tauri/src/agent_hub/targets src-tauri/src/agent_hub/projection
git commit -m "feat: materialize isolated managed cli packages"
```

### Task 6: Adopt Legacy Standalone Sources Without Duplicate Discovery

**Files:**
- Create: `src-tauri/src/agent_hub/packages/adoption.rs`
- Modify: `src-tauri/src/agent_hub/packages/mod.rs`
- Modify: `src-tauri/src/agent_hub/runtime.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Test: same files
- Test: `src-tauri/tests/quality_faults.rs`

**Interfaces:**
- Produces:

```rust
pub enum AdoptionOutcome {
    Adopted { archive_tree_hash: String, materialization_id: String },
    ExternalCollision { diagnostics: Vec<PortabilityDiagnostic> },
    Blocked { reason: String },
}
```

- Consumes: scanner origin hash/tree, package activator, CAS, atomic directory writer.

- [ ] **Step 1: Write success and collision tests**

Cover:

1. legacy `.claude/skills/review` imports bytes, activates generated Claude Plugin, archives CAS tree, then removes source;
2. legacy `.agents/skills/review` does the same for Codex;
3. OpenCode sees exactly one shared Skill after both adoptions;
4. unknown extra file, source hash drift, failed CLI activation, non-empty destination collision and crash before archive commit all preserve the legacy directory and do not expose a second copy.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::packages::adoption
cargo test --locked --test quality_faults agent_hub_adoption -- --nocapture --test-threads=1
```

Expected: adoption API/test is absent.

- [ ] **Step 3: Implement preview-first adoption**

Preview lists origin path, hash, canonical name, generated target package/invocation, unknown files and exact removal operation. User-level adoption requires explicit confirmation; project adoption is covered by the existing project opt-in confirmation.

- [ ] **Step 4: Implement the activation-before-removal transaction**

Persist prepared adoption, write/activate package, re-scan actual CLI discovery, put the original tree in CAS, then atomically rename the legacy directory into a private Hub staging archive and commit DB state. Delete staging only after DB commit. Recovery uses hashes to finish or restore the source.

- [ ] **Step 5: Handle new post-opt-in legacy sources**

Watcher detects new compatibility-path sources and schedules the same adoption state machine. Until success, mark `externalCollision`, block the generated duplicate and emit Attention; never auto-delete an unrecognized tree.

- [ ] **Step 6: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::packages::adoption
cargo test --locked --test quality_faults agent_hub_adoption -- --nocapture --test-threads=1
```

Expected: every fault point leaves one discoverable copy and one recoverable canonical/archive path.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/agent_hub/packages/adoption.rs src-tauri/src/agent_hub/packages/mod.rs src-tauri/src/agent_hub/runtime.rs src-tauri/src/storage/agent_hub_repo.rs src-tauri/tests/quality_faults.rs
git commit -m "feat: adopt legacy cli assets without duplicates"
```

### Task 7: Implement Target Presence, Enable, Detach and Delete Semantics

**Files:**
- Modify: `src-tauri/src/agent_hub/models.rs`
- Modify: `src-tauri/src/agent_hub/projection/scheduler.rs`
- Modify: `src-tauri/src/agent_hub/packages/activator.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`
- Modify: `src-tauri/src/commands/agent_hub.rs`
- Modify: `src-tauri/src/backend/control_agent_hub.rs`
- Test: same files

**Interfaces:**
- Produces command operations:

```text
agent_hub.set_target_presence
agent_hub.set_target_enabled
agent_hub.restore_detached_target
agent_hub.delete_asset_everywhere
```

- [ ] **Step 1: Write state-transition tests**

Assert:

- disable one target leaves other bindings and canonical revision untouched;
- `desiredPresence=absent` removes only that target’s owned paths/package;
- external whole-file/directory delete becomes `detached` with no automatic recreation;
- `restore_detached_target` schedules projection;
- `delete_asset_everywhere` appends one canonical tombstone and fans out removals;
- targetOnly last-target delete asks for the explicit everywhere action and never guesses.

- [ ] **Step 2: Run RED**

```bash
cd src-tauri
cargo test --locked agent_hub::projection
cargo test --locked commands::agent_hub
```

Expected: transition assertions fail.

- [ ] **Step 3: Implement an explicit transition table**

Centralize allowed transitions in `TargetBinding::apply_intent`. `desiredEnabled=false` uses the adapter’s declared disable strategy; absence removes only owned materialization. Unknown files or changed owned paths block removal and return exact preview.

- [ ] **Step 4: Compute aggregate status**

`full` requires every requested target component supported, present, enabled as desired and verified. Any unsupported mapping yields `partial`, `sourceOnly`, `activationRequired`, `externalCollision`, `detached` or `blocked`; the UI/API cannot infer full from a successful package write alone.

- [ ] **Step 5: Run GREEN**

```bash
cd src-tauri
cargo fmt --check
cargo test --locked agent_hub::projection
cargo test --locked commands::agent_hub
cargo test --locked backend::control_agent_hub
```

Expected: all transitions are deterministic and mutation DTO parity remains intact.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/agent_hub/models.rs src-tauri/src/agent_hub/projection/scheduler.rs src-tauri/src/agent_hub/packages/activator.rs src-tauri/src/storage/agent_hub_repo.rs src-tauri/src/commands/agent_hub.rs src-tauri/src/backend/control_agent_hub.rs
git commit -m "feat: enforce target-local agent asset state"
```

### Task 8: Expand the Unified Target Matrix and Migrate Claude Asset Actions

**Files:**
- Modify: `web/src/api/agentHub.ts`
- Modify: `web/src/lib/types/agentHub.ts`
- Modify: `web/src/lib/schemas/agentHub.ts`
- Modify: `web/src/pages/AgentHub/useAgentHubController.ts`
- Modify: `web/src/pages/AgentHub/AgentHub.tsx`
- Create: `web/src/pages/AgentHub/AssetAdoptionDialog.tsx`
- Create: `web/src/pages/AgentHub/TargetStatusCell.tsx`
- Modify: `web/src/pages/AgentHub/AgentHub.module.css`
- Modify: `web/src/components/domain/AgentAssetRow/AgentAssetRow.tsx`
- Modify: `web/src/i18n/locales/{en,zh}/agentHub.json`
- Modify: `web/src/App.tsx`
- Test: corresponding `.test.ts` and `.test.tsx`
- Test: `web/tests/agent-hub.spec.ts`

**Interfaces:**
- Consumes: Gate B asset DTO, adoption preview, materialized invocation and target transition commands.
- Produces: one scope/kind inventory with three target cells and explicit actions.

- [ ] **Step 1: Write decoder and status-matrix tests**

Decode every Gate B status and reject unknown required fields. Table-driven UI tests assert:

```text
full                -> verified invocation
partial             -> missing/unequal components listed
sourceOnly          -> source target shown, no install action elsewhere
activationRequired  -> manual activation instructions
externalCollision   -> adoption/collision preview
detached            -> restore/remove/everywhere choices
blocked             -> support/evidence reason
```

- [ ] **Step 2: Write interaction tests**

Cover adoption confirmation, enable/disable one target, target removal, everywhere delete confirmation, collision navigation and rapid scope switching. All mutations refresh by revision cursor; stale responses cannot replace the active scope.

- [ ] **Step 3: Run RED**

```bash
cd web
npm test -- AgentHub agentHub AgentAssetRow
```

Expected: missing statuses/actions fail.

- [ ] **Step 4: Implement the target matrix**

Keep data access in `useAgentHubController`; pure views do not import `@/api/*`. Reuse `Dialog`, `Drawer`, `Card`, `Pill`, `Button`, `StatusMessage`. Show canonical name separately from materialized alias/invocation.

- [ ] **Step 5: Retire Claude-only navigation ownership**

Old `/claude-code` remains a redirect for N/N+1. Existing old API DTO continues to work, but the UI no longer exposes legacy LAN pull or directly moves Skill directories. Add copy explaining that LAN push arrives in Gate C without claiming it already ships.

- [ ] **Step 6: Run GREEN**

```bash
cd web
npm test -- AgentHub agentHub AgentAssetRow localeParity
npm run check:css-tokens
npm run check:i18n
npm run build
```

Expected: all pass; both languages cover every new status and all Hooks precede early returns.

- [ ] **Step 7: Commit**

```bash
git add web/src/api/agentHub.ts web/src/lib/types/agentHub.ts web/src/lib/schemas/agentHub.ts web/src/pages/AgentHub web/src/components/domain/AgentAssetRow web/src/i18n web/src/App.tsx
git commit -m "feat: add portable asset target matrix"
```

### Task 9: Certify Gate B with Real Discovery, Round-Trip and Isolation Evidence

**Files:**
- Create: `src-tauri/tests/agent_hub_gate_b_smoke.rs`
- Modify: `src-tauri/tests/agent_hub_cli_contract.rs`
- Modify: `web/tests/agent-hub.spec.ts`
- Modify: `docs/prd.md`
- Modify: `docs/development/testing.md`
- Modify: `docs/development/quality-matrix.json`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`

**Interfaces:**
- Produces evidence IDs `L2-AGENT-HUB-B-001`, `L3-AGENT-HUB-B-CLI-001`, `E2E-AGENT-HUB-B-001`.

- [ ] **Step 1: Write the process smoke**

With isolated HOME/data/config roots and fake activators, import shared Skill/Command/Agent/MCP plus Claude/Codex targetOnly Skills. Verify:

1. each target’s scanner reports one shared Skill;
2. OpenCode never reports the Claude/Codex targetOnly Skill;
3. unmanaged TOML/JSONC config survives enable/disable/update/remove;
4. legacy adoption crash recovers to exactly one discoverable source;
5. credential bytes match canonical and all target configs;
6. captured logs do not contain the credential fixture.

- [ ] **Step 2: Extend E2E**

Add scope/kind filters, target status cells, alias display, adoption preview, collision recovery and target/everywhere delete actions. Keep Gate A project/instruction cases.

- [ ] **Step 3: Run focused verification**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked agent_hub::assets
cargo test --locked agent_hub::targets
cargo test --locked agent_hub::packages
cargo test --locked --test agent_hub_gate_b_smoke -- --nocapture --test-threads=1
cd ../web
npm run lint
npm run check:css-tokens
npm run check:i18n
npm test -- AgentHub agentHub AgentAssetRow localeParity
npm run build
npm run test:e2e -- agent-hub.spec.ts
cd ..
node scripts/check-agent-hub-support-manifest.mjs --gate-b
node scripts/check-quality-traceability.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all pass; L3 rows only become verified for exact versions actually exercised, and unexecuted platforms remain `NOT VERIFIED`.

- [ ] **Step 4: Update persistent truth**

Document shipped Gate B behavior, managed physical paths, support matrix and legacy adoption/rollback. Keep Snapshot/LAN/Git/Plugin decomposition/OpenCode runtime marked as later gates.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/agent_hub_gate_b_smoke.rs src-tauri/tests/agent_hub_cli_contract.rs web/tests/agent-hub.spec.ts docs/prd.md docs/development/testing.md docs/development/quality-matrix.json src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "feat: complete portable agent asset support"
```

## Completion Contract

Gate B is complete only when Task 9's full command set passes from a clean committed Gate B branch, shared assets are discovered exactly once per target, target-only assets never leak to other CLIs, unmanaged TOML/JSONC survives round trips, adoption/crash recovery leaves one discoverable source, target-local enable/detach/delete semantics hold, credential bytes remain exact, and logs contain no credential fixture. Support evidence applies only to exact tested CLI versions; every unexecuted platform remains `NOT VERIFIED`.
