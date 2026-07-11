# cc-partner 工程改进治理总纲

- 日期：2026-07-11
- 状态：方案已确认，待用户复核书面 spec
- 文档类型：工程治理总纲；各工作流后续独立生成 implementation plan

## 1. 背景

cc-partner 已从局域网文件传输、截图和 Prompt 管理工具发展为包含 Tauri 桌面宿主、独立 backend CLI、移动 Workbench、本机/远端项目、tmux terminal、文件/Git 工作区和 Orchestrator 的跨平台系统。现有工程基础能够支撑功能，但出现了几类结构性风险：

- 前端存在大量测试文件，却没有统一 test runner 或 CI 入口。
- `Workbench.tsx` 同时承载多个领域的状态和副作用，修改风险持续升高。
- 日常 CI 只在 Ubuntu 运行，macOS/Windows 的平台行为到发版时才暴露。
- P2P 缺少显式协议版本、capability 和稳定错误码。
- 远端 Orchestrator runtime snapshot 仍返回 unsupported 空数据。
- detached backend 丢弃 stdout/stderr，故障后缺少本地证据。
- README 和部分开发说明已经落后于真实产品与发布机制。

本总纲的目标不是一次性重构整个仓库，而是把工程改进拆成可独立实施、验证、回滚的工作流，并规定正确依赖顺序。

## 2. 已确认决策

1. 工程改进使用一份治理总纲；每个工作流后续独立编写 implementation plan。
2. 主顺序采用“测试基础先行”。
3. 前端单测统一迁移到 Vitest。
4. Vitest 与 Playwright 分离为 unit 和 E2E 两条门禁。
5. Workbench 使用领域 controller/hook 拆分，不引入 Redux/Zustand。
6. P2P 支持一代滚动兼容，不永久维护多个历史协议版本。
7. macOS/Windows smoke 在相关 PR 运行，并由每日定时任务兜底。
8. hosted runner 第一阶段不承诺真实 WSL/tmux、GUI 或系统权限自动化。
9. backend doctor 以受控本地轮转日志为前置。
10. 远端 runtime snapshot 离线时只保留进程内缓存，明确过期，不持久化。
11. P2P 幂等采用风险分类和渐进治理，不批量改造全部写接口。

## 3. 目标

1. 建立可重复、可发现、跨平台可解释的验证体系。
2. 降低 Workbench 单体页面的修改、审查和回归风险。
3. 让 P2P 协议可以安全演进并支持混合版本设备。
4. 让远端 Orchestrator 和独立 backend 具备真实可观测性。
5. 让后台故障留下本地、受限、可脱敏的诊断证据。
6. 让 README、PRD 和分层开发指令与实现保持一致。

## 4. 非目标

- 不改变 Workbench 产品行为或视觉设计。
- 不引入 Redux、Zustand 或新的前端框架。
- 不重写 Rust/Tauri 架构。
- 不在协议治理中一次性改造全部接口。
- 不新增云端日志、遥测、自动诊断上传或用户行为分析。
- 不把不稳定的 WSL/tmux/GUI 环境测试描述为 CI 已覆盖。
- 不在同一变更中同时实现全局 Inbox 和 Workbench 拆分。
- 不要求本总纲由一个 PR 或一个 implementation plan 完成。

## 5. 执行波次与依赖

```text
Wave 1 · 测试基础
Vitest 迁移 → unit/e2e scripts → CI 门禁 → Workbench characterization

Wave 2 · 可维护性
按领域 controller 拆分 Workbench，保持行为不变

Wave 3 · 远端可靠性
protocolVersion/capabilities/error envelope
→ remote runtime snapshot
→ 渐进式幂等治理

Wave 4 · 运行保障
相关 PR + 每日 macOS/Windows smoke
→ 受控轮转日志
→ backend doctor

Wave 5 · 文档校准
README、AGENTS/CLAUDE、PRD、命令与 CI 说明同步
```

Doctor/日志和不依赖行为变化的文档事实修正可以在不冲突时并行，但不得绕过各自前置测试。每个工作流必须在自身完成后保持主分支可运行，不能依赖后续波次补齐半成品。

## 6. 工作流一：前端测试统一入口

