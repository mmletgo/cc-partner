# 质量门禁与架构治理设计

- 日期：2026-07-13
- 状态：方案已确认，待实施
- 适用范围：桌面/移动关键旅程、HTTP/IPC 契约、前端性能与 token 门禁、故障注入、超大模块边界、文档事实追踪

## 1. 背景

项目已有稳定 Vitest、Playwright、Ubuntu Rust 门禁、macOS/Windows backend smoke、P2P error envelope、Doctor 和 Workbench controller characterization。本设计不重复这些已完成工作，而是在其上补齐仍缺失的产品旅程和长期治理：

- Playwright 目前只覆盖 Attention 与 Screenshot Overlay，Transfer、Scratchpad、Prompts、Workbench、mobile、permission、Settings 和 LAN 无凭据边界缺少浏览器级闭环。
- 浏览器 mock、Rust route/command integration、hosted smoke、真实 GUI/权限/多主机 LAN 的证据边界没有统一 test ID 和追踪表。
- TypeScript 泛型只提供编译期假设，`invoke<T>`/`postJson<T>` 对损坏或混合版本 DTO 不做运行时校验。
- bundle 体积和 CSS token 引用没有自动预算；大模块仍持续增长。
- 测试以 happy path 为主，网络中断、stale response、DB busy、批次中途失败和 malformed DTO 的注入分散。

## 2. 目标与非目标

### 2.1 目标

1. 为八类关键旅程建立稳定、可诊断的浏览器 mock E2E，并把 LAN 内无凭据全读写、非支持网络来源拒绝与浏览器跨站防护纳入边界回归。
2. 明确四层证据：unit/contract、browser mock、backend/Tauri integration、真机 certification；任何文档不得越层宣称。
3. 对关键 HTTP/IPC DTO 进行无 `any` 的运行时解码，错误包含 surface/command/schema path，但不泄露正文。
4. 把 bundle、CSS token、模块规模和事实/coverage 追踪变成 CI 可执行门禁。
5. 建立可复用故障注入 seam，覆盖离线、超时、旧响应、事务 rollback 和 malformed payload。
6. 按领域拆分超大模块，保持现有 API、视觉和产品行为。

### 2.2 非目标

- 不重新迁移 Vitest，不重新拆 Workbench controllers，不重新设计 P2P error/Doctor。
- 不用 Playwright browser mock 冒充真实 Tauri WebView、系统权限或多主机 LAN。
- 不引入 Redux/Zustand、自动上传 telemetry 或第三方 SaaS 测试平台。
- 不追求一次 PR 把所有大文件降到 500 行；先建立 ratchet，再分领域迁移。
- 不为 LAN 引入配对、签名、Bearer、cookie session 或逐设备身份；这里只消费 LAN 信任边界 spec 的 socket peer 范围、route inventory 与 Origin/Host/WebSocket 合同。

## 3. 四层验证模型

| 层 | 名称 | 运行位置 | 能证明 | 不能证明 |
| --- | --- | --- | --- | --- |
| L0 | Unit/contract | Vitest + Rust unit | reducer、parser、schema、状态机、SQL | 浏览器/系统集成 |
| L1 | Browser mock E2E | Playwright Chromium | 路由、交互、a11y、错误/恢复、前端 API 调用契约 | Tauri command 注册、WebView、OS 权限、真实 LAN |
| L2 | Backend/Tauri integration | Rust integration + 可选本机 Tauri driver smoke | command/route、SQLite、sidecar、事件/文件/进程边界 | hosted 环境缺失的原生权限与多主机网络 |
| L3 | Real-device certification | macOS/Windows/Ubuntu 真机 + 两台 LAN 主机 | GUI/WebView、权限弹窗、WSL/tmux、多主机 mDNS 与无凭据全读写边界 | 不自动等同每次 PR CI |

L3 证据以版本、commit SHA、平台、执行人/日期和 PASS/FAIL/NOT VERIFIED 记录，90 天过期；过期后文档只能写“历史通过，当前未认证”。不提交截图中的用户凭据或真实项目路径。

## 4. 关键旅程 E2E

所有 L1 测试复用 `web/tests/fixtures.ts` 的 console/pageerror 守卫和一个新的 deterministic backend harness；禁止每个 spec 手写整套 Tauri mock。test ID 固定如下：

