# macOS Input Monitoring Root Fix Implementation Plan

> **For agentic workers:** 按任务顺序实施，并对行为改动执行 test-first 的 Red/Green/Refactor；当前环境未提供 executing-plans/subagent-driven-development 时，由主 agent 在当前分支内联执行。步骤使用 checkbox（`- [ ]`）追踪。

**Goal:** 为内部 macOS 构建建立稳定自签名代码身份，并把输入监控改为只使用公开 IOHID API、显式分离 Request/Settings/Reopen 的可验证权限流。

**Architecture:** 内部稳定版与开发版使用独立 Bundle ID 和同一固定 Code Signing identity；社区构建保持可构建但权限主体 fail closed。Rust 将输入监控状态映射与系统 FFI 分层，前端消费 `granted|denied|notDetermined|unavailable`，发布脚本与 CI 对 ad-hoc、错误证书和错误 Bundle ID fail closed。

**Tech Stack:** Rust/Tauri 2、macOS IOKit/CoreGraphics/ApplicationServices、React 19/TypeScript/Vitest、Node.js scripts、GitHub Actions、codesign/security。

## Global Constraints

- 生产权限路径不得调用私有 `TCCAccessPreflight`、`tccutil` 或运行时 `codesign`。
- 输入监控只以 `IOHIDCheckAccess/IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)` 为权威。
- 内部稳定 Bundle ID 固定 `com.cc-partner.app.internal`；内部开发 Bundle ID 固定 `com.cc-partner.app.internal.dev`。
- 内部签名失败不得回退 ad-hoc；社区构建必须返回 `unavailable`，不得假装可授权。
- Request、Open Settings、Reopen 必须是三条独立用户动作；启动和回前台只查询。
- Rust 测试必须使用隔离数据目录，不得写真实 `~/.cc-partner` 或修改宿主 TCC。
- 前端 Hooks 必须位于所有 early return 之前；所有新文案走 i18n。
- 无 Developer ID 期间，公开 workflow 不把 macOS ad-hoc 包标为正式支持。

---

## File Structure

- `src-tauri/src/permissions/input_monitoring.rs`：输入监控状态、受支持 Bundle 身份、IOHID provider 与公开 Request。
- `src-tauri/src/permissions/mod.rs`：其它权限、统一 DTO、设置跳转和 relaunch；删除私有 TCC/pending/reset/重签逻辑。
- `src-tauri/src/commands/permissions.rs`：主线程 Request、独立 Open Settings、状态查询命令。
- `src-tauri/src/lib.rs`：注册新命令，移除 pending 冷启动副作用。
- `web/src/lib/types/core.ts`、`web/src/lib/schemas/config.ts`：输入监控状态与权限操作结果合同。
- `web/src/api/config.ts`、`web/src/hooks/usePermissions.ts`：分离 Request/Open Settings/Reopen。
- `web/src/pages/Welcome/*`、`web/src/lib/permissionEntries.tsx`、`web/src/components/domain/PermissionCard/*`：四态 UI 与动态动作文案。
- `src-tauri/tauri.internal.conf.json`：内部稳定版 Tauri overlay。
- `scripts/prepare-macos-dev-app.mjs`：有固定 identity 时生成内部 Dev；无 identity 时生成社区 Dev。
- `scripts/check-macos-signing-contract.mjs`：签名合同验证及 self-test。
- `scripts/build-macos-internal.sh`：本地内部包入口。
- `.github/workflows/internal-macos.yml`：受保护、手动触发的内部 macOS 构建。
- `.github/workflows/release-tauri.yml`：公开稳定发布移除 macOS ad-hoc 正式产物。
- `docs/development/macos-internal-signing.md`：证书创建、信任、构建、迁移和 L3 操作手册。

---

### Task 1: 输入监控公开状态模型

**Files:**
- Create: `src-tauri/src/permissions/input_monitoring.rs`
- Modify: `src-tauri/src/permissions/mod.rs`
- Test: `src-tauri/src/permissions/input_monitoring.rs`

**Interfaces:**
- Produces: `InputMonitoringState`, `InputMonitoringPermissionState`, `check_input_monitoring_state()`, `request_input_monitoring_access()`。
- Consumes: macOS `IOHIDCheckAccess` / `IOHIDRequestAccess`；非 macOS返回 `Granted`。

- [ ] **Step 1: 写状态映射失败测试**