### 6.1 当前问题

`web/` 已有大量 `.test.ts` 和 Playwright 测试，但 `package.json` 只有 dev/build/lint/preview。测试依赖 `web/CLAUDE.md` 中手工串联的 `npx --yes tsx` 命令，CI 没有执行这些测试。

### 6.2 目标设计

引入 Vitest 作为统一单元测试框架：

- 现有顶层断言机械迁移为 `describe/test`。
- 迁移只调整 runner 结构，不顺便重写业务断言。
- 纯 helper 默认 Node environment。
- 确实依赖 DOM 的文件显式声明 jsdom。
- 使用项目锁定的本地依赖，不在 CI 使用 `npx --yes` 浮动安装 runner。
- 本轮不设置覆盖率百分比 KPI。

固定脚本：

```json
{
  "test": "vitest run",
  "test:unit:watch": "vitest",
  "test:e2e": "playwright test",
  "test:all": "npm run test && npm run test:e2e"
}
```

迁移纪律：

1. 建立迁移清单，记录全部现有测试文件。
2. 尚未迁移的文件继续由 legacy `tsx` runner 执行。
3. 已迁移文件只由 Vitest 执行，避免同一测试重复运行。
4. 清单全部迁移并核对后，删除 legacy runner 和手工文件列表。
5. 任一测试丢失、静默跳过或只在开发者本机运行都视为迁移失败。

### 6.3 CI

- unit job 执行 `npm test`。
- E2E 使用独立 Chromium job 执行 `npm run test:e2e`。
- E2E 失败上传 screenshot、trace 和浏览器日志。
- unit 与 E2E 均为合并门禁。
- build/lint 保持独立步骤，便于区分失败类别。

### 6.4 完成标准

- 干净环境执行 `npm ci && npm test` 能自动发现全部单测。
- 任一断言失败返回非零退出码。
- CI 明确运行 unit 和 E2E。
- `web/CLAUDE.md` 不再维护逐文件测试命令长清单。

## 7. 工作流二：Workbench 行为保持型拆分

### 7.1 原则

- 先 characterization，后抽离。
- 每次只迁移一个领域。
- 不改变 UI、文案、路由、API、事件名或产品行为。
- 不改变 xterm DOM 常驻、terminal buffer、replay gate 或 tmux attach 语义。
- 不引入新的全局状态库或巨型 Context。

### 7.2 Controller 边界

```text
useWorkbenchProjectController
  项目选择、remote offline、跨项目 request stale guard

useWorkbenchTerminalController
  session/window/pane、事件、buffer/replay、focus、resize

useWorkbenchWorktreeGitController
  worktree 生命周期、commit/push/merge、Git 状态刷新

useWorkbenchFileController
  目录、tabs、dirty guard、打开/保存/格式化、文件 CRUD

useWorkbenchAutomationController
  自动化控制台、task deep link、执行现场切换

Overlay controllers
  Prompt optimizer、Claude session search
```

Controller 持有权威状态和副作用；已有 domain/view 组件只消费明确 props。`Workbench.tsx` 保留页面布局和必要的跨领域协调。

### 7.3 Characterization Tests

拆分前必须覆盖：

- project/worktree 切换和旧响应丢弃
- remote offline 写操作禁用与恢复
- terminal replay、focus、resize 和 route 切出后的 buffer 连续性
- dirty file tab 切换、保存和丢弃 guard
- worktree create/remove/merge 与 Git 刷新
- automation deep link 和执行现场切换
- Prompt optimizer/session search overlay 生命周期

### 7.4 渐进拆分纪律

1. 补当前领域 characterization test。
2. 运行测试确认基线通过。
3. 抽离一个 controller，不同时改邻接领域。
4. 运行领域测试、全部 Workbench 测试、lint 和 build。
5. 提交该领域拆分，再进入下一个领域。

### 7.5 完成标准

- `Workbench.tsx` 格式化后总行数不超过 1,200 行。
- 页面文件不再实现 terminal、files、Git/worktree 和 automation 的具体副作用。
- 新文件具有单一职责，不通过复制 state、转发大段代码或无意义分文件达成行数目标。
- 每个 controller 可以在不渲染完整 Workbench 的情况下进行针对性测试。
- 全部既有行为测试、lint 和 build 通过。

