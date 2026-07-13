# LAN Trust Boundary Hardening Implementation Plan

> 实施基线：产品只有一种固定 LAN 语义。合法 loopback/LAN peer 无需身份凭据，P2P、Mobile、Workbench、Orchestrator 的全部业务查询、写入与执行动作直接允许。

**Goal:** 在不增加账号、配对、token、session、设备身份或读写权限分级的前提下，以真实 socket peer、Host/Origin/WebSocket guard、现有资源上限和准确风险提示，把 HTTP 暴露边界限制在产品支持的本机/LAN 范围。

**Architecture:** 保留现有单个 wildcard listener、首选端口 62116 与占用递增模型；axum 使用 `ConnectInfo<SocketAddr>` 获取真实 peer，先执行 LAN scope gate，再执行 Host/Origin/Content-Type guard。普通业务 API 对合法 loopback/LAN peer 一律放行；backend stop 单独要求 loopback + 既有 control-file token。Workbench preview registry 继续约束 proxy 目标，并为不含 `allow-same-origin` 的 opaque iframe 精确兼容 `Origin: null`。资源层复用当前 32 MiB 全局上限和现有领域常量，不建立全路由策略矩阵。

**Tech Stack:** Rust 2021、Tauri 2、axum 0.7、tokio、reqwest、mdns-sd、React 19、TypeScript、Vitest、Playwright。

## Global Constraints

- 开始前读取根 `AGENTS.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md` 与 `docs/p2p-protocol.md`；新增业务函数和组件遵守中文 Business Logic / Code Logic 注释规范。
- LAN 业务 API 不增加任何 credential、配对、设备登记、签名、cookie 登录态或逐设备授权。
- 不实现任何可切换的 LAN 暴露/只读模式；不增加 mode config、runtime、migration、UI 或 capability token。
- 不创建读写权限分类、只读 gate 或全路由 browser/body/timeout/concurrency 策略矩阵。
- socket gate 只信 `ConnectInfo<SocketAddr>`，忽略 `Forwarded`、`X-Forwarded-For`、`X-Real-IP`。
- 支持范围仅为 loopback、RFC1918、IPv4 link-local、IPv6 ULA、IPv6 link-local；IPv4-mapped IPv6 先规范化。
- 当前 wildcard listener 可以保留，但 UI/doctor 必须如实显示；不得宣称 socket 只绑定 LAN interface。
- 普通浏览器 API 的 Host/Origin 失败必须拒绝；native P2P 无 Origin 保持兼容。
- preview iframe 继续不包含 `allow-same-origin`；只允许有效 preview session 的 proxy path 使用 opaque `Origin: null`，其它 API 的 null Origin 一律拒绝。
- backend stop 必须同时要求 loopback peer 和既有 control-file token；token 不扩散到业务 API。
- 保留全局 32 MiB、transfer 960 KiB、文本 5 MiB、preview proxy 32 MiB 等现有上限；不复制成全路由目录。
- 日志不记录 Prompt、终端内容、文件正文、Cookie、Authorization 或完整 query。
- 不新增 LAN 权限 capability；新旧 native peer 继续无凭据互操作。
- 每个 task 都必须遵循失败证据 → 最小实现 → focused/影响面验证 → self-review → 独立提交，不得跨 task broad stage。

## Dependency Graph

```text
T1 Socket scope + stop isolation
 ├── T2 Host / Origin / WebSocket guard
 └── T3 Resource-limit regression
          T1 + T2 + T3
                ↓
         T4 UI + doctor risk
                ↓
         T5 Protocol + docs alignment
                ↓
         T6 Integration + cross-platform evidence
```

最大并行 wave：`T1 → (T2 | T3) → T4 → T5 → T6`。

## Exact Middleware Order

```text
ConnectInfo → request id → LAN socket gate → Host/Origin/Content-Type guard
→ existing global/domain resource limits → handler → error-envelope fallback
```

preview proxy 的 `Origin: null` 最终判断必须发生在现有 registry 成功查到 preview session 之后；全局 guard 只能识别 proxy namespace，不能把 null Origin 普遍放行。

### Task 1: Enforce LAN Socket Scope and Isolate Backend Stop

**Files:**

- Create `src-tauri/src/net/lan_guard.rs`。
- Modify `src-tauri/src/net/mod.rs`。
- Modify `src-tauri/src/net/http_server.rs`。
- Modify `src-tauri/src/net/error_response.rs`。

**New contracts to implement:**

