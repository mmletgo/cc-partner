# Agent Session Runtime 设计

- 日期：2026-07-15
- 状态：已批准
- 父级：`2026-07-15-lan-agent-program-design.md`
- 对应计划：`docs/superpowers/plans/2026-07-15-agent-session-runtime.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

Workbench 当前持久化 terminal window/session，并有 output/status event、replay 与远端 NDJSON bridge；Orchestrator 另外在 task 上保存 Claude session/transcript/runtime 字段。普通 terminal 中手工启动 Claude/Codex 不可见，Orchestrator 的 Claude 状态又不是 terminal 的共享权威运行态。

缺失的统一能力包括：

- Agent session 的稳定 ID、provider、native session ID 与 lifecycle；
- terminal/project/worktree/task 的统一关联；
- Hook/OSC ingestion、单调版本与迟到事件保护；
- owner restart、event Gap、terminal 丢失后的 reconciliation；
- desktop、remote、mobile 共用的 bounded snapshot；
- Orchestrator 从 task 私有 Claude runtime 迁移到共享 Agent runtime。

## 2. 目标

1. 建立 provider-neutral `AgentSessionRuntime`，同时覆盖普通 Workbench 和 Orchestrator Agent。
2. Agent 状态由 owning device 自动感知、持久最小 metadata 并通过现有 event bus 投影。
3. 同一 terminal 可以先后运行多个 Agent，但任一时刻最多一个 active Agent session。
4. 迟到 Hook、旧 terminal 输出或 owner 重启不能覆盖更新的 Agent session。
5. Gap 后通过 snapshot 完整恢复当前状态，不依赖完整 terminal replay。
6. Orchestrator task/attempt 引用统一 Agent session，保留一个版本的 Claude legacy dual-write。

## 3. 非目标

- 不在本 spec 实现 Claude/Codex 启动命令、provider probe 或实验组。
- 不持久化 Prompt、assistant 回复、terminal bytes、transcript path 或 env 值。
- 不把 Agent Hook ingestion 暴露成无边界 LAN 写接口。
- 不做 Agent 历史统计、Fleet UI 或系统通知；分别属于 A9、A6、A2。
- 不从窗口标题、任意 stdout 文本或进程名猜测 generic terminal 已完成。

## 4. 领域模型

```rust
pub enum AgentSessionPhase {
    Launching,
    Working,
    NeedsInput,
    Idle,
    Completed,
    Failed,
    Disconnected,
}