## 8. 工作流三：P2P 协议治理

### 8.1 协议发现

`/api/health` 增加：

```json
{
  "protocol_version": 1,
  "capabilities": [
    "errors.envelope.v1",
    "orchestrator.runtime-snapshot.v1",
    "attention.v1"
  ]
}
```

mDNS TXT 使用 `proto=1` 和 `caps=<comma-separated capabilities>`。`caps` 按字典序编码，完整 TXT 字符串最多 220 个 UTF-8 bytes；加入下一个完整 capability token 会超过上限时停止编码，不能截断 token。health 始终返回完整权威列表，调用任何新路由前仍须以 health 结果为准，mDNS 只用于发现和快速预判。

兼容政策：

- 缺少字段的对端视为 legacy v0。
- 新客户端只有在 capability 存在时调用新路由。
- 旧客户端继续使用已有路由和响应。
- 当前代维护 v0→v1 滚动兼容；下一次 protocol major 可以删除 v0 兼容读取。

### 8.2 标准错误

为保持旧客户端可读，错误 envelope 保留 `error: string`：

```json
{
  "error": "用户可读消息",
  "code": "workbench.path_conflict",
  "request_id": "uuid",
  "retryable": false,
  "details": {}
}
```

规则：

- `code` 是稳定机器语义，客户端不得解析中文 `error` 决策。
- `details` 只能包含脱敏、结构化的诊断字段。
- 客户端继续兼容只有 `{ "error": "..." }` 的旧响应。
- HTTP 状态固定映射：validation=`400`、not found=`404`、conflict=`409`、offline/unavailable=`503`、timeout=`504`、internal=`500`。认证或授权能力未来接入时使用 `401/403`，不得复用 `500`。
- 请求可携带 `X-CC-Request-Id`；缺失时服务端生成。
- request ID 在响应 header、错误 body 和 tracing span 中保持一致。

### 8.3 Capability Gate

每个新 P2P 能力必须：

1. 定义稳定 capability 名称与版本。
2. 在 health 完整列表中声明。
3. 由调用方 gate 后再访问路由。
4. 对 unsupported、offline、invalid response 分别建模。
5. 提供 legacy 对端契约测试。

### 8.4 幂等风险清单

所有 P2P 写端点分类为：

- 只读，不适用幂等
- 天然幂等，可安全重放
- 需要 idempotency key
- 禁止传输层自动重试

本轮治理要求：

- 保留 Orchestrator create 的 `clientRequestId`。
- 新的可重试副作用接口必须声明幂等策略。
- 优先评估 worktree create、session create、远端任务动作。
- commit、push、merge、delete 默认禁止自动重试，直到领域层定义安全语义。
- 不在同一改动中批量给所有写端点添加统一 key。

### 8.5 完成标准

- 新旧设备混合版本契约测试通过。
- 新客户端不会向缺少 capability 的对端调用新路由。
- 稳定 error code 驱动 offline/unsupported/conflict/retryable 行为。
- request ID 能串联客户端错误、服务端响应和日志。
- 写端点风险清单进入 P2P 领域文档并由新增接口遵守。

## 9. 工作流四：远端 Orchestrator Runtime Snapshot

### 9.1 目标

远端项目的 runtime snapshot 必须来自 owning device，不能返回本机 scheduler/telemetry 的替代数据。

### 9.2 设计

- owning device 新增只读 P2P runtime snapshot route。
- route 固定为 `POST /api/orchestrator/runtime-snapshot`，请求体为 `{ "project_id": "<local-project-id>" }`，成功响应复用现有 runtime snapshot DTO。
- route 只接受 owning device 上的 local project。
- remote shortcut 递归请求必须被拒绝。
- route 复用本机 runtime snapshot 构造逻辑，不复制 scheduler 状态计算。
- 调用端只在 `orchestrator.runtime-snapshot.v1` capability 存在时请求。
- project/worktree/session ID 映射继续使用现有 `remote:<device>:` 规则。

返回状态：

- `live`：在线且收到 owning device 快照。
- `unsupported`：对端没有 capability。
- `offline`：对端不可达。
- `unavailable`：对端响应无效或 runtime 不可用。

