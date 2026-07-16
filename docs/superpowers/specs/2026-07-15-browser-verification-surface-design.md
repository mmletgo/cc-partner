# Browser Verification Surface 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：现有 Workbench Browser preview/proxy；Orchestrator evidence 适配依赖既有任务系统
- 对应计划：`docs/superpowers/plans/2026-07-15-browser-verification-surface.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，opaque ID 不是认证 token。

## 1. 问题

现有 Browser Workspace 已能在 owning device 发现 loopback HTTP target，使用不可预测 preview ID、TTL、HTTP/WS proxy 与 sandbox iframe进行人工预览；但无法提供稳定 DOM snapshot、click/fill/wait、console、screenshot 或自动 assertion evidence。

父页不能读取无 `allow-same-origin` iframe DOM，而取消 sandbox 会破坏现有安全边界。因此自动验证必须在 owner 侧独立执行，不能让前端变成任意网页代理。

## 2. 目标

1. 在 owning device 按需启动 ephemeral managed Chromium runtime；空闲退出，不新增常驻 daemon。
2. verification target 必须绑定现有 live preview registry 与已校验 loopback URL。
3. 提供 accessibility snapshot、click、fill、wait、screenshot、console/errors 与有限 network summary。
4. 默认自动运行 load/console/screenshot smoke；workflow 声明 assertion 时自动产生结构化结果。
5. Browser evidence 可进入 Orchestrator/experiment，但浏览器领域不依赖 task 表。
6. local、remote、mobile/CLI 使用同一 owner command helper和 bounded DTO。

## 3. 非目标

- 不接受任意公网 URL、file URL、data URL 或用户提供的 CDP endpoint。
- 不导入系统浏览器 cookie、history、extension 或 profile。
- 不提供 arbitrary JavaScript eval、任意 CSS selector shell 或通用爬虫。
- 不取消现有 iframe sandbox，也不让 proxy 注入高权限 bridge。
- 不要求用户手工圈选元素才能开始验证。
- 不把浏览器 runtime 变成第二个 daemon。

## 4. Engine 选择

首版使用 Chrome for Testing `chrome-headless-shell` `150.0.7871.114`，由 release matrix 管理固定版本；Rust CDP 客户端锁定 `chromiumoxide = 0.7.0`（MSRV 1.70，低于项目 Rust 1.77.2）：

- 按 `linux64/mac-arm64/mac-x64/win64` 准备固定 URL、SHA-256 与 Chromium 资源；不依赖用户手工安装 Node/Playwright。
- sidecar 按需启动 Chromium child，使用随机 remote-debugging pipe/port和全新临时 profile。
- child 只绑定 owner loopback；不监听 LAN。
- 每个 verification session 最长 30 分钟，空闲 60 秒自动关闭。
- crash、owner shutdown、session expiry 后递归删除临时 profile；删除失败记录路径哈希而非真实路径。
- release bundle 增量预算单独记录，三平台未实测保持 `NOT VERIFIED`。

现有 iframe preview继续负责人工查看，二者共享 target validation，不共享 cookie/profile。

## 5. 领域模型

```rust
pub struct BrowserVerificationSession {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub preview_id: String,
    pub owner_instance_id: String,
    pub state: BrowserVerificationState,
    pub created_at: String,
    pub last_activity_at: String,
    pub expires_at: String,
}

pub enum BrowserVerificationCommand {
    Snapshot { max_nodes: u32 },
    Click { node_ref: String },
    Fill { node_ref: String, value: String },
    WaitFor { condition: BrowserWaitCondition, timeout_ms: u64 },
    Screenshot { full_page: bool },
    ReadConsole { after_sequence: u64 },
}

