# cc-partner 全局 Inbox 设计

- 日期：2026-07-11
- 状态：方案已确认，待用户复核书面 spec
- 适用端：Tauri 桌面端、`/mobile`

## 1. 背景

cc-partner 已经具备项目级 Workbench、Orchestrator、远端项目 shortcut、remote task mirror/outbox、tmux 依赖管理和移动端 Workbench。随着项目和设备数量增加，用户需要逐个进入项目自动化面板、任务详情或设置页，才能发现哪些事项正在等待人工决定或阻塞工作继续。

本设计吸收 Happier 的“全局注意力入口”思想，但不把 cc-partner 改造成通知中心、聊天客户端或通用 Agent 平台。Inbox 必须遵循 cc-partner 的产品哲学：本地优先、项目工作台为中心、真实状态为权威、操作留在原业务界面。

## 2. 产品定义

全局 Inbox 是“当前阻塞工作的实时索引”。它回答的问题只有一个：

> 现在有哪些事情需要我处理，工作才能继续？

Inbox 不是：

- 通知历史
- 已读/未读消息箱
- 新的任务管理系统
- Orchestrator 看板替代品
- 可直接执行交付、重试或文件操作的快捷动作面板

## 3. 已确认决策

1. Inbox 采用后端实时投影，不创建 Inbox 业务表。
2. 条目没有已读、忽略、稍后提醒、手工关闭或已解决历史。
3. 条目只要仍然阻塞就持续显示并计数；权威状态恢复后立即消失。
4. Inbox 只解释问题并导航到权威业务界面，不直接执行 Deliver、Retry、Request Rework 等动作。
5. 桌面端使用侧栏固定入口和独立页面。
6. 移动端保持“项目”为默认首屏，在导航第二项增加“待处理”和实时数量。
7. 桌面端和移动端共享同一聚合 DTO、分类、计数和排序语义。
8. 第一版不实现系统 Push。
9. 本机业务操作成功后主动刷新；远端外部变化通过可见期间 10 秒轮询兜底。
10. 远端离线时继续显示最近 mirror 中的待处理任务，明确标识为缓存状态并计入数量。

## 4. 目标

1. 用户从任意桌面页面都能看到当前待处理总数。
2. 用户在一个页面内查看所有项目和设备上当前需要决定或已经阻塞的事项。
3. 用户点击条目后先进入问题解释面，再决定是否交付、返工、重试或打开执行现场。
4. 桌面与移动端针对同一快照显示相同数量和分类。
5. Inbox 不复制 Orchestrator、outbox 或依赖管理的状态机。
6. 新 attention source 必须通过明确接口接入，不能在 Inbox 页面散落业务判断。

## 5. 非目标

第一版不包含：

- outbox `pending`、`sending` 或 `discarded`
- 普通设备上线/下线
- 屏幕录制、输入监控、辅助功能或通知权限缺失
- 文件传输失败
- 健康提醒
- 应用更新
- 普通运行中、排队中或重试中的任务
- 防火墙单次探测异常
- 已解决历史、搜索、筛选、批量操作或用户自定义规则
- Inbox 内直接执行任何具有业务副作用的动作

文件传输失败只有在传输领域先实现失败原因持久化、真实重试和明确终止语义后，才能作为后续 source 接入。权限类事项只有在所属领域能够提供明确的 `blocked capability + reason + resolution target` 后，才能接入。

## 6. 第一版 Attention Source

### 6.1 来源矩阵

| Source | 进入条件 | 分类 | 退出条件 | 目标 |
| --- | --- | --- | --- | --- |
| Orchestrator Human Review | `workflowState === "humanReview"` | `decision` | workflow 不再是 Human Review | 对应任务详情与 Evidence |
| Orchestrator Blocked | `runState === "blocked"`；legacy 数据先经现有兼容映射 | `blocked` | runState 不再是 Blocked | 对应任务详情与 Evidence |
| Remote outbox failed | outbox `status === "failed"`，且原业务界面已提供重新发送与放弃发送 | `blocked` | 重新发送后恢复为 pending，或放弃后进入 discarded | 对应项目自动化面板中的 outbox 条目 |
| tmux dependency | 至少存在一个 Workbench 项目，且状态为 `missing`、`failed` 或 `unsupported` | `environment` | 无 Workbench 项目或依赖恢复 ready | Settings 依赖环境页 |

### 6.2 不把网络状态当作 Source

设备离线不是独立条目。只有离线已经造成明确业务后果，例如 remote outbox failed，才生成 attention item。这样可以避免短暂 mDNS 抖动或自动恢复过程制造待办。

