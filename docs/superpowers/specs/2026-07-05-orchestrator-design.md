# cc-partner Orchestrator 自动编排器设计

- 日期：2026-07-05
- 状态：基础方案已确认；配置模型已由后续远端自治/验证闭环设计修订为 Settings 全局自动化配置
- 参考：OpenAI Symphony [README](https://github.com/openai/symphony/blob/main/README.md)、[SPEC](https://github.com/openai/symphony/blob/main/SPEC.md)、[Elixir README](https://github.com/openai/symphony/blob/main/elixir/README.md)

## 1. 背景

cc-partner 现有 Workbench 已经具备本机/远端项目、Git worktree、tmux-backed terminal window/pane、文件工作区、Git 提交树、Prompt 优化和移动端 Workbench 能力。当前缺口不是“再加一个终端管理器”，而是把 Claude Code 任务执行提升到更高一层：用户管理任务和交付结果，系统负责准备隔离工作区、启动 Claude Code、观察运行状态、执行验证与交付动作。

OpenAI Symphony 的核心思想是把项目工作转成长期自动化服务：读取任务、创建隔离 workspace、运行 coding agent、保留可观测状态、处理 blocked/retry，并让工程师管理工作而不是盯着 agent。cc-partner 第一版不直接复制 Symphony 的 Linear 与 Codex app-server 形态，而是基于当前产品能力做本地化设计：内置任务队列 + 可见 tmux Runner + Workbench 深链接接管。

## 2. 已确认决策

- Orchestrator 自动化工作区迁入 Workbench 中心区域，作为终端和文件工作区同级的 workspace view；旧独立路由仅作为 deep link 兼容入口。
- 第一版任务源使用 **cc-partner 内置任务队列**，暂不接 Linear/GitHub Issues。
- Runner 使用 **可见 tmux 终端 Runner**：复用现有 Workbench terminal window/pane。
- 自动化交付策略为全自动：自动 commit、push 任务分支、merge 回主 worktree/主分支、push 主分支。
- 配置入口采用设备级全局形态：scheduler 启用、并发、验证命令和交付开关统一放到 Settings 自动化 tab；任务队列仍按项目隔离。

## 3. 目标

1. 在 Workbench 自动化工作区提供当前项目任务看板，让用户看到该项目的 Draft、Queued、Running、Blocked、Done 任务。
2. 支持在 cc-partner 内创建任务，并绑定到某个 Workbench 项目。
3. 调度器按 Settings 自动化 tab 中的设备级并发限制自动领取本机 Queued 任务。
4. 每个任务创建独立 Git worktree，避免污染主工作区。
5. 每个任务创建可见 tmux terminal window，自动启动 Claude Code 并写入任务 Prompt。
6. 用户可以随时从 Orchestrator 跳转 Workbench，进入该任务绑定的 project/worktree/session 接管。
7. 任务完成后执行 Settings 自动化 tab 配置的验证命令。
8. 验证通过后自动完成 commit、任务分支 push、merge 主分支、push 主分支。
9. 任一阶段失败时进入 Blocked，保留 worktree、终端、错误原因和重试入口。
10. 提供完整交付证据链：diff 摘要、验证结果、commit hash、push/merge/push main 结果。

## 4. 非目标

1. 第一版不接 Linear、GitHub Issues、Jira 或其他外部任务源。
2. 第一版不实现 Codex app-server runner。
3. 第一版不做多机器 worker pool；远端项目仍沿用现有 Workbench 远端代理能力。
4. 第一版不做 PR review 自动化、PR 创建或视频 walkthrough。
5. 第一版不做通用工作流引擎；只服务 Claude Code 项目任务自动执行。
6. 第一版不做强权限沙箱，仍按 cc-partner 可信本机/局域网工具定位，但 UI 必须明确展示自动交付风险。

## 5. 信息架构

Workbench 中心工作区新增「自动化」视图，与终端层、文件工作区同级；旧「Orchestrator」路由重定向到 Workbench。

```text
Workbench 自动化视图 / OrchestratorPanel
├─ 左栏：当前项目任务看板
│  ├─ 状态分组：Draft / Queued / Running / Blocked / Done
│  ├─ 项目筛选
│  ├─ 状态筛选
│  └─ 新建任务
│
├─ 中栏：任务详情与运行现场
│  ├─ 任务目标
│  ├─ 验收标准
│  ├─ 当前阶段
│  ├─ 绑定 project / worktree / terminal window
│  ├─ 运行日志摘要
│  └─ 跳转 Workbench 接管
│
└─ 右栏：交付证据
   ├─ Diff summary
   ├─ 验证命令结果
   ├─ commit hash
   ├─ branch push 结果
   ├─ merge main 结果
   ├─ push main 结果
   ├─ blocked reason
   └─ retry / abort / cleanup
```

Workbench 继续负责项目现场，不承担全局调度看板职责。Orchestrator 中点击 Running/Blocked 任务时，应深链接到 Workbench，并自动选中对应 project、worktree 与 terminal window。

## 6. 任务状态流

```text
Draft
  用户创建但尚未进入调度队列。

Queued
  等待调度器领取。

Preparing
  正在创建 worktree、初始化任务元数据、创建 terminal window。

Running
  Claude Code 已在可见 tmux 终端中运行。

Verifying
  Claude Code 任务结束后，系统执行 Settings 自动化 tab 中配置的验证命令。

Delivering
  系统正在 commit、push 任务分支、merge 主分支、push 主分支。

Done
  全部交付动作成功完成。

Blocked
  任一阶段失败、验证失败、merge 冲突、push 失败、Claude Code 等待人工输入或策略拒绝时进入。

Aborted
  用户主动停止任务。
```

Blocked 状态必须记录阻塞阶段、错误摘要、完整日志入口、是否可重试、是否可人工接管、是否需要清理 worktree。

## 7. 核心交互

### 7.1 新建任务

用户点击「新建任务」后填写：

- 项目。
- 任务标题。
- 任务目标。
- 验收标准。
- 可选关联文件/目录。
创建后默认进入 Draft。用户点击「加入队列」后变为 Queued。

### 7.2 自动调度

调度器按设备级全局配置工作：

- Settings 自动化 tab 的自动调度开关关闭时，不领取本机任务。
- 本设备共享一个最大并发任务数。
- 本机 local Workbench 项目的 Running/Preparing/Verifying/Delivering 任务总数达到并发上限时，后续任务留在 Queued。
- 远端项目由远端设备自己的全局自动化配置自治执行。

### 7.3 Runner 准备

任务进入 Preparing 后：

1. 基于项目主 worktree 创建 `agent/<task-id>` 风格的任务分支。
2. 创建或记录任务 worktree 元数据。
3. 创建 Workbench terminal window，绑定该 worktree cwd。
4. 在终端中启动 Claude Code。
5. 自动写入任务 Prompt。

Prompt 必须包含：

- 任务标题和目标。
- 验收标准。
- 当前项目路径和 worktree 分支。
- Settings 自动化 tab 中的自动交付开关语义。
- 项目 AGENTS.md/CLAUDE.md 遵守要求。
- 完成后必须停止在可验证状态，不让 Claude Code 自行执行系统外不可追踪动作。

### 7.4 运行观察与接管

Running 状态下，Orchestrator 展示：

- 当前 terminal window 名称。
- 运行时长。
- 最近终端输出摘要。
- 当前 git dirty 状态。
- 最近阶段事件。
- 「打开 Workbench」按钮。

用户打开 Workbench 后，可以直接输入、分屏、看文件、看 Git 历史。接管不改变任务状态；如果用户手动终止终端，则任务进入 Blocked 或 Aborted，取决于动作来源和后端检测结果。

### 7.5 验证

Claude Code 终端运行结束或系统检测到任务声明完成后，任务进入 Verifying。

验证命令来自 Settings 自动化 tab 的设备级配置。无命令时记录 skipped verification evidence，并继续交给验证 Claude 结合任务目标、验收标准和 diff 做最终裁决；命令启动/读取/超时等基础设施错误才进入 Blocked。

验证输出需要归档到任务证据链。失败时不进入 Delivering。

### 7.6 自动交付

验证通过后进入 Delivering：

1. 在任务 worktree 中执行 commit。
2. push 任务分支。
3. 切换/解析主 worktree。
4. merge 任务分支到主分支。
5. push 主分支。
6. 刷新 Workbench worktrees、sessions、Git history。
7. 记录 commit hash、分支名、merge 结果和 push main 结果。

自动交付失败时必须保留现场，不得静默清理 worktree。merge 冲突、主分支 dirty、push 被拒绝、验证命令失败都进入 Blocked。

## 8. Settings 全局自动化配置

每台 cc-partner 设备在 Settings 的「自动化」tab 中维护一份 Orchestrator 运行配置：

- `enabled`：是否允许自动调度。
- `maxConcurrentTasks`：最大并发任务数。
- `verificationCommands`：验证命令列表。
- `autoCommit`：是否自动 commit。
- `autoPushTaskBranch`：是否自动 push 任务分支。
- `autoMergeToMain`：是否自动 merge 主分支。
- `autoPushMain`：是否自动 push 主分支。
- `retryLimit`：预留字段；当前验证修复循环不设置固定轮数，由用户 Abort 终止。

后端 legacy `orchestrator_project_config` 表仅保留存储兼容和调试读取能力，不作为用户可见配置路径，也不影响 scheduler、验证或 delivery runtime。

## 9. 数据模型草案

### 9.1 OrchestratorTask

- `id`
- `projectId`
- `title`
- `goal`
- `acceptanceCriteria`
- `status`
- `priority`
- `branchName`
- `worktreeId`
- `sessionId`
- `createdAt`
- `updatedAt`
- `startedAt`
- `finishedAt`
- `blockedReason`
- `attempt`

### 9.2 OrchestratorTaskEvent

- `id`
- `taskId`
- `kind`
- `message`
- `payloadJson`
- `createdAt`

事件用于记录状态变化、终端摘要、验证开始/结束、commit/push/merge 结果、阻塞原因。

### 9.3 OrchestratorEvidence

- `id`
- `taskId`
- `kind`
- `title`
- `summary`
- `content`
- `createdAt`

证据类型包括 `diffSummary`、`verificationOutput`、`commit`、`pushBranch`、`mergeMain`、`pushMain`。

### 9.4 OrchestratorAutomationConfig

设备级全局配置存储在 `AppConfig.orchestrator`，字段见第 8 节。legacy `OrchestratorProjectConfig` 仅用于历史数据兼容。

## 10. 后端架构

建议新增 `src-tauri/src/orchestrator/` 领域模块：

- `models.rs`：任务、事件、证据和 legacy 项目配置 DTO。
- `repo.rs`：SQLite 持久化。
- `scheduler.rs`：设备级全局调度循环、并发控制、重试。
- `runner.rs`：调用 Workbench worktree/session 能力创建可见 Runner。
- `delivery.rs`：验证、commit、push、merge、push main。
- `prompt.rs`：任务 Prompt 生成。
- `events.rs`：向前端 emit `orchestrator:*` 事件。

复用现有能力：

- Workbench project 记录。
- Workbench worktree 创建/移除。
- tmux session/window 创建与输入写入。
- Git commit/push/merge 边界。
- terminal output buffer。
- Prompt 优化/Claude CLI 配置。

不应复制第二套 PTY、Git worktree 或远端项目代理逻辑。

## 11. 前端架构

建议新增：

- `web/src/pages/Orchestrator/`
  - 页面主布局。
  - 全局任务队列。
  - 任务详情。
  - 证据链。
  - Evidence 面板。
- `web/src/api/orchestrator.ts`
  - Tauri invoke 封装。
- `web/src/lib/orchestrator.ts`
  - 状态机 helper、任务排序、按钮可用性。
- `web/src/i18n/locales/{zh,en}/orchestrator.json`
  - 页面文案。

现有 Workbench 需要提供深链接能力：

- 从 Orchestrator 跳转 `/workbench?projectId=<projectId>&worktreeId=<worktreeId>&sessionId=<sessionId>`。
- Workbench 进入后选中对应 project/worktree/session。
- 若 session 已关闭，显示任务现场不可用提示。

## 12. 错误处理

- Claude Code 启动失败：Blocked，保留 worktree，记录 stderr/错误。
- 终端退出码非 0：Blocked。
- 验证命令失败：Blocked，记录命令、退出码、输出。
- 主 worktree dirty：Blocked，不自动 merge。
- merge 冲突：Blocked，保留冲突状态和任务 worktree。
- push 任务分支失败：Blocked，保留本地提交。
- push main 失败：Blocked，记录本地主分支已 merge 但远端未更新。
- 用户手动 abort：Aborted，按配置决定是否清理 worktree。

Blocked 任务必须提供：

- 重试当前阶段。
- 打开 Workbench 接管。
- 标记为已人工处理。
- 中止并保留现场。

## 13. 测试策略

### 13.1 Rust

- 调度器状态机单测：Queued → Preparing → Running → Verifying → Delivering → Done。
- 并发控制单测：同项目并发限制、不同项目互不影响。
- 阻塞状态单测：验证失败、merge 冲突、push 失败。
- repo 单测：任务、事件、证据 CRUD。
- delivery 单测：使用临时 Git repo 验证 commit/push/merge/push main 边界。

### 13.2 前端

- helper 单测：状态分组、任务排序、操作按钮可用性。
- Orchestrator 页面状态测试：空态、Running、Blocked、Done。
- 深链接 helper 测试：projectId/worktreeId/sessionId 参数解析。
- i18n key 通过 `npm run build` 校验。

### 13.3 手工验证

- 在本地测试仓库创建任务并自动完成完整交付链。
- 验证失败时进入 Blocked 且保留 worktree/terminal。
- push main 失败时能明确显示“本地已 merge，远端未 push”。
- 从 Orchestrator 跳转 Workbench 后能选中正确终端。

## 14. 分阶段实现建议

### Phase 1：任务与全局配置

- 数据表。
- API。
- Orchestrator 页面静态与 CRUD。
- Settings 自动化 tab。

### Phase 2：可见 Runner

- 自动创建 worktree。
- 自动创建 Workbench terminal window。
- 自动写入任务 Prompt。
- Running/Blocked 基础状态。

### Phase 3：验证与证据链

- 验证命令执行。
- 事件和证据归档。
- 失败阻塞与重试。

### Phase 4：全自动交付

- 自动 commit。
- push 任务分支。
- merge 主分支。
- push 主分支。
- Done/Blocked 完整证据展示。

## 15. 相关后续能力

第一版稳定后，可以继续扩展：

- Linear / GitHub Issues connector。
- Codex app-server 后台 Runner。
- 多设备 worker pool。
- PR 创建与 review 证据。
- 移动端 Orchestrator 状态查看。
- 项目级 WORKFLOW.md 契约可以作为任务说明/验证建议来源，但不应重新引入用户可见的项目级自动化配置。

## 16. 自检

- 本设计聚焦单一第一版目标：内置任务队列 + 可见 Runner + 全自动交付。
- 没有把 Linear/GitHub、app-server、多 worker pool 塞入第一版。
- 自动交付风险由 Settings 全局配置显式控制，并由 Blocked 兜底。
- 与现有 Workbench 的职责边界明确：Orchestrator 管任务，Workbench 管现场。
- 测试面覆盖调度、状态机、交付失败和深链接。
