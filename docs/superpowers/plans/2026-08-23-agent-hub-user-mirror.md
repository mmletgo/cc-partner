# Agent Hub 用户级镜像 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Agent Hub 用户级 Pull/Push 从逐项复制改成一次镜像全部已登记 Agent 的三槽、原生提示词文件和 Skill/Command/Plugin/MCP，目标多余删除/停用并立刻写盘。

**Architecture:** 新增领域模块 `agent_hub/user_mirror`：源端冻结全 Agent 用户级 inventory + CAS selection；目标端 preview diff 后 apply（导入 Hub → 写白名单原生文件 → portable upsert → 删除多余 Skill/Command → Disable 多余 Plugin → 删多余 MCP → rescan）。Pull 的 apply 只走本机 owner/control；Push 走新的 `user-mirror` prepare/objects/commit（commit 必须含写盘）。新能力 `agent-hub.user-mirror.v1` 与路由同发；`AGENT_HUB_API_VERSION=5`。

**Tech Stack:** Rust 2021、serde、sqlx/SQLite、axum、reqwest、现有 ObjectStore/SnapshotEnvelope/portable_actions/user_instructions writers；React 19 + CSS Modules + Vitest/Playwright。

**Spec:** `docs/superpowers/specs/2026-08-23-agent-hub-user-mirror-design.md`

---

## Global Constraints

- 权威合同是上述 Spec。与 2026-08-10「勾选 Pull、原生安装 scan-only」冲突时以本 Spec 为准。
- 范围仅用户级；`all_hub_targets()` 当前为 claude/codex/opencode/grok/gemini/cursor/pi。同名对号入座，禁止跨 Agent 翻译。
- 原生路径只允许现有白名单：`user_instructions/native_files.rs` 的 `declared_native_paths` + 各 `targets/*.rs` 已物化的 adapted/exclusive 文件。禁止写仓库根 `AGENTS.md`、禁止把 OpenCode fallback 写成 Claude 的 `CLAUDE.md`。
- Inventory/UI/log 永不包含 MCP secret 或绝对路径；凭据只在 CAS 对象里。
- 不 spawn 未认证 CLI。Plugin 目标多余只 Disable viewing 标记。Skill 多余 detach，不删 `~/.agents` 源树、不 `destroyStore` 仍被其他 Agent 使用的包。
- 缺 `agent-hub.user-mirror.v1` 整次失败，零请求打到旧 `portable-pull` / `agent-hub.v1` push。
- `expected-device` / `clientRequestId` 不是身份认证。LAN 风险文案必须保留。
- 部分成功不回滚。Plan TTL 15 分钟。CAS chunk ≤ 8 MiB，累计 ≤ 512 MiB。
- 业务函数中文 Business Logic / Code Logic docstring；UTF-8；hooks 在 early return 前。
- 验证用 `./scripts/cc-partner-cargo.sh test --locked …`，禁止 `CARGO_TARGET_DIR=/tmp/...`。

---

## File Structure

| 文件 | 职责 |
|------|------|
| `src-tauri/src/agent_hub/user_mirror/mod.rs` | 模块出口 |
| `src-tauri/src/agent_hub/user_mirror/models.rs` | DTO、logical file id、错误 code 常量 |
| `src-tauri/src/agent_hub/user_mirror/inventory.rs` | 本机/源端全 Agent 用户级 metadata inventory |
| `src-tauri/src/agent_hub/user_mirror/selection.rs` | 冻结 SnapshotEnvelope + CAS 对象 |
| `src-tauri/src/agent_hub/user_mirror/preview.rs` | 源 vs 目标 diff → plan |
| `src-tauri/src/agent_hub/user_mirror/apply.rs` | dest apply：写盘 + extras + rescan |
| `src-tauri/src/agent_hub/user_mirror/ledger.rs` | SQLite plan/result 幂等 |
| `src-tauri/src/agent_hub/user_mirror/service.rs` | `preview_user_mirror` / `apply_user_mirror` / `get_user_mirror` |
| `src-tauri/src/agent_hub/user_mirror/push.rs` | 源侧 multi-peer push（新路由） |
| `src-tauri/src/net/protocol.rs` | `CAPABILITY_USER_MIRROR_V1` |
| `src-tauri/src/backend/control.rs` | `AGENT_HUB_API_VERSION = 5` |
| `src-tauri/src/net/routes/agent_hub.rs` + `http_server.rs` | 六条 LAN 路由 |
| `src-tauri/src/commands/agent_hub.rs` + `control_agent_hub.rs` + `control_client.rs` + `lib.rs` | Tauri/control |
| `src-tauri/src/storage/agent_hub_repo.rs` | `agent_hub_user_mirror_plans` 表 |
| `src-tauri/tests/agent_hub_user_mirror_smoke.rs` | L2-AGENT-HUB-USER-MIRROR-001/002/003 |
| `web/src/lib/types/userMirror.ts` + `web/src/lib/schemas/userMirror.ts` + `web/src/api/userMirror.ts` | 前端合同 |
| `web/src/pages/AgentHub/userMirror/*` | Pull/Push 对话框与 controller |
| `web/tests/agent-hub-user-mirror.spec.ts` | E2E-AGENT-HUB-USER-MIRROR-001 |
| `docs/p2p-protocol.md` `docs/prd.md` `docs/development/quality-matrix.json` `docs/development/testing.md` `docs/development/adapt-new-agent.md` 分层 `AGENTS.md` | 文档 |

生产 UI 停止调用 `agent_hub_preview_portable_pull` / `apply_portable_pull` / `push_selection`。旧路由保留 N/N+1。