### 6.3 Outbox Failed 的解决闭环

remote outbox failed 接入 Inbox 前，Orchestrator 领域必须提供两个真实动作：

- **重新发送**：只允许 failed 条目原子恢复为 pending，保留原 request payload 和 `clientRequestId`。dispatcher 继续使用同一幂等键，不能创建重复远端任务。
- **放弃发送**：只允许 failed 条目进入新的 `discarded` 终态。discarded 条目不参与 dispatcher、不进入 active outbox 列表、不生成 Inbox 条目，但仍保留领域审计记录。

这两个动作只出现在原项目自动化界面。Inbox 仅导航到 outbox 条目，不复制动作、确认或错误处理。

### 6.4 远端 Mirror

远端 shortcut 在线时，聚合器刷新 owning device 的任务 mirror，并把任务标记为 `freshness=live`。

远端不可达时，聚合器读取最近一次 mirror：

- Human Review 或 Blocked 条目继续显示并计数。
- 条目标记 `freshness=cached`。
- UI 显示“远端缓存 · 最后同步于 …”。
- 设备恢复后立即重新确认权威状态。
- 不用本机 scheduler、runner 或 runtime 数据替代远端数据。

### 6.5 Source 时间语义

- 任务条目的 `updatedAt` 使用任务权威更新时间。
- outbox 条目的 `updatedAt` 使用 outbox 权威更新时间。
- cached 远端条目的 `cachedAt` 使用 mirror 最后成功同步时间，不能使用当前查询时间伪装新鲜度。
- Workbench dependency manager 增加进程内 `statusChangedAt`；仅在依赖状态值发生变化时更新。tmux 条目的 `updatedAt` 使用该时间，不在每次轮询时重置。

## 7. 领域模型

### 7.1 Snapshot

```ts
interface AttentionSnapshot {
  generatedAt: string;
  counts: {
    total: number;
    decision: number;
    blocked: number;
    environment: number;
  };
  items: AttentionItem[];
}
```

### 7.2 Item

```ts
type AttentionCategory = 'decision' | 'blocked' | 'environment';
type AttentionFreshness = 'live' | 'cached';
type AttentionSourceKind =
  | 'orchestratorHumanReview'
  | 'orchestratorBlocked'
  | 'remoteOutboxFailed'
  | 'workbenchDependency';

interface AttentionItem {
  id: string;
  category: AttentionCategory;
  sourceKind: AttentionSourceKind;
  title: string;
  summary: string;
  updatedAt: string;
  freshness: AttentionFreshness;
  cachedAt: string | null;
  project: {
    id: string;
    name: string;
    kind: 'local' | 'remote';
  } | null;
  device: {
    id: string;
    name: string;
  } | null;
  target: AttentionTarget;
}
```

### 7.3 语义化跳转目标

后端不得返回桌面或移动端 URL。它返回语义化 target，由各端转换为自己的导航行为。

```ts
type AttentionTarget =
  | {
      kind: 'orchestratorTask';
      projectId: string;
      taskId: string;
    }
  | {
      kind: 'remoteOutbox';
      projectId: string;
      outboxId: string;
    }
  | {
      kind: 'settings';
      tab: 'dependencies';
    };
```

### 7.4 稳定 ID

稳定 ID 由 source 和权威实体 ID 构造：

```text
orchestrator:human-review:<taskId>
orchestrator:blocked:<taskId>
orchestrator:outbox-failed:<outboxId>
workbench:dependency:tmux
```

同一任务在不同 source 条件间切换时使用不同 ID，避免前端把 Human Review 与 Blocked 当成同一个未变化条目。聚合器按最终 ID 去重。

## 8. 聚合架构

后端新增 `attention` 领域，内部包含：

- `AttentionAggregator`：调用 source、合并、去重、分类、排序和计数。
- `OrchestratorAttentionSource`：读取本机任务、remote mirror 和 remote outbox。
- `WorkbenchDependencyAttentionSource`：读取项目数量与 tmux 依赖状态。
- DTO 映射：生成稳定、camelCase 的前端协议。

数据流：

```text
Orchestrator tasks ─┐
Remote mirrors ─────┤
Remote outbox ──────┼→ AttentionAggregator → AttentionSnapshot
Workbench projects ─┤
tmux dependency ────┘
```

约束：

