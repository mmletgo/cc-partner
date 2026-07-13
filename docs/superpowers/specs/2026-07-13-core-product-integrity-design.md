# cc-partner 核心产品完整性设计

- 日期：2026-07-13
- 状态：方案已确认，可进入 implementation plan
- 范围：文件传输、速记本、Prompt、Claude History、Settings、权限与页面轮询

## 1. 背景与目标

当前后端已经具备真实分块传输、取消、速记本持久化、Prompt CRUD 和各配置读取能力，但部分前端旅程仍存在“看起来成功、实际未执行”或“失败后没有可靠状态”的缺口。本方案把这些缺口作为同一类产品完整性问题治理：用户看到的操作状态必须与权威后端一致；页面切换、并发请求、后台标签页和部分接口失败不得制造数据丢失、旧数据覆盖或整页不可用。

目标：

1. 文件传输从原生路径选择到发送、进度、取消形成真实闭环。
2. 速记本最后一次输入在切页、路由卸载和 GUI 关闭前进入可靠保存队列。
3. Prompt 新建、更新、删除失败时回滚，不保留伪成功状态，并允许原地重试。
4. Claude History、Settings、权限与轮询页面具备明确的并发、失败和可见性语义。
5. 为上述关键用户旅程建立 Vitest 与 Playwright/Tauri fake 回归面。

## 2. 已确认方案与取舍

采用“共享异步基础设施 + 领域小状态机”，不在每个页面复制 request ref、visibility listener 和错误处理。

- 轮询统一使用 `useVisibilityPolling`：首次立即执行；页面不可见时停止；恢复可见立即刷新；同一任务永远只有一个 in-flight 请求；卸载后不回写。
- 速记本使用 AppShell 生命周期内常驻的 autosave queue，不依赖 React effect cleanup 能等待 Promise。路由卸载触发 flush，关闭 GUI 前显式 await 全部 pending write；`pagehide` 仅作 best-effort 补充。
- Prompt 继续允许乐观显示，但每个 mutation 保存快照；失败必须回滚并恢复草稿/删除位置。页面同一实体 mutation 期间禁用冲突动作。
- Settings 初始化改为独立资源 `allSettled`。只有主 `AppConfig` 失败阻止通用设置编辑；其余 tab 局部失败、局部重试，不再让 11 个请求任一失败拖垮整页。
- 权限的 required 定义保持现状：屏幕录制、辅助功能、输入监控决定 onboarding 完成；通知权限仍展示但为 optional。请求改为逐项触发，避免一次打开多个系统授权面板。

未采用的方案：

- 不使用浏览器 `<input type="file">` 作为桌面路径来源：WebView 的 `File` 只保证名称，不保证三平台可供 Rust 打开的绝对路径。
- 不在卸载 cleanup 中直接 `await invoke`：React 不等待 cleanup Promise，无法构成关闭保证。
- 不把 Prompt 失败记录留作仅存在内存的“离线记录”：当前产品没有本地 outbox 语义，伪记录会在重启后消失。

## 3. 文件传输闭环

### 3.1 路径来源

“浏览”使用 `@tauri-apps/plugin-dialog` 的 `open({ multiple: false, directory: false })`，仅接受返回的字符串路径。拖放使用 `getCurrentWebview().onDragDropEvent` 的 `drop.paths`，取首个路径。普通浏览器测试环境不注册 Tauri drag listener；隐藏 `<input>` 不再作为生产发送路径，仅可删除。

路径在前端只展示 basename，完整路径仅保存在内存并传给 `send_transfer`。macOS、Windows、Ubuntu 路径均按不透明字符串处理，不自行拼接、URI decode 或替换分隔符。用户取消对话框不报错且保留原选择；拖入多个文件时本轮只选择第一个并显示本地化提示；目录由后端现有文件校验拒绝并显示错误。

### 3.2 API 契约

```ts
export interface SendTransferResult {
  accepted: true;
  deviceId: string;
  filePath: string;
  id: string;
}

transferApi.send(deviceId: string, filePath: string): Promise<SendTransferResult>
transferApi.cancel(taskId: string): Promise<{ ok: true; id: string }>
```

发送时按钮进入 `sending`，禁止重复提交；成功后清空选择、立即刷新任务列表并用 `id` 定位新任务；失败保留路径与目标设备，显示可读错误并允许重试。取消是逐任务 busy 状态，成功立即刷新，失败保留任务并显示行级错误。

后端当前没有 pause、resume、retry、open-file 命令，因此这些按钮不得渲染。`TransferItem` 只在回调存在时渲染相应动作。后续实现策略是先定义领域命令与状态转换，再恢复 UI：pause/resume 需要 sender/receiver checkpoint 协议；retry 必须从历史任务构造新 transfer id 并验证源文件仍存在；open 仅允许 completed receive task 且通过 opener 打开已保存文件。三者不以空回调占位。

### 3.3 刷新

设备列表与任务列表均通过 visibility-aware single-flight loader。任务每 3 秒、设备每 5 秒；发送/取消成功后强制刷新不等待下一 tick。已有列表时刷新失败保留列表并标 stale；首屏失败显示重试，不清空一份已成功的数据。

## 4. 速记本持久化

新增 `ScratchpadAutosaveProvider` 常驻于 AppShell：按 `pageId` 保存最新 pending content，同页输入覆盖旧 pending，不同页可分别排队。500ms debounce 到期执行 `flushPage`；同一页面仅一个 in-flight save，保存期间产生的新版本在前一版本完成后继续 flush。失败时 pending 不丢弃，状态为 `error`，回到页面后显示重试。

