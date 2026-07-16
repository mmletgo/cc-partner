# Agent State Projection 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：`2026-07-15-agent-session-runtime-design.md`
- 对应计划：`docs/superpowers/plans/2026-07-15-agent-state-projection.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

Agent Runtime 即使成为 owner 真值，如果只停留在后端，用户仍需逐个打开 terminal 判断状态。现有 Desktop terminal tab 只知道 terminal running/exited，Mobile 能解析 terminal status 但尚未实时消费；Attention 也没有 terminal/Agent target。

本 spec 负责把统一 runtime 转换为低噪音、可恢复、只导航的用户投影。

## 2. 目标

1. Desktop 与 Mobile 自动展示当前 Agent provider、phase 与最后活动时间。
2. `needsInput/failed` 自动进入 Attention；正常 working/idle/completed 不制造 Inbox 噪音。
3. 系统通知让用户不必持续盯住应用，但不从通知执行任何业务动作。
4. remote offline、cached、unsupported 与 live 明确区分。
5. event Gap、owner restart 和前端重挂载不会重复 Attention 或通知。

## 3. 非目标

- 不实现 Agent 启动、resume、任务审批或 inline 输入。
- 不展示 Prompt、回复、terminal transcript、diff 或 evidence 正文。
- 不把 Attention 变成已读/未读、snooze、队列审批或持久消息中心。
- 不新增全局 Quick Open。
- 不承担跨项目 Fleet 聚合；A6 只消费本 spec 的投影 helper。

## 4. 展示模型

```ts
export type AgentPhase =
  | 'launching'
  | 'working'
  | 'needsInput'
  | 'idle'
  | 'completed'
  | 'failed'
  | 'disconnected'

export interface AgentSessionProjection {
  id: string
  projectId: string
  worktreeId?: string
  terminalSessionId: string
  taskId?: string
  providerId: string
  phase: AgentPhase
  lastActivityAt: string
  freshness: 'live' | 'cached' | 'offline' | 'unsupported'
}
```

显示规则：

- terminal tab 只显示一个状态点、provider 短标签和必要的 `needsInput/failed` 文案。
- working/idle 不闪烁、不循环动画；遵守 `prefers-reduced-motion`。
- completed 在当前 session 上短暂显示后归入普通 terminal 状态，不常驻 badge。
- remote cached 必须显示缓存标识和时间，不能伪装在线。

## 5. Desktop 与 Mobile

### 5.1 Desktop

- `AgentRuntimeProvider` 负责 snapshot→listen handshake、Gap 重建和按 session 索引。
- terminal tab、session list 与 task detail 只消费 selector，不直接调用 API。
- 点击状态进入已经存在的 terminal/task authority，不打开新命令面板。
- Hooks 保持在所有 early return 之前；Workbench 不新增页面级聚合 controller。

### 5.2 Mobile

- 先修复现有 `terminalStatus` 已解析但未消费的问题。
- mobile runtime store 同时消费 `terminalStatus` 与 `agentRuntime`。
- Agent 状态只出现在当前项目/会话；离开页面后不轮询所有 remote shortcut。
- `needsInput` 点击导航到相应 terminal；不在 Mobile Attention 行内发送输入。

## 6. Attention v2

新增 source：

```rust
pub enum AttentionSourceKind {
    OrchestratorHumanReview,
    OrchestratorBlocked,
    RemoteOutboxFailed,
    WorkbenchDependency,
    AgentNeedsInput,
    AgentFailed,
    ExperimentNeedsDecision,
}

pub enum AttentionTarget {
    OrchestratorTask { project_id: String, task_id: String },
    RemoteOutbox { project_id: String, outbox_id: String },
    Settings { section: String },
    AgentSession {
        project_id: String,
        worktree_id: Option<String>,
        terminal_session_id: String,
        agent_session_id: String,
    },
    Experiment { project_id: String, experiment_id: String },
}
```

- capability 升级为 `attention.v2`；旧 peer 仍通过 v1 返回既有 source。
- source 由当前 Agent runtime 实时派生，不新增 Attention 持久表。
- stable key 为 `agent:<agentSessionId>:<phase>:<version>`。
- phase 离开 `needsInput/failed` 后条目自动消失。
- Attention 页面依旧只有导航动作。
- A2只定义`ExperimentNeedsDecision`/`Experiment`的投影合同与前端兼容解码；A4 reducer落地后由A4注册实际experiment source/event，A2不得反向依赖尚不存在的experiment repo。

## 7. 低噪音系统通知

默认策略：

| kind | 默认 | 条件 |
|---|---|---|
| Agent needs input | 开 | phase 首次进入 needsInput |
| Agent failed | 开 | phase 首次进入 failed |
| Experiment needs decision | 开 | 组级 reducer 无法产生唯一 winner |
| Orchestrator blocked | 开 | 新 state revision |
| Remote outbox failed | 开 | 新 durable attempt/revision |
| Completed/Done | 关 | 用户显式开启 |

- OS 权限只通过用户明确点击申请，拒绝后不循环提示。
- dedupe key 为 `{sourceKind,opaqueSourceId,stateVersion}`。
- 首次 snapshot 只建立 baseline，不补发历史通知。
- App 前台且对应 authority 可见时只更新状态/Attention，不发 OS 通知。
- title/body 使用固定通用文案，不含项目名、任务标题、Agent 内容或路径。
- 不注册 notification action、`onAction` 或 deep-link payload；用户从应用内 Attention/badge 导航。

## 8. 失败、兼容与回滚

- snapshot 失败保留最后 display-only cache，并标记 cached/offline；不制造新的 failure Attention。
- unknown provider 用 provider ID 原样安全显示，不能阻断 terminal。
- v1 peer 不接收 Agent/experiment source；Desktop 显示 unsupported，不把缺失解释为空列表。
- 关闭 OS 通知不影响 Attention 与 runtime。
- rollback 删除 UI 投影/provider 不删除 runtime 数据，也不改变 terminal output/status。

## 9. 测试与验收

1. snapshot/listen handshake、buffer drain、Gap、owner change 和首次 baseline 有 hook 测试。
2. Desktop terminal tab 的七种 phase、cached/offline/unsupported 与 reduced-motion 有组件测试。
3. Mobile terminalStatus 与 agentRuntime 实时更新有回归测试。
4. Attention v1/v2 mixed-version、稳定排序、条目出现/消失与导航目标有 Rust/TS 测试。
5. 通知默认、permission、dedupe、前台抑制、隐私文案和无 action callback 有测试。
6. E2E 覆盖 Agent needsInput event → Attention → terminal authority，过程中无 inline action。

## 10. Spec 自审

- 正常 working/completed 路径不增加用户操作。
- 只有 needsInput、failed 和 experiment ambiguity 进入 Attention。
- 通知和 Attention 都是 projection，不成为第二份 runtime 真值。
