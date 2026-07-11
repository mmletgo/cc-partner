# P2P Protocol Metadata and Error Envelope Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 P2P 增加一代滚动兼容的协议版本、capability gate、稳定错误 envelope 与端到端 request ID，并建立写端点幂等风险清单。

**Architecture:** `/api/health` 返回完整权威协议信息，mDNS TXT 只承载有长度上限的发现提示；缺字段的设备按 legacy v0 解析。axum 边界新增 request-context middleware 和 `P2pError`，不改变 Tauri IPC 的现有 `AppError` reject 契约。客户端统一解析 v1 envelope，同时兼容旧 `{error}`；新能力必须先以 health capability gate。

**Tech Stack:** Rust 2021, axum 0.7, reqwest, serde, uuid, tracing, mdns-sd, tokio.

## Global Constraints

- 执行顺序：在 Vitest/CI 与 Workbench controller 两个 Wave 1/2 计划完成后开始本计划。
- 执行阶段先使用 `superpowers:using-git-worktrees` 创建独立 worktree/branch；每次 broad `git add` 前检查 `git status --short`，只提交本计划文件。
- 开始前读取根 `AGENTS.md` 与 `src-tauri/CLAUDE.md`；新增/修改 Rust 业务函数必须有中文 Business Logic / Code Logic doc comment。
- 本轮只维护 v0→v1 一代兼容；不得引入永久多版本分支或猜测式版本比较。
- mDNS 只用于发现和快速预判；调用新路由前必须以 health 的完整 capability 列表为准。
- 不直接修改 `AppError` 的 Tauri 序列化外形；标准 envelope 只在 HTTP/P2P 边界产生。
- `error` 保留用户可读文本；客户端业务判断只能使用 `code`、HTTP status 和 typed error kind。
- `details` 只能携带脱敏结构化字段；禁止请求正文、路径内容、凭据、环境变量和 Authorization header。
- 新增可重试副作用 route 必须先在幂等清单声明策略；commit/push/merge/delete 不得自动重试。

---

## File Structure

- Create `src-tauri/src/net/protocol.rs`: protocol v1 常量、capability 列表、legacy 解析和 mDNS 编码。
- Create `src-tauri/src/net/request_context.rs`: `X-CC-Request-Id` 读取/生成、request extension、响应 header 和 tracing span middleware。
- Create `src-tauri/src/net/error_response.rs`: `P2pErrorEnvelope`、稳定 error code/status/retryable 映射和 `IntoResponse`。
- Create `src-tauri/src/net/peer_error.rs`: 客户端 typed error 与 v0/v1 response 解析。
- Modify `src-tauri/src/net/mod.rs`: 导出新模块。
- Modify `src-tauri/src/net/routes/health.rs`: 返回完整 protocol metadata。
- Modify `src-tauri/src/net/discovery.rs`: 发布/解析 `proto` 与有界 `caps` TXT。
- Modify `src-tauri/src/models/device.rs`: 保存发现提示但不把它当权威 capability。
- Modify `src-tauri/src/net/http_server.rs`: 安装 request ID middleware，并逐 route 使用统一错误边界。
- Modify `src-tauri/src/error.rs`: 增加只读错误分类 helper，不改变 IPC response。
- Modify `src-tauri/src/net/peer_client.rs`: 暴露 `health_info`，保留旧 `health` wrapper。
- Modify `src-tauri/src/orchestrator/remote_client.rs`: 复用统一 v0/v1 error parser。
- Create `docs/p2p-protocol.md`: 协议兼容政策、capability 规则和全部 P2P 写端点风险表。
- Create `scripts/check-p2p-route-inventory.mjs`: 对照 router literal paths 与协议表，阻止漏分类。
- Modify `src-tauri/CLAUDE.md`: 记录新增 route 的协议/错误/幂等门禁。

## Shared Interfaces