pub struct BrowserVerificationEvidence {
    pub session_id: String,
    pub url_path: String,
    pub page_title: Option<String>,
    pub assertions: Vec<BrowserAssertionResult>,
    pub console_errors: Vec<BrowserConsoleEntry>,
    pub screenshot_id: Option<String>,
    pub truncated: bool,
    pub captured_at: String,
}
```

## 6. 安全与资源限制

- session create 只接受 `previewId`；owner 从 registry解析 URL，不接受调用者传 host/port。
- 每次 navigation 都重新验证 scheme、loopback host 与显式 port；redirect 离开 allowlist立即终止。
- snapshot 最多 5,000 nodes/2 MiB；只返回 role/name/state/bounds/source hint，不返回 password value。
- fill 拒绝 password、file、hidden 和跨 origin frame；value 最大 64 KiB且不写日志/evidence。
- wait timeout 100–30,000 ms；每 session 同时最多一个 mutating command。
- screenshot PNG 最大 8 MiB，超限缩放或返回 `resource_limit`；本机 evidence 默认保留 7 天，不进 Cloud Sync。
- console 最多 1,000 entries/1 MiB，错误正文清洗 URL query、header、cookie 和 stack绝对路径。
- network 只返回 method、loopback path、status、duration、resource type；不返回 request/response body或 header。

## 7. Snapshot 与交互

- snapshot 使用 accessibility tree + bounded DOM metadata，生成 session-local opaque `nodeRef`。
- `nodeRef` 包含 generation；navigation/DOM generation变化后旧 ref 返回 conflict，不盲点新元素。
- click/fill 只接受当前 snapshot ref，不接受 arbitrary selector。
- source hint 仅在开发服务器提供可靠 `data-source`/source-map metadata时返回；未知保持空。
- automatic smoke：create session → wait DOMContentLoaded → snapshot → collect console → screenshot。
- workflow assertion 使用明确结构化条件：role/name可见、文本存在、URL path、console error count、HTTP status；不执行任意脚本。

## 8. API、远端与 capability

- 本地 Tauri、control API、remote P2P 与 mobile/CLI 调用统一 `BrowserVerificationService`。
- capability 为 `workbench.browser-verification.v1`。
- remote command在 project owner执行；当前设备只代理 command/evidence。
- session ID 和 nodeRef均为不可预测 opaque ID；旧 peer 显示 unsupported。
- query 可以按稳定 session/generation重试；click/fill使用 command request ID与结果对账，不盲重放。

## 9. Orchestrator Evidence 适配

- browser domain 输出中立 `BrowserVerificationEvidence`，不引用 task表。
- Orchestrator adapter把 assertion/console/screenshot ID写成 `browserVerification` evidence kind。
- workflow 未声明 browser validation 时，发现 preview后自动运行 smoke；无 preview则记录 `not_applicable`，不把普通非 Web task判失败。
- required assertion 缺少 preview或engine unavailable时为 verification failure，而不是静默跳过。
- experiment只消费结构化摘要和 screenshot ID，不把页面正文放入 comparative prompt。

## 10. 失败与回滚

- Chromium unavailable/crash 返回 `browser_engine_unavailable`/`browser_engine_crashed`，清理 child/profile并允许一次新 session重试。
- redirect escape、stale node、resource limit、timeout、owner offline有独立错误 code。
- remote 断线后 mutation结果未知时先查询 command result，不自动重发 click/fill。
- rollback 删除 verification route/registry entry和临时文件，不影响现有 Browser preview iframe/proxy。
- release 发现平台资产异常时禁用 verification capability，不能回退为不安全 iframe DOM访问。

## 11. 测试与验收

1. target binding、redirect escape、scheme/host/port、TTL与opaque ID有 Rust测试。
2. engine lifecycle覆盖lazy start、idle exit、crash、shutdown、profile cleanup。
3. snapshot node limit、password redaction、stale node、fill限制和console/network清洗。
4. command idempotency/result reconciliation与remote unsupported/lost ACK有route测试。
5. automatic smoke、required assertion、not_applicable和Orchestrator evidence adapter有集成测试。
6. E2E使用本地fixture验证snapshot→click→fill→wait→screenshot→console error。
7. macOS/Windows/Ubuntu managed Chromium打包和实际截图分别记录L3结果；未执行保持NOT VERIFIED。

## 12. Spec 自审

- 自动验证在owner执行，没有削弱iframe sandbox或扩大成任意URL代理。
- 浏览器domain与Orchestrator解耦，只通过evidence adapter集成。
- normal smoke不要求用户选择元素，只有明确失败才进入Attention。