- Internal `LanPeerScope { Loopback, Lan, Denied }`；不是用户配置或权限模式。
- Pure peer classifier that accepts `IpAddr`, normalizes IPv4-mapped IPv6, and returns `LanPeerScope`。
- axum middleware that extracts the production `ConnectInfo<SocketAddr>` and rejects `Denied`/missing peer before handlers。
- backend stop peer check that requires `Loopback` before the existing token comparison。

- [ ] **Step 1: Write failing pure and middleware tests**

Add table tests for every supported boundary and representative denied addresses. Add tests proving forwarded headers do not change scope, a denied peer cannot reach a probe handler, ordinary LAN business requests reach the handler, and backend stop requires both loopback and the current token.

Required test names:

- `classify_peer_ip_covers_supported_and_denied_ranges`
- `forwarded_headers_never_change_socket_scope`
- `lan_socket_gate_rejects_denied_peer_before_handler`
- `lan_socket_gate_allows_loopback_and_lan_business_requests`
- `backend_stop_rejects_non_loopback_even_with_valid_token`

- [ ] **Step 2: Capture red evidence**

```bash
cd src-tauri
cargo test --locked net::lan_guard::tests --lib
cargo test --locked net::http_server::tests::backend_stop_rejects_non_loopback_even_with_valid_token --lib
```

Expected: FAIL because `lan_guard` and connect-info enforcement are not wired.

- [ ] **Step 3: Implement the gate**

Use the existing Router and listener; change the serve boundary to provide `ConnectInfo<SocketAddr>`. Install the gate outside all `/api`, `/mobile`, static and preview handlers. Ignore all proxy source headers. Map denial to the existing error envelope with HTTP 403 and a stable non-authentication code/message. Update stop handling so loopback is checked before reading/comparing the control token.

Do not add configuration fields, mode state, route effects or credentials.

- [ ] **Step 4: Verify focused behavior**

```bash
cd src-tauri
cargo test --locked net::lan_guard::tests --lib
cargo test --locked net::http_server::tests --lib
cargo test --locked net::error_response::tests --lib
```

Expected: PASS; legal LAN writes remain credential-free, denied peers never reach handlers, and stop requires loopback + token.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/lan_guard.rs src-tauri/src/net/mod.rs src-tauri/src/net/http_server.rs src-tauri/src/net/error_response.rs
git commit -m "feat: enforce fixed lan socket boundary"
```

### Task 2: Guard Browser HTTP and WebSocket Requests

**Depends on:** T1.

**Files:**

- Modify `src-tauri/src/net/lan_guard.rs`。
- Modify `src-tauri/src/net/http_server.rs`。
- Modify `src-tauri/src/workbench/browser_proxy.rs`。
- Modify `src-tauri/src/net/error_response.rs` only if a stable existing envelope mapping is missing。

**Rules to implement:**

- Validate Host against supported literal IPs, `localhost`, and the current process's controlled `.local` hostname; require the actual HTTP port.
- Ordinary API: absent Origin is valid for native clients; present Origin must exactly match `http://<Host>`; `Origin: null` is rejected.
- Ordinary API WebSocket: same rule as ordinary API.
- Preview proxy: cross-site non-null Origin is rejected; `Origin: null` is allowed only after the existing registry resolves a live previewId under the desktop/mobile proxy prefixes.
- Preserve the current preview iframe sandbox without `allow-same-origin`.
- Reject form, multipart and `text/plain` writes for ordinary APIs; do not impose this restriction on preview proxy payloads.
- Do not emit wildcard/reflected CORS or credential CORS headers.

- [ ] **Step 1: Write the hostile-browser matrix first**

Required tests:

- valid LAN IP/loopback/controlled `.local` Host with actual port succeeds;
- arbitrary Host and wrong port fail;
- same-origin mobile write succeeds;
- cross-origin and ordinary API `Origin: null` fail;
- native P2P without Origin succeeds;
- cross-origin WebSocket fails;
- valid previewId accepts opaque `Origin: null` HTTP and WebSocket;
- unknown/expired previewId with `Origin: null` fails;
- preview null-origin exception cannot access other `/api/*` routes;
- form/multipart/`text/plain` ordinary writes fail;
- preview iframe sandbox test continues to prove `allow-same-origin` is absent。

- [ ] **Step 2: Capture red evidence**

```bash
cd src-tauri
cargo test --locked net::lan_guard::tests::browser_request_matrix --lib
cargo test --locked workbench::browser_proxy::tests::opaque_origin_matrix --lib
cd ../web
npm test -- src/pages/Workbench/workbenchBrowserPreview.test.ts
```