## Task Graph

```
T1 -> T2 -> T3 -> T4 -> T5 -> T6 -> T7 -> T8 -> T9 -> T10 -> T11
                                                          |
                                                          +-> T12 -> T13 -> T14
                                                          +-> T15
T11 完成前不得宣称后端完成。T14 完成前不得宣称 UI 完成。T15 与 T11 可在 T10 后并行，但 capability 行必须在路由落地后写入 p2p-protocol。
```

---

### Task 1: 能力 token、API v5、DTO

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/mod.rs`
- Create: `src-tauri/src/agent_hub/user_mirror/models.rs`
- Modify: `src-tauri/src/agent_hub/mod.rs`（`pub mod user_mirror;`）
- Modify: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/backend/control.rs`（`AGENT_HUB_API_VERSION: u32 = 5`）
- Modify: `src-tauri/AGENTS.md` 中 `AGENT_HUB_API_VERSION=4` → `=5` 并加半句「v5 = 用户级镜像写盘」

- [ ] **Step 1: 写失败测试（capability 宣告 + 版本）**

在 `src-tauri/src/net/protocol.rs` 的 tests 模块追加：

```rust
#[test]
fn server_protocol_info_declares_user_mirror_v1_in_dictionary_order() {
    let info = server_protocol_info();
    assert!(info.supports(CAPABILITY_USER_MIRROR_V1));
    let caps = info.capabilities;
    let ui = caps.iter().position(|c| c == CAPABILITY_USER_INSTRUCTIONS_V1);
    let um = caps.iter().position(|c| c == CAPABILITY_USER_MIRROR_V1);
    let v1 = caps.iter().position(|c| c == CAPABILITY_AGENT_HUB_V1);
    assert!(ui < um && um < v1, "dictionary order: user-instructions < user-mirror < v1");
}

#[test]
fn user_mirror_capability_token_is_stable() {
    assert_eq!(CAPABILITY_USER_MIRROR_V1, "agent-hub.user-mirror.v1");
}
```

在 `src-tauri/src/backend/control.rs` tests 把所有 `assert_eq!(…, 4)` 对 `AGENT_HUB_API_VERSION` 改为 `5`（先跑现有测试确认失败）。

在 `models.rs` 的 `#[cfg(test)]`：

```rust
#[test]
fn user_mirror_error_codes_are_stable() {
    assert_eq!(USER_MIRROR_CAPABILITY_UNSUPPORTED, "USER_MIRROR_CAPABILITY_UNSUPPORTED");
    assert_eq!(USER_MIRROR_STALE, "USER_MIRROR_STALE");
    assert_eq!(USER_MIRROR_PREVIEW_REQUIRED, "USER_MIRROR_PREVIEW_REQUIRED");
    assert_eq!(USER_MIRROR_TRANSFER_LIMIT, "USER_MIRROR_TRANSFER_LIMIT");
    assert_eq!(USER_MIRROR_NATIVE_PATH_FORBIDDEN, "USER_MIRROR_NATIVE_PATH_FORBIDDEN");
    assert_eq!(USER_MIRROR_LEGACY_LOSSY_BLOCKED, "USER_MIRROR_LEGACY_LOSSY_BLOCKED");
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
./scripts/cc-partner-cargo.sh test --locked --lib net::protocol::tests::server_protocol_info_declares_user_mirror_v1_in_dictionary_order -- --nocapture --test-threads=1
```

Expected: 编译失败（`CAPABILITY_USER_MIRROR_V1` 不存在）。

- [ ] **Step 3: 最小实现**

`protocol.rs` 在 `CAPABILITY_USER_INSTRUCTIONS_V1` 后增加：

```rust
/// 能力 token：用户级全 Agent 镜像 Pull/Push。
///
/// Business Logic: 客户端在 inventory/selection/objects 或 dest commit 前必须确认对端
///     已挂载镜像路由；缺失时整次失败，禁止回落逐项 portable-pull / agent-hub.v1 push。
///     expected-device / clientRequestId 不是身份认证。
/// Code Logic: `agent-hub.user-mirror.v1`，与六条 user-mirror 路由同 build 宣告。
pub const CAPABILITY_USER_MIRROR_V1: &str = "agent-hub.user-mirror.v1";
```

`server_protocol_info()` 的 vec 在 `CAPABILITY_USER_INSTRUCTIONS_V1` 与 `CAPABILITY_AGENT_HUB_V1` 之间插入 `CAPABILITY_USER_MIRROR_V1.to_string()`（字典序：`user-instructions` < `user-mirror` < `v1`）。

`models.rs` 定义（全部 `#[serde(rename_all = "camelCase")]`）：

