# Frontend Async State and Mobile Transport 设计

- 日期：2026-07-14
- 状态：已确认
- 依赖：复用现有 runtime schema、visibility polling、Workbench context guards 与 mobile HTTP routes

## 1. 问题

Settings、ClaudeMd 等保存路径在请求返回后无条件回填提交时的旧快照，可能覆盖保存期间的新输入；Scratchpad 快速切页可被乱序响应拉回旧页。Devices 保存失败会清草稿，CcHistory 删除失败不回滚。Workbench Git 长操作会把旧 project/worktree 的 busy/error 写入新上下文。Mobile project detail 首次失败后同项目无法真正重试，HTTP transport 也没有统一 timeout、AbortSignal 和 mutation 结果对账。

## 2. 目标

1. 建立可复用的 safe-save 状态合同：`submittedSnapshot + editVersion + requestSeq`。
2. 所有页面/项目/worktree 异步结果只有在 context key 与 sequence 仍有效时才能提交 UI。
3. 失败保留用户输入、提供行级重试或回滚；删除失败恢复原项，成功删除不伪造无后端合同的 Undo。
4. Mobile transport 统一支持连接、首字节、读取与操作超时以及上下文取消。
5. query 可以安全重试；mutation 仅在幂等或完成状态对账后重试。
6. 异步成功/失败反馈具备 live region 和稳定 accessible name。

## 3. 非目标

- 不引入 Redux/Zustand 或通用数据获取框架。
- 不改变后端业务语义，也不把所有局部 state 强行合并成单个页面 controller。
- 不为不具备幂等/对账能力的 mutation 做自动重放。

## 4. Safe-save 合同

```ts
export interface SaveAttempt<T> {
  requestSeq: number
  submittedSnapshot: T
  submittedEditVersion: number
}

export interface SaveResolution<T> {
  baseline: T
  draft: T
  dirty: boolean
  applied: boolean
}
```

- 用户每次编辑递增 `editVersion`。
- submit 捕获 snapshot/version/seq。
- success 总是更新“已保存基线”；只有当前 version 与 submitted version 相同且 draft 仍等于 snapshot 时才回填服务端值。
- 期间有新编辑则保留当前 draft，dirty 继续为 true。
- 旧 seq 的 success/error 均不改当前状态。

首先覆盖 Settings 与 ClaudeMd，再将纯 helper 用于其它表单。helper 放在 `web/src/lib/asyncState/`，不包含业务 API。

## 5. 页面与操作状态

### 5.1 Scratchpad

`openPage` 使用 `{pageId, navigationSeq}`；只有最新 navigationSeq 且 active target 相同才提交。同步后 reload 还需校验 `draftVersion`。切页期间允许继续选择其它页，但 loading/error 属于目标 page，不全局锁死。

### 5.2 Devices 与 CcHistory

- `saveTarget` 返回明确 result；仅 success 清除 draft。
- CcHistory 删除保存条目快照；失败回滚并可重试。成功软删除不在本轨道承诺 Undo，因为当前后端没有 restore/vector-clock 合同；未来增加时必须由独立同步 spec 定义。
- 保存到 Prompt 失败显示可重试反馈，不能静默。

### 5.3 Workbench operation context

```ts
type WorkbenchOperationKey = {
  projectId: string
  worktreeId: string | null
  sequence: number
}
```

commit/push/merge/create/remove 的 success、catch、finally 每个状态写入点都调用 `isCurrentOperation(key)`；busy/error 按 context 存储或在离开 context 时清理，不污染新项目。

## 6. Mobile transport

### 6.1 状态

```ts
type MobileConnectionState =
  | { kind: 'online'; lastSucceededAt: number }
  | { kind: 'reconnecting'; attempt: number; cachedSince: number | null }
  | { kind: 'offline'; lastError: string; since: number }
```

项目详情维护 `idle/loading/ready/error`。同项目早退只允许 `ready`；`error` 点击或显式按钮必须重试。

### 6.2 Timeout 与取消

