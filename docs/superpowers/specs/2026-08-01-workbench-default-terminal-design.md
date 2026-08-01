# Workbench 默认打开终端界面 — 设计

日期:2026-08-01
作者:brainstorming 与用户协作产出
状态:待用户审阅

## 背景与目标

用户在 cc-partner 工作台打开具体项目时,希望中心区域默认进入终端界面。当前行为:

- `Workbench.tsx:132` 已经把 `workspaceView` 的 React 初始 state 设为 `'terminal'`。
- 但 `useWorkspaceSafeRestore` 在项目列表就绪后跑 `workbenchApi.layout.preflight()`,把上次会话保存的 `workspaceView`(files / browser)回写到 UI,覆盖默认 terminal。
- 用户多次反馈"打开项目看到的是文件/浏览器而不是终端"。

期望:

- **首次打开本会话的项目时,中心区域强制为 terminal**。
- 之后用户在会话内手动切到 files/browser 不被回滚。
- 命名 snapshot 应用时保留快照中的 `workspaceView`(用户显式 apply)。
- `inspectorTab` 默认 `files`,行为不变(可被恢复)。
- 硬编码,本期不暴露设置项。

## 范围

**包含**

- 修改 `web/src/pages/Workbench/useWorkspaceSafeRestore.ts`,在初始 restore 路径下注入「强制 terminal」开关。
- 更新既有 / 新增单元测试,锁定新行为。
- i18n / CSS / Rust 后端 / mobile web 入口:**不动**。

**不包含**

- 不修改 `applyWorkspaceRestorePlan`、`workspaceLayout`、`workspaceRestore.ts` 的 pure 实现。
- 不修改 `Workbench.tsx` 主体(已通过窄 hook 控制)。
- 不动 inspectorTab 默认值。
- 不引入设置项 / 不写本地存储。
- 不动 mobile web / Tauri 后端。

## 设计

### 核心:在 `useWorkspaceSafeRestore` bridge 注入强制开关

文件:`web/src/pages/Workbench/useWorkspaceSafeRestore.ts`

```ts
// runRestoreWithUi 新增可选参数
forceTerminalWorkspaceView?: boolean;

// buildBridge 接收该选项,生成条件化的 setWorkspaceView
setWorkspaceView: (view: WorkspaceView) => {
  const target = options.forceTerminalWorkspaceView ? 'terminal' : view;
  setWorkspaceView(target as WorkbenchFileWorkspaceView);
},
```

调用方约定:

- **初始 restore effect**(挂载 + projects 就绪后唯一一次执行):传 `forceTerminalWorkspaceView: true`。
- **命名 snapshot apply**(`applyNamedSnapshot`):不传,保持默认 false,尊重快照中的 `workspaceView`。

### 不变量

1. 用户在会话内通过 `requestWorkspaceView` / 终端/文件/浏览器切换按钮直接调用 `setWorkspaceView` 不经过 bridge,完全不受影响。
2. `applyWorkspaceRestorePlan` 的 `restoredCount` / `skippedCount` / `reasons` summary 语义不变,只是 `setWorkspaceView` 写入的值变成了 `'terminal'` 而非 `plan.workspaceView`。
3. `suppressContextResetRef` 现有的 50ms 抑制窗口仍然保证 project/worktree effect 不被 restore 顺序竞态清除。
4. `restoreSummary` 仍然触发 partial / complete notice,UI 反馈链路不变。

### 关键文件改动

| 文件 | 改动 |
| --- | --- |
| `web/src/pages/Workbench/useWorkspaceSafeRestore.ts` | 新增 `forceTerminalWorkspaceView` 参数,bridge `setWorkspaceView` 条件化;初始 restore 调用传入 `true` |
| `web/src/pages/Workbench/useWorkspaceSafeRestore.test.tsx`(如不存在则新建) | 单元测试:初始 restore 后 workspaceView === 'terminal',命名 snapshot apply 后 workspaceView === snapshot.value |
| `web/src/pages/Workbench/workbenchWorkspaceSwitch.test.ts`(或等价文件名) | 静态合同:断言初始 restore 不再带回 files / browser |

> 不改 `applyWorkspaceRestorePlan`,pure 函数语义保持;不让「强制 terminal」扩散到恢复层。