```rust
pub const USER_MIRROR_PLAN_TTL_MINUTES: i64 = 15;
pub const USER_MIRROR_DEST_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub const USER_MIRROR_CAPABILITY_UNSUPPORTED: &str = "USER_MIRROR_CAPABILITY_UNSUPPORTED";
pub const USER_MIRROR_PEER_OFFLINE: &str = "USER_MIRROR_PEER_OFFLINE";
pub const USER_MIRROR_STALE: &str = "USER_MIRROR_STALE";
pub const USER_MIRROR_PREVIEW_REQUIRED: &str = "USER_MIRROR_PREVIEW_REQUIRED";
pub const USER_MIRROR_TRANSFER_LIMIT: &str = "USER_MIRROR_TRANSFER_LIMIT";
pub const USER_MIRROR_NATIVE_PATH_FORBIDDEN: &str = "USER_MIRROR_NATIVE_PATH_FORBIDDEN";
pub const USER_MIRROR_LEGACY_LOSSY_BLOCKED: &str = "USER_MIRROR_LEGACY_LOSSY_BLOCKED";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorDirection { Pull, Push }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorChangeOp { Write, Replace, Clear, Delete, Disable }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorMcpCredentialFactDto {
    pub present: bool,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorNativeFileFactDto {
    /// 稳定逻辑 id，例如 `claude.native.CLAUDE.md` / `cursor.slot.adapted`
    pub logical_id: String,
    pub content_hash: Option<String>,
    pub exists: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPortableItemDto {
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub content_hash: Option<String>,
    pub tree_hash: Option<String>,
    pub actual_enabled: Option<bool>,
    pub mcp_credential: Option<UserMirrorMcpCredentialFactDto>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorSlotHashesDto {
    pub common: Option<String>,
    pub adapted: Option<String>,
    pub exclusive: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentInventoryDto {
    pub target: AgentTarget,
    pub slots: UserMirrorSlotHashesDto,
    pub native_files: Vec<UserMirrorNativeFileFactDto>,
    pub items: Vec<UserMirrorPortableItemDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorInventoryDto {
    pub source_device_id: String,
    pub inventory_snapshot_hash: String,
    pub refreshed_at: String,
    pub agents: Vec<UserMirrorAgentInventoryDto>,
    pub credential_bearing_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewUserMirrorRequest {
    pub direction: UserMirrorDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_device_id: Option<String>,
    #[serde(default)]
    pub peer_device_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorFileChangeDto {
    pub logical_id: String,
    pub op: UserMirrorChangeOp,
    pub source_hash: Option<String>,
    pub dest_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPortableChangeDto {
    pub kind: PortableAssetKind,
    pub native_id: String,
    pub display_name: String,
    pub op: UserMirrorChangeOp,
    pub credential_bearing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentPlanDto {
    pub target: AgentTarget,
    pub instruction_writes: Vec<UserMirrorFileChangeDto>,
    pub portable_upserts: Vec<UserMirrorPortableChangeDto>,
    pub portable_deletes: Vec<UserMirrorPortableChangeDto>,
    pub plugin_disables: Vec<UserMirrorPortableChangeDto>,
    pub mcp_deletes: Vec<UserMirrorPortableChangeDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorPlanDto {
    pub plan_token: String,
    pub expires_at: String,
    pub direction: UserMirrorDirection,
    pub source_device_id: String,
    pub destination_device_id: String,
    pub remote_inventory_snapshot_hash: String,
    pub local_inventory_snapshot_hash: String,
    pub credential_bearing_count: u64,
    pub has_credential_bearing_assets: bool,
    pub agents: Vec<UserMirrorAgentPlanDto>,
    pub blocking_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyUserMirrorRequest {
    pub plan_token: String,
    pub client_request_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UserMirrorItemState {
    Succeeded,
    Failed,
    Skipped,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorAgentResultDto {
    pub target: AgentTarget,
    pub state: UserMirrorItemState,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMirrorResultDto {
    pub plan_token: String,
    pub client_request_id: String,
    pub source_device_id: String,
    pub destination_device_id: String,
    pub partial: bool,
    pub agents: Vec<UserMirrorAgentResultDto>,
}
```

`mod.rs` 只 `pub use` models。`control.rs` 常量改为 5。

- [ ] **Step 4: 跑测试确认通过**

```bash
./scripts/cc-partner-cargo.sh test --locked --lib net::protocol::tests::server_protocol_info_declares_user_mirror_v1_in_dictionary_order -- --nocapture --test-threads=1
./scripts/cc-partner-cargo.sh test --locked --lib backend::control -- --nocapture --test-threads=1
```

Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/user_mirror src-tauri/src/agent_hub/mod.rs src-tauri/src/net/protocol.rs src-tauri/src/backend/control.rs src-tauri/AGENTS.md
git commit -m "$(cat <<'EOF'
feat(agent-hub): add user-mirror DTOs, capability, API v5

Introduce agent-hub.user-mirror.v1 and AGENT_HUB_API_VERSION=5 so mixed
GUI/sidecar cannot run the new overwrite without a matching major.
EOF
)"
```

---

### Task 2: 本机全 Agent 用户级 inventory

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/inventory.rs`
- Modify: `user_mirror/mod.rs`
- Test: `inventory.rs` 内 `#[cfg(test)]`

- [ ] **Step 1: 写失败测试**

用 `tempfile` + `TargetEnvironment` 隔离 HOME（抄 `portable_inventory/scanner.rs` 的 `seed_all_targets_fixture` 精简版：只建 Claude `CLAUDE.md`、一个 skill 目录、Codex 空、Grok `AGENTS.md`）。

```rust
#[tokio::test]
async fn build_local_user_mirror_inventory_covers_all_hub_targets_and_redacts_secrets() {
    let env = seed_user_mirror_homes();
    std::fs::write(env.claude_home.join("CLAUDE.md"), "# src claude").unwrap();
    std::fs::create_dir_all(env.claude_home.join("skills/hello")).unwrap();
    std::fs::write(
        env.claude_home.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: d\n---\n",
    )
    .unwrap();
    std::fs::write(
        env.claude_home.join(".claude.json"),
        r#"{"mcpServers":{"s":{"env":{"TOKEN":"plain-secret-xyz"}}}}"#,
    )
    .unwrap();

    let dto = build_local_user_mirror_inventory(&env.app_state, "dev-a")
        .await
        .unwrap();
    let targets: Vec<_> = dto.agents.iter().map(|a| a.target).collect();
    for t in crate::agent_catalog::all_hub_targets() {
        assert!(targets.contains(&t), "missing {t:?}");
    }
    let json = serde_json::to_string(&dto).unwrap();
    assert!(!json.contains("plain-secret-xyz"));
    assert!(!json.contains(&env.claude_home.to_string_lossy().to_string()));
    let claude = dto.agents.iter().find(|a| a.target == AgentTarget::Claude).unwrap();
    assert!(claude.native_files.iter().any(|f| f.logical_id == "claude.native.CLAUDE.md" && f.exists));
    assert!(claude.items.iter().any(|i| i.kind == PortableAssetKind::Skill && i.native_id == "hello"));
}
```