### 9.3 离线缓存

- desktop 和 mobile 各自在自己的 runtime provider/store 中保留最后一次成功快照。
- 离线后继续显示缓存内容，同时标记 `remoteStatus=offline` 和最后更新时间。
- 缓存不得写入数据库或磁盘。
- 应用重启且远端仍离线时显示 offline/unavailable 空态。
- 缓存内容不能被 scheduler、验证或交付逻辑消费。

### 9.4 完成标准

- local snapshot 行为不变。
- 在线 remote 显示 owning device 的 generatedAt、tick、slots、running/retrying 和 recent events。
- offline、unsupported、unavailable 有不同 UI 和契约测试。
- 不存在读取本机 telemetry 伪装远端状态的路径。

## 10. 工作流五：macOS/Windows Smoke

### 10.1 触发策略

- Ubuntu 完整质量门禁继续覆盖所有代码 PR。
- macOS/Windows smoke 在 Rust、Workbench、backend、脚本、依赖或 CI 配置变化时运行。
- docs-only、纯翻译和无关静态资源改动跳过 PR 跨平台 job。
- 主分支每日运行一次无路径过滤的 macOS/Windows smoke，捕获 runner 与工具链漂移。

### 10.2 第一阶段覆盖

- backend CLI `start → health → status → stop`
- stale control file 和重复启动行为
- 平台路径、shell 和命令解析单测
- PTY 基本创建、echo 和退出
- Unix process group 与 Windows detached process 生命周期
- Rust/Tauri 最小构建检查

### 10.3 明确不覆盖

Hosted runner 第一阶段不承诺：

- 真实 Windows WSL + tmux
- macOS 原生权限弹窗
- GUI/WebView 自动化
- 局域网 mDNS 多机发现

缺少环境时必须输出明确 skip reason，并在 job summary 中列为未验证。不得通过静默 skip 或长期 `continue-on-error` 宣称能力通过。

### 10.4 运行纪律

- job 与子进程都有明确 timeout。
- 失败上传 backend log、测试输出和必要诊断 artifact。
- 结束时清理测试进程和临时 control file。
- smoke 必须可重复执行，不依赖 runner 上一次状态。

## 11. 工作流六：受控日志与 Backend Doctor

### 11.1 受控本地日志

独立 backend 将 tracing 输出写入应用数据目录：

- 单文件上限 5 MiB。
- 保留 3 个历史文件。
- Unix 文件权限限制为当前用户可读写。
- detached 启动不再丢弃全部诊断输出。
- 结构化记录 request ID、领域、操作、结果、耗时和脱敏错误。

禁止写入：

- Prompt 或会话正文
- 文件内容或请求正文
- 完整环境变量
- token、密码、密钥、Authorization header
- Claude/Codex 凭据

### 11.2 Doctor 命令

```text
cc-partner-backend doctor
cc-partner-backend doctor --json
```

JSON schema 至少包含：

```text
schemaVersion
generatedAt
status: healthy | degraded | unhealthy
app/backend version
platform/arch
control file / pid / port / health
data/database/log path readability
mDNS summary
Git/tmux/WSL/Claude CLI dependency summary
recent sanitized error summaries
log path
```

隐私约束：

- JSON 模式 stdout 只能输出合法 JSON。
- tracing 和说明写 stderr，不能污染 JSON。
- 用户 home 在输出中统一替换为 `<HOME>`。
- 不枚举或输出环境变量。
- 不输出项目名、项目内容、Prompt 或凭据。
- 不提供上传、远程收集或自动提交 issue 行为。

退出码：

- `0`：healthy
- `1`：degraded
- `2`：unhealthy 或 doctor 无法完成

状态判定：

- `healthy`：所有核心路径可读写、control 状态自洽，且没有依赖或连通性 warning。backend 正常停止本身只是信息，不构成 warning。
- `degraded`：doctor 完整执行，但存在非致命问题，例如可选 tmux/WSL/Claude CLI 缺失、mDNS 探测失败或可恢复 stale control file。
- `unhealthy`：数据目录、数据库或日志目录不可用，active control 指向不可达进程且无法恢复，或 doctor 无法完成核心检查。