```rust
pub const PROTOCOL_VERSION: u16 = 1;
pub const CAPABILITY_ERRORS_ENVELOPE_V1: &str = "errors.envelope.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerProtocolInfo {
    #[serde(default)]
    pub protocol_version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct P2pErrorEnvelope {
    pub error: String,
    pub code: String,
    pub request_id: String,
    pub retryable: bool,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PeerCallError {
    Unsupported { capability: String },
    Offline { message: String },
    InvalidResponse { message: String },
    Remote {
        status: reqwest::StatusCode,
        envelope: P2pErrorEnvelope,
    },
}
```

Stable status mapping:

| Meaning | HTTP | Code prefix | retryable |
| --- | ---: | --- | --- |
| validation | 400 | `request.validation` / domain validation | false |
| not found | 404 | `<domain>.not_found` | false |
| conflict | 409 | `<domain>.conflict` | false |
| offline/unavailable | 503 | `peer.offline` / `<domain>.unavailable` | true only when replay is safe |
| timeout | 504 | `peer.timeout` | true only when replay is safe |
| internal | 500 | `internal.error` | false |
| future authentication | 401/403 | `auth.*` | false |

---

### Task 1: Define Protocol v1 and Legacy Parsing

**Files:**
- Create: `src-tauri/src/net/protocol.rs`
- Modify: `src-tauri/src/net/mod.rs`

- [ ] **Step 1: Write failing protocol tests**

Cover: missing fields deserialize to version `0` and empty capabilities; v1 round-trip; capability list is sorted/deduplicated; unknown future fields are ignored; `supports()` matches exact full tokens only.

```rust
#[test]
fn missing_protocol_fields_are_legacy_v0() {
    let info: PeerProtocolInfo = serde_json::from_str("{}").unwrap();
    assert_eq!(info.protocol_version, 0);
    assert!(info.capabilities.is_empty());
}
```

- [ ] **Step 2: Run the focused failing test**

```bash
cd src-tauri
cargo test --locked net::protocol::tests::missing_protocol_fields_are_legacy_v0
```

Expected: compile failure because `net::protocol` does not exist.

- [ ] **Step 3: Implement protocol constants and helpers**

`server_protocol_info()` must return version `1` and the complete, sorted list of capabilities whose routes are present in that build. This plan initially advertises only `errors.envelope.v1`; the runtime and Inbox plans add their constants and declarations atomically with their routes. `PeerProtocolInfo::supports(&self, capability)` requires `protocol_version >= 1` and an exact token match. Keep deserialization tolerant of absent and additional fields.

- [ ] **Step 4: Verify protocol tests**

```bash
cd src-tauri
cargo test --locked net::protocol::tests
```

Expected: all protocol model tests pass.

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/mod.rs src-tauri/src/net/protocol.rs
git commit -m "feat: define p2p protocol metadata"
```

---

### Task 2: Expose Authoritative Health Metadata

**Files:**
- Modify: `src-tauri/src/net/routes/health.rs`
- Modify: `src-tauri/src/net/peer_client.rs`

- [ ] **Step 1: Extend health contract tests before implementation**

Add route serialization tests asserting `protocol_version: 1` and `errors.envelope.v1`. Add client fixtures for a legacy response with no new fields and a v1 response with capabilities. Add a negative assertion that unimplemented runtime/attention capabilities are not advertised yet.

- [ ] **Step 2: Extend `HealthResponse` compatibly**

Add flattened or explicit `protocol_version` and `capabilities` fields with serde defaults. The server always fills them from `server_protocol_info()`; client deserialization accepts absent fields as v0.

- [ ] **Step 3: Add typed `health_info` without breaking callers**

Implement:

```rust
pub async fn health_info(&self, base_url: &str) -> Result<HealthResponse, PeerCallError>;
pub async fn health(&self, base_url: &str) -> bool;
```

The legacy `health` wrapper returns `health_info(...).await.map(|r| r.ok).unwrap_or(false)`. New capability-gated callers use only `health_info`.

- [ ] **Step 4: Run health route/client tests**

```bash
cd src-tauri
cargo test --locked net::routes::health::tests
cargo test --locked net::peer_client::tests
```

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/routes/health.rs src-tauri/src/net/peer_client.rs
git commit -m "feat: publish p2p capabilities in health"
```

---