- [ ] **Step 2: 跑测试确认失败**

```bash
./scripts/cc-partner-cargo.sh test --locked --lib agent_hub::user_mirror::inventory -- --nocapture --test-threads=1
```

Expected: `build_local_user_mirror_inventory` 未定义。

- [ ] **Step 3: 实现 `build_local_user_mirror_inventory`**

对 `all_hub_targets()`：

1. 槽 hash：读该 target 用户级 Hub canonical（`inspect_user_instruction_workspace` 已有路径；无资产则 `None`）。
2. 原生文件：`declared_native_paths` 中属于该 target 的文件 + `TargetPathResolver` 上 adapted/exclusive 物化路径。`logical_id` 用 `native_logical_id(target, file_name)` / `slot_logical_id(target, "adapted"|"exclusive")`。读 hash/size；**DTO 不放 path**。
3. portable：`inspect_portable_inventory_query(state, PortableInventoryQuery { targets: [t], scope: user, … })`，映射为 `UserMirrorPortableItemDto`；MCP 只抄 `present`+`hash`。
4. `inventory_snapshot_hash` = SHA-256(canonical JSON of agents without `refreshed_at`)。
5. `credential_bearing_count` = mcp `present==true` 条数。

Grok/Cursor **仓库** `AGENTS.md` 不进此 inventory（用户级镜像不扫项目）。`homes.grok.config_root/AGENTS.md` 若在 `declared_native_paths` 则作为用户级文件收录。

- [ ] **Step 4: 测试通过**

```bash
./scripts/cc-partner-cargo.sh test --locked --lib agent_hub::user_mirror::inventory -- --nocapture --test-threads=1
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/agent_hub/user_mirror
git commit -m "feat(agent-hub): scan all-agent user-scope mirror inventory"
```

---

### Task 3: 源端 selection 冻结（CAS）

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/selection.rs`
- Reuse: `object_store`, `snapshot/envelope.rs` 的 `SnapshotEnvelopeV1`；可参考 `snapshot/portable_builder.rs` 但不调用 adopt。

- [ ] **Step 1: 失败测试**

```rust
#[tokio::test]
async fn freeze_user_mirror_selection_puts_native_bytes_and_skill_tree_in_cas() {
    // 同源 Task 2 fixture
    let built = freeze_user_mirror_selection(&state, &inventory).await.unwrap();
    assert!(!built.envelope.objects.is_empty());
    let total: u64 = built.object_bytes.values().map(|b| b.len() as u64).sum();
    assert!(total > 0);
    assert!(total <= USER_MIRROR_DEST_MAX_TOTAL_BYTES);
    let json = serde_json::to_string(&built.envelope).unwrap();
    assert!(!json.contains("plain-secret-xyz")); // envelope 元数据无 secret；对象字节可含
    let secret_obj = built.object_bytes.values().any(|b| b.windows(15).any(|w| w == b"plain-secret-xyz"));
    assert!(secret_obj, "MCP credential bytes belong in CAS objects");
}
```

- [ ] **Step 2: 跑测失败**

```bash
./scripts/cc-partner-cargo.sh test --locked --lib agent_hub::user_mirror::selection -- --nocapture --test-threads=1
```

- [ ] **Step 3: 实现 `freeze_user_mirror_selection`**

- 按 inventory 的 logical_id / portable identity 读源文件字节，`ObjectStore::put_blob`。
- Plugin 包用现有 package bytes；Skill/Command 用 tree hash（`hash_skill_directory`）。
- MCP 用 canonical JSON **含凭据原文**。
- 超 512 MiB → `AppError::validation(USER_MIRROR_TRANSFER_LIMIT)`。
- `legacyLossy` 对象标记 blocked，apply 不得覆盖真凭据。
- 源端 **零** adopt/uninstall。

返回：

```rust
pub struct BuiltUserMirrorSelection {
    pub envelope: SnapshotEnvelopeV1,
    pub object_bytes: BTreeMap<String, Vec<u8>>,
    pub transfer_id: String,
    pub item_bindings: Vec<UserMirrorObjectBinding>, // logical_id or kind+native_id+target → object hash
}
```

进程内 staging map（`transfer_id` → objects），与 `source_read_object_chunk` 同类，TTL 随 transfer。

- [ ] **Step 4: 测试通过并 commit**

```bash
git commit -m "feat(agent-hub): freeze user-mirror selection into CAS"
```

---

### Task 4: Preview diff

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/preview.rs`

- [ ] **Step 1: 失败测试（纯函数可先测 `diff_inventories`）**

