# Agent Adapter Platform 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：`2026-07-15-agent-session-runtime-design.md`
- 对应计划：`docs/superpowers/plans/2026-07-15-agent-adapter-platform.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

当前 Orchestrator workflow 虽有 `runner.provider/max_turns/stall_timeout_ms`，parser 只允许 `claudeCodeVisible`；Runner 又硬编码 `claude\n{prompt}\n`，attempt provider 和 runtime 关联同样硬编码 Claude。`max_turns` 与 `stall_timeout_ms` 尚未真正参与运行时控制。

这阻止 Codex、其他 CLI Agent 与显式 generic terminal 复用已有 worktree、claim、terminal、verification 和 delivery 状态机。

## 2. 目标

1. 引入 owner-local、provider-neutral Agent adapter registry。
2. 首先把现有 Claude 行为等价迁入 adapter，再增加 `codexVisible` 与 `genericTerminal`。
3. 自动 probe 可执行文件与版本，不要求用户输入路径或同步凭据。
4. adapter 只负责 probe、launch、resume、runtime normalization、completion 和 usage；Runner 继续拥有 claim/worktree/terminal/task 状态机。
5. 让 `max_turns` 与 `stall_timeout_ms` 真正生效，并有明确终止/evidence 语义。
6. mixed-version 和 downgrade 不得把非 Claude task 静默改成 Claude。

## 3. 非目标

- 不在首版替换 verifier provider；现有 Claude verifier 保持独立 judge。
- 不自动安装 Agent CLI、不接管 provider 登录、不读取或同步 API key。
- 不把 arbitrary shell command 当作 adapter 配置。
- 不通过窗口标题、进程输出猜测 generic terminal 完成。
- 不改变 owning-device、claim CAS、worktree 或 delivery authority。

## 4. Adapter 合同

```rust
pub struct AgentProviderId(pub String);

pub struct AgentProbeResult {
    pub provider_id: AgentProviderId,
    pub availability: AgentAvailability,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub reason_code: Option<String>,
}

pub struct AgentLaunchRequest {
    pub agent_session_id: String,
    pub terminal_session_id: String,
    pub cwd: String,
    pub prompt: String,
    pub native_session_id: Option<String>,
    pub max_turns: u32,
    pub stall_timeout_ms: u64,
}

pub struct AgentLaunchPlan {
    pub executable: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub env: Vec<(String, String)>,
    pub completion: CompletionContract,
}

pub trait AgentAdapter: Send + Sync {
    fn provider_id(&self) -> AgentProviderId;
    fn probe(&self) -> Result<AgentProbeResult, AppError>;
    fn build_launch_plan(&self, request: &AgentLaunchRequest) -> Result<AgentLaunchPlan, AppError>;
    fn normalize_runtime_event(&self, event: NativeAgentEvent) -> Result<AgentRuntimeMutation, AppError>;
    fn extract_usage(&self, event: &NativeAgentEvent) -> Option<AgentUsageDelta>;
}
```

Registry 只接受内置 provider ID：

- `claudeCodeVisible`
- `codexVisible`
- `genericTerminal`

workflow 中未知 ID fail-closed，返回明确诊断。CLI path 由 owner probe 产生，不经 P2P 传输。

## 5. Provider 行为

### 5.1 Claude

- 首次迁移保持 visible terminal 行为与现有 prompt 注入语义。
- 优先消费结构化 Hook/OSC 获取 native session、phase、completion 和 usage。
- 一个版本内保留 JSONL scanner 和 `ORCHESTRATOR_DEV_DONE` sentinel 作为 compatibility fallback。
- characterization tests 必须证明普通 Claude task 的 claim、attempt、verification 与 delivery 不变。

### 5.2 Codex

- owner probe `codex` executable/version；不可用返回 `provider_unavailable`。
- visible terminal launch 使用受控 args/stdin，不拼 shell 字符串。
- native session/resume 仅使用 Codex 提供的结构化 session ID；无法可靠恢复时不伪装 resume。
- completion/usage 通过 adapter Hook/OSC 归一到 A1 runtime。

### 5.3 Generic terminal

- 只用于用户显式选择的受控 executable/args allowlist，不接受 workflow 任意 shell 文本。
- completion 仅允许独立 sentinel 或用户从权威 task detail 明确结束。
- usage/model/native session 默认 unknown。
- 不参与 full-auto experiment，除非配置了确定性 completion 与 verification。

## 6. Workflow 与运行时预算

```yaml
runner:
  provider: codexVisible
  max_turns: 12
  stall_timeout_ms: 300000
```

- `max_turns` 沿用现有 1–20 边界，定义为单 task 允许的 development attempt 总数（首轮加 verifier repair），不把各 provider 无法统一验证的内部 tool turn 当作计数。
- 创建下一 attempt 前检查 `next_attempt <= max_turns`；达到上限后不得创建新的 worktree/session/attempt，task 进入 Blocked，evidence code 为 `runner_max_turns_exceeded`。
- `stall_timeout_ms` 沿用现有 30,000–1,800,000 边界；以 active Agent runtime `last_activity_at` 为准，没有活动时使用 `runtime_started_at`。
- 超时前先执行一次 provider-specific liveness reconciliation；仍无活动才终止并写 `runner_stalled` evidence。
- timeout/turn monitor 属 owning device scheduler，不由 GUI timer 决定。
- 配置存在但无法对 provider 生效时 parser 必须报错，不能接受后忽略。

## 7. Probe、配置与远端

- probe 结果由 owner 缓存 60 秒，并在 executable path/mtime 变化时失效。
- Desktop/remote/mobile 只获取 provider ID、availability、version 和 reason code。
- path、env、credential、登录状态细节不进入 remote DTO。
- remote workflow 保存和 dispatch 都由 owner 验证 provider availability。
- capability 为 `orchestrator.agent-adapters.v1`；旧 owner 返回 unsupported。

## 8. 存储与迁移

- attempt 增加 `provider_id/native_session_id/agent_session_id`；task legacy `runner_provider` 继续 dual-write一个版本。
- `NULL` 与旧 `claudeCodeVisible` 映射 Claude adapter。
- active non-Claude task 降级前必须 drain/cancel；文档明确旧版本 retry 会错误启动 Claude的风险。
- 回滚保留新列；关闭 registry 后只允许新建 Claude task，现存非 Claude task显示 unsupported，不自动转换。

## 9. 测试与验收

1. registry/provider ID/parser/unknown provider 与 probe cache 有单测。
2. Claude characterization 覆盖现有 launch、attempt、sentinel、verification 和 delivery。
3. Codex unavailable/launch/resume/native session/runtime event 有 adapter contract tests。
4. generic terminal 拒绝 arbitrary shell、unknown completion 和 full-auto 不安全配置。
5. max turns/stall timeout/liveness reconciliation/evidence code 使用虚拟时钟测试。
6. remote provider probe、capability unsupported、无 path/env/secret DTO 有 route 测试。
7. mixed-version dual-write与 downgrade guard 有 migration/integration 测试。

## 10. Spec 自审

- adapter 没有复制 Runner、scheduler、worktree、verification 或 delivery 状态机。
- 首版不会因为扩展 provider 而替换 verifier judge。
- 所有可配字段都有真实运行时消费者，不留下“可配置但不生效”的合同。