### 数据流(初始 restore)

```
mount → projects.length > 0 && !restoreRan
  → runRestoreWithUi({ previous, loadPlan, forceTerminalWorkspaceView: true })
    → suppressContextResetRef.current = true
    → workbenchApi.layout.preflight()  ← 仍然拿上次的 layout (含 workspaceView=files)
    → workbenchApi.layout.apply(plan)
    → applyWorkspaceRestorePlan(bridge_with_force_terminal=true)
        → ordered actions 里 workspaceView action
        → bridge.setWorkspaceView(plan.workspaceView = 'files')
            → 但 bridge 检测到 forceTerminalWorkspaceView=true,实际写 'terminal'
    → suppressContextResetRef.current = false (50ms 后)
```

### 数据流(命名 snapshot apply)

```
user click [apply named snapshot]
  → applyNamedSnapshot(layoutId)
    → runRestoreWithUi({ previous, loadPlan, forceTerminalWorkspaceView: false })  // 默认
      → bridge.setWorkspaceView(plan.workspaceView = 'files')  ← 尊重快照
```

### 数据流(用户手动切换)

```
user click [Files] button → requestWorkspaceView('files') → setWorkspaceView('files')
  ↑ 不经过 bridge,直接走 page-level setter,不受 forceTerminalWorkspaceView 影响
```

## 测试

### 单元(必须)

- `useWorkspaceSafeRestore.test.tsx`:
  - 「初始 restore 后,即使预存的 layout.workspaceView='files',实际写入仍是 'terminal'」
  - 「命名 snapshot apply 后,workspaceView 写入 snapshot.workspaceView 的值」
  - 「预存 layout 缺失(preflight 空)时,不调用 setWorkspaceView」

### 静态合同

- 在 `workspaceRestore.test.ts` 维持原 `applyWorkspaceRestorePlan` 行为测试,**不**改这些断言。
- 在 `useWorkspaceSafeRestore.test.tsx` 验证 bridge 内部强制行为。

### E2E

- `frontend-foundation.spec.ts` 或 `workbench.spec.ts`:模拟「上次保存 files view → 重新启动 → 打开项目 → 中心区域为 terminal」(可选,优先级低)。

### 不期望 / 禁止的测试

- 不要在 E2E 测 `applyWorkspaceRestorePlan` 内部行为(pure 函数已被单测覆盖)。
- 不要在 Workbench.tsx 字符化测试里直接断言 workspaceView(应通过 stub bridge 测)。

## 风险与权衡

| 风险 | 缓解 |
| --- | --- |
| 用户希望首次也保留 files view(个性化) | 留作后续设置项;本期硬编码 |
| 命名 snapshot 用户期望强制 terminal | 已澄清:命名 snapshot 是显式 apply,应尊重快照 |
| 与 `workspace_layout` 自动保存交互 | autosave 500ms debounce 仍按用户实际选择的 view 写盘,下一次会话 restore 时仍会被本次初始 restore 覆盖回 terminal(预期) |
| `restoreSummary` 计数看起来「恢复了 view 但 UI 不是 files」 | summary 仍按 `restoredCount += 1` 计入 view 恢复,与现有 partial 语义一致,这是「强制 terminal 仍算恢复了 view」的可接受偏差 |

## 实施拆解(高层)

1. 修改 `useWorkspaceSafeRestore.ts`:
   - `runRestoreWithUi` 参数加 `forceTerminalWorkspaceView?: boolean`
   - `buildBridge` 接收 options,`setWorkspaceView` 条件化
   - 初始 restore effect 传入 `true`,`applyNamedSnapshot` 不传
2. 写 / 扩 `useWorkspaceSafeRestore.test.tsx`
3. 跑 `npm run lint && npm test -- useWorkspaceSafeRestore workspaceRestore`
4. 跑 `npm run check:i18n && npm run check:bundle`(确认无回归)
5. 跑 `npm run test:e2e -- workbench.spec.ts`(回归 E2E)

## 不在本次范围

- 设置项 / 用户偏好 UI
- inspectorTab 默认值改动
- mobile web 同步行为
- Rust 后端任何接口
- 工作现场跨设备同步(目前只有本机 `desktop:auto` slot)
