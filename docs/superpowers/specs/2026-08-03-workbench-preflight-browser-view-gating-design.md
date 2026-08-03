# Workbench preflight 按 view gate 浏览器目标 — 设计 spec

- Date: 2026-08-03
- Owner: backend (cc-partner)
- Status: approved, in implementation

## 背景与动机

用户反馈：**新进入 Workbench 中的某个项目，自动跳到「项目预览」(browser) 视图，而非所期望的 terminal 视图**。

根因（已定位）：
- `src-tauri/src/workbench/workspace_restore.rs::preflight_workspace_restore` 在 `:534-563` 总是把 `layout.browser_target_url`（即使非 null）作为 `browserTarget` Select action 推入 plan
- 前端 apply 顺序（`web/src/pages/Workbench/workspaceRestore.ts` `sortActions`）: `project → worktree → session → workspaceView → inspectorTab → browserTarget`
- `useWorkspaceSafeRestore.ts` 的 bridge `restoreBrowserTarget`（`:229-240`）显式 `setWorkspaceView('browser')`，**完全绕过 `forceTerminalWorkspaceView`**（原代码注释：「此处不走 forceTerminalWorkspaceView 强制逻辑」）
- 前端 Workbench `Workbench.tsx:497-545` 的项目/worktree effect 在 `suppressContextResetRef` 窗口内（50 ms `setTimeout`）被门禁，不重置 `workspaceView`，等 suppress 解除时 UI 已稳定停在 `'browser'`

触发条件：layout 里 `workspaceView: 'terminal' | 'files' | 'automation'` 且 `browser_target_url` 非 null。layout 在关闭 workbench 后保留的是 `browserTargetUrl`（之前 session 用过的 dev server URL），所以即使上次停在 terminal 也复现。

## 修复方案

### 1. 后端 `preflight_workspace_restore` 按 view gate

`src-tauri/src/workbench/workspace_restore.rs::preflight_workspace_restore`，在 `:534-563` 的 browser 段加 view gate：

- 当 `layout.workspace_view == 'browser'` 时：行为不变 — normalize URL + 推 `Select` action，或推 `Skip BrowserTargetInvalid` / `Skip BrowserNotRequested`
- 当 `layout.workspace_view != 'browser'` 时（即 `terminal` / `files` / `automation`）：不再推 `Select` action，而是推一条 `Skip` action，`reason = BrowserSkippedForNonBrowserView`（新 variant，见 §2）；不再写入 `plan.browser_target_url`

### 2. 新增 `RestoreSkipReason` variant

`src-tauri/src/workbench/workspace_restore.rs` 的 `RestoreSkipReason` enum（`:33-69`）新增：

```rust
/// layout.workspace_view != 'browser'：plan 不发 browserTarget action
/// （即使浏览器目标仍有效，也由下次显式切到 browser 时再恢复）。
BrowserSkippedForNonBrowserView,
```

`as_str()`（`:78-97`）追加 `"browserSkippedForNonBrowserView"`。

`has_skip` 逻辑（`:565-570`）把这个新 reason 也加进「不计 partial」白名单（与 `BrowserNotRequested` 同档）— 否则该 skip 会让 `status=Partial`，污染前端 restore notice。

### 3. `plan.browser_target_url` 字段含义

按用户决议：**workspaceView != browser 时，仍保留 layout 里 normalize 后的 URL，让前端编辑框 placeholder 可用**。
- 也就是说：preflight 的 `browser_target_url` 局部变量仍执行 `normalize_browser_target_url`，但**不**仅在推送 Select action 时才赋值 — 现在变成无论是否推 Select 都把 normalize 结果传给 `finalize_plan`
- 这样 `WorkspaceRestorePlan.browser_target_url` 始终是「可用于还原的值」，但 actions 不再强加 view 切换

`finalize_plan`（`:895-918`）无需改 — `browser_target_url` 已经是函数入参，由调用方决定。

### 4. 命名 snapshot apply 路径

`useWorkspaceSafeRestore.ts::applyNamedSnapshot`（`:444-468`）调用 `runRestoreWithUi` 时不传 `forceTerminalWorkspaceView`（保持既有尊重 snapshot 行为）。当用户在 snapshot 里保存了 browser view + URL，apply 后会回到 browser — 这是预期行为。
- snapshot 误存「terminal view + URL」时，apply 后 UI 也不应再 auto-切到 browser。后端 gate 在这路径上同样生效（同一份 server-side `preflight_workspace_restore`），修复自动覆盖。
- `WorkspaceSnapshotDialog.tsx` 保存路径（`:53` `handleSave`）调用 `saveNamedSnapshot`（`:414-442`），把 UI 当前 selection 写 layout — 修改 UI 后下次 restore 自然走新的 gate。

### 5. mobile / remote path