| ID | 文件 | 最小闭环 |
| --- | --- | --- |
| `E2E-TRANSFER-001` | `web/tests/transfer.spec.ts` | 选择真实路径 DTO→设备→send→progress→completed；cancel/failure/retry；未实现动作不可点击 |
| `E2E-SCRATCH-001` | `web/tests/scratchpad.spec.ts` | 输入→切页/卸载 flush→刷新仍在；保存失败可重试且不伪装成功 |
| `E2E-PROMPTS-001` | `web/tests/prompts.spec.ts` | create/update/delete success；每个 reject 回滚原位置并提示 |
| `E2E-WORKBENCH-001` | `web/tests/workbench.spec.ts` | 项目→worktree→terminal/files→stale response 丢弃→offline 禁写/恢复 |
| `E2E-MOBILE-001` | `web/tests/mobile-workbench.spec.ts` | 手机视口项目/Attention/terminal/files/automation；导航抽屉焦点和断线恢复 |
| `E2E-PERM-001` | `web/tests/permissions.spec.ts` | 初次检查失败→明确错误/重试；逐项授权；notification 不阻断；截图缺权回 Welcome |
| `E2E-SETTINGS-001` | `web/tests/settings.spec.ts` | 一个非核心 loader 失败只影响 tab；修改/保存/回滚；dependency/automation deep link |
| `E2E-LAN-001` | `web/tests/lan-boundary.spec.ts` | 合法 LAN 中 native P2P 与 `/mobile` 无凭据全读写；公网 peer、forwarded-header 伪造、异常 Host/Origin、非法 WebSocket Origin 与远程 stop 均被拒绝 |

测试 selector 优先 role/name/label；只有无语义节点才加稳定 `data-testid`。中英文至少各跑一条关键旅程；主题和 reduced-motion 在 mobile/Settings 中抽样。测试不等待真实轮询，使用 harness 主动发布事件/推进 clock。

## 5. 统一 deterministic backend harness

新增 `web/tests/support/backendHarness.ts`：

- 注入 `window.__TAURI_INTERNALS__.invoke`、Tauri event callback registry 和同源 `fetch` mock。
- 用强类型 command/route registry；未注册调用立即失败并打印 command/path。
- 支持 `resolve/reject/defer`、按调用序号返回、AbortSignal、事件 emit、调用记录。
- `defer` 用于制造 stale response；fault profile 固定为 `networkOffline|timeout|malformedJson|permissionDenied|conflict|dbBusy|lanBoundaryRejected|crossSiteRejected`。
- 测试结束断言无 pending request、未消费预期和泄漏 listener。

生产代码不得读取 `__ccTest*` 全局；harness 只通过既有 Tauri/fetch 边界注入。

## 6. 关键 DTO 运行时 schema

不新增大型 schema 库，使用小型组合式 decoder，避免把验证依赖加入 mobile 首包。新增：

```ts
export interface Decoder<T> {
  readonly name: string;
  decode(value: unknown, path?: string): T;
}

export class ContractDecodeError extends Error {
  readonly contract: string;
  readonly path: string;
}

export function invokeDecoded<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  decoder: Decoder<T>,
): Promise<T>;
```

HTTP `getJson/postJson` 增加可选 decoder overload。首批强制覆盖：

- protocol health/capabilities 与 P2P error envelope；
- Attention snapshot；
- Orchestrator runtime snapshot/task/outbox；
- Transfer send result/task/progress event；
- Workbench project/worktree/session/path/file save result；
- Settings config/defaults、Permissions status；

decoder 必须拒绝错误 enum、缺必填字段、非有限 number、错误 nullability 和超深/超大数组。错误日志只写 contract/path/actual primitive kind/request ID，不序列化 payload。legacy 可选字段只能在对应 decoder 中显式 default，禁止全局宽松 cast。

## 7. 性能、CSS 与架构门禁

### 7.1 Bundle budgets

基于 Vite manifest 计算入口闭包 gzip，不把 sourcemap 算入运行时但单独限制 map：

- desktop `index.html` initial JS gzip ≤ 320 KiB；
- mobile `mobile.html` initial JS gzip ≤ 280 KiB；
- 任一 lazy JS chunk gzip ≤ 700 KiB；
- 全部运行时 JS gzip ≤ 1,400 KiB；
- 生产 sourcemap 总原始大小 ≤ 2 MiB，或改为 hidden/upload-only 后 dist 不含 `.map`。

性能拆包计划落地前，以当前基线生成 `scripts/bundle-budget-baseline.json`，门禁先禁止增长；达到上述最终值后删除 baseline 豁免。任何预算调整必须有体积报告和用户批准，不得在失败 PR 中顺手抬高。

### 7.2 Undefined token

`scripts/check-css-tokens.mjs` 扫描 `web/src/**/*.css` 的 `var(--name)`：无 fallback 的变量必须在 `tokens.css` 定义；语义颜色 token 必须在 `:root` 和 `[data-theme="dark"]` 两套出现。脚本必须自测未知变量、fallback、注释、重复定义、暗色缺失。先修复现有 `--bg-2/--bg-primary/--bg-elevated/--fg-muted/--border-subtle/--border-strong/--warning` 漂移，再启用 CI。

