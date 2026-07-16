# Orchestrator Review, Workflow and Notifications 设计

> **执行状态（2026-07-15）：已被部分取代。** Review Diff、review digest、Changes UI、mobile diff、Diff→Rework E2E 与 Deliver 人工确认门已取消；通知合同迁移到 Agent State Projection；WORKFLOW.md 向导保留为历史设计但不在当前 LAN Agent Program 实施。

- 日期：2026-07-14
- 状态：已确认（N6 作为串行执行单元已被 `2026-07-15-lan-agent-program` 部分取代；见上文执行状态）
- 依赖：现有 Orchestrator workflow/action/evidence、Attention 导航投影、系统通知权限与 Workbench file/Git 能力

## 1. 问题

任务进入 Human Review 后，用户需要离开详情抽屉自行检查改动；当前产品明确没有 Git diff 面板。`WORKFLOW.md` 无项目级创建/编辑/校验入口，解析失败只阻断调度。Human Review、Blocked、failed outbox 和完成状态没有统一系统通知，用户必须持续盯住应用。

## 2. 目标

1. Human Review 详情中展示有界、只读的文件列表、diff stat 与文本 diff，并与 Evidence、Deliver、Request Rework 同屏。
2. Request Rework 明确记录用户意见并进入既有 evidence/attempt 流程。
3. 提供项目级 WORKFLOW.md 创建、打开、编辑、校验与错误定位向导。
4. 为 Human Review、Blocked、failed outbox 和 Done 提供可配置、去重的系统通知，并让对应 Attention/Workbench authority 提供 deep link。
5. 当前桌面 notification plugin 不承诺点击回调：系统通知只提醒，Attention/deep link 只导航到权威界面，均不直接执行 Deliver、Retry、Discard 或安装。

## 3. 非目标

- 不实现 PR 创建、交互式 conflict resolution、diff 内编辑或完整代码审查平台。
- 不允许 WORKFLOW.md 启用自动交付；delivery 继续只由 Settings 控制。
- 不持久化系统通知正文之外的 Prompt、diff 内容或终端文本。
- 不改变 Attention source 与“只导航”语义。

## 4. Review diff

```rust
pub struct OrchestratorReviewDiff {
    pub task_id: String,
    pub base_ref: String,
    pub head_ref: String,
    pub files: Vec<ReviewDiffFile>,
    pub total_files: u32,
    pub truncated: bool,
    pub review_digest: String,
}

pub struct ReviewDiffFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub patch: Option<String>,
    pub binary: bool,
    pub truncated: bool,
}
```

- 后端仅对 Human Review/Rework 关联的任务 worktree 生成 diff。
- 上限：200 files、总 patch 2 MiB、单文件 patch 256 KiB；二进制只返回 metadata。
- base/head 从任务 attempt/worktree 权威元数据派生，不接受浏览器任意仓库路径或 ref。
- capability 固定为 `orchestrator.review-diff.v1`；snapshot 明确覆盖 base revision、HEAD revision、staged/unstaged/untracked dirty worktree 与 unborn repository。
- `reviewDigest` 不哈希截断后的展示 patch，而是 SHA-256(base/tree identity + 按 repo-relative path 排序的 status/mode/old blob oid/new content hash)；new/dirty/untracked 内容用 streaming hash，展示截断不影响 digest。
- desktop local、remote P2P 与 mobile 均复用同一 command helper；remote 由 owning device 生成。

## 5. Human Review 交互

任务抽屉分为 Summary、Changes、Evidence 三个 tab；复用 roving tab 合同：`tablist/tab/tabpanel`、唯一 tab stop、Arrow/Home/End、面板关联与选择后局部滚动。默认打开 Summary，存在 diff 时显示文件/增删摘要。Deliver 与 Request Rework 固定在抽屉 footer：

- Deliver 仍受 full-auto delivery 配置与现有动作合同控制。
- controller 以 `{projectId,taskId,attemptId,requestSeq,digest}` 保存审阅；选择/attempt/reload 变化立即 abort/清空，旧响应不得回填。Deliver 前重新采集 snapshot，digest 漂移则返回 Conflict，要求重新审阅，不能交付未看过的变更。
- Request Rework 打开 Dialog，意见必填 1–2000 字，提交后写 evidence 并进入 Rework。
- 对声明支持 review diff 的 owner，diff 加载失败时不隐藏 Evidence、Rework 或 Deliver：Rework 保持可用，Deliver 保持可见但禁用，并显示局部错误与重试；只有拿到并确认 digest 后才允许 Deliver。旧 peer 明确不支持 capability 时才沿用原 Deliver 合同。
- mobile 本轨道为 inspection-only：用 grouped list + `aria-expanded/aria-controls` 文件按钮展开，只把当前选择文件 patch 挂入 DOM，并显示“请在桌面端完成审核”；不宣称 mobile 可 Deliver/Rework。

## 6. WORKFLOW.md 向导

### 6.1 步骤