```rust
#[test]
fn diff_inventories_replaces_overlap_and_deletes_dest_extras() {
    let source = fixture_inventory("src", AgentTarget::Claude, &["keep"], &["gone-on-src"]);
    let dest = fixture_inventory("dst", AgentTarget::Claude, &["keep"], &["dest-only-skill"]);
    let plan = diff_inventories(&source, &dest, "src-dev", "dst-dev", UserMirrorDirection::Pull);
    let claude = plan.agents.iter().find(|a| a.target == AgentTarget::Claude).unwrap();
    assert!(claude.portable_upserts.iter().any(|c| c.native_id == "keep"));
    assert!(claude.portable_deletes.iter().any(|c| c.native_id == "dest-only-skill"));
    assert!(!claude.portable_deletes.iter().any(|c| c.native_id == "gone-on-src"));
}

#[test]
fn diff_inventories_plugin_extra_is_disable_not_delete() {
    let source = fixture_plugin("src", &[]);
    let dest = fixture_plugin("dst", &["orphan-plug"]);
    let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
    let claude = &plan.agents[0];
    assert!(claude.plugin_disables.iter().any(|c| c.native_id == "orphan-plug"
        && c.op == UserMirrorChangeOp::Disable));
    assert!(claude.portable_deletes.iter().all(|c| c.kind != PortableAssetKind::Plugin));
}

#[test]
fn diff_inventories_mcp_extra_is_delete_and_counts_credentials() {
    let mut source = fixture_mcp("s", &["keep"]);
    source.credential_bearing_count = 1;
    let dest = fixture_mcp("d", &["keep", "extra"]);
    let plan = diff_inventories(&source, &dest, "s", "d", UserMirrorDirection::Pull);
    assert!(plan.has_credential_bearing_assets);
    assert!(plan.agents[0].mcp_deletes.iter().any(|c| c.native_id == "extra"));
}
```

空槽/空文件：dest 有、source `exists=false` → `instruction_writes` `op=Clear`。

- [ ] **Step 2: 跑测失败 → 实现 `diff_inventories`（零 IO）→ 通过**

身份键：native file = `logical_id`；portable = `(target, kind, native_id)`。

- [ ] **Step 3: `preview_user_mirror_for_state`**

- Pull：`require_capability(peer, CAPABILITY_USER_MIRROR_V1)`，否则 `USER_MIRROR_CAPABILITY_UNSUPPORTED`。拉远端 inventory（Task 8 路由未就绪前，service 可先调 `build_local` 仅测 diff；本 task 用函数注入 `fetch_remote` trait 或 `#[cfg(test)]` stub）。
- 本机 inventory + diff → 填 `plan_token` / `expires_at`。
- 本 task **先不写 SQLite**，返回 DTO；ledger 在 T7。

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(agent-hub): preview user-mirror as replace-plus-delete diff"
```

---

### Task 5: Apply 提示词槽 + 原生文件

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/apply.rs`（本 task 只做 instruction 段）
- 调用：`write_user_native_instruction_file`；Hub 槽用现有 `save_user_instruction_blocks` / inspect 同构 API（按 target 循环，`deviceId` 禁止嵌套）。

- [ ] **Step 1: 失败测试**

双 `data_dir`：source Claude `CLAUDE.md` = `FROM-SRC`；dest = `OLD-DEST`。再在 dest 放一个白名单内、source 不存在的空文件需要 Clear。

```rust
#[tokio::test]
async fn apply_instruction_mirror_overwrites_native_bytes_and_clears_missing() {
    apply_user_mirror_instructions(&dest_state, &plan, &objects).await.unwrap();
    let dest_claude = std::fs::read_to_string(dest_home.join(".claude/CLAUDE.md")).unwrap();
    assert_eq!(dest_claude, "FROM-SRC");
    // Grok 不得写入「项目仓库」AGENTS.md 夹具
    assert!(!std::fs::read_to_string(&repo_agents).unwrap_or_default().contains("FROM-SRC"));
}
```

另测：解析到白名单外 path → `USER_MIRROR_NATIVE_PATH_FORBIDDEN`，其他 Agent 继续（本 task 若只跑 Claude 则可单测函数 `write_one_native`）。

- [ ] **Step 2: 实现**

对每个 `instruction_writes`：

1. `logical_id` → 本机白名单绝对路径（仅 dest process 解析，不信 LAN path）。
2. `Write`/`Replace`：从 CAS 取字节，UTF-8 校验，`WriteUserNativeInstructionFileRequest { expected_hash: dest_hash }`。
3. `Clear`：`content=""`。
4. 同步覆盖该 target 三槽 canonical（源 selection 里的 slot objects）。空源槽写空块。
5. 失败记录在该 Agent `Failed`，**不回滚**已写文件。

- [ ] **Step 3: 测试通过并 commit**

```bash
git commit -m "feat(agent-hub): mirror user instruction slots and native files"
```

---

### Task 6: Apply portable upsert + extras

**Files:**
- Modify: `apply.rs`
- 复用 `portable_actions` 的 enable/disable/attach 与 MCP config_patch；**不要**新写 CLI spawn。

- [ ] **Step 1: 失败测试**

1. 源有 skill `keep`，dest 有 `dest-only` → apply 后 dest 库存无 `dest-only`，native 软链拆除，`~/.agents` 夹具若存在仍在。
2. dest-only plugin → viewing disable，包目录仍在。
3. dest-only MCP server 从 json/toml 删除；源 MCP 凭据出现在 dest 配置文件，**不得**出现在 result JSON。
4. `legacyLossy` MCP → 该条 `USER_MIRROR_LEGACY_LOSSY_BLOCKED`，dest 原凭据保留。

- [ ] **Step 2: 实现顺序（与 Spec §6.5）**

CAS 已在本地 → 按 Agent：portable upsert（视为 `replaceAfterPreview`）→ Skill/Command extras detach → Plugin extras Disable → MCP extras 删 leaf 键。每 Agent 独立 `Succeeded/Failed`。`partial = agents.iter().any(|a| a.state != Succeeded)`。