Expected: Rust tests FAIL because Host/Origin enforcement is absent; the existing iframe sandbox regression remains green.

- [ ] **Step 3: Implement global and preview-specific checks**

Build Host decisions from the request Host, actual port, supported IP classifier and current device's controlled mDNS name; never learn allowed hosts from requests. Apply ordinary API Origin/Content-Type rules in `lan_guard`.

For preview proxy, keep global rejection for cross-site non-null Origin, but defer null-origin acceptance until `lookup_preview_or_not_found` succeeds. Apply the same session-aware decision before WebSocket upstream connection. Do not forward a rejected request upstream and do not add `allow-same-origin` to the iframe.

- [ ] **Step 4: Verify compatible clients**

```bash
cd src-tauri
cargo test --locked net::lan_guard::tests --lib
cargo test --locked workbench::browser_proxy::tests --lib
cargo test --locked net::peer_client::tests --lib
cd ../web
npm test -- src/pages/Workbench/workbenchBrowserPreview.test.ts src/mobile/mobileBrowserPanel.test.ts
```

Expected: PASS; same-origin mobile, no-Origin native P2P and opaque preview remain functional while ordinary null/cross-origin requests fail.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/lan_guard.rs src-tauri/src/net/http_server.rs src-tauri/src/workbench/browser_proxy.rs src-tauri/src/net/error_response.rs web/src/components/domain/WorkbenchBrowserWorkspace
git commit -m "feat: guard lan browser request origins"
```

### Task 3: Lock Existing Global and Heavy-entry Resource Limits

**Depends on:** T1. May run in parallel with T2.

**Files:**

- Modify `src-tauri/src/net/http_server.rs`。
- Modify `src-tauri/src/net/routes/transfer.rs` only if route-level chunk enforcement is needed to reject before receiver work。
- Modify `src-tauri/src/transfer/receiver.rs` tests only as needed for boundary coverage。
- Modify `src-tauri/src/workbench/file_content.rs` tests only as needed for boundary coverage。
- Modify `src-tauri/src/workbench/browser_proxy.rs` tests only as needed for boundary coverage。

**Boundaries to preserve:**

- Existing `BODY_LIMIT_BYTES = 32 MiB` outer limit。
- Existing `CHUNK_SIZE = 960 KiB` transfer limit; reject `CHUNK_SIZE + 1` before disk mutation。
- Existing `MAX_EDITABLE_TEXT_BYTES = 5 MiB` domain limit; JSON escaping must still fit below the 32 MiB outer ceiling。
- Existing `PROXY_BODY_LIMIT_BYTES = 32 MiB` preview limit and no-follow redirect behavior。
- Existing SQLite/HTML/image/CSV limits and remote request timeout classes remain owned by their current modules。

- [ ] **Step 1: Add boundary tests before changing enforcement**

Test exact limit and limit+1 for transfer chunk and preview proxy; verify oversized file save does not overwrite the target; verify global 413 is wrapped by the existing error envelope; verify the escaped 5 MiB text-save payload still fits the outer limit.

- [ ] **Step 2: Run focused red/green baseline**

```bash
cd src-tauri
cargo test --locked net::http_server::tests --lib
cargo test --locked transfer::receiver::tests --lib
cargo test --locked workbench::file_content::tests --lib
cargo test --locked workbench::browser_proxy::tests::http_proxy_rejects_oversized_request_body_without_forwarding --lib
```

Expected: existing green tests stay green; any new early-rejection gap fails before implementation.

- [ ] **Step 3: Close only demonstrated gaps**

Keep `DefaultBodyLimit` as the single outer limit. If the transfer route currently accepts more than `CHUNK_SIZE` before receiver validation, add a route-local body limit for that one endpoint. Preserve domain constants rather than copying them into a new policy table. Do not add per-route effect, timeout, retry or semaphore registries.

- [ ] **Step 4: Verify no regression or payload logging**

```bash
cd src-tauri
cargo test --locked net::http_server::tests --lib
cargo test --locked transfer::receiver::tests --lib
cargo test --locked workbench::file_content::tests --lib
cargo test --locked workbench::browser_proxy::tests --lib
rg -n "request_body|terminal_content|prompt_content|full_query" src/net src/backend/logging.rs
```

Expected: all boundary tests PASS and production code contains no payload logging.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/net/http_server.rs src-tauri/src/net/routes/transfer.rs src-tauri/src/transfer/receiver.rs src-tauri/src/workbench/file_content.rs src-tauri/src/workbench/browser_proxy.rs
git commit -m "test: lock lan resource boundaries"
```

