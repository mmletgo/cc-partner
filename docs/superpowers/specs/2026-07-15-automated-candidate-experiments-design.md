# Automated Candidate Experiments 设计

- 日期：2026-07-15
- 状态：已批准
- 依赖：Agent Session Runtime、Agent State Projection合同、Agent Adapter Platform、Browser Verification Surface
- 对应计划：`docs/superpowers/plans/2026-07-15-automated-candidate-experiments.md`
- 共享边界：继承 A0 owning-device 与固定 LAN 合同；business API 无调用者身份鉴权，capability 只表示协议能力。

## 1. 问题

同一任务可能有多种实现策略或 Agent/provider 组合。让用户手工创建多个 worktree、重复输入 Prompt、等待、检查 diff 并选择结果，会把系统可以自动完成的协调工作转移给用户。

现有 Orchestrator 已有 task/worktree/attempt/evidence、设备级并发 claim、自动 verifier 与 delivery，但没有实验组、candidate 级交付隔离、比较 reducer、组级幂等或公平调度。

## 2. 目标

1. 用户只需创建一次 experiment，系统自动创建并运行多个 candidate。
2. candidate 复用普通 task/worktree/attempt/evidence，不复制第二套任务状态机。
3. 所有 candidate 在同一 project owning device 上执行，不自动复制仓库或跨设备迁移。
4. candidate 通过确定性硬门禁后，由 comparative verifier 自动选出唯一 winner。
5. full-auto 且置信为 `high` 时自动交付 winner；loser 永不 commit、push、merge。
6. 只有无合格 winner、并列或置信不足时，产生一个组级 Attention。
7. remote/offline create 具备组级原子幂等，不用多条普通 task outbox 拼接。

## 3. 非目标

- 不提供候选 Diff 对比、批注或 Request Rework 审查器。
- 不自动选择“最空闲设备”，不跨 owner 竞跑。
- 不把最快完成时间作为主要质量标准。
- 不让每个 candidate 各自产生 Human Review/Attention。
- 不让 loser 进入现有 delivery。
- 不在首版支持 provider credential、仓库或 browser profile 的跨设备复制。

## 4. 数据模型

```rust
pub enum ExperimentStatus {
    Draft,
    Queued,
    Running,
    Comparing,
    WinnerReady,
    Delivering,
    Completed,
    NeedsDecision,
    Failed,
    Cancelled,
}

pub enum CandidateOutcome {
    Pending,
    Running,
    CandidateReady,
    Rejected,
    Winner,
    Loser,
    Failed,
    Cancelled,
}

pub enum ComparativeConfidence {
    High,
    Medium,
    Low,
}

pub struct OrchestratorExperiment {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub goal: String,
    pub acceptance: String,
    pub status: ExperimentStatus,
    pub selection_policy: String,
    pub max_parallel: u32,
    pub winner_task_id: Option<String>,
    pub selection_reason: Option<String>,
    pub confidence: Option<ComparativeConfidence>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
}
```

新增表：

- `orchestrator_experiments`
- `orchestrator_experiment_candidates(experiment_id,task_id,ordinal,provider_id,strategy_label,outcome,selection_metadata_json,created_at,updated_at)`
- `orchestrator_experiment_evidence`：只存聚合判定、风险和状态转换，不复制 child evidence。
- `orchestrator_experiment_outbox` 与 mirror 表：保持组级 create 的原子性。

candidate task 增加 `experiment_id` 与 `delivery_suppressed=true`，但继续使用既有普通 task repo/state machine。

## 5. 创建合同

```json
{
  "clientRequestId": "stable-uuid",
  "projectId": "project-id",
  "title": "实现登录态恢复",
  "goal": "...",
  "acceptance": "...",
  "maxParallel": 2,
  "candidates": [
    {"providerId":"claudeCodeVisible","strategyLabel":"minimal"},
    {"providerId":"codexVisible","strategyLabel":"refactor"}
  ]
}
```

- candidate 数量 2–8；`maxParallel` 1–min(candidate count, device max concurrency)。
- owner 在一个事务中创建 experiment、candidate tasks、初始 evidence 和 idempotency fingerprint。
- 同一 `clientRequestId`+相同 fingerprint 返回同一 experiment；不同 fingerprint 返回 conflict。
- remote shortcut 将整组请求转发 owner；离线时写一条 experiment outbox，而不是 N 条 task outbox。
- capability 为 `orchestrator.experiments.v1`。

## 6. 调度与公平

