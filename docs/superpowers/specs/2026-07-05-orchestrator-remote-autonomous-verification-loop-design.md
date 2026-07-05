# Orchestrator 远端自治与 Claude 验证闭环设计

- 日期：2026-07-05
- 状态：方案已确认，待实现计划
- 范围：Workbench 自动化工作区、Orchestrator 后端、局域网远端 Workbench/Orchestrator 协议、项目策略编辑

## 1. 背景

当前 Orchestrator 已经具备项目任务队列、项目策略读取、Workbench 可见 Runner、验证命令执行、delivery evidence 和 full-auto Git 交付能力。但现状有三个明显缺口：

1. 项目并发上限等策略只存在于后端 `orchestrator_project_config` 表，前端只能展示，不能设置。
2. Runner 只支持本机 Workbench 项目。远端项目会在 `prepare_visible_runner` 阶段直接失败。
3. 开发完成后的验证需要用户手动点击，且验证失败后任务进入 Blocked，不会自动把失败原因交给新的 Claude 继续修复。

本设计把自动化能力升级为：项目策略可配置、局域网远端项目由远端设备自治执行、本机可离线暂存远端任务、开发与验证形成 Claude 闭环。

## 2. 已确认决策

1. 验证采用混合模式：先执行项目验证命令，再把命令输出、任务目标、验收标准和当前 diff 交给验证 Claude，由验证 Claude 最终裁决 `pass/fail`。
2. 开发与验证循环不设置固定轮数。只要验证 Claude 判定未通过，就继续自动进入下一轮开发，直到通过或用户主动终止。
3. 远端项目采用远端自治。远端项目的队列、Runner、验证循环和交付都由远端设备自己的 cc-partner 接管，本机只负责展示、操作和同步。
4. 远端设备离线时，本机允许创建 pending 任务。远端上线后自动投递给远端 Orchestrator。
5. 验证未通过后的修复使用同一个任务 worktree，但创建新的 terminal window 和新的开发 Claude，把失败反馈作为修复 Prompt 注入。

## 3. 目标

1. 在 Workbench 自动化策略卡中提供项目策略编辑能力，覆盖启用开关、并发上限、验证命令和自动交付开关。
2. 本机项目继续由本机 Orchestrator 执行，远端项目由远端 Orchestrator 自治执行。
3. 本机在远端离线时允许创建 pending remote task，并在远端恢复在线后自动投递。
4. 开发任务完成后自动进入验证闭环，不再依赖用户手动点击“开始验证”。
5. 验证失败时不直接 Blocked，而是在同一 worktree 中新建 terminal window，启动新的开发 Claude 继续修复。
6. 每一轮开发、验证命令、验证 Claude 裁决、修复反馈和最终交付都写入 evidence。
7. 用户始终可以从 Workbench 接管任务现场，或主动 Abort。

## 4. 非目标

1. 不做多设备 worker pool。任务不会由任意设备抢占执行，只由项目所在设备自治。
2. 不做 PR 创建、PR review 自动化或外部 issue 系统接入。
3. 不做强沙箱。仍按 cc-partner 可信本机和可信局域网设备定位。
4. 不把远端任务完整复制成本机任务 source of truth。本机只保存 pending outbox 和远端任务镜像。
5. 不做跨设备同一任务的协同编辑。远端任务一旦投递成功，执行权归远端设备。

## 5. 用户体验

### 5.1 项目策略编辑

自动化工作区的策略卡从只读展示升级为可编辑：

- `enabled`：是否允许 scheduler 自动领取该项目任务。
- `maxConcurrentTasks`：项目并发任务上限，前端输入范围 1 到 8，后端拒绝小于 1 或大于 8。
- `verificationCommands`：多行命令列表，空行忽略。
- `autoCommit`、`autoPushTaskBranch`、`autoMergeToMain`、`autoPushMain`：full-auto delivery 四阶段开关。

保存后立即刷新策略卡。scheduler 每个 tick 重新读取数据库策略，因此无需重启应用。

### 5.2 远端项目任务创建

当 active project 是远端项目时：

- 远端在线：前端创建任务会调用本机命令，本机通过 P2P 把任务请求转发给远端设备；远端创建真实 task 并返回 remote task DTO，本机展示镜像。
- 远端离线：前端仍允许创建任务，本机写入 pending outbox。任务显示为“待发送到远端”，不可入队和执行。
- 远端重新在线：后台 outbox dispatcher 自动投递 pending 任务。投递成功后，本机 pending 项变成远端 task 镜像。

### 5.3 自动开发与验证闭环

任务被 scheduler 领取后：

1. 创建任务 worktree。
2. 创建开发 terminal window。
3. 启动开发 Claude 并注入任务 Prompt。
4. 开发 Claude 结束或声明完成后，后端自动进入验证阶段。
5. 后端执行项目验证命令，收集输出。
6. 后端启动验证 Claude，输入任务目标、验收标准、验证命令输出、当前 Git diff 和必要的文件摘要。
7. 验证 Claude 输出结构化裁决。
8. 如果裁决为通过，任务进入 Delivering。
9. 如果裁决为不通过，任务进入下一轮 Running，在同一 worktree 中创建新的开发 terminal window，启动新的开发 Claude，注入修复 Prompt。