pub struct AgentSessionRuntime {
    pub id: String,
    pub project_id: String,
    pub worktree_id: Option<String>,
    pub terminal_session_id: String,
    pub orchestrator_task_id: Option<String>,
    pub orchestrator_attempt: Option<u32>,
    pub provider_id: String,
    pub native_session_id: Option<String>,
    pub phase: AgentSessionPhase,
    pub version: u64,
    pub started_at: String,
    pub last_activity_at: String,
    pub ended_at: Option<String>,
    pub outcome_code: Option<String>,
}
```

约束：

- `id` 由 owner 生成 UUID；remote 只包装为 `remote:<deviceId>:<innerId>`。
- `(terminal_session_id, active=true)` 唯一；启动新 Agent 时必须先终结旧 active row。
- `version` 在 owner 内单调递增；mutation 必须匹配 `agentSessionId + terminalSessionId + expectedVersion`。
- `native_session_id` 只在 adapter 提供可靠结构化值时填写，未知保持 `None`；它是owner-local adapter关联字段，不进入Tauri/control/P2P/Mobile projection DTO。
- `Completed/Failed/Disconnected` 为 terminal 级终态；resume 创建新 runtime row，并用 `resumed_from_agent_session_id` 形成历史关系。

## 5. 存储

新增 `workbench_agent_sessions`：

- 主键 `id`；外键式功能关联 `project_id/worktree_id/terminal_session_id/task_id`；
- provider/native ID、phase/version、时间戳、outcome code；
- `resumed_from_agent_session_id`；
- active terminal 唯一索引和 project/worktree/last_activity 索引。

SQLite 不保存 transcript path、last message、Prompt、response 或 token。Orchestrator 旧字段只用于迁移兼容，不能成为新 UI 的数据源。

## 6. 自动发现与本地 ingestion

### 6.1 稳定上下文

owner 创建/attach terminal 时，把下列非敏感 ID 注入 pane/shell 环境：

- `CC_PARTNER_PROJECT_ID`
- `CC_PARTNER_WORKTREE_ID`
- `CC_PARTNER_TERMINAL_SESSION_ID`
- `CC_PARTNER_OWNER_INSTANCE_ID`

tmux 与 raw PTY 必须得到一致的上下文；不得注入 control token、device token 或 provider credential。

普通 Workbench 用户手开 Claude/Codex/OpenCode 时，owner 在 auto-title 成功绑定（含同名/手改 settled）后经 `ensure_interactive_active` 保证一条 **Idle** active 行，供 snapshot/hint 投影。不得覆盖 Orchestrator terminal；无 title/Hook 不得伪造 `Working`。

### 6.2 OSC 合同

Agent adapter hook 将结构化事件编码为 app-private OSC：

```text
OSC 777 ; cc-partner-agent-v1 ; <base64url(JSON)> ST
```

payload：

```json
{
  "agentSessionId": "uuid",
  "terminalSessionId": "uuid",
  "providerId": "claudeCodeVisible",
  "nativeSessionId": "optional",
  "phase": "working",
  "version": 3,
  "occurredAt": "RFC3339"
}
```

terminal backend 在把 bytes 写入 replay/UI 前识别并剥离该帧，交给 owner runtime reducer。无效 base64、超限、未知 phase、错误 terminal ID 只产生有界诊断，不进入 terminal UI。

单帧上限 16 KiB；同一 terminal 每秒最多接受 20 个 Agent event，超出合并为最后状态并计数。

### 6.3 Reconciliation

- owner 启动时读取 active Agent rows，与持久 terminal/tmux runtime 对账。
- terminal 不存在或确认 exited：active Agent 转 `Disconnected`。
- Hook 恢复并携带更高 version：更新现有 row。
- event version 小于等于当前 version：幂等丢弃。
- terminal 上已经存在另一个 active Agent：旧 Agent 先终结，新 Agent 再成为 active。

## 7. Snapshot 与事件

新增 owner event：

```rust
pub struct AgentRuntimeChangedEvent {
    pub agent_session: AgentSessionRuntimeDto,
}
```

新增 bounded snapshot：

```rust
pub struct AgentRuntimeSnapshot {
    pub owner_instance_id: String,
    pub as_of_sequence: u64,
    pub project_id: Option<String>,
    pub sessions: Vec<AgentSessionRuntimeDto>,
    pub truncated: bool,
}
```

- 默认只返回 active sessions；明确请求 history 时由 A9 提供，不复用该 route。
- 单 snapshot 最多 1,000 active sessions；按 `lastActivityAt DESC,id` 稳定排序。
- capability 为 `workbench.agent-runtime.v1`。
- desktop Tauri、local control、remote P2P 与 mobile HTTP 使用同一 owner helper。
- `AgentSessionRuntimeDto`只保留稳定cc-partner ID、关联ID、provider、phase/version与时间戳，明确剔除`native_session_id`；UI/CLI不需要provider-native ID。
- 扩展 `/api/workbench/events` 前，旧前端 decoder 必须先改为忽略未知 event；随后新增 `agentRuntime` variant。
- Gap/owner change 后暂停增量应用，取 snapshot baseline，再按 cursor 排空 buffered events。

## 8. Orchestrator 接入

- Runner 创建 terminal 后创建 `AgentSessionRuntime(Launching)`，并把 ID 传给 adapter launch plan。
- attempt 新增 `agent_session_id`；task runtime projection 从 runtime repo 读取，不再扫描 task 私有 last message。
- 现有 Claude transcript scanner仅作为一个版本的 legacy association fallback，并且 transcript path 不进入新 runtime DTO。
- completion reducer 先更新 Agent runtime，再由既有 task state machine 进入 Verifying。
- 一个版本内 dual-write `claude_session_id/runtime_*`，并用 characterization test 保证旧 UI/route 不回退。

## 9. 失败、兼容与回滚

- 未安装/未启用 Hook 时 session 可保持 `Launching/Idle`；不得伪造 `Working`。
- generic terminal 没有结构化 completion 时只允许显式 sentinel 或人工结束。
- 旧 peer 缺少 capability 显示 `unsupported`，不回退为普通 Claude session。
- 降级前需停止或结束非 Claude active Agent；否则旧版本可能错误恢复为 Claude。
- rollback 保留新表与 legacy dual-write；关闭 runtime projection 不影响 terminal output/status。

## 10. 测试与验收

1. storage migration、active 唯一性、resume 关系和 version CAS 有 Rust 测试。
2. OSC 拆帧覆盖分片、合并、无效 base64、16 KiB、频率限制和不泄漏 terminal UI。
3. tmux/raw PTY 均注入稳定非敏感 ID；环境中不含 token。
4. owner restart、terminal exited、迟到 event、Gap snapshot、unknown event decoder 有回归测试。
5. local/remote/mobile ID 映射和 unsupported capability 有 route/API 测试。
6. Orchestrator legacy dual-write、completion→Verifying 和旧普通 terminal 行为不回退。

## 11. Spec 自审

- Runtime 只负责“现在发生什么”，不承担 adapter 启动策略、历史统计或 UI 汇总。
- 普通 Workbench 与 Orchestrator 共用一份 Agent session 真值。
- 任何正文、凭据和 transcript path 均未进入新持久层或 P2P DTO。