### 11.3 测试

- 日志达到 5 MiB 后正确轮转并最多保留 3 个历史文件。
- Unix 权限符合当前用户读写要求。
- JSON snapshot 稳定且 stdout 可直接解析。
- running、stopped、stale control、端口占用和依赖缺失均有测试。
- token、secret、Authorization、Prompt、文件内容和 home 用户名不能出现在日志或 doctor fixture 输出。

## 12. 工作流七：README 与分层文档校准

### 12.1 README

首页定位调整为：

1. local-first 多设备 Workbench
2. Mobile Workbench
3. Orchestrator 自动编排与可见执行
4. headless backend CLI
5. 文件传输、截图、Prompt、速记本等辅助能力

必须修正：

- 已弃用 tauri-action 的发布描述
- 固定首选端口、占用递增的真实策略
- 当前本机 IPC 与 P2P HTTP 边界
- headless backend 启动方式
- 当前平台和依赖限制

### 12.2 PRD 与指令文件

- 产品行为变化进入 `docs/prd.md`。
- 根 `AGENTS.md` 只维护概览、顶级目录地图、组件清单和关键陷阱。
- `web/CLAUDE.md` 维护真实 npm/Vitest/Playwright 命令与前端约束。
- `src-tauri/CLAUDE.md` 维护 P2P capability/error、runtime snapshot、logging/doctor 和平台测试约束。
- 指令文件不记录任务时间线或提交摘要。

### 12.3 校验

- README 内部链接有效。
- 安装、开发、测试和 doctor 命令至少完成静态或 smoke 校验。
- 文档描述的 CI job、协议字段和平台范围与仓库实现一致。

## 13. 总体验证矩阵

| 工作流 | 必须验证 |
| --- | --- |
| Vitest/CI | `npm ci && npm test`、E2E job、失败非零、artifact |
| Workbench 拆分 | characterization、全部 Workbench tests、lint、build |
| P2P 治理 | v0/v1 混合契约、capability gate、error code、request ID |
| Remote snapshot | live/offline/unsupported/unavailable、ID 映射、无本机替代 |
| Cross-platform | macOS/Windows lifecycle、PTY、路径、最小构建、每日 schedule |
| Logs/doctor | rotation、权限、JSON、退出码、敏感字段扫描 |
| 文档 | 链接、命令、事实与实现一致性 |

## 14. 合并与发布纪律

1. 不允许把多个波次压成一个不可审查的大 PR。
2. 每个工作流独立实施、验证、提交和回滚。
3. Workbench 拆分不得与 Inbox UI 实现混在同一提交。
4. 协议升级先发布兼容读取方，再发布使用新 capability 的调用方。
5. 数据库如因后续工作必须变化，单独提供迁移、兼容和回滚；本总纲本身不要求新增业务表。
6. doctor 和日志通过敏感字段回归测试后，才能启用默认持久化。
7. 跨平台 job 不稳定时先修复或明确降级，不允许长期 `continue-on-error`。
8. 每个工作流完成后更新最相关指令文件，不生成任务总结 Markdown。

## 15. 后续 Implementation Plan 拆分

用户书面复核本总纲后，implementation plan 必须按以下边界分别生成：

1. Vitest migration and frontend CI
2. Workbench characterization and controller extraction
3. P2P protocol metadata and error envelope
4. Remote Orchestrator runtime snapshot
5. macOS/Windows smoke CI
6. Backend rotating logs and doctor
7. README and layered documentation calibration

这些计划可以按已定义依赖顺序执行；不得生成一个包含全部七项的巨型 implementation plan。

## 16. 总纲完成标准

1. 七个工作流分别达到自身完成标准。
2. 维护者可以从 CI、doctor 和文档判断“什么已验证、什么未验证”。
3. Workbench 修改不再要求理解一个超过 4,000 行的页面文件。
4. 新 P2P 能力不再依靠错误文案或版本猜测判断支持情况。
5. 远端 runtime 不再显示本机替代数据。
6. 后台 backend 失败后存在本地、受限、可脱敏诊断证据。
7. 文档中的定位、命令、协议和发布机制与实现保持一致。