循环不设置固定次数。`attempt` 每轮递增。用户可以随时 Abort，Abort 保留 worktree、terminal 和 evidence。

## 6. 任务状态模型

保留现有状态，但扩展语义：

```text
Draft
Queued
Preparing
Running
Verifying
Delivering
Done
Blocked
Aborted
```

新增内部阶段概念，不一定扩展前端主状态枚举：

- `developmentAttempt`：第几轮开发。
- `verificationAttempt`：第几轮验证。
- `remotePending`：本机 outbox 中尚未投递到远端的任务，不进入远端 Orchestrator 状态机。
- `remoteMirrored`：本机展示远端任务镜像，不由本机 scheduler 领取。

状态流：

```text
Draft -> Queued -> Preparing -> Running -> Verifying
Verifying + verifier pass -> Delivering -> Done
Verifying + verifier fail -> Running
任意阶段 + 用户 Abort -> Aborted
基础设施错误 -> Blocked
```

验证不通过属于任务质量问题，继续循环。基础设施错误包括执行设备上的 Claude CLI 无法启动、验证器输出不可解析、worktree 丢失、Git 交付失败等，这些进入 Blocked。本机视角下的远端离线不改写远端任务状态，只让本机 pending 或镜像刷新暂停。

## 7. 后端架构

### 7.1 项目策略更新命令

新增命令：

- `update_orchestrator_project_config(projectId, patch)`

后端校验：

- `projectId` 非空。
- `maxConcurrentTasks` 在 1 到 8。
- `verificationCommands` trim 后过滤空行，序列化为 JSON 数组。
- delivery 四个开关可以单独关闭，但如果任务进入 Delivering 时任一关闭，现有 delivery pipeline 继续 Blocked，避免静默跳过交付阶段。

前端 `orchestratorApi` 增加 `updateProjectConfig`。

### 7.2 远端自治 API

远端设备新增 P2P HTTP endpoints：

- `POST /api/orchestrator/tasks/create`
- `POST /api/orchestrator/tasks/{id}/queue`
- `GET /api/orchestrator/tasks?projectId=...`
- `GET /api/orchestrator/tasks/{id}/evidence`
- `POST /api/orchestrator/tasks/{id}/retry`
- `POST /api/orchestrator/tasks/{id}/abort`
- `GET /api/orchestrator/project-config?projectId=...`
- `POST /api/orchestrator/project-config/update`

这些 endpoint 在远端设备上复用本机 Tauri command 的业务逻辑，不复制第二套状态机。

本机 remote client 只负责协议转发和错误映射。远端项目必须先通过现有 `/api/workbench/projects/open` 确保远端本机 project row 存在，再用远端 local project id 创建 Orchestrator task。

### 7.3 Pending outbox

本机新增表 `orchestrator_remote_outbox`：

- `id`
- `device_id`
- `device_name`
- `remote_project_path`
- `remote_project_id`
- `request_json`
- `status`: `pending | sending | mirrored | failed`
- `remote_task_id`
- `last_error`
- `created_at`
- `updated_at`
- `sent_at`

后台 outbox dispatcher：

1. 监听设备在线状态或按固定 interval 扫描 pending outbox。
2. 对在线设备，先确保远端项目打开。
3. 投递创建任务请求。
4. 成功后保存 `remote_task_id`，状态置为 `mirrored`。
5. 失败时保存 `last_error`，状态回到 `pending` 或 `failed`。网络离线类错误保持 `pending`，协议或校验类错误置为 `failed`。

前端任务列表合并展示：

- 本机项目：本机任务列表。
- 远端在线项目：远端任务列表 + 本机尚未投递的 pending 项。
- 远端离线项目：本机 pending 项 + 最近一次远端镜像快照。

### 7.4 自动完成检测

当前 Running 任务需要用户手动点击完成。新设计中 Runner 启动 Claude 时应使用可观测输出协议：

- 开发 Claude 仍在可见 terminal window 中运行。
- 任务 Prompt 要求 Claude 完成后输出明确 sentinel，例如 `ORCHESTRATOR_DEV_DONE`。
- 后端 terminal output 监听器检测 sentinel 后，自动触发 `complete_orchestrator_agent_run`。
- 如果 terminal 异常退出且未输出 sentinel，任务进入 Blocked，原因记录为开发 Claude 未完成。

sentinel 只作为自动推进信号，不作为质量判定。质量由验证命令和验证 Claude 决定。

### 7.5 验证 Claude

新增验证器模块，例如 `orchestrator/verifier.rs`。

输入：

- 任务标题、目标、验收标准。
- 当前 attempt。
- 验证命令列表和输出。
- 当前 worktree Git diff 摘要及完整 diff 上限截断文本。
- 最近一轮开发 evidence。

输出必须是 JSON：

```json
{
  "passed": false,
  "reason": "未满足验收标准的具体原因",
  "repairPrompt": "给下一轮开发 Claude 的具体修复指令",
  "riskNotes": ["需要人工关注的风险"]
}
```