1. 聚合器不写任务、outbox、依赖或 Inbox 数据。
2. remote project mirror 刷新最多同时执行 4 个请求，不能对设备无限扇出。
3. 网络错误按现有规则回退 mirror；损坏 mirror、仓库错误等非预期 source 错误使整次聚合失败。
4. 聚合失败不能返回看似完整的部分快照，避免条目错误消失。
5. 分类顺序固定为 `decision → blocked → environment`。
6. 分类内按 `updatedAt` 倒序；时间相同按稳定 ID 排序，保证两端顺序确定。

## 9. API

### 9.1 Tauri

```text
list_attention_items() -> AttentionSnapshot
```

### 9.2 Mobile HTTP

```text
GET /api/mobile/attention -> AttentionSnapshot
```

该能力应通过 P2P capability `attention.v1` 暴露。旧后端不支持时，移动端显示明确 unsupported 状态，不猜测或拼接旧接口结果。

## 10. 客户端缓存与刷新

桌面端和移动端各使用一个全局 Attention Provider，但消费相同 DTO 和纯规则 helper。

刷新时机：

1. Provider 首次挂载。
2. 应用或页面重新获得焦点。
3. `document.visibilityState` 恢复为 visible。
4. Deliver、Request Rework、Retry、Refresh、依赖安装等相关业务动作成功。
5. 用户手动点击刷新。
6. 页面可见期间每 10 秒兜底轮询。

并发规则：

- 每次请求携带递增 request ID。
- 旧响应不得覆盖更新请求的结果。
- 组件卸载后不得更新状态。
- 已有完整快照时刷新失败，保留旧快照和数量并标记 stale。
- 初次加载失败时没有可用快照，badge 不显示虚假数字。

## 11. 导航与 Deep Link

### 11.1 桌面端

任务目标进入：

```text
/workbench?projectId=<projectId>&view=automation&taskId=<taskId>
```

outbox 目标进入：

```text
/workbench?projectId=<projectId>&view=automation&outboxId=<outboxId>
```

依赖目标进入：

```text
/settings?tab=dependencies
```

Workbench 必须按 project → automation view → task/outbox 分阶段应用 deep link。任务 deep link 打开详情与 Evidence，不直接打开终端。任务详情继续提供现有“打开执行现场”。Settings 在已经挂载时也必须响应 search params 变化，不能只在首次初始化读取 tab。

### 11.2 移动端

移动端 target mapper：

- `orchestratorTask`：选择项目，进入现有 Automation panel，选择任务并展开详情/Evidence。
- `remoteOutbox`：选择项目，进入现有 Automation panel，聚焦 outbox 条目。
- `settings`：进入现有 Settings panel 的依赖区域。

移动端不得创建第二套任务详情、Evidence 或依赖设置组件。

### 11.3 目标已变化

如果目标已不存在或已经解决：

1. 显示“事项已解决或状态已变化”。
2. 立即刷新 Attention Provider。
3. 返回 Inbox 列表，不进入空白详情或错误终端。

## 12. 桌面 UI/UX

### 12.1 入口

- 在 AppShell 侧栏“首页”之后增加“待处理”。
- 复用现有 `NavItem` badge。
- `total === 0` 时不显示 badge。
- `1..99` 显示真实数字，超过 99 显示 `99+`。

### 12.2 页面

页面标题为“待处理”，副标题为“只保留会阻塞工作继续的事项”。

分组：

- “需要你的决定”
- “运行受阻”
- “环境受阻”

空分组不渲染。每个条目显示：

- 标题
- 原因摘要
- 项目和设备
- 更新时间
- live/cached 状态
- 导航动作文案

动作文案固定按目标语义生成：

- Human Review：前往复核
- Blocked：查看阻塞原因
- outbox failed：查看失败项
- tmux：打开设置

整行可以点击，行内仍保留可聚焦的明确动作控件。

## 13. 移动 UI/UX

- 保持“项目”为默认首屏。
- 导航顺序以 `projects → attention → automation → terminal ...` 开始。
- “待处理”显示与桌面相同的总数规则。
- 内容使用相同三组语义和排序，但采用紧凑列表，不复制桌面宽表格。
- 点击条目后关闭导航抽屉并进入目标 panel。
- 移动端不在顶栏永久增加第二个 attention 按钮。
- 有条目时不自动抢占首屏。

## 14. 状态与错误体验

### 14.1 初次加载

- Inbox 页面显示结构骨架。
- badge 暂不显示。
- 加载完成后一次性显示真实数量。

### 14.2 初次失败

- 页面显示明确错误和“重新加载”。
- badge 不显示。
- 不把错误解释为“当前没有事项”。

### 14.3 后续刷新失败

- 保留最后一次完整列表与 badge。
- 页面顶部显示“状态可能已过期”和最后成功更新时间。
- 提供手动刷新。