### Task 4: Show the Fixed LAN Risk in Existing UI and Doctor Surfaces

**Depends on:** T1, T2 and T3.

**Files:**

- Modify `web/src/components/domain/MobileAccessCard/MobileAccessCard.tsx` only if markup is needed; reuse its existing warning area。
- Modify `web/src/components/domain/MobileAccessCard/mobileAccessCard.test.ts`。
- Modify `web/src/components/domain/LanFirewallDependencyCard/LanFirewallDependencyCard.tsx` only if markup is needed; reuse existing Card/Pill/tokens。
- Modify `web/src/i18n/locales/zh/settings.json`。
- Modify `web/src/i18n/locales/en/settings.json`。
- Modify `src-tauri/src/backend/cli.rs` and its existing doctor text tests。

No LAN mode panel/helper/DTO, mobile write guard or new Settings control may be created.

- [ ] **Step 1: Write wording and rendering tests**

Assert the Chinese UI/doctor output contains the fixed meaning “同一可达网络中的任何设备均可读取、写入和执行；系统不验证调用者身份”。Assert MobileAccessCard still shows QR/URLs whenever valid LAN URLs exist. Assert LanFirewallDependencyCard still shows LAN IP, actual HTTP port and UDP 5353. Assert no text uses “安全”“已认证”“可信设备” or mode names.

- [ ] **Step 2: Capture red evidence**

```bash
cd web
npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts
cd ../src-tauri
cargo test --locked backend::cli::tests::doctor_text_fixture --lib
```

Expected: FAIL until fixed risk wording is present.

- [ ] **Step 3: Update existing surfaces only**

Replace the current vague “可信局域网” wording with the explicit no-identity warning in Chinese and equivalent English. Keep existing URL/QR/firewall behavior. In doctor text, print the actual port already present in the snapshot, state that the listener is wildcard when the current server remains `0.0.0.0`, and append the fixed risk statement. Never print the control token.

- [ ] **Step 4: Verify frontend and CLI**

```bash
cd web
npm test -- src/components/domain/MobileAccessCard/mobileAccessCard.test.ts src/lib/lanFirewallDependency.test.ts
npm run lint
npm run build
cd ../src-tauri
cargo test --locked backend::cli::tests --lib
cargo test --locked backend::doctor::tests --lib
```

Expected: PASS with no new component, no hard-coded CSS values and no mode UI.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/domain/MobileAccessCard web/src/components/domain/LanFirewallDependencyCard web/src/i18n/locales/zh/settings.json web/src/i18n/locales/en/settings.json src-tauri/src/backend/cli.rs
git commit -m "docs: show fixed unauthenticated lan risk"
```

### Task 5: Align Protocol Inventory, Product Docs and Operator Guidance

**Depends on:** T1–T4.

**Files:**

- Read/verify `scripts/check-p2p-route-inventory.mjs`; modify it only if the existing method/path inventory actually drifts。
- Modify `docs/p2p-protocol.md`。
- Modify `docs/prd.md`。
- Modify `docs/development/backend-operations.md`。
- Modify `docs/development/testing.md`。
- Modify `README.md`。
- Modify `AGENTS.md`。
- Modify `src-tauri/CLAUDE.md`。
- Modify `web/CLAUDE.md`。

Do not add or modify a LAN permission capability in `src-tauri/src/net/protocol.rs`.

- [ ] **Step 1: Add docs/inventory assertions**

Extend existing documentation checker self-tests so configurable LAN modes, route authorization matrices, LAN permission capability metadata and claims that LAN peers are authenticated are rejected. Assert the fixed no-identity risk statement is present in PRD/operator guidance and that firewall docs use UDP 5353 plus actual TCP port.

- [ ] **Step 2: Capture red evidence**

```bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-docs.mjs
```

Expected: documentation check FAILS until stale mode language is removed.

- [ ] **Step 3: Update persistent product and protocol documentation**

Document the single LAN behavior, supported IP ranges, wildcard listener + socket gate, Host/Origin rules, preview null-origin exception, stop lifecycle exception, resource limits and remaining risk. Keep existing route retry/idempotency tables; do not add authorization/effect columns. State that old/new native peers continue without credentials and no capability negotiation is required.

- [ ] **Step 4: Verify documentation and protocol inventory**

```bash
node scripts/check-p2p-route-inventory.mjs
node scripts/check-docs.mjs
node scripts/check-docs.mjs --self-test
```

Expected: scripts and negative fixtures PASS; no stale configurable-mode or route-authorization contract remains.

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md docs/p2p-protocol.md docs/prd.md docs/development/backend-operations.md docs/development/testing.md scripts/check-docs.mjs src-tauri/CLAUDE.md web/CLAUDE.md
git commit -m "docs: define fixed unauthenticated lan boundary"
```

