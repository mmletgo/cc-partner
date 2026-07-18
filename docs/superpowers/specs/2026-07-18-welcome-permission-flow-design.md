# Welcome 系统权限交互设计

- 日期：2026-07-18
- 状态：已实现（方案 1：禁自动 relaunch / 可选重新打开 / request 按类型分流）
- 范围：Welcome 页四项权限「去设置」分流、回前台同步、可选重新打开应用
- 非范围：改权限种类、Onboarding 四项必齐规则、LAN 披露、假绿检测

## 1. 背景与问题

### 1.1 现象

1. 用户在系统设置中打开权限开关后，Welcome 仍显示未授权。  
2. 为实现「授权后显示已授权」曾加入**静默 relaunch**；结果是点击「去设置」即闪屏/白屏。

### 1.2 根因

| 问题 | 说明 |
| --- | --- |
| macOS 进程态滞后 | 屏幕录制 / 辅助功能 / 输入监控在**当前进程**内，系统开关打开后检测 API 常仍返回未授权（日志曾见 `IOHID=Denied` 且 `bundle_id=com.cc-partner.app.dev`）。 |
| 错误 relaunch 时机 | 点「去设置」→ `request()` → **立即** `applyPermissionsAfterSettings` → 因尚未授权判定 still denied → **立刻 relaunch**。用户还没拨开关进程已被杀掉 → 闪屏/白屏。 |
| 双开流程 | 现网 `request_permission` 默认「登记/请求 + 打开设置」两条都走，与「按权限分流」目标不符。 |
| 说教文案 | 用户明确不要 Welcome 出现「完全退出 / 开对条目」类长文案。 |

### 1.3 成功标准（用户确认）

用户选择交互策略 **C**，落地形态为 **方案 1**：

- 点「去设置」**绝不**自动重启。  
- 回前台多轮 recheck；能绿则绿。  
- 仅当「去过设置 + 回前台 + 多轮仍未授权」时，出现**可选**「重新打开应用」。  
- 无说教长文案。

## 2. 目标与非目标

### 2.1 目标

1. 「去设置」只做：登记（如需）+ 打开对应设置页或（必要时）系统授权框。  
2. 用户授权并回到应用后，尽量通过 recheck 显示已授权。  
3. 进程态滞后时，提供可选 relaunch 出口，且仅用户点击后执行。  
4. 修复闪白屏：删除 request 后立即 relaunch 的路径。

### 2.2 非目标

- 不在检测 API 为 false 时把卡片标成已授权（禁止假绿）。  
- 不改四项必齐（screenCapture / accessibility / inputMonitoring / notification）。  
- 不解决 Dev/Release 开错 Bundle 的系统级误授权（身份问题）。  
- 不在 check / 挂载路径对辅助功能自动 prompt。

## 3. 权限分流（点「去设置」）

### 3.1 统一原则

1. **登记**（若系统列表需要本 app）：技术步骤，尽量不制造多余弹窗。  
2. **主动作**：能直达对应系统设置页 → **只打开该页**；不能有效完成首次授权 → 才走系统权限请求框。  
3. **禁止**：在点「去设置」时 relaunch / 杀进程。

### 3.2 四项矩阵

| 权限 | 登记（如需） | 主动作 | 禁止 |
| --- | --- | --- | --- |
| 屏幕录制 | 未决定时可 `CGRequestScreenCaptureAccess`（系统可能出框） | 打开「隐私 → 屏幕录制」 | 不 relaunch |
| 辅助功能 | `AXIsProcessTrustedWithOptions(prompt)` 登记列表 | 打开「隐私 → 辅助功能」 | check 路径不 prompt |
| 输入监控 | **静默登记**（IOHID / CG / listen-only tap），保证列表有本 app | 打开「隐私 → 输入监控」 | 登记后不 relaunch |
| 通知 | — | **未决定** → `requestAuthorization`；**已拒绝** → 打开通知设置；**已授权** → no-op + recheck | 默认不「请求 + 开设置」双开 |

用户确认：四项在「列表需要本 app」时都要做登记；主路径以打开设置为准（通知未决定除外）。

### 3.3 后端 API 语义（实现时调整）

`request_permission(type, open_settings?)` 建议语义：

1. `register_if_needed(type)`  
2. 按矩阵选择 `open_settings_pane` 或 `system_prompt` 或 `noop`  
3. 返回扩展字段（兼容现有）：

```text
{ ok, requested, opened, action: 'settings' | 'prompt' | 'noop' }
```

前端可不依赖 `action` 做核心状态机，但有利于测试与日志。

## 4. 回前台状态机（方案 1）

### 4.1 状态

| 状态 | 含义 |
| --- | --- |
| `idle` | 默认；展示卡片 + 简短 waiting / ready hint |
| `awaiting_user_in_settings` | 本会话已点过至少一项 sticky「去设置」，用户可能在系统设置中 |
| `syncing` | 回前台后多轮 recheck 进行中 |
| `needs_reopen` | 多轮 recheck 后 sticky 权限仍未齐；展示可选「重新打开应用」 |

**Sticky 权限**：`screenCapture` | `accessibility` | `inputMonitoring`（通知通常即时，不单独驱动 needs_reopen）。

### 4.2 转移