安装失败不得标成功；rescan 必须观察到 upsert 的 native_id，delete 必须观察不到。

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(agent-hub): mirror portable assets and destination extras"
```

---

### Task 7: Plan ledger 与 get()

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/ledger.rs`
- Modify: `src-tauri/src/storage/agent_hub_repo.rs`（`CREATE TABLE IF NOT EXISTS agent_hub_user_mirror_plans`：`plan_token PK, expires_at, plan_json, client_request_id, claimed_at, consumed_at, result_json, created_at`）
- Modify: `user_mirror/service.rs`（本 task 创建）

- [ ] **Step 1: 失败测试**

抄 `claim_portable_pull_plan` 语义：

- 同 `plan_token`+`client_request_id` 且已有 `result_json` → Replay。
- 已 claim 未 complete → Pending → `get` 返回 `outcomeUnknown`。
- 不同 plan 抢同一 `client_request_id` → conflict。
- 过期 plan apply → `USER_MIRROR_STALE`。

- [ ] **Step 2: 实现 claim/complete/get，preview 插入 plan 行**

`preview` 写库；`apply` claim → 调 T5/T6 → `complete`。Apply 入口检查确认：service 层不负责 UI checkbox，但 **缺少 plan** 或 token mismatch → `USER_MIRROR_PREVIEW_REQUIRED`。

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(agent-hub): persist user-mirror plans with request-id replay"
```

---

### Task 8: LAN 六路由

**Files:**
- Modify: `src-tauri/src/net/routes/agent_hub.rs`
- Modify: `src-tauri/src/net/http_server.rs`
- Modify: `docs/p2p-protocol.md`（与路由同 commit，否则 `check-p2p-route-inventory` 失败）
- Modify: `src-tauri/src/net/routes/agent_hub.rs` tests（现有 capability 原子性测试加上 user-mirror）

路由（handler 名与 path 必须被 inventory 脚本扫到）：

| Method | Path |
|--------|------|
| POST | `/api/agent-hub/user-mirror/inventory` |
| POST | `/api/agent-hub/user-mirror/selection` |
| GET | `/api/agent-hub/user-mirror/objects/:transferId/:objectHash` |
| POST | `/api/agent-hub/user-mirror/prepare` |
| PUT | `/api/agent-hub/user-mirror/:transferId/objects/:objectHash` |
| POST | `/api/agent-hub/user-mirror/:transferId/commit` |

- [ ] **Step 1: 扩展 `routes/agent_hub.rs` 里现有「push 三路由 + portable-pull 三路由同 build 宣告」测试，要求 `CAPABILITY_USER_MIRROR_V1` 与六条 handler 符号同时存在。先失败。**

- [ ] **Step 2: 实现 handler**

源端：

- inventory：`build_local_user_mirror_inventory`；metadata-only 断言（无 `sourcePath`、无绝对 home、无 secret 子串检测可放单测）。
- selection：`freeze_user_mirror_selection`，返回 `{transferId, envelope, missingObjectHashes}`。
- GET objects：chunk ≤ 8 MiB，query `offset`。

目标端 Push 接收：

- prepare/objects：可复用 `replication/receiver` 的 staging 实现，但 **独立 transfer 前缀** `user-mirror/`，避免与旧 push ledger 混幂等键。
- commit：校验 objects → 调 `apply_user_mirror` 全流程（写盘+extras）。成功响应必须能证明 native 已写（L2 读文件）；禁止只 `SnapshotImporter::commit_import`。

Body 仍走 P2pError 信封。`X-Cc-Partner-Expected-Device-Id` 走现有 guard。

- [ ] **Step 3: 文档表**

在 `docs/p2p-protocol.md` Capability 列表插入 `agent-hub.user-mirror.v1`（字典序），并在路由表加六行：retry class 与现 push/pull 对齐（prepare/commit `requires-idempotency-key`，objects `no-transport-retry`）。写明 **不是身份认证**、commit **含原生写盘**。

- [ ] **Step 4: 验证**

```bash
node scripts/check-p2p-route-inventory.mjs
./scripts/cc-partner-cargo.sh test --locked --lib net::routes::agent_hub -- --nocapture --test-threads=1
```

Expected: inventory 脚本 exit 0。

- [ ] **Step 5: Commit**

```bash
git commit -m "feat(agent-hub): advertise user-mirror.v1 routes atomically"
```

---

### Task 9: Tauri / control 接线

**Files:**
- Modify: `src-tauri/src/commands/agent_hub.rs`
- Modify: `src-tauri/src/backend/control_agent_hub.rs`
- Modify: `src-tauri/src/backend/control_client.rs`
- Modify: `src-tauri/src/lib.rs` `invoke_handler`

命令：

- `agent_hub_preview_user_mirror`
- `agent_hub_apply_user_mirror`（长 mutation，timeout 900s）
- `agent_hub_get_user_mirror`

Control op：

- `agent_hub.preview_user_mirror`（mutation：写 plan）
- `agent_hub.apply_user_mirror`（mutation）
- `agent_hub.get_user_mirror`（只读）

- [ ] **Step 1: 失败测试**

`control_agent_hub.rs` 现有字符串断言测试追加三个 op 名；`is_mutation_op`：preview+apply true，get false。`lib.rs` 源码测试（若已有 command 名列表）追加三个 `agent_hub_*_user_mirror`。

- [ ] **Step 2: 实现**

与 `preview_portable_pull` 同形：`proxy_agent_hub!` → `UserMirrorService::…`。Apply 前 `require_agent_hub_write_compatibility(5)`（即当前常量）。GUI 不得直连 peer。

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(agent-hub): expose user-mirror preview/apply/get on control"
```