切页、创建、删除、清空、LAN/GitHub 同步前继续显式 await 当前页 flush。Scratchpad 路由卸载调用 `flushAll()` 但 queue 本身不卸载；GUI 的“仅关闭”和“前后端都关闭”在 `exitGui()` 前 await `flushAllPendingWrites()`。保存失败时中止关闭并在关闭对话框显示错误，让用户重试或取消；不静默退出。`pagehide` 调用 best-effort flush，但不把它描述为强保证。

保存结果只在仍为当前页时回写编辑区；旧页面保存完成不得覆盖新页面草稿。`listPages` 刷新失败不否定正文已保存，只显示列表刷新警告。

## 5. Prompt CRUD 一致性

每次 mutation 记录 `PromptMutation`：

```ts
type PromptMutation =
  | { kind: 'create'; optimisticId: string; draft: PromptDraft }
  | { kind: 'update'; id: string; before: Prompt; draft: PromptDraft }
  | { kind: 'delete'; id: string; before: Prompt; index: number };
```

- create：先插入 optimistic row；成功用服务端 DTO 替换；失败删除 optimistic row、恢复新建草稿和错误横幅。
- update：先更新 row；成功用服务端 DTO 校准；失败恢复 `before`、重新打开同一编辑草稿。
- delete：先移除；成功结束；失败按原 index 恢复并保留删除失败提示。
- retry：重放保存的 mutation payload；成功清除错误。同步失败显示错误且保留现有列表。

同一实体 pending 时禁止再次编辑/删除；不同实体可独立操作。标签列表在 mutation 成功或回滚后从当前 prompts 派生，避免 tags API 与列表短暂分叉。

## 6. 请求正确性与局部容错

### 6.1 Claude History

`loadProjects` 与 `loadPrompts` 分别维护递增 sequence。Prompt 响应只有在 sequence 最新且 `{projectPath, search}` 仍等于当前请求上下文时才可回写。切到无项目时使旧 sequence 失效。刷新采集和同步复用同一受保护 loader；失败不得静默，使用非阻塞提示。

### 6.2 Settings

资源分组：

- `core`：当前 AppConfig（必需）、defaults、version。
- `cloudSync`、`githubTrending`、`health`、`automation`：各自 current/default。

`loadSettingsResources()` 使用 `Promise.allSettled` 返回每组 discriminated result。`configApi.get()` 失败显示通用设置错误；defaults 失败时禁用“恢复默认”但仍允许编辑当前值；version 失败只影响 About；业务 tab 失败只在对应 panel 显示 retry。重试只请求失败分组，不重置其他 tab 未保存草稿。

### 6.3 权限

`usePermissions` 暴露 `status/loading/refreshing/error/requesting/request/refresh/allRequiredGranted`。首轮失败结束 loading 并显示错误，不再永久“检查中”；已有状态刷新失败保留状态并标错误。`request(type)` 只请求一个权限，结束后刷新；同一权限请求期间禁用自身按钮。Welcome 和 Settings 都提供逐项按钮与显式“重新检查”。非 macOS 的现有平台映射保持不变。

## 7. 共享轮询合同

```ts
export interface VisibilityPollingOptions {
  intervalMs: number;
  enabled?: boolean;
  runImmediately?: boolean;
  refreshOnVisible?: boolean;
}

export interface VisibilityPollingResult {
  runNow: () => Promise<void>;
  inFlight: boolean;
}
```

`useVisibilityPolling(task, options)` 丢弃重叠 tick；`runNow` 在已有请求时返回同一 Promise。Transfer、Devices、Health、Permissions 迁移到该合同。Attention 已有同等且更完整的 Provider 语义，不迁移或重写。HealthOverlay 的本地倒计时不属于网络轮询，不暂停。

## 8. 用户反馈与可访问性

- 异步错误使用可读文案，不显示 `[object Object]`；可恢复错误提供 retry。
- sending/saving/requesting 使用 `aria-busy`，错误横幅用 `role="alert"`，非错误完成提示用 `role="status"`。
- Transfer dropzone 支持 Enter/Space 打开原生选择器；选中文件名可读且不暴露完整绝对路径。
- 所有新增文案进入现有 zh/en namespace，不写 JSX 硬编码字符串。

## 9. 测试与验收

Vitest 必须覆盖：single-flight/visibility、dialog 与 native drop path、send/cancel DTO、Scratchpad debounce/unmount/close flush 和失败保留、Prompt 三种 rollback/retry、CcHistory 逆序响应、Settings 每组失败、permissions 首轮失败/逐项请求。

Playwright 使用 Tauri invoke/dialog/event fakes 覆盖：选择文件→发送→出现任务→取消；Prompt API reject 后 UI 回滚；权限检查失败后显示重试而非永久 loading。跨平台 smoke 至少在 Rust 单测校验 Windows/Unix 路径作为不透明字符串进入 sender；真实系统 dialog、原生拖放和三平台文件发送仍列为人工 GUI smoke，不冒充 hosted CI 已验证。

完成标准：页面不再包含 Transfer no-op 动作或 Prompt 静默 catch；速记本 GUI 关闭会等待 pending write；四类轮询页面在 hidden 时无新请求且恢复时只发一次；全部前端 lint/build/unit/e2e 与相关 Rust transfer tests 通过。

## 10. 非目标与文档同步

- 本轮不实现暂停/继续、失败重试、系统打开文件。
- 不改变 transfer chunk、resume protocol 或接收端安全策略。
- 不引入离线 Prompt outbox 或跨设备冲突合并。
- 不重构 Settings 全部 JSX；结构性拆分由前端基础计划负责。

实现后更新 `docs/prd.md` 的传输、速记本、权限失败语义，以及 `web/CLAUDE.md` 的轮询和 autosave queue 约定；新增组件时同步根 `AGENTS.md` 组件清单。
