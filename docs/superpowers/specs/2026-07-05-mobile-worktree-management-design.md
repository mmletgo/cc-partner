# Mobile Worktree Management Design

## Goal

移动端 `/mobile` Workbench 需要补齐 worktree 的显示、快速切换、完整管理、合并和删除能力，并与现有 Terminal、Files、Git、Prompt 面板共享同一个 active worktree 上下文。

## Confirmed Interaction Direction

用户确认采用 `B + A` 混合方案：

- `B`：顶部状态栏 worktree pill 作为快速入口，点击后打开底部抽屉，支持随时查看和快速切换 worktree。
- `A`：保留独立 `Worktrees` 管理面板，承载完整列表、新建、切换、合并、删除等管理动作。

Git 面板继续保留当前 worktree 的 Commit / Push / Merge 上下文快捷入口，但不再作为 worktree 操作的唯一入口。

## Current Context

现有移动端 Workbench 已经具备以下基础能力：

- `MobileWorkbench` 维护 `activeProject`、`worktrees`、`activeWorktree`、`sessions`、`activeSession`。
- `MobileWorkbenchShell` 顶部状态栏已展示项目、worktree、session 三个 status pill，但 pill 目前只是静态信息。
- `MobileWorktreePanel` 已支持 worktree 列表、创建、切换和删除，但列表信息较轻，且缺少 merge 操作。
- `MobileGitPanel` 已支持当前 worktree 的 Commit / Push / Merge，merge 通过父级 `handleMergeWorktree` 复用 destructive flow。
- `mobilePanelState.ts` 已提供 `runMobileWorktreeRemovalFlow`、`runMobileWorktreeMergeFlow`、`runMobileWorktreeRefreshFlow` 和 Files dirty context guard。
- 移动端 HTTP transport 已暴露 worktrees list/create/commit/push/merge/remove 等操作。

设计应优先复用这些状态和 helper，不新增第二套 worktree 模型。

## User Experience

### 1. Topbar Worktree Quick Switch

顶部状态栏中的 worktree pill 改为可点击控件：

- 无 active project 时保持禁用或静态占位。
- 有 active project 但无 worktree 时显示空态文案，点击可打开抽屉并展示空列表。
- 有 active worktree 时显示 worktree 名称，附带下拉/展开 affordance。

点击后打开底部抽屉。抽屉内容：

- 当前项目名称与当前 active worktree。
- worktree 列表，每项显示：
  - 名称；
  - branch；
  - main / linked 标记；
  - clean / dirty / conflict；
  - ahead / behind；
  - canPush 简要状态。
- 点击列表项执行快速切换。
- 抽屉底部提供「管理 Worktrees」入口，跳转到独立 Worktrees 面板。
- 第一版抽屉不暴露「合并」「删除」快捷按钮；危险操作集中在完整 Worktrees 面板，降低手机端误触风险。

### 2. Full Worktrees Panel

独立 `Worktrees` 面板作为完整管理页：

- 面板头部展示当前项目、刷新入口和新建 worktree 表单。
- 新建表单与桌面端保持一致：固定 prefix 选择（feature/fix/chore/docs/refactor/test/hotfix）+ suffix 输入，最终组合成 `<prefix>/<suffix>`。
- 列表项展示完整状态：
  - worktree name；
  - branch；
  - root/path 简要信息；
  - main / linked 标记；
  - clean / dirty / conflict；
  - changed/conflicts 数量；
  - ahead/behind；
  - canPush。
- 每项操作：
  - `切换`：所有 worktree 可用。
  - `合并`：仅非 main worktree 可用。
  - `删除`：仅非 main worktree 可用。
- 主 worktree 不能合并或删除。

### 3. Git Panel Relationship

Git 面板仍以当前 active worktree 为上下文：

- 保留 Commit / Push / Merge。
- Commit / Push 成功后刷新当前 worktree 状态与提交列表。
- Merge 成功后源 worktree 可能被删除，Git 面板应清空旧提交请求并等待父级切换到 fallback worktree。
- Git 面板不负责展示全量 worktree 列表，避免 Git 历史视图变成第二个管理页。

## State And Data Flow

### Active Worktree

`MobileWorkbench` 继续作为唯一 active worktree owner：

- `activeWorktree` 写入后同步 `activeWorktreeRef`。
- 切换 active worktree 时调用 `setActiveWorktreeWithSession`，同时选择匹配的 preferred session。
- Terminal、Files、Git、Prompt 均读取父级传入的 active worktree。

