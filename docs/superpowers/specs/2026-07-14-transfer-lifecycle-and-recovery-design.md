# Transfer Lifecycle and Recovery 设计

- 日期：2026-07-14
- 状态：已确认
- 依赖：复用现有 send/cancel、chunk resume、durable finalize、transfer repository 与 event schema

## 1. 问题

底层传输协议已支持断点语义，页面也能发送和取消，但用户仍缺少失败阶段、重试/续传、结果对账、打开文件和在文件夹中显示等闭环。局域网超时后 mutation 可能已经生效，盲目重试会产生重复任务或错误状态。1 GiB 真实断点续传仍未获得 L3 证据。

## 2. 目标

1. 为失败或中断任务提供真实的 retry/resume 命令与幂等合同。
2. 区分 queued/connecting/transferring/finalizing/completed/cancelled/failed，并暴露稳定失败阶段与可重试性。
3. timeout/断线后通过稳定 `clientOperationId` 或 protocol transfer id 查询权威状态；`X-CC-Request-Id` 仅用于链路追踪，不作为幂等键。
4. 本机已接收 completed 任务在桌面 GUI 提供 Open 与 Reveal；不存在或权限失败时给可重试错误，mobile/远端调用明确 unsupported。
5. 历史列表只渲染真实可执行动作，不显示假按钮。

## 3. 非目标

- 不重新实现基础 send/cancel、分块算法或 durable finalize。
- 不在本轮实现文件夹批量传输、压缩、互联网中继或内容加密。
- 不自动打开收到的文件，不越过操作系统权限。

## 4. 状态与接口

```rust
pub enum TransferPhase {
    Queued,
    Connecting,
    Transferring,
    Finalizing,
    Completed,
    Cancelled,
    Failed,
}

pub struct TransferFailure {
    pub stage: TransferFailureStage,
    pub code: String,
    pub retryable: bool,
    pub message: String,
}
```

保留现有 coarse `TransferStatus` 以兼容旧 GUI/sidecar，共享 DTO 只新增 nullable `phase`，旧行/未知 phase 由 status 推导而不是把未知值映射 Failed。新增/扩展命令：

- `retry_transfer(task_id, client_operation_id)`：失败且可重试时创建同一 logical transfer 的新 attempt。
- `resume_transfer(task_id, client_operation_id)`：存在双方 resume metadata 时从已确认 offset 继续，并复用稳定 `protocolTransferId` 以命中 receiver checkpoint。
- `get_transfer_operation(client_operation_id)`：返回 `notFound/pending/succeeded/failed`，供 uncertain outcome 对账。
- `prepare_transfer_open(task_id, action)`：sidecar 仅验证 `direction=Receive + completed + path exists`，经 loopback control 返回 same-device GUI 的 local target；GUI 再调用 Tauri opener。P2P/mobile 不暴露路径也不激活 owner OS。

`clientOperationId` 是发送端 sidecar 全局唯一持久幂等键，数据库使用 `UNIQUE(clientOperationId)`，并保存 canonical operation payload fingerprint（operation kind、logical task/source identity、peer 与预期 protocol transfer id）；`get_transfer_operation` 只按该键查询。same id + same payload 回放已记录 outcome，same id + different payload 返回 typed `operationIdConflict`，绝不能把另一个 task 或 retry/resume 当成原操作。`requestId` 是每次 HTTP/invoke 的追踪字段。每个 attempt 另有 `attemptId`，resume 复用 `protocolTransferId`，full retry 可生成新的 protocol id。

## 5. Retry 与 Resume 规则

- `pending/transferring/finalizing` 不显示 retry；只允许 cancel。
- `failed + retryable + resume metadata` 显示“继续传输”。
- `failed + retryable` 无 resume metadata 显示“重新传输”。
- `cancelled` 默认不自动恢复；若源文件仍匹配 fingerprint，可由用户显式重新传输。
- source fingerprint 固定为 `{size,mtimeNsOrNull,sha256}`：SHA 在 blocking worker 对打开的文件句柄计算，spawn 前重取 size/mtime；mtime 不可用时重新校验 size+SHA。传输后 receiver SHA 必须等于预期；任一不一致均拒绝旧 resume/标记 source changed，不在 TOCTOU 情况继续 finalize。
- queued attempt 必须先以唯一 clientOperationId + payload fingerprint 在发送端事务 claim，唯一 winner 才可 spawn；metadata/整文件 SHA 在 `spawn_blocking` 中完成，使 Queued/Connecting 可观察且不阻塞 async runtime。
- receiver 只以 `protocolTransferId` 持有现有 durable promotion/finalize journal，先持久化 finalizing intent，再幂等 rename/hash 并提交 receiver completed；它不持有发送端 `clientOperationId`。发送端最终 ACK 丢失时先按 protocol id 查询 receiver complete/status，在确认 receiver succeeded 后于本地单事务提交 sender task completed + operation outcome。任一侧 crash/recovery 都只能返回 succeeded、pending 或 typed failure，不重复破坏性 finalize。

## 6. UI

复用 `TransferItem` 的可选 callback 模型：

- 进度行展示 phase、速率、已传/总量和失败原因。
- failure card 提供 Retry/Resume；completed 提供 Open/Reveal；不存在 callback 的动作不渲染。
- uncertain 状态显示“正在确认结果”，对账完成前不提供再次发送。
- 历史按 active/needs-attention/recent-completed 分区，空组省略。
- 错误使用用户可读 message，诊断详情可复制稳定 code/request id，不暴露绝对敏感路径给远端。

## 7. 失败与兼容

- 旧 peer 无 resume capability 时回退“重新传输”，不伪装续传。
- 对账 route 不存在时保留当前任务并显示 unsupported，不自动重放。
- Open/Reveal 仅 same-device desktop GUI 对 `direction=Receive` 的 completed 任务执行；sidecar 只通过 lifecycle control 验证并返回 local target，P2P/mobile 返回 unsupported，失败不在当前设备猜测路径。
- repository 幂等 schema upgrade 保留 logical transfer id、attempt 与 last failure；旧行默认 attempt=1，`migrations/0001_init.sql` 仅同步 schema 文档。

## 8. 测试与验收

1. 同一 clientOperationId 并发 retry/resume 只产生一个 attempt；insert 后 spawn 前 crash 可恢复，trace request id 变化不影响对账。
2. finalizing 各 crash point 与 ACK 丢失后 reconciliation 返回 completed/pending 真值，不重复 finalize。
3. 源 fingerprint 变化拒绝 resume；旧 peer 正确回退 retry。
4. 每个 phase 只渲染合法动作，TransferItem callback 缺失时动作不存在。
5. Open/Reveal 成功、文件缺失、权限失败、GUI control owner offline 与 mobile/P2P unsupported 均有 typed 结果。
6. 1 GiB 中途断网、进程重启、恢复完成与 SHA-256 一致性进入 N8 L3。

## 9. 持久文档

实现时更新 `docs/prd.md` Transfer 行为、`docs/p2p-protocol.md`、`src-tauri/CLAUDE.md`、`web/CLAUDE.md` 和质量矩阵。

## 10. Spec 自审

- send/cancel 与 durable finalize 不重做；只补恢复和结果闭环。
- retry、resume、uncertain、open/reveal 的动作条件没有歧义。
- 真实大文件续传只在 N8 取得真机证据后宣称通过。