- 浏览器 Fetch 无法可靠区分 TCP connect 与 first byte，因此 query 只暴露 overall 15 秒预算，覆盖 fetch 与 body decode；事件流不使用 overall timeout，server 每 15 秒发送 typed NDJSON `{"type":"heartbeat","sentAt":"<RFC3339>"}`，parser 在业务事件解码前识别。client 使用独立 lifecycle controller 与每连接 controller；35 秒无任何完整 data/heartbeat frame 时只 abort 当前连接并创建新 controller 重连。
- mutation 默认 30 秒；Git merge/push、依赖安装等长操作可由调用方声明更长预算。
- project/worktree/context 切换时 abort 旧 query。
- transport error 只分类 `timeout/callerAbort/network/protocol/decode`；mutation 发生 timeout/network 时领域结果一律为 `unknown`，由 controller 先查询业务状态，transport 不猜 `notStarted/confirmedFailed`。
- Workbench Git 四类 mutation 使用 sidecar durable operation ledger 与稳定 `clientOperationId`；Tauri/HTTP 都返回成功通道 typed envelope `succeeded | unknown`，其中 unknown 只携带 caller 已知的 operation id/transport class，不能伪造未收到的 reconciliation intent；definitive validation/conflict 才走原错误通道。ledger 在执行前持久化 canonical payload hash 与 reconciliation intent：commit 的 before HEAD + staged expected tree hash，push 的 local/remote ref，merge 的 source/main HEAD，remove 的 exact worktree identity；same id/different payload typed conflict。unknown 后 controller 先按 operation id 查询 owning sidecar ledger 取得 intent/state，再刷新权威 Git/worktree 状态。commit 只有 `newHead.parent=beforeHead && newHead.tree=expectedTree` 才确认，不能用相同 message 代替内容 identity；push/merge/remove 同理验证其精确后置条件。ledger 不可达/notFound/pending/不一致保持“结果未知”并禁用盲重放；notFound 也只允许以同一幂等 ID重送，不生成新动作。新 remote peer 通过 capability 传播 envelope/status；旧 peer 成功可映射 succeeded，timeout/network 只能显示不可对账 unknown 且绝不重放。desktop/mobile 共用 envelope 与纯判定 helper。

### 6.3 重试

- GET/只读 POST：指数退避最多 2 次，页面不可见时不自动重试。
- mutation：只有稳定 operation ID + 后端幂等或明确 status reconciliation 才允许重试。
- 恢复在线后刷新当前 project/worktree/session 和可见 panel；保留离线期间可安全显示的缓存，并标明时间。

## 7. 可访问性

- `TagInput` 必须接收 `aria-label` 或 `aria-labelledby`，placeholder 不能充当名称。
- 新增/复用统一 Status/Toast live region：成功为 `role=status`，阻断失败为 `role=alert`，避免重复播报轮询错误。
- busy 按钮保持可辨识名称；失败后焦点留在触发动作或移动到错误摘要。

## 8. 测试与验收

1. Settings/ClaudeMd：submit A 后输入 B，再 resolve A，B 保留且 dirty=true。
2. Scratchpad：B/C 逆序完成只显示 C；sync reload 不覆盖新 draft。
3. Devices：失败后字段保留；CcHistory 删除失败回滚并可重试，成功删除不显示无后端合同的假 Undo。
4. Workbench：操作中切换 context，旧 success/catch/finally 均不写新 context；commit/push/merge/remove unknown 后按权威状态矩阵进入 confirmedSucceeded 或仍 unknown，后者不盲重放。
5. Mobile：首次详情失败后同项目可重试；切项目取消旧请求。
6. timeout：query overall budget 覆盖 decode，unknown mutation 只对账不盲重放，半开 event stream 在 heartbeat watchdog 后重连。
7. Testing Library/axe 或等价合同验证 accessible name 与 live region。

## 9. 持久文档

实现时更新 `docs/prd.md` 的可靠性合同、`web/CLAUDE.md` 的 async/context 所有权和 mobile transport 测试说明。

## 10. Spec 自审

- safe-save 与 stale context 是纯基础合同，不与业务 controller 混合。
- query 与 mutation 的重试边界明确，不会重复执行高风险动作。
- 所有失败路径保留草稿、回滚或显式标注结果未知。