### 14.4 空列表

- badge 消失。
- 页面显示“当前没有阻塞工作的事项”。
- 不显示庆祝动画、统计卡或编造数据。

### 14.5 自动移除

条目解决后从列表移除，不弹 toast、不抢焦点、不强制滚动。用户当前正在查看已解决任务详情时，由原业务界面负责呈现最新状态。

## 15. 视觉与可访问性

1. 所有颜色、字体、间距、圆角和阴影复用 `tokens.css`。
2. 不新增 Inbox 专属配色体系。
3. 类别和 freshness 必须有文字，颜色只作为辅助。
4. 使用语义化 `nav`、heading、section 和 list 结构。
5. 桌面和移动端点击目标至少 44×44px。
6. 所有操作支持键盘和清晰的 `focus-visible`。
7. 自动刷新只更新低打扰状态文本，不使用会反复朗读整张列表的强制 live region。
8. 中英文文案进入独立 Inbox i18n namespace；业务错误摘要仍来自权威领域的结构化字段。
9. 浅色和深色必须使用现有两套 token 完整验证。

视觉探索已经确认：

- 桌面选择独立页面方案，不使用全局浮层或 Workbench 项目抽屉。
- 移动选择导航第二项方案，不使用顶栏永久角标或有阻塞时自动抢占首屏。

## 16. 测试设计

### 16.1 Rust 单元与集成测试

必须覆盖：

1. 四类 source 的进入和退出条件。
2. pending/sending/discarded、已解决任务和无项目时 tmux 的排除规则。
3. 稳定 ID、去重、分类顺序、时间排序和相同时间 tie-break。
4. 在线 remote 为 live，离线 mirror 为 cached 并保留时间。
5. remote mirror 损坏和 source 仓库失败不返回误导性部分快照。
6. Tauri command 与 Mobile HTTP 的 DTO 一致性。
7. legacy 远端不支持 `attention.v1` 的明确 unsupported 行为。
8. 远端不可达时 mirror/outbox 组合规则。
9. failed outbox 重新发送后原子恢复 pending 并保留 `clientRequestId`；放弃后进入 discarded 且不再参与聚合。

### 16.2 前端单元测试

必须覆盖：

1. badge 的零项、普通数字和 `99+`。
2. 三组渲染、空组隐藏、空态和 cached 标识。
3. 初次错误与刷新错误保留旧快照。
4. request ID stale guard。
5. 已知业务操作成功后的主动失效刷新。
6. desktop target 到 URL 的映射。
7. mobile target 到 panel/project/task/outbox 的映射。
8. 目标已解决时的刷新和回退。
9. Hooks 位于所有 early return 之前。

### 16.3 端到端场景

1. 创建 Human Review 任务，两端 badge 增加；Request Rework 后立即消失。
2. 创建 Blocked 任务，点击后打开对应详情与 Evidence。
3. pending/sending/discarded outbox 不显示，failed 后显示并跳到对应条目。
4. failed outbox 在原业务界面重新发送后立即从 Inbox 消失；放弃后进入 discarded 并保持不显示。
5. 远端在线时显示 live；断开后使用 cached mirror；恢复后重新确认状态。
6. 无 Workbench 项目时 tmux 缺失不显示；添加项目后显示并跳到依赖设置。
7. 模拟刷新失败，旧列表和数量保持不变并显示 stale。
8. 目标在点击前已解决时不进入空白详情。

## 17. 文档与项目记忆

实现完成时同步更新：

- `docs/prd.md`：记录 Inbox 的持久产品行为、source 范围和双端入口。
- 根 `AGENTS.md`：新增 domain/layout 组件时更新组件清单。
- `web/CLAUDE.md`：记录 Inbox 测试命令、Provider 和 deep link 约束。
- `src-tauri/CLAUDE.md`：记录 attention 聚合、P2P capability 和无 Inbox 表约束。

设计草图保存在 gitignored 的 `.superpowers/brainstorm/`，不作为生产资产提交。

## 18. 完成标准

1. 不新增 Inbox 数据表、已读状态或用户交互元数据。
2. 桌面和移动端对同一 snapshot 显示相同总数。
3. 本机解决动作成功后立即刷新；远端外部变化在可见状态下最迟 10 秒反映。
4. cached 远端条目不会伪装成 live。
5. 所有条目只导航，不直接执行副作用。
6. 空列表、首次错误、刷新错误和目标已变化都有明确体验。
7. Inbox 相关 Rust、Vitest、Playwright、lint 和 build 全部通过。
8. PRD 与相关分层指令同步更新。