---

### Task 10: Push sender

**Files:**
- Create: `src-tauri/src/agent_hub/user_mirror/push.rs`
- `apply_user_mirror` 的 Push 方向：本机构建 selection 一次，对每个 peer：capability+`device.request-binding.v1`+`health.device_id==peer_id` → prepare → PUT missing → commit。并发 ≤ 3。一 peer 失败不影响其他。

- [ ] **Step 1: 失败测试（frozen axum peer，抄 `agent_hub_replication_smoke`）**

- 对端无 user-mirror capability → 该 target `USER_MIRROR_CAPABILITY_UNSUPPORTED`，**零**请求到 `/api/agent-hub/push/prepare`。
- 对端有能力：commit 后对端夹具 `CLAUDE.md` 等于源。
- device_id mismatch → fail-closed，不传 objects。

- [ ] **Step 2: 实现并接入 `apply_user_mirror`（direction=Push 时走 sender，不在本机写盘）**

Pull：source=peer，dest=local apply。  
Push：source=local freeze，dest=each peer commit。

失败 peer 写入源侧失败表（与现 `list_failed_source_push_targets` 同形或新列 `kind=user_mirror`）。Attention `agent_hub_source` 增加稳定 id `agent-hub:mirror-failed:<requestId>:<peerId>`，summary 仅 peer 名/error code，无 payload/secret；Inbox 只导航。

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(agent-hub): push user-mirror snapshot with dest native apply"
```

---

### Task 11: L2 smoke

**Files:**
- Create: `src-tauri/tests/agent_hub_user_mirror_smoke.rs`

三个 `#[tokio::test]`，文件头注释 Evidence id。

1. **`l2_agent_hub_user_mirror_001_full_mirror`**  
   双隔离 data_dir + frozen loopback。全 `all_hub_targets` 出现在 inventory。Claude 三槽+`CLAUDE.md`+skill 对齐。Dest 多余 skill 消失。Grok **不**改夹具里的「仓库 AGENTS.md」。MCP UI JSON 无 secret。Health 宣告 token。缺 token 的第二 peer：零 portable-pull / 旧 push 命中（计数器）。

2. **`l2_agent_hub_user_mirror_002_partial_no_rollback`**  
   注入一个 Agent 写失败（只读目录或 failpoint）。结果 `partial=true`，已成功 Agent 文件仍是源内容。同 `clientRequestId` 再 apply 重放同一 result。

3. **`l2_agent_hub_user_mirror_003_extras_policy`**  
   Skill detach、Plugin disable 非 uninstall（包目录还在）、MCP server 键删除。

```bash
./scripts/cc-partner-cargo.sh test --locked --test agent_hub_user_mirror_smoke -- --nocapture --test-threads=1
```

Expected: PASS。

- [ ] **Commit**

```bash
git commit -m "test(agent-hub): L2 user-mirror dual-data_dir smoke"
```

---

### Task 12: 前端类型与 API

**Files:**
- Create: `web/src/lib/types/userMirror.ts`（字段与 Rust camelCase **逐字相同**）
- Create: `web/src/lib/schemas/userMirror.ts`（fail-closed decoder）
- Create: `web/src/api/userMirror.ts`
- Test: `web/src/api/userMirror.test.ts`

```ts
export const userMirrorApi = {
  preview: (request: PreviewUserMirrorRequest) =>
    invokeDecoded('agent_hub_preview_user_mirror', { request }, userMirrorPlanDecoder),
  apply: (request: ApplyUserMirrorRequest) =>
    invokeDecoded('agent_hub_apply_user_mirror', { request }, userMirrorResultDecoder),
  get: (clientRequestId: string) =>
    invokeDecoded('agent_hub_get_user_mirror', { clientRequestId }, userMirrorResultDecoder),
};
```

Decoder 拒绝未知 `direction`/`op`/`state`。MCP 结构若出现 `token`/`value`/`secret` 字段必须 fail。

```bash
cd web && npm test -- userMirror
```

- [ ] **Commit**

```bash
git commit -m "feat(web): add user-mirror invoke client and decoders"
```

---

### Task 13: 替换 Pull/Push UI

**Files:**
- Create: `web/src/pages/AgentHub/userMirror/UserMirrorDialog.tsx`
- Create: `web/src/pages/AgentHub/userMirror/UserMirrorDialog.module.css`（只用 `var(--*)`）
- Create: `web/src/pages/AgentHub/userMirror/useUserMirrorController.ts`
- Create: `web/src/pages/AgentHub/userMirror/userMirrorPresentation.ts`
- Modify: `AgentHub.tsx`、`useAgentHubController.ts`、`LanPushDialog.tsx` 停用、`PortablePullDrawer` 生产入口删除（文件可留一版 re-export 以免测试残留，或直接改测试）
- Modify: `web/src/i18n/locales/{zh,en}/agentHub.json`（`userMirror.*`，保留 LAN 无鉴权句）
- Test: `useUserMirrorController.test.ts`、`UserMirrorDialog.test.tsx`

**Controller 合同**

- `direction: 'pull' | 'push'`
- Pull：只选 `sourceDeviceId`（在线 peer）
- Push：`selectedPeerIds: string[]`
- 无 kind 过滤、无 item checkbox、无 conflict policy、无 mode radio
- `preview()` / `apply()` / `reconcile()`；换设备必须清 plan
- `canApply = Boolean(plan) && confirmed && !busy && !stale`
- `confirmed` checkbox 默认 false

**Dialog**