### Task 3: Add Bounded mDNS Protocol Hints

**Files:**
- Modify: `src-tauri/src/net/discovery.rs`
- Modify: `src-tauri/src/models/device.rs`

- [ ] **Step 1: Write UTF-8 byte-boundary tests**

Tests must assert: tokens sort lexicographically; duplicates disappear; a token that would take the entire `caps=<...>` TXT string over 220 UTF-8 bytes is omitted in full; no token is truncated; multibyte test tokens use `.as_bytes().len()` rather than character count.

- [ ] **Step 2: Implement `encode_mdns_capabilities`**

```rust
pub fn encode_mdns_capabilities(capabilities: &[String], max_txt_bytes: usize) -> String;
```

Build from complete tokens one at a time. `max_txt_bytes` includes the `caps=` prefix and commas. Return an empty `caps=` value if the first token cannot fit.

- [ ] **Step 3: Publish and parse `proto`/`caps`**

Service registration adds `proto=1` and `caps=<bounded-list>`. Discovery stores them as non-authoritative hints on `Device`; missing or malformed `proto` becomes `0`, malformed/empty capability tokens are dropped. Existing devices without TXT additions remain discoverable.

- [ ] **Step 4: Run discovery tests**

```bash
cd src-tauri
cargo test --locked net::discovery::tests
```

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/discovery.rs src-tauri/src/models/device.rs
git commit -m "feat: advertise bounded p2p metadata over mdns"
```

---

### Task 4: Establish the Request-ID Boundary

**Files:**
- Create: `src-tauri/src/net/request_context.rs`
- Modify: `src-tauri/src/net/mod.rs`
- Modify: `src-tauri/src/net/http_server.rs`

- [ ] **Step 1: Write middleware contract tests**

Use a tiny axum router to prove: a valid incoming `X-CC-Request-Id` is preserved; missing/blank/invalid values produce a UUID; handler extensions and response header see the same ID; parallel requests do not share IDs.

- [ ] **Step 2: Implement the extractor and middleware**

```rust
#[derive(Debug, Clone)]
pub struct P2pRequestContext {
    pub request_id: String,
}
```

The middleware validates printable ASCII with a 128-byte maximum, inserts the context into request extensions, creates a tracing span field `request_id`, and writes exactly the same value into the response header.

- [ ] **Step 3: Install the middleware around every `/api/*` P2P/mobile route**

Apply it once at the router layer in `http_server.rs`; do not add per-handler UUID generation. Confirm health and a representative workbench route both return the header.

- [ ] **Step 4: Run tests**

```bash
cd src-tauri
cargo test --locked net::request_context::tests
cargo test --locked net::http_server::tests
```

- [ ] **Step 5: Commit**

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/request_context.rs src-tauri/src/net/mod.rs src-tauri/src/net/http_server.rs
git commit -m "feat: correlate p2p requests with request ids"
```

---

### Task 5: Add the HTTP Error Envelope Without Changing Tauri IPC

**Files:**
- Create: `src-tauri/src/net/error_response.rs`
- Modify: `src-tauri/src/net/mod.rs`
- Modify: `src-tauri/src/error.rs`

- [ ] **Step 1: Write status/code/retryability table tests**

Test validation, not-found, conflict, unavailable, timeout and internal cases. Assert `details` defaults to `{}` and every body request ID matches the injected context. Add a regression proving direct Tauri serialization of `AppError` remains the old form.

- [ ] **Step 2: Add a non-serializing AppError classification helper**

Expose a stable internal category from `AppError` while retaining existing variants and IPC conversion. Add specific constructors only where the current enum cannot distinguish validation/conflict/unavailable/timeout.

- [ ] **Step 3: Implement `P2pError` and `IntoResponse`**

`P2pError::from_app_error(error, context, domain_code)` constructs `P2pErrorEnvelope`; the `IntoResponse` implementation sets status and `X-CC-Request-Id`. `retryable` is false by default and can only be enabled explicitly by a route whose idempotency policy permits replay.

- [ ] **Step 4: Verify no HTTP path emits request bodies/details accidentally**

Use fixtures containing `Authorization`, token-like strings, Prompt text and absolute home paths; assert none appears in `details` or debug formatting.

- [ ] **Step 5: Run tests and commit**

```bash
cd src-tauri
cargo test --locked net::error_response::tests
cargo test --locked error::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/error_response.rs src-tauri/src/net/mod.rs src-tauri/src/error.rs
git commit -m "feat: standardize p2p error responses"
```

---

### Task 6: Migrate the Axum Boundary by Domain

**Files:**
- Modify: `src-tauri/src/net/routes/mod.rs`
- Modify: route modules registered in `src-tauri/src/net/http_server.rs`

- [ ] **Step 1: Add route-level envelope tests for each status class**

Start with existing helper responses in `routes/mod.rs`. Prove a validation route returns 400, missing entity 404, conflict 409, unavailable 503 and unexpected repository failure 500, each with the same request ID in header/body.

- [ ] **Step 2: Replace the shared blanket 500 conversion**

Route handlers return `P2pResult<T>` or call one shared response adapter. Preserve success DTOs and route paths. Do not change business logic while migrating the boundary.

- [ ] **Step 3: Convert health, sync, transfer and asset-sync routes**

Migrate `health.rs`, `sync.rs`, `scratchpad_sync.rs`, `claude_md_sync.rs`, `claude_code_assets.rs`, `cc_history.rs`, `ssh_target_sync.rs` and `transfer.rs`. Run their route tests, then commit:

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/routes
git commit -m "refactor: standardize sync and transfer http errors"
```

- [ ] **Step 4: Convert Workbench read routes**

Migrate list/get/status/open/preview/replay/search/read handlers in `workbench.rs` and add 400/404/503 fixtures. Leave writes unchanged in this step. Run the Workbench route tests, then commit:

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/routes/workbench.rs
git commit -m "refactor: standardize workbench read errors"
```

- [ ] **Step 5: Convert Workbench write routes**

Migrate create/save/format/write/resize/focus/session/worktree/Git/delete handlers without adding retries. Add conflict and unavailable fixtures, run Workbench route tests, then commit:

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/routes/workbench.rs
git commit -m "refactor: standardize workbench write errors"
```

- [ ] **Step 6: Convert Orchestrator and Mobile adapters**

Migrate `orchestrator.rs` and `mobile.rs`; preserve all success DTOs and remote-action semantics. Run their tests, then commit:

```bash
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/routes/orchestrator.rs src-tauri/src/net/routes/mobile.rs
git commit -m "refactor: standardize orchestrator mobile errors"
```

- [ ] **Step 7: Run all route tests**

```bash
cd src-tauri
cargo test --locked net::routes:: -- --nocapture
```

Expected: all error responses use v1 envelope, while success payloads remain unchanged. `git status --short` has no uncommitted route migration.

---

### Task 7: Unify Client Error Parsing and Capability Gates

**Files:**
- Create: `src-tauri/src/net/peer_error.rs`
- Modify: `src-tauri/src/net/mod.rs`
- Modify: `src-tauri/src/net/peer_client.rs`
- Modify: `src-tauri/src/orchestrator/remote_client.rs`

- [ ] **Step 1: Write v0/v1 parser contract tests**

Fixtures must cover a complete v1 envelope, legacy `{ "error": "旧错误" }`, invalid JSON, timeout/network error and an error status with empty body. Legacy bodies become `PeerCallError::Remote` with a synthesized code `legacy.remote_error`, not string-parsed business decisions.

- [ ] **Step 2: Implement shared response parsing**

The parser consumes status, response header request ID and body bytes once. For v1, header/body IDs must match or produce `InvalidResponse`. For v0, retain the header ID if present and synthesize missing structured fields.

- [ ] **Step 3: Add one reusable capability gate**

```rust
pub async fn require_capability(
    &self,
    base_url: &str,
    capability: &'static str,
) -> Result<HealthResponse, PeerCallError>;
```

It always checks authoritative health metadata and returns `Unsupported` without calling the feature route.

- [ ] **Step 4: Migrate Orchestrator remote client first**

Replace its local `error` string extraction with `peer_error`. Keep every existing remote route path and success DTO unchanged. Add a server hit counter proving a missing capability stops before the new route.

- [ ] **Step 5: Run client tests and commit**

```bash
cd src-tauri
cargo test --locked net::peer_error::tests
cargo test --locked net::peer_client::tests
cargo test --locked orchestrator::remote_client::tests
git -C "$(git rev-parse --show-toplevel)" add src-tauri/src/net/peer_error.rs src-tauri/src/net/mod.rs src-tauri/src/net/peer_client.rs src-tauri/src/orchestrator/remote_client.rs
git commit -m "feat: parse p2p errors and gate capabilities"
```

---

### Task 8: Publish the Idempotency Risk Inventory

**Files:**
- Create: `docs/p2p-protocol.md`
- Create: `scripts/check-p2p-route-inventory.mjs`
- Modify: `src-tauri/CLAUDE.md`

- [ ] **Step 1: Inventory every route registered in `http_server.rs`**

Create a table with method, path, owner module, side effect, retry class and key/guard. Every route must be assigned exactly one class: `read-only`, `naturally-idempotent`, `requires-idempotency-key`, or `no-transport-retry`.

- [ ] **Step 2: Apply the minimum mandatory classifications**

- Orchestrator create: `requires-idempotency-key`, preserving `clientRequestId` and existing repository transaction.
- worktree/session/transfer-init/preview creation: `requires-idempotency-key` pending separate domain implementation; clients do not auto-retry until implemented.
- vector-clock sync push, offset-aware transfer chunks, resize/focus and base-hash save: `naturally-idempotent` only after the implementation is verified in code and cited in the table.
- terminal write, commit, push, merge, remove/delete/close and Orchestrator actions: `no-transport-retry`.

- [ ] **Step 3: Add the protocol change checklist to `src-tauri/CLAUDE.md`**

Require capability constant, health declaration, legacy contract test, typed error code, request ID, inventory row and explicit retry policy for every new route.

- [ ] **Step 4: Verify the route inventory is complete**

Implement `scripts/check-p2p-route-inventory.mjs` with Node built-ins only. It extracts literal `/api/` paths from `src-tauri/src/net/http_server.rs`, extracts the path column from `docs/p2p-protocol.md`, prints missing/extra entries and exits `1` on mismatch. Dynamic wildcard/suffix routes are represented by the exact literal prefix used in the router and documented identically.

- [ ] **Step 5: Commit**

```bash
node scripts/check-p2p-route-inventory.mjs
git -C "$(git rev-parse --show-toplevel)" add docs/p2p-protocol.md scripts/check-p2p-route-inventory.mjs src-tauri/CLAUDE.md
git commit -m "docs: define p2p compatibility and retry policy"
```

---

### Task 9: Final Protocol Verification

- [ ] **Step 1: Run the complete focused suite**

```bash
cd src-tauri
cargo test --locked net:: -- --nocapture
cargo test --locked orchestrator::remote_client::tests
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

- [ ] **Step 2: Verify compatibility invariants**

Run v0 and v1 fixture servers. Confirm v0 health remains readable, v1 capabilities are authoritative, a missing capability prevents a new route call, old `{error}` remains user-readable, and request ID is identical in client error, response body/header and server span.

- [ ] **Step 3: Check prohibited shortcuts**

```bash
rg -n "contains\(.*error|error.*contains|continue-on-error" src-tauri/src/net src-tauri/src/orchestrator/remote_client.rs
```

Review every match; no business decision may parse localized error text and no protocol test may be made non-blocking.

## Completion Contract

- `/api/health` returns protocol v1 and complete capabilities; absent fields remain valid legacy v0.
- mDNS hints are sorted, token-complete and at most 220 UTF-8 bytes, but never used as the final gate.
- All P2P errors have stable status/code/request ID/retryability/details while legacy client responses remain readable.
- Tauri IPC error behavior is unchanged.
- New capabilities cannot be called before authoritative capability verification.
- Every registered P2P route appears in the idempotency inventory; unsafe writes have no automatic transport retry.