### Quick Switch Drawer

快速抽屉应作为 shell 或父级受控 UI：

- 由 topbar worktree pill 打开/关闭。
- 列表数据来自 `MobileWorkbench` 的 `worktrees` state。
- 切换动作复用 `handleSelectWorktree`。
- 刷新动作复用 `refreshWorktrees`。
- 抽屉不发起合并或删除，危险操作统一进入完整 Worktrees 面板或 Git 面板执行。

### Refresh

所有创建、删除、合并、commit、push 后都应调用 `refreshWorktrees({ expectedProjectId })` 或等价流程：

- 如果当前 active worktree 仍存在，保留当前 active。
- 如果当前 active worktree 已被删除或合并清理，选择主 worktree 或首个 worktree 作为 fallback。
- 如果 Files 有未保存草稿且目标 context 不同，先确认；取消时不得应用新列表或切换 active。

## Safety And Error Handling

### Dirty Files Guard

以下动作会改变 Files 上下文，必须经过现有 dirty guard：

- 快速切换 worktree。
- Worktrees 面板切换 worktree。
- 删除当前 active worktree。
- 合并当前 active worktree。
- 删除或合并非 active worktree 不应触发 Files dirty guard，除非操作结果会导致 active fallback。

删除/合并这类 destructive 操作必须先做 confirm-only guard；只有后端成功后才调用 discard token 清理 dirty snapshot。

### Destructive Confirm

删除 worktree：

- 必须二次确认。
- 主 worktree 禁用删除。
- 删除 active worktree 成功后切换到 fallback worktree。
- 后端失败时保留当前 active 和 Files 草稿。

合并 worktree：

- 仅非 main worktree 可用。
- 必须二次确认，文案说明会合并到主工作区且源 worktree 可能被清理。
- 合并成功后源 worktree 从 UI 中移除，并切回 fallback worktree。
- 后端失败时保留当前 active、列表和 Files 草稿。

### Stale Guard

延续当前 request id/ref 模式：

- 项目切换后旧 worktrees/sessions 响应不得覆盖新项目。
- merge/delete 返回前如果 active project 已变化，不应用结果。
- Git commits 请求必须绑定当前 project/worktree context。

## Visual And Component Constraints

- 沿用 `MobileWorkbench.module.css` 的移动端列表、toolbar、status card 样式，不新建不必要的视觉体系。
- 所有颜色、间距、圆角、阴影使用 `web/src/styles/tokens.css` token。
- 用户可见文案写入 `web/src/i18n/locales/{zh,en}/workbench.json`。
- React hooks 必须位于所有 early return 之前。
- 新增或修改函数必须补充中文 docstring，描述 Business Logic 与 Code Logic。

## Scope

第一版应包含：

- 顶部 worktree pill 可点击并打开快速切换抽屉。
- 抽屉展示 worktree 列表并支持快速切换。
- 抽屉提供进入完整 Worktrees 面板的入口。
- Worktrees 面板补齐 merge 操作。
- Worktrees 面板补齐更完整的状态展示。
- Worktrees 新建表单改为 prefix + suffix，与桌面端一致。
- 删除/合并/切换均走现有 dirty guard 和 destructive flow。
- 更新 mobile worktree 相关纯函数测试。

第一版不包含：

- 移动端远端项目 worktree 管理。
- 新后端权限或鉴权模型。
- 新的 Git 图形提交树。
- 自定义 merge conflict 解决 UI。
- 后端 worktree API 重写。

## Verification Plan

实现阶段应优先跑移动端相关验证：

- `cd web && npx --yes tsx src/mobile/mobilePanelState.test.ts`
- `cd web && npx --yes tsx src/mobile/mobileWorkbenchState.test.ts`
- `cd web && npx --yes tsx src/mobile/mobileTerminalReplay.test.ts`
- 若新增或修改 HTTP adapter 契约：`cd web && npx --yes tsx src/api/workbenchHttp.test.ts`
- 最后运行：`cd web && npx tsc --noEmit`

如果实现涉及 Rust HTTP routes，再补充对应 `cargo test` 或 `cargo check`，但本设计目标是优先复用既有 HTTP routes。

## Resolved Decisions

- 交互方向：采用 `B + A` 混合方案。
- 快速抽屉：第一版只做快速查看、切换和进入完整 Worktrees 面板。
- 危险操作：合并和删除只放在完整 Worktrees 面板与 Git 面板的当前 worktree 上下文操作区，不放在快速抽屉内。