1. 检测：missing/valid/invalid/readError。
2. 缺失时可从内置默认模板创建；已有文件通过 `view=files&path=WORKFLOW.md` typed deep link，由现有 automation controller 分阶段选择上下文并调用 file controller 的 `openFileByPath`，Workbench 只注入 bridge，不新增第八个 controller。
3. 编辑时实时执行前端基础 YAML 语法提示，但保存前必须调用后端权威 validator。
4. validator 返回 path、line、column、code、message 和规范化 preview。
5. 保存使用 expected hash，文件已变化时拒绝覆盖并要求重新加载。
6. 保存成功后刷新 runtime snapshot/workflow status；不自动 dispatch。

capability 固定为 `orchestrator.workflow-document.v1`。向导只允许现有固定 workflow states、prompt template 与 validation commands；不能改变 delivery。

## 7. 系统通知

```ts
type OperationalNotificationKind =
  | 'humanReview'
  | 'blocked'
  | 'remoteOutboxFailed'
  | 'taskDone'
```

- 用户可按 kind 开关，默认 Human Review/Blocked/outbox failed 开，Done 关；字段属于现有 Orchestrator config，由 Settings controller 受控表单经 N1 owner 保存，view 不直接调 API。
- sidecar Orchestrator/outbox 状态转换产生全局 `OperationalNotificationEvent { kind, opaqueSourceId, stateVersion, occurredAt }`；task `stateVersion` 来自持久状态 revision，Done 不依赖 Attention 投影或当前打开项目。owner 提供最多 1,000 条、无正文的 `OperationalNotificationSnapshot { asOfCursor, items, truncated }`，在 event cursor 稳定的 DB 读取窗口生成。GUI 首次/gap 用它建立 baseline，再从 cursor 续订，不补发旧通知；owner restart/gap 后未来新 revision 仍须通知。
- 跨进程链固定为：sidecar 持久 revision/写 N1 `event_bus` → loopback+control-token snapshot/stream control routes → GUI `BackendControlClient` relay → Tauri event；网页 hook 只通过注册的 Tauri snapshot command/API 与 event listener 消费。hook 必须先注册 listener 并按 `(ownerId,sequence)` 缓冲，再请求 snapshot；baseline 建立后丢弃 `<= asOfCursor`，顺序消费更大的 buffered/live events。Gap/owner change 立即暂停消费、保留后续缓冲并重新 baseline，避免 snapshot→listen 窗口丢事件。sidecar 不尝试直接 emit GUI 进程事件，GUI 也不读本地空的 Orchestrator repo。owner id/sequence、断线 replay 与 Gap 处理复用 N1 合同。
- dedupe key 为 `{sourceKind, opaqueSourceId, stateVersion}`；同一状态不重复通知。断线 replay 复用相同 key，owner gap snapshot 只重建 baseline。
- 本轨道即使插件导出 action API，也主动只发送标准通知，不注册 action type/onAction，不承诺点击导航；这些跨平台行为尚未认证。用户从 Attention 或应用内 badge 进入既有 deep link。
- App 在前台且对应 authority 已可见时只更新 Inbox/badge，不重复发系统通知。
- 权限未授权时显示非阻断设置提示；不循环请求权限。

## 8. 失败、兼容与安全

- 旧 remote peer 无 diff/workflow capability 时显示 unsupported，Deliver/Rework 仍可按原合同工作。
- diff path 永远是 repo-relative，拒绝越界路径；错误/日志不包含 patch 正文。
- notification 可见 title/body 只使用通用状态文案（例如“有任务等待审核”）；不传 `extra` deep link/action payload，不包含任务标题、项目名、goal、acceptance、diff、evidence 或终端内容，避免锁屏泄漏。
- WORKFLOW 保存失败保留 draft；hash conflict 不自动合并。

## 9. 测试与验收

1. 200 files/2 MiB/256 KiB 上限、binary、path escape、截断尾部变化仍改变完整 review digest、dirty/untracked/unborn 与 remote owner 行为有 Rust 测试。
2. task drawer 的 Summary/Changes/Evidence、局部错误和 footer focus 合同通过组件/E2E。
3. Rework 意见进入 evidence，失败保留输入且任务状态不变。
4. WORKFLOW missing/create/valid/invalid/hash conflict/remote unsupported 全覆盖。
5. owner notification event 的 Human Review/Blocked/outbox failed/Done、冷启动 baseline、断线 replay 去重、前台抑制、权限缺失与 preference 有单测；桌面通知不注册点击/business action。
6. Attention 数量与 source 不因系统通知实现而重复或漂移。

## 10. 持久文档

实现时更新 `docs/prd.md`、Orchestrator 与 Workbench 分层文档、P2P capability/route inventory、通知权限说明和质量矩阵。

## 11. Spec 自审

- Review diff 有明确安全边界和体量上限，不扩成 IDE。
- WORKFLOW 不改变 delivery 权威；通知只导航不动作。
- desktop/mobile/remote 复用同一后端 helper，不产生第二套任务模型。