### 7.3 模块规模 ratchet

`scripts/check-module-boundaries.mjs` 使用基线 JSON 禁止受治理文件新增行数，并最终执行软/硬阈值：TS/TSX 1,000/1,500 行，Rust 2,500/5,000 行。硬阈值只允许已有基线文件；新文件不得超过软阈值。allowlist 条目必须有 owner/reason/expiresAt（最长 90 天）。

## 8. 超大模块拆分边界

- `Orchestrator.tsx`：页面 composition、`useOrchestratorBoardController`、task details/evidence drawer、create dialog、runtime/outbox panels。
- `Settings.tsx`：`useSettingsController` + General/Dependencies/Sync/AI/Automation/About panels；每个 panel 只消费窄 view model。
- `MobileAutomationPanel.tsx`：`useMobileAutomationController`、task list/detail、create dialog、runtime/outbox sections。
- `lib/types.ts`：拆到 `lib/types/{attention,config,orchestrator,transfer,workbench,...}.ts`，保留 `lib/types.ts` 兼容 re-export，随后逐域迁移 import。
- `orchestrator/repo.rs`：拆成 `repo/{mod,schema,tasks,attempts,evidence,remote}.rs`；`OrchestratorRepo` 公共签名保持不变。
- `commands/workbench.rs` 与 `commands/orchestrator.rs`：变为目录模块，按 projects/sessions/files/git/browser/automation/action 分组；`lib.rs` 注册 command 名完全不变。

`transfer/receiver.rs` 和 `workbench/dependencies.rs` 暂只进入 no-growth ratchet；其安全/平台耦合高，必须另写专门 spec 后再拆。

## 9. 故障注入矩阵

| 故障 | L0/L2 注入点 | 必须结果 |
| --- | --- | --- |
| stale response | deferred Promise / request sequence | 旧响应不能覆盖新选择 |
| HTTP offline/timeout | transport fake / AbortSignal | 保留可用缓存并明确 stale/offline |
| malformed DTO | decoder fixture | fail closed；显示可重试错误；不渲染部分危险状态 |
| permission denied/check reject | permission adapter | 不永久“检查中”；可重试/可跳过非阻塞项 |
| DB busy/transaction row N fail | repo test seam | 有界等待；整批 rollback |
| sync response lost after commit | peer fake | 幂等重试不产生重复/回退 |
| LAN 部署边界 | socket peer/forwarded-header integration | 合法 LAN peer 的 native P2P 与 `/mobile` 无凭据全读写；公网 peer 与 forwarded-header 伪造在 handler 前被拒绝 |
| browser cross-site request | Origin/Host/WebSocket Origin fixtures | 跨站写、异常 Host 与非法 WebSocket Origin 被拒绝；同源 `/mobile` 与无浏览器 Origin 的 native P2P 继续工作；远程 peer 不能触发 backend stop |
| terminal/event disconnect | event bridge fake | listener 回收、reconnect/replay 不重复输入 |

故障 seam 只能存在于测试构造器或 trait 注入，不允许生产环境变量打开“测试故障”。

## 10. 事实与 coverage traceability

新增机器可读 `docs/development/quality-matrix.json`，每条 requirement 包含：

```json
{
  "id": "E2E-TRANSFER-001",
  "surface": "transfer",
  "level": "L1",
  "tests": ["web/tests/transfer.spec.ts"],
  "command": "cd web && npm run test:e2e -- transfer.spec.ts",
  "ciJob": "frontend-e2e",
  "platforms": ["chromium-linux"],
  "exclusions": ["real Tauri file dialog", "multi-host LAN"]
}
```

`scripts/check-quality-traceability.mjs` 验证 ID 唯一、文件存在、命令来自 package/workflow、L3 记录未过期、文档引用的 test ID 存在。`docs/development/testing.md` 只呈现矩阵和明确 exclusions；PRD 只记录持久产品行为，不记录测试任务历史。

## 11. 完成标准

1. 八个 L1 test ID 全部在 CI 执行，失败保留 trace/video/browser logs。
2. 关键 DTO 的 malformed fixtures 全部 fail closed，正常/legacy fixtures 通过。
3. bundle、CSS token、module ratchet、quality traceability 四个脚本均有 self-test 并进入 CI。
4. 指定六类超大模块完成领域拆分，公共 API/command/route/视觉行为不变；其余巨型文件不再增长。
5. L3 矩阵明确记录 macOS 权限、Windows/WSL、三平台 GUI，以及双主机 mDNS、无凭据全读写与非支持网络边界拒绝；未执行项标 `NOT VERIFIED`。
6. `npm run lint && npm run build && npm test && npm run test:e2e`、Rust fmt/clippy/test、docs/route inventory 全部通过。