`commands/workbench/layout.rs::preflight_remote_layout`（`:520-581`）走 `RemoteWorkbenchClient::preflight_workspace_restore` POST owning device — owning device 的 server `preflight_workspace_restore` 是被修改的同一份代码，自动覆盖 remote shortcut + mobile（`net/routes/workbench.rs::list_remote_projects` 等入口都经这一条隧道路径）。

`partial_remote_controller_only`（`:583-619`）离线兜底：直接构造 plan，已经置 `browser_target_url: None`，符合本 spec。

## 不变更的语义

- **首次 restore 的 `forceTerminalWorkspaceView=true` 行为保持**：仍是 UI 层强制 terminal。但因为后端不再发 `Select` browserTarget action，第一次 restore 也不再依赖 forceTerminal 的 hack；命名 snapshot apply 路径自然也得益。
- **`restoreBrowserTarget` bridge 实现保留**：仍是显式 `setWorkspaceView('browser')`，这是命名 snapshot 「用户选了 browser」路径需要的行为。后端 gate 已确保不会在 terminal/files/automation 上意外触发它。
- **`auto-save` 行为保留**：当前 UI selection 经 `WorkspaceLayoutAutosaveCoordinator` 持续写入 layout。如果用户从 browser 切到 terminal，`browserTargetUrl` 仍保留在 layout 里 — 下次 restore 由本 spec 的 gate 保证不会自动回到 browser。这是符合预期的（用户重启用过的 URL，下次手切还可见）。

## 测试矩阵

### Rust（必需）

`src-tauri/src/workbench/workspace_restore.rs::tests` 模块新增（按既有 `preflight_*` 命名风格）：

1. `preflight_skips_browser_when_view_is_terminal_with_url`：
   - `WorkspaceLayout { workspace_view: 'terminal', browser_target_url: Some('http://127.0.0.1:3000') }`
   - Project / worktree / session 都存在
   - 期望：`plan.actions` 没有 target=`browserTarget` 且 outcome=Select 的项；
   - 期望：有一条 `target='browserTarget' outcome=Skip reason=Some(BrowserSkippedForNonBrowserView)`；
   - 期望：`plan.status` 不因 browser skip 而变成 Partial；
   - 期望：`plan.browser_target_url` 是 normalize 后的值（保留给 UI placeholder）。

2. `preflight_restores_browser_when_view_is_browser_with_url`：
   - 显式标 view=`browser` + URL → 既有 Select action 行为不变（回归基线）。

3. `preflight_skips_browser_for_files_and_automation_views`：
   - 验证 view=`files` / `automation` 都跳过 browserTarget Select action。

4. `has_skip_excludes_browser_skipped_for_non_browser_view`：
   - 直接断言 status 计算逻辑：actions 只含 `[project Select, worktree Select, workspaceView Select, inspectorTab Select, browserTarget Skip(BrowserSkippedForNonBrowserView)]` 时，`status == Complete`（不是 Partial）。

### Web（轻量回归）

`web/src/pages/Workbench/workspaceRestore.test.ts` 已覆盖 `applyWorkspaceRestorePlan` 的项目/工作区路径。**无需为本次加新 case**：因为 runtime 后端不再发 browserTarget action，前端 sortActions/forEach 路径不变，只是 actions 数组少一项。

可以加一条 sanity 测试：`applyWorkspaceRestorePlan_skips_browser_when_view_is_terminal` — 喂一个不包含 browserTarget action 的 plan，断言 `bridge.setWorkspaceView` 被调用 `'terminal'`，`bridge.restoreBrowserTarget` 不被调用。这能在前端层守住 bridge 的 forceTerminal 注释被无意重新激活。

## 验证命令

```bash
cd src-tauri
cargo test --locked workbench::workspace_restore --lib
cargo test --locked workbench::workspace_layout --lib
cargo test --locked net::routes::workbench --lib
cargo test --locked commands::workbench::layout --lib
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo check --locked --bins

cd web
npm test -- src/pages/Workbench/workspaceRestore.test.ts
npm run lint
```

## 风险与回滚

- **风险**：如果用户确实想让前次关在 terminal 的项目启动时也切到 browser，本 spec 会让他们失望。判断：这是 source of confusion，不应保留。
- **回滚**：revert 单个 commit；autosave layout 已经持久化，新一次启动会用旧 gate 行为恢复（用户可以靠 UI 重启验证回滚）。
- **无须数据迁移**：`workbench_workspace_layouts` schema 不变。

## 文档影响

- `docs/development/testing.md` 的「Workbench Workspace safe restore」段：补充新 skip reason 与本 spec 引用的「plan.browser_target_url 与 plan.actions 关系」。
- `docs/superpowers/specs/` 本文件落地后不需要再更新其它 spec。