- 预览按 Agent 列出 writes/upserts/deletes/disables 计数 + 凭据 Pill
- 确认文案 key：`agentHub:userMirror.confirmOverwrite`
- 忙时 `closeOnEscape={false}` `closeOnBackdrop={false}`
- StatusMessage 展示 partial

壳层按钮仍叫 Pull/Push，hint 改为镜像。项目锁继续 `onPull/onPush` no-op。

```bash
cd web && npm test -- useUserMirrorController UserMirrorDialog
cd web && npm run check:i18n && npm run check:css-tokens
```

- [ ] **Commit**

```bash
git commit -m "feat(web): replace Agent Hub pull/push pickers with user mirror"
```

---

### Task 14: E2E

**Files:**
- Create: `web/tests/agent-hub-user-mirror.spec.ts`
- Modify: `web/tests/agent-hub-interaction.spec.ts`（去掉逐项 Pull 勾选断言，改为镜像对话框；devices capabilities 含 `agent-hub.user-mirror.v1`）
- 更新 `backendHarness` 命令：`agent_hub_preview_user_mirror` / `apply` / `get`

用例（`E2E-AGENT-HUB-USER-MIRROR-001`）：

1. 打开 Pull：无 `lan-push-mode-fullHub`、无 inventory checkbox、无冲突策略 radio。
2. 未预览时 Apply disabled。
3. preview 后未勾选确认，Apply disabled。
4. 勾选后 Apply 调用 `agent_hub_apply_user_mirror`，body 仅 `planToken`+`clientRequestId`。
5. 换 peer 后旧 plan 消失，Apply disabled。
6. Push：无 asset id 输入；多 peer 报告区存在。

```bash
cd web && npm run test:e2e -- agent-hub-user-mirror.spec.ts
```

- [ ] **Commit**

```bash
git commit -m "test(web): e2e Agent Hub user-mirror pull/push dialogs"
```

---

### Task 15: 文档与质量矩阵

**Files:**
- `docs/prd.md` §2.5：Pull/Push 改为用户级全 Agent 镜像写盘；删除「勾选复制」表述；`AGENT_HUB_API_VERSION=5`。
- `docs/development/testing.md`：登记三条 L2 + E2E + L3 NOT VERIFIED。
- `docs/development/quality-matrix.json`：插入

```json
{
  "id": "L2-AGENT-HUB-USER-MIRROR-001",
  "surface": "agent-hub",
  "level": "L2",
  "tests": ["src-tauri/tests/agent_hub_user_mirror_smoke.rs"],
  "command": "cd src-tauri && cargo test --locked --test agent_hub_user_mirror_smoke -- --nocapture --test-threads=1",
  "ciJob": "quality",
  "platforms": ["rust"],
  "exclusions": [
    "LAN identity authentication",
    "real dual-host mDNS",
    "packaged desktop GUI"
  ],
  "docs": [
    "docs/p2p-protocol.md",
    "docs/superpowers/specs/2026-08-23-agent-hub-user-mirror-design.md"
  ],
  "notes": "Dual isolated data_dir user-scope mirror: all hub targets, native instruction bytes, dest extra skill gone, Grok does not write repo AGENTS.md, MCP secrets absent from DTO, missing capability hits zero legacy pull/push routes."
}
```

同样加 `L2-…-002`、`L2-…-003`、`E2E-AGENT-HUB-USER-MIRROR-001`（command `cd web && npm run test:e2e -- agent-hub-user-mirror.spec.ts`）、`L3-AGENT-HUB-USER-MIRROR-001` status `NOT VERIFIED`。

- `docs/development/adapt-new-agent.md` 硬规则：用户发起的 user-mirror 允许写该身份用户级白名单文件与 MCP leaf / viewing Disable，不 spawn 未认证 CLI；新身份必须进入 `all_hub_targets` 否则镜像缺席要显式失败而不是跳过。
- 根 `AGENTS.md`、`web/AGENTS.md`、`src-tauri/AGENTS.md`：Agent Hub 句改为「用户级 Pull/Push = 全 Agent 镜像」，链到 spec。
- 跑：

```bash
node scripts/check-docs.mjs
node scripts/check-quality-traceability.mjs
node scripts/check-p2p-route-inventory.mjs
```

- [ ] **Commit**

```bash
git commit -m "docs: record Agent Hub user-mirror contract and evidence IDs"
```

---

## Self-Review

| Spec 条款 | Task |
|-----------|------|
| M1 镜像对齐（多余删除） | T4 T6 T11-003 |
| M2 全部已登记 Agent | T2 T11-001 |
| M3 立刻写盘 | T5 T8 commit T10 |
| M4 Pull 与 Push | T9 T10 T13 |
| M5 新操作 + token + 不回落 | T1 T8 T10 缺能力计数器 |
| M6 部分成功不回滚 | T6 T7 T11-002 |
| M7 预览 + 确认勾选 | T4 T13 T14 |
| M8 用户级 only | T2 只扫 user；项目锁 UI 保留 |
| 三槽 + 原生文档 | T2 T5 |
| Skill/Command detach 不删 ~/.agents | T6 T11-003 |
| Plugin Disable 非 Uninstall | T4 T6 T11-003 |
| MCP 凭据 CAS、UI 脱敏、多余删除 | T2 T3 T6 |
| Grok/Cursor 不写仓库 AGENTS.md | T2 T5 T11-001 |
| API v5 | T1 T9 |
| 512 MiB / 8 MiB | T1 常量 T3 T8 |
| Attention 镜像失败 | T10 |
| 就地远程编辑 / Git / 项目 | 不改那些模块 |

无 TBD。类型名全程 `UserMirror*`。Tauri 名全程 `agent_hub_*_user_mirror`。