### Task 6: Complete Integration, Cross-platform and Real-device Evidence

**Depends on:** T1–T5.

**Files:**

- Create `src-tauri/tests/lan_trust_boundary_smoke.rs` for real HTTP behavior that can be exercised through a bound test server。
- Modify `.github/workflows/cross-platform-smoke.yml`。
- Modify `docs/development/testing.md` with the final evidence matrix。

Browser L1 coverage remains owned by S6 `web/tests/lan-boundary.spec.ts`; this task must not create a duplicate Playwright journey.

- [ ] **Step 1: Write integration failures before fixtures**

Cover:

- credential-free loopback/LAN business read and write success;
- denied/public classifier and forwarded spoof evidence without trusting a production HTTP header;
- valid Host/port, hostile Host, same-origin mobile, cross-origin/null-origin ordinary API;
- valid preview session opaque `Origin: null` HTTP/WebSocket and invalid preview rejection;
- stop loopback/token combinations;
- global/transfer/file/proxy resource boundaries;
- native no-Origin interoperability。

Injected peer addresses are acceptable for pure/middleware tests but must be labeled as injected evidence, not a real network result.

- [ ] **Step 2: Run the new suite and capture red evidence**

```bash
cd src-tauri
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
```

Expected: selected cases FAIL until all Tasks are integrated.

- [ ] **Step 3: Add cross-platform and manual evidence**

Run Rust smoke on macOS、Windows、Ubuntu. Record a manual checklist for two machines and one phone covering mDNS discovery, Mobile QR, credential-free P2P/Workbench/Orchestrator writes, Browser Preview HTTP/WebSocket, actual port fallback and firewall profile. Do not simulate a remote public peer with `X-Forwarded-For` and call it production evidence.

- [ ] **Step 4: Run final quality gates**

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --locked --test lan_trust_boundary_smoke -- --nocapture --test-threads=1
cd ../web
npm run lint
npm run build
npm test
npm run test:e2e
cd ..
node scripts/check-p2p-route-inventory.mjs
node scripts/check-docs.mjs
git diff --check
```

Expected: all automated commands PASS. Real-device items not actually executed are recorded as `NOT VERIFIED`.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/lan_trust_boundary_smoke.rs .github/workflows/cross-platform-smoke.yml docs/development/testing.md
git commit -m "test: verify fixed lan trust boundary"
```

## Completion Contract

This plan is complete only when all of the following are true:

- 合法 loopback/LAN peer 不携带身份凭据即可完成现有 P2P、Mobile、Workbench 和 Orchestrator 的查询、写入与执行。
- 产品和代码中不存在 LAN 暴露模式、mode config/runtime/UI/migration、只读 gate、第二套路由权限分类或 LAN 权限 capability token。
- 真实 socket peer 是 LAN scope 的唯一来源；forwarded headers 不能改变决策；denied peer 在 handler 前失败。
- Host、Origin、Content-Type 与 WebSocket hostile matrix 通过；普通 API 拒绝 `Origin: null`，有效 preview session 的 opaque null-origin HTTP/WebSocket 保持可用，iframe 仍无 `allow-same-origin`。
- backend stop 同时要求 loopback peer 与既有 control-file token，且 token 未扩散到业务 API、health、mDNS、UI、doctor 或日志。
- 32 MiB 全局、960 KiB transfer chunk、5 MiB 文本、32 MiB preview proxy 及其它现有领域上限有回归证据，且没有全路由预算矩阵。
- MobileAccessCard、LanFirewallDependencyCard、doctor、PRD 和运维文档准确说明“同一可达网络任何设备可读写执行，系统不验证身份”，并展示实际端口/mDNS 信息。
- 未新增 LAN 权限 capability；新旧 native peer 无凭据互操作；协议 route/retry inventory 保持一致。
- macOS、Windows、Ubuntu 自动化结果被记录；真实双机和手机项目未执行时明确标记 `NOT VERIFIED`。