- candidate 继续消耗设备全局 slot，不创建第二套资源计数。
- claim CAS 同时检查 device global cap 与 experiment `max_parallel`。
- scheduler 同一轮对同一 experiment 最多 claim 一个 candidate，再轮转其他普通 task/experiment，避免单组吃满槽位。
- Preparing lease、claim token、attempt generation 与现有 task 完全一致。
- repair attempt 受 adapter `max_turns` 与现有 retry budget 共同约束。
- cancellation 可以取消单 candidate 或整组；取消整组时 running Agent 先终止，再落 candidate/experiment 终态。

## 7. 验证、比较与唯一交付

### 7.1 硬门禁

candidate 只有同时满足以下条件才进入 `CandidateReady`：

- development attempt 正常完成；
- validation infrastructure 正常而非 unknown；
- deterministic validation 全部通过；
- verifier verdict 为 passed；
- 未取消、未 Blocked、未超过 repair/turn/stall budget；
- Browser Verification 中声明为 required 的 assertion 通过。

candidate 到达此状态时禁止调用普通 per-task delivery。

### 7.2 Comparative verifier

输入只包含 goal、acceptance、每个 candidate 的结构化 validation/evidence 摘要、risk notes、bounded machine diff digest/stat 和 Browser evidence 摘要；不把完整 patch交给用户。

输出：

```rust
pub struct ComparativeVerdict {
    pub winner_task_id: Option<String>,
    pub confidence: ComparativeConfidence,
    pub reason: String,
    pub risk_notes: Vec<String>,
    pub tied_task_ids: Vec<String>,
}
```

规则：

- 只有一个 CandidateReady：它是唯一 winner，confidence 为 `high`。
- 多个 CandidateReady：comparative verifier 必须给出唯一 winner 且 confidence=`high` 才可自动交付。
- `medium/low`、并列、judge error 或无 CandidateReady：experiment 进入 `NeedsDecision`，只产生一个组级 Attention。
- full-auto 关闭时进入 `WinnerReady`，用户只确认系统推荐结果，不阅读 diff；确认仍在 experiment authority 完成。
- 任何时刻只允许 `winner_task_id` 进入现有 delivery helper，事务内再次验证 experiment version 与 winner identity。

## 8. UI 与用户负担

- 创建器提供 goal/acceptance 与 2–8 个 provider/strategy 选择；默认根据 owner probe 预填两个可用 adapter。
- board 只显示组级进度、candidate 状态和自动推荐理由，不默认展示源码差异。
- 正常 full-auto 完成只产生一个 experiment result，不产生 N 个 review 项。
- NeedsDecision 页面只提供：接受推荐、选择另一个已通过 candidate、取消；不提供 diff 批注。
- Attention 行只导航到 experiment detail。

## 9. 远端、失败与回滚

- owner offline 时 experiment outbox 使用稳定 request ID、sending lease、retry classification 与 mirror transaction。
- 协议失败标记 failed，不盲重放；网络失败回 pending。
- 旧 peer 无 capability 返回 unsupported，不降级为多条 task。
- comparative judge 失败不影响 child evidence，组进入 NeedsDecision/Failed。
- 降级前必须停止新实验、完成或取消 active group，并把所有 loser child 置 Cancelled/Aborted；否则旧版本可能把 candidate 当普通 task交付。
- rollback 保留 additive tables；关闭 experiments 后普通 task路径不变。

## 10. 测试与验收

1. 创建事务、2–8 边界、fingerprint conflict 与 remote outbox 原子性有 repo/route 测试。
2. global cap、group cap、同轮公平、claim CAS 与 cancellation 有 scheduler 测试。
3. candidate verification→CandidateReady 且 delivery suppressed 有状态机测试。
4. 唯一 candidate、high-confidence winner、tie、medium/low、judge error、零合格 candidate 有 reducer 测试。
5. loser 永不进入 delivery；winner 交付前 version/identity 二次校验。
6. 一个 experiment 只产生一个 NeedsDecision Attention。
7. mixed-version unsupported、outbox lost ACK reconcile 与 downgrade quiesce 有集成测试。
8. E2E 覆盖创建→并行运行→自动 winner→唯一交付，以及歧义→单一决策入口。

## 11. Spec 自审

- candidate 复用普通 task 状态机，没有第二套 worktree/attempt/evidence。
- 正常路径不需要人工 Diff 比较。
- 自动交付只发生在确定性门禁通过、唯一 winner 且 confidence=high 时。