解析规则：

- `passed=true` 时 `repairPrompt` 可以为空。
- `passed=false` 时 `repairPrompt` 必须非空。
- JSON 不可解析、字段缺失或 Claude CLI 启动失败属于验证基础设施错误，任务进入 Blocked。

### 7.6 失败后继续开发

验证 Claude 判定失败时：

1. 写入 `verificationReview` evidence，summary 为 `failed`。
2. `attempt += 1`。
3. 任务从 Verifying 回到 Preparing。
4. 在同一 worktree 中创建新的 terminal window。
5. 新 terminal 创建并挂账成功后，任务重新进入 Running，并启动新的开发 Claude。
6. Prompt 包含原始任务、验收标准、验证命令输出、验证 Claude 的 `reason` 和 `repairPrompt`。

旧 terminal window 不删除。前端任务详情展示当前 active session，同时保留历史 session id 列表或 evidence 中的 session 引用，方便回溯。

## 8. 前端架构

### 8.1 OrchestratorPanel

OrchestratorPanel 保持只拥有任务看板、任务详情、创建表单、Evidence 和策略卡。

新增：

- 策略编辑模式。
- pending remote task 样式。
- attempt 轮次展示。
- 验证闭环状态展示，例如“第 4 轮开发中”“验证 Claude 未通过，正在重新修复”。
- 远端自治提示：远端项目的自动化由设备名对应的远端 cc-partner 执行。

### 8.2 Workbench deep link

远端 task mirror 的 `projectId/worktreeId/sessionId` 使用现有 remote id 前缀规则。打开 Workbench 时：

- 远端在线：切换到 remote project shortcut，并聚焦远端 worktree/session。
- 远端离线：展示离线提示，不丢失 pending 或 mirror 状态。

## 9. Evidence 设计

新增 evidence kind：

- `developmentAttempt`：每轮开发启动、session id、Prompt 摘要。
- `verificationOutput`：验证命令输出，沿用现有 kind。
- `verificationReview`：验证 Claude 的 JSON 裁决和自然语言摘要。
- `repairPrompt`：传给下一轮开发 Claude 的修复指令。
- `remoteOutbox`：pending 投递、投递失败、投递成功记录。
- `delivery`：沿用现有交付证据。

Evidence 按 task id、created_at、id 稳定排序。

## 10. 错误处理

- 远端离线：本机 pending outbox 保留，UI 显示待发送，不进入 Blocked。
- 远端投递协议错误：outbox 置 failed，显示 last_error。
- 远端执行期间离线：本机镜像停止刷新；远端任务状态以远端恢复后的真实状态为准。
- 开发 Claude 未输出完成 sentinel 即退出：Blocked。
- 验证命令失败：不会直接 Blocked，而是作为输入交给验证 Claude。
- 验证 Claude 判定失败：继续下一轮开发。
- 验证 Claude 无法启动或输出不可解析：Blocked。
- 交付阶段 Git 失败：沿用现有 delivery Blocked 逻辑。
- 用户 Abort：任意阶段立即置 Aborted，不删除 worktree/session。

## 11. 测试计划

后端：

- 项目策略更新校验：并发范围、验证命令 trim、delivery flags 保存。
- pending outbox：离线创建、上线投递、协议失败、重复投递幂等。
- 远端 Orchestrator HTTP endpoints：创建、列表、evidence、retry、abort、配置更新。
- scheduler：本机项目本机领取，远端项目不由本机 scheduler 领取。
- 验证闭环：验证命令失败但验证 Claude 判定 fail 后进入下一轮 Running；判定 pass 后进入 Delivering。
- verifier JSON 解析失败进入 Blocked。

前端：

- 策略卡编辑和保存。
- pending remote task 展示。
- 远端离线时创建任务进入 pending。
- 远端在线时任务列表展示远端真实任务和 pending 合并结果。
- attempt 和 evidence 展示。
- Blocked/Abort 控制仍只作用于当前项目任务。

集成：

- 两台 cc-partner 局域网设备，远端项目离线创建 pending，上线后自动投递。
- 远端任务完成验证闭环并在远端完成 full-auto delivery。
- 本机从 mirror deep link 打开远端 Workbench 现场。

## 12. 迁移与兼容

这是内部开发阶段能力，不要求向后兼容旧 Orchestrator 行为。数据库迁移采用 `CREATE TABLE IF NOT EXISTS` 和必要的 `ALTER TABLE` 检查式迁移。

现有任务状态保持可读。旧 Running 任务仍可通过手动按钮进入验证，但新 Runner 创建的任务默认走 sentinel 自动完成。

## 13. 实施顺序建议

1. 项目策略 update 命令和前端编辑入口。
2. 远端 Orchestrator HTTP API 与 remote client。
3. pending outbox 和远端任务镜像展示。
4. Runner 自动完成 sentinel。
5. 验证 Claude 模块。
6. 验证失败后同 worktree 新 terminal 修复循环。
7. Evidence 和 UI 轮次展示完善。

这个顺序先补上“能配置并发”和“远端自治基础”，再改动更高风险的自动闭环。