```rust
#[test]
fn maps_iohid_state_only_for_supported_subject() {
    assert_eq!(state_from_raw(false, 0), InputMonitoringState::Unavailable);
    assert_eq!(state_from_raw(true, 0), InputMonitoringState::Granted);
    assert_eq!(state_from_raw(true, 1), InputMonitoringState::Denied);
    assert_eq!(state_from_raw(true, 2), InputMonitoringState::NotDetermined);
    assert_eq!(state_from_raw(true, 99), InputMonitoringState::Unavailable);
}

#[test]
fn accepts_only_internal_bundle_ids() {
    assert!(is_supported_subject(Some("com.cc-partner.app.internal")));
    assert!(is_supported_subject(Some("com.cc-partner.app.internal.dev")));
    assert!(!is_supported_subject(Some("com.cc-partner.app")));
    assert!(!is_supported_subject(None));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked permissions::input_monitoring::tests -- --nocapture`

Expected: FAIL，模块/类型尚不存在。

- [ ] **Step 3: 实现最小状态模型与 provider**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputMonitoringState {
    Granted,
    Denied,
    NotDetermined,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMonitoringPermissionState {
    pub granted: bool,
    pub state: InputMonitoringState,
}

pub const INTERNAL_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app.internal";
pub const INTERNAL_DEV_BUNDLE_IDENTIFIER: &str = "com.cc-partner.app.internal.dev";

fn state_from_raw(supported: bool, raw: u32) -> InputMonitoringState {
    if !supported {
        return InputMonitoringState::Unavailable;
    }
    match raw {
        0 => InputMonitoringState::Granted,
        1 => InputMonitoringState::Denied,
        2 => InputMonitoringState::NotDetermined,
        _ => InputMonitoringState::Unavailable,
    }
}
```

`request_input_monitoring_access()` 必须先读 before；仅 `NotDetermined` 调一次 IOHID Request；再读 after。Denied/Granted/Unavailable 返回 noop，不调用 Request。

- [ ] **Step 4: 删除旧输入监控私有/补偿路径**

从 `permissions/mod.rs` 删除：`TCCAccessPreflight`、ListenEvent TCC preflight、`reset_listen_event_tcc*`、pending/rotated marker、`rotate_dev_adhoc_codesign`、输入监控 CG Request 聚合和诊断入口。保留 `check_input_monitoring_access()` 兼容调用，但实现为 `check_input_monitoring_state().granted`。

- [ ] **Step 5: 运行 Rust 权限测试**

Run: `cd src-tauri && cargo test --locked permissions:: -- --nocapture`

Expected: PASS，且输出不包含 `tccutil reset`、`codesign` 或真实 data_dir 写入。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/permissions/input_monitoring.rs src-tauri/src/permissions/mod.rs
git commit -m "fix: make IOHID authoritative for input monitoring"
```

---

### Task 2: 分离权限 IPC 与启动副作用

**Files:**
- Modify: `src-tauri/src/commands/permissions.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/permissions/mod.rs`
- Test: `src-tauri/src/permissions/mod.rs`

**Interfaces:**
- Consumes: Task 1 `request_input_monitoring_access()`。
- Produces: `request_permission(type)`, `open_permission_settings(type)`, `relaunch_for_permissions()`。

- [ ] **Step 1: 写操作隔离失败测试**

为 provider 加测试 double，断言：

```rust
#[test]
fn denied_request_is_noop_and_never_opens_settings() {
    let provider = FakeProvider::new(1);
    let result = request_with_provider(&provider, true);
    assert_eq!(result.operation, PermissionOperation::Noop);
    assert_eq!(provider.request_calls(), 0);
}
```

设置 URL 解析另测为纯函数，Request 测试不得 spawn `open`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd src-tauri && cargo test --locked denied_request_is_noop_and_never_opens_settings -- --nocapture`

Expected: FAIL，操作枚举/注入 seam 尚不存在。

- [ ] **Step 3: 调整 DTO 与命令**

```rust
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionOperation { Request, OpenSettings, Noop }

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionActionResult {
    pub permission: String,
    pub operation: PermissionOperation,
    pub before: String,
    pub after: String,
}
```

`request_permission` 移除 `open_settings` 参数。输入监控通过 `run_on_main_thread` 调 Task 1 Request；其它权限仅调用各自公开 Request。新增 `open_permission_settings(type)` 只 spawn 系统设置 URL。

- [ ] **Step 4: 移除启动 pending 与诊断环境副作用**

从 `lib.rs` 删除 `CC_PARTNER_IM_REQUEST`、legacy register arg、`consume_pending_input_monitoring_request()`。setup 不再在 health daemon 前执行权限 mutation。

启动时仅删除应用自有两个 legacy marker；该清理用隔离 data_dir 单测证明，不调用系统命令。

- [ ] **Step 5: 注册并验证命令**

Run: `cd src-tauri && cargo test --locked permissions:: commands::permissions -- --nocapture`

Expected: PASS。

Run: `cd src-tauri && cargo check --locked --lib`

Expected: PASS，无未注册命令/类型错误。

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/commands/permissions.rs src-tauri/src/lib.rs src-tauri/src/permissions
git commit -m "refactor: separate permission request and settings actions"
```

---

### Task 3: 前端四态权限流

**Files:**
- Modify: `web/src/lib/types/core.ts`
- Modify: `web/src/lib/schemas/config.ts`
- Modify: `web/src/lib/schemas/config.test.ts`
- Modify: `web/src/api/config.ts`
- Modify: `web/src/hooks/usePermissions.ts`
- Modify: `web/src/hooks/usePermissions.test.tsx`
- Modify: `web/src/lib/permissionEntries.tsx`
- Modify: `web/src/lib/permissionEntries.test.ts`
- Modify: `web/src/components/domain/PermissionCard/PermissionCard.tsx`
- Modify: `web/src/pages/Welcome/Welcome.tsx`
- Modify: `web/src/pages/Welcome/Welcome.test.tsx`
- Modify: `web/src/pages/Settings/controllers/useSettingsUpdatePermissions.ts`
- Modify: `web/src/i18n/locales/zh/welcome.json`
- Modify: `web/src/i18n/locales/en/welcome.json`

**Interfaces:**
- Consumes: Task 2 IPC 和 `inputMonitoring.state`。
- Produces: `request(type)`, `openSettings(type)`, `relaunch()` 三个前端动作。

- [ ] **Step 1: 写 decoder 与 hook 失败测试**

```ts
expect(permissionsStatusDecoder.decode({
  screenCapture: { granted: false },
  inputMonitoring: { granted: false, state: 'notDetermined' },
  accessibility: { granted: false },
  notification: { granted: false },
}).inputMonitoring.state).toBe('notDetermined')
```

Hook 测试分别断言 `request('inputMonitoring')` 只 invoke `request_permission`，`openSettings('inputMonitoring')` 只 invoke `open_permission_settings`。

- [ ] **Step 2: 运行测试确认失败**

Run: `cd web && npm test -- src/lib/schemas/config.test.ts src/hooks/usePermissions.test.tsx`

Expected: FAIL，state/独立动作尚不存在。

- [ ] **Step 3: 更新类型、schema、API 与 hook**

```ts
export type InputMonitoringState =
  | 'granted'
  | 'denied'
  | 'notDetermined'
  | 'unavailable'

export interface InputMonitoringPermissionState {
  granted: boolean
  state: InputMonitoringState
}
```

删除 `openSettings?: boolean` 参数和 `requestMissing()` 批量副作用。新增 `openSettings(type)`；relaunch 仍只由 Welcome 显式按钮使用。

- [ ] **Step 4: 写 Welcome 四态失败测试**

分别渲染 inputMonitoring 四种状态并断言按钮：请求授权、打开系统设置、已授权、查看构建说明。点击 denied 时不得调用 request；点击 notDetermined 时不得 open settings。

- [ ] **Step 5: 实现动态 PermissionEntry 与 Welcome**

`PermissionEntry` 增加 `action: request|openSettings|none|buildHelp` 与 `actionLabel`。PermissionCard 接收父级文案，不能再把所有未授权项写死为“去设置”。

删除输入监控的 `BACKEND_NEEDS_RELAUNCH`/pending 专用相位；保留用户从设置返回后的查询与显式 reopen 提示。`unavailable` 展示内部签名 `.app` 说明且不触发权限 IPC。

- [ ] **Step 6: 运行前端定向验证**

Run: `cd web && npm test -- src/lib/schemas/config.test.ts src/hooks/usePermissions.test.tsx src/lib/permissionEntries.test.ts src/pages/Welcome`

Expected: PASS。

Run: `cd web && npm run build`

Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add web/src/api/config.ts web/src/hooks/usePermissions* web/src/lib web/src/components/domain/PermissionCard web/src/pages/Welcome web/src/pages/Settings/controllers/useSettingsUpdatePermissions.ts web/src/i18n
git commit -m "fix: expose explicit macOS permission actions"
```

---

### Task 4: 内部签名配置与本地合同

**Files:**
- Create: `src-tauri/tauri.internal.conf.json`
- Create: `scripts/check-macos-signing-contract.mjs`
- Create: `scripts/check-macos-signing-contract.test.mjs`
- Create: `scripts/build-macos-internal.sh`
- Modify: `scripts/prepare-macos-dev-app.mjs`
- Modify: `scripts/macos-dev-cargo-runner.sh`
- Test: `scripts/check-macos-signing-contract.test.mjs`

**Interfaces:**
- Env: `CC_PARTNER_INTERNAL_SIGNING_IDENTITY`, `CC_PARTNER_INTERNAL_CERT_SHA256`。
- Produces: fail-closed internal `.app` 与签名摘要。

- [ ] **Step 1: 写签名解析/校验失败测试**

```js
assert.throws(() => validateSigningMetadata({
  identifier: 'com.cc-partner.app.internal',
  authority: '',
  requirement: '# designated => cdhash H"abc"',
  certSha256: 'AA',
}, { expectedIdentifier: 'com.cc-partner.app.internal', expectedCertSha256: 'AA' }), /ad-hoc/)
```

再覆盖错误 Bundle ID、错误指纹与稳定非 cdhash DR 成功。

- [ ] **Step 2: 运行测试确认失败**

Run: `node --test scripts/check-macos-signing-contract.test.mjs`

Expected: FAIL，模块不存在。

- [ ] **Step 3: 实现签名合同脚本**

脚本读取 `codesign -dvvv`、`codesign -d -r-`、`codesign --verify --deep --strict` 与 leaf certificate SHA-256；只打印 identifier、Authority/CN、短指纹和 DR 类型。缺 env、ad-hoc、指纹不匹配或 nested verify 失败 exit 1。

- [ ] **Step 4: 增加内部 Tauri overlay 与构建入口**

```json
{
  "productName": "cc-partner Internal",
  "identifier": "com.cc-partner.app.internal",
  "bundle": {
    "macOS": { "signingIdentity": "cc-partner Internal Code Signing" }
  }
}
```

`build-macos-internal.sh` 先 `security find-identity` 精确匹配 identity，再调用锁定 Tauri CLI：

```bash
web/node_modules/.bin/tauri build --config src-tauri/tauri.internal.conf.json
node scripts/check-macos-signing-contract.mjs <app-path> com.cc-partner.app.internal
```

- [ ] **Step 5: 修改开发壳签名分流**

存在 `CC_PARTNER_INTERNAL_SIGNING_IDENTITY` 时：Bundle ID=`com.cc-partner.app.internal.dev`、display name=`cc-partner Internal (Dev)`、codesign 使用该 identity；缺失时保留社区 Dev 名称但权限后端返回 unavailable。禁止内部 identity 失败后回退 `-`。

- [ ] **Step 6: 验证脚本**

Run: `node --test scripts/check-macos-signing-contract.test.mjs`

Expected: PASS。

Run: `node scripts/prepare-macos-dev-app.mjs --self-test`

Expected: PASS。

- [ ] **Step 7: 提交**

```bash
git add src-tauri/tauri.internal.conf.json scripts/check-macos-signing-contract* scripts/build-macos-internal.sh scripts/prepare-macos-dev-app.mjs scripts/macos-dev-cargo-runner.sh
git commit -m "build: add stable internal macOS signing channel"
```

---

### Task 5: 发布分流与运维文档

**Files:**
- Create: `.github/workflows/internal-macos.yml`
- Create: `docs/development/macos-internal-signing.md`
- Modify: `.github/workflows/release-tauri.yml`
- Modify: `src-tauri/CLAUDE.md`
- Modify: `web/CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `docs/development/real-device-certification.md`
- Modify: `docs/development/quality-matrix.json`

**Interfaces:**
- Workflow secrets: `MACOS_INTERNAL_CERTIFICATE_P12_BASE64`, `MACOS_INTERNAL_CERTIFICATE_PASSWORD`, `MACOS_INTERNAL_KEYCHAIN_PASSWORD`, `MACOS_INTERNAL_CERT_SHA256`。
- Produces: 手动内部 artifact；公开 workflow 不发布正式 macOS ad-hoc artifact。

- [ ] **Step 1: 增加文档/清单合同失败检查**

更新 `scripts/check-docs.mjs` 或现有事实检查，使公开 release 注释/矩阵中出现“macOS ad-hoc production releasable”时失败；内部签名文档必须包含 Bundle ID、四个 secret 名和 L3 ID。

- [ ] **Step 2: 运行事实检查确认失败**

Run: `node scripts/check-docs.mjs`

Expected: FAIL，现有 workflow 仍声明 macOS ad-hoc 正式发布。

- [ ] **Step 3: 实现内部 workflow**

workflow 仅 `workflow_dispatch`，绑定 `environment: internal-macos`；临时 Keychain 导入 P12，验证证书指纹，构建 aarch64 内部包，执行签名合同后上传 artifact；`always()` 删除临时 Keychain。不得打印 secret 或 P12 内容。

- [ ] **Step 4: 调整公开 release 与项目记忆**

公开 `release-tauri.yml` 移除 macOS matrix 或将其从 publish assets 排除，保留 Windows/Linux。根/分层指令明确：macOS 内部稳定包走 internal workflow；公开源码构建为社区无权限保证通道。

- [ ] **Step 5: 写证书与迁移 runbook**

文档包含：创建长期 Code Signing identity、导出/备份 P12、客户端只安装公钥证书、首次 Gatekeeper 手动允许、旧 ad-hoc → internal 一次重新授权、证书泄露/到期处理、不得提交私钥。

- [ ] **Step 6: 更新 L3 诚实状态**

为当前根治建立新的执行要求；旧 0.7.0 PASS 保留为历史但不能覆盖当前 commit。实际证书未安装前状态为 `NOT VERIFIED`。

- [ ] **Step 7: 运行文档合同并提交**

Run: `node scripts/check-docs.mjs --self-test && node scripts/check-docs.mjs && node scripts/check-quality-traceability.mjs`

Expected: PASS。

```bash
git add .github/workflows docs AGENTS.md src-tauri/CLAUDE.md web/CLAUDE.md scripts/check-docs.mjs
git commit -m "ci: split internal macOS and public release channels"
```

---

### Task 6: 集成验证与交付边界

**Files:**
- Modify as required only for defects found by the commands below.

**Interfaces:**
- Consumes: Tasks 1–5。
- Produces: 自动验证 evidence 与明确 L3 blocker。

- [ ] **Step 1: 静态禁止项扫描**

Run:

```bash
rg -n "TCCAccessPreflight|tccutil|rotate_dev_adhoc_codesign|input-monitoring-pending-request|CGRequestListenEventAccess" src-tauri/src src-tauri/native
```

Expected: 无生产权限路径命中；仅设计/历史测试 fixture 若有必须有明确 allowlist。

- [ ] **Step 2: Rust 定向与质量验证**

Run:

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked permissions:: -- --nocapture
```

Expected: PASS；不得出现真实 TCC prompt/reset。

- [ ] **Step 3: 前端验证**

Run:

```bash
cd web
npm run lint
npm run build
npm test -- src/lib/schemas/config.test.ts src/hooks/usePermissions.test.tsx src/lib/permissionEntries.test.ts src/pages/Welcome src/pages/Settings/controllers/useSettingsUpdatePermissions.test.tsx
```

Expected: PASS。

- [ ] **Step 4: 脚本与文档验证**

Run:

```bash
node --test scripts/check-macos-signing-contract.test.mjs
node scripts/prepare-macos-dev-app.mjs --self-test
node scripts/check-docs.mjs --self-test
node scripts/check-docs.mjs
node scripts/check-quality-traceability.mjs
```

Expected: PASS。

- [ ] **Step 5: 内部证书可用性判断**

Run: `security find-identity -v -p codesigning | rg -F "cc-partner Internal Code Signing"`

Expected: 若 identity 存在，继续内部构建与签名合同；若不存在，自动验证完成但 L3 保持 `NOT VERIFIED`，不得生成 ad-hoc 替代品。

- [ ] **Step 6: 有证书时执行内部构建/L3；无证书时记录阻塞**

有证书：运行 `scripts/build-macos-internal.sh`，再按 `docs/development/macos-internal-signing.md` 的 deny→grant→upgrade 清单执行。

无证书：最终交付明确写“代码与自动合同通过；稳定内部签名和真实 TCC 列表尚未验证，需要操作者安装证书后执行 L3”。

- [ ] **Step 7: 最终提交**

```bash
git add -A
git commit -m "fix: root macOS input monitoring identity flow"
```