```text
idle
  --点 sticky「去设置」--> awaiting_user_in_settings
        （仅登记+开设置/弹框；无 relaunch）

awaiting_user_in_settings
  --visibility/focus 可见--> syncing

syncing
  --多轮 recheck 后 sticky 全 granted--> idle
  --多轮后仍有 sticky denied--> needs_reopen
  --四项全 granted（含 notification）--> idle（并可启用「继续使用」）

needs_reopen
  --用户点「重新打开应用」--> relaunch_for_permissions（open .app）
  --recheck 变为 granted--> idle
  --本会话已展示过 reopen 且用户忽略--> 保持 needs_reopen 或降级为 idle+waiting
        （实现取：保持按钮直至 granted 或离开 Welcome）
```

### 4.3 Recheck 策略

- 回前台进入 `syncing` 后强制序列示例：`0 / 400ms / 1.2s / 2.5s`（可微调，须可测）。  
- 每轮：`refresh()` 或直接 `check_permissions`。  
- 全局 2s `useVisibilityPolling` 保留作兜底；hidden 暂停。  
- **禁止**：`request()` resolve 后立即进入 relaunch 分支。

### 4.4 Relaunch 规则

| 规则 | 要求 |
| --- | --- |
| 触发 | 仅 `needs_reopen` 下用户点击「重新打开应用」 |
| 实现 | macOS：`open` enclosing `.app`（延迟后 exit）；禁止直接 exec `Contents/MacOS/*` |
| 次数 | 不自动连点；按钮可再次点击，但不得在无点击时重启 |
| 命令 | 保留/收敛 `relaunch_for_permissions`，仅按钮路径调用 |

## 5. UI 文案

| 场景 | 中文 | 英文 |
| --- | --- | --- |
| 默认等待 | 正在等待系统授权… | Waiting for system authorization… |
| 同步中 | 正在同步权限状态… | Syncing permission status… |
| 需可选重启 | 权限可能需重新打开应用后生效 | Permissions may apply after reopening the app |
| 按钮 | 重新打开应用 | Reopen app |

**禁止**：完全退出、开对条目、Dev/Release 长说明、教学段落。

页脚主 hint 只显示上表之一；卡片仍用「已授权 / 去设置」。

## 6. 模块改动边界

| 模块 | 改动 |
| --- | --- |
| `web/src/pages/Welcome/Welcome.tsx` | 实现状态机；删除自动 relaunch；接线 reopen 按钮 |
| `web/src/hooks/usePermissions.ts` | 保持轮询/request/refresh；不负责 relaunch |
| `web/src/api/config.ts` | `relaunchForPermissions` 仅供按钮；`requestPermission` 契约对齐 |
| `web/src/i18n/locales/{zh,en}/welcome.json` | 同步文案 key；移除说教 key |
| `src-tauri/src/permissions/mod.rs` | 按 §3 分流 request；保留 `relaunch_for_permissions`（open .app） |
| `src-tauri/src/commands/permissions.rs` | 命令注册与返回值 |
| `web/CLAUDE.md` / `src-tauri/CLAUDE.md` | 更新权限流程需求描述 |

### 必须删除的错误路径

1. Welcome：`request()` 后立刻 `applyPermissionsAfterSettings → relaunch`  
2. visibility/focus：未授权即自动 relaunch  
3. 任何「点去设置就杀进程」逻辑  

## 7. 测试计划

### 7.1 自动化

- Welcome/状态机单测（jsdom）：  
  - 点去设置 **不** 调用 relaunch  
  - 回前台进入 syncing；mock 全 granted → idle  
  - mock 仍 denied → needs_reopen 且出现按钮；点击才 relaunch  
- `usePermissions` 既有合同保持  
- Rust：`request_permission` 返回形状；`shell_single_quote`；无 product bundle fail-closed  
- localeParity / i18n  

### 7.2 手动（macOS）

1. 点「去设置」：打开对应设置，应用不闪退、不白屏。  
2. 打开开关后回应用：能检测则卡片变绿。  
3. 输入监控若仍红：出现短文案 +「重新打开应用」；点击后 relaunch，再进 Welcome 应变绿（开关已开前提下）。  

## 8. 风险与接受

| 风险 | 处理 |
| --- | --- |
| 输入监控必须新进程 | 可选按钮，不自动杀进程 |
| 打开设置但列表无 app | 四项登记策略（§3） |
| relaunch 用错 exec | 强制 LaunchServices `open` `.app` |
| 用户拒绝授权 | 保持未授权卡片；可跳过 Welcome |

## 9. 实现顺序（供后续 plan）

1. **紧急**：拆除自动 relaunch（消除闪白屏）。  
2. Welcome 状态机 + 文案 + 可选按钮。  
3. 后端 `request_permission` 按权限分流。  
4. 测试与 CLAUDE.md 更新。  
5. 手动验证四项路径。  

## 10. 决议记录

| 项 | 决议 |
| --- | --- |
| 交互策略 | C + 方案 1 |
| 去设置双开 | 否；按权限分流 |
| 登记 | 四项在需要时都登记；输入监控静默登记 + 开设置 |
| 自动 relaunch | 禁止 |
| 可选 relaunch | 仅 needs_reopen + 用户点击 |
| 说教文案 | 禁止 |
